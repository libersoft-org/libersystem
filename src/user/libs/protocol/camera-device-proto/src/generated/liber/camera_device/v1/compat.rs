use super::*;
use alloc::string::String;

#[test]
fn camera_open_wire_is_stable() {
	let sample = CameraOpen { connection_generation: 7, name: String::from("x"), generation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(CameraOpen::decode(&bytes).unwrap(), sample);
}
#[test]
fn camera_drop_reason_wire_is_stable() {
	let sample = CameraDropReason::NoBuffer;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(CameraDropReason::decode(&bytes).unwrap(), sample);
}
#[test]
fn camera_drop_wire_is_stable() {
	let sample = CameraDrop { stream_generation: 7, first_sequence: 7, count: 7, reason: CameraDropReason::NoBuffer };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 1];
	assert_eq!(bytes, golden);
	assert_eq!(CameraDrop::decode(&bytes).unwrap(), sample);
}
#[test]
fn camera_gap_wire_is_stable() {
	let sample = CameraGap { stream_generation: 7, next_sequence: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(CameraGap::decode(&bytes).unwrap(), sample);
}
#[test]
fn camera_fixture_stats_wire_is_stable() {
	let sample = CameraFixtureStats { frames_written: 7, dropped_no_buffer: 7, dropped_device: 7, gaps: 7, mapped: 7, stops: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(CameraFixtureStats::decode(&bytes).unwrap(), sample);
}
