// THE USB AUDIO CLASS DECISIONS, WITH NO CONTROLLER BEHIND THEM.
//
// The rule this tree keeps for every class: what a descriptor MEANS is decided here, against
// fixtures, and the transport module below only moves bytes. A UAC topology is almost entirely
// numbers the DEVICE chose - how many channels, how many bits, which rates, which alternate setting
// carries an endpoint at all - so it is exactly the kind of parse that is wrong quietly.

use crate::descriptor;
use alloc::vec::Vec;

/// The audio interface class, and its two subclasses this driver speaks.
pub const CLASS_AUDIO: u8 = 0x01;
pub const SUBCLASS_AUDIOCONTROL: u8 = 0x01;
pub const SUBCLASS_AUDIOSTREAMING: u8 = 0x02;

/// Class-specific interface descriptors and the two subtypes a streaming interface carries.
pub const DT_CS_INTERFACE: u8 = 0x24;
pub const AS_GENERAL: u8 = 0x01;
pub const AS_FORMAT_TYPE: u8 = 0x02;

/// THE INTERFACE PROTOCOL A UAC2 FUNCTION CARRIES on its audio interfaces (`IP_VERSION_02_00`); UAC1's is zero. One
/// configuration is one version - a mix of the two is refused as malformed.
pub const PROTOCOL_UAC2: u8 = 0x20;

/// UAC2's audio-control subtypes this walk reads: the terminals, which name the clock they run on, and the clock
/// entities. A SELECTOR or a MULTIPLIER is read only to be refused by name.
pub const AC_INPUT_TERMINAL: u8 = 0x02;
pub const AC_OUTPUT_TERMINAL: u8 = 0x03;
pub const AC_CLOCK_SOURCE: u8 = 0x0A;
pub const AC_CLOCK_SELECTOR: u8 = 0x0B;
pub const AC_CLOCK_MULTIPLIER: u8 = 0x0C;

/// Format type one: the PCM family. Every other type is a compressed or vendor format this driver
/// refuses rather than guesses at.
pub const FORMAT_TYPE_I: u8 = 0x01;
/// The format tag a Type I interface carries for plain PCM.
pub const FORMAT_TAG_PCM: u16 = 0x0001;

/// What this system's audio wire is, which is what an alternate setting has to match EXACTLY.
///
/// A REFUSAL AND NOT A CONVERSION. Resampling belongs to a mixer if it belongs anywhere, and a
/// driver that quietly took 44.1 kHz for 48 plays everything a semitone out with nothing reporting
/// it - which sounds like a bad recording rather than like a bug.
pub const WANTED_RATE_HZ: u32 = driver_protocol::audio::RATE_HZ;
pub const WANTED_CHANNELS: u8 = driver_protocol::audio::CHANNELS;
pub const WANTED_BITS: u8 = driver_protocol::audio::BITS;

/// Why a device cannot be bound as an audio sink.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotBindable {
	/// No configuration carries an audio-control interface.
	NoAudioControl,
	/// No streaming interface offers an alternate setting this system's wire can use.
	NoUsableFormat,
	/// A descriptor ended before a field this parse needs.
	Malformed,
	/// More interfaces or alternate settings than this bounded walk follows.
	TooMany,
	/// A UAC2 streaming interface whose terminal runs on no clock source this driver can set: none, or a selector
	/// or a multiplier - choosing a clock is topology this driver does not take on.
	ClockTopology,
}

/// The largest number of records this walk follows, because every length in a configuration
/// descriptor is the device's own number.
pub const MAX_RECORDS: usize = 256;

/// What a Type I format descriptor says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FormatOne {
	pub channels: u8,
	/// Bytes per sample per channel, which is NOT the bit resolution: a device may carry 24 bits in
	/// four bytes, and a driver that sized its buffer from the resolution is short by a quarter.
	pub subframe_bytes: u8,
	pub bits: u8,
	/// Whether the rates are a continuous range rather than a discrete list.
	pub continuous: bool,
	/// The discrete rates, or the range's two ends when `continuous`.
	pub rates: [u32; MAX_RATES],
	pub rate_count: usize,
	/// A UAC2 format, which carries NO rates: they are the clock's, asked for with `GET RANGE` before the stream
	/// starts (`range_offers`). `suits` then answers for the shape alone.
	pub clocked: bool,
}

/// How many discrete sample rates this parse keeps. A device listing more is not refused - the ones
/// past this are simply not considered, because the answer this driver needs is whether ONE
/// particular rate is among them.
pub const MAX_RATES: usize = 8;

/// Read a Type I format descriptor.
///
/// THE RATE FIELD IS TWENTY-FOUR BITS AND LITTLE-ENDIAN, which is the field in this class that is
/// most often read as thirty-two: 48000 is `0x80 0xBB 0x00`, and a reader taking four bytes swallows
/// the next rate's low byte and answers a frequency no device has.
pub fn format_one(record: &descriptor::Record<'_>) -> Result<FormatOne, NotBindable> {
	let format_type = record.field(3).map_err(|_| NotBindable::Malformed)?;
	if format_type != FORMAT_TYPE_I {
		return Err(NotBindable::NoUsableFormat);
	}
	let channels = record.field(4).map_err(|_| NotBindable::Malformed)?;
	let subframe_bytes = record.field(5).map_err(|_| NotBindable::Malformed)?;
	let bits = record.field(6).map_err(|_| NotBindable::Malformed)?;
	let kind = record.field(7).map_err(|_| NotBindable::Malformed)?;
	let mut rates = [0u32; MAX_RATES];
	let mut rate_count = 0usize;
	// ZERO MEANS A RANGE AND ANYTHING ELSE MEANS A COUNT, which is the one place this descriptor
	// changes shape. A driver reading a range as a one-entry list takes the LOWER bound as the only
	// rate the device has.
	let entries = if kind == 0 { 2 } else { kind as usize };
	for index in 0..entries.min(MAX_RATES) {
		let at = 8 + index * 3;
		let low = record.field(at).map_err(|_| NotBindable::Malformed)? as u32;
		let mid = record.field(at + 1).map_err(|_| NotBindable::Malformed)? as u32;
		let high = record.field(at + 2).map_err(|_| NotBindable::Malformed)? as u32;
		rates[index] = low | mid << 8 | high << 16;
		rate_count += 1;
	}
	Ok(FormatOne { channels, subframe_bytes, bits, continuous: kind == 0, rates, rate_count, clocked: false })
}

/// Read a UAC2 Type I format descriptor: `bSubslotSize` and `bBitResolution`, with the channel count the general
/// descriptor before it gave - the format descriptor no longer carries it, nor any rate.
pub fn format_two(record: &descriptor::Record<'_>, channels: u8) -> Result<FormatOne, NotBindable> {
	let format_type = record.field(3).map_err(|_| NotBindable::Malformed)?;
	if format_type != FORMAT_TYPE_I {
		return Err(NotBindable::NoUsableFormat);
	}
	let subframe_bytes = record.field(4).map_err(|_| NotBindable::Malformed)?;
	let bits = record.field(5).map_err(|_| NotBindable::Malformed)?;
	Ok(FormatOne { channels, subframe_bytes, bits, continuous: false, rates: [0; MAX_RATES], rate_count: 0, clocked: true })
}

/// The most subranges a clock's `GET RANGE` answer is read for - a device saying more is refused rather than half read.
pub const MAX_SUBRANGES: usize = 16;

/// The bytes asked for in a `GET RANGE` of a sampling-frequency control: the count and `MAX_SUBRANGES` triples.
pub const RANGE_BYTES: u16 = 2 + 12 * MAX_SUBRANGES as u16;

/// WHETHER A UAC2 CLOCK OFFERS `rate_hz`, from its answer to `GET RANGE` of the sampling-frequency control: a
/// two-byte count and that many (minimum, maximum, resolution) triples of four bytes. A discrete rate is a triple whose
/// ends are equal; a range offers each step of its resolution from its minimum, and every rate in it when the
/// resolution is zero. An answer shorter than the count it states, or a count past `MAX_SUBRANGES`, is refused as
/// malformed rather than read as far as it goes.
pub fn range_offers(answer: &[u8], rate_hz: u32) -> Result<bool, NotBindable> {
	let count = answer.get(..2).map(|count| u16::from_le_bytes([count[0], count[1]]) as usize).ok_or(NotBindable::Malformed)?;
	if count > MAX_SUBRANGES || answer.len() < 2 + count * 12 {
		return Err(NotBindable::Malformed);
	}
	let word = |at: usize| u32::from_le_bytes([answer[at], answer[at + 1], answer[at + 2], answer[at + 3]]);
	Ok((0..count).any(|index| {
		let at = 2 + index * 12;
		let (low, high, step) = (word(at), word(at + 4), word(at + 8));
		low <= rate_hz && rate_hz <= high && (step == 0 || (rate_hz - low) % step == 0)
	}))
}

/// THE PACKETS A STREAM KEEPS AHEAD OF THE BUS, AND HOW MANY OF THEM TO ONE COMPLETION EVENT, COUNTED IN TIME: about
/// `AHEAD_US` ahead and an event every `GROUP_US`, bounded by the page's `buffers` and by `MAX_AHEAD`. Counted in
/// packets, sixteen ahead is sixteen milliseconds at full speed and two at high speed with a one-microframe interval -
/// where the same driver would be answering an event every millisecond and running dry if it were one late.
pub const AHEAD_US: u32 = 16_000;
pub const GROUP_US: u32 = 8_000;
pub const MAX_AHEAD: u32 = 128;

pub fn posting(interval_us: u32, buffers: u32) -> (u32, u32) {
	let interval = interval_us.max(1);
	let ahead = (AHEAD_US / interval).clamp(1, buffers.clamp(1, MAX_AHEAD));
	let group = (GROUP_US / interval).clamp(1, ahead);
	(ahead, group)
}

impl FormatOne {
	/// Whether this format carries exactly what the wire above asks for.
	///
	/// EXACTLY, AND THE SUBFRAME SIZE IS PART OF IT. A device offering sixteen bits in a four-byte
	/// subframe carries the same samples in twice the bytes, so a driver matching on the RESOLUTION
	/// alone hands the device half a period and calls it whole.
	pub fn suits(&self, rate_hz: u32, channels: u8, bits: u8) -> bool {
		if self.channels != channels || self.bits != bits || self.subframe_bytes as u32 != bits as u32 / 8 {
			return false;
		}
		if self.clocked {
			return true;
		}
		if self.continuous {
			return self.rate_count == 2 && self.rates[0] <= rate_hz && rate_hz <= self.rates[1];
		}
		self.rates[..self.rate_count].contains(&rate_hz)
	}

	/// How many bytes one period of `frames` costs in this format.
	pub fn period_bytes(&self, frames: u32) -> u32 {
		frames * self.channels as u32 * self.subframe_bytes as u32
	}
}

/// How many isochronous packets one period is, at this endpoint's packet size.
///
/// ONE PACKET PER SERVICE INTERVAL AND NOT ONE PER PERIOD. An isochronous endpoint carries at most
/// `max_packet` bytes each time the bus schedules it, so a period longer than that is several
/// transfers - and a driver that posted the whole period as one asks the controller for a packet
/// the endpoint cannot carry, which it answers with a babble error rather than by splitting it.
pub fn packets_for(period_bytes: u32, max_packet: u16) -> usize {
	if max_packet == 0 {
		return 0;
	}
	period_bytes.div_ceil(max_packet as u32) as usize
}

/// How many bytes the packet at `index` carries.
///
/// THE LAST ONE IS SHORT AND THAT IS NOT AN ERROR. A period is whatever the wire's period is, and
/// the endpoint's packet size is whatever the device chose; they divide evenly only by accident.
pub fn packet_span(period_bytes: u32, max_packet: u16, index: usize) -> u32 {
	let taken = index as u32 * max_packet as u32;
	period_bytes.saturating_sub(taken).min(max_packet as u32)
}

/// Whether an endpoint descriptor is an isochronous one going OUT to the device.
///
/// BITS 1:0 ARE THE TRANSFER TYPE AND `01` IS ISOCHRONOUS. The bits above them are the
/// synchronisation and usage types, which a driver that compared the whole byte would read as a
/// different transfer type on every device that sets them.
pub fn isochronous_out(address: u8, attributes: u8) -> bool {
	attributes & 0x03 == 0x01 && address & 0x80 == 0
}

/// Whether an endpoint descriptor is an isochronous DATA endpoint coming IN from the device.
///
/// DATA, BECAUSE AN ASYNCHRONOUS SINK HAS AN ISOCHRONOUS IN ENDPOINT TOO: its feedback endpoint, which
/// carries the rate the sink wants rather than samples, and says so in the usage type (bits 5:4, `01`).
/// A driver that took every isochronous IN endpoint for a microphone would record a speaker's clock.
pub fn isochronous_in(address: u8, attributes: u8) -> bool {
	attributes & 0x03 == 0x01 && attributes & 0x30 == 0 && address & 0x80 != 0
}

/// How an isochronous data endpoint keeps time with the host (bits 3:2 of its attributes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sync {
	None,
	/// The device runs its own clock and says what rate it wants, through a feedback endpoint.
	Asynchronous,
	/// The device follows whatever arrives.
	Adaptive,
	/// The device follows the bus's frames.
	Synchronous,
}

fn sync_of(attributes: u8) -> Sync {
	match attributes >> 2 & 0x3 {
		1 => Sync::Asynchronous,
		2 => Sync::Adaptive,
		3 => Sync::Synchronous,
		_ => Sync::None,
	}
}

/// AN ASYNCHRONOUS SINK'S EXPLICIT FEEDBACK ENDPOINT: the isochronous IN endpoint its data endpoint's `bSynchAddress`
/// names, in the same alternate setting, with its own packet size and interval, and how often it refreshes - every
/// `2^refresh` frames.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Feedback {
	pub endpoint: u8,
	pub max_packet: u16,
	pub interval: u8,
	pub refresh: u8,
}

/// Whether an endpoint is an isochronous IN endpoint of feedback usage (bits 5:4, `01`).
pub fn feedback_in(address: u8, attributes: u8) -> bool {
	attributes & 0x03 == 0x01 && attributes >> 4 & 0x3 == 0x01 && address & 0x80 != 0
}

/// AN ISOCHRONOUS ENDPOINT'S SERVICE INTERVAL, in microseconds: `2^(bInterval-1)` frames at full speed, microframes at
/// high speed.
pub fn interval_us(interval: u8, high_speed: bool) -> u32 {
	let exponent = u32::from(interval.clamp(1, 16)) - 1;
	if high_speed { 125 << exponent } else { 1000 << exponent }
}

/// A FEEDBACK ENDPOINT'S VALUE as frames per service interval, 16.16: three bytes of 10.14 at full speed, four of 16.16
/// at high speed. `None` for a packet too short to carry one.
pub fn feedback_value(bytes: &[u8], high_speed: bool) -> Option<u32> {
	if high_speed { bytes.get(..4).map(|value| u32::from_le_bytes([value[0], value[1], value[2], value[3]])) } else { bytes.get(..3).map(|value| (u32::from_le_bytes([value[0], value[1], value[2], 0]) & 0x00ff_ffff) << 2) }
}

/// THE FRAMES EACH SERVICE INTERVAL CARRIES, so that their average is exactly the rate: whole frames a packet, the
/// fraction carried from one to the next, and never more than the endpoint's packet holds. The rate is the nominal one
/// for an adaptive or synchronous sink and the device's own for an asynchronous one - a feedback value more than a
/// frame an interval from nominal is IGNORED and counted, never obeyed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pacer {
	nominal: u32,
	rate: u32,
	carry: u32,
	most: u32,
	/// Feedback values ignored as too far from nominal.
	pub ignored: u64,
}

impl Pacer {
	/// `nominal` frames a service interval, 16.16, and `most` frames one packet holds.
	pub fn new(nominal: u32, most: u32) -> Pacer {
		Pacer { nominal, rate: nominal, carry: 0, most, ignored: 0 }
	}

	/// The nominal rate at `rate_hz` for a service interval of `interval_us`, 16.16.
	pub fn nominal_for(rate_hz: u32, interval_us: u32) -> u32 {
		((u64::from(rate_hz) << 16) * u64::from(interval_us) / 1_000_000) as u32
	}

	/// What `next` would answer, without taking it.
	pub fn peek(&self) -> u32 {
		(((u64::from(self.carry) + u64::from(self.rate)) >> 16) as u32).min(self.most)
	}

	/// The next packet's frames.
	pub fn next(&mut self) -> u32 {
		let total = u64::from(self.carry) + u64::from(self.rate);
		self.carry = (total & 0xffff) as u32;
		((total >> 16) as u32).min(self.most)
	}

	/// A feedback value, 16.16: obeyed when it is within a frame an interval of nominal, ignored and counted otherwise.
	pub fn feedback(&mut self, value: u32) -> bool {
		if value.abs_diff(self.nominal) > 1 << 16 {
			self.ignored += 1;
			return false;
		}
		self.rate = value;
		true
	}

	/// The rate in use, 16.16.
	pub fn rate(&self) -> u32 {
		self.rate
	}
}

/// Which way PCM crosses an isochronous endpoint: to a device that plays it, or from one that records.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
	Sink,
	Source,
}

/// What a bound audio device is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Binding {
	pub config_value: u8,
	/// The streaming interface and the alternate setting that carries the endpoint.
	///
	/// ALTERNATE ZERO CARRIES NO ENDPOINT BY SPECIFICATION - it is the zero-bandwidth setting a
	/// device sits in when nothing is playing - so a driver that never selected one has an
	/// interface with nothing on it and a stream that never starts.
	pub streaming_interface: u8,
	pub alternate: u8,
	pub endpoint: u8,
	pub max_packet: u16,
	/// The polling interval, as the exponent the endpoint descriptor states.
	pub interval: u8,
	pub format: FormatOne,
	/// How the data endpoint keeps time, and an asynchronous sink's explicit feedback endpoint.
	pub sync: Sync,
	pub feedback: Option<Feedback>,
	/// A UAC2 setting's clock: its rate is set there, not on the endpoint.
	pub clock: Option<Clock>,
}

/// THE CLOCK SOURCE A UAC2 STREAM RUNS ON, and the audio-control interface its requests are addressed through - the
/// two halves of their `wIndex`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Clock {
	pub control_interface: u8,
	pub id: u8,
}

/// Walk one configuration for an audio sink this system's wire can drive.
pub fn bind(config: &[u8]) -> Result<Binding, NotBindable> {
	bind_for(config, Direction::Sink)
}

/// Walk one configuration for a streaming setting that carries this system's wire in `direction`: a sink's
/// isochronous OUT, or a source's isochronous IN data endpoint.
///
/// UAC1 AND UAC2 BOTH, told apart by the audio interfaces' protocol. A UAC2 function says less in its streaming
/// interface - the channel count is in the general descriptor, and the rates are not there at all but on the clock
/// the linked terminal runs on - so its walk also reads the audio-control interface's terminals and clocks, and the
/// binding carries the clock its rate is set on.
pub fn bind_for(config: &[u8], direction: Direction) -> Result<Binding, NotBindable> {
	let mut config_value: Option<u8> = None;
	// The audio-control interface - its number and its protocol, the version the whole function speaks - and whether
	// the descriptors being read are its own.
	let mut control: Option<(u8, u8)> = None;
	let mut in_control = false;
	// The interface and alternate currently being described, its protocol, and what has been read about it.
	let mut current: Option<(u8, u8)> = None;
	let mut current_protocol: u8 = 0;
	let mut format: Option<FormatOne> = None;
	let mut general_is_pcm = false;
	// A UAC2 general descriptor's terminal link and channel count, which its format descriptor does not repeat.
	let mut terminal_link: u8 = 0;
	let mut channels_two: u8 = 0;
	let mut best: Option<Binding> = None;
	let mut best_link: u8 = 0;
	let mut best_protocol: u8 = 0;
	// The best's `bSynchAddress`, and every feedback endpoint seen with the setting it is in.
	let mut synch_address: u8 = 0;
	let mut feedbacks: Vec<((u8, u8), Feedback)> = Vec::new();
	// UAC2's terminals with the clock each runs on, and its clock entities with their kind.
	let mut terminals: Vec<(u8, u8)> = Vec::new();
	let mut clocks: Vec<(u8, u8)> = Vec::new();
	let mut records = 0usize;
	for record in descriptor::Walk::new(config) {
		records += 1;
		if records > MAX_RECORDS {
			return Err(NotBindable::TooMany);
		}
		let field = |at: usize| record.field(at).map_err(|_| NotBindable::Malformed);
		match record.kind {
			descriptor::DT_CONFIG => config_value = record.field(5).ok(),
			descriptor::DT_INTERFACE => {
				let number = field(2)?;
				let alternate = field(3)?;
				let class = field(5)?;
				let subclass = field(6)?;
				let protocol = record.field(7).unwrap_or(0);
				// EACH INTERFACE DESCRIPTOR ENDS THE ONE BEFORE IT, so what was read about the
				// previous alternate setting stops applying here.
				current = None;
				format = None;
				general_is_pcm = false;
				in_control = false;
				terminal_link = 0;
				channels_two = 0;
				if class != CLASS_AUDIO {
					continue;
				}
				match subclass {
					SUBCLASS_AUDIOCONTROL => {
						if control.is_none() {
							control = Some((number, protocol));
						}
						in_control = true;
					}
					SUBCLASS_AUDIOSTREAMING => {
						current = Some((number, alternate));
						current_protocol = protocol;
					}
					_ => {}
				}
			}
			// UAC2'S TOPOLOGY, AS FAR AS A CLOCK: the terminals name the clock they run on, and a clock entity says what
			// kind it is. UAC1 needs none of it - its rate is the endpoint's.
			DT_CS_INTERFACE if in_control => {
				if control.is_some_and(|(_, protocol)| protocol == PROTOCOL_UAC2) {
					let subtype = field(2)?;
					match subtype {
						AC_INPUT_TERMINAL => terminals.push((field(3)?, field(7)?)),
						AC_OUTPUT_TERMINAL => terminals.push((field(3)?, field(8)?)),
						AC_CLOCK_SOURCE | AC_CLOCK_SELECTOR | AC_CLOCK_MULTIPLIER => clocks.push((field(3)?, subtype)),
						_ => {}
					}
				}
			}
			DT_CS_INTERFACE if current.is_some() => {
				let subtype = field(2)?;
				match (subtype, current_protocol == PROTOCOL_UAC2) {
					// THE FORMAT TAG IS THE CLAIM AND THE FORMAT DESCRIPTOR IS THE SHAPE. A device whose general
					// descriptor says a compressed tag and whose format descriptor says Type I is describing two
					// different things, and playing PCM into it is playing PCM into a decoder.
					(AS_GENERAL, false) => general_is_pcm = record.field16(5).map_err(|_| NotBindable::Malformed)? == FORMAT_TAG_PCM,
					// UAC2's: the terminal it links to, Type I with PCM among its formats (bit zero of four bytes), and
					// the channel count.
					(AS_GENERAL, true) => {
						terminal_link = field(3)?;
						let formats = u32::from_le_bytes([field(6)?, field(7)?, field(8)?, field(9)?]);
						general_is_pcm = field(5)? == FORMAT_TYPE_I && formats & 1 != 0;
						channels_two = field(10)?;
					}
					(AS_FORMAT_TYPE, false) => format = format_one(&record).ok(),
					(AS_FORMAT_TYPE, true) => format = format_two(&record, channels_two).ok(),
					_ => {}
				}
			}
			descriptor::DT_ENDPOINT => {
				let (Some((number, alternate)), Some(found)) = (current, format) else { continue };
				if !general_is_pcm {
					continue;
				}
				let address = field(2)?;
				let attributes = field(3)?;
				let packet = record.field16(4).map_err(|_| NotBindable::Malformed)?;
				let interval = record.field(6).unwrap_or(1);
				// A FEEDBACK ENDPOINT IS KEPT WITH ITS SETTING, for the data endpoint that names it - which may come
				// before or after it.
				if feedback_in(address, attributes) {
					feedbacks.push(((number, alternate), Feedback { endpoint: address, max_packet: packet & 0x07FF, interval, refresh: record.field(7).unwrap_or(0) }));
					continue;
				}
				let carries = match direction {
					Direction::Sink => isochronous_out(address, attributes),
					Direction::Source => isochronous_in(address, attributes),
				};
				if !carries || !found.suits(WANTED_RATE_HZ, WANTED_CHANNELS, WANTED_BITS) {
					continue;
				}
				// THE FIRST ONE THAT SUITS, which is the bounded deterministic choice this tree
				// makes everywhere else: a policy about which of two identical settings is better
				// is a knob with no answer.
				if best.is_none() {
					best = Some(Binding { config_value: 0, streaming_interface: number, alternate, endpoint: address, max_packet: packet & 0x07FF, interval, format: found, sync: sync_of(attributes), feedback: None, clock: None });
					synch_address = record.field(8).unwrap_or(0);
					best_link = terminal_link;
					best_protocol = current_protocol;
				}
			}
			_ => {}
		}
	}
	let Some((control_interface, protocol)) = control else { return Err(NotBindable::NoAudioControl) };
	let mut binding = best.ok_or(NotBindable::NoUsableFormat)?;
	binding.config_value = config_value.ok_or(NotBindable::Malformed)?;
	// ONE VERSION: a UAC2 control interface over a UAC1 streaming interface, or the reverse, is two things at once.
	if best_protocol != protocol {
		return Err(NotBindable::Malformed);
	}
	// A UAC2 SETTING'S CLOCK: the one its linked terminal runs on, and only a clock SOURCE - a selector or a multiplier
	// is a choice of clock this driver does not make.
	if protocol == PROTOCOL_UAC2 {
		let clock = terminals.iter().find(|(id, _)| *id == best_link).map(|&(_, clock)| clock).ok_or(NotBindable::ClockTopology)?;
		match clocks.iter().find(|(id, _)| *id == clock) {
			Some(&(_, AC_CLOCK_SOURCE)) => binding.clock = Some(Clock { control_interface, id: clock }),
			_ => return Err(NotBindable::ClockTopology),
		}
	}
	// AN ASYNCHRONOUS SINK'S FEEDBACK: in UAC1 the endpoint its `bSynchAddress` names, in its own setting; in UAC2 - whose
	// endpoints carry no such field - the feedback endpoint of that setting. Or none, which leaves it played at the
	// nominal rate.
	if direction == Direction::Sink && binding.sync == Sync::Asynchronous {
		let setting = (binding.streaming_interface, binding.alternate);
		binding.feedback = feedbacks.iter().find(|(at, feedback)| *at == setting && (protocol == PROTOCOL_UAC2 || (synch_address != 0 && feedback.endpoint == synch_address))).map(|(_, feedback)| *feedback);
	}
	Ok(binding)
}

#[cfg(test)]
mod tests;
