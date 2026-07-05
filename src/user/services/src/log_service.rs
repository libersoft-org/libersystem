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
use proto::system::{Entry, Error, Query, Severity, config};
use rt::*;

// The bounded in-memory journal: at most this many records, newest dropping
// oldest - deep enough to diagnose well past the last minute. The depth is the
// operator's policy (the `log.capacity` config key); this is the default until the
// supervisor delivers a ConfigService client (LogService starts before ConfigService,
// so the client arrives on the control channel once the config tree is up). A later
// `set` applies at the next boot. Persistence (the on-disk journal) is a later
// milestone.
const JOURNAL_CAP: usize = 4096;

// The in-memory journal: a bounded list of canonical log entries.
struct Journal {
	entries: Vec<Entry>,
	cap: usize,
}

impl Journal {
	fn new() -> Journal {
		Journal { entries: Vec::new(), cap: JOURNAL_CAP }
	}

	// Collect the entries matching `q`: at or above its minimum severity (none = all),
	// capped by its limit (0 = no cap). A limited query returns the NEWEST matches -
	// the journal is deep now, and a bounded reply full of the oldest records would
	// be useless for diagnosis. Shared by `query` (one reply) and `tail`
	// (streamed frame by frame) so both filter identically.
	fn filtered(&self, q: &Query) -> Vec<Entry> {
		let min: u8 = q.min_severity.map(|s: Severity| s as u8).unwrap_or(0);
		let mut out: Vec<Entry> = Vec::new();
		for entry in &self.entries {
			if (entry.severity as u8) < min {
				continue;
			}
			out.push(entry.clone());
		}
		if q.limit != 0 && out.len() > q.limit as usize {
			out.drain(..out.len() - q.limit as usize);
		}
		out
	}
}

// The generated Log service contract: store an emitted entry, answer a query with
// the matching entries (filtered by minimum severity and capped by `limit`).
impl Service for Journal {
	fn emit(&mut self, entry: Entry) -> Result<(), Error> {
		self.entries.push(entry);
		if self.entries.len() > self.cap {
			self.entries.remove(0);
		}
		Ok(())
	}

	fn query(&mut self, q: Query) -> Result<Vec<Entry>, Error> {
		Ok(self.filtered(&q))
	}

	// The streaming counterpart of `query`: the same bounded source, but the
	// generated codec frames each entry onto a fresh sub-channel instead of
	// packing them into one reply. Here the source is bounded (the journal), so
	// we return the snapshot and the serve loop streams it frame by frame.
	fn tail(&mut self, q: Query) -> Vec<Entry> {
		self.filtered(&q)
	}
}

impl Journal {
	// Adopt the config tree's journal depth (the `log.capacity` key) over the
	// delivered ConfigService client: read it once, trim if the journal already
	// outgrew it, and close the client. A later `set` applies at the next boot.
	fn adopt_capacity(&mut self, config: u64) {
		let mut client = config::Client::new(ChannelTransport { chan: config });
		if let Some(Ok(value)) = client.get("log.capacity") {
			if let Ok(cap) = value.parse::<usize>() {
				if cap > 0 {
					self.cap = cap;
					if self.entries.len() > self.cap {
						self.entries.drain(..self.entries.len() - self.cap);
					}
				}
			}
		}
		unsafe { close(config) };
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
	let service: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"SERVE") }.unwrap_or_else(|| exit());

	// 3. serve generated emit/query requests until the client side closes. Each
	//    request is dispatched into the journal; the reply it produces is sent back
	//    (emit replies `result<unit, error>`, query replies the matching entries).
	//    OP_TAIL is special: it opens a stream. We mint a fresh sub-channel, hand
	//    the consumer end back out-of-band alongside the correlation id, then frame
	//    each entry onto the producer end and close it to mark end-of-stream.
	//    The bootstrap channel stays in the waitset for the supervisor's late
	//    "CONFIG" delivery: LogService starts before ConfigService, so its config
	//    client can only arrive once the config tree is up.
	let mut journal: Journal = Journal::new();
	let mut request: [u8; 1024] = [0u8; 1024];
	let mut reply: [u8; 4096] = [0u8; 4096];
	unsafe {
		serve_multi_seeded(service, &[bootstrap], &mut request, &mut reply, |chan: u64, req: &[u8], handle: u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> {
			if chan == bootstrap {
				if req == b"CONFIG" && handle != 0 {
					journal.adopt_capacity(handle);
				}
				return None;
			}
			// OP_TAIL opens a stream served out of band (no byte reply); everything else
			// dispatches to a single reply. The stream is minted on the channel the
			// request arrived on, so each client gets its own tail.
			let op: u16 = if req.len() >= 2 { u16::from_le_bytes([req[0], req[1]]) } else { 0 };
			if op == log::OP_TAIL {
				stream_tail(&mut journal, chan, req);
				None
			} else {
				log::dispatch(&mut journal, req, handle, out, reply_handle)
			}
		});
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
		close(producer);
	}
}
