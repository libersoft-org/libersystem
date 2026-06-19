// LogService - the userspace structured-logging service.
//
// ServiceManager starts this program from the init package and hands it a
// bootstrap channel. LogService reports in over that channel, then waits for a
// "SERVE" message carrying the channel its clients reach it on. Over the serve
// channel clients EMIT canonical `LogRecord`s - the journald model: structured
// data, not lines of text - which the service keeps in a bounded in-memory
// journal, and QUERY back, rendered as human text, JSON, or CBOR. The same
// canonical records, three representations.
//
// Until a SERVE channel arrives the service simply stands (the current boot chain
// does not wire one yet); when the supervisor that started it drops the bootstrap
// channel, the service exits.

#![no_std]
#![no_main]

use rt::log::{self, FORMAT_CBOR, FORMAT_JSON, LogRecord, OP_EMIT, OP_QUERY};
use rt::*;

// the bounded journal: at most this many records, each at most this many wire
// bytes. A real eviction policy / unbounded persistence is a later phase.
const RING_CAP: usize = 8;
const SLOT_BYTES: usize = 160;

// the largest rendered query reply we will build
const REPLY_MAX: usize = 2048;

// A bounded ring of canonical `LogRecord` wire bytes, newest overwriting oldest
// once full. Stored verbatim: the canonical form is what a query renders from.
struct Journal {
	slots: [[u8; SLOT_BYTES]; RING_CAP],
	lens: [usize; RING_CAP],
	head: usize,
	count: usize,
}

impl Journal {
	fn new() -> Journal {
		Journal { slots: [[0u8; SLOT_BYTES]; RING_CAP], lens: [0usize; RING_CAP], head: 0, count: 0 }
	}

	// Store one record's wire bytes, dropping anything empty or too large for a slot.
	fn push(&mut self, record: &[u8]) {
		if record.is_empty() || record.len() > SLOT_BYTES {
			return;
		}
		self.slots[self.head][..record.len()].copy_from_slice(record);
		self.lens[self.head] = record.len();
		self.head = (self.head + 1) % RING_CAP;
		if self.count < RING_CAP {
			self.count += 1;
		}
	}

	// The `i`-th stored record (0 = oldest) as its wire bytes.
	fn record(&self, i: usize) -> &[u8] {
		let start: usize = if self.count == RING_CAP { self.head } else { 0 };
		let idx: usize = (start + i) % RING_CAP;
		&self.slots[idx][..self.lens[idx]]
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

	// 3. serve emit/query requests until the client side closes.
	let mut journal: Journal = Journal::new();
	let mut reply: [u8; REPLY_MAX] = [0u8; REPLY_MAX];
	loop {
		match unsafe { recv_blocking(service, &mut buf) } {
			// An empty message is the explicit quit sentinel (a client that keeps the
			// peer to read replies cannot close it to signal end-of-stream).
			Received::Message { len, .. } if len == 0 => break,
			Received::Message { len, .. } => unsafe { serve(service, &mut journal, &buf[..len], &mut reply) },
			Received::Closed => break,
		}
	}
	exit();
}

// Handle one request: store an emitted record, or answer a query with the matching
// records rendered in the requested representation.
unsafe fn serve(service: u64, journal: &mut Journal, request: &[u8], reply: &mut [u8]) {
	unsafe {
		match request[0] {
			OP_EMIT => journal.push(&request[1..]),
			OP_QUERY if request.len() >= 3 => {
				let format: u8 = request[1];
				let min_severity: u8 = request[2];
				let n: usize = render_query(journal, format, min_severity, reply);
				send_blocking(service, &reply[..n], 0);
			}
			_ => {}
		}
	}
}

// Render every stored record whose severity is at least `min_severity`, in the
// requested representation, into `reply`. Returns the number of bytes written.
fn render_query(journal: &Journal, format: u8, min_severity: u8, reply: &mut [u8]) -> usize {
	match format {
		FORMAT_JSON => render_json_array(journal, min_severity, reply),
		FORMAT_CBOR => render_cbor_array(journal, min_severity, reply),
		_ => render_text_lines(journal, min_severity, reply),
	}
}

// Whether a record passes the query's minimum-severity filter.
fn passes(rec: &LogRecord, min_severity: u8) -> bool {
	rec.severity() as u8 >= min_severity
}

// Human text: one record per line, each terminated by a newline.
fn render_text_lines(journal: &Journal, min_severity: u8, reply: &mut [u8]) -> usize {
	let mut pos: usize = 0;
	for i in 0..journal.count {
		let rec: LogRecord<'_> = match LogRecord::parse(journal.record(i)) {
			Some(r) => r,
			None => continue,
		};
		if !passes(&rec, min_severity) {
			continue;
		}
		match log::render_text(&rec, &mut reply[pos..]) {
			Some(n) => {
				pos += n;
				if pos < reply.len() {
					reply[pos] = b'\n';
					pos += 1;
				}
			}
			None => break,
		}
	}
	pos
}

// JSON: an array of per-record objects.
fn render_json_array(journal: &Journal, min_severity: u8, reply: &mut [u8]) -> usize {
	if reply.is_empty() {
		return 0;
	}
	reply[0] = b'[';
	let mut pos: usize = 1;
	let mut first: bool = true;
	for i in 0..journal.count {
		let rec: LogRecord<'_> = match LogRecord::parse(journal.record(i)) {
			Some(r) => r,
			None => continue,
		};
		if !passes(&rec, min_severity) {
			continue;
		}
		if !first {
			if pos >= reply.len() {
				break;
			}
			reply[pos] = b',';
			pos += 1;
		}
		match log::render_json(&rec, &mut reply[pos..]) {
			Some(n) => {
				pos += n;
				first = false;
			}
			None => break,
		}
	}
	if pos < reply.len() {
		reply[pos] = b']';
		pos += 1;
	}
	pos
}

// CBOR: an array of per-record maps. The journal holds fewer than 24 records, so
// the array header fits the one-byte short form (major type 4, `0x80 | count`).
fn render_cbor_array(journal: &Journal, min_severity: u8, reply: &mut [u8]) -> usize {
	if reply.is_empty() {
		return 0;
	}
	let mut matched: usize = 0;
	for i in 0..journal.count {
		if let Some(rec) = LogRecord::parse(journal.record(i)) {
			if passes(&rec, min_severity) {
				matched += 1;
			}
		}
	}
	reply[0] = 0x80 | (matched.min(RING_CAP) as u8);
	let mut pos: usize = 1;
	for i in 0..journal.count {
		let rec: LogRecord<'_> = match LogRecord::parse(journal.record(i)) {
			Some(r) => r,
			None => continue,
		};
		if !passes(&rec, min_severity) {
			continue;
		}
		match log::render_cbor(&rec, &mut reply[pos..]) {
			Some(n) => pos += n,
			None => break,
		}
	}
	pos
}
