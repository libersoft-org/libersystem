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
use proto::system::{audio, config, device, input, log, network, process, system_graph, time, volume, Component, ConfigEntry, DeviceEntry, Entry, OpenOpts, ProcessInfo, Query, Timestamp, TraceSpan};
use rt::*;

// the file the shell reads at startup to prove the StorageService round-trip works
const SELF_CHECK_URI: &[u8] = b"vol://system/hello.txt";

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. receive the per-service client channels from ServiceManager, in the order it
	//    sends them: storage (`cat`), log (`log`), device (`dev`), process (`ps`/`run`),
	//    config (`config`/`set`), network (`ip`/`ping`/...), time (`date`), audio (`beep`),
	//    then a read-only view of the init package. Each is a tagged capability over the
	//    bootstrap channel.
	let storage: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"STORAGE") }.unwrap_or_else(|| exit());
	let logsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"LOG") }.unwrap_or_else(|| exit());
	let devsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"DEVICE") }.unwrap_or_else(|| exit());
	let procsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"PROCESS") }.unwrap_or_else(|| exit());
	let cfgsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"CONFIG") }.unwrap_or_else(|| exit());
	let netsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"NET") }.unwrap_or_else(|| exit());
	let timesvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"TIME") }.unwrap_or_else(|| exit());
	let audiosvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"AUDIO") }.unwrap_or_else(|| exit());
	// The InputService client: `mouse` subscribes to its pointer-event stream and prints
	// the recent text-cell positions (the plumbing echo - no mouse-driven UI yet).
	let inputsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"INPUT") }.unwrap_or_else(|| exit());
	// The SystemGraphService client: `graph` queries the live system graph (components,
	// devices, dependency edges, counters, and trace spans) and renders it as CLI / JSON
	// / CBOR. Sent right after INPUT, matching ServiceManager's send order.
	let graphsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"GRAPH") }.unwrap_or_else(|| exit());
	// The console channel to ConsoleService: the shell writes its output to it (routed
	// via stdout) and reads its keystrokes from it. The userspace terminal renders the
	// output and forwards the input, so the shell talks to the console, not the kernel.
	let console: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"CONSOLE") }.unwrap_or_else(|| exit());
	set_stdout(console);
	// The per-VT control channel to ConsoleService: the shell announces its foreground
	// job on it (SET_FG / CLEAR_FG) so the tty signals it on Ctrl+C / Ctrl+Z / Ctrl+\,
	// and learns of a Ctrl+Z suspend (JOB_STOPPED) so it can background the job.
	let control: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"CONTROL") }.unwrap_or_else(|| exit());
	// The init package lets the shell spawn foreground programs (echo, later the net
	// tools); the archive is mapped 'static and parsed once.
	let (_pkg_handle, archive): (u64, &'static [u8]) = unsafe { recv_package(bootstrap, &mut buf) }.unwrap_or_else(|| exit());
	let package: Package = Package::parse(archive).unwrap_or_else(|| exit());
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
		repl(console, control, storage, logsvc, devsvc, procsvc, cfgsvc, netsvc, timesvc, audiosvc, inputsvc, graphsvc, adminsvc, &package, &mut buf);
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
unsafe fn repl(console: u64, control: u64, storage: u64, logsvc: u64, devsvc: u64, procsvc: u64, cfgsvc: u64, netsvc: u64, timesvc: u64, audiosvc: u64, inputsvc: u64, graphsvc: u64, adminsvc: u64, package: &Package, buf: &mut [u8]) {
	unsafe {
		let mut jobs: Jobs = Jobs::new(control);
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
			if dispatch(line, storage, logsvc, devsvc, procsvc, cfgsvc, netsvc, timesvc, audiosvc, inputsvc, graphsvc, adminsvc, package, &mut jobs) {
				return;
			}
			reap_jobs(&mut jobs);
			print(b"\x1b[1;32m> \x1b[0m");
		}
	}
}

// Dispatch one command line. Returns true if the shell should exit.
// A background or suspended job the shell tracks: the child Process handle (to
// signal it), the channel the shell learns of its completion on, a display name, a
// small id, and whether it is currently stopped (suspended by Ctrl+Z).
struct Job {
	id: usize,
	proc: u64,
	done: u64,
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
// BOTH its completion channel and the control channel. ConsoleService's line discipline
// interprets the signal keys itself (the tty's ISIG behaviour, relocated there) and, on
// a suspend, sends JOB_STOPPED back here. Returns Some(job) when it was suspended (the
// caller backgrounds it), or None when it finished or was interrupted (its handles are
// closed here). CLEAR_FG releases the tty's hold on the job before returning to the
// prompt.
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
		let waits: [u64; 2] = [job.done, control];
		let mut cbuf: [u8; 32] = [0u8; 32];
		loop {
			let ready: i64 = wait_any(&waits, 0);
			if ready == 0 {
				// The job's channel is ready: it sent its completion message or its end
				// closed (a signal terminated it). Either way the job is done; release the
				// tty and reap it.
				send_blocking(control, b"CLEAR_FG", 0);
				close(job.done);
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
					close(job.done);
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

// Poll each running (not stopped) job: once its completion channel is no longer empty
// it has finished, so announce it and drop it. Called before each prompt, the way a
// shell reports a background job's completion.
unsafe fn reap_jobs(jobs: &mut Jobs) {
	unsafe {
		let mut buf: [u8; 16] = [0u8; 16];
		let mut i: usize = 0;
		while i < jobs.list.len() {
			if jobs.list[i].stopped {
				i += 1;
				continue;
			}
			match try_recv(jobs.list[i].done, &mut buf) {
				Polled::Empty => i += 1,
				_ => {
					let job = jobs.list.remove(i);
					print(b"[");
					print_usize(job.id);
					print(b"] done   ");
					print(&job.name);
					print(b"\n");
					close(job.done);
					close(job.proc);
				}
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

unsafe fn dispatch(line: &[u8], storage: u64, logsvc: u64, devsvc: u64, procsvc: u64, cfgsvc: u64, netsvc: u64, timesvc: u64, audiosvc: u64, inputsvc: u64, graphsvc: u64, adminsvc: u64, package: &Package, jobs: &mut Jobs) -> bool {
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
			print(b"  ls [vol://vol]   list volumes, or a volume's files via StorageService\n");
			print(b"  beep [hz] [ms]   play a tone via AudioService\n");
			print(b"  mouse            show recent pointer events via InputService\n");
			print(b"  script [<cmd>]   run a command in a fresh pty-hosted shell and record it\n");
			print(b"  log [json]       show the system journal via LogService\n");
			print(b"  log tail [json]  stream the journal via LogService (sub-channel)\n");
			print(b"  dev [json]       list devices via DeviceService\n");
			print(b"  graph [json|cbor]  show the live system graph and counters via SystemGraphService\n");
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
		if let Some(rest) = line.strip_prefix(b"stop ") {
			stop_service(adminsvc, trim(rest));
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
		if line == b"beep" {
			beep_cmd(audiosvc, b"");
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"beep ") {
			beep_cmd(audiosvc, trim(rest));
			return false;
		}
		if line == b"mouse" {
			mouse_cmd(inputsvc);
			return false;
		}
		if line == b"ip" || line == b"net" {
			spawn_net_tool(jobs, netsvc, package, b"ip", b"", bg);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"ping ") {
			spawn_net_tool(jobs, netsvc, package, b"ping", trim(rest), bg);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"nslookup ") {
			spawn_net_tool(jobs, netsvc, package, b"nslookup", trim(rest), bg);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"host ") {
			spawn_net_tool(jobs, netsvc, package, b"nslookup", trim(rest), bg);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"tcp ") {
			spawn_net_tool(jobs, netsvc, package, b"tcp", trim(rest), bg);
			return false;
		}
		if line == b"arp" {
			spawn_net_tool(jobs, netsvc, package, b"arp", b"", bg);
			return false;
		}
		if line == b"ss" || line == b"netstat" {
			spawn_net_tool(jobs, netsvc, package, b"ss", b"", bg);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"nc ") {
			spawn_net_tool(jobs, netsvc, package, b"nc", trim(rest), bg);
			return false;
		}
		if line == b"httpd" {
			spawn_net_tool(jobs, netsvc, package, b"httpd", b"", true);
			return false;
		}
		if line == b"echo" {
			exec(jobs, package, b"echo", b"", 0, bg);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"echo ") {
			exec(jobs, package, b"echo", trim(rest), 0, bg);
			return false;
		}
		if line == b"ls" {
			ls_cmd(storage, b"");
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"ls ") {
			ls_cmd(storage, trim(rest));
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
		if line == b"script" {
			run_script(jobs, package, b"");
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"script ") {
			run_script(jobs, package, trim(rest));
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
unsafe fn exec(jobs: &mut Jobs, package: &Package, name: &[u8], args: &[u8], cap: u64, bg: bool) {
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
		// `spawn` moves the child end into the new process as its bootstrap handle and
		// returns the child Process handle (which carries MANAGE, so the shell can signal
		// it for job control).
		let proc: i64 = spawn(elf, child);
		if proc < 0 {
			print(name);
			print(b": could not start\n");
			close(parent);
			return;
		}
		// Hand the child our console as its stdout (a SEND dup of our console channel), then
		// its arguments + an optional inherited capability (e.g. a NetworkService client).
		send_stdout(parent);
		send_blocking(parent, args, cap);
		let id: usize = jobs.next_id;
		jobs.next_id += 1;
		let job: Job = Job { id, proc: proc as u64, done: parent, name: name.to_vec(), stopped: false };
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
unsafe fn spawn_net_tool(jobs: &mut Jobs, netsvc: u64, package: &Package, name: &[u8], args: &[u8], bg: bool) {
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
		exec(jobs, package, name, args, tool_netsvc, bg);
	}
}

// Record a session: ask the console (over the tty control channel) to host a shell on a
// fresh pseudo-terminal, then hand the master end to the `script` tool, which drives the
// pty's shell with `cmd` and prints the captured session. This is the foreground side of
// the PTY abstraction - a program (script) hosting a terminal it is not the hardware
// console for (the same path a future ssh drives).
unsafe fn run_script(jobs: &mut Jobs, package: &Package, cmd: &[u8]) {
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
		exec(jobs, package, b"script", cmd, master, false);
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

// List the volume set via the StorageService `list` op. With no argument print the
// volume set; with `vol://<volume>` print that volume's files (name + size). The single
// phase-1 volume is `system`.
unsafe fn ls_cmd(storage: u64, arg: &[u8]) {
	unsafe {
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let files = match client.list() {
			Some(Ok(f)) => f,
			_ => {
				print(b"ls: StorageService unavailable\n");
				return;
			}
		};
		if arg.is_empty() {
			print(b"volumes (1):\n  vol://system (");
			print_usize(files.len());
			print(b" files)\n");
			return;
		}
		let vol: &[u8] = arg.strip_prefix(b"vol://").unwrap_or(arg);
		let vol: &[u8] = vol.strip_suffix(b"/").unwrap_or(vol);
		if vol != b"system" {
			print(b"ls: unknown volume\n");
			return;
		}
		print(b"vol://system (");
		print_usize(files.len());
		print(b" files):\n");
		for f in &files {
			print(b"  ");
			print(f.name.as_bytes());
			print(b" ");
			print_usize(f.size as usize);
			print(b" bytes\n");
		}
	}
}
