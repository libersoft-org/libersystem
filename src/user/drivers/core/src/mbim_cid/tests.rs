use super::*;
use crate::descriptor;
use alloc::vec::Vec;

// The function the harness's MBIM emulator presents: a control interface with the header, union and MBIM
// functional descriptors and a notification pipe; a data interface with an empty alternate zero and the bulk
// pair on alternate one.
fn modem() -> Vec<u8> {
	let mut out: Vec<u8> = alloc::vec![9, descriptor::DT_CONFIG, 0, 0, 2, 1, 0, 0xc0, 1];
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 0, 0, 1, CLASS_COMMUNICATIONS, SUBCLASS_MBIM, 0, 0]);
	out.extend_from_slice(&[5, DT_CS_INTERFACE, 0x00, 0x10, 0x01]);
	out.extend_from_slice(&[5, DT_CS_INTERFACE, FN_UNION, 0, 1]);
	out.extend_from_slice(&[12, DT_CS_INTERFACE, FN_MBIM, 0x00, 0x01, 0x00, 0x10, 16, 128, 0xdc, 0x05, 0x20]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x85, 0x03, 0x10, 0x00, 9]);
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 1, 0, 0, CLASS_CDC_DATA, 0, PROTOCOL_NTB, 0]);
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 1, 1, 2, CLASS_CDC_DATA, 0, PROTOCOL_NTB, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x81, 0x02, 0x00, 0x02, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x02, 0x02, 0x00, 0x02, 0]);
	let total = out.len() as u16;
	out[2..4].copy_from_slice(&total.to_le_bytes());
	out
}

#[test]
fn an_mbim_function_binds_its_control_and_the_data_setting_its_union_names() {
	let bound = bind(&modem()).expect("an MBIM function binds");
	assert_eq!((bound.control_interface, bound.data_interface, bound.data_alternate), (0, 1, 1));
	assert_eq!((bound.notify.address, bound.bulk_in.address, bound.bulk_out.address), (0x85, 0x81, 0x02));
	assert_eq!((bound.max_control, bound.max_segment), (4096, 1500));
}

#[test]
fn a_union_naming_an_interface_without_the_bulk_pair_is_incomplete() {
	let mut bytes = modem();
	// The union's subordinate becomes interface 7, which is not there.
	let at = bytes.windows(3).position(|window| window == [DT_CS_INTERFACE, FN_UNION, 0]).unwrap();
	bytes[at + 3] = 7;
	assert_eq!(bind(&bytes), Err(NotBindable::Incomplete));
}

#[test]
fn a_connect_set_is_sixty_fixed_bytes_and_the_apn_after_them() {
	let info = connect_set(0, true, "internet");
	assert_eq!(&info[..8], &[0, 0, 0, 0, 1, 0, 0, 0]);
	assert_eq!(u32::from_le_bytes(info[8..12].try_into().unwrap()), 60, "the APN's offset is past the fixed part");
	assert_eq!(u32::from_le_bytes(info[12..16].try_into().unwrap()), 16, "eight UTF-16 units");
	assert_eq!(u32::from_le_bytes(info[40..44].try_into().unwrap()), IP_TYPE_IPV4);
	assert_eq!(&info[44..60], &CONTEXT_INTERNET);
	assert_eq!(info.len(), 76);
}

fn ip_configuration(prefix: u32, available: u32) -> Vec<u8> {
	let mut info = alloc::vec![0u8; 60];
	let put = |info: &mut Vec<u8>, at: usize, value: u32| info[at..at + 4].copy_from_slice(&value.to_le_bytes());
	put(&mut info, 4, available);
	put(&mut info, 12, 1);
	put(&mut info, 16, 60);
	put(&mut info, 28, 68);
	put(&mut info, 36, 1);
	put(&mut info, 40, 72);
	put(&mut info, 52, 1400);
	info.extend_from_slice(&prefix.to_le_bytes());
	info.extend_from_slice(&[10, 64, 0, 2]);
	info.extend_from_slice(&[10, 64, 0, 1]);
	info.extend_from_slice(&[10, 64, 0, 1]);
	info
}

#[test]
fn an_ipv4_configuration_is_read_by_its_own_offsets() {
	let config = ipv4_configuration(&ip_configuration(30, 0x0f)).expect("it reads").expect("it has IPv4");
	assert_eq!(config, Ipv4 { address: u32::from_be_bytes([10, 64, 0, 2]), prefix: 30, gateway: Some(u32::from_be_bytes([10, 64, 0, 1])), dns: alloc::vec![u32::from_be_bytes([10, 64, 0, 1])], mtu: 1400 });
	assert_eq!(ipv4_configuration(&ip_configuration(30, 0)), Ok(None), "no IPv4 address is an IPv6-only context");
	assert_eq!(ipv4_configuration(&ip_configuration(40, 0x0f)), Err(InfoRefused::Field), "a prefix past thirty-two is not one");
	let mut past = ip_configuration(30, 0x0f);
	past[16..20].copy_from_slice(&200u32.to_le_bytes());
	assert_eq!(ipv4_configuration(&past), Err(InfoRefused::Field), "an address offset past the buffer");
}

#[test]
fn strings_are_bounded_by_the_buffer_and_by_their_contract() {
	let info = InfoWriter::new(64).u32(0).u32(0).u32(0).u32(0).u32(0).u32(0).u32(0).u32(0).u32(0).u32(0).string("490154203237518").string("fw 1.0").string("LiberSystem harness modem").finish();
	let caps = device_caps(&info).expect("device caps read");
	assert_eq!(caps.device_id.as_deref(), Some("490154203237518"));
	assert_eq!(caps.hardware.as_deref(), Some("LiberSystem harness modem"));
	let mut odd = info.clone();
	odd[44..48].copy_from_slice(&3u32.to_le_bytes());
	assert_eq!(device_caps(&odd), Err(InfoRefused::Field), "an odd UTF-16 size");
	let long = InfoWriter::new(64).u32(0).u32(0).u32(0).u32(0).u32(0).u32(0).u32(0).u32(0).u32(0).u32(0).string(&"x".repeat(40)).string("").string("").finish();
	assert_eq!(device_caps(&long), Err(InfoRefused::Field), "a string past the contract's bound is refused, not cut");
}

#[test]
fn a_done_and_an_indication_carry_their_service_cid_and_buffer() {
	let body = command(&BASIC_CONNECT, CID_SIGNAL_STATE, false, &[]);
	assert_eq!(body.len(), 28);
	let mut answered: Vec<u8> = BASIC_CONNECT.to_vec();
	answered.extend_from_slice(&CID_SIGNAL_STATE.to_le_bytes());
	answered.extend_from_slice(&STATUS_SUCCESS.to_le_bytes());
	answered.extend_from_slice(&20u32.to_le_bytes());
	answered.extend_from_slice(&[20, 0, 0, 0, 99, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
	let read = done(&answered).expect("a done reads");
	assert_eq!((read.cid, read.status, signal_dbm(&read.info)), (CID_SIGNAL_STATE, 0, Ok(Some(-73))));
	assert_eq!(done(&answered[..40]), Err(InfoRefused::Short), "a buffer length past what arrived");
	let mut indication: Vec<u8> = BASIC_CONNECT.to_vec();
	indication.extend_from_slice(&CID_REGISTER_STATE.to_le_bytes());
	indication.extend_from_slice(&0u32.to_le_bytes());
	assert_eq!(indicated(&indication).map(|read| read.cid), Ok(CID_REGISTER_STATE));
	assert_eq!(signal_dbm(&[99, 0, 0, 0]), Ok(None), "99 is unknown");
}
