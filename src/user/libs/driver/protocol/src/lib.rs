// THE BRING-UP WIRE BETWEEN DeviceManager AND A DRIVER.
//
// One handshake for every driver in the image, versioned, with the binding's generation on
// everything either side says. Small enough to read in one sitting, which is the point: the tree had
// no handshake at all. `launch_one` called `recv_blocking` and treated any message as success
// whatever it contained, so a driver that sent nothing recognisable was a driver that came up - and
// the text each family sent (`driver.virtio-blk: online (00:02.0)`, a different string per driver)
// was read by nobody. What looked like a per-family dialect was really no protocol.
//
// EVERY FRAME IS HEADER-THEN-PAYLOAD, LITTLE-ENDIAN, WITH THE WIDTHS WRITTEN DOWN. Encoded and
// decoded field by field rather than by casting a `repr(C)` struct over the bytes: a struct with a
// `u64` in it has alignment padding, and a wire format that depends on where a compiler puts padding
// is a wire format that is one target away from disagreeing with itself.

#![no_std]

// One constant, on every frame. A frame without it is not a frame - which is the first thing a
// reader can say about hostile input, and the cheapest.
pub const MAGIC: u32 = 0x5744_5250;

// THE PROTOCOL VERSION, AND THERE IS EXACTLY ONE OF THEM.
//
// It appears in the frame header AND in the ELF note every driver carries, and both come from here.
// Two numbers that could disagree would make "the handshake confirms what the note claimed" a check
// of nothing. Adding an opcode, or changing any payload below, bumps this.
pub const VERSION: u16 = 1;

// magic(4) + version(2) + opcode(2) + generation(8) + payload_len(4).
pub const HEADER_LEN: usize = 20;

// The largest payload any opcode defines, which is `BIND`'s. A frame declaring more is refused
// before anything is read.
pub const MAX_PAYLOAD: usize = core::mem::size_of::<abi::DeviceInfo>() + 2;

// The most providers a driver may offer during ONE handshake.
//
// "Any number" is not a bound, and a driver is a separate process that may be wrong or malicious. It
// cannot be the registry's per-entry count either: that is a later milestone's, and this one has to
// be implementable before it. So the bound is protocol-wide here and narrowed per entry later; an
// offer past it is refused with its handle closed rather than accumulated.
pub const MAX_INITIAL_OFFERS: usize = 4;

// The ELF note every driver carries, read from the STAGED artifact before the device is claimed.
//
// Refusing after the claim would mean taking a device back from something that should never have
// held it. The note's contents are `{MAGIC, VERSION}` - the same two constants above, so a driver
// cannot declare one version and speak another without the handshake catching it.
pub const NOTE_NAME: &[u8] = b"LiberDriver\0";
pub const NOTE_TYPE: u32 = 1;

// The note's bytes, in the ELF note layout: namesz, descsz, type, the name, then the descriptor.
//
// Both lengths are already multiples of four, so there is no padding to get wrong - which is why
// the name carries its own terminator and is exactly twelve bytes.
pub const NOTE_LEN: usize = 12 + 12 + 8;

const fn note_bytes() -> [u8; NOTE_LEN] {
	let mut out = [0u8; NOTE_LEN];
	let namesz = (NOTE_NAME.len() as u32).to_le_bytes();
	let descsz = 8u32.to_le_bytes();
	let ntype = NOTE_TYPE.to_le_bytes();
	let magic = MAGIC.to_le_bytes();
	let version = VERSION.to_le_bytes();
	let mut i = 0;
	while i < 4 {
		out[i] = namesz[i];
		out[4 + i] = descsz[i];
		out[8 + i] = ntype[i];
		out[24 + i] = magic[i];
		i += 1;
	}
	let mut j = 0;
	while j < 12 {
		out[12 + j] = NOTE_NAME[j];
		j += 1;
	}
	out[28] = version[0];
	out[29] = version[1];
	out
}

// THE NOTE EVERY DRIVER CARRIES, AND IT IS THE ONE THING THAT MAKES THE VERSION KNOWABLE BEFORE THE
// CLAIM. Refusing a driver AFTER the claim would mean taking a device back from something that
// should never have held it.
//
// A SECTION THIS BUILD KEEPS, which is the half of this that was checked rather than assumed. All
// three linker scripts end with `/DISCARD/ : { *(.eh_frame*) *(.note .note.*) }`, and `mkpackages`
// then runs `llvm-strip --strip-all` over what is left - so a note emitted into an ordinary
// `.note.*` section would be gone twice over, and the failure would be SILENT: every driver would
// read as having no note, which is indistinguishable from a driver that declares no version. So the
// section has a name the discard does not match, it is `KEEP`-ed in all three scripts, and it lands
// inside `.rodata`, which is `SHF_ALLOC` and therefore survives `--strip-all`.
//
// THE DRIVER EMITS IT, FROM THE SAME CONSTANT THE RUNTIME SPEAKS. A note written by the packager
// would be the packager's opinion of the driver's version rather than the driver's own, and the two
// could differ for exactly the reason this milestone exists: nothing checks them against each other.
#[used]
#[unsafe(link_section = ".liberdrv.note")]
pub static PROTOCOL_NOTE: [u8; NOTE_LEN] = note_bytes();

// The version the note in THIS binary declares, read back out of the note's own bytes.
//
// Not `VERSION`. The point is to read what was actually emitted: a build that dropped the note, or
// emitted a different one, must not be able to answer this correctly from the constant it should
// have used. Referencing the static here is also what keeps its object file linked in at all.
pub fn declared_version() -> u16 {
	u16::from_le_bytes([PROTOCOL_NOTE[28], PROTOCOL_NOTE[29]])
}

// THE VERSION A STAGED ARTIFACT DECLARES, read out of its bytes before it is launched.
//
// `declared_version` above answers for the RUNNING binary, which is why it could never be the
// pre-claim check the note exists for: `common::handshake` calls it after the process has already
// been spawned and the device already claimed. This reads an arbitrary ELF slice - the one
// DeviceManager is about to launch - so the refusal can happen while refusing still costs nothing.
//
// LOCATED BY THE NOTE'S OWN FIXED PREFIX rather than by walking sections. The first 28 bytes are
// constant for every driver this build produces - namesz, descsz, type, the name and the magic - and
// the magic is in the note precisely to make it findable. A section walk would be a second ELF parser
// on a hostile input path to answer a question a memchr already answers.
//
// `None` means no note: an artifact this build did not produce, or one whose note was stripped. That
// is not the same as a version mismatch and the caller says so.
pub fn declared_version_in(elf: &[u8]) -> Option<u16> {
	let prefix = &PROTOCOL_NOTE[..28];
	if elf.len() < NOTE_LEN {
		return None;
	}
	// A CHEAP FIRST BYTE, THEN THE COMPARISON. A 28-byte slice compare at every offset of a driver
	// image is a call per byte of the artifact, and DeviceManager runs this before the claim on the
	// boot path: measured on the shipping image it cost enough of the bind window that the driver's
	// `READY` arrived after the manager had stopped waiting, and a working driver was reported as a
	// handshake timeout. The note's first byte is `namesz` - 12 - so most offsets are rejected by one
	// comparison.
	let first = prefix[0];
	for at in 0..=elf.len() - NOTE_LEN {
		if elf[at] == first && &elf[at..at + 28] == prefix {
			return Some(u16::from_le_bytes([elf[at + 28], elf[at + 29]]));
		}
	}
	None
}

// What one side is saying. Five, and each has ONE direction.
//
// `RESOURCE` and `OFFER` cannot be one opcode however similar they look: they travel in opposite
// directions, and with one handle per frame the manager needs to send more than one capability.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Opcode {
	// manager -> driver, once, first. Names the device and carries no handle.
	Bind = 1,
	// manager -> driver, `resource_count` times. One capability each.
	Resource = 2,
	// driver -> manager, up to `MAX_INITIAL_OFFERS` times. One capability each, held UNPUBLISHED
	// until `Ready` and closed on `Failed`, so a driver that dies half way through announcing
	// itself announces nothing.
	Offer = 3,
	// driver -> manager, terminal. Empty.
	Ready = 4,
	// driver -> manager, terminal. One `DriverFailureCode`.
	Failed = 5,
	// driver -> manager, AFTER the handshake. One publisher-local token: the provider this driver
	// published under that token is going away. Not terminal - the driver stays bound and its other
	// publications stay published.
	//
	// It names the TOKEN and not the manager's identity, because a driver never sees one. That is
	// the same rule the identity itself is minted under, applied to the other direction.
	Withdraw = 6,
	// manager -> driver, after the handshake. One sequence number. "Are you making progress on your
	// CONTROL path?" - which is a different question from whether the device is busy, and a driver
	// may not pet its watchdog through an unrelated child.
	Ping = 7,
	// manager -> driver. "Stop, and mean all of it": take no new work, finish or explicitly abandon
	// what was accepted, flush, quieten the device, and answer `STOPPED`.
	//
	// ONE ROUND TRIP, NOT TWO. A separate `QUIESCE` followed by a `STOP` is two crossings for one
	// transition and the second has nothing left to do.
	Stop = 9,
	// driver -> manager, terminal for the binding: everything it accepted is finished or abandoned
	// and its device is quiet.
	Stopped = 10,
	// manager -> driver, after the handshake. ONE CAPABILITY: a server endpoint for the provider
	// this driver published under the token in the payload, minted by the MANAGER and handed over
	// for the driver to serve.
	//
	// A CONNECTION PER CONSUMER, WHICH IS WHY THIS EXISTS. A provider was one channel, transferred
	// once, and `Catalogue::take` moved it to the first consumer that asked - so a second subscriber
	// could see the provider as metadata and had no way to reach it, and handing it the SAME channel
	// would be two consumers competing over one reply queue rather than two connections.
	//
	// THE MANAGER MINTS THE PAIR, not the driver, and that is the whole reason this needs no reply.
	// A driver answering with a handle would be a second round trip and a second thing to fail
	// half-way; the manager already transfers capabilities TO drivers for every resource a bind
	// hands over, so this is that mechanism and not a new one. The driver adds the endpoint to what
	// it serves; the manager keeps the client end for whoever asked.
	Connect = 11,
	// driver -> manager, the answer, echoing the sequence it was asked with.
	//
	// THE SEQUENCE IS WHY THIS IS NOT `rt::heartbeat`. That one counts ANY message as a pong -
	// `matches!(try_recv(..), Polled::Message { .. })` - so a driver emitting unrelated traffic
	// reads as responsive, and a busy driver and a wedged one look the same. An answer that does not
	// echo the number it was asked with is not an answer to this question.
	Pong = 8,
}

impl Opcode {
	// An unknown opcode is REFUSED rather than accepted as "some message arrived", which is the
	// whole of what happens today.
	pub fn from_u16(value: u16) -> Option<Self> {
		match value {
			1 => Some(Opcode::Bind),
			2 => Some(Opcode::Resource),
			3 => Some(Opcode::Offer),
			4 => Some(Opcode::Ready),
			5 => Some(Opcode::Failed),
			6 => Some(Opcode::Withdraw),
			7 => Some(Opcode::Ping),
			8 => Some(Opcode::Pong),
			9 => Some(Opcode::Stop),
			10 => Some(Opcode::Stopped),
			11 => Some(Opcode::Connect),
			_ => None,
		}
	}

	// How many capability handles a frame with this opcode must carry - exactly, not at most.
	//
	// THE COUNT IS CHECKED AGAINST THIS AND A FRAME CARRYING THE WRONG NUMBER IS REFUSED WITH EVERY
	// HANDLE IT ARRIVED WITH CLOSED. The channel does not enforce one handle per frame; this
	// protocol imposes it. `sys_channel_send_caps` moves a whole list and the ordinary receive takes
	// the first and drops the rest, so a reader that assumed one and used the plain receive would
	// silently discard whatever a driver attached beyond it - capabilities gone, nobody told.
	pub fn handle_count(self) -> usize {
		match self {
			Opcode::Bind | Opcode::Ready | Opcode::Failed | Opcode::Withdraw | Opcode::Ping | Opcode::Pong | Opcode::Stop | Opcode::Stopped => 0,
			Opcode::Resource | Opcode::Offer | Opcode::Connect => 1,
		}
	}

	// Whether this opcode ends the handshake. A second terminal frame, or an offer after one, is
	// refused.
	pub fn is_terminal(self) -> bool {
		matches!(self, Opcode::Ready | Opcode::Failed)
	}

	// Whether this opcode ends the BINDING, which is a different question from ending the handshake.
	// `STOPPED` is the only one: a driver that has said it has nothing left in flight and a quiet
	// device has finished being this binding.
	pub fn ends_the_binding(self) -> bool {
		matches!(self, Opcode::Stopped)
	}
}

// What a `RESOURCE` frame is carrying. The set is what `launch_one` already sends, named.
//
// Which of them a given driver gets depends on the driver, so the sequence has no length a driver
// could infer - which is why `BIND` states it. Without both the count and the kind, a driver either
// waits forever for a resource it will never be sent or starts before one it needs has arrived, and
// that is a race no amount of care at the call site removes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResourceKind {
	// The device's own MMIO capability. Arrives without RIGHT_TRANSFER: the driver is the end of
	// the line for it.
	Device = 1,
	// The device's MSI-X interrupt.
	Irq = 2,
	// The raw key sink, for the two drivers that produce keystrokes.
	Keys = 3,
	// A connection that can ask for a reboot and nothing else.
	SysPower = 4,
	// The capability that lets keystrokes reach the console.
	Console = 5,
}

impl ResourceKind {
	pub fn from_u16(value: u16) -> Option<Self> {
		match value {
			1 => Some(ResourceKind::Device),
			2 => Some(ResourceKind::Irq),
			3 => Some(ResourceKind::Keys),
			4 => Some(ResourceKind::SysPower),
			5 => Some(ResourceKind::Console),
			_ => None,
		}
	}
}

// WHAT AN `OFFER` IS OFFERING.
//
// NOT A CLOSED SET, and deliberately not validated as one. The vocabulary of provider kinds belongs
// to the milestone that builds typed providers and the dependency graph; this protocol has to be
// implementable before that one exists, so what it does here is carry the number and let the manager
// route on it. An unknown kind is the manager's to refuse, not the frame decoder's.
//
// These are what today's drivers publish, named so the manager and the drivers agree on the numbers
// rather than each writing its own.
pub mod provider {
	pub const BLOCK: u16 = 1;
	pub const NET: u16 = 2;
	pub const DISPLAY: u16 = 3;
	pub const AUDIO: u16 = 4;
	pub const INPUT: u16 = 5;
	pub const USB_BUS: u16 = 6;
	pub const POINTER: u16 = 7;
	pub const CONSOLE_BYTES: u16 = 8;
}

// WHAT A DRIVER CAN HONESTLY KNOW ABOUT ITSELF.
//
// A closed set, and deliberately NOT the manager's own failure vocabulary. A driver is hostile input
// by this protocol's own rule, and reusing the manager's causes would let one declare
// `iommu-required` or `teardown-unconfirmed` - things only the manager can determine, which would
// then be recorded as fact.
//
// The retryable column is READ rather than decided again at each call site: a set given as three
// examples in prose is a set every implementer closes differently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DriverFailureCode {
	// What it was handed does not work, and a second attempt hands it the same thing.
	ResourceUnusable = 1,
	// The part may yet come up. This is the case a rebind exists for.
	DeviceNotResponding = 2,
	// A transient shortage, and the backoff is what it costs.
	OutOfMemory = 3,
	// It read the device and will not drive it. Nothing changes that.
	UnsupportedDevice = 4,
	// The driver does not know what went wrong, so nothing says a second try differs.
	InternalError = 5,
}

impl DriverFailureCode {
	// What the driver said, for a reader. Deliberately not `Debug`: these are read by people, in a
	// boot log, beside the binding they are about.
	pub fn name(self) -> &'static [u8] {
		match self {
			DriverFailureCode::ResourceUnusable => b"the driver says what it was handed does not work",
			DriverFailureCode::DeviceNotResponding => b"the driver says the device is not responding",
			DriverFailureCode::OutOfMemory => b"the driver says it is out of memory",
			DriverFailureCode::UnsupportedDevice => b"the driver read the device and will not drive it",
			DriverFailureCode::InternalError => b"the driver does not know what went wrong",
		}
	}

	pub fn from_u16(value: u16) -> Option<Self> {
		match value {
			1 => Some(DriverFailureCode::ResourceUnusable),
			2 => Some(DriverFailureCode::DeviceNotResponding),
			3 => Some(DriverFailureCode::OutOfMemory),
			4 => Some(DriverFailureCode::UnsupportedDevice),
			5 => Some(DriverFailureCode::InternalError),
			_ => None,
		}
	}

	pub fn retryable(self) -> bool {
		matches!(self, DriverFailureCode::DeviceNotResponding | DriverFailureCode::OutOfMemory)
	}
}

// Why a frame was refused. Each of these is a case a test names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameError {
	// Fewer bytes than a header, or fewer than the header says the payload has.
	TooShort,
	// The first four bytes are not `MAGIC`. Not a frame at all.
	NotAFrame,
	// A version this build does not implement.
	Version(u16),
	// A number that is not one of the five.
	UnknownOpcode(u16),
	// A `payload_len` past `MAX_PAYLOAD`, refused before anything is read.
	PayloadTooLong(u32),
	// The payload is not the size this opcode's shape requires.
	PayloadShape,
	// The frame carried a number of handles this opcode does not define.
	HandleCount { expected: usize, found: usize },
	// A field inside the payload is not one of its defined values.
	UnknownValue(u16),
	// More bytes than the header declares. `payload_len` IS the number of bytes after the header, so
	// a frame with anything past them is not a frame this protocol defines.
	TrailingBytes { declared: u32, received: usize },
}

// One frame's header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
	pub version: u16,
	pub opcode: Opcode,
	// P02M0098's claim generation, on every frame in both directions. A frame stamped with a
	// generation that is no longer current is dropped and its handles closed, so a message from a
	// process the manager has already replaced cannot be mistaken for the replacement's.
	pub generation: u64,
	pub payload_len: u32,
}

impl Header {
	pub fn encode(&self) -> [u8; HEADER_LEN] {
		let mut out = [0u8; HEADER_LEN];
		out[0..4].copy_from_slice(&MAGIC.to_le_bytes());
		out[4..6].copy_from_slice(&self.version.to_le_bytes());
		out[6..8].copy_from_slice(&(self.opcode as u16).to_le_bytes());
		out[8..16].copy_from_slice(&self.generation.to_le_bytes());
		out[16..20].copy_from_slice(&self.payload_len.to_le_bytes());
		out
	}

	// EVERY REFUSAL HAPPENS BEFORE THE THING IT WOULD PROTECT IS USED, which is the order this reads
	// in: not a frame, then not this version, then not an opcode, then a length that could not be
	// one of ours - and only then is the payload looked at.
	pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
		if bytes.len() < HEADER_LEN {
			return Err(FrameError::TooShort);
		}
		let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
		if magic != MAGIC {
			return Err(FrameError::NotAFrame);
		}
		let version = u16::from_le_bytes([bytes[4], bytes[5]]);
		if version != VERSION {
			return Err(FrameError::Version(version));
		}
		let raw_opcode = u16::from_le_bytes([bytes[6], bytes[7]]);
		let Some(opcode) = Opcode::from_u16(raw_opcode) else {
			return Err(FrameError::UnknownOpcode(raw_opcode));
		};
		let generation = u64::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]]);
		let payload_len = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
		if payload_len as usize > MAX_PAYLOAD {
			return Err(FrameError::PayloadTooLong(payload_len));
		}
		if bytes.len() < HEADER_LEN + payload_len as usize {
			return Err(FrameError::TooShort);
		}
		// AND NOT MORE THAN IT DECLARES EITHER. This was a lower bound alone, so a `READY` declaring
		// a zero-byte payload in a message carrying one extra byte decoded cleanly, `payload()`
		// returned the empty declared prefix and the remainder was silently dropped - a malformed
		// length accepted, which is the one thing a length field is checked for. Both receive paths
		// hand this the exact message they received, so the bound is an equality.
		if bytes.len() > HEADER_LEN + payload_len as usize {
			return Err(FrameError::TrailingBytes { declared: payload_len, received: bytes.len() - HEADER_LEN });
		}
		Ok(Header { version, opcode, generation, payload_len })
	}

	// The payload of a frame whose header this is, out of the same buffer.
	pub fn payload<'a>(&self, bytes: &'a [u8]) -> &'a [u8] {
		&bytes[HEADER_LEN..HEADER_LEN + self.payload_len as usize]
	}

	// Whether the handles that arrived with this frame are the number its opcode defines. Exactly,
	// not at most - see `Opcode::handle_count`.
	pub fn check_handles(&self, found: usize) -> Result<(), FrameError> {
		let expected = self.opcode.handle_count();
		if found == expected { Ok(()) } else { Err(FrameError::HandleCount { expected, found }) }
	}
}

// ------------------------------------------------------------------ payloads

// `BIND`: the `DeviceInfo` the manager already sends, plus its own count of the resource list it is
// about to send.
//
// The count is a promise the manager makes about what it is ABOUT TO SEND, which is the only kind it
// can keep: the registry entry has no resource list to read one from.
pub const BIND_LEN: usize = core::mem::size_of::<abi::DeviceInfo>() + 2;

pub fn encode_bind(info: &abi::DeviceInfo, resource_count: u16, out: &mut [u8]) -> usize {
	let size = core::mem::size_of::<abi::DeviceInfo>();
	// SAFETY: `DeviceInfo` is `repr(C)` and plain data - the same bytes `SYS_DEVICE_INFO` copies out.
	let info_bytes = unsafe { core::slice::from_raw_parts(info as *const abi::DeviceInfo as *const u8, size) };
	out[..size].copy_from_slice(info_bytes);
	out[size..size + 2].copy_from_slice(&resource_count.to_le_bytes());
	BIND_LEN
}

pub fn decode_bind(payload: &[u8]) -> Result<(abi::DeviceInfo, u16), FrameError> {
	if payload.len() != BIND_LEN {
		return Err(FrameError::PayloadShape);
	}
	let size = core::mem::size_of::<abi::DeviceInfo>();
	let mut info = abi::DeviceInfo::default();
	// SAFETY: as above, and the length is exactly the struct's.
	unsafe { core::ptr::copy_nonoverlapping(payload.as_ptr(), &mut info as *mut abi::DeviceInfo as *mut u8, size) };
	let count = u16::from_le_bytes([payload[size], payload[size + 1]]);
	Ok((info, count))
}

// `RESOURCE`, `OFFER` and `FAILED` all carry exactly one `u16`, and each validates it against its
// own closed set. The shared shape is written once; what the number MEANS is not.
pub const U16_PAYLOAD_LEN: usize = 2;

pub fn encode_u16(value: u16, out: &mut [u8]) -> usize {
	out[..2].copy_from_slice(&value.to_le_bytes());
	U16_PAYLOAD_LEN
}

fn decode_u16(payload: &[u8]) -> Result<u16, FrameError> {
	if payload.len() != U16_PAYLOAD_LEN {
		return Err(FrameError::PayloadShape);
	}
	Ok(u16::from_le_bytes([payload[0], payload[1]]))
}

// A `PING` or `PONG` carries a sequence and nothing else.
//
// NOT THE GENERATION: P02M0161 put that in the frame HEADER, on every frame in both directions, so a
// second copy in the body is a second thing that can disagree with the first.
pub const SEQUENCE_PAYLOAD_LEN: usize = 4;

pub fn encode_sequence(sequence: u32, out: &mut [u8]) -> usize {
	out[..4].copy_from_slice(&sequence.to_le_bytes());
	SEQUENCE_PAYLOAD_LEN
}

pub fn decode_sequence(payload: &[u8]) -> Result<u32, FrameError> {
	if payload.len() != SEQUENCE_PAYLOAD_LEN {
		return Err(FrameError::PayloadShape);
	}
	Ok(u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]))
}

// The longest a driver may be given to answer a `PING`, in monotonic ticks.
//
// NOT A NEW NUMBER. ServiceManager already carries a justified one - 100 ticks, about a second at
// the 100-ticks-per-second monotonic clock, with an operator override. A driver deadline is the same
// question about a different subject, so it takes the same ceiling rather than a second constant
// that drifts from it.
//
// AND THE LOWER BOUND IS NOT PEDANTRY: `wait_any` reads a deadline of 0 as NO TIMEOUT, so an entry
// declaring zero would not be supervised strictly - it would not be supervised at all, and would
// look like the most responsive driver in the machine.
pub const MAX_HEARTBEAT_DEADLINE: u32 = 100;

// How often to ping, given the deadline an entry declared: half of it, ROUNDED UP.
//
// Half of a deadline of 1 is 0, and a period of zero is either a busy loop or - through the same
// `wait_any` rule as above - no timeout at all. Both are the failures this bound exists to prevent,
// reached by arithmetic. Rounding up means an entry asking for a longer deadline is pinged LESS
// often and never the other way round, and a driver always has one whole period to answer in.
pub fn heartbeat_period(deadline: u32) -> u32 {
	deadline.div_ceil(2).max(1)
}

// The publisher-local token a `WITHDRAW` names.
pub fn decode_withdraw(payload: &[u8]) -> Result<u16, FrameError> {
	decode_u16(payload)
}

pub fn decode_resource(payload: &[u8]) -> Result<ResourceKind, FrameError> {
	let raw = decode_u16(payload)?;
	ResourceKind::from_u16(raw).ok_or(FrameError::UnknownValue(raw))
}

// An offer's provider kind is NOT validated against a closed set here: the set of provider kinds is
// a later milestone's and does not exist yet. What is bounded here is how many offers one handshake
// may carry, which is this protocol's business and `MAX_INITIAL_OFFERS`.
// An offer is a KIND and a publisher-local TOKEN.
//
// The kind says what the provider is; the token says which of this driver's publications it is. A
// driver publishing two providers of one kind has to be able to say later which of them is going
// away, and it cannot name an identity it never sees - the manager assigns those, precisely so that
// a compromised driver cannot advertise itself as the system disk. The token is the driver's own
// and unique only within that driver, which is enough to name its own publications and useless for
// naming anybody else's.
pub const OFFER_PAYLOAD_LEN: usize = 4;

pub fn encode_offer(kind: u16, token: u16, out: &mut [u8]) -> usize {
	out[..2].copy_from_slice(&kind.to_le_bytes());
	out[2..4].copy_from_slice(&token.to_le_bytes());
	OFFER_PAYLOAD_LEN
}

pub fn decode_offer(payload: &[u8]) -> Result<(u16, u16), FrameError> {
	if payload.len() != OFFER_PAYLOAD_LEN {
		return Err(FrameError::PayloadShape);
	}
	Ok((u16::from_le_bytes([payload[0], payload[1]]), u16::from_le_bytes([payload[2], payload[3]])))
}

pub fn decode_failed(payload: &[u8]) -> Result<DriverFailureCode, FrameError> {
	let raw = decode_u16(payload)?;
	DriverFailureCode::from_u16(raw).ok_or(FrameError::UnknownValue(raw))
}

// `READY` carries nothing, and a `READY` carrying something is not a `READY`.
pub fn decode_ready(payload: &[u8]) -> Result<(), FrameError> {
	if payload.is_empty() { Ok(()) } else { Err(FrameError::PayloadShape) }
}

#[cfg(test)]
mod tests;
