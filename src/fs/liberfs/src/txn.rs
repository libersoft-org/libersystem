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
		self.txn = Some(Txn { inode_root: self.inode_root, inode_root_crc: self.inode_root_crc, next_inode: self.next_inode, snap_root: self.snap_root, snap_root_crc: self.snap_root_crc, snapshots: self.snapshots.clone(), compress: self.compress });
		self.fresh.clear();
		self.dead.clear();
		self.snapshots_dirty = false;
		self.decomp.clear();
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
		// A TRANSACTION THAT CHANGED NOTHING DOES NOT WRITE.
		//
		// `mutate` commits whenever the body returns `Ok`, so `rename("foo", "foo")` - which
		// `rename_inner` short-circuits - wrote a superblock, advanced the generation, and rolled
		// the previous one into a snapshot, for no change. Nothing was incorrect about it; it was a
		// write, wear, and a generation step that a caller can repeat indefinitely.
		//
		// Unchanged means every field the superblock carries and every set the commit acts on: the
		// two roots and their checksums, the inode counter, the compression flag, the snapshot
		// table, and the fresh and dead block sets. If all of those are as `begin` found them,
		// ending the transaction publishes exactly what is already published.
		if let Some(txn) = &self.txn {
			let same = self.inode_root == txn.inode_root && self.inode_root_crc == txn.inode_root_crc && self.next_inode == txn.next_inode && self.snap_root == txn.snap_root && self.snap_root_crc == txn.snap_root_crc && self.compress == txn.compress && !self.snapshots_dirty && self.fresh.is_empty() && self.dead.is_empty();
			if same {
				self.txn = None;
				return Ok(());
			}
		}
		// the generation numbers every commit and nothing refused the last one, so the
		// increment below could wrap and produce a superblock that looks OLDER than the
		// one it supersedes - which would make the volume mount at the wrong generation
		// from then on. Refuse the commit instead; the volume stays exactly as it is.
		// OFF BY ONE, AND THE ONE MATTERS. This refused a commit AT `u64::MAX`, which stops the
		// increment wrapping - but the increment then publishes `MAX` from `MAX - 1`, and the
		// superblock parser refuses `MAX` outright. So the last permitted commit wrote a volume
		// that cannot be mounted again: the write succeeds, the medium is consistent, and the next
		// boot reads the paired slot and silently goes back a generation.
		//
		// The bound belongs where the parser's is: the generation ABOUT TO BE WRITTEN must be one
		// the parser will accept.
		if self.generation >= u64::MAX - 1 {
			self.abort();
			return Err(FsError::NoSpace);
		}
		if let Err(e) = self.flush_wcsum() {
			self.abort();
			return Err(e);
		}
		let sb = Superblock { num_blocks: self.num_blocks, generation: self.generation + 1, inode_root: self.inode_root, inode_root_crc: self.inode_root_crc, next_inode: self.next_inode, root_inode: self.root_inode, snap_root: self.snap_root, snap_root_crc: self.snap_root_crc, uuid: self.uuid, label: self.label, compress: self.compress };
		let new_slot = (self.slot + 1) % SUPER_SLOTS;
		// barrier: the transaction's blocks must be on the medium before the superblock
		// that references them. A failure here is still safely rolled back - the
		// superblock has not been touched, so the medium stands on the old generation.
		if !self.dev.flush() {
			self.abort();
			return Err(FsError::Io);
		}
		// THE NEXT GENERATION'S DEAD LIST, BUILT WHILE FAILING IS STILL FREE.
		//
		// After the commit, the set this transaction dropped becomes the next commit's `dead_prev`,
		// and copying it allocates. That copy used to happen AFTER the superblock write, with a
		// bare `?` - so a volume that ran out of memory there answered `NoMemory` for a transaction
		// that was already durable. StorageService maps `NoMemory` to `again`, which tells the
		// caller to retry, which is the one thing that must not happen against a generation that
		// has already moved.
		//
		// The correct answer for a failure past that line is ten lines below this one: read-only
		// and `CommitUncertain`, with a comment saying the caller is told not to retry the write it
		// thinks it lost. This is the same hazard reached by a different road, and the cheapest fix
		// is not to be on that road: the list is known before the barrier, so it is built before
		// the barrier, and the only thing left after the superblock lands is a swap that cannot
		// fail.
		//
		// AND THE REFUSAL ROLLS BACK, which it did not. This was a bare `?`, and a bare `?` here is
		// the one shape `finish()` cannot repair: `finish` calls `abort()` only when the transaction
		// BODY failed, so a body that succeeded goes straight to `commit()` and its error is
		// returned unchanged. What that left behind was a live mount carrying uncommitted state -
		// `self.txn` still holding the rollback snapshot, `inode_root` pointing at the new
		// uncommitted root, `fresh` and `dead` holding the failed operation's blocks - while the
		// caller was told `NoMemory`, which StorageService maps to `again`.
		//
		// The next `mutate()` then calls `begin()`, which overwrites the rollback snapshot and
		// clears both sets, and a later unrelated commit publishes the new root: an operation whose
		// caller was told it failed, committed. The block bookkeeping leans toward a leak, which is
		// the safe direction, and the transaction contract is broken either way.
		//
		// This is before the point of no return - the superblock has not been touched - so `abort()`
		// is exactly right and is what it exists for.
		let mut next_dead_prev: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
		if try_reserve_blocks(&mut next_dead_prev, self.dead.len()).is_err() {
			self.abort();
			return Err(FsError::NoMemory);
		}
		// `dead` is a `BTreeSet`, so this arrives in ascending order - which is what the reclaim
		// above and `derive_free` both produce, and what keeps the two paths interchangeable.
		next_dead_prev.extend(self.dead.iter().copied());
		// the point of no return: once the superblock write is ATTEMPTED, the new
		// generation may be durable no matter what the device reports (a reported
		// failure can still have landed; a torn write is caught by the slot's
		// self-CRC on the next mount). Rolling back here would return the fresh
		// blocks to the pool while the medium may name them - a later transaction
		// would overwrite a mountable generation's trees. So the in-memory state
		// adopts the new generation either way; what a failure costs is writability.
		let landed = self.dev.write_block(new_slot as u64, &serialize_superblock(&sb)) && self.dev.flush();

		// the generation this commit superseded becomes the snapshot; its blocks stay
		// reserved by the free map.
		if let Some(t) = self.txn.take() {
			self.prev_inode_root = t.inode_root;
			self.prev_inode_root_crc = t.inode_root_crc;
			self.prev_snap_root = t.snap_root;
			self.prev_snap_root_crc = t.snap_root_crc;
			self.prev_valid = true;
		}
		self.generation += 1;
		self.slot = new_slot;
		// "fresh" is a transaction concept: after the commit the blocks are simply part
		// of the live generation (and the caches may serve them like any other).
		self.fresh.clear();
		// the commit reclaims old-generation blocks (they may be reused and rewritten),
		// so caches keyed by physical blocks must not outlive it.
		self.decomp.clear();
		self.rcsum = None;
		if !landed {
			// the commit's durability is unknown (the write or the barrier after it
			// failed): the device is failing, so degrade to read-only. The free map is
			// never consulted for another allocation, which makes the skipped reclaim
			// below moot - and the in-memory state matches whichever superblock a
			// remount finds, since both name only blocks the first barrier made durable.
			//
			// NOT `Io`. The superblock write was attempted, so this cannot say the commit did not
			// happen - and a caller told `Io` retries, which is the one thing that must not happen
			// against a volume whose generation may already have moved.
			self.read_only = true;
			return Err(FsError::CommitUncertain);
		}
		if self.snapshots_dirty {
			// the pinned set changed: rebuild it (and the free map) by the full walk. The
			// commit itself already landed (the superblock is on disk); if the walk finds
			// damage, the allocator can no longer be trusted - degrade to read-only
			// rather than allocate from an incomplete map.
			self.snapshots_dirty = false;
			self.dead.clear();
			return match self.derive_free() {
				Ok(()) => Ok(()),
				Err(_) => {
					// The superblock is PROVABLY on the disk here - this is the path after a
					// successful publish - and the walk that rebuilds the free map failed. The
					// commit happened; what is unknown is whether this mount can go on safely, and
					// it cannot, so it is read-only and the caller is told not to retry the write
					// it thinks it lost.
					self.read_only = true;
					Err(FsError::CommitUncertain)
				}
			};
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
		// A SWAP, AND SWAPS DO NOT FAIL. The list was built above, before the barrier, precisely so
		// nothing past the point of no return can answer with an error the caller would retry.
		self.dead_prev = next_dead_prev;
		self.dead.clear();
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
			self.compress = t.compress;
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
		self.decomp.clear();
		self.wcsum = None;
		self.rcsum = None;
		// the transaction may have replaced cached inodes/entries with rolled-back
		// versions: drop both caches wholesale.
		self.icache.clear();
		self.dcache.clear();
	}

	// Finish a mutation: commit on success, roll back on failure. A failed commit
	// cleans up after itself (a rollback before the superblock write, read-only
	// adoption after it), so it is not rolled back again here.
	pub(crate) fn finish(&mut self, r: Result<(), FsError>) -> Result<(), FsError> {
		match r {
			Ok(()) => self.commit(),
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
	// Set `b` in `map`, reporting whether it was already set. Under `mark_strict` a
	// repeat is recorded as two owners of one block - the thing a bitmap cannot
	// otherwise express. The FIRST repeat is kept; later ones change nothing.
	pub(crate) fn mark(&mut self, map: &mut [u8], b: u64) -> bool {
		if test_bit(map, b) {
			if self.mark_strict && self.mark_dup.is_none() {
				self.mark_dup = Some(b);
			}
			return false;
		}
		set_bit(map, b);
		true
	}

	// The categorised walk: `Ok(kinds)` for a walk that COMPLETED, whether or not it found damage,
	// and `Err` only for a failure of this machine rather than of the volume (a refused allocation).
	//
	// Split out from `derive_free` because the single `Err(FsError::Corrupt)` that used to be the
	// only answer was thrown away twice over: once here, where four different problems became one
	// error, and again in `fsck`, which counted whatever came back as a checksum failure. The
	// categories were already known at every site that raised them; nothing carried them out.
	pub(crate) fn derive_free_kinds(&mut self) -> Result<WalkDamage, FsError> {
		self.walk_damage = WalkDamage::default();
		self.mark_dup = None;
		self.mark_max_inode = 0;
		let len = self.free.len();
		let mut live = try_zeroed(len)?;
		set_bit(&mut live, 0);
		set_bit(&mut live, 1);
		// the live generation is the one place where every block has exactly one owner.
		//
		// And the one place where every inode has exactly one name. The bitmap is sized by the
		// inode numbers this volume has issued, not by the pool, and it is allocated fallibly like
		// every other map here.
		// SIZED BY WHAT IS REACHABLE, not by what was ever issued.
		//
		// This was `next_inode / 8` under a comment calling that "the inode numbers this volume has
		// issued, not the pool" - presented as the smaller of the two, and on a long-lived volume it
		// is the larger. Inode numbers are never recycled, so ten live files on a volume that has
		// created four billion temporaries wanted half a gigabyte of bitmap to check ten names. It
		// was fallible, so that is a mount that fails rather than one that corrupts; it is still the
		// wrong size.
		//
		// A `BTreeSet` is bounded by the tree instead, and every operation it needs - "have I seen
		// this inode" - is what a set is for.
		let mut names: Vec<u32> = Vec::new();
		// THE ROOT IS ALREADY SPOKEN FOR, and the map started saying it was not.
		//
		// A bit is set on an inode's first sighting and an alias is declared on its second, which is
		// right for every inode that has one name. The root has NONE - nothing may name it - so its
		// first sighting is already the alias. Over a zeroed map a directory record pointing at
		// inode 0 merely set the bit and the walk carried on, and a volume with a namespace loop
		// through the root mounted writable.
		//
		// `fsck` has always had this right, and for a reason worth copying rather than re-deriving:
		// its `reached` set contains the root BEFORE the walk starts. Seeding the same way here is
		// what makes the two checkers agree by construction instead of by coincidence, which is the
		// gap this whole milestone is named after.
		crate::try_push(&mut names, ROOT_INODE)?;
		self.mark_names = Some(names);
		self.mark_alias = false;
		self.mark_strict = true;
		self.mark_inode_tree(self.inode_root, self.inode_root_crc, &mut live)?;
		// every block of the snapshot chain and every pinned snapshot generation stay
		// reserved, so a later commit never reuses an earlier root's blocks. The raw walk
		// stops at a pointer outside the pool or a link that cannot be read (flagged as
		// damage below) - and at a marked block, so a corrupt cycle terminates.
		{
			let mut ptr = self.snap_root;
			let mut buf = try_zeroed(BLOCK_SIZE)?;
			while ptr != 0 && ptr < self.num_blocks && !test_bit(&live, ptr) {
				self.mark(&mut live, ptr);
				if !self.dev.read_block(ptr, &mut buf) {
					self.walk_damage.io = true;
					break;
				}
				ptr = u64::from_le_bytes(buf[0..8].try_into().unwrap());
			}
		}
		// everything past here spans generations, where sharing is legal by design.
		let dup = self.mark_dup.take();
		// THE ALIAS IS A PROPERTY OF THE FINISHED SET, decided in one pass over it rather than by a
		// flag raised during the walk - which is also what makes the sort valid: there is nothing to
		// preserve about the order the inodes were reached in.
		//
		// Seeding `ROOT_INODE` first still catches a record pointing at the root, because a
		// duplicate root shows up as an adjacent pair like any other.
		if let Some(names) = self.mark_names.as_mut() {
			names.sort_unstable();
			self.mark_alias = names.windows(2).any(|pair| pair[0] == pair[1]);
		}
		self.mark_names = None;
		if self.mark_alias {
			self.read_only = true;
		}
		self.mark_strict = false;
		let mut pinned = try_zeroed(len)?;
		for i in 0..self.snapshots.len() {
			let (root, crc) = (self.snapshots[i].inode_root, self.snapshots[i].inode_root_crc);
			self.mark_inode_tree(root, crc, &mut pinned)?;
		}
		let mut prev = try_zeroed(len)?;
		if self.prev_valid {
			self.mark_inode_tree(self.prev_inode_root, self.prev_inode_root_crc, &mut prev)?;
			// and its snapshot table: both the chain's own blocks and the generations it
			// names. A snapshot deleted in the live generation is still live in the one
			// the older superblock describes, and that superblock is mountable until the
			// next commit overwrites its slot - which is exactly when `dead_prev` frees
			// these blocks. Reading it is best-effort: a table that cannot be read leaves
			// the reservation incomplete, so it is damage, and read-only is the answer.
			if self.prev_snap_root != 0 {
				let (root, crc) = (self.prev_snap_root, self.prev_snap_root_crc);
				let mut chain: Vec<u64> = Vec::new();
				match self.read_snapshot_table(root, crc, &mut |b| chain.push(b)) {
					Ok(snaps) => {
						for b in chain {
							self.mark(&mut prev, b);
						}
						for s in snaps {
							self.mark_inode_tree(s.inode_root, s.inode_root_crc, &mut prev)?;
						}
					}
					Err(e) => self.walk_damage.from_error(&e),
				}
			}
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
						// Ascending by construction - `i` and `bit` both climb - so the vector is
						// sorted and duplicate-free without being told to be.
						crate::try_push(&mut self.dead_prev, i as u64 * 8 + bit)?;
					}
				}
			}
		}
		self.pinned = pinned;
		self.dead.clear();
		// a walk that could not complete (an unreadable node, a broken spill chain)
		// leaves the free map incomplete: surface it, so the caller degrades to
		// read-only rather than allocate blocks the map failed to reserve.
		let mut damage = core::mem::take(&mut self.walk_damage);
		// two owners of one block in the live generation. The map itself cannot show it -
		// it is one bit either way - so it is caught while deriving and reported here.
		// Allocating from such a map is safe; the danger is the DELETE, which returns a
		// block the other owner still reads. Read-only forecloses both.
		//
		// STRUCTURAL, not a checksum failure: every block involved matched its checksum, and what
		// is wrong is that two owners claim one of them. The medium did exactly what it was told.
		if dup.is_some() {
			damage.structural = true;
		}
		// An inode reachable under two names is the same kind of statement: the bytes are what were
		// written and the namespace they describe cannot be true.
		if self.mark_alias {
			damage.structural = true;
		}
		Ok(damage)
	}

	// The answer the mount path and the commit path want: did the walk leave a free map that can be
	// allocated from? `Corrupt` for any damage, whatever kind - the allocator's question has one
	// answer, and the categories are for the operator's report.
	pub(crate) fn derive_free(&mut self) -> Result<(), FsError> {
		if self.derive_free_kinds()?.any() { Err(FsError::Corrupt) } else { Ok(()) }
	}

	// Mark, in `map`, every block the inode B+tree rooted at `ptr` references: the tree
	// nodes themselves, and for each live inode either its file data / checksum /
	// overflow blocks or its directory's B+tree. Reads are raw (no checksum check), like
	// the old generation walk. Damage - an unreadable node, a spill chain that fails its
	// CRC - is FLAGGED (`walk_damage`) and skipped, never fatal: aborting the mount here
	// would present an intact-superblock volume as unformatted, and one flipped bit must
	// not cost the volume. Iterative (an explicit work list), so the depth of the trees
	// never grows the call stack.
	pub(crate) fn mark_inode_tree(&mut self, root: u64, root_crc: u32, map: &mut [u8]) -> Result<(), FsError> {
		// Each queued node carries the key interval the separators above it route to.
		//
		// Without it this walk could say a node was well-formed and not that it BELONGED where it
		// was reached from. `validate_fixed_leaf` and `validate_internal` are local: a leaf holding
		// keys 100, 200, 900 is perfectly ordered, and if the root separator sends 900 down the
		// other child then no lookup will ever find that record - so an insert of the same key lands
		// in the other subtree and the volume now holds it twice, in a freshly checksummed
		// generation, with every later read decided by which path it walks.
		//
		// `fsck` has long carried this interval and a writable mount did not run `fsck`.
		// Carrying it HERE costs no extra I/O: this walk already visits every live tree block to
		// build the free map, so the range rides along and the answer becomes "the block is
		// reserved AND it is where the tree says it should be".
		let mut nodes: Vec<(u64, u32, Option<u64>, Option<u64>)> = Vec::new();
		// a pointer outside the pool is a corrupt link (skipped, not followed into
		// whatever lies past the volume); an already-marked node is either a corrupt
		// cycle (which must not hang the walk) or a subtree shared with an earlier
		// root walked into the same map - marked means walked (or queued), so skip
		// both. Marking happens at PUSH, so a hostile node fanning hundreds of links
		// at one block queues it once, not once per link - the work list stays
		// bounded by the pool.
		if root != 0 && root < self.num_blocks && self.mark(map, root) {
			nodes.push((root, root_crc, None, None));
		}
		let mut buf = try_zeroed(BLOCK_SIZE)?;
		while let Some((ptr, want, lower, upper)) = nodes.pop() {
			// The device did not answer. The metadata may be perfectly good.
			if !self.dev.read_block(ptr, &mut buf) {
				self.walk_damage.io = true;
				continue;
			}
			// Under `mark_strict` - the live generation - a node is checked against the CRC
			// its PARENT recorded for it. Without this the walk followed any block that read
			// cleanly, so a node the parent would have rejected could move an extent pointer,
			// hide a subtree, or omit a live block from the map, and the mount stayed
			// WRITABLE, because nothing had failed. A checksum-blind walk is a recovery tool,
			// not a source of truth for an allocator. The older generations keep the blind
			// walk on purpose: there the point is to reserve what MIGHT still be referenced,
			// and refusing to follow damage would under-reserve.
			// A block came back and is not what was written.
			if self.mark_strict && crc32c(&buf) != want {
				self.walk_damage.checksum = true;
				continue;
			}
			// a byte that is neither node type was treated as an internal node, so the
			// walk read separators and child links out of whatever the block held. A
			// block that is not a node of the tree is damage in every generation.
			// The checksum matched and the block is still not a node. Structural.
			if node_type(&buf) != NODE_LEAF && node_type(&buf) != NODE_INTERNAL {
				self.walk_damage.structural = true;
				continue;
			}
			// The LOCAL structure too, under `mark_strict`. The interval above proves a node
			// belongs where it was reached from; these prove it is a node at all - keys ordered in
			// a leaf, separators ordered and children present in an internal node.
			//
			// "Local and absent are different things": the comment above explains why the interval
			// was added and reads as though the local checks were being relied on elsewhere.
			// `fsck` runs them and `tree_insert_node` runs them before mutating; the mount did not.
			// A CRC-valid leaf holding keys 0, 2, 1 is inside its interval and unordered, so it
			// passed - and then `tree_lookup` binary-searches it, answers `None` for a key that is
			// there, and `remove_inner` treats exactly that as a dangling directory entry and drops
			// the only name of a live inode. On purpose, as the operator's repair verb, on a volume
			// that was never broken in the way that verb assumes.
			//
			// It costs no I/O: the block is already read.
			if self.mark_strict {
				let local = if node_type(&buf) == NODE_LEAF { validate_fixed_leaf(&buf, INODE_REC, 8) } else { validate_internal(&buf) };
				if local.is_err() {
					self.walk_damage.structural = true;
					continue;
				}
			}
			if node_type(&buf) == NODE_LEAF {
				for i in 0..leaf_count(&buf, INODE_REC) {
					let rec = NODE_HDR + i * INODE_REC;
					if self.mark_strict {
						let key = u64::from_le_bytes(buf[rec..rec + 8].try_into().unwrap());
						self.mark_max_inode = self.mark_max_inode.max(key);
						// Outside the interval routing sends here, so no lookup can reach it -
						// which makes a later insert of the same key create a SECOND record in the
						// subtree that lookup does reach. Damage, so the mount degrades to
						// read-only: the data and the repair verb both survive, and nothing writes
						// a new generation over a tree that already contradicts itself.
						//
						// Only under `mark_strict`. The snapshot and previous-generation walks are
						// deliberately blind - there the job is to reserve what might still be
						// referenced, and refusing to follow a wrong-looking subtree would
						// under-reserve.
						if lower.is_some_and(|l| key < l) || upper.is_some_and(|u| key >= u) {
							self.walk_damage.structural = true;
						}
					}
					let off = rec + 8;
					let mut inode = Inode::parse(&buf[off..off + INODE_SIZE]);
					if inode.r#type == TYPE_FILE {
						// complete the extent map from the overflow chain before marking
						// (the spill and dir walks use their own buffers, so the leaf
						// image in `buf` stays intact). A file whose chain cannot be
						// loaded is damage, not a mount failure.
						let marked = self.load_spill(&mut inode).and_then(|()| self.collect_inode_blocks(&inode, map));
						if let Err(e) = marked {
							self.walk_damage.from_error(&e);
						}
					} else if inode.r#type == TYPE_DIR {
						self.mark_dir_tree(inode.dir_root, inode.dir_root_crc, map)?;
					} else {
						// A type byte that is neither is documented as inert - reads and
						// writes refuse it, a listing shows it, `remove` clears it - and that
						// story holds for an inode AUTHORED that way. It does not hold for the
						// way one actually appears: a flipped byte in the type field of a real
						// file. Its data, checksum and spill blocks were then reserved by
						// nobody while the mount stayed writable, so the allocator could hand
						// out blocks that were still recoverable.
						//
						// Marked as a FILE, which is what `Inode::parse` already builds it as.
						// Degrading to read-only would protect the same blocks and take the
						// repair verb with it - the operator could no longer remove the record
						// that caused it. Reserving is the conservative direction here: at
						// worst it holds blocks nothing needs until the inode is removed, and
						// at best it is exactly right, because the record usually IS a file.
						// The structural pass reports the type, so it is not silent either.
						let marked = self.load_spill(&mut inode).and_then(|()| self.collect_inode_blocks(&inode, map));
						if let Err(e) = marked {
							self.walk_damage.from_error(&e);
						}
						// AND as a directory, because the flip could have gone the other way.
						//
						// `INO_MAP` is an overlay: `dir_root` for a directory, `spill` for
						// everything else. Reserving only the file reading protects one
						// bit-flip out of two - an inode that WAS a directory arrives with
						// `extent_count` zero, so the file reading marks almost nothing, and
						// its B+tree's internal and leaf nodes are left free for the allocator
						// while the tree still references them. The mirror of the case this
						// was written for: the child inodes and their data survive, and the
						// index that named them is overwritten.
						//
						// So both readings are marked. They cost the same blocks when the file
						// reading is the right one (`mark_dir_tree` refuses a block that is not
						// a node and stops), and the union is the only answer that is safe
						// whichever way the byte went. Over-reserving holds space until the
						// record is removed; under-reserving loses data.
						// SPECULATIVE, so what it finds is never reported as damage. One of the two
						// readings is nonsense by definition - for a real file the overlay points at
						// a spill chain block, which is not a tree node - and a walk that says so is
						// telling the truth about the guess, not about the volume. Degrading the
						// mount to read-only on it would take the repair verb away over a reading
						// that was never expected to hold.
						// Into its OWN map, then folded in.
						//
						// Three things go wrong if it marks straight into `map`. The root has
						// already been marked by the file reading (the overlay is one field, so both
						// readings start at the same block), and `mark_dir_tree` skips an
						// already-marked node - so the walk would stop at the root and reserve none
						// of the children, which is the entire point. Under `mark_strict` the
						// overlap would also read as two owners in one generation, which is
						// corruption, and the CRC check would refuse a guess for not verifying.
						//
						// A scratch map costs one allocation on a path that only runs for a damaged
						// inode, and it lets the guess be a guess: it descends as far as the bytes
						// allow, says nothing about what it finds, and its result is added to the
						// reservation rather than compared with it.
						//
						// The map is allocated with `?`, and the distinction matters more than it
						// looks. A failure to PARSE the guess is correctly ignored, for the reason
						// above. A failure to ALLOCATE the scratch map is not the guess failing; it
						// is the protection not happening. This was `try_zeroed(..).ok()` with an
						// `if let` around it, so under memory pressure the whole speculative walk
						// was skipped, `derive_free` returned Ok, the mount stayed writable, and
						// none of the directory's tree blocks were reserved - which is exactly the
						// loss this reading exists to prevent, disappearing without a word at the
						// moment the machine is under strain. `NoMemory` at mount is an answer this
						// crate carries everywhere else; it is the right one here too.
						// Allocated before the walk state is touched, so the error path leaves
						// nothing half-set behind it.
						let mut scratch = try_zeroed(map.len())?;
						let strict = self.mark_strict;
						let damage = self.walk_damage;
						self.mark_strict = false;
						let _ = self.mark_dir_tree(inode.dir_root_from_overlay(), inode.dir_root_crc_from_overlay(), &mut scratch);
						for (into, from) in map.iter_mut().zip(scratch.iter()) {
							*into |= *from;
						}
						self.mark_strict = strict;
						self.walk_damage = damage;
					}
				}
			} else {
				let count = internal_count(&buf);
				for i in 0..=count {
					let child = child_ptr(&buf, i);
					// The interval narrows by one separator on each side: child `i` holds keys at or
					// above separator `i - 1` and below separator `i`, with the ends open.
					let child_lower = if i == 0 { lower } else { Some(sep_key(&buf, i - 1)) };
					let child_upper = if i == count { upper } else { Some(sep_key(&buf, i)) };
					if child < self.num_blocks && self.mark(map, child) {
						nodes.push((child, child_crc(&buf, i), child_lower, child_upper));
					}
				}
			}
		}
		Ok(())
	}

	// Mark every node block of a directory's B+tree. The entries themselves point at
	// inodes, which the inode-tree walk already covers, so only the nodes are marked.
	// Iterative and damage-tolerant like `mark_inode_tree`.
	pub(crate) fn mark_dir_tree(&mut self, root: u64, root_crc: u32, map: &mut [u8]) -> Result<(), FsError> {
		// The same routing interval `mark_inode_tree` carries, for the same reason, and it was left
		// out when that one got it.
		//
		// The directory tree is where the defect actually bites. A record whose hash the separators
		// above route to the OTHER child is unreachable by lookup - so a create of that same name
		// goes down the side routing does reach and writes a SECOND record for it, and the volume
		// now has one name twice in one directory, in a freshly checksummed generation. Every local
		// check passes on both leaves: ordering, the stored hash, the name policy, the CRC.
		//
		// `fsck` has long found this (`fsck_finds_a_record_routing_will_never_reach`) and a
		// writable mount did not run `fsck`.
		let mut nodes: Vec<(u64, u32, Option<u64>, Option<u64>)> = Vec::new();
		// same guards as `mark_inode_tree`: skip out-of-pool links and marked blocks,
		// marking at push so the work list stays bounded by the pool.
		if root != 0 && root < self.num_blocks && self.mark(map, root) {
			nodes.push((root, root_crc, None, None));
		}
		let mut buf = try_zeroed(BLOCK_SIZE)?;
		while let Some((ptr, want, lower, upper)) = nodes.pop() {
			// The device did not answer. The metadata may be perfectly good.
			if !self.dev.read_block(ptr, &mut buf) {
				self.walk_damage.io = true;
				continue;
			}
			// A block came back and is not what was written.
			if self.mark_strict && crc32c(&buf) != want {
				self.walk_damage.checksum = true;
				continue;
			}
			// The checksum matched and the block is still not a node. Structural.
			if node_type(&buf) != NODE_LEAF && node_type(&buf) != NODE_INTERNAL {
				self.walk_damage.structural = true;
				continue;
			}
			if node_type(&buf) == NODE_INTERNAL {
				let count = internal_count(&buf);
				for i in 0..=count {
					let child = child_ptr(&buf, i);
					let child_lower = if i == 0 { lower } else { Some(sep_key(&buf, i - 1)) };
					let child_upper = if i == count { upper } else { Some(sep_key(&buf, i)) };
					if child < self.num_blocks && self.mark(map, child) {
						nodes.push((child, child_crc(&buf, i), child_lower, child_upper));
					}
				}
			} else if self.mark_strict {
				// ONLY the routing interval here, and NOT the local validators, which is a
				// deliberate difference from what an audit asked for.
				//
				// A locally malformed leaf - unsorted, truncated, a stored hash that is not the
				// name's - already has a considered answer in this tree: the listing still works,
				// because it is what the operator has left, and every EDIT is refused as `Corrupt`.
				// Two tests pin that (`an_unsorted_directory_leaf_may_be_read_but_not_edited` and
				// its truncated twin), and running the same validators here would turn one damaged
				// directory into a read-only volume and take the rescue away.
				//
				// A MISROUTED record is a different kind of fault and the write path cannot see it.
				// Both leaves are locally perfect; what is wrong is that one of them is in the
				// wrong place. Lookup goes where the separators point, does not find the name, and
				// a create then writes a SECOND record for it - one name twice in one directory, in
				// a freshly checksummed generation, with nothing afterwards to say which is real.
				// That is worth the whole mount.
				for rec in &crate::dir::dir_leaf_parse(&buf) {
					if lower.is_some_and(|l| rec.hash < l) || upper.is_some_and(|u| rec.hash >= u) {
						self.walk_damage.structural = true;
					}
					// ONE INODE, ONE NAME - checked here because this walk is already reading every
					// leaf of every directory in the live generation, so it costs a bit per inode
					// and no extra I/O.
					//
					// There is no hardlink API and no link count, so that is the format's rule and
					// nothing enforced it. An image with `/a` and `/b` both naming inode 7 mounted
					// writable, and `remove("a")` then freed inode 7's blocks and deleted it from
					// the inode tree while `/b` still pointed at it - a live name resolving to a
					// record that is gone, and blocks the allocator will hand to something else.
					//
					// Read-only rather than a walk-damage flag: the repair for an alias is to
					// remove one of the two names, and that removal is exactly the operation that
					// destroys the shared inode. `fsck` names both.
					// APPENDED, NOT INSERTED IN ORDER. Nothing about the correctness needs this
					// sorted DURING the walk: the question is "has this inode been seen", and the
					// answer is only wanted at the end.
					//
					// Sorted insertion was O(N²) memmove during MOUNT. Inode numbers arrive in the
					// order the namespace walk reaches them and the directory B+tree is ordered by
					// `(name_hash, name)`, so arrival order is effectively unrelated to numeric
					// order and each insert moved about half the vector - on a filesystem whose own
					// milestones aim at directories with millions of entries.
					//
					// FALLIBLY, which is the whole reason this is a `Vec`: a mount that cannot hold
					// one more inode number answers `NoMemory` and is refused. `try_push` rather
					// than `try_reserve` so the fault injector is inside the allocation the test
					// sweeping budgets is named for.
					if let Some(names) = self.mark_names.as_mut() {
						crate::try_push(names, rec.child)?;
					}
				}
			}
		}
		Ok(())
	}

	// B+tree node and generic tree operations

	// Read a B+tree node block, verifying it against the CRC32C its parent link stored.
	// A mismatch is FsError::Corrupt, so on-disk damage to a tree node is caught on the
	// live path (lookup / insert / delete / enumeration / fsck). A pointer outside the
	// pool is the same damage: past the pool's end lies another partition's data on a
	// shared device, and a checksum proves integrity, not sanity - a forged link with a
	// matching CRC must not surface foreign bytes as tree contents.
	pub(crate) fn read_node(&mut self, ptr: u64, crc: u32, buf: &mut [u8]) -> Result<(), FsError> {
		self.read_node_raw(ptr, crc, buf)?;
		// AND THE NODE HAS TO SAY WHICH KIND IT IS. Every caller tests `node_type(&buf) ==
		// NODE_LEAF` and treats everything else as an INTERNAL node - so a byte of 2, or 200, was
		// routed through as a router and its bytes read as (key, pointer, CRC) triples. The CRC
		// proves integrity and not sanity: a node whose kind field is not one this format defines is
		// damage, and this is the one place every live walker comes through.
		if !matches!(buf[0], NODE_INTERNAL | NODE_LEAF) {
			return Err(FsError::Corrupt);
		}
		Ok(())
	}

	// The same read WITHOUT the kind check, for `fsck`.
	//
	// The checker's whole job is to classify a block that is not a node, so it has to be able to
	// READ one: refusing it here would turn `the_snapshot_checker_refuses_a_block_that_is_not_a_node`
	// into an error where it currently reports one structural fault and carries on. The live paths
	// get the refusal; the checker gets the bytes and decides what they are.
	pub(crate) fn read_node_raw(&mut self, ptr: u64, crc: u32, buf: &mut [u8]) -> Result<(), FsError> {
		if ptr >= self.num_blocks {
			return Err(FsError::Corrupt);
		}
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
		let mut buf = try_zeroed(BLOCK_SIZE)?;
		// bounded descent: no legitimate tree is deeper than TREE_DEPTH_MAX, so a
		// longer path is a hostile chain of one-child internals - Corrupt, not a crawl.
		for _ in 0..TREE_DEPTH_MAX {
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
		Err(FsError::Corrupt)
	}

	// Insert or overwrite `record` (numeric key `key`, full key width `keylen`) in the
	// B+tree rooted at (`root`, `root_crc`); `rec` is the record width and `leaf_max` the
	// leaf capacity. Returns the new root (ptr, crc). Copy-on-write: every node on the
	// path is rewritten to a fresh block (or in place if already fresh this transaction).
	pub(crate) fn tree_insert(&mut self, root: u64, root_crc: u32, key: u64, record: &[u8], rec: usize, leaf_max: usize, keylen: usize) -> Result<(u64, u32), FsError> {
		if root == 0 {
			// empty tree: a new leaf with the single record.
			let blk = self.alloc_meta()?;
			let mut buf = try_zeroed(BLOCK_SIZE)?;
			node_set_header(&mut buf, NODE_LEAF, 1);
			buf[NODE_HDR..NODE_HDR + rec].copy_from_slice(record);
			let crc = self.write_node_to(blk, &buf)?;
			return Ok((blk, crc));
		}
		let outcome = self.tree_insert_node(root, root_crc, key, record, rec, leaf_max, keylen, TREE_DEPTH_MAX)?;
		self.settle_root(outcome)
	}

	// Turn an insert outcome into the tree's new root: an updated node is the root as
	// is; a split builds a new internal root over the two halves.
	pub(crate) fn settle_root(&mut self, outcome: Ins) -> Result<(u64, u32), FsError> {
		match outcome {
			Ins::Updated(p, c) => Ok((p, c)),
			Ins::Split(lp, lc, sep, rp, rc) => {
				let blk = self.alloc_meta()?;
				let mut buf = try_zeroed(BLOCK_SIZE)?;
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
					let mut lbuf = try_zeroed(BLOCK_SIZE)?;
					node_set_header(&mut lbuf, NODE_INTERNAL, mid);
					for i in 0..mid {
						set_sep(&mut lbuf, i, seps[i]);
					}
					for i in 0..=mid {
						set_child(&mut lbuf, i, kids[i].0, kids[i].1);
					}
					let rcount = s - mid - 1;
					let mut rbuf = try_zeroed(BLOCK_SIZE)?;
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

	pub(crate) fn tree_insert_node(&mut self, ptr: u64, crc: u32, key: u64, record: &[u8], rec: usize, leaf_max: usize, keylen: usize, depth: usize) -> Result<Ins, FsError> {
		// the depth budget bounds the recursion (and so the stack) against a hostile
		// chain of one-child internals; no legitimate tree comes near it.
		if depth == 0 {
			return Err(FsError::Corrupt);
		}
		let mut buf = try_zeroed(BLOCK_SIZE)?;
		self.read_node(ptr, crc, &mut buf)?;
		if node_type(&buf) == NODE_LEAF {
			// The search below is a binary search, so a leaf whose keys do not ascend answers
			// arbitrarily and the insert acts on that answer. `leaf_count` clamps the count to what
			// the block can hold, which keeps the loop in bounds and says nothing about the order.
			validate_fixed_leaf(&buf, rec, keylen).map_err(|_| FsError::Corrupt)?;
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
			let mut lbuf = try_zeroed(BLOCK_SIZE)?;
			node_set_header(&mut lbuf, NODE_LEAF, split);
			for (i, r) in recs[..split].iter().enumerate() {
				let off = NODE_HDR + i * rec;
				lbuf[off..off + rec].copy_from_slice(r);
			}
			let mut rbuf = try_zeroed(BLOCK_SIZE)?;
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
		validate_internal(&buf).map_err(|_| FsError::Corrupt)?;
		let ci = route_child(&buf, internal_count(&buf), key);
		let cp = child_ptr(&buf, ci);
		let cc = child_crc(&buf, ci);
		let outcome = self.tree_insert_node(cp, cc, key, record, rec, leaf_max, keylen, depth - 1)?;
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
		match self.tree_delete_node(root, root_crc, key, probe, rec, keylen, TREE_DEPTH_MAX)? {
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
		let mut buf = try_zeroed(BLOCK_SIZE)?;
		// bounded like every descent: a longer single-child chain is a hostile shape.
		for _ in 0..TREE_DEPTH_MAX {
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
		Err(FsError::Corrupt)
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

	pub(crate) fn tree_delete_node(&mut self, ptr: u64, crc: u32, key: u64, probe: &[u8], rec: usize, keylen: usize, depth: usize) -> Result<Del, FsError> {
		// bounded like the insert recursion: a deeper path is a hostile shape.
		if depth == 0 {
			return Err(FsError::Corrupt);
		}
		let mut buf = try_zeroed(BLOCK_SIZE)?;
		self.read_node(ptr, crc, &mut buf)?;
		if node_type(&buf) == NODE_LEAF {
			validate_fixed_leaf(&buf, rec, keylen).map_err(|_| FsError::Corrupt)?;
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
		validate_internal(&buf).map_err(|_| FsError::Corrupt)?;
		let ci = route_child(&buf, internal_count(&buf), key);
		let cp = child_ptr(&buf, ci);
		let cc = child_crc(&buf, ci);
		let outcome = self.tree_delete_node(cp, cc, key, probe, rec, keylen, depth - 1)?;
		self.internal_absorb_del(&mut buf, ptr, ci, outcome)
	}
}

// What is wrong with a node, judged from the block alone.
//
// The write path and the structural pass ask the same question about a node and used to answer it
// separately. `fsck` knew ten invariants; a writable mount checked ONE - that a directory leaf holds
// as many records as its header claims - and edited whatever else it found. The reachable case is
// ordering: `dir_recs_search` is a binary search, so a checksum-valid leaf holding hashes in the
// order 900, 100, 500 answers arbitrarily, and `dir_insert_node` takes that answer as either "the
// name is here, replace its child" or "insert at this position". It then writes the leaf back and
// `write_node_to` computes a fresh CRC over it: a new generation, correctly checksummed, built on a
// structure the format cannot produce, with nothing left to say which name was duplicated and which
// became unreachable.
//
// So the rules live here, once, and both callers use them. `fsck` turns a fault into a sentence;
// the write path turns it into `Corrupt` and refuses the mutation, leaving the damage where an
// operator can still see it. Only the LOCAL invariants are here - those a single block answers.
// Routing intervals, duplicate names across a whole directory and blocks appearing twice need the
// walk, and stay in the structural pass.
//
// Refusing to mutate a damaged node takes a repair verb away, which is a real cost and the one this
// trades against: `remove` on a directory holding a bad record now fails. That is the same trade
// the truncated-leaf check already made, and the alternative is a mutation that makes the damage
// authoritative and consistent, which no later pass can distinguish from a volume that was always
// that way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeFault {
	// The header's count is more than the block can physically hold.
	CountAboveCapacity,
	// The parse stopped early: fewer records are present than the header claims.
	Truncated { held: usize, claimed: usize },
	// Keys, or (hash, name) pairs, are not strictly ascending. Strictness is what also rules out a
	// duplicate within the node.
	OutOfOrder,
	// A directory record's stored hash is not the hash of its name.
	StoredHashMismatch,
	// A directory record's name is not one this format can address.
	NameNotAddressable,
	// An internal node's separators are not strictly ascending.
	SeparatorsOutOfOrder,
	// An internal node routes a whole key interval into a slot pointing nowhere.
	ChildMissing(usize),
}

impl NodeFault {
	// The sentence `fsck` reports, given the block it is about. The write path never calls this.
	pub(crate) fn describe(&self, at: u64) -> Vec<u8> {
		match *self {
			NodeFault::CountAboveCapacity => alloc::format!("node {at} claims more records than it can hold"),
			NodeFault::Truncated { held, claimed } => alloc::format!("leaf {at} holds {held} of the {claimed} records it claims"),
			NodeFault::OutOfOrder => alloc::format!("leaf {at} is not in ascending key order"),
			NodeFault::StoredHashMismatch => alloc::format!("leaf {at} holds a record whose stored hash is not its name's"),
			NodeFault::NameNotAddressable => alloc::format!("leaf {at} holds a record whose name is not one this format can address"),
			NodeFault::SeparatorsOutOfOrder => alloc::format!("node {at} has separators out of order"),
			NodeFault::ChildMissing(slot) => alloc::format!("node {at} has no child in slot {slot}, so a range of names routes nowhere"),
		}
		.into_bytes()
	}
}

// The invariants an INTERNAL node must satisfy before anything descends through it or edits it.
//
// A descent picks a slot by comparing the key against the separators, so separators out of order
// route to the wrong subtree, and a null child routes a whole interval into block 0.
pub(crate) fn validate_internal(buf: &[u8]) -> Result<(), NodeFault> {
	if node_count(buf) > INTERNAL_MAX - 1 {
		return Err(NodeFault::CountAboveCapacity);
	}
	let count = internal_count(buf);
	let mut last: Option<u64> = None;
	for i in 0..count {
		let sep = sep_key(buf, i);
		if last.is_some_and(|previous| sep <= previous) {
			return Err(NodeFault::SeparatorsOutOfOrder);
		}
		last = Some(sep);
	}
	for i in 0..=count {
		if child_ptr(buf, i) == 0 {
			return Err(NodeFault::ChildMissing(i));
		}
	}
	Ok(())
}

// The same for a fixed-record LEAF of the generic tree - the inode tree today. `keylen` is the
// prefix of a record that is its key, which is what `tree_insert_node`'s binary search compares.
pub(crate) fn validate_fixed_leaf(buf: &[u8], rec: usize, keylen: usize) -> Result<(), NodeFault> {
	if node_count(buf) > (BLOCK_SIZE - NODE_HDR) / rec {
		return Err(NodeFault::CountAboveCapacity);
	}
	let count = leaf_count(buf, rec);
	for i in 1..count {
		let previous = NODE_HDR + (i - 1) * rec;
		let current = NODE_HDR + i * rec;
		if key_cmp(&buf[previous..previous + keylen], &buf[current..current + keylen]) != Ordering::Less {
			return Err(NodeFault::OutOfOrder);
		}
	}
	Ok(())
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

// Reserve exactly `n` block numbers, or report that the memory could not be had.
//
// A FUNCTION RATHER THAN A BARE `try_reserve_exact`, so the allocation injector can reach it.
// `inject` hooks `try_zeroed` and nothing else, which left the one refusal path inside `commit()`
// with no way to be exercised - and a path that cannot be exercised is how that one came to be
// written as a bare `?`, returning `NoMemory` with the transaction still open for somebody else's
// commit to publish. The helper is the difference between a rule and a hope.
fn try_reserve_blocks(target: &mut alloc::vec::Vec<u64>, n: usize) -> Result<(), FsError> {
	#[cfg(test)]
	if crate::inject::should_fail() {
		return Err(FsError::NoMemory);
	}
	target.try_reserve_exact(n).map_err(|_| FsError::NoMemory)
}
