// THE USB AUDIO CLASS DECISIONS, WITH NO CONTROLLER BEHIND THEM.
//
// The rule this tree keeps for every class: what a descriptor MEANS is decided here, against
// fixtures, and the transport module below only moves bytes. A UAC topology is almost entirely
// numbers the DEVICE chose - how many channels, how many bits, which rates, which alternate setting
// carries an endpoint at all - so it is exactly the kind of parse that is wrong quietly.

use crate::descriptor;

/// The audio interface class, and its two subclasses this driver speaks.
pub const CLASS_AUDIO: u8 = 0x01;
pub const SUBCLASS_AUDIOCONTROL: u8 = 0x01;
pub const SUBCLASS_AUDIOSTREAMING: u8 = 0x02;

/// Class-specific interface descriptors and the two subtypes a streaming interface carries.
pub const DT_CS_INTERFACE: u8 = 0x24;
pub const AS_GENERAL: u8 = 0x01;
pub const AS_FORMAT_TYPE: u8 = 0x02;

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
	Ok(FormatOne { channels, subframe_bytes, bits, continuous: kind == 0, rates, rate_count })
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
}

/// Walk one configuration for an audio sink this system's wire can drive.
pub fn bind(config: &[u8]) -> Result<Binding, NotBindable> {
	bind_for(config, Direction::Sink)
}

/// Walk one configuration for a streaming setting that carries this system's wire in `direction`: a sink's
/// isochronous OUT, or a source's isochronous IN data endpoint.
pub fn bind_for(config: &[u8], direction: Direction) -> Result<Binding, NotBindable> {
	let mut config_value: Option<u8> = None;
	let mut have_control = false;
	// The interface and alternate currently being described, and what has been read about it.
	let mut current: Option<(u8, u8)> = None;
	let mut format: Option<FormatOne> = None;
	let mut general_is_pcm = false;
	let mut best: Option<Binding> = None;
	let mut records = 0usize;
	for record in descriptor::Walk::new(config) {
		records += 1;
		if records > MAX_RECORDS {
			return Err(NotBindable::TooMany);
		}
		match record.kind {
			descriptor::DT_CONFIG => config_value = record.field(5).ok(),
			descriptor::DT_INTERFACE => {
				let number = record.field(2).map_err(|_| NotBindable::Malformed)?;
				let alternate = record.field(3).map_err(|_| NotBindable::Malformed)?;
				let class = record.field(5).map_err(|_| NotBindable::Malformed)?;
				let subclass = record.field(6).map_err(|_| NotBindable::Malformed)?;
				// EACH INTERFACE DESCRIPTOR ENDS THE ONE BEFORE IT, so what was read about the
				// previous alternate setting stops applying here.
				current = None;
				format = None;
				general_is_pcm = false;
				if class != CLASS_AUDIO {
					continue;
				}
				match subclass {
					SUBCLASS_AUDIOCONTROL => have_control = true,
					SUBCLASS_AUDIOSTREAMING => current = Some((number, alternate)),
					_ => {}
				}
			}
			DT_CS_INTERFACE if current.is_some() => {
				let subtype = record.field(2).map_err(|_| NotBindable::Malformed)?;
				match subtype {
					AS_GENERAL => {
						// THE FORMAT TAG IS THE CLAIM AND THE FORMAT DESCRIPTOR IS THE SHAPE. A
						// device whose general descriptor says a compressed tag and whose format
						// descriptor says Type I is describing two different things, and playing
						// PCM into it is playing PCM into a decoder.
						general_is_pcm = record.field16(5).map_err(|_| NotBindable::Malformed)? == FORMAT_TAG_PCM;
					}
					AS_FORMAT_TYPE => format = format_one(&record).ok(),
					_ => {}
				}
			}
			descriptor::DT_ENDPOINT => {
				let (Some((number, alternate)), Some(found)) = (current, format) else { continue };
				if !general_is_pcm {
					continue;
				}
				let address = record.field(2).map_err(|_| NotBindable::Malformed)?;
				let attributes = record.field(3).map_err(|_| NotBindable::Malformed)?;
				let packet = record.field16(4).map_err(|_| NotBindable::Malformed)?;
				let interval = record.field(6).unwrap_or(1);
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
					best = Some(Binding { config_value: 0, streaming_interface: number, alternate, endpoint: address, max_packet: packet & 0x07FF, interval, format: found });
				}
			}
			_ => {}
		}
	}
	if !have_control {
		return Err(NotBindable::NoAudioControl);
	}
	let mut binding = best.ok_or(NotBindable::NoUsableFormat)?;
	binding.config_value = config_value.ok_or(NotBindable::Malformed)?;
	Ok(binding)
}

#[cfg(test)]
mod tests;
