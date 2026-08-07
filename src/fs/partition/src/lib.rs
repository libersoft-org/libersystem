//! partition - what a raw disk actually IS, decided before anything is written to it.
//!
//! One question, asked once, by the only caller that ever needs it: StorageService, before
//! it decides whether the disk in front of it may be formatted. The answer used to be an
//! `Option` - "there is a LiberFS partition here" or nothing - and "nothing" was read as
//! licence to lay a filesystem over the whole device from sector zero. That is where a
//! protective MBR, a GPT header and a partition entry array live.
//!
//! M0143 established the rule for the mount ("I could not tell" may not be read as "there
//! is nothing here") and M0144 applied it to a GPT that IS recognised. The negative case -
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

use alloc::vec;
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
const HDR_CURRENT_LBA: usize = 24;
const HDR_BACKUP_LBA: usize = 32;
const HDR_FIRST_USABLE: usize = 40;
const HDR_LAST_USABLE: usize = 48;
const HDR_ENTRIES_LBA: usize = 72;
const HDR_NUM_ENTRIES: usize = 80;
const HDR_ENTRY_SIZE: usize = 84;
const HDR_ENTRIES_CRC: usize = 88;

// The UEFI-mandated minimum header size, and the largest this build will read (one sector -
// a header claiming more than the block it lives in is not a header).
const HDR_SIZE_MIN: usize = 92;

// A partition entry's fixed offsets.
const ENT_TYPE_GUID: usize = 0;
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
// Every variant except `Blank` means "do not format this whole device". They are kept
// separate rather than collapsed into one refusal because the operator's next move differs
// for each: a foreign filesystem is data to copy off, an MBR is a table to convert, a
// corrupt GPT is a table to repair, and an I/O failure is a cable to check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disk {
	// LBA 0 and LBA 1 were both read and neither carries a partition table, a protective
	// MBR, or a filesystem this build recognises. The ONLY answer that licenses laying a
	// filesystem over a whole device.
	Blank,
	// No partition table, and LBA 0 carries a LiberFS superblock: a volume written straight
	// onto the medium by this system, which is what the fixed whole-device layout produces.
	//
	// Distinct from `Blank` on purpose. Both mean "the whole device is the container", so
	// both reach the same mount - but only one of them describes a disk with nothing on it,
	// and an answer that cannot tell those apart is how this went wrong the first time.
	// What keeps the existing volume safe is the MOUNT, which formats only on `Unformatted`.
	LiberFsWholeDevice,
	// A GPT naming a usable LiberFS partition, as (first LBA, last LBA) inclusive.
	LiberFs { first: u64, last: u64 },
	// A GPT, verified end to end, that names no usable LiberFS partition. The disk belongs
	// to something else.
	GptWithoutLiberFs,
	// An MBR partition table (or a protective MBR whose GPT did not verify - see
	// `CorruptGpt` for the case where the signature was there). Not a blank disk.
	MbrWithoutLiberFs,
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

	let mbr = classify_mbr(&lba0);
	if &lba1[HDR_SIGNATURE..HDR_SIGNATURE + 8] == b"EFI PART" {
		return match read_gpt(dev, &lba1) {
			Some(gpt) => find_liberfs(&gpt),
			// the signature is there and the table is not believable. Consult the backup
			// before condemning it: a damaged primary header with an intact backup is the
			// case UEFI defines a backup FOR.
			None => match backup_gpt(dev) {
				Some(gpt) => find_liberfs(&gpt),
				None => Disk::CorruptGpt,
			},
		};
	}
	// no GPT signature at LBA 1. A protective MBR says one is SUPPOSED to be there, so its
	// absence is damage rather than a plain MBR disk - and the backup is where to look.
	if mbr == Mbr::Protective {
		return match backup_gpt(dev) {
			Some(gpt) => find_liberfs(&gpt),
			None => Disk::CorruptGpt,
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
	Disk::Blank
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
}

// Classify LBA 0's MBR partition table. A hybrid MBR (a protective 0xEE entry BESIDE real
// ones) reads as `Partitioned`, which is the safe direction: it says a table is there.
fn classify_mbr(lba0: &[u8]) -> Mbr {
	if lba0[510] != 0x55 || lba0[511] != 0xAA {
		return Mbr::Absent;
	}
	let mut protective = false;
	let mut real = false;
	for i in 0..4 {
		let e = &lba0[446 + i * 16..446 + (i + 1) * 16];
		let kind = e[4];
		let sectors = u32::from_le_bytes(e[12..16].try_into().unwrap());
		// an empty slot is type 0 with no length; either alone is enough to skip it.
		if kind == 0 || sectors == 0 {
			continue;
		}
		if kind == 0xEE {
			protective = true;
		} else {
			real = true;
		}
	}
	match (real, protective) {
		(true, _) => Mbr::Partitioned,
		(false, true) => Mbr::Protective,
		(false, false) => Mbr::Absent,
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
	first_usable: u64,
	last_usable: u64,
	// the header's own sector and the entry array's span, so a partition entry that
	// overlaps the metadata that names it can be refused.
	header_lba: u64,
	backup_lba: u64,
	entries_lba: u64,
	entries_last_lba: u64,
}

// Verify a GPT header block and the entry array it points at, or None if anything about it
// cannot be true.
//
// The old probe checked the signature, that `entry_size` was a power of two in range, that
// `num_entries` was non-zero, and then trusted the rest. It checked NEITHER CRC, no header
// LBA relation, and nothing against the device's capacity - so an entry with the LiberFS
// type GUID, `first = 1` and `last = 100000` passed every test there was, and the mount then
// targeted LBA 1, which is the primary GPT header itself. Finding no superblock there, the
// service formatted on top of the partition table that had named the partition.
fn read_gpt(dev: &mut impl Sectors, header: &[u8]) -> Option<Gpt> {
	if &header[HDR_SIGNATURE..HDR_SIGNATURE + 8] != b"EFI PART" {
		return None;
	}
	// revision 1.0 is what every GPT in existence carries; a different one is a format this
	// build has not been told how to read.
	if u32::from_le_bytes(header[HDR_REVISION..HDR_REVISION + 4].try_into().ok()?) != 0x0001_0000 {
		return None;
	}
	let header_size = u32::from_le_bytes(header[HDR_SIZE..HDR_SIZE + 4].try_into().ok()?) as usize;
	if header_size < HDR_SIZE_MIN || header_size > SECTOR_SIZE {
		return None;
	}
	// the header's own CRC32, computed over `header_size` bytes with the CRC field zeroed.
	// This is the check that makes every field below worth reading at all.
	let stored = u32::from_le_bytes(header[HDR_CRC..HDR_CRC + 4].try_into().ok()?);
	let mut probe = header[..header_size].to_vec();
	probe[HDR_CRC..HDR_CRC + 4].fill(0);
	if crc32(&probe) != stored {
		return None;
	}

	let header_lba = u64::from_le_bytes(header[HDR_CURRENT_LBA..HDR_CURRENT_LBA + 8].try_into().ok()?);
	let backup_lba = u64::from_le_bytes(header[HDR_BACKUP_LBA..HDR_BACKUP_LBA + 8].try_into().ok()?);
	let first_usable = u64::from_le_bytes(header[HDR_FIRST_USABLE..HDR_FIRST_USABLE + 8].try_into().ok()?);
	let last_usable = u64::from_le_bytes(header[HDR_LAST_USABLE..HDR_LAST_USABLE + 8].try_into().ok()?);
	let entries_lba = u64::from_le_bytes(header[HDR_ENTRIES_LBA..HDR_ENTRIES_LBA + 8].try_into().ok()?);
	let num_entries = u32::from_le_bytes(header[HDR_NUM_ENTRIES..HDR_NUM_ENTRIES + 4].try_into().ok()?) as u64;
	let entry_size = u32::from_le_bytes(header[HDR_ENTRY_SIZE..HDR_ENTRY_SIZE + 4].try_into().ok()?) as usize;

	// the relations BETWEEN the fields, which a CRC cannot speak to: a header may be
	// perfectly intact and still describe a disk that cannot exist.
	// LBA 0 is the protective MBR's sector by spec, so neither copy of the header can be
	// there, and the two copies cannot be the same sector.
	if header_lba == backup_lba || header_lba == 0 || backup_lba == 0 {
		return None;
	}
	if first_usable == 0 || last_usable < first_usable {
		return None;
	}
	// the UEFI spec puts both copies of the metadata OUTSIDE the usable range, which is what
	// makes "a partition inside the usable range cannot overlap the tables" true.
	if header_lba >= first_usable && header_lba <= last_usable {
		return None;
	}
	if backup_lba >= first_usable && backup_lba <= last_usable {
		return None;
	}
	// the entry array: a power-of-two width no smaller than the 128 bytes the spec fixes,
	// at least one entry, and a span that fits in the address space.
	if entry_size < 128 || entry_size > SECTOR_SIZE || !entry_size.is_power_of_two() {
		return None;
	}
	if num_entries == 0 || entries_lba == 0 {
		return None;
	}
	// checked throughout: `num_entries * entry_size` is two numbers off the medium
	// multiplied together, and the product decides how far the array reaches.
	let array_bytes = num_entries.checked_mul(entry_size as u64)?;
	let array_sectors = array_bytes.div_ceil(SECTOR_SIZE as u64);
	let entries_last_lba = entries_lba.checked_add(array_sectors)?.checked_sub(1)?;
	// and the array is metadata too, so it may not sit in the usable range either.
	if entries_last_lba >= first_usable && entries_lba <= last_usable {
		return None;
	}

	// the device is the outermost bound on every number above. A device that will not say
	// its size leaves the header unbounded, and an unbounded header may not be acted on.
	let capacity = dev.capacity()?;
	if last_usable >= capacity || header_lba >= capacity || backup_lba >= capacity || entries_last_lba >= capacity {
		return None;
	}

	// the entry array's own CRC32, over exactly `num_entries * entry_size` bytes. Without
	// it every span below is unverified, which is the same class of mistake one level in.
	let walk = num_entries.min(MAX_ENTRIES);
	if walk != num_entries {
		// the array is longer than this build walks, so its CRC cannot be confirmed and the
		// entries cannot be trusted. Refusing is the honest answer.
		return None;
	}
	let mut array: Vec<u8> = vec![0u8; array_bytes as usize];
	let mut sector = [0u8; SECTOR_SIZE];
	for i in 0..array_sectors {
		if !dev.read(entries_lba.checked_add(i)?, &mut sector) {
			return None;
		}
		let start = (i * SECTOR_SIZE as u64) as usize;
		let end = (start + SECTOR_SIZE).min(array.len());
		array[start..end].copy_from_slice(&sector[..end - start]);
	}
	let stored = u32::from_le_bytes(header[HDR_ENTRIES_CRC..HDR_ENTRIES_CRC + 4].try_into().ok()?);
	if crc32(&array) != stored {
		return None;
	}

	Some(Gpt { entries: array, entry_size, first_usable, last_usable, header_lba, backup_lba, entries_lba, entries_last_lba })
}

// The backup GPT: its header is the disk's LAST sector, and it describes the same partitions
// as the primary. Consulted only when the primary did not verify, which is what a backup is
// for - and it is the difference between telling an operator their table is damaged and
// telling them their disk is blank.
fn backup_gpt(dev: &mut impl Sectors) -> Option<Gpt> {
	let capacity = dev.capacity()?;
	let mut header = [0u8; SECTOR_SIZE];
	if !dev.read(capacity.checked_sub(1)?, &mut header) {
		return None;
	}
	read_gpt(dev, &header)
}

// Walk a verified GPT's entry array for the first usable LiberFS partition.
//
// "Usable" is the whole point: a span is only usable if it lies wholly inside the usable
// range the header declares, does not overlap either copy of the GPT metadata, and is big
// enough to hold a volume. A degenerate entry is skipped rather than fatal - another entry
// may be the real volume - so what falls out the bottom is "this GPT names no LiberFS
// partition", which is a complete answer about a disk that belongs to something else.
fn find_liberfs(gpt: &Gpt) -> Disk {
	for e in gpt.entries.chunks_exact(gpt.entry_size) {
		if e[ENT_TYPE_GUID..ENT_TYPE_GUID + 16] == UNUSED_TYPE_GUID || e[ENT_TYPE_GUID..ENT_TYPE_GUID + 16] != LIBERFS_TYPE_GUID {
			continue;
		}
		let first = u64::from_le_bytes(e[ENT_FIRST_LBA..ENT_FIRST_LBA + 8].try_into().unwrap());
		let last = u64::from_le_bytes(e[ENT_LAST_LBA..ENT_LAST_LBA + 8].try_into().unwrap());
		if usable_span(gpt, first, last) {
			return Disk::LiberFs { first, last };
		}
	}
	Disk::GptWithoutLiberFs
}

// Is `first..=last` a span this build may hand to a filesystem?
fn usable_span(gpt: &Gpt, first: u64, last: u64) -> bool {
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
