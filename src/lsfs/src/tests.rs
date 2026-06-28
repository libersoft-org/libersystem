// Host tests for LSFS, run with `cd src/lsfs && cargo test`. A Vec-backed block
// device stands in for the disk: a fresh device is formatted, exercised through the
// public API, and re-mounted to prove the on-disk state persists - the in-memory
// analog of surviving a reboot.

use super::*;

// A RAM-backed block device: one contiguous Vec of `num_blocks` blocks. Dropping and
// re-mounting from the same Vec models a reboot (the bytes persist, the in-memory
// filesystem state does not).
struct MemDevice {
	blocks: Vec<u8>,
}

impl MemDevice {
	fn new(num_blocks: u32) -> MemDevice {
		MemDevice { blocks: vec![0u8; num_blocks as usize * BLOCK_SIZE] }
	}
}

impl BlockDevice for MemDevice {
	fn read_block(&mut self, index: u32, buf: &mut [u8]) -> bool {
		let start = index as usize * BLOCK_SIZE;
		let Some(src) = self.blocks.get(start..start + BLOCK_SIZE) else {
			return false;
		};
		buf[..BLOCK_SIZE].copy_from_slice(src);
		true
	}

	fn write_block(&mut self, index: u32, buf: &[u8]) -> bool {
		let start = index as usize * BLOCK_SIZE;
		let Some(dst) = self.blocks.get_mut(start..start + BLOCK_SIZE) else {
			return false;
		};
		dst.copy_from_slice(&buf[..BLOCK_SIZE]);
		true
	}
}

const NBLOCKS: u32 = 64;

#[test]
fn format_then_mount_is_empty() {
	let dev = MemDevice::new(NBLOCKS);
	let fs = Lsfs::format(dev, NBLOCKS).unwrap();
	let dev = fs.into_device();
	let mut fs = Lsfs::mount(dev).unwrap();
	assert!(fs.list().unwrap().is_empty());
	assert_eq!(fs.lookup(b"missing.txt"), None);
}

#[test]
fn mount_rejects_unformatted_device() {
	let dev = MemDevice::new(NBLOCKS);
	assert!(Lsfs::mount(dev).is_none());
}

#[test]
fn write_then_read_round_trips() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"hello.txt", b"Hello, world!").unwrap();
	assert_eq!(fs.read_file(b"hello.txt").unwrap(), b"Hello, world!");
	let listing = fs.list().unwrap();
	assert_eq!(listing.len(), 1);
	assert_eq!(listing[0].0, b"hello.txt");
	assert_eq!(listing[0].1, 13);
}

#[test]
fn data_survives_a_remount() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"motd.txt", b"persist me").unwrap();
	fs.write_file(b"a", b"first").unwrap();
	let dev = fs.into_device();

	// re-mount from the same bytes: the files are still there (a "reboot").
	let mut fs = Lsfs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"motd.txt").unwrap(), b"persist me");
	assert_eq!(fs.read_file(b"a").unwrap(), b"first");
	assert_eq!(fs.list().unwrap().len(), 2);
}

#[test]
fn overwrite_replaces_contents() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"short").unwrap();
	fs.write_file(b"f", b"a much longer replacement payload").unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"a much longer replacement payload");
	// still one entry - overwrite reused the inode.
	assert_eq!(fs.list().unwrap().len(), 1);
}

#[test]
fn remove_deletes_and_frees() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let big: Vec<u8> = (0..(BLOCK_SIZE * 3 + 7)).map(|i| (i % 251) as u8).collect();
	fs.write_file(b"big.bin", &big).unwrap();
	assert_eq!(fs.read_file(b"big.bin").unwrap(), big);
}

#[test]
fn empty_file_round_trips() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"empty", b"").unwrap();
	assert_eq!(fs.read_file(b"empty").unwrap(), b"");
	assert_eq!(fs.list().unwrap()[0].1, 0);
}

#[test]
fn rejects_too_long_a_name() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let long = vec![b'x'; NAME_MAX + 1];
	assert_eq!(fs.write_file(&long, b"data"), Err(FsError::TooLong));
}

#[test]
fn reports_out_of_space() {
	// a tiny filesystem: too few data blocks for an oversized file.
	let small: u32 = 6;
	let mut fs = Lsfs::format(MemDevice::new(small), small).unwrap();
	let payload = vec![b'z'; BLOCK_SIZE * 5];
	assert_eq!(fs.write_file(b"toobig", &payload), Err(FsError::NoSpace));
}

#[test]
fn many_small_files_fill_the_directory() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	blocks: std::collections::HashMap<u32, Vec<u8>>,
	num_blocks: u32,
}

impl SparseDevice {
	fn new(num_blocks: u32) -> SparseDevice {
		SparseDevice { blocks: std::collections::HashMap::new(), num_blocks }
	}
}

impl BlockDevice for SparseDevice {
	fn read_block(&mut self, index: u32, buf: &mut [u8]) -> bool {
		if index >= self.num_blocks {
			return false;
		}
		match self.blocks.get(&index) {
			Some(b) => buf[..BLOCK_SIZE].copy_from_slice(b),
			None => buf[..BLOCK_SIZE].fill(0),
		}
		true
	}

	fn write_block(&mut self, index: u32, buf: &[u8]) -> bool {
		if index >= self.num_blocks {
			return false;
		}
		self.blocks.insert(index, buf[..BLOCK_SIZE].to_vec());
		true
	}
}

#[test]
fn nested_directories_resolve_and_list() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	// the root shows only the top-level directory.
	let root = fs.list().unwrap();
	assert_eq!(root.len(), 1);
	assert_eq!(root[0].0, b"a");
}

#[test]
fn write_creates_missing_parents() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	// no explicit mkdir: write auto-creates the parent chain.
	fs.write_file(b"docs/notes/today.txt", b"hello").unwrap();
	assert_eq!(fs.read_file(b"docs/notes/today.txt").unwrap(), b"hello");
	assert!(fs.lookup(b"docs/notes").is_some());
}

#[test]
fn nested_paths_survive_a_remount() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"etc/motd", b"welcome").unwrap();
	fs.mkdir(b"var/log").unwrap();
	let dev = fs.into_device();
	let mut fs = Lsfs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"etc/motd").unwrap(), b"welcome");
	assert!(fs.lookup(b"var/log").is_some());
}

#[test]
fn remove_rejects_a_nonempty_directory() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"dir/child", b"x").unwrap();
	assert_eq!(fs.remove(b"dir"), Err(FsError::Invalid));
	// removing the child then the now-empty directory works.
	fs.remove(b"dir/child").unwrap();
	fs.remove(b"dir").unwrap();
	assert_eq!(fs.lookup(b"dir"), None);
}

#[test]
fn rejects_dot_and_dot_dot_segments() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	assert_eq!(fs.write_file(b"a/../b", b"x"), Err(FsError::Invalid));
	assert_eq!(fs.read_file(b"./x"), Err(FsError::Invalid));
	assert_eq!(fs.mkdir(b"x//y"), Err(FsError::Invalid));
}

#[test]
fn single_indirect_large_file() {
	// a file past the direct pointers exercises the single indirect block.
	let nblocks: u32 = 128;
	let mut fs = Lsfs::format(MemDevice::new(nblocks), nblocks).unwrap();
	let size = BLOCK_SIZE * (DIRECT + 5) + 123;
	let big: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
	fs.write_file(b"big", &big).unwrap();
	assert_eq!(fs.read_file(b"big").unwrap(), big);
	// remount and re-read to prove the indirect block persisted.
	let dev = fs.into_device();
	let mut fs = Lsfs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"big").unwrap(), big);
	// overwriting with a smaller file frees the indirect chain and reuses the inode.
	fs.write_file(b"big", b"small").unwrap();
	assert_eq!(fs.read_file(b"big").unwrap(), b"small");
}

#[test]
fn double_indirect_large_file() {
	// a file past the direct + single-indirect range reaches the double indirect.
	let nblocks: u32 = (DIRECT + PTRS_PER_BLOCK + 160) as u32;
	let mut fs = Lsfs::format(SparseDevice::new(nblocks), nblocks).unwrap();
	let blocks = DIRECT + PTRS_PER_BLOCK + 3;
	let size = BLOCK_SIZE * blocks;
	let big: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
	fs.write_file(b"huge", &big).unwrap();
	assert_eq!(fs.read_file(b"huge").unwrap(), big);
}

#[test]
fn many_files_across_multiple_inode_blocks() {
	// a volume large enough for far more than one inode block's worth of files.
	let nblocks: u32 = 400;
	let mut fs = Lsfs::format(MemDevice::new(nblocks), nblocks).unwrap();
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
fn multi_block_bitmap_large_volume() {
	// past 32768 blocks the bitmap needs more than one block; a sparse device lets us
	// format such a volume without allocating it whole.
	let nblocks: u32 = 40_000;
	let mut fs = Lsfs::format(SparseDevice::new(nblocks), nblocks).unwrap();
	fs.write_file(b"f", b"on a big volume").unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"on a big volume");
	let dev = fs.into_device();
	let mut fs = Lsfs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"on a big volume");
}

// M50: offset / partial reads and writes.

#[test]
fn write_at_in_the_middle_keeps_the_rest() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"AAAAAAAAAA").unwrap();
	fs.write_at(b"f", 3, b"BBB").unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"AAABBBAAAA");
}

#[test]
fn write_at_can_extend_the_file() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"abc").unwrap();
	fs.write_at(b"f", 3, b"defgh").unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"abcdefgh");
	assert_eq!(fs.stat(b"f").unwrap().size, 8);
}

#[test]
fn write_at_creates_the_file() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_at(b"dir/new.txt", 0, b"fresh").unwrap();
	assert_eq!(fs.read_file(b"dir/new.txt").unwrap(), b"fresh");
}

#[test]
fn write_at_past_the_end_leaves_a_zero_hole() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = Lsfs::mount(dev).unwrap();
	assert_eq!(fs.read_at(b"f", off, 3).unwrap(), b"end");
}

#[test]
fn read_at_clamps_to_the_end() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"0123456789").unwrap();
	assert_eq!(fs.read_at(b"f", 4, 3).unwrap(), b"456");
	assert_eq!(fs.read_at(b"f", 8, 100).unwrap(), b"89");
	assert_eq!(fs.read_at(b"f", 10, 5).unwrap(), b"");
}

#[test]
fn append_grows_across_block_boundaries() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let chunk = vec![b'x'; BLOCK_SIZE - 3];
	fs.append(b"log", &chunk).unwrap();
	fs.append(b"log", b"YYYYYY").unwrap();
	let out = fs.read_file(b"log").unwrap();
	assert_eq!(out.len(), chunk.len() + 6);
	assert_eq!(&out[chunk.len()..], b"YYYYYY");
}

#[test]
fn truncate_shrinks_and_grows() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"a.txt", b"payload").unwrap();
	fs.rename(b"a.txt", b"sub/b.txt").unwrap();
	assert_eq!(fs.lookup(b"a.txt"), None);
	assert_eq!(fs.read_file(b"sub/b.txt").unwrap(), b"payload");
}

#[test]
fn rename_replaces_an_existing_file() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"old/inner/file", b"deep").unwrap();
	fs.rename(b"old", b"new").unwrap();
	assert_eq!(fs.lookup(b"old"), None);
	assert_eq!(fs.read_file(b"new/inner/file").unwrap(), b"deep");
}

#[test]
fn rename_rejects_a_directory_into_its_own_subtree() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"a/b/c").unwrap();
	assert_eq!(fs.rename(b"a", b"a/b/inside"), Err(FsError::Invalid));
	// the tree is untouched.
	assert!(fs.stat(b"a/b/c").unwrap().is_dir);
}

#[test]
fn rename_rejects_overwriting_a_nonempty_directory() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"src", b"x").unwrap();
	fs.write_file(b"dst/keep", b"y").unwrap();
	assert_eq!(fs.rename(b"src", b"dst"), Err(FsError::Invalid));
}

// M50: fsck / orphan reclamation.

#[test]
fn fsck_reclaims_leaked_blocks_and_orphan_inodes() {
	let mut fs = Lsfs::format(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"live", b"keep me").unwrap();
	// simulate a crash mid-write: an inode and a data block are claimed but never
	// linked into any directory. We forge that state through the test-visible internals.
	let orphan = fs.alloc_inode().unwrap();
	let mut leaked = Inode::empty(KIND_FILE);
	let blk = fs.alloc_one().unwrap();
	leaked.direct[0] = blk;
	leaked.size = 1;
	fs.write_inode(orphan, &leaked).unwrap();

	let report = fs.fsck().unwrap();
	assert_eq!(report.reclaimed_inodes, 1);
	assert_eq!(report.reclaimed_blocks, 1);
	// the live file is untouched and the reclaimed inode is free again.
	assert_eq!(fs.read_file(b"live").unwrap(), b"keep me");
	assert!(!fs.is_alloc(blk));
	// a clean filesystem reclaims nothing.
	let again = fs.fsck().unwrap();
	assert_eq!(again.reclaimed_inodes, 0);
	assert_eq!(again.reclaimed_blocks, 0);
}

