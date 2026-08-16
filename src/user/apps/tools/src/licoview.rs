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
use lico::{FileType, HexPattern, HexPatternError, InputDecoder, InputEvent, Key, LineState, MAX_DESCRIPTOR_BYTES, MouseTracking, SyntaxDescriptor, TerminalGuard, TerminalOptions, TerminalWriter, TokenSpan, append_display_line, detect_file_type, parse_descriptor, select_descriptor};
use proto::system::{LaunchContext, SelectedFile};
use rt::*;
use storage_proto::path;
use tools::{ConsoleWriter, VolumeSet, read_volume_file, read_volume_window};
use volume_client_provider as _;

// How much of a file is held at once when it is PAGED, so the viewer's memory is a function of the
// screen rather than of the file.
const WINDOW_BYTES: usize = 64 * 1024;
// The most of one line the viewer will render. A file with no newline in it is one enormous line.
const MAX_LINE_BYTES: usize = 8192;
// A file at or below this is read whole; above it the viewer pages.
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
		// THE SELECTED-FILE TAG COMES FIRST, where the launcher sends it - before the vocabulary
		// grants. It is always sent to this program, bare when there was no file, because a tag read
		// where nothing arrives consumes the next message and then blocks forever.
		let selected: u64 = recv_tagged(bootstrap, &mut buf, CAP_SELECTED_FILE).unwrap_or(0);
		let opened: Option<SelectedFile> = if selected == 0 { None } else { recv_launch_bytes(bootstrap).as_deref().and_then(SelectedFile::decode) };
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
		// THE SELECTED-FILE GRANT, WHEN THERE IS ONE. A launch over one file hands this program a
		// client scoped to exactly that path - not the directory it sits in, not a sibling - so the
		// viewer opened on a file cannot reopen the file beside it or list the directory to find
		// out what those are. The record that follows says which URI to open through it.
		//
		// ABSENT IS THE ORDINARY LAUNCH, which is why this is an `Option` rather than a
		// requirement: `licoview PATH` typed at a shell still resolves a path against the volume
		// bundle its own manifest grants, and that path is checked the way it always was.
		let (uri, storage) = match opened.as_ref() {
			Some(opened) => (String::from(opened.uri.as_str()), selected),
			None => {
				if arg.is_empty() || arg.iter().any(u8::is_ascii_whitespace) {
					print(b"Usage: licoview PATH\n");
					exit();
				}
				let Some(uri) = path::resolve(cwd, arg) else {
					eprint(b"licoview: invalid path\n");
					exit();
				};
				let storage = volumes.client_for(cwd, arg);
				(uri, storage)
			}
		};
		let arg: &[u8] = match opened.as_ref() {
			Some(opened) => opened.name.as_bytes(),
			None => arg,
		};
		let mut source = match Source::open(storage, &uri) {
			Some(source) => source,
			None => {
				eprint(b"licoview: cannot open that file\n");
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
				let _ = view_file(terminal.writer(), &mut source, arg, &descriptors);
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

// WHERE THE BYTES COME FROM.
//
// A file that fits is HELD; one that does not is PAGED, through a table of line-start offsets built
// in one pass and one window in hand. Everything above this reads through the same three methods, so
// there is one rendering path and one search rather than two - and the streamed one is not the one
// nobody exercises.
//
// The index costs eight bytes a line against the line itself, and it is what makes going BACKWARD
// cheap: a forward-only viewer is `head` with extra steps, and finding a line again needs either
// the whole file or a table like this one.
enum Source {
	Held { bytes: Vec<u8>, lines: Vec<u64> },
	Paged { storage: u64, uri: String, len: u64, lines: Vec<u64>, window: Vec<u8>, window_at: u64 },
}

impl Source {
	// Read the file, or - when it is past the bound - index it and page it.
	unsafe fn open(storage: u64, uri: &str) -> Option<Source> {
		if let Ok(bytes) = unsafe { read_volume_file(storage, uri, MAX_VIEW_BYTES) } {
			let lines = line_index(&bytes);
			return Some(Source::Held { bytes, lines });
		}
		let mut lines: Vec<u64> = Vec::new();
		lines.try_reserve(1024).ok()?;
		lines.push(0);
		let mut at: u64 = 0;
		loop {
			let window = unsafe { read_volume_window(storage, uri, at, WINDOW_BYTES as u32) }.ok()?;
			if window.is_empty() {
				break;
			}
			for (index, byte) in window.iter().enumerate() {
				if *byte == b'\n' && lines.try_reserve(1).is_ok() {
					lines.push(at + index as u64 + 1);
				}
			}
			at += window.len() as u64;
		}
		if at == 0 {
			return None;
		}
		let mut owned = String::new();
		owned.try_reserve(uri.len()).ok()?;
		owned.push_str(uri);
		Some(Source::Paged { storage, uri: owned, len: at, lines, window: Vec::new(), window_at: 0 })
	}

	fn len(&self) -> usize {
		match self {
			Source::Held { bytes, .. } => bytes.len(),
			Source::Paged { len, .. } => *len as usize,
		}
	}

	fn lines(&self) -> &[u64] {
		match self {
			Source::Held { lines, .. } => lines,
			Source::Paged { lines, .. } => lines,
		}
	}

	// At most `limit` bytes from `offset`. A held file slices; a paged one reads the window that
	// covers the offset, re-reading only when the one in hand does not.
	fn read(&mut self, offset: usize, limit: usize) -> Vec<u8> {
		let mut out: Vec<u8> = Vec::new();
		match self {
			Source::Held { bytes, .. } => {
				let start = offset.min(bytes.len());
				let end = (start + limit).min(bytes.len());
				if out.try_reserve_exact(end - start).is_ok() {
					out.extend_from_slice(&bytes[start..end]);
				}
			}
			Source::Paged { storage, uri, window, window_at, .. } => {
				let covered = offset as u64 >= *window_at && (offset as u64) < *window_at + window.len() as u64;
				if !covered {
					let start = offset as u64 - offset as u64 % WINDOW_BYTES as u64;
					*window = unsafe { read_volume_window(*storage, uri, start, WINDOW_BYTES as u32) }.unwrap_or_default();
					*window_at = start;
				}
				let within = ((offset as u64).saturating_sub(*window_at) as usize).min(window.len());
				let end = (within + limit).min(window.len());
				if out.try_reserve_exact(end - within).is_ok() {
					out.extend_from_slice(&window[within..end]);
				}
				// A RUN THAT REACHES THE END OF A WINDOW CONTINUES INTO THE NEXT, because a line
				// straddling a window boundary is an ordinary line and a viewer that showed half of
				// it would be showing the window rather than the file.
				if end == window.len() && out.len() < limit && (offset + out.len()) < self.len() {
					let rest = self.read(offset + out.len(), limit - out.len());
					if out.try_reserve(rest.len()).is_ok() {
						out.extend_from_slice(&rest);
					}
				}
			}
		}
		out
	}

	// The line that begins at or before `offset`, and where it starts.
	fn line_start_at(&self, offset: usize) -> usize {
		let lines = self.lines();
		match lines.binary_search(&(offset as u64)) {
			Ok(index) => lines[index] as usize,
			Err(0) => 0,
			Err(index) => lines[index - 1] as usize,
		}
	}

	fn line_number_at(&self, offset: usize) -> usize {
		let lines = self.lines();
		match lines.binary_search(&(offset as u64)) {
			Ok(index) => index + 1,
			Err(index) => index.max(1),
		}
	}

	// Where the line beginning at `start` ends, not counting its terminator.
	fn line_end(&self, start: usize) -> usize {
		let lines = self.lines();
		match lines.binary_search(&(start as u64)) {
			Ok(index) if index + 1 < lines.len() => lines[index + 1] as usize - 1,
			_ => self.len(),
		}
	}

	fn next_line(&self, start: usize) -> usize {
		let end = self.line_end(start);
		if end < self.len() { end + 1 } else { start }
	}

	fn previous_line(&self, start: usize) -> usize {
		if start == 0 {
			return 0;
		}
		self.line_start_at(start - 1)
	}

	// Search, over windows that OVERLAP by the pattern's length minus one - the whole difficulty of
	// searching a file you are not holding.
	fn find(&mut self, needle_len: usize, backward: bool, from: usize, matches: impl Fn(&[u8]) -> bool) -> Option<usize> {
		if needle_len == 0 || needle_len > self.len() {
			return None;
		}
		let last = self.len() - needle_len;
		if backward {
			let mut at = from.min(self.len()).checked_sub(1)?.min(last);
			loop {
				if matches(&self.read(at, needle_len)) {
					return Some(at);
				}
				at = at.checked_sub(1)?;
			}
		}
		let mut at = from.min(self.len());
		while at <= last {
			if matches(&self.read(at, needle_len)) {
				return Some(at);
			}
			at += 1;
		}
		None
	}
}

// The offsets every line begins at, for a file that is held.
fn line_index(bytes: &[u8]) -> Vec<u64> {
	let mut lines: Vec<u64> = Vec::new();
	if lines.try_reserve(1).is_err() {
		return lines;
	}
	lines.push(0);
	for (index, byte) in bytes.iter().enumerate() {
		if *byte == b'\n' && lines.try_reserve(1).is_ok() {
			lines.push(index as u64 + 1);
		}
	}
	lines
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

unsafe fn view_file(output: &mut impl TerminalWriter, source: &mut Source, name: &[u8], descriptors: &[SyntaxDescriptor]) -> bool {
	unsafe {
		let input = stdin();
		let mut decoder = InputDecoder::new();
		let head = source.read(0, 32);
		let kind = detect_file_type(name, &head, false);
		// A FILE THAT IS NOT TEXT OPENS AS BYTES. Showing a decoded rendering of an executable is
		// showing a reader something that is not there, and the first thing they would do is switch.
		let mode = if matches!(kind, FileType::Binary | FileType::Archive | FileType::Executable | FileType::Image | FileType::Audio) { Mode::Hex } else { Mode::Text };
		let mut view = View { mode, position: 0, wrap: true, numbers: false, highlight: true, prompt: Prompt::None, entry: Vec::new(), text_needle: Vec::new(), hex_needle: None, hex_last: false, ignore_case: false, whole_word: false, status: Vec::new(), last_match: None };
		view.say(b"t/r/x mode  w wrap  # numbers  h light  / find  \\ hex  i case  W word  g/o/% goto  n/p repeat  q quit");
		let mut redraw = true;
		loop {
			if redraw {
				if !render(output, source, name, &view, kind, descriptors) {
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
							match view.apply(event, source) {
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

	fn apply(&mut self, event: InputEvent, source: &mut Source) -> ViewAction {
		if self.prompt != Prompt::None {
			return self.apply_prompt(event, source);
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
			InputEvent::Key(Key::Byte(b'n')) => return self.repeat(source, false),
			InputEvent::Key(Key::Byte(b'p')) => return self.repeat(source, true),
			InputEvent::Key(Key::ArrowDown) => self.position = self.step(source, self.position, true),
			InputEvent::Key(Key::ArrowUp) => self.position = self.step(source, self.position, false),
			InputEvent::Key(Key::PageDown) => self.position = self.page(source, self.position, true),
			InputEvent::Key(Key::PageUp) => self.position = self.page(source, self.position, false),
			InputEvent::Key(Key::Home) => self.position = 0,
			InputEvent::Key(Key::End) => self.position = self.page(source, source.len(), false),
			InputEvent::Pointer(pointer) if pointer.pressed && pointer.code == 64 => self.position = self.page(source, self.position, false),
			InputEvent::Pointer(pointer) if pointer.pressed && pointer.code == 65 => self.position = self.page(source, self.position, true),
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

	// MOVEMENT IS INDEX ARITHMETIC and reads nothing. That is what the line table buys: paging
	// through a file the viewer is not holding costs a lookup per row rather than a read.
	fn step(&self, source: &Source, from: usize, down: bool) -> usize {
		match self.mode {
			Mode::Hex => {
				if down {
					let len = source.len();
					(from + HEX_BYTES_PER_ROW).min(len - len % HEX_BYTES_PER_ROW)
				} else {
					from.saturating_sub(HEX_BYTES_PER_ROW)
				}
			}
			_ => {
				if down {
					source.next_line(from)
				} else {
					source.previous_line(from)
				}
			}
		}
	}

	fn page(&self, source: &Source, mut from: usize, down: bool) -> usize {
		for _ in 0..VIEW_ROWS {
			let next = self.step(source, from, down);
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

	fn apply_prompt(&mut self, event: InputEvent, source: &mut Source) -> ViewAction {
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
			InputEvent::Key(Key::Enter) => self.commit_prompt(source),
			InputEvent::Key(Key::Byte(byte)) if byte >= 0x20 && byte != 0x7f => {
				if self.entry.len() < MAX_PROMPT_BYTES && self.entry.try_reserve(1).is_ok() {
					self.entry.push(byte);
				}
				ViewAction::Redraw
			}
			_ => ViewAction::None,
		}
	}

	fn commit_prompt(&mut self, source: &mut Source) -> ViewAction {
		let prompt = self.prompt;
		let entry = core::mem::take(&mut self.entry);
		self.prompt = Prompt::None;
		match prompt {
			Prompt::Find => {
				self.text_needle = entry;
				self.hex_last = false;
				self.last_match = None;
				self.repeat(source, false)
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
						self.repeat(source, false)
					}
					Err(error) => {
						self.say(hex_error(error));
						ViewAction::Redraw
					}
				}
			}
			Prompt::GotoLine => match parse_decimal(&entry) {
				Some(line) => {
					// STRAIGHT INTO THE INDEX. Walking line by line would read the file to
					// reach a line the table already knows the offset of.
					let lines = source.lines();
					self.position = lines.get(line.max(1) - 1).copied().unwrap_or(*lines.last().unwrap_or(&0)) as usize;
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
					self.jump(source, offset.min(source.len()));
					ViewAction::Redraw
				}
				None => {
					self.say(b"that is not a byte offset");
					ViewAction::Redraw
				}
			},
			Prompt::GotoPercent => match parse_decimal(&entry) {
				Some(percent) if percent <= 100 => {
					self.jump(source, source.len() / 100 * percent.min(100));
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
	fn jump(&mut self, source: &Source, offset: usize) {
		self.position = match self.mode {
			Mode::Hex => offset - offset % HEX_BYTES_PER_ROW,
			_ => source.line_start_at(offset),
		};
		self.last_match = Some(offset);
		self.say(b"");
	}

	// The next match of whichever query was asked for last. A SEARCH THAT FINDS NOTHING LEAVES THE
	// VIEW WHERE IT WAS and says so - moving to the end on a failed search loses the reader's place,
	// which is the thing they were looking at when they typed the query.
	fn repeat(&mut self, source: &mut Source, backward: bool) -> ViewAction {
		let anchor = self.last_match.unwrap_or(self.position);
		let from = if backward { anchor } else { anchor + 1 };
		let hit = if self.hex_last {
			match self.hex_needle.take() {
				Some(pattern) => {
					let found = source.find(pattern.len(), backward, from, |window| pattern.matches(window));
					self.hex_needle = Some(pattern);
					found
				}
				None => {
					self.say(b"no byte pattern yet - press \\ first");
					return ViewAction::Redraw;
				}
			}
		} else if self.text_needle.is_empty() {
			self.say(b"nothing to find yet - press / first");
			return ViewAction::Redraw;
		} else {
			// THE QUERY IS COMPARED WINDOW BY WINDOW, so a file the viewer is not holding is searched
			// the same way as one it is - one search rather than two.
			let needle = core::mem::take(&mut self.text_needle);
			let ignore_case = self.ignore_case;
			let whole_word = self.whole_word;
			let mut at = from;
			let found = loop {
				let hit = source.find(needle.len(), backward, at, |window| if ignore_case { window.iter().zip(needle.iter()).all(|(a, b)| a.eq_ignore_ascii_case(b)) } else { window == needle.as_slice() });
				let Some(hit) = hit else { break None };
				// WHOLE WORD IS A FILTER AND NOT A STOP: a match rejected for having a letter beside
				// it must not end the search, or `alpha` inside `alphabet` would hide the real word
				// further along. The bytes either side are read where they are, which is one more
				// window lookup rather than a second pass.
				if !whole_word || !word_bounded(source, hit, needle.len()) {
					break Some(hit);
				}
				at = if backward { hit } else { hit + 1 };
			};
			self.text_needle = needle;
			found
		};
		match hit {
			Some(at) => {
				self.jump(source, at);
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

// Whether a match at `at` has a word byte against either end - in which case whole-word rejects it.
// Word bytes are ASCII alphanumeric plus `_`, the same rule the editor's word movement uses.
fn word_bounded(source: &mut Source, at: usize, len: usize) -> bool {
	let before = at.checked_sub(1).map(|index| source.read(index, 1)).and_then(|byte| byte.first().copied());
	let after = source.read(at + len, 1).first().copied();
	before.is_some_and(is_word_byte) || after.is_some_and(is_word_byte)
}

fn is_word_byte(byte: u8) -> bool {
	byte.is_ascii_alphanumeric() || byte == b'_'
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

fn render(output: &mut impl TerminalWriter, source: &mut Source, name: &[u8], view: &View, kind: FileType, descriptors: &[SyntaxDescriptor]) -> bool {
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
	append_decimal(&mut rendered, source.len());
	rendered.push(b'\n');
	append_safe(&mut rendered, &view.status, VIEW_COLUMNS);
	if view.prompt != Prompt::None {
		append_safe(&mut rendered, &view.entry, 64);
		rendered.push(b'_');
	}
	rendered.extend_from_slice(b"\n\n");
	let ok = match view.mode {
		Mode::Hex => render_hex(&mut rendered, source, view),
		_ => render_lines(&mut rendered, source, view, name, descriptors),
	};
	ok && output.write(&rendered)
}

fn render_hex(output: &mut Vec<u8>, source: &mut Source, view: &View) -> bool {
	let mut at = view.position - view.position % HEX_BYTES_PER_ROW;
	let len = source.len();
	for _ in 0..VIEW_ROWS {
		if at >= len {
			break;
		}
		// ONE ROW AT A TIME, sixteen bytes read where they are. A held file slices; a paged one
		// takes them out of the window it already has.
		let row = source.read(at, HEX_BYTES_PER_ROW);
		append_hex_offset(output, at);
		output.extend_from_slice(b"  ");
		let end = at + row.len();
		for index in 0..HEX_BYTES_PER_ROW {
			if index < row.len() {
				append_hex_byte(output, row[index]);
			} else {
				output.extend_from_slice(b"  ");
			}
			output.push(b' ');
			if index == HEX_BYTES_PER_ROW / 2 - 1 {
				output.push(b' ');
			}
		}
		output.push(b'|');
		for &byte in &row {
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

fn render_lines(output: &mut Vec<u8>, source: &mut Source, view: &View, name: &[u8], descriptors: &[SyntaxDescriptor]) -> bool {
	// The descriptor is chosen ONCE per render from the file's name and its first line, and a file
	// that matches nothing is plain text - which is this milestone's rule, and the reason every
	// failure here is an absence rather than an error.
	let first_line = source.read(0, source.line_end(0).min(MAX_LINE_BYTES));
	let selection = if view.highlight && view.mode == Mode::Text { select_descriptor(descriptors, name, &first_line) } else { None };
	let mut state = LineState::new();
	let mut spans = [TokenSpan { start: 0, end: 0, style: 0 }; 64];
	let len = source.len();
	let mut line_start_offset = view.position.min(len);
	let mut number = source.line_number_at(line_start_offset);
	let columns = if view.numbers { VIEW_COLUMNS - 7 } else { VIEW_COLUMNS };
	for _ in 0..VIEW_ROWS {
		if line_start_offset >= len {
			break;
		}
		// ONE LINE, BOUNDED. A file with no newline in it is one enormous line, and a viewer that
		// read it whole to show eighty columns of it would fail on exactly the file somebody opened
		// a viewer for.
		let end = source.line_end(line_start_offset);
		let wanted = (end - line_start_offset).min(MAX_LINE_BYTES);
		let raw = source.read(line_start_offset, wanted);
		let line: &[u8] = raw.strip_suffix(b"\r").unwrap_or(&raw);
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
		let next = source.next_line(line_start_offset);
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

fn trim(mut bytes: &[u8]) -> &[u8] {
	while bytes.first().is_some_and(|byte| byte.is_ascii_whitespace()) {
		bytes = &bytes[1..];
	}
	while bytes.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
		bytes = &bytes[..bytes.len() - 1];
	}
	bytes
}
