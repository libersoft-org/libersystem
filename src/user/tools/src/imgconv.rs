// imgconv - governed image conversion over volume capabilities only.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use imgconv::Error;
use proto::path;
use proto::system::{volume, OpenOpts};
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buffer = [0u8; 1024];
	unsafe {
		inherit_stdout(bootstrap);
		let args = match recv_blocking(bootstrap, &mut buffer) {
			Received::Message { len, .. } => buffer[..len].to_vec(),
			Received::Closed => exit(),
		};
		let system = recv_tagged(bootstrap, &mut buffer, b"SYSTEM").unwrap_or(0);
		let media = recv_tagged(bootstrap, &mut buffer, b"MEDIA").unwrap_or(0);
		let iso = recv_tagged(bootstrap, &mut buffer, b"ISO").unwrap_or(0);
		let udf = recv_tagged(bootstrap, &mut buffer, b"UDF").unwrap_or(0);
		let usb = recv_tagged(bootstrap, &mut buffer, b"USB").unwrap_or(0);
		let cwd = match recv_blocking(bootstrap, &mut buffer) {
			Received::Message { len, .. } => buffer[..len].to_vec(),
			Received::Closed => Vec::new(),
		};
		let cwd = core::str::from_utf8(&cwd).unwrap_or("");
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
			print(b"imgconv: volume unavailable\n");
			exit();
		}
		let input = match read_file(input_storage, &input_uri) {
			Some(input) => input,
			None => {
				print(b"imgconv: cannot read input\n");
				exit();
			}
		};
		if !config.force && exists(output_storage, &output_uri) {
			print(b"imgconv: destination exists (use --force)\n");
			exit();
		}
		let (encoded, info) = match imgconv::convert(&input, &config) {
			Ok(result) => result,
			Err(error) => fail(error),
		};
		let staged = match make_buffer(&encoded) {
			Some(staged) => staged,
			None => {
				print(b"imgconv: out of memory\n");
				exit();
			}
		};
		let mut client = volume::Client::new(ChannelTransport { chan: output_storage });
		if !matches!(client.write(&output_uri, &staged), Some(Ok(()))) {
			print(b"imgconv: cannot write output\n");
			exit();
		}
		let mut line = String::from("imgconv: ");
		line.push_str(info.input_format.name());
		line.push(' ');
		tools::push_decimal(&mut line, info.source_width as u64);
		line.push('x');
		tools::push_decimal(&mut line, info.source_height as u64);
		line.push_str(" -> ");
		line.push_str(info.output_format.name());
		line.push(' ');
		tools::push_decimal(&mut line, info.output_width as u64);
		line.push('x');
		tools::push_decimal(&mut line, info.output_height as u64);
		if let Some(mode) = info.mode {
			line.push_str(match mode {
				imgconv::Mode::Lossless => " mode=lossless",
				imgconv::Mode::Lossy => " mode=lossy",
			});
		}
		if let Some(quality) = info.quality {
			line.push_str(" quality=");
			tools::push_decimal(&mut line, quality as u64);
		}
		if let Some(compression) = info.compression {
			line.push_str(" compression=");
			tools::push_decimal(&mut line, compression as u64);
		}
		line.push_str(" bytes=");
		tools::push_decimal(&mut line, info.output_bytes as u64);
		line.push_str(" metadata=stripped\n");
		print(line.as_bytes());
	}
	exit();
}

unsafe fn read_file(storage: u64, uri: &str) -> Option<Vec<u8>> {
	unsafe {
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
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
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
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
