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

use alloc::collections::BTreeMap;
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
// and a CRC32C paired with every block pointer. The version stays 1 pre-release; the
// FEATURES flags word records layout revisions instead, so a volume laid down by an
// older build is detected (its flags differ) rather than mis-parsed.
const MAGIC: [u8; 8] = *b"LIBERFS1";
const VERSION: u32 = 1;
// Feature flags the superblock must carry, bit for bit: bit 0 is the second-revision
// layout (variable-length directory records, the chained snapshot table, the identity
// and algorithm fields, per-volume compression). Unknown or missing bits reject the
// mount.
const FEATURES: u64 = 0x1;
// Algorithm identifiers recorded in the superblock, so a mount never verifies with the
// wrong checksum or decodes with the wrong codec.
const CSUM_ALGO_CRC32C: u8 = 1;
const CODEC_LZ4: u8 = 2;
// The volume label's fixed on-disk field width (NUL padded).
const LABEL_MAX: usize = 32;

// The two superblock slots (blocks 0 and 1): a commit writes the new superblock to the
// inactive slot, so the active one survives a torn write. The block pool begins right
// after them.
const SUPER_SLOTS: u32 = 2;
const POOL_START: u64 = SUPER_SLOTS as u64;

// One inode is a fixed 256-byte slot: a kind byte, a size, two timestamps, then either
// (for a file) the extent map's overflow pointer and count and EXTENTS_INLINE inline
// extents, or (for a directory) its B+tree root pointer and that root's CRC32C. An
// opaque owner tag sits at OWNER_TAG_OFF. Each slot is stored, keyed by inode number,
// in a leaf of the inode B+tree. The field offsets within the slot, by name, so the
// parser and writer cannot drift apart:
const INODE_SIZE: usize = 256;
const INO_KIND_OFF: usize = 0;
const INO_SIZE_OFF: usize = 8;
const INO_CTIME_OFF: usize = 16;
const INO_MTIME_OFF: usize = 24;
// the overlay: a file's spill pointer / a directory's tree root, then its CRC32C,
// then (files only) the total extent count.
const INO_MAP_OFF: usize = 32;
const INO_MAP_CRC_OFF: usize = 40;
const INO_EXTENT_COUNT_OFF: usize = 44;

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

// Transparent per-extent compression uses a dependency-free LZ4 block-format coder (no
// external crate, no_std). LZ4 frames data as sequences: a token byte (literal count
// high nibble, match length low nibble, each extended by 255-bytes), the literals, a
// 2-byte little-endian match offset (1..=65535), and the match length (minimum 4).
// The stream begins with the uncompressed length (u32, little-endian) so it decodes
// without external size metadata. A compressed extent stores this stream across whole
// blocks, each with its own CRC32C, so the integrity checks cover the stored
// (compressed) bytes. The superblock records the codec ID, so a mount never decodes
// with the wrong coder.
const LZ_MIN_MATCH: usize = 4;
const LZ_HASH_BITS: usize = 14;
const LZ_HASH_SIZE: usize = 1 << LZ_HASH_BITS;
// The last five bytes of an LZ4 stream are always literals and a match may not start
// within twelve bytes of the end (the spec's parsing-restriction margin, kept for
// interoperability and simple bounds).
const LZ_LAST_LITERALS: usize = 5;
const LZ_MATCH_MARGIN: usize = 12;

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
// A directory leaf record is variable-length: the name hash (u64), the child inode
// (u32), a length byte, then the name's bytes - 13 bytes plus the name, so a 4 KiB
// leaf holds a couple hundred typical entries instead of a fixed few. Records are
// kept sorted by (hash, name) and the whole leaf is rewritten compactly on every
// change (it is copied up by CoW anyway).
const DIR_REC_HDR: usize = 13;

// CRC32C (Castagnoli) lookup tables, built at compile time: eight tables of 256
// entries for slice-by-8, where table[t] advances a byte's contribution through
// 8 - t further zero bytes. The reflected polynomial is 0x82F63B78.
const CRC32C_TABLES: [[u32; 256]; 8] = {
	let mut tables = [[0u32; 256]; 8];
	let mut i = 0;
	while i < 256 {
		let mut crc = i as u32;
		let mut j = 0;
		while j < 8 {
			let mask = (crc & 1).wrapping_neg();
			crc = (crc >> 1) ^ (0x82F6_3B78 & mask);
			j += 1;
		}
		tables[0][i] = crc;
		i += 1;
	}
	let mut t = 1;
	while t < 8 {
		let mut i = 0;
		while i < 256 {
			let prev = tables[t - 1][i];
			tables[t][i] = (prev >> 8) ^ tables[0][(prev & 0xFF) as usize];
			i += 1;
		}
		t += 1;
	}
	tables
};

// CRC32C of a block's bytes: computed on write, stored beside the pointer, and rechecked
// on read so a flipped bit on disk surfaces as `FsError::Corrupt` rather than bad data.
// Slice-by-8: eight bytes advance per table round instead of one, which matters when
// every stored block is checksummed on both sides of the device.
fn crc32c(data: &[u8]) -> u32 {
	let mut crc = 0xFFFF_FFFFu32;
	let mut chunks = data.chunks_exact(8);
	for c in &mut chunks {
		let lo = u32::from_le_bytes([c[0], c[1], c[2], c[3]]) ^ crc;
		let hi = u32::from_le_bytes([c[4], c[5], c[6], c[7]]);
		crc = CRC32C_TABLES[7][(lo & 0xFF) as usize] ^ CRC32C_TABLES[6][((lo >> 8) & 0xFF) as usize] ^ CRC32C_TABLES[5][((lo >> 16) & 0xFF) as usize] ^ CRC32C_TABLES[4][(lo >> 24) as usize] ^ CRC32C_TABLES[3][(hi & 0xFF) as usize] ^ CRC32C_TABLES[2][((hi >> 8) & 0xFF) as usize] ^ CRC32C_TABLES[1][((hi >> 16) & 0xFF) as usize] ^ CRC32C_TABLES[0][(hi >> 24) as usize];
	}
	for &b in chunks.remainder() {
		crc = (crc >> 8) ^ CRC32C_TABLES[0][((crc ^ b as u32) & 0xFF) as usize];
	}
	!crc
}

// A filesystem error. The variants map onto the `Storage.Volume` `error` enum at the
// service boundary (NotFound -> not-found, NoSpace -> again, ReadOnly -> denied, the
// rest -> invalid) - but they stay precise here, so a caller (and a test) can tell a
// bad name from a wrong kind from a non-empty directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
	NotFound,
	NoSpace,
	TooLong,
	// A malformed path or name: an empty segment, "." or "..", not UTF-8, or a byte
	// outside the portable-name policy.
	BadName,
	// The path names a directory where a file was required (writing, truncating).
	IsDir,
	// The path names a file where a directory was required (a path component, rmdir).
	NotDir,
	// Removing or replacing a directory that still has entries.
	NotEmpty,
	// Creating something that already exists (a duplicate snapshot name).
	Exists,
	// An operation the filesystem cannot perform (moving a directory into its own
	// subtree, formatting an impossibly small pool) or an internal inconsistency (an
	// inode record missing from the tree).
	Invalid,
	// A block read back with a CRC32C that did not match the one stored beside its
	// pointer: on-disk corruption, surfaced instead of returning the bad bytes.
	Corrupt,
	Io,
	// The mount is read-only (a snapshot mount, or a volume degraded by a corrupt
	// snapshot table): every mutation is refused so the on-disk state stays intact.
	ReadOnly,
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

// What an [`LiberFs::fsck`] pass found: how many live data blocks failed their checksum
// (on-disk corruption found while walking the trees), and the paths of the live files
// holding them - so the operator knows WHAT is damaged, and [`LiberFs::restore_file`]
// knows what to heal from a pinned generation. (Copy-on-write left fsck nothing to
// reclaim: a crash can no longer leak blocks or orphan an inode.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsckReport {
	pub checksum_failures: u32,
	pub damaged: Vec<Vec<u8>>,
}

// A fixed-size block device: the whole filesystem is read and written one
// BLOCK_SIZE-byte block at a time, addressed by a filesystem-relative block index in
// `0..num_blocks`. Implementors map that onto their backing (disk sectors, a Vec).
pub trait BlockDevice {
	// Read block `index` into `buf` (exactly BLOCK_SIZE bytes). False on I/O failure.
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool;
	// Write `buf` (exactly BLOCK_SIZE bytes) to block `index`. False on I/O failure.
	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool;
	// Make every write issued so far durable (flush the device's volatile write cache)
	// before any later write reaches the medium. The commit protocol brackets the
	// superblock write with this barrier, so crash atomicity holds on devices that
	// reorder cached writes. False on I/O failure. A backing with no volatile cache
	// (memory, a write-through disk) may keep this default no-op.
	fn flush(&mut self) -> bool {
		true
	}
}

// The parsed superblock, cached in memory for the life of a mount. With copy-on-write
// the inode table moves on every commit, so the superblock points at it through an
// index block rather than a fixed region; `generation` orders the two slots and the
// trailing self-CRC catches a torn commit. The identity fields (uuid, label) and the
// compression switch ride along, so they commit atomically with everything else.
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
	// The snapshot table: the first block of the snapshot chain (0 = none) and that
	// block's CRC32C. Carried in the superblock so the pinned snapshots commit atomically
	// with the generation and survive a remount.
	snap_root: u64,
	snap_root_crc: u32,
	// Volume identity: a caller-supplied unique id and a human-readable label.
	uuid: [u8; 16],
	label: [u8; LABEL_MAX],
	// Per-volume transparent compression: chosen at format time, togglable on a live
	// volume; governs new whole-file writes only.
	compress: bool,
}

// Byte offset of the superblock's own CRC32C within its block; the checksum covers the
// whole block with these four bytes zeroed, so a half-written superblock fails it. The
// remaining superblock field offsets, by name, so the serializer and parser cannot
// drift apart:
const SB_CRC_OFFSET: usize = 56;
const SB_MAGIC_OFF: usize = 0;
const SB_VERSION_OFF: usize = 8;
const SB_BLOCK_SIZE_OFF: usize = 12;
const SB_NUM_BLOCKS_OFF: usize = 16;
const SB_NEXT_INODE_OFF: usize = 24;
const SB_GENERATION_OFF: usize = 28;
const SB_INODE_ROOT_OFF: usize = 36;
const SB_INODE_ROOT_CRC_OFF: usize = 44;
const SB_ROOT_INODE_OFF: usize = 52;
const SB_SNAP_ROOT_OFF: usize = 60;
const SB_SNAP_ROOT_CRC_OFF: usize = 68;
const SB_FEATURES_OFF: usize = 72;
const SB_UUID_OFF: usize = 80;
const SB_LABEL_OFF: usize = 96;
const SB_CSUM_ALGO_OFF: usize = 128;
const SB_CODEC_OFF: usize = 129;
const SB_COMPRESS_OFF: usize = 130;

// A named snapshot pins an earlier generation's inode-tree root so its blocks are not
// reclaimed. The snapshot table is a chain of blocks rooted at `snap_root`: each block
// carries the shared chain header (below), then fixed records of a NUL-padded name,
// the pinned inode-tree root and its CRC32C, and the generation - at the named record
// offsets. (4096 - 16) / 84 = 48 records per block; the chain is unbounded, so there
// is no cap on how many snapshots a volume holds.
const SNAP_NAME_MAX: usize = 64;
const SNAP_HDR: usize = CHAIN_HDR;
const SNAP_REC: usize = SNAP_NAME_MAX + 20;
const SNAPS_PER_BLOCK: usize = (BLOCK_SIZE - SNAP_HDR) / SNAP_REC;
// field offsets within one snapshot record, after the name.
const SNAP_ROOT_OFF: usize = SNAP_NAME_MAX;
const SNAP_ROOT_CRC_OFF: usize = SNAP_NAME_MAX + 8;
const SNAP_GEN_OFF: usize = SNAP_NAME_MAX + 12;

// The shared chain-block header, used by both the extent overflow chain and the
// snapshot chain: the next block's pointer (u64) and CRC32C (u32), then a record
// count (u32).
const CHAIN_NEXT_OFF: usize = 0;
const CHAIN_CRC_OFF: usize = 8;
const CHAIN_COUNT_OFF: usize = 12;
const CHAIN_HDR: usize = 16;

// In-memory cache bounds: how many parsed inodes and how many (directory, name) ->
// inode entries are kept between operations, and the largest extent map worth caching
// (a pathologically fragmented file would otherwise hold megabytes of cache). Both
// caches only skip re-reads - every hit was verified when it was first read.
const ICACHE_MAX: usize = 64;
const DCACHE_MAX: usize = 256;
const ICACHE_EXTENTS_MAX: usize = 4096;

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
#[derive(Clone)]
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
		let kind = buf[INO_KIND_OFF];
		let mut owner_tag = [0u8; OWNER_TAG_LEN];
		owner_tag.copy_from_slice(&buf[OWNER_TAG_OFF..OWNER_TAG_OFF + OWNER_TAG_LEN]);
		let mut inode = Inode { kind, size: u64::from_le_bytes(buf[INO_SIZE_OFF..INO_SIZE_OFF + 8].try_into().unwrap()), ctime: u64::from_le_bytes(buf[INO_CTIME_OFF..INO_CTIME_OFF + 8].try_into().unwrap()), mtime: u64::from_le_bytes(buf[INO_MTIME_OFF..INO_MTIME_OFF + 8].try_into().unwrap()), owner_tag, extents: Vec::new(), spill: 0, spill_crc: 0, extent_count: 0, dir_root: 0, dir_root_crc: 0 };
		let map = u64::from_le_bytes(buf[INO_MAP_OFF..INO_MAP_OFF + 8].try_into().unwrap());
		let map_crc = u32::from_le_bytes(buf[INO_MAP_CRC_OFF..INO_MAP_CRC_OFF + 4].try_into().unwrap());
		if kind == KIND_DIR {
			inode.dir_root = map;
			inode.dir_root_crc = map_crc;
		} else {
			inode.spill = map;
			inode.spill_crc = map_crc;
			inode.extent_count = u32::from_le_bytes(buf[INO_EXTENT_COUNT_OFF..INO_EXTENT_COUNT_OFF + 4].try_into().unwrap());
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
		buf[INO_KIND_OFF] = self.kind;
		buf[INO_SIZE_OFF..INO_SIZE_OFF + 8].copy_from_slice(&self.size.to_le_bytes());
		buf[INO_CTIME_OFF..INO_CTIME_OFF + 8].copy_from_slice(&self.ctime.to_le_bytes());
		buf[INO_MTIME_OFF..INO_MTIME_OFF + 8].copy_from_slice(&self.mtime.to_le_bytes());
		buf[OWNER_TAG_OFF..OWNER_TAG_OFF + OWNER_TAG_LEN].copy_from_slice(&self.owner_tag);
		if self.kind == KIND_DIR {
			buf[INO_MAP_OFF..INO_MAP_OFF + 8].copy_from_slice(&self.dir_root.to_le_bytes());
			buf[INO_MAP_CRC_OFF..INO_MAP_CRC_OFF + 4].copy_from_slice(&self.dir_root_crc.to_le_bytes());
		} else {
			buf[INO_MAP_OFF..INO_MAP_OFF + 8].copy_from_slice(&self.spill.to_le_bytes());
			buf[INO_MAP_CRC_OFF..INO_MAP_CRC_OFF + 4].copy_from_slice(&self.spill_crc.to_le_bytes());
			buf[INO_EXTENT_COUNT_OFF..INO_EXTENT_COUNT_OFF + 4].copy_from_slice(&self.extent_count.to_le_bytes());
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
	// In-memory free map, one bit per block, derived at mount and maintained
	// incrementally at each commit - never written to disk.
	free: Vec<u8>,
	// Next-fit allocation cursors: where the next data scan starts (moving up from the
	// pool's low end) and the next metadata scan (moving down from its high end), so an
	// allocation resumes where the last one left off instead of rescanning the pool.
	data_cursor: u64,
	meta_cursor: u64,
	// A reserved run of consecutive data blocks (next block, blocks remaining) that
	// `alloc_data` consumes before falling back to the bitmap scan: a whole-file write
	// reserves its span up front so the file lands contiguously.
	run: Option<(u64, u32)>,
	// Blocks allocated by the in-flight transaction: safe to overwrite in place (no
	// committed generation references them yet).
	fresh: BTreeSet<u64>,
	// Committed blocks the in-flight transaction stopped referencing (`dead`), and those
	// the previous committed transaction dropped (`dead_prev`). The superseded generation
	// still references the latter as the rolling snapshot, so they free at the NEXT
	// commit - each commit clears `dead_prev`'s unpinned bits and promotes `dead`,
	// keeping the free map exact without rewalking the volume.
	dead: BTreeSet<u64>,
	dead_prev: BTreeSet<u64>,
	// Every block a named snapshot pins (one bit per block, rebuilt by the full
	// derivation whenever the snapshot set changes): a dead block that is pinned stays
	// allocated until the snapshot holding it is deleted.
	pinned: Vec<u8>,
	// Did the in-flight transaction create or delete a snapshot? Its commit then runs
	// the full free-map derivation (the pinned set changed) instead of the incremental
	// promotion.
	snapshots_dirty: bool,
	// The state captured at `begin`, restored by `abort` and used by `commit` to reserve
	// the generation it supersedes.
	txn: Option<Txn>,
	// A one-extent cache of the most recently decompressed run, keyed by its first stored
	// block, so a sequential read of a compressed extent decodes it only once.
	decomp: Option<(u64, Vec<u8>)>,
	// The in-flight checksum block being assembled (always a fresh block): sequential
	// writes edit it in memory and it reaches the device once, on eviction or at commit -
	// instead of a read-modify-write per data block.
	wcsum: Option<(u64, Vec<u8>)>,
	// The most recently verified committed checksum block (pointer, its CRC32C, bytes):
	// a sequential read of a long raw extent verifies its checksum block once, not once
	// per data block.
	rcsum: Option<(u64, u32, Vec<u8>)>,
	// Bounded caches of parsed inodes and (directory, name) -> inode lookups, so path
	// resolution and repeated stats stop re-reading the trees; entries are updated on
	// write, dropped on delete, and cleared wholesale on abort.
	icache: BTreeMap<u32, Inode>,
	dcache: BTreeMap<(u32, Vec<u8>), u32>,
	// Refuse every mutation: set for snapshot mounts (writing through one would
	// interleave generations) and when the mount is degraded (a corrupt snapshot table
	// no longer pins its generations, so a commit could reuse pinned blocks).
	read_only: bool,
	// Volume identity and the per-volume compression switch, carried in the superblock.
	uuid: [u8; 16],
	label: [u8; LABEL_MAX],
	compress: bool,
	// One reusable block-sized buffer for the per-block hot paths (the copy-on-write
	// copy loop), taken and returned with mem::take so no allocation rides every block.
	scratch: Vec<u8>,
	clock: u64,
}

// Options for [`LiberFs::format_opts`]: the volume's unique id, its human-readable
// label (truncated to LABEL_MAX bytes), and whether transparent compression starts
// enabled (off by default; togglable later with [`LiberFs::set_compression`]).
#[derive(Clone, Default)]
pub struct FormatOpts {
	pub uuid: [u8; 16],
	pub label: Vec<u8>,
	pub compress: bool,
}

mod blkalloc;
mod dir;
mod fsck;
mod fsops;
mod inode;
mod snapshot;
mod txn;

pub(crate) use blkalloc::*;
pub(crate) use dir::*;
pub(crate) use fsops::*;
pub(crate) use txn::*;

#[cfg(test)]
mod tests;
