use crate::*;

impl<D: BlockDevice> LiberFs<D> {
	pub(crate) fn is_alloc(&self, block: u64) -> bool {
		self.free[(block / 8) as usize] & (1 << (block % 8)) != 0
	}

	// Mark `block` used and record it as fresh (allocated by this transaction, so safe
	// to overwrite in place).
	fn claim(&mut self, block: u64) {
		self.free[(block / 8) as usize] |= 1 << (block % 8);
		self.fresh.insert(block);
	}

	// Claim one free block. Data blocks are taken from the low end of the pool and
	// metadata (checksum, extent-overflow, inode-table, index) from the high end, so a
	// run of data blocks stays physically contiguous and coalesces into one extent
	// instead of being split by interleaved metadata. Each side resumes at a next-fit
	// cursor (wrapping once), scanning the bitmap a byte at a time, so an allocation
	// is O(1) amortized instead of a fresh scan of the whole pool.
	pub(crate) fn alloc_block(&mut self, meta: bool) -> Result<u64, FsError> {
		if meta {
			if let Some(block) = self.scan_down(self.meta_cursor).or_else(|| self.scan_down(self.num_blocks - 1)) {
				self.meta_cursor = block;
				self.claim(block);
				return Ok(block);
			}
		} else if let Some(block) = self.scan_up(self.data_cursor).or_else(|| self.scan_up(POOL_START)) {
			self.data_cursor = block + 1;
			self.claim(block);
			return Ok(block);
		}
		Err(FsError::NoSpace)
	}

	// The first free block at or above `from` (data side), scanning whole bitmap bytes
	// and finishing with trailing_zeros on the first byte with a free bit.
	fn scan_up(&self, from: u64) -> Option<u64> {
		let mut block = from.max(POOL_START);
		if block >= self.num_blocks {
			return None;
		}
		// finish the partial leading byte first.
		while block < self.num_blocks && block % 8 != 0 {
			if !self.is_alloc(block) {
				return Some(block);
			}
			block += 1;
		}
		let mut byte = (block / 8) as usize;
		let last = ((self.num_blocks - 1) / 8) as usize;
		while byte <= last {
			if self.free[byte] != 0xFF {
				let candidate = byte as u64 * 8 + (!self.free[byte]).trailing_zeros() as u64;
				if candidate < self.num_blocks {
					return Some(candidate);
				}
				return None;
			}
			byte += 1;
		}
		None
	}

	// The first free block at or below `from` (metadata side), scanning bitmap bytes
	// downward and finishing with leading_zeros on the first byte with a free bit.
	fn scan_down(&self, from: u64) -> Option<u64> {
		let mut block = from.min(self.num_blocks - 1);
		if block <= POOL_START {
			return None;
		}
		// finish the partial trailing byte first.
		while block > POOL_START && block % 8 != 7 {
			if !self.is_alloc(block) {
				return Some(block);
			}
			block -= 1;
		}
		let mut byte = (block / 8) as isize;
		let first = (POOL_START / 8) as isize;
		while byte >= first {
			if self.free[byte as usize] != 0xFF {
				let candidate = byte as u64 * 8 + (7 - (!self.free[byte as usize]).leading_zeros() as u64);
				if candidate > POOL_START {
					return Some(candidate);
				}
				return None;
			}
			byte -= 1;
		}
		None
	}

	pub(crate) fn alloc_data(&mut self) -> Result<u64, FsError> {
		// consume the reserved run first: a whole-file write claimed its span up front,
		// so its blocks come out consecutive by construction.
		if let Some((next, remaining)) = self.run {
			self.run = if remaining > 1 { Some((next + 1, remaining - 1)) } else { None };
			return Ok(next);
		}
		self.alloc_block(false)
	}

	pub(crate) fn alloc_meta(&mut self) -> Result<u64, FsError> {
		self.alloc_block(true)
	}

	// Reserve `len` consecutive free data blocks and hand them to `alloc_data` one at a
	// time: a whole-file write lands contiguously (one extent per checksum-block span)
	// instead of trusting the cursor not to be interrupted. Claims the whole run up
	// front (fresh, so an abort releases it). No-op when no contiguous run exists - the
	// write falls back to per-block allocation.
	pub(crate) fn reserve_run(&mut self, len: usize) {
		// the run count is stored as u32: clamp an absurd reservation (a single write
		// past 16 TiB) rather than silently truncating the claim/release accounting.
		let len = len.min(u32::MAX as usize);
		if len < 2 || self.run.is_some() {
			return;
		}
		if let Some(start) = self.find_run(len) {
			for b in start..start + len as u64 {
				self.claim(b);
			}
			self.run = Some((start, len as u32));
			self.data_cursor = start + len as u64;
		}
	}

	// Release whatever remains of the reserved run (a write that ended early, or a
	// transaction rollback): the unconsumed blocks return to the pool.
	pub(crate) fn release_run(&mut self) {
		if let Some((next, remaining)) = self.run.take() {
			for b in next..next + remaining as u64 {
				self.unclaim(b);
			}
		}
	}

	// The start of the first run of `len` consecutive free blocks at or above the data
	// cursor (wrapping once), or None. Scans free bytes (0x00 = eight free blocks) so
	// long runs are found a byte at a time.
	fn find_run(&self, len: usize) -> Option<u64> {
		let scan = |from: u64| -> Option<u64> {
			let mut start = from.max(POOL_START);
			let mut count = 0usize;
			let mut block = start;
			while block < self.num_blocks {
				if self.is_alloc(block) {
					count = 0;
					// skip whole allocated bytes.
					if block % 8 == 0 {
						let mut byte = (block / 8) as usize;
						let last = ((self.num_blocks - 1) / 8) as usize;
						while byte <= last && self.free[byte] == 0xFF {
							byte += 1;
						}
						block = (byte as u64 * 8).max(block + 1);
					} else {
						block += 1;
					}
					start = block;
					continue;
				}
				count += 1;
				if count == len {
					return Some(start);
				}
				block += 1;
			}
			None
		};
		scan(self.data_cursor).or_else(|| scan(POOL_START))
	}

	// Release a block this transaction claimed but ended up not referencing (a
	// contiguity gap while assembling a compressed run, or an unconsumed reservation):
	// clear its free-map bit and forget it was fresh, so the pool does not carry dead
	// claims until commit. A released block may be re-allocated and rewritten at once,
	// so any cache holding its old content must go with it.
	pub(crate) fn unclaim(&mut self, block: u64) {
		self.free[(block / 8) as usize] &= !(1 << (block % 8));
		self.fresh.remove(&block);
		self.forget_block(block);
	}

	// Drop every cache entry describing `block`: called when the block is released or
	// stops being referenced, so no cache can outlive the bytes it describes.
	pub(crate) fn forget_block(&mut self, block: u64) {
		if matches!(&self.wcsum, Some((wp, _)) if *wp == block) {
			self.wcsum = None;
		}
		if matches!(&self.rcsum, Some((rp, _, _)) if *rp == block) {
			self.rcsum = None;
		}
		if matches!(&self.decomp, Some((dp, _)) if *dp == block) {
			self.decomp = None;
		}
	}

	// Record that the in-flight transaction stopped referencing `ptr`. A block the
	// transaction itself allocated simply returns to the pool; a committed block joins
	// the `dead` list - the superseded generation still references it as the rolling
	// snapshot, so the commit after next frees it (unless a named snapshot pins it).
	// This is what keeps the free map exact without rewalking the volume at commit.
	pub(crate) fn drop_block(&mut self, ptr: u64) {
		if ptr == 0 {
			return;
		}
		if self.fresh.contains(&ptr) {
			self.unclaim(ptr);
		} else {
			self.dead.insert(ptr);
			self.forget_block(ptr);
		}
	}

	// Walk a chain of blocks (the shared CHAIN_* header) from `start`, calling `f` with
	// each block's pointer before following its next pointer. The walk is raw (no CRC
	// check, matching the old-generation walks) and stops at a pointer outside the
	// pool, so a corrupt link never panics the walker or wanders into garbage. The one
	// skeleton behind dropping, marking, and releasing every chain.
	pub(crate) fn walk_chain(&mut self, start: u64, mut f: impl FnMut(&mut Self, u64)) -> Result<(), FsError> {
		let mut ptr = start;
		let mut buf = vec![0u8; BLOCK_SIZE];
		while ptr != 0 && ptr < self.num_blocks {
			f(self, ptr);
			if !self.dev.read_block(ptr, &mut buf) {
				return Err(FsError::Io);
			}
			ptr = u64::from_le_bytes(buf[CHAIN_NEXT_OFF..CHAIN_NEXT_OFF + 8].try_into().unwrap());
		}
		Ok(())
	}

	// Drop every block a file inode references: each run's stored blocks and its
	// checksum block, plus the extent overflow chain. The complement of
	// `collect_inode_blocks`, for a file being deleted or wholly replaced. The extent
	// map must be complete (`load_spill` already run).
	pub(crate) fn drop_inode_blocks(&mut self, inode: &Inode) -> Result<(), FsError> {
		for i in 0..inode.extents.len() {
			let ext = inode.extents[i];
			for off in 0..ext.store_len as u64 {
				self.drop_block(ext.physical + off);
			}
			self.drop_block(ext.csum);
		}
		self.walk_chain(inode.spill, |fs, ptr| fs.drop_block(ptr))
	}

	// Copy-on-write a block reference. A pointer this transaction already allocated is
	// returned as is (safe to mutate in place). A committed block (or the 0 "unmapped"
	// sentinel) is copied up to a fresh block (data low, metadata high) and the old
	// contents copied into it (or zeroed), so the committed generation keeps the
	// original untouched - and the original is recorded dropped, since the new
	// generation references the copy instead. The copy rides the reusable scratch
	// buffer: this runs once per overwritten block, so it must not allocate.
	pub(crate) fn cow_block(&mut self, ptr: u64, meta: bool) -> Result<u64, FsError> {
		if ptr != 0 && self.fresh.contains(&ptr) {
			return Ok(ptr);
		}
		let fresh = self.alloc_block(meta)?;
		let mut buf = core::mem::take(&mut self.scratch);
		if buf.len() != BLOCK_SIZE {
			buf = vec![0u8; BLOCK_SIZE];
		}
		let ok = if ptr != 0 {
			self.read_block_csum_aware(ptr, &mut buf)
		} else {
			// the scratch may hold a previous copy: the unmapped sentinel means zeros.
			buf.fill(0);
			true
		};
		let ok = ok && self.dev.write_block(fresh, &buf);
		self.scratch = buf;
		if !ok {
			return Err(FsError::Io);
		}
		self.drop_block(ptr);
		Ok(fresh)
	}

	pub(crate) fn cow_meta(&mut self, ptr: u64) -> Result<u64, FsError> {
		self.cow_block(ptr, true)
	}

	// Read block `ptr`, serving the in-flight checksum write cache first: a fresh
	// checksum block being assembled lives in memory until eviction or commit, so a
	// read of it must not go to the (stale) device copy.
	pub(crate) fn read_block_csum_aware(&mut self, ptr: u64, buf: &mut [u8]) -> bool {
		if let Some((wp, bytes)) = &self.wcsum {
			if *wp == ptr {
				buf[..BLOCK_SIZE].copy_from_slice(bytes);
				return true;
			}
		}
		self.dev.read_block(ptr, buf)
	}

	// Write the in-flight checksum block to the device, if one is pending. Called on
	// eviction (a different checksum block is touched) and before the commit barrier.
	pub(crate) fn flush_wcsum(&mut self) -> Result<(), FsError> {
		if let Some((ptr, bytes)) = self.wcsum.take() {
			if !self.dev.write_block(ptr, &bytes) {
				return Err(FsError::Io);
			}
		}
		Ok(())
	}

	// Read the CRC32C of an extent's block at slot `slot` from its checksum block,
	// verifying that block's own checksum first (so a flipped bit in the checksum
	// metadata is caught, not silently trusted). The in-flight write cache serves a
	// fresh block being assembled; a committed block is verified once and then served
	// from the read cache for the rest of a sequential run.
	pub(crate) fn read_csum(&mut self, csum: u64, csum_crc: u32, slot: usize) -> Result<u32, FsError> {
		let off = slot * 4;
		if let Some((wp, bytes)) = &self.wcsum {
			if *wp == csum {
				return Ok(u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()));
			}
		}
		if let Some((rp, rcrc, bytes)) = &self.rcsum {
			if *rp == csum && *rcrc == csum_crc {
				return Ok(u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()));
			}
		}
		let mut buf = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(csum, &mut buf) {
			return Err(FsError::Io);
		}
		if crc32c(&buf) != csum_crc {
			return Err(FsError::Corrupt);
		}
		let crc = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
		// safe to cache even a fresh block: the hit is keyed by the expected CRC32C, so
		// a later in-place edit (which changes the extent's csum_crc) misses and re-reads
		// - and the in-flight wcsum copy always shadows this cache anyway.
		self.rcsum = Some((csum, csum_crc, buf));
		Ok(crc)
	}

	// Set slot `slot` of checksum block `csum` (always fresh - the callers copy a
	// committed block up first) to `crc` and return the block's new CRC32C (the
	// extent's `csum_crc`). The edit happens in the in-flight write cache; the device
	// sees the block once, on eviction or at commit, so a sequential run of writes
	// costs one device write instead of a read-modify-write per block.
	pub(crate) fn set_csum_slot(&mut self, csum: u64, slot: usize, crc: u32) -> Result<u32, FsError> {
		let cached = matches!(&self.wcsum, Some((wp, _)) if *wp == csum);
		if !cached {
			self.flush_wcsum()?;
			let mut buf = vec![0u8; BLOCK_SIZE];
			if !self.dev.read_block(csum, &mut buf) {
				return Err(FsError::Io);
			}
			self.wcsum = Some((csum, buf));
		}
		let bytes = &mut self.wcsum.as_mut().unwrap().1;
		let off = slot * 4;
		bytes[off..off + 4].copy_from_slice(&crc.to_le_bytes());
		Ok(crc32c(bytes))
	}

	// file block mapping (extents)

	// Read logical block `logical` of `inode` into `buf` via its extent map, verifying
	// the per-block checksum. Returns false (and leaves `buf` untouched) for a hole - a
	// logical block no extent covers, which the caller reads back as zeros. A checksum
	// mismatch is `FsError::Corrupt`.
	pub(crate) fn read_logical(&mut self, inode: &Inode, logical: usize, buf: &mut [u8]) -> Result<bool, FsError> {
		let lb = logical as u64;
		let ext = match find_extent(&inode.extents, lb) {
			Some(i) => inode.extents[i],
			None => return Ok(false),
		};
		if ext.clen != 0 {
			// a compressed run: serve the block from the whole extent's decompressed
			// image, decoding once and caching it for the rest of a sequential read.
			let cached = matches!(&self.decomp, Some((key, _)) if *key == ext.physical);
			if !cached {
				let decoded = self.decompress_extent(&ext)?;
				self.decomp = Some((ext.physical, decoded));
			}
			let data = &self.decomp.as_ref().unwrap().1;
			let start = (lb - ext.logical) as usize * BLOCK_SIZE;
			buf.fill(0);
			if start < data.len() {
				let end = (start + BLOCK_SIZE).min(data.len());
				buf[..end - start].copy_from_slice(&data[start..end]);
			}
			return Ok(true);
		}
		let off = (lb - ext.logical) as usize;
		if !self.dev.read_block(ext.physical + off as u64, buf) {
			return Err(FsError::Io);
		}
		let crc = self.read_csum(ext.csum, ext.csum_crc, off)?;
		if crc32c(buf) != crc {
			return Err(FsError::Corrupt);
		}
		Ok(true)
	}

	// Write `buf` as logical block `logical` of `inode`, updating the extent map in
	// memory and recording the block's checksum. Overwriting a mapped block replaces it
	// with a fresh one (never copying the old contents - `buf` is always a whole block,
	// so a copy would be overwritten immediately; a block already fresh this transaction
	// is rewritten in place) and may split its run; writing a hole appends to the run
	// before it when the new block is physically contiguous, otherwise starts a new run.
	// The caller persists the inode, which flushes the map to disk.
	pub(crate) fn write_logical(&mut self, inode: &mut Inode, logical: usize, buf: &[u8]) -> Result<(), FsError> {
		let lb = logical as u64;
		// a compressed run cannot be edited in place: thaw it back to raw blocks first, so
		// this overwrite (and any later block of the run) proceeds on a raw extent.
		if let Some(i) = find_extent(&inode.extents, lb) {
			if inode.extents[i].clen != 0 {
				self.thaw_extent(inode, i)?;
			}
		}
		let crc = crc32c(buf);
		if let Some(i) = find_extent(&inode.extents, lb) {
			let ext = inode.extents[i];
			let off = (lb - ext.logical) as usize;
			let old = ext.physical + off as u64;
			// a fresh block is rewritten in place; a committed one is replaced by a
			// fresh allocation and recorded dropped - the whole block is about to be
			// written, so copying the old contents up would be wasted device work.
			let new_phys = if self.fresh.contains(&old) {
				old
			} else {
				let fresh = self.alloc_data()?;
				self.drop_block(old);
				fresh
			};
			if !self.dev.write_block(new_phys, buf) {
				return Err(FsError::Io);
			}
			self.overwrite_block(inode, i, off, new_phys, crc)?;
			return Ok(());
		}
		let phys = self.alloc_data()?;
		if !self.dev.write_block(phys, buf) {
			return Err(FsError::Io);
		}
		self.place_block(inode, lb, phys, crc)
	}

	// Record a freshly allocated data block `phys` as logical block `lb` of `inode`,
	// extending the run that ends at `lb` when it is physically contiguous and still has
	// room in its checksum block, or inserting a new single-block run otherwise.
	pub(crate) fn place_block(&mut self, inode: &mut Inode, lb: u64, phys: u64, crc: u32) -> Result<(), FsError> {
		let pos = inode.extents.partition_point(|e| e.logical <= lb);
		if pos > 0 {
			let prev = inode.extents[pos - 1];
			if prev.clen == 0 && prev.end() == lb && prev.physical + prev.length as u64 == phys && (prev.length as usize) < CRCS_PER_BLOCK {
				let csum = self.cow_meta(prev.csum)?;
				let csum_crc = self.set_csum_slot(csum, prev.length as usize, crc)?;
				let e = &mut inode.extents[pos - 1];
				e.length += 1;
				e.store_len += 1;
				e.csum = csum;
				e.csum_crc = csum_crc;
				return Ok(());
			}
		}
		let csum = self.alloc_meta()?;
		let mut cbuf = vec![0u8; BLOCK_SIZE];
		cbuf[0..4].copy_from_slice(&crc.to_le_bytes());
		let csum_crc = crc32c(&cbuf);
		// seed the in-flight write cache instead of writing the device: the run's next
		// blocks edit this same checksum block, and the flush (eviction or commit)
		// writes it once.
		self.flush_wcsum()?;
		self.wcsum = Some((csum, cbuf));
		inode.extents.insert(pos, Extent { logical: lb, physical: phys, length: 1, csum, csum_crc, store_len: 1, clen: 0 });
		Ok(())
	}

	// Apply an overwrite of the block at offset `off` in extent `i`, now living at
	// `new_phys`. If the block did not move (it was already fresh this transaction) the
	// run is intact and only its checksum changes; otherwise the run splits into the
	// unchanged prefix, the single rewritten block, and the unchanged suffix, copying
	// the checksum sub-ranges so every block keeps its CRC.
	pub(crate) fn overwrite_block(&mut self, inode: &mut Inode, i: usize, off: usize, new_phys: u64, crc: u32) -> Result<(), FsError> {
		let ext = inode.extents[i];
		if new_phys == ext.physical + off as u64 {
			let csum = self.cow_meta(ext.csum)?;
			let csum_crc = self.set_csum_slot(csum, off, crc)?;
			let e = &mut inode.extents[i];
			e.csum = csum;
			e.csum_crc = csum_crc;
			return Ok(());
		}
		let mut old_csum = vec![0u8; BLOCK_SIZE];
		if !self.read_block_csum_aware(ext.csum, &mut old_csum) {
			return Err(FsError::Io);
		}
		if crc32c(&old_csum) != ext.csum_crc {
			return Err(FsError::Corrupt);
		}
		let mut pieces: Vec<Extent> = Vec::new();
		if off > 0 {
			// the prefix is unchanged: reuse the original checksum block (its leading
			// slots still match the kept blocks).
			pieces.push(Extent { logical: ext.logical, physical: ext.physical, length: off as u32, csum: ext.csum, csum_crc: ext.csum_crc, store_len: off as u32, clen: 0 });
		} else {
			// no prefix piece: nothing in the new generation references the original
			// checksum block any more.
			self.drop_block(ext.csum);
		}
		// the rewritten block gets a fresh single-entry checksum block.
		let mid_csum = self.alloc_meta()?;
		let mut cbuf = vec![0u8; BLOCK_SIZE];
		cbuf[0..4].copy_from_slice(&crc.to_le_bytes());
		if !self.dev.write_block(mid_csum, &cbuf) {
			return Err(FsError::Io);
		}
		pieces.push(Extent { logical: ext.logical + off as u64, physical: new_phys, length: 1, csum: mid_csum, csum_crc: crc32c(&cbuf), store_len: 1, clen: 0 });
		if off + 1 < ext.length as usize {
			let slen = ext.length as usize - off - 1;
			let suf_csum = self.alloc_meta()?;
			let mut sbuf = vec![0u8; BLOCK_SIZE];
			// copy the original CRCs of the suffix down to the start of the new block.
			sbuf[0..slen * 4].copy_from_slice(&old_csum[(off + 1) * 4..(off + 1 + slen) * 4]);
			if !self.dev.write_block(suf_csum, &sbuf) {
				return Err(FsError::Io);
			}
			pieces.push(Extent { logical: ext.logical + off as u64 + 1, physical: ext.physical + off as u64 + 1, length: slen as u32, csum: suf_csum, csum_crc: crc32c(&sbuf), store_len: slen as u32, clen: 0 });
		}
		inode.extents.splice(i..i + 1, pieces);
		Ok(())
	}

	// Decompress a compressed extent's stored blocks and rewrite its span as a raw 1:1
	// run (each logical block its own fresh data block with a per-block checksum),
	// dropping the compressed record. Editing a compressed file falls back to raw; a
	// later whole-file write recompresses it. The old stored and checksum blocks become
	// unreferenced and are reclaimed when the free map is rederived at commit.
	pub(crate) fn thaw_extent(&mut self, inode: &mut Inode, i: usize) -> Result<(), FsError> {
		let ext = inode.extents[i];
		let decoded = self.decompress_extent(&ext)?;
		inode.extents.remove(i);
		// the compressed record's stored blocks and checksum block leave the new
		// generation with it.
		for s in 0..ext.store_len as u64 {
			self.drop_block(ext.physical + s);
		}
		self.drop_block(ext.csum);
		let mut blk = vec![0u8; BLOCK_SIZE];
		for lo in 0..ext.length as usize {
			blk.fill(0);
			let start = lo * BLOCK_SIZE;
			if start < decoded.len() {
				let end = (start + BLOCK_SIZE).min(decoded.len());
				blk[..end - start].copy_from_slice(&decoded[start..end]);
			}
			let crc = crc32c(&blk);
			let phys = self.alloc_data()?;
			if !self.dev.write_block(phys, &blk) {
				return Err(FsError::Io);
			}
			self.place_block(inode, ext.logical + lo as u64, phys, crc)?;
		}
		Ok(())
	}

	// Read and verify the stored (compressed) blocks of a compressed extent, then decode
	// them into the run's uncompressed image. Each stored block is checked against its
	// CRC32C in the checksum block, so corruption of the compressed bytes surfaces as
	// `FsError::Corrupt` rather than bad data.
	pub(crate) fn decompress_extent(&mut self, ext: &Extent) -> Result<Vec<u8>, FsError> {
		let mut cbuf = vec![0u8; BLOCK_SIZE];
		if !self.read_block_csum_aware(ext.csum, &mut cbuf) {
			return Err(FsError::Io);
		}
		if crc32c(&cbuf) != ext.csum_crc {
			return Err(FsError::Corrupt);
		}
		let mut comp = vec![0u8; ext.store_len as usize * BLOCK_SIZE];
		for s in 0..ext.store_len as usize {
			let dst = &mut comp[s * BLOCK_SIZE..(s + 1) * BLOCK_SIZE];
			if !self.dev.read_block(ext.physical + s as u64, dst) {
				return Err(FsError::Io);
			}
			let stored = u32::from_le_bytes(cbuf[s * 4..s * 4 + 4].try_into().unwrap());
			if crc32c(dst) != stored {
				return Err(FsError::Corrupt);
			}
		}
		// clen is CRC-protected with the extent record, but bound it defensively anyway:
		// a slice past the stored bytes must never panic.
		let clen = (ext.clen as usize).min(comp.len());
		Ok(lz_decompress(&comp[..clen]))
	}

	// Try to transparently compress each of a freshly written file's raw extents in
	// place: a run that shrinks to fewer blocks becomes a compressed record, an
	// incompressible one is left raw. Run as the last step of a whole-file write, so the
	// block-by-block writer stays simple and partial updates keep working on raw runs.
	pub(crate) fn compress_inode(&mut self, inode: &mut Inode) -> Result<(), FsError> {
		for i in 0..inode.extents.len() {
			self.compress_extent(inode, i)?;
		}
		Ok(())
	}

	// Compress raw extent `i` if its bytes shrink to fewer blocks. The compressed stream
	// is written across a contiguous run of fresh data blocks with one checksum block
	// (one CRC32C per stored block), and the extent rewritten to point at it; the old raw
	// blocks become unreferenced and are reclaimed at commit. The run stays raw if it is
	// a single block, does not shrink, or a contiguous stored run is unavailable. Every
	// source block is verified against its stored CRC32C first: compressing discards the
	// raw blocks' checksums, so re-encoding unverified bytes would launder a device
	// corruption into a compressed extent with a fresh, valid checksum - a mismatch
	// leaves the run raw, where a read or fsck still surfaces the damage.
	pub(crate) fn compress_extent(&mut self, inode: &mut Inode, i: usize) -> Result<(), FsError> {
		let ext = inode.extents[i];
		if ext.clen != 0 || ext.length < 2 {
			return Ok(());
		}
		let mut ubuf = vec![0u8; ext.length as usize * BLOCK_SIZE];
		for off in 0..ext.length as usize {
			let dst = &mut ubuf[off * BLOCK_SIZE..(off + 1) * BLOCK_SIZE];
			if !self.dev.read_block(ext.physical + off as u64, dst) {
				return Err(FsError::Io);
			}
			match self.read_csum(ext.csum, ext.csum_crc, off) {
				Ok(crc) if crc32c(dst) == crc => {}
				Ok(_) | Err(FsError::Corrupt) => return Ok(()),
				Err(e) => return Err(e),
			}
		}
		let comp = lz_compress(&ubuf);
		let store_len = comp.len().div_ceil(BLOCK_SIZE);
		if store_len >= ext.length as usize {
			return Ok(());
		}
		// claim a contiguous run of stored blocks (data is taken low-to-high, so fresh
		// data allocations run contiguously); on a gap, release the claims and leave the
		// run raw - nothing references them, so holding them would only waste the pool
		// until the commit rederivation.
		let first = self.alloc_data()?;
		let mut last = first;
		for _ in 1..store_len {
			let b = self.alloc_data()?;
			if b != last + 1 {
				for claimed in first..=last {
					self.unclaim(claimed);
				}
				self.unclaim(b);
				return Ok(());
			}
			last = b;
		}
		let mut blk = vec![0u8; BLOCK_SIZE];
		let mut cbuf = vec![0u8; BLOCK_SIZE];
		for s in 0..store_len {
			blk.fill(0);
			let start = s * BLOCK_SIZE;
			let end = (start + BLOCK_SIZE).min(comp.len());
			blk[..end - start].copy_from_slice(&comp[start..end]);
			if !self.dev.write_block(first + s as u64, &blk) {
				return Err(FsError::Io);
			}
			let crc = crc32c(&blk);
			cbuf[s * 4..s * 4 + 4].copy_from_slice(&crc.to_le_bytes());
		}
		let csum = self.alloc_meta()?;
		if !self.dev.write_block(csum, &cbuf) {
			return Err(FsError::Io);
		}
		// the raw run's blocks and checksum block leave the new generation with it.
		for off in 0..ext.length as u64 {
			self.drop_block(ext.physical + off);
		}
		self.drop_block(ext.csum);
		inode.extents[i] = Extent { logical: ext.logical, physical: first, length: ext.length, csum, csum_crc: crc32c(&cbuf), store_len: store_len as u32, clen: comp.len() as u32 };
		Ok(())
	}

	// Count the live data blocks of `inode` whose on-disk bytes no longer match the
	// CRC32C stored for them in their run's checksum block. A run whose checksum block
	// is itself corrupt counts as wholly bad. A compressed run is checked over its stored
	// (compressed) blocks, since those are the bytes the checksum covers.
	pub(crate) fn count_corrupt(&mut self, inode: &Inode) -> Result<u32, FsError> {
		let mut bad = 0;
		let mut buf = vec![0u8; BLOCK_SIZE];
		let mut cbuf = vec![0u8; BLOCK_SIZE];
		for i in 0..inode.extents.len() {
			let ext = inode.extents[i];
			if !self.read_block_csum_aware(ext.csum, &mut cbuf) {
				return Err(FsError::Io);
			}
			if crc32c(&cbuf) != ext.csum_crc {
				bad += ext.store_len;
				continue;
			}
			for off in 0..ext.store_len as usize {
				if !self.dev.read_block(ext.physical + off as u64, &mut buf) {
					return Err(FsError::Io);
				}
				let c = u32::from_le_bytes(cbuf[off * 4..off * 4 + 4].try_into().unwrap());
				if crc32c(&buf) != c {
					bad += 1;
				}
			}
		}
		Ok(bad)
	}
}

// The bit helpers tolerate an out-of-range block (a corrupt pointer read off a raw
// chain walk): setting is skipped, testing reads as allocated - so damage never
// panics and never frees a block it should not.
pub(crate) fn set_bit(bitmap: &mut [u8], b: u64) {
	let i = (b / 8) as usize;
	if i < bitmap.len() {
		bitmap[i] |= 1 << (b % 8);
	}
}

pub(crate) fn clear_bit(bitmap: &mut [u8], b: u64) {
	let i = (b / 8) as usize;
	if i < bitmap.len() {
		bitmap[i] &= !(1 << (b % 8));
	}
}

pub(crate) fn test_bit(bitmap: &[u8], b: u64) -> bool {
	let i = (b / 8) as usize;
	if i < bitmap.len() {
		bitmap[i] & (1 << (b % 8)) != 0
	} else {
		true
	}
}

// Index of the extent covering logical block `lb`, or None if it falls in a hole. The
// runs are sorted by `logical`, so the candidate is the last one starting at or before
// `lb`; a binary search keeps lookups cheap on a many-extent file.
pub(crate) fn find_extent(extents: &[Extent], lb: u64) -> Option<usize> {
	let pos = extents.partition_point(|e| e.logical <= lb);
	if pos == 0 {
		return None;
	}
	if extents[pos - 1].covers(lb) {
		Some(pos - 1)
	} else {
		None
	}
}

// Hash the 4-byte prefix at `w` into an LZ_HASH_BITS-wide match-finder bucket.
pub(crate) fn lz_hash(w: &[u8]) -> usize {
	let v = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
	(v.wrapping_mul(0x9E37_79B1) >> (32 - LZ_HASH_BITS)) as usize
}

// Compress `src` with the dependency-free LZ4 block-format coder described at the LZ_*
// constants. The stream begins with the uncompressed length, so `lz_decompress` needs
// no external size. A single-entry hash table finds the most recent position sharing a
// 4-byte prefix; every candidate is verified by comparing bytes, so the table only
// affects the ratio, never correctness. The format is the standard LZ4 block layout
// (token, literals, offset, extended match length), chosen over the old LZSS coder for
// a better ratio (64 KiB offsets, unbounded match lengths) at higher speed (token
// framing instead of per-item control bits).
pub(crate) fn lz_compress(src: &[u8]) -> Vec<u8> {
	let n = src.len();
	let mut out = Vec::with_capacity(n / 2 + 16);
	out.extend_from_slice(&(n as u32).to_le_bytes());
	let mut head = vec![-1i64; LZ_HASH_SIZE];
	let mut i = 0usize;
	let mut anchor = 0usize;
	// matches may neither start past this point nor extend into the last five bytes.
	let match_limit = n.saturating_sub(LZ_MATCH_MARGIN);
	let literal_end = n.saturating_sub(LZ_LAST_LITERALS);
	while i < match_limit {
		// find the most recent earlier position sharing our 4-byte prefix.
		let h = lz_hash(&src[i..]);
		let cand = head[h];
		head[h] = i as i64;
		let mut matched = 0usize;
		let mut offset = 0usize;
		if cand >= 0 {
			let c = cand as usize;
			let dist = i - c;
			if dist <= 0xFFFF && src[c..c + 4] == src[i..i + 4] {
				// verified 4-byte match: extend it as far as the margin allows.
				let mut l = 4;
				let max = literal_end - i;
				while l < max && src[c + l] == src[i + l] {
					l += 1;
				}
				matched = l;
				offset = dist;
			}
		}
		if matched < LZ_MIN_MATCH {
			i += 1;
			continue;
		}
		// one sequence: the literals since the last match, then this match.
		lz_put_sequence(&mut out, &src[anchor..i], offset, matched);
		// index a couple of positions inside the match so long runs stay findable.
		let end = i + matched;
		let mut p = i + 1;
		while p < end.min(match_limit) && p < i + 3 {
			head[lz_hash(&src[p..])] = p as i64;
			p += 1;
		}
		i = end;
		anchor = end;
	}
	// the trailing literals (always at least LZ_LAST_LITERALS when anything matched).
	lz_put_literals(&mut out, &src[anchor..]);
	out
}

// Append one LZ4 sequence: token, extended literal length, the literals, the 2-byte
// offset, and the extended match length (stored as `len - LZ_MIN_MATCH`).
fn lz_put_sequence(out: &mut Vec<u8>, literals: &[u8], offset: usize, matched: usize) {
	let ml = matched - LZ_MIN_MATCH;
	let lit_nibble = literals.len().min(15) as u8;
	let ml_nibble = ml.min(15) as u8;
	out.push(lit_nibble << 4 | ml_nibble);
	lz_put_extra(out, literals.len(), 15);
	out.extend_from_slice(literals);
	out.extend_from_slice(&(offset as u16).to_le_bytes());
	lz_put_extra(out, ml, 15);
}

// Append a literals-only tail sequence (no match: token's low nibble unused, no offset).
fn lz_put_literals(out: &mut Vec<u8>, literals: &[u8]) {
	if literals.is_empty() {
		return;
	}
	let lit_nibble = literals.len().min(15) as u8;
	out.push(lit_nibble << 4);
	lz_put_extra(out, literals.len(), 15);
	out.extend_from_slice(literals);
}

// LZ4 length extension: a nibble of 15 is followed by 255-valued bytes plus a final
// remainder byte until the full length is encoded.
fn lz_put_extra(out: &mut Vec<u8>, len: usize, nibble_max: usize) {
	if len < nibble_max {
		return;
	}
	let mut rest = len - nibble_max;
	while rest >= 255 {
		out.push(255);
		rest -= 255;
	}
	out.push(rest as u8);
}

// Decode a stream produced by `lz_compress` back into its original bytes. Bounds-checked
// throughout, so a malformed stream yields whatever decoded cleanly rather than panicking
// (a compressed extent's stored blocks are checksum-verified before this is called).
pub(crate) fn lz_decompress(src: &[u8]) -> Vec<u8> {
	if src.len() < 4 {
		return Vec::new();
	}
	let n = u32::from_le_bytes(src[0..4].try_into().unwrap()) as usize;
	let mut out = Vec::with_capacity(n);
	let mut p = 4;
	while out.len() < n && p < src.len() {
		let token = src[p];
		p += 1;
		// literals: high nibble, extended.
		let mut lit = (token >> 4) as usize;
		if lit == 15 {
			match lz_take_extra(src, &mut p) {
				Some(extra) => lit += extra,
				None => return out,
			}
		}
		if p + lit > src.len() {
			return out;
		}
		out.extend_from_slice(&src[p..p + lit]);
		p += lit;
		if out.len() >= n || p + 2 > src.len() {
			break;
		}
		// the match: 2-byte offset, then the extended length.
		let offset = u16::from_le_bytes([src[p], src[p + 1]]) as usize;
		p += 2;
		let mut ml = (token & 0x0F) as usize;
		if ml == 15 {
			match lz_take_extra(src, &mut p) {
				Some(extra) => ml += extra,
				None => return out,
			}
		}
		ml += LZ_MIN_MATCH;
		if offset == 0 || offset > out.len() {
			return out;
		}
		let start = out.len() - offset;
		for k in 0..ml {
			let byte = out[start + k];
			out.push(byte);
		}
	}
	out
}

// Read an LZ4 length extension at `*p`: 255-valued bytes plus the final remainder.
fn lz_take_extra(src: &[u8], p: &mut usize) -> Option<usize> {
	let mut extra = 0usize;
	loop {
		let b = *src.get(*p)?;
		*p += 1;
		extra += b as usize;
		if b != 255 {
			return Some(extra);
		}
	}
}
