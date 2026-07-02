use crate::*;

impl<D: BlockDevice> LiberFs<D> {
	// Run `body` as one transaction: begin, run, commit on success, roll back on
	// failure. The single gate every public mutation goes through - a read-only mount
	// (a snapshot, or a volume degraded by a corrupt snapshot table) is refused here,
	// so no mutation path can touch the disk.
	pub(crate) fn mutate(&mut self, body: impl FnOnce(&mut Self) -> Result<(), FsError>) -> Result<(), FsError> {
		if self.read_only {
			return Err(FsError::ReadOnly);
		}
		self.begin();
		let r = body(self);
		self.finish(r)
	}

	// Begin a mutation: snapshot the inode-tree root, next-inode counter and snapshot
	// table so they can be restored on failure and the inode root reserved as the
	// previous generation on commit, and clear the transaction-scoped state (the fresh
	// and dead block sets and the caches).
	pub(crate) fn begin(&mut self) {
		self.txn = Some(Txn { inode_root: self.inode_root, inode_root_crc: self.inode_root_crc, next_inode: self.next_inode, snap_root: self.snap_root, snap_root_crc: self.snap_root_crc, snapshots: self.snapshots.clone() });
		self.fresh.clear();
		self.dead.clear();
		self.snapshots_dirty = false;
		self.decomp = None;
		self.wcsum = None;
		self.rcsum = None;
	}

	// Commit the in-flight mutation: write a new superblock (incremented generation,
	// carrying the new inode-tree root, next-inode counter and snapshot table) to the
	// inactive slot - the single atomic write that publishes the whole transaction. The
	// superblock write is bracketed by device flushes: the first makes every block the
	// transaction wrote durable before the superblock can name them, the second makes
	// the commit itself durable - so a device with a volatile write cache cannot
	// reorder the commit point ahead of its data. The superseded generation becomes the
	// read-only snapshot; the one before that is reclaimed INCREMENTALLY: the blocks
	// the previous transaction recorded dropped (`dead_prev`) lose their free-map bits
	// (unless a named snapshot pins them) and this transaction's `dead` set takes their
	// place - no walk of the volume. Only a commit that changed the snapshot set runs
	// the full derivation, because the pinned map must be rebuilt.
	pub(crate) fn commit(&mut self) -> Result<(), FsError> {
		// an unconsumed run reservation and the pending checksum block must settle
		// before the barrier: the first returns claimed-but-unused blocks, the second
		// is a transaction block write like any other.
		self.release_run();
		self.flush_wcsum()?;
		let sb = Superblock { num_blocks: self.num_blocks, generation: self.generation + 1, inode_root: self.inode_root, inode_root_crc: self.inode_root_crc, next_inode: self.next_inode, root_inode: self.root_inode, snap_root: self.snap_root, snap_root_crc: self.snap_root_crc, uuid: self.uuid, label: self.label, compress: self.compress };
		let new_slot = (self.slot + 1) % SUPER_SLOTS;
		// barrier: the transaction's blocks must be on the medium before the superblock
		// that references them.
		if !self.dev.flush() {
			return Err(FsError::Io);
		}
		// the commit point: a single superblock write swaps the live root atomically.
		if !self.dev.write_block(new_slot as u64, &serialize_superblock(&sb)) {
			return Err(FsError::Io);
		}
		// barrier: the commit is not durable until the superblock itself is.
		if !self.dev.flush() {
			return Err(FsError::Io);
		}

		// the generation this commit superseded becomes the snapshot; its blocks stay
		// reserved by the free map.
		if let Some(t) = self.txn.take() {
			self.prev_inode_root = t.inode_root;
			self.prev_inode_root_crc = t.inode_root_crc;
			self.prev_valid = true;
		}
		self.generation += 1;
		self.slot = new_slot;
		// "fresh" is a transaction concept: after the commit the blocks are simply part
		// of the live generation (and the caches may serve them like any other).
		self.fresh.clear();
		// the commit reclaims old-generation blocks (they may be reused and rewritten),
		// so caches keyed by physical blocks must not outlive it.
		self.decomp = None;
		self.rcsum = None;
		if self.snapshots_dirty {
			// the pinned set changed: rebuild it (and the free map) by the full walk.
			self.snapshots_dirty = false;
			self.dead.clear();
			return self.derive_free();
		}
		// the incremental reclaim: what the superseded transaction dropped is now
		// referenced by no generation - free it unless a named snapshot pins it (a
		// pinned block stays reserved until its snapshot is deleted, which rederives).
		let dead_prev = core::mem::take(&mut self.dead_prev);
		for b in dead_prev {
			if !test_bit(&self.pinned, b) {
				clear_bit(&mut self.free, b);
			}
		}
		self.dead_prev = core::mem::take(&mut self.dead);
		Ok(())
	}

	// Roll back a failed mutation: restore the inode-tree root, next-inode counter and
	// snapshot table, release every block the transaction claimed, and forget its drops
	// - so the half-written fresh blocks return to the pool and on-disk state is
	// untouched. No walk: the fresh set IS the exact list of claimed blocks.
	pub(crate) fn abort(&mut self) {
		if let Some(t) = self.txn.take() {
			self.inode_root = t.inode_root;
			self.inode_root_crc = t.inode_root_crc;
			self.next_inode = t.next_inode;
			self.snap_root = t.snap_root;
			self.snap_root_crc = t.snap_root_crc;
			self.snapshots = t.snapshots;
		}
		self.release_run();
		let fresh = core::mem::take(&mut self.fresh);
		for b in fresh {
			clear_bit(&mut self.free, b);
		}
		// the rolled-back transaction dropped nothing after all; dead_prev (the LAST
		// committed transaction's drops) stays for the next commit.
		self.dead.clear();
		self.snapshots_dirty = false;
		self.decomp = None;
		self.wcsum = None;
		self.rcsum = None;
		// the transaction may have replaced cached inodes/entries with rolled-back
		// versions: drop both caches wholesale.
		self.icache.clear();
		self.dcache.clear();
	}

	// Finish a mutation: commit on success, roll back on failure - including a failed
	// commit (a flush or superblock write that did not land), so the in-memory roots
	// never drift from the on-disk generation the mount still stands on.
	pub(crate) fn finish(&mut self, r: Result<(), FsError>) -> Result<(), FsError> {
		match r {
			Ok(()) => {
				let committed = self.commit();
				if committed.is_err() {
					self.abort();
				}
				committed
			}
			Err(e) => {
				self.abort();
				Err(e)
			}
		}
	}

	// Rebuild the in-memory allocation state from scratch: the free map (blocks 0 and 1
	// plus every block the live and previous generations reference, the snapshot table
	// block, and every pinned snapshot generation), the pinned map (the snapshot
	// generations alone), and `dead_prev` (the blocks only the previous generation
	// holds - exactly what the next commit may free). Called at mount, from fsck, and
	// after a commit that changed the snapshot set; every other commit maintains the
	// state incrementally.
	pub(crate) fn derive_free(&mut self) -> Result<(), FsError> {
		let len = self.free.len();
		let mut live = vec![0u8; len];
		set_bit(&mut live, 0);
		set_bit(&mut live, 1);
		self.mark_inode_tree(self.inode_root, &mut live)?;
		// every block of the snapshot chain and every pinned snapshot generation stay
		// reserved, so a later commit never reuses an earlier root's blocks. The raw walk
		// stops at a pointer outside the pool (a corrupt chain block - the mount is
		// already degrading to read-only in that case).
		{
			let mut ptr = self.snap_root;
			let mut buf = vec![0u8; BLOCK_SIZE];
			while ptr != 0 && ptr < self.num_blocks && !test_bit(&live, ptr) {
				set_bit(&mut live, ptr);
				if !self.dev.read_block(ptr, &mut buf) {
					return Err(FsError::Io);
				}
				ptr = u64::from_le_bytes(buf[0..8].try_into().unwrap());
			}
		}
		let mut pinned = vec![0u8; len];
		for i in 0..self.snapshots.len() {
			let root = self.snapshots[i].inode_root;
			self.mark_inode_tree(root, &mut pinned)?;
		}
		let mut prev = vec![0u8; len];
		if self.prev_valid {
			self.mark_inode_tree(self.prev_inode_root, &mut prev)?;
		}
		// the free map is the union; dead_prev is what only the previous generation
		// (and no snapshot) holds - the blocks the next commit is allowed to free.
		self.dead_prev.clear();
		for i in 0..len {
			self.free[i] = live[i] | pinned[i] | prev[i];
			let only_prev = prev[i] & !live[i] & !pinned[i];
			if only_prev != 0 {
				for bit in 0..8 {
					if only_prev & (1 << bit) != 0 {
						self.dead_prev.insert(i as u64 * 8 + bit);
					}
				}
			}
		}
		self.pinned = pinned;
		self.dead.clear();
		Ok(())
	}

	// Mark, in `map`, every block the inode B+tree rooted at `ptr` references: the tree
	// nodes themselves, and for each live inode either its file data / checksum /
	// overflow blocks or its directory's B+tree. Reads are raw (no checksum check), like
	// the old generation walk, so a corrupt block does not abort the mount or rebuild.
	// Iterative (an explicit work list), so the depth of the trees never grows the call
	// stack.
	pub(crate) fn mark_inode_tree(&mut self, root: u64, map: &mut [u8]) -> Result<(), FsError> {
		let mut nodes: Vec<u64> = Vec::new();
		if root != 0 {
			nodes.push(root);
		}
		let mut buf = vec![0u8; BLOCK_SIZE];
		while let Some(ptr) = nodes.pop() {
			// a pointer outside the pool is a corrupt link (skipped, not followed into
			// whatever lies past the volume); an already-marked node is either a corrupt
			// cycle (which must not hang the walk) or a subtree shared with an earlier
			// root walked into the same map - marked means walked, so skip both.
			if ptr >= self.num_blocks || test_bit(map, ptr) {
				continue;
			}
			set_bit(map, ptr);
			if !self.dev.read_block(ptr, &mut buf) {
				return Err(FsError::Io);
			}
			if node_type(&buf) == NODE_LEAF {
				for i in 0..leaf_count(&buf, INODE_REC) {
					let off = NODE_HDR + i * INODE_REC + 8;
					let mut inode = Inode::parse(&buf[off..off + INODE_SIZE]);
					if inode.kind == KIND_FILE {
						// complete the extent map from the overflow chain before marking
						// (the spill and dir walks use their own buffers, so the leaf
						// image in `buf` stays intact).
						self.load_spill(&mut inode)?;
						self.collect_inode_blocks(&inode, map)?;
					} else if inode.kind == KIND_DIR {
						self.mark_dir_tree(inode.dir_root, map)?;
					}
				}
			} else {
				for i in 0..=internal_count(&buf) {
					nodes.push(child_ptr(&buf, i));
				}
			}
		}
		Ok(())
	}

	// Mark every node block of a directory's B+tree. The entries themselves point at
	// inodes, which the inode-tree walk already covers, so only the nodes are marked.
	// Iterative like `mark_inode_tree`.
	pub(crate) fn mark_dir_tree(&mut self, root: u64, map: &mut [u8]) -> Result<(), FsError> {
		let mut nodes: Vec<u64> = Vec::new();
		if root != 0 {
			nodes.push(root);
		}
		let mut buf = vec![0u8; BLOCK_SIZE];
		while let Some(ptr) = nodes.pop() {
			// same guards as `mark_inode_tree`: skip out-of-pool links and marked nodes.
			if ptr >= self.num_blocks || test_bit(map, ptr) {
				continue;
			}
			set_bit(map, ptr);
			if !self.dev.read_block(ptr, &mut buf) {
				return Err(FsError::Io);
			}
			if node_type(&buf) == NODE_INTERNAL {
				for i in 0..=internal_count(&buf) {
					nodes.push(child_ptr(&buf, i));
				}
			}
		}
		Ok(())
	}

	// B+tree node and generic tree operations

	// Read a B+tree node block, verifying it against the CRC32C its parent link stored.
	// A mismatch is FsError::Corrupt, so on-disk damage to a tree node is caught on the
	// live path (lookup / insert / delete / enumeration / fsck).
	pub(crate) fn read_node(&mut self, ptr: u64, crc: u32, buf: &mut [u8]) -> Result<(), FsError> {
		if !self.dev.read_block(ptr, buf) {
			return Err(FsError::Io);
		}
		if crc32c(buf) != crc {
			return Err(FsError::Corrupt);
		}
		Ok(())
	}

	// The block to write an updated node to: reuse one this transaction already
	// allocated (overwrite in place), else allocate a fresh metadata block and record
	// the committed original dropped - the new generation references the rewrite, so
	// the original leaves with the superseded generation.
	pub(crate) fn node_dest(&mut self, ptr: u64) -> Result<u64, FsError> {
		if ptr != 0 && self.fresh.contains(&ptr) {
			return Ok(ptr);
		}
		let fresh = self.alloc_meta()?;
		self.drop_block(ptr);
		Ok(fresh)
	}

	// Write `buf` to block `ptr` and return its CRC32C (to store in the parent link).
	pub(crate) fn write_node_to(&mut self, ptr: u64, buf: &[u8]) -> Result<u32, FsError> {
		if !self.dev.write_block(ptr, buf) {
			return Err(FsError::Io);
		}
		Ok(crc32c(buf))
	}

	// Look up `key` in the B+tree rooted at (`root`, `root_crc`), returning the matching
	// leaf record (whose leading `probe.len()` bytes equal `probe`) or None. `rec` is the
	// record width. Internal nodes route by the numeric u64 `key`; a leaf is searched by
	// the full probe so records sharing a u64 key are disambiguated by the bytes after it.
	pub(crate) fn tree_lookup(&mut self, root: u64, root_crc: u32, key: u64, probe: &[u8], rec: usize) -> Result<Option<Vec<u8>>, FsError> {
		if root == 0 {
			return Ok(None);
		}
		let mut ptr = root;
		let mut crc = root_crc;
		let mut buf = vec![0u8; BLOCK_SIZE];
		loop {
			self.read_node(ptr, crc, &mut buf)?;
			if node_type(&buf) == NODE_LEAF {
				let (mut lo, mut hi) = (0usize, leaf_count(&buf, rec));
				while lo < hi {
					let mid = (lo + hi) / 2;
					let off = NODE_HDR + mid * rec;
					match key_cmp(&buf[off..off + probe.len()], probe) {
						Ordering::Less => lo = mid + 1,
						Ordering::Greater => hi = mid,
						Ordering::Equal => return Ok(Some(buf[off..off + rec].to_vec())),
					}
				}
				return Ok(None);
			}
			// internal: route to the child whose range holds `key`.
			let ci = route_child(&buf, internal_count(&buf), key);
			ptr = child_ptr(&buf, ci);
			crc = child_crc(&buf, ci);
		}
	}

	// Insert or overwrite `record` (numeric key `key`, full key width `keylen`) in the
	// B+tree rooted at (`root`, `root_crc`); `rec` is the record width and `leaf_max` the
	// leaf capacity. Returns the new root (ptr, crc). Copy-on-write: every node on the
	// path is rewritten to a fresh block (or in place if already fresh this transaction).
	pub(crate) fn tree_insert(&mut self, root: u64, root_crc: u32, key: u64, record: &[u8], rec: usize, leaf_max: usize, keylen: usize) -> Result<(u64, u32), FsError> {
		if root == 0 {
			// empty tree: a new leaf with the single record.
			let blk = self.alloc_meta()?;
			let mut buf = vec![0u8; BLOCK_SIZE];
			node_set_header(&mut buf, NODE_LEAF, 1);
			buf[NODE_HDR..NODE_HDR + rec].copy_from_slice(record);
			let crc = self.write_node_to(blk, &buf)?;
			return Ok((blk, crc));
		}
		let outcome = self.tree_insert_node(root, root_crc, key, record, rec, leaf_max, keylen)?;
		self.settle_root(outcome)
	}

	// Turn an insert outcome into the tree's new root: an updated node is the root as
	// is; a split builds a new internal root over the two halves.
	pub(crate) fn settle_root(&mut self, outcome: Ins) -> Result<(u64, u32), FsError> {
		match outcome {
			Ins::Updated(p, c) => Ok((p, c)),
			Ins::Split(lp, lc, sep, rp, rc) => {
				let blk = self.alloc_meta()?;
				let mut buf = vec![0u8; BLOCK_SIZE];
				node_set_header(&mut buf, NODE_INTERNAL, 1);
				set_sep(&mut buf, 0, sep);
				set_child(&mut buf, 0, lp, lc);
				set_child(&mut buf, 1, rp, rc);
				let crc = self.write_node_to(blk, &buf)?;
				Ok((blk, crc))
			}
		}
	}

	// Absorb a child's insert outcome into internal node `buf` (at `ptr`, child index
	// `ci`): rewire an updated child, or take in a split - inserting the lifted
	// separator and the right half when there is room, else splitting this internal
	// node too and lifting the middle separator further. Shared by every tree flavour
	// (the inode tree's fixed leaves, the directories' variable-record leaves).
	pub(crate) fn internal_absorb(&mut self, buf: &mut [u8], ptr: u64, ci: usize, outcome: Ins) -> Result<Ins, FsError> {
		let count = internal_count(buf);
		match outcome {
			Ins::Updated(np, nc) => {
				let dest = self.node_dest(ptr)?;
				set_child(buf, ci, np, nc);
				let ncrc = self.write_node_to(dest, buf)?;
				Ok(Ins::Updated(dest, ncrc))
			}
			Ins::Split(lp, lc, sep, rp, rc) => {
				if count + 2 <= INTERNAL_MAX {
					// room: replace child ci with the left half and insert the separator
					// and the right half after it.
					let dest = self.node_dest(ptr)?;
					let sstart = NODE_HDR + ci * SEP_SIZE;
					let send = NODE_HDR + count * SEP_SIZE;
					buf.copy_within(sstart..send, sstart + SEP_SIZE);
					set_sep(buf, ci, sep);
					let cstart = INTERNAL_CHILD_BASE + (ci + 1) * CHILD_SIZE;
					let cend = INTERNAL_CHILD_BASE + (count + 1) * CHILD_SIZE;
					buf.copy_within(cstart..cend, cstart + CHILD_SIZE);
					set_child(buf, ci, lp, lc);
					set_child(buf, ci + 1, rp, rc);
					node_set_header(buf, NODE_INTERNAL, count + 1);
					let ncrc = self.write_node_to(dest, buf)?;
					Ok(Ins::Updated(dest, ncrc))
				} else {
					// full: build the combined separator and child arrays, split them,
					// and lift the middle separator to the parent.
					let mut seps: Vec<u64> = (0..count).map(|i| sep_key(buf, i)).collect();
					let mut kids: Vec<(u64, u32)> = (0..=count).map(|i| (child_ptr(buf, i), child_crc(buf, i))).collect();
					seps.insert(ci, sep);
					kids[ci] = (lp, lc);
					kids.insert(ci + 1, (rp, rc));
					let s = seps.len();
					let mid = s / 2;
					let up = seps[mid];
					let left_dest = self.node_dest(ptr)?;
					let right_dest = self.alloc_meta()?;
					let mut lbuf = vec![0u8; BLOCK_SIZE];
					node_set_header(&mut lbuf, NODE_INTERNAL, mid);
					for i in 0..mid {
						set_sep(&mut lbuf, i, seps[i]);
					}
					for i in 0..=mid {
						set_child(&mut lbuf, i, kids[i].0, kids[i].1);
					}
					let rcount = s - mid - 1;
					let mut rbuf = vec![0u8; BLOCK_SIZE];
					node_set_header(&mut rbuf, NODE_INTERNAL, rcount);
					for i in 0..rcount {
						set_sep(&mut rbuf, i, seps[mid + 1 + i]);
					}
					for i in 0..=rcount {
						set_child(&mut rbuf, i, kids[mid + 1 + i].0, kids[mid + 1 + i].1);
					}
					let lcrc = self.write_node_to(left_dest, &lbuf)?;
					let rcrc = self.write_node_to(right_dest, &rbuf)?;
					Ok(Ins::Split(left_dest, lcrc, up, right_dest, rcrc))
				}
			}
		}
	}

	pub(crate) fn tree_insert_node(&mut self, ptr: u64, crc: u32, key: u64, record: &[u8], rec: usize, leaf_max: usize, keylen: usize) -> Result<Ins, FsError> {
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node(ptr, crc, &mut buf)?;
		if node_type(&buf) == NODE_LEAF {
			let count = leaf_count(&buf, rec);
			// find the insert position, or an exact match by the full key.
			let (mut lo, mut hi) = (0usize, count);
			let mut exact = false;
			while lo < hi {
				let mid = (lo + hi) / 2;
				let off = NODE_HDR + mid * rec;
				match key_cmp(&buf[off..off + keylen], &record[..keylen]) {
					Ordering::Less => lo = mid + 1,
					Ordering::Greater => hi = mid,
					Ordering::Equal => {
						exact = true;
						lo = mid;
						break;
					}
				}
			}
			let pos = lo;
			if exact {
				// overwrite in place (after copying the node up).
				let dest = self.node_dest(ptr)?;
				let off = NODE_HDR + pos * rec;
				buf[off..off + rec].copy_from_slice(record);
				let ncrc = self.write_node_to(dest, &buf)?;
				return Ok(Ins::Updated(dest, ncrc));
			}
			if count < leaf_max {
				// insert, shifting the tail right by one record.
				let dest = self.node_dest(ptr)?;
				let start = NODE_HDR + pos * rec;
				let end = NODE_HDR + count * rec;
				buf.copy_within(start..end, start + rec);
				buf[start..start + rec].copy_from_slice(record);
				node_set_header(&mut buf, NODE_LEAF, count + 1);
				let ncrc = self.write_node_to(dest, &buf)?;
				return Ok(Ins::Updated(dest, ncrc));
			}
			// full: gather every record with the new one inserted, then split in two.
			let mut recs: Vec<Vec<u8>> = Vec::with_capacity(count + 1);
			for i in 0..count {
				let off = NODE_HDR + i * rec;
				recs.push(buf[off..off + rec].to_vec());
			}
			recs.insert(pos, record.to_vec());
			let split = leaf_split_point(&recs);
			let left_dest = self.node_dest(ptr)?;
			let right_dest = self.alloc_meta()?;
			let mut lbuf = vec![0u8; BLOCK_SIZE];
			node_set_header(&mut lbuf, NODE_LEAF, split);
			for (i, r) in recs[..split].iter().enumerate() {
				let off = NODE_HDR + i * rec;
				lbuf[off..off + rec].copy_from_slice(r);
			}
			let mut rbuf = vec![0u8; BLOCK_SIZE];
			node_set_header(&mut rbuf, NODE_LEAF, recs.len() - split);
			for (i, r) in recs[split..].iter().enumerate() {
				let off = NODE_HDR + i * rec;
				rbuf[off..off + rec].copy_from_slice(r);
			}
			let lcrc = self.write_node_to(left_dest, &lbuf)?;
			let rcrc = self.write_node_to(right_dest, &rbuf)?;
			let sep = u64::from_le_bytes(recs[split][0..8].try_into().unwrap());
			return Ok(Ins::Split(left_dest, lcrc, sep, right_dest, rcrc));
		}
		// internal: route to a child and recurse; the shared absorber takes the outcome.
		let ci = route_child(&buf, internal_count(&buf), key);
		let cp = child_ptr(&buf, ci);
		let cc = child_crc(&buf, ci);
		let outcome = self.tree_insert_node(cp, cc, key, record, rec, leaf_max, keylen)?;
		self.internal_absorb(&mut buf, ptr, ci, outcome)
	}

	// Delete `key` from the B+tree rooted at (`root`, `root_crc`). Returns the new root
	// (ptr, crc) and whether a record was removed. Empty leaves and single-child roots
	// are collapsed; there is no rebalancing or merging of half-full nodes, which keeps
	// deletion O(log n) and is sound for a copy-on-write tree (a thin node only wastes a
	// little space, never breaks lookup).
	pub(crate) fn tree_delete(&mut self, root: u64, root_crc: u32, key: u64, probe: &[u8], rec: usize, keylen: usize) -> Result<(u64, u32, bool), FsError> {
		if root == 0 {
			return Ok((0, 0, false));
		}
		match self.tree_delete_node(root, root_crc, key, probe, rec, keylen)? {
			Del::NotFound => Ok((root, root_crc, false)),
			Del::Empty => Ok((0, 0, true)),
			Del::Updated(p, c) => {
				let (ptr, crc) = self.collapse_root(p, c)?;
				Ok((ptr, crc, true))
			}
		}
	}

	// Collapse a root that became a single-child internal node, repeatedly; each
	// collapsed node leaves the new generation. Shared by every tree flavour.
	pub(crate) fn collapse_root(&mut self, mut ptr: u64, mut crc: u32) -> Result<(u64, u32), FsError> {
		let mut buf = vec![0u8; BLOCK_SIZE];
		loop {
			self.read_node(ptr, crc, &mut buf)?;
			if node_type(&buf) == NODE_INTERNAL && node_count(&buf) == 0 {
				let cp = child_ptr(&buf, 0);
				let cc = child_crc(&buf, 0);
				self.drop_block(ptr);
				ptr = cp;
				crc = cc;
			} else {
				return Ok((ptr, crc));
			}
		}
	}

	// Absorb a child's delete outcome into internal node `buf` (at `ptr`, child index
	// `ci`): rewire an updated child, or drop an emptied one along with an adjacent
	// separator. Shared by every tree flavour.
	pub(crate) fn internal_absorb_del(&mut self, buf: &mut [u8], ptr: u64, ci: usize, outcome: Del) -> Result<Del, FsError> {
		let count = internal_count(buf);
		match outcome {
			Del::NotFound => Ok(Del::NotFound),
			Del::Updated(np, nc) => {
				let dest = self.node_dest(ptr)?;
				set_child(buf, ci, np, nc);
				let ncrc = self.write_node_to(dest, buf)?;
				Ok(Del::Updated(dest, ncrc))
			}
			Del::Empty => {
				if count == 0 {
					// a single-child internal whose only child emptied empties too.
					self.drop_block(ptr);
					return Ok(Del::Empty);
				}
				// drop child ci and an adjacent separator (the one to its left when ci is
				// the last child, else the one to its right).
				let dest = self.node_dest(ptr)?;
				let sidx = if ci == count { ci - 1 } else { ci };
				let sstart = NODE_HDR + sidx * SEP_SIZE;
				let send = NODE_HDR + count * SEP_SIZE;
				buf.copy_within(sstart + SEP_SIZE..send, sstart);
				let cstart = INTERNAL_CHILD_BASE + ci * CHILD_SIZE;
				let cend = INTERNAL_CHILD_BASE + (count + 1) * CHILD_SIZE;
				buf.copy_within(cstart + CHILD_SIZE..cend, cstart);
				node_set_header(buf, NODE_INTERNAL, count - 1);
				let ncrc = self.write_node_to(dest, buf)?;
				Ok(Del::Updated(dest, ncrc))
			}
		}
	}

	pub(crate) fn tree_delete_node(&mut self, ptr: u64, crc: u32, key: u64, probe: &[u8], rec: usize, keylen: usize) -> Result<Del, FsError> {
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node(ptr, crc, &mut buf)?;
		if node_type(&buf) == NODE_LEAF {
			let count = leaf_count(&buf, rec);
			let (mut lo, mut hi) = (0usize, count);
			let mut found = None;
			while lo < hi {
				let mid = (lo + hi) / 2;
				let off = NODE_HDR + mid * rec;
				match key_cmp(&buf[off..off + keylen], probe) {
					Ordering::Less => lo = mid + 1,
					Ordering::Greater => hi = mid,
					Ordering::Equal => {
						found = Some(mid);
						break;
					}
				}
			}
			let pos = match found {
				Some(p) => p,
				None => return Ok(Del::NotFound),
			};
			if count == 1 {
				// the leaf empties: the parent drops it, so it leaves the new generation.
				self.drop_block(ptr);
				return Ok(Del::Empty);
			}
			let dest = self.node_dest(ptr)?;
			let start = NODE_HDR + pos * rec;
			let end = NODE_HDR + count * rec;
			buf.copy_within(start + rec..end, start);
			node_set_header(&mut buf, NODE_LEAF, count - 1);
			let ncrc = self.write_node_to(dest, &buf)?;
			return Ok(Del::Updated(dest, ncrc));
		}
		// internal: route and recurse; the shared absorber takes the outcome.
		let ci = route_child(&buf, internal_count(&buf), key);
		let cp = child_ptr(&buf, ci);
		let cc = child_crc(&buf, ci);
		let outcome = self.tree_delete_node(cp, cc, key, probe, rec, keylen)?;
		self.internal_absorb_del(&mut buf, ptr, ci, outcome)
	}
}

// B+tree node accessors. A node block begins with an 8-byte header: a type byte
// (NODE_LEAF or NODE_INTERNAL) then a u16 entry count at bytes 2..4; the entries follow.
pub(crate) fn node_type(buf: &[u8]) -> u8 {
	buf[0]
}

pub(crate) fn node_count(buf: &[u8]) -> usize {
	u16::from_le_bytes(buf[2..4].try_into().unwrap()) as usize
}

// Entry counts come off the medium, and a CRC32C proves integrity, not sanity: a
// checksummed-but-hostile block (or plain corruption on the raw generation walks) can
// claim a count no node can hold, running the entry loops past the 4096-byte block.
// Every consumer clamps to what the node type physically fits: a leaf by its record
// width, an internal node by its separator region.
pub(crate) fn leaf_count(buf: &[u8], rec: usize) -> usize {
	node_count(buf).min((BLOCK_SIZE - NODE_HDR) / rec)
}

pub(crate) fn internal_count(buf: &[u8]) -> usize {
	node_count(buf).min(INTERNAL_MAX - 1)
}

pub(crate) fn node_set_header(buf: &mut [u8], typ: u8, count: usize) {
	buf[..NODE_HDR].fill(0);
	buf[0] = typ;
	buf[2..4].copy_from_slice(&(count as u16).to_le_bytes());
}

// Internal-node separator key `i`: child `i` holds keys below it, child `i + 1` keys at
// or above it. Separators sit in a fixed region right after the header.
pub(crate) fn sep_key(buf: &[u8], i: usize) -> u64 {
	let off = NODE_HDR + i * SEP_SIZE;
	u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

pub(crate) fn set_sep(buf: &mut [u8], i: usize, key: u64) {
	let off = NODE_HDR + i * SEP_SIZE;
	buf[off..off + 8].copy_from_slice(&key.to_le_bytes());
}

// Internal-node child link `i`: its block pointer and that block's CRC32C. Child links
// sit in a fixed region after the separators, so offsets do not shift with the count.
pub(crate) fn child_ptr(buf: &[u8], i: usize) -> u64 {
	let off = INTERNAL_CHILD_BASE + i * CHILD_SIZE;
	u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

pub(crate) fn child_crc(buf: &[u8], i: usize) -> u32 {
	let off = INTERNAL_CHILD_BASE + i * CHILD_SIZE + 8;
	u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

pub(crate) fn set_child(buf: &mut [u8], i: usize, ptr: u64, crc: u32) {
	let off = INTERNAL_CHILD_BASE + i * CHILD_SIZE;
	buf[off..off + 8].copy_from_slice(&ptr.to_le_bytes());
	buf[off + 8..off + 12].copy_from_slice(&crc.to_le_bytes());
}

// Internal-node routing: the index of the child whose key range holds `key` (child i
// holds keys below separator i, child i + 1 keys at or above it). One helper, so the
// two trees' lookup/insert/delete share the rule instead of copying the loop.
pub(crate) fn route_child(buf: &[u8], count: usize, key: u64) -> usize {
	let mut ci = 0;
	while ci < count && sep_key(buf, ci) <= key {
		ci += 1;
	}
	ci
}

// Compare two leaf keys: the leading u64 numerically (so leaf order matches the numeric
// routing in internal nodes), then any remaining bytes lexicographically (the name, for
// a directory record, disambiguating a shared hash). Both slices are one key wide.
pub(crate) fn key_cmp(a: &[u8], b: &[u8]) -> Ordering {
	let ka = u64::from_le_bytes(a[0..8].try_into().unwrap());
	let kb = u64::from_le_bytes(b[0..8].try_into().unwrap());
	match ka.cmp(&kb) {
		Ordering::Equal => a[8..].cmp(&b[8..]),
		other => other,
	}
}

// Where to split an overfull leaf's records in two: the midpoint, nudged so two records
// sharing a u64 key never straddle the split (the parent routes by that key alone, so
// equal keys must stay in one leaf). Records are unique in the inode tree, so this is the
// plain midpoint there; in a directory it matters only for an astronomically rare 64-bit
// hash collision.
pub(crate) fn leaf_split_point(recs: &[Vec<u8>]) -> usize {
	let n = recs.len();
	let key_at = |i: usize| -> u64 { u64::from_le_bytes(recs[i][0..8].try_into().unwrap()) };
	let mut up = n / 2;
	while up < n && key_at(up) == key_at(up - 1) {
		up += 1;
	}
	if up < n {
		return up;
	}
	// no key boundary above the midpoint: look below it (only reached when most of the
	// leaf shares one 64-bit key).
	let mut down = n / 2;
	while down > 1 && key_at(down) == key_at(down - 1) {
		down -= 1;
	}
	down
}
