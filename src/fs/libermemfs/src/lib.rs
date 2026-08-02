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
	// Held only by a reserved volume: the memory taken at mount so a later write cannot fail
	// for want of it. It is never read - owning it IS the reservation - and it shrinks as files
	// are written so the volume's total footprint stays at the capacity rather than doubling.
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

	pub fn used(&self) -> u64 {
		self.root.bytes() as u64
	}

	// The bytes a reserved volume is holding but not yet storing. Zero for a capped volume,
	// which holds nothing it is not using. Exposed because the reservation is the whole
	// difference between the two policies and a test cannot otherwise see it.
	pub fn reserved_bytes(&self) -> u64 {
		self.reservation.len() as u64
	}

	pub fn free(&self) -> u64 {
		self.capacity().saturating_sub(self.used())
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
		}
		if parts.len() > MAX_PATH_DEPTH {
			return Err(FsError::TooLong);
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
	// It has to run after EVERY mutation, not just after a write that grows a file. The first
	// version only ever shrank the reservation, so deleting or shrinking a file handed that
	// memory back to the heap rather than back to the reservation - and a volume that was
	// supposed to guarantee its space silently stopped guaranteeing it.
	//
	// Growing back is safe: the bytes were released by this same call path a moment earlier.
	fn resync_reservation(&mut self) {
		if self.policy != Policy::Reserved {
			return;
		}
		let target = self.capacity.saturating_sub(self.used() as usize);
		let held = self.reservation.len();
		if target <= held {
			// Giving memory up always works. Released rather than merely shortened, so the
			// volume's footprint is what it stores plus what it still holds, not both at their
			// high-water marks.
			self.reservation.truncate(target);
			self.reservation.shrink_to_fit();
			return;
		}
		// Taking it back can fail, and `resize` would ABORT rather than report it - which in a
		// storage service is a crash where a degraded guarantee would do. Best effort: reserve
		// what is available, and if less comes back than was released, the volume holds less
		// than its capacity and `reserved_bytes` says so. A caller that cares can see it;
		// nothing is corrupted either way.
		if self.reservation.try_reserve_exact(target - held).is_ok() {
			self.reservation.resize(target, 0);
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

	// Write a whole file, creating or replacing it. Replacing frees the old bytes first, so
	// rewriting a file at the capacity succeeds rather than needing room for both copies.
	pub fn write_file(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
		if data.len() > MAX_FILE_BYTES {
			return Err(FsError::TooLong);
		}
		let parts = Self::segments(path)?;
		let entries = self.root.count();
		let used = self.used() as usize;
		let capacity = self.capacity;
		let policy = self.policy;
		// The tree mutation happens in its own scope so its borrow of `self` ends before the
		// reservation below is touched - two disjoint borrows expressed as two statements
		// rather than as an unsafe aliasing trick.
		let previous = {
			let (children, name) = self.parent_mut(&parts)?;
			let previous = match children.get(name) {
				Some(Node::Directory(_)) => return Err(FsError::IsDir),
				Some(Node::File(existing)) => existing.len(),
				None => {
					if entries >= MAX_ENTRIES {
						return Err(FsError::NoSpace);
					}
					0
				}
			};
			// The capacity check is the same for both policies - what differs is only whether the
			// memory was already taken at mount. Subtracting `previous` first is what lets a file be
			// rewritten at a full volume: replacing does not need room for both copies.
			let after = used - previous + data.len();
			if after > capacity {
				return Err(FsError::NoSpace);
			}
			// The bytes are built BEFORE the entry is replaced. Inserting an empty file first and
			// filling it afterwards loses the previous contents when the allocation fails - a
			// failed write must leave what was there untouched, not truncate it to nothing.
			let mut written: Vec<u8> = Vec::new();
			written.try_reserve_exact(data.len()).map_err(|_| FsError::NoSpace)?;
			written.extend_from_slice(data);
			children.insert(String::from(name), Node::File(written));
			previous
		};
		let _ = previous;
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
		let entries = self.root.count();
		let (children, name) = self.parent_mut(&parts)?;
		if children.contains_key(name) {
			return Err(FsError::Exists);
		}
		if entries >= MAX_ENTRIES {
			return Err(FsError::NoSpace);
		}
		children.insert(String::from(name), Node::Directory(BTreeMap::new()));
		Ok(())
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
