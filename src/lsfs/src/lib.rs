//! LSFS - a small writable on-disk filesystem for LiberSystem.
//!
//! The on-disk layout is a deliberately small Unix-flavoured filesystem: a
//! superblock, a multi-block allocation bitmap, a multi-block inode table, then data
//! blocks. Directories form a tree from the root inode; inodes carry direct block
//! pointers plus a single and a double indirect pointer, so files and directories
//! grow well past one inode's worth of direct blocks. Every block pointer (in the
//! inode and in the indirect blocks) is paired with a CRC32C of the block it points
//! at, so on-disk corruption is caught when the block is read. It backs the
//! `Storage.Volume` API and survives a reboot.
//!
//! All I/O goes through the [`BlockDevice`] trait (one fixed-size block at a time),
//! so the same code drives a real virtio-blk disk in StorageService and a
//! `Vec`-backed device in the host tests. The crate is `no_std` for the userspace
//! build and pulls in `std` only under `cargo test` so it can be exercised on the
//! host.
//!
//! ## Crash integrity (ordered writes)
//!
//! Mutations flush blocks in an order that keeps the filesystem consistent across a
//! crash mid-write: file data is written before the bitmap that allocates it, the
//! bitmap before the inode that points at it, and the inode before the directory
//! entry that names it; on delete the directory entry is cleared before the inode
//! and blocks it referenced are freed. A crash between steps can only leak blocks or
//! an inode (an orphan), never expose a dangling reference or corrupt live data. A
//! later [`Lsfs::fsck`] pass walks the directory tree, rebuilds the allocation bitmap
//! from the blocks live inodes actually reference, and frees any leaked blocks and
//! orphan inodes.
//!
//! ## Integrity (block checksums)
//!
//! Each block is checksummed with a CRC32C stored beside the pointer to it (in the
//! inode for direct blocks, in the indirect block for the rest). The checksum is
//! computed on write and rechecked on every read, so a flipped bit on disk surfaces
//! as [`FsError::Corrupt`] instead of silently corrupt data; `fsck` walks every live
//! data block and reports how many fail their checksum.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::collections::BTreeSet;
use alloc::vec;
use alloc::vec::Vec;

// One filesystem block. Eight 512-byte disk sectors, a page; the I/O unit of the
// BlockDevice trait.
pub const BLOCK_SIZE: usize = 4096;

// On-disk superblock magic and format version. Mount rejects anything else (a fresh
// or stale-format disk), so StorageService knows to reformat. Version 2 added nested
// directories, a multi-block inode table and bitmap, indirect blocks, and per-inode
// timestamps; version 3 pairs every block pointer with a CRC32C of the block it points
// at (in the inode and in the indirect blocks) for on-read integrity checking.
const MAGIC: [u8; 8] = *b"LSFS0001";
const VERSION: u32 = 3;

// One inode is a fixed 256-byte slot: a kind byte, a size, two timestamps, a single
// and a double indirect pointer, then DIRECT (block pointer, block CRC32C) entries. 16
// inodes fit one block.
const INODE_SIZE: usize = 256;
const INODES_PER_BLOCK: usize = BLOCK_SIZE / INODE_SIZE;
// A block reference is a (u32 block pointer, u32 CRC32C) pair: 8 bytes. The CRC covers
// the referenced block, so a flipped bit on disk is caught when the block is read.
const ENTRY_SIZE: usize = 8;
// (256 - 40) / 8 = 27 direct entries; a file's first 27 blocks (108 KiB) need no
// indirection. Beyond that the single indirect block adds PTRS_PER_BLOCK more and the
// double indirect block PTRS_PER_BLOCK^2 more.
const DIRECT: usize = (INODE_SIZE - 40) / ENTRY_SIZE;
// Block references (pointer + CRC) that fit one indirect block.
const PTRS_PER_BLOCK: usize = BLOCK_SIZE / ENTRY_SIZE;

// Inode kinds. 0 is also the "free" marker, so a freed inode reads back as Free.
const KIND_FREE: u8 = 0;
const KIND_FILE: u8 = 1;
const KIND_DIR: u8 = 2;

// How many inodes a freshly formatted volume gets: one per this many blocks, but at
// least MIN_INODES and rounded up to whole inode blocks.
const BLOCKS_PER_INODE: u32 = 4;
const MIN_INODES: u32 = 16;

// The root directory is inode 0; other inodes are allocated from 1.
const ROOT_INODE: u32 = 0;

// One directory entry is 32 bytes: a NUL-padded name then the entry's inode number. A
// free slot has an empty name (first byte NUL); 128 entries fit one block.
const DENTRY_SIZE: usize = 32;
const NAME_MAX: usize = DENTRY_SIZE - 4;
const DENTRIES_PER_BLOCK: usize = BLOCK_SIZE / DENTRY_SIZE;

// CRC32C (Castagnoli) lookup table, built at compile time. Each block's checksum is a
// CRC32C of its bytes, stored next to the pointer to it; the reflected polynomial is
// 0x82F63B78.
const CRC32C_TABLE: [u32; 256] = {
	let mut table = [0u32; 256];
	let mut i = 0;
	while i < 256 {
		let mut crc = i as u32;
		let mut j = 0;
		while j < 8 {
			let mask = (crc & 1).wrapping_neg();
			crc = (crc >> 1) ^ (0x82F6_3B78 & mask);
			j += 1;
		}
		table[i] = crc;
		i += 1;
	}
	table
};

// CRC32C of a block's bytes: computed on write, stored beside the pointer, and rechecked
// on read so a flipped bit on disk surfaces as `FsError::Corrupt` rather than bad data.
fn crc32c(data: &[u8]) -> u32 {
	let mut crc = 0xFFFF_FFFFu32;
	for &b in data {
		crc = (crc >> 8) ^ CRC32C_TABLE[((crc ^ b as u32) & 0xFF) as usize];
	}
	!crc
}

// A filesystem error. The variants map onto the `Storage.Volume` `error` enum at the
// service boundary (NotFound -> not-found, NoSpace -> again, the rest -> invalid).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
	NotFound,
	NoSpace,
	TooLong,
	Invalid,
	// A block read back with a CRC32C that did not match the one stored beside its
	// pointer: on-disk corruption, surfaced instead of returning the bad bytes.
	Corrupt,
	Io,
}

// Metadata about one path, returned by [`Lsfs::stat`]: its byte length, whether it is
// a directory, and its created / modified logical timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
	pub size: u64,
	pub is_dir: bool,
	pub ctime: u64,
	pub mtime: u64,
}

// What an [`Lsfs::fsck`] pass reclaimed: blocks that the bitmap marked allocated but
// no live inode referenced, and inodes that were allocated but named by no directory
// (orphans left by a crash mid-write); plus how many live data blocks failed their
// checksum (on-disk corruption found while walking the tree).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsckReport {
	pub reclaimed_blocks: u32,
	pub reclaimed_inodes: u32,
	pub checksum_failures: u32,
}

// A fixed-size block device: the whole filesystem is read and written one
// BLOCK_SIZE-byte block at a time, addressed by a filesystem-relative block index in
// `0..num_blocks`. Implementors map that onto their backing (disk sectors, a Vec).
pub trait BlockDevice {
	// Read block `index` into `buf` (exactly BLOCK_SIZE bytes). False on I/O failure.
	fn read_block(&mut self, index: u32, buf: &mut [u8]) -> bool;
	// Write `buf` (exactly BLOCK_SIZE bytes) to block `index`. False on I/O failure.
	fn write_block(&mut self, index: u32, buf: &[u8]) -> bool;
}

// The parsed superblock, cached in memory for the life of a mount.
#[derive(Clone, Copy)]
struct Superblock {
	num_blocks: u32,
	num_inodes: u32,
	bitmap_start: u32,
	bitmap_blocks: u32,
	inode_start: u32,
	// Size of the inode table in blocks; read back from the superblock and used by
	// fsck to bound its scan.
	#[allow(dead_code)]
	inode_blocks: u32,
	data_start: u32,
	root_inode: u32,
}

// One inode, parsed from / rendered to its 256-byte on-disk slot.
struct Inode {
	kind: u8,
	size: u64,
	ctime: u64,
	mtime: u64,
	indirect: u32,
	dindirect: u32,
	// Direct block pointers and, beside each, the CRC32C of the block it points at.
	direct: [u32; DIRECT],
	direct_crc: [u32; DIRECT],
}

impl Inode {
	fn empty(kind: u8) -> Inode {
		Inode { kind, size: 0, ctime: 0, mtime: 0, indirect: 0, dindirect: 0, direct: [0u32; DIRECT], direct_crc: [0u32; DIRECT] }
	}

	fn parse(buf: &[u8]) -> Inode {
		let mut direct = [0u32; DIRECT];
		let mut direct_crc = [0u32; DIRECT];
		for i in 0..DIRECT {
			let off = 40 + i * ENTRY_SIZE;
			direct[i] = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
			direct_crc[i] = u32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap());
		}
		Inode { kind: buf[0], size: u64::from_le_bytes(buf[8..16].try_into().unwrap()), ctime: u64::from_le_bytes(buf[16..24].try_into().unwrap()), mtime: u64::from_le_bytes(buf[24..32].try_into().unwrap()), indirect: u32::from_le_bytes(buf[32..36].try_into().unwrap()), dindirect: u32::from_le_bytes(buf[36..40].try_into().unwrap()), direct, direct_crc }
	}

	fn write(&self, buf: &mut [u8]) {
		for b in buf[..INODE_SIZE].iter_mut() {
			*b = 0;
		}
		buf[0] = self.kind;
		buf[8..16].copy_from_slice(&self.size.to_le_bytes());
		buf[16..24].copy_from_slice(&self.ctime.to_le_bytes());
		buf[24..32].copy_from_slice(&self.mtime.to_le_bytes());
		buf[32..36].copy_from_slice(&self.indirect.to_le_bytes());
		buf[36..40].copy_from_slice(&self.dindirect.to_le_bytes());
		for i in 0..DIRECT {
			let off = 40 + i * ENTRY_SIZE;
			buf[off..off + 4].copy_from_slice(&self.direct[i].to_le_bytes());
			buf[off + 4..off + 8].copy_from_slice(&self.direct_crc[i].to_le_bytes());
		}
	}

	// Number of data blocks the file's `size` occupies.
	fn nblocks(&self) -> usize {
		(self.size as usize).div_ceil(BLOCK_SIZE)
	}
}

// A mounted LSFS over a block device. Holds the superblock and the whole allocation
// bitmap in memory; inodes, directory entries, and file data are read and written on
// demand. `clock` is a logical timestamp the caller can advance (no wall clock lives
// in this crate); mutations stamp inode `mtime` from it.
pub struct Lsfs<D: BlockDevice> {
	dev: D,
	sb: Superblock,
	bitmap: Vec<u8>,
	clock: u64,
}

impl<D: BlockDevice> Lsfs<D> {
	// Format `dev` as a fresh, empty LSFS spanning `num_blocks` blocks (an empty root
	// directory, no files), then return it mounted. The inode table and bitmap scale
	// with the volume. The device must hold the metadata blocks plus one data block.
	pub fn format(mut dev: D, num_blocks: u32) -> Result<Lsfs<D>, FsError> {
		// scale the inode count with the volume, rounded up to whole inode blocks.
		let want_inodes = (num_blocks / BLOCKS_PER_INODE).max(MIN_INODES);
		let inode_blocks = (want_inodes as usize * INODE_SIZE).div_ceil(BLOCK_SIZE) as u32;
		let num_inodes = inode_blocks * INODES_PER_BLOCK as u32;
		// one bitmap bit per block; size the bitmap to cover the whole volume.
		let bitmap_blocks = num_blocks.div_ceil((BLOCK_SIZE * 8) as u32);
		let bitmap_start = 1u32;
		let inode_start = bitmap_start + bitmap_blocks;
		let data_start = inode_start + inode_blocks;
		if num_blocks <= data_start {
			return Err(FsError::Invalid);
		}
		let sb = Superblock { num_blocks, num_inodes, bitmap_start, bitmap_blocks, inode_start, inode_blocks, data_start, root_inode: ROOT_INODE };

		// superblock
		let mut block = vec![0u8; BLOCK_SIZE];
		block[0..8].copy_from_slice(&MAGIC);
		block[8..12].copy_from_slice(&VERSION.to_le_bytes());
		block[12..16].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
		block[16..20].copy_from_slice(&num_blocks.to_le_bytes());
		block[20..24].copy_from_slice(&num_inodes.to_le_bytes());
		block[24..28].copy_from_slice(&bitmap_start.to_le_bytes());
		block[28..32].copy_from_slice(&bitmap_blocks.to_le_bytes());
		block[32..36].copy_from_slice(&inode_start.to_le_bytes());
		block[36..40].copy_from_slice(&inode_blocks.to_le_bytes());
		block[40..44].copy_from_slice(&data_start.to_le_bytes());
		block[44..48].copy_from_slice(&ROOT_INODE.to_le_bytes());
		if !dev.write_block(0, &block) {
			return Err(FsError::Io);
		}

		// allocation bitmap: the metadata blocks (0..data_start) are allocated, the
		// rest free. The bitmap spans bitmap_blocks blocks.
		let mut bitmap = vec![0u8; bitmap_blocks as usize * BLOCK_SIZE];
		for b in 0..data_start {
			bitmap[(b / 8) as usize] |= 1 << (b % 8);
		}
		for i in 0..bitmap_blocks {
			let off = i as usize * BLOCK_SIZE;
			if !dev.write_block(bitmap_start + i, &bitmap[off..off + BLOCK_SIZE]) {
				return Err(FsError::Io);
			}
		}

		// zero the inode table, then write the root directory inode (an empty dir).
		let zero = vec![0u8; BLOCK_SIZE];
		for b in inode_start..data_start {
			if !dev.write_block(b, &zero) {
				return Err(FsError::Io);
			}
		}
		let mut fs = Lsfs { dev, sb, bitmap, clock: 0 };
		fs.write_inode(ROOT_INODE, &Inode::empty(KIND_DIR))?;
		Ok(fs)
	}

	// Mount an existing LSFS on `dev`. Returns None if the superblock magic, version,
	// or block size does not match (an unformatted or foreign disk).
	pub fn mount(mut dev: D) -> Option<Lsfs<D>> {
		let mut block = vec![0u8; BLOCK_SIZE];
		if !dev.read_block(0, &mut block) {
			return None;
		}
		if block[0..8] != MAGIC {
			return None;
		}
		if u32::from_le_bytes(block[8..12].try_into().ok()?) != VERSION {
			return None;
		}
		if u32::from_le_bytes(block[12..16].try_into().ok()?) as usize != BLOCK_SIZE {
			return None;
		}
		let sb = Superblock { num_blocks: u32::from_le_bytes(block[16..20].try_into().ok()?), num_inodes: u32::from_le_bytes(block[20..24].try_into().ok()?), bitmap_start: u32::from_le_bytes(block[24..28].try_into().ok()?), bitmap_blocks: u32::from_le_bytes(block[28..32].try_into().ok()?), inode_start: u32::from_le_bytes(block[32..36].try_into().ok()?), inode_blocks: u32::from_le_bytes(block[36..40].try_into().ok()?), data_start: u32::from_le_bytes(block[40..44].try_into().ok()?), root_inode: u32::from_le_bytes(block[44..48].try_into().ok()?) };
		let mut bitmap = vec![0u8; sb.bitmap_blocks as usize * BLOCK_SIZE];
		for i in 0..sb.bitmap_blocks {
			let off = i as usize * BLOCK_SIZE;
			if !dev.read_block(sb.bitmap_start + i, &mut bitmap[off..off + BLOCK_SIZE]) {
				return None;
			}
		}
		Some(Lsfs { dev, sb, bitmap, clock: 0 })
	}

	// Resolve a path to its inode number, or None if any segment is missing.
	pub fn lookup(&mut self, path: &[u8]) -> Option<u32> {
		self.resolve(path).ok()
	}

	// Read the whole file at `path` into a freshly allocated buffer.
	pub fn read_file(&mut self, path: &[u8]) -> Result<Vec<u8>, FsError> {
		let inode_num = self.resolve(path)?;
		let inode = self.read_inode(inode_num)?;
		if inode.kind != KIND_FILE {
			return Err(FsError::NotFound);
		}
		let mut out = Vec::with_capacity(inode.size as usize);
		let mut block = vec![0u8; BLOCK_SIZE];
		let mut remaining = inode.size as usize;
		for i in 0..inode.nblocks() {
			// a hole (a sparse gap left by a write past the end) reads back as zeros;
			// a mapped block is verified against its stored checksum.
			if !self.read_logical(&inode, i, &mut block)? {
				for b in block.iter_mut() {
					*b = 0;
				}
			}
			let take = remaining.min(BLOCK_SIZE);
			out.extend_from_slice(&block[..take]);
			remaining -= take;
		}
		Ok(out)
	}

	// List the root directory as (name, size) pairs, one per live entry.
	pub fn list(&mut self) -> Result<Vec<(Vec<u8>, u64)>, FsError> {
		self.read_dir_inode(self.sb.root_inode)
	}

	// List the directory at `path` as (name, size) pairs.
	pub fn read_dir(&mut self, path: &[u8]) -> Result<Vec<(Vec<u8>, u64)>, FsError> {
		let inode_num = self.resolve(path)?;
		if self.read_inode(inode_num)?.kind != KIND_DIR {
			return Err(FsError::Invalid);
		}
		self.read_dir_inode(inode_num)
	}

	// Create the directory at `path`, plus any missing parents (mkdir -p). Succeeds if
	// it already exists as a directory.
	pub fn mkdir(&mut self, path: &[u8]) -> Result<(), FsError> {
		let segs = split_segments(path)?;
		let mut parent = self.sb.root_inode;
		for seg in segs {
			parent = self.dir_lookup_or_create(parent, seg)?;
		}
		Ok(())
	}

	// Create or overwrite the file at `path` with `data` (create-or-truncate). Missing
	// parent directories are created. The new data and indirect blocks are written and
	// the inode pointed at them before the old contents are freed, so a crash leaves
	// either the previous file or the new one intact.
	pub fn write_file(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
		let (parent, name) = self.resolve_parent(path, true)?;
		let existing = self.dir_find_in(parent, name);
		let old = match existing {
			Some((num, _, _)) => {
				let inode = self.read_inode(num)?;
				if inode.kind != KIND_FILE {
					return Err(FsError::Invalid);
				}
				Some((num, inode))
			}
			None => None,
		};
		let inode_num = match &old {
			Some((num, _)) => *num,
			None => self.alloc_inode()?,
		};

		// build the new inode, allocating fresh blocks (the old ones stay reserved
		// until the inode points away from them).
		let mut inode = Inode::empty(KIND_FILE);
		inode.size = data.len() as u64;
		inode.ctime = match &old {
			Some((_, o)) => o.ctime,
			None => self.clock,
		};
		inode.mtime = self.clock;
		let mut block = vec![0u8; BLOCK_SIZE];
		for i in 0..inode.nblocks() {
			let start = i * BLOCK_SIZE;
			let end = (start + BLOCK_SIZE).min(data.len());
			for b in block.iter_mut() {
				*b = 0;
			}
			block[..end - start].copy_from_slice(&data[start..end]);
			self.write_logical(&mut inode, i, &block)?;
		}

		// point the inode at the new blocks, then name it (new files only).
		self.write_inode(inode_num, &inode)?;
		if old.is_none() {
			self.dir_add(parent, name, inode_num)?;
		}

		// free the previous contents (now unreferenced).
		if let Some((_, o)) = old {
			self.free_blocks(&o)?;
		}
		Ok(())
	}

	// Delete the file or empty directory at `path`. The directory entry is cleared
	// first, so a crash leaves at worst an orphaned inode, never a dangling name.
	pub fn remove(&mut self, path: &[u8]) -> Result<(), FsError> {
		let (parent, name) = self.resolve_parent(path, false)?;
		let inode_num = self.dir_find_in(parent, name).ok_or(FsError::NotFound)?.0;
		let inode = self.read_inode(inode_num)?;
		if inode.kind == KIND_DIR && !self.read_dir_inode(inode_num)?.is_empty() {
			return Err(FsError::Invalid);
		}

		// 1. clear the directory entry.
		self.dir_clear(parent, name)?;
		// 2. free the inode, then 3. free its blocks.
		self.write_inode(inode_num, &Inode::empty(KIND_FREE))?;
		self.free_blocks(&inode)?;
		Ok(())
	}

	// Recover the device, consuming the filesystem.
	pub fn into_device(self) -> D {
		self.dev
	}

	// metadata and timestamps

	// Advance the logical clock the filesystem stamps onto inode `mtime` (and `ctime`
	// for new files). The caller injects a real time source; there is no wall clock in
	// this crate.
	pub fn set_clock(&mut self, now: u64) {
		self.clock = now;
	}

	// Return metadata for the file or directory at `path`.
	pub fn stat(&mut self, path: &[u8]) -> Result<Stat, FsError> {
		let inode_num = self.resolve(path)?;
		let inode = self.read_inode(inode_num)?;
		Ok(Stat { size: inode.size, is_dir: inode.kind == KIND_DIR, ctime: inode.ctime, mtime: inode.mtime })
	}

	// offset / partial reads and writes

	// Read up to `len` bytes of the file at `path` starting at byte `offset`. Returns
	// fewer bytes (or none) if the range runs past the end; holes read back as zeros.
	pub fn read_at(&mut self, path: &[u8], offset: u64, len: usize) -> Result<Vec<u8>, FsError> {
		let inode_num = self.resolve(path)?;
		let inode = self.read_inode(inode_num)?;
		if inode.kind != KIND_FILE {
			return Err(FsError::NotFound);
		}
		if offset >= inode.size || len == 0 {
			return Ok(Vec::new());
		}
		let end = (offset + len as u64).min(inode.size);
		let mut out = Vec::with_capacity((end - offset) as usize);
		let mut buf = vec![0u8; BLOCK_SIZE];
		let first = (offset / BLOCK_SIZE as u64) as usize;
		let last = ((end - 1) / BLOCK_SIZE as u64) as usize;
		for lb in first..=last {
			let block_start = lb as u64 * BLOCK_SIZE as u64;
			if !self.read_logical(&inode, lb, &mut buf)? {
				for b in buf.iter_mut() {
					*b = 0;
				}
			}
			let copy_start = offset.max(block_start);
			let copy_end = end.min(block_start + BLOCK_SIZE as u64);
			out.extend_from_slice(&buf[(copy_start - block_start) as usize..(copy_end - block_start) as usize]);
		}
		Ok(out)
	}

	// Write `data` into the file at `path` starting at byte `offset`, creating the file
	// (and any missing parents) if needed and extending it if the write runs past the
	// end. A gap between the old end and `offset` becomes a hole that reads as zeros.
	// Only the touched blocks are rewritten - the rest of the file is left in place.
	pub fn write_at(&mut self, path: &[u8], offset: u64, data: &[u8]) -> Result<(), FsError> {
		let (parent, name) = self.resolve_parent(path, true)?;
		let inode_num = match self.dir_find_in(parent, name) {
			Some((num, _, _)) => {
				if self.read_inode(num)?.kind != KIND_FILE {
					return Err(FsError::Invalid);
				}
				num
			}
			None => {
				let num = self.alloc_inode()?;
				let mut f = Inode::empty(KIND_FILE);
				f.ctime = self.clock;
				f.mtime = self.clock;
				self.write_inode(num, &f)?;
				self.dir_add(parent, name, num)?;
				num
			}
		};
		let mut inode = self.read_inode(inode_num)?;
		if !data.is_empty() {
			let start = offset;
			let end = offset + data.len() as u64;
			let first = (start / BLOCK_SIZE as u64) as usize;
			let last = ((end - 1) / BLOCK_SIZE as u64) as usize;
			let mut buf = vec![0u8; BLOCK_SIZE];
			for lb in first..=last {
				let block_start = lb as u64 * BLOCK_SIZE as u64;
				let full = start <= block_start && end >= block_start + BLOCK_SIZE as u64;
				// a full-block overwrite needs no read; a partial one preserves whatever
				// is there (zeros for a hole or a block past the old end).
				if full || !self.read_logical(&inode, lb, &mut buf)? {
					for b in buf.iter_mut() {
						*b = 0;
					}
				}
				let copy_start = start.max(block_start);
				let copy_end = end.min(block_start + BLOCK_SIZE as u64);
				let buf_off = (copy_start - block_start) as usize;
				let data_off = (copy_start - start) as usize;
				let n = (copy_end - copy_start) as usize;
				buf[buf_off..buf_off + n].copy_from_slice(&data[data_off..data_off + n]);
				self.write_logical(&mut inode, lb, &buf)?;
			}
			if end > inode.size {
				inode.size = end;
			}
		}
		inode.mtime = self.clock;
		self.write_inode(inode_num, &inode)?;
		Ok(())
	}

	// Append `data` to the end of the file at `path` (creating it if needed).
	pub fn append(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
		let size = match self.resolve(path) {
			Ok(num) => self.read_inode(num)?.size,
			Err(FsError::NotFound) => 0,
			Err(e) => return Err(e),
		};
		self.write_at(path, size, data)
	}

	// Resize the file at `path` to `new_len`: shrinking frees the blocks past the new
	// end, growing leaves a hole (which reads as zeros). Leaked indirect blocks from a
	// partial shrink are reclaimed by the next `fsck`.
	pub fn truncate(&mut self, path: &[u8], new_len: u64) -> Result<(), FsError> {
		let inode_num = self.resolve(path)?;
		let mut inode = self.read_inode(inode_num)?;
		if inode.kind != KIND_FILE {
			return Err(FsError::Invalid);
		}
		if new_len < inode.size {
			let keep = (new_len as usize).div_ceil(BLOCK_SIZE);
			self.free_from(&mut inode, keep)?;
			// zero the slack past the new end in the last kept block, so that a later
			// grow back over it reads zeros rather than the discarded tail.
			let tail = (new_len % BLOCK_SIZE as u64) as usize;
			if tail != 0 {
				let lb = (new_len / BLOCK_SIZE as u64) as usize;
				let mut buf = vec![0u8; BLOCK_SIZE];
				if self.read_logical(&inode, lb, &mut buf)? {
					for b in buf[tail..].iter_mut() {
						*b = 0;
					}
					// rewriting the block refreshes its stored checksum too.
					self.write_logical(&mut inode, lb, &buf)?;
				}
			}
		}
		inode.size = new_len;
		inode.mtime = self.clock;
		self.write_inode(inode_num, &inode)?;
		Ok(())
	}

	// rename / move within the volume

	// Move the file or directory at `from` to `to` within the same volume. Missing
	// parent directories of `to` are created. An existing file (or empty directory) at
	// `to` is replaced. The destination entry is written before the source entry is
	// cleared, so a crash leaves the object reachable under at least one name - never
	// lost. Moving a directory into its own subtree is rejected.
	pub fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<(), FsError> {
		let (pf, nf) = self.resolve_parent(from, false)?;
		let inode_f = self.dir_find_in(pf, nf).ok_or(FsError::NotFound)?.0;
		let from_inode = self.read_inode(inode_f)?;
		let (pt, nt) = self.resolve_parent(to, true)?;

		// a directory may not move into itself or one of its descendants.
		if from_inode.kind == KIND_DIR && self.subtree_contains(inode_f, pt)? {
			return Err(FsError::Invalid);
		}

		let dest = self.dir_find_in(pt, nt).map(|(num, _, _)| num);
		if let Some(inode_t) = dest {
			if inode_t == inode_f {
				return Ok(());
			}
			let ti = self.read_inode(inode_t)?;
			if ti.kind == KIND_DIR && !self.read_dir_inode(inode_t)?.is_empty() {
				return Err(FsError::Invalid);
			}
		}

		// 1. point the destination name at the moved inode (add or overwrite).
		self.dir_set(pt, nt, inode_f)?;
		// 2. clear the source entry.
		self.dir_clear(pf, nf)?;
		// 3. free the inode the destination name used to hold, if any and distinct.
		if let Some(inode_t) = dest {
			if inode_t != inode_f {
				let ti = self.read_inode(inode_t)?;
				self.write_inode(inode_t, &Inode::empty(KIND_FREE))?;
				self.free_blocks(&ti)?;
			}
		}
		Ok(())
	}

	// consistency

	// Walk the directory tree and rebuild the allocation bitmap from the blocks live
	// inodes actually reference, reclaiming blocks leaked by a crash mid-write, and
	// free any allocated-but-unreferenced (orphan) inodes. Returns what was reclaimed.
	pub fn fsck(&mut self) -> Result<FsckReport, FsError> {
		// 1. every inode reachable by name from the root.
		let mut reachable = BTreeSet::new();
		reachable.insert(self.sb.root_inode);
		self.gather_reachable(self.sb.root_inode, &mut reachable)?;

		// 2. a fresh bitmap: the metadata blocks plus the blocks of reachable inodes,
		// while checking every live data block against its stored checksum.
		let mut rebuilt = vec![0u8; self.bitmap.len()];
		for b in 0..self.sb.data_start {
			set_bit(&mut rebuilt, b);
		}
		let mut checksum_failures = 0;
		for &num in &reachable {
			let inode = self.read_inode(num)?;
			self.collect_inode_blocks(&inode, &mut rebuilt)?;
			checksum_failures += self.count_corrupt(&inode)?;
		}

		// 3. reclaim orphan inodes (allocated but named by no directory).
		let mut reclaimed_inodes = 0;
		for num in 1..self.sb.num_inodes {
			if self.read_inode(num)?.kind != KIND_FREE && !reachable.contains(&num) {
				self.write_inode(num, &Inode::empty(KIND_FREE))?;
				reclaimed_inodes += 1;
			}
		}

		// 4. install the rebuilt bitmap, counting the blocks it freed.
		let before = self.count_alloc(&self.bitmap);
		let after = self.count_alloc(&rebuilt);
		self.bitmap = rebuilt;
		self.flush_bitmap()?;
		Ok(FsckReport { reclaimed_blocks: before - after, reclaimed_inodes, checksum_failures })
	}

	// inode I/O

	fn inode_location(&self, num: u32) -> (u32, usize) {
		let block = self.sb.inode_start + num / INODES_PER_BLOCK as u32;
		let offset = (num as usize % INODES_PER_BLOCK) * INODE_SIZE;
		(block, offset)
	}

	fn read_inode(&mut self, num: u32) -> Result<Inode, FsError> {
		if num >= self.sb.num_inodes {
			return Err(FsError::Invalid);
		}
		let (block_idx, offset) = self.inode_location(num);
		let mut block = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(block_idx, &mut block) {
			return Err(FsError::Io);
		}
		Ok(Inode::parse(&block[offset..offset + INODE_SIZE]))
	}

	fn write_inode(&mut self, num: u32, inode: &Inode) -> Result<(), FsError> {
		if num >= self.sb.num_inodes {
			return Err(FsError::Invalid);
		}
		let (block_idx, offset) = self.inode_location(num);
		let mut block = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(block_idx, &mut block) {
			return Err(FsError::Io);
		}
		inode.write(&mut block[offset..offset + INODE_SIZE]);
		if !self.dev.write_block(block_idx, &block) {
			return Err(FsError::Io);
		}
		Ok(())
	}

	// Find a free inode slot (1..num_inodes), claim it as an empty file, and return
	// its number.
	fn alloc_inode(&mut self) -> Result<u32, FsError> {
		for num in 1..self.sb.num_inodes {
			if self.read_inode(num)?.kind == KIND_FREE {
				self.write_inode(num, &Inode::empty(KIND_FILE))?;
				return Ok(num);
			}
		}
		Err(FsError::NoSpace)
	}

	// block bitmap

	fn is_alloc(&self, block: u32) -> bool {
		self.bitmap[(block / 8) as usize] & (1 << (block % 8)) != 0
	}

	fn mark(&mut self, block: u32, allocated: bool) {
		let byte = (block / 8) as usize;
		let bit = 1u8 << (block % 8);
		if allocated {
			self.bitmap[byte] |= bit;
		} else {
			self.bitmap[byte] &= !bit;
		}
	}

	fn flush_bitmap(&mut self) -> Result<(), FsError> {
		for i in 0..self.sb.bitmap_blocks {
			let off = i as usize * BLOCK_SIZE;
			let blk = self.sb.bitmap_start + i;
			let buf = self.bitmap[off..off + BLOCK_SIZE].to_vec();
			if !self.dev.write_block(blk, &buf) {
				return Err(FsError::Io);
			}
		}
		Ok(())
	}

	// Find `n` free data blocks without claiming them, in ascending order.
	fn find_free_blocks(&self, n: usize) -> Result<Vec<u32>, FsError> {
		let mut found = Vec::with_capacity(n);
		if n == 0 {
			return Ok(found);
		}
		for block in self.sb.data_start..self.sb.num_blocks {
			if !self.is_alloc(block) {
				found.push(block);
				if found.len() == n {
					return Ok(found);
				}
			}
		}
		Err(FsError::NoSpace)
	}

	// Allocate one data block (find + claim + flush), returning its index.
	fn alloc_one(&mut self) -> Result<u32, FsError> {
		let block = self.find_free_blocks(1)?[0];
		self.mark(block, true);
		self.flush_bitmap()?;
		Ok(block)
	}

	// Allocate one block and zero its contents - for indirect and directory blocks,
	// whose unused slots must read back as zero.
	fn alloc_zeroed(&mut self) -> Result<u32, FsError> {
		let block = self.alloc_one()?;
		let zero = vec![0u8; BLOCK_SIZE];
		if !self.dev.write_block(block, &zero) {
			return Err(FsError::Io);
		}
		Ok(block)
	}

	// block references: a (pointer, CRC32C) pair at index `idx` in an indirect block

	fn read_entry(&mut self, block: u32, idx: usize) -> Result<(u32, u32), FsError> {
		let mut buf = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(block, &mut buf) {
			return Err(FsError::Io);
		}
		let off = idx * ENTRY_SIZE;
		let ptr = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
		let crc = u32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap());
		Ok((ptr, crc))
	}

	fn write_entry_at(&mut self, block: u32, idx: usize, ptr: u32, crc: u32) -> Result<(), FsError> {
		let mut buf = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(block, &mut buf) {
			return Err(FsError::Io);
		}
		let off = idx * ENTRY_SIZE;
		buf[off..off + 4].copy_from_slice(&ptr.to_le_bytes());
		buf[off + 4..off + 8].copy_from_slice(&crc.to_le_bytes());
		if !self.dev.write_block(block, &buf) {
			return Err(FsError::Io);
		}
		Ok(())
	}

	// file block mapping (direct -> single indirect -> double indirect)

	// Resolve logical block `logical` of `inode` to (physical block, stored CRC32C), or
	// None if it is not mapped (a hole).
	fn map_for_read(&mut self, inode: &Inode, logical: usize) -> Result<Option<(u32, u32)>, FsError> {
		if logical < DIRECT {
			return Ok(nonzero(inode.direct[logical]).map(|p| (p, inode.direct_crc[logical])));
		}
		let l = logical - DIRECT;
		if l < PTRS_PER_BLOCK {
			if inode.indirect == 0 {
				return Ok(None);
			}
			let (ptr, crc) = self.read_entry(inode.indirect, l)?;
			return Ok(nonzero(ptr).map(|p| (p, crc)));
		}
		let d = l - PTRS_PER_BLOCK;
		let first = d / PTRS_PER_BLOCK;
		let second = d % PTRS_PER_BLOCK;
		if first >= PTRS_PER_BLOCK {
			return Err(FsError::NoSpace);
		}
		if inode.dindirect == 0 {
			return Ok(None);
		}
		let (mid, _) = self.read_entry(inode.dindirect, first)?;
		if mid == 0 {
			return Ok(None);
		}
		let (ptr, crc) = self.read_entry(mid, second)?;
		Ok(nonzero(ptr).map(|p| (p, crc)))
	}

	// Ensure logical block `logical` of `inode` is mapped, allocating the data block
	// (and any indirect blocks on the way) if needed, and record `crc` beside its
	// pointer. Returns the physical block. Updates `inode`'s pointers in memory - the
	// caller persists the inode.
	fn map_for_write(&mut self, inode: &mut Inode, logical: usize, crc: u32) -> Result<u32, FsError> {
		if logical < DIRECT {
			if inode.direct[logical] == 0 {
				inode.direct[logical] = self.alloc_one()?;
			}
			inode.direct_crc[logical] = crc;
			return Ok(inode.direct[logical]);
		}
		let l = logical - DIRECT;
		if l < PTRS_PER_BLOCK {
			if inode.indirect == 0 {
				inode.indirect = self.alloc_zeroed()?;
			}
			let (mut data, _) = self.read_entry(inode.indirect, l)?;
			if data == 0 {
				data = self.alloc_one()?;
			}
			self.write_entry_at(inode.indirect, l, data, crc)?;
			return Ok(data);
		}
		let d = l - PTRS_PER_BLOCK;
		let first = d / PTRS_PER_BLOCK;
		let second = d % PTRS_PER_BLOCK;
		if first >= PTRS_PER_BLOCK {
			return Err(FsError::NoSpace);
		}
		if inode.dindirect == 0 {
			inode.dindirect = self.alloc_zeroed()?;
		}
		let (mut mid, _) = self.read_entry(inode.dindirect, first)?;
		if mid == 0 {
			mid = self.alloc_zeroed()?;
			// the double indirect points at a mid block, not data: no data CRC here.
			self.write_entry_at(inode.dindirect, first, mid, 0)?;
		}
		let (mut data, _) = self.read_entry(mid, second)?;
		if data == 0 {
			data = self.alloc_one()?;
		}
		self.write_entry_at(mid, second, data, crc)?;
		Ok(data)
	}

	// Read logical block `logical` of `inode` into `buf`, verifying its stored
	// checksum. Returns false (and leaves `buf` untouched) for a hole; a mismatch is
	// `FsError::Corrupt`.
	fn read_logical(&mut self, inode: &Inode, logical: usize, buf: &mut [u8]) -> Result<bool, FsError> {
		match self.map_for_read(inode, logical)? {
			Some((phys, crc)) => {
				if !self.dev.read_block(phys, buf) {
					return Err(FsError::Io);
				}
				if crc32c(buf) != crc {
					return Err(FsError::Corrupt);
				}
				Ok(true)
			}
			None => Ok(false),
		}
	}

	// Write `buf` as logical block `logical` of `inode`, allocating the block if needed
	// and recording its checksum. Updates `inode` in memory - the caller persists it.
	fn write_logical(&mut self, inode: &mut Inode, logical: usize, buf: &[u8]) -> Result<(), FsError> {
		let crc = crc32c(buf);
		let phys = self.map_for_write(inode, logical, crc)?;
		if !self.dev.write_block(phys, buf) {
			return Err(FsError::Io);
		}
		Ok(())
	}

	// Count the live data blocks of `inode` whose on-disk bytes no longer match the
	// checksum stored beside their pointer.
	fn count_corrupt(&mut self, inode: &Inode) -> Result<u32, FsError> {
		let mut bad = 0;
		let mut buf = vec![0u8; BLOCK_SIZE];
		for lb in 0..inode.nblocks() {
			if let Some((phys, crc)) = self.map_for_read(inode, lb)? {
				if !self.dev.read_block(phys, &mut buf) {
					return Err(FsError::Io);
				}
				if crc32c(&buf) != crc {
					bad += 1;
				}
			}
		}
		Ok(bad)
	}

	// Free every block an inode references - its data blocks and the indirect blocks
	// that map them - then flush the bitmap.
	fn free_blocks(&mut self, inode: &Inode) -> Result<(), FsError> {
		for i in 0..DIRECT {
			if inode.direct[i] != 0 {
				self.mark(inode.direct[i], false);
			}
		}
		if inode.indirect != 0 {
			for idx in 0..PTRS_PER_BLOCK {
				let (p, _) = self.read_entry(inode.indirect, idx)?;
				if p != 0 {
					self.mark(p, false);
				}
			}
			self.mark(inode.indirect, false);
		}
		if inode.dindirect != 0 {
			for first in 0..PTRS_PER_BLOCK {
				let (mid, _) = self.read_entry(inode.dindirect, first)?;
				if mid == 0 {
					continue;
				}
				for second in 0..PTRS_PER_BLOCK {
					let (p, _) = self.read_entry(mid, second)?;
					if p != 0 {
						self.mark(p, false);
					}
				}
				self.mark(mid, false);
			}
			self.mark(inode.dindirect, false);
		}
		self.flush_bitmap()
	}

	// path resolution

	// Resolve a full path to its inode number, walking directories from the root.
	fn resolve(&mut self, path: &[u8]) -> Result<u32, FsError> {
		let segs = split_segments(path)?;
		let mut inode_num = self.sb.root_inode;
		for seg in segs {
			if self.read_inode(inode_num)?.kind != KIND_DIR {
				return Err(FsError::NotFound);
			}
			inode_num = self.dir_find_in(inode_num, seg).ok_or(FsError::NotFound)?.0;
		}
		Ok(inode_num)
	}

	// Resolve a path to (the parent directory inode, the final segment). With
	// `create`, missing parent directories are created (mkdir -p); without it, a
	// missing parent is an error.
	fn resolve_parent<'a>(&mut self, path: &'a [u8], create: bool) -> Result<(u32, &'a [u8]), FsError> {
		let segs = split_segments(path)?;
		let last: &'a [u8] = segs[segs.len() - 1];
		let mut parent = self.sb.root_inode;
		for &seg in &segs[..segs.len() - 1] {
			parent = if create {
				self.dir_lookup_or_create(parent, seg)?
			} else {
				let child = self.dir_find_in(parent, seg).ok_or(FsError::NotFound)?.0;
				if self.read_inode(child)?.kind != KIND_DIR {
					return Err(FsError::Invalid);
				}
				child
			};
		}
		Ok((parent, last))
	}

	// Find child `name` in `parent`, or create it as a directory; return its inode.
	fn dir_lookup_or_create(&mut self, parent: u32, name: &[u8]) -> Result<u32, FsError> {
		if let Some((child, _, _)) = self.dir_find_in(parent, name) {
			if self.read_inode(child)?.kind != KIND_DIR {
				return Err(FsError::Invalid);
			}
			return Ok(child);
		}
		let num = self.alloc_inode()?;
		let mut dir = Inode::empty(KIND_DIR);
		dir.ctime = self.clock;
		dir.mtime = self.clock;
		self.write_inode(num, &dir)?;
		self.dir_add(parent, name, num)?;
		Ok(num)
	}

	// directory operations (on any directory inode)

	// Scan directory `dir_num` for `name`, returning (child inode, the logical
	// directory block holding the entry, the entry's byte offset in that block).
	fn dir_find_in(&mut self, dir_num: u32, name: &[u8]) -> Option<(u32, usize, usize)> {
		let dir = self.read_inode(dir_num).ok()?;
		let mut block = vec![0u8; BLOCK_SIZE];
		for i in 0..dir.nblocks() {
			if !self.read_logical(&dir, i, &mut block).ok()? {
				continue;
			}
			for slot in 0..DENTRIES_PER_BLOCK {
				let off = slot * DENTRY_SIZE;
				if block[off] == 0 {
					continue;
				}
				if entry_name(&block[off..off + DENTRY_SIZE]) == name {
					let inode = u32::from_le_bytes(block[off + NAME_MAX..off + NAME_MAX + 4].try_into().ok()?);
					return Some((inode, i, off));
				}
			}
		}
		None
	}

	// Collect every live (name, inode) entry in directory `dir_num`.
	fn dir_entries_of(&mut self, dir_num: u32) -> Result<Vec<(Vec<u8>, u32)>, FsError> {
		let dir = self.read_inode(dir_num)?;
		let mut out = Vec::new();
		let mut block = vec![0u8; BLOCK_SIZE];
		for i in 0..dir.nblocks() {
			if !self.read_logical(&dir, i, &mut block)? {
				continue;
			}
			for slot in 0..DENTRIES_PER_BLOCK {
				let off = slot * DENTRY_SIZE;
				if block[off] == 0 {
					continue;
				}
				let inode = u32::from_le_bytes(block[off + NAME_MAX..off + NAME_MAX + 4].try_into().unwrap());
				out.push((entry_name(&block[off..off + DENTRY_SIZE]).to_vec(), inode));
			}
		}
		Ok(out)
	}

	// List directory `dir_num` as (name, size) pairs.
	fn read_dir_inode(&mut self, dir_num: u32) -> Result<Vec<(Vec<u8>, u64)>, FsError> {
		let mut out = Vec::new();
		for (name, inode_num) in self.dir_entries_of(dir_num)? {
			let size = self.read_inode(inode_num)?.size;
			out.push((name, size));
		}
		Ok(out)
	}

	// Add a (name, inode) entry to directory `dir_num`, reusing a free slot or growing
	// the directory by one block.
	fn dir_add(&mut self, dir_num: u32, name: &[u8], child: u32) -> Result<(), FsError> {
		let mut dir = self.read_inode(dir_num)?;
		let mut block = vec![0u8; BLOCK_SIZE];

		// reuse a free slot in an existing directory block.
		for i in 0..dir.nblocks() {
			if !self.read_logical(&dir, i, &mut block)? {
				return Err(FsError::Io);
			}
			for slot in 0..DENTRIES_PER_BLOCK {
				let off = slot * DENTRY_SIZE;
				if block[off] == 0 {
					write_entry(&mut block[off..off + DENTRY_SIZE], name, child);
					self.write_logical(&mut dir, i, &block)?;
					dir.mtime = self.clock;
					self.write_inode(dir_num, &dir)?;
					return Ok(());
				}
			}
		}

		// no room: grow the directory by one block.
		let logical = dir.nblocks();
		for b in block.iter_mut() {
			*b = 0;
		}
		write_entry(&mut block[0..DENTRY_SIZE], name, child);
		self.write_logical(&mut dir, logical, &block)?;
		dir.size += BLOCK_SIZE as u64;
		dir.mtime = self.clock;
		self.write_inode(dir_num, &dir)?;
		Ok(())
	}

	// Point an existing entry `name` in directory `dir_num` at `child`, or add it if it
	// is not there yet.
	fn dir_set(&mut self, dir_num: u32, name: &[u8], child: u32) -> Result<(), FsError> {
		if let Some((_, logical, off)) = self.dir_find_in(dir_num, name) {
			let mut dir = self.read_inode(dir_num)?;
			let mut block = vec![0u8; BLOCK_SIZE];
			self.read_logical(&dir, logical, &mut block)?;
			block[off + NAME_MAX..off + NAME_MAX + 4].copy_from_slice(&child.to_le_bytes());
			self.write_logical(&mut dir, logical, &block)?;
			self.write_inode(dir_num, &dir)?;
			return Ok(());
		}
		self.dir_add(dir_num, name, child)
	}

	// Clear entry `name` from directory `dir_num` (leaving a free slot).
	fn dir_clear(&mut self, dir_num: u32, name: &[u8]) -> Result<(), FsError> {
		let (_, logical, off) = self.dir_find_in(dir_num, name).ok_or(FsError::NotFound)?;
		let mut dir = self.read_inode(dir_num)?;
		let mut block = vec![0u8; BLOCK_SIZE];
		self.read_logical(&dir, logical, &mut block)?;
		for b in block[off..off + DENTRY_SIZE].iter_mut() {
			*b = 0;
		}
		self.write_logical(&mut dir, logical, &block)?;
		self.write_inode(dir_num, &dir)?;
		Ok(())
	}

	// Does the subtree rooted at directory `root_dir` contain inode `target` (as the
	// directory itself or any descendant)? Used to reject moving a directory into
	// itself.
	fn subtree_contains(&mut self, root_dir: u32, target: u32) -> Result<bool, FsError> {
		if root_dir == target {
			return Ok(true);
		}
		for (_, child) in self.dir_entries_of(root_dir)? {
			if self.read_inode(child)?.kind == KIND_DIR && self.subtree_contains(child, target)? {
				return Ok(true);
			}
		}
		Ok(false)
	}

	// Free the file's data blocks from logical block `keep` to the end, clearing their
	// pointers, and free the indirect blocks that map only the freed region. A partial
	// shrink may leave a half-used indirect block allocated; `fsck` reclaims it.
	fn free_from(&mut self, inode: &mut Inode, keep: usize) -> Result<(), FsError> {
		let total = inode.nblocks();
		for lb in keep..total {
			self.clear_block_at(inode, lb)?;
		}
		if keep <= DIRECT && inode.indirect != 0 {
			self.mark(inode.indirect, false);
			inode.indirect = 0;
		}
		if keep <= DIRECT + PTRS_PER_BLOCK && inode.dindirect != 0 {
			for first in 0..PTRS_PER_BLOCK {
				let (mid, _) = self.read_entry(inode.dindirect, first)?;
				if mid != 0 {
					self.mark(mid, false);
				}
			}
			self.mark(inode.dindirect, false);
			inode.dindirect = 0;
		}
		self.flush_bitmap()
	}

	// Free the data block at logical index `lb` and clear its pointer slot (leaving any
	// indirect blocks in place).
	fn clear_block_at(&mut self, inode: &mut Inode, lb: usize) -> Result<(), FsError> {
		if lb < DIRECT {
			if inode.direct[lb] != 0 {
				self.mark(inode.direct[lb], false);
				inode.direct[lb] = 0;
				inode.direct_crc[lb] = 0;
			}
			return Ok(());
		}
		let l = lb - DIRECT;
		if l < PTRS_PER_BLOCK {
			if inode.indirect != 0 {
				let (p, _) = self.read_entry(inode.indirect, l)?;
				if p != 0 {
					self.mark(p, false);
					self.write_entry_at(inode.indirect, l, 0, 0)?;
				}
			}
			return Ok(());
		}
		let d = l - PTRS_PER_BLOCK;
		let first = d / PTRS_PER_BLOCK;
		let second = d % PTRS_PER_BLOCK;
		if inode.dindirect != 0 {
			let (mid, _) = self.read_entry(inode.dindirect, first)?;
			if mid != 0 {
				let (p, _) = self.read_entry(mid, second)?;
				if p != 0 {
					self.mark(p, false);
					self.write_entry_at(mid, second, 0, 0)?;
				}
			}
		}
		Ok(())
	}

	// Add every inode named under directory `dir` (recursively) to `set`.
	fn gather_reachable(&mut self, dir: u32, set: &mut BTreeSet<u32>) -> Result<(), FsError> {
		for (_, child) in self.dir_entries_of(dir)? {
			if set.insert(child) && self.read_inode(child)?.kind == KIND_DIR {
				self.gather_reachable(child, set)?;
			}
		}
		Ok(())
	}

	// Set the bitmap bit for every block an inode references - its data blocks and the
	// indirect blocks that map them.
	fn collect_inode_blocks(&mut self, inode: &Inode, bitmap: &mut [u8]) -> Result<(), FsError> {
		for i in 0..DIRECT {
			if inode.direct[i] != 0 {
				set_bit(bitmap, inode.direct[i]);
			}
		}
		if inode.indirect != 0 {
			set_bit(bitmap, inode.indirect);
			for idx in 0..PTRS_PER_BLOCK {
				let (p, _) = self.read_entry(inode.indirect, idx)?;
				if p != 0 {
					set_bit(bitmap, p);
				}
			}
		}
		if inode.dindirect != 0 {
			set_bit(bitmap, inode.dindirect);
			for first in 0..PTRS_PER_BLOCK {
				let (mid, _) = self.read_entry(inode.dindirect, first)?;
				if mid == 0 {
					continue;
				}
				set_bit(bitmap, mid);
				for second in 0..PTRS_PER_BLOCK {
					let (p, _) = self.read_entry(mid, second)?;
					if p != 0 {
						set_bit(bitmap, p);
					}
				}
			}
		}
		Ok(())
	}

	// Count the allocated blocks recorded in `bitmap`, within the volume.
	fn count_alloc(&self, bitmap: &[u8]) -> u32 {
		let mut n = 0;
		for b in 0..self.sb.num_blocks {
			if bitmap[(b / 8) as usize] & (1 << (b % 8)) != 0 {
				n += 1;
			}
		}
		n
	}
}

// The name held in a directory entry: the name field up to its first NUL.
fn entry_name(entry: &[u8]) -> &[u8] {
	let name = &entry[..NAME_MAX];
	match name.iter().position(|&b| b == 0) {
		Some(end) => &name[..end],
		None => name,
	}
}

// Render a directory entry: the NUL-padded name then the inode number.
fn write_entry(entry: &mut [u8], name: &[u8], inode: u32) {
	for b in entry[..DENTRY_SIZE].iter_mut() {
		*b = 0;
	}
	entry[..name.len()].copy_from_slice(name);
	entry[NAME_MAX..NAME_MAX + 4].copy_from_slice(&inode.to_le_bytes());
}

// Set the allocation bit for block `b` in `bitmap`.
fn set_bit(bitmap: &mut [u8], b: u32) {
	bitmap[(b / 8) as usize] |= 1 << (b % 8);
}

// A block pointer as an Option: 0 is the "unmapped" sentinel, anything else a real
// block.
fn nonzero(block: u32) -> Option<u32> {
	if block == 0 {
		None
	} else {
		Some(block)
	}
}

// Split a path into its validated segments. Each segment must be non-empty, no longer
// than NAME_MAX, neither "." nor "..", and free of NUL bytes - so a resolved path can
// never escape the volume or name an invalid entry.
fn split_segments(path: &[u8]) -> Result<Vec<&[u8]>, FsError> {
	if path.is_empty() {
		return Err(FsError::Invalid);
	}
	let mut segs = Vec::new();
	for seg in path.split(|&b| b == b'/') {
		if seg.is_empty() || seg == b"." || seg == b".." {
			return Err(FsError::Invalid);
		}
		if seg.len() > NAME_MAX {
			return Err(FsError::TooLong);
		}
		if seg.iter().any(|&c| c == 0) {
			return Err(FsError::Invalid);
		}
		segs.push(seg);
	}
	Ok(segs)
}

#[cfg(test)]
mod tests;
