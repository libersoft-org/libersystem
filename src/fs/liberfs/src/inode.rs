use crate::*;

impl<D: BlockDevice> LiberFs<D> {
	// Read inode `num` from the inode B+tree. Missing (never allocated or freed) is
	// FsError::Invalid; a tree node failing its checksum is FsError::Corrupt. A hit
	// populates the bounded inode cache (extent map complete), so hot inodes - every
	// path component, a stat-ed file - stop re-walking the tree and re-reading the
	// overflow chain.
	pub(crate) fn read_inode(&mut self, num: u32) -> Result<Inode, FsError> {
		if let Some(inode) = self.icache.get(&num) {
			return Ok(inode.clone());
		}
		let key = num as u64;
		let probe = key.to_le_bytes();
		match self.tree_lookup(self.inode_root, self.inode_root_crc, key, &probe, INODE_REC)? {
			Some(rec) => {
				let mut inode = Inode::parse(&rec[8..8 + INODE_SIZE]);
				if inode.r#type != TYPE_DIR {
					// complete the extent map from the overflow chain (a no-op for a
					// file whose runs all fit inline).
					//
					// "File-shaped unless it is a directory" - the same rule `Inode::parse`,
					// the mark walk and the structural pass use. The gate here read
					// `== TYPE_FILE`, so an inode with an unknown type byte arrived with
					// only its INLINE runs: the mark walk reserved its whole map (it loads
					// the chain itself) and every OTHER caller saw a short one. The delete
					// path is where that mattered - it can only drop the blocks it can see.
					self.load_spill(&mut inode)?;
				}
				self.icache_put(num, inode.clone());
				Ok(inode)
			}
			None => Err(FsError::Invalid),
		}
	}

	// Remember inode `num`, evicting the LARGEST cached number once the cache is full
	// (inode 0 - the root directory, the hottest entry on every path resolution - and
	// the low, old numbers stay put) and skipping a pathologically fragmented extent
	// map (the cache only skips re-reads).
	pub(crate) fn icache_put(&mut self, num: u32, inode: Inode) {
		if inode.extents.len() > ICACHE_EXTENTS_MAX {
			self.icache.remove(&num);
			return;
		}
		if self.icache.len() >= ICACHE_MAX && !self.icache.contains_key(&num) {
			if let Some(k) = self.icache.keys().next_back().cloned() {
				self.icache.remove(&k);
			}
		}
		self.icache.insert(num, inode);
	}

	// Append the spilled extents (those past EXTENTS_INLINE) from the overflow chain to
	// `inode.extents`, which `parse` filled only with the inline runs. Each chain block
	// carries the (pointer, CRC32C) of the next, so a flipped bit in the chain is caught.
	pub(crate) fn load_spill(&mut self, inode: &mut Inode) -> Result<(), FsError> {
		if inode.extent_count as usize <= inode.extents.len() {
			return Ok(());
		}
		// An extent names a run of blocks, so an inode cannot hold more extents than the pool holds
		// blocks - a bound the VOLUME states, unlike the `u32` in the header, which is a width
		// rather than a limit anybody chose. Reached from `derive_free` at mount, so this is not
		// confined to reading a hostile file: one inode with a long spill chain costs the
		// allocation before the mount finishes.
		if inode.extent_count as u64 > self.num_blocks {
			return Err(FsError::Corrupt);
		}
		let mut ptr = inode.spill;
		let mut crc = inode.spill_crc;
		let mut buf = vec![0u8; BLOCK_SIZE];
		let mut steps = 0u64;
		while ptr != 0 {
			// bound the walk like `walk_chain`: a pointer outside the pool is damage,
			// and no chain can be longer than the pool - a CRC-consistent forged cycle
			// (checksums prove integrity, not sanity) must not hang every read of the
			// inode.
			if ptr >= self.num_blocks || steps >= self.num_blocks {
				return Err(FsError::Corrupt);
			}
			steps += 1;
			if !self.dev.read_block(ptr, &mut buf) {
				return Err(FsError::Io);
			}
			if crc32c(&buf) != crc {
				return Err(FsError::Corrupt);
			}
			// the count is read off the medium (a checksum proves integrity, not
			// sanity), and a count that cannot be true is REFUSED rather than trimmed.
			// Clamping produced a possible number out of an impossible one, so the
			// structural pass then had nothing to disagree with: a chain block claiming
			// more records than fit in a block, or more than the inode is still missing,
			// read as a perfectly ordinary short block. This is the same treatment
			// `Extent::parse` was given - keep what is on the medium, and say Corrupt
			// when the medium says something this format cannot have written.
			let want = (inode.extent_count as usize).saturating_sub(inode.extents.len());
			let count = u32::from_le_bytes(buf[CHAIN_COUNT_OFF..CHAIN_COUNT_OFF + 4].try_into().unwrap()) as usize;
			if count > EXTENTS_PER_BLOCK || count > want {
				return Err(FsError::Corrupt);
			}
			// Fallible, because the count comes off the medium. `read_file` and `read_range` were
			// given this discipline and the two metadata chain walks were not, which left the
			// mount reachable through the one place the caller cannot decline.
			if inode.extents.try_reserve(count).is_err() {
				// The MEMORY, not the medium: this is a `Vec` that could not grow, and telling a
				// caller the disk is full sends it deleting files that would not have helped.
				return Err(FsError::NoMemory);
			}
			for i in 0..count {
				let off = CHAIN_HDR + i * EXTENT_SIZE;
				inode.extents.push(Extent::parse(&buf[off..off + EXTENT_SIZE]));
			}
			ptr = u64::from_le_bytes(buf[CHAIN_NEXT_OFF..CHAIN_NEXT_OFF + 8].try_into().unwrap());
			crc = u32::from_le_bytes(buf[CHAIN_CRC_OFF..CHAIN_CRC_OFF + 4].try_into().unwrap());
		}
		Ok(())
	}

	// Persist `inode.extents` past the inline ones into a fresh overflow chain (one
	// block per EXTENTS_PER_BLOCK runs) and set the `spill` / `spill_crc` /
	// `extent_count` header fields to match. The superseded chain's blocks leave the
	// new generation. The chain is built back to front so each block can hold the
	// (pointer, CRC32C) of the one after it. Always called by `write_inode`, so the
	// inode slot and chain stay consistent.
	pub(crate) fn flush_extents(&mut self, inode: &mut Inode) -> Result<(), FsError> {
		// the rebuilt chain replaces the old one wholesale: drop the old blocks.
		self.walk_chain(inode.spill, |fs, ptr| fs.drop_block(ptr))?;
		inode.extent_count = inode.extents.len() as u32;
		if inode.extents.len() <= EXTENTS_INLINE {
			inode.spill = 0;
			inode.spill_crc = 0;
			return Ok(());
		}
		// Chunked over the extent map IN PLACE, with no copy of it.
		//
		// This used to be `inode.extents[EXTENTS_INLINE..].to_vec()`, and the copy was the last
		// infallible allocation on the write side: an ordinary file makes it invisible, a heavily
		// fragmented one makes it large, and an allocator that refuses aborts the service instead of
		// returning `NoSpace`. P02M0123 applied that rule to the mount side; this is the other side of
		// the same structure. The copy was never needed - the chunks are read once, in order, and
		// `Extent` is `Copy` - so the fix removes an allocation rather than making one fallible.
		let spilled = inode.extents.len() - EXTENTS_INLINE;
		let blocks = spilled.div_ceil(EXTENTS_PER_BLOCK);
		let mut next_ptr = 0u64;
		let mut next_crc = 0u32;
		// Back to front, so each block can carry the pointer and CRC of the one after it.
		for block in (0..blocks).rev() {
			let from = EXTENTS_INLINE + block * EXTENTS_PER_BLOCK;
			let to = (from + EXTENTS_PER_BLOCK).min(inode.extents.len());
			let chunk = &inode.extents[from..to];
			let blk = self.alloc_meta()?;
			let mut buf = try_zeroed(BLOCK_SIZE)?;
			buf[CHAIN_NEXT_OFF..CHAIN_NEXT_OFF + 8].copy_from_slice(&next_ptr.to_le_bytes());
			buf[CHAIN_CRC_OFF..CHAIN_CRC_OFF + 4].copy_from_slice(&next_crc.to_le_bytes());
			buf[CHAIN_COUNT_OFF..CHAIN_COUNT_OFF + 4].copy_from_slice(&(chunk.len() as u32).to_le_bytes());
			for (i, ext) in chunk.iter().enumerate() {
				let off = CHAIN_HDR + i * EXTENT_SIZE;
				ext.write(&mut buf[off..off + EXTENT_SIZE]);
			}
			if !self.dev.write_block(blk, &buf) {
				return Err(FsError::Io);
			}
			next_ptr = blk;
			next_crc = crc32c(&buf);
		}
		inode.spill = next_ptr;
		inode.spill_crc = next_crc;
		Ok(())
	}

	// Write inode `num` into the inode B+tree, rebuilding its extent overflow chain
	// first (for a file) so the inode slot and chain agree. The insert copies every tree
	// node on the path up to a fresh block and updates `inode_root`; the change is
	// published by `commit`.
	pub(crate) fn write_inode(&mut self, num: u32, inode: &mut Inode) -> Result<(), FsError> {
		if inode.r#type == TYPE_FILE {
			self.flush_extents(inode)?;
		}
		let mut rec = vec![0u8; INODE_REC];
		rec[0..8].copy_from_slice(&(num as u64).to_le_bytes());
		inode.write(&mut rec[8..8 + INODE_SIZE]);
		let (root, crc) = self.tree_insert(self.inode_root, self.inode_root_crc, num as u64, &rec, INODE_REC, INODE_LEAF_MAX, INODE_KEYLEN)?;
		self.inode_root = root;
		self.inode_root_crc = crc;
		self.icache_put(num, inode.clone());
		Ok(())
	}

	// Hand out a fresh inode number from the monotonic counter (never reused). The
	// caller writes the inode right after, so nothing is inserted into the tree here.
	// Inode numbers are handed out MONOTONICALLY and never reused, so the counter is a
	// lifetime budget rather than a concurrent-file limit: a volume that churns temporary
	// files exhausts it however empty the disk is, at about 4.3 billion creations. That
	// is a documented limit of this format version, not an oversight - reuse would need
	// either a free-inode map or a generation number beside each record, and widening the
	// field is a format revision. The refusal below is what keeps it from wrapping into
	// an inode that already exists.
	pub(crate) fn alloc_inode(&mut self) -> Result<u32, FsError> {
		let num = self.next_inode;
		if num == u32::MAX {
			return Err(FsError::NoSpace);
		}
		self.next_inode += 1;
		Ok(num)
	}

	// Remove inode `num` from the inode B+tree (its data blocks are recorded dropped by
	// the caller; the previous generation pins them until the commit after next).
	pub(crate) fn free_inode(&mut self, num: u32) -> Result<(), FsError> {
		let probe = (num as u64).to_le_bytes();
		let (root, crc, _) = self.tree_delete(self.inode_root, self.inode_root_crc, num as u64, &probe, INODE_REC, INODE_KEYLEN)?;
		self.inode_root = root;
		self.inode_root_crc = crc;
		self.icache.remove(&num);
		Ok(())
	}
}
