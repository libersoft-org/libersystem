use super::*;
use alloc::string::String;

#[test]
fn outcome_wire_is_stable() {
	let sample = Outcome::Answered;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(Outcome::decode(&bytes).unwrap(), sample);
}
#[test]
fn status_wire_is_stable() {
	let sample = Status { outcome: Outcome::Answered, cc: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7];
	assert_eq!(bytes, golden);
	assert_eq!(Status::decode(&bytes).unwrap(), sample);
}
#[test]
fn interface_kind_wire_is_stable() {
	let sample = InterfaceKind::Kcs;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(InterfaceKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn device_wire_is_stable() {
	let sample = Device { device_id: 7, device_revision: 7, firmware_major: 7, firmware_minor: 7, ipmi_major: 7, ipmi_minor: 7, manufacturer: 7, product: 7, support: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 7, 7, 7, 7, 7, 0, 0, 0, 7, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(Device::decode(&bytes).unwrap(), sample);
}
#[test]
fn binding_info_wire_is_stable() {
	let sample = BindingInfo { name: String::from("x"), guid: true, administrable: true, interface: InterfaceKind::Kcs, binding: String::from("x"), available: true, device: Device { device_id: 7, device_revision: 7, firmware_major: 7, firmware_minor: 7, ipmi_major: 7, ipmi_minor: 7, manufacturer: 7, product: 7, support: 7 }, sensor_count: 7, sdr_truncated: true, watchdog: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 1, 1, 1, 1, 0, 120, 1, 7, 7, 7, 7, 7, 7, 7, 0, 0, 0, 7, 0, 7, 7, 0, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(BindingInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn reading_state_wire_is_stable() {
	let sample = ReadingState::Known;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(ReadingState::decode(&bytes).unwrap(), sample);
}
#[test]
fn record_kind_wire_is_stable() {
	let sample = RecordKind::Full;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(RecordKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn sensor_wire_is_stable() {
	let sample = Sensor { number: 7, lun: 7, name: String::from("x"), kind: RecordKind::Full, sensor_type: 7, event_type: 7, entity_id: 7, entity_instance: 7, units: 7, base_unit: 7, modifier_unit: 7, state: ReadingState::Known, raw: 7, value: 7, bits: 7, upper_non_recoverable: Some(7), upper_critical: Some(7), upper_non_critical: Some(7), lower_non_critical: Some(7), lower_critical: Some(7), lower_non_recoverable: Some(7) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		7,
		7,
		1,
		0,
		120,
		1,
		7,
		7,
		7,
		7,
		7,
		7,
		7,
		1,
		7,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
	];
	assert_eq!(bytes, golden);
	assert_eq!(Sensor::decode(&bytes).unwrap(), sample);
}
#[test]
fn sensors_wire_is_stable() {
	let sample = Sensors { status: Status { outcome: Outcome::Answered, cc: 7 }, sensors: alloc::vec![Sensor { number: 7, lun: 7, name: String::from("x"), kind: RecordKind::Full, sensor_type: 7, event_type: 7, entity_id: 7, entity_instance: 7, units: 7, base_unit: 7, modifier_unit: 7, state: ReadingState::Known, raw: 7, value: 7, bits: 7, upper_non_recoverable: Some(7), upper_critical: Some(7), upper_non_critical: Some(7), lower_non_critical: Some(7), lower_critical: Some(7), lower_non_recoverable: Some(7) }], truncated: true, refused: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		1,
		7,
		1,
		0,
		7,
		7,
		1,
		0,
		120,
		1,
		7,
		7,
		7,
		7,
		7,
		7,
		7,
		1,
		7,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
	];
	assert_eq!(bytes, golden);
	assert_eq!(Sensors::decode(&bytes).unwrap(), sample);
}
#[test]
fn sel_info_wire_is_stable() {
	let sample = SelInfo { status: Status { outcome: Outcome::Answered, cc: 7 }, version: 7, entries: 7, free: 7, last_addition: 7, last_erase: 7, overflow: true, reserve: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 7, 7, 0, 7, 0, 7, 0, 0, 0, 7, 0, 0, 0, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(SelInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn ours_wire_is_stable() {
	let sample = Ours::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(Ours::decode(&bytes).unwrap(), sample);
}
#[test]
fn sel_entry_wire_is_stable() {
	let sample = SelEntry { id: 7, record_type: 7, timestamp: 7, generator: 7, sensor_type: 7, sensor: 7, event_type: 7, deassertion: true, data: alloc::vec![7], ours: Ours::None };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 7, 7, 0, 0, 0, 7, 0, 7, 7, 7, 1, 1, 0, 7, 0];
	assert_eq!(bytes, golden);
	assert_eq!(SelEntry::decode(&bytes).unwrap(), sample);
}
#[test]
fn sel_refused_wire_is_stable() {
	let sample = SelRefused { id: 7, record_type: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(SelRefused::decode(&bytes).unwrap(), sample);
}
#[test]
fn sel_page_wire_is_stable() {
	let sample = SelPage { status: Status { outcome: Outcome::Answered, cc: 7 }, entries: alloc::vec![SelEntry { id: 7, record_type: 7, timestamp: 7, generator: 7, sensor_type: 7, sensor: 7, event_type: 7, deassertion: true, data: alloc::vec![7], ours: Ours::None }], next: 7, refused: alloc::vec![SelRefused { id: 7, record_type: 7 }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 1, 0, 7, 0, 7, 7, 0, 0, 0, 7, 0, 7, 7, 7, 1, 1, 0, 7, 0, 7, 0, 1, 0, 7, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(SelPage::decode(&bytes).unwrap(), sample);
}
#[test]
fn fru_device_wire_is_stable() {
	let sample = FruDevice { device: 7, name: String::from("x"), status: Status { outcome: Outcome::Answered, cc: 7 }, chassis_type: 7, chassis_part: Some(String::from("x")), chassis_serial: Some(String::from("x")), manufactured: 7, board_manufacturer: Some(String::from("x")), board_product: Some(String::from("x")), board_serial: Some(String::from("x")), board_part: Some(String::from("x")), product_manufacturer: Some(String::from("x")), product_name: Some(String::from("x")), product_part: Some(String::from("x")), product_version: Some(String::from("x")), product_serial: Some(String::from("x")), product_asset: Some(String::from("x")), multirecord: true, refused: alloc::vec![String::from("x")] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		7,
		1,
		0,
		120,
		1,
		7,
		7,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		7,
		0,
		0,
		0,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		1,
		0,
		120,
	];
	assert_eq!(bytes, golden);
	assert_eq!(FruDevice::decode(&bytes).unwrap(), sample);
}
#[test]
fn fru_wire_is_stable() {
	let sample = Fru { status: Status { outcome: Outcome::Answered, cc: 7 }, devices: alloc::vec![FruDevice { device: 7, name: String::from("x"), status: Status { outcome: Outcome::Answered, cc: 7 }, chassis_type: 7, chassis_part: Some(String::from("x")), chassis_serial: Some(String::from("x")), manufactured: 7, board_manufacturer: Some(String::from("x")), board_product: Some(String::from("x")), board_serial: Some(String::from("x")), board_part: Some(String::from("x")), product_manufacturer: Some(String::from("x")), product_name: Some(String::from("x")), product_part: Some(String::from("x")), product_version: Some(String::from("x")), product_serial: Some(String::from("x")), product_asset: Some(String::from("x")), multirecord: true, refused: alloc::vec![String::from("x")] }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		1,
		7,
		1,
		0,
		7,
		1,
		0,
		120,
		1,
		7,
		7,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		7,
		0,
		0,
		0,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		120,
		1,
		1,
		0,
		1,
		0,
		120,
	];
	assert_eq!(bytes, golden);
	assert_eq!(Fru::decode(&bytes).unwrap(), sample);
}
#[test]
fn chassis_wire_is_stable() {
	let sample = Chassis { status: Status { outcome: Outcome::Answered, cc: 7 }, power_on: true, overload: true, interlock: true, power_fault: true, control_fault: true, restore_policy: 7, last_event: 7, intrusion: true, drive_fault: true, cooling_fault: true, identify: Some(7) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 1, 1, 1, 1, 1, 7, 7, 1, 1, 1, 1, 7];
	assert_eq!(bytes, golden);
	assert_eq!(Chassis::decode(&bytes).unwrap(), sample);
}
#[test]
fn lan_user_wire_is_stable() {
	let sample = LanUser { id: 7, name: String::from("x"), enabled: true, privilege: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 1, 0, 120, 1, 7];
	assert_eq!(bytes, golden);
	assert_eq!(LanUser::decode(&bytes).unwrap(), sample);
}
#[test]
fn lan_channel_wire_is_stable() {
	let sample = LanChannel { channel: 7, source: Some(7), address: alloc::vec![7], mask: alloc::vec![7], gateway: alloc::vec![7], mac: alloc::vec![7], vlan: Some(7), users: alloc::vec![LanUser { id: 7, name: String::from("x"), enabled: true, privilege: 7 }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 1, 7, 1, 0, 7, 1, 0, 7, 1, 0, 7, 1, 0, 7, 1, 7, 0, 1, 0, 7, 1, 0, 120, 1, 7];
	assert_eq!(bytes, golden);
	assert_eq!(LanChannel::decode(&bytes).unwrap(), sample);
}
#[test]
fn lan_wire_is_stable() {
	let sample = Lan { status: Status { outcome: Outcome::Answered, cc: 7 }, channels: alloc::vec![LanChannel { channel: 7, source: Some(7), address: alloc::vec![7], mask: alloc::vec![7], gateway: alloc::vec![7], mac: alloc::vec![7], vlan: Some(7), users: alloc::vec![LanUser { id: 7, name: String::from("x"), enabled: true, privilege: 7 }] }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 1, 0, 7, 1, 7, 1, 0, 7, 1, 0, 7, 1, 0, 7, 1, 0, 7, 1, 7, 0, 1, 0, 7, 1, 0, 120, 1, 7];
	assert_eq!(bytes, golden);
	assert_eq!(Lan::decode(&bytes).unwrap(), sample);
}
#[test]
fn system_event_kind_wire_is_stable() {
	let sample = SystemEventKind::BootCompleted;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(SystemEventKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn change_kind_wire_is_stable() {
	let sample = ChangeKind::Available;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(ChangeKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn change_wire_is_stable() {
	let sample = Change { kind: ChangeKind::Available, name: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(Change::decode(&bytes).unwrap(), sample);
}
#[test]
fn bmc_summary_wire_is_stable() {
	let sample = BmcSummary { name: String::from("x"), administrable: true, available: true, device: Device { device_id: 7, device_revision: 7, firmware_major: 7, firmware_minor: 7, ipmi_major: 7, ipmi_minor: 7, manufacturer: 7, product: 7, support: 7 }, bindings: alloc::vec![BindingInfo { name: String::from("x"), guid: true, administrable: true, interface: InterfaceKind::Kcs, binding: String::from("x"), available: true, device: Device { device_id: 7, device_revision: 7, firmware_major: 7, firmware_minor: 7, ipmi_major: 7, ipmi_minor: 7, manufacturer: 7, product: 7, support: 7 }, sensor_count: 7, sdr_truncated: true, watchdog: true }], paired: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 1, 1, 7, 7, 7, 7, 7, 7, 7, 0, 0, 0, 7, 0, 7, 1, 0, 1, 0, 120, 1, 1, 1, 1, 0, 120, 1, 7, 7, 7, 7, 7, 7, 7, 0, 0, 0, 7, 0, 7, 7, 0, 1, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(BmcSummary::decode(&bytes).unwrap(), sample);
}
