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

impl<D: BlockDevice> LiberFs<D> {
	// Format `dev` as a fresh, empty LiberFS spanning `num_blocks` blocks (an empty root
	// directory, no files), then return it mounted. Generation 0 lays out the two
	// superblock slots and a single inode-tree leaf holding the root directory inode;
	// everything else is the free pool. Inodes and directory nodes are allocated on
	// demand thereafter, so a fresh volume reserves no fixed inode region.
	pub fn format(mut dev: D, num_blocks: u64) -> Result<LiberFs<D>, FsError> {
		// generation-0 layout: [slot 0][slot 1][inode-tree root leaf], then the free
		// pool. The root directory inode starts empty (no entries, no B+tree yet).
		if num_blocks <= POOL_START + 1 {
			return Err(FsError::Invalid);
		}
		let leaf_block: u64 = POOL_START;

		// the inode tree's sole leaf: one record keyed by inode 0 (the root directory).
		let mut leaf = vec![0u8; BLOCK_SIZE];
		node_set_header(&mut leaf, NODE_LEAF, 1);
		leaf[NODE_HDR..NODE_HDR + 8].copy_from_slice(&(ROOT_INODE as u64).to_le_bytes());
		Inode::empty(KIND_DIR).write(&mut leaf[NODE_HDR + 8..NODE_HDR + 8 + INODE_SIZE]);
		if !dev.write_block(leaf_block, &leaf) {
			return Err(FsError::Io);
		}
		let leaf_crc = crc32c(&leaf);

		// generation 0 in slot 0; slot 1 left invalid (zeroed) until the first commit
		// ping-pongs onto it.
		let zero = vec![0u8; BLOCK_SIZE];
		let sb = Superblock { num_blocks, generation: 0, inode_root: leaf_block, inode_root_crc: leaf_crc, next_inode: ROOT_INODE + 1, root_inode: ROOT_INODE, snap_root: 0, snap_root_crc: 0 };
		if !dev.write_block(0, &serialize_superblock(&sb)) {
			return Err(FsError::Io);
		}
		if !dev.write_block(1, &zero) {
			return Err(FsError::Io);
		}

		let mut fs = LiberFs { dev, num_blocks, root_inode: ROOT_INODE, generation: 0, slot: 0, inode_root: leaf_block, inode_root_crc: leaf_crc, next_inode: ROOT_INODE + 1, prev_inode_root: 0, prev_inode_root_crc: 0, prev_valid: false, snap_root: 0, snap_root_crc: 0, snapshots: Vec::new(), free: vec![0u8; (num_blocks as usize).div_ceil(8)], fresh: BTreeSet::new(), txn: None, decomp: None, clock: 0 };
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

	// Mount a named snapshot read-only: the consistent, pinned state captured when the
	// snapshot was created. Returns None if the volume has no such snapshot. Like
	// `mount_snapshot`, the handle is meant for reading; the live free map (which already
	// reserves the snapshot's blocks) is reused unchanged.
	pub fn mount_named_snapshot(dev: D, name: &[u8]) -> Option<LiberFs<D>> {
		let mut fs = Self::mount(dev)?;
		let snap = fs.snapshots.iter().find(|s| s.name == name)?.clone();
		fs.inode_root = snap.inode_root;
		fs.inode_root_crc = snap.inode_root_crc;
		fs.generation = snap.generation;
		Some(fs)
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
		let (prev_inode_root, prev_inode_root_crc, prev_valid) = match prev_slot {
			Some(ps) => {
				let psb = slots[ps as usize]?;
				(psb.inode_root, psb.inode_root_crc, true)
			}
			None => (0, 0, false),
		};

		let mut fs = LiberFs { dev, num_blocks: sb.num_blocks, root_inode: sb.root_inode, generation: sb.generation, slot: cur_slot, inode_root: sb.inode_root, inode_root_crc: sb.inode_root_crc, next_inode: sb.next_inode, prev_inode_root, prev_inode_root_crc, prev_valid, snap_root: sb.snap_root, snap_root_crc: sb.snap_root_crc, snapshots: Vec::new(), free: vec![0u8; (sb.num_blocks as usize).div_ceil(8)], fresh: BTreeSet::new(), txn: None, decomp: None, clock: 0 };
		fs.load_snapshot_table().ok()?;
		fs.derive_free().ok()?;
		Some(fs)
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
		let existing = self.dir_lookup(parent, name)?;
		let old = match existing {
			Some(num) => {
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

		// transparently compress the freshly written runs: a run that shrinks is replaced
		// by a compressed record, an incompressible one stays raw.
		self.compress_inode(&mut inode)?;

		// point the inode at the new blocks, then name it (new files only). The old
		// inode and blocks are not freed here - the commit's previous generation keeps
		// them as the snapshot, and the next commit reclaims them.
		self.write_inode(inode_num, &mut inode)?;
		if old.is_none() {
			self.dir_insert(parent, name, inode_num)?;
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
		let inode_num = self.dir_lookup(parent, name)?.ok_or(FsError::NotFound)?;
		let inode = self.read_inode(inode_num)?;
		if inode.kind == KIND_DIR && inode.size != 0 {
			return Err(FsError::Invalid);
		}

		// clear the directory entry and free the inode in the new generation; its old
		// blocks remain referenced by the previous generation.
		self.dir_remove(parent, name)?;
		self.free_inode(inode_num)?;
		Ok(())
	}

	// snapshots

	// Create a named, read-only snapshot pinning the current generation's inode-tree
	// root, so its blocks survive later commits until the snapshot is deleted. The name
	// must be non-empty, at most SNAP_NAME_MAX bytes, and unique among existing
	// snapshots; a volume holds at most SNAP_MAX snapshots.
	pub fn create_snapshot(&mut self, name: &[u8]) -> Result<(), FsError> {
		if name.is_empty() {
			return Err(FsError::Invalid);
		}
		if name.len() > SNAP_NAME_MAX {
			return Err(FsError::TooLong);
		}
		if self.snapshots.iter().any(|s| s.name == name) {
			return Err(FsError::Invalid);
		}
		if self.snapshots.len() >= SNAP_MAX {
			return Err(FsError::NoSpace);
		}
		self.begin();
		let r = self.create_snapshot_inner(name);
		self.finish(r)
	}

	fn create_snapshot_inner(&mut self, name: &[u8]) -> Result<(), FsError> {
		// pin the current live generation: the snapshot-table write is the only change,
		// so the committed generation keeps this exact inode-tree root.
		self.snapshots.push(Snapshot { name: name.to_vec(), inode_root: self.inode_root, inode_root_crc: self.inode_root_crc, generation: self.generation });
		self.write_snapshot_table()
	}

	// List the named snapshots as (name, generation) pairs, oldest first.
	pub fn list_snapshots(&mut self) -> Result<Vec<(Vec<u8>, u64)>, FsError> {
		Ok(self.snapshots.iter().map(|s| (s.name.clone(), s.generation)).collect())
	}

	// Delete the named snapshot, releasing the blocks only it pinned (reclaimed by the
	// rederived free map). An unknown name is NotFound.
	pub fn delete_snapshot(&mut self, name: &[u8]) -> Result<(), FsError> {
		if !self.snapshots.iter().any(|s| s.name == name) {
			return Err(FsError::NotFound);
		}
		self.begin();
		let r = self.delete_snapshot_inner(name);
		self.finish(r)
	}

	fn delete_snapshot_inner(&mut self, name: &[u8]) -> Result<(), FsError> {
		self.snapshots.retain(|s| s.name != name);
		self.write_snapshot_table()
	}

	// Serialize the in-memory snapshot table to a fresh metadata block (copy-on-write),
	// updating snap_root and its CRC32C; an empty table clears the pointer. The fresh
	// block is published by the commit's superblock write.
	fn write_snapshot_table(&mut self) -> Result<(), FsError> {
		if self.snapshots.is_empty() {
			self.snap_root = 0;
			self.snap_root_crc = 0;
			return Ok(());
		}
		let mut block = vec![0u8; BLOCK_SIZE];
		block[0..4].copy_from_slice(&(self.snapshots.len() as u32).to_le_bytes());
		for (i, s) in self.snapshots.iter().enumerate() {
			let off = SNAP_HDR + i * SNAP_REC;
			block[off..off + s.name.len()].copy_from_slice(&s.name);
			block[off + SNAP_NAME_MAX..off + SNAP_NAME_MAX + 8].copy_from_slice(&s.inode_root.to_le_bytes());
			block[off + SNAP_NAME_MAX + 8..off + SNAP_NAME_MAX + 12].copy_from_slice(&s.inode_root_crc.to_le_bytes());
			block[off + SNAP_NAME_MAX + 12..off + SNAP_NAME_MAX + 20].copy_from_slice(&s.generation.to_le_bytes());
		}
		let ptr = self.snap_root;
		let dest = self.node_dest(ptr)?;
		let crc = self.write_node_to(dest, &block)?;
		self.snap_root = dest;
		self.snap_root_crc = crc;
		Ok(())
	}

	// Load the snapshot table the superblock points at into memory. The block is checked
	// against snap_root_crc; a corrupt or empty table yields no snapshots, so a damaged
	// table never pins (or walks) garbage.
	fn load_snapshot_table(&mut self) -> Result<(), FsError> {
		self.snapshots = Vec::new();
		if self.snap_root == 0 {
			return Ok(());
		}
		let mut block = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(self.snap_root, &mut block) {
			return Err(FsError::Io);
		}
		if crc32c(&block) != self.snap_root_crc {
			return Ok(());
		}
		let count = (u32::from_le_bytes(block[0..4].try_into().unwrap()) as usize).min(SNAP_MAX);
		for i in 0..count {
			let off = SNAP_HDR + i * SNAP_REC;
			let name = name_in(&block[off..off + SNAP_NAME_MAX]).to_vec();
			let inode_root = u64::from_le_bytes(block[off + SNAP_NAME_MAX..off + SNAP_NAME_MAX + 8].try_into().unwrap());
			let inode_root_crc = u32::from_le_bytes(block[off + SNAP_NAME_MAX + 8..off + SNAP_NAME_MAX + 12].try_into().unwrap());
			let generation = u64::from_le_bytes(block[off + SNAP_NAME_MAX + 12..off + SNAP_NAME_MAX + 20].try_into().unwrap());
			self.snapshots.push(Snapshot { name, inode_root, inode_root_crc, generation });
		}
		Ok(())
	}

	// Recover the device, consuming the filesystem.
	pub fn into_device(self) -> D {
		self.dev
	}

	// Borrow the backing block device without consuming the filesystem, so a caller can
	// open a second read-only view (a snapshot) over the same backing.
	pub fn device(&self) -> &D {
		&self.dev
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
		let inode_num = match self.dir_lookup(parent, name)? {
			Some(num) => {
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
				self.dir_insert(parent, name, num)?;
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
		let inode_f = self.dir_lookup(pf, nf)?.ok_or(FsError::NotFound)?;
		let from_inode = self.read_inode(inode_f)?;
		let (pt, nt) = self.resolve_parent(to, true)?;

		// a directory may not move into itself or one of its descendants.
		if from_inode.kind == KIND_DIR && self.subtree_contains(inode_f, pt)? {
			return Err(FsError::Invalid);
		}

		let dest = self.dir_lookup(pt, nt)?;
		if let Some(inode_t) = dest {
			if inode_t == inode_f {
				return Ok(());
			}
			let ti = self.read_inode(inode_t)?;
			if ti.kind == KIND_DIR && ti.size != 0 {
				return Err(FsError::Invalid);
			}
		}

		// point the destination name at the moved inode (add or overwrite), clear the
		// source entry, and free the inode the destination used to hold. Its old blocks
		// stay with the previous generation; the next commit reclaims them.
		self.dir_insert(pt, nt, inode_f)?;
		self.dir_remove(pf, nf)?;
		if let Some(inode_t) = dest {
			if inode_t != inode_f {
				self.free_inode(inode_t)?;
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
		let mut checksum_failures = self.check_inode_tree(self.inode_root, self.inode_root_crc)?;
		// every pinned snapshot generation is part of the live volume: verify its blocks
		// too, so corruption in a snapshot is reported and the walk accounts for it.
		for i in 0..self.snapshots.len() {
			let (root, crc) = (self.snapshots[i].inode_root, self.snapshots[i].inode_root_crc);
			checksum_failures += self.check_inode_tree(root, crc)?;
		}
		Ok(FsckReport { reclaimed_blocks: 0, reclaimed_inodes: 0, checksum_failures })
	}

	// Walk the inode B+tree, verifying every node against its stored checksum, and sum
	// the corrupt data blocks of every live file.
	fn check_inode_tree(&mut self, ptr: u64, crc: u32) -> Result<u32, FsError> {
		if ptr == 0 {
			return Ok(0);
		}
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node(ptr, crc, &mut buf)?;
		let count = node_count(&buf);
		let mut bad = 0;
		if node_type(&buf) == NODE_LEAF {
			for i in 0..count {
				let off = NODE_HDR + i * INODE_REC + 8;
				let mut inode = Inode::parse(&buf[off..off + INODE_SIZE]);
				if inode.kind == KIND_FILE {
					self.load_spill(&mut inode)?;
					bad += self.count_corrupt(&inode)?;
				}
			}
		} else {
			for i in 0..=count {
				bad += self.check_inode_tree(child_ptr(&buf, i), child_crc(&buf, i))?;
			}
		}
		Ok(bad)
	}

	// transactions

	// Begin a mutation: snapshot the inode-tree root, next-inode counter and snapshot
	// table so they can be restored on failure and the inode root reserved as the
	// previous generation on commit, and clear the fresh-block set.
	fn begin(&mut self) {
		self.txn = Some(Txn { inode_root: self.inode_root, inode_root_crc: self.inode_root_crc, next_inode: self.next_inode, snap_root: self.snap_root, snap_root_crc: self.snap_root_crc, snapshots: self.snapshots.clone() });
		self.fresh.clear();
		self.decomp = None;
	}

	// Commit the in-flight mutation: write a new superblock (incremented generation,
	// carrying the new inode-tree root, next-inode counter and snapshot table) to the
	// inactive slot - the single atomic write that publishes the whole transaction. The
	// superseded generation becomes the read-only snapshot; the one before it is
	// reclaimed by rederiving the free map.
	fn commit(&mut self) -> Result<(), FsError> {
		let sb = Superblock { num_blocks: self.num_blocks, generation: self.generation + 1, inode_root: self.inode_root, inode_root_crc: self.inode_root_crc, next_inode: self.next_inode, root_inode: self.root_inode, snap_root: self.snap_root, snap_root_crc: self.snap_root_crc };
		let new_slot = (self.slot + 1) % SUPER_SLOTS;
		// the commit point: a single superblock write swaps the live root atomically.
		if !self.dev.write_block(new_slot as u64, &serialize_superblock(&sb)) {
			return Err(FsError::Io);
		}

		// the generation this commit superseded becomes the snapshot; its blocks stay
		// reserved by the rederived free map.
		if let Some(t) = self.txn.take() {
			self.prev_inode_root = t.inode_root;
			self.prev_inode_root_crc = t.inode_root_crc;
			self.prev_valid = true;
		}
		self.generation += 1;
		self.slot = new_slot;
		self.derive_free()
	}

	// Roll back a failed mutation: restore the inode-tree root, next-inode counter and
	// snapshot table and rederive the free map, so the half-written fresh blocks are
	// forgotten and on-disk state is untouched.
	fn abort(&mut self) {
		if let Some(t) = self.txn.take() {
			self.inode_root = t.inode_root;
			self.inode_root_crc = t.inode_root_crc;
			self.next_inode = t.next_inode;
			self.snap_root = t.snap_root;
			self.snap_root_crc = t.snap_root_crc;
			self.snapshots = t.snapshots;
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
	// slots) plus every block the live and previous generations reference, the snapshot
	// table block, and every pinned snapshot generation. Called at mount and after each
	// commit; nothing else persists allocation state.
	fn derive_free(&mut self) -> Result<(), FsError> {
		let mut map = vec![0u8; self.free.len()];
		set_bit(&mut map, 0);
		set_bit(&mut map, 1);
		self.mark_inode_tree(self.inode_root, &mut map)?;
		if self.prev_valid {
			self.mark_inode_tree(self.prev_inode_root, &mut map)?;
		}
		// the snapshot table block and every pinned snapshot generation stay reserved, so
		// a later commit never reuses an earlier root's blocks.
		if self.snap_root != 0 {
			set_bit(&mut map, self.snap_root);
		}
		for i in 0..self.snapshots.len() {
			let root = self.snapshots[i].inode_root;
			self.mark_inode_tree(root, &mut map)?;
		}
		self.free = map;
		Ok(())
	}

	// Mark, in `map`, every block the inode B+tree rooted at `ptr` references: the tree
	// nodes themselves, and for each live inode either its file data / checksum /
	// overflow blocks or its directory's B+tree. Reads are raw (no checksum check), like
	// the old generation walk, so a corrupt block does not abort the mount or rebuild.
	fn mark_inode_tree(&mut self, ptr: u64, map: &mut [u8]) -> Result<(), FsError> {
		if ptr == 0 {
			return Ok(());
		}
		set_bit(map, ptr);
		let mut buf = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(ptr, &mut buf) {
			return Err(FsError::Io);
		}
		let count = node_count(&buf);
		if node_type(&buf) == NODE_LEAF {
			for i in 0..count {
				let off = NODE_HDR + i * INODE_REC + 8;
				let mut inode = Inode::parse(&buf[off..off + INODE_SIZE]);
				if inode.kind == KIND_FILE {
					// complete the extent map from the overflow chain before marking.
					self.load_spill(&mut inode)?;
					self.collect_inode_blocks(&inode, map)?;
				} else if inode.kind == KIND_DIR {
					self.mark_dir_tree(inode.dir_root, map)?;
				}
			}
		} else {
			for i in 0..=count {
				self.mark_inode_tree(child_ptr(&buf, i), map)?;
			}
		}
		Ok(())
	}

	// Mark every node block of a directory's B+tree. The entries themselves point at
	// inodes, which the inode-tree walk already covers, so only the nodes are marked.
	fn mark_dir_tree(&mut self, ptr: u64, map: &mut [u8]) -> Result<(), FsError> {
		if ptr == 0 {
			return Ok(());
		}
		set_bit(map, ptr);
		let mut buf = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(ptr, &mut buf) {
			return Err(FsError::Io);
		}
		if node_type(&buf) == NODE_INTERNAL {
			let count = node_count(&buf);
			for i in 0..=count {
				self.mark_dir_tree(child_ptr(&buf, i), map)?;
			}
		}
		Ok(())
	}

	// B+tree node and generic tree operations

	// Read a B+tree node block, verifying it against the CRC32C its parent link stored.
	// A mismatch is FsError::Corrupt, so on-disk damage to a tree node is caught on the
	// live path (lookup / insert / delete / enumeration / fsck).
	fn read_node(&mut self, ptr: u64, crc: u32, buf: &mut [u8]) -> Result<(), FsError> {
		if !self.dev.read_block(ptr, buf) {
			return Err(FsError::Io);
		}
		if crc32c(buf) != crc {
			return Err(FsError::Corrupt);
		}
		Ok(())
	}

	// The block to write an updated node to: reuse one this transaction already
	// allocated (overwrite in place), else copy up to a fresh metadata block so the
	// committed generation keeps the original.
	fn node_dest(&mut self, ptr: u64) -> Result<u64, FsError> {
		if ptr != 0 && self.fresh.contains(&ptr) { Ok(ptr) } else { self.alloc_meta() }
	}

	// Write `buf` to block `ptr` and return its CRC32C (to store in the parent link).
	fn write_node_to(&mut self, ptr: u64, buf: &[u8]) -> Result<u32, FsError> {
		if !self.dev.write_block(ptr, buf) {
			return Err(FsError::Io);
		}
		Ok(crc32c(buf))
	}

	// Look up `key` in the B+tree rooted at (`root`, `root_crc`), returning the matching
	// leaf record (whose leading `probe.len()` bytes equal `probe`) or None. `rec` is the
	// record width. Internal nodes route by the numeric u64 `key`; a leaf is searched by
	// the full probe so records sharing a u64 key are disambiguated by the bytes after it.
	fn tree_lookup(&mut self, root: u64, root_crc: u32, key: u64, probe: &[u8], rec: usize) -> Result<Option<Vec<u8>>, FsError> {
		if root == 0 {
			return Ok(None);
		}
		let mut ptr = root;
		let mut crc = root_crc;
		let mut buf = vec![0u8; BLOCK_SIZE];
		loop {
			self.read_node(ptr, crc, &mut buf)?;
			let count = node_count(&buf);
			if node_type(&buf) == NODE_LEAF {
				let (mut lo, mut hi) = (0usize, count);
				while lo < hi {
					let mid = (lo + hi) / 2;
					let off = NODE_HDR + mid * rec;
					match key_cmp(&buf[off..off + probe.len()], probe) {
						Ordering::Less => lo = mid + 1,
						Ordering::Greater => hi = mid,
						Ordering::Equal => return Ok(Some(buf[off..off + rec].to_vec())),
					}
				}
				return Ok(None);
			}
			// internal: route to the child whose range holds `key`.
			let mut ci = 0;
			while ci < count && sep_key(&buf, ci) <= key {
				ci += 1;
			}
			ptr = child_ptr(&buf, ci);
			crc = child_crc(&buf, ci);
		}
	}

	// Insert or overwrite `record` (numeric key `key`, full key width `keylen`) in the
	// B+tree rooted at (`root`, `root_crc`); `rec` is the record width and `leaf_max` the
	// leaf capacity. Returns the new root (ptr, crc). Copy-on-write: every node on the
	// path is rewritten to a fresh block (or in place if already fresh this transaction).
	fn tree_insert(&mut self, root: u64, root_crc: u32, key: u64, record: &[u8], rec: usize, leaf_max: usize, keylen: usize) -> Result<(u64, u32), FsError> {
		if root == 0 {
			// empty tree: a new leaf with the single record.
			let blk = self.alloc_meta()?;
			let mut buf = vec![0u8; BLOCK_SIZE];
			node_set_header(&mut buf, NODE_LEAF, 1);
			buf[NODE_HDR..NODE_HDR + rec].copy_from_slice(record);
			let crc = self.write_node_to(blk, &buf)?;
			return Ok((blk, crc));
		}
		match self.tree_insert_node(root, root_crc, key, record, rec, leaf_max, keylen)? {
			Ins::Updated(p, c) => Ok((p, c)),
			Ins::Split(lp, lc, sep, rp, rc) => {
				// the root split: build a new internal root over the two halves.
				let blk = self.alloc_meta()?;
				let mut buf = vec![0u8; BLOCK_SIZE];
				node_set_header(&mut buf, NODE_INTERNAL, 1);
				set_sep(&mut buf, 0, sep);
				set_child(&mut buf, 0, lp, lc);
				set_child(&mut buf, 1, rp, rc);
				let crc = self.write_node_to(blk, &buf)?;
				Ok((blk, crc))
			}
		}
	}

	fn tree_insert_node(&mut self, ptr: u64, crc: u32, key: u64, record: &[u8], rec: usize, leaf_max: usize, keylen: usize) -> Result<Ins, FsError> {
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node(ptr, crc, &mut buf)?;
		let count = node_count(&buf);
		if node_type(&buf) == NODE_LEAF {
			// find the insert position, or an exact match by the full key.
			let (mut lo, mut hi) = (0usize, count);
			let mut exact = false;
			while lo < hi {
				let mid = (lo + hi) / 2;
				let off = NODE_HDR + mid * rec;
				match key_cmp(&buf[off..off + keylen], &record[..keylen]) {
					Ordering::Less => lo = mid + 1,
					Ordering::Greater => hi = mid,
					Ordering::Equal => {
						exact = true;
						lo = mid;
						break;
					}
				}
			}
			let pos = lo;
			if exact {
				// overwrite in place (after copying the node up).
				let dest = self.node_dest(ptr)?;
				let off = NODE_HDR + pos * rec;
				buf[off..off + rec].copy_from_slice(record);
				let ncrc = self.write_node_to(dest, &buf)?;
				return Ok(Ins::Updated(dest, ncrc));
			}
			if count < leaf_max {
				// insert, shifting the tail right by one record.
				let dest = self.node_dest(ptr)?;
				let start = NODE_HDR + pos * rec;
				let end = NODE_HDR + count * rec;
				buf.copy_within(start..end, start + rec);
				buf[start..start + rec].copy_from_slice(record);
				node_set_header(&mut buf, NODE_LEAF, count + 1);
				let ncrc = self.write_node_to(dest, &buf)?;
				return Ok(Ins::Updated(dest, ncrc));
			}
			// full: gather every record with the new one inserted, then split in two.
			let mut recs: Vec<Vec<u8>> = Vec::with_capacity(count + 1);
			for i in 0..count {
				let off = NODE_HDR + i * rec;
				recs.push(buf[off..off + rec].to_vec());
			}
			recs.insert(pos, record.to_vec());
			let split = leaf_split_point(&recs);
			let left_dest = self.node_dest(ptr)?;
			let right_dest = self.alloc_meta()?;
			let mut lbuf = vec![0u8; BLOCK_SIZE];
			node_set_header(&mut lbuf, NODE_LEAF, split);
			for (i, r) in recs[..split].iter().enumerate() {
				let off = NODE_HDR + i * rec;
				lbuf[off..off + rec].copy_from_slice(r);
			}
			let mut rbuf = vec![0u8; BLOCK_SIZE];
			node_set_header(&mut rbuf, NODE_LEAF, recs.len() - split);
			for (i, r) in recs[split..].iter().enumerate() {
				let off = NODE_HDR + i * rec;
				rbuf[off..off + rec].copy_from_slice(r);
			}
			let lcrc = self.write_node_to(left_dest, &lbuf)?;
			let rcrc = self.write_node_to(right_dest, &rbuf)?;
			let sep = u64::from_le_bytes(recs[split][0..8].try_into().unwrap());
			return Ok(Ins::Split(left_dest, lcrc, sep, right_dest, rcrc));
		}
		// internal: route to a child and recurse.
		let mut ci = 0;
		while ci < count && sep_key(&buf, ci) <= key {
			ci += 1;
		}
		let cp = child_ptr(&buf, ci);
		let cc = child_crc(&buf, ci);
		match self.tree_insert_node(cp, cc, key, record, rec, leaf_max, keylen)? {
			Ins::Updated(np, nc) => {
				let dest = self.node_dest(ptr)?;
				set_child(&mut buf, ci, np, nc);
				let ncrc = self.write_node_to(dest, &buf)?;
				Ok(Ins::Updated(dest, ncrc))
			}
			Ins::Split(lp, lc, sep, rp, rc) => {
				if count + 2 <= INTERNAL_MAX {
					// room: replace child ci with the left half and insert the separator
					// and the right half after it.
					let dest = self.node_dest(ptr)?;
					let sstart = NODE_HDR + ci * SEP_SIZE;
					let send = NODE_HDR + count * SEP_SIZE;
					buf.copy_within(sstart..send, sstart + SEP_SIZE);
					set_sep(&mut buf, ci, sep);
					let cstart = INTERNAL_CHILD_BASE + (ci + 1) * CHILD_SIZE;
					let cend = INTERNAL_CHILD_BASE + (count + 1) * CHILD_SIZE;
					buf.copy_within(cstart..cend, cstart + CHILD_SIZE);
					set_child(&mut buf, ci, lp, lc);
					set_child(&mut buf, ci + 1, rp, rc);
					node_set_header(&mut buf, NODE_INTERNAL, count + 1);
					let ncrc = self.write_node_to(dest, &buf)?;
					Ok(Ins::Updated(dest, ncrc))
				} else {
					// full: build the combined separator and child arrays, split them,
					// and lift the middle separator to the parent.
					let mut seps: Vec<u64> = (0..count).map(|i| sep_key(&buf, i)).collect();
					let mut kids: Vec<(u64, u32)> = (0..=count).map(|i| (child_ptr(&buf, i), child_crc(&buf, i))).collect();
					seps.insert(ci, sep);
					kids[ci] = (lp, lc);
					kids.insert(ci + 1, (rp, rc));
					let s = seps.len();
					let mid = s / 2;
					let up = seps[mid];
					let left_dest = self.node_dest(ptr)?;
					let right_dest = self.alloc_meta()?;
					let mut lbuf = vec![0u8; BLOCK_SIZE];
					node_set_header(&mut lbuf, NODE_INTERNAL, mid);
					for i in 0..mid {
						set_sep(&mut lbuf, i, seps[i]);
					}
					for i in 0..=mid {
						set_child(&mut lbuf, i, kids[i].0, kids[i].1);
					}
					let rcount = s - mid - 1;
					let mut rbuf = vec![0u8; BLOCK_SIZE];
					node_set_header(&mut rbuf, NODE_INTERNAL, rcount);
					for i in 0..rcount {
						set_sep(&mut rbuf, i, seps[mid + 1 + i]);
					}
					for i in 0..=rcount {
						set_child(&mut rbuf, i, kids[mid + 1 + i].0, kids[mid + 1 + i].1);
					}
					let lcrc = self.write_node_to(left_dest, &lbuf)?;
					let rcrc = self.write_node_to(right_dest, &rbuf)?;
					Ok(Ins::Split(left_dest, lcrc, up, right_dest, rcrc))
				}
			}
		}
	}

	// Delete `key` from the B+tree rooted at (`root`, `root_crc`). Returns the new root
	// (ptr, crc) and whether a record was removed. Empty leaves and single-child roots
	// are collapsed; there is no rebalancing or merging of half-full nodes, which keeps
	// deletion O(log n) and is sound for a copy-on-write tree (a thin node only wastes a
	// little space, never breaks lookup).
	fn tree_delete(&mut self, root: u64, root_crc: u32, key: u64, probe: &[u8], rec: usize, keylen: usize) -> Result<(u64, u32, bool), FsError> {
		if root == 0 {
			return Ok((0, 0, false));
		}
		match self.tree_delete_node(root, root_crc, key, probe, rec, keylen)? {
			Del::NotFound => Ok((root, root_crc, false)),
			Del::Empty => Ok((0, 0, true)),
			Del::Updated(p, c) => {
				// collapse a root that became a single-child internal node, repeatedly.
				let mut ptr = p;
				let mut crc = c;
				let mut buf = vec![0u8; BLOCK_SIZE];
				loop {
					self.read_node(ptr, crc, &mut buf)?;
					if node_type(&buf) == NODE_INTERNAL && node_count(&buf) == 0 {
						let cp = child_ptr(&buf, 0);
						let cc = child_crc(&buf, 0);
						ptr = cp;
						crc = cc;
					} else {
						break;
					}
				}
				Ok((ptr, crc, true))
			}
		}
	}

	fn tree_delete_node(&mut self, ptr: u64, crc: u32, key: u64, probe: &[u8], rec: usize, keylen: usize) -> Result<Del, FsError> {
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node(ptr, crc, &mut buf)?;
		let count = node_count(&buf);
		if node_type(&buf) == NODE_LEAF {
			let (mut lo, mut hi) = (0usize, count);
			let mut found = None;
			while lo < hi {
				let mid = (lo + hi) / 2;
				let off = NODE_HDR + mid * rec;
				match key_cmp(&buf[off..off + keylen], probe) {
					Ordering::Less => lo = mid + 1,
					Ordering::Greater => hi = mid,
					Ordering::Equal => {
						found = Some(mid);
						break;
					}
				}
			}
			let pos = match found {
				Some(p) => p,
				None => return Ok(Del::NotFound),
			};
			if count == 1 {
				return Ok(Del::Empty);
			}
			let dest = self.node_dest(ptr)?;
			let start = NODE_HDR + pos * rec;
			let end = NODE_HDR + count * rec;
			buf.copy_within(start + rec..end, start);
			node_set_header(&mut buf, NODE_LEAF, count - 1);
			let ncrc = self.write_node_to(dest, &buf)?;
			return Ok(Del::Updated(dest, ncrc));
		}
		// internal: route and recurse.
		let mut ci = 0;
		while ci < count && sep_key(&buf, ci) <= key {
			ci += 1;
		}
		let cp = child_ptr(&buf, ci);
		let cc = child_crc(&buf, ci);
		match self.tree_delete_node(cp, cc, key, probe, rec, keylen)? {
			Del::NotFound => Ok(Del::NotFound),
			Del::Updated(np, nc) => {
				let dest = self.node_dest(ptr)?;
				set_child(&mut buf, ci, np, nc);
				let ncrc = self.write_node_to(dest, &buf)?;
				Ok(Del::Updated(dest, ncrc))
			}
			Del::Empty => {
				if count == 0 {
					// a single-child internal whose only child emptied empties too.
					return Ok(Del::Empty);
				}
				// drop child ci and an adjacent separator (the one to its left when ci is
				// the last child, else the one to its right).
				let dest = self.node_dest(ptr)?;
				let sidx = if ci == count { ci - 1 } else { ci };
				let sstart = NODE_HDR + sidx * SEP_SIZE;
				let send = NODE_HDR + count * SEP_SIZE;
				buf.copy_within(sstart + SEP_SIZE..send, sstart);
				let cstart = INTERNAL_CHILD_BASE + ci * CHILD_SIZE;
				let cend = INTERNAL_CHILD_BASE + (count + 1) * CHILD_SIZE;
				buf.copy_within(cstart + CHILD_SIZE..cend, cstart);
				node_set_header(&mut buf, NODE_INTERNAL, count - 1);
				let ncrc = self.write_node_to(dest, &buf)?;
				Ok(Del::Updated(dest, ncrc))
			}
		}
	}

	// inode I/O

	// Read inode `num` from the inode B+tree. Missing (never allocated or freed) is
	// FsError::Invalid; a tree node failing its checksum is FsError::Corrupt.
	fn read_inode(&mut self, num: u32) -> Result<Inode, FsError> {
		let key = num as u64;
		let probe = key.to_le_bytes();
		match self.tree_lookup(self.inode_root, self.inode_root_crc, key, &probe, INODE_REC)? {
			Some(rec) => {
				let mut inode = Inode::parse(&rec[8..8 + INODE_SIZE]);
				if inode.kind == KIND_FILE {
					// complete the extent map from the overflow chain (a no-op for a
					// file whose runs all fit inline).
					self.load_spill(&mut inode)?;
				}
				Ok(inode)
			}
			None => Err(FsError::Invalid),
		}
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

	// Write inode `num` into the inode B+tree, rebuilding its extent overflow chain
	// first (for a file) so the inode slot and chain agree. The insert copies every tree
	// node on the path up to a fresh block and updates `inode_root`; the change is
	// published by `commit`.
	fn write_inode(&mut self, num: u32, inode: &mut Inode) -> Result<(), FsError> {
		if inode.kind == KIND_FILE {
			self.flush_extents(inode)?;
		}
		let mut rec = vec![0u8; INODE_REC];
		rec[0..8].copy_from_slice(&(num as u64).to_le_bytes());
		inode.write(&mut rec[8..8 + INODE_SIZE]);
		let (root, crc) = self.tree_insert(self.inode_root, self.inode_root_crc, num as u64, &rec, INODE_REC, INODE_LEAF_MAX, INODE_KEYLEN)?;
		self.inode_root = root;
		self.inode_root_crc = crc;
		Ok(())
	}

	// Hand out a fresh inode number from the monotonic counter (never reused). The
	// caller writes the inode right after, so nothing is inserted into the tree here.
	fn alloc_inode(&mut self) -> Result<u32, FsError> {
		let num = self.next_inode;
		if num == u32::MAX {
			return Err(FsError::NoSpace);
		}
		self.next_inode += 1;
		Ok(num)
	}

	// Remove inode `num` from the inode B+tree (its data blocks are reclaimed when the
	// free map is rederived at commit, the previous generation pinning them until then).
	fn free_inode(&mut self, num: u32) -> Result<(), FsError> {
		let probe = (num as u64).to_le_bytes();
		let (root, crc, _) = self.tree_delete(self.inode_root, self.inode_root_crc, num as u64, &probe, INODE_REC, INODE_KEYLEN)?;
		self.inode_root = root;
		self.inode_root_crc = crc;
		Ok(())
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
		if ext.clen != 0 {
			// a compressed run: serve the block from the whole extent's decompressed
			// image, decoding once and caching it for the rest of a sequential read.
			let cached = matches!(&self.decomp, Some((key, _)) if *key == ext.physical);
			if !cached {
				let decoded = self.decompress_extent(&ext)?;
				self.decomp = Some((ext.physical, decoded));
			}
			let data = &self.decomp.as_ref().unwrap().1;
			let start = (lb - ext.logical) as usize * BLOCK_SIZE;
			for b in buf.iter_mut() {
				*b = 0;
			}
			if start < data.len() {
				let end = (start + BLOCK_SIZE).min(data.len());
				buf[..end - start].copy_from_slice(&data[start..end]);
			}
			return Ok(true);
		}
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
		// a compressed run cannot be edited in place: thaw it back to raw blocks first, so
		// this overwrite (and any later block of the run) proceeds on a raw extent.
		if let Some(i) = find_extent(&inode.extents, lb) {
			if inode.extents[i].clen != 0 {
				self.thaw_extent(inode, i)?;
			}
		}
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
			if prev.clen == 0 && prev.end() == lb && prev.physical + prev.length as u64 == phys && (prev.length as usize) < CRCS_PER_BLOCK {
				let csum = self.cow_meta(prev.csum)?;
				let csum_crc = self.set_csum_slot(csum, prev.length as usize, crc)?;
				let e = &mut inode.extents[pos - 1];
				e.length += 1;
				e.store_len += 1;
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
		inode.extents.insert(pos, Extent { logical: lb, physical: phys, length: 1, csum, csum_crc: crc32c(&cbuf), store_len: 1, clen: 0 });
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
			pieces.push(Extent { logical: ext.logical, physical: ext.physical, length: off as u32, csum: ext.csum, csum_crc: ext.csum_crc, store_len: off as u32, clen: 0 });
		}
		// the rewritten block gets a fresh single-entry checksum block.
		let mid_csum = self.alloc_meta()?;
		let mut cbuf = vec![0u8; BLOCK_SIZE];
		cbuf[0..4].copy_from_slice(&crc.to_le_bytes());
		if !self.dev.write_block(mid_csum, &cbuf) {
			return Err(FsError::Io);
		}
		pieces.push(Extent { logical: ext.logical + off as u64, physical: new_phys, length: 1, csum: mid_csum, csum_crc: crc32c(&cbuf), store_len: 1, clen: 0 });
		if off + 1 < ext.length as usize {
			let slen = ext.length as usize - off - 1;
			let suf_csum = self.alloc_meta()?;
			let mut sbuf = vec![0u8; BLOCK_SIZE];
			// copy the original CRCs of the suffix down to the start of the new block.
			sbuf[0..slen * 4].copy_from_slice(&old_csum[(off + 1) * 4..(off + 1 + slen) * 4]);
			if !self.dev.write_block(suf_csum, &sbuf) {
				return Err(FsError::Io);
			}
			pieces.push(Extent { logical: ext.logical + off as u64 + 1, physical: ext.physical + off as u64 + 1, length: slen as u32, csum: suf_csum, csum_crc: crc32c(&sbuf), store_len: slen as u32, clen: 0 });
		}
		inode.extents.splice(i..i + 1, pieces);
		Ok(())
	}

	// Decompress a compressed extent's stored blocks and rewrite its span as a raw 1:1
	// run (each logical block its own fresh data block with a per-block checksum),
	// dropping the compressed record. Editing a compressed file falls back to raw; a
	// later whole-file write recompresses it. The old stored and checksum blocks become
	// unreferenced and are reclaimed when the free map is rederived at commit.
	fn thaw_extent(&mut self, inode: &mut Inode, i: usize) -> Result<(), FsError> {
		let ext = inode.extents[i];
		let decoded = self.decompress_extent(&ext)?;
		inode.extents.remove(i);
		let mut blk = vec![0u8; BLOCK_SIZE];
		for lo in 0..ext.length as usize {
			for b in blk.iter_mut() {
				*b = 0;
			}
			let start = lo * BLOCK_SIZE;
			if start < decoded.len() {
				let end = (start + BLOCK_SIZE).min(decoded.len());
				blk[..end - start].copy_from_slice(&decoded[start..end]);
			}
			let crc = crc32c(&blk);
			let phys = self.alloc_data()?;
			if !self.dev.write_block(phys, &blk) {
				return Err(FsError::Io);
			}
			self.place_block(inode, ext.logical + lo as u64, phys, crc)?;
		}
		Ok(())
	}

	// Read and verify the stored (compressed) blocks of a compressed extent, then decode
	// them into the run's uncompressed image. Each stored block is checked against its
	// CRC32C in the checksum block, so corruption of the compressed bytes surfaces as
	// `FsError::Corrupt` rather than bad data.
	fn decompress_extent(&mut self, ext: &Extent) -> Result<Vec<u8>, FsError> {
		let mut cbuf = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(ext.csum, &mut cbuf) {
			return Err(FsError::Io);
		}
		if crc32c(&cbuf) != ext.csum_crc {
			return Err(FsError::Corrupt);
		}
		let mut comp = vec![0u8; ext.store_len as usize * BLOCK_SIZE];
		for s in 0..ext.store_len as usize {
			let dst = &mut comp[s * BLOCK_SIZE..(s + 1) * BLOCK_SIZE];
			if !self.dev.read_block(ext.physical + s as u64, dst) {
				return Err(FsError::Io);
			}
			let stored = u32::from_le_bytes(cbuf[s * 4..s * 4 + 4].try_into().unwrap());
			if crc32c(dst) != stored {
				return Err(FsError::Corrupt);
			}
		}
		Ok(lz_decompress(&comp[..ext.clen as usize]))
	}

	// Try to transparently compress each of a freshly written file's raw extents in
	// place: a run that shrinks to fewer blocks becomes a compressed record, an
	// incompressible one is left raw. Run as the last step of a whole-file write, so the
	// block-by-block writer stays simple and partial updates keep working on raw runs.
	fn compress_inode(&mut self, inode: &mut Inode) -> Result<(), FsError> {
		for i in 0..inode.extents.len() {
			self.compress_extent(inode, i)?;
		}
		Ok(())
	}

	// Compress raw extent `i` if its bytes shrink to fewer blocks. The compressed stream
	// is written across a contiguous run of fresh data blocks with one checksum block
	// (one CRC32C per stored block), and the extent rewritten to point at it; the old raw
	// blocks become unreferenced and are reclaimed at commit. The run stays raw if it is
	// a single block, does not shrink, or a contiguous stored run is unavailable.
	fn compress_extent(&mut self, inode: &mut Inode, i: usize) -> Result<(), FsError> {
		let ext = inode.extents[i];
		if ext.clen != 0 || ext.length < 2 {
			return Ok(());
		}
		let mut ubuf = vec![0u8; ext.length as usize * BLOCK_SIZE];
		for off in 0..ext.length as usize {
			let dst = &mut ubuf[off * BLOCK_SIZE..(off + 1) * BLOCK_SIZE];
			if !self.dev.read_block(ext.physical + off as u64, dst) {
				return Err(FsError::Io);
			}
		}
		let comp = lz_compress(&ubuf);
		let store_len = comp.len().div_ceil(BLOCK_SIZE);
		if store_len >= ext.length as usize {
			return Ok(());
		}
		// claim a contiguous run of stored blocks (data is taken low-to-high, so fresh
		// data allocations run contiguously); leave the run raw if a gap appears.
		let first = self.alloc_data()?;
		let mut last = first;
		for _ in 1..store_len {
			let b = self.alloc_data()?;
			if b != last + 1 {
				return Ok(());
			}
			last = b;
		}
		let mut blk = vec![0u8; BLOCK_SIZE];
		let mut cbuf = vec![0u8; BLOCK_SIZE];
		for s in 0..store_len {
			for b in blk.iter_mut() {
				*b = 0;
			}
			let start = s * BLOCK_SIZE;
			let end = (start + BLOCK_SIZE).min(comp.len());
			blk[..end - start].copy_from_slice(&comp[start..end]);
			if !self.dev.write_block(first + s as u64, &blk) {
				return Err(FsError::Io);
			}
			let crc = crc32c(&blk);
			cbuf[s * 4..s * 4 + 4].copy_from_slice(&crc.to_le_bytes());
		}
		let csum = self.alloc_meta()?;
		if !self.dev.write_block(csum, &cbuf) {
			return Err(FsError::Io);
		}
		inode.extents[i] = Extent { logical: ext.logical, physical: first, length: ext.length, csum, csum_crc: crc32c(&cbuf), store_len: store_len as u32, clen: comp.len() as u32 };
		Ok(())
	}

	// Count the live data blocks of `inode` whose on-disk bytes no longer match the
	// CRC32C stored for them in their run's checksum block. A run whose checksum block
	// is itself corrupt counts as wholly bad. A compressed run is checked over its stored
	// (compressed) blocks, since those are the bytes the checksum covers.
	fn count_corrupt(&mut self, inode: &Inode) -> Result<u32, FsError> {
		let mut bad = 0;
		let mut buf = vec![0u8; BLOCK_SIZE];
		let mut cbuf = vec![0u8; BLOCK_SIZE];
		for ext in inode.extents.iter() {
			if !self.dev.read_block(ext.csum, &mut cbuf) {
				return Err(FsError::Io);
			}
			if crc32c(&cbuf) != ext.csum_crc {
				bad += ext.store_len;
				continue;
			}
			for off in 0..ext.store_len as usize {
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
			inode_num = self.dir_lookup(inode_num, seg)?.ok_or(FsError::NotFound)?;
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
				let child = self.dir_lookup(parent, seg)?.ok_or(FsError::NotFound)?;
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
		if let Some(child) = self.dir_lookup(parent, name)? {
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
		self.dir_insert(parent, name, num)?;
		Ok(num)
	}

	// directory operations (on any directory inode)

	// Look up `name` in directory `dir_num` through its B+tree: the child inode, or None
	// if absent. Errors if `dir_num` is not a directory.
	fn dir_lookup(&mut self, dir_num: u32, name: &[u8]) -> Result<Option<u32>, FsError> {
		let dir = self.read_inode(dir_num)?;
		if dir.kind != KIND_DIR {
			return Err(FsError::NotFound);
		}
		let probe = dir_probe(name);
		match self.tree_lookup(dir.dir_root, dir.dir_root_crc, name_hash(name), &probe, DIR_REC)? {
			Some(rec) => Ok(Some(u32::from_le_bytes(rec[8 + NAME_MAX..8 + NAME_MAX + 4].try_into().unwrap()))),
			None => Ok(None),
		}
	}

	// Insert entry `name` -> `child` into directory `dir_num`, or repoint it if it is
	// already there. The directory's B+tree root (and the entry count it stores in
	// `size`) are updated and the directory inode rewritten.
	fn dir_insert(&mut self, dir_num: u32, name: &[u8], child: u32) -> Result<(), FsError> {
		let mut dir = self.read_inode(dir_num)?;
		if dir.kind != KIND_DIR {
			return Err(FsError::NotFound);
		}
		let key = name_hash(name);
		let existed = {
			let probe = dir_probe(name);
			self.tree_lookup(dir.dir_root, dir.dir_root_crc, key, &probe, DIR_REC)?.is_some()
		};
		let record = dir_record(name, child);
		let (root, crc) = self.tree_insert(dir.dir_root, dir.dir_root_crc, key, &record, DIR_REC, DIR_LEAF_MAX, DIR_KEYLEN)?;
		dir.dir_root = root;
		dir.dir_root_crc = crc;
		if !existed {
			dir.size += 1;
		}
		dir.mtime = self.clock;
		self.write_inode(dir_num, &mut dir)?;
		Ok(())
	}

	// Remove entry `name` from directory `dir_num`. NotFound if it is not there.
	fn dir_remove(&mut self, dir_num: u32, name: &[u8]) -> Result<(), FsError> {
		let mut dir = self.read_inode(dir_num)?;
		if dir.kind != KIND_DIR {
			return Err(FsError::NotFound);
		}
		let probe = dir_probe(name);
		let (root, crc, removed) = self.tree_delete(dir.dir_root, dir.dir_root_crc, name_hash(name), &probe, DIR_REC, DIR_KEYLEN)?;
		if !removed {
			return Err(FsError::NotFound);
		}
		dir.dir_root = root;
		dir.dir_root_crc = crc;
		dir.size = dir.size.saturating_sub(1);
		dir.mtime = self.clock;
		self.write_inode(dir_num, &mut dir)?;
		Ok(())
	}

	// Collect every (name, inode) entry in directory `dir_num`, in key order.
	fn dir_entries_of(&mut self, dir_num: u32) -> Result<Vec<(Vec<u8>, u32)>, FsError> {
		let dir = self.read_inode(dir_num)?;
		let mut out = Vec::new();
		self.collect_dir_entries(dir.dir_root, dir.dir_root_crc, &mut out)?;
		Ok(out)
	}

	// Walk the directory B+tree rooted at (`ptr`, `crc`), appending each leaf's entries.
	fn collect_dir_entries(&mut self, ptr: u64, crc: u32, out: &mut Vec<(Vec<u8>, u32)>) -> Result<(), FsError> {
		if ptr == 0 {
			return Ok(());
		}
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node(ptr, crc, &mut buf)?;
		let count = node_count(&buf);
		if node_type(&buf) == NODE_LEAF {
			for i in 0..count {
				let off = NODE_HDR + i * DIR_REC;
				let name = name_in(&buf[off + 8..off + 8 + NAME_MAX]).to_vec();
				let inode = u32::from_le_bytes(buf[off + 8 + NAME_MAX..off + 8 + NAME_MAX + 4].try_into().unwrap());
				out.push((name, inode));
			}
		} else {
			for i in 0..=count {
				let cp = child_ptr(&buf, i);
				let cc = child_crc(&buf, i);
				self.collect_dir_entries(cp, cc, out)?;
			}
		}
		Ok(())
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

	// Set the bitmap bit for every block an inode references: each run's stored (data or
	// compressed) blocks and its checksum block, plus the blocks of the extent overflow
	// chain.
	fn collect_inode_blocks(&mut self, inode: &Inode, bitmap: &mut [u8]) -> Result<(), FsError> {
		for ext in inode.extents.iter() {
			for off in 0..ext.store_len as u64 {
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
