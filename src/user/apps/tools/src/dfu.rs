// dfu - ask for a firmware image to be written to a USB DFU target, or for a target's firmware to be read out of it,
// through the trusted administrative path.
//
//   dfu TARGET IMAGE
//   dfu backup TARGET FILE [MAX]
//
// TARGET names the device the way the DFU executor answers to: `dfu:VVVV:PPPP` by vendor and product, or the
// live binding it prints (`dfu:port3.0/if0/1d6b:0104`). IMAGE is a file, relative to the working directory or an
// absolute URI - a DFU 1.1 image, whose suffix, where it has one, the executor holds against the target.
//
// NOTHING IS WRITTEN - OR READ - BECAUSE THIS PROGRAM ASKED. It puts one request to AdminService on the request
// connection PermissionManager minted for this launch - the `dfu` scope: firmware download and upload, on `dfu:`
// targets, and nothing else - and AdminService shows the operation, frozen, on the protected screen. Only a person's
// confirmation there turns the request into a grant, and the grant into one attempt; a decline, or no answer, does
// nothing. What the device then reports is printed as it came: completed, refused, or an end nobody observed - which
// is never retried and never described as rolled back, because DFU gives a host no rollback to promise.
//
// A BACKUP reads the target's firmware OUT - at most MAX bytes, a mebibyte unless named - into FILE: an explicit
// request, confirmed like a download, since a firmware image can hold the device's own secrets and licensed code. A
// TRANSACTIONAL WRITER over FILE is opened before anything is asked - a destination this program cannot write is
// refused before the screen is shown - and the image is committed through it only when it is whole: the volume
// publishes a writer's bytes at its commit, as the file's whole contents, and an aborted one publishes nothing, so a
// failed backup never leaves a file that looks like one. A target that does not declare upload is refused before the
// screen too, by its executor.

#![no_std]
#![no_main]

extern crate alloc;

use admin_client::{AdminAuthorityClient, AdminRequestClient};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use proto::system::{AdminAction, AdminAnswer, AdminRead, AdminRequestArgs, AdminResult, LaunchContext, OpenOpts, WriterMode};
use rt::*;
use storage_proto::path;
use volume_client::{VolumeClient, WRITER_CHUNK, WriterClient};

// The largest image a backup reads unless the command names less - the executor's own bound.
const MAX_UPLOAD: u32 = 1 << 20;

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
		let (shared, _) = shared_copy(core::slice::from_raw_parts(from as *const u8, size));
		unmap_object(opened.file);
		close(opened.file);
		(shared, size as u32)
	}
}

// Bytes in an object of this program's own, handed over read-only.
fn shared_copy(bytes: &[u8]) -> (u64, u32) {
	let copy = memory_object_create(bytes.len() as u64);
	if copy < 0 {
		fail("no memory for the request");
	}
	let copy = copy as u64;
	let Some(to) = (unsafe { map_object(copy) }) else { fail("the request's copy cannot be mapped") };
	unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), to as *mut u8, bytes.len()) };
	unmap_object(copy);
	let shared = duplicate(copy, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
	close(copy);
	if shared < 0 {
		fail("the request cannot be handed over");
	}
	(shared as u64, bytes.len() as u32)
}

fn hex(bytes: &[u8]) -> String {
	let mut out = String::new();
	for byte in bytes {
		out.push_str(&format!("{byte:02x}"));
	}
	out
}

fn download(connection: u64, target: &str, file: &str, storage: u64, uri: &str) {
	let (payload, length) = unsafe { image_of(storage, uri) };
	let args = AdminRequestArgs { action: AdminAction::FirmwareDownload, target: String::from(target), parameters: Vec::new(), payload_length: length, label: format!("firmware {file}") };
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
}

// THE IMAGE, COMMITTED WHOLE through the writer opened for it: true once FILE holds all of it and nothing else.
fn keep(mut writer: WriterClient, image: u64, length: u32) -> bool {
	let Some(addr) = (unsafe { map_object(image) }) else {
		let _ = writer.abort();
		close(writer.handle());
		return false;
	};
	let bytes = unsafe { core::slice::from_raw_parts(addr as *const u8, length as usize) };
	let mut written = true;
	for chunk in bytes.chunks(WRITER_CHUNK) {
		if !matches!(writer.write(chunk), Some(Ok(_))) {
			written = false;
			break;
		}
	}
	unmap_object(image);
	if !written {
		let _ = writer.abort();
		close(writer.handle());
		return false;
	}
	let published = writer.commit();
	close(writer.handle());
	matches!(published, Some(Ok(published)) if published == u64::from(length))
}

// NOTHING IS PUBLISHED: the writer aborted, and FILE is as it was.
fn forget(mut writer: WriterClient) {
	let _ = writer.abort();
	close(writer.handle());
}

fn backup(connection: u64, target: &str, file: &str, bound: u32, storage: u64, uri: &str) {
	// THE DESTINATION IS OPENED FOR WRITING BEFORE ANYTHING IS ASKED: one this program cannot write is refused before
	// the screen.
	let writer = match VolumeClient::new(storage).open_writer(uri, WriterMode::Replace) {
		Some(Ok(writer)) => writer,
		_ => fail(&format!("{file}: cannot be written here - nothing was asked")),
	};
	// THE PAYLOAD IS THE TARGET'S NAME, and the parameters the bound and the file - what the screen shows.
	let (payload, length) = shared_copy(target.as_bytes());
	let mut parameters = Vec::from(bound.to_le_bytes());
	parameters.extend_from_slice(uri.as_bytes());
	let args = AdminRequestArgs { action: AdminAction::FirmwareUpload, target: String::from(target), parameters, payload_length: length, label: format!("backup to {file}") };
	say(&format!("the firmware of {target}, at most {bound} bytes, into {file} - confirm it on the protected screen"));
	match AdminRequestClient::new(connection).request(&args, payload) {
		Some(Ok(AdminAnswer::Granted(grant))) => {
			say("confirmed - reading it");
			let outcome = AdminAuthorityClient::new(grant.grant).execute_read();
			close(grant.grant);
			match outcome {
				Some(Ok(AdminRead::Completed(image))) => {
					let kept = keep(writer, image.image, image.length);
					close(image.image);
					if kept {
						say(&format!("completed - {} bytes, SHA-256 {}, written to {file}", image.length, hex(&image.digest)));
					} else {
						say(&format!("the image was read and could not be written to {file} - nothing was published there"));
					}
				}
				Some(Ok(AdminRead::Failed)) => {
					forget(writer);
					say("failed - nothing was read out; no file was written");
				}
				Some(Ok(AdminRead::OutcomeUnknown)) => {
					forget(writer);
					say("the end was not observed - nothing was handed over; it is not retried");
				}
				Some(Err(error)) => {
					forget(writer);
					say(&format!("not started: {error:?}"));
				}
				None => {
					forget(writer);
					say("the attempt went unanswered - no file was written");
				}
			}
		}
		Some(Ok(AdminAnswer::Declined)) => {
			forget(writer);
			say("declined - nothing was read");
		}
		Some(Err(error)) => {
			forget(writer);
			say(&format!("refused: {error:?}"));
		}
		None => {
			forget(writer);
			say("the request went unanswered - nothing was read");
		}
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
	let words: Vec<&str> = context.arguments.split_whitespace().collect();
	let (reading, target, file, bound) = match words.as_slice() {
		["backup", target, file] => (true, *target, *file, MAX_UPLOAD),
		["backup", target, file, most] => match most.parse::<u32>() {
			Ok(most) if most > 0 && most <= MAX_UPLOAD => (true, *target, *file, most),
			_ => fail(&format!("{most}: MAX is a number of bytes from 1 to {MAX_UPLOAD}")),
		},
		[target, file] if *target != "backup" => (false, *target, *file, 0),
		_ => fail("usage: dfu TARGET IMAGE | dfu backup TARGET FILE [MAX] - TARGET is dfu:VVVV:PPPP or the binding the target printed"),
	};
	if !target.starts_with("dfu:") {
		fail(&format!("{target}: not a DFU target - they are named dfu:..."));
	}
	if connection == 0 {
		fail("this launch holds no administrative request connection, so nothing can be asked");
	}
	let Some(uri) = path::resolve(&context.cwd, file.as_bytes()) else { fail(&format!("{file}: not a path")) };
	let storage = path::volume_client(&context.cwd, file.as_bytes(), system, media, iso, udf, usb, path::NOT_GRANTED, path::NOT_GRANTED);
	if reading {
		backup(connection, target, file, bound, storage, &uri);
	} else {
		download(connection, target, file, storage, &uri);
	}
	exit();
}
