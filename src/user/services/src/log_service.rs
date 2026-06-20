// LogService - the userspace structured-logging service.
//
// ServiceManager starts this program from the init package and hands it a
// bootstrap channel. LogService reports in, then waits for a "SERVE" message
// carrying the channel its clients reach it on. Over that channel clients speak
// the generated `liber:system` Log bindings (the proto crate): they EMIT
// canonical `Entry` records - the journald model: structured data, not lines of
// text - into a bounded in-memory journal, and QUERY them back. The query returns
// structured entries; rendering to text or JSON happens on the client (the
// shell), so the same canonical records render many ways from one form.
//
// When the supervisor that started it drops the bootstrap channel, the service
// exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::log::{self, Service};
use proto::system::{Entry, Error, Query, Severity};
use rt::*;

// The bounded journal: at most this many records, newest dropping oldest. With a
// heap available a real eviction policy / persistence is a later phase.
const JOURNAL_CAP: usize = 32;

// The in-memory journal: a bounded list of canonical log entries.
struct Journal {
	entries: Vec<Entry>,
}

impl Journal {
	fn new() -> Journal {
		Journal { entries: Vec::new() }
	}
}

// The generated Log service contract: store an emitted entry, answer a query with
// the matching entries (filtered by minimum severity and capped by `limit`).
impl Service for Journal {
	fn emit(&mut self, entry: Entry) -> Result<(), Error> {
		self.entries.push(entry);
		if self.entries.len() > JOURNAL_CAP {
			self.entries.remove(0);
		}
		Ok(())
	}

	fn query(&mut self, q: Query) -> Result<Vec<Entry>, Error> {
		let min: u8 = q.min_severity.map(|s| s as u8).unwrap_or(0);
		let mut out: Vec<Entry> = Vec::new();
		for entry in &self.entries {
			if (entry.severity as u8) < min {
				continue;
			}
			out.push(entry.clone());
			if q.limit != 0 && out.len() as u32 >= q.limit {
				break;
			}
		}
		Ok(out)
	}

	// The streaming counterpart of `query`: the same bounded source, but the
	// generated codec frames each entry onto a fresh sub-channel instead of
	// packing them into one reply. Here the source is bounded (the journal), so
	// we return the snapshot and the serve loop streams it frame by frame.
	fn tail(&mut self, q: Query) -> Vec<Entry> {
		let min: u8 = q.min_severity.map(|s: Severity| s as u8).unwrap_or(0);
		let mut out: Vec<Entry> = Vec::new();
		for entry in &self.entries {
			if (entry.severity as u8) < min {
				continue;
			}
			out.push(entry.clone());
			if q.limit != 0 && out.len() as u32 >= q.limit {
				break;
			}
		}
		out
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. report in to the supervisor that started us.
	unsafe {
		send_blocking(bootstrap, b"LogService: online", 0);
	}

	// 2. wait for the serve channel clients reach us on. If the supervisor drops
	//    the bootstrap channel first (no clients this boot), we are done.
	let service: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 5 && &buf[..5] == b"SERVE" => handle,
		_ => exit(),
	};

	// 3. serve generated emit/query requests until the client side closes. Each
	//    request is dispatched into the journal; the reply it produces is sent back
	//    (emit replies `result<unit, error>`, query replies the matching entries).
	//    OP_TAIL is special: it opens a stream. We mint a fresh sub-channel, hand
	//    the consumer end back out-of-band alongside the correlation id, then frame
	//    each entry onto the producer end and close it to mark end-of-stream.
	let mut journal: Journal = Journal::new();
	let mut request: [u8; 1024] = [0u8; 1024];
	let mut reply: [u8; 4096] = [0u8; 4096];
	loop {
		match unsafe { recv_blocking(service, &mut request) } {
			// An empty message is the explicit quit sentinel.
			Received::Message { len, .. } if len == 0 => break,
			Received::Message { len, handle } => {
				let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
				if op == log::OP_TAIL {
					stream_tail(&mut journal, service, &request[..len]);
				} else {
					let mut reply_handle: u64 = 0;
					if let Some(n) = log::dispatch(&mut journal, &request[..len], handle, &mut reply, &mut reply_handle) {
						unsafe {
							send_blocking(service, &reply[..n], reply_handle);
						}
					}
				}
			}
			Received::Closed => break,
		}
	}
	exit();
}

// Serve one OP_TAIL request: decode it, gather the bounded snapshot, then stream
// the entries to the client over a fresh sub-channel. The reply on the service
// channel carries just the correlation id and the consumer endpoint (out-of-band);
// each entry then travels as its own framed message on the producer endpoint, and
// closing the producer tells the client the stream has ended.
fn stream_tail(journal: &mut Journal, service: u64, request: &[u8]) {
	let (corr, items): (u32, Vec<Entry>) = match log::tail_open(journal, request) {
		Some(v) => v,
		None => return,
	};
	let (producer, consumer): (u64, u64) = match unsafe { channel() } {
		Some(pair) => pair,
		None => return,
	};
	let corr_bytes: [u8; 4] = corr.to_le_bytes();
	unsafe {
		send_blocking(service, &corr_bytes, consumer);
	}
	let mut frame: [u8; 1024] = [0u8; 1024];
	for (seq, item) in items.iter().enumerate() {
		if let Some(n) = log::tail_frame(seq as u32, item, &mut frame) {
			unsafe {
				send_blocking(producer, &frame[..n], 0);
			}
		}
	}
	unsafe {
		syscall(SYS_HANDLE_CLOSE, producer, 0, 0, 0);
	}
}
