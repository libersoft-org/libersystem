// shell - the userspace command shell (the last component up in the boot chain).
//
// ServiceManager starts this program and hands it the StorageService client
// channel. The shell first proves the service round-trip works by reading a file
// (`cat`), then reports in and becomes the system's interactive console: it
// registers a channel the kernel feeds keystrokes to (the kernel owns the serial
// UART until a virtio-console driver exists), runs a read-eval-print loop over it,
// and drives the services over IPC. This is the phase-0 kernel CLI moved into a
// userspace component.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::{config, device, log, network, process, time, volume, ConfigEntry, DeviceEntry, Entry, OpenOpts, ProcessInfo, Query, Timestamp};
use rt::*;

// the file the shell reads at startup to prove the StorageService round-trip works
const SELF_CHECK_URI: &[u8] = b"vol://system/hello.txt";

// maximum length of a typed command line
const LINE_MAX: usize = 128;

// how many past command lines the editor remembers for up/down recall
const HIST_MAX: usize = 32;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. receive the per-service client channels from ServiceManager, in the order it
	//    sends them: storage (`cat`), log (`log`), device (`dev`), process (`ps`/`run`),
	//    config (`config`/`set`), network (`ip`/`ping`/...), then a read-only view of
	//    the init package. Each is a tagged capability over the bootstrap channel.
	let storage: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"STORAGE") }.unwrap_or_else(|| exit());
	let logsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"LOG") }.unwrap_or_else(|| exit());
	let devsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"DEVICE") }.unwrap_or_else(|| exit());
	let procsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"PROCESS") }.unwrap_or_else(|| exit());
	let cfgsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"CONFIG") }.unwrap_or_else(|| exit());
	let netsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"NET") }.unwrap_or_else(|| exit());
	let timesvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"TIME") }.unwrap_or_else(|| exit());
	// The init package lets the shell spawn foreground programs (echo, later the net
	// tools); the archive is mapped 'static and parsed once.
	let (_pkg_handle, archive): (u64, &'static [u8]) = unsafe { recv_package(bootstrap, &mut buf) }.unwrap_or_else(|| exit());
	let package: Package = Package::parse(archive).unwrap_or_else(|| exit());

	// 2. self-check: prove the StorageService round-trip works by reading a file.
	if !unsafe { cat(storage, SELF_CHECK_URI) } {
		exit();
	}

	// 3. report in once the service round-trip has succeeded.
	unsafe {
		send_blocking(bootstrap, b"Shell: online", 0);
	}

	// 4. become the interactive console and run the read-eval-print loop.
	unsafe {
		repl(storage, logsvc, devsvc, procsvc, cfgsvc, netsvc, timesvc, &package, &mut buf);
	}
	exit();
}

// Register a console channel with the kernel and run the read-eval-print loop. The
// kernel feeds keystrokes on the channel; an `Editor` line-edits them (a movable
// cursor, mid-line insert/delete, command history) and we dispatch each completed
// line. Returns when the user types `exit`.
unsafe fn repl(storage: u64, logsvc: u64, devsvc: u64, procsvc: u64, cfgsvc: u64, netsvc: u64, timesvc: u64, package: &Package, buf: &mut [u8]) {
	unsafe {
		// The kernel sends console input on `feed`; we receive it on `input`.
		let (feed, input): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return,
		};
		if sys_is_err(syscall(SYS_CONSOLE_ATTACH, feed, 0, 0, 0)) {
			return;
		}
		let mut ed: Editor = Editor::new();
		loop {
			let n: usize = match recv_blocking(input, buf) {
				Received::Message { len, .. } => len,
				Received::Closed => return,
			};
			for i in 0..n {
				// Feed each byte to the editor; a true return means the line was
				// submitted (Enter), so dispatch it, record it in history, and prompt.
				if ed.feed(buf[i]) {
					if dispatch(&ed.line[..ed.len], storage, logsvc, devsvc, procsvc, cfgsvc, netsvc, timesvc, package) {
						return;
					}
					ed.commit();
					print(b"\x1b[1;32m> \x1b[0m");
				}
			}
		}
	}
}

// The interactive line editor: the line being typed plus a cursor, a command
// history for up/down recall, and a small state machine that decodes the ANSI escape
// sequences the navigation keys arrive as (ESC [ A/B/C/D for the arrows, ESC [ H / F
// for Home / End, ESC [ 3 ~ for Delete) - the same bytes a serial terminal sends and
// `driver.virtio-input` emits, so both consoles edit identically. Redraw uses only
// carriage return, backspace (a non-destructive cursor-left), spaces, and reprinting,
// which the framebuffer console already renders, so no terminal-specific output is
// needed.
struct Editor {
	line: [u8; LINE_MAX],
	len: usize,
	cursor: usize,
	history: Vec<Vec<u8>>,
	// Browse position in history; equal to `history.len()` means the live (not yet
	// recalled) line.
	hist_pos: usize,
	// Escape-sequence decoder state: 0 = normal, 1 = after ESC, 2 = after ESC '['.
	esc: u8,
	// The numeric parameter accumulated in a `ESC [ <n> ~` sequence.
	csi_param: u8,
}

impl Editor {
	fn new() -> Editor {
		Editor { line: [0u8; LINE_MAX], len: 0, cursor: 0, history: Vec::new(), hist_pos: 0, esc: 0, csi_param: 0 }
	}

	// Feed one input byte. Returns true when the line is submitted (Enter): the caller
	// reads `line[..len]`, dispatches it, then calls `commit`.
	unsafe fn feed(&mut self, b: u8) -> bool {
		unsafe {
			match self.esc {
				1 => {
					if b == b'[' {
						self.esc = 2;
						self.csi_param = 0;
					} else {
						self.esc = 0;
					}
					return false;
				}
				2 => {
					self.csi(b);
					return false;
				}
				_ => {}
			}
			match b {
				0x1b => self.esc = 1,
				b'\n' | b'\r' => {
					print(b"\n");
					return true;
				}
				0x08 | 0x7f => self.backspace(),
				0x20..=0x7e => self.insert(b),
				_ => {}
			}
			false
		}
	}

	// Handle the byte after `ESC [`: the final letter of an arrow / Home / End move, a
	// digit of a `ESC [ <n> ~` parameter, or the `~` that ends one.
	unsafe fn csi(&mut self, b: u8) {
		unsafe {
			match b {
				b'A' => {
					self.history_prev();
					self.esc = 0;
				}
				b'B' => {
					self.history_next();
					self.esc = 0;
				}
				b'C' => {
					self.right();
					self.esc = 0;
				}
				b'D' => {
					self.left();
					self.esc = 0;
				}
				b'H' => {
					self.home();
					self.esc = 0;
				}
				b'F' => {
					self.end();
					self.esc = 0;
				}
				b'0'..=b'9' => self.csi_param = self.csi_param.wrapping_mul(10).wrapping_add(b - b'0'),
				b'~' => {
					match self.csi_param {
						1 | 7 => self.home(),
						4 | 8 => self.end(),
						3 => self.delete(),
						_ => {}
					}
					self.esc = 0;
				}
				_ => self.esc = 0,
			}
		}
	}

	// Insert a printable character at the cursor, shifting the tail right and redrawing
	// it, then leaving the cursor just after the new character.
	unsafe fn insert(&mut self, c: u8) {
		unsafe {
			if self.len >= LINE_MAX {
				return;
			}
			let mut i: usize = self.len;
			while i > self.cursor {
				self.line[i] = self.line[i - 1];
				i -= 1;
			}
			self.line[self.cursor] = c;
			self.len += 1;
			print(&self.line[self.cursor..self.len]);
			self.cursor += 1;
			self.move_left(self.len - self.cursor);
		}
	}

	// Delete the character before the cursor (Backspace), shifting the tail left.
	unsafe fn backspace(&mut self) {
		unsafe {
			if self.cursor == 0 {
				return;
			}
			let mut i: usize = self.cursor;
			while i < self.len {
				self.line[i - 1] = self.line[i];
				i += 1;
			}
			self.cursor -= 1;
			self.len -= 1;
			print(b"\x08");
			print(&self.line[self.cursor..self.len]);
			print(b" ");
			self.move_left(self.len - self.cursor + 1);
		}
	}

	// Delete the character at the cursor (the Delete key), shifting the tail left.
	unsafe fn delete(&mut self) {
		unsafe {
			if self.cursor >= self.len {
				return;
			}
			let mut i: usize = self.cursor + 1;
			while i < self.len {
				self.line[i - 1] = self.line[i];
				i += 1;
			}
			self.len -= 1;
			print(&self.line[self.cursor..self.len]);
			print(b" ");
			self.move_left(self.len - self.cursor + 1);
		}
	}

	// Move the cursor one cell left (non-destructive backspace).
	unsafe fn left(&mut self) {
		unsafe {
			if self.cursor > 0 {
				print(b"\x08");
				self.cursor -= 1;
			}
		}
	}

	// Move the cursor one cell right by reprinting the character it sits on.
	unsafe fn right(&mut self) {
		unsafe {
			if self.cursor < self.len {
				print(&self.line[self.cursor..self.cursor + 1]);
				self.cursor += 1;
			}
		}
	}

	// Move the cursor to the start of the line.
	unsafe fn home(&mut self) {
		unsafe {
			self.move_left(self.cursor);
			self.cursor = 0;
		}
	}

	// Move the cursor to the end of the line.
	unsafe fn end(&mut self) {
		unsafe {
			print(&self.line[self.cursor..self.len]);
			self.cursor = self.len;
		}
	}

	// Emit `n` backspaces to step the cursor `n` cells left.
	unsafe fn move_left(&self, n: usize) {
		unsafe {
			for _ in 0..n {
				print(b"\x08");
			}
		}
	}

	// Replace the whole line with `new` and redraw: walk to the end, erase the old line
	// leftward, then echo the new one (the cursor lands at its end). Used by history.
	unsafe fn replace_line(&mut self, new: &[u8]) {
		unsafe {
			print(&self.line[self.cursor..self.len]);
			for _ in 0..self.len {
				print(b"\x08 \x08");
			}
			let n: usize = new.len().min(LINE_MAX);
			self.line[..n].copy_from_slice(&new[..n]);
			self.len = n;
			self.cursor = n;
			print(&self.line[..n]);
		}
	}

	// Recall the previous history entry (Up).
	unsafe fn history_prev(&mut self) {
		unsafe {
			if self.hist_pos == 0 {
				return;
			}
			self.hist_pos -= 1;
			let mut tmp: [u8; LINE_MAX] = [0u8; LINE_MAX];
			let h: &[u8] = &self.history[self.hist_pos];
			let n: usize = h.len().min(LINE_MAX);
			tmp[..n].copy_from_slice(&h[..n]);
			self.replace_line(&tmp[..n]);
		}
	}

	// Recall the next history entry (Down), or the empty live line past the newest.
	unsafe fn history_next(&mut self) {
		unsafe {
			if self.hist_pos >= self.history.len() {
				return;
			}
			self.hist_pos += 1;
			if self.hist_pos == self.history.len() {
				self.replace_line(b"");
			} else {
				let mut tmp: [u8; LINE_MAX] = [0u8; LINE_MAX];
				let h: &[u8] = &self.history[self.hist_pos];
				let n: usize = h.len().min(LINE_MAX);
				tmp[..n].copy_from_slice(&h[..n]);
				self.replace_line(&tmp[..n]);
			}
		}
	}

	// After a submitted line is dispatched: record it in history (skipping an empty
	// line or an immediate duplicate), then reset for the next line.
	fn commit(&mut self) {
		let trimmed: &[u8] = trim(&self.line[..self.len]);
		if !trimmed.is_empty() && self.history.last().map(|h: &Vec<u8>| h.as_slice()) != Some(trimmed) {
			if self.history.len() >= HIST_MAX {
				self.history.remove(0);
			}
			self.history.push(trimmed.to_vec());
		}
		self.len = 0;
		self.cursor = 0;
		self.hist_pos = self.history.len();
		self.esc = 0;
		self.csi_param = 0;
	}
}

// Dispatch one command line. Returns true if the shell should exit.
unsafe fn dispatch(line: &[u8], storage: u64, logsvc: u64, devsvc: u64, procsvc: u64, cfgsvc: u64, netsvc: u64, timesvc: u64, package: &Package) -> bool {
	unsafe {
		let line = trim(line);
		if line.is_empty() {
			return false;
		}
		if line == b"exit" || line == b"quit" {
			print(b"shell: exiting\n");
			return true;
		}
		if line == b"help" {
			print(b"commands:\n");
			print(b"  help             show this help\n");
			print(b"  echo <text>      print text\n");
			print(b"  cat <vol://...>  read a file via StorageService\n");
			print(b"  log [json]       show the system journal via LogService\n");
			print(b"  log tail [json]  stream the journal via LogService (sub-channel)\n");
			print(b"  dev [json]       list devices via DeviceService\n");
			print(b"  ps               list started processes via ProcessService\n");
			print(b"  run <name>       start a program via ProcessService\n");
			print(b"  config [<key>]   list the config tree or read one key via ConfigService\n");
			print(b"  set <key> <val>  write a config key via ConfigService\n");
			print(b"  ip | net         show the network interface and ARP cache\n");
			print(b"  ping <ip>        send an ICMP echo via the net driver\n");
			print(b"  nslookup <name>  resolve a name to an address via DNS\n");
			print(b"  tcp <ip> <port>  open a TCP connection and probe it (HTTP GET)\n");
			print(b"  nc <ip> <port>   open a raw TCP connection (optional request to send)\n");
			print(b"  arp              show the ARP / neighbor cache\n");
			print(b"  ss | netstat     list the live sockets\n");
			print(b"  httpd            serve HTTP on port 80 (background)\n");
			print(b"  exit             stop the shell and halt\n");
			return false;
		}
		if line == b"log" {
			query_log(logsvc, timesvc, false);
			return false;
		}
		if line == b"log json" {
			query_log(logsvc, timesvc, true);
			return false;
		}
		if line == b"log tail" {
			tail_log(logsvc, timesvc, false);
			return false;
		}
		if line == b"log tail json" {
			tail_log(logsvc, timesvc, true);
			return false;
		}
		if line == b"dev" {
			query_devices(devsvc, false);
			return false;
		}
		if line == b"dev json" {
			query_devices(devsvc, true);
			return false;
		}
		if line == b"ps" {
			query_processes(procsvc);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"run ") {
			run_process(procsvc, trim(rest));
			return false;
		}
		if line == b"config" {
			query_config(cfgsvc);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"config ") {
			get_config(cfgsvc, trim(rest));
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"set ") {
			set_config(cfgsvc, trim(rest));
			return false;
		}
		if line == b"date" {
			show_date(timesvc);
			return false;
		}
		if line == b"ip" || line == b"net" {
			spawn_net_tool(netsvc, package, b"ip", b"");
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"ping ") {
			spawn_net_tool(netsvc, package, b"ping", trim(rest));
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"nslookup ") {
			spawn_net_tool(netsvc, package, b"nslookup", trim(rest));
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"host ") {
			spawn_net_tool(netsvc, package, b"nslookup", trim(rest));
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"tcp ") {
			spawn_net_tool(netsvc, package, b"tcp", trim(rest));
			return false;
		}
		if line == b"arp" {
			spawn_net_tool(netsvc, package, b"arp", b"");
			return false;
		}
		if line == b"ss" || line == b"netstat" {
			spawn_net_tool(netsvc, package, b"ss", b"");
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"nc ") {
			spawn_net_tool(netsvc, package, b"nc", trim(rest));
			return false;
		}
		if line == b"httpd" {
			spawn_net_tool_bg(netsvc, package, b"httpd");
			return false;
		}
		if line == b"echo" {
			exec(package, b"echo", b"", 0);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"echo ") {
			exec(package, b"echo", trim(rest), 0);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"cat ") {
			let uri = trim(rest);
			if !cat(storage, uri) {
				print(b"cat: could not read ");
				print(uri);
				print(b"\n");
			}
			return false;
		}
		print(b"\x1b[31munknown command: ");
		print(line);
		print(b" (try 'help')\x1b[0m\n");
		false
	}
}

// Spawn a standalone program `name` from the init package as a foreground child,
// hand it `args` over a bootstrap channel, and wait for it to finish. The child
// runs as its own process and prints its output to the console directly (a
// program's stdout reaches the console via SYS_DEBUG_WRITE); it sends a completion
// message just before it exits, which we wait on - an exited process is briefly a
// zombie whose channel has not yet closed, so we cannot rely on the channel closing
// to detect exit. This is the foreground exec primitive the net tools build on.
unsafe fn exec(package: &Package, name: &[u8], args: &[u8], cap: u64) {
	unsafe {
		let elf: &[u8] = match package.lookup(name) {
			Some(e) => e,
			None => {
				print(name);
				print(b": program not found\n");
				return;
			}
		};
		let (parent, child): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return,
		};
		// `spawn` moves the child end into the new process as its bootstrap handle.
		if spawn(elf, child) < 0 {
			print(name);
			print(b": could not start\n");
			close(parent);
			return;
		}
		// Hand the child its arguments (and an optional inherited capability, e.g. a
		// NetworkService client channel for a net tool), then block until it signals
		// completion.
		send_blocking(parent, args, cap);
		let mut done: [u8; 16] = [0u8; 16];
		match recv_blocking(parent, &mut done) {
			Received::Message { .. } | Received::Closed => {}
		}
		close(parent);
	}
}

// Spawn a standalone program `name` as a background child (no wait): hand it `args`
// and an optional capability over a bootstrap channel, then detach. Unlike `exec`, the
// shell does not wait for completion - the child runs on its own until it exits (e.g.
// a background server running until its service channel closes). The child receives
// its arguments + capability in its first recv before we drop our end.
unsafe fn exec_bg(package: &Package, name: &[u8], args: &[u8], cap: u64) {
	unsafe {
		let elf: &[u8] = match package.lookup(name) {
			Some(e) => e,
			None => {
				print(name);
				print(b": program not found\n");
				return;
			}
		};
		let (parent, child): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return,
		};
		if spawn(elf, child) < 0 {
			print(name);
			print(b": could not start\n");
			close(parent);
			return;
		}
		send_blocking(parent, args, cap);
		close(parent);
	}
}

// Spawn a network tool as a foreground program, giving it its OWN NetworkService
// client channel: `network.open` mints a fresh client channel, which we transfer to
// the tool alongside its arguments. Each tool talks to NetworkService over its own
// channel rather than sharing the shell's (a shared channel would race), and the
// shell keeps its own `netsvc`.
unsafe fn spawn_net_tool(netsvc: u64, package: &Package, name: &[u8], args: &[u8]) {
	unsafe {
		if netsvc == 0 {
			print(name);
			print(b": no network interface\n");
			return;
		}
		let mut client = network::Client::new(ChannelTransport { chan: netsvc });
		let tool_netsvc: u64 = match client.open() {
			Some(Ok(h)) => h,
			_ => {
				print(name);
				print(b": network service unavailable\n");
				return;
			}
		};
		exec(package, name, args, tool_netsvc);
	}
}

// Spawn a network tool as a background program (no wait), giving it its OWN
// NetworkService client channel - like `spawn_net_tool` but detached, for a
// long-running server (httpd) that should not block the interactive shell.
unsafe fn spawn_net_tool_bg(netsvc: u64, package: &Package, name: &[u8]) {
	unsafe {
		if netsvc == 0 {
			print(name);
			print(b": no network interface\n");
			return;
		}
		let mut client = network::Client::new(ChannelTransport { chan: netsvc });
		let tool_netsvc: u64 = match client.open() {
			Some(Ok(h)) => h,
			_ => {
				print(name);
				print(b": network service unavailable\n");
				return;
			}
		};
		exec_bg(package, name, b"", tool_netsvc);
	}
}

// Render typed records as a JSON array - each via its generated to_json(), comma-
// separated within [ ] - the framing the `json` variants of the query commands share.
unsafe fn print_json_array<T, F: Fn(&T) -> String>(items: &[T], to_json: F) {
	unsafe {
		print(b"[");
		let mut first: bool = true;
		for item in items {
			if !first {
				print(b",");
			}
			first = false;
			print(to_json(item).as_bytes());
		}
		print(b"]\n");
	}
}

// Render typed records as text, one per line, each via its generated to_text().
unsafe fn print_text_lines<T, F: Fn(&T) -> String>(items: &[T], to_text: F) {
	unsafe {
		for item in items {
			print(to_text(item).as_bytes());
			print(b"\n");
		}
	}
}

// Query LogService for the journal over the generated Log client and print it,
// rendering each returned entry to text or JSON on the client side. The query
// asks for all severities (no minimum) and no limit.
// Show the current wall-clock time via TimeService, rendered as ISO-8601 UTC.
unsafe fn show_date(timesvc: u64) {
	unsafe {
		let mut client = time::Client::new(ChannelTransport { chan: timesvc });
		match client.now() {
			Some(Ok(ts)) => {
				let mut out: [u8; 24] = [0u8; 24];
				let n: usize = ts.render(&mut out);
				print(&out[..n]);
				print(b"\n");
			}
			Some(Err(_)) => print(b"date: time error\n"),
			None => print(b"date: service unavailable\n"),
		}
	}
}

// The Unix epoch (seconds) at monotonic tick 0, from TimeService: the current wall
// time minus the seconds elapsed since boot. Lets the shell render a log record's
// monotonic timestamp as wall-clock. None if TimeService is unavailable.
unsafe fn boot_epoch(timesvc: u64) -> Option<u64> {
	unsafe {
		let mut client = time::Client::new(ChannelTransport { chan: timesvc });
		match client.now() {
			Some(Ok(ts)) => Some(ts.unix_secs.saturating_sub(clock() / 100)),
			_ => None,
		}
	}
}

// Render one log entry as text, prefixed with its wall-clock time when the boot epoch
// is known (the record's monotonic tick converted to UTC), else the bare record.
fn entry_text(e: &Entry, epoch: Option<u64>) -> String {
	match epoch {
		Some(base) => {
			let wall: u64 = base + e.timestamp / 100;
			let mut iso: [u8; 24] = [0u8; 24];
			let n: usize = Timestamp { unix_secs: wall }.render(&mut iso);
			let mut s: String = String::from(core::str::from_utf8(&iso[..n]).unwrap_or(""));
			s.push(' ');
			s.push_str(&e.to_text());
			s
		}
		None => e.to_text(),
	}
}

unsafe fn query_log(logsvc: u64, timesvc: u64, json: bool) {
	unsafe {
		let q = Query { since: None, min_severity: None, source: None, limit: 0 };
		let epoch: Option<u64> = boot_epoch(timesvc);
		let mut client = log::Client::new(ChannelTransport { chan: logsvc });
		match client.query(&q) {
			Some(Ok(entries)) => {
				if json {
					print_json_array(&entries, |e: &Entry| -> String { e.to_json() });
				} else {
					print_text_lines(&entries, |e: &Entry| -> String { entry_text(e, epoch) });
				}
			}
			Some(Err(_)) => print(b"log: query error\n"),
			None => print(b"log: service unavailable\n"),
		}
	}
}

// Stream the system journal via LogService's OP_TAIL. Unlike `query`, which packs
// every matching entry into a single reply, `tail` returns a fresh sub-channel:
// the service frames each entry as its own message on it and closes it to mark the
// end of the stream. We drain the frames and render each entry on the client side,
// exactly like `log`, but one streamed record at a time.
unsafe fn tail_log(logsvc: u64, timesvc: u64, json: bool) {
	unsafe {
		let q = Query { since: None, min_severity: None, source: None, limit: 0 };
		let epoch: Option<u64> = boot_epoch(timesvc);
		let mut client = log::Client::new(ChannelTransport { chan: logsvc });
		let consumer: u64 = match client.tail(&q) {
			Some(h) => h,
			None => {
				print(b"log: service unavailable\n");
				return;
			}
		};
		if json {
			print(b"[");
		}
		let mut first: bool = true;
		let mut frame: [u8; 1024] = [0u8; 1024];
		loop {
			match recv_blocking(consumer, &mut frame) {
				Received::Message { len, .. } => {
					if let Some(entry) = log::tail_read(&frame[..len]) {
						if json {
							if !first {
								print(b",");
							}
							first = false;
							print(entry.to_json().as_bytes());
						} else {
							print(entry_text(&entry, epoch).as_bytes());
							print(b"\n");
						}
					}
				}
				Received::Closed => break,
			}
		}
		if json {
			print(b"]\n");
		}
		close(consumer);
	}
}

// Query DeviceService for the discovered devices over the generated Device client
// and print them, rendering each typed entry to text or JSON on the client side.
unsafe fn query_devices(devsvc: u64, json: bool) {
	unsafe {
		let mut client = device::Client::new(ChannelTransport { chan: devsvc });
		match client.list() {
			Some(Ok(entries)) => {
				if json {
					print_json_array(&entries, |e: &DeviceEntry| -> String { e.to_json() });
				} else {
					print_text_lines(&entries, |e: &DeviceEntry| -> String { e.to_text() });
				}
			}
			Some(Err(_)) => print(b"dev: query error\n"),
			None => print(b"dev: service unavailable\n"),
		}
	}
}

// Query ProcessService for the processes it has started and print each typed entry.
unsafe fn query_processes(procsvc: u64) {
	unsafe {
		let mut client = process::Client::new(ChannelTransport { chan: procsvc });
		match client.list() {
			Some(Ok(procs)) => print_text_lines(&procs, |p: &ProcessInfo| -> String { p.to_text() }),
			Some(Err(_)) => print(b"ps: query error\n"),
			None => print(b"ps: service unavailable\n"),
		}
	}
}

// Start the program named `name` via ProcessService and report the new process.
unsafe fn run_process(procsvc: u64, name: &[u8]) {
	unsafe {
		let name = match core::str::from_utf8(name) {
			Ok(s) => s,
			Err(_) => {
				print(b"run: invalid name\n");
				return;
			}
		};
		let mut client = process::Client::new(ChannelTransport { chan: procsvc });
		match client.start(name) {
			Some(Ok(info)) => {
				print(b"started ");
				print(info.to_text().as_bytes());
				print(b"\n");
			}
			Some(Err(_)) => {
				print(b"run: could not start ");
				print(name.as_bytes());
				print(b"\n");
			}
			None => print(b"run: service unavailable\n"),
		}
	}
}

// Query ConfigService for the whole configuration tree and print each typed node.
unsafe fn query_config(cfgsvc: u64) {
	unsafe {
		let mut client = config::Client::new(ChannelTransport { chan: cfgsvc });
		match client.list() {
			Some(Ok(entries)) => print_text_lines(&entries, |e: &ConfigEntry| -> String { e.to_text() }),
			Some(Err(_)) => print(b"config: query error\n"),
			None => print(b"config: service unavailable\n"),
		}
	}
}

// Read one configuration node by key via ConfigService and print its value.
unsafe fn get_config(cfgsvc: u64, key: &[u8]) {
	unsafe {
		let key = match core::str::from_utf8(key) {
			Ok(s) => s,
			Err(_) => {
				print(b"config: invalid key\n");
				return;
			}
		};
		let mut client = config::Client::new(ChannelTransport { chan: cfgsvc });
		match client.get(key) {
			Some(Ok(value)) => {
				print(value.as_bytes());
				print(b"\n");
			}
			Some(Err(_)) => {
				print(b"config: no such key ");
				print(key.as_bytes());
				print(b"\n");
			}
			None => print(b"config: service unavailable\n"),
		}
	}
}

// Write a configuration node via ConfigService: `rest` is "<key> <value>".
unsafe fn set_config(cfgsvc: u64, rest: &[u8]) {
	unsafe {
		let (key, value): (&[u8], &[u8]) = match rest.iter().position(|&b: &u8| b == b' ') {
			Some(i) => (&rest[..i], trim(&rest[i + 1..])),
			None => {
				print(b"usage: set <key> <value>\n");
				return;
			}
		};
		let key = match core::str::from_utf8(key) {
			Ok(s) => s,
			Err(_) => {
				print(b"set: invalid key\n");
				return;
			}
		};
		let value = match core::str::from_utf8(value) {
			Ok(s) => s,
			Err(_) => {
				print(b"set: invalid value\n");
				return;
			}
		};
		let entry = ConfigEntry { key: alloc::string::String::from(key), value: alloc::string::String::from(value) };
		let mut client = config::Client::new(ChannelTransport { chan: cfgsvc });
		match client.set(&entry) {
			Some(Ok(())) => print(b"ok\n"),
			Some(Err(_)) => print(b"set: error\n"),
			None => print(b"set: service unavailable\n"),
		}
	}
}

// Trim leading and trailing ASCII spaces from a byte slice.
fn trim(mut s: &[u8]) -> &[u8] {
	while let [b' ', rest @ ..] = s {
		s = rest;
	}
	while let [rest @ .., b' '] = s {
		s = rest;
	}
	s
}

// Open `uri` through the StorageService channel `storage` over the generated volume
// client, map the returned shared buffer, print its bytes to the console, and
// release it. Returns true on success.
unsafe fn cat(storage: u64, uri: &[u8]) -> bool {
	unsafe {
		let opts: OpenOpts = OpenOpts { path: alloc::string::String::from_utf8_lossy(uri).into_owned(), write: false, create: false };
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let result = match client.open(&opts) {
			Some(Ok(r)) => r,
			_ => return false,
		};
		if result.file == 0 || result.size == 0 {
			return false;
		}
		// map the shared buffer, print the file, then release it.
		let mapped: u64 = match map_object(result.file) {
			Some(base) => base,
			None => return false,
		};
		let contents: &[u8] = core::slice::from_raw_parts(mapped as *const u8, result.size as usize);
		print(contents);
		if contents.last() != Some(&b'\n') {
			print(b"\n");
		}
		unmap_object(result.file);
		close(result.file);
		true
	}
}
