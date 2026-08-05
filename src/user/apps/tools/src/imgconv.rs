// imgconv - governed image conversion over volume capabilities only.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use imgconv::Error;
use ipc_client::make_buffer;
use proto::system::{LaunchContext, OpenOpts};
use rt::*;
use storage_proto::path;
use volume_client::VolumeClient;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buffer = [0u8; 1024];
	unsafe {
		inherit_stdout(bootstrap);
		set_alloc_error_message(b"imgconv: out of memory\n");
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let argument: &[u8] = context.arguments.as_bytes();
		let args = argument.to_vec();
		// Taken BY NAME out of the bundle, which ends at READY. The volumes this tool has no use
		// for are simply not taken, and the set closes them when it drops - where before they had
		// to be drained by hand, because a message left on the channel was read as the NEXT thing
		// this tool expected, and the thing after the bundle is the working directory.
		let mut volumes: CapSet = recv_caps(bootstrap);
		let system = volumes.take(CAP_SYSTEM);
		let media = volumes.take(CAP_MEDIA);
		let iso = volumes.take(CAP_ISO);
		let udf = volumes.take(CAP_UDF);
		let usb = volumes.take(CAP_USB);
		let cwd: &str = &context.cwd;
		if trim(&args) == b"--help" {
			print(imgconv::help_text().as_bytes());
			exit();
		}
		let config = match imgconv::parse_args(&args) {
			Ok(config) => config,
			Err(error) => fail(error),
		};
		let input_uri = match path::resolve(cwd, config.input.as_bytes()) {
			Some(uri) => uri,
			None => fail(Error::InvalidOptions),
		};
		let output_uri = match path::resolve(cwd, config.output.as_bytes()) {
			Some(uri) => uri,
			None => fail(Error::InvalidOptions),
		};
		let input_storage = path::volume_client(cwd, config.input.as_bytes(), system, media, iso, udf, usb);
		let output_storage = path::volume_client(cwd, config.output.as_bytes(), system, media, iso, udf, usb);
		if input_storage == 0 || output_storage == 0 {
			eprint(b"imgconv: volume unavailable\n");
			exit();
		}
		let input = match read_file(input_storage, &input_uri) {
			Some(input) => input,
			None => {
				eprint(b"imgconv: cannot read input\n");
				exit();
			}
		};
		if !config.force && exists(output_storage, &output_uri) {
			eprint(b"imgconv: destination exists (use --force)\n");
			exit();
		}
		let (encoded, info) = match imgconv::convert(&input, &config) {
			Ok(result) => result,
			Err(error) => fail(error),
		};
		let staged = match make_buffer(&encoded) {
			Some(staged) => staged,
			None => {
				eprint(b"imgconv: out of memory\n");
				exit();
			}
		};
		let mut client = VolumeClient::new(output_storage);
		if !matches!(client.write(&output_uri, &staged), Some(Ok(()))) {
			eprint(b"imgconv: cannot write output\n");
			exit();
		}
		let mut line = String::from("imgconv: ");
		line.push_str(info.input_format.name());
		line.push(' ');
		push_decimal(&mut line, info.source_width as u64);
		line.push('x');
		push_decimal(&mut line, info.source_height as u64);
		line.push_str(" -> ");
		line.push_str(info.output_format.name());
		line.push(' ');
		push_decimal(&mut line, info.output_width as u64);
		line.push('x');
		push_decimal(&mut line, info.output_height as u64);
		if let Some(mode) = info.mode {
			line.push_str(match mode {
				imgconv::Mode::Lossless => " mode=lossless",
				imgconv::Mode::Lossy => " mode=lossy",
			});
		}
		if let Some(quality) = info.quality {
			line.push_str(" quality=");
			push_decimal(&mut line, quality as u64);
		}
		if let Some(compression) = info.compression {
			line.push_str(" compression=");
			push_decimal(&mut line, compression as u64);
		}
		line.push_str(" bytes=");
		push_decimal(&mut line, info.output_bytes as u64);
		line.push_str(" metadata=stripped\n");
		print(line.as_bytes());
	}
	exit();
}

fn trim(mut bytes: &[u8]) -> &[u8] {
	while bytes.first().is_some_and(|byte| byte.is_ascii_whitespace()) {
		bytes = &bytes[1..];
	}
	while bytes.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
		bytes = &bytes[..bytes.len() - 1];
	}
	bytes
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
		Error::InvalidOptions => b"imgconv: invalid options\n".as_slice(),
		Error::UnsupportedOption => b"imgconv: option not supported by output format\n".as_slice(),
		Error::UnsupportedFormat => b"imgconv: unsupported image format\n".as_slice(),
		Error::InvalidImage => b"imgconv: invalid or corrupt image\n".as_slice(),
		Error::TooLarge => b"imgconv: image is too large\n".as_slice(),
	};
	unsafe { print(message) };
	exit()
}
