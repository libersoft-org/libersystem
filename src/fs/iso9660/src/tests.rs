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
		// The terminator carries a version too, and it is 1 like every other descriptor. The
		// builder left it zero, which no mastering tool does - the parser now reads the field, so
		// the fixture has to be a shape the format actually produces.
		img[18 * SECTOR_SIZE + 6] = 1;
	} else {
		img[17 * SECTOR_SIZE] = 255;
		img[17 * SECTOR_SIZE + 1..17 * SECTOR_SIZE + 6].copy_from_slice(b"CD001");
		img[17 * SECTOR_SIZE + 6] = 1;
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
	// The Volume Sequence Number, which a real record carries and the parser now requires: an
	// extent belonging to another volume of a set would be read at that LBA on whichever disc is
	// actually in the drive.
	a[28..30].copy_from_slice(&1u16.to_le_bytes());
	a[30..32].copy_from_slice(&1u16.to_be_bytes());
	a[32] = 2;
	a[33..35].copy_from_slice(b"AB");
	let mut b = vec![0u8; 42];
	b[0] = 42;
	both32(&mut b, 2, 21);
	both32(&mut b, 10, 0);
	b[28..30].copy_from_slice(&1u16.to_le_bytes());
	b[30..32].copy_from_slice(&1u16.to_be_bytes());
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
	// Both halves, because a mismatch between them is a different finding with its own test: what
	// is under test here is a medium that consistently claims something impossible.
	let mut big_root = build_iso(false);
	big_root[16 * SECTOR_SIZE + 156 + 10..16 * SECTOR_SIZE + 156 + 14].copy_from_slice(&u32::MAX.to_le_bytes());
	big_root[16 * SECTOR_SIZE + 156 + 14..16 * SECTOR_SIZE + 156 + 18].copy_from_slice(&u32::MAX.to_be_bytes());
	assert!(Iso9660::mount(MemDisc { data: big_root }).is_none(), "a root extent past the volume");
	let mut big_vol = build_iso(false);
	big_vol[16 * SECTOR_SIZE + 80..16 * SECTOR_SIZE + 84].copy_from_slice(&1000u32.to_le_bytes());
	big_vol[16 * SECTOR_SIZE + 84..16 * SECTOR_SIZE + 88].copy_from_slice(&1000u32.to_be_bytes());
	assert!(Iso9660::mount(MemDisc { data: big_vol }).is_none(), "a block count past the device");
	let mut big_file = build_iso(false);
	let hello = 19 * SECTOR_SIZE + HELLO_REC;
	big_file[hello + 10..hello + 14].copy_from_slice(&u32::MAX.to_le_bytes());
	big_file[hello + 14..hello + 18].copy_from_slice(&u32::MAX.to_be_bytes());
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
fn an_extended_attribute_record_is_skipped_not_served() {
	// rec[1] counts XAR blocks at the extent's start - the data begins after them,
	// and serving the XAR as content would be a silent misread.
	let mut img = build_iso(false);
	img.extend(vec![0u8; SECTOR_SIZE]); // block 23 for the shifted content
	both32(&mut img, 16 * SECTOR_SIZE + 80, 24);
	let world = 20 * SECTOR_SIZE + 68; // WORLD.TXT's record after SUB's "." and ".."
	img[world + 1] = 1; // one XAR block ahead of the data
	let (old, new) = (22 * SECTOR_SIZE, 23 * SECTOR_SIZE);
	img.copy_within(old..old + 5, new);
	img[old..old + 5].copy_from_slice(b"XARBL");
	let mut fs = Iso9660::mount(MemDisc { data: img }).unwrap();
	assert_eq!(fs.read_file(b"SUB/WORLD.TXT").unwrap(), b"world");
}

#[test]
fn an_interleaved_file_is_refused_not_misread() {
	// a nonzero file-unit/gap pair stores the file with gap blocks woven in - reading
	// it contiguously would serve the gaps as content.
	let mut img = build_iso(false);
	img[19 * SECTOR_SIZE + HELLO_REC + 26] = 1;
	img[19 * SECTOR_SIZE + HELLO_REC + 27] = 1;
	let mut fs = Iso9660::mount(MemDisc { data: img }).unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::Invalid));
}

#[test]
fn a_joliet_escape_later_in_the_field_is_not_joliet() {
	// Rewritten 2026-08-11 rather than preserved. It used to assert that an escape sequence found
	// ANYWHERE in the 32-byte field made a descriptor Joliet, which is a layout Joliet does not
	// produce: the sequence is written at the start of the field, contiguously. Accepting it
	// anywhere means any descriptor that happens to contain those three bytes - in a publisher
	// string, in a mastering tool's padding - has its namespace taken as the volume's.
	let mut img = build_iso(true);
	// Move the SVD's escape sequence off the start of the field.
	img[17 * SECTOR_SIZE + 88..17 * SECTOR_SIZE + 91].copy_from_slice(&[0, 0, 0]);
	img[17 * SECTOR_SIZE + 100..17 * SECTOR_SIZE + 103].copy_from_slice(b"%/E");
	let mut fs = Iso9660::mount(MemDisc { data: img }).expect("the PVD still mounts it");
	// The volume's records are UCS-2 here, so reading them through the PVD namespace gives
	// something other than the Joliet names - which is the point: the SVD was NOT taken as Joliet.
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(!names.contains(&"HELLO.TXT".to_string()), "a descriptor whose escape is not at the start of the field must not supply the namespace: {names:?}");
}

#[test]
fn a_root_extended_attribute_record_is_skipped() {
	// the root record in the descriptor can carry an XAR length too - the root's
	// records begin after those blocks, like any extent's.
	let mut img = build_iso(false);
	img.extend(vec![0u8; SECTOR_SIZE * 2]); // blocks 23 (garbage XAR) and 24 unused
	both32(&mut img, 16 * SECTOR_SIZE + 80, 25);
	let (a, b) = (19 * SECTOR_SIZE, 23 * SECTOR_SIZE);
	img.copy_within(a..a + SECTOR_SIZE, b); // the root's real records move to 23
	let root_rec = 16 * SECTOR_SIZE + 156;
	img[root_rec + 1] = 1; // one XAR block ahead of the root data
	both32(&mut img, root_rec + 2, 22); // extent at 22: the XAR, data at 23
	let mut fs = Iso9660::mount(MemDisc { data: img }).unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), b"hello iso");
}

#[test]
fn an_associated_file_never_surfaces_or_matches() {
	// an associated file (flag 0x04, a secondary stream) precedes its same-named main
	// file - it must neither duplicate the listing nor shadow the main content.
	let mut img = build_iso(false);
	let mut fork = record(22, 5, false, &name("HELLO.TXT", false, false));
	fork[25] |= 0x04; // the fork points at the "world" block - shadowing would show
	let root = dir_block(&[record(19, SECTOR_SIZE as u32, true, &[0]), record(19, SECTOR_SIZE as u32, true, &[1]), fork, record(21, 9, false, &name("HELLO.TXT", false, false))]);
	img[19 * SECTOR_SIZE..20 * SECTOR_SIZE].copy_from_slice(&root);
	let mut fs = Iso9660::mount(MemDisc { data: img }).unwrap();
	let hits = fs.list().unwrap().into_iter().filter(|f| f.name == "HELLO.TXT").count();
	assert_eq!(hits, 1, "the fork must not duplicate the listing");
	assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), b"hello iso", "the fork must not shadow the main content");
}

#[test]
fn duplicate_versions_list_once() {
	// "F;1" and "F;2" decode to one name; records order equal names adjacently with
	// versions descending, so the listing keeps the first and lookups already take it.
	let mut img = build_iso(false);
	let dup = record(22, 5, false, b"HELLO.TXT;2");
	img[19 * SECTOR_SIZE + ROOT_FREE..19 * SECTOR_SIZE + ROOT_FREE + dup.len()].copy_from_slice(&dup);
	let mut fs = Iso9660::mount(MemDisc { data: img }).unwrap();
	let hits = fs.list().unwrap().into_iter().filter(|f| f.name == "HELLO.TXT").count();
	assert_eq!(hits, 1);
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
	// REFUSED, not skipped, since 2026-08-12. A record with no identifier is a malformed record, and
	// a listing that quietly omits it reports success over a directory it could not read - which is
	// the "corrupt directory becomes a short listing" finding, in its smallest form.
	assert_eq!(fs.list(), Err(FsError::Corrupt), "a record with no name is corruption, not an entry to pass over");

	// The rest of this test is about a directory that is intact, so it gets one.
	let mut img = build_iso(false);
	let root_off = 19 * SECTOR_SIZE;
	let _ = root_off;
	let mut fs = Iso9660::mount(MemDisc { data: img.clone() }).unwrap();
	let list = fs.list().unwrap();
	assert!(list.iter().all(|f| !f.name.is_empty()), "{list:?}");
	img.clear();
	assert_eq!(fs.read_file(b""), Err(FsError::NotFound));
	let sub = list.iter().find(|f| f.name == "SUB").unwrap();
	assert_eq!((sub.is_dir, sub.size), (true, 0), "a directory must list with size zero");
	let mut up: Vec<_> = fs.list_dir(b"SUB/..").unwrap().into_iter().map(|f| f.name).collect();
	up.sort();
	assert_eq!(up, ["HELLO.TXT", "SUB"]);
}

#[test]
fn a_contradictory_both_endian_field_is_refused() {
	// ECMA-119 records the critical numbers twice, once per byte order, and this reader took the
	// little half of all of them. A volume whose two halves disagree is internally contradictory
	// and used to mount.
	let mut img = build_iso(false);
	// The big half of the volume space size, made to disagree with the little half.
	img[16 * SECTOR_SIZE + 84..16 * SECTOR_SIZE + 88].copy_from_slice(&999u32.to_be_bytes());
	assert!(Iso9660::mount(MemDisc { data: img }).is_none(), "a both-endian field whose halves disagree must not mount");

	// And the same for the root record's extent, which is where a disagreement would point the
	// whole namespace somewhere else.
	let mut img = build_iso(false);
	img[16 * SECTOR_SIZE + 156 + 6..16 * SECTOR_SIZE + 156 + 10].copy_from_slice(&99u32.to_be_bytes());
	assert!(Iso9660::mount(MemDisc { data: img }).is_none(), "a root extent whose halves disagree must not mount");
}

#[test]
fn a_volume_with_no_primary_descriptor_is_refused() {
	// A Joliet SVD supplies the namespace on top of a PVD; it is not a volume on its own. The
	// match answered `(Some(joliet), _)`, so a recognised Joliet descriptor alone was enough.
	let mut img = build_iso(true);
	// Turn the PVD into a descriptor of no interest, leaving the Joliet SVD and the terminator.
	img[16 * SECTOR_SIZE] = 3;
	assert!(Iso9660::mount(MemDisc { data: img }).is_none(), "a Joliet SVD without a PVD is not a mountable volume");
}

#[test]
fn a_descriptor_set_with_no_terminator_is_refused() {
	// Nothing recorded whether the terminator was ever seen, so a set that simply stopped mounted.
	let mut img = build_iso(false);
	img[17 * SECTOR_SIZE] = 3; // was 255
	assert!(Iso9660::mount(MemDisc { data: img }).is_none(), "a descriptor set with no terminator must not mount");
}

#[test]
fn a_wrong_descriptor_version_is_refused() {
	let mut img = build_iso(false);
	img[16 * SECTOR_SIZE + 6] = 2;
	assert!(Iso9660::mount(MemDisc { data: img }).is_none(), "the descriptor version is part of the format and is read");
}

#[test]
fn a_root_record_that_is_not_a_directory_is_refused() {
	// The root record was taken entirely on trust: two numbers read out of it and nothing checked.
	let mut img = build_iso(false);
	img[16 * SECTOR_SIZE + 156 + 25] = 0; // clear the directory flag
	assert!(Iso9660::mount(MemDisc { data: img }).is_none(), "a root record without the directory flag is not a root");

	let mut img = build_iso(false);
	img[16 * SECTOR_SIZE + 156] = 30; // a length no Directory Record has
	assert!(Iso9660::mount(MemDisc { data: img }).is_none(), "a root record of the wrong length is refused");
}

#[test]
fn a_record_crossing_a_sector_boundary_is_corrupt() {
	// ECMA-119: a Directory Record shall not cross a logical sector boundary. The walk knew half
	// the rule - a zero length skips to the next sector - and never checked the other half.
	let mut img = build_iso(false);
	// The root extent declared as 200 bytes rather than a whole sector, so the boundary the walk
	// must respect is close enough for one record to straddle it - a record's length is a single
	// byte, so it cannot reach across a 2048-byte sector on its own.
	let root_rec = 16 * SECTOR_SIZE + 156;
	img[root_rec + 10..root_rec + 14].copy_from_slice(&200u32.to_le_bytes());
	img[root_rec + 14..root_rec + 18].copy_from_slice(&200u32.to_be_bytes());
	// A record at 148 claiming 100 bytes runs past the 200 the extent declares.
	let at = 19 * SECTOR_SIZE + ROOT_FREE;
	img[at] = 100;
	img[at + 32] = 1;
	let mut fs = Iso9660::mount(MemDisc { data: img }).expect("the volume still mounts");
	assert!(matches!(fs.list(), Err(FsError::Corrupt)), "a record that runs past its sector is corruption, not the end of the directory");
}

#[test]
fn a_malformed_record_is_corrupt_rather_than_a_short_listing() {
	// Both walks used to `break` on a record they could not use, so a damaged directory was
	// indistinguishable from one that does not hold the name: `NotFound` from a lookup and an `Ok`
	// with a short list from a read.
	let mut img = build_iso(false);
	let at = 19 * SECTOR_SIZE + ROOT_FREE;
	img[at] = 8; // shorter than a Directory Record can be
	let mut fs = Iso9660::mount(MemDisc { data: img }).expect("the volume still mounts");
	assert!(matches!(fs.list(), Err(FsError::Corrupt)), "a record too short to be one is corruption");
}

// Build a root directory whose first record carries a SUSP `SP` (and optionally an `ER`), and one
// ordinary record whose system-use area carries a Rock Ridge `NM`.
fn iso_with_susp(announce_rrip: bool, nm: &[u8]) -> Vec<u8> {
	iso_with_susp_id(if announce_rrip { Some(b"RRIP_1991A".as_slice()) } else { None }, nm)
}

// The same, with the `ER` identifier chosen: an extension this reader does not implement must not
// switch the name source, because reading somebody else's `NM` is the guess the check removes.
fn iso_with_susp_id(extension: Option<&[u8]>, nm: &[u8]) -> Vec<u8> {
	// The root block is REBUILT rather than patched: growing the "." record in place would push it
	// over the records that follow, and the walk would read the wreckage instead of the fixture.
	let mut img = build_iso(false);
	let mut dot = record(19, SECTOR_SIZE as u32, true, &[0]);
	let mut sp: Vec<u8> = vec![b'S', b'P', 7, 1, 0xBE, 0xEF, 0];
	if let Some(id) = extension {
		// A REAL `ER`: sig, len, version, then LEN_ID, LEN_DES, LEN_SRC, EXT_VER and the identifier
		// itself. The fixture used to be `[b'E', b'R', 8, 1, 1, 0, 0, 0]` - a declared identifier
		// length of one inside a total length of eight, which is exactly the header, so the
		// identifier it promised did not exist. It announced Rock Ridge anyway, because the parser
		// was scanning for the two letters rather than reading the entry.
		sp.extend_from_slice(&[b'E', b'R', (8 + id.len()) as u8, 1, id.len() as u8, 0, 0, 1]);
		sp.extend_from_slice(id);
	}
	dot.extend_from_slice(&sp);
	dot[0] = dot.len() as u8;

	// An ordinary record whose system-use area carries the Rock Ridge NM under test.
	let id: &[u8] = b"X.TXT;1";
	let base = 33 + id.len() + (id.len() % 2 == 0) as usize;
	let mut x = vec![0u8; base + nm.len()];
	x[0] = x.len() as u8;
	both32(&mut x, 2, 21);
	both32(&mut x, 10, 9);
	x[28..30].copy_from_slice(&1u16.to_le_bytes());
	x[30..32].copy_from_slice(&1u16.to_be_bytes());
	x[32] = id.len() as u8;
	x[33..33 + id.len()].copy_from_slice(id);
	x[base..].copy_from_slice(nm);

	let root = dir_block(&[dot, record(19, SECTOR_SIZE as u32, true, &[1]), x]);
	img[19 * SECTOR_SIZE..19 * SECTOR_SIZE + root.len()].copy_from_slice(&root);
	img
}

#[test]
fn an_er_this_reader_does_not_implement_does_not_turn_rock_ridge_on() {
	// `ER` names WHICH extension is in the area, and that name was never read: the test was
	// `sys.windows(2).any(|w| w == b"ER")`, satisfied by two bytes anywhere - inside another
	// extension's payload, inside a name, inside padding.
	//
	// Announcing something else must leave the ISO identifier in place. Reading a foreign
	// extension's `NM` is exactly the guess the SUSP negotiation was added to remove, one step
	// further along.
	let nm: Vec<u8> = [b"NM".as_slice(), &[9, 1, 0], b"real"].concat();
	let mut fs = Iso9660::mount(MemDisc { data: iso_with_susp_id(Some(b"XA_SOMETHING"), &nm) }).expect("mount");
	let names: Vec<String> = fs.list().expect("list").into_iter().map(|f| f.name).collect();
	assert!(names.iter().any(|n| n.starts_with("X.TXT")), "the ISO identifier stands: {names:?}");
	assert!(!names.iter().any(|n| n == "real"), "an extension this reader does not implement must not name the file: {names:?}");

	// And a well-formed `ER` whose declared identifier length runs past the entry is not an
	// announcement either - it is a malformed one.
	let mut fs = Iso9660::mount(MemDisc { data: iso_with_susp_id(Some(b"RRIP_1991A_AND_MORE_THAN_FITS"), &nm) }).expect("mount");
	let names: Vec<String> = fs.list().expect("list").into_iter().map(|f| f.name).collect();
	assert!(!names.iter().any(|n| n == "real"), "an identifier this reader does not know is not this reader's: {names:?}");
}

#[test]
fn a_rock_ridge_name_is_read_only_when_susp_announces_it() {
	// `NM` was believed wherever those two bytes appeared, with no `SP` and no `ER` - so a valid
	// non-Rock-Ridge volume whose system-use area happens to contain them had its filenames
	// replaced by whatever followed.
	let nm: Vec<u8> = [b"NM".as_slice(), &[9, 1, 0], b"real"].concat();

	// Announced: the Rock Ridge name wins.
	let mut fs = Iso9660::mount(MemDisc { data: iso_with_susp(true, &nm) }).expect("mount");
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(names.contains(&"real".to_string()), "with SP and ER present the NM name is used: {names:?}");

	// Not announced: the ISO9660 identifier stands.
	let mut fs = Iso9660::mount(MemDisc { data: iso_with_susp(false, &nm) }).expect("mount");
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(names.contains(&"X.TXT".to_string()), "without SUSP announcing RRIP the identifier stands: {names:?}");
	assert!(!names.contains(&"real".to_string()), "an NM nobody announced must not become a name: {names:?}");
}

#[test]
fn a_continued_rock_ridge_name_falls_back_rather_than_truncating() {
	// An `NM` with the CONTINUE flag, or a `CE` this parser cannot follow, yields a PREFIX - so two
	// long names differing late become one name, and that name is the lookup key. Falling back to
	// the identifier is a name the medium really carries; half of one is not.
	let nm: Vec<u8> = [b"NM".as_slice(), &[9, 1, 0x01], b"half"].concat();
	let mut fs = Iso9660::mount(MemDisc { data: iso_with_susp(true, &nm) }).expect("mount");
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(names.contains(&"X.TXT".to_string()), "a continued name falls back to the identifier: {names:?}");
	assert!(!names.contains(&"half".to_string()), "a truncated Rock Ridge name must not be served: {names:?}");
}

#[test]
fn an_odd_length_joliet_identifier_is_refused() {
	// `chunks_exact(2)` dropped a trailing odd byte and an invalid unit became '?', so two distinct
	// damaged identifiers could collide on one name - and the name is the key.
	let mut img = build_iso(true);
	let at = 19 * SECTOR_SIZE + ROOT_FREE;
	let id = "AB".encode_utf16().flat_map(|u| u.to_be_bytes()).collect::<Vec<u8>>();
	let base = 33 + id.len() + 1 + (id.len() % 2 == 0) as usize;
	let mut rec = vec![0u8; base];
	rec[0] = rec.len() as u8;
	both32(&mut rec, 2, 21);
	both32(&mut rec, 10, 9);
	rec[28..30].copy_from_slice(&1u16.to_le_bytes());
	rec[30..32].copy_from_slice(&1u16.to_be_bytes());
	rec[32] = (id.len() + 1) as u8; // an ODD identifier length
	rec[33..33 + id.len()].copy_from_slice(&id);
	img[at..at + rec.len()].copy_from_slice(&rec);
	let mut fs = Iso9660::mount(MemDisc { data: img }).expect("mount");
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(!names.iter().any(|n| n.contains('?')), "a damaged UCS-2 identifier must not become a lossy name: {names:?}");
}

#[test]
fn a_directory_read_as_a_file_says_so() {
	// `NotFound` said the name does not exist, when what is true is that it names a directory.
	let mut fs = Iso9660::mount(MemDisc { data: build_iso(false) }).expect("mount");
	assert_eq!(fs.read_file(b"SUB"), Err(FsError::IsDir));
	// And a file used as a path component is `NotDir`, not `NotFound`.
	assert!(matches!(fs.read_file(b"HELLO.TXT/X"), Err(FsError::NotDir)));
}

#[test]
fn a_surrogate_ucs2_unit_is_refused() {
	// An unpaired surrogate is not a character. It used to become '?', so two distinct damaged
	// identifiers collided on one name - and the name is what a lookup matches.
	let mut img = build_iso(true);
	let at = 19 * SECTOR_SIZE + ROOT_FREE;
	let id: Vec<u8> = [0xD8u8, 0x00, 0x00, 0x41].to_vec(); // lone high surrogate, then 'A'
	let base = 33 + id.len() + (id.len() % 2 == 0) as usize;
	let mut rec = vec![0u8; base];
	rec[0] = rec.len() as u8;
	both32(&mut rec, 2, 21);
	both32(&mut rec, 10, 9);
	rec[28..30].copy_from_slice(&1u16.to_le_bytes());
	rec[30..32].copy_from_slice(&1u16.to_be_bytes());
	rec[32] = id.len() as u8;
	rec[33..33 + id.len()].copy_from_slice(&id);
	img[at..at + rec.len()].copy_from_slice(&rec);
	let mut fs = Iso9660::mount(MemDisc { data: img }).expect("mount");
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(!names.iter().any(|n| n.contains('?')), "an unpaired surrogate must not become a lossy name: {names:?}");
}

#[test]
fn a_nonzero_susp_skip_is_honoured() {
	// SUSP's `SP` declares how many bytes of each System Use Area belong to somebody else. Reading
	// from offset 0 regardless is how another extension's bytes become a filename.
	let nm: Vec<u8> = [b"NM".as_slice(), &[9, 1, 0], b"real"].concat();
	let mut img = iso_with_susp(true, &nm);
	// Declare a skip of 4 without moving the NM: the parser must then read past the NM's start and
	// find nothing it recognises, rather than reporting a name from bytes it was told to skip.
	let root = 19 * SECTOR_SIZE;
	let dot_len = img[root] as usize;
	// FOUND rather than computed. This was `root + dot_len - 15`, which encoded "ER is 8 bytes and
	// SP is 7" - so making the fixture's `ER` a real one, with an identifier in it, moved the entry
	// and broke a test that is not about either length.
	let sp_at = img[root..root + dot_len].windows(2).position(|w| w == b"SP").map(|i| root + i).expect("the fixture's SP");
	img[sp_at + 6] = 4;
	let mut fs = Iso9660::mount(MemDisc { data: img }).expect("mount");
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(!names.contains(&"real".to_string()), "a declared skip must be honoured, not read past: {names:?}");
}

#[test]
fn a_multi_volume_set_is_refused_at_the_mount() {
	// The header says multi-volume sets are outside this reader's subset and refused, and ONE line
	// enforced it: a per-record sequence-number check, halfway through a listing. The Volume Set
	// Size in the PVD - which says how many volumes the set has - was not read at all, and neither
	// was the ROOT record's own sequence number, so the one record that decides where the whole
	// namespace begins was exempt from the rule every other record obeyed.
	let mut img = build_iso(false);
	let pvd = 16 * SECTOR_SIZE;
	img[pvd + 120..pvd + 122].copy_from_slice(&2u16.to_le_bytes());
	img[pvd + 122..pvd + 124].copy_from_slice(&2u16.to_be_bytes());
	assert!(Iso9660::mount(MemDisc { data: img }).is_none(), "a set of two volumes is refused before anything is read");

	// And the root record's own volume, which every other record is checked for.
	let mut img = build_iso(false);
	let root_rec = pvd + 156;
	img[root_rec + 28..root_rec + 30].copy_from_slice(&2u16.to_le_bytes());
	img[root_rec + 30..root_rec + 32].copy_from_slice(&2u16.to_be_bytes());
	assert!(Iso9660::mount(MemDisc { data: img }).is_none(), "a root record naming another volume is not this volume's root");
}

#[test]
fn a_volume_from_another_set_is_refused() {
	// The Volume Sequence Number names the volume an extent lives on; with one block device behind
	// this reader, volume 2's extent would be read at that LBA on whichever disc is in the drive.
	let mut img = build_iso(false);
	let hello = 19 * SECTOR_SIZE + HELLO_REC;
	img[hello + 28..hello + 30].copy_from_slice(&2u16.to_le_bytes());
	img[hello + 30..hello + 32].copy_from_slice(&2u16.to_be_bytes());
	let mut fs = Iso9660::mount(MemDisc { data: img }).expect("mount");
	// The record names volume 2 and this reader has one device, so reading its extent would read
	// that LBA on whichever disc is in the drive. Omitting it from the listing was the old answer and
	// it is the wrong shape: a directory this reader cannot represent is refused, rather than served
	// with a hole in it that a caller cannot see.
	assert_eq!(fs.list(), Err(FsError::Corrupt), "a record from another volume of a set makes the directory unreadable, not shorter");
}

// Whether this machine can master an ISO with a tool that has no stake in this parser's
// assumptions. `xorriso` is the one in Debian; its absence skips the test LOUDLY, because a golden
// test that quietly does not run is worse than one that is not written.
fn xorriso_available() -> bool {
	match std::process::Command::new("xorriso").arg("-version").output() {
		Ok(out) => out.status.success(),
		Err(_) => {
			std::eprintln!("SKIPPED: xorriso is not installed, so the independent-mastering test cannot run");
			false
		}
	}
}

// An ISO mastered by `xorriso` with Rock Ridge, carrying a long name, a nested directory and two
// files, returned as the bytes of the disc.
fn xorriso_image() -> Option<Vec<u8>> {
	if !xorriso_available() {
		return None;
	}
	let dir = std::env::temp_dir().join("iso-gold");
	let _ = std::fs::remove_dir_all(&dir);
	let tree = dir.join("tree");
	std::fs::create_dir_all(tree.join("sub")).expect("a source tree");
	// A name ECMA-119 cannot hold: lower case, longer than 8.3, and with a second dot. Rock Ridge
	// is what carries it, which is the point of the test.
	std::fs::write(tree.join("A-Long.Name.txt"), b"mastered by xorriso, not by this crate\n").expect("a file");
	std::fs::write(tree.join("sub").join("nested.txt"), b"one level down\n").expect("a nested file");
	let path = dir.join("gold.iso");
	let made = std::process::Command::new("xorriso").arg("-as").arg("mkisofs").arg("-quiet").arg("-rational-rock").arg("-o").arg(&path).arg(&tree).output().expect("xorriso runs");
	if !made.status.success() {
		std::eprintln!("SKIPPED: xorriso could not master the image ({})", String::from_utf8_lossy(&made.stderr).trim());
		return None;
	}
	std::fs::read(&path).ok()
}

#[test]
fn an_image_from_an_independent_tool_lists_and_reads_back() {
	// EVERY OTHER TEST IN THIS FILE USES A FIXTURE THIS CRATE BUILDS, and that is the shape which
	// let the Rock Ridge negotiation be wrong for as long as it was: a synthetic image omits what
	// the parser does not check, so both stay wrong together. `xorriso` has no stake in this
	// parser's assumptions - it writes a real `ER` entry with a real identifier, real `NM` records
	// and a real SUSP area, whatever this reader happens to look for.
	let Some(bytes) = xorriso_image() else { return };
	let mut fs = Iso9660::mount(MemDisc { data: bytes }).expect("an xorriso image mounts");

	let root = fs.list().expect("the root lists");
	let names: Vec<&str> = root.iter().map(|e| e.name.as_str()).collect();
	assert!(names.contains(&"A-Long.Name.txt"), "Rock Ridge carries the long name, got {names:?}");
	assert!(names.contains(&"sub"), "and the subdirectory, got {names:?}");

	assert_eq!(fs.read_file(b"A-Long.Name.txt").as_deref(), Ok(&b"mastered by xorriso, not by this crate\n"[..]));
	assert_eq!(fs.read_file(b"sub/nested.txt").as_deref(), Ok(&b"one level down\n"[..]));

	// And the ranged read over the same file, which is the reader a large file goes through.
	let mut window = [0u8; 8];
	let read = fs.read_file_into(b"A-Long.Name.txt", 9, &mut window).expect("a ranged read");
	assert_eq!(&window[..read], b"by xorri", "byte 9 onwards, from a real disc");
	assert_eq!(fs.read_file_into(b"sub/nested.txt", 1000, &mut window), Ok(0), "past the end reads nothing");
}
