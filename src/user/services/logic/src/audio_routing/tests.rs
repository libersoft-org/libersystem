use super::*;
use alloc::vec;

#[test]
// HEADPHONES CONNECTED TAKE THE MUSIC, and the speakers return when they go.
fn an_arrival_becomes_the_default_and_its_leaving_returns_the_previous_one() {
	let mut routing = Routing::new();
	assert_eq!(routing.default(Direction::Output), None);
	routing.arrive(1, &[Direction::Output, Direction::Input]);
	routing.arrive(2, &[Direction::Output]);
	assert_eq!(routing.default(Direction::Output), Some(2), "the newest output is the default");
	assert_eq!(routing.default(Direction::Input), Some(1), "an output-only arrival leaves the input default alone");
	routing.arrive(3, &[Direction::Output]);
	routing.leave(3);
	assert_eq!(routing.default(Direction::Output), Some(2), "the default it displaced returns");
	routing.leave(2);
	assert_eq!(routing.default(Direction::Output), Some(1));
	routing.leave(1);
	assert_eq!(routing.default(Direction::Output), None);
	assert_eq!(routing.default(Direction::Input), None);
}

#[test]
// THE OPERATOR'S CHOICE is where an arrival would put the device; a later arrival still takes over, and the choice
// returns when it leaves.
fn the_operator_chooses_among_the_devices_that_serve_the_direction() {
	let mut routing = Routing::new();
	routing.arrive(1, &[Direction::Output]);
	routing.arrive(2, &[Direction::Output, Direction::Input]);
	assert!(routing.choose(1, Direction::Output));
	assert_eq!(routing.default(Direction::Output), Some(1));
	assert!(!routing.choose(1, Direction::Input), "a device that cannot record is no input");
	routing.arrive(4, &[Direction::Output]);
	assert_eq!(routing.default(Direction::Output), Some(4));
	routing.leave(4);
	assert_eq!(routing.default(Direction::Output), Some(1), "the operator's choice, not the device it displaced");
	assert!(!routing.choose(9, Direction::Output), "an unknown device is refused");
}

#[test]
// A STREAM THAT NAMED A DEVICE plays there while it serves the direction, and moves to the default when it leaves -
// never ending; with nothing at all it plays nowhere.
fn a_named_stream_moves_to_the_default_when_its_device_leaves() {
	let mut routing = Routing::new();
	routing.arrive(1, &[Direction::Output]);
	routing.arrive(2, &[Direction::Output]);
	assert_eq!(place(&routing, Some(1), Direction::Output), Some(1));
	assert_eq!(place(&routing, None, Direction::Output), Some(2));
	routing.leave(1);
	assert_eq!(place(&routing, Some(1), Direction::Output), Some(2));
	routing.leave(2);
	assert_eq!(place(&routing, Some(1), Direction::Output), None);
	assert_eq!(place(&routing, Some(1), Direction::Input), None);
}

#[test]
// A VOICE SESSION takes the voice device both ways where there is one, and the default output and input otherwise.
fn voice_goes_to_the_headset_or_else_the_defaults() {
	let mut routing = Routing::new();
	assert_eq!(place_voice(&routing), (None, None));
	routing.arrive(1, &[Direction::Output, Direction::Input]);
	assert_eq!(place_voice(&routing), (Some(1), Some(1)));
	routing.arrive(5, &[Direction::Output]);
	assert_eq!(place_voice(&routing), (Some(5), Some(1)));
	routing.arrive(7, &[Direction::Voice]);
	assert_eq!(place_voice(&routing), (Some(7), Some(7)));
	assert_eq!(routing.default(Direction::Output), Some(5), "a voice device is not the music's output");
	routing.leave(7);
	assert_eq!(place_voice(&routing), (Some(5), Some(1)));
}

#[test]
fn the_inventory_bound_forgets_the_oldest_rather_than_growing() {
	let mut routing = Routing::new();
	for id in 0..(MAX_DEVICES as u32 + 4) {
		routing.arrive(id, &[Direction::Output]);
	}
	assert_eq!(routing.output.as_slice().len(), MAX_DEVICES);
	assert_eq!(routing.default(Direction::Output), Some(MAX_DEVICES as u32 + 3));
	assert!(!routing.serves(0, Direction::Output));
	routing.arrive(10, &[Direction::Output]);
	assert_eq!(routing.output.as_slice().iter().filter(|held| **held == 10).count(), 1, "an arrival held already is moved, not doubled");
}

#[test]
// THE LEVEL'S CURVE: unity at 100, silence at 0, a quarter of the amplitude at 50, and nothing past unity.
fn a_level_is_a_squared_gain_and_scaling_stays_in_range() {
	assert_eq!(gain_q15(100), 32_768);
	assert_eq!(gain_q15(0), 0);
	assert_eq!(gain_q15(50), 8_192);
	assert_eq!(gain_q15(200), 32_768);
	assert_eq!(scale(i16::MAX, gain_q15(100)), i16::MAX);
	assert_eq!(scale(i16::MIN, gain_q15(100)), i16::MIN);
	assert_eq!(scale(20_000, gain_q15(50)), 5_000);
	assert_eq!(scale(-20_000, gain_q15(50)), -5_000);
	assert_eq!(scale(12_345, 0), 0);
}

#[test]
// THE TIMER'S FRAMES are counted from its start: forty-eight thousand a second whatever the steps, and the wait for the
// next period lands where the period comes due.
fn the_timer_pacer_counts_from_its_start_without_drift() {
	let mut pacer = TimerPacer::new(48_000, 1_000);
	assert_eq!(pacer.due(1_000), 0);
	let mut now = 1_000;
	let mut played = 0;
	for _ in 0..1000 {
		now += 3_333_333;
		let due = pacer.due(now);
		pacer.take(due);
		played += due;
	}
	assert_eq!(played, (3_333_333u128 * 1000 * 48_000 / 1_000_000_000) as u64, "no step's rounding is lost");
	let next = pacer.when(512);
	assert_eq!(pacer.due(next), 512);
	assert_eq!(pacer.due(next - 1), 511);
	let voice = TimerPacer::new(8_000, 0);
	assert_eq!(voice.due(1_000_000_000), 8_000);
}

#[test]
// ONE OUT FOR ONE IN: nothing goes before a packet came, silence answers an arrival with nothing ready, and the credit
// is bounded.
fn a_voice_link_sends_one_for_each_that_arrives() {
	let mut link = OneForOne::default();
	assert!(!link.send());
	link.arrived();
	link.arrived();
	assert!(link.send());
	link.silence();
	assert_eq!(link.silent, 1);
	assert!(!link.send());
	for _ in 0..20 {
		link.arrived();
	}
	let mut sent = 0;
	while link.send() {
		sent += 1;
	}
	assert_eq!(sent, 8, "a stall does not bank more than eight");
}

#[test]
// THE JITTER BUFFER: it fills to half before it plays, an overflow drops the oldest period, an underrun is silence -
// both counted - and it refills before it plays again.
fn the_jitter_buffer_is_bounded_both_ways_and_counts_what_it_loses() {
	// 200 ms at 1 kHz mono is 200 frames; periods of 50.
	let mut jitter = Jitter::new(1_000, 1, 200);
	let period = |tag: i16| vec![tag; 50];
	jitter.push(period(1));
	assert_eq!(jitter.pop(), None, "filling, not dry");
	assert_eq!(jitter.underruns, 0);
	jitter.push(period(2));
	assert_eq!(jitter.pop(), Some(period(1)));
	jitter.push(period(3));
	jitter.push(period(4));
	jitter.push(period(5));
	jitter.push(period(6));
	assert_eq!(jitter.overflows, 1, "the fifth period past 200 frames drops the oldest");
	assert_eq!(jitter.held(), 200);
	assert_eq!(jitter.pop(), Some(period(3)));
	for _ in 0..3 {
		assert!(jitter.pop().is_some());
	}
	assert_eq!(jitter.pop(), None);
	assert_eq!(jitter.underruns, 1);
	jitter.push(period(7));
	assert_eq!(jitter.pop(), None, "it refills to half before playing again");
	assert_eq!(jitter.underruns, 1, "and that is not counted twice");
	// A PERIOD LONGER THAN THE WHOLE BOUND is dropped, not held.
	jitter.push(vec![0; 400]);
	assert_eq!(jitter.overflows, 3);
}
