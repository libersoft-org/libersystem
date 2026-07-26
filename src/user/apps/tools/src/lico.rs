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
use lico::{Focus, InputDecoder, InputEvent, Key, MouseTracking, TerminalGuard, TerminalOptions, TerminalWriter};
use proto::system::{FileInfo, FileType};
use rt::*;
use storage_proto::path;
use tools::{ConsoleWriter, ListDirectoryError, VolumeSet, list_volume_directory};
use volume_client_provider as _;

const PANEL_WIDTH: usize = 38;
const PANEL_ROWS: usize = 16;
const MAX_PANEL_ENTRIES: usize = 4_096;

struct Panel {
	path: String,
	entries: Vec<FileInfo>,
	selected: usize,
}

impl Panel {
	fn new(path: String) -> Panel {
		Panel { path, entries: Vec::new(), selected: 0 }
	}

	fn refresh(&mut self, volumes: &VolumeSet) -> Result<(), ListDirectoryError> {
		let storage = volumes.client_for(&self.path, self.path.as_bytes());
		let mut entries = unsafe { list_volume_directory(storage, &self.path, MAX_PANEL_ENTRIES)? };
		entries.sort_by(|left, right| {
			let left_dir = left.r#type == FileType::Dir;
			let right_dir = right.r#type == FileType::Dir;
			right_dir.cmp(&left_dir).then_with(|| left.name.cmp(&right.name))
		});
		self.entries = entries;
		if self.entries.is_empty() {
			self.selected = 0;
		} else if self.selected >= self.entries.len() {
			self.selected = self.entries.len() - 1;
		}
		Ok(())
	}

	fn move_up(&mut self) -> bool {
		if self.selected == 0 {
			return false;
		}
		self.selected -= 1;
		true
	}

	fn move_down(&mut self) -> bool {
		if self.selected + 1 >= self.entries.len() {
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
		let Some(last) = self.entries.len().checked_sub(1) else { return false };
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
		let argument: Vec<u8> = match recv_blocking(bootstrap, &mut bootstrap_buffer) {
			Received::Message { len, .. } => bootstrap_buffer[..len].to_vec(),
			Received::Closed => exit(),
		};
		let volumes = VolumeSet::receive(bootstrap, &mut bootstrap_buffer);
		let cwd: Vec<u8> = match recv_blocking(bootstrap, &mut bootstrap_buffer) {
			Received::Message { len, .. } => bootstrap_buffer[..len].to_vec(),
			Received::Closed => Vec::new(),
		};
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
					print(b"lico: invalid path\n");
					exit();
				}
			}
		};
		if stdin() == 0 || stdout() == 0 {
			print(b"lico: interactive terminal unavailable\n");
		} else {
			catch_interrupt();
			let mut output = ConsoleWriter::new(stdout());
			let options = TerminalOptions { alternate_screen: true, raw_input: true, disable_echo: true, hide_cursor: true, mouse: MouseTracking::Press, bracketed_paste: false };
			if let Some(mut terminal) = TerminalGuard::enter(&mut output, options) {
				let _ = manage(terminal.writer(), &volumes, initial);
			}
		}
	}
	exit();
}

unsafe fn manage(output: &mut impl TerminalWriter, volumes: &VolumeSet, initial: String) -> bool {
	unsafe {
		let mut panels = [Panel::new(initial.clone()), Panel::new(initial)];
		let mut focus = Focus::new(2);
		let mut status: &[u8] = b"Tab/pointer panel  arrows select  Enter directory  Backspace parent  F10 exit";
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
							match apply_event(event, &mut panels, &mut focus, volumes, &mut status) {
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

fn apply_event(event: InputEvent, panels: &mut [Panel; 2], focus: &mut Focus, volumes: &VolumeSet, status: &mut &[u8]) -> ManagerAction {
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
			*status = b"F1 help  F3 view  F4 edit  F5 copy  F6 move  F7 mkdir  F8 delete  F10 exit";
			ManagerAction::Redraw
		}
		InputEvent::Key(Key::Function(3)) | InputEvent::Key(Key::Function(4)) | InputEvent::Key(Key::Function(5)) | InputEvent::Key(Key::Function(6)) | InputEvent::Key(Key::Function(7)) | InputEvent::Key(Key::Function(8)) => {
			*status = b"selected-file actions require a scoped file grant and transactional writer";
			ManagerAction::Redraw
		}
		InputEvent::Pointer(pointer) if pointer.pressed => pointer_focus(pointer.column, pointer.row, panels, focus),
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
	let Some(entry) = panel.entries.get(panel.selected) else { return ManagerAction::None };
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

fn pointer_focus(column: u16, row: u16, panels: &mut [Panel; 2], focus: &mut Focus) -> ManagerAction {
	let panel = usize::from(column > PANEL_WIDTH as u16 + 2);
	focus.select(panel as u16);
	let row = row.saturating_sub(3) as usize;
	if row < PANEL_ROWS {
		let start = visible_start(&panels[panel]);
		if start + row < panels[panel].entries.len() {
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
	append_safe(output, panel.path.as_bytes(), PANEL_WIDTH - 1);
	let used = output.len() - start;
	if used < PANEL_WIDTH {
		pad(output, PANEL_WIDTH - used);
	}
}

fn render_entry(output: &mut Vec<u8>, panel: &Panel, index: usize, active: bool) {
	let row_start = output.len();
	if let Some(entry) = panel.entries.get(index) {
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
	if panel.entries.len() <= PANEL_ROWS { 0 } else { panel.selected.saturating_sub(PANEL_ROWS / 2).min(panel.entries.len() - PANEL_ROWS) }
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
		ListDirectoryError::TooManyEntries => b"directory exceeds the 4096-entry panel bound",
		ListDirectoryError::OutOfMemory => b"not enough memory for the directory listing",
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
