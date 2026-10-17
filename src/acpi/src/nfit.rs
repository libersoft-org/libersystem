//! THE NVDIMM FIRMWARE INTERFACE TABLE: the persistent memory the machine has, which modules back it, and what the
//! platform promises about a write reaching it.
//!
//! WHAT IS READ, AND WHAT IS NOT. The system-physical-address ranges, the region mappings that say which module
//! backs which range and in what state it is, the flush hints and the platform's capabilities - each bounded by the
//! table's own length and by its structure's. The control regions are read so they can be refused: a range whose type
//! is a module's control or block-window region holds the module's registers and its firmware interface, and nothing
//! derived from this table ever describes one as a device. Vendor management commands, labels and interleave quirks
//! are not read at all.
//!
//! A PERSISTENT-MEMORY NAMESPACE, AS THIS READER ANSWERS ONE, is one persistent-memory range that passed every check
//! below, with what its modules say about it folded in: the OR of their state flags, how many back it and how many
//! flush hints they name. No label is read, so a namespace is the whole range - what an unlabelled module is.

use super::{Error, Table};

/// An address range type, as the table writes a GUID: the first three fields little-endian, the last eight as written.
pub type Guid = [u8; 16];

/// Persistent memory: 66F0D379-B4F3-4074-AC43-0D3318B78CDB.
pub const PERSISTENT_MEMORY: Guid = [0x79, 0xd3, 0xf0, 0x66, 0xf3, 0xb4, 0x74, 0x40, 0xac, 0x43, 0x0d, 0x33, 0x18, 0xb7, 0x8c, 0xdb];
/// A module's control region: 92F701F6-13B4-405D-910B-299367E8234C.
pub const CONTROL_REGION: Guid = [0xf6, 0x01, 0xf7, 0x92, 0xb4, 0x13, 0x5d, 0x40, 0x91, 0x0b, 0x29, 0x93, 0x67, 0xe8, 0x23, 0x4c];
/// A module's block data window: 91AF0530-5D86-470E-A6B0-0A2DB9408249.
pub const BLOCK_DATA_WINDOW: Guid = [0x30, 0x05, 0xaf, 0x91, 0x86, 0x5d, 0x0e, 0x47, 0xa6, 0xb0, 0x0a, 0x2d, 0xb9, 0x40, 0x82, 0x49];
/// Volatile memory: 7305944F-FDDA-44E3-B16C-3F22D252E5D0.
pub const VOLATILE_MEMORY: Guid = [0x4f, 0x94, 0x05, 0x73, 0xda, 0xfd, 0xe3, 0x44, 0xb1, 0x6c, 0x3f, 0x22, 0xd2, 0x52, 0xe5, 0xd0];

/// The structure types this reader decodes.
pub const TYPE_SPA_RANGE: u16 = 0;
pub const TYPE_REGION_MAPPING: u16 = 1;
pub const TYPE_CONTROL_REGION: u16 = 4;
pub const TYPE_FLUSH_HINT: u16 = 6;
pub const TYPE_CAPABILITIES: u16 = 7;

/// A region mapping's state flags.
/// The module's last save to its persistent media failed.
pub const STATE_SAVE_FAILED: u16 = 1 << 0;
/// The module's last restore from its persistent media failed.
pub const STATE_RESTORE_FAILED: u16 = 1 << 1;
/// The platform's flush of the module before the last power loss failed.
pub const STATE_FLUSH_FAILED: u16 = 1 << 2;
/// The module is not armed: a write to it will not be saved at the next power loss.
pub const STATE_NOT_ARMED: u16 = 1 << 3;
/// The module has observed SMART health events.
pub const STATE_SMART_EVENTS: u16 = 1 << 4;
/// The module is not mapped into any address range.
pub const STATE_NOT_MAPPED: u16 = 1 << 6;

/// The platform capabilities.
/// The processor's caches are flushed to persistence on a power loss: a store is durable once it is globally visible.
pub const CAPABILITY_CPU_CACHE_FLUSH: u32 = 1 << 0;
/// The memory controller's queues are flushed to persistence on a power loss: a store is durable once it has left the
/// processor's caches.
pub const CAPABILITY_MEMORY_CONTROLLER_FLUSH: u32 = 1 << 1;

/// Where the structures start: after the header and four reserved bytes.
pub const FIRST_STRUCTURE: usize = 40;

/// The most structures one walk visits, and the most flush hints one structure is read for: bounds a real table is
/// far inside and a hostile one cannot stretch.
pub const MAX_STRUCTURES: usize = 1024;
pub const MAX_FLUSH_HINTS: usize = 16;

/// The page a namespace's base and length are held to.
pub const PAGE: u64 = 4096;

/// One system-physical-address range.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SpaRange {
	pub index: u16,
	pub flags: u16,
	pub proximity: u32,
	pub kind: Guid,
	pub base: u64,
	pub len: u64,
	/// The EFI memory attributes the range may be mapped with.
	pub attributes: u64,
}

/// One region mapping: which module backs which part of which range, and its state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RegionMapping {
	pub handle: u32,
	pub physical_id: u16,
	pub region_id: u16,
	pub spa_index: u16,
	pub control_index: u16,
	pub size: u64,
	pub offset: u64,
	pub device_address: u64,
	pub interleave_index: u16,
	pub interleave_ways: u16,
	pub state: u16,
}

/// One module's control region, by its identity: what it is and how many block windows it has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ControlRegion {
	pub index: u16,
	pub vendor: u16,
	pub device: u16,
	pub revision: u16,
	pub format: u16,
	pub windows: u16,
}

/// One module's flush hints: the addresses whose write drains the memory controller's queues for it, at most
/// `MAX_FLUSH_HINTS` of them read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FlushHint {
	pub handle: u32,
	pub declared: u16,
	pub addresses: [u64; MAX_FLUSH_HINTS],
}

impl FlushHint {
	/// The addresses read: the declared count, bounded.
	pub fn addresses(&self) -> &[u64] {
		&self.addresses[..(self.declared as usize).min(MAX_FLUSH_HINTS)]
	}
}

/// One structure of the table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Structure {
	Spa(SpaRange),
	Mapping(RegionMapping),
	Control(ControlRegion),
	FlushHint(FlushHint),
	/// The capabilities the platform declares valid - the bits past its highest valid one cleared.
	Capabilities(u32),
	/// A structure of a type this reader does not decode, or a known type too short for its fields.
	Other(u16),
}

/// The table's structures, each checked against the table and against its own length before it is decoded; one that
/// does not fit ends the walk, as does the bound.
pub fn structures(bytes: &[u8], mut visit: impl FnMut(Structure)) -> Result<(), Error> {
	let table = Table::with_signature(bytes, b"NFIT")?;
	let mut offset = FIRST_STRUCTURE;
	for _ in 0..MAX_STRUCTURES {
		let (Some(kind), Some(length)) = (table.u16_at(offset), table.u16_at(offset + 2).map(usize::from)) else { break };
		if length < 4 || offset.checked_add(length).is_none_or(|end| end > table.len()) {
			break;
		}
		visit(decode(&table, offset, kind, length));
		offset += length;
	}
	Ok(())
}

fn decode(table: &Table<'_>, at: usize, kind: u16, length: usize) -> Structure {
	let u16_at = |field: usize| table.u16_at(at + field).unwrap_or(0);
	let u32_at = |field: usize| table.u32_at(at + field).unwrap_or(0);
	let u64_at = |field: usize| table.u64_at(at + field).unwrap_or(0);
	match kind {
		TYPE_SPA_RANGE if length >= 56 => {
			let mut guid = [0u8; 16];
			guid.copy_from_slice(&table.bytes()[at + 16..at + 32]);
			Structure::Spa(SpaRange { index: u16_at(4), flags: u16_at(6), proximity: u32_at(12), kind: guid, base: u64_at(32), len: u64_at(40), attributes: u64_at(48) })
		}
		TYPE_REGION_MAPPING if length >= 48 => Structure::Mapping(RegionMapping { handle: u32_at(4), physical_id: u16_at(8), region_id: u16_at(10), spa_index: u16_at(12), control_index: u16_at(14), size: u64_at(16), offset: u64_at(24), device_address: u64_at(32), interleave_index: u16_at(40), interleave_ways: u16_at(42), state: u16_at(44) }),
		// THE IDENTITY PART IS 32 BYTES; the block-window fields after it are not read.
		TYPE_CONTROL_REGION if length >= 32 => Structure::Control(ControlRegion { index: u16_at(4), vendor: u16_at(6), device: u16_at(8), revision: u16_at(10), format: u16_at(28), windows: u16_at(30) }),
		TYPE_FLUSH_HINT if length >= 16 => {
			// THE DECLARED COUNT IS HELD TO THE STRUCTURE'S OWN LENGTH, then to the bound: a count the length does not
			// cover names addresses another structure owns.
			let declared = u16_at(8);
			let fits = (length - 16) / 8;
			let declared = declared.min(u16::try_from(fits).unwrap_or(u16::MAX));
			let mut addresses = [0u64; MAX_FLUSH_HINTS];
			for (slot, address) in addresses.iter_mut().enumerate().take(declared as usize) {
				*address = u64_at(16 + slot * 8);
			}
			Structure::FlushHint(FlushHint { handle: u32_at(4), declared, addresses })
		}
		TYPE_CAPABILITIES if length >= 16 => {
			let highest = table.u8_at(at + 4).unwrap_or(0);
			let valid = if highest >= 31 { u32::MAX } else { (1u32 << (highest + 1)) - 1 };
			Structure::Capabilities(u32_at(8) & valid)
		}
		_ => Structure::Other(kind),
	}
}

/// One persistent-memory namespace: a range that passed every check, and what its modules say about it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Namespace {
	pub index: u16,
	pub base: u64,
	pub len: u64,
	pub attributes: u64,
	/// The OR of the state flags of every module backing it.
	pub state: u16,
	/// How many modules back it: more than one is an interleave set.
	pub modules: u16,
	/// How many flush hints those modules name.
	pub flush_hints: u16,
	/// The platform's capabilities, where the table declares them.
	pub capabilities: Option<u32>,
}

/// Why a persistent-memory range is not a namespace.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Index zero, which the specification reserves.
	Index,
	/// A range of no length.
	Empty,
	/// A base or a length that is not whole pages.
	Unaligned,
	/// A range whose end does not fit in an address.
	Wraps,
	/// Another range has the same index.
	Duplicate,
	/// The range overlaps another range of the table, of whatever type - a control region's among them.
	Overlap,
	/// No module backs it.
	Unbacked,
	/// The modules backing it do not account for its length.
	Uncovered,
}

/// Every persistent-memory range of the table, as a namespace or as the reason it is not one, with the range's index.
/// A range of any other type is never visited: a control region or a block window is a module's registers, and
/// volatile memory is not this reader's.
pub fn namespaces(bytes: &[u8], mut visit: impl FnMut(Result<Namespace, (u16, Refusal)>)) -> Result<(), Error> {
	let mut capabilities = None;
	structures(bytes, |structure| {
		if let Structure::Capabilities(found) = structure {
			capabilities = Some(found);
		}
	})?;
	structures(bytes, |structure| {
		let Structure::Spa(range) = structure else { return };
		if range.kind != PERSISTENT_MEMORY {
			return;
		}
		visit(namespace(bytes, &range, capabilities).map_err(|refusal| (range.index, refusal)));
	})
}

fn overlaps(a: u64, a_len: u64, b: u64, b_len: u64) -> bool {
	a < b.saturating_add(b_len) && b < a.saturating_add(a_len)
}

fn namespace(bytes: &[u8], range: &SpaRange, capabilities: Option<u32>) -> Result<Namespace, Refusal> {
	if range.index == 0 {
		return Err(Refusal::Index);
	}
	if range.len == 0 {
		return Err(Refusal::Empty);
	}
	if range.base % PAGE != 0 || range.len % PAGE != 0 {
		return Err(Refusal::Unaligned);
	}
	if range.base.checked_add(range.len).is_none() {
		return Err(Refusal::Wraps);
	}
	// EVERY OTHER RANGE, AND EVERY MODULE: a second walk, which a table this reader bounds keeps cheap.
	let (mut same_index, mut overlap) = (0usize, false);
	let (mut modules, mut state, mut covered, mut wraps) = (0u16, 0u16, 0u64, false);
	let mut handles = [0u32; 64];
	let mut held = 0usize;
	let _ = structures(bytes, |structure| match structure {
		Structure::Spa(other) => {
			if other.index == range.index {
				same_index += 1;
				if other != *range {
					overlap |= overlaps(range.base, range.len, other.base, other.len);
				}
			} else if overlaps(range.base, range.len, other.base, other.len) {
				overlap = true;
			}
		}
		// A MODULE THE TABLE SAYS IS NOT MAPPED backs nothing, whatever index it names.
		Structure::Mapping(mapping) if mapping.spa_index == range.index && mapping.state & STATE_NOT_MAPPED == 0 => {
			modules = modules.saturating_add(1);
			state |= mapping.state;
			match covered.checked_add(mapping.size) {
				Some(sum) => covered = sum,
				None => wraps = true,
			}
			if held < handles.len() {
				handles[held] = mapping.handle;
				held += 1;
			}
		}
		_ => {}
	});
	if same_index > 1 {
		return Err(Refusal::Duplicate);
	}
	if overlap {
		return Err(Refusal::Overlap);
	}
	if modules == 0 {
		return Err(Refusal::Unbacked);
	}
	if wraps || covered != range.len {
		return Err(Refusal::Uncovered);
	}
	let mut flush_hints = 0u16;
	let _ = structures(bytes, |structure| {
		if let Structure::FlushHint(hint) = structure
			&& handles[..held].contains(&hint.handle)
		{
			flush_hints = flush_hints.saturating_add(hint.addresses().len() as u16);
		}
	});
	Ok(Namespace { index: range.index, base: range.base, len: range.len, attributes: range.attributes, state, modules, flush_hints, capabilities })
}

/// THE NAMESPACE AS A ROW CARRIES IT TO ITS DRIVER: one fixed record, so a table of any size reaches the driver as
/// the few fields that are about its range. `[index u16][state u16][modules u16][flush hints u16][capabilities u32]
/// [capabilities present u32][base u64][len u64][attributes u64]`, little-endian.
pub const RECORD_LEN: usize = 40;

impl Namespace {
	pub fn encode(&self) -> [u8; RECORD_LEN] {
		let mut out = [0u8; RECORD_LEN];
		out[0..2].copy_from_slice(&self.index.to_le_bytes());
		out[2..4].copy_from_slice(&self.state.to_le_bytes());
		out[4..6].copy_from_slice(&self.modules.to_le_bytes());
		out[6..8].copy_from_slice(&self.flush_hints.to_le_bytes());
		out[8..12].copy_from_slice(&self.capabilities.unwrap_or(0).to_le_bytes());
		out[12..16].copy_from_slice(&u32::from(self.capabilities.is_some()).to_le_bytes());
		out[16..24].copy_from_slice(&self.base.to_le_bytes());
		out[24..32].copy_from_slice(&self.len.to_le_bytes());
		out[32..40].copy_from_slice(&self.attributes.to_le_bytes());
		out
	}

	pub fn decode(bytes: &[u8]) -> Option<Namespace> {
		let bytes: &[u8; RECORD_LEN] = bytes.get(..RECORD_LEN)?.try_into().ok()?;
		let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
		let u32_at = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
		let u64_at = |at: usize| u64::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3], bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]);
		let present = match u32_at(12) {
			0 => false,
			1 => true,
			_ => return None,
		};
		Some(Namespace { index: u16_at(0), state: u16_at(2), modules: u16_at(4), flush_hints: u16_at(6), capabilities: present.then(|| u32_at(8)), base: u64_at(16), len: u64_at(24), attributes: u64_at(32) })
	}
}

#[cfg(test)]
mod tests;
