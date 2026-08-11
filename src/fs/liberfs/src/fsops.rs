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
	// Format a volume with a ZERO uuid, for tests and scratch images.
	//
	// A `no_std` crate cannot invent randomness, so a real volume's identity has to come from the
	// caller - and this entry point cannot supply one. `FormatOpts::default()` is sixteen zeros, so
	// every volume made this way shares the id that `uuid()` documents as unique, which is exactly
	// the thing an id is for. Named so a caller has to mean it, and `format_opts` is the way to
	// make a volume anything else will identify.
	pub fn format_scratch(dev: D, num_blocks: u64) -> Result<LiberFs<D>, FsError> {
		Self::format_opts(dev, num_blocks, FormatOpts::default())
	}

	// `format` with explicit volume identity and the compression switch.
	pub fn format_opts(mut dev: D, num_blocks: u64, opts: FormatOpts) -> Result<LiberFs<D>, FsError> {
		// generation-0 layout: [slot 0][slot 1][inode-tree root leaf], then the free
		// pool. The root directory inode starts empty (no entries, no B+tree yet).
		if num_blocks <= POOL_START + 1 {
			return Err(FsError::Invalid);
		}
		// the free maps are sized from the pool, and the cast was unchecked in both
		// places: it truncates on a 32-bit target - producing a bitmap too small for the
		// volume, so the allocator would hand out blocks it never tracked - and asks for
		// an enormous infallible allocation on a 64-bit one. The bound is documented as
		// the format's maximum volume size and refused here.
		let map_len = free_map_len(num_blocks)?;
		let (free_map, pinned_map) = (try_zeroed(map_len)?, try_zeroed(map_len)?);
		let leaf_block: u64 = POOL_START;
		let mut label = [0u8; LABEL_MAX];
		// The record is specified as UTF-8, so it has to BE UTF-8 - and a NUL inside it
		// would silently truncate the label at the next mount, since `label()` reads up to
		// the first NUL. Neither was checked: only the clamp below was, which avoided
		// cutting a character in half in a value that might not have been text at all.
		// Same treatment as a snapshot name, and for the same reason.
		if opts.label.contains(&0) || core::str::from_utf8(&opts.label).is_err() {
			return Err(FsError::BadName);
		}
		// clamp the label to LABEL_MAX without cutting a UTF-8 character in half.
		let mut take = opts.label.len().min(LABEL_MAX);
		while take > 0 && take < opts.label.len() && (opts.label[take] & 0xC0) == 0x80 {
			take -= 1;
		}
		label[..take].copy_from_slice(&opts.label[..take]);

		// the inode tree's sole leaf: one record keyed by inode 0 (the root directory).
		let mut leaf = vec![0u8; BLOCK_SIZE];
		node_set_header(&mut leaf, NODE_LEAF, 1);
		leaf[NODE_HDR..NODE_HDR + 8].copy_from_slice(&(ROOT_INODE as u64).to_le_bytes());
		Inode::empty(TYPE_DIR).write(&mut leaf[NODE_HDR + 8..NODE_HDR + 8 + INODE_SIZE]);
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

		let mut fs = LiberFs { dev, num_blocks, root_inode: ROOT_INODE, generation: 0, slot: 0, inode_root: leaf_block, inode_root_crc: leaf_crc, next_inode: ROOT_INODE + 1, prev_inode_root: 0, prev_inode_root_crc: 0, prev_snap_root: 0, prev_snap_root_crc: 0, prev_valid: false, snap_root: 0, snap_root_crc: 0, snapshots: Vec::new(), free: free_map, data_cursor: POOL_START, meta_cursor: num_blocks - 1, run: None, fresh: BTreeSet::new(), dead: BTreeSet::new(), dead_prev: BTreeSet::new(), pinned: pinned_map, snapshots_dirty: false, txn: None, decomp: DecompCache::new(), wcsum: None, rcsum: None, icache: BTreeMap::new(), dcache: BTreeMap::new(), read_only: false, walk_damage: false, mark_strict: false, mark_dup: None, mark_names: None, mark_alias: false, mark_max_inode: 0, uuid: opts.uuid, label, compress: opts.compress, scratch: vec![0u8; BLOCK_SIZE], clock: 0 };
		fs.derive_free()?;
		Ok(fs)
	}

	// Mount an existing LiberFS on `dev` at its newest committed generation. Returns None
	// if neither superblock slot is a valid LiberFS (an unformatted or foreign disk).
	pub fn mount(dev: D) -> Result<LiberFs<D>, MountError> {
		Self::mount_at(dev, MountMode::Newest)
	}

	// Mount the newest slot that parses, whatever happened to the other one, and never
	// writable. The recovery door for the refusals `Newest` now makes: an operator whose
	// disk lost a superblock read, or whose volume was written by a build this one does
	// not know, can still get the data off. It is deliberately a different call, because
	// the whole point of the refusal is that this is not something to do by accident.
	pub fn mount_recovery(dev: D) -> Result<LiberFs<D>, MountError> {
		Self::mount_at(dev, MountMode::Recovery)
	}

	// Mount the previous generation read-only: the consistent snapshot of the filesystem one
	// commit ago. `Ok(None)` when there is no older generation - a freshly formatted or
	// single-generation volume. The handle is read-only: every mutation is refused, so the
	// generations can never interleave.
	//
	// `Option` ALONE was the wrong shape, and the comment that argued for it only covered the
	// absence. `.ok()` turned a corrupt volume, an I/O failure, an unsupported format and a memory
	// shortage into the same answer as "there is no snapshot here" - which is exactly the
	// conflation `MountError` was introduced to end on the main mount.
	pub fn mount_snapshot(dev: D) -> Result<Option<LiberFs<D>>, MountError> {
		match Self::mount_at(dev, MountMode::Previous) {
			Ok(fs) => Ok(Some(fs)),
			// The one ordinary absence. Everything else is a fault and says so.
			Err(MountError::Unformatted) => Ok(None),
			Err(error) => Err(error),
		}
	}

	// Mount a named snapshot read-only: the consistent, pinned state captured when the
	// snapshot was created. `Ok(None)` when the volume has no such snapshot; a volume that could
	// not be mounted at all is an error, for the reason above. Like `mount_snapshot`, the handle
	// refuses every mutation; the live free map (which already reserves the snapshot's blocks) is
	// reused unchanged.
	pub fn mount_named_snapshot(dev: D, name: &[u8]) -> Result<Option<LiberFs<D>>, MountError> {
		let mut fs = Self::mount(dev)?;
		let Some(snap) = fs.snapshots.iter().find(|s| s.name == name).cloned() else {
			return Ok(None);
		};
		fs.inode_root = snap.inode_root;
		fs.inode_root_crc = snap.inode_root_crc;
		fs.generation = snap.generation;
		fs.read_only = true;
		// switching the root switches the generation, so every cache keyed by something
		// this generation decides is now describing the wrong tree - the same clearing
		// `with_root` does for the same reason. This was latent rather than harmless:
		// nothing populated these caches between the mount and the swap, so the stale
		// entries had no chance to exist. The moment the mount read a single inode of its
		// own, the root directory it cached was the LIVE one and the snapshot resolved
		// every path through it.
		fs.icache.clear();
		fs.dcache.clear();
		fs.decomp.clear();
		Ok(Some(fs))
	}

	pub(crate) fn mount_at(mut dev: D, mode: MountMode) -> Result<LiberFs<D>, MountError> {
		let newest = !matches!(mode, MountMode::Previous);
		// read and validate both superblock slots.
		let mut buf = vec![0u8; BLOCK_SIZE];
		let mut slots: [Option<Superblock>; SUPER_SLOTS as usize] = [None, None];
		// What the slots turned out to be, so a failure can say which it was. An I/O failure
		// anywhere outranks everything: a device that did not answer has told us nothing about
		// what is on it, and "nothing" must never be read as "blank".
		let mut saw_io = false;
		let mut saw_ours = false;
		let mut saw_unsupported = false;
		for s in 0..SUPER_SLOTS {
			if !dev.read_block(s as u64, &mut buf) {
				saw_io = true;
				continue;
			}
			match read_slot(&buf) {
				SlotRead::Valid(sb) => {
					saw_ours = true;
					slots[s as usize] = Some(sb);
				}
				SlotRead::Corrupt => saw_ours = true,
				SlotRead::Unsupported => {
					saw_ours = true;
					saw_unsupported = true;
				}
				SlotRead::Unformatted => {}
			}
		}
		// The verdict when no slot is usable, in order of what it costs to be wrong about.
		let no_slot = || -> MountError {
			if saw_io {
				MountError::Io
			} else if saw_unsupported {
				MountError::Unsupported
			} else if saw_ours {
				MountError::Corrupt
			} else {
				MountError::Unformatted
			}
		};
		// A slot that did not answer, or that this build cannot read, is not a slot whose
		// generation is known - and a WRITABLE mount that proceeds without knowing it will
		// hand out the blocks that slot's generation holds and then overwrite the slot
		// itself. One failed 4 KiB read was enough to destroy a newer consistent
		// generation, and an older build meeting a newer format did the same thing on
		// purpose.
		//
		// These flags used to be consulted only when NOTHING parsed, which is the one case
		// where they change nothing that matters: there is no volume to protect. They are
		// consulted here instead, where there IS one.
		//
		// `Recovery` skips this and is read-only for exactly that reason; `Previous` is
		// read-only by construction and already requires two valid slots.
		if matches!(mode, MountMode::Newest) {
			if saw_io {
				return Err(MountError::Io);
			}
			if saw_unsupported {
				return Err(MountError::Unsupported);
			}
		}
		// order the valid slots by generation: the higher is the live root, the lower
		// the snapshot.
		let mut valid: Vec<(u32, u64)> = (0..SUPER_SLOTS).filter_map(|s| slots[s as usize].map(|sb| (s, sb.generation))).collect();
		valid.sort_by_key(|&(_, g)| g);
		let (cur_slot, prev_slot) = if newest {
			let &(cur, _) = valid.last().ok_or_else(no_slot)?;
			let prev = valid.iter().rev().nth(1).map(|&(s, _)| s);
			(cur, prev)
		} else {
			// the snapshot: the lower generation, only if there are two.
			if valid.len() < 2 {
				return Err(MountError::Unformatted);
			}
			(valid[0].0, None)
		};

		// THE TWO SLOTS MUST DESCRIBE THE SAME VOLUME. Each was validated alone and nothing ever
		// compared them, so two checksum-valid slots from unrelated states mounted as a pair - and
		// `derive_free` then reads the previous root under the current slot's geometry, with that
		// rolling snapshot being part of what keeps the allocator honest. The four fields that make
		// them one volume are the identity, the geometry, the namespace root, and consecutive
		// generations: a commit writes the other slot with generation + 1 and nothing else.
		//
		// A mismatch means one of the slots is not this volume's, and which one cannot be known
		// from here - so the newer is used ALONE and the mount is read-only. That keeps both slots
		// on the medium for repair instead of letting the next commit overwrite the evidence.
		let mut unpaired = false;
		if let (Some(a), Some(b)) = (slots[0], slots[1]) {
			let (older, newer) = if a.generation <= b.generation { (a, b) } else { (b, a) };
			unpaired = older.uuid != newer.uuid || older.num_blocks != newer.num_blocks || older.root_inode != newer.root_inode || older.generation + 1 != newer.generation;
		}

		// The snapshot mount is the older slot BECAUSE it is one commit behind this one. Unpaired,
		// it is one commit behind nothing, so there is no snapshot to serve.
		if unpaired && !newest {
			return Err(MountError::Corrupt);
		}

		let sb = slots[cur_slot as usize].ok_or(MountError::Corrupt)?;
		// the medium must actually cover the claimed pool: a checksummed superblock can
		// still lie about `num_blocks` (hostile authoring), and sizing the free maps or
		// walking the generations off such a claim means an absurd allocation or reads
		// past the volume. Probing the last claimed block bounds the claim by the device.
		if !dev.read_block(sb.num_blocks - 1, &mut buf) {
			return Err(MountError::DeviceTooSmall);
		}
		let map_len = free_map_len(sb.num_blocks).map_err(|_| MountError::Unsupported)?;
		// built before the filesystem, so a size this machine cannot map is a mount ERROR
		// and not an allocator abort taking the whole service with it.
		let free_map = try_zeroed(map_len).map_err(|_| MountError::NoMemory)?;
		let pinned_map = try_zeroed(map_len).map_err(|_| MountError::NoMemory)?;
		let (prev_inode_root, prev_inode_root_crc, prev_snap_root, prev_snap_root_crc, prev_valid) = match prev_slot.filter(|_| !unpaired) {
			Some(ps) => {
				let psb = slots[ps as usize].ok_or(MountError::Corrupt)?;
				(psb.inode_root, psb.inode_root_crc, psb.snap_root, psb.snap_root_crc, true)
			}
			None => (0, 0, 0, 0, false),
		};

		let mut fs = LiberFs { dev, num_blocks: sb.num_blocks, root_inode: sb.root_inode, generation: sb.generation, slot: cur_slot, inode_root: sb.inode_root, inode_root_crc: sb.inode_root_crc, next_inode: sb.next_inode, prev_inode_root, prev_inode_root_crc, prev_snap_root, prev_snap_root_crc, prev_valid, snap_root: sb.snap_root, snap_root_crc: sb.snap_root_crc, snapshots: Vec::new(), free: free_map, data_cursor: POOL_START, meta_cursor: sb.num_blocks - 1, run: None, fresh: BTreeSet::new(), dead: BTreeSet::new(), dead_prev: BTreeSet::new(), pinned: pinned_map, snapshots_dirty: false, txn: None, decomp: DecompCache::new(), wcsum: None, rcsum: None, icache: BTreeMap::new(), dcache: BTreeMap::new(), read_only: !newest || unpaired, walk_damage: false, mark_strict: false, mark_dup: None, mark_names: None, mark_alias: false, mark_max_inode: 0, uuid: sb.uuid, label: sb.label, compress: sb.compress, scratch: vec![0u8; BLOCK_SIZE], clock: 0 };
		// a corrupt snapshot table degrades the mount to read-only instead of failing it:
		// the pinned generations it named can no longer be reserved, so a commit could
		// reuse their blocks - refusing every mutation keeps them (and the table block
		// itself, for repair) intact. An I/O failure fails the mount as before.
		match fs.load_snapshot_table() {
			Ok(()) => {}
			Err(FsError::Corrupt) => fs.read_only = true,
			// a refused allocation is the MACHINE, not the medium - see `derive_free` below.
			Err(FsError::NoSpace) => return Err(MountError::NoMemory),
			Err(_) => return Err(MountError::Io),
		}
		// a generation walk that could not complete (an unreadable node, a broken spill
		// chain) leaves the free map incomplete: degrade to read-only - a read-only
		// mount never allocates, so the incomplete map is harmless, and failing the
		// mount instead would present the volume as unformatted (and cost its data to
		// the next format). An error other than Corrupt still fails the mount.
		if matches!(mode, MountMode::Recovery) {
			fs.read_only = true;
		}
		match fs.derive_free() {
			Ok(()) => {}
			Err(FsError::Corrupt) => fs.read_only = true,
			// the generation walk sizes its own maps, and `try_zeroed` reports a refused
			// allocation as `NoSpace`. Folding that into `Io` told the operator the disk
			// did not answer when the disk was fine and the MACHINE was short - a wrong
			// diagnosis pointing at the wrong component. The process no longer aborting was
			// the point of the fallible allocation; carrying the reason through is the rest
			// of it, and `MountError::NoMemory` already existed to say exactly this.
			Err(FsError::NoSpace) => return Err(MountError::NoMemory),
			Err(_) => return Err(MountError::Io),
		}
		// the two superblock relations that need the tree to settle. Both are read-only
		// degradations rather than mount failures: the volume's data is intact and
		// readable, and read-only is exactly what forecloses the harm in each case.
		//
		// `next_inode` naming an inode that ALREADY exists is the dangerous one -
		// `alloc_inode` hands the number out unexamined, so the next file created takes
		// over an existing inode and every name pointing at it.
		// The counter must sit above EVERY live inode, not merely on a free number. The
		// live walk in `derive_free` above recorded the highest key it saw, which is the
		// whole invariant; the direct read stays as a cheap second opinion for the case
		// the walk could not complete.
		if (fs.next_inode as u64) <= fs.mark_max_inode {
			fs.read_only = true;
		}
		match fs.read_inode(fs.next_inode) {
			Err(FsError::Invalid) => {}
			Ok(_) => fs.read_only = true,
			Err(FsError::Corrupt | FsError::Io) => fs.read_only = true,
			Err(e) => return Err(map_mount_error(e)),
		}
		// THE ROOT IS INODE 0, which is what the format says and what `format` writes. The parser
		// asked only that `next_inode > root_inode`, so an image could nominate any other directory
		// inode as the root and mount over a subtree, with the rest of the inode tree present,
		// checksummed and unreachable.
		//
		// Checked HERE and not in `parse_checked`, and the difference matters: refusing the
		// superblock outright makes the mount fall back to the previous generation and mount it
		// WRITABLE, so a hand-written root would silently discard the newest generation instead of
		// being reported. Degrading is the answer that keeps what is there and says so.
		if fs.root_inode != ROOT_INODE {
			fs.read_only = true;
		}
		// and the root of the namespace has to be a directory, or every path resolution
		// starts from something that cannot hold names.
		match fs.read_inode(fs.root_inode) {
			Ok(inode) if inode.r#type == TYPE_DIR => {}
			Ok(_) | Err(FsError::Invalid | FsError::Corrupt | FsError::Io) => fs.read_only = true,
			Err(e) => return Err(map_mount_error(e)),
		}
		Ok(fs)
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
	// `Ok(None)` means the path names nothing. Every other answer is an ERROR and says
	// which: this used to be `resolve(path).ok()`, which made "no such file" and a disk
	// that could not be read and a malformed path into the same word - the mount's
	// ambiguity one layer up.
	pub fn lookup(&mut self, path: &[u8]) -> Result<Option<u32>, FsError> {
		match self.resolve(path) {
			Ok(num) => Ok(Some(num)),
			Err(FsError::NotFound) => Ok(None),
			Err(e) => Err(e),
		}
	}

	// Read the whole file at `path` into a freshly allocated buffer.
	pub fn read_file(&mut self, path: &[u8]) -> Result<Vec<u8>, FsError> {
		let inode_num = self.resolve(path)?;
		let inode = self.read_inode(inode_num)?;
		if inode.r#type != TYPE_FILE {
			return Err(FsError::IsDir);
		}
		let size = inode.size;
		// A size the pool cannot hold is refused as a WHOLE-file read: the buffer could neither be
		// allocated nor filled. The refusal is right and the WORD was wrong - this said `Corrupt`,
		// while acknowledging in the same breath that a sparse file may legitimately be sized past
		// the pool's bytes and stays readable through `read_at`. So a caller was told the medium
		// was inconsistent about a file that is fine, and sent looking for a fault that is not
		// there. `TooLarge` says what it is: too big for one buffer, ask for a range.
		if size > self.num_blocks.saturating_mul(BLOCK_SIZE as u64) {
			return Err(FsError::TooLarge);
		}
		self.read_range(&inode, 0, size)
	}

	// Read up to `len` bytes of `inode` starting at byte `offset` - the one range
	// reader behind both `read_file` (the whole file) and `read_at` (a slice). Returns
	// fewer bytes (or none) if the range runs past the end; holes read back as zeros.
	// Lengths and block indexes are u64 end to end, so a 32-bit build never silently
	// truncates a large file (an allocation it cannot hold fails as itself).
	// Contiguous raw-extent runs move as one large device request (up to the run
	// buffer) instead of one block at a time.
	pub(crate) fn read_range(&mut self, inode: &Inode, offset: u64, len: u64) -> Result<Vec<u8>, FsError> {
		// the most blocks one device request carries (1 MB); the run buffer's size.
		const RUN_BLOCKS: u64 = 256;
		if offset >= inode.size || len == 0 {
			return Ok(Vec::new());
		}
		let end = offset.saturating_add(len).min(inode.size);
		// FALLIBLE, both of them. The fallible-allocation discipline reached the free maps and the
		// mount and stopped at the door of the read path, which is the one place a number off the
		// medium decides the size directly: a checksum-consistent inode with an enormous sparse
		// size makes `read_file` ask for that many bytes in one go, and `Vec::with_capacity` answers
		// an impossible request by ABORTING the process. The volume's byte count bounds it, and a
		// volume can be larger than the machine.
		let mut out: Vec<u8> = Vec::new();
		// `usize::try_from`, because the comment above claims u64 end to end "so a 32-bit build never
		// silently truncates a large file" and the next line used to be an `as usize`. On a 64-bit
		// target the conversion cannot fail and the claim was true by accident; on the 32-bit one
		// the comment was written for, it was false.
		let want = usize::try_from(end - offset).map_err(|_| FsError::TooLarge)?;
		out.try_reserve_exact(want).map_err(|_| FsError::NoMemory)?;
		let first = offset / BLOCK_SIZE as u64;
		let last = (end - 1) / BLOCK_SIZE as u64;
		// The run buffer is bounded by RUN_BLOCKS (1 MB) rather than by the file, so it is small -
		// and it is still a request the machine may refuse.
		let mut buf = try_zeroed((last - first + 1).min(RUN_BLOCKS) as usize * BLOCK_SIZE)?;
		let mut lb = first;
		while lb <= last {
			let want = (last - lb + 1).min(RUN_BLOCKS);
			let (n, mapped) = self.read_logical_run(inode, lb, want, &mut buf)?;
			if !mapped {
				buf[..n as usize * BLOCK_SIZE].fill(0);
			}
			let run_start = lb * BLOCK_SIZE as u64;
			let copy_start = offset.max(run_start);
			// same ceiling as `write_at_inner`: a sparse file may legitimately be sized to
			// the top of the address space, and the last run of it starts within a run's
			// length of u64::MAX. `end` bounds the result either way, so saturating is
			// exact rather than merely safe.
			let copy_end = end.min(run_start.saturating_add(n * BLOCK_SIZE as u64));
			out.extend_from_slice(&buf[(copy_start - run_start) as usize..(copy_end - run_start) as usize]);
			lb += n;
		}
		Ok(out)
	}

	// List the root directory as (name, size, is_dir, mtime, ctime) tuples, one per
	// live entry.
	pub fn list(&mut self) -> Result<Vec<(Vec<u8>, u64, bool, u64, u64)>, FsError> {
		self.read_dir_inode(self.root_inode)
	}

	// List the directory at `path` as (name, size, is_dir, mtime, ctime) tuples.
	pub fn read_dir(&mut self, path: &[u8]) -> Result<Vec<(Vec<u8>, u64, bool, u64, u64)>, FsError> {
		let inode_num = self.resolve(path)?;
		if self.read_inode(inode_num)?.r#type != TYPE_DIR {
			return Err(FsError::NotDir);
		}
		self.read_dir_inode(inode_num)
	}

	// One page of a directory listing: at most `max` rows in key order, starting after the name
	// `after`. The last name of a page is the cursor for the next one, and `Vec::is_empty` ends the
	// enumeration.
	//
	// `read_dir` materialises the whole directory - every entry, an inode read each - which is the
	// wrong shape for a tree built to hold millions. This does the work of a page: the subtrees
	// entirely before the cursor are never read and the walk stops when the page is full. `read_dir`
	// stays for the callers that genuinely want everything at once (and for the ones that cannot
	// hold a cursor across a call).
	pub fn read_dir_page(&mut self, path: &[u8], after: Option<&[u8]>, max: usize) -> Result<Vec<(Vec<u8>, u64, bool, u64, u64)>, FsError> {
		let inode_num = self.resolve(path)?;
		self.read_dir_page_inode(inode_num, after, max)
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
				if inode.r#type != TYPE_FILE {
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
		let mut inode = Inode::empty(TYPE_FILE);
		inode.size = data.len() as u64;
		inode.ctime = match &old {
			Some((_, o)) => o.ctime,
			None => self.clock,
		};
		// the owner travels with the ctime, and for the same reason: overwriting a file's
		// CONTENTS does not make it a different file. This path builds a fresh inode
		// while the partial-write and truncate paths keep the existing one - so they kept
		// the tag and this one silently cleared it, against the documented contract.
		// Nothing consumes the tag yet, which is exactly how it would have stayed wrong.
		if let Some((_, o)) = &old {
			inode.owner_tag = o.owner_tag;
		}
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
		if self.read_inode(inode_num)?.r#type != TYPE_DIR {
			return Err(FsError::NotDir);
		}
		self.remove_inner(path)
	}

	pub(crate) fn remove_inner(&mut self, path: &[u8]) -> Result<(), FsError> {
		let (parent, name) = self.resolve_parent(path, false)?;
		let inode_num = self.dir_lookup(parent, name)?.ok_or(FsError::NotFound)?;
		// a dangling entry (its inode does not exist - a hostile or corrupt volume;
		// a legitimate writer commits entry and inode atomically) has nothing to drop
		// or free, but its NAME must be removable - this is the operator's repair verb
		// for what fsck names, and without it the only remedy is a reformat.
		let inode = match self.read_inode(inode_num) {
			Ok(inode) => Some(inode),
			Err(FsError::Invalid) => None,
			Err(e) => return Err(e),
		};
		if let Some(inode) = &inode {
			// `size` is a cached count and the tree is the fact. A directory that claims
			// to be empty and is not would otherwise have its tree freed while its
			// children stayed in the inode table, reachable by nothing.
			if inode.r#type == TYPE_DIR && (inode.size != 0 || self.dir_has_entries(inode)?) {
				return Err(FsError::NotEmpty);
			}

			// clear the directory entry and free the inode in the new generation; its old
			// blocks remain referenced by the previous generation and leave the new one.
			self.drop_deleted_inode(inode)?;
		}
		self.dir_remove(parent, name)?;
		if inode.is_some() {
			self.free_inode(inode_num)?;
		}
		Ok(())
	}

	// Record every block a deleted inode references as dropped: a file's data blocks
	// and extent chain, or - defensively - the tree nodes of a directory whose root is
	// non-zero despite the empty size the caller verified (damaged or hostile; a
	// legitimate empty directory's root is 0). Shared by the delete and the
	// rename-replace paths, so neither can leak what the other drops.
	//
	// "File-shaped unless it is a directory", which is the rule `Inode::parse` and the
	// mark walk already use. The test was `== TYPE_FILE`, and an unknown type byte is
	// neither that nor a directory with a root (`parse` fills `dir_root` only for
	// TYPE_DIR, so it is zero) - so removing such an inode dropped NOTHING while the
	// mark walk reserved its data, checksum and spill blocks. They then stayed marked
	// used for the life of the mount, and the repair verb that cleared the record did
	// not bring the space back with it.
	pub(crate) fn drop_deleted_inode(&mut self, inode: &Inode) -> Result<(), FsError> {
		if inode.r#type != TYPE_DIR {
			self.drop_inode_blocks(inode)?;
		} else if inode.dir_root != 0 {
			// mark-then-scan is O(pool) per call - accepted: this path runs only for a
			// damaged directory (never on a healthy volume), and the marked map is the
			// one exact answer to "which blocks does this tree hold".
			let mut map = try_zeroed(self.free.len())?;
			self.mark_dir_tree(inode.dir_root, inode.dir_root_crc, &mut map)?;
			for b in 0..self.num_blocks {
				if test_bit(&map, b) {
					self.drop_block(b);
				}
			}
		}
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

// Bytes of bitmap for a pool of `num_blocks`, refusing a volume this build cannot hold a
// map for.
fn free_map_len(num_blocks: u64) -> Result<usize, FsError> {
	if num_blocks > MAX_BLOCKS {
		return Err(FsError::Invalid);
	}
	usize::try_from(num_blocks).map(|n| n.div_ceil(8)).map_err(|_| FsError::Invalid)
}

// The mount's answer to an error that is not one of the ones it handles by name.
fn map_mount_error(e: FsError) -> MountError {
	match e {
		FsError::Io => MountError::Io,
		_ => MountError::Corrupt,
	}
}

// Parse and validate a superblock block: it must carry the LiberFS magic and version,
// match this build's block size, feature flags, and algorithm ids, and pass its own
// CRC32C. Returns None otherwise (an unformatted slot, a foreign disk, a torn commit,
// or a volume laid down by a build with a different layout or algorithms - which the
// flags catch instead of a silent mis-parse).
//
// The mount path reads slots through `read_slot` directly, because it needs to know WHICH
// way a slot failed; this collapses that back to yes-or-no for the tests, which mostly
// want the fields out of a slot they know is good.
#[cfg(test)]
pub(crate) fn parse_superblock(block: &[u8]) -> Option<Superblock> {
	match read_slot(block) {
		SlotRead::Valid(sb) => Some(sb),
		_ => None,
	}
}

// What a superblock slot turned out to be.
//
// The distinction exists because the CALLER formats. Collapsing "there is no filesystem here" into
// the same answer as "this disk did not answer" or "this was written by a build we cannot read"
// means a transient fault and an unsupported version both look like a blank disk, and the volume
// is reformatted. Every arm below is a different decision about someone's data.
pub(crate) enum SlotRead {
	Valid(Superblock),
	// No magic: a blank slot, or a disk that is not ours.
	Unformatted,
	// Our magic, and a version, block size, feature set or algorithm this build cannot read.
	Unsupported,
	// Ours and readable, and it failed its own checks - a torn commit, bit rot, a claim the
	// layout cannot support.
	Corrupt,
}

pub(crate) fn read_slot(block: &[u8]) -> SlotRead {
	if block.len() < BLOCK_SIZE {
		return SlotRead::Corrupt;
	}
	if block[SB_MAGIC_OFF..SB_MAGIC_OFF + 8] != MAGIC {
		return SlotRead::Unformatted;
	}
	// Past this point the disk IS ours, so nothing below may report it as unformatted.
	let field = |off: usize, len: usize| -> Option<u64> {
		match len {
			4 => block[off..off + 4].try_into().ok().map(|b| u64::from(u32::from_le_bytes(b))),
			_ => block[off..off + 8].try_into().ok().map(u64::from_le_bytes),
		}
	};
	let (Some(version), Some(block_size), Some(features)) = (field(SB_VERSION_OFF, 4), field(SB_BLOCK_SIZE_OFF, 4), field(SB_FEATURES_OFF, 8)) else {
		return SlotRead::Corrupt;
	};
	if version != u64::from(VERSION) || block_size != BLOCK_SIZE as u64 || features != FEATURES {
		return SlotRead::Unsupported;
	}
	if block[SB_CSUM_ALGO_OFF] != CSUM_ALGO_CRC32C || block[SB_CODEC_OFF] != CODEC_LZ4 {
		return SlotRead::Unsupported;
	}
	match parse_checked(block) {
		Some(sb) => SlotRead::Valid(sb),
		None => SlotRead::Corrupt,
	}
}

// The rest of the parse, once the slot is known to be ours and readable by this build.
fn parse_checked(block: &[u8]) -> Option<Superblock> {
	// verify the self-CRC by recomputing over the block with its CRC bytes zeroed.
	let stored = u32::from_le_bytes(block[SB_CRC_OFFSET..SB_CRC_OFFSET + 4].try_into().ok()?);
	let mut probe = block[..BLOCK_SIZE].to_vec();
	probe[SB_CRC_OFFSET..SB_CRC_OFFSET + 4].fill(0);
	if crc32c(&probe) != stored {
		return None;
	}
	// a CRC proves integrity, not sanity: a pool smaller than the fixed layout can
	// underflow the mount arithmetic, so reject it here (the format refuses to create
	// one; the upper bound is checked at mount by probing the device itself).
	let num_blocks = u64::from_le_bytes(block[SB_NUM_BLOCKS_OFF..SB_NUM_BLOCKS_OFF + 8].try_into().ok()?);
	if num_blocks <= POOL_START + 1 {
		return None;
	}
	// A CRC proves integrity, not sense, and the fields were only ever checked one at a
	// time. What follows is the relations BETWEEN them, as far as they can be settled
	// without reading anything - the two that need the tree are settled at mount.
	let inode_root = u64::from_le_bytes(block[SB_INODE_ROOT_OFF..SB_INODE_ROOT_OFF + 8].try_into().ok()?);
	let snap_root = u64::from_le_bytes(block[SB_SNAP_ROOT_OFF..SB_SNAP_ROOT_OFF + 8].try_into().ok()?);
	let next_inode = u32::from_le_bytes(block[SB_NEXT_INODE_OFF..SB_NEXT_INODE_OFF + 4].try_into().ok()?);
	let root_inode = u32::from_le_bytes(block[SB_ROOT_INODE_OFF..SB_ROOT_INODE_OFF + 4].try_into().ok()?);
	let generation = u64::from_le_bytes(block[SB_GENERATION_OFF..SB_GENERATION_OFF + 8].try_into().ok()?);
	// every formatted volume has a root leaf, so 0 is not a root, and a root outside the
	// pool cannot be read at all.
	if inode_root == 0 || inode_root >= num_blocks {
		return None;
	}
	// the snapshot chain may legitimately be absent; it may not be outside the pool.
	if snap_root >= num_blocks {
		return None;
	}
	// `next_inode` hands out numbers ABOVE everything in use, and the root directory is
	// always in use - so a counter at or below it names something that already exists,
	// and `alloc_inode` would hand it out for a new file to take over.
	if next_inode <= root_inode {
		return None;
	}
	// the generation is incremented at every commit, and the increment is unchecked.
	if generation == u64::MAX {
		return None;
	}
	// The compression flag is written as 0 or 1 and was read as `!= 0`, so 2 and 255 were also
	// true. Nothing goes wrong today; a byte the writer can never produce is a byte the parser
	// should not accept, because the day it means something else it will already have been mounted.
	if block[SB_COMPRESS_OFF] > 1 {
		return None;
	}
	let mut uuid = [0u8; 16];
	uuid.copy_from_slice(&block[SB_UUID_OFF..SB_UUID_OFF + 16]);
	let mut label = [0u8; LABEL_MAX];
	label.copy_from_slice(&block[SB_LABEL_OFF..SB_LABEL_OFF + LABEL_MAX]);
	// The write side refuses a label that is not UTF-8 or that carries a NUL; the read
	// side accepted anything, so a volume authored elsewhere produced a `label()` that was
	// not what the record held. `name_in` is what `label()` returns, so that is what has
	// to be text - the padding after it is not part of the name.
	let name = name_in(&label).len();
	if core::str::from_utf8(&label[..name]).is_err() {
		return None;
	}
	// and the field AFTER the terminator has to be the zero padding the writer lays down.
	// Nothing reads those bytes, so this is not about what `label()` returns; it is that
	// `system\0\xff\xffgarbage` is not something any writer of this format produces, and
	// a field shaped like that is itself evidence the record was written by something
	// else. Refusing costs nothing and turns a provably foreign image away at the door.
	if label[name..].iter().any(|&b| b != 0) {
		return None;
	}
	Some(Superblock { num_blocks, generation, inode_root, inode_root_crc: u32::from_le_bytes(block[SB_INODE_ROOT_CRC_OFF..SB_INODE_ROOT_CRC_OFF + 4].try_into().ok()?), next_inode, root_inode, snap_root, snap_root_crc: u32::from_le_bytes(block[SB_SNAP_ROOT_CRC_OFF..SB_SNAP_ROOT_CRC_OFF + 4].try_into().ok()?), uuid, label, compress: block[SB_COMPRESS_OFF] != 0 })
}
