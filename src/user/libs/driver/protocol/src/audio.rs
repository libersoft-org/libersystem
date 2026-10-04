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
//!   - a message of `PERIOD_BYTES` is one PCM period to PLAY. The reply is `OK`;
//!   - an EMPTY message ends the playback stream. The reply is `OK`;
//!   - a ONE-BYTE message is a COMMAND: `CMD_CAPTURE` asks for one captured period and is answered
//!     with the period itself, `CMD_CAPTURE_STOP` ends the capture stream and is answered `OK`, and
//!     `CMD_STATS` asks for the provider's playback counters and is answered with `STATS_BYTES` of
//!     them - or refused, by a provider that keeps none.
//!
//! A PLAYED PERIOD IS ANSWERED WHEN THE PROVIDER HAS TAKEN IT. A provider that keeps a standing stream
//! answers `OK` once the period is queued and there is room for the next, so its consumer is paced by
//! the device's clock; one that plays a period at a time answers when it has played.
//!
//! ONE BYTE IS UNAMBIGUOUS BECAUSE EVERY PERIOD IS EXACTLY `PERIOD_BYTES`. AudioService pads the
//! last period of a stream with silence rather than sending a short one, so a message of any other
//! length was never a shape this protocol had - which is what lets a length stand in for a tag.

/// Ask for one captured period.
pub const CMD_CAPTURE: u8 = 1;
/// End the capture stream.
pub const CMD_CAPTURE_STOP: u8 = 2;
/// The provider's playback counters, please.
pub const CMD_STATS: u8 = 3;

/// One PCM period: 512 stereo signed-16-bit frames, about 10.6 ms at 48 kHz.
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
	/// A length no shape of this wire has, or a command byte it does not define.
	///
	/// REFUSED AND NOT GUESSED AT. A server that played a short message would play part of a period
	/// as though it were a whole one, which is a click; one that treated an unknown command byte as
	/// capture would answer a question nobody asked with a buffer.
	Unknown,
}

/// Read one message's shape from its bytes.
pub fn message(bytes: &[u8]) -> Message {
	match bytes.len() {
		0 => Message::EndPlayback,
		1 => match bytes[0] {
			CMD_CAPTURE => Message::Capture,
			CMD_CAPTURE_STOP => Message::EndCapture,
			CMD_STATS => Message::Stats,
			_ => Message::Unknown,
		},
		len if len == PERIOD_BYTES as usize => Message::Play,
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

/// A REFUSAL IS AN EMPTY REPLY, and an empty reply is never a period: a period is `PERIOD_BYTES`.
/// That is what lets a consumer tell "here are your samples" from "no" without a second field.
pub const REFUSED: &[u8] = b"";

#[cfg(test)]
mod tests;
