// log - print the system journal, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it two
// capabilities - a LogService client (to query the journal) and a TimeService client (to
// resolve the boot epoch, so each record's monotonic tick renders as wall-clock time) - and
// forwards it the shell's stdout console and the argument string (the sub-form: "", "json",
// "tail", or "tail json"). log queries or streams the journal through its grants and prints
// each entry to the inherited stdout, then exits. A standalone command, not a shell built-in:
// it reaches the services only through the capabilities the permission store granted it, and
// renders on the same terminal as the shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::{log, time, Entry, Query, Timestamp};
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the sub-form ("", "json", "tail", "tail json").
		let args: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		// 3. receive the two capabilities the manifest grants, in vocabulary order: log then time.
		let logsvc: u64 = recv_tagged(bootstrap, &mut buf, b"LOG").unwrap_or_else(|| exit());
		let timesvc: u64 = recv_tagged(bootstrap, &mut buf, b"TIME").unwrap_or_else(|| exit());
		let (tail, json): (bool, bool) = match &args[..] {
			b"json" => (false, true),
			b"tail" => (true, false),
			b"tail json" => (true, true),
			_ => (false, false),
		};
		if tail {
			tail_log(logsvc, timesvc, json);
		} else {
			query_log(logsvc, timesvc, json);
		}
	}
	exit();
}

// Query LogService for the whole journal over the granted log client and print it, rendering
// each entry to text (prefixed with its wall-clock time) or JSON on the client side. The
// query asks for all severities and no limit.
unsafe fn query_log(logsvc: u64, timesvc: u64, json: bool) {
	unsafe {
		let q = Query { since: None, min_severity: None, source: None, limit: 0 };
		let epoch: Option<u64> = boot_epoch(timesvc);
		let mut client = log::Client::new(ChannelTransport { chan: logsvc });
		match client.query(&q) {
			Some(Ok(entries)) => {
				if json {
					print(b"[");
					let mut first: bool = true;
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
						print(entry_text(e, epoch).as_bytes());
						print(b"\n");
					}
				}
			}
			Some(Err(_)) => print(b"log: query error\n"),
			None => print(b"log: service unavailable\n"),
		}
	}
}

// Stream the system journal via LogService's tail op: it returns a fresh sub-channel, frames
// each entry as its own message on it, and closes it to mark the end of the stream. We drain
// the frames and render each entry on the client side, one streamed record at a time.
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

// Resolve the boot epoch (Unix seconds at tick 0) via TimeService: now minus the monotonic
// clock, so a record's tick can be rendered as wall-clock time. None if time is unavailable.
unsafe fn boot_epoch(timesvc: u64) -> Option<u64> {
	unsafe {
		let mut client = time::Client::new(ChannelTransport { chan: timesvc });
		match client.now() {
			Some(Ok(ts)) => Some(ts.unix_secs.saturating_sub(clock() / 100)),
			_ => None,
		}
	}
}

// Render one log entry as text, prefixed with its wall-clock time when the boot epoch is
// known (the record's monotonic tick converted to UTC), else the bare record.
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
