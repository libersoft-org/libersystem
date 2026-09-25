use super::*;
use crate::hid::fields;
use power_model::schema::{AlarmKind, ChargeState, Tristate, ValueState};

/// THE HARNESS'S UPS: the report descriptor `usb-gadget.sh` gives its `ups` gadget, byte for byte. A power
/// summary with the five status flags, a percentage remaining capacity and a run time in an input report; the
/// capacity mode, three capacities and the battery voltage in a feature report; and a writable
/// DelayBeforeShutdown in a second feature report.
pub const UPS_DESCRIPTOR: &[u8] = &[
	0x05,
	0x84,
	0x09,
	0x04,
	0xa1,
	0x01,
	0x09,
	0x24,
	0xa1,
	0x00,
	0x85,
	0x01,
	0x05,
	0x85,
	0x09,
	0xd0,
	0x09,
	0x44,
	0x09,
	0x45,
	0x09,
	0x42,
	0x09,
	0x4b,
	0x15,
	0x00,
	0x25,
	0x01,
	0x75,
	0x01,
	0x95,
	0x05,
	0x81,
	0x02,
	0x75,
	0x03,
	0x95,
	0x01,
	0x81,
	0x01,
	0x09,
	0x66,
	0x25,
	0x64,
	0x75,
	0x08,
	0x81,
	0x02,
	0x09,
	0x68,
	0x27,
	0xff,
	0xff,
	0x00,
	0x00,
	0x66,
	0x01,
	0x10,
	0x75,
	0x10,
	0x81,
	0x02,
	0x65,
	0x00,
	0x85,
	0x02,
	0x09,
	0x2c,
	0x25,
	0x03,
	0x75,
	0x08,
	0xb1,
	0x02,
	0x09,
	0x67,
	0x09,
	0x83,
	0x09,
	0x29,
	0x25,
	0x64,
	0x95,
	0x03,
	0xb1,
	0x02,
	0x05,
	0x84,
	0x09,
	0x30,
	0x27,
	0xff,
	0xff,
	0x00,
	0x00,
	0x67,
	0x21,
	0xd1,
	0xf0,
	0x00,
	0x55,
	0x05,
	0x75,
	0x10,
	0x95,
	0x01,
	0xb1,
	0x02,
	0x65,
	0x00,
	0x55,
	0x00,
	0x85,
	0x03,
	0x09,
	0x57,
	0x16,
	0xff,
	0xff,
	0x26,
	0xff,
	0x7f,
	0x66,
	0x01,
	0x10,
	0x75,
	0x10,
	0xb1,
	0x02,
	0x65,
	0x00,
	0xc0,
	0xc0,
];

fn reports(flags: u8, capacity: u8, runtime: u16) -> Vec<Report> {
	let runtime = runtime.to_le_bytes();
	alloc::vec![
		Report { kind: ReportKind::Input, report_id: 1, body: alloc::vec![flags, capacity, runtime[0], runtime[1]] },
		// Percent capacity mode, 100 % full and design, a 20 % limit, 13.80 V.
		Report { kind: ReportKind::Feature, report_id: 2, body: alloc::vec![2, 100, 100, 20, 0x64, 0x05] },
		Report { kind: ReportKind::Feature, report_id: 3, body: alloc::vec![0xff, 0xff] },
	]
}

#[test]
fn the_harness_ups_descriptor_maps_every_value_it_carries() {
	let table = fields(UPS_DESCRIPTOR).expect("the descriptor parses");
	assert!(table.uses_ids);
	assert_eq!(table.body_bytes(ReportKind::Input, 1), 4);
	assert_eq!(table.body_bytes(ReportKind::Feature, 2), 6);
	assert_eq!(table.body_bytes(ReportKind::Feature, 3), 2);
	let map = map(&table).expect("a UPS maps");
	assert_eq!(map.ac_present.map(|f| (f.kind, f.report_id, f.bit_offset)), Some((ReportKind::Input, 1, 0)));
	assert_eq!(map.remaining_capacity.map(|f| (f.report_id, f.bit_offset, f.size)), Some((1, 8, 8)));
	assert_eq!(map.voltage.map(|f| (f.kind, f.report_id, f.bit_offset, f.unit_exponent)), Some((ReportKind::Feature, 2, 32, 5)));
	assert_eq!(map.delay_before_shutdown.map(|f| (f.report_id, f.logical_min)), Some((3, -1)));
	assert!(map.outlets.is_empty(), "no outlet collection, so nothing to switch");
	assert_eq!(map.reports(), alloc::vec![(ReportKind::Feature, 2), (ReportKind::Input, 1)]);
}

#[test]
fn a_ups_on_mains_and_on_battery_normalises_through_the_model() {
	let table = fields(UPS_DESCRIPTOR).unwrap();
	let map = map(&table).unwrap();
	// AC present and charging, 80 %, an hour.
	let on_mains = power_model::hid::ups(&decode(&map, &reports(0b0_0011, 80, 3600)));
	assert_eq!((on_mains.online, on_mains.charge), (Tristate::Yes, ChargeState::Charging));
	assert_eq!((on_mains.state_of_charge.state, on_mains.state_of_charge.value), (ValueState::Known, 8000));
	assert_eq!((on_mains.runtime.state, on_mains.runtime.value), (ValueState::Known, 3600));
	assert_eq!((on_mains.voltage.state, on_mains.voltage.value), (ValueState::Known, 13_800_000), "13.80 V, in microvolts");
	assert!(on_mains.controls.schedule_off && on_mains.controls.cancel_off && !on_mains.controls.set_output);
	assert!(on_mains.alarms.iter().any(|alarm| alarm.kind == AlarmKind::OnBattery && alarm.state == Tristate::No));
	// On battery: AC absent, discharging, 79 %, half an hour.
	let on_battery = power_model::hid::ups(&decode(&map, &reports(0b0_0100, 79, 1800)));
	assert_eq!((on_battery.online, on_battery.charge), (Tristate::No, ChargeState::Discharging));
	assert!(on_battery.alarms.iter().any(|alarm| alarm.kind == AlarmKind::OnBattery && alarm.state == Tristate::Yes));
}

#[test]
fn a_value_whose_report_was_not_read_is_absent_not_zero() {
	let table = fields(UPS_DESCRIPTOR).unwrap();
	let map = map(&table).unwrap();
	let ups = decode(&map, &reports(0b0_0011, 80, 3600)[..1]);
	assert!(ups.voltage.is_none() && ups.capacity_mode.is_none(), "the feature report was not read, so its values are not known");
	assert_eq!(ups.status.ac_present, Some(true));
}

#[test]
fn a_control_is_written_into_its_own_bits_and_only_inside_its_range() {
	let table = fields(UPS_DESCRIPTOR).unwrap();
	let map = map(&table).unwrap();
	let delay = map.delay_before_shutdown.unwrap();
	let mut body = alloc::vec![0xff, 0xff];
	assert!(write_control(&mut body, &delay, 60));
	assert_eq!(body, [60, 0]);
	assert!(write_control(&mut body, &delay, -1), "minus one cancels, and the field's range admits it");
	assert_eq!(body, [0xff, 0xff]);
	assert!(!write_control(&mut body, &delay, 40_000), "a delay past the logical maximum is never written");
	assert!(!write_control(&mut body, &delay, -2));
}

#[test]
fn a_keyboard_is_not_a_power_device() {
	let keyboard: &[u8] = &[0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0xc0];
	assert_eq!(map(&fields(keyboard).unwrap()), None);
}

#[test]
fn outlets_are_counted_by_their_own_collection() {
	// Two Outlet collections, each with a writable SwitchOn/Off in a feature report, under a UPS.
	let mut desc: Vec<u8> = alloc::vec![0x05, 0x84, 0x09, 0x04, 0xa1, 0x01, 0x85, 0x05];
	for _ in 0..2 {
		desc.extend_from_slice(&[0x09, 0x20, 0xa1, 0x00, 0x09, 0x6b, 0x15, 0x00, 0x25, 0x01, 0x75, 0x08, 0x95, 0x01, 0xb1, 0x02, 0xc0]);
	}
	desc.push(0xc0);
	let table = fields(&desc).unwrap();
	let map = map(&table).expect("a UPS with two switchable outlets maps");
	assert_eq!(map.outlets.len(), 2);
	assert_eq!((map.outlets[0].bit_offset, map.outlets[1].bit_offset), (0, 8));
	assert_eq!((map.outlets[0].occurrence, map.outlets[1].occurrence), (0, 1));
}
