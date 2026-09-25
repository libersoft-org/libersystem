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

// The function the harness's UVC emulator presents: a control interface with its header and two terminals, and a
// streaming interface whose alternate zero carries the bulk endpoint, an input header, one YUY2 format with one
// 64x48 frame, and its colour matching.
fn usb_camera(bulk: bool) -> Vec<u8> {
	use crate::descriptor;
	let mut out: Vec<u8> = alloc::vec![9, descriptor::DT_CONFIG, 0, 0, 2, 1, 0, 0x80, 50];
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 0, 0, 0, CLASS_VIDEO, SUBCLASS_CONTROL, 0, 0]);
	out.extend_from_slice(&[13, CS_INTERFACE, VC_HEADER, 0x10, 0x01, 40, 0, 0x80, 0x8d, 0x5b, 0x00, 1, 1]);
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 1, 0, u8::from(bulk), CLASS_VIDEO, SUBCLASS_STREAMING, 0, 0]);
	out.extend_from_slice(&[14, CS_INTERFACE, 0x01, 1, 77, 0, 0x81, 0, 2, 0, 0, 0, 1, 0]);
	let mut format = alloc::vec![27, CS_INTERFACE, VS_FORMAT_UNCOMPRESSED, 1, 1];
	format.extend_from_slice(&YUY2_GUID);
	format.extend_from_slice(&[16, 1, 0, 0, 0, 0]);
	out.extend_from_slice(&format);
	let mut frame = alloc::vec![30, CS_INTERFACE, VS_FRAME_UNCOMPRESSED, 1, 0, 64, 0, 48, 0];
	frame.extend_from_slice(&[0; 8]);
	frame.extend_from_slice(&6144u32.to_le_bytes());
	frame.extend_from_slice(&333_333u32.to_le_bytes());
	frame.push(1);
	frame.extend_from_slice(&333_333u32.to_le_bytes());
	out.extend_from_slice(&frame);
	out.extend_from_slice(&[6, CS_INTERFACE, VS_COLORFORMAT, 1, 1, 4]);
	if bulk {
		out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x81, 0x02, 0x00, 0x02, 0]);
	}
	let total = out.len() as u16;
	out[2..4].copy_from_slice(&total.to_le_bytes());
	out
}

#[test]
fn a_bulk_streaming_camera_binds_and_its_graph_normalizes() {
	let bound = bind(&usb_camera(true)).expect("a bulk camera binds");
	assert_eq!((bound.control_interface, bound.streaming_interface, bound.bulk_in.address, bound.version, bound.probe_length()), (0, 1, 0x81, 0x0110, 34));
	let normalized = normalize(&bound.graph).expect("the streaming records normalize, header and all");
	assert_eq!(normalized.formats.len(), 1);
	assert_eq!((normalized.formats[0].sizes[0].width, normalized.formats[0].sizes[0].height), (64, 48));
}

#[test]
fn an_isochronous_camera_is_refused_by_name() {
	assert_eq!(bind(&usb_camera(false)), Err(NotBindable::Isochronous));
}

#[test]
fn a_probe_carries_the_selection_and_its_answer_the_sizes() {
	let request = probe(1, 1, 333_333, 34);
	assert_eq!((request.len(), request[2], request[3]), (34, 1, 1));
	let mut answer = request.clone();
	answer[18..22].copy_from_slice(&6144u32.to_le_bytes());
	answer[22..26].copy_from_slice(&4096u32.to_le_bytes());
	assert_eq!(probed(&answer), Some(Probed { format: 1, frame: 1, interval: 333_333, max_frame: 6144, max_payload: 4096 }));
	assert_eq!(probed(&answer[..20]), None);
}

#[test]
fn a_payload_header_is_believed_only_as_far_as_its_length() {
	assert_eq!(payload(&[2, 0x83, 9, 9]), Some(Payload { fid: true, eof: true, err: false, data: 2 }));
	assert_eq!(payload(&[12, 0x40]), None, "a header longer than its transfer");
	assert_eq!(payload(&[1, 0]), None, "a header shorter than itself");
	assert!(payload(&[2, 0xc0]).unwrap().err);
}

// THE CONSUMER'S BUFFERS, as the class module keeps them: `open` takes the next queued one, and a frame's bytes
// are whatever was written into it by the time it ended.
struct Buffers {
	queued: Vec<usize>,
	open: Option<Vec<u8>>,
	ended: Vec<(Assembled, Vec<u8>)>,
}

impl Buffers {
	fn queued(capacities: &[usize]) -> Self {
		Buffers { queued: capacities.to_vec(), open: None, ended: Vec::new() }
	}
}

impl FrameSink for Buffers {
	fn open(&mut self) -> Option<u64> {
		if self.queued.is_empty() {
			return None;
		}
		let capacity = self.queued.remove(0);
		self.open = Some(alloc::vec![0; capacity]);
		Some(capacity as u64)
	}

	fn write(&mut self, offset: u32, data: &[u8]) {
		let buffer = self.open.as_mut().expect("a write only into an open buffer");
		let end = offset as usize + data.len();
		assert!(end <= buffer.len(), "a write past the capacity `open` returned: {end} of {}", buffer.len());
		buffer[offset as usize..end].copy_from_slice(data);
	}

	fn finish(&mut self, frame: Assembled) {
		let bytes = self.open.take().map(|buffer| buffer[..frame.written as usize].to_vec()).unwrap_or_default();
		self.ended.push((frame, bytes));
	}
}

// One payload: a two-byte header with its bits, then the data.
fn part(fid: bool, eof: bool, err: bool, data: &[u8]) -> Vec<u8> {
	let mut info = 0x80;
	if fid {
		info |= HEADER_FID;
	}
	if eof {
		info |= HEADER_EOF;
	}
	if err {
		info |= HEADER_ERR;
	}
	let mut out = alloc::vec![2, info];
	out.extend_from_slice(data);
	out
}

fn feed(assembler: &mut Assembler, buffers: &mut Buffers, parts: &[Vec<u8>]) {
	for transfer in parts {
		assembler.feed(transfer, 64, buffers);
	}
}

#[test]
fn frames_are_assembled_from_their_payloads_and_ended_by_their_eof() {
	let mut assembler = Assembler::new();
	let mut buffers = Buffers::queued(&[16, 16]);
	feed(&mut assembler, &mut buffers, &[part(false, false, false, &[1, 2, 3]), part(false, true, false, &[4, 5]), part(true, false, false, &[6]), part(true, true, false, &[7])]);
	assert_eq!(buffers.ended.len(), 2);
	assert_eq!(buffers.ended[0], (Assembled { buffered: true, written: 5, bad: false, eof: true }, alloc::vec![1, 2, 3, 4, 5]));
	assert_eq!(buffers.ended[1].1, alloc::vec![6, 7]);
	assert!(buffers.ended[0].0.whole(Some(5)) && buffers.ended[0].0.whole(None));
	assert!(!buffers.ended[0].0.whole(Some(6)), "a frame of a fixed-length format is whole only at that length");
	assert!(!assembler.assembling());
}

#[test]
fn a_fid_that_changes_before_an_eof_ends_the_frame_it_interrupts() {
	let mut assembler = Assembler::new();
	let mut buffers = Buffers::queued(&[16, 16]);
	feed(&mut assembler, &mut buffers, &[part(false, false, false, &[1, 2]), part(true, false, false, &[3]), part(true, true, false, &[4])]);
	let (interrupted, bytes) = &buffers.ended[0];
	assert_eq!((*interrupted, bytes.as_slice()), (Assembled { buffered: true, written: 2, bad: false, eof: false }, &[1, 2][..]));
	assert!(interrupted.whole(Some(2)), "a device that does not set EOF still delivers frames of a fixed length");
	assert!(!interrupted.whole(None), "but a compressed frame no EOF ended may have lost its last payload");
	assert_eq!(buffers.ended[1].1, alloc::vec![3, 4], "and the payload that interrupted it starts the next");
}

#[test]
fn after_a_frame_ends_its_fid_starts_nothing_until_it_toggles() {
	let mut assembler = Assembler::new();
	let mut buffers = Buffers::queued(&[16, 16]);
	// A device out of step with its own frames: more of frame 0's FID after its EOF.
	feed(&mut assembler, &mut buffers, &[part(false, true, false, &[1]), part(false, false, false, &[9, 9]), part(false, true, false, &[9]), part(true, true, false, &[2])]);
	assert_eq!(buffers.ended.len(), 2, "the stale payloads opened no frame: {:?}", buffers.ended);
	assert_eq!((buffers.ended[0].1.as_slice(), buffers.ended[1].1.as_slice()), (&[1][..], &[2][..]));
	assert!(buffers.queued.is_empty() && buffers.ended.iter().all(|(frame, _)| frame.buffered), "and took no buffer");
}

#[test]
fn the_devices_error_bit_loses_the_frame_and_nothing_more_of_it_is_written() {
	let mut assembler = Assembler::new();
	let mut buffers = Buffers::queued(&[16, 16]);
	feed(&mut assembler, &mut buffers, &[part(false, false, false, &[1, 2]), part(false, false, true, &[3, 4]), part(false, true, false, &[5]), part(true, true, false, &[6])]);
	assert_eq!(buffers.ended[0], (Assembled { buffered: true, written: 2, bad: true, eof: true }, alloc::vec![1, 2]));
	assert!(!buffers.ended[0].0.whole(None) && !buffers.ended[0].0.whole(Some(2)));
	assert!(buffers.ended[1].0.whole(Some(1)), "and the frame after it is unaffected");
}

#[test]
fn a_header_that_is_not_one_loses_the_open_frame_and_opens_nothing() {
	let mut assembler = Assembler::new();
	let mut buffers = Buffers::queued(&[16]);
	for broken in [alloc::vec![], alloc::vec![0u8], alloc::vec![1, 0x80], alloc::vec![12, 0x80, 1]] {
		assembler.feed(&broken, 64, &mut buffers);
	}
	assert!(!assembler.assembling() && buffers.queued.len() == 1, "nothing was opened by a header that is not one");
	feed(&mut assembler, &mut buffers, &[part(false, false, false, &[1]), alloc::vec![12, 0x80, 1], part(false, true, false, &[2])]);
	assert_eq!(buffers.ended, alloc::vec![(Assembled { buffered: true, written: 1, bad: true, eof: true }, alloc::vec![1])]);
}

#[test]
fn data_past_the_buffer_or_past_a_frame_is_lost_and_never_written() {
	let mut assembler = Assembler::new();
	let mut buffers = Buffers::queued(&[4, 64]);
	feed(&mut assembler, &mut buffers, &[part(false, false, false, &[1, 2, 3]), part(false, true, false, &[4, 5])]);
	assert_eq!(buffers.ended[0], (Assembled { buffered: true, written: 3, bad: true, eof: true }, alloc::vec![1, 2, 3]), "past its buffer");
	// A buffer larger than a frame may be: the limit holds.
	assembler.feed(&part(true, false, false, &[0; 60]), 64, &mut buffers);
	assembler.feed(&part(true, true, false, &[0; 5]), 64, &mut buffers);
	assert_eq!((buffers.ended[1].0.written, buffers.ended[1].0.bad), (60, true), "past the most a frame may be");
}

#[test]
fn a_frame_with_no_buffer_is_assembled_to_its_end_unbuffered() {
	let mut assembler = Assembler::new();
	let mut buffers = Buffers::queued(&[]);
	feed(&mut assembler, &mut buffers, &[part(false, false, false, &[1]), part(false, true, false, &[2])]);
	assert_eq!(buffers.ended[0].0, Assembled { buffered: false, written: 0, bad: false, eof: true });
	assert!(!buffers.ended[0].0.whole(None));
	buffers.queued.push(8);
	feed(&mut assembler, &mut buffers, &[part(true, true, false, &[3])]);
	assert!(buffers.ended[1].0.whole(Some(1)), "a buffer queued since is the next frame's");
}

#[test]
fn an_eof_with_no_data_ends_the_frame() {
	let mut assembler = Assembler::new();
	let mut buffers = Buffers::queued(&[8]);
	feed(&mut assembler, &mut buffers, &[part(false, false, false, &[1, 2]), part(false, true, false, &[])]);
	assert_eq!(buffers.ended[0].0, Assembled { buffered: true, written: 2, bad: false, eof: true });
}
