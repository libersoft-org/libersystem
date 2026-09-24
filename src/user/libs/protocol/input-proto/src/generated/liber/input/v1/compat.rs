use super::*;
use alloc::string::String;

#[test]
fn pointer_event_wire_is_stable() {
	let sample = PointerEvent { col: 7, row: 7, buttons: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 7, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(PointerEvent::decode(&bytes).unwrap(), sample);
}
#[test]
fn key_event_wire_is_stable() {
	let sample = KeyEvent { code: 7, pressed: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 1];
	assert_eq!(bytes, golden);
	assert_eq!(KeyEvent::decode(&bytes).unwrap(), sample);
}
#[test]
fn contact_event_wire_is_stable() {
	let sample = ContactEvent { id: 7, tip: true, x: 7, y: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 1, 7, 0, 7, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ContactEvent::decode(&bytes).unwrap(), sample);
}
#[test]
fn trusted_input_kind_wire_is_stable() {
	let sample = TrustedInputKind::Attention;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(TrustedInputKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn trusted_input_wire_is_stable() {
	let sample = TrustedInput { kind: TrustedInputKind::Attention, epoch: 7, usage: 7, down: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 1];
	assert_eq!(bytes, golden);
	assert_eq!(TrustedInput::decode(&bytes).unwrap(), sample);
}
