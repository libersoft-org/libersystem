use super::*;
use alloc::string::String;

#[test]
fn i2c_functionality_wire_is_stable() {
	let sample = I2cFunctionality { plain: true, max_transfer: 7, quick: true, byte: true, byte_data: true, word_data: true, block_write: true, block_read: true, i2c_block_read: true, pec: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 1, 1, 1, 1, 1, 1, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(I2cFunctionality::decode(&bytes).unwrap(), sample);
}
#[test]
fn i2c_status_wire_is_stable() {
	let sample = I2cStatus::Ok;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(I2cStatus::decode(&bytes).unwrap(), sample);
}
#[test]
fn i2c_reply_wire_is_stable() {
	let sample = I2cReply { status: I2cStatus::Ok, bytes: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(I2cReply::decode(&bytes).unwrap(), sample);
}
