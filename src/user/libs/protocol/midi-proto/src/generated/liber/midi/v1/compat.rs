use super::*;
use alloc::string::String;

#[test]
fn midi_endpoint_id_wire_is_stable() {
	let sample = MidiEndpointId { slot: 7, generation: 7, binding_generation: 7, endpoint: 7, incarnation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(MidiEndpointId::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_protocol_wire_is_stable() {
	let sample = MidiProtocol::Midi1;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(MidiProtocol::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_direction_wire_is_stable() {
	let sample = MidiDirection::Receive;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(MidiDirection::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_endpoint_wire_is_stable() {
	let sample = MidiEndpoint { id: MidiEndpointId { slot: 7, generation: 7, binding_generation: 7, endpoint: 7, incarnation: 7 }, name: String::from("x"), protocol: MidiProtocol::Midi1, direction: MidiDirection::Receive, cables: 7, receiving: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 120, 1, 1, 7, 1];
	assert_eq!(bytes, golden);
	assert_eq!(MidiEndpoint::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_chunk_kind_wire_is_stable() {
	let sample = MidiChunkKind::Short;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(MidiChunkKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_chunk_wire_is_stable() {
	let sample = MidiChunk { cable: 7, kind: MidiChunkKind::Short, bytes: alloc::vec![7], sysex_message: Some(7), start: true, end: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 1, 1, 0, 7, 1, 7, 0, 0, 0, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(MidiChunk::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_abort_reason_wire_is_stable() {
	let sample = MidiAbortReason::Cap;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(MidiAbortReason::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_abort_wire_is_stable() {
	let sample = MidiAbort { cable: 7, message: 7, reason: MidiAbortReason::Cap };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 0, 0, 0, 1];
	assert_eq!(bytes, golden);
	assert_eq!(MidiAbort::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_fault_code_wire_is_stable() {
	let sample = MidiFaultCode::Alignment;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(MidiFaultCode::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_fault_wire_is_stable() {
	let sample = MidiFault { cable: Some(7), code: MidiFaultCode::Alignment };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 1];
	assert_eq!(bytes, golden);
	assert_eq!(MidiFault::decode(&bytes).unwrap(), sample);
}
#[test]
fn midi_item_wire_is_stable() {
	let sample = MidiItem::Chunk(MidiChunk { cable: 7, kind: MidiChunkKind::Short, bytes: alloc::vec![7], sysex_message: Some(7), start: true, end: true });
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 1, 1, 0, 7, 1, 7, 0, 0, 0, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(MidiItem::decode(&bytes).unwrap(), sample);
}
