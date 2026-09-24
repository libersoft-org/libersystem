// camread - the camera gate's inventory-only client. DEVELOPMENT-ONLY.
//
// It holds `camera` and nothing else. It proves what that is: cameras and their formats can be listed and
// paged, and nothing sent on that connection starts a camera - a capture `start` sent where an inventory
// client can send it is not an operation there, and the camera stays stopped.

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
use proto::system::{LaunchContext, camera, camera_capture};
use rt::*;
use services::capability_names::*;

const TICKS: u64 = 100;

fn fail(line: &[u8]) -> ! {
	print(b"camread: FAIL ");
	print(line);
	print(b"\n");
	exit();
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	if recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode).is_none() {
		exit();
	}
	let inventory = recv_tagged(bootstrap, &mut buf, CAP_CAMERA).unwrap_or(0);
	if inventory == 0 {
		fail(b"the inventory grant was not delivered");
	}
	let client = || camera::Client::with_deadline(ChannelTransport { chan: inventory }, clock() + 5 * TICKS);
	let cameras = match client().cameras() {
		Some(Ok(cameras)) if !cameras.is_empty() => cameras,
		_ => fail(b"no camera was listed"),
	};
	let first = &cameras[0];
	if first.formats.len() != 2 {
		fail(b"the fixture camera's two formats were not listed");
	}
	match client().sizes(&first.id, &first.generation, &1, &0) {
		Some(Ok(page)) if page.total == 2 && page.sizes.len() == 2 => {}
		_ => fail(b"the YUY2 sizes could not be paged"),
	}
	if !matches!(client().sizes(&first.id, &(first.generation + 1), &1, &0), Some(Err(_))) {
		fail(b"a page under another snapshot generation was answered");
	}
	// A CAPTURE START ON AN INVENTORY CONNECTION starts nothing.
	let _ = camera_capture::Client::with_deadline(ChannelTransport { chan: inventory }, clock() + 2 * TICKS).start();
	sleep_until(clock() + TICKS / 2);
	let fresh = camera::Client::with_deadline(ChannelTransport { chan: inventory }, clock() + 2 * TICKS).cameras();
	if matches!(fresh, Some(Ok(ref listed)) if listed.iter().any(|info| info.streaming)) {
		fail(b"an inventory connection started a camera");
	}
	print(b"camread: PASS inventory listed and paged the formats, and a start sent on it started nothing\n");
	exit();
}
