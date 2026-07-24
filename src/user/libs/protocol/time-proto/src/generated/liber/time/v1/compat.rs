use super::*;
use alloc::string::String;

#[test]
fn timestamp_wire_is_stable() {
	let sample = Timestamp { unix_secs: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Timestamp::decode(&bytes).unwrap(), sample);
}
