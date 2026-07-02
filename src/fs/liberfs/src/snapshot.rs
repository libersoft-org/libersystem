use crate::*;

impl<D: BlockDevice> LiberFs<D> {
	// Create a named, read-only snapshot pinning the current generation's inode-tree
	// root, so its blocks survive later commits until the snapshot is deleted. The name
	// must be non-empty, at most SNAP_NAME_MAX bytes, and unique among existing
	// snapshots; the chained table holds any number of them.
	pub fn create_snapshot(&mut self, name: &[u8]) -> Result<(), FsError> {
		if name.is_empty() {
			return Err(FsError::BadName);
		}
		if name.len() > SNAP_NAME_MAX {
			return Err(FsError::TooLong);
		}
		if self.snapshots.iter().any(|s| s.name == name) {
			return Err(FsError::Exists);
		}
		self.mutate(|fs| fs.create_snapshot_inner(name))
	}

	pub(crate) fn create_snapshot_inner(&mut self, name: &[u8]) -> Result<(), FsError> {
		// pin the current live generation: the snapshot-table write is the only change,
		// so the committed generation keeps this exact inode-tree root. The pinned set
		// changes, so this commit rebuilds the free map and pinned map by the full walk.
		self.snapshots.push(Snapshot { name: name.to_vec(), inode_root: self.inode_root, inode_root_crc: self.inode_root_crc, generation: self.generation });
		self.snapshots_dirty = true;
		self.write_snapshot_table()
	}

	// List the named snapshots as (name, generation) pairs, oldest first.
	pub fn list_snapshots(&mut self) -> Result<Vec<(Vec<u8>, u64)>, FsError> {
		Ok(self.snapshots.iter().map(|s| (s.name.clone(), s.generation)).collect())
	}

	// Delete the named snapshot, releasing the blocks only it pinned (reclaimed by the
	// rederived free map). An unknown name is NotFound.
	pub fn delete_snapshot(&mut self, name: &[u8]) -> Result<(), FsError> {
		if !self.snapshots.iter().any(|s| s.name == name) {
			return Err(FsError::NotFound);
		}
		self.mutate(|fs| fs.delete_snapshot_inner(name))
	}

	pub(crate) fn delete_snapshot_inner(&mut self, name: &[u8]) -> Result<(), FsError> {
		// the deleted snapshot's blocks unpin: this commit rebuilds the free map and
		// pinned map by the full walk, which is what reclaims them.
		self.snapshots.retain(|s| s.name != name);
		self.snapshots_dirty = true;
		self.write_snapshot_table()
	}

	// Serialize the in-memory snapshot table to a fresh chain of metadata blocks
	// (copy-on-write: the old chain's blocks are dropped), updating snap_root and its
	// CRC32C; an empty table clears the pointer. Built back to front so each block
	// carries the (pointer, CRC32C) of the one after it; published by the commit's
	// superblock write.
	pub(crate) fn write_snapshot_table(&mut self) -> Result<(), FsError> {
		// the rebuilt chain replaces the old one wholesale: drop the old blocks (the raw
		// walk stops at a pointer outside the pool).
		let mut old = self.snap_root;
		let mut buf = vec![0u8; BLOCK_SIZE];
		while old != 0 && old < self.num_blocks {
			self.drop_block(old);
			if !self.dev.read_block(old, &mut buf) {
				return Err(FsError::Io);
			}
			old = u64::from_le_bytes(buf[0..8].try_into().unwrap());
		}
		if self.snapshots.is_empty() {
			self.snap_root = 0;
			self.snap_root_crc = 0;
			return Ok(());
		}
		let mut next_ptr = 0u64;
		let mut next_crc = 0u32;
		let snapshots = self.snapshots.clone();
		for chunk in snapshots.chunks(SNAPS_PER_BLOCK).rev() {
			let blk = self.alloc_meta()?;
			let mut block = vec![0u8; BLOCK_SIZE];
			block[CHAIN_NEXT_OFF..CHAIN_NEXT_OFF + 8].copy_from_slice(&next_ptr.to_le_bytes());
			block[CHAIN_CRC_OFF..CHAIN_CRC_OFF + 4].copy_from_slice(&next_crc.to_le_bytes());
			block[CHAIN_COUNT_OFF..CHAIN_COUNT_OFF + 4].copy_from_slice(&(chunk.len() as u32).to_le_bytes());
			for (i, s) in chunk.iter().enumerate() {
				let off = SNAP_HDR + i * SNAP_REC;
				block[off..off + s.name.len()].copy_from_slice(&s.name);
				block[off + SNAP_ROOT_OFF..off + SNAP_ROOT_OFF + 8].copy_from_slice(&s.inode_root.to_le_bytes());
				block[off + SNAP_ROOT_CRC_OFF..off + SNAP_ROOT_CRC_OFF + 4].copy_from_slice(&s.inode_root_crc.to_le_bytes());
				block[off + SNAP_GEN_OFF..off + SNAP_GEN_OFF + 8].copy_from_slice(&s.generation.to_le_bytes());
			}
			if !self.dev.write_block(blk, &block) {
				return Err(FsError::Io);
			}
			next_ptr = blk;
			next_crc = crc32c(&block);
		}
		self.snap_root = next_ptr;
		self.snap_root_crc = next_crc;
		Ok(())
	}

	// Load the snapshot chain the superblock points at into memory. Each block is
	// checked against the CRC32C its predecessor (or the superblock) recorded; a
	// mismatch is FsError::Corrupt - the caller (mount) degrades the volume to
	// read-only, because the pinned generations the table named can no longer be
	// reserved and a commit could reuse their blocks. Silently dropping the table here
	// would quietly destroy every named snapshot.
	pub(crate) fn load_snapshot_table(&mut self) -> Result<(), FsError> {
		self.snapshots = Vec::new();
		let mut ptr = self.snap_root;
		let mut crc = self.snap_root_crc;
		let mut block = vec![0u8; BLOCK_SIZE];
		while ptr != 0 {
			if !self.dev.read_block(ptr, &mut block) {
				return Err(FsError::Io);
			}
			if crc32c(&block) != crc {
				return Err(FsError::Corrupt);
			}
			let count = (u32::from_le_bytes(block[CHAIN_COUNT_OFF..CHAIN_COUNT_OFF + 4].try_into().unwrap()) as usize).min(SNAPS_PER_BLOCK);
			for i in 0..count {
				let off = SNAP_HDR + i * SNAP_REC;
				let name = name_in(&block[off..off + SNAP_NAME_MAX]).to_vec();
				let inode_root = u64::from_le_bytes(block[off + SNAP_ROOT_OFF..off + SNAP_ROOT_OFF + 8].try_into().unwrap());
				let inode_root_crc = u32::from_le_bytes(block[off + SNAP_ROOT_CRC_OFF..off + SNAP_ROOT_CRC_OFF + 4].try_into().unwrap());
				let generation = u64::from_le_bytes(block[off + SNAP_GEN_OFF..off + SNAP_GEN_OFF + 8].try_into().unwrap());
				self.snapshots.push(Snapshot { name, inode_root, inode_root_crc, generation });
			}
			ptr = u64::from_le_bytes(block[CHAIN_NEXT_OFF..CHAIN_NEXT_OFF + 8].try_into().unwrap());
			crc = u32::from_le_bytes(block[CHAIN_CRC_OFF..CHAIN_CRC_OFF + 4].try_into().unwrap());
		}
		Ok(())
	}

	// Recover the device, consuming the filesystem.
	pub fn into_device(self) -> D {
		self.dev
	}

	// Borrow the backing block device without consuming the filesystem, so a caller can
	// open a second read-only view (a snapshot) over the same backing.
	pub fn device(&self) -> &D {
		&self.dev
	}

	// metadata and timestamps

	// Advance the logical clock the filesystem stamps onto inode `mtime` (and `ctime`
	// for new files). The caller injects a real time source; there is no wall clock in
	// this crate.
	pub fn set_clock(&mut self, now: u64) {
		self.clock = now;
	}

	// Return metadata for the file or directory at `path`.
	pub fn stat(&mut self, path: &[u8]) -> Result<Stat, FsError> {
		let inode_num = self.resolve(path)?;
		let inode = self.read_inode(inode_num)?;
		Ok(Stat { size: inode.size, is_dir: inode.kind == KIND_DIR, ctime: inode.ctime, mtime: inode.mtime })
	}

	// offset / partial reads and writes

	// Read up to `len` bytes of the file at `path` starting at byte `offset`. Returns
	// fewer bytes (or none) if the range runs past the end; holes read back as zeros.
	pub fn read_at(&mut self, path: &[u8], offset: u64, len: usize) -> Result<Vec<u8>, FsError> {
		let inode_num = self.resolve(path)?;
		let inode = self.read_inode(inode_num)?;
		if inode.kind != KIND_FILE {
			return Err(FsError::IsDir);
		}
		self.read_range(&inode, offset, len)
	}

	// Write `data` into the file at `path` starting at byte `offset`, creating the file
	// (and any missing parents) if needed and extending it if the write runs past the
	// end. A gap between the old end and `offset` becomes a hole that reads as zeros.
	// Only the touched blocks are rewritten (each copied up to a fresh block), the rest
	// of the file is left in place, and the change commits atomically.
	pub fn write_at(&mut self, path: &[u8], offset: u64, data: &[u8]) -> Result<(), FsError> {
		self.mutate(|fs| fs.write_at_inner(path, offset, data))
	}

	pub(crate) fn write_at_inner(&mut self, path: &[u8], offset: u64, data: &[u8]) -> Result<(), FsError> {
		let (parent, name) = self.resolve_parent(path, true)?;
		let inode_num = match self.dir_lookup(parent, name)? {
			Some(num) => {
				if self.read_inode(num)?.kind != KIND_FILE {
					return Err(FsError::IsDir);
				}
				num
			}
			None => {
				let num = self.alloc_inode()?;
				let mut f = Inode::empty(KIND_FILE);
				f.ctime = self.clock;
				f.mtime = self.clock;
				self.write_inode(num, &mut f)?;
				self.dir_insert(parent, name, num)?;
				num
			}
		};
		let mut inode = self.read_inode(inode_num)?;
		if !data.is_empty() {
			let start = offset;
			let end = offset + data.len() as u64;
			let first = (start / BLOCK_SIZE as u64) as usize;
			let last = ((end - 1) / BLOCK_SIZE as u64) as usize;
			let mut buf = vec![0u8; BLOCK_SIZE];
			for lb in first..=last {
				let block_start = lb as u64 * BLOCK_SIZE as u64;
				let full = start <= block_start && end >= block_start + BLOCK_SIZE as u64;
				// a full-block overwrite needs no read; a partial one preserves whatever
				// is there (zeros for a hole or a block past the old end).
				if full || !self.read_logical(&inode, lb, &mut buf)? {
					for b in buf.iter_mut() {
						*b = 0;
					}
				}
				let copy_start = start.max(block_start);
				let copy_end = end.min(block_start + BLOCK_SIZE as u64);
				let buf_off = (copy_start - block_start) as usize;
				let data_off = (copy_start - start) as usize;
				let n = (copy_end - copy_start) as usize;
				buf[buf_off..buf_off + n].copy_from_slice(&data[data_off..data_off + n]);
				self.write_logical(&mut inode, lb, &buf)?;
			}
			if end > inode.size {
				inode.size = end;
			}
		}
		inode.mtime = self.clock;
		self.write_inode(inode_num, &mut inode)?;
		Ok(())
	}

	// Append `data` to the end of the file at `path` (creating it if needed).
	pub fn append(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
		self.mutate(|fs| fs.append_inner(path, data))
	}

	pub(crate) fn append_inner(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
		let size = match self.resolve(path) {
			Ok(num) => self.read_inode(num)?.size,
			Err(FsError::NotFound) => 0,
			Err(e) => return Err(e),
		};
		self.write_at_inner(path, size, data)
	}

	// Resize the file at `path` to `new_len`: shrinking drops the blocks past the new
	// end, growing leaves a hole (which reads as zeros). Copy-on-write: the change goes
	// to fresh blocks and commits atomically.
	pub fn truncate(&mut self, path: &[u8], new_len: u64) -> Result<(), FsError> {
		self.mutate(|fs| fs.truncate_inner(path, new_len))
	}

	pub(crate) fn truncate_inner(&mut self, path: &[u8], new_len: u64) -> Result<(), FsError> {
		let inode_num = self.resolve(path)?;
		let mut inode = self.read_inode(inode_num)?;
		if inode.kind != KIND_FILE {
			return Err(FsError::IsDir);
		}
		if new_len < inode.size {
			let keep = (new_len as usize).div_ceil(BLOCK_SIZE);
			self.free_from(&mut inode, keep)?;
			// zero the slack past the new end in the last kept block, so that a later
			// grow back over it reads zeros rather than the discarded tail.
			let tail = (new_len % BLOCK_SIZE as u64) as usize;
			if tail != 0 {
				let lb = (new_len / BLOCK_SIZE as u64) as usize;
				let mut buf = vec![0u8; BLOCK_SIZE];
				if self.read_logical(&inode, lb, &mut buf)? {
					for b in buf[tail..].iter_mut() {
						*b = 0;
					}
					// rewriting the block refreshes its stored checksum too.
					self.write_logical(&mut inode, lb, &buf)?;
				}
			}
		}
		inode.size = new_len;
		inode.mtime = self.clock;
		self.write_inode(inode_num, &mut inode)?;
		Ok(())
	}

	// rename / move within the volume

	// Move the file or directory at `from` to `to` within the same volume. Missing
	// parent directories of `to` are created. An existing file (or empty directory) at
	// `to` is replaced. Copy-on-write: the whole move commits atomically, so a crash
	// leaves the object reachable under exactly one name - never lost or doubled.
	// Moving a directory into its own subtree is rejected.
	pub fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<(), FsError> {
		self.mutate(|fs| fs.rename_inner(from, to))
	}

	pub(crate) fn rename_inner(&mut self, from: &[u8], to: &[u8]) -> Result<(), FsError> {
		let (pf, nf) = self.resolve_parent(from, false)?;
		let inode_f = self.dir_lookup(pf, nf)?.ok_or(FsError::NotFound)?;
		let from_inode = self.read_inode(inode_f)?;
		let (pt, nt) = self.resolve_parent(to, true)?;

		// a directory may not move into itself or one of its descendants.
		if from_inode.kind == KIND_DIR && self.subtree_contains(inode_f, pt)? {
			return Err(FsError::Invalid);
		}

		let dest = self.dir_lookup(pt, nt)?;
		if let Some(inode_t) = dest {
			if inode_t == inode_f {
				return Ok(());
			}
			let ti = self.read_inode(inode_t)?;
			if ti.kind == KIND_DIR && ti.size != 0 {
				return Err(FsError::NotEmpty);
			}
		}

		// point the destination name at the moved inode (add or overwrite), clear the
		// source entry, and free the inode the destination used to hold. Its old blocks
		// stay with the previous generation and leave the new one.
		self.dir_insert(pt, nt, inode_f)?;
		self.dir_remove(pf, nf)?;
		if let Some(inode_t) = dest {
			if inode_t != inode_f {
				let ti = self.read_inode(inode_t)?;
				if ti.kind == KIND_FILE {
					self.drop_inode_blocks(&ti)?;
				}
				self.free_inode(inode_t)?;
			}
		}
		Ok(())
	}
}
