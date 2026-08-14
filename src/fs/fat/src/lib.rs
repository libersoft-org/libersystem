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
//! The exFAT boot region IS rewritten, in one place and for one reason: `set_volume_dirty` brackets
//! every metadata transaction with the VolumeDirty flag and stamps PercentInUse to 0xFF - "unknown"
//! - because this driver does not maintain the figure and a stale one is worse than none. Nothing
//! else in the boot region is touched.
//!
//! (This header said the region was never rewritten and that the volume-dirty flags stayed
//! untouched, a thousand lines above the code that writes them. In a filesystem driver the header
//! is where somebody learns what the code promises about the medium, which makes a stale one worth
//! more than an ordinary stale comment.)
//!
//! The classic FAT[1] clean-shutdown bits are still untouched, and readers treat them as advisory.

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

// What one directory read may allocate. The exFAT specification bounds a directory at 256 MiB, so a
// chain past this is a volume outside its own format rather than a large one - which matters
// because `read_dir_bytes` used `usize::MAX` for any directory that records no length, and every
// exFAT root records none.
pub const MAX_DIR_BYTES: usize = 256 * 1024 * 1024;

// What `read_file` may allocate when the caller does not say. `read_file_bounded` takes the
// caller's own ceiling; this is the default for the callers that have no opinion, and it is
// deliberately generous - the point is that a number a hostile volume writes cannot name an
// unbounded allocation, not that files must be small.
pub const MAX_FILE_BYTES: usize = 256 * 1024 * 1024;

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
// exFAT's only end-of-chain value. Unlike the classic families it has no reserved top nibble, so a
// terminator is all thirty-two bits set and nothing less.
const EXFAT_EOC: u32 = 0xFFFF_FFFF;

// A walk along one cluster chain, with the four things every walk in this file has to get right:
// the cluster is inside the volume, it is not an end marker, a `NoFatChain` file advances by
// arithmetic and a chained one by its links, and a chain that loops back on itself is caught.
//
// A TYPE RATHER THAN A PATTERN. `read_file`, `read_file_range`, `read_chain`, `read_chain_to_end`,
// the free path and the grow path each re-implemented all four, and the sixth author to write it
// forgot one: `read_file_range`'s Floyd detection bounded the SKIP to the offset and its read loop
// had none, so a self-loop read at offset 0 handed the caller the same cluster twice and called it
// a file. The comment above it said the walk was "bounded by the same Floyd cycle detection every
// other walk in this file uses", which was true of half of it.
struct ChainCursor {
	cluster: u32,
	// Floyd's slow pointer: one link for every two the walk takes, so a cycle is caught in the
	// length of the cycle rather than by a step budget - which cannot tell repetition from length.
	slow: u32,
	steps: u64,
	no_fat_chain: bool,
	max: u32,
}

impl ChainCursor {
	fn new(first: u32, no_fat_chain: bool, max: u32) -> Self {
		Self { cluster: first, slow: first, steps: 0, no_fat_chain, max }
	}

	// The cluster this cursor is on, or why it is not one: outside the volume, below the first data
	// cluster, or the end of the chain.
	fn current<D: BlockDevice>(&self, fs: &FatFs<D>) -> Result<u32, FsError> {
		if self.cluster < 2 || self.cluster > self.max || fs.is_end(self.cluster) {
			return Err(FsError::Corrupt);
		}
		Ok(self.cluster)
	}

	// Step to the next cluster. `Invalid` for a chain that loops back on itself.
	fn advance<D: BlockDevice>(&mut self, fs: &mut FatFs<D>) -> Result<(), FsError> {
		if self.no_fat_chain {
			self.cluster = self.cluster.checked_add(1).ok_or(FsError::Invalid)?;
			return Ok(());
		}
		self.cluster = fs.next_cluster(self.cluster)?;
		self.steps += 1;
		if self.steps % 2 == 0 {
			self.slow = fs.next_cluster(self.slow)?;
		}
		if self.cluster >= 2 && self.cluster == self.slow {
			return Err(FsError::Invalid);
		}
		if self.steps > self.max as u64 {
			return Err(FsError::Invalid);
		}
		Ok(())
	}
}

pub struct FatFs<D: BlockDevice> {
	dev: D,
	geo: Geometry,
	// The wall clock (Unix seconds, UTC) new directory entries are stamped with; 0
	// (unset) still yields the valid DOS epoch date 1980-01-01.
	clock: u64,
	// Set when a rollback could not be written, which means the FAT on the medium no longer
	// describes its own contents: mirrored copies that disagree, an entry torn across two sectors,
	// or clusters marked in use with nothing naming them. Every later mutation is refused with
	// `ReadOnly` rather than layered on top of a table that cannot be trusted.
	//
	// The alternative is to carry on and hope the next write repairs it, which is how a torn FAT12
	// entry becomes two files sharing a cluster. A mount that stops mutating keeps the damage at
	// whatever the failed write left, where a repair tool can still see it.
	degraded: bool,
	// The volume's case-folding rule. exFAT keeps it on the medium as the Up-case Table and every
	// implementation must use the one it finds; the classic families fold ASCII by rule, so they
	// get a table that says exactly that.
	upcase: dir::Upcase,
	// What the mount's scan of the exFAT root established: where the Allocation Bitmap is. None on
	// the classic families, which have no root to scan.
	exfat_root: Option<ExfatRoot>,
}

// Why a directory swap failed, and whether the new entry may already be on the medium.
//
// `placed` is the only thing the caller needs to decide what it may free: before the new entry set
// is written, nothing is published and the new chain is the caller's to release; after it, the
// commit is ambiguous and freeing would hand out clusters a live entry names.
//
// AND IT DECIDES THE ERROR, which it did not. The two call sites answered `Err(error)` with whatever
// the device reported - `Io` - and the comment beside them said "the commit is ambiguous" in as many
// words. `Io` reaches a caller through StorageService as `Again`, which tells it to do the one thing
// that is unsafe here: repeat a create whose first attempt may already be on the medium.
//
// `FsError::CommitUncertain` exists for exactly this and says so at its definition: "a caller told
// `Io` reasonably retries; a caller told this one must not". StorageService already maps it to a
// refusal, LiberFS already raises it, and this backend - which has the clearest case for it in the
// tree - had zero occurrences of it.
//
// So the error is derived from `placed` rather than passed alongside it, in one place, and the mount
// goes degraded with it: `ensure_writable` consults that flag, so the refusal sticks for every later
// mutation rather than only for the call that discovered it.
// What one scan of the exFAT root established, so nothing re-derives it per operation.
#[derive(Clone, Copy)]
struct ExfatRoot {
	bitmap_first: u32,
	bitmap_size: u64,
}

struct SwapFailure {
	error: FsError,
	placed: bool,
}

impl SwapFailure {
	// What the caller is told. An ambiguous commit is `CommitUncertain` whatever the device said,
	// because what the device said is not the useful fact - what may be on the medium is.
	fn reported(&self) -> FsError {
		if self.placed { FsError::CommitUncertain } else { self.error }
	}
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
			let geo = Geometry::exfat(&boot)?;
			// The specification requires the boot checksum to be confirmed before ANY field of the
			// boot region is used, and this driver used all of them without ever computing it. The
			// checksum covers the eleven sectors from the main boot sector to the OEM parameters,
			// skipping the three bytes the running system rewrites (VolumeFlags and PercentInUse),
			// and sector 11 holds nothing but that value repeated.
			//
			// It is the only check that catches a boot region edited in place - a plausible
			// geometry with one field changed passes every bound but not the sum.
			if !exfat_boot_checksum_ok(&mut dev, geo.bytes_per_sector) {
				return None;
			}
			geo
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
		let mut fs = FatFs { dev, geo, clock: 0, degraded: false, upcase: dir::Upcase::ascii(), exfat_root: None };
		// An exFAT volume without a readable Up-case Table is refused rather than mounted with a
		// guess: every name decision on it - lookup, collision, the hash written into an entry set -
		// would be this driver's opinion instead of the volume's, and the damage is silent.
		if fs.geo.kind == Kind::ExFat {
			let (root, upcase) = fs.scan_exfat_root().ok()?;
			fs.exfat_root = Some(root);
			fs.upcase = upcase;
			// VolumeDirty already set means the last writer never finished: the metadata may be
			// mid-transaction. Mounting it read-only is what the flag is FOR - writing over a
			// volume in that state is how a recoverable inconsistency becomes an unrecoverable one.
			// A repair tool clears the flag; this driver does not.
			if u16::from_le_bytes([boot[106], boot[107]]) & 0x02 != 0 {
				fs.degraded = true;
			}
		}
		Some(fs)
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
			let bm = self.read_chain(bm_first, self.bitmap_cap(bm_size))?;
			let bm_used = bm.len().min(bm_size as usize);
			for c in 2..=max {
				let idx = (c - 2) as usize;
				if idx / 8 < bm_used && bm[idx / 8] & (1 << (idx % 8)) == 0 {
					free += 1;
				}
			}
			return Ok(free * cluster_bytes);
		}
		let bps = self.geo.bytes_per_sector as usize;
		let mut window = vec![0u8; 2 * bps];
		let mut loaded: Option<u32> = None;
		for c in 2..=max {
			let within = self.fat_window(&mut window, &mut loaded, c)?;
			if fat_entry_in(&window, within, self.geo.kind, c) == 0 {
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
		self.read_file_bounded(path, MAX_FILE_BYTES)
	}

	// The same read with the caller's OWN ceiling, which is what a caller needs to be able to say.
	//
	// `read_file` returned the whole file as a `Vec` sized from a length the medium supplies, and
	// the caller had no way to state how much it was willing to take - so "what is bounded now is
	// everything sized by a number a hostile volume writes" was true of the FAT and not of the
	// files. A file past the ceiling is `TooLarge`, which `fs-core` documents as "the answer does
	// not fit in one buffer; a ranged read is the solution".
	// Read `buffer.len()` bytes of a file from `offset`, answering how many were copied.
	//
	// `fs-core` documents `TooLarge` as the error whose answer is a RANGED READ, and this crate had
	// none - so a file past `MAX_FILE_BYTES` on a FAT or exFAT volume was unreadable by any means it
	// offered, which for removable media is a size an ordinary disc carries.
	//
	// One cluster of working memory whatever the file's size: the chain is walked to the offset
	// without buffering what it skips, which is also the primitive the directory ceiling wanted
	// underneath it.
	pub fn read_file_range(&mut self, path: &[u8], offset: u64, buffer: &mut [u8]) -> Result<usize, FsError> {
		let (parent, name) = split_parent(path)?;
		let dir = self.resolve_dir(parent)?;
		let entry = self.find_entry(&dir, name)?;
		if entry.is_dir {
			return Err(FsError::IsDir);
		}
		// Past the ValidDataLength the bytes are undefined on disk and the media's home systems
		// serve zeros, so a preallocated tail reads as zeros here too rather than as stale clusters.
		let readable = entry.size.min(entry.valid_len);
		if offset >= entry.size || buffer.is_empty() {
			return Ok(0);
		}
		let want = ((entry.size - offset) as usize).min(buffer.len());
		if offset >= readable {
			buffer[..want].fill(0);
			return Ok(want);
		}
		// A read that CROSSES the ValidDataLength gets its tail zeroed here rather than being cut
		// short. It used to be truncated to the VDL, so a caller reading across it got fewer bytes
		// than it asked for inside a file it had not reached the end of, and had to call again to be
		// told zeros - while `read_file_bounded`, reading the same volume, zero-filled in one answer.
		// Two readers of one volume disagreeing about what is past the VDL is the defect; which of
		// the two behaviours is right is a separate and easier question.
		let from_disk = (readable - offset) as usize;
		if want > from_disk {
			buffer[from_disk..want].fill(0);
		}
		// `answer` is what the caller is told - the disk bytes plus the zeroed tail - and `want` is
		// how far the read loop below goes.
		let answer = want;
		let want = want.min(from_disk);
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as u64;
		let max = self.max_cluster();
		// ONE CURSOR FOR THE SKIP AND THE READ. The skip walked with Floyd detection and the read
		// loop had none, so a chain that loops back on itself was refused only when the caller asked
		// for an offset far enough in to walk it - and at offset 0 the skip does not run at all, so
		// a self-loop was read twice and reported as two clusters of file.
		let skip = offset / cluster_bytes;
		let mut chain = ChainCursor::new(entry.first_cluster, entry.no_fat_chain, max);
		if entry.no_fat_chain {
			// Contiguous: the skip is arithmetic, and one `checked_add` says so better than `skip`
			// steps that cannot fail differently.
			chain.cluster = chain.cluster.checked_add(u32::try_from(skip).map_err(|_| FsError::Invalid)?).ok_or(FsError::Invalid)?;
		} else {
			for _ in 0..skip {
				chain.current(self)?;
				chain.advance(self)?;
			}
		}
		let mut done = 0usize;
		let mut scratch = vec![0u8; cluster_bytes as usize];
		let mut within = (offset % cluster_bytes) as usize;
		while done < want {
			let cluster = chain.current(self)?;
			let sec = self.cluster_fs_sector(cluster);
			self.read_fs_sectors(sec, self.geo.sectors_per_cluster, &mut scratch)?;
			let take = (cluster_bytes as usize - within).min(want - done);
			buffer[done..done + take].copy_from_slice(&scratch[within..within + take]);
			done += take;
			within = 0;
			chain.advance(self)?;
		}
		debug_assert_eq!(done, want, "the read loop fills exactly the disk-backed part");
		Ok(answer)
	}

	pub fn read_file_bounded(&mut self, path: &[u8], limit: usize) -> Result<Vec<u8>, FsError> {
		let (parent, name) = split_parent(path)?;
		let dir = self.resolve_dir(parent)?;
		let entry = self.find_entry(&dir, name)?;
		if entry.is_dir {
			return Err(FsError::IsDir);
		}
		// the bytes past the ValidDataLength are undefined on disk and the media's home
		// systems serve them as zeros - a preallocated tail must never leak stale
		// cluster content (classic entries carry no VDL: theirs equals the size).
		if entry.size > limit as u64 {
			return Err(FsError::TooLarge);
		}
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
		self.ensure_writable()?;
		let (parent, name) = split_parent(path)?;
		check_name(name)?;
		let dir = self.resolve_dir(parent)?;
		if self.geo.kind == Kind::ExFat {
			return self.under_dirty_flag(|fs| fs.exfat_write(&dir, name, data));
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
		// 1b. BARRIER: the data and the FAT are durable before the entry that names them is issued.
		//     Otherwise the device may write them back in either order and the entry can land
		//     first - a directory pointing at clusters whose contents never arrived.
		if let Err(e) = self.barrier() {
			let _ = self.free_chain(first);
			return Err(e);
		}
		// 2. swap the directory entry in ONE read-modify-write: mark the old entry deleted
		//    in the in-memory copy (its slots become reusable for the new entry), place the
		//    new entry set, and write the directory back once.
		let old_first = match self.swap_entry(&dir, name, first, data.len() as u32) {
			Ok(old) => old,
			// Freed only when nothing was published. Once the new entry set may be on the medium the
			// commit is ambiguous, and freeing the chain it names is what hands live clusters back
			// to the free pool for the next allocation to cross-link.
			Err(failure) => {
				if !failure.placed {
					let _ = self.free_chain(first);
				} else {
					// The medium may be carrying a directory entry naming this file. Refusing every
					// later mutation is what keeps that at "one uncertain commit" rather than
					// letting a retry allocate over it.
					self.degraded = true;
				}
				return Err(failure.reported());
			}
		};
		// 3. only now is the old chain unreachable - free it, best-effort: the write is
		//    durable at this point, so a failing device may cost lost clusters (the class
		//    the free walks already accept), never a false failure of a finished write.
		// 2b. BARRIER before the old chain is freed: the new entry has to be durable first, or a
		//     crash can leave the OLD entry live with its clusters already back in the free pool.
		self.barrier()?;
		if let Some(old) = old_first {
			let _ = self.free_chain(old);
		}
		// Durable when it returns, which is what a caller that reports a written file assumes.
		self.barrier()
	}

	// Delete a file named by a `/`-separated path: free its cluster chain and clear its
	// directory entry, for any of the four families.
	pub fn remove(&mut self, path: &[u8]) -> Result<(), FsError> {
		self.ensure_writable()?;
		let (parent, name) = split_parent(path)?;
		let dir = self.resolve_dir(parent)?;
		if self.geo.kind == Kind::ExFat {
			return self.under_dirty_flag(|fs| fs.exfat_remove(&dir, name));
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
		// The same rule the name parser applies, because a path reaching here directly - `list_dir`
		// takes one - must not be judged by a different one. An empty path is the root; an empty
		// SEGMENT inside a path is malformed.
		let body = path.strip_prefix(b"/").unwrap_or(path);
		if !body.is_empty() && body.split(|&b| b == b'/').any(|s| s.is_empty()) {
			return Err(FsError::BadName);
		}
		let mut dir = Dir::at(self.root_cluster());
		for seg in body.split(|&b| b == b'/').filter(|s| !s.is_empty()) {
			let e = self.find_entry(&dir, seg)?;
			// A path component that names a file is not a missing directory, it is the wrong kind
			// of thing - which `FsError` has a word for and this returned `NotFound` instead of.
			if !e.is_dir {
				return Err(FsError::NotDir);
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
		entries.into_iter().find(|e| e.matches(name, &self.upcase)).ok_or(FsError::NotFound)
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
			Kind::ExFat => parse_exfat_dir(&bytes, &self.upcase),
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
	// Read the WHOLE chain, refusing past `cap` rather than sizing the allocation from the medium.
	//
	// `read_chain`'s `limit` is an exact length - a chain that ends early is `Invalid`, because the
	// entry's own size says how much is there - and `usize::MAX` is its "read to the end" sentinel.
	// A directory that records no length needs the second meaning WITH a ceiling, which is a third
	// thing: read to the end, and refuse a volume whose end is past what its own format allows.
	// A chain read to its end, refusing before it allocates past `cap` rather than after.
	//
	// This was `read_chain(first, usize::MAX)` and then a length check on the result: the limit was
	// applied to what had already been built, so a long acyclic chain on a hostile volume allocated
	// all of it and was then refused. `read_chain`'s own comment names this hazard for the cyclic
	// case - a cycle read "with no limit at all (the exFAT root, the allocation bitmap) grew the
	// buffer a cluster at a time... an enormous allocation performed on behalf of a medium the
	// driver does not trust" - and the cycle was closed with Floyd while the acyclic case reached
	// the same allocation through the caller that had been given a ceiling.
	//
	// `read_chain` stops as soon as it has `cap` bytes, so the difference between "the chain fits"
	// and "the chain is too long" is one more cluster, and that is what is asked for.
	fn read_whole_chain(&mut self, first: u32, cap: usize) -> Result<Vec<u8>, FsError> {
		self.read_chain_to_end(first, cap)
	}

	// Walk a chain to its terminator, refusing BEFORE the allocation that would pass `cap` rather
	// than after it.
	//
	// `read_whole_chain` was `read_chain(first, usize::MAX)` and a length check on the result, so
	// the ceiling was applied to a buffer that had already been built - a long acyclic chain on a
	// hostile volume allocated all of it and was then refused. `read_chain`'s own comment names the
	// hazard for the cyclic case ("an enormous allocation performed on behalf of a medium the driver
	// does not trust") and Floyd closed that one; the acyclic case reached the same allocation
	// through the caller that had been handed a ceiling.
	//
	// Separate from `read_chain` rather than a flag on it, because the two ask different questions.
	// `read_chain` is given a LENGTH the entry declared and treats a chain that ends early as a
	// truncated file; this one has no declared length - the chain's end is the answer - and a cap
	// that the chain does not reach is the ordinary case rather than an error.
	#[cfg(test)]
	pub(crate) fn read_chain_to_end_for_test(&mut self, first: u32, cap: usize) -> Result<Vec<u8>, FsError> {
		self.read_chain_to_end(first, cap)
	}

	fn read_chain_to_end(&mut self, first: u32, cap: usize) -> Result<Vec<u8>, FsError> {
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		let max = self.max_cluster();
		let mut out: Vec<u8> = Vec::new();
		// Through `ChainCursor`, as every other walk in this file now is.
		let mut chain = ChainCursor::new(first, false, max);
		while let Ok(cluster) = chain.current(self) {
			// BEFORE the read, not after the buffer holds it. This is the whole difference.
			if out.len().saturating_add(cluster_bytes) > cap {
				return Err(FsError::TooLarge);
			}
			let sec = self.cluster_fs_sector(cluster);
			let mut buf = vec![0u8; cluster_bytes];
			self.read_fs_sectors(sec, self.geo.sectors_per_cluster, &mut buf)?;
			out.extend_from_slice(&buf);
			chain.advance(self)?;
		}
		Ok(out)
	}

	fn read_chain(&mut self, first: u32, limit: usize) -> Result<Vec<u8>, FsError> {
		if limit == 0 {
			return Ok(Vec::new());
		}
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		let max = self.max_cluster();
		let mut out: Vec<u8> = Vec::new();
		// Floyd over the chain: `slow` follows one link for every two the read takes, so a chain
		// that loops back on itself is caught in the length of the cycle.
		//
		// A step budget cannot do this job, which is why the one that was here did not. A cluster
		// pointing at itself, read for a 1024-byte file with 512-byte clusters, is two steps and
		// returns 1024 bytes of the same cluster twice as `Ok` - well inside any budget the volume
		// could justify. The corruption is the repetition, not the length. The same cycle read with
		// no limit at all (the exFAT root, the allocation bitmap) grew the buffer a cluster at a
		// time until the budget tripped at `cluster_count`, which on a real volume is an enormous
		// allocation performed on behalf of a medium the driver does not trust.
		// Through `ChainCursor`, which is where all four of those rules live now.
		let mut chain = ChainCursor::new(first, false, max);
		while let Ok(cluster) = chain.current(self) {
			let sec = self.cluster_fs_sector(cluster);
			let mut buf = vec![0u8; cluster_bytes];
			self.read_fs_sectors(sec, self.geo.sectors_per_cluster, &mut buf)?;
			out.extend_from_slice(&buf);
			if out.len() >= limit {
				break;
			}
			chain.advance(self)?;
		}
		// A chain that ended before it produced the bytes it was read for is a truncated file, not
		// a short one: the entry's own length says how much is there and the FAT does not have it.
		// Returning the prefix as `Ok` made a 20 MiB entry backed by one cluster answer with 4 KiB
		// of data and no indication that the rest was missing - and made `first_cluster = 0` with a
		// nonzero size read as a perfectly good empty file.
		if limit != usize::MAX && out.len() < limit {
			return Err(FsError::Invalid);
		}
		if limit != usize::MAX {
			out.truncate(limit);
		}
		Ok(out)
	}

	// How much of the allocation bitmap's chain may be read: its recorded size, rounded up to whole
	// clusters. The bitmap is a structure that says how long it is, so reading it with no limit at
	// all took the bound from the volume instead - the one place a hostile medium controls.
	fn bitmap_cap(&self, bm_size: u64) -> usize {
		let cluster_bytes = self.geo.sectors_per_cluster as u64 * self.geo.bytes_per_sector as u64;
		bm_size.div_ceil(cluster_bytes).saturating_mul(cluster_bytes).min(usize::MAX as u64) as usize
	}

	// A two-sector sliding window over the active FAT, for the scans that used to read the whole
	// table into memory.
	//
	// A 66000-cluster FAT32 table is 264 KiB and a 4 TiB volume's is a gigabyte; reading it whole to
	// count zeros made the driver's memory a function of the medium's size, which is the one number
	// a hostile volume controls. The window costs the same device reads - each FAT sector is still
	// read once - and holds two sectors, because a FAT12 entry can straddle the boundary between
	// them and half an entry is not an entry.
	//
	// Returns the offset of `cluster`'s first byte within the window.
	fn fat_window(&mut self, window: &mut [u8], loaded: &mut Option<u32>, cluster: u32) -> Result<usize, FsError> {
		let bps = self.geo.bytes_per_sector as u64;
		let off = match self.geo.kind {
			Kind::Fat12 => cluster as u64 + cluster as u64 / 2,
			Kind::Fat16 => cluster as u64 * 2,
			Kind::Fat32 | Kind::ExFat => cluster as u64 * 4,
		};
		let sector = (off / bps) as u32;
		if *loaded != Some(sector) {
			let fat_base = self.geo.reserved_sectors + self.geo.active_fat * self.geo.fat_size;
			// Two sectors unless the second would be past the table, which is the last-entry case.
			let count = if sector + 1 < self.geo.fat_size { 2 } else { 1 };
			let bytes = count as usize * bps as usize;
			self.read_fs_sectors((fat_base + sector) as u64, count, &mut window[..bytes])?;
			window[bytes..].fill(0);
			*loaded = Some(sector);
		}
		Ok((off % bps) as usize)
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
			Kind::ExFat => u32::from_le_bytes([buf[within], buf[within + 1], buf[within + 2], buf[within + 3]]),
			Kind::Fat32 => u32::from_le_bytes([buf[within], buf[within + 1], buf[within + 2], buf[within + 3]]) & 0x0FFF_FFFF,
		})
	}

	// True when a FAT entry is an end-of-chain marker for the family's width.
	fn is_end(&self, cluster: u32) -> bool {
		match self.geo.kind {
			Kind::Fat12 => cluster >= 0x0FF8,
			Kind::Fat16 => cluster >= 0xFFF8,
			// exFAT's only end-of-chain value is 0xFFFFFFFF; FAT32 reserves 0x0FFFFFF8 and up.
			Kind::ExFat => cluster == 0xFFFF_FFFF,
			Kind::Fat32 => cluster >= 0x0FFF_FFF8,
		}
	}

	// The write BARRIER: everything issued so far is durable before anything after it is issued.
	//
	// `BlockDevice::flush` existed, is documented in fs-core as the barrier a commit protocol needs,
	// and the USB backend implements it as SCSI SYNCHRONIZE CACHE(10) - and this crate never called
	// it, not once. Without it "the data was written before the directory entry" describes the order
	// the CPU issued the writes and not the order they become durable, so the ordering every
	// crash-safety claim in this file rests on was never actually requested of the device.
	fn barrier(&mut self) -> Result<(), FsError> {
		if self.dev.flush() { Ok(()) } else { Err(FsError::Io) }
	}

	// The device, so a test can inspect what the driver actually asked it for.
	#[cfg(test)]
	pub(crate) fn device_for_test(&mut self) -> &mut D {
		&mut self.dev
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
	// The FAT write and read, reachable from the tests. The exFAT/FAT32 split below is the kind of
	// thing an internal round trip cannot see - both ends shared the mistake - so a test has to be
	// able to write an entry and read back what actually landed.
	#[cfg(test)]
	pub(crate) fn set_fat_entry_for_test(&mut self, cluster: u32, val: u32) -> Result<(), FsError> {
		self.set_fat_entry(cluster, val)
	}

	#[cfg(test)]
	pub(crate) fn fat_entry_for_test(&mut self, cluster: u32) -> u32 {
		self.next_cluster(cluster).unwrap_or(0)
	}

	// Write one FAT entry to every current copy, ALL OR NOTHING.
	//
	// The naive loop over copies is not atomic in two ways, and both leave a table that describes
	// something other than the volume's contents. With mirroring it can write copy 0 and fail on
	// copy 1, so the copies disagree and which one a foreign driver believes decides whether a
	// cluster is free. On FAT12 an entry can straddle a sector boundary, `write_fs_sectors` writes
	// 512 bytes at a time, and a failure between them tears the entry across its two nibbles.
	//
	// So every sector modified here is read back into an undo list first, and any failure puts the
	// originals back - including the copy that failed, because its first sector may have landed.
	// If a restore write fails too, the volume is past repairing from here and the mount degrades.
	fn set_fat_entry(&mut self, cluster: u32, val: u32) -> Result<(), FsError> {
		if self.degraded {
			return Err(FsError::ReadOnly);
		}
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
		// What was there before, per copy, in the order it was overwritten.
		let mut undo: Vec<(u64, Vec<u8>)> = Vec::new();
		for fat in copies {
			let fat_base = self.geo.reserved_sectors + fat * self.geo.fat_size;
			let sec = fat_base as u64 + byte_off / bps as u64;
			let within = (byte_off % bps as u64) as usize;
			let mut buf = vec![0u8; (bps * sectors) as usize];
			if let Err(error) = self.read_fs_sectors(sec, sectors, &mut buf) {
				self.restore_fat_sectors(undo, sectors);
				return Err(error);
			}
			let original = buf.clone();
			match self.geo.kind {
				Kind::Fat12 => {
					let cur = u16::from_le_bytes([buf[within], buf[within + 1]]);
					let next = if cluster & 1 == 1 { (cur & 0x000F) | ((val as u16) << 4) } else { (cur & 0xF000) | (val as u16 & 0x0FFF) };
					buf[within..within + 2].copy_from_slice(&next.to_le_bytes());
				}
				Kind::Fat16 => buf[within..within + 2].copy_from_slice(&(val as u16).to_le_bytes()),
				// A full 32-bit entry, written whole: exFAT has no reserved top nibble, and
				// preserving one turned a real 0xFFFFFFFF terminator into 0xF0000005 when the chain
				// was grown to cluster 5.
				Kind::ExFat => buf[within..within + 4].copy_from_slice(&val.to_le_bytes()),
				Kind::Fat32 => {
					let cur = u32::from_le_bytes([buf[within], buf[within + 1], buf[within + 2], buf[within + 3]]);
					// FAT32 entries are 28-bit: the top nibble belongs to the medium.
					let next = (cur & 0xF000_0000) | (val & 0x0FFF_FFFF);
					buf[within..within + 4].copy_from_slice(&next.to_le_bytes());
				}
			}
			// Recorded BEFORE the write, not after: a write that fails may still have landed its
			// first sector, and that is exactly the copy the undo list has to cover.
			undo.push((sec, original));
			if let Err(error) = self.write_fs_sectors(sec, sectors, &buf) {
				self.restore_fat_sectors(undo, sectors);
				return Err(error);
			}
		}
		Ok(())
	}

	// Put back the sectors an interrupted `set_fat_entry` overwrote, newest first.
	//
	// A failure here is not recoverable and must not be swallowed: the caller asked to undo because
	// the medium refused a write, and if it refuses the undo too there is no state left to return
	// to. The mount degrades, every later mutation is refused, and what the medium holds stays
	// whatever the failure left it - which a repair tool can read, unlike the result of continuing.
	fn restore_fat_sectors(&mut self, undo: Vec<(u64, Vec<u8>)>, sectors: u32) {
		for (sec, original) in undo.into_iter().rev() {
			if self.write_fs_sectors(sec, sectors, &original).is_err() {
				self.degraded = true;
			}
		}
	}

	// Allocate `n` free clusters into an end-terminated chain, returning them in order.
	// Zero clusters is an empty file. NoSpace if the table runs out of free entries. The
	// scan runs over ONE in-memory image of the ACTIVE FAT copy (a per-candidate device
	// read made allocation O(volume) round-trips on slow media); a failure writing a
	// link unwinds the slots already written, so nothing leaks - and because `set_fat_entry`
	// is itself all-or-nothing, the entry that failed is not among them.
	fn alloc_chain(&mut self, n: usize) -> Result<Vec<u32>, FsError> {
		if n == 0 {
			return Ok(Vec::new());
		}
		let bps = self.geo.bytes_per_sector as usize;
		let mut window = vec![0u8; 2 * bps];
		let mut loaded: Option<u32> = None;
		let mut chain: Vec<u32> = Vec::with_capacity(n);
		let mut c = 2u32;
		let max = self.max_cluster();
		while chain.len() < n {
			if c > max {
				return Err(FsError::NoSpace);
			}
			let within = self.fat_window(&mut window, &mut loaded, c)?;
			if fat_entry_in(&window, within, self.geo.kind, c) == 0 {
				chain.push(c);
			}
			c += 1;
		}
		// The terminator the FORMAT defines, not one constant for both: exFAT's only end-of-chain
		// value is 0xFFFFFFFF, and writing 0x0FFF_FFFF made every volume this driver created
		// non-conforming - readable here only because the reader shared the mistake.
		let eoc = match self.geo.kind {
			Kind::ExFat => EXFAT_EOC,
			_ => 0x0FFF_FFFF,
		};
		for i in 0..chain.len() {
			let val = if i + 1 < chain.len() { chain[i + 1] } else { eoc };
			if let Err(e) = self.set_fat_entry(chain[i], val) {
				// `chain[i]` needs no undo: `set_fat_entry` is all-or-nothing, so the entry that
				// failed is back to what it was. Only the links already written have to go.
				for &done in &chain[..i] {
					if self.set_fat_entry(done, 0).is_err() {
						// The medium refused the undo of an allocation it had accepted. Those
						// clusters are now marked in use with no entry naming them - a leak the
						// next mount cannot tell from a live file, and one that grows with every
						// retry. Refusing further mutations keeps it at a leak.
						self.degraded = true;
						break;
					}
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
	#[cfg(test)]
	pub(crate) fn read_dir_bytes_for_test(&mut self, dir: &Dir) -> Result<Vec<u8>, FsError> {
		self.read_dir_bytes(dir)
	}

	#[cfg(test)]
	pub(crate) fn is_end_for_test(&self, cluster: u32) -> bool {
		self.is_end(cluster)
	}

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
		// A CEILING A LEGAL VOLUME CANNOT EXCEED, rather than `usize::MAX`.
		//
		// This reached `read_chain(dir.cluster, usize::MAX)` whenever a directory records no length,
		// which is EVERY exFAT root - a root has no entry set of its own to record one. The chain
		// checks are sound, so a long ACYCLIC chain is legal and forced an allocation as large as
		// the volume allows. `load_upcase` reads the root at mount, so this ran before anything had
		// vouched for the volume.
		//
		// The exFAT specification bounds a directory at 256 MiB, so a chain past `MAX_DIR_BYTES` is
		// a volume outside its own format rather than a large one.
		self.read_whole_chain(dir.cluster, MAX_DIR_BYTES)
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
		let bps = self.geo.bytes_per_sector as usize;
		let at = |i: usize| orig.get(i).copied().unwrap_or(0);
		// WHICH SECTORS CHANGED, not the span between the first and last change.
		//
		// The span is one number and it is the wrong one: an overwrite that puts the new entry in
		// an early hole and deletes the old one hundreds of clusters later rewrote every cluster
		// between them. That is write amplification on a medium whose writes are the scarce thing,
		// and - because a directory swap is not atomic - it is also a crash window hundreds of
		// clusters wide where two sectors would do.
		let count = bytes.len().div_ceil(bps);
		let mut dirty = vec![false; count];
		for (sector, flag) in dirty.iter_mut().enumerate() {
			let from = sector * bps;
			let to = (from + bps).min(bytes.len());
			*flag = (from..to).any(|i| bytes[i] != at(i));
		}
		if !dirty.iter().any(|&d| d) {
			return Ok(());
		}
		if dir.cluster == 0 {
			let start = (self.geo.reserved_sectors + self.geo.num_fats * self.geo.fat_size) as u64;
			return self.write_dirty_runs(start, &dirty, 0, count, bytes);
		}
		let spc = self.geo.sectors_per_cluster as usize;
		let clusters = count.div_ceil(spc);
		if dir.nfc_len.is_some() {
			for k in 0..clusters {
				self.write_dirty_runs(self.cluster_fs_sector(dir.cluster + k as u32), &dirty, k * spc, spc, bytes)?;
			}
			return Ok(());
		}
		// The chain is still walked whole - a later cluster can be dirty when an earlier one is not
		// - but a clean cluster costs a FAT lookup rather than a write.
		let mut c = dir.cluster;
		let mut k = 0usize;
		while k < clusters && c >= 2 && !self.is_end(c) {
			if c > self.max_cluster() {
				return Err(FsError::Invalid);
			}
			self.write_dirty_runs(self.cluster_fs_sector(c), &dirty, k * spc, spc, bytes)?;
			c = self.next_cluster(c)?;
			k += 1;
		}
		Ok(())
	}

	// Write the dirty sectors of one region as merged runs: `dirty[from..from + len]` describes the
	// region's sectors, and `base` is where its first sector lives on the volume. Adjacent dirty
	// sectors become one write, which is what keeps a multi-entry set from becoming one write per
	// 512 bytes.
	fn write_dirty_runs(&mut self, base: u64, dirty: &[bool], from: usize, len: usize, bytes: &[u8]) -> Result<(), FsError> {
		let bps = self.geo.bytes_per_sector as usize;
		let mut i = 0;
		while i < len {
			if !dirty.get(from + i).copied().unwrap_or(false) {
				i += 1;
				continue;
			}
			let start = i;
			while i < len && dirty.get(from + i).copied().unwrap_or(false) {
				i += 1;
			}
			let at = (from + start) * bps;
			let end = (from + i) * bps;
			if end > bytes.len() {
				return Err(FsError::Invalid);
			}
			self.write_fs_sectors(base + start as u64, (i - start) as u32, &bytes[at..end])?;
		}
		Ok(())
	}

	// Remove the entry named `name` (its long name or 8.3 short form) from `dir`: mark
	// its 8.3 record plus any long fragments deleted and release its chain. Returns
	// whether the name was present.
	fn unlink_in(&mut self, dir: &Dir, name: &[u8]) -> Result<bool, FsError> {
		let mut bytes = self.read_dir_bytes(dir)?;
		let orig = bytes.clone();
		match mark_unlinked(&mut bytes, name, &self.upcase)? {
			None => Ok(false),
			Some(e) => {
				self.write_dir_dirty(dir, &bytes, &orig)?;
				// A BARRIER BEFORE THE FREE. "Durable once the directory write lands" is the
				// assumption a barrier exists to stop making, and `write_file` beside this no longer
				// makes it. The failure is the mirror of the publish one: the FAT free reaches the
				// medium, the directory deletion does not, and after a crash a live entry names
				// clusters the FAT calls free - which the next allocation cross-links.
				//
				// A failing barrier means the deletion is not known to be durable, so the clusters
				// stay where they are. Lost space against a cross-link is not a close comparison.
				self.barrier()?;
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
	fn swap_entry(&mut self, dir: &Dir, name: &[u8], first: u32, size: u32) -> Result<Option<u32>, SwapFailure> {
		// Everything before the first write publishes nothing, so its failures carry `placed: false`.
		let fail = |error: FsError| SwapFailure { error, placed: false };
		let mut bytes = self.read_dir_bytes(dir).map_err(fail)?;
		let orig = bytes.clone();
		scrub_after_terminator(&mut bytes);
		let old = mark_unlinked(&mut bytes, name, &self.upcase).map_err(fail)?;
		let name: &[u8] = match &old {
			Some(o) if writable_name(o.name.as_bytes()) => o.name.as_bytes(),
			_ => name,
		};
		// The attributes of the file being replaced, plus archive - not a fresh 0x20, which is how
		// an overwrite used to clear read-only, hidden and system.
		let attr = old.as_ref().map_or(dir::ATTR_ARCHIVE, |o| (o.attr & dir::ATTR_CARRIED) | dir::ATTR_ARCHIVE);
		let mut entries = build_entries(name, &bytes, first, size, attr, self.dos_stamp()).map_err(fail)?;
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
				return Err(fail(FsError::NoSpace));
			}
			self.grow_dir(dir.cluster, &mut bytes).map_err(fail)?;
		};
		for (k, e) in entries.iter().enumerate() {
			bytes[at + k * 32..at + k * 32 + 32].copy_from_slice(e);
		}
		// TWO ordered writes, not one, and the order is the whole protocol.
		//
		// It was one `write_dir_dirty` over the span between the lowest and highest change, written
		// cluster by cluster - so the cluster holding the NEW entry could land while the later
		// cluster holding the OLD entry's deletion failed. `swap_entry` then returned `Err`, the
		// caller freed the new data chain, and the medium was left with a live directory entry
		// pointing at clusters that were back in the free pool. The next allocation cross-links
		// them. The comment above `write_file` says a failure part-way never costs the old file;
		// that is the claim this broke.
		//
		// An I/O error means the commit is AMBIGUOUS - not that nothing was written - so the new
		// chain cannot be freed once its entry may be live. The order below makes that decidable:
		//
		//   1. place the new entry set, with the old one still live. A failure here published
		//      nothing, so the caller may free the new chain.
		//   2. barrier.
		//   3. retire the old entry set. A failure here leaves BOTH entries live - a duplicate name,
		//      which `fsck` can resolve and which loses nobody's data - and the caller must not free
		//      anything.
		//
		// `placed` is what tells the caller which side of that line it is on.
		// The intermediate state: the new entry set placed, and everything else exactly as it was -
		// including the old entry, still live. Built from `bytes` (which already has the right
		// length, the directory may have grown) by restoring every byte outside the new set from
		// `orig`; the grown tail has no `orig` and is zeros in both.
		let new_span = at..at + entries.len() * 32;
		let mut staged = bytes.clone();
		for i in 0..staged.len() {
			if !new_span.contains(&i) {
				staged[i] = orig.get(i).copied().unwrap_or(0);
			}
		}
		// Past the NEXT line the new entry may be on the medium, and the line after it cannot
		// change that.
		//
		// The barrier used to carry `placed: false`, which reads the failure as "nothing was
		// written". It is not: `write_dir_dirty` returning `Ok` means the device ACCEPTED those
		// sectors, and a flush that then fails says the durability is unknown - not that the cache
		// was thrown away. A device does not discard accepted writes because a flush reported an
		// error; it is at least as likely to commit them a moment later. So the caller freed the new
		// chain while its entry was live on the medium, which is the one outcome this protocol
		// exists to prevent.
		//
		// `placed: true` gives the benign state the protocol already describes for its third step:
		// both entries live, a duplicate name, no data lost, `fsck` resolves it.
		//
		// A PARTIAL write is the same question one step earlier, and it gets the same answer: an
		// entry set can straddle a sector boundary, so a `write_dir_dirty` that fails part-way may
		// also have published. Nothing here may assume otherwise.
		self.write_dir_dirty(dir, &staged, &orig).map_err(|error| SwapFailure { error, placed: true })?;
		self.barrier().map_err(|error| SwapFailure { error, placed: true })?;
		self.write_dir_dirty(dir, &bytes, &staged).map_err(|error| SwapFailure { error, placed: true })?;
		self.barrier().map_err(|error| SwapFailure { error, placed: true })?;
		Ok(old.map(|o| o.first_cluster))
	}

	// Grow a chained directory by one zeroed cluster: allocate it, zero it on the device
	// (BEFORE linking - once linked, stale content would parse as directory entries if a
	// later write fails), link it at the end of the chain, and extend the in-memory copy
	// to match. A failure part-way frees the fresh cluster, so nothing leaks.
	//
	// A failure AFTER this returns is a different matter, and deliberately not undone: the caller
	// writes the entry into the grown directory, and if that write fails the directory stays one
	// cluster longer. The tail is zeroed and parses as free slots, so it costs space rather than
	// correctness - and it does not compound, because a grow only happens when no free run exists
	// and the tail it leaves IS one. The retry writes into it instead of growing again.
	// `a_directory_grown_by_a_failed_write_is_grown_at_most_once` holds that bound.
	//
	// Rolling it back would trade a bounded cost for an unbounded risk: the entry write may have
	// landed in the new cluster before failing, and freeing a cluster a directory entry lives in is
	// how a directory loses files rather than space.
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
	// Raise or clear exFAT's VolumeDirty, and mark PercentInUse unknown while allocation is moving.
	//
	// These are the two fields the specification asks a writer to maintain and this driver skipped
	// "to save sector writes". VolumeDirty is the recovery signal: raised before a metadata
	// transaction and cleared after it, so a volume that lost power mid-write says so instead of
	// looking clean. PercentInUse is required to be kept current or set to FFh - unknown - and
	// unknown is both honest and free, where recomputing a percentage would cost a bitmap scan on
	// every allocation.
	//
	// Both fields sit in the three bytes the boot checksum deliberately skips, which is exactly
	// because they are meant to be rewritten under a running system: maintaining them cannot
	// invalidate the checksum.
	// Every mutation passes through here. `set_fat_entry` refuses on its own, which covers most of
	// them, but not a remove of a file with no clusters - and "most mutations" is not a read-only
	// mount.
	fn ensure_writable(&self) -> Result<(), FsError> {
		if self.degraded {
			return Err(FsError::ReadOnly);
		}
		Ok(())
	}

	fn set_volume_dirty(&mut self, dirty: bool) -> Result<(), FsError> {
		if self.geo.kind != Kind::ExFat {
			return Ok(());
		}
		let mut boot = [0u8; SECTOR_SIZE];
		if !self.dev.read_block(0, &mut boot) {
			return Err(FsError::Io);
		}
		let mut flags = u16::from_le_bytes([boot[106], boot[107]]);
		if dirty {
			flags |= 0x02;
		} else {
			flags &= !0x02;
		}
		boot[106..108].copy_from_slice(&flags.to_le_bytes());
		boot[112] = 0xFF;
		if !self.dev.write_block(0, &boot) {
			return Err(FsError::Io);
		}
		Ok(())
	}

	// Run a metadata transaction between the two halves of the dirty flag. A failure to CLEAR it is
	// not a failure of the operation - the data is written - but it does leave the volume looking
	// unclean, which is the safe direction to fail in.
	fn under_dirty_flag<R>(&mut self, body: impl FnOnce(&mut Self) -> Result<R, FsError>) -> Result<R, FsError> {
		// SET, FLUSH, RUN, FLUSH, CLEAR, FLUSH - and only clear on success.
		//
		// This cleared the flag unconditionally, which is precisely backwards: a `body` that fails
		// is the case the flag exists for, because a metadata mutation may have landed part-way. The
		// volume was told the next mount that it was consistent over exactly the state that is not.
		//
		// And the flushes are what make it a bracket rather than advice. Without them the flag and
		// the metadata it brackets can reach the medium in any order, so a correctly-set flag can
		// arrive after the damage it was meant to announce.
		self.set_volume_dirty(true)?;
		self.barrier()?;
		let outcome = body(self);
		if outcome.is_err() {
			// LEFT UP, and that is all. The flag says "a metadata transaction on this volume did not
			// complete", which the next mount and every other implementation can act on.
			//
			// Degrading the mount here as well was tried and reverted: most failures inside a
			// transaction are ones the operation has already undone - a grow that gave its cluster
			// back, an allocation that rolled its links back - and turning those into a read-only
			// volume punishes a clean refusal. Degrading belongs where the UNDO failed, which is
			// where `set_fat_entry` already does it.
			return outcome;
		}
		self.barrier()?;
		let _ = self.set_volume_dirty(false);
		let _ = self.barrier();
		outcome
	}

	fn exfat_write(&mut self, dir: &Dir, name: &[u8], data: &[u8]) -> Result<(), FsError> {
		let cluster_bytes = (self.geo.sectors_per_cluster * self.geo.bytes_per_sector) as usize;
		let need = data.len().div_ceil(cluster_bytes);
		let chain = self.exfat_alloc(need)?;
		let first = chain.first().copied().unwrap_or(0);
		if let Err(e) = self.write_clusters(&chain, data) {
			let _ = self.exfat_free(first);
			return Err(e);
		}
		// The same barriers as the classic path, for the same reason.
		if let Err(e) = self.barrier() {
			let _ = self.exfat_free(first);
			return Err(e);
		}
		let old = match self.exfat_swap_entry(dir, name, first, data.len() as u64) {
			Ok(old) => old,
			// Freed only when nothing was published, exactly as the classic caller decides it.
			Err(failure) => {
				if !failure.placed {
					let _ = self.exfat_free(first);
				} else {
					self.degraded = true;
				}
				return Err(failure.reported());
			}
		};
		// the write is durable once the entry set lands - the release of the replaced
		// clusters is best-effort, like the classic path's.
		self.barrier()?;
		if let Some(old) = old {
			let _ = self.exfat_release(&old);
		}
		self.barrier()
	}

	// Delete an exFAT file: clear its entry set's in-use bits and release its clusters.
	fn exfat_remove(&mut self, dir: &Dir, name: &[u8]) -> Result<(), FsError> {
		let mut bytes = self.read_dir_bytes(dir)?;
		let orig = bytes.clone();
		let Some(old) = exfat_mark_unlinked(&mut bytes, name, &self.upcase)? else {
			return Err(FsError::NotFound);
		};
		self.write_dir_dirty(dir, &bytes, &orig)?;
		// The same barrier as the classic path, for the same reason: the entry has to be durably
		// gone before its clusters go back. It is also the ordering the exFAT specification
		// recommends for a delete - directory, then FAT, then the allocation bitmap.
		self.barrier()?;
		let _ = self.exfat_release(&old);
		Ok(())
	}

	// Swap an exFAT entry set in ONE read-modify-write: mark any old set's in-use bits
	// cleared (its slots become reusable), place the new set (growing a chained
	// directory by whole clusters until the set fits), write the directory back once. An
	// overwrite preserves the replaced set's on-disk name and creation stamp, as the
	// media's home systems do. Returns the replaced entry, whose clusters only then
	// are safe to release.
	fn exfat_swap_entry(&mut self, dir: &Dir, name: &[u8], first: u32, size: u64) -> Result<Option<Raw>, SwapFailure> {
		// THE SAME PROTOCOL THE CLASSIC PATH HAS, which this never got.
		//
		// This marked the old set unlinked AND placed the new one in the same buffer, wrote it once,
		// and returned a bare `FsError` - so the caller could not tell which side of the publish a
		// failure fell on and freed the new chain unconditionally. A `write_dir_dirty` that lands
		// the sector holding the new entry and fails on a later one therefore left a live entry
		// naming clusters that were back in the free pool, and the next allocation cross-linked
		// them. Exactly the finding the classic path was fixed for.
		//
		// It is worse here than it was there, and the reason is the format: an exFAT entry set
		// carries its in-use bit in the FILE entry, which is FIRST. A set that straddles a sector
		// boundary and lands only partially leaves the live half on the medium. In a classic set the
		// 8.3 entry is last, so the same partial write is likelier to leave nothing live.
		let fail = |error: FsError| SwapFailure { error, placed: false };
		let mut bytes = self.read_dir_bytes(dir).map_err(fail)?;
		let orig = bytes.clone();
		scrub_after_terminator(&mut bytes);
		let old = exfat_mark_unlinked(&mut bytes, name, &self.upcase).map_err(fail)?;
		let name: &[u8] = match &old {
			Some(o) if writable_name(o.name.as_bytes()) => o.name.as_bytes(),
			_ => name,
		};
		let attr = old.as_ref().map_or(dir::ATTR_ARCHIVE, |o| (o.attr & dir::ATTR_CARRIED) | dir::ATTR_ARCHIVE);
		let mut set = build_exfat_set(name, first, size, self.exfat_stamp(), attr, &self.upcase);
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
				return Err(fail(FsError::NoSpace));
			}
			self.exfat_grow_dir(dir, &mut bytes).map_err(fail)?;
		};
		bytes[at..at + set.len()].copy_from_slice(&set);
		// TWO ORDERED WRITES with a barrier between them, and the order is the protocol: place the
		// new set while the old one is still IN USE, then retire the old one. A failure after the
		// first write leaves both live - a duplicate name, which `fsck` resolves and which loses
		// nobody's data - and the caller must not free anything.
		//
		// `staged` is `bytes` with everything outside the new set restored from `orig`, so the only
		// difference from the medium is the new set itself.
		let new_span = at..at + set.len();
		let mut staged = bytes.clone();
		for i in 0..staged.len() {
			if !new_span.contains(&i) {
				staged[i] = orig.get(i).copied().unwrap_or(0);
			}
		}
		// Past here the new set may be on the medium, and neither the barrier nor a partial write
		// can take that back - see the classic path for why a failing flush is not "nothing
		// happened".
		let placed = |error: FsError| SwapFailure { error, placed: true };
		self.write_dir_dirty(dir, &staged, &orig).map_err(placed)?;
		self.barrier().map_err(placed)?;
		self.write_dir_dirty(dir, &bytes, &staged).map_err(placed)?;
		self.barrier().map_err(placed)?;
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
		// The tail is kept, not just walked: undoing the link needs to know which entry to put the
		// end-of-chain marker back into, and recomputing it after a failure would walk a chain the
		// failure may have left in a different shape.
		let tail = self.write_fs_sectors(self.cluster_fs_sector(grow), self.geo.sectors_per_cluster, &vec![0u8; cluster_bytes]).and_then(|()| self.last_cluster(dir.cluster));
		let last = match tail {
			Ok(last) => last,
			Err(e) => {
				let _ = self.exfat_free(grow);
				return Err(e);
			}
		};
		if let Err(e) = self.set_fat_entry(last, grow) {
			let _ = self.exfat_free(grow);
			return Err(e);
		}
		bytes.resize(bytes.len() + cluster_bytes, 0);
		if let Some(p) = dir.parent {
			if let Err(error) = self.exfat_grow_parent_record(&p, cluster_bytes as u64) {
				// The cluster is linked and the parent's record still describes the shorter
				// directory. Returning here would leave a chain longer than its own DataLength -
				// a cluster marked in use that nothing reaches, which no later mount can tell from
				// a live one. Put the terminator back and release it.
				bytes.truncate(bytes.len() - cluster_bytes);
				if self.set_fat_entry(last, EXFAT_EOC).is_err() || self.exfat_free(grow).is_err() {
					self.degraded = true;
				}
				return Err(error);
			}
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
	// The volume's Up-case Table, read from the 0x82 entry in the root and checked against the
	// checksum recorded beside it.
	//
	// This is required reading, not an optimisation. exFAT defines case-insensitivity by the table
	// the FORMATTER wrote, so a driver that folds by its own rule computes different name hashes
	// and different collisions than the system the medium came from - a file written here with a
	// non-ASCII name is listable there and not openable by name.
	// ONE SCAN OF THE ROOT, AT MOUNT, VALIDATED WHOLE.
	//
	// This was two functions. `load_upcase` read the whole root looking for `0x82` and stopped at the
	// first one; `exfat_bitmap` read the whole root again, enforced the critical-primary rule and
	// required exactly one `0x81`. Each enforced a different subset of the root's rules, which is how
	// one of them came to hold a rule that belongs to the mount: `exfat_bitmap`'s own comment said
	// "refusing at the mount is where every other hostile-media decision in this file is made, and
	// `exfat_bitmap` is the first thing the mount asks for" - and the mount did not ask for it. It
	// called `load_upcase` and read the dirty flag, so a volume whose root carried `0x84`, a critical
	// primary this driver does not know, mounted, listed and read, and was refused only when
	// something needed free space.
	//
	// And the up-case table was chosen by POSITION, which is the same defect the bitmap fix was
	// written for, one entry type over: the format allows exactly one `0x82`, this took the first and
	// never looked at the rest.
	fn scan_exfat_root(&mut self) -> Result<(ExfatRoot, dir::Upcase), FsError> {
		let bytes = self.read_dir_bytes(&Dir::at(self.geo.root_cluster))?;
		let mut bitmap: Option<(u32, u64)> = None;
		let mut upcase: Option<(u32, u32, u64)> = None;
		let mut i = 0;
		while i + 32 <= bytes.len() {
			let e = &bytes[i..i + 32];
			if e[0] == 0x00 {
				break;
			}
			// AN UNRECOGNISED CRITICAL PRIMARY MAKES THE VOLUME INVALID.
			//
			// A primary is in use (bit 7) and not secondary (bit 6 clear); benign is bit 5. So the
			// critical primaries are `0x80..=0x9F`, of which this reader knows `0x81` (Allocation
			// Bitmap), `0x82` (Up-case Table), `0x83` (Volume Label) and `0x85` (File). Anything
			// else in that range is a structure the volume says the reader must understand, and
			// listing the volume anyway is operating on a layout this driver does not know.
			if e[0] & 0xE0 == 0x80 && !matches!(e[0], 0x81 | 0x82 | 0x83 | 0x85) {
				return Err(FsError::Invalid);
			}
			if e[0] == 0x81 {
				// THE FLAGS AND THE COUNT, not the type byte alone.
				//
				// With one FAT there is exactly one Allocation Bitmap and its BitmapIdentifier (bit
				// 0 of the flags) says it is the first. This mount already refuses `NumberOfFats
				// != 1`, so the rule it needs is the simple one, and a second bitmap entry is a
				// volume outside its own format rather than a choice to make.
				if e[1] & 0x01 != 0 || bitmap.is_some() {
					return Err(FsError::Invalid);
				}
				// A RESERVED FIELD IS A FIELD WITH A REQUIRED VALUE, which is this file's standard
				// for a parser of untrusted media: seven reserved bits above the identifier, and
				// eighteen reserved bytes between the flags and the FirstCluster.
				if e[1] & 0xFE != 0 || e[2..20].iter().any(|byte| *byte != 0) {
					return Err(FsError::Invalid);
				}
				let first = u32::from_le_bytes([e[20], e[21], e[22], e[23]]);
				let size = u64::from_le_bytes([e[24], e[25], e[26], e[27], e[28], e[29], e[30], e[31]]);
				bitmap = Some((first, size));
			}
			if e[0] == 0x82 {
				// EXACTLY ONE, for the same reason as the bitmap: a volume with two would be
				// mounted against whichever came first.
				if upcase.is_some() {
					return Err(FsError::Invalid);
				}
				let stored = u32::from_le_bytes([e[4], e[5], e[6], e[7]]);
				let first = u32::from_le_bytes([e[20], e[21], e[22], e[23]]);
				let size = u64::from_le_bytes([e[24], e[25], e[26], e[27], e[28], e[29], e[30], e[31]]);
				// Two bytes per unit and 65536 units is every table there can be; a DataLength past
				// that describes a table larger than the character set it maps.
				if size == 0 || size > 2 * 0x1_0000 {
					return Err(FsError::Invalid);
				}
				upcase = Some((first, stored, size));
			}
			i += 32;
		}
		let (bitmap_first, bitmap_size) = bitmap.ok_or(FsError::Invalid)?;
		let (first, stored, size) = upcase.ok_or(FsError::Invalid)?;
		let cluster_bytes = self.geo.sectors_per_cluster as u64 * self.geo.bytes_per_sector as u64;
		let cap = size.div_ceil(cluster_bytes).saturating_mul(cluster_bytes) as usize;
		let mut table = self.read_chain(first, cap)?;
		table.truncate(size as usize);
		if dir::Upcase::checksum(&table) != stored {
			return Err(FsError::Invalid);
		}
		let decoded = dir::Upcase::decode(&table).ok_or(FsError::Invalid)?;
		Ok((ExfatRoot { bitmap_first, bitmap_size }, decoded))
	}

	// WHERE THE BITMAP IS, as the mount's own scan of the root recorded it.
	//
	// This used to re-read and re-validate the whole root on every allocation and free. The rules it
	// enforced belong to the mount and are enforced there now; what is left is the answer.
	fn exfat_bitmap(&mut self) -> Result<(u32, u64), FsError> {
		let root = self.exfat_root.ok_or(FsError::Invalid)?;
		Ok((root.bitmap_first, root.bitmap_size))
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
		let mut bm = self.read_chain(bm_first, self.bitmap_cap(bm_size))?;
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
		// The terminator the FORMAT defines, not one constant for both: exFAT's only end-of-chain
		// value is 0xFFFFFFFF, and writing 0x0FFF_FFFF made every volume this driver created
		// non-conforming - readable here only because the reader shared the mistake.
		let eoc = match self.geo.kind {
			Kind::ExFat => EXFAT_EOC,
			_ => 0x0FFF_FFFF,
		};
		for i in 0..chain.len() {
			let val = if i + 1 < chain.len() { chain[i + 1] } else { eoc };
			if let Err(e) = self.set_fat_entry(chain[i], val) {
				// `chain[i]` needs no undo: `set_fat_entry` is all-or-nothing, so the entry that
				// failed is back to what it was. Only the links already written have to go.
				for &done in &chain[..i] {
					if self.set_fat_entry(done, 0).is_err() {
						// The medium refused the undo of an allocation it had accepted. Those
						// clusters are now marked in use with no entry naming them - a leak the
						// next mount cannot tell from a live file, and one that grows with every
						// retry. Refusing further mutations keeps it at a leak.
						self.degraded = true;
						break;
					}
				}
				return Err(e);
			}
		}
		// THE BITMAP IS PART OF THE SAME TRANSACTION AS THE LINKS, and it was not.
		//
		// The FAT links are written one at a time with a real rollback and a degrade when the undo
		// fails - the fix from finding 11 doing its job - and then this wrote the whole bitmap in one
		// call with no rollback at all. A failure here returned `Err` with the FAT already saying the
		// clusters were chained, and for exFAT the bitmap is the AUTHORITY on whether a cluster is
		// free, so the two disagreeing is not a cosmetic inconsistency.
		if self.write_dir_bytes(&bm_dir, &bm).is_err() {
			// Undo the links this call wrote, the same way a failed `set_fat_entry` undoes them.
			for &done in &chain {
				if self.set_fat_entry(done, 0).is_err() {
					break;
				}
			}
			// AND DEGRADE, whether or not the undo succeeded.
			//
			// The comment above this block says the bitmap "is part of the same transaction as the
			// links", and only the links half had a rollback: `write_dir_bytes` is a run of sector
			// writes, so a failure part-way leaves earlier sectors written - and for exFAT the
			// bitmap is the AUTHORITY on whether a cluster is free. Bits set to in-use with the FAT
			// rolled back and no entry naming them are clusters nothing can reach or reclaim.
			//
			// The link undo is worth keeping and is not enough on its own: the moment a partial
			// bitmap write is possible the mount's allocation state is unknown, and only refusing
			// further mutation keeps that at a leak rather than letting the next allocation hand out
			// a cluster the bitmap already claims. The milestone's summary of this - "undo the
			// bitmap, or degrade" - was true of neither half.
			self.degraded = true;
			return Err(FsError::Io);
		}
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
		let mut bm = self.read_chain(bm_first, self.bitmap_cap(bm_size))?;
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
		// The mirror of the allocation's transaction: here the FAT entries are cleared inside the
		// walk and the bitmap is written once at the end, so a failed bitmap write leaves clusters
		// the FAT calls free and the bitmap calls allocated. There is nothing to undo - the walk has
		// already run - so the volume is degraded instead, which keeps it at a leak rather than
		// letting the next allocation hand out a cluster the bitmap still claims.
		if self.write_dir_bytes(&bm_dir, &bm).is_err() {
			self.degraded = true;
			return Err(FsError::Io);
		}
		Ok(())
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
		let mut bm = self.read_chain(bm_first, self.bitmap_cap(bm_size))?;
		let bm_used = bm.len().min(bm_size as usize);
		for i in 0..count {
			let idx = (first + i - 2) as usize;
			let byte = idx / 8;
			if byte < bm_used {
				bm[byte] &= !(1 << (idx % 8));
			}
		}
		// THE SAME FAILURE CONTRACT AS `exfat_free`, which sets `degraded` and answers `Io` when its
		// bitmap write fails. Two allocation modes of one format should not differ in what a failed
		// bitmap write MEANS: for exFAT the bitmap is the authority on whether a cluster is free, so
		// a write that half-landed leaves clusters marked in use with nothing naming them, and only
		// refusing further mutation keeps that at a leak.
		if self.write_dir_bytes(&bm_dir, &bm).is_err() {
			self.degraded = true;
			return Err(FsError::Io);
		}
		Ok(())
	}
}

// Read `cluster`'s entry from an in-memory image of the FAT, for the allocation scan
// (an out-of-image offset reads as non-free, so it is never handed out).
// One FAT entry, read from a buffer at a known offset. This replaced a variant that indexed the
// whole table, which was the only thing keeping the whole table in memory.
fn fat_entry_in(buf: &[u8], within: usize, kind: Kind, cluster: u32) -> u32 {
	match kind {
		Kind::Fat12 => {
			if within + 2 > buf.len() {
				return 1;
			}
			let v = u16::from_le_bytes([buf[within], buf[within + 1]]);
			if cluster & 1 == 1 { (v >> 4) as u32 } else { (v & 0x0FFF) as u32 }
		}
		Kind::Fat16 => {
			if within + 2 > buf.len() {
				return 1;
			}
			u16::from_le_bytes([buf[within], buf[within + 1]]) as u32
		}
		// exFAT has no reserved top nibble: a terminator is all thirty-two bits, and masking it to
		// twenty-eight would turn one into an ordinary cluster pointer.
		Kind::ExFat => {
			if within + 4 > buf.len() {
				return 1;
			}
			u32::from_le_bytes([buf[within], buf[within + 1], buf[within + 2], buf[within + 3]])
		}
		Kind::Fat32 => {
			if within + 4 > buf.len() {
				return 1;
			}
			u32::from_le_bytes([buf[within], buf[within + 1], buf[within + 2], buf[within + 3]]) & 0x0FFF_FFFF
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
		// The boot signature, which exFAT requires exactly as the classic families do. It was
		// checked on the BPB path only, so an exFAT volume reached the geometry on its name alone.
		if b[510] != 0x55 || b[511] != 0xAA {
			return None;
		}
		// MustBeZero: the fifty-three bytes a classic BPB would occupy. A volume with anything
		// there was formatted by something that believed it was writing a different filesystem.
		if b[11..64].iter().any(|&byte| byte != 0) {
			return None;
		}
		// FileSystemRevision. A major version this driver has never seen means fields it reads may
		// mean something else, and guessing is how a reader corrupts a volume it does not know.
		if b[105] != 1 {
			return None;
		}
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
		// The FAT cannot start inside the boot region. The specification's floor is 24 sectors -
		// twelve for the main boot region and twelve for its backup - and without it a volume
		// declaring `fat_offset = 1` passed every other bound here, after which the first FAT write
		// would have gone through the backup boot sector.
		if fat_offset < 24 {
			return None;
		}
		// The FAT has to be able to address the heap the volume claims: one 32-bit entry per
		// cluster plus the two reserved entries. A shorter one used to mount as a quietly smaller
		// filesystem, because `max_cluster` takes the minimum of the two - which keeps writes in
		// bounds and hides that the volume disagrees with itself.
		let needed = (cluster_count as u64 + 2).saturating_mul(4).div_ceil(bytes_per_sector as u64);
		if (fat_size as u64) < needed {
			return None;
		}
		// VolumeLength must actually contain the heap it describes, and the specification's floor
		// is 2^20 bytes' worth of sectors.
		let volume_length = u64::from_le_bytes([b[72], b[73], b[74], b[75], b[76], b[77], b[78], b[79]]);
		let heap_end = cluster_heap_offset as u64 + cluster_count as u64 * sectors_per_cluster as u64;
		if volume_length < heap_end || volume_length < (1 << 20) / bytes_per_sector as u64 {
			return None;
		}
		// TexFAT is out of scope, and refusing only its second-FAT-active flag was half a rule: a
		// volume declaring two FATs with the first active mounted, and the geometry then recorded
		// `num_fats: 1`, so the second was neither read nor maintained - this driver writing to a
		// TexFAT volume would leave a stale copy behind for the system that does understand it.
		if num_fats != 1 {
			return None;
		}
		if u16::from_le_bytes([b[106], b[107]]) & 0x01 != 0 {
			return None;
		}
		Some(Geometry { kind: Kind::ExFat, bytes_per_sector, sectors_per_cluster, reserved_sectors: fat_offset, num_fats: 1, fat_size, root_entries: 0, root_cluster, first_data_sector: cluster_heap_offset, cluster_count, fsinfo_sector: 0, active_fat: 0, mirror: true })
	}
}

// The exFAT boot checksum: a rotating 32-bit sum over the eleven sectors from the main boot sector
// through the OEM parameters, with VolumeFlags (bytes 106-107) and PercentInUse (byte 112) skipped
// because the running system rewrites them without restamping the sum. Sector 11 holds that value
// repeated for its whole length, and every copy must agree.
fn exfat_boot_checksum_ok<D: BlockDevice>(dev: &mut D, bytes_per_sector: u32) -> bool {
	let mut sum: u32 = 0;
	let per_sector = bytes_per_sector as usize / SECTOR_SIZE;
	let mut buf = [0u8; SECTOR_SIZE];
	for block in 0..11 * per_sector {
		if !dev.read_block(block as u64, &mut buf) {
			return false;
		}
		for (i, &byte) in buf.iter().enumerate() {
			let at = block * SECTOR_SIZE + i;
			if at == 106 || at == 107 || at == 112 {
				continue;
			}
			sum = (sum >> 1) | (sum << 31);
			sum = sum.wrapping_add(byte as u32);
		}
	}
	for block in 0..per_sector {
		if !dev.read_block((11 * per_sector + block) as u64, &mut buf) {
			return false;
		}
		for chunk in buf.chunks_exact(4) {
			if u32::from_le_bytes(chunk.try_into().unwrap()) != sum {
				return false;
			}
		}
	}
	true
}
