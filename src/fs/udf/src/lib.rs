//! UDF - a read-only backend for DVD / Blu-ray and large optical (`.udf`) media,
//! behind the same [`BlockDevice`] trait FAT, ISO9660 and LiberFS use. It sits behind
//! `Storage.Volume` as just another FS backend: per the layering principle several
//! filesystems mount behind one volume API, and UDF is the format DVDs and Blu-ray
//! discs use, so M58 (ISO9660) covers CDs and this completes optical-media interop.
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
// absolute LBA. Implementors map that onto their backing (disc sectors, a Vec). The
// backend never writes, so there is no write_block.
pub trait BlockDevice {
	// Read block `lba` into `buf` (exactly SECTOR_SIZE bytes). False on I/O failure.
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool;
}

// A UDF error. The variants map onto the `Storage.Volume` `error` enum at the service
// boundary (NotFound -> not-found, the rest -> invalid).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
	NotFound,
	Invalid,
	TooLong,
	Io,
}

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

impl<D: BlockDevice> Udf<D> {
	// The block device this filesystem reads through.
	pub fn device(&self) -> &D {
		&self.dev
	}
	// Mount UDF media: read the Anchor at LBA 256, scan the Main Volume Descriptor
	// Sequence for the partition start and File Set, then the root directory ICB. None if
	// the layout cannot be followed.
	pub fn mount(mut dev: D) -> Option<Udf<D>> {
		let mut block = [0u8; SECTOR_SIZE];
		if !dev.read_block(AVDP_LBA, &mut block) || le16(&block[0..2]) != TAG_AVDP || !tag_ok(&block) || le32(&block[12..16]) != AVDP_LBA as u32 {
			return None;
		}
		let vds_len = le32(&block[16..20]);
		let vds_loc = le32(&block[20..24]);
		let mut part: Option<(u32, u32)> = None;
		let mut fileset_lb: Option<u32> = None;
		// the sequence length is the medium's claim - a real MVDS is a handful of
		// descriptors, so the scan is clamped rather than driven megablocks far.
		let count = (vds_len as usize / SECTOR_SIZE).clamp(1, 64);
		for i in 0..count as u64 {
			if !dev.read_block(vds_loc as u64 + i, &mut block) {
				return None;
			}
			// a descriptor must checksum AND record its own address - a stale or copied
			// block is skipped, never trusted.
			if !tag_ok(&block) || le32(&block[12..16]) as u64 != vds_loc as u64 + i {
				continue;
			}
			match le16(&block[0..2]) {
				TAG_PARTITION => part = Some((le32(&block[188..192]), le32(&block[192..196]))),
				TAG_LOGICAL_VOLUME => fileset_lb = Some(le32(&block[252..256])),
				TAG_TERMINATING => break,
				_ => {}
			}
		}
		let (part_start, part_len) = part?;
		let fileset_lb = fileset_lb?;
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
		if !dev.read_block(part_start as u64 + fileset_lb as u64, &mut block) || le16(&block[0..2]) != TAG_FILE_SET || !tag_ok(&block) || le32(&block[12..16]) != fileset_lb {
			return None;
		}
		let root_icb = le32(&block[404..408]);
		if root_icb >= part_len {
			return None;
		}
		Some(Udf { dev, geo: Geometry { part_start, part_len, root_icb } })
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
		while off + 38 <= data.len() {
			let fid = &data[off..];
			if le16(&fid[0..2]) != TAG_FILE_ID || !tag_ok(fid) {
				break;
			}
			let l_iu = le16(&fid[36..38]) as usize;
			let l_fi = fid[19] as usize;
			let total = 38 + l_iu + l_fi;
			if off + total > data.len() {
				break;
			}
			let chars = fid[18];
			let parent = chars & 0x08 != 0;
			let deleted = chars & 0x04 != 0;
			let is_dir = chars & 0x02 != 0;
			let id = decode_name(&fid[38 + l_iu..38 + l_iu + l_fi]);
			let hit = if parent { name == b".." } else { !deleted && !id.is_empty() && eq_ci(&id, name) };
			if hit {
				return Ok((le32(&fid[24..28]), is_dir));
			}
			off += (total + 3) & !3;
		}
		Err(FsError::NotFound)
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
			if le16(&fid[0..2]) != TAG_FILE_ID || !tag_ok(fid) {
				break;
			}
			let l_iu = le16(&fid[36..38]) as usize;
			let l_fi = fid[19] as usize;
			let total = 38 + l_iu + l_fi;
			if off + total > data.len() {
				break;
			}
			let chars = fid[18];
			if chars & 0x08 == 0 && chars & 0x04 == 0 {
				let is_dir = chars & 0x02 != 0;
				let id = decode_name(&fid[38 + l_iu..38 + l_iu + l_fi]);
				// an unreadable child header lists as size 0 by decision - the listing
				// stays best-effort, the file's own read reports the error honestly.
				let size = if is_dir { 0 } else { self.icb_size(le32(&fid[24..28])).unwrap_or(0) };
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
	fn icb_size(&mut self, lb: u32) -> Result<u64, FsError> {
		if lb >= self.geo.part_len {
			return Err(FsError::Invalid);
		}
		let mut block = [0u8; SECTOR_SIZE];
		if !self.dev.read_block(self.geo.part_start as u64 + lb as u64, &mut block) {
			return Err(FsError::Io);
		}
		if !tag_ok(&block) || le32(&block[12..16]) != lb {
			return Err(FsError::Invalid);
		}
		match le16(&block[0..2]) {
			TAG_FILE_ENTRY | TAG_EXT_FILE_ENTRY => Ok(le64(&block[56..64])),
			_ => Err(FsError::Invalid),
		}
	}

	// Read a File Entry's data: inline (embedded) bytes, or short / long allocation
	// extents followed to the information length. Every value comes off the medium, so
	// the ICB block, the information length, the descriptor region, and every extent
	// are bounded by the partition before a buffer is allocated or a block read; an
	// unrecorded (sparse) extent reads as zeros, never as stale disk content.
	fn read_icb(&mut self, lb: u32) -> Result<Vec<u8>, FsError> {
		if lb >= self.geo.part_len {
			return Err(FsError::Invalid);
		}
		let mut block = [0u8; SECTOR_SIZE];
		if !self.dev.read_block(self.geo.part_start as u64 + lb as u64, &mut block) {
			return Err(FsError::Io);
		}
		// the tag checksum gates garbage; the tag location gates a descriptor copied to
		// the wrong block (its recorded address must be its own).
		if !tag_ok(&block) || le32(&block[12..16]) != lb {
			return Err(FsError::Invalid);
		}
		let tag = le16(&block[0..2]);
		let (header, l_ea_off, l_ad_off) = match tag {
			TAG_FILE_ENTRY => (176usize, 168usize, 172usize),
			TAG_EXT_FILE_ENTRY => (216usize, 208usize, 212usize),
			_ => return Err(FsError::Invalid),
		};
		let info_len = le64(&block[56..64]) as usize;
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
		// embedded: the file's bytes sit inline in the File Entry.
		if alloc == 3 {
			let end = (ad_off + info_len).min(block.len());
			return Ok(block[ad_off..end].to_vec());
		}
		// only short_ad, long_ad, and embedded forms exist on real media - extended_ad
		// (20-byte records) and the reserved values are refused rather than misparsed
		// with the wrong step.
		if alloc != 0 && alloc != 1 {
			return Err(FsError::Invalid);
		}
		// the information length is the medium's claim - it cannot exceed what the
		// partition could hold, so a forged length never allocates without bound.
		if info_len as u64 > self.geo.part_len as u64 * SECTOR_SIZE as u64 {
			return Err(FsError::Invalid);
		}
		// short_ad (8 bytes) or long_ad (16 bytes) extents, read to the info length; the
		// descriptor region is clamped to the File Entry block it lives in.
		let step = if alloc == 1 { 16 } else { 8 };
		let ad_end = (ad_off + l_ad).min(block.len());
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
			let mut cur = self.geo.part_start as u64 + lba as u64;
			let mut left = take;
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

// Decode a UDF d-string file identifier: the first byte is the compression id (8 =
// 8-bit Latin-1, 16 = 16-bit UCS-2 big-endian); the rest are the characters. An unknown
// id yields an empty name (the record is then skipped), never noise decoded as text.
fn decode_name(id: &[u8]) -> String {
	if id.is_empty() {
		return String::new();
	}
	let mut s = String::new();
	if id[0] == 16 {
		for c in id[1..].chunks_exact(2) {
			s.push(char::from_u32(u16::from_be_bytes([c[0], c[1]]) as u32).unwrap_or('?'));
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
