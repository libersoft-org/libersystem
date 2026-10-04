use super::*;

fn policy() -> Policy {
	Policy::new(Settings::default())
}

fn with(settings: Settings) -> Policy {
	Policy::new(settings)
}

#[test]
fn the_defaults_are_the_owners() {
	let settings = Settings::default();
	assert_eq!(settings.lid, LidAction::ScreenOff);
	assert!(!settings.idle_suspends_on_battery);
	assert_eq!(settings.idle_after_seconds, 15 * 60);
	assert_eq!(settings.critical, CriticalAction::Nothing);
	assert_eq!(settings.power_button, ButtonAction::PowerOff);
	assert_eq!(settings.sleep_button, ButtonAction::Suspend);
}

#[test]
fn closing_the_lid_turns_the_screen_off_and_opening_it_on_again() {
	let mut p = policy();
	assert_eq!(p.event(Event::Lid(false)), Action::ScreenOn, "the lid's first state is an edge");
	assert_eq!(p.event(Event::Lid(true)), Action::ScreenOff);
	assert_eq!(p.event(Event::Lid(true)), Action::Nothing, "a lid still closed is no second closing");
	assert_eq!(p.event(Event::Lid(false)), Action::ScreenOn);
	assert_eq!(p.event(Event::Lid(false)), Action::Nothing);
}

#[test]
fn a_relaunched_policy_turns_on_a_screen_an_earlier_instance_turned_off() {
	// THE LID OPENED WHILE NO INSTANCE WAS LISTENING: the new one's first state is open, and it turns the screen on.
	let mut p = policy();
	assert_eq!(p.event(Event::Lid(false)), Action::ScreenOn);
	// And one relaunched with the lid closed turns it off - asked twice, which DisplayService takes as nothing.
	let mut p = policy();
	assert_eq!(p.event(Event::Lid(true)), Action::ScreenOff);
}

#[test]
fn an_external_display_in_use_keeps_the_lid_from_acting() {
	let mut p = policy();
	assert_eq!(p.event(Event::ExternalDisplay(true)), Action::Nothing);
	assert_eq!(p.event(Event::Lid(true)), Action::Nothing, "the screen stays on for the external display");
	assert_eq!(p.event(Event::Lid(false)), Action::ScreenOn, "turning on a screen that is on changes nothing");
	let mut p = with(Settings { lid: LidAction::Suspend, ..Settings::default() });
	assert_eq!(p.event(Event::ExternalDisplay(true)), Action::Nothing);
	assert_eq!(p.event(Event::Lid(true)), Action::Nothing, "an external display in use keeps the machine awake");
	assert_eq!(p.event(Event::Lid(false)), Action::Nothing);
	assert_eq!(p.event(Event::ExternalDisplay(false)), Action::Nothing);
	assert_eq!(p.event(Event::Lid(true)), Action::Suspend(Why::Lid));
}

#[test]
fn a_lid_set_to_suspend_suspends_once_per_closing() {
	let mut p = with(Settings { lid: LidAction::Suspend, ..Settings::default() });
	assert_eq!(p.event(Event::Lid(true)), Action::Suspend(Why::Lid));
	assert_eq!(p.event(Event::Lid(true)), Action::Nothing);
	assert_eq!(p.event(Event::Lid(false)), Action::Nothing);
	assert_eq!(p.event(Event::Lid(true)), Action::Suspend(Why::Lid));
}

#[test]
fn a_lid_set_to_nothing_does_nothing() {
	let mut p = with(Settings { lid: LidAction::Nothing, ..Settings::default() });
	assert_eq!(p.event(Event::Lid(true)), Action::Nothing);
	assert_eq!(p.event(Event::Lid(false)), Action::Nothing);
}

#[test]
fn by_default_idleness_suspends_nothing_on_battery_either() {
	let mut p = policy();
	assert_eq!(p.event(Event::Idle), Action::Nothing);
	assert_eq!(p.event(Event::Power { on_battery: true, critical: false }), Action::Nothing);
	assert_eq!(p.event(Event::Active), Action::Nothing);
	assert_eq!(p.event(Event::Idle), Action::Nothing);
}

#[test]
fn idleness_set_to_suspend_does_so_on_battery_alone_once_per_edge() {
	let mut p = with(Settings { idle_suspends_on_battery: true, ..Settings::default() });
	assert_eq!(p.event(Event::Idle), Action::Nothing, "on line power idleness does nothing");
	assert_eq!(p.event(Event::Active), Action::Nothing);
	assert_eq!(p.event(Event::Power { on_battery: true, critical: false }), Action::Nothing);
	assert_eq!(p.event(Event::Idle), Action::Suspend(Why::Idle));
	assert_eq!(p.event(Event::Idle), Action::Nothing, "an edge, not a level");
	assert_eq!(p.event(Event::Active), Action::Nothing);
	assert_eq!(p.event(Event::Idle), Action::Suspend(Why::Idle));
}

#[test]
fn going_on_battery_while_idle_suspends_once_where_idleness_suspends() {
	let mut p = with(Settings { idle_suspends_on_battery: true, ..Settings::default() });
	assert_eq!(p.event(Event::Idle), Action::Nothing);
	assert_eq!(p.event(Event::Power { on_battery: true, critical: false }), Action::Suspend(Why::Idle));
	assert_eq!(p.event(Event::Power { on_battery: true, critical: false }), Action::Nothing, "still on battery is no new edge");
}

#[test]
fn by_default_a_critical_battery_does_nothing_and_is_told_once() {
	let mut p = policy();
	assert!(!p.critical_told());
	assert_eq!(p.event(Event::Power { on_battery: true, critical: true }), Action::Nothing);
	assert!(p.critical_told(), "the edge was taken, though nothing was done");
	assert_eq!(p.event(Event::Power { on_battery: false, critical: false }), Action::Nothing);
	assert!(!p.critical_told(), "and is taken again at the next time it is critical");
}

#[test]
fn a_critical_battery_set_to_power_off_powers_off_in_order_once() {
	let mut p = with(Settings { critical: CriticalAction::PowerOff, ..Settings::default() });
	assert_eq!(p.event(Event::Power { on_battery: true, critical: true }), Action::PowerOff);
	assert_eq!(p.event(Event::Power { on_battery: true, critical: true }), Action::Nothing, "once until it is no longer critical");
	assert_eq!(p.event(Event::Power { on_battery: false, critical: false }), Action::Nothing);
	assert_eq!(p.event(Event::Power { on_battery: true, critical: true }), Action::PowerOff, "and again at the next time it is");
}

#[test]
fn a_critical_battery_set_to_hibernate_does_once_and_a_refusal_powers_off() {
	let mut p = with(Settings { critical: CriticalAction::Hibernate, ..Settings::default() });
	assert_eq!(p.event(Event::Power { on_battery: true, critical: true }), Action::Hibernate(Why::Critical));
	assert_eq!(p.hibernation_refused(Why::Critical), Action::PowerOff);
	assert_eq!(p.event(Event::Power { on_battery: true, critical: true }), Action::Nothing, "once until it is no longer critical");
	assert_eq!(p.event(Event::Power { on_battery: false, critical: false }), Action::Nothing);
	assert_eq!(p.event(Event::Power { on_battery: true, critical: true }), Action::Hibernate(Why::Critical), "and again at the next time it is");
}

#[test]
fn a_critical_battery_set_to_nothing_does_nothing() {
	let mut p = with(Settings { critical: CriticalAction::Nothing, idle_suspends_on_battery: true, ..Settings::default() });
	assert_eq!(p.event(Event::Idle), Action::Nothing);
	assert_eq!(p.event(Event::Power { on_battery: true, critical: true }), Action::Suspend(Why::Idle), "only the critical action is off - going on battery idle still suspends where idleness does");
}

#[test]
fn the_keys_set_the_settings_and_a_refused_value_keeps_its_default() {
	assert_eq!(Settings::from_keys([None; 6]), (Settings::default(), alloc::vec![]));
	let (settings, refused) = Settings::from_keys([Some("suspend"), Some("on"), Some("120"), Some("hibernate"), Some("suspend"), Some("hibernate")]);
	assert_eq!(settings, Settings { lid: LidAction::Suspend, idle_suspends_on_battery: true, idle_after_seconds: 120, critical: CriticalAction::Hibernate, power_button: ButtonAction::Suspend, sleep_button: ButtonAction::Hibernate });
	assert!(refused.is_empty());
	let (settings, refused) = Settings::from_keys([Some("nothing"), Some("off"), None, Some("power-off"), Some("nothing"), Some("nothing")]);
	assert_eq!(settings, Settings { lid: LidAction::Nothing, critical: CriticalAction::PowerOff, power_button: ButtonAction::Nothing, sleep_button: ButtonAction::Nothing, ..Settings::default() });
	assert!(refused.is_empty());
	let (settings, refused) = Settings::from_keys([Some("sleep"), Some("yes"), Some("0"), Some("halt"), Some("reboot"), Some("power-off")]);
	assert_eq!(settings, Settings::default());
	assert_eq!(refused, alloc::vec!["power.lid", "power.idle-suspend", "power.idle-after-s", "power.critical", "power.button", "power.sleep-button"], "a sleep button that powered off would be a second power button");
	let (_, refused) = Settings::from_keys([None, None, Some("ten"), None, None, None]);
	assert_eq!(refused, alloc::vec!["power.idle-after-s"]);
}

#[test]
fn by_default_a_power_button_powers_off_in_order_and_a_sleep_button_suspends_at_every_press() {
	let mut p = policy();
	assert_eq!(p.event(Event::Pressed(Button::Power)), Action::PowerOff);
	assert_eq!(p.event(Event::Pressed(Button::Power)), Action::PowerOff, "a press is an edge of its own - the second is asked for again");
	assert_eq!(p.event(Event::Pressed(Button::Sleep)), Action::Suspend(Why::SleepButton));
	assert_eq!(p.event(Event::Pressed(Button::Sleep)), Action::Suspend(Why::SleepButton));
	assert_eq!((p.button(Button::Power), p.button(Button::Sleep)), (ButtonAction::PowerOff, ButtonAction::Suspend));
}

#[test]
fn a_button_does_what_it_is_set_to_and_nothing_does_nothing() {
	let mut p = with(Settings { power_button: ButtonAction::Suspend, sleep_button: ButtonAction::Nothing, ..Settings::default() });
	assert_eq!(p.event(Event::Pressed(Button::Power)), Action::Suspend(Why::PowerButton));
	assert_eq!(p.event(Event::Pressed(Button::Sleep)), Action::Nothing);
	let mut p = with(Settings { power_button: ButtonAction::Nothing, ..Settings::default() });
	assert_eq!(p.event(Event::Pressed(Button::Power)), Action::Nothing);
	assert_eq!(p.event(Event::Lid(true)), Action::ScreenOff, "and nothing else is touched by it");
}

#[test]
fn a_button_set_to_hibernate_falls_back_by_what_it_is_for() {
	let mut p = with(Settings { power_button: ButtonAction::Hibernate, sleep_button: ButtonAction::Hibernate, ..Settings::default() });
	assert_eq!(p.event(Event::Pressed(Button::Power)), Action::Hibernate(Why::PowerButton));
	assert_eq!(p.hibernation_refused(Why::PowerButton), Action::PowerOff, "the power button's point is a machine that stops");
	assert_eq!(p.event(Event::Pressed(Button::Sleep)), Action::Hibernate(Why::SleepButton));
	assert_eq!(p.hibernation_refused(Why::SleepButton), Action::Nothing, "a sleep button never powers the machine off");
}

#[test]
fn a_press_changes_no_edge_the_lid_idleness_or_the_battery_keep() {
	let mut p = with(Settings { critical: CriticalAction::PowerOff, ..Settings::default() });
	assert_eq!(p.event(Event::Lid(true)), Action::ScreenOff);
	assert_eq!(p.event(Event::Power { on_battery: true, critical: true }), Action::PowerOff);
	assert_eq!(p.event(Event::Pressed(Button::Sleep)), Action::Suspend(Why::SleepButton));
	assert_eq!(p.event(Event::Lid(true)), Action::Nothing, "the lid is still closed");
	assert_eq!(p.event(Event::Power { on_battery: true, critical: true }), Action::Nothing, "the battery is still critical");
	assert!(p.critical_told());
}
