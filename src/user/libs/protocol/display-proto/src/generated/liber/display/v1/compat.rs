use super::*;
use alloc::string::String;

#[test]
fn pixel_format_wire_is_stable() {
	let sample = PixelFormat::B8g8r8x8;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(PixelFormat::decode(&bytes).unwrap(), sample);
}
#[test]
fn display_event_wire_is_stable() {
	let sample = DisplayEvent { width: 7, height: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(DisplayEvent::decode(&bytes).unwrap(), sample);
}
#[test]
fn presentation_stats_wire_is_stable() {
	let sample = PresentationStats { presents: 7, direct_presents: 7, scaled_presents: 7, source_pixels: 7, output_pixels: 7, blit_ns: 7, flush_ns: 7, max_present_ns: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
	];
	assert_eq!(bytes, golden);
	assert_eq!(PresentationStats::decode(&bytes).unwrap(), sample);
}
