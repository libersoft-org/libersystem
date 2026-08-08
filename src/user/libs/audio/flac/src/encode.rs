//! Writing native FLAC, incrementally, in the profile this leaf can read.
//!
//! Lossless, so the assertion a test can make here is the strongest one available: the samples that
//! come back are the samples that went in, bit for bit, through this tree's own decoder.
//!
//! The compression is fixed-order linear prediction with Rice-coded residuals - orders zero through
//! four, the constant and verbatim subframes, stereo decorrelation, and a partition search over the
//! residual. General LPC, where the encoder solves for its own coefficients, is not here: the
//! container and the frame layout are identical either way, so it is a ratio improvement that fits
//! behind this interface rather than a different format. What is here is chosen by MEASURING -
//! every candidate is costed in bits and the cheapest is written - so the effort setting bounds how
//! many candidates are tried rather than guessing which one wins.
//!
//! Nothing here keeps the track. One block is held, encoded, and written.

use crate::{Error, crc8, crc16};
use alloc::vec::Vec;
use pcm::Format;
use pcm::encode::{Sink, SinkError};

// The block size every encoder in the wild uses, and the smallest one FLAC's metadata can describe.
const BLOCK: usize = 4096;
const MIN_BLOCK: usize = 16;
// A Rice parameter of fifteen is the escape code, so fourteen is the largest that means itself.
const MAX_RICE: u32 = 14;
// Sixteen partitions over a 4096-sample block is 256 residuals each, which is about where the gain
// from splitting stops paying for the four bits per partition it costs.
const MAX_PARTITION_ORDER: u32 = 4;
const MAX_PARTITIONS: usize = 1 << MAX_PARTITION_ORDER;
// Input is signed sixteen-bit, which is what every decoder in this tree hands out.
const SAMPLE_BITS: u8 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
	Unsupported,
	Invalid,
	TooLarge,
	Destination(SinkError),
}

impl From<SinkError> for EncodeError {
	fn from(error: SinkError) -> EncodeError {
		EncodeError::Destination(error)
	}
}

impl From<Error> for EncodeError {
	fn from(error: Error) -> EncodeError {
		match error {
			Error::Unsupported => EncodeError::Unsupported,
			Error::TooLarge => EncodeError::TooLarge,
			_ => EncodeError::Invalid,
		}
	}
}

// How hard to look for a smaller frame. Every level writes a correct file and they all decode to the
// same samples; they differ only in how many candidates are costed before one is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effort {
	// One fixed order, one Rice partition, channels left alone. What a slow machine manages in real
	// time on a stream it is also decoding.
	Fast,
	// Every fixed order, a partition search, and stereo decorrelation. The default.
	Balanced,
	// The same candidates with the deepest partition search.
	Thorough,
}

impl Effort {
	// The normalised 0..100 a conversion tool exposes, mapped onto what actually changes here rather
	// than onto a hundred settings that do not exist.
	pub const fn from_percent(percent: u8) -> Effort {
		match percent {
			0..=32 => Effort::Fast,
			33..=74 => Effort::Balanced,
			_ => Effort::Thorough,
		}
	}

	const fn orders(self) -> &'static [usize] {
		match self {
			Effort::Fast => &[2],
			_ => &[0, 1, 2, 3, 4],
		}
	}

	const fn partition_order(self) -> u32 {
		match self {
			Effort::Fast => 0,
			Effort::Balanced => 3,
			Effort::Thorough => MAX_PARTITION_ORDER,
		}
	}

	const fn decorrelates(self) -> bool {
		!matches!(self, Effort::Fast)
	}
}

pub struct Encoder<S: Sink> {
	sink: S,
	format: Format,
	effort: Effort,
	streaminfo_at: u64,
	pending: Vec<i16>,
	frames: u64,
	frame_number: u64,
	min_block: usize,
	max_block: usize,
	min_frame: u32,
	max_frame: u32,
	// Scratch, reused between blocks so a long track allocates once and then stops. `mid` and
	// `side` are fields for that reason and no other: the decorrelated channels are SWAPPED into
	// place rather than assigned, so the buffer they displace comes back here to be filled again.
	// Assigning would drop it and take a fresh one every block, which is a per-block allocation on
	// a path whose whole claim is that it does not have one.
	left: Vec<i32>,
	right: Vec<i32>,
	mid: Vec<i32>,
	side: Vec<i32>,
	residual: Vec<i32>,
	writer: BitWriter,
}

impl<S: Sink> Encoder<S> {
	pub fn new(mut sink: S, format: Format, effort: Effort) -> Result<Encoder<S>, EncodeError> {
		sink.patch(0, &[])?;
		sink.write(b"fLaC")?;
		// STREAMINFO, last and only metadata block. Everything in it that is not known yet goes out
		// as zero and is corrected at `finish` - the whole block rewritten at once, because five
		// patches at five remembered offsets is five chances to be off by one.
		sink.write(&[0x80, 0, 0, 34])?;
		let streaminfo_at = sink.written();
		sink.write(&[0u8; 34])?;
		Ok(Encoder { sink, format, effort, streaminfo_at, pending: Vec::new(), frames: 0, frame_number: 0, min_block: usize::MAX, max_block: 0, min_frame: u32::MAX, max_frame: 0, left: Vec::new(), right: Vec::new(), mid: Vec::new(), side: Vec::new(), residual: Vec::new(), writer: BitWriter::new() })
	}

	pub fn push(&mut self, interleaved: &[i16]) -> Result<(), EncodeError> {
		let channels = self.format.channels() as usize;
		if interleaved.len() % channels != 0 {
			return Err(EncodeError::Invalid);
		}
		self.frames = self.frames.checked_add((interleaved.len() / channels) as u64).ok_or(EncodeError::TooLarge)?;
		self.pending.try_reserve(interleaved.len()).map_err(|_| EncodeError::TooLarge)?;
		self.pending.extend_from_slice(interleaved);
		// Held back to `BLOCK + MIN_BLOCK` rather than emitted greedily: FLAC's smallest legal block
		// is sixteen frames, and an encoder that empties itself at every opportunity can be left
		// holding three at the end with nowhere to put them.
		while self.pending.len() >= (BLOCK + MIN_BLOCK) * channels {
			self.emit(BLOCK)?;
		}
		Ok(())
	}

	pub fn finish(mut self) -> Result<(S, u64), EncodeError> {
		let channels = self.format.channels() as usize;
		// Shorter than one legal block and FLAC cannot describe it - STREAMINFO's minimum block size
		// is sixteen frames. Said out loud rather than written as a file whose header contradicts it.
		if self.frames < MIN_BLOCK as u64 {
			return Err(EncodeError::Invalid);
		}
		while !self.pending.is_empty() {
			let held = self.pending.len() / channels;
			let take = if held <= BLOCK { held } else { core::cmp::min(BLOCK, held - MIN_BLOCK) };
			self.emit(take)?;
		}

		let mut info = [0u8; 34];
		info[..2].copy_from_slice(&(self.min_block as u16).to_be_bytes());
		info[2..4].copy_from_slice(&(self.max_block as u16).to_be_bytes());
		info[4..7].copy_from_slice(&self.min_frame.to_be_bytes()[1..]);
		info[7..10].copy_from_slice(&self.max_frame.to_be_bytes()[1..]);
		let packed = ((self.format.rate() as u64) << 44) | (((channels as u64) - 1) << 41) | (((SAMPLE_BITS as u64) - 1) << 36) | (self.frames & 0x0f_ffff_ffff);
		info[10..18].copy_from_slice(&packed.to_be_bytes());
		// The MD5 of the unencoded audio stays zero, which the format defines as "not computed"
		// rather than as a wrong answer. Producing one means carrying a digest across the whole
		// track, which is affordable; claiming a digest nothing was checked against would be worse
		// than admitting there is none.
		self.sink.patch(self.streaminfo_at, &info)?;
		Ok((self.sink, self.frames))
	}

	fn emit(&mut self, block_size: usize) -> Result<(), EncodeError> {
		let channels = self.format.channels() as usize;
		let taken = block_size.checked_mul(channels).ok_or(EncodeError::TooLarge)?;
		if block_size == 0 || taken > self.pending.len() {
			return Err(EncodeError::Invalid);
		}
		self.left.clear();
		self.right.clear();
		self.left.try_reserve(block_size).map_err(|_| EncodeError::TooLarge)?;
		self.right.try_reserve(block_size).map_err(|_| EncodeError::TooLarge)?;
		for frame in 0..block_size {
			self.left.push(self.pending[frame * channels] as i32);
			if channels == 2 {
				self.right.push(self.pending[frame * channels + 1] as i32);
			}
		}

		// Which stereo layout to write. Independent channels cost what they cost; the three
		// decorrelated ones each replace a channel with a difference that is usually much smaller,
		// and which of them wins depends on the material rather than on a rule - so all four are
		// costed and the cheapest is kept.
		let assignment = if channels == 1 { 0u8 } else { self.choose_stereo(block_size) };

		self.writer.reset();
		self.write_frame_header(block_size, assignment)?;
		let widths = subframe_widths(assignment);
		for (index, width) in widths.iter().take(channels).enumerate() {
			let samples = if index == 0 { core::mem::take(&mut self.left) } else { core::mem::take(&mut self.right) };
			let outcome = write_subframe(&mut self.writer, &samples, *width, self.effort, &mut self.residual);
			if index == 0 {
				self.left = samples;
			} else {
				self.right = samples;
			}
			outcome?;
		}
		self.writer.align_zero();
		let crc = crc16(&self.writer.bytes);
		self.writer.bytes.extend_from_slice(&crc.to_be_bytes());

		let size = u32::try_from(self.writer.bytes.len()).map_err(|_| EncodeError::TooLarge)?;
		self.min_frame = self.min_frame.min(size);
		self.max_frame = self.max_frame.max(size);
		self.min_block = self.min_block.min(block_size);
		self.max_block = self.max_block.max(block_size);
		self.sink.write(&self.writer.bytes)?;
		self.pending.drain(..taken);
		self.frame_number += 1;
		Ok(())
	}

	// Cost the four stereo layouts, leave the two channels the winner needs in `left` and `right`.
	fn choose_stereo(&mut self, block_size: usize) -> u8 {
		if !self.effort.decorrelates() {
			return 1;
		}
		// Mid is the average and side the difference. The decoder rebuilds both exactly, because the
		// parity it needs to undo the halving is carried in the low bit of the difference.
		self.mid.clear();
		self.side.clear();
		if self.mid.try_reserve(block_size).is_err() || self.side.try_reserve(block_size).is_err() {
			return 1;
		}
		for frame in 0..block_size {
			let left = self.left[frame] as i64;
			let right = self.right[frame] as i64;
			self.mid.push(((left + right) >> 1) as i32);
			self.side.push((left - right) as i32);
		}

		// The scratch is moved out so the costing can borrow it while it reads the channels.
		let mut residual = core::mem::take(&mut self.residual);
		let left_cost = plan_for(&self.left, SAMPLE_BITS, self.effort, &mut residual).cost;
		let right_cost = plan_for(&self.right, SAMPLE_BITS, self.effort, &mut residual).cost;
		let mid_cost = plan_for(&self.mid, SAMPLE_BITS, self.effort, &mut residual).cost;
		let side_cost = plan_for(&self.side, SAMPLE_BITS + 1, self.effort, &mut residual).cost;
		self.residual = residual;

		// Ties go to the earlier - and simpler - layout, so the choice is reproducible.
		let mut best = (1u8, left_cost + right_cost);
		for candidate in [(8u8, left_cost + side_cost), (9, side_cost + right_cost), (10, mid_cost + side_cost)] {
			if candidate.1 < best.1 {
				best = candidate;
			}
		}
		match best.0 {
			// Left and side.
			8 => core::mem::swap(&mut self.right, &mut self.side),
			// Side and right.
			9 => core::mem::swap(&mut self.left, &mut self.side),
			// Mid and side.
			10 => {
				core::mem::swap(&mut self.left, &mut self.mid);
				core::mem::swap(&mut self.right, &mut self.side);
			}
			_ => {}
		}
		best.0
	}

	fn write_frame_header(&mut self, block_size: usize, assignment: u8) -> Result<(), EncodeError> {
		// The header is whole bytes, so it is built directly and then run through CRC-8 - which is
		// what the format asks for, and what makes the checksum a check rather than a copy of the
		// bit writer's arithmetic.
		let block_code: u8 = if block_size == BLOCK { 12 } else { 7 };
		let number = self.frame_number;
		let header = &mut self.writer.bytes;
		header.push(0xff);
		// No reserved bit set, fixed-size blocking - which makes the number below the frame's index
		// rather than the index of its first sample.
		header.push(0xf8);
		// Rate code zero: take it from STREAMINFO. Every rate `Format` can name is already there, so
		// naming it twice would only be a second place for the two to disagree.
		header.push(block_code << 4);
		// Bits code four is sixteen; the low bit is reserved and stays zero.
		header.push((assignment << 4) | (4 << 1));
		write_utf8_number(header, number);
		if block_code == 7 {
			let encoded = u16::try_from(block_size - 1).map_err(|_| EncodeError::TooLarge)?;
			header.extend_from_slice(&encoded.to_be_bytes());
		}
		let checksum = crc8(header);
		header.push(checksum);
		Ok(())
	}
}

// The bit width of each subframe under a stereo assignment: the decorrelated channel carries a
// difference of two samples, which needs one bit more than either of them.
const fn subframe_widths(assignment: u8) -> [u8; 2] {
	match assignment {
		8 => [SAMPLE_BITS, SAMPLE_BITS + 1],
		9 => [SAMPLE_BITS + 1, SAMPLE_BITS],
		10 => [SAMPLE_BITS, SAMPLE_BITS + 1],
		_ => [SAMPLE_BITS, SAMPLE_BITS],
	}
}

#[derive(Clone, Copy)]
enum Shape {
	Constant,
	Verbatim,
	Fixed { order: usize, partition_order: u32, parameters: [u8; MAX_PARTITIONS] },
}

struct Plan {
	shape: Shape,
	cost: u64,
}

// Cost every candidate for one channel and keep the cheapest.
//
// The costs are exact rather than estimated - each is the number of bits that candidate would write
// - so "cheapest" means smallest file, not smallest guess.
fn plan_for(samples: &[i32], bits: u8, effort: Effort, residual: &mut Vec<i32>) -> Plan {
	let block_size = samples.len();
	let header = 8u64;
	let mut best = Plan { shape: Shape::Verbatim, cost: header + block_size as u64 * bits as u64 };

	if !samples.is_empty() && samples.iter().all(|&sample| sample == samples[0]) {
		let cost = header + bits as u64;
		if cost < best.cost {
			best = Plan { shape: Shape::Constant, cost };
		}
	}

	for &order in effort.orders() {
		if order >= block_size {
			continue;
		}
		if fixed_residual(samples, order, residual).is_err() {
			continue;
		}
		let mut chosen: Option<(u32, [u8; MAX_PARTITIONS], u64)> = None;
		for partition_order in 0..=effort.partition_order() {
			let partitions = 1usize << partition_order;
			if block_size % partitions != 0 {
				continue;
			}
			let partition_size = block_size / partitions;
			// The first partition gives up its opening samples to the warm-up, so it must have some
			// left; the decoder rejects the frame otherwise.
			if partition_size <= order {
				continue;
			}
			let mut parameters = [0u8; MAX_PARTITIONS];
			let mut total = 2 + 4 + partitions as u64 * 4;
			let mut usable = true;
			let mut consumed = 0usize;
			for partition in 0..partitions {
				let count = if partition == 0 { partition_size - order } else { partition_size };
				let values = &residual[consumed..consumed + count];
				consumed += count;
				match best_parameter(values) {
					Some((parameter, cost)) => {
						parameters[partition] = parameter as u8;
						total += cost;
					}
					None => {
						usable = false;
						break;
					}
				}
			}
			if usable && chosen.as_ref().is_none_or(|(_, _, cost)| total < *cost) {
				chosen = Some((partition_order, parameters, total));
			}
		}
		if let Some((partition_order, parameters, residual_cost)) = chosen {
			let cost = header + order as u64 * bits as u64 + residual_cost;
			if cost < best.cost {
				best = Plan { shape: Shape::Fixed { order, partition_order, parameters }, cost };
			}
		}
	}
	best
}

// The residual of a fixed predictor of the given order, in i64 so the intermediate cannot wrap and
// checked back into i32 so a signal that would not fit is declined rather than written wrong.
fn fixed_residual(samples: &[i32], order: usize, residual: &mut Vec<i32>) -> Result<(), EncodeError> {
	residual.clear();
	residual.try_reserve(samples.len().saturating_sub(order)).map_err(|_| EncodeError::TooLarge)?;
	for index in order..samples.len() {
		let at = |back: usize| samples[index - back] as i64;
		let prediction = match order {
			0 => 0,
			1 => at(1),
			2 => 2 * at(1) - at(2),
			3 => 3 * at(1) - 3 * at(2) + at(3),
			4 => 4 * at(1) - 6 * at(2) + 4 * at(3) - at(4),
			_ => return Err(EncodeError::Invalid),
		};
		let value = samples[index] as i64 - prediction;
		residual.push(i32::try_from(value).map_err(|_| EncodeError::TooLarge)?);
	}
	Ok(())
}

// The cheapest Rice parameter for one partition, and what it costs in bits.
//
// The search starts from the average magnitude - a Rice code is cheapest when its parameter is about
// the log of the mean - and looks either side of it rather than over all fifteen, which is the
// difference between an encoder that keeps up and one that does not.
fn best_parameter(values: &[i32]) -> Option<(u32, u64)> {
	if values.is_empty() {
		return Some((0, 0));
	}
	let mut total = 0u64;
	for &value in values {
		total = total.saturating_add(zigzag(value));
	}
	let mean = (total / values.len() as u64).max(1);
	let estimate = 64 - mean.leading_zeros();
	let low = estimate.saturating_sub(2).min(MAX_RICE);
	let high = (estimate + 1).min(MAX_RICE);
	let mut best: Option<(u32, u64)> = None;
	for parameter in low..=high {
		if let Some(cost) = rice_cost(values, parameter)
			&& best.as_ref().is_none_or(|(_, previous)| cost < *previous)
		{
			best = Some((parameter, cost));
		}
	}
	best
}

// What one partition costs at one parameter, or nothing if it cannot be written: a quotient the
// decoder would refuse to read back is not a cost, it is a different file.
fn rice_cost(values: &[i32], parameter: u32) -> Option<u64> {
	let mut total = 0u64;
	for &value in values {
		let quotient = zigzag(value) >> parameter;
		if quotient > 0x00ff_ffff {
			return None;
		}
		total = total.checked_add(quotient + 1 + parameter as u64)?;
	}
	Some(total)
}

// Signed to unsigned, alternating, so that small negatives cost as little as small positives. The
// inverse of what the decoder does when it reads a residual back.
const fn zigzag(value: i32) -> u64 {
	let value = value as i64;
	((value << 1) ^ (value >> 63)) as u64
}

fn write_subframe(writer: &mut BitWriter, samples: &[i32], bits: u8, effort: Effort, residual: &mut Vec<i32>) -> Result<(), EncodeError> {
	let plan = plan_for(samples, bits, effort, residual);
	match plan.shape {
		Shape::Constant => {
			writer.write(0, 1);
			writer.write(0, 6);
			writer.write(0, 1);
			writer.write_signed(samples[0] as i64, bits as u32);
		}
		Shape::Verbatim => {
			writer.write(0, 1);
			writer.write(1, 6);
			writer.write(0, 1);
			for &sample in samples {
				writer.write_signed(sample as i64, bits as u32);
			}
		}
		Shape::Fixed { order, partition_order, parameters } => {
			writer.write(0, 1);
			writer.write(8 + order as u64, 6);
			writer.write(0, 1);
			for &sample in &samples[..order] {
				writer.write_signed(sample as i64, bits as u32);
			}
			// Recomputed rather than carried out of the costing pass: the scratch buffer is shared
			// with the candidates that lost, and one extra subtraction per sample is cheaper than a
			// second buffer per channel.
			fixed_residual(samples, order, residual)?;
			writer.write(0, 2);
			writer.write(partition_order as u64, 4);
			let partitions = 1usize << partition_order;
			let partition_size = samples.len() / partitions;
			let mut consumed = 0usize;
			for partition in 0..partitions {
				let count = if partition == 0 { partition_size - order } else { partition_size };
				let parameter = parameters[partition] as u32;
				writer.write(parameter as u64, 4);
				for &value in &residual[consumed..consumed + count] {
					let unsigned = zigzag(value);
					writer.unary(unsigned >> parameter);
					writer.write(unsigned, parameter);
				}
				consumed += count;
			}
		}
	}
	Ok(())
}

// The frame number, in the variable-length encoding FLAC borrows from UTF-8 and extends to thirty-six
// bits. The length is the shortest that can hold the value, which is what the decoder checks.
pub(crate) fn write_utf8_number(bytes: &mut Vec<u8>, value: u64) {
	if value < 0x80 {
		bytes.push(value as u8);
		return;
	}
	let mut length = 2usize;
	for (limit, candidate) in [(1u64 << 11, 3usize), (1 << 16, 4), (1 << 21, 5), (1 << 26, 6), (1 << 31, 7)] {
		if value >= limit {
			length = candidate;
		}
	}
	let prefix = ((0xffu16 << (8 - length)) & 0xff) as u8;
	bytes.push(prefix | (value >> (6 * (length - 1))) as u8);
	for index in (0..length - 1).rev() {
		bytes.push(0x80 | ((value >> (6 * index)) & 0x3f) as u8);
	}
}

pub(crate) struct BitWriter {
	pub(crate) bytes: Vec<u8>,
	accumulator: u64,
	bits: u32,
}

impl BitWriter {
	pub(crate) fn new() -> BitWriter {
		BitWriter { bytes: Vec::new(), accumulator: 0, bits: 0 }
	}

	fn reset(&mut self) {
		self.bytes.clear();
		self.accumulator = 0;
		self.bits = 0;
	}

	// Most significant bit first, which is the order the reader on the other side takes them in.
	// At most seven bits are ever held, so a thirty-two bit write cannot overflow the accumulator.
	pub(crate) fn write(&mut self, value: u64, count: u32) {
		if count == 0 {
			return;
		}
		let masked = if count >= 64 { value } else { value & ((1u64 << count) - 1) };
		self.accumulator = (self.accumulator << count) | masked;
		self.bits += count;
		while self.bits >= 8 {
			self.bits -= 8;
			self.bytes.push((self.accumulator >> self.bits) as u8);
		}
	}

	fn write_signed(&mut self, value: i64, count: u32) {
		self.write(value as u64, count);
	}

	// `count` zeros then a one. Written in pieces because the accumulator holds sixty-four bits and
	// a quotient may be larger than that.
	pub(crate) fn unary(&mut self, count: u64) {
		let mut remaining = count;
		while remaining >= 32 {
			self.write(0, 32);
			remaining -= 32;
		}
		self.write(1, remaining as u32 + 1);
	}

	pub(crate) fn align_zero(&mut self) {
		if self.bits != 0 {
			let padding = 8 - self.bits;
			self.write(0, padding);
		}
	}
}
