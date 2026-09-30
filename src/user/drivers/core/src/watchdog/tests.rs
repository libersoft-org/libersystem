use super::*;

#[test]
fn the_i6300esb_splits_the_timeout_into_two_preloads_of_its_unit_and_rounds_down() {
	// 60 s: two stages of 30 s, each 30,000,000,000 ns / 983,040 ns = 30517 units.
	let division = i6300esb(60_000).expect("sixty seconds counts");
	assert_eq!(division.count, 30_517);
	assert_eq!(division.effective_ms, 59_998, "rounded down, never up");
	assert!(division.effective_ms <= 60_000);
	assert_eq!(i6300esb(1), None, "below two units nothing counts");
	let (min, max, granularity) = i6300esb_range();
	assert_eq!((min, granularity), (2, 2));
	assert!(i6300esb(min).is_some());
	// Past the 20-bit preload the division stops at the longest the device counts.
	let longest = i6300esb(u32::MAX).expect("a long timeout is cut to the longest");
	assert_eq!(longest.count, ESB_PRELOAD_MAX);
	assert_eq!(longest.effective_ms, max);
}

#[test]
fn the_tco_counts_pairs_of_ticks_from_two_to_1023() {
	assert_eq!(tco(60_000), Some(Division { effective_ms: 60_000, count: 50 }));
	assert_eq!(tco(61_199), Some(Division { effective_ms: 60_000, count: 50 }), "rounded down to 1.2 s");
	assert_eq!(tco(2_399), None, "below 2.4 s the device cannot count");
	assert_eq!(tco(2_400), Some(Division { effective_ms: 2_400, count: 2 }));
	assert_eq!(tco(u32::MAX), Some(Division { effective_ms: 1_227_600, count: 1_023 }));
	assert_eq!(tco_range(), (2_400, 1_227_600, 1_200));
}

#[test]
fn a_wdat_counts_its_table_s_period_within_its_table_s_range() {
	assert_eq!(wdat(60_000, 1_200, 2, 1_023), Some(Division { effective_ms: 60_000, count: 50 }));
	assert_eq!(wdat(1_000, 1_200, 2, 1_023), None, "below the table's minimum");
	assert_eq!(wdat(10_000_000, 1_000, 1, 600), Some(Division { effective_ms: 600_000, count: 600 }), "cut to the table's maximum");
	assert_eq!(wdat(60_000, 0, 1, 10), None, "a period of nothing is no table");
	assert_eq!(wdat(60_000, 1_000, 10, 5), None, "nor a range upside down");
}

#[test]
fn a_bmc_counts_tenths_of_a_second_in_sixteen_bits() {
	assert_eq!(bmc(60_000), Some(Division { effective_ms: 60_000, count: 600 }));
	assert_eq!(bmc(99), None);
	assert_eq!(bmc(u32::MAX), Some(Division { effective_ms: 6_553_500, count: 65_535 }));
}

// THE WATCHDOG'S STEP IN A SLEEP, over a timer the test drives: left alone when not armed; disarmed where the device
// allows it; otherwise its longest timeout with the bound it gives - unless the device stops counting in the state
// asked for; and armed again at the resume with the timeout it had.
#[test]
fn a_watchdog_in_a_sleep_is_disarmed_where_it_can_be_and_otherwise_bounds_the_sleep() {
	use crate::common::SleepStep;
	use driver_protocol::{SleepState, SuspendOutcome, SuspendRequest};
	use proto::system::{Error, WatchdogDescription};

	#[derive(Default)]
	struct Fake {
		can_disarm: bool,
		stops_in_s3: bool,
		armed: Option<u32>,
		arms: Vec<u32>,
		pets: u32,
	}
	impl super::Timer for Fake {
		fn describe(&mut self) -> WatchdogDescription {
			WatchdogDescription { device: String::from("fake"), min_timeout_ms: 1_000, max_timeout_ms: 600_000, granularity_ms: 1_000, can_disarm: self.can_disarm, survives_reset: false, stops_in_suspend_to_idle: false, stops_in_s3: self.stops_in_s3, running_at_bind: false, last_reset_was_watchdog: false }
		}
		fn arm(&mut self, timeout_ms: u32) -> Result<u32, Error> {
			self.armed = Some(timeout_ms);
			self.arms.push(timeout_ms);
			Ok(timeout_ms)
		}
		fn pet(&mut self) -> Result<(), Error> {
			self.pets += 1;
			Ok(())
		}
		fn disarm(&mut self) -> Result<(), Error> {
			if !self.can_disarm {
				return Err(Error::Unsupported);
			}
			self.armed = None;
			Ok(())
		}
	}
	let idle = SuspendRequest { state: SleepState::Idle, arm_wake: false, timed_wake_ms: 0 };
	let ram = SuspendRequest { state: SleepState::Ram, arm_wake: false, timed_wake_ms: 0 };
	let run = |timer: &mut Fake, armed: Option<u32>, request: &SuspendRequest| -> (u64, SuspendOutcome, Option<u32>) {
		let mut provider = super::Provider { timer, consumer_acted: true, armed_ms: armed };
		if let Some(timeout) = armed {
			provider.timer.armed = Some(timeout);
		}
		let mut serving = crate::common::Serving::from_offers(&[]);
		let mut step = super::Sleep { provider: &mut provider, serving: &mut serving, slept_ms: None };
		let answer = step.suspend(request);
		let during = step.provider.timer.armed;
		assert!(step.resume(request.state.loses_power()));
		(answer.awake_by_ms, answer.outcome, during)
	};
	// Not armed: left alone, and left alone after.
	let mut timer = Fake { can_disarm: true, ..Fake::default() };
	assert_eq!(run(&mut timer, None, &idle), (0, SuspendOutcome::Done, None));
	assert!(timer.arms.is_empty(), "a timeout set on a stopped timer would start it");
	// Armed and able to stop: disarmed for the sleep, armed again with its timeout after it.
	let mut timer = Fake { can_disarm: true, ..Fake::default() };
	assert_eq!(run(&mut timer, Some(30_000), &idle), (0, SuspendOutcome::Done, None));
	assert_eq!(timer.arms, vec![30_000], "armed again with the timeout it had");
	// Armed, unable to stop, counting in suspend to idle: its longest timeout, petted, and that is the bound.
	let mut timer = Fake { can_disarm: false, stops_in_s3: true, ..Fake::default() };
	assert_eq!(run(&mut timer, Some(30_000), &idle), (600_000, SuspendOutcome::Done, Some(600_000)));
	assert_eq!(timer.pets, 1, "petted a last time");
	assert_eq!(timer.arms, vec![600_000, 30_000]);
	// The same device in S3, where it loses its power: no bound.
	let mut timer = Fake { can_disarm: false, stops_in_s3: true, ..Fake::default() };
	assert_eq!(run(&mut timer, Some(30_000), &ram).0, 0);
	assert_eq!(timer.arms, vec![600_000, 30_000]);
	// One that keeps counting in S3 bounds that sleep too.
	let mut timer = Fake { can_disarm: false, stops_in_s3: false, ..Fake::default() };
	assert_eq!(run(&mut timer, Some(30_000), &ram).0, 600_000);
}
