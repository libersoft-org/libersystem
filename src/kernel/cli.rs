// A minimal command shell over the serial port: read a line, dispatch a command,
// print a typed result. The commands cover the phase-0 surface - inspect a
// volume, read a file (which round-trips to the StorageManager service), and dump
// the live System Graph. The shell is kernel-side for the MVP; a userspace CLI
// component is later work.

#![allow(dead_code)]

use alloc::string::String;
#[cfg(not(test))]
use alloc::sync::Arc;
#[cfg(not(test))]
use alloc::vec::Vec;

use crate::graph;
#[cfg(not(test))]
use crate::object::process::Process;

// The single ramdisk volume served in phase 0/1. Both the volume listing and the
// per-volume file listing resolve against this name.
const SYSTEM_VOLUME: &str = "system";

// Run one command line: split into a command word and its argument, then
// dispatch. A blank line does nothing.
pub fn run_line(line: &str) {
	let line = line.trim();
	if line.is_empty() {
		return;
	}
	let (command, rest) = match line.split_once(' ') {
		Some((c, r)) => (c, r.trim()),
		None => (line, ""),
	};
	match command {
		"help" => help(),
		"graph" | "ps" => graph::render(&graph::collect()),
		"ls" => list(rest),
		"cat" => print_file(rest),
		"reboot" => crate::arch::reset(),
		"poweroff" | "shutdown" => crate::arch::poweroff(),
		other => crate::serial_println!("unknown command: {} (try 'help')", other),
	}
}

fn help() {
	crate::serial_println!("commands:");
	crate::serial_println!("  help                  show this help");
	crate::serial_println!("  graph                 dump the live System Graph");
	crate::serial_println!("  ls                    list the available volumes");
	crate::serial_println!("  ls <vol://volume>     list the files on a volume");
	crate::serial_println!("  cat <vol://vol/path>  print a file (via StorageManager)");
	crate::serial_println!("  reboot                reboot the machine");
	crate::serial_println!("  poweroff              power the machine off");
	crate::serial_println!("  exit                  stop the shell and halt");
}

// `ls`: with no argument list the available volumes; with `<vol://volume>` list
// the files on that volume.
fn list(arg: &str) {
	if arg.is_empty() {
		list_volumes();
	} else {
		list_volume(arg);
	}
}

// `ls` with no argument: list the available volumes. Phase 0/1 serves a single
// ramdisk volume, reported here with its file count when a volume is loaded.
fn list_volumes() {
	match crate::volume_package_bytes() {
		Some(bytes) => {
			let files: usize = crate::pkg::Package::parse(bytes).map(|p| p.len()).unwrap_or(0);
			crate::serial_println!("volumes (1):");
			crate::serial_println!("  vol://{} ({} files)", SYSTEM_VOLUME, files);
		}
		None => crate::serial_println!("no volumes are loaded"),
	}
}

// `ls <vol://volume>`: list the files on a volume by reading the ramdisk archive.
fn list_volume(arg: &str) {
	let volume: &str = match parse_volume(arg) {
		Some(v) => v,
		None => {
			crate::serial_println!("usage: ls <vol://volume>");
			return;
		}
	};
	if volume != SYSTEM_VOLUME {
		crate::serial_println!("ls: unknown volume '{}'", volume);
		return;
	}
	let bytes: &[u8] = match crate::volume_package_bytes() {
		Some(b) => b,
		None => {
			crate::serial_println!("ls: no volume is loaded");
			return;
		}
	};
	let package = match crate::pkg::Package::parse(bytes) {
		Some(p) => p,
		None => {
			crate::serial_println!("ls: the volume is malformed");
			return;
		}
	};
	crate::serial_println!("vol://{} ({} files):", SYSTEM_VOLUME, package.len());
	for index in 0..package.len() {
		if let Some(name) = package.name(index) {
			let size: usize = package.lookup(name).map(|b| b.len()).unwrap_or(0);
			crate::serial_println!("  {:<20} {} bytes", core::str::from_utf8(name).unwrap_or("<bad>"), size);
		}
	}
}

// `cat <vol://volume/path>`: read a file through the StorageManager and print it.
fn print_file(arg: &str) {
	let uri: &str = arg.trim();
	if uri.is_empty() {
		crate::serial_println!("usage: cat <vol://volume/path>");
		return;
	}
	match crate::storage_read(uri.as_bytes()) {
		Ok(bytes) => match core::str::from_utf8(&bytes) {
			Ok(text) => {
				crate::serial_print!("{}", text);
				if !text.ends_with('\n') {
					crate::serial_println!();
				}
			}
			Err(_) => crate::serial_println!("cat: {} is {} bytes of binary data", uri, bytes.len()),
		},
		Err(reason) => crate::serial_println!("cat: {}: {}", uri, reason),
	}
}

// Normalise a volume argument: accept either "vol://system" or a bare "system",
// trimming the scheme and any trailing slash. Returns None for an empty argument.
fn parse_volume(arg: &str) -> Option<&str> {
	let arg = arg.trim();
	if arg.is_empty() {
		return None;
	}
	let volume = arg.strip_prefix("vol://").unwrap_or(arg);
	let volume = volume.trim_end_matches('/');
	if volume.is_empty() {
		return None;
	}
	Some(volume)
}

// Read one line from the serial port into `buf`, echoing typed characters so the
// user sees their input. Recognises carriage return / newline as end of line and
// backspace / delete as erase. Control characters are ignored.
fn read_line(buf: &mut String) {
	buf.clear();
	loop {
		let byte: u8 = crate::arch::serial::read_byte_blocking();
		match byte {
			b'\r' | b'\n' => {
				crate::serial_println!();
				return;
			}
			0x08 | 0x7f => {
				if buf.pop().is_some() {
					// erase the character on the terminal: back up, overwrite, back up
					crate::serial_print!("\x08 \x08");
				}
			}
			0x20..=0x7e => {
				buf.push(byte as char);
				crate::serial_print!("{}", byte as char);
			}
			_ => {}
		}
	}
}

// Run the interactive shell: prompt, read a line, run it, repeat. Returns when
// the user types `exit` or `quit`.
pub fn run_interactive() {
	let mut line = String::new();
	loop {
		crate::serial_print!("> ");
		read_line(&mut line);
		let trimmed: &str = line.trim();
		if trimmed == "exit" || trimmed == "quit" {
			crate::serial_println!("shell: exiting");
			return;
		}
		run_line(trimmed);
	}
}

// A scripted shell session for the boot log: run a handful of commands as if
// typed, so the CLI's behaviour is visible on serial and the framebuffer without
// requiring input. Builds a couple of illustrative processes first so the `graph`
// command shows live structure, then drops them.
#[cfg(not(test))]
pub fn demo() {
	crate::serial_println!("cli: serial command shell ready - scripted session follows");
	let samples: Vec<Arc<Process>> = sample_processes();
	for line in ["help", "ls", "ls vol://system", "cat vol://system/hello.txt", "graph"] {
		crate::serial_println!("> {}", line);
		run_line(line);
	}
	drop(samples);
}

// Build a couple of live processes under the root Domain, each holding a few
// handles, so the System Graph has structure to show. They are never scheduled;
// the returned Arcs keep them alive for the duration of the demo.
#[cfg(not(test))]
fn sample_processes() -> Vec<Arc<Process>> {
	use crate::object::address_space::AddressSpace;
	use crate::object::channel::Channel;
	use crate::object::event::Event;
	use crate::object::memory_object::MemoryObject;
	use crate::object::rights::Rights;
	let mut out: Vec<Arc<Process>> = Vec::new();
	// a process holding a channel endpoint and a shared buffer
	let p1 = Process::new(AddressSpace::kernel(), crate::sched::root_domain());
	let (endpoint, _peer) = Channel::create();
	p1.install(endpoint, Rights::ALL, 1);
	if let Some(buffer) = MemoryObject::create(4096) {
		p1.install(buffer, Rights::READ | Rights::MAP, 0);
	}
	out.push(p1);
	// a process holding an event
	let p2 = Process::new(AddressSpace::kernel(), crate::sched::root_domain());
	p2.install(Event::create(), Rights::ALL, 2);
	out.push(p2);
	out
}
