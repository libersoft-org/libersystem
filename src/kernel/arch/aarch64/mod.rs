// aarch64 (ARM64) architecture backend.
//
// STATUS: BOOTS, and runs the whole test suite. The header here said `STUB` and "a boot on this
// arch is not possible until then" while this target was passing 226 of 226 tests, which is worth
// recording rather than quietly deleting: the comment could stay wrong for so long because what it
// describes is REAL. The ARMv8-A mechanics are all implemented in the submodules below - VMSAv8
// page tables in `paging`, the VBAR_EL1 vector table in `exceptions`, the GIC and the generic timer
// in `gic`/`apic`/`interrupts`, PSCI SMP wake in `psci`/`smp`, the SVC syscall path in `syscall`,
// TPIDR_EL1 per-CPU in `percpu`, the PL011 UART in `serial`, and device-tree parsing in `dtb`.
//
// What IS still `todo!()` is the portable init contract listed in `arch/mod.rs`: `init`,
// `init_interrupts`, `init_syscalls`, `init_tsc`, `init_bsp_percpu` and `init_ap`, plus the shims
// the x86 bring-up reaches them through. Seventeen stubs, and none of them is on a path this target
// takes. aarch64 boots from firmware straight into `boot::aarch64_main`, which is the EL1 entry and
// drives the whole bring-up itself - console, memory, paging, per-CPU, GIC, timer, SMP, scheduler,
// then the userspace boot chain - so it never enters the bootloader-handoff `main::kmain` that
// calls those hooks. They exist so the shared crate root type-checks for `aarch64-unknown-none`.
//
// The consequence, which `arch/mod.rs` states once for both device-tree targets: the HAL contract
// is satisfied for everything after boot and bypassed for boot itself.

mod boot;
mod dtb;
mod exceptions;
mod gic;
pub mod psci;
pub mod serial;
pub mod usercopy;
mod virtio_blk;

// halt the kernel forever (wait-for-event)
pub fn halt_loop() -> ! {
	loop {
		unsafe {
			core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
		}
	}
}

pub fn init_interrupts() {
	todo!("aarch64: GIC + generic timer")
}

pub fn init_syscalls() {
	todo!("aarch64: SVC vector wiring")
}

pub fn init_tsc() {
	todo!("aarch64: generic-timer frequency")
}

pub fn init_bsp_percpu(_mpidr: u32) {
	todo!("aarch64: TPIDR_EL1 for the boot core")
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

// The QEMU fw-cfg MMIO base the device tree named, recorded during boot so the profile can be
// read before there is any memory to allocate. Zero when the tree had no fw-cfg node.
static FWCFG_BASE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub(crate) fn set_fwcfg_base(base: u64) {
	FWCFG_BASE.store(base, core::sync::atomic::Ordering::Relaxed);
}

// Name the boot profile the host selected, or `None` for an ordinary boot. The host names it
// over fw-cfg, so selecting one changes no byte the guest is built from - the same kernel,
// loader and system image boot with or without it. That is what lets a scenario runner drive a
// cold boot of this target: the profile is what makes DeviceManager start a control agent.
pub fn boot_profile() -> Option<&'static str> {
	let mut name = [0u8; 32];
	let base = FWCFG_BASE.load(core::sync::atomic::Ordering::Relaxed);
	let len = crate::arch::common::fwcfg::read_file(base, b"opt/org.libersystem/profile", &mut name, super::paging::phys_to_virt)?;
	match &name[..len] {
		b"development" => Some("development"),
		_ => None,
	}
}

// Write the CPU's model name into `out`, returning the byte count. aarch64 exposes
// no brand string; decode MIDR_EL1's implementer + part number to a name (a small
// table of the parts we run on), falling back to the raw ids. Feeds `lscpu`.
pub fn cpu_brand(out: &mut [u8]) -> usize {
	let midr: u64;
	unsafe {
		core::arch::asm!("mrs {}, midr_el1", out(reg) midr, options(nomem, nostack, preserves_flags));
	}
	let implementer: u64 = (midr >> 24) & 0xff;
	let part: u64 = (midr >> 4) & 0xfff;
	let name: &str = match (implementer, part) {
		(0x41, 0xd08) => "ARM Cortex-A72",
		(0x41, 0xd0c) => "ARM Neoverse-N1",
		(0x41, 0xd40) => "ARM Neoverse-V1",
		(0x41, _) => "ARM",
		(0x51, _) => "Qualcomm",
		(0x61, _) => "Apple",
		_ => "aarch64",
	};
	let b: &[u8] = name.as_bytes();
	let n: usize = b.len().min(out.len());
	out[..n].copy_from_slice(&b[..n]);
	n
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

// install the CPU exception vectors and enable memory-protection features
pub fn init() {
	todo!("aarch64: VBAR_EL1 + MMU protection bits")
}

pub fn init_ap(_cpu_id: usize, _mpidr: u32) {
	todo!("aarch64: secondary-core bring-up")
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
	pub fn send_wake_ipi(dest: u32) {
		// Bounce a halted core out of WFI so its idle loop re-checks its run queue: send
		// it SGI 0 (the wake IPI). The delivery is the whole message; gic::handle_irq EOIs
		// it and the core's idle loop picks up the enqueued work.
		super::gic::send_sgi(dest, 0);
	}
	pub fn send_init(_dest: u32) {
		todo!("aarch64 PSCI wake")
	}
	pub fn send_startup(_dest: u32, _vector: u8) {
		todo!("aarch64 PSCI wake")
	}
	// See the x86_64 note: a test build adds a harness-controlled skew so a deadline is reachable.
	pub fn ticks() -> u64 {
		let base = super::gic::ticks();
		#[cfg(test)]
		{
			base + crate::tests::clock_skew()
		}
		#[cfg(not(test))]
		{
			base
		}
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
		crate::arch::common::time::cycles_to_ns(cycles, hz())
	}
}

// ------------------------------------------------------------------ ioapic
pub mod ioapic {
	pub fn route(_gsi: u32, _vector: u8, _dest: u32) {
		todo!("aarch64 GIC routing")
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

	// No hardware source on this port yet.
	//
	// aarch64 has one in the architecture - FEAT_RNG's RNDR register - and nothing here detects or uses it, so
	// every draw comes from the formula below. That is why `SYS_RANDOM_GET` refuses on this
	// architecture rather than answering: the alternative is a syscall named for a key handing out
	// numbers derived from the boot clock, on every machine, always. The boot log says so out loud.
	pub fn secure_available() -> bool {
		false
	}

	pub fn secure(_buf: &mut [u8]) -> bool {
		false
	}

	// Deterministic, seeded from the clock. Distinguishable, never secret.
	pub fn insecure(buf: &mut [u8]) {
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
// (aarch64 wakes secondaries via PSCI CPU_ON, not a real-mode trampoline; these
// keep the portable names so smp.rs links until the real wake path replaces them.)
pub mod apboot {
	// No 32-bit CR3 load on this port, so no root is out of reach; the portable name exists
	// because the SMP path asks before it installs anything (KERN-ARCH-010).
	pub fn cr3_is_reachable(_root: u64) -> bool {
		true
	}
	#[must_use]
	pub unsafe fn install(_dst: *mut u8, _ttbr: u64, _entry: u64) -> bool {
		todo!("aarch64 PSCI wake")
	}
	pub unsafe fn set_stack(_dst: *mut u8, _stack_top: u64) {
		todo!("aarch64 PSCI wake")
	}
}

// ----------------------------------------------------------------- syscall
pub mod syscall {
	#[cfg(test)]
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
			// THE STATUS IS LATCHED HERE, because this is where the syscall ENDS.
			//
			// `SYS_USER_EXIT` does not go through `syscall_dispatch`: its portable arm never
			// returns - it unwinds to the kernel thread that entered EL0 - and this trap path has
			// to do that unwinding itself, from its caller, with the trap frame still on the stack.
			// So the shortcut is right. What it lost is the one thing that arm does BEFORE
			// unwinding: latching the status the program is reporting.
			//
			// The effect was that NO program on this port ever recorded an exit status. A waiter
			// could see that a process had finished and never whether it succeeded, and
			// ProcessService - which keeps an entry that is stopped with no status, because that is
			// what a launch which has not started yet looks like - held every program it ever
			// launched for the life of the system. That is the observation P02M0088 carried as "a
			// ring-3 child is not woken on aarch64": the child woke, ran and exited exactly as it
			// should, and the bookkeeping never learned it had.
			if let Some(thread) = crate::sched::current_thread() {
				thread.process().set_exit_status(unsafe { *frame.add(0) });
			}
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
	#[cfg(test)]
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

	// `enter`, with canaries in the kernel's callee-saved FP registers. Returns a bitmask of the
	// ones the excursion did not give back (bit 0 = d8 .. bit 7 = d15), which is zero when the ABI
	// this port claims is actually held. Test-only: it exists to measure KERN-ARCH-002.
	#[cfg(test)]
	pub unsafe fn enter_measuring_fp(entry: u64, user_stack: u64, arg: u64) -> u64 {
		unsafe extern "C" {
			fn aarch64_el0_fp_probe(entry: u64, user_sp: u64, arg: u64, resume_slot: *mut u64) -> u64;
		}
		let slot = match crate::sched::current_thread() {
			Some(thread) => thread.syscall_rsp_addr(),
			None => return u64::MAX,
		};
		let mismatch = unsafe { aarch64_el0_fp_probe(entry, user_stack, arg, slot) };
		if let Some(thread) = crate::sched::current_thread() {
			thread.set_syscall_rsp(0);
		}
		mismatch
	}

	// Unwind from an EL0 syscall back to the kernel that called `enter`, using the
	// current thread's parked resume pointer.
	pub fn exit_to_kernel() -> ! {
		let resume = crate::sched::current_thread().map_or(0, |thread| thread.syscall_rsp_load());
		// ZERO IS NOT A STACK POINTER, and this handed it to one.
		//
		// `aarch64_exit_el0` opens with `mov sp, x0` and immediately `ldp x19, x20, [sp, #0]`, so a
		// `resume` of zero sets `SP` to zero and reads from address zero - a trap taken with an
		// unusable `SP`, which is the class of failure this milestone exists for and the one shape
		// of it that needs no corruption at all to reach. `map_or(0, ..)` produces it twice over: no
		// current thread, and a thread whose `syscall_rsp` is still the zero it was built with
		// because it never entered EL0.
		//
		// Halting here says which of those happened, at the instruction that caused it, instead of
		// leaving the exception entry to discover it one fault later with nothing left to name.
		if resume == 0 {
			panic!("aarch64: exit_to_kernel with no parked EL0 resume stack ({}), so there is nothing to return to", if crate::sched::current_thread().is_some() { "the running thread never entered EL0" } else { "no thread is current on this core" });
		}
		// AND IT HAS TO BE ON THIS THREAD'S OWN STACK, which refusing zero does not establish.
		//
		// `mov sp, x0` here is one of only four instructions in this port that can put a value into
		// `SP` from a register, and it is the ONLY one that takes it from memory a thread owns. The
		// wedge this milestone is chasing ends with `SP` inside the kernel image, which cannot come
		// from arithmetic on a good stack pointer - so either it came through one of those four, or
		// the value was already wrong. Three of the four are now checked and silent; this closes the
		// fourth, and a silence here is as informative as a report.
		if let Some(thread) = crate::sched::current_thread() {
			let (base, len) = thread.kstack_region();
			if resume < base || resume > base + len as u64 {
				panic!("aarch64: exit_to_kernel would return onto {resume:#x}, which is not on this thread's kernel stack {base:#x}..={:#x} - the parked EL0 resume pointer was overwritten", base + len as u64);
			}
		}
		unsafe { aarch64_exit_el0(resume) }
	}

	// The embedded ring-3 probe programs the kernel test suite runs at EL0 (mirrors the
	// x86_64 usermode probes). Each returns its position-independent A64 instruction
	// bytes, copied into a USER page before entering EL0.
	#[cfg(test)]
	pub fn program_bytes() -> &'static [u8] {
		as_bytes(&PROGRAM_BASIC)
	}
	#[cfg(test)]
	pub fn program_fault_bytes() -> &'static [u8] {
		as_bytes(&PROGRAM_FAULT)
	}
	#[cfg(test)]
	pub fn program_yield_bytes() -> &'static [u8] {
		as_bytes(&PROGRAM_YIELD)
	}
	#[cfg(test)]
	pub fn program_nx_bytes() -> &'static [u8] {
		as_bytes(&PROGRAM_NX)
	}
	#[cfg(test)]
	pub fn program_stack_probe_bytes() -> &'static [u8] {
		as_bytes(&PROGRAM_STACK_PROBE)
	}
	#[cfg(test)]
	pub fn program_spin_bytes() -> &'static [u8] {
		as_bytes(&PROGRAM_SPIN)
	}
	#[cfg(test)]
	pub fn program_register_scrub_bytes() -> &'static [u8] {
		as_bytes(&PROGRAM_REGISTER_SCRUB)
	}

	// Reinterpret a program's instruction words as the little-endian byte slice the
	// test harness copies into a USER page (aarch64 is little-endian, so the u32
	// words are already in instruction-fetch order).
	#[cfg(test)]
	fn as_bytes(words: &'static [u32]) -> &'static [u8] {
		unsafe { core::slice::from_raw_parts(words.as_ptr() as *const u8, core::mem::size_of_val(words)) }
	}

	// A64 instruction encoders (const, so the syscall numbers and immediates bake in
	// at compile time). Register 31 names SP in the load/store/add/sub base position
	// and XZR in the MOVZ/MOVK/ORR destination/source position. These build the tiny
	// position-independent ring-3 probe programs the kernel test suite runs at EL0;
	// they mirror the x86_64 usermode probe programs one to one.
	#[cfg(test)]
	const SVC0: u32 = 0xD400_0001; // svc #0
	#[cfg(test)]
	const fn movz(rd: u32, imm: u16, hw: u32) -> u32 {
		0xD280_0000 | (hw << 21) | ((imm as u32) << 5) | rd
	}
	#[cfg(test)]
	const fn movk(rd: u32, imm: u16, hw: u32) -> u32 {
		0xF280_0000 | (hw << 21) | ((imm as u32) << 5) | rd
	}
	#[cfg(test)]
	const fn mov_reg(rd: u32, rm: u32) -> u32 {
		0xAA00_03E0 | (rm << 16) | rd // orr rd, xzr, rm
	}
	#[cfg(test)]
	const fn mov_from_sp(rd: u32) -> u32 {
		0x9100_03E0 | rd // add rd, sp, #0
	}
	#[cfg(test)]
	const fn sub_imm(rd: u32, rn: u32, imm12: u32, shift12: u32) -> u32 {
		0xD100_0000 | (shift12 << 22) | (imm12 << 10) | (rn << 5) | rd
	}
	#[cfg(test)]
	const fn add_imm(rd: u32, rn: u32, imm12: u32) -> u32 {
		0x9100_0000 | (imm12 << 10) | (rn << 5) | rd
	}
	#[cfg(test)]
	const fn subs_imm(rd: u32, rn: u32, imm12: u32) -> u32 {
		0xF100_0000 | (imm12 << 10) | (rn << 5) | rd
	}
	#[cfg(test)]
	const fn str_off(rt: u32, rn: u32, byte_off: u32) -> u32 {
		0xF900_0000 | ((byte_off / 8) << 10) | (rn << 5) | rt
	}
	#[cfg(test)]
	const fn ldr_off(rt: u32, rn: u32, byte_off: u32) -> u32 {
		0xF940_0000 | ((byte_off / 8) << 10) | (rn << 5) | rt
	}
	#[cfg(test)]
	const fn strh_off(rt: u32, rn: u32, byte_off: u32) -> u32 {
		0x7900_0000 | ((byte_off / 2) << 10) | (rn << 5) | rt
	}
	#[cfg(test)]
	const fn cbz(rt: u32, insns_back: u32) -> u32 {
		// Branch to a label `insns_back` instructions earlier (imm19 is a signed
		// instruction count).
		let imm19 = (0u32.wrapping_sub(insns_back)) & 0x7ffff;
		0xB400_0000 | (imm19 << 5) | rt
	}
	#[cfg(test)]
	const fn b_ne(insns_back: u32) -> u32 {
		let imm19 = (0u32.wrapping_sub(insns_back)) & 0x7ffff;
		0x5400_0000 | (imm19 << 5) | 1 // cond = NE
	}
	#[cfg(test)]
	const fn br(rn: u32) -> u32 {
		0xD61F_0000 | (rn << 5)
	}
	// orr rd, rn, rm - the general form `mov_reg` is the xzr special case of.
	#[cfg(test)]
	const fn orr_reg(rd: u32, rn: u32, rm: u32) -> u32 {
		0xAA00_0000 | (rm << 16) | (rn << 5) | rd
	}
	// fmov xd, dn / fmov dd, xn - the low 64 bits of a SIMD register, moved either way.
	#[cfg(test)]
	const fn fmov_x_from_d(rd: u32, vn: u32) -> u32 {
		0x9E66_0000 | (vn << 5) | rd
	}
	#[cfg(test)]
	const fn fmov_d_from_x(vd: u32, rn: u32) -> u32 {
		0x9E67_0000 | (rn << 5) | vd
	}
	// umov xd, vn.d[1] - the HIGH 64 bits, which `fmov` cannot reach.
	#[cfg(test)]
	const fn umov_x_from_high(rd: u32, vn: u32) -> u32 {
		0x4E18_3C00 | (vn << 5) | rd
	}
	#[cfg(test)]
	const B_SELF: u32 = 0x1400_0000; // b . (guard against running off the end)

	#[cfg(test)]
	use crate::syscall::{SYS_CHANNEL_SEND, SYS_DEBUG_WRITE, SYS_USER_EXIT, SYS_YIELD};

	// Basic ring-3 probe: SYS_CHANNEL_SEND(x0 = handle, "OK", 2, 0), SYS_DEBUG_WRITE('U'),
	// SYS_USER_EXIT. x0 arrives as the bootstrap Channel handle.
	#[cfg(test)]
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
	#[cfg(test)]
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
	#[cfg(test)]
	static PROGRAM_FAULT: [u32; 4] = [
		movz(0, (FAULT_PROBE_ADDR & 0xffff) as u16, 0),         // x0 low  = 0xd000
		movk(0, ((FAULT_PROBE_ADDR >> 16) & 0xffff) as u16, 1), // x0 high = 0x0dea
		str_off(0, 0, 0),                                       // [x0] = x0 -> fault
		B_SELF,
	];

	// No-execute probe: jump into the writable, no-execute stack page. The instruction
	// fetch there aborts (W^X) before a byte executes.
	#[cfg(test)]
	static PROGRAM_NX: [u32; 3] = [
		sub_imm(0, 31, 64, 0), // x0 = sp - 64 (inside the stack page)
		br(0),                 // fetch from a NO_EXECUTE page -> instruction abort
		B_SELF,
	];

	// Stack-growth probe: x0 = page count. Store one qword per page walking DOWN from
	// the entry stack pointer, then exit cleanly (or fault at the Domain's stack floor).
	#[cfg(test)]
	static PROGRAM_STACK_PROBE: [u32; 7] = [
		mov_from_sp(1),      // x1 = sp
		sub_imm(1, 1, 1, 1), // x1 -= 4096 (imm 1, shift 12)
		str_off(1, 1, 0),    // [x1] = x1 (touch the page)
		subs_imm(0, 0, 1),   // x0 -= 1, set flags
		b_ne(3),             // loop back 3 insns while x0 != 0
		movz(8, SYS_USER_EXIT as u16, 0),
		SVC0,
	];

	// Register-scrub probe: x0 = a shared data page. Folds every user-visible register except x0
	// itself - x1..x30 and both halves of v0..v31 - into x1, stores it at [x0], then writes
	// garbage into d8..d15 and exits. The kernel reads that word and requires zero.
	//
	// x0 is excluded because it is the argument this program was given, and SP because it is the
	// stack it was given; both are values the kernel means to hand over. Everything else is a
	// register whose contents ring 3 has no business seeing (KERN-ARCH-002).
	#[cfg(test)]
	const fn register_scrub_program() -> [u32; 170] {
		let mut out = [B_SELF; 170];
		let mut at = 0usize;
		// x1 is the accumulator AND one of the registers under test: it starts as whatever it
		// arrived with, and every other one is folded into it.
		let mut n = 2u32;
		while n <= 30 {
			out[at] = orr_reg(1, 1, n);
			at += 1;
			n += 1;
		}
		// x2 has been folded in by now, so it is free to use as the scratch the SIMD reads need.
		let mut v = 0u32;
		while v < 32 {
			out[at] = fmov_x_from_d(2, v);
			at += 1;
			out[at] = orr_reg(1, 1, 2);
			at += 1;
			out[at] = umov_x_from_high(2, v);
			at += 1;
			out[at] = orr_reg(1, 1, 2);
			at += 1;
			v += 1;
		}
		out[at] = str_off(1, 0, 0); // [x0] = everything this program could see
		at += 1;
		// And now clobber the kernel's callee-saved FP halves, which an EL0 program is entitled to
		// do. What comes back on the kernel side is the other half of this measurement.
		out[at] = movz(9, 0x0bad, 0);
		at += 1;
		let mut d = 8u32;
		while d <= 15 {
			out[at] = fmov_d_from_x(d, 9);
			at += 1;
			d += 1;
		}
		out[at] = movz(8, SYS_USER_EXIT as u16, 0);
		at += 1;
		out[at] = SVC0;
		out
	}
	#[cfg(test)]
	static PROGRAM_REGISTER_SCRUB: [u32; 170] = register_scrub_program();

	// CPU-bound spinner: x0 = shared data page. [x0] is a stop flag another thread
	// raises through the frame's kernel mapping, [x0 + 8] a counter this loop bumps so
	// an observer sees it running. It makes no syscall until the flag is set.
	#[cfg(test)]
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

#[cfg(test)]
mod tests;
