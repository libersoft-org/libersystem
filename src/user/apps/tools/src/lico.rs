// Lico - governed two-panel file manager.
//
// This first manager slice owns a full-screen terminal, maintains independent bounded panel
// listings, and navigates only the explicitly granted volume clients. File mutation, process
// associations, command execution, persistence and selected-file grants stay behind later
// contracts; their function keys remain visible but report that boundary instead of silently
// falling back to ambient authority.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use lico::{Bookmarks, EntryKey, Focus, History, InputDecoder, InputEvent, Key, MouseTracking, SortKey, SortSpec, TerminalGuard, TerminalOptions, TerminalWriter, order, quick_search};
use proto::system::{Error, FileInfo, FileType, LaunchContext};
use rt::*;
use storage_proto::path;
use tools::{ConsoleWriter, ListDirectoryError, VolumeSet, list_volume_directory};
use volume_client_provider as _;

const PANEL_WIDTH: usize = 38;
const PANEL_ROWS: usize = 16;
const MAX_PANEL_ENTRIES: usize = 4_096;

struct Panel {
	path: String,
	// Everything the volume listed, kept whole. The presentation - what is sorted, what is hidden,
	// what a filter admits - is applied to the VIEW rather than to this, so turning a filter off
	// costs nothing and never needs the directory read again.
	entries: Vec<FileInfo>,
	// Indexes into `entries`, in the order the reader sees them. One indirection, and it is what
	// makes the selection survive a sort: the cursor names a position in this list, and the entry it
	// points at is looked up rather than copied.
	view: Vec<usize>,
	selected: usize,
	sort: SortSpec,
	// A literal name filter. Not a glob: the glob matcher lives in the command library and a panel
	// filter is a thing somebody types one letter at a time, where a partial pattern that matches
	// nothing looks like a broken panel.
	filter: Vec<u8>,
	// What has been typed for the quick search, cleared by anything that is not another letter.
	typed: Vec<u8>,
	history: History,
}

impl Panel {
	fn new(path: String) -> Panel {
		Panel { path, entries: Vec::new(), view: Vec::new(), selected: 0, sort: SortSpec::default(), filter: Vec::new(), typed: Vec::new(), history: History::new() }
	}

	fn key_of(entry: &FileInfo) -> EntryKey<'_> {
		EntryKey { name: entry.name.as_bytes(), size: entry.size, modified: entry.mtime, is_dir: entry.r#type == FileType::Dir }
	}

	fn refresh(&mut self, volumes: &VolumeSet) -> Result<(), ListDirectoryError> {
		let storage = volumes.client_for(&self.path, self.path.as_bytes());
		let entries = unsafe { list_volume_directory(storage, &self.path, MAX_PANEL_ENTRIES)? };
		self.entries = entries;
		self.history.visit(self.path.as_bytes());
		self.rebuild();
		Ok(())
	}

	// The view, from the listing and the current presentation. Called after a refresh and after
	// every change to the ordering or the filter, and it is the ONLY place either is applied - two
	// places would eventually disagree about what the reader is looking at.
	fn rebuild(&mut self) {
		let selected_name: Option<String> = self.current().map(|entry| entry.name.clone());
		self.view.clear();
		if self.view.try_reserve(self.entries.len()).is_err() {
			return;
		}
		for (index, entry) in self.entries.iter().enumerate() {
			let key = Panel::key_of(entry);
			if !self.sort.admits(&key) {
				continue;
			}
			if !self.filter.is_empty() && !contains(entry.name.as_bytes(), &self.filter) {
				continue;
			}
			self.view.push(index);
		}
		let entries = &self.entries;
		let spec = self.sort;
		self.view.sort_by(|left, right| order(spec, &Panel::key_of(&entries[*left]), &Panel::key_of(&entries[*right])));
		// THE CURSOR FOLLOWS THE ENTRY, not the position. Re-sorting a listing and leaving the
		// cursor on row four selects whatever moved there, which is how a reader deletes the wrong
		// file after changing the sort order.
		self.selected = match selected_name {
			Some(name) => self.view.iter().position(|index| self.entries[*index].name == name).unwrap_or(0),
			None => 0,
		};
		if self.selected >= self.view.len() {
			self.selected = self.view.len().saturating_sub(1);
		}
	}

	fn current(&self) -> Option<&FileInfo> {
		self.view.get(self.selected).map(|index| &self.entries[*index])
	}

	fn len(&self) -> usize {
		self.view.len()
	}

	fn entry_at(&self, row: usize) -> Option<&FileInfo> {
		self.view.get(row).map(|index| &self.entries[*index])
	}

	// Type a letter of a quick search: the cursor moves to the first name beginning with what has
	// been typed so far, and a letter that matches nothing is REFUSED rather than added - so the
	// search never gets into a state where every further keystroke also matches nothing.
	fn quick(&mut self, byte: u8) -> bool {
		let mut typed = core::mem::take(&mut self.typed);
		if typed.try_reserve(1).is_err() {
			return false;
		}
		typed.push(byte);
		let names = self.view.iter().map(|index| self.entries[*index].name.as_bytes());
		match quick_search(names, &typed, self.selected) {
			Some(at) => {
				self.selected = at;
				self.typed = typed;
				true
			}
			None => {
				typed.pop();
				self.typed = typed;
				false
			}
		}
	}

	fn cycle_sort(&mut self) {
		self.sort.key = match self.sort.key {
			SortKey::Name => SortKey::Extension,
			SortKey::Extension => SortKey::Size,
			SortKey::Size => SortKey::Modified,
			SortKey::Modified => SortKey::Type,
			SortKey::Type => SortKey::Name,
		};
		self.rebuild();
	}

	fn sort_label(&self) -> &'static [u8] {
		match self.sort.key {
			SortKey::Name => b"name",
			SortKey::Extension => b"ext",
			SortKey::Size => b"size",
			SortKey::Modified => b"time",
			SortKey::Type => b"type",
		}
	}

	fn move_up(&mut self) -> bool {
		if self.selected == 0 {
			return false;
		}
		self.selected -= 1;
		true
	}

	fn move_down(&mut self) -> bool {
		if self.selected + 1 >= self.view.len() {
			return false;
		}
		self.selected += 1;
		true
	}

	fn move_home(&mut self) -> bool {
		if self.selected == 0 {
			return false;
		}
		self.selected = 0;
		true
	}

	fn move_end(&mut self) -> bool {
		let Some(last) = self.view.len().checked_sub(1) else { return false };
		if self.selected == last {
			return false;
		}
		self.selected = last;
		true
	}
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ManagerAction {
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
		let cwd = core::str::from_utf8(&cwd).unwrap_or("vol://system");
		let argument = trim(&argument);
		let initial = if argument.is_empty() {
			if cwd.starts_with("vol://") { String::from(cwd) } else { String::from("vol://system") }
		} else if argument.iter().any(u8::is_ascii_whitespace) {
			print(b"Usage: lico [DIRECTORY]\n");
			exit();
		} else {
			match path::resolve(cwd, argument) {
				Some(path) => path,
				None => {
					eprint(b"lico: invalid path\n");
					exit();
				}
			}
		};
		if stdin() == 0 || stdout() == 0 {
			eprint(b"lico: interactive terminal unavailable\n");
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
				let _ = manage(terminal.writer(), &volumes, initial);
			}
			// And back to cooked input and echo, through the same request path.
			if owns_tty {
				tty_set_mode(false, true);
			}
		}
	}
	exit();
}

unsafe fn manage(output: &mut impl TerminalWriter, volumes: &VolumeSet, initial: String) -> bool {
	unsafe {
		let mut panels = [Panel::new(initial.clone()), Panel::new(initial)];
		let mut focus = Focus::new(2);
		let mut bookmarks = Bookmarks::new();
		let mut status: &[u8] = b"Tab panel  arrows select  Enter open  Backspace parent  F1 keys  F10 exit";
		for panel in &mut panels {
			if let Err(error) = panel.refresh(volumes) {
				status = list_error(error);
			}
		}
		let input = stdin();
		let mut decoder = InputDecoder::new();
		let mut redraw = true;
		loop {
			if redraw {
				if !render(output, &panels, &focus, status) {
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
							match apply_event(event, &mut panels, &mut focus, volumes, &mut bookmarks, &mut status) {
								ManagerAction::None => {}
								ManagerAction::Redraw => redraw = true,
								ManagerAction::Exit => return true,
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

fn apply_event(event: InputEvent, panels: &mut [Panel; 2], focus: &mut Focus, volumes: &VolumeSet, bookmarks: &mut Bookmarks, status: &mut &[u8]) -> ManagerAction {
	let active = focus.active().unwrap_or(0) as usize;
	match event {
		InputEvent::Key(Key::Escape) | InputEvent::Key(Key::Function(10)) => ManagerAction::Exit,
		InputEvent::Key(Key::Tab) => {
			focus.next();
			ManagerAction::Redraw
		}
		InputEvent::Key(Key::ArrowUp) => changed(panels[active].move_up()),
		InputEvent::Key(Key::ArrowDown) => changed(panels[active].move_down()),
		InputEvent::Key(Key::PageUp) => changed(repeat_move(&mut panels[active], false)),
		InputEvent::Key(Key::PageDown) => changed(repeat_move(&mut panels[active], true)),
		InputEvent::Key(Key::Home) => changed(panels[active].move_home()),
		InputEvent::Key(Key::End) => changed(panels[active].move_end()),
		InputEvent::Key(Key::Enter) => enter_directory(&mut panels[active], volumes, status),
		InputEvent::Key(Key::Backspace) | InputEvent::Key(Key::ArrowLeft) => parent_directory(&mut panels[active], volumes, status),
		InputEvent::Key(Key::Function(1)) => {
			*status = b"Tab focus  ^U swap  s sort  r reverse  . hidden  d dirs  f filter  b mark  1-9 go  ,/; back/fwd  type to search";
			ManagerAction::Redraw
		}
		// THE PRESENTATION KEYS. Each rebuilds the view rather than re-reading the directory: what
		// changed is how the listing is being looked at, and asking the volume again for the same
		// answer is both slower and a chance for it to have changed underneath.
		InputEvent::Key(Key::Byte(b's')) => {
			panels[active].cycle_sort();
			ManagerAction::Redraw
		}
		InputEvent::Key(Key::Byte(b'r')) => {
			panels[active].sort.reverse = !panels[active].sort.reverse;
			panels[active].rebuild();
			ManagerAction::Redraw
		}
		InputEvent::Key(Key::Byte(b'.')) => {
			panels[active].sort.show_hidden = !panels[active].sort.show_hidden;
			panels[active].rebuild();
			ManagerAction::Redraw
		}
		InputEvent::Key(Key::Byte(b'd')) => {
			panels[active].sort.directories_first = !panels[active].sort.directories_first;
			panels[active].rebuild();
			ManagerAction::Redraw
		}
		InputEvent::Key(Key::Byte(b'f')) => {
			// The filter is what has been typed for the quick search: one keystroke turns a search
			// into a standing filter, which is the same question asked twice.
			panels[active].filter = core::mem::take(&mut panels[active].typed);
			panels[active].rebuild();
			*status = if panels[active].filter.is_empty() { b"filter cleared" } else { b"filter set from the typed name - f with nothing typed clears it" };
			ManagerAction::Redraw
		}
		// PANEL SWAP is one operation and not two navigations: the paths trade places and each
		// panel's selection and ordering go with its own path.
		InputEvent::Key(Key::Control(0x15)) => {
			panels.swap(0, 1);
			ManagerAction::Redraw
		}
		InputEvent::Key(Key::Byte(b',')) => navigate_history(&mut panels[active], volumes, status, false),
		InputEvent::Key(Key::Byte(b';')) => navigate_history(&mut panels[active], volumes, status, true),
		InputEvent::Key(Key::Byte(b'b')) => {
			let path = panels[active].path.clone();
			*status = if bookmarks.add(path.as_bytes()) { b"bookmarked - 1..9 goes to one" } else { b"the bookmark list is full; it refuses rather than dropping an older one" };
			ManagerAction::Redraw
		}
		InputEvent::Key(Key::Byte(byte @ b'1'..=b'9')) => match bookmarks.get((byte - b'1') as usize).and_then(|path| core::str::from_utf8(path).ok()).map(String::from) {
			Some(path) => go_to(&mut panels[active], volumes, status, path),
			None => {
				*status = b"no bookmark there";
				ManagerAction::Redraw
			}
		},
		InputEvent::Key(Key::Function(3)) | InputEvent::Key(Key::Function(4)) | InputEvent::Key(Key::Function(5)) | InputEvent::Key(Key::Function(6)) | InputEvent::Key(Key::Function(7)) | InputEvent::Key(Key::Function(8)) => {
			*status = b"selected-file actions require a scoped file grant and transactional writer";
			ManagerAction::Redraw
		}
		InputEvent::Pointer(pointer) if pointer.pressed => pointer_focus(pointer.column, pointer.row, panels, focus),
		// TYPING MOVES TO THE NAME. Last in the match, so every binding above wins - which is the
		// orthodox trade, and the filter is how a reader reaches a name starting with one of them.
		InputEvent::Key(Key::Byte(byte)) if byte > 0x20 && byte != 0x7f => {
			if !panels[active].quick(byte) {
				*status = b"no name begins with that";
			}
			ManagerAction::Redraw
		}
		_ => ManagerAction::None,
	}
}

fn changed(changed: bool) -> ManagerAction {
	if changed { ManagerAction::Redraw } else { ManagerAction::None }
}

fn repeat_move(panel: &mut Panel, down: bool) -> bool {
	let mut changed = false;
	for _ in 0..PANEL_ROWS {
		let moved = if down { panel.move_down() } else { panel.move_up() };
		if !moved {
			break;
		}
		changed = true;
	}
	changed
}

fn enter_directory(panel: &mut Panel, volumes: &VolumeSet, status: &mut &[u8]) -> ManagerAction {
	let Some(entry) = panel.current() else { return ManagerAction::None };
	if entry.r#type != FileType::Dir {
		*status = b"F3/F4 associations require a selected-file grant";
		return ManagerAction::Redraw;
	}
	let Some(path) = path::resolve(&panel.path, entry.name.as_bytes()) else {
		*status = b"cannot resolve selected directory";
		return ManagerAction::Redraw;
	};
	panel.path = path;
	panel.selected = 0;
	match panel.refresh(volumes) {
		Ok(()) => ManagerAction::Redraw,
		Err(error) => {
			*status = list_error(error);
			ManagerAction::Redraw
		}
	}
}

fn parent_directory(panel: &mut Panel, volumes: &VolumeSet, status: &mut &[u8]) -> ManagerAction {
	let parent = parent_uri(&panel.path);
	if parent == panel.path {
		return ManagerAction::None;
	}
	panel.path = parent;
	panel.selected = 0;
	match panel.refresh(volumes) {
		Ok(()) => ManagerAction::Redraw,
		Err(error) => {
			*status = list_error(error);
			ManagerAction::Redraw
		}
	}
}

// Go to a path the reader named - a bookmark, a history entry. A directory that will not open
// leaves the panel WHERE IT WAS: a bookmark to a volume that is no longer mounted must not empty
// the panel somebody was using.
fn go_to(panel: &mut Panel, volumes: &VolumeSet, status: &mut &[u8], path: String) -> ManagerAction {
	let previous = core::mem::replace(&mut panel.path, path);
	panel.typed.clear();
	match panel.refresh(volumes) {
		Ok(()) => {
			panel.selected = 0;
			ManagerAction::Redraw
		}
		Err(error) => {
			panel.path = previous;
			*status = list_error(error);
			let _ = panel.refresh(volumes);
			ManagerAction::Redraw
		}
	}
}

// Back and forward through the panel's own history. The entry is taken from the history FIRST and
// the visit `refresh` then records is the place it just arrived at, which `History::visit`
// recognises as no navigation at all - otherwise going back would itself be a new visit and the
// history would be a loop.
fn navigate_history(panel: &mut Panel, volumes: &VolumeSet, status: &mut &[u8], forward: bool) -> ManagerAction {
	let target = if forward { panel.history.forward() } else { panel.history.back() };
	let Some(path) = target.and_then(|bytes| core::str::from_utf8(bytes).ok()).map(String::from) else {
		*status = if forward { b"nothing forward from here" } else { b"nothing before this" };
		return ManagerAction::Redraw;
	};
	panel.path = path;
	panel.typed.clear();
	match panel.refresh(volumes) {
		Ok(()) => {
			panel.selected = 0;
			ManagerAction::Redraw
		}
		Err(error) => {
			*status = list_error(error);
			ManagerAction::Redraw
		}
	}
}

// Whether `name` holds `needle`, ignoring ASCII case. The panel filter is a literal because it is
// typed one letter at a time, where a partial pattern that matches nothing looks like a broken
// panel rather than an unfinished expression.
fn contains(name: &[u8], needle: &[u8]) -> bool {
	needle.is_empty() || name.windows(needle.len()).any(|window| window.eq_ignore_ascii_case(needle))
}

fn pointer_focus(column: u16, row: u16, panels: &mut [Panel; 2], focus: &mut Focus) -> ManagerAction {
	let panel = usize::from(column > PANEL_WIDTH as u16 + 2);
	focus.select(panel as u16);
	let row = row.saturating_sub(3) as usize;
	if row < PANEL_ROWS {
		let start = visible_start(&panels[panel]);
		if start + row < panels[panel].len() {
			panels[panel].selected = start + row;
		}
	}
	ManagerAction::Redraw
}

fn render(output: &mut impl TerminalWriter, panels: &[Panel; 2], focus: &Focus, status: &[u8]) -> bool {
	let mut rendered = Vec::new();
	if rendered.try_reserve_exact((PANEL_ROWS + 5) * (PANEL_WIDTH * 2 + 8)).is_err() {
		return false;
	}
	rendered.extend_from_slice(b"\x1b[H\x1b[2J\x1b[1mlico\x1b[0m  two-panel file manager\n");
	render_panel_header(&mut rendered, &panels[0], focus.active() == Some(0));
	rendered.extend_from_slice(b" | ");
	render_panel_header(&mut rendered, &panels[1], focus.active() == Some(1));
	rendered.push(b'\n');
	let left_start = visible_start(&panels[0]);
	let right_start = visible_start(&panels[1]);
	for row in 0..PANEL_ROWS {
		render_entry(&mut rendered, &panels[0], left_start + row, focus.active() == Some(0));
		rendered.extend_from_slice(b" | ");
		render_entry(&mut rendered, &panels[1], right_start + row, focus.active() == Some(1));
		rendered.push(b'\n');
	}
	rendered.extend_from_slice(b"\nF1 Help  F3 View  F4 Edit  F5 Copy  F6 Move  F7 MkDir  F8 Delete  F10 Exit\n");
	rendered.extend_from_slice(status);
	output.write(&rendered)
}

fn render_panel_header(output: &mut Vec<u8>, panel: &Panel, active: bool) {
	let start = output.len();
	output.push(if active { b'>' } else { b' ' });
	// THE MARKER STAYS AGAINST THE PATH. Which panel is active is read at a glance from the left
	// edge, and anything between the two turns that glance into reading.
	append_safe(output, panel.path.as_bytes(), PANEL_WIDTH.saturating_sub(8));
	// THE ORDERING GOES AFTER IT, because a listing sorted by size looks like a listing sorted by
	// name that is in the wrong order, and a filter that hides most of a directory looks like an
	// empty one - so both say so where the reader is already looking.
	output.push(b' ');
	append_safe(output, panel.sort_label(), 4);
	if panel.sort.reverse {
		output.push(b'^');
	}
	if !panel.filter.is_empty() {
		output.push(b'*');
	}
	let used = output.len() - start;
	if used < PANEL_WIDTH {
		pad(output, PANEL_WIDTH - used);
	}
}

fn render_entry(output: &mut Vec<u8>, panel: &Panel, index: usize, active: bool) {
	let row_start = output.len();
	if let Some(entry) = panel.entry_at(index) {
		output.push(if active && index == panel.selected { b'>' } else { b' ' });
		append_safe(output, entry.name.as_bytes(), PANEL_WIDTH.saturating_sub(12));
		if entry.r#type == FileType::Dir {
			output.push(b'/');
		}
		let used = output.len() - row_start;
		if used < PANEL_WIDTH - 9 {
			pad(output, PANEL_WIDTH - 9 - used);
		}
		append_decimal(output, entry.size as usize);
	} else {
		output.push(b' ');
	}
	let used = output.len() - row_start;
	if used < PANEL_WIDTH {
		pad(output, PANEL_WIDTH - used);
	}
}

fn visible_start(panel: &Panel) -> usize {
	if panel.len() <= PANEL_ROWS { 0 } else { panel.selected.saturating_sub(PANEL_ROWS / 2).min(panel.len() - PANEL_ROWS) }
}

fn parent_uri(uri: &str) -> String {
	let root_end = uri[6..].find('/').map(|offset| offset + 6).unwrap_or(uri.len());
	let trimmed = uri.trim_end_matches('/');
	match trimmed.rfind('/') {
		Some(index) if index >= root_end => String::from(&trimmed[..index]),
		_ => String::from(uri),
	}
}

fn list_error(error: ListDirectoryError) -> &'static [u8] {
	match error {
		ListDirectoryError::Unavailable => b"selected volume is unavailable",
		// The volume said why, which it could not before the listing had an error arm.
		ListDirectoryError::Refused(Error::Denied) => b"permission denied for that directory",
		ListDirectoryError::Refused(Error::NotFound) => b"that directory is not there",
		ListDirectoryError::Refused(Error::Again) => b"the volume is busy; try again",
		ListDirectoryError::Refused(_) => b"the volume refused the listing",
		ListDirectoryError::TooManyEntries => b"directory exceeds the 4096-entry panel bound",
		ListDirectoryError::OutOfMemory => b"not enough memory for the directory listing",
		ListDirectoryError::Malformed => b"the directory listing arrived damaged and was not shown in part",
	}
}

fn append_safe(output: &mut Vec<u8>, bytes: &[u8], limit: usize) {
	for &byte in bytes.iter().take(limit) {
		output.push(if byte < 0x20 || byte == 0x7f { b'.' } else { byte });
	}
}

fn pad(output: &mut Vec<u8>, count: usize) {
	for _ in 0..count {
		output.push(b' ');
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
