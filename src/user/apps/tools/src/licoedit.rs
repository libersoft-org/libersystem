// licoedit - governed bounded in-memory text editor.
//
// This initial editor deliberately has no persistence path until a transactional writer/write-back
// grant is available. It can navigate and edit a bounded text buffer, reports that saving is
// unavailable, and leaves the original file untouched on every exit.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use lico::{FileType, InputDecoder, InputEvent, Key, MouseTracking, TerminalGuard, TerminalOptions, TerminalWriter, TextBuffer, TextBufferError, append_display_line, detect_file_type};
use proto::system::LaunchContext;
use rt::*;
use storage_proto::path;
use tools::{ConsoleWriter, VolumeSet, read_volume_file};
use volume_client_provider as _;

const MAX_EDIT_BYTES: usize = 512 * 1024;
const EDIT_COLUMNS: usize = 72;
const EDIT_ROWS: usize = 18;

#[derive(Clone, Copy, Eq, PartialEq)]
enum EditAction {
	None,
	Redraw,
	Exit,
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut bootstrap_buffer = [0u8; 256];
	unsafe {
		inherit_stdout(bootstrap);
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let argument: Vec<u8> = context.arguments.clone().into_bytes();
		let volumes = VolumeSet::receive(bootstrap, &mut bootstrap_buffer);
		let cwd: Vec<u8> = context.cwd.clone().into_bytes();
		let cwd = core::str::from_utf8(&cwd).unwrap_or("");
		let argument = trim(&argument);
		if argument.is_empty() || argument.iter().any(u8::is_ascii_whitespace) {
			print(b"Usage: licoedit PATH\n");
			exit();
		}
		let Some(uri) = path::resolve(cwd, argument) else {
			eprint(b"licoedit: invalid path\n");
			exit();
		};
		let storage = volumes.client_for(cwd, argument);
		let file = match read_volume_file(storage, &uri, MAX_EDIT_BYTES) {
			Ok(file) => file,
			Err(_) => {
				eprint(b"licoedit: cannot open file or it exceeds the current 512 kB limit\n");
				exit();
			}
		};
		if matches!(detect_file_type(argument, &file[..file.len().min(32)], false), FileType::Binary | FileType::Archive | FileType::Executable | FileType::Image | FileType::Audio) {
			eprint(b"licoedit: binary content opens read-only in licoview\n");
			exit();
		}
		let mut buffer = match TextBuffer::from_bytes(&file, MAX_EDIT_BYTES) {
			Ok(buffer) => buffer,
			Err(_) => {
				eprint(b"licoedit: cannot allocate editor buffer\n");
				exit();
			}
		};
		if stdin() == 0 || stdout() == 0 {
			eprint(b"licoedit: interactive terminal unavailable\n");
		} else {
			catch_interrupt();
			let mut output = ConsoleWriter::new(stdout());
			let options = TerminalOptions { alternate_screen: true, raw_input: true, disable_echo: true, hide_cursor: true, mouse: MouseTracking::Off, bracketed_paste: false };
			// THE TTY'S MODES, ASKED FOR RATHER THAN PRINTED. These were `ESC[?9001h` / `ESC[?9002l`
			// in this program's own OUTPUT, where a program's data and its requests are the same bytes -
			// so `cat` on a file holding them reconfigured the terminal. `tty_set_mode` goes over the
			// control channel the shell hands to an interactive foreground job; false means there is no
			// terminal to ask, and the program runs cooked rather than failing.
			let owns_tty: bool = unsafe { tty_set_mode(true, false) };
			if let Some(mut terminal) = TerminalGuard::enter(&mut output, options) {
				let _ = edit(terminal.writer(), &mut buffer, argument);
			}
			// And back to cooked input and echo, through the same request path.
			if owns_tty {
				unsafe { tty_set_mode(false, true) };
			}
		}
	}
	exit();
}

unsafe fn edit(output: &mut impl TerminalWriter, buffer: &mut TextBuffer, name: &[u8]) -> bool {
	unsafe {
		let input = stdin();
		let mut decoder = InputDecoder::new();
		let mut status: &[u8] = b"F10/Esc exit  Ctrl+S save unavailable";
		let mut redraw = true;
		loop {
			if redraw {
				if !render(output, buffer, name, status) {
					return false;
				}
				redraw = false;
			}
			if interrupted() {
				return true;
			}
			let ready = wait_any(&[input], 0);
			if interrupted() || ready < 0 {
				return true;
			}
			let mut bytes = [0u8; 64];
			loop {
				match try_recv(input, &mut bytes) {
					Polled::Message { len, .. } => {
						for &byte in &bytes[..len] {
							let Some(event) = decoder.feed(byte) else { continue };
							match apply_event(event, buffer, &mut status) {
								EditAction::None => {}
								EditAction::Redraw => redraw = true,
								EditAction::Exit => return true,
							}
						}
					}
					Polled::Empty => break,
					Polled::Closed => return true,
				}
			}
		}
	}
}

fn apply_event(event: InputEvent, buffer: &mut TextBuffer, status: &mut &[u8]) -> EditAction {
	match event {
		InputEvent::Key(Key::Escape) | InputEvent::Key(Key::Function(10)) => EditAction::Exit,
		InputEvent::Key(Key::Control(0x13)) => {
			*status = b"save requires a transactional writer";
			EditAction::Redraw
		}
		InputEvent::Key(Key::ArrowLeft) => changed(buffer.move_left()),
		InputEvent::Key(Key::ArrowRight) => changed(buffer.move_right()),
		InputEvent::Key(Key::ArrowUp) => changed(buffer.move_up()),
		InputEvent::Key(Key::ArrowDown) => changed(buffer.move_down()),
		InputEvent::Key(Key::PageUp) => changed(repeat(buffer, false)),
		InputEvent::Key(Key::PageDown) => changed(repeat(buffer, true)),
		InputEvent::Key(Key::Home) => changed(buffer.move_home()),
		InputEvent::Key(Key::End) => changed(buffer.move_end()),
		InputEvent::Key(Key::Backspace) => changed(buffer.delete_before()),
		InputEvent::Key(Key::Delete) => changed(buffer.delete_at()),
		InputEvent::Key(Key::Enter) => insert(buffer, b'\n', status),
		InputEvent::Key(Key::Byte(byte)) if byte >= 0x20 && byte != 0x7f => insert(buffer, byte, status),
		_ => EditAction::None,
	}
}

fn changed(changed: bool) -> EditAction {
	if changed { EditAction::Redraw } else { EditAction::None }
}

fn repeat(buffer: &mut TextBuffer, down: bool) -> bool {
	let mut changed = false;
	for _ in 0..EDIT_ROWS {
		let moved = if down { buffer.move_down() } else { buffer.move_up() };
		if !moved {
			break;
		}
		changed = true;
	}
	changed
}

fn insert(buffer: &mut TextBuffer, byte: u8, status: &mut &[u8]) -> EditAction {
	match buffer.insert(byte) {
		Ok(()) => EditAction::Redraw,
		Err(TextBufferError::TooLarge) => {
			*status = b"editor buffer limit reached";
			EditAction::Redraw
		}
		Err(TextBufferError::OutOfMemory) => {
			*status = b"editor out of memory";
			EditAction::Redraw
		}
	}
}

fn render(output: &mut impl TerminalWriter, buffer: &TextBuffer, name: &[u8], status: &[u8]) -> bool {
	let mut rendered = Vec::new();
	if rendered.try_reserve_exact((EDIT_ROWS + 4) * (EDIT_COLUMNS * 4 + 24)).is_err() {
		return false;
	}
	rendered.extend_from_slice(b"\x1b[H\x1b[2J\x1b[1mlicoedit\x1b[0m ");
	append_safe(&mut rendered, name, 40);
	rendered.extend_from_slice(if buffer.is_dirty() { b"  modified\n" } else { b"  clean\n" });
	rendered.extend_from_slice(status);
	rendered.extend_from_slice(b"\n\n");

	let cursor_line = buffer.line_start_at(buffer.cursor());
	let mut start = cursor_line;
	for _ in 0..EDIT_ROWS / 2 {
		let previous = previous_line_start(buffer, start);
		if previous == start {
			break;
		}
		start = previous;
	}
	let mut line_number = 1 + buffer.bytes()[..start].iter().filter(|&&byte| byte == b'\n').count();
	for _ in 0..EDIT_ROWS {
		if start > buffer.bytes().len() {
			break;
		}
		rendered.push(if start == cursor_line { b'>' } else { b' ' });
		append_decimal(&mut rendered, line_number);
		rendered.extend_from_slice(b" ");
		let line = buffer.line_at(start);
		let line = line.strip_suffix(b"\r").unwrap_or(line);
		if append_display_line(line, EDIT_COLUMNS, 8, &mut rendered).is_err() {
			return false;
		}
		rendered.push(b'\n');
		let next = buffer.next_line_start(start);
		if next == start {
			break;
		}
		start = next;
		line_number += 1;
	}
	output.write(&rendered)
}

fn previous_line_start(buffer: &TextBuffer, start: usize) -> usize {
	if start == 0 {
		return 0;
	}
	buffer.line_start_at(start.saturating_sub(1))
}

fn append_safe(output: &mut Vec<u8>, bytes: &[u8], limit: usize) {
	for &byte in bytes.iter().take(limit) {
		output.push(if byte < 0x20 || byte == 0x7f { b'.' } else { byte });
	}
}

fn append_decimal(output: &mut Vec<u8>, value: usize) {
	let mut digits = [0u8; 20];
	let mut value = value;
	let mut count = 0;
	loop {
		digits[count] = b'0' + (value % 10) as u8;
		value /= 10;
		count += 1;
		if value == 0 {
			break;
		}
	}
	for index in (0..count).rev() {
		output.push(digits[index]);
	}
}

fn trim(mut bytes: &[u8]) -> &[u8] {
	while bytes.first().is_some_and(|byte| byte.is_ascii_whitespace()) {
		bytes = &bytes[1..];
	}
	while bytes.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
		bytes = &bytes[..bytes.len() - 1];
	}
	bytes
}
