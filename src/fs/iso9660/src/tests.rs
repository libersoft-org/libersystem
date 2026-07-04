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
	if joliet {
		s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
	} else {
		s.into_bytes()
	}
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
	both32(&mut pvd, 80, 23); // volume space size: the whole 23-block image
	pvd[128..130].copy_from_slice(&2048u16.to_le_bytes());
	pvd[130..132].copy_from_slice(&2048u16.to_be_bytes());
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

// The root block's records: "." (34) + ".." (34) + SUB (36) + HELLO.TXT;1 (44) = 148
// bytes, so 148 is the first free record slot and 104 is HELLO.TXT's offset.
const ROOT_FREE: usize = 148;
const HELLO_REC: usize = 104;

#[test]
fn malformed_records_do_not_panic() {
	// (a) an even-id-length record ending exactly after its identifier (the pad byte
	// missing) used to slice past the record for the system-use area; (b) a Rock Ridge
	// NM entry with length 4 used to build an inverted range. Both must parse cleanly.
	let mut img = build_iso(false);
	let root_off = 19 * SECTOR_SIZE;
	let mut a = vec![0u8; 35];
	a[0] = 35;
	both32(&mut a, 2, 21);
	both32(&mut a, 10, 0);
	a[32] = 2;
	a[33..35].copy_from_slice(b"AB");
	let mut b = vec![0u8; 42];
	b[0] = 42;
	both32(&mut b, 2, 21);
	both32(&mut b, 10, 0);
	b[32] = 1;
	b[33] = b'C';
	b[34..36].copy_from_slice(b"NM");
	b[36] = 4; // sig + len + version only: no flags, no name
	b[37] = 1;
	img[root_off + ROOT_FREE..root_off + ROOT_FREE + 35].copy_from_slice(&a);
	img[root_off + ROOT_FREE + 35..root_off + ROOT_FREE + 77].copy_from_slice(&b);
	let mut fs = Iso9660::mount(MemDisc { data: img }).unwrap();
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(names.contains(&"AB".to_string()) && names.contains(&"C".to_string()), "{names:?}");
}

#[test]
fn forged_extents_do_not_allocate_or_mount() {
	// the extents are the medium's own claims: a root length past the volume refuses
	// at mount, a volume claiming more blocks than the device refuses at mount, and a
	// forged file size errors cleanly instead of allocating gigabytes up front.
	let mut big_root = build_iso(false);
	big_root[16 * SECTOR_SIZE + 156 + 10..16 * SECTOR_SIZE + 156 + 14].copy_from_slice(&u32::MAX.to_le_bytes());
	assert!(Iso9660::mount(MemDisc { data: big_root }).is_none(), "a root extent past the volume");
	let mut big_vol = build_iso(false);
	big_vol[16 * SECTOR_SIZE + 80..16 * SECTOR_SIZE + 84].copy_from_slice(&1000u32.to_le_bytes());
	assert!(Iso9660::mount(MemDisc { data: big_vol }).is_none(), "a block count past the device");
	let mut big_file = build_iso(false);
	let hello = 19 * SECTOR_SIZE + HELLO_REC;
	big_file[hello + 10..hello + 14].copy_from_slice(&u32::MAX.to_le_bytes());
	let mut fs = Iso9660::mount(MemDisc { data: big_file }).unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::Invalid));
}

#[test]
fn a_non_2048_block_size_does_not_mount() {
	// the backend reads in 2048-byte units; a volume with another legal logical block
	// size would be read at wrong positions - it must refuse, not misread.
	let mut img = build_iso(false);
	img[16 * SECTOR_SIZE + 128..16 * SECTOR_SIZE + 130].copy_from_slice(&512u16.to_le_bytes());
	assert!(Iso9660::mount(MemDisc { data: img }).is_none());
}

#[test]
fn a_multi_extent_file_is_refused_not_truncated() {
	// flag bit 0x80 marks a file continuing in further records; serving only the first
	// extent would be a silent truncation.
	let mut img = build_iso(false);
	img[19 * SECTOR_SIZE + HELLO_REC + 25] |= 0x80;
	let mut fs = Iso9660::mount(MemDisc { data: img }).unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::Invalid));
}

#[test]
fn listing_contract_and_dot_dot() {
	// an empty-named record never surfaces or matches an empty lookup, a directory
	// lists with size zero, and ".." resolves to the parent as on the other backends.
	let mut img = build_iso(false);
	let root_off = 19 * SECTOR_SIZE;
	let mut e = vec![0u8; 34];
	e[0] = 34; // id_len 0: an empty name
	img[root_off + ROOT_FREE..root_off + ROOT_FREE + 34].copy_from_slice(&e);
	let mut fs = Iso9660::mount(MemDisc { data: img }).unwrap();
	let list = fs.list().unwrap();
	assert!(list.iter().all(|f| !f.name.is_empty()), "{list:?}");
	assert_eq!(fs.read_file(b""), Err(FsError::NotFound));
	let sub = list.iter().find(|f| f.name == "SUB").unwrap();
	assert_eq!((sub.is_dir, sub.size), (true, 0), "a directory must list with size zero");
	let mut up: Vec<_> = fs.list_dir(b"SUB/..").unwrap().into_iter().map(|f| f.name).collect();
	up.sort();
	assert_eq!(up, ["HELLO.TXT", "SUB"]);
}
