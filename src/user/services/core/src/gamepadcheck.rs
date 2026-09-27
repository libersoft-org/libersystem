// gamepadcheck - the in-guest scenario driver for the gamepad tool's gate. DEVELOPMENT-ONLY.
//
// It is the second half of ONE typed line, `gamepad --lines | gamepadcheck`: it reads the tool's own lines on
// its standard input and drives the gamepad fixture through its control endpoint. THE PIPE IS THE
// SYNCHRONISATION: it waits for `gamepad: watching` before it touches the fixture, and after each step waits,
// bounded, for the line that step must produce before taking the next - so nothing is scripted ahead of a tool
// that is not listening, and a line for the wrong gamepad during a step fails it.
//
// The steps: two gamepads attach and arrive under two ids, each at rest; button 3 pressed on the SECOND shows on
// the second and not the first; releasing it clears it; X on the first moved to 0 and then to 255 shows the
// range's ends; the second's hat moved east and back shows `E` and `centred`; the first detaches, its departure
// shows, and a button on the second still shows; the first attaches again and arrives under a NEW id. Then
// `gamepadcheck: PASS which gamepad pressed what`, its input closed, and what is left detached - whose departure
// lines the tool cannot write to a closed pipe, which ends it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::gamepad_fixture;
use rt::*;

const TICKS: u64 = 100;
// How long one step may take to show: generous, because the tool's lines cross InputService and the pipe.
const STEP_TICKS: u64 = 10 * TICKS;
// The harness gamepad at rest: every axis at the midpoint of 0..255.
const MID: [i32; 4] = [127; 4];

fn fail(why: &[u8]) -> ! {
	print(b"gamepadcheck: FAIL ");
	print(why);
	print(b"\n");
	exit_with(1);
}

// The tool's output, as lines.
struct Lines {
	input: u64,
	held: Vec<u8>,
}

impl Lines {
	// The next whole line, waiting until `deadline`; `None` when it passed.
	fn next(&mut self, deadline: u64) -> Option<Vec<u8>> {
		loop {
			if let Some(end) = self.held.iter().position(|&byte| byte == b'\n') {
				let line: Vec<u8> = self.held.drain(..=end).take(end).collect();
				return Some(line);
			}
			let mut buf = [0u8; 512];
			match try_recv_caps(self.input, &mut buf) {
				PolledCaps::Message { len, handles } => {
					for &handle in handles.as_slice() {
						close(handle);
					}
					self.held.extend_from_slice(&buf[..len]);
				}
				PolledCaps::Empty => {
					if clock() >= deadline {
						return None;
					}
					// Woken by a line or by the deadline; the loop above tells which.
					let _ = wait_any(&[self.input], deadline);
				}
				PolledCaps::Closed => fail(b"the tool's output ended"),
			}
		}
	}
}

// One `gamepad: <word> <id> ...` line: its word, its id and the rest.
fn parse(line: &[u8]) -> Option<(&[u8], u32, &[u8])> {
	let rest = line.strip_prefix(b"gamepad: ")?;
	let mut words = rest.splitn(3, |&byte| byte == b' ');
	let word = words.next()?;
	let id = core::str::from_utf8(words.next()?).ok()?.parse().ok()?;
	Some((word, id, words.next().unwrap_or(&[])))
}

// A `key=value` field of a line's rest.
fn field<'a>(rest: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
	rest.split(|&byte| byte == b' ').find_map(|part| part.strip_prefix(key).and_then(|value| value.strip_prefix(b"=")))
}

struct Probe {
	fixture: u64,
	lines: Lines,
}

impl Probe {
	fn attach(&self, label: &str) -> u32 {
		match gamepad_fixture::Client::with_deadline(ChannelTransport { chan: self.fixture }, clock() + 2 * TICKS).attach(label) {
			Some(Ok(handle)) => handle,
			_ => fail(b"the fixture would not attach a gamepad"),
		}
	}

	fn report(&self, handle: u32, buttons: u32, hat: u8, axes: [i32; 4]) {
		if !matches!(gamepad_fixture::Client::with_deadline(ChannelTransport { chan: self.fixture }, clock() + 2 * TICKS).report(&handle, &buttons, &hat, &axes), Some(Ok(()))) {
			fail(b"the fixture would not report");
		}
	}

	fn detach(&self, handle: u32) {
		if !matches!(gamepad_fixture::Client::with_deadline(ChannelTransport { chan: self.fixture }, clock() + 2 * TICKS).detach(&handle), Some(Ok(()))) {
			fail(b"the fixture would not detach a gamepad");
		}
	}

	// Wait for the line a step must produce: `word` for gamepad `id` whose rest satisfies `wanted`. A line about
	// another gamepad than `id` - other than the ones the step allows - fails the step.
	fn expect(&mut self, step: &[u8], word: &[u8], id: u32, wanted: impl Fn(&[u8]) -> bool) {
		let deadline = clock() + STEP_TICKS;
		loop {
			let Some(line) = self.lines.next(deadline) else { fail(step) };
			let Some((seen, seen_id, rest)) = parse(&line) else { continue };
			if seen_id != id {
				print(b"gamepadcheck: FAIL ");
				print(step);
				print(b": a line for the wrong gamepad: ");
				print(&line);
				print(b"\n");
				exit_with(1);
			}
			if seen == word && wanted(rest) {
				return;
			}
		}
	}

	// Wait for an `arrived` line and the state that follows it; the id it arrived under, and its label.
	fn arrival(&mut self, step: &[u8]) -> (u32, Vec<u8>) {
		let deadline = clock() + STEP_TICKS;
		loop {
			let Some(line) = self.lines.next(deadline) else { fail(step) };
			let Some((seen, id, rest)) = parse(&line) else { continue };
			if seen != b"arrived" {
				continue;
			}
			let label = rest.strip_prefix(b"label=").and_then(|value| value.split(|&byte| byte == b' ').next()).unwrap_or(&[]).to_vec();
			// AT REST UNTIL ITS FIRST REPORT: no buttons, each axis at its midpoint, the hat centred - never a
			// zeroed hat, which would read as north.
			self.expect(step, b"state", id, |state| field(state, b"buttons") == Some(b"none") && field(state, b"axes") == Some(b"127,127,127,127") && field(state, b"hats") == Some(b"centred"));
			return (id, label);
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let _ = recv_launch_bytes(bootstrap);
	let fixture = recv_tagged(bootstrap, &mut buf, b"FIXTURE").unwrap_or(0);
	if fixture == 0 {
		fail(b"a grant this probe needs was not delivered");
	}
	if stdin() == 0 {
		fail(b"there is no input - run it as `gamepad --lines | gamepadcheck`");
	}
	let mut probe = Probe { fixture, lines: Lines { input: stdin(), held: Vec::new() } };

	// THE TOOL IS LISTENING before anything is attached.
	let deadline = clock() + STEP_TICKS;
	loop {
		match probe.lines.next(deadline) {
			Some(line) if line == b"gamepad: watching" => break,
			Some(_) => {}
			None => fail(b"the tool never said it was watching"),
		}
	}

	// TWO GAMEPADS, TWO IDS.
	let first = probe.attach("pad-one");
	let (first_id, first_label) = probe.arrival(b"the first gamepad did not arrive at rest");
	let second = probe.attach("pad-two");
	let (second_id, second_label) = probe.arrival(b"the second gamepad did not arrive at rest");
	if first_id == second_id || first_label != b"pad-one" || second_label != b"pad-two" {
		fail(b"two gamepads did not arrive under two ids with their own labels");
	}

	// BUTTON 3 ON THE SECOND shows on the second, and nothing shows for the first.
	probe.report(second, 1 << 2, 8, MID);
	probe.expect(b"button 3 on the second gamepad did not show on it", b"state", second_id, |state| field(state, b"buttons") == Some(b"3"));
	probe.report(second, 0, 8, MID);
	probe.expect(b"releasing button 3 did not clear it", b"state", second_id, |state| field(state, b"buttons") == Some(b"none"));

	// X ON THE FIRST to the range's ends.
	probe.report(first, 0, 8, [0, 127, 127, 127]);
	probe.expect(b"X at 0 on the first gamepad did not show", b"state", first_id, |state| field(state, b"axes") == Some(b"0,127,127,127"));
	probe.report(first, 0, 8, [255, 127, 127, 127]);
	probe.expect(b"X at 255 on the first gamepad did not show", b"state", first_id, |state| field(state, b"axes") == Some(b"255,127,127,127"));

	// THE SECOND'S HAT east and back.
	probe.report(second, 0, 2, MID);
	probe.expect(b"the second gamepad's hat east did not show as E", b"state", second_id, |state| field(state, b"hats") == Some(b"E"));
	probe.report(second, 0, 8, MID);
	probe.expect(b"the second gamepad's hat back did not show as centred", b"state", second_id, |state| field(state, b"hats") == Some(b"centred"));

	// THE FIRST LEAVES, and the second still works.
	probe.detach(first);
	probe.expect(b"the first gamepad's departure did not show", b"departed", first_id, |_| true);
	probe.report(second, 1, 8, MID);
	probe.expect(b"a button on the second gamepad did not show after the first left", b"state", second_id, |state| field(state, b"buttons") == Some(b"1"));

	// THE FIRST COMES BACK AS A NEW GAMEPAD.
	let again = probe.attach("pad-one");
	let (again_id, _) = probe.arrival(b"the first gamepad did not arrive again");
	if again_id == first_id || again_id == second_id {
		fail(b"a gamepad plugged back arrived under an id it had before");
	}

	print(b"gamepadcheck: PASS which gamepad pressed what\n");
	// ITS INPUT CLOSED FIRST, then what is left detached: the tool's departure lines meet a closed pipe, which
	// ends it.
	close(stdin());
	probe.detach(second);
	probe.detach(again);
	exit();
}
