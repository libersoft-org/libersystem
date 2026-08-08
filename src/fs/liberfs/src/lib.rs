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
//! changed data goes to freshly allocated blocks (written outright - a data block is
//! always replaced whole, so nothing is copied first), while the metadata describing
//! it - the extent and checksum blocks, the inode, and the inode- and directory-B+tree
//! nodes on the path - is copied up to a fresh block once per transaction and then
//! edited in place. The
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
//! ## Compression (transparent, per extent, per volume)
//!
//! Compression is a per-volume switch carried in the superblock: OFF by default,
//! chosen at format time and togglable on a live volume; it governs new whole-file
//! writes only. When it is on, a whole-file write compresses each of its runs with a
//! dependency-free LZ4 block-format coder ([`lz_compress`]): a run whose bytes shrink
//! to fewer blocks is stored as a compressed extent - the compressed stream packed
//! into a contiguous run of stored blocks, the original block span kept as the
//! extent's logical `length` - while an incompressible run is left raw. Reads decode
//! the extent transparently, so a file reads back identically whether or not it
//! compressed. The per-block CRC32C covers the stored (compressed) bytes, so
//! integrity and `fsck` work unchanged. Editing a compressed file thaws the touched
//! run back to raw blocks (a later whole-file write recompresses it), keeping partial
//! writes simple. Compression is a space optimization only: it never changes a file's
//! contents or the `Storage.Volume` API.

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
// and algorithm fields, per-volume compression); bit 1 is the 256-byte label field
// (the algorithm bytes moved past it). Unknown or missing bits reject the mount.
const FEATURES: u64 = 0x3;
// Algorithm identifiers recorded in the superblock, so a mount never verifies with the
// wrong checksum or decodes with the wrong codec.
const CSUM_ALGO_CRC32C: u8 = 1;
const CODEC_LZ4: u8 = 2;
// The volume label's fixed on-disk field width (NUL padded).
const LABEL_MAX: usize = 256;

// The two superblock slots (blocks 0 and 1): a commit writes the new superblock to the
// inactive slot, so the active one survives a torn write. The block pool begins right
// after them.
const SUPER_SLOTS: u32 = 2;
const POOL_START: u64 = SUPER_SLOTS as u64;

// One inode is a fixed 256-byte slot: a type byte, a size, two timestamps, then either
// (for a file) the extent map's overflow pointer and count and EXTENTS_INLINE inline
// extents, or (for a directory) its B+tree root pointer and that root's CRC32C. An
// opaque owner tag sits at OWNER_TAG_OFF. Each slot is stored, keyed by inode number,
// in a leaf of the inode B+tree. The field offsets within the slot, by name, so the
// parser and writer cannot drift apart:
const INODE_SIZE: usize = 256;
const INO_TYPE_OFF: usize = 0;
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
// The deepest tree any walk follows. A legitimate B+tree over a 2^64-block pool with
// branching >= 2 never exceeds 64 levels (real trees stay in single digits); a deeper
// path is a hostile shape - a checksummed chain of one-child internals - built to
// overflow the recursive walkers, and fails as Corrupt instead.
const TREE_DEPTH_MAX: usize = 64;

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
// Byte offset of the first inline extent: past the fixed header (type, size, two
// timestamps, the extent-overflow pointer and count) and the owner tag.
const EXTENT_OFF: usize = OWNER_TAG_OFF + OWNER_TAG_LEN;
// (256 - 72) / 40 = 4 extents live inline in the inode; a file of up to four runs needs
// no overflow block at all. Beyond that, extents spill to a chain of extent blocks.
const EXTENTS_INLINE: usize = (INODE_SIZE - EXTENT_OFF) / EXTENT_SIZE;
// A checksum block holds one CRC32C (4 bytes) per stored block of its extent, so an
// extent stores at most this many blocks (1024 = 4 MB) and spans at most that many
// logical blocks. A longer file is several extents.
const CRCS_PER_BLOCK: usize = BLOCK_SIZE / 4;
// An extent-overflow block: the shared chain header (CHAIN_*, below), then the extent
// records. (4096 - 16) / 40 = 102 extents per overflow block.
const EXTENTS_PER_BLOCK: usize = (BLOCK_SIZE - CHAIN_HDR) / EXTENT_SIZE;

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

// Inode types. A live inode record is always a file or a directory; a freed inode is
// deleted from the tree rather than tombstoned, so there is no "free" type.
const TYPE_FILE: u8 = 1;
const TYPE_DIR: u8 = 2;

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
// (u32), a length byte, then the name's bytes - 13 bytes plus the name, so a 4 kB
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
// bad name from a wrong type from a non-empty directory. The type is the shared
// fs-core one, so LiberFS, FAT, ISO9660 and UDF all report through one error enum.
pub use fscore::FsError;

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
// The largest volume this format describes, in blocks: 2^40 blocks is 4 PiB at a 4 KiB
// block, and the free bitmap for one is already 128 GiB - so what bites first is the
// memory for the map, not the width of the field. Formatting or mounting past this is
// refused rather than truncated into a bitmap too small for the volume, which would have
// the allocator hand out blocks it never tracked. Public because it is a limit of the
// FORMAT, and a caller sizing a volume needs to be able to name it.
pub const MAX_BLOCKS: u64 = 1 << 40;

// A zeroed byte map of `len`, or `NoSpace` if the machine will not give it.
//
// Every one of these is sized from a number read off the medium, and `vec![0u8; len]`
// ABORTS the process when the allocator refuses. `MAX_BLOCKS` bounds what the format may
// claim - 4 PiB, whose bitmap is 128 GiB - and a mount builds several such maps, so a
// checksum-consistent superblock naming a legal size could take StorageService down
// through the allocator rather than returning a mount error. The format's ceiling and the
// machine's are different numbers, and only one of them was being checked.
pub(crate) fn try_zeroed(len: usize) -> Result<Vec<u8>, FsError> {
	#[cfg(test)]
	if inject::should_fail() {
		return Err(FsError::NoSpace);
	}
	let mut v: Vec<u8> = Vec::new();
	v.try_reserve_exact(len).map_err(|_| FsError::NoSpace)?;
	v.resize(len, 0);
	Ok(v)
}

// Deterministic refusal of the Nth `try_zeroed`, so the paths that must SURVIVE a refused
// allocation can be exercised without exhausting the host.
//
// The genuine trigger - the machine running out between two allocations of the same size -
// cannot be reached by a test that only chooses `num_blocks`: if the first map fits, so
// does the second. So the paths that answer a refusal were unreachable, which is how one
// of them came to answer it by returning silently and another by blaming the disk.
//
// The budget is a THREAD-LOCAL, because `cargo test` runs tests in parallel and a global
// switch would inject into whichever test happened to be allocating at the time. That is
// the same mistake as the kernel's per-CPU version of this, for the same reason.
#[cfg(test)]
pub(crate) mod inject {
	use core::cell::Cell;

	std::thread_local! {
		// how many more `try_zeroed` calls succeed before one is refused; `None` is disarmed.
		static BUDGET: Cell<Option<usize>> = const { Cell::new(None) };
	}

	// Let `successes` further allocations through, then refuse every one after that until
	// `disarm`. Armed for the calling thread only, which is the test that armed it.
	pub(crate) fn fail_after(successes: usize) {
		BUDGET.with(|b| b.set(Some(successes)));
	}

	pub(crate) fn disarm() {
		BUDGET.with(|b| b.set(None));
	}

	pub(super) fn should_fail() -> bool {
		BUDGET.with(|b| match b.get() {
			None => false,
			Some(0) => true,
			Some(n) => {
				b.set(Some(n - 1));
				false
			}
		})
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsckReport {
	pub checksum_failures: u32,
	pub damaged: Vec<Vec<u8>>,
	// What the STRUCTURAL pass found: shapes that are internally impossible even though
	// every block they live in matches its checksum. A checksum proves a block came back
	// as it was written; it has no opinion about whether what was written can be true.
	// Kept apart from `checksum_failures` because the two mean different things to an
	// operator: one says the medium is failing, the other says the metadata is wrong.
	pub structural_failures: u32,
	// One line per structural fault, in the order found, naming what and where.
	pub faults: Vec<Vec<u8>>,
}

// A fixed-size block device: the whole filesystem is read and written one
// BLOCK_SIZE-byte block at a time, addressed by a filesystem-relative block index in
// `0..num_blocks`. The trait is the shared fs-core one (a block is exactly `buf.len()`
// bytes, so LiberFS's 4 kB blocks, FAT's 512-byte sectors and ISO/UDF's 2048-byte
// blocks all use it); LiberFS's device passes 4 kB buffers and implements the batch
// `read_blocks`, `write_block` and `flush` the write path relies on.
pub use fscore::BlockDevice;

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
const SB_CSUM_ALGO_OFF: usize = SB_LABEL_OFF + LABEL_MAX;
const SB_CODEC_OFF: usize = SB_CSUM_ALGO_OFF + 1;
const SB_COMPRESS_OFF: usize = SB_CODEC_OFF + 1;

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
		// clamp the lengths to what one checksum block can cover (the writer's own
		// ceiling): the record comes off the medium, and a checksummed-but-hostile
		// length must not drive the block loops or decode buffers past all reason.
		// Raw, deliberately. These used to be clamped to CRCS_PER_BLOCK here, which turned
		// an impossible extent into a possible one before anything could object: a forged
		// 5000/5000 arrived at `check_extent` as a perfectly well-formed 1024/1024 raw run
		// and passed. The file was then silently reinterpreted as a shorter run plus a
		// hole, the blocks past the first 1024 were reserved by nobody, `fsck` never saw
		// the value that was actually on the medium, and the mount stayed writable.
		//
		// The ceiling is now the validator's business: a run cannot be longer than the
		// checksum block that vouches for it, and `check_extent` says so.
		let length = u32::from_le_bytes(buf[16..20].try_into().unwrap());
		let store_len = u32::from_le_bytes(buf[32..36].try_into().unwrap());
		Extent { logical: u64::from_le_bytes(buf[0..8].try_into().unwrap()), physical: u64::from_le_bytes(buf[8..16].try_into().unwrap()), length, csum_crc: u32::from_le_bytes(buf[20..24].try_into().unwrap()), csum: u64::from_le_bytes(buf[24..32].try_into().unwrap()), store_len, clen: u32::from_le_bytes(buf[36..40].try_into().unwrap()) }
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

	// The first logical block past the run. Saturating: a hostile `logical` near the
	// address ceiling must not overflow the arithmetic (the range simply ends at the
	// ceiling and the lookup misses).
	fn end(&self) -> u64 {
		self.logical.saturating_add(self.length as u64)
	}

	// Does the run cover logical block `lb`?
	fn covers(&self, lb: u64) -> bool {
		lb >= self.logical && lb < self.end()
	}

	// The stored block at index `i` of the run. Saturating like `end`: a hostile
	// `physical` must not overflow - the read of the saturated address fails or its
	// checksum mismatches, surfacing as the damage it is.
	fn stored(&self, i: u64) -> u64 {
		self.physical.saturating_add(i)
	}
}

// One inode, parsed from / rendered to its 256-byte on-disk slot. A file and a directory
// share the header (type, size, two timestamps, owner tag) but overlay the rest: a file
// keeps its extent map (the inline runs plus the `spill` overflow pointer and the total
// `extent_count`), while a directory keeps its B+tree root (`dir_root` and that root's
// `dir_root_crc`) in the same bytes and leaves the extent fields zero. `extents` is the
// in-memory extent map of a file: `parse` fills only the EXTENTS_INLINE inline runs, and
// [`LiberFs::read_inode`] completes it from the overflow chain rooted at `spill`.
#[derive(Clone)]
struct Inode {
	r#type: u8,
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
	fn empty(r#type: u8) -> Inode {
		Inode { r#type, size: 0, ctime: 0, mtime: 0, owner_tag: [0u8; OWNER_TAG_LEN], extents: Vec::new(), spill: 0, spill_crc: 0, extent_count: 0, dir_root: 0, dir_root_crc: 0 }
	}

	// Parse the fixed header and, for a file, the inline extents (any spilled ones are
	// appended afterwards by `read_inode`); for a directory, the B+tree root pointer.
	// A type byte that is neither TYPE_FILE nor TYPE_DIR (hostile authoring - the
	// writer never emits one) parses file-shaped, by DECISION, and lands harmless
	// end to end: reads and writes refuse it (their TYPE_FILE gates fail), a listing
	// shows it inert, the mark walks reserve its blocks as the file it parses as,
	// `remove` clears it AND returns those blocks, and the structural pass names the
	// type. A change to any `type` branching must keep that story consistent -
	// "file-shaped unless it is a directory" is the rule, and the two ends of it
	// (reserve, release) have to be read together or the space is held until the next
	// remount. The one thing that stops a `remove` is a spill chain that cannot be
	// read, which refuses rather than drop blocks it cannot enumerate - the same
	// answer a TYPE_FILE inode with the same damage gets.
	fn parse(buf: &[u8]) -> Inode {
		let r#type = buf[INO_TYPE_OFF];
		let mut owner_tag = [0u8; OWNER_TAG_LEN];
		owner_tag.copy_from_slice(&buf[OWNER_TAG_OFF..OWNER_TAG_OFF + OWNER_TAG_LEN]);
		let mut inode = Inode { r#type, size: u64::from_le_bytes(buf[INO_SIZE_OFF..INO_SIZE_OFF + 8].try_into().unwrap()), ctime: u64::from_le_bytes(buf[INO_CTIME_OFF..INO_CTIME_OFF + 8].try_into().unwrap()), mtime: u64::from_le_bytes(buf[INO_MTIME_OFF..INO_MTIME_OFF + 8].try_into().unwrap()), owner_tag, extents: Vec::new(), spill: 0, spill_crc: 0, extent_count: 0, dir_root: 0, dir_root_crc: 0 };
		let map = u64::from_le_bytes(buf[INO_MAP_OFF..INO_MAP_OFF + 8].try_into().unwrap());
		let map_crc = u32::from_le_bytes(buf[INO_MAP_CRC_OFF..INO_MAP_CRC_OFF + 4].try_into().unwrap());
		if r#type == TYPE_DIR {
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
		buf[..INODE_SIZE].fill(0);
		buf[INO_TYPE_OFF] = self.r#type;
		buf[INO_SIZE_OFF..INO_SIZE_OFF + 8].copy_from_slice(&self.size.to_le_bytes());
		buf[INO_CTIME_OFF..INO_CTIME_OFF + 8].copy_from_slice(&self.ctime.to_le_bytes());
		buf[INO_MTIME_OFF..INO_MTIME_OFF + 8].copy_from_slice(&self.mtime.to_le_bytes());
		buf[OWNER_TAG_OFF..OWNER_TAG_OFF + OWNER_TAG_LEN].copy_from_slice(&self.owner_tag);
		if self.r#type == TYPE_DIR {
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

	// The overlay field read as a DIRECTORY root, whatever the type byte says.
	//
	// `INO_MAP` holds `dir_root` for a directory and `spill` for everything else, and `parse`
	// commits to one reading from the type byte alone. For an unknown type the byte is the thing
	// that cannot be trusted, so the mark walk has to reserve both readings - and for the file
	// reading it already has `spill`, which is the same field.
	fn dir_root_from_overlay(&self) -> u64 {
		if self.r#type == TYPE_DIR { self.dir_root } else { self.spill }
	}

	fn dir_root_crc_from_overlay(&self) -> u32 {
		if self.r#type == TYPE_DIR { self.dir_root_crc } else { self.spill_crc }
	}

	// Number of data blocks the file's `size` occupies. u64, so a 32-bit build never
	// truncates a large file's block count.
	fn nblocks(&self) -> u64 {
		self.size.div_ceil(BLOCK_SIZE as u64)
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
	// The per-volume compression switch, saved with everything else because
	// `set_compression` changes it INSIDE the transaction. Without it here, a commit
	// that failed at its first barrier left the caller told `Io`, the disk without the
	// change, and the filesystem in memory reporting it as made - and writing it into
	// the superblock on the next unrelated commit.
	compress: bool,
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
// The number of recently-decompressed compressed runs LiberFs keeps in memory. A small
// LRU rather than a single slot, so alternating sequential reads over a handful of
// compressed extents each decode once instead of evicting one another on every switch.
pub(crate) const DECOMP_CACHE_ENTRIES: usize = 8;

// An LRU cache of decompressed compressed runs, keyed by each run's first stored block.
// The most-recently-used entry is last; a full cache evicts the least-recently-used one
// (at the front). Bounded to DECOMP_CACHE_ENTRIES runs, so a pathological read pattern
// cannot grow it without limit.
// Why a mount failed, because the caller's next move depends on it.
//
// This was an `Option`, and the storage service read every `None` as "no filesystem here" and laid
// down a fresh one. A disk that failed to answer, a volume written by a build that reads a
// different layout, and a genuinely blank disk were the same answer - so a transient device fault
// at boot was enough to reformat a healthy system volume.
//
// The tree had already fought this twice: a corrupt snapshot table degrades the mount to read-only
// rather than failing, and so does an incomplete generation walk, precisely because failing would
// "present the volume as unformatted (and cost its data to the next format)". This finishes that
// argument for the paths that were left out of it.
//
// Only `Unformatted` may be answered by formatting. Everything else keeps every byte where it is.
// Which generation a mount is asking for, and on what terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountMode {
	// The live generation, writable. Refuses when anything about the OTHER slot is
	// unknown, because a writable mount that proceeds anyway can overwrite it.
	Newest,
	// The generation one commit back, read-only. Needs both slots valid by definition.
	Previous,
	// The newest slot that parses, whatever the other one did - always read-only, so
	// nothing it cannot account for can be overwritten.
	Recovery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountError {
	// No superblock: a blank disk, or one belonging to something else.
	Unformatted,
	// Ours, and written by a build this one cannot read - a version, feature set or algorithm.
	Unsupported,
	// Ours and readable, and its structure failed its own checks.
	Corrupt,
	// The device did not answer. Says nothing about what is on it.
	Io,
	// The medium does not cover the pool the superblock claims.
	DeviceTooSmall,
	// The volume is a size this machine cannot hold the free maps for. A limit of the
	// RUNNING SYSTEM rather than of the format - the same volume may mount elsewhere,
	// and nothing about the medium is wrong. Reported instead of aborting the process
	// through the allocator, which is what an infallible `vec!` did.
	NoMemory,
}

// What identifies a cached decode. The address alone is not enough: truncating a live
// file leaves the physical blocks and the compressed stream untouched while shrinking
// `length`, so the live extent and the snapshot's longer one share a starting block and
// describe different data. Reading the live version cached the SHORT decode, and the
// snapshot then read its first block correctly and got zeros for the rest - which
// `restore_file` could write back into the live tree as fact.
pub(crate) type DecompKey = (u64, u32, u32);

pub(crate) struct DecompCache {
	entries: Vec<(DecompKey, Vec<u8>)>,
}

impl DecompCache {
	pub(crate) fn new() -> DecompCache {
		DecompCache { entries: Vec::new() }
	}

	// The decompressed bytes of the run keyed at `key`, promoted to most-recently-used, or
	// None on a miss.
	pub(crate) fn get(&mut self, key: DecompKey) -> Option<&[u8]> {
		let pos = self.entries.iter().position(|(k, _)| *k == key)?;
		let entry = self.entries.remove(pos);
		self.entries.push(entry);
		Some(&self.entries.last().unwrap().1)
	}

	// Cache `data` for the run keyed at `key` as most-recently-used, evicting the least-
	// recently-used run when the cache is full. A key already present is replaced.
	pub(crate) fn insert(&mut self, key: DecompKey, data: Vec<u8>) {
		if let Some(pos) = self.entries.iter().position(|(k, _)| *k == key) {
			self.entries.remove(pos);
		}
		if self.entries.len() >= DECOMP_CACHE_ENTRIES {
			self.entries.remove(0);
		}
		self.entries.push((key, data));
	}

	// Drop every cached run starting at `block` (its stored blocks are being rewritten).
	// All of them: one address can now carry several keys, and the one being invalidated
	// is the one whose bytes are about to change - which is every one of them.
	pub(crate) fn forget(&mut self, block: u64) {
		self.entries.retain(|((p, _, _), _)| *p != block);
	}

	// Drop every cached run (a transaction boundary reused blocks the cache described).
	pub(crate) fn clear(&mut self) {
		self.entries.clear();
	}
}

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
	// The previous generation's SNAPSHOT table, kept for the same reason as its inode
	// root. Without it, deleting a snapshot and committing left the older superblock
	// describing a generation in which that snapshot was live, while its table blocks
	// and the blocks its root reached had already been declared free - so a crash before
	// the next commit could leave a mountable superblock naming data that was gone.
	prev_snap_root: u64,
	prev_snap_root_crc: u32,
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
	// A small LRU cache of the most recently decompressed runs, keyed by each run's first
	// stored block, so alternating sequential reads over a few compressed extents each
	// decode once instead of thrashing a single slot.
	decomp: DecompCache,
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
	// Set by the free-map generation walks when a file's spill chain or a tree read
	// fails mid-walk: the derived free map is then incomplete, so the walk's caller
	// must degrade (a mount goes read-only, a commit refuses) rather than allocate
	// from a map that may hand out live blocks.
	walk_damage: bool,
	// Two-owner detection for the free-map walks. Marking a bitmap is idempotent -
	// setting a bit twice is setting a bit - so an image in which two files point at one
	// data block, or two parents at one subtree, derives a free map that looks perfect.
	// Delete one owner and the block joins `dead`; a commit later hands it out while the
	// other owner is still reading it.
	//
	// `mark_strict` is set only for the walk of the LIVE generation, where every block
	// has exactly one owner. Sharing ACROSS generations is the whole point of
	// copy-on-write and stays legal: the previous generation and the snapshots are
	// marked into their own maps, and all the snapshots share one map precisely because
	// they are expected to share subtrees with each other. `mark_dup` holds the first
	// block seen twice, which `derive_free` turns into corruption.
	mark_strict: bool,
	mark_dup: Option<u64>,
	// The highest inode key the live walk saw. `next_inode` hands out numbers ABOVE
	// everything in use, and checking only that `next_inode` itself is free is not that
	// invariant: with inodes 1 and 3 live and 2 deleted, a counter of 2 passes - and the
	// second file created then takes 3 and overwrites the one that exists, along with
	// every name pointing at it.
	mark_max_inode: u64,
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
