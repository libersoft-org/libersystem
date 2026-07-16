#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;

use ogg::PacketReader;
use pcm::Format;

macro_rules! try_old {
	($expr:expr) => {
		match $expr {
			core::result::Result::Ok(value) => value,
			core::result::Result::Err(error) => return Err(core::convert::From::from(error)),
		}
	};
}

macro_rules! record_residue_pre_inverse {
	($residue_vectors:expr) => {};
}

macro_rules! record_residue_post_inverse {
	($residue_vectors:expr) => {};
}

macro_rules! record_pre_mdct {
	($audio_spectri:expr) => {};
}

macro_rules! record_post_mdct {
	($audio_spectri:expr) => {};
}

#[path = "core/audio.rs"]
#[allow(dead_code)]
mod audio;
#[path = "core/bitpacking.rs"]
mod bitpacking;
#[path = "core/header.rs"]
#[allow(dead_code)]
mod header;
#[path = "core/header_cached.rs"]
mod header_cached;
#[path = "core/huffman_tree.rs"]
mod huffman_tree;
#[path = "core/imdct.rs"]
mod imdct;
#[cfg(test)]
#[path = "core/imdct_test.rs"]
mod imdct_test;
#[path = "core/samples.rs"]
#[allow(dead_code)]
mod samples;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Truncated,
	Invalid,
	Unsupported,
	Checksum,
	Sequence,
	TooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata {
	pub rate: u32,
	pub channels: u8,
	pub frames: u64,
	pub duration_ms: u64,
}

pub struct Vorbis<'a> {
	bytes: &'a [u8],
	format: Format,
	metadata: Metadata,
	ident: header::IdentHeader,
	setup: header::SetupHeader,
}

impl<'a> Vorbis<'a> {
	pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
		let mut reader = PacketReader::new(bytes);
		let ident_packet = reader.next_packet().map_err(map_ogg)?.ok_or(Error::Truncated)?;
		if !ident_packet.bos || ident_packet.eos {
			return Err(Error::Invalid);
		}
		let ident = header::read_header_ident(&ident_packet.data).map_err(map_header)?;
		let format = Format::new(ident.audio_sample_rate, ident.audio_channels).ok_or(Error::Unsupported)?;
		let comment_packet = reader.next_packet().map_err(map_ogg)?.ok_or(Error::Truncated)?;
		if comment_packet.bos || comment_packet.eos {
			return Err(Error::Invalid);
		}
		header::read_header_comment(&comment_packet.data).map_err(map_header)?;
		let setup_packet = reader.next_packet().map_err(map_ogg)?.ok_or(Error::Truncated)?;
		if setup_packet.bos || setup_packet.eos {
			return Err(Error::Invalid);
		}
		let setup = header::read_header_setup(&setup_packet.data, ident.audio_channels, (ident.blocksize_0, ident.blocksize_1)).map_err(map_header)?;
		let frames = scan_audio_packets(&mut reader, &ident, &setup)?;
		let duration_ms = frames.checked_mul(1_000).ok_or(Error::TooLarge)? / format.rate() as u64;
		Ok(Self { bytes, format, metadata: Metadata { rate: format.rate(), channels: format.channels(), frames, duration_ms }, ident, setup })
	}

	pub const fn metadata(&self) -> Metadata {
		self.metadata
	}

	pub const fn format(&self) -> Format {
		self.format
	}

	pub fn decoder(&self) -> Decoder<'_> {
		Decoder { vorbis: self, reader: PacketReader::new(self.bytes), headers_remaining: 3, previous_window: audio::PreviousWindowRight::new(), current_granule: None, first_audio_packet: true, pending: Vec::new(), pending_frame: 0, emitted: 0, done: false }
	}
}

fn scan_audio_packets(reader: &mut PacketReader<'_>, ident: &header::IdentHeader, setup: &header::SetupHeader) -> Result<u64, Error> {
	let mut first = true;
	let mut frames = 0u64;
	let mut current_granule = None;
	let mut saw_audio = false;
	let mut saw_eos = false;
	while let Some(packet) = reader.next_packet().map_err(map_ogg)? {
		saw_audio = true;
		let decoded = audio::get_decoded_sample_count(ident, setup, &packet.data).map_err(map_audio)? as u64;
		let mut emitted = if first { 0 } else { decoded };
		if packet.eos {
			let end = packet.granule_position.ok_or(Error::Invalid)?;
			if let Some(start) = current_granule {
				emitted = emitted.min(end.saturating_sub(start));
			}
			saw_eos = true;
		}
		frames = frames.checked_add(emitted).ok_or(Error::TooLarge)?;
		if first {
			current_granule = packet.page_granule_position;
		} else if let Some(granule) = packet.granule_position {
			current_granule = Some(granule);
		} else if let Some(granule) = current_granule.as_mut() {
			*granule = granule.checked_add(emitted).ok_or(Error::TooLarge)?;
		}
		first = false;
	}
	if !saw_audio || !saw_eos || frames == 0 {
		return Err(Error::Invalid);
	}
	Ok(frames)
}

pub struct Decoder<'a> {
	vorbis: &'a Vorbis<'a>,
	reader: PacketReader<'a>,
	headers_remaining: u8,
	previous_window: audio::PreviousWindowRight,
	current_granule: Option<u64>,
	first_audio_packet: bool,
	pending: Vec<i16>,
	pending_frame: usize,
	emitted: u64,
	done: bool,
}

impl Decoder<'_> {
	pub fn remaining_frames(&self) -> u64 {
		self.vorbis.metadata.frames.saturating_sub(self.emitted)
	}

	pub fn read_i16_le(&mut self, max_frames: usize, output: &mut Vec<u8>) -> Result<usize, Error> {
		if max_frames == 0 {
			return Err(Error::Invalid);
		}
		output.clear();
		let channels = self.vorbis.format.channels() as usize;
		let byte_capacity = max_frames.checked_mul(channels).and_then(|samples| samples.checked_mul(2)).ok_or(Error::TooLarge)?;
		output.try_reserve(byte_capacity).map_err(|_| Error::TooLarge)?;
		while output.len() / (channels * 2) < max_frames {
			if self.pending_frame == self.pending.len() / channels {
				self.pending.clear();
				self.pending_frame = 0;
				if self.done || self.emitted >= self.vorbis.metadata.frames {
					self.done = true;
					break;
				}
				self.decode_packet()?;
				if self.pending.is_empty() {
					continue;
				}
			}
			let remaining = max_frames - output.len() / (channels * 2);
			let available = self.pending.len() / channels - self.pending_frame;
			let take = remaining.min(available).min(self.remaining_frames() as usize);
			let start = self.pending_frame * channels;
			let end = start + take * channels;
			for sample in &self.pending[start..end] {
				output.extend_from_slice(&sample.to_le_bytes());
			}
			self.pending_frame += take;
			self.emitted += take as u64;
		}
		Ok(output.len() / (channels * 2))
	}

	fn decode_packet(&mut self) -> Result<(), Error> {
		while self.headers_remaining != 0 {
			self.reader.next_packet().map_err(map_ogg)?.ok_or(Error::Truncated)?;
			self.headers_remaining -= 1;
		}
		let packet = match self.reader.next_packet().map_err(map_ogg)? {
			Some(packet) => packet,
			None => {
				self.done = true;
				return Ok(());
			}
		};
		let decoded: samples::InterleavedSamples<i16> = audio::read_audio_packet_generic(&self.vorbis.ident, &self.vorbis.setup, &packet.data, &mut self.previous_window).map_err(map_audio)?;
		self.pending = decoded.samples;
		let channels = self.vorbis.format.channels() as usize;
		if self.pending.len() % channels != 0 {
			return Err(Error::Invalid);
		}
		if packet.eos {
			let end = packet.granule_position.ok_or(Error::Invalid)?;
			if let Some(start) = self.current_granule {
				let target = usize::try_from(end.saturating_sub(start)).unwrap_or(usize::MAX);
				self.pending.truncate(target.min(self.pending.len() / channels) * channels);
			}
			self.done = true;
		}
		let frames = self.pending.len() / channels;
		if self.first_audio_packet {
			self.current_granule = packet.page_granule_position;
			self.first_audio_packet = false;
		} else if let Some(granule) = packet.granule_position {
			self.current_granule = Some(granule);
		} else if let Some(granule) = self.current_granule.as_mut() {
			*granule = granule.checked_add(frames as u64).ok_or(Error::TooLarge)?;
		}
		Ok(())
	}
}

fn map_ogg(error: ogg::Error) -> Error {
	match error {
		ogg::Error::Truncated => Error::Truncated,
		ogg::Error::Checksum => Error::Checksum,
		ogg::Error::Sequence => Error::Sequence,
		ogg::Error::TooLarge => Error::TooLarge,
		ogg::Error::Invalid => Error::Invalid,
	}
}

fn map_header(error: header::HeaderReadError) -> Error {
	match error {
		header::HeaderReadError::EndOfPacket => Error::Truncated,
		header::HeaderReadError::UnsupportedVorbisVersion => Error::Unsupported,
		header::HeaderReadError::BufferNotAddressable => Error::TooLarge,
		_ => Error::Invalid,
	}
}

fn map_audio(error: audio::AudioReadError) -> Error {
	match error {
		audio::AudioReadError::EndOfPacket => Error::Truncated,
		audio::AudioReadError::BufferNotAddressable => Error::TooLarge,
		_ => Error::Invalid,
	}
}

fn ilog(value: u64) -> u8 {
	64 - value.leading_zeros() as u8
}

fn bit_reverse(value: u32) -> u32 {
	let mut reversed = value;
	reversed = ((reversed & 0xaaaa_aaaa) >> 1) | ((reversed & 0x5555_5555) << 1);
	reversed = ((reversed & 0xcccc_cccc) >> 2) | ((reversed & 0x3333_3333) << 2);
	reversed = ((reversed & 0xf0f0_f0f0) >> 4) | ((reversed & 0x0f0f_0f0f) << 4);
	reversed = ((reversed & 0xff00_ff00) >> 8) | ((reversed & 0x00ff_00ff) << 8);
	(reversed >> 16) | (reversed << 16)
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn decodes_staged_stream_to_independent_golden() {
		let vorbis = Vorbis::parse(include_bytes!("../../../volume/test.ogg")).unwrap();
		assert_eq!(vorbis.metadata(), Metadata { rate: 44_100, channels: 1, frames: 328_104, duration_ms: 7_440 });
		let mut decoder = vorbis.decoder();
		let mut pcm = Vec::new();
		let mut chunk = Vec::new();
		loop {
			let frames = decoder.read_i16_le(37, &mut chunk).unwrap();
			if frames == 0 {
				break;
			}
			pcm.extend_from_slice(&chunk);
		}
		assert_eq!(pcm.len(), 656_208);
		let golden = include_bytes!("../tests/test.pcm");
		for (actual, expected) in pcm.chunks_exact(2).zip(golden.chunks_exact(2)) {
			let actual = i16::from_le_bytes([actual[0], actual[1]]) as i32;
			let expected = i16::from_le_bytes([expected[0], expected[1]]) as i32;
			assert!((actual - expected).abs() <= 1);
		}
		assert_eq!(decoder.remaining_frames(), 0);
	}

	#[test]
	fn rejects_truncated_corrupt_and_non_vorbis_streams() {
		let source = include_bytes!("../../../volume/test.ogg");
		for length in [0, 4, 26, source.len() - 1] {
			assert!(Vorbis::parse(&source[..length]).is_err());
		}
		let mut corrupt = source.to_vec();
		*corrupt.last_mut().unwrap() ^= 1;
		assert_eq!(Vorbis::parse(&corrupt).err(), Some(Error::Checksum));
		let mut wrong_header = source.to_vec();
		let signature = wrong_header.windows(7).position(|bytes| bytes == b"\x01vorbis").unwrap();
		wrong_header[signature] = 3;
		let crc = ogg::ogg_crc(&wrong_header[..wrong_header[26] as usize + 27 + wrong_header[27..27 + wrong_header[26] as usize].iter().map(|length| *length as usize).sum::<usize>()]);
		wrong_header[22..26].copy_from_slice(&crc.to_le_bytes());
		assert_eq!(Vorbis::parse(&wrong_header).err(), Some(Error::Invalid));
	}

	#[test]
	fn rejects_zero_frame_reads() {
		let vorbis = Vorbis::parse(include_bytes!("../../../volume/test.ogg")).unwrap();
		let mut decoder = vorbis.decoder();
		assert_eq!(decoder.read_i16_le(0, &mut Vec::new()), Err(Error::Invalid));
	}

	#[test]
	fn rejects_compact_oversized_header_allocations() {
		let mut setup = b"\x05vorbis\x00BCV\x01\x00".to_vec();
		setup.extend_from_slice(&[1, 0, 4]);
		assert!(matches!(header::read_header_setup(&setup, 1, (6, 6)), Err(header::HeaderReadError::BufferNotAddressable)));

		let mut comments = b"\x03vorbis\x00\x00\x00\x00".to_vec();
		comments.extend_from_slice(&4_097u32.to_le_bytes());
		assert!(matches!(header::read_header_comment(&comments), Err(header::HeaderReadError::BufferNotAddressable)));
	}
}
