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

- Copy-on-write, single atomic superblock swap per commit (no journal), bracketed
  by device flush barriers so the ordering holds on a volatile write cache.
- 4 KiB blocks, 64-bit block addresses; a flat pool scaling into exabytes.
- Inodes in a B+tree keyed by number, allocated on demand (no fixed inode table).
- Directories are name-keyed B+trees with variable-length records: O(log n)
  lookup/insert/remove, a couple hundred typical entries per leaf.
- Files mapped by extents with sparse holes; up to hundreds of GiB.
- CRC32C (slice-by-8) on every data block and metadata node; corruption surfaces,
  not silent - and `fsck` names the damaged files, `restore` heals them from a
  snapshot.
- Transparent per-extent LZ4 compression - per-volume, off by default, togglable
  on a live volume.
- Read-only snapshots (enforced), including named snapshots pinning any
  generation - unbounded, in a chained table.
- Volume identity in the superblock: uuid, label, feature flags, algorithm ids.
- `no_std` userspace build; `std` only under `cargo test`. All I/O through one
  `BlockDevice` trait (read, write, flush), so the same code drives virtio-blk
  and a `Vec` in tests.

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

## Comparison with other filesystems

Legend: yes = supported, no = not supported, layer = handled by another
LiberSystem layer.

| Capability            | FAT12/16/32 | exFAT | NTFS | ext4 | XFS | Btrfs | ZFS | LiberFS |
|-----------------------|:-----------:|:-----:|:----:|:----:|:---:|:-----:|:---:|:-------:|
| Crash atomicity       | none        | none  | journal | journal | journal | CoW | CoW | CoW |
| Copy-on-write         | no          | no    | no   | no   | no  | yes   | yes | yes |
| Data checksums        | no          | no    | no   | metadata | metadata | yes | yes | yes (CRC32C) |
| Snapshots             | no          | no    | VSS  | no   | no  | yes   | yes | yes |
| Compression           | no          | no    | yes  | no   | no  | yes   | yes | yes (LZ4, per-volume switch) |
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
- Hard links and symlinks (no nlink field): names bind capabilities, and
  aliasing complicates the one-name-one-file model; revisit with a concrete need.
- Deduplication and encryption.
- Multi-device, RAID, pooled storage; single device, single volume.
- Quotas; online defrag/resize beyond the modernization milestones.
- Metadata duplication (a DUP profile for true metadata self-heal) - a candidate
  feature flag if single-device self-heal ever becomes a requirement.
