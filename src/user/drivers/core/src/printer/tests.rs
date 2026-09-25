use super::*;
use crate::descriptor;
use alloc::vec::Vec;

fn config(settings: &[(u8, u8, u8, &[(u8, u8)])]) -> Vec<u8> {
	let mut out: Vec<u8> = alloc::vec![9, descriptor::DT_CONFIG, 0, 0, 1, 1, 0, 0xc0, 1];
	for &(interface, alternate, protocol, endpoints) in settings {
		out.extend_from_slice(&[9, descriptor::DT_INTERFACE, interface, alternate, endpoints.len() as u8, CLASS_PRINTER, SUBCLASS_PRINTER, protocol, 0]);
		for &(address, attributes) in endpoints {
			out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, address, attributes, 0x00, 0x02, 0]);
		}
	}
	let total = out.len() as u16;
	out[2..4].copy_from_slice(&total.to_le_bytes());
	out
}

#[test]
fn the_bidirectional_setting_is_preferred_over_the_unidirectional_one() {
	// The shape usb_f_printer presents, and the order a real printer lists its alternates in: 1, then 2.
	let bytes = config(&[(0, 0, PROTOCOL_UNIDIRECTIONAL, &[(0x01, 0x02)]), (0, 1, PROTOCOL_BIDIRECTIONAL, &[(0x01, 0x02), (0x82, 0x02)])]);
	let bound = bind(&bytes).expect("a printer binds");
	assert_eq!((bound.interface, bound.alternate, bound.protocol), (0, 1, PROTOCOL_BIDIRECTIONAL));
	assert_eq!(bound.bulk_out.address, 0x01);
	assert_eq!(bound.bulk_in.map(|e| e.address), Some(0x82));
	assert_eq!(bound.device_id_index(), 0x0001);
}

#[test]
fn a_unidirectional_printer_binds_with_no_in_pipe() {
	let bytes = config(&[(2, 0, PROTOCOL_UNIDIRECTIONAL, &[(0x03, 0x02)])]);
	let bound = bind(&bytes).expect("a unidirectional printer binds");
	assert_eq!(bound.bulk_in, None);
	assert_eq!(bound.device_id_index(), 0x0200);
}

#[test]
fn a_bidirectional_setting_missing_its_in_pipe_falls_back_to_the_unidirectional_one() {
	let bytes = config(&[(0, 0, PROTOCOL_BIDIRECTIONAL, &[(0x01, 0x02)]), (0, 1, PROTOCOL_UNIDIRECTIONAL, &[(0x01, 0x02)])]);
	let bound = bind(&bytes).expect("the usable setting is taken");
	assert_eq!((bound.alternate, bound.protocol), (1, PROTOCOL_UNIDIRECTIONAL));
}

#[test]
fn ieee_1284_4_and_ipp_over_usb_are_not_print_streams() {
	assert_eq!(bind(&config(&[(0, 0, 3, &[(0x01, 0x02), (0x82, 0x02)])])), Err(NotBindable::NoPrinterInterface));
	assert_eq!(bind(&config(&[(0, 0, 4, &[(0x01, 0x02), (0x82, 0x02)])])), Err(NotBindable::NoPrinterInterface));
}

#[test]
fn a_printer_without_a_bulk_out_pipe_is_refused_by_name() {
	// An interrupt OUT where the bulk pipe should be.
	assert_eq!(bind(&config(&[(0, 0, PROTOCOL_UNIDIRECTIONAL, &[(0x01, 0x03)])])), Err(NotBindable::NoBulkPipes));
}

#[test]
fn a_device_id_is_believed_only_as_far_as_its_length_and_the_transfer_agree() {
	let mut id: Vec<u8> = alloc::vec![0, 0];
	id.extend_from_slice(b"MFG:Liber;MDL:Harness;CLS:PRINTER;");
	let length = id.len() as u16;
	id[..2].copy_from_slice(&length.to_be_bytes());
	assert_eq!(device_id_length(&id), Some(id.len()));
	// Trailing bytes past the declared length are not the ID.
	let mut longer = id.clone();
	longer.extend_from_slice(b"garbage");
	assert_eq!(device_id_length(&longer), Some(id.len()));
	// A length that claims more than arrived, or less than itself, is not a length.
	assert_eq!(device_id_length(&id[..10]), None);
	assert_eq!(device_id_length(&[0, 1, b'x']), None);
	assert_eq!(device_id_length(&[0]), None);
}
