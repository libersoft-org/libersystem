use super::*;
use alloc::string::String;

#[test]
fn outcome_wire_is_stable() {
	let sample = Outcome::Done;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(Outcome::decode(&bytes).unwrap(), sample);
}
#[test]
fn interface_kind_wire_is_stable() {
	let sample = InterfaceKind::Fifo;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(InterfaceKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn tpm_info_wire_is_stable() {
	let sample = TpmInfo { interface: InterfaceKind::Fifo, manufacturer: String::from("x"), vendor: String::from("x"), firmware_1: 7, firmware_2: 7, sealing: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 0, 120, 1, 0, 120, 7, 0, 0, 0, 7, 0, 0, 0, 1];
	assert_eq!(bytes, golden);
	assert_eq!(TpmInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn info_answer_wire_is_stable() {
	let sample = InfoAnswer { outcome: Outcome::Done, code: 7, info: Some(TpmInfo { interface: InterfaceKind::Fifo, manufacturer: String::from("x"), vendor: String::from("x"), firmware_1: 7, firmware_2: 7, sealing: true }) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 1, 1, 1, 0, 120, 1, 0, 120, 7, 0, 0, 0, 7, 0, 0, 0, 1];
	assert_eq!(bytes, golden);
	assert_eq!(InfoAnswer::decode(&bytes).unwrap(), sample);
}
#[test]
fn bytes_answer_wire_is_stable() {
	let sample = BytesAnswer { outcome: Outcome::Done, code: 7, bytes: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(BytesAnswer::decode(&bytes).unwrap(), sample);
}
#[test]
fn status_wire_is_stable() {
	let sample = Status { outcome: Outcome::Done, code: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Status::decode(&bytes).unwrap(), sample);
}
#[test]
fn quote_wire_is_stable() {
	let sample = Quote { attest: alloc::vec![7], signature_r: alloc::vec![7], signature_s: alloc::vec![7], point_x: alloc::vec![7], point_y: alloc::vec![7], pcr_digest: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 7, 1, 0, 7, 1, 0, 7, 1, 0, 7, 1, 0, 7, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(Quote::decode(&bytes).unwrap(), sample);
}
#[test]
fn quote_answer_wire_is_stable() {
	let sample = QuoteAnswer { outcome: Outcome::Done, code: 7, quote: Some(Quote { attest: alloc::vec![7], signature_r: alloc::vec![7], signature_s: alloc::vec![7], point_x: alloc::vec![7], point_y: alloc::vec![7], pcr_digest: alloc::vec![7] }) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 1, 1, 0, 7, 1, 0, 7, 1, 0, 7, 1, 0, 7, 1, 0, 7, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(QuoteAnswer::decode(&bytes).unwrap(), sample);
}
#[test]
fn grant_wire_is_stable() {
	let sample = Grant::Tpm;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(Grant::decode(&bytes).unwrap(), sample);
}
#[test]
fn device_description_wire_is_stable() {
	let sample = DeviceDescription { outcome: Outcome::Done, code: 7, interface: InterfaceKind::Fifo, manufacturer: String::from("x"), vendor: String::from("x"), firmware_1: 7, firmware_2: 7, owner_auth_set: true, owner_enabled: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 1, 1, 0, 120, 1, 0, 120, 7, 0, 0, 0, 7, 0, 0, 0, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(DeviceDescription::decode(&bytes).unwrap(), sample);
}
