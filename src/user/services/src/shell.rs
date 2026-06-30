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
use proto::path;
use proto::system::{audio, config, device, input, log, network, permission, process, resources, session, system_graph, time, volume, AuditEntry, Budget, Component, ConfigEntry, DeviceEntry, Entry, FileKind, OpenOpts, ProcessInfo, Query, Timestamp, TraceSpan};
use rt::*;

// the file the shell reads at startup to prove the StorageService round-trip works
const SELF_CHECK_URI: &[u8] = b"vol://system/hello.txt";

// the working directory the shell starts in - the persistent system volume, so the
// prompt sits in real storage and relative paths resolve against it
const DEFAULT_CWD: &str = "vol://system";

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. receive the per-service client channels from ServiceManager, in the order it
	//    sends them: storage (`cat`), log (`log`), device (`dev`), process (`ps`/`run`
	//    and the launcher the shell runs foreground programs through), config
	//    (`config`/`set`), network (`ip`/`ping`/...), time (`date`), audio (`beep`). Each
	//    is a tagged capability over the bootstrap channel. The extended capabilities the
	//    primary VT also gets (the media / iso / udf volumes, input, graph, perm, resource)
	//    arrive as 0 on a non-primary VT - ConsoleService cannot mint them per VT (input /
	//    graph are single-client, the rest are simply not proxied), and the dependent
	//    command then reports the service unavailable.
	let storage: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"STORAGE") }.unwrap_or_else(|| exit());
	// The media StorageService client: the FAT vol://media volume off a second
	// virtio-blk disk. Sent right after STORAGE; `cat`/`ls` route vol://media to it.
	let media: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"MEDIA") }.unwrap_or(0);
	// The ISO StorageService client: the read-only ISO9660 vol://iso volume off a third
	// virtio-blk disk. Sent right after MEDIA; `cat`/`ls` route vol://iso to it.
	let iso: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"ISO") }.unwrap_or(0);
	// The UDF StorageService client: the read-only UDF vol://udf volume off a fourth
	// virtio-blk disk. Sent right after ISO; `cat`/`ls` route vol://udf to it.
	let udf: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"UDF") }.unwrap_or(0);
	let logsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"LOG") }.unwrap_or_else(|| exit());
	let devsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"DEVICE") }.unwrap_or_else(|| exit());
	let procsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"PROCESS") }.unwrap_or_else(|| exit());
	let cfgsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"CONFIG") }.unwrap_or_else(|| exit());
	let netsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"NET") }.unwrap_or_else(|| exit());
	let timesvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"TIME") }.unwrap_or_else(|| exit());
	let audiosvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"AUDIO") }.unwrap_or_else(|| exit());
	// The InputService client: `mouse` subscribes to its pointer-event stream and prints
	// the recent text-cell positions (the plumbing echo - no mouse-driven UI yet).
	let inputsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"INPUT") }.unwrap_or(0);
	// The SystemGraphService client: `graph` queries the live system graph (components,
	// devices, dependency edges, counters, and trace spans) and renders it as CLI / JSON
	// / CBOR. Sent right after INPUT, matching ServiceManager's send order.
	let graphsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"GRAPH") }.unwrap_or(0);
	// The PermissionManager client: `perm` queries the permission audit trail (which
	// capabilities each launched component was and was not granted under its manifest)
	// and renders it as CLI / JSON. Sent right after GRAPH, matching ServiceManager's
	// send order.
	let permsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"PERM") }.unwrap_or(0);
	// The ResourceManager client: `usage` queries the live per-Domain resource budgets
	// (memory, handles, threads, IPC queue, DMA - used and limit) and renders them as CLI
	// / JSON. Sent right after PERM, matching ServiceManager's send order.
	let ressvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"RESOURCE") }.unwrap_or(0);
	// The session (SessionService) the shell runs under: the long-lived owner of the
	// working directory (and, later, the environment). `cd` round-trips to it and the
	// prompt reads its cwd, so the cwd survives a shell restart - the supervisor keeps the
	// session and hands each (re)started shell a fresh capability to the same one. Sent
	// right after RESOURCE, matching both spawn paths; 0 on a minimal boot with no session.
	let session: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"SESSION") }.unwrap_or(0);
	// The console channel to ConsoleService: the shell writes its output to it (routed
	// via stdout) and reads its keystrokes from it. The userspace terminal renders the
	// output and forwards the input, so the shell talks to the console, not the kernel.
	let console: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"CONSOLE") }.unwrap_or_else(|| exit());
	set_stdout(console);
	// The per-VT control channel to ConsoleService: the shell announces its foreground
	// job on it (SET_FG / CLEAR_FG) so the tty signals it on Ctrl+C / Ctrl+Z / Ctrl+\,
	// and learns of a Ctrl+Z suspend (JOB_STOPPED) so it can background the job.
	let control: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"CONTROL") }.unwrap_or_else(|| exit());
	// The admin channel to ServiceManager: `stop <service>` drives a reverse-dependency
	// teardown over it. Sent last, only by the ServiceManager-spawned VT 1 shell; other
	// shells run without it (`adminsvc` stays 0 and `stop` reports it is unavailable).
	let adminsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"ADMIN") }.unwrap_or(0);

	// 2. self-check: prove the StorageService round-trip works by reading a file.
	if !unsafe { cat(storage, SELF_CHECK_URI) } {
		exit();
	}

	// 3. report in once the service round-trip has succeeded.
	unsafe {
		send_blocking(bootstrap, b"Shell: online", 0);
	}

	// 4. greet the operator with the product banner (the message of the day), then
	//    become the interactive console and run the read-eval-print loop.
	print_motd();
	unsafe {
		repl(console, control, storage, media, iso, udf, logsvc, devsvc, procsvc, cfgsvc, netsvc, timesvc, audiosvc, inputsvc, graphsvc, permsvc, ressvc, session, adminsvc, &mut buf);
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
unsafe fn repl(console: u64, control: u64, storage: u64, media: u64, iso: u64, udf: u64, logsvc: u64, devsvc: u64, procsvc: u64, cfgsvc: u64, netsvc: u64, timesvc: u64, audiosvc: u64, inputsvc: u64, graphsvc: u64, permsvc: u64, ressvc: u64, session: u64, adminsvc: u64, buf: &mut [u8]) {
	unsafe {
		let mut jobs: Jobs = Jobs::new(control);
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
		loop {
			let n: usize = match recv_blocking(console, buf) {
				Received::Message { len, .. } => len,
				Received::Closed => return,
			};
			// A zero-byte read is the tty's EOF (Ctrl+D on an empty line): log out.
			if n == 0 {
				print(b"\n");
				return;
			}
			// The terminal delivers a whole submitted line (with a trailing newline);
			// trim it, dispatch it, reap finished jobs, and print the next prompt.
			let line: &[u8] = trim(&buf[..n]);
			if dispatch(line, storage, media, iso, udf, logsvc, devsvc, procsvc, cfgsvc, netsvc, timesvc, audiosvc, inputsvc, graphsvc, permsvc, ressvc, session, adminsvc, &mut jobs, &mut cwd) {
				return;
			}
			reap_jobs(&mut jobs);
			// the prompt shows the current working directory, so it sits in real storage.
			print(b"\x1b[1;32m");
			print(cwd.as_bytes());
			print(b"> \x1b[0m");
		}
	}
}

// Dispatch one command line. Returns true if the shell should exit.
// A background or suspended job the shell tracks: the child Process handle (which it
// both signals and waits on, the handle becoming ready once the process terminates),
// a display name, a small id, and whether it is currently stopped (suspended by
// Ctrl+Z).
struct Job {
	id: usize,
	proc: u64,
	name: Vec<u8>,
	stopped: bool,
}

// The shell's job-control state: the per-VT control channel to ConsoleService (the tty
// signals the foreground job over it and reports a Ctrl+Z suspend back), the tracked
// jobs, and the next id to assign.
struct Jobs {
	control: u64,
	list: Vec<Job>,
	next_id: usize,
}

impl Jobs {
	fn new(control: u64) -> Jobs {
		Jobs { control, list: Vec::new(), next_id: 1 }
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

// Resolve a `fg` / `bg` argument to a job-list index: an explicit id, or the most
// recent job when no id is given.
fn job_index(jobs: &Jobs, arg: &[u8]) -> Option<usize> {
	let arg = trim(arg);
	if arg.is_empty() {
		return if jobs.list.is_empty() { None } else { Some(jobs.list.len() - 1) };
	}
	let id = parse_usize(arg)?;
	jobs.list.iter().position(|j: &Job| j.id == id)
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
					let mut stopped = job;
					stopped.stopped = true;
					return Some(stopped);
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

// Poll each running (not stopped) job: once its Process handle reads ready it has
// terminated, so announce it and drop it. Called before each prompt, the way a shell
// reports a background job's completion.
unsafe fn reap_jobs(jobs: &mut Jobs) {
	unsafe {
		let mut i: usize = 0;
		while i < jobs.list.len() {
			if jobs.list[i].stopped {
				i += 1;
				continue;
			}
			if poll_ready(jobs.list[i].proc) {
				let job = jobs.list.remove(i);
				print(b"[");
				print_usize(job.id);
				print(b"] done   ");
				print(&job.name);
				print(b"\n");
				close(job.proc);
			} else {
				i += 1;
			}
		}
	}
}

// `jobs`: list the tracked background / stopped jobs.
unsafe fn list_jobs(jobs: &Jobs) {
	unsafe {
		if jobs.list.is_empty() {
			print(b"no jobs\n");
			return;
		}
		for job in &jobs.list {
			print(b"[");
			print_usize(job.id);
			print(b"] ");
			print(if job.stopped { b"stopped  " } else { b"running  " });
			print(&job.name);
			print(b"\n");
		}
	}
}

// `fg [id]`: bring a job to the foreground - resume it if stopped (SIG_CONT), then run
// it foreground again (so it can be interrupted / suspended once more).
unsafe fn fg_job(jobs: &mut Jobs, arg: &[u8]) {
	unsafe {
		let idx: usize = match job_index(jobs, arg) {
			Some(i) => i,
			None => {
				print(b"fg: no such job\n");
				return;
			}
		};
		let mut job: Job = jobs.list.remove(idx);
		if job.stopped {
			signal(job.proc, SIG_CONT);
			job.stopped = false;
		}
		print(&job.name);
		print(b"\n");
		if let Some(suspended) = run_foreground(jobs.control, job) {
			jobs.list.push(suspended);
		}
	}
}

// `bg [id]`: resume a stopped job in the background (SIG_CONT), leaving it tracked.
unsafe fn bg_job(jobs: &mut Jobs, arg: &[u8]) {
	unsafe {
		let idx: usize = match job_index(jobs, arg) {
			Some(i) => i,
			None => {
				print(b"bg: no such job\n");
				return;
			}
		};
		if !jobs.list[idx].stopped {
			print(b"bg: job already running\n");
			return;
		}
		signal(jobs.list[idx].proc, SIG_CONT);
		jobs.list[idx].stopped = false;
		print(b"[");
		print_usize(jobs.list[idx].id);
		print(b"] ");
		print(&jobs.list[idx].name);
		print(b" &\n");
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
// for a display mode-set until virtio-gpu drives it, M44), then report the new size from
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

unsafe fn dispatch(line: &[u8], storage: u64, media: u64, iso: u64, udf: u64, logsvc: u64, devsvc: u64, procsvc: u64, cfgsvc: u64, netsvc: u64, timesvc: u64, audiosvc: u64, inputsvc: u64, graphsvc: u64, permsvc: u64, ressvc: u64, session: u64, adminsvc: u64, jobs: &mut Jobs, cwd: &mut String) -> bool {
	unsafe {
		let line = trim(line);
		if line.is_empty() {
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
		if line == b"jobs" {
			list_jobs(jobs);
			return false;
		}
		if line == b"fg" {
			fg_job(jobs, b"");
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"fg ") {
			fg_job(jobs, trim(rest));
			return false;
		}
		if line == b"bg" {
			bg_job(jobs, b"");
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"bg ") {
			bg_job(jobs, trim(rest));
			return false;
		}
		if line == b"exit" || line == b"quit" {
			print(b"shell: exiting\n");
			return true;
		}
		if line == b"reboot" {
			system_power(POWER_REBOOT);
			print(b"reboot: failed\n");
			return false;
		}
		if line == b"poweroff" || line == b"shutdown" {
			system_power(POWER_OFF);
			print(b"poweroff: failed\n");
			return false;
		}
		if line == b"help" {
			print(b"commands:\n");
			print(b"  help             show this help\n");
			print(b"  clear            clear the screen\n");
			print(b"  size             show the terminal size (cols x rows)\n");
			print(b"  resize <c> <r>   resize the terminal to c cols x r rows\n");
			print(b"  echo <text>      print text\n");
			print(b"  cat <vol://...>  read a file via StorageService\n");
			print(b"  lsvol            list the available volumes via StorageService\n");
			print(b"  cd [<path>]      change the working directory (no argument returns home)\n");
			print(b"  ls [<path>]      list a directory's entries (the working directory by default)\n");
			print(b"  write <vol://...> <text>  create or overwrite a file via StorageService\n");
			print(b"  rm <vol://...>   delete a file via StorageService\n");
			print(b"  mkdir <vol://...>  create a directory via StorageService\n");
			print(b"  rmdir <vol://...>  remove an empty directory via StorageService\n");
			print(b"  snap [list]      list the volume's named snapshots via StorageService\n");
			print(b"  snap create <name>  pin a named read-only snapshot of the volume\n");
			print(b"  snap delete <name>  delete a named snapshot, releasing its blocks\n");
			print(b"  snap cat <name> <vol://...>  read a file from a snapshot (an earlier state)\n");
			print(b"  beep [hz] [ms]   play a tone via AudioService\n");
			print(b"  mouse            show recent pointer events via InputService\n");
			print(b"  script [<cmd>]   run a command in a fresh pty-hosted shell and record it\n");
			print(b"  log [json]       show the system journal via LogService\n");
			print(b"  log tail [json]  stream the journal via LogService (sub-channel)\n");
			print(b"  dev [json]       list devices via DeviceService\n");
			print(b"  graph [json|cbor]  show the live system graph and counters via SystemGraphService\n");
			print(b"  perm [json]      show the permission audit trail via PermissionManager\n");
			print(b"  usage [json]     show per-Domain resource budgets via ResourceManager\n");
			print(b"  stop <service>   stop a service and its dependents via ServiceManager\n");
			print(b"  ps               list started processes via ProcessService\n");
			print(b"  run <name>       start a program via ProcessService\n");
			print(b"  config [<key>]   list the config tree or read one key via ConfigService\n");
			print(b"  set <key> <val>  write a config key via ConfigService\n");
			print(b"  ip | net         show the network interface and ARP cache\n");
			print(b"  ping [-c n] [--json] <host>  ICMP echo a host (name or address); --json emits a JSON document\n");
			print(b"  nslookup <name>  resolve a name to an address via DNS\n");
			print(b"  tcp <ip> <port>  open a TCP connection and probe it (HTTP GET)\n");
			print(b"  nc <ip> <port>   open a raw TCP connection (optional request to send)\n");
			print(b"  arp              show the ARP / neighbor cache\n");
			print(b"  ss | netstat     list the live sockets\n");
			print(b"  httpd            serve HTTP on port 80 (background)\n");
			print(b"  <cmd> &          run a command in the background\n");
			print(b"  jobs             list background / stopped jobs\n");
			print(b"  fg [id]          resume a job in the foreground\n");
			print(b"  bg [id]          resume a stopped job in the background\n");
			print(b"  Ctrl+C / Ctrl+Z  interrupt / suspend the foreground job\n");
			print(b"  Ctrl+\\           terminate the foreground job\n");
			print(b"  Ctrl+D           end input (log out) at an empty prompt\n");
			print(b"  reboot           reboot the machine\n");
			print(b"  poweroff         power the machine off\n");
			print(b"  exit             stop the shell and halt\n");
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
		if line == b"log" {
			// Launch `log` as its own sandboxed ELF through PermissionManager (the launcher /
			// granter), which grants it a log client and a time client and forwards it this
			// terminal and the sub-form argument. A shell with no PermissionManager (a non-primary
			// VT) queries the journal inline instead.
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"log", b"", cwd.as_bytes());
			if !launched {
				query_log(logsvc, timesvc, false);
			}
			return false;
		}
		if line == b"log json" {
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"log", b"json", cwd.as_bytes());
			if !launched {
				query_log(logsvc, timesvc, true);
			}
			return false;
		}
		if line == b"log tail" {
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"log", b"tail", cwd.as_bytes());
			if !launched {
				tail_log(logsvc, timesvc, false);
			}
			return false;
		}
		if line == b"log tail json" {
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"log", b"tail json", cwd.as_bytes());
			if !launched {
				tail_log(logsvc, timesvc, true);
			}
			return false;
		}
		if line == b"dev" {
			// Launch `dev` as its own sandboxed ELF through PermissionManager (the launcher /
			// granter), which grants it a device client and forwards it this terminal and the
			// sub-form argument. A shell with no PermissionManager (a non-primary VT) queries the
			// devices inline instead.
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"dev", b"", cwd.as_bytes());
			if !launched {
				query_devices(devsvc, false);
			}
			return false;
		}
		if line == b"dev json" {
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"dev", b"json", cwd.as_bytes());
			if !launched {
				query_devices(devsvc, true);
			}
			return false;
		}
		if line == b"graph" {
			query_graph(graphsvc, GraphFmt::Text);
			return false;
		}
		if line == b"graph json" {
			query_graph(graphsvc, GraphFmt::Json);
			return false;
		}
		if line == b"graph cbor" {
			query_graph(graphsvc, GraphFmt::Cbor);
			return false;
		}
		if line == b"perm" {
			// Launch `perm` as its own sandboxed ELF through PermissionManager (the launcher /
			// granter), which grants it a client to its own serve channel and forwards it this
			// terminal and the sub-form argument. A shell with no PermissionManager (a non-primary
			// VT) reads the audit trail inline instead.
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"perm", b"", cwd.as_bytes());
			if !launched {
				query_permission(permsvc, false);
			}
			return false;
		}
		if line == b"perm json" {
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"perm", b"json", cwd.as_bytes());
			if !launched {
				query_permission(permsvc, true);
			}
			return false;
		}
		if line == b"usage" {
			// Launch `usage` as its own sandboxed ELF through PermissionManager (the launcher /
			// granter), which grants it a resource client and forwards it this terminal and the
			// sub-form argument. A shell with no PermissionManager (a non-primary VT) reads the
			// budgets inline instead.
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"usage", b"", cwd.as_bytes());
			if !launched {
				query_resource(ressvc, false);
			}
			return false;
		}
		if line == b"usage json" {
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"usage", b"json", cwd.as_bytes());
			if !launched {
				query_resource(ressvc, true);
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"stop ") {
			// Launch `stop` as its own sandboxed ELF through PermissionManager (the launcher /
			// granter), which grants it a ServiceManager admin channel and forwards it this
			// terminal and the service name. A shell with no PermissionManager (a non-primary VT)
			// stops the service inline instead.
			let name: &[u8] = trim(rest);
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"stop", name, cwd.as_bytes());
			if !launched {
				stop_service(adminsvc, name);
			}
			return false;
		}
		if line == b"ps" {
			// Launch `ps` as its own sandboxed ELF through PermissionManager (the launcher /
			// granter), which grants it a process client and forwards it this terminal. A shell
			// with no PermissionManager (a non-primary VT) lists the processes inline instead.
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"ps", b"", cwd.as_bytes());
			if !launched {
				query_processes(procsvc);
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"run ") {
			// Launch `run` as its own sandboxed ELF through PermissionManager (the launcher /
			// granter), which grants it a process client and forwards it this terminal and the
			// program name. A shell with no PermissionManager (a non-primary VT) starts the
			// program inline instead.
			let name: &[u8] = trim(rest);
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"run", name, cwd.as_bytes());
			if !launched {
				run_process(procsvc, name);
			}
			return false;
		}
		if line == b"config" {
			// Launch `config` as its own sandboxed ELF through PermissionManager (the launcher /
			// granter), which grants it a config client and forwards it this terminal and the
			// sub-form argument. A shell with no PermissionManager (a non-primary VT) queries the
			// store inline instead.
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"config", b"", cwd.as_bytes());
			if !launched {
				query_config(cfgsvc);
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"config ") {
			let key: &[u8] = trim(rest);
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"config", key, cwd.as_bytes());
			if !launched {
				get_config(cfgsvc, key);
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"set ") {
			let args: &[u8] = trim(rest);
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"set", args, cwd.as_bytes());
			if !launched {
				set_config(cfgsvc, args);
			}
			return false;
		}
		if line == b"date" {
			// Launch `date` as its own sandboxed ELF through PermissionManager (the launcher /
			// granter), which grants it just a time client and forwards it this terminal. A
			// shell with no PermissionManager (a non-primary VT) reads the clock inline instead.
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"date", b"", cwd.as_bytes());
			if !launched {
				show_date(timesvc);
			}
			return false;
		}
		if line == b"beep" {
			// Launch `beep` as its own sandboxed ELF through PermissionManager (the launcher /
			// granter), which grants it an audio client and forwards it this terminal and the
			// argument string. A shell with no PermissionManager (a non-primary VT) plays the
			// tone inline instead.
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"beep", b"", cwd.as_bytes());
			if !launched {
				beep_cmd(audiosvc, b"");
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"beep ") {
			let args: &[u8] = trim(rest);
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"beep", args, cwd.as_bytes());
			if !launched {
				beep_cmd(audiosvc, args);
			}
			return false;
		}
		if line == b"mouse" {
			mouse_cmd(inputsvc);
			return false;
		}
		if line == b"ip" || line == b"net" {
			spawn_net_tool(jobs, netsvc, procsvc, b"ip", b"", bg);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"ping ") {
			spawn_net_tool(jobs, netsvc, procsvc, b"ping", trim(rest), bg);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"nslookup ") {
			spawn_net_tool(jobs, netsvc, procsvc, b"nslookup", trim(rest), bg);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"host ") {
			spawn_net_tool(jobs, netsvc, procsvc, b"nslookup", trim(rest), bg);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"tcp ") {
			spawn_net_tool(jobs, netsvc, procsvc, b"tcp", trim(rest), bg);
			return false;
		}
		if line == b"arp" {
			spawn_net_tool(jobs, netsvc, procsvc, b"arp", b"", bg);
			return false;
		}
		if line == b"ss" || line == b"netstat" {
			spawn_net_tool(jobs, netsvc, procsvc, b"ss", b"", bg);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"nc ") {
			spawn_net_tool(jobs, netsvc, procsvc, b"nc", trim(rest), bg);
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
		if line == b"lsvol" {
			// Launch `lsvol` as its own sandboxed ELF through PermissionManager (the launcher /
			// granter), which grants it the four volume StorageService clients (the `volumes`
			// capability) and forwards it this terminal. A shell with no PermissionManager (a
			// non-primary VT) lists the volumes inline instead.
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"lsvol", b"", cwd.as_bytes());
			if !launched {
				lsvol_cmd(storage, media, iso, udf);
			}
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
			cd_cmd(cwd, trim(rest), session, storage, media, iso, udf);
			return false;
		}
		if line == b"ls" {
			// no argument lists the current working directory; route through PermissionManager
			// (the governed ELF, which inherits this cwd and resolves it) for the system volume,
			// falling back to the inline listing on other volumes or a shell with no
			// PermissionManager.
			if on_system_volume(cwd, b"") && permsvc != 0 {
				run_tool(permsvc, b"ls", b"", cwd.as_bytes());
			} else {
				ls_cmd(storage, media, iso, udf, cwd.as_bytes());
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"ls ") {
			let arg: &[u8] = trim(rest);
			// For a system-volume directory, launch `ls` as its own sandboxed ELF through
			// PermissionManager (the launcher / granter): it inherits this cwd, resolves the
			// (relative or absolute) path itself, and reports its own result. Other volumes
			// (media / iso / udf), and a shell with no PermissionManager (a non-primary VT), list
			// the directory inline instead - resolving the path here against the same cwd.
			if on_system_volume(cwd, arg) && permsvc != 0 {
				run_tool(permsvc, b"ls", arg, cwd.as_bytes());
			} else {
				match path::resolve(cwd, arg) {
					Some(uri) => ls_cmd(storage, media, iso, udf, uri.as_bytes()),
					None => print(b"ls: invalid path\n"),
				}
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"cat ") {
			let arg: &[u8] = trim(rest);
			// For a system-volume file, launch `cat` as its own sandboxed ELF through
			// PermissionManager (the launcher / granter): it inherits this cwd, resolves the path
			// itself, and reports its own errors. Other volumes (media / iso / udf), and a shell
			// with no PermissionManager (a non-primary VT), read the file inline instead.
			if on_system_volume(cwd, arg) && permsvc != 0 {
				run_tool(permsvc, b"cat", arg, cwd.as_bytes());
			} else {
				match path::resolve(cwd, arg) {
					Some(uri) => {
						let chan: u64 = storage_for(uri.as_bytes(), storage, media, iso, udf);
						if !cat(chan, uri.as_bytes()) {
							print(b"cat: could not read ");
							print(uri.as_bytes());
							print(b"\n");
						}
					}
					None => print(b"cat: invalid path\n"),
				}
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"write ") {
			let rest = trim(rest);
			// "write <path> <text>": split on the first space.
			match rest.iter().position(|&b: &u8| b == b' ') {
				Some(sp) => {
					let path_arg: &[u8] = trim(&rest[..sp]);
					let text: &[u8] = trim(&rest[sp + 1..]);
					// For a system-volume file, launch `write` as its own sandboxed ELF through
					// PermissionManager (the launcher / granter): it inherits this cwd, resolves the
					// path from the "<path> <text>" argument itself, and reports its own result. Other
					// volumes (media / iso / udf), and a shell with no PermissionManager (a non-primary
					// VT), write inline.
					if on_system_volume(cwd, path_arg) && permsvc != 0 {
						let mut arg: Vec<u8> = Vec::with_capacity(path_arg.len() + 1 + text.len());
						arg.extend_from_slice(path_arg);
						arg.push(b' ');
						arg.extend_from_slice(text);
						run_tool(permsvc, b"write", &arg, cwd.as_bytes());
					} else {
						match path::resolve(cwd, path_arg) {
							Some(uri) => {
								let chan: u64 = storage_for(uri.as_bytes(), storage, media, iso, udf);
								write_cmd(chan, uri.as_bytes(), text);
							}
							None => print(b"write: invalid path\n"),
						}
					}
				}
				None => print(b"usage: write <path> <text>\n"),
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"rm ") {
			let arg: &[u8] = trim(rest);
			// For a system-volume file, launch `rm` as its own sandboxed ELF through
			// PermissionManager (the launcher / granter): it inherits this cwd, resolves the path
			// itself, and reports its own result. Other volumes (media / iso / udf), and a shell
			// with no PermissionManager (a non-primary VT), remove the file inline instead.
			if on_system_volume(cwd, arg) && permsvc != 0 {
				run_tool(permsvc, b"rm", arg, cwd.as_bytes());
			} else {
				match path::resolve(cwd, arg) {
					Some(uri) => {
						let chan: u64 = storage_for(uri.as_bytes(), storage, media, iso, udf);
						rm_cmd(chan, uri.as_bytes());
					}
					None => print(b"rm: invalid path\n"),
				}
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"mkdir ") {
			let arg: &[u8] = trim(rest);
			// For a system-volume directory, launch `mkdir` as its own sandboxed ELF through
			// PermissionManager (the launcher / granter): it inherits this cwd, resolves the path
			// itself, and reports its own result. Other volumes (media / iso / udf), and a shell
			// with no PermissionManager (a non-primary VT), create the directory inline instead.
			if on_system_volume(cwd, arg) && permsvc != 0 {
				run_tool(permsvc, b"mkdir", arg, cwd.as_bytes());
			} else {
				match path::resolve(cwd, arg) {
					Some(uri) => {
						let chan: u64 = storage_for(uri.as_bytes(), storage, media, iso, udf);
						mkdir_cmd(chan, uri.as_bytes());
					}
					None => print(b"mkdir: invalid path\n"),
				}
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"rmdir ") {
			let arg: &[u8] = trim(rest);
			// For a system-volume directory, launch `rmdir` as its own sandboxed ELF through
			// PermissionManager (the launcher / granter): it inherits this cwd, resolves the path
			// itself, and reports its own result. Other volumes (media / iso / udf), and a shell
			// with no PermissionManager (a non-primary VT), remove the directory inline instead.
			if on_system_volume(cwd, arg) && permsvc != 0 {
				run_tool(permsvc, b"rmdir", arg, cwd.as_bytes());
			} else {
				match path::resolve(cwd, arg) {
					Some(uri) => {
						let chan: u64 = storage_for(uri.as_bytes(), storage, media, iso, udf);
						rmdir_cmd(chan, uri.as_bytes());
					}
					None => print(b"rmdir: invalid path\n"),
				}
			}
			return false;
		}
		if line == b"snap" || line == b"snap list" {
			// Launch `snap` as its own sandboxed ELF through PermissionManager (the launcher /
			// granter), which grants it a storage client and forwards it this terminal and the
			// snapshot sub-form. A shell with no PermissionManager (a non-primary VT) manages
			// snapshots inline instead.
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"snap", b"list", cwd.as_bytes());
			if !launched {
				snap_list_cmd(storage);
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"snap create ") {
			let name: &[u8] = trim(rest);
			let mut arg: Vec<u8> = Vec::with_capacity(7 + name.len());
			arg.extend_from_slice(b"create ");
			arg.extend_from_slice(name);
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"snap", &arg, cwd.as_bytes());
			if !launched {
				snap_create_cmd(storage, name);
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"snap delete ") {
			let name: &[u8] = trim(rest);
			let mut arg: Vec<u8> = Vec::with_capacity(7 + name.len());
			arg.extend_from_slice(b"delete ");
			arg.extend_from_slice(name);
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"snap", &arg, cwd.as_bytes());
			if !launched {
				snap_delete_cmd(storage, name);
			}
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"snap cat ") {
			let rest = trim(rest);
			let mut arg: Vec<u8> = Vec::with_capacity(4 + rest.len());
			arg.extend_from_slice(b"cat ");
			arg.extend_from_slice(rest);
			let launched: bool = permsvc != 0 && run_tool(permsvc, b"snap", &arg, cwd.as_bytes());
			if !launched {
				// "snap cat <name> <vol://...>": split on the first space.
				match rest.iter().position(|&b: &u8| b == b' ') {
					Some(sp) => {
						let uri = trim(&rest[sp + 1..]);
						if !snap_cat(storage, &rest[..sp], uri) {
							print(b"snap cat: could not read ");
							print(uri);
							print(b"\n");
						}
					}
					None => print(b"usage: snap cat <name> <vol://...>\n"),
				}
			}
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
		print(b"\x1b[31munknown command: ");
		print(line);
		print(b" (try 'help')\x1b[0m\n");
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
		// Hand the child our console as its stdout (a SEND dup of our console channel), then
		// its arguments + an optional inherited capability (e.g. a NetworkService client).
		send_stdout(parent);
		send_blocking(parent, args, cap);
		// The bootstrap is delivered; the child drains it from its own end, so the shell no
		// longer needs the parent end. Drop it - the shell now tracks the job solely by its
		// waitable Process handle (ready once the child terminates), not a completion channel.
		close(parent);
		let id: usize = jobs.next_id;
		jobs.next_id += 1;
		let job: Job = Job { id, proc, name: name.to_vec(), stopped: false };
		if bg {
			// Background: track the job and return to the prompt; its completion is reaped
			// before a later prompt.
			print(b"[");
			print_usize(id);
			print(b"] ");
			print(name);
			print(b" &\n");
			jobs.list.push(job);
		} else if let Some(suspended) = run_foreground(jobs.control, job) {
			// Suspended by Ctrl+Z: keep it as a stopped background job.
			jobs.list.push(suspended);
		}
	}
}

// Hand a freshly spawned child our console as its stdout: a SEND dup of our console
// channel transferred in a "STDOUT" message (the child's `rt::inherit_stdout` adopts
// it), so the program's `print` output renders on the same terminal. Sent before the
// argv/capability message. A handle of 0 (no console) leaves the child on serial.
unsafe fn send_stdout(parent: u64) {
	unsafe {
		let so: u64 = stdout();
		let dup: u64 = if so != 0 {
			let d: i64 = duplicate(so, RIGHT_SEND | RIGHT_TRANSFER);
			if d > 0 {
				d as u64
			} else {
				0
			}
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
// runs with exactly its manifest's capabilities. Foreground only this milestone (no
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
		let mut obuf: [u8; 1024] = [0u8; 1024];
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

// `beep [hz] [ms]`: play a tone via AudioService. Both arguments are optional and
// default to a 440 Hz tone for 200 ms; AudioService clamps them to its supported
// range. A bare "no audio device" error is reported when the system has no virtio-
// sound device (e.g. under test), so the command degrades cleanly without one.
unsafe fn beep_cmd(audiosvc: u64, args: &[u8]) {
	unsafe {
		let mut freq: u16 = 440;
		let mut millis: u32 = 200;
		let mut parts = args.split(|&b| b == b' ').filter(|s: &&[u8]| !s.is_empty());
		if let Some(f) = parts.next() {
			match parse_usize(f) {
				Some(v) => freq = v.min(u16::MAX as usize) as u16,
				None => {
					print(b"beep: invalid frequency\n");
					return;
				}
			}
		}
		if let Some(m) = parts.next() {
			match parse_usize(m) {
				Some(v) => millis = v.min(u32::MAX as usize) as u32,
				None => {
					print(b"beep: invalid duration\n");
					return;
				}
			}
		}
		let mut client = audio::Client::new(ChannelTransport { chan: audiosvc });
		match client.beep(&freq, &millis) {
			Some(Ok(())) => {}
			Some(Err(_)) => print(b"beep: no audio device\n"),
			None => print(b"beep: service unavailable\n"),
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
				Received::Message { len, .. } => {
					if let Some(event) = input::subscribe_read(&buf[..len]) {
						print(b"  (");
						print_usize(event.col as usize);
						print(b", ");
						print_usize(event.row as usize);
						print(b") buttons=");
						print_usize(event.buttons as usize);
						print(b"\n");
						count += 1;
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

// The representation the `graph` command renders the snapshot in: human-readable text
// (the default), a JSON document, or a CBOR document shown as hex. The JSON and CBOR
// forms are the same bytes a remote consumer would read off the wire in a later phase.
enum GraphFmt {
	Text,
	Json,
	Cbor,
}

// Query SystemGraphService for the live system graph over the generated client and
// render it in the requested representation: the components (each with its kind,
// state, dependency edges, and live counters) and the trace spans, each via the
// generated to_text / to_json / to_cbor on the client side - the one typed API, many
// representations rule. A 0 handle means the service is not wired (e.g. a non-primary
// VT this milestone), reported as unavailable rather than blocking.
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
				GraphFmt::Json => {
					print(graph.to_json().as_bytes());
					print(b"\n");
				}
				GraphFmt::Cbor => print_hex(&graph.to_cbor()),
			},
			Some(Err(_)) => print(b"graph: query error\n"),
			None => print(b"graph: service unavailable\n"),
		}
	}
}

// Query PermissionManager for the permission audit trail over the generated Permission
// client and print it, rendering each typed audit entry (component, capability, whether
// it was granted) to text or JSON on the client side. The trail records every grant
// decision the manager made when it launched a component under its manifest - the
// executable record of the enforced sandbox. A 0 handle means the manager is not wired
// (a non-primary VT), reported as unavailable rather than blocking.
unsafe fn query_permission(permsvc: u64, json: bool) {
	unsafe {
		if permsvc == 0 {
			print(b"perm: service unavailable\n");
			return;
		}
		let mut client = permission::Client::new(ChannelTransport { chan: permsvc });
		match client.audit() {
			Some(Ok(entries)) => {
				if json {
					print_json_array(&entries, |e: &AuditEntry| -> String { e.to_json() });
				} else {
					print_text_lines(&entries, |e: &AuditEntry| -> String { e.to_text() });
				}
			}
			Some(Err(_)) => print(b"perm: query error\n"),
			None => print(b"perm: service unavailable\n"),
		}
	}
}

// Ask ResourceManager for the live per-Domain budgets and render them: as JSON (the
// generated wire form, one document per budget) or as a compact text table - one line
// per budget, each resource shown as `kind=used/limit`, with an unlimited limit
// (u64::MAX, the kernel's UNLIMITED sentinel) shown as `unlimited` rather than the raw
// number. A 0 handle means this shell was not granted the ResourceManager client.
unsafe fn query_resource(ressvc: u64, json: bool) {
	unsafe {
		if ressvc == 0 {
			print(b"usage: service unavailable\n");
			return;
		}
		let mut client = resources::Client::new(ChannelTransport { chan: ressvc });
		match client.usage() {
			Some(Ok(budgets)) => {
				if json {
					print_json_array(&budgets, |b: &Budget| -> String { b.to_json() });
				} else {
					for b in budgets.iter() {
						print_budget(b);
					}
				}
			}
			Some(Err(_)) => print(b"usage: query error\n"),
			None => print(b"usage: service unavailable\n"),
		}
	}
}

// Render one budget as a compact text line: `<name>: kind=used/limit ...`, with the
// kernel's UNLIMITED sentinel (u64::MAX) shown as `unlimited`.
unsafe fn print_budget(budget: &Budget) {
	unsafe {
		let mut line = String::new();
		line.push_str(&budget.name);
		line.push(':');
		for u in budget.usage.iter() {
			line.push(' ');
			line.push_str(&u.kind.to_text());
			line.push('=');
			push_amount(&mut line, u.used);
			line.push('/');
			push_amount(&mut line, u.limit);
		}
		line.push('\n');
		print(line.as_bytes());
	}
}

// Append a resource amount, rendering the kernel's UNLIMITED sentinel (u64::MAX) as
// `unlimited` rather than the raw 64-bit number.
fn push_amount(out: &mut String, value: u64) {
	use core::fmt::Write as _;
	if value == u64::MAX {
		out.push_str("unlimited");
	} else {
		let _ = write!(out, "{value}");
	}
}

// Ask ServiceManager to stop a service and its dependents: send the bare service name
// over the admin channel and print the reply - the newline-joined teardown order on
// success, or a not-found notice. A 0 handle means this shell was not granted the admin
// channel (only VT 1 is), reported as unavailable rather than blocking.
unsafe fn stop_service(adminsvc: u64, name: &[u8]) {
	unsafe {
		if adminsvc == 0 {
			print(b"stop: service unavailable\n");
			return;
		}
		if name.is_empty() {
			print(b"stop: usage: stop <service>\n");
			return;
		}
		if !send_blocking(adminsvc, name, 0) {
			print(b"stop: request failed\n");
			return;
		}
		let mut rbuf: [u8; 512] = [0u8; 512];
		match recv_blocking(adminsvc, &mut rbuf) {
			Received::Message { len, .. } => {
				if rbuf[..len].starts_with(b"STOPPED\n") {
					print(b"stopped:\n");
					print(&rbuf[8..len]);
					print(b"\n");
				} else if len >= 8 && &rbuf[..8] == b"NOTFOUND" {
					print(b"stop: no such running service\n");
				} else {
					print(&rbuf[..len]);
					print(b"\n");
				}
			}
			Received::Closed => print(b"stop: supervisor gone\n"),
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
	while let [first, rest @ ..] = s {
		if first.is_ascii_whitespace() {
			s = rest;
		} else {
			break;
		}
	}
	while let [rest @ .., last] = s {
		if last.is_ascii_whitespace() {
			s = rest;
		} else {
			break;
		}
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

// List the available volumes via the StorageService `list` op: print the volume set with
// a per-volume file count. The volumes are `system` (writable LiberFS), `media`
// (FAT12/16/32 or exFAT off a second disk), `iso` (read-only ISO9660 off a third disk),
// and `udf` (read-only UDF off a fourth disk).
unsafe fn lsvol_cmd(storage: u64, media: u64, iso: u64, udf: u64) {
	unsafe {
		let sys: usize = volume_count(storage, "vol://system");
		let med: usize = volume_count(media, "vol://media");
		let opt: usize = volume_count(iso, "vol://iso");
		let dvd: usize = volume_count(udf, "vol://udf");
		print(b"volumes (4):\n  vol://system (");
		print_usize(sys);
		print(b" files)\n  vol://media (");
		print_usize(med);
		print(b" files)\n  vol://iso (");
		print_usize(opt);
		print(b" files)\n  vol://udf (");
		print_usize(dvd);
		print(b" files)\n");
	}
}

// List the entries of a directory via the StorageService `list` op: `vol://<volume>`
// lists that volume's root, `vol://<volume>/<subdir>` a subdirectory (name + size, with
// a trailing `/` marking a subdirectory). `lsvol` lists the volume set itself.
unsafe fn ls_cmd(storage: u64, media: u64, iso: u64, udf: u64, arg: &[u8]) {
	unsafe {
		// the URI is vol://<volume>[/<subdir>]; the volume picks the channel, the whole
		// URI names the directory to list.
		let rest: &[u8] = match arg.strip_prefix(b"vol://") {
			Some(r) => r,
			None => {
				print(b"ls: unknown volume\n");
				return;
			}
		};
		let vol: &[u8] = match rest.iter().position(|&b: &u8| b == b'/') {
			Some(i) => &rest[..i],
			None => rest,
		};
		let chan: u64 = match vol {
			b"system" => storage,
			b"media" => media,
			b"iso" => iso,
			b"udf" => udf,
			_ => {
				print(b"ls: unknown volume\n");
				return;
			}
		};
		let uri: &str = match core::str::from_utf8(arg) {
			Ok(s) => s,
			Err(_) => {
				print(b"ls: invalid path\n");
				return;
			}
		};
		let mut client = volume::Client::new(ChannelTransport { chan });
		let files = match client.list(uri) {
			Some(Ok(f)) => f,
			_ => {
				print(b"ls: StorageService unavailable\n");
				return;
			}
		};
		print(arg);
		print(b" (");
		print_usize(files.len());
		print(b" entries):\n");
		for f in &files {
			print(b"  ");
			print(f.name.as_bytes());
			match f.kind {
				FileKind::Dir => print(b"/\n"),
				FileKind::File => {
					print(b" ");
					print_usize(f.size as usize);
					print(b" bytes\n");
				}
			}
		}
	}
}

// Count the files on a volume, for the `lsvol` overview; 0 if the service is unavailable.
unsafe fn volume_count(storage: u64, uri: &str) -> usize {
	let mut client = volume::Client::new(ChannelTransport { chan: storage });
	match client.list(uri) {
		Some(Ok(f)) => f.len(),
		_ => 0,
	}
}

// Pick the StorageService client for a vol:// URI: vol://media is the FAT media
// disk, vol://iso the ISO9660 disk, vol://udf the UDF disk, everything else the system
// volume.
fn storage_for(uri: &[u8], storage: u64, media: u64, iso: u64, udf: u64) -> u64 {
	let v: Option<&[u8]> = uri.strip_prefix(b"vol://");
	if v.map(|r: &[u8]| r.starts_with(b"media/") || r == b"media").unwrap_or(false) {
		media
	} else if v.map(|r: &[u8]| r.starts_with(b"iso/") || r == b"iso").unwrap_or(false) {
		iso
	} else if v.map(|r: &[u8]| r.starts_with(b"udf/") || r == b"udf").unwrap_or(false) {
		udf
	} else {
		storage
	}
}

// True when a path argument resolves onto the system volume (a relative path inherits the
// cwd's volume) - the one volume PermissionManager-launched tools are granted. The routing
// test for the governed-ELF path: it parses only the volume, leaving the full resolution to
// the tool itself.
fn on_system_volume(cwd: &str, arg: &[u8]) -> bool {
	matches!(path::volume(cwd, arg), Some(v) if v == b"system")
}

// Change the working directory. The target is resolved against the current cwd and
// must be an existing directory, which we confirm by listing it through the owning
// StorageService; only then does the prompt move there.
unsafe fn cd_cmd(cwd: &mut String, arg: &[u8], session: u64, storage: u64, media: u64, iso: u64, udf: u64) {
	unsafe {
		let target: String = match path::resolve(cwd, arg) {
			Some(t) => t,
			None => {
				print(b"cd: invalid path\n");
				return;
			}
		};
		let chan: u64 = storage_for(target.as_bytes(), storage, media, iso, udf);
		let mut client = volume::Client::new(ChannelTransport { chan });
		match client.list(&target) {
			Some(Ok(_)) => {
				cwd.clear();
				cwd.push_str(&target);
				// Persist the new cwd in the session so it outlives this shell; the local
				// cache above is what the prompt and path resolution read each line.
				if session != 0 {
					let _ = session::Client::new(ChannelTransport { chan: session }).chdir(&target);
				}
			}
			_ => {
				print(b"cd: not a directory: ");
				print(target.as_bytes());
				print(b"\n");
			}
		}
	}
}
// text is staged in a fresh read-only shared buffer and handed over out-of-band as a
// zero-copy `buffer`; the service writes it to the on-disk filesystem, so it survives
// a reboot.
unsafe fn write_cmd(storage: u64, uri: &[u8], text: &[u8]) {
	unsafe {
		let data: proto::codec::Buffer = match make_buffer(text) {
			Some(b) => b,
			None => {
				print(b"write: out of memory\n");
				return;
			}
		};
		let path: String = String::from_utf8_lossy(uri).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		match client.write(&path, &data) {
			Some(Ok(())) => {
				print(b"wrote ");
				print(uri);
				print(b"\n");
			}
			_ => {
				print(b"write: could not write ");
				print(uri);
				print(b"\n");
			}
		}
	}
}

// Delete a file on the volume via the StorageService `remove` op.
unsafe fn rm_cmd(storage: u64, uri: &[u8]) {
	unsafe {
		let path: String = String::from_utf8_lossy(uri).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		match client.remove(&path) {
			Some(Ok(())) => {
				print(b"removed ");
				print(uri);
				print(b"\n");
			}
			_ => {
				print(b"rm: could not remove ");
				print(uri);
				print(b"\n");
			}
		}
	}
}

// Create a directory on the volume via the StorageService `mkdir` op, making any
// missing parents (mkdir -p).
unsafe fn mkdir_cmd(storage: u64, uri: &[u8]) {
	unsafe {
		let path: String = String::from_utf8_lossy(uri).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		match client.mkdir(&path) {
			Some(Ok(())) => {
				print(b"created ");
				print(uri);
				print(b"\n");
			}
			_ => {
				print(b"mkdir: could not create ");
				print(uri);
				print(b"\n");
			}
		}
	}
}

// Remove an empty directory on the volume via the StorageService `rmdir` op.
unsafe fn rmdir_cmd(storage: u64, uri: &[u8]) {
	unsafe {
		let path: String = String::from_utf8_lossy(uri).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		match client.rmdir(&path) {
			Some(Ok(())) => {
				print(b"removed ");
				print(uri);
				print(b"\n");
			}
			_ => {
				print(b"rmdir: could not remove ");
				print(uri);
				print(b"\n");
			}
		}
	}
}

// List the volume's named snapshots via the StorageService `snap-list` op (each as
// name + pinned generation), oldest first.
unsafe fn snap_list_cmd(storage: u64) {
	unsafe {
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let snaps = match client.snap_list() {
			Some(Ok(s)) => s,
			_ => {
				print(b"snap: StorageService unavailable\n");
				return;
			}
		};
		print(b"snapshots (");
		print_usize(snaps.len());
		print(b"):\n");
		for s in &snaps {
			print(b"  ");
			print(s.name.as_bytes());
			print(b" (generation ");
			print_usize(s.generation as usize);
			print(b")\n");
		}
	}
}

// Create a named read-only snapshot of the volume via the `snap-create` op, pinning
// the current state so a later `snap cat` can read it.
unsafe fn snap_create_cmd(storage: u64, name: &[u8]) {
	unsafe {
		let snapshot: String = String::from_utf8_lossy(name).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		match client.snap_create(&snapshot) {
			Some(Ok(())) => {
				print(b"created snapshot ");
				print(name);
				print(b"\n");
			}
			_ => {
				print(b"snap create: could not create ");
				print(name);
				print(b"\n");
			}
		}
	}
}

// Delete a named snapshot via the `snap-delete` op, releasing the blocks it pinned.
unsafe fn snap_delete_cmd(storage: u64, name: &[u8]) {
	unsafe {
		let snapshot: String = String::from_utf8_lossy(name).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		match client.snap_delete(&snapshot) {
			Some(Ok(())) => {
				print(b"deleted snapshot ");
				print(name);
				print(b"\n");
			}
			_ => {
				print(b"snap delete: could not delete ");
				print(name);
				print(b"\n");
			}
		}
	}
}

// Read a file from inside a named snapshot via the `snap-open` op, printing an earlier
// state of the volume - the snapshot counterpart of `cat`.
unsafe fn snap_cat(storage: u64, name: &[u8], uri: &[u8]) -> bool {
	unsafe {
		let snapshot: String = String::from_utf8_lossy(name).into_owned();
		let path: String = String::from_utf8_lossy(uri).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let result = match client.snap_open(&snapshot, &path) {
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

// Stage `bytes` in a fresh MemoryObject and return a transferable read-only buffer
// (read + map + transfer) over it for a zero-copy `write`. The caller passes the
// buffer to the generated client, whose send consumes the handle. An empty write
// still allocates a one-byte object (length 0) so the create cannot fail on a
// zero-length request.
unsafe fn make_buffer(bytes: &[u8]) -> Option<proto::codec::Buffer> {
	unsafe {
		let alloc_len: usize = bytes.len().max(1);
		let obj: u64 = syscall(SYS_MEMORY_OBJECT_CREATE, alloc_len as u64, 0, 0, 0);
		if sys_is_err(obj) {
			return None;
		}
		let mapped: u64 = match map_object(obj) {
			Some(base) => base,
			None => {
				close(obj);
				return None;
			}
		};
		core::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped as *mut u8, bytes.len());
		unmap_object(obj);
		let granted: i64 = duplicate(obj, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		close(obj);
		if granted < 0 {
			return None;
		}
		Some(proto::codec::Buffer { handle: granted as u64, len: bytes.len() as u64 })
	}
}
