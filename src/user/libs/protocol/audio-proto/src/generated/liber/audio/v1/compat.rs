use super::*;
use alloc::string::String;

#[test]
fn audio_transport_wire_is_stable() {
	let sample = AudioTransport::Provider;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(AudioTransport::decode(&bytes).unwrap(), sample);
}
#[test]
fn audio_format_wire_is_stable() {
	let sample = AudioFormat { rate: 7, channels: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(AudioFormat::decode(&bytes).unwrap(), sample);
}
#[test]
fn audio_device_wire_is_stable() {
	let sample = AudioDevice { id: 7, label: String::from("x"), transport: AudioTransport::Provider, output: Some(AudioFormat { rate: 7, channels: 7 }), input: Some(AudioFormat { rate: 7, channels: 7 }), voice: true, route: true, latency_us: 7, volume: 7, hardware_volume: true, default_output: true, default_input: true, default_voice: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 1, 0, 120, 1, 1, 7, 0, 0, 0, 7, 1, 7, 0, 0, 0, 7, 1, 1, 7, 0, 0, 0, 7, 1, 1, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(AudioDevice::decode(&bytes).unwrap(), sample);
}
#[test]
fn audio_direction_wire_is_stable() {
	let sample = AudioDirection::Output;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(AudioDirection::decode(&bytes).unwrap(), sample);
}
#[test]
fn audio_stream_info_wire_is_stable() {
	let sample = AudioStreamInfo { device: Some(7), named: Some(7), format: AudioFormat { rate: 7, channels: 7 }, latency_us: 7, silent_frames: 7, voice: true, route: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 1, 7, 0, 0, 0, 7, 0, 0, 0, 7, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(AudioStreamInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn audio_counters_wire_is_stable() {
	let sample = AudioCounters { moves: 7, silent_frames: 7, route_overflows: 7, route_underruns: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(AudioCounters::decode(&bytes).unwrap(), sample);
}
#[test]
fn audio_resources_wire_is_stable() {
	let sample = AudioResources { counted: true, underruns: 7, silent_frames: 7, feedback_q16: 7, feedback_ignored: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(AudioResources::decode(&bytes).unwrap(), sample);
}
#[test]
fn call_state_wire_is_stable() {
	let sample = CallState::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(CallState::decode(&bytes).unwrap(), sample);
}
#[test]
fn call_command_wire_is_stable() {
	let sample = CallCommand::Answer;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(CallCommand::decode(&bytes).unwrap(), sample);
}
