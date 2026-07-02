use crate::*;

impl<D: BlockDevice> LiberFs<D> {
	// The pool size this filesystem was formatted with, in filesystem blocks (recorded
	// in the superblock; a volume never silently grows past it).
	pub fn num_blocks(&self) -> u64 {
		self.num_blocks
	}

	// Format `dev` as a fresh, empty LiberFS spanning `num_blocks` blocks (an empty root
	// directory, no files), then return it mounted. Generation 0 lays out the two
	// superblock slots and a single inode-tree leaf holding the root directory inode;
	// everything else is the free pool. Inodes and directory nodes are allocated on
	// demand thereafter, so a fresh volume reserves no fixed inode region.
	pub fn format(mut dev: D, num_blocks: u64) -> Result<LiberFs<D>, FsError> {
		// generation-0 layout: [slot 0][slot 1][inode-tree root leaf], then the free
		// pool. The root directory inode starts empty (no entries, no B+tree yet).
		if num_blocks <= POOL_START + 1 {
			return Err(FsError::Invalid);
		}
		let leaf_block: u64 = POOL_START;

		// the inode tree's sole leaf: one record keyed by inode 0 (the root directory).
		let mut leaf = vec![0u8; BLOCK_SIZE];
		node_set_header(&mut leaf, NODE_LEAF, 1);
		leaf[NODE_HDR..NODE_HDR + 8].copy_from_slice(&(ROOT_INODE as u64).to_le_bytes());
		Inode::empty(KIND_DIR).write(&mut leaf[NODE_HDR + 8..NODE_HDR + 8 + INODE_SIZE]);
		if !dev.write_block(leaf_block, &leaf) {
			return Err(FsError::Io);
		}
		let leaf_crc = crc32c(&leaf);

		// generation 0 in slot 0; slot 1 left invalid (zeroed) until the first commit
		// ping-pongs onto it.
		let zero = vec![0u8; BLOCK_SIZE];
		let sb = Superblock { num_blocks, generation: 0, inode_root: leaf_block, inode_root_crc: leaf_crc, next_inode: ROOT_INODE + 1, root_inode: ROOT_INODE, snap_root: 0, snap_root_crc: 0 };
		if !dev.write_block(0, &serialize_superblock(&sb)) {
			return Err(FsError::Io);
		}
		if !dev.write_block(1, &zero) {
			return Err(FsError::Io);
		}

		let mut fs = LiberFs { dev, num_blocks, root_inode: ROOT_INODE, generation: 0, slot: 0, inode_root: leaf_block, inode_root_crc: leaf_crc, next_inode: ROOT_INODE + 1, prev_inode_root: 0, prev_inode_root_crc: 0, prev_valid: false, snap_root: 0, snap_root_crc: 0, snapshots: Vec::new(), free: vec![0u8; (num_blocks as usize).div_ceil(8)], fresh: BTreeSet::new(), txn: None, decomp: None, clock: 0 };
		fs.derive_free()?;
		Ok(fs)
	}

	// Mount an existing LiberFS on `dev` at its newest committed generation. Returns None
	// if neither superblock slot is a valid LiberFS (an unformatted or foreign disk).
	pub fn mount(dev: D) -> Option<LiberFs<D>> {
		Self::mount_at(dev, true)
	}

	// Mount the previous generation read-only: the consistent snapshot of the
	// filesystem one commit ago. Returns None unless both superblock slots are valid (a
	// freshly formatted or single-generation volume has no older snapshot). The handle
	// is meant for reading; writing to it would interleave generations.
	pub fn mount_snapshot(dev: D) -> Option<LiberFs<D>> {
		Self::mount_at(dev, false)
	}

	// Mount a named snapshot read-only: the consistent, pinned state captured when the
	// snapshot was created. Returns None if the volume has no such snapshot. Like
	// `mount_snapshot`, the handle is meant for reading; the live free map (which already
	// reserves the snapshot's blocks) is reused unchanged.
	pub fn mount_named_snapshot(dev: D, name: &[u8]) -> Option<LiberFs<D>> {
		let mut fs = Self::mount(dev)?;
		let snap = fs.snapshots.iter().find(|s| s.name == name)?.clone();
		fs.inode_root = snap.inode_root;
		fs.inode_root_crc = snap.inode_root_crc;
		fs.generation = snap.generation;
		Some(fs)
	}

	pub(crate) fn mount_at(mut dev: D, newest: bool) -> Option<LiberFs<D>> {
		// read and validate both superblock slots.
		let mut buf = vec![0u8; BLOCK_SIZE];
		let mut slots: [Option<Superblock>; SUPER_SLOTS as usize] = [None, None];
		for s in 0..SUPER_SLOTS {
			if dev.read_block(s as u64, &mut buf) {
				slots[s as usize] = parse_superblock(&buf);
			}
		}
		// order the valid slots by generation: the higher is the live root, the lower
		// the snapshot.
		let mut valid: Vec<(u32, u64)> = (0..SUPER_SLOTS).filter_map(|s| slots[s as usize].map(|sb| (s, sb.generation))).collect();
		valid.sort_by_key(|&(_, g)| g);
		let (cur_slot, prev_slot) = if newest {
			let &(cur, _) = valid.last()?;
			let prev = valid.iter().rev().nth(1).map(|&(s, _)| s);
			(cur, prev)
		} else {
			// the snapshot: the lower generation, only if there are two.
			if valid.len() < 2 {
				return None;
			}
			(valid[0].0, None)
		};

		let sb = slots[cur_slot as usize]?;
		let (prev_inode_root, prev_inode_root_crc, prev_valid) = match prev_slot {
			Some(ps) => {
				let psb = slots[ps as usize]?;
				(psb.inode_root, psb.inode_root_crc, true)
			}
			None => (0, 0, false),
		};

		let mut fs = LiberFs { dev, num_blocks: sb.num_blocks, root_inode: sb.root_inode, generation: sb.generation, slot: cur_slot, inode_root: sb.inode_root, inode_root_crc: sb.inode_root_crc, next_inode: sb.next_inode, prev_inode_root, prev_inode_root_crc, prev_valid, snap_root: sb.snap_root, snap_root_crc: sb.snap_root_crc, snapshots: Vec::new(), free: vec![0u8; (sb.num_blocks as usize).div_ceil(8)], fresh: BTreeSet::new(), txn: None, decomp: None, clock: 0 };
		fs.load_snapshot_table().ok()?;
		fs.derive_free().ok()?;
		Some(fs)
	}

	// Resolve a path to its inode number, or None if any segment is missing.
	pub fn lookup(&mut self, path: &[u8]) -> Option<u32> {
		self.resolve(path).ok()
	}

	// Read the whole file at `path` into a freshly allocated buffer.
	pub fn read_file(&mut self, path: &[u8]) -> Result<Vec<u8>, FsError> {
		let inode_num = self.resolve(path)?;
		let inode = self.read_inode(inode_num)?;
		if inode.kind != KIND_FILE {
			return Err(FsError::NotFound);
		}
		let mut out = Vec::with_capacity(inode.size as usize);
		let mut block = vec![0u8; BLOCK_SIZE];
		let mut remaining = inode.size as usize;
		for i in 0..inode.nblocks() {
			// a hole (a sparse gap left by a write past the end) reads back as zeros;
			// a mapped block is verified against its stored checksum.
			if !self.read_logical(&inode, i, &mut block)? {
				for b in block.iter_mut() {
					*b = 0;
				}
			}
			let take = remaining.min(BLOCK_SIZE);
			out.extend_from_slice(&block[..take]);
			remaining -= take;
		}
		Ok(out)
	}

	// List the root directory as (name, size, is_dir) triples, one per live entry.
	pub fn list(&mut self) -> Result<Vec<(Vec<u8>, u64, bool)>, FsError> {
		self.read_dir_inode(self.root_inode)
	}

	// List the directory at `path` as (name, size, is_dir) triples.
	pub fn read_dir(&mut self, path: &[u8]) -> Result<Vec<(Vec<u8>, u64, bool)>, FsError> {
		let inode_num = self.resolve(path)?;
		if self.read_inode(inode_num)?.kind != KIND_DIR {
			return Err(FsError::Invalid);
		}
		self.read_dir_inode(inode_num)
	}

	// Create the directory at `path`, plus any missing parents (mkdir -p). Succeeds if
	// it already exists as a directory.
	pub fn mkdir(&mut self, path: &[u8]) -> Result<(), FsError> {
		self.begin();
		let r = self.mkdir_inner(path);
		self.finish(r)
	}

	pub(crate) fn mkdir_inner(&mut self, path: &[u8]) -> Result<(), FsError> {
		let segs = split_segments(path)?;
		let mut parent = self.root_inode;
		for seg in segs {
			parent = self.dir_lookup_or_create(parent, seg)?;
		}
		Ok(())
	}

	// Create or overwrite the file at `path` with `data` (create-or-truncate). Missing
	// parent directories are created. Copy-on-write: the new data, extent and checksum
	// blocks, and inode are written to freshly allocated blocks and the transaction
	// commits with a single superblock swap, so a crash leaves either the previous file
	// or the new one intact - never a torn mix.
	pub fn write_file(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
		self.begin();
		let r = self.write_file_inner(path, data);
		self.finish(r)
	}

	pub(crate) fn write_file_inner(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
		let (parent, name) = self.resolve_parent(path, true)?;
		let existing = self.dir_lookup(parent, name)?;
		let old = match existing {
			Some(num) => {
				let inode = self.read_inode(num)?;
				if inode.kind != KIND_FILE {
					return Err(FsError::Invalid);
				}
				Some((num, inode))
			}
			None => None,
		};
		let inode_num = match &old {
			Some((num, _)) => *num,
			None => self.alloc_inode()?,
		};

		// build the new inode from scratch: every logical block is written to a fresh
		// block (the old file's blocks stay referenced by the previous generation).
		let mut inode = Inode::empty(KIND_FILE);
		inode.size = data.len() as u64;
		inode.ctime = match &old {
			Some((_, o)) => o.ctime,
			None => self.clock,
		};
		inode.mtime = self.clock;
		let mut block = vec![0u8; BLOCK_SIZE];
		for i in 0..inode.nblocks() {
			let start = i * BLOCK_SIZE;
			let end = (start + BLOCK_SIZE).min(data.len());
			for b in block.iter_mut() {
				*b = 0;
			}
			block[..end - start].copy_from_slice(&data[start..end]);
			self.write_logical(&mut inode, i, &block)?;
		}

		// transparently compress the freshly written runs: a run that shrinks is replaced
		// by a compressed record, an incompressible one stays raw.
		self.compress_inode(&mut inode)?;

		// point the inode at the new blocks, then name it (new files only). The old
		// inode and blocks are not freed here - the commit's previous generation keeps
		// them as the snapshot, and the next commit reclaims them.
		self.write_inode(inode_num, &mut inode)?;
		if old.is_none() {
			self.dir_insert(parent, name, inode_num)?;
		}
		Ok(())
	}

	// Delete the file or empty directory at `path`. Copy-on-write: the new generation
	// drops the directory entry and frees the inode; a crash before the commit leaves
	// the file fully intact.
	pub fn remove(&mut self, path: &[u8]) -> Result<(), FsError> {
		self.begin();
		let r = self.remove_inner(path);
		self.finish(r)
	}

	// Remove the empty directory at `path`. Rejects a regular file (use `remove`) and a
	// non-empty directory, so a directory is never deleted with its contents.
	pub fn rmdir(&mut self, path: &[u8]) -> Result<(), FsError> {
		self.begin();
		let r = self.rmdir_inner(path);
		self.finish(r)
	}

	pub(crate) fn rmdir_inner(&mut self, path: &[u8]) -> Result<(), FsError> {
		let inode_num = self.resolve(path)?;
		if self.read_inode(inode_num)?.kind != KIND_DIR {
			return Err(FsError::Invalid);
		}
		self.remove_inner(path)
	}

	pub(crate) fn remove_inner(&mut self, path: &[u8]) -> Result<(), FsError> {
		let (parent, name) = self.resolve_parent(path, false)?;
		let inode_num = self.dir_lookup(parent, name)?.ok_or(FsError::NotFound)?;
		let inode = self.read_inode(inode_num)?;
		if inode.kind == KIND_DIR && inode.size != 0 {
			return Err(FsError::Invalid);
		}

		// clear the directory entry and free the inode in the new generation; its old
		// blocks remain referenced by the previous generation.
		self.dir_remove(parent, name)?;
		self.free_inode(inode_num)?;
		Ok(())
	}
}

// Render a superblock to a fresh BLOCK_SIZE block. The self-CRC covers the whole
// block with its own four bytes zeroed, so a torn write (any byte wrong) fails it on
// mount and the slot is rejected.
pub(crate) fn serialize_superblock(sb: &Superblock) -> Vec<u8> {
	let mut block = vec![0u8; BLOCK_SIZE];
	block[0..8].copy_from_slice(&MAGIC);
	block[8..12].copy_from_slice(&VERSION.to_le_bytes());
	block[12..16].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
	block[16..24].copy_from_slice(&sb.num_blocks.to_le_bytes());
	block[24..28].copy_from_slice(&sb.next_inode.to_le_bytes());
	block[28..36].copy_from_slice(&sb.generation.to_le_bytes());
	block[36..44].copy_from_slice(&sb.inode_root.to_le_bytes());
	block[44..48].copy_from_slice(&sb.inode_root_crc.to_le_bytes());
	block[52..56].copy_from_slice(&sb.root_inode.to_le_bytes());
	// the snapshot-table pointer and its CRC32C sit past the self-CRC field, so they are
	// covered by the whole-block checksum below.
	block[60..68].copy_from_slice(&sb.snap_root.to_le_bytes());
	block[68..72].copy_from_slice(&sb.snap_root_crc.to_le_bytes());
	// the CRC bytes are already zero; checksum the block and store it over them.
	let crc = crc32c(&block);
	block[SB_CRC_OFFSET..SB_CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
	block
}

// Parse and validate a superblock block: it must carry the LiberFS magic and version,
// match this build's block size, and pass its own CRC32C. Returns None otherwise (an
// unformatted slot, a foreign disk, or a torn commit).
pub(crate) fn parse_superblock(block: &[u8]) -> Option<Superblock> {
	if block.len() < BLOCK_SIZE {
		return None;
	}
	if block[0..8] != MAGIC {
		return None;
	}
	if u32::from_le_bytes(block[8..12].try_into().ok()?) != VERSION {
		return None;
	}
	if u32::from_le_bytes(block[12..16].try_into().ok()?) != BLOCK_SIZE as u32 {
		return None;
	}
	// verify the self-CRC by recomputing over the block with its CRC bytes zeroed.
	let stored = u32::from_le_bytes(block[SB_CRC_OFFSET..SB_CRC_OFFSET + 4].try_into().ok()?);
	let mut probe = block[..BLOCK_SIZE].to_vec();
	probe[SB_CRC_OFFSET..SB_CRC_OFFSET + 4].fill(0);
	if crc32c(&probe) != stored {
		return None;
	}
	Some(Superblock { num_blocks: u64::from_le_bytes(block[16..24].try_into().ok()?), generation: u64::from_le_bytes(block[28..36].try_into().ok()?), inode_root: u64::from_le_bytes(block[36..44].try_into().ok()?), inode_root_crc: u32::from_le_bytes(block[44..48].try_into().ok()?), next_inode: u32::from_le_bytes(block[24..28].try_into().ok()?), root_inode: u32::from_le_bytes(block[52..56].try_into().ok()?), snap_root: u64::from_le_bytes(block[60..68].try_into().ok()?), snap_root_crc: u32::from_le_bytes(block[68..72].try_into().ok()?) })
}
