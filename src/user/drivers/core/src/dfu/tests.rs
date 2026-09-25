use super::*;
use crate::descriptor;
use alloc::vec::Vec;

// The shape `usb_ffs.py`'s DFU emulator presents: one DFU-mode interface, its functional descriptor, no endpoints.
fn target(protocol: u8, attributes: u8, transfer: u16) -> Vec<u8> {
	let mut out: Vec<u8> = alloc::vec![9, descriptor::DT_CONFIG, 0, 0, 1, 1, 0, 0x80, 50];
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 0, 0, 0, CLASS_APPLICATION, SUBCLASS_DFU, protocol, 0]);
	let transfer = transfer.to_le_bytes();
	out.extend_from_slice(&[9, DT_DFU_FUNCTIONAL, attributes, 0xff, 0x00, transfer[0], transfer[1], 0x10, 0x01]);
	let total = out.len() as u16;
	out[2..4].copy_from_slice(&total.to_le_bytes());
	out
}

#[test]
fn a_dfu_mode_interface_binds_as_a_download_target() {
	let bound = bind(&target(PROTOCOL_DFU_MODE, ATTR_CAN_DOWNLOAD | ATTR_MANIFESTATION_TOLERANT, 1024)).expect("a DFU target binds");
	assert_eq!((bound.mode, bound.transfer_size, bound.version), (Mode::Dfu, 1024, 0x0110));
	assert!(bound.can_download());
}

#[test]
fn a_runtime_interface_binds_and_says_what_its_dfu_mode_will_do() {
	let bound = bind(&target(PROTOCOL_RUNTIME, ATTR_CAN_DOWNLOAD | ATTR_WILL_DETACH, 1024)).expect("a runtime interface binds");
	assert_eq!(bound.mode, Mode::Runtime);
	assert!(bound.can_download(), "its descriptor describes the DFU mode it detaches into");
	assert!(bound.will_detach(), "and that it leaves the bus by itself");
	let waits = bind(&target(PROTOCOL_RUNTIME, ATTR_CAN_DOWNLOAD, 1024)).unwrap();
	assert!(!waits.will_detach(), "one without the bit waits for the host's reset");
	// `target` writes a detach timeout of 255 ms: it is given that and the five seconds a re-enumeration takes.
	assert_eq!(bound.return_window_ms(), 255 + 5_000);
	let nothing = bind(&target(PROTOCOL_RUNTIME, 0, 1024)).unwrap();
	assert!(!nothing.can_download(), "a device that says it takes no download is not a download target in either mode");
}

#[test]
fn a_device_that_came_back_is_the_one_that_detached_only_where_it_was_and_by_its_serial() {
	let detached = Identity { port: 3, route: 0, serial: Some("LIBERDFU0001") };
	assert!(same_device(&detached, &Identity { port: 3, route: 0, serial: Some("LIBERDFU0001") }), "the same place and the same serial - whatever its product id now");
	assert!(!same_device(&detached, &Identity { port: 3, route: 0, serial: Some("LIBERDFU0002") }), "another serial is another device");
	assert!(!same_device(&detached, &Identity { port: 3, route: 0, serial: None }), "and so is one that no longer says");
	assert!(!same_device(&detached, &Identity { port: 4, route: 0, serial: Some("LIBERDFU0001") }), "another port is another place");
	assert!(!same_device(&detached, &Identity { port: 3, route: 0x12, serial: Some("LIBERDFU0001") }), "and another route below it");
	let unnamed = Identity { port: 3, route: 0, serial: None };
	assert!(same_device(&unnamed, &Identity { port: 3, route: 0, serial: None }), "a device with no serial is followed by its place alone");
	assert!(!same_device(&unnamed, &Identity { port: 3, route: 0, serial: Some("NEW") }), "and a serial it did not have is not it");
}

#[test]
fn a_transfer_size_too_small_to_use_is_refused_and_a_large_one_is_bounded() {
	assert_eq!(bind(&target(PROTOCOL_DFU_MODE, ATTR_CAN_DOWNLOAD, 4)), Err(NotBindable::BadTransferSize));
	assert_eq!(bind(&target(PROTOCOL_DFU_MODE, ATTR_CAN_DOWNLOAD, 0xffff)).unwrap().transfer_size, MAX_TRANSFER);
}

#[test]
fn a_dfu_interface_without_its_functional_descriptor_is_refused() {
	let mut bytes = target(PROTOCOL_DFU_MODE, ATTR_CAN_DOWNLOAD, 1024);
	bytes.truncate(18);
	let total = bytes.len() as u16;
	bytes[2..4].copy_from_slice(&total.to_le_bytes());
	assert_eq!(bind(&bytes), Err(NotBindable::NoFunctionalDescriptor));
}

#[test]
fn a_status_answer_is_six_bytes_and_a_known_state() {
	assert_eq!(status(&[0, 0x10, 0x00, 0x00, STATE_DNLOAD_IDLE, 0]), Some(Status { status: 0, poll_timeout_ms: 16, state: STATE_DNLOAD_IDLE }));
	assert_eq!(status(&[0, 0, 0, 0, 11, 0]), None, "a state past dfuERROR is not one");
	assert_eq!(status(&[0, 0, 0, 0, 2]), None);
}

#[test]
fn a_suffix_is_held_against_the_target_and_its_crc() {
	let image_bytes: Vec<u8> = (0..300u32).map(|n| n as u8).collect();
	let payload = with_suffix(&image_bytes, 0x1d6b, 0x0104);
	assert_eq!(image(&payload, 0x1d6b, 0x0104), Ok(&image_bytes[..]));
	assert_eq!(image(&payload, 0x1d6b, 0x0105), Err(SuffixRefused::WrongDevice));
	let mut corrupt = payload.clone();
	corrupt[10] ^= 1;
	assert_eq!(image(&corrupt, 0x1d6b, 0x0104), Err(SuffixRefused::Crc));
	// No suffix: nothing to validate, and the payload is the image.
	assert_eq!(image(&image_bytes, 0x1d6b, 0x0104), Ok(&image_bytes[..]));
	// A suffix that is all there is.
	assert_eq!(image(&with_suffix(&[], 0x1d6b, 0x0104), 0x1d6b, 0x0104), Err(SuffixRefused::Malformed));
}

#[test]
fn the_suffix_crc_is_the_reference_tools_one() {
	// CRC-32 without the final inversion: the standard check value 0xcbf43926 inverted.
	assert_eq!(suffix_crc(b"123456789"), !0xcbf4_3926);
}
