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
//!   promise is that the memory can always be taken back: the reservation is one contiguous
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
	Directory(Children),
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
fn insert(children: &mut Children, name: String, node: Node) -> Result<(), FsError> {
	match children.binary_search_by(|(key, _)| key.as_str().cmp(name.as_str())) {
		Ok(at) => {
			children[at].1 = node;
			Ok(())
		}
		Err(at) => {
			children.try_reserve(1).map_err(|_| FsError::NoSpace)?;
			children.insert(at, (name, node));
			Ok(())
		}
	}
}

fn remove(children: &mut Children, name: &str) -> Option<Node> {
	let at = children.binary_search_by(|(key, _)| key.as_str().cmp(name)).ok()?;
	Some(children.remove(at).1)
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
			Node::File(data) => data.len(),
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
			Node::File(data) => data.capacity(),
			Node::Directory(children) => children.iter().map(|(_, node)| node.allocated()).sum(),
		}
	}

	// The bytes this node's subtree spends on NAMES. A name is memory the caller caused to be
	// allocated, so it counts against the capacity: without it a volume could be filled with
	// 4095 maximum-length names and exceed its capacity by a megabyte - a quarter of a 4 MiB
	// reserved volume, which would make "reserved" mean nothing.
	//
	// A node does not count its own name; the parent holding the key does.
	fn names(&self) -> usize {
		match self {
			Node::File(_) => 0,
			Node::Directory(children) => children.iter().map(|(name, node)| name.len() + node.names()).sum(),
		}
	}

	fn count(&self) -> usize {
		match self {
			Node::File(_) => 1,
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
			let mut allocation: Vec<u8> = Vec::new();
			allocation.try_reserve_exact(capacity).map_err(|_| FsError::NoSpace)?;
			reservation.try_reserve(1).map_err(|_| FsError::NoSpace)?;
			reservation.push(Chunk { allocation, bytes: capacity });
			reserved = capacity;
		}
		Ok(LiberMemFs { root: Node::Directory(Children::new()), policy, capacity, reservation, reserved })
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
		(self.root.allocated() + self.root.names()) as u64
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
	pub fn writable_len(&self, path: &[u8]) -> Result<usize, FsError> {
		let (parts, count) = Self::segments(path)?;
		let parts = &parts[..count];
		if parts.is_empty() {
			return Err(FsError::IsDir);
		}
		let free = self.capacity.saturating_sub(self.footprint() as usize);
		match self.resolve(parts)? {
			// A rewrite may use what the file already holds, plus whatever is still free.
			Some(Node::File(existing)) => Ok((existing.capacity() + free).min(MAX_FILE_BYTES)),
			Some(Node::Directory(_)) => Err(FsError::IsDir),
			// A new entry pays for its name out of the same free space.
			None => {
				if self.root.count() >= MAX_ENTRIES {
					return Err(FsError::NoSpace);
				}
				let name = parts.last().map_or(0, |name| name.len());
				Ok(free.saturating_sub(name).min(MAX_FILE_BYTES))
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
		for segment in text.split('/') {
			if segment.is_empty() {
				// Leading, trailing and doubled separators are tolerated: `/a/b/`, `a//b` and
				// `a/b` name the same thing, which is what every caller in this tree expects.
				continue;
			}
			if segment == "." || segment == ".." {
				return Err(FsError::BadName);
			}
			if segment.len() > MAX_NAME_BYTES {
				return Err(FsError::TooLong);
			}
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
			Node::File(_) => Err(FsError::NotDir),
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
			Node::File(_) => Err(FsError::NotDir),
		}
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
		self.reservation.push(Chunk { allocation, bytes });
		self.reserved += bytes;
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
			Some(Node::File(data)) => {
				let mut out: Vec<u8> = Vec::new();
				out.try_reserve_exact(data.len()).map_err(|_| FsError::NoSpace)?;
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
		if data.len() > MAX_FILE_BYTES {
			return Err(FsError::TooLong);
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
			Some(Node::File(existing)) => (existing.capacity(), existing.capacity().max(data.len()), false),
			None => {
				if self.root.count() >= MAX_ENTRIES {
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
		self.resync_reservation();
		stored
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
			let file = self.file_mut(parts)?;
			if data.len() > file.len() {
				file.try_reserve_exact(data.len() - file.len()).map_err(|_| FsError::NoSpace)?;
			}
			file.clear();
			file.extend_from_slice(data);
			return Ok(());
		}
		// A new entry: the bytes are built before the entry exists, so a failure leaves the tree
		// untouched.
		let mut written: Vec<u8> = Vec::new();
		written.try_reserve_exact(data.len()).map_err(|_| FsError::NoSpace)?;
		written.extend_from_slice(data);
		let mut name = String::new();
		let last = parts.last().copied().ok_or(FsError::BadName)?;
		name.try_reserve_exact(last.len()).map_err(|_| FsError::NoSpace)?;
		name.push_str(last);
		let children = self.parent_mut(parts)?;
		insert(children, name, Node::File(written))
	}

	// The bytes of the file at `parts`, for rewriting in place.
	fn file_mut(&mut self, parts: &[&str]) -> Result<&mut Vec<u8>, FsError> {
		let name = *parts.last().ok_or(FsError::BadName)?;
		let children = self.parent_mut(parts)?;
		match find_mut(children, name) {
			Some(Node::File(data)) => Ok(data),
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
				out.try_reserve_exact(children.len()).map_err(|_| FsError::NoSpace)?;
				for (name, node) in children {
					let mut copy = String::new();
					copy.try_reserve_exact(name.len()).map_err(|_| FsError::NoSpace)?;
					copy.push_str(name);
					out.push(Entry { name: copy, size: node.bytes() as u64, is_dir: node.is_dir() });
				}
				Ok(out)
			}
			Some(Node::File(_)) => Err(FsError::NotDir),
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
		if self.root.count() >= MAX_ENTRIES || self.footprint() as usize + name.len() > self.capacity {
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
		name.try_reserve_exact(last.len()).map_err(|_| FsError::NoSpace)?;
		name.push_str(last);
		let children = self.parent_mut(parts)?;
		insert(children, name, Node::Directory(Children::new()))
	}

	pub fn remove(&mut self, path: &[u8]) -> Result<(), FsError> {
		let (parts, count) = Self::segments(path)?;
		let parts = &parts[..count];
		let name = *parts.last().ok_or(FsError::BadName)?;
		let children = self.parent_mut(parts)?;
		match find(children, name) {
			Some(Node::Directory(_)) => Err(FsError::IsDir),
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
			Some(Node::File(_)) => Err(FsError::NotDir),
			None => Err(FsError::NotFound),
		}
	}
}

#[cfg(test)]
mod tests;
