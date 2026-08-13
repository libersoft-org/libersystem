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
	root_icb: LogicalAddress,
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
	fn physical(&self, at: LogicalAddress) -> Option<u64> {
		// One physical partition is the whole of this reader's subset, so anything else is refused
		// HERE rather than misread somewhere else.
		if at.partition != 0 || at.lb >= self.part_len {
			return None;
		}
		Some(self.part_start as u64 + at.lb as u64)
	}
}

// A file's bytes with the logical block each of them came from.
//
// The runs are `(byte_offset, first_logical_block)`, one per extent, in offset order; inside a run
// the block advances every `SECTOR_SIZE` bytes. That is enough to turn any offset in the flattened
// buffer back into the block it was read from, which is what a FID's Descriptor Tag location has to
// be checked against.
struct Flattened {
	data: Vec<u8>,
	runs: Vec<(usize, u32)>,
}

impl Flattened {
	// The logical block holding the byte at `offset`, or None when the offset is past everything
	// that was read (which the callers treat as corruption, since a FID must lie in the data).
	fn block_at(&self, offset: usize) -> Option<u32> {
		let mut found: Option<(usize, u32)> = None;
		for &(start, lb) in &self.runs {
			if start <= offset {
				found = Some((start, lb));
			} else {
				break;
			}
		}
		let (start, lb) = found?;
		let blocks = ((offset - start) / SECTOR_SIZE) as u32;
		lb.checked_add(blocks)
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
		let mut io_error = false;
		for &anchor in &anchors {
			// An UNREADABLE anchor is the device, not the medium. With every anchor failing to read,
			// this fell out of the loop and answered `NotUdf` - so one bad sector reported a
			// perfectly good UDF disc as not being UDF at all, which is the exact outcome this error
			// type was added to prevent.
			if anchor != 0 && !dev.read_block(anchor, &mut block) {
				io_error = true;
				continue;
			}
			if anchor == 0 || !validate_descriptor(&block, TAG_AVDP, anchor as u32) {
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
			return Err(if io_error { MountError::Io } else { MountError::NotUdf });
		}
		let mut found: Option<(u32, u32, u32)> = None;
		// The reason the LAST sequence gave, kept so a volume whose Main and Reserve both refuse is
		// reported by WHY rather than as a generic corruption. A Reserve sequence that succeeds
		// still wins - this is only consulted when none of them did.
		let mut refusal = MountError::Corrupt;
		for &(vds_len, vds_loc) in sequences.iter().take(sequence_count) {
			match Self::scan_sequence(&mut dev, vds_len, vds_loc) {
				Ok(triple) => {
					found = Some(triple);
					break;
				}
				Err(why) => refusal = why,
			}
		}
		// An anchor was found and no sequence yielded a partition and a File Set: the volume claims
		// to be UDF and either does not hold together or uses something this reader does not
		// implement - and those are different things to tell a caller.
		let Some((part_start, part_len, fileset_lb)) = found else {
			return Err(refusal);
		};
		Self::finish_mount(dev, part_start, part_len, fileset_lb)
	}

	// One volume descriptor sequence: the partition and the File Set it names, or None when this
	// sequence cannot supply them - in which case the caller tries the next one.
	// `Result`, not `Option`, because the three answers are not the same answer.
	//
	// `MountError` was declared with four variants and only two could ever be built: everything this
	// function refused - a 4096-byte block size, a Type-2 map, more than one map, a partition number
	// that does not match its map - came back as `None` and `mount_checked` turned it into `Corrupt`,
	// blaming the medium for a shape this reader simply does not implement. A failed `read_block`
	// came back the same way, blaming the medium for the device.
	fn scan_sequence(dev: &mut D, vds_len: u32, vds_loc: u32) -> Result<(u32, u32, u32), MountError> {
		let mut block = [0u8; SECTOR_SIZE];
		let mut part: Option<(u32, u32, u32)> = None;
		// The partition number the LVD's map names, and the number the partition descriptor carries.
		// The parser kept one global partition and assumed the two referred to each other.
		let mut map_partition: Option<u16> = None;
		let mut part_number: Option<u16> = None;
		let mut lvd: Option<(u32, [u8; SECTOR_SIZE])> = None;
		let mut fileset_lb: Option<(u32, u32)> = None;
		// the sequence length is the medium's claim - a real MVDS is a handful of
		// descriptors, so the scan is clamped rather than driven megablocks far.
		// The sequence length, rounded UP and not floored, and a zero length refused rather than
		// turned into one block: a descriptor the anchor does not claim is in the sequence was
		// getting parsed, and a partial last block was getting dropped. The ceiling stays - a real
		// Main VDS is a handful of descriptors and a forged length must not drive the scan
		// megablocks far - but reaching it is a refusal rather than a quiet stop.
		if vds_len == 0 {
			return Err(MountError::Corrupt);
		}
		let count = (vds_len as usize).div_ceil(SECTOR_SIZE);
		if count > 64 {
			return Err(MountError::Corrupt);
		}
		for i in 0..count as u64 {
			// The DEVICE, not the medium. This answered the same way a damaged descriptor does, so
			// an unreadable sector was reported as a corrupt volume.
			if !dev.read_block(vds_loc as u64 + i, &mut block) {
				return Err(MountError::Io);
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
				TAG_PARTITION if part.is_none_or(|(seen, _, _)| vdsn >= seen) => {
					// PartitionNumber at 22, which nothing read - so the map could name partition 3
					// and the descriptor be partition 0 and the two were used together anyway.
					part_number = Some(le16(&block[22..24]));
					part = Some((vdsn, le32(&block[188..192]), le32(&block[192..196])));
				}
				// GUARDED BY THE SEQUENCE NUMBER, like the partition descriptor beside it.
				//
				// `TAG_PARTITION` has `vdsn >= seen` and this arm had nothing, and it `return None`s
				// from inside the loop on an unsupported block size or domain identifier - so an
				// OLDER logical volume descriptor describing something this reader refuses ended the
				// scan before the newer, supported one was seen. On exactly the rewritable media the
				// prevailing-descriptor rule exists for.
				// COLLECTED, then judged - which the guard above this could not do.
				//
				// The comment that guard carries names the defect exactly: an older logical volume
				// descriptor describing something this reader refuses "ended the scan before the
				// newer, supported one was seen". It does not fix it. `vdsn >= seen` stops an older
				// descriptor OVERWRITING a newer one's data; the `return Err` inside the arm stops
				// the scan, and it runs before any higher VDSN can appear.
				//
				// So nothing is refused from in here. The candidate with the highest VDSN is kept
				// and the refusals happen after the sequence has been walked, where "is there a
				// better one" is a question with an answer.
				TAG_LOGICAL_VOLUME if lvd.is_none_or(|(seen, _): (u32, [u8; SECTOR_SIZE])| vdsn >= seen) => {
					lvd = Some((vdsn, block));
				}
				TAG_TERMINATING => break,
				_ => {}
			}
		}
		// THE PREVAILING LVD, judged now that no higher VDSN can appear.
		let Some((_, block)) = lvd else {
			return Err(MountError::Corrupt);
		};
		// What UDF is this, and can this reader address it at all? None of these three was read
		// before this milestone: the parser assumed 2048-byte blocks throughout and had no way to
		// say whether it was looking at 1.02 or 2.60.
		if le32(&block[212..216]) != SECTOR_SIZE as u32 {
			return Err(MountError::Unsupported);
		}
		// The Domain Identifier, which says this is a UDF volume and not merely an ECMA-167 one this
		// reader has no business interpreting.
		if &block[217..236] != b"*OSTA UDF Compliant" {
			return Err(MountError::Unsupported);
		}
		// AND WHICH UDF THIS IS. The revision lives in the domain identifier's suffix - two bytes,
		// BCD, little-endian.
		//
		// AN EXACT LIST, not a range. `0x0102..=0x0260` admits 1.03, 1.49, 2.02, 2.37 and every
		// other value in between, none of which is a revision that exists. The published ones are
		// what this accepts, which is also the list a reader can check against the specification
		// rather than take on trust.
		let revision = le16(&block[240..242]);
		if !matches!(revision, 0x0102 | 0x0150 | 0x0200 | 0x0201 | 0x0250 | 0x0260) {
			return Err(MountError::Unsupported);
		}
		// EXACTLY one Type-1 (physical) partition map. Every logical address here is resolved
		// against the one physical partition, which is correct for this shape and silently wrong for
		// any other.
		let map_len = le32(&block[264..268]) as usize;
		let map_count = le32(&block[268..272]);
		if map_count != 1 || map_len < 2 || 440 + map_len > block.len() {
			return Err(MountError::Unsupported);
		}
		// AND THE CRC HAS TO COVER THE MAP: its length is declared rather than fixed, so the bound
		// belongs where the declared length is known.
		if 16 + (le16(&block[10..12]) as usize) < 440 + map_len {
			return Err(MountError::Corrupt);
		}
		// A TYPE-1 MAP IS SIX BYTES: type, length, volume sequence number, partition number.
		if block[440] != 1 || block[441] as usize != 6 || map_len != 6 {
			return Err(MountError::Unsupported);
		}
		if le16(&block[442..444]) != 1 {
			return Err(MountError::Unsupported);
		}
		let map_partition = Some(le16(&block[444..446]));
		let fileset_lb = Some((0u32, le32(&block[252..256])));
		// The File Set's partition reference, which was discarded. With one map it can only be
		// partition 0, and requiring that is what makes the discard safe.
		if le16(&block[256..258]) != 0 {
			return Err(MountError::Unsupported);
		}
		let (Some((_, part_start, part_len)), Some((_, fileset_lb))) = (part, fileset_lb) else {
			return Err(MountError::Corrupt);
		};
		// THE MAP AND THE DESCRIPTOR MUST BE TALKING ABOUT THE SAME PARTITION. Both numbers are read
		// now; before, neither was, and the reader used the one partition descriptor it had found
		// for whatever the map referred to.
		let (Some(map_partition), Some(part_number)) = (map_partition, part_number) else {
			return Err(MountError::Corrupt);
		};
		if map_partition != part_number {
			return Err(MountError::Unsupported);
		}
		Ok((part_start, part_len, fileset_lb))
	}

	// The checks that do not depend on which sequence supplied the answer.
	// `Result`, not `Option` - so the two device reads below stay `Io`.
	//
	// The caller did `.ok_or(MountError::Corrupt)`, and this function performs the last two reads of
	// a mount: the partition-end probe and the File Set. A device that failed either was reported as
	// a corrupt volume, which is the exact conflation `MountError` was introduced to end and the one
	// StorageService now depends on to decide between `Again` and a refusal.
	fn finish_mount(mut dev: D, part_start: u32, part_len: u32, fileset_lb: u32) -> Result<Udf<D>, MountError> {
		let mut block = [0u8; SECTOR_SIZE];
		// the partition length bounds every partition-relative address; a zero length or
		// a File Set outside it cannot form a volume, and the partition's last block must
		// exist on the device - or a forged or truncated image mounts and only fails, or
		// allocates without bound, inside a later read (the real media size then bounds
		// every extent).
		if part_len == 0 || fileset_lb >= part_len {
			return Err(MountError::Corrupt);
		}
		if !dev.read_block(part_start as u64 + part_len as u64 - 1, &mut block) {
			return Err(MountError::Io);
		}
		if !dev.read_block(part_start as u64 + fileset_lb as u64, &mut block) {
			return Err(MountError::Io);
		}
		if !validate_descriptor(&block, TAG_FILE_SET, fileset_lb) {
			return Err(MountError::Corrupt);
		}
		// The root ICB is a `long_ad`, and its PARTITION was thrown away here - only the block
		// number was read. Kept now, so a File Set naming another partition is refused by the
		// resolver rather than read as partition 0.
		let Some(root_icb) = LogicalAddress::from_long_ad(&block, 400) else {
			return Err(MountError::Corrupt);
		};
		if root_icb.lb >= part_len {
			return Err(MountError::Corrupt);
		}
		Ok(Udf { dev, geo: Geometry { part_start, part_len, root_icb } })
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
		// The FID said this is not a directory; the File Entry has to agree.
		self.read_icb(icb, Some(false))
	}

	// Walk path segments from the root, descending into each named subdirectory, and
	// return the final directory's ICB. An empty path is the root.
	fn resolve_dir(&mut self, path: &[u8]) -> Result<LogicalAddress, FsError> {
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
	fn find_entry(&mut self, dir_icb: LogicalAddress, name: &[u8]) -> Result<(LogicalAddress, bool), FsError> {
		let dir = self.read_icb_mapped(dir_icb, Some(true))?;
		let data = &dir.data;
		let mut off = 0usize;
		// An EXACT match wins outright; a case-folded one is a fallback, and an ambiguous fallback
		// is an error rather than whichever record came first. `README`, `Readme` and `readme` can
		// all exist on a UDF volume, and answering with the first was a coin toss that made the
		// others unreachable by their own names.
		let mut folded: Option<(LogicalAddress, bool)> = None;
		let mut folded_ambiguous = false;
		// A DIRECTORY'S RECORDS TILE ITS EXTENT. Anything from one to thirty-seven bytes after the
		// last FID ends this walk and the listing returns `Ok`, so a directory whose records do not
		// tile its extent exactly reads as a healthy one that is simply missing a file. The check
		// after the loop is what says otherwise.
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
			// AND ITS BODY CRC, through the same function everything else goes through.
			//
			// A FID was checked with `tag_ok` alone - the sixteen-byte tag checksum - so the name,
			// the File Characteristics byte, the child ICB address and the partition reference could
			// all be altered and the medium only had to fix up one byte. The FID is the descriptor
			// that decides what a file is called, what it is, and where it lives; it is the last one
			// that should be exempt.
			//
			// A FID's length is its own, not the buffer's, which is why this takes an end - and the
			// end is the PADDED length, because that is the record a writer computed its CRC over.
			//
			// AND THE COVERAGE IS `38 + l_iu + l_fi`, not the constant 38. The name is read from
			// `fid[38 + l_iu .. 38 + l_iu + l_fi]`, so a forged `DescriptorCRCLength` of 22 covers
			// bytes 16..38 - the whole fixed part, including the child ICB at 20 - passes, and
			// leaves every byte of the file identifier outside the range the CRC was computed over.
			// The name can then be edited freely and the CRC still matches, which is the finding
			// this milestone opened with, one field further in.
			// THE FID'S OWN BLOCK, which used to be `None` under a comment saying a FID has no
			// address to check. It has: the Descriptor Tag location is the logical block the
			// descriptor's first byte is in, and `Flattened` kept that through the flattening. A FID
			// copied from another directory - the exact forgery this field exists to catch - now
			// fails here instead of listing as a file.
			let Some(here) = dir.block_at(off) else {
				return Err(FsError::Corrupt);
			};
			if !validate_descriptor_covering(fid, TAG_FILE_ID, Some(here), (total + 3) & !3, total) {
				return Err(FsError::Corrupt);
			}
			let chars = fid[18];
			let parent = chars & 0x08 != 0;
			let deleted = chars & 0x04 != 0;
			let is_dir = chars & 0x02 != 0;
			// A name that does not decode is a CORRUPT record, not one to pass over: skipping it
			// listed a damaged directory as a tidy one.
			let Some(id) = decode_name(&fid[38 + l_iu..38 + l_iu + l_fi]) else {
				return Err(FsError::Corrupt);
			};
			if parent {
				if name == b".." {
					return Ok((LogicalAddress::from_long_ad(fid, 20).ok_or(FsError::Corrupt)?, is_dir));
				}
			} else if !deleted && !id.is_empty() {
				if id.as_bytes() == name {
					return Ok((LogicalAddress::from_long_ad(fid, 20).ok_or(FsError::Corrupt)?, is_dir));
				}
				if eq_ci(&id, name) {
					if folded.is_some() {
						folded_ambiguous = true;
					}
					let child = LogicalAddress::from_long_ad(fid, 20).ok_or(FsError::Corrupt)?;
					folded = Some((child, is_dir));
				}
			}
			off += (total + 3) & !3;
		}
		// The records have to tile the extent exactly: a tail too short to be a record is a
		// directory whose structure does not add up, and reading it as one that simply ends there
		// is the same silent-shortening this reader refuses everywhere else.
		if off != data.len() {
			return Err(FsError::Corrupt);
		}
		if folded_ambiguous {
			return Err(FsError::BadName);
		}
		folded.ok_or(FsError::NotFound)
	}

	// Read every File Identifier in a directory into FileInfos, skipping the parent
	// entry, deleted records, and empty names. The size column comes from the child's
	// File Entry HEADER - a listing never pulls file contents through the device.
	fn read_dir(&mut self, dir_icb: LogicalAddress) -> Result<Vec<FileInfo>, FsError> {
		let dir = self.read_icb_mapped(dir_icb, Some(true))?;
		let data = &dir.data;
		let mut out = Vec::new();
		let mut off = 0usize;
		// A DIRECTORY'S RECORDS TILE ITS EXTENT. Anything from one to thirty-seven bytes after the
		// last FID ends this walk and the listing returns `Ok`, so a directory whose records do not
		// tile its extent exactly reads as a healthy one that is simply missing a file. The check
		// after the loop is what says otherwise.
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
			// AND ITS BODY CRC, through the same function everything else goes through.
			//
			// A FID was checked with `tag_ok` alone - the sixteen-byte tag checksum - so the name,
			// the File Characteristics byte, the child ICB address and the partition reference could
			// all be altered and the medium only had to fix up one byte. The FID is the descriptor
			// that decides what a file is called, what it is, and where it lives; it is the last one
			// that should be exempt.
			//
			// A FID's length is its own, not the buffer's, which is why this takes an end - and the
			// end is the PADDED length, because that is the record a writer computed its CRC over.
			//
			// AND THE COVERAGE IS `38 + l_iu + l_fi`, not the constant 38. The name is read from
			// `fid[38 + l_iu .. 38 + l_iu + l_fi]`, so a forged `DescriptorCRCLength` of 22 covers
			// bytes 16..38 - the whole fixed part, including the child ICB at 20 - passes, and
			// leaves every byte of the file identifier outside the range the CRC was computed over.
			// The name can then be edited freely and the CRC still matches, which is the finding
			// this milestone opened with, one field further in.
			// THE FID'S OWN BLOCK, which used to be `None` under a comment saying a FID has no
			// address to check. It has: the Descriptor Tag location is the logical block the
			// descriptor's first byte is in, and `Flattened` kept that through the flattening. A FID
			// copied from another directory - the exact forgery this field exists to catch - now
			// fails here instead of listing as a file.
			let Some(here) = dir.block_at(off) else {
				return Err(FsError::Corrupt);
			};
			if !validate_descriptor_covering(fid, TAG_FILE_ID, Some(here), (total + 3) & !3, total) {
				return Err(FsError::Corrupt);
			}
			let chars = fid[18];
			if chars & 0x08 == 0 && chars & 0x04 == 0 {
				let is_dir = chars & 0x02 != 0;
				let Some(id) = decode_name(&fid[38 + l_iu..38 + l_iu + l_fi]) else {
					return Err(FsError::Corrupt);
				};
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
					match self.icb_size(LogicalAddress::from_long_ad(fid, 20).ok_or(FsError::Corrupt)?, is_dir) {
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
		if off != data.len() {
			return Err(FsError::Corrupt);
		}
		Ok(out)
	}

	// The information length recorded in a File Entry's header - the size a listing
	// reports, read from the one header block instead of the whole content.
	fn icb_size(&mut self, at: LogicalAddress, expect_dir: bool) -> Result<u64, FsError> {
		let Some(block_at) = self.geo.physical(at) else {
			return Err(FsError::Invalid);
		};
		let mut block = [0u8; SECTOR_SIZE];
		if !self.dev.read_block(block_at, &mut block) {
			return Err(FsError::Io);
		}
		// `Corrupt`, and DELIBERATELY not `Invalid`.
		//
		// Every semantic refusal below this - an extended_ad form, a symlink, an unsupported ICB
		// strategy - answers `Invalid`. When a failed checksum answered it too, a test that mutated
		// a descriptor and forgot to recompute its CRC got the answer it was asserting from a
		// completely different check, and its own branch was never reached. Three tests in this file
		// were passing that way. A distinct error means the next forgotten refresh fails loudly
		// instead of quietly agreeing.
		if !validate_descriptor(&block, le16(&block[0..2]), at.lb) {
			return Err(FsError::Corrupt);
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
	fn read_icb(&mut self, at: LogicalAddress, expect_dir: Option<bool>) -> Result<Vec<u8>, FsError> {
		Ok(self.read_icb_mapped(at, expect_dir)?.data)
	}

	// A directory's bytes AND where each of them came from.
	//
	// `read_icb` flattens a file's extents into one buffer and throws away which logical block each
	// byte came from - which is why the FID walkers passed `None` as the expected tag location and
	// said in a comment that "a FID does not have an address of its own to check". It has one: a
	// Descriptor Tag's location is the logical block holding the descriptor's first byte, and a FID
	// lives in a block of the directory extent it was read from. The number was not unavailable, it
	// was DISCARDED - and with it unchecked, a FID copied from one directory into another passes
	// every test this parser makes, which is precisely the class the location field exists to catch.
	fn read_icb_mapped(&mut self, at: LogicalAddress, expect_dir: Option<bool>) -> Result<Flattened, FsError> {
		let Some(block_at) = self.geo.physical(at) else {
			return Err(FsError::Invalid);
		};
		let mut block = [0u8; SECTOR_SIZE];
		if !self.dev.read_block(block_at, &mut block) {
			return Err(FsError::Io);
		}
		// the tag checksum gates garbage; the tag location gates a descriptor copied to
		// the wrong block (its recorded address must be its own).
		// `Corrupt`, and DELIBERATELY not `Invalid`.
		//
		// Every semantic refusal below this - an extended_ad form, a symlink, an unsupported ICB
		// strategy - answers `Invalid`. When a failed checksum answered it too, a test that mutated
		// a descriptor and forgot to recompute its CRC got the answer it was asserting from a
		// completely different check, and its own branch was never reached. Three tests in this file
		// were passing that way. A distinct error means the next forgotten refresh fails loudly
		// instead of quietly agreeing.
		if !validate_descriptor(&block, le16(&block[0..2]), at.lb) {
			return Err(FsError::Corrupt);
		}
		// The ICB Strategy Type: 4 (direct) and 4096 (hierarchical) are what a conforming reader
		// supports, and this one reads the File Entry directly - which is strategy 4. A 4096 volume
		// was parsed as though it were a 4, so a wrong answer where a named refusal belongs.
		// THE FILE ENTRY'S OWN TYPE, checked against what the FID claimed.
		//
		// `icb_size` does this and runs when a directory is LISTED; `read_file` takes `is_dir` from
		// the FID, refuses a directory, and then came here without ever asking the File Entry
		// whether it agreed. A crafted FID claiming "regular file" over a File Entry that is a
		// directory therefore read the directory's bytes as file content - the finding, still open
		// on the path a caller actually uses.
		//
		// Type 4 is a directory and 5 a regular file; the caller says which it expected.
		if let Some(want_dir) = expect_dir {
			let is_dir = block[27] == 4;
			if is_dir != want_dir {
				return Err(FsError::Corrupt);
			}
		}
		// THE ICB TAG'S STRATEGY IS AT 20, NOT 28.
		//
		// The ICB Tag begins at 16: PriorRecordedNumberOfDirectEntries (4), StrategyType (2 at +4),
		// StrategyParameter (2), MaximumNumberOfEntries (2), reserved (1), FileType (1 at +11),
		// ParentICBLocation (6 at +12), Flags (2 at +18). So 28 is the low half of the PARENT ICB's
		// block number, and the strategy was never being read at all.
		//
		// The synthetic fixtures wrote 4 at 28 and agreed with the mistake, which is why an internal
		// round trip could not see it - the same shape as the terminator, the descriptor CRC, the
		// version and the partition map before it. A real `mkfs.udf` volume's parent ICB is not 4,
		// so no directory on real media has ever been readable: the independent-formatter test
		// mounted such an image and stopped before listing it.
		let strategy = le16(&block[20..22]);
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
		// AND THE CRC HAS TO COVER THE ALLOCATION AREA. `crc_must_cover` bounds a File Entry to 216
		// - `InformationLength` at 56, the ICB tag through 212, `l_ad` at 172 - and the allocation
		// descriptors are not at a constant offset: they are at `header + l_ea`, and each one gives
		// an extent length, an extent type, a logical block and a partition that this reader then
		// FOLLOWS to read data.
		//
		// With a large `l_ea`, or simply enough descriptors, those bytes are past 216 and a declared
		// `DescriptorCRCLength` of 200 leaves them unvouched. This is the more serious of the two:
		// a forged FID name misnames a file, a forged allocation descriptor redirects a read.
		//
		// The embedded case is the same expression, and for the reason it is the same: for ICB
		// strategy with embedded data the bytes this reader RETURNS are `ad_off..ad_off + info_len`,
		// bounded by `l_ad`, so the coverage requirement is `header + l_ea + l_ad` there too.
		let Some(alloc_end) = ad_off.checked_add(l_ad) else {
			return Err(FsError::Invalid);
		};
		if 16 + (le16(&block[10..12]) as usize) < alloc_end.min(block.len()) {
			return Err(FsError::Corrupt);
		}
		// What ONE read may allocate, which is not what the partition could hold.
		//
		// `info_len` was checked against the partition's byte size and then allocated whole, so a
		// 100 GB partition made an 80 GB `InformationLength` "valid" and the allocation was
		// attempted - and `find_entry` reads a whole directory the same way, so a path lookup was
		// enough to trigger it. The real fix is `read_at(offset, buf)` on the volume API, which is
		// somebody else's surface; the ceiling here is what this crate can do about it today.
		// `TooLarge`, not `TooLong`. `fs-core` documents `TooLong` as a name or path that is too
		// long and `TooLarge` as "the answer does not fit in one buffer; a ranged read is the
		// solution" - which is precisely the state of affairs, and the wrong one of the two was
		// being returned.
		if info_len > MAX_READ_BYTES {
			return Err(FsError::TooLarge);
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
			// The embedded bytes live in the File Entry's own block, so every offset in them maps
			// to `at.lb` - one run, and the data is shorter than a block by construction.
			return Ok(Flattened { data: block[ad_off..end].to_vec(), runs: alloc::vec![(0usize, at.lb)] });
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
		// FALLIBLY. `vec![0u8; n]` is infallible, so a length the medium chose - up to the ceiling
		// above, which is 64 MB - could abort a userspace service rather than answer it.
		let mut out: Vec<u8> = Vec::new();
		if out.try_reserve_exact(info_len).is_err() {
			return Err(FsError::NoMemory);
		}
		out.resize(info_len, 0);
		// One entry per extent: the byte offset the extent starts at, and the logical block that
		// byte is in. Within a run the block advances every `SECTOR_SIZE` bytes, which is what
		// `Flattened::block_at` reconstructs.
		let mut runs: Vec<(usize, u32)> = Vec::new();
		let mut done = 0usize;
		let mut ad = ad_off;
		while done < info_len && ad + step <= ad_end {
			let raw = le32(&block[ad..ad + 4]);
			let len = (raw & 0x3fff_ffff) as usize;
			let ext_type = raw >> 30;
			// A `long_ad` carries its partition; a `short_ad` has none and is partition 0 by
			// definition. Reading the block number alone treated the two the same, so a `long_ad`
			// naming another partition was read here as this one.
			let lba = le32(&block[ad + 4..ad + 8]);
			let extent = if step == 16 { LogicalAddress::from_long_ad(&block, ad).ok_or(FsError::Corrupt)? } else { LogicalAddress { lb: lba, partition: 0 } };
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
			if runs.try_reserve(1).is_err() {
				return Err(FsError::NoMemory);
			}
			// The run is recorded for an unrecorded extent too: it reads as zeros, and a FID in it
			// is nonsense, but the offsets after it must still map to the right blocks.
			runs.push((done, extent.lb));
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
			let Some(mut cur) = self.geo.physical(extent) else {
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
		Ok(Flattened { data: out, runs })
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
// The furthest byte this reader TRUSTS in each descriptor it validates, measured from the start of
// the descriptor - so the CRC has to cover at least that far or the fields below it are outside it.
//
// THE LENGTH IS THE MEDIUM'S CLAIM AND NOTHING BOUNDED IT FROM BELOW. The only test was
// `crc_len == 0 || 16 + crc_len > block.len()`, so `DescriptorCRCLength = 1` passed as long as that
// one byte CRC'd - and everything this parser then believed was outside the covered range: the
// partition start at 188, the File Set address at 252, the partition map at 440. The CRC became a
// formality a forger satisfies by declaring less of the descriptor is covered.
//
// The fixtures could not have found it: `tag()` always writes full coverage, because that is what a
// formatter does. A validator whose only exercise is media that behave correctly is checked against
// the honest case alone.
// A UDF address is a BLOCK AND THE PARTITION IT IS IN, and this reader kept only the block.
//
// `Geometry::physical` takes a partition reference and every one of its three callers passed the
// literal `0`, because the references the medium carries were dropped before they reached it: the
// File Set Descriptor's root ICB is a `long_ad` and only its block number was read, a FID's child
// ICB likewise, and a `long_ad` in an allocation list likewise. So a crafted volume naming
// partition 1 for a child ICB was read as partition 0 - the same misinterpretation the resolver was
// written to prevent, happening in three places instead of everywhere.
//
// Carrying the pair means the refusal happens once, in the resolver, which is what it is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct LogicalAddress {
	lb: u32,
	partition: u16,
}

impl LogicalAddress {
	// The `long_ad` at `at`: length(4), block(4), partition(2), implementation use(6).
	fn from_long_ad(bytes: &[u8], at: usize) -> Option<LogicalAddress> {
		let block = bytes.get(at..at + 16)?;
		// THE EXTENT LENGTH, which this read past.
		//
		// A `long_ad` is extent length, logical block address, implementation use - and the length's
		// top two bits are the extent TYPE: 0 recorded and allocated, 1 not recorded but allocated,
		// 2 not recorded and not allocated, 3 the next extent of allocation descriptors. Only the
		// address was read, so an ICB reference recorded as UNALLOCATED, or of zero length, was
		// followed as though it named data.
		let length = le32(&block[0..4]);
		if length >> 30 != 0 || length & 0x3fff_ffff == 0 {
			return None;
		}
		Some(LogicalAddress { lb: le32(&block[4..8]), partition: le16(&block[8..10]) })
	}
}

fn crc_must_cover(tag: u16) -> usize {
	match tag {
		// Main and Reserve sequence extents: two 8-byte `extent_ad`s at 16..32.
		TAG_AVDP => 32,
		// PartitionStartingLocation at 188, PartitionLength at 192.
		TAG_PARTITION => 196,
		// LogicalBlockSize at 212, the domain identifier at 217..236, the File Set `long_ad` at
		// 248..264, the map table length at 264 and the map count at 268. The MAP itself starts at
		// 440 and its length is declared rather than fixed, so its coverage is checked where the map
		// is read - a constant here would be a guess. Measured against `mkfs.udf`: one Type-1 map is
		// six bytes and the descriptor covers to 446.
		TAG_LOGICAL_VOLUME => 440,
		// The root ICB `long_ad` at 400..416.
		TAG_FILE_SET => 416,
		// InformationLength at 56, the ICB tag through 212, `l_ad` at 172. The EXTENDED form is what
		// `mkfs.udf` actually writes for a root - measured at 256 bytes covered, which clears this.
		TAG_FILE_ENTRY | TAG_EXT_FILE_ENTRY => 216,
		// A FID is variable-length; its own `l_iu`/`l_fi` bound it, and the fixed part runs to 38.
		TAG_FILE_ID => 38,
		_ => 16,
	}
}

fn validate_descriptor(block: &[u8], expected_tag: u16, expected_location: u32) -> bool {
	validate_descriptor_within(block, expected_tag, Some(expected_location), block.len())
}

// The same, over a descriptor that does not own the whole buffer - a FID inside a directory's
// bytes, where `end` is the record's own length rather than the block's.
// The same, with a coverage requirement the CALLER computes from the descriptor's own declared
// lengths - which is what a variable-length descriptor needs and what `crc_must_cover`'s constants
// cannot give.
//
// `crc_must_cover` says of the LVD's partition map that its "length is declared rather than fixed,
// so its coverage is checked where the map is read - a constant here would be a guess", and that
// dynamic check exists. A FID's name and a File Entry's allocation descriptors have exactly the same
// property and were left on `38` and `216`: a forged `DescriptorCRCLength` could cover the fixed part
// and leave the name, or the descriptors this reader then follows to read data, outside the range the
// CRC vouches for.
#[cfg_attr(test, allow(dead_code))]
fn validate_descriptor_covering(block: &[u8], expected_tag: u16, expected_location: Option<u32>, end: usize, must_cover: usize) -> bool {
	if !validate_descriptor_within(block, expected_tag, expected_location, end) {
		return false;
	}
	16 + (le16(&block[10..12]) as usize) >= must_cover
}

fn validate_descriptor_within(block: &[u8], expected_tag: u16, expected_location: Option<u32>, end: usize) -> bool {
	if block.len() < 16 || end > block.len() || !tag_ok(block) {
		return false;
	}
	// THE VERSION, which was read past entirely. It is 2 or 3 for the descriptors this format
	// defines, and a reader that does not check it is reading a structure it has not confirmed is
	// the structure it thinks it is.
	let version = le16(&block[2..4]);
	if version != 2 && version != 3 {
		return false;
	}
	if le16(&block[0..2]) != expected_tag {
		return false;
	}
	// A descriptor that records its own address is checked against where it was READ, which is what
	// catches a stale or copied block. A FID does not have an address of its own to check - it lives
	// inside another descriptor's extent - so the caller passes `None` rather than a number it would
	// have to invent.
	if let Some(location) = expected_location
		&& le32(&block[12..16]) != location
	{
		return false;
	}
	let crc_len = le16(&block[10..12]) as usize;
	// A zero CRC length is how a descriptor says it carries no body CRC. UDF requires one, so this
	// is a descriptor that does not conform rather than one to wave through.
	if crc_len == 0 || 16 + crc_len > end {
		return false;
	}
	// And it must cover the fields this reader goes on to trust.
	if 16 + crc_len < crc_must_cover(expected_tag) {
		return false;
	}
	crc_ccitt(&block[16..16 + crc_len]) == le16(&block[8..10])
}

// Decode a UDF d-string file identifier: the first byte is the compression id (8 =
// 8-bit Latin-1, 16 = 16-bit UCS-2 big-endian); the rest are the characters. An unknown
// id yields an empty name (the record is then skipped), never noise decoded as text.
// Decode a CS0 d-string into a name, or NONE when it is not a name.
//
// It returned `String::new()` for every malformed form - an unknown compression id, an odd UTF-16
// length, an invalid scalar - and both directory walkers skipped a record whose name came back
// empty. So a damaged directory listed as a healthy one that was simply missing a file, which is the
// silent-shortening this reader refuses everywhere else. The refusal has to be a value the caller
// cannot mistake for "no name here", and `Option` is that value.
//
// `Some("")` means the field was genuinely empty, which is what a parent record carries and the only
// place an empty name is legal.
fn decode_name(id: &[u8]) -> Option<String> {
	if id.is_empty() {
		return Some(String::new());
	}
	let mut s = String::new();
	if id[0] == 16 {
		// An odd length is corruption and an invalid code unit is corruption; neither may become a
		// name. `unwrap_or('?')` mapped every bad unit to one character, so two different malformed
		// names collided - and `chunks_exact(2)` dropped a trailing odd byte on the way.
		if id[1..].len() % 2 != 0 {
			return None;
		}
		for c in id[1..].chunks_exact(2) {
			let ch = char::from_u32(u16::from_be_bytes([c[0], c[1]]) as u32)?;
			s.push(ch);
		}
	} else if id[0] == 8 {
		for &b in &id[1..] {
			s.push(b as char);
		}
	} else {
		// An unknown compression id: the bytes are not text in any encoding this reader knows.
		return None;
	}
	Some(s)
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

// The coverage rule, reachable from a test without reverse-engineering a whole volume image.
//
// The two call sites compute `must_cover` from lengths a fixture would have to be built around -
// `38 + l_iu + l_fi` inside a directory's bytes, `header + l_ea + l_ad` inside a File Entry - and a
// test that builds those to reach one comparison is testing the fixture. The rule is one line and
// this is what asserts it.
#[cfg(test)]
pub(crate) fn coverage_for_test(block: &[u8], tag: u16, end: usize, must_cover: usize) -> bool {
	validate_descriptor_covering(block, tag, None, end, must_cover)
}

#[cfg(test)]
pub(crate) fn validate_descriptor_within_for_test(block: &[u8], tag: u16, end: usize) -> bool {
	validate_descriptor_within(block, tag, None, end)
}
