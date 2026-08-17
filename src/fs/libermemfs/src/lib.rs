//! LiberMemFS - a writable filesystem held in memory.
//!
//! Every other backend in this tree reads a medium: LiberFS a disk, FAT removable media,
//! ISO9660 and UDF optical images. This one has no medium. A file IS a heap allocation and a
//! directory IS a list of names, so there is no block layer, no on-disk format, no allocator
//! and nothing to check on mount - and nothing survives a reboot.
//!
//! That absence is the design. A block allocator, a transaction log and a checksum protect
//! against a medium that can lose or corrupt what was written; RAM either holds the whole
//! program's memory correctly or the program is already gone. Carrying those mechanisms here
//! would cost work on every write to defend against a failure mode that does not exist.
//!
//! Two allocation policies share this one implementation, because they differ only in WHEN the
//! memory is charged, never in what a file or a directory is:
//!
//! - `Policy::Reserved` takes its whole capacity at mount, so mounting fails when the memory is
//!   not available and nothing else in the process can take it afterwards. What it does NOT
//!   promise is that the memory can always be taken back: the reservation is held as a bounded
//!   list of chunks (see `MAX_RESERVATION_CHUNKS`) rather than one contiguous
//!   allocation, and after deletes fragment the heap the total free memory can cover it while no
//!   single block does. When that happens the volume holds less than its capacity, says so
//!   through `reserved_bytes()`, and reports it to clients as a volume no longer covering its
//!   own free space. A guarantee that cannot degrade needs an arena the volume allocates
//!   everything from, which is a different design and is recorded as such in M0139d.
//! - `Policy::Capped` takes memory as files are written and refuses past the limit: nothing is
//!   taken in advance, and the limit is a ceiling rather than a reservation. It is NOT true that
//!   nothing unused is held - a file keeps the allocation it grew to until it is removed, and the
//!   capacity counts that, so a volume can be full of memory it is no longer storing anything in.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fscore::FsError;

// Bounds. Every other bounded resource in this tree refuses rather than truncates, and a
// filesystem that can exhaust the kernel heap is a denial of service whatever it is called.
pub const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 4096;

// What one directory slot costs: the name's `String` header plus the `Node` beside it. Measured
// from the types rather than written down, so it cannot drift from what a slot actually is.
pub const SLOT_BYTES: usize = core::mem::size_of::<(String, Node)>();

// Tables at or below this many slots are left alone when they empty out - moving a handful of
// entries to save a few hundred bytes costs more than it returns.
const MIN_SLOTS: usize = 8;

// The ceiling on directory tables, live and retained together. It is a SEPARATE budget from the
// volume's capacity, deliberately: the capacity bounds what a caller stored, and a four-slot table
// under a twelve-byte volume would make every small volume unusable while measuring nothing the
// caller controls. What this bounds is the thing the caller CAN drive without limit - retained
// slots - and `shrink_children` keeps a table within a factor of four of its contents, so reaching
// this at all means something has gone wrong rather than merely gone large.
pub const MAX_METADATA_BYTES: usize = 4 * MAX_ENTRIES * SLOT_BYTES;
pub const MAX_NAME_BYTES: usize = 255;
pub const MAX_PATH_DEPTH: usize = 16;
// The longest path that can name anything: every segment at its limit, with a separator between
// them. Checked before the path is walked, so the depth limit bounds the WORK and not only the
// result.
// Below this a partial reservation is not worth another attempt: the remainder cannot cover a
// meaningful write, and each attempt is a refused allocation.
const MIN_RESERVATION_STEP: usize = 4096;

// A ceiling on how many pieces a reservation may be split into. A badly fragmented heap could
// otherwise leave a volume holding thousands of small chunks, whose own vector and allocator
// headers are memory the capacity does not count.
const MAX_RESERVATION_CHUNKS: usize = 64;

pub const MAX_PATH_BYTES: usize = MAX_PATH_DEPTH * (MAX_NAME_BYTES + 1);

// One piece of a reserved volume's held memory.
//
// The size is recorded rather than read off the buffer, because the buffer's LENGTH is zero on
// purpose - only its capacity is taken, and nothing is ever written into it. Subtracting
// `len()` when giving a chunk back therefore subtracted nothing: the reservation emptied itself
// while the accounting went on claiming every byte was still held.
struct Chunk {
	// Held for its capacity alone. Never read, never written.
	allocation: Vec<u8>,
	// What the chunk actually OWNS, which is `allocation.capacity()` and not what was asked for.
	//
	// It recorded the REQUEST. `try_reserve_exact` promises at least that and may give more, so on
	// an allocator that rounds, `reserved_bytes` under-reported what the volume was holding - and
	// with up to `MAX_RESERVATION_CHUNKS` of them the slack accumulates. A reserved volume could
	// then report `footprint + reserved == capacity` while physically holding more, which makes the
	// configured capacity stop being a truthful bound on the heap.
	bytes: usize,
}

// A parsed path: bounded by the depth limit, so it lives on the stack.
type Segments<'a> = [&'a str; MAX_PATH_DEPTH];

// When a volume's memory is charged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
	// Charged at mount: the capacity is held whether or not it is used.
	Reserved,
	// Charged at write, and released only when a file is removed - a shrunken file keeps its
	// allocation, which the capacity counts.
	Capped,
}

// A directory's children: sorted by name, so lookup is a binary search and listing is already in
// order, and backed by a `Vec` so the space for an entry can be reserved FALLIBLY.
//
// `BTreeMap` was the obvious choice and the last thing here that could end the process: it has no
// fallible insert, so a volume that had checked its capacity, reserved its data and reserved its
// name could still die allocating the node to put them in. Everything else in this filesystem
// answers `NoSpace`; this was the one place that answered by exiting.
type Children = Vec<(String, Node)>;

// One entry in a directory. Files own their bytes; directories own their children.
enum Node {
	File(Vec<u8>),
	// A file an open stream is filling, holding whatever the destination held when the stream
	// opened: empty for a new file, the previous contents for a replacement.
	//
	// THE CLAIM HAS TO BE IN THE TREE, not only in `Pending`. `stream_begin` inserted an ordinary
	// `Node::File` and recorded `placeholder: true` beside it - so nothing another caller could
	// reach knew the entry was spoken for, `write_file` and `remove` operated on it as an ordinary
	// file, and `stream_abort` then removed the path unconditionally. Two clients and no timing
	// subtlety:
	//
	//     A: stream_begin("x")           the claim
	//     B: write_file("x", important)  Ok - B is told the write succeeded
	//     A: stream_abort()              remove("x") - B's file is gone
	//
	// A completed write, reported as completed, destroyed by an unrelated abort. So the node
	// carries the claim, every mutator refuses it, and the abort restores exactly what the stream
	// found: nothing for a new destination, the previous bytes for a replaced one.
	//
	// Reads are NOT refused. A replacement stream must leave the file exactly as it was until it
	// commits, which is this milestone's own rule - so reading a claimed entry answers the bytes it
	// is holding, which are the file's current contents.
	Claimed(Vec<u8>),
	Directory(Children),
}

impl Node {
	// Is a stream holding this entry? Every mutator asks, because a claim nothing enforces is not a
	// claim.
	fn is_claimed(&self) -> bool {
		matches!(self, Node::Claimed(_))
	}
}

// Find `name` among sorted children.
fn find<'a>(children: &'a Children, name: &str) -> Option<&'a Node> {
	children.binary_search_by(|(key, _)| key.as_str().cmp(name)).ok().map(|at| &children[at].1)
}

fn find_mut<'a>(children: &'a mut Children, name: &str) -> Option<&'a mut Node> {
	let at = children.binary_search_by(|(key, _)| key.as_str().cmp(name)).ok()?;
	Some(&mut children[at].1)
}

// Insert or replace, reserving the slot before anything is moved into it.
// What `Vec::try_reserve(1)` grows a table to, when the table is full.
//
// `RawVec::grow_amortized` takes `max(capacity * 2, required)` and then raises it to a floor that
// depends on the element size - eight slots for a one-byte element, four for anything up to a
// kilobyte, one above that - and stores exactly that number as the new `capacity()`. So this is not
// an estimate of what the ALLOCATOR gave back; it is what `Vec` will ask for and then report, which
// is the number `metadata_bytes()` reads.
//
// It is predicted rather than measured because the ceiling has to be decided BEFORE the allocation:
// a table that grew past the limit cannot be put back, since `shrink_children` keeps a table within
// a factor of four of its contents and would leave the growth in place. `a_full_table_grows_the_way
// _the_metadata_ceiling_predicts` in the test module checks the prediction against a real `Vec`, so
// a change in the standard library's growth policy is a failing test rather than a silent ceiling
// that no longer holds.
fn grown_capacity(capacity: usize, required: usize) -> usize {
	let floor = if SLOT_BYTES == 1 {
		8
	} else if SLOT_BYTES <= 1024 {
		4
	} else {
		1
	};
	capacity.saturating_mul(2).max(required).max(floor)
}

// Would one more entry in a table of `capacity` slots holding `len` of them keep the volume's
// directory tables within `ceiling`, given that they hold `metadata` bytes right now?
//
// A PURE FUNCTION OF FOUR NUMBERS, so the rule can be exercised AT the ceiling - which a volume
// cannot be driven to. `MAX_METADATA_BYTES` is four times the entire node budget and
// `shrink_children` keeps every table within a factor of four of its contents, so building that
// state through the public API would need more entries than a volume is allowed to hold. The rule
// is the thing worth testing; the volume is what applies it. See
// `the_metadata_ceiling_is_decided_against_the_table_the_entry_grows`.
fn metadata_admits_a_slot(metadata: usize, capacity: usize, len: usize, ceiling: usize) -> bool {
	// An insert into a table with a spare slot allocates nothing, so no metadata ceiling can have
	// anything to say about it. The old rule refused this at exactly the boundary, because the
	// question it asked was about the volume rather than about the work.
	if len < capacity {
		return true;
	}
	let grown = grown_capacity(capacity, len + 1);
	metadata.saturating_sub(capacity * SLOT_BYTES).saturating_add(grown * SLOT_BYTES) <= ceiling
}

fn insert(children: &mut Children, name: String, node: Node) -> Result<(), FsError> {
	match children.binary_search_by(|(key, _)| key.as_str().cmp(name.as_str())) {
		Ok(at) => {
			children[at].1 = node;
			Ok(())
		}
		Err(at) => {
			children.try_reserve(1).map_err(|_| FsError::NoMemory)?;
			children.insert(at, (name, node));
			Ok(())
		}
	}
}

fn remove(children: &mut Children, name: &str) -> Option<Node> {
	let node = take(children, name)?;
	shrink_children(children);
	Some(node)
}

// The same, WITHOUT the shrink.
//
// `rename` reserves one slot in the destination's table specifically so the insert cannot fail after
// the source is gone. For a rename within one directory those are the same table, and `remove`'s
// shrink replaces it with one sized exactly to its contents - giving the reserved slot back, so the
// insert had to allocate again and a failure there dropped the node with it. The ABI promises rename
// is atomic within a volume. The window is narrow, since `shrink_children` only acts at or below a
// quarter occupancy, and it is exactly the case the reservation was written to make impossible.
fn take(children: &mut Children, name: &str) -> Option<Node> {
	let at = children.binary_search_by(|(key, _)| key.as_str().cmp(name)).ok()?;
	Some(children.remove(at).1)
}

// Give a directory's table back when it is mostly empty.
//
// `Vec::remove` never shrinks, so without this a directory that held four thousand entries and now
// holds none still owns the backing store for four thousand slots - and nothing in the volume's
// accounting could see it, because `allocated()` and `names()` only ever walked the entries that
// EXIST. Fill a directory, empty it, leave it, make another: at 56 bytes a slot, the sequence the
// audit describes retains 447 MB through the ordinary public `remove`.
//
// MAX_ENTRIES bounds live nodes, which is what the documentation's "a few hundred kilobytes
// whatever the capacity" is true of. It says nothing about capacity that was kept, and the
// difference was three orders of magnitude.
//
// Two shrinks, and the difference between them matters:
//
//   * empty: assign a fresh `Vec`, which allocates nothing and CANNOT fail. Free in both senses.
//   * mostly empty: build a right-sized table with `try_reserve_exact` and move into it. Fallible,
//     and a failure simply keeps the oversized table - refusing a `remove` because the volume could
//     not afford to shrink would be absurd.
//
// `shrink_to_fit` does neither: it aborts the process on allocation failure, which is the one thing
// this crate is written not to do.
fn shrink_children(children: &mut Children) {
	if children.is_empty() {
		*children = Children::new();
		return;
	}
	// A small table is not worth moving, and a table within a factor of four of its contents is the
	// slack a growing directory needs anyway.
	if children.capacity() <= MIN_SLOTS || children.len() * 4 > children.capacity() {
		return;
	}
	// A GUARANTEE, NOT AN ACCIDENT: a table this function has shrunk has room for at least one more
	// entry without allocating.
	//
	// Several call sites lean on it - `rename` removes from one table and inserts into another, and
	// the shape that made the old defect unreachable was exactly this - and until now it held by the
	// way `MIN_SLOTS`, the quarter-occupancy window and an EXACT-fit resize happened to interact.
	// Exact fit does not actually provide it: a table of a hundred entries shrunk to a capacity of a
	// hundred has no room at all, and the next insert allocates. What made it look true was that the
	// callers reaching this path had small tables.
	//
	// So the headroom is asked for rather than hoped for. One slot is what the property needs and
	// all it needs: the guarantee is about the insert that FOLLOWS a shrink, not about a run of
	// them, and a run has to be able to allocate anyway.
	let want = children.len().saturating_add(1).max(MIN_SLOTS);
	let mut smaller = Children::new();
	if smaller.try_reserve_exact(want).is_err() {
		return;
	}
	smaller.extend(children.drain(..));
	debug_assert!(smaller.capacity() > smaller.len(), "a shrunk table must have room for the insert that follows it");
	*children = smaller;
}

impl Node {
	fn is_dir(&self) -> bool {
		matches!(self, Node::Directory(_))
	}

	// The bytes this node and everything under it holds. Directories are counted by their
	// contents only: the name table is bookkeeping, not stored data, and charging for it would
	// make the reported usage depend on how deeply a caller happened to nest its files.
	fn bytes(&self) -> usize {
		match self {
			// A claimed entry counts as what it holds. It is a file another caller can read and the
			// stream may yet restore, so leaving it out would report memory the volume is holding
			// as free - the accounting error the retained-table finding was about, in miniature.
			Node::File(data) | Node::Claimed(data) => data.len(),
			Node::Directory(children) => children.iter().map(|(_, node)| node.bytes()).sum(),
		}
	}

	// The memory this node's subtree actually HOLDS, which is not what it stores: a vector keeps
	// its allocation when it is cleared, so a file shrunk from 60 KiB to nothing still owns 60 KiB.
	//
	// Counting `len` made the volume report that memory as free while the file still had it -
	// which let a capped volume be pushed to multiples of its capacity by a grow-and-shrink cycle,
	// and let a reserved volume hold its files' historical capacity AND a full reservation at
	// once, reporting itself intact throughout. Every accounting question - the capacity check,
	// `free`, the reservation target - is answered from this; only `used` reports `len`, because
	// that is what the word means to a caller.
	fn allocated(&self) -> usize {
		match self {
			Node::File(data) | Node::Claimed(data) => data.capacity(),
			Node::Directory(children) => children.iter().map(|(_, node)| node.allocated()).sum(),
		}
	}

	// The bytes this node's subtree spends on NAMES. A name is memory the caller caused to be
	// allocated, so it counts against the capacity: without it a volume could be filled with
	// 4095 maximum-length names and exceed its capacity by a megabyte - a quarter of a 4 MiB
	// reserved volume, which would make "reserved" mean nothing.
	//
	// A node does not count its own name; the parent holding the key does.
	//
	// `capacity()` RATHER THAN `len()`, for the same reason `allocated` counts a file's capacity.
	// Every name is built with `try_reserve_exact`, which promises AT LEAST what was asked for and
	// may give more - so an allocator that rounds leaves the volume physically holding more than
	// `footprint` reports, across up to `MAX_ENTRIES` names. `capacity` is what the `String`
	// actually owns, which is the question every accounting caller is really asking.
	fn names(&self) -> usize {
		match self {
			Node::File(_) | Node::Claimed(_) => 0,
			Node::Directory(children) => children.iter().map(|(name, node)| name.capacity() + node.names()).sum(),
		}
	}

	// The bytes this subtree's directory TABLES hold, live slots and retained ones alike. Kept
	// apart from `allocated()` because it is bounded separately - see MAX_METADATA_BYTES - but
	// answered from the same walk, so the two cannot disagree about the shape of the tree.
	fn table_bytes(&self) -> usize {
		match self {
			Node::File(_) | Node::Claimed(_) => 0,
			Node::Directory(children) => children.capacity() * SLOT_BYTES + children.iter().map(|(_, node)| node.table_bytes()).sum::<usize>(),
		}
	}

	fn count(&self) -> usize {
		match self {
			Node::File(_) | Node::Claimed(_) => 1,
			Node::Directory(children) => 1 + children.iter().map(|(_, node)| node.count()).sum::<usize>(),
		}
	}
}

// One directory entry as a caller sees it.
pub struct Entry {
	pub name: String,
	pub size: u64,
	pub is_dir: bool,
}

pub struct LiberMemFs {
	root: Node,
	policy: Policy,
	capacity: usize,
	// Held only by a reserved volume: the memory taken at mount so a later write cannot fail for
	// want of it. It is never read - owning it IS the reservation - and it tracks the UNUSED part
	// of the capacity, released as files grow and taken back as they shrink, so the volume's
	// footprint is what it stores plus what it still holds rather than both at once.
	//
	// Only its CAPACITY is taken; its length stays zero and its bytes are never written. This
	// kernel allocates physical frames when a memory object is created, not when a page is first
	// touched (`MemoryObject::create_in` charges the Domain and calls `allocate_pages` up front),
	// so the frames behind the heap are already committed and already charged. Writing zeros into
	// them holds nothing that owning the allocation does not already hold.
	reservation: Vec<Chunk>,
	// What `reservation` holds. Tracked rather than read back from the vector, whose length is
	// deliberately zero and whose capacity may legitimately exceed what was asked for.
	reserved: usize,
	// A stream being received into this volume, before it becomes a file.
	//
	// It lives HERE rather than on the caller's heap because the accounting has to see it. A
	// storage service accumulating a stream in its own `Vec` consumes memory this volume knows
	// nothing about, so `free()` keeps reporting room that is already spent and a reserved volume's
	// guarantee - the whole point of the policy - does not cover the operation that needs it most.
	// The identical write through the ordinary path succeeded while the stream ran the heap out.
	pending: Option<Pending>,
}

// A stream in progress: where it is going, and what has arrived so far.
struct Pending {
	path: Vec<u8>,
	data: Vec<u8>,
	// THE NAME IS NOT HELD HERE, and this block used to say it was - describing a `name` field
	// "allocated at begin and moved into the tree at commit", with a `None for a replace` state,
	// beside a struct whose fields are `path`, `data`, `placeholder` and `offered`. The comment
	// outlived two revisions of the design it was written for.
	//
	// The problem it names is real and is still solved, by a different mechanism. A stream is
	// allowed roughly `capacity - path.len()`, and the pending state then holds the path plus that
	// data - the whole capacity - so `stream_commit` allocating a fresh `String` for the name at
	// the very end meant a transfer could accept one hundred per cent of what it was promised and
	// then fail on a few bytes of metadata, after the sender had done all the work.
	//
	// What `stream_begin` does now is stronger than holding the name: it puts a `Node::Claimed`
	// entry in the tree, which pays for the name AND the directory slot up front and takes the path
	// out of anyone else's reach for the stream's lifetime. So the name is in the tree rather than
	// in this struct, and `footprint()` counts it there - which is why that function's comment says
	// counting it here as well would be double-counting.
	//
	// Whether `stream_begin` created that entry rather than claiming a file that was already there.
	// An abort takes a created one away and puts a claimed one back; a commit writes over either.
	placeholder: bool,
	// How many bytes the last `stream_spare` handed out and nobody has accounted for yet.
	//
	// Without it the pair is not a protocol, it is two functions that happen to be called in the
	// right order: `stream_spare` resizes the pending data to its new total - filling with zeros -
	// before handing back the slice, and `stream_advance` truncates using numbers the CALLER
	// supplies. StorageService uses them correctly, and the API is public, so `stream_spare(4096)`
	// followed by `stream_commit()` stored four kilobytes of zeros nobody wrote, and an `offered`
	// that did not match the last offer truncated data that was already valid.
	offered: Option<usize>,
}

impl LiberMemFs {
	// Mount an empty volume. A reserved volume takes its capacity here, so this is where it
	// fails when the memory is not available; a capped volume always mounts.
	pub fn mount(policy: Policy, capacity: usize) -> Result<LiberMemFs, FsError> {
		let mut reservation = Vec::new();
		let mut reserved = 0;
		if policy == Policy::Reserved {
			// `try_reserve` rather than a plain allocation: running out here must be an error
			// the caller can report, not an abort inside the storage service.
			//
			// AND THE ERROR IS `NoMemory`, WHICH IT WAS NOT. Every failed reservation in this file
			// answered `NoSpace` - 52 of them, against zero uses of `NoMemory` - even though
			// `fs-core` grew `NoMemory` precisely because the two drive opposite policies: `NoSpace`
			// says delete something or use another volume, `NoMemory` says the machine is under
			// pressure and the same request may well succeed in a moment. `read_file` and
			// `list_entries` were the clearest cases, because they add nothing to the volume and
			// still reported it full. The split is mechanical and it is the whole fix: an explicit
			// capacity check stays `NoSpace`, a refused allocation is `NoMemory`.
			let mut allocation: Vec<u8> = Vec::new();
			allocation.try_reserve_exact(capacity).map_err(|_| FsError::NoMemory)?;
			reservation.try_reserve(1).map_err(|_| FsError::NoMemory)?;
			let held = allocation.capacity();
			reservation.push(Chunk { allocation, bytes: held });
			reserved = held;
		}
		Ok(LiberMemFs { root: Node::Directory(Children::new()), policy, capacity, reservation, reserved, pending: None })
	}

	pub fn policy(&self) -> Policy {
		self.policy
	}

	pub fn capacity(&self) -> u64 {
		self.capacity as u64
	}

	// The file data stored, which is what a caller means by "used" and what every other backend
	// reports. It deliberately excludes names - see `footprint` for what the capacity bounds.
	pub fn used(&self) -> u64 {
		self.root.bytes() as u64
	}

	// The memory this volume has caused to be allocated: file data plus the names holding it.
	// This is what the capacity bounds, so a volume cannot exceed it by filling itself with
	// names instead of contents.
	pub fn footprint(&self) -> u64 {
		// The pending stream counts. It is memory this volume has caused to be allocated, exactly
		// like a file's, and leaving it out is what let the accounting disagree with the heap.
		// The prepared name is no longer counted here and does not need to be: it is IN THE TREE
		// now, as the placeholder entry `stream_begin` inserts, so `self.root.names()` below already
		// has it. Counting it twice would be the accounting error this line exists to prevent, in
		// the other direction.
		let pending = self.pending.as_ref().map_or(0, |p| p.data.capacity() + p.path.capacity());
		(self.root.allocated() + self.root.names() + pending) as u64
	}

	// What the volume's directory tables hold, retained capacity included.
	//
	// This is the number that did not exist, and its absence is the whole finding: `footprint()`,
	// `free()` and `reservation_intact()` all walked live entries only, so a directory that had been
	// filled and emptied reported its retained table as absent - hundreds of megabytes of it, if a
	// caller repeated the trick, all reachable through the ordinary public API.
	pub fn metadata_bytes(&self) -> u64 {
		self.root.table_bytes() as u64
	}

	// The bytes a reserved volume is holding but not yet storing. Zero for a capped volume,
	// which holds nothing it is not using. Exposed because the reservation is the whole
	// difference between the two policies and a test cannot otherwise see it.
	pub fn reserved_bytes(&self) -> u64 {
		self.reserved as u64
	}

	// The most that may be written to `path`, and whether the path can be written at all.
	//
	// Answered for the PATH rather than for the volume, because the two differ in both directions.
	// Reporting the free space refuses a rewrite of a file that already holds the whole volume -
	// the write would reuse its buffer and need nothing new - and it accepts a new file of exactly
	// the free space, whose name then does not fit. A streaming caller has to know this before it
	// accepts a byte.
	// The most a STREAM to this path may carry, which is not the same answer.
	//
	// `writable_len` is right for an ordinary write, which replaces a file's contents in place and
	// may therefore use what that file already holds. A stream cannot: it keeps the old contents
	// until the commit - that is what makes a failed transfer leave the file as it was - and builds
	// the new ones beside them. So the old file's allocation is NOT available to it, and answering
	// as if it were let a stream be admitted by the preflight and refused by its first chunk.
	//
	// The asymmetry is real rather than an accounting slip: an atomic replace needs room for both
	// versions at once. This says so instead of promising otherwise.
	pub fn stream_len(&self, path: &[u8]) -> Result<usize, FsError> {
		let (parts, count) = Self::segments(path)?;
		let parts = &parts[..count];
		if parts.is_empty() {
			return Err(FsError::IsDir);
		}
		let free = self.capacity.saturating_sub(self.footprint() as usize);
		// The WHOLE path, on BOTH arms, because `stream_begin` allocates the pending path for a
		// replace exactly as it does for a create and charges `path.len()` against the capacity
		// either way. The existing-file arm answered `free` and the stream could take `free -
		// path.len()`: on a volume of 1000 bytes where `abc` and its name occupy 903, the preflight
		// promised 97 and the stream accepted 94. Deterministic, not an allocator edge - and the
		// test that pinned the property covered the nested-create case only.
		//
		// The two numbers have to be the same number, and refusing has to be the same refusal:
		// `Ok(0)` here used to mean "no room for another entry", which `writable_len` answers with
		// `Err(NoSpace)` and `stream_begin` answers with `Err(NoSpace)`. Three answers to one
		// question in one file.
		let pending = path.len();
		match self.resolve(parts)? {
			Some(Node::Directory(_)) => Err(FsError::IsDir),
			// A CLAIMED DESTINATION ANSWERS WHAT THE OPERATION ANSWERS. `stream_begin` refuses a
			// claimed path with `Exists`; this reported the space a stream would have, so a caller
			// could ask during another caller's open stream, be told a number, prepare that payload
			// and be refused. Not a data-loss path, and the same class this milestone has closed
			// twice: a preflight answering a question the operation behind it answers differently.
			Some(Node::Claimed(_)) => Err(FsError::Exists),
			Some(Node::File(_)) => {
				if free < pending {
					return Err(FsError::NoSpace);
				}
				Ok((free - pending).min(MAX_FILE_BYTES))
			}
			None => {
				// The path AND the name, because `stream_begin` takes both and holds both: the
				// destination's metadata is prepared up front so a commit cannot fail on it after
				// the whole transfer has been accepted. Whatever begin charges, this must promise.
				let cost = pending + parts.last().map_or(0, |name| name.len());
				if !self.room_for_an_entry(parts) || free < cost {
					return Err(FsError::NoSpace);
				}
				Ok((free - cost).min(MAX_FILE_BYTES))
			}
		}
	}

	pub fn writable_len(&self, path: &[u8]) -> Result<usize, FsError> {
		let (parts, count) = Self::segments(path)?;
		let parts = &parts[..count];
		if parts.is_empty() {
			return Err(FsError::IsDir);
		}
		let free = self.capacity.saturating_sub(self.footprint() as usize);
		match self.resolve(parts)? {
			// A claimed destination is `Exists`, which is what `write_file` answers for it. See
			// `stream_len` for why the two must be the same answer.
			Some(Node::Claimed(_)) => Err(FsError::Exists),
			// A rewrite may use what the file already holds, plus whatever is still free.
			Some(Node::File(existing)) => Ok(existing.capacity().saturating_add(free).min(MAX_FILE_BYTES)),
			Some(Node::Directory(_)) => Err(FsError::IsDir),
			// A new entry pays for its name out of the same free space.
			None => {
				if !self.room_for_an_entry(parts) {
					return Err(FsError::NoSpace);
				}
				let name = parts.last().map_or(0, |name| name.len());
				// `Ok(0)` means an EMPTY FILE CAN BE CREATED HERE, and nothing weaker. With one
				// free byte and a five-byte name it used to answer `Ok(0)`, which StorageService
				// turns into `WritePlan::Allowed { max_len: Some(0) }` - documented as "this path
				// may be written" - and the write then failed with `NoSpace` because the name alone
				// did not fit. A preflight that says yes to something the next call refuses is
				// worse than no preflight.
				if free < name {
					return Err(FsError::NoSpace);
				}
				Ok((free - name).min(MAX_FILE_BYTES))
			}
		}
	}

	// Whether a reserved volume still holds everything it promised.
	//
	// False when a regrow fell short - the memory was there at mount but could not be taken back
	// after a delete, usually because the heap fragmented and no single block covers the
	// reservation any more. The volume keeps working; what it has lost is the promise that a
	// write up to its capacity cannot fail for want of memory. Capped volumes are always intact,
	// having promised nothing.
	//
	// Exposed because the promise is the only thing separating the two policies, and a volume
	// that quietly stopped keeping it is indistinguishable from one that still does.
	// Whether the volume still holds the byte total it promised.
	//
	// A SUM, and only a sum. A reservation held as several chunks satisfies this while no single
	// contiguous run large enough for the next file exists, so "intact" means the arithmetic is
	// right rather than that the next write can be served. Growing a monolithic file needs the old
	// block and the new one at once, which no byte total can promise; see docs/LIBERMEMFS.md, and
	// the arena recorded there as the thing that would make the word mean more than it does.
	pub fn reservation_intact(&self) -> bool {
		// The footprint must also be WITHIN the capacity. `try_reserve_exact` promises at least
		// what was asked for and may give more, so a generous allocator can put the footprint over
		// the capacity - and then `capacity - footprint` saturates to zero, which a volume holding
		// nothing would match. Saturating removed the panic; this removes the false answer.
		self.policy != Policy::Reserved || (self.footprint() <= self.capacity() && self.reserved as u64 == self.capacity() - self.footprint())
	}

	// What can still be written, after both the data and the names ALREADY held.
	//
	// Exact for rewriting an existing file, whose name is already paid for; one name short for
	// creating a new one, which is charged for its name on top. Nothing can do better - the length
	// of a name that does not exist yet is not knowable - so this is the contract rather than a
	// rounding error, and a caller creating an entry should expect to need `free()` minus the name.
	// Reporting `capacity - used` would be worse again, promising room that every name already
	// stored has taken.
	//
	// A reserved volume reports what it HOLDS rather than what the capacity rule allows, and the
	// two differ only when a regrow has fallen short. That keeps one meaning for the word: room
	// this volume can actually promise. A degraded reservation therefore shows up as less free
	// space everywhere it is reported - including through the storage service's `status`, which
	// has no field for "the guarantee slipped" and would otherwise report a volume as healthy
	// while it no longer covered its own free space.
	pub fn free(&self) -> u64 {
		let by_capacity = self.capacity().saturating_sub(self.footprint());
		match self.policy {
			Policy::Reserved => by_capacity.min(self.reserved as u64),
			Policy::Capped => by_capacity,
		}
	}

	// Split a path into its segments, rejecting everything that is not a plain relative path.
	// `.` and `..` are refused rather than resolved: this filesystem has no working directory
	// and no use for either, and accepting `..` would be the one way to name something outside
	// the volume.
	fn segments(path: &[u8]) -> Result<(Segments<'_>, usize), FsError> {
		// Bounded BEFORE the UTF-8 pass, because that pass walks the whole input. Checking depth
		// inside the loop bounds the vector but not the work: a path of a million separators has
		// no non-empty segments, so it never reaches the depth limit however long it is, and every
		// operation - including a read on a volume with nothing left in it - would still walk it.
		// The longest legal path is every segment at its limit with a separator between them.
		if path.len() > MAX_PATH_BYTES {
			return Err(FsError::TooLong);
		}
		let text = core::str::from_utf8(path).map_err(|_| FsError::BadName)?;
		// A fixed array rather than a vector: the depth is bounded, so the segments fit on the
		// stack and path parsing allocates nothing at all. It used to `push` into a `Vec`, which
		// aborts the process when memory is short - so a read on a tight heap died before it
		// reached the fallible allocation it was supposed to refuse at.
		let mut parts: Segments<'_> = [""; MAX_PATH_DEPTH];
		let mut count = 0usize;
		let last_index = text.split('/').count().saturating_sub(1);
		for (index, segment) in text.split('/').enumerate() {
			if segment.is_empty() {
				// A SEPARATOR AT EITHER END IS A SEPARATOR; ONE IN THE MIDDLE IS A MISSING NAME.
				//
				// This skipped every empty segment under a comment saying `/a/b/`, `a//b` and `a/b`
				// named the same thing, "which is what every caller in this tree expects". They do
				// not: `fscore::validate_name_segment` answers `BadName` for an empty segment, and
				// the sentence claiming consensus was the part that was wrong.
				//
				// `a//b` is now refused, which removes the real ambiguity - two spellings of one
				// path, on a filesystem whose sibling backend rejects one of them.
				//
				// Leading and trailing separators are still tolerated, and that is a DECISION
				// rather than an oversight. LiberFS is stricter still - `split_segments` passes
				// every segment to the validator, so `/a/b` is `BadName` there - and matching it
				// would change what `vol://` paths mean for every caller in the tree. Which of the
				// two rules the Storage ABI should state is a question for the ABI; what could not
				// stand is a middle segment meaning different things on two volumes under it.
				//
				// TIGHTENING THIS WAS TRIED AND WITHDRAWN (2026-08-16), and the measurement is the
				// reason it is still written this way. The Storage boundary DOES already answer the
				// question - `rt::RelativePath` refuses an empty segment, `.`, `..`, NUL and
				// backslash before any backend sees a path - so nothing that arrives through
				// `vol://` can carry these spellings and the tolerance is unreachable from outside.
				// Refusing them here anyway turned SEVEN tests red and hung the model soak, because
				// this crate's own callers and fixtures use the leading-slash form throughout. That
				// is an internal renaming exercise, not the finding: the audit's own preferred
				// answer is normalisation at the boundary, which is what already happens.
				if index != 0 && index != last_index {
					return Err(FsError::BadName);
				}
				continue;
			}
			// The SHARED policy, not this filesystem's own. Two writable backends were enforcing
			// two different rules while StorageService mounted both, so an application could create
			// `foo:bar` on the live `vol://system` and not on an installed LiberFS one - the same
			// call answering differently depending on what happened to be underneath. `.` and `..`
			// and non-UTF-8 were already refused here; the portable-byte check was not, and it is
			// what `FsError::BadName` documents.
			fscore::validate_name_segment(segment.as_bytes(), MAX_NAME_BYTES)?;
			if count == MAX_PATH_DEPTH {
				return Err(FsError::TooLong);
			}
			parts[count] = segment;
			count += 1;
		}
		Ok((parts, count))
	}

	// Resolve a path to the node it names, or say why it does not name one.
	//
	// `Ok(None)` means every directory on the way exists and the final name is simply absent -
	// which is what a create needs to know. `Err(NotDir)` means a FILE was used as a directory,
	// and `Err(NotFound)` that a directory on the way is missing. Returning `Option` conflated all
	// three, so `read_file("f/child")` answered NotFound while `write_file` on the same path
	// answered NotDir through the mutable walk: one wrong path, two different errors, and
	// `FsError::NotDir` exists for exactly this case.
	fn resolve(&self, parts: &[&str]) -> Result<Option<&Node>, FsError> {
		let Some((name, directories)) = parts.split_last() else {
			// The empty path names the root.
			return Ok(Some(&self.root));
		};
		let children = self.parent_of(directories)?;
		Ok(find(children, name))
	}

	// The directory `directories` names, walking from the root. Read-only, so a caller can learn
	// whether a parent exists BEFORE anything is released or allocated.
	fn parent_of(&self, directories: &[&str]) -> Result<&Children, FsError> {
		let mut node = &self.root;
		for part in directories {
			let Node::Directory(children) = node else { return Err(FsError::NotDir) };
			node = find(children, part).ok_or(FsError::NotFound)?;
		}
		match node {
			Node::Directory(children) => Ok(children),
			Node::File(_) | Node::Claimed(_) => Err(FsError::NotDir),
		}
	}

	// Walk to the parent of `parts`, creating nothing.
	//
	// Returns the directory alone; the final name comes from `parts`, which the caller already
	// holds. Returning the name from here would tie its lifetime to the borrow of `self`, so a
	// caller could not hold both.
	fn parent_mut(&mut self, parts: &[&str]) -> Result<&mut Children, FsError> {
		let (_, directories) = parts.split_last().ok_or(FsError::BadName)?;
		let mut node = &mut self.root;
		for part in directories {
			let Node::Directory(children) = node else { return Err(FsError::NotDir) };
			node = find_mut(children, part).ok_or(FsError::NotFound)?;
		}
		match node {
			Node::Directory(children) => Ok(children),
			Node::File(_) | Node::Claimed(_) => Err(FsError::NotDir),
		}
	}

	// Walk to the parent of `parts` without borrowing mutably. The read-only twin of `parent_mut`,
	// for the preflight answers (`stream_len`, `writable_len`) that must give the same verdict as
	// the write without being allowed to change anything.
	fn parent(&self, parts: &[&str]) -> Result<&Children, FsError> {
		let (_, directories) = parts.split_last().ok_or(FsError::BadName)?;
		let mut node = &self.root;
		for part in directories {
			let Node::Directory(children) = node else { return Err(FsError::NotDir) };
			node = find(children, part).ok_or(FsError::NotFound)?;
		}
		match node {
			Node::Directory(children) => Ok(children),
			Node::File(_) | Node::Claimed(_) => Err(FsError::NotDir),
		}
	}

	// Whether the volume may take another entry AT `parts`, by BOTH limits: the count of live nodes
	// and the bytes its directory tables would hold AFTERWARDS. Two rules rather than one because
	// they bound different things - MAX_ENTRIES bounds what exists, MAX_METADATA_BYTES bounds what
	// was kept - and the second is the one a caller can drive without limit through fill-and-empty
	// cycles.
	//
	// AGAINST THE RESULT, AND FOR THE DESTINATION'S OWN TABLE. This was
	// `self.root.count() < MAX_ENTRIES && self.metadata_bytes() < MAX_METADATA_BYTES` - a check on
	// the state BEFORE the allocation, with no idea which table the entry was going into - and it
	// was wrong in both directions.
	//
	// It let an insert finish above the ceiling: `insert` does `children.try_reserve(1)`, which may
	// DOUBLE the table rather than add one slot, so a table admitted a byte below the limit could
	// come back megabytes above it, with no post-check and no rollback. "A hard ceiling on live and
	// retained directory tables" was a ceiling on the state before the allocation.
	//
	// And it refused an insert that needed no allocation at all: at exactly the boundary, a table
	// with a free slot already in it was turned away, because the question asked was about the
	// volume rather than about the work.
	//
	// Reaching this ceiling should not happen either way: `shrink_children` keeps every table
	// within a factor of four of its contents. It is here so that "should not" is enforced rather
	// than argued - which is only true if what it enforces is the result.
	fn room_for_an_entry(&self, parts: &[&str]) -> bool {
		self.root.count() < MAX_ENTRIES && self.room_for_an_entry_by_bytes(parts)
	}

	// The metadata half alone, for `rename` - which moves an entry rather than adding one, so the
	// node count is unchanged and only the destination table's growth is in question.
	fn room_for_an_entry_by_bytes(&self, parts: &[&str]) -> bool {
		// A missing or non-directory parent is not a metadata question - the insert will refuse it
		// on its own, with a better error than `NoSpace`. Answering `true` leaves that refusal
		// where it belongs.
		let Ok(children) = self.parent(parts) else { return true };
		metadata_admits_a_slot(self.metadata_bytes() as usize, children.capacity(), children.len(), MAX_METADATA_BYTES)
	}

	// Give up the whole reservation. The vector and the byte count move together and must never
	// disagree, so nothing sets either of them directly - every accounting defect in this
	// filesystem has been two pieces of the same fact drifting apart.
	fn release_reservation(&mut self) {
		self.reservation = Vec::new();
		self.reserved = 0;
	}

	// Take `bytes` for the reservation, or report that it could not be had.
	//
	// The SLOT is reserved before the chunk, because pushing is an allocation too: the chunk was
	// taken fallibly and then stored with an infallible `push`, which ends the process now that
	// the allocator returns null instead of exiting on its own.
	fn reserve_chunk(&mut self, bytes: usize) -> bool {
		if bytes == 0 || self.reservation.len() >= MAX_RESERVATION_CHUNKS {
			return false;
		}
		if self.reservation.try_reserve(1).is_err() {
			return false;
		}
		let mut allocation: Vec<u8> = Vec::new();
		if allocation.try_reserve_exact(bytes).is_err() {
			return false;
		}
		// Measured AFTER the allocation, because what the volume holds is what the allocator gave.
		let held = allocation.capacity();
		self.reservation.push(Chunk { allocation, bytes: held });
		self.reserved += held;
		true
	}

	// Hold exactly the unused part of a reserved volume's capacity, so its footprint stays at
	// what it took at mount however its contents change.
	//
	// It has to run after EVERY mutation, not just after a write that grows a file. An earlier
	// version only ever shrank the reservation, so deleting or shrinking a file handed that
	// memory back to the heap rather than back to the reservation - and a volume that was
	// supposed to guarantee its space silently stopped guaranteeing it. `mkdir` and `rmdir` count
	// too, since a name is part of the footprint.
	fn resync_reservation(&mut self) {
		if self.policy != Policy::Reserved {
			return;
		}
		let target = self.capacity.saturating_sub(self.footprint() as usize);
		if self.reserved == target {
			return;
		}
		// Dropped and reallocated rather than resized in place, in both directions.
		//
		// Resizing looks cheaper and is not. `shrink_to_fit` reallocates and COPIES the bytes it
		// keeps, so every write to a reserved volume would memcpy what remains of the
		// reservation - megabytes per write on a volume of any size, to preserve bytes that
		// nothing ever reads. Dropping first also means the old block is gone before the new one
		// is asked for, so the two are never outstanding together: the same rule the write path
		// follows, and for the same reason.
		//
		// Nothing is written into the new block either. Filling it cost a memset of the whole
		// remaining reservation on every mutation - megabytes per operation on a large volume,
		// the same price as the copy it replaced - and bought nothing: the frames are committed
		// and charged when the heap maps them, not when a page is first touched, so the
		// allocation alone is the reservation.
		//
		// Best effort on the way back up, and fallible on purpose: any infallible allocation -
		// `Vec::resize`, `with_capacity`, a plain `reserve` - ABORTS when the memory is not there,
		// which in a storage service is a crash where a degraded guarantee would do. If the memory
		// cannot be had the volume holds less than its capacity and `reserved_bytes` says so.
		// Grown by ADDING a chunk, never by replacing what is already held.
		//
		// Releasing first and then hunting could leave the volume worse than it started: holding
		// 32 MiB, asked for 33, and ending with 16 because that was the first size that fit. "Best
		// effort" has to mean at least what it already had. Holding the reservation as several
		// chunks also makes fragmentation far less likely to defeat it - a heap with no 33 MiB
		// block very often has two of 16.
		let held: usize = self.reserved;
		if target < held {
			// Shrinking: give back whole chunks from the end until the rest fits the target.
			while self.reserved > target {
				let Some(chunk) = self.reservation.pop() else { break };
				self.reserved -= chunk.bytes;
			}
			// One chunk may still straddle the target; trading it for an exact one is worth a
			// single allocation, and failing that the volume simply holds a little less.
			if self.reserved < target {
				self.reserve_chunk(target - self.reserved);
			}
			return;
		}
		// Growing: ask for the whole shortfall first, and only halve after a refusal. The floor
		// bounds the RETRIES, not the first attempt - guarding the first ask would refuse to grow
		// a small volume at all.
		let mut want = target - held;
		while want > 0 {
			if self.reserve_chunk(want) {
				want = target - self.reserved;
				continue;
			}
			if want < MIN_RESERVATION_STEP {
				break;
			}
			want /= 2;
		}
		// The allocation's only purpose is that it exists: nothing reads it and nothing writes it,
		// which is exactly the shape a dead-allocation optimisation is entitled to delete. Letting
		// the pointer escape here says otherwise, and costs nothing at runtime.
		for chunk in &self.reservation {
			let _ = core::hint::black_box(chunk.allocation.as_ptr());
		}
	}

	// Read a whole file.
	//
	// The copy is fallible: `Vec::clone` aborts the process when the memory is not there, and a
	// 64 MiB file is a 64 MiB allocation inside a storage service that should answer `NoSpace`
	// rather than die.
	pub fn read_file(&self, path: &[u8]) -> Result<Vec<u8>, FsError> {
		let (parts, count) = Self::segments(path)?;
		let parts = &parts[..count];
		match self.resolve(parts)? {
			// A CLAIMED entry reads. A stream that is abandoned must leave the file exactly as it
			// was, so until it commits the bytes it is holding ARE the file's contents - empty for
			// a destination that did not exist, the previous contents for one being replaced.
			// Refusing the read would make an open stream change what other callers see, which is
			// the opposite of what the claim is for.
			Some(Node::File(data)) | Some(Node::Claimed(data)) => {
				let mut out: Vec<u8> = Vec::new();
				out.try_reserve_exact(data.len()).map_err(|_| FsError::NoMemory)?;
				out.extend_from_slice(data);
				Ok(out)
			}
			Some(Node::Directory(_)) => Err(FsError::IsDir),
			None => Err(FsError::NotFound),
		}
	}

	// Write a whole file, creating or replacing it.
	//
	// The order matters and is the fix for three separate defects. Everything that can refuse is
	// checked while the volume is untouched: the parent is resolved READ-ONLY first, so a write to
	// a path that cannot exist costs nothing, answers `NotFound` or `NotDir` rather than `NoSpace`
	// on a full volume, and - crucially - cannot refuse after the reservation has been released.
	// Only then is anything released or allocated.
	pub fn write_file(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
		// `TooLarge`, NOT `TooLong`. `fs-core` documents `TooLong` as "a path or name longer than the
		// filesystem allows" and `TooLarge` as an answer that does not fit one buffer, and this is a
		// ceiling on the FILE. A caller told `TooLong` may reasonably shorten its path, which cannot
		// help; every byte-count refusal in this file now says the one thing the caller can act on.
		if data.len() > MAX_FILE_BYTES {
			return Err(FsError::TooLarge);
		}
		let (parts, count) = Self::segments(path)?;
		let parts = &parts[..count];
		if parts.is_empty() {
			// The root is a directory and cannot be written as a file.
			return Err(FsError::IsDir);
		}
		// Resolves the parent as a side effect, so an absent or non-directory parent is refused
		// here rather than after the data has been copied.
		// What this name holds today and what it would hold afterwards - both as ALLOCATIONS, since
		// that is what the capacity bounds. A rewrite that fits inside the file's existing
		// allocation costs nothing; one that does not grows it to exactly the new length.
		let (previous, becomes, is_new) = match self.resolve(parts)? {
			Some(Node::Directory(_)) => return Err(FsError::IsDir),
			// A destination an open stream is filling is not available to be written. `Exists` is
			// the answer `mkdir` already gives for the same condition: the name is taken.
			Some(Node::Claimed(_)) => return Err(FsError::Exists),
			Some(Node::File(existing)) => (existing.capacity(), existing.capacity().max(data.len()), false),
			None => {
				if !self.room_for_an_entry(parts) {
					return Err(FsError::NoSpace);
				}
				(0, data.len(), true)
			}
		};
		// Subtracting what the file already holds is what lets it be rewritten on a full volume.
		// A new entry also costs its name; replacing an existing one does not, because the name is
		// already there and already counted.
		let name_cost = if is_new { parts.last().map_or(0, |name| name.len()) } else { 0 };
		if self.footprint() as usize - previous + becomes + name_cost > self.capacity {
			return Err(FsError::NoSpace);
		}
		// A reserved volume releases what it is holding BEFORE allocating. Otherwise the
		// allocation and the reservation are outstanding at the same time and the write can fail
		// for memory the volume is itself sitting on - precisely the failure a reservation exists
		// to prevent.
		//
		// All of it, not just the difference: the whole point is that the bytes are certainly
		// available, and releasing exactly enough leaves that resting on the reservation being
		// perfectly in step, which it is not after a best-effort regrow has fallen short.
		// Released only when something is actually going to be allocated. A rewrite that fits
		// inside the file's existing buffer allocates nothing - no data, no name, no map node - so
		// giving up the reservation and hunting for it again could only make the volume worse.
		let allocates = becomes > previous || is_new;
		if self.policy == Policy::Reserved && allocates {
			self.release_reservation();
		}
		// In its own call so every temporary it makes is dropped BEFORE the resync. Holding a
		// refused file's bytes while asking for the reservation back means competing with
		// yourself for the memory: the regrow can fall short for exactly the allocation that is
		// about to be thrown away.
		let stored = self.store(parts, data, is_new);
		// The same post-allocation check the streaming path has had, for the same reason:
		// `try_reserve_exact` promises AT LEAST what was asked for, `footprint` counts what was
		// actually given, and a generous allocator could leave a successful write with the volume
		// over its own capacity. The streaming path undid its overshoot and this one computed its
		// bound before `store` and never looked again.
		//
		// A new entry is undone completely - the file was not there a moment ago. In-place growth
		// is not: the old contents are gone and putting the file back the way it was is not
		// possible from here, so the overshoot is accepted and the volume records it rather than
		// discarding a write that succeeded. That asymmetry is the monolithic `Vec` arguing for the
		// chunked storage the reservation finding wants.
		if stored.is_ok() && is_new && (self.footprint() as usize) > self.capacity {
			let _ = self.remove(path);
			self.resync_reservation();
			return Err(FsError::NoSpace);
		}
		self.resync_reservation();
		stored
	}

	// Write a file from a buffer the caller hands over, taking ownership of the allocation.
	//
	// A streamed write collects the whole file before the filesystem sees any of it, so the
	// collected buffer and the volume's reservation are outstanding at the same time - a reserved
	// volume would accept a file through `write_file` and refuse the identical file through a
	// stream, because the stream needs the memory twice. Taking the caller's buffer as the file's
	// own storage removes the second copy: nothing is allocated here at all.
	// Begin receiving a stream into `path`.
	//
	// The destination is validated NOW, so a stream to a missing parent or to a directory is
	// refused before a byte is taken, and the caller learns it while it still costs nothing.
	pub fn stream_begin(&mut self, path: &[u8]) -> Result<(), FsError> {
		// THE RESYNC HAPPENS AFTER THE LOCALS ARE GONE, which is why this is a wrapper.
		//
		// Two refusal branches inside used to resync while an allocation they had just made was
		// still a live local: `owned` after `extend_from_slice` succeeded, and `owned` plus
		// `owned_name` when the `insert` failed. Neither was in `self.pending` and neither was in
		// the tree, so `footprint()` could not see them - and `resync_reservation` therefore tried
		// to take back the WHOLE declared free capacity while part of the heap was still held by
		// memory nothing was accounting for. On a tight heap the regrow came up short, the locals
		// dropped at the `return`, no second resync ever ran, and the volume was left answering
		// `NoSpace` with `reservation_intact() == false` - the invariant this filesystem documents
		// as restored by every refusal.
		//
		// `write_file`, `truncate` and `stream_commit` all have this shape already: the fallible
		// work happens in an `_unsynced` body whose temporaries are destroyed by the return, and
		// the caller resynchronises once, unconditionally, on both endings. It is not an
		// improvement in tidiness - it is the only ordering under which the resync sees the heap
		// the volume actually has.
		if self.pending.is_some() {
			return Err(FsError::Invalid);
		}
		let begun = self.stream_begin_unsynced(path);
		self.resync_reservation();
		begun
	}

	fn stream_begin_unsynced(&mut self, path: &[u8]) -> Result<(), FsError> {
		let (parts, count) = Self::segments(path)?;
		let parts = &parts[..count];
		if parts.is_empty() {
			return Err(FsError::IsDir);
		}
		let is_new = match self.resolve(parts)? {
			Some(Node::Directory(_)) => return Err(FsError::IsDir),
			// Unreachable while `self.pending.is_some()` is refused above - one stream at a time,
			// and a claim only exists while its stream does. Refused rather than assumed away.
			Some(Node::Claimed(_)) => return Err(FsError::Exists),
			Some(Node::File(_)) => false,
			None => {
				if !self.room_for_an_entry(parts) {
					return Err(FsError::NoSpace);
				}
				true
			}
		};
		// The path counts against the volume, so opening a stream on a full one must be refused
		// rather than quietly taking it over capacity. Nothing of the file has arrived yet; this is
		// the entry's own cost.
		// The path AND the destination's name, because both are taken below and both are held for
		// the stream's lifetime.
		//
		// AND NOT THE SLOT, which this briefly added under a comment saying "`footprint` counts the
		// directory table it goes into". It does not: `footprint` is `allocated + names + pending`
		// and the tables are `metadata_bytes`, a separate budget under `MAX_METADATA_BYTES` - which
		// is the whole point of the retained-table finding this milestone opened with. The comment
		// forty lines below said so correctly at the same time, and the arithmetic followed the
		// wrong one.
		//
		// It had a consequence past the tidiness. `stream_len` costs a new destination at
		// `path + name`; charging `+ SLOT_BYTES` here made a volume with room for one and not the
		// other answer `Ok(0)` to the preflight and `NoSpace` to the begin - the same
		// preflight/execute disagreement `writable_len` was fixed for. The slot is bounded where a
		// directory entry has always been bounded: `room_for_an_entry` above, and the fallible
		// `insert` below.
		let entry_cost = path.len() + if is_new { parts.last().map_or(0, |name| name.len()) } else { 0 };
		if (self.footprint() as usize).saturating_add(entry_cost) > self.capacity {
			return Err(FsError::NoSpace);
		}
		// Release the reservation BEFORE allocating, the way the ordinary write does.
		//
		// A reserved volume holds its whole free capacity in the reservation, so allocating the
		// path beside it competes with memory the volume itself is sitting on: a stream could be
		// refused for want of a few bytes the volume was holding for exactly this. The ordinary
		// path has released first since the reservation existed; the stream path did not.
		//
		// Released HERE and not in the wrapper, so it is released only when there is something to
		// release it for - the same discipline `write_file_owned_unsynced_named` follows. A refusal
		// above this line (a bad path, a directory, no room for an entry) never touched the
		// reservation and must not start: dropping it and asking for it back is a window in which
		// a tight heap can decline, and a volume should not acquire that window for a call that
		// allocates nothing. The wrapper's resync is a no-op when nothing moved.
		if self.policy == Policy::Reserved {
			self.release_reservation();
		}
		let mut owned: Vec<u8> = Vec::new();
		if owned.try_reserve_exact(path.len()).is_err() {
			return Err(FsError::NoMemory);
		}
		owned.extend_from_slice(path);
		// The destination's NAME, taken now rather than at commit - see `Pending::name`. It is the
		// expensive half of the metadata (up to 255 bytes against a few dozen for a slot) and the
		// half that is charged to the volume's capacity, so taking it here is what turns "the
		// commit failed after accepting everything" into "the stream was refused before anything
		// was sent".
		//
		// AND THE SLOT, by putting an empty file there now.
		//
		// The name was prepaid and the slot was not, so `insert`'s `children.try_reserve(1)` was a
		// fallible allocation at COMMIT - after the whole file had been accepted. "The stream
		// promised to take this and the commit cannot fail" was therefore not true: it could fail,
		// on one slot, at the end.
		//
		// A placeholder answers that and the namespace question together. `stream_begin` validated
		// a destination and then held nothing, so another client could `rmdir` the parent or
		// `mkdir` over the path while a large transfer was in flight and the commit would discover
		// it - work already done, thrown away. An entry in the tree is a claim on the name.
		//
		// This is not the budget confusion that was refused before. RESERVING a slot inside the
		// capacity arithmetic would make a reserved volume count memory the metadata budget does
		// not; inserting an ordinary entry charges the metadata budget, which is where a directory
		// entry has always been charged. `room_for_an_entry` has already said one is available.
		//
		// BOTH CASES ARE CLAIMED, which the first version of this did not do: `let placeholder =
		// is_new` meant a stream REPLACING a file held nothing at all, so the sequence the claim
		// exists to prevent still worked - remove the destination mid-transfer and the commit
		// re-resolved to nothing and took the create path, allocating the name and the slot at the
		// end after all. A replacement claims the node it found, keeping the previous contents
		// inside the claim so an abort can put them back untouched.
		let placeholder = is_new;
		if is_new {
			let last = parts.last().copied().unwrap_or("");
			let mut owned_name = String::new();
			if owned_name.try_reserve_exact(last.len()).is_err() {
				return Err(FsError::NoMemory);
			}
			owned_name.push_str(last);
			let placed = self.parent_mut(parts).and_then(|children| insert(children, owned_name, Node::Claimed(Vec::new())));
			if placed.is_err() {
				return Err(FsError::NoSpace);
			}
		} else {
			// Claim in place. No allocation: the existing buffer moves into the claim and back out
			// of it, so this cannot fail and a replacement stream cannot be refused for memory
			// after its destination has been checked.
			let last = *parts.last().unwrap_or(&"");
			if let Ok(children) = self.parent_mut(parts)
				&& let Some(node) = find_mut(children, last)
				&& let Node::File(data) = node
			{
				*node = Node::Claimed(core::mem::take(data));
			}
		}
		self.pending = Some(Pending { path: owned, data: Vec::new(), placeholder, offered: None });
		// The allocator may hand back more than was asked for, and `footprint` counts what it
		// actually gave. A volume must not end up over its own capacity because it was given a
		// generous answer, so the overshoot is undone here rather than merely noticed later by
		// `reservation_intact`.
		if (self.footprint() as usize) > self.capacity {
			self.stream_abort();
			return Err(FsError::NoSpace);
		}
		Ok(())
	}

	// Take one chunk. Charged against the volume as it arrives, so a stream that will not fit is
	// refused at the chunk that crosses the line rather than when the heap runs out.
	pub fn stream_push(&mut self, chunk: &[u8]) -> Result<(), FsError> {
		let Some(pending) = self.pending.as_ref() else { return Err(FsError::Invalid) };
		// NOT WHILE AN OFFER IS OUTSTANDING. `stream_spare` hands the caller a zero-filled window
		// and `stream_advance` truncates by `data.len() - (offered - written)` afterwards, so a push
		// in between makes that arithmetic keep the WRONG BYTES: spare(100) then push("abc") then
		// advance(100, 0) keeps the first three, which are the offer's zeros, and discards the three
		// that were pushed. The volume then commits a three-byte file of zeros and reports it whole.
		//
		// Which is the sentence `stream_commit` already carries - "bytes that were never written, in
		// a file that reports itself complete" - reached through the one entry point that was not
		// asking. The question belongs at every door into the pending buffer, not at one of them.
		if pending.offered.is_some() {
			return Err(FsError::Invalid);
		}
		let want = pending.data.len().saturating_add(chunk.len());
		if want > MAX_FILE_BYTES {
			self.stream_abort();
			return Err(FsError::TooLarge);
		}
		// What the volume would hold once this chunk is in: everything except the pending buffer's
		// current allocation, plus what it would grow to.
		let without = (self.footprint() as usize).saturating_sub(pending.data.capacity());
		if without.saturating_add(want) > self.capacity {
			self.stream_abort();
			return Err(FsError::NoSpace);
		}
		// A reserved volume gives the room back BEFORE the allocation, so the two are never
		// outstanding at once - the failure the reservation exists to prevent.
		if self.policy == Policy::Reserved {
			self.release_reservation();
		}
		let pending = self.pending.as_mut().ok_or(FsError::Invalid)?;
		// `chunk.len()`, NOT `want - capacity`.
		//
		// `try_reserve_exact(additional)` guarantees room for `len + additional`, not for
		// `capacity + additional`. Reserving the difference from CAPACITY asks for too little
		// whenever `len < capacity < want` - with len 20, capacity 32 and want 40 it asks for 28,
		// which is already satisfied - and the `extend_from_slice` below then grows the vector
		// through the ordinary INFALLIBLE path. Under memory pressure that aborts the service,
		// which is the single failure this filesystem exists to avoid.
		if pending.data.try_reserve_exact(chunk.len()).is_err() {
			self.stream_abort();
			return Err(FsError::NoMemory);
		}
		pending.data.extend_from_slice(chunk);
		// The check the preflight above cannot make: `try_reserve_exact` promises at least what was
		// asked for and may give MORE, and the footprint counts what it actually gave. `stream_spare`
		// has had this since over-allocation was first taken seriously; this path did not, so a
		// generous allocator could carry a reserved volume past its own capacity through the public
		// API. StorageService drives the memory filesystem through `stream_spare`, so production was
		// covered and the hole was left exactly where a second caller would find it.
		//
		// Reachable only when the allocator returns more than was asked for: this path reserves
		// exactly what it appends, so `len` and `capacity` move together otherwise. That makes it a
		// latent defect rather than a reproducible one, and the reason no test here forces it.
		if (self.footprint() as usize) > self.capacity {
			self.stream_abort();
			return Err(FsError::NoSpace);
		}
		self.resync_reservation();
		Ok(())
	}

	// Make room for `want` more bytes and hand back the space to receive INTO.
	//
	// The transport used to allocate the whole message as its own vector and then copy it in here,
	// so a reserved volume held its reservation, that chunk, the pending buffer and - while it grew
	// - the pending buffer's old and new allocations, all at once. The reservation could not
	// promise to take one chunk near its capacity because of a buffer the volume did not know
	// about. Receiving straight into the pending buffer removes both the second allocation and the
	// copy.
	//
	// The space is zeroed rather than exposed uninitialised: a memset costs far less than the
	// allocation it replaces, and the alternative is unsafe code on the receive path of a service
	// that reads from untrusted peers.
	pub fn stream_spare(&mut self, want: usize) -> Result<&mut [u8], FsError> {
		let Some(pending) = self.pending.as_ref() else { return Err(FsError::Invalid) };
		// One offer at a time. A second `stream_spare` over an open one would hand out a slice
		// whose predecessor is still unaccounted for, and the truncation at the end could then only
		// guess which of them the numbers described.
		if pending.offered.is_some() {
			return Err(FsError::Invalid);
		}
		let filled = pending.data.len();
		let total = filled.saturating_add(want);
		if total > MAX_FILE_BYTES {
			self.stream_abort();
			return Err(FsError::TooLarge);
		}
		let without = (self.footprint() as usize).saturating_sub(pending.data.capacity());
		if without.saturating_add(total) > self.capacity {
			self.stream_abort();
			return Err(FsError::NoSpace);
		}
		if self.policy == Policy::Reserved {
			self.release_reservation();
		}
		let pending = self.pending.as_mut().ok_or(FsError::Invalid)?;
		if pending.data.try_reserve_exact(want).is_err() {
			self.stream_abort();
			return Err(FsError::NoMemory);
		}
		pending.data.resize(total, 0);
		// `try_reserve_exact` promises at least what was asked for and may give more, and the
		// footprint counts what it actually gave. Undo an overshoot rather than leave the volume
		// over its own capacity and find out later from an invariant check.
		if (self.footprint() as usize) > self.capacity {
			self.stream_abort();
			return Err(FsError::NoSpace);
		}
		let pending = self.pending.as_mut().ok_or(FsError::Invalid)?;
		pending.offered = Some(total - filled);
		Ok(&mut pending.data[filled..])
	}

	// Keep `written` of the space handed out, discarding the rest.
	// Close the outstanding offer, keeping `written` of it.
	//
	// `offered` is checked against what was actually handed out rather than believed. A caller that
	// passes a different number was describing some other offer, and truncating by it discards
	// bytes that were already valid; a caller that closes an offer that was never opened has
	// nothing to close. Both are refused, and the offer - if there is one - stays open.
	pub fn stream_advance(&mut self, offered: usize, written: usize) -> Result<(), FsError> {
		let Some(pending) = self.pending.as_mut() else { return Err(FsError::Invalid) };
		let Some(outstanding) = pending.offered else { return Err(FsError::Invalid) };
		if offered != outstanding || written > outstanding {
			return Err(FsError::Invalid);
		}
		let keep = pending.data.len() - (outstanding - written);
		pending.data.truncate(keep);
		pending.offered = None;
		self.resync_reservation();
		Ok(())
	}

	// Store what arrived. The buffer becomes the file's own storage, so nothing is copied and the
	// volume never holds two versions of the contents at once.
	pub fn stream_commit(&mut self) -> Result<(), FsError> {
		// An outstanding offer means the caller was handed a slice and never said how much of it it
		// wrote. Committing would store the rest as the zeros `stream_spare` filled it with - bytes
		// that were never written, in a file that reports itself complete.
		if self.pending.as_ref().is_some_and(|p| p.offered.is_some()) {
			return Err(FsError::Invalid);
		}
		let Some(pending) = self.pending.take() else { return Err(FsError::Invalid) };
		// Adopt FIRST, resync ONCE, at the end - and the end is here, not inside the store.
		//
		// Resyncing before the adoption regrew the reservation while the data and the path were
		// still alive in this local - memory the volume had just stopped counting and was about to
		// hand to the file - so the regrow competed with itself. Moving it into the store fixed
		// that and left a smaller version of the same shape behind.
		//
		// The path has to stay alive while the store reads it, so a resync INSIDE the store runs
		// with those bytes held and no longer counted - and could fail to regrow the reservation
		// over them on a tight or fragmented heap, leaving it short after a commit that succeeded.
		// A few hundred bytes rather than the whole file's buffer, and still not nothing.
		//
		// So the store does not resync: it adopts, the path drops when `pending` does, and the one
		// resync happens here with the volume's footprint being exactly what it will be.
		let stored = self.write_file_owned_unsynced(&pending.path, pending.data);
		// THE CLAIM ENDS HERE, whichever way the store went.
		//
		// The store has two paths: `insert` replaces the node outright, which already leaves a
		// `Node::File`, and ADOPT rewrites the existing buffer through `file_mut` - which leaves
		// the node exactly as it found it, claimed. So a committed stream whose data fitted the
		// destination's existing allocation would have left a claim nobody could ever release, and
		// the file would have been permanently unwritable and unremovable.
		//
		// Unconditional, because a FAILED commit must not leave one either: the stream is over
		// either way, and a claim outlives its stream in no case.
		self.release_claim(&pending.path);
		drop(pending.path);
		self.resync_reservation();
		stored
	}

	// Turn a claimed entry back into an ordinary file, wherever the stream that claimed it ended.
	fn release_claim(&mut self, path: &[u8]) {
		let Ok((parts, count)) = Self::segments(path) else { return };
		let parts = &parts[..count];
		let Some(&last) = parts.last() else { return };
		let Ok(children) = self.parent_mut(parts) else { return };
		if let Some(node) = find_mut(children, last)
			&& let Node::Claimed(data) = node
		{
			*node = Node::File(core::mem::take(data));
		}
	}

	// Give up. The room goes back to the volume, and the destination is untouched - a stream that
	// cannot be completed leaves the file exactly as it was.
	pub fn stream_abort(&mut self) {
		// RELEASE THE CLAIM, and release only the claim.
		//
		// This used to be `if pending.placeholder { self.remove(&path) }` - an unconditional remove
		// of whatever was at that path. Nothing checked that the node there was still the entry
		// this stream inserted, and nothing stopped another caller replacing it, so:
		//
		//     A: stream_begin("x")           the placeholder
		//     B: write_file("x", important)  accepted, because the placeholder was an ordinary file
		//     A: stream_abort()              remove("x") - B's completed write, gone
		//
		// Both halves are closed: `Node::Claimed` makes the mutators refuse, so B cannot get in;
		// and this looks at the node before touching it, so even a path that changed underneath a
		// future caller is left alone rather than removed on the strength of a flag.
		//
		// MOVED, not cloned: `pending` is owned here, and a `clone()` of a 255-byte path is an
		// infallible allocation on the path that runs when the volume is already out of room.
		if let Some(pending) = self.pending.take() {
			let placeholder = pending.placeholder;
			let path = pending.path;
			if let Ok((parts, count)) = Self::segments(&path) {
				let parts = &parts[..count];
				if let Some(&last) = parts.last()
					&& let Ok(children) = self.parent_mut(parts)
					&& find(children, last).is_some_and(Node::is_claimed)
				{
					if placeholder {
						// It existed only as this stream's claim: it goes with the stream.
						remove(children, last);
					} else if let Some(node) = find_mut(children, last)
						&& let Node::Claimed(data) = node
					{
						// It existed before the stream did. The bytes the claim was holding ARE
						// the file, so putting them back leaves it exactly as it was found - which
						// is what this function's own contract says.
						*node = Node::File(core::mem::take(data));
					}
				}
			}
		}
		self.pending = None;
		self.resync_reservation();
	}

	// Whether a stream is in progress, for a caller that has to decide whether to abort one.
	pub fn streaming(&self) -> bool {
		self.pending.is_some()
	}

	pub fn write_file_owned(&mut self, path: &[u8], data: Vec<u8>) -> Result<(), FsError> {
		// The claim refusal lives in the PUBLIC entry points and not in `_unsynced`, because
		// `stream_commit` reaches `_unsynced` to replace the claim it made itself. A caller that
		// arrives here is somebody else.
		if self.is_claimed(path) {
			return Err(FsError::Exists);
		}
		let stored = self.write_file_owned_unsynced(path, data);
		self.resync_reservation();
		stored
	}

	// Is an open stream holding this path? The public mutators ask before they touch it.
	fn is_claimed(&self, path: &[u8]) -> bool {
		let Ok((parts, count)) = Self::segments(path) else { return false };
		matches!(self.resolve(&parts[..count]), Ok(Some(node)) if node.is_claimed())
	}

	fn write_file_owned_unsynced(&mut self, path: &[u8], data: Vec<u8>) -> Result<(), FsError> {
		self.write_file_owned_unsynced_named(path, data, None)
	}

	fn write_file_owned_unsynced_named(&mut self, path: &[u8], data: Vec<u8>, prepared: Option<String>) -> Result<(), FsError> {
		if data.len() > MAX_FILE_BYTES {
			return Err(FsError::TooLarge);
		}
		let (parts, count) = Self::segments(path)?;
		let parts = &parts[..count];
		if parts.is_empty() {
			return Err(FsError::IsDir);
		}
		let (previous, is_new) = match self.resolve(parts)? {
			Some(Node::Directory(_)) => return Err(FsError::IsDir),
			Some(Node::File(existing)) | Some(Node::Claimed(existing)) => (existing.capacity(), false),
			None => {
				if !self.room_for_an_entry(parts) {
					return Err(FsError::NoSpace);
				}
				(0, true)
			}
		};
		// The buffer arrives already allocated, so what the volume takes on is its CAPACITY -
		// EXCEPT where `adopt` keeps the file's existing buffer and copies into it, which it does
		// whenever the new contents fit. There the volume takes on nothing new at all.
		//
		// Charging `data.capacity()` in that branch refused writes that would have fitted: an
		// allocator may hand `try_reserve_exact` more than was asked for, so a 60 MB stream into a
		// 64 MB file could arrive in a 68 MB vector and be rejected while the write it guards
		// would have reused the 64 MB already there. This filesystem is careful elsewhere to
		// respect over-allocation; the guard has to branch the way the write does.
		let name_cost = if is_new { parts.last().map_or(0, |name| name.len()) } else { 0 };
		let becomes = if !is_new && data.len() <= previous { previous } else { data.capacity() };
		if self.footprint() as usize - previous + becomes + name_cost > self.capacity {
			return Err(FsError::NoSpace);
		}
		// Only the name and the entry slot are allocated here, so the reservation is released
		// only when there is something to release it for.
		if self.policy == Policy::Reserved && is_new {
			self.release_reservation();
		}
		self.adopt(parts, data, is_new, prepared)
	}

	// Take ownership of `data` as the file at `parts`.
	fn adopt(&mut self, parts: &[&str], data: Vec<u8>, is_new: bool, prepared: Option<String>) -> Result<(), FsError> {
		if !is_new {
			// Reusing the file's existing buffer when the new contents fit, exactly as an ordinary
			// rewrite does. Assigning the incoming vector instead would silently COMPACT the file
			// - the same logical shrink accounting differently depending on which API the caller
			// reached for - and the document says a file keeps its allocation until it is removed.
			let file = self.file_mut(parts)?;
			// The same give-back as the ordinary rewrite: a stream that delivered nothing leaves
			// the file holding nothing, not holding its old block.
			if data.is_empty() {
				*file = Vec::new();
			} else if data.len() <= file.capacity() {
				file.clear();
				file.extend_from_slice(&data);
			} else {
				*file = data;
			}
			return Ok(());
		}
		// The name a stream prepared at `stream_begin`, or a fresh one for an ordinary write.
		let name = match prepared {
			Some(name) => name,
			None => {
				let last = parts.last().copied().ok_or(FsError::BadName)?;
				let mut name = String::new();
				name.try_reserve_exact(last.len()).map_err(|_| FsError::NoMemory)?;
				name.push_str(last);
				name
			}
		};
		let children = self.parent_mut(parts)?;
		insert(children, name, Node::File(data))
	}

	// Put `data` at `parts`, which `write_file` has already established is writable.
	fn store(&mut self, parts: &[&str], data: &[u8], is_new: bool) -> Result<(), FsError> {
		if !is_new {
			// Rewriting reuses the file's own allocation. Building a second vector would hold the
			// old contents and the new ones at once - so a volume at its capacity would need
			// twice the file to write into itself, which is the failure the capacity check
			// claims to have avoided by subtracting the previous size.
			//
			// The reserve happens BEFORE the clear, so a refused rewrite leaves the file exactly
			// as it was rather than truncated to nothing.
			// The file's current allocation, read before anything borrows it mutably.
			let held = self.file_mut(parts)?.capacity();
			let file = self.file_mut(parts)?;
			// TRUNCATION TO NOTHING GIVES THE BLOCK BACK. A capped volume otherwise keeps the
			// quota - `allocated()` counts capacity, which is the rule that closed the
			// grow-and-shrink hole and is worth keeping - but `vol://tmp` is exactly where a caller
			// expects a truncate to return the space, and the empty case costs nothing to give:
			// assigning a fresh `Vec` frees the block and cannot fail. Larger shrinks still hold
			// their allocation, because compacting them is fallible and losing a write to it would
			// be a worse trade.
			if data.is_empty() {
				*file = Vec::new();
				return Ok(());
			}
			// Only when the bytes do not fit what the file ALREADY holds. `data.len() > file.len()`
			// was the wrong test: a file of ten bytes in a forty-four byte buffer taking twenty new
			// ones needs no allocation at all, and replacing the buffer there would quietly compact
			// the file - the high-water rule this filesystem states, broken by the fix for something
			// else.
			if data.len() > file.capacity() {
				// GROWN INTO A NEW BUFFER when the allocator would overshoot, so the overshoot is
				// recoverable.
				//
				// `try_reserve_exact` promises AT LEAST what was asked for, and `write_file` undid a
				// generous answer only for a NEW entry - for in-place growth it accepted it, because
				// the old contents were already gone and putting the file back was not possible from
				// there. So `write_file` could return `Ok(())` on a volume whose footprint then
				// exceeded its own capacity.
				//
				// Reserving into a separate vector first makes the decision reversible: if the
				// allocator gives more than the volume can hold, the new buffer is dropped and the
				// file is untouched. The extra cost is one buffer of the new size, which is what the
				// comment above says building a second vector would cost - and it is paid only on
				// the growth path, where the file is getting bigger anyway.
				let mut grown: Vec<u8> = Vec::new();
				if grown.try_reserve_exact(data.len()).is_err() {
					return Err(FsError::NoMemory);
				}
				let would_be = self.footprint() as usize - held + grown.capacity();
				if would_be > self.capacity {
					return Err(FsError::NoSpace);
				}
				grown.extend_from_slice(data);
				let file = self.file_mut(parts)?;
				*file = grown;
				return Ok(());
			}
			file.clear();
			file.extend_from_slice(data);
			return Ok(());
		}
		// A new entry: the bytes are built before the entry exists, so a failure leaves the tree
		// untouched.
		let mut written: Vec<u8> = Vec::new();
		written.try_reserve_exact(data.len()).map_err(|_| FsError::NoMemory)?;
		written.extend_from_slice(data);
		let mut name = String::new();
		let last = parts.last().copied().ok_or(FsError::BadName)?;
		name.try_reserve_exact(last.len()).map_err(|_| FsError::NoMemory)?;
		name.push_str(last);
		let children = self.parent_mut(parts)?;
		insert(children, name, Node::File(written))
	}

	// The bytes of the file at `parts`, for rewriting in place.
	fn file_mut(&mut self, parts: &[&str]) -> Result<&mut Vec<u8>, FsError> {
		let name = *parts.last().ok_or(FsError::BadName)?;
		let children = self.parent_mut(parts)?;
		match find_mut(children, name) {
			Some(Node::File(data)) | Some(Node::Claimed(data)) => Ok(data),
			Some(Node::Directory(_)) => Err(FsError::IsDir),
			None => Err(FsError::NotFound),
		}
	}

	// List a directory. Fallible throughout: a directory at the entry limit with long names is a
	// megabyte of names, and `collect` would abort rather than refuse.
	pub fn list_entries(&self, path: &[u8]) -> Result<Vec<Entry>, FsError> {
		let (parts, count) = Self::segments(path)?;
		let parts = &parts[..count];
		match self.resolve(parts)? {
			Some(Node::Directory(children)) => {
				let mut out: Vec<Entry> = Vec::new();
				out.try_reserve_exact(children.len()).map_err(|_| FsError::NoMemory)?;
				for (name, node) in children {
					let mut copy = String::new();
					copy.try_reserve_exact(name.len()).map_err(|_| FsError::NoMemory)?;
					copy.push_str(name);
					// A DIRECTORY REPORTS ZERO, which is what the generated `Storage` documentation
					// says plainly and what the adapter passes straight to the client. This filled
					// the field with `node.bytes()` - the recursive size of everything underneath -
					// so a client reading the documented contract got a different quantity with no
					// way to tell. An occupied-bytes figure is a fine thing to want and wants its
					// own call.
					let size = if node.is_dir() { 0 } else { node.bytes() as u64 };
					out.push(Entry { name: copy, size, is_dir: node.is_dir() });
				}
				Ok(out)
			}
			Some(Node::File(_)) | Some(Node::Claimed(_)) => Err(FsError::NotDir),
			None => Err(FsError::NotFound),
		}
	}

	// Create one directory. Same ordering as `write_file`, and for the same reasons: everything
	// that can refuse is checked before anything is released.
	pub fn mkdir(&mut self, path: &[u8]) -> Result<(), FsError> {
		let (parts, count) = Self::segments(path)?;
		let parts = &parts[..count];
		let name = *parts.last().ok_or(FsError::BadName)?;
		// Resolves the parent too, so an absent or non-directory parent refuses here.
		if self.resolve(parts)?.is_some() {
			return Err(FsError::Exists);
		}
		// A directory stores nothing but still costs its name, and that name is memory the caller
		// asked for.
		if !self.room_for_an_entry(parts) || self.footprint() as usize + name.len() > self.capacity {
			return Err(FsError::NoSpace);
		}
		if self.policy == Policy::Reserved {
			self.release_reservation();
		}
		let made = self.make_dir(parts);
		self.resync_reservation();
		made
	}

	// Insert the directory, in its own call so its temporaries are gone before the resync.
	fn make_dir(&mut self, parts: &[&str]) -> Result<(), FsError> {
		let last = parts.last().copied().ok_or(FsError::BadName)?;
		let mut name = String::new();
		name.try_reserve_exact(last.len()).map_err(|_| FsError::NoMemory)?;
		name.push_str(last);
		let children = self.parent_mut(parts)?;
		insert(children, name, Node::Directory(Children::new()))
	}

	// Move a file's entry from one name to another, contents and all.
	//
	// THE BYTES NEVER MOVE: the node is taken out of one directory and put into another, so a
	// rename costs the two names and nothing else. That is what makes it atomic here as well as on
	// the disk - there is no window in which the file exists half-way, because the only mutation is
	// one table's entries.
	//
	// An EXISTING destination is refused, and a CLAIMED source or destination with it: a name an
	// open stream is filling is spoken for, and moving it out from under the stream is the same
	// defect `remove` refuses.
	pub fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<(), FsError> {
		let (source_parts, source_count) = Self::segments(from)?;
		let source_parts = &source_parts[..source_count];
		let source_name = *source_parts.last().ok_or(FsError::BadName)?;
		let (destination_parts, destination_count) = Self::segments(to)?;
		let destination_parts = &destination_parts[..destination_count];
		let destination_name = *destination_parts.last().ok_or(FsError::BadName)?;
		// EVERYTHING THAT CAN REFUSE, FIRST. The source has to be an ordinary file, the
		// destination has to be free, and the destination's parent has to exist - all asked
		// before the entry is taken out of the tree, because putting it back is a second failure
		// with nowhere to report it.
		//
		// BEFORE the same-path shortcut, which used to come first - so renaming something that is
		// not there to itself answered `Ok(())`.
		match self.resolve(source_parts)? {
			Some(Node::File(_)) => {}
			Some(Node::Directory(_)) => return Err(FsError::IsDir),
			Some(Node::Claimed(_)) => return Err(FsError::Exists),
			None => return Err(FsError::NotFound),
		}
		if source_parts == destination_parts {
			return Ok(());
		}
		if self.resolve(destination_parts)?.is_some() {
			return Err(FsError::Exists);
		}
		// AND THE NAME DELTA IS CHARGED. Names are part of `footprint()`, and this allocated the new
		// one and checked nothing - so on a volume exactly full, renaming `a` to `very-long-name`
		// succeeded as long as the host heap had room and left `footprint()` above `capacity()`,
		// which is the one invariant this milestone exists to hold.
		let footprint = self.footprint() as usize;
		if footprint - source_name.len() + destination_name.len() > self.capacity {
			return Err(FsError::NoSpace);
		}
		// AND THE METADATA CEILING, which a rename used to walk straight past.
		//
		// It does not change the node count, so `MAX_ENTRIES` has nothing to say - and that was
		// read as "no entry check applies". The destination's table still has to grow by one, and
		// growth is exactly what `MAX_METADATA_BYTES` bounds: a caller could rename entries between
		// directories to drive the retained tables past the ceiling one move at a time, through the
		// one mutator that never asked.
		//
		// Asked for the DESTINATION, whose table grows; the source's shrinks or stays, and neither
		// can push the total up.
		if !self.room_for_an_entry_by_bytes(destination_parts) {
			return Err(FsError::NoSpace);
		}
		// THE RESERVATION IS RELEASED BEFORE THE FIRST ALLOCATION, which every other mutator does
		// and this one did not.
		//
		// Under `Policy::Reserved` the volume holds its unused capacity as a real allocation. The
		// destination name and the destination table's growth were allocated with that reservation
		// STILL HELD, so a rename that fits the volume - and whose bytes were, by the policy's whole
		// claim, already set aside - could fail because the filesystem was sitting on the memory it
		// needed. Every preflight above has already run, so from here on the only refusals left are
		// allocation ones.
		if self.policy == Policy::Reserved {
			self.release_reservation();
		}
		// IN ITS OWN CALL, so the resync below runs on EVERY path out - the same shape `write_file`
		// uses, and for the same two reasons: every temporary the move makes is dropped before the
		// reservation is asked for again, and a refusal cannot leave the volume holding less than
		// its policy promises.
		let moved = self.rename_unsynced(source_parts, source_name, destination_parts, destination_name);
		self.resync_reservation();
		moved
	}

	// The allocating half of `rename`. Every preflight has already run, so the only refusals left
	// here are allocation ones - and the caller resyncs the reservation whichever way this ends.
	fn rename_unsynced(&mut self, source_parts: &[&str], source_name: &str, destination_parts: &[&str], destination_name: &str) -> Result<(), FsError> {
		let mut owned = String::new();
		owned.try_reserve_exact(destination_name.len()).map_err(|_| FsError::NoMemory)?;
		owned.push_str(destination_name);
		// The destination's table may have to grow by one, and that growth can be refused - so it
		// is done while the file is still where it was. `insert` reserves before it inserts, and
		// the entry it would displace was ruled out above.
		{
			let children = self.parent_mut(destination_parts)?;
			children.try_reserve(1).map_err(|_| FsError::NoMemory)?;
		}
		// `take` RATHER THAN `remove`: the shrink inside `remove` would hand the slot just reserved
		// back before the insert could use it, which for a rename within one directory is the same
		// table. See `take`.
		let node = {
			let children = self.parent_mut(source_parts)?;
			take(children, source_name).ok_or(FsError::NotFound)?
		};
		let children = self.parent_mut(destination_parts)?;
		insert(children, owned, node)?;
		// The source's table is shrunk AFTER the insert has succeeded, so the reservation is never
		// alive across a shrink of the table it is in.
		{
			let children = self.parent_mut(source_parts)?;
			shrink_children(children);
		}
		Ok(())
	}

	// Set a file's length: shorter drops the tail, longer extends with ZEROS.
	//
	// The zeros are the promise, not a side effect of how the buffer grows: a file extended this
	// way reads as zeros rather than as whatever the allocator last held there.
	pub fn truncate(&mut self, path: &[u8], length: u64) -> Result<(), FsError> {
		let (parts, count) = Self::segments(path)?;
		let parts = &parts[..count];
		let name = *parts.last().ok_or(FsError::BadName)?;
		// THE FILE-SIZE BOUND IS A PROPERTY OF THE VOLUME, not of which call reached it. `write_file`
		// and the streaming path both refuse past this; `truncate` bounded `want` by the volume's
		// capacity alone, so on a large volume a caller could build a file no other write path in
		// this crate would accept.
		//
		// COMPARED BEFORE THE NARROWING, and in a width that can hold the argument. This was
		// `usize::try_from(length).map_err(|_| FsError::NoSpace)?` and then the bound - so on a
		// 32-bit target a length past `usize::MAX` answered `NoSpace`, which says the volume is
		// full, when the truth is that no volume of any size would take it. `length` arrives as a
		// `u64` from an ABI that is the same width everywhere; the check that decides WHICH refusal
		// it is has to be too. No 32-bit target exists in this tree today, which is exactly why it
		// cost nothing to put right now rather than after one does.
		if length > MAX_FILE_BYTES as u64 {
			return Err(FsError::TooLarge);
		}
		// Cannot fail after the bound above - `MAX_FILE_BYTES` is a `usize` - but written fallibly
		// rather than with a cast, so raising the constant past a target's word size is a refusal
		// and not a truncation.
		let want = usize::try_from(length).map_err(|_| FsError::TooLarge)?;
		// WHAT THE FILE COSTS TODAY, AS AN ALLOCATION. This read `bytes.len()`, and the volume
		// accounts `capacity()` - so a file holding 20 bytes in an 80-byte buffer, already charged
		// for 80, was refused a grow to 40 that allocates nothing. That is the `len`-versus-
		// `capacity` confusion this milestone was opened for, in a function written after it closed.
		let have: usize = match self.resolve(parts)? {
			Some(Node::File(bytes)) => bytes.capacity(),
			Some(Node::Directory(_)) => return Err(FsError::IsDir),
			Some(Node::Claimed(_)) => return Err(FsError::Exists),
			None => return Err(FsError::NotFound),
		};
		let becomes = have.max(want);
		if self.footprint() as usize - have + becomes > self.capacity {
			return Err(FsError::NoSpace);
		}
		// ALLOCATE, CHECK, SWAP - the shape `write_file` uses, and the reason it uses it. Mutating
		// in place meant trusting what `try_reserve_exact` returned: the allocator promises AT LEAST
		// what was asked for, `footprint` counts what was actually given, and a generous allocator
		// left a successful truncate with the volume over its own capacity.
		//
		// It also meant a refused grow could return with the reservation dropped - released before
		// the allocation, with `resync_reservation()` after the `?`. That is exactly the defect the
		// previous round found and fixed in `write_file`, in the function beside it. Building the new
		// buffer first makes all three one problem with one answer.
		let allocates = becomes > have;
		if self.policy == Policy::Reserved && allocates {
			self.release_reservation();
		}
		let outcome = self.truncate_swap(parts, name, want, have);
		self.resync_reservation();
		outcome
	}

	// The mutating half of `truncate`, in its own call so every temporary it makes is dropped BEFORE
	// the caller resyncs its reservation - holding a refused buffer while asking for the reservation
	// back means competing with yourself for the memory.
	fn truncate_swap(&mut self, parts: &[&str], name: &str, want: usize, charged: usize) -> Result<(), FsError> {
		let capacity = self.capacity;
		let footprint = self.footprint() as usize;
		let children = self.parent_mut(parts)?;
		let Some(Node::File(bytes)) = find_mut(children, name) else {
			return Err(FsError::NotFound);
		};
		if want <= bytes.len() {
			bytes.truncate(want);
			// THROUGH THE FALLIBLE PATH, because `shrink_to_fit` aborts the storage service when the
			// smaller allocation fails - which is precisely why `shrink_children` next door is
			// hand-written rather than calling it. One shrink policy, one implementation.
			//
			// A shrink that cannot allocate leaves the buffer where it is: the file is the right
			// length and the volume is holding more than it needs, which is a cost rather than a
			// defect, and the resync below reports the truth either way.
			let mut smaller: Vec<u8> = Vec::new();
			// AND IT IS ADOPTED ONLY IF IT IS ACTUALLY SMALLER. `try_reserve_exact` promises AT
			// LEAST what was asked for, which is the sentence the grow path below is built around -
			// and this path took whatever came back on faith. A generous allocator answering a
			// shrink with a larger block than the buffer already had would have made a SHRINK raise
			// the volume's footprint. Measuring both sides is the same discipline, applied to the
			// same uncertainty, in the same function.
			if smaller.try_reserve_exact(want).is_ok() && smaller.capacity() < bytes.capacity() && footprint - charged + smaller.capacity() <= capacity {
				smaller.extend_from_slice(&bytes[..want]);
				*bytes = smaller;
			}
			return Ok(());
		}
		// LONGER THAN THE CONTENTS AND INSIDE THE BUFFER: no allocation at all.
		//
		// This branched on `len` alone, so a file holding 20 bytes in an 80-byte buffer built a
		// fresh 40-byte Vec to grow to 40 - and the caller had just computed, from `capacity`, that
		// the operation allocates nothing and therefore may keep its reservation. The outer function
		// and this one disagreed about whether an allocation was about to happen, and on a tight
		// heap the disagreement was a `NoSpace` for memory the volume was already holding.
		//
		// Extending in place is also what `write_file` does with the same buffer, which is where the
		// 20-in-80 shape comes from: a partial overwrite deliberately keeps the high-water
		// allocation. A grow that rebuilt it would hand back memory the volume is still charged for.
		if want <= bytes.capacity() {
			bytes.resize(want, 0);
			return Ok(());
		}
		// Growing past the buffer: build the whole new one, measure what the allocator actually
		// gave, and only then swap it in.
		let mut grown: Vec<u8> = Vec::new();
		grown.try_reserve_exact(want).map_err(|_| FsError::NoMemory)?;
		if footprint - charged + grown.capacity() > capacity {
			return Err(FsError::NoSpace);
		}
		grown.extend_from_slice(bytes);
		grown.resize(want, 0);
		*bytes = grown;
		Ok(())
	}

	// Create an empty file when asked and it is missing; otherwise leave the contents alone.
	//
	// This volume reports no timestamps - nothing here outlives the boot that made it - so there is
	// no mtime to stamp and `touch` over an existing file is a no-op that says the file is there.
	// Answering `invalid` instead would tell a caller the volume cannot do it, which is not true.
	pub fn touch(&mut self, path: &[u8], create: bool) -> Result<(), FsError> {
		let (parts, count) = Self::segments(path)?;
		let parts = &parts[..count];
		match self.resolve(parts)? {
			Some(Node::File(_)) => Ok(()),
			Some(Node::Directory(_)) => Err(FsError::IsDir),
			Some(Node::Claimed(_)) => Err(FsError::Exists),
			None if create => self.write_file(path, &[]),
			None => Err(FsError::NotFound),
		}
	}

	pub fn remove(&mut self, path: &[u8]) -> Result<(), FsError> {
		let (parts, count) = Self::segments(path)?;
		let parts = &parts[..count];
		let name = *parts.last().ok_or(FsError::BadName)?;
		let children = self.parent_mut(parts)?;
		match find(children, name) {
			Some(Node::Directory(_)) => Err(FsError::IsDir),
			// The name is held by an open stream. Removing it was how an abort came to delete
			// another client's file: the entry went away, the next writer recreated it, and the
			// abort then removed THAT.
			Some(Node::Claimed(_)) => Err(FsError::Exists),
			Some(Node::File(_)) => {
				remove(children, name);
				self.resync_reservation();
				Ok(())
			}
			None => Err(FsError::NotFound),
		}
	}

	// Remove an empty directory. A directory with entries is refused rather than removed
	// recursively: deleting a tree by naming its root is a different operation, and doing it
	// silently is how a caller loses more than it meant to.
	pub fn rmdir(&mut self, path: &[u8]) -> Result<(), FsError> {
		let (parts, count) = Self::segments(path)?;
		let parts = &parts[..count];
		let name = *parts.last().ok_or(FsError::BadName)?;
		let children = self.parent_mut(parts)?;
		match find(children, name) {
			Some(Node::Directory(entries)) if entries.is_empty() => {
				remove(children, name);
				// The name it held counted toward the footprint, so removing it gives those
				// bytes back and a reserved volume must take them into its reservation.
				self.resync_reservation();
				Ok(())
			}
			Some(Node::Directory(_)) => Err(FsError::NotEmpty),
			Some(Node::File(_)) | Some(Node::Claimed(_)) => Err(FsError::NotDir),
			None => Err(FsError::NotFound),
		}
	}
}

#[cfg(test)]
mod tests;
