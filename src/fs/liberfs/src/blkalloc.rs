use crate::*;

impl<D: BlockDevice> LiberFs<D> {
	pub(crate) fn is_alloc(&self, block: u64) -> bool {
		self.free[(block / 8) as usize] & (1 << (block % 8)) != 0
	}

	// Claim one free block, marking it used and recording it as fresh (allocated by
	// this transaction, so safe to overwrite in place). Data blocks are taken from the
	// low end of the pool and metadata (checksum, extent-overflow, inode-table, index)
	// from the high end, so a run of data blocks stays physically contiguous and
	// coalesces into one extent instead of being split by interleaved metadata.
	pub(crate) fn alloc_block(&mut self, meta: bool) -> Result<u64, FsError> {
		let claim = |free: &mut [u8], block: u64| {
			free[(block / 8) as usize] |= 1 << (block % 8);
		};
		if meta {
			let mut block = self.num_blocks;
			while block > POOL_START {
				block -= 1;
				if !self.is_alloc(block) {
					claim(&mut self.free, block);
					self.fresh.insert(block);
					return Ok(block);
				}
			}
		} else {
			for block in POOL_START..self.num_blocks {
				if !self.is_alloc(block) {
					claim(&mut self.free, block);
					self.fresh.insert(block);
					return Ok(block);
				}
			}
		}
		Err(FsError::NoSpace)
	}

	pub(crate) fn alloc_data(&mut self) -> Result<u64, FsError> {
		self.alloc_block(false)
	}

	pub(crate) fn alloc_meta(&mut self) -> Result<u64, FsError> {
		self.alloc_block(true)
	}

	// Copy-on-write a block reference. A pointer this transaction already allocated is
	// returned as is (safe to mutate in place). A committed block (or the 0 "unmapped"
	// sentinel) is copied up to a fresh block (data low, metadata high) and the old
	// contents copied into it (or zeroed), so the committed generation keeps the
	// original untouched.
	pub(crate) fn cow_block(&mut self, ptr: u64, meta: bool) -> Result<u64, FsError> {
		if ptr != 0 && self.fresh.contains(&ptr) {
			return Ok(ptr);
		}
		let fresh = self.alloc_block(meta)?;
		let mut buf = vec![0u8; BLOCK_SIZE];
		if ptr != 0 && !self.dev.read_block(ptr, &mut buf) {
			return Err(FsError::Io);
		}
		if !self.dev.write_block(fresh, &buf) {
			return Err(FsError::Io);
		}
		Ok(fresh)
	}

	pub(crate) fn cow_data(&mut self, ptr: u64) -> Result<u64, FsError> {
		self.cow_block(ptr, false)
	}

	pub(crate) fn cow_meta(&mut self, ptr: u64) -> Result<u64, FsError> {
		self.cow_block(ptr, true)
	}

	// Read the CRC32C of an extent's block at slot `slot` from its checksum block,
	// verifying that block's own checksum first (so a flipped bit in the checksum
	// metadata is caught, not silently trusted).
	pub(crate) fn read_csum(&mut self, csum: u64, csum_crc: u32, slot: usize) -> Result<u32, FsError> {
		let mut buf = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(csum, &mut buf) {
			return Err(FsError::Io);
		}
		if crc32c(&buf) != csum_crc {
			return Err(FsError::Corrupt);
		}
		let off = slot * 4;
		Ok(u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()))
	}

	// Set slot `slot` of checksum block `csum` to `crc` and return the block's new
	// CRC32C (the extent's `csum_crc`). The block is read, edited, and written back.
	pub(crate) fn set_csum_slot(&mut self, csum: u64, slot: usize, crc: u32) -> Result<u32, FsError> {
		let mut buf = vec![0u8; BLOCK_SIZE];
		if !self.dev.read_block(csum, &mut buf) {
			return Err(FsError::Io);
		}
		let off = slot * 4;
		buf[off..off + 4].copy_from_slice(&crc.to_le_bytes());
		if !self.dev.write_block(csum, &buf) {
			return Err(FsError::Io);
		}
		Ok(crc32c(&buf))
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
			for b in buf.iter_mut() {
				*b = 0;
			}
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
	// memory and recording the block's checksum. Overwriting a mapped block copies it
	// up (and may split its run); writing a hole appends to the run before it when the
	// new block is physically contiguous, otherwise starts a new run. The caller
	// persists the inode, which flushes the map to disk.
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
			let new_phys = self.cow_data(ext.physical + off as u64)?;
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
		if !self.dev.write_block(csum, &cbuf) {
			return Err(FsError::Io);
		}
		inode.extents.insert(pos, Extent { logical: lb, physical: phys, length: 1, csum, csum_crc: crc32c(&cbuf), store_len: 1, clen: 0 });
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
		if !self.dev.read_block(ext.csum, &mut old_csum) {
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
		let mut blk = vec![0u8; BLOCK_SIZE];
		for lo in 0..ext.length as usize {
			for b in blk.iter_mut() {
				*b = 0;
			}
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
		if !self.dev.read_block(ext.csum, &mut cbuf) {
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
		Ok(lz_decompress(&comp[..ext.clen as usize]))
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
	// a single block, does not shrink, or a contiguous stored run is unavailable.
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
		}
		let comp = lz_compress(&ubuf);
		let store_len = comp.len().div_ceil(BLOCK_SIZE);
		if store_len >= ext.length as usize {
			return Ok(());
		}
		// claim a contiguous run of stored blocks (data is taken low-to-high, so fresh
		// data allocations run contiguously); leave the run raw if a gap appears.
		let first = self.alloc_data()?;
		let mut last = first;
		for _ in 1..store_len {
			let b = self.alloc_data()?;
			if b != last + 1 {
				return Ok(());
			}
			last = b;
		}
		let mut blk = vec![0u8; BLOCK_SIZE];
		let mut cbuf = vec![0u8; BLOCK_SIZE];
		for s in 0..store_len {
			for b in blk.iter_mut() {
				*b = 0;
			}
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
		for ext in inode.extents.iter() {
			if !self.dev.read_block(ext.csum, &mut cbuf) {
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

pub(crate) fn set_bit(bitmap: &mut [u8], b: u64) {
	bitmap[(b / 8) as usize] |= 1 << (b % 8);
}

// Index of the extent covering logical block `lb`, or None if it falls in a hole. The
// runs are sorted by `logical`, so the candidate is the last one starting at or before
// `lb`; a binary search keeps lookups cheap on a many-extent file.
pub(crate) fn find_extent(extents: &[Extent], lb: u64) -> Option<usize> {
	let pos = extents.partition_point(|e| e.logical <= lb);
	if pos == 0 {
		return None;
	}
	if extents[pos - 1].covers(lb) { Some(pos - 1) } else { None }
}

// Hash the 3-byte prefix at `w` into an LZ_HASH_BITS-wide match-finder bucket.
pub(crate) fn lz_hash(w: &[u8]) -> usize {
	let v = (w[0] as u32) << 16 | (w[1] as u32) << 8 | w[2] as u32;
	(v.wrapping_mul(0x9E37_79B1) >> (32 - LZ_HASH_BITS)) as usize
}

// Record position `i` in the hash chain (most-recent-first) so later positions can find
// it as a match candidate. `prev` is windowed (LZ_WINDOW entries): within the active
// window the low bits of a position are unique, and the match walk stops once a candidate
// is more than a window back, so the wrap never yields a wrong match.
pub(crate) fn lz_insert(src: &[u8], i: usize, head: &mut [i32], prev: &mut [i32]) {
	if i + LZ_MIN_MATCH <= src.len() {
		let h = lz_hash(&src[i..]);
		prev[i & (LZ_WINDOW - 1)] = head[h];
		head[h] = i as i32;
	}
}

// Compress `src` with the small LZSS coder described at the LZ_* constants: a control
// byte per eight items, each a literal byte or a (distance, length) back-reference into
// the 4096-byte sliding window. The stream begins with the uncompressed length, so
// `lz_decompress` needs no external size. Dependency-free and no_std; the ratio is modest
// but the format is simple and the decoder trivial, which is what an on-disk filesystem
// wants. Every candidate match is verified by comparing bytes, so the windowed hash chain
// only affects the ratio, never correctness.
pub(crate) fn lz_compress(src: &[u8]) -> Vec<u8> {
	let n = src.len();
	let mut out = Vec::with_capacity(n / 2 + 8);
	out.extend_from_slice(&(n as u32).to_le_bytes());
	let mut head = vec![-1i32; LZ_HASH_SIZE];
	let mut prev = vec![-1i32; LZ_WINDOW];
	let mut i = 0usize;
	while i < n {
		let ctrl = out.len();
		out.push(0u8);
		let mut flags = 0u8;
		let mut bit = 0;
		while bit < 8 && i < n {
			let (mut best_len, mut best_dist) = (0usize, 0usize);
			if i + LZ_MIN_MATCH <= n {
				let mut cand = head[lz_hash(&src[i..])];
				let mut probes = 0;
				while cand >= 0 && probes < LZ_MAX_CHAIN {
					let c = cand as usize;
					let dist = i - c;
					if dist > LZ_WINDOW {
						break;
					}
					let max = (n - i).min(LZ_MAX_MATCH);
					let mut l = 0;
					while l < max && src[c + l] == src[i + l] {
						l += 1;
					}
					if l > best_len {
						best_len = l;
						best_dist = dist;
						if l == LZ_MAX_MATCH {
							break;
						}
					}
					cand = prev[c & (LZ_WINDOW - 1)];
					probes += 1;
				}
			}
			if best_len >= LZ_MIN_MATCH {
				let dist_code = (best_dist - 1) as u16;
				out.push((dist_code & 0xFF) as u8);
				out.push((((dist_code >> 8) as u8) << 4) | (best_len - LZ_MIN_MATCH) as u8);
				let end = i + best_len;
				while i < end {
					lz_insert(src, i, &mut head, &mut prev);
					i += 1;
				}
			} else {
				flags |= 1 << bit;
				out.push(src[i]);
				lz_insert(src, i, &mut head, &mut prev);
				i += 1;
			}
			bit += 1;
		}
		out[ctrl] = flags;
	}
	out
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
		let flags = src[p];
		p += 1;
		let mut bit = 0;
		while bit < 8 && out.len() < n {
			if flags & (1 << bit) != 0 {
				if p >= src.len() {
					return out;
				}
				out.push(src[p]);
				p += 1;
			} else {
				if p + 1 >= src.len() {
					return out;
				}
				let dist = (((src[p + 1] >> 4) as usize) << 8 | src[p] as usize) + 1;
				let len = (src[p + 1] & 0x0F) as usize + LZ_MIN_MATCH;
				p += 2;
				if dist > out.len() {
					return out;
				}
				let start = out.len() - dist;
				for k in 0..len {
					let byte = out[start + k];
					out.push(byte);
				}
			}
			bit += 1;
		}
	}
	out
}
