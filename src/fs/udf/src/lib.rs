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

// The partition's start LBA plus the root directory ICB block (partition-relative);
// every read derives from these, so mounting is just locating one File Set Descriptor.
struct Geometry {
	part_start: u32,
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
		if !dev.read_block(AVDP_LBA, &mut block) || le16(&block[0..2]) != TAG_AVDP {
			return None;
		}
		let vds_len = le32(&block[16..20]);
		let vds_loc = le32(&block[20..24]);
		let mut part_start: Option<u32> = None;
		let mut fileset_lb: Option<u32> = None;
		let count = (vds_len as usize / SECTOR_SIZE).max(1);
		for i in 0..count as u64 {
			if !dev.read_block(vds_loc as u64 + i, &mut block) {
				return None;
			}
			match le16(&block[0..2]) {
				TAG_PARTITION => part_start = Some(le32(&block[188..192])),
				TAG_LOGICAL_VOLUME => fileset_lb = Some(le32(&block[252..256])),
				TAG_TERMINATING => break,
				_ => {}
			}
		}
		let part_start = part_start?;
		let fileset_lb = fileset_lb?;
		if !dev.read_block(part_start as u64 + fileset_lb as u64, &mut block) || le16(&block[0..2]) != TAG_FILE_SET {
			return None;
		}
		let root_icb = le32(&block[404..408]);
		Some(Udf { dev, geo: Geometry { part_start, root_icb } })
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
	// returning its ICB block and whether it is a directory.
	fn find_entry(&mut self, dir_icb: u32, name: &[u8]) -> Result<(u32, bool), FsError> {
		let data = self.read_icb(dir_icb)?;
		let mut off = 0usize;
		while off + 38 <= data.len() {
			let fid = &data[off..];
			if le16(&fid[0..2]) != TAG_FILE_ID {
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
			let id = &fid[38 + l_iu..38 + l_iu + l_fi];
			if !parent && !deleted && eq_ci(&decode_name(id), name) {
				return Ok((le32(&fid[24..28]), is_dir));
			}
			off += (total + 3) & !3;
		}
		Err(FsError::NotFound)
	}

	// Read every File Identifier in a directory into FileInfos, skipping the parent entry.
	fn read_dir(&mut self, dir_icb: u32) -> Result<Vec<FileInfo>, FsError> {
		let data = self.read_icb(dir_icb)?;
		let mut out = Vec::new();
		let mut off = 0usize;
		while off + 38 <= data.len() {
			let fid = &data[off..];
			if le16(&fid[0..2]) != TAG_FILE_ID {
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
				let size = if is_dir { 0 } else { self.read_icb(le32(&fid[24..28])).map(|d| d.len()).unwrap_or(0) as u64 };
				out.push(FileInfo { name: id, size, is_dir });
			}
			off += (total + 3) & !3;
		}
		Ok(out)
	}

	// Read a File Entry's data: inline (embedded) bytes, or one short / long allocation
	// extent followed to its end. The information length caps the read.
	fn read_icb(&mut self, lb: u32) -> Result<Vec<u8>, FsError> {
		let mut block = [0u8; SECTOR_SIZE];
		if !self.dev.read_block(self.geo.part_start as u64 + lb as u64, &mut block) {
			return Err(FsError::Io);
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
		let ad_off = header + l_ea;
		if ad_off > block.len() {
			return Err(FsError::Invalid);
		}
		// embedded: the file's bytes sit inline in the File Entry.
		if alloc == 3 {
			let end = (ad_off + info_len).min(block.len());
			return Ok(block[ad_off..end].to_vec());
		}
		// short_ad (8 bytes) or long_ad (16 bytes) extents, read to the info length.
		let step = if alloc == 1 { 16 } else { 8 };
		let mut out = vec![0u8; info_len];
		let mut done = 0usize;
		let mut ad = ad_off;
		while done < info_len && ad + step <= ad_off + l_ad {
			let len = (le32(&block[ad..ad + 4]) & 0x3fff_ffff) as usize;
			let lba = le32(&block[ad + 4..ad + 8]);
			let mut cur = self.geo.part_start as u64 + lba as u64;
			let mut left = len.min(info_len - done);
			while left > 0 {
				if !self.dev.read_block(cur, &mut block) {
					return Err(FsError::Io);
				}
				let n = left.min(SECTOR_SIZE);
				out[done..done + n].copy_from_slice(&block[..n]);
				done += n;
				left -= n;
				cur += 1;
			}
			ad += step;
		}
		Ok(out)
	}
}

// Decode a UDF d-string file identifier: the first byte is the compression id (8 =
// 8-bit Latin-1, 16 = 16-bit UCS-2 big-endian); the rest are the characters.
fn decode_name(id: &[u8]) -> String {
	if id.is_empty() {
		return String::new();
	}
	let mut s = String::new();
	if id[0] == 16 {
		for c in id[1..].chunks_exact(2) {
			s.push(char::from_u32(u16::from_be_bytes([c[0], c[1]]) as u32).unwrap_or('?'));
		}
	} else {
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

// Case-insensitive ASCII name compare (queries may differ in case from the stored name).
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
