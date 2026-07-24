use super::*;
use alloc::string::String;

#[test]
fn device_type_wire_is_stable() {
	let sample = DeviceType::Unknown;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(DeviceType::decode(&bytes).unwrap(), sample);
}
#[test]
fn device_entry_wire_is_stable() {
	let sample = DeviceEntry { index: 7, r#type: DeviceType::Unknown, mmio_len: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(DeviceEntry::decode(&bytes).unwrap(), sample);
}
#[test]
fn usb_device_wire_is_stable() {
	let sample = UsbDevice { port: 7, speed: String::from("x"), vendor: 7, product: 7, class: 7, r#type: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 1, 0, 120, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(UsbDevice::decode(&bytes).unwrap(), sample);
}
