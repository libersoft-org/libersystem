//! UDF - a read-only backend for DVD / Blu-ray and large optical (`.udf`) media,
//! behind the same [`BlockDevice`] trait FAT, ISO9660 and LiberFS use. It sits behind
//! `Storage.Volume` as just another FS backend: per the layering principle several
//! filesystems mount behind one volume API, and UDF is the format DVDs and Blu-ray
//! discs use, so ISO9660 covers CDs and this completes optical-media interop.
//!
//! Read-only by design - no allocation or write path. Mounting reads the Anchor Volume
//! Descriptor Pointer at LBA 256, scans the Main Volume Descriptor Sequence for the
//! Partition Descriptor (its start LBA) and the Logical Volume Descriptor (the File Set
//! location), then the File Set Descriptor for the root directory ICB. A file is found by
//! walking `/`-separated segments from the root: each directory's File Entry yields its
//! data extent, scanned for File Identifier Descriptors, and the next directory or file
//! File Entry is read in turn. Data lives inline in the File Entry (embedded) or in short
//! / long allocation extents. Names are OSTA compressed Unicode (8-bit Latin-1 or 16-bit
//! UCS-2). All addresses are partition-relative, resolved against the partition start.
//!
//! The media is untrusted: every block address, length, and extent is bounded by the
//! partition's own length (whose last block is verified to exist on the device at
//! mount) before a buffer is allocated, descriptor tag checksums and locations are
//! verified, and an unrecorded (sparse) extent reads as zeros, never as stale disk
//! content. One physical partition is assumed (the long_ad partition references are
//! not interpreted) and the UDF 2.50+ metadata partition (Blu-ray) is not - such
//! volumes refuse to mount rather than misread.
//!
//! ## What this reader does not do
//!
//! Named here rather than discovered from a refusal. It reads volumes with a 2048-byte logical
//! block size, exactly one Type-1 (physical) partition map, and ICB strategy 4; anything else is
//! refused by name at mount. Type-3 allocation descriptors - the continuation into an Allocation
//! Extent Descriptor - are refused, so a sufficiently fragmented but entirely valid file cannot be
//! read. The Metadata Partition Map that BDMV requires is not implemented, which is what "no
//! Blu-ray support" means concretely. One read allocates at most 64 MiB, because the volume API
//! has no `read_at` yet; a larger file is refused rather than attempted.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

#[cfg(test)]
mod tests;

// One logical block. UDF sets a block size in the Logical Volume Descriptor, but it is
// 2048 in practice and that is the unit a disc and a `.udf` image read in; the device
// reads one 2048-byte block at a time, by absolute LBA.
pub const SECTOR_SIZE: usize = 2048;

// The Anchor Volume Descriptor Pointer sits at a fixed LBA; it points at the Main Volume
// Descriptor Sequence, so mounting starts here.
const AVDP_LBA: u64 = 256;

// The most one `read_file` or directory read may allocate: 64 MiB.
//
// Not a format limit - UDF files are far larger - but the limit of what this crate's
// read-it-all-into-a-Vec primitive can honestly do. A larger file is refused by name rather than
// attempted, until the volume API grows `read_at`.
const MAX_READ_BYTES: usize = 64 * 1024 * 1024;

// Descriptor tag identifiers (ECMA-167) we read.
const TAG_AVDP: u16 = 2;
const TAG_PARTITION: u16 = 5;
const TAG_LOGICAL_VOLUME: u16 = 6;
const TAG_TERMINATING: u16 = 8;
const TAG_FILE_SET: u16 = 256;
const TAG_FILE_ID: u16 = 257;
const TAG_FILE_ENTRY: u16 = 261;
const TAG_EXT_FILE_ENTRY: u16 = 266;

// A block device: optical media is read one 2048-byte logical block at a time, by
// absolute LBA. The trait is the shared fs-core one (a block is exactly `buf.len()`
// bytes); UDF is read-only, so it uses only `read_block` and keeps fs-core's
// refuse-write and no-op-flush defaults.
pub use fscore::BlockDevice;

// A UDF error. The variants map onto the `Storage.Volume` `error` enum at the service
// boundary (NotFound -> not-found, the rest -> invalid). The type is the shared fs-core
// one, so every backend reports through one error enum; UDF uses only the read subset.
pub use fscore::FsError;

// One directory entry: a name, a byte length, and whether it is a subdirectory. The
// listing the shell shows; a directory reports a length of zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
	pub name: String,
	pub size: u64,
	pub is_dir: bool,
}

// The partition's start LBA and length in blocks (bounding every partition-relative
// address and extent), plus the root directory ICB block (partition-relative); every
// read derives from these, so mounting is just locating one File Set Descriptor.
struct Geometry {
	part_start: u32,
	part_len: u32,
	root_icb: u32,
}

// A mounted UDF volume: the device plus its geometry. Reads are on demand, so nothing is
// cached beyond the root ICB; a directory or file is read by following its extent as
// asked.
pub struct Udf<D: BlockDevice> {
	dev: D,
	geo: Geometry,
}

// Why a mount did not happen.
//
// `mount` answered `Option`, so "this is not UDF", "this UDF is damaged", "this UDF uses something
// this reader does not implement" and "the device did not answer" were the same answer - and a
// probe could not tell "try the next backend" from "this IS UDF and it is broken, do not pretend
// otherwise". The other backends in this tree already distinguish these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountError {
	// No anchor, or nothing that claims to be UDF: a blank disc, or one belonging to something else.
	NotUdf,
	// UDF, and using something this reader does not implement - a partition map it cannot resolve,
	// a logical block size it does not read, an ICB strategy it does not follow.
	Unsupported,
	// UDF, and its own structures failed its own checks.
	Corrupt,
	// The device did not answer. Says nothing about what is on it.
	Io,
}

impl Geometry {
	// One logical address -> one physical block, in ONE place.
	//
	// A UDF logical address is a block number AND a partition reference number, and the Partition
	// Maps say how each reference is translated. Every call site here took the block number, threw
	// the reference away, and added `part_start` - correct for exactly one shape (a single physical
	// partition with every reference pointing into it) and silently wrong for any other.
	//
	// The mount now REFUSES anything but that shape, so the translation below is sound; putting it
	// here means adding a partition type later is a change in one function rather than at five call
	// sites, and the bound comes with it.
	fn physical(&self, partition_ref: u16, lb: u32) -> Option<u64> {
		if partition_ref != 0 || lb >= self.part_len {
			return None;
		}
		Some(self.part_start as u64 + lb as u64)
	}
}

impl<D: BlockDevice> Udf<D> {
	// The block device this filesystem reads through.
	pub fn device(&self) -> &D {
		&self.dev
	}
	// Mount UDF media: read the Anchor at LBA 256, scan the Main Volume Descriptor
	// Sequence for the partition start and File Set, then the root directory ICB. None if
	// the layout cannot be followed.
	// The probe form, kept because most callers only ask "is this UDF": `mount_checked` is the one
	// that says WHY not.
	pub fn mount(dev: D) -> Option<Udf<D>> {
		Self::mount_checked(dev).ok()
	}

	pub fn mount_checked(mut dev: D) -> Result<Udf<D>, MountError> {
		let mut block = [0u8; SECTOR_SIZE];
		// UDF puts anchors at 256, N-256 and N precisely so a damaged one is survivable, and each
		// anchor carries a RESERVE volume descriptor sequence beside the Main one. This read one
		// anchor and one sequence and answered `None` if either was damaged - on optical media,
		// where that redundancy is most likely to be the thing that saves the volume.
		// Only the anchor at 256 here. The other two live at N-256 and N, and `BlockDevice` does not
		// expose N - so finding them needs a device-size accessor on the trait, which is a change
		// to the shared contract rather than to this crate. Said plainly rather than left as an
		// unexplained single read; the RESERVE sequence below is the redundancy that IS reachable.
		let anchors = [AVDP_LBA];
		let mut sequences: [(u32, u32); 6] = [(0, 0); 6];
		let mut sequence_count = 0usize;
		for &anchor in &anchors {
			if anchor == 0 || !dev.read_block(anchor, &mut block) || !validate_descriptor(&block, TAG_AVDP, anchor as u32) {
				continue;
			}
			// Main first, then Reserve: the specification's own read order.
			for (len_at, loc_at) in [(16usize, 20usize), (24usize, 28usize)] {
				let len = le32(&block[len_at..len_at + 4]);
				let loc = le32(&block[loc_at..loc_at + 4]);
				if len != 0 && sequence_count < sequences.len() {
					sequences[sequence_count] = (len, loc);
					sequence_count += 1;
				}
			}
		}
		// No anchor that validates, and no sequence to read: this is not a UDF volume, which is a
		// different answer from a UDF volume that is broken.
		if sequence_count == 0 {
			return Err(MountError::NotUdf);
		}
		let mut found: Option<(u32, u32, u32)> = None;
		for &(vds_len, vds_loc) in sequences.iter().take(sequence_count) {
			if let Some(triple) = Self::scan_sequence(&mut dev, vds_len, vds_loc) {
				found = Some(triple);
				break;
			}
		}
		// An anchor was found and no sequence yielded a partition and a File Set: the volume claims
		// to be UDF and does not hold together.
		let Some((part_start, part_len, fileset_lb)) = found else {
			return Err(MountError::Corrupt);
		};
		Self::finish_mount(dev, part_start, part_len, fileset_lb).ok_or(MountError::Corrupt)
	}

	// One volume descriptor sequence: the partition and the File Set it names, or None when this
	// sequence cannot supply them - in which case the caller tries the next one.
	fn scan_sequence(dev: &mut D, vds_len: u32, vds_loc: u32) -> Option<(u32, u32, u32)> {
		let mut block = [0u8; SECTOR_SIZE];
		let mut part: Option<(u32, u32, u32)> = None;
		let mut fileset_lb: Option<(u32, u32)> = None;
		// the sequence length is the medium's claim - a real MVDS is a handful of
		// descriptors, so the scan is clamped rather than driven megablocks far.
		// The sequence length, rounded UP and not floored, and a zero length refused rather than
		// turned into one block: a descriptor the anchor does not claim is in the sequence was
		// getting parsed, and a partial last block was getting dropped. The ceiling stays - a real
		// Main VDS is a handful of descriptors and a forged length must not drive the scan
		// megablocks far - but reaching it is a refusal rather than a quiet stop.
		if vds_len == 0 {
			return None;
		}
		let count = (vds_len as usize).div_ceil(SECTOR_SIZE);
		if count > 64 {
			return None;
		}
		for i in 0..count as u64 {
			if !dev.read_block(vds_loc as u64 + i, &mut block) {
				return None;
			}
			// a descriptor must checksum AND record its own address - a stale or copied
			// block is skipped, never trusted.
			// Every descriptor in the sequence, body CRC included: a bit flipped in a partition
			// start passed every check this parser made as long as the first sixteen bytes were fine.
			if !validate_descriptor(&block, le16(&block[0..2]), (vds_loc as u64 + i) as u32) {
				continue;
			}
			// The PREVAILING descriptor, by Volume Descriptor Sequence Number, not the last one seen.
			//
			// Each descriptor overwrote the previous one, so a volume that has been updated - the
			// normal state of a rewritable disc, where a new descriptor is appended rather than the
			// old one erased - could be read through a stale copy. The sequence number is what
			// ECMA-167 provides to order them.
			let vdsn = le32(&block[16..20]);
			match le16(&block[0..2]) {
				TAG_PARTITION if part.is_none_or(|(seen, _, _)| vdsn >= seen) => part = Some((vdsn, le32(&block[188..192]), le32(&block[192..196]))),
				TAG_LOGICAL_VOLUME => {
					// What UDF is this, and can this reader address it at all?
					//
					// None of these three was read. The parser assumed 2048-byte blocks throughout
					// and had no way to say whether it was looking at 1.02 or 2.60 - and the header
					// claimed a metadata-partition volume would "refuse to mount rather than being
					// misread", which no code could produce. What happened instead was that the
					// File Set reference resolved against the physical partition and landed on
					// something that was not a File Set Descriptor, so the mount failed by accident.
					if le32(&block[212..216]) != SECTOR_SIZE as u32 {
						return None;
					}
					// The Domain Identifier, which says this is a UDF volume and not merely an
					// ECMA-167 one this reader has no business interpreting.
					if &block[217..236] != b"*OSTA UDF Compliant" {
						return None;
					}
					// EXACTLY one Type-1 (physical) partition map. Every logical address here is
					// resolved against the one physical partition, which is correct for this shape
					// and silently wrong for any other - so anything else is refused by name rather
					// than misread.
					let map_count = le32(&block[268..272]);
					let map_len = le32(&block[272..276]) as usize;
					if map_count != 1 || map_len < 2 || 440 + map_len > block.len() {
						return None;
					}
					if block[440] != 1 {
						return None;
					}
					if fileset_lb.is_none_or(|(seen, _)| vdsn >= seen) {
						fileset_lb = Some((vdsn, le32(&block[252..256])));
					}
					// The File Set's partition reference, which was discarded. With one map it can
					// only be partition 0, and requiring that is what makes the discard safe.
					if le16(&block[256..258]) != 0 {
						return None;
					}
				}
				TAG_TERMINATING => break,
				_ => {}
			}
		}
		let (_, part_start, part_len) = part?;
		let (_, fileset_lb) = fileset_lb?;
		Some((part_start, part_len, fileset_lb))
	}

	// The checks that do not depend on which sequence supplied the answer.
	fn finish_mount(mut dev: D, part_start: u32, part_len: u32, fileset_lb: u32) -> Option<Udf<D>> {
		let mut block = [0u8; SECTOR_SIZE];
		// the partition length bounds every partition-relative address; a zero length or
		// a File Set outside it cannot form a volume, and the partition's last block must
		// exist on the device - or a forged or truncated image mounts and only fails, or
		// allocates without bound, inside a later read (the real media size then bounds
		// every extent).
		if part_len == 0 || fileset_lb >= part_len {
			return None;
		}
		if !dev.read_block(part_start as u64 + part_len as u64 - 1, &mut block) {
			return None;
		}
		if !dev.read_block(part_start as u64 + fileset_lb as u64, &mut block) || !validate_descriptor(&block, TAG_FILE_SET, fileset_lb) {
			return None;
		}
		let root_icb = le32(&block[404..408]);
		if root_icb >= part_len {
			return None;
		}
		Some(Udf { dev, geo: Geometry { part_start, part_len, root_icb } })
	}

	// The partition's size in bytes - the length the Main Volume Descriptor Sequence
	// declares, for volume status reporting. Read-only media, so it is all in use.
	pub fn total_bytes(&self) -> u64 {
		self.geo.part_len as u64 * SECTOR_SIZE as u64
	}

	// List the volume's root directory.
	pub fn list(&mut self) -> Result<Vec<FileInfo>, FsError> {
		self.read_dir(self.geo.root_icb)
	}

	// List a subdirectory named by a `/`-separated path. An empty path is the root.
	pub fn list_dir(&mut self, path: &[u8]) -> Result<Vec<FileInfo>, FsError> {
		let icb = self.resolve_dir(path)?;
		self.read_dir(icb)
	}

	// Read a whole file named by a `/`-separated path into a Vec.
	pub fn read_file(&mut self, path: &[u8]) -> Result<Vec<u8>, FsError> {
		let (parent, name) = split_parent(path)?;
		let dir = self.resolve_dir(parent)?;
		let (icb, is_dir) = self.find_entry(dir, name)?;
		if is_dir {
			return Err(FsError::NotFound);
		}
		self.read_icb(icb)
	}

	// Walk path segments from the root, descending into each named subdirectory, and
	// return the final directory's ICB. An empty path is the root.
	fn resolve_dir(&mut self, path: &[u8]) -> Result<u32, FsError> {
		let mut icb = self.geo.root_icb;
		for seg in path.split(|&b| b == b'/').filter(|s| !s.is_empty()) {
			let (next, is_dir) = self.find_entry(icb, seg)?;
			if !is_dir {
				return Err(FsError::NotFound);
			}
			icb = next;
		}
		Ok(icb)
	}

	// Scan a directory for a File Identifier matching `name` (case-insensitively),
	// returning its ICB block and whether it is a directory. The parent entry matches
	// the name "..", so paths through it resolve as on the other backends.
	fn find_entry(&mut self, dir_icb: u32, name: &[u8]) -> Result<(u32, bool), FsError> {
		let data = self.read_icb(dir_icb)?;
		let mut off = 0usize;
		// An EXACT match wins outright; a case-folded one is a fallback, and an ambiguous fallback
		// is an error rather than whichever record came first. `README`, `Readme` and `readme` can
		// all exist on a UDF volume, and answering with the first was a coin toss that made the
		// others unreachable by their own names.
		let mut folded: Option<(u32, bool)> = None;
		let mut folded_ambiguous = false;
		while off + 38 <= data.len() {
			let fid = &data[off..];
			// A record that is not a File Identifier Descriptor, or one that runs past the
			// directory's data, is CORRUPTION - not the end of the directory. Breaking here and
			// answering `NotFound` meant a caller could not tell a missing file from a damaged
			// directory, which is what `FsError::Corrupt` exists for.
			if le16(&fid[0..2]) != TAG_FILE_ID || !tag_ok(fid) {
				return Err(FsError::Corrupt);
			}
			let l_iu = le16(&fid[36..38]) as usize;
			let l_fi = fid[19] as usize;
			let total = 38 + l_iu + l_fi;
			if off + total > data.len() {
				return Err(FsError::Corrupt);
			}
			let chars = fid[18];
			let parent = chars & 0x08 != 0;
			let deleted = chars & 0x04 != 0;
			let is_dir = chars & 0x02 != 0;
			let id = decode_name(&fid[38 + l_iu..38 + l_iu + l_fi]);
			if parent {
				if name == b".." {
					return Ok((le32(&fid[24..28]), is_dir));
				}
			} else if !deleted && !id.is_empty() {
				if id.as_bytes() == name {
					return Ok((le32(&fid[24..28]), is_dir));
				}
				if eq_ci(&id, name) {
					if folded.is_some() {
						folded_ambiguous = true;
					}
					folded = Some((le32(&fid[24..28]), is_dir));
				}
			}
			off += (total + 3) & !3;
		}
		if folded_ambiguous {
			return Err(FsError::BadName);
		}
		folded.ok_or(FsError::NotFound)
	}

	// Read every File Identifier in a directory into FileInfos, skipping the parent
	// entry, deleted records, and empty names. The size column comes from the child's
	// File Entry HEADER - a listing never pulls file contents through the device.
	fn read_dir(&mut self, dir_icb: u32) -> Result<Vec<FileInfo>, FsError> {
		let data = self.read_icb(dir_icb)?;
		let mut out = Vec::new();
		let mut off = 0usize;
		while off + 38 <= data.len() {
			let fid = &data[off..];
			// Corruption, not the end: a listing that stopped at the damage and returned `Ok`
			// was a short list with nothing to say so.
			if le16(&fid[0..2]) != TAG_FILE_ID || !tag_ok(fid) {
				return Err(FsError::Corrupt);
			}
			let l_iu = le16(&fid[36..38]) as usize;
			let l_fi = fid[19] as usize;
			let total = 38 + l_iu + l_fi;
			if off + total > data.len() {
				return Err(FsError::Corrupt);
			}
			let chars = fid[18];
			if chars & 0x08 == 0 && chars & 0x04 == 0 {
				let is_dir = chars & 0x02 != 0;
				let id = decode_name(&fid[38 + l_iu..38 + l_iu + l_fi]);
				// an unreadable child header lists as size 0 by decision - the listing
				// stays best-effort, the file's own read reports the error honestly.
				// A child whose File Entry cannot be read is not a zero-byte file.
				//
				// `unwrap_or(0)` turned an unreadable, misplaced or corrupt File Entry into an entry
				// that lists as empty - so a listing described a damaged volume as a tidy one, and
				// the caller had nothing to go on.
				let size = if is_dir {
					0
				} else {
					match self.icb_size(le32(&fid[24..28]), is_dir) {
						Ok(size) => size,
						Err(error) => return Err(error),
					}
				};
				if !id.is_empty() {
					out.push(FileInfo { name: id, size, is_dir });
				}
			}
			off += (total + 3) & !3;
		}
		Ok(out)
	}

	// The information length recorded in a File Entry's header - the size a listing
	// reports, read from the one header block instead of the whole content.
	fn icb_size(&mut self, lb: u32, expect_dir: bool) -> Result<u64, FsError> {
		let Some(at) = self.geo.physical(0, lb) else {
			return Err(FsError::Invalid);
		};
		let mut block = [0u8; SECTOR_SIZE];
		if !self.dev.read_block(at, &mut block) {
			return Err(FsError::Io);
		}
		if !validate_descriptor(&block, le16(&block[0..2]), lb) {
			return Err(FsError::Invalid);
		}
		// The TYPE the target itself records, checked against what the directory said.
		//
		// `is_dir` came from the File Identifier Descriptor's characteristics byte and nothing
		// compared it with the ICB's own file type - so the two could disagree, after which a
		// regular file's contents get parsed as a stream of FIDs, or a directory is served as file
		// data. The caller passes what the FID claimed and this refuses a mismatch.
		match le16(&block[0..2]) {
			TAG_FILE_ENTRY | TAG_EXT_FILE_ENTRY => {
				// ICB file type: 4 is a directory, 5 a regular file.
				let entry_is_dir = block[27] == 4;
				if entry_is_dir != expect_dir {
					return Err(FsError::Corrupt);
				}
				Ok(le64(&block[56..64]))
			}
			_ => Err(FsError::Invalid),
		}
	}

	// Read a File Entry's data: inline (embedded) bytes, or short / long allocation
	// extents followed to the information length. Every value comes off the medium, so
	// the ICB block, the information length, the descriptor region, and every extent
	// are bounded by the partition before a buffer is allocated or a block read; an
	// unrecorded (sparse) extent reads as zeros, never as stale disk content.
	fn read_icb(&mut self, lb: u32) -> Result<Vec<u8>, FsError> {
		let Some(at) = self.geo.physical(0, lb) else {
			return Err(FsError::Invalid);
		};
		let mut block = [0u8; SECTOR_SIZE];
		if !self.dev.read_block(at, &mut block) {
			return Err(FsError::Io);
		}
		// the tag checksum gates garbage; the tag location gates a descriptor copied to
		// the wrong block (its recorded address must be its own).
		if !validate_descriptor(&block, le16(&block[0..2]), lb) {
			return Err(FsError::Invalid);
		}
		// The ICB Strategy Type: 4 (direct) and 4096 (hierarchical) are what a conforming reader
		// supports, and this one reads the File Entry directly - which is strategy 4. A 4096 volume
		// was parsed as though it were a 4, so a wrong answer where a named refusal belongs.
		let strategy = le16(&block[28..30]);
		if strategy != 4 {
			return Err(FsError::Invalid);
		}
		let tag = le16(&block[0..2]);
		let (header, l_ea_off, l_ad_off) = match tag {
			TAG_FILE_ENTRY => (176usize, 168usize, 172usize),
			TAG_EXT_FILE_ENTRY => (216usize, 208usize, 212usize),
			_ => return Err(FsError::Invalid),
		};
		// `try_from`, not `as`: a 64-bit length from the medium cast to `usize` and then added to an
		// offset is an overflow waiting for a hostile descriptor.
		let Ok(info_len) = usize::try_from(le64(&block[56..64])) else {
			return Err(FsError::Invalid);
		};
		let l_ea = le32(&block[l_ea_off..l_ea_off + 4]) as usize;
		let l_ad = le32(&block[l_ad_off..l_ad_off + 4]) as usize;
		let alloc = le16(&block[34..36]) & 0x07;
		// a symlink File Entry (ICB file type 12) stores its target path as data - the
		// volume API has no symlink semantics, so it refuses rather than serves path
		// bytes as file content.
		if block[27] == 12 {
			return Err(FsError::Invalid);
		}
		let ad_off = header + l_ea;
		if ad_off > block.len() {
			return Err(FsError::Invalid);
		}
		// What ONE read may allocate, which is not what the partition could hold.
		//
		// `info_len` was checked against the partition's byte size and then allocated whole, so a
		// 100 GB partition made an 80 GB `InformationLength` "valid" and the allocation was
		// attempted - and `find_entry` reads a whole directory the same way, so a path lookup was
		// enough to trigger it. The real fix is `read_at(offset, buf)` on the volume API, which is
		// somebody else's surface; the ceiling here is what this crate can do about it today.
		if info_len > MAX_READ_BYTES {
			return Err(FsError::TooLong);
		}
		// The partition bound BEFORE the embedded branch, not after it.
		//
		// The order was the other way round, so `InformationLength = u64::MAX` reached
		// `ad_off + info_len` first: an overflow in debug and, in release, a wrap to
		// `block[176..175]` - a panic either way. A merely large value clamped with `.min()` and
		// returned the tail of the block as `Ok`, so a file the descriptor calls 5000 bytes came
		// back as 1872. A panic and a silent truncation from the same three lines.
		if info_len as u64 > self.geo.part_len as u64 * SECTOR_SIZE as u64 {
			return Err(FsError::Invalid);
		}
		// embedded: the file's bytes sit inline in the File Entry.
		if alloc == 3 {
			// The embedded data has to fit in the descriptor area it is declared in, and in the
			// block. A length past either is corruption, not a length to shorten.
			let Some(end) = ad_off.checked_add(info_len) else {
				return Err(FsError::Invalid);
			};
			if end > block.len() || info_len > l_ad {
				return Err(FsError::Corrupt);
			}
			return Ok(block[ad_off..end].to_vec());
		}
		// only short_ad, long_ad, and embedded forms exist on real media - extended_ad
		// (20-byte records) and the reserved values are refused rather than misparsed
		// with the wrong step.
		if alloc != 0 && alloc != 1 {
			return Err(FsError::Invalid);
		}
		// short_ad (8 bytes) or long_ad (16 bytes) extents, read to the info length; the
		// descriptor region is clamped to the File Entry block it lives in.
		let step = if alloc == 1 { 16 } else { 8 };
		// A declared descriptor length that runs past the block is a CORRUPT descriptor, not one to
		// quietly shorten - clamping it turned "the medium says something impossible" into "the
		// chain ends here", which the loop below then reported as a complete file.
		let Some(ad_end) = ad_off.checked_add(l_ad) else {
			return Err(FsError::Invalid);
		};
		if ad_end > block.len() {
			return Err(FsError::Corrupt);
		}
		let mut out = vec![0u8; info_len];
		let mut done = 0usize;
		let mut ad = ad_off;
		while done < info_len && ad + step <= ad_end {
			let raw = le32(&block[ad..ad + 4]);
			let len = (raw & 0x3fff_ffff) as usize;
			let ext_type = raw >> 30;
			let lba = le32(&block[ad + 4..ad + 8]);
			// a zero-length extent terminates the sequence; a type-3 entry chains to
			// further descriptors - not followed, refused rather than read as data.
			if len == 0 {
				break;
			}
			if ext_type == 3 {
				return Err(FsError::Invalid);
			}
			// The DECLARED extent, bounded before anything is taken from it.
			//
			// The check below used to be against `take` - how much of it the information length
			// still wanted - so a descriptor claiming a gigabyte starting at the partition's last
			// block passed as long as only five bytes were wanted. Nothing was read out of bounds,
			// and the descriptor was still structurally impossible; the header says every extent is
			// bounded by the partition, so it is bounded here.
			if lba as u64 + (len as u64).div_ceil(SECTOR_SIZE as u64) > self.geo.part_len as u64 {
				return Err(FsError::Invalid);
			}
			let take = len.min(info_len - done);
			if ext_type != 0 {
				// an unrecorded extent (allocated or not) has no written data - it
				// reads as zeros, never as whatever the disk blocks hold.
				done += take;
				ad += step;
				continue;
			}
			// the extent must lie inside the partition, or it would read foreign blocks.
			if lba as u64 + (take as u64).div_ceil(SECTOR_SIZE as u64) > self.geo.part_len as u64 {
				return Err(FsError::Invalid);
			}
			let Some(mut cur) = self.geo.physical(0, lba) else {
				return Err(FsError::Invalid);
			};
			// The whole contiguous extent in ONE call. `fs-core` offers `read_blocks` so a run like
			// this is one device round trip; a block at a time is a message per 2 KiB behind the
			// IPC-backed block device the storage service hands over.
			let whole = take / SECTOR_SIZE;
			if whole > 0 {
				if !self.dev.read_blocks(cur, whole as u64, &mut out[done..done + whole * SECTOR_SIZE]) {
					return Err(FsError::Io);
				}
				done += whole * SECTOR_SIZE;
				cur += whole as u64;
			}
			// The tail, when the extent does not end on a block boundary.
			let mut left = take - whole * SECTOR_SIZE;
			// the data lands in its own buffer - `block` still holds the File Entry,
			// whose remaining descriptors the scan parses after this extent.
			let mut data = [0u8; SECTOR_SIZE];
			while left > 0 {
				if !self.dev.read_block(cur, &mut data) {
					return Err(FsError::Io);
				}
				let n = left.min(SECTOR_SIZE);
				out[done..done + n].copy_from_slice(&data[..n]);
				done += n;
				left -= n;
				cur += 1;
			}
			ad += step;
		}
		// The whole file, or none of it. The buffer starts as zeros and the loop exits on a
		// zero-length extent, on running out of descriptor area, or on `l_ad` being too small - and
		// then returned `Ok` regardless, so a file declaring 100 KiB whose descriptors cover 2 KiB
		// came back as 2 KiB of data and 98 KiB of zeros, indistinguishable from a real file.
		//
		// A SPARSE extent is a legitimate source of zeros and is counted above; a MISSING descriptor
		// is not, and conflating them is the silent corruption this crate exists to avoid.
		if done != info_len {
			return Err(FsError::Corrupt);
		}
		Ok(out)
	}
}

// Verify a descriptor tag: byte 4 is the checksum of the other fifteen tag bytes,
// mandatory in the format - a garbage block must not parse as a descriptor.
fn tag_ok(block: &[u8]) -> bool {
	let mut sum = 0u8;
	for (i, &b) in block[..16].iter().enumerate() {
		if i != 4 {
			sum = sum.wrapping_add(b);
		}
	}
	sum == block[4]
}

// The CRC-ITU-T (CCITT) polynomial UDF uses for `DescriptorCRC`, computed the way ECMA-167 defines.
fn crc_ccitt(bytes: &[u8]) -> u16 {
	let mut crc: u16 = 0;
	for &b in bytes {
		crc ^= (b as u16) << 8;
		for _ in 0..8 {
			crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
		}
	}
	crc
}

// A descriptor is what it says it is: the right tag, its own address, an intact TAG, and an intact
// BODY.
//
// The tag checksum covers the first sixteen bytes and was the only thing checked - so a bit flipped
// in a partition start, a File Set location, an `InformationLength` or an allocation descriptor
// passed every test this parser made. `DescriptorCRC` and `DescriptorCRCLength` cover the body and
// exist for exactly that, and on optical media - which is what this backend is for - that is the
// protection that matters most.
//
// One function rather than four call sites checking different subsets of the same four things.
fn validate_descriptor(block: &[u8], expected_tag: u16, expected_location: u32) -> bool {
	if block.len() < 16 || !tag_ok(block) {
		return false;
	}
	if le16(&block[0..2]) != expected_tag || le32(&block[12..16]) != expected_location {
		return false;
	}
	let crc_len = le16(&block[10..12]) as usize;
	// A zero CRC length is how a descriptor says it carries no body CRC. UDF requires one, so this
	// is a descriptor that does not conform rather than one to wave through.
	if crc_len == 0 || 16 + crc_len > block.len() {
		return false;
	}
	crc_ccitt(&block[16..16 + crc_len]) == le16(&block[8..10])
}

// Decode a UDF d-string file identifier: the first byte is the compression id (8 =
// 8-bit Latin-1, 16 = 16-bit UCS-2 big-endian); the rest are the characters. An unknown
// id yields an empty name (the record is then skipped), never noise decoded as text.
fn decode_name(id: &[u8]) -> String {
	if id.is_empty() {
		return String::new();
	}
	let mut s = String::new();
	if id[0] == 16 {
		// An odd length is corruption and an invalid code unit is corruption; neither may become a
		// name. `unwrap_or('?')` mapped every bad unit to one character, so two different malformed
		// names collided - and `chunks_exact(2)` dropped a trailing odd byte on the way.
		if id[1..].len() % 2 != 0 {
			return String::new();
		}
		for c in id[1..].chunks_exact(2) {
			let Some(ch) = char::from_u32(u16::from_be_bytes([c[0], c[1]]) as u32) else {
				return String::new();
			};
			s.push(ch);
		}
	} else if id[0] == 8 {
		for &b in &id[1..] {
			s.push(b as char);
		}
	}
	s
}

// Split a `/`-separated path into (parent dir, final name); errors on an empty name.
fn split_parent(path: &[u8]) -> Result<(&[u8], &[u8]), FsError> {
	let path = path.strip_prefix(b"/").unwrap_or(path);
	match path.iter().rposition(|&b| b == b'/') {
		Some(i) => Ok((&path[..i], &path[i + 1..])),
		None => Ok((b"", path)),
	}
}

// Case-insensitive ASCII name compare, consistent with the sibling backends behind the
// volume API. UDF itself is case-sensitive-preserving - two names differing only in
// case are legal siblings there - so the first match wins and a case-distinct sibling
// is shadowed, by decision.
fn eq_ci(a: &str, b: &[u8]) -> bool {
	a.len() == b.len() && a.bytes().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

// A little-endian u16 from a 2-byte slice.
fn le16(b: &[u8]) -> u16 {
	u16::from_le_bytes([b[0], b[1]])
}

// A little-endian u32 from a 4-byte slice.
fn le32(b: &[u8]) -> u32 {
	u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

// A little-endian u64 from an 8-byte slice.
fn le64(b: &[u8]) -> u64 {
	u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}
