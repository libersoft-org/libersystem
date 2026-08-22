//! UDF - a read-only backend for DVD and large optical (`.udf`) media,
//! behind the same [`BlockDevice`] trait FAT, ISO9660 and LiberFS use. It sits behind
//! `Storage.Volume` as just another FS backend: per the layering principle several
//! filesystems mount behind one volume API, and UDF is the format DVDs and Blu-ray
//! discs use, so ISO9660 covers CDs and this covers DVDs. It is NOT a Blu-ray reader:
//! BDMV needs the Metadata Partition Map, which is not implemented - said here in the
//! first sentence rather than a paragraph later, because a reader who stops after one
//! line should not come away with the wrong answer.
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
//! content. One physical partition is supported, and a `long_ad`'s partition reference
//! IS read: it is carried through `LogicalAddress` and a reference this reader has no
//! map for is refused rather than assumed to be zero. (This paragraph said the
//! references were "not interpreted" until 2026-08-15, which described the reader as it
//! was before they were.) The UDF 2.50+ metadata partition (Blu-ray) is not supported -
//! such volumes refuse to mount rather than misread.
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
	// This machine could not get the memory the mount needed. NOT a statement about the medium.
	//
	// It was `Corrupt`, which is the wrong answer in the most expensive direction: it sends a person
	// to replace a disc that is fine, and it makes the failure look permanent when retrying under
	// less pressure would succeed. `LiberFS` has drawn this distinction since its own audit; the
	// three other backends in this tree drew it too, and this one had not.
	NoMemory,
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
		// ALL THREE, when the backing knows how big it is. `BlockDevice::block_count` answers `None`
		// by default - a stream, or a window onto something larger, genuinely does not know - and a
		// backing that does gets the other two anchors for free. A disc whose anchor at 256 is
		// unreadable is exactly the case this redundancy exists for, and it used to mount nothing.
		//
		// `n` is the last addressable block, so the pair is `n - 256` and `n`. Zero entries are
		// skipped by the loop below, which is what a backing with no size or a disc too small for
		// the pair produces.
		let n = dev.block_count().map(|count| count.saturating_sub(1)).unwrap_or(0);
		let anchors = [AVDP_LBA, if n > AVDP_LBA { n - AVDP_LBA } else { 0 }, n];
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
		// THE PREVAILING DESCRIPTOR PER PARTITION NUMBER, which is what the rule is.
		//
		// This kept ONE candidate and took the highest sequence number it saw, recording that
		// descriptor's `PartitionNumber` beside it - so a volume carrying a descriptor for partition
		// 0 at VDSN 10 and one for partition 1 at VDSN 20, with a Type-1 map naming partition 0, was
		// left holding partition 1's. The comparison at the end then refused a volume it should have
		// mounted, reading the right rule off the wrong pair. Safe, since it refuses rather than
		// reading the wrong partition's extent, and the single-partition media this reader is aimed
		// at do not produce it - and the code stated a rule it did not implement.
		//
		// `Vec` rather than a map, because a real Main VDS is a handful of descriptors and the scan
		// is already clamped at 64 blocks: the lookup below is over at most that many entries.
		let mut partitions: Vec<(u16, u32, u32, u32)> = Vec::new();
		let mut lvd: Option<(u32, [u8; SECTOR_SIZE])> = None;
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
			// THE LAST BLOCK MAY BE PARTIAL, and this validated every candidate against the whole
			// 2 kB regardless. `vds_len` is rounded UP to a block count, so a length of 2049 claims
			// one full block plus one byte - and a complete Partition or Logical Volume Descriptor
			// placed in that second block was accepted whole, with every field trusted, though the
			// anchor's extent contains one byte of it. A descriptor the sequence does not claim is
			// not in the sequence.
			let claimed = (vds_len as usize).saturating_sub(i as usize * SECTOR_SIZE);
			if claimed < SECTOR_SIZE {
				break;
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
				TAG_PARTITION => {
					// PartitionNumber at 22, which nothing read - so the map could name partition 3
					// and the descriptor be partition 0 and the two were used together anyway.
					// THE PARTITION'S CONTENTS HAVE TO BE THE ONES THIS READER IMPLEMENTS. The
					// logical volume's Domain Identifier is checked for `*OSTA UDF Compliant` and
					// this descriptor's Partition Contents was not checked at all - so a
					// checksum-correct descriptor for ANOTHER contents format was combined with the
					// UDF logical volume and then read with UDF File Set and ICB rules. For the
					// physical-partition profile this reader supports, the identifier is `+NSR03`.
					// `+NSR02` OR `+NSR03`: the second and third editions of ECMA-167, both of which
					// UDF builds on. MEASURED, because the first attempt required `+NSR03` alone and
					// a real `mkudffs --udfrev=1.02` image does not contain that string anywhere -
					// it writes `+NSR02`. Demanding one revision would have refused media the
					// standard tool produces, which is the failure mode a conformance check has to
					// avoid in the other direction.
					if !matches!(&block[25..31], b"+NSR02" | b"+NSR03") {
						continue;
					}
					let number = le16(&block[22..24]);
					let entry = (number, vdsn, le32(&block[188..192]), le32(&block[192..196]));
					match partitions.iter_mut().find(|(seen, _, _, _)| *seen == number) {
						// Prevailing WITHIN this partition number, which is where the rule lives.
						Some(existing) if vdsn >= existing.1 => *existing = entry,
						Some(_) => {}
						None => {
							if partitions.try_reserve(1).is_err() {
								return Err(MountError::NoMemory);
							}
							partitions.push(entry);
						}
					}
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
		let map_partition = le16(&block[444..446]);
		// THE FILE SET IS NAMED BY A `long_ad`, AND ALL SIXTEEN BYTES OF IT MEAN SOMETHING.
		//
		// `LogicalVolumeContentsUse` at 248 is a full `long_ad` identifying the first extent of the
		// File Set Descriptor Sequence. This read the logical block and the partition reference out
		// of the middle of it and ignored the first four bytes - which carry the extent's LENGTH and
		// TYPE. So a volume naming an extent of zero bytes, or one recorded as unallocated, was
		// followed anyway, and whatever happened to be at that block was mounted if it looked like a
		// File Set Descriptor.
		//
		// That is the same defect that was fixed for the root and child ICB references, in the one
		// place still reading a `long_ad` by hand - and the parser those fixes produced is right
		// here.
		let Some(fileset) = LogicalAddress::parse_long_ad(&block, 248) else {
			return Err(MountError::Corrupt);
		};
		// Recorded and allocated, and at least one descriptor long. `extent_type` 0 is the only one
		// that names bytes that exist; a File Set Sequence is not a file, so "allocated but not
		// recorded" has no reading worth following.
		if fileset.extent_type != 0 || fileset.length < SECTOR_SIZE {
			return Err(MountError::Corrupt);
		}
		let fileset_lb = fileset.address.lb;
		// The File Set's partition reference, which was discarded. With one map it can only be
		// partition 0, and requiring that is what makes the discard safe.
		if fileset.address.partition != 0 {
			return Err(MountError::Unsupported);
		}
		// AND A SEQUENCE LONGER THAN ONE DESCRIPTOR IS REFUSED RATHER THAN ASSUMED AWAY.
		//
		// ECMA-167 defines a File Set Descriptor SEQUENCE: several descriptors, possibly continued
		// through a Next Extent, with the prevailing one chosen by File Set Number. UDF requires a
		// single File Set on ordinary media and permits several on WORM. This reader takes the first
		// descriptor of the extent and always did - which is right for the single-File-Set case and
		// silently wrong for the other, because it would mount whichever File Set happens to be
		// first rather than the one that prevails.
		//
		// Refusing is the honest answer for a shape this reader does not implement, and it is the
		// same answer it gives to a Metadata Partition Map. An extent long enough to hold a second
		// descriptor is not proof that one is there - but a volume whose writer sized it that way is
		// a volume this reader should not be guessing about.
		if fileset.length > SECTOR_SIZE {
			return Err(MountError::Unsupported);
		}
		// THE DESCRIPTOR FOR THE PARTITION THE MAP NAMES. The question used to be "does the one
		// descriptor we kept happen to be the one the map wants", which is a different question with
		// the same answer only on media that carry one partition.
		let Some(&(_, _, part_start, part_len)) = partitions.iter().find(|(number, _, _, _)| *number == map_partition) else {
			// The map names a partition this volume carries no descriptor for. `Unsupported` rather
			// than `Corrupt`: a volume may legitimately describe partitions this reader's one-map
			// rule cannot reach, and refusing is not the same as calling the medium broken.
			return Err(if partitions.is_empty() { MountError::Corrupt } else { MountError::Unsupported });
		};
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
		// THE FILE SET DESCRIPTOR SEQUENCE CAN CONTINUE, and `NextExtent` was never read.
		//
		// `finish_mount` took the first FSD's tag, CRC and location and its Root Directory ICB, and
		// looked no further - so a volume whose PREVAILING File Set lives in the continuation
		// mounted from the superseded one, silently, with every descriptor it read perfectly valid.
		// Refusing a File Set extent longer than one sector closed the other way the sequence
		// continues and left this one open.
		//
		// Multi-File-Set is out of scope, so the safe rule is stated rather than guessed: the
		// sequence is one descriptor, `NextExtent` is zero, and anything else is `Unsupported`
		// rather than `Corrupt` - the volume is well formed and this reader does not implement it,
		// which is a different thing to tell an operator.
		//
		// THE OFFSETS ARE ECMA-167'S AND WERE CHECKED AGAINST A REAL DISC, because the first
		// attempt at this used guessed ones and read `InterchangeLevel: 1, Max: 0` out of a volume
		// `mkfs.udf` had written as 3 and 3. A File Set Descriptor is:
		//
		//     16  RecordingDateandTime (12)      28  InterchangeLevel        30  MaximumInterchange
		//     32  CharacterSetList               40  FileSetNumber           44  FileSetDescNumber
		//     48  LogicalVolumeIdentifierCharacterSet (64)                  112  LogicalVolumeIdentifier (128)
		//    240  FileSetCharacterSet (64)      304  FileSetIdentifier (32) 400  RootDirectoryICB (16)
		//    416  DomainIdentifier (32)         448  NextExtent (16)        464  SystemStreamDirectoryICB
		//
		// `NextExtent` is a `long_ad`: length, then location.
		if le32(&block[448..452]) != 0 || le32(&block[452..456]) != 0 {
			return Err(MountError::Unsupported);
		}
		// AND THE SINGLE-SET SHAPE. `FileSetNumber` and `FileSetDescriptorNumber` are both zero for
		// the one and only set; a non-zero either way is a volume with more sets than this reader
		// will walk, and taking its first descriptor as "the" File Set is the same mistake as
		// ignoring `NextExtent`.
		if le32(&block[40..44]) != 0 || le32(&block[44..48]) != 0 {
			return Err(MountError::Unsupported);
		}
		// THE INTERCHANGE LEVELS, which say what shapes the structures may take. A volume may not
		// declare a level above the maximum it also declares, and this reader is written against
		// levels up to 3 - the highest ECMA-167 defines. A higher one is a volume using features
		// this parser has not been written for, and assuming otherwise is guessing in the direction
		// that reads garbage.
		let interchange = le16(&block[28..30]);
		let max_interchange = le16(&block[30..32]);
		if interchange == 0 || interchange > max_interchange || max_interchange > 3 {
			return Err(MountError::Unsupported);
		}
		// THE CHARACTER SET THE NAMES ARE IN, which this parser ASSUMED. Every identifier it decodes
		// is read as OSTA CS0, and the descriptor carries `LogicalVolumeIdentifierCharacterSet` and
		// `FileSetCharacterSet` precisely to say whether that is right. A field that exists to
		// confirm the interpretation, present and unread, is the wrong direction to guess in.
		//
		// A charspec is a one-byte type then 63 bytes of information; CS0 is type 0 with
		// "OSTA Compressed Unicode" in the information field.
		if block[48] != 0 || block[240] != 0 {
			return Err(MountError::Unsupported);
		}
		if &block[49..49 + 23] != b"OSTA Compressed Unicode" || &block[241..241 + 23] != b"OSTA Compressed Unicode" {
			return Err(MountError::Unsupported);
		}
		// THE DOMAIN IDENTIFIER. It names the specification the structures conform to, and the same
		// reasoning applies: this reader implements the OSTA Compliant domain, and reading a volume
		// that declares another one is reading structures under rules it does not know. The
		// Logical Volume Descriptor's copy is already checked; this is the File Set's own.
		if &block[417..417 + 19] != b"*OSTA UDF Compliant" {
			return Err(MountError::Unsupported);
		}
		// The root ICB is a `long_ad`, and its PARTITION was thrown away here - only the block
		// number was read. Kept now, so a File Set naming another partition is refused by the
		// resolver rather than read as partition 0.
		let Some(root_icb) = LogicalAddress::from_icb_long_ad(&block, 400) else {
			return Err(MountError::Corrupt);
		};
		if root_icb.lb >= part_len {
			return Err(MountError::Corrupt);
		}
		// AND IN THIS PARTITION. `root_icb.lb < part_len` bounds the block number and says nothing
		// about which partition the `long_ad` names, so `mount_checked` answered `Ok(Udf)` for a
		// volume whose root is unaddressable and the failure surfaced later, inside
		// `Geometry::physical`, as something that reads like corruption. This reader resolves
		// partition 0 and only partition 0; the reason is known here, so it is answered here.
		if root_icb.partition != 0 {
			return Err(MountError::Unsupported);
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
		// `.` AND `..` ARE REFUSED, which `fs-core` documents as `BadName` and this backend admitted.
		// The writable backends refuse them and so does `rt::RelativePath`, which every `vol://`
		// path is parsed by before a backend sees it - so this reader was admitting a spelling
		// nothing could deliver to it, and disagreeing with the shared contract about what a path
		// is. The parent records themselves are still parsed and required; they are structure, not
		// path syntax.
		//
		// EMPTY SEGMENTS STAY TOLERATED, for the reason recorded in `iso9660` and `libermemfs`:
		// this crate's own fixtures use the leading-slash form, the boundary already refuses it, and
		// the tolerance is unreachable rather than wrong.
		for seg in path.split(|&b| b == b'/') {
			if seg.is_empty() {
				continue;
			}
			if seg == b"." || seg == b".." {
				return Err(FsError::BadName);
			}
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
		let mut exact: Option<(LogicalAddress, bool)> = None;
		let mut folded: Option<(LogicalAddress, bool)> = None;
		let mut folded_ambiguous = false;
		// The rules one record cannot answer: one parent, first, and no repeated active name.
		let mut rules = DirRules::default();
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
			if !fid_lengths_ok(fid, l_iu, l_fi, total) {
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
			// A DELETED RECORD IS SKIPPED BEFORE ITS NAME IS READ, and the order is the whole point.
			//
			// UDF overwrites a deleted entry's compression id - 8 becomes 254, 16 becomes 255 - so
			// the identifier of a deleted FID deliberately decodes as nothing. `decode_name` knows
			// 8 and 16 and answers `None` to everything else, which this function turned into
			// `Corrupt` for the WHOLE directory. So one deleted file made every lookup in that
			// directory fail.
			//
			// `read_dir` over the same records tests the flag first and never decodes the name, so
			// the same volume LISTED correctly and could not be looked up in - two functions over
			// one record type disagreeing about whether it is readable. Deleted FIDs are a standard
			// mechanism and the specification recommends reusing them, so this is ordinary media
			// rather than a hostile case.
			if deleted {
				// A deleted record still occupies a slot, so it counts toward "the parent comes
				// first" - but its name is not part of the namespace and may repeat.
				rules.record(parent, None)?;
				off += (total + 3) & !3;
				continue;
			}
			// A name that does not decode is a CORRUPT record, not one to pass over: skipping it
			// listed a damaged directory as a tidy one.
			let Some(id) = decode_name(&fid[38 + l_iu..38 + l_iu + l_fi]) else {
				return Err(FsError::Corrupt);
			};
			rules.record(parent, if parent { None } else { Some(id.as_str()) })?;
			// RECORDED, NOT RETURNED. This answered the moment it found an exact match, so the
			// whole-directory rules below - one parent, first, and no repeated active name - only
			// ever ran when the lookup FAILED. A directory with two active records of the same name
			// therefore answered with the first and hid the second permanently, which is the parser
			// picking one of several inconsistent objects where it should refuse the directory.
			//
			// The cost is one more pass over a buffer that is already in memory: `read_icb_mapped`
			// has read the whole directory before this loop starts, so finishing the scan is
			// parsing and no additional I/O.
			if parent {
				if name == b".." {
					exact = Some((LogicalAddress::from_icb_long_ad(fid, 20).ok_or(FsError::Corrupt)?, is_dir));
				}
			} else if !id.is_empty() {
				// `deleted` is no longer tested here: such a record never reaches this point.
				if id.as_bytes() == name {
					exact = Some((LogicalAddress::from_icb_long_ad(fid, 20).ok_or(FsError::Corrupt)?, is_dir));
				}
				if eq_ci(&id, name) {
					if folded.is_some() {
						folded_ambiguous = true;
					}
					let child = LogicalAddress::from_icb_long_ad(fid, 20).ok_or(FsError::Corrupt)?;
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
		rules.finish()?;
		// An exact match wins over a case-folded one, and the folded ambiguity only matters when
		// there was no exact match to prefer.
		if let Some(found) = exact {
			return Ok(found);
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
		// The same whole-directory rules `find_entry` applies. Two walkers over one record type
		// disagreeing about whether a directory is readable is a defect this file has already had
		// once, so the rule lives in one place and both call it.
		let mut rules = DirRules::default();
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
			if !fid_lengths_ok(fid, l_iu, l_fi, total) {
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
			if parent || deleted {
				rules.record(parent, None)?;
			}
			if !parent && !deleted {
				let is_dir = chars & 0x02 != 0;
				let Some(id) = decode_name(&fid[38 + l_iu..38 + l_iu + l_fi]) else {
					return Err(FsError::Corrupt);
				};
				rules.record(false, Some(id.as_str()))?;
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
					match self.icb_size(LogicalAddress::from_icb_long_ad(fid, 20).ok_or(FsError::Corrupt)?, is_dir) {
						Ok(size) => size,
						Err(error) => return Err(error),
					}
				};
				if !id.is_empty() {
					// FALLIBLY: the entry count is the medium's, so this vector's growth is a
					// number the disc chose - and an infallible growth that fails aborts
					// StorageService and every volume it serves rather than refusing this one.
					out.try_reserve(1).map_err(|_| FsError::NoMemory)?;
					out.push(FileInfo { name: id, size, is_dir });
				}
			}
			off += (total + 3) & !3;
		}
		if off != data.len() {
			return Err(FsError::Corrupt);
		}
		rules.finish()?;
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
				let entry_is_dir = file_type_is_dir(block[27])?;
				if entry_is_dir != expect_dir {
					return Err(FsError::Corrupt);
				}
				// BOUNDED BY WHAT THE PARTITION COULD HOLD. The information length comes off the
				// medium and nothing looked at it: a forged 2^63 was reported to a caller as a file
				// size, and a listing that says a file is eight exabytes is a listing that lies
				// about the disc. The partition's own length is the ceiling that cannot be exceeded
				// by any file inside it.
				let size = le64(&block[56..64]);
				let ceiling = (self.geo.part_len as u64).saturating_mul(SECTOR_SIZE as u64);
				if size > ceiling {
					return Err(FsError::Corrupt);
				}
				Ok(size)
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
		// MATCHED EXACTLY, whether or not the caller stated an expectation: a FIFO served as a file
		// is the failure this is about, and it is a failure with no expectation attached.
		let is_dir = file_type_is_dir(block[27])?;
		if let Some(want_dir) = expect_dir
			&& is_dir != want_dir
		{
			return Err(FsError::Corrupt);
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
		// AND `alloc_end` HAS TO MEAN SOMETHING, before the allocation form is branched on.
		//
		// The clamp below is what lets an honest descriptor pass, and for the external forms it costs
		// nothing because `ad_end > block.len()` is refused twenty lines later. The EMBEDDED branch
		// returns before that, and its own bounds - `end > block.len() || info_len > l_ad` - are both
		// satisfied trivially by a LARGE `l_ad`. So an embedded File Entry declaring `l_ad = 100000`
		// and `info_len = 5` inside a 2048-byte block was accepted, and five bytes returned for a
		// descriptor claiming its allocation area runs fifty blocks past itself.
		//
		// Nothing was read out of bounds and nothing was misattributed. What failed is that `l_ad`
		// was not required to mean anything - and it is one of the two numbers the coverage
		// requirement above is computed from, so a number that means nothing is a requirement that
		// means nothing.
		if alloc_end > block.len() {
			return Err(FsError::Corrupt);
		}
		if 16 + (le16(&block[10..12]) as usize) < alloc_end {
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
		// THE ALIGNMENT THE FORMAT REQUIRES, CHECKED BECAUSE IT IS THE RULE.
		//
		// ECMA-167 requires the extended attributes to be a multiple of four bytes and the
		// allocation descriptors to be a whole number of descriptors - eight bytes for a short_ad,
		// sixteen for a long_ad. Nothing asked. Requiring the extents to account for exactly
		// `InformationLength` rejects most misaligned descriptors as a side effect, and a format
		// rule that holds only because a later arithmetic step happens to notice is a rule that
		// stops holding when the arithmetic changes.
		//
		// Checked HERE rather than beside the parse, so a descriptor that is both misaligned and
		// impossible in some larger way still reports the larger thing: an `InformationLength` of
		// `u64::MAX` is `TooLarge` whatever its descriptor area is a multiple of.
		if l_ea % 4 != 0 {
			return Err(FsError::Corrupt);
		}
		match alloc {
			0 if l_ad % 8 != 0 => return Err(FsError::Corrupt),
			1 if l_ad % 16 != 0 => return Err(FsError::Corrupt),
			_ => {}
		}
		// embedded: the file's bytes sit inline in the File Entry.
		if alloc == 3 {
			// The embedded data has to fit in the descriptor area it is declared in, and in the
			// block. A length past either is corruption, not a length to shorten.
			let Some(end) = ad_off.checked_add(info_len) else {
				return Err(FsError::Invalid);
			};
			// EXACTLY, not "at most". For embedded data the allocation-descriptor area IS the data,
			// so `InformationLength` and `LengthOfAllocationDescriptors` describe ONE object and a
			// disagreement between them is a File Entry that contradicts itself.
			//
			// This was `info_len > l_ad`, which accepted `InformationLength = 5` with
			// `LengthOfAllocationDescriptors = 64` and returned the first five bytes of a
			// sixty-four-byte object as the whole file. The test beside it asserted that case passes
			// and called it "the honest descriptor", so the semantics were locked in rather than
			// checked - which is the thing this milestone exists to catch.
			if end > block.len() || info_len != l_ad {
				return Err(FsError::Corrupt);
			}
			// The embedded bytes live in the File Entry's own block, so every offset in them maps
			// to `at.lb` - one run, and the data is shorter than a block by construction.
			// `to_vec` is infallible; the length comes from the File Entry. Bounded by a block by
			// construction, which is why this is small - and copied fallibly anyway, because
			// "small today" is how the next ceiling change becomes an abort.
			let mut data: Vec<u8> = Vec::new();
			data.try_reserve_exact(end - ad_off).map_err(|_| FsError::NoMemory)?;
			data.extend_from_slice(&block[ad_off..end]);
			let mut runs: Vec<(usize, u32)> = Vec::new();
			runs.try_reserve_exact(1).map_err(|_| FsError::NoMemory)?;
			runs.push((0usize, at.lb));
			return Ok(Flattened { data, runs });
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
		// THE WHOLE DECLARED DESCRIPTOR AREA, and the extents have to account for exactly
		// `InformationLength`.
		//
		// The loop stopped at `done == info_len` and took `len.min(info_len - done)`, so an extent
		// declaring 2048 bytes against an `InformationLength` of 5 had 2043 bytes silently ignored,
		// and every descriptor after the point where the count was reached was never parsed at all.
		// `min()` was papering over a disagreement between two pieces of metadata that describe one
		// file - the same class as the silent truncation this milestone already closed, one layer
		// further down.
		//
		// So: an extent that would take the total past the declared length is corruption, the scan
		// runs to the end of `l_ad`, and anything after the terminator has to be zero-length
		// padding. The `done != info_len` check at the bottom then means what it says.
		let mut terminated = false;
		while ad + step <= ad_end {
			let raw = le32(&block[ad..ad + 4]);
			let len = (raw & 0x3fff_ffff) as usize;
			let ext_type = raw >> 30;
			// A `long_ad` carries its partition; a `short_ad` has none and is partition 0 by
			// definition. Reading the block number alone treated the two the same, so a `long_ad`
			// naming another partition was read here as this one.
			let lba = le32(&block[ad + 4..ad + 8]);
			// PARSED, not judged: the four-way decision is the three tests below, and it is the same
			// decision for both allocation forms. Going through the ICB reference's rule here made
			// the terminator and the two unrecorded types unreachable for `long_ad` alone.
			let extent = if step == 16 { LogicalAddress::parse_long_ad(&block, ad).ok_or(FsError::Corrupt)?.address } else { LogicalAddress { lb: lba, partition: 0 } };
			// a zero-length extent terminates the sequence; a type-3 entry chains to
			// further descriptors - not followed, refused rather than read as data.
			if terminated {
				// Past the terminator the descriptor area is padding. A non-zero length here is an
				// extent the file's declared length does not account for - a descriptor left
				// uninterpreted, which is exactly what "the extents cover the file" must exclude.
				if len != 0 {
					return Err(FsError::Corrupt);
				}
				ad += step;
				continue;
			}
			if len == 0 {
				terminated = true;
				ad += step;
				continue;
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
			// REFUSED, not clamped: an extent that would take the total past the declared length is
			// two pieces of metadata disagreeing about one file.
			if len > info_len - done {
				return Err(FsError::Corrupt);
			}
			let take = len;
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

// A parsed `long_ad`: where it points, how long it is, and which of the four extent types it is.
//
// PARSING IS NOT POLICY, and conflating the two broke a caller. `from_long_ad` returned
// `Option<LogicalAddress>` and encoded the ICB reference's rule in the `None` - refusing every extent
// type but 0 and every zero length - while the file-data allocation loop uses the same helper and has
// its own four-way decision two lines further down: a zero length TERMINATES the list, type 3 is
// refused, and types 1 and 2 read as zeros. Both of those paths went dead for `long_ad` and stayed
// live for `short_ad`, which does not go through the helper: one rule implemented twice and
// disagreeing, which is this file's recurring shape, arrived at by fixing one of its instances.
//
// A valid file with a sparse `long_ad` extent - the ordinary shape of a sparse file on a `long_ad`
// volume - became `FsError::Corrupt`. Not a safety defect; an interoperability one, and one this
// milestone introduced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct LongAd {
	address: LogicalAddress,
	length: usize,
	// 0 recorded and allocated, 1 allocated but not recorded, 2 neither, 3 the next extent of
	// allocation descriptors.
	extent_type: u32,
}

impl LogicalAddress {
	// The `long_ad` at `at`, PARSED: length(4), block(4), partition(2), implementation use(6).
	// `None` only when the bytes are not there - every caller states its own rule about what the
	// parts mean.
	fn parse_long_ad(bytes: &[u8], at: usize) -> Option<LongAd> {
		let block = bytes.get(at..at + 16)?;
		let raw = le32(&block[0..4]);
		Some(LongAd { address: LogicalAddress { lb: le32(&block[4..8]), partition: le16(&block[8..10]) }, length: (raw & 0x3fff_ffff) as usize, extent_type: raw >> 30 })
	}

	// The `long_ad` at `at` AS AN ICB REFERENCE, which is a stricter thing than a `long_ad`.
	//
	// The length's top two bits are the extent TYPE, and only the address used to be read - so an ICB
	// reference recorded as UNALLOCATED, or of zero length, was followed as though it named data. An
	// ICB reference has to be a recorded, allocated extent with something in it; there is no reading
	// of "the root directory is here and it is not recorded" that is worth following.
	//
	// SERVES ONE CALLER SHAPE. The file-data allocation loop wants `parse_long_ad` above, because
	// its rules for those cases are different and legitimate.
	// AN ICB REFERENCE IS AN ADDRESS AND A LENGTH, and this threw the length away.
	//
	// The extent was checked for being recorded and non-zero and then only the address was
	// returned - so every caller passed a bare address into `read_icb_mapped`, which reads and
	// validates fields across a whole 2 kB block and may parse an allocation-descriptor region, with
	// nothing proving those bytes are inside the extent the referring `long_ad` declared. A
	// reference claiming 40 bytes had a 2048-byte descriptor read out of it.
	//
	// One block is what this reader can act on: an ICB spanning more is a shape it does not
	// implement, and an extent shorter than a File Entry's fixed header describes something that is
	// not one.
	fn from_icb_long_ad(bytes: &[u8], at: usize) -> Option<LogicalAddress> {
		let parsed = Self::parse_long_ad(bytes, at)?;
		if parsed.extent_type != 0 || parsed.length == 0 {
			return None;
		}
		// 176 is the File Entry header up to its allocation descriptors; anything shorter cannot be
		// one whatever else is true of it.
		if (parsed.length as usize) < 176 || parsed.length as usize > SECTOR_SIZE {
			return None;
		}
		Some(parsed.address)
	}
}

// Whether an ICB file type is a directory - and whether it is one of the two this volume API has
// semantics for at all.
//
// Type 4 is a directory and type 5 an ordinary byte file. Everything else - 6 block device, 7
// character device, 8 extended-attribute file, 9 FIFO, 10 socket, 11 terminal entry, 12 symbolic
// link, 13 stream directory - was read as `!= 4`, therefore "not a directory", therefore served as
// an ordinary file. Only the symlink was refused, which is the shape of the right answer applied to
// the one type somebody happened to think of.
//
// `Invalid` and not `Corrupt`: a FIFO File Entry is a shape the medium is entitled to carry, and this
// reader has no semantics for it. Blaming the medium for that would be blaming it for being more
// than this reader implements.
fn file_type_is_dir(file_type: u8) -> Result<bool, FsError> {
	match file_type {
		4 => Ok(true),
		5 => Ok(false),
		_ => Err(FsError::Invalid),
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
		// The root ICB `long_ad` at 400..416, the Domain Identifier at 416..448 and `NextExtent` at
		// 448..464 - all of which `finish_mount` now makes decisions from, so all of which the
		// recorded CRC has to vouch for. It stopped at 416, which left the domain and the
		// continuation pointer outside the range the CRC was computed over: exactly the shape of
		// the finding this milestone opened with, at the descriptor that decides where the root is.
		// `mkfs.udf` records 496, which clears this comfortably.
		TAG_FILE_SET => 464,
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

// THE RULES A DIRECTORY AS A WHOLE HAS TO SATISFY, tracked across its records.
//
// `fid_lengths_ok` judges one record at a time and cannot see any of these: exactly one parent
// entry, that entry first, and no two ACTIVE records sharing a name. The last has a visible
// consequence - `find_entry` returns the first exact match, so a directory with two active FIDs of
// the same name hides the second permanently. Case-fold ambiguity was already refused and exact
// duplication was not, which is the parser picking one of several inconsistent objects where it
// should refuse the directory.
#[derive(Default)]
struct DirRules {
	seen: usize,
	parents: usize,
	names: Vec<String>,
}

impl DirRules {
	// One record, in the order the walk meets them. `None` for a deleted record's name, which is
	// not part of the namespace and may legitimately repeat.
	fn record(&mut self, parent: bool, name: Option<&str>) -> Result<(), FsError> {
		if parent {
			self.parents += 1;
			// FIRST, and exactly one. A parent record anywhere else is a directory whose shape no
			// writer produces, and two of them is a directory with two parents.
			if self.parents > 1 || self.seen != 0 {
				return Err(FsError::Corrupt);
			}
		}
		if let Some(name) = name {
			// SORTED, so this is a binary search rather than a walk of everything seen so far.
			//
			// It was `names.iter().any(..)` per record, which is quadratic in the directory's entry
			// count - and the count comes off the medium. A large directory on a hostile or merely
			// damaged disc therefore turned a listing into a long single-threaded scan inside
			// StorageService, which serves every other volume from the same thread.
			//
			// A sorted `Vec` rather than a set: this crate is `no_std` with `alloc` and has no
			// hasher, and the insertion point falls out of the lookup that already happened.
			let at = match self.names.binary_search_by(|held| held.as_str().cmp(name)) {
				Ok(_) => return Err(FsError::Corrupt),
				Err(at) => at,
			};
			self.names.try_reserve(1).map_err(|_| FsError::NoMemory)?;
			// The name is copied fallibly too: it is a string off the medium, and `String::from`
			// would abort the service rather than refuse the disc.
			let mut owned = String::new();
			owned.try_reserve_exact(name.len()).map_err(|_| FsError::NoMemory)?;
			owned.push_str(name);
			self.names.insert(at, owned);
		}
		self.seen += 1;
		Ok(())
	}

	// The root directory is the exception: it is its own parent and carries a parent record like
	// any other, so this is about SHAPE and not about which directory it is. A directory with no
	// parent record at all is one the walk cannot climb out of.
	fn finish(&self) -> Result<(), FsError> {
		if self.parents != 1 { Err(FsError::Corrupt) } else { Ok(()) }
	}
}

// THE FIXED LENGTHS UDF PUTS ON A FILE IDENTIFIER DESCRIPTOR, which this reader checked none of.
//
// Everything else about a FID was validated - the tag, the body CRC, the dynamic CRC coverage, the
// tag location, and the two lengths against the directory's own buffer - so a record could be
// internally consistent, pass every one of those, and still be a shape no UDF writer produces.
// That is a gap in a parser whose whole premise is that the medium is untrusted: a forgery only has
// to be self-consistent, and these are the constraints that make self-consistency harder to reach.
//
// Each one is from UDF 2.60 and each rules out something a forger would otherwise have free:
//   - a descriptor larger than one logical block, which no writer emits and which lets a single
//     record span the whole directory;
//   - `FileVersionNumber` other than 1, which the specification fixes and nothing reads, so any
//     other value is a field carrying something else;
//   - an implementation-use area that is neither absent nor large enough to hold the header it is
//     defined to start with;
//   - a name of one byte, which is a compression id and no characters - not a name, and a shape
//     that would otherwise decode as the empty string and be skipped silently.
//
// The parent record is the deliberate exception: it carries no name at all, which is why the length
// rule is written against `l_fi == 0` for it rather than being relaxed for everybody.
fn fid_lengths_ok(fid: &[u8], l_iu: usize, l_fi: usize, total: usize) -> bool {
	if total > SECTOR_SIZE {
		return false;
	}
	if le16(&fid[16..18]) != 1 {
		return false;
	}
	// A MULTIPLE OF FOUR, which the format requires and nothing asked. The implementation-use area
	// is followed by the file identifier and then by padding to a four-byte boundary, so a length
	// that is not itself aligned puts every later field at an offset no writer produces.
	if l_iu != 0 && (l_iu < 32 || l_iu % 4 != 0) {
		return false;
	}
	// THE RESERVED FILE CHARACTERISTICS BITS. UDF 2.60 defines bits 0-4 (hidden, directory, deleted,
	// parent, metadata) and reserves the rest. A record setting one of them is carrying something
	// this reader does not understand in a field it makes decisions from.
	if fid[18] & 0xe0 != 0 {
		return false;
	}
	let parent = fid[18] & 0x08 != 0;
	// `l_fi > 1` FOR A NON-PARENT, not `l_fi != 1`.
	//
	// The rule was written to reject the degenerate one-byte "compression id and no characters"
	// case and let `l_fi == 0` straight through - which decodes to `Some("")` and is then quietly
	// skipped by both directory walkers. So a directory holding a nameless non-parent record listed
	// as a healthy directory that was simply missing a file, which is the silent shortening this
	// reader refuses everywhere else, reached through the one arm of the length rule.
	if parent { l_fi == 0 } else { l_fi > 1 }
}

fn decode_name(id: &[u8]) -> Option<String> {
	if id.is_empty() {
		return Some(String::new());
	}
	let mut s = String::new();
	// Reserved up front, fallibly: a name is bounded by its record, but every push below was
	// infallible and the record's length is the medium's number. Worst case is three UTF-8 bytes
	// per UCS-2 unit, which is what the OSTA compression byte selects between.
	if s.try_reserve(id.len() * 3).is_err() {
		return None;
	}
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
