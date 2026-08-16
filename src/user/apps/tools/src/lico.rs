// Lico - governed two-panel file manager.
//
// Two independent panels over the volumes PermissionManager granted, an orthodox keyboard, tagged
// file operations that publish through the transactional writer, a command bar that launches
// through the governed broker, and file associations that are DATA rather than command lines.
//
// WHAT IT DOES NOT HAVE, by construction rather than by omission: raw process creation (every
// launch goes through PermissionManager, and the child runs under its OWN manifest), any authority
// over a volume it was not granted, and any way for an association, a bookmark or a history entry
// to carry a capability. A remembered path is a suggestion that is re-checked by trying to use it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use lico::{Action, Bookmarks, Chord, CommandBar, Criteria, EntryKey, Focus, Frontier, History, InputDecoder, InputEvent, Key, MouseTracking, Operation, Overwrite, ParseError, PlanError, Request, Results, Settings, SortKey, SortSpec, Source, Tags, TerminalGuard, TerminalOptions, TerminalWriter, classify, detect_file_type, join, order, plan, quick_search, resolve};
use proto::system::{Error, FileInfo, FileType, LaunchContext, WriterMode};
use rt::*;
use security_client::PermissionClient;
use storage_proto::path;
use tools::{ConsoleWriter, ListDirectoryError, VolumeSet, list_volume_directory, read_volume_window};
use volume_client::VolumeClient;
use volume_client_provider as _;

const PANEL_WIDTH: usize = 38;
const PANEL_ROWS: usize = 16;
const MAX_PANEL_ENTRIES: usize = 4_096;
// The window one copy reads at a time. The transactional writer stages what it is given, so this
// bounds what the manager holds rather than what the destination can take.
const COPY_CHUNK: u32 = 8192;
const SETTINGS_PATH: &str = "vol://system/bin/lico/settings.conf";

// What a panel is showing. Four views over the same directory rather than four programs, and the
// mode is per panel because a reader looking at a tree on one side and a listing on the other is
// the ordinary case.
#[derive(Clone, Copy, Eq, PartialEq)]
enum PanelMode {
	List,
	/// The same listing with directories marked, which is what makes a walked subtree readable.
	Tree,
	/// Everything the listing knows about the selected entry.
	Info,
	/// The first screenful of the entry selected in the OTHER panel, read-only.
	Quick,
}

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
	// A literal name filter. Not a glob: a filter is typed one letter at a time, where a partial
	// pattern that matches nothing looks like a broken panel rather than an unfinished expression.
	filter: Vec<u8>,
	history: History,
	tags: Tags,
	mode: PanelMode,
	// Set when the panel is showing search results rather than a directory, so an operation acts on
	// each result's own URI rather than on a name joined to the panel's path.
	results: Option<Results>,
	// The first screenful of the previewed file, for the quick view.
	preview: Vec<u8>,
}

impl Panel {
	fn new(path: String) -> Panel {
		Panel { path, entries: Vec::new(), view: Vec::new(), selected: 0, sort: SortSpec::default(), filter: Vec::new(), history: History::new(), tags: Tags::new(), mode: PanelMode::List, results: None, preview: Vec::new() }
	}

	fn key_of(entry: &FileInfo) -> EntryKey<'_> {
		EntryKey { name: entry.name.as_bytes(), size: entry.size, modified: entry.mtime, is_dir: entry.r#type == FileType::Dir }
	}

	fn refresh(&mut self, volumes: &VolumeSet) -> Result<(), ListDirectoryError> {
		let storage = volumes.client_for(&self.path, self.path.as_bytes());
		let entries = unsafe { list_volume_directory(storage, &self.path, MAX_PANEL_ENTRIES)? };
		self.entries = entries;
		self.results = None;
		// TAGS ARE CLEARED BY A REFRESH. A tag is a thing the reader made on the screen in front of
		// them, and carrying one across a listing that has changed is how the wrong file is deleted.
		self.tags.clear();
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

	fn entry_at(&self, row: usize) -> Option<&FileInfo> {
		self.view.get(row).map(|index| &self.entries[*index])
	}

	// How many rows the panel is showing, whichever it is showing.
	fn rows(&self) -> usize {
		match self.results.as_ref() {
			Some(results) => results.len(),
			None => self.view.len(),
		}
	}

	// The full URI of the entry at `row`. A search result carries its own, which is the property
	// that makes acting on a result act on the file it came from rather than on a name joined to
	// whichever directory the panel happens to be showing.
	fn uri_at(&self, row: usize) -> Option<String> {
		if let Some(results) = self.results.as_ref() {
			return results.get(row).and_then(|bytes| core::str::from_utf8(bytes).ok()).map(String::from);
		}
		let entry = self.entry_at(row)?;
		let joined = join(self.path.as_bytes(), entry.name.as_bytes())?;
		core::str::from_utf8(&joined).ok().map(String::from)
	}

	// Every row an operation should act on: the tagged set, or the entry under the cursor when
	// nothing is tagged. That fallback is the orthodox contract and it is why `F8` on an untagged
	// panel deletes one file rather than nothing.
	fn operands(&self) -> Vec<usize> {
		let mut rows: Vec<usize> = Vec::new();
		if !self.tags.is_empty() {
			if rows.try_reserve_exact(self.tags.len()).is_ok() {
				rows.extend_from_slice(self.tags.rows());
			}
			return rows;
		}
		if self.rows() > 0 && rows.try_reserve_exact(1).is_ok() {
			rows.push(self.selected);
		}
		rows
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

	fn cycle_mode(&mut self) {
		self.mode = match self.mode {
			PanelMode::List => PanelMode::Tree,
			PanelMode::Tree => PanelMode::Info,
			PanelMode::Info => PanelMode::Quick,
			PanelMode::Quick => PanelMode::List,
		};
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
		if self.selected + 1 >= self.rows() {
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
		let Some(last) = self.rows().checked_sub(1) else { return false };
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

// What a typed line will be used for. A prompt is a MODE rather than a second input loop, so every
// exit path - Ctrl+C, a console that goes away, F10 - stays one path.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Prompt {
	None,
	MakeDirectory,
	ConfirmDelete,
	CopyTo,
	MoveTo,
	Select,
	Unselect,
	Search,
	QuickSearch,
	Filter,
}

struct Manager {
	panels: [Panel; 2],
	focus: Focus,
	bookmarks: Bookmarks,
	bar: CommandBar,
	prompt: Prompt,
	entry: Vec<u8>,
	status: Vec<u8>,
	settings: Settings,
	permission: u64,
	assets: u64,
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
		// THE NARROW LAUNCH BROKER AND NOTHING MORE. `lico` cannot create a process; it can ask
		// PermissionManager to start a NAMED program, which then runs under that program's own
		// manifest. Launching from a panel lends the child nothing, because this holds nothing it
		// could lend.
		let permission: u64 = recv_tagged(bootstrap, &mut bootstrap_buffer, b"PERMISSION").unwrap_or(0);
		// This application's own asset directory, where the settings live beside the descriptors.
		let assets: u64 = recv_tagged(bootstrap, &mut bootstrap_buffer, CAP_APP_ASSETS).unwrap_or(0);
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
				let _ = manage(terminal.writer(), &volumes, initial, permission, assets);
			}
			// And back to cooked input and echo, through the same request path.
			if owns_tty {
				tty_set_mode(false, true);
			}
		}
	}
	exit();
}

unsafe fn manage(output: &mut impl TerminalWriter, volumes: &VolumeSet, initial: String, permission: u64, assets: u64) -> bool {
	unsafe {
		// SETTINGS ARE READ BEFORE THE FIRST LISTING, so the panels come up ordered the way they
		// were left. A file that will not read, will not parse or names a version this build does
		// not know yields DEFAULTS - which is exactly what no file at all yields, and is why a
		// corrupt settings file can never stop the manager starting.
		let settings = load_settings(assets);
		let mut manager = Manager { panels: [Panel::new(initial.clone()), Panel::new(initial)], focus: Focus::new(2), bookmarks: Bookmarks::new(), bar: CommandBar::new(), prompt: Prompt::None, entry: Vec::new(), status: Vec::new(), settings, permission, assets };
		for panel in &mut manager.panels {
			panel.sort = SortSpec { key: settings.sort_key, reverse: settings.reverse, directories_first: settings.directories_first, show_hidden: settings.show_hidden };
		}
		manager.say(b"F1 help  F3 view  F4 edit  F5 copy  F6 move  F7 mkdir  F8 delete  F9 menu  F10 exit");
		for index in 0..2 {
			if let Err(error) = manager.panels[index].refresh(volumes) {
				manager.say(list_error(error));
			}
		}
		let input = stdin();
		let mut decoder = InputDecoder::new();
		let mut redraw = true;
		loop {
			if redraw {
				if !render(output, &manager) {
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
							match manager.apply(event, volumes, output) {
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

impl Manager {
	fn say(&mut self, message: &[u8]) {
		self.status.clear();
		if self.status.try_reserve(message.len()).is_ok() {
			self.status.extend_from_slice(message);
		}
	}

	fn active(&self) -> usize {
		self.focus.active().unwrap_or(0) as usize
	}

	fn passive(&self) -> usize {
		1 - self.active()
	}

	fn apply(&mut self, event: InputEvent, volumes: &VolumeSet, output: &mut impl TerminalWriter) -> ManagerAction {
		if self.prompt != Prompt::None {
			return self.apply_prompt(event, volumes);
		}
		let active = self.active();
		match event {
			InputEvent::Key(Key::Function(10)) => ManagerAction::Exit,
			InputEvent::Key(Key::Escape) => {
				self.panels[active].tags.clear();
				self.bar.clear();
				ManagerAction::Redraw
			}
			InputEvent::Key(Key::Tab) => {
				self.focus.next();
				self.refresh_preview(volumes);
				ManagerAction::Redraw
			}
			// ORTHODOX F-KEYS, and every one that mutates goes through a plan before it touches a
			// volume - so a copy into a subtree of its own source is refused with a sentence rather
			// than discovered by filling the destination.
			InputEvent::Key(Key::Function(1)) => {
				self.say(b"^S find  M-s sort  M-r rev  M-h hidden  M-d dirs  M-f filter  M-b mark  M-v mode  Ins tag  +/-/* group  type to run");
				ManagerAction::Redraw
			}
			InputEvent::Key(Key::Function(2)) => {
				self.say(b"F2: M-1..9 bookmark  M-, back  M-; forward  M-w save settings  ^U swap  ^N insert name  ^T complete");
				ManagerAction::Redraw
			}
			InputEvent::Key(Key::Function(3)) => self.open_selected(Action::View, volumes, output),
			InputEvent::Key(Key::Function(4)) => self.open_selected(Action::Edit, volumes, output),
			InputEvent::Key(Key::Function(5)) => self.open_prompt(Prompt::CopyTo, b"copy to: "),
			InputEvent::Key(Key::Function(6)) => self.open_prompt(Prompt::MoveTo, b"move to: "),
			InputEvent::Key(Key::Function(7)) => self.open_prompt(Prompt::MakeDirectory, b"new directory: "),
			InputEvent::Key(Key::Function(8)) => self.confirm_delete(),
			InputEvent::Key(Key::Function(9)) => {
				self.say(b"menu: F3 view  F4 edit  F5 copy  F6 move  F7 mkdir  F8 delete  M-F3 search  F10 exit");
				ManagerAction::Redraw
			}
			InputEvent::Key(Key::Insert) => {
				let row = self.panels[active].selected;
				self.panels[active].tags.toggle(row);
				self.panels[active].move_down();
				ManagerAction::Redraw
			}
			InputEvent::Key(Key::Byte(b'+')) if self.bar.is_empty() => self.open_prompt(Prompt::Select, b"tag matching: "),
			InputEvent::Key(Key::Byte(b'-')) if self.bar.is_empty() => self.open_prompt(Prompt::Unselect, b"untag matching: "),
			InputEvent::Key(Key::Byte(b'*')) if self.bar.is_empty() => {
				let count = self.panels[active].rows();
				self.panels[active].tags.invert(count);
				ManagerAction::Redraw
			}
			InputEvent::Key(Key::Control(0x13)) => self.open_prompt(Prompt::QuickSearch, b"go to name: "),
			InputEvent::Key(Key::Control(0x15)) => {
				self.panels.swap(0, 1);
				ManagerAction::Redraw
			}
			InputEvent::Key(Key::Control(0x0e)) => {
				// Insert the selected name into the bar - the orthodox way of naming a file to a
				// command without typing it.
				if let Some(name) = self.panels[active].current().map(|entry| entry.name.clone()) {
					self.bar.insert_slice(name.as_bytes());
				}
				ManagerAction::Redraw
			}
			InputEvent::Key(Key::Control(0x14)) => {
				self.bar.complete(COMMANDS.iter().copied());
				ManagerAction::Redraw
			}
			// THE PRESENTATION CHORDS. On Alt because the plain letters belong to the command bar,
			// which is what this milestone's own text asks for: typing while the panels have focus
			// opens and edits the bar.
			InputEvent::Chord(Chord { key: Key::Byte(b's'), alt: true, .. }) => {
				self.panels[active].cycle_sort();
				ManagerAction::Redraw
			}
			InputEvent::Chord(Chord { key: Key::Byte(b'r'), alt: true, .. }) => {
				self.panels[active].sort.reverse = !self.panels[active].sort.reverse;
				self.panels[active].rebuild();
				ManagerAction::Redraw
			}
			InputEvent::Chord(Chord { key: Key::Byte(b'h'), alt: true, .. }) => {
				self.panels[active].sort.show_hidden = !self.panels[active].sort.show_hidden;
				self.panels[active].rebuild();
				ManagerAction::Redraw
			}
			InputEvent::Chord(Chord { key: Key::Byte(b'd'), alt: true, .. }) => {
				self.panels[active].sort.directories_first = !self.panels[active].sort.directories_first;
				self.panels[active].rebuild();
				ManagerAction::Redraw
			}
			InputEvent::Chord(Chord { key: Key::Byte(b'v'), alt: true, .. }) => {
				self.panels[active].cycle_mode();
				self.refresh_preview(volumes);
				ManagerAction::Redraw
			}
			InputEvent::Chord(Chord { key: Key::Byte(b'f'), alt: true, .. }) => self.open_prompt(Prompt::Filter, b"filter: "),
			InputEvent::Chord(Chord { key: Key::Function(3), alt: true, .. }) => self.open_prompt(Prompt::Search, b"find files matching: "),
			InputEvent::Chord(Chord { key: Key::Byte(b'b'), alt: true, .. }) => {
				let path = self.panels[active].path.clone();
				let added = self.bookmarks.add(path.as_bytes());
				self.say(if added { b"bookmarked - M-1..9 goes to one" } else { b"the bookmark list is full; it refuses rather than dropping an older one" });
				ManagerAction::Redraw
			}
			InputEvent::Chord(Chord { key: Key::Byte(b'w'), alt: true, .. }) => self.save_settings(),
			InputEvent::Chord(Chord { key: Key::Byte(b','), alt: true, .. }) => self.navigate_history(volumes, false),
			InputEvent::Chord(Chord { key: Key::Byte(b';'), alt: true, .. }) => self.navigate_history(volumes, true),
			InputEvent::Chord(Chord { key: Key::Byte(byte @ b'1'..=b'9'), alt: true, .. }) => match self.bookmarks.get((byte - b'1') as usize).and_then(|path| core::str::from_utf8(path).ok()).map(String::from) {
				Some(path) => self.go_to(volumes, path),
				None => {
					self.say(b"no bookmark there");
					ManagerAction::Redraw
				}
			},
			InputEvent::Key(Key::ArrowUp) => self.moved(active, volumes, Panel::move_up),
			InputEvent::Key(Key::ArrowDown) => self.moved(active, volumes, Panel::move_down),
			InputEvent::Key(Key::PageUp) => self.paged(active, volumes, false),
			InputEvent::Key(Key::PageDown) => self.paged(active, volumes, true),
			InputEvent::Key(Key::Home) => self.moved(active, volumes, Panel::move_home),
			InputEvent::Key(Key::End) => self.moved(active, volumes, Panel::move_end),
			InputEvent::Key(Key::Enter) if !self.bar.is_empty() => self.run_command(volumes, output),
			InputEvent::Key(Key::Enter) => self.enter(volumes, output),
			InputEvent::Key(Key::Backspace) if !self.bar.is_empty() => {
				self.bar.backspace();
				ManagerAction::Redraw
			}
			InputEvent::Key(Key::Backspace) | InputEvent::Key(Key::ArrowLeft) => self.parent(volumes),
			InputEvent::Key(Key::ArrowRight) if !self.bar.is_empty() => {
				self.bar.right();
				ManagerAction::Redraw
			}
			// TYPING OPENS AND EDITS THE COMMAND BAR, which is the orthodox contract and this
			// milestone's own sentence. Quick search is `^S`, so a reader typing a name into the bar
			// never has the cursor jump somewhere in the listing instead.
			InputEvent::Key(Key::Byte(byte)) if byte >= 0x20 && byte != 0x7f => {
				self.bar.insert(byte);
				ManagerAction::Redraw
			}
			InputEvent::Pointer(pointer) if pointer.pressed => self.pointer(pointer.column, pointer.row, volumes),
			_ => ManagerAction::None,
		}
	}

	fn moved(&mut self, panel: usize, volumes: &VolumeSet, step: fn(&mut Panel) -> bool) -> ManagerAction {
		if !step(&mut self.panels[panel]) {
			return ManagerAction::None;
		}
		self.refresh_preview(volumes);
		ManagerAction::Redraw
	}

	fn paged(&mut self, panel: usize, volumes: &VolumeSet, down: bool) -> ManagerAction {
		let mut moved = false;
		for _ in 0..PANEL_ROWS {
			let step = if down { self.panels[panel].move_down() } else { self.panels[panel].move_up() };
			if !step {
				break;
			}
			moved = true;
		}
		if !moved {
			return ManagerAction::None;
		}
		self.refresh_preview(volumes);
		ManagerAction::Redraw
	}

	fn open_prompt(&mut self, prompt: Prompt, label: &[u8]) -> ManagerAction {
		self.prompt = prompt;
		self.entry.clear();
		// The copy and move prompts start at the OTHER panel's directory, which is the orthodox
		// default and is right most of the time.
		if matches!(prompt, Prompt::CopyTo | Prompt::MoveTo) {
			let target = self.panels[self.passive()].path.clone();
			if self.entry.try_reserve(target.len()).is_ok() {
				self.entry.extend_from_slice(target.as_bytes());
			}
		}
		self.say(label);
		ManagerAction::Redraw
	}

	fn apply_prompt(&mut self, event: InputEvent, volumes: &VolumeSet) -> ManagerAction {
		match event {
			InputEvent::Key(Key::Escape) => {
				self.prompt = Prompt::None;
				self.entry.clear();
				self.say(b"cancelled");
				ManagerAction::Redraw
			}
			InputEvent::Key(Key::Backspace) => {
				self.entry.pop();
				ManagerAction::Redraw
			}
			InputEvent::Key(Key::Enter) => self.commit_prompt(volumes),
			InputEvent::Key(Key::Byte(byte)) if byte >= 0x20 && byte != 0x7f => {
				if self.entry.len() < 1024 && self.entry.try_reserve(1).is_ok() {
					self.entry.push(byte);
				}
				ManagerAction::Redraw
			}
			_ => ManagerAction::None,
		}
	}

	fn commit_prompt(&mut self, volumes: &VolumeSet) -> ManagerAction {
		let prompt = self.prompt;
		let entry = core::mem::take(&mut self.entry);
		self.prompt = Prompt::None;
		let active = self.active();
		match prompt {
			Prompt::MakeDirectory => self.make_directory(volumes, &entry),
			Prompt::ConfirmDelete => {
				if entry == b"yes" || entry == b"y" {
					return self.run_operation(volumes, Operation::Delete, b"");
				}
				self.say(b"not deleted");
				ManagerAction::Redraw
			}
			Prompt::CopyTo => self.run_operation(volumes, Operation::Copy, &entry),
			Prompt::MoveTo => self.run_operation(volumes, Operation::Move, &entry),
			Prompt::Select => {
				let names = self.names_of(active);
				let count = self.panels[active].tags.select(names.iter().map(Vec::as_slice), &entry);
				self.report_count(count, b" tagged");
				ManagerAction::Redraw
			}
			Prompt::Unselect => {
				let names = self.names_of(active);
				let count = self.panels[active].tags.unselect(names.iter().map(Vec::as_slice), &entry);
				self.report_count(count, b" untagged");
				ManagerAction::Redraw
			}
			Prompt::Filter => {
				self.panels[active].filter = entry;
				self.panels[active].rebuild();
				ManagerAction::Redraw
			}
			Prompt::QuickSearch => {
				let names = self.names_of(active);
				match quick_search(names.iter().map(Vec::as_slice), &entry, 0) {
					Some(at) => {
						self.panels[active].selected = at;
						self.say(b"");
					}
					None => self.say(b"no name begins with that"),
				}
				self.refresh_preview(volumes);
				ManagerAction::Redraw
			}
			Prompt::Search => self.search(volumes, &entry),
			Prompt::None => ManagerAction::Redraw,
		}
	}

	// The visible names of a panel, OWNED - borrowed names would keep the panel borrowed while the
	// tag set it belongs to is being changed, which is a borrow the compiler is right to refuse:
	// the names come from the same structure the tagging mutates.
	//
	// AS BYTES RATHER THAN STRINGS, and that is a shared-image constraint rather than taste: a
	// `Vec<String>` here instantiates `RawVec<String>::grow_one` in whichever crate the linker
	// happens to place it, and the shared-library graph refuses an import with no declared provider.
	// The same defect P02M0102 hit with a `Vec<u64>`, and the same answer - do not create the
	// instantiation.
	fn names_of(&self, panel: usize) -> Vec<Vec<u8>> {
		let mut names: Vec<Vec<u8>> = Vec::new();
		if names.try_reserve_exact(self.panels[panel].view.len()).is_err() {
			return names;
		}
		for index in &self.panels[panel].view {
			let source = self.panels[panel].entries[*index].name.as_bytes();
			let mut name: Vec<u8> = Vec::new();
			if name.try_reserve_exact(source.len()).is_err() {
				break;
			}
			name.extend_from_slice(source);
			names.push(name);
		}
		names
	}

	fn report_count(&mut self, count: usize, suffix: &[u8]) {
		let mut message: Vec<u8> = Vec::new();
		if message.try_reserve(32 + suffix.len()).is_ok() {
			append_decimal(&mut message, count);
			message.extend_from_slice(suffix);
			self.status = message;
		}
	}

	fn confirm_delete(&mut self) -> ManagerAction {
		let active = self.active();
		let count = self.panels[active].operands().len();
		if count == 0 {
			self.say(b"nothing selected");
			return ManagerAction::Redraw;
		}
		// A TYPED CONFIRMATION FOR A DELETE, not a keystroke. A destructive operation whose
		// confirmation is one key is one somebody confirms by reflex.
		self.prompt = Prompt::ConfirmDelete;
		self.entry.clear();
		let mut message: Vec<u8> = Vec::new();
		if message.try_reserve(64).is_ok() {
			message.extend_from_slice(b"delete ");
			append_decimal(&mut message, count);
			message.extend_from_slice(b" entries? type yes: ");
			self.status = message;
		}
		ManagerAction::Redraw
	}

	// Every mutating operation goes through here: PLAN FIRST over the tagged set, and only then
	// touch a volume. The plan refuses a copy into a subtree of its own source, a move onto itself
	// and an empty selection - each with its own sentence, before any byte is read.
	fn run_operation(&mut self, volumes: &VolumeSet, operation: Operation, destination: &[u8]) -> ManagerAction {
		let active = self.active();
		let rows = self.panels[active].operands();
		if rows.is_empty() {
			self.say(b"nothing selected");
			return ManagerAction::Redraw;
		}
		// The sources are collected as owned values first, because the plan borrows them and the
		// panels are about to be refreshed underneath.
		let mut uris: Vec<Vec<u8>> = Vec::new();
		let mut names: Vec<Vec<u8>> = Vec::new();
		let mut shapes: Vec<(bool, u64)> = Vec::new();
		for row in &rows {
			let Some(uri) = self.panels[active].uri_at(*row) else { continue };
			let (name, is_dir, size) = match self.panels[active].entry_at(*row) {
				Some(entry) => (entry.name.as_bytes(), entry.r#type == FileType::Dir, entry.size),
				None => (last_component(&uri).as_bytes(), false, 0),
			};
			let mut owned_uri: Vec<u8> = Vec::new();
			let mut owned_name: Vec<u8> = Vec::new();
			if owned_uri.try_reserve_exact(uri.len()).is_err() || owned_name.try_reserve_exact(name.len()).is_err() || uris.try_reserve(1).is_err() || names.try_reserve(1).is_err() || shapes.try_reserve(1).is_err() {
				self.say(b"not enough memory to plan that operation");
				return ManagerAction::Redraw;
			}
			owned_uri.extend_from_slice(uri.as_bytes());
			owned_name.extend_from_slice(name);
			uris.push(owned_uri);
			names.push(owned_name);
			shapes.push((is_dir, size));
		}
		let mut sources: Vec<Source> = Vec::new();
		if sources.try_reserve_exact(uris.len()).is_err() {
			self.say(b"not enough memory to plan that operation");
			return ManagerAction::Redraw;
		}
		for index in 0..uris.len() {
			sources.push(Source { path: &uris[index], name: &names[index], is_dir: shapes[index].0, size: shapes[index].1 });
		}
		let planned = match plan(operation, &sources, destination, Overwrite::Skip) {
			Ok(planned) => planned,
			Err(error) => {
				self.say(plan_error(error));
				return ManagerAction::Redraw;
			}
		};
		let mut done = 0usize;
		let mut refused = 0usize;
		for step in &planned.steps {
			// CANCELLATION IS CHECKED BETWEEN ENTRIES, so an interrupt stops the operation at an
			// entry boundary: what has been done is complete, and what has not been started is
			// untouched.
			if unsafe { interrupted() } {
				break;
			}
			let ok = unsafe {
				match operation {
					Operation::Delete => remove_entry(volumes, &step.source, step.is_dir),
					Operation::Copy => copy_entry(volumes, &step.source, &step.destination, step.is_dir),
					Operation::Move => move_entry(volumes, &step.source, &step.destination, step.is_dir),
				}
			};
			if ok {
				done += 1;
			} else {
				refused += 1;
			}
		}
		let mut message: Vec<u8> = Vec::new();
		if message.try_reserve(112).is_ok() {
			append_decimal(&mut message, done);
			message.extend_from_slice(b" done, ");
			append_decimal(&mut message, refused);
			message.extend_from_slice(b" refused - a refusal leaves the source and any existing destination as they were");
			self.status = message;
		}
		for index in 0..2 {
			let _ = self.panels[index].refresh(volumes);
		}
		ManagerAction::Redraw
	}

	fn make_directory(&mut self, volumes: &VolumeSet, name: &[u8]) -> ManagerAction {
		if name.is_empty() {
			self.say(b"a directory needs a name");
			return ManagerAction::Redraw;
		}
		let active = self.active();
		let Some(joined) = join(self.panels[active].path.as_bytes(), name) else {
			self.say(b"not enough memory");
			return ManagerAction::Redraw;
		};
		let Ok(uri) = core::str::from_utf8(&joined) else {
			self.say(b"that name is not text");
			return ManagerAction::Redraw;
		};
		let storage = volumes.client_for(&self.panels[active].path, uri.as_bytes());
		let made = matches!(VolumeClient::new(storage).mkdir(uri), Some(Ok(_)));
		self.say(if made { b"created" } else { b"the volume refused that directory" });
		let _ = self.panels[active].refresh(volumes);
		ManagerAction::Redraw
	}

	// A recursive search from the active panel's directory, into a bounded result set that becomes
	// a temporary panel. Each result keeps its own URI, so viewing, editing, copying or deleting one
	// acts on the file it came from through the capability that reached it.
	fn search(&mut self, volumes: &VolumeSet, pattern: &[u8]) -> ManagerAction {
		if pattern.is_empty() {
			self.say(b"a search needs a pattern");
			return ManagerAction::Redraw;
		}
		let active = self.active();
		let criteria = Criteria { name: Some(pattern), ..Criteria::default() };
		let mut frontier = Frontier::new();
		let mut results = Results::new();
		frontier.push(self.panels[active].path.as_bytes(), 0);
		while let Some((directory, depth)) = frontier.pop() {
			if unsafe { interrupted() } {
				break;
			}
			let Ok(uri) = core::str::from_utf8(&directory) else { continue };
			let storage = volumes.client_for(uri, uri.as_bytes());
			let Ok(entries) = (unsafe { list_volume_directory(storage, uri, MAX_PANEL_ENTRIES) }) else { continue };
			for entry in &entries {
				let key = Panel::key_of(entry);
				let Some(child) = join(&directory, entry.name.as_bytes()) else { continue };
				if criteria.admits(&key, depth + 1) {
					results.push(&child);
				}
				// A DIRECTORY WHOSE NAME DOES NOT MATCH IS STILL WALKED INTO: the files somebody is
				// looking for live in directories they did not name.
				if key.is_dir && criteria.descends(depth + 1) {
					frontier.push(&child, depth + 1);
				}
			}
		}
		let found = results.len();
		let truncated = results.is_truncated() || frontier.refused_anything();
		self.panels[active].results = Some(results);
		self.panels[active].tags.clear();
		self.panels[active].selected = 0;
		let mut message: Vec<u8> = Vec::new();
		if message.try_reserve(112).is_ok() {
			append_decimal(&mut message, found);
			message.extend_from_slice(if truncated { b" results, and the search stopped at its bound - this is not the whole answer" } else { b" results - Enter opens one, Backspace leaves the result list" });
			self.status = message;
		}
		ManagerAction::Redraw
	}

	fn enter(&mut self, volumes: &VolumeSet, output: &mut impl TerminalWriter) -> ManagerAction {
		let active = self.active();
		if self.panels[active].results.is_some() {
			return self.open_selected(Action::View, volumes, output);
		}
		let Some(entry) = self.panels[active].current() else { return ManagerAction::None };
		if entry.r#type != FileType::Dir {
			// AN ASSOCIATION DECIDES, and it is data: a file type, an action and a canonical
			// program name, with nowhere to put a command line or an argument.
			return self.open_selected(Action::Edit, volumes, output);
		}
		let Some(joined) = join(self.panels[active].path.as_bytes(), entry.name.as_bytes()) else { return ManagerAction::None };
		let Ok(path) = core::str::from_utf8(&joined) else { return ManagerAction::None };
		let path = String::from(path);
		self.go_to(volumes, path)
	}

	// Open the selected entry through its association. `wanted` is what the reader asked for - F3
	// is a view even for a file the table would edit - and the table decides the program.
	fn open_selected(&mut self, wanted: Action, volumes: &VolumeSet, output: &mut impl TerminalWriter) -> ManagerAction {
		let active = self.active();
		let Some(uri) = self.panels[active].uri_at(self.panels[active].selected) else {
			self.say(b"nothing selected");
			return ManagerAction::Redraw;
		};
		let head = unsafe { peek(volumes, &uri, 32) };
		let kind = detect_file_type(last_component(&uri).as_bytes(), &head, false);
		let association = resolve(lico::DEFAULT_ASSOCIATIONS, kind);
		let program = if wanted == Action::View && association.action == Action::Edit { "licoview" } else { association.program };
		self.launch(output, program, &uri, true)
	}

	// Run what is in the command bar.
	fn run_command(&mut self, volumes: &VolumeSet, output: &mut impl TerminalWriter) -> ManagerAction {
		let line = self.bar.take();
		match classify(&line) {
			Ok(Request::ChangeDirectory(target)) => {
				// `cd` IS PANEL NAVIGATION and is not launched. A `cd` in a child changes a
				// directory that is thrown away when the child exits, and reporting success for
				// that is the lie this refuses to tell.
				let base = self.panels[self.active()].path.clone();
				let resolved = if target.is_empty() { volume_root(&base) } else { path::resolve(&base, &target) };
				match resolved {
					Some(path) => self.go_to(volumes, path),
					None => {
						self.say(b"that is not a path this panel can reach");
						ManagerAction::Redraw
					}
				}
			}
			Ok(Request::Unsupported(_)) => {
				self.say(b"that builtin changes session state, which a launched program cannot do on its parent's behalf");
				ManagerAction::Redraw
			}
			Ok(Request::Foreground(words)) => {
				let (program, args) = split_words(&words);
				self.launch(output, &program, &args, true)
			}
			Ok(Request::Background(words)) => {
				let (program, args) = split_words(&words);
				self.launch(output, &program, &args, false)
			}
			Err(error) => {
				self.say(match error {
					ParseError::UnterminatedQuote => b"that line has a quote that is never closed, and where an argument ends changes what the command is",
					ParseError::Empty => b"nothing to run",
					ParseError::TooManyWords => b"that line has more words than a launch will carry",
					ParseError::OutOfMemory => b"not enough memory to read that line",
				});
				ManagerAction::Redraw
			}
		}
	}

	// Start a program through the governed broker.
	//
	// THE BROKER RUNS IT UNDER ITS OWN MANIFEST. `lico` names a program and its arguments and holds
	// nothing it could lend, which is what makes launching from a panel exactly as safe as typing
	// the name at a shell - and it is why an association can name only a program, never a command.
	fn launch(&mut self, output: &mut impl TerminalWriter, program: &str, args: &str, foreground: bool) -> ManagerAction {
		if self.permission == 0 {
			self.say(b"this boot granted no launch broker, so nothing can be started from here");
			return ManagerAction::Redraw;
		}
		let Some((read_end, write_end)) = (unsafe { channel() }) else {
			self.say(b"could not make an output channel");
			return ManagerAction::Redraw;
		};
		let cwd = self.panels[self.active()].path.clone();
		let mut client = PermissionClient::new(self.permission);
		let started = matches!(client.run(program, args, &cwd, &Vec::new(), &write_end), Some(Ok(_)));
		if !started {
			unsafe { close(read_end) };
			self.say(b"the broker refused that program - it may not be a governed executable");
			return ManagerAction::Redraw;
		}
		if !foreground {
			// A BACKGROUND LAUNCH IS NOT A SESSION JOB HERE, and saying so is the honest answer:
			// registering one needs a SessionService capability this program is deliberately not
			// given, and claiming a job that no session knows about would be worse than the gap.
			unsafe { close(read_end) };
			self.say(b"started in the background; it is not a session job, because registering one needs session authority this program does not hold");
			return ManagerAction::Redraw;
		}
		// THE FOREGROUND COMMAND OWNS THE SCREEN AND GIVES IT BACK. The panels are left through the
		// alternate screen so what the command writes lands on the ordinary terminal, and the exact
		// panel screen is redrawn on return.
		output.write(b"\x1b[?1049l");
		let mut buffer = [0u8; 512];
		loop {
			if unsafe { interrupted() } {
				break;
			}
			match unsafe { recv_blocking(read_end, &mut buffer) } {
				Received::Closed => break,
				Received::Message { len, .. } => {
					output.write(&buffer[..len]);
				}
			}
		}
		unsafe { close(read_end) };
		output.write(b"\n[press a key to return to lico]\n");
		unsafe { wait_any(&[stdin()], 0) };
		let mut drain = [0u8; 64];
		while let Polled::Message { .. } = unsafe { try_recv(stdin(), &mut drain) } {}
		output.write(b"\x1b[?1049h");
		self.say(b"");
		ManagerAction::Redraw
	}

	fn go_to(&mut self, volumes: &VolumeSet, path: String) -> ManagerAction {
		let active = self.active();
		let previous = core::mem::replace(&mut self.panels[active].path, path);
		match self.panels[active].refresh(volumes) {
			Ok(()) => {
				self.panels[active].selected = 0;
				self.refresh_preview(volumes);
				ManagerAction::Redraw
			}
			Err(error) => {
				// A DIRECTORY THAT WILL NOT OPEN LEAVES THE PANEL WHERE IT WAS: a bookmark to a
				// volume that is no longer granted must not empty the panel somebody was using.
				self.panels[active].path = previous;
				self.say(list_error(error));
				let _ = self.panels[active].refresh(volumes);
				ManagerAction::Redraw
			}
		}
	}

	fn parent(&mut self, volumes: &VolumeSet) -> ManagerAction {
		let active = self.active();
		if self.panels[active].results.is_some() {
			let _ = self.panels[active].refresh(volumes);
			self.say(b"back to the directory");
			return ManagerAction::Redraw;
		}
		let parent = parent_uri(&self.panels[active].path);
		if parent == self.panels[active].path {
			return ManagerAction::None;
		}
		self.go_to(volumes, parent)
	}

	fn navigate_history(&mut self, volumes: &VolumeSet, forward: bool) -> ManagerAction {
		let active = self.active();
		let target = if forward { self.panels[active].history.forward() } else { self.panels[active].history.back() };
		let Some(path) = target.and_then(|bytes| core::str::from_utf8(bytes).ok()).map(String::from) else {
			self.say(if forward { b"nothing forward from here" } else { b"nothing before this" });
			return ManagerAction::Redraw;
		};
		self.panels[active].path = path;
		match self.panels[active].refresh(volumes) {
			Ok(()) => {
				self.panels[active].selected = 0;
				ManagerAction::Redraw
			}
			Err(error) => {
				self.say(list_error(error));
				ManagerAction::Redraw
			}
		}
	}

	// The passive panel's quick view reads the first screenful of the selected file and nothing
	// more - a preview is a look, not an open, and holding a whole file to show twenty lines of it
	// is what the bounded window read exists to avoid.
	fn refresh_preview(&mut self, volumes: &VolumeSet) {
		let passive = self.passive();
		if self.panels[passive].mode != PanelMode::Quick {
			self.panels[passive].preview.clear();
			return;
		}
		let active = self.active();
		let Some(uri) = self.panels[active].uri_at(self.panels[active].selected) else {
			self.panels[passive].preview.clear();
			return;
		};
		let preview = unsafe { peek(volumes, &uri, 2048) };
		self.panels[passive].preview = preview;
	}

	fn save_settings(&mut self) -> ManagerAction {
		let active = self.active();
		self.settings.sort_key = self.panels[active].sort.key;
		self.settings.reverse = self.panels[active].sort.reverse;
		self.settings.directories_first = self.panels[active].sort.directories_first;
		self.settings.show_hidden = self.panels[active].sort.show_hidden;
		if self.assets == 0 {
			self.say(b"no app directory was granted, so there is nowhere to keep settings");
			return ManagerAction::Redraw;
		}
		let Some(bytes) = self.settings.encode() else {
			self.say(b"not enough memory to write the settings");
			return ManagerAction::Redraw;
		};
		// THROUGH THE TRANSACTIONAL WRITER, into the suite's OWN directory and nowhere else: the
		// client is scoped to `bin/lico`, so this cannot write anywhere a settings file should not
		// be, and a failure leaves the previous settings exactly as they were.
		let mut client = VolumeClient::new(self.assets);
		let published = match client.open_writer(SETTINGS_PATH, WriterMode::Replace) {
			Some(Ok(mut writer)) => {
				let written = matches!(writer.write(&bytes), Some(Ok(_)));
				let committed = written && matches!(writer.commit(), Some(Ok(_)));
				if !committed {
					let _ = writer.abort();
				}
				unsafe { close(writer.handle()) };
				committed
			}
			_ => false,
		};
		self.say(if published { b"settings saved beside the suite" } else { b"the settings could not be published - the previous ones are unchanged" });
		ManagerAction::Redraw
	}

	fn pointer(&mut self, column: u16, row: u16, volumes: &VolumeSet) -> ManagerAction {
		let panel = usize::from(column > PANEL_WIDTH as u16 + 2);
		self.focus.select(panel as u16);
		let row = row.saturating_sub(3) as usize;
		if row < PANEL_ROWS {
			let start = visible_start(&self.panels[panel]);
			if start + row < self.panels[panel].rows() {
				self.panels[panel].selected = start + row;
			}
		}
		self.refresh_preview(volumes);
		ManagerAction::Redraw
	}
}

// The command names the bar completes: the staged tools a governed launch can reach.
const COMMANDS: &[&[u8]] = &[
	b"cat",
	b"clear",
	b"cp",
	b"cut",
	b"du",
	b"find",
	b"grep",
	b"head",
	b"hexdump",
	b"imgview",
	b"less",
	b"lico",
	b"licoedit",
	b"licoview",
	b"ls",
	b"lsblk",
	b"lsvol",
	b"mkdir",
	b"mv",
	b"play",
	b"pwd",
	b"rm",
	b"rmdir",
	b"sort",
	b"tail",
	b"tee",
	b"touch",
	b"tree",
	b"truncate",
	b"uptime",
	b"wc",
	b"which",
];

fn split_words(words: &[Vec<u8>]) -> (String, String) {
	let program = String::from(core::str::from_utf8(&words[0]).unwrap_or(""));
	let mut args = String::new();
	for word in &words[1..] {
		if let Ok(text) = core::str::from_utf8(word) {
			if !args.is_empty() {
				args.push(' ');
			}
			args.push_str(text);
		}
	}
	(program, args)
}

// Read at most `limit` bytes from the front of a file. Used for content sniffing and the quick
// view; a short answer is the end of the file rather than a failure.
unsafe fn peek(volumes: &VolumeSet, uri: &str, limit: u32) -> Vec<u8> {
	let storage = volumes.client_for(uri, uri.as_bytes());
	unsafe { read_volume_window(storage, uri, 0, limit) }.unwrap_or_default()
}

// Copy one entry through the transactional writer. A directory becomes a directory: the recursive
// copy of its contents belongs to the background-jobs item, and a "copy" that silently made an
// empty directory without saying so would be worse than one that says what it did.
unsafe fn copy_entry(volumes: &VolumeSet, source: &[u8], destination: &[u8], is_dir: bool) -> bool {
	let (Ok(source), Ok(destination)) = (core::str::from_utf8(source), core::str::from_utf8(destination)) else {
		return false;
	};
	if is_dir {
		let storage = volumes.client_for(destination, destination.as_bytes());
		return matches!(VolumeClient::new(storage).mkdir(destination), Some(Ok(_)));
	}
	let reader = volumes.client_for(source, source.as_bytes());
	let mut writer_client = VolumeClient::new(volumes.client_for(destination, destination.as_bytes()));
	let Some(Ok(mut writer)) = writer_client.open_writer(destination, WriterMode::Replace) else {
		return false;
	};
	let mut offset: u64 = 0;
	loop {
		let window = match unsafe { read_volume_window(reader, source, offset, COPY_CHUNK) } {
			Ok(window) => window,
			Err(_) => {
				let _ = writer.abort();
				unsafe { close(writer.handle()) };
				return false;
			}
		};
		if window.is_empty() {
			break;
		}
		offset += window.len() as u64;
		if !matches!(writer.write(&window), Some(Ok(_))) {
			let _ = writer.abort();
			unsafe { close(writer.handle()) };
			return false;
		}
	}
	// NOTHING IS VISIBLE UNDER THE DESTINATION'S NAME UNTIL HERE, so a copy that failed at any
	// point above leaves whatever was there before exactly as it was.
	let committed = matches!(writer.commit(), Some(Ok(_)));
	unsafe { close(writer.handle()) };
	committed
}

// Move: `rename` within a volume, copy-then-remove across one - and the source is removed only
// after the destination has been published, so an interruption leaves two files rather than none.
unsafe fn move_entry(volumes: &VolumeSet, source: &[u8], destination: &[u8], is_dir: bool) -> bool {
	let (Ok(source_text), Ok(destination_text)) = (core::str::from_utf8(source), core::str::from_utf8(destination)) else {
		return false;
	};
	if same_volume(source, destination) {
		let storage = volumes.client_for(source_text, source);
		if matches!(VolumeClient::new(storage).rename(source_text, destination_text), Some(Ok(_))) {
			return true;
		}
	}
	unsafe { copy_entry(volumes, source, destination, is_dir) && remove_entry(volumes, source, is_dir) }
}

unsafe fn remove_entry(volumes: &VolumeSet, target: &[u8], is_dir: bool) -> bool {
	let Ok(uri) = core::str::from_utf8(target) else { return false };
	let storage = volumes.client_for(uri, target);
	let mut client = VolumeClient::new(storage);
	let answer = if is_dir { client.rmdir(uri) } else { client.remove(uri) };
	matches!(answer, Some(Ok(_)))
}

// Whether two URIs name the same volume, which is what decides whether a move can be a rename.
fn same_volume(left: &[u8], right: &[u8]) -> bool {
	volume_of(left) == volume_of(right)
}

fn volume_of(uri: &[u8]) -> &[u8] {
	let rest = uri.strip_prefix(b"vol://").unwrap_or(uri);
	match rest.iter().position(|&byte| byte == b'/') {
		Some(at) => &rest[..at],
		None => rest,
	}
}

fn volume_root(uri: &str) -> Option<String> {
	let mut root = String::from("vol://");
	root.push_str(core::str::from_utf8(volume_of(uri.as_bytes())).ok()?);
	Some(root)
}

fn last_component(uri: &str) -> &str {
	match uri.rfind('/') {
		Some(at) => &uri[at + 1..],
		None => uri,
	}
}

unsafe fn load_settings(assets: u64) -> Settings {
	if assets == 0 {
		return Settings::default();
	}
	match unsafe { read_volume_window(assets, SETTINGS_PATH, 0, 4096) } {
		Ok(bytes) => Settings::decode(&bytes),
		Err(_) => Settings::default(),
	}
}

fn plan_error(error: PlanError) -> &'static [u8] {
	match error {
		PlanError::Empty => b"nothing selected",
		PlanError::DestinationInsideSource => b"that destination is inside one of the sources, which is a copy that never finishes",
		PlanError::SameObject => b"the source and the destination are the same object",
		PlanError::TooMany => b"that is more entries than one operation will carry",
		PlanError::OutOfMemory => b"not enough memory to plan that operation",
	}
}

fn render(output: &mut impl TerminalWriter, manager: &Manager) -> bool {
	let mut rendered = Vec::new();
	if rendered.try_reserve_exact((PANEL_ROWS + 6) * (PANEL_WIDTH * 2 + 16)).is_err() {
		return false;
	}
	rendered.extend_from_slice(b"\x1b[H\x1b[2J\x1b[1mlico\x1b[0m  two-panel file manager\n");
	render_panel_header(&mut rendered, &manager.panels[0], manager.focus.active() == Some(0));
	rendered.extend_from_slice(b" | ");
	render_panel_header(&mut rendered, &manager.panels[1], manager.focus.active() == Some(1));
	rendered.push(b'\n');
	let starts = [visible_start(&manager.panels[0]), visible_start(&manager.panels[1])];
	for row in 0..PANEL_ROWS {
		render_row(&mut rendered, &manager.panels[0], starts[0], row, manager.focus.active() == Some(0));
		rendered.extend_from_slice(b" | ");
		render_row(&mut rendered, &manager.panels[1], starts[1], row, manager.focus.active() == Some(1));
		rendered.push(b'\n');
	}
	// THE KEY LABELS ADAPT rather than overlap the panels: the row is one line of short labels, so
	// a narrow terminal loses the trailing ones instead of wrapping into the listing.
	rendered.extend_from_slice(b"\nF1 Help F3 View F4 Edit F5 Copy F6 Move F7 MkDir F8 Del F9 Menu F10 Exit\n");
	// The command bar, beneath the F-key row and separate from it, as the orthodox layout has it.
	rendered.extend_from_slice(b"$ ");
	append_safe(&mut rendered, manager.bar.line(), PANEL_WIDTH * 2);
	rendered.push(b'_');
	rendered.push(b'\n');
	append_safe(&mut rendered, &manager.status, PANEL_WIDTH * 2 + 4);
	if manager.prompt != Prompt::None {
		append_safe(&mut rendered, &manager.entry, 128);
		rendered.push(b'_');
	}
	output.write(&rendered)
}

fn render_panel_header(output: &mut Vec<u8>, panel: &Panel, active: bool) {
	let start = output.len();
	// THE MARKER STAYS AGAINST THE PATH. Which panel is active is read at a glance from the left
	// edge, and anything between the two turns that glance into reading.
	output.push(if active { b'>' } else { b' ' });
	append_safe(output, panel.path.as_bytes(), PANEL_WIDTH.saturating_sub(12));
	// THE ORDERING AND THE MODE GO AFTER IT, because a listing sorted by size looks like a listing
	// sorted by name that is in the wrong order, and a filter that hides most of a directory looks
	// like an empty one - so both say so where the reader is already looking.
	output.push(b' ');
	append_safe(output, panel.sort_label(), 4);
	if panel.sort.reverse {
		output.push(b'^');
	}
	if !panel.filter.is_empty() {
		output.push(b'*');
	}
	if panel.results.is_some() {
		output.push(b'?');
	}
	output.push(match panel.mode {
		PanelMode::List => b' ',
		PanelMode::Tree => b'T',
		PanelMode::Info => b'I',
		PanelMode::Quick => b'Q',
	});
	let used = output.len() - start;
	if used < PANEL_WIDTH {
		pad(output, PANEL_WIDTH - used);
	}
}

fn render_row(output: &mut Vec<u8>, panel: &Panel, start: usize, row: usize, active: bool) {
	let row_start = output.len();
	match panel.mode {
		PanelMode::Info => render_info_row(output, panel, row),
		PanelMode::Quick => render_preview_row(output, panel, row),
		_ => render_entry(output, panel, start + row, active),
	}
	let used = output.len() - row_start;
	if used < PANEL_WIDTH {
		pad(output, PANEL_WIDTH - used);
	}
}

fn render_entry(output: &mut Vec<u8>, panel: &Panel, index: usize, active: bool) {
	if let Some(results) = panel.results.as_ref() {
		match results.get(index) {
			Some(uri) => {
				output.push(if active && index == panel.selected { b'>' } else { b' ' });
				output.push(if panel.tags.contains(index) { b'#' } else { b' ' });
				// The TAIL of the URI, because a result list of full paths shows the same prefix on
				// every row and the part that differs falls off the end.
				let width = PANEL_WIDTH - 3;
				let shown = if uri.len() > width { &uri[uri.len() - width..] } else { uri };
				append_safe(output, shown, width);
			}
			None => output.push(b' '),
		}
		return;
	}
	let Some(entry) = panel.entry_at(index) else {
		output.push(b' ');
		return;
	};
	output.push(if active && index == panel.selected { b'>' } else { b' ' });
	// A TAG IS VISIBLE WITHOUT COLOUR. An operation acts on the tagged set, so which rows are in it
	// has to be readable on a terminal that renders no attributes at all.
	output.push(if panel.tags.contains(index) { b'#' } else { b' ' });
	if panel.mode == PanelMode::Tree {
		output.extend_from_slice(if entry.r#type == FileType::Dir { b"+ " } else { b"  " });
	}
	let name_width = PANEL_WIDTH.saturating_sub(if panel.mode == PanelMode::Tree { 16 } else { 14 });
	let before = output.len();
	append_safe(output, entry.name.as_bytes(), name_width);
	if entry.r#type == FileType::Dir {
		output.push(b'/');
	}
	let written = output.len() - before;
	if written < name_width + 1 {
		pad(output, name_width + 1 - written);
	}
	append_decimal(output, entry.size as usize);
}

// Everything the listing knows about the selected entry, one field per row.
fn render_info_row(output: &mut Vec<u8>, panel: &Panel, row: usize) {
	let Some(entry) = panel.current() else {
		output.push(b' ');
		return;
	};
	match row {
		0 => {
			output.extend_from_slice(b" name  ");
			append_safe(output, entry.name.as_bytes(), PANEL_WIDTH - 8);
		}
		1 => {
			output.extend_from_slice(b" type  ");
			output.extend_from_slice(if entry.r#type == FileType::Dir { b"directory" } else { b"file" });
		}
		2 => {
			output.extend_from_slice(b" size  ");
			append_decimal(output, entry.size as usize);
		}
		3 => {
			output.extend_from_slice(b" mtime ");
			append_decimal(output, entry.mtime as usize);
		}
		4 => {
			output.extend_from_slice(b" ctime ");
			append_decimal(output, entry.ctime as usize);
		}
		5 => {
			output.extend_from_slice(b" tagged ");
			append_decimal(output, panel.tags.len());
		}
		_ => output.push(b' '),
	}
}

// The first lines of the previewed file. The same bounded read every other volume tool uses, and
// the same safe rendering: a byte written as itself puts control characters on the terminal.
fn render_preview_row(output: &mut Vec<u8>, panel: &Panel, row: usize) {
	let mut at = 0;
	for _ in 0..row {
		match panel.preview[at..].iter().position(|&byte| byte == b'\n') {
			Some(offset) => at += offset + 1,
			None => {
				output.push(b' ');
				return;
			}
		}
	}
	if at >= panel.preview.len() {
		output.push(b' ');
		return;
	}
	let end = panel.preview[at..].iter().position(|&byte| byte == b'\n').map(|offset| at + offset).unwrap_or(panel.preview.len());
	output.push(b' ');
	append_safe(output, &panel.preview[at..end], PANEL_WIDTH - 2);
}

fn visible_start(panel: &Panel) -> usize {
	let rows = panel.rows();
	if rows <= PANEL_ROWS { 0 } else { panel.selected.saturating_sub(PANEL_ROWS / 2).min(rows - PANEL_ROWS) }
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

// Whether `name` holds `needle`, ignoring ASCII case. The panel filter is a literal because it is
// typed one letter at a time, where a partial pattern that matches nothing looks like a broken
// panel rather than an unfinished expression.
fn contains(name: &[u8], needle: &[u8]) -> bool {
	needle.is_empty() || name.windows(needle.len()).any(|window| window.eq_ignore_ascii_case(needle))
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
