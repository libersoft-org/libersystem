//! partition - what a raw disk actually IS, decided before anything is written to it.
//!
//! One question, asked once, by the only caller that ever needs it: StorageService, before
//! it decides whether the disk in front of it may be formatted. The answer used to be an
//! `Option` - "there is a LiberFS partition here" or nothing - and "nothing" was read as
//! licence to lay a filesystem over the whole device from sector zero. That is where a
//! protective MBR, a GPT header and a partition entry array live.
//!
//! The rule was established for the mount ("I could not tell" may not be read as "there
//! is nothing here") and then applied to a GPT that IS recognised. The negative case -
//! LBA 1 does not begin with `EFI PART` - stayed exactly as dangerous as it had been: an
//! ordinary MBR-partitioned disk, a hybrid MBR, a GPT whose signature is damaged while its
//! backup is intact, a superfloppy carrying FAT or ext4 at LBA 0, and any disk holding
//! foreign data all answered the same word, and the service formatted every one of them.
//!
//! So the answers here are the whole vocabulary, and exactly ONE of them - [`Disk::Blank`] -
//! licenses formatting a whole device. Getting it requires reading BOTH LBA 0 and LBA 1 and
//! finding: no active MBR partition entry, no protective MBR, no GPT, and no recognisable
//! foreign filesystem. Everything else names what was found, and the caller changes nothing.
//!
//! The GPT that IS parsed is verified rather than believed: both CRC32s, every header LBA
//! relation, the entry array's own span, the partition against the device's real capacity,
//! and checked arithmetic throughout - because those numbers decide WHERE the caller may
//! write. When the primary GPT does not verify, the backup at the last sector is consulted
//! before anything is called damaged.
//!
//! The crate is `no_std` and reads through a caller-supplied [`Sectors`], so the same code
//! answers for a virtio-blk disk behind a block service and for a `Vec` of sectors in a test.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

// The sector size this crate speaks. GPT is defined in terms of the medium's logical block
// size; 512 is what every disk this system drives reports, and a 4Kn disk would need the
// size threaded through from the block service rather than assumed here.
pub const SECTOR_SIZE: usize = 512;

// The LiberFS GPT partition type GUID, 4C424653-0001-4000-8000-4C6962657246 ("LBFS" /
// "LiberF"), in its on-disk byte order (the first three groups little-endian, the rest as
// written). A disk partitioned by any other system marks a LiberFS volume with this GUID
// and the volume is found by it.
pub const LIBERFS_TYPE_GUID: [u8; 16] = [0x53, 0x46, 0x42, 0x4C, 0x01, 0x00, 0x00, 0x40, 0x80, 0x00, 0x4C, 0x69, 0x62, 0x65, 0x72, 0x46];

// An all-zero type GUID marks an UNUSED entry array slot. It is not a partition and it is
// not evidence of one.
const UNUSED_TYPE_GUID: [u8; 16] = [0; 16];

// A LiberFS superblock's magic. Duplicated from the filesystem crate rather than depended
// on: this crate answers what is on a disk, and taking a dependency on the filesystem to
// recognise eight bytes would invert that - the probe runs BEFORE anything decides which
// filesystem to reach for.
const LIBERFS_MAGIC: &[u8; 8] = b"LIBERFS1";

// The smallest partition worth mounting, in sectors: 16 LiberFS blocks (two superblock
// slots, the root leaf, and room to breathe). A GPT entry below this is ignored - the
// disk's content must never be able to kill the storage service by making a format fail.
pub const MIN_PARTITION_SECTORS: u64 = 16 * 8;

// The GPT header's fixed offsets, all little-endian.
const HDR_SIGNATURE: usize = 0;
const HDR_REVISION: usize = 8;
const HDR_SIZE: usize = 12;
const HDR_CRC: usize = 16;
// Revision 1.0 reserves the four bytes between the header CRC and `MyLBA`. Zero, always.
const HDR_RESERVED: usize = 20;
const HDR_CURRENT_LBA: usize = 24;
const HDR_BACKUP_LBA: usize = 32;
const HDR_FIRST_USABLE: usize = 40;
const HDR_LAST_USABLE: usize = 48;
// The disk's own identity. Not consulted to decide anything about a partition - it is here so the
// two copies of the table can be compared as descriptions of ONE disk.
const HDR_DISK_GUID: usize = 56;
const HDR_ENTRIES_LBA: usize = 72;
const HDR_NUM_ENTRIES: usize = 80;
const HDR_ENTRY_SIZE: usize = 84;
const HDR_ENTRIES_CRC: usize = 88;

// The UEFI-mandated minimum header size, and the largest this build will read (one sector -
// a header claiming more than the block it lives in is not a header).
const HDR_SIZE_MIN: usize = 92;

// A partition entry's fixed offsets.
const ENT_TYPE_GUID: usize = 0;
// The partition's own identity, as opposed to its type. Two used entries may not share one.
const ENT_UNIQUE_GUID: usize = 16;
const ENT_FIRST_LBA: usize = 32;
const ENT_LAST_LBA: usize = 40;

// The most entry-array slots this build will walk. The UEFI default array is 128; the cap
// keeps a header claiming four billion entries from turning the probe into a disk scan.
const MAX_ENTRIES: u64 = 512;

// A source of 512-byte sectors, by absolute LBA.
//
// `false` means the device did not answer, which is never the same claim as "the sector is
// blank" - the whole point of this crate is that those two must not collapse into one word.
pub trait Sectors {
	// Read one sector into `buf` (exactly `SECTOR_SIZE` bytes). False on I/O failure.
	fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool;

	// The device's capacity in sectors, or None when the device cannot say.
	//
	// A partition table's numbers decide where the caller may WRITE, and the only thing
	// that bounds them from outside is the medium itself. A device that will not answer
	// leaves every span unbounded above, and the probe says so rather than guessing.
	fn capacity(&mut self) -> Option<u64>;
}

// What the disk turned out to be.
//
// NONE of them authorises a write. They are kept separate rather than collapsed into one refusal
// because the operator's next move differs for each: a foreign filesystem is data to copy off, an
// MBR is a table to convert, a corrupt GPT is a table to repair, an I/O failure is a cable to
// check, and a blank disk is one to write a volume image to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disk {
	// The disk carries no partition table and every byte of it this build looked at is ZERO.
	//
	// A DIAGNOSIS, and nothing more. It used to be the one answer that licensed laying a
	// filesystem over a whole device, and that is gone: StorageService mounts what is on a disk
	// and never creates. Formatting is `mkpackages` writing a verified volume image, on a machine
	// where somebody is standing.
	//
	// The distinction from `UnknownData` is still worth keeping, because the two send an operator
	// somewhere different: one says "this disk is empty, you probably meant to write a volume to
	// it", the other says "there is something here, find out what before you do".
	//
	// And it is still only as good as where it looked. There is no complete list of filesystem
	// signatures and there never will be, so there is no complete list of PLACES either: a disk
	// zero across every sector this build reads and full from LBA 2048 answers `Blank`. That was a
	// data-loss hazard while it authorised a write. As a diagnosis it is a hint that can be wrong,
	// which is all it now claims to be.
	Blank,
	// The disk carries no partition table, no filesystem this build recognises, and bytes
	// that are not zero. Something is on it and this build cannot say what.
	//
	// The whole reason this variant exists: a false positive here costs a boot, and a false
	// negative costs the disk.
	UnknownData,
	// No partition table, and LBA 0 carries a LiberFS superblock: a volume written straight
	// onto the medium by this system, which is what the fixed whole-device layout produces.
	//
	// Distinct from `Blank` on purpose. Both mean "the whole device is the container", so
	// both reach the same mount - but only one of them describes a disk with nothing on it,
	// and an answer that cannot tell those apart is how this went wrong the first time.
	// What kept the existing volume safe USED TO BE the mount, which formatted only on
	// `Unformatted`. Nothing formats at boot any more - the capability was removed rather than
	// gated - so this answer no longer authorises a write at all. It is a description of what the
	// medium looks like, and the distinction is kept because an answer that cannot tell an empty
	// disk from a formatted one is how this went wrong the first time.
	LiberFsWholeDevice,
	// A GPT naming a usable LiberFS partition, as (first LBA, last LBA) inclusive.
	LiberFs { first: u64, last: u64 },
	// A GPT, verified end to end, that names no usable LiberFS partition. The disk belongs
	// to something else.
	GptWithoutLiberFs,
	// A verified GPT naming MORE THAN ONE usable LiberFS partition, with nothing to choose between
	// them. Not `LiberFs`, because this answer authorises a writable mount and the identity of what
	// would be mounted is decided by entry order - which a partitioning tool or a clone may change
	// without touching a byte of either filesystem. Selecting one needs a configured unique GUID or
	// another boot policy; until there is one, the honest answer is that the disk is ambiguous.
	AmbiguousLiberFs,
	// An MBR partition table (or a protective MBR whose GPT did not verify - see
	// `CorruptGpt` for the case where the signature was there). Not a blank disk.
	MbrWithoutLiberFs,
	// BOTH tables, both describing the disk: a hybrid, where a protective 0xEE entry sits beside
	// real MBR partitions and a GPT covers the same medium. Some install media and some
	// dual-booted disks are laid out this way.
	//
	// Its own answer rather than "read the GPT and ignore the rest", which is what this build did:
	// two tables describing one disk disagree by construction, and nothing here can say which the
	// owner meant. A LiberFS entry in the GPT may sit on top of an MBR partition's range and no
	// check in either table would notice. This answer authorises a WRITE, so the honest response to
	// two answers is to give neither.
	HybridMbrAndGpt,
	// A GPT signature, or a protective MBR, followed by a table this build cannot believe -
	// neither the primary nor the backup verified. Emphatically not a blank disk: the
	// partitions are probably all still there, and formatting is the one thing that would
	// finish them off.
	CorruptGpt,
	// No partition table, and LBA 0 carries a filesystem this build recognises - a
	// "superfloppy" formatted straight onto the medium. `name` is what it looks like,
	// for the operator's benefit.
	ForeignFilesystem { name: &'static str },
	// The device did not answer. Nothing at all is known about what is on it.
	Io,
	// This machine would not give the probe the memory to verify a table. Says nothing about
	// the disk, which is exactly why it is its own answer rather than folded into `CorruptGpt`
	// - one of those means "repair the table" and the other means "the disk is fine".
	NoMemory,
}

// Read the disk and say what it is. Nothing is written and nothing is assumed; every answer
// is backed by sectors that were actually read.
pub fn probe(dev: &mut impl Sectors) -> Disk {
	let mut lba0 = [0u8; SECTOR_SIZE];
	let mut lba1 = [0u8; SECTOR_SIZE];
	// BOTH sectors, before anything is decided. The old probe read LBA 1 alone, so an MBR
	// disk and a blank one were the same observation - it never looked at the sector where
	// the difference lives.
	if !dev.read(0, &mut lba0) || !dev.read(1, &mut lba1) {
		return Disk::Io;
	}

	let mbr = classify_mbr(&lba0, dev.capacity());
	// A real hybrid is decided BEFORE the GPT is read. `classify_mbr` used to be computed here and
	// then thrown away whenever LBA 1 carried a signature, so a disk with real MBR partitions AND a
	// valid GPT was decided entirely by the GPT.
	if mbr == Mbr::Partitioned && &lba1[HDR_SIGNATURE..HDR_SIGNATURE + 8] == b"EFI PART" {
		return Disk::HybridMbrAndGpt;
	}
	// AND A GPT DISK MUST CARRY A PROTECTIVE MBR, which the primary path did not require at all: an
	// `EFI PART` signature at LBA 1 was enough on its own, so a disk whose LBA 0 had been erased -
	// or whose `0xEE` entry protects nothing - was reported as an ordinary, fully verified GPT. The
	// protective MBR is the reason legacy tooling does not see a GPT disk as empty space, so its
	// absence is exactly the state in which something else may already have written there.
	//
	// `CorruptGpt` rather than a softer answer, because this result authorises a WRITE: the caller
	// mounts what `probe` names, and "the metadata is not the shape it must be" is the one thing
	// that has to stop that.
	if &lba1[HDR_SIGNATURE..HDR_SIGNATURE + 8] == b"EFI PART" && !matches!(mbr, Mbr::Protective) {
		return Disk::CorruptGpt;
	}
	if &lba1[HDR_SIGNATURE..HDR_SIGNATURE + 8] == b"EFI PART" {
		let counterpart = primary_counterpart(dev);
		return match read_gpt(dev, &lba1, 1, counterpart) {
			// The primary verified, and the backup is consulted anyway.
			//
			// It used not to be: a verified primary was acted on and the other copy never read. Two
			// individually-valid tables that CONTRADICT each other were therefore not an ambiguity,
			// and this answer authorises a format. Agreement between the copies is cheap evidence,
			// and its absence is exactly the sort of thing that should stop a write.
			Ok(gpt) => match backup_gpt(dev) {
				Ok(backup) if copies_agree(&gpt, &backup) => {
					let companion = companion_entries(&gpt, Some(&backup));
					find_liberfs(&gpt, companion)
				}
				// The backup is unreadable or damaged: the primary stands on its own, which is what
				// a backup being a BACKUP means. Only a backup that verifies and disagrees is a
				// reason to refuse.
				Err(_) => {
					let companion = companion_entries(&gpt, None);
					find_liberfs(&gpt, companion)
				}
				Ok(_) => Disk::CorruptGpt,
			},
			Err(Fault::NoMemory) => Disk::NoMemory,
			Err(Fault::Io) => Disk::Io,
			// the signature is there and the table is not believable. Consult the backup
			// before condemning it: a damaged primary header with an intact backup is the
			// case UEFI defines a backup FOR.
			Err(Fault::Unusable) => match backup_gpt(dev) {
				Ok(gpt) => {
					let companion = companion_entries(&gpt, None);
					find_liberfs(&gpt, companion)
				}
				Err(Fault::NoMemory) => Disk::NoMemory,
				Err(Fault::Io) => Disk::Io,
				Err(Fault::Unusable) => Disk::CorruptGpt,
			},
		};
	}
	// no GPT signature at LBA 1. A protective MBR says one is SUPPOSED to be there, so its
	// absence is damage rather than a plain MBR disk - and the backup is where to look.
	if mbr == Mbr::Protective {
		return match backup_gpt(dev) {
			Ok(gpt) => {
				let companion = companion_entries(&gpt, None);
				find_liberfs(&gpt, companion)
			}
			Err(Fault::NoMemory) => Disk::NoMemory,
			Err(Fault::Io) => Disk::Io,
			Err(Fault::Unusable) => Disk::CorruptGpt,
		};
	}
	if mbr == Mbr::Partitioned {
		return Disk::MbrWithoutLiberFs;
	}
	// no table of either kind: the disk may still carry a filesystem laid straight onto the
	// medium, which no partition-table check can see.
	if &lba0[0..8] == LIBERFS_MAGIC {
		return Disk::LiberFsWholeDevice;
	}
	if let Some(name) = foreign_filesystem(&lba0) {
		return Disk::ForeignFilesystem { name };
	}
	// Nothing was recognised, which is not the same claim as nothing being there. What decides
	// it is whether the medium is ZERO where anything at all would announce itself.
	// A whole-device format should know how large the device is.
	//
	// The GPT path already refuses a device that will not report its size, because every span in a
	// table is bounded from outside by the medium alone. The blank path did not, and the asymmetry
	// was the wrong way round: the GPT case merely declines to mount, and this one licenses writing
	// over the whole disk. A device that cannot say how big it is cannot be shown to be empty.
	if dev.capacity().is_none() {
		return Disk::UnknownData;
	}
	match scanned_blank(dev, &lba0, &lba1) {
		Some(true) => Disk::Blank,
		Some(false) => Disk::UnknownData,
		None => Disk::Io,
	}
}

// The span a blank disk has to be zero across, in sectors from the start: the two table
// sectors, then every announcement point up to and including 64 KiB.
//
// It covers, in order: the MBR and GPT header sectors; the ext4 / xfs / btrfs-adjacent
// superblock region at 1 KiB; the swap signature near 4 KiB; the ISO9660 and UDF volume
// descriptors at 32 KiB; and the btrfs superblock at 64 KiB. That is 129 single-sector reads,
// once, at boot - the cost of not formatting somebody's disk.
const BLANK_SCAN_SECTORS: u64 = 129;

// And the far ones, which no contiguous prefix of a sane length would reach: the ZFS labels,
// which sit at 256 KiB and 512 KiB from the start of the device.
const BLANK_FAR_PROBES: [u64; 2] = [512, 1024];

// Is every byte this build looks at zero? None if the device stopped answering.
//
// A sector past the end of the medium is not a failure - a device smaller than the scan simply
// has nothing there to look at - so the scan is bounded by the reported capacity when there is
// one, and a read that fails beyond it ends the scan rather than condemning the disk.
fn scanned_blank(dev: &mut impl Sectors, lba0: &[u8], lba1: &[u8]) -> Option<bool> {
	if lba0.iter().any(|&b| b != 0) || lba1.iter().any(|&b| b != 0) {
		return Some(false);
	}
	let capacity = dev.capacity();
	let mut sector = [0u8; SECTOR_SIZE];
	for lba in (2..BLANK_SCAN_SECTORS).chain(BLANK_FAR_PROBES) {
		// a sector the medium does not have is nothing to object to.
		if capacity.is_some_and(|c| lba >= c) {
			continue;
		}
		if !dev.read(lba, &mut sector) {
			// A device that reported a capacity and then refused a sector inside it is
			// failing, and this scan is deciding whether the disk may be written over -
			// so it says so rather than deciding on what it managed to read. A device that
			// would not report a capacity at all cannot be bounded, and a refusal there is
			// taken as the end of the medium.
			return if capacity.is_none() { Some(true) } else { None };
		}
		if sector.iter().any(|&b| b != 0) {
			return Some(false);
		}
	}
	Some(true)
}

// What LBA 0's partition table looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mbr {
	// No boot signature, or a boot signature with no usable entry: nothing claims the disk.
	Absent,
	// One entry of type 0xEE spanning the disk: the protective MBR a GPT disk carries so
	// that MBR-only software sees the disk as fully allocated.
	Protective,
	// At least one entry naming a real partition.
	Partitioned,
	// A boot signature and a `0xEE` entry that is not a protective MBR: more than one of them, or
	// one that does not start at LBA 1 and cover the disk. Distinct from `Absent` because "nothing
	// claims this disk" and "something claims it and the claim is malformed" are different facts,
	// and only the second is evidence of damage.
	Malformed,
}

// Classify LBA 0's MBR partition table. A hybrid MBR (a protective 0xEE entry BESIDE real
// ones) reads as `Partitioned`, which is the safe direction: it says a table is there.
fn classify_mbr(lba0: &[u8], capacity: Option<u64>) -> Mbr {
	if lba0[510] != 0x55 || lba0[511] != 0xAA {
		return Mbr::Absent;
	}
	let mut protective = 0usize;
	let mut malformed_protective = false;
	let mut real = false;
	for i in 0..4 {
		let e = &lba0[446 + i * 16..446 + (i + 1) * 16];
		let kind = e[4];
		let first = u32::from_le_bytes(e[8..12].try_into().unwrap());
		let sectors = u32::from_le_bytes(e[12..16].try_into().unwrap());
		// an empty slot is type 0 with no length; either alone is enough to skip it.
		if kind == 0 || sectors == 0 {
			continue;
		}
		if kind == 0xEE {
			protective += 1;
			// A PROTECTIVE ENTRY HAS A SHAPE, and `kind == 0xEE` was the whole of the test. UEFI
			// fixes it: it starts at LBA 1 and spans the rest of the disk, which is what makes
			// MBR-only software see the whole disk as taken - the one job it exists for. An entry at
			// LBA 1234 spanning one sector protects nothing, and reading it as proof that a GPT is
			// expected reads a damaged table as a healthy one.
			//
			// COVERAGE RATHER THAN AN EXACT LENGTH. The specification's value is `disk - 1` and the
			// convention when that does not fit 32 bits is `0xFFFFFFFF`, but tools differ in the last
			// sector and a reader that demanded one exact number would refuse disks that are
			// perfectly protected. What has to hold is that nothing past the entry looks free.
			let covers = sectors == u32::MAX || capacity.is_none_or(|c| u64::from(sectors) + 1 >= c);
			if first != 1 || !covers {
				malformed_protective = true;
			}
		} else {
			real = true;
		}
	}
	match (real, protective, malformed_protective) {
		(true, _, _) => Mbr::Partitioned,
		// More than one `0xEE` entry, or one that does not cover the disk from LBA 1, is not a
		// protective MBR. `Malformed` rather than `Absent`, because the difference matters to the
		// caller: nothing claims the disk, versus something claims it and is wrong.
		(false, 1, false) => Mbr::Protective,
		(false, 0, _) => Mbr::Absent,
		(false, _, _) => Mbr::Malformed,
	}
}

// A GPT header this build is willing to act on: only the fields that decide where a caller
// may write, and only once every one of them has been checked against the others.
struct Gpt {
	// the entry array as it was read to verify its CRC. Kept rather than re-read: the bytes
	// the spans come from must be the same bytes the checksum was computed over, or the
	// verification proves nothing about what the walk below acts on.
	entries: Vec<u8>,
	entry_size: usize,
	num_entries: u64,
	// The disk this header says it describes. Compared between the primary and the backup and
	// otherwise unused: two copies that disagree about which disk they are on are not two copies.
	disk_guid: [u8; 16],
	first_usable: u64,
	last_usable: u64,
	// the header's own sector and the entry array's span, so a partition entry that
	// overlaps the metadata that names it can be refused.
	header_lba: u64,
	backup_lba: u64,
	entries_lba: u64,
	entries_last_lba: u64,
}

// Why a GPT could not be acted on. The two are kept apart because they are claims about
// different things: one is about the disk, the other about this machine - and answering
// "your partition table is damaged" when the truth is "I could not allocate 16 KiB" sends an
// operator to the wrong component. LiberFS learned the same lesson as `MountError::NoMemory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
	// The table is not one this build can believe.
	Unusable,
	// The table might be fine; the memory to verify it was not there.
	NoMemory,
	// The table might be fine; the DEVICE would not give it back. Kept apart from `Unusable`
	// because the two send an operator to different places: one means repair the table, the other
	// means check the cable. Nothing is written either way, so this is diagnosis rather than
	// safety - which is the only reason it went unnoticed.
	Io,
}

// A zeroed buffer of `len`, or `NoMemory` if the machine will not give it.
//
// This crate exists partly so that a hostile disk cannot take StorageService down, and it was
// doing its own allocation with `vec![0u8; n]` - which ABORTS the process when the allocator
// refuses. The size is bounded (`MAX_ENTRIES` and the entry-size ceiling cap the array at about
// 256 KiB), so it is not an attacker's number without limit; it is still an allocation on a
// path whose whole point is to report rather than die.
fn try_zeroed(len: usize) -> Result<Vec<u8>, Fault> {
	let mut v: Vec<u8> = Vec::new();
	v.try_reserve_exact(len).map_err(|_| Fault::NoMemory)?;
	v.resize(len, 0);
	Ok(v)
}

// Verify a GPT header block and the entry array it points at, or a `Fault` saying which kind of
// answer this is.
//
// `at` is the LBA the header was actually READ FROM, and `counterpart` the LBA the other copy must
// live at. Without them a header is only checked against itself, and a header's account of its own
// position is exactly the field an attacker sets: a correctly-checksummed header physically at LBA
// 1 claiming `current_lba = 65000` with a `first_usable` of 1 passes every relation between its own
// fields, and a LiberFS entry may then start at LBA 1 - the sector the header is sitting in. The
// mount finds no superblock there and, in the system this was written against, the service formatted
// over the table. Format-on-boot has since been removed, so the ending is now a refused mount rather
// than a destroyed partition table - and the header is still lying about where it is, which is the
// thing this crate was written to close.
//
// The old probe checked the signature, that `entry_size` was a power of two in range, that
// `num_entries` was non-zero, and then trusted the rest. It checked NEITHER CRC, no header
// LBA relation, and nothing against the device's capacity - so an entry with the LiberFS
// type GUID, `first = 1` and `last = 100000` passed every test there was, and the mount then
// targeted LBA 1, which is the primary GPT header itself. Finding no superblock there, the
// service formatted on top of the partition table that had named the partition.
fn read_gpt(dev: &mut impl Sectors, header: &[u8], at: u64, counterpart: u64) -> Result<Gpt, Fault> {
	if &header[HDR_SIGNATURE..HDR_SIGNATURE + 8] != b"EFI PART" {
		return Err(Fault::Unusable);
	}
	// revision 1.0 is what every GPT in existence carries; a different one is a format this
	// build has not been told how to read.
	if u32::from_le_bytes(header[HDR_REVISION..HDR_REVISION + 4].try_into().ok().ok_or(Fault::Unusable)?) != 0x0001_0000 {
		return Err(Fault::Unusable);
	}
	let header_size = u32::from_le_bytes(header[HDR_SIZE..HDR_SIZE + 4].try_into().ok().ok_or(Fault::Unusable)?) as usize;
	if header_size < HDR_SIZE_MIN || header_size > SECTOR_SIZE {
		return Err(Fault::Unusable);
	}
	// THE RESERVED FIELD IS RESERVED, and nothing looked at it. A producer may put anything in a
	// field no reader checks, and then it is not reserved - it is a place to hide a byte that two
	// tools disagree about. These are FORMAT invariants and not checksum ones: the CRC is computed
	// over whatever is there, so a nonconforming value carries a perfectly correct checksum.
	//
	// Revision 1.0's reserved word sits between the header CRC and `MyLBA`, and everything from
	// `HeaderSize` to the end of the logical block must be zero too - that tail is not covered by
	// the CRC at all, so it is the one place a byte can differ between two readings of the same
	// "verified" header.
	if header[HDR_RESERVED..HDR_RESERVED + 4] != [0, 0, 0, 0] {
		return Err(Fault::Unusable);
	}
	if header[header_size..].iter().any(|&b| b != 0) {
		return Err(Fault::Unusable);
	}
	// the header's own CRC32, computed over `header_size` bytes with the CRC field zeroed.
	// This is the check that makes every field below worth reading at all.
	let stored = u32::from_le_bytes(header[HDR_CRC..HDR_CRC + 4].try_into().ok().ok_or(Fault::Unusable)?);
	let mut probe = try_zeroed(header_size)?;
	probe.copy_from_slice(&header[..header_size]);
	probe[HDR_CRC..HDR_CRC + 4].fill(0);
	if crc32(&probe) != stored {
		return Err(Fault::Unusable);
	}

	let header_lba = u64::from_le_bytes(header[HDR_CURRENT_LBA..HDR_CURRENT_LBA + 8].try_into().ok().ok_or(Fault::Unusable)?);
	let backup_lba = u64::from_le_bytes(header[HDR_BACKUP_LBA..HDR_BACKUP_LBA + 8].try_into().ok().ok_or(Fault::Unusable)?);
	let first_usable = u64::from_le_bytes(header[HDR_FIRST_USABLE..HDR_FIRST_USABLE + 8].try_into().ok().ok_or(Fault::Unusable)?);
	let last_usable = u64::from_le_bytes(header[HDR_LAST_USABLE..HDR_LAST_USABLE + 8].try_into().ok().ok_or(Fault::Unusable)?);
	let entries_lba = u64::from_le_bytes(header[HDR_ENTRIES_LBA..HDR_ENTRIES_LBA + 8].try_into().ok().ok_or(Fault::Unusable)?);
	let num_entries = u32::from_le_bytes(header[HDR_NUM_ENTRIES..HDR_NUM_ENTRIES + 4].try_into().ok().ok_or(Fault::Unusable)?) as u64;
	let entry_size = u32::from_le_bytes(header[HDR_ENTRY_SIZE..HDR_ENTRY_SIZE + 4].try_into().ok().ok_or(Fault::Unusable)?) as usize;

	// the relations BETWEEN the fields, which a CRC cannot speak to: a header may be
	// perfectly intact and still describe a disk that cannot exist.
	// LBA 0 is the protective MBR's sector by spec, so neither copy of the header can be
	// there, and the two copies cannot be the same sector.
	if header_lba == backup_lba || header_lba == 0 || backup_lba == 0 {
		return Err(Fault::Unusable);
	}
	// And the table has to agree with the medium about where it is. These four are what tie a
	// header to a disk rather than to itself: it sits where it says it sits, and it names the other
	// copy where the other copy actually is.
	if header_lba != at || backup_lba != counterpart {
		return Err(Fault::Unusable);
	}
	if first_usable == 0 || last_usable < first_usable {
		return Err(Fault::Unusable);
	}
	// the UEFI spec puts both copies of the metadata OUTSIDE the usable range, which is what
	// makes "a partition inside the usable range cannot overlap the tables" true.
	if header_lba >= first_usable && header_lba <= last_usable {
		return Err(Fault::Unusable);
	}
	if backup_lba >= first_usable && backup_lba <= last_usable {
		return Err(Fault::Unusable);
	}
	// the entry array: a power-of-two width no smaller than the 128 bytes the spec fixes,
	// at least one entry, and a span that fits in the address space.
	if entry_size < 128 || entry_size > SECTOR_SIZE || !entry_size.is_power_of_two() {
		return Err(Fault::Unusable);
	}
	if num_entries == 0 || entries_lba == 0 {
		return Err(Fault::Unusable);
	}
	// checked throughout: `num_entries * entry_size` is two numbers off the medium
	// multiplied together, and the product decides how far the array reaches.
	let array_bytes = num_entries.checked_mul(entry_size as u64).ok_or(Fault::Unusable)?;
	let array_sectors = array_bytes.div_ceil(SECTOR_SIZE as u64);
	let entries_last_lba = entries_lba.checked_add(array_sectors).and_then(|n| n.checked_sub(1)).ok_or(Fault::Unusable)?;
	// and the array is metadata too, so it may not sit in the usable range either.
	if entries_last_lba >= first_usable && entries_lba <= last_usable {
		return Err(Fault::Unusable);
	}
	// AND IT BELONGS TO ITS OWN COPY, which "outside the usable range" does not say.
	//
	// Outside that range means one of two places - below `first_usable` or above `last_usable` - and
	// this accepted either for either header. So a PRIMARY header could point its array into the
	// BACKUP metadata region at the far end of the disk, or lay it across its own sector, and still
	// verify end to end: both CRCs correct, every relation above satisfied, and a structurally
	// impossible table reported as a good one. That answer authorises a write.
	//
	// The spec's geometry is exact. The primary header sits at LBA 1 and its array follows, ending
	// before `FirstUsableLBA`; the backup's array follows `LastUsableLBA` and ends before the backup
	// header at the last sector. Each copy's metadata is contiguous with itself, which is also what
	// makes the two independently recoverable - the reason there are two of them.
	if header_lba < first_usable {
		// The primary: below the usable range, and after the header it belongs to.
		if entries_lba <= header_lba || entries_last_lba >= first_usable {
			return Err(Fault::Unusable);
		}
	} else {
		// The backup: above the usable range, and before the header it belongs to.
		if entries_lba <= last_usable || entries_last_lba >= header_lba {
			return Err(Fault::Unusable);
		}
	}

	// the device is the outermost bound on every number above. A device that will not say
	// its size leaves the header unbounded, and an unbounded header may not be acted on.
	let capacity = dev.capacity().ok_or(Fault::Unusable)?;
	if last_usable >= capacity || header_lba >= capacity || backup_lba >= capacity || entries_last_lba >= capacity {
		return Err(Fault::Unusable);
	}

	// the entry array's own CRC32, over exactly `num_entries * entry_size` bytes. Without
	// it every span below is unverified, which is the same class of mistake one level in.
	let walk = num_entries.min(MAX_ENTRIES);
	if walk != num_entries {
		// the array is longer than this build walks, so its CRC cannot be confirmed and the
		// entries cannot be trusted. Refusing is the honest answer.
		return Err(Fault::Unusable);
	}
	let mut array: Vec<u8> = try_zeroed(array_bytes as usize)?;
	let mut sector = [0u8; SECTOR_SIZE];
	for i in 0..array_sectors {
		if !dev.read(entries_lba.checked_add(i).ok_or(Fault::Unusable)?, &mut sector) {
			return Err(Fault::Io);
		}
		let start = (i * SECTOR_SIZE as u64) as usize;
		let end = (start + SECTOR_SIZE).min(array.len());
		array[start..end].copy_from_slice(&sector[..end - start]);
	}
	let stored = u32::from_le_bytes(header[HDR_ENTRIES_CRC..HDR_ENTRIES_CRC + 4].try_into().ok().ok_or(Fault::Unusable)?);
	if crc32(&array) != stored {
		return Err(Fault::Unusable);
	}

	let mut disk_guid = [0u8; 16];
	disk_guid.copy_from_slice(&header[HDR_DISK_GUID..HDR_DISK_GUID + 16]);
	Ok(Gpt { entries: array, entry_size, num_entries, disk_guid, first_usable, last_usable, header_lba, backup_lba, entries_lba, entries_last_lba })
}

// The backup GPT: its header is the disk's LAST sector, and it describes the same partitions
// as the primary. Consulted only when the primary did not verify, which is what a backup is
// for - and it is the difference between telling an operator their table is damaged and
// telling them their disk is blank.
fn backup_gpt(dev: &mut impl Sectors) -> Result<Gpt, Fault> {
	let capacity = dev.capacity().ok_or(Fault::Unusable)?;
	let last = capacity.checked_sub(1).ok_or(Fault::Unusable)?;
	let mut header = [0u8; SECTOR_SIZE];
	if !dev.read(last, &mut header) {
		return Err(Fault::Io);
	}
	// The backup sits at the last sector and names LBA 1 as its counterpart - the mirror of what the
	// primary must say.
	read_gpt(dev, &header, last, 1)
}

// Where the PRIMARY header must say its backup lives: the last sector of the medium. A device that
// will not report its size cannot answer that, and `u64::MAX` is a value no header can carry
// legitimately - so such a device fails the relation rather than skipping it, which is the same
// direction every other unbounded case takes here.
fn primary_counterpart(dev: &mut impl Sectors) -> u64 {
	match dev.capacity() {
		Some(capacity) => capacity.saturating_sub(1),
		None => u64::MAX,
	}
}

// Do the two copies of the table describe the same disk, laid out the same way?
//
// This was `backup.entries == gpt.entries` - the raw entry array and nothing else. Two headers can
// carry byte-identical arrays and still disagree about `first_usable`, `last_usable`, the entry
// count, the entry size or which disk they are on, each with a correct CRC over its own
// contradictory contents. `entry_size` is the sharpest of them: the same bytes read at a different
// stride are a different table, and `find_liberfs` walks `chunks_exact(entry_size)`.
//
// The three fields that MUST differ are excluded by construction: `header_lba`, `backup_lba` and
// `entries_lba` are each copy's own position, and a backup that agreed with the primary about them
// would be the broken one.
fn copies_agree(primary: &Gpt, backup: &Gpt) -> bool {
	primary.entries == backup.entries && primary.entry_size == backup.entry_size && primary.num_entries == backup.num_entries && primary.disk_guid == backup.disk_guid && primary.first_usable == backup.first_usable && primary.last_usable == backup.last_usable
}

// Walk a verified GPT's entry array for the first usable LiberFS partition.
//
// "Usable" is the whole point: a span is only usable if it lies wholly inside the usable
// range the header declares, does not overlap either copy of the GPT metadata, and is big
// enough to hold a volume. A degenerate entry is skipped rather than fatal - another entry
// may be the real volume - so what falls out the bottom is "this GPT names no LiberFS
// partition", which is a complete answer about a disk that belongs to something else.
// Where the OTHER copy of the table keeps its entry array.
//
// A `Gpt` knows its own `entries_lba..=entries_last_lba` and nothing about its counterpart's, so a
// partition was only ever checked against ONE of the two arrays: validating the primary never asked
// whether an entry overlapped the backup's array, and validating the backup never asked about the
// primary's. A conforming table puts `last_usable` below the backup metadata so the question does
// not arise; a checksum-valid hostile one is what this parser is for, and there the two spans are
// independent facts. A LiberFS partition selected over the backup array is a writable filesystem
// sitting on the recovery copy of the table that describes it.
//
// `observed` is the counterpart when it was actually read - the only case that is a fact rather than
// an inference. Otherwise the span is DERIVED from the layout the specification fixes: the array
// sits immediately before the header at the end of the disk, and immediately after it at the start.
// Derivation can be wrong about a table that puts its array somewhere unusual, and being wrong here
// costs a refused partition rather than an overwritten one, which is the direction this file takes
// everywhere else.
fn companion_entries(gpt: &Gpt, observed: Option<&Gpt>) -> Option<(u64, u64)> {
	if let Some(other) = observed {
		return Some((other.entries_lba, other.entries_last_lba));
	}
	let array_bytes = gpt.num_entries.checked_mul(gpt.entry_size as u64)?;
	let sectors = array_bytes.div_ceil(SECTOR_SIZE as u64);
	if gpt.backup_lba > gpt.header_lba {
		// This is the primary; the counterpart is the last sector and its array ends just below it.
		let last = gpt.backup_lba.checked_sub(1)?;
		Some((last.checked_sub(sectors.checked_sub(1)?)?, last))
	} else {
		// This is the backup; the primary's array follows the primary header.
		let first = gpt.backup_lba.checked_add(1)?;
		Some((first, first.checked_add(sectors.checked_sub(1)?)?))
	}
}

// Does `first..=last` touch the counterpart's metadata?
fn hits_companion(companion: Option<(u64, u64)>, first: u64, last: u64) -> bool {
	match companion {
		Some((c_first, c_last)) => first <= c_last && c_first <= last,
		None => false,
	}
}

fn find_liberfs(gpt: &Gpt, companion: Option<(u64, u64)>) -> Disk {
	// The whole table has to be consistent before any one entry of it is acted on.
	//
	// This used to look only at LiberFS-typed entries: each was checked against the usable range and
	// against both copies of the metadata, and nothing compared it with the OTHER partitions. A
	// checksum-valid table naming a Linux partition at 2048..30000 and a LiberFS one at
	// 10000..40000 was accepted, and formatting the second destroyed half the first. A tool that
	// only READS a table may reasonably trust it; this answer authorises a write.
	if table_is_inconsistent(gpt, companion) {
		return Disk::CorruptGpt;
	}
	// EXACTLY ONE CANDIDATE, OR NONE OF THEM.
	//
	// This returned the FIRST structurally usable LiberFS entry it met, so which filesystem the
	// system mounts - writable - was decided by array order. Two non-overlapping LiberFS partitions
	// both pass the table-wide consistency pass; swapping their entries, which a partitioning tool
	// or a disk clone may do for reasons of its own, silently changes which one is the system
	// volume. There is no stable identity in that answer, and nothing reported the ambiguity.
	//
	// Refusing is the only safe answer available here: this rule has no configured unique GUID to
	// select by, so it cannot know WHICH of two candidates was meant, and picking one would be
	// guessing about a volume it is about to authorise writes to. `CorruptGpt` is the answer the
	// caller already handles as "do not act on this table".
	let mut found: Option<(u64, u64)> = None;
	for e in gpt.entries.chunks_exact(gpt.entry_size) {
		if e[ENT_TYPE_GUID..ENT_TYPE_GUID + 16] == UNUSED_TYPE_GUID || e[ENT_TYPE_GUID..ENT_TYPE_GUID + 16] != LIBERFS_TYPE_GUID {
			continue;
		}
		let first = u64::from_le_bytes(e[ENT_FIRST_LBA..ENT_FIRST_LBA + 8].try_into().unwrap());
		let last = u64::from_le_bytes(e[ENT_LAST_LBA..ENT_LAST_LBA + 8].try_into().unwrap());
		if !usable_span(gpt, companion, first, last) {
			continue;
		}
		if found.is_some() {
			return Disk::AmbiguousLiberFs;
		}
		found = Some((first, last));
	}
	match found {
		Some((first, last)) => Disk::LiberFs { first, last },
		None => Disk::GptWithoutLiberFs,
	}
}

// Is every used entry of this table one this build is willing to act on, and do any two of them
// claim the same sector?
//
// Quadratic in the entry count, which is capped at `MAX_ENTRIES` - 512 entries is 130,000 integer
// comparisons, once, at boot, and the alternative is sorting a copy of the array to save a
// microsecond nobody will notice.
//
// It ALLOCATES NOTHING. This collected the spans into a `Vec` first, which put an infallible
// allocation back into a hostile-media path the surrounding parser had just been rewritten to keep
// fallible throughout - and the crate is a dependency of both the kernel and the storage service,
// so the failure is an allocation abort in ring 0 or the userspace allocation-error handler, in a
// function whose enclosing answer already has a `NoMemory` variant. The algorithm was already
// quadratic, so two nested iterators over the array do the same work with nothing to size.
//
// And it checks EVERY used entry, not only the LiberFS candidate. `find_liberfs` says the whole
// table has to be consistent before any one entry of it is acted on, and this implemented a third
// of that: entries were compared against each other and against nothing else, while `usable_span` -
// which checks first <= last, containment in the declared usable range, and non-overlap with both
// copies of the metadata - ran only for entries carrying the LiberFS type GUID. So a checksum-valid
// table with a Linux entry at first = 1, last = 1, sitting on top of the GPT header itself and
// overlapping no other partition, passed. A tool that only READS a table may reasonably shrug at
// that; the answer here authorises a write.
fn table_is_inconsistent(gpt: &Gpt, companion: Option<(u64, u64)>) -> bool {
	let used = |e: &[u8]| e[ENT_TYPE_GUID..ENT_TYPE_GUID + 16] != UNUSED_TYPE_GUID;
	let span = |e: &[u8]| {
		let first = u64::from_le_bytes(e[ENT_FIRST_LBA..ENT_FIRST_LBA + 8].try_into().unwrap());
		let last = u64::from_le_bytes(e[ENT_LAST_LBA..ENT_LAST_LBA + 8].try_into().unwrap());
		(first, last)
	};
	// Every used entry has to be a span this build could hand to a filesystem - whatever
	// filesystem that is. A foreign partition that is structurally impossible says the table was
	// not written by anything that agrees with this build about what a table is, and that is a
	// reason to leave the whole disk alone rather than to pick the one entry that looks fine.
	//
	// `MIN_PARTITION_SECTORS` is the one rule NOT applied here: a small foreign partition is
	// perfectly legitimate and only a LiberFS volume needs room for a volume.
	for entry in gpt.entries.chunks_exact(gpt.entry_size).filter(|e| used(e)) {
		let (first, last) = span(entry);
		if first == 0 || last < first {
			return true;
		}
		if first < gpt.first_usable || last > gpt.last_usable {
			return true;
		}
		if first <= gpt.header_lba && gpt.header_lba <= last {
			return true;
		}
		if first <= gpt.backup_lba && gpt.backup_lba <= last {
			return true;
		}
		if first <= gpt.entries_last_lba && gpt.entries_lba <= last {
			return true;
		}
		if hits_companion(companion, first, last) {
			return true;
		}
	}
	for (index, a) in gpt.entries.chunks_exact(gpt.entry_size).enumerate().filter(|(_, e)| used(e)) {
		let (a_first, a_last) = span(a);
		// A UNIQUE PARTITION GUID THAT IS NOT UNIQUE. The field's whole purpose is to name one
		// partition across tools, reboots and clones, and nothing compared them - so a table could
		// carry two partitions with one identity and every consumer that selects by GUID would pick
		// whichever it met first. Same class as the ambiguity `find_liberfs` now refuses, one field
		// lower down: an identifier that identifies two things is not an identifier.
		//
		// All-zero is exempt, because an unused slot is already excluded by `used` and a zero GUID
		// in a used entry is a table that never assigned one rather than two that collide.
		let a_guid = &a[ENT_UNIQUE_GUID..ENT_UNIQUE_GUID + 16];
		for b in gpt.entries.chunks_exact(gpt.entry_size).skip(index + 1).filter(|e| used(e)) {
			let (b_first, b_last) = span(b);
			if a_first <= b_last && b_first <= a_last {
				return true;
			}
			if a_guid != UNUSED_TYPE_GUID && a_guid == &b[ENT_UNIQUE_GUID..ENT_UNIQUE_GUID + 16] {
				return true;
			}
		}
	}
	false
}

// Is `first..=last` a span this build may hand to a filesystem?
fn usable_span(gpt: &Gpt, companion: Option<(u64, u64)>, first: u64, last: u64) -> bool {
	if first == 0 || last < first {
		return false;
	}
	// wholly inside what the header calls usable. This is what refuses the entry that names
	// LBA 1: the primary header's own sector is below `first_usable` by construction.
	if first < gpt.first_usable || last > gpt.last_usable {
		return false;
	}
	// and belt-and-braces against a header that put its metadata inside its own usable
	// range anyway - `read_gpt` refuses such a header, so this can only fire if that check
	// is ever relaxed.
	if first <= gpt.header_lba && gpt.header_lba <= last {
		return false;
	}
	if first <= gpt.backup_lba && gpt.backup_lba <= last {
		return false;
	}
	if first <= gpt.entries_last_lba && gpt.entries_lba <= last {
		return false;
	}
	// and the OTHER copy's array, which this header does not describe - see `companion_entries`.
	if hits_companion(companion, first, last) {
		return false;
	}
	match last.checked_sub(first).and_then(|n| n.checked_add(1)) {
		Some(sectors) => sectors >= MIN_PARTITION_SECTORS,
		None => false,
	}
}

// Does LBA 0 carry a filesystem laid straight onto the medium (no partition table)?
//
// This is deliberately a recogniser and not a parser: the answer only has to be good enough
// to say "somebody's data is here, do not format it". A false positive costs a boot; a false
// negative costs the disk, so the signatures kept here are the unambiguous ones.
fn foreign_filesystem(lba0: &[u8]) -> Option<&'static str> {
	// FAT12/16/32 and exFAT: the jump instruction plus the type string where the BPB puts
	// it. The jump alone is too weak (0xEB is an ordinary byte), so both must agree.
	let jump = lba0[0] == 0xEB && lba0[2] == 0x90 || lba0[0] == 0xE9;
	if jump {
		if &lba0[3..11] == b"EXFAT   " {
			return Some("exFAT");
		}
		if &lba0[54..59] == b"FAT12" || &lba0[54..59] == b"FAT16" || &lba0[54..57] == b"FAT" {
			return Some("FAT");
		}
		if &lba0[82..87] == b"FAT32" {
			return Some("FAT32");
		}
		if &lba0[3..11] == b"NTFS    " {
			return Some("NTFS");
		}
	}
	// ISO9660 and UDF put their descriptors far past sector 0, so a data CD imaged onto a
	// disk is not recognised here. That is the honest limit of a one-sector look, and it is
	// recorded rather than papered over: an unrecognised medium answers `Blank` only if it
	// also carries no table and no signature above.
	None
}

// CRC-32 as UEFI specifies it for GPT: the reflected, table-free ISO-HDLC polynomial
// (0xEDB88320), initialised and finalised with all ones. NOT the CRC32C the filesystem uses
// for its own blocks - a different polynomial for a different format, and using the wrong
// one would fail every genuine GPT on the planet.
//
// Public because anything that BUILDS a GPT for this probe to read needs the same function,
// and a second implementation beside it is how the two would drift.
pub fn crc32(bytes: &[u8]) -> u32 {
	let mut crc: u32 = 0xFFFF_FFFF;
	for &b in bytes {
		crc ^= b as u32;
		for _ in 0..8 {
			let mask = (crc & 1).wrapping_neg();
			crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
		}
	}
	!crc
}

#[cfg(test)]
mod tests;
