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
pub fn exit_qemu(success: bool) -> ! {
	// Terminate QEMU (run with `-semihosting`) via the Angel SYS_EXIT_EXTENDED call,
	// passing an exit code the test runner maps to pass/fail: 0 = success, 1 = failure.
	// The parameter block is {reason, exit_code}; ADP_Stopped_ApplicationExit (0x20026)
	// is the normal-exit reason. The `hlt #0xf000` is the A64 semihosting trap.
	let block: [u64; 2] = [0x20026, if success { 0 } else { 1 }];
	unsafe {
		core::arch::asm!(
			".inst 0xd45e0000", // hlt #0xf000 - the A64 semihosting trap
			in("x0") 0x20u64, // SYS_EXIT_EXTENDED
			in("x1") block.as_ptr(),
			options(nostack),
		);
	}
	halt_loop()
}

// ------------------------------------------------------------------ paging
pub mod paging;

// ----------------------------------------------------------------- context
pub mod context;

// ------------------------------------------------------------------ percpu
pub mod percpu;

// -------------------------------------------------------------- interrupts
pub mod interrupts;

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
	pub fn send_wake_ipi(dest: u32) {
		// Bounce a halted core out of WFI so its idle loop re-checks its run queue: send
		// it SGI 0 (the wake IPI). The delivery is the whole message; gic::handle_irq EOIs
		// it and the core's idle loop picks up the enqueued work.
		super::gic::send_sgi(dest, 0);
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
// the current time as seconds since the Unix epoch. Reached through the physical
// direct map (the kernel runs higher-half, so TTBR0 is the caller's user space).
pub mod rtc {
	pub fn read_unix() -> u64 {
		const PL031_DR: u64 = 0x0901_0000;
		let va = super::paging::phys_to_virt(PL031_DR);
		unsafe { core::ptr::read_volatile(va as *const u32) as u64 }
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
	pub unsafe fn invoke(num: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
		// A ring-0 (kernel-context) system call: route straight to the portable syscall
		// table, the way the in-kernel callers and the test harness use it. Mark this a
		// kernel caller (from_user = false) so buffer checks accept kernel-owned buffers -
		// EL0 calls arrive through the SVC trap and `dispatch`, which sets from_user itself.
		super::percpu::set_from_user(false);
		crate::syscall::syscall_dispatch(num, a0, a1, a2, a3)
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

	// The embedded ring-3 probe programs the kernel test suite runs at EL0 (mirrors the
	// x86_64 usermode probes). Each returns its position-independent A64 instruction
	// bytes, copied into a USER page before entering EL0.
	pub fn program_bytes() -> &'static [u8] {
		as_bytes(&PROGRAM_BASIC)
	}
	pub fn program_fault_bytes() -> &'static [u8] {
		as_bytes(&PROGRAM_FAULT)
	}
	pub fn program_yield_bytes() -> &'static [u8] {
		as_bytes(&PROGRAM_YIELD)
	}
	pub fn program_nx_bytes() -> &'static [u8] {
		as_bytes(&PROGRAM_NX)
	}
	pub fn program_stack_probe_bytes() -> &'static [u8] {
		as_bytes(&PROGRAM_STACK_PROBE)
	}
	pub fn program_spin_bytes() -> &'static [u8] {
		as_bytes(&PROGRAM_SPIN)
	}

	// Reinterpret a program's instruction words as the little-endian byte slice the
	// test harness copies into a USER page (aarch64 is little-endian, so the u32
	// words are already in instruction-fetch order).
	fn as_bytes(words: &'static [u32]) -> &'static [u8] {
		unsafe { core::slice::from_raw_parts(words.as_ptr() as *const u8, core::mem::size_of_val(words)) }
	}

	// A64 instruction encoders (const, so the syscall numbers and immediates bake in
	// at compile time). Register 31 names SP in the load/store/add/sub base position
	// and XZR in the MOVZ/MOVK/ORR destination/source position. These build the tiny
	// position-independent ring-3 probe programs the kernel test suite runs at EL0;
	// they mirror the x86_64 usermode probe programs one to one.
	const SVC0: u32 = 0xD400_0001; // svc #0
	const fn movz(rd: u32, imm: u16, hw: u32) -> u32 {
		0xD280_0000 | (hw << 21) | ((imm as u32) << 5) | rd
	}
	const fn movk(rd: u32, imm: u16, hw: u32) -> u32 {
		0xF280_0000 | (hw << 21) | ((imm as u32) << 5) | rd
	}
	const fn mov_reg(rd: u32, rm: u32) -> u32 {
		0xAA00_03E0 | (rm << 16) | rd // orr rd, xzr, rm
	}
	const fn mov_from_sp(rd: u32) -> u32 {
		0x9100_03E0 | rd // add rd, sp, #0
	}
	const fn sub_imm(rd: u32, rn: u32, imm12: u32, shift12: u32) -> u32 {
		0xD100_0000 | (shift12 << 22) | (imm12 << 10) | (rn << 5) | rd
	}
	const fn add_imm(rd: u32, rn: u32, imm12: u32) -> u32 {
		0x9100_0000 | (imm12 << 10) | (rn << 5) | rd
	}
	const fn subs_imm(rd: u32, rn: u32, imm12: u32) -> u32 {
		0xF100_0000 | (imm12 << 10) | (rn << 5) | rd
	}
	const fn str_off(rt: u32, rn: u32, byte_off: u32) -> u32 {
		0xF900_0000 | ((byte_off / 8) << 10) | (rn << 5) | rt
	}
	const fn ldr_off(rt: u32, rn: u32, byte_off: u32) -> u32 {
		0xF940_0000 | ((byte_off / 8) << 10) | (rn << 5) | rt
	}
	const fn strh_off(rt: u32, rn: u32, byte_off: u32) -> u32 {
		0x7900_0000 | ((byte_off / 2) << 10) | (rn << 5) | rt
	}
	const fn cbz(rt: u32, insns_back: u32) -> u32 {
		// Branch to a label `insns_back` instructions earlier (imm19 is a signed
		// instruction count).
		let imm19 = (0u32.wrapping_sub(insns_back)) & 0x7ffff;
		0xB400_0000 | (imm19 << 5) | rt
	}
	const fn b_ne(insns_back: u32) -> u32 {
		let imm19 = (0u32.wrapping_sub(insns_back)) & 0x7ffff;
		0x5400_0000 | (imm19 << 5) | 1 // cond = NE
	}
	const fn br(rn: u32) -> u32 {
		0xD61F_0000 | (rn << 5)
	}
	const B_SELF: u32 = 0x1400_0000; // b . (guard against running off the end)

	use crate::syscall::{SYS_CHANNEL_SEND, SYS_DEBUG_WRITE, SYS_USER_EXIT, SYS_YIELD};

	// Basic ring-3 probe: SYS_CHANNEL_SEND(x0 = handle, "OK", 2, 0), SYS_DEBUG_WRITE('U'),
	// SYS_USER_EXIT. x0 arrives as the bootstrap Channel handle.
	static PROGRAM_BASIC: [u32; 17] = [
		mov_reg(19, 0),         // x19 = handle (svc preserves it via the trap frame)
		sub_imm(31, 31, 16, 0), // sp -= 16 (scratch for "OK")
		movz(1, 0x4b4f, 0),     // w1 = 'O','K'
		strh_off(1, 31, 0),     // [sp] = "OK"
		mov_reg(0, 19),         // x0 = handle
		mov_from_sp(1),         // x1 = sp (bytes ptr)
		movz(2, 2, 0),          // x2 = len 2
		movz(3, 0, 0),          // x3 = xfer 0
		movz(8, SYS_CHANNEL_SEND as u16, 0),
		SVC0,
		movz(0, 0x55, 0), // x0 = 'U'
		movz(1, 0, 0),    // x1 = len 0 (single-byte debug write)
		movz(8, SYS_DEBUG_WRITE as u16, 0),
		SVC0,
		movz(8, SYS_USER_EXIT as u16, 0),
		SVC0,
		B_SELF,
	];

	// Cooperative-yield probe: save the handle, SYS_YIELD x3 (so two instances on one
	// core interleave), then send "OK" and exit.
	static PROGRAM_YIELD: [u32; 20] = [
		mov_reg(19, 0), // x19 = handle
		movz(8, SYS_YIELD as u16, 0),
		SVC0,
		movz(8, SYS_YIELD as u16, 0),
		SVC0,
		movz(8, SYS_YIELD as u16, 0),
		SVC0,
		sub_imm(31, 31, 16, 0), // sp -= 16
		movz(1, 0x4b4f, 0),     // "OK"
		strh_off(1, 31, 0),
		mov_reg(0, 19), // x0 = handle
		mov_from_sp(1), // x1 = sp
		movz(2, 2, 0),  // len 2
		movz(3, 0, 0),  // xfer 0
		movz(8, SYS_CHANNEL_SEND as u16, 0),
		SVC0,
		movz(8, SYS_USER_EXIT as u16, 0),
		SVC0,
		B_SELF,
		B_SELF,
	];

	// Fault probe: write to FAULT_PROBE_ADDR (unmapped) to raise a page fault from EL0.
	static PROGRAM_FAULT: [u32; 4] = [
		movz(0, (FAULT_PROBE_ADDR & 0xffff) as u16, 0),         // x0 low  = 0xd000
		movk(0, ((FAULT_PROBE_ADDR >> 16) & 0xffff) as u16, 1), // x0 high = 0x0dea
		str_off(0, 0, 0),                                       // [x0] = x0 -> fault
		B_SELF,
	];

	// No-execute probe: jump into the writable, no-execute stack page. The instruction
	// fetch there aborts (W^X) before a byte executes.
	static PROGRAM_NX: [u32; 3] = [
		sub_imm(0, 31, 64, 0), // x0 = sp - 64 (inside the stack page)
		br(0),                 // fetch from a NO_EXECUTE page -> instruction abort
		B_SELF,
	];

	// Stack-growth probe: x0 = page count. Store one qword per page walking DOWN from
	// the entry stack pointer, then exit cleanly (or fault at the Domain's stack floor).
	static PROGRAM_STACK_PROBE: [u32; 7] = [
		mov_from_sp(1),      // x1 = sp
		sub_imm(1, 1, 1, 1), // x1 -= 4096 (imm 1, shift 12)
		str_off(1, 1, 0),    // [x1] = x1 (touch the page)
		subs_imm(0, 0, 1),   // x0 -= 1, set flags
		b_ne(3),             // loop back 3 insns while x0 != 0
		movz(8, SYS_USER_EXIT as u16, 0),
		SVC0,
	];

	// CPU-bound spinner: x0 = shared data page. [x0] is a stop flag another thread
	// raises through the frame's kernel mapping, [x0 + 8] a counter this loop bumps so
	// an observer sees it running. It makes no syscall until the flag is set.
	static PROGRAM_SPIN: [u32; 7] = [
		ldr_off(1, 0, 8), // x1 = [x0 + 8]
		add_imm(1, 1, 1), // x1 += 1
		str_off(1, 0, 8), // [x0 + 8] = x1
		ldr_off(2, 0, 0), // x2 = [x0] (stop flag)
		cbz(2, 4),        // loop back 4 insns while the flag is 0
		movz(8, SYS_USER_EXIT as u16, 0),
		SVC0,
	];
}

// --------------------------------------------------------------------- pci
pub mod pci;
