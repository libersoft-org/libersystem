// Host tests for LiberFS, run with `cd src/liberfs && cargo test`. A Vec-backed block
// device stands in for the disk: a fresh device is formatted, exercised through the
// public API, and re-mounted to prove the on-disk state persists - the in-memory
// analog of surviving a reboot.

use super::*;

// A RAM-backed block device: one contiguous Vec of `num_blocks` blocks. Dropping and
// re-mounting from the same Vec models a reboot (the bytes persist, the in-memory
// filesystem state does not). Cloning models taking the same disk image two ways - a
// clean mount versus one of a crash-damaged copy.
#[derive(Clone)]
struct MemDevice {
	blocks: Vec<u8>,
}

impl MemDevice {
	fn new(num_blocks: u64) -> MemDevice {
		MemDevice { blocks: vec![0u8; num_blocks as usize * BLOCK_SIZE] }
	}
}

impl BlockDevice for MemDevice {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		let Some(start) = (index as usize).checked_mul(BLOCK_SIZE) else {
			return false;
		};
		let Some(src) = self.blocks.get(start..start + BLOCK_SIZE) else {
			return false;
		};
		buf[..BLOCK_SIZE].copy_from_slice(src);
		true
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		let Some(start) = (index as usize).checked_mul(BLOCK_SIZE) else {
			return false;
		};
		let Some(dst) = self.blocks.get_mut(start..start + BLOCK_SIZE) else {
			return false;
		};
		dst.copy_from_slice(&buf[..BLOCK_SIZE]);
		true
	}
}

const NBLOCKS: u64 = 64;

#[test]
fn format_then_mount_is_empty() {
	let dev = MemDevice::new(NBLOCKS);
	let fs = LiberFs::format(dev, NBLOCKS).unwrap();
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert!(fs.list().unwrap().is_empty());
	assert_eq!(fs.lookup(b"missing.txt"), None);
}

#[test]
fn mount_rejects_unformatted_device() {
	let dev = MemDevice::new(NBLOCKS);
	assert!(LiberFs::mount(dev).is_none());
}

#[test]
fn write_then_read_round_trips() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"hello.txt", b"Hello, world!").unwrap();
	assert_eq!(fs.read_file(b"hello.txt").unwrap(), b"Hello, world!");
	let listing = fs.list().unwrap();
	assert_eq!(listing.len(), 1);
	assert_eq!(listing[0].0, b"hello.txt");
	assert_eq!(listing[0].1, 13);
}

#[test]
fn data_survives_a_remount() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"motd.txt", b"persist me").unwrap();
	fs.write_file(b"a", b"first").unwrap();
	let dev = fs.into_device();

	// re-mount from the same bytes: the files are still there (a "reboot").
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"motd.txt").unwrap(), b"persist me");
	assert_eq!(fs.read_file(b"a").unwrap(), b"first");
	assert_eq!(fs.list().unwrap().len(), 2);
}

#[test]
fn overwrite_replaces_contents() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"short").unwrap();
	fs.write_file(b"f", b"a much longer replacement payload").unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"a much longer replacement payload");
	// still one entry - overwrite reused the inode.
	assert_eq!(fs.list().unwrap().len(), 1);
}

#[test]
fn remove_deletes_and_frees() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"gone.txt", b"temporary").unwrap();
	fs.remove(b"gone.txt").unwrap();
	assert_eq!(fs.lookup(b"gone.txt"), None);
	assert_eq!(fs.read_file(b"gone.txt"), Err(FsError::NotFound));
	assert_eq!(fs.remove(b"gone.txt"), Err(FsError::NotFound));

	// the freed blocks and inode are reusable: many create/delete cycles do not run
	// the filesystem out of space.
	for _ in 0..200 {
		fs.write_file(b"churn", b"reuse the same slot").unwrap();
		fs.remove(b"churn").unwrap();
	}
	assert!(fs.list().unwrap().is_empty());
}

#[test]
fn multi_block_file_round_trips() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let big: Vec<u8> = (0..(BLOCK_SIZE * 3 + 7)).map(|i| (i % 251) as u8).collect();
	fs.write_file(b"big.bin", &big).unwrap();
	assert_eq!(fs.read_file(b"big.bin").unwrap(), big);
}

#[test]
fn empty_file_round_trips() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"empty", b"").unwrap();
	assert_eq!(fs.read_file(b"empty").unwrap(), b"");
	assert_eq!(fs.list().unwrap()[0].1, 0);
}

#[test]
fn rejects_too_long_a_name() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let long = vec![b'x'; NAME_MAX + 1];
	assert_eq!(fs.write_file(&long, b"data"), Err(FsError::TooLong));
}

#[test]
fn reports_out_of_space() {
	// a tiny filesystem: too few data blocks for an oversized file.
	let small: u64 = 6;
	let mut fs = LiberFs::format(MemDevice::new(small), small).unwrap();
	let payload = vec![b'z'; BLOCK_SIZE * 5];
	assert_eq!(fs.write_file(b"toobig", &payload), Err(FsError::NoSpace));
}

#[test]
fn many_small_files_fill_the_directory() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	for i in 0..10u8 {
		let name = [b'f', b'0' + i];
		fs.write_file(&name, b"x").unwrap();
	}
	assert_eq!(fs.list().unwrap().len(), 10);
	for i in 0..10u8 {
		let name = [b'f', b'0' + i];
		assert_eq!(fs.read_file(&name).unwrap(), b"x");
	}
}

// M49: nested directories and capacity scaling.

// A sparse RAM device backed by a map: only written blocks cost memory, so a huge
// volume can be formatted in a test without allocating it whole.
struct SparseDevice {
	blocks: std::collections::HashMap<u64, Vec<u8>>,
	num_blocks: u64,
}

impl SparseDevice {
	fn new(num_blocks: u64) -> SparseDevice {
		SparseDevice { blocks: std::collections::HashMap::new(), num_blocks }
	}
}

impl BlockDevice for SparseDevice {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		if index >= self.num_blocks {
			return false;
		}
		match self.blocks.get(&index) {
			Some(b) => buf[..BLOCK_SIZE].copy_from_slice(b),
			None => buf[..BLOCK_SIZE].fill(0),
		}
		true
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		if index >= self.num_blocks {
			return false;
		}
		self.blocks.insert(index, buf[..BLOCK_SIZE].to_vec());
		true
	}
}

#[test]
fn nested_directories_resolve_and_list() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"a/b/c").unwrap();
	fs.write_file(b"a/b/c/file.txt", b"deep").unwrap();
	assert_eq!(fs.read_file(b"a/b/c/file.txt").unwrap(), b"deep");
	// every directory level resolves.
	assert!(fs.lookup(b"a").is_some());
	assert!(fs.lookup(b"a/b").is_some());
	assert!(fs.lookup(b"a/b/c").is_some());
	// listing a nested directory shows its child.
	let entries = fs.read_dir(b"a/b/c").unwrap();
	assert_eq!(entries.len(), 1);
	assert_eq!(entries[0].0, b"file.txt");
	// the file reports as a regular file, not a directory.
	assert!(!entries[0].2);
	// the root shows only the top-level directory.
	let root = fs.list().unwrap();
	assert_eq!(root.len(), 1);
	assert_eq!(root[0].0, b"a");
	// the entry reports as a directory.
	assert!(root[0].2);
}

#[test]
fn rmdir_removes_an_empty_directory_only() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"empty").unwrap();
	fs.mkdir(b"full").unwrap();
	fs.write_file(b"full/f", b"x").unwrap();
	fs.write_file(b"file", b"y").unwrap();
	// a non-empty directory is refused.
	assert_eq!(fs.rmdir(b"full"), Err(FsError::NotEmpty));
	// a regular file is refused (use remove).
	assert_eq!(fs.rmdir(b"file"), Err(FsError::NotDir));
	// an empty directory is removed.
	assert!(fs.rmdir(b"empty").is_ok());
	assert!(fs.lookup(b"empty").is_none());
}

#[test]
fn write_creates_missing_parents() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	// no explicit mkdir: write auto-creates the parent chain.
	fs.write_file(b"docs/notes/today.txt", b"hello").unwrap();
	assert_eq!(fs.read_file(b"docs/notes/today.txt").unwrap(), b"hello");
	assert!(fs.lookup(b"docs/notes").is_some());
}

#[test]
fn nested_paths_survive_a_remount() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"etc/motd", b"welcome").unwrap();
	fs.mkdir(b"var/log").unwrap();
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"etc/motd").unwrap(), b"welcome");
	assert!(fs.lookup(b"var/log").is_some());
}

#[test]
fn remove_rejects_a_nonempty_directory() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"dir/child", b"x").unwrap();
	assert_eq!(fs.remove(b"dir"), Err(FsError::NotEmpty));
	// removing the child then the now-empty directory works.
	fs.remove(b"dir/child").unwrap();
	fs.remove(b"dir").unwrap();
	assert_eq!(fs.lookup(b"dir"), None);
}

#[test]
fn rejects_dot_and_dot_dot_segments() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	assert_eq!(fs.write_file(b"a/../b", b"x"), Err(FsError::BadName));
	assert_eq!(fs.read_file(b"./x"), Err(FsError::BadName));
	assert_eq!(fs.mkdir(b"x//y"), Err(FsError::BadName));
}

#[test]
fn many_files_across_multiple_inode_blocks() {
	// a volume holding far more files than one inode-tree leaf, so the inode B+tree grows
	// past a single node.
	let nblocks: u64 = 400;
	let mut fs = LiberFs::format(MemDevice::new(nblocks), nblocks).unwrap();
	let count = 100u32;
	for i in 0..count {
		let name = format!("file{i}");
		fs.write_file(name.as_bytes(), name.as_bytes()).unwrap();
	}
	assert_eq!(fs.list().unwrap().len() as u32, count);
	for i in 0..count {
		let name = format!("file{i}");
		assert_eq!(fs.read_file(name.as_bytes()).unwrap(), name.as_bytes());
	}
}

#[test]
fn a_large_volume_formats_and_round_trips() {
	// the free map is derived, so it scales to a large volume for free; a sparse device
	// lets us format such a volume without allocating it whole.
	let nblocks: u64 = 40_000;
	let mut fs = LiberFs::format(SparseDevice::new(nblocks), nblocks).unwrap();
	fs.write_file(b"f", b"on a big volume").unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"on a big volume");
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"on a big volume");
}

// M50: offset / partial reads and writes.

#[test]
fn write_at_in_the_middle_keeps_the_rest() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"AAAAAAAAAA").unwrap();
	fs.write_at(b"f", 3, b"BBB").unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"AAABBBAAAA");
}

#[test]
fn write_at_can_extend_the_file() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"abc").unwrap();
	fs.write_at(b"f", 3, b"defgh").unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"abcdefgh");
	assert_eq!(fs.stat(b"f").unwrap().size, 8);
}

#[test]
fn write_at_creates_the_file() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_at(b"dir/new.txt", 0, b"fresh").unwrap();
	assert_eq!(fs.read_file(b"dir/new.txt").unwrap(), b"fresh");
}

#[test]
fn write_at_past_the_end_leaves_a_zero_hole() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"abc").unwrap();
	// a gap larger than a block, so the skipped blocks are never allocated.
	let off = (BLOCK_SIZE * 2 + 10) as u64;
	fs.write_at(b"f", off, b"end").unwrap();
	let data = fs.read_file(b"f").unwrap();
	assert_eq!(data.len(), off as usize + 3);
	assert_eq!(&data[..3], b"abc");
	assert!(data[3..off as usize].iter().all(|&b| b == 0));
	assert_eq!(&data[off as usize..], b"end");
	// remount: the hole survives.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_at(b"f", off, 3).unwrap(), b"end");
}

#[test]
fn read_at_clamps_to_the_end() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"0123456789").unwrap();
	assert_eq!(fs.read_at(b"f", 4, 3).unwrap(), b"456");
	assert_eq!(fs.read_at(b"f", 8, 100).unwrap(), b"89");
	assert_eq!(fs.read_at(b"f", 10, 5).unwrap(), b"");
}

#[test]
fn append_grows_across_block_boundaries() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let chunk = vec![b'x'; BLOCK_SIZE - 3];
	fs.append(b"log", &chunk).unwrap();
	fs.append(b"log", b"YYYYYY").unwrap();
	let out = fs.read_file(b"log").unwrap();
	assert_eq!(out.len(), chunk.len() + 6);
	assert_eq!(&out[chunk.len()..], b"YYYYYY");
}

#[test]
fn truncate_shrinks_and_grows() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let big: Vec<u8> = (0..BLOCK_SIZE * 3).map(|i| (i % 251) as u8).collect();
	fs.write_file(b"f", &big).unwrap();
	fs.truncate(b"f", 5).unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), &big[..5]);
	// grow back: the new tail reads as zeros.
	fs.truncate(b"f", 20).unwrap();
	let out = fs.read_file(b"f").unwrap();
	assert_eq!(out.len(), 20);
	assert_eq!(&out[..5], &big[..5]);
	assert!(out[5..].iter().all(|&b| b == 0));
}

#[test]
fn truncate_frees_blocks_for_reuse() {
	// a small volume: if the truncated tail were not freed it would run out of space.
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let big: Vec<u8> = vec![7u8; BLOCK_SIZE * 8];
	for _ in 0..30 {
		fs.write_file(b"scratch", &big).unwrap();
		fs.truncate(b"scratch", 0).unwrap();
	}
	assert_eq!(fs.stat(b"scratch").unwrap().size, 0);
}

// M50: timestamps and stat.

#[test]
fn stat_reports_kind_size_and_timestamps() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.set_clock(100);
	fs.write_file(b"f", b"hello").unwrap();
	let st = fs.stat(b"f").unwrap();
	assert!(!st.is_dir);
	assert_eq!(st.size, 5);
	assert_eq!(st.mtime, 100);

	fs.set_clock(250);
	fs.write_at(b"f", 5, b"!").unwrap();
	let st = fs.stat(b"f").unwrap();
	assert_eq!(st.size, 6);
	assert_eq!(st.mtime, 250);

	fs.mkdir(b"d").unwrap();
	assert!(fs.stat(b"d").unwrap().is_dir);
	assert_eq!(fs.stat(b"missing"), Err(FsError::NotFound));
}

// M50: rename / move within the volume.

#[test]
fn rename_moves_a_file() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"a.txt", b"payload").unwrap();
	fs.rename(b"a.txt", b"sub/b.txt").unwrap();
	assert_eq!(fs.lookup(b"a.txt"), None);
	assert_eq!(fs.read_file(b"sub/b.txt").unwrap(), b"payload");
}

#[test]
fn rename_replaces_an_existing_file() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"src", b"new").unwrap();
	fs.write_file(b"dst", b"old").unwrap();
	fs.rename(b"src", b"dst").unwrap();
	assert_eq!(fs.read_file(b"dst").unwrap(), b"new");
	assert_eq!(fs.lookup(b"src"), None);
	// the inode the destination used to hold was freed: churn does not leak it.
	for _ in 0..200 {
		fs.write_file(b"churn", b"x").unwrap();
		fs.remove(b"churn").unwrap();
	}
}

#[test]
fn rename_moves_a_directory_subtree() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"old/inner/file", b"deep").unwrap();
	fs.rename(b"old", b"new").unwrap();
	assert_eq!(fs.lookup(b"old"), None);
	assert_eq!(fs.read_file(b"new/inner/file").unwrap(), b"deep");
}

#[test]
fn rename_rejects_a_directory_into_its_own_subtree() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"a/b/c").unwrap();
	assert_eq!(fs.rename(b"a", b"a/b/inside"), Err(FsError::Invalid));
	// the tree is untouched.
	assert!(fs.stat(b"a/b/c").unwrap().is_dir);
}

#[test]
fn rename_rejects_overwriting_a_nonempty_directory() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"src", b"x").unwrap();
	fs.write_file(b"dst/keep", b"y").unwrap();
	assert_eq!(fs.rename(b"src", b"dst"), Err(FsError::NotEmpty));
}

// M51: block checksums (integrity).

// Flip the first byte of the given needle where it sits on disk, modelling bit rot.
fn corrupt_bytes(dev: &mut MemDevice, needle: &[u8]) {
	let pos = dev.blocks.windows(needle.len()).position(|w| w == needle).expect("content on disk");
	dev.blocks[pos] ^= 0xFF;
}

// Pseudo-random, incompressible bytes (a small LCG), so a file stays raw on disk and its
// content lands verbatim rather than being squashed by transparent compression.
fn noise(n: usize) -> Vec<u8> {
	let mut s: u32 = 0x1234_5678;
	(0..n)
		.map(|_| {
			s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
			(s >> 24) as u8
		})
		.collect()
}

#[test]
fn a_flipped_byte_is_caught_on_read() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"the quick brown fox").unwrap();
	let mut dev = fs.into_device();
	corrupt_bytes(&mut dev, b"the quick brown fox");
	let mut fs = LiberFs::mount(dev).unwrap();
	// the checksum no longer matches: a distinct error, not the corrupt bytes.
	assert_eq!(fs.read_file(b"f"), Err(FsError::Corrupt));
}

#[test]
fn a_flipped_byte_in_an_extent_file_is_caught() {
	// a multi-block file keeps a per-block CRC32C in its extent's checksum block;
	// flipping a data byte far into the run is still caught on read.
	let nblocks: u64 = 128;
	let mut fs = LiberFs::format(MemDevice::new(nblocks), nblocks).unwrap();
	let size = BLOCK_SIZE * 6;
	let marker = b"a needle near the end";
	// incompressible payload so the run stays raw: a compressed extent would not hold the
	// marker verbatim on disk for corrupt_bytes to find.
	let mut big: Vec<u8> = noise(size);
	let at = size - 64;
	big[at..at + marker.len()].copy_from_slice(marker);
	fs.write_file(b"big", &big).unwrap();
	let mut dev = fs.into_device();
	corrupt_bytes(&mut dev, marker);
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"big"), Err(FsError::Corrupt));
}

#[test]
fn fsck_reports_a_checksum_failure() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"integrity matters here").unwrap();
	let mut dev = fs.into_device();
	corrupt_bytes(&mut dev, b"integrity matters here");
	let mut fs = LiberFs::mount(dev).unwrap();
	let report = fs.fsck().unwrap();
	assert_eq!(report.checksum_failures, 1);
	// fsck names the damaged file, not just a count.
	assert_eq!(report.damaged, vec![b"f".to_vec()]);
}

#[test]
fn a_clean_file_survives_a_remount_with_checksums() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let payload: Vec<u8> = (0..(BLOCK_SIZE * 2 + 17)).map(|i| (i % 251) as u8).collect();
	fs.write_file(b"data.bin", &payload).unwrap();
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	// an untouched disk verifies cleanly: every block matches its stored checksum.
	assert_eq!(fs.read_file(b"data.bin").unwrap(), payload);
	assert_eq!(fs.fsck().unwrap().checksum_failures, 0);
}

// M52: copy-on-write atomicity and snapshots.

// The superblock slot (block 0 or 1) holding the newer generation - the root a clean
// mount would pick. The generation is the little-endian u64 at byte 28 of the slot.
fn newest_super_slot(dev: &MemDevice) -> u32 {
	let generation = |slot: u32| -> u64 {
		let off = slot as usize * BLOCK_SIZE + 28;
		u64::from_le_bytes(dev.blocks[off..off + 8].try_into().unwrap())
	};
	if generation(1) > generation(0) {
		1
	} else {
		0
	}
}

#[test]
fn a_torn_commit_keeps_the_previous_file_whole() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"version one").unwrap();
	fs.write_file(b"f", b"version two").unwrap();
	let dev = fs.into_device();

	// an intact disk mounts the complete new file.
	let mut clean = LiberFs::mount(dev.clone()).unwrap();
	assert_eq!(clean.read_file(b"f").unwrap(), b"version two");

	// model a crash that lost the latest commit: tear the newest superblock slot by
	// flipping one byte. The byte sits past the header fields, so magic and version
	// still parse - it is the slot's self-CRC that rejects it. Mount must fall back to
	// the previous root: the complete old file, never a torn mix of the two.
	let mut torn = dev;
	let slot = newest_super_slot(&torn);
	torn.blocks[slot as usize * BLOCK_SIZE + 200] ^= 0xFF;
	let mut fs = LiberFs::mount(torn).unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"version one");
}

#[test]
fn a_previous_root_mounts_read_only_as_a_snapshot() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"version one").unwrap();
	fs.write_file(b"f", b"version two").unwrap();
	let dev = fs.into_device();

	// the live mount sees the newest write.
	let mut live = LiberFs::mount(dev.clone()).unwrap();
	assert_eq!(live.read_file(b"f").unwrap(), b"version two");

	// the generation one commit back is still reachable, holding the old contents - the
	// groundwork a read-only snapshot is built on.
	let mut snap = LiberFs::mount_snapshot(dev).unwrap();
	assert_eq!(snap.read_file(b"f").unwrap(), b"version one");
}

#[test]
fn a_freshly_formatted_volume_has_no_snapshot() {
	let fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let dev = fs.into_device();
	// only generation 0 has ever been written: there is no older root to mount.
	assert!(LiberFs::mount_snapshot(dev).is_none());
}

// M53: 64-bit addressing, large files and long names.

#[test]
fn a_long_name_round_trips() {
	// a 255-byte name fills the whole record name field with no terminator.
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let name = vec![b'n'; NAME_MAX];
	fs.write_file(&name, b"long").unwrap();
	assert_eq!(fs.read_file(&name).unwrap(), b"long");
	// the full name lists back exactly and survives a remount.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(&name).unwrap(), b"long");
	assert_eq!(fs.list().unwrap()[0].0, name);
}

#[test]
fn rejects_unportable_name_characters() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	// the portable-name policy rejects the punctuation and control
	// bytes, on top of the path separator and NUL the parser already forbids.
	let bad: [&[u8]; 10] = [b"a\\b", b"a:b", b"a*b", b"a?b", b"a<b", b"a>b", b"a|b", b"a\"b", b"a\x01b", b"a\x7fb"];
	for name in bad {
		assert_eq!(fs.write_file(name, b"x"), Err(FsError::BadName));
	}
	// allowed punctuation, spaces and non-ASCII bytes still work.
	let ok = "resume v2 (final).txt".as_bytes();
	fs.write_file(ok, b"ok").unwrap();
	assert_eq!(fs.read_file(ok).unwrap(), b"ok");
}

// M54: extents and sparse files.

#[test]
fn large_contiguous_file_uses_few_extents() {
	// a big file written in one shot lands in a contiguous run of data blocks, so the
	// whole thing collapses into a couple of extents instead of a pointer per block.
	let nblocks: u64 = 4096;
	let mut fs = LiberFs::format(SparseDevice::new(nblocks), nblocks).unwrap();
	// 1501 blocks: past one extent's 1024-block (4 MB) checksum cap, so it needs two.
	let size = BLOCK_SIZE * 1500 + 321;
	let big: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
	fs.write_file(b"big", &big).unwrap();
	assert_eq!(fs.read_file(b"big").unwrap(), big);
	// the 1501 blocks map with two extents, not 1501 pointers.
	let num = fs.lookup(b"big").unwrap();
	assert_eq!(fs.read_inode(num).unwrap().extents.len(), 2);
	// the extents persist across a remount.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"big").unwrap(), big);
	// overwriting with a smaller file frees the run and reuses the inode.
	fs.write_file(b"big", b"small").unwrap();
	assert_eq!(fs.read_file(b"big").unwrap(), b"small");
}

#[test]
fn sparse_file_occupies_only_written_blocks() {
	// a file can be far larger logically than the device is physically: writing two
	// spans far apart allocates only those blocks, never the hole between them.
	let nblocks: u64 = 4096;
	let mut fs = LiberFs::format(SparseDevice::new(nblocks), nblocks).unwrap();
	fs.write_at(b"sparse", 0, b"start").unwrap();
	// half a million blocks past the start - the gap alone dwarfs the whole device.
	let far = 500_000u64 * BLOCK_SIZE as u64;
	fs.write_at(b"sparse", far, b"end").unwrap();
	// the file logically spans far past the device; had the hole been allocated, a
	// 500k-block file could never fit a 4096-block device.
	assert_eq!(fs.stat(b"sparse").unwrap().size, far + 3);
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_at(b"sparse", 0, 5).unwrap(), b"start");
	assert_eq!(fs.read_at(b"sparse", far, 3).unwrap(), b"end");
	// the hole between the two spans reads back as zeros.
	assert_eq!(fs.read_at(b"sparse", BLOCK_SIZE as u64, 4).unwrap(), vec![0u8; 4]);
}

// M55: B+tree directories and dynamic inode allocation.

#[test]
fn a_directory_scales_to_thousands_of_entries() {
	// enough entries to force internal-node splits in both the directory B+tree (keyed by
	// name hash) and the inode B+tree (keyed by number): a leaf holds at most
	// DIR_LEAF_MAX / INODE_LEAF_MAX records and an internal node at most INTERNAL_MAX
	// children. The inode tree's sequential keys leave each split leaf about half full,
	// so a couple of thousand files alone push it past two levels and exercise the
	// internal-node split.
	let nblocks: u64 = 12_000;
	let mut fs = LiberFs::format(MemDevice::new(nblocks), nblocks).unwrap();
	let count = 2000u32;
	for i in 0..count {
		let name = format!("file{i:05}");
		fs.write_file(name.as_bytes(), name.as_bytes()).unwrap();
	}
	// every entry is present and reads back its own name.
	assert_eq!(fs.list().unwrap().len() as u32, count);
	for i in 0..count {
		let name = format!("file{i:05}");
		assert_eq!(fs.read_file(name.as_bytes()).unwrap(), name.as_bytes());
	}

	// remove every third entry, then confirm the rest survive and the gaps are gone.
	let mut removed = 0u32;
	for i in (0..count).step_by(3) {
		let name = format!("file{i:05}");
		fs.remove(name.as_bytes()).unwrap();
		removed += 1;
	}
	assert_eq!(fs.list().unwrap().len() as u32, count - removed);

	// the survivors persist across a remount; the removed ones stay gone.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.list().unwrap().len() as u32, count - removed);
	for i in 0..count {
		let name = format!("file{i:05}");
		if i % 3 == 0 {
			assert_eq!(fs.lookup(name.as_bytes()), None);
		} else {
			assert_eq!(fs.read_file(name.as_bytes()).unwrap(), name.as_bytes());
		}
	}
}

#[test]
fn inodes_are_allocated_dynamically_without_a_fixed_cap() {
	// a small volume creates as many files as its data blocks allow, not a preallocated
	// inode count: inodes come from the B+tree on demand, so the only limit is space.
	let nblocks: u64 = 256;
	let mut fs = LiberFs::format(MemDevice::new(nblocks), nblocks).unwrap();
	let mut made = 0u32;
	loop {
		let name = format!("f{made}");
		match fs.write_file(name.as_bytes(), b"x") {
			Ok(()) => made += 1,
			Err(FsError::NoSpace) => break,
			Err(e) => panic!("unexpected error: {e:?}"),
		}
	}
	// far more files than any small fixed inode table would have reserved room for.
	assert!(made > 16, "only {made} files created");
	assert_eq!(fs.list().unwrap().len() as u32, made);

	// the inodes and entries survive a remount.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.list().unwrap().len() as u32, made);
	assert_eq!(fs.read_file(b"f0").unwrap(), b"x");
}

// M56: named, pinned snapshots.

#[test]
fn a_named_snapshot_reads_an_earlier_state() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"version one").unwrap();
	fs.create_snapshot(b"before").unwrap();
	fs.write_file(b"f", b"version two").unwrap();
	let dev = fs.into_device();

	// the live volume sees the newest write.
	let mut live = LiberFs::mount(dev.clone()).unwrap();
	assert_eq!(live.read_file(b"f").unwrap(), b"version two");

	// the named snapshot reads the state captured when it was created - through a
	// snapshot mount, and through the cheap in-place read the service's snap-open
	// rides (no second mount, no volume walk).
	let mut snap = LiberFs::mount_named_snapshot(dev.clone(), b"before").unwrap();
	assert_eq!(snap.read_file(b"f").unwrap(), b"version one");
	let mut live = LiberFs::mount(dev).unwrap();
	assert_eq!(live.read_file_from_snapshot(b"before", b"f").unwrap(), b"version one");
	assert_eq!(live.read_file_from_snapshot(b"missing", b"f"), Err(FsError::NotFound));
	// the re-rooted read leaves the live tree exactly where it was.
	assert_eq!(live.read_file(b"f").unwrap(), b"version two");
}

#[test]
fn snapshots_are_listed_and_survive_a_remount() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"v1").unwrap();
	fs.create_snapshot(b"first").unwrap();
	fs.write_file(b"f", b"v2").unwrap();
	fs.create_snapshot(b"second").unwrap();

	let listed = fs.list_snapshots().unwrap();
	assert_eq!(listed.len(), 2);
	assert_eq!(listed[0].0, b"first");
	assert_eq!(listed[1].0, b"second");
	// each pins a later generation than the one before it.
	assert!(listed[1].1 > listed[0].1);

	// the table is carried in the superblock, so it survives a remount.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	let listed = fs.list_snapshots().unwrap();
	assert_eq!(listed.len(), 2);
	assert_eq!(listed[0].0, b"first");
	assert_eq!(listed[1].0, b"second");
}

#[test]
fn a_snapshot_keeps_a_file_the_live_tree_deleted() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"keep.txt", b"original").unwrap();
	fs.create_snapshot(b"backup").unwrap();
	fs.remove(b"keep.txt").unwrap();
	let dev = fs.into_device();

	// the live tree no longer has the file.
	let mut live = LiberFs::mount(dev.clone()).unwrap();
	assert_eq!(live.read_file(b"keep.txt"), Err(FsError::NotFound));

	// the snapshot still holds it, blocks pinned against reclamation.
	let mut snap = LiberFs::mount_named_snapshot(dev, b"backup").unwrap();
	assert_eq!(snap.read_file(b"keep.txt").unwrap(), b"original");
}

#[test]
fn the_free_map_honors_every_pinned_generation() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	// three named snapshots, each pinning a different version of the same file.
	fs.write_file(b"f", b"one").unwrap();
	fs.create_snapshot(b"s1").unwrap();
	fs.write_file(b"f", b"two").unwrap();
	fs.create_snapshot(b"s2").unwrap();
	fs.write_file(b"f", b"three").unwrap();
	fs.create_snapshot(b"s3").unwrap();
	// churn the live file well past all three snapshots; the rolling previous-generation
	// retention moves on, but the named pins must keep each earlier root reachable.
	for v in 0..8 {
		let payload = format!("live-{v}");
		fs.write_file(b"f", payload.as_bytes()).unwrap();
	}
	let dev = fs.into_device();

	// every pinned generation still reads its captured content after a remount.
	assert_eq!(LiberFs::mount_named_snapshot(dev.clone(), b"s1").unwrap().read_file(b"f").unwrap(), b"one");
	assert_eq!(LiberFs::mount_named_snapshot(dev.clone(), b"s2").unwrap().read_file(b"f").unwrap(), b"two");
	assert_eq!(LiberFs::mount_named_snapshot(dev.clone(), b"s3").unwrap().read_file(b"f").unwrap(), b"three");

	// the live volume reads the newest content and verifies clean: fsck accounts for
	// every pinned snapshot generation as well as the live tree.
	let mut live = LiberFs::mount(dev).unwrap();
	assert_eq!(live.read_file(b"f").unwrap(), b"live-7");
	assert_eq!(live.fsck().unwrap().checksum_failures, 0);
}

#[test]
fn deleting_a_snapshot_releases_its_pinned_blocks() {
	// how many single-block files a volume still accepts: a capacity probe.
	fn fill(fs: &mut LiberFs<MemDevice>) -> u32 {
		let mut n = 0u32;
		loop {
			let name = format!("fill{n}");
			match fs.write_file(name.as_bytes(), b"x") {
				Ok(()) => n += 1,
				Err(FsError::NoSpace) => return n,
				Err(e) => panic!("unexpected error: {e:?}"),
			}
		}
	}
	let nblocks: u64 = 48;
	// incompressible, so the file really pins six data blocks (a compressed run would
	// shrink to one, weakening the capacity margin the assertion checks).
	let big: Vec<u8> = noise(BLOCK_SIZE * 6);

	// pin a multi-block file in a snapshot, delete it from the live tree, then roll the
	// previous-generation retention forward so ONLY the named snapshot pins its blocks.
	let mut fs = LiberFs::format(MemDevice::new(nblocks), nblocks).unwrap();
	fs.write_file(b"big", &big).unwrap();
	fs.create_snapshot(b"snap").unwrap();
	fs.remove(b"big").unwrap();
	fs.write_file(b"tmp", b"y").unwrap();
	fs.remove(b"tmp").unwrap();
	let with_snapshot = fill(&mut fs);

	// the same sequence, but delete the snapshot first: big's blocks are reclaimed, so
	// the volume now accepts strictly more fill files.
	let mut fs = LiberFs::format(MemDevice::new(nblocks), nblocks).unwrap();
	fs.write_file(b"big", &big).unwrap();
	fs.create_snapshot(b"snap").unwrap();
	fs.remove(b"big").unwrap();
	fs.write_file(b"tmp", b"y").unwrap();
	fs.remove(b"tmp").unwrap();
	fs.delete_snapshot(b"snap").unwrap();
	let without_snapshot = fill(&mut fs);

	assert!(without_snapshot > with_snapshot, "deleting the snapshot freed no blocks: {without_snapshot} !> {with_snapshot}");
}

#[test]
fn snapshot_name_rules_are_enforced() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"x").unwrap();
	// an empty name is rejected.
	assert_eq!(fs.create_snapshot(b""), Err(FsError::BadName));
	// a name longer than the field is rejected.
	let long = vec![b'a'; SNAP_NAME_MAX + 1];
	assert_eq!(fs.create_snapshot(&long), Err(FsError::TooLong));
	// a duplicate name is rejected.
	fs.create_snapshot(b"dup").unwrap();
	assert_eq!(fs.create_snapshot(b"dup"), Err(FsError::Exists));
	// deleting an unknown snapshot is NotFound; deleting the real one succeeds.
	assert_eq!(fs.delete_snapshot(b"missing"), Err(FsError::NotFound));
	fs.delete_snapshot(b"dup").unwrap();
	assert!(fs.list_snapshots().unwrap().is_empty());
}

// M57: transparent per-extent compression.

// Format with compression enabled: the compression tests opt in (the default is off).
fn format_lz(dev: MemDevice, num_blocks: u64) -> LiberFs<MemDevice> {
	LiberFs::format_opts(dev, num_blocks, FormatOpts { compress: true, ..FormatOpts::default() }).unwrap()
}

#[test]
fn a_compressible_file_shrinks_and_round_trips() {
	let mut fs = format_lz(MemDevice::new(NBLOCKS), NBLOCKS);
	// four blocks of repeating text: highly compressible, so the run shrinks.
	let big: Vec<u8> = b"the quick brown fox jumps over the lazy dog. ".iter().cycle().take(BLOCK_SIZE * 4).copied().collect();
	fs.write_file(b"big", &big).unwrap();
	assert_eq!(fs.read_file(b"big").unwrap(), big);
	let num = fs.lookup(b"big").unwrap();
	let ext = fs.read_inode(num).unwrap().extents[0];
	assert!(ext.clen != 0, "expected a compressed extent");
	assert!((ext.store_len as usize) < ext.length as usize, "compressed run should use fewer blocks");
	// it reads back identically across a remount and verifies clean.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"big").unwrap(), big);
	assert_eq!(fs.fsck().unwrap().checksum_failures, 0);
}

#[test]
fn an_incompressible_file_stays_raw() {
	let mut fs = format_lz(MemDevice::new(NBLOCKS), NBLOCKS);
	let big = noise(BLOCK_SIZE * 4);
	fs.write_file(b"rnd", &big).unwrap();
	assert_eq!(fs.read_file(b"rnd").unwrap(), big);
	// random bytes do not shrink, so the run is stored raw: store_len == length, clen 0.
	let num = fs.lookup(b"rnd").unwrap();
	let ext = fs.read_inode(num).unwrap().extents[0];
	assert_eq!(ext.clen, 0);
	assert_eq!(ext.store_len, ext.length);
}

#[test]
fn editing_a_compressed_file_thaws_it() {
	let mut fs = format_lz(MemDevice::new(NBLOCKS), NBLOCKS);
	let mut big: Vec<u8> = b"compress me well, ".iter().cycle().take(BLOCK_SIZE * 4).copied().collect();
	fs.write_file(b"big", &big).unwrap();
	let num = fs.lookup(b"big").unwrap();
	assert!(fs.read_inode(num).unwrap().extents[0].clen != 0);
	// overwriting a block thaws the run back to raw and keeps the data correct.
	fs.write_at(b"big", BLOCK_SIZE as u64, b"PATCH").unwrap();
	big[BLOCK_SIZE..BLOCK_SIZE + 5].copy_from_slice(b"PATCH");
	assert_eq!(fs.read_file(b"big").unwrap(), big);
	for ext in fs.read_inode(num).unwrap().extents.iter() {
		assert_eq!(ext.clen, 0, "edited file should be raw");
	}
}

#[test]
fn compression_checksums_catch_corruption() {
	let mut fs = format_lz(MemDevice::new(NBLOCKS), NBLOCKS);
	let big: Vec<u8> = b"checksum the stored bytes. ".iter().cycle().take(BLOCK_SIZE * 4).copied().collect();
	fs.write_file(b"big", &big).unwrap();
	let num = fs.lookup(b"big").unwrap();
	let ext = fs.read_inode(num).unwrap().extents[0];
	let mut dev = fs.into_device();
	// flip a byte in a stored (compressed) block: the per-block CRC32C catches it.
	dev.blocks[ext.physical as usize * BLOCK_SIZE] ^= 0xFF;
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"big"), Err(FsError::Corrupt));
	assert_eq!(fs.fsck().unwrap().checksum_failures, 1);
}

#[test]
fn the_codec_round_trips_varied_inputs() {
	for input in [Vec::new(), vec![0u8; 9000], b"hello hello hello hello world".to_vec(), noise(8000)] {
		assert_eq!(lz_decompress(&lz_compress(&input), input.len()), input);
	}
}

#[test]
fn compression_is_off_by_default_and_togglable() {
	let compressible: Vec<u8> = b"toggle me on and off. ".iter().cycle().take(BLOCK_SIZE * 4).copied().collect();

	// the default volume never compresses: the run stays raw.
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	assert!(!fs.compression());
	fs.write_file(b"raw", &compressible).unwrap();
	let num = fs.lookup(b"raw").unwrap();
	assert_eq!(fs.read_inode(num).unwrap().extents[0].clen, 0);

	// switched on, a new write compresses; the earlier file keeps its raw form.
	fs.set_compression(true).unwrap();
	assert!(fs.compression());
	fs.write_file(b"packed", &compressible).unwrap();
	let num = fs.lookup(b"packed").unwrap();
	assert!(fs.read_inode(num).unwrap().extents[0].clen != 0);
	let raw = fs.lookup(b"raw").unwrap();
	assert_eq!(fs.read_inode(raw).unwrap().extents[0].clen, 0);

	// the switch survives a remount, and switching off leaves old compressed files
	// readable while new writes land raw.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert!(fs.compression());
	fs.set_compression(false).unwrap();
	fs.write_file(b"raw2", &compressible).unwrap();
	let num = fs.lookup(b"raw2").unwrap();
	assert_eq!(fs.read_inode(num).unwrap().extents[0].clen, 0);
	assert_eq!(fs.read_file(b"packed").unwrap(), compressible);
}

#[test]
fn the_volume_identity_survives_a_remount() {
	// a label well past the old 32-byte field proves the 256-byte width.
	let long: Vec<u8> = b"backup-volume-".iter().cycle().take(200).copied().collect();
	let opts = FormatOpts { uuid: [7u8; 16], label: long.clone(), compress: false };
	let fs = LiberFs::format_opts(MemDevice::new(NBLOCKS), NBLOCKS, opts).unwrap();
	let dev = fs.into_device();
	let fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.uuid(), [7u8; 16]);
	assert_eq!(fs.label(), &long[..]);
}

#[test]
fn a_volume_with_foreign_feature_flags_does_not_mount() {
	let fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let mut dev = fs.into_device();
	// flip a feature bit in slot 0 and refresh its self-CRC: the flags are alien now,
	// so the mount must reject the volume rather than mis-parse its layout.
	dev.blocks[72] ^= 0x02;
	let crc_probe: Vec<u8> = {
		let mut probe = dev.blocks[..BLOCK_SIZE].to_vec();
		probe[SB_CRC_OFFSET..SB_CRC_OFFSET + 4].fill(0);
		probe
	};
	let crc = crc32c(&crc_probe);
	dev.blocks[SB_CRC_OFFSET..SB_CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
	assert!(LiberFs::mount(dev).is_none());
}

#[test]
fn names_must_be_utf8() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	// a bare continuation byte is not UTF-8: rejected, so one file has one name.
	assert_eq!(fs.write_file(b"bad\x80name", b"x"), Err(FsError::BadName));
	// real multi-byte UTF-8 works.
	let name = "soubor-\u{10D}e\u{161}tina.txt".as_bytes();
	fs.write_file(name, b"ok").unwrap();
	assert_eq!(fs.read_file(name).unwrap(), b"ok");
}

#[test]
fn fsck_names_a_damaged_file_in_a_subdirectory() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"docs/inner/report.txt", b"the full path should be named").unwrap();
	fs.write_file(b"clean.txt", b"untouched").unwrap();
	let mut dev = fs.into_device();
	corrupt_bytes(&mut dev, b"the full path should be named");
	let mut fs = LiberFs::mount(dev).unwrap();
	let report = fs.fsck().unwrap();
	assert_eq!(report.checksum_failures, 1);
	assert_eq!(report.damaged, vec![b"docs/inner/report.txt".to_vec()]);
}

#[test]
fn restore_from_a_snapshot_heals_a_damaged_file() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"version one - the good copy").unwrap();
	fs.create_snapshot(b"backup").unwrap();
	// the rewrite lands on fresh blocks, so the snapshot's copy stays independent.
	fs.write_file(b"f", b"version two - about to break").unwrap();
	let mut dev = fs.into_device();
	corrupt_bytes(&mut dev, b"version two - about to break");
	let mut fs = LiberFs::mount(dev).unwrap();

	// the live file is damaged and fsck names it; the snapshot's copy is intact.
	assert_eq!(fs.read_file(b"f"), Err(FsError::Corrupt));
	assert_eq!(fs.fsck().unwrap().damaged, vec![b"f".to_vec()]);

	// restore copies the snapshot's version into the live tree: readable again,
	// explicitly at the snapshot's (older) content.
	fs.restore_file(b"f", b"backup").unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"version one - the good copy");
	assert!(fs.fsck().unwrap().damaged.is_empty());

	// an unknown snapshot is NotFound; the empty name restores from the previous
	// generation - one more commit first, so the restored state IS that generation
	// (right after the restore, "previous" is still the damaged pre-restore tree).
	assert_eq!(fs.restore_file(b"f", b"missing"), Err(FsError::NotFound));
	fs.write_file(b"other", b"tick").unwrap();
	fs.restore_file(b"f", b"").unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"version one - the good copy");
}

#[test]
fn snapshots_scale_past_a_single_table_block() {
	// more snapshots than one chain block holds (48): the chained table has no cap.
	let nblocks: u64 = 512;
	let mut fs = LiberFs::format(MemDevice::new(nblocks), nblocks).unwrap();
	fs.write_file(b"f", b"seed").unwrap();
	for i in 0..60u32 {
		let name = format!("snap{i:02}");
		fs.write_file(b"f", name.as_bytes()).unwrap();
		fs.create_snapshot(name.as_bytes()).unwrap();
	}
	assert_eq!(fs.list_snapshots().unwrap().len(), 60);

	// the whole chain survives a remount; an early and a late snapshot both read
	// their pinned content, and deletion still works.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.list_snapshots().unwrap().len(), 60);
	fs.delete_snapshot(b"snap30").unwrap();
	assert_eq!(fs.list_snapshots().unwrap().len(), 59);
	let dev = fs.into_device();
	assert_eq!(LiberFs::mount_named_snapshot(dev.clone(), b"snap00").unwrap().read_file(b"f").unwrap(), b"snap00");
	assert_eq!(LiberFs::mount_named_snapshot(dev, b"snap59").unwrap().read_file(b"f").unwrap(), b"snap59");
}

// M73: correctness hardening (flush barriers, read-only mounts, corruption honesty).

// What a device saw, in order: a block write or a flush barrier. The flush-ordering
// test asserts the commit protocol from this log.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ev {
	Write(u64),
	Flush,
}

// A MemDevice that logs every write and flush, to prove the commit protocol brackets
// the superblock write with barriers.
struct FlushLogDevice {
	inner: MemDevice,
	log: Vec<Ev>,
}

impl BlockDevice for FlushLogDevice {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		self.inner.read_block(index, buf)
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		self.log.push(Ev::Write(index));
		self.inner.write_block(index, buf)
	}

	fn flush(&mut self) -> bool {
		self.log.push(Ev::Flush);
		true
	}
}

#[test]
fn a_commit_brackets_the_superblock_write_with_flushes() {
	let dev = FlushLogDevice { inner: MemDevice::new(NBLOCKS), log: Vec::new() };
	let fs = LiberFs::format(dev, NBLOCKS).unwrap();
	// drop the format's own events, then observe one whole transaction (a mount only
	// reads, so the log stays empty until the write).
	let mut dev = fs.into_device();
	dev.log.clear();
	let mut fs = LiberFs::mount(dev).unwrap();
	fs.write_file(b"f", b"durable").unwrap();
	let dev = fs.into_device();
	let log = &dev.log;

	// exactly one superblock write (the commit point), and it is the tail of the log,
	// bracketed by the two barriers: every transaction block is on the medium before
	// the superblock names it, and the commit itself is durable before we report Ok.
	let sb_writes = log.iter().filter(|e| matches!(e, Ev::Write(0) | Ev::Write(1))).count();
	assert_eq!(sb_writes, 1, "one commit writes one superblock: {log:?}");
	let n = log.len();
	assert!(n >= 3, "expected writes plus the commit tail: {log:?}");
	assert_eq!(log[n - 1], Ev::Flush, "the commit must end with a barrier: {log:?}");
	assert!(matches!(log[n - 2], Ev::Write(0) | Ev::Write(1)), "the superblock write sits between the barriers: {log:?}");
	assert_eq!(log[n - 3], Ev::Flush, "a barrier must precede the superblock write: {log:?}");
	// no data write hides between the barriers or after the commit.
	for e in &log[..n - 3] {
		assert!(matches!(e, Ev::Write(b) if *b > 1), "only transaction blocks precede the commit tail: {log:?}");
	}
}

#[test]
fn a_corrupt_snapshot_table_degrades_the_mount_to_read_only() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"pinned").unwrap();
	fs.create_snapshot(b"keep").unwrap();
	let mut dev = fs.into_device();

	// flip one byte of the snapshot-table block the newest superblock points at.
	let slot = newest_super_slot(&dev) as usize;
	let snap_root = u64::from_le_bytes(dev.blocks[slot * BLOCK_SIZE + 60..slot * BLOCK_SIZE + 68].try_into().unwrap());
	assert!(snap_root != 0, "the volume should carry a snapshot table");
	dev.blocks[snap_root as usize * BLOCK_SIZE + 3] ^= 0xFF;

	// the volume still mounts (the live tree is intact) but read-only: the pinned
	// generations can no longer be reserved, so no commit may reuse their blocks.
	let mut fs = LiberFs::mount(dev).unwrap();
	assert!(fs.is_read_only(), "a corrupt snapshot table must force read-only");
	assert_eq!(fs.read_file(b"f").unwrap(), b"pinned");
	assert_eq!(fs.write_file(b"g", b"nope"), Err(FsError::ReadOnly));
	assert_eq!(fs.remove(b"f"), Err(FsError::ReadOnly));
}

#[test]
fn snapshot_mounts_refuse_writes() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"one").unwrap();
	fs.create_snapshot(b"pin").unwrap();
	fs.write_file(b"f", b"two").unwrap();
	let dev = fs.into_device();

	// both snapshot mounts read fine and refuse every mutation.
	let mut prev = LiberFs::mount_snapshot(dev.clone()).unwrap();
	assert!(prev.is_read_only());
	assert_eq!(prev.write_file(b"f", b"x"), Err(FsError::ReadOnly));
	let mut named = LiberFs::mount_named_snapshot(dev.clone(), b"pin").unwrap();
	assert!(named.is_read_only());
	assert_eq!(named.read_file(b"f").unwrap(), b"one");
	assert_eq!(named.write_at(b"f", 0, b"x"), Err(FsError::ReadOnly));
	assert_eq!(named.rename(b"f", b"g"), Err(FsError::ReadOnly));
	assert_eq!(named.create_snapshot(b"more"), Err(FsError::ReadOnly));
	// even a no-change compression request is refused: the policy has no side door.
	assert_eq!(named.set_compression(false), Err(FsError::ReadOnly));

	// the live mount stays writable.
	let mut live = LiberFs::mount(dev).unwrap();
	assert!(!live.is_read_only());
	live.write_file(b"f", b"three").unwrap();
	assert_eq!(live.read_file(b"f").unwrap(), b"three");
}

// A MemDevice that corrupts one chosen block as it is written, modeling a device
// that damages the bytes between the write and the compressor's read-back.
struct BadWriteDevice {
	inner: MemDevice,
	corrupt_block: u64,
}

impl BlockDevice for BadWriteDevice {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		self.inner.read_block(index, buf)
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		let mut bytes = buf.to_vec();
		if index == self.corrupt_block {
			bytes[0] ^= 0xFF;
		}
		self.inner.write_block(index, &bytes)
	}
}

#[test]
fn compression_never_launders_a_corrupt_source_block() {
	// the first data block a fresh volume allocates: right past the two superblock
	// slots and the format's inode-tree leaf.
	let first_data: u64 = POOL_START + 1;
	let dev = BadWriteDevice { inner: MemDevice::new(NBLOCKS), corrupt_block: first_data };
	let mut fs = LiberFs::format_opts(dev, NBLOCKS, FormatOpts { compress: true, ..FormatOpts::default() }).unwrap();
	// four compressible blocks; the device damages the first as it lands. The
	// compressor must notice the read-back fails its just-stored CRC and leave the
	// run raw - re-encoding it would discard the only checksum that knows.
	let big: Vec<u8> = b"a very compressible refrain. ".iter().cycle().take(BLOCK_SIZE * 4).copied().collect();
	fs.write_file(b"big", &big).unwrap();
	let num = fs.lookup(b"big").unwrap();
	let ext = fs.read_inode(num).unwrap().extents[0];
	assert_eq!(ext.physical, first_data, "the run should start at the first data block");
	assert_eq!(ext.clen, 0, "a run with a bad source block must stay raw");
	// the damage stays detectable: the read fails its checksum and fsck counts it.
	assert_eq!(fs.read_file(b"big"), Err(FsError::Corrupt));
	assert_eq!(fs.fsck().unwrap().checksum_failures, 1);
}

// M74: the incremental free map and the next-fit allocator.

// After every committed mutation, the incrementally maintained free map must equal
// what the full volume walk would derive - the invariant the whole incremental
// scheme stands on. `derive_free` recomputes free, pinned and dead_prev from the
// trees, so calling it mid-scenario is state-preserving: any drift is a bug in the
// drop bookkeeping (a leak if incremental holds more, a corruption risk if less).
#[test]
fn the_incremental_free_map_matches_a_full_rederivation() {
	fn check(fs: &mut LiberFs<MemDevice>, what: &str) {
		let saved = fs.free.clone();
		fs.derive_free().unwrap();
		for b in 0..fs.num_blocks {
			let inc = test_bit(&saved, b);
			let full = test_bit(&fs.free, b);
			assert_eq!(inc, full, "free map drifted after {what}: block {b} incremental={inc} full={full}");
		}
	}
	let nblocks: u64 = 256;
	let mut fs = LiberFs::format_opts(MemDevice::new(nblocks), nblocks, FormatOpts { compress: true, ..FormatOpts::default() }).unwrap();
	let compressible: Vec<u8> = b"squeeze me flat. ".iter().cycle().take(BLOCK_SIZE * 4).copied().collect();

	fs.write_file(b"a", &noise(BLOCK_SIZE * 3)).unwrap();
	check(&mut fs, "a fresh write");
	fs.write_file(b"a", &noise(BLOCK_SIZE * 5 + 100)).unwrap();
	check(&mut fs, "a whole-file replace");
	fs.write_file(b"c", &compressible).unwrap();
	check(&mut fs, "a compressed write");
	fs.write_at(b"c", BLOCK_SIZE as u64, b"patch").unwrap();
	check(&mut fs, "a thawing patch");
	fs.write_at(b"a", 100, b"xx").unwrap();
	check(&mut fs, "an overwrite that splits a run");
	fs.write_at(b"a", (BLOCK_SIZE * 8) as u64, b"far").unwrap();
	check(&mut fs, "a sparse extension");
	fs.truncate(b"a", BLOCK_SIZE as u64 + 5).unwrap();
	check(&mut fs, "a shortening truncate");
	fs.truncate(b"a", 0).unwrap();
	check(&mut fs, "a truncate to zero");
	fs.mkdir(b"d/e").unwrap();
	check(&mut fs, "mkdir -p");
	fs.write_file(b"d/e/f", b"x").unwrap();
	check(&mut fs, "a nested write");
	fs.rename(b"d/e/f", b"g").unwrap();
	check(&mut fs, "a rename");
	fs.write_file(b"h", b"y").unwrap();
	fs.rename(b"g", b"h").unwrap();
	check(&mut fs, "a replacing rename");
	fs.remove(b"h").unwrap();
	check(&mut fs, "a remove");
	fs.rmdir(b"d/e").unwrap();
	check(&mut fs, "an rmdir");

	// snapshots: creation and deletion rebuild by the full walk; the churn between
	// them exercises the incremental path with pinned blocks in play.
	fs.write_file(b"pinned", &noise(BLOCK_SIZE * 2)).unwrap();
	fs.create_snapshot(b"s").unwrap();
	check(&mut fs, "a snapshot create");
	fs.write_file(b"pinned", &noise(BLOCK_SIZE * 2 + 7)).unwrap();
	check(&mut fs, "replacing a pinned file");
	fs.remove(b"pinned").unwrap();
	check(&mut fs, "removing a pinned file");
	fs.delete_snapshot(b"s").unwrap();
	check(&mut fs, "a snapshot delete");

	// churn to a steady state: the freed blocks must actually come back for reuse.
	for round in 0..20 {
		fs.write_file(b"cycle", &noise(BLOCK_SIZE * 4)).unwrap();
		check(&mut fs, "churn");
		let _ = round;
	}

	// the state persists: a remount derives the same map and reads everything back.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"cycle").unwrap(), noise(BLOCK_SIZE * 4));
	assert_eq!(fs.fsck().unwrap().checksum_failures, 0);
}

// A whole-file write reserves its span up front, so the file lands in one extent per
// checksum-block span even when the pool is checkered by earlier churn.
#[test]
fn a_whole_file_write_lands_contiguously() {
	let nblocks: u64 = 512;
	let mut fs = LiberFs::format(MemDevice::new(nblocks), nblocks).unwrap();
	// checker the pool: many small files, then remove every other one.
	for i in 0..24u32 {
		let name = format!("frag{i}");
		fs.write_file(name.as_bytes(), &noise(BLOCK_SIZE)).unwrap();
	}
	for i in (0..24u32).step_by(2) {
		let name = format!("frag{i}");
		fs.remove(name.as_bytes()).unwrap();
	}
	// two commits so the removals' blocks actually free (the deferred reclaim).
	fs.write_file(b"tick", b"1").unwrap();
	fs.write_file(b"tick", b"2").unwrap();
	// a 40-block file: bigger than any single freed hole, so without the up-front
	// reservation the per-block cursor would stitch it from fragments.
	let big = noise(BLOCK_SIZE * 40);
	fs.write_file(b"big", &big).unwrap();
	let num = fs.lookup(b"big").unwrap();
	assert_eq!(fs.read_inode(num).unwrap().extents.len(), 1, "the write should land as one contiguous extent");
	assert_eq!(fs.read_file(b"big").unwrap(), big);
}

// M74: the scaling benchmark. Ignored in the normal run (it takes seconds); run with
// `cargo test --release bench_scaling -- --ignored --nocapture` and record the
// numbers in docs/PERF.md. Three costs the milestone attacks: a large write (the
// allocator and checksum batching), a sequential re-read (the checksum read cache),
// and a many-file tree (the per-commit free-map rederivation). Device reads/writes
// are counted too: on a RAM-backed test device the I/O counts, not the wall time,
// are what predict real-disk behaviour.
#[test]
#[ignore]
fn bench_scaling() {
	use std::time::Instant;

	// a SparseDevice that counts its reads and writes.
	struct CountingDevice {
		inner: SparseDevice,
		reads: u64,
		writes: u64,
	}
	impl BlockDevice for CountingDevice {
		fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
			self.reads += 1;
			self.inner.read_block(index, buf)
		}
		fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
			self.writes += 1;
			self.inner.write_block(index, buf)
		}
	}

	// a 1 GB volume, sparse so only written blocks cost test memory.
	let nblocks: u64 = 262_144;
	let dev = CountingDevice { inner: SparseDevice::new(nblocks), reads: 0, writes: 0 };
	let mut fs = LiberFs::format(dev, nblocks).unwrap();

	// one 64 MB incompressible file.
	let big = noise(64 * 1024 * 1024);
	let (r0, w0) = (fs.device().reads, fs.device().writes);
	let t = Instant::now();
	fs.write_file(b"big", &big).unwrap();
	println!("bench: 64 MB write: {:?} ({} reads, {} writes)", t.elapsed(), fs.device().reads - r0, fs.device().writes - w0);

	let (r0, w0) = (fs.device().reads, fs.device().writes);
	let t = Instant::now();
	assert_eq!(fs.read_file(b"big").unwrap().len(), big.len());
	println!("bench: 64 MB read: {:?} ({} reads, {} writes)", t.elapsed(), fs.device().reads - r0, fs.device().writes - w0);

	// two thousand small files: every write commits, so this measures how commit cost
	// grows with the volume's live metadata.
	let (r0, w0) = (fs.device().reads, fs.device().writes);
	let t = Instant::now();
	for i in 0..2000u32 {
		let name = format!("small{i:04}");
		fs.write_file(name.as_bytes(), name.as_bytes()).unwrap();
	}
	println!("bench: 2000 small files: {:?} ({} reads, {} writes)", t.elapsed(), fs.device().reads - r0, fs.device().writes - w0);

	// a stat per file: the lookup/read path over many files.
	let (r0, w0) = (fs.device().reads, fs.device().writes);
	let t = Instant::now();
	for i in 0..2000u32 {
		let name = format!("small{i:04}");
		assert!(fs.stat(name.as_bytes()).unwrap().size > 0);
	}
	println!("bench: 2000 stats: {:?} ({} reads, {} writes)", t.elapsed(), fs.device().reads - r0, fs.device().writes - w0);
}

// M76: the audit's test-coverage gaps.

// Records sharing a 64-bit name hash: the leaf machinery must disambiguate lookups by
// the name bytes and never let a split straddle an equal-hash group (internal nodes
// route by hash alone). A real FNV collision is impractical to find, so the pure leaf
// helpers are exercised with synthetic colliding records.
#[test]
fn colliding_hashes_stay_searchable_and_never_straddle_a_split() {
	let rec = |hash: u64, name: &[u8], child: u32| DirRec { hash, name: name.to_vec(), child };
	// a leaf where most records share one hash, sorted by (hash, name).
	let recs = vec![rec(5, b"aaa", 1), rec(7, b"bbb", 2), rec(7, b"ccc", 3), rec(7, b"ddd", 4), rec(7, b"eee", 5), rec(9, b"fff", 6)];

	// lookup disambiguates by name within the shared hash.
	assert_eq!(dir_recs_search(&recs, 7, b"ccc"), Ok(2));
	assert_eq!(dir_recs_search(&recs, 7, b"ddd"), Ok(3));
	assert!(dir_recs_search(&recs, 7, b"zzz").is_err());

	// the split point lands on a hash boundary, never inside the 7-group.
	let split = dir_split_point(&recs);
	assert!(split == 1 || split == 5, "split {split} would straddle the equal-hash group");
	assert!(recs[split].hash != recs[split - 1].hash);

	// the round trip through the on-disk leaf form preserves the colliding records.
	let mut buf = vec![0u8; BLOCK_SIZE];
	dir_leaf_write(&mut buf, &recs);
	let back = dir_leaf_parse(&buf);
	assert_eq!(back.len(), recs.len());
	for (a, b) in recs.iter().zip(back.iter()) {
		assert_eq!((a.hash, &a.name, a.child), (b.hash, &b.name, b.child));
	}

	// the fixed-record split helper honors the same rule (the inode-tree flavour).
	let fixed: Vec<Vec<u8>> = recs.iter().map(|r| r.hash.to_le_bytes().to_vec()).collect();
	let split = leaf_split_point(&fixed);
	let key = |i: usize| u64::from_le_bytes(fixed[i][0..8].try_into().unwrap());
	assert!(key(split) != key(split - 1), "equal keys must stay in one leaf");
}

// A file with more extents than fit inline in the inode (4) spills to the overflow
// chain; the chain must round-trip through writes, reads and a remount.
#[test]
fn a_many_extent_file_round_trips_through_the_spill_chain() {
	let nblocks: u64 = 512;
	let mut fs = LiberFs::format(MemDevice::new(nblocks), nblocks).unwrap();
	// eight sparse spans, far enough apart that each is its own extent: twice the
	// inline capacity, so the map spills.
	let span = |i: u64| i * 16 * BLOCK_SIZE as u64;
	for i in 0..8u64 {
		let payload = format!("span-{i}");
		fs.write_at(b"sparse", span(i), payload.as_bytes()).unwrap();
	}
	let num = fs.lookup(b"sparse").unwrap();
	let count = fs.read_inode(num).unwrap().extents.len();
	assert!(count > EXTENTS_INLINE, "eight spans should overflow the {EXTENTS_INLINE} inline extents (got {count})");

	// every span reads back, before and after a remount; fsck stays clean.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	for i in 0..8u64 {
		let payload = format!("span-{i}");
		assert_eq!(fs.read_at(b"sparse", span(i), payload.len()).unwrap(), payload.as_bytes());
	}
	assert_eq!(fs.fsck().unwrap().checksum_failures, 0);
	// shrinking away the spilled extents collapses the chain cleanly too.
	fs.truncate(b"sparse", span(2)).unwrap();
	let count = fs.read_inode(num).unwrap().extents.len();
	assert!(count <= EXTENTS_INLINE, "the truncated map should fit inline again (got {count})");
	assert_eq!(fs.read_at(b"sparse", span(1), 6).unwrap(), b"span-1");
}

// A patch that straddles two compressed extents must thaw both and keep every byte.
#[test]
fn a_write_across_a_compressed_extent_boundary_thaws_both_runs() {
	// 1200 compressible blocks: past one extent's 1024-block checksum cap, so the file
	// maps as two extents, both compressed by the whole-file write.
	let nblocks: u64 = 4096;
	let mut fs = LiberFs::format_opts(SparseDevice::new(nblocks), nblocks, FormatOpts { compress: true, ..FormatOpts::default() }).unwrap();
	let mut big: Vec<u8> = b"boundary boundary boundary. ".iter().cycle().take(BLOCK_SIZE * 1200).copied().collect();
	fs.write_file(b"big", &big).unwrap();
	let num = fs.lookup(b"big").unwrap();
	let extents = fs.read_inode(num).unwrap().extents;
	assert_eq!(extents.len(), 2, "1200 blocks should map as two extents");
	assert!(extents.iter().all(|e| e.clen != 0), "both runs should have compressed");

	// patch across the 1024-block boundary: half in the first extent, half in the
	// second. Both runs thaw; the content is exact.
	let boundary: u64 = 1024 * BLOCK_SIZE as u64;
	let patch = b"#### the patch straddles the extent boundary ####";
	let start = (boundary as usize) - patch.len() / 2;
	fs.write_at(b"big", start as u64, patch).unwrap();
	big[start..start + patch.len()].copy_from_slice(patch);
	assert_eq!(fs.read_file(b"big").unwrap(), big);
	for ext in fs.read_inode(num).unwrap().extents.iter() {
		assert_eq!(ext.clen, 0, "a patched run must be raw");
	}
	// the patched file survives a remount and verifies clean.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"big").unwrap(), big);
	assert_eq!(fs.fsck().unwrap().checksum_failures, 0);
}

// M77: fsck must verify the disk, not the caches.

// A MemDevice that corrupts reads of one (externally switchable) block: the shared
// cell lets the test flip corruption on while the filesystem stays mounted, with its
// caches warm - exactly the case fsck must not be fooled by.
struct SwitchableCorruptDevice {
	inner: MemDevice,
	target: std::rc::Rc<core::cell::Cell<u64>>,
}

impl BlockDevice for SwitchableCorruptDevice {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		if !self.inner.read_block(index, buf) {
			return false;
		}
		if index == self.target.get() {
			buf[20] ^= 0xFF;
		}
		true
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		self.inner.write_block(index, buf)
	}
}

#[test]
fn fsck_verifies_the_disk_not_the_caches() {
	let target = std::rc::Rc::new(core::cell::Cell::new(0u64));
	let dev = SwitchableCorruptDevice { inner: MemDevice::new(512), target: target.clone() };
	let mut fs = LiberFs::format(dev, 512).unwrap();
	// a file with a spill chain (more extents than fit inline), then warm the inode
	// cache by reading it back.
	let span = |i: u64| i * 16 * BLOCK_SIZE as u64;
	for i in 0..8u64 {
		let payload = format!("span-{i}");
		fs.write_at(b"sparse", span(i), payload.as_bytes()).unwrap();
	}
	let num = fs.lookup(b"sparse").unwrap();
	let spill = fs.read_inode(num).unwrap().spill;
	assert!(spill != 0, "eight extents should spill");
	assert_eq!(fs.fsck().unwrap().checksum_failures, 0);

	// corrupt the spill block's reads while the inode sits warm in the cache: fsck
	// must reload from the device and surface the damage, not serve the cached map.
	target.set(spill);
	assert_eq!(fs.fsck().map(|_| ()), Err(FsError::Corrupt), "fsck served a cached inode instead of verifying the disk");

	// healed device, clean report again (the caches repopulate from good reads).
	target.set(0);
	assert_eq!(fs.fsck().unwrap().checksum_failures, 0);
	assert_eq!(fs.read_at(b"sparse", span(7), 6).unwrap(), b"span-7");
}

// The on-disk format is defined little-endian at fixed offsets, independent of the
// host. These golden assertions pin the serializers byte for byte: they pass on any
// architecture or they catch an accidental format change (which must instead bump
// FEATURES and update the specification in LIBERFS.md).

#[test]
fn the_superblock_layout_matches_the_specification() {
	let sb = Superblock {
		num_blocks: 0x1122_3344_5566_7788,
		generation: 0x0102_0304_0506_0708,
		inode_root: 0xAABB_CCDD_EEFF_0011,
		inode_root_crc: 0xDEAD_BEEF,
		next_inode: 0x0BAD_F00D,
		root_inode: 0,
		snap_root: 0x2233_4455_6677_8899,
		snap_root_crc: 0xCAFE_BABE,
		uuid: *b"0123456789abcdef",
		label: {
			let mut l = [0u8; LABEL_MAX];
			l[..6].copy_from_slice(b"golden");
			l
		},
		compress: true,
	};
	let block = serialize_superblock(&sb);
	assert_eq!(&block[0..8], b"LIBERFS1");
	assert_eq!(&block[8..12], &1u32.to_le_bytes(), "version");
	assert_eq!(&block[12..16], &4096u32.to_le_bytes(), "block size");
	assert_eq!(&block[16..24], &0x1122_3344_5566_7788u64.to_le_bytes(), "num_blocks");
	assert_eq!(&block[24..28], &0x0BAD_F00Du32.to_le_bytes(), "next_inode");
	assert_eq!(&block[28..36], &0x0102_0304_0506_0708u64.to_le_bytes(), "generation");
	assert_eq!(&block[36..44], &0xAABB_CCDD_EEFF_0011u64.to_le_bytes(), "inode_root");
	assert_eq!(&block[44..48], &0xDEAD_BEEFu32.to_le_bytes(), "inode_root_crc");
	assert_eq!(&block[52..56], &0u32.to_le_bytes(), "root_inode");
	assert_eq!(&block[60..68], &0x2233_4455_6677_8899u64.to_le_bytes(), "snap_root");
	assert_eq!(&block[68..72], &0xCAFE_BABEu32.to_le_bytes(), "snap_root_crc");
	assert_eq!(&block[72..80], &3u64.to_le_bytes(), "feature flags");
	assert_eq!(&block[80..96], b"0123456789abcdef", "uuid");
	assert_eq!(&block[96..102], b"golden", "label");
	assert_eq!(block[352], 1, "checksum algorithm id (CRC32C)");
	assert_eq!(block[353], 2, "codec id (LZ4)");
	assert_eq!(block[354], 1, "compression switch");
	// the self-CRC at 56..60 covers the whole block with its own bytes zeroed.
	let stored = u32::from_le_bytes(block[56..60].try_into().unwrap());
	let mut probe = block.clone();
	probe[56..60].fill(0);
	assert_eq!(stored, crc32c(&probe), "superblock self-CRC");
	// and the parser reads the same volume back.
	let parsed = parse_superblock(&block).expect("the golden superblock must parse");
	assert_eq!(parsed.num_blocks, sb.num_blocks);
	assert_eq!(parsed.uuid, sb.uuid);
	assert!(parsed.compress);
}

#[test]
fn the_record_layouts_match_the_specification() {
	// one extent record: 40 bytes, all fields little-endian at fixed offsets.
	let ext = Extent { logical: 0x0102_0304_0506_0708, physical: 0x1112_1314_1516_1718, length: 0x2122_2324, csum: 0x3132_3334_3536_3738, csum_crc: 0x4142_4344, store_len: 0x5152_5354, clen: 0x6162_6364 };
	let mut rec = [0u8; EXTENT_SIZE];
	ext.write(&mut rec);
	assert_eq!(&rec[0..8], &0x0102_0304_0506_0708u64.to_le_bytes(), "logical");
	assert_eq!(&rec[8..16], &0x1112_1314_1516_1718u64.to_le_bytes(), "physical");
	assert_eq!(&rec[16..20], &0x2122_2324u32.to_le_bytes(), "length");
	assert_eq!(&rec[20..24], &0x4142_4344u32.to_le_bytes(), "csum_crc");
	assert_eq!(&rec[24..32], &0x3132_3334_3536_3738u64.to_le_bytes(), "csum");
	assert_eq!(&rec[32..36], &0x5152_5354u32.to_le_bytes(), "store_len");
	assert_eq!(&rec[36..40], &0x6162_6364u32.to_le_bytes(), "clen");
	let back = Extent::parse(&rec);
	// the parser clamps both lengths to one checksum block's coverage (CRCS_PER_BLOCK):
	// the writer never exceeds it, so a larger stored value is hostile or corrupt.
	assert_eq!((back.logical, back.physical, back.length, back.csum, back.csum_crc, back.store_len, back.clen), (ext.logical, ext.physical, CRCS_PER_BLOCK as u32, ext.csum, ext.csum_crc, CRCS_PER_BLOCK as u32, ext.clen));

	// one file inode slot: 256 bytes, header fields then the file overlay.
	let mut inode = Inode::empty(KIND_FILE);
	inode.size = 0x0A0B_0C0D_0E0F_1011;
	inode.ctime = 0x100;
	inode.mtime = 0x200;
	inode.owner_tag = *b"owner-tag-16byte";
	inode.spill = 0x0708_090A_0B0C_0D0E;
	inode.spill_crc = 0x1234_5678;
	inode.extent_count = 5;
	inode.extents.push(ext);
	let mut slot = [0u8; INODE_SIZE];
	inode.write(&mut slot);
	assert_eq!(slot[0], KIND_FILE, "kind");
	assert_eq!(&slot[8..16], &0x0A0B_0C0D_0E0F_1011u64.to_le_bytes(), "size");
	assert_eq!(&slot[16..24], &0x100u64.to_le_bytes(), "ctime");
	assert_eq!(&slot[24..32], &0x200u64.to_le_bytes(), "mtime");
	assert_eq!(&slot[32..40], &0x0708_090A_0B0C_0D0Eu64.to_le_bytes(), "spill");
	assert_eq!(&slot[40..44], &0x1234_5678u32.to_le_bytes(), "spill_crc");
	assert_eq!(&slot[44..48], &5u32.to_le_bytes(), "extent_count");
	assert_eq!(&slot[56..72], b"owner-tag-16byte", "owner tag");
	assert_eq!(&slot[72..112], &rec, "first inline extent at byte 72");

	// a directory inode overlays its tree root on the same map bytes.
	let mut dir = Inode::empty(KIND_DIR);
	dir.dir_root = 0x4041_4243_4445_4647;
	dir.dir_root_crc = 0x5051_5253;
	let mut dslot = [0u8; INODE_SIZE];
	dir.write(&mut dslot);
	assert_eq!(dslot[0], KIND_DIR);
	assert_eq!(&dslot[32..40], &0x4041_4243_4445_4647u64.to_le_bytes(), "dir_root");
	assert_eq!(&dslot[40..44], &0x5051_5253u32.to_le_bytes(), "dir_root_crc");

	// one directory leaf: the node header, then variable records back to back.
	let recs = vec![DirRec { hash: 0x0102_0304_0506_0708, name: b"a.txt".to_vec(), child: 0x0A0B_0C0D }];
	let mut leaf = vec![0u8; BLOCK_SIZE];
	dir_leaf_write(&mut leaf, &recs);
	assert_eq!(leaf[0], NODE_LEAF, "node type");
	assert_eq!(&leaf[2..4], &1u16.to_le_bytes(), "record count");
	assert_eq!(&leaf[8..16], &0x0102_0304_0506_0708u64.to_le_bytes(), "record hash");
	assert_eq!(&leaf[16..20], &0x0A0B_0C0Du32.to_le_bytes(), "record child");
	assert_eq!(leaf[20], 5, "record name length");
	assert_eq!(&leaf[21..26], b"a.txt", "record name");

	// the CRC32C test vector pins the checksum definition (Castagnoli, reflected,
	// init and final xor 0xFFFFFFFF): the RFC 3720 example.
	assert_eq!(crc32c(b"123456789"), 0xE306_9283);
}

// hostile-disk robustness: a CRC32C proves integrity, not sanity - every count,
// length and pointer off the medium is bounded before use, so an authored or
// corrupt volume can never panic, hang or absurdly allocate the mount.

// Doctor superblock slot `slot` in a raw device image: apply `f` to its bytes, then
// recompute the self-CRC - the forgery a hostile author can always produce.
fn forge_superblock(dev: &mut MemDevice, slot: usize, f: impl FnOnce(&mut [u8])) {
	let sb = &mut dev.blocks[slot * BLOCK_SIZE..(slot + 1) * BLOCK_SIZE];
	f(sb);
	sb[SB_CRC_OFFSET..SB_CRC_OFFSET + 4].fill(0);
	let crc = crc32c(sb);
	sb[SB_CRC_OFFSET..SB_CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
}

// The slot holding the live (higher) generation in a raw device image.
fn active_slot(dev: &MemDevice) -> usize {
	let slot_gen = |s: usize| parse_superblock(&dev.blocks[s * BLOCK_SIZE..(s + 1) * BLOCK_SIZE]).map(|sb| sb.generation);
	if slot_gen(1) > slot_gen(0) {
		1
	} else {
		0
	}
}

#[test]
fn an_insane_pool_size_in_the_superblock_is_refused() {
	// a checksummed superblock can still lie about the pool: a claim below the fixed
	// layout is rejected outright, one past the device fails the mount's probe of the
	// last claimed block - either way None, never a panic or an absurd allocation.
	let fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let mut dev = fs.into_device();
	forge_superblock(&mut dev, 0, |sb| sb[SB_NUM_BLOCKS_OFF..SB_NUM_BLOCKS_OFF + 8].copy_from_slice(&0u64.to_le_bytes()));
	assert!(LiberFs::mount(dev.clone()).is_none());
	forge_superblock(&mut dev, 0, |sb| sb[SB_NUM_BLOCKS_OFF..SB_NUM_BLOCKS_OFF + 8].copy_from_slice(&(1u64 << 60).to_le_bytes()));
	assert!(LiberFs::mount(dev).is_none());
}

#[test]
fn a_corrupt_node_count_cannot_panic_the_mount() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"a.txt", b"payload").unwrap();
	let root = fs.inode_root;
	let mut dev = fs.into_device();
	// stamp an impossible record count into the live tree's root (raw corruption, so
	// the node no longer matches its CRC): the mount's raw generation walks clamp it
	// and survive, and the verified read path reports the damage as itself.
	let start = root as usize * BLOCK_SIZE;
	dev.blocks[start + 2..start + 4].copy_from_slice(&u16::MAX.to_le_bytes());
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"a.txt"), Err(FsError::Corrupt));
}

#[test]
fn a_checksummed_but_insane_node_count_cannot_panic_a_lookup() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"a.txt", b"payload").unwrap();
	let root = fs.inode_root;
	let mut dev = fs.into_device();
	// a hostile author checksums whatever they write: stamp the impossible count AND
	// forge the root CRC in the superblock so the node verifies. The clamp keeps every
	// walk inside the block: the lookup completes with a sane outcome, never a panic.
	let start = root as usize * BLOCK_SIZE;
	dev.blocks[start + 2..start + 4].copy_from_slice(&u16::MAX.to_le_bytes());
	let crc = crc32c(&dev.blocks[start..start + BLOCK_SIZE]);
	let slot = active_slot(&dev);
	forge_superblock(&mut dev, slot, |sb| sb[SB_INODE_ROOT_CRC_OFF..SB_INODE_ROOT_CRC_OFF + 4].copy_from_slice(&crc.to_le_bytes()));
	let mut fs = LiberFs::mount(dev).unwrap();
	let outcome = fs.read_file(b"a.txt");
	assert!(matches!(outcome, Ok(_) | Err(FsError::NotFound) | Err(FsError::Corrupt)));
	let _ = fs.fsck().unwrap();
}

#[test]
fn a_looped_snapshot_chain_cannot_hang_the_mount() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"keep.txt", b"pinned").unwrap();
	fs.create_snapshot(b"snap").unwrap();
	let snap_root = fs.snap_root;
	let mut dev = fs.into_device();
	// loop the snapshot table's chain back onto itself: the mount's generation walk
	// must terminate (marked means walked), and the CRC-checked table loader degrades
	// the volume to read-only as with any table damage.
	let start = snap_root as usize * BLOCK_SIZE;
	dev.blocks[start..start + 8].copy_from_slice(&snap_root.to_le_bytes());
	let fs = LiberFs::mount(dev).unwrap();
	assert!(fs.is_read_only());
}

#[test]
fn a_lying_compression_header_cannot_allocate_unbounded_memory() {
	// the stream's own length header is attacker-controlled: the decoder clamps it to
	// the caller's ceiling (the run's logical size) instead of allocating what it says.
	let mut src = vec![0u8; 32];
	src[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
	assert!(lz_decompress(&src, BLOCK_SIZE).len() <= BLOCK_SIZE);
	// and a legitimate stream still round-trips under its real ceiling.
	let input: Vec<u8> = b"bounded decode ".iter().cycle().take(4500).copied().collect();
	assert_eq!(lz_decompress(&lz_compress(&input), input.len()), input);
}

#[test]
fn a_write_past_the_addressable_end_is_refused() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	assert_eq!(fs.write_at(b"f", u64::MAX - 2, b"abc"), Err(FsError::Invalid));
	// the failed transaction rolled back whole: not even the file was created.
	assert_eq!(fs.lookup(b"f"), None);
}

#[test]
fn fsck_reports_metadata_damage_instead_of_dying() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"docs").unwrap();
	fs.write_file(b"docs/a.txt", b"payload").unwrap();
	let root = fs.inode_root;
	let mut dev = fs.into_device();
	// flip a byte in the inode tree's root: everything below it is unreadable, but
	// fsck hands back a report naming the damage instead of dying on the first node.
	dev.blocks[root as usize * BLOCK_SIZE + 100] ^= 0xFF;
	let mut fs = LiberFs::mount(dev).unwrap();
	let report = fs.fsck().unwrap();
	assert!(report.checksum_failures >= 1);
	assert_eq!(report.damaged, vec![b"/".to_vec()]);
}

// Doctor inode-tree leaf record `rec` in a raw device image (assumes the tree is a
// single leaf): apply `f` to the record's 256-byte inode slot, then re-checksum the
// leaf into the active superblock - the full forgery chain a hostile author performs.
fn forge_inode_slot(dev: &mut MemDevice, f: impl FnOnce(&mut [u8])) {
	let slot = active_slot(dev);
	let sb = parse_superblock(&dev.blocks[slot * BLOCK_SIZE..(slot + 1) * BLOCK_SIZE]).unwrap();
	let leaf_start = sb.inode_root as usize * BLOCK_SIZE;
	let slot_off = leaf_start + NODE_HDR + INODE_REC + 8;
	f(&mut dev.blocks[slot_off..slot_off + INODE_SIZE]);
	let crc = crc32c(&dev.blocks[leaf_start..leaf_start + BLOCK_SIZE]);
	forge_superblock(dev, slot, |sb| sb[SB_INODE_ROOT_CRC_OFF..SB_INODE_ROOT_CRC_OFF + 4].copy_from_slice(&crc.to_le_bytes()));
}

// Write a file fragmented into six extents (sparse, alternating logical blocks), so
// its extent map spills past the four inline slots into an overflow chain block.
fn write_spilled_file(fs: &mut LiberFs<MemDevice>) -> Vec<u8> {
	let chunk = vec![0xA5u8; BLOCK_SIZE];
	for i in 0..6u64 {
		fs.write_at(b"frag.bin", i * 2 * BLOCK_SIZE as u64, &chunk).unwrap();
	}
	fs.read_file(b"frag.bin").unwrap()
}

#[test]
fn a_forged_spill_count_cannot_panic_the_mount() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let expected = write_spilled_file(&mut fs);
	let mut dev = fs.into_device();
	// stamp an impossible record count into the spill chain block and re-checksum the
	// whole forgery chain (chain -> inode slot -> leaf -> superblock): the clamp must
	// keep the parse inside the block, in every walk and on the read path.
	let mut spill = 0u64;
	forge_inode_slot(&mut dev, |slot| {
		spill = u64::from_le_bytes(slot[INO_MAP_OFF..INO_MAP_OFF + 8].try_into().unwrap());
	});
	let start = spill as usize * BLOCK_SIZE;
	dev.blocks[start + CHAIN_COUNT_OFF..start + CHAIN_COUNT_OFF + 4].copy_from_slice(&u32::MAX.to_le_bytes());
	let chain_crc = crc32c(&dev.blocks[start..start + BLOCK_SIZE]);
	forge_inode_slot(&mut dev, |slot| {
		slot[INO_MAP_CRC_OFF..INO_MAP_CRC_OFF + 4].copy_from_slice(&chain_crc.to_le_bytes());
	});
	let mut fs = LiberFs::mount(dev).expect("the mount must survive the forged count");
	assert_eq!(fs.read_file(b"frag.bin").unwrap(), expected, "the real extents still read");
}

#[test]
fn a_sparse_size_past_the_pool_cannot_demand_the_moon() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	// a legitimate sparse file sized past the pool's byte count: a whole-file read
	// could neither allocate nor fill the buffer, so it is refused - while an
	// explicit-length read of the written range still works.
	let past_pool = NBLOCKS * BLOCK_SIZE as u64 + 40_000;
	fs.write_at(b"sparse.bin", past_pool, b"tail").unwrap();
	assert_eq!(fs.read_file(b"sparse.bin"), Err(FsError::Corrupt));
	assert_eq!(fs.read_at(b"sparse.bin", past_pool, 4).unwrap(), b"tail");
}

#[test]
fn a_looped_namespace_cannot_hang_the_walks() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"a/b").unwrap();
	fs.write_file(b"a/b/f.txt", b"payload").unwrap();
	// forge a namespace cycle through the crate's own machinery: an entry in a/b
	// pointing back at the root directory (a legitimate tree is acyclic; a hostile
	// volume need not be).
	let sub = fs.lookup(b"a/b").unwrap();
	fs.mutate(|fs| fs.dir_insert(sub, b"up", ROOT_INODE)).unwrap();
	// fsck's namespace walk terminates (visited set) and reports no false damage...
	let report = fs.fsck().unwrap();
	assert_eq!(report.checksum_failures, 0);
	// ...and the rename cycle check terminates too, still refusing the move.
	assert_eq!(fs.rename(b"a", b"a/b/x"), Err(FsError::Invalid));
}

#[test]
fn a_pathologically_deep_tree_is_refused_not_overflowed() {
	let pool = 256u64;
	let mut fs = LiberFs::format(MemDevice::new(pool), pool).unwrap();
	fs.write_file(b"a.txt", b"payload").unwrap();
	let (real_root, real_crc) = (fs.inode_root, fs.inode_root_crc);
	let mut dev = fs.into_device();
	// stack 70 checksummed one-child internal nodes above the real root: a shape no
	// legitimate writer produces, built to blow a recursive walker's stack.
	let (mut child, mut ccrc) = (real_root, real_crc);
	for i in 0..70u64 {
		let blk = 100 + i; // free pool blocks well past the format's layout
		let mut node = vec![0u8; BLOCK_SIZE];
		node_set_header(&mut node, NODE_INTERNAL, 0);
		set_child(&mut node, 0, child, ccrc);
		let start = blk as usize * BLOCK_SIZE;
		dev.blocks[start..start + BLOCK_SIZE].copy_from_slice(&node);
		child = blk;
		ccrc = crc32c(&node);
	}
	let slot = active_slot(&dev);
	forge_superblock(&mut dev, slot, |sb| {
		sb[SB_INODE_ROOT_OFF..SB_INODE_ROOT_OFF + 8].copy_from_slice(&child.to_le_bytes());
		sb[SB_INODE_ROOT_CRC_OFF..SB_INODE_ROOT_CRC_OFF + 4].copy_from_slice(&ccrc.to_le_bytes());
	});
	// the mount's iterative walks handle the depth; the bounded descents refuse it.
	let mut fs = LiberFs::mount(dev).expect("the mount must survive the deep tree");
	assert_eq!(fs.read_file(b"a.txt"), Err(FsError::Corrupt));
	let report = fs.fsck().unwrap();
	assert!(report.checksum_failures >= 1, "fsck reports the hostile shape as damage");
}

#[test]
fn extent_fields_near_the_address_ceiling_cannot_overflow() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"a.txt", b"payload").unwrap();
	let mut dev = fs.into_device();
	// forge the file's first inline extent to sit at the address ceiling: every
	// arithmetic step (end, covers, stored-block loops) must saturate, not overflow.
	forge_inode_slot(&mut dev, |slot| {
		let ext = &mut slot[EXTENT_OFF..EXTENT_OFF + EXTENT_SIZE];
		ext[0..8].copy_from_slice(&(u64::MAX - 2).to_le_bytes()); // logical
		ext[8..16].copy_from_slice(&(u64::MAX - 2).to_le_bytes()); // physical
		ext[16..20].copy_from_slice(&8u32.to_le_bytes()); // length
		ext[32..36].copy_from_slice(&8u32.to_le_bytes()); // store_len
	});
	let mut fs = LiberFs::mount(dev).expect("the mount must survive the forged extent");
	// the moved-away extent no longer covers block 0: the read sees a hole (zeros),
	// bounded garbage rather than a panic.
	assert_eq!(fs.read_file(b"a.txt").unwrap(), vec![0u8; 7]);
}

#[test]
fn a_broken_spill_chain_degrades_the_mount_not_the_volume() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	// the fragmented file first, so it takes inode 1 - the record the forge helper
	// targets; the healthy file follows as inode 2.
	write_spilled_file(&mut fs);
	fs.write_file(b"keep.txt", b"other data").unwrap();
	let mut dev = fs.into_device();
	// flip one raw byte in the fragmented file's spill chain block: before this
	// milestone the failed generation walk FAILED THE MOUNT, and an unmountable
	// volume is what the storage layer reformats - one bit would have cost every
	// file. Now the walk flags the damage and the volume mounts read-only.
	let mut spill = 0u64;
	forge_inode_slot(&mut dev, |slot| {
		spill = u64::from_le_bytes(slot[INO_MAP_OFF..INO_MAP_OFF + 8].try_into().unwrap());
	});
	dev.blocks[spill as usize * BLOCK_SIZE + CHAIN_HDR] ^= 0xFF;
	let mut fs = LiberFs::mount(dev).expect("one damaged chain must not fail the mount");
	assert!(fs.is_read_only(), "an incomplete free map means no allocation: read-only");
	assert_eq!(fs.read_file(b"keep.txt").unwrap(), b"other data", "undamaged files still read");
	assert_eq!(fs.read_file(b"frag.bin"), Err(FsError::Corrupt), "the damaged file reports as itself");
	assert_eq!(fs.write_file(b"new.txt", b"x"), Err(FsError::ReadOnly));
}
