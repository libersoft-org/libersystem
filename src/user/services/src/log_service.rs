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
// The journal is also durable: once ServiceManager delivers a volume client
// ("STORAGE" on the control channel - LogService starts before StorageService),
// the records of each boot are batched into `vol://system/log/boot-<n>` as
// length-framed encoded entries, size-capped per boot and rotated across boots.
// A `query` naming a boot number reads that file back, so `log --boot <n>` shows
// a previous boot's journal after a reboot.
//
// When the supervisor that started it drops the bootstrap channel, the service
// exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use proto::system::log::{self, Service};
use proto::system::{Entry, Error, OpenOpts, Query, Severity, config, volume};
use rt::*;

// The bounded in-memory journal: at most this many records, newest dropping
// oldest - deep enough to diagnose well past the last minute. The depth is the
// operator's policy (the `log.capacity` config key); this is the default until the
// supervisor delivers a ConfigService client (LogService starts before ConfigService,
// so the client arrives on the control channel once the config tree is up). A later
// `set` applies at the next boot.
const JOURNAL_CAP: usize = 4096;

// How often the serve loop's housekeeping tick flushes batched records to the
// volume (100 Hz ticks, ~5 s). Records are never written per emit: a flush
// happens on this timer, and immediately on a severity >= error record.
const FLUSH_TICKS: u64 = 500;

// On-disk rotation defaults, standing until the config tree answers (the
// `log.boots` / `log.disk-cap` keys): how many boots' journals are kept, and the
// per-boot byte cap when the volume's capacity cannot be read either. The
// derived default is capacity/1024, clamped to [64 kB, 1 MB].
const BOOTS_KEPT_DEFAULT: u32 = 8;
const DISK_CAP_FALLBACK: u64 = 256 * 1024;

// The durable side of the journal: this boot's records, encoded and batched for
// `vol://system/log/boot-<n>`.
struct Disk {
	// The volume client, 0 until ServiceManager delivers it.
	volume: u64,
	// This boot's number (one past the newest journal already on the volume).
	boot: u32,
	// Encoded records of this boot, oldest first, `total` bytes with framing.
	// Bounded by `cap` (oldest dropped); the whole sequence is rewritten to the
	// boot file on flush - the volume's write op is create-or-overwrite, and the
	// cap keeps the file small.
	frames: VecDeque<Vec<u8>>,
	total: usize,
	dirty: bool,
	// Per-boot byte cap; 0 until derived from the volume (or set by config).
	cap: u64,
	// How many boots' files are kept on the volume.
	boots: u32,
}

impl Disk {
	fn new() -> Disk {
		Disk { volume: 0, boot: 0, frames: VecDeque::new(), total: 0, dirty: false, cap: 0, boots: BOOTS_KEPT_DEFAULT }
	}

	// Record one entry for the disk: encode it and evict the oldest frames past
	// the per-boot cap. Cheap - no IO happens here.
	fn record(&mut self, entry: &Entry) {
		let encoded: Vec<u8> = entry.encode_vec();
		self.total += 4 + encoded.len();
		self.frames.push_back(encoded);
		let cap: usize = if self.cap != 0 { self.cap as usize } else { DISK_CAP_FALLBACK as usize };
		while self.total > cap && self.frames.len() > 1 {
			if let Some(oldest) = self.frames.pop_front() {
				self.total -= 4 + oldest.len();
			}
		}
		self.dirty = true;
	}

	// Adopt the delivered volume client: ensure `log/` exists, rotate old boot
	// files down to the kept count, pick this boot's number (one past the newest
	// kept), derive the per-boot cap from the volume's capacity unless config
	// already set one, and flush the records batched so far.
	fn attach(&mut self, volume: u64) {
		self.volume = volume;
		let mut client = volume::Client::new(ChannelTransport { chan: volume });
		let _ = client.mkdir("vol://system/log");
		if self.cap == 0 {
			self.cap = match client.capacity() {
				Some(Ok(bytes)) => (bytes / 1024).clamp(64 * 1024, 1024 * 1024),
				_ => DISK_CAP_FALLBACK,
			};
		}
		self.boot = self.prune(&mut client, self.boots.saturating_sub(1)) + 1;
		self.flush();
	}

	// Delete the oldest boot files until at most `keep` remain, returning the
	// newest boot number seen (0 = none). Called at attach (keep = boots - 1, so
	// this boot fits under the count) and again when config lowers the count.
	fn prune(&mut self, client: &mut volume::Client<ChannelTransport>, keep: u32) -> u32 {
		let mut boots: Vec<u32> = match client.list("vol://system/log") {
			Some(consumer) => unsafe { drain_stream(consumer, volume::list_read) }.iter().filter_map(|e| e.name.strip_prefix("boot-").and_then(|n| n.parse::<u32>().ok())).collect(),
			None => return 0,
		};
		boots.sort_unstable();
		while boots.len() > keep as usize {
			let oldest: u32 = boots.remove(0);
			let _ = client.remove(&format!("vol://system/log/boot-{oldest}"));
		}
		boots.last().copied().unwrap_or(0)
	}

	// Write this boot's batched records out as one length-framed file. Batched:
	// the serve loop calls this on its housekeeping tick and emit on a severity
	// >= error record, never per record. Failure (a read-only test volume) is
	// silent - the frames stay for the next attempt once new records arrive.
	fn flush(&mut self) {
		if self.volume == 0 || !self.dirty {
			return;
		}
		self.dirty = false;
		let mut bytes: Vec<u8> = Vec::with_capacity(self.total);
		for frame in &self.frames {
			bytes.extend_from_slice(&(frame.len() as u32).to_le_bytes());
			bytes.extend_from_slice(frame);
		}
		let data: proto::codec::Buffer = match unsafe { make_buffer(&bytes) } {
			Some(b) => b,
			None => return,
		};
		let path: String = format!("vol://system/log/boot-{}", self.boot);
		let mut client = volume::Client::new(ChannelTransport { chan: self.volume });
		let _ = client.write(&path, &data);
	}

	// Read a kept boot's journal back: decode the length-framed records of
	// `vol://system/log/boot-<n>`. None when the file does not exist (an unkept
	// or never-written boot) or no volume serves.
	fn read_boot(&self, boot: u32) -> Option<Vec<Entry>> {
		if self.volume == 0 {
			return None;
		}
		let mut client = volume::Client::new(ChannelTransport { chan: self.volume });
		let opts: OpenOpts = OpenOpts { path: format!("vol://system/log/boot-{boot}"), write: false, create: false };
		let result = match client.open(&opts) {
			Some(Ok(r)) if r.file != 0 => r,
			_ => return None,
		};
		let mapped: u64 = match unsafe { map_object(result.file) } {
			Some(base) => base,
			None => {
				unsafe { close(result.file) };
				return None;
			}
		};
		let bytes: &[u8] = unsafe { core::slice::from_raw_parts(mapped as *const u8, result.size as usize) };
		let mut entries: Vec<Entry> = Vec::new();
		let mut at: usize = 0;
		while at + 4 <= bytes.len() {
			let len: usize = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
			at += 4;
			if at + len > bytes.len() {
				break;
			}
			if let Some(entry) = Entry::decode(&bytes[at..at + len]) {
				entries.push(entry);
			}
			at += len;
		}
		unsafe {
			unmap_object(result.file);
			close(result.file);
		}
		Some(entries)
	}
}

// Stage bytes in a shared buffer for a zero-copy volume write: a read+map+transfer
// duplicate travels with the request, our own handle is closed.
unsafe fn make_buffer(bytes: &[u8]) -> Option<proto::codec::Buffer> {
	unsafe {
		let obj: i64 = memory_object_create(bytes.len().max(1) as u64);
		if obj < 0 {
			return None;
		}
		let obj: u64 = obj as u64;
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

// The in-memory journal: a bounded list of canonical log entries, plus the
// durable side (this boot's encoded records, batched for the volume).
struct Journal {
	entries: Vec<Entry>,
	cap: usize,
	disk: Disk,
}

impl Journal {
	fn new() -> Journal {
		Journal { entries: Vec::new(), cap: JOURNAL_CAP, disk: Disk::new() }
	}

	// Collect the entries of `source` matching `q`: at or above its minimum
	// severity (none = all), capped by its limit (0 = no cap). A limited query
	// returns the NEWEST matches - the journal is deep now, and a bounded reply
	// full of the oldest records would be useless for diagnosis. Shared by `query`
	// (one reply) and `tail` (streamed frame by frame) so both filter identically.
	fn filtered(source: &[Entry], q: &Query) -> Vec<Entry> {
		let min: u8 = q.min_severity.map(|s: Severity| s as u8).unwrap_or(0);
		let mut out: Vec<Entry> = Vec::new();
		for entry in source {
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
		self.disk.record(&entry);
		// a severity >= error record is flushed at once - the machine may be about
		// to die, and this is the record that says why. Everything else waits for
		// the housekeeping tick.
		if entry.severity as u8 >= Severity::Error as u8 {
			self.disk.flush();
		}
		self.entries.push(entry);
		if self.entries.len() > self.cap {
			self.entries.remove(0);
		}
		Ok(())
	}

	fn query(&mut self, q: Query) -> Result<Vec<Entry>, Error> {
		// a boot selector reads that boot's journal off the volume; otherwise the
		// live in-memory journal answers.
		if let Some(boot) = q.boot {
			let entries: Vec<Entry> = self.disk.read_boot(boot).ok_or(Error::NotFound)?;
			return Ok(Journal::filtered(&entries, &q));
		}
		Ok(Journal::filtered(&self.entries, &q))
	}

	// The streaming counterpart of `query`: the same bounded source, but the
	// generated codec frames each entry onto a fresh sub-channel instead of
	// packing them into one reply. Here the source is bounded (the journal), so
	// we return the snapshot and the serve loop streams it frame by frame.
	fn tail(&mut self, q: Query) -> Vec<Entry> {
		if let Some(boot) = q.boot {
			let entries: Vec<Entry> = self.disk.read_boot(boot).unwrap_or_default();
			return Journal::filtered(&entries, &q);
		}
		Journal::filtered(&self.entries, &q)
	}
}

impl Journal {
	// Adopt the config tree's journal policy over the delivered ConfigService
	// client: the in-memory depth (`log.capacity`), the on-disk per-boot byte cap
	// (`log.disk-cap`; 0 = keep the volume-derived default) and the kept-boots
	// count (`log.boots`), then close the client. Read once; a later `set` applies
	// at the next boot.
	fn adopt_config(&mut self, config: u64) {
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
		if let Some(Ok(value)) = client.get("log.disk-cap") {
			if let Ok(cap) = value.parse::<u64>() {
				if cap > 0 {
					self.disk.cap = cap;
				}
			}
		}
		if let Some(Ok(value)) = client.get("log.boots") {
			if let Ok(boots) = value.parse::<u32>() {
				if boots > 0 && boots != self.disk.boots {
					self.disk.boots = boots;
					// re-prune under the new count if the volume is already attached
					// (config arrives after storage in the boot order).
					if self.disk.volume != 0 {
						let mut vol = volume::Client::new(ChannelTransport { chan: self.disk.volume });
						self.disk.prune(&mut vol, boots);
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
	//    deliveries - LogService starts before both services it consumes: "STORAGE"
	//    (the volume the on-disk journal persists to) and "CONFIG" (the journal
	//    policy). The FLUSH_TICKS housekeeping tick (chan 0) flushes batched
	//    records.
	let mut journal: Journal = Journal::new();
	let mut request: [u8; 1024] = [0u8; 1024];
	let mut reply: [u8; 4096] = [0u8; 4096];
	unsafe {
		serve_multi_ticked(service, &[bootstrap], FLUSH_TICKS, &mut request, &mut reply, |chan: u64, req: &[u8], handle: u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> {
			if chan == 0 {
				journal.disk.flush();
				return None;
			}
			if chan == bootstrap {
				if req == b"STORAGE" && handle != 0 {
					journal.disk.attach(handle);
				} else if req == b"CONFIG" && handle != 0 {
					journal.adopt_config(handle);
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
