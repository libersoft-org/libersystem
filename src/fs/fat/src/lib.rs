//! FAT - a read-only FAT12 / FAT16 / FAT32 and exFAT backend for foreign removable
//! media (USB sticks, SD cards, install images), behind the same [`BlockDevice`] trait
//! LiberFS uses. It sits behind `Storage.Volume` as just another FS backend: per the
//! layering principle, several filesystems mount behind one volume API, and FAT is the
//! ubiquitous interchange format so reading it makes those media readable.
//!
//! Read-first by design, with a full write path. The boot sector is parsed and the
//! family auto-detected: a small cluster count is FAT12, a medium one FAT16, a large one
//! FAT32, and an `EXFAT ` magic is exFAT. A file is found by walking `/`-separated path
//! segments from the root, each lookup scanning a directory's 32-byte entries (assembling
//! VFAT long file names from their UTF-16 fragments, or the exFAT file-name entry set) and
//! following the cluster chain through the allocation table. All four families also
//! create, overwrite, and delete files - FAT12/16/32 allocate from the FAT and write
//! every copy; exFAT allocates from the allocation bitmap and writes its 0x85/0xC0/0xC1
//! entry sets, so >4 GB removable media is writable.

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

// A block device: foreign media is read and written one 512-byte sector at a time, by
// absolute LBA. Implementors map that onto their backing (disk sectors, a Vec). The read
// path mounts and lists; the write path creates, overwrites, and deletes files.
pub trait BlockDevice {
	// Read sector `lba` into `buf` (exactly SECTOR_SIZE bytes). False on I/O failure.
	fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> bool;
	// Write `buf` (exactly SECTOR_SIZE bytes) to sector `lba`. False on I/O failure.
	fn write_sector(&mut self, lba: u64, buf: &[u8]) -> bool;
}

// A FAT error. The variants map onto the `Storage.Volume` `error` enum at the service
// boundary (NotFound -> not-found, the rest -> invalid / again).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
	NotFound,
	Invalid,
	TooLong,
	NoSpace,
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

	// The mounted family's name ("fat12" / "fat16" / "fat32" / "exfat"), for volume
	// status reporting.
	pub fn kind_name(&self) -> &'static str {
		match self.geo.kind {
			Kind::Fat12 => "fat12",
			Kind::Fat16 => "fat16",
			Kind::Fat32 => "fat32",
			Kind::ExFat => "exfat",
		}
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

	// Create or overwrite a file named by a `/`-separated path with `data`, allocating a
	// cluster chain and writing a directory entry, for any of the four families.
	pub fn write_file(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
		let (parent, name) = split_parent(path)?;
		if name.is_empty() || name.len() > 255 {
			return Err(FsError::TooLong);
		}
		let dir = self.resolve_dir(parent)?;
		if self.geo.kind == Kind::ExFat {
			return self.exfat_write(dir, name, data);
		}
		self.unlink_in(dir, name, false)?;
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		let need = data.len().div_ceil(cluster_bytes);
		let chain = self.alloc_chain(need)?;
		for (i, c) in chain.iter().enumerate() {
			let mut buf = vec![0u8; cluster_bytes];
			let off = i * cluster_bytes;
			let end = (off + cluster_bytes).min(data.len());
			if off < data.len() {
				buf[..end - off].copy_from_slice(&data[off..end]);
			}
			let fs = self.geo.first_data_sector as u64 + (*c as u64 - 2) * self.geo.sectors_per_cluster as u64;
			self.write_fs_sectors(fs, self.geo.sectors_per_cluster, &buf)?;
		}
		let first = chain.first().copied().unwrap_or(0);
		self.add_entry(dir, name, first, data.len() as u32, 0x20)
	}

	// Delete a file named by a `/`-separated path: free its cluster chain and clear its
	// directory entry, for any of the four families.
	pub fn remove(&mut self, path: &[u8]) -> Result<(), FsError> {
		let (parent, name) = split_parent(path)?;
		let dir = self.resolve_dir(parent)?;
		if self.geo.kind == Kind::ExFat {
			return self.exfat_remove(dir, name);
		}
		if !self.unlink_in(dir, name, true)? {
			return Err(FsError::NotFound);
		}
		Ok(())
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

	// Write `count` logical sectors of `buf` starting at fs sector `sec`, expanding each
	// logical sector to its 512-byte device sectors. The write mirror of read_fs_sectors.
	fn write_fs_sectors(&mut self, sec: u64, count: u32, buf: &[u8]) -> Result<(), FsError> {
		let ratio = (self.geo.bytes_per_sector / SECTOR_SIZE as u32) as u64;
		let total = count as u64 * ratio;
		for i in 0..total {
			let off = i as usize * SECTOR_SIZE;
			if !self.dev.write_sector(sec * ratio + i, &buf[off..off + SECTOR_SIZE]) {
				return Err(FsError::Io);
			}
		}
		Ok(())
	}

	// The last usable cluster index, derived from the FAT size and entry width: a chain
	// is allocated by scanning [2, max_cluster] for free entries.
	fn max_cluster(&self) -> u32 {
		let bytes = self.geo.fat_size as u64 * self.geo.bytes_per_sector as u64;
		let entries = match self.geo.kind {
			Kind::Fat12 => bytes * 2 / 3,
			Kind::Fat16 => bytes / 2,
			Kind::Fat32 | Kind::ExFat => bytes / 4,
		};
		entries.saturating_sub(1) as u32
	}

	// Write `val` into `cluster`'s FAT slot, in every FAT copy. FAT12 packs two entries
	// into three bytes, so a slot is read-modified-written; FAT16/32 align to the width.
	fn set_fat_entry(&mut self, cluster: u32, val: u32) -> Result<(), FsError> {
		let bps = self.geo.bytes_per_sector;
		let byte_off = match self.geo.kind {
			Kind::Fat12 => cluster as u64 + (cluster as u64 / 2),
			Kind::Fat16 => cluster as u64 * 2,
			Kind::Fat32 | Kind::ExFat => cluster as u64 * 4,
		};
		for fat in 0..self.geo.num_fats {
			let fat_base = self.geo.reserved_sectors + fat * self.geo.fat_size;
			let sec = fat_base as u64 + byte_off / bps as u64;
			let within = (byte_off % bps as u64) as usize;
			let mut buf = vec![0u8; (bps * 2) as usize];
			self.read_fs_sectors(sec, 2, &mut buf)?;
			match self.geo.kind {
				Kind::Fat12 => {
					let cur = u16::from_le_bytes([buf[within], buf[within + 1]]);
					let next = if cluster & 1 == 1 { (cur & 0x000F) | ((val as u16) << 4) } else { (cur & 0xF000) | (val as u16 & 0x0FFF) };
					buf[within..within + 2].copy_from_slice(&next.to_le_bytes());
				}
				Kind::Fat16 => buf[within..within + 2].copy_from_slice(&(val as u16).to_le_bytes()),
				Kind::Fat32 | Kind::ExFat => buf[within..within + 4].copy_from_slice(&(val & 0x0FFF_FFFF).to_le_bytes()),
			}
			self.write_fs_sectors(sec, 2, &buf)?;
		}
		Ok(())
	}

	// Allocate `n` free clusters into an end-terminated chain, returning them in order.
	// Zero clusters is an empty file. NoSpace if the table runs out of free entries.
	fn alloc_chain(&mut self, n: usize) -> Result<Vec<u32>, FsError> {
		let mut chain: Vec<u32> = Vec::with_capacity(n);
		let mut c = 2u32;
		let max = self.max_cluster();
		while chain.len() < n {
			if c > max {
				return Err(FsError::NoSpace);
			}
			if self.next_cluster(c)? == 0 && !chain.contains(&c) {
				chain.push(c);
			}
			c += 1;
		}
		let eoc = 0x0FFF_FFFF;
		for i in 0..chain.len() {
			let val = if i + 1 < chain.len() { chain[i + 1] } else { eoc };
			self.set_fat_entry(chain[i], val)?;
		}
		Ok(chain)
	}

	// Free a cluster chain, marking each slot free. Cluster 0 means no chain.
	fn free_chain(&mut self, first: u32) -> Result<(), FsError> {
		let mut cluster = first;
		let mut guard = 0u32;
		while cluster >= 2 && !self.is_end(cluster) {
			let next = self.next_cluster(cluster)?;
			self.set_fat_entry(cluster, 0)?;
			cluster = next;
			guard += 1;
			if guard > self.max_cluster() {
				break;
			}
		}
		Ok(())
	}

	// Read a directory's raw bytes: the fixed root region for FAT12/16, else its chain.
	fn read_dir_bytes(&mut self, cluster: u32) -> Result<Vec<u8>, FsError> {
		if cluster == 0 { self.read_root_region() } else { self.read_chain(cluster, usize::MAX) }
	}

	// Write a directory's raw bytes back, to the fixed root region or its cluster chain.
	fn write_dir_bytes(&mut self, cluster: u32, bytes: &[u8]) -> Result<(), FsError> {
		if cluster == 0 {
			let start = self.geo.reserved_sectors + self.geo.num_fats * self.geo.fat_size;
			let sectors = (self.geo.root_entries * 32).div_ceil(self.geo.bytes_per_sector);
			self.write_fs_sectors(start as u64, sectors, bytes)?;
			return Ok(());
		}
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		let mut c = cluster;
		let mut off = 0usize;
		while off < bytes.len() && c >= 2 && !self.is_end(c) {
			let fs = self.geo.first_data_sector as u64 + (c as u64 - 2) * self.geo.sectors_per_cluster as u64;
			self.write_fs_sectors(fs, self.geo.sectors_per_cluster, &bytes[off..off + cluster_bytes])?;
			off += cluster_bytes;
			c = self.next_cluster(c)?;
		}
		Ok(())
	}

	// Remove the entry named `name` from directory `cluster`: clear its 8.3 plus any long
	// fragments and, if `free`, release its chain. Returns whether the name was present.
	fn unlink_in(&mut self, cluster: u32, name: &[u8], free: bool) -> Result<bool, FsError> {
		let mut bytes = self.read_dir_bytes(cluster)?;
		let mut start = 0usize;
		let mut i = 0usize;
		while i + 32 <= bytes.len() {
			let e = &bytes[i..i + 32];
			if e[0] == 0x00 {
				return Ok(false);
			}
			if e[0] == 0xE5 || e[11] == 0x0F {
				i += 32;
				continue;
			}
			if e[11] & 0x08 != 0 {
				start = i + 32;
				i += 32;
				continue;
			}
			let nm = short_name(e).into_bytes();
			let first = ((u16::from_le_bytes([e[20], e[21]]) as u32) << 16) | u16::from_le_bytes([e[26], e[27]]) as u32;
			if eq_ignore_case(&nm, name) {
				if e[11] & 0x10 != 0 {
					return Err(FsError::Invalid);
				}
				for off in (start..=i).step_by(32) {
					bytes[off] = 0xE5;
				}
				self.write_dir_bytes(cluster, &bytes)?;
				if free {
					self.free_chain(first)?;
				}
				return Ok(true);
			}
			start = i + 32;
			i += 32;
		}
		Ok(false)
	}

	// Add a directory entry for `name` (8.3 plus long fragments when needed), pointing at
	// `first` cluster with `size` bytes and `attr`. NoSpace if the directory is full.
	fn add_entry(&mut self, cluster: u32, name: &[u8], first: u32, size: u32, attr: u8) -> Result<(), FsError> {
		let entries = build_entries(name, first, size, attr);
		let mut bytes = self.read_dir_bytes(cluster)?;
		let need = entries.len() * 32;
		let slot = free_run(&bytes, entries.len());
		let at = match slot {
			Some(p) => p,
			None => {
				if cluster == 0 {
					return Err(FsError::NoSpace);
				}
				let grow = self.alloc_chain(1)?[0];
				let last = self.last_cluster(cluster)?;
				self.set_fat_entry(last, grow)?;
				let p = bytes.len();
				bytes.resize(p + (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize, 0);
				p
			}
		};
		for (k, e) in entries.iter().enumerate() {
			bytes[at + k * 32..at + k * 32 + 32].copy_from_slice(e);
		}
		let _ = need;
		self.write_dir_bytes(cluster, &bytes)
	}

	// The last cluster of a chain, for appending: walk to the end-of-chain marker.
	fn last_cluster(&mut self, first: u32) -> Result<u32, FsError> {
		let mut c = first;
		loop {
			let next = self.next_cluster(c)?;
			if self.is_end(next) {
				return Ok(c);
			}
			c = next;
		}
	}

	// Create or overwrite an exFAT file: drop any existing entry set, allocate the data
	// clusters from the allocation bitmap, write them, and add the 0x85 / 0xC0 / 0xC1 set.
	fn exfat_write(&mut self, dir: u32, name: &[u8], data: &[u8]) -> Result<(), FsError> {
		self.exfat_unlink(dir, name, false)?;
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		let need = data.len().div_ceil(cluster_bytes);
		let chain = self.exfat_alloc(need)?;
		for (i, c) in chain.iter().enumerate() {
			let mut buf = vec![0u8; cluster_bytes];
			let off = i * cluster_bytes;
			let end = (off + cluster_bytes).min(data.len());
			if off < data.len() {
				buf[..end - off].copy_from_slice(&data[off..end]);
			}
			let fs = self.geo.first_data_sector as u64 + (*c as u64 - 2) * self.geo.sectors_per_cluster as u64;
			self.write_fs_sectors(fs, self.geo.sectors_per_cluster, &buf)?;
		}
		let first = chain.first().copied().unwrap_or(0);
		self.exfat_add_entry(dir, name, first, data.len() as u64)
	}

	// Delete an exFAT file: clear its entry set and free its cluster chain and bitmap bits.
	fn exfat_remove(&mut self, dir: u32, name: &[u8]) -> Result<(), FsError> {
		if !self.exfat_unlink(dir, name, true)? {
			return Err(FsError::NotFound);
		}
		Ok(())
	}

	// Locate the allocation bitmap (the 0x81 entry in the root): its first cluster and its
	// byte length. exFAT tracks free clusters as a bit per cluster, set when allocated.
	fn exfat_bitmap(&mut self) -> Result<(u32, u64), FsError> {
		let bytes = self.read_dir_bytes(self.geo.root_cluster)?;
		let mut i = 0;
		while i + 32 <= bytes.len() {
			let e = &bytes[i..i + 32];
			if e[0] == 0x00 {
				break;
			}
			if e[0] == 0x81 {
				let first = u32::from_le_bytes([e[20], e[21], e[22], e[23]]);
				let size = u64::from_le_bytes([e[24], e[25], e[26], e[27], e[28], e[29], e[30], e[31]]);
				return Ok((first, size));
			}
			i += 32;
		}
		Err(FsError::Invalid)
	}

	// Allocate `n` clusters from the bitmap into a FAT-linked chain, returning them in
	// order. Sets the bitmap bit and the FAT entry of each; NoSpace if the volume is full.
	fn exfat_alloc(&mut self, n: usize) -> Result<Vec<u32>, FsError> {
		if n == 0 {
			return Ok(Vec::new());
		}
		let (bm_first, _bm_size) = self.exfat_bitmap()?;
		let mut bm = self.read_chain(bm_first, usize::MAX)?;
		let max = self.max_cluster();
		let mut chain: Vec<u32> = Vec::with_capacity(n);
		let mut c = 2u32;
		while chain.len() < n {
			if c > max {
				return Err(FsError::NoSpace);
			}
			let idx = (c - 2) as usize;
			let byte = idx / 8;
			let bit = idx % 8;
			if byte < bm.len() && bm[byte] & (1 << bit) == 0 {
				bm[byte] |= 1 << bit;
				chain.push(c);
			}
			c += 1;
		}
		self.write_dir_bytes(bm_first, &bm)?;
		let eoc = 0x0FFF_FFFF;
		for i in 0..chain.len() {
			let val = if i + 1 < chain.len() { chain[i + 1] } else { eoc };
			self.set_fat_entry(chain[i], val)?;
		}
		Ok(chain)
	}

	// Free an exFAT chain: clear each cluster's bitmap bit and FAT slot. First 0 = none.
	fn exfat_free(&mut self, first: u32) -> Result<(), FsError> {
		if first < 2 {
			return Ok(());
		}
		let (bm_first, _bm_size) = self.exfat_bitmap()?;
		let mut bm = self.read_chain(bm_first, usize::MAX)?;
		let mut cluster = first;
		let mut guard = 0u32;
		while cluster >= 2 && !self.is_end(cluster) {
			let next = self.next_cluster(cluster)?;
			let idx = (cluster - 2) as usize;
			let byte = idx / 8;
			if byte < bm.len() {
				bm[byte] &= !(1 << (idx % 8));
			}
			self.set_fat_entry(cluster, 0)?;
			cluster = next;
			guard += 1;
			if guard > self.max_cluster() {
				break;
			}
		}
		self.write_dir_bytes(bm_first, &bm)
	}

	// Add an exFAT entry set (0x85 file + 0xC0 stream + 0xC1 names) to directory `dir`,
	// pointing at `first` cluster with `size` bytes. NoSpace if the directory is full.
	fn exfat_add_entry(&mut self, dir: u32, name: &[u8], first: u32, size: u64) -> Result<(), FsError> {
		let set = build_exfat_set(name, first, size);
		let mut bytes = self.read_dir_bytes(dir)?;
		let count = set.len() / 32;
		let at = exfat_free_run(&bytes, count).ok_or(FsError::NoSpace)?;
		bytes[at..at + set.len()].copy_from_slice(&set);
		self.write_dir_bytes(dir, &bytes)
	}

	// Remove the entry named `name` from exFAT directory `dir`: clear the type bit of each
	// entry in its set and, if `free`, release its chain. Returns whether it was present.
	fn exfat_unlink(&mut self, dir: u32, name: &[u8], free: bool) -> Result<bool, FsError> {
		let mut bytes = self.read_dir_bytes(dir)?;
		let mut i = 0;
		while i + 32 <= bytes.len() {
			if bytes[i] == 0x00 {
				break;
			}
			if bytes[i] != 0x85 {
				i += 32;
				continue;
			}
			let secondary = bytes[i + 1] as usize;
			let mut nm: Vec<u16> = Vec::new();
			let mut len = 0usize;
			let mut first = 0u32;
			for k in 1..=secondary {
				let s = i + k * 32;
				if s + 32 > bytes.len() {
					break;
				}
				let x = &bytes[s..s + 32];
				if x[0] == 0xC0 {
					len = x[3] as usize;
					first = u32::from_le_bytes([x[20], x[21], x[22], x[23]]);
				} else if x[0] == 0xC1 {
					for c in 0..15 {
						nm.push(u16::from_le_bytes([x[2 + c * 2], x[3 + c * 2]]));
					}
				}
			}
			nm.truncate(len);
			if eq_ignore_case(decode_utf16(&nm).as_bytes(), name) {
				for k in 0..=secondary {
					let off = i + k * 32;
					if off < bytes.len() {
						bytes[off] &= 0x7F;
					}
				}
				self.write_dir_bytes(dir, &bytes)?;
				if free {
					self.exfat_free(first)?;
				}
				return Ok(true);
			}
			i += (secondary + 1) * 32;
		}
		Ok(false)
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
		// FAT32 announces itself by its BPB shape - no fixed root region and the FAT
		// size in the 32-bit field - regardless of the cluster count: a small FAT32
		// volume (e.g. an mtools-formatted stick) sits inside the FAT16 cluster range,
		// so the count thresholds alone would misclassify it (and then read an empty
		// fixed root region that does not exist). The thresholds decide FAT12 vs FAT16
		// for the classic layouts only.
		let kind = if root_entries == 0 && fat16 == 0 {
			Kind::Fat32
		} else if clusters < 4085 {
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

// Build a directory entry set: the 8.3 short entry, preceded by VFAT long-name fragments
// when the name is not a plain uppercase 8.3 name. Fragments are emitted last-first.
fn build_entries(name: &[u8], first: u32, size: u32, attr: u8) -> Vec<[u8; 32]> {
	let short = gen_short(name);
	let mut out: Vec<[u8; 32]> = Vec::new();
	if name != short_name(&short).as_bytes() {
		let units: Vec<u16> = String::from_utf8_lossy(name).encode_utf16().collect();
		let sum = lfn_checksum(&short);
		let frags = units.len().div_ceil(13).max(1);
		for f in (0..frags).rev() {
			let mut e = [0u8; 32];
			e[0] = (f as u8 + 1) | if f + 1 == frags { 0x40 } else { 0 };
			e[11] = 0x0F;
			e[13] = sum;
			for c in 0..13 {
				let idx = f * 13 + c;
				let v = if idx < units.len() {
					units[idx]
				} else if idx == units.len() {
					0
				} else {
					0xFFFF
				};
				let pos = [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30][c];
				e[pos..pos + 2].copy_from_slice(&v.to_le_bytes());
			}
			out.push(e);
		}
	}
	let mut e = [0u8; 32];
	e[0..11].copy_from_slice(&short);
	e[11] = attr;
	e[20..22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
	e[26..28].copy_from_slice(&(first as u16).to_le_bytes());
	e[28..32].copy_from_slice(&size.to_le_bytes());
	out.push(e);
	out
}

// Generate an 8.3 short name padded with spaces: base in 8 columns, extension in 3,
// uppercased. Names that do not fit are accepted on a best-effort, space-padded basis.
fn gen_short(name: &[u8]) -> [u8; 11] {
	let mut s = [0x20u8; 11];
	let dot = name.iter().rposition(|&b| b == b'.');
	let (base, ext): (&[u8], &[u8]) = match dot {
		Some(p) => (&name[..p], &name[p + 1..]),
		None => (name, b""),
	};
	for (i, &b) in base.iter().take(8).enumerate() {
		s[i] = b.to_ascii_uppercase();
	}
	for (i, &b) in ext.iter().take(3).enumerate() {
		s[8 + i] = b.to_ascii_uppercase();
	}
	s
}

// The VFAT checksum of an 8.3 name: a byte-by-byte rotate-and-add, stamped on every long
// fragment so a stale fragment cannot be paired with the wrong short entry.
fn lfn_checksum(short: &[u8; 11]) -> u8 {
	let mut sum: u8 = 0;
	for &b in short {
		sum = sum.rotate_right(1).wrapping_add(b);
	}
	sum
}

// Find the offset of the first run of `n` free entries (0x00 fresh or 0xE5 deleted) in a
// directory region, or None when there is no contiguous gap that large.
fn free_run(bytes: &[u8], n: usize) -> Option<usize> {
	let mut run = 0usize;
	let mut i = 0usize;
	while i + 32 <= bytes.len() {
		if bytes[i] == 0x00 || bytes[i] == 0xE5 {
			run += 1;
			if run == n {
				return Some(i + 32 - n * 32);
			}
		} else {
			run = 0;
		}
		i += 32;
	}
	None
}

// Build an exFAT entry set for `name`: a 0x85 file entry, a 0xC0 stream extension (FAT
// chain, length, first cluster), and 0xC1 name fragments, stamped with the set checksum.
fn build_exfat_set(name: &[u8], first: u32, size: u64) -> Vec<u8> {
	let units: Vec<u16> = String::from_utf8_lossy(name).encode_utf16().collect();
	let frags = units.len().div_ceil(15);
	let count = 1 + frags;
	let mut set = vec![0u8; (count + 1) * 32];
	set[0] = 0x85;
	set[1] = count as u8;
	set[4..6].copy_from_slice(&0x20u16.to_le_bytes());
	set[32] = 0xC0;
	set[33] = 0x01;
	set[35] = units.len() as u8;
	set[36..38].copy_from_slice(&exfat_name_hash(&units).to_le_bytes());
	set[40..48].copy_from_slice(&size.to_le_bytes());
	set[52..56].copy_from_slice(&first.to_le_bytes());
	set[56..64].copy_from_slice(&size.to_le_bytes());
	for f in 0..frags {
		let base = (2 + f) * 32;
		set[base] = 0xC1;
		for c in 0..15 {
			let idx = f * 15 + c;
			let v = if idx < units.len() { units[idx] } else { 0 };
			set[base + 2 + c * 2..base + 4 + c * 2].copy_from_slice(&v.to_le_bytes());
		}
	}
	let sum = exfat_set_checksum(&set);
	set[2..4].copy_from_slice(&sum.to_le_bytes());
	set
}

// The exFAT entry-set checksum: a 16-bit rotate-and-add over every byte of the set,
// skipping the two checksum bytes (2, 3) of the first entry where the value lands.
fn exfat_set_checksum(set: &[u8]) -> u16 {
	let mut sum: u16 = 0;
	for (i, &b) in set.iter().enumerate() {
		if i == 2 || i == 3 {
			continue;
		}
		sum = sum.rotate_right(1).wrapping_add(b as u16);
	}
	sum
}

// The exFAT file-name hash, over the UTF-16LE name bytes; not verified by the reader but
// written for correctness so a real exFAT driver accepts the file.
fn exfat_name_hash(units: &[u16]) -> u16 {
	let mut hash: u16 = 0;
	for &u in units {
		for b in u.to_le_bytes() {
			hash = hash.rotate_right(1).wrapping_add(b as u16);
		}
	}
	hash
}

// Find the first run of `n` free exFAT entries (0x00 fresh or a cleared type bit) in a
// directory region, or None when there is no contiguous gap that large.
fn exfat_free_run(bytes: &[u8], n: usize) -> Option<usize> {
	let mut run = 0usize;
	let mut i = 0usize;
	while i + 32 <= bytes.len() {
		if bytes[i] == 0x00 || bytes[i] & 0x80 == 0 {
			run += 1;
			if run == n {
				return Some(i + 32 - n * 32);
			}
		} else {
			run = 0;
		}
		i += 32;
	}
	None
}
