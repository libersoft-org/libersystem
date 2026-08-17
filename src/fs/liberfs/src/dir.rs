use crate::*;

impl<D: BlockDevice> LiberFs<D> {
	// Resolve a full path to its inode number, walking directories from the root.
	pub(crate) fn resolve(&mut self, path: &[u8]) -> Result<u32, FsError> {
		let segs = split_segments(path)?;
		let mut inode_num = self.root_inode;
		for seg in segs {
			inode_num = self.dir_lookup(inode_num, seg)?.ok_or(FsError::NotFound)?;
		}
		Ok(inode_num)
	}

	// Resolve a path to (the parent directory inode, the final segment). With
	// `create`, missing parent directories are created (mkdir -p); without it, a
	// missing parent is an error.
	pub(crate) fn resolve_parent<'a>(&mut self, path: &'a [u8], create: bool) -> Result<(u32, &'a [u8]), FsError> {
		let segs = split_segments(path)?;
		let last: &'a [u8] = segs[segs.len() - 1];
		let mut parent = self.root_inode;
		for &seg in &segs[..segs.len() - 1] {
			parent = if create {
				self.dir_lookup_or_create(parent, seg)?
			} else {
				let child = self.dir_lookup(parent, seg)?.ok_or(FsError::NotFound)?;
				if self.read_inode(child)?.r#type != TYPE_DIR {
					return Err(FsError::NotDir);
				}
				child
			};
		}
		Ok((parent, last))
	}

	// Find child `name` in `parent`, or create it as a directory; return its inode.
	pub(crate) fn dir_lookup_or_create(&mut self, parent: u32, name: &[u8]) -> Result<u32, FsError> {
		if let Some(child) = self.dir_lookup(parent, name)? {
			if self.read_inode(child)?.r#type != TYPE_DIR {
				return Err(FsError::NotDir);
			}
			return Ok(child);
		}
		let num = self.alloc_inode()?;
		let mut dir = Inode::empty(TYPE_DIR);
		dir.ctime = self.clock;
		dir.mtime = self.clock;
		self.write_inode(num, &mut dir)?;
		self.dir_insert(parent, name, num)?;
		Ok(num)
	}

	// directory operations (on any directory inode)

	// Look up `name` in directory `dir_num` through its B+tree: the child inode, or None
	// if absent. NotDir if `dir_num` is not a directory. A hit populates the bounded
	// dentry cache, so path resolution stops re-walking the tree for hot names.
	pub(crate) fn dir_lookup(&mut self, dir_num: u32, name: &[u8]) -> Result<Option<u32>, FsError> {
		if let Some(child) = self.dcache.get(&(dir_num, name.to_vec())) {
			return Ok(Some(*child));
		}
		let dir = self.read_inode(dir_num)?;
		if dir.r#type != TYPE_DIR {
			return Err(FsError::NotDir);
		}
		match self.dir_tree_lookup(dir.dir_root, dir.dir_root_crc, name)? {
			Some(child) => {
				self.dcache_put(dir_num, name, child);
				Ok(Some(child))
			}
			None => Ok(None),
		}
	}

	// Remember (directory, name) -> child, evicting the LARGEST key once the cache is
	// full (the root directory's entries - directory 0, the hottest - stay put; plain
	// bounded eviction, the cache only skips re-reads). Overwriting a cached key
	// evicts nothing, like `icache_put`.
	pub(crate) fn dcache_put(&mut self, dir_num: u32, name: &[u8], child: u32) {
		let key = (dir_num, name.to_vec());
		if self.dcache.len() >= DCACHE_MAX && !self.dcache.contains_key(&key) {
			if let Some(k) = self.dcache.keys().next_back().cloned() {
				self.dcache.remove(&k);
			}
		}
		self.dcache.insert(key, child);
	}

	// Insert entry `name` -> `child` into directory `dir_num`, or repoint it if it is
	// already there. The directory's B+tree root (and the entry count it stores in
	// `size`) are updated and the directory inode rewritten.
	pub(crate) fn dir_insert(&mut self, dir_num: u32, name: &[u8], child: u32) -> Result<(), FsError> {
		let mut dir = self.read_inode(dir_num)?;
		if dir.r#type != TYPE_DIR {
			return Err(FsError::NotDir);
		}
		let existed = self.dir_tree_lookup(dir.dir_root, dir.dir_root_crc, name)?.is_some();
		let (root, crc) = self.dir_tree_insert(dir.dir_root, dir.dir_root_crc, name, child)?;
		dir.dir_root = root;
		dir.dir_root_crc = crc;
		if !existed {
			// a checksummed `size` of u64::MAX is not something any writer produces, and
			// nothing refuses such an inode at mount - so this panicked in debug and wrapped
			// to zero in release, committing a count that says the directory is empty.
			dir.size = dir.size.checked_add(1).ok_or(FsError::Corrupt)?;
		}
		dir.mtime = self.clock;
		self.write_inode(dir_num, &mut dir)?;
		self.dcache_put(dir_num, name, child);
		Ok(())
	}

	// Is this directory tree empty? One node read, whatever the tree's size - which is what makes
	// it affordable on every removal where a full walk is not.
	fn dir_tree_is_empty(&mut self, root: u64, crc: u32) -> bool {
		if root == 0 {
			return true;
		}
		// A refused allocation cannot be reported from a `bool`, and answering "empty" for a
		// directory this could not read would be the false end-of-directory this milestone just
		// removed one layer up. Not empty is the safe answer: the caller re-reads.
		let Ok(mut buf) = try_zeroed(BLOCK_SIZE) else { return false };
		if self.read_node(root, crc, &mut buf).is_err() {
			// Unreadable is not empty, and a removal must not conclude anything from it.
			return false;
		}
		node_type(&buf) == NODE_LEAF && node_count(&buf) == 0
	}

	// Remove entry `name` from directory `dir_num`. NotFound if it is not there.
	pub(crate) fn dir_remove(&mut self, dir_num: u32, name: &[u8]) -> Result<(), FsError> {
		let mut dir = self.read_inode(dir_num)?;
		if dir.r#type != TYPE_DIR {
			return Err(FsError::NotDir);
		}
		let (root, crc, removed) = self.dir_tree_delete(dir.dir_root, dir.dir_root_crc, name)?;
		if !removed {
			return Err(FsError::NotFound);
		}
		self.dcache.remove(&(dir_num, name.to_vec()));
		dir.dir_root = root;
		dir.dir_root_crc = crc;
		// THE TREE IS THE TRUTH, and `size` is a cache of it. `saturating_sub` meant a directory
		// whose stored count was 0 while its tree held five entries lost a real entry and kept the
		// count at 0 - and the inode was then re-checksummed and committed, so the removal made the
		// lie permanent and internally consistent.
		//
		// Refusing is the wrong answer here even though the state is corrupt: removing the children
		// is exactly how an operator repairs such a directory, and refusing closes the only route
		// they have. So a count that cannot describe what was just removed is DERIVED from the tree
		// that remains - the one operation that both survives the damage and ends it.
		// UNDERFLOW IS THE DETECTABLE CASE, NOT THE ONLY ONE. A stored count of 100 over a tree of
		// five decrements to 99 and is written back as though it were now correct - the same lie,
		// made permanent by the same commit, and invisible to the arithmetic because subtracting one
		// from 100 succeeds.
		//
		// Deriving on EVERY removal would close it and is refused deliberately: `collect_dir_entries`
		// walks the whole tree, so `rm -rf` over a large directory would become quadratic. Paying
		// O(n) per unlink to repair a count that is only ever wrong on a damaged volume is the wrong
		// trade.
		//
		// So the count stays a cache and stops being trusted where it is cheap not to trust it: an
		// EMPTY tree is the one shape a walk does not cost anything to confirm, and a non-zero count
		// over it is exactly the disagreement that has been observed. `fsck` reports the general
		// case - "size says N entries and the tree holds M" - which is where an operator can act on
		// it, and the deeper answer is to stop recording the count at all. That is a format change
		// and it is written up in the milestone rather than smuggled in here.
		dir.size = match dir.size.checked_sub(1) {
			Some(size) if size == 0 || !self.dir_tree_is_empty(root, crc) => size,
			// The count says entries remain and the tree says none do. Believe the tree.
			Some(_) => 0,
			None => {
				let mut entries = Vec::new();
				self.collect_dir_entries(root, crc, &mut entries, TREE_DEPTH_MAX)?;
				entries.len() as u64
			}
		};
		dir.mtime = self.clock;
		self.write_inode(dir_num, &mut dir)?;
		Ok(())
	}

	// Collect every (name, inode) entry in directory `dir_num`, in key order.
	pub(crate) fn dir_entries_of(&mut self, dir_num: u32) -> Result<Vec<(Vec<u8>, u32)>, FsError> {
		let dir = self.read_inode(dir_num)?;
		let mut out = Vec::new();
		self.collect_dir_entries(dir.dir_root, dir.dir_root_crc, &mut out, TREE_DEPTH_MAX)?;
		Ok(out)
	}

	// Does this directory hold anything, according to its TREE rather than its `size`?
	//
	// Emptiness was `inode.size == 0`, a cached count. A damaged image can have a
	// directory whose size says zero and whose tree still holds entries; deleting it then
	// freed the tree and left the child inodes in the global inode table - live to the
	// free-map walk, unreachable from any name, and invisible to a namespace-based
	// `fsck`, which can only see what it can reach.
	//
	// A tree that cannot be READ is not evidence of emptiness either: the error
	// propagates, so a delete refuses rather than assuming there was nothing there.
	pub(crate) fn dir_has_entries(&mut self, inode: &Inode) -> Result<bool, FsError> {
		let mut out = Vec::new();
		// one entry is the whole answer, so the walk stops there rather than reading the
		// rest of a directory it has already decided about.
		self.collect_dir_entries_bounded(inode.dir_root, inode.dir_root_crc, &mut out, TREE_DEPTH_MAX, true)?;
		Ok(!out.is_empty())
	}

	// Walk the directory B+tree rooted at (`ptr`, `crc`), appending each leaf's entries.
	//
	// Iterative, with a visited bitmap, and both matter. The depth budget alone bounded
	// the STACK and not the work: an internal node whose two hundred child links all point
	// at the next level's single node - checksums agreeing throughout, which is buildable
	// bottom-up - was walked `fanout ^ depth` times. Every ordinary directory operation
	// comes through here, so `list`, `read_dir`, `rmdir`, the replacing `rename`,
	// `subtree_contains` and the live namespace walk of `fsck` could all be made to run
	// for a practical eternity by one small image. A read-only mount does not help; these
	// are all reads.
	//
	// A block reached twice within ONE directory tree is corruption, not sharing: a tree
	// in a single generation is a tree, and the copy-on-write sharing that is legal
	// elsewhere is between generations, never inside one.
	//
	// `stop_early` lets a caller that only needs to know whether ANY entry exists stop at
	// the first one instead of building the whole list.
	//
	// The stack is LIFO, so children are pushed in REVERSE and child 0 pops first. Pushing
	// them forwards popped the last child first, which reverses the leaf GROUPS while
	// leaving the records inside each leaf sorted - so a directory small enough to be one
	// leaf listed correctly and any directory with an internal node listed backwards
	// through `list` and `read_dir`, both of which this method documents as key-ordered.
	// Every directory test fit in a single leaf, where LIFO and FIFO are the same thing,
	// which is why nothing noticed.
	pub(crate) fn collect_dir_entries_bounded(&mut self, root: u64, root_crc: u32, out: &mut Vec<(Vec<u8>, u32)>, depth: usize, stop_early: bool) -> Result<(), FsError> {
		if root == 0 {
			return Ok(());
		}
		// The visited set is proportional to the TREE, not to the volume. `try_zeroed(self.free.len())`
		// is one bit per block of the whole volume for the sake of walking one directory: 32 MiB at
		// 1 TiB, 128 MiB at 4 TiB, half a gigabyte at 16 TiB - allocated and thrown away by every
		// `read_dir`, every `rmdir`, and every `list`. A `BTreeSet` costs a node per block actually
		// visited, which is what the walk reads anyway - the same shape the transaction's `fresh`
		// and `dead` sets already use for block sets whose size follows the work.
		let mut visited: BTreeSet<u64> = BTreeSet::new();
		// (block, crc, depth remaining)
		let mut stack: Vec<(u64, u32, usize)> = vec![(root, root_crc, depth)];
		let mut buf = try_zeroed(BLOCK_SIZE)?;
		while let Some((ptr, crc, left)) = stack.pop() {
			// Only the ROOT may be absent, and it was handled above - so a zero reaching
			// here came out of a child slot of an internal node, which is an impossible
			// shape rather than an empty corner: a B+tree node routes a whole key interval
			// into every one of its `count + 1` slots, and a slot pointing nowhere means
			// every name in that interval resolves to nothing while the entry counts still
			// add up. This used to `continue`, so such a directory listed short and looked
			// healthy.
			if ptr == 0 {
				return Err(FsError::Corrupt);
			}
			if left == 0 || ptr >= self.num_blocks {
				return Err(FsError::Corrupt);
			}
			if !visited.insert(ptr) {
				return Err(FsError::Corrupt);
			}
			self.read_node(ptr, crc, &mut buf)?;
			if node_type(&buf) == NODE_LEAF {
				for rec in dir_leaf_parse(&buf) {
					out.push((rec.name, rec.child));
					if stop_early {
						return Ok(());
					}
				}
			} else {
				// reverse, so the LIFO stack pops child 0 first and the walk is a
				// left-to-right depth-first one - which is what "in key order" means.
				for i in (0..=internal_count(&buf)).rev() {
					stack.push((child_ptr(&buf, i), child_crc(&buf, i), left - 1));
				}
			}
		}
		Ok(())
	}

	pub(crate) fn collect_dir_entries(&mut self, ptr: u64, crc: u32, out: &mut Vec<(Vec<u8>, u32)>, depth: usize) -> Result<(), FsError> {
		self.collect_dir_entries_bounded(ptr, crc, out, depth, false)
	}

	// One PAGE of a directory: at most `max` entries in key order, starting after `after`.
	//
	// The tree is built for millions of entries and the only way to enumerate it returned every one
	// of them in a single `Vec`, with an inode read each. This walks the same tree and does the work
	// of a page: internal nodes are entered at the child the cursor's hash routes to, so the
	// subtrees entirely before the cursor are never read, and the walk stops as soon as the page is
	// full. Ordering is the tree's own - by (name hash, name) - which is what `after` means.
	pub(crate) fn collect_dir_page(&mut self, root: u64, root_crc: u32, after: Option<&[u8]>, max: usize, out: &mut Vec<(Vec<u8>, u32)>) -> Result<(), FsError> {
		if root == 0 || max == 0 {
			return Ok(());
		}
		let cursor = after.map(|name| (name_hash(name), name));
		let mut visited: BTreeSet<u64> = BTreeSet::new();
		let mut stack: Vec<(u64, u32, usize)> = vec![(root, root_crc, TREE_DEPTH_MAX)];
		let mut buf = try_zeroed(BLOCK_SIZE)?;
		while let Some((ptr, crc, left)) = stack.pop() {
			if ptr == 0 || left == 0 || ptr >= self.num_blocks {
				return Err(FsError::Corrupt);
			}
			if !visited.insert(ptr) {
				return Err(FsError::Corrupt);
			}
			self.read_node(ptr, crc, &mut buf)?;
			if node_type(&buf) == NODE_LEAF {
				for rec in dir_leaf_parse(&buf) {
					// Strictly after the cursor, in the tree's own order. A leaf can hold the
					// cursor itself and entries on both sides of it.
					if let Some((hash, name)) = cursor
						&& (name_hash(&rec.name), rec.name.as_slice()) <= (hash, name)
					{
						continue;
					}
					out.push((rec.name, rec.child));
					if out.len() == max {
						return Ok(());
					}
				}
			} else {
				let count = internal_count(&buf);
				// Everything before this child is entirely below the cursor. For a node above the
				// cursor `route_child` answers 0 and nothing is skipped, which is the same walk as
				// before; for one below it, everything is skipped, which is also right.
				let first = match cursor {
					Some((hash, _)) => route_child(&buf, count, hash),
					None => 0,
				};
				for i in (first..=count).rev() {
					stack.push((child_ptr(&buf, i), child_crc(&buf, i), left - 1));
				}
			}
		}
		Ok(())
	}

	// A page of `read_dir`'s rows, with the same per-entry contract (a dangling or damaged entry is
	// skipped; an I/O failure is not).
	pub(crate) fn read_dir_page_inode(&mut self, dir_num: u32, after: Option<&[u8]>, max: usize) -> Result<Vec<(Vec<u8>, u64, bool, u64, u64)>, FsError> {
		let dir = self.read_inode(dir_num)?;
		if dir.r#type != TYPE_DIR {
			return Err(FsError::NotDir);
		}
		// A PAGE OF `max` HEALTHY ENTRIES, not a page of `max` records some of which survive.
		//
		// `collect_dir_page` stops after `max` RAW records and the loop below then drops the ones
		// whose inode will not read. So a run of `max` dangling or damaged records produced an EMPTY
		// page - and the public contract says an empty page ends the enumeration, with no cursor
		// left to reach the healthy entries behind them. A paginated backup, sync or repair walk
		// would silently omit everything after the first damaged run.
		//
		// Refilling from the last raw name keeps the cursor moving through the damage instead of
		// stopping in it. Bounded: each round advances past at least one record, and the walk ends
		// when a round returns nothing, which is the tree really being exhausted.
		let mut out = Vec::new();
		let mut cursor: Option<Vec<u8>> = after.map(|a| a.to_vec());
		while out.len() < max {
			let mut page = Vec::new();
			let want = max - out.len();
			self.collect_dir_page(dir.dir_root, dir.dir_root_crc, cursor.as_deref(), want, &mut page)?;
			if page.is_empty() {
				break;
			}
			cursor = page.last().map(|(name, _): &(Vec<u8>, u32)| name.clone());
			for (name, inode_num) in page {
				match self.read_inode(inode_num) {
					Ok(inode) => out.push((name, inode.size, inode.r#type == TYPE_DIR, inode.mtime, inode.ctime)),
					Err(FsError::Io) => return Err(FsError::Io),
					// Dropped from the page and stepped over by the cursor above, so the entries
					// behind them stay reachable.
					Err(FsError::Invalid | FsError::Corrupt) => {}
					Err(e) => return Err(e),
				}
			}
		}
		Ok(out)
	}

	// directory B+tree operations over variable-length leaf records. Internal nodes
	// route by the u64 name hash exactly like every other tree (the shared absorb and
	// collapse helpers apply); leaves hold DirRec records sorted by (hash, name) and are
	// rewritten compactly on every change.

	// Find `name`'s child inode in the tree rooted at (`root`, `root_crc`).
	pub(crate) fn dir_tree_lookup(&mut self, root: u64, root_crc: u32, name: &[u8]) -> Result<Option<u32>, FsError> {
		if root == 0 {
			return Ok(None);
		}
		let hash = name_hash(name);
		let mut ptr = root;
		let mut crc = root_crc;
		let mut buf = try_zeroed(BLOCK_SIZE)?;
		// bounded descent, like every tree walk: a longer path is a hostile shape.
		for _ in 0..TREE_DEPTH_MAX {
			self.read_node(ptr, crc, &mut buf)?;
			if node_type(&buf) == NODE_LEAF {
				let recs = dir_leaf_parse(&buf);
				return Ok(match dir_recs_search(&recs, hash, name) {
					Ok(pos) => Some(recs[pos].child),
					Err(_) => None,
				});
			}
			let count = internal_count(&buf);
			let ci = route_child(&buf, count, hash);
			ptr = child_ptr(&buf, ci);
			crc = child_crc(&buf, ci);
		}
		Err(FsError::Corrupt)
	}

	// Insert or repoint `name` -> `child`; returns the tree's new root.
	pub(crate) fn dir_tree_insert(&mut self, root: u64, root_crc: u32, name: &[u8], child: u32) -> Result<(u64, u32), FsError> {
		if root == 0 {
			let blk = self.alloc_meta()?;
			let mut buf = try_zeroed(BLOCK_SIZE)?;
			dir_leaf_write(&mut buf, &[DirRec { hash: name_hash(name), name: name.to_vec(), child }]);
			let crc = self.write_node_to(blk, &buf)?;
			return Ok((blk, crc));
		}
		let outcome = self.dir_insert_node(root, root_crc, name, child, TREE_DEPTH_MAX)?;
		self.settle_root(outcome)
	}

	// A directory leaf is what it CLAIMS to be, or it is not one to edit.
	//
	// `dir_leaf_parse` is deliberately tolerant: it stops at a record it cannot complete and returns
	// what it managed, so a damaged directory can still be listed and rescued. That is right for a
	// read and wrong for a write. The structural pass calls a leaf holding fewer records than its
	// header claims a truncated leaf and reports it - and it only runs when someone asks for `fsck`,
	// so an ordinary writable mount would go on inserting into a list already out of order,
	// rewriting the leaf compactly and making the damage permanent and consistent.
	//
	// Checked where the mutation happens rather than at mount: a whole structural pass at mount
	// costs every boot, and this costs a pass over a block that has just been read anyway.
	//
	// This checked the record COUNT and nothing else, which is one of the four invariants a single
	// leaf can be judged against - and not the one the write path actually depends on. See
	// `validate_dir_leaf`.
	fn leaf_is_whole(buf: &[u8], recs: &[DirRec]) -> Result<(), FsError> {
		validate_dir_leaf(buf, recs).map_err(|_| FsError::Corrupt)
	}

	pub(crate) fn dir_insert_node(&mut self, ptr: u64, crc: u32, name: &[u8], child: u32, depth: usize) -> Result<Ins, FsError> {
		// bounded like the shared insert recursion: a deeper path is a hostile shape.
		if depth == 0 {
			return Err(FsError::Corrupt);
		}
		let hash = name_hash(name);
		let mut buf = try_zeroed(BLOCK_SIZE)?;
		self.read_node(ptr, crc, &mut buf)?;
		if node_type(&buf) == NODE_LEAF {
			let mut recs = dir_leaf_parse(&buf);
			Self::leaf_is_whole(&buf, &recs)?;
			match dir_recs_search(&recs, hash, name) {
				Ok(pos) => recs[pos].child = child,
				Err(pos) => recs.insert(pos, DirRec { hash, name: name.to_vec(), child }),
			}
			if dir_leaf_size(&recs) <= BLOCK_SIZE {
				let dest = self.node_dest(ptr)?;
				dir_leaf_write(&mut buf, &recs);
				let ncrc = self.write_node_to(dest, &buf)?;
				return Ok(Ins::Updated(dest, ncrc));
			}
			// overfull: split at a hash boundary near the byte midpoint (records sharing
			// a hash must stay in one leaf, since internal nodes route by hash alone).
			// When no such boundary exists - every record in the leaf shares one hash -
			// there is no split this format can represent, and the fallback used to cut
			// the group at index 1 anyway. Routing then reached one of the two leaves and
			// every name in the other became unfindable while still occupying the tree.
			//
			// Refusing the insert is the honest answer until the format grows either
			// full-key routing in the separators or a collision chain: nothing is lost,
			// nothing becomes invisible, and the caller is told the directory is full.
			// Getting here at all takes enough names colliding in a 64-bit hash to
			// overflow a whole leaf, which is a construction rather than an accident.
			let Some(split) = dir_split_point(&recs) else {
				return Err(FsError::NoSpace);
			};
			let left_dest = self.node_dest(ptr)?;
			let right_dest = self.alloc_meta()?;
			let mut lbuf = try_zeroed(BLOCK_SIZE)?;
			dir_leaf_write(&mut lbuf, &recs[..split]);
			let mut rbuf = try_zeroed(BLOCK_SIZE)?;
			dir_leaf_write(&mut rbuf, &recs[split..]);
			let lcrc = self.write_node_to(left_dest, &lbuf)?;
			let rcrc = self.write_node_to(right_dest, &rbuf)?;
			return Ok(Ins::Split(left_dest, lcrc, recs[split].hash, right_dest, rcrc));
		}
		// The descent trusts the separators to route and the child links to point somewhere; a node
		// that fails either is one this insert must not walk through, let alone rebuild on the way
		// back up.
		validate_internal(&buf).map_err(|_| FsError::Corrupt)?;
		let count = internal_count(&buf);
		let ci = route_child(&buf, count, hash);
		let cp = child_ptr(&buf, ci);
		let cc = child_crc(&buf, ci);
		let outcome = self.dir_insert_node(cp, cc, name, child, depth - 1)?;
		self.internal_absorb(&mut buf, ptr, ci, outcome)
	}

	// Delete `name`; returns the tree's new root and whether a record was removed.
	pub(crate) fn dir_tree_delete(&mut self, root: u64, root_crc: u32, name: &[u8]) -> Result<(u64, u32, bool), FsError> {
		if root == 0 {
			return Ok((0, 0, false));
		}
		match self.dir_delete_node(root, root_crc, name, TREE_DEPTH_MAX)? {
			Del::NotFound => Ok((root, root_crc, false)),
			Del::Empty => Ok((0, 0, true)),
			Del::Updated(p, c) => {
				let (ptr, crc) = self.collapse_root(p, c)?;
				Ok((ptr, crc, true))
			}
		}
	}

	pub(crate) fn dir_delete_node(&mut self, ptr: u64, crc: u32, name: &[u8], depth: usize) -> Result<Del, FsError> {
		// bounded like the shared delete recursion: a deeper path is a hostile shape.
		if depth == 0 {
			return Err(FsError::Corrupt);
		}
		let hash = name_hash(name);
		let mut buf = try_zeroed(BLOCK_SIZE)?;
		self.read_node(ptr, crc, &mut buf)?;
		if node_type(&buf) == NODE_LEAF {
			let mut recs = dir_leaf_parse(&buf);
			Self::leaf_is_whole(&buf, &recs)?;
			let pos = match dir_recs_search(&recs, hash, name) {
				Ok(pos) => pos,
				Err(_) => return Ok(Del::NotFound),
			};
			if recs.len() == 1 {
				// the leaf empties: the parent drops it.
				self.drop_block(ptr);
				return Ok(Del::Empty);
			}
			recs.remove(pos);
			let dest = self.node_dest(ptr)?;
			dir_leaf_write(&mut buf, &recs);
			let ncrc = self.write_node_to(dest, &buf)?;
			return Ok(Del::Updated(dest, ncrc));
		}
		validate_internal(&buf).map_err(|_| FsError::Corrupt)?;
		let count = internal_count(&buf);
		let ci = route_child(&buf, count, hash);
		let cp = child_ptr(&buf, ci);
		let cc = child_crc(&buf, ci);
		let outcome = self.dir_delete_node(cp, cc, name, depth - 1)?;
		self.internal_absorb_del(&mut buf, ptr, ci, outcome)
	}

	// List directory `dir_num` as (name, size, is_dir, mtime, ctime) tuples. An entry
	// whose inode cannot be read - dangling (Invalid) or damaged (Corrupt / Io) - is
	// skipped, so one bad entry never unlists the healthy rest; fsck names the damage
	// and `remove` clears it.
	pub(crate) fn read_dir_inode(&mut self, dir_num: u32) -> Result<Vec<(Vec<u8>, u64, bool, u64, u64)>, FsError> {
		let mut out = Vec::new();
		for (name, inode_num) in self.dir_entries_of(dir_num)? {
			match self.read_inode(inode_num) {
				Ok(inode) => out.push((name, inode.size, inode.r#type == TYPE_DIR, inode.mtime, inode.ctime)),
				// a dangling or damaged entry is skipped, by decision: a listing must not
				// be stopped by one bad record on a volume the operator is trying to
				// rescue, and `fsck` is what names those.
				//
				// An I/O failure is NOT that. The disk did not answer, so nothing is known
				// about the entry - and a listing that quietly omits it says the file is
				// gone, which a backup or sync tool downstream reads as a deletion to
				// propagate. One transient read then deletes the file at the other end.
				Err(FsError::Io) => return Err(FsError::Io),
				Err(FsError::Invalid | FsError::Corrupt) => {}
				Err(e) => return Err(e),
			}
		}
		Ok(out)
	}

	// Does the subtree rooted at directory `root_dir` contain inode `target` (as the
	// directory itself or any descendant)? Used to reject moving a directory into
	// itself. Iterative (a work list of directories), so nesting depth never grows the
	// call stack; a visited set makes a hostile namespace (a cycle, or many names
	// aliasing one subtree) terminate instead of looping or blowing up. A child whose
	// inode cannot be read propagates its error - stricter than the skip-the-bad-child
	// listing contract, by DECISION: this walk guards rename against creating a
	// namespace cycle, and an unverifiable child could be the very directory being
	// moved into - refusing the move is the safe side, and the operator's repair verbs
	// (fsck names the damage, remove clears it) unblock the rename.
	pub(crate) fn subtree_contains(&mut self, root_dir: u32, target: u32) -> Result<bool, FsError> {
		let mut dirs: Vec<u32> = vec![root_dir];
		let mut seen: BTreeSet<u32> = BTreeSet::new();
		while let Some(dir) = dirs.pop() {
			if dir == target {
				return Ok(true);
			}
			if !seen.insert(dir) {
				continue;
			}
			for (_, child) in self.dir_entries_of(dir)? {
				if self.read_inode(child)?.r#type == TYPE_DIR {
					dirs.push(child);
				}
			}
		}
		Ok(false)
	}

	// Drop the file's blocks from logical block `keep` to the end: runs wholly past the
	// cut are removed, a run straddling it is shortened. Under copy-on-write nothing is
	// freed immediately - the dropped data, checksum, and overflow blocks stop being
	// referenced by the new generation (recorded on the dead list, freed the commit
	// after next; until then the previous generation still pins them as a snapshot). A
	// shortened raw run keeps its checksum block (its leading slots still match the
	// kept blocks) and drops only the cut tail's data blocks; a shortened compressed
	// run keeps everything, since decoding needs the whole stored stream.
	pub(crate) fn free_from(&mut self, inode: &mut Inode, keep: u64) -> Result<(), FsError> {
		let mut kept: Vec<Extent> = Vec::new();
		let extents = core::mem::take(&mut inode.extents);
		for ext in extents {
			if ext.logical >= keep {
				// wholly cut: its stored blocks and checksum block leave the new
				// generation.
				for off in 0..ext.store_len as u64 {
					self.drop_block(ext.stored(off));
				}
				self.drop_block(ext.csum);
				continue;
			}
			if ext.end() <= keep {
				kept.push(ext);
				continue;
			}
			let mut e = ext;
			e.length = (keep - ext.logical) as u32;
			if ext.clen == 0 {
				// a raw run drops the cut tail's data blocks; the checksum block stays
				// (shared with the kept prefix).
				for off in e.length as u64..ext.length as u64 {
					self.drop_block(ext.stored(off));
				}
				e.store_len = e.length;
			}
			kept.push(e);
		}
		inode.extents = kept;
		Ok(())
	}

	// Set the bitmap bit for every block an inode references: each run's stored (data or
	// compressed) blocks and its checksum block, plus the blocks of the extent overflow
	// chain.
	pub(crate) fn collect_inode_blocks(&mut self, inode: &Inode, bitmap: &mut [u8]) -> Result<(), FsError> {
		for i in 0..inode.extents.len() {
			let ext = &inode.extents[i];
			// The free map is what the allocator hands blocks out from, so an extent whose
			// fields contradict each other may not be marked from: the loop below walks
			// `store_len`, the read path serves `length`, and where those disagree the
			// difference is a block one file reads and the allocator believes is free.
			// Refusing here reaches the caller as walk damage, which degrades the mount to
			// read-only - the same treatment the tree already gives a node it cannot read,
			// and for the same reason: nothing may be handed out on the strength of a map
			// derived from metadata that cannot be true.
			self.check_extent(ext)?;
			for off in 0..ext.store_len as u64 {
				self.mark(bitmap, ext.stored(off));
			}
			// UNCONDITIONAL, now that `check_extent` above requires the pointer to be inside the
			// pool. The `!= 0` skip was what let an extent naming block 0 or 1 as its checksum
			// escape the duplicate-owner detector entirely: the live bitmap starts with those two
			// reserved, so the reference was never compared against anything and a forged image
			// stayed writable. With the pointer validated, there is no absent case left to skip.
			self.mark(bitmap, ext.csum);
		}
		self.walk_chain(inode.spill, |fs, ptr| {
			fs.mark(bitmap, ptr);
		})
	}
}

// One in-memory directory entry: the name's FNV-1a hash (the routing key), the name,
// and the child inode. Leaves hold these sorted by (hash, name).
pub(crate) struct DirRec {
	pub(crate) hash: u64,
	pub(crate) name: Vec<u8>,
	pub(crate) child: u32,
}

// Parse a directory leaf's variable-length records: count in the node header, then
// [hash u64][child u32][len u8][name] each, back to back. The loop is bounds-checked
// and the count clamped to what the block can hold, so an insane header yields what
// parsed cleanly rather than a panic or an absurd allocation.
pub(crate) fn dir_leaf_parse(buf: &[u8]) -> Vec<DirRec> {
	let count = node_count(buf).min((BLOCK_SIZE - NODE_HDR) / DIR_REC_HDR);
	let mut recs = Vec::with_capacity(count);
	let mut off = NODE_HDR;
	for _ in 0..count {
		if off + DIR_REC_HDR > buf.len() {
			break;
		}
		let hash = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
		let child = u32::from_le_bytes(buf[off + 8..off + 12].try_into().unwrap());
		let len = buf[off + 12] as usize;
		if off + DIR_REC_HDR + len > buf.len() {
			break;
		}
		let name = buf[off + DIR_REC_HDR..off + DIR_REC_HDR + len].to_vec();
		recs.push(DirRec { hash, name, child });
		off += DIR_REC_HDR + len;
	}
	recs
}

// The serialized byte size of a leaf holding `recs`.
pub(crate) fn dir_leaf_size(recs: &[DirRec]) -> usize {
	NODE_HDR + recs.iter().map(|r| DIR_REC_HDR + r.name.len()).sum::<usize>()
}

// Serialize `recs` (sorted) into a leaf block, zero-padding the tail.
pub(crate) fn dir_leaf_write(buf: &mut [u8], recs: &[DirRec]) {
	buf.fill(0);
	node_set_header(buf, NODE_LEAF, recs.len());
	let mut off = NODE_HDR;
	for r in recs {
		buf[off..off + 8].copy_from_slice(&r.hash.to_le_bytes());
		buf[off + 8..off + 12].copy_from_slice(&r.child.to_le_bytes());
		buf[off + 12] = r.name.len() as u8;
		buf[off + DIR_REC_HDR..off + DIR_REC_HDR + r.name.len()].copy_from_slice(&r.name);
		off += DIR_REC_HDR + r.name.len();
	}
}

// Binary-search `recs` (sorted by (hash, name)) for the entry named `name`.
// Everything one directory leaf can be judged on without walking the tree.
//
// `dir_recs_search` binary-searches these records, so ORDER is not a nicety here: over an unsorted
// leaf a binary search answers arbitrarily, and the caller acts on that answer. The other three
// come with it at no extra cost, and they are the same rules the structural pass applies - shared
// rather than restated, so the two cannot drift apart the way they had.
//
// Strictly ascending also rules out a duplicate name, which is why there is no separate check for
// one: two equal records cannot both be strictly greater than the record before them.
pub(crate) fn validate_dir_leaf(buf: &[u8], recs: &[DirRec]) -> Result<(), NodeFault> {
	// Truncation first, and it is the only count check a directory leaf needs. Records here are
	// variable-width, so there is no fixed capacity to compare a header against - and a count above
	// what the block could possibly hold shows up as a parse that stopped early, which is the more
	// specific answer anyway: it says how many were actually there.
	if recs.len() != node_count(buf) {
		return Err(NodeFault::Truncated { held: recs.len(), claimed: node_count(buf) });
	}
	let mut last: Option<(u64, &[u8])> = None;
	for rec in recs {
		if name_hash(&rec.name) != rec.hash {
			return Err(NodeFault::StoredHashMismatch);
		}
		// The SAME rule the path API enforces when it creates a name: a record the API could never
		// have written is a record no path can address afterwards.
		if validate_name_segment(&rec.name).is_err() {
			return Err(NodeFault::NameNotAddressable);
		}
		if let Some((hash, name)) = last {
			if (rec.hash, rec.name.as_slice()) <= (hash, name) {
				return Err(NodeFault::OutOfOrder);
			}
		}
		last = Some((rec.hash, rec.name.as_slice()));
	}
	Ok(())
}

pub(crate) fn dir_recs_search(recs: &[DirRec], hash: u64, name: &[u8]) -> Result<usize, usize> {
	recs.binary_search_by(|r| match r.hash.cmp(&hash) {
		Ordering::Equal => r.name.as_slice().cmp(name),
		other => other,
	})
}

// Where to split an overfull leaf's records: the record index nearest the byte
// midpoint, nudged so two records sharing a hash never straddle the split (the parent
// routes by hash alone). Mirrors the fixed-record `leaf_split_point`.
// The index to split an overfull leaf at, or None when every record shares one hash and
// no split keeps the group whole.
pub(crate) fn dir_split_point(recs: &[DirRec]) -> Option<usize> {
	let total = dir_leaf_size(recs) - NODE_HDR;
	let mut acc = 0usize;
	let mut mid = recs.len() / 2;
	for (i, r) in recs.iter().enumerate() {
		acc += DIR_REC_HDR + r.name.len();
		if acc * 2 >= total {
			mid = (i + 1).min(recs.len() - 1);
			break;
		}
	}
	let mut up = mid.max(1);
	while up < recs.len() && recs[up].hash == recs[up - 1].hash {
		up += 1;
	}
	if up < recs.len() {
		return Some(up);
	}
	let mut down = mid;
	while down > 1 && recs[down].hash == recs[down - 1].hash {
		down -= 1;
	}
	// `down` stopped at 1 either because it found a boundary there or because it ran out
	// of room to keep looking. Only the first is a split; the second would cut the group.
	if down >= 1 && recs[down].hash != recs[down - 1].hash {
		return Some(down);
	}
	None
}

// The name held in a directory record's NUL-padded name field: up to the first NUL.
pub(crate) fn name_in(field: &[u8]) -> &[u8] {
	match field.iter().position(|&b| b == 0) {
		Some(end) => &field[..end],
		None => field,
	}
}

// FNV-1a 64-bit hash of an entry name: the B+tree key that orders a directory's entries.
pub(crate) fn name_hash(name: &[u8]) -> u64 {
	let mut h: u64 = 0xcbf2_9ce4_8422_2325;
	for &b in name {
		h ^= b as u64;
		h = h.wrapping_mul(0x0000_0100_0000_01b3);
	}
	h
}

// Split a path into its validated segments. Each segment must be non-empty, no longer
// than NAME_MAX, neither "." nor "..", and free of NUL bytes - so a resolved path can
// never escape the volume or name an invalid entry. Names must be valid UTF-8, so one
// file has one name (no byte-soup aliases a rendering cannot distinguish); a
// portable-name policy is enforced on top: the cross-platform-unsafe set
// (`\ : * ? < > | "` and control bytes) is rejected beyond `/` and NUL. That is the BYTE SET
// those media share; whether a name is portable to them is a wider question, and
// `fscore::is_portable_name` is where it is asked.
pub(crate) fn split_segments(path: &[u8]) -> Result<Vec<&[u8]>, FsError> {
	if path.is_empty() {
		return Err(FsError::BadName);
	}
	// Bounded BEFORE the walk, because the walk is what costs: a path is refused for being longer
	// than any path this filesystem can hold, rather than parsed and then found to be too deep.
	if path.len() > PATH_MAX {
		return Err(FsError::TooLong);
	}
	let mut segs = Vec::new();
	// Fallibly, and at a size the depth limit bounds. `Vec::push` aborts the process when memory is
	// short, which turns a hostile path into a crash in a crate whose every other allocation is
	// fallible.
	if segs.try_reserve(path.split(|&b| b == b'/').count().min(PATH_DEPTH_MAX)).is_err() {
		return Err(FsError::NoMemory);
	}
	// A LEADING OR TRAILING SEPARATOR IS NORMALISED AWAY, which is the Storage ABI's rule stated in
	// `storage.lsidl` and now implemented by both backends rather than answered differently by each.
	//
	// This passed EVERY segment to the validator, so `/a/b` was `BadName` here and an ordinary path
	// on LiberMemFS - one spelling of one path that resolved or did not depending on which
	// filesystem was mounted. A MIDDLE empty segment is still refused, because `a//b` genuinely has
	// two readings and that is the disagreement worth removing.
	let trimmed = {
		let front = path.strip_prefix(b"/").unwrap_or(path);
		let back = front.strip_suffix(b"/").unwrap_or(front);
		back
	};
	if trimmed.is_empty() {
		return Err(FsError::BadName);
	}
	for seg in trimmed.split(|&b| b == b'/') {
		if segs.len() == PATH_DEPTH_MAX {
			return Err(FsError::TooLong);
		}
		validate_name_segment(seg)?;
		segs.push(seg);
	}
	Ok(segs)
}

// PORTABILITY IS A SEPARATE QUESTION, and `fscore::is_portable_name` answers it.
//
// The comment below used to claim that this validator's byte set "makes names move cleanly onto
// FAT and NTFS media and other systems", and it does not: `CON`, `NUL`, `COM1` resolve to hardware
// there rather than to files, and a trailing dot or space is silently stripped, so two distinct
// names here become one there and the second write destroys the first. Case folding and Unicode
// normalisation are not covered by either and cannot be by a rule of this shape - `Foo` and `foo`
// are two names here and one on a case-insensitive volume, which is a property of the destination.
//
// The filesystem is NOT tightened for it. A checker that refuses names a medium legitimately
// carries is worse than one that accepts them, and a volume written elsewhere must stay readable.
// What exists now is a question a caller can ask before it copies a tree.
//
// Is `seg` a name this filesystem can address? The ONE answer, used both by
// `split_segments` (the write side, deciding what may be created) and by the structural
// pass (the read side, deciding what a record found on the medium may claim to be).
//
// They were two answers. The path API refused invalid UTF-8, empty segments, `.` and
// `..`, control characters, the reserved punctuation set, NUL and over-long names; the
// directory checker knew two of those eight. So a record named `..`, or `a/b`, or one
// carrying a control byte, passed an `fsck` that reported the directory clean - and no
// path the API can build ever reaches it afterwards, because the API refuses to spell it.
// A checker weaker than the writer cannot notice an image the writer could not have
// produced, which is most of what a checker is for.
//
// `/` is in the set even though `split_segments` can never hand it one (it splits on it):
// a record READ off the medium can carry it, and a name with a separator inside it
// resolves to a path that names something else entirely.
pub(crate) fn validate_name_segment(seg: &[u8]) -> Result<(), FsError> {
	fscore::validate_name_segment(seg, NAME_MAX)
}
