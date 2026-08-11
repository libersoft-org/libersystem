use crate::*;

impl<D: BlockDevice> LiberFs<D> {
	// Create a named, read-only snapshot pinning the current generation's inode-tree
	// root, so its blocks survive later commits until the snapshot is deleted. The name
	// must be non-empty UTF-8 without NUL (the on-disk record is NUL-padded UTF-8, so
	// an embedded NUL would silently truncate the name - and change its identity - at
	// the next mount), at most SNAP_NAME_MAX bytes, and unique among existing
	// snapshots; the chained table holds any number of them.
	pub fn create_snapshot(&mut self, name: &[u8]) -> Result<(), FsError> {
		if name.is_empty() || name.contains(&0) || core::str::from_utf8(name).is_err() {
			return Err(FsError::BadName);
		}
		if name.len() > SNAP_NAME_MAX {
			return Err(FsError::TooLong);
		}
		if self.snapshots.iter().any(|s| s.name == name) {
			return Err(FsError::Exists);
		}
		// The writer's half of the format's ceiling: what a mount refuses to read, a write must
		// refuse to create, or the volume becomes one this build cannot open.
		if self.snapshots.len() >= MAX_SNAPSHOTS {
			return Err(FsError::NoSpace);
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
		// the rebuilt chain replaces the old one wholesale: drop the old blocks.
		let old = self.snap_root;
		self.walk_chain(old, |fs, ptr| fs.drop_block(ptr))?;
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
		self.snapshots = self.read_snapshot_table(self.snap_root, self.snap_root_crc, &mut |_| {})?;
		Ok(())
	}

	// The chain walk itself, over an ARBITRARY root, so the previous generation's table
	// can be read too - that generation's snapshots have to stay reserved until the
	// superblock describing them is itself overwritten, and nothing could read them
	// before this was separable. `seen` is handed every chain block as it is visited,
	// which is what lets the free-map walk reserve the table's own blocks in one pass.
	pub(crate) fn read_snapshot_table(&mut self, root: u64, root_crc: u32, seen: &mut dyn FnMut(u64)) -> Result<Vec<Snapshot>, FsError> {
		let mut out: Vec<Snapshot> = Vec::new();
		// Name hashes, kept sorted, so uniqueness costs a binary search instead of a scan.
		//
		// The check itself is right and was added for a good reason - a forged table could name two
		// snapshots the same and the mount stayed writable. The SHAPE was the problem: `out.iter()
		// .any(..)` per record is quadratic, and the walk admitted as many records as the chain had
		// room for. On a one-gigabyte volume that is around twelve million records and something
		// near 10^14 comparisons, so the mount would not finish long before it ran out of memory -
		// and a hang at mount is a system that does not boot with nothing said about why.
		//
		// A hash collision is not a duplicate, so a match is confirmed against the names.
		let mut hashes: Vec<(u64, usize)> = Vec::new();
		let mut ptr = root;
		let mut crc = root_crc;
		let mut block = vec![0u8; BLOCK_SIZE];
		let mut steps = 0u64;
		while ptr != 0 {
			// bound the walk like `walk_chain`: a pointer outside the pool is damage,
			// and no chain can be longer than the pool - a CRC-consistent forged cycle
			// (checksums prove integrity, not sanity) must not hang the mount or grow
			// the table without limit.
			if ptr >= self.num_blocks || steps >= self.num_blocks {
				return Err(FsError::Corrupt);
			}
			steps += 1;
			seen(ptr);
			if !self.dev.read_block(ptr, &mut block) {
				return Err(FsError::Io);
			}
			if crc32c(&block) != crc {
				return Err(FsError::Corrupt);
			}
			// a count above what the block can hold is impossible, not something to trim
			// down to the nearest possible value and carry on with.
			let count = u32::from_le_bytes(block[CHAIN_COUNT_OFF..CHAIN_COUNT_OFF + 4].try_into().unwrap()) as usize;
			if count > SNAPS_PER_BLOCK {
				return Err(FsError::Corrupt);
			}
			for i in 0..count {
				let off = SNAP_HDR + i * SNAP_REC;
				let name = name_in(&block[off..off + SNAP_NAME_MAX]).to_vec();
				let inode_root = u64::from_le_bytes(block[off + SNAP_ROOT_OFF..off + SNAP_ROOT_OFF + 8].try_into().unwrap());
				let inode_root_crc = u32::from_le_bytes(block[off + SNAP_ROOT_CRC_OFF..off + SNAP_ROOT_CRC_OFF + 4].try_into().unwrap());
				let generation = u64::from_le_bytes(block[off + SNAP_GEN_OFF..off + SNAP_GEN_OFF + 8].try_into().unwrap());
				// A record read off the medium gets the same scrutiny as one being
				// written, which it did not before: `create_snapshot` demands a non-empty
				// unique UTF-8 name, and the loader demanded nothing at all - so a forged
				// or damaged table could name a snapshot with no name, two with the same
				// name, or a root outside the pool, and the mount stayed writable.
				//
				// The name is already NUL-terminated by `name_in`, so what is left to
				// check is that something survives the terminator and that it is text.
				if name.is_empty() || core::str::from_utf8(&name).is_err() {
					return Err(FsError::Corrupt);
				}
				// And the field AFTER the terminator has to be the zero padding the writer lays
				// down. `read_superblock` was taught this for the volume label and this record was
				// left with half the rule: `name_in` stops at the first NUL and looks no further,
				// so `backup\0\xff\xffgarbage` read as the snapshot "backup". Nothing consumes
				// those bytes, which is why it went unnoticed - the point is that no writer of this
				// format produces a field shaped that way, so a field shaped that way is itself
				// evidence the record came from something else.
				if block[off + name.len()..off + SNAP_NAME_MAX].iter().any(|&b| b != 0) {
					return Err(FsError::Corrupt);
				}
				let hash = name_hash(&name);
				match hashes.binary_search_by_key(&hash, |&(h, _)| h) {
					// Some record already hashes here. Hashes are not identities, so the names are
					// compared - only over the run that shares this hash, which is one entry in
					// every case that is not a deliberate collision.
					Ok(at) => {
						let mut first = at;
						while first > 0 && hashes[first - 1].0 == hash {
							first -= 1;
						}
						if hashes[first..].iter().take_while(|&&(h, _)| h == hash).any(|&(_, i)| out[i].name == name) {
							return Err(FsError::Corrupt);
						}
						if hashes.try_reserve(1).is_err() {
							return Err(FsError::NoMemory);
						}
						hashes.insert(at, (hash, out.len()));
					}
					Err(at) => {
						if hashes.try_reserve(1).is_err() {
							return Err(FsError::NoMemory);
						}
						hashes.insert(at, (hash, out.len()));
					}
				}
				// a root of 0 is the "no tree" sentinel and no generation has one - even a
				// freshly formatted volume has a root leaf - and a root outside the pool
				// cannot be walked at all.
				if inode_root == 0 || inode_root >= self.num_blocks {
					return Err(FsError::Corrupt);
				}
				// a snapshot pins a generation that ALREADY happened: the record carries
				// the generation live when it was taken, and the superblock carries the
				// one the commit produced, so a stored number above the live one is
				// impossible however it got there.
				if generation > self.generation {
					return Err(FsError::Corrupt);
				}
				// The ceiling, checked as the walk goes rather than after it: a table claiming more
				// than the format permits is refused before the memory is spent on reading it.
				if out.len() >= MAX_SNAPSHOTS {
					return Err(FsError::Corrupt);
				}
				if out.try_reserve(1).is_err() {
					return Err(FsError::NoMemory);
				}
				out.push(Snapshot { name, inode_root, inode_root_crc, generation });
			}
			ptr = u64::from_le_bytes(block[CHAIN_NEXT_OFF..CHAIN_NEXT_OFF + 8].try_into().unwrap());
			crc = u32::from_le_bytes(block[CHAIN_CRC_OFF..CHAIN_CRC_OFF + 4].try_into().unwrap());
		}
		Ok(out)
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
		Ok(Stat { size: inode.size, is_dir: inode.r#type == TYPE_DIR, ctime: inode.ctime, mtime: inode.mtime })
	}

	// offset / partial reads and writes

	// Read up to `len` bytes of the file at `path` starting at byte `offset`. Returns
	// fewer bytes (or none) if the range runs past the end; holes read back as zeros.
	pub fn read_at(&mut self, path: &[u8], offset: u64, len: usize) -> Result<Vec<u8>, FsError> {
		let inode_num = self.resolve(path)?;
		let inode = self.read_inode(inode_num)?;
		if inode.r#type != TYPE_FILE {
			return Err(FsError::IsDir);
		}
		self.read_range(&inode, offset, len as u64)
	}

	// Write `data` into the file at `path` starting at byte `offset`, creating the file
	// (and any missing parents) if needed and extending it if the write runs past the
	// end. A gap between the old end and `offset` becomes a hole that reads as zeros.
	// Only the touched blocks are rewritten (each copied up to a fresh block), the rest
	// of the file is left in place, and the change commits atomically.
	pub fn write_at(&mut self, path: &[u8], offset: u64, data: &[u8]) -> Result<(), FsError> {
		self.mutate(|fs| fs.write_at_inner(path, Some(offset), data))
	}

	// The body behind `write_at` and `append`: `offset` of None means "the current end
	// of the file", so append resolves the path once, here, instead of twice.
	pub(crate) fn write_at_inner(&mut self, path: &[u8], offset: Option<u64>, data: &[u8]) -> Result<(), FsError> {
		let (parent, name) = self.resolve_parent(path, true)?;
		let inode_num = match self.dir_lookup(parent, name)? {
			Some(num) => {
				if self.read_inode(num)?.r#type != TYPE_FILE {
					return Err(FsError::IsDir);
				}
				num
			}
			None => {
				let num = self.alloc_inode()?;
				let mut f = Inode::empty(TYPE_FILE);
				f.ctime = self.clock;
				f.mtime = self.clock;
				self.write_inode(num, &mut f)?;
				self.dir_insert(parent, name, num)?;
				num
			}
		};
		let mut inode = self.read_inode(inode_num)?;
		let offset = offset.unwrap_or(inode.size);
		if !data.is_empty() {
			let start = offset;
			// an offset that runs the write past the addressable end is refused, not
			// wrapped (a wrap would report a no-op as success).
			let end = offset.checked_add(data.len() as u64).ok_or(FsError::Invalid)?;
			let first = start / BLOCK_SIZE as u64;
			let last = (end - 1) / BLOCK_SIZE as u64;
			let mut buf = vec![0u8; BLOCK_SIZE];
			for lb in first..=last {
				let block_start = lb * BLOCK_SIZE as u64;
				// The guard above refuses a write that runs PAST the addressable end, so
				// everything here is inside it - but the last block of the address space
				// begins less than a block below the ceiling, and adding BLOCK_SIZE to its
				// absolute start overflows: a panic in debug, a wrap in release. The
				// distances are computed from `end` and `block_start` instead, which are
				// both known to be in range, and the one remaining sum saturates.
				let full = start <= block_start && end - block_start >= BLOCK_SIZE as u64;
				// a full-block overwrite needs no read; a partial one preserves whatever
				// is there (zeros for a hole or a block past the old end).
				if full || !self.read_logical(&inode, lb, &mut buf)? {
					buf.fill(0);
				}
				let copy_start = start.max(block_start);
				let copy_end = end.min(block_start.saturating_add(BLOCK_SIZE as u64));
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
		self.mutate(|fs| fs.write_at_inner(path, None, data))
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
		if inode.r#type != TYPE_FILE {
			return Err(FsError::IsDir);
		}
		if new_len < inode.size {
			let keep = new_len.div_ceil(BLOCK_SIZE as u64);
			self.free_from(&mut inode, keep)?;
			// zero the slack past the new end in the last kept block, so that a later
			// grow back over it reads zeros rather than the discarded tail.
			let tail = (new_len % BLOCK_SIZE as u64) as usize;
			if tail != 0 {
				let lb = new_len / BLOCK_SIZE as u64;
				let mut buf = vec![0u8; BLOCK_SIZE];
				if self.read_logical(&inode, lb, &mut buf)? {
					buf[tail..].fill(0);
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
		if from_inode.r#type == TYPE_DIR && self.subtree_contains(inode_f, pt)? {
			return Err(FsError::Invalid);
		}

		let dest = self.dir_lookup(pt, nt)?;
		if let Some(inode_t) = dest {
			if inode_t == inode_f {
				return Ok(());
			}
			let ti = self.read_inode(inode_t)?;
			// same as the delete path: the tree decides, not the cached count, since
			// replacing this name frees the directory it used to hold.
			if ti.r#type == TYPE_DIR && (ti.size != 0 || self.dir_has_entries(&ti)?) {
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
				self.drop_deleted_inode(&ti)?;
				self.free_inode(inode_t)?;
			}
		}
		Ok(())
	}
}
