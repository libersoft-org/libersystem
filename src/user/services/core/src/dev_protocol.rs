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

use alloc::string::String;
use alloc::vec::Vec;
use bootproto::compat;
use bootproto::elf::Elf;
use ipc_client::ChannelTransport;
use proto::generated::liber::security::v1 as security;
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

// The bounds a launch is held to. A launched program is the one thing here that produces
// output at its own pace, so what it may accumulate before anyone reads it has to be a
// number: past this the oldest is dropped and the reader is told, which keeps a chatty
// program from being a way to grow the agent without limit.
pub const MAX_LAUNCH_NAME: usize = 64;
pub const MAX_LAUNCH_ARGS: usize = 512;
pub const MAX_LAUNCH_OUTPUT: usize = 65536;

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
pub const OP_LAUNCH: u8 = 0x30;
pub const OP_LAUNCH_ACK: u8 = 0x31;
pub const OP_LAUNCH_OUTPUT: u8 = 0x32;
pub const OP_LAUNCH_BYTES: u8 = 0x33;
pub const OP_LAUNCH_STOP: u8 = 0x34;
pub const OP_LAUNCH_STOP_ACK: u8 = 0x35;
pub const OP_TERM_INPUT: u8 = 0x20;
pub const OP_TERM_ACK: u8 = 0x21;
pub const OP_RESET: u8 = 0x22;
pub const OP_RESET_ACK: u8 = 0x23;
pub const OP_RESTART: u8 = 0x24;
pub const OP_RESTART_ACK: u8 = 0x25;
// The guest's own account of system memory, so a host can ask whether a scenario gave back
// what it took. A scenario's own launch runs in a Domain the kernel frees with the process, so
// what this catches is the leak that outlives it: memory a service is still holding on behalf
// of a run that ended, which nothing else in a teardown can see.
pub const OP_MEM_STATS: u8 = 0x26;
pub const OP_MEM_STATS_REPLY: u8 = 0x27;
// The scenario fixture area: files a scenario needs on the volume to work against, written
// under a reserved name prefix and removed as a set. Scenarios share one persistent guest, so
// a fixture that outlived its run would be inherited by the next one - these exist to be
// deleted, and the run that made them is the run that answers for them.
pub const OP_FIXTURE_PUT: u8 = 0x28;
pub const OP_FIXTURE_ACK: u8 = 0x29;
pub const OP_FIXTURE_CLEAR: u8 = 0x2a;
pub const OP_FIXTURE_CLEAR_ACK: u8 = 0x2b;
pub const OP_ERROR: u8 = 0xff;

// Statuses. Every rejection names one of these, so a failure is explained by the frame that
// caused it rather than by a timeout on the host.
// The fixture area's bounds. Both are refusals rather than guidance: a scenario that would
// exceed them is told so when it asks, because the alternative is a persistent guest whose
// volume fills up over a day of runs and fails something unrelated.
pub const FIXTURE_PREFIX: &str = "vol://system/fixture-";
pub const MAX_FIXTURES: usize = 32;
pub const MAX_FIXTURE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_FIXTURE_NAME: usize = 48;

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
// No launcher is wired, so nothing can be launched through the manifest.
pub const ST_NO_LAUNCHER: u16 = 23;
// The launcher refused: no such component, or its manifest does not permit it.
pub const ST_LAUNCH_REFUSED: u16 = 24;
// Output was asked for with no launch to read it from.
pub const ST_NO_LAUNCH: u16 = 25;
// The fixture area is full, or the name is not one a scenario may write.
pub const ST_FIXTURE_FULL: u16 = 26;
pub const ST_FIXTURE_NAME: u16 = 27;
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

// One launched program: the channel its output arrives on, what has arrived and not yet been
// read, and whether it has finished. A launch is deliberately singular - a scenario drives
// one program at a time, and one is what can be reasoned about without correlating output to
// a launch id on every frame.
struct Launch {
	output: u64,
	// The launched program itself, kept so it can be stopped. A scenario that starts an
	// interactive program has to be able to end it: without this the only way out is the
	// program deciding to leave, and a run that fails halfway leaves it holding the terminal
	// for every run after it.
	task: u64,
	buffered: Vec<u8>,
	// Set when the buffer overran and the oldest output was dropped, so a reader is told
	// rather than shown a gap it cannot see.
	truncated: bool,
	// The program's output channel closed, which is how a launched program reports it is
	// finished: nothing else can write there once it is gone.
	exited: bool,
}

// Both handles go when the launch does. A launch is replaced by the next one and dropped by a
// reset, and neither is a place to leak the process handle and the channel it wrote to.
impl Drop for Launch {
	fn drop(&mut self) {
		unsafe {
			if self.output != 0 {
				close(self.output);
			}
			if self.task != 0 {
				close(self.task);
			}
		}
	}
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
	//
	// It is handed down rather than drawn here, because this session does not live as long as
	// the boot does: an agent can be replaced without the guest restarting, and a value drawn
	// per agent would tell every tool the guest had rebooted when it had not.
	boot_nonce: [u8; 8],
	// The volume client the installed artifacts are read through, to decide whether a
	// candidate may stand in for the one it shadows. Zero when none was wired, which leaves
	// every verdict unknown rather than silently claiming compatibility.
	storage: u64,
	// The PermissionManager client a launch goes through. Zero until it is delivered, which
	// happens after this session exists: PermissionManager starts later in the boot than the
	// agent does. Launching through it rather than around it is the point - it remains the
	// only ordinary launcher, and a scenario gets exactly the component's manifest grants.
	launcher: u64,
	// The one launch in flight, if any.
	launch: Option<Launch>,
	// ProcessService's end of the resolution channel: a launch asks whether the registry holds
	// a generation of an artifact before reading the installed one. Zero until it is
	// delivered, and zero forever on a boot with no agent - in which case ProcessService simply
	// never gets an answer and reads the volume, which is the shipping behaviour.
	registry: u64,
	// Set by a restart request and read by the program above, which is the only thing that can
	// act on it: the protocol can decide that this agent should end, but ending it is not
	// something a state machine behind a `Sink` gets to do.
	restart: bool,
	candidate: Option<Candidate>,
	artifacts: Vec<Artifact>,
	// Live registry content in bytes, tracked rather than recomputed so the budget can be
	// checked before a reservation instead of after an allocation.
	registry_bytes: usize,
	// The fixture files this session has written, by bare name, and what they cost. Held so a
	// clear removes exactly what was made rather than whatever currently matches the prefix -
	// the difference matters if anything else ever writes there.
	fixtures: Vec<String>,
	fixture_bytes: usize,
}

impl Session {
	pub fn new(storage: u64, boot_nonce: [u8; 8]) -> Session {
		let mut profile: [u8; 32] = [0u8; 32];
		let len: usize = unsafe { boot_profile(&mut profile) };
		Session { boot_nonce, handshake: false, high_request: 0, next_generation: 1, registry_allowed: &profile[..len] == REGISTRY_PROFILE, storage, launcher: 0, launch: None, registry: 0, restart: false, candidate: None, artifacts: Vec::new(), registry_bytes: 0, fixtures: Vec::new(), fixture_bytes: 0 }
	}

	pub fn is_open(&self) -> bool {
		self.handshake
	}

	// Whether a host asked this agent to end so a fresh one takes its place. Read after every
	// parse, once the reply is already queued behind this session.
	pub fn restart_requested(&self) -> bool {
		self.restart
	}

	// Take the launcher, delivered after this session was created.
	pub fn set_launcher(&mut self, launcher: u64) {
		self.launcher = launcher;
	}

	// Take the resolution channel, delivered the same way, and announce on it. The
	// announcement is what tells ProcessService the channel has someone on it: it holds this
	// end from boot, long before an agent exists, and must not ask questions of a peer that
	// may never arrive.
	pub fn set_registry(&mut self, registry: u64) {
		self.registry = registry;
		unsafe { send_blocking(registry, services::REGISTRY_ANNOUNCEMENT, 0) };
	}

	pub fn registry_channel(&self) -> u64 {
		self.registry
	}

	// Answer one resolution query: a launch asks for an artifact by name, and gets the newest
	// generation's bytes or an empty reply.
	//
	// The answer is only ever the newest generation of exactly that name. There is no way to
	// ask for a different artifact than the one being loaded, and nothing here consults a path
	// - the caller has already resolved the manifest, and this only says whether the registry
	// is shadowing what it resolved.
	pub fn answer_resolution(&mut self) {
		let mut buf: [u8; 64] = [0u8; 64];
		let (len, _) = match unsafe { recv_blocking(self.registry, &mut buf) } {
			Received::Message { len, handle } => (len, handle),
			Received::Closed => {
				self.registry = 0;
				return;
			}
		};
		let name: &[u8] = &buf[..len.min(MAX_NAME)];
		let found = self.artifacts.iter().find(|artifact| artifact.name.as_slice() == name).and_then(|artifact| artifact.generations.last());
		let bytes: &[u8] = match found {
			Some(generation) if self.registry_allowed => &generation.bytes,
			_ => &[],
		};
		unsafe { send_blocking(self.registry, bytes, 0) };
	}

	// The channel a launched program's output arrives on, so the serve loop can wait on it
	// alongside everything else rather than polling.
	//
	// Zero once the program has exited, even though the handle is still open and still holds
	// its unread output. A closed channel is permanently ready, so waiting on one spins the
	// loop as fast as the scheduler will allow - which under a cooperative scheduler starves
	// every other thread in the guest. The output stays readable; only the wait stops.
	pub fn launch_channel(&self) -> u64 {
		match &self.launch {
			Some(launch) if !launch.exited => launch.output,
			_ => 0,
		}
	}

	// Drain whatever the launched program has printed into its bounded buffer. Called by the
	// serve loop when that channel wakes; the host reads it with OP_LAUNCH_OUTPUT.
	pub fn drain_launch(&mut self) {
		let Some(launch) = &mut self.launch else { return };
		loop {
			// Distinguish "nothing queued" from "nothing queued and the peer is gone". The
			// second is how a launched program reports that it finished - its end of this
			// channel goes with it - so treating both as "no work" would leave every launch
			// looking like it never ended.
			let pending: i64 = unsafe { channel_peek(launch.output) };
			if pending == ERR_PEER_CLOSED {
				launch.exited = true;
				return;
			}
			if pending < 0 {
				return;
			}
			match unsafe { recv_vec_blocking(launch.output) } {
				ReceivedVec::Message { bytes, .. } => {
					launch.buffered.extend_from_slice(&bytes);
					// The newest output is what a scenario is waiting on, so an overrun drops
					// the oldest and says so rather than refusing to record any more.
					if launch.buffered.len() > MAX_LAUNCH_OUTPUT {
						let excess: usize = launch.buffered.len() - MAX_LAUNCH_OUTPUT;
						launch.buffered.drain(..excess);
						launch.truncated = true;
					}
				}
				ReceivedVec::Closed => {
					launch.exited = true;
					return;
				}
				// The launch is over either way, but its output was NOT delivered in full -
				// mark it truncated so the buffered text is not read as everything it printed.
				ReceivedVec::Failed => {
					launch.truncated = true;
					launch.exited = true;
					return;
				}
			}
		}
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
		let session_scoped: bool = matches!(opcode, OP_PING | OP_PUB_BEGIN | OP_GEN_LIST | OP_ROLLBACK | OP_TERM_INPUT | OP_RESET | OP_RESTART | OP_LAUNCH | OP_LAUNCH_OUTPUT | OP_LAUNCH_STOP);
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
			OP_LAUNCH => self.launch(request, payload, sink),
			OP_LAUNCH_OUTPUT => self.launch_output(request, sink),
			OP_LAUNCH_STOP => self.launch_stop(request, sink),
			OP_TERM_INPUT => terminal_input(request, payload, sink),
			OP_RESET => self.reset(request, sink),
			OP_MEM_STATS => memory_stats(request, sink),
			OP_FIXTURE_PUT => self.fixture_put(request, payload, sink),
			OP_FIXTURE_CLEAR => self.fixture_clear(request, sink),
			OP_RESTART => self.request_restart(request, sink),
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
	// Write one fixture file for the running scenario. The name is a bare name, never a path:
	// it is joined to a reserved prefix here, so a scenario cannot reach anywhere else on the
	// volume no matter what it asks for, and the reserved prefix is what makes a clear able to
	// find them again.
	//
	// Request payload: name length u8, the name, then the bytes. Reply payload: the fixture
	// count and the bytes held after this one.
	fn fixture_put(&mut self, request: u32, payload: &[u8], sink: &mut impl Sink) -> bool {
		if payload.is_empty() {
			return sink.send(OP_ERROR, request, 0, ST_MALFORMED, &[]);
		}
		let name_len: usize = payload[0] as usize;
		if name_len == 0 || name_len > MAX_FIXTURE_NAME || payload.len() < 1 + name_len {
			return sink.send(OP_ERROR, request, 0, ST_FIXTURE_NAME, &[]);
		}
		let name: &[u8] = &payload[1..1 + name_len];
		if !name.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')) || name[0] == b'.' {
			return sink.send(OP_ERROR, request, 0, ST_FIXTURE_NAME, &[]);
		}
		let Ok(name) = core::str::from_utf8(name) else {
			return sink.send(OP_ERROR, request, 0, ST_FIXTURE_NAME, &[]);
		};
		let body: &[u8] = &payload[1 + name_len..];
		// A rewrite of a fixture this session already wrote costs only the difference, so a
		// scenario that writes the same file twice is not charged twice for it.
		let known: bool = self.fixtures.iter().any(|held| held == name);
		if !known && self.fixtures.len() >= MAX_FIXTURES {
			return sink.send(OP_ERROR, request, 0, ST_FIXTURE_FULL, &[]);
		}
		if self.fixture_bytes + body.len() > MAX_FIXTURE_BYTES {
			return sink.send(OP_ERROR, request, 0, ST_FIXTURE_FULL, &[]);
		}
		let mut path: String = String::from(FIXTURE_PREFIX);
		path.push_str(name);
		if !unsafe { write_volume_file(self.storage, &path, body) } {
			return sink.send(OP_ERROR, request, 0, ST_NO_SPACE, &[]);
		}
		if !known {
			self.fixtures.push(String::from(name));
		}
		self.fixture_bytes += body.len();
		let mut reply: Vec<u8> = Vec::with_capacity(6);
		reply.extend_from_slice(&(self.fixtures.len() as u16).to_le_bytes());
		reply.extend_from_slice(&(self.fixture_bytes as u32).to_le_bytes());
		sink.send(OP_FIXTURE_ACK, request, 0, ST_OK, &reply)
	}

	// Remove every fixture this session wrote and report what is left. Left is the number that
	// could not be removed, not the number there were: a teardown asks this to find out whether
	// the instance is clean, and "I tried" is not an answer to that question.
	//
	// Reply payload: removed u16, still held u16.
	fn fixture_clear(&mut self, request: u32, sink: &mut impl Sink) -> bool {
		let mut removed: u16 = 0;
		let mut stuck: Vec<String> = Vec::new();
		for name in core::mem::take(&mut self.fixtures) {
			let mut path: String = String::from(FIXTURE_PREFIX);
			path.push_str(&name);
			if unsafe { remove_volume_file(self.storage, &path) } {
				removed += 1;
			} else {
				stuck.push(name);
			}
		}
		let left: u16 = stuck.len() as u16;
		self.fixtures = stuck;
		if self.fixtures.is_empty() {
			self.fixture_bytes = 0;
		}
		let mut reply: Vec<u8> = Vec::with_capacity(4);
		reply.extend_from_slice(&removed.to_le_bytes());
		reply.extend_from_slice(&left.to_le_bytes());
		sink.send(OP_FIXTURE_CLEAR_ACK, request, 0, ST_OK, &reply)
	}

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

	// End this agent so its supervisor starts a fresh one. The acknowledgement is sent first
	// and the flag set after, so the reply is already queued in the driver's channel when the
	// process goes: a message that has left the sender outlives it, and the host gets its
	// answer rather than a disconnect it has to interpret.
	//
	// Everything this agent holds goes with it, the registry included. That is the operation,
	// not a side effect of it: the point of restarting is to get back the state a fresh boot
	// would have without paying for a boot, and a restart that carried the registry across
	// would leave the one thing most likely to be wedged exactly where it was. The reply says
	// how many generations went, so a host that meant `reset` learns it did something larger.
	fn request_restart(&mut self, request: u32, sink: &mut impl Sink) -> bool {
		let dropped: u16 = self.artifacts.iter().map(|artifact| artifact.generations.len() as u16).sum();
		let sent: bool = sink.send(OP_RESTART_ACK, request, 0, ST_OK, &dropped.to_le_bytes());
		self.restart = true;
		sent
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
// The guest's system memory account: free and total frames, free and total heap. Answered from
// the kernel rather than tracked here, because a figure this is used to compare across a
// scenario has to come from whatever the kernel would say if asked at any other moment.
//
// This is a whole-system reading on purpose, and that is the point rather than a compromise. A
// launched program runs in a Domain of its own that the kernel frees with the process, so a leak
// inside a scenario's own launch cannot survive it and needs no checking. What can survive it is
// a service that took something on the run's behalf and did not give it back - a retained
// MemoryObject, a journal entry, a registry generation - and none of that is visible from inside
// the scope that ended. A system reading sees it, at the cost of also seeing whatever else the
// system did meanwhile, which is why the comparison that uses it needs a tolerance rather than
// an equality.
//
// Reply payload: free_frames u64, total_frames u64, heap_free u64, heap_total u64.
fn memory_stats(request: u32, sink: &mut impl Sink) -> bool {
	let mut stats: MemoryStats = MemoryStats::default();
	if unsafe { rt::memory_stats(&mut stats) } < 0 {
		return sink.send(OP_ERROR, request, 0, ST_MALFORMED, &[]);
	}
	let mut reply: Vec<u8> = Vec::with_capacity(32);
	reply.extend_from_slice(&stats.free_frames.to_le_bytes());
	reply.extend_from_slice(&stats.total_frames.to_le_bytes());
	reply.extend_from_slice(&stats.heap_free.to_le_bytes());
	reply.extend_from_slice(&stats.heap_total.to_le_bytes());
	sink.send(OP_MEM_STATS_REPLY, request, 0, ST_OK, &reply)
}

// The ConsoleInputSource capability this agent was handed at bootstrap, which
// `SYS_CONSOLE_FEED` requires. Typing into a live console on a driven guest is exactly the
// authority that had no capability behind it, so the dev agent carries one like any other
// holder rather than being exempt for being a development tool.
//
// Zero when the boot handed out none, in which case the feed is refused and a scenario that
// tries to type gets ST_TERM_REFUSED - which is what a refusal should look like from the far
// end of the wire.
pub(crate) static CONSOLE_INPUT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn terminal_input(request: u32, payload: &[u8], sink: &mut impl Sink) -> bool {
	if payload.is_empty() {
		return sink.send(OP_ERROR, request, 0, ST_MALFORMED, &[]);
	}
	if payload.len() > MAX_TERM_INPUT {
		return sink.send(OP_ERROR, request, 0, ST_OVERSIZED, &[]);
	}
	let mut accepted: u16 = 0;
	let privilege = CONSOLE_INPUT.load(core::sync::atomic::Ordering::Relaxed);
	for &byte in payload {
		if unsafe { console_feed_serial(privilege, byte) } != 0 {
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
		// The comparison gave up rather than run. It says so, and says how much it spent, because
		// "this needs the cold path" and "this image is too tangled to decide cheaply" are different
		// things for whoever is publishing.
		compat::Reason::TooComplex { visits } => {
			out.extend_from_slice(b"export comparison exceeded its budget after ");
			push_number(&mut out, *visits as usize);
			out.extend_from_slice(b" symbol visits; use the cold path");
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
		// `provider_names`, because this asks whether the NAME is in the closure. `providers` now
		// yields `<name>:<identity-digest>` - the compatibility rule needs both halves and used to
		// throw the digest away - and matching a DT_NEEDED stem against that would never succeed.
		if !identity.provider_names().any(|provider| provider == stem) {
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
// Write `body` to `path` on the volume through the agent's storage client. The buffer the
// write takes is a transferred MemoryObject, and the call consumes it either way, so there is
// nothing to release here on a failure.
unsafe fn write_volume_file(storage: u64, path: &str, body: &[u8]) -> bool {
	unsafe {
		if storage == 0 {
			return false;
		}
		let Some(buffer) = ipc_client::make_buffer(body) else { return false };
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		matches!(client.write(path, &buffer), Some(Ok(())))
	}
}

// Remove `path` from the volume. Only names this session wrote are ever passed here, so a
// removal that fails means the file is still there and the caller is told so.
unsafe fn remove_volume_file(storage: u64, path: &str) -> bool {
	unsafe {
		if storage == 0 {
			return false;
		}
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		matches!(client.remove(path), Some(Ok(())))
	}
}

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

impl Session {
	// Launch a canonical program through PermissionManager, with arguments and a working
	// directory, and take the channel it writes its output to.
	//
	// Through the launcher, never around it: PermissionManager stays the only ordinary
	// launcher and its installed manifest stays the authority for what a component is allowed
	// to reach, so a scenario gets exactly the grants the manifest gives and cannot ask for
	// more. Nothing here is a shell - the name and the arguments are separate typed fields,
	// and the guest never concatenates them into something an interpreter would parse.
	//
	// Payload: name_len u8 | name | args_len u16 | args | cwd_len u16 | cwd.
	fn launch(&mut self, request: u32, payload: &[u8], sink: &mut impl Sink) -> bool {
		if self.launcher == 0 {
			return sink.send(OP_ERROR, request, 0, ST_NO_LAUNCHER, &[]);
		}
		let Some((name, args, cwd)) = parse_launch(payload) else {
			return sink.send(OP_ERROR, request, 0, ST_MALFORMED, &[]);
		};
		// A previous launch is replaced, and its channel released with it: one at a time is
		// the rule, and a scenario that starts another has finished with the first.
		self.launch = None;
		let (ours, theirs): (u64, u64) = match unsafe { channel() } {
			Some(pair) => pair,
			None => return sink.send(OP_ERROR, request, 0, ST_LAUNCH_REFUSED, &[]),
		};
		let started = security::permission::Client::new(ChannelTransport { chan: self.launcher }).run(name, args, cwd, &theirs);
		let (koid, task): (u64, u64) = match started {
			Some(Ok(result)) => (result.info.koid, result.task),
			_ => {
				unsafe { close(ours) };
				return sink.send(OP_ERROR, request, 0, ST_LAUNCH_REFUSED, &[]);
			}
		};
		self.launch = Some(Launch { output: ours, task, buffered: Vec::new(), truncated: false, exited: false });
		sink.send(OP_LAUNCH_ACK, request, 0, ST_OK, &koid.to_le_bytes())
	}

	// Stop the launched program. What it has already printed stays readable, because a
	// scenario tearing down still wants to know what it said before it was ended; what goes is
	// the program. Stopping one that has already finished is not an error - a caller cleaning
	// up cannot know which it is, and reporting that distinction is more useful than refusing.
	//
	// Reply payload: signalled u8.
	fn launch_stop(&mut self, request: u32, sink: &mut impl Sink) -> bool {
		let Some(launch) = &mut self.launch else {
			return sink.send(OP_ERROR, request, 0, ST_NO_LAUNCH, &[]);
		};
		let signalled: bool = !launch.exited && launch.task != 0 && unsafe { signal(launch.task, SIG_KILL) } >= 0;
		if signalled {
			launch.exited = true;
		}
		sink.send(OP_LAUNCH_STOP_ACK, request, 0, ST_OK, &[u8::from(signalled)])
	}

	// Hand over what the launched program has printed since the last read, and say whether it
	// has finished and whether anything was dropped. Reading consumes: a scenario asserts on a
	// stream, and re-reading what it already matched would make every assertion after the
	// first ambiguous.
	//
	// Reply payload: exited u8 | truncated u8 | bytes.
	fn launch_output(&mut self, request: u32, sink: &mut impl Sink) -> bool {
		let Some(launch) = &mut self.launch else {
			return sink.send(OP_ERROR, request, 0, ST_NO_LAUNCH, &[]);
		};
		let take: usize = launch.buffered.len().min(MAX_PAYLOAD - 2);
		let mut reply: Vec<u8> = Vec::with_capacity(take + 2);
		reply.push(u8::from(launch.exited));
		reply.push(u8::from(launch.truncated));
		reply.extend_from_slice(&launch.buffered[..take]);
		launch.buffered.drain(..take);
		launch.truncated = false;
		sink.send(OP_LAUNCH_BYTES, request, 0, ST_OK, &reply)
	}
}

// Split a launch payload into its three typed fields. Every length is checked against its own
// bound before the field is read, so a malformed payload is a refusal rather than a slice
// that happens to land somewhere.
fn parse_launch(payload: &[u8]) -> Option<(&str, &str, &str)> {
	let name_len: usize = *payload.first()? as usize;
	if name_len == 0 || name_len > MAX_LAUNCH_NAME {
		return None;
	}
	let name: &str = core::str::from_utf8(payload.get(1..1 + name_len)?).ok()?;
	let mut at: usize = 1 + name_len;
	let args_len: usize = u16::from_le_bytes([*payload.get(at)?, *payload.get(at + 1)?]) as usize;
	if args_len > MAX_LAUNCH_ARGS {
		return None;
	}
	at += 2;
	let args: &str = core::str::from_utf8(payload.get(at..at + args_len)?).ok()?;
	at += args_len;
	let cwd_len: usize = u16::from_le_bytes([*payload.get(at)?, *payload.get(at + 1)?]) as usize;
	if cwd_len > MAX_LAUNCH_ARGS {
		return None;
	}
	at += 2;
	let cwd: &str = core::str::from_utf8(payload.get(at..at + cwd_len)?).ok()?;
	if at + cwd_len != payload.len() || !name_is_valid(name.as_bytes()) {
		return None;
	}
	Some((name, args, cwd))
}
