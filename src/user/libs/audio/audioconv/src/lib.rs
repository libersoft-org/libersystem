//! Audio conversion: what to convert into what, and whether the options make sense together.
//!
//! The same division as `imgconv`. This library decides and does; the binary beside it holds the
//! capabilities and the paths. Everything here is pure over its input, so the question "does
//! `--bits 24` mean anything for Ogg Vorbis" is answered by a host test rather than by booting.
//!
//! One table answers that question, and the parser, the help text and the tests all read it. That
//! is the point of `PROFILES`: an option is inapplicable in exactly one place, so a format cannot
//! grow a capability in the help that the parser does not honour.
//!
//! Bounded, in the sense the milestone means: the ENCODED input is held - every decoder in this
//! tree parses from a slice - but the decoded audio never is. Frames move through in chunks of
//! `CHUNK_FRAMES`, get remixed and resampled in place, and go straight into the encoder, which
//! holds at most one block. An hour of audio costs what its file costs, not what its samples do.

#![no_std]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;
use pcm::Format;
use pcm::encode::{Remix, Resample, VecSink};

// How many frames cross the pipeline at a time. Large enough that the per-call overhead disappears,
// small enough that the working set stays a few kilobytes at any channel count.
const CHUNK_FRAMES: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	InvalidOptions,
	// The option is real but means nothing for this destination - `--quality` on a lossless format,
	// `--bits` on one that has only one width. Distinct from `InvalidOptions` because it is the
	// answer to "why was this rejected" that a user can act on.
	UnsupportedOption,
	UnsupportedFormat,
	// The input is not audio this tree can read, or it is damaged.
	InvalidAudio,
	// The destination format is one nothing here can write YET, which is not the same as one that
	// does not exist.
	NotImplemented,
	TooLarge,
}

// What the input turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
	Wav,
	Aiff,
	Aifc,
	Flac,
	WavPack,
	Vorbis,
	Mp3,
}

impl Container {
	pub const fn name(self) -> &'static str {
		match self {
			Container::Wav => "WAV",
			Container::Aiff => "AIFF",
			Container::Aifc => "AIFC",
			Container::Flac => "FLAC",
			Container::WavPack => "WavPack",
			Container::Vorbis => "Ogg Vorbis",
			Container::Mp3 => "MP3",
		}
	}
}

// What to write. One variant per profile rather than a format plus a pile of modifiers, because the
// profiles are what the capability table is keyed on and what a user names on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Profile {
	WavPcm,
	WavIma,
	WavMs,
	Aiff,
	Aifc,
	Flac,
	WavPack,
	Vorbis,
	Mp3,
}

// What each profile can be asked for. Read by `parse_args`, by `help_text` and by the tests, so a
// capability cannot be documented in one place and enforced in another.
pub struct Capabilities {
	pub profile: Profile,
	pub name: &'static str,
	// The first is the default when `--format` names the profile without a suffix to imply one.
	pub suffixes: &'static [&'static str],
	pub lossless: bool,
	// The sample widths `--bits` may select. Empty means the profile has no choice to offer.
	pub bits: &'static [u16],
	pub quality: bool,
	pub compression: bool,
	// Whether an encoder for it exists yet. False is a promise about the interface, not the output.
	pub implemented: bool,
}

pub const PROFILES: &[Capabilities] = &[
	Capabilities { profile: Profile::WavPcm, name: "WAV", suffixes: &["wav"], lossless: true, bits: &[8, 16, 24, 32], quality: false, compression: false, implemented: true },
	Capabilities { profile: Profile::WavIma, name: "WAV-IMA", suffixes: &[], lossless: false, bits: &[], quality: false, compression: false, implemented: true },
	Capabilities { profile: Profile::WavMs, name: "WAV-MS", suffixes: &[], lossless: false, bits: &[], quality: false, compression: false, implemented: true },
	Capabilities { profile: Profile::Aiff, name: "AIFF", suffixes: &["aiff", "aif"], lossless: true, bits: &[8, 16, 24, 32], quality: false, compression: false, implemented: true },
	Capabilities { profile: Profile::Aifc, name: "AIFC", suffixes: &["aifc"], lossless: true, bits: &[8, 16, 24, 32], quality: false, compression: false, implemented: true },
	Capabilities { profile: Profile::Flac, name: "FLAC", suffixes: &["flac"], lossless: true, bits: &[], quality: false, compression: true, implemented: true },
	Capabilities { profile: Profile::WavPack, name: "WavPack", suffixes: &["wv"], lossless: true, bits: &[], quality: false, compression: true, implemented: false },
	Capabilities { profile: Profile::Vorbis, name: "Ogg Vorbis", suffixes: &["ogg", "oga"], lossless: false, bits: &[], quality: true, compression: false, implemented: false },
	Capabilities { profile: Profile::Mp3, name: "MP3", suffixes: &["mp3"], lossless: false, bits: &[], quality: true, compression: false, implemented: false },
];

pub fn capabilities(profile: Profile) -> &'static Capabilities {
	// The table is ordered by the enum, and this asserts it rather than trusting it: a profile added
	// to one and not the other would otherwise answer with a neighbour's capabilities.
	let entry = &PROFILES[profile as usize];
	assert!(entry.profile as usize == profile as usize, "the capability table is out of order");
	entry
}

fn profile_named(value: &[u8]) -> Option<Profile> {
	if value.eq_ignore_ascii_case(b"wav") || value.eq_ignore_ascii_case(b"wav-pcm") {
		return Some(Profile::WavPcm);
	}
	if value.eq_ignore_ascii_case(b"wav-ima") || value.eq_ignore_ascii_case(b"ima") {
		return Some(Profile::WavIma);
	}
	if value.eq_ignore_ascii_case(b"wav-ms") || value.eq_ignore_ascii_case(b"ms-adpcm") {
		return Some(Profile::WavMs);
	}
	PROFILES.iter().find(|entry| entry.name.as_bytes().eq_ignore_ascii_case(value) || entry.suffixes.iter().any(|suffix| suffix.as_bytes().eq_ignore_ascii_case(value))).map(|entry| entry.profile)
}

fn profile_for_suffix(path: &[u8]) -> Option<Profile> {
	let dot = path.iter().rposition(|byte| *byte == b'.')?;
	let suffix = path.get(dot + 1..)?;
	if suffix.is_empty() {
		return None;
	}
	PROFILES.iter().find(|entry| entry.suffixes.iter().any(|known| known.as_bytes().eq_ignore_ascii_case(suffix))).map(|entry| entry.profile)
}

// What the input is, from its bytes rather than from its name.
//
// The same structural sniff `play` does, and for the same reason: a file called `.wav` that is
// really a FLAC should convert, and a file called `.flac` that is really nothing should be refused
// before anything is written.
pub fn sniff(bytes: &[u8]) -> Option<Container> {
	if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
		return Some(Container::Wav);
	}
	if bytes.starts_with(b"FORM") {
		return match bytes.get(8..12) {
			Some(b"AIFF") => Some(Container::Aiff),
			Some(b"AIFC") => Some(Container::Aifc),
			_ => None,
		};
	}
	if bytes.starts_with(b"fLaC") {
		return Some(Container::Flac);
	}
	if bytes.starts_with(b"wvpk") {
		return Some(Container::WavPack);
	}
	if bytes.starts_with(b"OggS") {
		return Some(Container::Vorbis);
	}
	if bytes.starts_with(b"ID3") || bytes.first() == Some(&0xff) && bytes.get(1).is_some_and(|byte| byte & 0xe0 == 0xe0) {
		return Some(Container::Mp3);
	}
	None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
	Convert,
	Help,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
	pub mode: Mode,
	pub input: String,
	pub output: String,
	pub profile: Option<Profile>,
	pub force: bool,
	pub rate: Option<u32>,
	pub channels: Option<u8>,
	pub bits: Option<u16>,
	pub quality: Option<u8>,
	pub compression: Option<u8>,
}

impl Config {
	// The profile actually being written: what `--format` said, or what the destination's suffix
	// implies. One place, so the parser's validation and the conversion cannot disagree.
	pub fn resolved_profile(&self) -> Option<Profile> {
		self.profile.or_else(|| profile_for_suffix(self.output.as_bytes()))
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResultInfo {
	pub source: Container,
	pub destination: Profile,
	pub source_rate: u32,
	pub source_channels: u8,
	pub source_frames: u64,
	pub rate: u32,
	pub channels: u8,
	pub frames: u64,
	pub duration_ms: u64,
	pub bytes: u64,
	// Version one strips tags, pictures and everything else that is not audio, and says so rather
	// than letting somebody find out when their album art is gone.
	pub stripped_metadata: bool,
}

pub fn help_text() -> String {
	let mut text = String::new();
	text.push_str("usage: audioconv [options] <input> <output>\n\n");
	text.push_str("Converts one audio file into another. The input format is detected from its\n");
	text.push_str("contents; the output format from the destination suffix, or from --format.\n\n");
	text.push_str("  --format NAME      write this profile instead of the one the suffix implies\n");
	text.push_str("  --force            replace the destination if it exists\n");
	text.push_str("  --rate HZ          resample to this rate (8000..48000)\n");
	text.push_str("  --channels N       remix to 1 or 2 channels\n");
	text.push_str("  --bits N           sample width, where the profile offers a choice\n");
	text.push_str("  --quality 0..100   lossy fidelity, where the profile is lossy\n");
	text.push_str("  --compression 0..100  effort, where the profile is lossless and compressed\n");
	text.push_str("  --help             print this and exit\n\n");
	text.push_str("Version one strips tags, pictures and every other non-audio field.\n\n");
	text.push_str("profiles:\n");
	for entry in PROFILES {
		// Built FROM the table, so a capability cannot be described here and refused by the parser.
		let mut line = String::new();
		let _ = write!(line, "  {:<11}", entry.name);
		let _ = write!(line, " {:<10}", if entry.suffixes.is_empty() { "--format" } else { entry.suffixes[0] });
		let _ = write!(line, " {}", if entry.lossless { "lossless" } else { "lossy   " });
		if !entry.bits.is_empty() {
			let _ = write!(line, "  --bits");
			for bits in entry.bits {
				let _ = write!(line, " {bits}");
			}
		}
		if entry.quality {
			let _ = write!(line, "  --quality");
		}
		if entry.compression {
			let _ = write!(line, "  --compression");
		}
		if !entry.implemented {
			let _ = write!(line, "  (not yet written)");
		}
		line.push('\n');
		text.push_str(&line);
	}
	text
}

pub fn parse_args(args: &[u8]) -> Result<Config, Error> {
	let mut config = Config { mode: Mode::Convert, input: String::new(), output: String::new(), profile: None, force: false, rate: None, channels: None, bits: None, quality: None, compression: None };
	let mut positional: [Option<&[u8]>; 2] = [None, None];
	let mut positional_count = 0usize;
	let mut words = args.split(|byte| byte.is_ascii_whitespace()).filter(|word| !word.is_empty());
	while let Some(word) = words.next() {
		if word == b"--help" {
			config.mode = Mode::Help;
			return Ok(config);
		}
		if word == b"--force" {
			config.force = true;
			continue;
		}
		if word.starts_with(b"--") {
			let value = words.next().ok_or(Error::InvalidOptions)?;
			match word {
				b"--format" => config.profile = Some(profile_named(value).ok_or(Error::UnsupportedFormat)?),
				// The bounds are `Format`'s, not a copy of them: a rate this tool accepted and `Format`
				// refused would fail later, in the middle of a conversion, with a worse message.
				b"--rate" => config.rate = Some(number(value, pcm::MIN_RATE as u64, pcm::OUTPUT_RATE as u64)? as u32),
				b"--channels" => config.channels = Some(number(value, 1, 2)? as u8),
				b"--bits" => config.bits = Some(number(value, 8, 32)? as u16),
				b"--quality" => config.quality = Some(number(value, 0, 100)? as u8),
				b"--compression" => config.compression = Some(number(value, 0, 100)? as u8),
				_ => return Err(Error::InvalidOptions),
			}
			continue;
		}
		if positional_count == positional.len() {
			return Err(Error::InvalidOptions);
		}
		positional[positional_count] = Some(word);
		positional_count += 1;
	}
	let (Some(input), Some(output)) = (positional[0], positional[1]) else {
		return Err(Error::InvalidOptions);
	};
	config.input = String::from_utf8_lossy(input).to_string();
	config.output = String::from_utf8_lossy(output).to_string();

	// Which profile is being written has to be known BEFORE the options can be judged, because
	// every one of them is judged against it.
	let profile = config.resolved_profile().ok_or(Error::UnsupportedFormat)?;
	let entry = capabilities(profile);
	if let Some(bits) = config.bits
		&& !entry.bits.contains(&bits)
	{
		return Err(Error::UnsupportedOption);
	}
	if config.quality.is_some() && !entry.quality {
		return Err(Error::UnsupportedOption);
	}
	if config.compression.is_some() && !entry.compression {
		return Err(Error::UnsupportedOption);
	}
	Ok(config)
}

fn number(value: &[u8], low: u64, high: u64) -> Result<u64, Error> {
	if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
		return Err(Error::InvalidOptions);
	}
	let mut total = 0u64;
	for &digit in value {
		total = total.checked_mul(10).and_then(|value| value.checked_add((digit - b'0') as u64)).ok_or(Error::InvalidOptions)?;
	}
	if !(low..=high).contains(&total) {
		return Err(Error::InvalidOptions);
	}
	Ok(total)
}

// One decoder, whichever it turned out to be, behind one call.
//
// The alternative is the conversion loop written six times, which is how the sixth one ends up
// subtly different from the other five.
trait Frames {
	fn remaining(&self) -> u64;
	fn read(&mut self, max_frames: usize, output: &mut Vec<u8>) -> Result<usize, Error>;
}

macro_rules! frames_for {
	($kind:ty) => {
		impl Frames for $kind {
			fn remaining(&self) -> u64 {
				self.remaining_frames()
			}

			fn read(&mut self, max_frames: usize, output: &mut Vec<u8>) -> Result<usize, Error> {
				self.read_i16_le(max_frames, output).map_err(|_| Error::InvalidAudio)
			}
		}
	};
}

frames_for!(wav::Decoder<'_>);
frames_for!(aiff::Decoder<'_>);
frames_for!(flac::Decoder<'_>);
frames_for!(wavpack::Decoder<'_>);
frames_for!(vorbis::Decoder<'_>);
frames_for!(mp3::Decoder<'_>);

// Where the encoded bytes end up, and the one thing every profile's encoder needs from this module.
enum Destination {
	Wav(wav::encode::Encoder<VecSink>),
	Aiff(aiff::encode::Encoder<VecSink>),
	Flac(flac::encode::Encoder<VecSink>),
}

impl Destination {
	fn push(&mut self, frames: &[i16]) -> Result<(), Error> {
		match self {
			Destination::Wav(encoder) => encoder.push(frames).map_err(wav_error),
			Destination::Aiff(encoder) => encoder.push(frames).map_err(aiff_error),
			Destination::Flac(encoder) => encoder.push(frames).map_err(flac_error),
		}
	}

	fn finish(self) -> Result<(Vec<u8>, u64), Error> {
		match self {
			Destination::Wav(encoder) => encoder.finish().map(|(sink, frames)| (sink.into_bytes(), frames)).map_err(wav_error),
			Destination::Aiff(encoder) => encoder.finish().map(|(sink, frames)| (sink.into_bytes(), frames)).map_err(aiff_error),
			Destination::Flac(encoder) => encoder.finish().map(|(sink, frames)| (sink.into_bytes(), frames)).map_err(flac_error),
		}
	}
}

fn wav_error(error: wav::encode::EncodeError) -> Error {
	match error {
		wav::encode::EncodeError::Unsupported => Error::UnsupportedOption,
		wav::encode::EncodeError::TooLarge | wav::encode::EncodeError::Destination(_) => Error::TooLarge,
		wav::encode::EncodeError::Invalid => Error::InvalidAudio,
	}
}

fn aiff_error(error: aiff::encode::EncodeError) -> Error {
	match error {
		aiff::encode::EncodeError::Unsupported => Error::UnsupportedOption,
		aiff::encode::EncodeError::TooLarge | aiff::encode::EncodeError::Destination(_) => Error::TooLarge,
		aiff::encode::EncodeError::Invalid => Error::InvalidAudio,
	}
}

fn flac_error(error: flac::encode::EncodeError) -> Error {
	match error {
		flac::encode::EncodeError::Unsupported => Error::UnsupportedOption,
		flac::encode::EncodeError::TooLarge | flac::encode::EncodeError::Destination(_) => Error::TooLarge,
		flac::encode::EncodeError::Invalid => Error::InvalidAudio,
	}
}

// Read one file, write another. Nothing here touches a volume: the caller supplies the bytes and
// takes the bytes back, which is what makes the whole of this testable without booting.
pub fn convert(input: &[u8], config: &Config) -> Result<(Vec<u8>, ResultInfo), Error> {
	let container = sniff(input).ok_or(Error::UnsupportedFormat)?;
	let profile = config.resolved_profile().ok_or(Error::UnsupportedFormat)?;
	let entry = capabilities(profile);
	if !entry.implemented {
		return Err(Error::NotImplemented);
	}

	match container {
		Container::Wav => {
			let file = wav::Wav::parse(input).map_err(|_| Error::InvalidAudio)?;
			let metadata = file.metadata();
			pump(file.decoder(), container, metadata.rate, metadata.channels, metadata.frames, profile, config)
		}
		Container::Aiff | Container::Aifc => {
			let file = aiff::Aiff::parse(input).map_err(|_| Error::InvalidAudio)?;
			let metadata = file.metadata();
			pump(file.decoder(), container, metadata.rate, metadata.channels, metadata.frames, profile, config)
		}
		Container::Flac => {
			let file = flac::Flac::parse(input).map_err(|_| Error::InvalidAudio)?;
			let metadata = file.metadata();
			pump(file.decoder(), container, metadata.rate, metadata.channels, metadata.frames, profile, config)
		}
		Container::WavPack => {
			let file = wavpack::WavPack::parse(input).map_err(|_| Error::InvalidAudio)?;
			let metadata = file.metadata();
			pump(file.decoder(), container, metadata.rate, metadata.channels, metadata.frames, profile, config)
		}
		Container::Vorbis => {
			let file = vorbis::Vorbis::parse(input).map_err(|_| Error::InvalidAudio)?;
			let metadata = file.metadata();
			pump(file.decoder(), container, metadata.rate, metadata.channels, metadata.frames, profile, config)
		}
		Container::Mp3 => {
			let file = mp3::Mp3::parse(input).map_err(|_| Error::InvalidAudio)?;
			let metadata = file.metadata();
			pump(file.decoder(), container, metadata.rate, metadata.channels, metadata.frames, profile, config)
		}
	}
}

#[allow(clippy::too_many_arguments)]
fn pump(mut source: impl Frames, container: Container, source_rate: u32, source_channels: u8, source_frames: u64, profile: Profile, config: &Config) -> Result<(Vec<u8>, ResultInfo), Error> {
	let rate = config.rate.unwrap_or(source_rate);
	let channels = config.channels.unwrap_or(source_channels);
	let format = Format::new(rate, channels).ok_or(Error::InvalidOptions)?;
	let remix = Remix::new(source_channels, channels).ok_or(Error::InvalidOptions)?;
	// Remix FIRST, then resample: the resampler interpolates whole frames, so doing it the other way
	// round would mean converting rate on a channel layout that is about to be thrown away.
	let mut resample = Resample::new(source_rate, rate, channels).ok_or(Error::InvalidOptions)?;
	let expected_frames = resample.output_frames(source_frames).ok_or(Error::TooLarge)?;

	// Room for the worst case any of these encoders can produce - four bytes per sample is the
	// widest PCM profile, and everything else is smaller - plus a header's worth. A ceiling rather
	// than an open-ended buffer, so a damaged input that claims a billion frames hits a bound.
	let ceiling = expected_frames.checked_mul(channels as u64).and_then(|samples| samples.checked_mul(4)).and_then(|bytes| bytes.checked_add(1 << 16)).ok_or(Error::TooLarge)?;

	let mut destination = build(profile, VecSink::new(ceiling), format, config)?;
	let mut raw = Vec::new();
	let mut decoded: Vec<i16> = Vec::new();
	let mut remixed: Vec<i16> = Vec::new();
	let mut resampled: Vec<i16> = Vec::new();
	while source.remaining() != 0 {
		let frames = source.read(CHUNK_FRAMES, &mut raw)?;
		if frames == 0 {
			break;
		}
		decoded.clear();
		decoded.try_reserve(raw.len() / 2).map_err(|_| Error::TooLarge)?;
		for pair in raw.chunks_exact(2) {
			decoded.push(i16::from_le_bytes([pair[0], pair[1]]));
		}
		remixed.clear();
		remix.apply(&decoded, &mut remixed).ok_or(Error::InvalidAudio)?;
		resampled.clear();
		resample.push(&remixed, &mut resampled).ok_or(Error::TooLarge)?;
		destination.push(&resampled)?;
	}
	resampled.clear();
	resample.finish(&mut resampled).ok_or(Error::TooLarge)?;
	if !resampled.is_empty() {
		destination.push(&resampled)?;
	}

	let (bytes, frames) = destination.finish()?;
	let duration_ms = frames.checked_mul(1_000).ok_or(Error::TooLarge)? / rate as u64;
	let info = ResultInfo { source: container, destination: profile, source_rate, source_channels, source_frames, rate, channels, frames, duration_ms, bytes: bytes.len() as u64, stripped_metadata: true };
	Ok((bytes, info))
}

fn build(profile: Profile, sink: VecSink, format: Format, config: &Config) -> Result<Destination, Error> {
	// The defaults each profile falls back to when the option that would choose was not given.
	let bits = config.bits.unwrap_or(16);
	let effort = flac::encode::Effort::from_percent(config.compression.unwrap_or(50));
	match profile {
		Profile::WavPcm => wav::encode::Encoder::new(sink, format, wav::encode::Output::Pcm { bits }).map(Destination::Wav).map_err(wav_error),
		Profile::WavIma => wav::encode::Encoder::new(sink, format, wav::encode::Output::ima_default(format.channels())).map(Destination::Wav).map_err(wav_error),
		Profile::WavMs => wav::encode::Encoder::new(sink, format, wav::encode::Output::ms_default(format.channels())).map(Destination::Wav).map_err(wav_error),
		Profile::Aiff => aiff::encode::Encoder::new(sink, format, aiff::encode::Output::Aiff { bits }).map(Destination::Aiff).map_err(aiff_error),
		// AIFC in the byte order the machine already has, which is the reason the profile exists.
		Profile::Aifc => aiff::encode::Encoder::new(sink, format, aiff::encode::Output::Aifc { bits, little_endian: true }).map(Destination::Aiff).map_err(aiff_error),
		Profile::Flac => flac::encode::Encoder::new(sink, format, effort).map(Destination::Flac).map_err(flac_error),
		Profile::WavPack | Profile::Vorbis | Profile::Mp3 => Err(Error::NotImplemented),
	}
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
