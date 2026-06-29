# LiberFS

LiberFS is LiberSystem's native filesystem: a small, writable, copy-on-write
on-disk format that backs the `Storage.Volume` API and survives reboots. It is
built around four ideas - copy-on-write commits, extent-mapped files, per-block
checksums, and snapshots - which together give crash atomicity and data
integrity without a journal, at a fraction of the size of Btrfs or ZFS. It is
written for a single-device VM appliance, so it intentionally drops RAID, dedup,
encryption, quotas, and on-disk permissions; authorization lives in
LiberSystem's capability layer, not in the filesystem.

This document describes how LiberFS is built and what it does, then compares it
with the common removable, Windows, and Linux/Unix filesystems.

## At a glance

- Copy-on-write, single atomic superblock swap per commit (no journal).
- 4 KiB blocks, 64-bit block addresses; a flat pool scaling into exabytes.
- Inodes in a B+tree keyed by number, allocated on demand (no fixed inode table).
- Directories are name-keyed B+trees: O(log n) lookup/insert/remove.
- Files mapped by extents with sparse holes; up to hundreds of GiB.
- CRC32C on every data block and metadata node; corruption surfaces, not silent.
- Transparent per-extent LZSS compression.
- Read-only snapshots, including named snapshots pinning any generation.
- `no_std` userspace build; `std` only under `cargo test`. All I/O through one
  `BlockDevice` trait, so the same code drives virtio-blk and a `Vec` in tests.

## On-disk layout

A LiberFS volume is two superblock slots followed by one flat pool of 4 KiB
blocks:

```
block 0   superblock slot A
block 1   superblock slot B
block 2.. block pool: inode B+tree, directory B+trees, file extents,
          per-extent checksum blocks, snapshot table - all allocated on demand
```

There is no fixed inode table and no on-disk allocation bitmap. Block addresses
are 64-bit, so the volume scales from gigabytes into exabytes. The free map is
reconstructed in memory at mount by walking the blocks the live generations
reference; this is simple and crash-safe, at the cost of a mount that scales with
allocated blocks rather than being O(1).

### Superblocks

The two slots at blocks 0 and 1 hold the format magic (`LIBERFS1`), the version,
the current generation number, the inode-tree root pointer, the snapshot-table
pointer, and a self-CRC. A commit writes the new superblock to the inactive slot;
the active one survives a torn write. Mount picks the slot with the highest valid
generation whose self-CRC passes; a torn write fails its CRC and mount falls back
to the other slot.

### Inodes (a B+tree, not a table)

Inodes live in a B+tree keyed by inode number, one 256-byte slot per inode: a
kind byte, a size, two timestamps, an opaque owner tag, and then either a file's
extent map (an overflow pointer/count plus inline extents) or a directory's
B+tree root pointer and CRC. Because inodes are allocated on demand, a volume
never runs out of inodes while it has free space, and an empty volume wastes
none. The owner tag is stored but never interpreted - it is the capability
layer's, so the filesystem itself carries no permission logic.

### Directories (name-keyed B+trees)

Each directory is its own B+tree keyed by the hash of an entry's name, so lookup,
insert, and remove are O(log n) and one directory can hold millions of entries
with no linear scan. Every tree node is a single block, copy-on-write, with its
CRC32C kept in the parent link.

### Files (extents, sparse holes, compression)

A file maps its data with extents - each a contiguous run of blocks paired with
one checksum block (so one extent spans at most 4 MiB). Four extents sit inline in
the inode; more spill to an overflow chain (built back to front so each block
carries the next block's pointer and CRC). An unwritten range simply has no
extent: a sparse hole that reads back as zeros and costs no blocks. Each run is
compressed with a small dependency-free LZSS coder when its bytes shrink to fewer
blocks, raw otherwise; reads decode transparently, so a file reads back
identically regardless. Editing a compressed run thaws it to raw; a later
whole-file write recompresses it.

## Crash atomicity (copy-on-write, no journal)

A mutation never overwrites a block a committed generation still references.
Changed data, its extent and checksum blocks, the inode, and every inode- and
directory-tree node on the path to it are written to freshly allocated blocks
(copied up once per transaction, then updated in place). The transaction commits
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
records its inode-tree root in a snapshot table, `list_snapshots` enumerates,
`delete_snapshot` drops one, and `mount_named_snapshot` re-roots a read-only
mount at it. The free-map walk reserves every pinned generation, so its blocks
are never reused until the snapshot is deleted.

## Integrity (block checksums + fsck)

Every data block is checksummed with a CRC32C in its extent's checksum block, and
every metadata node beside its own pointer. The checksum is written on write and
rechecked on read, so a flipped bit surfaces as `FsError::Corrupt` instead of
silent bad data. `fsck` walks every live data block - in the live tree and in
every pinned snapshot - and reports how many fail; with copy-on-write a crash
leaks no blocks and orphans no inode, so there is nothing left to reclaim.

## Interfaces

All I/O goes through the `BlockDevice` trait, one fixed 4 KiB block at a time, so
the same code drives a real virtio-blk disk in StorageService and a `Vec`-backed
device in host tests. The crate is `no_std` for userspace and pulls in `std` only
under `cargo test`. It backs `Storage.Volume`, the same typed contract every
other backend uses, so foreign media mounts the same way: LiberSystem reads and
writes FAT (incl. exFAT) and reads ISO9660 and UDF behind that one API.

## Comparison with other filesystems

Legend: yes = supported, no = not supported, layer = handled by another
LiberSystem layer.

| Capability            | FAT12/16/32 | exFAT | NTFS | ext4 | XFS | Btrfs | ZFS | LiberFS |
|-----------------------|:-----------:|:-----:|:----:|:----:|:---:|:-----:|:---:|:-------:|
| Crash atomicity       | none        | none  | journal | journal | journal | CoW | CoW | CoW |
| Copy-on-write         | no          | no    | no   | no   | no  | yes   | yes | yes |
| Data checksums        | no          | no    | no   | metadata | metadata | yes | yes | yes (CRC32C) |
| Snapshots             | no          | no    | VSS  | no   | no  | yes   | yes | yes |
| Compression           | no          | no    | yes  | no   | no  | yes   | yes | yes (LZSS) |
| Sparse files          | no          | no    | yes  | yes  | yes | yes   | yes | yes |
| Dynamic inodes        | n/a         | n/a   | yes  | no   | yes | yes   | yes | yes (B+tree) |
| Dir index             | linear      | linear | B+tree | hashed | B+tree | B+tree | hash | B+tree |
| POSIX perms/ACL       | no          | no    | yes  | yes  | yes | yes   | yes | layer |
| Dedup                 | no          | no    | no   | no   | no  | yes   | yes | no |
| Encryption            | no          | no    | yes  | yes  | no  | yes   | yes | no |
| Multi-device / RAID   | no          | no    | no   | no   | no  | yes   | yes | no |

In design, LiberFS sits closest to Btrfs/ZFS: copy-on-write, end-to-end
checksums, snapshots, and compression. It deliberately omits the enterprise
surface - RAID/pools, dedup, encryption, quotas, on-disk permissions - to stay
small and to keep authorization in the capability layer. The classic FAT family
and exFAT are interchange formats with no integrity or snapshots; NTFS, ext4, and
XFS journal metadata but checksum no data and (mostly) lack snapshots. LiberFS
trades modest file/volume limits for a format small enough to read end to end.

## Out of scope (by decision)

- On-disk permissions/ACLs: only an opaque owner tag; enforcement is the
  capability layer's.
- Deduplication and encryption.
- Multi-device, RAID, pooled storage; single device, single volume.
- Quotas; online defrag/resize beyond the modernization milestones.
