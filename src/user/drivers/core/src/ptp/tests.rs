use super::*;
use crate::descriptor;
use alloc::vec::Vec;

// The shape QEMU's `usb-mtp` and every PTP camera present: one interface, a bulk pair and an event pipe.
fn camera() -> Vec<u8> {
	let mut out: Vec<u8> = alloc::vec![9, descriptor::DT_CONFIG, 0, 0, 1, 1, 0, 0xc0, 1];
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 0, 0, 3, CLASS_STILL_IMAGE, SUBCLASS_STILL_IMAGE, PROTOCOL_PTP, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x81, 0x02, 0x00, 0x02, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x02, 0x02, 0x00, 0x02, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x83, 0x03, 0x40, 0x00, 10]);
	let total = out.len() as u16;
	out[2..4].copy_from_slice(&total.to_le_bytes());
	out
}

#[test]
fn a_still_image_interface_binds_its_three_pipes() {
	let bound = bind(&camera()).expect("a PTP camera binds");
	assert_eq!((bound.bulk_in.address, bound.bulk_out.address, bound.interrupt_in.address), (0x81, 0x02, 0x83));
	assert_eq!(bound.interface, 0);
}

#[test]
fn a_camera_without_its_event_pipe_is_refused_by_name() {
	let mut bytes = camera();
	let cut = bytes.len() - 7;
	bytes.truncate(cut);
	bytes[4 + 9 + 4] = 2;
	let total = bytes.len() as u16;
	bytes[2..4].copy_from_slice(&total.to_le_bytes());
	assert_eq!(bind(&bytes), Err(NotBindable::NoPipes));
}

#[test]
fn another_protocol_is_not_ptp() {
	let mut bytes = camera();
	bytes[9 + 7] = 0x02;
	assert_eq!(bind(&bytes), Err(NotBindable::NoStillImageInterface));
}

#[test]
fn a_cancel_names_the_last_commands_transaction() {
	let mut command: Vec<u8> = Vec::new();
	command.extend_from_slice(&16u32.to_le_bytes());
	command.extend_from_slice(&1u16.to_le_bytes());
	command.extend_from_slice(&0x1009u16.to_le_bytes());
	command.extend_from_slice(&0x1234_5678u32.to_le_bytes());
	command.extend_from_slice(&7u32.to_le_bytes());
	assert_eq!(transaction(&command), 0x1234_5678);
	assert_eq!(transaction(&command[..10]), 0);
	assert_eq!(cancel_request(0x1234_5678), [0x01, 0x40, 0x78, 0x56, 0x34, 0x12]);
}

#[test]
fn a_device_status_is_believed_only_as_far_as_its_length() {
	assert_eq!(device_status(&[4, 0, 0x01, 0x20]), Some(DeviceStatus { code: STATUS_OK, stalled: Vec::new() }));
	assert_eq!(device_status(&[12, 0, 0x19, 0x20, 0x81, 0, 0, 0, 0x02, 0, 0, 0]), Some(DeviceStatus { code: STATUS_DEVICE_BUSY, stalled: alloc::vec![0x81, 0x02] }));
	// A length past what arrived, under its own header, or with half a parameter.
	assert_eq!(device_status(&[8, 0, 0x01, 0x20]), None);
	assert_eq!(device_status(&[2, 0, 0x01, 0x20]), None);
	assert_eq!(device_status(&[6, 0, 0x01, 0x20, 0x81, 0]), None);
	assert_eq!(device_status(&[4]), None);
}
