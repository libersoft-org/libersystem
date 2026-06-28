//! LSFS - a small writable on-disk filesystem for LiberSystem.
//!
//! The on-disk layout is a deliberately small Unix-flavoured filesystem: a
//! superblock, a multi-block allocation bitmap, a multi-block inode table, then data
//! blocks. Directories form a tree from the root inode; inodes carry direct block
//! pointers plus a single and a double indirect pointer, so files and directories
//! grow well past one inode's worth of direct blocks. It backs the `Storage.Volume`
//! API and survives a reboot.
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
//! an inode (an orphan reclaimed by a future `fsck`), never expose a dangling
//! reference or corrupt live data.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

// One filesystem block. Eight 512-byte disk sectors, a page; the I/O unit of the
// BlockDevice trait.
pub const BLOCK_SIZE: usize = 4096;

// On-disk superblock magic and format version. Mount rejects anything else (a fresh
// or stale-format disk), so StorageService knows to reformat. Version 2 adds nested
// directories, a multi-block inode table and bitmap, indirect blocks, and per-inode
// timestamps.
const MAGIC: [u8; 8] = *b"LSFS0001";
const VERSION: u32 = 2;

// One inode is a fixed 256-byte slot: a kind byte, a size, two timestamps, a single
// and a double indirect pointer, then DIRECT direct block pointers. 16 inodes fit one
// block.
const INODE_SIZE: usize = 256;
const INODES_PER_BLOCK: usize = BLOCK_SIZE / INODE_SIZE;
// (256 - 40) / 4 = 54 direct pointers; a file's first 54 blocks (216 KiB) need no
// indirection. Beyond that the single indirect block adds PTRS_PER_BLOCK more and the
// double indirect block PTRS_PER_BLOCK^2 more.
const DIRECT: usize = (INODE_SIZE - 40) / 4;
// Block pointers (u32) that fit one indirect block.
const PTRS_PER_BLOCK: usize = BLOCK_SIZE / 4;

// Inode kinds. 0 is also the "free" marker, so a freed inode reads back as Free.
const KIND_FREE: u8 = 0;
const KIND_FILE: u8 = 1;
const KIND_DIR: u8 = 2;

// How many inodes a freshly formatted volume gets: one per this many blocks, but at
// least MIN_INODES and never so many the table cannot fit alongside one data block.
const BLOCKS_PER_INODE: u32 = 2;
const MIN_INODES: u32 = 16;

// The root directory is inode 0; other inodes are allocated from 1.
const ROOT_INODE: u32 = 0;

// One directory entry is 32 bytes: a NUL-padded name then the entry's inode number. A
// free slot has an empty name (first byte NUL); 128 entries fit one block.
const DENTRY_SIZE: usize = 32;
const NAME_MAX: usize = DENTRY_SIZE - 4;
const DENTRIES_PER_BLOCK: usize = BLOCK_SIZE / DENTRY_SIZE;

// A filesystem error. The variants map onto the `Storage.Volume` `error` enum at the
// service boundary (NotFound -> not-found, NoSpace -> again, the rest -> invalid).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
	NotFound,
	NoSpace,
	TooLong,
	Invalid,
	Io,
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
	direct: [u32; DIRECT],
}

impl Inode {
	fn empty(kind: u8) -> Inode {
		Inode { kind, size: 0, ctime: 0, mtime: 0, indirect: 0, dindirect: 0, direct: [0u32; DIRECT] }
	}

	fn parse(buf: &[u8]) -> Inode {
		let mut direct = [0u32; DIRECT];
		for (i, slot) in direct.iter_mut().enumerate() {
			let off = 40 + i * 4;
			*slot = u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
		}
		Inode { kind: buf[0], size: u64::from_le_bytes(buf[8..16].try_into().unwrap()), ctime: u64::from_le_bytes(buf[16..24].try_into().unwrap()), mtime: u64::from_le_bytes(buf[24..32].try_into().unwrap()), indirect: u32::from_le_bytes(buf[32..36].try_into().unwrap()), dindirect: u32::from_le_bytes(buf[36..40].try_into().unwrap()), direct }
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
		for (i, &b) in self.direct.iter().enumerate() {
			let off = 40 + i * 4;
			buf[off..off + 4].copy_from_slice(&b.to_le_bytes());
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
			let phys = self.block_map_read(&inode, i)?.ok_or(FsError::Io)?;
			if !self.dev.read_block(phys, &mut block) {
				return Err(FsError::Io);
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
			let phys = self.block_map_alloc(&mut inode, i)?;
			let start = i * BLOCK_SIZE;
			let end = (start + BLOCK_SIZE).min(data.len());
			for b in block.iter_mut() {
				*b = 0;
			}
			block[..end - start].copy_from_slice(&data[start..end]);
			if !self.dev.write_block(phys, &block) {
				return Err(FsError::Io);
			}
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
		let (inode_num, dir_block, slot) = self.dir_find_in(parent, name).ok_or(FsError::NotFound)?;
		let inode = self.read_inode(inode_num)?;
		if inode.kind == KIND_DIR && !self.read_dir_inode(inode_num)?.is_empty() {
			return Err(FsError::Invalid);
		}

		// 1. clear the directory entry.
		let mut block = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(dir_block, &mut block) {
			return Err(FsError::Io);
		}
		for b in block[slot..slot + DENTRY_SIZE].iter_mut() {
			*b = 0;
		}
		if !self.dev.write_block(dir_block, &block) {
			return Err(FsError::Io);
		}

		// 2. free the inode, then 3. free its blocks.
		self.write_inode(inode_num, &Inode::empty(KIND_FREE))?;
		self.free_blocks(&inode)?;
		Ok(())
	}

	// Recover the device, consuming the filesystem.
	pub fn into_device(self) -> D {
		self.dev
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

	// block pointers (one u32 slot inside an indirect block)

	fn read_ptr(&mut self, block: u32, idx: usize) -> Result<u32, FsError> {
		let mut buf = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(block, &mut buf) {
			return Err(FsError::Io);
		}
		let off = idx * 4;
		Ok(u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()))
	}

	fn write_ptr(&mut self, block: u32, idx: usize, val: u32) -> Result<(), FsError> {
		let mut buf = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(block, &mut buf) {
			return Err(FsError::Io);
		}
		let off = idx * 4;
		buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
		if !self.dev.write_block(block, &buf) {
			return Err(FsError::Io);
		}
		Ok(())
	}

	// file block mapping (direct -> single indirect -> double indirect)

	// Resolve logical block `logical` of `inode` to its physical block, or None if it
	// is not mapped.
	fn block_map_read(&mut self, inode: &Inode, logical: usize) -> Result<Option<u32>, FsError> {
		if logical < DIRECT {
			return Ok(nonzero(inode.direct[logical]));
		}
		let l = logical - DIRECT;
		if l < PTRS_PER_BLOCK {
			if inode.indirect == 0 {
				return Ok(None);
			}
			return Ok(nonzero(self.read_ptr(inode.indirect, l)?));
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
		let mid = self.read_ptr(inode.dindirect, first)?;
		if mid == 0 {
			return Ok(None);
		}
		Ok(nonzero(self.read_ptr(mid, second)?))
	}

	// Ensure logical block `logical` of `inode` is mapped, allocating the data block
	// (and any indirect blocks on the way) if needed; return its physical block.
	// Updates `inode`'s pointers in memory - the caller persists the inode.
	fn block_map_alloc(&mut self, inode: &mut Inode, logical: usize) -> Result<u32, FsError> {
		if logical < DIRECT {
			if inode.direct[logical] == 0 {
				inode.direct[logical] = self.alloc_one()?;
			}
			return Ok(inode.direct[logical]);
		}
		let l = logical - DIRECT;
		if l < PTRS_PER_BLOCK {
			if inode.indirect == 0 {
				inode.indirect = self.alloc_zeroed()?;
			}
			let mut data = self.read_ptr(inode.indirect, l)?;
			if data == 0 {
				data = self.alloc_one()?;
				self.write_ptr(inode.indirect, l, data)?;
			}
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
		let mut mid = self.read_ptr(inode.dindirect, first)?;
		if mid == 0 {
			mid = self.alloc_zeroed()?;
			self.write_ptr(inode.dindirect, first, mid)?;
		}
		let mut data = self.read_ptr(mid, second)?;
		if data == 0 {
			data = self.alloc_one()?;
			self.write_ptr(mid, second, data)?;
		}
		Ok(data)
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
				let p = self.read_ptr(inode.indirect, idx)?;
				if p != 0 {
					self.mark(p, false);
				}
			}
			self.mark(inode.indirect, false);
		}
		if inode.dindirect != 0 {
			for first in 0..PTRS_PER_BLOCK {
				let mid = self.read_ptr(inode.dindirect, first)?;
				if mid == 0 {
					continue;
				}
				for second in 0..PTRS_PER_BLOCK {
					let p = self.read_ptr(mid, second)?;
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

	// Scan directory `dir_num` for `name`, returning (child inode, the physical
	// directory block holding the entry, the entry's byte offset in that block).
	fn dir_find_in(&mut self, dir_num: u32, name: &[u8]) -> Option<(u32, u32, usize)> {
		let dir = self.read_inode(dir_num).ok()?;
		let mut block = vec![0u8; BLOCK_SIZE];
		for i in 0..dir.nblocks() {
			let phys = self.block_map_read(&dir, i).ok()??;
			if !self.dev.read_block(phys, &mut block) {
				return None;
			}
			for slot in 0..DENTRIES_PER_BLOCK {
				let off = slot * DENTRY_SIZE;
				if block[off] == 0 {
					continue;
				}
				if entry_name(&block[off..off + DENTRY_SIZE]) == name {
					let inode = u32::from_le_bytes(block[off + NAME_MAX..off + NAME_MAX + 4].try_into().ok()?);
					return Some((inode, phys, off));
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
			let phys = self.block_map_read(&dir, i)?.ok_or(FsError::Io)?;
			if !self.dev.read_block(phys, &mut block) {
				return Err(FsError::Io);
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
			let phys = self.block_map_read(&dir, i)?.ok_or(FsError::Io)?;
			if !self.dev.read_block(phys, &mut block) {
				return Err(FsError::Io);
			}
			for slot in 0..DENTRIES_PER_BLOCK {
				let off = slot * DENTRY_SIZE;
				if block[off] == 0 {
					write_entry(&mut block[off..off + DENTRY_SIZE], name, child);
					if !self.dev.write_block(phys, &block) {
						return Err(FsError::Io);
					}
					return Ok(());
				}
			}
		}

		// no room: grow the directory by one block.
		let logical = dir.nblocks();
		let phys = self.block_map_alloc(&mut dir, logical)?;
		for b in block.iter_mut() {
			*b = 0;
		}
		write_entry(&mut block[0..DENTRY_SIZE], name, child);
		if !self.dev.write_block(phys, &block) {
			return Err(FsError::Io);
		}
		dir.size += BLOCK_SIZE as u64;
		dir.mtime = self.clock;
		self.write_inode(dir_num, &dir)?;
		Ok(())
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
