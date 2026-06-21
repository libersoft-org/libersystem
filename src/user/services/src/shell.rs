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
use proto::system::{config, device, log, network, process, socket, volume, ConfigEntry, DeviceEntry, Endpoint, Entry, Error, Ipv4Addr, OpenOpts, PingStatus, ProcessInfo, Query};
use rt::*;

// the file the shell reads at startup to prove the StorageService round-trip works
const SELF_CHECK_URI: &[u8] = b"vol://system/hello.txt";

// maximum length of a typed command line
const LINE_MAX: usize = 128;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. receive the per-service client channels from ServiceManager, in the order it
	//    sends them: storage (`cat`), log (`log`), device (`dev`), process (`ps`/`run`),
	//    config (`config`/`set`). Each is a tagged capability over the bootstrap channel.
	let storage: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"STORAGE") }.unwrap_or_else(|| exit());
	let logsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"LOG") }.unwrap_or_else(|| exit());
	let devsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"DEVICE") }.unwrap_or_else(|| exit());
	let procsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"PROCESS") }.unwrap_or_else(|| exit());
	let cfgsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"CONFIG") }.unwrap_or_else(|| exit());
	let netsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"NET") }.unwrap_or_else(|| exit());

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
		repl(storage, logsvc, devsvc, procsvc, cfgsvc, netsvc, &mut buf);
	}
	exit();
}

// Register a console channel with the kernel and run the read-eval-print loop. The
// kernel feeds keystrokes on the channel; we line-edit them (echoing input,
// handling backspace) and dispatch each completed line. Returns when the user
// types `exit`.
unsafe fn repl(storage: u64, logsvc: u64, devsvc: u64, procsvc: u64, cfgsvc: u64, netsvc: u64, buf: &mut [u8]) {
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
						if dispatch(&line[..len], storage, logsvc, devsvc, procsvc, cfgsvc, netsvc) {
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
unsafe fn dispatch(line: &[u8], storage: u64, logsvc: u64, devsvc: u64, procsvc: u64, cfgsvc: u64, netsvc: u64) -> bool {
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
		if line == b"log tail" {
			tail_log(logsvc, false);
			return false;
		}
		if line == b"log tail json" {
			tail_log(logsvc, true);
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
		if line == b"ip" || line == b"net" {
			query_ip(netsvc);
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"ping ") {
			ping_host(netsvc, trim(rest));
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"nslookup ") {
			dns_lookup(netsvc, trim(rest));
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"host ") {
			dns_lookup(netsvc, trim(rest));
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"tcp ") {
			tcp_connect(netsvc, trim(rest));
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

// Query NetworkService for its interface state (`ip` / `net`) over the typed
// `network` interface and render it: our address, MAC, gateway, and the neighbor
// cache (each a typed object, not a packed byte blob).
unsafe fn query_ip(netsvc: u64) {
	unsafe {
		if netsvc == 0 {
			print(b"ip: no network interface\n");
			return;
		}
		let mut client = network::Client::new(ChannelTransport { chan: netsvc });
		match client.info() {
			Some(Ok(info)) => {
				print(b"net0: ");
				print_ip(&[info.addr.a, info.addr.b, info.addr.c, info.addr.d]);
				print(b"  mac ");
				print_mac(&info.mac);
				print(b"  gateway ");
				print_ip(&[info.gateway.a, info.gateway.b, info.gateway.c, info.gateway.d]);
				print(b"\n");
				if !info.neighbors.is_empty() {
					print(b"neighbors:\n");
					for n in &info.neighbors {
						print(b"  ");
						print_ip(&[n.addr.a, n.addr.b, n.addr.c, n.addr.d]);
						print(b"  ");
						print_mac(&n.mac);
						print(b"\n");
					}
				}
			}
			Some(Err(_)) => print(b"ip: network error\n"),
			None => print(b"ip: service unavailable\n"),
		}
	}
}

// Send a `ping` to `target` (a dotted-decimal address) through NetworkService over
// the typed `network` interface and render the outcome.
unsafe fn ping_host(netsvc: u64, target: &[u8]) {
	unsafe {
		if netsvc == 0 {
			print(b"ping: no network interface\n");
			return;
		}
		let ip: [u8; 4] = match parse_ip(target) {
			Some(a) => a,
			None => {
				print(b"ping: invalid address\n");
				return;
			}
		};
		let mut client = network::Client::new(ChannelTransport { chan: netsvc });
		let addr: Ipv4Addr = Ipv4Addr { a: ip[0], b: ip[1], c: ip[2], d: ip[3] };
		print(b"ping ");
		print(target);
		match client.ping(&addr) {
			Some(Ok(PingStatus::Reply)) => print(b": reply\n"),
			Some(Ok(PingStatus::Unreachable)) => print(b": unreachable (no route)\n"),
			Some(Ok(PingStatus::Timeout)) => print(b": no reply (timeout)\n"),
			Some(Err(_)) => print(b": error\n"),
			None => print(b": service unavailable\n"),
		}
	}
}

// Resolve `name` through NetworkService's DNS client (`nslookup` / `host`) over the
// typed `network` interface and render the resolved address or a not-found message.
unsafe fn dns_lookup(netsvc: u64, name: &[u8]) {
	unsafe {
		if netsvc == 0 {
			print(b"nslookup: no network interface\n");
			return;
		}
		if name.is_empty() || name.len() > 120 {
			print(b"nslookup: invalid name\n");
			return;
		}
		let name_str: &str = match core::str::from_utf8(name) {
			Ok(s) => s,
			Err(_) => {
				print(b"nslookup: invalid name\n");
				return;
			}
		};
		let mut client = network::Client::new(ChannelTransport { chan: netsvc });
		match client.resolve(name_str) {
			Some(Ok(addr)) => {
				print(name);
				print(b" has address ");
				print_ip(&[addr.a, addr.b, addr.c, addr.d]);
				print(b"\n");
			}
			Some(Err(_)) => {
				print(b"nslookup: could not resolve ");
				print(name);
				print(b"\n");
			}
			None => print(b"nslookup: network service gone\n"),
		}
	}
}

// Open a TCP connection to `<ip> <port>` through NetworkService, which hands back a
// socket as a capability (the channel a `socket` is served on); drive it with the
// typed `socket` interface - send a minimal HTTP/1.0 GET probe, drain the response
// with repeated `recv` until end of stream, then close.
unsafe fn tcp_connect(netsvc: u64, args: &[u8]) {
	unsafe {
		if netsvc == 0 {
			print(b"tcp: no network interface\n");
			return;
		}
		let sp: usize = match args.iter().position(|&b: &u8| b == b' ') {
			Some(i) => i,
			None => {
				print(b"tcp: usage: tcp <ip> <port>\n");
				return;
			}
		};
		let host: &[u8] = trim(&args[..sp]);
		let ip: [u8; 4] = match parse_ip(host) {
			Some(a) => a,
			None => {
				print(b"tcp: invalid address\n");
				return;
			}
		};
		let port: u16 = match parse_port(trim(&args[sp + 1..])) {
			Some(p) => p,
			None => {
				print(b"tcp: invalid port\n");
				return;
			}
		};
		// connect() returns the socket as a capability (the channel it is served on).
		let mut net = network::Client::new(ChannelTransport { chan: netsvc });
		let ep: Endpoint = Endpoint { addr: Ipv4Addr { a: ip[0], b: ip[1], c: ip[2], d: ip[3] }, port };
		let sockh: u64 = match net.connect(&ep) {
			Some(Ok(h)) => h,
			Some(Err(Error::NotFound)) => {
				print(b"tcp: unreachable (no route)\n");
				return;
			}
			Some(Err(Error::Denied)) => {
				print(b"tcp: connection refused\n");
				return;
			}
			Some(Err(_)) => {
				print(b"tcp: connection timed out\n");
				return;
			}
			None => {
				print(b"tcp: service unavailable\n");
				return;
			}
		};
		let mut sock = socket::Client::new(ChannelTransport { chan: sockh });
		print(b"tcp ");
		print(host);
		print(b": connected\n");
		// Send the probe, then drain the response a chunk at a time until EOF.
		if let Some(Ok(_)) = sock.send(&b"GET / HTTP/1.0\r\n\r\n".to_vec()) {
			loop {
				match sock.recv() {
					Some(Ok(data)) if !data.is_empty() => print(&data),
					_ => break,
				}
			}
			print(b"\n");
		} else {
			print(b"tcp: send failed\n");
		}
		let _ = sock.close();
		close(sockh);
	}
}

// Parse a decimal port number (0-65535), or None if malformed or out of range.
fn parse_port(s: &[u8]) -> Option<u16> {
	if s.is_empty() || s.len() > 5 {
		return None;
	}
	let mut v: u32 = 0;
	for &b in s {
		if !b.is_ascii_digit() {
			return None;
		}
		v = v * 10 + (b - b'0') as u32;
		if v > 65535 {
			return None;
		}
	}
	Some(v as u16)
}

// Parse a dotted-decimal IPv4 address into 4 octets, or None if malformed.
fn parse_ip(s: &[u8]) -> Option<[u8; 4]> {
	let mut octets: [u8; 4] = [0u8; 4];
	let mut idx: usize = 0;
	let mut value: u32 = 0;
	let mut digits: u32 = 0;
	for &b in s {
		if b == b'.' {
			if digits == 0 || idx >= 3 {
				return None;
			}
			octets[idx] = value as u8;
			idx += 1;
			value = 0;
			digits = 0;
		} else if b.is_ascii_digit() {
			value = value * 10 + (b - b'0') as u32;
			digits += 1;
			if value > 255 || digits > 3 {
				return None;
			}
		} else {
			return None;
		}
	}
	if idx != 3 || digits == 0 {
		return None;
	}
	octets[3] = value as u8;
	Some(octets)
}

// Print an IPv4 address (4 octets) in dotted-decimal form.
unsafe fn print_ip(ip: &[u8]) {
	unsafe {
		for (i, octet) in ip.iter().enumerate() {
			if i != 0 {
				print(b".");
			}
			print_dec(*octet);
		}
	}
}

// Print a MAC address (6 bytes) as colon-separated hex.
unsafe fn print_mac(mac: &[u8]) {
	unsafe {
		for (i, b) in mac.iter().enumerate() {
			if i != 0 {
				print(b":");
			}
			let hex: &[u8; 16] = b"0123456789abcdef";
			print(&[hex[(*b >> 4) as usize], hex[(*b & 0xf) as usize]]);
		}
	}
}

// Print a byte in decimal (0-255), no leading zeros.
unsafe fn print_dec(n: u8) {
	unsafe {
		if n >= 100 {
			print(&[b'0' + n / 100]);
		}
		if n >= 10 {
			print(&[b'0' + (n / 10) % 10]);
		}
		print(&[b'0' + n % 10]);
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
unsafe fn query_log(logsvc: u64, json: bool) {
	unsafe {
		let q = Query { since: None, min_severity: None, source: None, limit: 0 };
		let mut client = log::Client::new(ChannelTransport { chan: logsvc });
		match client.query(&q) {
			Some(Ok(entries)) => {
				if json {
					print_json_array(&entries, |e: &Entry| -> String { e.to_json() });
				} else {
					print_text_lines(&entries, |e: &Entry| -> String { e.to_text() });
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
unsafe fn tail_log(logsvc: u64, json: bool) {
	unsafe {
		let q = Query { since: None, min_severity: None, source: None, limit: 0 };
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
							print(entry.to_text().as_bytes());
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
