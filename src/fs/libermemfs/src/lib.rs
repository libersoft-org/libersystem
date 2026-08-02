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
//! - `Policy::Reserved` takes its whole capacity at mount. Mounting fails if the memory is not
//!   available, and afterwards a write cannot fail for want of memory that something else took.
//! - `Policy::Capped` takes memory as files are written and refuses past the limit. Nothing is
//!   held that is not used, and the limit is a ceiling rather than a reservation.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use fscore::FsError;

// Bounds. Every other bounded resource in this tree refuses rather than truncates, and a
// filesystem that can exhaust the kernel heap is a denial of service whatever it is called.
pub const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 4096;
pub const MAX_NAME_BYTES: usize = 255;
pub const MAX_PATH_DEPTH: usize = 16;

// When a volume's memory is charged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
	// Charged at mount: the capacity is held whether or not it is used.
	Reserved,
	// Charged at write: only what is stored is held, up to the capacity.
	Capped,
}

// One entry in a directory. Files own their bytes; directories own their children.
enum Node {
	File(Vec<u8>),
	Directory(BTreeMap<String, Node>),
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
			Node::Directory(children) => children.values().map(Node::bytes).sum(),
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
			Node::Directory(children) => 1 + children.values().map(Node::count).sum::<usize>(),
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
	reservation: Vec<u8>,
}

impl LiberMemFs {
	// Mount an empty volume. A reserved volume takes its capacity here, so this is where it
	// fails when the memory is not available; a capped volume always mounts.
	pub fn mount(policy: Policy, capacity: usize) -> Result<LiberMemFs, FsError> {
		let mut reservation = Vec::new();
		if policy == Policy::Reserved {
			// `try_reserve` rather than a plain allocation: running out here must be an error
			// the caller can report, not an abort inside the storage service.
			reservation.try_reserve_exact(capacity).map_err(|_| FsError::NoSpace)?;
			reservation.resize(capacity, 0);
		}
		Ok(LiberMemFs { root: Node::Directory(BTreeMap::new()), policy, capacity, reservation })
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
		(self.root.bytes() + self.root.names()) as u64
	}

	// The bytes a reserved volume is holding but not yet storing. Zero for a capped volume,
	// which holds nothing it is not using. Exposed because the reservation is the whole
	// difference between the two policies and a test cannot otherwise see it.
	pub fn reserved_bytes(&self) -> u64 {
		self.reservation.len() as u64
	}

	// What can still be written, after both the data and the names already held. Reported rather
	// than `capacity - used`, which would promise room that the next name will take.
	pub fn free(&self) -> u64 {
		self.capacity().saturating_sub(self.footprint())
	}

	// Split a path into its segments, rejecting everything that is not a plain relative path.
	// `.` and `..` are refused rather than resolved: this filesystem has no working directory
	// and no use for either, and accepting `..` would be the one way to name something outside
	// the volume.
	fn segments(path: &[u8]) -> Result<Vec<&str>, FsError> {
		let text = core::str::from_utf8(path).map_err(|_| FsError::BadName)?;
		let mut parts: Vec<&str> = Vec::new();
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
			parts.push(segment);
			// Refused here rather than after the loop, so this vector never holds more than the
			// depth limit. Checking at the end still returns the same error, but only after a path
			// of a thousand segments has been parsed into a thousand entries - an allocation the
			// caller sizes, charged to nothing, on every operation including reads. The limit is
			// meant to bound the work, not just describe the result.
			if parts.len() > MAX_PATH_DEPTH {
				return Err(FsError::TooLong);
			}
		}
		Ok(parts)
	}

	fn lookup(&self, parts: &[&str]) -> Option<&Node> {
		let mut node = &self.root;
		for part in parts {
			let Node::Directory(children) = node else { return None };
			node = children.get(*part)?;
		}
		Some(node)
	}

	// Walk to the parent of `parts`, creating nothing. Returns the parent directory and the
	// final name.
	fn parent_mut<'a>(&'a mut self, parts: &[&'a str]) -> Result<(&'a mut BTreeMap<String, Node>, &'a str), FsError> {
		let (name, directories) = parts.split_last().ok_or(FsError::BadName)?;
		let mut node = &mut self.root;
		for part in directories {
			let Node::Directory(children) = node else { return Err(FsError::NotDir) };
			node = children.get_mut(*part).ok_or(FsError::NotFound)?;
		}
		match node {
			Node::Directory(children) => Ok((children, *name)),
			Node::File(_) => Err(FsError::NotDir),
		}
	}

	// Hold exactly the unused part of a reserved volume's capacity, so its footprint stays at
	// what it took at mount however its contents change.
	//
	// It has to run after EVERY mutation, not just after a write that grows a file. An earlier
	// version only ever shrank the reservation, so deleting or shrinking a file handed that
	// memory back to the heap rather than back to the reservation - and a volume that was
	// supposed to guarantee its space silently stopped guaranteeing it.
	fn resync_reservation(&mut self) {
		if self.policy != Policy::Reserved {
			return;
		}
		let target = self.capacity.saturating_sub(self.footprint() as usize);
		if self.reservation.len() == target {
			return;
		}
		// Dropped and reallocated rather than resized in place, in both directions.
		//
		// Resizing looks cheaper and is not. `shrink_to_fit` reallocates and COPIES the bytes it
		// keeps, so every write to a reserved volume would memcpy what remains of the
		// reservation - megabytes per write on a volume of any size, to preserve zeros that
		// nothing ever reads. Dropping first also means the old block is gone before the new one
		// is asked for, so the two are never outstanding together: the same rule the write path
		// follows, and for the same reason.
		//
		// Best effort on the way back up: `Vec::resize` would ABORT on allocation failure, which
		// in a storage service is a crash where a degraded guarantee would do. If the memory
		// cannot be had the volume holds less than its capacity and `reserved_bytes` says so.
		self.reservation = Vec::new();
		let mut fresh: Vec<u8> = Vec::new();
		if fresh.try_reserve_exact(target).is_ok() {
			fresh.resize(target, 0);
			self.reservation = fresh;
		}
	}

	pub fn read_file(&mut self, path: &[u8]) -> Result<Vec<u8>, FsError> {
		let parts = Self::segments(path)?;
		match self.lookup(&parts) {
			Some(Node::File(data)) => Ok(data.clone()),
			Some(Node::Directory(_)) => Err(FsError::IsDir),
			None => Err(FsError::NotFound),
		}
	}

	// Write a whole file, creating or replacing it.
	pub fn write_file(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
		if data.len() > MAX_FILE_BYTES {
			return Err(FsError::TooLong);
		}
		let parts = Self::segments(path)?;
		// What this name holds today, if anything. Read before anything is changed, because the
		// capacity check and the reservation release below both need it and neither may run
		// against a half-modified tree.
		let (previous, is_new) = match self.lookup(&parts) {
			Some(Node::Directory(_)) => return Err(FsError::IsDir),
			Some(Node::File(existing)) => (existing.len(), false),
			None => {
				if self.root.count() >= MAX_ENTRIES {
					return Err(FsError::NoSpace);
				}
				(0, true)
			}
		};
		// Subtracting what the file already holds is what lets it be rewritten on a full volume:
		// replacing does not need room for both copies. The check is the same for both policies;
		// only where the memory comes from differs.
		// A new entry also costs its name; replacing an existing one does not, because the name is
		// already there and already counted.
		let name_cost = if is_new { parts.last().map_or(0, |name| name.len()) } else { 0 };
		let footprint = self.footprint() as usize;
		if footprint - previous + data.len() + name_cost > self.capacity {
			return Err(FsError::NoSpace);
		}
		// A reserved volume releases what it is holding BEFORE allocating the file. Otherwise the
		// allocation and the reservation are outstanding at the same time and the write can fail
		// for memory the volume is itself sitting on - precisely the failure a reservation exists
		// to prevent.
		//
		// All of it, not just the difference: the whole point is that the bytes are certainly
		// available, and releasing exactly enough leaves that resting on the reservation being
		// perfectly in step, which it is not after a best-effort regrow has fallen short. The
		// resync at the end puts back whatever the volume should still be holding.
		if self.policy == Policy::Reserved {
			self.reservation = Vec::new();
		}
		// The bytes are built before the entry is replaced. Inserting an empty file first and
		// filling it afterwards loses the previous contents when the allocation fails - a failed
		// write must leave what was there untouched, not truncate it to nothing.
		let mut written: Vec<u8> = Vec::new();
		if written.try_reserve_exact(data.len()).is_err() {
			// The release above is undone on the way out, so a refused write leaves the volume
			// holding exactly what it held before it was attempted.
			self.resync_reservation();
			return Err(FsError::NoSpace);
		}
		written.extend_from_slice(data);
		// `parent_mut` can still refuse - the parent may not exist, or a path component may be a
		// file - and it runs AFTER the reservation was released. Returning through `?` here would
		// walk past the resync and leave the volume holding less than its capacity for the rest
		// of its life, with nothing stored to account for it. Every exit from this point on goes
		// through the resync.
		match self.parent_mut(&parts) {
			Ok((children, name)) => {
				children.insert(String::from(name), Node::File(written));
			}
			Err(error) => {
				self.resync_reservation();
				return Err(error);
			}
		}
		self.resync_reservation();
		Ok(())
	}

	pub fn list_entries(&mut self, path: &[u8]) -> Result<Vec<Entry>, FsError> {
		let parts = Self::segments(path)?;
		match self.lookup(&parts) {
			Some(Node::Directory(children)) => Ok(children.iter().map(|(name, node)| Entry { name: name.clone(), size: node.bytes() as u64, is_dir: node.is_dir() }).collect()),
			Some(Node::File(_)) => Err(FsError::NotDir),
			None => Err(FsError::NotFound),
		}
	}

	pub fn mkdir(&mut self, path: &[u8]) -> Result<(), FsError> {
		let parts = Self::segments(path)?;
		// A directory stores nothing but still costs its name, and that name is memory the caller
		// asked for. Charging it is what stops a volume being filled past its capacity with
		// directories - `mkdir` had no capacity check at all.
		//
		// Every check is made before anything is released, so a refusal costs nothing.
		let name = *parts.last().ok_or(FsError::BadName)?;
		if self.lookup(&parts).is_some() {
			return Err(FsError::Exists);
		}
		if self.root.count() >= MAX_ENTRIES || self.footprint() as usize + name.len() > self.capacity {
			return Err(FsError::NoSpace);
		}
		// The name is an allocation like any other, so a reserved volume releases before making
		// it - the same rule the write path follows, for the same reason. Since names now count
		// toward the footprint, skipping this would leave the reservation permanently ahead of
		// what the volume actually holds.
		if self.policy == Policy::Reserved {
			self.reservation = Vec::new();
		}
		let inserted = match self.parent_mut(&parts) {
			Ok((children, name)) => {
				children.insert(String::from(name), Node::Directory(BTreeMap::new()));
				Ok(())
			}
			Err(error) => Err(error),
		};
		self.resync_reservation();
		inserted
	}

	pub fn remove(&mut self, path: &[u8]) -> Result<(), FsError> {
		let parts = Self::segments(path)?;
		let (children, name) = self.parent_mut(&parts)?;
		match children.get(name) {
			Some(Node::Directory(_)) => Err(FsError::IsDir),
			Some(Node::File(_)) => {
				children.remove(name);
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
		let parts = Self::segments(path)?;
		let (children, name) = self.parent_mut(&parts)?;
		match children.get(name) {
			Some(Node::Directory(entries)) if entries.is_empty() => {
				children.remove(name);
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
