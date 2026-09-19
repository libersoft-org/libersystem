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
//!     with the period itself, `CMD_CAPTURE_STOP` ends the capture stream and is answered `OK`.
//!
//! ONE BYTE IS UNAMBIGUOUS BECAUSE EVERY PERIOD IS EXACTLY `PERIOD_BYTES`. AudioService pads the
//! last period of a stream with silence rather than sending a short one, so a message of any other
//! length was never a shape this protocol had - which is what lets a length stand in for a tag.

/// Ask for one captured period.
pub const CMD_CAPTURE: u8 = 1;
/// End the capture stream.
pub const CMD_CAPTURE_STOP: u8 = 2;

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
			_ => Message::Unknown,
		},
		len if len == PERIOD_BYTES as usize => Message::Play,
		_ => Message::Unknown,
	}
}

/// The reply that means the server did what was asked.
pub const OK: &[u8] = b"OK";

/// A REFUSAL IS AN EMPTY REPLY, and an empty reply is never a period: a period is `PERIOD_BYTES`.
/// That is what lets a consumer tell "here are your samples" from "no" without a second field.
pub const REFUSED: &[u8] = b"";

#[cfg(test)]
mod tests;
