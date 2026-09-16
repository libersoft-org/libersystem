// THE NVM EXPRESS DECISIONS, WITH NO CONTROLLER BEHIND THEM.
//
// Everything here is a function from bytes a controller reported, or numbers a caller asked for, to
// an answer with a refusal in it. None of it touches MMIO, none of it allocates, and that is the
// point: the register layouts, the completion rules and the transfer arithmetic are where an NVMe
// driver is actually wrong, and all three can be watched failing on the host. What is left in the
// driver is the part a real controller has to be present for.
//
// The specification is NVM Express Base; the register and command layouts below name their offsets
// so a reader can check them against it rather than against this file's confidence.
//
// WHY THE REFUSALS ARE TYPED AND NOT `bool`. Every one of these answers a question the driver must
// act differently on: a controller that reports no NVM command set is a controller to leave alone,
// a namespace with metadata is one to skip while serving its siblings, and a transfer too large is
// a request to split rather than a device to give up on. A `None` would collapse all three.

// ------------------------------------------------------------------ controller capabilities

// Controller register offsets, fixed by the specification.
pub const REG_CAP: u64 = 0x00; // capabilities, 64-bit
pub const REG_VS: u64 = 0x08; // version
pub const REG_CC: u64 = 0x14; // configuration
pub const REG_CSTS: u64 = 0x1C; // status
pub const REG_AQA: u64 = 0x24; // admin queue attributes
pub const REG_ASQ: u64 = 0x28; // admin submission queue base, 64-bit
pub const REG_ACQ: u64 = 0x30; // admin completion queue base, 64-bit
// The doorbell array starts here; every queue's pair is computed from the stride.
pub const REG_DOORBELL_BASE: u64 = 0x1000;

// `CC.EN` and `CSTS.RDY` are both bit 0 of their register, which is the coincidence that makes a
// transposed pair of offsets impossible to see. They are named rather than written as `1`.
pub const CC_ENABLE: u32 = 1 << 0;
pub const CSTS_READY: u32 = 1 << 0;
// `CSTS.CFS` - the controller has failed. A bring-up that waits for READY without watching this
// waits for the full timeout on a controller that has already said it will never be ready.
pub const CSTS_FATAL: u32 = 1 << 1;

// A submission queue entry is 64 bytes and a completion queue entry is 16, and `CC.IOSQES`/`IOCQES`
// carry those as powers of two. They are constants of the command set rather than choices.
pub const SQ_ENTRY_LEN: usize = 64;
pub const CQ_ENTRY_LEN: usize = 16;
pub const SQ_ENTRY_SHIFT: u32 = 6;
pub const CQ_ENTRY_SHIFT: u32 = 4;

// Why a controller cannot be used at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unusable {
	// `CAP.CSS` does not advertise the NVM command set. Every command this driver sends belongs to
	// it, so there is nothing to send.
	NoNvmCommandSet,
	// The controller's minimum host page size is larger than the page this driver has. The PRP
	// arithmetic below is in host pages and would be describing the wrong unit.
	PageTooSmall,
	// `CAP.MQES` is zero, which would be a queue with no entries in it.
	NoQueueEntries,
}

// What `CAP` said, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capabilities {
	// The most entries any queue may have. THE WIRE VALUE IS ZERO-BASED and this is not: a driver
	// that programmed the raw field as a size would make every queue one entry too long, and the
	// entry past the end is the one the controller writes a completion into.
	pub max_entries: u32,
	// The doorbell stride, already as BYTES rather than as the register's power-of-two exponent.
	pub doorbell_stride: u64,
	// The worst-case enable/disable wait, in units of 500 ms as the register reports it.
	pub timeout_500ms: u32,
	// The controller's minimum host page size, in bytes.
	pub min_page: u64,
}

impl Capabilities {
	pub fn decode(cap: u64, host_page: u64) -> Result<Capabilities, Unusable> {
		// CAP.CSS is bits 44:37; bit 37 is the NVM command set.
		if (cap >> 37) & 0x01 == 0 {
			return Err(Unusable::NoNvmCommandSet);
		}
		// CAP.MQES is bits 15:0, zero-based.
		let mqes = (cap & 0xFFFF) as u32;
		if mqes == 0 {
			return Err(Unusable::NoQueueEntries);
		}
		// CAP.MPSMIN is bits 51:48, as `2^(12 + value)`.
		let min_page: u64 = 1u64 << (12 + ((cap >> 48) & 0x0F));
		if min_page > host_page {
			return Err(Unusable::PageTooSmall);
		}
		Ok(Capabilities {
			max_entries: mqes + 1,
			// CAP.DSTRD is bits 35:32, as `2^(2 + value)` bytes.
			doorbell_stride: 1u64 << (2 + ((cap >> 32) & 0x0F)),
			// CAP.TO is bits 31:24.
			timeout_500ms: ((cap >> 24) & 0xFF) as u32,
			min_page,
		})
	}

	// Where one queue's doorbell lives. `is_completion` picks the second of the pair.
	//
	// THE STRIDE IS WHY THIS IS A FUNCTION. Every controller this tree has met uses a stride of
	// four, so a driver that hard-coded `0x1000 + qid * 8` would work everywhere it was tested and
	// ring the wrong queue on a controller that reports anything else.
	pub fn doorbell(&self, queue: u16, is_completion: bool) -> u64 {
		let slot = (queue as u64) * 2 + u64::from(is_completion);
		REG_DOORBELL_BASE + slot * self.doorbell_stride
	}

	// How many entries a queue actually gets: what the caller wants, never more than the controller
	// allows, and never zero.
	pub fn queue_entries(&self, wanted: u32) -> u32 {
		wanted.clamp(1, self.max_entries)
	}
}

// ------------------------------------------------------------------------------ completions

// One completion queue entry, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Completion {
	pub command_id: u16,
	pub phase: bool,
	// Status Code Type (3 bits) and Status Code (8 bits), as the register packs them.
	pub status_type: u8,
	pub status_code: u8,
	// The submission queue head the controller has consumed up to.
	pub sq_head: u16,
}

impl Completion {
	pub fn decode(entry: &[u8; CQ_ENTRY_LEN]) -> Completion {
		let mut dw2 = [0u8; 4];
		dw2.copy_from_slice(&entry[8..12]);
		let mut dw3 = [0u8; 4];
		dw3.copy_from_slice(&entry[12..16]);
		let dw2 = u32::from_le_bytes(dw2);
		let dw3 = u32::from_le_bytes(dw3);
		Completion { sq_head: (dw2 & 0xFFFF) as u16, command_id: (dw3 & 0xFFFF) as u16, phase: dw3 & (1 << 16) != 0, status_code: ((dw3 >> 17) & 0xFF) as u8, status_type: ((dw3 >> 25) & 0x07) as u8 }
	}

	// Whether the command succeeded. Both fields must be zero: a non-zero type with a zero code is
	// still a failure, and reading only the code would call it success.
	pub fn succeeded(&self) -> bool {
		self.status_type == 0 && self.status_code == 0
	}
}

// What a completion turned out to be. THE UNEXPECTED ID IS ITS OWN ARM and not an error, because
// the two call for opposite actions: a failed command is reported to the caller that made it, and a
// completion carrying an id nobody is waiting for means the controller and this driver disagree
// about what is outstanding - which is a reason to stop using the queue, not to fail one request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reaped {
	// Nothing there yet: the phase bit still matches the previous pass around the ring.
	Empty,
	// The command this caller was waiting for, and whether it succeeded.
	Mine { succeeded: bool, sq_head: u16 },
	// A completion for a command id that is not outstanding.
	Unexpected { command_id: u16 },
}

// Read one completion slot. `expected_phase` flips every time the ring wraps, which is what tells a
// new entry from the one that was in that slot last time around: the memory is never cleared, so
// "is this entry new" has no other answer.
pub fn reap(entry: &[u8; CQ_ENTRY_LEN], expected_phase: bool, waiting_for: u16) -> Reaped {
	let completion = Completion::decode(entry);
	if completion.phase != expected_phase {
		return Reaped::Empty;
	}
	if completion.command_id != waiting_for {
		return Reaped::Unexpected { command_id: completion.command_id };
	}
	Reaped::Mine { succeeded: completion.succeeded(), sq_head: completion.sq_head }
}

// ------------------------------------------------------------------------ transfers and PRPs

// How a transfer's pages are described to the controller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prp {
	// One page or less: the address alone.
	Single { prp1: u64 },
	// Two pages: the second is given directly rather than through a list.
	Pair { prp1: u64, prp2: u64 },
	// More than two: `prp2` points at a list of the remaining page addresses. `entries` is how many
	// the list holds, which the caller must have room for.
	List { prp1: u64, entries: usize },
}

// Why a transfer cannot be described.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Untransferable {
	// Zero bytes is not a transfer.
	Empty,
	// Longer than the controller's maximum data transfer size.
	TooLarge,
	// More pages than one PRP list page can name, which is the bound this driver chose by carrying
	// exactly one list page.
	TooManyPages,
}

// Describe a physically contiguous transfer of `len` bytes starting at `base`.
//
// THE FIRST ENTRY MAY BE OFFSET INTO ITS PAGE AND THE REST MAY NOT. That asymmetry is the whole of
// PRP arithmetic and the easiest thing to get wrong: a driver that divided `len` by the page size
// would be one page short whenever the buffer did not start on a page boundary, and the missing
// page is the tail of the caller's data.
pub fn prp(base: u64, len: u64, page: u64, most_bytes: u64) -> Result<Prp, Untransferable> {
	if len == 0 {
		return Err(Untransferable::Empty);
	}
	if len > most_bytes {
		return Err(Untransferable::TooLarge);
	}
	let first_page_holds = page - (base & (page - 1));
	if len <= first_page_holds {
		return Ok(Prp::Single { prp1: base });
	}
	let rest = len - first_page_holds;
	// Rounded UP: a partial page is still a page the controller must be given.
	let more = rest.div_ceil(page);
	let second = base + first_page_holds;
	if more == 1 {
		return Ok(Prp::Pair { prp1: base, prp2: second });
	}
	// One list page holds `page / 8` addresses.
	let capacity = (page / 8) as usize;
	if more as usize > capacity {
		return Err(Untransferable::TooManyPages);
	}
	Ok(Prp::List { prp1: base, entries: more as usize })
}

// The address of the n-th page a PRP list must name, for a transfer described by `prp` above.
pub fn list_entry(base: u64, page: u64, index: usize) -> u64 {
	let first_page_holds = page - (base & (page - 1));
	base + first_page_holds + (index as u64) * page
}

// The largest single transfer this controller accepts, from `IDENTIFY`'s `MDTS`.
//
// ZERO MEANS NO LIMIT, which is the specification's encoding and not a zero-length transfer. A
// driver reading it as a size transfers nothing, forever, against every controller that declares
// itself unbounded - and QEMU's model is one of them.
pub fn max_transfer(mdts: u8, min_page: u64, driver_bound: u64) -> u64 {
	if mdts == 0 {
		return driver_bound;
	}
	match min_page.checked_shl(mdts as u32) {
		Some(bytes) => bytes.min(driver_bound),
		None => driver_bound,
	}
}

// ------------------------------------------------------------------------------- namespaces

// Why a namespace is not served, while its siblings still are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unservable {
	// Zero blocks: the namespace id exists but is not attached.
	Empty,
	// The formatted LBA carries metadata. Metadata changes the transfer length per block and this
	// driver does not carry it, so reading such a namespace would return the caller short data with
	// a success status.
	Metadata,
	// End-to-end protection is enabled. The protection bytes travel with the data and are checked by
	// the controller; a driver that ignores them writes blocks the controller then rejects, or
	// worse, accepts with protection information nobody computed.
	Protection,
	// The block size is not a power of two, or is smaller than a sector. Nothing in the block
	// contract above can express it.
	BlockSize,
}

// A namespace this driver will serve.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Namespace {
	pub blocks: u64,
	pub block_bytes: u32,
}

// Decide one namespace from the fields `IDENTIFY NAMESPACE` reported.
//
// `flbas` is the formatted LBA size field, whose low four bits index `lbaf`; `lbaf` is that entry,
// whose bits 23:16 are the block size as a power of two and whose bits 15:0 are the metadata size.
// `dps` is the end-to-end protection settings, and its low three bits select a protection type.
pub fn namespace(blocks: u64, flbas: u8, lbaf: u32, dps: u8) -> Result<Namespace, Unservable> {
	if blocks == 0 {
		return Err(Unservable::Empty);
	}
	if lbaf & 0xFFFF != 0 {
		return Err(Unservable::Metadata);
	}
	if dps & 0x07 != 0 {
		return Err(Unservable::Protection);
	}
	let shift = (lbaf >> 16) & 0xFF;
	// `2^9` is 512, the smallest the block contract can carry, and a shift of 32 or more is not a
	// size at all.
	if !(9..32).contains(&shift) {
		return Err(Unservable::BlockSize);
	}
	let _ = flbas;
	Ok(Namespace { blocks, block_bytes: 1u32 << shift })
}

// Which `lbaf` entry `flbas` selects. The index is the low four bits; bit 4 selects the upper half
// of the format list on controllers that declare more than sixteen, which this driver refuses to
// guess at rather than reading the wrong entry.
pub fn lba_format_index(flbas: u8) -> Option<usize> {
	if flbas & 0x10 != 0 {
		return None;
	}
	Some((flbas & 0x0F) as usize)
}

#[cfg(test)]
mod tests;
