// dfu - ask for a firmware image to be written to a USB DFU target, through the trusted administrative path.
//
//   dfu TARGET IMAGE
//
// TARGET names the device the way the DFU executor answers to: `dfu:VVVV:PPPP` by vendor and product, or the
// live binding it prints (`dfu:port3.0/if0/1d6b:0104`). IMAGE is a file, relative to the working directory or an
// absolute URI - a DFU 1.1 image, whose suffix, where it has one, the executor holds against the target.
//
// NOTHING IS WRITTEN BECAUSE THIS PROGRAM ASKED. It puts one request to AdminService on the request connection
// PermissionManager minted for this launch - the `dfu` scope: firmware download, on `dfu:` targets, and nothing
// else - and AdminService shows the operation, frozen, on the protected screen. Only a person's confirmation
// there turns the request into a grant, and the grant into one attempt; a decline, or no answer, writes nothing.
// What the device then reports is printed as it came: completed, refused, or an end nobody observed - which is
// never retried and never described as rolled back, because DFU gives a host no rollback to promise.

#![no_std]
#![no_main]

extern crate alloc;

use admin_client::{AdminAuthorityClient, AdminRequestClient};
use alloc::format;
use alloc::string::String;
use proto::system::{AdminAction, AdminAnswer, AdminRequestArgs, AdminResult, LaunchContext, OpenOpts};
use rt::*;
use storage_proto::path;
use volume_client::VolumeClient;

fn say(line: &str) {
	eprint(b"dfu: ");
	eprint(line.as_bytes());
	eprint(b"\n");
}

fn fail(line: &str) -> ! {
	say(line);
	exit();
}

// The image, copied out of the file into an object of this program's own, and a read-only handle to it for the
// request - so what AdminService freezes is what the file held when it was asked, whatever the file does next.
unsafe fn image_of(storage: u64, uri: &str) -> (u64, u32) {
	unsafe {
		let opts = OpenOpts { path: String::from(uri), write: false, create: false };
		let opened = match VolumeClient::new(storage).open(&opts) {
			Some(Ok(opened)) => opened,
			_ => fail(&format!("{uri}: cannot open")),
		};
		if opened.file == 0 || opened.size == 0 || opened.size > u32::MAX as u64 {
			fail(&format!("{uri}: empty, or too large to be an image"));
		}
		let size = opened.size as usize;
		let Some(from) = map_object(opened.file) else { fail(&format!("{uri}: cannot be mapped")) };
		let copy = memory_object_create(size as u64);
		if copy < 0 {
			fail("no memory for the image");
		}
		let copy = copy as u64;
		let Some(to) = map_object(copy) else { fail("the image's copy cannot be mapped") };
		core::ptr::copy_nonoverlapping(from as *const u8, to as *mut u8, size);
		unmap_object(opened.file);
		close(opened.file);
		unmap_object(copy);
		let shared = duplicate(copy, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		close(copy);
		if shared < 0 {
			fail("the image cannot be handed over");
		}
		(shared as u64, size as u32)
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let mut volumes: CapSet = recv_caps(bootstrap);
	let system = volumes.take(CAP_SYSTEM);
	let media = volumes.take(CAP_MEDIA);
	let iso = volumes.take(CAP_ISO);
	let udf = volumes.take(CAP_UDF);
	let usb = volumes.take(CAP_USB);
	let connection = recv_tagged(bootstrap, &mut buf, b"ADMINREQUEST").unwrap_or(0);
	let mut words = context.arguments.split_whitespace();
	let (Some(target), Some(file), None) = (words.next(), words.next(), words.next()) else {
		fail("usage: dfu TARGET IMAGE - TARGET is dfu:VVVV:PPPP or the binding the target printed");
	};
	if !target.starts_with("dfu:") {
		fail(&format!("{target}: not a DFU target - they are named dfu:..."));
	}
	if connection == 0 {
		fail("this launch holds no administrative request connection, so nothing can be asked");
	}
	let Some(uri) = path::resolve(&context.cwd, file.as_bytes()) else { fail(&format!("{file}: not a path")) };
	let storage = path::volume_client(&context.cwd, file.as_bytes(), system, media, iso, udf, usb, path::NOT_GRANTED, path::NOT_GRANTED);
	let (payload, length) = unsafe { image_of(storage, &uri) };
	let args = AdminRequestArgs { action: AdminAction::FirmwareDownload, target: String::from(target), parameters: alloc::vec::Vec::new(), payload_length: length, label: format!("firmware {file}") };
	say(&format!("{file} ({length} bytes) for {target} - confirm it on the protected screen"));
	match AdminRequestClient::new(connection).request(&args, payload) {
		Some(Ok(AdminAnswer::Granted(grant))) => {
			say("confirmed - writing it");
			let outcome = AdminAuthorityClient::new(grant.grant).execute();
			close(grant.grant);
			match outcome {
				Some(Ok(AdminResult::Completed)) => say("completed - the device took the image and manifested it"),
				Some(Ok(AdminResult::Failed)) => say("failed - the device refused the image; nothing is claimed about what it holds now"),
				Some(Ok(AdminResult::OutcomeUnknown)) => say("the end was not observed - the device went silent or left; it is not retried"),
				Some(Err(error)) => say(&format!("not started: {error:?}")),
				None => say("the attempt went unanswered"),
			}
		}
		Some(Ok(AdminAnswer::Declined)) => say("declined - nothing was written"),
		Some(Err(error)) => say(&format!("refused: {error:?}")),
		None => say("the request went unanswered - nothing was written"),
	}
	exit();
}
