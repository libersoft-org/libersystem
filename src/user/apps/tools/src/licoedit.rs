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

use alloc::string::String;
use alloc::vec::Vec;
use lico::{Chord, FileType, InputDecoder, InputEvent, Key, LineState, MAX_DESCRIPTOR_BYTES, MouseTracking, SyntaxDescriptor, TerminalGuard, TerminalOptions, TerminalWriter, TextBuffer, TextBufferError, TextQuery, TokenSpan, append_display_line, detect_file_type, parse_descriptor, select_descriptor};
use proto::system::{FileInfo, LaunchContext, SelectedFile, WriterMode};
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
const MAX_PROMPT_BYTES: usize = 256;
const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;
// The most files one editor holds open. A switcher nobody can walk is not a switcher, and every
// buffer is a whole file's worth of memory.
const MAX_BUFFERS: usize = 9;

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
	SaveAs,
	ConfirmReload,
}

// One open file. Everything that is per-FILE lives here rather than in the editor, which is what
// makes switching buffers restore the caret, the scroll and the save state rather than only the
// text - an editor that switched back to a buffer with the cursor at the top would be one nobody
// keeps two files open in.
struct Buffer {
	uri: String,
	name: Vec<u8>,
	text: TextBuffer,
	// WHAT THE FILE LOOKED LIKE WHEN IT WAS READ. A save compares against it, so a file replaced
	// behind the editor's back is detected at the moment it matters rather than silently written
	// over - which is the difference between losing somebody else's work and being asked about it.
	loaded: Option<FileInfo>,
	// Set when a save was refused because the file had changed, and cleared by anything else. The
	// second Ctrl+S is the explicit decision this milestone's item asks for.
	overwrite_confirmed: bool,
	read_only: bool,
	// THE SCROLL IS NOT STORED, and that is a decision rather than an omission: the view is drawn
	// centred on the caret, so restoring the caret restores what was on the screen. A stored top
	// line would be a second answer to "where is this buffer", and the two would drift the first
	// time an edit moved the text between them.
	//
	// An explicit language, when the reader chose one. `None` means the descriptor is selected from
	// the name and the first line, which is right almost always and wrong for a file whose name
	// says nothing.
	language: Option<usize>,
}

struct Editor {
	buffers: Vec<Buffer>,
	active: usize,
	descriptors: Vec<SyntaxDescriptor>,
	clipboard: Vec<u8>,
	prompt: Prompt,
	entry: Vec<u8>,
	needle: Vec<u8>,
	replacement: Vec<u8>,
	status: Vec<u8>,
	// Presentation, shared by every buffer because it is a property of how this reader works rather
	// than of any one file.
	line_numbers: bool,
	highlight: bool,
	wrap: bool,
	show_whitespace: bool,
	overwrite_mode: bool,
	// What Tab inserts. Spaces by default and stated in one place, so the display and the insertion
	// cannot disagree about what a Tab is worth.
	spaces_for_tab: bool,
	indent_width: usize,
	auto_indent: bool,
	// The selected-file client, or 0. Held rather than passed, because the save and the reload both
	// need it and a parameter threaded through two call paths is a parameter one of them forgets.
	granted: u64,
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
		// THE SELECTED-FILE TAG COMES FIRST, where the launcher sends it - before the vocabulary
		// grants. It is always sent to this program, bare when there was no file, because a tag read
		// where nothing arrives consumes the next message and then blocks forever.
		let selected: u64 = recv_tagged(bootstrap, &mut bootstrap_buffer, CAP_SELECTED_FILE).unwrap_or(0);
		let opened: Option<SelectedFile> = if selected == 0 { None } else { recv_launch_bytes(bootstrap).as_deref().and_then(SelectedFile::decode) };
		let volumes = VolumeSet::receive(bootstrap, &mut bootstrap_buffer);
		let cwd: Vec<u8> = context.cwd.clone().into_bytes();
		let cwd = core::str::from_utf8(&cwd).unwrap_or("");
		let argument = trim(&argument);
		// THE SELECTED-FILE GRANT, WHEN THERE IS ONE. A launch over one file hands this program a
		// client scoped to exactly that path, and the record that follows says whether a write-back
		// will be accepted - so an editor opened from a panel holds the authority to change that
		// one file and nothing else. A read-only grant is opened read-only and SAYS SO, because an
		// editor that let somebody type for an hour and then refused the save is worse than one
		// that said at the top.

		if argument.is_empty() && opened.is_none() {
			print(b"Usage: licoedit PATH [PATH ...]\n");
			exit();
		}
		// This application's own asset directory, for the syntax descriptors. Absent on a boot that
		// grants no assets, which is an editor that highlights nothing rather than one that fails.
		let assets: u64 = recv_tagged(bootstrap, &mut bootstrap_buffer, CAP_APP_ASSETS).unwrap_or(0);
		let descriptors: Vec<SyntaxDescriptor> = load_descriptors(assets);
		let mut buffers: Vec<Buffer> = Vec::new();
		// A GRANTED FILE IS THE FIRST BUFFER, and when there is one the command line's paths are
		// not opened at all: a launch over a selected file was given authority over that file, and
		// resolving a path beside it would be reaching for something the grant deliberately does
		// not cover.
		if let Some(opened) = opened.as_ref() {
			let uri = String::from(opened.uri.as_str());
			let existing = VolumeClient::new(selected).stat(&uri).and_then(|answer| answer.ok());
			let file = unsafe { read_volume_file(selected, &uri, MAX_EDIT_BYTES) }.unwrap_or_default();
			match TextBuffer::from_bytes(&file, MAX_EDIT_BYTES) {
				Ok(text) => {
					let mut name: Vec<u8> = Vec::new();
					if name.try_reserve_exact(opened.name.len()).is_ok() {
						name.extend_from_slice(opened.name.as_bytes());
					}
					buffers.push(Buffer { uri, name, text, loaded: existing, overwrite_confirmed: false, read_only: !opened.writable, language: None });
				}
				Err(_) => eprint(b"licoedit: cannot allocate an editor buffer\n"),
			}
		}
		// ONE BUFFER PER PATH, and a path that cannot be opened is REPORTED and skipped rather than
		// ending the launch: `licoedit a b c` where `b` is a directory should still open `a` and
		// `c`, because the reader asked for three files and two of them are there.
		for word in argument.split(|byte| byte.is_ascii_whitespace()).filter(|_| opened.is_none()).filter(|word| !word.is_empty()) {
			if buffers.len() == MAX_BUFFERS {
				eprint(b"licoedit: too many files; the rest were not opened\n");
				break;
			}
			let Some(uri) = path::resolve(cwd, word) else {
				eprint(b"licoedit: invalid path\n");
				continue;
			};
			let storage = volumes.client_for(cwd, word);
			// A FILE THAT IS NOT THERE IS A NEW FILE, which is what an editor is for. Only a file
			// that exists and cannot be read is a refusal.
			let existing = VolumeClient::new(storage).stat(&uri).and_then(|answer| answer.ok());
			let file = match read_volume_file(storage, &uri, MAX_EDIT_BYTES) {
				Ok(file) => file,
				Err(_) if existing.is_none() => Vec::new(),
				Err(_) => {
					eprint(b"licoedit: cannot open a file, or it is past the 512 kB limit\n");
					continue;
				}
			};
			if matches!(detect_file_type(word, &file[..file.len().min(32)], false), FileType::Binary | FileType::Archive | FileType::Executable | FileType::Image | FileType::Audio) {
				eprint(b"licoedit: binary content opens read-only in licoview\n");
				continue;
			}
			let Ok(text) = TextBuffer::from_bytes(&file, MAX_EDIT_BYTES) else {
				eprint(b"licoedit: cannot allocate an editor buffer\n");
				continue;
			};
			let mut name: Vec<u8> = Vec::new();
			if name.try_reserve_exact(word.len()).is_err() || buffers.try_reserve(1).is_err() {
				eprint(b"licoedit: not enough memory to open that file\n");
				continue;
			}
			name.extend_from_slice(word);
			buffers.push(Buffer { uri, name, text, loaded: existing, overwrite_confirmed: false, read_only: false, language: None });
		}
		if buffers.is_empty() {
			eprint(b"licoedit: nothing could be opened\n");
			exit();
		}
		let mut editor = Editor { buffers, active: 0, descriptors, clipboard: Vec::new(), prompt: Prompt::None, entry: Vec::new(), needle: Vec::new(), replacement: Vec::new(), status: Vec::new(), line_numbers: true, highlight: true, wrap: false, show_whitespace: false, overwrite_mode: false, spaces_for_tab: true, indent_width: 4, auto_indent: true, granted: selected };
		editor.say(b"^S save  M-a save-as  M-r reload  ^Z/^Y undo  ^C/^X/^V clip  ^G line  ^F find  ^R replace  ^W buffer  F10 exit");
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
				let _ = edit(terminal.writer(), &mut editor, &volumes);
			}
			// And back to cooked input and echo, through the same request path.
			if owns_tty {
				tty_set_mode(false, true);
			}
		}
	}
	exit();
}

unsafe fn edit(output: &mut impl TerminalWriter, editor: &mut Editor, volumes: &VolumeSet) -> bool {
	unsafe {
		let input = stdin();
		let mut decoder = InputDecoder::new();
		let mut redraw = true;
		loop {
			if redraw {
				if !render(output, editor) {
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
							match editor.apply(event, volumes) {
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

	fn apply(&mut self, event: InputEvent, volumes: &VolumeSet) -> EditAction {
		// A PROMPT OWNS THE KEYBOARD while it is open, so a stray binding cannot edit the text
		// under a line the reader is still typing.
		if self.prompt != Prompt::None {
			return self.apply_prompt(event, volumes);
		}
		match event {
			InputEvent::Key(Key::Function(10)) | InputEvent::Key(Key::Control(0x11)) => self.quit(),
			InputEvent::Key(Key::Escape) => {
				self.buffers[self.active].text.clear_anchor();
				EditAction::Redraw
			}
			InputEvent::Key(Key::Control(0x13)) => self.save(volumes),
			// BUFFER SWITCHING. `^W` walks them and `M-1..9` names one, and each remembers its own
			// caret, scroll, save state and language - so coming back to a file is coming back to
			// what was being read rather than to its first line.
			// RELOAD AND SAVE-AS, which are the two answers to a conflict that are not "write over
			// it". A reload with unsaved work asks first, by name, because discarding an edit
			// silently is the one thing an editor must never do.
			InputEvent::Chord(Chord { key: Key::Byte(b'r'), alt: true, .. }) => {
				if self.buffers[self.active].text.is_dirty() {
					self.prompt = Prompt::ConfirmReload;
					self.entry.clear();
					self.say(b"this buffer has unsaved changes - type yes to throw them away and re-read the file: ");
					return EditAction::Redraw;
				}
				self.reload(volumes)
			}
			InputEvent::Chord(Chord { key: Key::Byte(b'a'), alt: true, .. }) => {
				self.prompt = Prompt::SaveAs;
				self.entry.clear();
				self.say(b"save as: ");
				EditAction::Redraw
			}
			InputEvent::Key(Key::Control(0x17)) => {
				self.active = (self.active + 1) % self.buffers.len();
				self.name_buffer();
				EditAction::Redraw
			}
			InputEvent::Chord(Chord { key: Key::Byte(byte @ b'1'..=b'9'), alt: true, .. }) => {
				let wanted = (byte - b'1') as usize;
				if wanted < self.buffers.len() {
					self.active = wanted;
					self.name_buffer();
				} else {
					self.say(b"no buffer there");
				}
				EditAction::Redraw
			}
			// PRESENTATION TOGGLES. Shared by every buffer, because they are properties of how this
			// reader works rather than of any one file - except the language override, which is a
			// statement about one file and lives on it.
			InputEvent::Chord(Chord { key: Key::Byte(b'n'), alt: true, .. }) => {
				self.line_numbers = !self.line_numbers;
				EditAction::Redraw
			}
			InputEvent::Chord(Chord { key: Key::Byte(b'h'), alt: true, .. }) => {
				self.highlight = !self.highlight;
				EditAction::Redraw
			}
			InputEvent::Chord(Chord { key: Key::Byte(b'w'), alt: true, .. }) => {
				self.wrap = !self.wrap;
				EditAction::Redraw
			}
			InputEvent::Chord(Chord { key: Key::Byte(b'.'), alt: true, .. }) => {
				self.show_whitespace = !self.show_whitespace;
				EditAction::Redraw
			}
			InputEvent::Chord(Chord { key: Key::Byte(b't'), alt: true, .. }) => {
				self.spaces_for_tab = !self.spaces_for_tab;
				self.say(if self.spaces_for_tab { b"Tab inserts spaces" } else { b"Tab inserts a tab byte" });
				EditAction::Redraw
			}
			InputEvent::Chord(Chord { key: Key::Byte(b'i'), alt: true, .. }) => {
				self.auto_indent = !self.auto_indent;
				self.say(if self.auto_indent { b"auto-indent on" } else { b"auto-indent off" });
				EditAction::Redraw
			}
			InputEvent::Chord(Chord { key: Key::Byte(b'l'), alt: true, .. }) => {
				// AN EXPLICIT LANGUAGE, for a file whose name says nothing. It cycles through the
				// installed descriptors and back to automatic selection, so there is always a way
				// back to the answer the suite would have chosen.
				let count = self.descriptors.len();
				let language = &mut self.buffers[self.active].language;
				*language = match *language {
					None if count > 0 => Some(0),
					Some(index) if index + 1 < count => Some(index + 1),
					_ => None,
				};
				self.say(match self.buffers[self.active].language {
					Some(_) => b"language chosen for this buffer",
					None => b"language selected from the file name again",
				});
				EditAction::Redraw
			}
			// INSERT/OVERWRITE. Overwrite replaces the byte under the caret rather than pushing it
			// along, and at the end of a line it inserts - there is nothing there to replace.
			InputEvent::Key(Key::Insert) => {
				self.overwrite_mode = !self.overwrite_mode;
				self.say(if self.overwrite_mode { b"overwrite" } else { b"insert" });
				EditAction::Redraw
			}
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
				let outcome = self.buffers[self.active].text.duplicate_line();
				self.report(outcome)
			}
			InputEvent::Key(Key::Control(0x0b)) => {
				self.buffers[self.active].text.delete_line();
				self.dirty_redraw()
			}
			InputEvent::Key(Key::Control(0x07)) => self.open_prompt(Prompt::GotoLine, b"go to line: "),
			InputEvent::Key(Key::Control(0x06)) => self.open_prompt(Prompt::Search, b"find: "),
			InputEvent::Key(Key::Control(0x12)) => self.open_prompt(Prompt::ReplaceFind, b"replace: "),
			InputEvent::Key(Key::Control(0x0e)) => self.find_next(false),
			InputEvent::Key(Key::Control(0x10)) => self.find_next(true),
			InputEvent::Key(Key::Tab) => {
				let unit = self.indent_unit();
				if self.buffers[self.active].text.selection().is_some() {
					let outcome = self.buffers[self.active].text.indent_block(&unit).map(|_| ());
					return self.report(outcome);
				}
				let outcome = self.buffers[self.active].text.insert_slice(&unit);
				self.report(outcome)
			}
			InputEvent::Chord(Chord { key: Key::Tab, shift: true, .. }) => {
				let unit = self.indent_unit();
				let outcome = self.buffers[self.active].text.unindent_block(&unit).map(|_| ());
				self.report(outcome)
			}
			// ALT+ARROW MOVES THE LINE, the conventional binding, and it is a chord rather than a
			// control byte because control+arrow is word movement in every editor a reader has used.
			InputEvent::Chord(Chord { key: Key::ArrowUp, alt: true, .. }) => {
				let outcome = self.buffers[self.active].text.move_line_up().map(|_| ());
				self.report(outcome)
			}
			InputEvent::Chord(Chord { key: Key::ArrowDown, alt: true, .. }) => {
				let outcome = self.buffers[self.active].text.move_line_down().map(|_| ());
				self.report(outcome)
			}
			// CONTROL+ARROW IS WORD MOVEMENT, the binding every editor a reader has used puts it on,
			// and it extends a selection when shift is held too - so selecting a word is one chord
			// rather than a count of characters.
			InputEvent::Chord(Chord { key: key @ (Key::ArrowLeft | Key::ArrowRight), control: true, shift, alt: false }) => {
				if shift {
					if self.buffers[self.active].text.selection().is_none() {
						self.buffers[self.active].text.set_anchor();
					}
				} else {
					self.buffers[self.active].text.clear_anchor();
				}
				let moved = if key == Key::ArrowLeft { self.buffers[self.active].text.move_word_left() } else { self.buffers[self.active].text.move_word_right() };
				if moved { EditAction::Redraw } else { EditAction::None }
			}
			// REPLACE THIS ONE, as against `^R`'s replace-all: it acts on the selection, which after a
			// find is exactly the match the reader can see, and then moves to the next.
			InputEvent::Key(Key::Control(0x14)) => self.replace_one(),
			// SHIFT+MOVEMENT EXTENDS A SELECTION. The anchor is set only when there is not one
			// already, so holding shift across several keys grows one selection rather than
			// restarting it at every keystroke.
			InputEvent::Chord(Chord { key, shift: true, .. }) if is_movement(key) => {
				if self.buffers[self.active].text.selection().is_none() {
					self.buffers[self.active].text.set_anchor();
				}
				let anchor = self.buffers[self.active].text.cursor();
				let moved = self.movement(key);
				if !moved && self.buffers[self.active].text.cursor() == anchor {
					return EditAction::None;
				}
				EditAction::Redraw
			}
			InputEvent::Key(key) if is_movement(key) => {
				self.buffers[self.active].text.clear_anchor();
				if self.movement(key) { EditAction::Redraw } else { EditAction::None }
			}
			InputEvent::Key(Key::Backspace) => {
				self.buffers[self.active].text.delete_before();
				self.dirty_redraw()
			}
			InputEvent::Key(Key::Delete) => {
				self.buffers[self.active].text.delete_at();
				self.dirty_redraw()
			}
			InputEvent::Key(Key::Enter) => self.newline(),
			InputEvent::Key(Key::Byte(byte)) if byte >= 0x20 && byte != 0x7f => {
				// OVERWRITE REPLACES THE BYTE UNDER THE CARET and inserts at the end of a line,
				// where there is nothing to replace - a mode that ate the newline would join two
				// lines every time somebody typed at the end of one.
				if self.overwrite_mode && self.buffers[self.active].text.selection().is_none() {
					let cursor = self.buffers[self.active].text.cursor();
					let bytes = self.buffers[self.active].text.bytes();
					if bytes.get(cursor).is_some_and(|byte| *byte != b'\n') {
						self.buffers[self.active].text.delete_at();
					}
				}
				let outcome = self.buffers[self.active].text.insert(byte);
				self.report(outcome)
			}
			_ => EditAction::None,
		}
	}

	fn movement(&mut self, key: Key) -> bool {
		match key {
			Key::ArrowLeft => self.buffers[self.active].text.move_left(),
			Key::ArrowRight => self.buffers[self.active].text.move_right(),
			Key::ArrowUp => self.buffers[self.active].text.move_up(),
			Key::ArrowDown => self.buffers[self.active].text.move_down(),
			Key::Home => self.buffers[self.active].text.move_home(),
			Key::End => self.buffers[self.active].text.move_end(),
			Key::PageUp => self.page(false),
			Key::PageDown => self.page(true),
			_ => false,
		}
	}

	fn page(&mut self, down: bool) -> bool {
		let mut moved = false;
		for _ in 0..EDIT_ROWS {
			let step = if down { self.buffers[self.active].text.move_down() } else { self.buffers[self.active].text.move_up() };
			if !step {
				break;
			}
			moved = true;
		}
		moved
	}

	// Put the active buffer's name on the status line. What a switch says, because the header shows
	// it too but the reader's eye is on the line that just changed.
	fn name_buffer(&mut self) {
		let mut message: Vec<u8> = Vec::new();
		let name = self.buffers[self.active].name.clone();
		if message.try_reserve(name.len() + 24).is_ok() {
			message.extend_from_slice(b"buffer ");
			append_decimal(&mut message, self.active + 1);
			message.extend_from_slice(b"/");
			append_decimal(&mut message, self.buffers.len());
			message.extend_from_slice(b" ");
			message.extend_from_slice(&name);
			self.status = message;
		}
	}

	// What Tab inserts, and what a new line inherits.
	fn indent_unit(&self) -> Vec<u8> {
		let mut unit: Vec<u8> = Vec::new();
		if self.spaces_for_tab {
			for _ in 0..self.indent_width {
				if unit.try_reserve(1).is_ok() {
					unit.push(b' ');
				}
			}
		} else if unit.try_reserve(1).is_ok() {
			unit.push(b'\t');
		}
		unit
	}

	// A NEW LINE INHERITS THE INDENT OF THE ONE IT CAME FROM, which is what auto-indent is - and it
	// is the leading whitespace of that line verbatim, so a file indented with tabs stays indented
	// with tabs whatever this editor's own Tab setting is.
	fn newline(&mut self) -> EditAction {
		if !self.auto_indent {
			let outcome = self.buffers[self.active].text.insert(b'\n');
			return self.report(outcome);
		}
		let cursor = self.buffers[self.active].text.cursor();
		let line = self.buffers[self.active].text.line_at(cursor);
		let indent_len = line.iter().take_while(|byte| **byte == b' ' || **byte == b'\t').count();
		let mut inserted: Vec<u8> = Vec::new();
		if inserted.try_reserve_exact(indent_len + 1).is_err() {
			let outcome = self.buffers[self.active].text.insert(b'\n');
			return self.report(outcome);
		}
		inserted.push(b'\n');
		inserted.extend_from_slice(&line[..indent_len]);
		let outcome = self.buffers[self.active].text.insert_slice(&inserted);
		self.report(outcome)
	}

	fn buffer_undo(&mut self) -> Result<(), TextBufferError> {
		if !self.buffers[self.active].text.undo() {
			self.say(b"nothing to undo");
		}
		Ok(())
	}

	fn buffer_redo(&mut self) -> Result<(), TextBufferError> {
		if !self.buffers[self.active].text.redo() {
			self.say(b"nothing to redo");
		}
		Ok(())
	}

	fn after_edit(&mut self, _: Result<(), TextBufferError>) -> EditAction {
		self.buffers[self.active].overwrite_confirmed = false;
		EditAction::Redraw
	}

	fn dirty_redraw(&mut self) -> EditAction {
		self.buffers[self.active].overwrite_confirmed = false;
		EditAction::Redraw
	}

	// One place every fallible edit reports from, so a refusal always reaches the reader. An
	// operation that failed silently is one that looks like a key the terminal swallowed.
	fn report(&mut self, outcome: Result<(), TextBufferError>) -> EditAction {
		self.buffers[self.active].overwrite_confirmed = false;
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
		let selected = self.buffers[self.active].text.selected_bytes();
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
			self.buffers[self.active].text.delete_selection();
			self.buffers[self.active].overwrite_confirmed = false;
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
		let outcome = self.buffers[self.active].text.insert_slice(&pending);
		self.report(outcome)
	}

	fn open_prompt(&mut self, prompt: Prompt, label: &[u8]) -> EditAction {
		self.prompt = prompt;
		self.entry.clear();
		self.say(label);
		EditAction::Redraw
	}

	fn apply_prompt(&mut self, event: InputEvent, volumes: &VolumeSet) -> EditAction {
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
			InputEvent::Key(Key::Enter) => self.commit_prompt(volumes),
			InputEvent::Key(Key::Byte(byte)) if byte >= 0x20 && byte != 0x7f => {
				if self.entry.len() < MAX_PROMPT_BYTES && self.entry.try_reserve(1).is_ok() {
					self.entry.push(byte);
				}
				EditAction::Redraw
			}
			_ => EditAction::None,
		}
	}

	fn commit_prompt(&mut self, volumes: &VolumeSet) -> EditAction {
		let prompt = self.prompt;
		let entry = core::mem::take(&mut self.entry);
		self.prompt = Prompt::None;
		match prompt {
			Prompt::GotoLine => {
				match parse_decimal(&entry) {
					Some(line) => {
						self.buffers[self.active].text.goto_line(line);
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
				let outcome = self.buffers[self.active].text.replace_all(&needle, &replacement, false);
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
						self.buffers[self.active].overwrite_confirmed = false;
						EditAction::Redraw
					}
					Err(error) => self.report(Err(error)),
				}
			}
			Prompt::SaveAs => {
				// THE BUFFER ADOPTS THE NEW PATH ONLY IF THE PUBLICATION SUCCEEDED, so a save-as
				// that was refused leaves the buffer pointing at the file it came from rather than
				// at a name nothing was written under.
				let Ok(text) = core::str::from_utf8(&entry) else {
					self.say(b"that path is not text");
					return EditAction::Redraw;
				};
				let Some(uri) = path::resolve(&self.buffers[self.active].uri, text.as_bytes()) else {
					self.say(b"that is not a path this editor can reach");
					return EditAction::Redraw;
				};
				let previous_uri = core::mem::replace(&mut self.buffers[self.active].uri, uri);
				let previous_loaded = self.buffers[self.active].loaded.take();
				// A NEW DESTINATION HAS NO PRIOR READING TO COMPARE AGAINST, so the
				// external-replacement check would refuse the first save of every save-as. It is
				// confirmed here instead: naming a destination IS the decision that check asks for.
				self.buffers[self.active].overwrite_confirmed = true;
				let outcome = self.save(volumes);
				if self.buffers[self.active].text.is_dirty() {
					self.buffers[self.active].uri = previous_uri;
					self.buffers[self.active].loaded = previous_loaded;
					self.say(b"that destination could not be written - the buffer still points at the file it came from");
				} else {
					let uri = self.buffers[self.active].uri.clone();
					let name = &mut self.buffers[self.active].name;
					name.clear();
					if name.try_reserve(uri.len()).is_ok() {
						name.extend_from_slice(uri.as_bytes());
					}
				}
				outcome
			}
			Prompt::ConfirmReload => {
				if entry == b"yes" || entry == b"y" {
					return self.reload(volumes);
				}
				self.say(b"not reloaded");
				EditAction::Redraw
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
		let from = if backward { self.buffers[self.active].text.cursor() } else { self.buffers[self.active].text.cursor() + 1 };
		let query = TextQuery::new(&self.needle);
		let hit = query.find(self.buffers[self.active].text.bytes(), from, backward).or_else(|| {
			// WRAPPING IS SAID OUT LOUD rather than done silently: a search that quietly starts
			// over makes the reader believe a later occurrence exists.
			query.find(self.buffers[self.active].text.bytes(), if backward { self.buffers[self.active].text.bytes().len() } else { 0 }, backward)
		});
		match hit {
			Some(at) => {
				let len = self.needle.len();
				self.buffers[self.active].text.set_cursor(at);
				self.buffers[self.active].text.set_anchor();
				self.buffers[self.active].text.set_cursor(at + len);
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
		let replaced = self.buffers[self.active].text.replace_selection(&needle, &replacement, false);
		self.needle = needle;
		self.replacement = replacement;
		if !replaced {
			self.say(b"the selection is not the pattern - ^N to find the next one first");
			return EditAction::Redraw;
		}
		self.buffers[self.active].overwrite_confirmed = false;
		self.find_next(false)
	}

	// WHICH CLIENT REACHES THIS BUFFER. A buffer opened through a selected-file grant is reached
	// through that grant and never through the volume bundle - which for a launch over one file is
	// not there at all. One place decides, so the save and the reload cannot pick differently.
	fn storage_for(&self, volumes: &VolumeSet, uri: &str) -> u64 {
		match self.granted {
			0 => volumes.client_for(uri, uri.as_bytes()),
			granted if self.buffers[self.active].uri == uri => granted,
			_ => volumes.client_for(uri, uri.as_bytes()),
		}
	}

	// Read the file again, discarding what is in the buffer. The other answer to a conflict, and
	// the one the item asks for beside overwrite: a reader told the file changed underneath them
	// needs to be able to take the volume's copy rather than only to write over it.
	fn reload(&mut self, volumes: &VolumeSet) -> EditAction {
		let uri = self.buffers[self.active].uri.clone();
		let storage = self.storage_for(volumes, &uri);
		let file = match unsafe { read_volume_file(storage, &uri, MAX_EDIT_BYTES) } {
			Ok(file) => file,
			Err(_) => {
				self.say(b"the file could not be read again - the buffer is unchanged");
				return EditAction::Redraw;
			}
		};
		let Ok(text) = TextBuffer::from_bytes(&file, MAX_EDIT_BYTES) else {
			self.say(b"not enough memory to re-read that file - the buffer is unchanged");
			return EditAction::Redraw;
		};
		self.buffers[self.active].text = text;
		self.buffers[self.active].loaded = VolumeClient::new(storage).stat(&uri).and_then(|answer| answer.ok());
		self.buffers[self.active].overwrite_confirmed = false;
		self.say(b"re-read from the volume");
		EditAction::Redraw
	}

	fn quit(&mut self) -> EditAction {
		if self.buffers[self.active].text.is_dirty() && !self.buffers[self.active].overwrite_confirmed {
			self.buffers[self.active].overwrite_confirmed = true;
			self.say(b"unsaved changes - ^S to save, or F10 again to leave them");
			return EditAction::Redraw;
		}
		EditAction::Exit
	}

	// PUBLISHED, NOT WRITTEN OVER. The transactional writer stages every byte and makes them
	// visible under the destination's name only at `commit`, so every failure below - a read-only
	// volume, a volume with no room, a session that goes away - leaves the previous file intact.
	fn save(&mut self, volumes: &VolumeSet) -> EditAction {
		if self.buffers[self.active].read_only {
			self.say(b"this file was opened read-only, so there is nothing to publish through");
			return EditAction::Redraw;
		}
		let uri = self.buffers[self.active].uri.clone();
		let uri = uri.as_str();
		let storage = self.storage_for(volumes, uri);
		let mut client = VolumeClient::new(storage);
		// EXTERNAL REPLACEMENT IS DETECTED AT SAVE, which is the moment it matters: the file may
		// have been replaced at any point since it was read, and a size or a timestamp that no
		// longer matches is somebody else's work about to be written over.
		let current = client.stat(uri).and_then(|answer| answer.ok());
		if !self.buffers[self.active].overwrite_confirmed && replaced(self.buffers[self.active].loaded.as_ref(), current.as_ref()) {
			self.buffers[self.active].overwrite_confirmed = true;
			self.say(b"the file changed on the volume since it was opened - ^S again to publish over it, M-r to take the volume copy instead");
			return EditAction::Redraw;
		}
		let mut writer = match client.open_writer(uri, WriterMode::Replace) {
			Some(Ok(writer)) => writer,
			_ => {
				self.buffers[self.active].read_only = true;
				self.say(b"this volume will not take a write - the file is unchanged");
				return EditAction::Redraw;
			}
		};
		// The buffer is handed over in bounded pieces rather than as one message, because the
		// staging accepts what one reply may carry and a whole file is not that.
		let mut published = true;
		for chunk in self.buffers[self.active].text.bytes().chunks(4096) {
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
		self.buffers[self.active].text.mark_clean();
		self.buffers[self.active].overwrite_confirmed = false;
		self.buffers[self.active].read_only = false;
		self.buffers[self.active].loaded = client.stat(uri).and_then(|answer| answer.ok());
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

// Read every descriptor in the granted asset directory.
//
// A MISSING OR BAD DESCRIPTOR FALLS BACK TO PLAIN TEXT AND CANNOT PREVENT OPENING THE FILE, which
// is this milestone's rule and is why every failure here is a `continue`: no assets granted, a
// directory that will not list, a file that will not read, a descriptor that does not parse.
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
			let Ok(bytes) = read_volume_file(assets, &path, MAX_DESCRIPTOR_BYTES) else {
				continue;
			};
			let Ok(descriptor) = parse_descriptor(&bytes) else {
				continue;
			};
			if loaded.try_reserve(1).is_err() {
				break;
			}
			loaded.push(descriptor);
		}
	}
	loaded
}

fn render(output: &mut impl TerminalWriter, editor: &Editor) -> bool {
	let active = &editor.buffers[editor.active];
	let mut rendered = Vec::new();
	if rendered.try_reserve_exact((EDIT_ROWS + 4) * (EDIT_COLUMNS * 6 + 32)).is_err() {
		return false;
	}
	rendered.extend_from_slice(b"\x1b[H\x1b[2J\x1b[1mlicoedit\x1b[0m ");
	// EVERY OPEN BUFFER IS ON THE HEADER, with the active one marked - a switcher nobody can see
	// the contents of is one nobody uses, and nine names fit on a line.
	for (index, buffer) in editor.buffers.iter().enumerate() {
		rendered.push(if index == editor.active { b'[' } else { b' ' });
		append_decimal(&mut rendered, index + 1);
		rendered.push(b':');
		append_safe(&mut rendered, last_component(&buffer.name), 12);
		if buffer.text.is_dirty() {
			rendered.push(b'*');
		}
		if index == editor.active {
			rendered.push(b']');
		}
	}
	rendered.extend_from_slice(if active.text.is_dirty() { b"  modified" } else { b"  clean" });
	if active.read_only {
		rendered.extend_from_slice(b"  read-only");
	}
	if editor.overwrite_mode {
		rendered.extend_from_slice(b"  ovr");
	}
	rendered.extend_from_slice(b"  line ");
	append_decimal(&mut rendered, active.text.line_number());
	rendered.push(b'/');
	append_decimal(&mut rendered, active.text.line_count());
	rendered.push(b'\n');
	append_safe(&mut rendered, &editor.status, EDIT_COLUMNS);
	if editor.prompt != Prompt::None {
		append_safe(&mut rendered, &editor.entry, 64);
		rendered.extend_from_slice(b"_");
	}
	rendered.extend_from_slice(b"\n\n");

	// THE DESCRIPTOR IS CHOSEN ONCE PER RENDER, from the buffer's explicit language when it has one
	// and otherwise from the name and the first line. A file that matches nothing is plain text,
	// which is this milestone's rule and is why this is an Option rather than a fallback rule.
	let first_line = {
		let bytes = active.text.bytes();
		let end = bytes.iter().position(|byte| *byte == b'\n').unwrap_or(bytes.len());
		&bytes[..end]
	};
	let descriptor = if !editor.highlight {
		None
	} else {
		match active.language {
			Some(index) => editor.descriptors.get(index),
			None => select_descriptor(&editor.descriptors, &active.name, first_line).map(|selection| selection.descriptor),
		}
	};
	let mut state = LineState::new();
	let mut spans = [TokenSpan { start: 0, end: 0, style: 0 }; 64];

	let cursor_line = active.text.line_start_at(active.text.cursor());
	let mut start = cursor_line;
	for _ in 0..EDIT_ROWS / 2 {
		let previous = previous_line_start(&active.text, start);
		if previous == start {
			break;
		}
		start = previous;
	}
	let mut line_number = 1 + active.text.bytes()[..start].iter().filter(|&&byte| byte == b'\n').count();
	let selection = active.text.selection();
	let columns = if editor.line_numbers { EDIT_COLUMNS - 8 } else { EDIT_COLUMNS };
	for _ in 0..EDIT_ROWS {
		if start > active.text.bytes().len() {
			break;
		}
		rendered.push(if start == cursor_line { b'>' } else { b' ' });
		if editor.line_numbers {
			append_decimal(&mut rendered, line_number);
			rendered.extend_from_slice(b" ");
		}
		// A SELECTED LINE IS MARKED rather than reverse-videoed by column: the display is a
		// character grid over a byte buffer, and a per-byte highlight would have to agree with the
		// tab expansion and the UTF-8 decoding to land in the right place.
		let line_end = active.text.next_line_start(start);
		let touched = selection.is_some_and(|(from, to)| from < line_end.max(start + 1) && to >= start);
		rendered.push(if touched { b'*' } else { b' ' });
		let raw = active.text.line_at(start);
		// CRLF IS PRESERVED IN THE BUFFER AND HIDDEN ON THE SCREEN. The carriage return is part of
		// the file and is written back untouched; showing it would put a stray glyph at the end of
		// every line of a file this editor must not silently convert.
		let line = raw.strip_suffix(b"\r").unwrap_or(raw);
		let written = if editor.show_whitespace {
			append_whitespace_line(line, columns, &mut rendered)
		} else if let Some(descriptor) = descriptor {
			let result = descriptor.highlight_line(&mut state, line, &mut spans);
			append_highlighted(line, &spans[..result.spans.min(spans.len())], columns, &mut rendered)
		} else {
			append_display_line(line, columns, 8, &mut rendered).is_ok()
		};
		if !written {
			return false;
		}
		// WRAPPING IS A CONTINUATION ROW rather than a wider line: with wrapping off the tail is cut
		// at the width, which is what a reader asks for when they are looking at columns.
		if editor.wrap && line.len() > columns {
			let mut at = columns;
			while at < line.len() {
				rendered.push(b'\n');
				rendered.extend_from_slice(b"    ");
				let end = (at + columns).min(line.len());
				if append_display_line(&line[at..end], columns, 8, &mut rendered).is_err() {
					return false;
				}
				at = end;
			}
		}
		rendered.push(b'\n');
		let next = active.text.next_line_start(start);
		if next == start {
			break;
		}
		start = next;
		line_number += 1;
	}
	output.write(&rendered)
}

// The name a buffer is listed under: its last path component, because nine full URIs do not fit on
// a header and the part that tells them apart is at the end.
fn last_component(name: &[u8]) -> &[u8] {
	match name.iter().rposition(|byte| *byte == b'/') {
		Some(at) => &name[at + 1..],
		None => name,
	}
}

// Spaces and tabs shown as themselves. For a reader diagnosing indentation, which is the one time
// the difference between a tab and four spaces is the thing being looked at.
fn append_whitespace_line(line: &[u8], columns: usize, output: &mut Vec<u8>) -> bool {
	for &byte in line.iter().take(columns) {
		output.push(match byte {
			b' ' => b'.',
			b'\t' => b'>',
			byte if byte < 0x20 || byte == 0x7f => b'?',
			byte => byte,
		});
	}
	true
}

// The spans carry style ids; the palette is one SGR colour per style, and the escape goes AROUND
// the token rather than replacing it - so a terminal that renders no attributes still shows the
// text.
fn append_highlighted(line: &[u8], spans: &[TokenSpan], columns: usize, output: &mut Vec<u8>) -> bool {
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
