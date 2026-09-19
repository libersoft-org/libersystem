// THE AHCI DECISIONS, WITH NO CONTROLLER BEHIND THEM.
//
// The same split as `drivers::nvme` and for the same reason: the register layouts, the port state
// rules, the scatter-gather arithmetic and the capacity parse are where an AHCI driver is actually
// wrong, and every one of them is a function from numbers a controller reported to an answer with a
// refusal in it. None of it touches MMIO and none of it allocates, so a host test can watch each
// refusal happen rather than a guest having to be booted to find out.
//
// The specification is Intel's AHCI 1.3.1 and the ATA command set; offsets are named so a reader can
// check them against those rather than against this file's confidence.

// HBA registers, at the mapped ABAR base.
pub const REG_CAP: u64 = 0x00; // capabilities
pub const REG_GHC: u64 = 0x04; // global host control
pub const REG_PI: u64 = 0x0C; // ports implemented, one bit per port
pub const REG_CAP2: u64 = 0x24;

// `GHC` bits.
pub const GHC_RESET: u32 = 1 << 0;
pub const GHC_AHCI_ENABLE: u32 = 1 << 31;

// A port's registers live at `0x100 + port * 0x80`.
pub const PORT_BASE: u64 = 0x100;
pub const PORT_STRIDE: u64 = 0x80;
pub const PORT_CLB: u64 = 0x00; // command list base, 64-bit
pub const PORT_FB: u64 = 0x08; // received FIS base, 64-bit
pub const PORT_IS: u64 = 0x10; // interrupt status
pub const PORT_CMD: u64 = 0x18; // command and status
pub const PORT_TFD: u64 = 0x20; // task file data
pub const PORT_SIG: u64 = 0x24; // signature
pub const PORT_SSTS: u64 = 0x28; // SATA status
pub const PORT_SCTL: u64 = 0x2C; // SATA control - where a port reset is asked for
pub const PORT_SERR: u64 = 0x30; // SATA error
pub const PORT_SACT: u64 = 0x34; // SATA active: one bit per OUTSTANDING queued command
pub const PORT_CI: u64 = 0x38; // command issue, one bit per slot

// `PxCMD` bits.
pub const CMD_START: u32 = 1 << 0;
pub const CMD_FIS_RECEIVE_ENABLE: u32 = 1 << 4;
pub const CMD_FIS_RECEIVE_RUNNING: u32 = 1 << 14;
pub const CMD_LIST_RUNNING: u32 = 1 << 15;

// `PxTFD` status bits, which are the ATA status register.
pub const TFD_ERR: u32 = 1 << 0;
pub const TFD_DRQ: u32 = 1 << 3;
pub const TFD_BSY: u32 = 1 << 7;

// ATA commands this driver sends.
pub const ATA_READ_DMA_EXT: u8 = 0x25;
pub const ATA_WRITE_DMA_EXT: u8 = 0x35;
// THE QUEUED PAIR, which are not the same commands with a tag bolted on: their FIELDS MEAN
// DIFFERENT THINGS. See `queued_fis`.
pub const ATA_READ_FPDMA_QUEUED: u8 = 0x60;
pub const ATA_WRITE_FPDMA_QUEUED: u8 = 0x61;
pub const ATA_FLUSH_CACHE_EXT: u8 = 0xEA;
pub const ATA_IDENTIFY: u8 = 0xEC;

// The command list holds this many slots at most; `CAP.NCS` may report fewer.
pub const MAX_SLOTS: u32 = 32;

// A command table's scatter-gather list entry is sixteen bytes and its byte count field is
// zero-based, so one entry carries at most four mebibytes.
pub const PRDT_ENTRY_LEN: usize = 16;
pub const PRDT_MAX_BYTES: u32 = 4 * 1024 * 1024;

// The signature a port reports for what is attached to it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Attached {
	/// An ordinary SATA disk, which is the only thing this driver drives.
	Disk,
	/// An ATAPI device. REFUSED EXPLICITLY rather than half-driven: its command set is packet-based
	/// and nothing here speaks it, and a driver that issued ATA reads to one would get errors it
	/// would report as a failing disk.
	Atapi,
	/// A port multiplier or an enclosure services device, neither of which this driver supports.
	Unsupported,
	/// Nothing is attached.
	None,
}

/// What a port's signature register says is behind it.
pub fn attached(signature: u32) -> Attached {
	match signature {
		0x0000_0101 => Attached::Disk,
		0xEB14_0101 => Attached::Atapi,
		0xFFFF_FFFF | 0 => Attached::None,
		// Port multiplier (0x9669_0101) and enclosure services (0xC33C_0101) land here with
		// everything else this driver has no command set for.
		_ => Attached::Unsupported,
	}
}

/// Whether a port has a device present AND its physical link is up.
///
/// BOTH HALVES, because they are different questions and a port can answer yes to one. `DET` is
/// whether the controller detected a device, `IPM` is whether the interface is in an active power
/// state; a port with a device detected but its link in partial or slumber will not answer a command
/// until it is woken, and a driver that issued one would wait out its whole timeout.
pub fn link_up(ssts: u32) -> bool {
	let det = ssts & 0x0F;
	let ipm = (ssts >> 8) & 0x0F;
	det == 3 && ipm == 1
}

/// The ports this controller implements, as indices, from the `PI` bitmap.
///
/// THE BITMAP IS NOT A COUNT, and this is the trap it sets: `CAP.NP` says how many ports the
/// controller has and `PI` says WHICH of them exist, and they disagree on real hardware - a
/// controller reporting six ports may implement ports 0, 1 and 4. Walking `0..CAP.NP` reads the
/// registers of ports that are not there.
pub fn implemented_ports(pi: u32, most: u32) -> impl Iterator<Item = u32> {
	let bound = most.min(MAX_SLOTS);
	(0..bound).filter(move |port| pi & (1 << port) != 0)
}

/// Where a port's register block starts, relative to the ABAR base.
pub fn port_offset(port: u32) -> u64 {
	PORT_BASE + (port as u64) * PORT_STRIDE
}

/// Why a controller cannot be used at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unusable {
	/// It reports no implemented ports, so there is nothing behind it.
	NoPorts,
	/// It cannot address memory above four gibibytes and this host has put the command list there.
	/// REFUSED RATHER THAN TRUNCATED: a 32-bit controller handed the low half of a 64-bit address
	/// reads somebody else's memory, and writes into it on a read.
	NotSixtyFourBit,
}

/// What `CAP` said, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capabilities {
	/// How many command slots each port has. THE WIRE VALUE IS ZERO-BASED and this is not.
	pub slots: u32,
	/// How many ports the controller reports, also zero-based on the wire.
	pub ports: u32,
	/// Whether it can address memory above four gibibytes.
	pub sixty_four_bit: bool,
	/// Whether the controller supports native command queuing.
	pub queued: bool,
}

impl Capabilities {
	pub fn decode(cap: u32) -> Capabilities {
		Capabilities {
			// CAP.NCS is bits 12:8, zero-based.
			slots: ((cap >> 8) & 0x1F) + 1,
			// CAP.NP is bits 4:0, zero-based.
			ports: (cap & 0x1F) + 1,
			// CAP.S64A is bit 31.
			sixty_four_bit: cap & (1 << 31) != 0,
			// CAP.SNCQ is bit 30.
			queued: cap & (1 << 30) != 0,
		}
	}

	/// Whether this controller can be driven at all, given where the host put its structures.
	pub fn usable(&self, pi: u32, structures_above_4g: bool) -> Result<(), Unusable> {
		if pi == 0 {
			return Err(Unusable::NoPorts);
		}
		if structures_above_4g && !self.sixty_four_bit {
			return Err(Unusable::NotSixtyFourBit);
		}
		Ok(())
	}
}

/// How a command ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
	/// The slot's bit is still set: the controller has not finished.
	Pending,
	/// Finished, and the task file reports no error.
	Done,
	/// The device reported an error, with the task file's status and error bytes.
	Failed { status: u8, error: u8 },
}

/// Read one command's outcome from the port's command-issue register and task file.
///
/// THE TASK FILE IS CHECKED EVEN WHEN THE SLOT CLEARED, which is the half a driver forgets: AHCI
/// clears the command-issue bit when the command COMPLETES, not when it succeeds, so a failed read
/// looks exactly like a finished one to anybody watching only `PxCI`. The error is in `PxTFD`, and a
/// driver that does not read it reports somebody's bad sector as good data.
pub fn outcome(ci: u32, slot: u32, tfd: u32) -> Outcome {
	if ci & (1 << slot) != 0 {
		return Outcome::Pending;
	}
	if tfd & TFD_ERR != 0 {
		return Outcome::Failed { status: (tfd & 0xFF) as u8, error: ((tfd >> 8) & 0xFF) as u8 };
	}
	Outcome::Done
}

/// The four fields a queued command FIS carries that an ordinary one carries differently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct QueuedFis {
	/// FIS byte 3 - the low byte of the SECTOR COUNT, not a feature.
	pub features: u8,
	/// FIS byte 11 - the high byte of the sector count.
	pub features_exp: u8,
	/// FIS byte 12 - the TAG, in bits 7:3, and nothing in bits 2:0.
	pub count: u8,
	/// FIS byte 7 - LBA mode, and FUA when the caller asked for it.
	pub device: u8,
}

/// The field mapping of a queued command, which is where a first NCQ driver is wrong.
///
/// THE SECTOR COUNT MOVES INTO THE FEATURES FIELD AND THE TAG TAKES ITS PLACE. `READ DMA EXT` puts
/// the block count in the sector-count field and nothing in features; `READ FPDMA QUEUED` puts the
/// COUNT in features and features_exp, and the sector-count field carries the tag SHIFTED LEFT BY
/// THREE. A driver that filled these the ordinary way asks for tag 0 of a transfer whose length is
/// zero - which on most controllers means the maximum - and writes 65,536 sectors' worth of somebody
/// else's disk into a buffer sized for eight.
///
/// AND THE TAG IS NOT THE SLOT NUMBER BY ACCIDENT. AHCI requires the queued command to be issued in
/// the command slot whose number EQUALS its tag, so the two are one number and this function takes
/// it once.
///
/// BIT 6 OF THE DEVICE REGISTER IS ALWAYS SET, as for every other command here: it is what says the
/// address is a block number. FUA is bit 7 and is the caller's.
pub fn queued_fis(sectors: u32, tag: u32, fua: bool) -> QueuedFis {
	QueuedFis { features: sectors as u8, features_exp: (sectors >> 8) as u8, count: ((tag & 0x1F) << 3) as u8, device: (1 << 6) | if fua { 1 << 7 } else { 0 } }
}

/// How a QUEUED command ended, read from `PxSACT`, `PxCI` and the task file.
///
/// A QUEUED COMMAND IS OUTSTANDING WHILE ITS BIT IS SET IN `PxSACT`, AND NOT IN `PxCI`. The
/// controller clears `PxCI` when it has SENT the command to the device, which for a queued command
/// happens immediately and means nothing about the data - so a driver watching `PxCI` reports every
/// queued read complete the moment it was issued, and hands back a buffer the disk has not written.
/// `PxSACT` is what the device clears, through the Set Device Bits FIS, when the transfer is done.
///
/// AND AN ERROR STOPS THE WHOLE QUEUE, which is the other half a non-queued driver does not have to
/// think about: a failed queued command sets `TFD.ERR` and the port stops accepting, so every other
/// outstanding tag is abandoned rather than failed individually. This answers `Failed` for the tag
/// asked about, and the caller's business is that the rest are gone too.
pub fn queued_outcome(sact: u32, slot: u32, tfd: u32) -> Outcome {
	if tfd & TFD_ERR != 0 {
		return Outcome::Failed { status: (tfd & 0xFF) as u8, error: ((tfd >> 8) & 0xFF) as u8 };
	}
	if sact & (1 << slot) != 0 {
		return Outcome::Pending;
	}
	Outcome::Done
}

/// Why a transfer cannot be described.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Untransferable {
	/// Zero bytes is not a transfer.
	Empty,
	/// An odd length. A SATA transfer moves whole words, and the scatter-gather byte count's low bit
	/// is reserved and must be one, so an odd count cannot be expressed at all.
	OddLength,
	/// More entries than the command table this driver allocates can hold.
	TooManyEntries,
}

/// How many scatter-gather entries a physically contiguous transfer of `len` bytes needs.
pub fn prdt_entries(len: u64, most_entries: usize) -> Result<usize, Untransferable> {
	if len == 0 {
		return Err(Untransferable::Empty);
	}
	if len % 2 != 0 {
		return Err(Untransferable::OddLength);
	}
	let entries = len.div_ceil(PRDT_MAX_BYTES as u64) as usize;
	if entries > most_entries {
		return Err(Untransferable::TooManyEntries);
	}
	Ok(entries)
}

/// The byte count field for one scatter-gather entry: what it carries, minus one, because the field
/// is zero-based and a value of zero would be one byte rather than none.
pub fn prdt_count(bytes: u32) -> u32 {
	bytes.saturating_sub(1)
}

/// The bytes the n-th entry of such a transfer carries.
pub fn prdt_span(len: u64, index: usize) -> u32 {
	let consumed = (index as u64) * PRDT_MAX_BYTES as u64;
	let left = len.saturating_sub(consumed);
	left.min(PRDT_MAX_BYTES as u64) as u32
}

/// Why a disk is not served.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unservable {
	/// It reports no sectors.
	Empty,
	/// It does not support 48-bit addressing, and the EXT commands this driver sends are the 48-bit
	/// ones. Refused rather than silently addressed with 28 bits, which would cap the disk at 128
	/// gibibytes without saying so.
	NoLbaExt,
}

/// A disk this driver will serve.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Disk {
	pub sectors: u64,
	pub sector_bytes: u32,
}

/// Read what `IDENTIFY DEVICE` reported. `words` is the 256-word answer.
///
/// THE 48-BIT COUNT IS AT WORDS 100..104 and the 28-bit one at 60..62, and which is valid is word
/// 83's bit 10. A driver that read the 28-bit field on a large disk gets its size modulo 128 GiB and
/// writes past the end believing it is inside.
pub fn identify(words: &[u16]) -> Result<Disk, Unservable> {
	if words.len() < 104 {
		return Err(Unservable::Empty);
	}
	if words[83] & (1 << 10) == 0 {
		return Err(Unservable::NoLbaExt);
	}
	let sectors = (words[100] as u64) | ((words[101] as u64) << 16) | ((words[102] as u64) << 32) | ((words[103] as u64) << 48);
	if sectors == 0 {
		return Err(Unservable::Empty);
	}
	// A logical sector is 512 bytes unless word 106 says the device has a longer one; bit 14 set and
	// bit 15 clear is what makes that word meaningful at all, and bit 12 is what says the size is
	// not 512.
	let sector_bytes = if words.len() > 118 && words[106] & 0xC000 == 0x4000 && words[106] & (1 << 12) != 0 {
		let words_per_sector = (words[117] as u32) | ((words[118] as u32) << 16);
		words_per_sector.saturating_mul(2).max(512)
	} else {
		512
	};
	Ok(Disk { sectors, sector_bytes })
}

#[cfg(test)]
mod tests;
