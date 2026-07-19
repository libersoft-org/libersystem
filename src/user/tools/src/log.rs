// log - print the system journal, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it two
// capabilities - a LogService client (to query the journal) and a TimeService client (to
// resolve the boot epoch, so each record's monotonic tick renders as wall-clock time) - and
// forwards it the shell's stdout console and the argument string (the sub-form: "", "json",
// "tail", "tail json", or "--boot <n>" / "--boot <n> json" to read a previous boot's
// on-disk journal). log queries or streams the journal through its grants and prints
// each entry to the inherited stdout, then exits. A standalone command, not a shell built-in:
// it reaches the services only through the capabilities the permission store granted it, and
// renders on the same terminal as the shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use log_client::LogClient;
use proto::codec::JsonMode;
use proto::system::{Entry, Query, Timestamp, log};
use rt::*;
use time_client::TimeClient;

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
		// "--boot <n>" selects a previous boot's on-disk journal; the remainder
		// keeps the usual sub-forms.
		let (boot, rest): (Option<u32>, &[u8]) = match args.strip_prefix(b"--boot ") {
			Some(r) => {
				let end: usize = r.iter().position(|&b| b == b' ').unwrap_or(r.len());
				match core::str::from_utf8(&r[..end]).ok().and_then(|n| n.parse::<u32>().ok()) {
					Some(n) => (Some(n), r.get(end + 1..).unwrap_or(b"")),
					None => {
						print(b"log: usage: log --boot <n> [json]\n");
						exit();
					}
				}
			}
			None => (None, &args[..]),
		};
		let (tail, mode): (bool, Option<JsonMode>) = match rest {
			b"json" => (false, Some(JsonMode::Pretty)),
			b"json-min" => (false, Some(JsonMode::Min)),
			b"tail" => (true, None),
			b"tail json" => (true, Some(JsonMode::Pretty)),
			b"tail json-min" => (true, Some(JsonMode::Min)),
			_ => (false, None),
		};
		if tail {
			tail_log(logsvc, timesvc, mode);
		} else {
			query_log(logsvc, timesvc, boot, mode);
		}
	}
	exit();
}

// Query LogService for the newest journal records over the granted log client and print
// them, rendering each entry to text (prefixed with its wall-clock time) or JSON on the
// client side. The query asks for all severities, limited to the newest records that fit
// one typed reply - the journal itself is much deeper; `log tail` streams all of it. A
// boot selector reads a previous boot's on-disk journal instead of the live one (its
// ticks belong to that boot, so they render raw rather than against this boot's epoch).
unsafe fn query_log(logsvc: u64, timesvc: u64, boot: Option<u32>, mode: Option<JsonMode>) {
	unsafe {
		let q = Query { since: None, min_severity: None, source: None, boot, limit: 32 };
		let epoch: Option<u64> = if boot.is_none() { boot_epoch(timesvc) } else { None };
		let mut client = LogClient::new(logsvc);
		match client.query(&q) {
			Some(Ok(entries)) => {
				if let Some(mode) = mode {
					let mut out = String::from("[");
					for (i, e) in entries.iter().enumerate() {
						if i > 0 {
							out.push(',');
						}
						out.push_str(&e.to_json());
					}
					out.push(']');
					print(mode.render(out).as_bytes());
					print(b"\n");
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
// the frames and render each entry on the client side, one streamed record at a time. A
// live stream cannot buffer into one closing array, so the JSON forms render per record:
// `tail json` pretty-prints each entry as its own document, `tail json-min` prints each
// entry minified on its own line (a JSON-lines stream).
unsafe fn tail_log(logsvc: u64, timesvc: u64, mode: Option<JsonMode>) {
	unsafe {
		let q = Query { since: None, min_severity: None, source: None, boot: None, limit: 0 };
		let epoch: Option<u64> = boot_epoch(timesvc);
		let mut client = LogClient::new(logsvc);
		let consumer: u64 = match client.tail(&q) {
			Some(h) => h,
			None => {
				print(b"log: service unavailable\n");
				return;
			}
		};
		let mut frame: [u8; 1024] = [0u8; 1024];
		loop {
			match recv_blocking(consumer, &mut frame) {
				Received::Message { len, mut handle } => {
					if let Some(entry) = log::tail_read(&frame[..len], &mut handle) {
						if let Some(mode) = mode {
							print(mode.render(entry.to_json()).as_bytes());
							print(b"\n");
						} else {
							print(entry_text(&entry, epoch).as_bytes());
							print(b"\n");
						}
					}
					if handle != 0 {
						close(handle);
					}
				}
				Received::Closed => break,
			}
		}
		close(consumer);
	}
}

// Resolve the boot epoch (Unix seconds at tick 0) via TimeService: now minus the monotonic
// clock, so a record's tick can be rendered as wall-clock time. None if time is unavailable.
unsafe fn boot_epoch(timesvc: u64) -> Option<u64> {
	unsafe {
		let mut client = TimeClient::new(timesvc);
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
