// play - governed streaming audio player.
//
// The tool reads one file through its volume bundle and reaches playback only through
// its scoped audio-stream grant. Decoders produce bounded signed-i16 LE chunks; the
// AudioService write reply advances by its accepted prefix, making IPC backpressure
// the playback clock without direct device access.

#![no_std]
#![no_main]

extern crate alloc;

use aiff::Aiff;
use alloc::string::String;
use alloc::vec::Vec;
use flac::Flac;
use mp3::Mp3;
use proto::path;
use proto::system::{OpenOpts, audio, pcm_stream, volume};
use rt::*;
use wav::Wav;

const CHUNK_FRAMES: usize = 1_024;

struct MappedFile {
	handle: u64,
	address: u64,
	len: usize,
}

impl MappedFile {
	unsafe fn open(storage: u64, uri: String) -> Option<MappedFile> {
		unsafe {
			let mut client = volume::Client::new(ChannelTransport { chan: storage });
			let opened = match client.open(&OpenOpts { path: uri, write: false, create: false })? {
				Ok(opened) if opened.file != 0 && opened.size != 0 => opened,
				_ => return None,
			};
			let len = match usize::try_from(opened.size) {
				Ok(len) => len,
				Err(_) => {
					close(opened.file);
					return None;
				}
			};
			let address = match map_object(opened.file) {
				Some(address) => address,
				None => {
					close(opened.file);
					return None;
				}
			};
			Some(MappedFile { handle: opened.file, address, len })
		}
	}

	unsafe fn bytes(&self) -> &[u8] {
		unsafe { core::slice::from_raw_parts(self.address as *const u8, self.len) }
	}
}

impl Drop for MappedFile {
	fn drop(&mut self) {
		unsafe {
			unmap_object(self.handle);
			close(self.handle);
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	unsafe {
		inherit_stdout(bootstrap);
		let arg = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		let system = recv_tagged(bootstrap, &mut buf, b"SYSTEM").unwrap_or(0);
		let media = recv_tagged(bootstrap, &mut buf, b"MEDIA").unwrap_or(0);
		let iso = recv_tagged(bootstrap, &mut buf, b"ISO").unwrap_or(0);
		let udf = recv_tagged(bootstrap, &mut buf, b"UDF").unwrap_or(0);
		let usb = recv_tagged(bootstrap, &mut buf, b"USB").unwrap_or(0);
		let audio_channel = recv_tagged(bootstrap, &mut buf, b"AUDIO_STREAM").unwrap_or(0);
		let cwd = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => Vec::new(),
		};
		let cwd = core::str::from_utf8(&cwd).unwrap_or("");
		let arg = tools::trim(&arg);
		let Some(uri) = path::resolve(cwd, arg) else {
			print(b"play: invalid path\n");
			exit();
		};
		let storage = path::volume_client(cwd, arg, system, media, iso, udf, usb);
		if storage == 0 || audio_channel == 0 {
			print(b"play: capability unavailable\n");
			exit();
		}
		let Some(file) = MappedFile::open(storage, uri) else {
			print(b"play: cannot open audio\n");
			exit();
		};
		catch_interrupt();
		if play_audio(audio_channel, file.bytes()).is_err() {
			print(b"play: unsupported or invalid audio\n");
		}
	}
	exit();
}

unsafe fn play_audio(audio_channel: u64, bytes: &[u8]) -> Result<(), ()> {
	if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
		let wav = Wav::parse(bytes).map_err(|_| ())?;
		let metadata = wav.metadata();
		return unsafe { play_decoded(audio_channel, "WAV", metadata.rate, metadata.channels, metadata.frames, wav.decoder()) };
	}
	if bytes.starts_with(b"FORM") && matches!(bytes.get(8..12), Some(b"AIFF") | Some(b"AIFC")) {
		let aiff = Aiff::parse(bytes).map_err(|_| ())?;
		let metadata = aiff.metadata();
		return unsafe { play_decoded(audio_channel, if bytes.get(8..12) == Some(b"AIFF") { "AIFF" } else { "AIFC" }, metadata.rate, metadata.channels, metadata.frames, aiff.decoder()) };
	}
	if bytes.starts_with(b"fLaC") {
		let flac = Flac::parse(bytes).map_err(|_| ())?;
		let metadata = flac.metadata();
		return unsafe { play_decoded(audio_channel, "FLAC", metadata.rate, metadata.channels, metadata.frames, flac.decoder()) };
	}
	if bytes.starts_with(b"ID3") || bytes.first() == Some(&0xff) && bytes.get(1).is_some_and(|byte| byte & 0xe0 == 0xe0) {
		let mp3 = Mp3::parse(bytes).map_err(|_| ())?;
		let metadata = mp3.metadata();
		return unsafe { play_decoded(audio_channel, "MP3", metadata.rate, metadata.channels, metadata.frames, mp3.decoder()) };
	}
	Err(())
}

trait PcmDecoder {
	fn remaining_frames(&self) -> u64;
	fn read_i16_le(&mut self, max_frames: usize, output: &mut Vec<u8>) -> Result<usize, ()>;
}

impl PcmDecoder for wav::Decoder<'_> {
	fn remaining_frames(&self) -> u64 {
		self.remaining_frames()
	}

	fn read_i16_le(&mut self, max_frames: usize, output: &mut Vec<u8>) -> Result<usize, ()> {
		self.read_i16_le(max_frames, output).map_err(|_| ())
	}
}

impl PcmDecoder for aiff::Decoder<'_> {
	fn remaining_frames(&self) -> u64 {
		self.remaining_frames()
	}

	fn read_i16_le(&mut self, max_frames: usize, output: &mut Vec<u8>) -> Result<usize, ()> {
		self.read_i16_le(max_frames, output).map_err(|_| ())
	}
}

impl PcmDecoder for flac::Decoder<'_> {
	fn remaining_frames(&self) -> u64 {
		self.remaining_frames()
	}

	fn read_i16_le(&mut self, max_frames: usize, output: &mut Vec<u8>) -> Result<usize, ()> {
		self.read_i16_le(max_frames, output).map_err(|_| ())
	}
}

impl PcmDecoder for mp3::Decoder<'_> {
	fn remaining_frames(&self) -> u64 {
		self.remaining_frames()
	}

	fn read_i16_le(&mut self, max_frames: usize, output: &mut Vec<u8>) -> Result<usize, ()> {
		self.read_i16_le(max_frames, output).map_err(|_| ())
	}
}

unsafe fn play_decoded(audio_channel: u64, container: &str, rate: u32, channels: u8, frames_total: u64, mut decoder: impl PcmDecoder) -> Result<(), ()> {
	let mut root = audio::Client::new(ChannelTransport { chan: audio_channel });
	let stream_channel = root.open_stream(&rate, &channels).and_then(Result::ok).ok_or(())?;
	let mut stream = pcm_stream::Client::new(ChannelTransport { chan: stream_channel });
	print_metadata(container, rate, channels, frames_total);
	let mut pcm = Vec::new();
	while decoder.remaining_frames() != 0 {
		if unsafe { interrupted() } {
			let _ = stream.close();
			return Ok(());
		}
		let frames = decoder.read_i16_le(CHUNK_FRAMES, &mut pcm)?;
		if frames == 0 {
			break;
		}
		let frame_bytes = channels as usize * 2;
		let mut accepted = 0usize;
		while accepted < frames {
			if unsafe { interrupted() } {
				let _ = stream.close();
				return Ok(());
			}
			let start = accepted.checked_mul(frame_bytes).ok_or(())?;
			let buffer = unsafe { make_buffer(&pcm[start..]) }.ok_or(())?;
			let count = stream.write(&buffer).and_then(Result::ok).ok_or(())? as usize;
			if count == 0 || count > frames - accepted {
				return Err(());
			}
			accepted += count;
		}
	}
	stream.close().and_then(Result::ok).ok_or(())
}

fn print_metadata(container: &str, rate: u32, channels: u8, frames: u64) {
	let mut line = String::from("play: ");
	line.push_str(container);
	line.push(' ');
	tools::push_decimal(&mut line, rate as u64);
	line.push_str(" Hz, ");
	tools::push_decimal(&mut line, channels as u64);
	line.push_str(if channels == 1 { " channel, " } else { " channels, " });
	tools::push_decimal(&mut line, frames);
	line.push_str(" frames\n");
	unsafe { print(line.as_bytes()) };
}
