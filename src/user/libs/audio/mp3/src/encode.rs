//! Writing MPEG-1 Layer III.
//!
//! THE SHAPE OF THIS ENCODER, chosen and stated rather than left for a reader to infer:
//!
//! - MPEG-1 Layer III only, at 32, 44.1 and 48 kHz, mono or stereo. MPEG-2 and 2.5 halve the sample
//!   rate and change the scalefactor bands and the side-info layout; they are a different format
//!   behind the same four-byte header, and this refuses them by name rather than emitting a frame
//!   labelled as something it is not.
//! - CONSTANT BITRATE, and no bit reservoir: every frame carries its own main data and
//!   `main_data_begin` is zero. The reservoir is what lets a demanding frame borrow room from a
//!   quiet one; without it a demanding frame is quantised harder instead. That is a ratio decision
//!   behind the same container, and it removes the one piece of state that makes a truncated stream
//!   undecodable from where it was cut.
//! - LONG BLOCKS ONLY (`block_type` 0). Short blocks exist so a transient does not smear across 576
//!   samples; without them an attack is softened. Legal, audible on percussion, and the honest next
//!   step.
//! - Two channels coded INDEPENDENTLY. Mid/side and intensity stereo are ratio improvements inside
//!   the same container.
//! - SCALEFACTORS ARE ZERO and the rate loop moves `global_gain` alone. A scalefactor spends bits to
//!   buy a finer step in one band; choosing which band deserves it is a psychoacoustic decision, and
//!   an encoder that allocates them by a rule it cannot justify is worse than one that does not
//!   allocate them at all.
//!
//! WHERE THE TABLES CAME FROM is in `tables.rs`, and it matters: the code tables and the analysis
//! window are data this format is defined by, and both were recovered mechanically and checked
//! rather than transcribed.

use alloc::vec::Vec;
use pcm::Format;
use pcm::encode::{Sink, SinkError};

mod bits;
mod filterbank;
mod tables;

use bits::BitWriter;
use filterbank::{BLOCK, BLOCKS_PER_GRANULE, Filterbank, LINES, granule_lines};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
	// A rate, channel count or bitrate this encoder does not write. Named apart from `Invalid`
	// because it is a configuration a caller can change rather than a mistake in the input.
	Unsupported,
	// The arguments cannot describe a stream, or a frame cannot be made to fit its own budget.
	Invalid,
	TooLarge,
	// The destination said no. Carried rather than flattened, because "the disk is full" and "this
	// destination cannot seek" lead a caller to do different things.
	Destination(SinkError),
}

impl From<SinkError> for EncodeError {
	fn from(error: SinkError) -> EncodeError {
		EncodeError::Destination(error)
	}
}

// The sample rates MPEG-1 names, in the order its two-bit field numbers them.
const RATES: [u32; 3] = [44_100, 48_000, 32_000];
// The bitrates MPEG-1 Layer III names, in the order its four-bit field numbers them. Index 0 is
// "free format" and index 15 is reserved; neither is written.
const BITRATES: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];

// A Layer III frame is two granules of 576 samples.
const FRAME_SAMPLES: usize = 1_152;

// SILENCE AHEAD OF THE TRACK, and the delay the gapless tag then declares.
//
// The transform's own delay is 481 samples, measured - the number the analysis window was fitted at.
// The trouble is that a decoder reading a Xing/Info tag computes its skip as
// `one frame + declared delay + 529`, so the smallest total it can express is 1681 samples; an
// encoder whose real delay is under that cannot say so, and declaring zero would leave the decoder
// cutting into the audio.
//
// So the track is PRIMED with silence, which is what every encoder that writes this tag does. 576
// samples of it puts the total where the tag can name it, and the declared delay is then
// `481 + 576 - 529`.
const PRIMER_SAMPLES: usize = 576;
const ENCODER_DELAY: u32 = 528;

// The four bytes that name the encoder in a Xing/Info tag. Decoders read the delay and padding only
// from a tag they recognise; this tree's own decoder knows this one, and one that does not simply
// declines to trim - which is what every decoder does with an encoder it has not heard of.
const ENCODER_TAG: &[u8; 4] = b"LiBr";

// The largest quantised magnitude the escape tables can carry: fifteen plus a thirteen-bit tail.
const MAX_QUANT: i32 = 15 + 8_191;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
	pub rate: u32,
	pub channels: u8,
	// Kilobits per second for the whole stream, one of the fifteen MPEG-1 Layer III values.
	pub bitrate: u32,
}

impl Config {
	// The default for a rate and channel count: 128 kbit/s for stereo and 64 for mono, which is
	// where this format's quality is conventionally judged.
	pub fn new(rate: u32, channels: u8) -> Option<Config> {
		let bitrate = if channels >= 2 { 128 } else { 64 };
		let config = Config { rate, channels, bitrate };
		config.rate_index()?;
		config.bitrate_index()?;
		Some(config)
	}

	// The same, at a bitrate the caller picks.
	pub fn with_bitrate(rate: u32, channels: u8, bitrate: u32) -> Option<Config> {
		let config = Config { rate, channels, bitrate };
		config.rate_index()?;
		config.bitrate_index()?;
		Some(config)
	}

	fn rate_index(&self) -> Option<u8> {
		if !(1..=2).contains(&self.channels) {
			return None;
		}
		RATES.iter().position(|rate| *rate == self.rate).map(|index| index as u8)
	}

	fn bitrate_index(&self) -> Option<u8> {
		BITRATES.iter().position(|value| *value == self.bitrate).filter(|index| *index != 0).map(|index| index as u8)
	}

	// The byte length of one frame, and whether it carries the padding slot.
	//
	// A Layer III frame is 1152 samples, so `144 * bitrate / rate` bytes - which is not a whole
	// number at 44.1 kHz, and the padding bit is how the format spends the remainder. An encoder
	// that ignored it would drift against the clock by about a byte every three frames.
	fn frame_bytes(&self, accumulator: &mut u32) -> (usize, bool) {
		let numerator = 144 * self.bitrate * 1_000;
		let base = numerator / self.rate;
		*accumulator += numerator % self.rate;
		let padded = *accumulator >= self.rate;
		if padded {
			*accumulator -= self.rate;
		}
		((base + u32::from(padded)) as usize, padded)
	}

	fn side_info_bytes(&self) -> usize {
		if self.channels == 1 { 17 } else { 32 }
	}
}

// What one granule of one channel decided, which is what the side info records.
#[derive(Clone, Copy, Default)]
struct GranuleInfo {
	part2_3_length: u32,
	big_values: u32,
	global_gain: u32,
	tables: [u32; 3],
	region0_count: u32,
	region1_count: u32,
	count1table_select: u32,
}

// How a granule's 576 lines are split: pairs coded with a big-values table, then quadruples of
// values in -1..1, then the zeros nobody codes at all.
struct Partition {
	big_values: usize,
	count1_quads: usize,
	// One table per region, and the two counts that say where the regions end. The three exist so
	// the loud low spectrum and the sparse high one can be coded by different tables; one table for
	// both is a table chosen for neither.
	tables: [u8; 3],
	region0_count: u32,
	region1_count: u32,
	count1_b: bool,
}

// Where a granule's three big-values regions end, in lines. The boundaries are scalefactor band
// edges - the format allows nothing else - so this picks the band nearest each third.
fn region_bounds(big_end: usize, rate_index: usize) -> (u32, u32, usize, usize) {
	let bands = &tables::SCALEFACTOR_BANDS[rate_index];
	// `region0_count` is four bits and `region1_count` three, and the decoder reads the boundaries
	// as `bands[region0_count + 1]` and `bands[region0_count + region1_count + 2]`.
	let nearest = |target: usize, low: usize, high: usize| -> usize {
		let mut best = low;
		let mut distance = usize::MAX;
		for band in low..=high {
			let value = bands[band] as usize;
			let delta = value.abs_diff(target);
			if delta < distance {
				distance = delta;
				best = band;
			}
		}
		best
	};
	let first = nearest(big_end / 3, 1, 16).max(1);
	let second = nearest(big_end * 2 / 3, first + 1, (first + 8).min(22));
	let region0_count = (first - 1) as u32;
	let region1_count = (second - first - 1) as u32;
	((region0_count).min(15), (region1_count).min(7), (bands[first] as usize).min(big_end), (bands[second] as usize).min(big_end))
}

fn representable(values: &[i32], table_select: u8) -> bool {
	let (table, linbits) = tables::TABLE_SELECT[table_select as usize];
	let xlen = tables::CODE_TABLES[table as usize].xlen as i32;
	if xlen <= 1 {
		return values.iter().all(|v| *v == 0);
	}
	let ceiling = if linbits == 0 { xlen - 1 } else { 15 + ((1i32 << linbits) - 1) };
	values.iter().all(|v| v.abs() <= ceiling)
}

// What a pair costs with a given table: the codeword, the escape tail, and a sign bit per non-zero.
fn pair_bits(x: i32, y: i32, table_select: u8) -> Option<u32> {
	let (table, linbits) = tables::TABLE_SELECT[table_select as usize];
	let code_table = &tables::CODE_TABLES[table as usize];
	let xlen = code_table.xlen as i32;
	let clamp = |v: i32| -> Option<i32> {
		let a = v.abs();
		if linbits != 0 {
			if a > 15 + ((1i32 << linbits) - 1) { None } else { Some(a.min(15)) }
		} else if a >= xlen {
			None
		} else {
			Some(a)
		}
	};
	let (cx, cy) = (clamp(x)?, clamp(y)?);
	let (len, _) = code_table.codes[(cx * xlen + cy) as usize];
	let mut total = len as u32;
	if linbits != 0 && cx == 15 {
		total += linbits as u32;
	}
	if x != 0 {
		total += 1;
	}
	if linbits != 0 && cy == 15 {
		total += linbits as u32;
	}
	if y != 0 {
		total += 1;
	}
	Some(total)
}

fn write_pair(out: &mut BitWriter, x: i32, y: i32, table_select: u8) -> Result<(), EncodeError> {
	let (table, linbits) = tables::TABLE_SELECT[table_select as usize];
	let code_table = &tables::CODE_TABLES[table as usize];
	let xlen = code_table.xlen as i32;
	let (ax, ay) = (x.abs(), y.abs());
	let (cx, cy) = if linbits != 0 { (ax.min(15), ay.min(15)) } else { (ax, ay) };
	if cx >= xlen || cy >= xlen {
		return Err(EncodeError::Invalid);
	}
	let (len, code) = code_table.codes[(cx * xlen + cy) as usize];
	out.put(len, code)?;
	if linbits != 0 && cx == 15 {
		out.put(linbits, (ax - 15) as u32)?;
	}
	if x != 0 {
		out.put(1, u32::from(x < 0))?;
	}
	if linbits != 0 && cy == 15 {
		out.put(linbits, (ay - 15) as u32)?;
	}
	if y != 0 {
		out.put(1, u32::from(y < 0))?;
	}
	Ok(())
}

fn quad_index(values: &[i32]) -> usize {
	let bit = |v: i32| usize::from(v != 0);
	bit(values[0]) * 8 + bit(values[1]) * 4 + bit(values[2]) * 2 + bit(values[3])
}

fn quad_bits(values: &[i32], table_b: bool) -> u32 {
	let table = if table_b { &tables::COUNT1_B } else { &tables::COUNT1_A };
	let (len, _) = table[quad_index(values)];
	len as u32 + values.iter().filter(|v| **v != 0).count() as u32
}

fn write_quad(out: &mut BitWriter, values: &[i32], table_b: bool) -> Result<(), EncodeError> {
	let table = if table_b { &tables::COUNT1_B } else { &tables::COUNT1_A };
	let (len, code) = table[quad_index(values)];
	out.put(len, code)?;
	for value in values {
		if *value != 0 {
			out.put(1, u32::from(*value < 0))?;
		}
	}
	Ok(())
}

// The smallest table that can carry a magnitude, by rule rather than by measurement.
//
// This is the RATE LOOP's estimate, not the encoder's choice. The loop asks "does this gain fit"
// twenty-odd times per granule, and pricing every pair against all thirty tables each time costs
// thirty times what the answer is worth - the loop only needs a number that moves the same way. The
// real choice is measured once, at the gain the loop settles on.
fn smallest_table(max_magnitude: i32) -> u8 {
	match max_magnitude {
		0..=1 => 1,
		2 => 2,
		3 => 5,
		4..=5 => 7,
		6..=7 => 10,
		8..=15 => 13,
		_ => {
			// An escape table, at the narrowest tail that reaches: 1, 2, 3, 4, 6, 8, 10, 13 bits.
			for candidate in 16..24u8 {
				if representable(&[max_magnitude], candidate) {
					return candidate;
				}
			}
			23
		}
	}
}

// What a granule costs at a gain, estimated: one table for the whole big-values area, plus the
// count1 tail. Monotone in the gain the same way the exact cost is, which is all the search needs.
fn estimate_bits(values: &[i32]) -> u32 {
	let mut end = 0usize;
	let mut peak = 0i32;
	for (index, value) in values.iter().enumerate() {
		if *value != 0 {
			end = index + 1;
		}
		peak = peak.max(value.abs());
	}
	if end % 2 != 0 {
		end += 1;
	}
	let mut count1_start = end;
	while count1_start >= 4 && values[count1_start - 4..count1_start].iter().all(|v| v.abs() <= 1) {
		count1_start -= 4;
	}
	let big_end = (count1_start / 2).min(288) * 2;
	let table = smallest_table(values[..big_end].iter().map(|v| v.abs()).max().unwrap_or(0));
	let mut total = 0u32;
	for pair in values[..big_end].chunks(2) {
		total += pair_bits(pair[0], pair[1], table).unwrap_or(u32::MAX / 4);
	}
	for quad in values[big_end..end].chunks(4) {
		total += quad_bits(quad, false).min(quad_bits(quad, true));
	}
	total
}

// Split a granule and choose its tables, by measuring rather than by rule of thumb.
fn partition(values: &[i32], rate_index: usize) -> Partition {
	let mut end = 0usize;
	for (index, value) in values.iter().enumerate() {
		if *value != 0 {
			end = index + 1;
		}
	}
	if end % 2 != 0 {
		end += 1;
	}
	// Walk the tail back while it is quadruples of values the count1 tables can carry.
	let mut count1_start = end;
	while count1_start >= 4 && values[count1_start - 4..count1_start].iter().all(|v| v.abs() <= 1) {
		count1_start -= 4;
	}
	let count1_quads = (end - count1_start) / 4;
	let big_values = (count1_start / 2).min(288);
	let big_end = big_values * 2;

	// THE TABLE IS CHOSEN BY COST, per region, over every one the format defines that can carry
	// that region's values. A rule keyed on the maximum magnitude would pick a table that fits and
	// not the one that is cheapest, and the difference is the whole of what a code table is for.
	let (region0_count, region1_count, first, second) = region_bounds(big_end, rate_index);
	let spans = [0..first, first..second, second..big_end];
	let mut tables_chosen = [0u8; 3];
	for (index, span) in spans.iter().enumerate() {
		let region = &values[span.clone()];
		if region.is_empty() {
			continue;
		}
		let mut best = u32::MAX;
		for candidate in 1..32u8 {
			if candidate == 4 || candidate == 14 || !representable(region, candidate) {
				continue;
			}
			let mut cost = 0u32;
			let mut ok = true;
			for pair in region.chunks(2) {
				match pair_bits(pair[0], pair[1], candidate) {
					Some(bits) => cost += bits,
					None => {
						ok = false;
						break;
					}
				}
			}
			if ok && cost < best {
				best = cost;
				tables_chosen[index] = candidate;
			}
		}
	}

	let mut cost_a = 0u32;
	let mut cost_b = 0u32;
	for quad in values[count1_start..end].chunks(4) {
		cost_a += quad_bits(quad, false);
		cost_b += quad_bits(quad, true);
	}
	Partition { big_values, count1_quads, tables: tables_chosen, region0_count, region1_count, count1_b: cost_b < cost_a }
}

fn granule_bits(values: &[i32], part: &Partition, rate_index: usize) -> u32 {
	let big_end = part.big_values * 2;
	let (_, _, first, second) = region_bounds(big_end, rate_index);
	let mut total = 0u32;
	for (index, span) in [0..first, first..second, second..big_end].iter().enumerate() {
		for pair in values[span.clone()].chunks(2) {
			total += pair_bits(pair[0], pair[1], part.tables[index]).unwrap_or(u32::MAX / 4);
		}
	}
	let count1_end = big_end + part.count1_quads * 4;
	for quad in values[big_end..count1_end].chunks(4) {
		total += quad_bits(quad, part.count1_b);
	}
	total
}

// Quantise one granule at a gain: `v = round((|x| / 2^((gain-210)/4))^(3/4))`, which is the
// inverse of the decoder's `|v|^(4/3) * 2^((gain-210)/4)`.
// Returns whether anything CLIPPED, which the rate loop needs: a gain fine enough to push a line
// past what the escape tables can carry does not code that line, it flattens it - and the loop must
// not choose such a gain however few bits it would cost. Without this the rate loop's own preference
// for finer steps drives it into clipping, and a high bitrate reconstructs worse than a low one.
// A square root by Newton's method, because this library carries no maths library - see the note in
// `tables.rs` about why. Five iterations from a seed taken out of the exponent field converge to
// float precision over the whole range this quantiser sees.
fn sqrt(x: f32) -> f32 {
	if x <= 0.0 {
		return 0.0;
	}
	let mut guess = f32::from_bits((x.to_bits() >> 1) + (127 << 22));
	for _ in 0..5 {
		guess = 0.5 * (guess + x / guess);
	}
	guess
}

// `x^(3/4)`, which is the quantiser's curve: `x / x^(1/4)`, and the fourth root is two square
// roots.
fn pow_three_quarters(x: f32) -> f32 {
	if x <= 0.0 {
		return 0.0;
	}
	x / sqrt(sqrt(x))
}

fn quantise(lines: &[f32], gain: i32, out: &mut [i32]) -> bool {
	let step = tables::QUANT_STEP[gain.clamp(0, 255) as usize];
	let mut clipped = false;
	for (line, slot) in lines.iter().zip(out.iter_mut()) {
		let magnitude = if *line < 0.0 { -*line } else { *line } / step;
		// The 0.4054 is the format's own rounding offset for this quantiser.
		let value = pow_three_quarters(magnitude) + 0.4054;
		let value = if value >= (MAX_QUANT + 1) as f32 {
			clipped = true;
			MAX_QUANT
		} else {
			value as i32
		};
		*slot = if *line < 0.0 { -value } else { value };
	}
	clipped
}

// The rate loop: the finest gain whose granule fits `budget` bits, found by walking down from the
// coarsest. Returns the quantised lines, how they are split, and the bits they cost.
//
// A SEARCH RATHER THAN A FORMULA, because the cost of a granule is not a smooth function of the
// gain: the Huffman table changes under it, and so does where the count1 region starts. Walking is
// twenty-odd evaluations of a granule, which is cheap next to the transform that produced it.
fn fit_granule(lines: &[f32], budget: u32, rate_index: usize, scratch: &mut [i32]) -> Option<(GranuleInfo, Partition)> {
	// BINARY SEARCH, because the cost is monotone in the gain: a coarser step is never more bits.
	// A linear walk over all 256 gains was two hundred and fifty evaluations of a granule where
	// eight will do, and a granule evaluation prices every pair against thirty code tables.
	let cost_at = |gain: i32, scratch: &mut [i32]| -> u32 {
		if quantise(lines, gain, scratch) {
			return u32::MAX;
		}
		estimate_bits(scratch)
	};
	// The coarsest gain must fit - at 255 the step is enormous and everything quantises to zero.
	if cost_at(255, scratch) > budget {
		return None;
	}
	let (mut low, mut high) = (0i32, 255i32);
	while low < high {
		let mid = (low + high) / 2;
		if cost_at(mid, scratch) <= budget { high = mid } else { low = mid + 1 }
	}
	// THE MEASURED CHOICE, at the gain the estimate settled on. The estimate prices one table over
	// the whole granule, so the measured cost with a table per region is never larger - but the
	// gain is stepped up until it fits rather than trusted to, because "never larger" is an
	// argument and this is a frame that has to fit.
	let mut gain = low;
	let (mut part, mut bits) = loop {
		quantise(lines, gain, scratch);
		let part = partition(scratch, rate_index);
		let bits = granule_bits(scratch, &part, rate_index);
		if bits <= budget {
			break (part, bits);
		}
		gain += 1;
		if gain > 255 {
			return None;
		}
	};
	let low = gain;
	let _ = (&mut part, &mut bits);
	let info = GranuleInfo { part2_3_length: bits, big_values: part.big_values as u32, global_gain: low as u32, tables: [part.tables[0] as u32, part.tables[1] as u32, part.tables[2] as u32], region0_count: part.region0_count, region1_count: part.region1_count, count1table_select: u32::from(part.count1_b) };
	Some((info, part))
}

fn write_header(out: &mut BitWriter, config: &Config, padded: bool) -> Result<(), EncodeError> {
	let rate_index = config.rate_index().ok_or(EncodeError::Unsupported)?;
	let bitrate_index = config.bitrate_index().ok_or(EncodeError::Unsupported)?;
	out.put(11, 0x7ff)?; // sync
	out.put(2, 3)?; // MPEG-1
	out.put(2, 1)?; // Layer III
	out.put(1, 1)?; // no CRC protection
	out.put(4, bitrate_index as u32)?;
	out.put(2, rate_index as u32)?;
	out.put(1, u32::from(padded))?;
	out.put(1, 0)?; // private
	// 3 is single channel and 0 is plain stereo. Joint stereo is 1 and is not written - see the
	// note at the top about coding the two channels independently.
	out.put(2, if config.channels == 1 { 3 } else { 0 })?;
	out.put(2, 0)?; // mode extension, meaningless outside joint stereo
	out.put(1, 0)?; // copyright
	out.put(1, 0)?; // original
	out.put(2, 0)?; // emphasis: none
	Ok(())
}

// The side info: 17 bytes for mono, 32 for stereo, and the layout is why those two numbers are what
// they are. It follows the header and precedes this frame's main data, because there is no
// reservoir for it to point back into.
fn write_side_info(out: &mut BitWriter, config: &Config, granules: &[[GranuleInfo; 2]; 2]) -> Result<(), EncodeError> {
	let channels = config.channels as usize;
	out.put(9, 0)?; // main_data_begin
	out.put(if channels == 1 { 5 } else { 3 }, 0)?; // private bits
	for _ in 0..channels {
		out.put(4, 0)?; // scfsi: every granule carries its own scalefactors, which are all zero
	}
	for granule in granules.iter() {
		for channel in granule.iter().take(channels) {
			out.put(12, channel.part2_3_length)?;
			out.put(9, channel.big_values)?;
			out.put(8, channel.global_gain)?;
			out.put(4, 0)?; // scalefac_compress
			out.put(1, 0)?; // window_switching_flag: long blocks only
			for table in channel.tables.iter() {
				out.put(5, *table)?;
			}
			out.put(4, channel.region0_count)?;
			out.put(3, channel.region1_count)?;
			out.put(1, 0)?; // preflag
			out.put(1, 0)?; // scalefac_scale
			out.put(1, channel.count1table_select)?;
		}
	}
	Ok(())
}

// A streaming MPEG-1 Layer III encoder over any `pcm::encode::Sink`.
//
// NOTHING IS HELD. The filterbank carries 512 samples of history per channel and the MDCT needs the
// granule after the one it emits, so the encoder's memory is a frame and a half - not a function of
// the track's length.
pub struct Encoder<S: Sink> {
	sink: S,
	config: Config,
	// Per channel: the filterbank, its part-filled input block, and the subband blocks not yet
	// turned into granules.
	banks: Vec<Filterbank>,
	pending: Vec<Vec<f32>>,
	blocks: Vec<Vec<[f32; 32]>>,
	frames: u64,
	samples: u64,
	padding_accumulator: u32,
	info_len: usize,
}

impl<S: Sink> Encoder<S> {
	pub fn new(sink: S, format: Format, bitrate: u32) -> Result<Encoder<S>, EncodeError> {
		let config = Config::with_bitrate(format.rate(), format.channels(), bitrate).ok_or(EncodeError::Unsupported)?;
		let channels = config.channels as usize;
		let mut encoder = Encoder { sink, config, banks: (0..channels).map(|_| Filterbank::new()).collect(), pending: (0..channels).map(|_| Vec::new()).collect(), blocks: (0..channels).map(|_| Vec::new()).collect(), frames: 0, samples: 0, padding_accumulator: 0, info_len: 0 };
		encoder.write_info_frame()?;
		// The primer goes in before anything the caller pushes, and is not counted as audio: it is
		// there to be dropped, and the tag is what says so.
		for _ in 0..PRIMER_SAMPLES {
			for channel in 0..channels {
				encoder.pending[channel].push(0.0);
			}
			encoder.take_blocks()?;
		}
		Ok(encoder)
	}

	// THE GAPLESS TAG, WRITTEN FIRST AND CORRECTED LAST. A Layer III frame is 1152 samples, so a
	// track whose length is not a multiple of that comes back longer than it went in - by up to a
	// frame at the end, and by the encoder's own delay at the start. The Xing/Info tag is where the
	// format records both, and it lives in the main-data area of a leading frame that decodes to
	// silence.
	//
	// It cannot be written correctly here, because the frame count is not known until the end - so
	// a placeholder goes out and `finish` patches it, which is what `Sink::patch` exists for and why
	// this encoder refuses a destination that only goes forward.
	fn write_info_frame(&mut self) -> Result<(), EncodeError> {
		let (frame_bytes, padded) = self.config.frame_bytes(&mut self.padding_accumulator);
		let mut out = BitWriter::new();
		write_header(&mut out, &self.config, padded)?;
		let empty = [[GranuleInfo::default(); 2]; 2];
		write_side_info(&mut out, &self.config, &empty)?;
		let mut bytes = out.into_bytes()?;
		bytes.resize(frame_bytes, 0);
		// `Info` rather than `Xing`: the two differ only in what they claim about the bitrate, and
		// this stream is constant.
		let tag = 4 + self.config.side_info_bytes();
		bytes[tag..tag + 4].copy_from_slice(b"Info");
		bytes[tag + 4..tag + 8].copy_from_slice(&1u32.to_be_bytes()); // flags: frame count present
		self.info_len = frame_bytes;
		self.sink.write(&bytes)?;
		Ok(())
	}

	// Interleaved signed-16-bit frames, whole frames only.
	pub fn push(&mut self, interleaved: &[i16]) -> Result<(), EncodeError> {
		let channels = self.config.channels as usize;
		if interleaved.len() % channels != 0 {
			return Err(EncodeError::Invalid);
		}
		for frame in interleaved.chunks(channels) {
			for (channel, sample) in frame.iter().enumerate() {
				// Into the -1..1 the transform works in, by the negative full scale so the one
				// sample that reaches it does not clip.
				self.pending[channel].push(*sample as f32 / 32_768.0);
			}
			self.take_blocks()?;
			// AND FRAMES AS SOON AS THERE ARE BLOCKS FOR ONE. Without this the block buffer grows
			// for the whole of a push - a caller handing over a minute of audio held ten megabytes
			// of subband samples per channel, which is exactly the "nothing is held" claim at the
			// top of this file being false. `audio-bench`'s peak-heap row is what found it.
			if self.blocks[0].len() >= 3 * BLOCKS_PER_GRANULE {
				self.drain()?;
			}
		}
		self.samples += (interleaved.len() / channels) as u64;
		self.drain()
	}

	// Turn every channel's part-filled block into a subband block once all of them are full.
	//
	// A BLOCK AT A TIME, NEVER A DRAIN FROM THE FRONT. This used to buffer the whole push and then
	// `drain(..32)` in a loop, which is quadratic in the size of the push: a caller handing over a
	// minute of audio in one call moved two hundred billion samples. The pending buffer now never
	// holds more than one block.
	fn take_blocks(&mut self) -> Result<(), EncodeError> {
		let channels = self.config.channels as usize;
		if self.pending[0].len() < BLOCK {
			return Ok(());
		}
		for channel in 0..channels {
			if self.pending[channel].len() < BLOCK {
				return Ok(());
			}
		}
		for channel in 0..channels {
			let mut input = [0.0f32; BLOCK];
			input.copy_from_slice(&self.pending[channel][..BLOCK]);
			self.pending[channel].clear();
			let mut out = [0.0f32; 32];
			self.banks[channel].push(&input, &mut out);
			self.blocks[channel].push(out);
		}
		Ok(())
	}

	// Turn whatever complete blocks are pending into frames. A frame is two granules, and the
	// second granule's MDCT reaches eighteen blocks past itself - so fifty-four blocks have to be
	// present to emit thirty-six.
	fn drain(&mut self) -> Result<(), EncodeError> {
		let channels = self.config.channels as usize;
		loop {
			self.take_blocks()?;
			if self.blocks.iter().map(|b| b.len()).min().unwrap_or(0) < 3 * BLOCKS_PER_GRANULE {
				return Ok(());
			}
			self.emit_frame()?;
			for channel in 0..channels {
				self.blocks[channel].drain(..2 * BLOCKS_PER_GRANULE);
			}
		}
	}

	fn emit_frame(&mut self) -> Result<(), EncodeError> {
		let channels = self.config.channels as usize;
		let (frame_bytes, padded) = self.config.frame_bytes(&mut self.padding_accumulator);
		let rate_index = self.config.rate_index().ok_or(EncodeError::Unsupported)? as usize;
		let overhead = 4 + self.config.side_info_bytes();
		let budget_bits = ((frame_bytes - overhead) * 8) as u32;
		// Split evenly, and let the first granule's slack go to the second.
		let mut remaining = budget_bits;
		let mut infos = [[GranuleInfo::default(); 2]; 2];
		let mut coded: Vec<(Vec<i32>, Partition)> = Vec::new();
		let mut scratch = alloc::vec![0i32; LINES];
		for granule in 0..2 {
			for channel in 0..channels {
				let slots = (2 - granule) * channels - channel;
				// AND NEVER MORE THAN THE FIELD CAN SAY. `part2_3_length` is twelve bits, so a
				// granule cannot be told to spend more than 4095 of them however much room the
				// frame has - at 320 kbit/s a frame's budget is twice that. Bits left over become
				// stuffing, which is what the format expects to find there.
				let share = (remaining / slots as u32).min(4_095);
				let lines = granule_lines(&self.blocks[channel], granule * BLOCKS_PER_GRANULE);
				let (info, part) = fit_granule(&lines, share, rate_index, &mut scratch).ok_or(EncodeError::Invalid)?;
				remaining -= info.part2_3_length;
				infos[granule][channel] = info;
				coded.push((scratch.clone(), part));
			}
		}

		let mut out = BitWriter::new();
		write_header(&mut out, &self.config, padded)?;
		write_side_info(&mut out, &self.config, &infos)?;
		for (values, part) in &coded {
			let big_end = part.big_values * 2;
			let (_, _, first, second) = region_bounds(big_end, rate_index);
			for (index, span) in [0..first, first..second, second..big_end].iter().enumerate() {
				for pair in values[span.clone()].chunks(2) {
					write_pair(&mut out, pair[0], pair[1], part.tables[index])?;
				}
			}
			let count1_end = big_end + part.count1_quads * 4;
			for quad in values[big_end..count1_end].chunks(4) {
				write_quad(&mut out, quad, part.count1_b)?;
			}
		}
		let mut bytes = out.into_bytes()?;
		if bytes.len() > frame_bytes {
			return Err(EncodeError::Invalid);
		}
		bytes.resize(frame_bytes, 0);
		self.sink.write(&bytes)?;
		self.frames += 1;
		Ok(())
	}

	// Flush the tail and return the sink with the number of SAMPLE FRAMES encoded - the track's own
	// length, which is what every other encoder in this tree returns and what a caller reports.
	// The count of format frames is the tag's business and stays here.
	//
	// THE TAIL NEEDS SILENCE PUSHED THROUGH IT, and a bounded amount: the filterbank holds 512
	// samples of history and the MDCT reaches a granule ahead, so the last real sample leaves the
	// transform about two frames after it went in. Feeding exactly that much and no more is what
	// stops a flush becoming a stream of frames of nothing.
	pub fn finish(mut self) -> Result<(S, u64), EncodeError> {
		let channels = self.config.channels as usize;
		let tail = 512 + FRAME_SAMPLES * 2;
		for _ in 0..tail {
			for channel in 0..channels {
				self.pending[channel].push(0.0);
			}
			self.take_blocks()?;
		}
		self.drain()?;

		// The tag, now that the count is known. `frames` counts audio frames; the leading tag frame
		// is not one of them, and neither the count nor the padding may include it.
		let encoded = self.frames.saturating_mul(FRAME_SAMPLES as u64);
		let trimmed = self.samples.saturating_add(ENCODER_DELAY as u64);
		// What the last frame carries past the end of the track, plus the delay at its start. A
		// padding this cannot express is clamped rather than wrapped: the file is then a fraction of
		// a frame longer than it claims, which is the smaller of the two lies available.
		let padding = encoded.saturating_sub(trimmed).min(4_095) as u32;
		let tag = 4 + self.config.side_info_bytes();
		let mut patch = Vec::new();
		patch.extend_from_slice(&(self.frames as u32).to_be_bytes());
		patch.extend_from_slice(ENCODER_TAG);
		// The delay and padding sit twenty-one bytes past the encoder tag, packed as two twelve-bit
		// fields across three bytes - which is where every decoder that reads them looks.
		patch.extend_from_slice(&[0u8; 17]);
		patch.push((ENCODER_DELAY >> 4) as u8);
		patch.push((((ENCODER_DELAY & 0x0f) << 4) | (padding >> 8)) as u8);
		patch.push((padding & 0xff) as u8);
		if tag + 8 + patch.len() <= self.info_len {
			self.sink.patch((tag + 8) as u64, &patch)?;
		}
		let samples = self.samples;
		Ok((self.sink, samples))
	}
}

#[cfg(test)]
mod tables_tests;
#[cfg(test)]
mod tests;
