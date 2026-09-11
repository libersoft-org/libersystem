//! The estimator's arithmetic, and the two schedules derived from it.

use super::*;

#[test]
fn before_any_measurement_the_timeout_is_one_second() {
	let rto = Rto::new();
	assert_eq!(rto.rto_ms(), INITIAL_RTO_MS);
	assert!(!rto.measured());
	assert_eq!(rto.srtt_ms(), None);
	assert_eq!(rto.rttvar_ms(), None);
}

#[test]
fn the_first_measurement_seeds_both_estimates_directly() {
	// RFC 6298 section 2.2: SRTT is the measurement, RTTVAR is half of it, and the timeout is four
	// variances above the mean - for a 100 ms path, 100 + 4 * 50 = 300 ms.
	let mut rto = Rto::new();
	assert!(rto.sample(100, false));
	assert_eq!(rto.srtt_ms(), Some(100));
	assert_eq!(rto.rttvar_ms(), Some(50));
	assert_eq!(rto.rto_ms(), 300);
}

#[test]
fn later_measurements_move_the_estimate_by_an_eighth_and_the_variance_by_a_quarter() {
	let mut rto = Rto::new();
	rto.sample(100, false);
	// A measurement equal to the estimate leaves the mean alone and shrinks the variance, which is
	// what makes a steady path tighten its timeout rather than keeping the first one for ever.
	rto.sample(100, false);
	assert_eq!(rto.srtt_ms(), Some(100), "an eighth of no error is no movement");
	assert_eq!(rto.rttvar_ms(), Some(37), "three quarters of 50");
	assert!(rto.rto_ms() < 300, "and the timeout follows it down");

	// A LONGER PATH MOVES BOTH AND THE TIMEOUT GROWS. The estimate takes an eighth of the error, so
	// one slow measurement does not double the timeout - and the variance takes a quarter, so a
	// path that varies keeps its margin.
	let before: u32 = rto.rto_ms();
	rto.sample(500, false);
	assert!(rto.srtt_ms().expect("measured") > 100);
	assert!(rto.rttvar_ms().expect("measured") > 37);
	assert!(rto.rto_ms() > before, "a longer round trip raises the timeout");
}

#[test]
fn karns_rule_refuses_a_measurement_from_a_retransmitted_segment() {
	// THE MEASUREMENT THAT CANNOT BE ATTRIBUTED IS NOT TAKEN. An acknowledgement covering a segment
	// that was sent twice may be answering either transmission, and the two readings differ by
	// exactly the retransmission interval - in the direction that makes the estimator too fast.
	let mut rto = Rto::new();
	rto.sample(100, false);
	let steady: Rto = rto;
	assert!(!rto.sample(10, true), "the sample is refused");
	assert_eq!(rto, steady, "and nothing about the estimator moved");
}

#[test]
fn the_timeout_is_clamped_at_both_ends() {
	// A fast path cannot drive the timeout below the floor this profile declares.
	let mut fast = Rto::new();
	fast.sample(0, false);
	assert_eq!(fast.rto_ms(), MIN_RTO_MS, "the 200 ms floor, which is this profile's deviation");

	// And a slow one cannot drive it past the ceiling.
	let mut slow = Rto::new();
	slow.sample(100_000, false);
	assert_eq!(slow.rto_ms(), MAX_RTO_MS);
}

#[test]
fn backoff_doubles_into_the_ceiling_and_a_measurement_resets_it() {
	let mut rto = Rto::new();
	assert_eq!(rto.rto_ms(), 1000);
	assert_eq!(rto.back_off(), 1);
	assert_eq!(rto.rto_ms(), 2000);
	assert_eq!(rto.back_off(), 2);
	assert_eq!(rto.rto_ms(), 4000);
	for _ in 0..10 {
		rto.back_off();
	}
	assert_eq!(rto.rto_ms(), MAX_RTO_MS, "doubling stops at the ceiling rather than overflowing past it");

	// A NEW MEASUREMENT ENDS THE BACKOFF. The path answered, so the doubling was compensating for
	// something that is over; carrying it forward would leave the connection slow while idle.
	rto.sample(100, false);
	assert_eq!(rto.backoffs(), 0);
	assert_eq!(rto.rto_ms(), 300);
}

#[test]
fn an_established_connection_gives_up_after_eight_retransmissions_of_one_segment() {
	let mut rto = Rto::new();
	for attempt in 1..MAX_DATA_RETRANSMISSIONS {
		rto.back_off();
		assert!(!rto.exhausted(), "attempt {attempt} is not the last");
	}
	rto.back_off();
	assert!(rto.exhausted(), "the eighth is");

	// And a fresh segment starts the count again without disturbing the estimate.
	let timeout: u32 = rto.rto_ms();
	rto.restart();
	assert_eq!(rto.backoffs(), 0);
	assert!(!rto.exhausted());
	assert_eq!(rto.rto_ms(), timeout, "restarting the count is not a new measurement");
}

#[test]
fn the_syn_schedule_spans_the_three_minutes_the_specification_reserves() {
	// THE DURATION AND THE CAP, because a duration-only assertion passes against an uncapped
	// doubling that this profile's own ceiling forbids.
	assert_eq!(syn_attempts(), 7);
	let expected: [u32; 7] = [1000, 2000, 4000, 8000, 16000, 32000, 60000];
	let mut retries: u32 = 0;
	for (index, interval) in expected.iter().enumerate() {
		assert_eq!(syn_interval(index), Some(*interval), "interval {index}");
		assert!(*interval <= MAX_RTO_MS, "no interval exceeds the ceiling");
		retries += interval;
	}
	assert_eq!(syn_interval(7), None, "and the schedule ends there");
	assert_eq!(retries, 123_000, "the retries themselves");
	assert_eq!(syn_schedule_ms(), 183_000, "plus the wait on the last, which is when the open fails");
	assert!(syn_schedule_ms() >= MIN_SYN_SCHEDULE_MS, "RFC 9293 section 3.8.3 reserves at least three minutes");

	// AND IT IS THE SHORTEST SCHEDULE THAT CLEARS IT: one fewer retransmission does not.
	let shorter: u32 = retries - expected[6] + expected[5];
	assert!(shorter < MIN_SYN_SCHEDULE_MS, "six retransmissions span {shorter} ms, which is short of the minimum");
}
