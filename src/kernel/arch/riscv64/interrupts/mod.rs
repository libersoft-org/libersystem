// riscv64 device-interrupt binding + MSI-X delivery via the AIA IMSIC.
//
// With QEMU's `virt,aia=aplic-imsic`, PCIe devices deliver MSI-X instead of wired INTx:
// a device signals by DMA-writing an interrupt identity (EID) to a hart's IMSIC S-mode
// file (imsic.rs), which pends that EID and raises the hart's external interrupt. So a
// device's MSI "vector" here is its EID: acquire_msi hands out a free EID, programs the
// device's MSI-X table entry to write it to the acquiring hart's IMSIC file, enables the
// EID there, and imsic::handle_external wakes the bound Interrupt when that EID fires.
//
// This mirrors the x86 (LAPIC-MSI) and aarch64 (GICv2m) backends: every driver that needs
// an interrupt uses MSI-X, the polled drivers (virtio-blk) need none, so is_bindable is
// always false and only the MSI window is live. Unlike the old PLIC INTx path, EIDs are
// per-device and edge-triggered - no shared line, no mask/complete dance, reliable
// delivery. The MSI-X table lives in a device BAR reached through the higher-half direct
// map (phys_to_virt), so no separate uncacheable mapping is needed.

#![allow(dead_code)]

use alloc::sync::Arc;

use crate::arch::common::msi::MsiRegistry;
use crate::object::interrupt::Interrupt;

// The device-IRQ vector window base (mirrors the contract; only the MSI window is live).
pub const IRQ_BASE: u8 = 32;

pub type HandlerFn = fn(u32);

// Device EIDs run 1..=MAX_MSI (EID 0 is "no interrupt"; the IMSIC EIE0 register holds
// EIDs 0..63 on RV64, so a single register covers them). Slot i (in the registry) maps
// to EID EID_BASE + i.
const EID_BASE: u32 = 1;
const MAX_MSI: usize = 62; // EIDs 1..=62, all within IMSIC EIE0

// The per-device MSI slot bindings (reserve / bind / dispatch / free bookkeeping, shared
// with x86/aarch64 via arch::common::msi). Slot i maps to EID EID_BASE + i.
static REGISTRY: MsiRegistry<MAX_MSI> = MsiRegistry::new();

// The registry slot an EID maps to, or None if it is outside the MSI window.
fn eid_slot(eid: u32) -> Option<usize> {
	if eid >= EID_BASE && ((eid - EID_BASE) as usize) < MAX_MSI { Some((eid - EID_BASE) as usize) } else { None }
}

// No legacy-INTx binding on riscv: every driver that needs an interrupt uses MSI-X.
pub fn is_bindable(_vector: u32) -> bool {
	false
}

// The INTx bind path is unused (see is_bindable); it always refuses.
pub fn bind(_vector: u32, _intr: &Arc<Interrupt>) -> bool {
	false
}

// Remove any binding for `vector` (an EID; called from an Interrupt's Drop). The EID's IMSIC enable
// bit is cleared best-effort, so a later stray MSI to it pends and dispatches to no one - WHILE IT
// STAYS UNOWNED. Reallocate the EID and that same stray message wakes its next owner, which is the
// defect the x86 backend spells out; so the slot is retired rather than freed and waits for
// `SYS_DEVICE_QUIESCED`.
pub fn unbind(vector: u32) {
	if let Some(slot) = eid_slot(vector) {
		super::imsic::disable_eid(vector as u32);
		REGISTRY.retire(slot);
	}
}

// Allocate a free EID and program a device's MSI-X table entry 0 so the device delivers
// it: message address = the acquiring hart's IMSIC S-file, message data = the EID. The
// EID is enabled on THIS hart (the one running the acquire), so the device's MSI targets
// it. `table_phys` is the device's MSI-X table (reached through the higher-half direct
// map). Returns the EID as the vector (None if every slot is taken); the caller enables
// MSI-X on the device (pci::msix_enable) and binds an Interrupt with bind_msi. `owner` is
// the discovered-device index (for the `lsirq` inventory); `dest` (the x86 LAPIC target)
// is unused - IMSIC targets the current hart.
// Give back a vector whose Interrupt never reached its owner - see the x86_64 `release_unused_msi`
// for why this is a free rather than a retire.
pub fn release_unused_msi(vector: u32) {
	if let Some(slot) = eid_slot(vector) {
		super::imsic::disable_eid(vector as u32);
		REGISTRY.free(slot);
	}
}

// One vector per device, entry 0 - see the x86_64 `acquire_msi`, which states the limit and why it
// exists. `MsiRegistry::acquire` does NOT enforce it, which this comment used to claim: the form that
// does is `acquire_unique_live`, reached through `acquire_msi_unique` below.
pub fn acquire_msi(table_phys: u64, _dest: u8, owner: u32) -> Option<u32> {
	program_acquired(REGISTRY.acquire(owner, MAX_MSI)?, table_phys)
}

// The same, and ONLY IF the device holds no live vector already - see
// `MsiRegistry::acquire_unique_live`. This is what `sys_device_msix_acquire` calls; the form above
// stays for the kernel's own bring-up test.
pub fn acquire_msi_unique(table_phys: u64, _dest: u8, owner: u32) -> Option<u32> {
	program_acquired(REGISTRY.acquire_unique_live(owner, MAX_MSI)?, table_phys)
}

fn program_acquired(slot: usize, table_phys: u64) -> Option<u32> {
	let eid = EID_BASE + slot as u32;
	let hart = super::percpu::this_cpu().lapic_id() as u64;
	program_msix_entry(table_phys, super::imsic::msi_address(hart), eid);
	super::imsic::enable_eid(eid);
	// The whole EID (KERN-ARCH-017): IMSIC identifiers are eleven bits, so narrowing one here
	// would arm the hardware under an identifier the kernel never records.
	Some(eid)
}

// Write a device's MSI-X table entry 0 (reached through the physical direct map): the
// message address is a hart's IMSIC S-file, so the device's DMA write of the message
// data (the EID) pends that EID on that hart. Vector control = 0 (unmasked). A driver
// must never write its own MSI-X table; only the kernel programs it here.
fn program_msix_entry(table_phys: u64, msg_addr: u64, eid: u32) {
	let entry = super::paging::phys_to_virt(table_phys) as *mut u32;
	unsafe {
		entry.add(0).write_volatile(msg_addr as u32); // message address low
		entry.add(1).write_volatile((msg_addr >> 32) as u32); // message address high
		entry.add(2).write_volatile(eid); // message data = the EID
		entry.add(3).write_volatile(0); // vector control (unmasked)
	}
}

// Bind `intr` to an MSI `vector` (an EID) so dispatch wakes it when the EID fires.
// Returns false if the vector is already bound to a live Interrupt.
pub fn bind_msi(vector: u32, intr: &Arc<Interrupt>) -> bool {
	match eid_slot(vector) {
		Some(slot) => REGISTRY.bind(slot, intr),
		None => false,
	}
}

// Whether `vector` (an EID) currently has a live driver binding.
pub fn is_bound(vector: u32) -> bool {
	match eid_slot(vector) {
		Some(slot) => REGISTRY.is_bound(slot),
		None => false,
	}
}

// End-of-interrupt for a serviced vector. IMSIC MSI is edge-triggered and unshared, so
// there is no level source to complete: a no-op (the stopei claim in handle_external
// already cleared the EID's pending bit), kept for the portable SYS_INTERRUPT_ACK path.
pub fn eoi(_vector: u32) {}

// Deliver a fired EID to its bound MSI driver. Returns true when the EID was a bound MSI
// vector (signaled here). Edge-triggered: just wake the bound driver.
pub fn dispatch_msi(eid: u32) -> bool {
	match eid_slot(eid) {
		Some(slot) => {
			REGISTRY.dispatch(slot);
			true
		}
		None => false,
	}
}

// The state of the vector at `index`, for the `lsirq` inventory. Index 0 is the kernel's
// own timer - the S-mode timer interrupt (SCAUSE code 5) - shown as a fixed vector like
// x86's LAPIC timer and aarch64's EL1 physical-timer PPI; the MSI window (each a device's
// EID) follows.
// Free every MSI vector that is masked and waiting for `device` to be confirmed stopped, and answer
// how many. Reached from `SYS_DEVICE_QUIESCED`.
pub fn release_msi_for_device(device: u32) -> usize {
	REGISTRY.release_for_device(device)
}

pub fn irq_info(index: usize) -> Option<abi::IrqInfo> {
	const TIMER_VECTOR: u32 = 5; // supervisor timer interrupt (scause code 5)
	if index == 0 {
		return Some(abi::IrqInfo { vector: TIMER_VECTOR, kind: abi::IRQ_KIND_FIXED, bound: 1, device: abi::IRQ_NO_DEVICE });
	}
	let slot = index - 1;
	if slot >= MAX_MSI {
		return None;
	}
	let eid = EID_BASE + slot as u32;
	Some(abi::IrqInfo { vector: eid, kind: abi::IRQ_KIND_MSI, bound: is_bound(eid) as u32, device: REGISTRY.owner(slot) })
}

// The number of vectors irq_info reports over (the timer entry plus the MSI window).
pub fn irq_info_len() -> usize {
	1 + MAX_MSI
}

// No kernel-side INTx handler registration on riscv (device interrupts are MSI; the
// timer is the S-mode timer, not an external source).
pub fn register(_vector: u32, _handler: HandlerFn) {}

// The IMSIC is brought up per hart in boot.rs / smp.rs (imsic::init_hart), so there is
// nothing left to initialize here.
pub fn init() {}

#[cfg(test)]
mod tests;
