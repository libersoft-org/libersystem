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
use proto::system::picker::{self, Service};
use proto::system::{Error, OpenOpts, Picked, volume};
use rt::*;

// The file the picker grants. Phase 1 stands in for the user's choice with a fixed
// file (deliberately a different file than the wasi_host's default, to show the
// granted file comes from the pick, not from the caller).
const PICKED_PATH: &[u8] = b"vol://system/motd.txt";
const PICKED_NAME: &[u8] = b"motd.txt";

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
	let storage: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"STORAGE") }.unwrap_or_else(|| exit());

	// 2. wait for the serve channel clients reach us on.
	let service: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"SERVE") }.unwrap_or_else(|| exit());

	// 3. report in to the supervisor that started us.
	unsafe {
		send_blocking(bootstrap, b"FilePicker: online", 0);
	}

	// 4. serve generated pick requests until the client side closes. The pick reply
	//    transfers the picked file's handle out-of-band (reply_handle).
	let mut picker: Picker = Picker { storage };
	let mut request: [u8; 256] = [0u8; 256];
	let mut reply: [u8; 256] = [0u8; 256];
	unsafe {
		serve(service, &mut request, &mut reply, |req: &[u8], handle: &mut u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> { picker::dispatch(&mut picker, req, handle, out, reply_handle) });
	}
	exit();
}
