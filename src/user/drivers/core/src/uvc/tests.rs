use super::*;

fn format_yuy2(index: u8, frames: u8, guid: [u8; 16]) -> Vec<u8> {
	let mut d = alloc::vec![27, CS_INTERFACE, VS_FORMAT_UNCOMPRESSED, index, frames];
	d.extend_from_slice(&guid);
	d.extend_from_slice(&[16, 1, 0, 0, 0, 0]);
	d
}

fn format_mjpeg(index: u8, frames: u8) -> Vec<u8> {
	alloc::vec![11, CS_INTERFACE, VS_FORMAT_MJPEG, index, frames, 0, 1, 0, 0, 0, 0]
}

fn frame(subtype: u8, index: u8, width: u16, height: u16, max_bytes: u32, intervals: &[u32]) -> Vec<u8> {
	let mut d = alloc::vec![(26 + 4 * intervals.len()) as u8, CS_INTERFACE, subtype, index, 0];
	d.extend_from_slice(&width.to_le_bytes());
	d.extend_from_slice(&height.to_le_bytes());
	d.extend_from_slice(&1_000_000u32.to_le_bytes());
	d.extend_from_slice(&2_000_000u32.to_le_bytes());
	d.extend_from_slice(&max_bytes.to_le_bytes());
	d.extend_from_slice(&intervals.first().copied().unwrap_or(333_333).to_le_bytes());
	d.push(intervals.len() as u8);
	for interval in intervals {
		d.extend_from_slice(&interval.to_le_bytes());
	}
	d
}

fn stepwise(subtype: u8, index: u8, width: u16, height: u16, max_bytes: u32, minimum: u32, maximum: u32, step: u32) -> Vec<u8> {
	let mut d = frame(subtype, index, width, height, max_bytes, &[]);
	d[0] = 38;
	for value in [minimum, maximum, step] {
		d.extend_from_slice(&value.to_le_bytes());
	}
	d
}

fn color(matrix: u8) -> Vec<u8> {
	alloc::vec![6, CS_INTERFACE, VS_COLORFORMAT, 1, 1, matrix]
}

fn graph(parts: &[Vec<u8>]) -> Vec<u8> {
	parts.concat()
}

fn camera() -> Vec<u8> {
	graph(&[
		format_yuy2(1, 2, YUY2_GUID),
		frame(VS_FRAME_UNCOMPRESSED, 1, 640, 480, 640 * 480 * 2, &[333_333, 666_666]),
		frame(VS_FRAME_UNCOMPRESSED, 2, 320, 240, 320 * 240 * 2, &[333_333]),
		color(4),
		format_mjpeg(2, 1),
		stepwise(VS_FRAME_MJPEG, 1, 1280, 720, 1 << 20, 333_333, 999_999, 333_333),
		color(1),
	])
}

#[test]
fn a_camera_normalizes_to_its_two_formats() {
	let normalized = normalize(&camera()).unwrap();
	assert_eq!(normalized.skipped, 0);
	assert_eq!(normalized.formats.len(), 2);
	let yuy2 = &normalized.formats[0];
	assert_eq!((yuy2.index, yuy2.kind, yuy2.sizes.len(), yuy2.matrix, yuy2.range), (1, Kind::Yuy2, 2, Matrix::Bt601, Range::Limited));
	assert_eq!(yuy2.sizes[0].intervals, Intervals::Discrete(alloc::vec![333_333, 666_666]));
	let mjpeg = &normalized.formats[1];
	assert_eq!((mjpeg.kind, mjpeg.matrix, mjpeg.range), (Kind::Mjpeg, Matrix::Bt709, Range::Full));
	assert_eq!(mjpeg.sizes[0].intervals, Intervals::Stepwise { minimum: 333_333, maximum: 999_999, step: 333_333 });
}

#[test]
fn an_unsupported_format_is_skipped_with_its_frames_and_counted() {
	let nv12 = [0x4e, 0x56, 0x31, 0x32, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71];
	let normalized = normalize(&graph(&[
		format_yuy2(1, 1, nv12),
		frame(VS_FRAME_UNCOMPRESSED, 1, 640, 480, 640 * 480 * 2, &[333_333]),
		format_mjpeg(2, 1),
		frame(VS_FRAME_MJPEG, 1, 640, 480, 1 << 20, &[333_333]),
	]))
	.unwrap();
	assert_eq!(normalized.skipped, 1);
	assert_eq!(normalized.formats.len(), 1);
	assert_eq!(normalized.formats[0].kind, Kind::Mjpeg);
}

#[test]
fn malformed_shapes_refuse_the_whole_graph() {
	let mut short = camera();
	short.truncate(short.len() - 2);
	assert_eq!(normalize(&short), Err(Refused::Malformed), "a descriptor past the graph");
	let mut wrong = format_mjpeg(1, 1);
	wrong[0] = 12;
	wrong.push(0);
	assert_eq!(normalize(&graph(&[wrong, frame(VS_FRAME_MJPEG, 1, 64, 64, 4096, &[333_333])])), Err(Refused::Malformed), "a format of the wrong length");
	assert_eq!(normalize(&frame(VS_FRAME_MJPEG, 1, 64, 64, 4096, &[333_333])), Err(Refused::Structure), "a frame with no format");
	let mut lying = frame(VS_FRAME_MJPEG, 1, 64, 64, 4096, &[333_333]);
	lying[25] = 2;
	assert_eq!(normalize(&graph(&[format_mjpeg(1, 1), lying])), Err(Refused::Malformed), "an interval count past the descriptor");
	assert_eq!(normalize(&[0, 0, 0]), Err(Refused::Malformed), "a zero length never advances");
}

#[test]
fn duplicates_counts_and_type_mismatches_are_structure() {
	assert_eq!(normalize(&graph(&[format_mjpeg(1, 1), frame(VS_FRAME_MJPEG, 1, 64, 64, 4096, &[333_333]), format_mjpeg(1, 1), frame(VS_FRAME_MJPEG, 1, 64, 64, 4096, &[333_333])])), Err(Refused::Structure), "a duplicate format index");
	assert_eq!(normalize(&graph(&[format_mjpeg(1, 2), frame(VS_FRAME_MJPEG, 1, 64, 64, 4096, &[333_333]), frame(VS_FRAME_MJPEG, 1, 32, 32, 4096, &[333_333])])), Err(Refused::Structure), "a duplicate frame index");
	assert_eq!(normalize(&graph(&[format_mjpeg(1, 2), frame(VS_FRAME_MJPEG, 1, 64, 64, 4096, &[333_333])])), Err(Refused::Structure), "fewer frames than declared");
	assert_eq!(normalize(&graph(&[format_yuy2(1, 1, YUY2_GUID), frame(VS_FRAME_MJPEG, 1, 64, 64, 64 * 64 * 2, &[333_333])])), Err(Refused::Structure), "an MJPEG frame under a YUY2 format");
	assert_eq!(normalize(&color(1)), Err(Refused::Structure), "a colour format describing nothing");
}

#[test]
fn dimensions_strides_and_sizes_are_checked_exactly() {
	assert_eq!(normalize(&graph(&[format_mjpeg(1, 1), frame(VS_FRAME_MJPEG, 1, 0, 64, 4096, &[333_333])])), Err(Refused::Invalid), "a zero width");
	// YUY2 needs two bytes a pixel: exactly enough is accepted, a byte less is not.
	assert!(normalize(&graph(&[format_yuy2(1, 1, YUY2_GUID), frame(VS_FRAME_UNCOMPRESSED, 1, 64, 64, 64 * 64 * 2, &[333_333])])).is_ok());
	assert_eq!(normalize(&graph(&[format_yuy2(1, 1, YUY2_GUID), frame(VS_FRAME_UNCOMPRESSED, 1, 64, 64, 64 * 64 * 2 - 1, &[333_333])])), Err(Refused::Invalid));
	// A stride and size that overflow are refused before anything is compared.
	assert_eq!(yuy2_layout(u16::MAX, u16::MAX), None);
	assert_eq!(normalize(&graph(&[format_yuy2(1, 1, YUY2_GUID), frame(VS_FRAME_UNCOMPRESSED, 1, u16::MAX, u16::MAX, MAX_FRAME_BYTES, &[333_333])])), Err(Refused::Invalid));
	// A buffer past what one client buffer may hold is refused, exactly at the bound.
	assert!(normalize(&graph(&[format_mjpeg(1, 1), frame(VS_FRAME_MJPEG, 1, 64, 64, MAX_FRAME_BYTES, &[333_333])])).is_ok());
	assert_eq!(normalize(&graph(&[format_mjpeg(1, 1), frame(VS_FRAME_MJPEG, 1, 64, 64, MAX_FRAME_BYTES + 1, &[333_333])])), Err(Refused::Invalid));
}

#[test]
fn intervals_are_checked_and_never_expanded() {
	let one = |intervals: &[u32]| normalize(&graph(&[format_mjpeg(1, 1), frame(VS_FRAME_MJPEG, 1, 64, 64, 4096, intervals)]));
	assert_eq!(one(&[0]), Err(Refused::Invalid), "a zero interval");
	assert_eq!(one(&[333_333, 333_333]), Err(Refused::Invalid), "a repeated interval");
	let range = |minimum, maximum, step| normalize(&graph(&[format_mjpeg(1, 1), stepwise(VS_FRAME_MJPEG, 1, 64, 64, 4096, minimum, maximum, step)]));
	assert_eq!(range(1_000_000, 333_333, 1), Err(Refused::Invalid), "a range upside down");
	assert_eq!(range(333_333, 666_666, 0), Err(Refused::Invalid), "a range with no step");
	assert_eq!(range(333_333, 1_000_000, 111_112), Err(Refused::Invalid), "a step that does not reach the maximum is refused, not rounded");
	assert!(range(333_333, 333_333, 0).is_ok(), "one value needs no step");
	assert!(range(1, u32::MAX, 1).is_ok(), "a wide range stays three numbers");
}

#[test]
fn every_budget_holds_exactly() {
	let mut eight: Vec<Vec<u8>> = Vec::new();
	for index in 1..=8u8 {
		eight.push(format_mjpeg(index, 1));
		eight.push(frame(VS_FRAME_MJPEG, 1, 64, 64, 4096, &[333_333]));
	}
	assert_eq!(normalize(&graph(&eight)).unwrap().formats.len(), MAX_FORMATS);
	eight.push(format_mjpeg(9, 1));
	eight.push(frame(VS_FRAME_MJPEG, 1, 64, 64, 4096, &[333_333]));
	assert_eq!(normalize(&graph(&eight)), Err(Refused::Budget), "a ninth format");
	assert_eq!(normalize(&graph(&[format_mjpeg(1, 33)])), Err(Refused::Budget), "thirty-three sizes declared");
	let many: Vec<u32> = (1..=32).map(|n| n * 1000).collect();
	assert!(normalize(&graph(&[format_mjpeg(1, 1), frame(VS_FRAME_MJPEG, 1, 64, 64, 4096, &many)])).is_ok());
	let too_many: Vec<u32> = (1..=33).map(|n| n * 1000).collect();
	let mut descriptor = frame(VS_FRAME_MJPEG, 1, 64, 64, 4096, &too_many);
	descriptor[0] = 26 + 4 * 33;
	assert_eq!(normalize(&graph(&[format_mjpeg(1, 1), descriptor])), Err(Refused::Budget), "thirty-three intervals");
	assert_eq!(normalize(&alloc::vec![0u8; MAX_GRAPH_BYTES + 1]), Err(Refused::Budget), "a graph past its budget is not read");
}

#[test]
fn selection_is_exactly_what_was_asked_or_nothing() {
	let normalized = normalize(&graph(&[
		format_yuy2(1, 1, YUY2_GUID),
		frame(VS_FRAME_UNCOMPRESSED, 1, 640, 480, 640 * 480 * 2, &[333_333, 666_666]),
		format_mjpeg(2, 1),
		stepwise(VS_FRAME_MJPEG, 1, 1280, 720, 1 << 20, 333_333, 999_999, 333_333),
	]))
	.unwrap();
	let yuy2 = select(&normalized, 1, 1, 666_666).unwrap();
	assert_eq!((yuy2.kind, yuy2.width, yuy2.height, yuy2.stride, yuy2.max_bytes), (Kind::Yuy2, 640, 480, 1280, 640 * 480 * 2));
	assert_eq!(select(&normalized, 1, 1, 500_000), None, "an interval not offered");
	assert_eq!(select(&normalized, 1, 2, 333_333), None, "a size not offered");
	assert_eq!(select(&normalized, 3, 1, 333_333), None, "a format not offered");
	assert!(select(&normalized, 2, 1, 666_666).is_some(), "inside the range, on a step");
	assert_eq!(select(&normalized, 2, 1, 666_667), None, "inside the range, off a step");
	assert_eq!(select(&normalized, 2, 1, 1_333_332), None, "past the range");
	assert_eq!(select(&normalized, 2, 1, 333_333).map(|selection| selection.stride), Some(0), "encoded data has no stride");
}

#[test]
// THE COLOUR MATRIX IN UVC'S NUMBERING, which is not H.273's: B,G and SMPTE 170M are BT.601 and 1 is BT.709,
// while FCC, SMPTE 240M, a reserved value and "unspecified" are unknown - H.273's 5 and 6 are BT.601, UVC's
// are 240M and reserved.
fn the_colour_matrix_follows_the_uvc_code_points() {
	for (code, expected) in [
		(0, Matrix::Unknown),
		(1, Matrix::Bt709),
		(2, Matrix::Unknown),
		(3, Matrix::Bt601),
		(4, Matrix::Bt601),
		(5, Matrix::Unknown),
		(6, Matrix::Unknown),
		(255, Matrix::Unknown),
	] {
		let normalized = normalize(&graph(&[format_mjpeg(1, 1), frame(VS_FRAME_MJPEG, 1, 64, 64, 4096, &[333_333]), color(code)])).unwrap();
		assert_eq!(normalized.formats[0].matrix, expected, "bMatrixCoefficients {code}");
	}
}
