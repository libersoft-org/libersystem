// adminhostile - an ordinary client on the screen while a protected session is taken. DEVELOPMENT-ONLY.
//
// It holds a display connection and a keyboard connection and nothing else, and it keeps doing what an
// ordinary client can: presenting over the whole screen, making new surfaces, asking for input focus and for
// the keyboard. While the protected screen is up none of it may show or reach anything - its surface is
// hidden and its presents discarded, the keyboard is nobody's on the ordinary path, and the keys a person
// presses to decide never arrive here.
//
//   adminhostile SECONDS

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use proto::system::{AcquiredImage, LaunchContext, SurfaceEvent, input};
use rt::*;

const TICKS: u64 = 100;
// A solid green nobody could mistake for the protected screen's field.
const GREEN: u32 = 0x0000_ff00;
const ENTER: u16 = 0x28;
const KEYPAD_ENTER: u16 = 0x58;
const ESCAPE: u16 = 0x29;

fn say(line: &str) {
	print(b"adminhostile: ");
	print(line.as_bytes());
	print(b"\n");
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 64];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let seconds: u64 = context.arguments.trim().parse().unwrap_or(20);
	let display = recv_tagged(bootstrap, &mut buf, b"DISPLAY").unwrap_or(0);
	let keys = recv_tagged(bootstrap, &mut buf, b"INPUT_KEYS").unwrap_or(0);
	if display == 0 || keys == 0 {
		say("FAIL a grant this client needs was not delivered");
		exit();
	}
	let client = surface::connect(display);
	let Some(Ok(mut screen)) = surface::Surface::create(&client, surface::wire_extent(0, 0), 2) else {
		say("FAIL no surface");
		exit();
	};
	let events = screen.events().unwrap_or(0);
	let mut stream: u64 = screen.input_focus().and_then(Result::ok).and_then(|focus| surface::subscribe_keys(keys, focus)).unwrap_or(0);
	let mut presents: u32 = 0;
	let mut hidden = false;
	let mut hidden_now = false;
	let mut shown_while_hidden: u32 = 0;
	let mut announced = false;
	let mut declined: u32 = 0;
	let mut received: u32 = 0;
	let mut deciding: u32 = 0;
	let mut refused: u32 = 0;
	let mut made: u32 = 0;
	let mut next_try = clock();
	let end = clock() + seconds * TICKS;
	let mut frame = [0u8; 32];
	let mut event_buf = [0u8; 512];
	while clock() < end {
		// DRAW OVER EVERYTHING, every tenth of a second, for as long as it is allowed to.
		match screen.acquire() {
			Some(Ok(AcquiredImage::Image(index))) => {
				if let Some(mapping) = screen.image(index) {
					let span = mapping.layout().backend_access_span(true).unwrap_or(0) as usize;
					let target = unsafe { core::slice::from_raw_parts_mut(mapping.addr() as *mut u8, span) };
					for word in target.chunks_exact_mut(4) {
						word.copy_from_slice(&GREEN.to_le_bytes());
					}
				}
				if matches!(screen.present_whole(index), Some(Ok(_))) {
					presents += 1;
					if !announced {
						announced = true;
						say("on screen");
					}
				}
			}
			Some(Ok(AcquiredImage::NotVisible)) => declined += 1,
			Some(Ok(AcquiredImage::OutOfDate)) => {
				let _ = screen.rebuild();
			}
			_ => {}
		}
		while events != 0 {
			match surface::try_read_event(events, &mut event_buf) {
				Some(SurfaceEvent::VisibilityChanged(visible)) => {
					hidden_now = !visible;
					hidden |= !visible;
				}
				Some(SurfaceEvent::Configure(_)) => {
					let _ = screen.rebuild();
				}
				Some(_) => {}
				None => break,
			}
		}
		// EVERY KEY THAT REACHES IT, and which of them would have decided something.
		while stream != 0 {
			match try_recv_caps(stream, &mut frame) {
				PolledCaps::Message { len, mut handles } => {
					if let Some(event) = input::subscribe_keys_read(&frame[..len], &mut handles) {
						received += 1;
						if matches!(event.code, ENTER | KEYPAD_ENTER | ESCAPE) {
							deciding += 1;
						}
					}
					for &leftover in handles.as_slice() {
						close(leftover);
					}
				}
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					close(stream);
					stream = 0;
				}
			}
		}
		// AND IT KEEPS TRYING: the keyboard back, and another surface to show.
		if clock() >= next_try {
			next_try = clock() + TICKS / 2;
			if stream == 0 {
				match screen.input_focus().and_then(Result::ok).and_then(|focus| surface::subscribe_keys(keys, focus)) {
					Some(again) => stream = again,
					None => refused += 1,
				}
			}
			// A NEW SURFACE WHILE IT IS HIDDEN, which is the one way an ordinary client might hope to come back on
			// top. It must arrive hidden too.
			if hidden_now && let Some(Ok(extra)) = surface::Surface::create(&client, surface::wire_extent(0, 0), 2) {
				made += 1;
				if extra.configuration().visible {
					shown_while_hidden += 1;
				}
				drop(extra);
			}
		}
		sleep_until(clock() + TICKS / 10);
	}
	say(&format!("presented {presents} frames, refused a frame {declined} times, hidden {hidden}, keys {received}, deciding keys {deciding}, keyboard refused {refused}, surfaces made while hidden {made}, of them shown {shown_while_hidden}"));
	if hidden && deciding == 0 && shown_while_hidden == 0 {
		say("PASS it neither covered the protected screen nor received a key that decides");
	} else {
		say("FAIL it covered the protected screen or received a key that decides");
	}
	exit();
}
