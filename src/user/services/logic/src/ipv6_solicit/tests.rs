// The exact formulas, checked against injected randomness rather than against a range.
//
// A schedule tested only by "it retried eventually" would pass with the jitter applied to the wrong
// operand, with equality at MRT re-jittered, or with a retry count that silently stops the host
// asking. These check the numbers the plan fixed.

use super::*;

#[test]
fn the_first_interval_is_irt_plus_rand_times_irt() {
	assert_eq!(first_interval(-100), 3_600, "RAND = -0.1");
	assert_eq!(first_interval(0), 4_000, "RAND = 0");
	assert_eq!(first_interval(50), 4_200, "RAND = +0.05");
	// The whole band, which the QEMU capture checks with tick tolerance.
	assert!((3_600..4_400).contains(&first_interval(RAND_MIN_MILLI)));
	assert!((3_600..4_400).contains(&first_interval(RAND_MAX_MILLI - 1)));
}

#[test]
fn a_subsequent_interval_jitters_rtprev_and_not_twice_it() {
	assert_eq!(next_interval(10_000, -100), 19_000, "RAND = -0.1");
	assert_eq!(next_interval(10_000, 0), 20_000, "RAND = 0");
	assert_eq!(next_interval(10_000, 50), 20_500, "RAND = +0.05");
	// The band is [1.9, 2.1) * RTprev. Jittering twice RTprev would give [1.8, 2.2) and pass a
	// looser test while being wrong.
	assert_eq!(next_interval(10_000, 99), 20_990);
	assert!(next_interval(10_000, 99) < 21_000);
}

#[test]
fn equality_with_mrt_is_not_above_it() {
	// An interval that doubles exactly onto MRT with no jitter stays: replacing it would re-jitter a
	// value already inside the band.
	assert_eq!(next_interval(MRT_MS / 2, 0), MRT_MS);
	// One thousandth above, and the replacement applies.
	let above = next_interval(MRT_MS / 2 + 1_000, 0);
	assert!(above <= MRT_MS, "replaced by the capped formula rather than left at 3602 s");
}

#[test]
fn the_capped_band_is_the_stated_one_and_mrt_is_not_a_hard_ceiling() {
	// Any RTprev large enough to exceed MRT lands in [3240, 3960) seconds.
	for rand in [-100i32, -50, 0, 50, 99] {
		let capped = next_interval(MRT_MS, rand);
		assert!((3_240_000..3_960_000).contains(&capped), "RAND {rand} gave {capped}");
	}
	assert_eq!(next_interval(MRT_MS, -100), 3_240_000);
	assert_eq!(next_interval(MRT_MS, 0), MRT_MS);
	assert_eq!(next_interval(MRT_MS, 50), 3_780_000, "above MRT after jitter, which is intended");

	// And a repeated capped retry stays in the band rather than growing.
	let mut interval = MRT_MS;
	for _ in 0..10 {
		interval = next_interval(interval, 99);
		assert!((3_240_000..3_960_000).contains(&interval));
	}
}

#[test]
fn a_lost_solicitation_is_retried_and_the_third_one_configures_the_host() {
	// The peer drops the first two and answers the third.
	let mut schedule = Solicitation::new();
	let first = schedule.start(0, 0);
	assert_eq!(first, 4_000);
	assert_eq!(schedule.deadline(), Some(4_000));
	assert!(schedule.is_running());

	let second = schedule.on_timeout(4_000, 0).expect("still running");
	assert_eq!(second, 8_000);
	assert_eq!(schedule.deadline(), Some(12_000));

	let third = schedule.on_timeout(12_000, 0).expect("still running");
	assert_eq!(third, 16_000);
	assert_eq!(schedule.sent(), 3);

	// The answer arrives and installs a default route.
	schedule.default_route_installed();
	assert!(!schedule.is_running());
	assert_eq!(schedule.deadline(), None);
	assert_eq!(schedule.on_timeout(30_000, 0), None, "a stray timer cannot restart it");
}

#[test]
fn only_an_installed_default_route_stops_it() {
	let mut schedule = Solicitation::new();
	schedule.start(0, 0);
	// An advertisement that arrived but reserved nothing leaves the host with no router, so the
	// schedule keeps running. The caller expresses that by simply not calling the stop.
	assert!(schedule.is_running());
	assert!(schedule.on_timeout(4_000, 0).is_some());
	assert!(schedule.is_running());
}

#[test]
fn the_list_becoming_empty_by_any_route_restarts_it_at_the_first_interval() {
	let mut schedule = Solicitation::new();
	schedule.start(0, 0);
	// Back off a long way.
	let mut now = 4_000u64;
	for _ in 0..10 {
		let interval = schedule.on_timeout(now, 0).expect("running");
		now += interval;
	}
	assert!(schedule.interval().expect("running") > 60_000);

	schedule.default_route_installed();
	assert!(!schedule.is_running());

	// THE ROUTER WITHDRAWS ITSELF WITH LIFETIME ZERO, which is not expiry. The list is empty either
	// way, and the state is what the rule names.
	let restarted = schedule.router_list_empty(now, 0);
	assert_eq!(restarted, 4_000, "a host that just lost its last router asks again promptly");
	assert!(schedule.is_running());
	assert_eq!(schedule.deadline(), Some(now + 4_000));
}

#[test]
fn there_is_no_retry_bound() {
	// MRC = 0 and MRD = 0: the count is diagnostic and never terminates the schedule.
	let mut schedule = Solicitation::new();
	schedule.start(0, 0);
	let mut now = 0u64;
	for _ in 0..100 {
		let interval = schedule.on_timeout(now, 0).expect("a host with no router keeps asking");
		now += interval;
	}
	assert!(schedule.is_running());
	assert_eq!(schedule.sent(), 101);
}
