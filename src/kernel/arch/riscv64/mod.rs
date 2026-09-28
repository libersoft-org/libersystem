// riscv64 (RISC-V) architecture backend.
//
// STATUS: BOOTS. The RISC-V mechanics are implemented across the submodules below
// (Sv39 page tables via SATP in `paging`, the STVEC trap vector + FP save/restore in
// `traps`, the SBI timer + AIA IMSIC MSI controller in `apic`/`imsic`/`interrupts`,
// SBI HSM hart_start SMP wake in `smp`, the ECALL syscall path in `syscall`, the `tp`
// per-CPU register in `percpu`, the 16550 console in `serial`, and DTB parsing
// in `dtb`). riscv64 boots directly on OpenSBI with no bootloader hand-off, so it does
// not enter through the shared `main::kmain`; instead `boot::riscv64_main` is the S-mode
// entry and drives the whole bring-up itself (memory, paging, per-CPU, SMP, scheduler,
// then the userspace boot chain to the shell).
//
// Because of that self-driven entry, the x86 bootloader hand-off (`init`, `init_interrupts`,
// `init_syscalls`, `init_tsc`, `init_bsp_percpu`, `init_ap` and the shims around them) is not part
// of this backend at all: it compiles for x86_64 alone, and the equivalent work happens inline in
// `boot::riscv64_main`. Those symbols used to be defined here as `todo!()` bodies so the shared
// crate root would type-check - unreachable, and indistinguishable from an unfinished port to
// anything that reads source rather than call graphs.

pub mod boot;
pub mod dtb;

// THE DEVICE TREE'S ADDRESS, KEPT so the topology can be read after boot has finished with it.
//
// The tree is parsed once during bring-up and the result is consumed field by field; nothing keeps
// it. The topology reader needs `numa-node-id` and `/distance-map` at a LATER moment - after the direct-map
// bound exists and before the frame allocator is partitioned - and re-parsing from the pointer is
// cheaper and smaller than retaining a `BootInfo` that is mostly already spent.
static DEVICE_TREE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
// WHETHER THIS BOOT WENT THROUGH THE DEVICE-TREE PATH AT ALL, which is not the same question as
// whether the hint is non-zero: QEMU hands this port a boot argument of ZERO and the tree is found
// at the address `dtb::locate` falls back to. Gating on the address alone read that boot as having
// no tree, and the topology it does publish was never looked at.
static DEVICE_TREE_TAKEN: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn remember_device_tree(hint: u64) {
	DEVICE_TREE.store(hint, core::sync::atomic::Ordering::Release);
	DEVICE_TREE_TAKEN.store(true, core::sync::atomic::Ordering::Release);
}

// What the tree says, re-read. `None` where this boot was handed no tree at all - the UEFI/no-DT
// profiles, which make no topology claim and are named as such.
pub fn device_tree_boot_info() -> Option<fdt::BootInfo> {
	if !DEVICE_TREE_TAKEN.load(core::sync::atomic::Ordering::Acquire) {
		return None;
	}
	let hint = DEVICE_TREE.load(core::sync::atomic::Ordering::Acquire);
	// SAFETY: the same pointer this port's bring-up parsed, and `dtb::parse` validates the blob's
	// header before reading any of it.
	unsafe { dtb::parse(hint) }
}

// The tree itself, for a reader that wants a question answered rather than the bring-up summary.
//
// IN EVERY BUILD: the boot's arming pass and describe pass read it, and so does the suite's claimed-line
// test, which asks the board where the `edu` function's INTx pin goes exactly as a hot-plug port's is asked.
//
// THE SUMMARY IS NOT A SUBSTITUTE, and the hot-plug arming is what showed it. `BootInfo` is a fixed
// set of fields decided at boot; which controller input a given PCI function's INTx pin reaches is a
// LOOKUP over a table whose size the board chooses, asked once per port, after the bus scan. Adding
// it to the summary would mean carrying a board's whole routing table through bring-up on the
// chance somebody asks - so the pointer is re-read, exactly as the topology reader already does.
pub fn device_tree() -> Option<fdt::Fdt> {
	if !DEVICE_TREE_TAKEN.load(core::sync::atomic::Ordering::Acquire) {
		return None;
	}
	let hint = DEVICE_TREE.load(core::sync::atomic::Ordering::Acquire);
	// SAFETY: as above - the pointer this port's bring-up parsed, and `Fdt`'s own header checks
	// refuse anything that is not a tree before a token is read.
	unsafe { dtb::located(hint) }
}

pub mod platform;
pub mod serial;
pub mod traps;
pub mod usercopy;

// halt the kernel forever (wait-for-interrupt)
pub fn halt_loop() -> ! {
	loop {
		unsafe {
			core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
		}
	}
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
// Whether this boot is the named no-device-tree regression profile.
//
// THE SAME RULE aarch64 HAS, AND riscv64 HAD NONE. A boot with no tree fell through to the compiled
// `qemu-virt-aia` descriptor unconditionally - the hardcoded `0x2800_0000` base with `USABLE = true` -
// so a machine that published no tree was handed QEMU `virt`'s IMSIC addresses and started writing
// MSIs into them. That is a static descriptor selected by every absent or unparseable tree, where M4
// asks for one selected only by a NAMED profile.
//
// A boot with no tree has no way to name itself - the machine description IS the tree - so the
// authorisation is compiled in and the harness that boots the profile is what sets it. The default is
// NO, which turns a machine this port cannot discover into a named refusal rather than an address
// nobody claimed.
pub fn boot_profile_authorises_no_dt() -> bool {
	option_env!("LIBER_NO_DT_PROFILE").is_some_and(|value| value == "1")
}

pub fn boot_profile() -> Option<&'static str> {
	let mut name = [0u8; 32];
	let base = FWCFG_BASE.load(core::sync::atomic::Ordering::Relaxed);
	let len = crate::arch::common::fwcfg::read_file(base, b"opt/org.libersystem/profile", &mut name, super::paging::phys_to_virt)?;
	match &name[..len] {
		b"development" => Some("development"),
		// A SECOND PROFILE, SO "A HARNESS IS WATCHING" IS ITS OWN CONDITION.
		//
		// `boot_main` emitted the `\x1ePERF` anchor whenever a profile was named, and the profile a
		// PERSON boots interactively (`DEV_PROFILE=1`) is the same one - so a raw record-separator
		// line addressed to a program appeared on a human's console. It is a development boot in
		// every other respect, so everything keyed on `is_some()` still holds; what this adds is a
		// way for the one line addressed to a tool to know a tool is there.
		b"development-trace" => Some("development-trace"),
		_ => None,
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
// The wired interrupt controller, for the one thing an MSI cannot express: a line a function
// asserts. See `aplic.rs` - on this machine it delivers those lines as MSIs to the IMSIC below.
// In every build: a platform row's claimed line is armed through it, and the suite claims one.
pub mod aplic;
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

	// WHETHER EVERY HART HAS Sstc, read from the device tree at init: `stimecmp` is then this supervisor's own
	// compare register and a timer is one CSR write rather than a call into the firmware.
	static SSTC: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

	// The next S-mode timer interrupt at counter reading `when` - `u64::MAX` for none - which also clears the
	// pending timer bit.
	fn set_timer(when: u64) {
		if SSTC.load(Ordering::Relaxed) {
			// `stimecmp` is CSR 0x14D.
			unsafe { core::arch::asm!("csrw 0x14d, {}", in(reg) when, options(nostack, preserves_flags)) };
		} else {
			sbi_set_timer(when);
		}
	}

	// EVERY TIMER INTERRUPT THE MACHINE TAKES, on any hart. The clock is computed from the `time` CSR and
	// advances without one, so the prologue proves the interrupt path by counting these rather than ticks.
	static TIMER_INTERRUPTS: AtomicU64 = AtomicU64::new(0);

	pub fn timer_interrupts() -> u64 {
		TIMER_INTERRUPTS.load(Ordering::Relaxed)
	}

	// Which harts' timers are a one-shot for a halt, so their interrupt clears the timer instead of re-arming
	// the tick.
	static ONE_SHOT: [core::sync::atomic::AtomicBool; crate::smp::MAX_CPUS] = [const { core::sync::atomic::AtomicBool::new(false) }; crate::smp::MAX_CPUS];

	// Arm the next periodic tick: now + timebase / TICK_HZ.
	pub fn arm_timer() {
		let interval = super::tsc::hz() / TICK_HZ as u64;
		set_timer(super::tsc::now() + interval);
	}

	// THIS HART'S TIMER AS A ONE-SHOT for a halt, at tick `deadline` - never, for `None` or a tick too far
	// away to express. The compare value is absolute on the same `time` CSR the clock reads, converted
	// through the sleep offset. False when the clock cannot convert a tick yet, and the tick then stays.
	pub fn timer_one_shot(deadline: Option<u64>) -> bool {
		let clock = &crate::arch::common::time::CLOCK;
		if !clock.anchored() {
			return false;
		}
		ONE_SHOT[crate::sched::current_cpu_id()].store(true, Ordering::Relaxed);
		set_timer(deadline.and_then(|tick| clock.counter_at(tick)).unwrap_or(u64::MAX));
		true
	}

	// THIS HART'S PERIODIC TICK, back after a halt.
	pub fn timer_periodic() {
		ONE_SHOT[crate::sched::current_cpu_id()].store(false, Ordering::Relaxed);
		arm_timer();
	}

	pub fn send_wake_ipi(dest: u64) {
		// SBI IPI extension (EID 0x735049 "sPI", FID 0): raise a supervisor software
		// interrupt on the target hart so it leaves wfi and re-checks the run queue.
		unsafe {
			core::arch::asm!(
				"ecall",
				in("a7") 0x735049usize,
				in("a6") 0usize,
				in("a0") 1usize,          // hart_mask = 1 bit, based at `dest`
				in("a1") dest as usize,   // hart_mask_base (SBI carries a hart id as an unsigned long)
				lateout("a0") _,
				options(nostack),
			);
		}
	}

	// See the x86_64 note: a test build adds a harness-controlled skew so a deadline is reachable.
	pub fn ticks() -> u64 {
		let base = crate::arch::common::time::CLOCK.ticks(super::tsc::now());
		#[cfg(test)]
		{
			base + crate::tests::clock_skew()
		}
		#[cfg(not(test))]
		{
			base
		}
	}

	// Advance the tick counter and re-arm the timer. Called from the S-mode timer
	// interrupt (traps.rs).
	pub fn on_timer_tick() {
		TIMER_INTERRUPTS.fetch_add(1, Ordering::Relaxed);
		// A ONE-SHOT FOR A HALT FIRES ONCE: the timer is cleared and the halt restores the tick. Otherwise EVERY
		// hart re-arms - that is what drives preemption on it. The clock is not moved here: it is computed from
		// the `time` CSR when read (`arch::common::time::CLOCK`).
		if crate::idle::ready() && ONE_SHOT[crate::sched::current_cpu_id()].load(Ordering::Relaxed) {
			set_timer(u64::MAX);
		} else {
			arm_timer();
		}
	}

	// Enable the S-mode timer interrupt (SIE.STIE, bit 5), the software interrupt
	// (SIE.SSIE, bit 1, for cross-hart wake IPIs), and the external interrupt (SIE.SEIE,
	// bit 9, for PLIC-routed device interrupts), then arm the first tick.
	pub fn init() {
		unsafe {
			core::arch::asm!("csrs sie, {}", in(reg) (1u64 << 5) | (1u64 << 1) | (1u64 << 9), options(nostack, preserves_flags));
		}
		// THE CLOCK STARTS WITH THE FIRST HART'S TIMER, the timebase frequency being the device tree's and
		// known by now; anchoring is once, so every later hart's call changes nothing.
		if crate::arch::common::time::CLOCK.anchor(super::tsc::now(), super::tsc::hz()) {
			// And the boot hart reads, once, whether every hart has Sstc.
			SSTC.store(unsafe { super::dtb::has_isa_extension(super::DEVICE_TREE.load(Ordering::Acquire), b"sstc") }, Ordering::Relaxed);
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
	// Read `/cpus/timebase-frequency` from the device tree once, at clock init.
	//
	// It was the constant 10,000,000 with a comment naming QEMU's virt machine, and every timeout,
	// tick conversion, timer and deadline is scaled by it - so on hardware that ticks at another
	// rate all of them are wrong by that ratio, silently. The fallback stays, because a tree that
	// does not carry the property leaves nothing else to go on, and it says so on the way past.
	static HZ: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
	const QEMU_VIRT_HZ: u64 = 10_000_000;

	pub fn init_from_dtb(dtb: u64) {
		match unsafe { super::dtb::timebase_frequency(dtb) } {
			Some(hz) if hz > 0 => HZ.store(hz as u64, core::sync::atomic::Ordering::Release),
			_ => {
				crate::serial_println!("riscv64: no /cpus/timebase-frequency in the device tree; assuming {QEMU_VIRT_HZ} Hz (QEMU virt)");
				HZ.store(QEMU_VIRT_HZ, core::sync::atomic::Ordering::Release);
			}
		}
	}

	// Kept so the existing `init()` call site still compiles; the tree is read by
	// `init_from_dtb`, which boot calls with the pointer it already has.
	pub fn init() {}

	pub fn hz() -> u64 {
		match HZ.load(core::sync::atomic::Ordering::Acquire) {
			0 => QEMU_VIRT_HZ,
			hz => hz,
		}
	}
	pub fn cycles_to_ns(cycles: u64) -> u64 {
		tickclock::cycles_to_ns(cycles, hz())
	}
}

// ----------------------------------------------------------------------- ioports
// PORT I/O: THIS MACHINE HAS NO PORT SPACE - every device register is memory - so no row ever carries a
// port resource, nothing is ever granted, and there is no bitmap to load. The portable kernel calls these
// on every port; each answers the one thing that is true here.
pub mod ioports {
	use crate::object::process::Process;

	pub fn supported() -> bool {
		false
	}

	pub fn switch_in(_process: &Process) {}

	pub fn switch_to_idle() {}

	pub fn service() {}

	pub fn reload_if_running(_process: &Process) {}

	pub fn firmware_blocks(_out: &mut [(u16, u16); 16]) -> usize {
		0
	}

	pub fn function_ports(_bus: u8, _dev: u8, _func: u8, _vendor: u16, _product: u16, _out: &mut [abi::PortResource; abi::MAX_PORT_RESOURCES]) -> usize {
		0
	}

	pub fn set_io_decode(_bus: u8, _dev: u8, _func: u8, _on: bool) {}
}

// ------------------------------------------------------------------ firmware
// NO ACPI ON THIS PORT: the machine describes itself with a device tree, which the kernel reads itself. The firmware
// interpreter's calls answer that there is nothing to interpret.
pub mod firmware {
	pub fn available() -> bool {
		false
	}

	pub fn table(_signature: &[u8; 4], _instance: usize) -> Option<&'static [u8]> {
		None
	}

	pub fn smi_command(_value: u8) -> i64 {
		abi::ERR_UNSUPPORTED
	}

	pub fn pm_timer() -> Option<u32> {
		None
	}

	pub fn global_lock_release() -> i64 {
		abi::ERR_UNSUPPORTED
	}

	pub fn nvram_read(_index: u64) -> i64 {
		abi::ERR_UNSUPPORTED
	}

	pub fn nvram_write(_index: u64, _value: u64) -> i64 {
		abi::ERR_UNSUPPORTED
	}

	pub fn gpe_request(_operation: u64, _gpe: u64) -> i64 {
		abi::ERR_UNSUPPORTED
	}

	pub fn gpe_instance_ended() {}

	#[cfg(not(test))]
	pub fn take_events(_out: &mut dyn FnMut(u8, u16)) {}

	// No line of a device-tree machine is the firmware's: its interrupt controllers are kernel-held rows.
	pub fn kernel_lines() -> alloc::vec::Vec<u32> {
		alloc::vec::Vec::new()
	}

	pub fn chipset_registers(_vendor: u16, _device: u16) -> &'static [(u16, u16)] {
		&[]
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

	// No hardware source on this port yet.
	//
	// riscv64 has one in the architecture - the Zkr extension's seed CSR - and nothing here detects or uses it, so
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
// (riscv64 wakes secondary harts via the SBI HSM `hart_start` call, not a
// real-mode trampoline; these keep the portable names so smp.rs links until
// the real wake path replaces them.)
pub mod apboot {}

// ----------------------------------------------------------------- syscall
pub mod syscall {
	// STVEC is already installed (traps::init), so a U-mode ecall lands in
	// __trap_entry -> riscv64_trap -> dispatch. Nothing extra to program here.
	pub fn init() {}

	#[cfg(test)]
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
			// launched for the life of the system. That is the observation recorded as "a
			// ring-3 child is not woken on aarch64": the child woke, ran and exited exactly as it
			// should, and the bookkeeping never learned it had.
			if let Some(thread) = crate::sched::current_thread() {
				thread.process().set_exit_status(unsafe { *frame.add(10) });
			}
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
