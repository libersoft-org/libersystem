// Host tests for the ISO9660 backend, run with `cd src/fs/iso9660 && cargo test`. A
// Vec-backed block device stands in for the disc; each image is synthesized in memory by
// a small builder, so the tests need no mkisofs and are deterministic - mounting the
// image, listing it, and reading files back proves descriptor scanning, the directory
// walk, plain 8.3 names, and Joliet long names all work.

use super::*;

// A RAM-backed block device: one contiguous Vec of 2048-byte blocks, read-only.
struct MemDisc {
	data: Vec<u8>,
}

impl BlockDevice for MemDisc {
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		let start = lba as usize * SECTOR_SIZE;
		let Some(src) = self.data.get(start..start + SECTOR_SIZE) else {
			return false;
		};
		buf.copy_from_slice(src);
		true
	}
}

// Write a both-endian u32 (LE then BE) at `off`, as ISO9660 stores its extent fields.
fn both32(buf: &mut [u8], off: usize, v: u32) {
	buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
	buf[off + 4..off + 8].copy_from_slice(&v.to_be_bytes());
}

// Build one fixed directory record into a Vec: extent LBA, size, dir flag, and id.
fn record(lba: u32, size: u32, is_dir: bool, id: &[u8]) -> Vec<u8> {
	let rec_len = 33 + id.len() + (id.len() % 2 == 0) as usize;
	let mut r = vec![0u8; rec_len];
	r[0] = rec_len as u8;
	both32(&mut r, 2, lba);
	both32(&mut r, 10, size);
	r[25] = if is_dir { 0x02 } else { 0 };
	r[28..30].copy_from_slice(&1u16.to_le_bytes());
	r[30..32].copy_from_slice(&1u16.to_be_bytes());
	r[32] = id.len() as u8;
	r[33..33 + id.len()].copy_from_slice(id);
	r
}

// Build a directory extent (one block) from its records.
fn dir_block(records: &[Vec<u8>]) -> Vec<u8> {
	let mut b = vec![0u8; SECTOR_SIZE];
	let mut off = 0;
	for r in records {
		b[off..off + r.len()].copy_from_slice(r);
		off += r.len();
	}
	b
}

// Encode a name: ASCII 8.3 + ";1" for the PVD, big-endian UCS-2 for Joliet.
fn name(s: &str, dir: bool, joliet: bool) -> Vec<u8> {
	let s = if dir { s.into() } else { format!("{s};1") };
	if joliet { s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect() } else { s.into_bytes() }
}

// Build a one-level ISO: PVD (+ optional Joliet SVD), terminator, root, one subdir, and
// files. Layout: 16 PVD, 17 SVD/term, 18 term, 19 root, 20 sub, 21.. file extents.
fn build_iso(joliet: bool) -> Vec<u8> {
	let mut img = vec![0u8; SECTOR_SIZE * 23];
	let root_lba = 19u32;
	let sub_lba = 20u32;
	let mut blk = |lba: u32, bytes: &[u8]| {
		let o = lba as usize * SECTOR_SIZE;
		img[o..o + bytes.len()].copy_from_slice(bytes);
	};
	// hello.txt at 21, world.txt at 22 (inside SUB)
	blk(21, b"hello iso");
	blk(22, b"world");
	let root = dir_block(&[
		record(root_lba, SECTOR_SIZE as u32, true, &[0]),
		record(root_lba, SECTOR_SIZE as u32, true, &[1]),
		record(sub_lba, SECTOR_SIZE as u32, true, &name("SUB", true, joliet)),
		record(21, 9, false, &name("HELLO.TXT", false, joliet)),
	]);
	let sub = dir_block(&[record(sub_lba, SECTOR_SIZE as u32, true, &[0]), record(root_lba, SECTOR_SIZE as u32, true, &[1]), record(22, 5, false, &name("WORLD.TXT", false, joliet))]);
	blk(19, &root);
	blk(20, &sub);
	// PVD at 16, Joliet SVD at 17 when asked, terminator after
	let mut pvd = vec![0u8; SECTOR_SIZE];
	pvd[0] = 1;
	pvd[1..6].copy_from_slice(b"CD001");
	pvd[6] = 1;
	pvd[156..156 + record(root_lba, SECTOR_SIZE as u32, true, &[0]).len()].copy_from_slice(&record(root_lba, SECTOR_SIZE as u32, true, &[0]));
	blk(16, &pvd);
	if joliet {
		let mut svd = pvd.clone();
		svd[0] = 2;
		svd[88..91].copy_from_slice(b"%/E");
		blk(17, &svd);
		img[18 * SECTOR_SIZE] = 255;
		img[18 * SECTOR_SIZE + 1..18 * SECTOR_SIZE + 6].copy_from_slice(b"CD001");
	} else {
		img[17 * SECTOR_SIZE] = 255;
		img[17 * SECTOR_SIZE + 1..17 * SECTOR_SIZE + 6].copy_from_slice(b"CD001");
	}
	img
}

#[test]
fn mount_list_read_8_3() {
	let mut fs = Iso9660::mount(MemDisc { data: build_iso(false) }).unwrap();
	let mut names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	names.sort();
	assert_eq!(names, ["HELLO.TXT", "SUB"]);
	assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), b"hello iso");
	assert_eq!(fs.read_file(b"SUB/WORLD.TXT").unwrap(), b"world");
}

#[test]
fn joliet_names() {
	let mut fs = Iso9660::mount(MemDisc { data: build_iso(true) }).unwrap();
	let mut names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	names.sort();
	assert_eq!(names, ["HELLO.TXT", "SUB"]);
	assert_eq!(fs.list_dir(b"SUB").unwrap().len(), 1);
	assert_eq!(fs.read_file(b"SUB/WORLD.TXT").unwrap(), b"world");
}

#[test]
fn missing_is_not_found() {
	let mut fs = Iso9660::mount(MemDisc { data: build_iso(false) }).unwrap();
	assert_eq!(fs.read_file(b"NOPE.TXT"), Err(FsError::NotFound));
}
