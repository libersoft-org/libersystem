// licoview - governed fullscreen text preview.
//
// This first viewer slice proves the shared LiberCommander terminal lifecycle on a real
// executable. It reads only a path from its explicitly granted volume clients, bounds the
// mapped file, owns raw/alternate terminal modes while active, and restores them on every
// normal, signal, terminal-disconnect, and output-failure exit. Streaming, raw/hex modes,
// search, syntax assets, and selected-file grants land in later LiberCommander slices.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use lico::{FileType, InputDecoder, InputEvent, Key, MouseTracking, TerminalGuard, TerminalOptions, TerminalWriter, append_display_line, detect_file_type};
use proto::system::LaunchContext;
use rt::*;
use storage_proto::path;
use tools::{ConsoleWriter, VolumeSet, read_volume_file};
use volume_client_provider as _;

const MAX_VIEW_BYTES: usize = 512 * 1024;
const VIEW_COLUMNS: usize = 80;
const VIEW_ROWS: usize = 20;

#[derive(Clone, Copy, Eq, PartialEq)]
enum ViewAction {
	None,
	Redraw,
	Exit,
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	unsafe {
		inherit_stdout(bootstrap);
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let arg: Vec<u8> = context.arguments.clone().into_bytes();
		let volumes = VolumeSet::receive(bootstrap, &mut buf);
		let cwd: Vec<u8> = context.cwd.clone().into_bytes();
		let cwd = core::str::from_utf8(&cwd).unwrap_or("");
		let arg = trim(&arg);
		if arg.is_empty() || arg.iter().any(u8::is_ascii_whitespace) {
			print(b"Usage: licoview PATH\n");
			exit();
		}
		let Some(uri) = path::resolve(cwd, arg) else {
			eprint(b"licoview: invalid path\n");
			exit();
		};
		let storage = volumes.client_for(cwd, arg);
		let file = match read_volume_file(storage, &uri, MAX_VIEW_BYTES) {
			Ok(file) => file,
			Err(_) => {
				eprint(b"licoview: cannot open file or it exceeds the current 512 kB limit\n");
				exit();
			}
		};
		if stdin() == 0 || stdout() == 0 {
			eprint(b"licoview: interactive terminal unavailable\n");
		} else {
			catch_interrupt();
			let mut output = ConsoleWriter::new(stdout());
			let options = TerminalOptions { alternate_screen: true, raw_input: true, disable_echo: true, hide_cursor: true, mouse: MouseTracking::Press, bracketed_paste: false };
			if let Some(mut terminal) = TerminalGuard::enter(&mut output, options) {
				let _ = view_file(terminal.writer(), &file, arg);
			}
		}
	}
	exit();
}

unsafe fn view_file(output: &mut impl TerminalWriter, bytes: &[u8], name: &[u8]) -> bool {
	unsafe {
		let input = stdin();
		let mut decoder = InputDecoder::new();
		let mut position = 0;
		let mut redraw = true;
		loop {
			if redraw {
				if !render(output, bytes, name, position) {
					return false;
				}
				redraw = false;
			}
			if interrupted() {
				return true;
			}
			let ready = wait_any(&[input], 0);
			if interrupted() {
				return true;
			}
			if ready < 0 {
				return true;
			}
			if ready != 0 {
				continue;
			}
			let mut input_bytes = [0u8; 64];
			loop {
				match try_recv(input, &mut input_bytes) {
					Polled::Message { len, .. } => {
						for &byte in &input_bytes[..len] {
							let Some(event) = decoder.feed(byte) else { continue };
							match apply_event(event, bytes, &mut position) {
								ViewAction::None => {}
								ViewAction::Redraw => redraw = true,
								ViewAction::Exit => return true,
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

fn apply_event(event: InputEvent, bytes: &[u8], position: &mut usize) -> ViewAction {
	let old = *position;
	match event {
		InputEvent::Key(Key::Byte(b'q')) | InputEvent::Key(Key::Escape) => return ViewAction::Exit,
		InputEvent::Key(Key::ArrowDown) => *position = next_line(bytes, *position),
		InputEvent::Key(Key::ArrowUp) => *position = previous_line(bytes, *position),
		InputEvent::Key(Key::PageDown) => *position = advance_lines(bytes, *position, VIEW_ROWS),
		InputEvent::Key(Key::PageUp) => *position = rewind_lines(bytes, *position, VIEW_ROWS),
		InputEvent::Key(Key::Home) => *position = 0,
		InputEvent::Key(Key::End) => *position = rewind_lines(bytes, bytes.len(), VIEW_ROWS.saturating_sub(1)),
		InputEvent::Pointer(pointer) if pointer.pressed && pointer.code == 64 => *position = rewind_lines(bytes, *position, VIEW_ROWS),
		InputEvent::Pointer(pointer) if pointer.pressed && pointer.code == 65 => *position = advance_lines(bytes, *position, VIEW_ROWS),
		_ => {}
	}
	if old == *position { ViewAction::None } else { ViewAction::Redraw }
}

fn render(output: &mut impl TerminalWriter, bytes: &[u8], name: &[u8], position: usize) -> bool {
	if !output.write(b"\x1b[H\x1b[2J\x1b[1mlicoview\x1b[0m  arrows/page scroll, wheel scroll, Home/End, q/Esc quit\n") {
		return false;
	}
	let kind = detect_file_type(name, &bytes[..bytes.len().min(32)], false);
	if !output.write(type_label(kind)) || !output.write(b" preview\n\n") {
		return false;
	}
	let mut line_start = position.min(bytes.len());
	for _ in 0..VIEW_ROWS {
		if line_start >= bytes.len() {
			break;
		}
		let end = line_end(bytes, line_start);
		let visible_end = if end > line_start && bytes[end - 1] == b'\r' { end - 1 } else { end };
		if !write_visible_line(output, &bytes[line_start..visible_end]) || !output.write(b"\n") {
			return false;
		}
		line_start = next_line(bytes, line_start);
	}
	true
}

fn write_visible_line(output: &mut impl TerminalWriter, bytes: &[u8]) -> bool {
	let mut rendered = Vec::new();
	append_display_line(bytes, VIEW_COLUMNS, 8, &mut rendered).is_ok() && output.write(&rendered)
}

fn type_label(kind: FileType) -> &'static [u8] {
	match kind {
		FileType::Rust => b"Rust",
		FileType::Lsidl => b"LSIDL",
		FileType::Toml => b"TOML",
		FileType::Json => b"JSON",
		FileType::Markdown => b"Markdown",
		FileType::Shell => b"shell",
		FileType::Config => b"config",
		FileType::Text => b"text",
		FileType::Binary => b"binary",
		FileType::Directory => b"directory",
		FileType::Image => b"image",
		FileType::Audio => b"audio",
		FileType::Archive => b"archive",
		FileType::Executable => b"executable",
	}
}

fn line_end(bytes: &[u8], start: usize) -> usize {
	let mut offset = start.min(bytes.len());
	while offset < bytes.len() && bytes[offset] != b'\n' {
		offset += 1;
	}
	offset
}

fn next_line(bytes: &[u8], start: usize) -> usize {
	let end = line_end(bytes, start);
	if end < bytes.len() { end + 1 } else { end }
}

fn previous_line(bytes: &[u8], start: usize) -> usize {
	if start == 0 {
		return 0;
	}
	let mut offset = start.saturating_sub(1);
	if offset > 0 && bytes[offset] == b'\n' {
		offset -= 1;
	}
	while offset > 0 && bytes[offset - 1] != b'\n' {
		offset -= 1;
	}
	offset
}

fn advance_lines(bytes: &[u8], mut start: usize, count: usize) -> usize {
	for _ in 0..count {
		let next = next_line(bytes, start);
		if next == start {
			break;
		}
		start = next;
	}
	start
}

fn rewind_lines(bytes: &[u8], mut start: usize, count: usize) -> usize {
	for _ in 0..count {
		let previous = previous_line(bytes, start);
		if previous == start {
			break;
		}
		start = previous;
	}
	start
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
