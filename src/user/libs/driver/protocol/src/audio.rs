//! THE AUDIO WIRE an `audio` provider serves.
//!
//! HERE FOR THE REASON THE BLOCK WIRE IS HERE: a contract written inside its first driver is copied
//! by its second - and this one was not even copied. It lived in `virtio_snd.rs` as three constants
//! and a comment, and the SECOND driver of this provider kind, `hda`, never read it: a one-byte
//! message meant "hand me a captured period" to one server and "play this one byte" to the other,
//! for the same `ProviderKind::Audio` a consumer asks the catalogue for.
//!
//! THREE MESSAGE SHAPES ON ONE CHANNEL, and a reader should not have to infer the third from code:
//!
//!   - a message of the provider's PERIOD is one PCM period to PLAY. The reply is `OK`;
//!   - an EMPTY message ends the playback stream. The reply is `OK`;
//!   - a message of one to `COMMAND_MAX` bytes is a COMMAND, its first byte saying which: `CMD_CAPTURE`
//!     asks for one captured period and is answered with the period itself, `CMD_CAPTURE_STOP` ends the
//!     capture stream and is answered `OK`, `CMD_STATS` asks for the provider's playback counters and
//!     is answered with `STATS_BYTES` of them - or refused, by a provider that keeps none - `CMD_FORMAT`
//!     asks what the provider is and is answered with `FORMAT_BYTES` of `DeviceFormat`, and `CMD_VOLUME`
//!     sets a device's own level and is answered `OK`, or refused by a device that has none.
//!
//! THE FORMAT IS THE PROVIDER'S, AND ASKED FOR FIRST. A sound card, a USB function and a Bluetooth
//! endpoint each play and capture in their own rate and channel count, and the period is the provider's
//! too: whatever `CMD_FORMAT` says. A provider that refuses the question is the fixed 48 kHz stereo with
//! `PERIOD_BYTES` periods every provider spoke before it was asked (`DeviceFormat::LEGACY`).
//!
//! A PLAYED PERIOD IS ANSWERED WHEN THE PROVIDER HAS TAKEN IT. A provider that keeps a standing stream
//! answers `OK` once the period is queued and there is room for the next, so its consumer is paced by
//! the device's clock; one that plays a period at a time answers when it has played.
//!
//! A COMMAND IS UNAMBIGUOUS BECAUSE EVERY PERIOD IS EXACTLY THE PROVIDER'S PERIOD, and at least
//! `MIN_PERIOD_BYTES`. AudioService pads the last period of a stream with silence rather than sending a
//! short one, so a message of any other length was never a shape this protocol had - which is what lets
//! a length stand in for a tag.

/// Ask for one captured period.
pub const CMD_CAPTURE: u8 = 1;
/// End the capture stream.
pub const CMD_CAPTURE_STOP: u8 = 2;
/// The provider's playback counters, please.
pub const CMD_STATS: u8 = 3;

/// One PCM period of the LEGACY format: 512 stereo signed-16-bit frames, about 10.6 ms at 48 kHz.
pub const PERIOD_BYTES: u32 = 2048;

/// The sample format every server of this wire negotiates, so a consumer need not ask.
pub const RATE_HZ: u32 = 48_000;
pub const CHANNELS: u8 = 2;
pub const BITS: u8 = 16;

/// What one message on this wire is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Message {
	/// One period to play, of exactly `PERIOD_BYTES`.
	Play,
	/// End the playback stream.
	EndPlayback,
	/// One captured period, please.
	Capture,
	/// End the capture stream.
	EndCapture,
	/// The playback counters.
	Stats,
	/// What the provider is.
	Format,
	/// Set the device's own level, 0 to 100.
	Volume(u8),
	/// A length no shape of this wire has, or a command byte it does not define.
	///
	/// REFUSED AND NOT GUESSED AT. A server that played a short message would play part of a period
	/// as though it were a whole one, which is a click; one that treated an unknown command byte as
	/// capture would answer a question nobody asked with a buffer.
	Unknown,
}

/// Read one message's shape from its bytes, for a provider of the legacy period.
pub fn message(bytes: &[u8]) -> Message {
	message_of(bytes, PERIOD_BYTES)
}

/// Read one message's shape for a provider whose period is `period_bytes`.
pub fn message_of(bytes: &[u8], period_bytes: u32) -> Message {
	match (bytes.len(), bytes.first().copied()) {
		(0, _) => Message::EndPlayback,
		(1, Some(CMD_CAPTURE)) => Message::Capture,
		(1, Some(CMD_CAPTURE_STOP)) => Message::EndCapture,
		(1, Some(CMD_STATS)) => Message::Stats,
		(1, Some(CMD_FORMAT)) => Message::Format,
		(2, Some(CMD_VOLUME)) if bytes[1] <= 100 => Message::Volume(bytes[1]),
		(len, _) if len == period_bytes as usize && period_bytes >= MIN_PERIOD_BYTES => Message::Play,
		_ => Message::Unknown,
	}
}

/// The reply that means the server did what was asked.
pub const OK: &[u8] = b"OK";

/// A PROVIDER'S PLAYBACK COUNTERS, the answer to `CMD_STATS`: the times its stream ran dry while it was meant to play
/// - one per dry spell - the frames of silence it played in their place, the rate a device's feedback asked for that
/// it plays at (frames per service interval, 16.16 - zero where nothing asks), and the feedback values it ignored as
/// too far from nominal to obey.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PlaybackStats {
	pub underruns: u64,
	pub silent_frames: u64,
	pub feedback_q16: u32,
	pub feedback_ignored: u64,
}

/// How long the answer to `CMD_STATS` is: never `OK`'s length, never a period's, never empty.
pub const STATS_BYTES: usize = 28;

impl PlaybackStats {
	pub fn encode(&self) -> [u8; STATS_BYTES] {
		let mut out = [0u8; STATS_BYTES];
		out[0..8].copy_from_slice(&self.underruns.to_le_bytes());
		out[8..16].copy_from_slice(&self.silent_frames.to_le_bytes());
		out[16..20].copy_from_slice(&self.feedback_q16.to_le_bytes());
		out[20..28].copy_from_slice(&self.feedback_ignored.to_le_bytes());
		out
	}

	/// The counters, or `None` for any other answer - a refusal among them.
	///
	/// `#[inline]`, THE CONSUMER'S HALF, for the reason `gamepad` gives: this crate is linked statically into
	/// the drivers and is no shared library, and AudioService - a dynamic executable - reads the answer too.
	#[inline]
	pub fn decode(bytes: &[u8]) -> Option<PlaybackStats> {
		if bytes.len() != STATS_BYTES {
			return None;
		}
		let word = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap_or([0; 8]));
		Some(PlaybackStats { underruns: word(0), silent_frames: word(8), feedback_q16: u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]), feedback_ignored: word(20) })
	}
}

/// THE PROVIDER'S FORMAT, please: answered with `FORMAT_BYTES` of `DeviceFormat`, or refused by a provider that predates
/// the question - which a consumer reads as `DeviceFormat::LEGACY`.
pub const CMD_FORMAT: u8 = 4;
/// SET THE DEVICE'S OWN LEVEL: a two-byte command, `[CMD_VOLUME, level]` with the level 0 to 100, answered `OK` - or
/// refused by a device that has no level of its own, whose samples the consumer scales instead.
pub const CMD_VOLUME: u8 = 5;

/// THE LONGEST COMMAND. Every command is shorter than any period, which is what lets a length stand in for a tag: a
/// period is at least `MIN_PERIOD_BYTES`.
pub const COMMAND_MAX: usize = 8;
pub const MIN_PERIOD_BYTES: u32 = 64;
/// The longest period any server of this wire takes or gives.
pub const MAX_PERIOD_BYTES: u32 = 16_384;

/// One direction's PCM format: signed 16-bit little-endian, interleaved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PcmFormat {
	pub rate: u32,
	pub channels: u8,
}

/// WHAT A PROVIDER IS, as `CMD_FORMAT` answers: the directions it serves in their formats, the period it plays and
/// gives in bytes - every playback message is exactly that long - its own latency, and whether it has a level of its
/// own and what that level is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeviceFormat {
	pub output: Option<PcmFormat>,
	pub input: Option<PcmFormat>,
	pub period_bytes: u32,
	pub latency_us: u32,
	pub hardware_volume: bool,
	pub volume: u8,
}

/// How long the answer to `CMD_FORMAT` is: never `OK`'s length, a period's, the counters' or empty.
pub const FORMAT_BYTES: usize = 24;

impl DeviceFormat {
	/// WHAT A PROVIDER THAT REFUSES THE QUESTION IS: the fixed 48 kHz stereo both ways every server of this wire spoke
	/// before it was asked, a period of `PERIOD_BYTES`, and a latency of two periods.
	pub const LEGACY: DeviceFormat = DeviceFormat { output: Some(PcmFormat { rate: RATE_HZ, channels: CHANNELS }), input: Some(PcmFormat { rate: RATE_HZ, channels: CHANNELS }), period_bytes: PERIOD_BYTES, latency_us: 2 * PERIOD_BYTES / 4 * 1_000_000 / RATE_HZ, hardware_volume: false, volume: 100 };

	pub fn encode(&self) -> [u8; FORMAT_BYTES] {
		let mut out = [0u8; FORMAT_BYTES];
		out[0] = 1;
		out[1] = u8::from(self.output.is_some()) | (u8::from(self.input.is_some()) << 1) | (u8::from(self.hardware_volume) << 2);
		let output = self.output.unwrap_or(PcmFormat { rate: 0, channels: 0 });
		let input = self.input.unwrap_or(PcmFormat { rate: 0, channels: 0 });
		out[2] = output.channels;
		out[3] = input.channels;
		out[4..8].copy_from_slice(&output.rate.to_le_bytes());
		out[8..12].copy_from_slice(&input.rate.to_le_bytes());
		out[12..16].copy_from_slice(&self.period_bytes.to_le_bytes());
		out[16..20].copy_from_slice(&self.latency_us.to_le_bytes());
		out[20] = self.volume.min(100);
		out
	}

	/// The format, or `None` for any other answer - a refusal among them - and for one no consumer could drive: a
	/// direction outside 8 to 48 kHz or one and two channels, or a period that is not whole frames within the bounds.
	#[inline]
	pub fn decode(bytes: &[u8]) -> Option<DeviceFormat> {
		if bytes.len() != FORMAT_BYTES || bytes[0] != 1 {
			return None;
		}
		let word = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
		let direction = |present: bool, rate: u32, channels: u8| -> Option<Option<PcmFormat>> {
			if !present {
				return Some(None);
			}
			((8_000..=48_000).contains(&rate) && (1..=2).contains(&channels)).then_some(Some(PcmFormat { rate, channels }))
		};
		let output = direction(bytes[1] & 1 != 0, word(4), bytes[2])?;
		let input = direction(bytes[1] & 2 != 0, word(8), bytes[3])?;
		let period_bytes = word(12);
		let frame = output.map_or(2, |format| u32::from(format.channels) * 2);
		if !(MIN_PERIOD_BYTES..=MAX_PERIOD_BYTES).contains(&period_bytes) || period_bytes % frame != 0 {
			return None;
		}
		Some(DeviceFormat { output, input, period_bytes, latency_us: word(16), hardware_volume: bytes[1] & 4 != 0, volume: bytes[20].min(100) })
	}
}

/// A REFUSAL IS AN EMPTY REPLY, and an empty reply is never a period: a period is `PERIOD_BYTES`.
/// That is what lets a consumer tell "here are your samples" from "no" without a second field.
pub const REFUSED: &[u8] = b"";

#[cfg(test)]
mod tests;
