// camhold - the camera gate's second capture client. DEVELOPMENT-ONLY.
//
// It holds a capture grant on the fixture camera, like `camcheck`, and exists because busy refusal, an
// inherited endpoint and a buffer an old client keeps each need a client other than the one observing it.
//
//   camhold hold N   stream for N seconds, returning every frame, then stop
//   camhold dup      start a stream, send THIS PROGRAM'S CAPTURE ENDPOINT down stdout, exit without stopping
//   camhold retain   capture one frame, stop, keep the buffer mapped while another grant captures, and prove
//                    nothing more was written into it

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
use proto::system::{CaptureEvent, Frame, Interval, LaunchContext, StreamRequest, camera_capture};
use rt::*;

const TICKS: u64 = 100;
const BYTES: u64 = 160 * 120 * 2;

// ON THE TERMINAL, WHICH IS STDERR: in a pipeline this program's stdout is the pipe to the next stage,
// and a line printed there is read as data and never seen.
fn fail(line: &[u8]) -> ! {
	eprint(b"camhold: FAIL ");
	eprint(line);
	eprint(b"\n");
	exit();
}

fn frame(stream: u64) -> Frame {
	let mut buf = [0u8; 512];
	let until = clock() + 5 * TICKS;
	loop {
		if wait(stream, until) != 0 {
			fail(b"no frame arrived");
		}
		if let PolledCaps::Message { len, .. } = try_recv_caps(stream, &mut buf) {
			let mut handles = wire::Handles::new();
			if let Some(CaptureEvent::Frame(frame)) = camera_capture::events_read(&buf[..len], &mut handles) {
				return frame;
			}
		}
	}
}

fn checksum(bytes: &[u8]) -> u64 {
	bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3))
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let grant = recv_tagged(bootstrap, &mut buf, b"CAMERACAPTURE").unwrap_or(0);
	if grant == 0 {
		fail(b"the capture grant was not delivered");
	}
	let client = || camera_capture::Client::with_deadline(ChannelTransport { chan: grant }, clock() + 10 * TICKS);
	if !matches!(client().negotiate(&StreamRequest { format: 1, size: 1, interval: Interval { numerator: 333_333, denominator: 10_000_000 } }), Some(Ok(_))) {
		fail(b"the negotiation was refused");
	}
	let object = memory_object_create(BYTES);
	if object <= 0 {
		fail(b"a buffer could not be created");
	}
	let object = object as u64;
	// SAFETY: this program's own memory object, mapped for the program's life.
	let Some(base) = (unsafe { map_object(object) }) else { fail(b"the buffer could not be mapped") };
	let lent = duplicate(object, RIGHT_MAP | RIGHT_READ | RIGHT_WRITE | RIGHT_TRANSFER);
	if lent < 0 || !matches!(client().register(&(lent as u64)), Some(Ok(_))) {
		fail(b"the buffer was not registered");
	}
	let Some(stream) = client().events() else { fail(b"the event stream did not open") };
	if !matches!(client().start(), Some(Ok(()))) {
		fail(b"the stream did not start");
	}
	let mut words = context.arguments.split(' ');
	match words.next().unwrap_or("") {
		"hold" => {
			let seconds: u64 = words.next().and_then(|n| n.parse().ok()).unwrap_or(3);
			let until = clock() + seconds * TICKS;
			while clock() < until {
				let got = frame(stream);
				let _ = client().release(&got.buffer, &got.lease);
			}
			let _ = client().stop();
			eprint(b"camhold: streamed and stopped\n");
		}
		"dup" => {
			frame(stream);
			// THE ENDPOINT ITSELF GOES: the next stage of the pipeline holds it, and this program ends.
			if !send_caps_blocking(stdout(), b"CAPTURE", &[grant]) {
				fail(b"the endpoint could not be sent");
			}
			eprint(b"camhold: sent its endpoint while streaming, and exits\n");
		}
		"retain" => {
			let got = frame(stream);
			if !matches!(client().stop(), Some(Ok(()))) {
				fail(b"the stream did not stop");
			}
			// SAFETY: the mapping covers the buffer, which this program created.
			let bytes = unsafe { core::slice::from_raw_parts(base as *const u8, got.valid_bytes as usize) };
			let before = checksum(bytes);
			// ANOTHER GRANT CAPTURES NOW; this buffer stays mapped here the whole time.
			sleep_until(clock() + 3 * TICKS);
			if checksum(bytes) != before {
				fail(b"a new grant's frames reached an old client's buffer");
			}
			eprint(b"camhold: PASS retain - an old client's buffer, kept mapped, received nothing after its stream stopped\n");
		}
		_ => fail(b"usage: camhold hold N | dup | retain"),
	}
	exit();
}
