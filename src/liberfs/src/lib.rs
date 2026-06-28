//! LiberFS - a small writable, copy-on-write on-disk filesystem for LiberSystem.
//!
//! The on-disk layout is a Unix-flavoured filesystem turned copy-on-write: two
//! superblock slots at blocks 0 and 1, then one flat pool of blocks (block 2 onward)
//! out of which the inode table, its index block, directory data, file data, and the
//! per-extent checksum blocks are all allocated. Block addresses are 64-bit, so a
//! volume scales from gigabytes into exabytes. Directories form a tree from the root
//! inode; an inode maps its data with extents - each a contiguous run of blocks paired
//! with one checksum block - held inline in the inode and spilling to an overflow chain
//! when there are many, so a file grows from a few blocks to hundreds of gigabytes and
//! an unwritten range simply has no extent (a sparse hole that reads back as zeros).
//! Every data block is paired with a CRC32C, kept in its extent's checksum block, so
//! on-disk corruption is caught when the block is read. Each inode also reserves an
//! opaque owner tag (stored, never interpreted: authorization lives in the capability
//! layer and StorageService, not in the filesystem). There is no on-disk allocation
//! bitmap: the free map is reconstructed in memory at mount from the blocks the live
//! generations reference. It backs the `Storage.Volume` API and survives a reboot.
//!
//! All I/O goes through the [`BlockDevice`] trait (one fixed-size block at a time),
//! so the same code drives a real virtio-blk disk in StorageService and a
//! `Vec`-backed device in the host tests. The crate is `no_std` for the userspace
//! build and pulls in `std` only under `cargo test` so it can be exercised on the
//! host.
//!
//! ## Crash atomicity (copy-on-write)
//!
//! A mutation never overwrites a block that a committed generation still references:
//! changed data, the extent and checksum blocks describing it, the inode, and the
//! inode-table block holding the inode are each written to a freshly allocated block
//! (copied up once per transaction, then updated in place). The transaction commits
//! with a single atomic
//! write of a new superblock - carrying an incremented generation and a self-CRC - to
//! the inactive of the two slots. A crash before that write leaves the old superblock
//! active and the old tree fully intact; a torn superblock write fails its self-CRC
//! and mount falls back to the other slot. So a crash mid-write always leaves either
//! the complete old file or the complete new file, never a torn mix, and the old root
//! is never lost.
//!
//! ## Snapshots (an old root kept reachable)
//!
//! Because the previous generation's blocks are not freed at commit (they stay
//! reserved by the free-map walk), the superblock slot it still occupies remains a
//! consistent, read-only snapshot of the filesystem one commit ago. [`LiberFs::mount`]
//! opens the newest generation; [`LiberFs::mount_snapshot`] opens that previous one
//! read-only. This is the structural groundwork for snapshots; the full snapshot UX
//! is a later milestone. The generation before last is reclaimed by the next commit.
//!
//! ## Integrity (block checksums)
//!
//! Each data block is checksummed with a CRC32C stored in its extent's checksum block,
//! and each metadata block beside its own pointer. The checksum is computed on write
//! and rechecked on every read, so a flipped bit on disk surfaces as
//! [`FsError::Corrupt`] instead of silently corrupt data; [`LiberFs::fsck`] walks
//! every live data block and reports how many fail their checksum. With copy-on-write
//! a crash can no longer leak blocks or orphan an inode, so `fsck` no longer needs to
//! reclaim them.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::collections::BTreeSet;
use alloc::vec;
use alloc::vec::Vec;

// One filesystem block. Eight 512-byte disk sectors, a page; the I/O unit of the
// BlockDevice trait.
pub const BLOCK_SIZE: usize = 4096;

// On-disk superblock magic and format version. Mount rejects anything else (a fresh
// or stale-format disk), so StorageService knows to reformat. Version 1 is the
// copy-on-write, extent-mapped layout: two superblock slots, a flat block pool with no
// on-disk bitmap, 64-bit block addresses, an inode table reached through an index
// block, nested directories, files mapped by extents (each a contiguous run with its
// own checksum block) and sparse holes, per-inode timestamps and an opaque owner tag,
// and a CRC32C paired with every block pointer.
const MAGIC: [u8; 8] = *b"LIBERFS1";
const VERSION: u32 = 1;

// The two superblock slots (blocks 0 and 1): a commit writes the new superblock to the
// inactive slot, so the active one survives a torn write. The block pool begins right
// after them.
const SUPER_SLOTS: u32 = 2;
const POOL_START: u64 = SUPER_SLOTS as u64;

// One inode is a fixed 256-byte slot: a kind byte, a size, two timestamps, the extent
// map's overflow pointer and count, an opaque owner tag, then EXTENTS_INLINE inline
// extent records. 16 inodes fit one block.
const INODE_SIZE: usize = 256;
const INODES_PER_BLOCK: usize = BLOCK_SIZE / INODE_SIZE;
// The inode-table index pairs each inode-table block with its CRC32C as a (u64 pointer,
// u32 CRC32C) entry: 12 bytes. The same pairing checksums the inode metadata itself.
const ENTRY_SIZE: usize = 12;
// (u64 pointer + u32 CRC) entries that fit one index block; also caps the inode table so
// its whole index fits a single block.
const PTRS_PER_BLOCK: usize = BLOCK_SIZE / ENTRY_SIZE;
// A reserved opaque owner / ACL tag, stored in every inode but never interpreted by the
// filesystem: authorization is the capability layer and StorageService, not POSIX
// permissions. Room to grow into a real owner identity without another format change.
const OWNER_TAG_LEN: usize = 16;
const OWNER_TAG_OFF: usize = 56;

// A file is mapped by EXTENTS: each is a contiguous run of blocks (a logical start, a
// physical start, a length) with one checksum block holding a CRC32C per block in the
// run. One extent record is 32 bytes on disk: logical (u64), physical (u64), length
// (u32), the checksum block's own CRC32C (u32), then the checksum block pointer (u64).
const EXTENT_SIZE: usize = 32;
// Byte offset of the first inline extent: past the fixed header (kind, size, two
// timestamps, the extent-overflow pointer and count) and the owner tag.
const EXTENT_OFF: usize = OWNER_TAG_OFF + OWNER_TAG_LEN;
// (256 - 72) / 32 = 5 extents live inline in the inode; a file of up to five runs needs
// no overflow block at all. Beyond that, extents spill to a chain of extent blocks.
const EXTENTS_INLINE: usize = (INODE_SIZE - EXTENT_OFF) / EXTENT_SIZE;
// A checksum block holds one CRC32C (4 bytes) per block of its extent, so an extent
// spans at most this many blocks (1024 = 4 MiB). A longer file is several extents.
const CRCS_PER_BLOCK: usize = BLOCK_SIZE / 4;
// An extent-overflow block: an 8-byte next-block pointer, its 4-byte CRC32C, a 4-byte
// count, then the extent records. (4096 - 16) / 32 = 127 extents per overflow block.
const EXTENT_HDR: usize = 16;
const EXTENTS_PER_BLOCK: usize = (BLOCK_SIZE - EXTENT_HDR) / EXTENT_SIZE;

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

// One directory entry is a fixed slot: a NUL-padded name then the entry's inode number.
// A free slot has an empty name (first byte NUL); a full 255-byte name uses the whole
// name field with no terminator. 15 entries fit one block.
const NAME_MAX: usize = 255;
const DENTRY_SIZE: usize = 260;
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

// Metadata about one path, returned by [`LiberFs::stat`]: its byte length, whether it is
// a directory, and its created / modified logical timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
	pub size: u64,
	pub is_dir: bool,
	pub ctime: u64,
	pub mtime: u64,
}

// What an [`LiberFs::fsck`] pass reclaimed: blocks that the bitmap marked allocated but
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
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool;
	// Write `buf` (exactly BLOCK_SIZE bytes) to block `index`. False on I/O failure.
	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool;
}

// The parsed superblock, cached in memory for the life of a mount. With copy-on-write
// the inode table moves on every commit, so the superblock points at it through an
// index block rather than a fixed region; `generation` orders the two slots and the
// trailing self-CRC catches a torn commit.
#[derive(Clone, Copy)]
struct Superblock {
	num_blocks: u64,
	num_inodes: u32,
	// Size of the inode table in blocks; also the number of live entries in the index
	// block.
	inode_blocks: u32,
	// Monotonic generation: a commit writes the new superblock with `generation + 1`,
	// so the newest valid slot is the live one and the other is the snapshot.
	generation: u64,
	// Pool block holding the inode-table index (the (pointer, CRC32C) of each
	// inode-table block), and the checksum of that index block.
	itable_index: u64,
	itable_index_crc: u32,
	root_inode: u32,
}

// Byte offset of the superblock's own CRC32C within its block; the checksum covers the
// whole block with these four bytes zeroed, so a half-written superblock fails it.
const SB_CRC_OFFSET: usize = 56;

// One extent: a contiguous run of `length` blocks mapped from logical block `logical`
// to physical block `physical`, paired with a checksum block (`csum`) holding the
// CRC32C of every block in the run, plus `csum_crc`, that checksum block's own CRC32C.
// A file's extents are kept sorted by `logical`; a logical block no extent covers is a
// hole that reads back as zeros (so a sparse file costs only its written runs).
#[derive(Clone, Copy)]
struct Extent {
	logical: u64,
	physical: u64,
	length: u32,
	csum: u64,
	csum_crc: u32,
}

impl Extent {
	fn parse(buf: &[u8]) -> Extent {
		Extent { logical: u64::from_le_bytes(buf[0..8].try_into().unwrap()), physical: u64::from_le_bytes(buf[8..16].try_into().unwrap()), length: u32::from_le_bytes(buf[16..20].try_into().unwrap()), csum_crc: u32::from_le_bytes(buf[20..24].try_into().unwrap()), csum: u64::from_le_bytes(buf[24..32].try_into().unwrap()) }
	}

	fn write(&self, buf: &mut [u8]) {
		buf[0..8].copy_from_slice(&self.logical.to_le_bytes());
		buf[8..16].copy_from_slice(&self.physical.to_le_bytes());
		buf[16..20].copy_from_slice(&self.length.to_le_bytes());
		buf[20..24].copy_from_slice(&self.csum_crc.to_le_bytes());
		buf[24..32].copy_from_slice(&self.csum.to_le_bytes());
	}

	// The first logical block past the run.
	fn end(&self) -> u64 {
		self.logical + self.length as u64
	}

	// Does the run cover logical block `lb`?
	fn covers(&self, lb: u64) -> bool {
		lb >= self.logical && lb < self.end()
	}
}

// One inode, parsed from / rendered to its 256-byte on-disk slot. `extents` is the
// in-memory extent map: `parse` fills only the EXTENTS_INLINE runs that live in the
// slot, and [`LiberFs::read_inode`] completes it from the overflow chain rooted at
// `spill`. `extent_count` is the total run count (inline plus spilled) - the header
// field that says how many extents to read.
struct Inode {
	kind: u8,
	size: u64,
	ctime: u64,
	mtime: u64,
	// An opaque owner / ACL tag, stored but never interpreted by the filesystem.
	owner_tag: [u8; OWNER_TAG_LEN],
	extents: Vec<Extent>,
	spill: u64,
	spill_crc: u32,
	extent_count: u32,
}

impl Inode {
	fn empty(kind: u8) -> Inode {
		Inode { kind, size: 0, ctime: 0, mtime: 0, owner_tag: [0u8; OWNER_TAG_LEN], extents: Vec::new(), spill: 0, spill_crc: 0, extent_count: 0 }
	}

	// Parse the fixed header and the inline extents; any spilled extents (when
	// `extent_count` exceeds EXTENTS_INLINE) are appended afterwards by `read_inode`.
	fn parse(buf: &[u8]) -> Inode {
		let extent_count = u32::from_le_bytes(buf[44..48].try_into().unwrap());
		let inline = (extent_count as usize).min(EXTENTS_INLINE);
		let mut extents = Vec::with_capacity(inline);
		for i in 0..inline {
			let off = EXTENT_OFF + i * EXTENT_SIZE;
			extents.push(Extent::parse(&buf[off..off + EXTENT_SIZE]));
		}
		let mut owner_tag = [0u8; OWNER_TAG_LEN];
		owner_tag.copy_from_slice(&buf[OWNER_TAG_OFF..OWNER_TAG_OFF + OWNER_TAG_LEN]);
		Inode { kind: buf[0], size: u64::from_le_bytes(buf[8..16].try_into().unwrap()), ctime: u64::from_le_bytes(buf[16..24].try_into().unwrap()), mtime: u64::from_le_bytes(buf[24..32].try_into().unwrap()), owner_tag, extents, spill: u64::from_le_bytes(buf[32..40].try_into().unwrap()), spill_crc: u32::from_le_bytes(buf[40..44].try_into().unwrap()), extent_count }
	}

	// Render the header and the first EXTENTS_INLINE extents into the 256-byte slot. The
	// `spill` / `spill_crc` / `extent_count` header fields and the overflow chain are set
	// beforehand by [`LiberFs::flush_extents`], which `write_inode` always calls first.
	fn write(&self, buf: &mut [u8]) {
		for b in buf[..INODE_SIZE].iter_mut() {
			*b = 0;
		}
		buf[0] = self.kind;
		buf[8..16].copy_from_slice(&self.size.to_le_bytes());
		buf[16..24].copy_from_slice(&self.ctime.to_le_bytes());
		buf[24..32].copy_from_slice(&self.mtime.to_le_bytes());
		buf[32..40].copy_from_slice(&self.spill.to_le_bytes());
		buf[40..44].copy_from_slice(&self.spill_crc.to_le_bytes());
		buf[44..48].copy_from_slice(&self.extent_count.to_le_bytes());
		buf[OWNER_TAG_OFF..OWNER_TAG_OFF + OWNER_TAG_LEN].copy_from_slice(&self.owner_tag);
		for (i, ext) in self.extents.iter().take(EXTENTS_INLINE).enumerate() {
			let off = EXTENT_OFF + i * EXTENT_SIZE;
			ext.write(&mut buf[off..off + EXTENT_SIZE]);
		}
	}

	// Number of data blocks the file's `size` occupies.
	fn nblocks(&self) -> usize {
		(self.size as usize).div_ceil(BLOCK_SIZE)
	}
}

// A mounted LiberFS over a block device. Copy-on-write: the inode table is reached
// through an in-memory `itable` (the (block, CRC32C) of each inode-table block, kept
// in sync with the index block) rather than a fixed region, and `free` is rebuilt at
// mount from the blocks the live and previous generations reference - there is no
// on-disk bitmap. `clock` is a logical timestamp the caller can advance (no wall clock
// lives in this crate); mutations stamp inode `mtime` from it.
//
// A mutation runs as a transaction: `begin` snapshots `itable`, the body allocates
// fresh blocks (tracked in `fresh`) and copies metadata up, and `commit` writes a new
// superblock to the inactive slot - or `abort` rolls back. The previous generation's
// `itable` and index stay reserved so it remains a read-only snapshot.
pub struct LiberFs<D: BlockDevice> {
	dev: D,
	num_blocks: u64,
	num_inodes: u32,
	inode_blocks: u32,
	root_inode: u32,
	// Live generation: its number, the superblock slot (0 or 1) it occupies, and the
	// per-inode-table-block (pointer, CRC32C) pairs plus the index block holding them.
	generation: u64,
	slot: u32,
	itable: Vec<(u64, u32)>,
	itable_index: u64,
	// The previous generation (the read-only snapshot), if any: its inode table and
	// index, kept reserved so a commit does not reuse its blocks.
	prev_itable: Option<Vec<(u64, u32)>>,
	prev_index: u64,
	// In-memory free map, one bit per block, derived at mount and after each commit -
	// never written to disk.
	free: Vec<u8>,
	// Blocks allocated by the in-flight transaction: safe to overwrite in place (no
	// committed generation references them yet).
	fresh: BTreeSet<u64>,
	// The `itable` snapshot taken at `begin`, restored by `abort` and used by `commit`
	// to reserve the generation it supersedes.
	txn_itable: Option<Vec<(u64, u32)>>,
	clock: u64,
}

impl<D: BlockDevice> LiberFs<D> {
	// Format `dev` as a fresh, empty LiberFS spanning `num_blocks` blocks (an empty root
	// directory, no files), then return it mounted. Generation 0 lays out the two
	// superblock slots, the inode-table blocks, and the index block; everything else is
	// the free pool. The inode table scales with the volume but is capped so its index
	// fits one block.
	pub fn format(mut dev: D, num_blocks: u64) -> Result<LiberFs<D>, FsError> {
		// scale the inode count with the volume, rounded up to whole inode blocks, and
		// capped so the whole inode-table index fits one block.
		let want_inodes = (num_blocks / BLOCKS_PER_INODE as u64).max(MIN_INODES as u64);
		let mut inode_blocks = (want_inodes as usize * INODE_SIZE).div_ceil(BLOCK_SIZE) as u32;
		if inode_blocks > PTRS_PER_BLOCK as u32 {
			inode_blocks = PTRS_PER_BLOCK as u32;
		}
		let num_inodes = inode_blocks * INODES_PER_BLOCK as u32;

		// generation-0 layout: [slot 0][slot 1][inode-table blocks][index block], then
		// the free pool. The root directory inode starts empty (no data blocks).
		let index_block: u64 = POOL_START + inode_blocks as u64;
		if num_blocks <= index_block + 1 {
			return Err(FsError::Invalid);
		}

		// write the inode-table blocks (all free but the root directory at inode 0) and
		// record each block's (pointer, CRC32C) in the index.
		let zero = vec![0u8; BLOCK_SIZE];
		let mut itable = Vec::with_capacity(inode_blocks as usize);
		for b in 0..inode_blocks {
			let mut block = zero.clone();
			if b == 0 {
				Inode::empty(KIND_DIR).write(&mut block[0..INODE_SIZE]);
			}
			let ptr = POOL_START + b as u64;
			if !dev.write_block(ptr, &block) {
				return Err(FsError::Io);
			}
			itable.push((ptr, crc32c(&block)));
		}
		let mut index = vec![0u8; BLOCK_SIZE];
		for (i, &(ptr, crc)) in itable.iter().enumerate() {
			let off = i * ENTRY_SIZE;
			index[off..off + 8].copy_from_slice(&ptr.to_le_bytes());
			index[off + 8..off + 12].copy_from_slice(&crc.to_le_bytes());
		}
		if !dev.write_block(index_block, &index) {
			return Err(FsError::Io);
		}
		let index_crc = crc32c(&index);

		// generation 0 in slot 0; slot 1 left invalid (zeroed) until the first commit
		// ping-pongs onto it.
		let sb = Superblock { num_blocks, num_inodes, inode_blocks, generation: 0, itable_index: index_block, itable_index_crc: index_crc, root_inode: ROOT_INODE };
		if !dev.write_block(0, &serialize_superblock(&sb)) {
			return Err(FsError::Io);
		}
		if !dev.write_block(1, &zero) {
			return Err(FsError::Io);
		}

		let mut fs = LiberFs { dev, num_blocks, num_inodes, inode_blocks, root_inode: ROOT_INODE, generation: 0, slot: 0, itable, itable_index: index_block, prev_itable: None, prev_index: 0, free: vec![0u8; (num_blocks as usize).div_ceil(8)], fresh: BTreeSet::new(), txn_itable: None, clock: 0 };
		fs.derive_free()?;
		Ok(fs)
	}

	// Mount an existing LiberFS on `dev` at its newest committed generation. Returns None
	// if neither superblock slot is a valid LiberFS (an unformatted or foreign disk).
	pub fn mount(dev: D) -> Option<LiberFs<D>> {
		Self::mount_at(dev, true)
	}

	// Mount the previous generation read-only: the consistent snapshot of the
	// filesystem one commit ago. Returns None unless both superblock slots are valid (a
	// freshly formatted or single-generation volume has no older snapshot). The handle
	// is meant for reading; writing to it would interleave generations.
	pub fn mount_snapshot(dev: D) -> Option<LiberFs<D>> {
		Self::mount_at(dev, false)
	}

	fn mount_at(mut dev: D, newest: bool) -> Option<LiberFs<D>> {
		// read and validate both superblock slots.
		let mut buf = vec![0u8; BLOCK_SIZE];
		let mut slots: [Option<Superblock>; SUPER_SLOTS as usize] = [None, None];
		for s in 0..SUPER_SLOTS {
			if dev.read_block(s as u64, &mut buf) {
				slots[s as usize] = parse_superblock(&buf);
			}
		}
		// order the valid slots by generation: the higher is the live root, the lower
		// the snapshot.
		let mut valid: Vec<(u32, u64)> = (0..SUPER_SLOTS).filter_map(|s| slots[s as usize].map(|sb| (s, sb.generation))).collect();
		valid.sort_by_key(|&(_, g)| g);
		let (cur_slot, prev_slot) = if newest {
			let &(cur, _) = valid.last()?;
			let prev = valid.iter().rev().nth(1).map(|&(s, _)| s);
			(cur, prev)
		} else {
			// the snapshot: the lower generation, only if there are two.
			if valid.len() < 2 {
				return None;
			}
			(valid[0].0, None)
		};

		let sb = slots[cur_slot as usize]?;
		let itable = Self::load_itable(&mut dev, &sb)?;
		let (prev_itable, prev_index) = match prev_slot {
			Some(ps) => {
				let psb = slots[ps as usize]?;
				(Some(Self::load_itable(&mut dev, &psb)?), psb.itable_index)
			}
			None => (None, 0),
		};

		let mut fs = LiberFs { dev, num_blocks: sb.num_blocks, num_inodes: sb.num_inodes, inode_blocks: sb.inode_blocks, root_inode: sb.root_inode, generation: sb.generation, slot: cur_slot, itable, itable_index: sb.itable_index, prev_itable, prev_index, free: vec![0u8; (sb.num_blocks as usize).div_ceil(8)], fresh: BTreeSet::new(), txn_itable: None, clock: 0 };
		fs.derive_free().ok()?;
		Some(fs)
	}

	// Read the inode-table index block named by `sb` and parse it into the per-block
	// (pointer, CRC32C) table. Fails if the index block does not match its checksum.
	fn load_itable(dev: &mut D, sb: &Superblock) -> Option<Vec<(u64, u32)>> {
		let mut idx = vec![0u8; BLOCK_SIZE];
		if !dev.read_block(sb.itable_index, &mut idx) {
			return None;
		}
		if crc32c(&idx) != sb.itable_index_crc {
			return None;
		}
		let mut itable = Vec::with_capacity(sb.inode_blocks as usize);
		for i in 0..sb.inode_blocks as usize {
			let off = i * ENTRY_SIZE;
			let ptr = u64::from_le_bytes(idx[off..off + 8].try_into().ok()?);
			let crc = u32::from_le_bytes(idx[off + 8..off + 12].try_into().ok()?);
			itable.push((ptr, crc));
		}
		Some(itable)
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
		self.read_dir_inode(self.root_inode)
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
		self.begin();
		let r = self.mkdir_inner(path);
		self.finish(r)
	}

	fn mkdir_inner(&mut self, path: &[u8]) -> Result<(), FsError> {
		let segs = split_segments(path)?;
		let mut parent = self.root_inode;
		for seg in segs {
			parent = self.dir_lookup_or_create(parent, seg)?;
		}
		Ok(())
	}

	// Create or overwrite the file at `path` with `data` (create-or-truncate). Missing
	// parent directories are created. Copy-on-write: the new data, extent and checksum
	// blocks, and inode are written to freshly allocated blocks and the transaction
	// commits with a single superblock swap, so a crash leaves either the previous file
	// or the new one intact - never a torn mix.
	pub fn write_file(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
		self.begin();
		let r = self.write_file_inner(path, data);
		self.finish(r)
	}

	fn write_file_inner(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
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

		// build the new inode from scratch: every logical block is written to a fresh
		// block (the old file's blocks stay referenced by the previous generation).
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

		// point the inode at the new blocks, then name it (new files only). The old
		// inode and blocks are not freed here - the commit's previous generation keeps
		// them as the snapshot, and the next commit reclaims them.
		self.write_inode(inode_num, &mut inode)?;
		if old.is_none() {
			self.dir_add(parent, name, inode_num)?;
		}
		Ok(())
	}

	// Delete the file or empty directory at `path`. Copy-on-write: the new generation
	// drops the directory entry and frees the inode; a crash before the commit leaves
	// the file fully intact.
	pub fn remove(&mut self, path: &[u8]) -> Result<(), FsError> {
		self.begin();
		let r = self.remove_inner(path);
		self.finish(r)
	}

	fn remove_inner(&mut self, path: &[u8]) -> Result<(), FsError> {
		let (parent, name) = self.resolve_parent(path, false)?;
		let inode_num = self.dir_find_in(parent, name).ok_or(FsError::NotFound)?.0;
		let inode = self.read_inode(inode_num)?;
		if inode.kind == KIND_DIR && !self.read_dir_inode(inode_num)?.is_empty() {
			return Err(FsError::Invalid);
		}

		// clear the directory entry and free the inode in the new generation; its old
		// blocks remain referenced by the previous generation.
		self.dir_clear(parent, name)?;
		self.write_inode(inode_num, &mut Inode::empty(KIND_FREE))?;
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
	// Only the touched blocks are rewritten (each copied up to a fresh block), the rest
	// of the file is left in place, and the change commits atomically.
	pub fn write_at(&mut self, path: &[u8], offset: u64, data: &[u8]) -> Result<(), FsError> {
		self.begin();
		let r = self.write_at_inner(path, offset, data);
		self.finish(r)
	}

	fn write_at_inner(&mut self, path: &[u8], offset: u64, data: &[u8]) -> Result<(), FsError> {
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
				self.write_inode(num, &mut f)?;
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
		self.write_inode(inode_num, &mut inode)?;
		Ok(())
	}

	// Append `data` to the end of the file at `path` (creating it if needed).
	pub fn append(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
		self.begin();
		let r = self.append_inner(path, data);
		self.finish(r)
	}

	fn append_inner(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
		let size = match self.resolve(path) {
			Ok(num) => self.read_inode(num)?.size,
			Err(FsError::NotFound) => 0,
			Err(e) => return Err(e),
		};
		self.write_at_inner(path, size, data)
	}

	// Resize the file at `path` to `new_len`: shrinking drops the blocks past the new
	// end, growing leaves a hole (which reads as zeros). Copy-on-write: the change goes
	// to fresh blocks and commits atomically.
	pub fn truncate(&mut self, path: &[u8], new_len: u64) -> Result<(), FsError> {
		self.begin();
		let r = self.truncate_inner(path, new_len);
		self.finish(r)
	}

	fn truncate_inner(&mut self, path: &[u8], new_len: u64) -> Result<(), FsError> {
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
		self.write_inode(inode_num, &mut inode)?;
		Ok(())
	}

	// rename / move within the volume

	// Move the file or directory at `from` to `to` within the same volume. Missing
	// parent directories of `to` are created. An existing file (or empty directory) at
	// `to` is replaced. Copy-on-write: the whole move commits atomically, so a crash
	// leaves the object reachable under exactly one name - never lost or doubled.
	// Moving a directory into its own subtree is rejected.
	pub fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<(), FsError> {
		self.begin();
		let r = self.rename_inner(from, to);
		self.finish(r)
	}

	fn rename_inner(&mut self, from: &[u8], to: &[u8]) -> Result<(), FsError> {
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

		// point the destination name at the moved inode (add or overwrite), clear the
		// source entry, and free the inode the destination used to hold. Its old blocks
		// stay with the previous generation; the next commit reclaims them.
		self.dir_set(pt, nt, inode_f)?;
		self.dir_clear(pf, nf)?;
		if let Some(inode_t) = dest {
			if inode_t != inode_f {
				self.write_inode(inode_t, &mut Inode::empty(KIND_FREE))?;
			}
		}
		Ok(())
	}

	// consistency

	// Verify integrity. With copy-on-write a crash can no longer leak blocks or orphan
	// an inode (the free map is derived and a commit is atomic), so there is nothing to
	// reclaim; what fsck still does is walk every live data block and check it against
	// its stored checksum, reporting how many fail. The free map is also rederived,
	// which is a no-op on a consistent volume.
	pub fn fsck(&mut self) -> Result<FsckReport, FsError> {
		self.derive_free()?;
		let mut checksum_failures = 0;
		for num in 0..self.num_inodes {
			let inode = self.read_inode(num)?;
			if inode.kind != KIND_FREE {
				checksum_failures += self.count_corrupt(&inode)?;
			}
		}
		Ok(FsckReport { reclaimed_blocks: 0, reclaimed_inodes: 0, checksum_failures })
	}

	// transactions

	// Begin a mutation: snapshot the inode table so it can be restored on failure and
	// reserved as the previous generation on commit, and clear the fresh-block set.
	fn begin(&mut self) {
		self.txn_itable = Some(self.itable.clone());
		self.fresh.clear();
	}

	// Commit the in-flight mutation: write the new inode-table index, then a new
	// superblock (incremented generation) to the inactive slot - the single atomic
	// write that publishes the whole transaction. The superseded generation becomes the
	// read-only snapshot; the one before it is reclaimed by rederiving the free map.
	fn commit(&mut self) -> Result<(), FsError> {
		// the index block holding the (pointer, CRC32C) of every inode-table block.
		let new_index = self.alloc_meta()?;
		let mut index = vec![0u8; BLOCK_SIZE];
		for (i, &(ptr, crc)) in self.itable.iter().enumerate() {
			let off = i * ENTRY_SIZE;
			index[off..off + 8].copy_from_slice(&ptr.to_le_bytes());
			index[off + 8..off + 12].copy_from_slice(&crc.to_le_bytes());
		}
		if !self.dev.write_block(new_index, &index) {
			return Err(FsError::Io);
		}
		let sb = Superblock { num_blocks: self.num_blocks, num_inodes: self.num_inodes, inode_blocks: self.inode_blocks, generation: self.generation + 1, itable_index: new_index, itable_index_crc: crc32c(&index), root_inode: self.root_inode };
		let new_slot = (self.slot + 1) % SUPER_SLOTS;
		// the commit point: a single superblock write swaps the live root atomically.
		if !self.dev.write_block(new_slot as u64, &serialize_superblock(&sb)) {
			return Err(FsError::Io);
		}

		// the generation this commit superseded becomes the snapshot; its blocks stay
		// reserved by the rederived free map.
		self.prev_itable = self.txn_itable.take();
		self.prev_index = self.itable_index;
		self.generation += 1;
		self.slot = new_slot;
		self.itable_index = new_index;
		self.derive_free()
	}

	// Roll back a failed mutation: restore the inode table and rederive the free map, so
	// the half-written fresh blocks are forgotten and on-disk state is untouched.
	fn abort(&mut self) {
		if let Some(saved) = self.txn_itable.take() {
			self.itable = saved;
		}
		self.fresh.clear();
		let _ = self.derive_free();
	}

	// Finish a mutation: commit on success, roll back on failure.
	fn finish(&mut self, r: Result<(), FsError>) -> Result<(), FsError> {
		match r {
			Ok(()) => self.commit(),
			Err(e) => {
				self.abort();
				Err(e)
			}
		}
	}

	// Rebuild the in-memory free map from scratch: blocks 0 and 1 (the superblock
	// slots) plus every block the live and previous generations reference. Called at
	// mount and after each commit; nothing else persists allocation state.
	fn derive_free(&mut self) -> Result<(), FsError> {
		let mut map = vec![0u8; self.free.len()];
		set_bit(&mut map, 0);
		set_bit(&mut map, 1);
		let cur = self.itable.clone();
		self.mark_generation(self.itable_index, &cur, &mut map)?;
		if let Some(prev) = self.prev_itable.clone() {
			self.mark_generation(self.prev_index, &prev, &mut map)?;
		}
		self.free = map;
		Ok(())
	}

	// Mark, in `map`, the index block and every block one generation references: each
	// inode-table block, and the data, checksum and extent-overflow blocks of each live
	// inode in it.
	fn mark_generation(&mut self, index_block: u64, itable: &[(u64, u32)], map: &mut [u8]) -> Result<(), FsError> {
		set_bit(map, index_block);
		let mut block = vec![0u8; BLOCK_SIZE];
		for &(tbl_ptr, _) in itable {
			set_bit(map, tbl_ptr);
			if !self.dev.read_block(tbl_ptr, &mut block) {
				return Err(FsError::Io);
			}
			for slot in 0..INODES_PER_BLOCK {
				let mut inode = Inode::parse(&block[slot * INODE_SIZE..slot * INODE_SIZE + INODE_SIZE]);
				if inode.kind != KIND_FREE {
					// complete the extent map from the overflow chain before marking.
					self.load_spill(&mut inode)?;
					self.collect_inode_blocks(&inode, map)?;
				}
			}
		}
		Ok(())
	}

	// inode I/O

	// Locate inode `num`: the index of its inode-table block in `itable` and its byte
	// offset within that block.
	fn inode_location(&self, num: u32) -> (usize, usize) {
		let block = num as usize / INODES_PER_BLOCK;
		let offset = (num as usize % INODES_PER_BLOCK) * INODE_SIZE;
		(block, offset)
	}

	fn read_inode(&mut self, num: u32) -> Result<Inode, FsError> {
		if num >= self.num_inodes {
			return Err(FsError::Invalid);
		}
		let (tbl, offset) = self.inode_location(num);
		let (ptr, crc) = self.itable[tbl];
		let mut block = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(ptr, &mut block) {
			return Err(FsError::Io);
		}
		// the inode-table block carries its own checksum (in the index), so a flipped
		// bit in the metadata is caught too.
		if crc32c(&block) != crc {
			return Err(FsError::Corrupt);
		}
		let mut inode = Inode::parse(&block[offset..offset + INODE_SIZE]);
		// complete the extent map from the overflow chain (a no-op for a file whose
		// runs all fit inline).
		self.load_spill(&mut inode)?;
		Ok(inode)
	}

	// Append the spilled extents (those past EXTENTS_INLINE) from the overflow chain to
	// `inode.extents`, which `parse` filled only with the inline runs. Each chain block
	// carries the (pointer, CRC32C) of the next, so a flipped bit in the chain is caught.
	fn load_spill(&mut self, inode: &mut Inode) -> Result<(), FsError> {
		if inode.extent_count as usize <= inode.extents.len() {
			return Ok(());
		}
		let mut ptr = inode.spill;
		let mut crc = inode.spill_crc;
		let mut buf = vec![0u8; BLOCK_SIZE];
		while ptr != 0 {
			if !self.dev.read_block(ptr, &mut buf) {
				return Err(FsError::Io);
			}
			if crc32c(&buf) != crc {
				return Err(FsError::Corrupt);
			}
			let count = u32::from_le_bytes(buf[12..16].try_into().unwrap()) as usize;
			for i in 0..count {
				let off = EXTENT_HDR + i * EXTENT_SIZE;
				inode.extents.push(Extent::parse(&buf[off..off + EXTENT_SIZE]));
			}
			ptr = u64::from_le_bytes(buf[0..8].try_into().unwrap());
			crc = u32::from_le_bytes(buf[8..12].try_into().unwrap());
		}
		Ok(())
	}

	// Persist `inode.extents` past the inline ones into a fresh overflow chain (one
	// block per EXTENTS_PER_BLOCK runs) and set the `spill` / `spill_crc` /
	// `extent_count` header fields to match. The chain is built back to front so each
	// block can hold the (pointer, CRC32C) of the one after it. Always called by
	// `write_inode`, so the inode slot and chain stay consistent.
	fn flush_extents(&mut self, inode: &mut Inode) -> Result<(), FsError> {
		inode.extent_count = inode.extents.len() as u32;
		if inode.extents.len() <= EXTENTS_INLINE {
			inode.spill = 0;
			inode.spill_crc = 0;
			return Ok(());
		}
		let spilled: Vec<Extent> = inode.extents[EXTENTS_INLINE..].to_vec();
		let mut next_ptr = 0u64;
		let mut next_crc = 0u32;
		for chunk in spilled.chunks(EXTENTS_PER_BLOCK).rev() {
			let blk = self.alloc_meta()?;
			let mut buf = vec![0u8; BLOCK_SIZE];
			buf[0..8].copy_from_slice(&next_ptr.to_le_bytes());
			buf[8..12].copy_from_slice(&next_crc.to_le_bytes());
			buf[12..16].copy_from_slice(&(chunk.len() as u32).to_le_bytes());
			for (i, ext) in chunk.iter().enumerate() {
				let off = EXTENT_HDR + i * EXTENT_SIZE;
				ext.write(&mut buf[off..off + EXTENT_SIZE]);
			}
			if !self.dev.write_block(blk, &buf) {
				return Err(FsError::Io);
			}
			next_ptr = blk;
			next_crc = crc32c(&buf);
		}
		inode.spill = next_ptr;
		inode.spill_crc = next_crc;
		Ok(())
	}

	// Write inode `num`, copying its inode-table block up to a fresh block (once per
	// transaction) and updating `itable` with the new (pointer, CRC32C). The extent
	// overflow chain is rebuilt first so the inode slot and chain agree. The change is
	// published by `commit`.
	fn write_inode(&mut self, num: u32, inode: &mut Inode) -> Result<(), FsError> {
		if num >= self.num_inodes {
			return Err(FsError::Invalid);
		}
		self.flush_extents(inode)?;
		let (tbl, offset) = self.inode_location(num);
		let ptr = self.cow_meta(self.itable[tbl].0)?;
		let mut block = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(ptr, &mut block) {
			return Err(FsError::Io);
		}
		inode.write(&mut block[offset..offset + INODE_SIZE]);
		if !self.dev.write_block(ptr, &block) {
			return Err(FsError::Io);
		}
		self.itable[tbl] = (ptr, crc32c(&block));
		Ok(())
	}

	// Find a free inode slot (1..num_inodes), claim it as an empty file, and return
	// its number.
	fn alloc_inode(&mut self) -> Result<u32, FsError> {
		for num in 1..self.num_inodes {
			if self.read_inode(num)?.kind == KIND_FREE {
				self.write_inode(num, &mut Inode::empty(KIND_FILE))?;
				return Ok(num);
			}
		}
		Err(FsError::NoSpace)
	}

	// block allocation (copy-on-write)

	fn is_alloc(&self, block: u64) -> bool {
		self.free[(block / 8) as usize] & (1 << (block % 8)) != 0
	}

	// Claim one free block, marking it used and recording it as fresh (allocated by
	// this transaction, so safe to overwrite in place). Data blocks are taken from the
	// low end of the pool and metadata (checksum, extent-overflow, inode-table, index)
	// from the high end, so a run of data blocks stays physically contiguous and
	// coalesces into one extent instead of being split by interleaved metadata.
	fn alloc_block(&mut self, meta: bool) -> Result<u64, FsError> {
		let claim = |free: &mut [u8], block: u64| {
			free[(block / 8) as usize] |= 1 << (block % 8);
		};
		if meta {
			let mut block = self.num_blocks;
			while block > POOL_START {
				block -= 1;
				if !self.is_alloc(block) {
					claim(&mut self.free, block);
					self.fresh.insert(block);
					return Ok(block);
				}
			}
		} else {
			for block in POOL_START..self.num_blocks {
				if !self.is_alloc(block) {
					claim(&mut self.free, block);
					self.fresh.insert(block);
					return Ok(block);
				}
			}
		}
		Err(FsError::NoSpace)
	}

	fn alloc_data(&mut self) -> Result<u64, FsError> {
		self.alloc_block(false)
	}

	fn alloc_meta(&mut self) -> Result<u64, FsError> {
		self.alloc_block(true)
	}

	// Copy-on-write a block reference. A pointer this transaction already allocated is
	// returned as is (safe to mutate in place). A committed block (or the 0 "unmapped"
	// sentinel) is copied up to a fresh block (data low, metadata high) and the old
	// contents copied into it (or zeroed), so the committed generation keeps the
	// original untouched.
	fn cow_block(&mut self, ptr: u64, meta: bool) -> Result<u64, FsError> {
		if ptr != 0 && self.fresh.contains(&ptr) {
			return Ok(ptr);
		}
		let fresh = self.alloc_block(meta)?;
		let mut buf = vec![0u8; BLOCK_SIZE];
		if ptr != 0 && !self.dev.read_block(ptr, &mut buf) {
			return Err(FsError::Io);
		}
		if !self.dev.write_block(fresh, &buf) {
			return Err(FsError::Io);
		}
		Ok(fresh)
	}

	fn cow_data(&mut self, ptr: u64) -> Result<u64, FsError> {
		self.cow_block(ptr, false)
	}

	fn cow_meta(&mut self, ptr: u64) -> Result<u64, FsError> {
		self.cow_block(ptr, true)
	}

	// Read the CRC32C of an extent's block at slot `slot` from its checksum block,
	// verifying that block's own checksum first (so a flipped bit in the checksum
	// metadata is caught, not silently trusted).
	fn read_csum(&mut self, csum: u64, csum_crc: u32, slot: usize) -> Result<u32, FsError> {
		let mut buf = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(csum, &mut buf) {
			return Err(FsError::Io);
		}
		if crc32c(&buf) != csum_crc {
			return Err(FsError::Corrupt);
		}
		let off = slot * 4;
		Ok(u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()))
	}

	// Set slot `slot` of checksum block `csum` to `crc` and return the block's new
	// CRC32C (the extent's `csum_crc`). The block is read, edited, and written back.
	fn set_csum_slot(&mut self, csum: u64, slot: usize, crc: u32) -> Result<u32, FsError> {
		let mut buf = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(csum, &mut buf) {
			return Err(FsError::Io);
		}
		let off = slot * 4;
		buf[off..off + 4].copy_from_slice(&crc.to_le_bytes());
		if !self.dev.write_block(csum, &buf) {
			return Err(FsError::Io);
		}
		Ok(crc32c(&buf))
	}

	// file block mapping (extents)

	// Read logical block `logical` of `inode` into `buf` via its extent map, verifying
	// the per-block checksum. Returns false (and leaves `buf` untouched) for a hole - a
	// logical block no extent covers, which the caller reads back as zeros. A checksum
	// mismatch is `FsError::Corrupt`.
	fn read_logical(&mut self, inode: &Inode, logical: usize, buf: &mut [u8]) -> Result<bool, FsError> {
		let lb = logical as u64;
		let ext = match find_extent(&inode.extents, lb) {
			Some(i) => inode.extents[i],
			None => return Ok(false),
		};
		let off = (lb - ext.logical) as usize;
		if !self.dev.read_block(ext.physical + off as u64, buf) {
			return Err(FsError::Io);
		}
		let crc = self.read_csum(ext.csum, ext.csum_crc, off)?;
		if crc32c(buf) != crc {
			return Err(FsError::Corrupt);
		}
		Ok(true)
	}

	// Write `buf` as logical block `logical` of `inode`, updating the extent map in
	// memory and recording the block's checksum. Overwriting a mapped block copies it
	// up (and may split its run); writing a hole appends to the run before it when the
	// new block is physically contiguous, otherwise starts a new run. The caller
	// persists the inode, which flushes the map to disk.
	fn write_logical(&mut self, inode: &mut Inode, logical: usize, buf: &[u8]) -> Result<(), FsError> {
		let lb = logical as u64;
		let crc = crc32c(buf);
		if let Some(i) = find_extent(&inode.extents, lb) {
			let ext = inode.extents[i];
			let off = (lb - ext.logical) as usize;
			let new_phys = self.cow_data(ext.physical + off as u64)?;
			if !self.dev.write_block(new_phys, buf) {
				return Err(FsError::Io);
			}
			self.overwrite_block(inode, i, off, new_phys, crc)?;
			return Ok(());
		}
		let phys = self.alloc_data()?;
		if !self.dev.write_block(phys, buf) {
			return Err(FsError::Io);
		}
		self.place_block(inode, lb, phys, crc)
	}

	// Record a freshly allocated data block `phys` as logical block `lb` of `inode`,
	// extending the run that ends at `lb` when it is physically contiguous and still has
	// room in its checksum block, or inserting a new single-block run otherwise.
	fn place_block(&mut self, inode: &mut Inode, lb: u64, phys: u64, crc: u32) -> Result<(), FsError> {
		let pos = inode.extents.partition_point(|e| e.logical <= lb);
		if pos > 0 {
			let prev = inode.extents[pos - 1];
			if prev.end() == lb && prev.physical + prev.length as u64 == phys && (prev.length as usize) < CRCS_PER_BLOCK {
				let csum = self.cow_meta(prev.csum)?;
				let csum_crc = self.set_csum_slot(csum, prev.length as usize, crc)?;
				let e = &mut inode.extents[pos - 1];
				e.length += 1;
				e.csum = csum;
				e.csum_crc = csum_crc;
				return Ok(());
			}
		}
		let csum = self.alloc_meta()?;
		let mut cbuf = vec![0u8; BLOCK_SIZE];
		cbuf[0..4].copy_from_slice(&crc.to_le_bytes());
		if !self.dev.write_block(csum, &cbuf) {
			return Err(FsError::Io);
		}
		inode.extents.insert(pos, Extent { logical: lb, physical: phys, length: 1, csum, csum_crc: crc32c(&cbuf) });
		Ok(())
	}

	// Apply an overwrite of the block at offset `off` in extent `i`, now living at
	// `new_phys`. If the block did not move (it was already fresh this transaction) the
	// run is intact and only its checksum changes; otherwise the run splits into the
	// unchanged prefix, the single rewritten block, and the unchanged suffix, copying
	// the checksum sub-ranges so every block keeps its CRC.
	fn overwrite_block(&mut self, inode: &mut Inode, i: usize, off: usize, new_phys: u64, crc: u32) -> Result<(), FsError> {
		let ext = inode.extents[i];
		if new_phys == ext.physical + off as u64 {
			let csum = self.cow_meta(ext.csum)?;
			let csum_crc = self.set_csum_slot(csum, off, crc)?;
			let e = &mut inode.extents[i];
			e.csum = csum;
			e.csum_crc = csum_crc;
			return Ok(());
		}
		let mut old_csum = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(ext.csum, &mut old_csum) {
			return Err(FsError::Io);
		}
		if crc32c(&old_csum) != ext.csum_crc {
			return Err(FsError::Corrupt);
		}
		let mut pieces: Vec<Extent> = Vec::new();
		if off > 0 {
			// the prefix is unchanged: reuse the original checksum block (its leading
			// slots still match the kept blocks).
			pieces.push(Extent { logical: ext.logical, physical: ext.physical, length: off as u32, csum: ext.csum, csum_crc: ext.csum_crc });
		}
		// the rewritten block gets a fresh single-entry checksum block.
		let mid_csum = self.alloc_meta()?;
		let mut cbuf = vec![0u8; BLOCK_SIZE];
		cbuf[0..4].copy_from_slice(&crc.to_le_bytes());
		if !self.dev.write_block(mid_csum, &cbuf) {
			return Err(FsError::Io);
		}
		pieces.push(Extent { logical: ext.logical + off as u64, physical: new_phys, length: 1, csum: mid_csum, csum_crc: crc32c(&cbuf) });
		if off + 1 < ext.length as usize {
			let slen = ext.length as usize - off - 1;
			let suf_csum = self.alloc_meta()?;
			let mut sbuf = vec![0u8; BLOCK_SIZE];
			// copy the original CRCs of the suffix down to the start of the new block.
			sbuf[0..slen * 4].copy_from_slice(&old_csum[(off + 1) * 4..(off + 1 + slen) * 4]);
			if !self.dev.write_block(suf_csum, &sbuf) {
				return Err(FsError::Io);
			}
			pieces.push(Extent { logical: ext.logical + off as u64 + 1, physical: ext.physical + off as u64 + 1, length: slen as u32, csum: suf_csum, csum_crc: crc32c(&sbuf) });
		}
		inode.extents.splice(i..i + 1, pieces);
		Ok(())
	}

	// Count the live data blocks of `inode` whose on-disk bytes no longer match the
	// CRC32C stored for them in their run's checksum block. A run whose checksum block
	// is itself corrupt counts as wholly bad.
	fn count_corrupt(&mut self, inode: &Inode) -> Result<u32, FsError> {
		let mut bad = 0;
		let mut buf = vec![0u8; BLOCK_SIZE];
		let mut cbuf = vec![0u8; BLOCK_SIZE];
		for ext in inode.extents.iter() {
			if !self.dev.read_block(ext.csum, &mut cbuf) {
				return Err(FsError::Io);
			}
			if crc32c(&cbuf) != ext.csum_crc {
				bad += ext.length;
				continue;
			}
			for off in 0..ext.length as usize {
				if !self.dev.read_block(ext.physical + off as u64, &mut buf) {
					return Err(FsError::Io);
				}
				let c = u32::from_le_bytes(cbuf[off * 4..off * 4 + 4].try_into().unwrap());
				if crc32c(&buf) != c {
					bad += 1;
				}
			}
		}
		Ok(bad)
	}

	// path resolution

	// Resolve a full path to its inode number, walking directories from the root.
	fn resolve(&mut self, path: &[u8]) -> Result<u32, FsError> {
		let segs = split_segments(path)?;
		let mut inode_num = self.root_inode;
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
		let mut parent = self.root_inode;
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
		self.write_inode(num, &mut dir)?;
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
					self.write_inode(dir_num, &mut dir)?;
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
		self.write_inode(dir_num, &mut dir)?;
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
			self.write_inode(dir_num, &mut dir)?;
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
		self.write_inode(dir_num, &mut dir)?;
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

	// Drop the file's blocks from logical block `keep` to the end: runs wholly past the
	// cut are removed, a run straddling it is shortened. Under copy-on-write nothing is
	// marked free here - the dropped data, checksum, and overflow blocks simply stop
	// being referenced by the new generation and are reclaimed when the free map is
	// rederived at commit (until then the previous generation still pins them as a
	// snapshot). A shortened run keeps its checksum block; its leading slots still match
	// the kept blocks.
	fn free_from(&mut self, inode: &mut Inode, keep: usize) -> Result<(), FsError> {
		let keep = keep as u64;
		let mut kept: Vec<Extent> = Vec::new();
		for ext in inode.extents.iter() {
			if ext.logical >= keep {
				continue;
			}
			if ext.end() <= keep {
				kept.push(*ext);
				continue;
			}
			let mut e = *ext;
			e.length = (keep - ext.logical) as u32;
			kept.push(e);
		}
		inode.extents = kept;
		Ok(())
	}

	// Set the bitmap bit for every block an inode references: each run's data blocks and
	// its checksum block, plus the blocks of the extent overflow chain.
	fn collect_inode_blocks(&mut self, inode: &Inode, bitmap: &mut [u8]) -> Result<(), FsError> {
		for ext in inode.extents.iter() {
			for off in 0..ext.length as u64 {
				set_bit(bitmap, ext.physical + off);
			}
			if ext.csum != 0 {
				set_bit(bitmap, ext.csum);
			}
		}
		let mut ptr = inode.spill;
		let mut buf = vec![0u8; BLOCK_SIZE];
		while ptr != 0 {
			set_bit(bitmap, ptr);
			if !self.dev.read_block(ptr, &mut buf) {
				return Err(FsError::Io);
			}
			ptr = u64::from_le_bytes(buf[0..8].try_into().unwrap());
		}
		Ok(())
	}
}

// Render a superblock to a fresh BLOCK_SIZE block. The self-CRC covers the whole
// block with its own four bytes zeroed, so a torn write (any byte wrong) fails it on
// mount and the slot is rejected.
fn serialize_superblock(sb: &Superblock) -> Vec<u8> {
	let mut block = vec![0u8; BLOCK_SIZE];
	block[0..8].copy_from_slice(&MAGIC);
	block[8..12].copy_from_slice(&VERSION.to_le_bytes());
	block[12..16].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
	block[16..24].copy_from_slice(&sb.num_blocks.to_le_bytes());
	block[24..28].copy_from_slice(&sb.num_inodes.to_le_bytes());
	block[28..36].copy_from_slice(&sb.generation.to_le_bytes());
	block[36..44].copy_from_slice(&sb.itable_index.to_le_bytes());
	block[44..48].copy_from_slice(&sb.itable_index_crc.to_le_bytes());
	block[48..52].copy_from_slice(&sb.inode_blocks.to_le_bytes());
	block[52..56].copy_from_slice(&sb.root_inode.to_le_bytes());
	// the CRC bytes are already zero; checksum the block and store it over them.
	let crc = crc32c(&block);
	block[SB_CRC_OFFSET..SB_CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
	block
}

// Parse and validate a superblock block: it must carry the LiberFS magic and version,
// match this build's block size, and pass its own CRC32C. Returns None otherwise (an
// unformatted slot, a foreign disk, or a torn commit).
fn parse_superblock(block: &[u8]) -> Option<Superblock> {
	if block.len() < BLOCK_SIZE {
		return None;
	}
	if block[0..8] != MAGIC {
		return None;
	}
	if u32::from_le_bytes(block[8..12].try_into().ok()?) != VERSION {
		return None;
	}
	if u32::from_le_bytes(block[12..16].try_into().ok()?) != BLOCK_SIZE as u32 {
		return None;
	}
	// verify the self-CRC by recomputing over the block with its CRC bytes zeroed.
	let stored = u32::from_le_bytes(block[SB_CRC_OFFSET..SB_CRC_OFFSET + 4].try_into().ok()?);
	let mut probe = block[..BLOCK_SIZE].to_vec();
	probe[SB_CRC_OFFSET..SB_CRC_OFFSET + 4].fill(0);
	if crc32c(&probe) != stored {
		return None;
	}
	Some(Superblock { num_blocks: u64::from_le_bytes(block[16..24].try_into().ok()?), num_inodes: u32::from_le_bytes(block[24..28].try_into().ok()?), inode_blocks: u32::from_le_bytes(block[48..52].try_into().ok()?), generation: u64::from_le_bytes(block[28..36].try_into().ok()?), itable_index: u64::from_le_bytes(block[36..44].try_into().ok()?), itable_index_crc: u32::from_le_bytes(block[44..48].try_into().ok()?), root_inode: u32::from_le_bytes(block[52..56].try_into().ok()?) })
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
fn set_bit(bitmap: &mut [u8], b: u64) {
	bitmap[(b / 8) as usize] |= 1 << (b % 8);
}

// Index of the extent covering logical block `lb`, or None if it falls in a hole. The
// runs are sorted by `logical`, so the candidate is the last one starting at or before
// `lb`; a binary search keeps lookups cheap on a many-extent file.
fn find_extent(extents: &[Extent], lb: u64) -> Option<usize> {
	let pos = extents.partition_point(|e| e.logical <= lb);
	if pos == 0 {
		return None;
	}
	if extents[pos - 1].covers(lb) {
		Some(pos - 1)
	} else {
		None
	}
}

// Split a path into its validated segments. Each segment must be non-empty, no longer
// than NAME_MAX, neither "." nor "..", and free of NUL bytes - so a resolved path can
// never escape the volume or name an invalid entry. A portable-name policy is enforced
// at this boundary: the cross-platform-unsafe set (`\ : * ? < > | "` and control bytes)
// is rejected on top of `/` and NUL, so a LiberFS name moves cleanly to FAT / NTFS media
// and other systems.
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
		if seg.iter().any(|&c| !is_portable_name_byte(c)) {
			return Err(FsError::Invalid);
		}
		segs.push(seg);
	}
	Ok(segs)
}

// Is byte `c` allowed in a portable file name? Rejects NUL and control bytes (0x00..=0x1F
// and 0x7F) and the cross-platform-reserved set `\ : * ? < > | "`. (`/` never reaches
// here - it is the path separator.)
fn is_portable_name_byte(c: u8) -> bool {
	if c < 0x20 || c == 0x7F {
		return false;
	}
	!matches!(c, b'\\' | b':' | b'*' | b'?' | b'<' | b'>' | b'|' | b'"')
}

#[cfg(test)]
mod tests;
