// The development-control protocol: the wire format, the session state machine and the
// operations, with no knowledge of how bytes reach it.
//
// It is kept separate from the driver that carries it on purpose. The transport today is a
// virtio-serial port this driver owns, but the protocol endpoint belongs with the
// development agent that will own the artifact registry, not with a device driver. Keeping
// the state machine behind a byte stream and a `Sink` means moving it there is a rewiring
// rather than a rewrite: the agent supplies a channel-backed sink and feeds it the same
// bytes.
//
// ---- the wire format ----
//
// Every frame is a 16-byte little-endian header, optionally followed by its payload:
//
//   magic u16 | version u8 | opcode u8 | request u32 | generation u32 | length u16 | status u16
//
// The header is fixed and leads with a magic so a desynchronised stream can be
// resynchronised on the magic alone. The x86_64 channel needs exactly that: UEFI writes its
// console output to every console-class device it enumerates, so the host sees a firmware
// preamble that nobody framed before the guest owns the port.
//
// Every bound is a constant here and is reported in the handshake, so a peer fails at the
// handshake instead of at the first payload that does not fit. Nothing grows with the
// session: the receive accumulator holds at most one frame, request IDs are tracked with a
// single watermark, a frame is dispatched whole or discarded whole, and the artifact
// registry is a fixed number of fixed-maximum candidates.
//
// The protocol carries bytes and typed fields only. No opcode accepts a host path, a guest
// path, a shell command or a capability request, and an artifact name is a bounded
// identifier checked character by character rather than anything the guest resolves.

#![allow(dead_code)]

extern crate alloc;

use alloc::vec::Vec;
use bootproto::compat;
use bootproto::elf::Elf;
use ipc_client::ChannelTransport;
use proto::system::{OpenOpts, volume};
use rt::*;

include!(concat!(env!("OUT_DIR"), "/program_paths.rs"));
include!(concat!(env!("OUT_DIR"), "/library_paths.rs"));

// The protocol's identifying prefix ("LD" on the wire) and the version this guest speaks. A
// version this guest does not know is refused rather than guessed at: the header layout
// after the version byte is what the version defines, so a mismatched frame's length field
// cannot be trusted to skip past it.
pub const MAGIC: u16 = 0x444c;
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 16;

// The frame bound, header included, and the payload bound that follows from it. The length
// field is a u16, so a payload larger than the bound is still expressible and therefore has
// to be rejected explicitly rather than assumed away.
pub const MAX_FRAME: usize = 65536;
pub const MAX_PAYLOAD: usize = MAX_FRAME - HEADER_LEN;
// The most requests a host may leave unanswered. The operations this version defines are
// all answered before the next frame is parsed, so nothing is ever outstanding yet and the
// bound exists for the peer to size itself by.
pub const MAX_OUTSTANDING: u16 = 16;

// The artifact registry's bounds, stated as numbers rather than left to whatever the
// allocator happens to tolerate. Every one of them is checked before a byte is reserved, so
// exceeding one is a typed refusal a host can act on and never an allocation that fails
// halfway through a publication.
//
// The registry is memory the driver holds and nothing else: candidates and generations never
// touch the canonical system volume, so a publication cannot damage what a cold boot would
// read, and a reboot returns the guest to exactly its built state.
pub const MAX_ARTIFACT: u32 = 32 * 1024 * 1024;
pub const MAX_REGISTRY: usize = 64 * 1024 * 1024;
pub const MAX_GENERATIONS_PER_ARTIFACT: usize = 3;
// An artifact name is a bounded identifier, not a path: it names a slot in the registry.
pub const MAX_NAME: usize = 48;

// What a DT_NEEDED entry appends to a provider's name, so a need can be matched against the
// declared provider closure.
const LIBRARY_SUFFIX: &str = ".lslib";

// The boot profile the artifact registry requires. Shadowing built artifacts with images a
// host streamed in is a development facility and must not exist anywhere else, so it is
// gated on the profile the firmware selected rather than on what happens to be attached.
//
// The gate is on the registry, not on the protocol. The same control channel is present in
// the cold test configuration of all three targets, where a runner drives a boot that has no
// business hot-publishing anything but very much needs to handshake, ping, type and reset.
// Gating the whole protocol would take that away to protect something the protocol does not
// do.
const REGISTRY_PROFILE: &[u8] = b"development";

// How a committed generation compares with the artifact it shadows, under the written
// provider compatibility rule.
//
// The comparison is against the INSTALLED artifact on the system volume, not against the
// generation published before it. That is the comparison a launch will have to make: what a
// process resolves today is the installed image, so whether a registry generation may stand
// in for it is a question about those two and nothing else. Comparing against the previous
// generation would answer a different question and would drift further from the real one
// with every publication.
pub const VERDICT_COMPATIBLE: u8 = 1;
pub const VERDICT_INCOMPATIBLE: u8 = 2;
// The installed artifact could not be read, so no claim about replacing it was made. Not a
// weaker COMPATIBLE: nothing was compared.
pub const VERDICT_UNKNOWN: u8 = 3;

// The most explanation a verdict carries. A rejection names the deciding field and its two
// values, which is a line, not a document; truncating past this keeps one publication from
// putting an unbounded reply on a bounded channel.
pub const MAX_VERDICT_DETAIL: usize = 200;

// The most terminal input one frame may carry. Far below the frame bound on purpose: each
// byte is a separate syscall into a console queue that is itself bounded, so a frame the
// size of the transport would mostly be refused after the queue filled. Sized to a
// comfortable line-oriented burst and reported in the handshake, so a host paces itself
// rather than discovering the limit halfway through a paste.
pub const MAX_TERM_INPUT: usize = 4096;

// Opcodes. Requests are host to guest, replies guest to host; a guest-to-host opcode
// arriving from the host is an unknown opcode, not a request.
pub const OP_HELLO: u8 = 0x01;
pub const OP_HELLO_ACK: u8 = 0x02;
pub const OP_PING: u8 = 0x03;
pub const OP_PONG: u8 = 0x04;
pub const OP_PUB_BEGIN: u8 = 0x10;
pub const OP_PUB_CHUNK: u8 = 0x11;
pub const OP_PUB_COMMIT: u8 = 0x12;
pub const OP_PUB_ABORT: u8 = 0x13;
pub const OP_PUB_ACK: u8 = 0x14;
pub const OP_GEN_LIST: u8 = 0x15;
pub const OP_GEN_LIST_REPLY: u8 = 0x16;
pub const OP_ROLLBACK: u8 = 0x17;
pub const OP_ROLLBACK_ACK: u8 = 0x18;
pub const OP_TERM_INPUT: u8 = 0x20;
pub const OP_TERM_ACK: u8 = 0x21;
pub const OP_RESET: u8 = 0x22;
pub const OP_RESET_ACK: u8 = 0x23;
pub const OP_ERROR: u8 = 0xff;

// Statuses. Every rejection names one of these, so a failure is explained by the frame that
// caused it rather than by a timeout on the host.
pub const ST_OK: u16 = 0;
pub const ST_BAD_VERSION: u16 = 1;
pub const ST_BAD_OPCODE: u16 = 2;
pub const ST_OVERSIZED: u16 = 3;
pub const ST_MALFORMED: u16 = 4;
pub const ST_HANDSHAKE_REQUIRED: u16 = 5;
pub const ST_DUPLICATE_REQUEST: u16 = 6;
pub const ST_TIMED_OUT: u16 = 7;
pub const ST_BUSY: u16 = 8;
pub const ST_BAD_GENERATION: u16 = 9;
pub const ST_INCOMPLETE: u16 = 10;
pub const ST_DIGEST_MISMATCH: u16 = 11;
pub const ST_NO_SPACE: u16 = 12;
pub const ST_TERM_REFUSED: u16 = 13;
// The publication verification statuses, in the order the checks run. Each names the check
// that refused the candidate, so a rejection is a fact about the image rather than a generic
// failure.
pub const ST_NOT_AN_IMAGE: u16 = 14;
pub const ST_WRONG_TARGET: u16 = 15;
pub const ST_NO_IDENTITY: u16 = 16;
pub const ST_NOT_OWNED: u16 = 17;
pub const ST_BAD_PROVIDERS: u16 = 18;
pub const ST_BAD_DYNAMIC: u16 = 19;
// A rollback with no earlier generation to return to.
pub const ST_NOTHING_TO_ROLL_BACK: u16 = 20;
// A registry operation on a boot that has no registry.
pub const ST_NO_REGISTRY: u16 = 21;
// A publication naming an artifact the installed manifest does not declare.
pub const ST_UNDECLARED: u16 = 22;

// The deadline on an incomplete frame, in scheduler ticks (100 Hz). A host that stops
// mid-frame - killed, disconnected, or writing a length it never delivers - must not leave
// the guest holding a fragment forever.
pub const PARTIAL_FRAME_TICKS: u64 = 200;

// The deadline on a silent session. The transport cannot report that a host disconnected,
// so silence is the only signal there is: a session that goes quiet for this long is closed
// and everything it holds is released, and the next host has to handshake again rather than
// inherit it. Comfortably longer than any single operation's own deadline, so it never cuts
// a working host off, and short enough that a crashed one does not leave its session
// standing.
pub const SESSION_IDLE_TICKS: u64 = 3000;

// The deadline on a publication between chunks. Shorter than the session's, because an
// open candidate holds a megabyte the guest cannot reclaim any other way: a host that stops
// streaming loses its candidate rather than parking that memory indefinitely.
pub const PUBLICATION_IDLE_TICKS: u64 = 1000;

// Where a completed frame goes. The transport implements this; the state machine never
// learns what it is. `false` means the peer has stopped consuming, which ends the session
// rather than being retried frame by frame - there is no point answering a peer that is not
// reading, and every later reply would inherit the same wait.
pub trait Sink {
	fn send(&mut self, opcode: u8, request: u32, generation: u32, status: u16, payload: &[u8]) -> bool;
}

// One artifact the guest is receiving. It is bounded at BEGIN, not as it grows, so the
// memory a publication can claim is known before the first byte of it arrives.
struct Candidate {
	generation: u32,
	name: Vec<u8>,
	total: u32,
	digest: [u8; 32],
	bytes: Vec<u8>,
	// When this candidate is abandoned for want of a chunk. Absolute ticks.
	deadline: u64,
}

// One verified, immutable generation of an artifact. It becomes visible in one step at the
// end of commit - every check has already passed by then, so there is no moment at which a
// half-verified generation is queryable.
struct Generation {
	generation: u32,
	digest: [u8; 32],
	bytes: Vec<u8>,
	// When it was published, as a Unix timestamp, or 0 when the guest has no clock to ask.
	// A forgotten override is easiest to recognise by being old.
	published_at: u64,
	// Whether this generation could have replaced the one it succeeded without a restart,
	// and what decided that. Recorded at commit, when both images are in hand, because that
	// is the only moment the comparison is cheap and the answer is what a caller asked for.
	verdict: u8,
	detail: Vec<u8>,
}

// One named artifact and the generations retained for it, newest last. Retention is per
// artifact rather than across the registry, so publishing one thing repeatedly cannot evict
// the history of everything else.
struct Artifact {
	name: Vec<u8>,
	generations: Vec<Generation>,
}

// One protocol session and the registry it operates on. A HELLO opens the session and the
// idle deadline closes it. That pair exists because a virtio-serial port without MULTIPORT
// reports no open and no close: the guest is never told that a host connected or went away,
// so a session that only ever opened would outlive the host that opened it, and the next
// host would silently inherit its state instead of starting from a known one. Bounding the
// session by silence turns a disconnect - crash, kill, unplugged terminal - into the same
// deterministic outcome as an orderly one, on a deadline rather than on an event the
// transport cannot deliver.
//
// The registry deliberately outlives a session while the candidate does not. Accepted
// generations are guest state a later host is meant to be able to query; a half-streamed
// candidate is one host's unfinished business and is cancelled with it.
pub struct Session {
	// Whether the handshake completed. Every other opcode fails until it has.
	handshake: bool,
	// The highest request ID accepted so far. IDs must be non-zero and strictly increasing,
	// which rejects both a duplicate and a replay with one word of state instead of a table
	// of in-flight IDs - and stays correct when the host does leave requests outstanding,
	// because it constrains the order they are issued in, not the order they are answered.
	high_request: u32,
	// The next generation to hand out. Monotonic for the guest's whole life, so a number is
	// never reused and a stale reference is always recognisable as stale.
	next_generation: u32,
	// Whether this boot may hold a registry at all, read once from the kernel because the
	// answer cannot change while the guest runs.
	registry_allowed: bool,
	// A value drawn once per boot and reported in every handshake, so a host can tell which
	// boot answered it. A development instance is meant to outlive the tools that drive it,
	// which is exactly the situation where a tool can be talking to a guest that restarted
	// under it - and publishing into, or reading a registry from, a boot that is not the one
	// you think you have is the kind of confusion this whole milestone exists to remove.
	boot_nonce: [u8; 8],
	// The volume client the installed artifacts are read through, to decide whether a
	// candidate may stand in for the one it shadows. Zero when none was wired, which leaves
	// every verdict unknown rather than silently claiming compatibility.
	storage: u64,
	candidate: Option<Candidate>,
	artifacts: Vec<Artifact>,
	// Live registry content in bytes, tracked rather than recomputed so the budget can be
	// checked before a reservation instead of after an allocation.
	registry_bytes: usize,
}

impl Session {
	pub fn new(storage: u64) -> Session {
		let mut profile: [u8; 32] = [0u8; 32];
		let len: usize = unsafe { boot_profile(&mut profile) };
		let mut boot_nonce: [u8; 8] = [0u8; 8];
		unsafe { random_get(&mut boot_nonce) };
		Session { boot_nonce, handshake: false, high_request: 0, next_generation: 1, registry_allowed: &profile[..len] == REGISTRY_PROFILE, storage, candidate: None, artifacts: Vec::new(), registry_bytes: 0 }
	}

	pub fn is_open(&self) -> bool {
		self.handshake
	}

	// Close the session and cancel what only it owned. The registry survives; the candidate
	// does not.
	pub fn close(&mut self) {
		self.handshake = false;
		self.high_request = 0;
		self.candidate = None;
	}

	// When the open candidate is abandoned, or 0 when none is open.
	pub fn publication_deadline(&self) -> u64 {
		match &self.candidate {
			Some(c) => c.deadline,
			None => 0,
		}
	}

	// Drop the candidate whose deadline expired. Nothing is reported: the request it belongs
	// to was answered when its last chunk was, and the host learns of the cancellation from
	// the next operation on that generation being refused.
	pub fn expire_publication(&mut self) {
		self.candidate = None;
	}

	// Discard the fragment whose deadline expired. It is reported against its own request
	// when a full header arrived, so a host that sent a length it never delivered is told
	// which request died rather than being left to time out; a fragment too short to name a
	// request is dropped silently, since there is nothing to answer.
	pub fn fail_partial(&mut self, pending: &mut Vec<u8>, sink: &mut impl Sink) {
		if pending.len() >= HEADER_LEN && u16::from_le_bytes([pending[0], pending[1]]) == MAGIC {
			let request: u32 = u32::from_le_bytes([pending[4], pending[5], pending[6], pending[7]]);
			sink.send(OP_ERROR, request, 0, ST_TIMED_OUT, &[]);
		}
		pending.clear();
	}

	// Parse and dispatch every complete frame the accumulator holds, leaving any trailing
	// fragment for the next arrival. Returns false when the sink stopped accepting replies.
	pub fn consume(&mut self, pending: &mut Vec<u8>, sink: &mut impl Sink) -> bool {
		loop {
			resync(pending);
			if pending.len() < HEADER_LEN {
				return true;
			}
			let version: u8 = pending[2];
			let opcode: u8 = pending[3];
			let request: u32 = u32::from_le_bytes([pending[4], pending[5], pending[6], pending[7]]);
			let generation: u32 = u32::from_le_bytes([pending[8], pending[9], pending[10], pending[11]]);
			let length: usize = u16::from_le_bytes([pending[12], pending[13]]) as usize;
			// A version this guest does not speak makes the rest of the frame unreadable: the
			// length field only means what the version says it means, so there is no safe
			// number of bytes to skip. Report it and drop everything buffered, which puts the
			// stream back at a resynchronisation point instead of at a guess.
			if version != VERSION {
				let live: bool = sink.send(OP_ERROR, request, 0, ST_BAD_VERSION, &[]);
				self.close();
				pending.clear();
				return live;
			}
			// Wait for the whole frame. The length is a u16 and the accumulator is sized for
			// the largest one expressible, so even an oversized frame is buffered in full and
			// then discarded in full, which keeps the stream framed rather than forcing a
			// resynchronisation after every rejection.
			if pending.len() < HEADER_LEN + length {
				return true;
			}
			let live: bool = self.dispatch(opcode, request, generation, &pending[HEADER_LEN..HEADER_LEN + length], sink);
			pending.drain(..HEADER_LEN + length);
			if !live {
				return false;
			}
		}
	}

	// Dispatch one complete, correctly versioned frame. Every path answers exactly once, with
	// a reply on success and an OP_ERROR naming the status on rejection, so the host never has
	// to distinguish a refusal from a loss.
	fn dispatch(&mut self, opcode: u8, request: u32, generation: u32, payload: &[u8], sink: &mut impl Sink) -> bool {
		if request == 0 {
			return sink.send(OP_ERROR, request, generation, ST_MALFORMED, &[]);
		}
		if payload.len() > MAX_PAYLOAD {
			return sink.send(OP_ERROR, request, generation, ST_OVERSIZED, &[]);
		}
		// The handshake is the session's reset point, so it is the one opcode exempt from the
		// request-ID watermark it resets. Everything else is refused until it has run.
		if opcode == OP_HELLO {
			if generation != 0 {
				return sink.send(OP_ERROR, request, generation, ST_MALFORMED, &[]);
			}
			self.close();
			self.handshake = true;
			self.high_request = request;
			let mut reply: [u8; 36] = [0u8; 36];
			reply[..4].copy_from_slice(&(MAX_FRAME as u32).to_le_bytes());
			reply[4..8].copy_from_slice(&(MAX_PAYLOAD as u32).to_le_bytes());
			reply[8..10].copy_from_slice(&MAX_OUTSTANDING.to_le_bytes());
			reply[10..12].copy_from_slice(&(MAX_NAME as u16).to_le_bytes());
			reply[12..16].copy_from_slice(&MAX_ARTIFACT.to_le_bytes());
			reply[16..18].copy_from_slice(&(MAX_GENERATIONS_PER_ARTIFACT as u16).to_le_bytes());
			reply[18..20].copy_from_slice(&(MAX_TERM_INPUT as u16).to_le_bytes());
			reply[20..24].copy_from_slice(&(MAX_REGISTRY as u32).to_le_bytes());
			reply[24] = u8::from(self.registry_allowed);
			reply[28..36].copy_from_slice(&self.boot_nonce);
			return sink.send(OP_HELLO_ACK, request, 0, ST_OK, &reply);
		}
		if !self.handshake {
			return sink.send(OP_ERROR, request, generation, ST_HANDSHAKE_REQUIRED, &[]);
		}
		if request <= self.high_request {
			return sink.send(OP_ERROR, request, generation, ST_DUPLICATE_REQUEST, &[]);
		}
		self.high_request = request;
		// The generation field names the candidate an operation applies to. The operations
		// that are not about one candidate must leave it zero, so the field never acquires an
		// accidental second meaning that a later version would have to preserve.
		let session_scoped: bool = matches!(opcode, OP_PING | OP_PUB_BEGIN | OP_GEN_LIST | OP_ROLLBACK | OP_TERM_INPUT | OP_RESET);
		if session_scoped && generation != 0 {
			return sink.send(OP_ERROR, request, generation, ST_MALFORMED, &[]);
		}
		// Every registry operation is refused outright on a boot that has no registry, in one
		// place rather than five, so no later operation can be added and quietly miss the gate.
		if !self.registry_allowed && matches!(opcode, OP_PUB_BEGIN | OP_PUB_CHUNK | OP_PUB_COMMIT | OP_PUB_ABORT | OP_GEN_LIST | OP_ROLLBACK) {
			return sink.send(OP_ERROR, request, generation, ST_NO_REGISTRY, &[]);
		}
		match opcode {
			// Echo the payload, so a ping measures the round trip of a real payload rather
			// than of an empty frame.
			OP_PING => sink.send(OP_PONG, request, 0, ST_OK, payload),
			OP_PUB_BEGIN => self.publication_begin(request, payload, sink),
			OP_PUB_CHUNK => self.publication_chunk(request, generation, payload, sink),
			OP_PUB_COMMIT => self.publication_commit(request, generation, payload, sink),
			OP_PUB_ABORT => self.publication_abort(request, generation, payload, sink),
			OP_GEN_LIST => self.generation_list(request, sink),
			OP_ROLLBACK => self.rollback(request, payload, sink),
			OP_TERM_INPUT => terminal_input(request, payload, sink),
			OP_RESET => self.reset(request, sink),
			_ => sink.send(OP_ERROR, request, generation, ST_BAD_OPCODE, &[]),
		}
	}

	// Open a candidate. Everything the publication will cost is declared and checked here -
	// its length, its name, its digest - so a host learns that its artifact is unacceptable
	// before it streams a megabyte rather than after.
	//
	// Payload: total u32 | digest [u8; 32] | name_len u8 | name.
	fn publication_begin(&mut self, request: u32, payload: &[u8], sink: &mut impl Sink) -> bool {
		if payload.len() < 37 {
			return sink.send(OP_ERROR, request, 0, ST_MALFORMED, &[]);
		}
		let total: u32 = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
		let name_len: usize = payload[36] as usize;
		if payload.len() != 37 + name_len || !name_is_valid(&payload[37..37 + name_len]) {
			return sink.send(OP_ERROR, request, 0, ST_MALFORMED, &[]);
		}
		if total == 0 || total > MAX_ARTIFACT {
			return sink.send(OP_ERROR, request, 0, ST_OVERSIZED, &[]);
		}
		// One candidate at a time. A host that wants to restart aborts first, which keeps the
		// memory bound a fact rather than a hope.
		if self.candidate.is_some() {
			return sink.send(OP_ERROR, request, 0, ST_BUSY, &[]);
		}
		// The registry budget is checked here, against what this publication would add on top
		// of what is already live, rather than at commit. Reserving 32 MB and discovering at
		// the end that it does not fit would waste the whole transfer, and reserving it and
		// failing to allocate would turn a stated limit into an allocator's opinion.
		if self.registry_bytes + total as usize > MAX_REGISTRY {
			return sink.send(OP_ERROR, request, 0, ST_NO_SPACE, &(self.registry_bytes as u32).to_le_bytes());
		}
		let mut digest: [u8; 32] = [0u8; 32];
		digest.copy_from_slice(&payload[4..36]);
		let generation: u32 = self.next_generation;
		self.next_generation += 1;
		let mut bytes: Vec<u8> = Vec::new();
		bytes.reserve_exact(total as usize);
		self.candidate = Some(Candidate { generation, name: payload[37..37 + name_len].to_vec(), total, digest, bytes, deadline: unsafe { clock() } + PUBLICATION_IDLE_TICKS });
		sink.send(OP_PUB_ACK, request, generation, ST_OK, &0u32.to_le_bytes())
	}

	// Take one chunk. The reply carries the byte count accepted so far, so a host can resume
	// its own accounting from what the guest actually has rather than from what it believes it
	// sent.
	fn publication_chunk(&mut self, request: u32, generation: u32, payload: &[u8], sink: &mut impl Sink) -> bool {
		let candidate = match &mut self.candidate {
			Some(c) if c.generation == generation => c,
			_ => return sink.send(OP_ERROR, request, generation, ST_BAD_GENERATION, &[]),
		};
		if payload.is_empty() {
			return sink.send(OP_ERROR, request, generation, ST_MALFORMED, &[]);
		}
		// Past the declared length is refused rather than truncated: the declaration is what
		// the memory bound was checked against, so overrunning it is the host contradicting
		// itself, not a stream that needs trimming.
		if candidate.bytes.len() + payload.len() > candidate.total as usize {
			let live: bool = sink.send(OP_ERROR, request, generation, ST_OVERSIZED, &[]);
			self.candidate = None;
			return live;
		}
		candidate.bytes.extend_from_slice(payload);
		candidate.deadline = unsafe { clock() } + PUBLICATION_IDLE_TICKS;
		let received: u32 = candidate.bytes.len() as u32;
		sink.send(OP_PUB_ACK, request, generation, ST_OK, &received.to_le_bytes())
	}

	// Accept the candidate, or refuse it. Nothing here is trusted from the chunks: the length
	// proves nothing was lost, the digest proves nothing was altered, and the checks after
	// them prove the bytes are an image this guest could actually use under the name it was
	// published as. Every check runs before anything becomes visible, so the registry never
	// holds a generation that only some of them passed.
	//
	// The checks are ordered cheapest and most fundamental first, and each has its own status,
	// so a refusal says which one decided it rather than leaving a host to guess.
	fn publication_commit(&mut self, request: u32, generation: u32, _payload: &[u8], sink: &mut impl Sink) -> bool {
		let candidate = match &self.candidate {
			Some(c) if c.generation == generation => c,
			_ => return sink.send(OP_ERROR, request, generation, ST_BAD_GENERATION, &[]),
		};
		if candidate.bytes.len() != candidate.total as usize {
			return sink.send(OP_ERROR, request, generation, ST_INCOMPLETE, &(candidate.bytes.len() as u32).to_le_bytes());
		}
		if bootproto::sha256::digest(&candidate.bytes) != candidate.digest {
			let live: bool = sink.send(OP_ERROR, request, generation, ST_DIGEST_MISMATCH, &[]);
			self.candidate = None;
			return live;
		}
		let installed_path: Option<&'static str> = declared_path(&candidate.name);
		if installed_path.is_none() {
			// An artifact the installed manifest does not declare has nothing to shadow, and
			// shadowing a name the system never had is how a registry stops being a
			// development convenience and starts being a way to introduce programs.
			let live: bool = sink.send(OP_ERROR, request, generation, ST_UNDECLARED, &[]);
			self.candidate = None;
			return live;
		}
		if let Some(status) = verify_image(&candidate.bytes, &candidate.name) {
			// A candidate that failed verification is dropped, not kept for a second opinion:
			// the bytes are known bad and holding them would spend the registry budget on
			// something no operation could ever use.
			let live: bool = sink.send(OP_ERROR, request, generation, status, &[]);
			self.candidate = None;
			return live;
		}
		let candidate = self.candidate.take().expect("candidate was matched above");
		// Decide, and record, whether this generation could have replaced the one it succeeds
		// without a restart. It is a recorded fact, never a gate: the artifact passed every
		// check above, so it belongs in the registry either way, and what the verdict decides
		// is whether installing it is a hot swap or needs the cold path.
		let index: Option<usize> = self.artifacts.iter().position(|entry| entry.name == candidate.name);
		let (verdict, detail): (u8, Vec<u8>) = match unsafe { read_installed(self.storage, installed_path.expect("the path was checked above")) } {
			Some(installed) => match compat::decide(&installed, &candidate.bytes) {
				compat::Verdict::Compatible => (VERDICT_COMPATIBLE, Vec::new()),
				compat::Verdict::Incompatible(reason) => (VERDICT_INCOMPATIBLE, explain(&reason)),
			},
			None => (VERDICT_UNKNOWN, b"the installed artifact could not be read".to_vec()),
		};
		let entry = Generation { generation: candidate.generation, digest: candidate.digest, published_at: unsafe { clock_rtc() }, verdict, detail, bytes: candidate.bytes };
		let added: usize = entry.bytes.len();
		let at: usize = match index {
			Some(at) => at,
			None => {
				self.artifacts.push(Artifact { name: candidate.name, generations: Vec::new() });
				self.artifacts.len() - 1
			}
		};
		// Retention is per artifact and by count: the oldest generation of THIS artifact makes
		// way, so publishing one thing repeatedly never evicts another thing's history.
		let artifact = &mut self.artifacts[at];
		if artifact.generations.len() >= MAX_GENERATIONS_PER_ARTIFACT {
			let dropped = artifact.generations.remove(0);
			self.registry_bytes -= dropped.bytes.len();
		}
		let artifact = &mut self.artifacts[at];
		artifact.generations.push(entry);
		self.registry_bytes += added;
		let entry = self.artifacts[at].generations.last().expect("the generation was just pushed");
		let mut reply: Vec<u8> = Vec::new();
		reply.extend_from_slice(&(self.artifacts.len() as u16).to_le_bytes());
		reply.push(entry.verdict);
		reply.push(entry.detail.len() as u8);
		reply.extend_from_slice(&entry.detail);
		sink.send(OP_PUB_ACK, request, generation, ST_OK, &reply)
	}

	// Return an artifact to the generation before its newest, as a named operation rather
	// than as something that only happens when a publication fails. A development loop
	// overshoots as often as it fails, and undoing that deliberately must not require
	// republishing an older image the host may no longer have.
	//
	// Payload: name_len u8 | name. Reply: the generation now newest, or a refusal when there
	// is nothing behind the current one.
	fn rollback(&mut self, request: u32, payload: &[u8], sink: &mut impl Sink) -> bool {
		if payload.is_empty() {
			return sink.send(OP_ERROR, request, 0, ST_MALFORMED, &[]);
		}
		let name_len: usize = payload[0] as usize;
		if payload.len() != 1 + name_len || !name_is_valid(&payload[1..]) {
			return sink.send(OP_ERROR, request, 0, ST_MALFORMED, &[]);
		}
		let at: usize = match self.artifacts.iter().position(|entry| entry.name == payload[1..]) {
			Some(at) => at,
			None => return sink.send(OP_ERROR, request, 0, ST_NOTHING_TO_ROLL_BACK, &[]),
		};
		if self.artifacts[at].generations.len() < 2 {
			return sink.send(OP_ERROR, request, 0, ST_NOTHING_TO_ROLL_BACK, &[]);
		}
		let dropped = self.artifacts[at].generations.pop().expect("length was checked above");
		self.registry_bytes -= dropped.bytes.len();
		let now = self.artifacts[at].generations.last().expect("length was checked above");
		let mut reply: [u8; 8] = [0u8; 8];
		reply[..4].copy_from_slice(&now.generation.to_le_bytes());
		reply[4..8].copy_from_slice(&dropped.generation.to_le_bytes());
		sink.send(OP_ROLLBACK_ACK, request, now.generation, ST_OK, &reply)
	}

	// Cancel the candidate. An abort of something already gone is a bad generation, not a
	// success: a host that believes it holds a candidate it does not must be told so.
	fn publication_abort(&mut self, request: u32, generation: u32, _payload: &[u8], sink: &mut impl Sink) -> bool {
		match &self.candidate {
			Some(c) if c.generation == generation => {
				self.candidate = None;
				sink.send(OP_PUB_ACK, request, generation, ST_OK, &[])
			}
			_ => sink.send(OP_ERROR, request, generation, ST_BAD_GENERATION, &[]),
		}
	}

	// List what the registry holds: every artifact, and for each the generations retained for
	// it oldest first. This is the query that makes a forgotten override visible - the newest
	// generation of every artifact here is one that shadows what the system volume carries,
	// and its publication time is what marks it as something left behind rather than
	// something just done.
	//
	// The reply is bounded by construction: the retained generation count and the name length
	// are both fixed, so the whole listing cannot approach the frame bound and never needs
	// paging.
	//
	// Reply payload: artifacts u16 | registry_bytes u32 | per artifact: name_len u8 | name |
	// generations u8 | per generation: generation u32 | length u32 | digest [u8; 32] |
	// published_at u64 | verdict u8 | detail_len u8 | detail.
	fn generation_list(&mut self, request: u32, sink: &mut impl Sink) -> bool {
		let mut reply: Vec<u8> = Vec::new();
		reply.extend_from_slice(&(self.artifacts.len() as u16).to_le_bytes());
		reply.extend_from_slice(&(self.registry_bytes as u32).to_le_bytes());
		for artifact in &self.artifacts {
			reply.push(artifact.name.len() as u8);
			reply.extend_from_slice(&artifact.name);
			reply.push(artifact.generations.len() as u8);
			for entry in &artifact.generations {
				reply.extend_from_slice(&entry.generation.to_le_bytes());
				reply.extend_from_slice(&(entry.bytes.len() as u32).to_le_bytes());
				reply.extend_from_slice(&entry.digest);
				reply.extend_from_slice(&entry.published_at.to_le_bytes());
				reply.push(entry.verdict);
				reply.push(entry.detail.len() as u8);
				reply.extend_from_slice(&entry.detail);
			}
		}
		sink.send(OP_GEN_LIST_REPLY, request, 0, ST_OK, &reply)
	}

	// Drop every piece of development state this protocol owns: the candidate being streamed
	// and the generations already accepted. It is deliberately not a reboot and does not
	// touch anything else in the guest - what a reset means will widen when the development
	// agent owns installed artifacts and running scenarios, and the reply says exactly what
	// was dropped so a caller never has to assume the scope.
	//
	// Reply payload: generations u16 | candidate u8.
	fn reset(&mut self, request: u32, sink: &mut impl Sink) -> bool {
		let dropped: u16 = self.artifacts.iter().map(|artifact| artifact.generations.len() as u16).sum();
		let candidate: u8 = u8::from(self.candidate.is_some());
		self.artifacts.clear();
		self.registry_bytes = 0;
		self.candidate = None;
		let mut reply: [u8; 3] = [0u8; 3];
		reply[..2].copy_from_slice(&dropped.to_le_bytes());
		reply[2] = candidate;
		sink.send(OP_RESET_ACK, request, 0, ST_OK, &reply)
	}
}

// Type into the guest's console. Bytes take the serial arrival path, which the console
// service accepts whether or not its display is focused: a runner driving a guest has no
// display to focus, and input that silently depended on focus would be the least
// reproducible thing in the protocol.
//
// Feeding stops at the first byte the console refuses rather than pushing past it, and the
// count that did land is reported either way, so a host resumes from what arrived instead
// of replaying a line and doubling it.
//
// Reply payload: accepted u16.
fn terminal_input(request: u32, payload: &[u8], sink: &mut impl Sink) -> bool {
	if payload.is_empty() {
		return sink.send(OP_ERROR, request, 0, ST_MALFORMED, &[]);
	}
	if payload.len() > MAX_TERM_INPUT {
		return sink.send(OP_ERROR, request, 0, ST_OVERSIZED, &[]);
	}
	let mut accepted: u16 = 0;
	for &byte in payload {
		if unsafe { console_feed_serial(byte) } != 0 {
			break;
		}
		accepted += 1;
	}
	let count: [u8; 2] = accepted.to_le_bytes();
	if accepted as usize == payload.len() { sink.send(OP_TERM_ACK, request, 0, ST_OK, &count) } else { sink.send(OP_ERROR, request, 0, ST_TERM_REFUSED, &count) }
}

// An artifact name identifies a registry slot. It is checked character by character rather
// than sanitised, because the only names worth accepting are the ones the guest would never
// have to interpret: no separator, no traversal, nothing a later consumer could mistake for
// a path.
fn name_is_valid(name: &[u8]) -> bool {
	if name.is_empty() || name.len() > MAX_NAME {
		return false;
	}
	name.iter().all(|&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.') && name[0] != b'.'
}

// Drop everything before the next plausible frame start. Leading junk is expected once per
// boot on x86_64 (the UEFI console preamble) and possible at any time from a desynchronised
// host, so the parser hunts for the magic rather than trusting the first byte it is given.
// When no magic is present the last byte is kept, because a magic can straddle two receive
// buffers.
fn resync(pending: &mut Vec<u8>) {
	if pending.len() >= 2 && u16::from_le_bytes([pending[0], pending[1]]) == MAGIC {
		return;
	}
	let mut i: usize = 1;
	while i + 2 <= pending.len() {
		if u16::from_le_bytes([pending[i], pending[i + 1]]) == MAGIC {
			pending.drain(..i);
			return;
		}
		i += 1;
	}
	let keep: usize = if pending.is_empty() { 0 } else { 1 };
	pending.drain(..pending.len() - keep);
}

// Render a rejection as the one line a caller needs: what decided it, and the two values
// that differed. It is written by hand rather than formatted so the driver keeps no
// formatting machinery, and truncated at the bound so a pathological name cannot stretch
// the reply.
fn explain(reason: &compat::Reason) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::new();
	match reason {
		compat::Reason::NotAnElf { installed } => {
			out.extend_from_slice(b"not a readable ELF: ");
			out.extend_from_slice(side(*installed));
		}
		compat::Reason::MachineMismatch { installed, candidate } => {
			out.extend_from_slice(b"built for a different machine: ");
			push_number(&mut out, *installed as usize);
			out.extend_from_slice(b" -> ");
			push_number(&mut out, *candidate as usize);
		}
		compat::Reason::MissingIdentity { installed } => {
			out.extend_from_slice(b"no identity record: ");
			out.extend_from_slice(side(*installed));
		}
		compat::Reason::UnknownIdentityFormat { format } => {
			out.extend_from_slice(b"unknown identity format ");
			out.extend_from_slice(format.as_bytes());
		}
		compat::Reason::MissingField { field, installed } => {
			out.extend_from_slice(b"identity field ");
			out.extend_from_slice(field.as_bytes());
			out.extend_from_slice(b" absent from ");
			out.extend_from_slice(side(*installed));
		}
		compat::Reason::IdentityField { field, installed, candidate } => {
			out.extend_from_slice(b"identity field ");
			out.extend_from_slice(field.as_bytes());
			out.extend_from_slice(b": ");
			out.extend_from_slice(installed.as_bytes());
			out.extend_from_slice(b" -> ");
			out.extend_from_slice(candidate.as_bytes());
		}
		compat::Reason::ProviderList { position, installed, candidate } => {
			out.extend_from_slice(b"provider closure at ");
			push_number(&mut out, *position);
			out.extend_from_slice(b": ");
			out.extend_from_slice(entry_or_end(*installed));
			out.extend_from_slice(b" -> ");
			out.extend_from_slice(entry_or_end(*candidate));
		}
		compat::Reason::NeededList { position, installed, candidate } => {
			out.extend_from_slice(b"dependency at ");
			push_number(&mut out, *position);
			out.extend_from_slice(b": ");
			out.extend_from_slice(entry_or_end(*installed));
			out.extend_from_slice(b" -> ");
			out.extend_from_slice(entry_or_end(*candidate));
		}
		compat::Reason::UnreadableDynamic { installed } => {
			out.extend_from_slice(b"unreadable dynamic table: ");
			out.extend_from_slice(side(*installed));
		}
		compat::Reason::ExportRemoved { symbol } => {
			out.extend_from_slice(b"export removed or narrowed: ");
			out.extend_from_slice(symbol.as_bytes());
		}
		compat::Reason::ExportChanged { symbol, field } => {
			out.extend_from_slice(b"export ");
			out.extend_from_slice(symbol.as_bytes());
			out.extend_from_slice(b" changed ");
			out.extend_from_slice(field.as_bytes());
		}
	}
	out.truncate(MAX_VERDICT_DETAIL);
	out
}

fn side(installed: bool) -> &'static [u8] {
	if installed { b"the installed provider" } else { b"the candidate" }
}

fn entry_or_end(entry: Option<&str>) -> &[u8] {
	match entry {
		Some(name) => name.as_bytes(),
		None => b"(end of list)",
	}
}

fn push_number(out: &mut Vec<u8>, value: usize) {
	if value >= 10 {
		push_number(out, value / 10);
	}
	out.push(b'0' + (value % 10) as u8);
}

// Verify that a candidate's bytes are an image this guest could use under the name it was
// published as. Returns the status of the first check that refused it, or None when every
// check passed.
//
// The order is deliberate: each check assumes what the ones before it established, so a
// later check never has to defend itself against input an earlier one would have caught.
fn verify_image(bytes: &[u8], name: &[u8]) -> Option<u16> {
	// The declared machine is read from the header before parsing, so an image built for
	// another target is refused as the wrong target rather than as an unreadable file - the
	// two are very different things for someone who just published from the wrong tree.
	let machine: u16 = match compat::declared_machine(bytes) {
		Some(machine) => machine,
		None => return Some(ST_NOT_AN_IMAGE),
	};
	let image = match Elf::parse_for_machine(bytes, machine) {
		Some(image) => image,
		None => return Some(ST_NOT_AN_IMAGE),
	};
	// `Elf::parse` accepts only this guest's own machine, which is exactly the question
	// being asked here.
	if Elf::parse(bytes).is_none() {
		return Some(ST_WRONG_TARGET);
	}
	let identity = match compat::Identity::read(&image) {
		Some(identity) => identity,
		None => return Some(ST_NO_IDENTITY),
	};
	if identity.field("format") != Some(compat::IDENTITY_FORMAT) {
		return Some(ST_NO_IDENTITY);
	}
	// Manifest ownership: the image has to be the artifact it is being published as. Without
	// this a host could shadow any name with any image, and the registry would be a place to
	// put bytes rather than a place to replace a specific one.
	match identity.field("artifact") {
		Some(artifact) if artifact.as_bytes() == name => {}
		_ => return Some(ST_NOT_OWNED),
	}
	// The dynamic metadata has to be readable, because everything that would later resolve
	// against this image reads it. An image whose dynamic table cannot be walked is not
	// something to discover at load time.
	let info = match image.dynamic_info() {
		Some(Some(info)) => info,
		_ => return Some(ST_BAD_DYNAMIC),
	};
	if image.symbols(&info).is_none() {
		return Some(ST_BAD_DYNAMIC);
	}
	// The declared providers have to account for what the image actually needs. The record's
	// provider list is the resolved closure, so every direct dependency must appear in it; a
	// need the record does not declare means the record and the image disagree about what
	// this artifact was linked against.
	let needed = match image.needed_names(&info) {
		Some(names) => names,
		None => return Some(ST_BAD_DYNAMIC),
	};
	for need in needed {
		let stem: &str = need.strip_suffix(LIBRARY_SUFFIX).unwrap_or(need);
		if !identity.providers().any(|provider| provider == stem) {
			return Some(ST_BAD_PROVIDERS);
		}
	}
	None
}

// Where the installed manifest says an artifact lives, or None when it declares no such
// name. This is the manifest remaining the authority for artifact names: the registry can
// shadow what the system already has and nothing else.
//
// A program and a library are looked up separately because the manifest keeps them apart,
// and a name that is neither is not an artifact this system knows.
fn declared_path(name: &[u8]) -> Option<&'static str> {
	let name: &str = core::str::from_utf8(name).ok()?;
	program_path(name).or_else(|| library_path(&alloc::format!("{name}.lslib")))
}

// Read an installed artifact off the system volume. Returns None when there is no storage
// client, or the artifact cannot be opened or mapped - all of which leave the verdict
// unknown rather than assumed.
unsafe fn read_installed(storage: u64, path: &str) -> Option<Vec<u8>> {
	unsafe {
		if storage == 0 {
			return None;
		}
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let result = match client.open(&OpenOpts { path: alloc::string::String::from(path), write: false, create: false })? {
			Ok(result) if result.file != 0 && result.size != 0 => result,
			_ => return None,
		};
		let len: usize = match usize::try_from(result.size) {
			Ok(len) => len,
			Err(_) => {
				close(result.file);
				return None;
			}
		};
		let address: u64 = match map_object(result.file) {
			Some(address) => address,
			None => {
				close(result.file);
				return None;
			}
		};
		let bytes: Vec<u8> = core::slice::from_raw_parts(address as *const u8, len).to_vec();
		unmap_object(result.file);
		close(result.file);
		Some(bytes)
	}
}
