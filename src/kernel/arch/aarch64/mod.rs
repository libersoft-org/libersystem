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
mod virtio_blk;

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

// Enable Advanced SIMD / floating-point access at EL0 and EL1 (CPACR_EL1.FPEN =
// 0b11), so FP/vector instructions - which the compiler emits for bulk memory
// operations - do not trap (EC 0x7). Called once per core during bring-up.
pub fn enable_fp() {
	unsafe {
		let mut cpacr: u64;
		core::arch::asm!("mrs {}, cpacr_el1", out(reg) cpacr, options(nomem, nostack, preserves_flags));
		cpacr |= 3 << 20;
		core::arch::asm!("msr cpacr_el1, {}", "isb", in(reg) cpacr, options(nostack, preserves_flags));
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
		// The running core's MPIDR affinity (Aff0 identifies the core on virt).
		let mpidr: u64;
		unsafe {
			core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
		}
		(mpidr & 0xff_ffff) as u32
	}
	pub fn eoi() {
		// aarch64 signals end-of-interrupt per IRQ inside gic::handle_irq (GICC_EOIR).
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
		super::gic::ticks()
	}
	pub fn init() {
		todo!("aarch64 GIC + timer (M116)")
	}
	pub fn init_ap() {
		todo!("aarch64 GIC per-core (M116)")
	}
}

// --------------------------------------------------------------------- tsc
// The ARM generic timer is the monotonic cycle clock: CNTVCT_EL0 counts at the
// fixed CNTFRQ_EL0 rate (62.5 MHz on QEMU virt), resetting to 0 at power-on.
pub mod tsc {
	use core::arch::asm;

	pub fn now() -> u64 {
		let v: u64;
		unsafe {
			asm!("mrs {}, cntvct_el0", out(reg) v, options(nomem, nostack, preserves_flags));
		}
		v
	}
	pub fn init() {}
	pub fn hz() -> u64 {
		let f: u64;
		unsafe {
			asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack, preserves_flags));
		}
		f
	}
	pub fn cycles_to_ns(cycles: u64) -> u64 {
		let f = hz();
		if f == 0 { 0 } else { (cycles as u128 * 1_000_000_000 / f as u128) as u64 }
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
// The PL031 real-time clock (QEMU virt at 0x0901_0000): its data register holds
// the current time as seconds since the Unix epoch. In the low device MMIO region
// the boot map already covers.
pub mod rtc {
	pub fn read_unix() -> u64 {
		const PL031_DR: usize = 0x0901_0000;
		unsafe { core::ptr::read_volatile(PL031_DR as *const u32) as u64 }
	}
}

// ------------------------------------------------------------------ random
// No architectural RNG is guaranteed on the bring-up core (FEAT_RNG / RNDR is
// optional), so this is a splitmix64 stream seeded and re-stirred from the
// generic-timer counter. Adequate for non-cryptographic kernel needs during
// bring-up; a real entropy source replaces it later.
pub mod random {
	use core::sync::atomic::{AtomicU64, Ordering};

	static STATE: AtomicU64 = AtomicU64::new(0);

	pub fn fill(buf: &mut [u8]) {
		let mut s = STATE.load(Ordering::Relaxed) ^ super::tsc::now() ^ 0x9E37_79B9_7F4A_7C15;
		for chunk in buf.chunks_mut(8) {
			s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
			let mut z = s;
			z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
			z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
			z ^= z >> 31;
			let bytes = z.to_le_bytes();
			chunk.copy_from_slice(&bytes[..chunk.len()]);
		}
		STATE.store(s, Ordering::Relaxed);
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

	// Dispatch an SVC from EL0 against the saved trap frame (x8 = syscall number,
	// x0..x3 = arguments, the result is written back into the x0 slot). Routes to
	// the portable kernel syscall table. Returns `true` for SYS_USER_EXIT (the
	// caller then unwinds back to the kernel thread that entered EL0), `false` to
	// `eret` back to the user program with the result in x0.
	pub unsafe fn dispatch(frame: *mut u64) -> bool {
		let num = unsafe { *frame.add(8) }; // x8
		if num == abi::SYS_USER_EXIT {
			return true;
		}
		let (a0, a1, a2, a3) = unsafe { (*frame.add(0), *frame.add(1), *frame.add(2), *frame.add(3)) };
		super::percpu::set_from_user(true);
		let result = crate::syscall::syscall_dispatch(num, a0, a1, a2, a3);
		super::percpu::set_from_user(false);
		unsafe { *frame.add(0) = result };
		false
	}
}

// ---------------------------------------------------------------- usermode
pub mod usermode {
	pub const FAULT_PROBE_ADDR: u64 = 0x0dea_d000;

	unsafe extern "C" {
		fn aarch64_enter_el0(entry: u64, user_sp: u64, arg: u64, spsr: u64, resume_slot: *mut u64);
		fn aarch64_exit_el0(resume_sp: u64) -> !;
	}

	// Drop to EL0 at `entry` with SP_EL0 = `user_stack` and x0 = `arg`. SPSR selects
	// EL0t with interrupts enabled (0x0) so the user thread is preemptible; the call
	// "returns" here when the user program makes SYS_USER_EXIT. The resume state is
	// parked in the calling thread's syscall_rsp slot, so concurrent user threads do
	// not clobber one another.
	pub unsafe fn enter(entry: u64, user_stack: u64, arg: u64) {
		let slot = match crate::sched::current_thread() {
			Some(thread) => thread.syscall_rsp_addr(),
			None => return,
		};
		unsafe { aarch64_enter_el0(entry, user_stack, arg, 0x0, slot) }
		if let Some(thread) = crate::sched::current_thread() {
			thread.set_syscall_rsp(0);
		}
	}

	// Unwind from an EL0 syscall back to the kernel that called `enter`, using the
	// current thread's parked resume pointer.
	pub fn exit_to_kernel() -> ! {
		let resume = crate::sched::current_thread().map_or(0, |thread| thread.syscall_rsp_load());
		unsafe { aarch64_exit_el0(resume) }
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
pub mod pci;
