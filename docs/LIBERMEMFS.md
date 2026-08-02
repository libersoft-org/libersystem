# LiberMemFS

LiberMemFS is LiberSystem's in-memory filesystem: a writable volume that holds its files
directly on the heap, backs the same `Storage.Volume` API as every other volume, and keeps
nothing across a reboot. It exists because every other backend in the system reads a medium -
LiberFS a disk, FAT removable media, ISO9660 and UDF optical images - so there was nowhere to
write that did not cost a disk write and did not outlive the thing that wrote it.

It has no on-disk format. There is no superblock, no allocator, no journal and no checksum,
because there is no medium those defend against. A file IS an allocation and a directory IS a
map of names, so this document is short by construction: most of what a filesystem document
usually describes does not exist here.

## At a glance

- Files are heap allocations; directories are ordered maps of names. No block layer.
- Two mount policies over one implementation: `Reserved` charges its capacity at mount,
  `Capped` charges as files are written.
- Bounded everywhere: file size, entry count, name length and path depth all refuse past their
  limit rather than truncating.
- `no_std` + `alloc`, ~270 lines, 12 unit tests. It implements the storage service's
  `FileSystem` trait directly, NOT `fscore::BlockDevice` - there is no block device under it.
- Mounted as `vol://ram` (reserved) and `vol://tmp` (capped).

## Why not LiberFS on a RAM disk

The obvious alternative was a `BlockDevice` over a memory buffer with LiberFS formatted onto
it: about fifty lines, and everything already tested. Measuring LiberFS is what settled it:

| LiberFS module | lines | needed in RAM? |
| --- | --- | --- |
| `blkalloc` | 970 | no - the heap allocates |
| `txn` | 810 | no - there is no crash to recover from |
| `lib` | 796 | mostly no - superblock, on-disk structures, checksums |
| `dir` | 543 | the idea, but a map rather than a B+tree |
| `fsops` | 511 | the idea, simpler |
| `snapshot` | 329 | no |
| `fsck` | 220 | no - it is rebuilt every boot |
| `inode` | 167 | the idea, simpler |

Reusing it would carry a block allocator, a transaction log and a checker that protect against a
medium that is not there, in order to reach data the CPU can already address. The "free, already
tested" argument is also thinner than it looks: much of LiberFS's 2495 lines of tests cover
durability, which is not a property RAM has.

The other difference is what a size limit means. A RAM DISK reserves its whole size whether or
not anything is stored in it. A memory filesystem holds only what is in it, so a limit is a CAP
rather than a reservation - and that distinction is exactly what the two policies below make
selectable instead of assumed.

## The two policies

`vol://ram` and `vol://tmp` are not two filesystems. Files, directories, path resolution and
every operation are identical. The only difference is WHEN the memory is charged.

| | `Reserved` (`vol://ram`) | `Capped` (`vol://tmp`) |
| --- | --- | --- |
| charged | at mount | at write |
| guarantees | the space is there | the space may be there |
| when memory is short | the MOUNT fails | the WRITE fails |
| unused space | held | available to everything else |
| suits | state that must not fail because something else took the memory | scratch, where waste is worse than an occasional refusal |

A reserved volume holds a buffer of exactly its unused capacity, resynced after every mutation,
so `used + reserved` is always the capacity and never more.

Two properties make that guarantee real rather than nominal, and both are easy to get wrong:

- **A write releases before it allocates.** Allocating the file first would leave the new bytes
  and the reservation outstanding at the same time, so a volume at its capacity would need twice
  its capacity to write into itself - failing for memory it was sitting on, which is exactly the
  failure a reservation exists to prevent. If the allocation is refused anyway, the release is
  undone, so a failed write leaves the volume holding what it held before.
- **Taking memory back is best effort.** After a file shrinks or is deleted the reservation
  regrows with `try_reserve_exact`; `Vec::resize` would ABORT on failure, which in a storage
  service is a crash where a degraded guarantee would do. If less comes back than was released,
  the volume holds less than its capacity and `reserved_bytes()` says so.
- **Every refusal restores the invariant, including the late ones.** A write releases before it
  allocates, so a refusal AFTER that point - an absent parent, a file used as a path component -
  has to give the bytes back on the way out. Returning through `?` there walks past the resync
  and leaves the volume permanently short with nothing stored to account for it.

The invariant is worth stating on its own because it is what the reserved policy IS, and because
every one of the defects found in this filesystem was a violation of it: **`used + reserved`
always equals the capacity.** A test asserts it across six different ways for an operation to be
refused, which is a stronger check than one test per known bug - it caught a defect that had not
been thought of.

Both policies enforce the same capacity through the same check. Only the moment of charging
differs, which is why they share an implementation.

## Bounds

Every limit refuses; none truncates. A filesystem that can exhaust the kernel heap is a denial
of service whatever it is called.

| bound | value | refused with |
| --- | --- | --- |
| `MAX_FILE_BYTES` | 64 MiB | `TooLong` |
| `MAX_ENTRIES` | 4096 nodes | `NoSpace` |
| `MAX_NAME_BYTES` | 255 per segment | `TooLong` |
| `MAX_PATH_DEPTH` | 16 segments | `TooLong` |

The entry count includes the root directory, so 4095 are usable. Path depth bounds the recursion
in the two tree walks (`bytes()` and `count()`), so neither can be driven into a deep-recursion
fault by a crafted path.

## Paths

A path is a byte string of `/`-separated segments and must be UTF-8.

- Leading, trailing and repeated separators are ignored: `/a/b/`, `a//b` and `a/b` name the same
  file. Callers in this tree produce all three.
- `.` and `..` are REFUSED, not resolved. This filesystem has no working directory and no
  outside, and accepting `..` would be the one way to name something beyond the volume.
- A name that is not UTF-8 is refused rather than stored under a mangled key.
- An empty path names the root, which can be listed but not written or removed.

## Operations

The nine methods of the storage service's `FileSystem` trait, all supported - LiberMemFS and
LiberFS are the only writable backends.

| operation | notes |
| --- | --- |
| `read_file` | a directory answers `IsDir` |
| `write_file` | creates or replaces whole; a parent that does not exist is `NotFound`, not created |
| `list_entries` | an empty path lists the root; a file answers `NotDir` |
| `mkdir` | one level; an existing name is `Exists` |
| `remove` | files only; a directory answers `IsDir` |
| `rmdir` | empty directories only; a populated one is `NotEmpty` |
| `capacity` / `status` | capacity, free bytes, never read-only, filesystem name `libermemfs` |

Three behaviours are worth stating because they are decisions rather than consequences:

**Replacing subtracts before it adds.** The capacity check subtracts the existing file's size
first, so rewriting a file on a full volume succeeds instead of demanding room for both copies.

**A failed write changes nothing.** The bytes are allocated and filled BEFORE the directory
entry is replaced. An earlier version inserted an empty file first and filled it afterwards,
which destroyed the previous contents whenever the allocation failed.

**`rmdir` never recurses.** Removing a tree by naming its root is a different operation, and
doing it silently is how a caller loses more than it meant to.

## What it deliberately does not do

- **No persistence.** A volume is empty after a restart. That is the property that distinguishes
  it from every other volume in the system, and there is a test for it precisely because a
  reader would otherwise assume it was broken.
- **No timestamps.** Files report zero times. Nothing here outlives the boot that made it, so a
  timestamp would describe an age with no meaning to a caller.
- **No snapshots, copy-on-write or checksums.** All three defend against a medium that can lose
  or corrupt what was written.
- **No overlay or union mount.** Composing a read-only volume with a writable one is a separate
  idea, needed for a live system booted from read-only media, and belongs with that work.
- **No swap.** When the memory is gone, the write is refused.

## Interfaces

- `LiberMemFs::mount(policy, capacity)` - an empty volume. A reserved mount fails with `NoSpace`
  when the capacity cannot be taken; a capped mount always succeeds.
- The storage service wraps it in a `MemFs` adapter implementing `FileSystem`, exactly as
  `IsoFs` and `UdfFs` wrap their backends.
- Bootstrap tags carry a decimal byte count rather than a block-service handle, because there is
  no block device to hand over: `RAMVOL<bytes>` and `TMPVOL<bytes>`.
- Both volumes are granted through the same `volumes` capability bundle as the disk-backed ones,
  under the tags `RAM` and `TMP`.

## Errors

Shared with every other backend through `fscore::FsError`, so the storage service maps one error
type at its boundary rather than one per backend. `NotFound`, `NoSpace`, `TooLong`, `BadName`,
`IsDir`, `NotDir`, `NotEmpty`, `Exists` and `Invalid` are used; `Corrupt`, `Io` and `ReadOnly`
are not - the first two describe a medium and the third a mount mode this filesystem never has.
