// less - a text pager: show a file (or a pipeline's output) one screen at a time.
//
// IT DOES NOT LOAD THE FILE. What it holds is an INDEX - the byte offset of every line - and one
// screen's worth of bytes at a time, read back through the bounded window every other volume tool
// uses. A pager that read the whole file into memory would be a pager that fails on exactly the
// file somebody reaches for a pager to read, and the index costs eight bytes a line against the
// line itself.
//
// THAT IS ALSO WHY BACKWARD MOVEMENT IS CHEAP. A pager that could only go forward would be `head`
// with extra steps; going back needs either the whole file or a way to find a line again, and the
// index is that way - it is built once on the way through and every later seek is arithmetic.
//
// A PIPELINE'S OUTPUT IS DIFFERENT AND IS TREATED DIFFERENTLY. A stream cannot be re-read, so when
// `less` is given no path it drains its input into memory first, bounded, and pages that. The
// difference is stated to the user in the status line rather than hidden: a truncated stream is not
// a short file, and a pager that silently showed the first megabyte of a longer stream would be
// lying about what the command produced.
//
// EVERY EXIT PATH RESTORES THE TERMINAL - normal quit, Ctrl+C, a console that went away - through
// `TerminalGuard`, which is the shared implementation rather than this program's own idea of what
// needs undoing.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use lico::{InputDecoder, InputEvent, Key, MouseTracking, TerminalGuard, TerminalOptions, TerminalWriter};
use proto::system::LaunchContext;
use rt::*;
use tools::{ConsoleWriter, Source, VolumeSet, Window, push_decimal, split_args};

// One window of the file, and the unit the index is built from.
const WINDOW: u32 = 16 * 1024;
// The most a piped input may hold. A stream cannot be re-read, so this is memory rather than an
// index - and the status line says when it was reached rather than pretending the stream ended.
const MAX_STREAM: usize = 4 * 1024 * 1024;
// The most lines this pager will index. Past it the file is shown up to that point and the status
// line says so - a bound somebody can read off the screen beats a tool that grows until it dies.
const MAX_LINES: usize = 1 << 20;
// The size assumed when the terminal will not say. A pager on a terminal that does not answer is
// still a pager; refusing to run would be the worse answer.
const FALLBACK_ROWS: usize = 24;
const FALLBACK_COLS: usize = 80;

// Where the pager is looking, and how it is drawing.
struct View {
	// The first line on screen, as an index into the line table.
	top: usize,
	// The first column, for the no-wrap case. Wrapping ignores it.
	left: usize,
	wrap: bool,
	numbers: bool,
	rows: usize,
	cols: usize,
	// The last search, so `n` and `N` repeat it.
	pattern: Vec<u8>,
	// What the status line has to say beyond the position - a truncation, a failed search.
	notice: Vec<u8>,
}

// What a keystroke asked for.
enum Action {
	None,
	Redraw,
	Quit,
	// Enter the search prompt; the bool is the direction.
	Search(bool),
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		inherit_stdout(bootstrap);
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let arguments: Vec<u8> = context.arguments.clone().into_bytes();
		let volumes: VolumeSet = VolumeSet::receive(bootstrap, &mut buf);
		let cwd: String = context.cwd.clone();

		let mut numbers = false;
		let mut wrap = true;
		let mut path: Option<&[u8]> = None;
		for word in split_args(&arguments) {
			match word {
				b"-N" | b"--numbers" => numbers = true,
				b"-S" | b"--no-wrap" => wrap = false,
				_ if word.starts_with(b"-") => {
					eprint(b"less: usage: less [-N][-S] [path]\n");
					exit();
				}
				_ if path.is_none() => path = Some(word),
				_ => {
					eprint(b"less: one path only\n");
					exit();
				}
			}
		}

		// THE TWO SHAPES: a file, which is indexed and read back window by window, and a stream,
		// which is drained once because it cannot be read twice.
		let (bytes, storage, uri, streamed) = match path {
			Some(argument) => {
				let Some(uri) = storage_proto::path::resolve(&cwd, argument) else {
					eprint(b"less: invalid path\n");
					exit();
				};
				let storage: u64 = volumes.client_for(&cwd, argument);
				if storage == 0 {
					eprint(b"less: no volume\n");
					exit();
				}
				(Vec::new(), storage, uri, false)
			}
			None => {
				let Some(mut source) = Source::from_stdin() else {
					eprint(b"less: usage: less [-N][-S] [path]\n");
					exit();
				};
				let mut held: Vec<u8> = Vec::new();
				let mut truncated = false;
				loop {
					match source.next() {
						Window::Bytes(chunk) => {
							if held.len() + chunk.len() > MAX_STREAM {
								truncated = true;
								break;
							}
							if held.try_reserve(chunk.len()).is_err() {
								truncated = true;
								break;
							}
							held.extend_from_slice(&chunk);
						}
						Window::End => break,
						Window::Failed => {
							eprint(b"less: the input stream failed\n");
							exit();
						}
					}
				}
				if truncated {
					// Said now, before the terminal is taken over, because it is a fact about what
					// the pager is showing rather than a transient message.
					eprint(b"less: the input is larger than this pager holds; showing the first part\n");
				}
				(held, 0, String::new(), true)
			}
		};

		// The line index. For a file it is built by reading through once; for a stream it is built
		// over the bytes already held.
		let Some(lines) = index_lines(storage, &uri, &bytes, streamed) else {
			eprint(b"less: cannot read ");
			eprint(if streamed { b"the input" } else { uri.as_bytes() });
			eprint(b"\n");
			exit();
		};

		if stdin() == 0 || stdout() == 0 {
			eprint(b"less: interactive terminal unavailable\n");
			exit();
		}
		catch_interrupt();
		let (rows, cols) = tty_winsize().map_or((FALLBACK_ROWS, FALLBACK_COLS), |(rows, cols)| (rows as usize, cols as usize));
		let mut output = ConsoleWriter::new(stdout());
		let options = TerminalOptions { alternate_screen: true, raw_input: true, disable_echo: true, hide_cursor: true, mouse: MouseTracking::Press, bracketed_paste: false };
		let owns_tty: bool = tty_set_mode(true, false);
		if let Some(mut terminal) = TerminalGuard::enter(&mut output, options) {
			let mut view = View { top: 0, left: 0, wrap, numbers, rows: rows.max(2), cols: cols.max(8), pattern: Vec::new(), notice: Vec::new() };
			page(terminal.writer(), storage, &uri, &bytes, streamed, &lines, &mut view);
		}
		if owns_tty {
			tty_set_mode(false, true);
		}
	}
	exit();
}

// Build the table of line-start offsets.
//
// ONE PASS, AND ONLY THE OFFSETS ARE KEPT. This is the whole reason the pager can open a file
// larger than memory: what it holds afterwards is eight bytes per line, and every screen it draws
// later is a window read at an offset this pass recorded.
unsafe fn index_lines(storage: u64, uri: &str, held: &[u8], streamed: bool) -> Option<Vec<u64>> {
	unsafe {
		let mut lines: Vec<u64> = Vec::new();
		lines.try_reserve(1).ok()?;
		lines.push(0);
		let mut offset: u64 = 0;
		if streamed {
			for (index, &byte) in held.iter().enumerate() {
				if byte == b'\n' && lines.len() < MAX_LINES {
					lines.try_reserve(1).ok()?;
					lines.push(index as u64 + 1);
				}
			}
			return Some(lines);
		}
		loop {
			let window = tools::read_volume_window(storage, uri, offset, WINDOW).ok()?;
			if window.is_empty() {
				break;
			}
			for (index, &byte) in window.iter().enumerate() {
				if byte == b'\n' {
					if lines.len() >= MAX_LINES {
						return Some(lines);
					}
					lines.try_reserve(1).ok()?;
					lines.push(offset + index as u64 + 1);
				}
			}
			offset = offset.saturating_add(window.len() as u64);
		}
		Some(lines)
	}
}

// The bytes of one line, without its terminator.
//
// Read on demand rather than held, which is what makes the pager's memory independent of the file:
// a screen is at most `rows` of these live at once.
unsafe fn line_bytes(storage: u64, uri: &str, held: &[u8], streamed: bool, lines: &[u64], index: usize) -> Vec<u8> {
	unsafe {
		let Some(&start) = lines.get(index) else { return Vec::new() };
		let end: u64 = lines.get(index + 1).copied().unwrap_or(u64::MAX);
		if streamed {
			let from = core::cmp::min(start as usize, held.len());
			let to = core::cmp::min(end as usize, held.len());
			let mut bytes = held[from..to].to_vec();
			if bytes.last() == Some(&b'\n') {
				bytes.pop();
			}
			return bytes;
		}
		let want: u32 = if end == u64::MAX { WINDOW } else { core::cmp::min(end - start, WINDOW as u64) as u32 };
		let Ok(mut bytes) = tools::read_volume_window(storage, uri, start, want.max(1)) else { return Vec::new() };
		if let Some(position) = bytes.iter().position(|&byte| byte == b'\n') {
			bytes.truncate(position);
		}
		bytes
	}
}

// The pager's loop: draw, wait for a key, act.
unsafe fn page(output: &mut impl TerminalWriter, storage: u64, uri: &str, held: &[u8], streamed: bool, lines: &[u64], view: &mut View) {
	unsafe {
		let input = stdin();
		let mut decoder = InputDecoder::new();
		let mut redraw = true;
		loop {
			if redraw {
				if !render(output, storage, uri, held, streamed, lines, view) {
					return;
				}
				redraw = false;
			}
			if interrupted() {
				return;
			}
			let ready = wait_any(&[input], 0);
			if interrupted() || ready < 0 {
				return;
			}
			if ready != 0 {
				continue;
			}
			let mut bytes = [0u8; 64];
			loop {
				match try_recv(input, &mut bytes) {
					Polled::Message { len, .. } => {
						for &byte in &bytes[..len] {
							let Some(event) = decoder.feed(byte) else { continue };
							match apply(event, lines.len(), view) {
								Action::None => {}
								Action::Redraw => redraw = true,
								Action::Quit => return,
								Action::Search(forward) => {
									// The prompt reads a line with the terminal still in raw mode,
									// so the pattern is taken here rather than through the line
									// discipline - which is off precisely so keys reach this loop.
									if let Some(pattern) = read_pattern(output, input, &mut decoder, view, forward) {
										view.pattern = pattern;
									}
									search(storage, uri, held, streamed, lines, view, forward);
									redraw = true;
								}
							}
						}
					}
					Polled::Empty => break,
					Polled::Closed => return,
				}
			}
		}
	}
}

// Turn one key into what it asks for.
fn apply(event: InputEvent, total: usize, view: &mut View) -> Action {
	let InputEvent::Key(key) = event else { return Action::None };
	// The last line that can be the top of a full screen: scrolling past it would show blank rows
	// under the end of the file, which is a pager that has lost its place rather than one at the end.
	let body = view.rows.saturating_sub(1);
	let last_top = total.saturating_sub(body);
	match key {
		Key::Byte(b'q') | Key::Escape => Action::Quit,
		Key::ArrowDown | Key::Byte(b'j') | Key::Enter => {
			view.top = core::cmp::min(view.top + 1, last_top);
			Action::Redraw
		}
		Key::ArrowUp | Key::Byte(b'k') => {
			view.top = view.top.saturating_sub(1);
			Action::Redraw
		}
		Key::PageDown | Key::Byte(b' ') | Key::Control(6) => {
			view.top = core::cmp::min(view.top + body, last_top);
			Action::Redraw
		}
		Key::PageUp | Key::Byte(b'b') | Key::Control(2) => {
			view.top = view.top.saturating_sub(body);
			Action::Redraw
		}
		Key::Home | Key::Byte(b'g') => {
			view.top = 0;
			view.left = 0;
			Action::Redraw
		}
		Key::End | Key::Byte(b'G') => {
			view.top = last_top;
			Action::Redraw
		}
		// Horizontal movement only means something without wrapping: with wrap on, every byte of
		// the line is already on the screen, so scrolling sideways would move nothing.
		Key::ArrowRight if !view.wrap => {
			view.left = view.left.saturating_add(8);
			Action::Redraw
		}
		Key::ArrowLeft if !view.wrap => {
			view.left = view.left.saturating_sub(8);
			Action::Redraw
		}
		Key::Byte(b'N') => {
			view.numbers = !view.numbers;
			Action::Redraw
		}
		Key::Byte(b'S') => {
			view.wrap = !view.wrap;
			view.left = 0;
			Action::Redraw
		}
		Key::Byte(b'/') => Action::Search(true),
		Key::Byte(b'?') => Action::Search(false),
		// Repeat, in either direction, without re-typing. `n` and `N` are taken; `N` toggles
		// numbers here, so the backward repeat is `p` - stated in the status line so nobody has to
		// guess.
		Key::Byte(b'n') => Action::Search(true),
		Key::Byte(b'p') => Action::Search(false),
		_ => Action::None,
	}
}

// Read a search pattern at the bottom of the screen, with the terminal still raw.
//
// Returns None when the user typed nothing and pressed Enter, which is how a repeat is asked for:
// the previous pattern stays and the search runs again from where the view is now.
unsafe fn read_pattern(output: &mut impl TerminalWriter, input: u64, decoder: &mut InputDecoder, view: &View, forward: bool) -> Option<Vec<u8>> {
	unsafe {
		let mut pattern: Vec<u8> = Vec::new();
		loop {
			let mut line: Vec<u8> = Vec::new();
			line.extend_from_slice(b"\x1b[");
			push_row(&mut line, view.rows);
			line.extend_from_slice(b";1H\x1b[2K");
			line.push(if forward { b'/' } else { b'?' });
			line.extend_from_slice(&pattern);
			if !output.write(&line) {
				return None;
			}
			let ready = wait_any(&[input], 0);
			if interrupted() || ready < 0 {
				return None;
			}
			let mut bytes = [0u8; 64];
			match try_recv(input, &mut bytes) {
				Polled::Message { len, .. } => {
					for &byte in &bytes[..len] {
						let Some(InputEvent::Key(key)) = decoder.feed(byte) else { continue };
						match key {
							Key::Enter => return if pattern.is_empty() { None } else { Some(pattern) },
							Key::Escape => return None,
							Key::Backspace => {
								pattern.pop();
							}
							Key::Byte(byte) if byte >= 0x20 => {
								if pattern.try_reserve(1).is_err() {
									return None;
								}
								pattern.push(byte);
							}
							_ => {}
						}
					}
				}
				Polled::Empty => {}
				Polled::Closed => return None,
			}
		}
	}
}

// Move the view to the next line containing the pattern.
//
// FROM THE LINE AFTER THE CURRENT TOP, not from the top itself, so repeating a search advances
// instead of finding the same line forever. A search that finds nothing leaves the view where it
// was and says so - moving to the end on a failed search would lose the reader's place.
unsafe fn search(storage: u64, uri: &str, held: &[u8], streamed: bool, lines: &[u64], view: &mut View, forward: bool) {
	unsafe {
		view.notice.clear();
		if view.pattern.is_empty() {
			view.notice.extend_from_slice(b"no pattern");
			return;
		}
		let total = lines.len();
		let mut index = view.top;
		for _ in 0..total {
			index = if forward {
				if index + 1 >= total {
					break;
				} else {
					index + 1
				}
			} else if index == 0 {
				break;
			} else {
				index - 1
			};
			let line = line_bytes(storage, uri, held, streamed, lines, index);
			if line.windows(view.pattern.len().max(1)).any(|window| window == view.pattern) {
				view.top = index;
				return;
			}
		}
		view.notice.extend_from_slice(b"pattern not found");
	}
}

// Draw one screen: the body, then a status line.
unsafe fn render(output: &mut impl TerminalWriter, storage: u64, uri: &str, held: &[u8], streamed: bool, lines: &[u64], view: &View) -> bool {
	unsafe {
		let mut screen: Vec<u8> = Vec::new();
		screen.extend_from_slice(b"\x1b[H\x1b[2J");
		let body = view.rows.saturating_sub(1);
		let mut drawn = 0;
		let mut index = view.top;
		while drawn < body && index < lines.len() {
			let line = line_bytes(storage, uri, held, streamed, lines, index);
			let mut rendered: Vec<u8> = Vec::new();
			if view.numbers {
				let mut number = String::new();
				push_decimal(&mut number, index as u64 + 1);
				// A fixed width, so the text does not shift as the numbers grow a digit.
				for _ in number.len()..6 {
					rendered.push(b' ');
				}
				rendered.extend_from_slice(number.as_bytes());
				rendered.push(b' ');
			}
			let prefix = rendered.len();
			let width = view.cols.saturating_sub(prefix).max(1);
			if view.wrap {
				// A long line takes as many rows as it needs, and each one counts against the
				// screen - so a screen of one very long line shows one line, which is what
				// wrapping means.
				let mut at = 0;
				while at < line.len() && drawn < body {
					let end = core::cmp::min(at + width, line.len());
					if at > 0 {
						screen.extend_from_slice(b"\n");
						for _ in 0..prefix {
							screen.push(b' ');
						}
					} else {
						screen.extend_from_slice(&rendered);
					}
					screen.extend_from_slice(&line[at..end]);
					at = end;
					drawn += 1;
				}
				if line.is_empty() {
					screen.extend_from_slice(&rendered);
					drawn += 1;
				}
				screen.extend_from_slice(b"\n");
			} else {
				screen.extend_from_slice(&rendered);
				let from = core::cmp::min(view.left, line.len());
				let to = core::cmp::min(from + width, line.len());
				screen.extend_from_slice(&line[from..to]);
				screen.extend_from_slice(b"\n");
				drawn += 1;
			}
			index += 1;
		}
		// The status line: where the view is, what it is showing and the keys that are not obvious.
		screen.extend_from_slice(b"\x1b[");
		push_row(&mut screen, view.rows);
		screen.extend_from_slice(b";1H\x1b[7m");
		let mut status = String::new();
		push_decimal(&mut status, view.top as u64 + 1);
		status.push('/');
		push_decimal(&mut status, lines.len() as u64);
		screen.extend_from_slice(status.as_bytes());
		screen.extend_from_slice(if streamed { b"  (stream)  " } else { b"  " });
		if !view.notice.is_empty() {
			screen.extend_from_slice(&view.notice);
			screen.extend_from_slice(b"  ");
		}
		screen.extend_from_slice(b"q quit  space/b page  / ? search  n p repeat  N numbers  S wrap\x1b[0m");
		output.write(&screen)
	}
}

// A row number into an escape sequence, without pulling in a formatter.
fn push_row(out: &mut Vec<u8>, row: usize) {
	let mut rendered = String::new();
	push_decimal(&mut rendered, row as u64);
	out.extend_from_slice(rendered.as_bytes());
}
