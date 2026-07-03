use crate::*;

impl<D: BlockDevice> LiberFs<D> {
	// Verify integrity. With copy-on-write a crash cannot leak blocks or orphan an
	// inode (the free map is derived and a commit is atomic), so there is nothing to
	// reclaim; what fsck does is walk the live namespace, check every file's data
	// blocks against their stored checksums, and NAME the damaged files - a count
	// alone would leave the operator knowing something is wrong but not what. The
	// pinned snapshot generations are verified too - inode trees, directory trees and
	// file data (counted; their files are named under the snapshot's own mount, not
	// here). Damage is REPORTED, never fatal - a corrupt node and an unreadable block
	// alike count as failures (named by path where one is known) and the walk
	// continues, so one bad block cannot silence the report of everything else. The
	// free map is rederived too (a no-op on a consistent volume); damage found THERE
	// additionally degrades the volume to read-only, the mount's own policy - the map
	// is incomplete, so no later allocation may trust it (a remount after repair
	// restores writes).
	pub fn fsck(&mut self) -> Result<FsckReport, FsError> {
		// verify the DISK, not the caches: a cached inode would skip its tree-path and
		// spill-chain verification, and a cached checksum block its re-read - damage
		// behind a warm cache would escape the report.
		self.icache.clear();
		self.dcache.clear();
		self.rcsum = None;
		self.decomp = None;
		let mut checksum_failures = 0u32;
		let mut damaged: Vec<Vec<u8>> = Vec::new();
		match self.derive_free() {
			Ok(()) => {}
			Err(FsError::Corrupt) => {
				checksum_failures = checksum_failures.saturating_add(1);
				self.read_only = true;
			}
			Err(e) => return Err(e),
		}
		// walk the live namespace from the root, tracking each file's full path (the
		// root directory itself reports as "/"). The visited set makes a hostile
		// namespace - a cycle, or many names aliasing one subtree - terminate instead
		// of looping or blowing up.
		let mut stack: Vec<(u32, Vec<u8>)> = vec![(self.root_inode, Vec::new())];
		let mut seen: BTreeSet<u32> = BTreeSet::new();
		seen.insert(self.root_inode);
		while let Some((dir, prefix)) = stack.pop() {
			let entries = match self.dir_entries_of(dir) {
				Ok(entries) => entries,
				// Invalid covers a dangling walk target: a directory entry (or the
				// superblock's root_inode) naming an inode that does not exist -
				// structural damage like any other.
				Err(FsError::Corrupt | FsError::Io | FsError::Invalid) => {
					checksum_failures = checksum_failures.saturating_add(1);
					damaged.push(if prefix.is_empty() { b"/".to_vec() } else { prefix });
					continue;
				}
				Err(e) => return Err(e),
			};
			for (name, child) in entries {
				let mut path = prefix.clone();
				if !path.is_empty() {
					path.push(b'/');
				}
				path.extend_from_slice(&name);
				let checked = self.read_inode(child).and_then(|inode| {
					if inode.kind == KIND_DIR {
						if seen.insert(child) {
							stack.push((child, path.clone()));
						}
						Ok(0)
					} else {
						self.count_corrupt(&inode)
					}
				});
				let bad = match checked {
					Ok(bad) => bad,
					// an unreadable block (a hostile out-of-pool pointer fails its read as
					// Io) and a dangling entry (Invalid: the inode does not exist) are
					// damage to the operator, exactly like a checksum mismatch.
					Err(FsError::Corrupt | FsError::Io | FsError::Invalid) => 1,
					Err(e) => return Err(e),
				};
				if bad > 0 {
					// saturating: a count past u32 reads as "beyond counting", which such
					// a volume is - never an overflow in the report's own arithmetic.
					checksum_failures = checksum_failures.saturating_add(bad);
					damaged.push(path);
				}
			}
		}
		// every pinned snapshot generation is part of the live volume: verify its
		// blocks too, so corruption in a snapshot is reported and the walk accounts
		// for it.
		for i in 0..self.snapshots.len() {
			let (root, crc) = (self.snapshots[i].inode_root, self.snapshots[i].inode_root_crc);
			checksum_failures = checksum_failures.saturating_add(match self.check_inode_tree(root, crc, TREE_DEPTH_MAX) {
				Ok(bad) => bad,
				Err(FsError::Corrupt | FsError::Io) => 1,
				Err(e) => return Err(e),
			});
		}
		Ok(FsckReport { checksum_failures, damaged })
	}

	// Read the whole file at `path` out of the named snapshot's pinned generation,
	// without mounting a second filesystem: a table lookup re-roots the read through
	// `with_root`, so the cost is the file's, not a volume walk. The one-file read
	// behind the service's snap-open.
	pub fn read_file_from_snapshot(&mut self, snapshot: &[u8], path: &[u8]) -> Result<Vec<u8>, FsError> {
		let snap = self.snapshots.iter().find(|s| s.name == snapshot).ok_or(FsError::NotFound)?;
		let (root, crc) = (snap.inode_root, snap.inode_root_crc);
		self.with_root(root, crc, |fs| fs.read_file(path))
	}

	// Copy the file at `path` out of a pinned generation into the live tree: the
	// recovery verb for a file fsck named. `snapshot` picks a named snapshot; an empty
	// name picks the previous generation (the rolling one-commit-back snapshot). The
	// restored bytes are the generation's version of the file - explicitly an older
	// version, the operator's call. Under copy-on-write the two generations usually
	// share the damaged block, so this heals only what the pinned generation still
	// holds intact (a block rewritten since diverged; a shared one is damaged in both).
	pub fn restore_file(&mut self, path: &[u8], snapshot: &[u8]) -> Result<(), FsError> {
		let (root, crc) = if snapshot.is_empty() {
			if !self.prev_valid {
				return Err(FsError::NotFound);
			}
			(self.prev_inode_root, self.prev_inode_root_crc)
		} else {
			let snap = self.snapshots.iter().find(|s| s.name == snapshot).ok_or(FsError::NotFound)?;
			(snap.inode_root, snap.inode_root_crc)
		};
		let data = self.with_root(root, crc, |fs| fs.read_file(path))?;
		self.write_file(path, &data)
	}

	// Run `f` with the inode tree re-rooted at (`root`, `crc`) - a read within a pinned
	// generation - then restore the live root. The caches are cleared on the way in and
	// out, since they describe whichever root is current.
	pub(crate) fn with_root<R>(&mut self, root: u64, crc: u32, f: impl FnOnce(&mut Self) -> R) -> R {
		let saved = (self.inode_root, self.inode_root_crc);
		self.inode_root = root;
		self.inode_root_crc = crc;
		self.icache.clear();
		self.dcache.clear();
		let r = f(self);
		self.inode_root = saved.0;
		self.inode_root_crc = saved.1;
		self.icache.clear();
		self.dcache.clear();
		r
	}

	// Walk the inode B+tree, verifying every node against its stored checksum, and sum
	// the corrupt data blocks of every live file. Directory inodes get their own tree
	// walked and verified too, so a snapshot generation's directory damage is caught
	// here and not only when the snapshot is mounted. A corrupt subtree counts as a
	// failure and the walk continues; only the root's own damage surfaces as the error
	// (the caller counts it). The depth budget bounds the recursion against a hostile
	// chain of one-child internals.
	pub(crate) fn check_inode_tree(&mut self, ptr: u64, crc: u32, depth: usize) -> Result<u32, FsError> {
		if ptr == 0 {
			return Ok(0);
		}
		if depth == 0 {
			return Err(FsError::Corrupt);
		}
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node(ptr, crc, &mut buf)?;
		let mut bad = 0u32;
		if node_type(&buf) == NODE_LEAF {
			for i in 0..leaf_count(&buf, INODE_REC) {
				let off = NODE_HDR + i * INODE_REC + 8;
				let mut inode = Inode::parse(&buf[off..off + INODE_SIZE]);
				let checked = if inode.kind == KIND_FILE {
					self.load_spill(&mut inode).and_then(|()| self.count_corrupt(&inode))
				} else if inode.kind == KIND_DIR {
					self.check_dir_tree(inode.dir_root, inode.dir_root_crc, TREE_DEPTH_MAX)
				} else {
					Ok(0)
				};
				bad = bad.saturating_add(match checked {
					Ok(b) => b,
					Err(FsError::Corrupt | FsError::Io) => 1,
					Err(e) => return Err(e),
				});
			}
		} else {
			for i in 0..=internal_count(&buf) {
				bad = bad.saturating_add(match self.check_inode_tree(child_ptr(&buf, i), child_crc(&buf, i), depth - 1) {
					Ok(b) => b,
					Err(FsError::Corrupt | FsError::Io) => 1,
					Err(e) => return Err(e),
				});
			}
		}
		Ok(bad)
	}

	// Walk a directory B+tree verifying every node against the CRC32C its parent link
	// stored, counting corrupt subtrees like `check_inode_tree`; only the root's own
	// damage surfaces as the error (the caller counts it).
	pub(crate) fn check_dir_tree(&mut self, ptr: u64, crc: u32, depth: usize) -> Result<u32, FsError> {
		if ptr == 0 {
			return Ok(0);
		}
		if depth == 0 {
			return Err(FsError::Corrupt);
		}
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node(ptr, crc, &mut buf)?;
		let mut bad = 0u32;
		if node_type(&buf) == NODE_INTERNAL {
			for i in 0..=internal_count(&buf) {
				bad = bad.saturating_add(match self.check_dir_tree(child_ptr(&buf, i), child_crc(&buf, i), depth - 1) {
					Ok(b) => b,
					Err(FsError::Corrupt | FsError::Io) => 1,
					Err(e) => return Err(e),
				});
			}
		}
		Ok(bad)
	}
}
