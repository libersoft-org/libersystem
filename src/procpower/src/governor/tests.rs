use super::*;

fn candidates() -> [Candidate; 4] {
	[
		Candidate { exit_latency_us: 1, target_residency_us: 1, enterable: true },
		Candidate { exit_latency_us: 50, target_residency_us: 150, enterable: true },
		Candidate { exit_latency_us: 300, target_residency_us: 900, enterable: true },
		Candidate { exit_latency_us: 2000, target_residency_us: 5000, enterable: true },
	]
}

#[test]
fn the_deepest_state_whose_residency_the_prediction_covers_is_chosen() {
	assert_eq!(choose(&candidates(), 0, None), 0);
	assert_eq!(choose(&candidates(), 149, None), 0, "a state pays only from its target residency");
	assert_eq!(choose(&candidates(), 150, None), 1);
	assert_eq!(choose(&candidates(), 4999, None), 2);
	assert_eq!(choose(&candidates(), 1_000_000, None), 3);
}

#[test]
fn the_latency_bound_keeps_every_deeper_state_out() {
	assert_eq!(choose(&candidates(), 1_000_000, Some(300)), 2, "an exit latency at the bound is allowed");
	assert_eq!(choose(&candidates(), 1_000_000, Some(299)), 1);
	assert_eq!(choose(&candidates(), 1_000_000, Some(0)), 0, "the smallest bound means the halt, never a spin");
}

#[test]
fn a_state_the_core_cannot_enter_is_passed_over_for_the_next_shallower() {
	let mut states = candidates();
	states[3].enterable = false;
	assert_eq!(choose(&states, 1_000_000, None), 2);
	states[2].enterable = false;
	states[1].enterable = false;
	assert_eq!(choose(&states, 1_000_000, None), 0);
}

#[test]
fn the_prediction_is_the_shorter_of_the_timer_and_the_history() {
	let mut predictor = Predictor::new();
	assert_eq!(predictor.predict(None), 0, "nothing armed and no history: assume the shortest");
	assert_eq!(predictor.predict(Some(7000)), 7000);
	predictor.observe(800);
	assert_eq!(predictor.predict(Some(7000)), 800, "the history, when shorter than the timer");
	assert_eq!(predictor.predict(Some(300)), 300, "the timer, when shorter than the history");
	assert_eq!(predictor.predict(None), 800);
	// THE NEWEST WEIGHS AN EIGHTH: a run of short periods pulls a long average down, a step at a time.
	for _ in 0..8 {
		predictor.observe(0);
	}
	assert!(predictor.predict(None) < 800 / 2, "eight short periods move the average more than halfway");
	assert!(predictor.predict(None) > 0, "and not all the way at once");
}
