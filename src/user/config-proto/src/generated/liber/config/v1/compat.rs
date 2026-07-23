use super::*;
use alloc::string::String;

#[test]
fn config_entry_wire_is_stable() {
	let sample = ConfigEntry { key: String::from("x"), value: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(ConfigEntry::decode(&bytes).unwrap(), sample);
}
