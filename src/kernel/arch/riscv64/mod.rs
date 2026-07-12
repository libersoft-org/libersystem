// riscv64 (RISC-V) architecture backend.
//
// STATUS: BOOTS. The RISC-V mechanics are implemented across the submodules below
// (Sv39 page tables via SATP in `paging`, the STVEC trap vector + FP save/restore in
// `traps`, the SBI timer + AIA IMSIC MSI controller in `apic`/`imsic`/`interrupts`,
// SBI HSM hart_start SMP wake in `smp`, the ECALL syscall path in `syscall`, the `tp`
// per-CPU register in `percpu`, the 16550 / SBI console in `serial`, and DTB parsing
// in `dtb`). riscv64 boots directly on OpenSBI with no bootloader hand-off, so it does
// not enter through the shared `main::kmain`; instead `boot::riscv64_main` is the S-mode
// entry and drives the whole bring-up itself (memory, paging, per-CPU, SMP, scheduler,
// then the userspace boot chain to the shell).
//
// Because of that self-driven entry, the portable `arch::*` init contract below
// (`init`, `init_interrupts`, `init_syscalls`, `init_tsc`, `init_bsp_percpu`,
// `init_ap`) - the hooks the bootloader-handoff `kmain` calls on x86_64 - is never
// reached on this arch. Those functions remain `todo!()` on purpose: they exist only so
// the shared crate root type-checks for `riscv64gc-unknown-none-elf`; the equivalent
// work happens inline in `boot::riscv64_main`.

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

// Idle the hart until an interrupt is pending, then take it. WFI is executed with
// SSTATUS.SIE = 0 so it WAKES on any enabled-and-pending interrupt (its wakeup depends
// on sie & sip, not the global enable) but does NOT trap - control falls through to the
// following `csrsi`, which re-enables interrupts so the pending handler runs. Clearing
// SIE across the WFI closes the lost-wakeup race: an IPI or timer that arrives just
// before the WFI (SSIP/STIP already set) makes WFI return immediately instead of
// consuming the interrupt first and then sleeping (which `csrsi; wfi` would do, since
// on riscv the enabled interrupt is taken between the two instructions - unlike x86's
// `sti; hlt`, where the pending interrupt is deferred until after the HLT).
pub fn idle_halt() {
	unsafe {
		core::arch::asm!("csrci sstatus, 2", "wfi", "csrsi sstatus, 2", options(nomem, nostack, preserves_flags));
	}
}

// reboot / power off via the SBI System Reset (SRST) extension (EID 0x53525354,
// FID 0 = sbi_system_reset(reset_type, reset_reason)): reset_type 0 = shutdown,
// 1 = cold reboot; reset_reason 0 = no reason. OpenSBI performs the platform action
// (on QEMU virt: cold reboot re-enters the firmware, shutdown exits QEMU).
pub fn reset() -> ! {
	sbi_system_reset(1, 0);
	halt_loop()
}

pub fn poweroff() -> ! {
	sbi_system_reset(0, 0);
	halt_loop()
}

fn sbi_system_reset(reset_type: u32, reset_reason: u32) {
	unsafe {
		core::arch::asm!(
			"ecall",
			in("a7") 0x5352_5354usize, // SRST extension id ("SRST")
			in("a6") 0usize,           // FID 0 = system_reset
			in("a0") reset_type as usize,
			in("a1") reset_reason as usize,
			lateout("a0") _,
			lateout("a1") _,
			options(nostack),
		);
	}
}

// Write the CPU's model name into `out`, returning the byte count. The mvendorid /
// marchid / mimpid identity registers are M-mode CSRs, unreadable from S-mode, so
// query them through the SBI Base extension (EID 0x10, FIDs 4/5/6). QEMU's generic
// rv64 reports all-zero ids, so a known vendor decodes to a name and the rest falls
// back to a plain "riscv64". Feeds `lscpu`.
pub fn cpu_brand(out: &mut [u8]) -> usize {
	let vendor: usize = sbi_base(4); // get_mvendorid
	let name: &str = match vendor {
		0x489 => "SiFive riscv64",
		0x5b7 => "T-Head riscv64",
		_ => "riscv64",
	};
	let b: &[u8] = name.as_bytes();
	let n: usize = b.len().min(out.len());
	out[..n].copy_from_slice(&b[..n]);
	n
}

// One SBI Base extension probe (EID 0x10): returns the value in a1 (a0 is the error
// code, 0 on the always-present Base extension), or 0 on any error.
fn sbi_base(fid: usize) -> usize {
	let error: isize;
	let value: usize;
	unsafe {
		core::arch::asm!(
			"ecall",
			in("a7") 0x10usize, // Base extension id
			in("a6") fid,
			lateout("a0") error,
			lateout("a1") value,
			options(nostack, nomem),
		);
	}
	if error == 0 { value } else { 0 }
}

#[cfg(test)]
pub fn exit_qemu(success: bool) -> ! {
	// Terminate QEMU (run with `-semihosting`) via the RISC-V semihosting
	// SYS_EXIT_EXTENDED call, passing a code the test runner maps to pass/fail:
	// 0 = success, 1 = failure. The parameter block is {reason, exit_code};
	// ADP_Stopped_ApplicationExit (0x20026) is the normal-exit reason. QEMU recognizes
	// the fixed three-instruction magic sequence (slli x0 / ebreak / srai x0) around the
	// `ebreak` as a semihosting trap and consumes it before any S-mode trap delivery;
	// `.option norvc` keeps the instructions uncompressed so the pattern matches exactly.
	let block: [u64; 2] = [0x20026, if success { 0 } else { 1 }];
	unsafe {
		core::arch::asm!(
			".option push",
			".option norvc",
			"slli x0, x0, 0x1f",
			"ebreak",
			"srai x0, x0, 0x7",
			".option pop",
			in("a0") 0x20usize, // SYS_EXIT_EXTENDED
			in("a1") block.as_ptr(),
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

// --------------------------------------------------------------------- smp
pub mod smp;

// -------------------------------------------------------------------- plic
pub mod imsic;

// -------------------------------------------------------------- interrupts
pub mod interrupts;

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
