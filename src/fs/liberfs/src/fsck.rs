use crate::*;

// What a structure pass found, by KIND. The report separates checksum, structural and stream
// failures on purpose - they send an operator to three different places, the medium, the metadata
// or the writer that produced the stream - and a single count plus a subtraction could not keep
// them apart.
#[derive(Debug, Default)]
pub(crate) struct StructureTally {
	// Faults in the shape of the metadata: a record that cannot be true.
	pub structural: u32,
	// A compressed extent whose stored blocks do not match their checksums. Already counted by the
	// scrub pass; named here so the operator learns which extent, and not added twice.
	pub checksum: u32,
	// The medium would not give the bytes back. Not a fault of the filesystem.
	pub io: u32,
	// Every block came back as written and the shape is legal, and the stream still does not
	// decode.
	pub stream: u32,
}

// Which tally an error belongs in, so a failure the MEDIUM caused is never counted as damage to the
// filesystem's shape.
//
// Three reports in `check_structure` wrote plain text and incremented nothing: the root inode, the
// snapshot table re-read and the inode-tree walk. So an unreadable disc, a checksum mismatch and a
// genuinely impossible record produced the same note and the same (zero) counts - and the caller
// that decides whether a volume is damaged or merely unreachable had nothing to decide on. This
// crate has drawn that distinction everywhere else since its own audit; the structure pass had not.
fn count_by_cause(error: FsError, tally: &mut StructureTally) {
	match error {
		FsError::Io => tally.io += 1,
		FsError::Corrupt => tally.checksum += 1,
		_ => tally.structural += 1,
	}
}

impl<D: BlockDevice> LiberFs<D> {
	// Verify integrity. With copy-on-write a crash cannot leak blocks or orphan an
	// inode (the free map is derived and a commit is atomic), so there is nothing to
	// reclaim; what fsck does is walk the live namespace, check every file's data
	// blocks against their stored checksums, and NAME the damaged files - a count
	// alone would leave the operator knowing something is wrong but not what. The
	// pinned snapshot generations are verified too - inode trees, directory trees and
	// file data (counted; their files are named under the snapshot's own mount, not
	// here). Damage is REPORTED, never fatal - a corrupt node and an unreadable block
	// alike count as failures (named by path where one is known) and the walk
	// continues, so one bad block cannot silence the report of everything else. The
	// free map is rederived too (a no-op on a consistent volume); damage found THERE
	// additionally degrades the volume to read-only, the mount's own policy - the map
	// is incomplete, so no later allocation may trust it (a remount after repair
	// restores writes).
	pub fn fsck(&mut self) -> Result<FsckReport, FsError> {
		// verify the DISK, not the caches: a cached inode would skip its tree-path and
		// spill-chain verification, and a cached checksum block its re-read - damage
		// behind a warm cache would escape the report.
		self.icache.clear();
		self.dcache.clear();
		self.rcsum = None;
		self.decomp.clear();
		let mut checksum_failures = 0u32;
		// THE LIVE WALK'S OWN COUNTS, kept beside the checksum one it used to fold them into. The
		// structure pass and the snapshot recursion each have a tally; the live namespace walk had
		// only this one number, which is why a failed read and a dangling entry both read as
		// checksum mismatches.
		let mut live_io = 0u32;
		let mut live_structural = 0u32;
		let mut damaged: Vec<Vec<u8>> = Vec::new();
		// THE FREE-MAP WALK'S DAMAGE, IN THE BUCKET IT BELONGS TO. This was
		// `Err(FsError::Corrupt) => checksum_failures += 1`, so a device that did not answer, a
		// block that is not a node of the tree, a leaf outside the interval that routes to it, an
		// inode with two names and two owners of one block were all reported as checksum failures -
		// in the one pass that visits every block of the live generation, and therefore the pass
		// most likely to be the first to see anything. `derive_free_kinds` carries the categories
		// out; each is counted once, because the walk reports whether it found a kind and not how
		// many times.
		let walk = self.derive_free_kinds()?;
		if walk.any() {
			self.read_only = true;
		}
		if walk.io {
			live_io = live_io.saturating_add(1);
		}
		if walk.checksum {
			checksum_failures = checksum_failures.saturating_add(1);
		}
		if walk.structural {
			live_structural = live_structural.saturating_add(1);
		}
		// walk the live namespace from the root, tracking each file's full path (the
		// root directory itself reports as "/"). The visited set makes a hostile
		// namespace - a cycle, or many names aliasing one subtree - terminate instead
		// of looping or blowing up.
		let mut stack: Vec<(u32, Vec<u8>)> = vec![(self.root_inode, Vec::new())];
		let mut seen: BTreeSet<u32> = BTreeSet::new();
		seen.insert(self.root_inode);
		while let Some((dir, prefix)) = stack.pop() {
			let entries = match self.dir_entries_of(dir) {
				Ok(entries) => entries,
				// THREE DIFFERENT FAULTS, COUNTED SEPARATELY - which they were not until 2026-08-15,
				// when all three landed in `checksum_failures`.
				//
				// The report grew an `io_failures` field for the snapshot walk, and the live walk
				// kept folding a failed READ into the checksum count. So the same disk not
				// answering was an I/O fault when the snapshot recursion met it and a checksum
				// mismatch when the live namespace did - and an operator reading the report cannot
				// tell a failing medium from a corrupted one, which is the first question they have.
				//
				// `Invalid` is a dangling walk target: a directory entry naming an inode that does
				// not exist. That is the NAMESPACE being wrong, not the bytes, and it is what
				// `structural_failures` counts.
				Err(FsError::Io) => {
					live_io = live_io.saturating_add(1);
					damaged.push(if prefix.is_empty() { b"/".to_vec() } else { prefix });
					continue;
				}
				Err(FsError::Corrupt) => {
					checksum_failures = checksum_failures.saturating_add(1);
					damaged.push(if prefix.is_empty() { b"/".to_vec() } else { prefix });
					continue;
				}
				Err(FsError::Invalid) => {
					live_structural = live_structural.saturating_add(1);
					damaged.push(if prefix.is_empty() { b"/".to_vec() } else { prefix });
					continue;
				}
				Err(e) => return Err(e),
			};
			for (name, child) in entries {
				let mut path = prefix.clone();
				if !path.is_empty() {
					path.push(b'/');
				}
				path.extend_from_slice(&name);
				let checked = self.read_inode(child).and_then(|inode| {
					if inode.r#type == TYPE_DIR {
						if seen.insert(child) {
							stack.push((child, path.clone()));
						}
						Ok(0)
					} else {
						self.count_corrupt(&inode)
					}
				});
				// Every one of these is damage the operator is shown the path of; which COUNTER it
				// lands in is the diagnosis, and the three are different diagnoses - see above.
				let bad = match checked {
					Ok(bad) => bad,
					Err(FsError::Io) => {
						live_io = live_io.saturating_add(1);
						damaged.push(path);
						continue;
					}
					Err(FsError::Invalid) => {
						live_structural = live_structural.saturating_add(1);
						damaged.push(path);
						continue;
					}
					Err(FsError::Corrupt) => 1,
					Err(e) => return Err(e),
				};
				if bad > 0 {
					// saturating: a count past u32 reads as "beyond counting", which such
					// a volume is - never an overflow in the report's own arithmetic.
					checksum_failures = checksum_failures.saturating_add(bad);
					damaged.push(path);
				}
			}
		}
		// every pinned snapshot generation is part of the live volume: verify its
		// blocks too, so corruption in a snapshot is reported and the walk accounts
		// for it.
		// one visited map for every tree walked below: a block belongs to one node of one
		// tree, and verifying it a second time reports nothing new. Snapshots sharing
		// subtrees with each other is the normal case, so this is also most of the work
		// saved on a healthy volume.
		let mut visited = try_zeroed(self.free.len())?;
		// ONE TALLY FOR THE SNAPSHOT PASS, reported into the same three counters the live pass uses.
		// `check_inode_tree` returned a bare `u32` and the caller added it to `checksum_failures`,
		// so every fault a snapshot could have - a medium that would not answer, a shape that cannot
		// be true, a stream that does not decode - was reported as the medium.
		let mut snapshot_tally = StructureTally::default();
		for i in 0..self.snapshots.len() {
			let (root, crc) = (self.snapshots[i].inode_root, self.snapshots[i].inode_root_crc);
			checksum_failures = checksum_failures.saturating_add(match self.check_inode_tree(root, crc, TREE_DEPTH_MAX, &mut visited, &mut snapshot_tally) {
				Ok(bad) => bad,
				// The ROOT's own damage, which the walk cannot classify from inside.
				Err(FsError::Io) => {
					snapshot_tally.io = snapshot_tally.io.saturating_add(1);
					0
				}
				Err(FsError::Corrupt) => 1,
				Err(e) => return Err(e),
			});
		}
		// the structural pass: shapes that no checksum can object to.
		let mut faults: Vec<Vec<u8>> = Vec::new();
		// A compressed stream that does not decode is neither: every block came back exactly as
		// written and the shape is legal, and yet the file cannot be read. Counted apart because
		// the three send an operator to three different places - the medium, the metadata, or the
		// writer that produced the stream.
		let tally = self.check_structure(&mut faults);
		// Every fault raised by the structure pass that is not one of the three named kinds IS a
		// structural one; counting them as "the rest" here rather than at each `note` keeps the
		// arithmetic in one place while the three that are NOT structural are counted where they
		// are raised.
		let named = tally.checksum + tally.io + tally.stream + tally.structural;
		let structural_failures = (faults.len() as u32 - named) + tally.structural + snapshot_tally.structural + live_structural;
		Ok(FsckReport {
			checksum_failures,
			damaged,
			structural_failures,
			stream_failures: tally.stream + snapshot_tally.stream,
			// REPORTED, from both halves. `snapshot_tally.io` was maintained and never read.
			io_failures: tally.io + snapshot_tally.io + live_io,
			faults,
		})
	}

	// Read the whole file at `path` out of the named snapshot's pinned generation,
	// without mounting a second filesystem: a table lookup re-roots the read through
	// `with_root`, so the cost is the file's, not a volume walk. The one-file read
	// behind the service's snap-open.
	pub fn read_file_from_snapshot(&mut self, snapshot: &[u8], path: &[u8]) -> Result<Vec<u8>, FsError> {
		let snap = self.snapshots.iter().find(|s| s.name == snapshot).ok_or(FsError::NotFound)?;
		let (root, crc) = (snap.inode_root, snap.inode_root_crc);
		self.with_root(root, crc, |fs| fs.read_file(path))
	}

	// Copy the file at `path` out of a pinned generation into the live tree: the
	// recovery verb for a file fsck named. `snapshot` picks a named snapshot; an empty
	// name picks the previous generation (the rolling one-commit-back snapshot). The
	// restored bytes are the generation's version of the file - explicitly an older
	// version, the operator's call. Under copy-on-write the two generations usually
	// share the damaged block, so this heals only what the pinned generation still
	// holds intact (a block rewritten since diverged; a shared one is damaged in both).
	pub fn restore_file(&mut self, path: &[u8], snapshot: &[u8]) -> Result<(), FsError> {
		let (root, crc) = if snapshot.is_empty() {
			if !self.prev_valid {
				return Err(FsError::NotFound);
			}
			(self.prev_inode_root, self.prev_inode_root_crc)
		} else {
			let snap = self.snapshots.iter().find(|s| s.name == snapshot).ok_or(FsError::NotFound)?;
			(snap.inode_root, snap.inode_root_crc)
		};
		let data = self.with_root(root, crc, |fs| fs.read_file(path))?;
		self.write_file(path, &data)
	}

	// Run `f` with the inode tree re-rooted at (`root`, `crc`) - a read within a pinned
	// generation - then restore the live root. The caches are cleared on the way in and
	// out, since they describe whichever root is current.
	// Everything a checksum cannot see. Each fault is recorded and the pass CONTINUES,
	// so one broken inode does not hide the rest - the caller gets the whole list, and an
	// empty list is the only clean answer.
	//
	// This is deliberately separate from the checksum scrub above. The two answer
	// different questions - "did the medium give back what was written" versus "can what
	// was written be true" - and an operator needs to know which one failed.
	pub(crate) fn check_structure(&mut self, faults: &mut Vec<Vec<u8>>) -> StructureTally {
		// COUNTED WHERE THE FAULT IS RAISED, not subtracted afterwards.
		//
		// This returned only the stream count and the report computed
		// `structural_failures = faults.len() - stream_failures`, so every fault that was neither
		// structural nor a stream landed in `structural_failures` anyway. A compressed extent whose
		// blocks fail their checksums was therefore counted TWICE - once by the scrub pass into
		// `checksum_failures`, and once again here - and an extent that could not be READ was
		// reported as a structural fault of the filesystem. The subtraction is also what would make
		// a fifth kind of fault silently wrong, which is the part worth removing.
		let mut tally = StructureTally::default();
		let note = |what: &str, faults: &mut Vec<Vec<u8>>| faults.push(what.as_bytes().to_vec());

		// the root of the namespace has to be a directory, or no path resolves.
		match self.read_inode(self.root_inode) {
			Ok(inode) if inode.r#type == TYPE_DIR => {}
			Ok(_) => note("root inode is not a directory", faults),
			Err(error) => {
				count_by_cause(error, &mut tally);
				note("root inode cannot be read", faults);
			}
		}

		// the snapshot table as it is ON DISK, not as the mount remembers it. Nothing
		// re-read it, so a table that had drifted from memory since the mount - or one
		// whose records were impossible - went unreported by the very tool asked to check.
		let (root, crc) = (self.snap_root, self.snap_root_crc);
		match self.read_snapshot_table(root, crc, &mut |_| {}) {
			Ok(disk) => {
				// Every field of the record, `inode_root_crc` included. It was left out, and it is
				// the one that decides whether the pinned root can be READ: a table whose stored
				// CRC has drifted from the loaded one names a generation that will refuse to open,
				// which is exactly the disagreement this comparison exists to find.
				if disk.len() != self.snapshots.len() || disk.iter().zip(self.snapshots.iter()).any(|(a, b)| a.name != b.name || a.inode_root != b.inode_root || a.inode_root_crc != b.inode_root_crc || a.generation != b.generation) {
					note("the snapshot table on disk differs from the mounted one", faults);
				}
			}
			Err(error) => {
				count_by_cause(error, &mut tally);
				note("the snapshot table cannot be re-read", faults);
			}
		}

		// every inode in the tree, checked as a shape and collected so the ones no name
		// reaches can be named.
		let mut inodes: BTreeSet<u32> = BTreeSet::new();
		let Ok(mut seen_blocks) = try_zeroed(self.free.len()) else {
			note("not enough memory to check the inode tree's structure", faults);
			return tally;
		};
		if let Err(error) = self.walk_inode_records(self.inode_root, self.inode_root_crc, TREE_DEPTH_MAX, &mut inodes, &mut seen_blocks, faults, &mut tally) {
			count_by_cause(error, &mut tally);
			note("the inode tree could not be walked to the end", faults);
		}

		// the directory trees, which the walk above reaches only as `dir_root` pointers.
		// The inode tree is checked strictly and directory trees were reached mostly
		// through `dir_leaf_parse`, which is deliberately tolerant: a record running past
		// the end of a block ends the parse and it returns what it managed to read. So a
		// truncated leaf produced a directory that lists part of itself, a `lookup` that
		// cannot find an entry which is on the medium, and an insert that then edits a
		// list already out of order - with nothing reporting a structural cause.
		for &num in inodes.iter() {
			let Ok(inode) = self.read_inode(num) else { continue };
			if inode.r#type == TYPE_DIR {
				self.check_dir_structure(num, &inode, faults);
			}
		}

		// reachability: every inode record should be findable from the root by name.
		// An orphan is live to the free-map walk and invisible to a namespace check,
		// which is exactly the pair that hides a whole subtree from an operator.
		let mut reached: BTreeSet<u32> = BTreeSet::new();
		let mut stack: Vec<u32> = vec![self.root_inode];
		reached.insert(self.root_inode);
		while let Some(dir) = stack.pop() {
			let entries = match self.dir_entries_of(dir) {
				Ok(entries) => entries,
				Err(_) => continue,
			};
			for (name, child) in entries {
				// ONE INODE, ONE NAME. There is no hardlink API and no link count, so that is the
				// format's rule - and nothing stated it. This `insert` was the cycle defence doing
				// double duty: a second reference to an already-reached inode was skipped rather
				// than reported, so an image with `/a` and `/b` both naming inode 7 passed `fsck`
				// clean, and `remove("a")` then freed inode 7's blocks and deleted it from the
				// inode tree while `/b` still pointed at it. On a directory it additionally makes
				// the namespace a graph.
				//
				// Skipping is still what the walk does - descending twice is the loop this defends
				// against - but it is now also a fault, which is the whole difference.
				if !reached.insert(child) {
					let mut fault = alloc::format!("inode {child} is named more than once; its second name is ").into_bytes();
					fault.extend_from_slice(&name);
					faults.push(fault);
					continue;
				}
				if matches!(self.read_inode(child), Ok(i) if i.r#type == TYPE_DIR) {
					stack.push(child);
				}
			}
		}
		// and the counter against the highest key the walk actually found.
		if let Some(&highest) = inodes.iter().next_back() {
			if self.next_inode <= highest {
				faults.push(alloc::format!("next_inode {} is not above the highest live inode {highest}", self.next_inode).into_bytes());
			}
		}
		let orphans = inodes.difference(&reached).count();
		if orphans != 0 {
			faults.push(alloc::format!("{orphans} inode record(s) reachable from no name").into_bytes());
		}
		tally
	}

	// Walk every leaf of the inode tree, checking the shape of each node and each record
	// it holds. Keys must ascend strictly within a leaf, a block must appear once, and
	// each file's extent map must be ordered, non-overlapping and as long as it claims.
	fn walk_inode_records(&mut self, ptr: u64, crc: u32, depth: usize, inodes: &mut BTreeSet<u32>, seen: &mut [u8], faults: &mut Vec<Vec<u8>>, tally: &mut StructureTally) -> Result<(), FsError> {
		self.walk_inode_records_in(ptr, crc, depth, (None, None), inodes, seen, faults, tally)
	}

	#[allow(clippy::too_many_arguments)]
	fn walk_inode_records_in(&mut self, ptr: u64, crc: u32, depth: usize, range: (Option<u64>, Option<u64>), inodes: &mut BTreeSet<u32>, seen: &mut [u8], faults: &mut Vec<Vec<u8>>, tally: &mut StructureTally) -> Result<(), FsError> {
		let (lower, upper) = range;
		if ptr == 0 {
			return Ok(());
		}
		if depth == 0 || ptr >= self.num_blocks {
			return Err(FsError::Corrupt);
		}
		if test_bit(seen, ptr) {
			faults.push(alloc::format!("block {ptr} appears twice in the inode tree").into_bytes());
			return Ok(());
		}
		set_bit(seen, ptr);
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node_raw(ptr, crc, &mut buf)?;
		if node_type(&buf) == NODE_LEAF {
			// The record count and the key order come from the function `tree_insert_node` and
			// `tree_delete_node` call before they binary-search this leaf, so the pass and the
			// write path cannot disagree about what a leaf has to be.
			if let Err(fault) = validate_fixed_leaf(&buf, INODE_REC, 8) {
				let mut message = b"inode ".to_vec();
				message.extend_from_slice(&fault.describe(ptr));
				faults.push(message);
			}
			for i in 0..leaf_count(&buf, INODE_REC) {
				let off = NODE_HDR + i * INODE_REC;
				let key = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
				// inside the interval the separators above route to this leaf, or the
				// record is on the medium and reachable by no lookup.
				if lower.is_some_and(|l| key < l) || upper.is_some_and(|u| key >= u) {
					faults.push(alloc::format!("inode leaf {ptr} holds a key routing will never reach").into_bytes());
				}
				if key > u32::MAX as u64 {
					faults.push(alloc::format!("inode leaf {ptr} holds a key outside the inode number range").into_bytes());
					continue;
				}
				inodes.insert(key as u32);
				let mut inode = Inode::parse(&buf[off + 8..off + 8 + INODE_SIZE]);
				if inode.r#type == TYPE_FILE {
					self.check_file_shape(key as u32, &mut inode, faults, tally);
				} else if inode.r#type != TYPE_DIR {
					// no writer emits one, so its presence is a fact worth naming even
					// though the free-map walk reserves its blocks conservatively.
					faults.push(alloc::format!("inode {key} has type {} which this build does not define", inode.r#type).into_bytes());
					self.check_file_shape(key as u32, &mut inode, faults, tally);
				}
			}
		} else {
			// Same shared rules the descent uses: counts, separator order, and a slot of an
			// internal node that points nowhere - an impossible shape, not an empty corner.
			if let Err(fault) = validate_internal(&buf) {
				let mut message = b"internal ".to_vec();
				message.extend_from_slice(&fault.describe(ptr));
				faults.push(message);
			}
			let count = internal_count(&buf);
			for i in 0..=count {
				let child_lower = if i == 0 { lower } else { Some(sep_key(&buf, i - 1)) };
				let child_upper = if i == count { upper } else { Some(sep_key(&buf, i)) };
				if child_ptr(&buf, i) == 0 {
					continue;
				}
				if self.walk_inode_records_in(child_ptr(&buf, i), child_crc(&buf, i), depth - 1, (child_lower, child_upper), inodes, seen, faults, tally).is_err() {
					faults.push(alloc::format!("subtree under {ptr} could not be walked").into_bytes());
				}
			}
		}
		Ok(())
	}

	// One directory's B+tree, checked as a structure rather than parsed for whatever it
	// yields: node types, record counts against what the header claims and against what
	// the block can hold, every record complete, `(hash, name)` strictly ascending, no
	// duplicate names, the stored hash equal to the name's own, names that are the text
	// the format says they are, separators ascending - and the inode's `size` equal to
	// the number of entries actually there.
	fn check_dir_structure(&mut self, num: u32, inode: &Inode, faults: &mut Vec<Vec<u8>>) {
		// a refused allocation used to `return`, so `fsck` checked nothing and reported
		// nothing and the structural report came back CLEAN for a directory it never
		// looked at. A report that cannot tell "checked and sound" from "not checked" is
		// worse than one that admits the difference, so the inability to check is itself
		// the fault - the same treatment the inode-tree pass above already gives it.
		let Ok(mut visited) = try_zeroed(self.free.len()) else {
			faults.push(alloc::format!("inode {num}: not enough memory to check this directory's structure").into_bytes());
			return;
		};
		// (block, crc, depth remaining, the half-open key interval this subtree is routed
		// for). `route_child` advances while `sep(i) <= key`, so child `i` holds keys in
		// `[sep(i-1), sep(i))` - lower inclusive, upper exclusive, and `None` at either
		// end is that end of the key space.
		//
		// This is what the other checks cannot see. Ordering within a leaf and ordering
		// among separators are both local; a leaf can be perfectly ordered and still hold
		// records that routing will never reach, because the separators above it send
		// their hashes down a different child. Those entries are on the medium, count
		// against the directory's size, and are findable by nothing.
		type Range = (Option<u64>, Option<u64>);
		let mut stack: Vec<(u64, u32, usize, Range)> = Vec::new();
		if inode.dir_root != 0 {
			stack.push((inode.dir_root, inode.dir_root_crc, TREE_DEPTH_MAX, (None, None)));
		}
		let mut buf = vec![0u8; BLOCK_SIZE];
		let mut entries: u64 = 0;
		let mut names: BTreeSet<Vec<u8>> = BTreeSet::new();
		while let Some((ptr, crc, left, (lower, upper))) = stack.pop() {
			if ptr == 0 {
				continue;
			}
			if left == 0 || ptr >= self.num_blocks {
				faults.push(alloc::format!("inode {num}: a directory node outside the pool or past the depth limit").into_bytes());
				continue;
			}
			if test_bit(&visited, ptr) {
				faults.push(alloc::format!("inode {num}: directory block {ptr} appears twice in one tree").into_bytes());
				continue;
			}
			set_bit(&mut visited, ptr);
			if self.read_node_raw(ptr, crc, &mut buf).is_err() {
				faults.push(alloc::format!("inode {num}: a directory node that does not match the checksum recorded for it").into_bytes());
				continue;
			}
			match node_type(&buf) {
				NODE_LEAF => {
					let recs = dir_leaf_parse(&buf);
					// The LOCAL invariants - truncation, ordering, the stored hash, the name
					// policy - come from the same function the write path calls before it edits a
					// leaf. Stated once and used twice, because they were stated twice and used
					// once: `fsck` knew them all and a writable mount checked the record count.
					if let Err(fault) = validate_dir_leaf(&buf, &recs) {
						let mut message = alloc::format!("inode {num}: directory ").into_bytes();
						message.extend_from_slice(&fault.describe(ptr));
						faults.push(message);
					}
					// and the ones only the WALK can answer: whether the separators above route
					// here, and whether a name appears elsewhere in this directory.
					for rec in recs {
						entries += 1;
						if lower.is_some_and(|l| rec.hash < l) || upper.is_some_and(|u| rec.hash >= u) {
							faults.push(alloc::format!("inode {num}: directory leaf {ptr} holds a record routing will never reach").into_bytes());
						}
						if !names.insert(rec.name.clone()) {
							faults.push(alloc::format!("inode {num}: a name appears twice in the directory").into_bytes());
						}
					}
				}
				NODE_INTERNAL => {
					if let Err(fault) = validate_internal(&buf) {
						let mut message = alloc::format!("inode {num}: directory ").into_bytes();
						message.extend_from_slice(&fault.describe(ptr));
						faults.push(message);
					}
					let count = internal_count(&buf);
					for i in 0..=count {
						let child_lower = if i == 0 { lower } else { Some(sep_key(&buf, i - 1)) };
						let child_upper = if i == count { upper } else { Some(sep_key(&buf, i)) };
						// A null child slot is not "nothing here". That reading is right for
						// an empty directory root and for a sentinel outside the tree, and
						// wrong for a slot of an internal node: the node routes a whole key
						// interval into each of its `count + 1` slots, so a slot pointing
						// nowhere makes every name in that interval resolve to nothing -
						// while the entry counts still agree and the checksums still verify,
						// which is exactly the shape a structural pass exists to catch.
						// `validate_internal` reports it; there is nothing below it to walk.
						if child_ptr(&buf, i) == 0 {
							continue;
						}
						stack.push((child_ptr(&buf, i), child_crc(&buf, i), left - 1, (child_lower, child_upper)));
					}
				}
				other => faults.push(alloc::format!("inode {num}: directory block {ptr} has node type {other}, which is not a node").into_bytes()),
			}
		}
		// `size` is a cached count and the tree is the fact; nothing compared them, so a
		// directory could claim any number - including one that overflows on the next
		// insert.
		if inode.size != entries {
			faults.push(alloc::format!("inode {num}: size says {} entries and the tree holds {entries}", inode.size).into_bytes());
		}
	}

	// One file's extent map: as long as the inode claims, ordered by logical offset,
	// non-overlapping, and every run individually possible.
	// How many of an inode's compressed extents do not decode.
	//
	// The live pass reports each one by name through `check_file_shape`; a snapshot generation has
	// no per-inode reporting and only needs the count. Both go through
	// `decompress_extent_detailed`, so whatever the live walk asks of an extent the snapshot walk
	// asks too - the difference between the two is which tree is being walked, not which checks
	// apply.
	fn count_undecodable(&mut self, inode: &Inode) -> u32 {
		let mut bad = 0u32;
		for i in 0..inode.extents.len() {
			let ext = &inode.extents[i];
			if ext.clen == 0 {
				continue;
			}
			if matches!(self.decompress_extent_detailed(ext), Err(crate::ExtentFault::Stream)) {
				bad = bad.saturating_add(1);
			}
		}
		bad
	}

	fn check_file_shape(&mut self, num: u32, inode: &mut Inode, faults: &mut Vec<Vec<u8>>, tally: &mut StructureTally) {
		if self.load_spill(inode).is_err() {
			faults.push(alloc::format!("inode {num}: the extent spill chain could not be read").into_bytes());
			return;
		}
		if inode.extents.len() != inode.extent_count as usize {
			faults.push(alloc::format!("inode {num}: the spill chain ends before the declared extent count").into_bytes());
		}
		// How many blocks the file's size can account for. Zero-length files map nothing.
		let blocks_in_file = inode.size.div_ceil(BLOCK_SIZE as u64);
		let mut end: Option<u64> = None;
		// INDEXED, not cloned. `inode.extents.clone()` is an infallible allocation sized by a
		// fragmented file's extent count - a number that comes off the medium - on the check whose
		// whole job is surviving a hostile image. `Extent` is `Copy`, so there was never a reason.
		for i in 0..inode.extents.len() {
			let ext = &inode.extents[i];
			if self.check_extent(ext).is_err() {
				faults.push(alloc::format!("inode {num}: an extent that cannot be true").into_bytes());
				continue;
			}
			if end.is_some_and(|e| ext.logical < e) {
				faults.push(alloc::format!("inode {num}: extents overlap or are out of order").into_bytes());
			}
			// `check_extent` bounds the PHYSICAL span and says nothing about `logical`, so a
			// run at the top of the address space passes it and then overflowed this sum -
			// a panic in debug, and in release a wrapped value that gets the overlap
			// comparison above wrong for every extent after it.
			let Some(next_end) = ext.logical.checked_add(ext.length as u64) else {
				faults.push(alloc::format!("inode {num}: an extent whose logical range leaves the address space").into_bytes());
				continue;
			};
			// AGAINST THE FILE'S END. Structure, ordering and overlap were all checked and the
			// extent was never compared with `inode.size`, so a file of 4096 bytes could carry an
			// extent mapped at logical block 1000: allocated, invisible in the file's contents, and
			// reserved for as long as the volume lives. No legitimate writer produces one. Sparse
			// holes are unaffected - this is about MAPPED blocks past the end.
			if next_end > blocks_in_file {
				faults.push(alloc::format!("inode {num}: an extent mapped past the end of the file").into_bytes());
			}
			// AND THE STREAM DECODES. `fsck` verified the checksum block's CRC and each stored
			// block's CRC and never ran the decoder, so an extent whose blocks all checksum
			// correctly and whose LZ stream is syntactically invalid gave "0 failures" here and
			// `Corrupt` on the first read. The decoded bytes are dropped immediately; one extent is
			// at most `CRCS_PER_BLOCK` blocks, so this is bounded whatever the file's size.
			if ext.clen != 0 {
				match self.decompress_extent_detailed(ext) {
					Ok(_) => {}
					Err(crate::ExtentFault::Stream) => {
						faults.push(alloc::format!("inode {num}: a compressed extent whose stream does not decode").into_bytes());
						tally.stream += 1;
					}
					// Already counted by the scrub pass. Reported here because the operator wants to
					// know WHICH extent, and not counted again because it is one fault.
					Err(crate::ExtentFault::Checksum) => {
						faults.push(alloc::format!("inode {num}: a compressed extent whose stored blocks do not match their checksums").into_bytes());
						tally.checksum += 1;
					}
					// The medium, not the filesystem.
					Err(crate::ExtentFault::Io) => {
						faults.push(alloc::format!("inode {num}: a compressed extent that could not be read").into_bytes());
						tally.io += 1;
					}
					Err(crate::ExtentFault::Shape(_)) => {
						faults.push(alloc::format!("inode {num}: a compressed extent whose record cannot be true").into_bytes());
						tally.structural += 1;
					}
				}
			}
			end = Some(next_end);
		}
	}

	pub(crate) fn with_root<R>(&mut self, root: u64, crc: u32, f: impl FnOnce(&mut Self) -> R) -> R {
		let saved = (self.inode_root, self.inode_root_crc);
		self.inode_root = root;
		self.inode_root_crc = crc;
		self.icache.clear();
		self.dcache.clear();
		// and the decompression cache, which was left alone: its contents describe
		// extents of whichever generation was live when they were decoded.
		self.decomp.clear();
		let r = f(self);
		self.inode_root = saved.0;
		self.inode_root_crc = saved.1;
		self.icache.clear();
		self.dcache.clear();
		self.decomp.clear();
		r
	}

	// Walk the inode B+tree, verifying every node against its stored checksum, and sum
	// the corrupt data blocks of every live file. Directory inodes get their own tree
	// walked and verified too, so a snapshot generation's directory damage is caught
	// here and not only when the snapshot is mounted. A corrupt subtree counts as a
	// failure and the walk continues; only the root's own damage surfaces as the error
	// (the caller counts it). The depth budget bounds the recursion against a hostile
	// chain of one-child internals.
	pub(crate) fn check_inode_tree(&mut self, ptr: u64, crc: u32, depth: usize, visited: &mut [u8], tally: &mut StructureTally) -> Result<u32, FsError> {
		if ptr == 0 {
			return Ok(0);
		}
		// A TREE TOO DEEP IS THE SHAPE BEING WRONG, NOT THE BYTES. This returned `Corrupt`, and the
		// caller counted `Corrupt` as a checksum failure - so an operator was told a block failed
		// its checksum when nothing had, and the actual fault (a tree deeper than the format
		// permits) was not reported at all.
		//
		// `FsError::Corrupt` is defined by fs-core as a checksum mismatch, and this crate was using
		// it for that AND for two structural faults. Counting the fault where it is RAISED, and
		// answering `Ok(0)` - no checksum failures below here - keeps the classification with the
		// code that knows what went wrong, which is the same shape `walk_inode_records` already
		// uses for the faults it names.
		if depth == 0 {
			tally.structural = tally.structural.saturating_add(1);
			return Ok(0);
		}
		// The raw marking walk carries a visited bitmap; this one carried only a depth
		// limit, which protects the stack and not the clock. A node with hundreds of
		// children all pointing at the next level's single node - with CRCs that agree,
		// which is buildable bottom-up and needs no cycle - is walked once per pointer per
		// level: fanout raised to the depth. Walking a SUBTREE twice tells nobody anything
		// the first pass did not, so the second descent is simply not made, and the walk is
		// bounded by the pool.
		//
		// The LINK is a different question, and the skip used to answer it too. `read_node`
		// verifies the block against the CRC32C recorded in the pointer that reached it, and
		// it sat after the visited test - so where two snapshots shared a block, only the
		// first snapshot's link was ever checked. A second snapshot pointing at the same
		// block with a wrong expected CRC cannot be opened at all, and fsck reported
		// nothing about it. Verifying first and skipping only the descent keeps the whole
		// saving (each block is walked once) and costs one read per shared edge - which is
		// linear in the tree's edges, never the exponential the bitmap was added to stop.
		// And a pointer outside the pool is the same kind of fault: the tree names a block this
		// volume does not have, which no checksum could ever have detected.
		if ptr >= self.num_blocks {
			tally.structural = tally.structural.saturating_add(1);
			return Ok(0);
		}
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node_raw(ptr, crc, &mut buf)?;
		if test_bit(visited, ptr) {
			return Ok(0);
		}
		set_bit(visited, ptr);
		// THE NODE TYPE, which this pass did not check at all.
		//
		// `read_node` checks the pointer bounds, the device read and the CRC, and nothing about
		// what the block IS; the branch below then tested `== NODE_LEAF` and treated EVERY other
		// value as an internal node. So a corrupted or forged type byte turned a leaf into an
		// internal node and its inode records into child pointers - inside the checker whose job is
		// to notice exactly that, walking whatever the block happened to hold.
		//
		// The raw marking walk in `derive_free` already requires the byte to be one of the two
		// values that exist ("a block that is not a node of the tree is damage in every
		// generation"); this is the same rule in the pass that REPORTS. Structural, because the
		// checksum matched: the medium gave back what was written and what was written cannot be a
		// node.
		let kind = node_type(&buf);
		if kind != NODE_LEAF && kind != NODE_INTERNAL {
			tally.structural = tally.structural.saturating_add(1);
			return Ok(0);
		}
		let mut bad = 0u32;
		if kind == NODE_LEAF {
			for i in 0..leaf_count(&buf, INODE_REC) {
				let off = NODE_HDR + i * INODE_REC + 8;
				let mut inode = Inode::parse(&buf[off..off + INODE_SIZE]);
				let checked = if inode.r#type == TYPE_FILE {
					// THE SAME EXTENT VALIDATION THE LIVE PASS DOES.
					//
					// This ended at `count_corrupt`, which asks the medium whether it gave back what
					// was written - physical checksums, nothing more. So a compressed extent held by
					// a pinned snapshot could have every CRC correct and a stream that does not
					// decode, and `fsck` called the volume clean while its own comment said the
					// pinned generations were verified. The data is unreadable and nothing says so
					// until somebody tries to read it, which for a snapshot may be after the live
					// copy is gone.
					// AND THE TWO ARE COUNTED APART, which folding them into one `u32` here undid.
					//
					// `count_undecodable` was added into the same number `count_corrupt` returns,
					// and the caller adds that number to `checksum_failures` - so a pinned snapshot
					// whose blocks all match their checksums and whose LZ stream does not decode was
					// reported as a MEDIUM fault. The report's whole purpose is keeping those three
					// causes apart, the live pass keeps them apart through `StructureTally`, and
					// `count_undecodable`'s own comment says the difference between the two walks is
					// which tree is being walked and not which checks apply. Then so is the
					// category.
					self.load_spill(&mut inode).and_then(|()| self.count_corrupt(&inode)).map(|bad| {
						let undecodable = self.count_undecodable(&inode);
						tally.stream = tally.stream.saturating_add(undecodable);
						bad
					})
				} else if inode.r#type == TYPE_DIR {
					self.check_dir_tree(inode.dir_root, inode.dir_root_crc, TREE_DEPTH_MAX, visited, tally)
				} else {
					Ok(0)
				};
				bad = bad.saturating_add(match checked {
					Ok(b) => b,
					// An unreadable node is the MEDIUM and a broken shape is the METADATA, and the
					// tally has a counter for each. `bad` keeps counting so the caller's existing
					// arithmetic is unchanged; what is new is that the reason is recorded.
					// `Corrupt` STAYS a checksum failure. It is tempting to read it as "a broken
					// shape" and file it under `structural`, and `fs-core` says otherwise in as
					// many words: `Corrupt` is "a block read back whose checksum did not match the
					// one stored beside its pointer". That is the medium, which is what
					// `checksum_failures` counts - so it goes on being counted through `bad`.
					//
					// `Io` is the one that was miscounted: the medium would not answer AT ALL,
					// which is not the same as answering wrongly, and the tally has a counter for
					// it.
					Err(FsError::Io) => {
						tally.io = tally.io.saturating_add(1);
						0
					}
					Err(FsError::Corrupt) => 1,
					Err(e) => return Err(e),
				});
			}
		} else {
			for i in 0..=internal_count(&buf) {
				bad = bad.saturating_add(match self.check_inode_tree(child_ptr(&buf, i), child_crc(&buf, i), depth - 1, visited, tally) {
					Ok(b) => b,
					// THE SAME SPLIT THE LEAF ARM MAKES, one level of recursion up. This was
					// `Err(Corrupt | Io) => 1`, so an I/O failure deep in a subtree was added to
					// `bad` and reported as a checksum failure - the exact miscategorisation the
					// round below it had just closed, at the recursion boundary.
					//
					// `Corrupt` STAYS a checksum failure: `fs-core` defines it as "a block read back
					// whose checksum did not match the one stored beside its pointer", which is the
					// medium answering wrongly. `Io` is the medium not answering at all.
					Err(FsError::Io) => {
						tally.io = tally.io.saturating_add(1);
						0
					}
					Err(FsError::Corrupt) => 1,
					Err(e) => return Err(e),
				});
			}
		}
		Ok(bad)
	}

	// Walk a directory B+tree verifying every node against the CRC32C its parent link
	// stored, counting corrupt subtrees like `check_inode_tree`; only the root's own
	// damage surfaces as the error (the caller counts it).
	pub(crate) fn check_dir_tree(&mut self, ptr: u64, crc: u32, depth: usize, visited: &mut [u8], tally: &mut StructureTally) -> Result<u32, FsError> {
		if ptr == 0 {
			return Ok(0);
		}
		// THE SAME THREE STRUCTURAL CASES THE INODE WALK ALREADY NAMES. These answered
		// `Err(FsError::Corrupt)`, which the caller counts into `bad` and the caller above that adds
		// to `checksum_failures` - so a tree deeper than the format permits, a pointer to a block
		// this volume does not have, and a block that is not a node were all reported as failed
		// checksums, when in each case every checksum involved matched.
		if depth == 0 {
			tally.structural = tally.structural.saturating_add(1);
			return Ok(0);
		}
		// same bound as the inode walk, sharing its map: a block is one node of one tree,
		// so a directory node reached twice - however many names alias it - is DESCENDED
		// into once. Its link is still verified every time, for the reason set out above:
		// the block's contents having been scrubbed says nothing about whether this
		// particular pointer records the right CRC for them.
		if ptr >= self.num_blocks {
			tally.structural = tally.structural.saturating_add(1);
			return Ok(0);
		}
		let mut buf = vec![0u8; BLOCK_SIZE];
		self.read_node_raw(ptr, crc, &mut buf)?;
		if test_bit(visited, ptr) {
			return Ok(0);
		}
		set_bit(visited, ptr);
		let kind = node_type(&buf);
		if kind != NODE_LEAF && kind != NODE_INTERNAL {
			tally.structural = tally.structural.saturating_add(1);
			return Ok(0);
		}
		let mut bad = 0u32;
		if kind == NODE_INTERNAL {
			for i in 0..=internal_count(&buf) {
				bad = bad.saturating_add(match self.check_dir_tree(child_ptr(&buf, i), child_crc(&buf, i), depth - 1, visited, tally) {
					Ok(b) => b,
					// As in `check_inode_tree`: the medium not answering is not the medium answering
					// wrongly, and the tally has a counter for each.
					Err(FsError::Io) => {
						tally.io = tally.io.saturating_add(1);
						0
					}
					Err(FsError::Corrupt) => 1,
					Err(e) => return Err(e),
				});
			}
		}
		Ok(bad)
	}
}
