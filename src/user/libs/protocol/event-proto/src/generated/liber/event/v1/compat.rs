use super::*;
use alloc::string::String;

#[test]
fn event_source_wire_is_stable() {
	let sample = EventSource { incarnation: 7, slot: 7, generation: 7, binding_generation: 7, endpoint: 7, receiver_generation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(EventSource::decode(&bytes).unwrap(), sample);
}
#[test]
fn event_header_wire_is_stable() {
	let sample = EventHeader { sequence: 7, received_ns: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(EventHeader::decode(&bytes).unwrap(), sample);
}
#[test]
fn event_end_reason_wire_is_stable() {
	let sample = EventEndReason::Overflow;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(EventEndReason::decode(&bytes).unwrap(), sample);
}
#[test]
fn event_end_wire_is_stable() {
	let sample = EventEnd { reason: EventEndReason::Overflow, next_sequence: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(EventEnd::decode(&bytes).unwrap(), sample);
}
#[test]
fn generic_event_wire_is_stable() {
	let sample = GenericEvent { header: EventHeader { sequence: 7, received_ns: 7 }, payload: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(GenericEvent::decode(&bytes).unwrap(), sample);
}
