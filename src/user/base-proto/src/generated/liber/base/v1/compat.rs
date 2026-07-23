use super::*;
use alloc::string::String;

#[test]
fn error_wire_is_stable() {
	let sample = Error::Denied;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(Error::decode(&bytes).unwrap(), sample);
}
