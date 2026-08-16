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

use alloc::string::String;
use alloc::vec::Vec;
use lico::{FileType, HexPattern, HexPatternError, InputDecoder, InputEvent, Key, LineState, MAX_DESCRIPTOR_BYTES, MouseTracking, SyntaxDescriptor, TerminalGuard, TerminalOptions, TerminalWriter, TextQuery, TokenSpan, append_display_line, detect_file_type, parse_descriptor, select_descriptor};
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
		// THIS APPLICATION'S OWN ASSET DIRECTORY, and nothing else on the volume.
		//
		// The syntax descriptors live under `bin/lico/`, the bundle LiberCommander's three programs
		// share, and reading them
		// through the volume bundle above would mean the viewer had every file on every mounted
		// volume in order to colour some keywords. This client is minted by PermissionManager from
		// the private admin endpoint and carries the directory as its SCOPE: a path outside it is
		// refused by StorageService, and the client cannot mint a broader one.
		//
		// Absent on a boot that grants no assets, which is a viewer that highlights nothing rather
		// than a viewer that fails - see `load_descriptors`.
		let assets: u64 = recv_tagged(bootstrap, &mut buf, CAP_APP_ASSETS).unwrap_or(0);
		let descriptors: Vec<SyntaxDescriptor> = load_descriptors(assets);
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
			// THE TTY'S MODES, ASKED FOR RATHER THAN PRINTED. These were `ESC[?9001h` / `ESC[?9002l`
			// in this program's own OUTPUT, where a program's data and its requests are the same bytes -
			// so `cat` on a file holding them reconfigured the terminal. `tty_set_mode` goes over the
			// control channel the shell hands to an interactive foreground job; false means there is no
			// terminal to ask, and the program runs cooked rather than failing.
			let owns_tty: bool = tty_set_mode(true, false);
			if let Some(mut terminal) = TerminalGuard::enter(&mut output, options) {
				let _ = view_file(terminal.writer(), &file, arg, &descriptors);
			}
			// And back to cooked input and echo, through the same request path.
			if owns_tty {
				tty_set_mode(false, true);
			}
		}
	}
	exit();
}

// Read every descriptor in the granted asset directory.
//
// A MISSING OR BAD DESCRIPTOR FALLS BACK TO PLAIN TEXT AND CANNOT PREVENT OPENING THE FILE, which
// is this milestone's rule and is why every failure here is a `continue`: no assets granted, a
// directory that will not list, a file that will not read, a descriptor that does not parse. The
// viewer's job is to show the file.
unsafe fn load_descriptors(assets: u64) -> Vec<SyntaxDescriptor> {
	let mut loaded: Vec<SyntaxDescriptor> = Vec::new();
	if assets == 0 {
		return loaded;
	}
	unsafe {
		let Ok(entries) = tools::list_volume_directory(assets, "vol://system/bin/lico/syntax", 64) else {
			return loaded;
		};
		for entry in &entries {
			if !entry.name.as_bytes().ends_with(b".syntax") {
				continue;
			}
			let mut path = String::from("vol://system/bin/lico/syntax/");
			path.push_str(&entry.name);
			let Ok(bytes) = tools::read_volume_window(assets, &path, 0, MAX_DESCRIPTOR_BYTES as u32) else {
				continue;
			};
			if let Ok(descriptor) = parse_descriptor(&bytes) {
				if loaded.try_reserve(1).is_err() {
					return loaded;
				}
				loaded.push(descriptor);
			}
		}
	}
	loaded
}

// WHAT THE READER IS LOOKING AT. Three views over the same bytes rather than three programs: a
// file that turns out not to be text is read as bytes without reopening it, and a text file whose
// encoding somebody is diagnosing is read as bytes in place.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Mode {
	/// Decoded text, tabs expanded, malformed bytes marked.
	Text,
	/// Every byte as itself, control bytes shown as dots - the bytes with no decoding in the way.
	Raw,
	/// Offset, sixteen bytes, ASCII.
	Hex,
}

// A typed line and what it will be used for. The same mechanism the editor uses, for the same
// reason: a prompt is a mode rather than a second input loop, so every exit path stays one path.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Prompt {
	None,
	Find,
	FindHex,
	GotoLine,
	GotoOffset,
	GotoPercent,
}

const HEX_BYTES_PER_ROW: usize = 16;
const MAX_PROMPT_BYTES: usize = 256;

struct View {
	mode: Mode,
	position: usize,
	wrap: bool,
	numbers: bool,
	highlight: bool,
	prompt: Prompt,
	entry: Vec<u8>,
	// The last query of each kind, kept apart: `n` repeats whichever kind was asked for last, and a
	// hexadecimal query and a text one are different questions about the same file.
	text_needle: Vec<u8>,
	hex_needle: Option<HexPattern>,
	hex_last: bool,
	// How a TEXT query is compared. Both are off by default, because a reader who typed exact
	// characters meant them - and both are shown in the header, so a search that finds nothing has
	// its rule visible rather than remembered.
	ignore_case: bool,
	whole_word: bool,
	status: Vec<u8>,
	// Where the last match began, so a repeat starts after it rather than finding it again.
	last_match: Option<usize>,
}

unsafe fn view_file(output: &mut impl TerminalWriter, bytes: &[u8], name: &[u8], descriptors: &[SyntaxDescriptor]) -> bool {
	unsafe {
		let input = stdin();
		let mut decoder = InputDecoder::new();
		let kind = detect_file_type(name, &bytes[..bytes.len().min(32)], false);
		// A FILE THAT IS NOT TEXT OPENS AS BYTES. Showing a decoded rendering of an executable is
		// showing a reader something that is not there, and the first thing they would do is switch.
		let mode = if matches!(kind, FileType::Binary | FileType::Archive | FileType::Executable | FileType::Image | FileType::Audio) { Mode::Hex } else { Mode::Text };
		let mut view = View { mode, position: 0, wrap: true, numbers: false, highlight: true, prompt: Prompt::None, entry: Vec::new(), text_needle: Vec::new(), hex_needle: None, hex_last: false, ignore_case: false, whole_word: false, status: Vec::new(), last_match: None };
		view.say(b"t/r/x mode  w wrap  # numbers  h light  / find  \\ hex  i case  W word  g/o/% goto  n/p repeat  q quit");
		let mut redraw = true;
		loop {
			if redraw {
				if !render(output, bytes, name, &view, kind, descriptors) {
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
			if ready != 0 {
				continue;
			}
			let mut input_bytes = [0u8; 64];
			loop {
				match try_recv(input, &mut input_bytes) {
					Polled::Message { len, .. } => {
						for &byte in &input_bytes[..len] {
							let Some(event) = decoder.feed(byte) else { continue };
							match view.apply(event, bytes) {
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

impl View {
	fn say(&mut self, message: &[u8]) {
		self.status.clear();
		if self.status.try_reserve(message.len()).is_ok() {
			self.status.extend_from_slice(message);
		}
	}

	fn apply(&mut self, event: InputEvent, bytes: &[u8]) -> ViewAction {
		if self.prompt != Prompt::None {
			return self.apply_prompt(event, bytes);
		}
		let old = self.position;
		match event {
			InputEvent::Key(Key::Byte(b'q')) | InputEvent::Key(Key::Escape) => return ViewAction::Exit,
			InputEvent::Key(Key::Byte(b't')) => return self.set_mode(Mode::Text),
			InputEvent::Key(Key::Byte(b'r')) => return self.set_mode(Mode::Raw),
			InputEvent::Key(Key::Byte(b'x')) => return self.set_mode(Mode::Hex),
			InputEvent::Key(Key::Byte(b'w')) => {
				self.wrap = !self.wrap;
				return ViewAction::Redraw;
			}
			InputEvent::Key(Key::Byte(b'#')) => {
				self.numbers = !self.numbers;
				return ViewAction::Redraw;
			}
			InputEvent::Key(Key::Byte(b'h')) => {
				self.highlight = !self.highlight;
				return ViewAction::Redraw;
			}
			InputEvent::Key(Key::Byte(b'i')) => {
				self.ignore_case = !self.ignore_case;
				self.last_match = None;
				return ViewAction::Redraw;
			}
			InputEvent::Key(Key::Byte(b'W')) => {
				self.whole_word = !self.whole_word;
				self.last_match = None;
				return ViewAction::Redraw;
			}
			InputEvent::Key(Key::Byte(b'/')) => return self.open_prompt(Prompt::Find, b"find text: "),
			InputEvent::Key(Key::Byte(b'\\')) => return self.open_prompt(Prompt::FindHex, b"find bytes (hex, ?? any): "),
			InputEvent::Key(Key::Byte(b'g')) => return self.open_prompt(Prompt::GotoLine, b"go to line: "),
			InputEvent::Key(Key::Byte(b'o')) => return self.open_prompt(Prompt::GotoOffset, b"go to byte offset: "),
			InputEvent::Key(Key::Byte(b'%')) => return self.open_prompt(Prompt::GotoPercent, b"go to percent: "),
			InputEvent::Key(Key::Byte(b'n')) => return self.repeat(bytes, false),
			InputEvent::Key(Key::Byte(b'p')) => return self.repeat(bytes, true),
			InputEvent::Key(Key::ArrowDown) => self.position = self.step(bytes, self.position, true),
			InputEvent::Key(Key::ArrowUp) => self.position = self.step(bytes, self.position, false),
			InputEvent::Key(Key::PageDown) => self.position = self.page(bytes, self.position, true),
			InputEvent::Key(Key::PageUp) => self.position = self.page(bytes, self.position, false),
			InputEvent::Key(Key::Home) => self.position = 0,
			InputEvent::Key(Key::End) => self.position = self.page(bytes, bytes.len(), false),
			InputEvent::Pointer(pointer) if pointer.pressed && pointer.code == 64 => self.position = self.page(bytes, self.position, false),
			InputEvent::Pointer(pointer) if pointer.pressed && pointer.code == 65 => self.position = self.page(bytes, self.position, true),
			_ => {}
		}
		if old == self.position { ViewAction::None } else { ViewAction::Redraw }
	}

	// SWITCHING MODES KEEPS THE PLACE. The position is a byte offset in every mode, so the reader
	// is looking at the same part of the file after the switch - only in the hex view it is rounded
	// down to a row boundary, because a hex dump whose rows do not start on a multiple of sixteen
	// is one nobody can compare against another.
	fn set_mode(&mut self, mode: Mode) -> ViewAction {
		if self.mode == mode {
			return ViewAction::None;
		}
		self.mode = mode;
		if mode == Mode::Hex {
			self.position -= self.position % HEX_BYTES_PER_ROW;
		}
		ViewAction::Redraw
	}

	fn step(&self, bytes: &[u8], from: usize, down: bool) -> usize {
		match self.mode {
			Mode::Hex => {
				if down {
					(from + HEX_BYTES_PER_ROW).min(bytes.len() - bytes.len() % HEX_BYTES_PER_ROW)
				} else {
					from.saturating_sub(HEX_BYTES_PER_ROW)
				}
			}
			_ => {
				if down {
					next_line(bytes, from)
				} else {
					previous_line(bytes, from)
				}
			}
		}
	}

	fn page(&self, bytes: &[u8], mut from: usize, down: bool) -> usize {
		for _ in 0..VIEW_ROWS {
			let next = self.step(bytes, from, down);
			if next == from {
				break;
			}
			from = next;
		}
		from
	}

	fn open_prompt(&mut self, prompt: Prompt, label: &[u8]) -> ViewAction {
		self.prompt = prompt;
		self.entry.clear();
		self.say(label);
		ViewAction::Redraw
	}

	fn apply_prompt(&mut self, event: InputEvent, bytes: &[u8]) -> ViewAction {
		match event {
			InputEvent::Key(Key::Escape) => {
				self.prompt = Prompt::None;
				self.entry.clear();
				self.say(b"cancelled");
				ViewAction::Redraw
			}
			InputEvent::Key(Key::Backspace) => {
				self.entry.pop();
				ViewAction::Redraw
			}
			InputEvent::Key(Key::Enter) => self.commit_prompt(bytes),
			InputEvent::Key(Key::Byte(byte)) if byte >= 0x20 && byte != 0x7f => {
				if self.entry.len() < MAX_PROMPT_BYTES && self.entry.try_reserve(1).is_ok() {
					self.entry.push(byte);
				}
				ViewAction::Redraw
			}
			_ => ViewAction::None,
		}
	}

	fn commit_prompt(&mut self, bytes: &[u8]) -> ViewAction {
		let prompt = self.prompt;
		let entry = core::mem::take(&mut self.entry);
		self.prompt = Prompt::None;
		match prompt {
			Prompt::Find => {
				self.text_needle = entry;
				self.hex_last = false;
				self.last_match = None;
				self.repeat(bytes, false)
			}
			Prompt::FindHex => {
				// THE WHOLE PATTERN IS VALIDATED BEFORE ANYTHING IS SCANNED, and a bad one says
				// which part of it is bad rather than reporting "not found" about a query that was
				// never a query.
				match HexPattern::parse(&entry) {
					Ok(pattern) => {
						self.hex_needle = Some(pattern);
						self.hex_last = true;
						self.last_match = None;
						self.repeat(bytes, false)
					}
					Err(error) => {
						self.say(hex_error(error));
						ViewAction::Redraw
					}
				}
			}
			Prompt::GotoLine => match parse_decimal(&entry) {
				Some(line) => {
					let mut at = 0;
					for _ in 1..line.max(1) {
						let next = next_line(bytes, at);
						if next == at {
							break;
						}
						at = next;
					}
					self.position = at;
					self.say(b"");
					ViewAction::Redraw
				}
				None => {
					self.say(b"that is not a line number");
					ViewAction::Redraw
				}
			},
			Prompt::GotoOffset => match parse_decimal(&entry) {
				Some(offset) => {
					self.jump(bytes, offset.min(bytes.len()));
					ViewAction::Redraw
				}
				None => {
					self.say(b"that is not a byte offset");
					ViewAction::Redraw
				}
			},
			Prompt::GotoPercent => match parse_decimal(&entry) {
				Some(percent) if percent <= 100 => {
					self.jump(bytes, bytes.len() / 100 * percent.min(100));
					ViewAction::Redraw
				}
				_ => {
					self.say(b"a percentage is 0 to 100");
					ViewAction::Redraw
				}
			},
			Prompt::None => ViewAction::Redraw,
		}
	}

	// Put a byte offset on the screen. In the text views the view starts at a line boundary, so an
	// offset in the middle of a line shows that whole line rather than its tail.
	fn jump(&mut self, bytes: &[u8], offset: usize) {
		self.position = match self.mode {
			Mode::Hex => offset - offset % HEX_BYTES_PER_ROW,
			_ => line_start(bytes, offset),
		};
		self.last_match = Some(offset);
		self.say(b"");
	}

	// The next match of whichever query was asked for last. A SEARCH THAT FINDS NOTHING LEAVES THE
	// VIEW WHERE IT WAS and says so - moving to the end on a failed search loses the reader's place,
	// which is the thing they were looking at when they typed the query.
	fn repeat(&mut self, bytes: &[u8], backward: bool) -> ViewAction {
		let anchor = self.last_match.unwrap_or(self.position);
		let from = if backward { anchor } else { anchor + 1 };
		let hit = if self.hex_last {
			match self.hex_needle.as_ref() {
				Some(pattern) => pattern.find(bytes, from, backward),
				None => {
					self.say(b"no byte pattern yet - press \\ first");
					return ViewAction::Redraw;
				}
			}
		} else if self.text_needle.is_empty() {
			self.say(b"nothing to find yet - press / first");
			return ViewAction::Redraw;
		} else {
			TextQuery { needle: &self.text_needle, ignore_case: self.ignore_case, whole_word: self.whole_word }.find(bytes, from, backward)
		};
		match hit {
			Some(at) => {
				self.jump(bytes, at);
				let mut message: Vec<u8> = Vec::new();
				if message.try_reserve(32).is_ok() {
					message.extend_from_slice(b"match at byte ");
					append_decimal(&mut message, at);
					self.status = message;
				}
				ViewAction::Redraw
			}
			None => {
				self.say(b"not found - the view stays where it was");
				ViewAction::Redraw
			}
		}
	}
}

fn hex_error(error: HexPatternError) -> &'static [u8] {
	match error {
		HexPatternError::Empty => b"a byte pattern needs at least one byte",
		HexPatternError::OddNibble => b"a byte is two hexadecimal digits - that names half of one",
		HexPatternError::NotHexadecimal => b"only hexadecimal digits, spaces and ?? are allowed",
		HexPatternError::TooLong => b"that pattern is longer than the search will carry",
	}
}

fn parse_decimal(bytes: &[u8]) -> Option<usize> {
	if bytes.is_empty() {
		return None;
	}
	let mut value: usize = 0;
	for &byte in bytes {
		let digit = byte.checked_sub(b'0').filter(|digit| *digit < 10)?;
		value = value.checked_mul(10)?.checked_add(digit as usize)?;
	}
	Some(value)
}

fn render(output: &mut impl TerminalWriter, bytes: &[u8], name: &[u8], view: &View, kind: FileType, descriptors: &[SyntaxDescriptor]) -> bool {
	let mut rendered: Vec<u8> = Vec::new();
	if rendered.try_reserve((VIEW_ROWS + 4) * (VIEW_COLUMNS * 4 + 32)).is_err() {
		return false;
	}
	rendered.extend_from_slice(b"\x1b[H\x1b[2J\x1b[1mlicoview\x1b[0m ");
	append_safe(&mut rendered, name, 32);
	rendered.extend_from_slice(b"  ");
	rendered.extend_from_slice(type_label(kind));
	rendered.extend_from_slice(match view.mode {
		Mode::Text => b"  text",
		Mode::Raw => b"  raw",
		Mode::Hex => b"  hex",
	});
	if view.ignore_case {
		rendered.extend_from_slice(b"  ignore-case");
	}
	if view.whole_word {
		rendered.extend_from_slice(b"  whole-word");
	}
	rendered.extend_from_slice(b"  byte ");
	append_decimal(&mut rendered, view.position);
	rendered.push(b'/');
	append_decimal(&mut rendered, bytes.len());
	rendered.push(b'\n');
	append_safe(&mut rendered, &view.status, VIEW_COLUMNS);
	if view.prompt != Prompt::None {
		append_safe(&mut rendered, &view.entry, 64);
		rendered.push(b'_');
	}
	rendered.extend_from_slice(b"\n\n");
	let ok = match view.mode {
		Mode::Hex => render_hex(&mut rendered, bytes, view),
		_ => render_lines(&mut rendered, bytes, view, name, descriptors),
	};
	ok && output.write(&rendered)
}

fn render_hex(output: &mut Vec<u8>, bytes: &[u8], view: &View) -> bool {
	let mut at = view.position - view.position % HEX_BYTES_PER_ROW;
	for _ in 0..VIEW_ROWS {
		if at >= bytes.len() {
			break;
		}
		append_hex_offset(output, at);
		output.extend_from_slice(b"  ");
		let end = (at + HEX_BYTES_PER_ROW).min(bytes.len());
		for index in at..at + HEX_BYTES_PER_ROW {
			if index < end {
				append_hex_byte(output, bytes[index]);
			} else {
				output.extend_from_slice(b"  ");
			}
			output.push(b' ');
			if index - at == HEX_BYTES_PER_ROW / 2 - 1 {
				output.push(b' ');
			}
		}
		output.push(b'|');
		for &byte in &bytes[at..end] {
			// A BYTE WRITTEN AS ITSELF PUTS CONTROL CHARACTERS ON THE TERMINAL, which is how a dump
			// of a binary file changes the terminal's mode - the same rule `hexdump` follows.
			output.push(if (0x20..0x7f).contains(&byte) { byte } else { b'.' });
		}
		output.push(b'|');
		output.push(b'\n');
		at = end.max(at + HEX_BYTES_PER_ROW);
	}
	true
}

fn render_lines(output: &mut Vec<u8>, bytes: &[u8], view: &View, name: &[u8], descriptors: &[SyntaxDescriptor]) -> bool {
	// The descriptor is chosen ONCE per render from the file's name and its first line, and a file
	// that matches nothing is plain text - which is this milestone's rule, and the reason every
	// failure here is an absence rather than an error.
	let first_line = &bytes[..line_end(bytes, 0).min(bytes.len())];
	let selection = if view.highlight && view.mode == Mode::Text { select_descriptor(descriptors, name, first_line) } else { None };
	let mut state = LineState::new();
	let mut spans = [TokenSpan { start: 0, end: 0, style: 0 }; 64];
	let mut line_start_offset = view.position.min(bytes.len());
	let mut number = 1 + bytes[..line_start_offset].iter().filter(|&&byte| byte == b'\n').count();
	let columns = if view.numbers { VIEW_COLUMNS - 7 } else { VIEW_COLUMNS };
	for _ in 0..VIEW_ROWS {
		if line_start_offset >= bytes.len() {
			break;
		}
		let end = line_end(bytes, line_start_offset);
		let visible_end = if end > line_start_offset && bytes[end - 1] == b'\r' { end - 1 } else { end };
		let line = &bytes[line_start_offset..visible_end];
		if view.numbers {
			append_padded_decimal(output, number, 5);
			output.extend_from_slice(b"  ");
		}
		match view.mode {
			Mode::Raw => append_raw_line(output, line, columns, view.wrap),
			_ => {
				if let Some(selected) = selection.as_ref() {
					let result = selected.descriptor.highlight_line(&mut state, line, &mut spans);
					if !append_highlighted(output, line, &spans[..result.spans.min(spans.len())], columns) {
						return false;
					}
				} else if append_display_line(line, columns, 8, output).is_err() {
					return false;
				}
			}
		}
		output.push(b'\n');
		let next = next_line(bytes, line_start_offset);
		if next == line_start_offset {
			break;
		}
		line_start_offset = next;
		number += 1;
	}
	true
}

// The spans carry style ids; the palette is one SGR colour per style, chosen so that a terminal
// with no colour still shows the text - the escape is around the token and never replaces it.
fn append_highlighted(output: &mut Vec<u8>, line: &[u8], spans: &[TokenSpan], columns: usize) -> bool {
	let mut at = 0;
	let mut written = 0;
	for span in spans {
		if span.start >= line.len() || span.end > line.len() || span.start < at {
			continue;
		}
		if append_display_line(&line[at..span.start], columns.saturating_sub(written), 8, output).is_err() {
			return false;
		}
		written += span.start - at;
		output.extend_from_slice(style_escape(span.style));
		if append_display_line(&line[span.start..span.end], columns.saturating_sub(written), 8, output).is_err() {
			return false;
		}
		output.extend_from_slice(b"\x1b[0m");
		written += span.end - span.start;
		at = span.end;
	}
	append_display_line(&line[at..], columns.saturating_sub(written), 8, output).is_ok()
}

fn style_escape(style: u8) -> &'static [u8] {
	match style % 6 {
		0 => b"\x1b[36m",
		1 => b"\x1b[33m",
		2 => b"\x1b[32m",
		3 => b"\x1b[35m",
		4 => b"\x1b[31m",
		_ => b"\x1b[34m",
	}
}

// RAW IS EVERY BYTE AS ITSELF, with control bytes shown rather than obeyed. With wrapping off the
// line is cut at the width, which is what a reader asks for when they are looking at columns.
fn append_raw_line(output: &mut Vec<u8>, line: &[u8], columns: usize, wrap: bool) {
	let mut written = 0;
	for &byte in line {
		if !wrap && written == columns {
			break;
		}
		output.push(if (0x20..0x7f).contains(&byte) { byte } else { b'.' });
		written += 1;
		if wrap && written == columns {
			output.push(b'\n');
			written = 0;
		}
	}
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

fn append_padded_decimal(output: &mut Vec<u8>, value: usize, width: usize) {
	let mut digits = [0u8; 20];
	let mut left = value;
	let mut count = 0;
	loop {
		digits[count] = b'0' + (left % 10) as u8;
		left /= 10;
		count += 1;
		if left == 0 {
			break;
		}
	}
	for _ in count..width {
		output.push(b' ');
	}
	for index in (0..count).rev() {
		output.push(digits[index]);
	}
}

fn append_hex_offset(output: &mut Vec<u8>, value: usize) {
	for shift in (0..8).rev() {
		output.push(hex_digit(((value >> (shift * 4)) & 0xf) as u8));
	}
}

fn append_hex_byte(output: &mut Vec<u8>, byte: u8) {
	output.push(hex_digit(byte >> 4));
	output.push(hex_digit(byte & 0xf));
}

fn hex_digit(value: u8) -> u8 {
	if value < 10 { b'0' + value } else { b'a' + value - 10 }
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

fn line_start(bytes: &[u8], offset: usize) -> usize {
	let mut at = offset.min(bytes.len());
	while at > 0 && bytes[at - 1] != b'\n' {
		at -= 1;
	}
	at
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

fn trim(mut bytes: &[u8]) -> &[u8] {
	while bytes.first().is_some_and(|byte| byte.is_ascii_whitespace()) {
		bytes = &bytes[1..];
	}
	while bytes.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
		bytes = &bytes[..bytes.len() - 1];
	}
	bytes
}
