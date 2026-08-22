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
use lico::{Action, Bookmarks, Chord, CommandBar, CriteriaError, EntryKey, Focus, Frontier, History, InputDecoder, InputEvent, Key, MouseTracking, Operation, Overwrite, ParseError, Plan, PlanError, Request, Results, Settings, SortKey, SortSpec, Source, Step, Tags, TerminalGuard, TerminalOptions, TerminalWriter, classify, deepest_first, detect_file_type, expand, join, order, parse_criteria, plan, quick_search, resolve};
use proto::system::{EnvVar, Error, FileInfo, FileType, LaunchContext, WriterMode};
use rt::*;
use security_client::PermissionClient;
use storage_proto::path;
use tools::{ConsoleWriter, ListDirectoryError, VolumeSet, list_volume_directory, read_volume_window};
use volume_client::{VolumeClient, WRITER_CHUNK};
use volume_client_provider as _;

const PANEL_WIDTH: usize = 38;
const PANEL_ROWS: usize = 16;
const MAX_PANEL_ENTRIES: usize = 4_096;
// The window one copy reads at a time. The transactional writer stages what it is given, so this
// bounds what the manager holds rather than what the destination can take.
const COPY_CHUNK: u32 = 8192;
// The window a content search reads at a time. Windows OVERLAP by the pattern's length minus one,
// so a match straddling a boundary is still found.
const CONTENT_CHUNK: u32 = 8192;
// How many steps of a running operation are advanced per turn of the input loop. A few rather than
// one, because an entry can be a rename - which is fast - and redrawing after every one of those
// spends more time on the screen than on the work.
const STEPS_PER_TURN: usize = 4;
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
	// How deep below the panel's own directory each visible row sits. Empty in list mode; in tree
	// mode it is what indents a row, and it is a PARALLEL ARRAY to `view` rather than a field on the
	// entry because the entries are the volume's answer and the depth is this panel's arrangement.
	depths: Vec<usize>,
	// Whether the listing shows the long columns. A per-panel choice, because the narrow panel is
	// usually the one being read and the wide one the one being worked in.
	long_columns: bool,
	// What the volume said about its own space, when it was asked. `None` when it would not say,
	// which is a volume that cannot answer rather than one with no room - and the difference is
	// worth showing.
	free_bytes: Option<u64>,
	// The first screenful of the previewed file, for the quick view.
	preview: Vec<u8>,
}

impl Panel {
	fn new(path: String) -> Panel {
		Panel { path, entries: Vec::new(), view: Vec::new(), selected: 0, sort: SortSpec::default(), filter: Vec::new(), history: History::new(), tags: Tags::new(), mode: PanelMode::List, results: None, depths: Vec::new(), long_columns: false, free_bytes: None, preview: Vec::new() }
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
		// A REBUILD IS A LIST. Tree mode re-walks after it, because the arrangement the depths
		// describe is one this rebuild has just thrown away - and leaving stale depths behind would
		// indent rows by an arrangement that no longer exists.
		self.depths.clear();
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
	SearchContent,
	QuickSearch,
	Filter,
}

// THE MENU IS THE SAME ACTIONS THE F-KEYS ARE, named once. Two tables would drift, and the way a
// menu drifts from its keys is that one of them quietly stops doing what its label says.
#[derive(Clone, Copy, Eq, PartialEq)]
enum MenuItem {
	View,
	Edit,
	Copy,
	Move,
	MakeDirectory,
	Delete,
	Find,
	Sort,
	Hidden,
	Mode,
	Columns,
	SaveSettings,
	Quit,
}

const MENU: &[(MenuItem, &[u8])] = &[
	(MenuItem::View, b"View          F3"),
	(MenuItem::Edit, b"Edit          F4"),
	(MenuItem::Copy, b"Copy          F5"),
	(MenuItem::Move, b"Move          F6"),
	(MenuItem::MakeDirectory, b"Make directory F7"),
	(MenuItem::Delete, b"Delete        F8"),
	(MenuItem::Find, b"Find files    M-F3"),
	(MenuItem::Sort, b"Sort order    M-s"),
	(MenuItem::Hidden, b"Hidden files  M-h"),
	(MenuItem::Mode, b"Panel mode    M-v"),
	(MenuItem::Columns, b"Long columns  M-c"),
	(MenuItem::SaveSettings, b"Save settings M-w"),
	(MenuItem::Quit, b"Quit          F10"),
];

// AN OPERATION IN PROGRESS. It is not a thread and does not pretend to be one: the manager advances
// it a few steps per turn of its own input loop, so the panels stay navigable, the progress is
// visible, and pause, resume and cancel are decisions the reader makes WHILE it runs rather than
// afterwards. A worker process would be the other shape, and it would need process authority this
// program is deliberately not given.
struct Job {
	plan: Plan,
	at: usize,
	done: usize,
	refused: usize,
	bytes: u64,
	paused: bool,
	operation: Operation,
	// WHY THE FIRST REFUSAL HAPPENED, kept because a count cannot be acted on. "1 refused" is the
	// same sentence for a full volume, a read-only one and a source that vanished, and only one of
	// those three is something the reader can fix in the next ten seconds. The FIRST is kept rather
	// than the last: it is the one that explains a run that went wrong from a particular point.
	reason: Option<&'static [u8]>,
}

impl Job {
	fn finished(&self) -> bool {
		self.at >= self.plan.steps.len()
	}
}

struct Manager {
	panels: [Panel; 2],
	focus: Focus,
	bookmarks: Bookmarks,
	bar: CommandBar,
	prompt: Prompt,
	// The name half of a search, held while the content half is being typed.
	pending_search: Vec<u8>,
	// The operation running now, if any.
	job: Option<Job>,
	// Which menu row is highlighted, or None when the menu is closed. A mode like the prompt, for
	// the same reason: one input loop and one set of exit paths.
	menu: Option<usize>,
	entry: Vec<u8>,
	status: Vec<u8>,
	settings: Settings,
	permission: u64,
	assets: u64,
	// The environment this manager was launched with, forwarded to what it launches. A child reads
	// what it inherited and cannot change what its parent or its session will see.
	environment: Vec<EnvVar>,
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
		// THE READS ARE IN THE ORDER THE LAUNCHER SENDS, and that order is `VOCABULARY`: Permission
		// before Volumes before AppAssets. It has to be, because `recv_tagged` BLOCKS and a tag read
		// out of turn consumes the message that was actually next - which is the ordered-bootstrap
		// hazard P02M0102 records, reached again here by reading three grants in the wrong sequence.
		//
		// THE NARROW LAUNCH BROKER AND NOTHING MORE. `lico` cannot create a process; it can ask
		// PermissionManager to start a NAMED program, which then runs under that program's own
		// manifest. Launching from a panel lends the child nothing, because this holds nothing it
		// could lend.
		let permission: u64 = recv_tagged(bootstrap, &mut bootstrap_buffer, b"PERMISSION").unwrap_or(0);
		let volumes = VolumeSet::receive(bootstrap, &mut bootstrap_buffer);
		// This application's own asset directory, where the settings live beside the descriptors.
		// Last in the vocabulary, so last here.
		let assets: u64 = recv_tagged(bootstrap, &mut bootstrap_buffer, CAP_APP_ASSETS).unwrap_or(0);
		let environment: Vec<EnvVar> = context.environment.clone();
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
				let _ = manage(terminal.writer(), &volumes, initial, permission, assets, environment);
			}
			// And back to cooked input and echo, through the same request path.
			if owns_tty {
				tty_set_mode(false, true);
			}
		}
	}
	exit();
}

#[allow(clippy::too_many_arguments)]
unsafe fn manage(output: &mut impl TerminalWriter, volumes: &VolumeSet, initial: String, permission: u64, assets: u64, environment: Vec<EnvVar>) -> bool {
	unsafe {
		// SETTINGS ARE READ BEFORE THE FIRST LISTING, so the panels come up ordered the way they
		// were left. A file that will not read, will not parse or names a version this build does
		// not know yields DEFAULTS - which is exactly what no file at all yields, and is why a
		// corrupt settings file can never stop the manager starting.
		let settings = load_settings(assets);
		let mut manager = Manager { panels: [Panel::new(initial.clone()), Panel::new(initial)], focus: Focus::new(2), bookmarks: Bookmarks::new(), bar: CommandBar::new(), prompt: Prompt::None, pending_search: Vec::new(), job: None, menu: None, entry: Vec::new(), status: Vec::new(), settings, permission, assets, environment };
		for panel in &mut manager.panels {
			panel.sort = SortSpec { key: settings.sort_key, reverse: settings.reverse, directories_first: settings.directories_first, show_hidden: settings.show_hidden };
		}
		manager.say(b"F1 help  F3 view  F4 edit  F5 copy  F6 move  F7 mkdir  F8 delete  F9 menu  F10 exit");
		for index in 0..2 {
			if let Err(error) = manager.panels[index].refresh(volumes) {
				manager.say(list_error(error));
			}
			manager.refresh_free_space(index, volumes);
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
				// AN INTERRUPT CANCELS THE JOB rather than the program, when one is running. What a
				// reader means by Ctrl+C over a running copy is "stop the copy", and leaving the
				// manager is what F10 is for.
				if manager.job.is_some() {
					manager.cancel_job(volumes);
					redraw = true;
					continue;
				}
				return true;
			}
			// A RUNNING JOB MUST NOT BLOCK THE LOOP. With one in progress the wait is a short
			// deadline rather than "until a key arrives", which is what keeps the panels navigable
			// while a copy runs - and with none it blocks, so an idle manager costs nothing.
			//
			// TICKS. The deadline `wait_any` takes is an ABSOLUTE LAPIC TICK COUNT, and this passed
			// `clock_ns()` - a different clock, reading nanoseconds. Seconds after boot that is a
			// tick number years away, so the wait never expired: a started operation advanced only
			// when a key happened to arrive, and one left alone made no progress at all. `rt` warns
			// about exactly this substitution, in as many words - "a hang that looks like a deadlock
			// rather than a wrong unit" - and this is what it looks like from the other side. The
			// governed `F8` test measures it: with `clock_ns()` here, the delete never completes.
			//
			// PERIODIC because this wake is HOUSEKEEPING rather than pending progress - a judgement
			// about what the wait MEANS, and not something that test distinguishes, since it passes
			// either way. A plain timed wait holds `run_until_idle` until its deadline, and the loop
			// driving the console polls the serial wire only BETWEEN those calls; a job re-arming a
			// deadline every tick would therefore keep typed input from being forwarded for as long
			// as it ran - including the interrupt that cancels it. A periodic waiter lets the system
			// settle each round and is still woken when it comes due.
			let ready = if manager.job.is_some() { wait_any_periodic(&[input], clock().saturating_add(1)) } else { wait_any(&[input], 0) };
			if interrupted() {
				if manager.job.is_some() {
					manager.cancel_job(volumes);
					redraw = true;
					continue;
				}
				return true;
			}
			if ready < 0 && manager.job.is_none() {
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
			if manager.advance_job(volumes) {
				redraw = true;
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
		if self.menu.is_some() {
			return self.apply_menu(event, volumes, output);
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
				self.menu = Some(0);
				self.say(b"arrows choose, Enter invokes, Esc closes");
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
			InputEvent::Key(Key::Control(0x10)) => {
				match self.job.as_mut() {
					Some(job) => {
						job.paused = !job.paused;
						let paused = job.paused;
						self.say(if paused { b"paused - ^P resumes, ^C cancels" } else { b"resumed" });
					}
					None => self.say(b"nothing is running"),
				}
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
				self.panels[active].rebuild();
				self.build_tree(active, volumes);
				self.refresh_preview(volumes);
				ManagerAction::Redraw
			}
			InputEvent::Chord(Chord { key: Key::Byte(b'c'), alt: true, .. }) => {
				self.panels[active].long_columns = !self.panels[active].long_columns;
				ManagerAction::Redraw
			}
			InputEvent::Chord(Chord { key: Key::Byte(b'f'), alt: true, .. }) => self.open_prompt(Prompt::Filter, b"filter: "),
			InputEvent::Chord(Chord { key: Key::Function(3), alt: true, .. }) => self.open_prompt(Prompt::Search, b"find files matching (glob, then -d N -f -D -s N -S N): "),
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

	// The menu, driven by the same events everything else is.
	fn apply_menu(&mut self, event: InputEvent, volumes: &VolumeSet, output: &mut impl TerminalWriter) -> ManagerAction {
		let Some(at) = self.menu else { return ManagerAction::None };
		match event {
			InputEvent::Key(Key::Escape) | InputEvent::Key(Key::Function(9)) => {
				self.menu = None;
				self.say(b"");
				ManagerAction::Redraw
			}
			InputEvent::Key(Key::ArrowUp) => {
				self.menu = Some(if at == 0 { MENU.len() - 1 } else { at - 1 });
				ManagerAction::Redraw
			}
			InputEvent::Key(Key::ArrowDown) => {
				self.menu = Some((at + 1) % MENU.len());
				ManagerAction::Redraw
			}
			// A CLICK ON A ROW CHOOSES IT AND INVOKES IT, which is what "the same actions through
			// pointer input" means - a pointer that could only highlight would be a slower keyboard.
			InputEvent::Pointer(pointer) if pointer.pressed => {
				let row = pointer.row.saturating_sub(3) as usize;
				if row < MENU.len() {
					self.menu = Some(row);
					return self.invoke_menu(volumes, output);
				}
				ManagerAction::Redraw
			}
			InputEvent::Key(Key::Enter) => self.invoke_menu(volumes, output),
			_ => ManagerAction::None,
		}
	}

	// Invoke the highlighted row. EVERY ARM CALLS THE SAME METHOD ITS F-KEY DOES, so the menu
	// cannot come to mean something different from the key beside it in the label.
	fn invoke_menu(&mut self, volumes: &VolumeSet, output: &mut impl TerminalWriter) -> ManagerAction {
		let Some(at) = self.menu.take() else { return ManagerAction::None };
		let active = self.active();
		match MENU[at].0 {
			MenuItem::View => self.open_selected(Action::View, volumes, output),
			MenuItem::Edit => self.open_selected(Action::Edit, volumes, output),
			MenuItem::Copy => self.open_prompt(Prompt::CopyTo, b"copy to: "),
			MenuItem::Move => self.open_prompt(Prompt::MoveTo, b"move to: "),
			MenuItem::MakeDirectory => self.open_prompt(Prompt::MakeDirectory, b"new directory: "),
			MenuItem::Delete => self.confirm_delete(),
			MenuItem::Find => self.open_prompt(Prompt::Search, b"find files matching (glob, then -d N -f -D -s N -S N): "),
			MenuItem::Sort => {
				self.panels[active].cycle_sort();
				ManagerAction::Redraw
			}
			MenuItem::Hidden => {
				self.panels[active].sort.show_hidden = !self.panels[active].sort.show_hidden;
				self.panels[active].rebuild();
				self.build_tree(active, volumes);
				ManagerAction::Redraw
			}
			MenuItem::Mode => {
				self.panels[active].cycle_mode();
				self.panels[active].rebuild();
				self.build_tree(active, volumes);
				self.refresh_preview(volumes);
				ManagerAction::Redraw
			}
			MenuItem::Columns => {
				self.panels[active].long_columns = !self.panels[active].long_columns;
				ManagerAction::Redraw
			}
			MenuItem::SaveSettings => self.save_settings(),
			MenuItem::Quit => ManagerAction::Exit,
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
			Prompt::Search => {
				// THE NAME FILTERS FIRST, THEN THE TEXT. Two prompts rather than one line with both
				// in it, because a content pattern is free text and would need quoting to sit
				// beside flags - and a search line that needs quoting is one people get wrong.
				self.pending_search = entry;
				self.open_prompt(Prompt::SearchContent, b"and containing (blank for any): ")
			}
			Prompt::SearchContent => {
				let line = core::mem::take(&mut self.pending_search);
				self.search(volumes, &line, &entry)
			}
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
		// ONE AT A TIME. Two operations over the same tree would race each other, and a queue is a
		// promise this cannot keep while the reader can still tag and retarget between them.
		if self.job.is_some() {
			self.say(b"an operation is already running - ^C cancels it first");
			return ManagerAction::Redraw;
		}
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
		// THE TREE IS WALKED BEFORE ANYTHING IS TOUCHED, so a recursive operation knows its whole
		// extent - and its refusals - while the volumes are still untouched. Directories come out
		// before their contents, which is the order a copy needs; a delete is reversed afterwards,
		// because removing a directory before its contents is a removal that fails.
		let mut planned = planned;
		if !self.walk_into_directories(volumes, &mut planned) {
			self.say(b"that tree is larger than one operation will carry - nothing was changed");
			return ManagerAction::Redraw;
		}
		if operation == Operation::Delete {
			deepest_first(&mut planned);
		}
		// STARTED, NOT RUN. The steps are advanced a few at a time by the input loop, so the panels
		// stay navigable and pause, resume and cancel are decisions the reader makes while it runs.
		self.job = Some(Job { plan: planned, at: 0, done: 0, refused: 0, bytes: 0, paused: false, operation, reason: None });
		self.say(b"^P pauses, ^C cancels - what is done stays done and what is not started is untouched");
		ManagerAction::Redraw
	}

	// Advance the running job by a few steps. True when anything changed, so the caller redraws.
	//
	// A FEW AT A TIME rather than one: an entry can be a rename, which is fast, and redrawing after
	// every one of those would spend more time on the screen than on the work.
	fn advance_job(&mut self, volumes: &VolumeSet) -> bool {
		let Some(job) = self.job.as_mut() else { return false };
		if job.paused {
			return false;
		}
		if job.finished() {
			let (done, refused, reason) = (job.done, job.refused, job.reason);
			self.job = None;
			let mut message: Vec<u8> = Vec::new();
			if message.try_reserve(112).is_ok() {
				append_decimal(&mut message, done);
				message.extend_from_slice(b" done, ");
				append_decimal(&mut message, refused);
				message.extend_from_slice(b" refused");
				// THE TAIL ONLY WHEN THERE WAS A REFUSAL, and it names the reason.
				//
				// It used to be one fixed sentence, appended whether anything was refused or not,
				// and at 89 bytes against this line's 80 it was CUT MID-WORD - so the safety
				// statement it existed to make, that a refusal leaves both sides as they were,
				// reached the screen as "as t". The reason plus the short form of that promise fits.
				if let Some(reason) = reason {
					message.extend_from_slice(b" - ");
					message.extend_from_slice(reason);
					message.extend_from_slice(b", nothing was half-written");
				}
				self.status = message;
			}
			for index in 0..2 {
				let _ = self.panels[index].refresh(volumes);
			}
			return true;
		}
		let operation = job.operation;
		for _ in 0..STEPS_PER_TURN {
			let Some(job) = self.job.as_mut() else { return true };
			if job.finished() {
				break;
			}
			let step = &job.plan.steps[job.at];
			let (source, destination, is_dir, size) = (step.source.clone(), step.destination.clone(), step.is_dir, step.size);
			let outcome = unsafe {
				match operation {
					Operation::Delete => remove_entry(volumes, &source, is_dir),
					Operation::Copy => copy_entry(volumes, &source, &destination, is_dir),
					Operation::Move => move_entry(volumes, &source, &destination, is_dir),
				}
			};
			let Some(job) = self.job.as_mut() else { return true };
			match outcome {
				Ok(()) => {
					job.done += 1;
					job.bytes = job.bytes.saturating_add(size);
				}
				Err(reason) => {
					job.refused += 1;
					job.reason.get_or_insert(reason);
				}
			}
			job.at += 1;
		}
		true
	}

	// Stop the job where it is. WHAT IS DONE STAYS DONE and what was not started is untouched -
	// there is no half-finished entry, because cancellation lands between steps and every step
	// publishes or does not.
	fn cancel_job(&mut self, volumes: &VolumeSet) {
		let Some(job) = self.job.take() else { return };
		let mut message: Vec<u8> = Vec::new();
		if message.try_reserve(112).is_ok() {
			message.extend_from_slice(b"cancelled after ");
			append_decimal(&mut message, job.done);
			message.extend_from_slice(b" of ");
			append_decimal(&mut message, job.plan.steps.len());
			message.extend_from_slice(b" - what was done is complete and what was not started is untouched");
			self.status = message;
		}
		for index in 0..2 {
			let _ = self.panels[index].refresh(volumes);
		}
	}

	// Expand every directory in the plan into its contents, breadth first, until nothing is left to
	// open. False when the tree is past a bound - and then NOTHING is done, because a plan that
	// covered part of a tree would report success over work it did not do.
	fn walk_into_directories(&mut self, volumes: &VolumeSet, planned: &mut Plan) -> bool {
		let mut at = 0;
		let mut depth = 1;
		while at < planned.steps.len() {
			if unsafe { interrupted() } {
				return true;
			}
			if !planned.steps[at].is_dir {
				at += 1;
				continue;
			}
			let parent = Step { source: planned.steps[at].source.clone(), destination: planned.steps[at].destination.clone(), is_dir: true, size: 0 };
			let Ok(uri) = core::str::from_utf8(&parent.source) else {
				at += 1;
				continue;
			};
			let storage = volumes.client_for(uri, uri.as_bytes());
			let Ok(entries) = (unsafe { list_volume_directory(storage, uri, MAX_PANEL_ENTRIES) }) else {
				at += 1;
				continue;
			};
			let mut children: Vec<Vec<u8>> = Vec::new();
			let mut names: Vec<Vec<u8>> = Vec::new();
			let mut shapes: Vec<(bool, u64)> = Vec::new();
			for entry in &entries {
				let Some(child) = join(&parent.source, entry.name.as_bytes()) else { continue };
				let mut name: Vec<u8> = Vec::new();
				if name.try_reserve_exact(entry.name.len()).is_err() || children.try_reserve(1).is_err() || names.try_reserve(1).is_err() || shapes.try_reserve(1).is_err() {
					return false;
				}
				name.extend_from_slice(entry.name.as_bytes());
				children.push(child);
				names.push(name);
				shapes.push((entry.r#type == FileType::Dir, entry.size));
			}
			let mut sources: Vec<Source> = Vec::new();
			if sources.try_reserve_exact(children.len()).is_err() {
				return false;
			}
			for index in 0..children.len() {
				sources.push(Source { path: &children[index], name: &names[index], is_dir: shapes[index].0, size: shapes[index].1 });
			}
			if expand(planned, &parent, &sources, depth).is_err() {
				return false;
			}
			depth += 1;
			at += 1;
		}
		true
	}

	// Build the tree view for a panel: the directory's own entries, and below each directory the
	// entries inside it, indented.
	//
	// LAZY AND BOUNDED. Only directories the reader has EXPANDED are walked - which for this first
	// slice is every directory at depth one and no further, so entering tree mode costs one listing
	// per subdirectory rather than a walk of the whole volume. The frontier is the same explicit,
	// bounded one the search uses, because a recursive walk over somebody else's tree is a stack
	// overflow waiting for a deep enough directory.
	fn build_tree(&mut self, panel: usize, volumes: &VolumeSet) {
		if self.panels[panel].mode != PanelMode::Tree || self.panels[panel].results.is_some() {
			return;
		}
		let base = self.panels[panel].path.clone();
		let rows: Vec<usize> = self.panels[panel].view.clone();
		let mut view: Vec<usize> = Vec::new();
		let mut depths: Vec<usize> = Vec::new();
		let mut extra: Vec<FileInfo> = Vec::new();
		for row in rows {
			if view.try_reserve(1).is_err() || depths.try_reserve(1).is_err() {
				return;
			}
			view.push(row);
			depths.push(0);
			if self.panels[panel].entries[row].r#type != FileType::Dir {
				continue;
			}
			let Some(child_uri) = join(base.as_bytes(), self.panels[panel].entries[row].name.as_bytes()) else { continue };
			let Ok(child_uri) = core::str::from_utf8(&child_uri) else { continue };
			let storage = volumes.client_for(child_uri, child_uri.as_bytes());
			let Ok(children) = (unsafe { list_volume_directory(storage, child_uri, MAX_PANEL_ENTRIES) }) else { continue };
			for child in children {
				let key = Panel::key_of(&child);
				if !self.panels[panel].sort.admits(&key) {
					continue;
				}
				if extra.try_reserve(1).is_err() || view.try_reserve(1).is_err() || depths.try_reserve(1).is_err() {
					return;
				}
				view.push(self.panels[panel].entries.len() + extra.len());
				depths.push(1);
				extra.push(child);
			}
		}
		if self.panels[panel].entries.try_reserve(extra.len()).is_err() {
			return;
		}
		for entry in extra {
			self.panels[panel].entries.push(entry);
		}
		self.panels[panel].view = view;
		self.panels[panel].depths = depths;
		if self.panels[panel].selected >= self.panels[panel].view.len() {
			self.panels[panel].selected = self.panels[panel].view.len().saturating_sub(1);
		}
	}

	// Ask the volume how much room it has. `None` is a volume that would not say, which is not the
	// same as one with no room and is shown differently.
	fn refresh_free_space(&mut self, panel: usize, volumes: &VolumeSet) {
		let uri = self.panels[panel].path.clone();
		let storage = volumes.client_for(&uri, uri.as_bytes());
		self.panels[panel].free_bytes = match VolumeClient::new(storage).status() {
			Some(Ok(status)) => Some(status.free_bytes),
			_ => None,
		};
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
	fn search(&mut self, volumes: &VolumeSet, line: &[u8], content: &[u8]) -> ManagerAction {
		let (pattern, mut criteria) = match parse_criteria(line) {
			Ok(parsed) => parsed,
			Err(error) => {
				self.say(criteria_error(error));
				return ManagerAction::Redraw;
			}
		};
		if pattern.is_empty() {
			self.say(b"a search needs a pattern - `*.rs -d 3 -f` is a glob and its filters");
			return ManagerAction::Redraw;
		}
		let active = self.active();
		criteria.name = Some(&pattern);
		// A CONTENT SEARCH ONLY EVER MATCHES FILES, whatever the name filter said: a directory has
		// no bytes to hold the text, and reporting one would be answering a different question.
		if !content.is_empty() {
			criteria.files_only = true;
		}
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
				if criteria.admits(&key, depth + 1) && (content.is_empty() || unsafe { file_contains(volumes, &child, content) }) {
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
		// ONLY AN EDIT ASKS FOR A WRITE-BACK. A viewer is opened over a grant that StorageService
		// will refuse a writer on, so "the viewer remains read-only" is enforced a service away
		// rather than being a property of the viewer's own good behaviour.
		let writable = association.action == Action::Edit && wanted == Action::Edit;
		self.launch_over_file(output, program, &uri, writable)
	}

	// Launch a program over ONE file, with an attenuated grant instead of a volume bundle.
	//
	// The broker mints the grant; this names the file and whether it may be written. The target
	// gets neither this panel's volumes nor permission to reopen a sibling path, which is the whole
	// reason the op exists - handing over a URI and a five-volume bundle would give a viewer every
	// file on every mounted volume in order to show one.
	fn launch_over_file(&mut self, output: &mut impl TerminalWriter, program: &str, uri: &str, writable: bool) -> ManagerAction {
		if self.permission == 0 {
			self.say(b"this boot granted no launch broker, so nothing can be opened from here");
			return ManagerAction::Redraw;
		}
		let Some((read_end, write_end)) = (unsafe { channel() }) else {
			self.say(b"could not make an output channel");
			return ManagerAction::Redraw;
		};
		let cwd = self.panels[self.active()].path.clone();
		let mut client = PermissionClient::new(self.permission);
		let started = matches!(client.run_with_file(program, "", &cwd, uri, &writable, &write_end), Some(Ok(_)));
		if !started {
			unsafe { close(read_end) };
			self.say(b"the broker refused to open that file with that program");
			return ManagerAction::Redraw;
		}
		self.hand_over_terminal(output, read_end)
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
		// THE ENVIRONMENT IS FORWARDED, not invented. What this manager inherited is what it hands
		// on, so a command run from the bar sees the same variables one typed at the shell would -
		// and the child holds the values with no capability to the session, so it can read what it
		// inherited and cannot change what its parent will see.
		// Bound rather than discarded so it lives as long as it used to: this scope hands the
		// terminal over below and the launch must not be dropped before that.
		let _started = match client.run(program, args, &cwd, &self.environment, &write_end) {
			Some(Ok(started)) => started,
			_ => {
				unsafe { close(read_end) };
				self.say(b"the broker refused that program - it may not be a governed executable");
				return ManagerAction::Redraw;
			}
		};
		if !foreground {
			// A BACKGROUND COMMAND BECOMES A SESSION JOB, so `jobs` and `fg` can see it and
			// something eventually reaps it. Without that it would be a process nobody is tracking,
			// which is the thing an `&` must not quietly produce.
			unsafe { close(read_end) };
			self.say(b"started in the background; it is not a session job, because nothing sends this program a session grant - see the note in the milestone");
			return ManagerAction::Redraw;
		}
		self.hand_over_terminal(output, read_end)
	}

	// THE FOREGROUND COMMAND OWNS THE SCREEN AND GIVES IT BACK. The panels are left through the
	// alternate screen so what the command writes lands on the ordinary terminal, and the exact
	// panel screen is redrawn on return. One implementation, because a command started from the
	// bar and a file opened by association are the same thing to the terminal.
	fn hand_over_terminal(&mut self, output: &mut impl TerminalWriter, read_end: u64) -> ManagerAction {
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
				self.build_tree(active, volumes);
				self.refresh_free_space(active, volumes);
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
		// THE F-KEY ROW IS CLICKABLE. It is drawn as labels with their keys on it, and a label that
		// looks like a button and is not is the thing a pointer user tries first.
		if row as usize == PANEL_ROWS + 4 {
			let slot = (column as usize) / 9;
			let key = [1u8, 3, 4, 5, 6, 7, 8, 9, 10];
			if let Some(function) = key.get(slot) {
				return self.apply(InputEvent::Key(Key::Function(*function)), volumes, &mut NullWriter);
			}
		}
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

// A writer that goes nowhere, for the pointer path: clicking `F3` chooses the action, and the
// action that needs the terminal takes it from the caller a moment later. Handing the click the
// real writer would mean a pointer press could enter and leave the alternate screen inside an
// event handler, which is a screen state nothing else in the loop expects.
struct NullWriter;

impl TerminalWriter for NullWriter {
	fn write(&mut self, _bytes: &[u8]) -> bool {
		true
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

// Copy one entry through the transactional writer. A directory becomes a directory and its
// contents are separate steps of the same plan - the walk expanded them before anything was
// touched, so this never has to decide what is inside one.
// THE VOLUME'S ANSWER IN WORDS A PERSON CAN ACT ON, which is the whole reason these codes are
// carried this far. The distinctions decide what the reader does next: "full" is fixed by deleting
// something, "would not take it" cannot be fixed from here at all, and "damaged" is a reason to
// stop copying anything else off that volume. Until IDL-006 several of them arrived as `again` -
// "try again" - which is advice that leads nowhere on a volume with no room left.
fn volume_words(error: Error) -> &'static [u8] {
	match error {
		Error::NoSpace => b"the volume is full",
		Error::Denied => b"the volume would not take it",
		Error::NotFound => b"it is not there any more",
		Error::Corrupt => b"the volume is damaged",
		Error::Io => b"the medium failed",
		Error::CommitUncertain => b"the volume stopped taking writes",
		Error::Exhausted => b"the volume service has no room to work",
		Error::Again => b"the volume was busy",
		Error::TimedOut => b"the volume did not answer in time",
		Error::Closed => b"the volume closed the connection",
		Error::Cancelled => b"the volume cancelled it",
		Error::Invalid | Error::Unsupported => b"the volume refused it",
	}
}

// The same, for a call that produced no answer at all: `None` is the transport failing, which is a
// different thing from the volume saying no and is worth its own words.
fn answer_words(answer: Option<Result<(), Error>>) -> Result<(), &'static [u8]> {
	match answer {
		Some(Ok(())) => Ok(()),
		Some(Err(error)) => Err(volume_words(error)),
		None => Err(b"the volume did not answer"),
	}
}

// A STEP'S OUTCOME, not a yes-or-no. Each of these used to return `bool`, so a job could count its
// refusals and never say what any of them was; the reason is available at every one of these call
// sites and was being dropped one line after it arrived.
unsafe fn copy_entry(volumes: &VolumeSet, source: &[u8], destination: &[u8], is_dir: bool) -> Result<(), &'static [u8]> {
	let (Ok(source), Ok(destination)) = (core::str::from_utf8(source), core::str::from_utf8(destination)) else {
		return Err(b"that name is not text");
	};
	if is_dir {
		let storage = volumes.client_for(destination, destination.as_bytes());
		return answer_words(VolumeClient::new(storage).mkdir(destination));
	}
	let reader = volumes.client_for(source, source.as_bytes());
	let mut writer_client = VolumeClient::new(volumes.client_for(destination, destination.as_bytes()));
	let mut writer = match writer_client.open_writer(destination, WriterMode::Replace) {
		Some(Ok(writer)) => writer,
		Some(Err(error)) => return Err(volume_words(error)),
		None => return Err(b"the volume did not answer"),
	};
	let mut offset: u64 = 0;
	loop {
		let window = match unsafe { read_volume_window(reader, source, offset, COPY_CHUNK) } {
			Ok(window) => window,
			Err(_) => {
				let _ = writer.abort();
				unsafe { close(writer.handle()) };
				return Err(b"the source could not be read");
			}
		};
		if window.is_empty() {
			break;
		}
		offset += window.len() as u64;
		// THE STEP THAT REPORTS A FULL VOLUME. A write is where the destination runs out of room,
		// and the abort below is what keeps that from leaving a partial file behind.
		//
		// The read window is larger than one request may carry, so it goes in `WRITER_CHUNK`
		// pieces - the protocol's own bound, named rather than guessed. This wrote the whole
		// window in one call, which is a size the service cannot receive.
		for piece in window.chunks(WRITER_CHUNK) {
			if let Err(reason) = answer_words(writer.write(piece).map(|answer| answer.map(|_| ()))) {
				let _ = writer.abort();
				unsafe { close(writer.handle()) };
				return Err(reason);
			}
		}
	}
	// NOTHING IS VISIBLE UNDER THE DESTINATION'S NAME UNTIL HERE, so a copy that failed at any
	// point above leaves whatever was there before exactly as it was.
	let committed = answer_words(writer.commit().map(|answer| answer.map(|_| ())));
	unsafe { close(writer.handle()) };
	committed
}

// Move: `rename` within a volume, copy-then-remove across one - and the source is removed only
// after the destination has been published, so an interruption leaves two files rather than none.
unsafe fn move_entry(volumes: &VolumeSet, source: &[u8], destination: &[u8], is_dir: bool) -> Result<(), &'static [u8]> {
	let (Ok(source_text), Ok(destination_text)) = (core::str::from_utf8(source), core::str::from_utf8(destination)) else {
		return Err(b"that name is not text");
	};
	if same_volume(source, destination) {
		let storage = volumes.client_for(source_text, source);
		if matches!(VolumeClient::new(storage).rename(source_text, destination_text), Some(Ok(_))) {
			return Ok(());
		}
	}
	// A rename that did not take is not reported: the copy-then-remove below is the answer for
	// every cross-volume move, and it reaches the same destination by the long road. What it
	// reports is what the reader sees.
	unsafe {
		copy_entry(volumes, source, destination, is_dir)?;
		remove_entry(volumes, source, is_dir)
	}
}

unsafe fn remove_entry(volumes: &VolumeSet, target: &[u8], is_dir: bool) -> Result<(), &'static [u8]> {
	let Ok(uri) = core::str::from_utf8(target) else { return Err(b"that name is not text") };
	let storage = volumes.client_for(uri, target);
	let mut client = VolumeClient::new(storage);
	let answer = if is_dir { client.rmdir(uri) } else { client.remove(uri) };
	answer_words(answer)
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

// Whether a file holds `needle`, read in bounded windows that OVERLAP by the needle's length minus
// one - so a match straddling a window boundary is still found, which is the whole difficulty of
// searching a file you are not holding.
unsafe fn file_contains(volumes: &VolumeSet, uri: &[u8], needle: &[u8]) -> bool {
	let Ok(uri) = core::str::from_utf8(uri) else { return false };
	if needle.is_empty() || needle.len() > CONTENT_CHUNK as usize {
		return false;
	}
	let storage = volumes.client_for(uri, uri.as_bytes());
	let overlap = needle.len() - 1;
	let mut offset: u64 = 0;
	let mut carry: Vec<u8> = Vec::new();
	loop {
		if unsafe { interrupted() } {
			return false;
		}
		let Ok(window) = (unsafe { read_volume_window(storage, uri, offset, CONTENT_CHUNK) }) else { return false };
		if window.is_empty() {
			return false;
		}
		offset += window.len() as u64;
		let mut span: Vec<u8> = Vec::new();
		if span.try_reserve_exact(carry.len() + window.len()).is_err() {
			return false;
		}
		span.extend_from_slice(&carry);
		span.extend_from_slice(&window);
		if span.windows(needle.len()).any(|candidate| candidate == needle) {
			return true;
		}
		carry.clear();
		let tail = span.len().saturating_sub(overlap);
		if carry.try_reserve_exact(span.len() - tail).is_err() {
			return false;
		}
		carry.extend_from_slice(&span[tail..]);
	}
}

fn criteria_error(error: CriteriaError) -> &'static [u8] {
	match error {
		CriteriaError::UnknownFlag => b"that line has a flag this search does not know - `-d N -f -D -s N -S N -t N -T N`",
		CriteriaError::MissingValue => b"that flag takes a number and was not given one",
		CriteriaError::NotANumber => b"that is not a number",
		CriteriaError::Contradictory => b"`-f` and `-D` together admit nothing at all",
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
	if let Some(at) = manager.menu {
		for (row, (_, label)) in MENU.iter().enumerate() {
			rendered.push(if row == at { b'>' } else { b' ' });
			append_safe(&mut rendered, label, PANEL_WIDTH);
			rendered.push(b'\n');
		}
		for _ in MENU.len()..PANEL_ROWS {
			rendered.push(b'\n');
		}
		rendered.extend_from_slice(b"\nF9/Esc closes the menu\n");
		append_safe(&mut rendered, &manager.status, PANEL_WIDTH * 2 + 4);
		return output.write(&rendered);
	}
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
	// THE PROGRESS IS ON THE SCREEN WHILE IT RUNS, with the entry being worked on: a copy that shows
	// only a count says nothing about which file it is stuck on.
	if let Some(job) = manager.job.as_ref() {
		rendered.extend_from_slice(match job.operation {
			Operation::Copy => b"copy ",
			Operation::Move => b"move ",
			Operation::Delete => b"delete ",
		});
		append_decimal(&mut rendered, job.at);
		rendered.push(b'/');
		append_decimal(&mut rendered, job.plan.steps.len());
		rendered.extend_from_slice(b"  ");
		append_units(&mut rendered, job.bytes);
		if job.plan.total_bytes > 0 {
			rendered.push(b'/');
			append_units(&mut rendered, job.plan.total_bytes);
			if job.plan.total_is_partial {
				rendered.push(b'+');
			}
		}
		rendered.extend_from_slice(if job.paused { b"  PAUSED  " } else { b"  " });
		if let Some(step) = job.plan.steps.get(job.at) {
			append_safe(&mut rendered, &step.source, PANEL_WIDTH);
		}
		rendered.push(b'\n');
	}
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
	// FREE SPACE, and a volume that would not say is shown as `?` rather than as zero - a volume
	// that cannot answer and one with no room are different facts and a reader acts differently on
	// each.
	output.push(b' ');
	match panel.free_bytes {
		Some(free) => append_units(output, free),
		None => output.push(b'?'),
	}
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
	// TREE ROWS ARE INDENTED BY THEIR DEPTH, which is what makes a walked subtree readable as one
	// rather than as a flat list with duplicate names in it.
	let indent = if panel.mode == PanelMode::Tree { panel.depths.get(index).copied().unwrap_or(0) * 2 } else { 0 };
	pad(output, indent);
	if panel.mode == PanelMode::Tree {
		output.extend_from_slice(if entry.r#type == FileType::Dir { b"+" } else { b" " });
	}
	// CONCISE SHOWS A SIZE; LONG SHOWS A SIZE AND A TIME. The narrow panel is usually the one being
	// read and the wide one the one being worked in, so the choice is per panel.
	let tail = if panel.long_columns { 20 } else { 12 };
	let name_width = PANEL_WIDTH.saturating_sub(tail + indent + 2);
	let before = output.len();
	append_safe(output, entry.name.as_bytes(), name_width);
	if entry.r#type == FileType::Dir {
		output.push(b'/');
	}
	let written = output.len() - before;
	if written < name_width + 1 {
		pad(output, name_width + 1 - written);
	}
	append_units(output, entry.size);
	if panel.long_columns {
		output.push(b' ');
		append_decimal(output, entry.mtime as usize);
	}
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

// A size a person can read at a glance: bytes below a kilobyte, then K, M, G. BINARY units, and
// the suffix is what says so - a listing that showed decimal kilobytes beside a volume reporting
// binary ones would disagree with itself by seven per cent.
fn append_units(output: &mut Vec<u8>, value: u64) {
	const SUFFIX: [u8; 4] = [b'B', b'K', b'M', b'G'];
	let mut scaled = value;
	let mut step = 0;
	while scaled >= 10_000 && step + 1 < SUFFIX.len() {
		scaled /= 1024;
		step += 1;
	}
	append_decimal(output, scaled as usize);
	output.push(SUFFIX[step]);
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
