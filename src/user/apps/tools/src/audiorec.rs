// audiorec - governed PCM capture to a WAV file.
//
// The tool holds two things and nothing else: somewhere to write, and the authority to RECORD. Not
// `audio-stream`, which would let it make a sound and is not what recording needs, and not `audio`,
// which is the whole service. "May record" is a microphone, so it is a grant of its own - see
// `audio-capture` in `security.lsidl`.
//
// NOTHING IS HELD. Periods arrive from AudioService one at a time, are converted straight into the
// encoder, and the encoder writes straight into a storage TRANSACTION. An hour of audio costs one
// period of memory, and a run that is interrupted, runs out of room, or loses the device leaves the
// destination exactly as it was - because the transaction is never committed. That is what makes
// "abort rather than leave an apparently valid truncated file" the default path rather than an
// error path somebody has to remember to write.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use audio_client::{AudioClient, PcmCaptureClient};
use audiorec::{Config, Error, Mode};
use pcm::Format;
use pcm::encode::{Sink, SinkError};
use proto::system::{LaunchContext, WriterMode};
use rt::*;
use storage_proto::path;
use volume_client::{VolumeClient, WRITER_CHUNK, WriterClient};
use wav::encode::{Encoder, Output};

// A `pcm::encode::Sink` over a storage write transaction.
//
// `patch` is the whole reason this is a transaction and not a stream: a RIFF header carries two
// lengths that are only known when the recording ends, and `writer.write-at` is what corrects them
// before the commit. A destination that could only go forward would have to be refused by the
// encoder at construction, which is exactly what `pcm::encode::ForwardOnly` exists to say.
struct WriterSink {
	writer: WriterClient,
	written: u64,
	failed: bool,
}

impl Sink for WriterSink {
	fn write(&mut self, bytes: &[u8]) -> Result<(), SinkError> {
		// One `write` carries at most `WRITER_CHUNK`; a period is smaller than that, but the header
		// and a future larger period are not this function's business to assume about.
		for chunk in bytes.chunks(WRITER_CHUNK) {
			match self.writer.write(chunk) {
				Some(Ok(_)) => {}
				_ => {
					self.failed = true;
					return Err(SinkError::Full);
				}
			}
		}
		self.written += bytes.len() as u64;
		Ok(())
	}

	fn patch(&mut self, at: u64, bytes: &[u8]) -> Result<(), SinkError> {
		match self.writer.write_at(at, bytes) {
			Some(Ok(())) => Ok(()),
			_ => {
				self.failed = true;
				Err(SinkError::Full)
			}
		}
	}

	fn written(&self) -> u64 {
		self.written
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		inherit_stdout(bootstrap);
		set_alloc_error_message(b"audiorec: out of memory\n");
		let mut buf: [u8; 256] = [0; 256];
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let args = context.arguments.as_bytes().to_vec();
		let mut volumes: CapSet = recv_caps(bootstrap);
		let system = volumes.take(CAP_SYSTEM);
		let media = volumes.take(CAP_MEDIA);
		let iso = volumes.take(CAP_ISO);
		let udf = volumes.take(CAP_UDF);
		let usb = volumes.take(CAP_USB);
		let audio_channel = recv_tagged(bootstrap, &mut buf, b"AUDIO_CAPTURE").unwrap_or(0);
		let cwd: &str = &context.cwd;

		let config: Config = match audiorec::parse_args(&args) {
			Ok(config) => config,
			Err(error) => fail(error),
		};
		if config.mode == Mode::Help {
			print(audiorec::help_text().as_bytes());
			exit();
		}
		let Some(output_uri) = path::resolve(cwd, config.output.as_bytes()) else { fail(Error::InvalidOptions) };
		let storage = path::volume_client(cwd, config.output.as_bytes(), system, media, iso, udf, usb, path::NOT_GRANTED, path::NOT_GRANTED);
		if storage == 0 {
			eprint(b"audiorec: volume unavailable\n");
			exit();
		}
		if audio_channel == 0 {
			eprint(b"audiorec: no capture authority\n");
			exit();
		}
		// Asked BEFORE the recording, so an hour of capture is not spent to find out the
		// destination was there all along.
		if !config.force && exists(storage, &output_uri) {
			eprint(b"audiorec: destination exists (use --force)\n");
			exit();
		}

		// The capture stream comes first. A machine whose sound device has no input stream says so
		// on the first read, so nothing is opened for writing until there is something to write.
		let capture: u64 = match AudioClient::new(audio_channel).open_capture(&config.rate, &config.channels) {
			Some(Ok(handle)) if handle != 0 => handle,
			_ => {
				eprint(b"audiorec: no capture stream on this machine\n");
				exit();
			}
		};

		let mut volume = VolumeClient::new(storage);
		let writer: WriterClient = match volume.open_writer(&output_uri, WriterMode::Replace) {
			Some(Ok(writer)) => writer,
			_ => {
				close(capture);
				eprint(b"audiorec: cannot write to that destination\n");
				exit();
			}
		};
		catch_interrupt();
		record(capture, writer, &config);
	}
	exit();
}

// The recording itself. Returns nothing: every outcome is reported here, because the difference
// between the outcomes is what the caller needs to be told.
unsafe fn record(capture: u64, writer: WriterClient, config: &Config) {
	unsafe {
		// `WriterClient` is a handle, so the copy kept here and the one inside the sink name the
		// same transaction: an abort through either gives the destination back.
		let format: Format = match Format::new(config.rate, config.channels) {
			Some(format) => format,
			None => {
				abort(writer, capture, b"audiorec: unsupported format\n");
				return;
			}
		};
		let sink = WriterSink { writer, written: 0, failed: false };
		let mut encoder: Encoder<WriterSink> = match Encoder::new(sink, format, Output::Pcm { bits: 16 }) {
			Ok(encoder) => encoder,
			Err(_) => {
				abort(writer, capture, b"audiorec: cannot start a WAV file here\n");
				return;
			}
		};

		let mut client = PcmCaptureClient::new(capture);
		let limit: u64 = config.frame_limit();
		let mut frames: u64 = 0;
		// CAPTURED AUDIO THAT DID NOT REACH THE FILE. A device period does not divide the requested
		// length, so the period that straddles the end carries a few frames past it; the same
		// happens at the RIFF ceiling. Milliseconds, but real ones - reported rather than rounded
		// away, because a recording that is shorter than the microphone was open is a fact about the
		// recording rather than an implementation detail.
		let mut dropped: u64 = 0;
		let mut peak: usize = 0;
		let mut stopped_early = false;
		let mut samples: Vec<i16> = Vec::new();
		loop {
			if interrupted() {
				break;
			}
			let period: Vec<u8> = match client.read() {
				Some(Ok(period)) if !period.is_empty() => period,
				// An empty answer is the stream ending; anything else is the device going away.
				Some(Ok(_)) => break,
				Some(Err(_)) | None => {
					abort(writer, capture, b"audiorec: the capture stream ended in an error\n");
					return;
				}
			};
			peak = peak.max(period.len());
			samples.clear();
			samples.reserve(period.len() / 2);
			for sample in period.chunks_exact(2) {
				samples.push(i16::from_le_bytes([sample[0], sample[1]]));
			}
			let channels = format.channels() as usize;
			let mut available = (samples.len() / channels) as u64;
			if frames + available > limit {
				dropped += frames + available - limit;
				available = limit - frames;
				stopped_early = true;
			}
			if available != 0 && encoder.push(&samples[..available as usize * channels]).is_err() {
				abort(writer, capture, b"audiorec: the destination refused the audio\n");
				return;
			}
			frames += available;
			if stopped_early {
				break;
			}
		}

		let (mut sink, written_frames) = match encoder.finish() {
			Ok(result) => result,
			Err(_) => {
				abort(writer, capture, b"audiorec: could not finish the WAV file\n");
				return;
			}
		};
		close(capture);
		if sink.failed {
			let _ = sink.writer.abort();
			close(sink.writer.handle());
			eprint(b"audiorec: the destination refused the audio\n");
			return;
		}
		let bytes: u64 = match sink.writer.commit() {
			Some(Ok(bytes)) => bytes,
			_ => {
				let _ = sink.writer.abort();
				close(sink.writer.handle());
				eprint(b"audiorec: the recording could not be published\n");
				return;
			}
		};
		close(sink.writer.handle());
		report(config, written_frames, bytes, dropped, peak);
	}
}

// Give the destination back untouched and say why. The transaction is what makes this leave nothing
// behind: an aborted session publishes nothing, so what is on the volume is what was there before.
unsafe fn abort(mut writer: WriterClient, capture: u64, message: &[u8]) {
	unsafe {
		let _ = writer.abort();
		close(writer.handle());
		close(capture);
		eprint(message);
	}
}

fn report(config: &Config, frames: u64, bytes: u64, dropped: u64, peak: usize) {
	let mut line = String::from("audiorec: ");
	push_decimal(&mut line, config.rate as u64);
	line.push_str("Hz/");
	push_decimal(&mut line, config.channels as u64);
	line.push_str("ch/16-bit ");
	push_decimal(&mut line, frames);
	line.push_str("fr duration=");
	push_decimal(&mut line, frames * 1_000 / config.rate.max(1) as u64);
	line.push_str("ms bytes=");
	push_decimal(&mut line, bytes);
	line.push_str(" dropped=");
	push_decimal(&mut line, dropped);
	// The recording is never held, so the peak is one period: the number is here to say that out
	// loud rather than to be watched.
	line.push_str(" peak=");
	push_decimal(&mut line, peak as u64);
	line.push('\n');
	unsafe { print(line.as_bytes()) };
}

fn push_decimal(out: &mut String, value: u64) {
	let mut digits = [0u8; 20];
	let mut value = value;
	let mut len = 0;
	loop {
		digits[len] = b'0' + (value % 10) as u8;
		value /= 10;
		len += 1;
		if value == 0 {
			break;
		}
	}
	for index in (0..len).rev() {
		out.push(digits[index] as char);
	}
}

fn exists(storage: u64, uri: &str) -> bool {
	matches!(VolumeClient::new(storage).stat(uri), Some(Ok(_)))
}

fn fail(error: Error) -> ! {
	unsafe {
		eprint(error.message().as_bytes());
		exit()
	}
}
