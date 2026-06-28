use crate::*;

impl<D: BlockDevice> LiberFs<D> {
	// Create a named, read-only snapshot pinning the current generation's inode-tree
	// root, so its blocks survive later commits until the snapshot is deleted. The name
	// must be non-empty, at most SNAP_NAME_MAX bytes, and unique among existing
	// snapshots; a volume holds at most SNAP_MAX snapshots.
	pub fn create_snapshot(&mut self, name: &[u8]) -> Result<(), FsError> {
		if name.is_empty() {
			return Err(FsError::Invalid);
		}
		if name.len() > SNAP_NAME_MAX {
			return Err(FsError::TooLong);
		}
		if self.snapshots.iter().any(|s| s.name == name) {
			return Err(FsError::Invalid);
		}
		if self.snapshots.len() >= SNAP_MAX {
			return Err(FsError::NoSpace);
		}
		self.begin();
		let r = self.create_snapshot_inner(name);
		self.finish(r)
	}

	pub(crate) fn create_snapshot_inner(&mut self, name: &[u8]) -> Result<(), FsError> {
		// pin the current live generation: the snapshot-table write is the only change,
		// so the committed generation keeps this exact inode-tree root.
		self.snapshots.push(Snapshot { name: name.to_vec(), inode_root: self.inode_root, inode_root_crc: self.inode_root_crc, generation: self.generation });
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
		self.begin();
		let r = self.delete_snapshot_inner(name);
		self.finish(r)
	}

	pub(crate) fn delete_snapshot_inner(&mut self, name: &[u8]) -> Result<(), FsError> {
		self.snapshots.retain(|s| s.name != name);
		self.write_snapshot_table()
	}

	// Serialize the in-memory snapshot table to a fresh metadata block (copy-on-write),
	// updating snap_root and its CRC32C; an empty table clears the pointer. The fresh
	// block is published by the commit's superblock write.
	pub(crate) fn write_snapshot_table(&mut self) -> Result<(), FsError> {
		if self.snapshots.is_empty() {
			self.snap_root = 0;
			self.snap_root_crc = 0;
			return Ok(());
		}
		let mut block = vec![0u8; BLOCK_SIZE];
		block[0..4].copy_from_slice(&(self.snapshots.len() as u32).to_le_bytes());
		for (i, s) in self.snapshots.iter().enumerate() {
			let off = SNAP_HDR + i * SNAP_REC;
			block[off..off + s.name.len()].copy_from_slice(&s.name);
			block[off + SNAP_NAME_MAX..off + SNAP_NAME_MAX + 8].copy_from_slice(&s.inode_root.to_le_bytes());
			block[off + SNAP_NAME_MAX + 8..off + SNAP_NAME_MAX + 12].copy_from_slice(&s.inode_root_crc.to_le_bytes());
			block[off + SNAP_NAME_MAX + 12..off + SNAP_NAME_MAX + 20].copy_from_slice(&s.generation.to_le_bytes());
		}
		let ptr = self.snap_root;
		let dest = self.node_dest(ptr)?;
		let crc = self.write_node_to(dest, &block)?;
		self.snap_root = dest;
		self.snap_root_crc = crc;
		Ok(())
	}

	// Load the snapshot table the superblock points at into memory. The block is checked
	// against snap_root_crc; a corrupt or empty table yields no snapshots, so a damaged
	// table never pins (or walks) garbage.
	pub(crate) fn load_snapshot_table(&mut self) -> Result<(), FsError> {
		self.snapshots = Vec::new();
		if self.snap_root == 0 {
			return Ok(());
		}
		let mut block = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(self.snap_root, &mut block) {
			return Err(FsError::Io);
		}
		if crc32c(&block) != self.snap_root_crc {
			return Ok(());
		}
		let count = (u32::from_le_bytes(block[0..4].try_into().unwrap()) as usize).min(SNAP_MAX);
		for i in 0..count {
			let off = SNAP_HDR + i * SNAP_REC;
			let name = name_in(&block[off..off + SNAP_NAME_MAX]).to_vec();
			let inode_root = u64::from_le_bytes(block[off + SNAP_NAME_MAX..off + SNAP_NAME_MAX + 8].try_into().unwrap());
			let inode_root_crc = u32::from_le_bytes(block[off + SNAP_NAME_MAX + 8..off + SNAP_NAME_MAX + 12].try_into().unwrap());
			let generation = u64::from_le_bytes(block[off + SNAP_NAME_MAX + 12..off + SNAP_NAME_MAX + 20].try_into().unwrap());
			self.snapshots.push(Snapshot { name, inode_root, inode_root_crc, generation });
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
			return Err(FsError::NotFound);
		}
		if offset >= inode.size || len == 0 {
			return Ok(Vec::new());
		}
		let end = (offset + len as u64).min(inode.size);
		let mut out = Vec::with_capacity((end - offset) as usize);
		let mut buf = vec![0u8; BLOCK_SIZE];
		let first = (offset / BLOCK_SIZE as u64) as usize;
		let last = ((end - 1) / BLOCK_SIZE as u64) as usize;
		for lb in first..=last {
			let block_start = lb as u64 * BLOCK_SIZE as u64;
			if !self.read_logical(&inode, lb, &mut buf)? {
				for b in buf.iter_mut() {
					*b = 0;
				}
			}
			let copy_start = offset.max(block_start);
			let copy_end = end.min(block_start + BLOCK_SIZE as u64);
			out.extend_from_slice(&buf[(copy_start - block_start) as usize..(copy_end - block_start) as usize]);
		}
		Ok(out)
	}

	// Write `data` into the file at `path` starting at byte `offset`, creating the file
	// (and any missing parents) if needed and extending it if the write runs past the
	// end. A gap between the old end and `offset` becomes a hole that reads as zeros.
	// Only the touched blocks are rewritten (each copied up to a fresh block), the rest
	// of the file is left in place, and the change commits atomically.
	pub fn write_at(&mut self, path: &[u8], offset: u64, data: &[u8]) -> Result<(), FsError> {
		self.begin();
		let r = self.write_at_inner(path, offset, data);
		self.finish(r)
	}

	pub(crate) fn write_at_inner(&mut self, path: &[u8], offset: u64, data: &[u8]) -> Result<(), FsError> {
		let (parent, name) = self.resolve_parent(path, true)?;
		let inode_num = match self.dir_lookup(parent, name)? {
			Some(num) => {
				if self.read_inode(num)?.kind != KIND_FILE {
					return Err(FsError::Invalid);
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
		self.begin();
		let r = self.append_inner(path, data);
		self.finish(r)
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
		self.begin();
		let r = self.truncate_inner(path, new_len);
		self.finish(r)
	}

	pub(crate) fn truncate_inner(&mut self, path: &[u8], new_len: u64) -> Result<(), FsError> {
		let inode_num = self.resolve(path)?;
		let mut inode = self.read_inode(inode_num)?;
		if inode.kind != KIND_FILE {
			return Err(FsError::Invalid);
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
		self.begin();
		let r = self.rename_inner(from, to);
		self.finish(r)
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
				return Err(FsError::Invalid);
			}
		}

		// point the destination name at the moved inode (add or overwrite), clear the
		// source entry, and free the inode the destination used to hold. Its old blocks
		// stay with the previous generation; the next commit reclaims them.
		self.dir_insert(pt, nt, inode_f)?;
		self.dir_remove(pf, nf)?;
		if let Some(inode_t) = dest {
			if inode_t != inode_f {
				self.free_inode(inode_t)?;
			}
		}
		Ok(())
	}

	// consistency

}
