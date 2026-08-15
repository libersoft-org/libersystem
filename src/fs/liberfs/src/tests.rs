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

// A device that refuses to read, so a mount failure can be told apart from a blank disk.
struct DeadDevice;

impl BlockDevice for DeadDevice {
	fn read_block(&mut self, _index: u64, _buf: &mut [u8]) -> bool {
		false
	}
	fn write_block(&mut self, _index: u64, _buf: &[u8]) -> bool {
		false
	}
}

// Fails reads of one chosen block, and counts nothing else.
struct OneBadSlotDevice {
	inner: MemDevice,
	bad: u64,
}

impl BlockDevice for OneBadSlotDevice {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		if index == self.bad {
			return false;
		}
		self.inner.read_block(index, buf)
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		self.inner.write_block(index, buf)
	}

	fn flush(&mut self) -> bool {
		self.inner.flush()
	}
}

#[test]
fn one_good_slot_beside_one_unknown_slot_is_not_a_writable_mount() {
	// the slot that did not answer is the one whose generation is unknown - and a
	// writable mount that proceeds without knowing it will hand out the blocks that
	// generation holds and then overwrite the slot itself. One failed 4 KiB read was
	// enough to destroy a newer, perfectly consistent generation.
	//
	// Two commits, so the two slots hold different generations and both are valid.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"one.txt", b"first").unwrap();
	fs.write_file(b"two.txt", b"second").unwrap();
	let good = fs.into_device();
	let newest = newest_super_slot(&good);
	assert!(LiberFs::mount(good.clone()).unwrap().read_file(b"two.txt").is_ok(), "the untouched image mounts and has both files");

	// the NEWER slot cannot be read. The older one is intact and would mount happily.
	let dev = OneBadSlotDevice { inner: good.clone(), bad: newest as u64 };
	assert_eq!(LiberFs::mount(dev).err(), Some(MountError::Io), "a slot that did not answer is not a slot to write past");

	// and the same when the other slot is a format this build does not know.
	let mut dev = good.clone();
	forge_superblock(&mut dev, newest as usize, |sb| sb[8..12].copy_from_slice(&99u32.to_le_bytes()));
	assert_eq!(LiberFs::mount(dev).err(), Some(MountError::Unsupported), "an older build may not write over a newer format's generation");

	// the recovery door is open, and is read-only.
	let dev = OneBadSlotDevice { inner: good.clone(), bad: newest as u64 };
	let mut rec = LiberFs::mount_recovery(dev).expect("recovery mounts what it can read");
	assert!(rec.is_read_only(), "recovery never writes");
	assert_eq!(rec.read_file(b"one.txt").unwrap(), b"first", "and it can still get the data off");
	assert_eq!(rec.write_file(b"three.txt", b"x"), Err(FsError::ReadOnly));
}

#[test]
fn a_mount_failure_says_which_failure_it_was() {
	// The whole point of the typed error, and the reason it exists: the CALLER formats.
	//
	// `mount` used to answer `None` for a blank disk, an unreadable device, and a volume written by
	// a build with a different layout alike, and the storage service read every one of them as "no
	// filesystem here" and laid down a fresh one. A device that hiccupped at boot was enough to
	// destroy a healthy system volume. These three answers must never be the same answer.
	assert_eq!(LiberFs::mount(DeadDevice).err(), Some(MountError::Io), "a device that will not read is not a blank disk");

	let blank = MemDevice::new(64);
	assert_eq!(LiberFs::mount(blank).err(), Some(MountError::Unformatted), "a disk with no superblock is the one case that may be formatted");

	// Ours, and from a build this one cannot read: the magic matches and the version does not.
	// `into_device` rather than a clone: formatting a COPY leaves the original blank, and the test
	// then measures an unformatted disk while claiming to measure an unsupported one.
	let mut dev = LiberFs::format_scratch(MemDevice::new(64), 64).expect("format").into_device();
	let mut block = vec![0u8; BLOCK_SIZE];
	assert!(dev.read_block(0, &mut block), "read the superblock back");
	let bumped = u32::from_le_bytes(block[SB_VERSION_OFF..SB_VERSION_OFF + 4].try_into().unwrap()) + 1;
	block[SB_VERSION_OFF..SB_VERSION_OFF + 4].copy_from_slice(&bumped.to_le_bytes());
	assert!(dev.write_block(0, &block), "write it back");
	assert!(dev.write_block(1, &block), "and the second slot with it");
	assert_eq!(LiberFs::mount(dev).err(), Some(MountError::Unsupported), "a volume from a build we cannot read is not a blank disk either");
}

#[test]
fn format_then_mount_is_empty() {
	let dev = MemDevice::new(NBLOCKS);
	let fs = LiberFs::format_scratch(dev, NBLOCKS).unwrap();
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert!(fs.list().unwrap().is_empty());
	assert_eq!(fs.lookup(b"missing.txt").unwrap(), None);
}

#[test]
fn the_storage_abis_separator_rule_is_this_backends_rule_too() {
	// THE RULE IS THE ABI'S, and it is stated in `src/idl/storage.lsidl` rather than answered
	// differently by each filesystem: a MIDDLE segment may not be empty, so `a//b` is `BadName` on
	// every volume; a leading or trailing separator is TOLERATED and means the same path without it.
	//
	// This backend used to pass EVERY segment to the validator, so `/a/b` was `BadName` here and an
	// ordinary path on LiberMemFS - one spelling of one path that resolved or did not depending on
	// which filesystem was mounted, which is the thing a shared ABI exists to prevent. The middle
	// case is the one that genuinely has two readings, and it stays refused on both.
	//
	// `libermemfs`'s `a_path_that_tries_to_leave_the_volume_is_refused` asserts the same four
	// answers against the other backend. Two tests rather than one because the point is that they
	// AGREE, and a shared helper would only prove that one implementation calls itself twice.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"d").expect("mkdir");
	fs.write_file(b"/d/x/", b"v").expect("leading and trailing separators are tolerated");
	assert_eq!(fs.read_file(b"d/x").expect("read"), b"v".to_vec());
	assert_eq!(fs.read_file(b"d//x"), Err(FsError::BadName), "a doubled separator is a missing name");
	assert_eq!(fs.write_file(b"d//y", b"v"), Err(FsError::BadName), "and it is refused on the way in, not normalised");
	// The degenerate spellings of the root, which normalising a leading separator must not turn
	// into a name.
	assert_eq!(fs.write_file(b"/", b"v"), Err(FsError::BadName), "the root is not a file");
	assert_eq!(fs.write_file(b"//", b"v"), Err(FsError::BadName), "nor is a path of separators");
}

#[test]
fn mount_rejects_unformatted_device() {
	let dev = MemDevice::new(NBLOCKS);
	assert!(LiberFs::mount(dev).is_err());
}

#[test]
fn write_then_read_round_trips() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"hello.txt", b"Hello, world!").unwrap();
	assert_eq!(fs.read_file(b"hello.txt").unwrap(), b"Hello, world!");
	let listing = fs.list().unwrap();
	assert_eq!(listing.len(), 1);
	assert_eq!(listing[0].0, b"hello.txt");
	assert_eq!(listing[0].1, 13);
}

#[test]
fn data_survives_a_remount() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"short").unwrap();
	fs.write_file(b"f", b"a much longer replacement payload").unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"a much longer replacement payload");
	// still one entry - overwrite reused the inode.
	assert_eq!(fs.list().unwrap().len(), 1);
}

#[test]
fn remove_deletes_and_frees() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"gone.txt", b"temporary").unwrap();
	fs.remove(b"gone.txt").unwrap();
	assert_eq!(fs.lookup(b"gone.txt").unwrap(), None);
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let big: Vec<u8> = (0..(BLOCK_SIZE * 3 + 7)).map(|i| (i % 251) as u8).collect();
	fs.write_file(b"big.bin", &big).unwrap();
	assert_eq!(fs.read_file(b"big.bin").unwrap(), big);
}

#[test]
fn empty_file_round_trips() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"empty", b"").unwrap();
	assert_eq!(fs.read_file(b"empty").unwrap(), b"");
	assert_eq!(fs.list().unwrap()[0].1, 0);
}

#[test]
fn rejects_too_long_a_name() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let long = vec![b'x'; NAME_MAX + 1];
	assert_eq!(fs.write_file(&long, b"data"), Err(FsError::TooLong));
}

#[test]
fn reports_out_of_space() {
	// a tiny filesystem: too few data blocks for an oversized file.
	let small: u64 = 6;
	let mut fs = LiberFs::format_scratch(MemDevice::new(small), small).unwrap();
	let payload = vec![b'z'; BLOCK_SIZE * 5];
	assert_eq!(fs.write_file(b"toobig", &payload), Err(FsError::NoSpace));
}

#[test]
fn many_small_files_fill_the_directory() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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

// Nested directories and capacity scaling.

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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"a/b/c").unwrap();
	fs.write_file(b"a/b/c/file.txt", b"deep").unwrap();
	assert_eq!(fs.read_file(b"a/b/c/file.txt").unwrap(), b"deep");
	// every directory level resolves.
	assert!(fs.lookup(b"a").unwrap().is_some());
	assert!(fs.lookup(b"a/b").unwrap().is_some());
	assert!(fs.lookup(b"a/b/c").unwrap().is_some());
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	assert!(fs.lookup(b"empty").unwrap().is_none());
}

#[test]
fn write_creates_missing_parents() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	// no explicit mkdir: write auto-creates the parent chain.
	fs.write_file(b"docs/notes/today.txt", b"hello").unwrap();
	assert_eq!(fs.read_file(b"docs/notes/today.txt").unwrap(), b"hello");
	assert!(fs.lookup(b"docs/notes").unwrap().is_some());
}

#[test]
fn nested_paths_survive_a_remount() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"etc/motd", b"welcome").unwrap();
	fs.mkdir(b"var/log").unwrap();
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"etc/motd").unwrap(), b"welcome");
	assert!(fs.lookup(b"var/log").unwrap().is_some());
}

#[test]
fn remove_rejects_a_nonempty_directory() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"dir/child", b"x").unwrap();
	assert_eq!(fs.remove(b"dir"), Err(FsError::NotEmpty));
	// removing the child then the now-empty directory works.
	fs.remove(b"dir/child").unwrap();
	fs.remove(b"dir").unwrap();
	assert_eq!(fs.lookup(b"dir").unwrap(), None);
}

#[test]
fn rejects_dot_and_dot_dot_segments() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	assert_eq!(fs.write_file(b"a/../b", b"x"), Err(FsError::BadName));
	assert_eq!(fs.read_file(b"./x"), Err(FsError::BadName));
	assert_eq!(fs.mkdir(b"x//y"), Err(FsError::BadName));
}

#[test]
fn many_files_across_multiple_inode_blocks() {
	// a volume holding far more files than one inode-tree leaf, so the inode B+tree grows
	// past a single node.
	let nblocks: u64 = 400;
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
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
	let mut fs = LiberFs::format_scratch(SparseDevice::new(nblocks), nblocks).unwrap();
	fs.write_file(b"f", b"on a big volume").unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"on a big volume");
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"on a big volume");
}

// Offset / partial reads and writes.

#[test]
fn write_at_in_the_middle_keeps_the_rest() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"AAAAAAAAAA").unwrap();
	fs.write_at(b"f", 3, b"BBB").unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"AAABBBAAAA");
}

#[test]
fn write_at_can_extend_the_file() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"abc").unwrap();
	fs.write_at(b"f", 3, b"defgh").unwrap();
	assert_eq!(fs.read_file(b"f").unwrap(), b"abcdefgh");
	assert_eq!(fs.stat(b"f").unwrap().size, 8);
}

#[test]
fn write_at_creates_the_file() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_at(b"dir/new.txt", 0, b"fresh").unwrap();
	assert_eq!(fs.read_file(b"dir/new.txt").unwrap(), b"fresh");
}

#[test]
fn write_at_past_the_end_leaves_a_zero_hole() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"0123456789").unwrap();
	assert_eq!(fs.read_at(b"f", 4, 3).unwrap(), b"456");
	assert_eq!(fs.read_at(b"f", 8, 100).unwrap(), b"89");
	assert_eq!(fs.read_at(b"f", 10, 5).unwrap(), b"");
}

#[test]
fn append_grows_across_block_boundaries() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let chunk = vec![b'x'; BLOCK_SIZE - 3];
	fs.append(b"log", &chunk).unwrap();
	fs.append(b"log", b"YYYYYY").unwrap();
	let out = fs.read_file(b"log").unwrap();
	assert_eq!(out.len(), chunk.len() + 6);
	assert_eq!(&out[chunk.len()..], b"YYYYYY");
}

#[test]
fn truncate_shrinks_and_grows() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let big: Vec<u8> = vec![7u8; BLOCK_SIZE * 8];
	for _ in 0..30 {
		fs.write_file(b"scratch", &big).unwrap();
		fs.truncate(b"scratch", 0).unwrap();
	}
	assert_eq!(fs.stat(b"scratch").unwrap().size, 0);
}

// Timestamps and stat.

#[test]
fn stat_reports_type_size_and_timestamps() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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

// Rename / move within the volume.

#[test]
fn rename_moves_a_file() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"a.txt", b"payload").unwrap();
	fs.rename(b"a.txt", b"sub/b.txt").unwrap();
	assert_eq!(fs.lookup(b"a.txt").unwrap(), None);
	assert_eq!(fs.read_file(b"sub/b.txt").unwrap(), b"payload");
}

#[test]
fn rename_replaces_an_existing_file() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"src", b"new").unwrap();
	fs.write_file(b"dst", b"old").unwrap();
	fs.rename(b"src", b"dst").unwrap();
	assert_eq!(fs.read_file(b"dst").unwrap(), b"new");
	assert_eq!(fs.lookup(b"src").unwrap(), None);
	// the inode the destination used to hold was freed: churn does not leak it.
	for _ in 0..200 {
		fs.write_file(b"churn", b"x").unwrap();
		fs.remove(b"churn").unwrap();
	}
}

#[test]
fn rename_moves_a_directory_subtree() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"old/inner/file", b"deep").unwrap();
	fs.rename(b"old", b"new").unwrap();
	assert_eq!(fs.lookup(b"old").unwrap(), None);
	assert_eq!(fs.read_file(b"new/inner/file").unwrap(), b"deep");
}

#[test]
fn rename_rejects_a_directory_into_its_own_subtree() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"a/b/c").unwrap();
	assert_eq!(fs.rename(b"a", b"a/b/inside"), Err(FsError::Invalid));
	// the tree is untouched.
	assert!(fs.stat(b"a/b/c").unwrap().is_dir);
}

#[test]
fn rename_rejects_overwriting_a_nonempty_directory() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"src", b"x").unwrap();
	fs.write_file(b"dst/keep", b"y").unwrap();
	assert_eq!(fs.rename(b"src", b"dst"), Err(FsError::NotEmpty));
}

// Block checksums (integrity).

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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let payload: Vec<u8> = (0..(BLOCK_SIZE * 2 + 17)).map(|i| (i % 251) as u8).collect();
	fs.write_file(b"data.bin", &payload).unwrap();
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	// an untouched disk verifies cleanly: every block matches its stored checksum.
	assert_eq!(fs.read_file(b"data.bin").unwrap(), payload);
	assert_eq!(fs.fsck().unwrap().checksum_failures, 0);
}

// Copy-on-write atomicity and snapshots.

// The superblock slot (block 0 or 1) holding the newer generation - the root a clean
// mount would pick. The generation is the little-endian u64 at byte 28 of the slot.
fn newest_super_slot(dev: &MemDevice) -> u32 {
	let generation = |slot: u32| -> u64 {
		let off = slot as usize * BLOCK_SIZE + 28;
		u64::from_le_bytes(dev.blocks[off..off + 8].try_into().unwrap())
	};
	if generation(1) > generation(0) { 1 } else { 0 }
}

#[test]
fn a_torn_commit_keeps_the_previous_file_whole() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"version one").unwrap();
	fs.write_file(b"f", b"version two").unwrap();
	let dev = fs.into_device();

	// the live mount sees the newest write.
	let mut live = LiberFs::mount(dev.clone()).unwrap();
	assert_eq!(live.read_file(b"f").unwrap(), b"version two");

	// the generation one commit back is still reachable, holding the old contents - the
	// groundwork a read-only snapshot is built on.
	let mut snap = LiberFs::mount_snapshot(dev).unwrap().unwrap();
	assert_eq!(snap.read_file(b"f").unwrap(), b"version one");
}

#[test]
fn a_freshly_formatted_volume_has_no_snapshot() {
	let fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let dev = fs.into_device();
	// only generation 0 has ever been written: there is no older root to mount.
	assert!(LiberFs::mount_snapshot(dev).unwrap().is_none());
}

// 64-bit addressing, large files and long names.

#[test]
fn a_long_name_round_trips() {
	// a 255-byte name fills the whole record name field with no terminator.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	// the portable-name policy rejects the punctuation and control
	// bytes, on top of the path separator and NUL the parser already forbids.
	//
	// The SAME list as LiberMemFS's `both_writable_backends_refuse_the_same_names`, deliberately:
	// the policy is one function in `fs/core` now, and the thing worth pinning per backend is that
	// each one reaches it. Two writable filesystems answering differently is what StorageService
	// turned into the same application call succeeding on `vol://system` and failing on an
	// installed volume.
	let bad: [&[u8]; 10] = [b"a\\b", b"a:b", b"a*b", b"a?b", b"a<b", b"a>b", b"a|b", b"a\"b", b"a\x01b", b"a\x7fb"];
	for name in bad {
		assert_eq!(fs.write_file(name, b"x"), Err(FsError::BadName));
	}
	// allowed punctuation, spaces and non-ASCII bytes still work.
	let ok = "resume v2 (final).txt".as_bytes();
	fs.write_file(ok, b"ok").unwrap();
	assert_eq!(fs.read_file(ok).unwrap(), b"ok");
}

// Extents and sparse files.

#[test]
fn large_contiguous_file_uses_few_extents() {
	// a big file written in one shot lands in a contiguous run of data blocks, so the
	// whole thing collapses into a couple of extents instead of a pointer per block.
	let nblocks: u64 = 4096;
	let mut fs = LiberFs::format_scratch(SparseDevice::new(nblocks), nblocks).unwrap();
	// 1501 blocks: past one extent's 1024-block (4 MB) checksum cap, so it needs two.
	let size = BLOCK_SIZE * 1500 + 321;
	let big: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
	fs.write_file(b"big", &big).unwrap();
	assert_eq!(fs.read_file(b"big").unwrap(), big);
	// the 1501 blocks map with two extents, not 1501 pointers.
	let num = fs.lookup(b"big").unwrap().unwrap();
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
	let mut fs = LiberFs::format_scratch(SparseDevice::new(nblocks), nblocks).unwrap();
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

// B+tree directories and dynamic inode allocation.

#[test]
fn a_directory_scales_to_thousands_of_entries() {
	// enough entries to force internal-node splits in both the directory B+tree (keyed by
	// name hash) and the inode B+tree (keyed by number): a leaf holds at most
	// DIR_LEAF_MAX / INODE_LEAF_MAX records and an internal node at most INTERNAL_MAX
	// children. The inode tree's sequential keys leave each split leaf about half full,
	// so a couple of thousand files alone push it past two levels and exercise the
	// internal-node split.
	let nblocks: u64 = 12_000;
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
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
			assert_eq!(fs.lookup(name.as_bytes()).unwrap(), None);
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
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

// Named, pinned snapshots.

#[test]
fn a_named_snapshot_reads_an_earlier_state() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut snap = LiberFs::mount_named_snapshot(dev.clone(), b"before").unwrap().unwrap();
	assert_eq!(snap.read_file(b"f").unwrap(), b"version one");
	let mut live = LiberFs::mount(dev).unwrap();
	assert_eq!(live.read_file_from_snapshot(b"before", b"f").unwrap(), b"version one");
	assert_eq!(live.read_file_from_snapshot(b"missing", b"f"), Err(FsError::NotFound));
	// the re-rooted read leaves the live tree exactly where it was.
	assert_eq!(live.read_file(b"f").unwrap(), b"version two");
}

#[test]
fn snapshots_are_listed_and_survive_a_remount() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"keep.txt", b"original").unwrap();
	fs.create_snapshot(b"backup").unwrap();
	fs.remove(b"keep.txt").unwrap();
	let dev = fs.into_device();

	// the live tree no longer has the file.
	let mut live = LiberFs::mount(dev.clone()).unwrap();
	assert_eq!(live.read_file(b"keep.txt"), Err(FsError::NotFound));

	// the snapshot still holds it, blocks pinned against reclamation.
	let mut snap = LiberFs::mount_named_snapshot(dev, b"backup").unwrap().unwrap();
	assert_eq!(snap.read_file(b"keep.txt").unwrap(), b"original");
}

#[test]
fn the_free_map_honors_every_pinned_generation() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	assert_eq!(LiberFs::mount_named_snapshot(dev.clone(), b"s1").unwrap().unwrap().read_file(b"f").unwrap(), b"one");
	assert_eq!(LiberFs::mount_named_snapshot(dev.clone(), b"s2").unwrap().unwrap().read_file(b"f").unwrap(), b"two");
	assert_eq!(LiberFs::mount_named_snapshot(dev.clone(), b"s3").unwrap().unwrap().read_file(b"f").unwrap(), b"three");

	// the live volume reads the newest content and verifies clean: fsck accounts for
	// every pinned snapshot generation as well as the live tree.
	let mut live = LiberFs::mount(dev).unwrap();
	assert_eq!(live.read_file(b"f").unwrap(), b"live-7");
	assert_eq!(live.fsck().unwrap().checksum_failures, 0);
}

#[test]
fn a_pinned_snapshots_undecodable_stream_is_reported() {
	// `check_inode_tree` ended at `count_corrupt`, which asks the medium whether it returned what
	// was written. So a compressed extent held by a PINNED SNAPSHOT could have every checksum right
	// and a stream that does not decode, and `fsck` reported the volume clean while its own comment
	// said the pinned generations were verified. For a snapshot that matters more than for the live
	// tree: the data may be needed after the live copy is gone.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.set_compression(true);
	fs.write_file(b"c.bin", &[0x41u8; 4 * BLOCK_SIZE]).unwrap();
	fs.create_snapshot(b"s1").unwrap();
	// REMOVED, not rewritten. Rewriting left a live `c.bin` whose own extent `first_extent_of`
	// then found, so the damage landed on the LIVE file and the live scrub reported it - which is
	// why the old assertion ("reported somewhere") passed: it was passing on a checksum failure in
	// the live tree, not on the snapshot's stream. The fixture agreed with the test rather than
	// with what the test was named for.
	//
	// With the file removed, the extent is reachable only through the snapshot, so anything fsck
	// says about it came from the snapshot walk.
	fs.remove(b"c.bin").unwrap();
	assert_eq!(fs.fsck().unwrap().stream_failures, 0, "clean before the damage");

	// Damage the stream and re-stamp every checksum over it, so the medium looks perfect.
	//
	// THE SNAPSHOT'S OWN EXTENT. `first_extent_of` reads the LIVE inode tree, which no longer names
	// this file at all - so it returned some other inode's extent and the damage landed nowhere
	// near the thing under test.
	let mut dev = fs.into_device();
	let (block, csum) = first_extent_of_snapshot(&dev, 0);
	let at = block as usize * BLOCK_SIZE;
	dev.blocks[at + 4..at + 16].copy_from_slice(&[0xFFu8; 12]);
	let fresh = crc32c(&dev.blocks[at..at + BLOCK_SIZE]);
	let cat = csum as usize * BLOCK_SIZE;
	dev.blocks[cat..cat + 4].copy_from_slice(&fresh.to_le_bytes());
	// And the checksum block's own CRC, which lives in the extent record inside the SNAPSHOT's
	// inode tree - so nothing anywhere reports a checksum failure and the only thing wrong with
	// this volume is the stream one snapshot holds.
	let fresh_csum_crc = crc32c(&dev.blocks[cat..cat + BLOCK_SIZE]);
	forge_snapshot_inode_slot(&mut dev, 0, |slot| slot[EXTENT_OFF + 20..EXTENT_OFF + 24].copy_from_slice(&fresh_csum_crc.to_le_bytes()));

	let mut live = LiberFs::mount(dev).unwrap();
	// The snapshot's copy really is unreadable, which is the harm the silence was hiding.
	assert_eq!(live.read_file_from_snapshot(b"s1", b"c.bin"), Err(FsError::Corrupt), "the snapshot's file is unreadable, whatever fsck says");
	let report = live.fsck().unwrap();
	// THE CATEGORY, not the total. This asserted "reported somewhere" - a disjunction over the
	// counters - which is exactly the assertion that cannot detect a MISCATEGORISATION, and
	// `stream_failures`, the counter the fault belongs in, was not even in it. The fault here is a
	// stream that does not decode over blocks that all match their checksums, so it is one thing and
	// the report should say which.
	//
	// The live equivalent got this right in the same round:
	// `a_compressed_extent_with_a_bad_checksum_is_counted_once` asserts all three.
	assert_eq!(report.stream_failures, 1, "an undecodable snapshot stream is a STREAM failure: {report:?}");
	assert_eq!(report.structural_failures, 0, "and not a structural one - the metadata is intact: {report:?}");
	// ALL FOUR COUNTERS, which this could not assert until the fixture's fourth re-stamp was found.
	//
	// The comment here used to record a guess: making a snapshot's medium look perfect takes four
	// re-stamps and only three were done, and the surviving checksum failure was blamed on
	// "something read through a path the raw edits do not reproduce - `read_block_csum_aware`, most
	// likely". It is not the reader. `derive_free` walks the PREVIOUS generation as well as the live
	// one and reads that generation's snapshot table through the CRC the OLDER superblock recorded;
	// copy-on-write only allocates a new snapshot block when the table changes, so both generations
	// pointed at the same block and the edit invalidated both copies of its CRC. Restamping only the
	// active slot left `read_snapshot_table` failing for the previous generation, which sets
	// `walk_damage`, which `derive_free` reports as `Corrupt`, which arrives here as one checksum
	// failure with no path to name. `forge_snapshot_inode_slot` restamps both slots now.
	//
	// So the test's premise - "an undecodable stream over blocks that all match their checksums" -
	// is established by the fixture rather than assumed, and what it pins is that the stream fault
	// is the ONLY thing counted.
	assert_eq!(report.checksum_failures, 0, "every block matches its checksum, which is the premise: {report:?}");
	assert_eq!(report.io_failures, 0, "and the medium answered every read: {report:?}");
	assert!(report.damaged.is_empty(), "nothing in the LIVE tree is damaged: {report:?}");
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
	fs.write_file(b"big", &big).unwrap();
	fs.create_snapshot(b"snap").unwrap();
	fs.remove(b"big").unwrap();
	fs.write_file(b"tmp", b"y").unwrap();
	fs.remove(b"tmp").unwrap();
	let with_snapshot = fill(&mut fs);

	// the same sequence, but delete the snapshot first: big's blocks are reclaimed, so
	// the volume now accepts strictly more fill files.
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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

// Transparent per-extent compression.

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
	let num = fs.lookup(b"big").unwrap().unwrap();
	let ext = fs.read_inode(num).unwrap().extents[0];
	assert!(ext.clen != 0, "expected a compressed extent");
	assert!((ext.store_len as usize) < ext.length as usize, "compressed run should use fewer blocks");
	// it reads back identically across a remount and verifies clean.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"big").unwrap(), big);
	assert_eq!(fs.fsck().unwrap().checksum_failures, 0);
}

// The decompression cache is a small LRU, not a single slot, so a read
// pattern that alternates between a few compressed runs decodes each once instead of
// re-reading its stored blocks on every switch (a single slot thrashed the moment a
// second run touched it). Measured with a device that counts its reads: after both runs
// are primed, re-reading the first costs zero device reads because the intervening read
// of the second did not evict it.
#[test]
fn the_decompression_lru_survives_an_intervening_compressed_read() {
	struct Counting {
		inner: MemDevice,
		reads: u64,
	}
	impl BlockDevice for Counting {
		fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
			self.reads += 1;
			self.inner.read_block(index, buf)
		}
		fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
			self.inner.write_block(index, buf)
		}
	}

	let dev = Counting { inner: MemDevice::new(NBLOCKS), reads: 0 };
	let mut fs = LiberFs::format_opts(dev, NBLOCKS, FormatOpts { compress: true, ..FormatOpts::default() }).unwrap();
	let a: Vec<u8> = b"alpha compresses very well. ".iter().cycle().take(BLOCK_SIZE * 4).copied().collect();
	let b: Vec<u8> = b"beta also compresses nicely. ".iter().cycle().take(BLOCK_SIZE * 4).copied().collect();
	fs.write_file(b"a", &a).unwrap();
	fs.write_file(b"b", &b).unwrap();

	// prime both runs into the cache: each first read decodes its stored blocks.
	assert_eq!(fs.read_file(b"a").unwrap(), a);
	assert_eq!(fs.read_file(b"b").unwrap(), b);

	// read `a` again: it was decoded before `b`, so a single-slot cache would have evicted
	// it and re-read its stored blocks. With the LRU it is still resident - no device read.
	let before = fs.device().reads;
	assert_eq!(fs.read_file(b"a").unwrap(), a);
	let cost = fs.device().reads - before;
	assert_eq!(cost, 0, "an intervening compressed read must not evict `a` from the decompression LRU (cost {cost} reads)");
}

#[test]
fn an_incompressible_file_stays_raw() {
	let mut fs = format_lz(MemDevice::new(NBLOCKS), NBLOCKS);
	let big = noise(BLOCK_SIZE * 4);
	fs.write_file(b"rnd", &big).unwrap();
	assert_eq!(fs.read_file(b"rnd").unwrap(), big);
	// random bytes do not shrink, so the run is stored raw: store_len == length, clen 0.
	let num = fs.lookup(b"rnd").unwrap().unwrap();
	let ext = fs.read_inode(num).unwrap().extents[0];
	assert_eq!(ext.clen, 0);
	assert_eq!(ext.store_len, ext.length);
}

#[test]
fn editing_a_compressed_file_thaws_it() {
	let mut fs = format_lz(MemDevice::new(NBLOCKS), NBLOCKS);
	let mut big: Vec<u8> = b"compress me well, ".iter().cycle().take(BLOCK_SIZE * 4).copied().collect();
	fs.write_file(b"big", &big).unwrap();
	let num = fs.lookup(b"big").unwrap().unwrap();
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
	let num = fs.lookup(b"big").unwrap().unwrap();
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
		assert_eq!(lz_decompress(&lz_compress(&input), input.len()).unwrap(), input);
	}
}

#[test]
fn compression_is_off_by_default_and_togglable() {
	let compressible: Vec<u8> = b"toggle me on and off. ".iter().cycle().take(BLOCK_SIZE * 4).copied().collect();

	// the default volume never compresses: the run stays raw.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	assert!(!fs.compression());
	fs.write_file(b"raw", &compressible).unwrap();
	let num = fs.lookup(b"raw").unwrap().unwrap();
	assert_eq!(fs.read_inode(num).unwrap().extents[0].clen, 0);

	// switched on, a new write compresses; the earlier file keeps its raw form.
	fs.set_compression(true).unwrap();
	assert!(fs.compression());
	fs.write_file(b"packed", &compressible).unwrap();
	let num = fs.lookup(b"packed").unwrap().unwrap();
	assert!(fs.read_inode(num).unwrap().extents[0].clen != 0);
	let raw = fs.lookup(b"raw").unwrap().unwrap();
	assert_eq!(fs.read_inode(raw).unwrap().extents[0].clen, 0);

	// the switch survives a remount, and switching off leaves old compressed files
	// readable while new writes land raw.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).unwrap();
	assert!(fs.compression());
	fs.set_compression(false).unwrap();
	fs.write_file(b"raw2", &compressible).unwrap();
	let num = fs.lookup(b"raw2").unwrap().unwrap();
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
	let fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	assert!(LiberFs::mount(dev).is_err());
}

#[test]
fn names_must_be_utf8() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	// a bare continuation byte is not UTF-8: rejected, so one file has one name.
	assert_eq!(fs.write_file(b"bad\x80name", b"x"), Err(FsError::BadName));
	// real multi-byte UTF-8 works.
	let name = "soubor-\u{10D}e\u{161}tina.txt".as_bytes();
	fs.write_file(name, b"ok").unwrap();
	assert_eq!(fs.read_file(name).unwrap(), b"ok");
}

#[test]
fn fsck_names_a_damaged_file_in_a_subdirectory() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
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
	assert_eq!(LiberFs::mount_named_snapshot(dev.clone(), b"snap00").unwrap().unwrap().read_file(b"f").unwrap(), b"snap00");
	assert_eq!(LiberFs::mount_named_snapshot(dev, b"snap59").unwrap().unwrap().read_file(b"f").unwrap(), b"snap59");
}

// Correctness hardening (flush barriers, read-only mounts, corruption honesty).

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
	let fs = LiberFs::format_scratch(dev, NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"one").unwrap();
	fs.create_snapshot(b"pin").unwrap();
	fs.write_file(b"f", b"two").unwrap();
	let dev = fs.into_device();

	// both snapshot mounts read fine and refuse every mutation.
	let mut prev = LiberFs::mount_snapshot(dev.clone()).unwrap().unwrap();
	assert!(prev.is_read_only());
	assert_eq!(prev.write_file(b"f", b"x"), Err(FsError::ReadOnly));
	let mut named = LiberFs::mount_named_snapshot(dev.clone(), b"pin").unwrap().unwrap();
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
	let num = fs.lookup(b"big").unwrap().unwrap();
	let ext = fs.read_inode(num).unwrap().extents[0];
	assert_eq!(ext.physical, first_data, "the run should start at the first data block");
	assert_eq!(ext.clen, 0, "a run with a bad source block must stay raw");
	// the damage stays detectable: the read fails its checksum and fsck counts it.
	assert_eq!(fs.read_file(b"big"), Err(FsError::Corrupt));
	assert_eq!(fs.fsck().unwrap().checksum_failures, 1);
}

// The incremental free map and the next-fit allocator.

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
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
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
	let num = fs.lookup(b"big").unwrap().unwrap();
	assert_eq!(fs.read_inode(num).unwrap().extents.len(), 1, "the write should land as one contiguous extent");
	assert_eq!(fs.read_file(b"big").unwrap(), big);
}

// The scaling benchmark. Ignored in the normal run (it takes seconds); run with
// `cargo test --release bench_scaling -- --ignored --nocapture` and record the
// numbers in docs/PERF.md. Three costs the benchmark attacks: a large write (the
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
	let mut fs = LiberFs::format_scratch(dev, nblocks).unwrap();

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

// The audit's test-coverage gaps.

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
	let split = dir_split_point(&recs).expect("this leaf has boundaries to split on");
	assert!(split == 1 || split == 5, "split {split} would straddle the equal-hash group");
	assert!(recs[split].hash != recs[split - 1].hash);

	// and a leaf with no boundary at all has no split: every record shares one hash, so
	// any index cuts the group. This used to fall back to index 1 and cut it anyway -
	// routing reached one of the two leaves and every name in the other stayed in the
	// tree, occupying space, findable by nothing.
	let all_one = vec![rec(7, b"bbb", 2), rec(7, b"ccc", 3), rec(7, b"ddd", 4), rec(7, b"eee", 5)];
	assert_eq!(dir_split_point(&all_one), None, "there is no split of a single hash group");

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
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
	// eight sparse spans, far enough apart that each is its own extent: twice the
	// inline capacity, so the map spills.
	let span = |i: u64| i * 16 * BLOCK_SIZE as u64;
	for i in 0..8u64 {
		let payload = format!("span-{i}");
		fs.write_at(b"sparse", span(i), payload.as_bytes()).unwrap();
	}
	let num = fs.lookup(b"sparse").unwrap().unwrap();
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
	let num = fs.lookup(b"big").unwrap().unwrap();
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

// fsck must verify the disk, not the caches.

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
	let mut fs = LiberFs::format_scratch(dev, 512).unwrap();
	// a file with a spill chain (more extents than fit inline), then warm the inode
	// cache by reading it back.
	let span = |i: u64| i * 16 * BLOCK_SIZE as u64;
	for i in 0..8u64 {
		let payload = format!("span-{i}");
		fs.write_at(b"sparse", span(i), payload.as_bytes()).unwrap();
	}
	let num = fs.lookup(b"sparse").unwrap().unwrap();
	let spill = fs.read_inode(num).unwrap().spill;
	assert!(spill != 0, "eight extents should spill");
	assert_eq!(fs.fsck().unwrap().checksum_failures, 0);

	// corrupt the spill block's reads while the inode sits warm in the cache: fsck
	// must reload from the device and surface the damage, not serve the cached map -
	// as a REPORT naming the file, never an error (the report is fsck's whole point).
	// The free-map rederivation hit the same damage, so the volume also degrades to
	// read-only (an incomplete map must never feed an allocation).
	target.set(spill);
	let report = fs.fsck().expect("fsck reports damage, it does not die on it");
	assert!(report.checksum_failures >= 1);
	assert!(report.damaged.contains(&b"sparse".to_vec()), "the damaged file is named");
	assert!(fs.is_read_only(), "free-map damage degrades the volume to read-only");

	// healed device: a clean report again (the caches repopulate from good reads) and
	// reads flow; the read-only degrade stays until a remount confirms the repair.
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
		inode_root: 0x0011_2233_4455_6677,
		inode_root_crc: 0xDEAD_BEEF,
		next_inode: 0x0BAD_F00D,
		root_inode: 0,
		snap_root: 0x0022_3344_5566_7788,
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
	assert_eq!(&block[36..44], &0x0011_2233_4455_6677u64.to_le_bytes(), "inode_root");
	assert_eq!(&block[44..48], &0xDEAD_BEEFu32.to_le_bytes(), "inode_root_crc");
	assert_eq!(&block[52..56], &0u32.to_le_bytes(), "root_inode");
	assert_eq!(&block[60..68], &0x0022_3344_5566_7788u64.to_le_bytes(), "snap_root");
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
	// the parser reads the record back FAITHFULLY, including values no legitimate writer
	// emits. It used to clamp both lengths to one checksum block's coverage here, which
	// meant an impossible extent arrived at the validator looking possible - so the
	// ceiling moved to `check_extent`, where exceeding it is an answer instead of a
	// silent repair.
	assert_eq!((back.logical, back.physical, back.length, back.csum, back.csum_crc, back.store_len, back.clen), (ext.logical, ext.physical, ext.length, ext.csum, ext.csum_crc, ext.store_len, ext.clen));

	// one file inode slot: 256 bytes, header fields then the file overlay.
	let mut inode = Inode::empty(TYPE_FILE);
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
	assert_eq!(slot[0], TYPE_FILE, "type");
	assert_eq!(&slot[8..16], &0x0A0B_0C0D_0E0F_1011u64.to_le_bytes(), "size");
	assert_eq!(&slot[16..24], &0x100u64.to_le_bytes(), "ctime");
	assert_eq!(&slot[24..32], &0x200u64.to_le_bytes(), "mtime");
	assert_eq!(&slot[32..40], &0x0708_090A_0B0C_0D0Eu64.to_le_bytes(), "spill");
	assert_eq!(&slot[40..44], &0x1234_5678u32.to_le_bytes(), "spill_crc");
	assert_eq!(&slot[44..48], &5u32.to_le_bytes(), "extent_count");
	assert_eq!(&slot[56..72], b"owner-tag-16byte", "owner tag");
	assert_eq!(&slot[72..112], &rec, "first inline extent at byte 72");

	// a directory inode overlays its tree root on the same map bytes.
	let mut dir = Inode::empty(TYPE_DIR);
	dir.dir_root = 0x4041_4243_4445_4647;
	dir.dir_root_crc = 0x5051_5253;
	let mut dslot = [0u8; INODE_SIZE];
	dir.write(&mut dslot);
	assert_eq!(dslot[0], TYPE_DIR);
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

// Rewrite the first record of the live snapshot table, repairing every checksum above
// it so the image is internally consistent - the point is always the CONTENT of the
// record, never a checksum failure standing in for it.
fn forge_snapshot_record(dev: &mut MemDevice, f: impl FnOnce(&mut [u8])) {
	forge_snapshot_record_at(dev, 0, f)
}

// The same, for a chosen record of the live table's first block.
fn forge_snapshot_record_at(dev: &mut MemDevice, index: usize, f: impl FnOnce(&mut [u8])) {
	let slot = active_slot(dev);
	let sb = parse_superblock(&dev.blocks[slot * BLOCK_SIZE..(slot + 1) * BLOCK_SIZE]).unwrap();
	let start = sb.snap_root as usize * BLOCK_SIZE;
	let rec = start + SNAP_HDR + index * SNAP_REC;
	f(&mut dev.blocks[rec..rec + SNAP_REC]);
	let crc = crc32c(&dev.blocks[start..start + BLOCK_SIZE]);
	forge_superblock(dev, slot, |sb| sb[SB_SNAP_ROOT_CRC_OFF..SB_SNAP_ROOT_CRC_OFF + 4].copy_from_slice(&crc.to_le_bytes()));
}

// The slot holding the live (higher) generation in a raw device image.
fn active_slot(dev: &MemDevice) -> usize {
	let slot_gen = |s: usize| parse_superblock(&dev.blocks[s * BLOCK_SIZE..(s + 1) * BLOCK_SIZE]).map(|sb| sb.generation);
	if slot_gen(1) > slot_gen(0) { 1 } else { 0 }
}

#[test]
fn an_insane_pool_size_in_the_superblock_is_refused() {
	// a checksummed superblock can still lie about the pool: a claim below the fixed
	// layout is rejected outright, one past the device fails the mount's probe of the
	// last claimed block - either way None, never a panic or an absurd allocation.
	let fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let mut dev = fs.into_device();
	forge_superblock(&mut dev, 0, |sb| sb[SB_NUM_BLOCKS_OFF..SB_NUM_BLOCKS_OFF + 8].copy_from_slice(&0u64.to_le_bytes()));
	assert!(LiberFs::mount(dev.clone()).is_err());
	forge_superblock(&mut dev, 0, |sb| sb[SB_NUM_BLOCKS_OFF..SB_NUM_BLOCKS_OFF + 8].copy_from_slice(&(1u64 << 60).to_le_bytes()));
	assert!(LiberFs::mount(dev).is_err());
}

#[test]
fn a_corrupt_node_count_cannot_panic_the_mount() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	assert!(matches!(outcome, Ok(_) | Err(FsError::NotFound) | Err(FsError::NotDir) | Err(FsError::Corrupt) | Err(FsError::Invalid)));
	let _ = fs.fsck().unwrap();
}

#[test]
fn a_looped_snapshot_chain_cannot_hang_the_mount() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
fn a_truncated_live_file_does_not_poison_the_snapshot_that_still_holds_it() {
	// truncating a compressed file leaves its physical blocks and its compressed stream
	// exactly where they are and only shrinks `length` - so the live extent and the
	// snapshot's extent start at the same block and describe different amounts of data.
	// With the cache keyed on the address alone, reading the live version cached the
	// SHORT decode, and the snapshot then read its first block correctly and got zeros
	// for the rest. `restore_file` could write that back into the live tree as fact.
	let mut fs = format_lz(MemDevice::new(NBLOCKS), NBLOCKS);
	let big: Vec<u8> = b"the quick brown fox jumps over the lazy dog. ".iter().cycle().take(BLOCK_SIZE * 4).copied().collect();
	fs.write_file(b"big", &big).unwrap();
	fs.create_snapshot(b"before").unwrap();
	let num = fs.lookup(b"big").unwrap().unwrap();
	let before = fs.read_inode(num).unwrap().extents[0];
	assert!(before.clen != 0, "expected a compressed extent");

	fs.truncate(b"big", BLOCK_SIZE as u64).unwrap();
	let num = fs.lookup(b"big").unwrap().unwrap();
	let after = fs.read_inode(num).unwrap().extents[0];
	assert_eq!(after.physical, before.physical, "the truncation left the stored blocks alone");
	assert!(after.length < before.length, "and only shrank the logical span");

	// read the LIVE file first, which is what fills the cache with the short decode.
	assert_eq!(fs.read_file(b"big").unwrap(), big[..BLOCK_SIZE]);
	// then the snapshot, whose extent covers all four blocks of the same stream.
	assert_eq!(fs.read_file_from_snapshot(b"before", b"big").unwrap(), big, "the snapshot still holds the whole run");
	// and restoring it must bring back the whole file, not a block and three of zeros.
	fs.restore_file(b"big", b"before").unwrap();
	assert_eq!(fs.read_file(b"big").unwrap(), big);
}

#[test]
fn a_stream_that_stops_early_is_corruption_not_short_data() {
	// the decoder used to return whatever it had managed to decode, and the read path
	// padded the rest of the logical block with zeros and handed it back as the file's
	// contents. Nothing here is bit rot - every physical checksum in this image is
	// repaired to match - so no checksum has an opinion about it. Only the decoder does.
	let mut fs = format_lz(MemDevice::new(NBLOCKS), NBLOCKS);
	let big: Vec<u8> = b"the quick brown fox jumps over the lazy dog. ".iter().cycle().take(BLOCK_SIZE * 4).copied().collect();
	fs.write_file(b"big", &big).unwrap();
	let num = fs.lookup(b"big").unwrap().unwrap();
	let ext = fs.read_inode(num).unwrap().extents[0];
	assert!(ext.clen != 0, "expected a compressed extent");
	let mut dev = fs.into_device();

	// a well-formed stream that simply ENDS: the header says the run's true logical
	// size, then one literal run of three bytes, and `clen` says that is all there is.
	// Nothing about it is malformed - no bad token, no impossible back-reference - so
	// every guard inside the decoder is satisfied and it runs off the end of its input
	// having produced three bytes of the sixteen thousand it promised.
	//
	// This distinction cost a draft: a first version left `clen` alone, so the decoder
	// read on into the zeroed tail, met a zero match offset, and refused THERE. The test
	// passed without the fix it was written for.
	let logical = ext.length as usize * BLOCK_SIZE;
	let mut stream = vec![0u8; ext.store_len as usize * BLOCK_SIZE];
	stream[0..4].copy_from_slice(&(logical as u32).to_le_bytes());
	stream[4] = 3 << 4; // three literals, no match
	stream[5..8].copy_from_slice(b"abc");

	// write it back and repair every checksum above it: the per-block CRCs in the
	// extent's checksum block, and that block's own CRC in the extent record.
	let mut cbuf = vec![0u8; BLOCK_SIZE];
	for k in 0..ext.store_len as usize {
		let blk = &stream[k * BLOCK_SIZE..(k + 1) * BLOCK_SIZE];
		let at = (ext.physical as usize + k) * BLOCK_SIZE;
		dev.blocks[at..at + BLOCK_SIZE].copy_from_slice(blk);
		cbuf[k * 4..k * 4 + 4].copy_from_slice(&crc32c(blk).to_le_bytes());
	}
	let cbuf_crc = crc32c(&cbuf);
	let cat = ext.csum as usize * BLOCK_SIZE;
	dev.blocks[cat..cat + BLOCK_SIZE].copy_from_slice(&cbuf);
	forge_inode_slot(&mut dev, |slot| {
		let mut fixed = ext;
		fixed.csum_crc = cbuf_crc;
		fixed.clen = 8; // header, token, three literals - the stream is over

		fixed.write(&mut slot[EXTENT_OFF..EXTENT_OFF + EXTENT_SIZE]);
	});

	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"big"), Err(FsError::Corrupt), "a stream that stops early is damage, not a short file padded with zeros");
}

#[test]
fn a_lying_compression_header_cannot_allocate_unbounded_memory() {
	// the stream's own length header is attacker-controlled, and it is now REFUSED
	// rather than clamped: a header that disagrees with the run's logical size means the
	// two describe different data, and nothing is allocated on the strength of it.
	let mut src = vec![0u8; 32];
	src[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
	assert_eq!(lz_decompress(&src, BLOCK_SIZE), None);
	// and a legitimate stream still round-trips under its real ceiling.
	let input: Vec<u8> = b"bounded decode ".iter().cycle().take(4500).copied().collect();
	assert_eq!(lz_decompress(&lz_compress(&input), input.len()).unwrap(), input);
}

#[test]
fn a_write_past_the_addressable_end_is_refused() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	assert_eq!(fs.write_at(b"f", u64::MAX - 2, b"abc"), Err(FsError::Invalid));
	// the failed transaction rolled back whole: not even the file was created.
	assert_eq!(fs.lookup(b"f").unwrap(), None);
}

#[test]
fn fsck_reports_metadata_damage_instead_of_dying() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
// Forge the inode record for a CHOSEN inode number, wherever the tree keeps it. `forge_inode_slot`
// reaches the second record of the root leaf, which is only the right slot while the tree is one
// leaf and the volume holds one file.
fn forge_inode_slot_of(dev: &mut MemDevice, num: u32, f: impl FnOnce(&mut [u8])) {
	let slot = active_slot(dev);
	let sb = parse_superblock(&dev.blocks[slot * BLOCK_SIZE..(slot + 1) * BLOCK_SIZE]).unwrap();
	// Walk the inode tree by hand to the leaf holding `num`, re-checksumming the path on the way
	// back out - the point is always the CONTENT of the record, never a checksum failure standing in
	// for it.
	let mut path: Vec<(u64, usize)> = Vec::new();
	let mut ptr = sb.inode_root;
	let mut crc = sb.inode_root_crc;
	loop {
		let start = ptr as usize * BLOCK_SIZE;
		let block = &dev.blocks[start..start + BLOCK_SIZE];
		assert_eq!(crc32c(block), crc, "the tree path must be intact before it is forged");
		if node_type(block) == NODE_LEAF {
			let count = leaf_count(block, INODE_REC);
			let index = (0..count).find(|i| u64::from_le_bytes(block[NODE_HDR + i * INODE_REC..NODE_HDR + i * INODE_REC + 8].try_into().unwrap()) == num as u64).expect("the inode is in the tree");
			let off = start + NODE_HDR + index * INODE_REC + 8;
			f(&mut dev.blocks[off..off + INODE_SIZE]);
			break;
		}
		let ci = route_child(block, internal_count(block), num as u64);
		path.push((ptr, ci));
		let (next, next_crc) = (child_ptr(block, ci), child_crc(block, ci));
		ptr = next;
		crc = next_crc;
	}
	// re-checksum from the leaf up to the root, then the superblock.
	let mut child_hash = {
		let start = ptr as usize * BLOCK_SIZE;
		crc32c(&dev.blocks[start..start + BLOCK_SIZE])
	};
	for &(node, ci) in path.iter().rev() {
		let start = node as usize * BLOCK_SIZE;
		let off = start + INTERNAL_CHILD_BASE + ci * CHILD_SIZE + 8;
		dev.blocks[off..off + 4].copy_from_slice(&child_hash.to_le_bytes());
		child_hash = crc32c(&dev.blocks[start..start + BLOCK_SIZE]);
	}
	forge_superblock(dev, slot, |sb| sb[SB_INODE_ROOT_CRC_OFF..SB_INODE_ROOT_CRC_OFF + 4].copy_from_slice(&child_hash.to_le_bytes()));
}

// The first extent record of the first file inode: its stored block and its checksum block.
// The first extent recorded in a SNAPSHOT's inode tree, and its checksum block.
//
// `first_extent_of` reads the LIVE tree, which is right for a live file and wrong for a test whose
// whole point is a block reachable only through a snapshot: once the file is removed the live tree
// no longer names it, so that helper reads whatever inode happens to occupy the slot and the damage
// lands somewhere unrelated. The old version of the pinned-snapshot test never damaged the
// snapshot's extent at all, and passed on a checksum failure elsewhere - the fixture agreeing with
// the test instead of with what the test was named for.
// `forge_inode_slot` for a SNAPSHOT's tree: edit the first inode record in the generation the
// snapshot record at `index` pins, and re-stamp the two CRCs above it - the leaf's, which the
// snapshot record stores, and the snapshot block's, which the superblock stores.
//
// Three levels, and that is the point: a fixture that damages a snapshot's data without walking all
// three produces a volume whose checksums disagree, which is a DIFFERENT fault from the one under
// test and is what the pinned-snapshot test was accidentally asserting.
fn forge_snapshot_inode_slot(dev: &mut MemDevice, index: usize, f: impl FnOnce(&mut [u8])) {
	let slot = active_slot(dev);
	let sb = parse_superblock(&dev.blocks[slot * BLOCK_SIZE..(slot + 1) * BLOCK_SIZE]).unwrap();
	let snap_start = sb.snap_root as usize * BLOCK_SIZE;
	let rec = snap_start + SNAP_HDR + index * SNAP_REC;
	let root = u64::from_le_bytes(dev.blocks[rec + SNAP_ROOT_OFF..rec + SNAP_ROOT_OFF + 8].try_into().unwrap());
	let leaf_start = root as usize * BLOCK_SIZE;
	let slot_off = leaf_start + NODE_HDR + INODE_REC + 8;
	f(&mut dev.blocks[slot_off..slot_off + INODE_SIZE]);
	let leaf_crc = crc32c(&dev.blocks[leaf_start..leaf_start + BLOCK_SIZE]);
	dev.blocks[rec + SNAP_ROOT_CRC_OFF..rec + SNAP_ROOT_CRC_OFF + 4].copy_from_slice(&leaf_crc.to_le_bytes());
	let snap_crc = crc32c(&dev.blocks[snap_start..snap_start + BLOCK_SIZE]);
	forge_superblock(dev, slot, |sb| sb[SB_SNAP_ROOT_CRC_OFF..SB_SNAP_ROOT_CRC_OFF + 4].copy_from_slice(&snap_crc.to_le_bytes()));
	// AND THE OTHER SUPERBLOCK SLOT, which is the fourth re-stamp this fixture was missing.
	//
	// `derive_free` walks the PREVIOUS generation as well as the live one, and reads that
	// generation's snapshot table through the CRC the older superblock recorded. Copy-on-write only
	// allocates a new snapshot block when the table changes, so both generations usually point at
	// the SAME block - and editing a record inside it invalidates both copies of its CRC. Restamping
	// only the active slot left `read_snapshot_table` failing for the previous generation, which
	// sets `walk_damage`, which `derive_free` reports as `Corrupt`, which `fsck` counts as one
	// checksum failure with no path to name.
	//
	// That is what the comment in `a_pinned_snapshots_undecodable_stream_is_reported` recorded as a
	// guess about `read_block_csum_aware`. It is not the reader: it is a generation the fixture was
	// not editing.
	let other = 1 - slot;
	let previous = parse_superblock(&dev.blocks[other * BLOCK_SIZE..(other + 1) * BLOCK_SIZE]);
	if previous.is_some_and(|sb| sb.snap_root as usize * BLOCK_SIZE == snap_start) {
		forge_superblock(dev, other, |sb| sb[SB_SNAP_ROOT_CRC_OFF..SB_SNAP_ROOT_CRC_OFF + 4].copy_from_slice(&snap_crc.to_le_bytes()));
	}
}

fn first_extent_of_snapshot(dev: &MemDevice, index: usize) -> (u64, u64) {
	let slot = active_slot(dev);
	let sb = parse_superblock(&dev.blocks[slot * BLOCK_SIZE..(slot + 1) * BLOCK_SIZE]).unwrap();
	let rec = sb.snap_root as usize * BLOCK_SIZE + SNAP_HDR + index * SNAP_REC;
	let root = u64::from_le_bytes(dev.blocks[rec + SNAP_ROOT_OFF..rec + SNAP_ROOT_OFF + 8].try_into().unwrap());
	let leaf_start = root as usize * BLOCK_SIZE;
	let slot_off = leaf_start + NODE_HDR + INODE_REC + 8;
	let ext = &dev.blocks[slot_off + EXTENT_OFF..slot_off + EXTENT_OFF + EXTENT_SIZE];
	(u64::from_le_bytes(ext[8..16].try_into().unwrap()), u64::from_le_bytes(ext[24..32].try_into().unwrap()))
}

fn first_extent_of(dev: &MemDevice) -> (u64, u64) {
	let slot = active_slot(dev);
	let sb = parse_superblock(&dev.blocks[slot * BLOCK_SIZE..(slot + 1) * BLOCK_SIZE]).unwrap();
	let leaf_start = sb.inode_root as usize * BLOCK_SIZE;
	let slot_off = leaf_start + NODE_HDR + INODE_REC + 8;
	let ext = &dev.blocks[slot_off + EXTENT_OFF..slot_off + EXTENT_OFF + EXTENT_SIZE];
	(u64::from_le_bytes(ext[8..16].try_into().unwrap()), u64::from_le_bytes(ext[24..32].try_into().unwrap()))
}

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

// Stamp `count` into the spill chain block of the spilled file and re-checksum the whole
// forgery chain (chain -> inode slot -> leaf -> superblock), so the forged block is what
// every walk and the read path actually see.
fn forge_spill_count(dev: &mut MemDevice, count: u32) {
	let mut spill = 0u64;
	forge_inode_slot(dev, |slot| {
		spill = u64::from_le_bytes(slot[INO_MAP_OFF..INO_MAP_OFF + 8].try_into().unwrap());
	});
	let start = spill as usize * BLOCK_SIZE;
	dev.blocks[start + CHAIN_COUNT_OFF..start + CHAIN_COUNT_OFF + 4].copy_from_slice(&count.to_le_bytes());
	let chain_crc = crc32c(&dev.blocks[start..start + BLOCK_SIZE]);
	forge_inode_slot(dev, |slot| {
		slot[INO_MAP_CRC_OFF..INO_MAP_CRC_OFF + 4].copy_from_slice(&chain_crc.to_le_bytes());
	});
}

#[test]
fn a_forged_spill_count_is_refused_rather_than_trimmed() {
	// An impossible record count in a chain block used to be CLAMPED - to what a block can
	// hold and to what the inode was still missing - so a checksum-consistent forgery
	// claiming more records than exist was quietly normalised into a possible one, and the
	// structural pass had nothing left to disagree with. `Extent::parse` was given the
	// other treatment (keep what is on the medium, refuse what cannot be true) and this
	// half was not.
	//
	// Both directions of impossible are covered: more than fits in a block, and more than
	// the inode says is still missing.
	for count in [u32::MAX, (EXTENTS_PER_BLOCK + 1) as u32, 3] {
		let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
		write_spilled_file(&mut fs);
		let mut dev = fs.into_device();
		forge_spill_count(&mut dev, count);
		// the mount still SURVIVES - it degrades to read-only, because a generation walk
		// that could not complete leaves the free map incomplete - and the file the forgery
		// is attached to reads as the damage it is instead of as a shorter file.
		let mut fs = LiberFs::mount(dev).expect("the mount must survive the forged count");
		assert!(fs.is_read_only(), "a chain the walk could not read leaves the map incomplete: {count}");
		assert_eq!(fs.read_file(b"frag.bin"), Err(FsError::Corrupt), "an impossible chain count is refused, not trimmed: {count}");
		// and fsck says so rather than reporting a file that is simply shorter than it was.
		let report = fs.fsck().unwrap();
		assert!(mentions(&report.faults, b"spill chain could not be read"), "fsck names the chain: {:?}", report.faults);
	}
}

#[test]
fn an_honest_spill_count_still_reads() {
	// the other side of the refusal above: the count `flush_extents` actually writes is
	// left alone, so re-stamping it changes nothing and the file reads back whole.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let expected = write_spilled_file(&mut fs);
	let mut dev = fs.into_device();
	forge_spill_count(&mut dev, 2);
	let mut fs = LiberFs::mount(dev).expect("an honest chain mounts");
	assert!(!fs.is_read_only(), "nothing about this volume is damaged");
	assert_eq!(fs.read_file(b"frag.bin").unwrap(), expected, "the real extents still read");
}

#[test]
fn a_sparse_size_past_the_pool_cannot_demand_the_moon() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	// a LEGITIMATE sparse file sized past the pool's byte count: a whole-file read could neither
	// allocate nor fill the buffer, so it is refused - while an explicit-length read of the written
	// range still works.
	//
	// The refusal says TooLarge rather than Corrupt, which is the difference between "ask for a
	// range" and "this volume is damaged". The test itself calls the file legitimate in the line
	// above, and then asserted the medium was inconsistent about it.
	let past_pool = NBLOCKS * BLOCK_SIZE as u64 + 40_000;
	fs.write_at(b"sparse.bin", past_pool, b"tail").unwrap();
	assert_eq!(fs.read_file(b"sparse.bin"), Err(FsError::TooLarge));
	assert_eq!(fs.read_at(b"sparse.bin", past_pool, 4).unwrap(), b"tail");
}

#[test]
fn a_looped_namespace_cannot_hang_the_walks() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"a/b").unwrap();
	fs.write_file(b"a/b/f.txt", b"payload").unwrap();
	// forge a namespace cycle through the crate's own machinery: an entry in a/b
	// pointing back at the root directory (a legitimate tree is acyclic; a hostile
	// volume need not be).
	let sub = fs.lookup(b"a/b").unwrap().unwrap();
	fs.mutate(|fs| fs.dir_insert(sub, b"up", ROOT_INODE)).unwrap();
	// fsck's namespace walk terminates (visited set) and reports no false damage...
	let report = fs.fsck().unwrap();
	assert_eq!(report.checksum_failures, 0);
	// ...and the rename cycle check terminates too, still refusing the move.
	//
	// `ReadOnly` rather than `Invalid` since 2026-08-12, and the change is the point: an entry
	// naming the root is an ALIAS of an inode that may have no names, so the mount now refuses to
	// write to this volume at all rather than refusing this one move. The termination this test is
	// named for is unaffected - both walks still return - and the refusal is earlier and wider.
	assert_eq!(fs.rename(b"a", b"a/b/x"), Err(FsError::ReadOnly));
	assert!(fs.is_read_only(), "a name resolving to the root took the volume read-only");
	// The walk that matters here still finishes, over the loop, with the volume in that state.
	assert!(fs.fsck().is_ok(), "and fsck still terminates over the cycle");
}

#[test]
fn a_pathologically_deep_tree_is_refused_not_overflowed() {
	let pool = 256u64;
	let mut fs = LiberFs::format_scratch(MemDevice::new(pool), pool).unwrap();
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
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
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
	// the out-of-pool stored blocks fail their reads as Io: to the operator that is
	// damage like any other - fsck counts it, names the file, and keeps walking.
	let report = fs.fsck().expect("an unreadable block must not kill the report");
	assert!(report.checksum_failures >= 1);
	assert!(report.damaged.contains(&b"a.txt".to_vec()));
}

#[test]
fn a_broken_spill_chain_degrades_the_mount_not_the_volume() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	// the fragmented file first, so it takes inode 1 - the record the forge helper
	// targets; the healthy file follows as inode 2.
	write_spilled_file(&mut fs);
	fs.write_file(b"keep.txt", b"other data").unwrap();
	let mut dev = fs.into_device();
	// flip one raw byte in the fragmented file's spill chain block: before this
	// fix the failed generation walk FAILED THE MOUNT, and an unmountable
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
	// and fsck on the degraded volume still hands the operator a report naming the
	// damage - the exact volume a report matters most for.
	let report = fs.fsck().expect("fsck must survive what the mount survived");
	assert!(report.checksum_failures >= 1);
	assert!(report.damaged.contains(&b"frag.bin".to_vec()));
}

#[test]
fn snapshot_names_must_be_utf8() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", b"x").unwrap();
	// the on-disk snapshot record is specified as UTF-8, like file names: byte soup
	// is refused at the crate boundary, not left for a foreign driver to choke on.
	assert_eq!(fs.create_snapshot(b"\xFF\xFEsoup"), Err(FsError::BadName));
	// an embedded NUL is valid UTF-8 but the record is NUL-padded: the name would
	// silently truncate - and change identity - at the next mount. Refused too.
	assert_eq!(fs.create_snapshot(b"a\0b"), Err(FsError::BadName));
	// valid UTF-8 beyond ASCII is a name like any other.
	fs.create_snapshot("z\u{00E1}loha".as_bytes()).unwrap();
	assert_eq!(fs.list_snapshots().unwrap().len(), 1);
}

#[test]
fn a_dangling_entry_is_reported_listable_around_and_removable() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"healthy.txt", b"ok").unwrap();
	fs.write_file(b"ghost.txt", b"gone").unwrap();
	let num = fs.lookup(b"ghost.txt").unwrap().unwrap();
	// forge the dangle through the crate's own machinery: drop the inode record and
	// leave the directory entry (a legitimate writer commits both atomically, so this
	// shape only exists on a hostile or corrupt volume).
	fs.mutate(|fs| {
		let inode = fs.read_inode(num)?;
		fs.drop_inode_blocks(&inode)?;
		fs.free_inode(num)
	})
	.unwrap();
	// fsck names the dangling entry instead of dying on it...
	let report = fs.fsck().unwrap();
	// ...AS STRUCTURAL DAMAGE, which is what it is. This asserted `checksum_failures` until
	// 2026-08-15, and the assertion was the taxonomy being wrong rather than the code: a directory
	// entry naming an inode that does not exist is the NAMESPACE being inconsistent, not the bytes
	// failing their checksum. An operator reading a report that says "checksum failure" reaches for
	// the medium; this fault is not about the medium.
	assert_eq!(report.checksum_failures, 0, "nothing here failed a checksum");
	assert!(report.structural_failures >= 1, "the dangling entry is structural damage");
	assert_eq!(report.io_failures, 0, "and the disk answered every read");
	assert!(report.damaged.contains(&b"ghost.txt".to_vec()));
	// ...the directory still lists its healthy entries around it...
	let names: Vec<Vec<u8>> = fs.list().unwrap().into_iter().map(|(n, _, _, _, _)| n).collect();
	assert!(names.contains(&b"healthy.txt".to_vec()));
	assert!(!names.contains(&b"ghost.txt".to_vec()));
	// ...and `remove` clears the name: the repair verb for what fsck named.
	fs.remove(b"ghost.txt").unwrap();
	assert_eq!(fs.lookup(b"ghost.txt").unwrap(), None);
	assert_eq!(fs.fsck().unwrap().checksum_failures, 0);
	assert_eq!(fs.read_file(b"healthy.txt").unwrap(), b"ok");
}

// The audit track's standing guard: deterministic random corruption over a volume
// carrying every on-disk structure. Reading code proves what a reviewer thought of;
// this probes what nobody thought of - and it must hold on every future change.

// A splitmix64 PRNG: the fuzz smoke test must reproduce exactly, so failures are
// debuggable by seed, never flaky.
struct Rng(u64);

impl Rng {
	fn next(&mut self) -> u64 {
		self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
		let mut z = self.0;
		z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
		z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
		z ^ (z >> 31)
	}
}

#[test]
fn random_corruption_never_panics_or_hangs() {
	// a rich volume, built once: nested directories, a compressed file, a fragmented
	// (spilled) file, and a snapshot - every structure the mounts and walks touch.
	let pool = 256u64;
	let mut fs = format_lz(MemDevice::new(pool), pool);
	fs.mkdir(b"docs/sub").unwrap();
	fs.write_file(b"docs/a.txt", b"payload one").unwrap();
	let compressible: Vec<u8> = b"compress me. ".iter().cycle().take(BLOCK_SIZE * 6).copied().collect();
	fs.write_file(b"docs/sub/b.txt", &compressible).unwrap();
	write_spilled_file(&mut fs);
	fs.create_snapshot(b"snap").unwrap();
	let pristine = fs.into_device();

	let mut rng = Rng(0x4C69_6265_7246_5321);
	for _ in 0..300 {
		// flip 1..=24 random bytes anywhere in the image, superblocks included.
		let mut dev = pristine.clone();
		let flips = 1 + (rng.next() % 24) as usize;
		for _ in 0..flips {
			let at = (rng.next() % dev.blocks.len() as u64) as usize;
			dev.blocks[at] ^= (rng.next() % 255 + 1) as u8;
		}
		// whatever the damage: the mount refuses, degrades, or serves - and every
		// probe completes with a Result, never a panic, hang, or blow-up.
		let Ok(mut fs) = LiberFs::mount(dev) else {
			continue;
		};
		let _ = fs.fsck();
		let _ = fs.list();
		let _ = fs.read_dir(b"docs");
		let _ = fs.read_file(b"docs/a.txt");
		let _ = fs.read_file(b"docs/sub/b.txt");
		let _ = fs.read_file(b"frag.bin");
		let _ = fs.list_snapshots();
		let _ = fs.read_file_from_snapshot(b"snap", b"docs/a.txt");
		let _ = fs.write_file(b"probe.txt", b"probe");
		let _ = fs.remove(b"docs/a.txt");
	}
}

// The revisit under the fs-track discipline.

// A MemDevice whose flush fails once a superblock has been written: the commit's data
// barrier passes, the commit point lands, and the durability barrier after it reports
// failure - the device-degrading moment the commit must survive without rolling back.
// Fails the NEXT barrier and then behaves. Enough to roll one transaction back and
// still commit the one after it, which is what it takes to see state that survived a
// rollback reach the disk on an unrelated write.
struct FailOneFlushDevice {
	inner: MemDevice,
	fail_next: bool,
}

impl BlockDevice for FailOneFlushDevice {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		self.inner.read_block(index, buf)
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		self.inner.write_block(index, buf)
	}

	fn flush(&mut self) -> bool {
		if self.fail_next {
			self.fail_next = false;
			return false;
		}
		self.inner.flush()
	}
}

#[test]
fn one_byte_at_the_last_addressable_offset() {
	// the guard on `offset + len` passes here - the sum is exactly u64::MAX - and
	// everything downstream used to add BLOCK_SIZE to an absolute position that already
	// sits inside the last block of the address space. Debug panics; release wraps, and
	// a wrapped copy range writes the byte somewhere else entirely.
	//
	// The existing ceiling test uses `u64::MAX - 2` with three bytes, so the FIRST guard
	// refuses it and none of this arithmetic is ever reached.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_at(b"top.bin", u64::MAX - 1, b"Z").unwrap();
	assert_eq!(fs.read_at(b"top.bin", u64::MAX - 1, 1).unwrap(), b"Z");
	// the rest of the file is a hole, and in particular the byte did not wrap to zero.
	assert_eq!(fs.read_at(b"top.bin", 0, 1).unwrap(), b"\0");
	// one past it is still refused, which is the guard doing its own job.
	assert_eq!(fs.write_at(b"top.bin", u64::MAX, b"Z"), Err(FsError::Invalid));
}

#[test]
fn a_rolled_back_compression_change_does_not_survive_in_memory() {
	// `set_compression` changes the switch INSIDE the transaction, and the transaction
	// record did not carry it - so a commit that failed at its first barrier told the
	// caller `Io`, left the disk without the change, and left the filesystem in memory
	// reporting it as made. The next unrelated commit then wrote it to the superblock.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"keep.txt", b"payload").unwrap();
	assert!(!fs.compress, "this volume starts uncompressed");
	let dev = FailOneFlushDevice { inner: fs.into_device(), fail_next: false };
	let mut fs = LiberFs::mount(dev).unwrap();

	fs.dev.fail_next = true;
	assert_eq!(fs.set_compression(true), Err(FsError::Io), "the barrier failed, so the change did not commit");
	assert!(!fs.compress, "and a change that did not commit is not a change");

	// the unrelated commit that used to carry it to disk.
	fs.write_file(b"other.txt", b"more").unwrap();
	let dev = fs.into_device().inner;
	let fs = LiberFs::mount(dev).unwrap();
	assert!(!fs.compress, "nothing may write a rolled-back setting into the superblock later");
}

struct FailFlushDevice {
	inner: MemDevice,
	sb_written: bool,
}

impl BlockDevice for FailFlushDevice {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		self.inner.read_block(index, buf)
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		if index < POOL_START {
			self.sb_written = true;
		}
		self.inner.write_block(index, buf)
	}

	fn flush(&mut self) -> bool {
		!self.sb_written
	}
}

#[test]
fn a_failed_durability_flush_adopts_the_commit_read_only() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"old.txt", b"committed").unwrap();
	let dev = FailFlushDevice { inner: fs.into_device(), sb_written: false };
	let mut fs = LiberFs::mount(dev).unwrap();

	// the transaction's blocks and the superblock land; only the flush after the
	// superblock reports failure. The superblock may be durable regardless of the
	// report, so the commit must NOT roll back - a rollback would return the fresh
	// blocks to the pool while the medium may name them, and a later transaction
	// would overwrite a mountable generation's trees. The filesystem adopts the new
	// generation and the failure costs writability instead.
	//
	// And it is reported as its own thing rather than as `Io`, because a caller told `Io` retries
	// - which is the one response that must not happen against a volume whose generation may
	// already have moved past what the caller read.
	assert_eq!(fs.write_file(b"new.txt", b"landed"), Err(FsError::CommitUncertain));
	assert!(fs.is_read_only(), "an uncertain commit degrades the volume to read-only");
	assert_eq!(fs.read_file(b"new.txt").unwrap(), b"landed", "the in-memory state matches the attempted commit");
	assert_eq!(fs.write_file(b"more.txt", b"nope"), Err(FsError::ReadOnly));

	// on this device the superblock did land: a remount stands on the new
	// generation, whole - exactly the state the mount adopted.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev.inner).unwrap();
	assert_eq!(fs.read_file(b"new.txt").unwrap(), b"landed");
	assert_eq!(fs.read_file(b"old.txt").unwrap(), b"committed");
}

// Solve for the four bytes at `off` of `block` such that the block's CRC32C equals
// those bytes read back as a u32 - the fixpoint a hostile author computes offline to
// build a chain block whose stored CRC vouches for itself. CRC32C is affine over
// GF(2), so the fixpoint is a 32-unknown linear system, solved here by an XOR basis;
// the map is not always invertible, so the block's last four (parser-ignored) bytes
// are tweaked until a solvable system comes up.
fn crc_fixpoint(block: &mut [u8], off: usize) {
	let f = |x: u32, block: &mut [u8]| -> u32 {
		block[off..off + 4].copy_from_slice(&x.to_le_bytes());
		crc32c(block)
	};
	let tweak_off = block.len() - 4;
	for t in 0u32.. {
		block[tweak_off..tweak_off + 4].copy_from_slice(&t.to_le_bytes());
		// want crc(block(x)) == x, i.e. (L xor I)(x) == f(0) with L the linear part.
		let c = f(0, block);
		let mut basis = [(0u32, 0u32); 32];
		for i in 0..32 {
			let mut v = f(1u32 << i, block) ^ c ^ (1u32 << i);
			let mut m = 1u32 << i;
			while v != 0 {
				let lead = (31 - v.leading_zeros()) as usize;
				if basis[lead].0 == 0 {
					basis[lead] = (v, m);
					break;
				}
				v ^= basis[lead].0;
				m ^= basis[lead].1;
			}
		}
		let mut r = c;
		let mut x = 0u32;
		let mut stuck = false;
		while r != 0 {
			let lead = (31 - r.leading_zeros()) as usize;
			if basis[lead].0 == 0 {
				stuck = true;
				break;
			}
			r ^= basis[lead].0;
			x ^= basis[lead].1;
		}
		if stuck {
			continue;
		}
		f(x, block);
		assert_eq!(crc32c(block), u32::from_le_bytes(block[off..off + 4].try_into().unwrap()));
		return;
	}
	unreachable!("some tweak yields a solvable fixpoint system");
}

#[test]
fn a_crc_consistent_snapshot_chain_cycle_terminates() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"keep.txt", b"pinned").unwrap();
	fs.create_snapshot(b"snap").unwrap();
	let snap_root = fs.snap_root;
	let mut dev = fs.into_device();

	// craft a self-referential chain block: next points at the block itself and the
	// CRC field holds the block's own CRC32C (the offline fixpoint), so every step of
	// an unbounded walk would pass its integrity check - a checksum proves integrity,
	// not sanity. The walk must terminate by the pool bound, not hang or grow the
	// table without limit.
	let start = snap_root as usize * BLOCK_SIZE;
	let block = &mut dev.blocks[start..start + BLOCK_SIZE];
	block[CHAIN_NEXT_OFF..CHAIN_NEXT_OFF + 8].copy_from_slice(&snap_root.to_le_bytes());
	crc_fixpoint(block, CHAIN_CRC_OFF);
	let crc = crc32c(&dev.blocks[start..start + BLOCK_SIZE]);
	let slot = newest_super_slot(&dev) as usize;
	forge_superblock(&mut dev, slot, |sb| sb[SB_SNAP_ROOT_CRC_OFF..SB_SNAP_ROOT_CRC_OFF + 4].copy_from_slice(&crc.to_le_bytes()));

	// the mount terminates and degrades to read-only, like any snapshot-table damage.
	let mut fs = LiberFs::mount(dev).unwrap();
	assert!(fs.is_read_only());
	assert_eq!(fs.read_file(b"keep.txt").unwrap(), b"pinned");
}

#[test]
fn a_crc_consistent_spill_chain_cycle_terminates() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	write_spilled_file(&mut fs);
	let mut dev = fs.into_device();

	// the same self-vouching cycle in a file's extent overflow chain: the extent
	// pushes stop once the inode's count is satisfied, but an unbounded walk would
	// follow the next pointers forever on every read of the inode.
	let mut spill = 0u64;
	forge_inode_slot(&mut dev, |slot| {
		spill = u64::from_le_bytes(slot[INO_MAP_OFF..INO_MAP_OFF + 8].try_into().unwrap());
	});
	let start = spill as usize * BLOCK_SIZE;
	{
		let block = &mut dev.blocks[start..start + BLOCK_SIZE];
		block[CHAIN_NEXT_OFF..CHAIN_NEXT_OFF + 8].copy_from_slice(&spill.to_le_bytes());
		crc_fixpoint(block, CHAIN_CRC_OFF);
	}
	let chain_crc = crc32c(&dev.blocks[start..start + BLOCK_SIZE]);
	forge_inode_slot(&mut dev, |slot| {
		slot[INO_MAP_CRC_OFF..INO_MAP_CRC_OFF + 4].copy_from_slice(&chain_crc.to_le_bytes());
	});

	// the mount's own walk hits the cycle too (it loads every file's spill chain), so
	// it degrades to read-only - and the read surfaces the damage instead of hanging.
	let mut fs = LiberFs::mount(dev).expect("the mount must terminate");
	assert_eq!(fs.read_file(b"frag.bin"), Err(FsError::Corrupt));
}

#[test]
fn an_out_of_pool_pointer_reads_as_damage_not_foreign_bytes() {
	// a 64-block pool on a 128-block device: the blocks past the pool stand in for
	// another partition on a shared disk.
	let mut fs = LiberFs::format_scratch(MemDevice::new(128), NBLOCKS).unwrap();
	fs.write_file(b"f.bin", &noise(BLOCK_SIZE)).unwrap();
	let mut dev = fs.into_device();

	// plant "foreign partition" bytes past the pool, beside a checksum block whose
	// first slot vouches for them - the full forgery chain, every CRC matching. The
	// gate, not the checksums, must keep the foreign bytes from surfacing.
	let foreign = vec![0x5Au8; BLOCK_SIZE];
	dev.blocks[100 * BLOCK_SIZE..101 * BLOCK_SIZE].copy_from_slice(&foreign);
	let mut cbuf = vec![0u8; BLOCK_SIZE];
	cbuf[0..4].copy_from_slice(&crc32c(&foreign).to_le_bytes());
	let cbuf_crc = crc32c(&cbuf);
	dev.blocks[101 * BLOCK_SIZE..102 * BLOCK_SIZE].copy_from_slice(&cbuf);

	// re-point the file's first extent at the foreign blocks.
	forge_inode_slot(&mut dev, |slot| {
		let ext = Extent { logical: 0, physical: 100, length: 1, csum: 101, csum_crc: cbuf_crc, store_len: 1, clen: 0 };
		ext.write(&mut slot[EXTENT_OFF..EXTENT_OFF + EXTENT_SIZE]);
	});
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"f.bin"), Err(FsError::Corrupt), "an out-of-pool run is damage, not another partition's data");

	// the same gate on tree nodes: an inode root past the pool, its CRC matching the
	// foreign bytes, must never surface them as names.
	//
	// This is now caught EARLIER than it used to be, and the assertion moved with it. A
	// superblock naming a root outside its own pool is not a superblock this build will
	// accept, so the slot is rejected outright and the mount falls back to the other one
	// - which is the whole point of keeping two. The foreign bytes are then unreachable
	// because nothing points at them at all, rather than because a later gate refused to
	// follow the pointer.
	let mut dev = fs.into_device();
	let slot = newest_super_slot(&dev) as usize;
	forge_superblock(&mut dev, slot, |sb| {
		sb[SB_INODE_ROOT_OFF..SB_INODE_ROOT_OFF + 8].copy_from_slice(&100u64.to_le_bytes());
		sb[SB_INODE_ROOT_CRC_OFF..SB_INODE_ROOT_CRC_OFF + 4].copy_from_slice(&crc32c(&foreign).to_le_bytes());
	});
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"f.bin"), Err(FsError::NotFound), "the fallback generation predates the file");
	// and nothing in the volume reads back as the planted bytes.
	for (name, ..) in fs.list().unwrap() {
		assert_ne!(fs.read_file(&name).unwrap_or_default(), foreign, "no name may yield the foreign bytes");
	}

	// both slots forged is a corrupt volume, not an unformatted one - so nothing formats
	// over it.
	let mut dev = fs.into_device();
	for slot in 0..SUPER_SLOTS as usize {
		forge_superblock(&mut dev, slot, |sb| {
			sb[SB_INODE_ROOT_OFF..SB_INODE_ROOT_OFF + 8].copy_from_slice(&100u64.to_le_bytes());
		});
	}
	assert_eq!(LiberFs::mount(dev).err(), Some(MountError::Corrupt), "a volume whose every slot is impossible is corrupt, never blank");
}

#[test]
fn a_named_snapshot_mount_does_not_answer_from_the_live_generation() {
	// switching to a snapshot swaps the inode root, which switches the generation - and
	// left the inode and directory caches holding entries decoded from the live tree.
	// It was latent only because nothing populated them between the mount and the swap;
	// reading a single inode during the mount was enough to make the snapshot resolve
	// every path through the LIVE root directory.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"keep.txt", b"original").unwrap();
	fs.create_snapshot(b"backup").unwrap();
	fs.remove(b"keep.txt").unwrap();
	let dev = fs.into_device();

	// warm every cache on the live generation first, then switch.
	let mut live = LiberFs::mount(dev.clone()).unwrap();
	assert_eq!(live.read_file(b"keep.txt"), Err(FsError::NotFound));
	assert!(live.list().unwrap().is_empty(), "the live generation really has nothing");
	drop(live);

	let mut snap = LiberFs::mount_named_snapshot(dev, b"backup").expect("the volume mounts").expect("the snapshot is there");
	assert_eq!(snap.read_file(b"keep.txt").unwrap(), b"original", "the snapshot answers from its own tree");
	assert_eq!(snap.list().unwrap().len(), 1, "and its own directory");
}

// Fails every read of one chosen block, once armed.
struct FailBlockDevice {
	inner: MemDevice,
	fail: Option<u64>,
}

impl BlockDevice for FailBlockDevice {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		if self.fail == Some(index) {
			return false;
		}
		self.inner.read_block(index, buf)
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		self.inner.write_block(index, buf)
	}

	fn flush(&mut self) -> bool {
		self.inner.flush()
	}
}

#[test]
fn a_listing_does_not_report_an_unreadable_entry_as_gone() {
	// skipping a dangling or damaged entry is deliberate - a listing must not be stopped
	// by one bad record on a volume someone is trying to rescue. An I/O failure is not
	// that: the disk did not answer, so nothing is known about the entry, and omitting it
	// says the file is GONE. A backup or sync tool downstream reads that as a deletion to
	// propagate, and one transient read deletes the file at the other end.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"d").unwrap();
	fs.write_file(b"d/keep.txt", b"payload").unwrap();
	let dev = fs.into_device();
	let slot = active_slot(&dev);
	let inode_leaf = parse_superblock(&dev.blocks[slot * BLOCK_SIZE..(slot + 1) * BLOCK_SIZE]).unwrap().inode_root;

	let mut fs = LiberFs::mount(FailBlockDevice { inner: dev, fail: None }).unwrap();
	// warm the caches for everything ABOVE the entry under test, so the failure lands on
	// the child's inode read and not on the walk that reaches it.
	assert_eq!(fs.list().unwrap().len(), 1);
	fs.dev.fail = Some(inode_leaf);

	assert_eq!(fs.read_dir(b"d"), Err(FsError::Io), "an entry the disk would not answer for is not an entry that is gone");
}

// Does any recorded fault mention `needle`? The faults are operator-facing sentences,
// so a test asserts what they SAY rather than matching one exactly.
fn mentions(faults: &[Vec<u8>], needle: &[u8]) -> bool {
	faults.iter().any(|f| f.windows(needle.len()).any(|w| w == needle))
}

#[test]
fn fsck_reports_shapes_no_checksum_can_object_to() {
	// the scrub answers "did the medium give back what was written". The structural pass
	// answers "can what was written be true", and nothing asked that before: a leaf out
	// of key order, an extent map that overlaps itself, a spill chain shorter than the
	// count it serves, an inode no name reaches - every one of them lives in blocks whose
	// checksums are perfect.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"a.txt", b"one").unwrap();
	fs.mkdir(b"d").unwrap();
	fs.write_file(b"d/b.txt", b"two").unwrap();

	// the control, and the half that matters most: a healthy volume must be silent.
	let report = fs.fsck().unwrap();
	assert_eq!(report.structural_failures, 0, "a healthy volume has no structural faults: {:?}", report.faults);

	// an inode no name reaches: drop the directory entry and leave the inode alone.
	// It stays live to the free-map walk and invisible to a namespace check - the pair
	// that hides a whole subtree from whoever is trying to find out what is wrong.
	let victim = fs.lookup(b"a.txt").unwrap().unwrap();
	fs.mutate(|fs| {
		let (parent, name) = fs.resolve_parent(b"a.txt", false)?;
		fs.dir_remove(parent, name)
	})
	.unwrap();
	let report = fs.fsck().unwrap();
	assert!(report.structural_failures > 0, "an orphaned inode is a structural fault");
	assert!(mentions(&report.faults, b"no name"), "and it says so: {:?}", report.faults);
	assert!(fs.read_inode(victim).is_ok(), "the record really is still there");
}

#[test]
fn fsck_finds_a_record_routing_will_never_reach() {
	// the check no local rule can make. A leaf can be perfectly ordered, its hashes can
	// all match their names, and the separators above it can be perfectly ascending -
	// and the leaf can still hold records that routing sends down the OTHER child. They
	// are on the medium, they count against the directory's size, and no lookup will ever
	// find them.
	//
	// Two records with known hashes, a separator between them, and both put on the wrong
	// side of it.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"d").unwrap();
	fs.write_file(b"d/aaa", b"one").unwrap();
	fs.write_file(b"d/zzz", b"two").unwrap();
	let a_num = fs.lookup(b"d/aaa").unwrap().unwrap();
	let z_num = fs.lookup(b"d/zzz").unwrap().unwrap();
	assert_eq!(fs.fsck().unwrap().structural_failures, 0, "the real tree is sound");
	let mut dev = fs.into_device();

	let (ha, hz) = (name_hash(b"aaa"), name_hash(b"zzz"));
	let (lo_name, lo_hash, lo_child, hi_name, hi_hash, hi_child) = if ha < hz { (b"aaa".as_slice(), ha, a_num, b"zzz".as_slice(), hz, z_num) } else { (b"zzz".as_slice(), hz, z_num, b"aaa".as_slice(), ha, a_num) };
	// the separator is the higher hash, so child 0 is routed (-inf, hi) and child 1
	// [hi, +inf). Each record then goes into the leaf that cannot be reached for it.
	let (leaf0, leaf1, internal) = (30usize, 31usize, 32usize);
	let mut buf = vec![0u8; BLOCK_SIZE];
	dir_leaf_write(&mut buf, &[DirRec { hash: hi_hash, name: hi_name.to_vec(), child: hi_child }]);
	dev.blocks[leaf0 * BLOCK_SIZE..(leaf0 + 1) * BLOCK_SIZE].copy_from_slice(&buf);
	let crc0 = crc32c(&buf);
	dir_leaf_write(&mut buf, &[DirRec { hash: lo_hash, name: lo_name.to_vec(), child: lo_child }]);
	dev.blocks[leaf1 * BLOCK_SIZE..(leaf1 + 1) * BLOCK_SIZE].copy_from_slice(&buf);
	let crc1 = crc32c(&buf);

	let at = internal * BLOCK_SIZE;
	dev.blocks[at..at + BLOCK_SIZE].fill(0);
	node_set_header(&mut dev.blocks[at..at + NODE_HDR], NODE_INTERNAL, 1);
	set_sep(&mut dev.blocks[at..at + BLOCK_SIZE], 0, hi_hash);
	for (i, (blk, crc)) in [(leaf0, crc0), (leaf1, crc1)].iter().enumerate() {
		let off = at + INTERNAL_CHILD_BASE + i * CHILD_SIZE;
		dev.blocks[off..off + 8].copy_from_slice(&(*blk as u64).to_le_bytes());
		dev.blocks[off + 8..off + 12].copy_from_slice(&crc.to_le_bytes());
	}
	let icrc = crc32c(&dev.blocks[at..at + BLOCK_SIZE]);
	forge_inode_slot(&mut dev, |slot| {
		slot[INO_MAP_OFF..INO_MAP_OFF + 8].copy_from_slice(&(internal as u64).to_le_bytes());
		slot[INO_MAP_CRC_OFF..INO_MAP_CRC_OFF + 4].copy_from_slice(&icrc.to_le_bytes());
		slot[INO_SIZE_OFF..INO_SIZE_OFF + 8].copy_from_slice(&2u64.to_le_bytes());
	});

	let mut fs = LiberFs::mount(dev).unwrap();
	// The MOUNT refuses to write to it, before `fsck` is asked anything.
	//
	// This assertion is the second half of a fix that was recorded as whole when it was not. The
	// routing interval went into `mark_inode_tree` and not into `mark_dir_tree`, and the directory
	// tree is where the fault is dangerous: a record no lookup can reach means a create of that
	// same name writes a SECOND one down the side routing does reach, and the volume then holds one
	// name twice in one directory with nothing to say which is real. The inode-tree version had a
	// test; this one did not, and the milestone was ticked anyway.
	assert!(fs.is_read_only(), "a directory holding a record routing cannot reach must not be written to");
	let report = fs.fsck().unwrap();
	assert!(mentions(&report.faults, b"routing will never reach"), "a misrouted record must be named: {:?}", report.faults);
	// and the ordering and hash checks stay quiet, so this is the range check speaking.
	assert!(!mentions(&report.faults, b"ascending"), "the leaves are ordered: {:?}", report.faults);
	assert!(!mentions(&report.faults, b"stored hash"), "the hashes match their names: {:?}", report.faults);
}

#[test]
fn fsck_checks_directory_trees_as_structures() {
	// the inode tree was checked strictly and directory trees were reached mostly through
	// `dir_leaf_parse`, which stops at a record it cannot complete and returns what it
	// read. A truncated leaf therefore produced a directory that lists part of itself and
	// a `lookup` that cannot find an entry which is on the medium - with `fsck` reporting
	// no structural cause for any of it.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"d").unwrap();
	for n in [b"a.txt".as_slice(), b"b.txt".as_slice(), b"c.txt".as_slice()] {
		let mut path = b"d/".to_vec();
		path.extend_from_slice(n);
		fs.write_file(&path, b"payload").unwrap();
	}
	let dir = fs.lookup(b"d").unwrap().unwrap();
	let root = fs.read_inode(dir).unwrap().dir_root;
	assert_eq!(fs.fsck().unwrap().structural_failures, 0, "a healthy volume has no structural faults");
	let good = fs.into_device();

	// a leaf claiming far more records than the block can hold: `dir_leaf_parse` walks
	// until a record would run past the end and then stops, returning fewer than the
	// header promised. That is the truncated-leaf shape, and the count is what names it.
	//
	// It has to be FAR more. Claiming a few extra just makes the parser read on into the
	// zero padding and hand back that many zero-length records - caught too, but by the
	// hash and ordering checks rather than by the count.
	let mut dev = good.clone();
	{
		let at = root as usize * BLOCK_SIZE;
		dev.blocks[at + 2..at + 4].copy_from_slice(&400u16.to_le_bytes());
	}
	let crc = crc32c(&dev.blocks[root as usize * BLOCK_SIZE..(root as usize + 1) * BLOCK_SIZE]);
	forge_inode_slot(&mut dev, |slot| {
		slot[INO_MAP_CRC_OFF..INO_MAP_CRC_OFF + 4].copy_from_slice(&crc.to_le_bytes());
	});
	let mut fs = LiberFs::mount(dev).unwrap();
	let report = fs.fsck().unwrap();
	assert!(mentions(&report.faults, b"of the"), "the short leaf must be named: {:?}", report.faults);

	// and a directory whose cached size disagrees with its own tree.
	let mut dev = good.clone();
	forge_inode_slot(&mut dev, |slot| {
		slot[INO_SIZE_OFF..INO_SIZE_OFF + 8].copy_from_slice(&99u64.to_le_bytes());
	});
	let mut fs = LiberFs::mount(dev).unwrap();
	let report = fs.fsck().unwrap();
	assert!(mentions(&report.faults, b"the tree holds"), "the size mismatch must be named: {:?}", report.faults);
}

#[test]
fn fsck_reports_an_extent_map_that_overlaps_itself() {
	// two runs of one file covering the same logical block. Each is individually
	// possible - in the pool, right shape - and together they are not.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f.bin", &noise(BLOCK_SIZE)).unwrap();
	let (b0, b1, cb) = (10u64, 11u64, 12u64);
	for b in [b0, b1, cb] {
		assert!(!fs.is_alloc(b), "block {b} must be free for this forgery to mean anything");
	}
	let mut dev = fs.into_device();

	let mut cbuf = vec![0u8; BLOCK_SIZE];
	for (k, b) in [b0, b1].iter().enumerate() {
		let at = *b as usize * BLOCK_SIZE;
		cbuf[k * 4..k * 4 + 4].copy_from_slice(&crc32c(&dev.blocks[at..at + BLOCK_SIZE]).to_le_bytes());
	}
	let cbuf_crc = crc32c(&cbuf);
	dev.blocks[cb as usize * BLOCK_SIZE..(cb as usize + 1) * BLOCK_SIZE].copy_from_slice(&cbuf);
	forge_inode_slot(&mut dev, |slot| {
		slot[INO_SIZE_OFF..INO_SIZE_OFF + 8].copy_from_slice(&(2 * BLOCK_SIZE as u64).to_le_bytes());
		slot[INO_EXTENT_COUNT_OFF..INO_EXTENT_COUNT_OFF + 4].copy_from_slice(&2u32.to_le_bytes());
		let a = Extent { logical: 0, physical: b0, length: 2, csum: cb, csum_crc: cbuf_crc, store_len: 2, clen: 0 };
		// starts inside the run before it: the same logical block twice.
		let b = Extent { logical: 1, physical: b1, length: 1, csum: cb, csum_crc: cbuf_crc, store_len: 1, clen: 0 };
		a.write(&mut slot[EXTENT_OFF..EXTENT_OFF + EXTENT_SIZE]);
		b.write(&mut slot[EXTENT_OFF + EXTENT_SIZE..EXTENT_OFF + 2 * EXTENT_SIZE]);
	});

	let mut fs = LiberFs::mount(dev).unwrap();
	let report = fs.fsck().unwrap();
	assert!(mentions(&report.faults, b"overlap"), "the overlap must be named: {:?}", report.faults);
}

#[test]
fn a_label_is_the_utf8_it_claims_to_be() {
	// the record is specified as UTF-8 and only the CLAMP respected that: the input was
	// never checked to be text, and a NUL inside it truncates the label at the next
	// mount, because `label()` reads up to the first NUL. What was written and what was
	// read back could differ.
	let mk = |label: &[u8]| LiberFs::format_opts(MemDevice::new(NBLOCKS), NBLOCKS, FormatOpts { label: label.to_vec(), ..FormatOpts::default() });
	assert_eq!(mk(b"has\0nul").err(), Some(FsError::BadName), "an embedded NUL would silently shorten the label");
	assert_eq!(mk(&[0xFFu8, 0xFE]).err(), Some(FsError::BadName), "and bytes that are not text are not a label");

	// what survives the round trip is what was asked for.
	let fs = mk("archiv-zálohy".as_bytes()).unwrap();
	assert_eq!(fs.label(), "archiv-zálohy".as_bytes());
	let dev = fs.into_device();
	let fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.label(), "archiv-zálohy".as_bytes(), "and again after a remount");
}

#[test]
fn a_volume_this_machine_cannot_map_is_an_error_not_an_abort() {
	// under MAX_BLOCKS and still enormous: a superblock may legally claim a size whose
	// free maps do not fit in this machine, and a mount builds several of them. The
	// allocation used to be an infallible `vec!`, which aborts the process - so a
	// checksum-consistent number took StorageService down instead of returning an error.
	//
	// 2^39 blocks is half the format ceiling; its bitmap is 64 GiB, and the mount wants
	// two before it walks anything.
	//
	// The answer is NoMemory, not NoSpace: the medium is not full, this machine cannot hold the
	// map. They used to be the same error, and they drive opposite policies - one says delete
	// something, the other says the service is under memory pressure.
	let huge = MAX_BLOCKS / 2;
	assert_eq!(LiberFs::format_scratch(MemDevice::new(8), huge).err(), Some(FsError::NoMemory), "the format path reports rather than aborts");
	// and the helper itself, which is what every derived map goes through.
	assert_eq!(try_zeroed((huge / 8) as usize).err(), Some(FsError::NoMemory));
	// while an ordinary size still succeeds, so the guard is not simply refusing.
	assert!(try_zeroed(1024).is_ok());
}

#[test]
fn a_volume_larger_than_this_build_can_map_is_refused() {
	// the free maps are sized `(num_blocks as usize).div_ceil(8)` with an unchecked cast:
	// on a 32-bit target that truncates and produces a bitmap too small for the volume,
	// so the allocator hands out blocks it never tracked. On a 64-bit one it is an
	// enormous infallible allocation. Both are now a refusal against a documented bound.
	assert_eq!(LiberFs::format_scratch(MemDevice::new(8), MAX_BLOCKS + 1).err(), Some(FsError::Invalid));
	assert_eq!(LiberFs::format_scratch(MemDevice::new(8), u64::MAX).err(), Some(FsError::Invalid));
}

#[test]
fn a_whole_file_overwrite_keeps_the_owner() {
	// the partial-write and truncate paths edit the existing inode and so keep the tag;
	// the whole-file path builds a fresh one and carried `ctime` across but not the
	// owner. Overwriting a file's CONTENTS does not make it a different file.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f.txt", b"one").unwrap();
	let num = fs.lookup(b"f.txt").unwrap().unwrap();
	let tag = [7u8; OWNER_TAG_LEN];
	fs.mutate(|fs| {
		let mut inode = fs.read_inode(num)?;
		inode.owner_tag = tag;
		fs.write_inode(num, &mut inode)
	})
	.unwrap();

	fs.write_at(b"f.txt", 0, b"X").unwrap();
	assert_eq!(fs.read_inode(num).unwrap().owner_tag, tag, "a partial write keeps the owner");
	fs.truncate(b"f.txt", 1).unwrap();
	assert_eq!(fs.read_inode(num).unwrap().owner_tag, tag, "and so does a truncate");

	fs.write_file(b"f.txt", b"replaced entirely").unwrap();
	assert_eq!(fs.lookup(b"f.txt").unwrap().unwrap(), num, "an overwrite reuses the inode");
	assert_eq!(fs.read_inode(num).unwrap().owner_tag, tag, "so a whole-file overwrite must keep it too");
}

#[test]
fn a_superblock_may_not_contradict_itself() {
	// the fields were each checked alone and never against each other. `next_inode`
	// hands out numbers above everything in use, so one at or below an inode that
	// EXISTS means the next file created takes over that inode and every name pointing
	// at it - which is why this one degrades the mount rather than being tidied up.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"a.txt", b"one").unwrap();
	fs.write_file(b"b.txt", b"two").unwrap();
	let good = fs.into_device();
	assert!(!LiberFs::mount(good.clone()).unwrap().is_read_only(), "the untouched image must mount writable");

	// a counter pointing back at an inode that exists.
	let mut dev = good.clone();
	let slot = newest_super_slot(&dev) as usize;
	forge_superblock(&mut dev, slot, |sb| sb[SB_NEXT_INODE_OFF..SB_NEXT_INODE_OFF + 4].copy_from_slice(&1u32.to_le_bytes()));
	assert!(LiberFs::mount(dev).unwrap().is_read_only(), "a next-inode counter naming a live inode may not allocate");

	// a root of the namespace that is not a directory.
	let mut dev = good.clone();
	let slot = newest_super_slot(&dev) as usize;
	forge_superblock(&mut dev, slot, |sb| sb[SB_ROOT_INODE_OFF..SB_ROOT_INODE_OFF + 4].copy_from_slice(&1u32.to_le_bytes()));
	assert!(LiberFs::mount(dev).unwrap().is_read_only(), "a root that is not a directory may not be written through");

	// and the ones settled without reading anything: the slot is simply not ours.
	for (what, off, len) in [("a counter at or below the root inode", SB_NEXT_INODE_OFF, 4), ("a root outside the pool", SB_INODE_ROOT_OFF, 8)] {
		let mut dev = good.clone();
		let slot = newest_super_slot(&dev) as usize;
		forge_superblock(&mut dev, slot, |sb| sb[off..off + len].fill(if len == 4 { 0 } else { 0xFF }));
		let mut fs = LiberFs::mount(dev).expect("the other slot is intact, so the volume still mounts");
		// the forged slot was refused, so this is the previous generation.
		assert!(fs.list().unwrap().len() < 2, "{what} must not be the generation that was mounted");
	}
}

#[test]
fn a_damaged_empty_directory_is_refused_not_emptied() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"victim").unwrap();
	fs.write_file(b"victim/f.txt", b"entry").unwrap();
	fs.write_file(b"src.txt", b"mover").unwrap();

	// forge the damaged-but-empty form through the crate's own machinery: size 0 with a
	// live directory tree.
	//
	// This test used to assert the replace SUCCEEDED and reclaimed the tree, which was
	// the right shape of answer for the leak it was written about and the wrong answer
	// overall: freeing the tree leaves the child inodes in the global inode table, live
	// to the free-map walk and reachable from no name at all - invisible to an `fsck`
	// that can only check what it can reach. Refusing costs nothing and strands nothing,
	// and the repair route stays open, because listing a directory walks its TREE rather
	// than trusting `size`: the operator can still see the children and remove them, and
	// then this same call succeeds.
	let victim = fs.resolve(b"victim").unwrap();
	let dir_root = fs.read_inode(victim).unwrap().dir_root;
	assert!(dir_root != 0, "the directory must hold a tree for any of this to mean anything");
	fs.mutate(|fs| {
		let mut inode = fs.read_inode(victim)?;
		inode.size = 0;
		fs.write_inode(victim, &mut inode)
	})
	.unwrap();

	assert_eq!(fs.rename(b"src.txt", b"victim"), Err(FsError::NotEmpty), "a directory that claims to be empty and is not may not be replaced");
	assert_eq!(fs.remove(b"victim"), Err(FsError::NotEmpty), "nor deleted");
	// nothing moved and nothing was freed: the child is still there and still readable.
	assert!(test_bit(&fs.free, dir_root), "the tree node is still a live block");
	assert_eq!(fs.read_file(b"victim/f.txt").unwrap(), b"entry");
	assert_eq!(fs.read_file(b"src.txt").unwrap(), b"mover");

	// and the repair route: remove the child the listing still shows, then the replace
	// goes through - and the tree it dropped is reclaimed rather than leaked, which is
	// what this test was originally written to pin.
	assert_eq!(fs.read_dir(b"victim").unwrap().len(), 1, "the listing walks the tree, so the child is visible");
	fs.remove(b"victim/f.txt").unwrap();
	fs.rename(b"src.txt", b"victim").unwrap();
	assert_eq!(fs.read_file(b"victim").unwrap(), b"mover");
	// one more commit ages the dropped blocks out (dead -> dead_prev -> reclaimed).
	fs.write_file(b"tick.txt", b"1").unwrap();
	assert!(!test_bit(&fs.free, dir_root), "the replaced directory's tree node is reclaimed, not leaked");
}

// The second-pass findings.

#[test]
fn a_forged_raw_length_does_not_read_past_the_pool() {
	// a 64-block pool on a 128-block device, like the earlier gate test - but this
	// forgery keeps every ADDRESS FIELD inside the pool and lies with the LENGTH:
	// a raw extent claiming 2 logical blocks over a 1-block stored span, its
	// physical start at the pool's last block, walks its second read past the pool.
	let mut fs = LiberFs::format_scratch(MemDevice::new(128), NBLOCKS).unwrap();
	fs.write_file(b"f.bin", &noise(BLOCK_SIZE)).unwrap();
	let mut dev = fs.into_device();

	// the foreign bytes past the pool, and an in-pool checksum block vouching for
	// both blocks of the claimed span - every CRC matches, only the gate can refuse.
	let foreign = vec![0xA7u8; BLOCK_SIZE];
	dev.blocks[64 * BLOCK_SIZE..65 * BLOCK_SIZE].copy_from_slice(&foreign);
	let zeros = vec![0u8; BLOCK_SIZE];
	let mut cbuf = vec![0u8; BLOCK_SIZE];
	cbuf[0..4].copy_from_slice(&crc32c(&zeros).to_le_bytes());
	cbuf[4..8].copy_from_slice(&crc32c(&foreign).to_le_bytes());
	let cbuf_crc = crc32c(&cbuf);
	dev.blocks[50 * BLOCK_SIZE..51 * BLOCK_SIZE].copy_from_slice(&cbuf);

	forge_inode_slot(&mut dev, |slot| {
		slot[INO_SIZE_OFF..INO_SIZE_OFF + 8].copy_from_slice(&(2 * BLOCK_SIZE as u64).to_le_bytes());
		slot[INO_EXTENT_COUNT_OFF..INO_EXTENT_COUNT_OFF + 4].copy_from_slice(&1u32.to_le_bytes());
		let ext = Extent { logical: 0, physical: 63, length: 2, csum: 50, csum_crc: cbuf_crc, store_len: 1, clen: 0 };
		ext.write(&mut slot[EXTENT_OFF..EXTENT_OFF + EXTENT_SIZE]);
	});
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"f.bin"), Err(FsError::Corrupt), "a length past the stored span is damage, not another partition's data");
}

#[test]
fn a_raw_extent_may_not_read_more_blocks_than_it_stored() {
	// the dangerous twin of the test above: the same disagreement between `length` and
	// `store_len`, but with the WHOLE forged span inside the pool, where the address gate
	// has nothing to refuse. The read path serves two blocks; every marking loop walks
	// `0..store_len` and marks one. The second block is reachable by this file and free
	// according to the allocator - so the next write is handed a block someone is already
	// reading, and overwrites it with no error anywhere.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f.bin", &noise(BLOCK_SIZE)).unwrap();
	// three blocks the filesystem itself says are free, in the DATA half of the pool.
	// Picking by eye put an earlier draft of this test on top of the inode table at the
	// metadata end: forging the inode then overwrote the payload, the CRC no longer
	// matched, and the mount refused for a reason that had nothing to do with the shape
	// of the extent. The test passed without the fix it was meant to prove.
	let (b0, b1, cb) = (10u64, 11u64, 12u64);
	for b in [b0, b1, cb] {
		assert!(!fs.is_alloc(b), "block {b} must be free for this forgery to mean anything");
	}
	let mut dev = fs.into_device();

	// two adjacent in-pool blocks and a checksum block vouching for both: every CRC in
	// this image matches, so nothing but the shape of the extent itself can object.
	let first = vec![0x11u8; BLOCK_SIZE];
	let second = vec![0x22u8; BLOCK_SIZE];
	dev.blocks[b0 as usize * BLOCK_SIZE..(b0 as usize + 1) * BLOCK_SIZE].copy_from_slice(&first);
	dev.blocks[b1 as usize * BLOCK_SIZE..(b1 as usize + 1) * BLOCK_SIZE].copy_from_slice(&second);
	let mut cbuf = vec![0u8; BLOCK_SIZE];
	cbuf[0..4].copy_from_slice(&crc32c(&first).to_le_bytes());
	cbuf[4..8].copy_from_slice(&crc32c(&second).to_le_bytes());
	let cbuf_crc = crc32c(&cbuf);
	dev.blocks[cb as usize * BLOCK_SIZE..(cb as usize + 1) * BLOCK_SIZE].copy_from_slice(&cbuf);

	forge_inode_slot(&mut dev, |slot| {
		slot[INO_SIZE_OFF..INO_SIZE_OFF + 8].copy_from_slice(&(2 * BLOCK_SIZE as u64).to_le_bytes());
		slot[INO_EXTENT_COUNT_OFF..INO_EXTENT_COUNT_OFF + 4].copy_from_slice(&1u32.to_le_bytes());
		let ext = Extent { logical: 0, physical: b0, length: 2, csum: cb, csum_crc: cbuf_crc, store_len: 1, clen: 0 };
		ext.write(&mut slot[EXTENT_OFF..EXTENT_OFF + EXTENT_SIZE]);
	});
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"f.bin"), Err(FsError::Corrupt), "a raw run that serves more blocks than it stored is impossible, wherever it points");
	// and the volume is read-only, which is the half that matters: the free map was
	// derived from an extent that cannot be true, so nothing may be allocated from it.
	// Without that, block `b1` is reachable through this file and free according to the
	// allocator, and the next write is handed a block someone else is already reading.
	assert!(fs.is_read_only(), "a free map derived from an impossible extent may not be allocated from");
	assert_eq!(fs.write_file(b"other.bin", b"x"), Err(FsError::ReadOnly), "and the refusal has to hold at the write");
}

#[test]
fn a_snapshot_record_off_the_medium_is_checked_like_one_being_written() {
	// `create_snapshot` demands a non-empty, unique, UTF-8 name and pins a root it just
	// computed. The loader demanded nothing: a table read off the medium could name a
	// snapshot with no name at all, or a root outside the pool, or a generation that has
	// not happened yet - and the mount stayed writable on the strength of it.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"keep.txt", b"payload").unwrap();
	fs.create_snapshot(b"s1").unwrap();
	let good = fs.into_device();

	// the unforged image is the control: whatever the forgeries below prove, they have
	// to prove it against a volume that mounts writable.
	assert!(!LiberFs::mount(good.clone()).unwrap().is_read_only(), "the untouched image must mount writable");

	let cases: [(&str, fn(&mut [u8])); 3] = [
		("a snapshot with no name", |rec| rec[..SNAP_NAME_MAX].fill(0)),
		("a root outside the pool", |rec| rec[SNAP_ROOT_OFF..SNAP_ROOT_OFF + 8].copy_from_slice(&NBLOCKS.to_le_bytes())),
		("a generation that has not happened", |rec| rec[SNAP_GEN_OFF..SNAP_GEN_OFF + 8].copy_from_slice(&u64::MAX.to_le_bytes())),
	];
	for (what, forge) in cases {
		let mut dev = good.clone();
		forge_snapshot_record(&mut dev, forge);
		let fs = LiberFs::mount(dev).unwrap();
		assert!(fs.is_read_only(), "{what} may not leave the volume writable");
	}
}

#[test]
fn a_deleted_snapshot_stays_reserved_while_the_older_superblock_names_it() {
	// deleting a snapshot commits a live generation that no longer holds it - but the
	// OTHER superblock slot still describes a generation in which it was live, and that
	// slot stays mountable until the next commit overwrites it. If the delete frees the
	// snapshot's blocks straight away, a crash in that window leaves a superblock which
	// `mount_snapshot` will offer as a complete image of a generation whose data is gone.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"keep.txt", b"payload").unwrap();
	fs.create_snapshot(b"s1").unwrap();
	let snap_root = fs.snapshots[0].inode_root;

	// a commit in between, so the live tree copies away from the snapshot's root and the
	// two are different blocks - otherwise the assertion below would hold for the wrong
	// reason, the live generation happening to sit on the same block.
	fs.write_file(b"other.txt", b"more").unwrap();
	assert_ne!(snap_root, fs.inode_root, "the snapshot's root must have been copied away from");

	fs.delete_snapshot(b"s1").unwrap();
	assert!(fs.list_snapshots().unwrap().is_empty(), "the live generation has no snapshots now");
	assert!(fs.is_alloc(snap_root), "the previous generation's snapshot table still names this tree, and its superblock is still mountable");

	// and the older generation genuinely still reads, which is what the reservation is for.
	let dev = fs.into_device();
	let mut prev = LiberFs::mount_snapshot(dev).expect("the volume mounts").expect("the previous generation must still be there");
	assert_eq!(prev.list_snapshots().unwrap().len(), 1, "it is the generation in which s1 was live");
	assert_eq!(prev.read_file_from_snapshot(b"s1", b"keep.txt").unwrap(), b"payload");
}

#[test]
fn a_node_the_parent_would_reject_may_not_feed_the_free_map() {
	// the free-map walk reads raw, without checksums, and that tolerance is deliberate:
	// damage must not cost a volume its data. What it leaves is the block that reads
	// CLEANLY and is wrong. Nothing fails, so the mount stays writable, and the map it
	// derived can omit a live block - which the next allocation then hands out.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"a.txt", b"one").unwrap();
	fs.mkdir(b"d").unwrap();
	let mut dev = fs.into_device();

	// one byte of an mtime in the inode tree's root leaf. The block still reads, still
	// parses, still has its node type and record count; only the CRC the superblock
	// recorded for it no longer matches.
	let slot = active_slot(&dev);
	let sb = parse_superblock(&dev.blocks[slot * BLOCK_SIZE..(slot + 1) * BLOCK_SIZE]).unwrap();
	let leaf = sb.inode_root as usize * BLOCK_SIZE;
	dev.blocks[leaf + NODE_HDR + 8 + INO_MTIME_OFF] ^= 0x01;

	let mut fs = LiberFs::mount(dev).unwrap();
	assert!(fs.is_read_only(), "a free map derived from a node its parent vouched differently for may not be allocated from");
	assert_eq!(fs.write_file(b"c.txt", b"x"), Err(FsError::ReadOnly), "and the refusal has to hold at the write");
}

#[test]
fn two_owners_of_one_block_degrade_the_mount() {
	// marking a bitmap is idempotent - setting a bit twice is setting a bit - so an
	// image where two extents point at one data block derives a free map that looks
	// perfect. Nothing is wrong until one owner is deleted: the block joins `dead`, a
	// commit hands it out, and the other owner is still reading it.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f.bin", &noise(2 * BLOCK_SIZE)).unwrap();
	let (b0, cb0, cb1) = (10u64, 11u64, 12u64);
	for b in [b0, cb0, cb1] {
		assert!(!fs.is_alloc(b), "block {b} must be free for this forgery to mean anything");
	}
	let mut dev = fs.into_device();

	// two one-block runs at consecutive logical offsets, both addressing b0.
	let mut cbuf = vec![0u8; BLOCK_SIZE];
	cbuf[0..4].copy_from_slice(&crc32c(&dev.blocks[b0 as usize * BLOCK_SIZE..(b0 as usize + 1) * BLOCK_SIZE]).to_le_bytes());
	let cbuf_crc = crc32c(&cbuf);
	for cb in [cb0, cb1] {
		dev.blocks[cb as usize * BLOCK_SIZE..(cb as usize + 1) * BLOCK_SIZE].copy_from_slice(&cbuf);
	}
	forge_inode_slot(&mut dev, |slot| {
		slot[INO_SIZE_OFF..INO_SIZE_OFF + 8].copy_from_slice(&(2 * BLOCK_SIZE as u64).to_le_bytes());
		slot[INO_EXTENT_COUNT_OFF..INO_EXTENT_COUNT_OFF + 4].copy_from_slice(&2u32.to_le_bytes());
		let a = Extent { logical: 0, physical: b0, length: 1, csum: cb0, csum_crc: cbuf_crc, store_len: 1, clen: 0 };
		let b = Extent { logical: 1, physical: b0, length: 1, csum: cb1, csum_crc: cbuf_crc, store_len: 1, clen: 0 };
		a.write(&mut slot[EXTENT_OFF..EXTENT_OFF + EXTENT_SIZE]);
		b.write(&mut slot[EXTENT_OFF + EXTENT_SIZE..EXTENT_OFF + 2 * EXTENT_SIZE]);
	});

	let mut fs = LiberFs::mount(dev).unwrap();
	assert!(fs.is_read_only(), "one block with two owners in the live generation is corruption");
	assert_eq!(fs.write_file(b"other.bin", b"x"), Err(FsError::ReadOnly), "and no allocation may proceed from that map");
}

#[test]
fn snapshots_may_share_every_block_they_like() {
	// the other side of the same rule, and the reason it is scoped to one generation:
	// copy-on-write means a snapshot and the live tree share almost everything, and two
	// snapshots share nearly all of each other. None of that is two owners, and a mount
	// that called it corruption would refuse every volume that has ever been snapshotted.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"keep.txt", b"payload").unwrap();
	fs.create_snapshot(b"one").unwrap();
	fs.write_file(b"second.txt", b"more").unwrap();
	fs.create_snapshot(b"two").unwrap();
	let dev = fs.into_device();

	let mut fs = LiberFs::mount(dev).unwrap();
	assert!(!fs.is_read_only(), "sharing across generations is the design, not damage");
	assert_eq!(fs.read_file(b"keep.txt").unwrap(), b"payload");
	fs.write_file(b"third.txt", b"still writable").unwrap();
}

#[test]
fn a_compressed_extent_may_not_claim_more_bytes_than_it_stored() {
	// the same class from the other side. A compressed run must store FEWER blocks than
	// it serves and its stream must fit in the blocks it says it holds; neither was
	// checked. `clen` past `store_len * BLOCK_SIZE` sends the decompressor reading off
	// the end of the buffer it was given, and `store_len >= length` is not a compressed
	// run at all.
	let pool = 128u64;
	let fs = LiberFs::format_scratch(MemDevice::new(pool), pool).unwrap();

	let ok = Extent { logical: 0, physical: 60, length: 4, csum: 50, csum_crc: 0, store_len: 2, clen: 2 * BLOCK_SIZE as u32 };
	assert!(fs.check_extent(&ok).is_ok(), "a stream exactly filling its stored blocks is legal");

	let overrun = Extent { clen: 2 * BLOCK_SIZE as u32 + 1, ..ok };
	assert_eq!(fs.check_extent(&overrun), Err(FsError::Corrupt), "a stream longer than the blocks holding it");

	// and NOT a demand that a compressed run store fewer blocks than it serves: that
	// holds when the run is created and stops holding the moment the file is truncated,
	// since shrinking `length` leaves the stored stream whole. Asserting it here is how
	// the first version of this check was found to make truncated compressed files
	// unreadable.
	let not_smaller = Extent { store_len: 4, ..ok };
	assert!(fs.check_extent(&not_smaller).is_ok(), "a truncated compressed run stores more than it serves, and is legal");

	let nothing_stored = Extent { store_len: 0, ..ok };
	assert_eq!(fs.check_extent(&nothing_stored), Err(FsError::Corrupt), "a compressed run storing nothing");

	let empty = Extent { logical: 0, physical: 60, length: 0, csum: 50, csum_crc: 0, store_len: 0, clen: 0 };
	assert_eq!(fs.check_extent(&empty), Err(FsError::Corrupt), "an extent covering no logical blocks");
}

// Panics past a read budget. A walk that is merely slow is indistinguishable from one
// that is stuck when you are watching a test, so the budget turns "too much work" into
// an ordinary failure with a number in it.
struct ReadBudgetDevice {
	inner: MemDevice,
	left: usize,
}

impl BlockDevice for ReadBudgetDevice {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		if self.left == 0 {
			panic!("the walk read far past the size of the pool - it is exponential, not merely thorough");
		}
		self.left -= 1;
		self.inner.read_block(index, buf)
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		self.inner.write_block(index, buf)
	}

	fn flush(&mut self) -> bool {
		self.inner.flush()
	}
}

#[test]
fn a_checksum_consistent_fan_cannot_make_a_listing_exponential() {
	// the same shape as the fsck test below, aimed at the path every ordinary directory
	// operation takes. `collect_dir_entries` had the depth budget and no visited set, so
	// it bounded the stack and not the work: nine levels of twelve-way fan at one node
	// each is twelve to the eighth visits over a tree of ten blocks.
	//
	// Read-only is no defence here - `list` and `read_dir` are reads.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"d").unwrap();
	fs.write_file(b"d/keep.txt", b"payload").unwrap();
	let mut dev = fs.into_device();

	const LEVELS: usize = 9;
	const FAN: usize = 12;
	let top = 20usize;
	let leaf = top + LEVELS;
	{
		let at = leaf * BLOCK_SIZE;
		dev.blocks[at..at + BLOCK_SIZE].fill(0);
		node_set_header(&mut dev.blocks[at..at + NODE_HDR], NODE_LEAF, 0);
	}
	let mut child = leaf as u64;
	let mut child_crc = crc32c(&dev.blocks[leaf * BLOCK_SIZE..(leaf + 1) * BLOCK_SIZE]);
	for level in (0..LEVELS).rev() {
		let blk = top + level;
		let at = blk * BLOCK_SIZE;
		dev.blocks[at..at + BLOCK_SIZE].fill(0);
		node_set_header(&mut dev.blocks[at..at + NODE_HDR], NODE_INTERNAL, FAN - 1);
		for i in 0..FAN {
			let off = at + INTERNAL_CHILD_BASE + i * CHILD_SIZE;
			dev.blocks[off..off + 8].copy_from_slice(&child.to_le_bytes());
			dev.blocks[off + 8..off + 12].copy_from_slice(&child_crc.to_le_bytes());
		}
		child = blk as u64;
		child_crc = crc32c(&dev.blocks[at..at + BLOCK_SIZE]);
	}
	// hang it off the directory inode, whose slot `forge_inode_slot` reaches.
	forge_inode_slot(&mut dev, |slot| {
		slot[INO_TYPE_OFF] = TYPE_DIR;
		slot[INO_MAP_OFF..INO_MAP_OFF + 8].copy_from_slice(&child.to_le_bytes());
		slot[INO_MAP_CRC_OFF..INO_MAP_CRC_OFF + 4].copy_from_slice(&child_crc.to_le_bytes());
	});

	let dev = ReadBudgetDevice { inner: dev, left: 20_000 };
	let mut fs = LiberFs::mount(dev).expect("the forged tree is checksum-consistent, so it mounts");
	// whatever it answers, it answers - a repeated node inside one directory tree is
	// corruption, and the walk is bounded by the pool either way.
	let _ = fs.list();
	let _ = fs.read_dir(b"d");
}

#[test]
fn a_checksum_consistent_fan_cannot_make_fsck_exponential() {
	// the raw marking walk carries a visited bitmap; `check_inode_tree` carried only a
	// depth limit, which protects the stack and not the clock. This shape needs no cycle
	// and no checksum failure: each level is one node whose children ALL point at the
	// single node of the next level, built bottom-up so every CRC agrees. The walk then
	// visits fanout^depth nodes on a tree with ten blocks in it.
	//
	// It has to hang off a SNAPSHOT: `fsck` walks the live volume by its namespace and
	// reaches `check_inode_tree` only for the pinned generations. A first version of this
	// test forged the live inode root instead, so the walk under test was never called
	// and it passed with the fix removed.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"a.txt", b"one").unwrap();
	fs.create_snapshot(b"s").unwrap();
	let mut dev = fs.into_device();

	const LEVELS: usize = 9;
	const FAN: usize = 12;
	let top = 20usize;
	// the bottom: an empty leaf, which is a legitimate node and ends the descent.
	let leaf = top + LEVELS;
	{
		let at = leaf * BLOCK_SIZE;
		dev.blocks[at..at + BLOCK_SIZE].fill(0);
		node_set_header(&mut dev.blocks[at..at + NODE_HDR], NODE_LEAF, 0);
	}
	let mut child = leaf as u64;
	let mut child_crc = crc32c(&dev.blocks[leaf * BLOCK_SIZE..(leaf + 1) * BLOCK_SIZE]);
	for level in (0..LEVELS).rev() {
		let blk = top + level;
		let at = blk * BLOCK_SIZE;
		dev.blocks[at..at + BLOCK_SIZE].fill(0);
		node_set_header(&mut dev.blocks[at..at + NODE_HDR], NODE_INTERNAL, FAN - 1);
		for i in 0..FAN {
			let off = at + INTERNAL_CHILD_BASE + i * CHILD_SIZE;
			dev.blocks[off..off + 8].copy_from_slice(&child.to_le_bytes());
			dev.blocks[off + 8..off + 12].copy_from_slice(&child_crc.to_le_bytes());
		}
		child = blk as u64;
		child_crc = crc32c(&dev.blocks[at..at + BLOCK_SIZE]);
	}
	// point the snapshot at it.
	forge_snapshot_record(&mut dev, |rec| {
		rec[SNAP_ROOT_OFF..SNAP_ROOT_OFF + 8].copy_from_slice(&child.to_le_bytes());
		rec[SNAP_ROOT_CRC_OFF..SNAP_ROOT_CRC_OFF + 4].copy_from_slice(&child_crc.to_le_bytes());
	});

	// generous: the pool is 64 blocks, and every walk in a mount plus an fsck should
	// stay within a few times that. 12^8 is about 400 million.
	let dev = ReadBudgetDevice { inner: dev, left: 20_000 };
	let mut fs = LiberFs::mount(dev).expect("the forged tree is checksum-consistent, so it mounts");
	fs.fsck().unwrap();
}

#[test]
fn a_self_fanning_internal_node_cannot_stall_the_mark_walk() {
	// a hostile directory tree: one internal node whose maximal fan of child links
	// all point back at the node itself. The mark walks read it raw (no CRC), so the
	// shape reaches them; marking at push queues the block once - the walk visits it,
	// requeues nothing, and the mount completes with the rest of the volume intact.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"d").unwrap();
	fs.write_file(b"keep.txt", b"payload").unwrap();
	let mut dev = fs.into_device();

	let node = 40u64;
	let start = node as usize * BLOCK_SIZE;
	{
		let block = &mut dev.blocks[start..start + BLOCK_SIZE];
		block[0] = NODE_INTERNAL;
		block[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
		for i in 0..INTERNAL_MAX {
			let off = INTERNAL_CHILD_BASE + i * CHILD_SIZE;
			block[off..off + 8].copy_from_slice(&node.to_le_bytes());
		}
	}
	// point the directory's tree root at the fanning node (raw walks follow it; the
	// checksummed live paths refuse it as the damage it is).
	forge_inode_slot(&mut dev, |slot| {
		slot[INO_MAP_OFF..INO_MAP_OFF + 8].copy_from_slice(&node.to_le_bytes());
	});
	let mut fs = LiberFs::mount(dev).expect("the mark walk must terminate");
	assert_eq!(fs.read_file(b"keep.txt").unwrap(), b"payload");
	assert!(test_bit(&fs.free, node), "the walked node is reserved like any referenced block");
}

// The third-pass follow-up nits.

#[test]
fn an_unknown_inode_type_is_inert_and_removable() {
	// a type byte the writer never emits (hostile authoring): the record must land
	// harmless - refused by reads and writes, shown inert by listings, and clearable
	// by the operator's repair verb.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"odd", b"payload").unwrap();
	let mut dev = fs.into_device();
	forge_inode_slot(&mut dev, |slot| {
		slot[INO_TYPE_OFF] = 7;
	});
	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"odd"), Err(FsError::IsDir), "a read refuses the unknown type");
	assert_eq!(fs.write_file(b"odd", b"x"), Err(FsError::IsDir), "an overwrite refuses it too");
	assert_eq!(fs.list().unwrap().len(), 1, "the record lists inert");

	// and its blocks are RESERVED, which is the half that was missing. The usual way an
	// unknown type appears is a flipped byte in the type field of a real file, and its
	// data blocks were reserved by nobody while the volume stayed writable - so the
	// allocator could hand out blocks that were still recoverable. Marking it as the file
	// it parses as costs at worst some held space until the record is removed.
	let odd = fs.lookup(b"odd").unwrap().unwrap();
	let data = fs.read_inode(odd).unwrap().extents[0].physical;
	assert!(fs.is_alloc(data), "the blocks behind an unknown type may not be free for the allocator");
	// the structural pass names it rather than passing over it in silence.
	let report = fs.fsck().unwrap();
	assert!(mentions(&report.faults, b"does not define"), "fsck names the unknown type: {:?}", report.faults);

	// and the repair verb still works, which read-only degradation would have cost.
	fs.remove(b"odd").unwrap();
	assert_eq!(fs.list().unwrap().len(), 0, "the repair verb clears it");
}

#[test]
fn overwriting_a_cached_entry_evicts_nothing() {
	// a full dentry cache: re-putting a key it already holds must not evict a
	// different entry (the insert replaces in place, like the inode cache).
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	for i in 0..DCACHE_MAX as u32 {
		fs.dcache_put(0, format!("name{i:04}").as_bytes(), i);
	}
	assert_eq!(fs.dcache.len(), DCACHE_MAX);
	fs.dcache_put(0, b"name0000", 999);
	assert_eq!(fs.dcache.len(), DCACHE_MAX, "the overwrite evicted nothing");
	assert_eq!(fs.dcache.get(&(0, b"name0000".to_vec())), Some(&999), "the overwrite landed");
	assert!(fs.dcache.contains_key(&(0, format!("name{:04}", DCACHE_MAX - 1).as_bytes().to_vec())), "the largest key survived the overwrite");
}

// P02M0116: the third audit's follow-ups, and the three regressions P02M0114 opened.

#[test]
fn a_directory_that_splits_lists_in_key_order() {
	// `collect_dir_entries` is documented as returning entries in key order, and P02M0114
	// replaced its recursion with a LIFO stack that pushed children 0..=count and popped
	// the last one FIRST. Records inside a leaf stayed sorted, so the reversal only shows
	// once a directory is big enough to have an internal node - and every directory test
	// there was fit in a single leaf, where LIFO and FIFO are the same thing.
	//
	// The tree routes by name hash, so "key order" is (hash, name) ascending, not
	// alphabetical: the assertion is against the same comparison the leaves are sorted by.
	let nblocks: u64 = 4_000;
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
	fs.mkdir(b"many").unwrap();
	// long names, so a leaf fills after a few dozen records and 400 of them force
	// several leaves under an internal node.
	let count = 400u32;
	for i in 0..count {
		fs.write_file(format!("many/entry-{i:04}-{}", "p".repeat(40)).as_bytes(), b"x").unwrap();
	}

	// the tree really did split - otherwise this test proves nothing at all.
	let dir = fs.lookup(b"many").unwrap().unwrap();
	let inode = fs.read_inode(dir).unwrap();
	let mut buf = vec![0u8; BLOCK_SIZE];
	fs.read_node(inode.dir_root, inode.dir_root_crc, &mut buf).unwrap();
	assert_eq!(node_type(&buf), NODE_INTERNAL, "400 long names must not fit in one leaf");

	let listed: Vec<Vec<u8>> = fs.read_dir(b"many").unwrap().into_iter().map(|(name, ..)| name).collect();
	assert_eq!(listed.len() as u32, count, "every entry is listed");
	let mut sorted = listed.clone();
	sorted.sort_by(|a, b| (name_hash(a), a).cmp(&(name_hash(b), b)));
	assert_eq!(listed, sorted, "a directory with an internal node lists in key order like any other");
}

#[test]
fn removing_an_unknown_type_inode_returns_its_blocks() {
	// P02M0114 taught the mark walk to reserve an unknown-type inode's blocks - file-shaped,
	// which is what `Inode::parse` builds it as - and did not follow that into the other
	// end. `drop_deleted_inode` branched on `== TYPE_FILE`, and an unknown type is neither
	// that nor a directory with a root, so removing the record dropped NOTHING: the blocks
	// stayed marked used for the life of the mount, and the repair verb the operator ran
	// to get them back did not get them back.
	//
	// The existing test asserts the record disappears. This one asserts the space does
	// too, which is the whole difference between the fix and the regression.
	//
	// The measurement is the same experiment twice - once on the file as written, once on
	// a byte-identical image with the type flipped to one no writer emits - because the
	// unknown type parses file-shaped and must therefore cost and return exactly what the
	// file does. A bare "free space went up" would pass on the day the numbers merely
	// moved; the control says what the right number IS.
	// The space has to come back WITHIN the mount. A remount, an fsck or a snapshot change
	// each rederive the free map from the medium and would hide the whole thing, which is
	// exactly why the regression could sit there: the blocks were never lost, only held
	// until something walked the volume again.
	let drain = |odd: bool| -> (Vec<u64>, Vec<u64>, u64) {
		let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
		// a spilled extent map, so the chain block is in the accounting with the data.
		write_spilled_file(&mut fs);
		let mut dev = fs.into_device();
		if odd {
			forge_inode_slot(&mut dev, |slot| slot[INO_TYPE_OFF] = 7);
		}
		let mut fs = LiberFs::mount(dev).unwrap();
		let num = fs.lookup(b"frag.bin").unwrap().unwrap();
		let inode = fs.read_inode(num).unwrap();
		assert_eq!(inode.r#type, if odd { 7 } else { TYPE_FILE }, "the forgery has to have landed");
		assert!(inode.extents.len() > EXTENTS_INLINE, "the map must spill, or the chain is not under test");
		let held: Vec<u64> = inode.extents.iter().flat_map(|e| [e.physical, e.csum]).chain([inode.spill]).collect();
		for &b in held.iter() {
			assert!(fs.is_alloc(b), "block {b} behind the record is reserved, as P02M0114 arranged");
		}
		fs.remove(b"frag.bin").unwrap();
		// what a transaction drops is freed one commit later - the superseded generation
		// still pins it until then. `set_compression` is the commit that allocates nothing
		// of its own, so what comes back is the deletion's alone (and it has to CHANGE the
		// setting, or the call is a no-op and no commit happens at all).
		fs.set_compression(true).unwrap();
		(held.clone(), held.into_iter().filter(|&b| fs.is_alloc(b)).collect(), fs.free_blocks())
	};
	let (plain_held, plain_stuck, plain_free) = drain(false);
	let (odd_held, odd_stuck, odd_free) = drain(true);
	assert_eq!(odd_held, plain_held, "the two images hold the same blocks to begin with");
	assert_eq!(plain_stuck, Vec::<u64>::new(), "removing a file returns every block it held");
	assert_eq!(odd_stuck, Vec::<u64>::new(), "and an unknown type is file-shaped, so removing it returns them too");
	assert_eq!(odd_free, plain_free, "down to the same free count as the file it parses as");
}

#[test]
fn a_null_child_slot_is_a_fault_not_an_empty_corner() {
	// `ptr == 0` means "nothing here", which is right for an empty directory root and for
	// a sentinel outside the tree - and wrong for a child slot of an internal node. Such a
	// node routes a whole key interval into every one of its `count + 1` slots, so a slot
	// pointing nowhere makes every name in that interval resolve to nothing while the
	// checksums verify and the counts add up. Both walks took it for an empty corner.
	let nblocks: u64 = 4_000;
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
	fs.mkdir(b"many").unwrap();
	for i in 0..400u32 {
		fs.write_file(format!("many/entry-{i:04}-{}", "p".repeat(40)).as_bytes(), b"x").unwrap();
	}
	let dir = fs.lookup(b"many").unwrap().unwrap();
	let mut inode = fs.read_inode(dir).unwrap();
	let mut buf = vec![0u8; BLOCK_SIZE];
	fs.read_node(inode.dir_root, inode.dir_root_crc, &mut buf).unwrap();
	assert_eq!(node_type(&buf), NODE_INTERNAL);

	// knock out child slot 1 and re-seal the tree above it, so the image is internally
	// consistent and the missing child is the only thing wrong with it.
	set_child(&mut buf, 1, 0, 0);
	let crc = fs.write_node_to(inode.dir_root, &buf).unwrap();
	inode.dir_root_crc = crc;
	fs.write_inode(dir, &mut inode).unwrap();
	fs.commit().unwrap();

	let report = fs.fsck().unwrap();
	assert!(mentions(&report.faults, b"has no child in slot 1"), "fsck names the missing child: {:?}", report.faults);
	// and the live walk refuses the shape rather than listing a directory short.
	assert_eq!(fs.read_dir(b"many"), Err(FsError::Corrupt), "a listing may not silently drop a whole routed interval");
}

#[test]
fn the_directory_checker_refuses_every_name_the_path_api_does() {
	// Creating a path refuses invalid UTF-8, empty segments, `.` and `..`, control
	// characters, the reserved punctuation set, NUL and over-long names. The structural
	// checker knew two of those eight, so a record read off the medium named `..`, or
	// `a/b`, or one carrying a control byte, passed an fsck that no path API could have
	// produced and none can address afterwards.
	for bad in [b"..".as_slice(), b"a/b".as_slice(), b"a\x01b".as_slice(), b"a:b".as_slice(), b".".as_slice()] {
		let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
		fs.write_file(b"placeholder", b"x").unwrap();
		let root = fs.root_inode;
		let mut inode = fs.read_inode(root).unwrap();
		let mut buf = vec![0u8; BLOCK_SIZE];
		fs.read_node(inode.dir_root, inode.dir_root_crc, &mut buf).unwrap();
		// rewrite the single leaf with one record carrying the impossible name, hash and
		// all, so nothing but the NAME is out of order.
		let child = dir_leaf_parse(&buf)[0].child;
		dir_leaf_write(&mut buf, &[DirRec { hash: name_hash(bad), name: bad.to_vec(), child }]);
		let crc = fs.write_node_to(inode.dir_root, &buf).unwrap();
		inode.dir_root_crc = crc;
		inode.size = 1;
		fs.write_inode(root, &mut inode).unwrap();
		fs.commit().unwrap();

		let report = fs.fsck().unwrap();
		assert!(mentions(&report.faults, b"not one this format can address"), "fsck must refuse the name {bad:?}: {:?}", report.faults);
	}
}

#[test]
fn a_label_field_with_a_dirty_tail_is_not_ours() {
	// `system\0\xff\xffgarbage` is not something any writer of this format produces: the
	// field is laid down NUL-padded. Nothing reads past the terminator, so this is not
	// about what `label()` returns - it is that a field shaped like that is itself proof
	// the record was written by something else, and refusing costs nothing.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"keep", b"payload").unwrap();
	let mut dev = fs.into_device();
	for slot in 0..SUPER_SLOTS as usize {
		forge_superblock(&mut dev, slot, |sb| {
			sb[SB_LABEL_OFF + 8..SB_LABEL_OFF + 16].copy_from_slice(b"\xff\xffgarbag");
		});
	}
	// Corrupt, not Unformatted, and the difference matters: the magic is there, so the
	// volume is OURS and failed its own checks - which is the answer the storage service
	// refuses to format over. "Unformatted" is the one word that licenses a format.
	assert_eq!(LiberFs::mount(dev).err(), Some(MountError::Corrupt), "a field no writer produces is not a superblock this build accepts");
}

#[test]
fn a_second_snapshots_bad_link_is_checked_too() {
	// fsck walks every snapshot with ONE visited bitmap, which is what makes it
	// affordable. The skip happened before the block was verified against the CRC recorded
	// in the link that reached it - so where two snapshots share a block, only the first
	// snapshot's link was ever checked. A second snapshot pointing at the same block with
	// a wrong expected CRC cannot be opened at all, and fsck reported nothing about it.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"shared.txt", b"payload").unwrap();
	// two snapshots taken back to back: nothing touched the inode tree between them, so
	// both records name the SAME root block. That is the shape the audit describes and the
	// one the shared bitmap cannot see past.
	fs.create_snapshot(b"first").unwrap();
	fs.create_snapshot(b"second").unwrap();
	assert_eq!(fs.list_snapshots().unwrap().len(), 2);
	assert_eq!(fs.fsck().unwrap().checksum_failures, 0, "both snapshots are sound to begin with");

	// break the SECOND record's expected root CRC only. The block itself is untouched and
	// the first snapshot still reaches it with the right CRC, so by the time the second
	// record is walked the block is already marked visited.
	let mut dev = fs.into_device();
	let mut first_root = 0u64;
	let mut second_root = 0u64;
	forge_snapshot_record_at(&mut dev, 0, |rec| first_root = u64::from_le_bytes(rec[SNAP_ROOT_OFF..SNAP_ROOT_OFF + 8].try_into().unwrap()));
	forge_snapshot_record_at(&mut dev, 1, |rec| {
		second_root = u64::from_le_bytes(rec[SNAP_ROOT_OFF..SNAP_ROOT_OFF + 8].try_into().unwrap());
		let crc = u32::from_le_bytes(rec[SNAP_ROOT_CRC_OFF..SNAP_ROOT_CRC_OFF + 4].try_into().unwrap());
		rec[SNAP_ROOT_CRC_OFF..SNAP_ROOT_CRC_OFF + 4].copy_from_slice(&(crc ^ 1).to_le_bytes());
	});
	assert_eq!(first_root, second_root, "the two snapshots must share the root, or this proves nothing");

	let mut fs = LiberFs::mount(dev).expect("the volume still mounts");
	let report = fs.fsck().unwrap();
	assert!(report.checksum_failures > 0, "a link that cannot be believed is a failure however many other links reach the same block");
	// and the snapshot really is unopenable, which is the harm the silence was hiding.
	assert_eq!(fs.read_file_from_snapshot(b"second", b"shared.txt"), Err(FsError::Corrupt));
	assert_eq!(fs.read_file_from_snapshot(b"first", b"shared.txt").unwrap(), b"payload");
}

// A guard that disarms the allocation injector however the test leaves - a panicking
// assertion must not leave the switch armed for the next test on this thread.
struct Injected;

impl Drop for Injected {
	fn drop(&mut self) {
		inject::disarm();
	}
}

fn fail_allocation_after(successes: usize) -> Injected {
	inject::fail_after(successes);
	Injected
}

// Refuse exactly one allocation, so a caller that discards the refusal is not rescued by the next
// one failing too. See `a_directorys_speculative_reservation_is_not_skipped_when_memory_is_short`.
fn fail_one_allocation_after(successes: usize) -> Injected {
	inject::fail_once_after(successes);
	Injected
}

#[test]
fn a_check_that_could_not_run_is_a_fault_not_a_clean_report() {
	// `check_dir_structure` began `let Ok(mut visited) = try_zeroed(..) else { return }`, so
	// a refused allocation checked nothing and reported nothing - and fsck handed back a
	// clean structural report for directories it never looked at. A report that cannot
	// distinguish "checked and sound" from "not checked" is worse than one that admits the
	// difference.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"d").unwrap();
	fs.write_file(b"d/f.txt", b"payload").unwrap();
	// clean first, so the fault below is the injection's and not the volume's.
	let mut clean: Vec<Vec<u8>> = Vec::new();
	fs.check_structure(&mut clean);
	assert_eq!(clean.len(), 0, "the volume itself is sound: {clean:?}");

	// the pass allocates its inode-tree map first and then one map per directory, so
	// letting exactly one through puts the refusal inside `check_dir_structure`.
	let mut faults: Vec<Vec<u8>> = Vec::new();
	{
		let _armed = fail_allocation_after(1);
		fs.check_structure(&mut faults);
	}
	assert!(mentions(&faults, b"memory to check this directory"), "a check that could not run says so: {faults:?}");
	// and the whole report carries it, so no caller reads a directory that was skipped as
	// a directory that passed.
	//
	// SWEPT RATHER THAN TUNED. This named one budget, chosen by counting the fallible growth points
	// the pass
	// made at the time - so making one more allocation fallible moved the number and the test failed
	// for a reason that was not about what it asserts. What it means is "there is a budget that lands
	// inside the directory walk", and sweeping says exactly that while also requiring that no budget
	// aborts or answers a clean report it did not earn.
	let mut reported = false;
	for budget in 0..40 {
		let outcome = {
			let _armed = fail_allocation_after(budget);
			fs.fsck()
		};
		match outcome {
			// Refused before it could report anything, which is the other legal answer.
			Err(FsError::NoMemory) => {}
			Err(other) => panic!("budget {budget}: a short allocation must not be reported as {other:?}"),
			Ok(report) => {
				if report.structural_failures > 0 {
					reported = true;
				}
			}
		}
	}
	assert!(reported, "some budget lands inside the directory walk, and the report counts what it could not check");
}

#[test]
fn a_mount_short_of_memory_does_not_blame_the_disk() {
	// `derive_free` sizes its own maps, and `try_zeroed` reports a refusal as `NoSpace`.
	// The mount folded everything but `Corrupt` into `MountError::Io`, so StorageService
	// told the operator the disk did not answer when the disk was fine and the machine was
	// short. The process no longer aborting was the point of the fallible allocation;
	// carrying the reason through is the rest of it.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"keep", b"payload").unwrap();
	let dev = fs.into_device();
	// the mount builds the two superblock-sized maps first (those already answer
	// NoMemory), and `derive_free` is what allocates next.
	let _armed = fail_allocation_after(2);
	assert_eq!(LiberFs::mount(dev).err(), Some(MountError::NoMemory), "a refused allocation is the machine, not the medium");
}

#[test]
fn a_mount_that_runs_short_inside_the_namespace_walk_refuses_rather_than_aborts() {
	// THE ALIAS SET, which stopped being fallible when it stopped being a bitmap.
	//
	// It was `next_inode / 8` bytes from `try_zeroed` - the wrong SIZE, since inode numbers are
	// never recycled, but a mount that failed rather than one that corrupted. Replacing it with a
	// `BTreeSet` fixed the size and dropped the refusal: `insert` has no fallible form, so a mount
	// walking a large namespace on a short heap aborted the process rather than answering
	// `NoMemory`.
	//
	// Every allocation budget from nothing to plenty: a mount either completes or refuses, and it
	// never does anything else. The interesting ones are in the middle, where the failure lands
	// inside the walk rather than on one of the maps before it.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	for i in 0..24u32 {
		fs.write_file(alloc::format!("f{i}").as_bytes(), b"payload").unwrap();
	}
	let dev = fs.into_device();
	let mut refused = 0u32;
	let mut mounted = 0u32;
	for budget in 0..64usize {
		let attempt = dev.clone();
		let _armed = fail_allocation_after(budget);
		match LiberFs::mount(attempt) {
			Ok(_) => mounted += 1,
			Err(MountError::NoMemory) => refused += 1,
			Err(other) => panic!("budget {budget}: a refused allocation must be NoMemory, not {other:?}"),
		}
	}
	assert!(refused > 0, "no budget in the sweep produced a refusal, so nothing here was exercised");
	assert!(mounted > 0, "no budget in the sweep completed a mount, so the sweep never reached the walk");

	// AND THE BUDGETS REACH THE WALK, which is what this test is named for and could not show.
	//
	// The alias set did not go through the injected allocator: it called `Vec::try_reserve` directly,
	// so every budget above landed on the free map, the pinned map and the other `try_zeroed` calls,
	// and none of them on the allocation the comment at the top of this test is about. A test that
	// names a specific allocation and cannot fail it is a check that passes for a reason other than
	// the one it states.
	//
	// The evidence is a COUNT that moves with the namespace: the walk pushes one inode number per
	// reachable inode, so a volume with more files passes strictly more growth points on mount. (A
	// "budget" counts calls that COULD be refused, not mallocs - `try_push` fires the injector even
	// when the vector already has room, and that is the unit the property is about: every growth
	// point can refuse and the caller survives it.) Nothing
	// else on the mount path scales with the file count.
	let wide = smallest_budget_that_mounts(&dev);
	let mut narrow_fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	for i in 0..12u32 {
		narrow_fs.write_file(alloc::format!("f{i}").as_bytes(), b"payload").unwrap();
	}
	let narrow = smallest_budget_that_mounts(&narrow_fs.into_device());
	assert!(wide - narrow >= 12, "twelve more files cost at least twelve more growth points ({narrow} against {wide}) - one per extra inode, which is the alias set being allocated through the injector at all. Nothing that scales with the FILE COUNT rather than the block count is allocated any other way here.");
}

// The smallest allocation budget under which a mount completes: one more than the number of
// growth points the mount passes - calls that could be refused, which is not the same as mallocs.
fn smallest_budget_that_mounts(dev: &MemDevice) -> usize {
	for budget in 0..512usize {
		let attempt = dev.clone();
		let _armed = fail_allocation_after(budget);
		if LiberFs::mount(attempt).is_ok() {
			return budget;
		}
	}
	panic!("no budget under 512 mounts this volume");
}

#[test]
fn an_unknown_type_that_was_a_directory_keeps_its_tree_reserved() {
	// The mirror of `removing_an_unknown_type_inode_returns_its_blocks`, and the direction that was
	// not covered. `INO_MAP` is an overlay - `dir_root` for a directory, `spill` for everything
	// else - and reserving only the FILE reading protects one bit-flip out of two.
	//
	// An inode that was a directory arrives with `extent_count` zero, so the file reading marks
	// almost nothing: its B+tree's internal and leaf nodes are left free for the allocator while the
	// tree still references them, and the mount stays writable. The child inodes and their data
	// survive; the index that named them is overwritten. That is worse than the case that was fixed,
	// because a lost directory takes a whole subtree's reachability with it.
	let nblocks: u64 = 4_000;
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
	// A directory big enough to have an internal node, so there are children to lose.
	fs.mkdir(b"many").unwrap();
	for i in 0..400u32 {
		fs.write_file(format!("many/entry-{i:04}-{}", "p".repeat(40)).as_bytes(), b"x").unwrap();
	}
	let dir = fs.lookup(b"many").unwrap().unwrap();
	let inode = fs.read_inode(dir).unwrap();
	let root = inode.dir_root;
	let mut buf = vec![0u8; BLOCK_SIZE];
	fs.read_node(root, inode.dir_root_crc, &mut buf).unwrap();
	assert_eq!(node_type(&buf), NODE_INTERNAL, "the directory must have an internal node, or there is nothing to lose");
	// every child of the root: the blocks the file reading cannot see.
	let children: Vec<u64> = (0..=internal_count(&buf)).map(|i| child_ptr(&buf, i)).collect();
	assert!(children.len() > 1);

	// flip the type byte of THIS inode to one no writer emits.
	let mut dev = fs.into_device();
	forge_inode_slot_of(&mut dev, dir, |slot| slot[INO_TYPE_OFF] = 7);
	let mut fs = LiberFs::mount(dev).expect("the volume still mounts");
	assert_eq!(fs.read_inode(dir).unwrap().r#type, 7, "the forgery has to have landed");

	assert!(fs.is_alloc(root), "the directory's root node is reserved");
	for &child in children.iter() {
		assert!(fs.is_alloc(child), "child node {child} of a directory that lost its type byte may not be free for the allocator");
	}
}

#[test]
fn a_read_this_machine_cannot_hold_reports_rather_than_aborts() {
	// The read path was the one place a number off the medium sized an allocation directly, and it
	// used `Vec::with_capacity` - which answers an impossible request by ABORTING the process. The
	// pool's byte count bounds it, and a volume can be larger than the machine it is mounted on.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"small", b"payload").unwrap();
	let num = fs.lookup(b"small").unwrap().unwrap();
	let inode = fs.read_inode(num).unwrap();

	// Refuse the read's own buffer, and the read reports instead of dying.
	//
	// `NoMemory`, not `NoSpace`: a refused ALLOCATION is the machine being short, and the two drive
	// opposite policies - `NoSpace` says delete something or use another volume, `NoMemory` says the
	// service is under pressure and the same request may well succeed in a moment. This asserted
	// `NoSpace` because that is what the fault injector used to return while the allocator beside it
	// returned `NoMemory`, so the test pinned the injector rather than the operation.
	{
		let _armed = fail_allocation_after(0);
		assert_eq!(fs.read_range(&inode, 0, 7), Err(FsError::NoMemory));
	}
	// and with the injector disarmed it reads as it always did.
	assert_eq!(fs.read_file(b"small").unwrap(), b"payload");
}

#[test]
fn a_truncated_directory_leaf_may_be_read_but_not_edited() {
	// The structural pass calls a leaf holding fewer records than its header claims a truncated
	// leaf, and it only runs when somebody asks for `fsck`. An ordinary writable mount went on
	// inserting into it - rewriting the leaf compactly from what parsed, which makes the damage
	// permanent and self-consistent and loses whatever the tail held.
	//
	// The forgery has to be a leaf that is nearly FULL: `dir_leaf_parse` breaks at a record whose
	// declared name runs past the end of the block, and a record can only reach the end when the
	// ones in front of it have filled the leaf. A short leaf with a raised count is not a truncated
	// leaf - the zero padding parses as empty-named records and the count comes out right.
	let nblocks: u64 = 2_000;
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
	fs.mkdir(b"d").unwrap();
	// SHORT names, many of them: the last record has to START within 255 bytes of the end of the
	// block for its declared length - one byte - to be able to run past it, and short records are
	// what pack a leaf that tightly. A three-byte name gives a 16-byte record, and 240 of them put
	// the last one at 3832 of the 4096 available, which is inside the reach of a u8 length.
	for i in 0..240u32 {
		fs.write_file(format!("d/{}", (b'a' + (i / 100) as u8) as char).as_bytes().iter().copied().chain(format!("{:02}", i % 100).bytes()).collect::<Vec<u8>>().as_slice(), b"x").unwrap();
	}
	let dir = fs.lookup(b"d").unwrap().unwrap();
	let mut inode = fs.read_inode(dir).unwrap();
	let mut buf = vec![0u8; BLOCK_SIZE];
	fs.read_node(inode.dir_root, inode.dir_root_crc, &mut buf).unwrap();
	assert_eq!(node_type(&buf), NODE_LEAF, "the names must still be one leaf");
	let claimed = node_count(&buf);
	assert_eq!(claimed, 240);

	// Walk to the LAST record and make its declared name run past the end of the block: the shape a
	// write cut short leaves behind.
	let mut off = NODE_HDR;
	for _ in 0..claimed - 1 {
		off += DIR_REC_HDR + buf[off + 12] as usize;
	}
	// The smallest declared length that runs past the end of the block, computed rather than
	// guessed: a record's length is one byte, so the last record has to START within 255 of the end
	// for the overrun to be expressible at all.
	let need = BLOCK_SIZE - off - DIR_REC_HDR + 1;
	assert!(need <= 255, "the last record starts at {off}, which is {need} short of a length a byte can hold - the leaf is not full enough");
	buf[off + 12] = need as u8;
	let crc = fs.write_node_to(inode.dir_root, &buf).unwrap();
	inode.dir_root_crc = crc;
	fs.write_inode(dir, &mut inode).unwrap();
	fs.commit().unwrap();

	// A listing still works - it is what the operator has left, and refusing it takes the rescue
	// away.
	assert_eq!(fs.read_dir(b"d").unwrap().len(), 239, "a truncated leaf still lists what it holds");
	// fsck names it.
	assert!(mentions(&fs.fsck().unwrap().faults, b"records it claims"), "fsck reports the truncated leaf");
	// and nothing edits it.
	assert_eq!(fs.write_file(b"d/another", b"c"), Err(FsError::Corrupt), "an insert into a leaf that is not what it claims is refused");
}

#[test]
fn an_unsorted_directory_leaf_may_be_read_but_not_edited() {
	// `leaf_is_whole` checked ONE invariant - that a leaf holds as many records as its header
	// claims - and the write path checked nothing else. `fsck` knew nine more, and the one the
	// write path actually depends on was among them: `dir_recs_search` is a BINARY SEARCH over the
	// records, so a checksum-valid leaf whose records do not ascend answers arbitrarily.
	//
	// `dir_insert_node` then takes that answer as either "the name is here, replace its child" or
	// "insert at this position", writes the leaf back with `dir_leaf_write`, and `write_node_to`
	// computes a fresh CRC over it. What lands is a new generation, correctly checksummed, built on
	// a structure the format cannot produce - a name duplicated or one made unreachable, and
	// nothing left afterwards to say which.
	let nblocks: u64 = 2_000;
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
	fs.mkdir(b"d").unwrap();
	// Enough names to be a leaf with several records, few enough to stay ONE leaf.
	for i in 0..8u32 {
		fs.write_file(format!("d/name{i:02}").as_bytes(), b"x").unwrap();
	}
	let dir = fs.lookup(b"d").unwrap().unwrap();
	let mut inode = fs.read_inode(dir).unwrap();
	let mut buf = vec![0u8; BLOCK_SIZE];
	fs.read_node(inode.dir_root, inode.dir_root_crc, &mut buf).unwrap();
	assert_eq!(node_type(&buf), NODE_LEAF, "the names must still be one leaf");
	let recs = dir_leaf_parse(&buf);
	assert_eq!(recs.len(), 8);

	// Swap two records so the leaf is out of order and NOTHING else about it changes: every record
	// is complete, the count matches, each stored hash is still its own name's, and the checksum is
	// rewritten so the block verifies. The only thing wrong is the order.
	let mut swapped: Vec<DirRec> = recs.iter().map(|r| DirRec { hash: r.hash, name: r.name.clone(), child: r.child }).collect();
	swapped.swap(0, 7);
	assert_ne!(swapped[0].hash, recs[0].hash, "the swap has to actually disorder the leaf");
	dir_leaf_write(&mut buf, &swapped);
	let crc = fs.write_node_to(inode.dir_root, &buf).unwrap();
	inode.dir_root_crc = crc;
	fs.write_inode(dir, &mut inode).unwrap();
	fs.commit().unwrap();

	// A listing still works: it is what the operator has left, and refusing it takes the rescue
	// away. Every name is there, whatever order they come back in.
	let listed = fs.read_dir(b"d").unwrap();
	assert_eq!(listed.len(), 8, "an unsorted leaf still lists what it holds");
	// fsck names it.
	assert!(mentions(&fs.fsck().unwrap().faults, b"ascending"), "fsck reports the disordered leaf");
	// And nothing edits it - neither an insert nor a delete may build a new generation on it.
	assert_eq!(fs.write_file(b"d/another", b"c"), Err(FsError::Corrupt), "an insert into a leaf whose order cannot be trusted is refused");
	assert_eq!(fs.remove(b"d/name03"), Err(FsError::Corrupt), "and so is a delete");
}

#[test]
fn a_directorys_speculative_reservation_is_not_skipped_when_memory_is_short() {
	// An inode with a type byte no writer emits is marked TWICE - once as a file, once as a
	// directory - because the overlay field means the flip could have gone either way, and
	// under-reserving loses data. The directory reading marks into its own scratch map.
	//
	// That map was `try_zeroed(map.len()).ok()` with an `if let` around it, so a refused allocation
	// skipped the whole speculative walk: `derive_free` returned Ok, the mount stayed WRITABLE, and
	// none of the directory's tree blocks were reserved. Exactly the loss the second reading exists
	// to prevent, disappearing without a word at the moment the machine is under strain.
	//
	// A failure to PARSE the guess is still ignored - one of the two readings is nonsense by
	// construction. A failure to ALLOCATE the protection is not the guess failing; it is the
	// protection not happening.
	let nblocks: u64 = 4_000;
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
	fs.mkdir(b"many").unwrap();
	for i in 0..400u32 {
		fs.write_file(format!("many/entry-{i:04}-{}", "p".repeat(40)).as_bytes(), b"x").unwrap();
	}
	let dir = fs.lookup(b"many").unwrap().unwrap();
	let mut dev = fs.into_device();
	forge_inode_slot_of(&mut dev, dir, |slot| slot[INO_TYPE_OFF] = 7);

	// Exactly ONE refusal, and it has to be the scratch map's. Three growth points precede it: the
	// mount's two superblock-sized maps, then `derive_free`'s live map.
	//
	// A one-shot refusal is the whole point of the fixture. With `fail_after`, every allocation
	// past the target fails too - so a walk that DISCARDED the scratch map's failure would hit the
	// next allocation and answer `NoMemory` anyway, and the test would pass over the defect it was
	// written for. Refusing one and letting the rest through is the only arrangement that can tell
	// a propagated refusal from a swallowed one.
	let _armed = fail_one_allocation_after(3);
	assert_eq!(LiberFs::mount(dev).err(), Some(MountError::NoMemory), "a protection that could not be allocated is a refused mount, not a silent gap");
}

#[test]
fn a_snapshot_table_has_a_ceiling_and_both_sides_of_it_agree() {
	// The table was built with an infallible `push` per record and the walk admitted as many as the
	// chain had room for - on a one-gigabyte volume, around twelve million names off a forged but
	// checksum-valid table. The duplicate-name check made it worse rather than better: `out.iter()
	// .any(..)` per record is quadratic, so the mount would not finish long before it ran out of
	// memory, and a hang at mount is a system that does not boot with nothing said about why.
	//
	// The ceiling is a format rule, so it has two halves: the writer refuses to create past it and
	// the reader refuses a medium claiming more. Only the writer's half can be reached from here
	// without forging a chain; they share the constant, which is the point of stating it once.
	let nblocks: u64 = 4_000;
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
	fs.write_file(b"payload", b"x").unwrap();
	for i in 0..MAX_SNAPSHOTS {
		fs.create_snapshot(format!("snap{i:04}").as_bytes()).unwrap_or_else(|e| panic!("snapshot {i} of {MAX_SNAPSHOTS}: {e:?}"));
	}
	assert_eq!(fs.create_snapshot(b"one-too-many"), Err(FsError::NoSpace), "the writer refuses to create a table this build could not read back");
	// and what it did create still reads back whole.
	let dev = fs.into_device();
	let mut fs = LiberFs::mount(dev).expect("a table at the ceiling still mounts");
	assert_eq!(fs.list_snapshots().unwrap().len(), MAX_SNAPSHOTS);
}

#[test]
fn a_snapshot_name_has_to_be_padded_the_way_a_writer_pads_it() {
	// `read_superblock` was taught this for the volume label and the snapshot record was left with
	// half the rule: `name_in` stops at the first NUL and looks no further, so
	// `keep\0\xff\xffgarbage` read back as the snapshot "keep". Nothing consumes those bytes, which
	// is why it went unnoticed - the argument is not about what the name resolves to but that no
	// writer of this format produces a field shaped that way, so a field shaped that way is itself
	// evidence the record came from somewhere else.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"payload", b"x").unwrap();
	fs.create_snapshot(b"keep").unwrap();
	let mut dev = fs.into_device();
	// Past the terminator, inside the fixed field: bytes no writer ever leaves there.
	forge_snapshot_record(&mut dev, |rec| {
		rec[5] = 0xFF;
		rec[6] = 0xFF;
	});
	assert!(matches!(LiberFs::mount(dev), Err(MountError::Corrupt) | Ok(_)), "the mount either refuses or degrades - it may not accept the record as written");
	// Precisely: the table cannot be loaded, which is what degrades the volume.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"payload", b"x").unwrap();
	fs.create_snapshot(b"keep").unwrap();
	let mut dev = fs.into_device();
	forge_snapshot_record(&mut dev, |rec| rec[5] = 0xFF);
	let fs = LiberFs::mount(dev);
	assert!(fs.as_ref().map(|f| f.is_read_only()).unwrap_or(true), "a snapshot record no writer produced may not leave the volume writable");
}

#[test]
fn an_extent_count_above_the_pool_is_refused_before_it_is_allocated_for() {
	// `load_spill` grew `inode.extents` with an infallible `push`, bounded by the inode's
	// `extent_count` - a `u32` off the medium, which is a WIDTH rather than a limit anybody chose.
	// It is reached from `derive_free` at mount, so this was not confined to reading a hostile file.
	//
	// An extent names a run of blocks, so an inode cannot hold more extents than the pool holds
	// blocks. That is the bound the volume states, and a claim above it is a claim the format
	// cannot have written.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"file", b"payload").unwrap();
	let num = fs.lookup(b"file").unwrap().unwrap();
	let mut dev = fs.into_device();
	forge_inode_slot_of(&mut dev, num, |slot| slot[INO_EXTENT_COUNT_OFF..INO_EXTENT_COUNT_OFF + 4].copy_from_slice(&u32::MAX.to_le_bytes()));
	// The mount's own walk reaches it, and answers about the medium rather than aborting.
	let mounted = LiberFs::mount(dev);
	match mounted {
		Err(MountError::Corrupt) => {}
		Ok(mut fs) => {
			assert_eq!(fs.read_file(b"file"), Err(FsError::Corrupt), "a count the pool cannot hold is refused rather than allocated for");
		}
		Err(other) => panic!("expected the medium to be blamed, got {other:?}"),
	}
}

// A write that cannot allocate the block buffer for a spilled extent map says so, and does not take
// the process down.
//
// The mount side of this rule was P02M0123's work: `load_spill` bounds `extent_count` and reserves
// fallibly. The WRITE side kept two infallible allocations - a `to_vec()` of the whole spilled map,
// and a `vec![0u8; BLOCK_SIZE]` per chain block - and the first one scales with how fragmented the
// file is, which is the case that reaches memory pressure in the first place.
//
// The copy is gone rather than made fallible: the chunks are read once, in order, and `Extent` is
// `Copy`, so the chain is built from indexed slices of the map itself. What remains is the block
// buffer, and this is the test that it reports rather than aborts.
#[test]
fn a_spilled_extent_map_that_cannot_allocate_its_block_says_so() {
	let nblocks: u64 = 512;
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
	let span = |i: u64| i * 16 * BLOCK_SIZE as u64;
	// Two inline capacities' worth of sparse spans, so the map has to spill.
	for i in 0..8u64 {
		fs.write_at(b"sparse", span(i), format!("span-{i}").as_bytes()).unwrap();
	}
	let num = fs.lookup(b"sparse").unwrap().unwrap();
	assert!(fs.read_inode(num).unwrap().extents.len() > EXTENTS_INLINE, "the fixture must spill or this proves nothing");

	// Refuse ONE allocation, some way into the next write. Refusing every allocation from the first
	// would fail somewhere earlier and prove nothing about the chain; refusing exactly one means the
	// error has to be carried out rather than being re-raised by the next attempt.
	//
	// The budget is a search rather than a constant: which allocation belongs to `flush_extents`
	// depends on what the write above did first, and pinning it would make this test a hostage to
	// unrelated allocation counts.
	let mut reported = false;
	for budget in 0..64 {
		// A fresh filesystem per budget: the injection is a count of GROWTH POINTS from the moment it
		// is armed, so the state it is armed over has to be identical each time.
		let mut attempt = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
		for i in 0..8u64 {
			attempt.write_at(b"sparse", span(i), format!("span-{i}").as_bytes()).unwrap();
		}
		let guard = fail_one_allocation_after(budget);
		let result = attempt.write_at(b"sparse", span(9), b"one more span");
		drop(guard);
		match result {
			// `NoMemory` is a refused allocation and `NoSpace` is a full volume. The search here
			// starves the ALLOCATOR, so the first is the answer; this asserted the second because
			// the injector returned it while the allocator returned the first.
			Err(FsError::NoMemory) | Err(FsError::NoSpace) => {
				reported = true;
				// And the filesystem is still answerable afterwards - a refused write is an error,
				// not a corrupted map.
				assert!(attempt.read_at(b"sparse", span(1), 6).is_ok(), "the earlier spans still read after a refused write");
				// AND THE WRITE THAT WAS REFUSED DID NOT HAPPEN, which is the half this sweep was
				// missing and the half the contract is about.
				//
				// `NoMemory` reaches StorageService as `again`, which tells the caller to retry - so
				// it is only a truthful answer if nothing landed. A commit that published its
				// superblock and THEN failed to allocate would answer `NoMemory` for a transaction
				// already on the medium, and the caller would retry a write it was told it had lost.
				// The answer for a failure past that line is `CommitUncertain`, which reaches the
				// caller as `denied` precisely so it does not retry.
				//
				// Reading the range back is what makes this a statement about the MEDIUM rather
				// than about a return value: the span the refused write aimed at must still be
				// absent.
				let mut probe = [0u8; 13];
				match attempt.read_at(b"sparse", span(9), 13) {
					Ok(bytes) => {
						probe[..bytes.len().min(13)].copy_from_slice(&bytes[..bytes.len().min(13)]);
						assert_ne!(&probe[..], b"one more span", "budget {budget}: the write reported NoMemory and its bytes are on the volume - a retryable answer for a transaction that landed");
					}
					// A range that was never written reads as absent or as zeros; either is the
					// write not having happened.
					Err(_) => {}
				}
			}
			Err(other) => panic!("a refused allocation must be NoMemory or NoSpace, not {other:?}"),
			Ok(()) => continue,
		}
	}
	assert!(reported, "no allocation budget in the search produced a refusal, so nothing here was exercised");
}

// A leaf holding a key the separators above it do not route to makes the mount READ-ONLY.
//
// The write path's structural validators are local: `validate_fixed_leaf` answers "are these records
// ordered and the right size", `validate_internal` answers "are these separators ordered and the
// children non-null". Neither can answer the question that matters here - does this node belong
// where it was reached from - because neither is told where it was reached from.
//
// The consequence is a second copy of a record. Lookup for a misrouted key goes down the subtree the
// separator names and does not find it, so an insert of the same key puts a record THERE, and the
// mutation writes a fresh, correctly checksummed generation holding it twice. Every later read is
// decided by which path it walks, and fsck's report arrives after the damage rather than before it.
//
// `fsck` has carried the routing interval since P02M0114; a writable mount never ran it. The interval
// now rides along with the free-map walk, which visits these blocks anyway.
#[test]
fn an_inode_leaf_outside_the_range_that_routes_to_it_makes_the_mount_read_only() {
	let nblocks: u64 = 512;
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
	// Enough inodes that the tree has an internal root to misroute from.
	for i in 0..200u32 {
		fs.write_file(format!("f{i:03}").as_bytes(), b"x").unwrap();
	}
	let mut dev = fs.into_device();
	let slot = active_slot(&dev);
	let sb = parse_superblock(&dev.blocks[slot * BLOCK_SIZE..(slot + 1) * BLOCK_SIZE]).unwrap();
	let root = sb.inode_root as usize;
	let at = root * BLOCK_SIZE;
	assert_eq!(node_type(&dev.blocks[at..at + BLOCK_SIZE]), NODE_INTERNAL, "200 files must give the inode tree an internal root");

	// The forgery is ONE field: the first separator, lowered to 2. Child 0 then routes to keys
	// below 2 and holds inode numbers far above it - every record in it is unreachable, and an
	// insert of any of those numbers would land in child 1.
	//
	// Deliberately the separator rather than the leaf: the leaf stays byte-for-byte a leaf the write
	// path accepts, which is the whole point. Nothing here is out of order, mis-hashed or the wrong
	// size, and a local validator has nothing to say about it.
	let before = sep_key(&dev.blocks[at..at + BLOCK_SIZE], 0);
	assert!(before > 2, "the fixture's first separator must be above the value we lower it to (was {before})");
	set_sep(&mut dev.blocks[at..at + BLOCK_SIZE], 0, 2);
	let crc = crc32c(&dev.blocks[at..at + BLOCK_SIZE]);
	forge_superblock(&mut dev, slot, |sb| sb[SB_INODE_ROOT_CRC_OFF..SB_INODE_ROOT_CRC_OFF + 4].copy_from_slice(&crc.to_le_bytes()));

	// It mounts - every checksum is right and every node is well-formed on its own - and it mounts
	// READ-ONLY, which is the answer that keeps both the data and the repair verb.
	let mut fs = LiberFs::mount(dev).expect("a checksum-consistent volume still mounts");
	assert!(fs.is_read_only(), "a tree whose leaves are not where routing sends them must not be written to");
	// And the structural pass names it, so an operator is told what it is rather than only that the
	// volume is read-only.
	let report = fs.fsck().unwrap();
	assert!(mentions(&report.faults, b"routing will never reach"), "fsck must name the misrouted records: {:?}", report.faults);
}

// A snapshot chain ON DISK carrying more records than the format permits is refused.
//
// `MAX_SNAPSHOTS` was covered from the writer's side - take 256 snapshots and the 257th is refused -
// which proves the writer stops and says nothing about the reader. The reader is the side that faces
// a hostile image, and before the cap it would build the whole table in memory: a checksum-valid
// chain can name millions of records, and the mount allocated one per record before looking at any
// of them.
#[test]
fn a_forged_chain_of_more_snapshots_than_the_format_allows_is_refused() {
	let nblocks: u64 = 512;
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
	fs.write_file(b"a.txt", b"payload").unwrap();
	let root = fs.inode_root;
	let root_crc = fs.inode_root_crc;
	let mut dev = fs.into_device();

	// One record past the ceiling, laid by hand into blocks the fixture is not using. Every
	// checksum is correct and every record is individually legal - a real root, a generation that
	// already happened, a name with canonical padding - so the only thing wrong with this table is
	// how long it is.
	const FORGED: usize = MAX_SNAPSHOTS + 1;
	let blocks = FORGED.div_ceil(SNAPS_PER_BLOCK);
	let base = 200usize;
	assert!(base + blocks < nblocks as usize, "the forged chain must fit in the fixture");
	let mut next_ptr = 0u64;
	let mut next_crc = 0u32;
	let mut left = FORGED;
	for i in (0..blocks).rev() {
		let count = left.min(SNAPS_PER_BLOCK);
		left -= count;
		let at = (base + i) * BLOCK_SIZE;
		dev.blocks[at..at + BLOCK_SIZE].fill(0);
		dev.blocks[at + CHAIN_NEXT_OFF..at + CHAIN_NEXT_OFF + 8].copy_from_slice(&next_ptr.to_le_bytes());
		dev.blocks[at + CHAIN_CRC_OFF..at + CHAIN_CRC_OFF + 4].copy_from_slice(&next_crc.to_le_bytes());
		dev.blocks[at + CHAIN_COUNT_OFF..at + CHAIN_COUNT_OFF + 4].copy_from_slice(&(count as u32).to_le_bytes());
		for r in 0..count {
			let off = at + SNAP_HDR + r * SNAP_REC;
			let name = format!("s{:05}", left + r);
			dev.blocks[off..off + name.len()].copy_from_slice(name.as_bytes());
			dev.blocks[off + SNAP_ROOT_OFF..off + SNAP_ROOT_OFF + 8].copy_from_slice(&root.to_le_bytes());
			dev.blocks[off + SNAP_ROOT_CRC_OFF..off + SNAP_ROOT_CRC_OFF + 4].copy_from_slice(&root_crc.to_le_bytes());
			// generation 0 is at or below every live generation, so the record's own sanity checks
			// pass and the length is the only thing left to refuse it for.
			dev.blocks[off + SNAP_GEN_OFF..off + SNAP_GEN_OFF + 8].copy_from_slice(&0u64.to_le_bytes());
		}
		next_ptr = (base + i) as u64;
		next_crc = crc32c(&dev.blocks[at..at + BLOCK_SIZE]);
	}
	let slot = active_slot(&dev);
	forge_superblock(&mut dev, slot, |sb| {
		sb[SB_SNAP_ROOT_OFF..SB_SNAP_ROOT_OFF + 8].copy_from_slice(&next_ptr.to_le_bytes());
		sb[SB_SNAP_ROOT_CRC_OFF..SB_SNAP_ROOT_CRC_OFF + 4].copy_from_slice(&next_crc.to_le_bytes());
	});

	// The mount does not build the table and does not write to the volume.
	let fs = LiberFs::mount(dev);
	match fs {
		Ok(fs) => assert!(fs.is_read_only(), "a table longer than the format allows must at least foreclose writing"),
		Err(_) => {}
	}
}

// `check_structure` compares every field of the on-disk snapshot table with the mounted one, and
// `inode_root_crc` is the field that was missing.
//
// It is the one that decides whether the pinned generation can be READ: a record whose stored CRC
// has drifted from the loaded one names a root that `read_node` will refuse, so the snapshot is
// unopenable while every other field still looks right. Name, root and generation all agreeing is
// exactly the case that hid it.
//
// The drift is introduced from the MOUNTED side rather than the disk, because a `MemDevice` moves
// into the filesystem and cannot be edited underneath it from a test. The comparison is symmetric -
// it reports the two tables differing, not which one moved - so this exercises the same branch a
// disk that changed under a long-lived mount would.
#[test]
fn a_snapshot_whose_root_crc_drifted_from_the_disk_is_reported() {
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"a.txt", b"payload").unwrap();
	fs.create_snapshot(b"before").unwrap();
	fs.write_file(b"b.txt", b"more").unwrap();
	assert_eq!(fs.fsck().unwrap().structural_failures, 0, "the volume is sound before the drift");

	// One field, on the mounted side. Name, root and generation are untouched.
	fs.snapshots[0].inode_root_crc ^= 1;
	let report = fs.fsck().unwrap();
	assert!(mentions(&report.faults, b"the snapshot table on disk differs from the mounted one"), "a drifted root CRC must be named: {:?}", report.faults);
}

#[test]
fn the_metadata_allocator_can_use_the_pools_first_block() {
	// `POOL_START` is the first block of the pool - the data allocator hands it out - and the
	// downward metadata scan tested `block <= POOL_START` and `candidate > POOL_START`, so it
	// skipped it. A volume whose only free block is that one answered `NoSpace` to a metadata
	// request while the block sat free. Nothing was permanently lost, because the data side could
	// still use it, which is exactly why it went unnoticed.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	// Fill the volume until nothing is free, then free exactly one block and ask for it as
	// metadata. Which block comes back last is the allocator's business; that a freed block can be
	// re-used by either side is not.
	let mut written = 0u32;
	loop {
		let name = alloc::format!("f{written:05}");
		match fs.write_file(name.as_bytes(), &[0xAA; 4096]) {
			Ok(()) => written += 1,
			Err(FsError::NoSpace) => break,
			Err(other) => panic!("unexpected {other:?}"),
		}
		assert!(written < 10_000, "the volume never filled");
	}
	assert!(written > 0, "the fixture must hold something");
	// Removing one file frees its blocks; a metadata allocation must then succeed.
	fs.remove(b"f00000").expect("remove");
	fs.write_file(b"after", b"x").expect("a freed block must be usable again, by either allocator");
	assert_eq!(fs.read_file(b"after").expect("read"), b"x");
}

#[test]
fn a_memory_shortage_is_not_a_full_disk() {
	// `NoSpace` means the medium is full and `NoMemory` means this machine is short, and they drive
	// opposite policies: delete something, versus wait for the service. Inside the filesystem every
	// fallible allocation answered `NoSpace`, so a caller under memory pressure was told to free
	// disk space that was already free.
	//
	// `try_zeroed` is the helper every derived map goes through, and the size here is a legal
	// superblock claim whose bitmap this machine cannot hold - the medium is not involved at all.
	assert_eq!(try_zeroed((MAX_BLOCKS / 16) as usize).err(), Some(FsError::NoMemory));
	assert!(try_zeroed(1024).is_ok(), "an ordinary size still succeeds, so the guard is not simply refusing");

	// And a genuinely full volume still says so, which is the half that must not move.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let mut written = 0u32;
	let full = loop {
		let name = alloc::format!("f{written:05}");
		match fs.write_file(name.as_bytes(), &[0xAA; 4096]) {
			Ok(()) => written += 1,
			Err(error) => break error,
		}
		assert!(written < 10_000, "the volume never filled");
	};
	assert_eq!(full, FsError::NoSpace, "a full medium is still a full medium");
}

#[test]
fn a_transaction_that_changed_nothing_writes_nothing() {
	// `mutate` committed whenever the body returned `Ok`, so a rename onto its own name - which
	// `rename_inner` short-circuits - wrote a superblock, advanced the generation and rolled the
	// previous one into a snapshot. Nothing incorrect; a write, wear, and a generation step a
	// caller can repeat indefinitely, on a volume that did not change.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"foo", b"content").unwrap();
	// The superblock slots are the medium's record of how many times it has been committed to:
	// a commit writes the inactive one, so the pair changes on every real transaction.
	let before = fs.into_device().blocks.clone();
	let mut fs = LiberFs::mount(MemDevice { blocks: before.clone() }).expect("remount");

	fs.rename(b"foo", b"foo").expect("a rename onto itself succeeds");
	let after_noop = fs.into_device().blocks.clone();
	assert_eq!(after_noop, before, "a no-op wrote to the medium");

	// And a real change still commits, so the shortcut is not simply refusing to write.
	let mut fs = LiberFs::mount(MemDevice { blocks: after_noop.clone() }).expect("remount");
	fs.rename(b"foo", b"bar").expect("rename");
	let after_real = fs.into_device().blocks.clone();
	assert_ne!(after_real, after_noop, "a real change must still commit");
	let mut fs = LiberFs::mount(MemDevice { blocks: after_real }).expect("remount");
	assert_eq!(fs.read_file(b"bar").expect("read"), b"content");
}

#[test]
fn a_path_is_bounded_as_a_whole_and_not_only_per_segment() {
	// Each segment was limited to 255 bytes and the path was not, and the segments were collected
	// with an infallible `Vec::push` - so a caller handing the crate a large buffer of short
	// segments got an allocation proportional to it, and a shortage there aborts the process rather
	// than refusing. StorageService bounds paths from outside; a filesystem crate that relies on
	// its caller for that is safe until somebody else calls it.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	let long: Vec<u8> = b"a/".repeat(PATH_MAX);
	assert_eq!(fs.read_file(&long), Err(FsError::TooLong), "a path longer than any this filesystem holds");
	assert_eq!(fs.write_file(&long, b"x"), Err(FsError::TooLong));

	// Depth, separately: a short path can still be deeper than the walk will go.
	let deep: Vec<u8> = b"a/".repeat(PATH_DEPTH_MAX + 1);
	assert!(deep.len() < PATH_MAX, "the fixture must test depth rather than length");
	assert_eq!(fs.read_file(&deep), Err(FsError::TooLong), "a path deeper than the limit");

	// And an ordinary path still resolves, so the bound is not simply refusing.
	fs.write_file(b"a/b/c", b"ok").expect("an ordinary path");
	assert_eq!(fs.read_file(b"a/b/c").expect("read"), b"ok");
}

#[test]
fn an_unordered_live_inode_leaf_takes_the_mount_read_only() {
	// `mark_inode_tree` checked the CRC, the node type and the routing interval, and its own
	// comment explained that `validate_fixed_leaf` and `validate_internal` are "local" - which read
	// as though the local checks were happening elsewhere. `fsck` runs them and `tree_insert_node`
	// runs them before mutating; the mount did not.
	//
	// A CRC-valid leaf whose keys are out of order is inside its routing interval, so it passed.
	// `tree_lookup` then binary-searches it and answers `None` for a key that is present, which
	// surfaces as `Invalid` - and `remove_inner` treats exactly that as a dangling directory entry,
	// deliberately, as the operator's repair verb. So `remove` drops the only name of a live inode
	// whose blocks are still allocated, and commits it.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	for i in 0..6u32 {
		fs.write_file(alloc::format!("f{i}").as_bytes(), b"x").unwrap();
	}
	let root = fs.inode_root;
	let mut dev = fs.into_device();

	// Swap two whole records in the live inode leaf, so nothing changes but the ORDER, and forge
	// the CRC the superblock records for it - a hostile author checksums what they write.
	let start = root as usize * BLOCK_SIZE;
	let count = u16::from_le_bytes(dev.blocks[start + 2..start + 4].try_into().unwrap()) as usize;
	assert!(count >= 3, "the fixture must hold enough records to disorder: {count}");
	// Two records in the MIDDLE, so the leaf's lowest and highest keys are exactly what they were.
	// Swapping the ends moves the min and max, which the routing interval and the max-inode
	// bookkeeping already notice - and then the test would be measuring those instead of this.
	let first = start + NODE_HDR + INODE_REC;
	let last = start + NODE_HDR + 2 * INODE_REC;
	let mut a = dev.blocks[first..first + INODE_REC].to_vec();
	let mut b = dev.blocks[last..last + INODE_REC].to_vec();
	assert_ne!(a[..8], b[..8], "the swap has to actually disorder the leaf");
	core::mem::swap(&mut a, &mut b);
	dev.blocks[first..first + INODE_REC].copy_from_slice(&a);
	dev.blocks[last..last + INODE_REC].copy_from_slice(&b);
	let crc = crc32c(&dev.blocks[start..start + BLOCK_SIZE]);
	let slot = active_slot(&dev);
	forge_superblock(&mut dev, slot, |sb| sb[SB_INODE_ROOT_CRC_OFF..SB_INODE_ROOT_CRC_OFF + 4].copy_from_slice(&crc.to_le_bytes()));

	// It still mounts - refusing outright would take the rescue away - and it mounts READ-ONLY,
	// which is what stops the repair verb from running on a volume it would destroy.
	let mut fs = LiberFs::mount(dev).expect("a damaged volume still mounts for reading");
	assert!(fs.is_read_only(), "a structurally impossible live inode tree must take the mount read-only");
	assert_eq!(fs.remove(b"f0"), Err(FsError::ReadOnly), "and nothing may commit on top of it");
	assert_eq!(fs.write_file(b"f9", b"x"), Err(FsError::ReadOnly));
}

#[test]
fn an_extent_map_that_ends_before_its_declared_count_is_corrupt() {
	// `load_spill` validated every chain block - the count, the CRC, the bounds, the walk length -
	// and never the TOTAL. A declared seven extents with four inline and a null spill pointer
	// satisfied all of them and returned `Ok`, and `fsck` had both the check and the message for it.
	//
	// The rewrite is what makes it destructive rather than merely wrong: `flush_extents` sets
	// `extent_count = extents.len()`, so editing such an inode replaces an incomplete map with a
	// self-consistent one and the missing extents become holes - permanently, in a generation that
	// checksums perfectly and that nothing will ever question again.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f", &[0xAB; 4096]).unwrap();
	let ino = fs.resolve(b"f").unwrap();
	let mut inode = fs.read_inode(ino).unwrap();
	assert!(inode.extents.len() <= EXTENTS_INLINE, "the fixture's map must start inline");

	// Declare more extents than the inode carries, with no chain to hold the rest. Written through
	// the inode slot so the tree and its checksums stay consistent - the medium is self-consistent
	// and only the map is a lie, which is the case that used to pass.
	inode.extent_count = (EXTENTS_INLINE + 3) as u32;
	inode.spill = 0;
	fs.write_inode_slot_for_test(ino, &inode).unwrap();
	fs.commit().unwrap();
	let dev = fs.into_device();

	let mut fs = LiberFs::mount(dev).expect("the volume still mounts");
	assert_eq!(fs.read_file(b"f"), Err(FsError::Corrupt), "an extent map that ends before its own count");
}

#[test]
fn portability_is_asked_about_rather_than_enforced() {
	// The validator's comment claimed its byte set made names "move cleanly onto FAT and NTFS", and
	// it did not: reserved device names resolve to hardware there, and a trailing dot or space is
	// stripped - so two distinct names here become one there and the second write destroys the
	// first. `fscore::is_portable_name` is the separate question; the filesystem is not tightened
	// for it, because refusing names a medium legitimately carries is worse than accepting them.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	for hostile in [&b"CON"[..], b"con", b"NUL.txt", b"aux", b"COM1", b"lpt9", b"trailing.", b"trailing "] {
		assert!(!fscore::is_portable_name(hostile), "{hostile:?} is not portable");
		// ...and the filesystem still stores it, because it can address it.
		if fs.write_file(hostile, b"x").is_ok() {
			assert_eq!(fs.read_file(hostile).expect("readable"), b"x");
		}
	}
	for fine in [&b"CONFIG"[..], b"com", b"COM10", b"notes.txt", "ěščř.txt".as_bytes(), b"a b c"] {
		assert!(fscore::is_portable_name(fine), "{fine:?} is portable");
	}
	// And what the filesystem itself refuses is refused by both.
	for illegal in [&b"a:b"[..], b"a*b", b"."[..].as_ref(), b".."] {
		assert!(!fscore::is_portable_name(illegal), "{illegal:?}");
		assert!(fs.write_file(illegal, b"x").is_err());
	}
}

#[test]
fn the_superblock_parser_is_as_strict_as_the_writer() {
	// Two asymmetries in a filesystem that has made writer/parser symmetry a principle. Neither is
	// dangerous today, which is exactly why the principle is worth keeping: the asymmetries that
	// matter are indistinguishable from the ones that do not until somebody writes an image by hand.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	// A DIRECTORY, so the nominated root below is a real one: with a file there the "root must be a
	// directory" check fires instead and the test would be measuring that.
	fs.write_file(b"d/f", b"x").unwrap();
	let dir = fs.resolve(b"d").unwrap();
	let good = fs.into_device();
	assert!(!LiberFs::mount(good.clone()).unwrap().is_read_only(), "the untouched image must mount writable");

	// A root that is not inode 0, and IS a directory - so the volume would mount over a subtree,
	// with the rest of the inode tree present, checksummed and unreachable. It mounts READ-ONLY
	// rather than being refused: refusing the superblock would make the mount fall back to the
	// previous generation and mount that writable, discarding the newest instead of reporting it.
	let mut dev = good.clone();
	let slot = newest_super_slot(&dev) as usize;
	forge_superblock(&mut dev, slot, |sb| sb[SB_ROOT_INODE_OFF..SB_ROOT_INODE_OFF + 4].copy_from_slice(&dir.to_le_bytes()));
	assert!(LiberFs::mount(dev).unwrap().is_read_only(), "a namespace root that is not inode 0");

	// A compression byte the writer can never produce. This one IS refused at parse - the slot is
	// unreadable rather than the volume wrong, so the other slot is the answer - and what must not
	// happen is `!= 0` reading 2 or 255 as "compression on", which would make every later write
	// take a path the volume never asked for.
	for byte in [2u8, 255] {
		let mut dev = good.clone();
		let slot = newest_super_slot(&dev) as usize;
		forge_superblock(&mut dev, slot, |sb| sb[SB_COMPRESS_OFF] = byte);
		let fs = LiberFs::mount(dev).expect("the other slot still carries a volume");
		assert!(!fs.compression(), "a compression byte of {byte} must not be read as true");
	}
}

#[test]
fn fsck_reports_an_extent_mapped_past_the_end_of_its_file() {
	// Structure, ordering and overlap were all checked and no extent was ever compared with the
	// file's size. A 4096-byte file could carry a run mapped at logical block 1000: allocated,
	// invisible in the file's contents, and reserved for as long as the volume lives. No writer
	// this filesystem has produces one - which is exactly why nothing noticed.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"f.bin", &noise(BLOCK_SIZE)).unwrap();
	let (b0, cb) = (10u64, 11u64);
	for b in [b0, cb] {
		assert!(!fs.is_alloc(b), "block {b} must be free for this forgery to mean anything");
	}
	let mut dev = fs.into_device();

	let mut cbuf = vec![0u8; BLOCK_SIZE];
	let at = b0 as usize * BLOCK_SIZE;
	cbuf[0..4].copy_from_slice(&crc32c(&dev.blocks[at..at + BLOCK_SIZE]).to_le_bytes());
	let cbuf_crc = crc32c(&cbuf);
	dev.blocks[cb as usize * BLOCK_SIZE..(cb as usize + 1) * BLOCK_SIZE].copy_from_slice(&cbuf);
	forge_inode_slot(&mut dev, |slot| {
		// one block of contents, and a run mapped a thousand blocks beyond it.
		slot[INO_SIZE_OFF..INO_SIZE_OFF + 8].copy_from_slice(&(BLOCK_SIZE as u64).to_le_bytes());
		slot[INO_EXTENT_COUNT_OFF..INO_EXTENT_COUNT_OFF + 4].copy_from_slice(&1u32.to_le_bytes());
		let past = Extent { logical: 1000, physical: b0, length: 1, csum: cb, csum_crc: cbuf_crc, store_len: 1, clen: 0 };
		past.write(&mut slot[EXTENT_OFF..EXTENT_OFF + EXTENT_SIZE]);
	});

	let mut fs = LiberFs::mount(dev).unwrap();
	let report = fs.fsck().unwrap();
	assert!(mentions(&report.faults, b"past the end of the file"), "the mapping past EOF must be named: {:?}", report.faults);
}

#[test]
fn fsck_runs_the_decompressor_and_counts_a_bad_stream_apart() {
	// Every stored block matching its CRC says the medium gave back what was written. It says
	// nothing about whether what was written decodes, and `fsck` ran no decoder at all - so a
	// volume with a syntactically invalid LZ stream reported zero failures and answered `Corrupt`
	// to the first read of the file. The three kinds of failure are counted apart because they send
	// an operator to three different places: the medium, the metadata, or the writer.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.set_compression(true).unwrap();
	// Compressible enough to be stored compressed: the check only runs on `clen != 0`.
	fs.write_file(b"c.bin", &vec![0x5Au8; 8 * BLOCK_SIZE]).unwrap();
	assert_eq!(fs.fsck().unwrap().stream_failures, 0, "a healthy compressed file decodes");
	let stored = fs.read_file(b"c.bin").unwrap();
	assert_eq!(stored.len(), 8 * BLOCK_SIZE, "and reads back whole");

	// Damage the STREAM, then re-stamp the checksums over the damage - which is what makes this
	// invisible to every check that existed. A truncated length byte in the middle of the stream
	// leaves the grammar unsatisfiable.
	let mut dev = fs.into_device();
	let (block, csum) = first_extent_of(&dev);
	let at = block as usize * BLOCK_SIZE;
	dev.blocks[at + 4..at + 16].copy_from_slice(&[0xFFu8; 12]);
	let fresh = crc32c(&dev.blocks[at..at + BLOCK_SIZE]);
	let cat = csum as usize * BLOCK_SIZE;
	dev.blocks[cat..cat + 4].copy_from_slice(&fresh.to_le_bytes());
	// And the checksum block's own CRC, which lives in the extent record - so nothing anywhere
	// reports a checksum failure and the only thing wrong with this volume is the stream.
	let fresh_csum_crc = crc32c(&dev.blocks[cat..cat + BLOCK_SIZE]);
	forge_inode_slot(&mut dev, |slot| slot[EXTENT_OFF + 20..EXTENT_OFF + 24].copy_from_slice(&fresh_csum_crc.to_le_bytes()));

	let mut fs = LiberFs::mount(dev).unwrap();
	assert_eq!(fs.read_file(b"c.bin"), Err(FsError::Corrupt), "the file is unreadable, whatever fsck says");
	let report = fs.fsck().unwrap();
	assert_eq!(report.stream_failures, 1, "the stream failure is counted as one: {:?}", report.faults);
	assert_eq!(report.checksum_failures, 0, "and not as a failing medium, because the medium is fine");
	assert!(mentions(&report.faults, b"stream does not decode"), "named for what it is: {:?}", report.faults);
	assert_eq!(report.structural_failures, 0, "the metadata is intact; only the stream is not");
}

#[test]
fn a_compressed_extent_with_a_bad_checksum_is_counted_once() {
	// The mirror of the test above, and the one that was missing. `structural_failures` was
	// `faults.len() - stream_failures`, so a compressed extent whose blocks fail their checksums was
	// counted TWICE - once by the scrub pass into `checksum_failures`, and once again as structural
	// because it was in `faults` and was not a stream failure. The three counters exist because they
	// send an operator to three different places; two of them naming one fault defeats that.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.set_compression(true);
	fs.write_file(b"c.bin", &[0x41u8; 4 * BLOCK_SIZE]).unwrap();
	assert_eq!(fs.fsck().unwrap().checksum_failures, 0, "a healthy compressed file is clean");

	// Damage a stored block and leave the checksum alone: the medium disagrees with what was
	// written, which is precisely one fault of precisely one kind.
	let mut dev = fs.into_device();
	let (block, _) = first_extent_of(&dev);
	let at = block as usize * BLOCK_SIZE;
	dev.blocks[at + 4..at + 16].copy_from_slice(&[0xFFu8; 12]);

	let mut fs = LiberFs::mount(dev).unwrap();
	let report = fs.fsck().unwrap();
	assert!(report.checksum_failures >= 1, "the scrub pass sees the medium disagreeing: {:?}", report.faults);
	assert_eq!(report.stream_failures, 0, "and it is not a stream failure");
	assert_eq!(report.structural_failures, 0, "nor a structural one - the metadata is untouched");
}

#[test]
fn two_superblock_slots_from_unrelated_volumes_do_not_mount_as_a_pair() {
	// Each slot validated alone and nothing ever compared them, so two checksum-valid slots from
	// unrelated states mounted as current + previous. `derive_free` then reads the previous root
	// under the CURRENT slot's geometry, and that rolling snapshot is part of what keeps the
	// allocator honest - so the blocks one volume's older generation holds are read as if they
	// belonged to another volume's.
	//
	// Four fields make two slots one volume: the uuid, the geometry, the namespace root, and
	// consecutive generations, because a commit writes the other slot with generation + 1 and
	// nothing else.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"a.bin", b"one").unwrap();
	fs.write_file(b"b.bin", b"two").unwrap();
	let dev = fs.into_device();
	let sound = LiberFs::mount(dev.clone()).unwrap();
	assert!(!sound.is_read_only(), "the volume is writable before the slots are made to disagree");
	drop(sound);

	// One field at a time, on the slot that is NOT current, so what changes is the pairing and
	// nothing else.
	let breaks = [
		("a different volume's uuid", SB_UUID_OFF, alloc::vec![0xAAu8; 16]),
		("a different geometry", SB_NUM_BLOCKS_OFF, (NBLOCKS - 1).to_le_bytes().to_vec()),
		// A live inode number, because a root above `next_inode` is refused at parse and the
		// slot would simply be invalid - which is a different thing being tested.
		("a different namespace root", SB_ROOT_INODE_OFF, 1u32.to_le_bytes().to_vec()),
		("generations that are not consecutive", SB_GENERATION_OFF, 0u64.to_le_bytes().to_vec()),
	];
	for (what, off, bytes) in breaks {
		let mut dev = dev.clone();
		let other = 1 - active_slot(&dev);
		forge_superblock(&mut dev, other, |sb| sb[off..off + bytes.len()].copy_from_slice(&bytes));
		let mut fs = LiberFs::mount(dev.clone()).expect("the newer slot still describes a mountable volume");
		assert!(fs.is_read_only(), "{what}: a volume whose two slots disagree must not be written to");
		assert_eq!(fs.read_file(b"a.bin").unwrap(), b"one", "{what}: and it still reads");
		// The older slot is not a snapshot of this volume, so there is no snapshot to serve.
		// A FAULT, not an absence: the older slot exists and is not this volume's snapshot, which is
		// exactly the difference `Result<Option<..>>` was introduced to carry.
		assert_eq!(LiberFs::mount_snapshot(dev).err(), Some(MountError::Corrupt), "{what}: the previous generation is not this volume's");
	}
}

#[test]
fn a_directory_count_that_disagrees_with_its_tree_is_repaired_not_ground_down() {
	// `dir.size` is a cache of the tree, and `remove` did `saturating_sub(1)` on it. A directory
	// whose stored count is 0 while its tree holds three entries lost a real entry and kept the
	// count at 0 - and the inode was re-checksummed and committed, so the removal made the lie
	// permanent and internally consistent. Refusing instead would close the only repair route there
	// is, because removing the children is how such a directory gets fixed.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.mkdir(b"d").unwrap();
	for name in [b"d/a" as &[u8], b"d/b", b"d/c"] {
		fs.write_file(name, b"x").unwrap();
	}
	let dir = fs.resolve(b"d").unwrap();
	fs.mutate(|fs| {
		let mut inode = fs.read_inode(dir)?;
		inode.size = 0;
		fs.write_inode(dir, &mut inode)
	})
	.unwrap();

	// One removal, and the count is what the tree says rather than 0 again.
	fs.remove(b"d/a").unwrap();
	assert_eq!(fs.read_inode(dir).unwrap().size, 2, "the count is derived from the tree that remains");
	assert_eq!(fs.read_dir(b"d").unwrap().len(), 2, "and it agrees with the listing");
	// And from there it behaves like any other directory: it empties, and then it can go.
	fs.remove(b"d/b").unwrap();
	fs.remove(b"d/c").unwrap();
	assert_eq!(fs.read_inode(dir).unwrap().size, 0);
	fs.remove(b"d").unwrap();
	assert_eq!(fs.read_dir(b"d").err(), Some(FsError::NotFound), "the repaired directory removes like any other");
}

#[test]
fn a_compression_attempt_that_runs_out_of_space_keeps_the_raw_file() {
	// Compression is documented as an optimisation: a run that cannot be stored compressed stays
	// raw. `compress_inode` propagated `NoSpace` through `write_file_inner`'s `?`, so a file that
	// FITS RAW - already written, in this very transaction - was refused and the whole transaction
	// rolled back, because the optional step could not get temporary blocks for a smaller copy of
	// what was already on the disk.
	//
	// The discriminator is the same volume with compression off: whatever the free map's exact
	// layout, a file that fits with compression disabled must also fit with it enabled. Nothing
	// about a margin has to be guessed, and the interesting case is the one where the two answers
	// used to differ.
	let nblocks: u64 = 48;
	let filler = noise(BLOCK_SIZE);

	let attempt = |files: u32, payload: &[u8], compress: bool| -> Result<(), FsError> {
		let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
		fs.set_compression(compress).unwrap();
		for i in 0..files {
			if fs.write_file(alloc::format!("fill{i}").as_bytes(), &filler).is_err() {
				break;
			}
		}
		fs.write_file(b"squeeze.bin", payload)?;
		assert_eq!(fs.read_file(b"squeeze.bin").unwrap(), payload, "a file that was accepted reads back whole");
		assert_eq!(fs.fsck().unwrap().structural_failures, 0, "and leaves the volume sound");
		Ok(())
	};

	// Both dimensions, because the window is narrow: a compression attempt needs its stored blocks
	// and a checksum block ON TOP of the raw run it is replacing, while a one-block filler consumes
	// two blocks (data + checksum). Sweeping the payload size as well as the fill level walks the
	// boundary rather than stepping over it.
	let mut saw_raw_fit = false;
	for blocks in 2..=6 {
		let payload = alloc::vec![0x77u8; blocks * BLOCK_SIZE];
		for files in 0..24 {
			let raw = attempt(files, &payload, false).is_ok();
			let compressed = attempt(files, &payload, true).is_ok();
			saw_raw_fit |= raw;
			assert!(!raw || compressed, "{blocks} block(s) after {files} file(s) of filler fit raw, so enabling compression must not refuse them");
		}
	}
	assert!(saw_raw_fit, "the volume took the payload at some fill level, or this test measured nothing");
	// HONEST LIMIT: this sweep does not diverge under the OLD behaviour either. A write that
	// succeeds leaves at least the slack its own commit needed for metadata, which is more than the
	// two blocks a compression attempt wants - so on this allocator the propagated `NoSpace` was
	// unreachable rather than merely rare. The fix is by construction and this pins the property
	// against the allocator changing shape, which is the point at which it would become reachable.
}

#[test]
fn a_directory_can_be_read_a_page_at_a_time() {
	// `read_dir` returns every entry in one `Vec`, with an inode read each, out of a tree built to
	// hold millions. A cursor is what the tree was already shaped for: pages in the tree's own
	// order, with the subtrees before the cursor never read.
	let nblocks: u64 = 4096;
	let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
	fs.mkdir(b"d").unwrap();
	// Long names on purpose: a directory leaf holds bytes, not entries, so two hundred short names
	// fit in ONE leaf and a tree with no internal node cannot show whether anything is pruned.
	let name_of = |i: u32| alloc::format!("d/f{i:03}{}", "n".repeat(200));
	for i in 0..200u32 {
		fs.write_file(name_of(i).as_bytes(), b"x").unwrap();
	}
	let whole = fs.read_dir(b"d").unwrap();
	assert_eq!(whole.len(), 200);

	// Paged, seven at a time, and the pages concatenate to exactly what one call returns - same
	// rows, same order.
	let mut paged: Vec<(Vec<u8>, u64, bool, u64, u64)> = Vec::new();
	let mut cursor: Option<Vec<u8>> = None;
	loop {
		let page = fs.read_dir_page(b"d", cursor.as_deref(), 7).unwrap();
		if page.is_empty() {
			break;
		}
		assert!(page.len() <= 7, "a page never exceeds its limit");
		cursor = Some(page.last().unwrap().0.clone());
		paged.extend(page);
		assert!(paged.len() <= 200, "the cursor advances, or this loop would not end");
	}
	assert_eq!(paged, whole, "the pages are the whole listing, in the same order");

	// And the work is a page's work. Counting block reads is the only honest way to say that: a
	// page from the far end of the directory must not read the tree that comes before it.
	struct Counting {
		inner: MemDevice,
		// A `Cell`, so the count can be read through the filesystem's shared `device()` borrow.
		reads: core::cell::Cell<u64>,
	}
	impl BlockDevice for Counting {
		fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
			self.reads.set(self.reads.get() + 1);
			self.inner.read_block(index, buf)
		}
		fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
			self.inner.write_block(index, buf)
		}
		fn flush(&mut self) -> bool {
			self.inner.flush()
		}
	}
	let mut counted = LiberFs::mount(Counting { inner: fs.into_device(), reads: core::cell::Cell::new(0) }).unwrap();
	let reads_for = |fs: &mut LiberFs<Counting>, after: Option<&[u8]>| -> u64 {
		fs.device().reads.set(0);
		let page = fs.read_dir_page(b"d", after, 7).unwrap();
		assert_eq!(page.len(), 7, "both pages are full pages, so the counts compare like with like");
		fs.device().reads.get()
	};
	let first_page = reads_for(&mut counted, None);
	let late_page = reads_for(&mut counted, Some(&paged[paged.len() - 8].0));
	// Each page reads seven inodes either way; what differs is the TREE walk. Without the cursor
	// pruning the subtrees before it, a page from the far end walks every leaf that comes before it
	// first - so this is the assertion that says the cursor does what it is for.
	// A late page costs no more than the first. Without the pruning it costs half again as much on
	// this directory (31 blocks against 19), because it walks every leaf before the cursor first.
	assert!(late_page <= first_page, "a late page read {late_page} blocks against {first_page} for the first: the walk is not being pruned");

	// The degenerate limits: nothing asked for, nothing returned; and a cursor past the end ends it.
	assert!(counted.read_dir_page(b"d", None, 0).unwrap().is_empty());
	assert!(counted.read_dir_page(b"d", Some(&paged.last().unwrap().0), 7).unwrap().is_empty());
	// A file is not a directory, whichever way it is read.
	assert_eq!(counted.read_dir_page(name_of(0).as_bytes(), None, 7).err(), Some(FsError::NotDir));
}

#[test]
fn a_directory_record_naming_the_root_is_an_alias_too() {
	// The alias map is set on an inode's FIRST sighting and reports on its second, which is right
	// for every inode that has a name. The root has none - nothing may name it - so its first
	// sighting is already the alias, and over a zeroed bitmap a record pointing at inode 0 merely
	// set bit 0 and the walk carried on. A volume with a namespace loop through the root mounted
	// writable, while `fsck` - whose `reached` set contains the root before the walk starts - called
	// the same image damaged. That disagreement is what this milestone is named after.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"a.txt", b"shared").unwrap();
	let root = fs.root_inode;
	let mut inode = fs.read_inode(root).unwrap();
	let mut buf = vec![0u8; BLOCK_SIZE];
	fs.read_node(inode.dir_root, inode.dir_root_crc, &mut buf).unwrap();
	let recs = dir_leaf_parse(&buf);
	assert_eq!(recs.len(), 1);
	// The one record now names the root itself: a loop, perfectly formed and checksummed.
	let looped: Vec<DirRec> = alloc::vec![DirRec { hash: recs[0].hash, name: recs[0].name.clone(), child: root }];
	dir_leaf_write(&mut buf, &looped);
	let crc = fs.write_node_to(inode.dir_root, &buf).unwrap();
	inode.dir_root_crc = crc;
	fs.write_inode(root, &mut inode).unwrap();
	fs.commit().unwrap();

	let mut remounted = LiberFs::mount(fs.into_device()).unwrap();
	assert!(remounted.is_read_only(), "a name resolving to the root is an alias of an inode that may have none");
	assert_eq!(remounted.write_file(b"b.txt", b"x"), Err(FsError::ReadOnly), "and nothing may be written over it");
}

#[test]
fn one_inode_with_two_names_is_refused_by_the_mount_and_named_by_fsck() {
	// There is no hardlink API and no link count, so the format's rule is one inode, one name - and
	// nothing said so. `fsck`'s namespace walk did `if !reached.insert(child) { continue; }`, which
	// is the cycle defence doing double duty: a second reference to an already-reached inode was
	// skipped rather than reported. So an image with `/a` and `/b` both naming inode 7 passed fsck
	// clean, and `remove("a")` freed inode 7's blocks and deleted it from the inode tree while `/b`
	// still pointed at it - a live name resolving to a record that is gone, and blocks the
	// allocator will hand to something else.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	fs.write_file(b"a.txt", b"shared").unwrap();
	fs.write_file(b"b.txt", b"other").unwrap();
	let root = fs.root_inode;
	let mut inode = fs.read_inode(root).unwrap();
	let mut buf = vec![0u8; BLOCK_SIZE];
	fs.read_node(inode.dir_root, inode.dir_root_crc, &mut buf).unwrap();
	// Point the second record at the first one's inode: both names, one inode, everything else
	// perfectly formed and checksummed.
	let recs = dir_leaf_parse(&buf);
	assert_eq!(recs.len(), 2, "the root directory is one leaf holding both names");
	let shared = recs[0].child;
	let aliased: Vec<DirRec> = alloc::vec![DirRec { hash: recs[0].hash, name: recs[0].name.clone(), child: shared }, DirRec { hash: recs[1].hash, name: recs[1].name.clone(), child: shared }];
	dir_leaf_write(&mut buf, &aliased);
	let crc = fs.write_node_to(inode.dir_root, &buf).unwrap();
	inode.dir_root_crc = crc;
	fs.write_inode(root, &mut inode).unwrap();
	fs.commit().unwrap();

	let report = fs.fsck().unwrap();
	assert!(mentions(&report.faults, b"named more than once"), "fsck must name the alias: {:?}", report.faults);

	// And a fresh mount refuses to write to it. The repair for an alias is to remove one of the two
	// names, and that removal is exactly what destroys the shared inode - so read-only is the
	// answer, with both names still readable and `fsck` naming them.
	let mut remounted = LiberFs::mount(fs.into_device()).unwrap();
	assert!(remounted.is_read_only(), "a volume with one inode under two names must not be written to");
	// Both names resolve to the one inode, whichever of the two the leaf's (hash, name) order put
	// first - which is the damage, visible.
	let (under_a, under_b) = (remounted.read_file(b"a.txt").unwrap(), remounted.read_file(b"b.txt").unwrap());
	assert_eq!(under_a, under_b, "two names, one inode, one set of bytes");
	assert!(under_a == b"shared" || under_a == b"other", "and they are one of the two files that were written");
	assert_eq!(remounted.remove(b"a.txt"), Err(FsError::ReadOnly), "and the removal that would free the shared inode is refused");
}

#[test]
fn a_refused_allocation_inside_commit_does_not_leave_the_operation_to_be_published_later() {
	// THE SHAPE `finish()` CANNOT REPAIR. It calls `abort()` when the transaction BODY fails; a
	// body that succeeded goes straight to `commit()`, and `commit()`'s error is returned unchanged.
	// So the one `?` inside `commit()` before the point of no return - building the next
	// generation's dead list - returned `NoMemory` with the transaction still open: `self.txn`
	// holding the rollback snapshot, `inode_root` pointing at the new uncommitted root, and `fresh`
	// and `dead` holding the failed operation's blocks.
	//
	// The visible consequence is not the refusal. It is what the NEXT mutation does: `begin()`
	// overwrites the rollback snapshot and clears both sets, and the commit after it publishes the
	// root the refused operation left behind - an operation whose caller was told it failed,
	// committed, under an unrelated write's generation.
	//
	// That is what this checks, and it is why the second write is here. A test that only asserted
	// "the refused write is not on the volume" passes against the defect, because at that moment it
	// is not: it is in memory, waiting for somebody else's commit to carry it.
	//
	// A SWEEP, because which allocation is the one inside `commit` depends on everything the write
	// did first, and a pinned budget stops testing the moment that changes.
	let nblocks: u64 = 512;
	let mut published = 0usize;
	let mut refusals = 0usize;
	for budget in 0..96 {
		let mut fs = LiberFs::format_scratch(MemDevice::new(nblocks), nblocks).unwrap();
		fs.write_file(b"keep", b"original").unwrap();

		let refused = {
			let _armed = fail_one_allocation_after(budget);
			fs.write_file(b"ghost", b"should not exist")
		};
		let Err(error) = refused else { continue };
		assert!(matches!(error, FsError::NoMemory | FsError::NoSpace), "budget {budget}: a starved allocator answers NoMemory or NoSpace, not {error:?}");
		refusals += 1;

		// An UNRELATED, successful mutation. This is the commit that used to carry the refused
		// operation's root onto the medium.
		if fs.write_file(b"after", b"unrelated").is_err() {
			// The volume may have gone read-only (a `CommitUncertain` path) or be genuinely out of
			// room; either way there is no later commit to publish anything and this budget has
			// nothing to say.
			continue;
		}

		// Re-mounted from the device, so the question is about the MEDIUM rather than about a cache.
		let mut remounted = LiberFs::mount(fs.into_device()).expect("the volume still mounts");
		assert_eq!(remounted.read_file(b"keep").as_deref(), Ok(&b"original"[..]), "budget {budget}: the file written before the refusal survives");
		assert_eq!(remounted.read_file(b"after").as_deref(), Ok(&b"unrelated"[..]), "budget {budget}: the unrelated write that followed is on the volume");
		if remounted.read_file(b"ghost").is_ok() {
			published += 1;
		}
	}
	assert!(refusals > 0, "no allocation budget produced a refused write, so this swept nothing");
	assert_eq!(published, 0, "{published} of {refusals} refused write(s) were published by the unrelated commit that followed - the caller was told NoMemory and the operation landed anyway");
}

#[test]
fn the_snapshot_checker_refuses_a_block_that_is_not_a_node() {
	// `read_node` checks the pointer bounds, the device read and the CRC, and NOTHING about what
	// the block is. `check_inode_tree` then tested `node_type(&buf) == NODE_LEAF` and treated every
	// other value as an internal node - so a corrupted or forged type byte turned a leaf into an
	// internal node and its inode records into child pointers, inside the checker whose job is to
	// notice exactly that. It then descended into whatever those bytes named.
	//
	// The raw marking walk in `derive_free` has required the byte to be one of the two values that
	// exist for some time; this is the same rule in the pass that reports, and the point of the test
	// is that the two checkers agree by construction rather than by coincidence.
	//
	// DRIVEN DIRECTLY rather than through `fsck()`. `derive_free` visits the snapshot trees too and
	// would flag the same block, so a whole-report assertion cannot tell which pass noticed - it
	// would be green with this checker still walking garbage. Calling it is what isolates it.
	let mut fs = LiberFs::format_scratch(MemDevice::new(NBLOCKS), NBLOCKS).unwrap();
	for i in 0..8u32 {
		fs.write_file(format!("f{i}").as_bytes(), b"payload").unwrap();
	}
	fs.create_snapshot(b"s1").unwrap();
	let mut dev = fs.into_device();

	// The snapshot's inode-tree root, with its type byte set to a value that is neither node type
	// and every checksum above it re-stamped, so the medium looks perfect and the only thing wrong
	// is that the block claims to be something that does not exist.
	let slot = active_slot(&dev);
	let sb = parse_superblock(&dev.blocks[slot * BLOCK_SIZE..(slot + 1) * BLOCK_SIZE]).unwrap();
	let snap_start = sb.snap_root as usize * BLOCK_SIZE;
	let rec = snap_start + SNAP_HDR;
	let root = u64::from_le_bytes(dev.blocks[rec + SNAP_ROOT_OFF..rec + SNAP_ROOT_OFF + 8].try_into().unwrap());
	let node = root as usize * BLOCK_SIZE;
	assert_eq!(dev.blocks[node], NODE_LEAF, "the fixture's snapshot root must be a leaf, or this forges the wrong thing");
	dev.blocks[node] = 7;
	let node_crc = crc32c(&dev.blocks[node..node + BLOCK_SIZE]);
	dev.blocks[rec + SNAP_ROOT_CRC_OFF..rec + SNAP_ROOT_CRC_OFF + 4].copy_from_slice(&node_crc.to_le_bytes());
	let snap_crc = crc32c(&dev.blocks[snap_start..snap_start + BLOCK_SIZE]);
	for s in 0..SUPER_SLOTS as usize {
		if parse_superblock(&dev.blocks[s * BLOCK_SIZE..(s + 1) * BLOCK_SIZE]).is_some_and(|sb| sb.snap_root as usize * BLOCK_SIZE == snap_start) {
			forge_superblock(&mut dev, s, |sb| sb[SB_SNAP_ROOT_CRC_OFF..SB_SNAP_ROOT_CRC_OFF + 4].copy_from_slice(&snap_crc.to_le_bytes()));
		}
	}

	let mut fs = LiberFs::mount(dev).expect("every checksum still matches, so it mounts");
	let mut visited = try_zeroed(fs.num_blocks.div_ceil(8) as usize).unwrap();
	let mut tally = crate::fsck::StructureTally::default();
	let bad = fs.check_inode_tree(root, node_crc, TREE_DEPTH_MAX, &mut visited, &mut tally).expect("the checker answers rather than failing");

	assert_eq!(tally.structural, 1, "a block that is neither node type is one structural fault: {tally:?}");
	assert_eq!(bad, 0, "and not a checksum failure - every checksum over this volume matches");
	assert_eq!(tally.checksum, 0, "nor a checksum fault found by descending into it: {tally:?}");
	assert_eq!(tally.io, 0, "and the medium answered every read: {tally:?}");
}
