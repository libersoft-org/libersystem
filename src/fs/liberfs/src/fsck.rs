use crate::*;

impl<D: BlockDevice> LiberFs<D> {
	// Verify integrity. With copy-on-write a crash cannot leak blocks or orphan an
	// inode (the free map is derived and a commit is atomic), so there is nothing to
	// reclaim; what fsck does is walk the live namespace, check every file's data
	// blocks against their stored checksums, and NAME the damaged files - a count
	// alone would leave the operator knowing something is wrong but not what. The
	// pinned snapshot generations are verified too (counted; their files are named
	// under the snapshot's own mount, not here). The free map is also rederived,
	// which is a no-op on a consistent volume.
	pub fn fsck(&mut self) -> Result<FsckReport, FsError> {
		self.derive_free()?;
		let mut checksum_failures = 0u32;
		let mut damaged: Vec<Vec<u8>> = Vec::new();
		// walk the live namespace from the root, tracking each file's full path.
		let mut stack: Vec<(u32, Vec<u8>)> = vec![(self.root_inode, Vec::new())];
		while let Some((dir, prefix)) = stack.pop() {
			for (name, child) in self.dir_entries_of(dir)? {
				let mut path = prefix.clone();
				if !path.is_empty() {
					path.push(b'/');
				}
				path.extend_from_slice(&name);
				let inode = self.read_inode(child)?;
				if inode.kind == KIND_DIR {
					stack.push((child, path));
				} else {
					let bad = self.count_corrupt(&inode)?;
					if bad > 0 {
						checksum_failures += bad;
						damaged.push(path);
					}
				}
			}
		}
		// every pinned snapshot generation is part of the live volume: verify its
		// blocks too, so corruption in a snapshot is reported and the walk accounts
		// for it.
		for i in 0..self.snapshots.len() {
			let (root, crc) = (self.snapshots[i].inode_root, self.snapshots[i].inode_root_crc);
			checksum_failures += self.check_inode_tree(root, crc)?;
		}
		Ok(FsckReport { checksum_failures, damaged })
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
	// the corrupt data blocks of every live file.
	pub(crate) fn check_inode_tree(&mut self, ptr: u64, crc: u32) -> Result<u32, FsError> {
		if ptr == 0 {
			return Ok(0);
		}
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node(ptr, crc, &mut buf)?;
		let count = node_count(&buf);
		let mut bad = 0;
		if node_type(&buf) == NODE_LEAF {
			for i in 0..count {
				let off = NODE_HDR + i * INODE_REC + 8;
				let mut inode = Inode::parse(&buf[off..off + INODE_SIZE]);
				if inode.kind == KIND_FILE {
					self.load_spill(&mut inode)?;
					bad += self.count_corrupt(&inode)?;
				}
			}
		} else {
			for i in 0..=count {
				bad += self.check_inode_tree(child_ptr(&buf, i), child_crc(&buf, i))?;
			}
		}
		Ok(bad)
	}
}
