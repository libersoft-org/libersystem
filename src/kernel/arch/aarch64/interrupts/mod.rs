// aarch64 device-interrupt binding + MSI-X delivery via GICv2m.
//
// The GIC has no per-vector "IDT" like x86: a device interrupt arrives at the core
// as a GIC INTID read from GICC_IAR in gic::handle_irq. MSI-X on a GICv2 is done with
// a GICv2m frame - a device signals by writing an SPI number to the frame's
// MSI_SETSPI_NS register (a DMA memory write), and the GIC then pends that SPI. So a
// device's MSI "vector" IS its GIC SPI INTID: acquire_msi hands out a free SPI,
// programs the device's MSI-X table entry to write it to the frame, enables the SPI
// in the distributor (edge-triggered, routed to the boot core), and gic::handle_irq
// wakes the bound Interrupt when that INTID fires.
//
// This mirrors x86 interrupts.rs, minus the legacy-INTx window: every aarch64 driver
// that needs an interrupt (virtio-net/input/snd, xhci, virtio-gpu) uses MSI-X, and
// the polled drivers (virtio-blk/console) need none - so is_bindable is always false
// and only the MSI window is live. The MSI-X table lives in a device BAR reachable
// through the higher-half physical direct map (phys_to_virt), so - unlike x86 - no
// separate uncacheable mapping is set up here.

#![allow(dead_code)]

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use alloc::sync::Arc;

use crate::arch::common::msi::MsiRegistry;
use crate::object::interrupt::Interrupt;

// The device-IRQ vector window base (mirrors the contract; only the MSI window is
// live on aarch64).
pub const IRQ_BASE: u8 = 32;

pub type HandlerFn = fn(u32);

// The GICv2m frame on QEMU's `virt` machine (gic-version=2), fixed just above the GIC
// CPU interface at 0x0801_0000. Its MSI_TYPER reports the SPI range the frame owns; a
// device writes an SPI number to MSI_SETSPI_NS to raise it.
const GICV2M_FRAME_BASE: u64 = 0x0802_0000;
const MSI_TYPER: u64 = 0x008; // [25:16] base SPI, [9:0] number of SPIs
const MSI_SETSPI_NS: u64 = 0x040; // a device writes its SPI number here to signal

// The MSI SPI range the GICv2m frame owns, read from MSI_TYPER at init: slot index
// 0..MSI_LEN maps to SPI INTID BASE_SPI + slot, and the SPI is the vector handed out.
static BASE_SPI: AtomicU32 = AtomicU32::new(0);
static MSI_LEN: AtomicUsize = AtomicUsize::new(0);

// Upper bound on the GICv2m SPIs tracked (QEMU virt exposes 64). Fixed-size tables
// keep the bindings off the heap and safe to touch from the interrupt path.
const MAX_MSI: usize = 64;

// The per-device MSI-X slot bindings (reserve / bind / dispatch / free bookkeeping,
// shared with x86 via arch::common::msi). Slot index i maps to SPI INTID
// BASE_SPI + i; only the first MSI_LEN slots (the frame's real SPI range) are used.
static REGISTRY: MsiRegistry<MAX_MSI> = MsiRegistry::new();

// The slot index of an SPI INTID, or None if it is outside the frame's MSI range.
fn spi_slot(intid: u32) -> Option<usize> {
	let base = BASE_SPI.load(Ordering::Relaxed);
	let len = MSI_LEN.load(Ordering::Relaxed);
	if intid >= base && ((intid - base) as usize) < len { Some((intid - base) as usize) } else { None }
}

// Whether `vector` (an SPI INTID) is a kernel MSI vector.
fn is_msi(vector: u32) -> bool {
	spi_slot(vector).is_some()
}

// No legacy-INTx binding on aarch64: every driver that needs an interrupt uses MSI-X.
pub fn is_bindable(_vector: u32) -> bool {
	false
}

// The INTx bind path is unused on aarch64 (see is_bindable); it always refuses.
pub fn bind(_vector: u32, _intr: &Arc<Interrupt>) -> bool {
	false
}

// Remove any binding for `vector` (called from an Interrupt's Drop).
//
// The SPI is retired rather than freed, for the reason the x86 backend spells out: the device's
// MSI-X entry was programmed to write the GICv2m frame, and nothing here can prove a write already
// on its way will not land. The SPI waits for `SYS_DEVICE_QUIESCED`.
pub fn unbind(vector: u32) {
	if let Some(slot) = spi_slot(vector) {
		REGISTRY.retire(slot);
	}
}

// Allocate a free MSI SPI and program a device's MSI-X table entry 0 so the device
// delivers it: message address = the GICv2m frame's MSI_SETSPI_NS register, message
// data = the SPI number. `table_phys` is the physical address of the device's MSI-X
// table (reached through the higher-half direct map). Returns the SPI as the vector
// (None if every slot is taken); the caller enables MSI-X on the device and binds an
// Interrupt to the returned vector with bind_msi. `owner` is the discovered-device
// index, retained for the `lsirq` inventory. `dest` (the x86 LAPIC target) is unused:
// GICv2m MSIs route through the distributor, which enable_msi_spi points at the boot
// core.
// Give back a vector whose Interrupt never reached its owner - see the x86_64 `release_unused_msi`
// for why this is a free rather than a retire.
pub fn release_unused_msi(vector: u32) {
	if let Some(slot) = spi_slot(vector) {
		REGISTRY.free(slot);
	}
}

// One vector per device, entry 0 - see the x86_64 `acquire_msi`, which states the limit and why
// `MsiRegistry::acquire_unique_live` refuses a device that already holds a live slot - `acquire`
// itself does not, which this comment used to claim.
pub fn acquire_msi(table_phys: u64, _dest: u8, owner: u32) -> Option<u32> {
	let len = MSI_LEN.load(Ordering::Relaxed);
	program_acquired(REGISTRY.acquire(owner, len)?, table_phys)
}

// The same, and ONLY IF the device holds no live vector already - see
// `MsiRegistry::acquire_unique_live`. This is what `sys_device_msix_acquire` calls; the form above
// stays for the kernel's own bring-up test.
pub fn acquire_msi_unique(table_phys: u64, _dest: u8, owner: u32) -> Option<u32> {
	let len = MSI_LEN.load(Ordering::Relaxed);
	program_acquired(REGISTRY.acquire_unique_live(owner, len)?, table_phys)
}

fn program_acquired(slot: usize, table_phys: u64) -> Option<u32> {
	let spi = BASE_SPI.load(Ordering::Relaxed) + slot as u32;
	program_msix_entry(table_phys, spi);
	super::gic::enable_msi_spi(spi);
	// THE WHOLE SPI (KERN-ARCH-017). GICv2m's base and count are ten-bit fields, so a frame
	// starting at SPI 256 or above returned an identifier that wrapped: the hardware stayed armed
	// under the real SPI while the registry, the bind, the teardown and `lsirq` all named another.
	Some(spi)
}

// Write a device's MSI-X table entry 0 (reached through the physical direct map): the
// message address is the GICv2m frame's MSI_SETSPI_NS register, so the device's DMA
// write of the message data (the SPI number) raises that SPI in the GIC. Vector
// control = 0 (unmasked). A driver must never write its own MSI-X table; only the
// kernel programs it here.
fn program_msix_entry(table_phys: u64, spi: u32) {
	let entry = super::paging::phys_to_virt(table_phys) as *mut u32;
	let msg_addr = GICV2M_FRAME_BASE + MSI_SETSPI_NS;
	unsafe {
		entry.add(0).write_volatile(msg_addr as u32); // message address low
		entry.add(1).write_volatile((msg_addr >> 32) as u32); // message address high
		entry.add(2).write_volatile(spi); // message data = the SPI number
		entry.add(3).write_volatile(0); // vector control (unmasked)
	}
}

// Bind `intr` to an MSI `vector` (an SPI INTID) so dispatch wakes it when the SPI
// fires. Returns false if the vector is already bound to a live Interrupt.
pub fn bind_msi(vector: u32, intr: &Arc<Interrupt>) -> bool {
	match spi_slot(vector) {
		Some(slot) => REGISTRY.bind(slot, intr),
		None => false,
	}
}

// Whether `vector` currently has a live driver binding. Used to confirm a crashed
// driver's IRQ was detached during cleanup.
pub fn is_bound(vector: u32) -> bool {
	match spi_slot(vector) {
		Some(slot) => REGISTRY.is_bound(slot),
		None => false,
	}
}

// End-of-interrupt for a serviced vector. MSI on aarch64 is edge-triggered and
// unshared, so there is no level source to complete: a no-op, kept for the portable
// SYS_INTERRUPT_ACK path (the riscv PLIC completes its level source here).
pub fn eoi(_vector: u32) {}

// Deliver a fired GIC INTID to a bound MSI driver, if it is one of the frame's SPIs.
// Returns true when the INTID was an MSI vector (handled here), so gic::handle_irq can
// tell it apart from the timer and other INTIDs. Edge-triggered: just wake the bound
// driver - there is no level source to mask.
pub fn dispatch_msi(intid: u32) -> bool {
	match spi_slot(intid) {
		Some(slot) => {
			REGISTRY.dispatch(slot);
			true
		}
		None => false,
	}
}

// The state of the MSI vector at `index` (its slot), for the `lsirq` inventory. Index
// 0 is the kernel's own timer - the EL1 physical-timer PPI (INTID 30 on QEMU virt),
// always in use - so the inventory shows a fixed kernel vector like x86's; the MSI
// window (each a device's per-device SPI) follows.
// Free every MSI vector that is masked and waiting for `device` to be confirmed stopped, and answer
// how many. Reached from `SYS_DEVICE_QUIESCED`.
pub fn release_msi_for_device(device: u32) -> usize {
	REGISTRY.release_for_device(device)
}

pub fn irq_info(index: usize) -> Option<abi::IrqInfo> {
	const TIMER_INTID: u32 = 30; // mirrors gic::TIMER_INTID (the EL1 physical-timer PPI)
	if index == 0 {
		return Some(abi::IrqInfo { vector: TIMER_INTID, kind: abi::IRQ_KIND_FIXED, bound: 1, device: abi::IRQ_NO_DEVICE });
	}
	let slot = index - 1;
	let len = MSI_LEN.load(Ordering::Relaxed);
	if slot >= len {
		return None;
	}
	let vector = BASE_SPI.load(Ordering::Relaxed) + slot as u32;
	Some(abi::IrqInfo { vector, kind: abi::IRQ_KIND_MSI, bound: is_bound(vector) as u32, device: REGISTRY.owner(slot) })
}

// The number of vectors irq_info reports over (the timer entry plus the frame's MSI SPIs).
pub fn irq_info_len() -> usize {
	1 + MSI_LEN.load(Ordering::Relaxed)
}

// No kernel-side INTx handlers on aarch64 (the timer is handled in gic::handle_irq).
pub fn register(_vector: u32, _handler: HandlerFn) {}

// Read the GICv2m frame's MSI SPI range (base SPI + count) so acquire_msi/dispatch can
// map slots to SPI INTIDs. Called once, after the GIC is up.
pub fn init() {
	let typer = unsafe { core::ptr::read_volatile(super::paging::phys_to_virt(GICV2M_FRAME_BASE + MSI_TYPER) as *const u32) };
	let base = (typer >> 16) & 0x3ff;
	let count = (typer & 0x3ff) as usize;
	BASE_SPI.store(base, Ordering::Relaxed);
	MSI_LEN.store(count.min(MAX_MSI), Ordering::Relaxed);
}

#[cfg(test)]
mod tests;
