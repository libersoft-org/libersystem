use super::*;

fn policy() -> Policy {
	Policy::new(Settings::default())
}

#[test]
fn closing_the_lid_suspends_once_per_closing_and_not_with_an_external_display() {
	let mut p = policy();
	assert_eq!(p.event(Event::Lid(true)), Action::Suspend(Why::Lid));
	assert_eq!(p.event(Event::Lid(true)), Action::Nothing, "a lid still closed is no second closing");
	assert_eq!(p.event(Event::Lid(false)), Action::Nothing);
	assert_eq!(p.event(Event::ExternalDisplay(true)), Action::Nothing);
	assert_eq!(p.event(Event::Lid(true)), Action::Nothing, "an external display in use keeps the machine awake");
	assert_eq!(p.event(Event::Lid(false)), Action::Nothing);
	assert_eq!(p.event(Event::ExternalDisplay(false)), Action::Nothing);
	assert_eq!(p.event(Event::Lid(true)), Action::Suspend(Why::Lid));
}

#[test]
fn idleness_suspends_on_battery_alone_once_per_edge() {
	let mut p = policy();
	assert_eq!(p.event(Event::Idle), Action::Nothing, "on line power idleness does nothing");
	assert_eq!(p.event(Event::Active), Action::Nothing);
	assert_eq!(p.event(Event::Power { on_battery: true, critical: false }), Action::Nothing);
	assert_eq!(p.event(Event::Idle), Action::Suspend(Why::Idle));
	assert_eq!(p.event(Event::Idle), Action::Nothing, "an edge, not a level");
	assert_eq!(p.event(Event::Active), Action::Nothing);
	assert_eq!(p.event(Event::Idle), Action::Suspend(Why::Idle));
}

#[test]
fn going_on_battery_while_idle_suspends_once() {
	let mut p = policy();
	assert_eq!(p.event(Event::Idle), Action::Nothing);
	assert_eq!(p.event(Event::Power { on_battery: true, critical: false }), Action::Suspend(Why::Idle));
	assert_eq!(p.event(Event::Power { on_battery: true, critical: false }), Action::Nothing, "still on battery is no new edge");
}

#[test]
fn a_critical_battery_hibernates_once_and_a_refusal_powers_off() {
	let mut p = policy();
	assert_eq!(p.event(Event::Power { on_battery: true, critical: true }), Action::Hibernate);
	assert_eq!(p.hibernation_refused(), Action::PowerOff);
	assert_eq!(p.event(Event::Power { on_battery: true, critical: true }), Action::Nothing, "once until it is no longer critical");
	assert_eq!(p.event(Event::Power { on_battery: false, critical: false }), Action::Nothing);
	assert_eq!(p.event(Event::Power { on_battery: true, critical: true }), Action::Hibernate, "and again at the next time it is");
}

#[test]
fn a_setting_turned_off_turns_its_action_off() {
	let mut p = Policy::new(Settings { lid_suspends: false, idle_suspends_on_battery: false, idle_after_seconds: 60 });
	assert_eq!(p.event(Event::Lid(true)), Action::Nothing);
	assert_eq!(p.event(Event::Power { on_battery: true, critical: false }), Action::Nothing);
	assert_eq!(p.event(Event::Idle), Action::Nothing);
}
