// SRAT and SLIT, read as bytes.
//
// FROM A SLICE, NOT FROM A POINTER, which is the whole reason this is testable: the kernel maps the
// table and hands the bytes here, and a host hands it a table with one byte changed. Every length in
// an ACPI table is a number the firmware wrote, and each of them is checked against the buffer that
// actually exists rather than against the table's own claims about itself.

use alloc::vec::Vec;

use crate::{Builder, Error, LOCAL_DISTANCE, MAX_NODES, NodeId};

// The header every ACPI system table starts with: signature, length, revision, checksum.
const HEADER_LEN: usize = 36;

// SRAT entry types.
const SRAT_PROCESSOR_LOCAL_APIC: u8 = 0;
const SRAT_MEMORY: u8 = 1;
const SRAT_PROCESSOR_LOCAL_X2APIC: u8 = 2;

// The `enabled` bit both processor and memory affinity structures carry in their flags.
const FLAG_ENABLED: u32 = 1 << 0;
// Memory affinity: this range can be hot-removed. It is described, and it is not seeded.
const FLAG_HOT_PLUGGABLE: u32 = 1 << 1;

fn le16(bytes: &[u8], at: usize) -> u16 {
	u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
	u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn le64(bytes: &[u8], at: usize) -> u64 {
	let mut out = [0u8; 8];
	out.copy_from_slice(&bytes[at..at + 8]);
	u64::from_le_bytes(out)
}

// The table's own header, checked against the buffer it arrived in.
//
// THREE SEPARATE THINGS ARE CHECKED and they fail differently: the signature says this is the table
// that was asked for, the length says the table is not longer than the memory holding it, and the
// checksum says the bytes are the ones firmware wrote. A table can pass any two and fail the third.
pub fn header_ok(bytes: &[u8], signature: &[u8; 4]) -> Result<usize, Error> {
	if bytes.len() < HEADER_LEN {
		return Err(Error::Truncated);
	}
	if &bytes[0..4] != signature {
		return Err(Error::WrongSignature);
	}
	let length = le32(bytes, 4) as usize;
	if length < HEADER_LEN || length > bytes.len() {
		return Err(Error::Truncated);
	}
	let sum = bytes[..length].iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte));
	if sum != 0 {
		return Err(Error::BadChecksum);
	}
	Ok(length)
}

// Read an SRAT into the builder.
//
// A DISABLED ENTRY IS NOT AN ERROR AND NOT AN AFFINITY. Firmware describes processors and memory
// that are present but not enabled - hot-plug slots, disabled cores - and both are skipped: an
// affinity recorded for memory that is not there would steer allocations at nothing.
pub fn parse_srat(bytes: &[u8], into: &mut Builder) -> Result<(), Error> {
	let length = header_ok(bytes, b"SRAT")?;
	// 36 header + 4 reserved (revision 1) + 8 reserved = the first entry at 48.
	let mut at = 48usize;
	while at + 2 <= length {
		let kind = bytes[at];
		let entry_len = bytes[at + 1] as usize;
		// AN ENTRY MUST FIT INSIDE THE TABLE IT CLAIMS TO BE IN. A length of zero would loop for
		// ever, and one running past the end is a table that lies about its own contents.
		if entry_len < 2 || at + entry_len > length {
			return Err(Error::Truncated);
		}
		// A KNOWN TYPE WHOSE LENGTH CONTRADICTS ITS OWN DEFINITION IS TRUNCATED, NOT UNKNOWN.
		//
		// The guards below used to be part of each match arm, so a `SRAT_PROCESSOR_LOCAL_APIC` with
		// a length of twelve fell past all of them into the catch-all and was skipped in the same
		// silence as a structure type this reader has never heard of. Those are opposite facts:
		// firmware describing a type this does not read is entitled to; firmware saying "this is a
		// processor record" and not supplying the fields a processor record has is a damaged table,
		// and the CPU it dropped is exactly the kind of loss nothing downstream can notice.
		let declared = match kind {
			SRAT_PROCESSOR_LOCAL_APIC => Some(16usize),
			SRAT_PROCESSOR_LOCAL_X2APIC => Some(24),
			SRAT_MEMORY => Some(40),
			_ => None,
		};
		if let Some(needed) = declared
			&& entry_len < needed
		{
			return Err(Error::Truncated);
		}
		match kind {
			SRAT_PROCESSOR_LOCAL_APIC if entry_len >= 16 => {
				let flags = le32(bytes, at + 4);
				if flags & FLAG_ENABLED != 0 {
					// The proximity domain is SPLIT ACROSS THE STRUCTURE: one byte at offset 2, and
					// three more at offset 9. Reading only the low byte is correct for every machine
					// with fewer than 256 nodes and silently wrong for the rest.
					let low = bytes[at + 2] as u32;
					let high = (bytes[at + 9] as u32) | ((bytes[at + 10] as u32) << 8) | ((bytes[at + 11] as u32) << 16);
					let node = NodeId(low | (high << 8));
					into.add_cpu(bytes[at + 3] as u64, node);
				}
			}
			SRAT_PROCESSOR_LOCAL_X2APIC if entry_len >= 24 => {
				let flags = le32(bytes, at + 12);
				if flags & FLAG_ENABLED != 0 {
					into.add_cpu(le32(bytes, at + 8) as u64, NodeId(le32(bytes, at + 4)));
				}
			}
			SRAT_MEMORY if entry_len >= 40 => {
				// THE MEMORY STRUCTURE'S PROXIMITY DOMAIN IS AT OFFSET 2, and the processor
				// structures' are not - the x2APIC one is at 4, and the classic APIC one is a byte
				// at 2 with three more at 9. Three structures, three layouts, and reading the memory
				// one at 4 lands on the two reserved bytes and the low half of the base address:
				// every range then reports node zero on a machine whose memory is split in two, and
				// the boot report says "node 0: 4095 MiB, node 1: 0 MiB" while the processors are
				// distributed correctly. That is what this looked like, and the fixture below had
				// been written from the same wrong reading.
				let node = NodeId(le32(bytes, at + 2));
				let flags = le32(bytes, at + 28);
				let base = (le32(bytes, at + 8) as u64) | ((le32(bytes, at + 12) as u64) << 32);
				let len = (le32(bytes, at + 16) as u64) | ((le32(bytes, at + 20) as u64) << 32);
				// A hot-pluggable range is DESCRIBED and not seeded: it may be removed under a
				// running system, and memory that can disappear is not memory to hand out.
				if flags & FLAG_ENABLED != 0 && flags & FLAG_HOT_PLUGGABLE == 0 && len > 0 {
					into.add_memory(base, len, node);
				} else if flags & FLAG_ENABLED != 0 {
					// The node exists even where this particular range is not usable.
					into.note_node(node);
				}
			}
			// A structure type this kernel does not read - a generic initiator, a memory-side cache -
			// is skipped by its own length rather than treated as an error. Firmware is entitled to
			// describe more than this reads.
			_ => {}
		}
		at += entry_len;
	}
	// The `_` for the unused constant: `le16` and `le64` are used by the SLIT and FDT readers.
	let _ = (le16(bytes, 0), le64(bytes, 0));
	Ok(())
}

// Read a SLIT: a square matrix of directed distances in the firmware's own node numbering.
//
// ASYMMETRIC IS LEGAL. The distance from node 0 to node 1 need not equal the distance back, and a
// reader that assumed symmetry would quietly halve a real machine's table.
pub fn parse_slit(bytes: &[u8], into: &mut Builder) -> Result<(), Error> {
	let length = header_ok(bytes, b"SLIT")?;
	if length < HEADER_LEN + 8 {
		return Err(Error::Truncated);
	}
	let count = le64(bytes, HEADER_LEN);
	// BOUNDED BEFORE ANYTHING IS ALLOCATED. The count is a firmware number and the matrix is its
	// square, so a plausible-looking sixty-five thousand becomes four gigabytes.
	if count == 0 || count as usize > MAX_NODES {
		return Err(Error::TooManyNodes);
	}
	let size = count as usize;
	let cells_at = HEADER_LEN + 8;
	if cells_at + size * size > length {
		return Err(Error::Truncated);
	}
	let mut cells = Vec::new();
	if cells.try_reserve(size * size).is_err() {
		return Err(Error::TooManyNodes);
	}
	cells.extend_from_slice(&bytes[cells_at..cells_at + size * size]);
	for index in 0..size {
		if cells[index * size + index] != LOCAL_DISTANCE {
			return Err(Error::MalformedMatrix);
		}
	}
	// AND NOTHING IS NEARER THAN LOCAL, REFUSED HERE RATHER THAN IN THE BUILDER.
	//
	// The builder rejects this too, and that is not the same thing at this boundary. The kernel reads
	// a `parse_slit` error as "the distances are bad, keep the affinity" and a `build` error as "the
	// topology does not hold together, discard all of it" - so leaving the check to the builder turned
	// a distance-only defect into the loss of every CPU and memory affinity the SRAT reported
	// correctly. The two readers are required to refuse the same false table; this is the ACPI half.
	for from in 0..size {
		for to in 0..size {
			if from != to && cells[from * size + to] < LOCAL_DISTANCE {
				return Err(Error::MalformedMatrix);
			}
		}
	}
	into.set_matrix(size, cells);
	Ok(())
}
