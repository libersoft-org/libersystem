use super::*;
use alloc::string::String;

#[test]
fn printer_attach_wire_is_stable() {
	let sample = PrinterAttach { version: 7, attachment: 7, device_id: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(PrinterAttach::decode(&bytes).unwrap(), sample);
}
#[test]
fn port_reading_wire_is_stable() {
	let sample = PortReading { bits: 7, cover_open: Some(true), jam: Some(true) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 1, 1, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(PortReading::decode(&bytes).unwrap(), sample);
}
