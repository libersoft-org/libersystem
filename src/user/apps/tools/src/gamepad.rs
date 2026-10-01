// gamepad - which button is pressed on which gamepad, in the text interface.
//
// IT WATCHES EVERY GAMEPAD FROM THE CONSOLE, through the one authority it holds: `input-gamepad`, a
// connection of InputService's gamepad scope on which only `observe-gamepads` works. InputService delivers
// that stream only while no graphical surface holds display focus, to one console program at a time, and
// never while a protected session is up - so this tool cannot watch a game's input, and it says so when it
// is refused rather than waiting on a stream that will not come.
//
// FULL-SCREEN BY DEFAULT, like `watch` and `less`: one line per connected gamepad - its number, its label,
// its pressed buttons by number, each axis by name with its value and range, each hat as a compass point or
// centred - redrawn in place as states arrive, every event already queued read before each redraw, with
// arrivals and departures in a status line. `q` or Ctrl+C leaves, and every exit path restores the terminal.
//
// `gamepad --lines` IS THE SAME STREAM AS LINES, for a log, a pipe and a gate: `gamepad: watching` once the
// stream is open, then one line per event, and `gamepad: closed` when it ends. It exits when the stream
// closes or when a line cannot be written because its reader has gone.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use input_client::InputClient;
use lico::{InputDecoder, InputEvent, Key, MouseTracking, TerminalGuard, TerminalOptions, TerminalWriter};
use proto::system::{Gamepad, GamepadAxis, GamepadEvent, GamepadState, LaunchContext, input};
use rt::*;
use tools::{ConsoleWriter, split_args};

const USAGE: &[u8] = b"Usage: gamepad [--lines]\nShows which button is pressed on which gamepad, full-screen; q or Ctrl+C leaves.\n--lines prints the same stream as lines, for a log or a pipe.\n";

// WHY A STREAM IS REFUSED OR ENDS: InputService answers either with nothing more than that, so the tool names
// every cause there is.
const REFUSED: &[u8] = b"gamepad: the gamepad stream was refused - a graphical application holds the display, a protected session is up, or another program already watches the gamepads\n";
const CLOSED: &[u8] = b"gamepad: the gamepad stream closed - a graphical application took the display, a protected session began, or this tool fell so far behind that an arrival or departure could not reach it\n";

// The largest frame of the stream: a `present` with every count at its most, and room over.
const FRAME: usize = 512;

// One gamepad as the stream has described it, and its latest state.
struct Pad {
	record: Gamepad,
	state: Option<GamepadState>,
}

// A Generic Desktop or Simulation axis by name, or none for a usage without one - which is shown in hex.
fn axis_name(usage: u32) -> Option<&'static str> {
	let id = usage & 0xffff;
	match usage >> 16 {
		0x01 => match id {
			0x30 => Some("X"),
			0x31 => Some("Y"),
			0x32 => Some("Z"),
			0x33 => Some("Rx"),
			0x34 => Some("Ry"),
			0x35 => Some("Rz"),
			0x36 => Some("Slider"),
			0x37 => Some("Dial"),
			0x38 => Some("Wheel"),
			_ => None,
		},
		0x02 => match id {
			0xb0 => Some("Aileron"),
			0xb1 => Some("AileronTrim"),
			0xb2 => Some("AntiTorque"),
			0xb5 => Some("Collective"),
			0xb6 => Some("DiveBrake"),
			0xb8 => Some("Elevator"),
			0xb9 => Some("ElevatorTrim"),
			0xba => Some("Rudder"),
			0xbb => Some("Throttle"),
			0xbf => Some("ToeBrake"),
			0xc3 => Some("WingFlaps"),
			0xc4 => Some("Accelerator"),
			0xc5 => Some("Brake"),
			0xc6 => Some("Clutch"),
			0xc7 => Some("Shifter"),
			0xc8 => Some("Steering"),
			0xc9 => Some("Turret"),
			0xca => Some("Barrel"),
			0xcb => Some("DivePlane"),
			0xcc => Some("Ballast"),
			0xcd => Some("Crank"),
			0xce => Some("HandleBars"),
			0xcf => Some("FrontBrake"),
			0xd0 => Some("RearBrake"),
			_ => None,
		},
		_ => None,
	}
}

fn push_axis_name(out: &mut Vec<u8>, axis: &GamepadAxis) {
	match axis_name(axis.usage) {
		Some(name) => out.extend_from_slice(name.as_bytes()),
		None => {
			out.extend_from_slice(b"0x");
			for shift in (0..8).rev() {
				out.push(b"0123456789abcdef"[(axis.usage >> (shift * 4) & 0xf) as usize]);
			}
		}
	}
}

// A hat as a compass point: north and then clockwise in eighths, or centred.
fn hat_name(hat: u8) -> &'static [u8] {
	match hat {
		0 => b"N",
		1 => b"NE",
		2 => b"E",
		3 => b"SE",
		4 => b"S",
		5 => b"SW",
		6 => b"W",
		7 => b"NW",
		_ => b"centred",
	}
}

fn push_signed(out: &mut Vec<u8>, value: i64) {
	if value < 0 {
		out.push(b'-');
	}
	let mut digits = [0u8; 20];
	let mut count = 0;
	let mut rest = value.unsigned_abs();
	loop {
		digits[count] = b'0' + (rest % 10) as u8;
		count += 1;
		rest /= 10;
		if rest == 0 {
			break;
		}
	}
	out.extend(digits[..count].iter().rev());
}

// `label=... axes=X:0..255,... buttons=16 hats=1`, what a `present` and an `arrived` line say of a gamepad.
fn push_description(out: &mut Vec<u8>, pad: &Gamepad) {
	out.extend_from_slice(b"label=");
	out.extend_from_slice(pad.label.as_bytes());
	out.extend_from_slice(b" axes=");
	if pad.axes.is_empty() {
		out.extend_from_slice(b"none");
	}
	for (index, axis) in pad.axes.iter().enumerate() {
		if index > 0 {
			out.push(b',');
		}
		push_axis_name(out, axis);
		out.push(b':');
		push_signed(out, axis.minimum as i64);
		out.extend_from_slice(b"..");
		push_signed(out, axis.maximum as i64);
	}
	out.extend_from_slice(b" buttons=");
	push_signed(out, pad.buttons as i64);
	out.extend_from_slice(b" hats=");
	push_signed(out, pad.hats as i64);
}

// The pressed buttons by number, or `none`.
fn push_buttons(out: &mut Vec<u8>, buttons: u32, separator: u8) {
	if buttons == 0 {
		out.extend_from_slice(b"none");
		return;
	}
	let mut first = true;
	for bit in 0..32 {
		if buttons & 1 << bit != 0 {
			if !first {
				out.push(separator);
			}
			first = false;
			push_signed(out, bit as i64 + 1);
		}
	}
}

// `buttons=1,16 axes=0,128,128,255 hats=E`, what a `state` line says.
fn push_state(out: &mut Vec<u8>, state: &GamepadState) {
	out.extend_from_slice(b"buttons=");
	push_buttons(out, state.buttons, b',');
	out.extend_from_slice(b" axes=");
	if state.axes.is_empty() {
		out.extend_from_slice(b"none");
	}
	for (index, value) in state.axes.iter().enumerate() {
		if index > 0 {
			out.push(b',');
		}
		push_signed(out, *value as i64);
	}
	out.extend_from_slice(b" hats=");
	if state.hats.is_empty() {
		out.extend_from_slice(b"none");
	}
	for (index, hat) in state.hats.iter().enumerate() {
		if index > 0 {
			out.push(b',');
		}
		out.extend_from_slice(hat_name(*hat));
	}
}

// One event as the line `--lines` prints for it.
fn event_line(event: &GamepadEvent) -> Vec<u8> {
	let mut line: Vec<u8> = Vec::with_capacity(160);
	line.extend_from_slice(b"gamepad: ");
	match event {
		GamepadEvent::Present(pad) | GamepadEvent::Arrived(pad) => {
			line.extend_from_slice(if matches!(event, GamepadEvent::Present(_)) { b"present " } else { b"arrived " });
			push_signed(&mut line, pad.id as i64);
			line.push(b' ');
			push_description(&mut line, pad);
		}
		GamepadEvent::State(state) => {
			line.extend_from_slice(b"state ");
			push_signed(&mut line, state.id as i64);
			line.push(b' ');
			push_state(&mut line, state);
		}
		GamepadEvent::Departed(id) => {
			line.extend_from_slice(b"departed ");
			push_signed(&mut line, *id as i64);
		}
	}
	line.push(b'\n');
	line
}

// Open the stream on the gamepad-scope connection; `None` when InputService refused it.
fn open(connection: u64) -> Option<u64> {
	InputClient::new(connection).observe_gamepads()
}

// What the next frame on the stream is.
enum Next {
	Event(GamepadEvent),
	Empty,
	Closed,
}

fn next(stream: u64, buf: &mut [u8]) -> Next {
	match try_recv_caps(stream, buf) {
		PolledCaps::Message { len, handles } => {
			let mut handles = handles;
			let event = input::observe_gamepads_read(&buf[..len], &mut handles);
			for &leftover in handles.as_slice() {
				close(leftover);
			}
			// A FRAME THAT DOES NOT DECODE is not an event, and not a reason to stop watching.
			match event {
				Some(event) => Next::Event(event),
				None => Next::Empty,
			}
		}
		PolledCaps::Empty => Next::Empty,
		PolledCaps::Closed => Next::Closed,
	}
}

// Write a line to whoever reads this tool's output; false when they have gone.
fn write_line(line: &[u8]) -> bool {
	let out = stdout();
	out != 0 && send_blocking(out, line, 0)
}

// `--lines`: the stream as lines until it closes or the reader goes.
fn lines(stream: u64) -> ! {
	if !write_line(b"gamepad: watching\n") {
		exit();
	}
	let mut buf = [0u8; FRAME];
	loop {
		if wait_any(&[stream], 0) < 0 {
			exit_with(1);
		}
		loop {
			match next(stream, &mut buf) {
				Next::Event(event) => {
					if !write_line(&event_line(&event)) {
						exit();
					}
				}
				Next::Empty => break,
				Next::Closed => {
					let _ = write_line(b"gamepad: closed\n");
					eprint(CLOSED);
					exit_with(1);
				}
			}
		}
	}
}

// Apply one event to the table, saying in the status line what arrived and what left.
fn apply(pads: &mut Vec<Pad>, event: GamepadEvent, status: &mut Vec<u8>) {
	match event {
		GamepadEvent::Present(record) => pads.push(Pad { record, state: None }),
		GamepadEvent::Arrived(record) => {
			status.clear();
			status.extend_from_slice(b"gamepad ");
			push_signed(status, record.id as i64);
			status.extend_from_slice(b" arrived: ");
			status.extend_from_slice(record.label.as_bytes());
			pads.push(Pad { record, state: None });
		}
		GamepadEvent::State(state) => {
			if let Some(pad) = pads.iter_mut().find(|pad| pad.record.id == state.id) {
				pad.state = Some(state);
			}
		}
		GamepadEvent::Departed(id) => {
			pads.retain(|pad| pad.record.id != id);
			status.clear();
			status.extend_from_slice(b"gamepad ");
			push_signed(status, id as i64);
			status.extend_from_slice(b" departed");
		}
	}
}

// One screen: a heading, a line per gamepad, and the status line.
fn render(output: &mut impl TerminalWriter, pads: &[Pad], status: &[u8]) -> bool {
	let mut screen: Vec<u8> = Vec::with_capacity(1024);
	screen.extend_from_slice(b"\x1b[H\x1b[2Jgamepad - q or Ctrl+C leaves\r\n\r\n");
	if pads.is_empty() {
		screen.extend_from_slice(b"No gamepad is connected.\r\n");
	}
	for pad in pads {
		push_signed(&mut screen, pad.record.id as i64);
		screen.extend_from_slice(b"  ");
		screen.extend_from_slice(pad.record.label.as_bytes());
		screen.extend_from_slice(b"\r\n    buttons: ");
		match &pad.state {
			Some(state) => push_buttons(&mut screen, state.buttons, b' '),
			None => screen.extend_from_slice(b"none"),
		}
		screen.extend_from_slice(b"\r\n    ");
		for (index, axis) in pad.record.axes.iter().enumerate() {
			if index > 0 {
				screen.extend_from_slice(b"  ");
			}
			push_axis_name(&mut screen, axis);
			screen.push(b' ');
			match pad.state.as_ref().and_then(|state| state.axes.get(index)) {
				Some(value) => push_signed(&mut screen, *value as i64),
				None => screen.push(b'-'),
			}
			screen.extend_from_slice(b" (");
			push_signed(&mut screen, axis.minimum as i64);
			screen.extend_from_slice(b"..");
			push_signed(&mut screen, axis.maximum as i64);
			screen.push(b')');
		}
		for hat in 0..pad.record.hats as usize {
			screen.extend_from_slice(b"  hat ");
			push_signed(&mut screen, hat as i64 + 1);
			screen.push(b' ');
			screen.extend_from_slice(hat_name(pad.state.as_ref().and_then(|state| state.hats.get(hat).copied()).unwrap_or(8)));
		}
		screen.extend_from_slice(b"\r\n");
	}
	screen.extend_from_slice(b"\r\n");
	screen.extend_from_slice(status);
	output.write(&screen)
}

// How the full screen ended.
enum Ended {
	Quit,
	Closed,
}

// The full screen: redraw, wait for a frame or a key, read everything queued, redraw.
fn watch(output: &mut impl TerminalWriter, stream: u64) -> Ended {
	let input = stdin();
	let mut decoder = InputDecoder::new();
	let mut pads: Vec<Pad> = Vec::new();
	let mut status: Vec<u8> = Vec::from(&b"watching"[..]);
	let mut buf = [0u8; FRAME];
	loop {
		// EVERY EVENT ALREADY QUEUED IS READ BEFORE THE SCREEN IS DRAWN, so a burst of states is one redraw.
		loop {
			match next(stream, &mut buf) {
				Next::Event(event) => apply(&mut pads, event, &mut status),
				Next::Empty => break,
				Next::Closed => return Ended::Closed,
			}
		}
		if !render(output, &pads, &status) || interrupted() {
			return Ended::Quit;
		}
		let ready = if input != 0 { wait_any(&[stream, input], 0) } else { wait_any(&[stream], 0) };
		if interrupted() || ready < 0 {
			return Ended::Quit;
		}
		if ready == 1 {
			let mut bytes = [0u8; 64];
			loop {
				match try_recv(input, &mut bytes) {
					Polled::Message { len, .. } => {
						for &byte in &bytes[..len] {
							match decoder.feed(byte) {
								// Ctrl+C arrives as its control byte, 0x03, with the terminal raw.
								Some(InputEvent::Key(Key::Byte(b'q' | b'Q'))) | Some(InputEvent::Key(Key::Control(0x03))) => return Ended::Quit,
								_ => {}
							}
						}
					}
					Polled::Empty => break,
					Polled::Closed => return Ended::Quit,
				}
			}
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 64];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let mut as_lines = false;
	for word in split_args(context.arguments.as_bytes()) {
		match word {
			b"--lines" => as_lines = true,
			b"--help" | b"-h" => {
				print(USAGE);
				exit();
			}
			_ => {
				eprint(USAGE);
				exit_with(2);
			}
		}
	}
	let Some(connection) = recv_tagged(bootstrap, &mut buf, b"INPUT_GAMEPAD") else {
		eprint(b"gamepad: this launch was granted no gamepad authority\n");
		exit_with(1);
	};
	let Some(stream) = open(connection) else {
		eprint(REFUSED);
		exit_with(1);
	};
	if as_lines {
		lines(stream);
	}
	if stdout() == 0 {
		eprint(b"gamepad: interactive terminal unavailable - `gamepad --lines` prints the stream as lines\n");
		exit_with(1);
	}
	catch_interrupt();
	let mut output = ConsoleWriter::new(stdout());
	let options = TerminalOptions { alternate_screen: true, raw_input: true, disable_echo: true, hide_cursor: true, mouse: MouseTracking::Off, bracketed_paste: false };
	let owns_tty: bool = tty_set_mode(true, false);
	let ended = match TerminalGuard::enter(&mut output, options) {
		Some(mut terminal) => watch(terminal.writer(), stream),
		None => Ended::Quit,
	};
	if owns_tty {
		tty_set_mode(false, true);
	}
	match ended {
		Ended::Quit => exit(),
		Ended::Closed => {
			eprint(CLOSED);
			exit_with(1);
		}
	}
}
