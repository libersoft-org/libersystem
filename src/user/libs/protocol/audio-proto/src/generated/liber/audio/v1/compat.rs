use super::*;
use alloc::string::String;

#[test]
fn audio_resources_wire_is_stable() {
	let sample = AudioResources { counted: true, underruns: 7, silent_frames: 7, feedback_q16: 7, feedback_ignored: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(AudioResources::decode(&bytes).unwrap(), sample);
}
