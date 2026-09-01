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

use alloc::sync::Arc;

use crate::arch::common::msi::MsiRegistry;
use crate::object::interrupt::Interrupt;

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
// bit is cleared IN THE FILE THAT HOLDS IT - which is not necessarily this hart's - so a later stray
// MSI to it pends and dispatches to no one WHILE IT STAYS UNOWNED. Reallocate the EID and that same
// stray message wakes its next owner, which is the defect the x86 backend spells out; so the slot is
// retired rather than freed and waits for `SYS_DEVICE_QUIESCED`.
//
// AND IF THE OWNING HART DOES NOT ANSWER, the identity is still armed somewhere, so the slot is
// quarantined instead: it leaks a vector rather than handing a live one to the next driver.
// Returns whether the teardown CONFIRMED. This is the port where it can fail: the EID is disabled by
// the hart that owns it, and a hart that does not answer leaves the slot armed - which is why the
// unconfirmed branch quarantines rather than retires. The answer is reported so a claim's terminal
// state can include it, instead of being decided by the IOMMU alone while a still-armed vector is
// charged to the claim.
pub fn unbind(vector: u32) -> bool {
	let Some(slot) = eid_slot(vector) else { return true };
	if super::imsic::disable_eid_on_owner(vector) {
		REGISTRY.retire(slot);
		return true;
	}
	REGISTRY.quarantine(slot);
	false
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
		// Same rule as `unbind`: an identity that could not be disabled is not one to hand back,
		// even though this vector never reached a driver.
		if super::imsic::disable_eid_on_owner(vector) {
			REGISTRY.free(slot);
		} else {
			REGISTRY.quarantine(slot);
		}
	}
}

// One vector per device, entry 0 - see the x86_64 `acquire_msi`, which states the limit and why it
// exists. `MsiRegistry::acquire` does NOT enforce it, which this comment used to claim: the form that
// does is `acquire_unique_live`, reached through `acquire_msi_unique` below.
#[cfg(test)]
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
	// A MACHINE WHOSE IMSIC THIS KERNEL COULD NOT ADDRESS HANDS OUT NO VECTOR. The boot said so and
	// took the path out of service; programming a table entry now would write the compiled address
	// this port refused to use, which is the whole point of having refused it.
	if !super::imsic::usable() {
		REGISTRY.free(slot);
		return None;
	}
	let eid = EID_BASE + slot as u32;
	let hart = super::percpu::this_cpu().lapic_id();
	// A HART WITH NO INTERRUPT FILE IS NOT AN MSI TARGET. `msi_address` is `base + hart * stride`,
	// so a hart past the array the controller declares names an address inside something else.
	if !super::imsic::has_file(hart) {
		REGISTRY.free(slot);
		return None;
	}
	program_msix_entry(table_phys, super::imsic::msi_address(hart), eid);
	super::imsic::enable_eid(eid);
	// The whole EID (KERN-ARCH-017): IMSIC identifiers are eleven bits, so narrowing one here
	// would arm the hardware under an identifier the kernel never records.
	Some(eid)
}

// Write a device's MSI-X table entry 0 (reached through the physical direct map): the
// message address is a hart's IMSIC S-file, so the device's DMA write of the message
// data (the EID) pends that EID on that hart. Vector control = 1 (MASKED until
// `unmask_msi`). A driver must never write its own MSI-X table; only the kernel
// programs it here.
//
// PROGRAMMED MASKED, AND UNMASKED ONLY WHEN THE ACQUIRE HAS COMMITTED (2026-09-01) - see the
// x86_64 port's comment at the same function for the stale-generation window this closes.
fn program_msix_entry(table_phys: u64, msg_addr: u64, eid: u32) {
	let entry = super::paging::phys_to_virt(table_phys) as *mut u32;
	unsafe {
		entry.add(0).write_volatile(msg_addr as u32); // message address low
		entry.add(1).write_volatile((msg_addr >> 32) as u32); // message address high
		entry.add(2).write_volatile(eid); // message data = the EID
		entry.add(3).write_volatile(1); // vector control (MASKED until `unmask_msi`)
	}
}

// Make the entry programmed above deliverable - the other half of its mask. This port reaches the
// table through the physical direct map, so the caller supplies the address it programmed.
pub fn unmask_msi(_vector: u32, table_phys: u64) {
	let entry = super::paging::phys_to_virt(table_phys) as *mut u32;
	// SAFETY: the same entry `program_msix_entry` wrote, through the same mapping.
	unsafe { entry.add(3).write_volatile(0) };
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
// How many MSI slots this device still holds. See `MsiRegistry::held_by_device`.
// WHETHER THIS DEVICE STILL HOLDS A SLOT WHOSE TEARDOWN HAS NOT HAPPENED.
//
// `unbind` RETIRES a slot (pending) and a disable the controller refused QUARANTINES it, so a slot
// that is neither is one still bound - and a claim release that publishes `Free` over one of those
// gives a vector back while an unbind is still on its way to it. See `device::settled_vectors` and
// `MsiRegistry::has_unbound`, which states why a quarantined slot is settled rather than outstanding.
pub fn msi_live_for_device(device: u32) -> bool {
	REGISTRY.has_unbound(device)
}

// How many of this device's slots are QUARANTINED - a vector stranded by a teardown the controller
// refused. Sampled either side of a release so the ones stranded by THAT release can be told from
// the ones it inherited. See `MsiRegistry::quarantined_for` and `device::release_claim`.
pub fn msi_quarantined_for_device(device: u32) -> usize {
	REGISTRY.quarantined_for(device)
}

pub fn msi_held_by_device(device: u32) -> usize {
	REGISTRY.held_by_device(device)
}

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

// Take a vector out of circulation the way an unanswered cross-hart disable does, so the rule can
// be tested without wedging a hart with interrupts off.
#[cfg(test)]
pub fn quarantine_for_test(vector: u32) {
	if let Some(slot) = eid_slot(vector) {
		REGISTRY.quarantine(slot);
	}
}

// Whether `vector` is out of circulation for the life of the boot.
#[cfg(test)]
pub fn is_quarantined(vector: u32) -> bool {
	eid_slot(vector).is_some_and(|slot| REGISTRY.is_quarantined(slot))
}

// The number of vectors irq_info reports over (the timer entry plus the MSI window).
pub fn irq_info_len() -> usize {
	1 + MAX_MSI
}

#[cfg(test)]
mod tests;
