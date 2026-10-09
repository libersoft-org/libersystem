// THE x86_64 HALF OF THE SLEEP ENTRY: the wake a suspend would miss, the device lines a suspend to idle masks, and
// SUSPEND TO RAM - ACPI S3.
//
// S3 IS ENTERED FROM THE BOOT CORE'S IDLE CONTEXT, never from the thread that asked. The firmware resumes the boot
// processor alone, at the FACS waking vector, and every other core comes back cold - so the one context worth saving
// is the boot core's own idle loop, which no thread owns. The asking thread blocks; the boot core, once idle, finds
// the request (`run_pending`), holds every other core in the idle loop's park (`idle::begin_hold`), and then:
//
//   saves what the kernel set on a device and no driver owns - the I/O APIC's redirection entries, every function's
//     configuration (BARs, bridge windows, command, MSI and MSI-X controls, PCI Express's device, link, slot and
//     root controls), the MSI-X entries it programmed (`interrupts::restore_msix_entries`), the configuration a
//     claim had it write (`declared`), the IOMMU (`iommu::resume_after_reset`), PM1's enables and the GPE masks;
//   arms the CMOS alarm with RTC_EN for a timed wake;
//   points the real-mode trampoline boot used for the other cores at `s3_resume_entry` on a stack of its own, on
//     the kernel's page tables with the loader's identity map reinstated for the trampoline's first instruction
//     after paging, and writes that page into the FACS waking vector;
//   saves its callee-saved registers and stack pointer, flushes the caches, and writes SLP_TYPa and SLP_TYPb, then
//     SLP_EN, into PM1a and PM1b control.
//
// ON THE WAKE the firmware runs the trampoline, which calls `s3_resume_entry`: the core's per-CPU block, descriptor
// tables, control registers, syscall entry and LAPIC are established again (`arch::resume_boot_core`), and the saved
// stack is returned into as if the call that slept had returned. Then - with the console UART's settings marked lost
// so its next line re-initialises it first - every saved setting is written back, the clock is rebased on a counter
// that restarted, the sleep's length is taken from the RTC, what woke the machine is read from PM1 status and NOT
// delivered as a press, and every other core is restarted through the trampoline.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU16, Ordering};

use abi::{ERR_INVALID, ERR_TIMED_OUT, ERR_UNSUPPORTED, SleepReport};

use super::port::{inw, outw};
use crate::arch::common::time::CLOCK;
use crate::sleep::boot_core::{self, Kind};

// ------------------------------------------------------------------ the portable hooks

// A FIXED BUTTON ALREADY PENDING: a wake the entry would miss, which refuses it.
pub fn wake_pending() -> bool {
	#[cfg(not(test))]
	{
		super::sci::button_pending()
	}
	#[cfg(test)]
	{
		false
	}
}

// A SLEEP'S DEVICE LINES: every claimed wired line masked - its driver has quiesced its device - and the general-
// purpose events down to the wake mask alone; and back after.
pub fn mask_device_lines() {
	super::interrupts::mask_claimed_lines();
	super::interrupts::mask_msix_entries();
	#[cfg(not(test))]
	super::sci::gpes_enter_sleep();
}

// WHAT THIS PORT'S ENTRY TAKES: suspend to idle always; suspend to RAM where the firmware has a FACS to hold the waking
// vector (the `\_S3` registration is the portable half); the fixed buttons from the FADT's flags.
pub fn offers_idle() -> bool {
	true
}

pub fn offers_ram() -> bool {
	crate::sleep::sleep_type(3).is_some() && facs().is_some()
}

// The snapshot needs the trampoline page the resume path restarts the other cores through, and PM1 to read the wake.
pub fn offers_disk() -> bool {
	crate::boot_info().smp_trampoline != 0 && ports().is_some()
}

pub fn fixed_buttons() -> u64 {
	#[cfg(not(test))]
	{
		super::sci::fixed_buttons()
	}
	// The suite arms no SCI, so it names no fixed button.
	#[cfg(test)]
	{
		0
	}
}

pub fn unmask_device_lines() {
	#[cfg(not(test))]
	super::sci::gpes_leave_sleep();
	super::interrupts::unmask_claimed_lines();
	super::interrupts::unmask_msix_entries();
}

// ------------------------------------------------------------------ the request

// SUSPEND TO RAM, hibernation's snapshot and a restore's replacement, each asked of the boot core's idle context
// (`sleep::boot_core`) - where the trampoline the resume path restarts the other cores through exists at all.
pub fn suspend_to_ram(after: Option<u64>) -> Result<SleepReport, i64> {
	let Some(pair) = crate::sleep::sleep_type(3) else { return Err(ERR_UNSUPPORTED) };
	if crate::boot_info().smp_trampoline == 0 {
		return Err(ERR_UNSUPPORTED);
	}
	boot_core::ask(Kind::Ram, pair, after)
}

// HIBERNATION'S SNAPSHOT: answered twice - see `hibernate`.
pub fn snapshot() -> Result<SleepReport, i64> {
	if crate::boot_info().smp_trampoline == 0 {
		return Err(ERR_UNSUPPORTED);
	}
	boot_core::ask(Kind::Snapshot, (0, 0), None)
}

// A RESTORE'S REPLACEMENT: answered only when it cannot happen.
pub fn replace() -> i64 {
	if crate::boot_info().smp_trampoline == 0 {
		return ERR_UNSUPPORTED;
	}
	match boot_core::ask(Kind::Replace, (0, 0), None) {
		Ok(_) => ERR_UNSUPPORTED,
		Err(error) => error,
	}
}

pub use super::hibernate::{context_is_ours, development_variant, enter_disk, kernel_image, ram_banks, replace_memory};
// The resume context a snapshot carries, which the suite's restore cases hand the kernel as an image's.
#[cfg(test)]
pub use super::hibernate::context;

// Whether a request waits for the boot core's idle context.
pub fn s3_pending() -> bool {
	boot_core::pending()
}

pub fn run_pending() {
	boot_core::run_pending()
}

// THE BOOT CORE'S RUN OF A REQUEST.
pub fn run(kind: Kind, pair: (u8, u8), after: Option<u64>) -> Result<SleepReport, i64> {
	match kind {
		Kind::Ram => run_ram(pair, after),
		Kind::Snapshot => super::hibernate::run_snapshot(),
		Kind::Replace => Err(crate::sleep::disk::replace_now()),
	}
}

// ------------------------------------------------------------------ the entry and the resume, on the boot core

// PM1 control: SLP_TYP in bits 12:10, SLP_EN bit 13. PM1 status: WAK_STS bit 15, RTC_STS bit 10, PWRBTN_STS bit 8.
const SLP_TYP: u16 = 0x7 << 10;
const SLP_EN: u16 = 1 << 13;
pub(super) const WAK_STS: u16 = 1 << 15;
pub(super) const RTC_STS: u16 = 1 << 10;
pub(super) const PWRBTN_STS: u16 = 1 << 8;
pub(super) const SLPBTN_STS: u16 = 1 << 9;
// PM1 enable: RTC_EN bit 10.
const RTC_EN: u16 = 1 << 10;

// What `enter_now` writes, set by `run` before it saves the core.
static PM1A_CONTROL: AtomicU16 = AtomicU16::new(0);
static PM1B_CONTROL: AtomicU16 = AtomicU16::new(0);
static SLP_TYP_A: AtomicU16 = AtomicU16::new(0);
static SLP_TYP_B: AtomicU16 = AtomicU16::new(0);

// The boot core's stack pointer at the save, which the resume returns into.
#[unsafe(no_mangle)]
static mut S3_SAVED_RSP: u64 = 0;

// The stack the resume entry runs on until it returns into the saved one.
#[repr(C, align(16))]
struct ResumeStack([u8; 16 * 1024]);
static mut S3_RESUME_STACK: ResumeStack = ResumeStack([0; 16 * 1024]);

// The resume stack's top, which a hibernation image's context names too.
#[allow(non_snake_case)]
pub(super) fn SLEEP_RESUME_STACK_TOP() -> u64 {
	(&raw const S3_RESUME_STACK as u64) + core::mem::size_of::<ResumeStack>() as u64
}

global_asm!(
	r#"
.section .text.s3, "ax"
.globl s3_save_and_enter
// s3_save_and_enter(enter: extern "C" fn() -> u64) -> u64: the callee-saved registers and the flags pushed and the
// stack pointer kept; `enter` writes SLP_EN and returns only if the machine did not sleep - answered 0 - while the
// resume returns into this frame with 1.
s3_save_and_enter:
	push rbp
	push rbx
	push r12
	push r13
	push r14
	push r15
	pushfq
	mov [rip + {saved}], rsp
	call rdi
	popfq
	pop r15
	pop r14
	pop r13
	pop r12
	pop rbx
	pop rbp
	xor eax, eax
	ret

.globl s3_resume_entry
// FROM THE TRAMPOLINE, on the resume stack with interrupts off: the core established again, then the saved frame.
s3_resume_entry:
	call {restore}
	mov rsp, [rip + {saved}]
	popfq
	pop r15
	pop r14
	pop r13
	pop r12
	pop rbx
	pop rbp
	mov eax, 1
	ret
"#,
	saved = sym S3_SAVED_RSP,
	restore = sym restore_core,
);

unsafe extern "C" {
	pub(super) fn s3_save_and_enter(enter: extern "C" fn() -> u64) -> u64;
	pub(super) fn s3_resume_entry();
}

extern "C" fn restore_core() {
	super::resume_boot_core();
}

// THE WRITES THAT SLEEP: the caches flushed, SLP_TYP into both control registers, then SLP_EN into both. On a machine
// that sleeps this never returns; one that does not is given a moment and answered with a refusal.
extern "C" fn enter_now() -> u64 {
	let (a, b) = (PM1A_CONTROL.load(Ordering::Relaxed), PM1B_CONTROL.load(Ordering::Relaxed));
	let (typ_a, typ_b) = (SLP_TYP_A.load(Ordering::Relaxed), SLP_TYP_B.load(Ordering::Relaxed));
	unsafe {
		asm!("wbinvd", options(nostack, preserves_flags));
		let value_a = (inw(a) & !(SLP_TYP | SLP_EN)) | (typ_a << 10);
		outw(a, value_a);
		let value_b = if b != 0 {
			let value = (inw(b) & !(SLP_TYP | SLP_EN)) | (typ_b << 10);
			outw(b, value);
			value
		} else {
			0
		};
		outw(a, value_a | SLP_EN);
		if b != 0 {
			outw(b, value_b | SLP_EN);
		}
	}
	for _ in 0..50_000_000u64 {
		core::hint::spin_loop();
	}
	0
}

// The FADT's PM1 blocks this entry needs: control a (required) and b, and event a's and b's status and enable.
pub(super) struct Ports {
	pub(super) control_a: u16,
	pub(super) control_b: u16,
	pub(super) status: [Option<(u16, u16)>; 2],
}

pub(super) fn ports() -> Option<Ports> {
	let fadt = super::firmware::fadt()?;
	let control_a = fadt.pm1a_control()?.io_port()?;
	let control_b = fadt.pm1b_control().and_then(|block| block.io_port()).unwrap_or(0);
	let event = |block: Option<acpi::Block>| block.and_then(|block| Some((block.io_port()?, block.enable_port()?)));
	Some(Ports { control_a, control_b, status: [event(fadt.pm1a_event()), event(fadt.pm1b_event())] })
}

// The FACS, in the direct map, when the firmware has one long enough to carry the waking vectors.
fn facs() -> Option<u64> {
	let fadt = super::firmware::fadt()?;
	let phys = fadt.facs()?;
	if !crate::mem::within_direct_map(phys, 32) {
		return None;
	}
	Some(crate::mem::hhdm_offset() + phys)
}

fn run_ram(pair: (u8, u8), after: Option<u64>) -> Result<SleepReport, i64> {
	let Some(ports) = ports() else { return Err(ERR_UNSUPPORTED) };
	let Some(facs) = facs() else { return Err(ERR_UNSUPPORTED) };
	let tramp_phys = crate::boot_info().smp_trampoline;
	let tramp = (crate::mem::hhdm_offset() + tramp_phys) as *mut u8;
	let kernel_cr3 = crate::sched::kernel_cr3();
	if tramp_phys == 0 || tramp_phys >= 0x10_0000 || !super::apboot::cr3_is_reachable(kernel_cr3) {
		return Err(ERR_UNSUPPORTED);
	}
	// THE TIMED WAKE IS THE CMOS ALARM, whole seconds from now, within the day the alarm reaches - on a machine with a CMOS
	// clock. On one without, the Time and Alarm Device's driver armed its own timer in its step, and the kernel arms
	// nothing.
	let rtc = super::rtc_present();
	let alarm = match after {
		Some(ns) if rtc => {
			if !super::rtc::arm_alarm(ns.div_ceil(1_000_000_000)) {
				return Err(ERR_INVALID);
			}
			true
		}
		_ => false,
	};
	let suspended_at = match crate::sleep::prologue("suspend to RAM") {
		Ok(at) => at,
		Err(error) => {
			if alarm {
				super::rtc::disarm_alarm();
			}
			return Err(error);
		}
	};
	let wall_before = if rtc { super::rtc::read_unix() } else { 0 };
	mask_device_lines();
	// EVERY OTHER CORE HELD in the idle loop's park before anything is saved: a core still running would change what
	// is being saved, and the S3 takes its context anyway.
	crate::idle::begin_hold();
	let mut waited = 0u32;
	while !crate::idle::all_others_held() {
		if waited > 1_000_000 {
			crate::idle::end_hold();
			unmask_device_lines();
			return Err(abandon(suspended_at, alarm, "every other core was not parked within its bound", ERR_TIMED_OUT));
		}
		waited += 1;
		core::hint::spin_loop();
	}
	super::disable_interrupts();
	super::ioapic::save_all();
	if !super::pci::save_config_all() {
		super::enable_interrupts();
		crate::idle::end_hold();
		unmask_device_lines();
		return Err(abandon(suspended_at, alarm, "the functions' configuration could not be saved", ERR_UNSUPPORTED));
	}
	if !super::paging::reinstate_bootstrap_identity(kernel_cr3) {
		super::enable_interrupts();
		crate::idle::end_hold();
		unmask_device_lines();
		return Err(abandon(suspended_at, alarm, "the trampoline's identity map was not kept", ERR_UNSUPPORTED));
	}
	// THE KERNEL'S OWN PAGE TABLES from here: the idle context may be on a process's, which has no identity map.
	unsafe { asm!("mov cr3, {}", in(reg) kernel_cr3, options(nostack, preserves_flags)) };
	// SAFETY: the page the trampoline was installed into at boot; nothing else uses it while every other core is held.
	unsafe {
		super::apboot::set_root(tramp, kernel_cr3);
		super::apboot::set_entry(tramp, s3_resume_entry as *const () as u64);
		super::apboot::set_stack(tramp, SLEEP_RESUME_STACK_TOP());
		// THE WAKING VECTOR: the trampoline's real-mode page, and the 64-bit vector cleared so the firmware uses it.
		core::ptr::write_volatile((facs + 12) as *mut u32, tramp_phys as u32);
		let length = core::ptr::read_volatile((facs + 4) as *const u32);
		if length >= 32 {
			core::ptr::write_volatile((facs + 24) as *mut u64, 0);
		}
	}
	// PM1: stale wake status acknowledged, and the RTC's wake enabled for an alarm.
	for (status, enable) in ports.status.iter().flatten() {
		unsafe {
			outw(*status, WAK_STS | RTC_STS);
			let armed = inw(*enable);
			outw(*enable, if alarm { armed | RTC_EN } else { armed & !RTC_EN });
		}
	}
	PM1A_CONTROL.store(ports.control_a, Ordering::Relaxed);
	PM1B_CONTROL.store(ports.control_b, Ordering::Relaxed);
	SLP_TYP_A.store(u16::from(pair.0), Ordering::Relaxed);
	SLP_TYP_B.store(u16::from(pair.1), Ordering::Relaxed);
	// SAFETY: the saved frame is this function's own, on the boot core's idle stack, which nothing else runs on.
	let slept = unsafe { s3_save_and_enter(enter_now) } == 1;
	let resumed_at = super::tsc::now();
	if !slept {
		super::paging::remove_reinstated_identity(kernel_cr3);
		super::enable_interrupts();
		crate::idle::end_hold();
		unmask_device_lines();
		return Err(abandon(suspended_at, alarm, "the machine did not enter S3", ERR_UNSUPPORTED));
	}
	// ------------------------------------------------------------------ awake
	// THE CONSOLE UART FIRST OF ALL: its settings were lost, and the next line re-initialises it before its first byte.
	super::serial::sleep_wake(true);
	CLOCK.rebase(suspended_at, resumed_at);
	// WHAT WOKE IT, read before anything acknowledges it - and a button reported, never delivered as a press.
	let mut raised = 0u16;
	for (status, enable) in ports.status.iter().flatten() {
		unsafe {
			raised |= inw(*status);
			outw(*status, WAK_STS | RTC_STS | PWRBTN_STS | SLPBTN_STS);
			outw(*enable, inw(*enable) & !RTC_EN);
		}
	}
	let wake = if raised & RTC_STS != 0 && alarm {
		abi::WAKE_RTC
	} else if raised & PWRBTN_STS != 0 {
		abi::WAKE_POWER_BUTTON
	} else if raised & SLPBTN_STS != 0 {
		abi::WAKE_SLEEP_BUTTON
	} else {
		abi::WAKE_PLATFORM
	};
	if alarm {
		super::rtc::disarm_alarm();
	}
	// EVERY SETTING THE KERNEL MADE, BACK - the routing and the functions' configuration before any interrupt can be
	// delivered, the MSI-X entries once their functions decode again, the IOMMU before any driver runs.
	super::ioapic::restore_all();
	super::pci::restore_config_all();
	super::interrupts::restore_msix_entries();
	crate::declared::replay_claim_writes();
	#[cfg(not(test))]
	super::sci::rearm_after_reset();
	let translating = crate::iommu::resume_after_reset();
	// THE SLEEP'S LENGTH FROM THE RTC: the counter restarted with the machine. With no RTC of the kernel's own it is not
	// known yet - the clock's driver hands the base again in its resume step, and the boot-time clock takes it then.
	let slept_ns = if rtc {
		let wall_after = super::rtc::read_unix();
		wall_after.saturating_sub(wall_before).saturating_mul(1_000_000_000)
	} else {
		crate::sleep::slept_unknown();
		0
	};
	crate::sleep::add_slept(slept_ns);
	// EVERY OTHER CORE BACK, through the trampoline, on the identity map that is still reinstated - then removed.
	crate::idle::end_hold();
	// A CORE THAT DID NOT COME BACK is said by the restart; the report lists no cores, since no core parked - the
	// per-core record is suspend to idle's.
	crate::smp::restart_after_resume(tramp, (tramp_phys >> 12) as u8);
	super::paging::remove_reinstated_identity(kernel_cr3);
	super::apic::timer_periodic();
	super::enable_interrupts();
	unmask_device_lines();
	if !translating {
		crate::serial_println!("sleep: the IOMMU did not come back - the drivers that require it will be refused at their resume");
	}
	let report = SleepReport { wake, slept_ns, ..SleepReport::default() };
	crate::sleep::epilogue(&report, true);
	Ok(report)
}

// A SLEEP THAT DID NOT HAPPEN, unwound: the alarm disarmed, the clock rebased on the counter that ran on, the reason
// said, and the COM1 window closed.
pub(super) fn abandon(suspended_at: u64, alarm: bool, why: &str, error: i64) -> i64 {
	if alarm {
		super::rtc::disarm_alarm();
	}
	CLOCK.rebase(suspended_at, super::tsc::now());
	crate::serial_println!("sleep: not entered - {why}");
	super::serial::sleep_end();
	error
}
