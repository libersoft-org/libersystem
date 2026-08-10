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
	w16(&mut b, 28, 4);
	w16(&mut b, 34, 3); // embedded alloc
	let mut body = Vec::new();
	for f in fids {
		body.extend_from_slice(f);
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
	tag(&mut pd, TAG_PARTITION, 257);
	blk(257, &pd);
	let mut lvd = vec![0u8; SECTOR_SIZE];
	// A CONFORMING Logical Volume Descriptor. The fixture used to write the File Set location and
	// nothing else, so the parser could not have read the block size, the domain identifier or the
	// partition maps even if it had wanted to - the same shape as the missing descriptor CRC: the
	// image agreed with the parser rather than with the format.
	w32(&mut lvd, 212, SECTOR_SIZE as u32); // LogicalBlockSize
	lvd[217..236].copy_from_slice(b"*OSTA UDF Compliant"); // DomainIdentifier
	w32(&mut lvd, 252, 259); // File Set at lb 259
	w16(&mut lvd, 256, 0); // ...in partition reference 0
	w32(&mut lvd, 268, 1); // NumberOfPartitionMaps
	w32(&mut lvd, 272, 6); // MapTableLength
	lvd[440] = 1; // one Type-1 (physical) partition map
	lvd[441] = 6; // its length
	tag(&mut lvd, TAG_LOGICAL_VOLUME, 258);
	blk(258, &lvd);
	let mut fsd = vec![0u8; SECTOR_SIZE];
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
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::Invalid));
	let mut img2 = build_udf();
	let fe2 = 263 * SECTOR_SIZE;
	img2[fe2 + 34] = 0; // short_ad
	w64(&mut img2[fe2..], 56, 5);
	w32(&mut img2[fe2..], 172, 8); // one descriptor
	w32(&mut img2[fe2..], 176, 2048); // recorded extent, 2048 bytes
	w32(&mut img2[fe2..], 180, 5000); // past the 264-block partition
	let mut fs2 = Udf::mount(MemDisc { data: img2 }).unwrap();
	assert_eq!(fs2.read_file(b"SUB/WORLD.TXT"), Err(FsError::Invalid));
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
	let mut fs2 = Udf::mount(MemDisc { data: img2 }).unwrap();
	assert_eq!(fs2.read_file(b"HELLO.TXT"), Err(FsError::Invalid));
}

#[test]
fn an_unchecksummed_descriptor_is_not_trusted() {
	// tag checksums are mandatory: a block merely starting with a plausible tag id
	// must not parse as a File Entry.
	let mut img = build_udf();
	img[262 * SECTOR_SIZE + 4] ^= 0x55;
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::Invalid));
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
		w16(b, 28, 4); // ICB strategy 4, which a real File Entry carries
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
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::Invalid));
}

#[test]
fn an_unknown_compression_id_does_not_decode() {
	// a d-string with an unknown compression id is noise, never text - the record
	// must not surface with a garbage name.
	let mut img = build_udf();
	let mut noise = fid("AB", false, false, 262);
	noise[38] = 254; // the compression id byte
	let sub = file_entry(261, true, &[fid("", true, true, 260), fid("WORLD.TXT", false, false, 263), noise], b"");
	img[261 * SECTOR_SIZE..262 * SECTOR_SIZE].copy_from_slice(&sub);
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	let names: Vec<_> = fs.list_dir(b"SUB").unwrap().into_iter().map(|f| f.name).collect();
	assert_eq!(names, ["WORLD.TXT"], "{names:?}");
}

#[test]
fn an_extended_ad_form_is_refused_not_misparsed() {
	// extended_ad records are 20 bytes - scanning them with the short_ad step parses
	// garbage extents; the form is refused instead.
	let mut img = build_udf();
	w16(&mut img[262 * SECTOR_SIZE..], 34, 2);
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::Invalid));
}

#[test]
fn a_symlink_file_entry_is_refused() {
	// a symlink stores its target path as data - the volume API has no symlink
	// semantics, so serving the path bytes as content would only mislead.
	let mut img = build_udf();
	img[262 * SECTOR_SIZE + 27] = 12;
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
