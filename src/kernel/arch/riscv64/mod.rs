// riscv64 (RISC-V) architecture backend.
//
// STATUS: STUB. This module satisfies the same arch contract as `arch::x86_64`
// (the set of `arch::*` symbols the portable kernel calls - see the contract
// listed in `arch/mod.rs`) so that a cross-build for `riscv64gc-unknown-none-elf`
// links, but nothing here is implemented yet: the RISC-V mechanics (Sv39 page
// tables via SATP, the STVEC trap vector, PLIC + CLINT / SBI timer, SBI HSM
// hart_start SMP wake, ECALL syscall, the `tp` per-CPU register, the 16550 / SBI
// console, DTB parsing) land in M117. Runtime entry points `todo!()`; a boot on
// this arch is not possible until then.

pub mod boot;
pub mod dtb;
pub mod serial;
pub mod traps;

// halt the kernel forever (wait-for-interrupt)
pub fn halt_loop() -> ! {
	loop {
		unsafe {
			core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
		}
	}
}

// install the trap vector and enable memory-protection features
pub fn init() {
	todo!("riscv64: STVEC + page-protection bits (M117)")
}

pub fn init_interrupts() {
	todo!("riscv64: PLIC + CLINT / SBI timer (M117)")
}

pub fn init_syscalls() {
	todo!("riscv64: ECALL vector wiring (M117)")
}

pub fn init_tsc() {
	todo!("riscv64: timebase-frequency (M117)")
}

pub fn init_bsp_percpu(_hartid: u32) {
	todo!("riscv64: tp register for the boot hart (M117)")
}

pub fn init_ap(_cpu_id: usize, _hartid: u32) {
	todo!("riscv64: secondary-hart bring-up (M117)")
}

// enable maskable interrupts on the current hart (set SSTATUS.SIE, bit 1)
pub fn enable_interrupts() {
	unsafe {
		core::arch::asm!("csrsi sstatus, 2", options(nomem, nostack, preserves_flags));
	}
}

pub fn disable_interrupts() {
	unsafe {
		core::arch::asm!("csrci sstatus, 2", options(nomem, nostack, preserves_flags));
	}
}

// True if supervisor interrupts are currently enabled (SSTATUS.SIE, bit 1).
pub fn interrupts_enabled() -> bool {
	let sstatus: u64;
	unsafe {
		core::arch::asm!("csrr {}, sstatus", out(reg) sstatus, options(nomem, nostack, preserves_flags));
	}
	sstatus & (1 << 1) != 0
}

// idle the hart until an interrupt (enable interrupts, then wait-for-interrupt)
pub fn idle_halt() {
	unsafe {
		core::arch::asm!("csrsi sstatus, 2", "wfi", options(nomem, nostack, preserves_flags));
	}
}

// reboot / power off via the SBI SRST extension - stubbed to a halt.
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
		todo!("riscv64 PLIC (M117)")
	}
}

// -------------------------------------------------------------------- apic
// (the riscv64 interrupt controller is the PLIC/CLINT; the module keeps the
// portable `apic` name for the contract until the ports rename it. The periodic
// scheduler tick is the S-mode timer, armed through the SBI TIME extension.)
pub mod apic {
	use crate::arch::common::time::TICK_HZ;
	use core::sync::atomic::{AtomicU64, Ordering};

	// Monotonic scheduler-tick counter (advanced by the timer interrupt).
	static TICKS: AtomicU64 = AtomicU64::new(0);
	// The boot hart id, captured at init (the local "apic" id).
	static BOOT_HART: AtomicU64 = AtomicU64::new(0);

	pub fn set_boot_hart(hartid: u64) {
		BOOT_HART.store(hartid, Ordering::Relaxed);
	}

	// Set the next S-mode timer interrupt via the legacy SBI set_timer (EID 0x00),
	// which also clears the pending timer bit.
	fn sbi_set_timer(when: u64) {
		unsafe {
			core::arch::asm!("ecall", in("a7") 0usize, in("a0") when, lateout("a0") _, options(nostack, preserves_flags));
		}
	}

	// Arm the next periodic tick: now + timebase / TICK_HZ.
	pub fn arm_timer() {
		let interval = super::tsc::hz() / TICK_HZ as u64;
		sbi_set_timer(super::tsc::now() + interval);
	}

	pub fn local_id() -> u32 {
		BOOT_HART.load(Ordering::Relaxed) as u32
	}

	// The timer is re-armed inside its interrupt handler, so EOI is a no-op.
	pub fn eoi() {}

	pub fn send_wake_ipi(_dest: u32) {
		// SBI IPI for the cross-hart scheduler wake - wired with the SMP increment.
	}
	pub fn send_init(_dest: u32) {}
	pub fn send_startup(_dest: u32, _vector: u8) {}

	pub fn ticks() -> u64 {
		TICKS.load(Ordering::Relaxed)
	}

	// Advance the tick counter and re-arm the timer. Called from the S-mode timer
	// interrupt (traps.rs).
	pub fn on_timer_tick() {
		TICKS.fetch_add(1, Ordering::Relaxed);
		arm_timer();
	}

	// Enable the S-mode timer interrupt (SIE.STIE, bit 5) and arm the first tick.
	pub fn init() {
		unsafe {
			core::arch::asm!("csrs sie, {}", in(reg) 1u64 << 5, options(nostack, preserves_flags));
		}
		arm_timer();
	}

	pub fn init_ap() {
		init();
	}
}

// --------------------------------------------------------------------- tsc
// The RISC-V `time` CSR is the monotonic cycle clock (read with a plain csrr); it
// counts at the fixed CLINT timebase (10 MHz on QEMU virt).
pub mod tsc {
	pub fn now() -> u64 {
		let t: u64;
		unsafe {
			core::arch::asm!("csrr {}, time", out(reg) t, options(nomem, nostack, preserves_flags));
		}
		t
	}
	pub fn init() {}
	pub fn hz() -> u64 {
		10_000_000 // QEMU virt CLINT timebase (aclint-mtimer @ 10 MHz)
	}
	pub fn cycles_to_ns(cycles: u64) -> u64 {
		crate::arch::common::time::cycles_to_ns(cycles, hz())
	}
}

// ------------------------------------------------------------------ ioapic
pub mod ioapic {
	pub fn route(_gsi: u32, _vector: u8, _dest: u32) {
		todo!("riscv64 PLIC routing (M117)")
	}
	pub fn init() {
		todo!("riscv64 PLIC (M117)")
	}
	pub fn mask(_gsi: u32) {
		todo!("riscv64 PLIC mask (M117)")
	}
}

// --------------------------------------------------------------------- rtc
pub mod rtc {
	pub fn read_unix() -> u64 {
		todo!("riscv64 Goldfish RTC (M117)")
	}
}

// ------------------------------------------------------------------ random
pub mod random {
	pub fn fill(_buf: &mut [u8]) {
		todo!("riscv64 entropy source (M117)")
	}
}

// ------------------------------------------------------------------ apboot
// (riscv64 wakes secondary harts via the SBI HSM `hart_start` call, not a
// real-mode trampoline; these keep the portable names so smp.rs links until
// M117 replaces the wake path.)
pub mod apboot {
	pub fn trampoline_len() -> usize {
		0
	}
	pub unsafe fn install(_dst: *mut u8, _satp: u64, _entry: u64) {
		todo!("riscv64 SBI HSM wake (M117)")
	}
	pub unsafe fn set_stack(_dst: *mut u8, _stack_top: u64) {
		todo!("riscv64 SBI HSM wake (M117)")
	}
}

// ----------------------------------------------------------------- syscall
pub mod syscall {
	pub fn init() {
		todo!("riscv64 ECALL wiring (M117)")
	}
	pub unsafe fn invoke(_num: u64, _a0: u64, _a1: u64, _a2: u64, _a3: u64) -> u64 {
		todo!("riscv64 ECALL (M117)")
	}
}

// ---------------------------------------------------------------- usermode
pub mod usermode {
	pub const FAULT_PROBE_ADDR: u64 = 0x0dea_d000;

	pub unsafe fn enter(_entry: u64, _user_stack: u64, _arg: u64) {
		todo!("riscv64 U-mode entry (M117)")
	}
	pub fn exit_to_kernel() -> ! {
		todo!("riscv64 U-mode return (M117)")
	}
	pub fn program_bytes() -> &'static [u8] {
		todo!("riscv64 test program bytes (M117)")
	}
	pub fn program_fault_bytes() -> &'static [u8] {
		todo!("riscv64 test program bytes (M117)")
	}
	pub fn program_yield_bytes() -> &'static [u8] {
		todo!("riscv64 test program bytes (M117)")
	}
	pub fn program_nx_bytes() -> &'static [u8] {
		todo!("riscv64 test program bytes (M117)")
	}
	pub fn program_stack_probe_bytes() -> &'static [u8] {
		todo!("riscv64 test program bytes (M117)")
	}
	pub fn program_spin_bytes() -> &'static [u8] {
		todo!("riscv64 test program bytes (M117)")
	}
}

// --------------------------------------------------------------------- pci
// PCI config space is a bus standard; only the config-space ACCESS mechanism is
// arch-specific (x86 ports vs riscv64 ECAM MMIO). The device types + scan logic are
// portable (`arch::common::pci`); M117 adds the riscv64 `ConfigAccess` backend, at
// which point these scans call `common::scan*::<Access>()` like the other arches. For
// now they return empty so the tree links, reusing the shared types.
pub mod pci {
	use alloc::vec::Vec;

	#[allow(unused_imports)]
	pub use crate::arch::common::pci::{PciDevice, VirtioCap, VirtioDevice, XhciDevice, virtio_type_name};

	pub fn scan() -> Vec<PciDevice> {
		Vec::new()
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
