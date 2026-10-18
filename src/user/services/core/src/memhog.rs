// memhog - a program that needs memory, for the balloon gate: `memhog MIB [SECONDS]` takes MIB mebibytes in 16 MiB
// objects, touches every page of each, says when it holds them all, holds them SECONDS (5 when not said) and ends.
// DEVELOPMENT-ONLY.
//
// IT ASKS AS AN ORDINARY PROGRAM DOES, AND WAITS WHEN THE MACHINE HAS NONE: an object the kernel cannot give yet is
// asked for again a fifth of a second later, for up to a minute - which is how a machine whose memory a balloon holds
// is seen to give it back under pressure rather than refuse the program that needs it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use proto::system::LaunchContext;
use rt::*;

const CHUNK: u64 = 16 << 20;
const PATIENCE_TICKS: u64 = 60 * 100;
const RETRY_TICKS: u64 = 20;

fn say(line: &str) {
	write_stdout(format!("memhog: {line}\n").as_bytes());
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	inherit_stdout(bootstrap);
	let launch = recv_launch_bytes(bootstrap);
	let context = launch.as_deref().and_then(LaunchContext::decode);
	let arguments = context.as_ref().map_or("", |context| context.arguments.trim());
	let mut words = arguments.split_whitespace();
	let Some(mib) = words.next().and_then(|word| word.parse::<u64>().ok()).filter(|mib| *mib > 0 && *mib <= 1 << 16) else {
		say("usage: memhog MIB [SECONDS]");
		exit_with(1);
	};
	let seconds = words.next().and_then(|word| word.parse::<u64>().ok()).unwrap_or(5).min(600);
	let chunks = (mib << 20).div_ceil(CHUNK);
	let mut held: Vec<u64> = Vec::new();
	if held.try_reserve_exact(chunks as usize).is_err() {
		say("no room to count the objects");
		exit_with(1);
	}
	let give_up = clock() + PATIENCE_TICKS;
	let mut waited = 0u64;
	while (held.len() as u64) < chunks {
		let object = memory_object_create(CHUNK);
		if object < 0 {
			if clock() >= give_up {
				say(&format!("only {} MiB could be had in a minute", held.len() as u64 * (CHUNK >> 20)));
				exit_with(1);
			}
			waited += 1;
			sleep_until(clock() + RETRY_TICKS);
			continue;
		}
		let object = object as u64;
		// EVERY PAGE TOUCHED, so the memory is the program's in fact and not only in an object's size.
		let Some(base) = (unsafe { map_object(object) }) else {
			say("an object could not be mapped");
			exit_with(1);
		};
		for page in 0..(CHUNK / 4096) {
			// SAFETY: inside the object just mapped.
			unsafe { core::ptr::write_volatile((base + page * 4096) as *mut u8, 0xA5) };
		}
		held.push(object);
	}
	say(&format!("{mib} MiB held after {waited} wait(s)"));
	sleep_until(clock() + seconds * 100);
	say("done");
	exit();
}
