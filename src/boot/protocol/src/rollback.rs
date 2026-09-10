// The monotonic boot floor: the highest security generation this machine has accepted, kept in
// firmware NVRAM and compared against every signed release before control is transferred.
//
// WHY A NUMBER AND NOT THE RELEASE STRING. A signature proves who made a release, not when; a
// correctly signed old release is still correctly signed, and an attacker who can replace boot
// media can put one back. The manifest therefore carries a canonical unsigned generation beside its
// human release string, covered by the same signature, and the loader keeps the highest one it has
// ever accepted in two independently validated firmware variables. Below the floor refuses; equal
// boots; above advances the floor - and only after the whole selected artifact set is verified.
//
// SHARED ON PURPOSE. The record's bytes, the classification of what firmware answered, and the
// decision of what to write and in which order are all here, `no_std`, host-tested, and driven
// through one `Firmware` trait: the loader implements it over UEFI runtime services, the host
// fixtures over a mock that can tear a write or refuse a read. The provisioning tool encodes the
// same bytes. A rollback floor whose loader, ceremony and fixtures each kept their own copy of the
// format would hold only by luck.
//
// THE THREAT MODEL IS THE PLAN'S: the attacker controls persistent boot media and not firmware
// NVRAM administration. Deleting the provisioned marker is therefore not defended against - a
// machine whose marker is gone is indistinguishable from one never provisioned, boots, and refuses
// to advance until the ceremony runs again; what it loses is availability of the floor, never its
// integrity, because every artifact it accepts is still signature-checked.

use crate::sha256;

// THE RECORD, BYTE BY BYTE. Sixty-four bytes exactly; every multi-byte field little-endian.
//
//   0    8   magic `LSROLLB1` - not this, not this format, never "an older version"
//   8    4   format version, 1
//   12   4   reserved, zero on write and zero to validate
//   16   32  product identity: SHA-256 of the loader's compiled-in product constant
//   48   8   the accepted floor
//   56   8   commit tag: SHA-256 of bytes 0..56 with the slot's ONE index byte appended, truncated
//            to its first eight bytes - so a write that stops partway fails validation instead of
//            parsing as a plausible lower floor, and slot A's bytes are not a valid slot B
pub const MAGIC: [u8; 8] = *b"LSROLLB1";
pub const FORMAT_VERSION: u32 = 1;
pub const RECORD_LEN: usize = 64;
pub const TAG_LEN: usize = 8;
const TAG_AT: usize = 56;
const FLOOR_AT: usize = 48;
const PRODUCT_AT: usize = 16;
const RESERVED_AT: usize = 12;
const VERSION_AT: usize = 8;

// The provisioned marker: one octet, `0x01`, written once by the ceremony and never by a boot.
pub const MARKER_LEN: usize = 1;
pub const MARKER_VALUE: u8 = 0x01;

// UEFI facts, fixed here so the loader, the ceremony, the mocked firmware and the gate use one set.
pub const ATTRIBUTE_NON_VOLATILE: u32 = 0x0000_0001;
pub const ATTRIBUTE_BOOTSERVICE_ACCESS: u32 = 0x0000_0002;
// NOT `RUNTIME_ACCESS`: nothing after ExitBootServices may read or write this state, which is what
// keeps the running system out of the set of things that can lower the floor. Any other mask is
// INVALID, not merely surprising.
pub const ATTRIBUTES: u32 = ATTRIBUTE_NON_VOLATILE | ATTRIBUTE_BOOTSERVICE_ACCESS;
// The vendor namespace: `4c696265-7253-7973-2d52-6f6c6c626b31`, allocated to this product for these
// variables and never the EFI global one.
pub const VENDOR_GUID_TEXT: &str = "4c696265-7253-7973-2d52-6f6c6c626b31";
pub const VENDOR_GUID_DATA1: u32 = 0x4c69_6265;
pub const VENDOR_GUID_DATA2: u16 = 0x7253;
pub const VENDOR_GUID_DATA3: u16 = 0x7973;
pub const VENDOR_GUID_DATA4: [u8; 8] = [0x2d, 0x52, 0x6f, 0x6c, 0x6c, 0x62, 0x6b, 0x31];
pub const SLOT_A_NAME: &str = "LiberSystemRollbackA";
pub const SLOT_B_NAME: &str = "LiberSystemRollbackB";
pub const MARKER_NAME: &str = "LiberSystemRollbackProvisioned";

// The names as firmware takes them: UTF-16, NUL-terminated.
pub const SLOT_A_NAME_UTF16: [u16; 21] = utf16(SLOT_A_NAME);
pub const SLOT_B_NAME_UTF16: [u16; 21] = utf16(SLOT_B_NAME);
pub const MARKER_NAME_UTF16: [u16; 31] = utf16(MARKER_NAME);

const fn utf16<const N: usize>(name: &str) -> [u16; N] {
	let bytes = name.as_bytes();
	assert!(bytes.len() + 1 == N, "the UTF-16 array is the name plus its terminator");
	let mut out = [0u16; N];
	let mut i = 0;
	while i < bytes.len() {
		assert!(bytes[i].is_ascii(), "a variable name is ASCII");
		out[i] = bytes[i] as u16;
		i += 1;
	}
	out
}

// The two slots. The index byte is the ONLY thing that makes slot A's bytes fail in slot B.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
	A,
	B,
}

impl Slot {
	pub const fn index_byte(self) -> u8 {
		match self {
			Slot::A => 0x00,
			Slot::B => 0x01,
		}
	}

	pub const fn other(self) -> Slot {
		match self {
			Slot::A => Slot::B,
			Slot::B => Slot::A,
		}
	}

	pub const fn name(self) -> &'static str {
		match self {
			Slot::A => SLOT_A_NAME,
			Slot::B => SLOT_B_NAME,
		}
	}

	pub const fn letter(self) -> &'static str {
		match self {
			Slot::A => "A",
			Slot::B => "B",
		}
	}

	pub const fn variable(self) -> Variable {
		match self {
			Slot::A => Variable::SlotA,
			Slot::B => Variable::SlotB,
		}
	}
}

// The three variables, as the firmware layer is asked for them. A typed selector rather than a
// name: a caller that could pass a name could read or write any variable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Variable {
	SlotA,
	SlotB,
	Marker,
}

impl Variable {
	pub const fn name(self) -> &'static str {
		match self {
			Variable::SlotA => SLOT_A_NAME,
			Variable::SlotB => SLOT_B_NAME,
			Variable::Marker => MARKER_NAME,
		}
	}

	pub const fn name_utf16(self) -> &'static [u16] {
		match self {
			Variable::SlotA => &SLOT_A_NAME_UTF16,
			Variable::SlotB => &SLOT_B_NAME_UTF16,
			Variable::Marker => &MARKER_NAME_UTF16,
		}
	}
}

// The product identity a record carries: the digest of the compiled-in product constant, so the
// record's size never depends on the product's name and no fifth copy of the name is made.
pub fn product_identity(product: &[u8]) -> [u8; 32] {
	sha256::digest(product)
}

fn tag(head: &[u8], slot: Slot) -> [u8; TAG_LEN] {
	// The hashed input is exactly 57 bytes: the record's first 56 and the slot's one index byte.
	let mut input = [0u8; TAG_AT + 1];
	input[..TAG_AT].copy_from_slice(&head[..TAG_AT]);
	input[TAG_AT] = slot.index_byte();
	let digest = sha256::digest(&input);
	let mut out = [0u8; TAG_LEN];
	out.copy_from_slice(&digest[..TAG_LEN]);
	out
}

// One slot's record for `floor`.
pub fn encode(floor: u64, slot: Slot, product: &[u8; 32]) -> [u8; RECORD_LEN] {
	let mut record = [0u8; RECORD_LEN];
	record[..MAGIC.len()].copy_from_slice(&MAGIC);
	record[VERSION_AT..VERSION_AT + 4].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
	record[PRODUCT_AT..PRODUCT_AT + 32].copy_from_slice(product);
	record[FLOOR_AT..FLOOR_AT + 8].copy_from_slice(&floor.to_le_bytes());
	let commit = tag(&record, slot);
	record[TAG_AT..].copy_from_slice(&commit);
	record
}

// Why a record is not one this loader believes. Each is a different thing to be looking at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Invalid {
	Length(usize),
	Magic,
	Version(u32),
	Reserved(u32),
	Product,
	Tag,
}

// The floor a record carries, if it is a valid record for `slot` and this product.
pub fn decode(bytes: &[u8], slot: Slot, product: &[u8; 32]) -> Result<u64, Invalid> {
	if bytes.len() != RECORD_LEN {
		return Err(Invalid::Length(bytes.len()));
	}
	if bytes[..MAGIC.len()] != MAGIC {
		return Err(Invalid::Magic);
	}
	let version = u32::from_le_bytes([bytes[VERSION_AT], bytes[VERSION_AT + 1], bytes[VERSION_AT + 2], bytes[VERSION_AT + 3]]);
	if version != FORMAT_VERSION {
		return Err(Invalid::Version(version));
	}
	let reserved = u32::from_le_bytes([bytes[RESERVED_AT], bytes[RESERVED_AT + 1], bytes[RESERVED_AT + 2], bytes[RESERVED_AT + 3]]);
	if reserved != 0 {
		return Err(Invalid::Reserved(reserved));
	}
	if bytes[PRODUCT_AT..PRODUCT_AT + 32] != product[..] {
		return Err(Invalid::Product);
	}
	// THE TAG IS CHECKED LAST AND DECIDES: a torn write leaves it absent or partial, and a record
	// copied from the other slot carries the other slot's index in it.
	if tag(bytes, slot) != bytes[TAG_AT..] {
		return Err(Invalid::Tag);
	}
	let mut floor = [0u8; 8];
	floor.copy_from_slice(&bytes[FLOOR_AT..FLOOR_AT + 8]);
	Ok(u64::from_le_bytes(floor))
}

// What firmware answered for one variable, as the typed read reports it. DISTINCT OUTCOMES: the
// loader used to collapse every unsuccessful status and every wrong size to "not there", which is
// precisely the answer it gives for a variable nobody ever wrote - and a defence that reads
// "access denied" as "never provisioned" is defeated by a firmware fault.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Read {
	// A read that SUCCEEDED and reported the variable as not present. The only absence there is.
	Absent,
	// The variable, with the attributes firmware reports for it. `len` bytes of `bytes` are valid;
	// a variable longer than a record is reported as `Oversized` instead.
	Present { attributes: u32, len: usize, bytes: [u8; RECORD_LEN] },
	Oversized(usize),
	AccessDenied,
	DeviceError,
	// Any other failure, with the firmware's status word.
	Failed(usize),
}

impl Read {
	// A present variable, from its bytes - the shape every fixture and the firmware layer build.
	pub fn present(attributes: u32, data: &[u8]) -> Read {
		if data.len() > RECORD_LEN {
			return Read::Oversized(data.len());
		}
		let mut bytes = [0u8; RECORD_LEN];
		bytes[..data.len()].copy_from_slice(data);
		Read::Present { attributes, len: data.len(), bytes }
	}
}

// What one slot holds, once its read has been judged.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotState {
	Valid(u64),
	Absent,
	Invalid(Invalid),
	WrongAttributes(u32),
	AccessDenied,
	DeviceError,
	Failed(usize),
}

impl SlotState {
	pub fn floor(self) -> Option<u64> {
		match self {
			SlotState::Valid(floor) => Some(floor),
			_ => None,
		}
	}
}

pub fn slot_state(read: Read, slot: Slot, product: &[u8; 32]) -> SlotState {
	match read {
		Read::Absent => SlotState::Absent,
		Read::AccessDenied => SlotState::AccessDenied,
		Read::DeviceError => SlotState::DeviceError,
		Read::Failed(status) => SlotState::Failed(status),
		Read::Oversized(len) => SlotState::Invalid(Invalid::Length(len)),
		Read::Present { attributes, len, bytes } => {
			if attributes != ATTRIBUTES {
				return SlotState::WrongAttributes(attributes);
			}
			match decode(&bytes[..len], slot, product) {
				Ok(floor) => SlotState::Valid(floor),
				Err(reason) => SlotState::Invalid(reason),
			}
		}
	}
}

// Why a marker is not believed. Every one of these REFUSES the boot: only a read that succeeds and
// reports the variable as not present is absence, and collapsing any of these into absence is the
// attack - a provisioned machine booting as unprovisioned, its floor gone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MarkerRefusal {
	Length(usize),
	Value(u8),
	Attributes(u32),
	AccessDenied,
	DeviceError,
	Failed(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MarkerState {
	Absent,
	Present,
	Refused(MarkerRefusal),
}

pub fn marker_state(read: Read) -> MarkerState {
	match read {
		Read::Absent => MarkerState::Absent,
		Read::AccessDenied => MarkerState::Refused(MarkerRefusal::AccessDenied),
		Read::DeviceError => MarkerState::Refused(MarkerRefusal::DeviceError),
		Read::Failed(status) => MarkerState::Refused(MarkerRefusal::Failed(status)),
		Read::Oversized(len) => MarkerState::Refused(MarkerRefusal::Length(len)),
		Read::Present { attributes, len, bytes } => {
			if len != MARKER_LEN {
				return MarkerState::Refused(MarkerRefusal::Length(len));
			}
			if attributes != ATTRIBUTES {
				return MarkerState::Refused(MarkerRefusal::Attributes(attributes));
			}
			if bytes[0] != MARKER_VALUE {
				return MarkerState::Refused(MarkerRefusal::Value(bytes[0]));
			}
			MarkerState::Present
		}
	}
}

// Why a provisioned machine's state cannot be believed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	Marker(MarkerRefusal),
	// The marker is present and neither slot holds a valid record: a provisioned machine whose
	// state is gone. Not reset by a boot; recovered by the ceremony.
	NoValidSlot { a: SlotState, b: SlotState },
}

// The machine's provisioning state, classified from the three reads. There is no fourth answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provisioning {
	// The marker is absent, WHATEVER the slots hold: the ceremony never completed. Boots, and
	// refuses to advance.
	Unprovisioned,
	// The marker is present and at least one slot is valid. `floor` is the highest valid value;
	// `lagging` is the slot that does not equal it - invalid, absent, unreadable or lower - which
	// every accepted boot converges before control is transferred.
	Provisioned { floor: u64, lagging: Option<Slot> },
	Refused(Refusal),
}

pub fn classify(marker: MarkerState, a: SlotState, b: SlotState) -> Provisioning {
	match marker {
		MarkerState::Refused(reason) => return Provisioning::Refused(Refusal::Marker(reason)),
		MarkerState::Absent => return Provisioning::Unprovisioned,
		MarkerState::Present => {}
	}
	match (a.floor(), b.floor()) {
		(Some(x), Some(y)) if x == y => Provisioning::Provisioned { floor: x, lagging: None },
		(Some(x), Some(y)) => Provisioning::Provisioned { floor: x.max(y), lagging: Some(if x < y { Slot::A } else { Slot::B }) },
		(Some(x), None) => Provisioning::Provisioned { floor: x, lagging: Some(Slot::B) },
		(None, Some(y)) => Provisioning::Provisioned { floor: y, lagging: Some(Slot::A) },
		(None, None) => Provisioning::Refused(Refusal::NoValidSlot { a, b }),
	}
}

// The writes an accepted boot performs, in order, each at the resulting floor. An advance is two
// writes - the older or inactive slot first, never the only valid highest one - so that torn
// before the first readback leaves the old floor, torn between the two leaves the new one already
// authoritative, and the steady state after any accepted boot is both slots equal. An equal-
// generation boot writes only a lagging slot, which is how an interrupted advance is completed by
// the next boot rather than left one deletion away from the old floor.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Writes {
	pub first: Option<Slot>,
	pub second: Option<Slot>,
}

impl Writes {
	// No write at all: an equal-generation boot on a machine whose slots already agree.
	pub fn none() -> Writes {
		Writes::default()
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
	// Boots; nothing is written.
	Unprovisioned,
	// Refused: a correctly signed release older than the highest this machine accepted.
	Below { floor: u64, generation: u64 },
	// Boots once every write has landed and read back. `floor` is what both slots hold afterwards.
	Accepted { previous: u64, floor: u64, writes: Writes },
}

pub fn decide(state: Provisioning, generation: u64) -> Result<Verdict, Refusal> {
	match state {
		Provisioning::Refused(reason) => Err(reason),
		Provisioning::Unprovisioned => Ok(Verdict::Unprovisioned),
		Provisioning::Provisioned { floor, lagging } => {
			if generation < floor {
				return Ok(Verdict::Below { floor, generation });
			}
			if generation == floor {
				return Ok(Verdict::Accepted { previous: floor, floor, writes: Writes { first: lagging, second: None } });
			}
			let first = lagging.unwrap_or(Slot::A);
			Ok(Verdict::Accepted { previous: floor, floor: generation, writes: Writes { first: Some(first), second: Some(first.other()) } })
		}
	}
}

// What a write path answered. `NoWritePath` is firmware that offers no `SetVariable` at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WriteFault {
	NoWritePath,
	Refused(usize),
}

// The firmware, as this module drives it: one typed read per variable and one typed write per
// slot. The loader implements it over UEFI runtime services; the host fixtures over a mock.
pub trait Firmware {
	fn read(&mut self, which: Variable) -> Read;
	fn write(&mut self, slot: Slot, record: &[u8; RECORD_LEN]) -> Result<(), WriteFault>;
}

// How an enforced boot ended without transferring control.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fault {
	Refused(Refusal),
	Below { floor: u64, generation: u64 },
	Write { slot: Slot, fault: WriteFault },
	// The slot did not read back as written - as what it holds instead.
	Readback { slot: Slot, found: SlotState },
}

// How an enforced boot ended when control may be transferred.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
	Unprovisioned { generation: u64 },
	Accepted { previous: u64, floor: u64, converged: Writes },
}

// Read the state, decide, and perform every write with its readback, in order. Nothing here
// prints; the loader says what happened from the value returned.
pub fn enforce(firmware: &mut impl Firmware, product: &[u8; 32], generation: u64) -> Result<Outcome, Fault> {
	let marker = marker_state(firmware.read(Variable::Marker));
	let a = slot_state(firmware.read(Variable::SlotA), Slot::A, product);
	let b = slot_state(firmware.read(Variable::SlotB), Slot::B, product);
	let verdict = decide(classify(marker, a, b), generation).map_err(Fault::Refused)?;
	match verdict {
		Verdict::Unprovisioned => Ok(Outcome::Unprovisioned { generation }),
		Verdict::Below { floor, generation } => Err(Fault::Below { floor, generation }),
		Verdict::Accepted { previous, floor, writes } => {
			for slot in [writes.first, writes.second].into_iter().flatten() {
				let record = encode(floor, slot, product);
				firmware.write(slot, &record).map_err(|fault| Fault::Write { slot, fault })?;
				// READ BACK BEFORE IT IS TREATED AS CURRENT: a write firmware accepted and did not
				// keep is a floor that exists in this boot's memory and nowhere else.
				let found = slot_state(firmware.read(slot.variable()), slot, product);
				if found != SlotState::Valid(floor) {
					return Err(Fault::Readback { slot, found });
				}
			}
			Ok(Outcome::Accepted { previous, floor, converged: writes })
		}
	}
}

#[cfg(test)]
mod tests;
