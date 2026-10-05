use super::*;
use alloc::string::String;

#[test]
fn backlight_function_wire_is_stable() {
	let sample = BacklightFunction { bus: 7, dev: 7, func: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(BacklightFunction::decode(&bytes).unwrap(), sample);
}
#[test]
fn monitor_identity_wire_is_stable() {
	let sample = MonitorIdentity { manufacturer: 7, product: 7, serial: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 7, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(MonitorIdentity::decode(&bytes).unwrap(), sample);
}
#[test]
fn backlight_target_wire_is_stable() {
	let sample = BacklightTarget::Function(BacklightFunction { bus: 7, dev: 7, func: 7 });
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(BacklightTarget::decode(&bytes).unwrap(), sample);
}
#[test]
fn backlight_source_wire_is_stable() {
	let sample = BacklightSource::Firmware;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(BacklightSource::decode(&bytes).unwrap(), sample);
}
#[test]
fn backlight_scale_wire_is_stable() {
	let sample = BacklightScale::Levels(alloc::vec![7]);
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 1, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(BacklightScale::decode(&bytes).unwrap(), sample);
}
#[test]
fn backlight_range_wire_is_stable() {
	let sample = BacklightRange { minimum: 7, maximum: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(BacklightRange::decode(&bytes).unwrap(), sample);
}
#[test]
fn backlight_description_wire_is_stable() {
	let sample = BacklightDescription { source: BacklightSource::Firmware, key: String::from("x"), scale: BacklightScale::Levels(alloc::vec![7]), ac_default: Some(7), battery_default: Some(7), target: BacklightTarget::Function(BacklightFunction { bus: 7, dev: 7, func: 7 }) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 1, 0, 120, 0, 1, 0, 7, 0, 0, 0, 1, 7, 0, 0, 0, 1, 7, 0, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(BacklightDescription::decode(&bytes).unwrap(), sample);
}
#[test]
fn backlight_hotkey_wire_is_stable() {
	let sample = BacklightHotkey::Up;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(BacklightHotkey::decode(&bytes).unwrap(), sample);
}
#[test]
fn backlight_event_wire_is_stable() {
	let sample = BacklightEvent::Level(7);
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(BacklightEvent::decode(&bytes).unwrap(), sample);
}
#[test]
fn light_response_wire_is_stable() {
	let sample = LightResponse { adjustment: 7, illuminance: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(LightResponse::decode(&bytes).unwrap(), sample);
}
#[test]
fn ambient_light_description_wire_is_stable() {
	let sample = AmbientLightDescription { curve: alloc::vec![LightResponse { adjustment: 7, illuminance: 7 }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(AmbientLightDescription::decode(&bytes).unwrap(), sample);
}
#[test]
fn illuminance_wire_is_stable() {
	let sample = Illuminance { milli_lux: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Illuminance::decode(&bytes).unwrap(), sample);
}
