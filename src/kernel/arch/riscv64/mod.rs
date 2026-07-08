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

// --------------------------------------------------------------------- smp
pub mod smp;

// -------------------------------------------------------------------- plic
pub mod plic;

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

	pub fn send_wake_ipi(dest: u32) {
		// SBI IPI extension (EID 0x735049 "sPI", FID 0): raise a supervisor software
		// interrupt on the target hart so it leaves wfi and re-checks the run queue.
		unsafe {
			core::arch::asm!(
				"ecall",
				in("a7") 0x735049usize,
				in("a6") 0usize,
				in("a0") 1usize,          // hart_mask = 1 bit, based at `dest`
				in("a1") dest as usize,   // hart_mask_base
				lateout("a0") _,
				options(nostack),
			);
		}
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

	// Enable the S-mode timer interrupt (SIE.STIE, bit 5), the software interrupt
	// (SIE.SSIE, bit 1, for cross-hart wake IPIs), and the external interrupt (SIE.SEIE,
	// bit 9, for PLIC-routed device interrupts), then arm the first tick.
	pub fn init() {
		unsafe {
			core::arch::asm!("csrs sie, {}", in(reg) (1u64 << 5) | (1u64 << 1) | (1u64 << 9), options(nostack, preserves_flags));
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
	// QEMU virt exposes a Goldfish RTC (device tree "rtc@101000"): TIME_LOW then
	// TIME_HIGH read the nanoseconds since the Unix epoch (reading LOW latches HIGH).
	const RTC_BASE: u64 = 0x0010_1000;
	pub fn read_unix() -> u64 {
		unsafe {
			let lo = core::ptr::read_volatile(super::paging::phys_to_virt(RTC_BASE) as *const u32) as u64;
			let hi = core::ptr::read_volatile(super::paging::phys_to_virt(RTC_BASE + 4) as *const u32) as u64;
			((hi << 32) | lo) / 1_000_000_000
		}
	}
}

// ------------------------------------------------------------------ random
// (RISC-V has no guaranteed userspace entropy source, so this is a splitmix64 stream
// seeded and re-stirred from the cycle counter - the same fallback the other arches
// use when their hardware RNG is absent.)
pub mod random {
	use core::sync::atomic::{AtomicU64, Ordering};

	static STATE: AtomicU64 = AtomicU64::new(0);

	pub fn fill(buf: &mut [u8]) {
		let mut s = STATE.load(Ordering::Relaxed) ^ super::tsc::now() ^ 0x9E37_79B9_7F4A_7C15;
		for chunk in buf.chunks_mut(8) {
			let z = crate::arch::common::rng::splitmix64(&mut s);
			let bytes = z.to_le_bytes();
			chunk.copy_from_slice(&bytes[..chunk.len()]);
		}
		STATE.store(s, Ordering::Relaxed);
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
	// STVEC is already installed (traps::init), so a U-mode ecall lands in
	// __trap_entry -> riscv64_trap -> dispatch. Nothing extra to program here.
	pub fn init() {}

	pub unsafe fn invoke(num: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
		// A ring-0 (kernel-context) system call: route straight to the portable syscall
		// table, the way the in-kernel callers and the test harness use it. Mark this a
		// kernel caller (from_user = false) so buffer checks accept kernel-owned buffers -
		// U-mode calls arrive through the ecall trap and `dispatch`, which sets it itself.
		super::percpu::set_from_user(false);
		crate::syscall::syscall_dispatch(num, a0, a1, a2, a3)
	}

	// Dispatch a U-mode ecall against the saved trap frame (a7 = syscall number,
	// a0..a3 = arguments, the result is written back into the a0 slot). Routes to the
	// portable kernel syscall table. Returns `true` for SYS_USER_EXIT (the caller then
	// unwinds back to the kernel thread that entered U-mode), `false` to `sret` back to
	// the user program with the result in a0.
	pub unsafe fn dispatch(frame: *mut u64) -> bool {
		let num = unsafe { *frame.add(17) }; // a7
		if num == abi::SYS_USER_EXIT {
			return true;
		}
		let (a0, a1, a2, a3) = unsafe { (*frame.add(10), *frame.add(11), *frame.add(12), *frame.add(13)) };
		super::percpu::set_from_user(true);
		let result = crate::syscall::syscall_dispatch(num, a0, a1, a2, a3);
		super::percpu::set_from_user(false);
		unsafe { *frame.add(10) = result };
		false
	}
}

// ---------------------------------------------------------------- usermode
pub mod usermode;

// --------------------------------------------------------------------- pci
// PCI config space is a bus standard; only the config-space ACCESS mechanism is
// arch-specific (x86 ports vs riscv64 ECAM MMIO). The device types + scan logic are
// portable (`arch::common::pci`); the riscv64 ECAM `ConfigAccess` backend lives in
// `pci.rs`.
pub mod pci;
