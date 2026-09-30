// sleepctl - the sleep's operator command: suspend now, hibernate now, an idle sleep's inhibition, and the last sleep.
//
// THE ONE SHIPPING HOLDER OF `system-sleep`. ServiceManager serves it; the authority to put the machine to sleep is not
// the authority to stop it, which is `system-power`'s, and this holds only the first.
//
// A SUSPEND IS ANSWERED AT ACCEPTANCE, never at the resume: the transaction freezes the applications - this tool among
// them - before it sleeps, so what this prints first is whether ServiceManager took the request. It then asks for the
// last sleep's record until the transaction has ended, which it can only do once it has been thawed, and prints how
// that sleep ended: slept and woke, refused before anything was frozen, or unwound - and where.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use process_client::SleepClient;
use proto::system::{LaunchContext, SleepOutcome, SleepReason, SleepRecord, SleepState, SleepStep, WakeReason};
use rt::*;
use tools::{parse_u64, split_args};

// How often the record is asked for while a sleep's transaction runs.
const POLL_TICKS: u64 = TICKS_PER_SECOND / 10;

const USAGE: &[u8] = b"usage: sleepctl [last | suspend [idle|ram] [SECONDS] | hibernate | inhibit SECONDS REASON]\n";

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let chan = recv_tagged(bootstrap, &mut buf, b"SLEEP").unwrap_or(0);
	if chan == 0 {
		print(b"sleepctl: the system-sleep authority was not granted\n");
		exit();
	}
	let mut sleep = SleepClient::new(chan);
	let args: Vec<&[u8]> = split_args(context.arguments.as_bytes()).collect();
	match args.as_slice() {
		[] | [b"last"] => last(&mut sleep),
		[b"suspend"] => suspend(&mut sleep, SleepState::Idle, 0),
		[b"suspend", state] => suspend(&mut sleep, state_of(state), 0),
		[b"suspend", state, seconds] => suspend(&mut sleep, state_of(state), parse_u64(seconds).unwrap_or_else(|| usage()).saturating_mul(1000)),
		[b"hibernate"] => hibernate(&mut sleep),
		[b"inhibit", seconds, reason @ ..] if !reason.is_empty() => {
			let seconds = parse_u64(seconds).unwrap_or_else(|| usage());
			let reason: Vec<String> = reason.iter().map(|word| String::from_utf8_lossy(word).into_owned()).collect();
			inhibit(&mut sleep, seconds, &reason.join(" "));
		}
		_ => usage(),
	}
	exit();
}

fn usage() -> ! {
	print(USAGE);
	exit()
}

fn state_of(word: &[u8]) -> SleepState {
	match word {
		b"idle" => SleepState::Idle,
		b"ram" => SleepState::Ram,
		_ => usage(),
	}
}

fn say(text: &str) {
	print(text.as_bytes());
	print(b"\n");
}

// ASKED FOR, answered at acceptance, and then the record of how it ended.
fn suspend(sleep: &mut SleepClient, state: SleepState, timed_wake_ms: u64) {
	match sleep.suspend(&state, &timed_wake_ms, &SleepReason::Requested) {
		Some(Ok(())) => say(&format!("sleepctl: {} accepted{}", state_name(state), if timed_wake_ms != 0 { format!(", with a timed wake in {} s", timed_wake_ms / 1000) } else { String::new() })),
		Some(Err(error)) => {
			say(&format!("sleepctl: the sleep was refused - {error:?}"));
			return;
		}
		None => {
			say("sleepctl: ServiceManager did not answer");
			return;
		}
	}
	wait_for_the_end(sleep);
}

fn hibernate(sleep: &mut SleepClient) {
	match sleep.hibernate(&SleepReason::Requested) {
		Some(Ok(())) => {
			say("sleepctl: hibernation accepted");
			wait_for_the_end(sleep);
		}
		Some(Err(error)) => say(&format!("sleepctl: hibernation was refused - {error:?}")),
		None => say("sleepctl: ServiceManager did not answer"),
	}
}

// AN IDLE SLEEP DELAYED while this runs, at most for its bound: the inhibition is the connection's, so it ends when
// this tool does.
fn inhibit(sleep: &mut SleepClient, seconds: u64, reason: &str) {
	let ms = u32::try_from(seconds.saturating_mul(1000)).unwrap_or(u32::MAX);
	match sleep.inhibit(&ms, reason) {
		Some(Ok(())) => say(&format!("sleepctl: an idle sleep is inhibited for {seconds} s ({reason})")),
		Some(Err(error)) => {
			say(&format!("sleepctl: the inhibition was refused - {error:?}"));
			return;
		}
		None => {
			say("sleepctl: ServiceManager did not answer");
			return;
		}
	}
	sleep_until(clock().saturating_add(seconds.saturating_mul(TICKS_PER_SECOND)));
	let _ = sleep.release();
	say("sleepctl: the inhibition is released");
}

// THE RECORD ONCE THE TRANSACTION HAS ENDED - which this tool sees only after it has been thawed.
fn wait_for_the_end(sleep: &mut SleepClient) {
	loop {
		match sleep.last_sleep() {
			Some(Ok(record)) if record.outcome != SleepOutcome::Running => {
				print_record(&record);
				return;
			}
			Some(Ok(_)) => sleep_until(clock().saturating_add(POLL_TICKS)),
			_ => {
				say("sleepctl: the last sleep's record could not be read");
				return;
			}
		}
	}
}

fn last(sleep: &mut SleepClient) {
	match sleep.last_sleep() {
		Some(Ok(record)) => print_record(&record),
		_ => say("sleepctl: the last sleep's record could not be read"),
	}
}

fn state_name(state: SleepState) -> &'static str {
	match state {
		SleepState::Idle => "suspend to idle",
		SleepState::Ram => "suspend to RAM",
		SleepState::Disk => "hibernation",
	}
}

fn step_name(step: SleepStep) -> &'static str {
	match step {
		SleepStep::None => "no step",
		SleepStep::Announce => "the announcement",
		SleepStep::Freeze => "the freeze",
		SleepStep::Flush => "the flush",
		SleepStep::Drivers => "the drivers",
		SleepStep::Platform => "the platform",
		SleepStep::Enter => "the entry",
	}
}

fn wake_name(wake: WakeReason) -> &'static str {
	match wake {
		WakeReason::Unknown => "an unknown wake",
		WakeReason::Timer => "the timed wake",
		WakeReason::PowerButton => "the power button",
		WakeReason::SleepButton => "the sleep button",
		WakeReason::Device => "a device",
		WakeReason::Rtc => "the RTC alarm",
		WakeReason::Platform => "the platform",
	}
}

fn print_record(record: &SleepRecord) {
	if record.requested == 0 {
		say("last sleep: none has been asked for");
		return;
	}
	say(&format!("last sleep: {} ({:?})", state_name(record.state), record.reason));
	match record.outcome {
		SleepOutcome::Running => say("  running now"),
		SleepOutcome::Resumed => {
			let detail = if record.wake == WakeReason::Device { format!(" (interrupt {})", record.wake_detail) } else { String::new() };
			say(&format!("  slept {} ms and was woken by {}{detail}", record.slept / 1_000_000, wake_name(record.wake)));
			for core in &record.cores {
				say(&format!("  cpu{}: woke {} time(s) for the timer, {} for an IPI, {} for a device while parked", core.cpu, core.timer, core.ipi, core.device));
			}
		}
		SleepOutcome::Refused => say(&format!("  refused before anything was frozen: {} - {}", record.who, record.why)),
		SleepOutcome::Unwound => say(&format!("  unwound at {}: {} - {}", step_name(record.step), record.who, record.why)),
	}
}
