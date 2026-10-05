use super::*;
use alloc::vec;

fn package(values: &[u64]) -> Value {
	Value::Package(values.iter().map(|value| Value::Integer(*value)).collect())
}

#[test]
fn bcl_is_normalised_into_its_defaults_and_sorted_deduplicated_levels() {
	let normalised = bcl(&package(&[80, 40, 100, 0, 20, 40, 20, 60, 150])).unwrap();
	assert_eq!(normalised, Levels { ac_default: Some(80), battery_default: Some(40), levels: vec![0, 20, 40, 60, 100] });
	let out_of_range_default = bcl(&package(&[120, 30, 10, 50])).unwrap();
	assert_eq!((out_of_range_default.ac_default, out_of_range_default.battery_default), (None, Some(30)), "a default above 100 is none");
}

#[test]
fn bcl_with_too_few_levels_or_the_wrong_shape_is_refused() {
	assert_eq!(bcl(&package(&[80, 40, 50])), Err(Refusal::TooFewLevels), "one level is no backlight");
	assert_eq!(bcl(&package(&[80, 40, 50, 50])), Err(Refusal::TooFewLevels), "nor is one level twice");
	assert_eq!(bcl(&package(&[80, 40, 101, 200])), Err(Refusal::TooFewLevels), "nor levels all past 100");
	assert_eq!(bcl(&package(&[80])), Err(Refusal::Shape("_BCL")));
	assert_eq!(bcl(&Value::Integer(5)), Err(Refusal::Shape("_BCL")));
	assert_eq!(bcl(&Value::Package(vec![Value::Integer(80), Value::Integer(40), Value::String("ten".into()), Value::Integer(50)])), Err(Refusal::NotInteger("a _BCL level")));
}

#[test]
fn bqc_outside_the_list_snaps_to_the_nearest_level_and_says_so() {
	let levels = [0, 20, 40, 60, 100];
	assert_eq!(bqc(&levels, 40), (40, false));
	assert_eq!(bqc(&levels, 45), (40, true));
	assert_eq!(bqc(&levels, 85), (100, true));
	assert_eq!(bqc(&levels, 1_000), (100, true));
}

#[test]
fn the_whole_notification_map() {
	assert_eq!(notification(0x85), Notification::Hotkey(Hotkey::Cycle));
	assert_eq!(notification(0x86), Notification::Hotkey(Hotkey::Up));
	assert_eq!(notification(0x87), Notification::Hotkey(Hotkey::Down));
	assert_eq!(notification(0x88), Notification::Hotkey(Hotkey::Zero));
	assert_eq!(notification(0x89), Notification::DisplayOff);
	assert_eq!(notification(0x80), Notification::Other(0x80), "the adapter's switch notifications are not the output's");
	assert_eq!(notification(0x8A), Notification::Other(0x8A));
}

#[test]
fn firmware_that_stepped_itself_is_one_step_not_two() {
	assert!(already_moved(Some(40), Some(60), Hotkey::Up));
	assert!(!already_moved(Some(40), Some(40), Hotkey::Up), "unmoved: the key is forwarded");
	assert!(!already_moved(Some(40), Some(20), Hotkey::Up), "moved the other way is no step of this key");
	assert!(already_moved(Some(40), Some(20), Hotkey::Down));
	assert!(already_moved(Some(100), Some(0), Hotkey::Cycle));
	assert!(!already_moved(Some(40), Some(0), Hotkey::Zero), "zero is never the firmware's own");
	assert!(!already_moved(None, Some(60), Hotkey::Up), "nothing set yet");
	assert!(!already_moved(Some(40), None, Hotkey::Up), "no _BQC");
}

#[test]
fn the_sensor_reads_lux_its_curve_sorted_and_its_polling_interval() {
	assert_eq!(ali(&Value::Integer(300)), Ok(Some(300_000)));
	assert_eq!(ali(&Value::Integer(0xFFFF_FFFF)), Ok(None), "not known");
	let curve = Value::Package(vec![package(&[100, 300]), package(&[70, 0]), package(&[150, 1000])]);
	assert_eq!(alr(&curve).unwrap(), vec![Response { adjustment: 70, illuminance: 0 }, Response { adjustment: 100, illuminance: 300 }, Response { adjustment: 150, illuminance: 1000 }]);
	assert_eq!(alr(&Value::Package(vec![package(&[70])])), Err(Refusal::Shape("an _ALR point")));
	assert_eq!(alp_ticks(&Value::Integer(0), 100), Ok(None), "zero: the firmware notifies");
	assert_eq!(alp_ticks(&Value::Integer(5), 100), Ok(Some(50)), "half a second");
	assert_eq!(sensor_notification(0x80), SensorNotification::Illuminance);
	assert_eq!(sensor_notification(0x82), SensorNotification::Response);
	assert_eq!(sensor_notification(0x81), SensorNotification::Other(0x81));
}
