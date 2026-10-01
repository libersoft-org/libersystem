// procprobe - the processor-power gate's load and idle pattern. DEVELOPMENT-ONLY.
//
// It holds no authority: it only runs, or waits, so the kernel's governors have something to decide over.
//
//   procprobe load SECONDS [THREADS]   THREADS cores kept busy for SECONDS (one by default) - one thread each, from
//                                      the runtime's worker pool, so one launch loads every core
//   procprobe idle SECONDS   the scripted idle pattern for SECONDS: waits of 300 ms, 30 ms and one tick in turn, each
//                            one an idle period of a known length the idle governor predicts against

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::LaunchContext;
use rt::*;

fn say(line: &[u8]) {
	print(b"procprobe: ");
	print(line);
	print(b"\n");
}

fn seconds(text: &[u8]) -> u64 {
	core::str::from_utf8(text).ok().and_then(|text| text.parse().ok()).unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let args: Vec<&[u8]> = context.arguments.as_bytes().split(|&byte| byte == b' ').filter(|arg| !arg.is_empty()).collect();
	match args.as_slice() {
		[b"load", time, rest @ ..] => {
			let until = clock() + seconds(time) * TICKS_PER_SECOND;
			let threads = rest.first().map_or(1, |count| seconds(count).clamp(1, 64) as usize);
			let pool = rt::pool::Pool::new(threads - 1);
			let mut lanes: Vec<u64> = alloc::vec![0; threads];
			let mut items: Vec<u64> = alloc::vec![until; threads];
			pool.for_each(&mut lanes, &mut items, &|spins: &mut u64, until: &mut u64| {
				while clock() < *until {
					*spins = core::hint::black_box(spins.wrapping_add(1));
				}
			});
			say(b"load done");
		}
		[b"idle", time] => {
			let until = clock() + seconds(time) * TICKS_PER_SECOND;
			let pattern = [TICKS_PER_SECOND * 3 / 10, TICKS_PER_SECOND * 3 / 100, 1];
			let mut at = 0;
			while clock() < until {
				sleep_until(clock() + pattern[at % pattern.len()]);
				at += 1;
			}
			say(b"idle done");
		}
		_ => say(b"usage: procprobe load SECONDS [THREADS] | idle SECONDS"),
	}
	exit()
}
