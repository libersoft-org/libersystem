# LiberFS

LiberFS is LiberSystem's native filesystem: a small, writable, copy-on-write
on-disk format that backs the `Storage.Volume` API and survives reboots. It is
built around four ideas - copy-on-write commits, extent-mapped files, per-block
checksums, and snapshots - which together give crash atomicity and data
integrity without a journal, at a fraction of the size of Btrfs or ZFS. It is
written for a single-device VM appliance, so it intentionally drops RAID, dedup,
encryption, quotas, and on-disk permissions; authorization lives in
LiberSystem's capability layer, not in the filesystem.

This document goes from the general to the specific: what LiberFS is and how it
compares with the common removable, Windows, and Linux/Unix filesystems, then
the design (layout, crash atomicity, snapshots, integrity), and finally the
formal on-disk specification and the rules a foreign driver must honor.

## At a glance

- Copy-on-write, single atomic superblock swap per commit (no journal), bracketed
  by device flush barriers so the ordering holds on a volatile write cache.
- 4 KiB blocks, 64-bit block addresses; a flat pool scaling into exabytes.
- Inodes in a B+tree keyed by number, allocated on demand (no fixed inode table).
- Directories are name-keyed B+trees with variable-length records: O(log n)
  lookup/insert/remove, a couple hundred typical entries per leaf.
- Files mapped by extents with sparse holes; file sizes to 16 EiB, volumes to
  64 ZiB (the limits table below).
- CRC32C (slice-by-8) on every data block and metadata node; corruption surfaces,
  not silent - and `fsck` names the damaged files, `restore` heals them from a
  snapshot.
- Transparent per-extent LZ4 compression - per-volume, off by default, togglable
  on a live volume.
- Read-only snapshots (enforced), including named snapshots pinning any
  generation - unbounded, in a chained table.
- Volume identity in the superblock: uuid, label, feature flags, algorithm ids.
- OS- and architecture-agnostic by construction: every field little-endian at a
  fixed offset, position-independent block addressing, and a GPT partition type
  GUID (`4C424653-0001-4000-8000-4C6962657246`) so any system finds the volume.
- `no_std` userspace build; `std` only under `cargo test`. All I/O through one
  `BlockDevice` trait (read, write, flush), so the same code drives virtio-blk
  and a `Vec` in tests.

## Comparison with other filesystems

Legend: ✓ = supported, ✗ = not supported, layer = handled by another
LiberSystem layer.

| Capability            | LiberFS | ZFS | Btrfs | XFS | ext4 | NTFS | exFAT | FAT12/16/32 |
|-----------------------|:-------:|:---:|:-----:|:---:|:----:|:----:|:-----:|:-----------:|
| Crash atomicity       | CoW     | CoW | CoW   | journal | journal | journal | ✗ | ✗ |
| Copy-on-write         | ✓       | ✓   | ✓     | ✗   | ✗    | ✗    | ✗     | ✗ |
| Data checksums        | ✓ (CRC32C) | ✓ | ✓   | metadata | metadata | ✗ | ✗ | ✗ |
| Snapshots             | ✓       | ✓   | ✓     | ✗   | ✗    | VSS  | ✗     | ✗ |
| Compression           | ✓ (LZ4, per-volume switch) | ✓ | ✓ | ✗ | ✗ | ✓ | ✗ | ✗ |
| Sparse files          | ✓       | ✓   | ✓     | ✓   | ✓    | ✓    | ✗     | ✗ |
| Dynamic inodes        | ✓ (B+tree) | ✓ | ✓   | ✓   | ✗    | ✓    | n/a   | n/a |
| Dir index             | B+tree  | hash | B+tree | B+tree | hashed | B+tree | linear | linear |
| POSIX perms/ACL       | layer   | ✓   | ✓     | ✓   | ✓    | ✓    | ✗     | ✗ |
| Dedup                 | ✗       | ✓   | ✓     | ✗   | ✗    | ✗    | ✗     | ✗ |
| Encryption            | ✗       | ✓   | ✓     | ✗   | ✓    | ✓    | ✗     | ✗ |
| Multi-device / RAID   | ✗       | ✓   | ✓     | ✗   | ✗    | ✗    | ✗     | ✗ |

### Limits

Format limits (the on-disk structures' ceilings); common practical figures for
the others.

| Limit | LiberFS | ZFS | Btrfs | XFS | ext4 | NTFS | exFAT | FAT12/16/32 |
|-------|:-------:|:---:|:-----:|:---:|:----:|:----:|:-----:|:-----------:|
| Max volume size | 64 ZiB (2^64 × 4 KiB blocks) | 256 ZiB | 16 EiB | 8 EiB | 1 EiB | 8 PiB | 128 PiB | 2 TiB (FAT32; 16 TiB with 4 KiB sectors) |
| Max file size | 16 EiB logical (u64 size; sparse); 16 PiB dense (2^32 extents × 4 MiB) | 16 EiB | 16 EiB | 8 EiB | 16 TiB | 8 PiB | 128 PiB | 4 GiB − 1 |
| Max files | 2^32 inode numbers over the volume's lifetime (never reused) | 2^48 per directory | 2^64 | 2^64 (dynamic) | fixed at mkfs (≤ 2^32) | 2^32 − 1 | ~2.8 M per directory | ~268 M (FAT32) |
| Max entries per directory | unbounded (B+tree) | 2^48 | 2^64 | 2^64 | ~10-12 M practical (htree) | 2^32 − 1 | ~2.8 M | 65 534 (FAT16/32 subdir) |
| Max name length | 255 bytes (UTF-8) | 255 bytes | 255 bytes | 255 bytes | 255 bytes | 255 UTF-16 units | 255 UTF-16 units | 8.3 (255 UTF-16 with VFAT LFN) |
| Max path length | unbounded | unbounded | unbounded | unbounded | unbounded | 32 767 UTF-16 units | 32 767 | 260 (classic Windows APIs) |
| Snapshots per volume | unbounded (chained table) | 2^64 | unbounded | n/a | n/a | VSS-bound | n/a | n/a |
| Volume label | 32 bytes UTF-8 | 256 | 256 | 12 | 16 | 32 UTF-16 | 11 | 11 |

LiberFS's figures are format ceilings; today's implementation also keeps a
1-bit-per-block free map in memory (32 MiB of RAM per TiB of volume), which is
the practical ceiling until a persistent space map replaces it. The inode-number
ceiling counts every file ever created, not just live ones - a design trade for
O(1) revocation-free numbering; 2^32 creates outlast an appliance's life by a
wide margin.

In design, LiberFS sits closest to Btrfs/ZFS: copy-on-write, end-to-end
checksums, snapshots, and compression. It deliberately omits the enterprise
surface - RAID/pools, dedup, encryption, quotas, on-disk permissions - to stay
small and to keep authorization in the capability layer. The classic FAT family
and exFAT are interchange formats with no integrity or snapshots; NTFS, ext4, and
XFS journal metadata but checksum no data and (mostly) lack snapshots. LiberFS
trades the enterprise feature surface, not the limits, for a format small
enough to read end to end.

## Out of scope (by decision)

- On-disk permissions/ACLs: only an opaque owner tag; enforcement is the
  capability layer's.
- Hard links and symlinks (no nlink field): names bind capabilities, and
  aliasing complicates the one-name-one-file model; revisit with a concrete need.
- Deduplication and encryption.
- Multi-device, RAID, pooled storage; single device, single volume.
- Quotas; online defrag/resize beyond the modernization milestones.
- Metadata duplication (a DUP profile for true metadata self-heal) - a candidate
  feature flag if single-device self-heal ever becomes a requirement.

## On-disk layout

A LiberFS volume is two superblock slots followed by one flat pool of 4 KiB
blocks:

```
block 0   superblock slot A
block 1   superblock slot B
block 2.. block pool: inode B+tree, directory B+trees, file extents,
          per-extent checksum blocks, snapshot chain - all allocated on demand
```

There is no fixed inode table and no on-disk allocation bitmap. Block addresses
are 64-bit, so the volume scales from gigabytes into exabytes. The free map is
reconstructed in memory at mount by walking the blocks the live generations
reference, then maintained INCREMENTALLY: each transaction records the committed
blocks it stopped referencing, and the commit after next frees them (the
superseded generation pins them for one commit; a named snapshot pins them for
its lifetime). A commit therefore never rewalks the volume - only mount, fsck,
and the snapshot ops do.

### Superblocks

The two slots at blocks 0 and 1 hold the format magic (`LIBERFS1`), the version,
a feature-flags word (recording layout revisions, so an older build's volume is
detected rather than mis-parsed), the volume's uuid and label, the checksum and
compression algorithm ids, the per-volume compression switch, the current
generation number, the inode-tree root pointer, the snapshot-chain pointer, and
a self-CRC. A commit writes the new superblock to the inactive slot; the active
one survives a torn write. Mount picks the slot with the highest valid
generation whose self-CRC passes; a torn write fails its CRC and mount falls
back to the other slot. The commit is bracketed by device flush barriers
(`BlockDevice::flush`, VIRTIO_BLK_T_FLUSH / SCSI SYNCHRONIZE CACHE at the
drivers), so a volatile write cache cannot reorder the commit point ahead of
the data it names.

### Inodes (a B+tree, not a table)

Inodes live in a B+tree keyed by inode number, one 256-byte slot per inode: a
kind byte, a size, two timestamps, an opaque owner tag, and then either a file's
extent map (an overflow pointer/count plus inline extents) or a directory's
B+tree root pointer and CRC. Because inodes are allocated on demand, a volume
never runs out of inodes while it has free space, and an empty volume wastes
none. The owner tag is stored but never interpreted - it is the capability
layer's, so the filesystem itself carries no permission logic.

### Directories (name-keyed B+trees)

Each directory is its own B+tree keyed by the FNV-1a hash of an entry's name, so
lookup, insert, and remove are O(log n) and one directory can hold millions of
entries with no linear scan. Leaf records are variable-length (13 bytes plus the
name), so a 4 KiB leaf holds a couple hundred typical entries; records sharing a
hash stay in one leaf, disambiguated by the name bytes. Names must be valid
UTF-8 and pass a portable-name policy, so one file has one name and it moves
cleanly to foreign media. Every tree node is a single block, copy-on-write, with
its CRC32C kept in the parent link. There are no hard links or symlinks - by
decision: names bind capabilities in LiberSystem, and aliasing complicates the
one-name-one-file model the security layer leans on; revisit only with a
concrete need.

### Files (extents, sparse holes, compression)

A file maps its data with extents - each a contiguous run of blocks paired with
one checksum block (so one extent spans at most 4 MiB). Four extents sit inline in
the inode; more spill to an overflow chain (built back to front so each block
carries the next block's pointer and CRC). An unwritten range simply has no
extent: a sparse hole that reads back as zeros and costs no blocks. When the
volume's compression switch is on (off by default; togglable live), each run of a
whole-file write is compressed with a dependency-free LZ4 block coder when its
bytes shrink to fewer blocks, raw otherwise; every source block is verified
against its checksum first, so compression never launders damage. Reads decode
transparently. Editing a compressed run thaws it to raw; a later whole-file
write recompresses it.

## Crash atomicity (copy-on-write, no journal)

A mutation never overwrites a block a committed generation still references.
Changed data goes to freshly allocated blocks (written outright - a data block is
always replaced whole, so nothing is copied first); the metadata describing it -
the extent and checksum blocks, the inode, and every inode- and directory-tree
node on the path - is copied up to a fresh block once per transaction and then
edited in place. The transaction commits
with a single atomic write of a new superblock - a bumped generation plus a
self-CRC - to the inactive slot. A crash before that write leaves the old
superblock active and the old tree intact; a torn superblock write fails its
CRC. So a crash mid-write always leaves either the whole old file or the whole
new file, never a torn mix - and never an orphaned inode or leaked block, so
`fsck` need not reclaim anything.

## Snapshots

Because the previous generation's blocks are not freed at commit, the slot it
occupies stays a consistent read-only snapshot of the volume one commit ago;
`mount_snapshot` opens it. Named snapshots pin any generation: `create_snapshot`
records its inode-tree root in a chained snapshot table (unbounded - 48 records
per chain block), `list_snapshots` enumerates, `delete_snapshot` drops one, and
`mount_named_snapshot` re-roots a read-only mount at it. Snapshot mounts REFUSE
every mutation (`FsError::ReadOnly`), so generations can never interleave. The
free map reserves every pinned generation, so its blocks are never reused until
the snapshot is deleted; a corrupt snapshot table degrades the mount to
read-only instead of silently unpinning anything.

## Integrity (block checksums + fsck + restore)

Every data block is checksummed with a CRC32C (slice-by-8) in its extent's
checksum block, and every metadata node beside its own pointer. The checksum is
written on write and rechecked on read, so a flipped bit surfaces as
`FsError::Corrupt` instead of silent bad data. `fsck` walks the live namespace
and every pinned snapshot, verifies every block, and NAMES the damaged files;
`restore_file` then copies a named file out of a snapshot (or the previous
generation) over the live one - explicitly that generation's older version, the
operator's call. True self-heal is impossible on this format: under
copy-on-write the generations usually share the physical block, so a damaged
shared block has no second copy - restore heals what a pinned generation still
holds intact. With copy-on-write a crash leaks no blocks and orphans no inode,
so there is nothing for fsck to reclaim.

## Interfaces

All I/O goes through the `BlockDevice` trait, one fixed 4 KiB block at a time, so
the same code drives a real virtio-blk disk in StorageService and a `Vec`-backed
device in host tests. The crate is `no_std` for userspace and pulls in `std` only
under `cargo test`. It backs `Storage.Volume`, the same typed contract every
other backend uses, so foreign media mounts the same way: LiberSystem reads and
writes FAT (incl. exFAT) and reads ISO9660 and UDF behind that one API.

## On-disk format specification (version 1, features 0x1)

The authoritative field-level layout, sufficient for an independent
implementation on any OS or architecture. The reference test suite pins these
tables byte for byte (`the_superblock_layout_matches_the_specification`,
`the_record_layouts_match_the_specification`); a layout change must set a new
feature-flag bit and update this section.

### General rules

- Every multi-byte integer is **little-endian**, at a fixed byte offset. No
  structure is a memory dump: there is no padding, no alignment requirement, and
  no host-endian field, so the format is identical on every architecture.
- The block size is **4096 bytes**. Block addresses are u64 indexes **relative
  to the volume's first byte** (block 0 = the container's start), so the volume
  is position-independent.
- The **container** is either a GPT partition whose type GUID is
  `4C424653-0001-4000-8000-4C6962657246` (the LiberFS partition type), or - on a
  LiberSystem factory disk without a GPT - the fixed region starting at
  512-byte sector 32768 (16 MiB in, past the boot archive).
- The checksum everywhere is **CRC32C** (Castagnoli): polynomial 0x1EDC6F41
  (reflected 0x82F63B78), reflected input/output, initial value and final XOR
  0xFFFFFFFF. Test vector: `crc32c("123456789") = 0xE3069283`.
- Compression is the **LZ4 block format**; a compressed stream is prefixed with
  its uncompressed length as a u32 (LE).
- All "reserved" bytes are written as zero and ignored on read.

### Superblock (blocks 0 and 1)

| Offset | Size | Field |
|-------:|-----:|-------|
| 0   | 8  | magic `LIBERFS1` (ASCII) |
| 8   | 4  | version, = 1 |
| 12  | 4  | block size, = 4096 |
| 16  | 8  | num_blocks: the pool size in blocks (the volume's whole span) |
| 24  | 4  | next_inode: the next inode number to hand out (monotonic, never reused) |
| 28  | 8  | generation (monotonic; the higher valid slot is the live root) |
| 36  | 8  | inode_root: block of the inode B+tree root |
| 44  | 4  | inode_root_crc: CRC32C of that root block |
| 48  | 4  | reserved |
| 52  | 4  | root_inode: the root directory's inode number, = 0 |
| 56  | 4  | self-CRC: CRC32C over the whole block with these 4 bytes zeroed |
| 60  | 8  | snap_root: first block of the snapshot chain (0 = none) |
| 68  | 4  | snap_root_crc: CRC32C of that block |
| 72  | 8  | feature flags, = 0x1; a reader MUST reject unknown or missing bits |
| 80  | 16 | volume uuid (opaque 16 bytes, assigned at format) |
| 96  | 32 | volume label (UTF-8, NUL padded) |
| 128 | 1  | checksum algorithm id, = 1 (CRC32C) |
| 129 | 1  | compression codec id, = 2 (LZ4) |
| 130 | 1  | compression switch: 1 = new whole-file writes compress |
| 131 | -  | reserved to end of block |

Mount: parse both slots; a slot is valid if magic, version, block size, feature
flags, algorithm ids and the self-CRC all check. The valid slot with the higher
generation is the live root; the other (if valid) is the previous generation.

### B+tree nodes (one block each)

Node header (8 bytes): type u8 at 0 (0 = internal, 1 = leaf), byte 1 reserved,
entry count u16 at 2, bytes 4..8 reserved.

**Internal node** (both trees): separator keys (u64) at `8 + i*8`; child links
at `1632 + i*12`, each a block pointer (u64) plus that child block's CRC32C
(u32). `count` separators and `count + 1` children; at most 203 separators (204
children). Child `i` holds keys below separator `i`, child `i + 1` keys at or
above it. Every child link's CRC verifies the child block on read.

**Inode-tree leaf**: `count` fixed 264-byte records at `8 + i*264`, sorted by
key: the inode number as u64, then the 256-byte inode slot (below). At most 15
records per leaf.

**Directory leaf**: `count` variable-length records back to back from offset 8,
sorted by (hash, name): name hash u64, child inode u32, name length u8, then
the name's bytes (13 bytes + name). Records sharing a hash never straddle a
leaf split, since internal nodes route by hash alone. The hash is FNV-1a
64-bit over the name's bytes (offset basis 0xCBF29CE484222325, prime
0x100000001B3).

### Inode slot (256 bytes, inside an inode-tree leaf record)

| Offset | Size | Field |
|-------:|-----:|-------|
| 0  | 1  | kind: 1 = file, 2 = directory |
| 1  | 7  | reserved |
| 8  | 8  | size: file bytes, or a directory's live entry count |
| 16 | 8  | ctime (u64 seconds since the Unix epoch, UTC) |
| 24 | 8  | mtime (likewise) |
| 32 | 8  | file: extent-overflow chain block (0 = none) / directory: dir-tree root block (0 = empty) |
| 40 | 4  | CRC32C of that block (chain head / tree root) |
| 44 | 4  | file only: total extent count (inline + spilled) |
| 48 | 8  | reserved |
| 56 | 16 | owner tag (opaque; never interpreted by the filesystem) |
| 72 | 160 | file only: up to 4 inline extent records (40 bytes each) |
| 232 | 24 | reserved |

### Extent record (40 bytes)

| Offset | Size | Field |
|-------:|-----:|-------|
| 0  | 8 | logical: first logical block of the run |
| 8  | 8 | physical: first stored block |
| 16 | 4 | length: logical blocks the run covers |
| 20 | 4 | csum_crc: CRC32C of the checksum block |
| 24 | 8 | csum: the run's checksum block |
| 32 | 4 | store_len: stored (physical) blocks; = length raw, < length compressed |
| 36 | 4 | clen: compressed byte length (0 = raw) |

A file's extents are sorted by `logical`; a logical block no extent covers is a
sparse hole reading as zeros. The **checksum block** holds one CRC32C (4 bytes)
per stored block, slot `i` at byte `i*4` covering stored block `physical + i`;
one extent therefore spans at most 1024 blocks (4 MiB). A **compressed extent**
(`clen > 0`) stores the `clen`-byte stream (u32 LE uncompressed length + LZ4
block format) across `store_len` blocks, zero-padded; the per-block CRCs cover
the stored (compressed) bytes.

### Chain blocks (extent overflow and snapshot table)

A chain block starts with a 16-byte header: next block u64 at 0 (0 = end),
next block's CRC32C u32 at 8, record count u32 at 12; records follow from 16.
The chain is verified link by link: the superblock (or inode) holds the first
block's CRC, each block holds the next one's.

- **Extent overflow** (a file's 5th extent onward): 40-byte extent records,
  at most 102 per block.
- **Snapshot chain** (`snap_root`): 84-byte records, at most 48 per block:
  name (64 bytes, UTF-8, NUL padded), pinned inode-tree root u64 at 64, its
  CRC32C u32 at 72, pinned generation u64 at 76. The chain is unbounded.

### Commit protocol and reachability

A writer MUST follow copy-on-write: never overwrite any block reachable from
either superblock slot or from any snapshot record. A transaction writes its
new blocks, then: **flush** the device's write cache, write the new superblock
(generation + 1) to the inactive slot, **flush** again. The single superblock
write is the commit point.

A block is **allocated** if and only if it is block 0 or 1, or reachable from
the live root, the previous valid root, the snapshot chain's blocks, or any
snapshot's pinned root (tree nodes, inode slots' data/checksum/overflow blocks,
directory tree nodes). Everything else is free; there is no on-disk bitmap. A
reader deriving the free map walks exactly this closure.

## Semantics a foreign driver must honor

- **Names** are byte-exact UTF-8, **case-sensitive**, with **no Unicode
  normalization**: two byte sequences are two names. A segment is 1..=255
  bytes, must not be `.` or `..`, and must not contain `/`, NUL, control bytes
  (0x00-0x1F, 0x7F) or `\ : * ? < > | "`. Enforce on create; treat violations
  in on-disk data as corruption.
- **No hard links, no symlinks** - one name, one file. Do not synthesize them.
- **Timestamps** are u64 seconds since the Unix epoch, UTC; only ctime and
  mtime exist. Synthesize atime (= mtime) and finer resolutions as needed.
- **No permissions/ACLs on disk**: authorization is LiberSystem's capability
  layer. Present mount-wide synthesized ownership/permissions (the FAT-driver
  convention: mount-time uid/gid/umask). The 16-byte **owner tag is opaque** -
  preserve it, never interpret or invent it.
- **Snapshots are read-only**: a mount re-rooted at a previous generation or a
  named snapshot must refuse every mutation, or it interleaves generations.
- **Writers follow the commit protocol** above (CoW + flush-superblock-flush)
  or mount read-only. Respect the compression switch and codec id; writing raw
  extents is always valid regardless of the switch.
- A **corrupt snapshot chain** (a link failing its CRC) must degrade the mount
  to read-only, never silently drop the table - the pinned generations' blocks
  would be reused.
