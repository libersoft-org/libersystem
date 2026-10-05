use super::*;
use crate::hid::fields;

/// THE HARNESS'S MONITOR: the report descriptor `usb-gadget.sh` gives its `monitor` gadget, byte for byte. A Monitor
/// Control collection with the EDID's first thirty-two bytes as an EDID Information feature report and a 0..100
/// Brightness feature report, and an Ambient Light collection whose input report carries a sixteen-bit illuminance in
/// lux.
#[rustfmt::skip]
pub const MONITOR_DESCRIPTOR: &[u8] = &[
	0x05, 0x80, 0x09, 0x01, 0xa1, 0x01,
	0x85, 0x01, 0x09, 0x02, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x20, 0xb2, 0x02, 0x01,
	0x05, 0x82, 0x85, 0x02, 0x09, 0x10, 0x15, 0x00, 0x25, 0x64, 0x75, 0x08, 0x95, 0x01, 0xb1, 0x02,
	0xc0,
	0x05, 0x20, 0x09, 0x41, 0xa1, 0x01,
	0x85, 0x03, 0x0a, 0xd1, 0x04, 0x15, 0x00, 0x27, 0xff, 0xff, 0x00, 0x00, 0x75, 0x10, 0x95, 0x01, 0x55, 0x00, 0x81, 0x02,
	0xc0,
];

#[test]
fn the_harness_monitor_maps_its_brightness_edid_and_illuminance() {
	let table = fields(MONITOR_DESCRIPTOR).expect("the descriptor parses");
	let map = map(&table).expect("a monitor");
	let brightness = map.brightness.expect("a brightness");
	assert_eq!((brightness.kind, brightness.report_id), (ReportKind::Feature, 2));
	assert_eq!(range(&brightness), (0, 100));
	assert_eq!(map.edid.len(), 32);
	assert!(map.edid.windows(2).all(|pair| pair[0].bit_offset + 8 == pair[1].bit_offset), "in order, a byte apart");
	let light = map.illuminance.expect("an illuminance");
	assert_eq!((light.kind, light.report_id, light.size), (ReportKind::Input, 3, 16));
	assert!(map.is_monitor() && map.is_light_sensor());
	assert_eq!(table.body_bytes(ReportKind::Feature, 1), 32);
}

#[test]
fn a_brightness_reads_and_writes_inside_its_range_only() {
	let table = fields(MONITOR_DESCRIPTOR).unwrap();
	let brightness = map(&table).unwrap().brightness.unwrap();
	let mut body = [0u8; 1];
	assert!(write_level(&mut body, &brightness, 70));
	assert_eq!(level(&brightness, &body), Some(70));
	assert!(!write_level(&mut body, &brightness, 101), "past the logical maximum");
	assert_eq!(body, [70]);
	assert_eq!(level(&brightness, &[200]), None, "a value outside the range is not a level");
	assert_eq!(level(&brightness, &[]), None, "nor is a short body");
}

#[test]
fn the_illuminance_is_scaled_by_its_unit_exponent() {
	let table = fields(MONITOR_DESCRIPTOR).unwrap();
	let light = map(&table).unwrap().illuminance.unwrap();
	assert_eq!(milli_lux(&light, &[0x2c, 0x01]), Some(300_000), "300 lux at exponent zero");
	let mut hundredths = light;
	hundredths.unit_exponent = -2;
	assert_eq!(milli_lux(&hundredths, &[0x2c, 0x01]), Some(3_000), "3 lux in hundredths");
	let mut tens = light;
	tens.unit_exponent = 1;
	assert_eq!(milli_lux(&tens, &[0x0a, 0x00]), Some(100_000));
	let mut signed = light;
	signed.logical_min = -100;
	assert_eq!(milli_lux(&signed, &[0xff, 0xff]), Some(0), "a negative reading is no light");
	let mut nullable = light;
	nullable.null_state = true;
	nullable.logical_max = 1000;
	assert_eq!(milli_lux(&nullable, &[0xff, 0xff]), None, "the null state is no reading");
}

#[test]
fn an_edid_base_block_gives_its_identity_and_anything_else_none() {
	let mut block = [0u8; 128];
	block[..8].copy_from_slice(&EDID_HEADER);
	block[8..10].copy_from_slice(&[0x10, 0xac]);
	block[10..12].copy_from_slice(&[0x50, 0x40]);
	block[12..16].copy_from_slice(&7u32.to_le_bytes());
	assert_eq!(edid_identity(&block), Some(Identity { manufacturer: 0x10ac, product: 0x4050, serial: 7 }));
	assert_eq!(edid_identity(&block[..15]), None);
	block[0] = 1;
	assert_eq!(edid_identity(&block), None, "no header, no EDID");
	let table = fields(MONITOR_DESCRIPTOR).unwrap();
	let map = map(&table).unwrap();
	let mut body = [0u8; 32];
	body[..8].copy_from_slice(&EDID_HEADER);
	assert_eq!(edid_bytes(&map, &body).len(), 32);
	assert_eq!(edid_bytes(&map, &body[..20]).len(), 20, "as many as the body has");
}

#[test]
fn a_sensor_reading_with_a_modifier_is_not_the_reading_and_a_keyboard_is_neither() {
	#[rustfmt::skip]
	let modified: &[u8] = &[
		0x05, 0x20, 0x09, 0x41, 0xa1, 0x01,
		0x0a, 0xd1, 0x24, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02,
		0xc0,
	];
	assert_eq!(map(&fields(modified).unwrap()), None, "only a modifier of the illuminance, no reading");
	#[rustfmt::skip]
	let keyboard: &[u8] = &[
		0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0xc0,
	];
	assert_eq!(map(&fields(keyboard).unwrap()), None);
}

#[test]
fn a_feature_report_may_be_wider_than_an_input_report() {
	#[rustfmt::skip]
	let wide_input: &[u8] = &[0x05, 0x80, 0x09, 0x01, 0xa1, 0x01, 0x09, 0x02, 0x75, 0x08, 0x96, 0x80, 0x00, 0x81, 0x02, 0xc0];
	assert_eq!(fields(wide_input), Err(crate::hid::FieldsRefused::TooLarge), "an input report past sixty-four bytes");
	#[rustfmt::skip]
	let too_wide_feature: &[u8] = &[0x05, 0x80, 0x09, 0x01, 0xa1, 0x01, 0x09, 0x02, 0x75, 0x08, 0x96, 0x01, 0x01, 0xb1, 0x02, 0xc0];
	assert_eq!(fields(too_wide_feature), Err(crate::hid::FieldsRefused::TooLarge), "a feature report past two hundred and fifty-six");
}

#[test]
fn the_input_path_leaves_a_monitor_to_its_class() {
	let layout = crate::hid::parse(MONITOR_DESCRIPTOR);
	assert!(!layout.is_useful(), "keyboard {} pointer {} consumer {} digitizer {} gamepad {}", layout.has_keyboard(), layout.has_pointer(), layout.has_consumer(), layout.has_digitizer(), layout.has_gamepad());
}

#[test]
fn a_sensors_reporting_and_power_states_are_found_and_chosen_by_selector() {
	#[rustfmt::skip]
	let sensor: &[u8] = &[
		0x05, 0x20, 0x09, 0x41, 0xa1, 0x01,
		0x85, 0x01,
		0x0a, 0x16, 0x03, 0x15, 0x00, 0x25, 0x05, 0x75, 0x08, 0x95, 0x01,
		0xa1, 0x02, 0x0a, 0x40, 0x08, 0x0a, 0x41, 0x08, 0x0a, 0x42, 0x08, 0x0a, 0x43, 0x08, 0x0a, 0x44, 0x08, 0x0a, 0x45, 0x08, 0xb1, 0x00, 0xc0,
		0x0a, 0x19, 0x03, 0x15, 0x00, 0x25, 0x05, 0x75, 0x08, 0x95, 0x01,
		0xa1, 0x02, 0x0a, 0x50, 0x08, 0x0a, 0x51, 0x08, 0x0a, 0x52, 0x08, 0x0a, 0x53, 0x08, 0x0a, 0x54, 0x08, 0x0a, 0x55, 0x08, 0xb1, 0x00, 0xc0,
		0x85, 0x02, 0x0a, 0xd1, 0x04, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02,
		0xc0,
	];
	let table = fields(sensor).expect("the sensor's descriptor parses");
	let map = map(&table).expect("a light sensor");
	let reporting = map.reporting_state.expect("its reporting state");
	let power = map.power_state.expect("its power state");
	assert_eq!((reporting.report_id, reporting.bit_offset, power.bit_offset), (1, 0, 8), "two one-byte properties in feature report 1");
	assert_eq!(selector_value(&reporting, ALL_EVENTS), Some(1), "All Events is the second selector");
	assert_eq!(selector_value(&power, FULL_POWER), Some(1), "D0 is the second selector");
	assert_eq!(selector_value(&reporting, PAGE_SENSOR << 16 | 0x0830), None, "a usage before the first selector chooses nothing");
	assert!(map.is_light_sensor() && !map.is_monitor());
	assert!(!table.fields.iter().any(|info| info.array && info.kind != ReportKind::Feature), "an input array is still not entered");
}
