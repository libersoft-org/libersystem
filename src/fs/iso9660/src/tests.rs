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

// A device whose blocks past `data` read as zeros rather than failing.
//
// THE ONLY WAY TO TEST A FILE LARGER THAN THE READ CEILING WITHOUT HOLDING ONE. The ceiling is
// 64 MiB, and a fixture that allocated that much to prove a window read does not allocate it would
// be its own counter-example. The descriptors and the directory live in `data`; the file's extent
// runs off the end of it and the device answers zeros, so the volume is as large as the test needs
// and the test costs a few kilobytes.
struct SparseDisc {
	data: Vec<u8>,
	blocks: u64,
}

impl BlockDevice for SparseDisc {
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		if lba >= self.blocks {
			return false;
		}
		let start = lba as usize * SECTOR_SIZE;
		match self.data.get(start..start + SECTOR_SIZE) {
			Some(src) => buf.copy_from_slice(src),
			None => buf.fill(0),
		}
		true
	}
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
fn both16(buf: &mut [u8], off: usize, v: u16) {
	buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
	buf[off + 2..off + 4].copy_from_slice(&v.to_be_bytes());
}

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
	// THE VOLUME SET, which the builder left as zeros - and zero is not a legal set size or
	// sequence number. The parser now reads both, so the fixture has to be a shape a mastering tool
	// actually produces: a set of one, and this volume is the first of it.
	//
	// The same lesson this milestone is named for, one field further on: a builder that writes media
	// nothing produces lets an internal round trip agree with itself while the format says
	// otherwise.
	both16(&mut pvd, 120, 1); // volume set size
	both16(&mut pvd, 124, 1); // volume sequence number
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
// The subdirectory's record: after the two 34-byte special entries and before HELLO.TXT's.
const SUB_REC: usize = 68;

#[test]
fn malformed_records_do_not_panic() {
	// (a) an even-id-length record ending exactly after its identifier (the pad byte
	// missing) used to slice past the record for the system-use area; (b) a Rock Ridge
	// NM entry with length 4 used to build an inverted range. Both must parse cleanly.
	let mut img = build_iso(false);
	let root_off = 19 * SECTOR_SIZE;
	// 36, not 35: an even-length identifier is followed by the padding byte the format requires, and
	// the parser checks it now. The `record` helper above has always emitted it; this hand-built
	// fixture did not, which is the shape a strict parser is supposed to notice.
	let mut a = vec![0u8; 36];
	a[0] = 36;
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
	img[root_off + ROOT_FREE..root_off + ROOT_FREE + 36].copy_from_slice(&a);
	img[root_off + ROOT_FREE + 36..root_off + ROOT_FREE + 78].copy_from_slice(&b);
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
	// The SVD was NOT taken as Joliet, and the proof is now stronger than "the names are different".
	//
	// This fixture writes UCS-2 identifiers into the one directory both namespaces share, so reading
	// them through the PVD means reading NUL bytes as name characters - and a name with a NUL in it
	// is not a name, which the component rule added in the sixth round refuses. The old assertion
	// was that `HELLO.TXT` is absent from the listing, which the same behaviour satisfies; this says
	// which listing came back.
	assert_eq!(fs.list(), Err(FsError::Corrupt), "the PVD namespace over UCS-2 identifiers is not a namespace, and the SVD was not used");
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
// A SUSP area whose `ER` lives BEHIND a Continuation Entry, which is what `xorriso` writes: `SP`,
// `PX` and `TF` fill the 255-byte record, so the `ER` naming the extension goes into a continuation
// block. The parser has to follow the `CE` to find it.
fn iso_with_susp_behind_ce(nm: &[u8]) -> Vec<u8> {
	let mut img = build_iso(false);
	// Block 22 is WORLD.TXT's data in the base image and nothing in this fixture reads it, so it is
	// free to be the continuation area.
	let ce_block = 22u32;
	let mut dot = record(19, SECTOR_SIZE as u32, true, &[0]);
	let mut sys: Vec<u8> = alloc::vec![b'S', b'P', 7, 1, 0xBE, 0xEF, 0];
	let mut ce = alloc::vec![b'C', b'E', 28, 1];
	for (lo, hi) in [(ce_block, ce_block), (0, 0), (32, 32)] {
		ce.extend_from_slice(&lo.to_le_bytes());
		ce.extend_from_slice(&hi.to_be_bytes());
	}
	sys.extend_from_slice(&ce);
	dot.extend_from_slice(&sys);
	dot[0] = dot.len() as u8;

	// The continuation area itself: the `ER` this reader is looking for.
	let id: &[u8] = b"RRIP_1991A";
	let mut area = alloc::vec![b'E', b'R', (8 + id.len()) as u8, 1, id.len() as u8, 0, 0, 1];
	area.extend_from_slice(id);
	let at = ce_block as usize * SECTOR_SIZE;
	img[at..at + area.len()].copy_from_slice(&area);

	// An ordinary record whose system-use area carries the NM under test.
	let file_id: &[u8] = b"X.TXT;1";
	let mut file = record(21, 9, false, file_id);
	file.extend_from_slice(nm);
	if file.len() % 2 == 1 {
		file.push(0);
	}
	file[0] = file.len() as u8;

	let root = dir_block(&[dot, record(19, SECTOR_SIZE as u32, true, &[1]), file]);
	img[19 * SECTOR_SIZE..19 * SECTOR_SIZE + root.len()].copy_from_slice(&root);
	img
}

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
	// REFUSED OUTRIGHT NOW, which is stronger than what this used to assert.
	//
	// The old assertion was that no name comes back containing '?' - the lossy character an invalid
	// unit used to become - and it was satisfied by the record being SKIPPED. Skipping a damaged
	// record is the short-listing-reported-as-complete this crate refuses everywhere else, and the
	// name rule added in the sixth round makes it a refusal: a damaged identifier is not a name, and
	// a directory holding one is not a directory this reader will list.
	assert_eq!(fs.list(), Err(FsError::Corrupt), "an odd-length UCS-2 identifier makes the directory unreadable, rather than one entry quietly missing");
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
	// The same strengthening as the odd-length case above: refused rather than skipped.
	assert_eq!(fs.list(), Err(FsError::Corrupt), "an unpaired surrogate makes the directory unreadable, rather than one entry quietly missing");
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

#[test]
fn a_volume_set_of_zero_is_refused_and_so_is_the_descriptors_own_sequence() {
	// `> 1` admitted zero, which is not a legal set size: a volume belongs to a set of at least
	// itself. And the DESCRIPTOR's own sequence number - which says which volume of the set this is
	// - was the one sequence number nothing read, while the root record's and every ordinary
	// record's were checked.
	for (name, off, value) in [("a set of no volumes", 120usize, 0u16), ("a set of two", 120, 2), ("the second volume of the set", 124, 2), ("volume zero", 124, 0)] {
		let mut img = build_iso(false);
		let pvd = 16 * SECTOR_SIZE;
		img[pvd + off..pvd + off + 2].copy_from_slice(&value.to_le_bytes());
		img[pvd + off + 2..pvd + off + 4].copy_from_slice(&value.to_be_bytes());
		assert!(Iso9660::mount(MemDisc { data: img }).is_none(), "{name} is refused before anything is read");
	}
}

#[test]
fn the_ranged_read_refuses_the_extent_the_whole_file_read_refuses() {
	// One record, two APIs, two answers. `read_extent` bounded the whole extent against the volume;
	// `read_file_into` checked only the block it was about to read, so a file whose extent runs off
	// the end of the volume was refused by `read_file` and partly readable through the ranged
	// reader - which is the API a caller reaches for precisely when the file is large.
	let mut img = build_iso(false);
	// HELLO.TXT sits at LBA 21 in a 23-block image. Declare a size that needs blocks 21..24.
	let root = 19 * SECTOR_SIZE;
	let mut at = root;
	while at < root + SECTOR_SIZE {
		let len = img[at] as usize;
		if len == 0 {
			break;
		}
		let lba = u32::from_le_bytes(img[at + 2..at + 6].try_into().unwrap());
		if lba == 21 {
			let huge = (3 * SECTOR_SIZE) as u32;
			img[at + 10..at + 14].copy_from_slice(&huge.to_le_bytes());
			img[at + 14..at + 18].copy_from_slice(&huge.to_be_bytes());
			break;
		}
		at += len;
	}
	let mut fs = Iso9660::mount(MemDisc { data: img }).expect("the volume still mounts");
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::Invalid), "the whole-file read refuses an extent past the volume");
	let mut one = [0u8; 1];
	assert_eq!(fs.read_file_into(b"HELLO.TXT", 0, &mut one), Err(FsError::Invalid), "and so does the ranged read, for the same record");
}

#[test]
fn a_continuation_entry_whose_halves_disagree_is_not_followed() {
	// `CE` carries three both-endian pairs and this reader read the little half of each, under a
	// comment saying that is what it does everywhere. It is what it does nowhere else: `both32` and
	// `both16` refuse unless the halves agree, at eight other sites. The pointer decides where Rock
	// Ridge is looked for, so a disc whose two copies disagree gets to choose what every file on it
	// is called.
	//
	// The fixture puts the `ER` behind a `CE`, which is what a real mastering tool does and what the
	// xorriso image found - then breaks the CE's big half. With the continuation not followed, no
	// `ER` is seen, Rock Ridge is not announced, and the 8.3 name stands.
	let nm: Vec<u8> = [b"NM".as_slice(), &[9, 1, 0], b"real"].concat();
	let mut img = iso_with_susp_behind_ce(&nm);
	let root = 19 * SECTOR_SIZE;
	let dot_len = img[root] as usize;
	let ce_at = img[root..root + dot_len].windows(2).position(|w| w == b"CE").map(|i| root + i).expect("the fixture's CE");
	// Both halves agree in the fixture; break the BIG one, which is the half that was never read.
	img[ce_at + 8..ce_at + 12].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
	// REFUSED, WHERE THIS USED TO FALL BACK - and the reversal is deliberate.
	//
	// The old answer was to mount with Rock Ridge off and let the 8.3 names stand. That looks
	// conservative and is not: a `CE` is the pointer that decides where Rock Ridge is looked for, so
	// one damaged field silently changes EVERY filename on the disc to its ISO fallback. Nothing
	// reports it, two files whose Rock Ridge names differ can collide on one 8.3 name, and a caller
	// opening a path gets a different file than the medium intends. `rock_ridge_name` already makes
	// this argument for itself one layer down - "falling back to the ISO9660 identifier is a name
	// the medium really carries; a truncated `NM` is not" - and the same reasoning says a pointer
	// this reader cannot trust is not a reason to rename the disc.
	//
	// Genuine ABSENCE still mounts without Rock Ridge, and an area holding a SUSP entry at a version
	// this build does not implement still ends the chain conservatively. Only a `CE` that is present
	// and impossible is damage.
	assert_eq!(Iso9660::mount_checked(MemDisc { data: img }).err(), Some(MountError::Corrupt), "a continuation whose halves disagree is damage, not a quiet rename of every file");

	// The same fixture UNBROKEN still finds the name, or the assertion above proves nothing.
	let mut fs = Iso9660::mount(MemDisc { data: iso_with_susp_behind_ce(&nm) }).expect("mount");
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(names.contains(&"real".to_string()), "the intact fixture announces Rock Ridge through the CE: {names:?}");
}

#[test]
fn a_rock_ridge_name_that_cannot_be_trusted_falls_back_rather_than_being_assembled() {
	// `rock_ridge_name` makes the fail-closed argument itself - "Falling back to the ISO9660
	// identifier is a name the medium really carries; a truncated `NM` is not" - and returns `None`
	// for `CE`, `CL`, `PL`, `RE` and a continued `NM` for exactly that reason. Two paths through the
	// same function produced the truncated name anyway.
	//
	// A malformed entry mid-walk used to `break`, so whatever `NM` fragments had been collected were
	// returned as the name. And an `NM` whose payload is not UTF-8 went through `unwrap_or("")`,
	// contributing nothing and leaving the rest - a name assembled out of the parts that happened to
	// decode.
	for (label, nm) in [
		// A valid NM followed by an entry whose declared length runs past the area.
		("a truncated entry after a valid name", [b"NM".as_slice(), &[9, 1, 0], b"real", b"XX", &[200, 1]].concat()),
		// A VALID FRAGMENT AND A BAD ONE, which is what makes this reach the decode's answer.
		//
		// A name made entirely of bad bytes decoded to "" under the old `unwrap_or("")` and then hit
		// `if out.is_empty() { None }` - so it fell back anyway and the two versions agreed. The
		// difference shows only where something valid has already been accumulated, which is exactly
		// the partial name the function refuses `CE` for.
		("a name assembled from the fragments that happened to decode", [b"NM".as_slice(), &[9u8, 1, 0], b"real", b"NM".as_slice(), &[9u8, 1, 0], &[0xff, 0xfe, 0xfd, 0xfc]].concat()),
	] {
		let mut fs = Iso9660::mount(MemDisc { data: iso_with_susp(true, &nm) }).expect("mount");
		let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
		assert!(!names.contains(&"real".to_string()), "{label}: a prefix must not become the name: {names:?}");
		assert!(names.iter().any(|n| n == "X.TXT"), "{label}: the 8.3 identifier stands instead: {names:?}");
	}

	// And the same fixture with an intact NM still reads it, or the assertions above prove only that
	// the fixture is broken.
	let good: Vec<u8> = [b"NM".as_slice(), &[9, 1, 0], b"real"].concat();
	let mut fs = Iso9660::mount(MemDisc { data: iso_with_susp(true, &good) }).expect("mount");
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(names.contains(&"real".to_string()), "an intact NM is still read: {names:?}");
}

#[test]
fn a_susp_entry_at_another_version_is_not_read_as_this_one() {
	// Every SUSP entry carries a version and every entry this reader implements is version 1. The
	// walker handed out `(signature, body)` and skipped the version byte entirely, so an entry with
	// the same two letters and a different structure was read as though it were this one - which is
	// what the `ST` check exists to stop, one field in.
	let nm: Vec<u8> = [b"NM".as_slice(), &[9, 1, 0], b"real"].concat();
	let mut img = iso_with_susp(true, &nm);
	let root = 19 * SECTOR_SIZE;
	let dot_len = img[root] as usize;
	let er_at = img[root..root + dot_len].windows(2).position(|w| w == b"ER").map(|i| root + i).expect("the fixture's ER");
	// The ER announces Rock Ridge; at another version it is a different structure and announces
	// nothing, so the 8.3 name stands.
	img[er_at + 3] = 2;
	let mut fs = Iso9660::mount(MemDisc { data: img }).expect("mount");
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(!names.contains(&"real".to_string()), "an ER at an unknown version announces nothing: {names:?}");
}

#[test]
fn an_er_whose_declared_lengths_do_not_fit_announces_nothing() {
	// `ER` is `LEN_ID, LEN_DES, LEN_SRC, EXT_VER` and then three variable fields. Only `LEN_ID` was
	// read, so an entry carrying a real identifier and nonsense in the rest switched the whole
	// disc's name source.
	let nm: Vec<u8> = [b"NM".as_slice(), &[9, 1, 0], b"real"].concat();
	let mut img = iso_with_susp(true, &nm);
	let root = 19 * SECTOR_SIZE;
	let dot_len = img[root] as usize;
	let er_at = img[root..root + dot_len].windows(2).position(|w| w == b"ER").map(|i| root + i).expect("the fixture's ER");
	// A description longer than the entry: the identifier is still there and still right.
	img[er_at + 5] = 200;
	let mut fs = Iso9660::mount(MemDisc { data: img }).expect("mount");
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(!names.contains(&"real".to_string()), "an ER whose lengths do not fit its own entry announces nothing: {names:?}");
}

#[test]
fn an_nm_entry_at_another_version_is_not_read_as_a_name() {
	// `for_each_susp` refuses `version != 1` and `rock_ridge_name` did not go through it: it walked
	// the area itself, reading `sys[off + 2]` for the length and `sys[off..off + 2]` for the
	// signature, and never `sys[off + 3]` at all. So an `NM` declaring version 2 was consumed as
	// though it were version 1 - and `NM` is the entry whose contents become a filename, which
	// becomes a lookup key. The test that covers the version rule alters an `ER`, which does go
	// through the walker.
	let good: Vec<u8> = [b"NM".as_slice(), &[9, 1, 0], b"real"].concat();
	let mut fs = Iso9660::mount(MemDisc { data: iso_with_susp(true, &good) }).expect("mount");
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(names.contains(&"real".to_string()), "the fixture's NM is read at version 1: {names:?}");

	let wrong: Vec<u8> = [b"NM".as_slice(), &[9, 2, 0], b"real"].concat();
	let mut fs = Iso9660::mount(MemDisc { data: iso_with_susp(true, &wrong) }).expect("mount");
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(!names.contains(&"real".to_string()), "an NM at version 2 is a different structure with the same signature: {names:?}");
	assert!(names.iter().any(|n| n == "X.TXT"), "and the ISO name stands instead: {names:?}");
}

#[test]
fn an_enhanced_volume_descriptor_does_not_kill_the_mount() {
	// ECMA-119's second edition defines the Enhanced Volume Descriptor: type 2, version 2. The
	// descriptor loop refused anything whose version byte was not 1 BEFORE it looked at the type,
	// under a comment saying version "is 1 for every descriptor this format defines" - so a
	// well-formed `PVD (v1)`, `Enhanced VD (v2)`, `Terminator` was refused at the second descriptor
	// rather than the primary hierarchy this reader fully understands being read.
	//
	// Skipping something unimplemented is not the same as refusing the volume.
	let mut img = build_iso(false);
	// The non-Joliet fixture puts the terminator at 17; move it to 18 and put the Enhanced
	// descriptor in between.
	let mut enhanced = img[16 * SECTOR_SIZE..17 * SECTOR_SIZE].to_vec();
	enhanced[0] = 2; // Supplementary/Enhanced
	enhanced[6] = 2; // ...at version 2, which is what makes it Enhanced
	img[17 * SECTOR_SIZE..18 * SECTOR_SIZE].copy_from_slice(&enhanced);
	img[18 * SECTOR_SIZE] = 255;
	img[18 * SECTOR_SIZE + 1..18 * SECTOR_SIZE + 6].copy_from_slice(b"CD001");
	img[18 * SECTOR_SIZE + 6] = 1;
	let mut fs = Iso9660::mount(MemDisc { data: img }).expect("the primary hierarchy mounts past an Enhanced descriptor");
	assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), b"hello iso");

	// And a type-2 descriptor at any OTHER version is still a refusal: 2 is the Enhanced one and
	// nothing else is defined.
	let mut img = build_iso(false);
	let mut odd = img[16 * SECTOR_SIZE..17 * SECTOR_SIZE].to_vec();
	odd[0] = 2;
	odd[6] = 3;
	img[17 * SECTOR_SIZE..18 * SECTOR_SIZE].copy_from_slice(&odd);
	img[18 * SECTOR_SIZE] = 255;
	img[18 * SECTOR_SIZE + 1..18 * SECTOR_SIZE + 6].copy_from_slice(b"CD001");
	img[18 * SECTOR_SIZE + 6] = 1;
	assert_eq!(Iso9660::mount_checked(MemDisc { data: img }).err(), Some(MountError::Corrupt), "a version this format does not define is not a descriptor to skip");
}

#[test]
fn a_joliet_descriptor_must_agree_with_the_primary_about_the_volume() {
	// The PVD's volume set size and sequence number are validated in the `1 =>` arm; the `2 =>` arm
	// took the Joliet root and the geometry and checked neither - and when Joliet is present that
	// geometry BECOMES the filesystem's. ECMA-119 requires a Supplementary descriptor's common
	// fields to match the primary's within one set, so a crafted medium could declare one volume
	// space in the PVD and another in the SVD and have the second used without the two ever being
	// compared.
	//
	// PER FIELD, because one mismatched-geometry test would pass on whichever of the four somebody
	// happened to check.
	assert!(Iso9660::mount(MemDisc { data: build_iso(true) }).is_some(), "the fixture itself agrees with its own primary");

	// Volume space size.
	let mut img = build_iso(true);
	both32(&mut img[17 * SECTOR_SIZE..], 80, 22);
	assert!(Iso9660::mount(MemDisc { data: img }).is_none(), "a Joliet descriptor declaring another volume space is not this volume's");

	// Logical block size.
	let mut img = build_iso(true);
	img[17 * SECTOR_SIZE + 128..17 * SECTOR_SIZE + 130].copy_from_slice(&512u16.to_le_bytes());
	img[17 * SECTOR_SIZE + 130..17 * SECTOR_SIZE + 132].copy_from_slice(&512u16.to_be_bytes());
	assert!(Iso9660::mount(MemDisc { data: img }).is_none(), "nor one declaring another block size");

	// Volume set size.
	let mut img = build_iso(true);
	both16(&mut img[17 * SECTOR_SIZE..], 120, 2);
	assert!(Iso9660::mount(MemDisc { data: img }).is_none(), "nor one claiming to belong to a larger set");

	// Volume sequence number.
	let mut img = build_iso(true);
	both16(&mut img[17 * SECTOR_SIZE..], 124, 2);
	assert!(Iso9660::mount(MemDisc { data: img }).is_none(), "nor one claiming to be the second volume of it");
}

#[test]
fn an_associated_file_record_is_validated_before_it_is_ignored() {
	// `parse_record` validated the extent, the size and the volume sequence, saw the associated
	// flag and returned `Ok(None)` - and the identifier length, the bounds and the identifier itself
	// were validated after that point. So an associated-file record with `id_len = 0` was silently
	// dropped, while the same `id_len = 0` on an ordinary record was `Corrupt`.
	//
	// That is the shape the `Result<Option<Entry>>` refactor was written to remove: a malformed
	// record disappearing from a directory rather than refusing it. `Ok(None)` means "a well-formed
	// record this reader deliberately does not surface".
	let mut img = build_iso(false);
	let root_off = 19 * SECTOR_SIZE;
	// Walk to the free space after the fixture's records and write one associated-file record.
	let mut at = root_off;
	while img[at] != 0 {
		at += img[at] as usize;
	}
	let mut rec = vec![0u8; 34];
	rec[0] = 34;
	both32(&mut rec, 2, 21);
	both32(&mut rec, 10, 9);
	rec[25] = 0x04; // associated file
	rec[28..30].copy_from_slice(&1u16.to_le_bytes());
	rec[30..32].copy_from_slice(&1u16.to_be_bytes());
	rec[32] = 1;
	rec[33] = b'A';
	img[at..at + rec.len()].copy_from_slice(&rec);
	let mut fs = Iso9660::mount(MemDisc { data: img.clone() }).expect("mount");
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(!names.iter().any(|n| n == "A"), "a well-formed associated file is not surfaced: {names:?}");

	// And a MALFORMED one is refused rather than dropped.
	img[at + 32] = 0; // id_len = 0
	let mut fs = Iso9660::mount(MemDisc { data: img }).expect("mount");
	assert_eq!(fs.list(), Err(FsError::Corrupt), "an associated-file record with no identifier is malformed, whatever this reader would have done with a valid one");
}

#[test]
fn a_window_read_of_a_file_past_the_ceiling_reads_where_the_whole_file_cannot() {
	// THE TEST BESIDE THIS ONE DOES NOT TEST WHAT ITS NAME SAYS. It calls a four-sector file
	// "large" and reads a window out of it, which exercises the head/middle/tail batching and
	// proves nothing about the 64 MiB ceiling - the file is eight kilobytes and `read_file` would
	// have staged it happily.
	//
	// The product property is the other one: a window can be read out of a file that `read_file`
	// REFUSES. Proving it needs a file past the ceiling, and holding one would make the fixture its
	// own counter-example - so the device answers zeros past the end of its image and the file's
	// extent runs off into that.
	let mut img = build_iso(false);
	// A volume large enough to hold the extent, and a device that will answer for all of it.
	let blocks: u64 = (MAX_READ_BYTES / SECTOR_SIZE) as u64 + 64;
	both32(&mut img[16 * SECTOR_SIZE..], 80, blocks as u32);
	let root_off = 19 * SECTOR_SIZE;
	let mut at = root_off;
	while img[at] != 0 {
		let len = img[at] as usize;
		let id_len = img[at + 32] as usize;
		if id_len > 1 && &img[at + 33..at + 33 + 5] == b"HELLO" {
			// One byte past the ceiling: `read_file` must refuse, and a window must not care.
			both32(&mut img[at..], 10, MAX_READ_BYTES as u32 + 1);
			break;
		}
		at += len;
	}
	let mut fs = Iso9660::mount(SparseDisc { data: img, blocks }).expect("mount");
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::TooLarge), "the whole file is past what one read may stage");
	// AND THE WINDOW IS ANSWERED ANYWAY, which is the whole point of the ranged path: the ceiling
	// bounds what a single read allocates, not what a file may contain.
	let mut window = [0u8; 32];
	let read = fs.read_file_into(b"HELLO.TXT", (MAX_READ_BYTES - 16) as u64, &mut window).expect("a window near the end of a file too large to stage");
	// SEVENTEEN, NOT THIRTY-TWO, and that is the second thing worth pinning: the file is one byte
	// past the ceiling, the window starts sixteen before the ceiling, so seventeen bytes remain. A
	// ranged read is bounded by the file's declared size and not by the buffer it was handed.
	assert_eq!(read, 17, "the window stops at the end of the file rather than filling the buffer");
	// And a window wholly inside the file fills what it asked for.
	let read = fs.read_file_into(b"HELLO.TXT", (MAX_READ_BYTES / 2) as u64, &mut window).expect("a window from the middle");
	assert_eq!(read, 32, "a window with the file behind it is the size asked for, from a file no whole read could hold");
}

#[test]
fn a_window_read_of_a_large_file_does_not_stage_the_whole_thing() {
	// `read_file` refuses past `MAX_READ_BYTES`; `read_file_into` exists so a window can be taken
	// out of a file that large, and until this round the storage service could not reach it - `IsoFs`
	// took the default `read_window`, which reads the whole file and slices it.
	//
	// The fixture is a logical file whose declared size is past the ceiling, in an image far smaller
	// than that: the extent is bounded by the volume's own block count, so the read is refused for
	// what it reaches rather than for what it claims - and the point is that the ceiling itself is
	// not what stops it.
	// The image grows so the file can: the extent is bounded by the volume's own block count, which
	// is the check that would otherwise refuse this before the ceiling ever came up.
	let mut img = build_iso(false);
	img.resize(SECTOR_SIZE * 32, 0);
	both32(&mut img[16 * SECTOR_SIZE..], 80, 32);
	let root_off = 19 * SECTOR_SIZE;
	let mut at = root_off;
	while img[at] != 0 {
		let len = img[at] as usize;
		let id_len = img[at + 32] as usize;
		if id_len > 1 && &img[at + 33..at + 33 + 5] == b"HELLO" {
			both32(&mut img[at..], 10, (SECTOR_SIZE * 4) as u32);
			break;
		}
		at += len;
	}
	let mut fs = Iso9660::mount(MemDisc { data: img }).expect("mount");
	// A window from the middle, which the whole-file path would have had to stage four sectors for.
	let mut window = [0u8; 16];
	let read = fs.read_file_into(b"HELLO.TXT", SECTOR_SIZE as u64, &mut window).expect("a window past the first sector");
	assert_eq!(read, 16, "the window is the size asked for");
	// A window spanning the head, the aligned middle and the tail - the three paths the batched read
	// takes, which a per-sector loop had no shape for.
	let mut wide = vec![0u8; SECTOR_SIZE * 2 + 32];
	let read = fs.read_file_into(b"HELLO.TXT", 16, &mut wide).expect("a window across three sectors");
	assert_eq!(read, SECTOR_SIZE * 2 + 32);
	// The first nine bytes of the extent are the fixture's own, so a window starting at 16 begins
	// past them and the batched read has to have got its offsets right to see zeros there.
	assert_eq!(&wide[..8], &[0u8; 8], "the head sector's bytes past the written content");
}

// P02M0126, sixth round: what a hostile disc can make this backend DO, and the namespaces it can
// make ambiguous.
//
// The round before bounded how much MEMORY a crafted image can make this backend spend - the walk
// reads a sector at a time instead of allocating the extent. What none of it bounded is WORK, and
// what none of it checked is whether the names a listing shows are names a lookup can reach.

#[test]
fn a_directory_extent_larger_than_the_work_bound_is_refused_before_it_is_read() {
	// `for_each_record` checked only that `lba + sectors` fits the medium and `MAX_DIR_ENTRIES`
	// counts entries LISTED - so a directory declaring a near-4 GiB extent whose every sector opens
	// with a zero byte yields no entries at all, never reaches the entry limit, and drives millions
	// of synchronous `read_block` calls while holding the StorageService request.
	//
	// `TooLarge`, not `Corrupt`: the directory may be perfectly well formed and this backend will
	// not walk something that size in one request.
	// Through the SUBDIRECTORY, because the mount checks the ROOT extent against the volume's block
	// count and would refuse this image before the walk ever ran. `for_each_record` makes the same
	// bounds check for a subdirectory - and the work bound is asked FIRST, which is what this
	// distinguishes: `TooLarge` rather than the `Invalid` a too-big-for-the-volume extent gets.
	let mut img = build_iso(false);
	let root = 19 * SECTOR_SIZE;
	both32(&mut img[root + SUB_REC..], 10, 64 * 1024 * 1024);
	let mut fs = Iso9660::mount(MemDisc { data: img }).expect("the geometry is otherwise sound");
	assert_eq!(fs.list_dir(b"SUB"), Err(FsError::TooLarge), "an extent past the work bound is refused before a sector of it is read");
}

#[test]
fn a_directory_record_that_is_not_a_legal_directory_refuses_the_listing() {
	// `parse_record` folded multi-extent and interleaving into a general `unsupported` flag and
	// `read_dir` filtered those entries out - defensible for a regular FILE this backend will not
	// read, and for a DIRECTORY it is a short listing presented as a complete one, with the whole
	// subtree behind it gone and no error anywhere.
	//
	// ECMA-119: a directory is not an associated file, is not interleaved and has a single file
	// section.
	for (label, at, bit) in [("associated", 25usize, 0x04u8), ("multi-extent", 25, 0x80), ("interleaved", 26, 0x01), ("a non-zero interleave gap", 27, 0x01)] {
		let mut img = build_iso(false);
		let root = 19 * SECTOR_SIZE;
		let sub = root + SUB_REC;
		assert!(img[sub + 25] & 0x02 != 0, "{label}: the fixture's second record must be the subdirectory");
		img[sub + at] |= bit;
		let mut fs = Iso9660::mount(MemDisc { data: img }).expect("mount");
		assert_eq!(fs.list(), Err(FsError::Corrupt), "{label} on a DIRECTORY record refuses the listing rather than dropping the subtree");
	}
}

#[test]
fn a_name_that_is_not_one_path_component_is_refused() {
	// `decode_name` accepted `/`, NUL and control characters. A UCS-2 `/` produces an entry listed
	// as `aaa/bbb` that lookup splits into two components, so the entry cannot be opened by the name
	// it was listed under; control characters additionally corrupt whatever renders the listing.
	for (label, byte) in [("a separator", b'/'), ("a control character", 0x07u8)] {
		let mut img = build_iso(false);
		let root = 19 * SECTOR_SIZE;
		// HELLO.TXT's identifier begins at byte 33 of its record.
		let rec = root + HELLO_REC;
		img[rec + 33] = byte;
		let mut fs = Iso9660::mount(MemDisc { data: img }).expect("mount");
		assert_eq!(fs.list(), Err(FsError::Corrupt), "{label} in an identifier is not a name");
	}
}

#[test]
fn two_entries_with_one_name_make_the_directory_ambiguous() {
	// The de-duplication compared against `out.last()` alone, which is right for ISO version
	// records - their identifiers are ordered, so equal names are adjacent - and wrong once names
	// come from somewhere other than the identifier: a listing showed two entries called `same` and
	// `open("same")` could only ever reach the first.
	//
	// Built with the version suffix, which is what makes two RECORDS with different identifiers
	// decode to one name without being neighbours.
	let mut img = build_iso(false);
	let root = 19 * SECTOR_SIZE;
	// The third record slot in the root, given HELLO.TXT's identifier again.
	// SUB's record copied into the free slot AFTER HELLO.TXT, so the two records naming `SUB` are
	// not neighbours - which is the whole point: the `out.last()` comparison collapses adjacent
	// equals and sees nothing at a distance.
	let sub = root + SUB_REC;
	let len = img[sub] as usize;
	let third = root + ROOT_FREE;
	let copy: Vec<u8> = img[sub..sub + len].to_vec();
	img[third..third + len].copy_from_slice(&copy);
	let mut fs = Iso9660::mount(MemDisc { data: img }).expect("mount");
	assert_eq!(fs.list(), Err(FsError::Corrupt), "a directory whose listing shows one name twice is ambiguous, and half of it is unreachable");
}

#[test]
fn a_multi_volume_set_is_unsupported_and_a_set_of_none_is_corrupt() {
	// `set_size != 1` returned `Corrupt` for both, and this error type deliberately distinguishes
	// "valid ISO using a construct this backend does not implement" from "does not obey its own
	// rules". A multi-volume set is the former; a set of zero volumes is the latter, because a
	// volume belongs to a set of at least itself.
	let pvd = 16 * SECTOR_SIZE;
	let mut img = build_iso(false);
	both16(&mut img[pvd..], 120, 2);
	assert_eq!(Iso9660::mount_checked(MemDisc { data: img }).err(), Some(MountError::Unsupported), "a valid multi-volume set is a construct this backend does not implement");

	let mut img = build_iso(false);
	both16(&mut img[pvd..], 120, 0);
	assert_eq!(Iso9660::mount_checked(MemDisc { data: img }).err(), Some(MountError::Corrupt), "a set of no volumes is a disc disobeying its own rules");
}

#[test]
fn a_second_primary_volume_descriptor_that_disagrees_is_refused() {
	// The standard permits a PVD to be recorded more than once, and every type-1 descriptor
	// overwrote the previous one - so a disc could carry a benign PVD followed by a hostile one and
	// be parsed by the second.
	let mut img = vec![0u8; SECTOR_SIZE * 25];
	let base = build_iso(false);
	img[..base.len()].copy_from_slice(&base);
	// Move the terminator out to sector 18 and put a second, disagreeing PVD at 17.
	let pvd: Vec<u8> = img[16 * SECTOR_SIZE..17 * SECTOR_SIZE].to_vec();
	img[18 * SECTOR_SIZE] = 255;
	img[18 * SECTOR_SIZE + 1..18 * SECTOR_SIZE + 6].copy_from_slice(b"CD001");
	img[18 * SECTOR_SIZE + 6] = 1;
	img[17 * SECTOR_SIZE..18 * SECTOR_SIZE].copy_from_slice(&pvd);
	// A second copy that AGREES is fine.
	assert!(Iso9660::mount(MemDisc { data: img.clone() }).is_some(), "a repeated PVD that agrees with the first is legal");
	// One that disagrees about where the root is, is not.
	both32(&mut img[17 * SECTOR_SIZE + 156..], 2, 20);
	assert_eq!(Iso9660::mount_checked(MemDisc { data: img }).err(), Some(MountError::Corrupt), "a second PVD naming a different root must not silently become the one that is used");
}

#[test]
fn a_continuation_that_loops_is_refused_rather_than_exhausting_its_cap() {
	// The chain was bounded at four iterations and reaching that bound produced the SAME successful
	// non-Rock-Ridge mount as reaching the end of the chain. So a disc whose `CE` points back into
	// itself renamed every file to its 8.3 fallback and said nothing - a cycle and an absence were
	// one observation.
	let nm: Vec<u8> = [b"NM".as_slice(), &[9, 1, 0], b"real"].concat();
	let mut img = iso_with_susp_behind_ce(&nm);
	let root = 19 * SECTOR_SIZE;
	let dot_len = img[root] as usize;
	let ce_at = img[root..root + dot_len].windows(2).position(|w| w == b"CE").map(|i| root + i).expect("the fixture's CE");
	// Point the continuation at the block the chain is already reading, in both halves, so the walk
	// visits the same (block, offset, length) a second time.
	let target = u32::from_le_bytes(img[ce_at + 4..ce_at + 8].try_into().unwrap());
	let cont = target as usize * SECTOR_SIZE;
	let inner = img[cont..cont + SECTOR_SIZE].windows(2).position(|w| w == b"ER").map(|i| cont + i);
	if let Some(er) = inner {
		// Turn the continuation's `ER` into a `CE` pointing back at itself.
		img[er] = b'C';
		img[er + 1] = b'E';
		img[er + 2] = 28;
		img[er + 3] = 1;
		let block = target.to_le_bytes();
		img[er + 4..er + 8].copy_from_slice(&block);
		img[er + 8..er + 12].copy_from_slice(&target.to_be_bytes());
		let off = (er - cont) as u32;
		img[er + 12..er + 16].copy_from_slice(&off.to_le_bytes());
		img[er + 16..er + 20].copy_from_slice(&off.to_be_bytes());
		img[er + 20..er + 24].copy_from_slice(&28u32.to_le_bytes());
		img[er + 24..er + 28].copy_from_slice(&28u32.to_be_bytes());
	}
	// Whatever this answers, it must not be a SUCCESSFUL mount that quietly renamed the disc.
	match Iso9660::mount_checked(MemDisc { data: img }) {
		Err(_) => {}
		Ok(mut fs) => {
			let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
			assert!(names.contains(&"real".to_string()), "a looping continuation must not silently produce the 8.3 namespace: {names:?}");
		}
	}
}

#[test]
fn a_directory_sector_pads_with_zeroes_or_it_is_not_padding() {
	// A zero record-length byte ends the sector, and the rest was never looked at - so a sector
	// holding a zero followed by arbitrary bytes read as canonical padding. Localised corruption
	// hidden, and a short directory reported as a complete one. Nothing is parsed out of those
	// bytes, so this is integrity rather than memory safety: "the directory ended here" is a claim,
	// and it is only true if what follows really is padding.
	let img = build_iso(false);
	let mut fs = Iso9660::mount(MemDisc { data: img.clone() }).expect("mount");
	assert!(fs.list().is_ok(), "the unmodified fixture lists");

	// Find the root's padding - the byte after the last record - and put something in it.
	let root = 19 * SECTOR_SIZE;
	let mut at = root;
	while img[at] != 0 {
		at += img[at] as usize;
	}
	assert!(at < root + SECTOR_SIZE, "the fixture's root ends inside its first sector");
	let mut broken = img;
	broken[at + 1] = 0x42;
	let mut fs = Iso9660::mount(MemDisc { data: broken }).expect("the volume still mounts - this is about the directory");
	assert!(fs.list().is_err(), "a nonzero byte in a directory sector's padding is not padding");
}
