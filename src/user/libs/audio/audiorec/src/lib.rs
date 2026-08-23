//! Recording: what to record, into what, and when to stop.
//!
//! The same division as `audioconv`. This library decides; the binary beside it holds the
//! capabilities, the capture channel and the transactional writer. Everything here is pure over its
//! input, so "does `--rate 96000` mean anything" and "how many frames fit in a RIFF file" are
//! answered by host tests rather than by booting a machine with a microphone.
//!
//! NOTHING HERE HOLDS AUDIO. A recording is written as it arrives - period by period into the
//! storage writer, whose transaction is what makes an interrupted run leave the destination
//! untouched - so the only numbers this library keeps are counters.

#![no_std]

extern crate alloc;

use alloc::string::String;
use pcm::Format;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	InvalidOptions,
	// No destination named. Its own answer rather than `InvalidOptions`, because it is the one
	// mistake a user makes by leaving something out rather than by typing something wrong.
	MissingOutput,
	// A rate or channel count `pcm::Format` does not accept: this hardware is 48 kHz stereo and
	// everything else is a conversion down from it.
	UnsupportedFormat,
	// The recording asked for cannot be described by a classic RIFF file. See `MAX_DATA_BYTES`.
	TooLong,
}

impl Error {
	pub const fn message(self) -> &'static str {
		match self {
			Error::InvalidOptions => "audiorec: unknown or malformed option\n",
			Error::MissingOutput => "audiorec: no destination named\n",
			Error::UnsupportedFormat => "audiorec: rates are 8000..48000 Hz and channels are 1 or 2\n",
			Error::TooLong => "audiorec: that recording cannot fit in a WAV file\n",
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
	Record,
	Help,
}

// THE CEILING CLASSIC RIFF HAS, and the reason this is not an implementation detail.
//
// A RIFF file's sizes are unsigned 32-bit: the `data` chunk length and the `RIFF` length that
// covers it. Past that a writer has three choices - write a wrong length, write RF64, or stop - and
// two of them produce a file that decodes into something other than what was recorded. RF64 is not
// implemented here and is not claimed; this stops, and says so.
//
// The bound is on the `data` chunk. The header this tree writes is 44 bytes (12 RIFF + 24 fmt + 8
// data), so a `data` chunk of `u32::MAX - HEADER_BYTES` is the largest that keeps the RIFF length
// in range as well.
pub const HEADER_BYTES: u64 = 44;
pub const MAX_DATA_BYTES: u64 = u32::MAX as u64 - HEADER_BYTES;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
	pub mode: Mode,
	pub output: String,
	pub rate: u32,
	pub channels: u8,
	// The recording's length in whole seconds, or None to record until interrupted.
	pub seconds: Option<u32>,
	pub force: bool,
}

impl Config {
	// The frame count this configuration stops at: the shorter of what `--seconds` asked for and
	// what a RIFF file can describe. An unbounded recording still has the second bound.
	pub fn frame_limit(&self) -> u64 {
		let by_riff = MAX_DATA_BYTES / (self.channels as u64 * 2);
		match self.seconds {
			Some(seconds) => (seconds as u64 * self.rate as u64).min(by_riff),
			None => by_riff,
		}
	}

	pub const fn frame_bytes(&self) -> u64 {
		self.channels as u64 * 2
	}
}

// `--seconds` is refused HERE rather than discovered at the ceiling: a run that would stop early is
// one the user should be told about before it records for an hour, not after.
fn check_length(rate: u32, channels: u8, seconds: Option<u32>) -> Result<(), Error> {
	let Some(seconds) = seconds else { return Ok(()) };
	let frames = (seconds as u64).checked_mul(rate as u64).ok_or(Error::TooLong)?;
	let bytes = frames.checked_mul(channels as u64 * 2).ok_or(Error::TooLong)?;
	if bytes > MAX_DATA_BYTES {
		return Err(Error::TooLong);
	}
	Ok(())
}

// `audiorec [options] <path>`: whitespace-separated, one destination, options before or after it.
pub fn parse_args(arguments: &[u8]) -> Result<Config, Error> {
	let mut config = Config { mode: Mode::Record, output: String::new(), rate: pcm::OUTPUT_RATE, channels: 2, seconds: None, force: false };
	let mut words = arguments.split(|byte| byte.is_ascii_whitespace()).filter(|word| !word.is_empty());
	let mut named_output = false;
	while let Some(word) = words.next() {
		match word {
			b"-h" | b"--help" => {
				config.mode = Mode::Help;
				return Ok(config);
			}
			b"-f" | b"--force" => config.force = true,
			b"-r" | b"--rate" => config.rate = number(words.next())? as u32,
			b"-c" | b"--channels" => {
				let channels = number(words.next())?;
				if channels > u8::MAX as u64 {
					return Err(Error::UnsupportedFormat);
				}
				config.channels = channels as u8;
			}
			b"-s" | b"--seconds" => {
				let seconds = number(words.next())?;
				if seconds == 0 || seconds > u32::MAX as u64 {
					return Err(Error::InvalidOptions);
				}
				config.seconds = Some(seconds as u32);
			}
			// A second destination is a mistake rather than a second recording: one capture stream
			// exists, and writing it to two files is `tee`'s job on a stream this tool does not have.
			_ if word.starts_with(b"-") => return Err(Error::InvalidOptions),
			_ => {
				if named_output {
					return Err(Error::InvalidOptions);
				}
				named_output = true;
				config.output = String::from_utf8_lossy(word).into_owned();
			}
		}
	}
	if !named_output {
		return Err(Error::MissingOutput);
	}
	// The format is checked against `pcm::Format`, which is the same bound AudioService applies -
	// so a rate this accepts is one `open-capture` will accept, and the tool never reports a
	// service refusal it could have explained itself.
	Format::new(config.rate, config.channels).ok_or(Error::UnsupportedFormat)?;
	check_length(config.rate, config.channels, config.seconds)?;
	Ok(config)
}

fn number(word: Option<&[u8]>) -> Result<u64, Error> {
	let word = word.ok_or(Error::InvalidOptions)?;
	if word.is_empty() {
		return Err(Error::InvalidOptions);
	}
	let mut value: u64 = 0;
	for &byte in word {
		if !byte.is_ascii_digit() {
			return Err(Error::InvalidOptions);
		}
		value = value.checked_mul(10).and_then(|v| v.checked_add((byte - b'0') as u64)).ok_or(Error::InvalidOptions)?;
	}
	Ok(value)
}

pub const fn help_text() -> &'static str {
	"audiorec - record PCM audio to a WAV file\n\
	 \n\
	 usage: audiorec [options] <path>\n\
	 \n\
	   -r, --rate N       sample rate in Hz, 8000..48000 (default 48000)\n\
	   -c, --channels N   1 or 2 (default 2)\n\
	   -s, --seconds N    stop after N seconds (default: until interrupted)\n\
	   -f, --force        overwrite an existing destination\n\
	   -h, --help         this text\n\
	 \n\
	 The recording is staged in a storage transaction and published only when it ends cleanly,\n\
	 so an interrupted run leaves the destination exactly as it was. Output is signed 16-bit\n\
	 little-endian PCM in a classic RIFF/WAVE container; a recording that would outgrow RIFF's\n\
	 32-bit sizes stops at the ceiling rather than writing a length that is not true.\n"
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
