use super::*;
use alloc::string::String;

#[test]
fn device_limits_wire_is_stable() {
	let sample = DeviceLimits { version: 7, pending: 7, fragments: 7, message_bytes: 7, assembly_bytes: 7, assemblies: 7, mtu: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 7, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 7, 0];
	assert_eq!(bytes, golden);
	assert_eq!(DeviceLimits::decode(&bytes).unwrap(), sample);
}
#[test]
fn device_open_wire_is_stable() {
	let sample = DeviceOpen { limits: DeviceLimits { version: 7, pending: 7, fragments: 7, message_bytes: 7, assembly_bytes: 7, assemblies: 7, mtu: 7 }, connection_generation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 7, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 7, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(DeviceOpen::decode(&bytes).unwrap(), sample);
}
#[test]
fn device_sim_wire_is_stable() {
	let sample = DeviceSim::Unknown;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(DeviceSim::decode(&bytes).unwrap(), sample);
}
#[test]
fn device_registration_wire_is_stable() {
	let sample = DeviceRegistration::Unknown;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(DeviceRegistration::decode(&bytes).unwrap(), sample);
}
#[test]
fn device_state_wire_is_stable() {
	let sample = DeviceState { manufacturer: String::from("x"), model: String::from("x"), sim: DeviceSim::Unknown, sim_generation: 7, registration: DeviceRegistration::Unknown, operator: Some(String::from("x")), signal_valid: true, rssi_dbm: 7, quality: 7, pin_attempts: Some(7), puk_attempts: Some(7), context_active: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 1, 0, 120, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 120, 1, 7, 0, 0, 0, 7, 1, 7, 1, 7, 1];
	assert_eq!(bytes, golden);
	assert_eq!(DeviceState::decode(&bytes).unwrap(), sample);
}
#[test]
fn ip_family_wire_is_stable() {
	let sample = IpFamily::Ipv4;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[4];
	assert_eq!(bytes, golden);
	assert_eq!(IpFamily::decode(&bytes).unwrap(), sample);
}
#[test]
fn ip_config_wire_is_stable() {
	let sample = IpConfig { family: IpFamily::Ipv4, address: 7, prefix: 7, gateway: Some(7), dns: alloc::vec![7], mtu: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[4, 7, 0, 0, 0, 7, 1, 7, 0, 0, 0, 1, 0, 7, 0, 0, 0, 7, 0];
	assert_eq!(bytes, golden);
	assert_eq!(IpConfig::decode(&bytes).unwrap(), sample);
}
#[test]
fn device_identity_wire_is_stable() {
	let sample = DeviceIdentity { imsi: Some(String::from("x")), iccid: Some(String::from("x")), msisdn: Some(String::from("x")) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 0, 120, 1, 1, 0, 120, 1, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(DeviceIdentity::decode(&bytes).unwrap(), sample);
}
#[test]
fn command_kind_wire_is_stable() {
	let sample = CommandKind::Query;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(CommandKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn command_wire_is_stable() {
	let sample = Command { connection_generation: 7, transaction: 7, kind: CommandKind::Query, sim_generation: 7, context_generation: 7, secret: alloc::vec![7], new_secret: alloc::vec![7], apn: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7, 1, 0, 7, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(Command::decode(&bytes).unwrap(), sample);
}
#[test]
fn command_status_wire_is_stable() {
	let sample = CommandStatus::Done;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(CommandStatus::decode(&bytes).unwrap(), sample);
}
#[test]
fn command_reply_wire_is_stable() {
	let sample = CommandReply { connection_generation: 7, transaction: 7, kind: CommandKind::Query, status: CommandStatus::Done, sim_generation: 7, context_generation: 7, state: Some(DeviceState { manufacturer: String::from("x"), model: String::from("x"), sim: DeviceSim::Unknown, sim_generation: 7, registration: DeviceRegistration::Unknown, operator: Some(String::from("x")), signal_valid: true, rssi_dbm: 7, quality: 7, pin_attempts: Some(7), puk_attempts: Some(7), context_active: true }), config: Some(IpConfig { family: IpFamily::Ipv4, address: 7, prefix: 7, gateway: Some(7), dns: alloc::vec![7], mtu: 7 }), identity: Some(DeviceIdentity { imsi: Some(String::from("x")), iccid: Some(String::from("x")), msisdn: Some(String::from("x")) }) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
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
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
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
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		0,
		120,
		1,
		0,
		120,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		0,
		120,
		1,
		7,
		0,
		0,
		0,
		7,
		1,
		7,
		1,
		7,
		1,
		1,
		4,
		7,
		0,
		0,
		0,
		7,
		1,
		7,
		0,
		0,
		0,
		1,
		0,
		7,
		0,
		0,
		0,
		7,
		0,
		1,
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
	];
	assert_eq!(bytes, golden);
	assert_eq!(CommandReply::decode(&bytes).unwrap(), sample);
}
#[test]
fn indication_wire_is_stable() {
	let sample = Indication { revision: 7, state: DeviceState { manufacturer: String::from("x"), model: String::from("x"), sim: DeviceSim::Unknown, sim_generation: 7, registration: DeviceRegistration::Unknown, operator: Some(String::from("x")), signal_valid: true, rssi_dbm: 7, quality: 7, pin_attempts: Some(7), puk_attempts: Some(7), context_active: true } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 120, 1, 0, 120, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 120, 1, 7, 0, 0, 0, 7, 1, 7, 1, 7, 1];
	assert_eq!(bytes, golden);
	assert_eq!(Indication::decode(&bytes).unwrap(), sample);
}
#[test]
fn datagram_wire_is_stable() {
	let sample = Datagram { context_generation: 7, bytes: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(Datagram::decode(&bytes).unwrap(), sample);
}
#[test]
fn fixture_stats_wire_is_stable() {
	let sample = FixtureStats { commands: 7, pin_attempts: 7, activations: 7, datagrams_in: 7, datagrams_out: 7, echo_replies: 7, dns_answers: 7, last_secret_bytes: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(FixtureStats::decode(&bytes).unwrap(), sample);
}
