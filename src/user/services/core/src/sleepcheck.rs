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
//   sleepcheck flood SECONDS   a line as fast as the terminal takes it, for SECONDS of the monotonic clock: output ready
//                              at every one of the console's waits, which a command typed meanwhile has to get past -
//                              the worst case of the counter the sleep gates type `sleepctl` over

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

// What every line begins with.
const PREFIX: &str = "sleepcheck: ";

// ONE MESSAGE A LINE: the prefix, the text and the newline written together. They were three writes, and the
// terminal is shared - `sleepctl`, the shell and every other program print through the same console channel, a
// message at a time - so another program's output could land between this line's text and its end. Where that output
// ended in an unfinished line of its own, this line's newline finished THAT line, and the gate reading the counter
// found a counter line cut with no end left to rejoin: `sleepctl`'s record of the sleep, its first line and the
// second without its newline, landed after count 73's text, and count 73's newline ended the record's second line
// (riscv64 `tickless-idle`, 2026-11-12). A line written whole has other programs' lines only before or after it; the
// kernel's own lines, which reach the wire by another path, are the gate's to rejoin.
fn say(text: &str) {
	let mut line: String = String::with_capacity(PREFIX.len() + text.len() + 1);
	line.push_str(PREFIX);
	line.push_str(text);
	line.push('\n');
	print(line.as_bytes());
}

fn usage() -> ! {
	say("usage: sleepcheck count SECONDS | timer MS | clocks | flood SECONDS");
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
		["flood", seconds] => flood(number(seconds)),
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

// NO TIMER AND NO PAUSE: each line is sent as soon as the last one went, and a full terminal channel holds the next one
// back, so the console finds this output waiting every time it looks.
fn flood(seconds: u64) {
	let end: u64 = clock().saturating_add(seconds.saturating_mul(TICKS_PER_SECOND));
	let mut n: u64 = 0;
	while clock() < end {
		n += 1;
		say(&format!("flood {n}"));
	}
	say("flood done");
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
