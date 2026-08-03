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
- `no_std` + `alloc`, ~700 lines, 32 unit tests. It implements NEITHER `fscore::BlockDevice` -
  there is no block device under it - nor the storage service's `FileSystem` trait: the service's
  `MemFs` adapter does that, exactly as `IsoFs` and `UdfFs` wrap their backends.
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
| unused space | held | not taken in the first place |
| suits | state that must not fail because something else took the memory | scratch, where waste is worse than an occasional refusal |

The capacity bounds the volume's FOOTPRINT - file data plus the names holding it - not the data
alone. A name is memory the caller asked for, and counting only contents left a hole: a volume
could be filled with long names storing nothing and end up a megabyte over its capacity, a
quarter of a 4 MiB reserved volume. `mkdir` was the sharper case, having had no capacity check at
all, because a directory stores no data.

`used()` therefore reports file data, which is what the word means to a caller and to every other
backend, while `footprint()` reports what the capacity actually bounds and `free()` subtracts both.

`free()` reports what the volume can actually promise: the capacity rule for a capped volume, and
for a reserved one whichever is smaller of that and what the reservation still holds - so a
reservation that fell short shows up as less free space wherever free space is reported, including
through the service's `status`. It is exact for rewriting a file that exists, whose name is already
paid for, and one name short for creating a new one, which is charged for its name on top. That is the contract, not a
rounding error: the length of a name that does not exist yet is not knowable, so a caller creating
an entry should expect to need `free()` minus the name. Reporting `capacity - used` instead would
be worse again - it would promise room that every name already stored has taken.

The footprint counts a file's ALLOCATION rather than its contents, because a cleared vector keeps
its buffer: a file shrunk from 60 KiB to nothing still owns 60 KiB, and counting what it stored
would report that memory as free while the file had it. `used()` still reports contents; the
capacity, `free()` and the reservation all work from what is held.

What the footprint still does NOT count is the per-entry overhead: the map node holding each
entry, and the vector header inside it - on the order of fifty to a hundred bytes each, none of it
charged. This is left uncharged deliberately rather than overlooked. A name is caller-controlled
and unbounded per entry, which is why not charging it was a hole worth closing; per-entry overhead
is a fixed implementation-defined cost already bounded by `MAX_ENTRIES`, so the worst case is a few
hundred kilobytes whatever the capacity. Charging it would mean writing an allocator-internals
guess into the capacity semantics - a 20-byte volume that can hold nothing - and the guess would be
wrong on the next allocator. A caller sizing a reserved volume should treat the capacity as
bounding stored bytes and names, not as the volume's total cost to the heap.

A reserved volume holds a buffer of exactly its unused capacity, resynced after every mutation,
so while the reservation is intact `footprint + reserved` is the capacity - and after a regrow has fallen short it is less, which `reservation_intact()` reports. Because names count, that
includes `mkdir` and `rmdir`: creating a directory takes its name's bytes out of the reservation
and removing one puts them back, which is easy to miss because a directory stores no data.

Two properties make that guarantee real rather than nominal, and both are easy to get wrong:

- **A write releases before it allocates, and releases all of it.** Allocating the file first
  would leave the new bytes and the reservation outstanding at the same time, so a volume at its
  capacity would need twice its capacity to write into itself - failing for memory it was sitting
  on, which is exactly the failure a reservation exists to prevent. Releasing only the difference
  would rest on the reservation being perfectly in step, which it is not after a regrow has
  fallen short; releasing all of it makes how much is held irrelevant to whether the write can
  proceed. If the allocation is refused anyway, the resync puts back what the volume should hold.
The reservation is held as several CHUNKS rather than one block. A regrow adds to what is there
instead of replacing it, so it can never end below what it already had - releasing first and then
hunting could leave a volume holding 32 MiB, asked for 33, and ending with 16. Chunks also make
fragmentation far less likely to defeat it: a heap with no 33 MiB block very often has two of 16.
Each chunk records the size it stands for, because its buffer's length is zero by design and only
its capacity is taken.

- **The reservation is dropped and reallocated, never resized - and never written to.**
  `shrink_to_fit` reallocates and COPIES what it keeps, so resizing would memcpy what remains of
  the reservation on every single write - megabytes per write on a volume of any size, to preserve
  bytes nothing ever reads. Dropping first also keeps the old block from being outstanding
  alongside the new one. Filling the new block is the same mistake wearing a different hat: it
  costs a memset of the whole remaining reservation per mutation and buys nothing, because this
  kernel commits and charges physical frames when a memory object is created rather than when a
  page is first touched. Owning the allocation IS the reservation; the bytes in it are decoration.
  Only the allocation's capacity is taken, and the size held is tracked in its own field rather
  than read back from the vector's length.
- **Taking memory back is best effort.** After a file shrinks or is deleted the reservation
  regrows with `try_reserve_exact`; `Vec::resize` would ABORT on failure, which in a storage
  service is a crash where a degraded guarantee would do. If less comes back than was released,
  the volume holds less than its capacity and `reserved_bytes()` says so.
- **Every refusal restores the invariant, including the late ones.** A write releases before it
  allocates, so a refusal AFTER that point - an absent parent, a file used as a path component -
  has to give the bytes back on the way out. Returning through `?` there walks past the resync
  and leaves the volume permanently short with nothing stored to account for it.

The invariant is worth stating on its own because it is what the reserved policy IS, and because
every one of the accounting defects found in this filesystem was a violation of it:
**`footprint + reserved` equals the capacity while `reservation_intact()` holds.** A test asserts it across six different ways
for an operation to be refused, which is a stronger check than one test per known bug - it caught
a defect that had not been thought of. It has one limit worth knowing before trusting it: it drove
only file operations, so when names began to count it went on passing while every directory
operation broke the invariant. An invariant test is only as strong as the set of operations it
runs through, and a second test now covers the directory ones.

Both policies enforce the same capacity through the same check. Only the moment of charging
differs, which is why they share an implementation.

One thing the capped policy does NOT do is give memory back to the system. The userspace heap
grows by mapping a region and never unmaps one, so freeing a file returns its bytes to the storage
service's own free list - reusable by that process, including by the other volumes it serves - but
the Domain stays charged at the high-water mark. A capped volume that has once been full costs the
system what its files still hold, permanently - and because a file keeps the allocation it grew
to, a grow-and-shrink cycle across several files can leave it holding multiples of what it
stores. The capacity check counts what is held rather than what is stored, so the volume refuses
rather than exceeding itself, but the memory is not returned until the files are removed.

So the capped policy's advantage is that it never takes what it never needs, not that it returns
what it took, and that distinction matters when sizing one: a `vol://tmp` that briefly peaks is a
permanent cost, while one that stays small is nearly free. Returning memory would require the heap
to unmap regions, which is a runtime change rather than a filesystem one.

## What an operation costs

Every mutation recomputes the footprint by walking the whole tree, and does it three times: once
for the entry count, once for the capacity check and once in the reservation resync. So a mutation
is O(entries), not O(path). Measured on the host, 2000 rewrites into a 2000-entry volume take 79 ms
optimized and 1.03 s unoptimized - which is why the storage service pins `opt-level = 2` for this
crate in dev builds, as it does for the filesystems with checksums.

Keeping a running total instead would make it O(depth), and it is deliberately not done. The
footprint and the reservation are two copies of one fact, and every accounting defect found in this
filesystem - five of nine - was those copies drifting apart. Deriving the number from the tree every
time means it cannot be stale; caching it would add a third copy to keep in step, on the exact path
that has gone wrong most often. `MAX_ENTRIES` bounds the walk at 4096 nodes, so the cost has a
ceiling.

## Bounds

Every limit refuses; none truncates. A filesystem that can exhaust the kernel heap is a denial
of service whatever it is called.

| bound | value | refused with |
| --- | --- | --- |
| `MAX_FILE_BYTES` | 64 MiB | `TooLong` |
| `MAX_ENTRIES` | 4096 nodes | `NoSpace` |
| the capacity, over data AND names | per mount | `NoSpace` |
| `MAX_NAME_BYTES` | 255 per segment | `TooLong` |
| `MAX_PATH_DEPTH` | 16 segments | `TooLong` |

The entry count includes the root directory, so 4095 are usable. Path depth bounds the recursion
in the three tree walks (`bytes()`, `names()` and `count()`), so none can be driven into a
deep-recursion fault by a crafted path: a directory at depth 16 can be created, and anything under
it needs a 17-segment path, which is refused.

The depth limit is enforced as the path is split rather than after. Checking at the end returns the
same error but only once a path of ten thousand segments has been parsed into ten thousand entries
- an allocation the caller sizes, charged to no volume, on every operation including reads against
a volume with nothing left in it. A limit meant to bound work has to be applied while the work is
being done.

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

## What `Reserved` does and does not promise

It takes the whole capacity at mount, so a mount fails when the memory is not available and
nothing else in the process can take it afterwards. That much is a guarantee.

What it cannot promise is that the memory comes BACK. The regrow is best effort: after deletes
fragment the heap, a chunk of the size wanted may not be there and the volume then holds less than
its capacity. Holding it in several chunks makes that far less likely than one block would - a heap
with no 33 MiB block very often has two of 16 - but it does not make it impossible.
`reservation_intact()` says whether it still does, and `free()` reports the smaller figure, so a
degraded volume is visible rather than merely documented. A guarantee that cannot degrade needs an
arena the volume allocates everything from - files, names and nodes - and hands nothing back to the
global heap. That is a different design, and it is recorded as open work rather than implied here.

Allocation is fallible wherever a caller can be told: the file bytes, the reservation, a read, a
listing and the name of a new entry all answer `NoSpace` instead of aborting, and path parsing
allocates nothing at all. This became true of the RUNNING system only when `rt`'s allocator was
changed to return null on exhaustion - it computed the null and then called an exit handler with
it, so every `try_reserve` in userspace was unreachable and the fallible paths here were only ever
exercised against a test allocator this crate wrote for itself.

Nothing here is infallible any more. A directory is a `Vec<(String, Node)>` kept sorted rather
than a `BTreeMap`, so the slot for a new entry is reserved with `try_reserve` before anything moves
into it - `BTreeMap` had no fallible insert and was the last allocation that could end the process
after every check had passed.

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
