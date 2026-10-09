// A RETURNING FIRMWARE SYSTEM SUSPEND. Unlike a hibernation replacement, this keeps the current
// kernel's memory and must restart every stopped core even when firmware refuses the transition.
// RISC-V currently supplies the retained RTC wake route and SBI SUSP entry; the saved contexts and
// machine settings are the same operations its hibernation path already uses.

use abi::{ERR_INTERRUPTED, ERR_TIMED_OUT, ERR_UNSUPPORTED, SleepReport};

use crate::arch;
use crate::arch::common::time::CLOCK;

const CORE_BOUND_NS: u64 = 10_000_000_000;

fn expired(started: u64) -> bool {
	tickclock::cycles_to_ns(arch::tsc::now().wrapping_sub(started), CLOCK.hz()) >= CORE_BOUND_NS
}

// A core that never finishes CPU_OFF, or cannot be restarted at its saved context, cannot safely
// be returned to the scheduler as an online core. Do not thaw applications onto that partial machine.
fn recovery_failed(why: &str, cpu: usize) -> ! {
	crate::serial_println!("sleep: system suspend cannot recover core {cpu} ({why}) - restarting the machine");
	arch::serial::flush_sync();
	arch::reset()
}

fn hold_others() -> Result<(), i64> {
	crate::idle::begin_hold();
	let started = arch::tsc::now();
	while !crate::idle::all_others_held() {
		if expired(started) {
			return Err(ERR_TIMED_OUT);
		}
		core::hint::spin_loop();
	}
	Ok(())
}

fn stop_others() -> Result<(), i64> {
	for cpu in 1..crate::smp::cpu_count() {
		crate::idle::request_system_stop(cpu);
	}
	let started = arch::tsc::now();
	for cpu in 1..crate::smp::cpu_count() {
		while !arch::resume::stopped(cpu) {
			let (state, error) = crate::idle::system_stop_state(cpu);
			if state == 3 {
				crate::serial_println!("sleep: core {cpu} refused its firmware stop ({error})");
				return Err(ERR_INTERRUPTED);
			}
			if expired(started) {
				return Err(ERR_TIMED_OUT);
			}
			core::hint::spin_loop();
		}
	}
	crate::serial_println!("sleep: system suspend - every other core is stopped");
	Ok(())
}

// A cancellation wins only before a core calls firmware. Settle every call already in flight
// before restoring or releasing anything; otherwise that core could turn off after the unwind.
fn settle_stops() {
	for cpu in 1..crate::smp::cpu_count() {
		crate::idle::cancel_system_stop(cpu);
	}
	let started = arch::tsc::now();
	for cpu in 1..crate::smp::cpu_count() {
		while crate::idle::system_stop_state(cpu).0 == 2 && !arch::resume::stopped(cpu) {
			if expired(started) {
				recovery_failed("the firmware stop did not settle", cpu);
			}
			core::hint::spin_loop();
		}
	}
}

// Global state and clocks have already been restored. Clear the returning stop requests before
// any saved context runs again, then release the hold and restart only cores that actually stopped.
fn release_others() {
	for cpu in 1..crate::smp::cpu_count() {
		crate::idle::clear_system_stop(cpu);
	}
	crate::idle::end_hold();
	for cpu in 1..crate::smp::cpu_count() {
		if arch::resume::stopped(cpu) {
			let error = arch::resume::start_saved(cpu);
			if error != 0 {
				crate::serial_println!("sleep: core {cpu}'s restart was refused ({error})");
				recovery_failed("the saved core could not start", cpu);
			}
		}
		let started = arch::tsc::now();
		while crate::idle::held(cpu) {
			if expired(started) {
				recovery_failed("the saved core did not leave its hold", cpu);
			}
			core::hint::spin_loop();
		}
	}
	crate::serial_println!("sleep: system suspend - all {} cores are running again", crate::smp::cpu_count());
}

pub fn run(after: Option<u64>) -> Result<SleepReport, i64> {
	if !arch::sleep::offers_ram() {
		return Err(ERR_UNSUPPORTED);
	}
	// This port's supported system wake is its retained RTC alarm. An untimed request has no
	// supported external wake source and is refused rather than relying on an ordinary core timer.
	let Some(after) = after else {
		crate::serial_println!("sleep: system suspend refused - this platform requires a timed retained RTC wake");
		return Err(ERR_UNSUPPORTED);
	};
	arch::rtc::arm_alarm(after)?;
	let suspended_at = match super::prologue("firmware system suspend") {
		Ok(at) => at,
		Err(error) => {
			arch::rtc::disarm_alarm();
			return Err(error);
		}
	};
	let wall_before = arch::rtc::read_unix_ns();
	arch::sleep::mask_device_lines();
	let mut saved = false;
	let result = (|| {
		hold_others()?;
		arch::disable_interrupts();
		if !arch::sleep::save_machine() {
			return Err(ERR_UNSUPPORTED);
		}
		saved = true;
		stop_others()?;
		if crate::power::armed() || arch::rtc::alarm_pending() {
			return Err(ERR_INTERRUPTED);
		}
		// Neither periodic nor monotonic deadlines are a system-suspend wake source. The RTC's
		// interrupt stays enabled in the saved IMSIC file, while global interrupt delivery is masked.
		arch::apic::timer_at_counter(None);
		let answer = arch::resume::save_and_leave(arch::sleep::enter_ram, 0);
		if answer == arch::resume::RESUMED {
			Ok(())
		} else {
			crate::serial_println!("sleep: SBI system suspend refused ({answer})");
			Err(ERR_INTERRUPTED)
		}
	})();
	arch::disable_interrupts();
	arch::apic::timer_at_counter(None);
	settle_stops();
	// First restore the clock, before any core can run a deadline check. RTC time remains meaningful
	// even if the platform's counter restarted. A refused transition is not credited as time asleep.
	CLOCK.rebase(suspended_at, arch::tsc::now());
	let slept_ns = if result.is_ok() { arch::rtc::read_unix_ns().saturating_sub(wall_before) } else { 0 };
	if result.is_ok() {
		super::add_slept(slept_ns);
		arch::serial::sleep_wake(true);
	}
	if saved {
		arch::sleep::restore_machine();
	}
	if result.is_ok() {
		crate::declared::replay_claim_writes();
		if !crate::iommu::resume_after_reset() {
			// As after hibernation, translation stays refused and affected drivers cannot resume DMA.
			crate::serial_println!("sleep: the IOMMU did not come back - its drivers cannot resume DMA");
		}
	}
	// Restore the pre-alarm route/enables AFTER the saved controller state has been written back.
	let alarm = arch::rtc::disarm_alarm();
	release_others();
	arch::sleep::unmask_device_lines();
	arch::apic::timer_periodic();
	arch::enable_interrupts();
	match result {
		Ok(()) => {
			let report = SleepReport { wake: if alarm { abi::WAKE_RTC } else { abi::WAKE_PLATFORM }, slept_ns, ..SleepReport::default() };
			super::epilogue(&report, true);
			Ok(report)
		}
		Err(error) => {
			crate::serial_println!("sleep: system suspend refused - every stopped core and the wake route restored ({error})");
			arch::serial::sleep_end();
			Err(error)
		}
	}
}
