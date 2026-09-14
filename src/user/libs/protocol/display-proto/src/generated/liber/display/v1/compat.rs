use super::*;
use alloc::string::String;

#[test]
fn scale_ratio_wire_is_stable() {
	let sample = ScaleRatio { numerator: 7, denominator: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ScaleRatio::decode(&bytes).unwrap(), sample);
}
#[test]
fn output_transform_wire_is_stable() {
	let sample = OutputTransform::Normal;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(OutputTransform::decode(&bytes).unwrap(), sample);
}
#[test]
fn subpixel_layout_wire_is_stable() {
	let sample = SubpixelLayout::Unknown;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(SubpixelLayout::decode(&bytes).unwrap(), sample);
}
#[test]
fn timestamp_evidence_wire_is_stable() {
	let sample = TimestampEvidence::Unavailable;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(TimestampEvidence::decode(&bytes).unwrap(), sample);
}
#[test]
fn present_outcome_wire_is_stable() {
	let sample = PresentOutcome::Displayed;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(PresentOutcome::decode(&bytes).unwrap(), sample);
}
#[test]
fn frame_timing_wire_is_stable() {
	let sample = FrameTiming { preferred_deadline: Some(7), refresh_interval: Some(7) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 0, 0, 0, 0, 1, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(FrameTiming::decode(&bytes).unwrap(), sample);
}
#[test]
fn present_complete_wire_is_stable() {
	let sample = PresentComplete { serial: 7, outcome: PresentOutcome::Displayed, evidence: TimestampEvidence::Unavailable, timing: FrameTiming { preferred_deadline: Some(7), refresh_interval: Some(7) } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 7, 0, 0, 0, 0, 0, 0, 0, 1, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(PresentComplete::decode(&bytes).unwrap(), sample);
}
#[test]
fn acquired_image_wire_is_stable() {
	let sample = AcquiredImage::Image(7);
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(AcquiredImage::decode(&bytes).unwrap(), sample);
}
#[test]
fn image_limits_wire_is_stable() {
	let sample = ImageLimits { minimum: 7, maximum: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ImageLimits::decode(&bytes).unwrap(), sample);
}
#[test]
fn presentation_stats_wire_is_stable() {
	let sample = PresentationStats { presents: 7, direct_presents: 7, scaled_presents: 7, source_pixels: 7, output_pixels: 7, blit_ns: 7, flush_ns: 7, max_present_ns: 7, displayed: 7, discarded: 7, replaced: 7, lost: 7 };
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
#[test]
fn display_resources_wire_is_stable() {
	let sample = DisplayResources { surfaces: 7, surface_bound: 7, present_images: 7, image_bound: 7, queued_presents: 7, present_bound: 7, damage_entries: 7, damage_bound: 7, waiters: 7, waiter_bound: 7, resets: 7, faulted: true };
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
		1,
	];
	assert_eq!(bytes, golden);
	assert_eq!(DisplayResources::decode(&bytes).unwrap(), sample);
}
