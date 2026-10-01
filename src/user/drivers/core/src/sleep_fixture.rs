// THE SLEEP GATE'S FIXTURE DRIVER - DEVELOPMENT-ONLY. Bound to an `ivshmem-plain` function the harness hot-plugs into the
// empty native hot-plug port after boot, so it is the LAST binding and DeviceManager suspends it FIRST: while it holds its
// answer, no other binding has been asked.
//
// WHAT IT DOES IS WHAT THE HARNESS WRITES into the shared memory's first word, read at each `SUSPEND`:
//   HOLD    the answer is held - said on the log - until the harness writes the release word, then `SUSPENDED`, done;
//   REFUSE  the suspend is refused, naming this binding - the test hook the transaction's unwinding is proved with;
//   anything else: done at once, as any driver that has nothing to stop.
// It counts every `SUSPEND` it saw in the second word, and holds nothing across the sleep.

#![no_std]
#![no_main]

extern crate alloc;

use drivers::common;
use rt::*;

// The shared memory's words.
const CONTROL: u64 = 0;
const RELEASE: u64 = 4;
const SEEN: u64 = 8;
const HOLD: u32 = 0x444C_4F48;
const REFUSE: u32 = 0x5355_4652;
const RELEASED: u32 = 0x534C_4552;
// How often a held answer looks for the release word.
const LOOK_TICKS: u64 = 1;

fn say(text: &str) {
	print(alloc::format!("driver.sleep-fixture: {text}\n").as_bytes());
}

struct Fixture {
	memory: u64,
}

impl Fixture {
	fn read(&self, offset: u64) -> u32 {
		// SAFETY: the window the claim mapped is the shared memory's, far longer than three words.
		unsafe { ((self.memory + offset) as *const u32).read_volatile() }
	}

	fn write(&self, offset: u64, value: u32) {
		// SAFETY: as above.
		unsafe { ((self.memory + offset) as *mut u32).write_volatile(value) }
	}
}

impl common::SleepStep for Fixture {
	fn suspend(&mut self, _request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		self.write(SEEN, self.read(SEEN).wrapping_add(1));
		let done = driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 };
		match self.read(CONTROL) {
			REFUSE => {
				say("refuses the suspend (the harness's hook)");
				driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Refused(driver_protocol::DriverFailureCode::DeviceNotResponding), awake_by_ms: 0 }
			}
			HOLD => {
				say("holds its answer until the harness releases it");
				while self.read(RELEASE) != RELEASED {
					sleep_until(clock() + LOOK_TICKS);
				}
				self.write(RELEASE, 0);
				say("released - answers done");
				done
			}
			_ => done,
		}
	}

	fn resume(&mut self, _lost_power: bool) -> bool {
		true
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	let memory: u64 = if resources.device != 0 { unsafe { syscall(SYS_DEVICE_MEMORY_MAP, resources.device, 0, 0, 0) } } else { 0 };
	if memory == 0 || sys_is_err(memory) {
		say("its shared memory could not be mapped");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	common::takes_sleep();
	if !common::online(bootstrap, &bind, b"driver.sleep-fixture: online", &[]) {
		exit();
	}
	let mut fixture = Fixture { memory };
	loop {
		let Some(ready) = common::wait_or_answer_until(bootstrap, &bind, &[], u64::MAX, None) else {
			if common::stop_requested() {
				common::finish_stop(bootstrap, &bind, resources.device, true);
			}
			exit();
		};
		if ready.is_none() && !common::take_sleep_step(bootstrap, &bind, &mut fixture) {
			if common::stop_requested() {
				common::finish_stop(bootstrap, &bind, resources.device, true);
			}
			exit();
		}
	}
}
