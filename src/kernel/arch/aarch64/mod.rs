// aarch64 (ARM64) architecture backend.
//
// STATUS: STUB. This module satisfies the same arch contract as `arch::x86_64`
// (the set of `arch::*` symbols the portable kernel calls - see the contract
// listed in `arch/mod.rs`) so that a cross-build for `aarch64-unknown-none`
// links, but nothing here is implemented yet: the ARMv8-A mechanics (VMSAv8
// page tables, the VBAR_EL1 vector table, the GIC + generic timer, PSCI SMP
// wake, SVC syscall, TPIDR_EL1 per-CPU, PL011 UART, DTB parsing) land in M116.
// Runtime entry points `todo!()`; a boot on this arch is not possible until then.

mod boot;
mod dtb;
mod exceptions;
mod gic;
mod psci;
pub mod serial;

// halt the kernel forever (wait-for-event)
pub fn halt_loop() -> ! {
	loop {
		unsafe {
			core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
		}
	}
}

// install the CPU exception vectors and enable memory-protection features
pub fn init() {
	todo!("aarch64: VBAR_EL1 + MMU protection bits (M116)")
}

pub fn init_interrupts() {
	todo!("aarch64: GIC + generic timer (M116)")
}

pub fn init_syscalls() {
	todo!("aarch64: SVC vector wiring (M116)")
}

pub fn init_tsc() {
	todo!("aarch64: generic-timer frequency (M116)")
}

pub fn init_bsp_percpu(_mpidr: u32) {
	todo!("aarch64: TPIDR_EL1 for the boot core (M116)")
}

pub fn init_ap(_cpu_id: usize, _mpidr: u32) {
	todo!("aarch64: secondary-core bring-up (M116)")
}

// enable maskable interrupts on the current core (clear DAIF.I)
pub fn enable_interrupts() {
	unsafe {
		core::arch::asm!("msr daifclr, #2", options(nomem, nostack, preserves_flags));
	}
}

pub fn disable_interrupts() {
	unsafe {
		core::arch::asm!("msr daifset, #2", options(nomem, nostack, preserves_flags));
	}
}

// True if IRQs are currently unmasked (DAIF.I clear, bit 7).
pub fn interrupts_enabled() -> bool {
	let daif: u64;
	unsafe {
		core::arch::asm!("mrs {}, daif", out(reg) daif, options(nomem, nostack, preserves_flags));
	}
	daif & (1 << 7) == 0
}

// idle the core until an interrupt (enable IRQs, then wait-for-interrupt)
pub fn idle_halt() {
	unsafe {
		core::arch::asm!("msr daifclr, #2", "wfi", options(nomem, nostack, preserves_flags));
	}
}

// reboot / power off via PSCI (SYSTEM_RESET / SYSTEM_OFF) - stubbed to a halt.
pub fn reset() -> ! {
	halt_loop()
}

pub fn poweroff() -> ! {
	halt_loop()
}

#[cfg(test)]
pub fn exit_qemu(_success: bool) -> ! {
	halt_loop()
}

// ------------------------------------------------------------------ paging
pub mod paging;

// ----------------------------------------------------------------- context
pub mod context;

// ------------------------------------------------------------------ percpu
pub mod percpu;

// -------------------------------------------------------------- interrupts
pub mod interrupts {
	use crate::object::interrupt::Interrupt;
	use alloc::sync::Arc;

	pub const IRQ_BASE: u8 = 32;

	pub type HandlerFn = fn(u8);

	pub fn is_bindable(_vector: u8) -> bool {
		false
	}
	pub fn bind(_vector: u8, _intr: &Arc<Interrupt>) -> bool {
		false
	}
	pub fn unbind(_vector: u8) {}
	pub fn acquire_msi(_table_phys: u64, _dest: u8, _owner: u32) -> Option<u8> {
		None
	}
	pub fn irq_info(_index: usize) -> Option<abi::IrqInfo> {
		None
	}
	pub fn irq_info_len() -> usize {
		0
	}
	pub fn bind_msi(_vector: u8, _intr: &Arc<Interrupt>) -> bool {
		false
	}
	pub fn is_bound(_vector: u8) -> bool {
		false
	}
	pub fn register(_vector: u8, _handler: HandlerFn) {}
	pub fn init() {
		todo!("aarch64 GIC (M116)")
	}
}

// -------------------------------------------------------------------- apic
// (the aarch64 interrupt controller is the GIC; the module keeps the portable
// `apic` name for the contract until the ports rename it.)
pub mod apic {
	pub fn local_id() -> u32 {
		todo!("aarch64 GIC/MPIDR (M116)")
	}
	pub fn eoi() {
		todo!("aarch64 GIC EOI (M116)")
	}
	pub fn send_wake_ipi(_dest: u32) {
		todo!("aarch64 GIC SGI (M116)")
	}
	pub fn send_init(_dest: u32) {
		todo!("aarch64 PSCI wake (M116)")
	}
	pub fn send_startup(_dest: u32, _vector: u8) {
		todo!("aarch64 PSCI wake (M116)")
	}
	pub fn ticks() -> u64 {
		todo!("aarch64 generic timer (M116)")
	}
	pub fn init() {
		todo!("aarch64 GIC + timer (M116)")
	}
	pub fn init_ap() {
		todo!("aarch64 GIC per-core (M116)")
	}
}

// --------------------------------------------------------------------- tsc
pub mod tsc {
	pub fn now() -> u64 {
		todo!("aarch64 CNTVCT (M116)")
	}
	pub fn init() {
		todo!("aarch64 CNTFRQ (M116)")
	}
	pub fn hz() -> u64 {
		todo!("aarch64 CNTFRQ (M116)")
	}
	pub fn cycles_to_ns(_cycles: u64) -> u64 {
		todo!("aarch64 timer scaling (M116)")
	}
}

// ------------------------------------------------------------------ ioapic
pub mod ioapic {
	pub fn route(_gsi: u32, _vector: u8, _dest: u32) {
		todo!("aarch64 GIC routing (M116)")
	}
	pub fn init() {
		todo!("aarch64 GIC distributor (M116)")
	}
	pub fn mask(_gsi: u32) {
		todo!("aarch64 GIC mask (M116)")
	}
}

// --------------------------------------------------------------------- rtc
pub mod rtc {
	pub fn read_unix() -> u64 {
		todo!("aarch64 PL031 RTC (M116)")
	}
}

// ------------------------------------------------------------------ random
pub mod random {
	pub fn fill(_buf: &mut [u8]) {
		todo!("aarch64 RNDR / DTB entropy (M116)")
	}
}

// ------------------------------------------------------------------ apboot
// (aarch64 wakes secondaries via PSCI CPU_ON, not a real-mode trampoline; these
// keep the portable names so smp.rs links until M116 replaces the wake path.)
pub mod apboot {
	pub fn trampoline_len() -> usize {
		0
	}
	pub unsafe fn install(_dst: *mut u8, _ttbr: u64, _entry: u64) {
		todo!("aarch64 PSCI wake (M116)")
	}
	pub unsafe fn set_stack(_dst: *mut u8, _stack_top: u64) {
		todo!("aarch64 PSCI wake (M116)")
	}
}

// ----------------------------------------------------------------- syscall
pub mod syscall {
	pub fn init() {
		todo!("aarch64 SVC wiring (M116)")
	}
	pub unsafe fn invoke(_num: u64, _a0: u64, _a1: u64, _a2: u64, _a3: u64) -> u64 {
		todo!("aarch64 SVC (M116)")
	}

	// Dispatch an SVC from EL0 against the saved trap frame (x8 = number, x0.. =
	// arguments, the result is written back into the x0 slot). Returns `true` for
	// the "exit" syscall (the caller then unwinds back to the kernel), `false` to
	// `eret` back to the user program.
	pub unsafe fn dispatch(frame: *mut u64) -> bool {
		let num = unsafe { *frame.add(8) }; // x8
		let a0 = unsafe { *frame.add(0) }; // x0
		match num {
			0 => {
				crate::serial_println!("aarch64: EL0 syscall exit(x0={a0:#x})");
				return true;
			}
			1 => {
				let r = a0 + 1;
				crate::serial_println!("aarch64: EL0 syscall demo(x0={a0:#x}) -> {r:#x}");
				unsafe { *frame.add(0) = r };
			}
			n => {
				crate::serial_println!("aarch64: EL0 syscall {n} unknown");
				unsafe { *frame.add(0) = u64::MAX };
			}
		}
		false
	}
}

// ---------------------------------------------------------------- usermode
pub mod usermode {
	pub const FAULT_PROBE_ADDR: u64 = 0x0dea_d000;

	unsafe extern "C" {
		fn aarch64_enter_el0(entry: u64, user_sp: u64, arg: u64, spsr: u64);
		fn aarch64_exit_el0() -> !;
	}

	// Drop to EL0 at `entry` with SP_EL0 = `user_stack` and x0 = `arg`. SPSR
	// selects EL0t with DAIF masked (0x3C0) so the demo runs uninterrupted; the
	// call "returns" here when the user program makes the exit syscall.
	pub unsafe fn enter(entry: u64, user_stack: u64, arg: u64) {
		unsafe { aarch64_enter_el0(entry, user_stack, arg, 0x3C0) }
	}

	// Unwind from an EL0 syscall back to the kernel that called `enter`.
	pub fn exit_to_kernel() -> ! {
		unsafe { aarch64_exit_el0() }
	}

	// A tiny EL0 program: syscall(1, 0x41) -> 0x42, syscall(1, 0x42) -> 0x43,
	// then syscall(0) to exit. Encoded AArch64 instructions, little-endian.
	pub fn program_bytes() -> &'static [u8] {
		static PROGRAM: [u32; 8] = [
			0xD280_0028, // mov x8, #1
			0xD280_0820, // mov x0, #0x41
			0xD400_0001, // svc #0
			0xD280_0028, // mov x8, #1
			0xD400_0001, // svc #0
			0xD280_0008, // mov x8, #0
			0xD400_0001, // svc #0
			0x1400_0000, // b .
		];
		unsafe { core::slice::from_raw_parts(PROGRAM.as_ptr() as *const u8, core::mem::size_of_val(&PROGRAM)) }
	}
	pub fn program_fault_bytes() -> &'static [u8] {
		todo!("aarch64 test program bytes (M116)")
	}
	pub fn program_yield_bytes() -> &'static [u8] {
		todo!("aarch64 test program bytes (M116)")
	}
	pub fn program_nx_bytes() -> &'static [u8] {
		todo!("aarch64 test program bytes (M116)")
	}
	pub fn program_stack_probe_bytes() -> &'static [u8] {
		todo!("aarch64 test program bytes (M116)")
	}
	pub fn program_spin_bytes() -> &'static [u8] {
		todo!("aarch64 test program bytes (M116)")
	}
}

// --------------------------------------------------------------------- pci
// PCI config space is a bus standard; only the config-space ACCESS mechanism is
// arch-specific (x86 I/O ports vs aarch64 ECAM MMIO). On QEMU's `virt` the PCIe
// ECAM lives at 0x3f00_0000 (16 MiB = 16 buses), inside the low device MMIO
// region the boot identity map already covers as Device memory, so config space
// is reachable directly. `scan` enumerates it; the virtio/xhci capability walks
// fill in with the drivers.
pub mod pci {
	use alloc::vec::Vec;
	use core::sync::atomic::{AtomicUsize, Ordering};

	// PCIe ECAM base (set from the device tree at boot) and the number of buses to
	// probe. On QEMU virt the ECAM is the high-mem window at 0x40_1000_0000.
	static ECAM_BASE: AtomicUsize = AtomicUsize::new(0);
	const ECAM_BUSES: u8 = 16;

	// Record the ECAM base discovered in the device tree.
	pub fn set_ecam_base(base: u64) {
		ECAM_BASE.store(base as usize, Ordering::Relaxed);
	}

	// Byte address of a config-space register for a given B/D/F.
	fn cfg_addr(bus: u8, dev: u8, func: u8, off: usize) -> usize {
		ECAM_BASE.load(Ordering::Relaxed) + ((bus as usize) << 20) + ((dev as usize) << 15) + ((func as usize) << 12) + off
	}
	fn cfg_read32(bus: u8, dev: u8, func: u8, off: usize) -> u32 {
		unsafe { core::ptr::read_volatile(cfg_addr(bus, dev, func, off) as *const u32) }
	}
	fn cfg_read16(bus: u8, dev: u8, func: u8, off: usize) -> u16 {
		unsafe { core::ptr::read_volatile(cfg_addr(bus, dev, func, off) as *const u16) }
	}
	fn cfg_read8(bus: u8, dev: u8, func: u8, off: usize) -> u8 {
		unsafe { core::ptr::read_volatile(cfg_addr(bus, dev, func, off) as *const u8) }
	}

	// Read the fixed header fields of one present function into a PciDevice.
	fn read_function(bus: u8, dev: u8, func: u8, vendor: u16) -> PciDevice {
		let mut bars = [0u32; 6];
		for (i, bar) in bars.iter_mut().enumerate() {
			*bar = cfg_read32(bus, dev, func, 0x10 + i * 4);
		}
		PciDevice { bus, dev, func, vendor, device_id: cfg_read16(bus, dev, func, 0x02), class: cfg_read8(bus, dev, func, 0x0b), subclass: cfg_read8(bus, dev, func, 0x0a), prog_if: cfg_read8(bus, dev, func, 0x09), header_type: cfg_read8(bus, dev, func, 0x0e), bars }
	}

	#[derive(Clone, Copy)]
	pub struct PciDevice {
		pub bus: u8,
		pub dev: u8,
		pub func: u8,
		pub vendor: u16,
		pub device_id: u16,
		pub class: u8,
		pub subclass: u8,
		pub prog_if: u8,
		pub header_type: u8,
		pub bars: [u32; 6],
	}

	#[derive(Clone, Copy, Default)]
	pub struct VirtioCap {
		pub bar: u8,
		pub offset: u32,
		pub length: u32,
		pub notify_multiplier: u32,
	}

	#[derive(Clone, Copy)]
	pub struct VirtioDevice {
		pub pci: PciDevice,
		pub virtio_type: u16,
		pub bar: u8,
		pub bar_phys: u64,
		pub region_len: u64,
		pub common: VirtioCap,
		pub notify: VirtioCap,
		pub isr: VirtioCap,
		pub device: VirtioCap,
		pub msix_cap: u16,
		pub msix_count: u16,
		pub msix_table_phys: u64,
	}

	#[derive(Clone, Copy)]
	pub struct XhciDevice {
		pub pci: PciDevice,
		pub bar_phys: u64,
		pub bar_len: u64,
		pub msix_cap: u16,
		pub msix_count: u16,
		pub msix_table_phys: u64,
	}

	// Enumerate every present function on the ECAM bus.
	pub fn scan() -> Vec<PciDevice> {
		let mut out = Vec::new();
		if ECAM_BASE.load(Ordering::Relaxed) == 0 {
			return out;
		}
		for bus in 0..ECAM_BUSES {
			for dev in 0..32u8 {
				if cfg_read16(bus, dev, 0, 0x00) == 0xffff {
					continue;
				}
				// Multi-function devices (header type bit 7) expose funcs 1..8.
				let funcs = if cfg_read8(bus, dev, 0, 0x0e) & 0x80 != 0 { 8 } else { 1 };
				for func in 0..funcs {
					let vendor = cfg_read16(bus, dev, func, 0x00);
					if vendor == 0xffff {
						continue;
					}
					out.push(read_function(bus, dev, func, vendor));
				}
			}
		}
		out
	}
	pub fn scan_virtio() -> Vec<VirtioDevice> {
		Vec::new()
	}
	pub fn scan_xhci() -> Vec<XhciDevice> {
		Vec::new()
	}
	pub fn set_intx_disabled(_bus: u8, _dev: u8, _func: u8, _disabled: bool) {}
	pub fn msix_enable(_bus: u8, _dev: u8, _func: u8, _cap: u16) {}
}
