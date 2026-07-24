use super::*;
use alloc::string::String;

#[test]
fn process_info_wire_is_stable() {
	let sample = ProcessInfo { koid: 7, name: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(ProcessInfo::decode(&bytes).unwrap(), sample);
}
