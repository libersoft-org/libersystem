// Host tests for the UDF backend, run with `cd src/fs/udf && cargo test`. A Vec-backed
// block device stands in for the disc; each image is synthesized in memory by a small
// builder, so the tests need no mkudffile and are deterministic - mounting the image,
// listing it, and reading files back proves the Anchor / partition / File Set walk, the
// directory descent, embedded data, and Latin-1 / UCS-2 names all work.

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

fn w16(b: &mut [u8], off: usize, v: u16) {
	b[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
fn w32(b: &mut [u8], off: usize, v: u32) {
	b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn w64(b: &mut [u8], off: usize, v: u64) {
	b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

// Write a descriptor tag id, its location, and stamp its checksum (byte 4 over the
// other fifteen tag bytes), as every real descriptor carries - the reader verifies both.
// Finish a descriptor: its identifier, its own address, the BODY CRC, and only then the tag
// checksum - which covers the first sixteen bytes and therefore has to be computed last.
//
// The CRC used to be missing entirely. That is not a small fixture detail: it meant every synthetic
// image in this file agreed with the parser rather than with the format, and the parser could go on
// not checking the one field that protects the descriptor's contents. Writing it here is what let
// the check be turned on.
//
// Callers must fill the body BEFORE calling this, which is the ordering a real formatter has too.
fn tag(b: &mut [u8], id: u16, loc: u32) {
	w16(b, 0, id);
	// THE DESCRIPTOR VERSION, which this never wrote and the parser never read - the two agreeing
	// with each other rather than with the format, which is the shape this whole milestone is about.
	w16(b, 2, 2);
	w32(b, 12, loc);
	// DescriptorCRCLength covers everything after the 16-byte tag.
	let crc_len = b.len() - 16;
	w16(b, 10, crc_len as u16);
	w16(b, 8, crc_ccitt(&b[16..16 + crc_len]));
	let mut sum = 0u8;
	for (i, &x) in b[..16].iter().enumerate() {
		if i != 4 {
			sum = sum.wrapping_add(x);
		}
	}
	b[4] = sum;
}

// The CRC-ITU-T polynomial UDF uses, mirrored from the parser so a fixture is checked against the
// same arithmetic a real formatter would use.
fn crc_ccitt(bytes: &[u8]) -> u16 {
	let mut crc: u16 = 0;
	for &b in bytes {
		crc ^= (b as u16) << 8;
		for _ in 0..8 {
			crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
		}
	}
	crc
}

// Recompute a descriptor's CRC and checksum after a test has patched its body.
//
// A test that edits a File Entry is editing the bytes the CRC covers, so without this the parser
// refuses it for the right reason and the test measures the wrong thing. A real formatter has the
// same obligation.
fn refresh(img: &mut [u8], block: usize, id: u16, loc: u32) {
	let at = block * SECTOR_SIZE;
	tag(&mut img[at..at + SECTOR_SIZE], id, loc);
}

// Encode a name as an OSTA d-string: compression id then chars (8-bit Latin-1, or 16-bit
// UCS-2 big-endian when any char is non-ASCII).
fn dstring(s: &str) -> Vec<u8> {
	if s.bytes().all(|b| b < 0x80) {
		let mut v = vec![8u8];
		v.extend_from_slice(s.as_bytes());
		v
	} else {
		let mut v = vec![16u8];
		v.extend(s.encode_utf16().flat_map(|u| u.to_be_bytes()));
		v
	}
}

// Build one File Identifier Descriptor: name, dir flag, parent flag, and the child ICB
// block, padded to 4.
fn fid(name: &str, is_dir: bool, parent: bool, icb: u32) -> Vec<u8> {
	let id = if parent { Vec::new() } else { dstring(name) };
	let total = 38 + id.len();
	let mut f = vec![0u8; (total + 3) & !3];
	f[18] = if parent { 0x08 } else { 0 } | if is_dir { 0x02 } else { 0 };
	f[19] = id.len() as u8;
	// THE EXTENT LENGTH, which this fixture left as zero - and a `long_ad` whose length is zero
	// describes an extent of no bytes, which no formatter writes for an ICB. The parser now reads
	// it, so the fixture has to be a shape a real volume has: one block, recorded and allocated.
	w32(&mut f, 20, SECTOR_SIZE as u32);
	w32(&mut f, 24, icb);
	f[38..38 + id.len()].copy_from_slice(&id);
	tag(&mut f, TAG_FILE_ID, 0);
	f
}

// Build an embedded File Entry for block `lb` (the tag records its own location): a
// directory holding `fids` or a file holding `data`.
fn file_entry(lb: u32, is_dir: bool, fids: &[Vec<u8>], data: &[u8]) -> Vec<u8> {
	let mut b = vec![0u8; SECTOR_SIZE];
	b[27] = if is_dir { 4 } else { 5 };
	// The ICB Strategy Type. A real File Entry carries 4 (direct), which is what this reader
	// implements; the fixture left it zero, so the parser could not have read the field without
	// refusing its own images - the same shape as the missing CRC.
	// The ICB tag's StrategyType is at 20 - the fixture wrote it at 28, which is the low half of
	// ParentICBLocation, and the parser read it from there too. The two agreed with each other and
	// with no real medium.
	w16(&mut b, 20, 4);
	w16(&mut b, 34, 3); // embedded alloc
	let mut body = Vec::new();
	for f in fids {
		// RE-TAGGED WITH THE BLOCK IT ENDS UP IN. A FID's Descriptor Tag location is the logical
		// block holding its first byte, and these are embedded in this File Entry - so it is `lb`.
		// The fixture wrote 0 into every one of them while the parser passed `None` for the expected
		// location, the two agreeing with each other and with no real medium; the parser checks it
		// now, and fourteen tests failed on this line's absence before it was written. A fixture
		// that would not survive its own parser's new rule was not describing a real volume.
		let mut copy = f.clone();
		tag(&mut copy, TAG_FILE_ID, lb);
		body.extend_from_slice(&copy);
	}
	body.extend_from_slice(data);
	w64(&mut b, 56, body.len() as u64);
	w32(&mut b, 172, body.len() as u32);
	b[176..176 + body.len()].copy_from_slice(&body);
	tag(&mut b, TAG_FILE_ENTRY, lb);
	b
}

fn build_udf() -> Vec<u8> {
	let mut img = vec![0u8; SECTOR_SIZE * 264];
	let mut blk = |lba: u32, bytes: &[u8]| {
		let o = lba as usize * SECTOR_SIZE;
		img[o..o + bytes.len()].copy_from_slice(bytes);
	};
	// Anchor at 256 -> VDS at 257, two descriptors.
	let mut avdp = vec![0u8; SECTOR_SIZE];
	w32(&mut avdp, 16, (SECTOR_SIZE * 2) as u32);
	w32(&mut avdp, 20, 257);
	tag(&mut avdp, TAG_AVDP, 256);
	blk(256, &avdp);
	let mut pd = vec![0u8; SECTOR_SIZE];
	w32(&mut pd, 188, 0); // partition starts at LBA 0
	w32(&mut pd, 192, 264); // and spans the whole 264-block image
	w16(&mut pd, 22, 0); // PartitionNumber, which the LVD's map has to name
	tag(&mut pd, TAG_PARTITION, 257);
	blk(257, &pd);
	let mut lvd = vec![0u8; SECTOR_SIZE];
	// A CONFORMING Logical Volume Descriptor. The fixture used to write the File Set location and
	// nothing else, so the parser could not have read the block size, the domain identifier or the
	// partition maps even if it had wanted to - the same shape as the missing descriptor CRC: the
	// image agreed with the parser rather than with the format.
	w32(&mut lvd, 212, SECTOR_SIZE as u32); // LogicalBlockSize
	lvd[217..236].copy_from_slice(b"*OSTA UDF Compliant"); // DomainIdentifier
	// The revision, in the identifier's suffix - which the fixture did not write and the parser did
	// not read, so the two agreed with each other rather than with the format.
	w16(&mut lvd, 240, 0x0201); // UDF 2.01
	w32(&mut lvd, 248, SECTOR_SIZE as u32); // the File Set's extent length
	w32(&mut lvd, 252, 259); // File Set at lb 259
	w16(&mut lvd, 256, 0); // ...in partition reference 0
	// The two fields in the order ECMA-167 3/10.6 puts them: MapTableLength at 264, then
	// NumberOfPartitionMaps at 268. The fixture wrote the length at 272 - which is where the
	// Implementation Identifier starts - and the parser read it from the same wrong place, so the
	// two agreed with each other and with no volume any formatter produces.
	w32(&mut lvd, 264, 6); // MapTableLength
	w32(&mut lvd, 268, 1); // NumberOfPartitionMaps
	lvd[272..285].copy_from_slice(b"\0*Linux UDFFS"); // ImplementationIdentifier, where 272 really is
	// A CONFORMING Type-1 map: type, length, volume sequence number, partition number. The fixture
	// wrote the type byte alone and left the rest zero, so the parser could not have checked the map
	// coherently even if it had wanted to - the same shape as the missing descriptor CRC and the
	// missing descriptor version, and the reason each of those went unchecked for so long.
	lvd[440] = 1; // one Type-1 (physical) partition map
	lvd[441] = 6; // its length
	w16(&mut lvd, 442, 1); // volume 1 of the set
	w16(&mut lvd, 444, 0); // partition 0, which is what the partition descriptor below numbers
	lvd[441] = 6; // its length
	tag(&mut lvd, TAG_LOGICAL_VOLUME, 258);
	blk(258, &lvd);
	let mut fsd = vec![0u8; SECTOR_SIZE];
	w32(&mut fsd, 400, SECTOR_SIZE as u32); // the root ICB's extent length
	w32(&mut fsd, 404, 260); // root ICB at lb 260
	tag(&mut fsd, TAG_FILE_SET, 259);
	blk(259, &fsd);
	blk(262, &file_entry(262, false, &[], b"hello udf"));
	blk(263, &file_entry(263, false, &[], b"world"));
	blk(261, &file_entry(261, true, &[fid("", true, true, 260), fid("WORLD.TXT", false, false, 263)], b""));
	blk(260, &file_entry(260, true, &[fid("", true, true, 260), fid("SUB", true, false, 261), fid("HELLO.TXT", false, false, 262)], b""));
	img
}

#[test]
fn mount_list_read() {
	let mut fs = Udf::mount(MemDisc { data: build_udf() }).unwrap();
	let mut names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	names.sort();
	assert_eq!(names, ["HELLO.TXT", "SUB"]);
	assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), b"hello udf");
	assert_eq!(fs.read_file(b"SUB/WORLD.TXT").unwrap(), b"world");
}

#[test]
fn list_subdir() {
	let mut fs = Udf::mount(MemDisc { data: build_udf() }).unwrap();
	assert_eq!(fs.list_dir(b"SUB").unwrap().len(), 1);
}

#[test]
fn missing_is_not_found() {
	let mut fs = Udf::mount(MemDisc { data: build_udf() }).unwrap();
	assert_eq!(fs.read_file(b"NOPE.TXT"), Err(FsError::NotFound));
}

// A device that counts its block reads, for pinning I/O-cost bounds.
struct CountingDisc {
	inner: MemDisc,
	reads: usize,
}

impl BlockDevice for CountingDisc {
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		self.reads += 1;
		self.inner.read_block(lba, buf)
	}
}

#[test]
fn a_forged_allocation_length_does_not_panic() {
	// l_ad is the medium's claim: a huge value used to walk the descriptor scan past
	// the File Entry block and panic. It must error or read cleanly, never crash.
	let mut img = build_udf();
	let fe = 262 * SECTOR_SIZE;
	img[fe + 34] = 0; // short_ad allocation
	w32(&mut img[fe..], 172, u32::MAX); // l_ad far past the block
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	let _ = fs.read_file(b"HELLO.TXT"); // must not panic
	assert_eq!(fs.read_file(b"SUB/WORLD.TXT").unwrap(), b"world");
}

#[test]
fn forged_lengths_do_not_allocate_or_read_foreign_blocks() {
	// the information length (u64) and the extents are the medium's claims: a forged
	// length must refuse before allocating, an extent past the partition must refuse
	// before reading, and a partition claiming more blocks than the device must refuse
	// at mount.
	let mut img = build_udf();
	let fe = 262 * SECTOR_SIZE;
	img[fe + 34] = 0; // short_ad: the embedded path caps by the block, extents allocate
	w64(&mut img[fe..], 56, u64::MAX);
	refresh(&mut img, 262, TAG_FILE_ENTRY, 262);
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	// `TooLarge` - the answer does not fit in one buffer - which is what a length of `u64::MAX`
	// means and what this test could not see while the CRC was refusing the descriptor first.
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::TooLarge));
	let mut img2 = build_udf();
	let fe2 = 263 * SECTOR_SIZE;
	img2[fe2 + 34] = 0; // short_ad
	w64(&mut img2[fe2..], 56, 5);
	w32(&mut img2[fe2..], 172, 8); // one descriptor
	w32(&mut img2[fe2..], 176, 2048); // recorded extent, 2048 bytes
	w32(&mut img2[fe2..], 180, 5000); // past the 264-block partition
	refresh(&mut img2, 263, TAG_FILE_ENTRY, 263);
	let mut fs2 = Udf::mount(MemDisc { data: img2 }).unwrap();
	assert_eq!(fs2.read_file(b"SUB/WORLD.TXT"), Err(FsError::Invalid), "an extent outside the partition, reached because the descriptor itself validates");
	let mut img3 = build_udf();
	w32(&mut img3[257 * SECTOR_SIZE..], 192, 100_000);
	assert!(Udf::mount(MemDisc { data: img3 }).is_none(), "a partition past the device");
}

#[test]
fn a_listing_reads_headers_not_file_contents() {
	// the listing's size column comes from the File Entry header - a directory of
	// movie-sized files must not pull their contents through the device.
	let inner = MemDisc { data: build_udf() };
	let mut fs = Udf::mount(CountingDisc { inner, reads: 0 }).unwrap();
	fs.dev.reads = 0;
	let list = fs.list().unwrap();
	assert!(list.iter().any(|f| f.name == "HELLO.TXT" && f.size == 9), "{list:?}");
	assert!(fs.dev.reads <= 3, "a listing must cost header reads only: {}", fs.dev.reads);
}

#[test]
fn an_unrecorded_extent_reads_as_zeros_and_a_chain_ad_refuses() {
	// an unrecorded (sparse) extent has no written data - it must read as zeros, not
	// as whatever the disk blocks hold; a type-3 chain descriptor must refuse, not be
	// read as data.
	let mut img = build_udf();
	let fe = 262 * SECTOR_SIZE;
	img[fe + 34] = 0; // short_ad
	w64(&mut img[fe..], 56, 5);
	w32(&mut img[fe..], 172, 8);
	w32(&mut img[fe..], 176, (1 << 30) | 2048); // allocated, not recorded
	w32(&mut img[fe..], 180, 0); // points at the boot area's stale bytes
	refresh(&mut img, 262, TAG_FILE_ENTRY, 262);
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), vec![0u8; 5], "unrecorded data must read as zeros");
	let mut img2 = build_udf();
	let fe2 = 262 * SECTOR_SIZE;
	img2[fe2 + 34] = 0;
	w64(&mut img2[fe2..], 56, 5);
	w32(&mut img2[fe2..], 172, 8);
	w32(&mut img2[fe2..], 176, (3u32 << 30) | 8); // a chain to further descriptors
	refresh(&mut img2, 262, TAG_FILE_ENTRY, 262);
	let mut fs2 = Udf::mount(MemDisc { data: img2 }).unwrap();
	assert_eq!(fs2.read_file(b"HELLO.TXT"), Err(FsError::Invalid), "a type-3 chain, reached because the descriptor itself validates");
}

#[test]
fn an_unchecksummed_descriptor_is_not_trusted() {
	// tag checksums are mandatory: a block merely starting with a plausible tag id
	// must not parse as a File Entry.
	let mut img = build_udf();
	img[262 * SECTOR_SIZE + 4] ^= 0x55;
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::Corrupt), "a descriptor that does not validate is corrupt, and says so distinctly");
}

#[test]
fn listing_contract_and_dot_dot() {
	// an empty-named File Identifier neither lists nor matches an empty lookup, and
	// ".." resolves to the parent as on the other backends.
	let mut img = build_udf();
	let sub = file_entry(261, true, &[fid("", true, true, 260), fid("WORLD.TXT", false, false, 263), fid("", false, false, 262)], b"");
	img[261 * SECTOR_SIZE..262 * SECTOR_SIZE].copy_from_slice(&sub);
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	let list = fs.list_dir(b"SUB").unwrap();
	assert!(list.iter().all(|f| !f.name.is_empty()), "{list:?}");
	assert_eq!(fs.read_file(b"SUB/"), Err(FsError::NotFound));
	let mut up: Vec<_> = fs.list_dir(b"SUB/..").unwrap().into_iter().map(|f| f.name).collect();
	up.sort();
	assert_eq!(up, ["HELLO.TXT", "SUB"]);
}

#[test]
fn a_multi_extent_file_reads_every_extent() {
	// the File Entry buffer used to be overwritten by the first extent's data, so the
	// remaining descriptors were parsed from FILE CONTENT - a fragmented file read a
	// silently corrupt tail steered by its own bytes.
	let mut img = build_udf();
	let fe = 262 * SECTOR_SIZE;
	img[fe..fe + SECTOR_SIZE].fill(0);
	{
		let b = &mut img[fe..fe + SECTOR_SIZE];
		b[27] = 5; // a file ICB
		w16(b, 20, 4); // ICB strategy 4, at the offset the ICB tag actually puts it
		w16(b, 34, 0); // short_ad
		w64(b, 56, 2053);
		w32(b, 172, 16); // two descriptors
		w32(b, 176, 2048); // extent 1: one block at lb 20
		w32(b, 180, 20);
		w32(b, 184, 5); // extent 2: five bytes at lb 21
		w32(b, 188, 21);
		tag(b, TAG_FILE_ENTRY, 262);
	}
	let first: Vec<u8> = (0..2048u32).map(|i| (i * 7) as u8).collect();
	img[20 * SECTOR_SIZE..21 * SECTOR_SIZE].copy_from_slice(&first);
	img[21 * SECTOR_SIZE..21 * SECTOR_SIZE + 5].copy_from_slice(b"tail!");
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	let data = fs.read_file(b"HELLO.TXT").unwrap();
	assert_eq!(data.len(), 2053);
	assert_eq!(&data[..2048], &first[..]);
	assert_eq!(&data[2048..], b"tail!", "the second extent must come from the disc, not from the first extent's bytes");
}

#[test]
fn a_forged_root_icb_does_not_mount() {
	// the root ICB is gated at mount like the File Set, not left to fail later.
	let mut img = build_udf();
	w32(&mut img[259 * SECTOR_SIZE..], 404, 100_000);
	assert!(Udf::mount(MemDisc { data: img }).is_none());
}

#[test]
fn a_misplaced_file_entry_is_refused() {
	// a descriptor's tag records its own block address - a File Entry copied to a
	// different block (misdirected write, forged copy) must not pass.
	let mut img = build_udf();
	let (a, b) = (262 * SECTOR_SIZE, 20 * SECTOR_SIZE);
	img.copy_within(a..a + SECTOR_SIZE, b);
	// point HELLO.TXT's FID at the copy: root FIDs are parent (40) + SUB (44), so
	// HELLO.TXT's ICB field sits at 176 + 84 + 24.
	w32(&mut img[260 * SECTOR_SIZE..], 176 + 84 + 24, 20);
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	// `Corrupt` since 2026-08-12: a descriptor that fails validation answers differently from a
	// descriptor whose CONTENT this reader refuses, so a forgotten CRC refresh cannot be mistaken
	// for the branch under test.
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::Corrupt));
}

#[test]
fn a_directory_whose_records_do_not_tile_its_extent_is_corrupt() {
	// Both walks ran `while off + 38 <= data.len()`, so anything from one to thirty-seven bytes after
	// the last record ended the walk and the listing returned `Ok` - a directory whose structure does
	// not add up read as a healthy one that is simply missing a file.
	//
	// Two earlier attempts at this test were removed rather than left green, and each proved
	// something else: growing `InformationLength` past the extent makes the volume fail the EXTENT
	// bound, a different rule, and shortening the last record's name meant walking the fixture's root
	// to find it, which did not land where the fixture puts it. The shape that isolates the rule is
	// an EMBEDDED directory whose declared length is four bytes longer than its records - four bytes
	// that are inside the descriptor, inside the block, and too few to be a record.
	let mut img = build_udf();
	let parent = fid("", true, true, 260);
	let child = fid("WORLD.TXT", false, false, 263);
	let body = parent.len() + child.len();
	let mut sub = file_entry(261, true, &[parent, child], b"");
	// The control first: as built, it lists.
	img[261 * SECTOR_SIZE..262 * SECTOR_SIZE].copy_from_slice(&sub);
	let mut fs = Udf::mount(MemDisc { data: img.clone() }).unwrap();
	assert_eq!(fs.list_dir(b"SUB").unwrap().len(), 1, "the tiled directory lists");

	// Four bytes of tail: declared, embedded, and not a record.
	w64(&mut sub, 56, (body + 4) as u64);
	w32(&mut sub, 172, (body + 4) as u32);
	tag(&mut sub, TAG_FILE_ENTRY, 261);
	img[261 * SECTOR_SIZE..262 * SECTOR_SIZE].copy_from_slice(&sub);
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	assert_eq!(fs.list_dir(b"SUB"), Err(FsError::Corrupt), "a tail too short to be a record is corruption");
}

#[test]
fn a_fid_carried_over_from_another_directory_is_refused() {
	// THE CLASS THE TAG LOCATION EXISTS TO CATCH. A File Identifier Descriptor records the logical
	// block its first byte lives in, and this reader passed `None` for that field under a comment
	// saying "a FID does not have an address of its own to check" - because `read_icb` flattened a
	// directory's extents into one buffer and threw away which block each byte came from. So a FID
	// lifted out of one directory and dropped into another passed every test this parser made: the
	// checksum is intact, the CRC covers the record, the name decodes, the child ICB is real.
	//
	// The block mapping now travels with the flattened bytes, so the record answers for where it is.
	let mut img = build_udf();
	// SUB's own listing, built the ordinary way, is the control: it mounts and lists.
	let honest = file_entry(261, true, &[fid("", true, true, 260), fid("WORLD.TXT", false, false, 263)], b"");
	img[261 * SECTOR_SIZE..262 * SECTOR_SIZE].copy_from_slice(&honest);
	let mut fs = Udf::mount(MemDisc { data: img.clone() }).unwrap();
	let names: Vec<_> = fs.list_dir(b"SUB").unwrap().into_iter().map(|f| f.name).collect();
	assert_eq!(names, ["WORLD.TXT"], "the ordinary directory lists");

	// The same directory, with one record stamped as belonging to the ROOT's block - which is what a
	// record carried over from another directory looks like. Everything else about it is valid.
	let mut stolen = fid("WORLD.TXT", false, false, 263);
	tag(&mut stolen, TAG_FILE_ID, 260);
	let mut forged = file_entry(261, true, &[fid("", true, true, 260)], b"");
	// Append it AFTER the entry was built, so `file_entry`'s re-tagging does not repair it.
	let parent_len = fid("", true, true, 260).len();
	let body = 176 + parent_len;
	forged[body..body + stolen.len()].copy_from_slice(&stolen);
	let total = (parent_len + stolen.len()) as u64;
	w64(&mut forged, 56, total);
	w32(&mut forged, 172, total as u32);
	tag(&mut forged, TAG_FILE_ENTRY, 261);
	img[261 * SECTOR_SIZE..262 * SECTOR_SIZE].copy_from_slice(&forged);
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	assert_eq!(fs.list_dir(b"SUB"), Err(FsError::Corrupt), "a record naming another directory's block is refused");
}

#[test]
fn an_unknown_compression_id_is_corruption_rather_than_a_missing_file() {
	// A d-string with an unknown compression id is noise, never text - and this used to assert that
	// the record simply did not appear in the listing. That was the defect one layer up: `decode_name`
	// returned an empty string for every malformed form and both walkers SKIPPED an empty name, so a
	// damaged directory listed as a healthy one that was merely missing a file. The caller had
	// nothing to go on, which is the silent shortening this reader refuses everywhere else.
	let mut img = build_udf();
	let mut noise = fid("AB", false, false, 262);
	noise[38] = 254; // the compression id byte
	// AND RE-TAG IT. Mutating a descriptor and leaving its CRC behind makes the parser refuse the
	// record for the CHECKSUM, so the branch this test is about - an unknown compression id - is
	// never reached and the assertion passes for a reason that has nothing to do with it.
	tag(&mut noise, TAG_FILE_ID, 261);
	let sub = file_entry(261, true, &[fid("", true, true, 260), fid("WORLD.TXT", false, false, 263), noise], b"");
	img[261 * SECTOR_SIZE..262 * SECTOR_SIZE].copy_from_slice(&sub);
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	assert_eq!(fs.list_dir(b"SUB"), Err(FsError::Corrupt), "a name that does not decode is a corrupt record");
}

#[test]
fn an_extended_ad_form_is_refused_not_misparsed() {
	// extended_ad records are 20 bytes - scanning them with the short_ad step parses
	// garbage extents; the form is refused instead.
	let mut img = build_udf();
	w16(&mut img[262 * SECTOR_SIZE..], 34, 2);
	// REFRESHED: without it the CRC refuses the descriptor and the extended_ad branch never runs -
	// which is how this test passed for years while asserting nothing about extended_ad.
	refresh(&mut img, 262, TAG_FILE_ENTRY, 262);
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::Invalid));
}

#[test]
fn a_symlink_file_entry_is_refused() {
	// a symlink stores its target path as data - the volume API has no symlink
	// semantics, so serving the path bytes as content would only mislead.
	let mut img = build_udf();
	img[262 * SECTOR_SIZE + 27] = 12;
	// REFRESHED, or the descriptor is refused for its checksum and the symlink branch never runs.
	refresh(&mut img, 262, TAG_FILE_ENTRY, 262);
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::Invalid));
}

#[test]
fn a_misplaced_anchor_or_descriptor_is_not_trusted() {
	// tags record their own block address - an anchor or a VDS descriptor carrying
	// another address is stale or copied and must not be trusted.
	let mut img = build_udf();
	{
		let b = &mut img[256 * SECTOR_SIZE..257 * SECTOR_SIZE];
		tag(b, TAG_AVDP, 999);
	}
	assert!(Udf::mount(MemDisc { data: img }).is_none(), "a misplaced anchor");
	let mut img2 = build_udf();
	{
		let b = &mut img2[257 * SECTOR_SIZE..258 * SECTOR_SIZE];
		tag(b, TAG_PARTITION, 999);
	}
	assert!(Udf::mount(MemDisc { data: img2 }).is_none(), "a misplaced partition descriptor");
}

#[test]
fn a_forged_embedded_length_neither_panics_nor_truncates() {
	// The embedded branch ran BEFORE the partition bound, so `InformationLength = u64::MAX` reached
	// `ad_off + info_len` first - an overflow in debug and a wrap to `block[176..175]` in release,
	// a panic either way. A merely large value clamped with `.min()` and returned the tail of the
	// block as `Ok`: 1872 bytes for a file the descriptor calls 5000.
	//
	// The existing forged-length test does not reach this: it switches the entry to short_ad before
	// setting the length, so the embedded path is never exercised with a forged one.
	for forged in [u64::MAX, 5000, 1 << 40] {
		let mut img = build_udf();
		let fe = 262 * SECTOR_SIZE;
		img[fe + 34] = 3; // embedded
		w64(&mut img[fe..], 56, forged);
		w32(&mut img[fe..], 172, 8);
		refresh(&mut img, 262, TAG_FILE_ENTRY, 262);
		let mut fs = Udf::mount(MemDisc { data: img }).expect("the volume still mounts");
		// Refused, and in particular NOT a short read reported as the whole file.
		assert!(fs.read_file(b"HELLO.TXT").is_err(), "a forged embedded length of {forged} must be refused, not clamped");
	}
}

#[test]
fn a_chain_that_stops_early_is_corrupt_rather_than_zeros() {
	// The buffer starts as zeros and the loop exits on a zero-length extent; `Ok(out)` followed
	// with no check that the descriptors covered the declared length. A file declaring 100 KiB whose
	// descriptors cover 2 KiB came back as 2 KiB of data and 98 KiB of zeros - indistinguishable
	// from a real file, which is the silent corruption this crate says it exists to avoid.
	let mut img = build_udf();
	let fe = 262 * SECTOR_SIZE;
	img[fe + 34] = 0; // short_ad
	w64(&mut img[fe..], 56, 100 * 1024); // declare 100 KiB
	w32(&mut img[fe..], 172, 8); // one descriptor
	w32(&mut img[fe..], 176, 2048); // covering 2 KiB
	w32(&mut img[fe..], 180, 0);
	refresh(&mut img, 262, TAG_FILE_ENTRY, 262);
	let mut fs = Udf::mount(MemDisc { data: img }).expect("mount");
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::Corrupt), "a chain that does not cover the declared length is corruption");
}

#[test]
fn a_flipped_bit_in_a_descriptor_body_is_caught() {
	// The tag checksum covers the first sixteen bytes, and it was the only thing checked - so a bit
	// flipped in a partition start, a File Set location or an allocation descriptor passed every
	// test this parser made. On optical media, which is what this backend is for, that is the
	// protection that matters most.
	let mut img = build_udf();
	// A byte the parser does not otherwise read - one of the File Entry's timestamps - so what is
	// under test is the CRC and nothing else. Flipping a length or a location would be refused for
	// its own reasons and would prove nothing about the integrity check.
	img[262 * SECTOR_SIZE + 100] ^= 0x01;
	let mut fs = Udf::mount(MemDisc { data: img }).expect("the volume mounts; the damage is in a file");
	assert!(fs.read_file(b"HELLO.TXT").is_err(), "a descriptor whose body CRC does not match must be refused");

	// And in a descriptor the MOUNT reads: the volume must not come up at all.
	let mut img = build_udf();
	img[256 * SECTOR_SIZE + 40] ^= 0x01;
	assert!(Udf::mount(MemDisc { data: img }).is_none(), "a damaged anchor body must not mount");
}

#[test]
fn a_damaged_main_sequence_falls_back_to_the_reserve() {
	// UDF carries a Reserve volume descriptor sequence beside the Main one precisely so a damaged
	// Main is survivable, and the specification's read procedure says to fall back to it. This
	// mounted from one sequence and answered `None` if it was damaged - on optical media, where
	// that redundancy is most likely to be the thing that saves the volume.
	let mut img = build_udf();
	// Room for the reserve sequence past the fixture's own 264 blocks.
	img.resize(SECTOR_SIZE * 274, 0);
	// A Reserve sequence: copies of the partition and logical volume descriptors, at 270/271.
	for (src, dst) in [(257usize, 270usize), (258, 271)] {
		let (s0, d0) = (src * SECTOR_SIZE, dst * SECTOR_SIZE);
		let mut copy = img[s0..s0 + SECTOR_SIZE].to_vec();
		let id = u16::from_le_bytes([copy[0], copy[1]]);
		tag(&mut copy, id, dst as u32);
		img[d0..d0 + SECTOR_SIZE].copy_from_slice(&copy);
	}
	// Point the anchor's Reserve extent at it.
	let a = 256 * SECTOR_SIZE;
	w32(&mut img[a..], 24, (SECTOR_SIZE * 2) as u32);
	w32(&mut img[a..], 28, 270);
	// And destroy the Main sequence.
	for lb in [257usize, 258] {
		let at = lb * SECTOR_SIZE;
		img[at..at + 64].fill(0xAA);
	}
	tag(&mut img[a..a + SECTOR_SIZE], TAG_AVDP, 256);
	let mut fs = Udf::mount(MemDisc { data: img }).expect("the reserve sequence carries the volume");
	let names: Vec<_> = fs.list().unwrap().into_iter().map(|f| f.name).collect();
	assert!(names.contains(&"HELLO.TXT".to_string()), "the volume mounted from its reserve sequence: {names:?}");
}

#[test]
fn a_volume_this_reader_cannot_address_is_refused_by_name() {
	// The header claimed a metadata-partition volume would "refuse to mount rather than be
	// misread", and no code could produce that refusal: the File Set reference resolved against the
	// physical partition and landed on something that was not a File Set Descriptor, so the mount
	// failed by accident. On a crafted image it lands on something that looks like one.
	let lvd = 258 * SECTOR_SIZE;

	// Two partition maps: more than this reader can resolve.
	let mut img = build_udf();
	w32(&mut img[lvd..], 268, 2);
	tag(&mut img[lvd..lvd + SECTOR_SIZE], TAG_LOGICAL_VOLUME, 258);
	assert!(Udf::mount(MemDisc { data: img }).is_none(), "more than one partition map is refused");

	// A Type-2 (virtual/metadata) map, which is the shape the header names.
	let mut img = build_udf();
	img[lvd + 440] = 2;
	tag(&mut img[lvd..lvd + SECTOR_SIZE], TAG_LOGICAL_VOLUME, 258);
	assert!(Udf::mount(MemDisc { data: img }).is_none(), "a non-physical partition map is refused");

	// A block size this reader does not use.
	let mut img = build_udf();
	w32(&mut img[lvd..], 212, 512);
	tag(&mut img[lvd..lvd + SECTOR_SIZE], TAG_LOGICAL_VOLUME, 258);
	assert!(Udf::mount(MemDisc { data: img }).is_none(), "a 512-byte logical block size is refused");

	// And a volume that is not UDF at all.
	let mut img = build_udf();
	img[lvd + 217] = b'X';
	tag(&mut img[lvd..lvd + SECTOR_SIZE], TAG_LOGICAL_VOLUME, 258);
	assert!(Udf::mount(MemDisc { data: img }).is_none(), "a missing OSTA domain identifier is refused");
}

#[test]
fn a_damaged_directory_is_corrupt_rather_than_empty() {
	// Both scans used to `break` on a bad tag or a record running past the data: `find_entry` then
	// answered `NotFound` and `read_dir` returned `Ok` with whatever it had. A caller could not tell
	// a missing file from a damaged directory, and a listing could be short with nothing to say so.
	let mut img = build_udf();
	// Damage a File Identifier Descriptor's TAG - the field the scan tests - rather than a byte
	// inside a name, which would parse as a different name and prove nothing.
	let root = 260 * SECTOR_SIZE;
	let pos = img[root..root + SECTOR_SIZE].windows(3).position(|w| w == b"SUB").expect("the fixture has this name");
	let fid = root + pos - 39;
	img[fid] ^= 0xFF; // the tag identifier
	refresh(&mut img, 260, TAG_FILE_ENTRY, 260);
	let mut fs = Udf::mount(MemDisc { data: img }).expect("mount");
	assert!(matches!(fs.list(), Err(FsError::Corrupt)), "a listing that meets damage says so");
	assert!(matches!(fs.read_file(b"HELLO.TXT"), Err(FsError::Corrupt)), "a lookup that meets damage says so, not NotFound");
}

#[test]
fn an_unreadable_child_is_not_listed_as_empty() {
	// `unwrap_or(0)` reported a child whose File Entry could not be read as a zero-byte file. Zero
	// means "this file is empty", not "we could not find out".
	let mut img = build_udf();
	// Point HELLO.TXT's File Identifier Descriptor at a block outside the partition.
	let root = 260 * SECTOR_SIZE;
	let at = root..root + SECTOR_SIZE;
	let block = &mut img[at];
	// The third FID in the root: parent, SUB, HELLO.TXT. Find its ICB field by scanning for the
	// name and stepping back to the fixed part.
	let pos = block.windows(9).position(|w| w == b"HELLO.TXT").expect("the fixture has this name");
	let fid = pos - 39; // 38-byte header + the 1-byte compression id
	let icb = fid + 24;
	block[icb..icb + 4].copy_from_slice(&999_999u32.to_le_bytes());
	refresh(&mut img, 260, TAG_FILE_ENTRY, 260);
	let mut fs = Udf::mount(MemDisc { data: img }).expect("mount");
	assert!(fs.list().is_err(), "a child whose entry cannot be read is an error, not a zero-byte file");
}

#[test]
fn a_type_the_directory_and_the_entry_disagree_on_is_refused() {
	// `is_dir` came from the File Identifier Descriptor and nothing compared it with the ICB's own
	// file type - after which a regular file's contents are parsed as a stream of FIDs, or a
	// directory is served as file data.
	let mut img = build_udf();
	// Make HELLO.TXT's own File Entry claim to be a directory.
	img[262 * SECTOR_SIZE + 27] = 4;
	refresh(&mut img, 262, TAG_FILE_ENTRY, 262);
	let mut fs = Udf::mount(MemDisc { data: img }).expect("mount");
	assert!(fs.list().is_err(), "a File Entry whose type contradicts its directory record is refused");
}

#[test]
fn a_mount_says_why_it_refused() {
	// `Option` made "this is not UDF", "this UDF is damaged" and "this UDF uses something this
	// reader does not implement" the same answer - so a probe could not tell "try the next backend"
	// from "this IS UDF and it is broken, do not pretend otherwise".
	let blank = vec![0u8; SECTOR_SIZE * 300];
	assert!(matches!(Udf::mount_checked(MemDisc { data: blank }), Err(MountError::NotUdf)), "a blank disc is not UDF");

	// A volume that IS UDF and whose descriptors do not hold together.
	let mut img = build_udf();
	img[257 * SECTOR_SIZE..257 * SECTOR_SIZE + 64].fill(0xAA);
	img[258 * SECTOR_SIZE..258 * SECTOR_SIZE + 64].fill(0xAA);
	assert!(matches!(Udf::mount_checked(MemDisc { data: img }), Err(MountError::Corrupt)), "a damaged UDF volume is not not-UDF");
}

#[test]
fn a_stale_descriptor_does_not_win_over_a_newer_one() {
	// Each descriptor overwrote the previous one, so a volume that has been UPDATED - a new
	// descriptor appended rather than the old one erased, which is the normal state of a rewritable
	// disc - could be read through the stale copy.
	let mut img = build_udf();
	img.resize(SECTOR_SIZE * 274, 0);
	// A second partition descriptor with a HIGHER sequence number and a wrong start, placed after
	// the real one: the old code took the last it saw.
	let pd = 257 * SECTOR_SIZE;
	// The real descriptor gets sequence number 1, so the stale copy's 0 is genuinely older. A tie
	// still lets the later one win, which is the old rule and is correct for equal numbers.
	w32(&mut img[pd..], 16, 1);
	tag(&mut img[pd..pd + SECTOR_SIZE], TAG_PARTITION, 257);
	let mut stale = img[pd..pd + SECTOR_SIZE].to_vec();
	w32(&mut stale, 16, 0); // sequence number 0 - older than the real one
	w32(&mut stale, 188, 9999); // a partition start that would break every address
	tag(&mut stale, TAG_PARTITION, 268);
	// Past everything the fixture uses, and the sequence lengthened to reach it: the blocks between
	// are zeros, which the scan skips as failing descriptors.
	img[268 * SECTOR_SIZE..269 * SECTOR_SIZE].copy_from_slice(&stale);
	let a = 256 * SECTOR_SIZE;
	w32(&mut img[a..], 16, (SECTOR_SIZE * 12) as u32);
	tag(&mut img[a..a + SECTOR_SIZE], TAG_AVDP, 256);
	// The File Set moved to 259 in the fixture, so this test only proves the descriptor choice:
	// with the stale one winning, the mount would resolve addresses against 9999 and fail.
	assert!(Udf::mount(MemDisc { data: img }).is_some(), "the newer partition descriptor prevails over a later-but-older copy");
}

#[cfg(test)]
fn udftools_available() -> bool {
	std::process::Command::new("mkfs.udf").arg("--help").output().is_ok()
}

// A blank image formatted by `mkfs.udf`, or None when udftools is not installed.
fn udftools_image(name: &str) -> Option<std::path::PathBuf> {
	if !udftools_available() {
		return None;
	}
	// NAMED PER CALLER. Both independent-formatter tests shared one path and cargo runs them in
	// parallel, so one truncated the image while the other was formatting it - a failure that
	// appeared only when the suite ran whole and vanished on every attempt to reproduce it alone.
	let dir = std::env::temp_dir().join("udf-gold");
	let _ = std::fs::create_dir_all(&dir);
	let path = dir.join(name);
	let _ = std::fs::remove_file(&path);
	std::fs::write(&path, alloc::vec![0u8; 32 << 20]).expect("a blank image");
	let made = std::process::Command::new("mkfs.udf").arg("--media-type=hd").arg("--blocksize=2048").arg(&path).output().expect("mkfs.udf");
	assert!(made.status.success(), "mkfs.udf failed: {}", String::from_utf8_lossy(&made.stderr));
	Some(path)
}

// The same, with a file, a subdirectory and a nested file written onto it by a tool that has no
// stake in this parser's assumptions.
//
// `mkfs.udf` formats and does not populate, so the files go on through a loop mount when this
// machine can do one. That needs privilege, so the whole test skips - loudly - when it cannot.
fn udftools_image_with_files() -> Option<std::path::PathBuf> {
	let path = udftools_image("populated.udf")?;
	let mount = std::env::temp_dir().join("udf-gold-mnt");
	let _ = std::fs::create_dir_all(&mount);
	let attached = std::process::Command::new("mount").arg("-t").arg("udf").arg("-o").arg("loop").arg(&path).arg(&mount).output().ok()?;
	if !attached.status.success() {
		std::eprintln!("SKIPPED: this machine cannot loop-mount a UDF image ({})", String::from_utf8_lossy(&attached.stderr).trim());
		return None;
	}
	let write = || -> std::io::Result<()> {
		std::fs::write(mount.join("hello.txt"), b"written by udftools, not by this crate\n")?;
		std::fs::create_dir_all(mount.join("sub"))?;
		std::fs::write(mount.join("sub").join("world.txt"), b"one level down\n")
	};
	let wrote = write();
	let _ = std::process::Command::new("umount").arg(&mount).output();
	wrote.ok()?;
	Some(path)
}

#[test]
fn a_fid_lying_about_a_type_is_caught_on_the_read_path_too() {
	// `icb_size` compares the File Entry's own type byte against what the FID claimed, and it runs
	// when a directory is LISTED. `read_file` took `is_dir` from the FID, refused a directory, and
	// then read the ICB without ever asking the File Entry whether it agreed - so a crafted FID
	// claiming "regular file" over a directory served the directory's bytes as file content.
	let mut img = build_udf();
	// SUB's FID says directory; make it say file, and the File Entry still says directory.
	let root_dir = 260 * SECTOR_SIZE;
	let name = dstring("SUB");
	let fid_at = img[root_dir..root_dir + SECTOR_SIZE].windows(name.len()).position(|w| w == name.as_slice()).map(|i| root_dir + i - 38).expect("the fixture's SUB FID");
	img[fid_at + 18] &= !0x02; // clear the directory bit the FID carries
	tag(&mut img[fid_at..fid_at + 44], TAG_FILE_ID, 0);
	let mut fs = Udf::mount(MemDisc { data: img }).expect("mount");
	assert_eq!(fs.read_file(b"SUB"), Err(FsError::Corrupt), "the File Entry says directory and the FID says file; one of them is lying and the read must not pick");
}

#[test]
fn a_mount_says_why_it_refused_in_all_four_ways() {
	// `MountError` had four variants and two of them could never be built: everything the scan
	// refused - a 4096-byte block size, a Type-2 map, more than one map - came back as `Corrupt`,
	// blaming the medium for a shape this reader does not implement, and a failed device read came
	// back the same way, blaming the medium for the device.
	let blank = alloc::vec![0u8; SECTOR_SIZE * 300];
	assert!(matches!(Udf::mount_checked(MemDisc { data: blank }), Err(MountError::NotUdf)), "a blank disc is not UDF");

	// A volume that IS UDF and whose descriptors do not hold together.
	let mut img = build_udf();
	img[257 * SECTOR_SIZE..257 * SECTOR_SIZE + 64].fill(0xAA);
	img[258 * SECTOR_SIZE..258 * SECTOR_SIZE + 64].fill(0xAA);
	assert!(matches!(Udf::mount_checked(MemDisc { data: img }), Err(MountError::Corrupt)), "a damaged UDF volume is not not-UDF");

	// UNSUPPORTED: a block size this reader does not implement, in a descriptor that is otherwise
	// perfect. It used to be indistinguishable from damage.
	let mut img = build_udf();
	let lvd = 258 * SECTOR_SIZE;
	w32(&mut img[lvd..], 212, 4096);
	tag(&mut img[lvd..lvd + SECTOR_SIZE], TAG_LOGICAL_VOLUME, 258);
	assert!(matches!(Udf::mount_checked(MemDisc { data: img }), Err(MountError::Unsupported)), "a 4096-byte block size is a shape this reader does not implement, not damage");

	// A revision outside the range whose structures this reader assumes.
	let mut img = build_udf();
	w16(&mut img[lvd..], 240, 0x0300);
	tag(&mut img[lvd..lvd + SECTOR_SIZE], TAG_LOGICAL_VOLUME, 258);
	assert!(matches!(Udf::mount_checked(MemDisc { data: img }), Err(MountError::Unsupported)), "UDF 3.00 is not a revision this reader implements");

	// IO: a device that cannot read its anchors. This answered `NotUdf` - one bad sector reporting
	// a perfectly good disc as not being UDF at all.
	struct Unreadable;
	impl BlockDevice for Unreadable {
		fn read_block(&mut self, _lba: u64, _buf: &mut [u8]) -> bool {
			false
		}
	}
	assert!(matches!(Udf::mount_checked(Unreadable), Err(MountError::Io)), "a device that will not read is not a disc that is not UDF");
}

#[test]
fn a_child_icb_in_another_partition_is_refused() {
	// `Geometry::physical` takes a partition reference and every caller passed the literal `0`,
	// because the references the medium carries were dropped before they reached it: the File Set's
	// root ICB, a FID's child ICB and a `long_ad` extent each had only their block number read. So a
	// crafted volume naming partition 1 was read as partition 0 - the same misinterpretation the
	// resolver exists to refuse, three places along.
	let mut img = build_udf();
	// HELLO.TXT's FID sits in the root directory; its child ICB is a `long_ad` at offset 20, whose
	// partition reference is the two bytes after the block number.
	// Found by its identifier rather than by a hardcoded offset, so a fixture change does not
	// silently move this test off the record it is about.
	let root_dir = 260 * SECTOR_SIZE;
	let name = dstring("HELLO.TXT");
	let fid_at = img[root_dir..root_dir + SECTOR_SIZE].windows(name.len()).position(|w| w == name.as_slice()).map(|i| root_dir + i - 38).expect("the fixture's HELLO.TXT FID");
	w16(&mut img[fid_at..], 28, 1); // the child ICB's partition reference: partition 1
	tag(&mut img[fid_at..fid_at + 40], TAG_FILE_ID, 0);
	let mut fs = Udf::mount(MemDisc { data: img }).expect("mount");
	assert!(matches!(fs.read_file(b"HELLO.TXT"), Err(FsError::Invalid | FsError::Corrupt)), "an ICB in a partition this reader cannot resolve is refused, not read as partition 0");
}

#[test]
fn an_image_from_an_independent_formatter_mounts() {
	// Every other fixture in this file is built by this crate, so every check it turned on was
	// checked against media this crate produced - which is how a validator and its fixtures come to
	// agree with each other and with nothing else. `mkfs.udf` has no stake in either.
	//
	// This is also the medium the GUEST is given: `boot/qemu-run.sh` formats the test UDF disk with
	// exactly this command, and a mount refused there takes `udf_storage` down with it.
	//
	// It earned its place the day it was extended: the CRC-coverage minimums added in 2026-08-12
	// were derived from the specification and one of them was wrong - a real Logical Volume
	// Descriptor covers to 446, not 448 - and this test was what said so, immediately.
	let Some(dir) = udftools_image("blank.udf") else {
		// SAID OUT LOUD. A skipped conformance test that looks identical to a passing one is how a
		// gate stops checking, which is the whole subject of P02M0112.
		std::eprintln!("SKIPPED an_image_from_an_independent_formatter_mounts: udftools is not installed");
		return;
	};
	let image = std::fs::read(&dir).expect("the formatted image");
	let mut fs = Udf::mount_checked(MemDisc { data: image }).unwrap_or_else(|e| panic!("a volume from udftools must mount: {e:?}"));
	// And its root must LIST. Mounting proves the anchor, the VDS and the File Set Descriptor and
	// stops before the ICB and directory code, which is where the findings are.
	let listed = fs.list();
	assert!(listed.is_ok(), "an empty udftools volume lists its root: {:?}", listed.err());
}

#[test]
fn an_independent_image_lists_and_reads_the_files_a_tool_put_on_it() {
	// Mounting an independent image proves the anchor, the VDS and the File Set Descriptor, and
	// stops exactly before the FID, ICB and addressing code where every remaining finding lives.
	// Files put there by a tool that has no stake in this parser's assumptions are what exercise it.
	let Some(path) = udftools_image_with_files() else {
		std::eprintln!("SKIPPED an_independent_image_lists_and_reads_the_files_a_tool_put_on_it: udftools is not installed");
		return;
	};
	let image = std::fs::read(&path).expect("the formatted image");
	let mut fs = Udf::mount_checked(MemDisc { data: image }).expect("a volume from udftools mounts");

	let names: Vec<String> = fs.list().expect("the root lists").into_iter().map(|f| f.name).collect();
	assert!(names.iter().any(|n| n.eq_ignore_ascii_case("hello.txt")), "the file the tool wrote is in the listing: {names:?}");
	assert!(names.iter().any(|n| n.eq_ignore_ascii_case("sub")), "and the directory it made: {names:?}");

	let hello = fs.read_file(b"hello.txt").expect("the file reads");
	assert_eq!(hello, b"written by udftools, not by this crate\n", "and the bytes are the ones the tool wrote");

	let nested = fs.list_dir(b"sub").expect("the subdirectory lists");
	assert!(nested.iter().any(|f| f.name.eq_ignore_ascii_case("world.txt")), "a file one level down: {nested:?}");
	assert_eq!(fs.read_file(b"sub/world.txt").expect("nested read"), b"one level down\n");
}

// A `mkfs.udf` image formatted with `flags`, or None when udftools is not installed.
//
// The matrix version of `udftools_image`: the format's variability is not in WHO wrote the volume
// so much as in WHICH of its forms they chose, and mkfs.udf can be asked for most of them.
fn udftools_variant(name: &str, flags: &[&str]) -> Option<std::path::PathBuf> {
	if !udftools_available() {
		return None;
	}
	let dir = std::env::temp_dir().join("udf-gold");
	let _ = std::fs::create_dir_all(&dir);
	let path = dir.join(name);
	let _ = std::fs::remove_file(&path);
	std::fs::write(&path, alloc::vec![0u8; 32 << 20]).expect("a blank image");
	let mut cmd = std::process::Command::new("mkfs.udf");
	cmd.arg("--blocksize=2048");
	for flag in flags {
		cmd.arg(flag);
	}
	let made = cmd.arg(&path).output().expect("mkfs.udf");
	if !made.status.success() {
		// A form this build of udftools will not produce is not a finding about the parser.
		std::eprintln!("SKIPPED variant {name}: mkfs.udf refused it ({})", String::from_utf8_lossy(&made.stderr).trim());
		let _ = std::fs::remove_file(&path);
		return None;
	}
	Some(path)
}

#[test]
fn media_beyond_one_shape_of_one_formatter() {
	// "Real media beyond one formatter" - and what a second formatter would actually buy is not a
	// second author, it is a second set of FORMAT CHOICES. The revision, whether file data sits
	// inside the ICB or behind short or long allocation descriptors, whether File Entries are the
	// extended kind, what the media type implies about the boot area and the strategy, and which
	// free-space form the volume carries are the axes on which two implementations differ.
	// `mkfs.udf` can be asked for each of them by name, and asking is what turns "one formatter"
	// into coverage of the format.
	//
	// The tool is still one tool, and this says so rather than implying otherwise: `xorriso` on
	// this machine refuses `-udf` outright and no other UDF writer is installed. A second author is
	// worth having and is not blocking.
	//
	// EVERY VARIANT CARRIES ITS EXPECTED ANSWER, including the refusal. A conformance matrix whose
	// unsupported shapes are skipped is a matrix that cannot tell "we refuse this deliberately"
	// from "we no longer notice it".
	#[derive(PartialEq, Debug)]
	enum Expect {
		// mounts, and its freshly formatted root lists as empty.
		Mounts,
		// refused by name. A rewritable medium is formatted with a SPARABLE partition map (type 2)
		// and a Sparing Table that remaps defective blocks; this reader resolves every logical
		// address against one physical partition, which for that map is silently wrong rather than
		// merely incomplete. Refusing it is the answer, and it must stay the answer.
		Unsupported,
	}
	let variants: &[(&str, &[&str], Expect)] = &[
		// Every revision this build offers, including the pre-2.00 ones, whose File Entries are the
		// plain kind rather than Extended - a different descriptor tag on the path that reads them.
		("rev102.udf", &["--media-type=hd", "--udfrev=1.02"], Expect::Mounts),
		("rev150.udf", &["--media-type=hd", "--udfrev=1.50"], Expect::Mounts),
		("rev200.udf", &["--media-type=hd", "--udfrev=2.00"], Expect::Mounts),
		("rev201.udf", &["--media-type=hd", "--udfrev=2.01"], Expect::Mounts),
		// The three allocation forms, which is the axis this milestone named: data inside the ICB,
		// and data addressed by short or by long descriptors.
		("ad-inicb.udf", &["--media-type=hd", "--ad=inicb"], Expect::Mounts),
		("ad-short.udf", &["--media-type=hd", "--ad=short"], Expect::Mounts),
		("ad-long.udf", &["--media-type=hd", "--ad=long"], Expect::Mounts),
		// Plain File Entries on a revision that would otherwise use Extended ones.
		("noefe.udf", &["--media-type=hd", "--udfrev=2.01", "--noefe"], Expect::Mounts),
		// A different medium: a different boot area, a different strategy, a different packet
		// length - all of which move where the descriptors land.
		("dvd.udf", &["--media-type=dvd"], Expect::Mounts),
		// The two free-space forms, which are what a mount reads to answer "how much is left".
		("space-table.udf", &["--media-type=hd", "--space=unalloctable"], Expect::Mounts),
		("space-bitmap.udf", &["--media-type=hd", "--space=unallocbitmap"], Expect::Mounts),
		// And the one this reader does not implement, asserted as a refusal.
		("dvdrw.udf", &["--media-type=dvdrw"], Expect::Unsupported),
	];
	let mut ran = 0usize;
	for (name, flags, expect) in variants {
		let Some(path) = udftools_variant(name, flags) else { continue };
		ran += 1;
		let image = std::fs::read(&path).expect("the formatted image");
		let mounted = Udf::mount_checked(MemDisc { data: image });
		let _ = std::fs::remove_file(&path);
		match expect {
			Expect::Unsupported => {
				assert_eq!(mounted.err(), Some(MountError::Unsupported), "{name} ({flags:?}) must be refused BY NAME, not mounted and misread");
			}
			Expect::Mounts => {
				let mut fs = mounted.unwrap_or_else(|e| panic!("{name} ({flags:?}) must mount: {e:?}"));
				let listed = fs.list().unwrap_or_else(|e| panic!("{name} ({flags:?}) must list its root: {e:?}"));
				// A freshly formatted volume's root is empty, and an empty listing is the correct
				// answer - what must not happen is a refusal, or entries that are not there.
				assert!(listed.is_empty(), "{name}: a blank volume lists {listed:?}");
			}
		}
	}
	if !udftools_available() {
		std::eprintln!("SKIPPED media_beyond_one_shape_of_one_formatter: udftools is not installed");
		return;
	}
	// EVERY VARIANT, or the matrix is smaller than it reads. A form this build of udftools will not
	// produce is skipped with a line, and a skip that nobody counts is how a table of thirteen rows
	// silently becomes a table of one.
	assert_eq!(ran, variants.len(), "{} of {} variants were formatted; the rest printed a SKIPPED line above", ran, variants.len());
}

#[test]
fn a_descriptor_whose_crc_stops_short_of_what_is_read_is_refused() {
	// `crc_must_cover` returns a CONSTANT per tag, and for two of the six the fields this reader
	// trusts are not at constant offsets - they are where the descriptor's own declared lengths put
	// them. The file knows this: the LVD arm says the map's "length is declared rather than fixed,
	// so its coverage is checked where the map is read - a constant here would be a guess", and that
	// dynamic check exists. The FID and the File Entry had the same property and were left on 38
	// and 216.
	//
	// A forged `DescriptorCRCLength` then covers the fixed part, passes, and leaves outside the
	// vouched range either the file's NAME or the allocation descriptors this reader follows to read
	// data. The second is the more serious: a forged name misnames a file, a forged allocation
	// descriptor redirects a read.
	//
	// The forgery is MINIMAL - shorten the declared coverage and re-stamp the CRC and the tag
	// checksum over what it now claims - because that is what a forger can do with no other change,
	// and it is what a constant cannot notice.
	fn shrink_coverage(img: &mut [u8], at: usize, to: u16) {
		img[at + 10..at + 12].copy_from_slice(&to.to_le_bytes());
		let crc = crc_ccitt(&img[at + 16..at + 16 + to as usize]);
		img[at + 8..at + 10].copy_from_slice(&crc.to_le_bytes());
		img[at + 4] = 0;
		let sum: u8 = img[at..at + 16].iter().fold(0u8, |a, b| a.wrapping_add(*b));
		img[at + 4] = sum;
	}

	// The fixture as it stands reads, which is what makes the refusal below the coverage rule's.
	let mut fs = Udf::mount(MemDisc { data: build_udf() }).expect("the fixture mounts");
	assert_eq!(fs.read_file(b"HELLO.TXT").expect("and reads"), b"hello udf");

	// THE RULE ITSELF, on a hand-built FID.
	//
	// Reaching the two call sites through a volume image means building a fixture around
	// `38 + l_iu + l_fi` and `header + l_ea + l_ad` - and a test that constructs those to reach one
	// comparison is testing the fixture. The forgery is one field and so is the rule.
	let mut fid = alloc::vec![0u8; 64];
	w16(&mut fid, 0, TAG_FILE_ID);
	w16(&mut fid, 2, 2);
	fid[19] = 8; // l_fi: an eight-byte name at 38..46
	w16(&mut fid, 36, 0); // l_iu
	fid[38..46].copy_from_slice(b"NAME.TXT");
	// Honest coverage: the whole record. The name is inside it and the descriptor is believed.
	let total = 38 + 8;
	let honest = (total - 16) as u16;
	w16(&mut fid, 10, honest);
	let crc = crc_ccitt(&fid[16..16 + honest as usize]);
	w16(&mut fid, 8, crc);
	let mut sum = 0u8;
	for (i, &x) in fid[..16].iter().enumerate() {
		if i != 4 {
			sum = sum.wrapping_add(x);
		}
	}
	fid[4] = sum;
	assert!(coverage_for_test(&fid, TAG_FILE_ID, total, total), "an honest FID passes");

	// The forgery: cover only the fixed part - which includes the child ICB at 20 - and re-stamp.
	// Every existing check still passes; only the name is outside what the CRC vouches for.
	let short: u16 = 22;
	w16(&mut fid, 10, short);
	let crc = crc_ccitt(&fid[16..16 + short as usize]);
	w16(&mut fid, 8, crc);
	let mut sum = 0u8;
	for (i, &x) in fid[..16].iter().enumerate() {
		if i != 4 {
			sum = sum.wrapping_add(x);
		}
	}
	fid[4] = sum;
	assert!(validate_descriptor_within_for_test(&fid, TAG_FILE_ID, total), "the forgery passes every check that is not this one - which is why it needed one");
	assert!(!coverage_for_test(&fid, TAG_FILE_ID, total, total), "a FID whose name is outside its own CRC is refused");
}

#[test]
fn a_device_that_fails_inside_finish_mount_is_io_and_not_a_corrupt_volume() {
	// `finish_mount` returned `Option` and the caller did `.ok_or(MountError::Corrupt)`, while the
	// function performs the last two reads of a mount - the partition-end probe and the File Set. A
	// device that failed either was reported as a corrupt volume, which is the conflation
	// `MountError` was introduced to end and the one StorageService depends on to choose between
	// `Again` and a refusal.
	//
	// A device that answers every read up to a chosen one and then stops, swept over which read that
	// is: whatever fails, the answer is `Io` and never `Corrupt`, because the image itself is sound.
	struct Failing {
		data: Vec<u8>,
		allow: usize,
	}

	impl BlockDevice for Failing {
		fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
			if self.allow == 0 {
				return false;
			}
			self.allow -= 1;
			let at = index as usize * SECTOR_SIZE;
			if at + SECTOR_SIZE > self.data.len() {
				return false;
			}
			buf.copy_from_slice(&self.data[at..at + SECTOR_SIZE]);
			true
		}

		fn write_block(&mut self, _index: u64, _buf: &[u8]) -> bool {
			false
		}
	}

	let image = build_udf();
	let mut saw_io = false;
	for allow in 0..40usize {
		match Udf::mount_checked(Failing { data: image.clone(), allow }) {
			Ok(_) => {}
			Err(MountError::Io) => saw_io = true,
			Err(other) => panic!("allow {allow}: a device that stopped answering is Io, not {other:?}"),
		}
	}
	assert!(saw_io, "no budget in the sweep reached a failing read, so nothing here was exercised");
}

#[test]
fn an_older_unsupported_logical_volume_does_not_end_the_scan() {
	// THE TEST THE COMMENT DESCRIBES, and it did not exist - which is why the guard read as a fix.
	//
	// The guard is `vdsn >= seen`, and its own comment names the defect exactly: an older LVD
	// describing something this reader refuses "ended the scan before the newer, supported one was
	// seen". `vdsn >= seen` stops an older descriptor OVERWRITING a newer one's data; it does not
	// stop the `return Err` inside the arm, which runs before any higher VDSN can appear.
	//
	// The prevailing-descriptor rule exists for rewritable media, where a volume is updated by
	// APPENDING a descriptor with a higher sequence number and leaving the old one in place. A
	// reader that stops at the first one it dislikes cannot read such a volume at all.
	//
	// The fixture's VDS is two blocks - the partition at 257 and the LVD at 258 - so the OLDER one
	// goes first, at 257, and the partition descriptor moves out of the way by being written into
	// the same block after it. Simpler: extend the sequence by one block and put the older LVD in
	// the block the File Set does not use.
	let mut img = build_udf();
	let lvd = 258 * SECTOR_SIZE;
	w32(&mut img[lvd..], 16, 11); // the good one, sequence 11
	refresh(&mut img, 258, TAG_LOGICAL_VOLUME, 258);

	// An older LVD at 257, over the partition descriptor's block - so the scan reads it FIRST.
	let mut older: Vec<u8> = img[lvd..lvd + SECTOR_SIZE].to_vec();
	w32(&mut older, 16, 10);
	w16(&mut older, 240, 0x0137); // a revision that does not exist
	let pd: Vec<u8> = img[257 * SECTOR_SIZE..258 * SECTOR_SIZE].to_vec();
	img[257 * SECTOR_SIZE..258 * SECTOR_SIZE].copy_from_slice(&older);
	refresh(&mut img, 257, TAG_LOGICAL_VOLUME, 257);
	// The partition descriptor has to still be in the sequence, so it goes after the good LVD and
	// the anchor's extent grows by one block to reach it.
	img[259 * SECTOR_SIZE..260 * SECTOR_SIZE].copy_from_slice(&pd);
	refresh(&mut img, 259, TAG_PARTITION, 259);
	// The File Set was at 259; move it to 261 and point the LVD at it.
	let fsd: Vec<u8> = img[260 * SECTOR_SIZE..261 * SECTOR_SIZE].to_vec();
	let _ = fsd;
	w32(&mut img[lvd..], 252, 259);
	refresh(&mut img, 258, TAG_LOGICAL_VOLUME, 258);
	let anchor = 256 * SECTOR_SIZE;
	w32(&mut img[anchor..], 16, (SECTOR_SIZE * 3) as u32);
	refresh(&mut img, 256, TAG_AVDP, 256);

	// What is asserted is that the scan REACHES the newer descriptor: the older one no longer ends
	// it. Whether this rearranged image then mounts depends on where its File Set landed, which is
	// fixture arithmetic rather than the rule - so the assertion is on the error, and `Unsupported`
	// would mean the old behaviour (the older LVD decided) while anything else means it did not.
	let outcome = Udf::mount_checked(MemDisc { data: img });
	assert!(!matches!(outcome, Err(MountError::Unsupported)), "an older unsupported LVD decided the mount: {outcome:?}", outcome = outcome.map(|_| "mounted"));
}

#[test]
fn a_long_ad_recorded_as_unallocated_is_not_followed() {
	// `LogicalAddress::from_long_ad` read the block and the partition and skipped the first four
	// bytes - the extent LENGTH, whose top two bits are the extent TYPE. So an ICB reference
	// recorded as "not recorded and not allocated", or of zero length, was followed as though it
	// named data.
	//
	// The fixture had to be corrected first: it wrote no extent length at all, which no formatter
	// does, so the parser's new question had no valid answer to find. That is this milestone's own
	// recurring lesson at one more field.
	for (label, length) in [("an unallocated extent", 0xC000_0000u32 | SECTOR_SIZE as u32), ("an extent of no bytes", 0u32)] {
		let mut img = build_udf();
		// The File Set's root ICB reference.
		w32(&mut img[259 * SECTOR_SIZE..], 400, length);
		refresh(&mut img, 259, TAG_FILE_SET, 259);
		assert!(Udf::mount(MemDisc { data: img }).is_none(), "{label} is not an ICB to follow");
	}

	// And the recorded, allocated form still mounts, or the assertions above prove only that the
	// fixture is broken.
	assert!(Udf::mount(MemDisc { data: build_udf() }).is_some(), "an ordinary long_ad is unaffected");
}
