use crate::*;

impl<D: BlockDevice> LiberFs<D> {
	// The pool size this filesystem was formatted with, in filesystem blocks (recorded
	// in the superblock; a volume never silently grows past it).
	pub fn num_blocks(&self) -> u64 {
		self.num_blocks
	}

	// Format `dev` as a fresh, empty LiberFS spanning `num_blocks` blocks (an empty root
	// directory, no files), then return it mounted. Default options: a zero uuid, no
	// label, compression off. Generation 0 lays out the two superblock slots and a
	// single inode-tree leaf holding the root directory inode; everything else is the
	// free pool. Inodes and directory nodes are allocated on demand thereafter, so a
	// fresh volume reserves no fixed inode region.
	pub fn format(dev: D, num_blocks: u64) -> Result<LiberFs<D>, FsError> {
		Self::format_opts(dev, num_blocks, FormatOpts::default())
	}

	// `format` with explicit volume identity and the compression switch.
	pub fn format_opts(mut dev: D, num_blocks: u64, opts: FormatOpts) -> Result<LiberFs<D>, FsError> {
		// generation-0 layout: [slot 0][slot 1][inode-tree root leaf], then the free
		// pool. The root directory inode starts empty (no entries, no B+tree yet).
		if num_blocks <= POOL_START + 1 {
			return Err(FsError::Invalid);
		}
		let leaf_block: u64 = POOL_START;
		let mut label = [0u8; LABEL_MAX];
		let take = opts.label.len().min(LABEL_MAX);
		label[..take].copy_from_slice(&opts.label[..take]);

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
		let sb = Superblock { num_blocks, generation: 0, inode_root: leaf_block, inode_root_crc: leaf_crc, next_inode: ROOT_INODE + 1, root_inode: ROOT_INODE, snap_root: 0, snap_root_crc: 0, uuid: opts.uuid, label, compress: opts.compress };
		if !dev.write_block(0, &serialize_superblock(&sb)) {
			return Err(FsError::Io);
		}
		if !dev.write_block(1, &zero) {
			return Err(FsError::Io);
		}
		// make the fresh layout durable before reporting the volume formatted.
		if !dev.flush() {
			return Err(FsError::Io);
		}

		let mut fs = LiberFs { dev, num_blocks, root_inode: ROOT_INODE, generation: 0, slot: 0, inode_root: leaf_block, inode_root_crc: leaf_crc, next_inode: ROOT_INODE + 1, prev_inode_root: 0, prev_inode_root_crc: 0, prev_valid: false, snap_root: 0, snap_root_crc: 0, snapshots: Vec::new(), free: vec![0u8; (num_blocks as usize).div_ceil(8)], data_cursor: POOL_START, meta_cursor: num_blocks - 1, run: None, fresh: BTreeSet::new(), dead: BTreeSet::new(), dead_prev: BTreeSet::new(), pinned: vec![0u8; (num_blocks as usize).div_ceil(8)], snapshots_dirty: false, txn: None, decomp: None, wcsum: None, rcsum: None, icache: BTreeMap::new(), dcache: BTreeMap::new(), read_only: false, uuid: opts.uuid, label, compress: opts.compress, scratch: vec![0u8; BLOCK_SIZE], clock: 0 };
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
	// is read-only: every mutation is refused, so the generations can never interleave.
	pub fn mount_snapshot(dev: D) -> Option<LiberFs<D>> {
		Self::mount_at(dev, false)
	}

	// Mount a named snapshot read-only: the consistent, pinned state captured when the
	// snapshot was created. Returns None if the volume has no such snapshot. Like
	// `mount_snapshot`, the handle refuses every mutation; the live free map (which
	// already reserves the snapshot's blocks) is reused unchanged.
	pub fn mount_named_snapshot(dev: D, name: &[u8]) -> Option<LiberFs<D>> {
		let mut fs = Self::mount(dev)?;
		let snap = fs.snapshots.iter().find(|s| s.name == name)?.clone();
		fs.inode_root = snap.inode_root;
		fs.inode_root_crc = snap.inode_root_crc;
		fs.generation = snap.generation;
		fs.read_only = true;
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

		let mut fs = LiberFs { dev, num_blocks: sb.num_blocks, root_inode: sb.root_inode, generation: sb.generation, slot: cur_slot, inode_root: sb.inode_root, inode_root_crc: sb.inode_root_crc, next_inode: sb.next_inode, prev_inode_root, prev_inode_root_crc, prev_valid, snap_root: sb.snap_root, snap_root_crc: sb.snap_root_crc, snapshots: Vec::new(), free: vec![0u8; (sb.num_blocks as usize).div_ceil(8)], data_cursor: POOL_START, meta_cursor: sb.num_blocks - 1, run: None, fresh: BTreeSet::new(), dead: BTreeSet::new(), dead_prev: BTreeSet::new(), pinned: vec![0u8; (sb.num_blocks as usize).div_ceil(8)], snapshots_dirty: false, txn: None, decomp: None, wcsum: None, rcsum: None, icache: BTreeMap::new(), dcache: BTreeMap::new(), read_only: !newest, uuid: sb.uuid, label: sb.label, compress: sb.compress, scratch: vec![0u8; BLOCK_SIZE], clock: 0 };
		// a corrupt snapshot table degrades the mount to read-only instead of failing it:
		// the pinned generations it named can no longer be reserved, so a commit could
		// reuse their blocks - refusing every mutation keeps them (and the table block
		// itself, for repair) intact. An I/O failure fails the mount as before.
		match fs.load_snapshot_table() {
			Ok(()) => {}
			Err(FsError::Corrupt) => fs.read_only = true,
			Err(_) => return None,
		}
		fs.derive_free().ok()?;
		Some(fs)
	}

	// Is this mount read-only (a snapshot mount, or degraded by a corrupt snapshot
	// table)? Every mutation on a read-only mount fails with FsError::ReadOnly.
	pub fn is_read_only(&self) -> bool {
		self.read_only
	}

	// The volume's unique id, assigned at format time.
	pub fn uuid(&self) -> [u8; 16] {
		self.uuid
	}

	// The volume's label (the NUL padding stripped).
	pub fn label(&self) -> &[u8] {
		name_in(&self.label)
	}

	// Is transparent compression enabled for new whole-file writes?
	pub fn compression(&self) -> bool {
		self.compress
	}

	// Switch transparent compression on or off for the volume. Governs new whole-file
	// writes only: existing extents keep their current form (a raw file compresses on
	// its next whole-file rewrite; a compressed one stays readable and thaws on partial
	// writes as always). Commits atomically like any mutation; a read-only mount
	// refuses even a no-change request, so the policy has no side door.
	pub fn set_compression(&mut self, enabled: bool) -> Result<(), FsError> {
		if self.read_only {
			return Err(FsError::ReadOnly);
		}
		if self.compress == enabled {
			return Ok(());
		}
		self.mutate(|fs| {
			fs.compress = enabled;
			Ok(())
		})
	}

	// How many pool blocks are free right now (a popcount over the in-memory free map),
	// and the pool's size: the `df` numbers, in blocks.
	pub fn free_blocks(&self) -> u64 {
		let mut used: u64 = 0;
		for &byte in self.free.iter() {
			used += byte.count_ones() as u64;
		}
		self.num_blocks - used
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
			return Err(FsError::IsDir);
		}
		let size = inode.size;
		self.read_range(&inode, 0, size)
	}

	// Read up to `len` bytes of `inode` starting at byte `offset` - the one range
	// reader behind both `read_file` (the whole file) and `read_at` (a slice). Returns
	// fewer bytes (or none) if the range runs past the end; holes read back as zeros.
	// Lengths and block indexes are u64 end to end, so a 32-bit build never silently
	// truncates a large file (an allocation it cannot hold fails as itself).
	pub(crate) fn read_range(&mut self, inode: &Inode, offset: u64, len: u64) -> Result<Vec<u8>, FsError> {
		if offset >= inode.size || len == 0 {
			return Ok(Vec::new());
		}
		let end = offset.saturating_add(len).min(inode.size);
		let mut out = Vec::with_capacity((end - offset) as usize);
		let mut buf = vec![0u8; BLOCK_SIZE];
		let first = offset / BLOCK_SIZE as u64;
		let last = (end - 1) / BLOCK_SIZE as u64;
		for lb in first..=last {
			let block_start = lb * BLOCK_SIZE as u64;
			if !self.read_logical(inode, lb, &mut buf)? {
				buf.fill(0);
			}
			let copy_start = offset.max(block_start);
			let copy_end = end.min(block_start + BLOCK_SIZE as u64);
			out.extend_from_slice(&buf[(copy_start - block_start) as usize..(copy_end - block_start) as usize]);
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
			return Err(FsError::NotDir);
		}
		self.read_dir_inode(inode_num)
	}

	// Create the directory at `path`, plus any missing parents (mkdir -p). Succeeds if
	// it already exists as a directory.
	pub fn mkdir(&mut self, path: &[u8]) -> Result<(), FsError> {
		self.mutate(|fs| fs.mkdir_inner(path))
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
		self.mutate(|fs| fs.write_file_inner(path, data))
	}

	pub(crate) fn write_file_inner(&mut self, path: &[u8], data: &[u8]) -> Result<(), FsError> {
		let (parent, name) = self.resolve_parent(path, true)?;
		let existing = self.dir_lookup(parent, name)?;
		let old = match existing {
			Some(num) => {
				let inode = self.read_inode(num)?;
				if inode.kind != KIND_FILE {
					return Err(FsError::IsDir);
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
		// block (the old file's blocks stay referenced by the previous generation, and
		// leave the new one - recorded via the dead list). A contiguous run is reserved
		// up front, so the file lands in as few extents as the pool allows.
		let mut inode = Inode::empty(KIND_FILE);
		inode.size = data.len() as u64;
		inode.ctime = match &old {
			Some((_, o)) => o.ctime,
			None => self.clock,
		};
		inode.mtime = self.clock;
		if let Some((_, o)) = &old {
			self.drop_inode_blocks(o)?;
		}
		self.reserve_run(inode.nblocks());
		let mut block = vec![0u8; BLOCK_SIZE];
		for i in 0..inode.nblocks() {
			// the data slice is memory-resident, so its offsets fit usize by definition.
			let start = (i * BLOCK_SIZE as u64) as usize;
			let end = (start + BLOCK_SIZE).min(data.len());
			block.fill(0);
			block[..end - start].copy_from_slice(&data[start..end]);
			self.write_logical(&mut inode, i, &block)?;
		}
		self.release_run();

		// transparently compress the freshly written runs when the volume opted in: a
		// run that shrinks is replaced by a compressed record, an incompressible one
		// stays raw. With compression off (the default) every run stays raw.
		if self.compress {
			self.compress_inode(&mut inode)?;
		}

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
		self.mutate(|fs| fs.remove_inner(path))
	}

	// Remove the empty directory at `path`. Rejects a regular file (use `remove`) and a
	// non-empty directory, so a directory is never deleted with its contents.
	pub fn rmdir(&mut self, path: &[u8]) -> Result<(), FsError> {
		self.mutate(|fs| fs.rmdir_inner(path))
	}

	pub(crate) fn rmdir_inner(&mut self, path: &[u8]) -> Result<(), FsError> {
		let inode_num = self.resolve(path)?;
		if self.read_inode(inode_num)?.kind != KIND_DIR {
			return Err(FsError::NotDir);
		}
		self.remove_inner(path)
	}

	pub(crate) fn remove_inner(&mut self, path: &[u8]) -> Result<(), FsError> {
		let (parent, name) = self.resolve_parent(path, false)?;
		let inode_num = self.dir_lookup(parent, name)?.ok_or(FsError::NotFound)?;
		let inode = self.read_inode(inode_num)?;
		if inode.kind == KIND_DIR && inode.size != 0 {
			return Err(FsError::NotEmpty);
		}

		// clear the directory entry and free the inode in the new generation; its old
		// blocks remain referenced by the previous generation and leave the new one.
		if inode.kind == KIND_FILE {
			self.drop_inode_blocks(&inode)?;
		} else if inode.dir_root != 0 {
			// an empty directory's tree root is 0; a non-zero root here cannot hold
			// entries, but drop its node(s) defensively.
			let mut map = vec![0u8; self.free.len()];
			self.mark_dir_tree(inode.dir_root, &mut map)?;
			for b in 0..self.num_blocks {
				if test_bit(&map, b) {
					self.drop_block(b);
				}
			}
		}
		self.dir_remove(parent, name)?;
		self.free_inode(inode_num)?;
		Ok(())
	}
}

// Render a superblock to a fresh BLOCK_SIZE block. The self-CRC covers the whole
// block with its own four bytes zeroed, so a torn write (any byte wrong) fails it on
// mount and the slot is rejected. Bytes 72 onward are the second-revision fields:
// the feature flags, the volume identity, and the algorithm/compression bytes.
pub(crate) fn serialize_superblock(sb: &Superblock) -> Vec<u8> {
	let mut block = vec![0u8; BLOCK_SIZE];
	block[SB_MAGIC_OFF..SB_MAGIC_OFF + 8].copy_from_slice(&MAGIC);
	block[SB_VERSION_OFF..SB_VERSION_OFF + 4].copy_from_slice(&VERSION.to_le_bytes());
	block[SB_BLOCK_SIZE_OFF..SB_BLOCK_SIZE_OFF + 4].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
	block[SB_NUM_BLOCKS_OFF..SB_NUM_BLOCKS_OFF + 8].copy_from_slice(&sb.num_blocks.to_le_bytes());
	block[SB_NEXT_INODE_OFF..SB_NEXT_INODE_OFF + 4].copy_from_slice(&sb.next_inode.to_le_bytes());
	block[SB_GENERATION_OFF..SB_GENERATION_OFF + 8].copy_from_slice(&sb.generation.to_le_bytes());
	block[SB_INODE_ROOT_OFF..SB_INODE_ROOT_OFF + 8].copy_from_slice(&sb.inode_root.to_le_bytes());
	block[SB_INODE_ROOT_CRC_OFF..SB_INODE_ROOT_CRC_OFF + 4].copy_from_slice(&sb.inode_root_crc.to_le_bytes());
	block[SB_ROOT_INODE_OFF..SB_ROOT_INODE_OFF + 4].copy_from_slice(&sb.root_inode.to_le_bytes());
	// the fields past the self-CRC offset are covered by the whole-block checksum below.
	block[SB_SNAP_ROOT_OFF..SB_SNAP_ROOT_OFF + 8].copy_from_slice(&sb.snap_root.to_le_bytes());
	block[SB_SNAP_ROOT_CRC_OFF..SB_SNAP_ROOT_CRC_OFF + 4].copy_from_slice(&sb.snap_root_crc.to_le_bytes());
	block[SB_FEATURES_OFF..SB_FEATURES_OFF + 8].copy_from_slice(&FEATURES.to_le_bytes());
	block[SB_UUID_OFF..SB_UUID_OFF + 16].copy_from_slice(&sb.uuid);
	block[SB_LABEL_OFF..SB_LABEL_OFF + LABEL_MAX].copy_from_slice(&sb.label);
	block[SB_CSUM_ALGO_OFF] = CSUM_ALGO_CRC32C;
	block[SB_CODEC_OFF] = CODEC_LZ4;
	block[SB_COMPRESS_OFF] = sb.compress as u8;
	// the CRC bytes are already zero; checksum the block and store it over them.
	let crc = crc32c(&block);
	block[SB_CRC_OFFSET..SB_CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
	block
}

// Parse and validate a superblock block: it must carry the LiberFS magic and version,
// match this build's block size, feature flags, and algorithm ids, and pass its own
// CRC32C. Returns None otherwise (an unformatted slot, a foreign disk, a torn commit,
// or a volume laid down by a build with a different layout or algorithms - which the
// flags catch instead of a silent mis-parse).
pub(crate) fn parse_superblock(block: &[u8]) -> Option<Superblock> {
	if block.len() < BLOCK_SIZE {
		return None;
	}
	if block[SB_MAGIC_OFF..SB_MAGIC_OFF + 8] != MAGIC {
		return None;
	}
	if u32::from_le_bytes(block[SB_VERSION_OFF..SB_VERSION_OFF + 4].try_into().ok()?) != VERSION {
		return None;
	}
	if u32::from_le_bytes(block[SB_BLOCK_SIZE_OFF..SB_BLOCK_SIZE_OFF + 4].try_into().ok()?) != BLOCK_SIZE as u32 {
		return None;
	}
	if u64::from_le_bytes(block[SB_FEATURES_OFF..SB_FEATURES_OFF + 8].try_into().ok()?) != FEATURES {
		return None;
	}
	if block[SB_CSUM_ALGO_OFF] != CSUM_ALGO_CRC32C || block[SB_CODEC_OFF] != CODEC_LZ4 {
		return None;
	}
	// verify the self-CRC by recomputing over the block with its CRC bytes zeroed.
	let stored = u32::from_le_bytes(block[SB_CRC_OFFSET..SB_CRC_OFFSET + 4].try_into().ok()?);
	let mut probe = block[..BLOCK_SIZE].to_vec();
	probe[SB_CRC_OFFSET..SB_CRC_OFFSET + 4].fill(0);
	if crc32c(&probe) != stored {
		return None;
	}
	let mut uuid = [0u8; 16];
	uuid.copy_from_slice(&block[SB_UUID_OFF..SB_UUID_OFF + 16]);
	let mut label = [0u8; LABEL_MAX];
	label.copy_from_slice(&block[SB_LABEL_OFF..SB_LABEL_OFF + LABEL_MAX]);
	Some(Superblock { num_blocks: u64::from_le_bytes(block[SB_NUM_BLOCKS_OFF..SB_NUM_BLOCKS_OFF + 8].try_into().ok()?), generation: u64::from_le_bytes(block[SB_GENERATION_OFF..SB_GENERATION_OFF + 8].try_into().ok()?), inode_root: u64::from_le_bytes(block[SB_INODE_ROOT_OFF..SB_INODE_ROOT_OFF + 8].try_into().ok()?), inode_root_crc: u32::from_le_bytes(block[SB_INODE_ROOT_CRC_OFF..SB_INODE_ROOT_CRC_OFF + 4].try_into().ok()?), next_inode: u32::from_le_bytes(block[SB_NEXT_INODE_OFF..SB_NEXT_INODE_OFF + 4].try_into().ok()?), root_inode: u32::from_le_bytes(block[SB_ROOT_INODE_OFF..SB_ROOT_INODE_OFF + 4].try_into().ok()?), snap_root: u64::from_le_bytes(block[SB_SNAP_ROOT_OFF..SB_SNAP_ROOT_OFF + 8].try_into().ok()?), snap_root_crc: u32::from_le_bytes(block[SB_SNAP_ROOT_CRC_OFF..SB_SNAP_ROOT_CRC_OFF + 4].try_into().ok()?), uuid, label, compress: block[SB_COMPRESS_OFF] != 0 })
}
