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
		let start = index as usize * BLOCK_SIZE;
		let Some(src) = self.blocks.get(start..start + BLOCK_SIZE) else {
			return false;
		};
		buf[..BLOCK_SIZE].copy_from_slice(src);
		true
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		let start = index as usize * BLOCK_SIZE;
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
	assert_eq!(fs.rmdir(b"full"), Err(FsError::Invalid));
	// a regular file is refused (use remove).
	assert_eq!(fs.rmdir(b"file"), Err(FsError::Invalid));
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
	assert_eq!(fs.remove(b"dir"), Err(FsError::Invalid));
	// removing the child then the now-empty directory works.
	fs.remove(b"dir/child").unwrap();
	fs.remove(b"dir").unwrap();
	assert_eq!(fs.lookup(b"dir"), None);
}

#[test]
fn rejects_dot_and_dot_dot_segments() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	assert_eq!(fs.write_file(b"a/../b", b"x"), Err(FsError::Invalid));
	assert_eq!(fs.read_file(b"./x"), Err(FsError::Invalid));
	assert_eq!(fs.mkdir(b"x//y"), Err(FsError::Invalid));
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
	assert_eq!(fs.rename(b"src", b"dst"), Err(FsError::Invalid));
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
	// fsck does not silently drop the still-referenced (if corrupt) block.
	assert_eq!(report.reclaimed_blocks, 0);
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
		assert_eq!(fs.write_file(name, b"x"), Err(FsError::Invalid));
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
	// 1501 blocks: past one extent's 1024-block (4 MiB) checksum cap, so it needs two.
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

	// the named snapshot reads the state captured when it was created.
	let mut snap = LiberFs::mount_named_snapshot(dev, b"before").unwrap();
	assert_eq!(snap.read_file(b"f").unwrap(), b"version one");
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
	assert_eq!(fs.create_snapshot(b""), Err(FsError::Invalid));
	// a name longer than the field is rejected.
	let long = vec![b'a'; SNAP_NAME_MAX + 1];
	assert_eq!(fs.create_snapshot(&long), Err(FsError::TooLong));
	// a duplicate name is rejected.
	fs.create_snapshot(b"dup").unwrap();
	assert_eq!(fs.create_snapshot(b"dup"), Err(FsError::Invalid));
	// deleting an unknown snapshot is NotFound; deleting the real one succeeds.
	assert_eq!(fs.delete_snapshot(b"missing"), Err(FsError::NotFound));
	fs.delete_snapshot(b"dup").unwrap();
	assert!(fs.list_snapshots().unwrap().is_empty());
}

// M57: transparent per-extent compression.

#[test]
fn a_compressible_file_shrinks_and_round_trips() {
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
		assert_eq!(lz_decompress(&lz_compress(&input)), input);
	}
}
