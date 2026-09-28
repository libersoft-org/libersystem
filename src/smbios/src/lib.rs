// The firmware's SMBIOS tables, read with a bound on every byte.
//
// WHAT SMBIOS IS FOR HERE. It is the firmware's description of the machine as a product - who made it,
// what it is called, its serial number and UUID - and, in its type-38 records, where an IPMI controller
// sits. Nothing in it is a resource the kernel drives: it names the system on the boot log, and its
// IPMI records are a cross-check for the ACPI description of the same controller.
//
// WHERE IT COMES FROM. The UEFI loader finds the entry point in the firmware's configuration table -
// SMBIOS 3.x's 64-bit one, else 2.x's 32-bit one - and hands its physical address over; the kernel reads
// the entry point, then the structure table it names. A direct device-tree boot has neither.
//
// EVERY LENGTH IS THE FIRMWARE'S CLAIM AND IS CHECKED BEFORE IT IS USED: the entry point's checksum and
// its own length, the table's length against a bound, each structure's formatted length against what is
// left of the table, and each string set against its terminator inside the table. A table that breaks
// any of them is refused where it breaks, never read past.

#![no_std]

#[cfg(test)]
mod tests;

// The largest structure table this reader walks. SMBIOS 3.x allows 4 GiB; a real machine's is a few
// kilobytes, and a caller maps the table before handing it over, so the bound is what the caller maps.
pub const MAX_TABLE_LENGTH: u32 = 1 << 20;
// And the most structures it visits, whatever the table says: a count is a claim too.
pub const MAX_STRUCTURES: usize = 4096;

// The bytes a caller should make readable at the entry point's address: the larger of the two forms.
pub const ENTRY_POINT_BYTES: usize = 31;

const ANCHOR_V2: &[u8; 4] = b"_SM_";
const ANCHOR_V2_INTERMEDIATE: &[u8; 5] = b"_DMI_";
const ANCHOR_V3: &[u8; 5] = b"_SM3_";
const V2_LENGTH: usize = 0x1F;
const V3_LENGTH: usize = 0x18;

// The structure types this crate decodes.
pub const TYPE_SYSTEM: u8 = 1;
pub const TYPE_IPMI: u8 = 38;
pub const TYPE_END: u8 = 127;

// Why a table was not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
	// Neither anchor at the address handed over.
	NoAnchor,
	// The entry point's own length is not its form's, or the bytes handed over are shorter than it.
	BadEntryLength,
	// A checksum over the entry point (or 2.x's intermediate one) does not sum to zero.
	BadChecksum,
	// The structure table is longer than this reader walks, or shorter than one header.
	BadTableLength,
	// A structure's formatted length is below its own header or runs past the table.
	BadStructure,
	// A structure's string set has no double NUL inside the table.
	UnterminatedStrings,
}

// What the entry point says: the version and where the structure table is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryPoint {
	pub major: u8,
	pub minor: u8,
	pub table_address: u64,
	// The table's length (2.x) or its maximum size (3.x) - either way the most a walk may read.
	pub table_length: u32,
	// 2.x states how many structures there are; 3.x ends the table with type 127 instead.
	pub structure_count: Option<u16>,
}

fn sums_to_zero(bytes: &[u8]) -> bool {
	bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) == 0
}

fn le16(bytes: &[u8], at: usize) -> u16 {
	u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
	u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn le64(bytes: &[u8], at: usize) -> u64 {
	let mut word = [0u8; 8];
	word.copy_from_slice(&bytes[at..at + 8]);
	u64::from_le_bytes(word)
}

// Read the entry point from `bytes`, the memory at the address the firmware published.
pub fn entry_point(bytes: &[u8]) -> Result<EntryPoint, Refusal> {
	if bytes.len() >= ANCHOR_V3.len() && &bytes[..ANCHOR_V3.len()] == ANCHOR_V3 {
		if bytes.len() < V3_LENGTH || bytes[6] as usize != V3_LENGTH {
			return Err(Refusal::BadEntryLength);
		}
		if !sums_to_zero(&bytes[..V3_LENGTH]) {
			return Err(Refusal::BadChecksum);
		}
		let table_length = le32(bytes, 0x0C);
		if table_length < 4 || table_length > MAX_TABLE_LENGTH {
			return Err(Refusal::BadTableLength);
		}
		return Ok(EntryPoint { major: bytes[7], minor: bytes[8], table_address: le64(bytes, 0x10), table_length, structure_count: None });
	}
	if bytes.len() >= ANCHOR_V2.len() && &bytes[..ANCHOR_V2.len()] == ANCHOR_V2 {
		if bytes.len() < V2_LENGTH || bytes[5] as usize != V2_LENGTH {
			return Err(Refusal::BadEntryLength);
		}
		if !sums_to_zero(&bytes[..V2_LENGTH]) {
			return Err(Refusal::BadChecksum);
		}
		// THE INTERMEDIATE ANCHOR AND ITS OWN CHECKSUM, which is what a legacy (DMI) reader validates:
		// the two halves are checked separately because they were written separately.
		if &bytes[0x10..0x15] != ANCHOR_V2_INTERMEDIATE || !sums_to_zero(&bytes[0x10..V2_LENGTH]) {
			return Err(Refusal::BadChecksum);
		}
		let table_length = u32::from(le16(bytes, 0x16));
		if table_length < 4 {
			return Err(Refusal::BadTableLength);
		}
		return Ok(EntryPoint { major: bytes[6], minor: bytes[7], table_address: u64::from(le32(bytes, 0x18)), table_length, structure_count: Some(le16(bytes, 0x1C)) });
	}
	Err(Refusal::NoAnchor)
}

// One structure: its type, handle, the formatted area (header included) and its string set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Structure<'a> {
	pub kind: u8,
	pub handle: u16,
	pub formatted: &'a [u8],
	strings: &'a [u8],
}

impl<'a> Structure<'a> {
	// A byte of the formatted area, or None past it - a field a shorter, older structure does not have.
	pub fn byte(&self, offset: usize) -> Option<u8> {
		self.formatted.get(offset).copied()
	}

	pub fn word(&self, offset: usize) -> Option<u16> {
		(offset + 2 <= self.formatted.len()).then(|| le16(self.formatted, offset))
	}

	pub fn qword(&self, offset: usize) -> Option<u64> {
		(offset + 8 <= self.formatted.len()).then(|| le64(self.formatted, offset))
	}

	// String `index` of the set, counting from one; zero is "no string", which is the specification's own
	// meaning, and an index past the set is None too.
	pub fn string(&self, index: u8) -> Option<&'a [u8]> {
		if index == 0 {
			return None;
		}
		self.strings.split(|byte| *byte == 0).filter(|text| !text.is_empty()).nth(index as usize - 1)
	}

	// The string whose index is the byte at `offset`.
	pub fn string_at(&self, offset: usize) -> Option<&'a [u8]> {
		self.string(self.byte(offset)?)
	}
}

// The structures of a table, in order, each checked before it is yielded. A refusal ends the walk.
pub struct Structures<'a> {
	table: &'a [u8],
	at: usize,
	seen: usize,
	limit: usize,
	done: bool,
}

// Walk `table`, the bytes at the entry point's table address, `entry.table_length` of them.
pub fn structures<'a>(table: &'a [u8], entry: &EntryPoint) -> Structures<'a> {
	let length = (entry.table_length as usize).min(table.len()).min(MAX_TABLE_LENGTH as usize);
	let limit = entry.structure_count.map_or(MAX_STRUCTURES, |count| (count as usize).min(MAX_STRUCTURES));
	Structures { table: &table[..length], at: 0, seen: 0, limit, done: false }
}

impl<'a> Iterator for Structures<'a> {
	type Item = Result<Structure<'a>, Refusal>;

	fn next(&mut self) -> Option<Self::Item> {
		if self.done || self.seen >= self.limit || self.at + 4 > self.table.len() {
			return None;
		}
		let rest = &self.table[self.at..];
		let length = rest[1] as usize;
		if length < 4 || length > rest.len() {
			self.done = true;
			return Some(Err(Refusal::BadStructure));
		}
		// THE STRING SET ENDS AT THE FIRST DOUBLE NUL after the formatted area - which, for a structure with
		// no strings at all, is the two NULs straight after it.
		let tail = &rest[length..];
		let Some(end) = tail.windows(2).position(|pair| pair == [0, 0]) else {
			self.done = true;
			return Some(Err(Refusal::UnterminatedStrings));
		};
		let structure = Structure { kind: rest[0], handle: le16(rest, 2), formatted: &rest[..length], strings: &tail[..end] };
		self.at += length + end + 2;
		self.seen += 1;
		if structure.kind == TYPE_END {
			self.done = true;
		}
		Some(Ok(structure))
	}
}

// THE SYSTEM, as type 1 names it - the product this machine is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SystemIdentity<'a> {
	pub manufacturer: Option<&'a [u8]>,
	pub product: Option<&'a [u8]>,
	pub version: Option<&'a [u8]>,
	pub serial: Option<&'a [u8]>,
	pub uuid: Option<[u8; 16]>,
	pub sku: Option<&'a [u8]>,
	pub family: Option<&'a [u8]>,
}

// The first type-1 structure, decoded; Ok(None) for a table that has none.
pub fn system_identity<'a>(table: &'a [u8], entry: &EntryPoint) -> Result<Option<SystemIdentity<'a>>, Refusal> {
	for structure in structures(table, entry) {
		let structure = structure?;
		if structure.kind != TYPE_SYSTEM {
			continue;
		}
		// THE UUID, when the structure is long enough to carry one and it is neither all zeros ("not
		// present") nor all ones ("not set") - the specification's two ways of saying there is none.
		let uuid = structure.formatted.get(8..24).and_then(|bytes| {
			let mut uuid = [0u8; 16];
			uuid.copy_from_slice(bytes);
			(uuid != [0; 16] && uuid != [0xFF; 16]).then_some(uuid)
		});
		return Ok(Some(SystemIdentity { manufacturer: structure.string_at(4), product: structure.string_at(5), version: structure.string_at(6), serial: structure.string_at(7), uuid, sku: structure.string_at(0x19), family: structure.string_at(0x1A) }));
	}
	Ok(None)
}

// THE UUID AS TEXT, in the byte order SMBIOS 2.6 and later define: the first three fields little-endian,
// the rest as stored.
pub fn uuid_text(uuid: &[u8; 16], out: &mut [u8; 36]) {
	const HEX: &[u8; 16] = b"0123456789abcdef";
	let order: [usize; 16] = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];
	let mut at = 0;
	for (position, &index) in order.iter().enumerate() {
		if matches!(position, 4 | 6 | 8 | 10) {
			out[at] = b'-';
			at += 1;
		}
		out[at] = HEX[(uuid[index] >> 4) as usize];
		out[at + 1] = HEX[(uuid[index] & 0xF) as usize];
		at += 2;
	}
}

// The IPMI interface types type 38 names.
pub const IPMI_KCS: u8 = 1;
pub const IPMI_SMIC: u8 = 2;
pub const IPMI_BT: u8 = 3;
pub const IPMI_SSIF: u8 = 4;

// ONE TYPE-38 RECORD: where an IPMI controller is, as the firmware's SMBIOS table says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IpmiRecord {
	// The record's position among the table's type-38 structures, which is what `smbios:38#n` names.
	pub instance: u16,
	pub handle: u16,
	pub interface: u8,
	pub revision: u8,
	pub i2c_target: u8,
	pub base: u64,
	// The modifier byte, when the record is long enough to carry one.
	pub modifier: Option<u8>,
	pub interrupt: Option<u8>,
}

impl IpmiRecord {
	// Whether the base names I/O space: its low bit, which is a flag here and not part of the address.
	pub fn io_space(&self) -> bool {
		self.base & 1 != 0
	}

	// THE ADDRESS OF A KCS, SMIC OR BT INTERFACE, decoded: the flag bit cleared, and address bit 0 taken from
	// the modifier byte's bit 4 - which is where the specification put the bit the flag displaced.
	pub fn address(&self) -> u64 {
		(self.base & !1) | u64::from(self.modifier.is_some_and(|modifier| modifier & 0x10 != 0))
	}

	// THE SEVEN-BIT ADDRESS OF AN SSIF INTERFACE: the base holds the eight-bit (shifted) form.
	pub fn ssif_address(&self) -> u8 {
		((self.base >> 1) & 0x7F) as u8
	}
}

// Every type-38 record, in table order; a refusal ends the walk and is returned.
pub fn ipmi_records(table: &[u8], entry: &EntryPoint, mut visit: impl FnMut(IpmiRecord)) -> Result<(), Refusal> {
	let mut instance = 0u16;
	for structure in structures(table, entry) {
		let structure = structure?;
		if structure.kind != TYPE_IPMI {
			continue;
		}
		// A RECORD TOO SHORT TO NAME AN ADDRESS names nothing and is skipped, but still counts as an instance:
		// `smbios:38#n` is the position in the table, not among the readable ones.
		if let (Some(interface), Some(revision), Some(i2c_target), Some(base)) = (structure.byte(4), structure.byte(5), structure.byte(6), structure.qword(8)) {
			visit(IpmiRecord { instance, handle: structure.handle, interface, revision, i2c_target, base, modifier: structure.byte(0x10), interrupt: structure.byte(0x11).filter(|number| *number != 0) });
		}
		instance = instance.saturating_add(1);
	}
	Ok(())
}
