use super::*;
use alloc::string::String;

#[test]
fn exchange_level_wire_is_stable() {
	let sample = ExchangeLevel::Character;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(ExchangeLevel::decode(&bytes).unwrap(), sample);
}
#[test]
fn card_protocol_wire_is_stable() {
	let sample = CardProtocol::T0;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(CardProtocol::decode(&bytes).unwrap(), sample);
}
#[test]
fn reader_id_wire_is_stable() {
	let sample = ReaderId { slot: 7, generation: 7, binding_generation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ReaderId::decode(&bytes).unwrap(), sample);
}
#[test]
fn reader_info_wire_is_stable() {
	let sample = ReaderInfo { id: ReaderId { slot: 7, generation: 7, binding_generation: 7 }, name: String::from("x"), alias: String::from("x"), slots: 7, exchange: ExchangeLevel::Character, pinpad: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 120, 1, 0, 120, 7, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(ReaderInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn presence_wire_is_stable() {
	let sample = Presence::Absent;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(Presence::decode(&bytes).unwrap(), sample);
}
#[test]
fn slot_state_wire_is_stable() {
	let sample = SlotState { slot: 7, presence: Presence::Absent, card_generation: 7, atr: alloc::vec![7], available: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 1, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7, 1];
	assert_eq!(bytes, golden);
	assert_eq!(SlotState::decode(&bytes).unwrap(), sample);
}
#[test]
fn event_kind_wire_is_stable() {
	let sample = EventKind::Snapshot;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(EventKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn card_event_wire_is_stable() {
	let sample = CardEvent { sequence: 7, kind: EventKind::Snapshot, slot: Some(SlotState { slot: 7, presence: Presence::Absent, card_generation: 7, atr: alloc::vec![7], available: true }) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 1, 1, 7, 1, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7, 1];
	assert_eq!(bytes, golden);
	assert_eq!(CardEvent::decode(&bytes).unwrap(), sample);
}
#[test]
fn transaction_wire_is_stable() {
	let sample = Transaction { id: 7, slot: 7, card_generation: 7, lease_ms: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Transaction::decode(&bytes).unwrap(), sample);
}
#[test]
fn apdu_response_wire_is_stable() {
	let sample = ApduResponse { data: alloc::vec![7], sw1: 7, sw2: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 7, 7, 7];
	assert_eq!(bytes, golden);
	assert_eq!(ApduResponse::decode(&bytes).unwrap(), sample);
}
#[test]
fn card_outcome_wire_is_stable() {
	let sample = CardOutcome::Done;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(CardOutcome::decode(&bytes).unwrap(), sample);
}
#[test]
fn exchange_result_wire_is_stable() {
	let sample = ExchangeResult { outcome: CardOutcome::Done, response: Some(ApduResponse { data: alloc::vec![7], sw1: 7, sw2: 7 }) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 1, 0, 7, 7, 7];
	assert_eq!(bytes, golden);
	assert_eq!(ExchangeResult::decode(&bytes).unwrap(), sample);
}
#[test]
fn pin_result_wire_is_stable() {
	let sample = PinResult::Verified;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(PinResult::decode(&bytes).unwrap(), sample);
}
#[test]
fn pin_outcome_wire_is_stable() {
	let sample = PinOutcome { result: PinResult::Verified, retries: Some(7) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 7];
	assert_eq!(bytes, golden);
	assert_eq!(PinOutcome::decode(&bytes).unwrap(), sample);
}
#[test]
fn authenticate_result_wire_is_stable() {
	let sample = AuthenticateResult { outcome: CardOutcome::Done, signature: Some(alloc::vec![7]) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(AuthenticateResult::decode(&bytes).unwrap(), sample);
}
#[test]
fn operations_wire_is_stable() {
	let sample = Operations { read: true, transact: true, authenticate: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(Operations::decode(&bytes).unwrap(), sample);
}
#[test]
fn pinpad_capabilities_wire_is_stable() {
	let sample = PinpadCapabilities { secure_verify: true, ascii: true, max_block: 7, min_digits: 7, max_digits: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 7, 7, 7];
	assert_eq!(bytes, golden);
	assert_eq!(PinpadCapabilities::decode(&bytes).unwrap(), sample);
}
#[test]
fn reader_description_wire_is_stable() {
	let sample = ReaderDescription { name: String::from("x"), slots: 7, exchange: ExchangeLevel::Character, protocols: 7, pinpad: PinpadCapabilities { secure_verify: true, ascii: true, max_block: 7, min_digits: 7, max_digits: 7 } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 7, 1, 7, 1, 1, 7, 7, 7];
	assert_eq!(bytes, golden);
	assert_eq!(ReaderDescription::decode(&bytes).unwrap(), sample);
}
#[test]
fn slot_report_wire_is_stable() {
	let sample = SlotReport { slot: 7, present: true, card_generation: 7, atr: alloc::vec![7], powered: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 1, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7, 1];
	assert_eq!(bytes, golden);
	assert_eq!(SlotReport::decode(&bytes).unwrap(), sample);
}
#[test]
fn request_header_wire_is_stable() {
	let sample = RequestHeader { request: 7, slot: 7, card_generation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(RequestHeader::decode(&bytes).unwrap(), sample);
}
#[test]
fn secure_verify_request_wire_is_stable() {
	let sample = SecureVerifyRequest { header: RequestHeader { request: 7, slot: 7, card_generation: 7 }, block: 7, min_digits: 7, max_digits: 7, timeout_seconds: 7, apdu: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 7, 0, 0, 0, 0, 0, 0, 0, 7, 7, 7, 7, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(SecureVerifyRequest::decode(&bytes).unwrap(), sample);
}
#[test]
fn provider_outcome_wire_is_stable() {
	let sample = ProviderOutcome::Done;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(ProviderOutcome::decode(&bytes).unwrap(), sample);
}
#[test]
fn provider_reply_wire_is_stable() {
	let sample = ProviderReply { header: RequestHeader { request: 7, slot: 7, card_generation: 7 }, outcome: ProviderOutcome::Done, response: alloc::vec![7], atr: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 7, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 7, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(ProviderReply::decode(&bytes).unwrap(), sample);
}
#[test]
fn reader_event_kind_wire_is_stable() {
	let sample = ReaderEventKind::Presence;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(ReaderEventKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn reader_event_wire_is_stable() {
	let sample = ReaderEvent { kind: ReaderEventKind::Presence, slot: 7, report: Some(SlotReport { slot: 7, present: true, card_generation: 7, atr: alloc::vec![7], powered: true }) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 1, 7, 1, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7, 1];
	assert_eq!(bytes, golden);
	assert_eq!(ReaderEvent::decode(&bytes).unwrap(), sample);
}
#[test]
fn fixture_pin_wire_is_stable() {
	let sample = FixturePin::Verified;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(FixturePin::decode(&bytes).unwrap(), sample);
}
#[test]
fn fixture_operation_wire_is_stable() {
	let sample = FixtureOperation { at: 7, reader: 7, slot: 7, what: String::from("x"), header: alloc::vec![7], pin_bytes: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 7, 1, 0, 120, 1, 0, 7, 1];
	assert_eq!(bytes, golden);
	assert_eq!(FixtureOperation::decode(&bytes).unwrap(), sample);
}
