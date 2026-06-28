//! FAT - a read-only FAT12 / FAT16 / FAT32 and exFAT backend for foreign removable
//! media (USB sticks, SD cards, install images), behind the same [`BlockDevice`] trait
//! LiberFS uses. It sits behind `Storage.Volume` as just another FS backend: per the
//! layering principle, several filesystems mount behind one volume API, and FAT is the
//! ubiquitous interchange format so reading it makes those media readable.
//!
//! Read-first by design. The boot sector is parsed and the family auto-detected: a small
//! cluster count is FAT12, a medium one FAT16, a large one FAT32, and an `EXFAT ` magic
//! is exFAT. A file is found by walking `/`-separated path segments from the root, each
//! lookup scanning a directory's 32-byte entries (assembling VFAT long file names from
//! their UTF-16 fragments, or the exFAT file-name entry set) and following the cluster
//! chain through the allocation table. Nothing is written; foreign media is only read.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

#[cfg(test)]
mod tests;

// One disk sector. FAT volumes set a logical sector size in the boot sector (almost
// always 512); the device reads physical 512-byte sectors and a larger logical sector is
// read as a run of them.
pub const SECTOR_SIZE: usize = 512;

// A read-only block device: foreign media is read one 512-byte sector at a time, by
// absolute LBA. Implementors map that onto their backing (disk sectors, a Vec). FAT is
// mounted read-first, so there is no write half.
pub trait BlockDevice {
	// Read sector `lba` into `buf` (exactly SECTOR_SIZE bytes). False on I/O failure.
	fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> bool;
}

// A FAT error. The variants map onto the `Storage.Volume` `error` enum at the service
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

// Which family the boot sector turned out to be. The three classic widths differ only in
// FAT-entry size and where the root directory lives; exFAT is a different layout.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
	Fat12,
	Fat16,
	Fat32,
	ExFat,
}

// The geometry read from the boot sector, in bytes/sectors/clusters, plus the family.
// Every read derives from these, so mounting is just parsing this once.
struct Geometry {
	kind: Kind,
	bytes_per_sector: u32,
	sectors_per_cluster: u32,
	reserved_sectors: u32,
	num_fats: u32,
	fat_size: u32,
	root_entries: u32,
	root_cluster: u32,
	first_data_sector: u32,
}

// A mounted FAT volume: the device plus its geometry. Reads are on demand, so nothing is
// cached beyond the geometry; a directory or file is read by following clusters as asked.
pub struct FatFs<D: BlockDevice> {
	dev: D,
	geo: Geometry,
}

impl<D: BlockDevice> FatFs<D> {
	// Mount foreign media: read the boot sector, detect the family, and compute the
	// geometry. None if the sector is unreadable or not a recognizable FAT volume.
	pub fn mount(mut dev: D) -> Option<FatFs<D>> {
		let mut boot = [0u8; SECTOR_SIZE];
		if !dev.read_sector(0, &mut boot) {
			return None;
		}
		let geo = if &boot[3..11] == b"EXFAT   " { Geometry::exfat(&boot)? } else { Geometry::bpb(&boot)? };
		Some(FatFs { dev, geo })
	}

	// List the volume's root directory.
	pub fn list(&mut self) -> Result<Vec<FileInfo>, FsError> {
		self.read_dir(self.root_cluster())
	}

	// List a subdirectory named by a `/`-separated path. An empty path is the root.
	pub fn list_dir(&mut self, path: &[u8]) -> Result<Vec<FileInfo>, FsError> {
		let cluster = self.resolve_dir(path)?;
		self.read_dir(cluster)
	}

	// Read a whole file named by a `/`-separated path into a Vec.
	pub fn read_file(&mut self, path: &[u8]) -> Result<Vec<u8>, FsError> {
		let (parent, name) = split_parent(path)?;
		let dir = self.resolve_dir(parent)?;
		let entry = self.find_entry(dir, name)?;
		if entry.is_dir {
			return Err(FsError::NotFound);
		}
		self.read_chain(entry.first_cluster, entry.size as usize)
	}

	// The cluster the root directory starts at. FAT32 and exFAT keep the root in the
	// cluster heap; FAT12/16 keep it in a fixed region, modelled here as cluster 0.
	fn root_cluster(&self) -> u32 {
		match self.geo.kind {
			Kind::Fat12 | Kind::Fat16 => 0,
			Kind::Fat32 | Kind::ExFat => self.geo.root_cluster,
		}
	}

	// Walk path segments from the root, descending into each named subdirectory, and
	// return the cluster the final directory starts at. An empty path is the root.
	fn resolve_dir(&mut self, path: &[u8]) -> Result<u32, FsError> {
		let mut cluster = self.root_cluster();
		for seg in path.split(|&b| b == b'/').filter(|s| !s.is_empty()) {
			let e = self.find_entry(cluster, seg)?;
			if !e.is_dir {
				return Err(FsError::NotFound);
			}
			cluster = e.first_cluster;
		}
		Ok(cluster)
	}

	// Find the entry named `name` (case-insensitive, ASCII) in the directory at
	// `cluster`, or NotFound. Reuses the same scan the listing does.
	fn find_entry(&mut self, cluster: u32, name: &[u8]) -> Result<Raw, FsError> {
		let entries = self.scan_dir(cluster)?;
		entries.into_iter().find(|e| eq_ignore_case(e.name.as_bytes(), name)).ok_or(FsError::NotFound)
	}

	// The listing of a directory: name + size + is_dir, dropping the "." / ".." links.
	fn read_dir(&mut self, cluster: u32) -> Result<Vec<FileInfo>, FsError> {
		let raw = self.scan_dir(cluster)?;
		Ok(raw.into_iter().filter(|e| e.name != "." && e.name != "..").map(|e| FileInfo { name: e.name, size: e.size, is_dir: e.is_dir }).collect())
	}

	// Read a directory's bytes (the fixed root region or a cluster chain) and parse its
	// entries, choosing the classic or the exFAT entry format.
	fn scan_dir(&mut self, cluster: u32) -> Result<Vec<Raw>, FsError> {
		let bytes = if cluster == 0 { self.read_root_region()? } else { self.read_chain(cluster, usize::MAX)? };
		match self.geo.kind {
			Kind::ExFat => parse_exfat_dir(&bytes),
			_ => parse_fat_dir(&bytes),
		}
	}

	// Read the fixed-size root directory region of a FAT12/16 volume into a Vec.
	fn read_root_region(&mut self) -> Result<Vec<u8>, FsError> {
		let root_sectors = (self.geo.root_entries * 32).div_ceil(self.geo.bytes_per_sector);
		let start = self.geo.reserved_sectors + self.geo.num_fats * self.geo.fat_size;
		let mut out = vec![0u8; (root_sectors * self.geo.bytes_per_sector) as usize];
		self.read_fs_sectors(start as u64, root_sectors, &mut out)?;
		Ok(out)
	}

	// Read a cluster chain starting at `first`, up to `limit` bytes (usize::MAX = the
	// whole chain), following the allocation table. Returns the bytes read.
	fn read_chain(&mut self, first: u32, limit: usize) -> Result<Vec<u8>, FsError> {
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		let mut out: Vec<u8> = Vec::new();
		let mut cluster = first;
		let mut guard = 0u32;
		while cluster >= 2 && !self.is_end(cluster) {
			let lba = self.cluster_lba(cluster);
			let mut buf = vec![0u8; cluster_bytes];
			self.read_fs_sectors(lba, self.geo.sectors_per_cluster, &mut buf)?;
			out.extend_from_slice(&buf);
			if out.len() >= limit {
				break;
			}
			cluster = self.next_cluster(cluster)?;
			guard += 1;
			if guard > self.geo.fat_size * (self.geo.bytes_per_sector / 4 + 1) {
				return Err(FsError::Invalid);
			}
		}
		if limit != usize::MAX {
			out.truncate(limit);
		}
		Ok(out)
	}

	// The first 512-byte LBA of `cluster` in the data region (clusters number from 2).
	fn cluster_lba(&self, cluster: u32) -> u64 {
		let fs = self.geo.first_data_sector as u64 + (cluster as u64 - 2) * self.geo.sectors_per_cluster as u64;
		fs * (self.geo.bytes_per_sector / SECTOR_SIZE as u32) as u64
	}

	// Read `count` logical sectors starting at fs sector `sec` into `buf`, expanding each
	// logical sector to its 512-byte device sectors.
	fn read_fs_sectors(&mut self, sec: u64, count: u32, buf: &mut [u8]) -> Result<(), FsError> {
		let ratio = (self.geo.bytes_per_sector / SECTOR_SIZE as u32) as u64;
		let total = count as u64 * ratio;
		for i in 0..total {
			let off = i as usize * SECTOR_SIZE;
			let mut s = [0u8; SECTOR_SIZE];
			if !self.dev.read_sector(sec * ratio + i, &mut s) {
				return Err(FsError::Io);
			}
			buf[off..off + SECTOR_SIZE].copy_from_slice(&s);
		}
		Ok(())
	}

	// The FAT entry for `cluster` - the next cluster in its chain - read from the first
	// allocation table. FAT12 packs entries in 1.5 bytes, FAT16 in 2, FAT32/exFAT in 4.
	fn next_cluster(&mut self, cluster: u32) -> Result<u32, FsError> {
		let bps = self.geo.bytes_per_sector;
		let fat_base = self.geo.reserved_sectors;
		let byte_off = match self.geo.kind {
			Kind::Fat12 => cluster as u64 + (cluster as u64 / 2),
			Kind::Fat16 => cluster as u64 * 2,
			Kind::Fat32 | Kind::ExFat => cluster as u64 * 4,
		};
		let sec = fat_base as u64 + byte_off / bps as u64;
		let within = (byte_off % bps as u64) as usize;
		let mut buf = vec![0u8; (bps * 2) as usize];
		self.read_fs_sectors(sec, 2, &mut buf)?;
		Ok(match self.geo.kind {
			Kind::Fat12 => {
				let v = u16::from_le_bytes([buf[within], buf[within + 1]]);
				if cluster & 1 == 1 { (v >> 4) as u32 } else { (v & 0x0FFF) as u32 }
			}
			Kind::Fat16 => u16::from_le_bytes([buf[within], buf[within + 1]]) as u32,
			Kind::Fat32 | Kind::ExFat => u32::from_le_bytes([buf[within], buf[within + 1], buf[within + 2], buf[within + 3]]) & 0x0FFF_FFFF,
		})
	}

	// True when a FAT entry is an end-of-chain marker for the family's width.
	fn is_end(&self, cluster: u32) -> bool {
		match self.geo.kind {
			Kind::Fat12 => cluster >= 0x0FF8,
			Kind::Fat16 => cluster >= 0xFFF8,
			Kind::Fat32 | Kind::ExFat => cluster >= 0x0FFF_FFF8,
		}
	}
}

// A directory entry as parsed off disk, before it becomes a FileInfo: keeps the first
// cluster so a file's bytes or a subdirectory can be read.
struct Raw {
	name: String,
	size: u64,
	is_dir: bool,
	first_cluster: u32,
}

impl Geometry {
	// Parse a FAT12/16/32 BIOS Parameter Block and classify by cluster count.
	fn bpb(b: &[u8]) -> Option<Geometry> {
		let bytes_per_sector = u16::from_le_bytes([b[11], b[12]]) as u32;
		let sectors_per_cluster = b[13] as u32;
		if bytes_per_sector < 512 || bytes_per_sector % 512 != 0 || sectors_per_cluster == 0 {
			return None;
		}
		let reserved_sectors = u16::from_le_bytes([b[14], b[15]]) as u32;
		let num_fats = b[16] as u32;
		let root_entries = u16::from_le_bytes([b[17], b[18]]) as u32;
		let total16 = u16::from_le_bytes([b[19], b[20]]) as u32;
		let fat16 = u16::from_le_bytes([b[22], b[23]]) as u32;
		let total32 = u32::from_le_bytes([b[32], b[33], b[34], b[35]]);
		let fat32 = u32::from_le_bytes([b[36], b[37], b[38], b[39]]);
		let total = if total16 != 0 { total16 } else { total32 };
		let fat_size = if fat16 != 0 { fat16 } else { fat32 };
		if num_fats == 0 || fat_size == 0 || total == 0 {
			return None;
		}
		let root_sectors = (root_entries * 32).div_ceil(bytes_per_sector);
		let first_data_sector = reserved_sectors + num_fats * fat_size + root_sectors;
		let clusters = (total - first_data_sector) / sectors_per_cluster;
		let kind = if clusters < 4085 {
			Kind::Fat12
		} else if clusters < 65525 {
			Kind::Fat16
		} else {
			Kind::Fat32
		};
		let root_cluster = if kind == Kind::Fat32 { u32::from_le_bytes([b[44], b[45], b[46], b[47]]) } else { 0 };
		Some(Geometry { kind, bytes_per_sector, sectors_per_cluster, reserved_sectors, num_fats, fat_size, root_entries, root_cluster, first_data_sector })
	}

	// Parse an exFAT boot sector. exFAT keeps everything in the cluster heap, so the root
	// region is a chain like any directory and root_entries is unused.
	fn exfat(b: &[u8]) -> Option<Geometry> {
		let fat_offset = u32::from_le_bytes([b[80], b[81], b[82], b[83]]);
		let fat_size = u32::from_le_bytes([b[84], b[85], b[86], b[87]]);
		let cluster_heap_offset = u32::from_le_bytes([b[88], b[89], b[90], b[91]]);
		let root_cluster = u32::from_le_bytes([b[96], b[97], b[98], b[99]]);
		let bytes_per_sector = 1u32 << b[108];
		let sectors_per_cluster = 1u32 << b[109];
		let num_fats = b[110] as u32;
		if bytes_per_sector < 512 || sectors_per_cluster == 0 || num_fats == 0 || cluster_heap_offset < 2 {
			return None;
		}
		Some(Geometry { kind: Kind::ExFat, bytes_per_sector, sectors_per_cluster, reserved_sectors: fat_offset, num_fats: 1, fat_size, root_entries: 0, root_cluster, first_data_sector: cluster_heap_offset })
	}
}

// Parse a classic (FAT12/16/32) directory region: 32-byte entries, with attr 0x0F VFAT
// long-name fragments accumulated ahead of the 8.3 short entry they describe.
fn parse_fat_dir(bytes: &[u8]) -> Result<Vec<Raw>, FsError> {
	let mut out: Vec<Raw> = Vec::new();
	let mut lfn: Vec<u16> = Vec::new();
	let mut i = 0;
	while i + 32 <= bytes.len() {
		let e = &bytes[i..i + 32];
		i += 32;
		if e[0] == 0x00 {
			break;
		}
		if e[0] == 0xE5 {
			lfn.clear();
			continue;
		}
		if e[11] == 0x0F {
			// a long-name fragment: 13 UTF-16 chars at offsets 1, 14, 28, ordered by the
			// sequence number, prepended so the assembled name reads forwards.
			let mut part: Vec<u16> = Vec::new();
			for &r in &[1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30] {
				part.push(u16::from_le_bytes([e[r], e[r + 1]]));
			}
			let mut merged = part;
			merged.extend_from_slice(&lfn);
			lfn = merged;
			continue;
		}
		if e[11] & 0x08 != 0 {
			lfn.clear();
			continue;
		}
		let name = if !lfn.is_empty() { decode_utf16(&lfn) } else { short_name(e) };
		lfn = Vec::new();
		let is_dir = e[11] & 0x10 != 0;
		let first_cluster = ((u16::from_le_bytes([e[20], e[21]]) as u32) << 16) | u16::from_le_bytes([e[26], e[27]]) as u32;
		let size = u32::from_le_bytes([e[28], e[29], e[30], e[31]]) as u64;
		out.push(Raw { name, size, is_dir, first_cluster });
	}
	Ok(out)
}

// Parse an exFAT directory: a file is an entry set of a 0x85 file, a 0xC0 stream
// extension (length + first cluster), and one or more 0xC1 file-name fragments.
fn parse_exfat_dir(bytes: &[u8]) -> Result<Vec<Raw>, FsError> {
	let mut out: Vec<Raw> = Vec::new();
	let mut i = 0;
	while i + 32 <= bytes.len() {
		let e = &bytes[i..i + 32];
		if e[0] == 0x00 {
			break;
		}
		if e[0] != 0x85 {
			i += 32;
			continue;
		}
		let secondary = e[1] as usize;
		let is_dir = u16::from_le_bytes([e[4], e[5]]) & 0x10 != 0;
		let mut name: Vec<u16> = Vec::new();
		let mut size = 0u64;
		let mut first_cluster = 0u32;
		let mut name_len = 0usize;
		for k in 1..=secondary {
			let s = i + k * 32;
			if s + 32 > bytes.len() {
				break;
			}
			let x = &bytes[s..s + 32];
			if x[0] == 0xC0 {
				name_len = x[3] as usize;
				first_cluster = u32::from_le_bytes([x[20], x[21], x[22], x[23]]);
				size = u64::from_le_bytes([x[24], x[25], x[26], x[27], x[28], x[29], x[30], x[31]]);
			} else if x[0] == 0xC1 {
				for c in 0..15 {
					name.push(u16::from_le_bytes([x[2 + c * 2], x[3 + c * 2]]));
				}
			}
		}
		name.truncate(name_len);
		out.push(Raw { name: decode_utf16(&name), size, is_dir, first_cluster });
		i += (secondary + 1) * 32;
	}
	Ok(out)
}

// Decode a UTF-16 name, dropping NUL padding and replacing anything invalid with '?'.
fn decode_utf16(units: &[u16]) -> String {
	let mut s = String::new();
	for c in char::decode_utf16(units.iter().copied().take_while(|&u| u != 0)) {
		s.push(c.unwrap_or('?'));
	}
	s
}

// The 8.3 short name of a classic entry: name, optional dot, extension, trimmed.
fn short_name(e: &[u8]) -> String {
	let base = trim_spaces(&e[0..8]);
	let ext = trim_spaces(&e[8..11]);
	let mut s = String::from_utf8_lossy(base).into_owned();
	if !ext.is_empty() {
		s.push('.');
		s.push_str(&String::from_utf8_lossy(ext));
	}
	s
}

// Drop trailing 0x20 padding from a fixed-width 8.3 field.
fn trim_spaces(b: &[u8]) -> &[u8] {
	let mut end = b.len();
	while end > 0 && b[end - 1] == 0x20 {
		end -= 1;
	}
	&b[..end]
}

// Split a path into (parent dir, final name), rejecting an empty final name.
fn split_parent(path: &[u8]) -> Result<(&[u8], &[u8]), FsError> {
	let trimmed = path.strip_suffix(b"/").unwrap_or(path);
	match trimmed.iter().rposition(|&b| b == b'/') {
		Some(p) => Ok((&trimmed[..p], &trimmed[p + 1..])),
		None => Ok((b"", trimmed)),
	}
}

// Compare two names ignoring ASCII case, as FAT lookups are case-insensitive.
fn eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
	a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}
