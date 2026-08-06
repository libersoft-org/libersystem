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
		self.decomp.clear();
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
					if inode.r#type == TYPE_DIR {
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
		// one visited map for every tree walked below: a block belongs to one node of one
		// tree, and verifying it a second time reports nothing new. Snapshots sharing
		// subtrees with each other is the normal case, so this is also most of the work
		// saved on a healthy volume.
		let mut visited = vec![0u8; self.free.len()];
		for i in 0..self.snapshots.len() {
			let (root, crc) = (self.snapshots[i].inode_root, self.snapshots[i].inode_root_crc);
			checksum_failures = checksum_failures.saturating_add(match self.check_inode_tree(root, crc, TREE_DEPTH_MAX, &mut visited) {
				Ok(bad) => bad,
				Err(FsError::Corrupt | FsError::Io) => 1,
				Err(e) => return Err(e),
			});
		}
		// the structural pass: shapes that no checksum can object to.
		let mut faults: Vec<Vec<u8>> = Vec::new();
		self.check_structure(&mut faults);
		Ok(FsckReport { checksum_failures, damaged, structural_failures: faults.len() as u32, faults })
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
	// Everything a checksum cannot see. Each fault is recorded and the pass CONTINUES,
	// so one broken inode does not hide the rest - the caller gets the whole list, and an
	// empty list is the only clean answer.
	//
	// This is deliberately separate from the checksum scrub above. The two answer
	// different questions - "did the medium give back what was written" versus "can what
	// was written be true" - and an operator needs to know which one failed.
	pub(crate) fn check_structure(&mut self, faults: &mut Vec<Vec<u8>>) {
		let note = |what: &str, faults: &mut Vec<Vec<u8>>| faults.push(what.as_bytes().to_vec());

		// the root of the namespace has to be a directory, or no path resolves.
		match self.read_inode(self.root_inode) {
			Ok(inode) if inode.r#type == TYPE_DIR => {}
			Ok(_) => note("root inode is not a directory", faults),
			Err(_) => note("root inode cannot be read", faults),
		}

		// the snapshot table as it is ON DISK, not as the mount remembers it. Nothing
		// re-read it, so a table that had drifted from memory since the mount - or one
		// whose records were impossible - went unreported by the very tool asked to check.
		let (root, crc) = (self.snap_root, self.snap_root_crc);
		match self.read_snapshot_table(root, crc, &mut |_| {}) {
			Ok(disk) => {
				if disk.len() != self.snapshots.len() || disk.iter().zip(self.snapshots.iter()).any(|(a, b)| a.name != b.name || a.inode_root != b.inode_root || a.generation != b.generation) {
					note("the snapshot table on disk differs from the mounted one", faults);
				}
			}
			Err(_) => note("the snapshot table cannot be re-read", faults),
		}

		// every inode in the tree, checked as a shape and collected so the ones no name
		// reaches can be named.
		let mut inodes: BTreeSet<u32> = BTreeSet::new();
		let mut seen_blocks = vec![0u8; self.free.len()];
		if self.walk_inode_records(self.inode_root, self.inode_root_crc, TREE_DEPTH_MAX, &mut inodes, &mut seen_blocks, faults).is_err() {
			note("the inode tree could not be walked to the end", faults);
		}

		// reachability: every inode record should be findable from the root by name.
		// An orphan is live to the free-map walk and invisible to a namespace check,
		// which is exactly the pair that hides a whole subtree from an operator.
		let mut reached: BTreeSet<u32> = BTreeSet::new();
		let mut stack: Vec<u32> = vec![self.root_inode];
		reached.insert(self.root_inode);
		while let Some(dir) = stack.pop() {
			let entries = match self.dir_entries_of(dir) {
				Ok(entries) => entries,
				Err(_) => continue,
			};
			for (_, child) in entries {
				if !reached.insert(child) {
					continue;
				}
				if matches!(self.read_inode(child), Ok(i) if i.r#type == TYPE_DIR) {
					stack.push(child);
				}
			}
		}
		let orphans = inodes.difference(&reached).count();
		if orphans != 0 {
			faults.push(alloc::format!("{orphans} inode record(s) reachable from no name").into_bytes());
		}
	}

	// Walk every leaf of the inode tree, checking the shape of each node and each record
	// it holds. Keys must ascend strictly within a leaf, a block must appear once, and
	// each file's extent map must be ordered, non-overlapping and as long as it claims.
	fn walk_inode_records(&mut self, ptr: u64, crc: u32, depth: usize, inodes: &mut BTreeSet<u32>, seen: &mut [u8], faults: &mut Vec<Vec<u8>>) -> Result<(), FsError> {
		if ptr == 0 {
			return Ok(());
		}
		if depth == 0 || ptr >= self.num_blocks {
			return Err(FsError::Corrupt);
		}
		if test_bit(seen, ptr) {
			faults.push(alloc::format!("block {ptr} appears twice in the inode tree").into_bytes());
			return Ok(());
		}
		set_bit(seen, ptr);
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node(ptr, crc, &mut buf)?;
		if node_type(&buf) == NODE_LEAF {
			// the record count is read off the medium, so a leaf claiming more records
			// than fit is not a leaf this format can produce.
			if node_count(&buf) > (BLOCK_SIZE - NODE_HDR) / INODE_REC {
				faults.push(alloc::format!("inode leaf {ptr} claims more records than it can hold").into_bytes());
			}
			let mut last: Option<u64> = None;
			for i in 0..leaf_count(&buf, INODE_REC) {
				let off = NODE_HDR + i * INODE_REC;
				let key = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
				if last.is_some_and(|l| key <= l) {
					faults.push(alloc::format!("inode leaf {ptr} is not in ascending key order").into_bytes());
				}
				last = Some(key);
				if key > u32::MAX as u64 {
					faults.push(alloc::format!("inode leaf {ptr} holds a key outside the inode number range").into_bytes());
					continue;
				}
				inodes.insert(key as u32);
				let mut inode = Inode::parse(&buf[off + 8..off + 8 + INODE_SIZE]);
				if inode.r#type == TYPE_FILE {
					self.check_file_shape(key as u32, &mut inode, faults);
				}
			}
		} else {
			if node_count(&buf) > INTERNAL_MAX - 1 {
				faults.push(alloc::format!("internal node {ptr} claims more children than it can hold").into_bytes());
			}
			let count = internal_count(&buf);
			let mut last: Option<u64> = None;
			for i in 0..count {
				let sep = sep_key(&buf, i);
				if last.is_some_and(|l| sep <= l) {
					faults.push(alloc::format!("internal node {ptr} has separators out of order").into_bytes());
				}
				last = Some(sep);
			}
			for i in 0..=count {
				if self.walk_inode_records(child_ptr(&buf, i), child_crc(&buf, i), depth - 1, inodes, seen, faults).is_err() {
					faults.push(alloc::format!("subtree under {ptr} could not be walked").into_bytes());
				}
			}
		}
		Ok(())
	}

	// One file's extent map: as long as the inode claims, ordered by logical offset,
	// non-overlapping, and every run individually possible.
	fn check_file_shape(&mut self, num: u32, inode: &mut Inode, faults: &mut Vec<Vec<u8>>) {
		if self.load_spill(inode).is_err() {
			faults.push(alloc::format!("inode {num}: the extent spill chain could not be read").into_bytes());
			return;
		}
		if inode.extents.len() != inode.extent_count as usize {
			faults.push(alloc::format!("inode {num}: the spill chain ends before the declared extent count").into_bytes());
		}
		let mut end: Option<u64> = None;
		for ext in inode.extents.iter() {
			if self.check_extent(ext).is_err() {
				faults.push(alloc::format!("inode {num}: an extent that cannot be true").into_bytes());
				continue;
			}
			if end.is_some_and(|e| ext.logical < e) {
				faults.push(alloc::format!("inode {num}: extents overlap or are out of order").into_bytes());
			}
			end = Some(ext.logical + ext.length as u64);
		}
	}

	pub(crate) fn with_root<R>(&mut self, root: u64, crc: u32, f: impl FnOnce(&mut Self) -> R) -> R {
		let saved = (self.inode_root, self.inode_root_crc);
		self.inode_root = root;
		self.inode_root_crc = crc;
		self.icache.clear();
		self.dcache.clear();
		// and the decompression cache, which was left alone: its contents describe
		// extents of whichever generation was live when they were decoded.
		self.decomp.clear();
		let r = f(self);
		self.inode_root = saved.0;
		self.inode_root_crc = saved.1;
		self.icache.clear();
		self.dcache.clear();
		self.decomp.clear();
		r
	}

	// Walk the inode B+tree, verifying every node against its stored checksum, and sum
	// the corrupt data blocks of every live file. Directory inodes get their own tree
	// walked and verified too, so a snapshot generation's directory damage is caught
	// here and not only when the snapshot is mounted. A corrupt subtree counts as a
	// failure and the walk continues; only the root's own damage surfaces as the error
	// (the caller counts it). The depth budget bounds the recursion against a hostile
	// chain of one-child internals.
	pub(crate) fn check_inode_tree(&mut self, ptr: u64, crc: u32, depth: usize, visited: &mut [u8]) -> Result<u32, FsError> {
		if ptr == 0 {
			return Ok(0);
		}
		if depth == 0 {
			return Err(FsError::Corrupt);
		}
		// The raw marking walk carries a visited bitmap; this one carried only a depth
		// limit, which protects the stack and not the clock. A node with hundreds of
		// children all pointing at the next level's single node - with CRCs that agree,
		// which is buildable bottom-up and needs no cycle - is walked once per pointer per
		// level: fanout raised to the depth. Verifying a block twice tells nobody anything
		// the first pass did not, so the second visit is simply not made, and the walk is
		// bounded by the pool.
		if ptr >= self.num_blocks {
			return Err(FsError::Corrupt);
		}
		if test_bit(visited, ptr) {
			return Ok(0);
		}
		set_bit(visited, ptr);
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node(ptr, crc, &mut buf)?;
		let mut bad = 0u32;
		if node_type(&buf) == NODE_LEAF {
			for i in 0..leaf_count(&buf, INODE_REC) {
				let off = NODE_HDR + i * INODE_REC + 8;
				let mut inode = Inode::parse(&buf[off..off + INODE_SIZE]);
				let checked = if inode.r#type == TYPE_FILE {
					self.load_spill(&mut inode).and_then(|()| self.count_corrupt(&inode))
				} else if inode.r#type == TYPE_DIR {
					self.check_dir_tree(inode.dir_root, inode.dir_root_crc, TREE_DEPTH_MAX, visited)
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
				bad = bad.saturating_add(match self.check_inode_tree(child_ptr(&buf, i), child_crc(&buf, i), depth - 1, visited) {
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
	pub(crate) fn check_dir_tree(&mut self, ptr: u64, crc: u32, depth: usize, visited: &mut [u8]) -> Result<u32, FsError> {
		if ptr == 0 {
			return Ok(0);
		}
		if depth == 0 {
			return Err(FsError::Corrupt);
		}
		// same bound as the inode walk, sharing its map: a block is one node of one tree,
		// so a directory node reached twice - however many names alias it - is verified
		// once.
		if ptr >= self.num_blocks {
			return Err(FsError::Corrupt);
		}
		if test_bit(visited, ptr) {
			return Ok(0);
		}
		set_bit(visited, ptr);
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node(ptr, crc, &mut buf)?;
		let mut bad = 0u32;
		if node_type(&buf) == NODE_INTERNAL {
			for i in 0..=internal_count(&buf) {
				bad = bad.saturating_add(match self.check_dir_tree(child_ptr(&buf, i), child_crc(&buf, i), depth - 1, visited) {
					Ok(b) => b,
					Err(FsError::Corrupt | FsError::Io) => 1,
					Err(e) => return Err(e),
				});
			}
		}
		Ok(bad)
	}
}
