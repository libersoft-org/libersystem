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
