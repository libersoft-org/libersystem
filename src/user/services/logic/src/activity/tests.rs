use super::*;

#[test]
fn idle_comes_once_after_the_interval_and_active_once_at_the_next_input() {
	let mut watch = Watch::new(1_000).expect("ten seconds is in bounds");
	assert_eq!(watch.tick(0, 999), None, "not before the interval");
	assert_eq!(watch.due(0), Some(1_000));
	assert_eq!(watch.tick(0, 1_000), Some(Edge::Idle));
	assert_eq!(watch.tick(0, 5_000), None, "an edge, not a level");
	assert_eq!(watch.due(0), None, "nothing is due while idle");
	assert_eq!(watch.input(), Some(Edge::Active));
	assert_eq!(watch.input(), None, "and active once");
}

#[test]
fn input_before_the_interval_moves_the_deadline_and_says_nothing() {
	let mut watch = Watch::new(1_000).expect("in bounds");
	assert_eq!(watch.input(), None, "an active watch says nothing on input");
	assert_eq!(watch.tick(500, 1_400), None, "the interval counts from the last input");
	assert_eq!(watch.due(500), Some(1_500));
	assert_eq!(watch.tick(500, 1_500), Some(Edge::Idle));
}

#[test]
fn an_interval_outside_a_second_to_a_day_is_refused() {
	assert!(Watch::new(MIN_IDLE_AFTER_TICKS - 1).is_none());
	assert!(Watch::new(MAX_IDLE_AFTER_TICKS + 1).is_none());
	assert!(Watch::new(MIN_IDLE_AFTER_TICKS).is_some());
}
