// share - the host share's one reader, on a development machine: `share ls [PATH]` lists a directory of `vol://host`,
// `share cat PATH` prints a file of it. DEVELOPMENT-ONLY.
//
// ITS ONE GRANT IS THE HOST SHARE - not the volumes - so what it reads it can only print, and a tool that holds the
// volumes never reaches the host: `cat vol://host/...` names a volume its grant does not carry. That separation is
// what the host-share gate checks, from both sides.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use ipc_client::ChannelTransport;
use proto::system::{Error, FileType, LaunchContext, OpenOpts, volume};
use rt::*;

fn fail(line: &str) -> ! {
	eprint(format!("share: {line}\n").as_bytes());
	exit_with(1);
}

fn why(error: Error) -> &'static str {
	match error {
		Error::NotFound => "not found",
		Error::Denied => "denied",
		Error::Invalid => "refused - not a name, a file or a directory this share holds",
		Error::Again => "the share is not there now; try again",
		Error::Unsupported => "not supported by the share",
		_ => "the share could not answer",
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	inherit_stdout(bootstrap);
	let launch = recv_launch_bytes(bootstrap);
	let context = launch.as_deref().and_then(LaunchContext::decode);
	let arguments = context.as_ref().map_or("", |context| context.arguments.trim());
	let mut buf = [0u8; 128];
	let grant = recv_tagged(bootstrap, &mut buf, b"HOST_SHARE").unwrap_or(0);
	if grant == 0 {
		fail("no host share was granted - this machine was started without one, or this build carries none");
	}
	let (verb, path) = match arguments.split_once(' ') {
		Some((verb, path)) => (verb, path.trim()),
		None => (arguments, ""),
	};
	let mut uri = String::from("vol://host/");
	uri.push_str(path.trim_start_matches('/'));
	let mut client = volume::Client::new(ChannelTransport { chan: grant });
	match verb {
		"ls" => {
			let consumer = match client.list(&uri) {
				Some(Ok(consumer)) => consumer,
				Some(Err(error)) => fail(&format!("{path}: {}", why(error))),
				None => fail("the host share's service did not answer"),
			};
			let Some(entries) = drain_stream_complete(consumer, volume::list_read) else { fail("the listing failed partway through") };
			for entry in &entries {
				let kind = if matches!(entry.r#type, FileType::Dir) { 'd' } else { '-' };
				write_stdout(format!("{kind} {:>10} {}\n", entry.size, entry.name).as_bytes());
			}
			write_stdout(format!("share: {} entries\n", entries.len()).as_bytes());
		}
		"cat" if !path.is_empty() => {
			let opened = match client.open(&OpenOpts { path: uri, write: false, create: false }) {
				Some(Ok(opened)) => opened,
				Some(Err(error)) => fail(&format!("{path}: {}", why(error))),
				None => fail("the host share's service did not answer"),
			};
			if opened.file != 0 && opened.size != 0 {
				// SAFETY: the object the service answered with, mapped here alone for as long as it is read.
				let Some(mapped) = (unsafe { map_object(opened.file) }) else { fail("the file's buffer could not be mapped") };
				let contents = unsafe { core::slice::from_raw_parts(mapped as *const u8, opened.size as usize) };
				if write_stdout(contents) && contents.last() != Some(&b'\n') {
					write_stdout(b"\n");
				}
				unmap_object(opened.file);
			}
			if opened.file != 0 {
				close(opened.file);
			}
		}
		_ => fail("usage: share ls [PATH] | share cat PATH"),
	}
	exit();
}
