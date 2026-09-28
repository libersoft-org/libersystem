use super::*;

fn on(device: Option<&str>) -> Policy {
	Policy::from_keys(BootMode::Development, Some(true), device, None, None, None).expect("the defaults are a policy")
}

fn off() -> Policy {
	Policy::from_keys(BootMode::Development, None, None, None, None, None).expect("the defaults are a policy")
}

// Run the choice to its end, answering every arm with `arms(name)` and every disarm with `disarms(name)`.
fn settle(choice: &mut Choice, policy: &Policy, arms: impl Fn(&str) -> bool, disarms: impl Fn(&str) -> bool) -> Vec<(String, Step)> {
	let mut done = Vec::new();
	while let Some(step) = choice.next(policy) {
		let name = match step {
			Step::Arm(at) | Step::Disarm(at) | Step::Feed(at) => choice.held()[at].name.clone(),
		};
		match step {
			Step::Arm(at) => choice.arm_answered(at, arms(&name)),
			Step::Disarm(at) => choice.disarm_answered(at, disarms(&name)),
			Step::Feed(at) => choice.feeding(at),
		}
		done.push((name, step));
		assert!(done.len() < 32, "the choice ends");
	}
	done
}

#[test]
fn the_mode_decides_the_default_and_a_mode_with_no_bytes_is_refused() {
	assert_eq!(BootMode::from_payload(b""), None, "the tag alone is refused, never read as a default");
	assert_eq!(BootMode::from_payload(b"shipping"), Some(BootMode::Shipping));
	assert_eq!(BootMode::from_payload(b"development"), Some(BootMode::Development));
	assert_eq!(BootMode::from_payload(b"test"), Some(BootMode::Test));
	assert_eq!(BootMode::from_payload(b"testing"), None);
	assert!(Policy::from_keys(BootMode::Shipping, None, None, None, None, None).unwrap().enabled, "a shipping image arms by default");
	assert!(!Policy::from_keys(BootMode::Development, None, None, None, None, None).unwrap().enabled, "a development image does not");
	assert!(!Policy::from_keys(BootMode::Test, None, None, None, None, None).unwrap().enabled, "nor does a test boot");
	assert!(Policy::from_keys(BootMode::Test, Some(true), None, None, None, None).unwrap().enabled, "the key decides over the mode");
}

#[test]
fn the_timeout_is_at_least_three_periods_and_the_deadline_under_one() {
	assert_eq!(Policy::from_keys(BootMode::Shipping, None, None, Some(44_999), Some(15_000), None), Err(PolicyError::TimeoutUnderThreePeriods));
	assert!(Policy::from_keys(BootMode::Shipping, None, None, Some(45_000), Some(15_000), None).is_ok(), "three periods exactly");
	assert_eq!(Policy::from_keys(BootMode::Shipping, None, None, None, Some(15_000), Some(15_000)), Err(PolicyError::DeadlineNotUnderPeriod));
	assert_eq!(Policy::from_keys(BootMode::Shipping, None, None, Some(0), None, None), Err(PolicyError::Zero));
	let policy = Policy::from_keys(BootMode::Shipping, None, Some(""), None, None, None).unwrap();
	assert_eq!((policy.timeout_ms, policy.period_ms, policy.deadline_ms, policy.device), (60_000, 15_000, 5_000, None), "the proposed defaults, and an empty device is none");
}

#[test]
fn nothing_is_decided_before_the_first_answered_question() {
	let mut choice = Choice::default();
	assert_eq!(choice.offer("tco", true), Offer::Take);
	assert_eq!(choice.next(&on(None)), None, "a timer found running is its driver's until the first answer");
	choice.start();
	assert_eq!(choice.next(&on(None)), Some(Step::Arm(0)));
}

#[test]
fn the_default_order_arms_the_first_that_accepts_and_disarms_or_feeds_the_rest() {
	let mut choice = Choice::default();
	for (name, can_disarm) in [("i6300esb", true), ("tco", true), ("bmc", false)] {
		assert_eq!(choice.offer(name, can_disarm), Offer::Take);
	}
	choice.start();
	// The TCO comes before the i6300esb in the order, and refuses: No-Reboot stays set.
	let done = settle(&mut choice, &on(None), |name| name != "tco", |_| true);
	assert_eq!(done[0], (String::from("tco"), Step::Arm(1)), "the TCO is tried first");
	assert_eq!(done[1], (String::from("i6300esb"), Step::Arm(0)), "then the next in the order, which arms");
	assert_eq!(choice.armed(), Some(0));
	assert!(done.contains(&(String::from("bmc"), Step::Feed(2))), "a timer that cannot stop is fed");
	assert_eq!(choice.held()[1].role, Role::Refused, "the refusing TCO is left alone");
	assert_eq!(choice.petted().collect::<Vec<_>>(), [0, 2], "the armed timer and the fed one are petted");
}

#[test]
fn a_named_device_is_the_only_one_and_its_absence_or_refusal_arms_nothing() {
	let mut choice = Choice::default();
	choice.offer("tco", true);
	choice.offer("i6300esb", true);
	choice.start();
	let done = settle(&mut choice, &on(Some("i6300esb")), |_| true, |_| true);
	assert_eq!(done[0], (String::from("i6300esb"), Step::Arm(1)), "the named one, whatever the order says");
	assert!(done.contains(&(String::from("tco"), Step::Disarm(0))), "and the other disarmed");
	// ABSENT: nothing is armed in its place.
	let mut absent = Choice::default();
	absent.offer("tco", true);
	absent.start();
	let policy = on(Some("wdat"));
	let done = settle(&mut absent, &policy, |_| true, |_| true);
	assert_eq!(done, [(String::from("tco"), Step::Disarm(0))], "the TCO is disarmed, not armed instead");
	assert!(absent.named_unavailable(&policy), "and the absence is reported");
	// REFUSING: likewise.
	let mut refusing = Choice::default();
	refusing.offer("tco", true);
	refusing.offer("i6300esb", true);
	refusing.start();
	let policy = on(Some("tco"));
	settle(&mut refusing, &policy, |name| name != "tco", |_| true);
	assert_eq!(refusing.armed(), None, "a named device that refuses is not replaced");
	assert!(refusing.named_unavailable(&policy));
}

#[test]
fn with_the_policy_off_everything_is_disarmed_or_fed() {
	let mut choice = Choice::default();
	choice.offer("tco", true);
	choice.offer("bmc", false);
	choice.offer("i6300esb", true);
	choice.start();
	// The i6300esb refuses to stop - locked by a predecessor - and is fed from then on.
	let done = settle(&mut choice, &off(), |_| panic!("nothing is armed with the policy off"), |name| name != "i6300esb");
	assert_eq!(done.len(), 3);
	assert_eq!(choice.armed(), None);
	assert_eq!(choice.held().iter().map(|held| held.role).collect::<Vec<_>>(), [Role::Disarmed, Role::Fed, Role::Fed]);
}

#[test]
fn a_later_provider_is_armed_only_if_it_would_have_been_chosen_and_an_armed_timer_is_never_switched() {
	let mut choice = Choice::default();
	choice.offer("i6300esb", true);
	choice.start();
	settle(&mut choice, &on(None), |_| true, |_| true);
	assert_eq!(choice.armed(), Some(0));
	// A WDAT published later comes first in the order - and the armed i6300esb is not switched for it.
	choice.offer("wdat", true);
	let done = settle(&mut choice, &on(None), |_| true, |_| true);
	assert_eq!(done, [(String::from("wdat"), Step::Disarm(1))], "the later provider is disarmed");
	assert_eq!(choice.armed(), Some(0));
	// With nothing armed a later provider that would have been chosen is armed.
	let mut empty = Choice::default();
	empty.start();
	assert_eq!(empty.next(&on(None)), None);
	empty.offer("tco", true);
	assert_eq!(empty.next(&on(None)), Some(Step::Arm(0)));
}

#[test]
fn a_fifth_provider_and_a_second_of_one_name_are_left_unopened() {
	let mut choice = Choice::default();
	for name in ["wdat", "tco", "i6300esb", "bmc"] {
		assert_eq!(choice.offer(name, true), Offer::Take);
	}
	assert_eq!(choice.offer("other", true), Offer::TooMany, "a fifth is not opened");
	let mut pair = Choice::default();
	assert_eq!(pair.offer("bmc", false), Offer::Take);
	assert_eq!(pair.offer("bmc", false), Offer::SameName, "two interfaces to one BMC are one timer");
	pair.withdraw("bmc");
	assert!(pair.held().is_empty(), "a withdrawn provider goes");
}

#[test]
fn a_pet_follows_only_an_answer_to_this_question_within_its_deadline() {
	let policy = on(None);
	let mut schedule = Schedule::new(&policy, 1_000);
	assert_eq!(schedule.tick(1_000), Tick::Ask(1));
	assert_eq!(schedule.tick(1_500), Tick::Wait(6_000), "the question is in flight until its deadline");
	assert!(!schedule.answered(7, 2_000), "an answer to another question is not a pet");
	assert!(schedule.answered(1, 2_000), "an answer within the deadline is a pet");
	assert!(schedule.answered_once());
	assert_eq!(schedule.tick(2_000), Tick::Wait(16_000), "the next question waits for its period");
	assert_eq!(schedule.tick(16_000), Tick::Ask(2));
	// LATE: past the deadline the question is given up and no pet follows.
	assert_eq!(schedule.tick(21_001), Tick::Wait(31_000));
	assert!(!schedule.answered(2, 21_002), "a late answer pets nothing");
	assert_eq!(schedule.tick(31_000), Tick::Ask(3), "and the next round asks again");
}
