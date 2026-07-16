// shell - the userspace command shell (the last component up in the boot chain).
//
// ServiceManager starts this program and hands it the StorageService client
// channel. The shell reports in and becomes the system's interactive console: it
// registers a channel the kernel feeds keystrokes to (the kernel owns the serial
// UART until a virtio-console driver exists), runs a read-eval-print loop over it,
// and drives the services over IPC. This is the phase-0 kernel CLI moved into a
// userspace component.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::codec::JsonMode;
use proto::path;
use proto::shell::{parse_and_expand, parse_assignment, trim};
use proto::system::{Component, EnvVar, JobEntry, JobInfo, TraceSpan, input, network, permission, process, session, system_graph, volume};
use rt::*;
use services::executable;

// The shell's builtins, shared with ConsoleService's line discipline: Tab completes the
// command word over the builtins plus the live bin/ listing, and the shell prints the
// matches on a double Tab - the way bash completes its builtins plus $PATH.
mod commands;

// the working directory the shell starts in - the persistent system volume, so the
// prompt sits in real storage and relative paths resolve against it
const DEFAULT_CWD: &str = "vol://system";

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	// 1. receive the whole bootstrap capability set (ended by READY) and take each
	//    capability by name - no ordering contract with the spawner. The volumes route
	//    `cat`/`ls`/`cd` (vol://system plus the removable media); process is the launcher
	//    the shell runs every governed tool through; network backs the net builtins. The
	//    extended capabilities a non-primary VT does not get (media / iso / udf / usb,
	//    input, graph, perm, resource) simply are not sent to it and read as 0 - the
	//    dependent command then reports the service unavailable. Grants the thin launcher
	//    does not use (log / device / config / time / audio, the resource client, ADMIN)
	//    are not taken and close with the set.
	let mut caps: CapSet = unsafe { recv_caps(bootstrap) };
	let storage: u64 = match caps.take(CAP_STORAGE) {
		0 => unsafe { fail_bootstrap(bootstrap, b"storage", b"required capability not granted") },
		h => h,
	};
	let media: u64 = caps.take(CAP_MEDIA);
	let iso: u64 = caps.take(CAP_ISO);
	let udf: u64 = caps.take(CAP_UDF);
	let usb: u64 = caps.take(CAP_USB);
	let procsvc: u64 = match caps.take(CAP_PROCESS) {
		0 => unsafe { fail_bootstrap(bootstrap, b"process", b"required capability not granted") },
		h => h,
	};
	let netsvc: u64 = match caps.take(CAP_NET) {
		0 => unsafe { fail_bootstrap(bootstrap, b"net", b"required capability not granted") },
		h => h,
	};
	let inputsvc: u64 = caps.take(CAP_INPUT);
	let graphsvc: u64 = caps.take(CAP_GRAPH);
	let permsvc: u64 = caps.take(CAP_PERM);
	// The ServiceManager admin channel (VT 1 is granted one; secondary VTs and a minimal
	// boot are not). The shell drives a graceful `poweroff` / `reboot` over it - a full
	// reverse-dependency teardown of the service tree, so no service is killed while a
	// dependent still needs it, before the machine powers off. With no admin channel the
	// power commands fall back to the direct power syscall (an immediate stop).
	let admin: u64 = caps.take(CAP_ADMIN);
	// The session (SessionService) the shell runs under: the long-lived owner of the
	// working directory (and, later, the environment). `cd` round-trips to it and the
	// prompt reads its cwd, so the cwd survives a shell restart - the supervisor keeps the
	// session and hands each (re)started shell a fresh capability to the same one.
	let session: u64 = caps.take(CAP_SESSION);
	// The console channel to ConsoleService: the shell writes its output to it (routed
	// via stdout) and reads its keystrokes from it. The userspace terminal renders the
	// output and forwards the input, so the shell talks to the console, not the kernel.
	let console: u64 = match caps.take(CAP_CONSOLE) {
		0 => unsafe { fail_bootstrap(bootstrap, b"console", b"required capability not granted") },
		h => h,
	};
	set_stdout(console);
	// The per-VT control channel to ConsoleService: the shell announces its foreground
	// job on it (SET_FG / CLEAR_FG) so the tty signals it on Ctrl+C / Ctrl+Z / Ctrl+\,
	// and learns of a Ctrl+Z suspend (JOB_STOPPED) so it can background the job.
	let control: u64 = match caps.take(CAP_CONTROL) {
		0 => unsafe { fail_bootstrap(bootstrap, b"control", b"required capability not granted") },
		h => h,
	};
	drop(caps);

	// 2. report in.
	unsafe {
		send_blocking(bootstrap, b"Shell: online", 0);
	}

	// 3. greet the operator with the product banner (the message of the day) - one blank
	//    line first, separating userspace from the kernel's boot log - then become the
	//    interactive console and run the read-eval-print loop.
	unsafe {
		print(b"\n");
	}
	print_motd();
	unsafe {
		repl(console, control, storage, media, iso, udf, usb, procsvc, netsvc, inputsvc, graphsvc, permsvc, session, admin);
	}
	// The REPL returned: the operator logged out (`exit` / Ctrl+D). Tell the supervisor
	// this is a deliberate exit before the bootstrap channel peer-closes, so a logout is
	// recorded as a clean stop instead of a crash (ConsoleService reloads a fresh shell).
	unsafe {
		announce_exit(bootstrap);
	}
	exit();
}

// Print the product banner as the message of the day, shown once when the shell
// becomes the interactive console, just before the first prompt. The metadata comes
// from product.conf (the single source of truth) via the build script's compile-time
// env vars, so it is never duplicated in the source.
fn print_motd() {
	let title: String = alloc::format!("{} {}", env!("PRODUCT_NAME"), env!("PRODUCT_VERSION"));
	// Three labelled URLs with their values aligned on a common column.
	let label_web: &str = "Web:";
	let label_github: &str = "GitHub:";
	let label_vendor: String = alloc::format!("by {}:", env!("PRODUCT_VENDOR"));
	let label_w: usize = label_web.len().max(label_github.len()).max(label_vendor.len());
	let web: String = alloc::format!("{:<w$} {}", label_web, env!("PRODUCT_WEBSITE"), w = label_w);
	let github: String = alloc::format!("{:<w$} {}", label_github, env!("PRODUCT_GITHUB"), w = label_w);
	let vendor: String = alloc::format!("{:<w$} {}", label_vendor, env!("PRODUCT_VENDOR_URL"), w = label_w);
	print_banner(&[title.as_str(), "", web.as_str(), github.as_str(), vendor.as_str()]);
}

// Print a product banner inside an ASCII frame (plain +/-/| so it renders on the
// console font, which carries only basic latin). The frame is sized to the longest
// line; each line is left-aligned and padded, and the whole box is flushed to the
// console in a single write.
fn print_banner(lines: &[&str]) {
	let mut width: usize = 0;
	for line in lines {
		if line.len() > width {
			width = line.len();
		}
	}
	let mut border: String = String::from("+");
	for _ in 0..width + 2 {
		border.push('-');
	}
	border.push('+');
	let mut out: String = String::new();
	out.push_str(&border);
	out.push('\n');
	for line in lines {
		out.push_str(&alloc::format!("| {:<width$} |\n", *line, width = width));
	}
	out.push_str(&border);
	out.push('\n');
	unsafe {
		print(out.as_bytes());
	}
}

// Run the read-eval-print loop over the console channel from ConsoleService. The
// terminal's line discipline cooks the keyboard input - a movable cursor, mid-line
// insert/delete, command history, the editing control keys - and hands us one finished
// line per message; we render our output (routed there via stdout). Returns when the
// user types `exit` or sends EOF (Ctrl+D on an empty line).
unsafe fn repl(console: u64, control: u64, storage: u64, media: u64, iso: u64, udf: u64, usb: u64, procsvc: u64, netsvc: u64, inputsvc: u64, graphsvc: u64, permsvc: u64, session: u64, admin: u64) {
	unsafe {
		let mut jobs: Jobs = Jobs::new(control, session);
		// The cwd is owned by the session (so it survives a shell restart); read it once at
		// startup, then keep a local cache so the prompt and path resolution need no IPC
		// round-trip each line. `cd` updates the session and refreshes this cache. With no
		// session (a minimal boot) the cwd is local-only and starts at the default volume.
		let mut cwd: String = if session != 0 {
			match session::Client::new(ChannelTransport { chan: session }).cwd() {
				Some(Ok(c)) => c,
				_ => String::from(DEFAULT_CWD),
			}
		} else {
			String::from(DEFAULT_CWD)
		};
		// The environment variables are owned by the session too; read them once at startup
		// into a local cache (name -> value) so `$`-expansion needs no IPC per line, then
		// keep the cache in step whenever an assignment or `unset` writes through. With no
		// session (a minimal boot) the environment is local-only and starts empty.
		let mut vars: Vec<(String, String)> = if session != 0 {
			match session::Client::new(ChannelTransport { chan: session }).env_list() {
				Some(Ok(list)) => list.into_iter().map(|v: EnvVar| (v.name, v.value)).collect(),
				_ => Vec::new(),
			}
		} else {
			Vec::new()
		};
		// Mirror our starting cwd to ConsoleService so argument Tab completion can resolve a
		// relative path from the very first prompt; refreshed after each `cd` below.
		send_set_cwd(control, &cwd);
		loop {
			// The line buffer matches the terminal's cooked line maximum (4 kB + the
			// newline) and lives on the heap - the kernel truncates a message to the
			// receiver's buffer silently, so it must never be smaller than a line.
			let mut line_buf: alloc::vec::Vec<u8> = alloc::vec![0u8; 4200];
			let n: usize = match recv_blocking(console, &mut line_buf) {
				Received::Message { len, .. } => len,
				Received::Closed => return,
			};
			// A zero-byte read is the tty's EOF (Ctrl+D on an empty line): log out.
			if n == 0 {
				print(b"\n");
				return;
			}
			// A line led by a tab is the line discipline's completion request (a cooked
			// line can never contain one): the bytes after the marker are the partial
			// command word. Print the matching commands - the builtins plus the live
			// bin/ listing, the way bash lists builtins plus $PATH - and re-draw the
			// prompt with the partial line (the discipline kept its buffer, so typing
			// continues in place).
			if line_buf[0] == b'\t' {
				let partial: &[u8] = &line_buf[1..n];
				print(b"\n");
				// The token under the cursor: the command word (no space before it) lists the
				// runnable programs, a later token lists the target directory's entries - the
				// same candidate sets the line discipline completes against.
				let tok_start: usize = partial.iter().rposition(|&c: &u8| c == b' ').map_or(0, |p: usize| p + 1);
				let token: &[u8] = &partial[tok_start..];
				let names: Vec<Vec<u8>> = if tok_start == 0 {
					let mut names: Vec<Vec<u8>> = bin_names(storage);
					for &builtin in commands::BUILTINS {
						names.push(builtin.as_bytes().to_vec());
					}
					names.sort();
					names.dedup();
					names
				} else {
					completion_dir_entries(&cwd, token, storage, media, iso, udf, usb)
				};
				// The filter prefix is the trailing path segment (after the last '/'), so a
				// directory's entries match on their bare name, the way the line discipline does.
				let seg_start: usize = token.iter().rposition(|&c: &u8| c == b'/').map_or(0, |p: usize| p + 1);
				let seg: &[u8] = &token[seg_start..];
				let mut listing: Vec<u8> = Vec::new();
				for name in &names {
					if name.starts_with(seg) {
						if !listing.is_empty() {
							listing.extend_from_slice(b"  ");
						}
						listing.extend_from_slice(name);
					}
				}
				listing.push(b'\n');
				print(&listing);
				print(b"\x1b[1;32m");
				print(cwd.as_bytes());
				print(b"> \x1b[0m");
				print(partial);
				continue;
			}
			// The terminal delivers a whole submitted line (with a trailing newline); trim
			// it, expand any `$NAME` / `${NAME}` against the environment, normalize its flags,
			// then dispatch it, reap finished jobs, and print the next prompt.
			let prepared: Vec<u8> = parse_and_expand(&line_buf[..n], &vars);
			let cwd_before: String = cwd.clone();
			if dispatch(&prepared, storage, media, iso, udf, usb, procsvc, netsvc, inputsvc, graphsvc, permsvc, session, admin, &mut jobs, &mut vars, &mut cwd) {
				return;
			}
			// A `cd` moved the prompt: refresh ConsoleService's copy so argument completion
			// resolves against the new directory.
			if cwd != cwd_before {
				send_set_cwd(control, &cwd);
			}
			jobs.reap();
			// the prompt shows the current working directory, so it sits in real storage.
			print(b"\x1b[1;32m");
			print(cwd.as_bytes());
			print(b"> \x1b[0m");
		}
	}
}

// Dispatch one command line. Returns true if the shell should exit.
// The in-flight foreground job: the child's live Process handle (ready once the process
// terminates) and a display name. Background and stopped jobs live in the session's job
// table, not here - this is only the job the shell is currently running in the foreground,
// or one just handed back after a Ctrl+Z suspend on its way to the session.
struct Job {
	proc: u64,
	name: Vec<u8>,
}

// The shell's job-control state. The job table itself lives in SessionService - so jobs
// (their live Process handles) survive a shell restart - and the shell is a thin client.
// It keeps the per-VT control channel to ConsoleService (the tty signals the foreground
// job over it and reports a Ctrl+Z suspend back) and the session channel it registers,
// lists and reaps jobs over. With no session (a minimal boot) job control degrades: `&`
// runs foreground and `jobs` / `fg` / `bg` report no jobs.
struct Jobs {
	control: u64,
	session: u64,
}

impl Jobs {
	fn new(control: u64, session: u64) -> Jobs {
		Jobs { control, session }
	}

	// Register a background or stopped job with the session, transferring it the live
	// Process handle (so the job survives a shell restart); the session assigns and returns
	// the small id. None when there is no session to hold it, or the register failed - the
	// caller then disposes of the job itself.
	fn register(&mut self, proc: u64, name: &[u8], stopped: bool) -> Option<u32> {
		if self.session == 0 {
			return None;
		}
		let name_str: &str = core::str::from_utf8(name).unwrap_or("");
		match session::Client::new(ChannelTransport { chan: self.session }).job_register(name_str, &stopped, &proc) {
			Some(Ok(id)) => Some(id),
			_ => None,
		}
	}

	// Run a job in the foreground; if Ctrl+Z suspends it, hand the stopped job to the
	// session as a background job. With no session to hold it, resume it instead (so it is
	// not left suspended) and stop tracking it.
	unsafe fn run_foreground_tracked(&mut self, job: Job) {
		unsafe {
			if let Some(suspended) = run_foreground(self.control, job) {
				if self.register(suspended.proc, &suspended.name, true).is_none() {
					signal(suspended.proc, SIG_CONT);
					close(suspended.proc);
				}
			}
		}
	}

	// Reap finished background jobs: ask the session which have terminated and announce
	// each. Called before each prompt, the way a shell reports a background job's completion.
	unsafe fn reap(&mut self) {
		unsafe {
			if self.session == 0 {
				return;
			}
			if let Some(Ok(finished)) = session::Client::new(ChannelTransport { chan: self.session }).job_reap() {
				for job in &finished {
					print(b"[");
					print_usize(job.id as usize);
					print(b"] done   ");
					print(job.name.as_bytes());
					print(b"\n");
				}
			}
		}
	}

	// `jobs`: list the session's tracked background / stopped jobs.
	unsafe fn list(&self) {
		unsafe {
			if self.session == 0 {
				print(b"no jobs\n");
				return;
			}
			match session::Client::new(ChannelTransport { chan: self.session }).job_list() {
				Some(Ok(jobs)) if !jobs.is_empty() => {
					for job in &jobs {
						print(b"[");
						print_usize(job.id as usize);
						print(b"] ");
						print(if job.stopped { b"stopped  " } else { b"running  " });
						print(job.name.as_bytes());
						print(b"\n");
					}
				}
				_ => print(b"no jobs\n"),
			}
		}
	}

	// Resolve a `fg` / `bg` argument to a session job id: an explicit id, or the most
	// recent job (the last the session lists) when no id is given. None when there is no
	// such job or no session.
	fn resolve_id(&self, arg: &[u8]) -> Option<u32> {
		let arg: &[u8] = trim(arg);
		if !arg.is_empty() {
			return parse_usize(arg).map(|n: usize| n as u32);
		}
		if self.session == 0 {
			return None;
		}
		match session::Client::new(ChannelTransport { chan: self.session }).job_list() {
			Some(Ok(list)) => list.last().map(|j: &JobInfo| j.id),
			_ => None,
		}
	}

	// `fg [id]`: bring a job to the foreground. Take it from the session (which transfers
	// us its Process handle back), resume it if stopped (SIG_CONT), then run it foreground
	// so it can be interrupted / suspended once more.
	unsafe fn fg(&mut self, arg: &[u8]) {
		unsafe {
			let id: u32 = match self.resolve_id(arg) {
				Some(i) => i,
				None => {
					print(b"fg: no such job\n");
					return;
				}
			};
			let entry: JobEntry = match session::Client::new(ChannelTransport { chan: self.session }).job_take(&id) {
				Some(Ok(e)) => e,
				_ => {
					print(b"fg: no such job\n");
					return;
				}
			};
			let stopped: bool = entry.info.stopped;
			let job: Job = Job { proc: entry.proc, name: entry.info.name.into_bytes() };
			if stopped {
				signal(job.proc, SIG_CONT);
			}
			print(&job.name);
			print(b"\n");
			self.run_foreground_tracked(job);
		}
	}

	// `bg [id]`: resume a stopped job in the background (SIG_CONT), leaving it tracked in
	// the session.
	unsafe fn bg(&mut self, arg: &[u8]) {
		unsafe {
			let id: u32 = match self.resolve_id(arg) {
				Some(i) => i,
				None => {
					print(b"bg: no such job\n");
					return;
				}
			};
			match session::Client::new(ChannelTransport { chan: self.session }).job_resume(&id) {
				Some(Ok(info)) => {
					print(b"[");
					print_usize(info.id as usize);
					print(b"] ");
					print(info.name.as_bytes());
					print(b" &\n");
				}
				_ => print(b"bg: no such job\n"),
			}
		}
	}
}

// Print a usize in decimal.
unsafe fn print_usize(mut n: usize) {
	unsafe {
		if n == 0 {
			print(b"0");
			return;
		}
		let mut buf: [u8; 20] = [0u8; 20];
		let mut i: usize = 20;
		while n > 0 {
			i -= 1;
			buf[i] = b'0' + (n % 10) as u8;
			n /= 10;
		}
		print(&buf[i..]);
	}
}

// Parse a decimal job id, or None if empty / non-numeric.
fn parse_usize(s: &[u8]) -> Option<usize> {
	if s.is_empty() {
		return None;
	}
	let mut v: usize = 0;
	for &b in s {
		if !b.is_ascii_digit() {
			return None;
		}
		v = v.checked_mul(10)?.checked_add((b - b'0') as usize)?;
	}
	Some(v)
}

// Run a foreground job: hand the tty this job (SET_FG, with a MANAGE+TRANSFER dup of its
// Process handle) so the user can interrupt it (Ctrl+C -> SIG_INT), suspend it
// (Ctrl+Z -> SIG_STOP) or quit it (Ctrl+\ -> SIG_TERM) from the keyboard, then wait on
// BOTH the job's Process handle and the control channel. ConsoleService's line discipline
// interprets the signal keys itself (the tty's ISIG behaviour, relocated there) and, on
// a suspend, sends JOB_STOPPED back here. Returns Some(job) when it was suspended (the
// caller backgrounds it), or None when it finished or was interrupted (its handle is
// closed here). The Process handle becomes ready once the process terminates - the
// kernel's process-terminated signal - so the shell waits for it directly instead of a
// separate completion channel. CLEAR_FG releases the tty's hold on the job before
// returning to the prompt.
unsafe fn run_foreground(control: u64, job: Job) -> Option<Job> {
	unsafe {
		// Discard any stale JOB_STOPPED a previous job's Ctrl+Z left queued, so the wait
		// below cannot mistake it for this job being suspended.
		drain_control(control);
		// Hand the tty a MANAGE+TRANSFER dup of the job (the spawn handle carries ALL
		// rights), so it can signal it; the shell keeps its own handle for fg / bg.
		let dup: i64 = duplicate(job.proc, RIGHT_MANAGE | RIGHT_TRANSFER);
		if dup >= 0 {
			send_blocking(control, b"SET_FG", dup as u64);
		}
		let waits: [u64; 2] = [job.proc, control];
		let mut cbuf: [u8; 32] = [0u8; 32];
		loop {
			let ready: i64 = wait_any(&waits, 0);
			if ready == 0 {
				// The Process handle is ready: the process has terminated (it exited or a
				// signal killed it). The job is done; release the tty and reap it.
				send_blocking(control, b"CLEAR_FG", 0);
				close(job.proc);
				return None;
			}
			match recv_blocking(control, &mut cbuf) {
				Received::Message { len, .. } if cbuf[..len].starts_with(b"JOB_STOPPED") => {
					// The tty suspended the job (Ctrl+Z): release the tty and hand the job
					// back to be tracked as a stopped background job.
					send_blocking(control, b"CLEAR_FG", 0);
					return Some(job);
				}
				Received::Closed => {
					// The console is gone; treat the job as finished.
					close(job.proc);
					return None;
				}
				_ => {} // an unknown control message; keep waiting
			}
		}
	}
}

// Discard any messages queued on the control channel without blocking - used to clear a
// stale JOB_STOPPED (a Ctrl+Z that raced a job's exit) before arming a new foreground
// job.
unsafe fn drain_control(control: u64) {
	unsafe {
		let mut cbuf: [u8; 32] = [0u8; 32];
		while let Polled::Message { .. } = try_recv(control, &mut cbuf) {}
	}
}

// `size`: query the terminal size from ConsoleService over the control channel (the typed
// winsize / TIOCGWINSZ route) and print it.
unsafe fn show_size(control: u64) {
	unsafe {
		send_blocking(control, b"GET_WINSIZE", 0);
		match recv_winsize(control, b"WINSIZE") {
			Some((rows, cols)) if rows != 0 && cols != 0 => {
				print(b"size: ");
				print_usize(cols as usize);
				print(b" cols x ");
				print_usize(rows as usize);
				print(b" rows\n");
			}
			_ => print(b"size: unavailable\n"),
		}
	}
}

// `resize <cols> <rows>`: ask ConsoleService to resize the terminal (the local stand-in
// for a display mode-set until virtio-gpu drives it), then report the new size from
// the RESIZE event it sends back.
unsafe fn resize_console(control: u64, args: &[u8]) {
	unsafe {
		let mut it = args.split(|&b| b == b' ').filter(|s: &&[u8]| !s.is_empty());
		let cols = it.next().and_then(parse_usize);
		let rows = it.next().and_then(parse_usize);
		let (cols, rows) = match (cols, rows) {
			(Some(c), Some(r)) if c > 0 && r > 0 => (c, r),
			_ => {
				print(b"usage: resize <cols> <rows>\n");
				return;
			}
		};
		let mut m: [u8; 15] = [0u8; 15];
		m[..11].copy_from_slice(b"SET_WINSIZE");
		m[11..13].copy_from_slice(&(cols as u16).to_le_bytes());
		m[13..15].copy_from_slice(&(rows as u16).to_le_bytes());
		send_blocking(control, &m, 0);
		if let Some((rows, cols)) = recv_winsize(control, b"RESIZE") {
			print(b"resized to ");
			print_usize(cols as usize);
			print(b" x ");
			print_usize(rows as usize);
			print(b"\n");
		}
	}
}

// Receive a winsize-bearing control reply with the given tag ([tag][rows u16][cols u16]),
// skipping any unrelated control message; returns (rows, cols).
unsafe fn recv_winsize(control: u64, tag: &[u8]) -> Option<(u16, u16)> {
	unsafe {
		let mut buf: [u8; 32] = [0u8; 32];
		loop {
			match recv_blocking(control, &mut buf) {
				Received::Message { len, .. } => {
					let m: &[u8] = &buf[..len];
					if m.starts_with(tag) && len >= tag.len() + 4 {
						let n = tag.len();
						let rows = u16::from_le_bytes([m[n], m[n + 1]]);
						let cols = u16::from_le_bytes([m[n + 2], m[n + 3]]);
						return Some((rows, cols));
					}
					// an unrelated control message: ignore it and keep waiting.
				}
				Received::Closed => return None,
			}
		}
	}
}

// `parse_assignment` (bare `NAME=VALUE` detection) lives in `proto::shell`, host-tested
// alongside the rest of the shell's line language.

// Set a shell variable: write it through to the session so it outlives the shell, then
// upsert the local cache. A value that is not valid UTF-8 is stored empty (the session
// contract carries a `string`); a shell with no session updates the cache only.
fn set_var(vars: &mut Vec<(String, String)>, session: u64, name: &str, value: &[u8]) {
	let value: &str = core::str::from_utf8(value).unwrap_or("");
	if session != 0 {
		let _ = session::Client::new(ChannelTransport { chan: session }).env_set(name, value);
	}
	match vars.iter_mut().find(|(n, _): &&mut (String, String)| n == name) {
		Some(entry) => {
			entry.1.clear();
			entry.1.push_str(value);
		}
		None => vars.push((String::from(name), String::from(value))),
	}
}

// Remove a shell variable: write the removal through to the session, then drop it from
// the cache. Unsetting an absent variable is a no-op, the way `unset` is.
fn unset_var(vars: &mut Vec<(String, String)>, session: u64, name: &str) {
	if session != 0 {
		let _ = session::Client::new(ChannelTransport { chan: session }).env_unset(name);
	}
	vars.retain(|(n, _): &(String, String)| n != name);
}

// `expand_vars` (the `$NAME` / `${NAME}` expansion) and `normalize_flags` (the
// `--json` / `--cbor` rewrite) live in `proto::shell`, run for every line by
// `parse_and_expand`, and are host-tested there.

// The `<name> json` / `<name> json-min` sub-form of a query command's line, if it is
// one - the argument string to forward to the tool. The tools parse the same two
// tokens (proto's JsonMode), so the shell only routes them.
fn json_subform<'a>(line: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
	let rest: &[u8] = line.strip_prefix(name)?;
	match rest {
		b" json" => Some(b"json"),
		b" json-min" => Some(b"json-min"),
		_ => None,
	}
}

// The launch shape of a governed tool the router matches from `TOOLS`: what a bare
// command word carries, and what sub-form it also accepts.
#[derive(Clone, Copy)]
enum Shape {
	// bare `name` only, no argument (e.g. `date`).
	Bare,
	// bare `name`, or the `name json` / `name json-min` sub-form (the query tools).
	Json,
	// bare `name`, or `name <rest>` (the rest passed through, trimmed).
	Rest,
	// `name <rest>` only, no bare form (e.g. `cat <path>`).
	Args,
}

// The governed tools PermissionManager launches, each by command word and launch shape.
// The router matches a line against this table (see `dispatch_tool`) instead of a
// separate `if` arm per tool, so adding a tool is one row and the routing stays short.
const TOOLS: &[(&[u8], Shape)] = &[
	(b"date", Shape::Bare),
	(b"uname", Shape::Bare),
	(b"uptime", Shape::Bare),
	(b"dmesg", Shape::Bare),
	(b"ps", Shape::Json),
	(b"free", Shape::Bare),
	(b"lsdev", Shape::Json),
	(b"perm", Shape::Json),
	(b"usage", Shape::Json),
	(b"lscpu", Shape::Json),
	(b"lsmem", Shape::Json),
	(b"lsirq", Shape::Json),
	(b"lspci", Shape::Json),
	(b"lsblk", Shape::Json),
	(b"lsusb", Shape::Json),
	(b"lsvol", Shape::Json),
	(b"log", Shape::Rest),
	(b"config", Shape::Rest),
	(b"lssvc", Shape::Rest),
	(b"beep", Shape::Rest),
	(b"play", Shape::Args),
	(b"ls", Shape::Rest),
	(b"du", Shape::Rest),
	(b"stop", Shape::Args),
	(b"run", Shape::Args),
	(b"set", Shape::Args),
	(b"cat", Shape::Args),
	(b"imgview", Shape::Args),
	(b"imgconv", Shape::Args),
	(b"write", Shape::Args),
	(b"rm", Shape::Args),
	(b"mkdir", Shape::Args),
	(b"rmdir", Shape::Args),
];

// The network tools, each by typed command word, the tool it launches (aliases fold in),
// and whether it takes an argument. `httpd` is not here - it always backgrounds and is
// routed on its own.
const NET_TOOLS: &[(&[u8], &[u8], bool)] = &[
	(b"ip", b"ip", false),
	(b"net", b"ip", false),
	(b"arp", b"arp", false),
	(b"ss", b"ss", false),
	(b"netstat", b"ss", false),
	(b"ping", b"ping", true),
	(b"nslookup", b"nslookup", true),
	(b"host", b"nslookup", true),
	(b"tcp", b"tcp", true),
	(b"nc", b"nc", true),
];

// `line` after a `name ` prefix - the argument of `name <rest>` - or None if the line is
// not that command word followed by a space.
fn strip_word<'a>(line: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
	let rest: &[u8] = line.strip_prefix(name)?;
	rest.strip_prefix(b" ")
}

// Route a line to a governed tool from `TOOLS`, returning whether it matched one.
// PermissionManager grants each tool its manifest and forwards this terminal, the cwd,
// and the argument the line's shape carries; a foreground launch, so a trailing `&` (the
// caller's `bg`) does not apply.
// Print command help: with `Some(cmd)`, that command's one-line synopsis (or an
// "unknown command" note); with `None`, every command's synopsis, sorted. The synopsis
// table lives in `commands` so it is shared with completion and stays in one place.
unsafe fn print_help(cmd: Option<&[u8]>) {
	unsafe {
		if let Some(cmd) = cmd {
			match commands::synopsis(cmd) {
				Some(text) => {
					print(text.as_bytes());
					print(b"\n");
				}
				None => {
					print(b"help: unknown command: ");
					print(cmd);
					print(b"\n");
				}
			}
			return;
		}
		let mut rows: Vec<&'static str> = commands::SYNOPSES.iter().map(|&(_, text)| text).collect();
		rows.sort_unstable();
		for text in rows {
			print(text.as_bytes());
			print(b"\n");
		}
	}
}

unsafe fn dispatch_tool(line: &[u8], permsvc: u64, cwd: &[u8]) -> bool {
	unsafe {
		for &(name, shape) in TOOLS {
			match shape {
				Shape::Bare => {
					if line == name {
						run_tool(permsvc, name, b"", cwd);
						return true;
					}
				}
				Shape::Json => {
					if line == name {
						run_tool(permsvc, name, b"", cwd);
						return true;
					}
					if let Some(args) = json_subform(line, name) {
						run_tool(permsvc, name, args, cwd);
						return true;
					}
				}
				Shape::Rest => {
					if line == name {
						run_tool(permsvc, name, b"", cwd);
						return true;
					}
					if let Some(rest) = strip_word(line, name) {
						run_tool(permsvc, name, trim(rest), cwd);
						return true;
					}
				}
				Shape::Args => {
					if let Some(rest) = strip_word(line, name) {
						run_tool(permsvc, name, trim(rest), cwd);
						return true;
					}
				}
			}
		}
		false
	}
}

// Route a line to a net tool from `NET_TOOLS`, returning whether it matched one. Each is
// spawned over a fresh NetworkService client, foreground or (with a trailing `&`, the
// caller's `bg`) background; the arg-taking forms pass the rest of the line through.
unsafe fn dispatch_net(line: &[u8], jobs: &mut Jobs, netsvc: u64, procsvc: u64, bg: bool) -> bool {
	unsafe {
		for &(word, tool, takes_args) in NET_TOOLS {
			if takes_args {
				if let Some(rest) = strip_word(line, word) {
					spawn_net_tool(jobs, netsvc, procsvc, tool, trim(rest), bg);
					return true;
				}
			} else if line == word {
				spawn_net_tool(jobs, netsvc, procsvc, tool, b"", bg);
				return true;
			}
		}
		false
	}
}

// Power the machine down gracefully. With a ServiceManager admin channel (VT 1), drive
// the supervisor's power verb over it: the supervisor tears the whole service tree down
// in reverse-dependency order - flushing LogService's last journal batch first - so no
// service is killed while a dependent still needs it, then powers the machine off (or
// reboots) from there. The banner is printed before the request goes out, because the
// teardown stops ConsoleService and the shell's output stops rendering. This blocks on
// the admin channel while the supervisor works; it only returns if there is no admin
// channel or the request could not be sent (a secondary VT / a minimal boot), and the
// caller then falls back to the direct power syscall - an immediate, ungraceful stop.
unsafe fn graceful_power(admin: u64, action: u64) {
	unsafe {
		if admin != 0 {
			print(if action == POWER_REBOOT { b"rebooting...\n" } else { b"powering off...\n" });
			let req: &[u8] = if action == POWER_REBOOT { b"!reboot" } else { b"!poweroff" };
			if send_blocking(admin, req, 0) {
				// The supervisor powers the machine off after the teardown, so this recv
				// normally never returns (the machine is gone). If it does return - the
				// channel closed because the supervisor is unexpectedly absent - fall
				// through to the direct syscall below as a backstop.
				let mut rbuf: [u8; 64] = [0u8; 64];
				let _ = recv_blocking(admin, &mut rbuf);
			}
		}
		system_power(action);
	}
}

unsafe fn dispatch(line: &[u8], storage: u64, media: u64, iso: u64, udf: u64, usb: u64, procsvc: u64, netsvc: u64, inputsvc: u64, graphsvc: u64, permsvc: u64, session: u64, admin: u64, jobs: &mut Jobs, vars: &mut Vec<(String, String)>, cwd: &mut String) -> bool {
	unsafe {
		// The line arrives already trimmed, `$`-expanded, and flag-normalized by
		// `parse_and_expand`; trim again so the recursive `time <command>` path (which passes
		// a slice of it) is clean too.
		let line = trim(line);
		if line.is_empty() {
			return false;
		}
		// `help` lists every command's synopsis; `help <command>` shows one. And a
		// `--help` anywhere on the line prints the leading command's synopsis instead of
		// launching it - so every command answers `--help` from one central table, with
		// no per-tool usage string. (`-h` is NOT intercepted: `free -h` / `du -h` mean
		// human-readable; only the unambiguous long form is help.)
		if line == b"help" {
			print_help(None);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"help ") {
			print_help(Some(trim(rest)));
			return false;
		}
		if line.split(|&b| b == b' ').any(|t: &[u8]| t == b"--help") {
			let cmd: &[u8] = line.split(|&b| b == b' ').next().unwrap_or(b"");
			print_help(Some(cmd));
			return false;
		}
		// A bare `NAME=VALUE` sets a shell variable (write it through to the session so it
		// persists, and update the cache); the value was already `$`-expanded upstream, so
		// `FOO=$BAR` copies BAR's value. Checked before the `&` split so a value may hold one.
		if let Some((name, value)) = parse_assignment(line) {
			set_var(vars, session, name, value);
			return false;
		}
		// A trailing `&` runs a spawnable command in the background.
		let (line, bg): (&[u8], bool) = match line.strip_suffix(b"&") {
			Some(rest) => (trim(rest), true),
			None => (line, false),
		};
		if line.is_empty() {
			return false;
		}
		// `time <command>` dispatches the command and prints its wall time from the
		// monotonic clock - the measuring instrument for throughput work (a foreground
		// tool runs to completion inside the dispatch, so the time covers it whole).
		if let Some(rest) = line.strip_prefix(b"time ") {
			let t0: u64 = clock_ns();
			let quit: bool = dispatch(trim(rest), storage, media, iso, udf, usb, procsvc, netsvc, inputsvc, graphsvc, permsvc, session, admin, jobs, vars, cwd);
			let us: u64 = (clock_ns() - t0) / 1_000;
			let line: String = alloc::format!("time: {}.{:03} s\n", us / 1_000_000, us % 1_000_000 / 1_000);
			print(line.as_bytes());
			return quit;
		}
		if line == b"env" {
			// List the environment the way `env` does, one `NAME=VALUE` per line, from the cache.
			for (name, value) in vars.iter() {
				print(name.as_bytes());
				print(b"=");
				print(value.as_bytes());
				print(b"\n");
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"unset ") {
			// Remove a variable: write through to the session, then drop it from the cache.
			if let Ok(name) = core::str::from_utf8(trim(rest)) {
				unset_var(vars, session, name);
			}
			return false;
		}
		if line == b"jobs" {
			jobs.list();
			return false;
		}
		if line == b"fg" {
			jobs.fg(b"");
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"fg ") {
			jobs.fg(trim(rest));
			return false;
		}
		if line == b"bg" {
			jobs.bg(b"");
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"bg ") {
			jobs.bg(trim(rest));
			return false;
		}
		if line == b"exit" || line == b"quit" {
			print(b"shell: exiting\n");
			return true;
		}
		if line == b"reboot" {
			graceful_power(admin, POWER_REBOOT);
			print(b"reboot: failed\n");
			return false;
		}
		if line == b"poweroff" || line == b"shutdown" {
			graceful_power(admin, POWER_OFF);
			print(b"poweroff: failed\n");
			return false;
		}
		if line == b"clear" {
			// ED (erase the whole display) + CUP (home the cursor) - the console's
			// cell-buffer terminal interprets these the same as any VT100 terminal.
			print(b"\x1b[2J\x1b[H");
			return false;
		}
		if line == b"size" {
			show_size(jobs.control);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"resize ") {
			resize_console(jobs.control, trim(rest));
			return false;
		}
		// The governed-tool routing (`date`, `ls`, `cat`, the `ls*` queries, ...) is a table
		// at the end of `dispatch` (`dispatch_tool`); only the commands that do not fit a table
		// shape stay as their own arms here: the graph/mouse views, the `cd` builtin, the
		// process-launcher tools (`echo` / `readln` / `script`), the multi-verb `snap` /
		// `volume`, the interactive `ps -i`, the flagged `free -h`, and the net tools.
		if line == b"graph" {
			query_graph(graphsvc, GraphFmt::Text);
			return false;
		}
		if line == b"graph json" {
			query_graph(graphsvc, GraphFmt::Json(JsonMode::Pretty));
			return false;
		}
		if line == b"graph json-min" {
			query_graph(graphsvc, GraphFmt::Json(JsonMode::Min));
			return false;
		}
		if line == b"graph cbor" {
			query_graph(graphsvc, GraphFmt::Cbor);
			return false;
		}
		if line == b"ps -i" {
			// The live view needs the terminal itself (raw input, in-place redraws), so it
			// launches through the interactive path: the same governed PermissionManager
			// launch, but handed a full-duplex dup of this console instead of a relay, and
			// set as the tty's foreground job.
			run_tool_interactive(jobs, permsvc, b"ps", b"-i", cwd.as_bytes());
			return false;
		}
		if line == b"free -h" {
			run_tool(permsvc, b"free", b"-h", cwd.as_bytes());
			return false;
		}
		if line == b"mouse" {
			mouse_cmd(inputsvc);
			return false;
		}
		// the network views: a table of net tools spawned over a fresh NetworkService client
		// (`httpd` is separate - it always backgrounds).
		if dispatch_net(line, jobs, netsvc, procsvc, bg) {
			return false;
		}
		if line == b"httpd" {
			spawn_net_tool(jobs, netsvc, procsvc, b"httpd", b"", true);
			return false;
		}
		if line == b"echo" {
			exec(jobs, procsvc, b"echo", b"", 0, bg);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"echo ") {
			exec(jobs, procsvc, b"echo", trim(rest), 0, bg);
			return false;
		}
		if line == b"readln" {
			// readln reads its stdin and echoes each line - the interactive counterpart to
			// echo, proving a foreground tool reads keyboard input, not just prints.
			exec(jobs, procsvc, b"readln", b"", 0, bg);
			return false;
		}
		if line == b"cd" {
			// no argument returns to the home volume
			cwd.clear();
			cwd.push_str(DEFAULT_CWD);
			if session != 0 {
				let _ = session::Client::new(ChannelTransport { chan: session }).chdir(DEFAULT_CWD);
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"cd ") {
			cd_cmd(cwd, trim(rest), session, storage, media, iso, udf, usb);
			return false;
		}
		if line == b"snap" || line == b"snap list" {
			// Launch `snap` as its own sandboxed ELF through PermissionManager (the launcher /
			// granter), which grants it a storage client and forwards this terminal and the
			// snapshot sub-form.
			run_tool(permsvc, b"snap", b"list", cwd.as_bytes());
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"snap create ") {
			let name: &[u8] = trim(rest);
			let mut arg: Vec<u8> = Vec::with_capacity(7 + name.len());
			arg.extend_from_slice(b"create ");
			arg.extend_from_slice(name);
			run_tool(permsvc, b"snap", &arg, cwd.as_bytes());
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"snap delete ") {
			let name: &[u8] = trim(rest);
			let mut arg: Vec<u8> = Vec::with_capacity(7 + name.len());
			arg.extend_from_slice(b"delete ");
			arg.extend_from_slice(name);
			run_tool(permsvc, b"snap", &arg, cwd.as_bytes());
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"snap cat ") {
			let rest = trim(rest);
			let mut arg: Vec<u8> = Vec::with_capacity(4 + rest.len());
			arg.extend_from_slice(b"cat ");
			arg.extend_from_slice(rest);
			run_tool(permsvc, b"snap", &arg, cwd.as_bytes());
			return false;
		}
		if line == b"volume" || line == b"volume status" {
			// Launch `volume` as its own sandboxed ELF through PermissionManager: the
			// filesystem's identity and health (label, size, free, compression, mount mode).
			run_tool(permsvc, b"volume", b"status", cwd.as_bytes());
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"volume ") {
			// the other volume verbs (compress on|off, fsck, restore <uri> [snapshot])
			// pass through whole; the tool validates the sub-form.
			run_tool(permsvc, b"volume", trim(rest), cwd.as_bytes());
			return false;
		}
		if line == b"script" {
			run_script(jobs, procsvc, b"");
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"script ") {
			run_script(jobs, procsvc, trim(rest));
			return false;
		}
		// everything else is a governed tool: match it against the tool table.
		if dispatch_tool(line, permsvc, cwd.as_bytes()) {
			return false;
		}
		print(b"\x1b[31munknown command: ");
		print(line);
		print(b" (Tab Tab lists the commands)\x1b[0m\n");
		false
	}
}

// Launch a standalone program `name` through ProcessService as a foreground child, hand
// it `args` over a bootstrap channel, and wait for it to finish. The shell never loads an
// ELF itself - ProcessService is the loading mechanism: it reads the program from the init
// package, moves the child end of the bootstrap channel in as the new process's bootstrap
// handle, and hands back the live process handle (which carries ALL rights, so the shell
// can both signal it for job control and wait on it). The child runs as its own process
// and prints its output to the console directly (a program's stdout reaches the console
// via SYS_DEBUG_WRITE); the shell waits on the Process handle, which the kernel readies
// once the process terminates - so no completion channel or zombie-lag handling is needed.
// This is the foreground exec primitive the net tools build on.
unsafe fn exec(jobs: &mut Jobs, procsvc: u64, name: &[u8], args: &[u8], cap: u64, bg: bool) {
	unsafe {
		let name_str: &str = match core::str::from_utf8(name) {
			Ok(s) => s,
			Err(_) => {
				print(name);
				print(b": invalid name\n");
				return;
			}
		};
		let (parent, child): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return,
		};
		// LAUNCH the program via ProcessService: the child end is transferred to it as the new
		// process's bootstrap handle, and it replies the live Process handle. On any failure the
		// child end has already been transferred (or never created), so we drop only our parent
		// end - the same posture the raw spawn took on a failed start.
		let mut client = process::Client::new(ChannelTransport { chan: procsvc });
		let proc: u64 = match client.launch(name_str, &child) {
			Some(Ok(started)) => started.task,
			Some(Err(_)) => {
				print(name);
				print(b": could not start\n");
				close(parent);
				return;
			}
			None => {
				print(name);
				print(b": process service unavailable\n");
				close(parent);
				return;
			}
		};
		// Hand the child our console as its stdout - and, for a foreground job, its stdin too
		// (a full-duplex dup of our console channel, the controlling terminal) - then its
		// arguments + an optional inherited capability (e.g. a NetworkService client).
		send_stdout(parent, !bg);
		send_blocking(parent, args, cap);
		// The bootstrap is delivered; the child drains it from its own end, so the shell no
		// longer needs the parent end. Drop it - the shell now tracks the job solely by its
		// waitable Process handle (ready once the child terminates), not a completion channel.
		close(parent);
		let job: Job = Job { proc, name: name.to_vec() };
		if bg {
			// Background: hand the job to the session (which holds the table, so it survives a
			// shell restart) and return to the prompt; its completion is reaped before a later
			// prompt. With no session to track it, run it in the foreground instead.
			match jobs.register(job.proc, &job.name, false) {
				Some(id) => {
					print(b"[");
					print_usize(id as usize);
					print(b"] ");
					print(name);
					print(b" &\n");
				}
				None => jobs.run_foreground_tracked(job),
			}
		} else {
			jobs.run_foreground_tracked(job);
		}
	}
}

// Hand a freshly spawned child our console as its stdout - and, for an `interactive`
// (foreground) launch, its stdin too. The console is a full-duplex controlling terminal:
// a SEND dup carries the child's `print` output to the same VT; granting RECEIVE as well
// lets the child read cooked input lines back from it (`rt::read_line`), so an interactive
// foreground tool gets keyboard input while the shell parks in `run_foreground`. A
// background job gets a SEND-only dup (no stdin - it must not race the shell for input).
// Transferred in a "STDOUT" message before the argv/capability message; the child's
// `rt::inherit_stdout` adopts it as both stdout and stdin. A handle of 0 (no console)
// leaves the child on serial with no input. Both dups carry WAIT so a child whose
// output outruns the console relay blocks in `wait` for room instead of yield-spinning.
unsafe fn send_stdout(parent: u64, interactive: bool) {
	unsafe {
		let so: u64 = stdout();
		let rights: u32 = if interactive { RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER } else { RIGHT_SEND | RIGHT_WAIT | RIGHT_TRANSFER };
		let dup: u64 = if so != 0 {
			let d: i64 = duplicate(so, rights);
			if d > 0 { d as u64 } else { 0 }
		} else {
			0
		};
		send_blocking(parent, b"STDOUT", dup);
	}
}

// Spawn a network tool as a foreground program, giving it its OWN NetworkService
// client channel: `network.open` mints a fresh client channel, which we transfer to
// the tool alongside its arguments. Each tool talks to NetworkService over its own
// channel rather than sharing the shell's (a shared channel would race), and the
// shell keeps its own `netsvc`.
unsafe fn spawn_net_tool(jobs: &mut Jobs, netsvc: u64, procsvc: u64, name: &[u8], args: &[u8], bg: bool) {
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
		exec(jobs, procsvc, name, args, tool_netsvc, bg);
	}
}

// Launch a system command as its own sandboxed ELF through PermissionManager and render
// its output on this terminal. The shell hands PermissionManager the command name, its
// argument string, the inherited working directory, and the write end of a fresh stdout
// channel; PermissionManager consults the command's permission manifest, starts it, grants
// it exactly its declared capabilities, and forwards that stdout end and the cwd to it. The
// command resolves a relative path argument against the inherited cwd, prints through the
// one capability it was granted, and the shell relays each message it sends to its own
// console until the command exits and its stdout end closes. Returns true once the command
// was launched (its own output, including any error it reports, has been rendered); false if
// PermissionManager could not start it, so the caller can fall back to an inline path. This
// is the governed-launch primitive: the shell reaches the OS commands only through
// PermissionManager (the launcher / granter), never the raw process loader, so each command
// runs with exactly its manifest's capabilities. Foreground only for now (no
// background / job control).
unsafe fn run_tool(permsvc: u64, name: &[u8], args: &[u8], cwd: &[u8]) -> bool {
	unsafe {
		let name_str: &str = match core::str::from_utf8(name) {
			Ok(s) => s,
			Err(_) => return false,
		};
		let args_str: &str = match core::str::from_utf8(args) {
			Ok(s) => s,
			Err(_) => return false,
		};
		let cwd_str: &str = match core::str::from_utf8(cwd) {
			Ok(s) => s,
			Err(_) => return false,
		};
		let (out_read, out_write): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		// Ask PermissionManager to launch the command, handing it the write end of our stdout
		// channel. On the request that end is transferred away (to PermissionManager and on to
		// the command), so we keep only the read end and never close the write end ourselves.
		let mut client = permission::Client::new(ChannelTransport { chan: permsvc });
		let task: u64 = match client.run(name_str, args_str, cwd_str, &out_write) {
			Some(Ok(started)) => started.task,
			_ => {
				close(out_read);
				return false;
			}
		};
		// Relay the command's output to our console as it prints, until it exits and its stdout
		// end closes. The buffer matches the console's own per-write size, so a single message
		// renders the same as it would straight from the command.
		let mut obuf: [u8; 4096] = [0u8; 4096];
		loop {
			match recv_blocking(out_read, &mut obuf) {
				Received::Message { len, .. } => print(&obuf[..len]),
				Received::Closed => break,
			}
		}
		close(out_read);
		close(task);
		true
	}
}

// Launch a governed command interactively: the same PermissionManager launch as
// `run_tool`, but handed a full-duplex dup of this console itself (its controlling
// terminal) as its STDOUT/stdin instead of a relay channel, and run as the tty's
// foreground job so signal keys reach it - the path a full-screen tool (`ps -i`)
// needs to flip the tty raw and redraw in place. The shell parks until the command
// exits (or hands it to the session on a Ctrl+Z suspend), exactly like an exec'd
// foreground job. Returns false if the command could not be launched.
unsafe fn run_tool_interactive(jobs: &mut Jobs, permsvc: u64, name: &[u8], args: &[u8], cwd: &[u8]) -> bool {
	unsafe {
		let name_str: &str = match core::str::from_utf8(name) {
			Ok(s) => s,
			Err(_) => return false,
		};
		let args_str: &str = match core::str::from_utf8(args) {
			Ok(s) => s,
			Err(_) => return false,
		};
		let cwd_str: &str = match core::str::from_utf8(cwd) {
			Ok(s) => s,
			Err(_) => return false,
		};
		let so: u64 = stdout();
		if so == 0 {
			return false;
		}
		let dup: i64 = duplicate(so, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_TRANSFER);
		if dup < 0 {
			return false;
		}
		let mut client = permission::Client::new(ChannelTransport { chan: permsvc });
		let task: u64 = match client.run(name_str, args_str, cwd_str, &(dup as u64)) {
			Some(Ok(started)) => started.task,
			_ => return false,
		};
		jobs.run_foreground_tracked(Job { proc: task, name: name.to_vec() });
		true
	}
}

// Record a session: ask the console (over the tty control channel) to host a shell on a
// fresh pseudo-terminal, then hand the master end to the `script` tool, which drives the
// pty's shell with `cmd` and prints the captured session. This is the foreground side of
// the PTY abstraction - a program (script) hosting a terminal it is not the hardware
// console for (the same path a future ssh drives).
unsafe fn run_script(jobs: &mut Jobs, procsvc: u64, cmd: &[u8]) {
	unsafe {
		// `PTY_OPEN` + the program to host (a shell); the console replies `PTY` + the master.
		let mut req: [u8; 13] = [0u8; 13];
		req[..8].copy_from_slice(b"PTY_OPEN");
		req[8..13].copy_from_slice(b"shell");
		send_blocking(jobs.control, &req[..13], 0);
		let mut rbuf: [u8; 32] = [0u8; 32];
		let master: u64 = match recv_blocking(jobs.control, &mut rbuf) {
			Received::Message { len, handle } if len >= 3 && &rbuf[..3] == b"PTY" && handle != 0 => handle,
			_ => {
				print(b"script: the console could not open a pty\n");
				return;
			}
		};
		exec(jobs, procsvc, b"script", cmd, master, false);
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

// `mouse`: subscribe to InputService's pointer-event stream and print the recent
// text-cell positions and button state - the plumbing echo (no mouse-driven UI yet).
// The stream is a bounded snapshot of the recent events, so it ends on its own; move
// the pointer in the graphical display first to populate it.
unsafe fn mouse_cmd(inputsvc: u64) {
	unsafe {
		let mut client = input::Client::new(ChannelTransport { chan: inputsvc });
		let consumer: u64 = match client.subscribe() {
			Some(handle) => handle,
			None => {
				print(b"mouse: service unavailable\n");
				return;
			}
		};
		let mut buf: [u8; 32] = [0u8; 32];
		let mut count: usize = 0;
		loop {
			match recv_blocking(consumer, &mut buf) {
				Received::Message { len, mut handle } => {
					if let Some(event) = input::subscribe_read(&buf[..len], &mut handle) {
						print(b"  (");
						print_usize(event.col as usize);
						print(b", ");
						print_usize(event.row as usize);
						print(b") buttons=");
						print_usize(event.buttons as usize);
						print(b"\n");
						count += 1;
					}
					if handle != 0 {
						close(handle);
					}
				}
				Received::Closed => break,
			}
		}
		close(consumer);
		if count == 0 {
			print(b"mouse: no pointer events yet (move the pointer in the graphical display)\n");
		}
	}
}

// The representation the `graph` command renders the snapshot in: human-readable text
// (the default), a JSON document, or a CBOR document shown as hex. The JSON and CBOR
// forms are the same bytes a remote consumer would read off the wire in a later phase.
enum GraphFmt {
	Text,
	Json(JsonMode),
	Cbor,
}

// Query SystemGraphService for the live system graph over the generated client and
// render it in the requested representation: the components (each with its kind,
// state, dependency edges, and live counters) and the trace spans, each via the
// generated to_text / to_json / to_cbor on the client side - the one typed API, many
// representations rule. A 0 handle means the service is not wired (e.g. a non-primary
// VT for now), reported as unavailable rather than blocking.
unsafe fn query_graph(graphsvc: u64, fmt: GraphFmt) {
	unsafe {
		if graphsvc == 0 {
			print(b"graph: service unavailable\n");
			return;
		}
		let mut client = system_graph::Client::new(ChannelTransport { chan: graphsvc });
		match client.snapshot() {
			Some(Ok(graph)) => match fmt {
				GraphFmt::Text => {
					print_text_lines(&graph.components, |c: &Component| -> String { c.to_text() });
					print_text_lines(&graph.spans, |s: &TraceSpan| -> String { s.to_text() });
				}
				GraphFmt::Json(mode) => {
					print(mode.render(graph.to_json()).as_bytes());
					print(b"\n");
				}
				GraphFmt::Cbor => print_hex(&graph.to_cbor()),
			},
			Some(Err(_)) => print(b"graph: query error\n"),
			None => print(b"graph: service unavailable\n"),
		}
	}
}

// Print bytes as a lowercase hex string on its own line - used to show a binary CBOR
// document on the text console (the same bytes a remote consumer reads off the wire).
unsafe fn print_hex(bytes: &[u8]) {
	unsafe {
		const HEX: &[u8; 16] = b"0123456789abcdef";
		let mut line: Vec<u8> = Vec::with_capacity(bytes.len() * 2 + 1);
		for &b in bytes {
			line.push(HEX[(b >> 4) as usize]);
			line.push(HEX[(b & 0x0f) as usize]);
		}
		line.push(b'\n');
		print(&line);
	}
}

// Trim leading and trailing ASCII spaces from a byte slice.
// `trim` (drop ASCII whitespace from both ends) lives in `proto::shell`.

// The names in the system volume's bin/ - the pool of runnable programs Tab completion
// offers alongside the builtins (the $PATH analogue). Empty when storage is unreachable.
fn bin_names(storage: u64) -> Vec<Vec<u8>> {
	let mut client = volume::Client::new(ChannelTransport { chan: storage });
	match client.list("vol://system/bin") {
		Some(consumer) => unsafe { drain_stream(consumer, volume::list_read) }.into_iter().filter_map(|f| executable::logical_name(&f.name).map(|name| name.as_bytes().to_vec())).collect(),
		None => Vec::new(),
	}
}

// Mirror the working directory to ConsoleService over the control channel (SET_CWD), so its
// line discipline can resolve a relative path argument for Tab completion. Sent at startup
// and after every `cd`.
unsafe fn send_set_cwd(control: u64, cwd: &str) {
	unsafe {
		let mut m: Vec<u8> = Vec::with_capacity(b"SET_CWD".len() + cwd.len());
		m.extend_from_slice(b"SET_CWD");
		m.extend_from_slice(cwd.as_bytes());
		send_blocking(control, &m, 0);
	}
}

// The entries of the directory a partial path argument names, for the double-Tab listing:
// the directory is everything up to the token's last '/' (or the cwd when it has none),
// resolved against the cwd and routed to the volume that owns it, so a listing works on any
// mounted volume. Each sub-directory carries a trailing '/'. Empty on a malformed path or an
// unreadable directory.
fn completion_dir_entries(cwd: &str, token: &[u8], storage: u64, media: u64, iso: u64, udf: u64, usb: u64) -> Vec<Vec<u8>> {
	let dir_arg: &[u8] = match token.iter().rposition(|&c: &u8| c == b'/') {
		Some(s) => &token[..s],
		None => b"",
	};
	let target: String = match path::resolve(cwd, dir_arg) {
		Some(t) => t,
		None => return Vec::new(),
	};
	let chan: u64 = path::volume_client(cwd, dir_arg, storage, media, iso, udf, usb);
	let mut client = volume::Client::new(ChannelTransport { chan });
	let mut names: Vec<Vec<u8>> = Vec::new();
	if let Some(consumer) = client.list(&target) {
		for f in unsafe { drain_stream(consumer, volume::list_read) } {
			let mut name: Vec<u8> = f.name.into_bytes();
			if f.r#type == proto::system::FileType::Dir {
				name.push(b'/');
			}
			names.push(name);
		}
	}
	names
}

// Pick the StorageService client for a vol:// URI: vol://media is the FAT media
// disk, vol://iso the ISO9660 disk, vol://udf the UDF disk, vol://usb the USB stick,
// everything else the system volume.
fn storage_for(uri: &[u8], storage: u64, media: u64, iso: u64, udf: u64, usb: u64) -> u64 {
	let v: Option<&[u8]> = uri.strip_prefix(b"vol://");
	if v.map(|r: &[u8]| r.starts_with(b"media/") || r == b"media").unwrap_or(false) {
		media
	} else if v.map(|r: &[u8]| r.starts_with(b"iso/") || r == b"iso").unwrap_or(false) {
		iso
	} else if v.map(|r: &[u8]| r.starts_with(b"udf/") || r == b"udf").unwrap_or(false) {
		udf
	} else if v.map(|r: &[u8]| r.starts_with(b"usb/") || r == b"usb").unwrap_or(false) {
		usb
	} else {
		storage
	}
}

// Change the working directory. The target is resolved against the current cwd and
// must be an existing directory, which we confirm by listing it through the owning
// StorageService; only then does the prompt move there.
unsafe fn cd_cmd(cwd: &mut String, arg: &[u8], session: u64, storage: u64, media: u64, iso: u64, udf: u64, usb: u64) {
	unsafe {
		let target: String = match path::resolve(cwd, arg) {
			Some(t) => t,
			None => {
				print(b"cd: invalid path\n");
				return;
			}
		};
		let chan: u64 = storage_for(target.as_bytes(), storage, media, iso, udf, usb);
		let mut client = volume::Client::new(ChannelTransport { chan });
		match client.list(&target) {
			Some(consumer) => {
				// a valid directory is enough - drain the entry stream unused.
				let _ = drain_stream(consumer, volume::list_read);
				cwd.clear();
				cwd.push_str(&target);
				// Persist the new cwd in the session so it outlives this shell; the local
				// cache above is what the prompt and path resolution read each line.
				if session != 0 {
					let _ = session::Client::new(ChannelTransport { chan: session }).chdir(&target);
				}
			}
			None => {
				print(b"cd: not a directory: ");
				print(target.as_bytes());
				print(b"\n");
			}
		}
	}
}
// (write/rm/mkdir/rmdir/snap commands now run as governed ELF tools launched through
// PermissionManager; the shell keeps only cd, graph, and mouse as built-ins.)
