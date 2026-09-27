use super::*;
use alloc::string::String;

#[test]
fn gpio_trigger_wire_is_stable() {
	let sample = GpioTrigger::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(GpioTrigger::decode(&bytes).unwrap(), sample);
}
#[test]
fn gpio_line_wire_is_stable() {
	let sample = GpioLine { line: 7, name: String::from("x"), trigger: GpioTrigger::None };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 1, 0, 120, 0];
	assert_eq!(bytes, golden);
	assert_eq!(GpioLine::decode(&bytes).unwrap(), sample);
}
#[test]
fn gpio_event_wire_is_stable() {
	let sample = GpioEvent { level: true, sequence: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(GpioEvent::decode(&bytes).unwrap(), sample);
}
