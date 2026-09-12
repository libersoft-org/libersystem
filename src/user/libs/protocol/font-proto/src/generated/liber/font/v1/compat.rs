use super::*;
use alloc::string::String;

#[test]
fn face_format_wire_is_stable() {
	let sample = FaceFormat::TruetypeGlyf;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(FaceFormat::decode(&bytes).unwrap(), sample);
}
#[test]
fn face_width_wire_is_stable() {
	let sample = FaceWidth::UltraCondensed;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(FaceWidth::decode(&bytes).unwrap(), sample);
}
#[test]
fn face_slant_wire_is_stable() {
	let sample = FaceSlant::Upright;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(FaceSlant::decode(&bytes).unwrap(), sample);
}
#[test]
fn face_identity_wire_is_stable() {
	let sample = FaceIdentity { digest: alloc::vec![7], index: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 7, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(FaceIdentity::decode(&bytes).unwrap(), sample);
}
#[test]
fn variation_axis_wire_is_stable() {
	let sample = VariationAxis { tag: 7, minimum: 7, default: 7, maximum: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(VariationAxis::decode(&bytes).unwrap(), sample);
}
#[test]
fn face_record_wire_is_stable() {
	let sample = FaceRecord { identity: FaceIdentity { digest: alloc::vec![7], index: 7 }, family: String::from("x"), style: String::from("x"), format: FaceFormat::TruetypeGlyf, weight: 7, width: FaceWidth::UltraCondensed, slant: FaceSlant::Upright, axes: alloc::vec![VariationAxis { tag: 7, minimum: 7, default: 7, maximum: 7 }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 7, 7, 0, 0, 0, 1, 0, 120, 1, 0, 120, 0, 7, 0, 0, 0, 1, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(FaceRecord::decode(&bytes).unwrap(), sample);
}
#[test]
fn face_bytes_wire_is_stable() {
	let sample = FaceBytes { identity: FaceIdentity { digest: alloc::vec![7], index: 7 }, generation: 7, length: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 7, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(FaceBytes::decode(&bytes).unwrap(), sample);
}
#[test]
fn resolve_outcome_wire_is_stable() {
	let sample = ResolveOutcome::Filled(7);
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ResolveOutcome::decode(&bytes).unwrap(), sample);
}
#[test]
fn generation_event_wire_is_stable() {
	let sample = GenerationEvent { generation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(GenerationEvent::decode(&bytes).unwrap(), sample);
}
#[test]
fn ceiling_wire_is_stable() {
	let sample = Ceiling::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(Ceiling::decode(&bytes).unwrap(), sample);
}
#[test]
fn rejection_reason_wire_is_stable() {
	let sample = RejectionReason::Declaration;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(RejectionReason::decode(&bytes).unwrap(), sample);
}
#[test]
fn rejection_wire_is_stable() {
	let sample = Rejection { name: String::from("x"), reason: RejectionReason::Declaration };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Rejection::decode(&bytes).unwrap(), sample);
}
#[test]
fn scan_report_wire_is_stable() {
	let sample = ScanReport { generation: 7, advanced: true, published_faces: 7, withdrawn: alloc::vec![String::from("x")], rejected: alloc::vec![Rejection { name: String::from("x"), reason: RejectionReason::Declaration }], breach: Ceiling::None };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 1, 7, 0, 0, 0, 1, 0, 1, 0, 120, 1, 0, 1, 0, 120, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ScanReport::decode(&bytes).unwrap(), sample);
}
#[test]
fn rescan_outcome_wire_is_stable() {
	let sample = RescanOutcome::Completed(ScanReport { generation: 7, advanced: true, published_faces: 7, withdrawn: alloc::vec![String::from("x")], rejected: alloc::vec![Rejection { name: String::from("x"), reason: RejectionReason::Declaration }], breach: Ceiling::None });
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 7, 0, 0, 0, 1, 0, 1, 0, 120, 1, 0, 1, 0, 120, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(RescanOutcome::decode(&bytes).unwrap(), sample);
}
