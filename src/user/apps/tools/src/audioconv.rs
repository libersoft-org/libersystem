// audioconv - governed audio conversion over volume capabilities only.
//
// The tool holds volumes and nothing else. It never touches AudioService and never asks for device
// authority, because converting a file is not playing one - a distinction the capability bundle
// makes rather than the code.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use audioconv::Error;
use ipc_client::make_buffer;
use proto::system::{LaunchContext, OpenOpts};
use rt::*;
use storage_proto::path;
use volume_client::VolumeClient;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		inherit_stdout(bootstrap);
		set_alloc_error_message(b"audioconv: out of memory\n");
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let args = context.arguments.as_bytes().to_vec();
		// Taken by name out of the bundle, which ends at READY. What is not taken is closed when the
		// set drops, so a volume this run has no use for is never held.
		let mut volumes: CapSet = recv_caps(bootstrap);
		let system = volumes.take(CAP_SYSTEM);
		let media = volumes.take(CAP_MEDIA);
		let iso = volumes.take(CAP_ISO);
		let udf = volumes.take(CAP_UDF);
		let usb = volumes.take(CAP_USB);
		let cwd: &str = &context.cwd;

		let config = match audioconv::parse_args(&args) {
			Ok(config) => config,
			Err(error) => fail(error),
		};
		if config.mode == audioconv::Mode::Help {
			print(audioconv::help_text().as_bytes());
			exit();
		}
		let Some(input_uri) = path::resolve(cwd, config.input.as_bytes()) else { fail(Error::InvalidOptions) };
		let Some(output_uri) = path::resolve(cwd, config.output.as_bytes()) else { fail(Error::InvalidOptions) };
		let input_storage = path::volume_client(cwd, config.input.as_bytes(), system, media, iso, udf, usb, path::NOT_GRANTED, path::NOT_GRANTED);
		let output_storage = path::volume_client(cwd, config.output.as_bytes(), system, media, iso, udf, usb, path::NOT_GRANTED, path::NOT_GRANTED);
		if input_storage == 0 || output_storage == 0 {
			eprint(b"audioconv: volume unavailable\n");
			exit();
		}
		let Some(input) = read_file(input_storage, &input_uri) else {
			eprint(b"audioconv: cannot read input\n");
			exit();
		};
		// Asked BEFORE the conversion, so an hour of decoding is not spent to find out the
		// destination was there all along.
		if !config.force && exists(output_storage, &output_uri) {
			eprint(b"audioconv: destination exists (use --force)\n");
			exit();
		}

		// The destination is written ONLY once the whole conversion has succeeded. An encode that
		// runs out of room, or an input that turns out to be damaged halfway through, leaves what
		// was already there untouched - which is the difference between a failed conversion and a
		// lost file.
		let (encoded, info) = match audioconv::convert(&input, &config) {
			Ok(result) => result,
			Err(error) => fail(error),
		};
		let Some(staged) = make_buffer(&encoded) else {
			eprint(b"audioconv: out of memory\n");
			exit();
		};
		let mut client = VolumeClient::new(output_storage);
		if !matches!(client.write(&output_uri, &staged), Some(Ok(()))) {
			eprint(b"audioconv: cannot write output\n");
			exit();
		}

		let mut line = String::from("audioconv: ");
		line.push_str(info.source.name());
		push_shape(&mut line, info.source_rate, info.source_channels, info.source_frames);
		line.push_str(" -> ");
		line.push_str(audioconv::capabilities(info.destination).name);
		push_shape(&mut line, info.rate, info.channels, info.frames);
		line.push_str(" duration=");
		push_decimal(&mut line, info.duration_ms);
		line.push_str("ms bytes=");
		push_decimal(&mut line, info.bytes);
		if info.stripped_metadata {
			line.push_str(" metadata=stripped");
		}
		line.push('\n');
		print(line.as_bytes());
	}
	exit();
}

// Rate, channels and length, the three numbers that say what a file actually is.
fn push_shape(out: &mut String, rate: u32, channels: u8, frames: u64) {
	out.push(' ');
	push_decimal(out, rate as u64);
	out.push_str("Hz/");
	push_decimal(out, channels as u64);
	out.push_str("ch/");
	push_decimal(out, frames);
	out.push_str("fr");
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

unsafe fn read_file(storage: u64, uri: &str) -> Option<Vec<u8>> {
	unsafe {
		let mut client = VolumeClient::new(storage);
		let opened = client.open(&OpenOpts { path: String::from(uri), write: false, create: false })?.ok()?;
		let len = usize::try_from(opened.size).ok()?;
		if opened.file == 0 || len == 0 {
			if opened.file != 0 {
				close(opened.file);
			}
			return None;
		}
		let mapped = map_object(opened.file)?;
		let bytes = core::slice::from_raw_parts(mapped as *const u8, len).to_vec();
		unmap_object(opened.file);
		close(opened.file);
		Some(bytes)
	}
}

unsafe fn exists(storage: u64, uri: &str) -> bool {
	unsafe {
		let mut client = VolumeClient::new(storage);
		match client.open(&OpenOpts { path: String::from(uri), write: false, create: false }) {
			Some(Ok(opened)) => {
				if opened.file != 0 {
					close(opened.file);
				}
				true
			}
			_ => false,
		}
	}
}

fn fail(error: Error) -> ! {
	let message = match error {
		Error::InvalidOptions => b"audioconv: invalid options\n".as_slice(),
		// Two cases behind one answer, and the message says both: an option this destination has no
		// use for (`--quality` on a lossless format), and a RATE it cannot carry - MPEG-1 Layer III
		// names 32, 44.1 and 48 kHz and nothing else, so an 8 kHz source needs `--rate` to become
		// an MP3 at all.
		Error::UnsupportedOption => b"audioconv: the output format does not support that option or sample rate\n".as_slice(),
		Error::UnsupportedFormat => b"audioconv: unsupported audio format\n".as_slice(),
		Error::InvalidAudio => b"audioconv: invalid or corrupt audio\n".as_slice(),
		// Distinct from Unsupported on purpose: the format is real and this tool will write it one
		// day, which is a different thing for a reader to be told.
		Error::NotImplemented => b"audioconv: writing that format is not implemented yet\n".as_slice(),
		Error::TooLarge => b"audioconv: the conversion does not fit\n".as_slice(),
	};
	unsafe { print(message) };
	exit()
}
