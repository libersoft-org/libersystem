use super::*;
use alloc::string::String;

#[test]
fn balloon_status_wire_is_stable() {
	let sample = BalloonStatus { target: 7, held: 7, allowed: 7, held_back: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(BalloonStatus::decode(&bytes).unwrap(), sample);
}
