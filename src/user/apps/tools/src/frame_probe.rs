// frame_probe - the integration gate for the frame loop an application has.
//
// WHAT IT PROVES IS THE SCHEDULING AND NOT THE PICTURE. It draws nothing worth looking at: a solid
// colour that changes per frame, so a harness can tell one frame from the next. What it exercises is
// everything the renderer is NOT allowed to know about - acquire, present, completion, pacing,
// resize, `out-of-date`, visibility - through the shared helper, against a real DisplayService.
//
// IT REPORTS WHAT IT DID, one line per decision, on stdout. A gate that only watched the device wire
// could see the frames and not the reasoning; what makes a busy spin or a frame past the negotiated
// limit VISIBLE is the loop saying which step it took.

#![no_std]
#![no_main]

extern crate alloc;

use graphics_app::{FrameLoop, Step};
use rt::*;

// The fewest frames worth reporting a frame rate over. Small, because what is under test is the
// ORDERING of the decisions rather than throughput.
const FRAMES: u32 = 3;
// A hard ceiling on loop iterations, so a probe that stops making progress ENDS rather than hanging
// the harness that is watching it: a gate whose failure mode is a timeout tells you nothing about
// which step went wrong.
const ITERATIONS: u32 = 20_000;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0; 64];
	inherit_stdout(bootstrap);
	let _ = recv_launch_bytes(bootstrap);
	let display: u64 = recv_tagged(bootstrap, &mut buf, b"DISPLAY").unwrap_or_else(|| exit());
	if display == 0 {
		print(b"frame-probe: no display\n");
		exit();
	}
	let client = surface::connect(display);
	// TWO IMAGES, which is the fewest a present queue will negotiate and the smallest number that
	// can show a limit being reached at all.
	let Some(Ok(mut frames)) = FrameLoop::open(&client, 0, 0, 2) else {
		print(b"frame-probe: no surface\n");
		exit();
	};
	print(b"frame-probe: open\n");

	let mut presented: u32 = 0;
	let mut awaited: u32 = 0;
	let mut rebuilt: u32 = 0;
	let mut idled: u32 = 0;
	let mut background: u32 = 0;
	let mut colour: u32 = 0x0000_1100;
	let mut floor_reported: bool = false;
	for _ in 0..ITERATIONS {
		// IT STOPS WHEN IT HAS SEEN WHAT IT IS HERE TO SHOW, and not after a fixed number of frames.
		// A probe that exited on a frame count would leave the harness driving a resize and a loss
		// of the screen at a program that had already gone - which is a gate that passes by never
		// reaching the thing it is gating.
		if presented >= FRAMES && !floor_reported {
			floor_reported = true;
			print(b"frame-probe: frames\n");
		}
		if presented >= FRAMES && rebuilt > 0 && background > 0 {
			break;
		}
		match frames.step() {
			Step::Draw => {
				let Some(frame) = frames.acquire() else { continue };
				// THE RENDERER'S WHOLE SHARE OF THIS: bytes in, bytes out, and nothing about a
				// queue. A solid fill stands in for one, and the loop around it is the point.
				let span = frame.layout.backend_access_span(true).unwrap_or(0) as usize;
				// SAFETY: the mapping is live for as long as the frame is held, and the span is the
				// layout's own answer for what a backend may touch.
				let target = unsafe { core::slice::from_raw_parts_mut(frame.addr as *mut u8, span) };
				for word in target.chunks_exact_mut(4) {
					word.copy_from_slice(&colour.to_le_bytes());
				}
				colour = colour.wrapping_add(0x0000_1100);
				// THE FIRST PRESENT OF A GENERATION IS THE WHOLE SURFACE, spelled as the variant
				// rather than as a rectangle covering the extent.
				if frames.present_whole(frame) {
					presented += 1;
				}
			}
			// EVERY IMAGE THIS LOOP MAY HOLD IS WITH THE SERVICE, so it waits for one to come back
			// rather than asking again - which against a deliberately non-blocking acquire would be
			// a spin that costs a core and never ends on its own.
			Step::AwaitCompletion => {
				awaited += 1;
				frames.park(None);
			}
			Step::Rebuild => {
				rebuilt += 1;
				if frames.rebuild().is_none_or(|outcome| outcome.is_err()) {
					print(b"frame-probe: rebuild failed\n");
					break;
				}
			}
			Step::Idle { until } => {
				// A HIDDEN LOOP AND A PACED ONE TAKE THE SAME STEP AND MEAN DIFFERENT THINGS, and
				// the difference is worth reporting: one is this loop waiting its turn and the other
				// is it declining to draw frames nobody will see.
				if frames.pacing().visible() {
					idled += 1;
				} else {
					background += 1;
				}
				frames.park(until);
			}
		}
	}

	let mut line: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	line.extend_from_slice(b"frame-probe:");
	for (name, value) in [
		(&b" presented="[..], presented),
		(b" awaited=", awaited),
		(b" rebuilt=", rebuilt),
		(b" idled=", idled),
		(b" background=", background),
		(b" inflight=", frames.pacing().in_flight() as u32),
		(b" limit=", frames.pacing().limit() as u32),
		(b" events=", frames.events_seen()),
		(b" configures=", frames.configures_seen()),
		(b" visibility=", frames.visibility_seen()),
		(b" visible=", frames.pacing().visible() as u32),
	] {
		line.extend_from_slice(name);
		push_number(&mut line, value);
	}
	line.push(b'\n');
	print(&line);
	exit();
}

fn push_number(out: &mut alloc::vec::Vec<u8>, value: u32) {
	if value == 0 {
		out.push(b'0');
		return;
	}
	let mut digits: [u8; 10] = [0; 10];
	let mut count = 0usize;
	let mut left = value;
	while left > 0 {
		digits[count] = b'0' + (left % 10) as u8;
		left /= 10;
		count += 1;
	}
	for index in (0..count).rev() {
		out.push(digits[index]);
	}
}
