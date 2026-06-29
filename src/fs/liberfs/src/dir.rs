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
					return Err(FsError::Invalid);
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
				return Err(FsError::Invalid);
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
	// if absent. Errors if `dir_num` is not a directory.
	pub(crate) fn dir_lookup(&mut self, dir_num: u32, name: &[u8]) -> Result<Option<u32>, FsError> {
		let dir = self.read_inode(dir_num)?;
		if dir.kind != KIND_DIR {
			return Err(FsError::NotFound);
		}
		let probe = dir_probe(name);
		match self.tree_lookup(dir.dir_root, dir.dir_root_crc, name_hash(name), &probe, DIR_REC)? {
			Some(rec) => Ok(Some(u32::from_le_bytes(rec[8 + NAME_MAX..8 + NAME_MAX + 4].try_into().unwrap()))),
			None => Ok(None),
		}
	}

	// Insert entry `name` -> `child` into directory `dir_num`, or repoint it if it is
	// already there. The directory's B+tree root (and the entry count it stores in
	// `size`) are updated and the directory inode rewritten.
	pub(crate) fn dir_insert(&mut self, dir_num: u32, name: &[u8], child: u32) -> Result<(), FsError> {
		let mut dir = self.read_inode(dir_num)?;
		if dir.kind != KIND_DIR {
			return Err(FsError::NotFound);
		}
		let key = name_hash(name);
		let existed = {
			let probe = dir_probe(name);
			self.tree_lookup(dir.dir_root, dir.dir_root_crc, key, &probe, DIR_REC)?.is_some()
		};
		let record = dir_record(name, child);
		let (root, crc) = self.tree_insert(dir.dir_root, dir.dir_root_crc, key, &record, DIR_REC, DIR_LEAF_MAX, DIR_KEYLEN)?;
		dir.dir_root = root;
		dir.dir_root_crc = crc;
		if !existed {
			dir.size += 1;
		}
		dir.mtime = self.clock;
		self.write_inode(dir_num, &mut dir)?;
		Ok(())
	}

	// Remove entry `name` from directory `dir_num`. NotFound if it is not there.
	pub(crate) fn dir_remove(&mut self, dir_num: u32, name: &[u8]) -> Result<(), FsError> {
		let mut dir = self.read_inode(dir_num)?;
		if dir.kind != KIND_DIR {
			return Err(FsError::NotFound);
		}
		let probe = dir_probe(name);
		let (root, crc, removed) = self.tree_delete(dir.dir_root, dir.dir_root_crc, name_hash(name), &probe, DIR_REC, DIR_KEYLEN)?;
		if !removed {
			return Err(FsError::NotFound);
		}
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
		let count = node_count(&buf);
		if node_type(&buf) == NODE_LEAF {
			for i in 0..count {
				let off = NODE_HDR + i * DIR_REC;
				let name = name_in(&buf[off + 8..off + 8 + NAME_MAX]).to_vec();
				let inode = u32::from_le_bytes(buf[off + 8 + NAME_MAX..off + 8 + NAME_MAX + 4].try_into().unwrap());
				out.push((name, inode));
			}
		} else {
			for i in 0..=count {
				let cp = child_ptr(&buf, i);
				let cc = child_crc(&buf, i);
				self.collect_dir_entries(cp, cc, out)?;
			}
		}
		Ok(())
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
	// itself.
	pub(crate) fn subtree_contains(&mut self, root_dir: u32, target: u32) -> Result<bool, FsError> {
		if root_dir == target {
			return Ok(true);
		}
		for (_, child) in self.dir_entries_of(root_dir)? {
			if self.read_inode(child)?.kind == KIND_DIR && self.subtree_contains(child, target)? {
				return Ok(true);
			}
		}
		Ok(false)
	}

	// Drop the file's blocks from logical block `keep` to the end: runs wholly past the
	// cut are removed, a run straddling it is shortened. Under copy-on-write nothing is
	// marked free here - the dropped data, checksum, and overflow blocks simply stop
	// being referenced by the new generation and are reclaimed when the free map is
	// rederived at commit (until then the previous generation still pins them as a
	// snapshot). A shortened run keeps its checksum block; its leading slots still match
	// the kept blocks.
	pub(crate) fn free_from(&mut self, inode: &mut Inode, keep: usize) -> Result<(), FsError> {
		let keep = keep as u64;
		let mut kept: Vec<Extent> = Vec::new();
		for ext in inode.extents.iter() {
			if ext.logical >= keep {
				continue;
			}
			if ext.end() <= keep {
				kept.push(*ext);
				continue;
			}
			let mut e = *ext;
			e.length = (keep - ext.logical) as u32;
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
		while ptr != 0 {
			set_bit(bitmap, ptr);
			if !self.dev.read_block(ptr, &mut buf) {
				return Err(FsError::Io);
			}
			ptr = u64::from_le_bytes(buf[0..8].try_into().unwrap());
		}
		Ok(())
	}
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

// A directory probe key (the name hash then the NUL-padded name): the DIR_KEYLEN-byte
// prefix a leaf record is matched against.
pub(crate) fn dir_probe(name: &[u8]) -> Vec<u8> {
	let mut probe = vec![0u8; DIR_KEYLEN];
	probe[0..8].copy_from_slice(&name_hash(name).to_le_bytes());
	probe[8..8 + name.len()].copy_from_slice(name);
	probe
}

// A full directory leaf record: the (hash, NUL-padded name) key then the child inode.
pub(crate) fn dir_record(name: &[u8], child: u32) -> Vec<u8> {
	let mut rec = vec![0u8; DIR_REC];
	rec[0..8].copy_from_slice(&name_hash(name).to_le_bytes());
	rec[8..8 + name.len()].copy_from_slice(name);
	rec[8 + NAME_MAX..8 + NAME_MAX + 4].copy_from_slice(&child.to_le_bytes());
	rec
}

// Split a path into its validated segments. Each segment must be non-empty, no longer
// than NAME_MAX, neither "." nor "..", and free of NUL bytes - so a resolved path can
// never escape the volume or name an invalid entry. A portable-name policy is enforced
// at this boundary: the cross-platform-unsafe set (`\ : * ? < > | "` and control bytes)
// is rejected on top of `/` and NUL, so a LiberFS name moves cleanly to FAT / NTFS media
// and other systems.
pub(crate) fn split_segments(path: &[u8]) -> Result<Vec<&[u8]>, FsError> {
	if path.is_empty() {
		return Err(FsError::Invalid);
	}
	let mut segs = Vec::new();
	for seg in path.split(|&b| b == b'/') {
		if seg.is_empty() || seg == b"." || seg == b".." {
			return Err(FsError::Invalid);
		}
		if seg.len() > NAME_MAX {
			return Err(FsError::TooLong);
		}
		if seg.iter().any(|&c| !is_portable_name_byte(c)) {
			return Err(FsError::Invalid);
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
