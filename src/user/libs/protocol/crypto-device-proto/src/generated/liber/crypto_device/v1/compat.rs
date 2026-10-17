use super::*;
use alloc::string::String;

#[test]
fn cipher_wire_is_stable() {
	let sample = Cipher::AesCbc;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(Cipher::decode(&bytes).unwrap(), sample);
}
#[test]
fn direction_wire_is_stable() {
	let sample = Direction::Encrypt;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(Direction::decode(&bytes).unwrap(), sample);
}
#[test]
fn crypto_limits_wire_is_stable() {
	let sample = CryptoLimits { ciphers: alloc::vec![Cipher::AesCbc], max_key_len: 7, max_request: 7, max_sessions: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(CryptoLimits::decode(&bytes).unwrap(), sample);
}
#[test]
fn crypto_output_wire_is_stable() {
	let sample = CryptoOutput { data: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(CryptoOutput::decode(&bytes).unwrap(), sample);
}
