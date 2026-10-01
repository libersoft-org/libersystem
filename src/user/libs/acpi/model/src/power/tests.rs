use super::*;
use alloc::string::String;
use alloc::vec;

fn resource(path: &str, order: u16) -> Resource {
	Resource { path: String::from(path), order }
}

fn paths(names: &[&str]) -> Vec<String> {
	names.iter().map(|name| String::from(*name)).collect()
}

// TWO DEVICES SHARING ONE RESOURCE: on with the first, off only with the last.
#[test]
fn a_shared_resource_is_on_while_any_device_holds_it_and_off_with_the_last() {
	let mut resources = Resources::new();
	let shared = resource("\\_SB_.PWR0", 0);
	let first = resources.transition("\\_SB_.TPAD", DState::D0, &[shared.clone()]);
	assert_eq!(first, Plan { on: paths(&["\\_SB_.PWR0"]), method: Some(b"_PS0"), off: vec![] });
	let second = resources.transition("\\_SB_.TSCR", DState::D0, &[shared.clone()]);
	assert!(second.on.is_empty(), "already on: the second holder switches nothing");
	assert_eq!(second.method, Some(b"_PS0"), "but the device itself is still asked into D0");
	assert_eq!(resources.count("\\_SB_.PWR0"), 2);
	let down = resources.transition("\\_SB_.TPAD", DState::D3Cold, &[]);
	assert_eq!(down, Plan { on: vec![], method: Some(b"_PS3"), off: vec![] }, "still held by the other device");
	let last = resources.transition("\\_SB_.TSCR", DState::D3Cold, &[]);
	assert_eq!(last.off, paths(&["\\_SB_.PWR0"]), "off with the last holder");
	assert_eq!(resources.count("\\_SB_.PWR0"), 0);
	let again = resources.transition("\\_SB_.TPAD", DState::D0, &[shared]);
	assert_eq!(again.on, paths(&["\\_SB_.PWR0"]), "and on again with the next first holder");
}

// THE ORDER: on in ascending `resource_order`, off in descending, and a resource both states name never switched.
#[test]
fn resources_switch_in_their_order_and_one_both_states_name_stays_on() {
	let mut resources = Resources::new();
	let (low, high, both) = (resource("\\_SB_.LOW_", 1), resource("\\_SB_.HIGH", 5), resource("\\_SB_.BOTH", 3));
	let up = resources.transition("\\_SB_.DEV0", DState::D0, &[high.clone(), both.clone(), low.clone()]);
	assert_eq!(up.on, paths(&["\\_SB_.LOW_", "\\_SB_.BOTH", "\\_SB_.HIGH"]));
	let hot = resources.transition("\\_SB_.DEV0", DState::D3Hot, &[both.clone()]);
	assert!(hot.on.is_empty(), "the resource D3hot keeps was never off");
	assert_eq!(hot.off, paths(&["\\_SB_.HIGH", "\\_SB_.LOW_"]), "the rest off, highest order first");
	assert_eq!(hot.method, Some(b"_PS3"));
	assert_eq!(resources.count("\\_SB_.BOTH"), 1);
	assert_eq!(resources.state("\\_SB_.DEV0"), Some(DState::D3Hot));
}

// THE SAME STATE ASKED TWICE runs nothing and changes no count; a resource named twice is held once.
#[test]
fn the_same_state_asked_again_runs_nothing() {
	let mut resources = Resources::new();
	let power = resource("\\_SB_.PWR0", 0);
	let first = resources.transition("\\_SB_.DEV0", DState::D0, &[power.clone(), power.clone()]);
	assert_eq!(first.on, paths(&["\\_SB_.PWR0"]));
	assert_eq!(resources.count("\\_SB_.PWR0"), 1, "named twice, held once");
	let again = resources.transition("\\_SB_.DEV0", DState::D0, &[power.clone()]);
	assert_eq!(again, Plan { on: vec![], method: None, off: vec![] });
	assert_eq!(resources.count("\\_SB_.PWR0"), 1, "one holder, however often it asked");
}

// A HOLDER GONE lets go of what it held, and a wake node's hold is a holder like a device.
#[test]
fn a_holder_forgotten_lets_its_resources_go_and_a_wake_hold_shares_the_count() {
	let mut resources = Resources::new();
	let power = resource("\\_SB_.PWR0", 0);
	resources.transition("\\_SB_.DEV0", DState::D0, &[power.clone()]);
	let (on, off) = resources.hold("wake \\_SB_.LID0", &[power.clone(), resource("\\_SB_.PWAK", 2)]);
	assert_eq!(on, paths(&["\\_SB_.PWAK"]), "the shared one is on already");
	assert!(off.is_empty());
	assert_eq!(resources.forget("\\_SB_.DEV0"), Vec::<String>::new(), "the wake hold still holds the shared one");
	assert_eq!(resources.state("\\_SB_.DEV0"), None);
	assert_eq!(resources.forget("wake \\_SB_.LID0"), paths(&["\\_SB_.PWAK", "\\_SB_.PWR0"]), "both off, highest order first");
	assert!(resources.forget("wake \\_SB_.LID0").is_empty(), "and twice is nothing");
	assert_eq!(resources.count("\\_SB_.PWR0"), 0);
}

// THE SLEEP'S STATE: D3cold unless the device wakes the machine, and then `_SxW` bounded by `_SxD`.
#[test]
fn a_sleep_puts_a_device_in_the_deepest_state_it_may_and_a_wake_device_where_it_still_wakes() {
	assert_eq!(sleep_state(None, None, false), DState::D3Cold);
	assert_eq!(sleep_state(Some(2), Some(3), false), DState::D3Cold, "no wake: the deepest, which _SxD never forbids");
	assert_eq!(sleep_state(None, Some(3), true), DState::D3Hot);
	assert_eq!(sleep_state(None, Some(4), true), DState::D3Cold);
	assert_eq!(sleep_state(Some(2), Some(1), true), DState::D2, "never shallower than _SxD");
	assert_eq!(sleep_state(Some(1), None, true), DState::D1, "no _SxW: _SxD");
	assert_eq!(sleep_state(None, None, true), DState::D0, "neither: D0");
	assert_eq!(sleep_state(Some(9), Some(7), true), DState::D0, "values that are no state are read as absent");
	assert_eq!(sleep_state(Some(4), None, true), DState::D0, "_SxD has no D3cold");
}

#[test]
fn the_sleep_objects_are_named_for_the_target() {
	assert_eq!(sleep_objects(0), Some((None, *b"_S0W")), "suspend to idle has no _S0D");
	assert_eq!(sleep_objects(3), Some((Some(*b"_S3D"), *b"_S3W")));
	assert_eq!(sleep_objects(4), Some((Some(*b"_S4D"), *b"_S4W")));
	assert_eq!(sleep_objects(5), None);
}

#[test]
fn the_wire_numbers_are_the_states() {
	for value in 0..=4u8 {
		assert_eq!(DState::from_u8(value).map(DState::as_u8), Some(value));
	}
	assert_eq!(DState::from_u8(5), None);
	assert_eq!(DState::D3Cold.method(), b"_PS3");
	assert_eq!(DState::D3Cold.resources(), None);
	assert_eq!(DState::D3Hot.resources(), Some(b"_PR3"));
}
