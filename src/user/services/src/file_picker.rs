// file_picker - the powerbox file-picker service.
//
// ServiceManager (or a scenario) starts this program and hands it a bootstrap
// channel, over which it receives a StorageService client - the capability the
// picker is trusted with - and a "SERVE" channel its clients reach it on. Over that
// channel a caller speaks the generated `liber:system` Picker bindings: `pick`
// prompts the user and hands back the chosen file as a capability (handle<file>).
//
// This is the powerbox pattern: a caller with no filesystem access of its own gains
// access to exactly the picked file, and to nothing else - authority flows from the
// act of picking, not from ambient filesystem access. The picker holds the trusted
// StorageService client; it opens only the file the user chose and transfers that
// one handle back. (Phase 1 simulates the user's choice with a fixed file; a real
// picker would prompt. The mechanism - a capability granted by picking - is the
// point.)

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use proto::codec::Transport;
use proto::system::picker::{self, Service};
use proto::system::{volume, Error, OpenOpts, Picked};
use rt::*;

// The file the picker grants. Phase 1 stands in for the user's choice with a fixed
// file (deliberately a different file than the wasi_host's M28 default, to show the
// granted file comes from the pick, not from the caller).
const PICKED_PATH: &[u8] = b"vol://system/motd.txt";
const PICKED_NAME: &[u8] = b"motd.txt";

// A proto Transport over an rt channel: send the request, then block for the reply.
struct ChannelTransport {
	chan: u64,
}

impl Transport for ChannelTransport {
	fn call(&mut self, request: &[u8], request_handle: u64) -> Option<(alloc::vec::Vec<u8>, u64)> {
		unsafe {
			if !send_blocking(self.chan, request, request_handle) {
				return None;
			}
			let mut reply: [u8; 256] = [0u8; 256];
			match recv_blocking(self.chan, &mut reply) {
				Received::Message { len, handle } => Some((reply[..len].to_vec(), handle)),
				Received::Closed => None,
			}
		}
	}
}

// The picker, holding its trusted StorageService client.
struct Picker {
	storage: u64,
}

impl Service for Picker {
	fn pick(&mut self) -> Result<Picked, Error> {
		// open the chosen file over StorageService and hand the caller that one file
		// capability (the open reply's handle<file>, transferred onward by `pick`).
		let opts: OpenOpts = OpenOpts { path: String::from_utf8_lossy(PICKED_PATH).into_owned(), write: false, create: false };
		let mut client = volume::Client::new(ChannelTransport { chan: self.storage });
		match client.open(&opts) {
			Some(Ok(result)) => Ok(Picked { file: result.file, size: result.size, name: String::from_utf8_lossy(PICKED_NAME).into_owned() }),
			Some(Err(e)) => Err(e),
			None => Err(Error::Again),
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. receive the StorageService client the picker is trusted with.
	let storage: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 && &buf[..7] == b"STORAGE" => handle,
		_ => exit(),
	};

	// 2. wait for the serve channel clients reach us on.
	let service: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 5 && &buf[..5] == b"SERVE" => handle,
		_ => exit(),
	};

	// 3. report in to the supervisor that started us.
	unsafe {
		send_blocking(bootstrap, b"FilePicker: online", 0);
	}

	// 4. serve generated pick requests until the client side closes. The pick reply
	//    transfers the picked file's handle out-of-band (reply_handle).
	let mut picker: Picker = Picker { storage };
	let mut request: [u8; 256] = [0u8; 256];
	let mut reply: [u8; 256] = [0u8; 256];
	loop {
		match unsafe { recv_blocking(service, &mut request) } {
			Received::Message { len, .. } if len == 0 => break,
			Received::Message { len, handle } => {
				let mut reply_handle: u64 = 0;
				if let Some(n) = picker::dispatch(&mut picker, &request[..len], handle, &mut reply, &mut reply_handle) {
					unsafe { send_blocking(service, &reply[..n], reply_handle) };
				}
			}
			Received::Closed => break,
		}
	}
	exit();
}
