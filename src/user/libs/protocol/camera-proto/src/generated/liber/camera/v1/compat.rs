use super::*;
use alloc::string::String;

#[test]
fn camera_id_wire_is_stable() {
	let sample = CameraId { slot: 7, generation: 7, binding_generation: 7, incarnation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(CameraId::decode(&bytes).unwrap(), sample);
}
#[test]
fn frame_type_wire_is_stable() {
	let sample = FrameType::Yuy2;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(FrameType::decode(&bytes).unwrap(), sample);
}
#[test]
fn color_range_wire_is_stable() {
	let sample = ColorRange::Unknown;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(ColorRange::decode(&bytes).unwrap(), sample);
}
#[test]
fn color_matrix_wire_is_stable() {
	let sample = ColorMatrix::Unknown;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(ColorMatrix::decode(&bytes).unwrap(), sample);
}
#[test]
fn interval_wire_is_stable() {
	let sample = Interval { numerator: 7, denominator: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Interval::decode(&bytes).unwrap(), sample);
}
#[test]
fn discrete_intervals_wire_is_stable() {
	let sample = DiscreteIntervals { values: alloc::vec![Interval { numerator: 7, denominator: 7 }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(DiscreteIntervals::decode(&bytes).unwrap(), sample);
}
#[test]
fn interval_range_wire_is_stable() {
	let sample = IntervalRange { minimum: Interval { numerator: 7, denominator: 7 }, maximum: Interval { numerator: 7, denominator: 7 }, step: Interval { numerator: 7, denominator: 7 } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(IntervalRange::decode(&bytes).unwrap(), sample);
}
#[test]
fn intervals_wire_is_stable() {
	let sample = Intervals::Discrete(DiscreteIntervals { values: alloc::vec![Interval { numerator: 7, denominator: 7 }] });
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 1, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Intervals::decode(&bytes).unwrap(), sample);
}
#[test]
fn frame_size_wire_is_stable() {
	let sample = FrameSize { index: 7, width: 7, height: 7, max_bytes: 7, intervals: Intervals::Discrete(DiscreteIntervals { values: alloc::vec![Interval { numerator: 7, denominator: 7 }] }) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 0, 7, 0, 7, 0, 0, 0, 0, 1, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(FrameSize::decode(&bytes).unwrap(), sample);
}
#[test]
fn format_info_wire_is_stable() {
	let sample = FormatInfo { index: 7, frame_type: FrameType::Yuy2, compressed: true, sizes: 7, range: ColorRange::Unknown, matrix: ColorMatrix::Unknown };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 1, 1, 7, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(FormatInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn camera_info_wire_is_stable() {
	let sample = CameraInfo { id: CameraId { slot: 7, generation: 7, binding_generation: 7, incarnation: 7 }, name: String::from("x"), formats: alloc::vec![FormatInfo { index: 7, frame_type: FrameType::Yuy2, compressed: true, sizes: 7, range: ColorRange::Unknown, matrix: ColorMatrix::Unknown }], generation: 7, streaming: true, available: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 120, 1, 0, 7, 1, 1, 7, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(CameraInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn format_page_wire_is_stable() {
	let sample = FormatPage { generation: 7, format: FormatInfo { index: 7, frame_type: FrameType::Yuy2, compressed: true, sizes: 7, range: ColorRange::Unknown, matrix: ColorMatrix::Unknown }, offset: 7, sizes: alloc::vec![FrameSize { index: 7, width: 7, height: 7, max_bytes: 7, intervals: Intervals::Discrete(DiscreteIntervals { values: alloc::vec![Interval { numerator: 7, denominator: 7 }] }) }], total: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 1, 1, 7, 0, 0, 7, 1, 0, 7, 7, 0, 7, 0, 7, 0, 0, 0, 0, 1, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(FormatPage::decode(&bytes).unwrap(), sample);
}
#[test]
fn stream_request_wire_is_stable() {
	let sample = StreamRequest { format: 7, size: 7, interval: Interval { numerator: 7, denominator: 7 } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(StreamRequest::decode(&bytes).unwrap(), sample);
}
#[test]
fn negotiated_wire_is_stable() {
	let sample = Negotiated { stream_generation: 7, format: 7, frame_type: FrameType::Yuy2, width: 7, height: 7, interval: Interval { numerator: 7, denominator: 7 }, max_bytes: 7, stride: 7, plane_offset: 7, range: ColorRange::Unknown, matrix: ColorMatrix::Unknown };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 1, 7, 0, 7, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Negotiated::decode(&bytes).unwrap(), sample);
}
#[test]
fn device_time_wire_is_stable() {
	let sample = DeviceTime { ticks: 7, width_bits: 7, frequency_hz: Some(7), clock_domain: 7, reset_generation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 1, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(DeviceTime::decode(&bytes).unwrap(), sample);
}
#[test]
fn frame_wire_is_stable() {
	let sample = Frame { stream_generation: 7, sequence: 7, buffer: 7, lease: 7, valid_bytes: 7, arrival_ns: 7, device: Some(DeviceTime { ticks: 7, width_bits: 7, frequency_hz: Some(7), clock_domain: 7, reset_generation: 7 }) };
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
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		1,
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
		7,
		0,
		0,
		0,
	];
	assert_eq!(bytes, golden);
	assert_eq!(Frame::decode(&bytes).unwrap(), sample);
}
#[test]
fn capture_event_wire_is_stable() {
	let sample = CaptureEvent::Frame(Frame { stream_generation: 7, sequence: 7, buffer: 7, lease: 7, valid_bytes: 7, arrival_ns: 7, device: Some(DeviceTime { ticks: 7, width_bits: 7, frequency_hz: Some(7), clock_domain: 7, reset_generation: 7 }) });
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
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
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		1,
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
		7,
		0,
		0,
		0,
	];
	assert_eq!(bytes, golden);
	assert_eq!(CaptureEvent::decode(&bytes).unwrap(), sample);
}
