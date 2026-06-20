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

use proto::system::{ConfigEntry, OpenOpts, Query, config, device, log, process, volume};
use rt::*;

// the file the shell reads at startup to prove the StorageService round-trip works
const SELF_CHECK_URI: &[u8] = b"vol://system/hello.txt";

// maximum length of a typed command line
const LINE_MAX: usize = 128;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. receive the StorageService client channel from ServiceManager.
	let storage: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 && &buf[..7] == b"STORAGE" => handle,
		_ => exit(),
	};

	// 1b. receive the LogService client channel the `log` command queries.
	let logsvc: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 3 && &buf[..3] == b"LOG" => handle,
		_ => exit(),
	};

	// 1c. receive the DeviceService client channel the `dev` command queries.
	let devsvc: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 6 && &buf[..6] == b"DEVICE" => handle,
		_ => exit(),
	};

	// 1d. receive the ProcessService client channel the `ps`/`run` commands use.
	let procsvc: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 && &buf[..7] == b"PROCESS" => handle,
		_ => exit(),
	};

	// 1e. receive the ConfigService client channel the `config`/`set` commands use.
	let cfgsvc: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 6 && &buf[..6] == b"CONFIG" => handle,
		_ => exit(),
	};

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
		repl(storage, logsvc, devsvc, procsvc, cfgsvc, &mut buf);
	}
	exit();
}

// Register a console channel with the kernel and run the read-eval-print loop. The
// kernel feeds keystrokes on the channel; we line-edit them (echoing input,
// handling backspace) and dispatch each completed line. Returns when the user
// types `exit`.
unsafe fn repl(storage: u64, logsvc: u64, devsvc: u64, procsvc: u64, cfgsvc: u64, buf: &mut [u8]) {
	unsafe {
		// The kernel sends console input on `feed`; we receive it on `input`.
		let (feed, input): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return,
		};
		if sys_is_err(syscall(SYS_CONSOLE_ATTACH, feed, 0, 0, 0)) {
			return;
		}
		let mut line: [u8; LINE_MAX] = [0u8; LINE_MAX];
		let mut len: usize = 0;
		loop {
			let n: usize = match recv_blocking(input, buf) {
				Received::Message { len, .. } => len,
				Received::Closed => return,
			};
			for i in 0..n {
				match buf[i] {
					b'\n' | b'\r' => {
						print(b"\n");
						if dispatch(&line[..len], storage, logsvc, devsvc, procsvc, cfgsvc) {
							return;
						}
						len = 0;
						print(b"> ");
					}
					0x08 | 0x7f => {
						if len > 0 {
							len -= 1;
							// erase the character on the terminal: back up, overwrite, back up
							print(b"\x08 \x08");
						}
					}
					byte @ 0x20..=0x7e => {
						if len < LINE_MAX {
							line[len] = byte;
							len += 1;
							print(&[byte]);
						}
					}
					_ => {}
				}
			}
		}
	}
}

// Dispatch one command line. Returns true if the shell should exit.
unsafe fn dispatch(line: &[u8], storage: u64, logsvc: u64, devsvc: u64, procsvc: u64, cfgsvc: u64) -> bool {
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
			print(b"  dev [json]       list devices via DeviceService\n");
			print(b"  ps               list started processes via ProcessService\n");
			print(b"  run <name>       start a program via ProcessService\n");
			print(b"  config [<key>]   list the config tree or read one key via ConfigService\n");
			print(b"  set <key> <val>  write a config key via ConfigService\n");
			print(b"  exit             stop the shell and halt\n");
			return false;
		}
		if line == b"log" {
			query_log(logsvc, false);
			return false;
		}
		if line == b"log json" {
			query_log(logsvc, true);
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
		if let Some(rest) = line.strip_prefix(b"echo ") {
			print(rest);
			print(b"\n");
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
		print(b"unknown command: ");
		print(line);
		print(b" (try 'help')\n");
		false
	}
}

// A proto Transport over an rt channel: send the request, then block for the
// reply. The generated Log client calls through this to reach LogService.
struct ChannelTransport {
	chan: u64,
}

impl proto::codec::Transport for ChannelTransport {
	fn call(&mut self, request: &[u8], request_handle: u64) -> Option<(alloc::vec::Vec<u8>, u64)> {
		unsafe {
			if !send_blocking(self.chan, request, request_handle) {
				return None;
			}
			let mut reply: [u8; 4096] = [0u8; 4096];
			match recv_blocking(self.chan, &mut reply) {
				Received::Message { len, handle } => Some((reply[..len].to_vec(), handle)),
				Received::Closed => None,
			}
		}
	}
}

// Query LogService for the journal over the generated Log client and print it,
// rendering each returned entry to text or JSON on the client side. The query
// asks for all severities (no minimum) and no limit.
unsafe fn query_log(logsvc: u64, json: bool) {
	unsafe {
		let q = Query { since: None, min_severity: None, source: None, limit: 0 };
		let mut client = log::Client::new(ChannelTransport { chan: logsvc });
		match client.query(&q) {
			Some(Ok(entries)) => {
				if json {
					print(b"[");
					let mut first = true;
					for entry in &entries {
						if !first {
							print(b",");
						}
						first = false;
						print(entry.to_json().as_bytes());
					}
					print(b"]\n");
				} else {
					for entry in &entries {
						print(entry.to_text().as_bytes());
						print(b"\n");
					}
				}
			}
			Some(Err(_)) => print(b"log: query error\n"),
			None => print(b"log: service unavailable\n"),
		}
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
					print(b"[");
					let mut first = true;
					for e in &entries {
						if !first {
							print(b",");
						}
						first = false;
						print(e.to_json().as_bytes());
					}
					print(b"]\n");
				} else {
					for e in &entries {
						print(e.to_text().as_bytes());
						print(b"\n");
					}
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
			Some(Ok(procs)) => {
				for p in &procs {
					print(p.to_text().as_bytes());
					print(b"\n");
				}
			}
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
			Some(Ok(entries)) => {
				for e in &entries {
					print(e.to_text().as_bytes());
					print(b"\n");
				}
			}
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
		let mapped: u64 = syscall(SYS_MEMORY_MAP, result.file, 0, 0, 0);
		if sys_is_err(mapped) {
			return false;
		}
		let contents: &[u8] = core::slice::from_raw_parts(mapped as *const u8, result.size as usize);
		print(contents);
		if contents.last() != Some(&b'\n') {
			print(b"\n");
		}
		syscall(SYS_MEMORY_UNMAP, result.file, 0, 0, 0);
		syscall(SYS_HANDLE_CLOSE, result.file, 0, 0, 0);
		true
	}
}
