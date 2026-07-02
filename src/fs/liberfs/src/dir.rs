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
				if self.read_inode(child)?.kind != KIND_DIR {
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
			if self.read_inode(child)?.kind != KIND_DIR {
				return Err(FsError::NotDir);
			}
			return Ok(child);
		}
		let num = self.alloc_inode()?;
		let mut dir = Inode::empty(KIND_DIR);
		dir.ctime = self.clock;
		dir.mtime = self.clock;
		self.write_inode(num, &mut dir)?;
		self.dir_insert(parent, name, num)?;
		Ok(num)
	}

	// directory operations (on any directory inode)

	// Look up `name` in directory `dir_num` through its B+tree: the child inode, or None
	// if absent. Errors if `dir_num` is not a directory. A hit populates the bounded
	// dentry cache, so path resolution stops re-walking the tree for hot names.
	pub(crate) fn dir_lookup(&mut self, dir_num: u32, name: &[u8]) -> Result<Option<u32>, FsError> {
		if let Some(child) = self.dcache.get(&(dir_num, name.to_vec())) {
			return Ok(Some(*child));
		}
		let dir = self.read_inode(dir_num)?;
		if dir.kind != KIND_DIR {
			return Err(FsError::NotFound);
		}
		match self.dir_tree_lookup(dir.dir_root, dir.dir_root_crc, name)? {
			Some(child) => {
				self.dcache_put(dir_num, name, child);
				Ok(Some(child))
			}
			None => Ok(None),
		}
	}

	// Remember (directory, name) -> child, evicting an arbitrary entry once the cache
	// is full (plain bounded eviction; the cache only skips re-reads).
	pub(crate) fn dcache_put(&mut self, dir_num: u32, name: &[u8], child: u32) {
		if self.dcache.len() >= DCACHE_MAX {
			if let Some(k) = self.dcache.keys().next().cloned() {
				self.dcache.remove(&k);
			}
		}
		self.dcache.insert((dir_num, name.to_vec()), child);
	}

	// Insert entry `name` -> `child` into directory `dir_num`, or repoint it if it is
	// already there. The directory's B+tree root (and the entry count it stores in
	// `size`) are updated and the directory inode rewritten.
	pub(crate) fn dir_insert(&mut self, dir_num: u32, name: &[u8], child: u32) -> Result<(), FsError> {
		let mut dir = self.read_inode(dir_num)?;
		if dir.kind != KIND_DIR {
			return Err(FsError::NotFound);
		}
		let existed = self.dir_tree_lookup(dir.dir_root, dir.dir_root_crc, name)?.is_some();
		let (root, crc) = self.dir_tree_insert(dir.dir_root, dir.dir_root_crc, name, child)?;
		dir.dir_root = root;
		dir.dir_root_crc = crc;
		if !existed {
			dir.size += 1;
		}
		dir.mtime = self.clock;
		self.write_inode(dir_num, &mut dir)?;
		self.dcache_put(dir_num, name, child);
		Ok(())
	}

	// Remove entry `name` from directory `dir_num`. NotFound if it is not there.
	pub(crate) fn dir_remove(&mut self, dir_num: u32, name: &[u8]) -> Result<(), FsError> {
		let mut dir = self.read_inode(dir_num)?;
		if dir.kind != KIND_DIR {
			return Err(FsError::NotFound);
		}
		let (root, crc, removed) = self.dir_tree_delete(dir.dir_root, dir.dir_root_crc, name)?;
		if !removed {
			return Err(FsError::NotFound);
		}
		self.dcache.remove(&(dir_num, name.to_vec()));
		dir.dir_root = root;
		dir.dir_root_crc = crc;
		dir.size = dir.size.saturating_sub(1);
		dir.mtime = self.clock;
		self.write_inode(dir_num, &mut dir)?;
		Ok(())
	}

	// Collect every (name, inode) entry in directory `dir_num`, in key order.
	pub(crate) fn dir_entries_of(&mut self, dir_num: u32) -> Result<Vec<(Vec<u8>, u32)>, FsError> {
		let dir = self.read_inode(dir_num)?;
		let mut out = Vec::new();
		self.collect_dir_entries(dir.dir_root, dir.dir_root_crc, &mut out)?;
		Ok(out)
	}

	// Walk the directory B+tree rooted at (`ptr`, `crc`), appending each leaf's entries.
	pub(crate) fn collect_dir_entries(&mut self, ptr: u64, crc: u32, out: &mut Vec<(Vec<u8>, u32)>) -> Result<(), FsError> {
		if ptr == 0 {
			return Ok(());
		}
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node(ptr, crc, &mut buf)?;
		if node_type(&buf) == NODE_LEAF {
			for rec in dir_leaf_parse(&buf) {
				out.push((rec.name, rec.child));
			}
		} else {
			let count = node_count(&buf);
			for i in 0..=count {
				let cp = child_ptr(&buf, i);
				let cc = child_crc(&buf, i);
				self.collect_dir_entries(cp, cc, out)?;
			}
		}
		Ok(())
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
		let mut buf = vec![0u8; BLOCK_SIZE];
		loop {
			self.read_node(ptr, crc, &mut buf)?;
			if node_type(&buf) == NODE_LEAF {
				let recs = dir_leaf_parse(&buf);
				return Ok(match dir_recs_search(&recs, hash, name) {
					Ok(pos) => Some(recs[pos].child),
					Err(_) => None,
				});
			}
			let count = node_count(&buf);
			let mut ci = 0;
			while ci < count && sep_key(&buf, ci) <= hash {
				ci += 1;
			}
			ptr = child_ptr(&buf, ci);
			crc = child_crc(&buf, ci);
		}
	}

	// Insert or repoint `name` -> `child`; returns the tree's new root.
	pub(crate) fn dir_tree_insert(&mut self, root: u64, root_crc: u32, name: &[u8], child: u32) -> Result<(u64, u32), FsError> {
		if root == 0 {
			let blk = self.alloc_meta()?;
			let mut buf = vec![0u8; BLOCK_SIZE];
			dir_leaf_write(&mut buf, &[DirRec { hash: name_hash(name), name: name.to_vec(), child }]);
			let crc = self.write_node_to(blk, &buf)?;
			return Ok((blk, crc));
		}
		let outcome = self.dir_insert_node(root, root_crc, name, child)?;
		self.settle_root(outcome)
	}

	pub(crate) fn dir_insert_node(&mut self, ptr: u64, crc: u32, name: &[u8], child: u32) -> Result<Ins, FsError> {
		let hash = name_hash(name);
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node(ptr, crc, &mut buf)?;
		if node_type(&buf) == NODE_LEAF {
			let mut recs = dir_leaf_parse(&buf);
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
			let split = dir_split_point(&recs);
			let left_dest = self.node_dest(ptr)?;
			let right_dest = self.alloc_meta()?;
			let mut lbuf = vec![0u8; BLOCK_SIZE];
			dir_leaf_write(&mut lbuf, &recs[..split]);
			let mut rbuf = vec![0u8; BLOCK_SIZE];
			dir_leaf_write(&mut rbuf, &recs[split..]);
			let lcrc = self.write_node_to(left_dest, &lbuf)?;
			let rcrc = self.write_node_to(right_dest, &rbuf)?;
			return Ok(Ins::Split(left_dest, lcrc, recs[split].hash, right_dest, rcrc));
		}
		let count = node_count(&buf);
		let mut ci = 0;
		while ci < count && sep_key(&buf, ci) <= hash {
			ci += 1;
		}
		let cp = child_ptr(&buf, ci);
		let cc = child_crc(&buf, ci);
		let outcome = self.dir_insert_node(cp, cc, name, child)?;
		self.internal_absorb(&mut buf, ptr, ci, outcome)
	}

	// Delete `name`; returns the tree's new root and whether a record was removed.
	pub(crate) fn dir_tree_delete(&mut self, root: u64, root_crc: u32, name: &[u8]) -> Result<(u64, u32, bool), FsError> {
		if root == 0 {
			return Ok((0, 0, false));
		}
		match self.dir_delete_node(root, root_crc, name)? {
			Del::NotFound => Ok((root, root_crc, false)),
			Del::Empty => Ok((0, 0, true)),
			Del::Updated(p, c) => {
				let (ptr, crc) = self.collapse_root(p, c)?;
				Ok((ptr, crc, true))
			}
		}
	}

	pub(crate) fn dir_delete_node(&mut self, ptr: u64, crc: u32, name: &[u8]) -> Result<Del, FsError> {
		let hash = name_hash(name);
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node(ptr, crc, &mut buf)?;
		if node_type(&buf) == NODE_LEAF {
			let mut recs = dir_leaf_parse(&buf);
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
		let count = node_count(&buf);
		let mut ci = 0;
		while ci < count && sep_key(&buf, ci) <= hash {
			ci += 1;
		}
		let cp = child_ptr(&buf, ci);
		let cc = child_crc(&buf, ci);
		let outcome = self.dir_delete_node(cp, cc, name)?;
		self.internal_absorb_del(&mut buf, ptr, ci, outcome)
	}

	// List directory `dir_num` as (name, size, is_dir) triples.
	pub(crate) fn read_dir_inode(&mut self, dir_num: u32) -> Result<Vec<(Vec<u8>, u64, bool)>, FsError> {
		let mut out = Vec::new();
		for (name, inode_num) in self.dir_entries_of(dir_num)? {
			let inode = self.read_inode(inode_num)?;
			out.push((name, inode.size, inode.kind == KIND_DIR));
		}
		Ok(out)
	}

	// Does the subtree rooted at directory `root_dir` contain inode `target` (as the
	// directory itself or any descendant)? Used to reject moving a directory into
	// itself. Iterative (a work list of directories), so nesting depth never grows the
	// call stack.
	pub(crate) fn subtree_contains(&mut self, root_dir: u32, target: u32) -> Result<bool, FsError> {
		let mut dirs: Vec<u32> = vec![root_dir];
		while let Some(dir) = dirs.pop() {
			if dir == target {
				return Ok(true);
			}
			for (_, child) in self.dir_entries_of(dir)? {
				if self.read_inode(child)?.kind == KIND_DIR {
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
	pub(crate) fn free_from(&mut self, inode: &mut Inode, keep: usize) -> Result<(), FsError> {
		let keep = keep as u64;
		let mut kept: Vec<Extent> = Vec::new();
		let extents = core::mem::take(&mut inode.extents);
		for ext in extents {
			if ext.logical >= keep {
				// wholly cut: its stored blocks and checksum block leave the new
				// generation.
				for off in 0..ext.store_len as u64 {
					self.drop_block(ext.physical + off);
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
					self.drop_block(ext.physical + off);
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
		for ext in inode.extents.iter() {
			for off in 0..ext.store_len as u64 {
				set_bit(bitmap, ext.physical + off);
			}
			if ext.csum != 0 {
				set_bit(bitmap, ext.csum);
			}
		}
		let mut ptr = inode.spill;
		let mut buf = vec![0u8; BLOCK_SIZE];
		while ptr != 0 && ptr < self.num_blocks {
			set_bit(bitmap, ptr);
			if !self.dev.read_block(ptr, &mut buf) {
				return Err(FsError::Io);
			}
			ptr = u64::from_le_bytes(buf[0..8].try_into().unwrap());
		}
		Ok(())
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
// [hash u64][child u32][len u8][name] each, back to back.
pub(crate) fn dir_leaf_parse(buf: &[u8]) -> Vec<DirRec> {
	let count = node_count(buf);
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
	for b in buf.iter_mut() {
		*b = 0;
	}
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
pub(crate) fn dir_recs_search(recs: &[DirRec], hash: u64, name: &[u8]) -> Result<usize, usize> {
	recs.binary_search_by(|r| match r.hash.cmp(&hash) {
		Ordering::Equal => r.name.as_slice().cmp(name),
		other => other,
	})
}

// Where to split an overfull leaf's records: the record index nearest the byte
// midpoint, nudged so two records sharing a hash never straddle the split (the parent
// routes by hash alone). Mirrors the fixed-record `leaf_split_point`.
pub(crate) fn dir_split_point(recs: &[DirRec]) -> usize {
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
		return up;
	}
	let mut down = mid;
	while down > 1 && recs[down].hash == recs[down - 1].hash {
		down -= 1;
	}
	down
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
// (`\ : * ? < > | "` and control bytes) is rejected beyond `/` and NUL, so a LiberFS
// name moves cleanly to FAT / NTFS media and other systems.
pub(crate) fn split_segments(path: &[u8]) -> Result<Vec<&[u8]>, FsError> {
	if path.is_empty() {
		return Err(FsError::BadName);
	}
	let mut segs = Vec::new();
	for seg in path.split(|&b| b == b'/') {
		if seg.is_empty() || seg == b"." || seg == b".." {
			return Err(FsError::BadName);
		}
		if seg.len() > NAME_MAX {
			return Err(FsError::TooLong);
		}
		if core::str::from_utf8(seg).is_err() {
			return Err(FsError::BadName);
		}
		if seg.iter().any(|&c| !is_portable_name_byte(c)) {
			return Err(FsError::BadName);
		}
		segs.push(seg);
	}
	Ok(segs)
}

// Is byte `c` allowed in a portable file name? Rejects NUL and control bytes (0x00..=0x1F
// and 0x7F) and the cross-platform-reserved set `\ : * ? < > | "`. (`/` never reaches
// here - it is the path separator.)
pub(crate) fn is_portable_name_byte(c: u8) -> bool {
	if c < 0x20 || c == 0x7F {
		return false;
	}
	!matches!(c, b'\\' | b':' | b'*' | b'?' | b'<' | b'>' | b'|' | b'"')
}
