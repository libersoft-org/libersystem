//! The encoder contract: where decoded audio goes when it is going back out.
//!
//! A decoder in this tree hands out canonical signed-i16 interleaved frames and says what rate and
//! how many channels they are. Encoding is that read backwards, and the whole of the contract is
//! three pieces that stay separate on purpose:
//!
//! - a `Sink`, which is somewhere bytes go and which is allowed to fail;
//! - `Remix` and `Resample`, the two transforms every codec would otherwise write for itself;
//! - the codecs, which stay in their own leaves and are not here.
//!
//! What is deliberately absent is a place to put a whole track. Every encoder in this tree takes
//! frames in bounded pushes and writes them out as it goes, because the machine this runs on may
//! have a few megabytes and the file may be an hour long. The one thing a container legitimately
//! cannot know until the end - how long it turned out to be - is handled by `patch`, not by keeping
//! the audio around until the answer is known.

use crate::Format;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkError {
	// The destination will not take any more. Distinct from `Failed`, because running out of room
	// is an ordinary outcome a caller may want to report differently from a broken disk.
	Full,
	// A patch was asked of a sink that only goes forward.
	//
	// This is not a defect in either side. An encoder whose container carries its own length in a
	// header says so by returning this, and a caller that gets it knows to stage the output
	// somewhere it can seek instead of discovering a wrong length in a finished file.
	Unseekable,
	// The destination failed.
	Failed,
}

// Somewhere encoded bytes go.
//
// `write` appends. `patch` goes back and overwrites bytes that were already written, which is how a
// RIFF or FORM size gets its real value: the header goes out with a placeholder, the audio streams
// past, and the two words that could not be known in advance are corrected at the end.
pub trait Sink {
	fn write(&mut self, bytes: &[u8]) -> Result<(), SinkError>;

	// Overwrite `bytes.len()` bytes at `at`. The default refuses, so a sink that only goes forward
	// is written by implementing one method and gets an honest error rather than silent corruption.
	fn patch(&mut self, at: u64, bytes: &[u8]) -> Result<(), SinkError> {
		let _ = (at, bytes);
		Err(SinkError::Unseekable)
	}

	// How many bytes have been written. Encoders use this to remember where a placeholder is.
	fn written(&self) -> u64;
}

// A sink over memory, with a ceiling.
//
// The ceiling is not decoration. This is what the host tests encode into and what a caller uses to
// stage a small output, and an encoder handed a hostile frame count should hit a bound rather than
// take the machine down with it.
pub struct VecSink {
	bytes: Vec<u8>,
	ceiling: u64,
}

impl VecSink {
	pub const fn new(ceiling: u64) -> VecSink {
		VecSink { bytes: Vec::new(), ceiling }
	}

	pub fn into_bytes(self) -> Vec<u8> {
		self.bytes
	}

	pub fn bytes(&self) -> &[u8] {
		&self.bytes
	}
}

impl Sink for VecSink {
	fn write(&mut self, bytes: &[u8]) -> Result<(), SinkError> {
		let end = (self.bytes.len() as u64).checked_add(bytes.len() as u64).ok_or(SinkError::Full)?;
		if end > self.ceiling {
			return Err(SinkError::Full);
		}
		self.bytes.try_reserve(bytes.len()).map_err(|_| SinkError::Full)?;
		self.bytes.extend_from_slice(bytes);
		Ok(())
	}

	fn patch(&mut self, at: u64, bytes: &[u8]) -> Result<(), SinkError> {
		let start = usize::try_from(at).map_err(|_| SinkError::Failed)?;
		let end = start.checked_add(bytes.len()).ok_or(SinkError::Failed)?;
		// A patch past what was written is a bug in the encoder, not a short destination: it means
		// the offset it remembered is not the offset it wrote to.
		let target = self.bytes.get_mut(start..end).ok_or(SinkError::Failed)?;
		target.copy_from_slice(bytes);
		Ok(())
	}

	fn written(&self) -> u64 {
		self.bytes.len() as u64
	}
}

// A sink that only goes forward, wrapping one that does not.
//
// Its purpose is to be refused. An encoder that needs to correct a header must say so when it is
// built rather than when it finishes, and this is what the tests use to prove it does.
pub struct ForwardOnly<S: Sink> {
	inner: S,
}

impl<S: Sink> ForwardOnly<S> {
	pub const fn new(inner: S) -> ForwardOnly<S> {
		ForwardOnly { inner }
	}

	pub fn into_inner(self) -> S {
		self.inner
	}
}

impl<S: Sink> Sink for ForwardOnly<S> {
	fn write(&mut self, bytes: &[u8]) -> Result<(), SinkError> {
		self.inner.write(bytes)
	}

	fn written(&self) -> u64 {
		self.inner.written()
	}
}

// Mono to stereo and back, and nothing else, because nothing else exists in `Format`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Remix {
	from: u8,
	to: u8,
}

impl Remix {
	pub fn new(from: u8, to: u8) -> Option<Remix> {
		if !(1..=2).contains(&from) || !(1..=2).contains(&to) {
			return None;
		}
		Some(Remix { from, to })
	}

	pub const fn passthrough(self) -> bool {
		self.from == self.to
	}

	pub const fn output_frames(self, input_frames: u64) -> u64 {
		input_frames
	}

	// Append the remixed frames. `input` is interleaved at `from` channels and must be whole frames.
	//
	// Stereo to mono is the average, rounded toward zero, computed in i32 so that two full-scale
	// samples of the same sign do not wrap on the way to being halved.
	pub fn apply(self, input: &[i16], output: &mut Vec<i16>) -> Option<()> {
		if input.len() % self.from as usize != 0 {
			return None;
		}
		let frames = input.len() / self.from as usize;
		let wanted = frames.checked_mul(self.to as usize)?;
		output.try_reserve(wanted).ok()?;
		match (self.from, self.to) {
			(1, 2) => {
				for &sample in input {
					output.push(sample);
					output.push(sample);
				}
			}
			(2, 1) => {
				for frame in input.chunks_exact(2) {
					output.push(((frame[0] as i32 + frame[1] as i32) / 2) as i16);
				}
			}
			_ => output.extend_from_slice(input),
		}
		Some(())
	}
}

// Rate conversion, streaming, by linear interpolation in integer arithmetic.
//
// Linear rather than the nearest-neighbour hold that `Format::advance` does for playback: playback
// is throwing samples at a device that is about to mix them anyway, and conversion is writing a
// file somebody keeps. The arithmetic is integer throughout so that the same input gives the same
// output on all three architectures - a conversion whose result depends on the floating-point unit
// is one whose test cannot say what it expects.
//
// Streaming means the interpolation carries across pushes: the last frame of one push is the left
// endpoint for the first output frame of the next, so a track converted in ten-thousand-frame
// chunks is bit-identical to the same track converted in one.
pub struct Resample {
	from: u32,
	to: u32,
	channels: u8,
	// The previous input frame, the left endpoint of the interval being interpolated across.
	held: [i16; 2],
	primed: bool,
	// Where the next output frame falls inside that interval, in units of 1/`to` of an input frame.
	// Kept relative to `held` rather than absolute so that an hour of audio does not overflow it.
	offset: u64,
}

impl Resample {
	pub fn new(from: u32, to: u32, channels: u8) -> Option<Resample> {
		// The bounds are `Format`'s, so a resampler cannot be built for a rate no `Format` can name
		// and then produce a file this tree's own decoders reject.
		Format::new(from, channels)?;
		Format::new(to, channels)?;
		Some(Resample { from, to, channels, held: [0; 2], primed: false, offset: 0 })
	}

	pub const fn passthrough(&self) -> bool {
		self.from == self.to
	}

	// How many frames `input_frames` will produce, counting the flush.
	//
	// The rule is `ceil(input_frames * to / from)`: every output frame whose position falls strictly
	// inside the input's duration, and the last input frame held for the tail of the final interval.
	// Stated here because a caller writing a container header needs the number before the audio, and
	// because a test that cannot say the count in advance is not testing the count.
	pub fn output_frames(&self, input_frames: u64) -> Option<u64> {
		if input_frames == 0 {
			return Some(0);
		}
		let scaled = input_frames.checked_mul(self.to as u64)?;
		let from = self.from as u64;
		Some(scaled / from + u64::from(scaled % from != 0))
	}

	// Append the frames this push produces. `input` is interleaved at `channels` and whole frames.
	pub fn push(&mut self, input: &[i16], output: &mut Vec<i16>) -> Option<()> {
		let channels = self.channels as usize;
		if input.len() % channels != 0 {
			return None;
		}
		if self.passthrough() {
			output.try_reserve(input.len()).ok()?;
			output.extend_from_slice(input);
			return Some(());
		}
		for frame in input.chunks_exact(channels) {
			let mut current = [0i16; 2];
			current[..channels].copy_from_slice(frame);
			if !self.primed {
				self.held = current;
				self.primed = true;
				continue;
			}
			self.interpolate_into(current, output)?;
		}
		Some(())
	}

	// The tail: the final input frame held across the last interval, so the output covers the whole
	// duration of the input rather than stopping one interval short of it.
	pub fn finish(&mut self, output: &mut Vec<i16>) -> Option<()> {
		if self.passthrough() || !self.primed {
			return Some(());
		}
		let held = self.held;
		self.interpolate_into(held, output)?;
		self.primed = false;
		Some(())
	}

	// Emit every output frame falling in [held, current), then make `current` the new left endpoint.
	//
	// The loop leaves `offset` at or past `to`, which is what makes the subtraction below always
	// valid - including when downsampling, where the loop can decline to run at all.
	fn interpolate_into(&mut self, current: [i16; 2], output: &mut Vec<i16>) -> Option<()> {
		let channels = self.channels as usize;
		let to = self.to as u64;
		while self.offset < to {
			output.try_reserve(channels).ok()?;
			for channel in 0..channels {
				let left = self.held[channel] as i64;
				let right = current[channel] as i64;
				let weight = self.offset as i64;
				let span = to as i64;
				output.push((((span - weight) * left + weight * right) / span) as i16);
			}
			self.offset += self.from as u64;
		}
		self.offset -= to;
		self.held = current;
		Some(())
	}
}
