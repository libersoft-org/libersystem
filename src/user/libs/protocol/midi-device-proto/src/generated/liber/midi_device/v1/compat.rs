use super::*;
use alloc::string::String;

#[test]
fn midi_bounds_wire_is_stable() {
	let sample = MidiBounds { version: 7, max_packets: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0];
	assert_eq!(bytes, golden);
	assert_eq!(MidiBounds::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_open_wire_is_stable() {
	let sample = MidiOpen { connection_generation: 7, bounds: MidiBounds { version: 7, max_packets: 7 } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 7, 0];
	assert_eq!(bytes, golden);
	assert_eq!(MidiOpen::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_device_endpoint_wire_is_stable() {
	let sample = MidiDeviceEndpoint { index: 7, name: String::from("x"), cables: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 1, 0, 120, 7];
	assert_eq!(bytes, golden);
	assert_eq!(MidiDeviceEndpoint::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_packet_batch_wire_is_stable() {
	let sample = MidiPacketBatch { endpoint: 7, receiver_generation: 7, received_ns: 7, packets: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(MidiPacketBatch::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_input_lost_wire_is_stable() {
	let sample = MidiInputLost { endpoint: 7, receiver_generation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(MidiInputLost::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_device_event_wire_is_stable() {
	let sample = MidiDeviceEvent::Batch(MidiPacketBatch { endpoint: 7, receiver_generation: 7, received_ns: 7, packets: alloc::vec![7] });
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(MidiDeviceEvent::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_fixture_stats_wire_is_stable() {
	let sample = MidiFixtureStats { batches: 7, packets: 7, receiving: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(MidiFixtureStats::decode(&bytes).unwrap(), sample);
}
