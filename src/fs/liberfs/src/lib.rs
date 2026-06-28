//! LiberFS - a small writable, copy-on-write on-disk filesystem for LiberSystem.
//!
//! The on-disk layout is a Unix-flavoured filesystem turned copy-on-write: two
//! superblock slots at blocks 0 and 1, then one flat pool of blocks (block 2 onward)
//! out of which the inode B+tree, directory B+trees, file data, and the per-extent
//! checksum blocks are all allocated. Block addresses are 64-bit, so a volume scales
//! from gigabytes into exabytes. Inodes are not a fixed table: they live in a B+tree
//! keyed by inode number (a node per block, copy-on-write and checksummed), allocated
//! on demand, so a volume never runs out of inodes while it has free space and an
//! empty one wastes none. Each directory is its own B+tree keyed by the hash of an
//! entry's name, so lookup, insert and remove are O(log n) and a directory holds
//! millions of entries without a linear scan. A file maps its data with extents -
//! each a contiguous run of blocks paired with one checksum block - held inline in the
//! inode and spilling to an overflow chain when there are many, so a file grows from a
//! few blocks to hundreds of gigabytes and an unwritten range simply has no extent (a
//! sparse hole that reads back as zeros). A run whose bytes shrink is transparently
//! compressed, stored across fewer blocks, and falls back to raw when they do not. Every
//! stored block is paired with a CRC32C, kept in its extent's checksum block, and every
//! tree node with its own CRC32C kept in the parent link, so on-disk corruption is caught
//! when the block is read. Each inode
//! also reserves an opaque owner tag (stored, never interpreted: authorization lives in
//! the capability layer and StorageService, not in the filesystem). There is no on-disk
//! allocation bitmap: the free map is reconstructed in memory at mount from the blocks
//! the live generations reference. It backs the `Storage.Volume` API and survives a
//! reboot.
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
//! inode- and directory-B+tree nodes on the path to it are each written to a freshly
//! allocated block (copied up once per transaction, then updated in place). The
//! transaction commits with a single atomic
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
//! read-only.
//!
//! A NAMED snapshot keeps any earlier generation reachable for as long as wanted:
//! [`LiberFs::create_snapshot`] records the live generation's inode-tree root in a
//! snapshot table the superblock points at, [`LiberFs::list_snapshots`] enumerates
//! them, and [`LiberFs::delete_snapshot`] drops one. The free-map walk reserves every
//! pinned generation, so their blocks are never reused until the snapshot is deleted;
//! [`LiberFs::mount_named_snapshot`] re-roots a read-only mount at a snapshot to read
//! that earlier state. The generation before last (if unnamed) is reclaimed by the
//! next commit.
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
//!
//! ## Compression (transparent, per extent)
//!
//! A whole-file write compresses each of its runs with a small, dependency-free LZSS
//! coder ([`lz_compress`]): a run whose bytes shrink to fewer blocks is stored as a
//! compressed extent - the compressed stream packed into a contiguous run of stored
//! blocks, the original block span kept as the extent's logical `length` - while an
//! incompressible run is left raw. Reads decode the extent transparently, so a file
//! reads back identically whether or not it compressed. The per-block CRC32C covers the
//! stored (compressed) bytes, so integrity and `fsck` work unchanged. Editing a
//! compressed file thaws the touched run back to raw blocks (a later whole-file write
//! recompresses it), keeping partial writes simple. Compression is a space optimization
//! only: it never changes a file's contents or the `Storage.Volume` API.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::collections::BTreeSet;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Ordering;

// One filesystem block. Eight 512-byte disk sectors, a page; the I/O unit of the
// BlockDevice trait.
pub const BLOCK_SIZE: usize = 4096;

// On-disk superblock magic and format version. Mount rejects anything else (a fresh
// or stale-format disk), so StorageService knows to reformat. Version 1 is the
// copy-on-write, extent-mapped layout: two superblock slots, a flat block pool with no
// on-disk bitmap, 64-bit block addresses, an inode B+tree keyed by number, directories
// that are name-keyed B+trees, files mapped by extents (each a contiguous run with its
// own checksum block) and sparse holes, per-inode timestamps and an opaque owner tag,
// and a CRC32C paired with every block pointer.
const MAGIC: [u8; 8] = *b"LIBERFS1";
const VERSION: u32 = 1;

// The two superblock slots (blocks 0 and 1): a commit writes the new superblock to the
// inactive slot, so the active one survives a torn write. The block pool begins right
// after them.
const SUPER_SLOTS: u32 = 2;
const POOL_START: u64 = SUPER_SLOTS as u64;

// One inode is a fixed 256-byte slot: a kind byte, a size, two timestamps, then either
// (for a file) the extent map's overflow pointer and count and EXTENTS_INLINE inline
// extents, or (for a directory) its B+tree root pointer and that root's CRC32C. An
// opaque owner tag sits at OWNER_TAG_OFF. Each slot is stored, keyed by inode number,
// in a leaf of the inode B+tree.
const INODE_SIZE: usize = 256;

// A B+tree node lives in one block: an 8-byte header (a type byte then a u16 entry
// count) followed by the entries. An internal node holds `count` u64 separator keys
// then `count + 1` child links, each a block pointer (u64) and that block's CRC32C
// (u32); a leaf holds `count` fixed-width records, each beginning with its u64 key.
// Nodes are copy-on-write and every child link carries the child's checksum.
const NODE_INTERNAL: u8 = 0;
const NODE_LEAF: u8 = 1;
const NODE_HDR: usize = 8;
const SEP_SIZE: usize = 8;
const CHILD_SIZE: usize = 12;
// Maximum children of an internal node: header + (C - 1) separators + C child links fit
// one block. The separators occupy the (C - 1)-slot region right after the header and
// the child links a fixed region after it, so offsets do not depend on the live count.
const INTERNAL_MAX: usize = (BLOCK_SIZE - NODE_HDR + SEP_SIZE) / (SEP_SIZE + CHILD_SIZE);
const INTERNAL_CHILD_BASE: usize = NODE_HDR + SEP_SIZE * (INTERNAL_MAX - 1);

// An inode-tree leaf record: the inode number (u64 key) then its 256-byte slot. The key
// is compared on its own 8 bytes, since inode numbers are unique.
const INODE_REC: usize = 8 + INODE_SIZE;
const INODE_LEAF_MAX: usize = (BLOCK_SIZE - NODE_HDR) / INODE_REC;
const INODE_KEYLEN: usize = 8;
// A reserved opaque owner / ACL tag, stored in every inode but never interpreted by the
// filesystem: authorization is the capability layer and StorageService, not POSIX
// permissions. Room to grow into a real owner identity without another format change.
const OWNER_TAG_LEN: usize = 16;
const OWNER_TAG_OFF: usize = 56;

// A file is mapped by EXTENTS: each is a contiguous run of blocks (a logical start, a
// physical start, a length) with one checksum block holding a CRC32C per stored block in
// the run. One extent record is 40 bytes on disk: logical (u64), physical (u64), length
// (u32), the checksum block's own CRC32C (u32), the checksum block pointer (u64), then
// the stored-block count (u32) and the compressed byte length (u32). A raw run stores
// `length` blocks (its stored count equals `length` and its compressed length is 0); a
// transparently compressed run stores fewer blocks holding the compressed bytes of the
// whole `length`-block span (see [`LiberFs::compress_inode`]).
const EXTENT_SIZE: usize = 40;
// Byte offset of the first inline extent: past the fixed header (kind, size, two
// timestamps, the extent-overflow pointer and count) and the owner tag.
const EXTENT_OFF: usize = OWNER_TAG_OFF + OWNER_TAG_LEN;
// (256 - 72) / 40 = 4 extents live inline in the inode; a file of up to four runs needs
// no overflow block at all. Beyond that, extents spill to a chain of extent blocks.
const EXTENTS_INLINE: usize = (INODE_SIZE - EXTENT_OFF) / EXTENT_SIZE;
// A checksum block holds one CRC32C (4 bytes) per stored block of its extent, so an
// extent stores at most this many blocks (1024 = 4 MiB) and spans at most that many
// logical blocks. A longer file is several extents.
const CRCS_PER_BLOCK: usize = BLOCK_SIZE / 4;
// An extent-overflow block: an 8-byte next-block pointer, its 4-byte CRC32C, a 4-byte
// count, then the extent records. (4096 - 16) / 40 = 102 extents per overflow block.
const EXTENT_HDR: usize = 16;
const EXTENTS_PER_BLOCK: usize = (BLOCK_SIZE - EXTENT_HDR) / EXTENT_SIZE;

// Transparent per-extent compression uses a small, dependency-free LZ77 / LZSS coder (no
// external crate, no_std). A control byte's eight bits flag the next eight items as a
// literal byte or a back-reference; a back-reference is a 12-bit distance (1..=4096, the
// sliding window) and a 4-bit length (LZ_MIN_MATCH..=LZ_MAX_MATCH). The compressed stream
// begins with the uncompressed length (u32, little-endian) so it decodes without external
// size metadata. A compressed extent stores this stream across whole blocks, each with
// its own CRC32C, so the integrity checks cover the stored (compressed) bytes.
const LZ_WINDOW: usize = 4096;
const LZ_MIN_MATCH: usize = 3;
const LZ_MAX_MATCH: usize = LZ_MIN_MATCH + 15;
const LZ_HASH_BITS: usize = 13;
const LZ_HASH_SIZE: usize = 1 << LZ_HASH_BITS;
// How many earlier positions sharing a 3-byte prefix to test for the longest match: a
// bounded chain keeps compression roughly linear while still finding most matches.
const LZ_MAX_CHAIN: usize = 32;

// Inode kinds. A live inode record is always a file or a directory; a freed inode is
// deleted from the tree rather than tombstoned, so there is no "free" kind.
const KIND_FILE: u8 = 1;
const KIND_DIR: u8 = 2;

// The root directory is inode 0; other inodes are handed out from a monotonic counter
// (`next_inode`) starting at 1, so a number is never reused and the inode B+tree holds
// only live inodes.
const ROOT_INODE: u32 = 0;

// A directory is a B+tree keyed by the hash of an entry's name. One leaf record is the
// name hash (u64 key), the NUL-padded name, then the child inode number (u32); records
// sort by (hash, name), so the key portion compared in a leaf is the hash plus the name.
// A full 255-byte name fills the whole name field with no terminator.
const NAME_MAX: usize = 255;
const DIR_REC: usize = 8 + NAME_MAX + 4;
const DIR_LEAF_MAX: usize = (BLOCK_SIZE - NODE_HDR) / DIR_REC;
const DIR_KEYLEN: usize = 8 + NAME_MAX;

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
	// Monotonic generation: a commit writes the new superblock with `generation + 1`,
	// so the newest valid slot is the live one and the other is the snapshot.
	generation: u64,
	// Root block of the inode B+tree and that root node's CRC32C; the tree is reached
	// from here rather than from a fixed inode region. 0 would mean an empty tree, which
	// never happens past format (format seeds the root directory as inode 0).
	inode_root: u64,
	inode_root_crc: u32,
	// The next inode number to hand out (monotonic; never reused), so the inode tree
	// holds only live inodes and a volume never runs out of inode numbers in practice.
	next_inode: u32,
	root_inode: u32,
	// The snapshot table: the block holding the named snapshots (0 = none) and that
	// block's CRC32C. Carried in the superblock so the pinned snapshots commit atomically
	// with the generation and survive a remount.
	snap_root: u64,
	snap_root_crc: u32,
}

// Byte offset of the superblock's own CRC32C within its block; the checksum covers the
// whole block with these four bytes zeroed, so a half-written superblock fails it.
const SB_CRC_OFFSET: usize = 56;

// A named snapshot pins an earlier generation's inode-tree root so its blocks are not
// reclaimed. The snapshot table is the one block `snap_root` points at: a u32 count and
// a u32 pad, then fixed records of a NUL-padded name, the pinned inode-tree root and its
// CRC32C, and the generation. (4096 - 8) / 84 = 48 snapshots fit one block.
const SNAP_NAME_MAX: usize = 64;
const SNAP_HDR: usize = 8;
const SNAP_REC: usize = SNAP_NAME_MAX + 20;
const SNAP_MAX: usize = (BLOCK_SIZE - SNAP_HDR) / SNAP_REC;

// One extent: a contiguous run of `length` logical blocks mapped from logical block
// `logical` to physical block `physical`, paired with a checksum block (`csum`) holding
// the CRC32C of every stored block in the run, plus `csum_crc`, that checksum block's own
// CRC32C. A run is either raw (`clen` == 0, `store_len` == `length`, one physical block
// per logical block) or transparently compressed (`clen` > 0, `store_len` < `length`, the
// `store_len` physical blocks holding the `clen`-byte compressed stream of the whole
// span). A file's extents are kept sorted by `logical`; a logical block no extent covers
// is a hole that reads back as zeros (so a sparse file costs only its written runs).
#[derive(Clone, Copy)]
struct Extent {
	logical: u64,
	physical: u64,
	length: u32,
	csum: u64,
	csum_crc: u32,
	// Stored (physical) blocks of the run: equals `length` for a raw run, fewer for a
	// compressed one. The checksum block holds one CRC32C per stored block.
	store_len: u32,
	// Compressed byte length: 0 for a raw run, else the length of the compressed stream
	// held across the `store_len` stored blocks.
	clen: u32,
}

impl Extent {
	fn parse(buf: &[u8]) -> Extent {
		Extent { logical: u64::from_le_bytes(buf[0..8].try_into().unwrap()), physical: u64::from_le_bytes(buf[8..16].try_into().unwrap()), length: u32::from_le_bytes(buf[16..20].try_into().unwrap()), csum_crc: u32::from_le_bytes(buf[20..24].try_into().unwrap()), csum: u64::from_le_bytes(buf[24..32].try_into().unwrap()), store_len: u32::from_le_bytes(buf[32..36].try_into().unwrap()), clen: u32::from_le_bytes(buf[36..40].try_into().unwrap()) }
	}

	fn write(&self, buf: &mut [u8]) {
		buf[0..8].copy_from_slice(&self.logical.to_le_bytes());
		buf[8..16].copy_from_slice(&self.physical.to_le_bytes());
		buf[16..20].copy_from_slice(&self.length.to_le_bytes());
		buf[20..24].copy_from_slice(&self.csum_crc.to_le_bytes());
		buf[24..32].copy_from_slice(&self.csum.to_le_bytes());
		buf[32..36].copy_from_slice(&self.store_len.to_le_bytes());
		buf[36..40].copy_from_slice(&self.clen.to_le_bytes());
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

// One inode, parsed from / rendered to its 256-byte on-disk slot. A file and a directory
// share the header (kind, size, two timestamps, owner tag) but overlay the rest: a file
// keeps its extent map (the inline runs plus the `spill` overflow pointer and the total
// `extent_count`), while a directory keeps its B+tree root (`dir_root` and that root's
// `dir_root_crc`) in the same bytes and leaves the extent fields zero. `extents` is the
// in-memory extent map of a file: `parse` fills only the EXTENTS_INLINE inline runs, and
// [`LiberFs::read_inode`] completes it from the overflow chain rooted at `spill`.
struct Inode {
	kind: u8,
	size: u64,
	ctime: u64,
	mtime: u64,
	// An opaque owner / ACL tag, stored but never interpreted by the filesystem.
	owner_tag: [u8; OWNER_TAG_LEN],
	// File mapping: the extent runs, the overflow chain pointer, and the total run count.
	extents: Vec<Extent>,
	spill: u64,
	spill_crc: u32,
	extent_count: u32,
	// Directory mapping: the root block of this directory's name-keyed B+tree and that
	// root node's CRC32C (0 / 0 for an empty directory). Overlaid on the file fields.
	dir_root: u64,
	dir_root_crc: u32,
}

impl Inode {
	fn empty(kind: u8) -> Inode {
		Inode { kind, size: 0, ctime: 0, mtime: 0, owner_tag: [0u8; OWNER_TAG_LEN], extents: Vec::new(), spill: 0, spill_crc: 0, extent_count: 0, dir_root: 0, dir_root_crc: 0 }
	}

	// Parse the fixed header and, for a file, the inline extents (any spilled ones are
	// appended afterwards by `read_inode`); for a directory, the B+tree root pointer.
	fn parse(buf: &[u8]) -> Inode {
		let kind = buf[0];
		let mut owner_tag = [0u8; OWNER_TAG_LEN];
		owner_tag.copy_from_slice(&buf[OWNER_TAG_OFF..OWNER_TAG_OFF + OWNER_TAG_LEN]);
		let mut inode = Inode { kind, size: u64::from_le_bytes(buf[8..16].try_into().unwrap()), ctime: u64::from_le_bytes(buf[16..24].try_into().unwrap()), mtime: u64::from_le_bytes(buf[24..32].try_into().unwrap()), owner_tag, extents: Vec::new(), spill: 0, spill_crc: 0, extent_count: 0, dir_root: 0, dir_root_crc: 0 };
		if kind == KIND_DIR {
			inode.dir_root = u64::from_le_bytes(buf[32..40].try_into().unwrap());
			inode.dir_root_crc = u32::from_le_bytes(buf[40..44].try_into().unwrap());
		} else {
			inode.spill = u64::from_le_bytes(buf[32..40].try_into().unwrap());
			inode.spill_crc = u32::from_le_bytes(buf[40..44].try_into().unwrap());
			inode.extent_count = u32::from_le_bytes(buf[44..48].try_into().unwrap());
			let inline = (inode.extent_count as usize).min(EXTENTS_INLINE);
			inode.extents.reserve(inline);
			for i in 0..inline {
				let off = EXTENT_OFF + i * EXTENT_SIZE;
				inode.extents.push(Extent::parse(&buf[off..off + EXTENT_SIZE]));
			}
		}
		inode
	}

	// Render the header into the 256-byte slot, then either the file's overflow fields
	// and first EXTENTS_INLINE extents or the directory's B+tree root. For a file, the
	// `spill` / `spill_crc` / `extent_count` fields and the overflow chain are set
	// beforehand by [`LiberFs::flush_extents`], which `write_inode` always calls first.
	fn write(&self, buf: &mut [u8]) {
		for b in buf[..INODE_SIZE].iter_mut() {
			*b = 0;
		}
		buf[0] = self.kind;
		buf[8..16].copy_from_slice(&self.size.to_le_bytes());
		buf[16..24].copy_from_slice(&self.ctime.to_le_bytes());
		buf[24..32].copy_from_slice(&self.mtime.to_le_bytes());
		buf[OWNER_TAG_OFF..OWNER_TAG_OFF + OWNER_TAG_LEN].copy_from_slice(&self.owner_tag);
		if self.kind == KIND_DIR {
			buf[32..40].copy_from_slice(&self.dir_root.to_le_bytes());
			buf[40..44].copy_from_slice(&self.dir_root_crc.to_le_bytes());
		} else {
			buf[32..40].copy_from_slice(&self.spill.to_le_bytes());
			buf[40..44].copy_from_slice(&self.spill_crc.to_le_bytes());
			buf[44..48].copy_from_slice(&self.extent_count.to_le_bytes());
			for (i, ext) in self.extents.iter().take(EXTENTS_INLINE).enumerate() {
				let off = EXTENT_OFF + i * EXTENT_SIZE;
				ext.write(&mut buf[off..off + EXTENT_SIZE]);
			}
		}
	}

	// Number of data blocks the file's `size` occupies.
	fn nblocks(&self) -> usize {
		(self.size as usize).div_ceil(BLOCK_SIZE)
	}
}

// The outcome of inserting into a B+tree subtree: either the node was rewritten in
// place (its new (ptr, crc)) or it split into two, lifting a separator key to the
// parent: left (ptr, crc), the separator, right (ptr, crc).
enum Ins {
	Updated(u64, u32),
	Split(u64, u32, u64, u64, u32),
}

// The outcome of deleting from a B+tree subtree: the key was not present, the node was
// rewritten (its new (ptr, crc)), or the node emptied and the parent should drop it.
enum Del {
	NotFound,
	Updated(u64, u32),
	Empty,
}

// One named, pinned snapshot in memory: the inode-tree root (and its CRC32C) of the
// generation it captured, kept reserved by the free-map walk so a later commit never
// reuses its blocks. Loaded from the snapshot table at mount.
#[derive(Clone)]
struct Snapshot {
	name: Vec<u8>,
	inode_root: u64,
	inode_root_crc: u32,
	generation: u64,
}

// The filesystem state captured at `begin`: the inode-tree root and next-inode counter,
// plus the snapshot table. `abort` restores it and `commit` reserves the generation it
// supersedes from it, so a rolled-back or committed snapshot create / delete leaves the
// in-memory state consistent with the disk.
struct Txn {
	inode_root: u64,
	inode_root_crc: u32,
	next_inode: u32,
	snap_root: u64,
	snap_root_crc: u32,
	snapshots: Vec<Snapshot>,
}

// A mounted LiberFS over a block device. Copy-on-write: the inodes are reached through
// the in-memory root of the inode B+tree (`inode_root` and its CRC32C) rather than a
// fixed region, and `free` is rebuilt at mount from the blocks the live and previous
// generations reference - there is no on-disk bitmap. `next_inode` hands out fresh inode
// numbers monotonically. `clock` is a logical timestamp the caller can advance (no wall
// clock lives in this crate); mutations stamp inode `mtime` from it.
//
// A mutation runs as a transaction: `begin` snapshots the inode-tree root and
// `next_inode`, the body allocates fresh blocks (tracked in `fresh`) and copies metadata
// up the trees, and `commit` writes a new superblock to the inactive slot - or `abort`
// rolls back. The previous generation's root stays reserved so it remains a read-only
// snapshot.
pub struct LiberFs<D: BlockDevice> {
	dev: D,
	num_blocks: u64,
	root_inode: u32,
	// Live generation: its number and the superblock slot (0 or 1) it occupies.
	generation: u64,
	slot: u32,
	// The inode B+tree: the root node's block and CRC32C, plus the next inode number to
	// hand out.
	inode_root: u64,
	inode_root_crc: u32,
	next_inode: u32,
	// The previous generation (the read-only snapshot), if any: its inode-tree root, kept
	// reserved so a commit does not reuse its blocks.
	prev_inode_root: u64,
	prev_inode_root_crc: u32,
	prev_valid: bool,
	// The snapshot table: the block the superblock points at (`snap_root` and its CRC32C)
	// and the named snapshots loaded from it, each pinning an earlier generation's root
	// so the free-map walk keeps its blocks reserved.
	snap_root: u64,
	snap_root_crc: u32,
	snapshots: Vec<Snapshot>,
	// In-memory free map, one bit per block, derived at mount and after each commit -
	// never written to disk.
	free: Vec<u8>,
	// Blocks allocated by the in-flight transaction: safe to overwrite in place (no
	// committed generation references them yet).
	fresh: BTreeSet<u64>,
	// The state captured at `begin`, restored by `abort` and used by `commit` to reserve
	// the generation it supersedes.
	txn: Option<Txn>,
	// A one-extent cache of the most recently decompressed run, keyed by its first stored
	// block, so a sequential read of a compressed extent decodes it only once.
	decomp: Option<(u64, Vec<u8>)>,
	clock: u64,
}

mod blkalloc;
mod dir;
mod fsck;
mod fsops;
mod inode;
mod snapshot;
mod txn;

// Render a superblock to a fresh BLOCK_SIZE block. The self-CRC covers the whole
// block with its own four bytes zeroed, so a torn write (any byte wrong) fails it on
// mount and the slot is rejected.
fn serialize_superblock(sb: &Superblock) -> Vec<u8> {
	let mut block = vec![0u8; BLOCK_SIZE];
	block[0..8].copy_from_slice(&MAGIC);
	block[8..12].copy_from_slice(&VERSION.to_le_bytes());
	block[12..16].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
	block[16..24].copy_from_slice(&sb.num_blocks.to_le_bytes());
	block[24..28].copy_from_slice(&sb.next_inode.to_le_bytes());
	block[28..36].copy_from_slice(&sb.generation.to_le_bytes());
	block[36..44].copy_from_slice(&sb.inode_root.to_le_bytes());
	block[44..48].copy_from_slice(&sb.inode_root_crc.to_le_bytes());
	block[52..56].copy_from_slice(&sb.root_inode.to_le_bytes());
	// the snapshot-table pointer and its CRC32C sit past the self-CRC field, so they are
	// covered by the whole-block checksum below.
	block[60..68].copy_from_slice(&sb.snap_root.to_le_bytes());
	block[68..72].copy_from_slice(&sb.snap_root_crc.to_le_bytes());
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
	Some(Superblock { num_blocks: u64::from_le_bytes(block[16..24].try_into().ok()?), generation: u64::from_le_bytes(block[28..36].try_into().ok()?), inode_root: u64::from_le_bytes(block[36..44].try_into().ok()?), inode_root_crc: u32::from_le_bytes(block[44..48].try_into().ok()?), next_inode: u32::from_le_bytes(block[24..28].try_into().ok()?), root_inode: u32::from_le_bytes(block[52..56].try_into().ok()?), snap_root: u64::from_le_bytes(block[60..68].try_into().ok()?), snap_root_crc: u32::from_le_bytes(block[68..72].try_into().ok()?) })
}

// The name held in a directory record's NUL-padded name field: up to the first NUL.
fn name_in(field: &[u8]) -> &[u8] {
	match field.iter().position(|&b| b == 0) {
		Some(end) => &field[..end],
		None => field,
	}
}

// FNV-1a 64-bit hash of an entry name: the B+tree key that orders a directory's entries.
fn name_hash(name: &[u8]) -> u64 {
	let mut h: u64 = 0xcbf2_9ce4_8422_2325;
	for &b in name {
		h ^= b as u64;
		h = h.wrapping_mul(0x0000_0100_0000_01b3);
	}
	h
}

// A directory probe key (the name hash then the NUL-padded name): the DIR_KEYLEN-byte
// prefix a leaf record is matched against.
fn dir_probe(name: &[u8]) -> Vec<u8> {
	let mut probe = vec![0u8; DIR_KEYLEN];
	probe[0..8].copy_from_slice(&name_hash(name).to_le_bytes());
	probe[8..8 + name.len()].copy_from_slice(name);
	probe
}

// A full directory leaf record: the (hash, NUL-padded name) key then the child inode.
fn dir_record(name: &[u8], child: u32) -> Vec<u8> {
	let mut rec = vec![0u8; DIR_REC];
	rec[0..8].copy_from_slice(&name_hash(name).to_le_bytes());
	rec[8..8 + name.len()].copy_from_slice(name);
	rec[8 + NAME_MAX..8 + NAME_MAX + 4].copy_from_slice(&child.to_le_bytes());
	rec
}

// B+tree node accessors. A node block begins with an 8-byte header: a type byte
// (NODE_LEAF or NODE_INTERNAL) then a u16 entry count at bytes 2..4; the entries follow.
fn node_type(buf: &[u8]) -> u8 {
	buf[0]
}

fn node_count(buf: &[u8]) -> usize {
	u16::from_le_bytes(buf[2..4].try_into().unwrap()) as usize
}

fn node_set_header(buf: &mut [u8], typ: u8, count: usize) {
	for b in buf[..NODE_HDR].iter_mut() {
		*b = 0;
	}
	buf[0] = typ;
	buf[2..4].copy_from_slice(&(count as u16).to_le_bytes());
}

// Internal-node separator key `i`: child `i` holds keys below it, child `i + 1` keys at
// or above it. Separators sit in a fixed region right after the header.
fn sep_key(buf: &[u8], i: usize) -> u64 {
	let off = NODE_HDR + i * SEP_SIZE;
	u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

fn set_sep(buf: &mut [u8], i: usize, key: u64) {
	let off = NODE_HDR + i * SEP_SIZE;
	buf[off..off + 8].copy_from_slice(&key.to_le_bytes());
}

// Internal-node child link `i`: its block pointer and that block's CRC32C. Child links
// sit in a fixed region after the separators, so offsets do not shift with the count.
fn child_ptr(buf: &[u8], i: usize) -> u64 {
	let off = INTERNAL_CHILD_BASE + i * CHILD_SIZE;
	u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

fn child_crc(buf: &[u8], i: usize) -> u32 {
	let off = INTERNAL_CHILD_BASE + i * CHILD_SIZE + 8;
	u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

fn set_child(buf: &mut [u8], i: usize, ptr: u64, crc: u32) {
	let off = INTERNAL_CHILD_BASE + i * CHILD_SIZE;
	buf[off..off + 8].copy_from_slice(&ptr.to_le_bytes());
	buf[off + 8..off + 12].copy_from_slice(&crc.to_le_bytes());
}

// Compare two leaf keys: the leading u64 numerically (so leaf order matches the numeric
// routing in internal nodes), then any remaining bytes lexicographically (the name, for
// a directory record, disambiguating a shared hash). Both slices are one key wide.
fn key_cmp(a: &[u8], b: &[u8]) -> Ordering {
	let ka = u64::from_le_bytes(a[0..8].try_into().unwrap());
	let kb = u64::from_le_bytes(b[0..8].try_into().unwrap());
	match ka.cmp(&kb) {
		Ordering::Equal => a[8..].cmp(&b[8..]),
		other => other,
	}
}

// Where to split an overfull leaf's records in two: the midpoint, nudged so two records
// sharing a u64 key never straddle the split (the parent routes by that key alone, so
// equal keys must stay in one leaf). Records are unique in the inode tree, so this is the
// plain midpoint there; in a directory it matters only for an astronomically rare 64-bit
// hash collision.
fn leaf_split_point(recs: &[Vec<u8>]) -> usize {
	let n = recs.len();
	let key_at = |i: usize| -> u64 { u64::from_le_bytes(recs[i][0..8].try_into().unwrap()) };
	let mut up = n / 2;
	while up < n && key_at(up) == key_at(up - 1) {
		up += 1;
	}
	if up < n {
		return up;
	}
	// no key boundary above the midpoint: look below it (only reached when most of the
	// leaf shares one 64-bit key).
	let mut down = n / 2;
	while down > 1 && key_at(down) == key_at(down - 1) {
		down -= 1;
	}
	down
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
	if extents[pos - 1].covers(lb) { Some(pos - 1) } else { None }
}

// Hash the 3-byte prefix at `w` into an LZ_HASH_BITS-wide match-finder bucket.
fn lz_hash(w: &[u8]) -> usize {
	let v = (w[0] as u32) << 16 | (w[1] as u32) << 8 | w[2] as u32;
	(v.wrapping_mul(0x9E37_79B1) >> (32 - LZ_HASH_BITS)) as usize
}

// Record position `i` in the hash chain (most-recent-first) so later positions can find
// it as a match candidate. `prev` is windowed (LZ_WINDOW entries): within the active
// window the low bits of a position are unique, and the match walk stops once a candidate
// is more than a window back, so the wrap never yields a wrong match.
fn lz_insert(src: &[u8], i: usize, head: &mut [i32], prev: &mut [i32]) {
	if i + LZ_MIN_MATCH <= src.len() {
		let h = lz_hash(&src[i..]);
		prev[i & (LZ_WINDOW - 1)] = head[h];
		head[h] = i as i32;
	}
}

// Compress `src` with the small LZSS coder described at the LZ_* constants: a control
// byte per eight items, each a literal byte or a (distance, length) back-reference into
// the 4096-byte sliding window. The stream begins with the uncompressed length, so
// `lz_decompress` needs no external size. Dependency-free and no_std; the ratio is modest
// but the format is simple and the decoder trivial, which is what an on-disk filesystem
// wants. Every candidate match is verified by comparing bytes, so the windowed hash chain
// only affects the ratio, never correctness.
fn lz_compress(src: &[u8]) -> Vec<u8> {
	let n = src.len();
	let mut out = Vec::with_capacity(n / 2 + 8);
	out.extend_from_slice(&(n as u32).to_le_bytes());
	let mut head = vec![-1i32; LZ_HASH_SIZE];
	let mut prev = vec![-1i32; LZ_WINDOW];
	let mut i = 0usize;
	while i < n {
		let ctrl = out.len();
		out.push(0u8);
		let mut flags = 0u8;
		let mut bit = 0;
		while bit < 8 && i < n {
			let (mut best_len, mut best_dist) = (0usize, 0usize);
			if i + LZ_MIN_MATCH <= n {
				let mut cand = head[lz_hash(&src[i..])];
				let mut probes = 0;
				while cand >= 0 && probes < LZ_MAX_CHAIN {
					let c = cand as usize;
					let dist = i - c;
					if dist > LZ_WINDOW {
						break;
					}
					let max = (n - i).min(LZ_MAX_MATCH);
					let mut l = 0;
					while l < max && src[c + l] == src[i + l] {
						l += 1;
					}
					if l > best_len {
						best_len = l;
						best_dist = dist;
						if l == LZ_MAX_MATCH {
							break;
						}
					}
					cand = prev[c & (LZ_WINDOW - 1)];
					probes += 1;
				}
			}
			if best_len >= LZ_MIN_MATCH {
				let dist_code = (best_dist - 1) as u16;
				out.push((dist_code & 0xFF) as u8);
				out.push((((dist_code >> 8) as u8) << 4) | (best_len - LZ_MIN_MATCH) as u8);
				let end = i + best_len;
				while i < end {
					lz_insert(src, i, &mut head, &mut prev);
					i += 1;
				}
			} else {
				flags |= 1 << bit;
				out.push(src[i]);
				lz_insert(src, i, &mut head, &mut prev);
				i += 1;
			}
			bit += 1;
		}
		out[ctrl] = flags;
	}
	out
}

// Decode a stream produced by `lz_compress` back into its original bytes. Bounds-checked
// throughout, so a malformed stream yields whatever decoded cleanly rather than panicking
// (a compressed extent's stored blocks are checksum-verified before this is called).
fn lz_decompress(src: &[u8]) -> Vec<u8> {
	if src.len() < 4 {
		return Vec::new();
	}
	let n = u32::from_le_bytes(src[0..4].try_into().unwrap()) as usize;
	let mut out = Vec::with_capacity(n);
	let mut p = 4;
	while out.len() < n && p < src.len() {
		let flags = src[p];
		p += 1;
		let mut bit = 0;
		while bit < 8 && out.len() < n {
			if flags & (1 << bit) != 0 {
				if p >= src.len() {
					return out;
				}
				out.push(src[p]);
				p += 1;
			} else {
				if p + 1 >= src.len() {
					return out;
				}
				let dist = (((src[p + 1] >> 4) as usize) << 8 | src[p] as usize) + 1;
				let len = (src[p + 1] & 0x0F) as usize + LZ_MIN_MATCH;
				p += 2;
				if dist > out.len() {
					return out;
				}
				let start = out.len() - dist;
				for k in 0..len {
					let byte = out[start + k];
					out.push(byte);
				}
			}
			bit += 1;
		}
	}
	out
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
