use super::*;
use alloc::string::String;

#[test]
fn ptp_attach_wire_is_stable() {
	let sample = PtpAttach { version: 7, attachment: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(PtpAttach::decode(&bytes).unwrap(), sample);
}
#[test]
fn ptp_event_container_wire_is_stable() {
	let sample = PtpEventContainer { attachment: 7, bytes: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(PtpEventContainer::decode(&bytes).unwrap(), sample);
}
#[test]
fn ptp_event_wire_is_stable() {
	let sample = PtpEvent::Container(PtpEventContainer { attachment: 7, bytes: alloc::vec![7] });
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(PtpEvent::decode(&bytes).unwrap(), sample);
}
