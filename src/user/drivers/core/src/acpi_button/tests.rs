use super::*;
use alloc::vec;

#[test]
fn a_node_s_class_is_its_id_and_a_battery_is_none_of_them() {
	assert_eq!(class_of([&b"PNP0C0C"[..]]), Some((Class::PowerButton, Source::Node)));
	assert_eq!(class_of([&b"PNP0C0E"[..]]), Some((Class::SleepButton, Source::Node)));
	assert_eq!(class_of([&b"LSFX0D00"[..], &b"PNP0C0D"[..]]), Some((Class::Lid, Source::Node)), "a vendor _HID with the class as a _CID");
	assert_eq!(class_of([&b"PNP0C0A"[..]]), None);
}

#[test]
fn the_kernel_s_fixed_button_rows_are_buttons_whose_presses_come_from_the_manager() {
	assert_eq!(class_of([&b"LNXPWRBN"[..]]), Some((Class::PowerButton, Source::Fixed)));
	assert_eq!(class_of([&b"LNXSLPBN"[..]]), Some((Class::SleepButton, Source::Fixed)));
	assert_eq!(class_of([&b"kernel:pwrbtn"[..], &b"LNXPWRBN"[..]]), Some((Class::PowerButton, Source::Fixed)), "the row's identity is no class");
	assert_eq!(class_of([&b"LNXLID"[..]]), None, "no fixed lid exists");
}

#[test]
fn a_press_is_the_consumer_s_only_when_it_reached_a_consumer() {
	assert_eq!(actor(true, true), Actor::Consumer);
	assert_eq!(actor(true, false), Actor::Driver, "a stream that is full or gone: the press is not lost");
	assert_eq!(actor(false, false), Actor::Driver, "no consumer: the press does what it did before any policy");
	assert_eq!(actor(false, true), Actor::Driver);
}

#[test]
fn a_press_and_a_wake_are_told_apart_and_nothing_else_is_taken_for_either() {
	assert_eq!(event_of(0x80), Event::Pressed);
	assert_eq!(event_of(0x02), Event::Woke, "the wake is never a second request");
	assert_eq!(event_of(0x81), Event::Other(0x81));
}

#[test]
fn a_lid_is_closed_at_zero_and_a_package_is_no_answer() {
	assert_eq!(lid_closed(&Value::Integer(0)), Ok(true));
	assert_eq!(lid_closed(&Value::Integer(1)), Ok(false));
	assert!(lid_closed(&Value::Package(vec![Value::Integer(0)])).is_err());
}
