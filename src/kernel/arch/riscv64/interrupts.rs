// riscv64 device-interrupt binding via wired INTx routed to the PLIC (M117).
//
// QEMU's `virt` PCIe host bridge has NO MSI/MSI-X delivery: a device signals with a
// wired INTx pin, routed through the standard PCIe swizzle to one of the PLIC's four
// PCIe sources (0x20..0x23). So, unlike the x86 LAPIC-MSI and aarch64 GICv2m backends,
// a device's "vector" here is its PLIC source id, and delivery is LEVEL-triggered: the
// PLIC gateway holds the source between claim and complete, and the device keeps its
// pin asserted until the driver deasserts it (a virtio driver reads the ISR-status
// register; xHCI clears IMAN.IP). The acquire path therefore re-enables the device's
// INTx pin (device::init disabled it) and unmasks the PLIC source; plic::handle_external
// signals the bound Interrupt WITHOUT completing (the gateway masks the source); and the
// SYS_INTERRUPT_ACK eoi completes it - by then the driver has deasserted the pin, so the
// source does not immediately re-fire.
//
// The portable MSI syscalls drive this unchanged: device_msix_acquire calls acquire_msi
// (which ignores the MSI-X table there is none to program and returns the PLIC source)
// then pci::msix_enable (a no-op on riscv: enabling PCI MSI-X would move the device off
// its INTx pin to a message the PLIC cannot receive), and interrupt_ack calls eoi.

#![allow(dead_code)]

use alloc::sync::Arc;

use crate::arch::common::msi::MsiRegistry;
use crate::object::interrupt::Interrupt;

// The device-IRQ vector window base (mirrors the contract). On riscv it is the first
// PCIe INTx PLIC source, so a vector doubles as its PLIC source id.
pub const IRQ_BASE: u8 = 32;

pub type HandlerFn = fn(u8);

// The QEMU virt PCIe INTx sources on the PLIC: the four INTx lines A..D map to PLIC
// sources 0x20..0x23, selected by the standard PCIe swizzle (pin + slot) % 4.
const PCIE_INTX_BASE: u32 = 0x20;
const PCIE_INTX_LINES: u32 = 4;

// Bindings for every PLIC source (the vector IS the source id). MAX_SOURCES matches the
// PLIC's source table; a driver's Interrupt is held weakly, so a gone driver clears its
// own binding on Drop.
const MAX_SOURCES: usize = 96;
static REGISTRY: MsiRegistry<MAX_SOURCES> = MsiRegistry::new();

// The PLIC source a device's INTx pin lands on: swizzle its PCI slot with its pin.
fn intx_source(bus: u8, dev: u8, func: u8) -> u32 {
	let pin = super::pci::interrupt_pin(bus, dev, func).max(1) as u32; // 1..4 (A..D)
	PCIE_INTX_BASE + ((dev as u32 + pin - 1) % PCIE_INTX_LINES)
}

// No legacy sys_interrupt_bind path on riscv: a driver takes its device interrupt
// through device_msix_acquire (acquire_msi + bind_msi), which the DeviceManager already
// uses on every arch. is_bindable / bind therefore always refuse, like aarch64.
pub fn is_bindable(_vector: u8) -> bool {
	false
}

pub fn bind(_vector: u8, _intr: &Arc<Interrupt>) -> bool {
	false
}

// Remove a vector's binding (called from an Interrupt's Drop): mask its PLIC source so
// the device cannot storm the now-unhandled line, then free the slot.
pub fn unbind(vector: u8) {
	super::plic::disable_source(vector as u32, super::plic::boot_hart());
	REGISTRY.free(vector as usize);
}

// Acquire the INTx-over-PLIC "MSI" for the discovered device at `owner`: resolve the
// PLIC source its wired pin lands on, reserve it, re-enable the device's INTx pin
// (device::init disabled every pin), and route + unmask the source on the boot hart.
// Returns the source as the vector; None if the source is out of range or already taken
// (INTx sharing among two interrupt-driven devices is not supported yet - the block
// drivers poll, so only the NIC and xHCI bind, and they land on distinct sources).
// `table_phys` / `dest` (the MSI-table and LAPIC-target inputs) are unused: there is no
// MSI-X table to program and the PLIC routes the source itself.
pub fn acquire_msi(_table_phys: u64, _dest: u8, owner: u32) -> Option<u8> {
	let (bus, dev, func) = crate::device::with(owner as usize, |d| (d.bus, d.dev, d.func))?;
	let source = intx_source(bus, dev, func);
	if source as usize >= MAX_SOURCES {
		return None;
	}
	if !REGISTRY.acquire_at(source as usize, owner) {
		return None;
	}
	super::pci::set_intx_disabled(bus, dev, func, false);
	super::plic::enable_source(source, super::plic::boot_hart());
	Some(source as u8)
}

// Bind `intr` to a `vector` (a PLIC source) so dispatch wakes it when the source fires.
// Returns false if the source is already bound to a live Interrupt.
pub fn bind_msi(vector: u8, intr: &Arc<Interrupt>) -> bool {
	REGISTRY.bind(vector as usize, intr)
}

// Whether `vector` currently has a live driver binding.
pub fn is_bound(vector: u8) -> bool {
	REGISTRY.is_bound(vector as usize)
}

// Deliver a fired PLIC `source` to a bound driver. Returns true when the source was a
// bound device INTx (signaled here), so plic::handle_external leaves it claimed for the
// driver's ack instead of completing it (level-triggered). False for an unbound source.
pub fn dispatch_intx(source: u32) -> bool {
	if (source as usize) < MAX_SOURCES && REGISTRY.is_bound(source as usize) {
		REGISTRY.dispatch(source as usize);
		true
	} else {
		false
	}
}

// End-of-interrupt for a serviced device INTx: complete its PLIC source so the gateway
// forwards the next assertion. Called from SYS_INTERRUPT_ACK, which first cleared the
// Interrupt's pending flag; the driver has already deasserted its device line, so the
// completed source does not immediately re-fire. Completion always targets the boot
// hart's context (where the source was enabled and claimed).
pub fn eoi(vector: u8) {
	super::plic::complete(super::plic::boot_hart(), vector as u32);
}

// The state of the vector at `index`, for the `lsirq` inventory. Index 0 is the
// kernel's own timer - the S-mode timer interrupt (SCAUSE code 5, armed through the SBI
// TIME extension), always in use and shown as a fixed vector like x86's LAPIC timer and
// aarch64's EL1 physical-timer PPI. The four PCIe INTx PLIC sources (0x20..0x23) follow.
pub fn irq_info(index: usize) -> Option<abi::IrqInfo> {
	const TIMER_VECTOR: u32 = 5; // supervisor timer interrupt (scause code 5)
	if index == 0 {
		return Some(abi::IrqInfo { vector: TIMER_VECTOR, kind: abi::IRQ_KIND_FIXED, bound: 1, device: abi::IRQ_NO_DEVICE });
	}
	let line = index - 1;
	if line >= PCIE_INTX_LINES as usize {
		return None;
	}
	let source = PCIE_INTX_BASE + line as u32;
	Some(abi::IrqInfo { vector: source, kind: abi::IRQ_KIND_FIXED, bound: is_bound(source as u8) as u32, device: REGISTRY.owner(source as usize) })
}

// The number of vectors irq_info reports over (the timer entry plus the four PCIe INTx sources).
pub fn irq_info_len() -> usize {
	1 + PCIE_INTX_LINES as usize
}

// No kernel-side INTx handler registration on riscv (device interrupts route to
// userspace drivers via the registry; the timer is the S-mode timer, not a PLIC source).
pub fn register(_vector: u8, _handler: HandlerFn) {}

// The PLIC is brought up on the boot hart in boot.rs (plic::init), before any device is
// discovered, so there is nothing left to initialize here.
pub fn init() {}
