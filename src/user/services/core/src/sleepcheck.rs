// sleepcheck - the sleep gate's probe. DEVELOPMENT-ONLY.
//
// It holds no authority: what it shows is what an APPLICATION sees across a sleep - the freeze and the two clocks - so
// it is launched as one and lands in the applications Domain the transaction freezes. Every line carries both clocks,
// and the host timestamps each line as it arrives, which is what the gate compares against the kernel's `sleep:
// entered` and `sleep: resumed` lines.
//
//   sleepcheck count SECONDS   a line every 100 ms, each on an absolute deadline of the monotonic clock: silent while
//                              frozen, and at its next value after the thaw rather than a burst catching up
//   sleepcheck timer MS        a Timer armed MS from now on the monotonic clock, its arming and its firing each said
//                              with both clocks - a sleep between the two is on the boot-time clock alone
//   sleepcheck clocks          both clocks and the kernel's RTC read, once

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use proto::system::LaunchContext;
use rt::*;

// The counter's period: a tenth of a second.
const PERIOD_TICKS: u64 = TICKS_PER_SECOND / 10;

fn say(text: &str) {
	print(b"sleepcheck: ");
	print(text.as_bytes());
	print(b"\n");
}

fn usage() -> ! {
	say("usage: sleepcheck count SECONDS | timer MS | clocks");
	exit()
}

// Both clocks, in milliseconds: the monotonic one leaves every sleep out, the boot-time one takes it in.
fn stamp() -> String {
	format!("mono-ms {} boot-ms {}", clock_ns() / 1_000_000, clock_boot_ns() / 1_000_000)
}

fn number(word: &str) -> u64 {
	word.parse::<u64>().unwrap_or_else(|_| usage())
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let words: Vec<&str> = context.arguments.split(' ').filter(|word| !word.is_empty()).collect();
	match words.as_slice() {
		["count", seconds] => count(number(seconds)),
		["timer", ms] => timer(number(ms)),
		["clocks"] => say(&format!("clocks {} rtc {}", stamp(), clock_rtc())),
		_ => usage(),
	}
	exit();
}

// ONE TIMER, REARMED AT EACH ABSOLUTE DEADLINE, so the period is the clock's and not the loop's: a frozen process owes
// nothing when it is thawed, since the monotonic clock did not move while it slept.
fn count(seconds: u64) {
	let Ok(timer) = u64::try_from(timer_create()) else {
		say("FAIL no timer");
		return;
	};
	let start: u64 = clock();
	for n in 1..=seconds.saturating_mul(10) {
		timer_set(timer, start + n * PERIOD_TICKS);
		if wait_any(&[timer], 0) < 0 {
			say("FAIL the timer's wait failed");
			break;
		}
		say(&format!("count {n} {}", stamp()));
	}
	close(timer);
	say("count done");
}

fn timer(ms: u64) {
	let Ok(timer) = u64::try_from(timer_create()) else {
		say("FAIL no timer");
		return;
	};
	let ticks: u64 = ms.saturating_mul(TICKS_PER_SECOND).div_ceil(1000);
	say(&format!("timer armed for {ms} ms {}", stamp()));
	timer_set(timer, clock().saturating_add(ticks));
	if wait_any(&[timer], 0) < 0 {
		say("FAIL the timer's wait failed");
	} else {
		say(&format!("timer fired {}", stamp()));
	}
	close(timer);
}
