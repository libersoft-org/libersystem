use crate::*;

impl<D: BlockDevice> LiberFs<D> {
	// Verify integrity. With copy-on-write a crash can no longer leak blocks or orphan
	// an inode (the free map is derived and a commit is atomic), so there is nothing to
	// reclaim; what fsck still does is walk every live data block and check it against
	// its stored checksum, reporting how many fail. The free map is also rederived,
	// which is a no-op on a consistent volume.
	pub fn fsck(&mut self) -> Result<FsckReport, FsError> {
		self.derive_free()?;
		let mut checksum_failures = self.check_inode_tree(self.inode_root, self.inode_root_crc)?;
		// every pinned snapshot generation is part of the live volume: verify its blocks
		// too, so corruption in a snapshot is reported and the walk accounts for it.
		for i in 0..self.snapshots.len() {
			let (root, crc) = (self.snapshots[i].inode_root, self.snapshots[i].inode_root_crc);
			checksum_failures += self.check_inode_tree(root, crc)?;
		}
		Ok(FsckReport { reclaimed_blocks: 0, reclaimed_inodes: 0, checksum_failures })
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

	// transactions

}
