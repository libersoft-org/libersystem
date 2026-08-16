// licoedit - governed fullscreen text editor.
//
// It owns the terminal while it runs, edits a bounded byte-preserving buffer, and publishes
// through StorageService's TRANSACTIONAL WRITER rather than writing over the file: nothing is
// visible under the destination's name until `commit`, so an editor that dies half way, a volume
// that runs out of space and a session that is interrupted all leave the old file exactly as it
// was. That is this milestone's own rule - "keep the old file on allocation, no-space, disconnect
// or validation failure" - written as the control flow rather than as an error path.
//
// Binary content opens read-only in `licoview` instead of being decoded lossily here.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use lico::{Chord, FileType, InputDecoder, InputEvent, Key, MouseTracking, TerminalGuard, TerminalOptions, TerminalWriter, TextBuffer, TextBufferError, TextQuery, append_display_line, detect_file_type};
use proto::system::{FileInfo, LaunchContext, WriterMode};
use rt::*;
use storage_proto::path;
use tools::{ConsoleWriter, VolumeSet, read_volume_file};
use volume_client::VolumeClient;
use volume_client_provider as _;

const MAX_EDIT_BYTES: usize = 512 * 1024;
const EDIT_COLUMNS: usize = 78;
const EDIT_ROWS: usize = 18;
// What one Tab inserts when nothing is selected. Spaces rather than a tab byte, and the width is
// stated in one place so the display and the insertion cannot disagree about what a Tab is worth.
const INDENT: &[u8] = b"    ";
const MAX_PROMPT_BYTES: usize = 256;
const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Eq, PartialEq)]
enum EditAction {
	None,
	Redraw,
	Exit,
}

// What a typed line will be used for when Enter is pressed. A prompt is a MODE rather than a
// separate loop, so Ctrl+C, a terminal that goes away and an exit all behave the same in it as out
// of it - an editor with a second input loop is an editor with a second set of exit paths.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Prompt {
	None,
	GotoLine,
	Search,
	ReplaceFind,
	ReplaceWith,
}

struct Editor {
	buffer: TextBuffer,
	clipboard: Vec<u8>,
	prompt: Prompt,
	entry: Vec<u8>,
	needle: Vec<u8>,
	replacement: Vec<u8>,
	status: Vec<u8>,
	// WHAT THE FILE LOOKED LIKE WHEN IT WAS READ. A save compares against it, so a file replaced
	// behind the editor's back is detected at the moment it matters rather than silently written
	// over - which is the difference between losing somebody else's work and being asked about it.
	loaded: Option<FileInfo>,
	// Set when a save was refused because the file had changed, and cleared by anything else. The
	// second Ctrl+S is the explicit decision this milestone's item asks for.
	overwrite_confirmed: bool,
	read_only: bool,
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
		// A FILE THAT IS NOT THERE IS A NEW FILE, which is what an editor is for. Only a file that
		// exists and cannot be read is a refusal.
		let existing = VolumeClient::new(storage).stat(&uri).and_then(|answer| answer.ok());
		let file = match read_volume_file(storage, &uri, MAX_EDIT_BYTES) {
			Ok(file) => file,
			Err(_) if existing.is_none() => Vec::new(),
			Err(_) => {
				eprint(b"licoedit: cannot open file or it exceeds the current 512 kB limit\n");
				exit();
			}
		};
		if matches!(detect_file_type(argument, &file[..file.len().min(32)], false), FileType::Binary | FileType::Archive | FileType::Executable | FileType::Image | FileType::Audio) {
			eprint(b"licoedit: binary content opens read-only in licoview\n");
			exit();
		}
		let buffer = match TextBuffer::from_bytes(&file, MAX_EDIT_BYTES) {
			Ok(buffer) => buffer,
			Err(_) => {
				eprint(b"licoedit: cannot allocate editor buffer\n");
				exit();
			}
		};
		let mut editor = Editor { buffer, clipboard: Vec::new(), prompt: Prompt::None, entry: Vec::new(), needle: Vec::new(), replacement: Vec::new(), status: Vec::new(), loaded: existing, overwrite_confirmed: false, read_only: false };
		editor.say(b"^S save  ^Z/^Y undo  ^C/^X/^V clip  ^G line  ^F find ^N next  ^R replace ^T one  ^K/^D line  F10 exit");
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
			let owns_tty: bool = tty_set_mode(true, false);
			if let Some(mut terminal) = TerminalGuard::enter(&mut output, options) {
				let _ = edit(terminal.writer(), &mut editor, storage, &uri, argument);
			}
			// And back to cooked input and echo, through the same request path.
			if owns_tty {
				tty_set_mode(false, true);
			}
		}
	}
	exit();
}

unsafe fn edit(output: &mut impl TerminalWriter, editor: &mut Editor, storage: u64, uri: &str, name: &[u8]) -> bool {
	unsafe {
		let input = stdin();
		let mut decoder = InputDecoder::new();
		let mut redraw = true;
		loop {
			if redraw {
				if !render(output, editor, name) {
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
							match editor.apply(event, storage, uri) {
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

impl Editor {
	fn say(&mut self, message: &[u8]) {
		self.status.clear();
		if self.status.try_reserve(message.len()).is_ok() {
			self.status.extend_from_slice(message);
		}
	}

	fn apply(&mut self, event: InputEvent, storage: u64, uri: &str) -> EditAction {
		// A PROMPT OWNS THE KEYBOARD while it is open, so a stray binding cannot edit the text
		// under a line the reader is still typing.
		if self.prompt != Prompt::None {
			return self.apply_prompt(event);
		}
		match event {
			InputEvent::Key(Key::Function(10)) | InputEvent::Key(Key::Control(0x11)) => self.quit(),
			InputEvent::Key(Key::Escape) => {
				self.buffer.clear_anchor();
				EditAction::Redraw
			}
			InputEvent::Key(Key::Control(0x13)) => self.save(storage, uri),
			InputEvent::Key(Key::Control(0x1a)) => {
				let outcome = self.buffer_undo();
				self.after_edit(outcome)
			}
			InputEvent::Key(Key::Control(0x19)) => {
				let outcome = self.buffer_redo();
				self.after_edit(outcome)
			}
			InputEvent::Key(Key::Control(0x03)) => self.copy(false),
			InputEvent::Key(Key::Control(0x18)) => self.copy(true),
			InputEvent::Key(Key::Control(0x16)) => self.paste(),
			InputEvent::Key(Key::Control(0x04)) => {
				let outcome = self.buffer.duplicate_line();
				self.report(outcome)
			}
			InputEvent::Key(Key::Control(0x0b)) => {
				self.buffer.delete_line();
				self.dirty_redraw()
			}
			InputEvent::Key(Key::Control(0x07)) => self.open_prompt(Prompt::GotoLine, b"go to line: "),
			InputEvent::Key(Key::Control(0x06)) => self.open_prompt(Prompt::Search, b"find: "),
			InputEvent::Key(Key::Control(0x12)) => self.open_prompt(Prompt::ReplaceFind, b"replace: "),
			InputEvent::Key(Key::Control(0x0e)) => self.find_next(false),
			InputEvent::Key(Key::Control(0x10)) => self.find_next(true),
			InputEvent::Key(Key::Tab) => {
				if self.buffer.selection().is_some() {
					let outcome = self.buffer.indent_block(INDENT).map(|_| ());
					return self.report(outcome);
				}
				let outcome = self.buffer.insert_slice(INDENT);
				self.report(outcome)
			}
			InputEvent::Chord(Chord { key: Key::Tab, shift: true, .. }) => {
				let outcome = self.buffer.unindent_block(INDENT).map(|_| ());
				self.report(outcome)
			}
			// ALT+ARROW MOVES THE LINE, the conventional binding, and it is a chord rather than a
			// control byte because control+arrow is word movement in every editor a reader has used.
			InputEvent::Chord(Chord { key: Key::ArrowUp, alt: true, .. }) => {
				let outcome = self.buffer.move_line_up().map(|_| ());
				self.report(outcome)
			}
			InputEvent::Chord(Chord { key: Key::ArrowDown, alt: true, .. }) => {
				let outcome = self.buffer.move_line_down().map(|_| ());
				self.report(outcome)
			}
			// CONTROL+ARROW IS WORD MOVEMENT, the binding every editor a reader has used puts it on,
			// and it extends a selection when shift is held too - so selecting a word is one chord
			// rather than a count of characters.
			InputEvent::Chord(Chord { key: key @ (Key::ArrowLeft | Key::ArrowRight), control: true, shift, alt: false }) => {
				if shift {
					if self.buffer.selection().is_none() {
						self.buffer.set_anchor();
					}
				} else {
					self.buffer.clear_anchor();
				}
				let moved = if key == Key::ArrowLeft { self.buffer.move_word_left() } else { self.buffer.move_word_right() };
				if moved { EditAction::Redraw } else { EditAction::None }
			}
			// REPLACE THIS ONE, as against `^R`'s replace-all: it acts on the selection, which after a
			// find is exactly the match the reader can see, and then moves to the next.
			InputEvent::Key(Key::Control(0x14)) => self.replace_one(),
			// SHIFT+MOVEMENT EXTENDS A SELECTION. The anchor is set only when there is not one
			// already, so holding shift across several keys grows one selection rather than
			// restarting it at every keystroke.
			InputEvent::Chord(Chord { key, shift: true, .. }) if is_movement(key) => {
				if self.buffer.selection().is_none() {
					self.buffer.set_anchor();
				}
				let anchor = self.buffer.cursor();
				let moved = self.movement(key);
				if !moved && self.buffer.cursor() == anchor {
					return EditAction::None;
				}
				EditAction::Redraw
			}
			InputEvent::Key(key) if is_movement(key) => {
				self.buffer.clear_anchor();
				if self.movement(key) { EditAction::Redraw } else { EditAction::None }
			}
			InputEvent::Key(Key::Backspace) => {
				self.buffer.delete_before();
				self.dirty_redraw()
			}
			InputEvent::Key(Key::Delete) => {
				self.buffer.delete_at();
				self.dirty_redraw()
			}
			InputEvent::Key(Key::Enter) => {
				let outcome = self.buffer.insert(b'\n');
				self.report(outcome)
			}
			InputEvent::Key(Key::Byte(byte)) if byte >= 0x20 && byte != 0x7f => {
				let outcome = self.buffer.insert(byte);
				self.report(outcome)
			}
			_ => EditAction::None,
		}
	}

	fn movement(&mut self, key: Key) -> bool {
		match key {
			Key::ArrowLeft => self.buffer.move_left(),
			Key::ArrowRight => self.buffer.move_right(),
			Key::ArrowUp => self.buffer.move_up(),
			Key::ArrowDown => self.buffer.move_down(),
			Key::Home => self.buffer.move_home(),
			Key::End => self.buffer.move_end(),
			Key::PageUp => self.page(false),
			Key::PageDown => self.page(true),
			_ => false,
		}
	}

	fn page(&mut self, down: bool) -> bool {
		let mut moved = false;
		for _ in 0..EDIT_ROWS {
			let step = if down { self.buffer.move_down() } else { self.buffer.move_up() };
			if !step {
				break;
			}
			moved = true;
		}
		moved
	}

	fn buffer_undo(&mut self) -> Result<(), TextBufferError> {
		if !self.buffer.undo() {
			self.say(b"nothing to undo");
		}
		Ok(())
	}

	fn buffer_redo(&mut self) -> Result<(), TextBufferError> {
		if !self.buffer.redo() {
			self.say(b"nothing to redo");
		}
		Ok(())
	}

	fn after_edit(&mut self, _: Result<(), TextBufferError>) -> EditAction {
		self.overwrite_confirmed = false;
		EditAction::Redraw
	}

	fn dirty_redraw(&mut self) -> EditAction {
		self.overwrite_confirmed = false;
		EditAction::Redraw
	}

	// One place every fallible edit reports from, so a refusal always reaches the reader. An
	// operation that failed silently is one that looks like a key the terminal swallowed.
	fn report(&mut self, outcome: Result<(), TextBufferError>) -> EditAction {
		self.overwrite_confirmed = false;
		match outcome {
			Ok(()) => EditAction::Redraw,
			Err(TextBufferError::TooLarge) => {
				self.say(b"editor buffer limit reached - nothing was changed");
				EditAction::Redraw
			}
			Err(TextBufferError::OutOfMemory) => {
				self.say(b"not enough memory for that edit - nothing was changed");
				EditAction::Redraw
			}
		}
	}

	fn copy(&mut self, cut: bool) -> EditAction {
		let selected = self.buffer.selected_bytes();
		if selected.is_empty() {
			self.say(b"nothing selected - hold shift with the arrows to select");
			return EditAction::Redraw;
		}
		if selected.len() > MAX_CLIPBOARD_BYTES {
			self.say(b"selection is past the clipboard budget");
			return EditAction::Redraw;
		}
		let mut copied: Vec<u8> = Vec::new();
		if copied.try_reserve_exact(selected.len()).is_err() {
			self.say(b"not enough memory to copy that selection");
			return EditAction::Redraw;
		}
		copied.extend_from_slice(selected);
		self.clipboard = copied;
		if cut {
			self.buffer.delete_selection();
			self.overwrite_confirmed = false;
		}
		EditAction::Redraw
	}

	fn paste(&mut self) -> EditAction {
		if self.clipboard.is_empty() {
			self.say(b"the clipboard is empty");
			return EditAction::Redraw;
		}
		// The clipboard is not borrowed across the edit: `insert_slice` takes `&mut self`, and the
		// bytes have to outlive that borrow.
		let mut pending: Vec<u8> = Vec::new();
		if pending.try_reserve_exact(self.clipboard.len()).is_err() {
			self.say(b"not enough memory to paste");
			return EditAction::Redraw;
		}
		pending.extend_from_slice(&self.clipboard);
		let outcome = self.buffer.insert_slice(&pending);
		self.report(outcome)
	}

	fn open_prompt(&mut self, prompt: Prompt, label: &[u8]) -> EditAction {
		self.prompt = prompt;
		self.entry.clear();
		self.say(label);
		EditAction::Redraw
	}

	fn apply_prompt(&mut self, event: InputEvent) -> EditAction {
		match event {
			InputEvent::Key(Key::Escape) => {
				self.prompt = Prompt::None;
				self.entry.clear();
				self.say(b"cancelled");
				EditAction::Redraw
			}
			InputEvent::Key(Key::Backspace) => {
				self.entry.pop();
				EditAction::Redraw
			}
			InputEvent::Key(Key::Enter) => self.commit_prompt(),
			InputEvent::Key(Key::Byte(byte)) if byte >= 0x20 && byte != 0x7f => {
				if self.entry.len() < MAX_PROMPT_BYTES && self.entry.try_reserve(1).is_ok() {
					self.entry.push(byte);
				}
				EditAction::Redraw
			}
			_ => EditAction::None,
		}
	}

	fn commit_prompt(&mut self) -> EditAction {
		let prompt = self.prompt;
		let entry = core::mem::take(&mut self.entry);
		self.prompt = Prompt::None;
		match prompt {
			Prompt::GotoLine => {
				match parse_decimal(&entry) {
					Some(line) => {
						self.buffer.goto_line(line);
						self.say(b"");
					}
					None => self.say(b"that is not a line number"),
				}
				EditAction::Redraw
			}
			Prompt::Search => {
				self.needle = entry;
				self.find_next(false)
			}
			Prompt::ReplaceFind => {
				self.needle = entry;
				self.open_prompt(Prompt::ReplaceWith, b"replace with: ")
			}
			Prompt::ReplaceWith => {
				self.replacement = entry;
				let (needle, replacement) = (core::mem::take(&mut self.needle), core::mem::take(&mut self.replacement));
				let outcome = self.buffer.replace_all(&needle, &replacement, false);
				self.needle = needle;
				self.replacement = replacement;
				match outcome {
					Ok(0) => {
						self.say(b"no occurrences");
						EditAction::Redraw
					}
					Ok(count) => {
						let mut message: Vec<u8> = Vec::new();
						append_decimal_vec(&mut message, count);
						message.extend_from_slice(b" replaced - one undo takes them all back");
						self.status = message;
						self.overwrite_confirmed = false;
						EditAction::Redraw
					}
					Err(error) => self.report(Err(error)),
				}
			}
			Prompt::None => EditAction::Redraw,
		}
	}

	// The next match, selected so a following replace acts on exactly what the reader can see.
	// Searching from the byte AFTER the cursor is what makes repeating advance rather than finding
	// the same place forever.
	fn find_next(&mut self, backward: bool) -> EditAction {
		if self.needle.is_empty() {
			self.say(b"nothing to find yet - ^F first");
			return EditAction::Redraw;
		}
		let from = if backward { self.buffer.cursor() } else { self.buffer.cursor() + 1 };
		let query = TextQuery::new(&self.needle);
		let hit = query.find(self.buffer.bytes(), from, backward).or_else(|| {
			// WRAPPING IS SAID OUT LOUD rather than done silently: a search that quietly starts
			// over makes the reader believe a later occurrence exists.
			query.find(self.buffer.bytes(), if backward { self.buffer.bytes().len() } else { 0 }, backward)
		});
		match hit {
			Some(at) => {
				let len = self.needle.len();
				self.buffer.set_cursor(at);
				self.buffer.set_anchor();
				self.buffer.set_cursor(at + len);
				self.say(b"");
				EditAction::Redraw
			}
			None => {
				self.say(b"not found - the view stays where it was");
				EditAction::Redraw
			}
		}
	}

	// Replace the match that is selected, then select the next one - so holding the key walks the
	// file replacing one at a time, and stopping leaves everything after it alone.
	fn replace_one(&mut self) -> EditAction {
		if self.needle.is_empty() {
			self.say(b"nothing to replace yet - ^R first");
			return EditAction::Redraw;
		}
		let (needle, replacement) = (core::mem::take(&mut self.needle), core::mem::take(&mut self.replacement));
		let replaced = self.buffer.replace_selection(&needle, &replacement, false);
		self.needle = needle;
		self.replacement = replacement;
		if !replaced {
			self.say(b"the selection is not the pattern - ^N to find the next one first");
			return EditAction::Redraw;
		}
		self.overwrite_confirmed = false;
		self.find_next(false)
	}

	fn quit(&mut self) -> EditAction {
		if self.buffer.is_dirty() && !self.overwrite_confirmed {
			self.overwrite_confirmed = true;
			self.say(b"unsaved changes - ^S to save, or F10 again to leave them");
			return EditAction::Redraw;
		}
		EditAction::Exit
	}

	// PUBLISHED, NOT WRITTEN OVER. The transactional writer stages every byte and makes them
	// visible under the destination's name only at `commit`, so every failure below - a read-only
	// volume, a volume with no room, a session that goes away - leaves the previous file intact.
	fn save(&mut self, storage: u64, uri: &str) -> EditAction {
		let mut client = VolumeClient::new(storage);
		// EXTERNAL REPLACEMENT IS DETECTED AT SAVE, which is the moment it matters: the file may
		// have been replaced at any point since it was read, and a size or a timestamp that no
		// longer matches is somebody else's work about to be written over.
		let current = client.stat(uri).and_then(|answer| answer.ok());
		if !self.overwrite_confirmed && replaced(self.loaded.as_ref(), current.as_ref()) {
			self.overwrite_confirmed = true;
			self.say(b"the file changed on the volume since it was opened - ^S again to publish over it");
			return EditAction::Redraw;
		}
		let mut writer = match client.open_writer(uri, WriterMode::Replace) {
			Some(Ok(writer)) => writer,
			_ => {
				self.read_only = true;
				self.say(b"this volume will not take a write - the file is unchanged");
				return EditAction::Redraw;
			}
		};
		// The buffer is handed over in bounded pieces rather than as one message, because the
		// staging accepts what one reply may carry and a whole file is not that.
		let mut published = true;
		for chunk in self.buffer.bytes().chunks(4096) {
			if !matches!(writer.write(chunk), Some(Ok(_))) {
				published = false;
				break;
			}
		}
		if !published {
			let _ = writer.abort();
			unsafe { close(writer.handle()) };
			self.say(b"the write failed - nothing was published and the file is unchanged");
			return EditAction::Redraw;
		}
		if !matches!(writer.commit(), Some(Ok(_))) {
			let _ = writer.abort();
			unsafe { close(writer.handle()) };
			self.say(b"the publication failed - the file is unchanged");
			return EditAction::Redraw;
		}
		unsafe { close(writer.handle()) };
		self.buffer.mark_clean();
		self.overwrite_confirmed = false;
		self.read_only = false;
		self.loaded = client.stat(uri).and_then(|answer| answer.ok());
		self.say(b"saved");
		EditAction::Redraw
	}
}

// Whether the file on the volume is no longer the one that was read. A file that was absent and is
// now there counts, because creating it was somebody else's act too.
fn replaced(loaded: Option<&FileInfo>, current: Option<&FileInfo>) -> bool {
	match (loaded, current) {
		(None, None) => false,
		(None, Some(_)) | (Some(_), None) => true,
		(Some(before), Some(now)) => before.size != now.size || before.mtime != now.mtime,
	}
}

fn is_movement(key: Key) -> bool {
	matches!(key, Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown | Key::Home | Key::End | Key::PageUp | Key::PageDown)
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

fn render(output: &mut impl TerminalWriter, editor: &Editor, name: &[u8]) -> bool {
	let mut rendered = Vec::new();
	if rendered.try_reserve_exact((EDIT_ROWS + 4) * (EDIT_COLUMNS * 4 + 24)).is_err() {
		return false;
	}
	rendered.extend_from_slice(b"\x1b[H\x1b[2J\x1b[1mlicoedit\x1b[0m ");
	append_safe(&mut rendered, name, 40);
	rendered.extend_from_slice(if editor.buffer.is_dirty() { b"  modified" } else { b"  clean" });
	if editor.read_only {
		rendered.extend_from_slice(b"  read-only");
	}
	rendered.extend_from_slice(b"  line ");
	append_decimal(&mut rendered, editor.buffer.line_number());
	rendered.push(b'/');
	append_decimal(&mut rendered, editor.buffer.line_count());
	rendered.push(b'\n');
	append_safe(&mut rendered, &editor.status, EDIT_COLUMNS);
	if editor.prompt != Prompt::None {
		append_safe(&mut rendered, &editor.entry, 64);
		rendered.extend_from_slice(b"_");
	}
	rendered.extend_from_slice(b"\n\n");

	let cursor_line = editor.buffer.line_start_at(editor.buffer.cursor());
	let mut start = cursor_line;
	for _ in 0..EDIT_ROWS / 2 {
		let previous = previous_line_start(&editor.buffer, start);
		if previous == start {
			break;
		}
		start = previous;
	}
	let mut line_number = 1 + editor.buffer.bytes()[..start].iter().filter(|&&byte| byte == b'\n').count();
	let selection = editor.buffer.selection();
	for _ in 0..EDIT_ROWS {
		if start > editor.buffer.bytes().len() {
			break;
		}
		rendered.push(if start == cursor_line { b'>' } else { b' ' });
		append_decimal(&mut rendered, line_number);
		rendered.extend_from_slice(b" ");
		// A SELECTED LINE IS MARKED rather than reverse-videoed by column: the display is a
		// character grid over a byte buffer, and a per-byte highlight would have to agree with the
		// tab expansion and the UTF-8 decoding to land in the right place.
		let line_end = editor.buffer.next_line_start(start);
		let touched = selection.is_some_and(|(from, to)| from < line_end.max(start + 1) && to >= start);
		rendered.push(if touched { b'*' } else { b' ' });
		let line = editor.buffer.line_at(start);
		let line = line.strip_suffix(b"\r").unwrap_or(line);
		if append_display_line(line, EDIT_COLUMNS, 8, &mut rendered).is_err() {
			return false;
		}
		rendered.push(b'\n');
		let next = editor.buffer.next_line_start(start);
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

fn append_decimal_vec(output: &mut Vec<u8>, value: usize) {
	append_decimal(output, value);
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
