use super::*;

// A counter at a thousand hertz: ten cycles to the tick, which keeps every number below readable.
const HZ: u64 = 1_000;

fn anchored_at(now: u64) -> Clock {
	let clock = Clock::new();
	assert!(clock.anchor(now, HZ));
	clock
}

#[test]
fn an_unanchored_clock_answers_zero_and_programs_nothing() {
	let clock = Clock::new();
	assert_eq!(clock.ticks(123_456), 0);
	assert_eq!(clock.nanos(123_456), 0);
	assert_eq!(clock.counter_at(5), None);
	assert!(!clock.anchor(0, TICK_HZ - 1), "a counter slower than the tick counts no tick");
	assert!(anchored_at(0).anchored());
	let once = anchored_at(0);
	assert!(!once.anchor(500, HZ), "anchored once");
}

#[test]
fn the_tick_is_the_counter_since_the_anchor_divided_by_the_cycles_in_one() {
	let clock = anchored_at(1_000);
	assert_eq!(clock.cycles_per_tick(), 10);
	assert_eq!(clock.ticks(1_000), 0);
	assert_eq!(clock.ticks(1_009), 0);
	assert_eq!(clock.ticks(1_010), 1);
	assert_eq!(clock.ticks(2_000), 100);
	// NOTHING HAS TO HAPPEN FOR TIME TO PASS: the reading is the whole computation.
	assert_eq!(clock.ticks(1_000 + 10 * 12_345), 12_345);
}

#[test]
fn a_reading_never_goes_backwards_whichever_core_takes_it() {
	let clock = anchored_at(1_000);
	assert_eq!(clock.ticks(2_000), 100);
	assert_eq!(clock.ticks(1_500), 100, "a core whose counter reads behind answers the larger value");
	assert_eq!(clock.ticks(500), 100, "and one behind the anchor itself neither wraps nor steps back");
	let fresh = anchored_at(1_000);
	assert_eq!(fresh.ticks(500), 0, "a reading behind the anchor is zero, not the far end of the counter");
	assert_eq!(fresh.elapsed(500), 0);
}

#[test]
fn a_tick_deadline_is_the_counter_reading_it_begins_at() {
	let clock = anchored_at(1_000);
	assert_eq!(clock.counter_at(0), Some(1_000));
	assert_eq!(clock.counter_at(5), Some(1_050));
	for tick in [0, 1, 7, 99, 100_000] {
		let counter = clock.counter_at(tick).unwrap();
		assert_eq!(anchored_at(1_000).ticks(counter), tick, "tick {tick} begins at {counter}");
		assert_eq!(anchored_at(1_000).ticks(counter - 1), tick.saturating_sub(1));
	}
	assert_eq!(clock.counter_at(u64::MAX), None, "a deadline too far away to express is not programmed at all");
}

#[test]
fn nanoseconds_are_the_counter_less_the_offset() {
	let clock = anchored_at(1_000);
	assert_eq!(clock.nanos(1_000), 1_000_000_000, "since the counter's zero, as SYS_CLOCK_MONO_NS always answered");
	assert_eq!(clock.nanos(1_500), 1_500_000_000);
	assert_eq!(cycles_to_ns(3, 0), 0);
}

#[test]
fn a_sleep_on_a_counter_that_kept_running_is_excluded_from_both_clocks() {
	let clock = anchored_at(0);
	assert_eq!(clock.ticks(5_000), 500);
	let nanos_at_suspend = clock.nanos(5_000);
	clock.suspend(5_000);
	assert!(clock.suspended());
	// INSIDE THE SUSPENDED STATE every reading answers the suspend value - on any core, in any interrupt.
	for reading in [5_000, 6_000, 9_000, u64::MAX / 2] {
		assert_eq!(clock.ticks(reading), 500, "a reading at {reading} counts none of the sleep");
		assert_eq!(clock.nanos(reading), nanos_at_suspend);
	}
	assert_eq!(clock.counter_at(501), None, "and nothing is programmed against a clock that is not running");
	// THE REBASE, four thousand cycles later on the same counter.
	clock.rebase(5_000, 9_000);
	assert!(!clock.suspended());
	assert_eq!(clock.ticks(9_000), 500, "time resumes where it stopped");
	assert_eq!(clock.ticks(9_100), 510);
	assert_eq!(clock.nanos(9_000), nanos_at_suspend);
	assert_eq!(clock.counter_at(510), Some(9_100), "and a deadline is converted through the offset");
	assert_eq!(clock.ticks(8_000), 510, "no reading after the rebase is below one already answered");
}

#[test]
fn a_sleep_on_a_counter_that_restarted_lands_where_the_clock_left_off() {
	let clock = anchored_at(1_000);
	assert_eq!(clock.ticks(6_000), 500);
	let nanos_at_suspend = clock.nanos(6_000);
	clock.suspend(6_000);
	assert_eq!(clock.ticks(20), 500, "a restarted counter read before the rebase changes nothing");
	// S3: the counter came back from zero and reads twenty.
	clock.rebase(6_000, 20);
	assert_eq!(clock.ticks(20), 500, "the small reading is the suspend value");
	assert_eq!(clock.ticks(120), 510);
	assert_eq!(clock.nanos(20), nanos_at_suspend);
	assert_eq!(clock.counter_at(510), Some(120));
	assert_eq!(clock.ticks(0), 510, "and no reading after it is below one already answered");
}

#[test]
fn a_suspension_never_raises_the_maximum_past_the_suspend_value() {
	let clock = anchored_at(0);
	assert_eq!(clock.ticks(3_000), 300);
	clock.suspend(3_000);
	assert_eq!(clock.ticks(1_000_000), 300, "a counter far ahead during the sleep");
	clock.rebase(3_000, 1_000_000);
	assert_eq!(clock.ticks(1_000_000), 300, "the maximum was not raised by the reading taken while suspended");
	assert_eq!(clock.ticks(1_000_050), 305);
}

#[test]
fn two_sleeps_accumulate_in_the_one_offset() {
	let clock = anchored_at(0);
	clock.suspend(1_000);
	clock.rebase(1_000, 3_000);
	assert_eq!(clock.ticks(3_000), 100);
	clock.suspend(4_000);
	assert_eq!(clock.ticks(4_000), 200);
	clock.rebase(4_000, 10);
	assert_eq!(clock.ticks(10), 200, "a second sleep, on a counter that restarted, after one on a counter that kept running");
	assert_eq!(clock.ticks(1_010), 300);
	assert_eq!(clock.counter_at(300), Some(1_010));
}
