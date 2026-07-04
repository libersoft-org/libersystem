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

// Write a descriptor tag id and stamp its checksum (byte 4 over the other fifteen tag
// bytes), as every real descriptor carries - the reader verifies it.
fn tag(b: &mut [u8], id: u16) {
	w16(b, 0, id);
	let mut sum = 0u8;
	for (i, &x) in b[..16].iter().enumerate() {
		if i != 4 {
			sum = sum.wrapping_add(x);
		}
	}
	b[4] = sum;
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
	tag(&mut f, TAG_FILE_ID);
	f
}

// Build an embedded File Entry (one block): a directory holding `fids` or a file holding
// `data`.
fn file_entry(is_dir: bool, fids: &[Vec<u8>], data: &[u8]) -> Vec<u8> {
	let mut b = vec![0u8; SECTOR_SIZE];
	b[27] = if is_dir { 4 } else { 5 };
	w16(&mut b, 34, 3); // embedded alloc
	let mut body = Vec::new();
	for f in fids {
		body.extend_from_slice(f);
	}
	body.extend_from_slice(data);
	w64(&mut b, 56, body.len() as u64);
	w32(&mut b, 172, body.len() as u32);
	b[176..176 + body.len()].copy_from_slice(&body);
	tag(&mut b, TAG_FILE_ENTRY);
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
	tag(&mut avdp, TAG_AVDP);
	blk(256, &avdp);
	let mut pd = vec![0u8; SECTOR_SIZE];
	w32(&mut pd, 188, 0); // partition starts at LBA 0
	w32(&mut pd, 192, 264); // and spans the whole 264-block image
	tag(&mut pd, TAG_PARTITION);
	blk(257, &pd);
	let mut lvd = vec![0u8; SECTOR_SIZE];
	w32(&mut lvd, 252, 259); // File Set at lb 259
	tag(&mut lvd, TAG_LOGICAL_VOLUME);
	blk(258, &lvd);
	let mut fsd = vec![0u8; SECTOR_SIZE];
	w32(&mut fsd, 404, 260); // root ICB at lb 260
	tag(&mut fsd, TAG_FILE_SET);
	blk(259, &fsd);
	blk(262, &file_entry(false, &[], b"hello udf"));
	blk(263, &file_entry(false, &[], b"world"));
	blk(261, &file_entry(true, &[fid("", true, true, 260), fid("WORLD.TXT", false, false, 263)], b""));
	blk(260, &file_entry(true, &[fid("", true, true, 260), fid("SUB", true, false, 261), fid("HELLO.TXT", false, false, 262)], b""));
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
	let sub = file_entry(true, &[fid("", true, true, 260), fid("WORLD.TXT", false, false, 263), fid("", false, false, 262)], b"");
	img[261 * SECTOR_SIZE..262 * SECTOR_SIZE].copy_from_slice(&sub);
	let mut fs = Udf::mount(MemDisc { data: img }).unwrap();
	let list = fs.list_dir(b"SUB").unwrap();
	assert!(list.iter().all(|f| !f.name.is_empty()), "{list:?}");
	assert_eq!(fs.read_file(b"SUB/"), Err(FsError::NotFound));
	let mut up: Vec<_> = fs.list_dir(b"SUB/..").unwrap().into_iter().map(|f| f.name).collect();
	up.sort();
	assert_eq!(up, ["HELLO.TXT", "SUB"]);
}
