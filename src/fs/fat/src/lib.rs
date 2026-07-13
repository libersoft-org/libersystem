//! FAT - a FAT12 / FAT16 / FAT32 and exFAT backend for foreign removable
//! media (USB sticks, SD cards, install images), behind the same [`BlockDevice`] trait
//! LiberFS uses. It sits behind `Storage.Volume` as just another FS backend: per the
//! layering principle, several filesystems mount behind one volume API, and FAT is the
//! ubiquitous interchange format so reading it makes those media readable.
//!
//! Read-first by design, with a full write path. The boot sector is parsed and the
//! family auto-detected: a small cluster count is FAT12, a medium one FAT16, a large one
//! FAT32, and an `EXFAT ` magic is exFAT. A file is found by walking `/`-separated path
//! segments from the root, each lookup scanning a directory's 32-byte entries (assembling
//! VFAT long file names from their UTF-16 fragments, or the exFAT entry set - including
//! Windows' common NoFatChain contiguous form) and following the cluster chain through
//! the allocation table. All four families also create, overwrite, and delete files -
//! FAT12/16/32 allocate from the FAT and write every copy; exFAT allocates from the
//! allocation bitmap and writes its 0x85/0xC0/0xC1 entry sets, so >4 GB removable media
//! is writable. An overwrite writes the new data before the directory entry swaps and
//! frees the old chain last, so a failure part-way never costs the old file. The media
//! is untrusted: every value off the boot sector and the chains is bounded before use,
//! so a malformed volume is refused or errors cleanly instead of panicking or hanging.
//! The exFAT boot region is never rewritten: PercentInUse stays as the formatter left
//! it and the volume-dirty flags (exFAT VolumeFlags, the classic FAT[1] clean-shutdown
//! bits) stay untouched - the exFAT boot checksum excludes VolumeFlags and
//! PercentInUse, so maintaining them would cost only extra sector writes per
//! operation; the write path stays minimal and readers treat both as advisory.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

#[cfg(test)]
mod tests;

mod dir;
use dir::{Raw, build_entries, build_exfat_set, check_name, dos_datetime, exfat_free_run, exfat_mark_unlinked, exfat_set_checksum, free_run, mark_unlinked, parse_exfat_dir, parse_fat_dir, scrub_after_terminator, split_parent, writable_name};
#[cfg(test)]
use dir::{existing_shorts, short_char, trim_spaces};

// One disk sector. FAT volumes set a logical sector size in the boot sector (almost
// always 512); the device reads physical 512-byte sectors and a larger logical sector is
// read as a run of them.
pub const SECTOR_SIZE: usize = 512;

// A block device: foreign media is read and written one 512-byte sector at a time, by
// absolute LBA (its block index). The trait is the shared fs-core one (a block is
// exactly `buf.len()` bytes, so FAT's 512-byte sectors, ISO/UDF's 2048-byte blocks and
// LiberFS's 4 kB blocks all use it); FAT reads and writes, so it uses `read_block` and
// `write_block`. The read path mounts and lists; the write path creates, overwrites,
// and deletes files.
pub use fscore::BlockDevice;

// A FAT error. The variants map onto the `Storage.Volume` `error` enum at the service
// boundary (NotFound -> not-found, the rest -> invalid / again). The type is the shared
// fs-core one, so LiberFS, FAT, ISO9660 and UDF all report through one error enum; FAT
// uses the read subset plus `NoSpace`.
pub use fscore::FsError;

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

// A resolved directory: the cluster its data starts at (0 = the FAT12/16 fixed root
// region) and, for an exFAT NoFatChain directory, its valid data length - such a
// directory occupies contiguous clusters with no FAT chain at all, so every read and
// write of it must go by length, never by following the FAT.
#[derive(Clone, Copy)]
struct Dir {
	cluster: u32,
	nfc_len: Option<u64>,
	// the DataLength recorded for a CHAINED exFAT directory (None = the root or a
	// classic directory, which record none) - the read is bounded by the lesser of the
	// record and the chain, the way the media's home systems read it.
	rec_len: Option<u64>,
	// where this directory's own entry set lives (None = the root, which has no
	// record) - the exFAT grow path must update the DataLength recorded there.
	parent: Option<Parent>,
}

// The location of a directory's entry set in its parent: the parent directory's
// handle fields plus the set's byte range, so growing the directory can rewrite the
// stream extension's recorded lengths.
#[derive(Clone, Copy)]
struct Parent {
	cluster: u32,
	nfc_len: Option<u64>,
	set_off: usize,
	ent_off: usize,
}

impl Dir {
	fn at(cluster: u32) -> Dir {
		Dir { cluster, nfc_len: None, rec_len: None, parent: None }
	}
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
	// The REAL data-cluster count off the boot sector (the BPB arithmetic, or exFAT's
	// ClusterCount field) - the FAT's byte size usually has slack past it, so allocation
	// must be capped by this, never by the table's capacity alone.
	cluster_count: u32,
	// The FAT32 FSInfo sector (0 = none / not FAT32), so allocate and free can keep its
	// free-cluster count in step for other systems.
	fsinfo_sector: u32,
	// Which FAT copy is current, and whether writes mirror into every copy. FAT32's
	// ExtFlags can disable runtime mirroring, naming one active copy - the others are
	// then stale by specification, so reads must use the active one and writes must
	// leave the stale ones alone.
	active_fat: u32,
	mirror: bool,
}

// A mounted FAT volume: the device plus its geometry. Reads are on demand, so nothing is
// cached beyond the geometry; a directory or file is read by following clusters as asked.
pub struct FatFs<D: BlockDevice> {
	dev: D,
	geo: Geometry,
	// The wall clock (Unix seconds, UTC) new directory entries are stamped with; 0
	// (unset) still yields the valid DOS epoch date 1980-01-01.
	clock: u64,
}

impl<D: BlockDevice> FatFs<D> {
	// Mount foreign media: read the boot sector, detect the family, and compute the
	// geometry. None if the sector is unreadable or not a recognizable FAT volume - the
	// exFAT magic gates one path, the 0x55AA boot signature the classic BPB one, so a
	// random sector with plausible numbers does not mount.
	pub fn mount(mut dev: D) -> Option<FatFs<D>> {
		let mut boot = [0u8; SECTOR_SIZE];
		if !dev.read_block(0, &mut boot) {
			return None;
		}
		let geo = if &boot[3..11] == b"EXFAT   " {
			Geometry::exfat(&boot)?
		} else {
			if boot[510] != 0x55 || boot[511] != 0xAA {
				return None;
			}
			Geometry::bpb(&boot)?
		};
		// the geometry is the medium's own claim: the last sector it implies (the end of
		// the cluster heap, which lies past the FAT region in every family) must actually
		// exist on the device - or a forged or truncated layout mounts and only fails, or
		// allocates without bound, deep inside a later operation. The real media size
		// then bounds every downstream read and allocation.
		let ratio = (geo.bytes_per_sector / SECTOR_SIZE as u32) as u64;
		let heap_end = geo.first_data_sector as u64 + geo.cluster_count as u64 * geo.sectors_per_cluster as u64;
		let mut last = [0u8; SECTOR_SIZE];
		if !dev.read_block(heap_end * ratio - 1, &mut last) {
			return None;
		}
		Some(FatFs { dev, geo, clock: 0 })
	}

	// Set the wall clock (Unix seconds, UTC) subsequent writes stamp their directory
	// entries with, so files we create carry real timestamps on other systems.
	pub fn set_clock(&mut self, unix_secs: u64) {
		self.clock = unix_secs;
	}

	// List the volume's root directory.
	pub fn list(&mut self) -> Result<Vec<FileInfo>, FsError> {
		self.read_dir(&Dir::at(self.root_cluster()))
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

	// The data area's size in bytes - the cluster heap the boot sector declares.
	pub fn total_bytes(&self) -> u64 {
		self.geo.cluster_count as u64 * self.geo.sectors_per_cluster as u64 * self.geo.bytes_per_sector as u64
	}

	// The unallocated share of the data area in bytes: FAT12/16/32 count the zero
	// entries of the active allocation table (read once, decoded per family width),
	// exFAT the clear bits of its allocation bitmap. A fresh count per call - this
	// crate caches no allocation state, and the volumes it serves are small.
	pub fn free_bytes(&mut self) -> Result<u64, FsError> {
		let cluster_bytes: u64 = self.geo.sectors_per_cluster as u64 * self.geo.bytes_per_sector as u64;
		let max = self.max_cluster();
		let mut free: u64 = 0;
		if self.geo.kind == Kind::ExFat {
			let (bm_first, bm_size) = self.exfat_bitmap()?;
			let bm = self.read_chain(bm_first, usize::MAX)?;
			let bm_used = bm.len().min(bm_size as usize);
			for c in 2..=max {
				let idx = (c - 2) as usize;
				if idx / 8 < bm_used && bm[idx / 8] & (1 << (idx % 8)) == 0 {
					free += 1;
				}
			}
			return Ok(free * cluster_bytes);
		}
		let bps = self.geo.bytes_per_sector;
		let mut table = vec![0u8; (self.geo.fat_size * bps) as usize];
		let fat_base = self.geo.reserved_sectors + self.geo.active_fat * self.geo.fat_size;
		self.read_fs_sectors(fat_base as u64, self.geo.fat_size, &mut table)?;
		for c in 2..=max {
			let entry: u32 = match self.geo.kind {
				Kind::Fat12 => {
					let off = c as usize + c as usize / 2;
					if off + 1 >= table.len() {
						break;
					}
					let v = u16::from_le_bytes([table[off], table[off + 1]]);
					if c & 1 == 1 { (v >> 4) as u32 } else { (v & 0x0FFF) as u32 }
				}
				Kind::Fat16 => {
					let off = c as usize * 2;
					if off + 1 >= table.len() {
						break;
					}
					u16::from_le_bytes([table[off], table[off + 1]]) as u32
				}
				Kind::Fat32 | Kind::ExFat => {
					let off = c as usize * 4;
					if off + 3 >= table.len() {
						break;
					}
					u32::from_le_bytes([table[off], table[off + 1], table[off + 2], table[off + 3]]) & 0x0FFF_FFFF
				}
			};
			if entry == 0 {
				free += 1;
			}
		}
		Ok(free * cluster_bytes)
	}

	// List a subdirectory named by a `/`-separated path. An empty path is the root.
	pub fn list_dir(&mut self, path: &[u8]) -> Result<Vec<FileInfo>, FsError> {
		let dir = self.resolve_dir(path)?;
		self.read_dir(&dir)
	}

	// Read a whole file named by a `/`-separated path into a Vec.
	pub fn read_file(&mut self, path: &[u8]) -> Result<Vec<u8>, FsError> {
		let (parent, name) = split_parent(path)?;
		let dir = self.resolve_dir(parent)?;
		let entry = self.find_entry(&dir, name)?;
		if entry.is_dir {
			return Err(FsError::NotFound);
		}
		// the bytes past the ValidDataLength are undefined on disk and the media's home
		// systems serve them as zeros - a preallocated tail must never leak stale
		// cluster content (classic entries carry no VDL: theirs equals the size).
		let disk = entry.size.min(entry.valid_len) as usize;
		let mut out = if entry.no_fat_chain {
			// an exFAT NoFatChain file occupies contiguous clusters and its FAT entries
			// were never written - read it by length, not by following the FAT.
			self.read_contiguous(entry.first_cluster, disk)?
		} else {
			self.read_chain(entry.first_cluster, disk)?
		};
		if out.len() == disk && (disk as u64) < entry.size {
			// the zero tail is bounded by the volume itself, so a forged DataLength
			// cannot inflate the read past what the cluster heap could hold.
			let cluster_bytes = self.geo.sectors_per_cluster as u64 * self.geo.bytes_per_sector as u64;
			if entry.size > self.geo.cluster_count as u64 * cluster_bytes {
				return Err(FsError::Invalid);
			}
			out.resize(entry.size as usize, 0);
		}
		Ok(out)
	}

	// Create or overwrite a file named by a `/`-separated path with `data`, allocating a
	// cluster chain and writing a directory entry, for any of the four families. The new
	// data is fully on disk before the directory entry swaps over, and the old chain is
	// freed only after the swap - so a failure part-way never costs the old file.
	pub fn write_file(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
		let (parent, name) = split_parent(path)?;
		check_name(name)?;
		let dir = self.resolve_dir(parent)?;
		if self.geo.kind == Kind::ExFat {
			return self.exfat_write(&dir, name, data);
		}
		// classic FAT records a 32-bit size; a larger buffer would silently truncate.
		if data.len() > u32::MAX as usize {
			return Err(FsError::TooLong);
		}
		// 1. allocate and write the NEW chain first (no directory entry names it yet, so
		//    a failure here leaks nothing once the chain is freed on the error path).
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		let need = data.len().div_ceil(cluster_bytes);
		let chain = self.alloc_chain(need)?;
		let first = chain.first().copied().unwrap_or(0);
		if let Err(e) = self.write_clusters(&chain, data) {
			let _ = self.free_chain(first);
			return Err(e);
		}
		// 2. swap the directory entry in ONE read-modify-write: mark the old entry deleted
		//    in the in-memory copy (its slots become reusable for the new entry), place the
		//    new entry set, and write the directory back once.
		let old_first = match self.swap_entry(&dir, name, first, data.len() as u32) {
			Ok(old) => old,
			Err(e) => {
				let _ = self.free_chain(first);
				return Err(e);
			}
		};
		// 3. only now is the old chain unreachable - free it, best-effort: the write is
		//    durable at this point, so a failing device may cost lost clusters (the class
		//    the free walks already accept), never a false failure of a finished write.
		if let Some(old) = old_first {
			let _ = self.free_chain(old);
		}
		Ok(())
	}

	// Delete a file named by a `/`-separated path: free its cluster chain and clear its
	// directory entry, for any of the four families.
	pub fn remove(&mut self, path: &[u8]) -> Result<(), FsError> {
		let (parent, name) = split_parent(path)?;
		let dir = self.resolve_dir(parent)?;
		if self.geo.kind == Kind::ExFat {
			return self.exfat_remove(&dir, name);
		}
		if !self.unlink_in(&dir, name)? {
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
	// return the directory the final segment names. An empty path is the root. A `..`
	// entry pointing at the root carries first cluster 0, which on FAT32/exFAT means the
	// root cluster, not the FAT12/16 fixed region.
	fn resolve_dir(&mut self, path: &[u8]) -> Result<Dir, FsError> {
		let mut dir = Dir::at(self.root_cluster());
		for seg in path.split(|&b| b == b'/').filter(|s| !s.is_empty()) {
			let e = self.find_entry(&dir, seg)?;
			if !e.is_dir {
				return Err(FsError::NotFound);
			}
			let cluster = if e.first_cluster == 0 { self.root_cluster() } else { e.first_cluster };
			let nfc_len = if e.no_fat_chain && e.first_cluster != 0 { Some(e.size) } else { None };
			let rec_len = if self.geo.kind == Kind::ExFat && nfc_len.is_none() && cluster != self.root_cluster() { Some(e.size) } else { None };
			let parent = if cluster == self.root_cluster() { None } else { Some(Parent { cluster: dir.cluster, nfc_len: dir.nfc_len, set_off: e.set_off, ent_off: e.ent_off }) };
			dir = Dir { cluster, nfc_len, rec_len, parent };
		}
		Ok(dir)
	}

	// Find the entry named `name` (case-insensitive, ASCII; the long name or its 8.3
	// short form) in `dir`, or NotFound. Reuses the same scan the listing does.
	fn find_entry(&mut self, dir: &Dir, name: &[u8]) -> Result<Raw, FsError> {
		let entries = self.scan_dir(dir)?;
		entries.into_iter().find(|e| e.matches(name)).ok_or(FsError::NotFound)
	}

	// The listing of a directory: name + size + is_dir, dropping the "." / ".." links.
	// A directory reports a length of zero whatever its entry records (exFAT records
	// the directory's DataLength there) - the FileInfo contract, uniform across families.
	fn read_dir(&mut self, dir: &Dir) -> Result<Vec<FileInfo>, FsError> {
		let raw = self.scan_dir(dir)?;
		Ok(raw.into_iter().filter(|e| e.name != "." && e.name != "..").map(|e| FileInfo { name: e.name, size: if e.is_dir { 0 } else { e.size }, is_dir: e.is_dir }).collect())
	}

	// Read a directory's bytes (the fixed root region, a contiguous NoFatChain run, or a
	// cluster chain) and parse its entries, choosing the classic or the exFAT format.
	fn scan_dir(&mut self, dir: &Dir) -> Result<Vec<Raw>, FsError> {
		let bytes = self.read_dir_bytes(dir)?;
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
	// whole chain), following the allocation table. Returns the bytes read. The step
	// guard is the volume's real cluster count - no legitimate chain can be longer -
	// and a cluster VALUE outside the heap is corruption, never a sector address.
	fn read_chain(&mut self, first: u32, limit: usize) -> Result<Vec<u8>, FsError> {
		if limit == 0 {
			return Ok(Vec::new());
		}
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		let max = self.max_cluster();
		let mut out: Vec<u8> = Vec::new();
		let mut cluster = first;
		let mut guard = 0u32;
		while cluster >= 2 && !self.is_end(cluster) {
			if cluster > max {
				return Err(FsError::Invalid);
			}
			let sec = self.cluster_fs_sector(cluster);
			let mut buf = vec![0u8; cluster_bytes];
			self.read_fs_sectors(sec, self.geo.sectors_per_cluster, &mut buf)?;
			out.extend_from_slice(&buf);
			if out.len() >= limit {
				break;
			}
			cluster = self.next_cluster(cluster)?;
			guard += 1;
			if guard > max {
				return Err(FsError::Invalid);
			}
		}
		if limit != usize::MAX {
			out.truncate(limit);
		}
		Ok(out)
	}

	// Read `limit` bytes from contiguous clusters starting at `first` - the exFAT
	// NoFatChain form, whose FAT entries were never written. The length comes off the
	// medium, so the run is bounded against the cluster heap before a byte is read.
	fn read_contiguous(&mut self, first: u32, limit: usize) -> Result<Vec<u8>, FsError> {
		if limit == 0 {
			return Ok(Vec::new());
		}
		let count = self.nfc_run(first, limit as u64)?;
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		let mut out: Vec<u8> = Vec::new();
		for i in 0..count {
			let sec = self.cluster_fs_sector(first + i);
			let mut buf = vec![0u8; cluster_bytes];
			self.read_fs_sectors(sec, self.geo.sectors_per_cluster, &mut buf)?;
			out.extend_from_slice(&buf);
		}
		out.truncate(limit);
		Ok(out)
	}

	// Bound an exFAT NoFatChain run off untrusted media: `size` bytes as contiguous
	// clusters from `first`. The length is the medium's own claim, so a run that would
	// leave the cluster heap is refused - a forged size can neither hang the free walk,
	// grow a read allocation without bound, nor overflow the cluster arithmetic.
	fn nfc_run(&self, first: u32, size: u64) -> Result<u32, FsError> {
		let cluster_bytes = self.geo.sectors_per_cluster as u64 * self.geo.bytes_per_sector as u64;
		let count = size.div_ceil(cluster_bytes);
		let max = self.max_cluster();
		if first < 2 || first > max || count > (max - first + 1) as u64 {
			return Err(FsError::Invalid);
		}
		Ok(count as u32)
	}

	// The DOS (date, time) pair of the volume's clock, for stamping classic entries -
	// the valid epoch date 1980-01-01 when the clock is unset.
	fn dos_stamp(&self) -> (u16, u16) {
		dos_datetime(self.clock)
	}

	// The exFAT 32-bit timestamp of the volume's clock (the DOS pair packed date-high).
	fn exfat_stamp(&self) -> u32 {
		let (date, time) = dos_datetime(self.clock);
		((date as u32) << 16) | time as u32
	}

	// The first fs (logical) sector of `cluster` in the data region (clusters number
	// from 2). Callers hand it to read_fs_sectors / write_fs_sectors, which expand a
	// logical sector into its 512-byte device sectors - exactly once, there.
	fn cluster_fs_sector(&self, cluster: u32) -> u64 {
		self.geo.first_data_sector as u64 + (cluster as u64 - 2) * self.geo.sectors_per_cluster as u64
	}

	// Read `count` logical sectors starting at fs sector `sec` into `buf`, expanding each
	// logical sector to its 512-byte device sectors.
	fn read_fs_sectors(&mut self, sec: u64, count: u32, buf: &mut [u8]) -> Result<(), FsError> {
		let ratio = (self.geo.bytes_per_sector / SECTOR_SIZE as u32) as u64;
		let total = count as u64 * ratio;
		for i in 0..total {
			let off = i as usize * SECTOR_SIZE;
			let mut s = [0u8; SECTOR_SIZE];
			if !self.dev.read_block(sec * ratio + i, &mut s) {
				return Err(FsError::Io);
			}
			buf[off..off + SECTOR_SIZE].copy_from_slice(&s);
		}
		Ok(())
	}

	// The FAT entry for `cluster` - the next cluster in its chain - read from the first
	// allocation table. FAT12 packs entries in 1.5 bytes (a slot straddling a sector
	// boundary reads the sector pair), FAT16 in 2, FAT32/exFAT in 4. The index comes
	// off the medium, so an out-of-heap value is refused before it can become a table
	// offset.
	fn next_cluster(&mut self, cluster: u32) -> Result<u32, FsError> {
		if cluster < 2 || cluster > self.max_cluster() {
			return Err(FsError::Invalid);
		}
		let bps = self.geo.bytes_per_sector;
		let fat_base = self.geo.reserved_sectors + self.geo.active_fat * self.geo.fat_size;
		let byte_off = match self.geo.kind {
			Kind::Fat12 => cluster as u64 + (cluster as u64 / 2),
			Kind::Fat16 => cluster as u64 * 2,
			Kind::Fat32 | Kind::ExFat => cluster as u64 * 4,
		};
		// only a FAT12 slot can straddle a logical sector boundary (the wider slots
		// align to their width) - touch the sector pair only then.
		let sectors: u32 = if byte_off % bps as u64 == bps as u64 - 1 { 2 } else { 1 };
		let sec = fat_base as u64 + byte_off / bps as u64;
		let within = (byte_off % bps as u64) as usize;
		let mut buf = vec![0u8; (bps * sectors) as usize];
		self.read_fs_sectors(sec, sectors, &mut buf)?;
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
			if !self.dev.write_block(sec * ratio + i, &buf[off..off + SECTOR_SIZE]) {
				return Err(FsError::Io);
			}
		}
		Ok(())
	}

	// The last usable cluster index: the lesser of what the FAT table can address and
	// what the data region actually holds (clusters number from 2, so the last valid
	// index is cluster_count + 1) - the FAT's byte size usually has slack past the real
	// cluster count, and allocating from the slack would write outside the volume.
	fn max_cluster(&self) -> u32 {
		let bytes = self.geo.fat_size as u64 * self.geo.bytes_per_sector as u64;
		let entries = match self.geo.kind {
			Kind::Fat12 => bytes * 2 / 3,
			Kind::Fat16 => bytes / 2,
			Kind::Fat32 | Kind::ExFat => bytes / 4,
		};
		let cap = entries.saturating_sub(1).min(u32::MAX as u64) as u32;
		cap.min(self.geo.cluster_count.saturating_add(1))
	}

	// Write `val` into `cluster`'s FAT slot, in every FAT copy. FAT12 packs two entries
	// into three bytes (a slot straddling a sector boundary is a two-sector
	// read-modify-write; any other slot touches only its own sector); FAT16 aligns to
	// the width; FAT32 read-modify-writes too, preserving the entry's reserved top
	// nibble as the specification requires. An out-of-heap index is refused before it
	// can become a table offset - on corrupt media that offset lands in the volume's
	// own data.
	fn set_fat_entry(&mut self, cluster: u32, val: u32) -> Result<(), FsError> {
		if cluster < 2 || cluster > self.max_cluster() {
			return Err(FsError::Invalid);
		}
		let bps = self.geo.bytes_per_sector;
		let byte_off = match self.geo.kind {
			Kind::Fat12 => cluster as u64 + (cluster as u64 / 2),
			Kind::Fat16 => cluster as u64 * 2,
			Kind::Fat32 | Kind::ExFat => cluster as u64 * 4,
		};
		let sectors: u32 = if byte_off % bps as u64 == bps as u64 - 1 { 2 } else { 1 };
		// with mirroring disabled only the active copy is current - the others are
		// stale by specification and stay untouched.
		let copies = if self.geo.mirror { 0..self.geo.num_fats } else { self.geo.active_fat..self.geo.active_fat + 1 };
		for fat in copies {
			let fat_base = self.geo.reserved_sectors + fat * self.geo.fat_size;
			let sec = fat_base as u64 + byte_off / bps as u64;
			let within = (byte_off % bps as u64) as usize;
			let mut buf = vec![0u8; (bps * sectors) as usize];
			self.read_fs_sectors(sec, sectors, &mut buf)?;
			match self.geo.kind {
				Kind::Fat12 => {
					let cur = u16::from_le_bytes([buf[within], buf[within + 1]]);
					let next = if cluster & 1 == 1 { (cur & 0x000F) | ((val as u16) << 4) } else { (cur & 0xF000) | (val as u16 & 0x0FFF) };
					buf[within..within + 2].copy_from_slice(&next.to_le_bytes());
				}
				Kind::Fat16 => buf[within..within + 2].copy_from_slice(&(val as u16).to_le_bytes()),
				Kind::Fat32 | Kind::ExFat => {
					let cur = u32::from_le_bytes([buf[within], buf[within + 1], buf[within + 2], buf[within + 3]]);
					let next = (cur & 0xF000_0000) | (val & 0x0FFF_FFFF);
					buf[within..within + 4].copy_from_slice(&next.to_le_bytes());
				}
			}
			self.write_fs_sectors(sec, sectors, &buf)?;
		}
		Ok(())
	}

	// Allocate `n` free clusters into an end-terminated chain, returning them in order.
	// Zero clusters is an empty file. NoSpace if the table runs out of free entries. The
	// scan runs over ONE in-memory image of the ACTIVE FAT copy (a per-candidate device
	// read made allocation O(volume) round-trips on slow media); a failure writing a
	// link unwinds the slots already written, so nothing leaks.
	fn alloc_chain(&mut self, n: usize) -> Result<Vec<u32>, FsError> {
		if n == 0 {
			return Ok(Vec::new());
		}
		let mut fat = vec![0u8; (self.geo.fat_size as u64 * self.geo.bytes_per_sector as u64) as usize];
		let fat_base = self.geo.reserved_sectors + self.geo.active_fat * self.geo.fat_size;
		self.read_fs_sectors(fat_base as u64, self.geo.fat_size, &mut fat)?;
		let mut chain: Vec<u32> = Vec::with_capacity(n);
		let mut c = 2u32;
		let max = self.max_cluster();
		while chain.len() < n {
			if c > max {
				return Err(FsError::NoSpace);
			}
			if fat_entry_at(&fat, self.geo.kind, c) == 0 {
				chain.push(c);
			}
			c += 1;
		}
		let eoc = 0x0FFF_FFFF;
		for i in 0..chain.len() {
			let val = if i + 1 < chain.len() { chain[i + 1] } else { eoc };
			if let Err(e) = self.set_fat_entry(chain[i], val) {
				for &done in &chain[..i] {
					let _ = self.set_fat_entry(done, 0);
				}
				return Err(e);
			}
		}
		self.fsinfo_adjust(-(chain.len() as i64), chain.last().copied());
		Ok(chain)
	}

	// Write `data` over the clusters of a freshly allocated chain, zero-padding the tail.
	fn write_clusters(&mut self, chain: &[u32], data: &[u8]) -> Result<(), FsError> {
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		for (i, c) in chain.iter().enumerate() {
			let mut buf = vec![0u8; cluster_bytes];
			let off = i * cluster_bytes;
			let end = (off + cluster_bytes).min(data.len());
			if off < data.len() {
				buf[..end - off].copy_from_slice(&data[off..end]);
			}
			self.write_fs_sectors(self.cluster_fs_sector(*c), self.geo.sectors_per_cluster, &buf)?;
		}
		Ok(())
	}

	// Free a cluster chain, marking each slot free. Cluster 0 means no chain. A corrupt
	// chain (a cycle, or a next value outside the heap) stops the walk - best-effort,
	// like the step guard - and the FSInfo count reflects whatever was freed even when
	// the walk errors out part-way.
	fn free_chain(&mut self, first: u32) -> Result<(), FsError> {
		let mut freed = 0i64;
		let r = self.free_walk(first, &mut freed);
		self.fsinfo_adjust(freed, None);
		r
	}

	fn free_walk(&mut self, first: u32, freed: &mut i64) -> Result<(), FsError> {
		let max = self.max_cluster();
		let mut cluster = first;
		let mut guard = 0u32;
		while cluster >= 2 && !self.is_end(cluster) {
			if cluster > max {
				break;
			}
			let next = self.next_cluster(cluster)?;
			self.set_fat_entry(cluster, 0)?;
			*freed += 1;
			cluster = next;
			guard += 1;
			if guard > max {
				break;
			}
		}
		Ok(())
	}

	// Keep the FAT32 FSInfo sector's free-cluster count in step after an allocate (a
	// negative delta) or a free, so other systems reading media we wrote see a truthful
	// number; an allocation also leaves the "next free cluster" hint at its last
	// cluster - the spec's convention - instead of letting it go stale. Best-effort
	// advisory metadata: a missing sector, bad signatures, or the unknown sentinel
	// (0xFFFFFFFF) leave it untouched, and an I/O failure is ignored - the count is a
	// hint, never the allocation's source of truth.
	fn fsinfo_adjust(&mut self, delta: i64, hint: Option<u32>) {
		if self.geo.fsinfo_sector == 0 || delta == 0 {
			return;
		}
		let mut buf = vec![0u8; self.geo.bytes_per_sector as usize];
		if self.read_fs_sectors(self.geo.fsinfo_sector as u64, 1, &mut buf).is_err() {
			return;
		}
		let lead = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
		let sig = u32::from_le_bytes([buf[484], buf[485], buf[486], buf[487]]);
		let trail = u32::from_le_bytes([buf[508], buf[509], buf[510], buf[511]]);
		if lead != 0x4161_5252 || sig != 0x6141_7272 || trail != 0xAA55_0000 {
			return;
		}
		let free = u32::from_le_bytes([buf[488], buf[489], buf[490], buf[491]]);
		if free == 0xFFFF_FFFF {
			return;
		}
		let new = (free as i64 + delta).clamp(0, self.geo.cluster_count as i64) as u32;
		buf[488..492].copy_from_slice(&new.to_le_bytes());
		if let Some(h) = hint {
			buf[492..496].copy_from_slice(&h.to_le_bytes());
		}
		let _ = self.write_fs_sectors(self.geo.fsinfo_sector as u64, 1, &buf);
	}

	// Read a directory's raw bytes: the fixed root region for FAT12/16, a contiguous
	// NoFatChain run for an exFAT directory carrying one, else its cluster chain.
	fn read_dir_bytes(&mut self, dir: &Dir) -> Result<Vec<u8>, FsError> {
		if dir.cluster == 0 {
			return self.read_root_region();
		}
		if let Some(len) = dir.nfc_len {
			let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
			let count = self.nfc_run(dir.cluster, len)? as usize;
			return self.read_contiguous(dir.cluster, count * cluster_bytes);
		}
		if let Some(len) = dir.rec_len {
			// a chained exFAT directory is read by its recorded DataLength (rounded up
			// to whole clusters), the way the media's home systems read it - a chain
			// longer than the record must not surface extra entries.
			let cluster_bytes = self.geo.sectors_per_cluster as u64 * self.geo.bytes_per_sector as u64;
			let cap = len.div_ceil(cluster_bytes).saturating_mul(cluster_bytes).min(usize::MAX as u64) as usize;
			return self.read_chain(dir.cluster, cap);
		}
		self.read_chain(dir.cluster, usize::MAX)
	}

	// Write a directory's raw bytes back: to the fixed root region, over the contiguous
	// NoFatChain run, or along its cluster chain. The allocation bitmap goes through
	// here; directory mutations use write_dir_dirty instead.
	fn write_dir_bytes(&mut self, dir: &Dir, bytes: &[u8]) -> Result<(), FsError> {
		if dir.cluster == 0 {
			let start = self.geo.reserved_sectors + self.geo.num_fats * self.geo.fat_size;
			let sectors = (self.geo.root_entries * 32).div_ceil(self.geo.bytes_per_sector);
			self.write_fs_sectors(start as u64, sectors, bytes)?;
			return Ok(());
		}
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		if dir.nfc_len.is_some() {
			let mut off = 0usize;
			let mut i = 0u32;
			while off + cluster_bytes <= bytes.len() {
				self.write_fs_sectors(self.cluster_fs_sector(dir.cluster + i), self.geo.sectors_per_cluster, &bytes[off..off + cluster_bytes])?;
				off += cluster_bytes;
				i += 1;
			}
			return Ok(());
		}
		let mut c = dir.cluster;
		let mut off = 0usize;
		while off + cluster_bytes <= bytes.len() && c >= 2 && !self.is_end(c) {
			if c > self.max_cluster() {
				return Err(FsError::Invalid);
			}
			self.write_fs_sectors(self.cluster_fs_sector(c), self.geo.sectors_per_cluster, &bytes[off..off + cluster_bytes])?;
			off += cluster_bytes;
			c = self.next_cluster(c)?;
		}
		Ok(())
	}

	// Write back only the byte range of a directory's in-memory copy that changed
	// against `orig`, the copy it was read as (zero-extended past its length - a grown
	// tail cluster reaches the device zeroed before it is linked). Cluster granularity;
	// the fixed root region goes by sectors. A one-entry mutation must not rewrite a
	// whole big directory: that amplifies every write, and a power cut mid-rewrite
	// could tear entries unrelated to the operation.
	fn write_dir_dirty(&mut self, dir: &Dir, bytes: &[u8], orig: &[u8]) -> Result<(), FsError> {
		let at = |i: usize| orig.get(i).copied().unwrap_or(0);
		let mut lo = 0usize;
		while lo < bytes.len() && bytes[lo] == at(lo) {
			lo += 1;
		}
		if lo == bytes.len() {
			return Ok(());
		}
		let mut hi = bytes.len();
		while hi > lo && bytes[hi - 1] == at(hi - 1) {
			hi -= 1;
		}
		if dir.cluster == 0 {
			let bps = self.geo.bytes_per_sector as usize;
			let start = (self.geo.reserved_sectors + self.geo.num_fats * self.geo.fat_size) as u64;
			let (first, last) = (lo / bps, (hi - 1) / bps);
			return self.write_fs_sectors(start + first as u64, (last - first + 1) as u32, &bytes[first * bps..(last + 1) * bps]);
		}
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		let (first, last) = (lo / cluster_bytes, (hi - 1) / cluster_bytes);
		if dir.nfc_len.is_some() {
			for k in first..=last {
				let off = k * cluster_bytes;
				if off + cluster_bytes <= bytes.len() {
					self.write_fs_sectors(self.cluster_fs_sector(dir.cluster + k as u32), self.geo.sectors_per_cluster, &bytes[off..off + cluster_bytes])?;
				}
			}
			return Ok(());
		}
		let mut c = dir.cluster;
		let mut k = 0usize;
		while k <= last && c >= 2 && !self.is_end(c) {
			if c > self.max_cluster() {
				return Err(FsError::Invalid);
			}
			let off = k * cluster_bytes;
			if k >= first && off + cluster_bytes <= bytes.len() {
				self.write_fs_sectors(self.cluster_fs_sector(c), self.geo.sectors_per_cluster, &bytes[off..off + cluster_bytes])?;
			}
			c = self.next_cluster(c)?;
			k += 1;
		}
		Ok(())
	}

	// Remove the entry named `name` (its long name or 8.3 short form) from `dir`: mark
	// its 8.3 record plus any long fragments deleted and release its chain. Returns
	// whether the name was present.
	fn unlink_in(&mut self, dir: &Dir, name: &[u8]) -> Result<bool, FsError> {
		let mut bytes = self.read_dir_bytes(dir)?;
		let orig = bytes.clone();
		match mark_unlinked(&mut bytes, name)? {
			None => Ok(false),
			Some(e) => {
				self.write_dir_dirty(dir, &bytes, &orig)?;
				// the unlink is durable once the directory write lands - the free is
				// best-effort (a failing device costs lost clusters, never a false
				// failure of a finished remove).
				let _ = self.free_chain(e.first_cluster);
				Ok(true)
			}
		}
	}

	// Swap the directory entry for `name` in ONE read-modify-write: mark any old entry
	// deleted in the in-memory copy (its slots become reusable), place the new entry set
	// (a unique 8.3 short + long fragments when needed, growing a chained directory by
	// whole clusters until the set fits), and write the directory back once. An
	// overwrite preserves what the media's home systems preserve: the replaced entry's
	// on-disk name (a match through the 8.3 alias must not rename the file) and its
	// creation stamp. Returns the replaced entry's first cluster, which only then is
	// safe to free.
	fn swap_entry(&mut self, dir: &Dir, name: &[u8], first: u32, size: u32) -> Result<Option<u32>, FsError> {
		let mut bytes = self.read_dir_bytes(dir)?;
		let orig = bytes.clone();
		scrub_after_terminator(&mut bytes);
		let old = mark_unlinked(&mut bytes, name)?;
		let name: &[u8] = match &old {
			Some(o) if writable_name(o.name.as_bytes()) => o.name.as_bytes(),
			_ => name,
		};
		let mut entries = build_entries(name, &bytes, first, size, 0x20, self.dos_stamp())?;
		if let Some(o) = &old {
			// the creation stamp (tenths + time + date) carries over from the replaced
			// entry - only the byte 0 of its records was marked, the fields are intact.
			let last = entries.len() - 1;
			let stamp: [u8; 5] = bytes[o.ent_off + 13..o.ent_off + 18].try_into().unwrap();
			entries[last][13..18].copy_from_slice(&stamp);
		}
		let at = loop {
			if let Some(p) = free_run(&bytes, entries.len()) {
				break p;
			}
			// the fixed FAT12/16 root region cannot grow, and an exFAT NoFatChain
			// directory has no chain to extend.
			if dir.cluster == 0 || dir.nfc_len.is_some() {
				return Err(FsError::NoSpace);
			}
			self.grow_dir(dir.cluster, &mut bytes)?;
		};
		for (k, e) in entries.iter().enumerate() {
			bytes[at + k * 32..at + k * 32 + 32].copy_from_slice(e);
		}
		self.write_dir_dirty(dir, &bytes, &orig)?;
		Ok(old.map(|o| o.first_cluster))
	}

	// Grow a chained directory by one zeroed cluster: allocate it, zero it on the device
	// (BEFORE linking - once linked, stale content would parse as directory entries if a
	// later write fails), link it at the end of the chain, and extend the in-memory copy
	// to match. A failure part-way frees the fresh cluster, so nothing leaks.
	fn grow_dir(&mut self, cluster: u32, bytes: &mut Vec<u8>) -> Result<(), FsError> {
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		let grow = self.alloc_chain(1)?[0];
		let linked = self.write_fs_sectors(self.cluster_fs_sector(grow), self.geo.sectors_per_cluster, &vec![0u8; cluster_bytes]).and_then(|()| self.last_cluster(cluster)).and_then(|last| self.set_fat_entry(last, grow));
		if let Err(e) = linked {
			let _ = self.free_chain(grow);
			return Err(e);
		}
		let p = bytes.len();
		bytes.resize(p + cluster_bytes, 0);
		Ok(())
	}

	// The last cluster of a chain, for appending: walk to the end-of-chain marker. A
	// chain that hits a free/reserved entry (< 2), leaves the heap, or runs past the
	// cluster count (a cycle on corrupt media) is refused - never walked into FAT[0],
	// out of the volume, or forever.
	fn last_cluster(&mut self, first: u32) -> Result<u32, FsError> {
		let max = self.max_cluster();
		let mut c = first;
		let mut guard = 0u32;
		loop {
			let next = self.next_cluster(c)?;
			if self.is_end(next) {
				return Ok(c);
			}
			if next < 2 || next > max {
				return Err(FsError::Invalid);
			}
			c = next;
			guard += 1;
			if guard > max {
				return Err(FsError::Invalid);
			}
		}
	}

	// Create or overwrite an exFAT file: allocate the data clusters from the allocation
	// bitmap and write them first, then swap the 0x85 / 0xC0 / 0xC1 entry set in one
	// directory write, and only then release the replaced file's clusters - a failure
	// part-way never costs the old file.
	fn exfat_write(&mut self, dir: &Dir, name: &[u8], data: &[u8]) -> Result<(), FsError> {
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		let need = data.len().div_ceil(cluster_bytes);
		let chain = self.exfat_alloc(need)?;
		let first = chain.first().copied().unwrap_or(0);
		if let Err(e) = self.write_clusters(&chain, data) {
			let _ = self.exfat_free(first);
			return Err(e);
		}
		let old = match self.exfat_swap_entry(dir, name, first, data.len() as u64) {
			Ok(old) => old,
			Err(e) => {
				let _ = self.exfat_free(first);
				return Err(e);
			}
		};
		// the write is durable once the entry set lands - the release of the replaced
		// clusters is best-effort, like the classic path's.
		if let Some(old) = old {
			let _ = self.exfat_release(&old);
		}
		Ok(())
	}

	// Delete an exFAT file: clear its entry set's in-use bits and release its clusters.
	fn exfat_remove(&mut self, dir: &Dir, name: &[u8]) -> Result<(), FsError> {
		let mut bytes = self.read_dir_bytes(dir)?;
		let orig = bytes.clone();
		let Some(old) = exfat_mark_unlinked(&mut bytes, name)? else {
			return Err(FsError::NotFound);
		};
		self.write_dir_dirty(dir, &bytes, &orig)?;
		// durable once the directory write lands - the release is best-effort.
		let _ = self.exfat_release(&old);
		Ok(())
	}

	// Swap an exFAT entry set in ONE read-modify-write: mark any old set's in-use bits
	// cleared (its slots become reusable), place the new set (growing a chained
	// directory by whole clusters until the set fits), write the directory back once. An
	// overwrite preserves the replaced set's on-disk name and creation stamp, as the
	// media's home systems do. Returns the replaced entry, whose clusters only then
	// are safe to release.
	fn exfat_swap_entry(&mut self, dir: &Dir, name: &[u8], first: u32, size: u64) -> Result<Option<Raw>, FsError> {
		let mut bytes = self.read_dir_bytes(dir)?;
		let orig = bytes.clone();
		scrub_after_terminator(&mut bytes);
		let old = exfat_mark_unlinked(&mut bytes, name)?;
		let name: &[u8] = match &old {
			Some(o) if writable_name(o.name.as_bytes()) => o.name.as_bytes(),
			_ => name,
		};
		let mut set = build_exfat_set(name, first, size, self.exfat_stamp());
		if let Some(o) = &old {
			// the creation stamp (timestamp + 10ms increment + UTC marker) carries over
			// from the replaced set; the checksum is restamped over the final bytes.
			let stamp: [u8; 4] = bytes[o.set_off + 8..o.set_off + 12].try_into().unwrap();
			set[8..12].copy_from_slice(&stamp);
			set[20] = bytes[o.set_off + 20];
			set[22] = bytes[o.set_off + 22];
			let sum = exfat_set_checksum(&set);
			set[2..4].copy_from_slice(&sum.to_le_bytes());
		}
		let at = loop {
			if let Some(p) = exfat_free_run(&bytes, set.len() / 32) {
				break p;
			}
			// a NoFatChain directory occupies contiguous clusters - it cannot extend
			// without relocation, so it refuses instead.
			if dir.nfc_len.is_some() {
				return Err(FsError::NoSpace);
			}
			self.exfat_grow_dir(dir, &mut bytes)?;
		};
		bytes[at..at + set.len()].copy_from_slice(&set);
		self.write_dir_dirty(dir, &bytes, &orig)?;
		Ok(old)
	}

	// Grow a chained exFAT directory by one zeroed cluster: allocate it from the
	// bitmap, zero it on the device (BEFORE linking, like the classic grow), link it at
	// the end of the FAT chain, extend the in-memory copy, and grow the DataLength /
	// ValidDataLength recorded in the directory's own entry set in its parent (the root
	// has no record - its extent is the FAT chain alone). A failure part-way frees the
	// fresh cluster, so nothing leaks.
	fn exfat_grow_dir(&mut self, dir: &Dir, bytes: &mut Vec<u8>) -> Result<(), FsError> {
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		let grow = self.exfat_alloc(1)?[0];
		let linked = self.write_fs_sectors(self.cluster_fs_sector(grow), self.geo.sectors_per_cluster, &vec![0u8; cluster_bytes]).and_then(|()| self.last_cluster(dir.cluster)).and_then(|last| self.set_fat_entry(last, grow));
		if let Err(e) = linked {
			let _ = self.exfat_free(grow);
			return Err(e);
		}
		bytes.resize(bytes.len() + cluster_bytes, 0);
		if let Some(p) = dir.parent {
			self.exfat_grow_parent_record(&p, cluster_bytes as u64)?;
		}
		Ok(())
	}

	// Add `delta` bytes to the DataLength and ValidDataLength of the stream extension
	// inside the entry set at `p`, restamp the set checksum, and write the parent
	// directory back - the bookkeeping half of growing an exFAT directory.
	fn exfat_grow_parent_record(&mut self, p: &Parent, delta: u64) -> Result<(), FsError> {
		let pdir = Dir { cluster: p.cluster, nfc_len: p.nfc_len, rec_len: None, parent: None };
		let mut bytes = self.read_dir_bytes(&pdir)?;
		let orig = bytes.clone();
		let end = p.ent_off + 32;
		if p.set_off >= p.ent_off || end > bytes.len() {
			return Err(FsError::Invalid);
		}
		let mut s = p.set_off + 32;
		while s + 32 <= end {
			if bytes[s] == 0xC0 {
				for field in [s + 8, s + 24] {
					let len = u64::from_le_bytes(bytes[field..field + 8].try_into().unwrap()).saturating_add(delta);
					bytes[field..field + 8].copy_from_slice(&len.to_le_bytes());
				}
				break;
			}
			s += 32;
		}
		let sum = exfat_set_checksum(&bytes[p.set_off..end]);
		bytes[p.set_off + 2..p.set_off + 4].copy_from_slice(&sum.to_le_bytes());
		self.write_dir_dirty(&pdir, &bytes, &orig)
	}

	// Release a replaced or removed exFAT file's clusters: a NoFatChain file (Windows'
	// common contiguous form, whose FAT entries were never written) frees its contiguous
	// run from the bitmap alone; a chained file walks and clears the FAT too.
	fn exfat_release(&mut self, old: &Raw) -> Result<(), FsError> {
		if old.no_fat_chain { self.exfat_free_contiguous(old.first_cluster, old.size) } else { self.exfat_free(old.first_cluster) }
	}

	// Locate the allocation bitmap (the 0x81 entry in the root): its first cluster and its
	// byte length. exFAT tracks free clusters as a bit per cluster, set when allocated.
	fn exfat_bitmap(&mut self) -> Result<(u32, u64), FsError> {
		let bytes = self.read_dir_bytes(&Dir::at(self.geo.root_cluster))?;
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
	// order. The FAT entries are written before the bitmap (a failure part-way unwinds
	// the written slots and leaves the bitmap untouched, so nothing leaks); NoSpace if
	// the volume is full.
	fn exfat_alloc(&mut self, n: usize) -> Result<Vec<u32>, FsError> {
		if n == 0 {
			return Ok(Vec::new());
		}
		let (bm_first, bm_size) = self.exfat_bitmap()?;
		let bm_dir = Dir::at(bm_first);
		let mut bm = self.read_chain(bm_first, usize::MAX)?;
		// the bitmap's declared byte length bounds the bits we may interpret; the buffer
		// keeps its cluster granularity for the write-back.
		let bm_used = bm.len().min(bm_size as usize);
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
			if byte < bm_used && bm[byte] & (1 << bit) == 0 {
				bm[byte] |= 1 << bit;
				chain.push(c);
			}
			c += 1;
		}
		let eoc = 0x0FFF_FFFF;
		for i in 0..chain.len() {
			let val = if i + 1 < chain.len() { chain[i + 1] } else { eoc };
			if let Err(e) = self.set_fat_entry(chain[i], val) {
				for &done in &chain[..i] {
					let _ = self.set_fat_entry(done, 0);
				}
				return Err(e);
			}
		}
		self.write_dir_bytes(&bm_dir, &bm)?;
		Ok(chain)
	}

	// Free an exFAT chain: clear each cluster's bitmap bit and FAT slot. First 0 = none.
	// A corrupt chain (a cycle or an out-of-heap next) stops the walk, best-effort.
	fn exfat_free(&mut self, first: u32) -> Result<(), FsError> {
		if first < 2 {
			return Ok(());
		}
		let (bm_first, bm_size) = self.exfat_bitmap()?;
		let bm_dir = Dir::at(bm_first);
		let mut bm = self.read_chain(bm_first, usize::MAX)?;
		let bm_used = bm.len().min(bm_size as usize);
		let max = self.max_cluster();
		let mut cluster = first;
		let mut guard = 0u32;
		while cluster >= 2 && !self.is_end(cluster) {
			if cluster > max {
				break;
			}
			let next = self.next_cluster(cluster)?;
			let idx = (cluster - 2) as usize;
			let byte = idx / 8;
			if byte < bm_used {
				bm[byte] &= !(1 << (idx % 8));
			}
			self.set_fat_entry(cluster, 0)?;
			cluster = next;
			guard += 1;
			if guard > max {
				break;
			}
		}
		self.write_dir_bytes(&bm_dir, &bm)
	}

	// Free a NoFatChain file's contiguous cluster run: clear its bitmap bits. The FAT
	// holds nothing for such a file, so there is nothing to walk or clear there.
	fn exfat_free_contiguous(&mut self, first: u32, size: u64) -> Result<(), FsError> {
		if first < 2 || size == 0 {
			return Ok(());
		}
		let count = self.nfc_run(first, size)?;
		let (bm_first, bm_size) = self.exfat_bitmap()?;
		let bm_dir = Dir::at(bm_first);
		let mut bm = self.read_chain(bm_first, usize::MAX)?;
		let bm_used = bm.len().min(bm_size as usize);
		for i in 0..count {
			let idx = (first + i - 2) as usize;
			let byte = idx / 8;
			if byte < bm_used {
				bm[byte] &= !(1 << (idx % 8));
			}
		}
		self.write_dir_bytes(&bm_dir, &bm)
	}
}

// Read `cluster`'s entry from an in-memory image of the FAT, for the allocation scan
// (an out-of-image offset reads as non-free, so it is never handed out).
fn fat_entry_at(fat: &[u8], kind: Kind, cluster: u32) -> u32 {
	let off = match kind {
		Kind::Fat12 => cluster as usize + cluster as usize / 2,
		Kind::Fat16 => cluster as usize * 2,
		Kind::Fat32 | Kind::ExFat => cluster as usize * 4,
	};
	match kind {
		Kind::Fat12 => {
			if off + 2 > fat.len() {
				return 1;
			}
			let v = u16::from_le_bytes([fat[off], fat[off + 1]]);
			if cluster & 1 == 1 { (v >> 4) as u32 } else { (v & 0x0FFF) as u32 }
		}
		Kind::Fat16 => {
			if off + 2 > fat.len() {
				return 1;
			}
			u16::from_le_bytes([fat[off], fat[off + 1]]) as u32
		}
		Kind::Fat32 | Kind::ExFat => {
			if off + 4 > fat.len() {
				return 1;
			}
			u32::from_le_bytes([fat[off], fat[off + 1], fat[off + 2], fat[off + 3]]) & 0x0FFF_FFFF
		}
	}
}
impl Geometry {
	// Parse a FAT12/16/32 BIOS Parameter Block and classify by cluster count. Every
	// value comes off untrusted removable media, so the region arithmetic runs in u64
	// and a layout whose regions exceed the sector count is refused, never underflowed.
	fn bpb(b: &[u8]) -> Option<Geometry> {
		let bytes_per_sector = u16::from_le_bytes([b[11], b[12]]) as u32;
		let sectors_per_cluster = b[13] as u32;
		// the specification allows only 512 / 1024 / 2048 / 4096 byte logical sectors,
		// and a cluster of a power of two up to 128 sectors.
		if !(512..=4096).contains(&bytes_per_sector) || !bytes_per_sector.is_power_of_two() || !sectors_per_cluster.is_power_of_two() || sectors_per_cluster > 128 {
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
		// a zero reserved count would put the FAT region at the boot sector, so the
		// first FAT write would overwrite it - refuse the layout at mount. A FAT count
		// above 2 is spec-tolerated (though no formatter emits one) and stays accepted:
		// the region arithmetic below and the mount probe bound it like any layout.
		if num_fats == 0 || fat_size == 0 || total == 0 || reserved_sectors == 0 {
			return None;
		}
		let root_sectors = (root_entries as u64 * 32).div_ceil(bytes_per_sector as u64);
		let first_data = reserved_sectors as u64 + num_fats as u64 * fat_size as u64 + root_sectors;
		if first_data >= total as u64 || first_data > u32::MAX as u64 {
			return None;
		}
		let first_data_sector = first_data as u32;
		let clusters = ((total as u64 - first_data) / sectors_per_cluster as u64) as u32;
		// a volume with no data clusters is degenerate - refuse it, as the exFAT path
		// does - and a count past the spec ceiling would make the BAD-cluster marker a
		// "valid" cluster index the chain walks would follow as data.
		if clusters == 0 || clusters > 0x0FFF_FFF3 {
			return None;
		}
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
		// a classic volume with no root region is degenerate (nothing could ever live in
		// its root); the FAT32 shape rule above already claimed the legitimate zero.
		if kind != Kind::Fat32 && root_entries == 0 {
			return None;
		}
		let root_cluster = if kind == Kind::Fat32 { u32::from_le_bytes([b[44], b[45], b[46], b[47]]) } else { 0 };
		// a FAT32 root outside the heap is degenerate (0 would even read the nonexistent
		// fixed root region) - refuse it at mount.
		if kind == Kind::Fat32 && (root_cluster < 2 || root_cluster as u64 > clusters as u64 + 1) {
			return None;
		}
		// FAT32's ExtFlags: bit 7 disables runtime mirroring and bits 0-3 then name the
		// only current FAT copy - the others are stale by specification, so reading copy
		// 0 there would follow wrong chains and cross-link real data on allocation.
		let ext_flags = if kind == Kind::Fat32 { u16::from_le_bytes([b[40], b[41]]) as u32 } else { 0 };
		let (mirror, active_fat) = if ext_flags & 0x80 != 0 { (false, ext_flags & 0x0F) } else { (true, 0) };
		if active_fat >= num_fats {
			return None;
		}
		let fsinfo = if kind == Kind::Fat32 { u16::from_le_bytes([b[48], b[49]]) as u32 } else { 0 };
		let fsinfo_sector = if fsinfo != 0 && fsinfo < reserved_sectors { fsinfo } else { 0 };
		Some(Geometry { kind, bytes_per_sector, sectors_per_cluster, reserved_sectors, num_fats, fat_size, root_entries, root_cluster, first_data_sector, cluster_count: clusters, fsinfo_sector, active_fat, mirror })
	}

	// Parse an exFAT boot sector. exFAT keeps everything in the cluster heap, so the root
	// region is a chain like any directory and root_entries is unused. The two size
	// fields are shift exponents off untrusted media: they are bounded BEFORE shifting
	// (the spec's 512-4096 byte sectors and a 32 MB cluster ceiling), so a forged
	// exponent can neither panic a debug build nor wrap into a plausible geometry.
	fn exfat(b: &[u8]) -> Option<Geometry> {
		let bps_shift = b[108];
		let spc_shift = b[109];
		if !(9..=12).contains(&bps_shift) || spc_shift > 25 - bps_shift {
			return None;
		}
		let fat_offset = u32::from_le_bytes([b[80], b[81], b[82], b[83]]);
		let fat_size = u32::from_le_bytes([b[84], b[85], b[86], b[87]]);
		let cluster_heap_offset = u32::from_le_bytes([b[88], b[89], b[90], b[91]]);
		let cluster_count = u32::from_le_bytes([b[92], b[93], b[94], b[95]]);
		let root_cluster = u32::from_le_bytes([b[96], b[97], b[98], b[99]]);
		let bytes_per_sector = 1u32 << bps_shift;
		let sectors_per_cluster = 1u32 << spc_shift;
		let num_fats = b[110] as u32;
		// degenerate pointers are refused at mount: a zero FAT size or offset would send
		// the FAT walks into the boot region (bpb refuses both already), a root outside
		// the heap cannot be a directory, a FAT region overlapping the cluster heap
		// would make a FAT-slot write clobber file data, and a cluster count past the
		// spec ceiling would make the BAD-cluster marker a "valid" cluster index.
		if num_fats == 0 || fat_offset == 0 || fat_size == 0 || cluster_heap_offset < 2 || cluster_count == 0 || cluster_count > 0x0FFF_FFF3 || root_cluster < 2 || root_cluster as u64 > cluster_count as u64 + 1 {
			return None;
		}
		if fat_offset as u64 + num_fats as u64 * fat_size as u64 > cluster_heap_offset as u64 {
			return None;
		}
		// TexFAT's second-FAT selection (VolumeFlags bit 0 = the second FAT is active)
		// is out of scope - refuse rather than read the wrong table.
		if u16::from_le_bytes([b[106], b[107]]) & 0x01 != 0 {
			return None;
		}
		Some(Geometry { kind: Kind::ExFat, bytes_per_sector, sectors_per_cluster, reserved_sectors: fat_offset, num_fats: 1, fat_size, root_entries: 0, root_cluster, first_data_sector: cluster_heap_offset, cluster_count, fsinfo_sector: 0, active_fat: 0, mirror: true })
	}
}
