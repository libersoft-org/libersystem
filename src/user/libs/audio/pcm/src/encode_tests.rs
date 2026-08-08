use super::encode::*;
use alloc::vec;
use alloc::vec::Vec;

#[test]
fn a_sink_refuses_past_its_ceiling_and_patches_within_it() {
	let mut sink = VecSink::new(8);
	assert_eq!(sink.write(b"abcd"), Ok(()));
	assert_eq!(sink.written(), 4);
	assert_eq!(sink.write(b"efgh"), Ok(()));
	assert_eq!(sink.write(b"i"), Err(SinkError::Full));
	assert_eq!(sink.patch(2, b"XY"), Ok(()));
	// A patch past what was written is the encoder's bug, not the destination's, and is reported as
	// a failure rather than growing the file to meet it.
	assert_eq!(sink.patch(7, b"XY"), Err(SinkError::Failed));
	assert_eq!(sink.bytes(), b"abXYefgh");
}

#[test]
fn a_forward_only_sink_refuses_the_patch_and_takes_the_write() {
	let mut sink = ForwardOnly::new(VecSink::new(16));
	assert_eq!(sink.write(b"abcd"), Ok(()));
	assert_eq!(sink.patch(0, b"X"), Err(SinkError::Unseekable));
	assert_eq!(sink.into_inner().bytes(), b"abcd");
}

#[test]
fn remix_duplicates_mono_and_averages_stereo() {
	let up = Remix::new(1, 2).unwrap();
	let mut output = Vec::new();
	up.apply(&[100, -100], &mut output).unwrap();
	assert_eq!(output, vec![100, 100, -100, -100]);

	let down = Remix::new(2, 1).unwrap();
	output.clear();
	// The average is computed in i32: two full-scale samples of the same sign would wrap if it
	// were not, and the result would be a loud click at exactly the loudest moment.
	down.apply(&[i16::MIN, i16::MIN, 32767, 32767, 10, -11], &mut output).unwrap();
	assert_eq!(output, vec![i16::MIN, 32767, 0]);

	assert!(Remix::new(1, 1).unwrap().passthrough());
	assert!(Remix::new(3, 1).is_none());
	// Whole frames only.
	assert!(Remix::new(2, 1).unwrap().apply(&[1, 2, 3], &mut output).is_none());
}

#[test]
fn resampling_doubles_by_interpolating_between_the_frames_it_was_given() {
	let mut resample = Resample::new(24_000, 48_000, 1).unwrap();
	let mut output = Vec::new();
	resample.push(&[0, 100, 200], &mut output).unwrap();
	resample.finish(&mut output).unwrap();
	// Two output frames per input frame, the odd ones exactly halfway. The final frame is held
	// across the last interval, so the output covers the input's whole duration.
	assert_eq!(output, vec![0, 50, 100, 150, 200, 200]);
	assert_eq!(resample.output_frames(3), Some(6));
	assert_eq!(output.len(), 6);
}

#[test]
fn resampling_down_drops_the_frames_that_fall_between_outputs() {
	let mut resample = Resample::new(48_000, 24_000, 1).unwrap();
	let mut output = Vec::new();
	resample.push(&[0, 10, 20, 30, 40, 50], &mut output).unwrap();
	resample.finish(&mut output).unwrap();
	assert_eq!(resample.output_frames(6), Some(3));
	assert_eq!(output.len(), 3);
	assert_eq!(output, vec![0, 20, 40]);
}

#[test]
fn resampling_in_chunks_gives_the_same_samples_as_in_one() {
	// The whole point of carrying the interpolation state across pushes. A track converted in
	// whatever sizes the reader happens to hand over must be byte-identical to the same track
	// converted in one piece, or the output depends on the I/O pattern.
	let input: Vec<i16> = (0..401i32).map(|n| ((n * 977) % 4001 - 2000) as i16).collect();
	let mut whole = Vec::new();
	let mut one = Resample::new(32_000, 44_100, 1).unwrap();
	one.push(&input, &mut whole).unwrap();
	one.finish(&mut whole).unwrap();

	for chunk in [1usize, 2, 7, 64, 400] {
		let mut piecewise = Vec::new();
		let mut many = Resample::new(32_000, 44_100, 1).unwrap();
		for part in input.chunks(chunk) {
			many.push(part, &mut piecewise).unwrap();
		}
		many.finish(&mut piecewise).unwrap();
		assert_eq!(piecewise, whole, "chunk size {chunk} changed the output");
	}
	assert_eq!(one.output_frames(401).unwrap() as usize, whole.len());
}

#[test]
fn resampling_keeps_the_channels_apart_and_passes_equal_rates_through() {
	let mut resample = Resample::new(8_000, 16_000, 2).unwrap();
	let mut output = Vec::new();
	resample.push(&[0, 1000, 100, 1100], &mut output).unwrap();
	resample.finish(&mut output).unwrap();
	assert_eq!(output, vec![0, 1000, 50, 1050, 100, 1100, 100, 1100]);

	let mut same = Resample::new(48_000, 48_000, 2).unwrap();
	assert!(same.passthrough());
	output.clear();
	same.push(&[1, 2, 3, 4], &mut output).unwrap();
	same.finish(&mut output).unwrap();
	assert_eq!(output, vec![1, 2, 3, 4]);
	assert_eq!(same.output_frames(2), Some(2));

	// Rates `Format` will not name cannot be resampled to or from, so no conversion can produce a
	// file this tree's own decoders reject.
	assert!(Resample::new(48_000, 96_000, 1).is_none());
	assert!(Resample::new(1_000, 48_000, 1).is_none());
}
