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
