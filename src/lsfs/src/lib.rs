//! LSFS - a simple writable on-disk filesystem for LiberSystem (phase 2).
//!
//! The on-disk layout is a deliberately small Unix-flavoured filesystem: a
//! superblock, a one-block allocation bitmap, a fixed inode table, then data
//! blocks. There is a single directory (the root); inodes carry direct block
//! pointers only. It is enough to create / write / read / delete files behind the
//! `Storage.Volume` API and survive a reboot - the modern CoW / checksum / snapshot
//! filesystem stays phase 3.
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
// or stale-format disk), so StorageService knows to reformat.
const MAGIC: [u8; 8] = *b"LSFS0001";
const VERSION: u32 = 1;

// One inode is a fixed 128 bytes: a kind byte, a size, then DIRECT direct block
// pointers. 32 inodes fit one block.
const INODE_SIZE: usize = 128;
const INODES_PER_BLOCK: usize = BLOCK_SIZE / INODE_SIZE;
const NUM_INODES: u32 = 32;
// (128 - 16) / 4 = 28 direct pointers, so a file is at most 28 blocks (112 KiB).
const DIRECT: usize = (INODE_SIZE - 16) / 4;

// Inode kinds. 0 is also the "free" marker, so a freed inode reads back as Free.
const KIND_FREE: u8 = 0;
const KIND_FILE: u8 = 1;
const KIND_DIR: u8 = 2;

// The root directory is inode 0; file inodes are allocated from 1.
const ROOT_INODE: u32 = 0;

// One directory entry is 32 bytes: a NUL-padded name then the file's inode number.
// A free slot has an empty name (first byte NUL); 128 entries fit one block.
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
	bitmap_block: u32,
	inode_start: u32,
	data_start: u32,
	root_inode: u32,
}

// One inode, parsed from / rendered to its 128-byte on-disk slot.
struct Inode {
	kind: u8,
	size: u64,
	direct: [u32; DIRECT],
}

impl Inode {
	fn empty(kind: u8) -> Inode {
		Inode { kind, size: 0, direct: [0u32; DIRECT] }
	}

	fn parse(buf: &[u8]) -> Inode {
		let mut direct = [0u32; DIRECT];
		for (i, slot) in direct.iter_mut().enumerate() {
			let off = 16 + i * 4;
			*slot = u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
		}
		Inode { kind: buf[0], size: u64::from_le_bytes(buf[8..16].try_into().unwrap()), direct }
	}

	fn write(&self, buf: &mut [u8]) {
		for b in buf[..INODE_SIZE].iter_mut() {
			*b = 0;
		}
		buf[0] = self.kind;
		buf[8..16].copy_from_slice(&self.size.to_le_bytes());
		for (i, &b) in self.direct.iter().enumerate() {
			let off = 16 + i * 4;
			buf[off..off + 4].copy_from_slice(&b.to_le_bytes());
		}
	}

	// Number of data blocks the file's `size` occupies.
	fn nblocks(&self) -> usize {
		(self.size as usize).div_ceil(BLOCK_SIZE)
	}
}

// A mounted LSFS over a block device. Holds the superblock and the allocation bitmap
// (one block) in memory; inodes, directory entries, and file data are read and
// written on demand.
pub struct Lsfs<D: BlockDevice> {
	dev: D,
	sb: Superblock,
	bitmap: Vec<u8>,
}

impl<D: BlockDevice> Lsfs<D> {
	// Format `dev` as a fresh, empty LSFS spanning `num_blocks` blocks (an empty root
	// directory, no files), then return it mounted. The device must hold at least the
	// metadata blocks plus one data block.
	pub fn format(mut dev: D, num_blocks: u32) -> Result<Lsfs<D>, FsError> {
		let inode_blocks = (NUM_INODES as usize * INODE_SIZE).div_ceil(BLOCK_SIZE) as u32;
		let bitmap_block = 1u32;
		let inode_start = 2u32;
		let data_start = inode_start + inode_blocks;
		if num_blocks <= data_start || num_blocks as usize > BLOCK_SIZE * 8 {
			return Err(FsError::Invalid);
		}
		let sb = Superblock { num_blocks, num_inodes: NUM_INODES, bitmap_block, inode_start, data_start, root_inode: ROOT_INODE };

		// superblock
		let mut block = vec![0u8; BLOCK_SIZE];
		block[0..8].copy_from_slice(&MAGIC);
		block[8..12].copy_from_slice(&VERSION.to_le_bytes());
		block[12..16].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
		block[16..20].copy_from_slice(&num_blocks.to_le_bytes());
		block[20..24].copy_from_slice(&NUM_INODES.to_le_bytes());
		block[24..28].copy_from_slice(&bitmap_block.to_le_bytes());
		block[28..32].copy_from_slice(&inode_start.to_le_bytes());
		block[32..36].copy_from_slice(&data_start.to_le_bytes());
		block[36..40].copy_from_slice(&ROOT_INODE.to_le_bytes());
		if !dev.write_block(0, &block) {
			return Err(FsError::Io);
		}

		// allocation bitmap: the metadata blocks (0..data_start) are allocated, the
		// rest free.
		let mut bitmap = vec![0u8; BLOCK_SIZE];
		for b in 0..data_start {
			bitmap[(b / 8) as usize] |= 1 << (b % 8);
		}
		if !dev.write_block(bitmap_block, &bitmap) {
			return Err(FsError::Io);
		}

		// zero the inode table, then write the root directory inode (an empty dir).
		let zero = vec![0u8; BLOCK_SIZE];
		for b in inode_start..data_start {
			if !dev.write_block(b, &zero) {
				return Err(FsError::Io);
			}
		}
		let mut fs = Lsfs { dev, sb, bitmap };
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
		let sb = Superblock { num_blocks: u32::from_le_bytes(block[16..20].try_into().ok()?), num_inodes: u32::from_le_bytes(block[20..24].try_into().ok()?), bitmap_block: u32::from_le_bytes(block[24..28].try_into().ok()?), inode_start: u32::from_le_bytes(block[28..32].try_into().ok()?), data_start: u32::from_le_bytes(block[32..36].try_into().ok()?), root_inode: u32::from_le_bytes(block[36..40].try_into().ok()?) };
		let mut bitmap = vec![0u8; BLOCK_SIZE];
		if !dev.read_block(sb.bitmap_block, &mut bitmap) {
			return None;
		}
		Some(Lsfs { dev, sb, bitmap })
	}

	// Look a file up by name in the root directory, returning its inode number.
	pub fn lookup(&mut self, name: &[u8]) -> Option<u32> {
		self.dir_find(name).map(|(inode, _, _)| inode)
	}

	// Read the whole file named `name` into a freshly allocated buffer.
	pub fn read_file(&mut self, name: &[u8]) -> Result<Vec<u8>, FsError> {
		let inode_num = self.dir_find(name).ok_or(FsError::NotFound)?.0;
		let inode = self.read_inode(inode_num)?;
		if inode.kind != KIND_FILE {
			return Err(FsError::NotFound);
		}
		let mut out = Vec::with_capacity(inode.size as usize);
		let mut block = vec![0u8; BLOCK_SIZE];
		let mut remaining = inode.size as usize;
		for i in 0..inode.nblocks() {
			if !self.dev.read_block(inode.direct[i], &mut block) {
				return Err(FsError::Io);
			}
			let take = remaining.min(BLOCK_SIZE);
			out.extend_from_slice(&block[..take]);
			remaining -= take;
		}
		Ok(out)
	}

	// List the root directory as (name, size) pairs, one per live file.
	pub fn list(&mut self) -> Result<Vec<(Vec<u8>, u64)>, FsError> {
		let mut out = Vec::new();
		for (name, inode_num) in self.dir_entries()? {
			let size = self.read_inode(inode_num)?.size;
			out.push((name, size));
		}
		Ok(out)
	}

	// Create or overwrite the file `name` with `data` (create-or-truncate). New data
	// blocks are written and committed before the old ones are freed, so a crash
	// leaves either the previous contents or the new ones intact.
	pub fn write_file(&mut self, name: &[u8], data: &[u8]) -> Result<(), FsError> {
		if name.is_empty() || name.len() > NAME_MAX {
			return Err(FsError::TooLong);
		}
		let needed = data.len().div_ceil(BLOCK_SIZE);
		if needed > DIRECT {
			return Err(FsError::NoSpace);
		}

		let existing = self.dir_find(name).map(|(inode, _, _)| inode);
		let inode_num = match existing {
			Some(n) => n,
			None => self.alloc_inode()?,
		};
		let old = if existing.is_some() { self.read_inode(inode_num)? } else { Inode::empty(KIND_FILE) };

		// find (do not yet mark) the data blocks the new contents need.
		let new_blocks = self.find_free_blocks(needed)?;

		// 1. write the file data into the new blocks (data before metadata).
		let mut block = vec![0u8; BLOCK_SIZE];
		for (i, &blk) in new_blocks.iter().enumerate() {
			let start = i * BLOCK_SIZE;
			let end = (start + BLOCK_SIZE).min(data.len());
			for b in block.iter_mut() {
				*b = 0;
			}
			block[..end - start].copy_from_slice(&data[start..end]);
			if !self.dev.write_block(blk, &block) {
				return Err(FsError::Io);
			}
		}

		// 2. commit the allocation bitmap (the new blocks are now reserved; any old
		//    blocks stay reserved until step 4).
		for &blk in &new_blocks {
			self.mark(blk, true);
		}
		self.flush_bitmap()?;

		// 3. point the inode at the new blocks with the new size.
		let mut inode = Inode::empty(KIND_FILE);
		inode.size = data.len() as u64;
		for (i, &blk) in new_blocks.iter().enumerate() {
			inode.direct[i] = blk;
		}
		self.write_inode(inode_num, &inode)?;

		// 4. name it in the directory (new files only) - the entry comes last, so the
		//    file is reachable only once its blocks and inode are durable.
		if existing.is_none() {
			self.dir_add(name, inode_num)?;
		}

		// 5. free the previous contents' blocks (now unreferenced).
		if existing.is_some() {
			for i in 0..old.nblocks() {
				self.mark(old.direct[i], false);
			}
			self.flush_bitmap()?;
		}
		Ok(())
	}

	// Delete the file `name`. The directory entry is cleared first, so a crash leaves
	// at worst an orphaned inode, never a dangling name.
	pub fn remove(&mut self, name: &[u8]) -> Result<(), FsError> {
		let (inode_num, dir_block, slot) = self.dir_find(name).ok_or(FsError::NotFound)?;
		let inode = self.read_inode(inode_num)?;

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

		// 2. free the inode.
		self.write_inode(inode_num, &Inode::empty(KIND_FREE))?;

		// 3. free the data blocks.
		for i in 0..inode.nblocks() {
			self.mark(inode.direct[i], false);
		}
		self.flush_bitmap()?;
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
		let bitmap_block = self.sb.bitmap_block;
		let bitmap = self.bitmap.clone();
		if !self.dev.write_block(bitmap_block, &bitmap) {
			return Err(FsError::Io);
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

	// directory (root only)

	// Scan the root directory for `name`, returning (file inode, the directory block
	// holding the entry, the entry's byte offset within that block).
	fn dir_find(&mut self, name: &[u8]) -> Option<(u32, u32, usize)> {
		let root = self.read_inode(self.sb.root_inode).ok()?;
		let mut block = vec![0u8; BLOCK_SIZE];
		for i in 0..root.nblocks() {
			let dir_block = root.direct[i];
			if !self.dev.read_block(dir_block, &mut block) {
				return None;
			}
			for slot in 0..DENTRIES_PER_BLOCK {
				let off = slot * DENTRY_SIZE;
				if block[off] == 0 {
					continue;
				}
				if entry_name(&block[off..off + DENTRY_SIZE]) == name {
					let inode = u32::from_le_bytes(block[off + NAME_MAX..off + NAME_MAX + 4].try_into().ok()?);
					return Some((inode, dir_block, off));
				}
			}
		}
		None
	}

	// Collect every live (name, inode) entry in the root directory.
	fn dir_entries(&mut self) -> Result<Vec<(Vec<u8>, u32)>, FsError> {
		let root = self.read_inode(self.sb.root_inode)?;
		let mut out = Vec::new();
		let mut block = vec![0u8; BLOCK_SIZE];
		for i in 0..root.nblocks() {
			if !self.dev.read_block(root.direct[i], &mut block) {
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

	// Add a (name, inode) entry to the root directory, growing it by one block if no
	// existing block has a free slot.
	fn dir_add(&mut self, name: &[u8], inode: u32) -> Result<(), FsError> {
		let mut root = self.read_inode(self.sb.root_inode)?;
		let mut block = vec![0u8; BLOCK_SIZE];

		// reuse a free slot in an existing directory block.
		for i in 0..root.nblocks() {
			let dir_block = root.direct[i];
			if !self.dev.read_block(dir_block, &mut block) {
				return Err(FsError::Io);
			}
			for slot in 0..DENTRIES_PER_BLOCK {
				let off = slot * DENTRY_SIZE;
				if block[off] == 0 {
					write_entry(&mut block[off..off + DENTRY_SIZE], name, inode);
					if !self.dev.write_block(dir_block, &block) {
						return Err(FsError::Io);
					}
					return Ok(());
				}
			}
		}

		// no room: grow the directory by one block.
		if root.nblocks() >= DIRECT {
			return Err(FsError::NoSpace);
		}
		let dir_block = self.alloc_one()?;
		for b in block.iter_mut() {
			*b = 0;
		}
		write_entry(&mut block[0..DENTRY_SIZE], name, inode);
		if !self.dev.write_block(dir_block, &block) {
			return Err(FsError::Io);
		}
		root.direct[root.nblocks()] = dir_block;
		root.size += BLOCK_SIZE as u64;
		self.write_inode(self.sb.root_inode, &root)?;
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

#[cfg(test)]
mod tests;
