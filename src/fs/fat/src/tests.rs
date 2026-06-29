// Host tests for the FAT backend, run with `cd src/fat && cargo test`. A Vec-backed
// sector device stands in for the disk; each family's volume is synthesized in memory by
// a small image builder, so the tests need no external mkfs tools and are deterministic -
// mounting the image, listing it, and reading files back proves the boot-sector
// detection, the cluster-chain walk, VFAT long names, and the exFAT entry sets all work,
// and writing then re-reading proves cluster allocation and entry creation round-trip.

use super::*;

// A RAM-backed sector device: one contiguous Vec of 512-byte sectors, read and written.
struct MemDisk {
	data: Vec<u8>,
}

impl BlockDevice for MemDisk {
	fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		let start = lba as usize * SECTOR_SIZE;
		let Some(src) = self.data.get(start..start + SECTOR_SIZE) else {
			return false;
		};
		buf.copy_from_slice(src);
		true
	}

	fn write_sector(&mut self, lba: u64, buf: &[u8]) -> bool {
		let start = lba as usize * SECTOR_SIZE;
		let Some(dst) = self.data.get_mut(start..start + SECTOR_SIZE) else {
			return false;
		};
		dst.copy_from_slice(buf);
		true
	}
}

// One file to lay into a synthesized image: a path and its bytes. A trailing "/" path is
// an empty directory.
struct File {
	path: &'static str,
	data: &'static [u8],
}

// Build a classic FAT image (12 / 16 / 32 chosen by `kind`) holding `files`. spc 1, one
// FAT; clusters are handed out per file/dir, FAT chains and directory entries written so
// the reader walks them exactly as it would a real disk. Subdirectories get "." / "..".
fn build_fat(kind: Kind, files: &[File]) -> Vec<u8> {
	let bps: usize = 512;
	let spc: usize = 1;
	let reserved: usize = if kind == Kind::Fat32 { 32 } else { 1 };
	let root_entries: usize = if kind == Kind::Fat32 { 0 } else { 512 };
	let clusters: usize = match kind {
		Kind::Fat12 => 1000,
		Kind::Fat16 => 5000,
		_ => 66000,
	};
	let ent: usize = match kind {
		Kind::Fat12 => return build_fat12(files, clusters),
		Kind::Fat16 => 2,
		_ => 4,
	};
	let fat_size = (clusters * ent).div_ceil(bps);
	let root_sectors = (root_entries * 32).div_ceil(bps);
	let first_data = reserved + fat_size + root_sectors;
	let total = first_data + clusters;
	let mut img = vec![0u8; total * bps];
	let mut fat = vec![0u8; fat_size * bps];
	let root_cluster = if kind == Kind::Fat32 { 2 } else { 0 };
	let mut next = if kind == Kind::Fat32 { 3 } else { 2 };
	// place files/dirs and fill the root directory.
	let mut root: Vec<u8> = Vec::new();
	for f in files {
		place_classic(&mut img, &mut fat, &mut next, &mut root, f, first_data, ent, kind);
	}
	if kind == Kind::Fat32 {
		let lba = (first_data + (root_cluster - 2)) * bps;
		img[lba..lba + root.len().min(bps)].copy_from_slice(&root[..root.len().min(bps)]);
		set_fat(&mut fat, ent, root_cluster, 0x0FFF_FFFF);
	} else {
		let root_off = (reserved + fat_size) * bps;
		img[root_off..root_off + root.len()].copy_from_slice(&root);
	}
	img[(reserved * bps)..(reserved * bps) + fat.len()].copy_from_slice(&fat);
	write_bpb(&mut img, bps, spc, reserved, fat_size, root_entries, total, root_cluster);
	img
}

// FAT12 is built by the same shape but with 12-bit FAT entries; kept separate so the
// generic path stays 16/32. spc 1, one FAT, root region, a handful of files.
fn build_fat12(files: &[File], clusters: usize) -> Vec<u8> {
	let bps: usize = 512;
	let reserved: usize = 1;
	let root_entries: usize = 512;
	let fat_size = (clusters * 3).div_ceil(2).div_ceil(bps);
	let root_sectors = (root_entries * 32).div_ceil(bps);
	let first_data = reserved + fat_size + root_sectors;
	let total = first_data + clusters;
	let mut img = vec![0u8; total * bps];
	let mut fat = vec![0u8; fat_size * bps];
	let mut next = 2;
	let mut root: Vec<u8> = Vec::new();
	for f in files {
		place_classic(&mut img, &mut fat, &mut next, &mut root, f, first_data, 12, Kind::Fat12);
	}
	let root_off = (reserved + fat_size) * bps;
	img[root_off..root_off + root.len()].copy_from_slice(&root);
	img[(reserved * bps)..(reserved * bps) + fat.len()].copy_from_slice(&fat);
	write_bpb(&mut img, bps, 1, reserved, fat_size, root_entries, total, 0);
	img
}

// Lay one file or one-level subdirectory into the data region and add its directory
// record (with a VFAT long name when the name is not a clean 8.3). Subdir holds its child.
fn place_classic(img: &mut [u8], fat: &mut [u8], next: &mut usize, dir: &mut Vec<u8>, f: &File, first_data: usize, ent: usize, kind: Kind) {
	let bps: usize = 512;
	if let Some((sub, child)) = f.path.split_once('/') {
		let dir_cluster = *next;
		*next += 1;
		set_fat(fat, ent, dir_cluster, end_marker(kind));
		let mut sub_dir: Vec<u8> = Vec::new();
		push_entry(&mut sub_dir, ".", true, 0, dir_cluster as u32);
		push_entry(&mut sub_dir, "..", true, 0, 0);
		place_classic(img, fat, next, &mut sub_dir, &File { path: leak(child), data: f.data }, first_data, ent, kind);
		let off = (first_data + dir_cluster - 2) * bps;
		img[off..off + sub_dir.len()].copy_from_slice(&sub_dir);
		push_entry(dir, sub, true, 0, dir_cluster as u32);
	} else {
		let cluster = *next;
		*next += 1;
		set_fat(fat, ent, cluster, end_marker(kind));
		let off = (first_data + cluster - 2) * bps;
		img[off..off + f.data.len()].copy_from_slice(f.data);
		push_entry(dir, f.path, false, f.data.len() as u32, cluster as u32);
	}
}

// A static-lifetime copy of a string, for recursing one level of subdirectory.
fn leak(s: &str) -> &'static str {
	Box::leak(s.to_string().into_boxed_str())
}

fn end_marker(kind: Kind) -> u32 {
	match kind {
		Kind::Fat12 => 0x0FF8,
		Kind::Fat16 => 0xFFF8,
		_ => 0x0FFF_FFF8,
	}
}

// Set FAT entry `cluster` to `val` for the family's width.
fn set_fat(fat: &mut [u8], ent: usize, cluster: usize, val: u32) {
	match ent {
		2 => fat[cluster * 2..cluster * 2 + 2].copy_from_slice(&(val as u16).to_le_bytes()),
		4 => fat[cluster * 4..cluster * 4 + 4].copy_from_slice(&val.to_le_bytes()),
		_ => {
			let off = cluster + cluster / 2;
			let cur = u16::from_le_bytes([fat[off], fat[off + 1]]);
			let merged = if cluster & 1 == 1 { (cur & 0x000F) | ((val as u16) << 4) } else { (cur & 0xF000) | (val as u16 & 0x0FFF) };
			fat[off..off + 2].copy_from_slice(&merged.to_le_bytes());
		}
	}
}

// Append a directory record: a VFAT long-name run for a non-8.3 name, then the 8.3 entry.
fn push_entry(dir: &mut Vec<u8>, name: &str, is_dir: bool, size: u32, cluster: u32) {
	let short = short83(name);
	if name != "." && name != ".." && name.as_bytes() != trim_spaces83(&short) {
		let units: Vec<u16> = name.encode_utf16().collect();
		let sum = checksum(&short);
		dir.extend_from_slice(&lfn_entry(&units, sum));
	}
	let mut e = [0u8; 32];
	e[0..11].copy_from_slice(&short);
	e[11] = if is_dir { 0x10 } else { 0x20 };
	e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
	e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
	e[28..32].copy_from_slice(&size.to_le_bytes());
	dir.extend_from_slice(&e);
}

// The single LFN entry for a short name (tests keep names <= 13 chars), seq 1 + last.
fn lfn_entry(units: &[u16], sum: u8) -> [u8; 32] {
	let mut e = [0xFFu8; 32];
	e[0] = 0x41;
	e[11] = 0x0F;
	e[12] = 0;
	e[13] = sum;
	e[26] = 0;
	e[27] = 0;
	let slots = [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
	for (i, &s) in slots.iter().enumerate() {
		let v = if i < units.len() {
			units[i]
		} else if i == units.len() {
			0
		} else {
			0xFFFF
		};
		e[s..s + 2].copy_from_slice(&v.to_le_bytes());
	}
	e
}

fn checksum(short: &[u8; 11]) -> u8 {
	let mut sum = 0u8;
	for &c in short {
		sum = sum.rotate_right(1).wrapping_add(c);
	}
	sum
}

fn short83(name: &str) -> [u8; 11] {
	let mut s = [0x20u8; 11];
	if name == "." {
		s[0] = b'.';
		return s;
	}
	if name == ".." {
		s[0] = b'.';
		s[1] = b'.';
		return s;
	}
	let (base, ext) = name.split_once('.').unwrap_or((name, ""));
	for (i, c) in base.bytes().take(8).enumerate() {
		s[i] = c.to_ascii_uppercase();
	}
	for (i, c) in ext.bytes().take(3).enumerate() {
		s[8 + i] = c.to_ascii_uppercase();
	}
	s
}

fn trim_spaces83(s: &[u8; 11]) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::new();
	out.extend_from_slice(trim_spaces(&s[0..8]));
	let ext = trim_spaces(&s[8..11]);
	if !ext.is_empty() {
		out.push(b'.');
		out.extend_from_slice(ext);
	}
	out
}

fn write_bpb(img: &mut [u8], bps: usize, spc: usize, reserved: usize, fat_size: usize, root_entries: usize, total: usize, root_cluster: usize) {
	img[11..13].copy_from_slice(&(bps as u16).to_le_bytes());
	img[13] = spc as u8;
	img[14..16].copy_from_slice(&(reserved as u16).to_le_bytes());
	img[16] = 1;
	img[17..19].copy_from_slice(&(root_entries as u16).to_le_bytes());
	if total < 0x10000 {
		img[19..21].copy_from_slice(&(total as u16).to_le_bytes());
	} else {
		img[32..36].copy_from_slice(&(total as u32).to_le_bytes());
	}
	if root_cluster != 0 {
		img[36..40].copy_from_slice(&(fat_size as u32).to_le_bytes());
		img[44..48].copy_from_slice(&(root_cluster as u32).to_le_bytes());
	} else {
		img[22..24].copy_from_slice(&(fat_size as u16).to_le_bytes());
	}
	img[510] = 0x55;
	img[511] = 0xAA;
}

// Build a small exFAT image: a 24-sector reserved boot region, a 32-bit FAT, and a
// cluster heap with an allocation bitmap (cluster 2), a root directory (cluster 3) and
// file clusters. The bitmap and the 0x81 entry are written so the write path can find
// free clusters; spc 1; FAT chains written so the reader follows them.
fn build_exfat(files: &[File]) -> Vec<u8> {
	let bps = 512;
	let reserved = 24;
	let clusters = 64;
	let fat_size = (clusters * 4usize).div_ceil(bps);
	let heap = reserved + fat_size;
	let total = heap + clusters;
	let mut img = vec![0u8; total * bps];
	let mut fat = vec![0u8; fat_size * bps];
	let mut bm = vec![0u8; clusters.div_ceil(8)];
	let mut next = 4;
	let mut root: Vec<u8> = Vec::new();
	// the allocation bitmap lives in cluster 2; the root in cluster 3; both stay allocated.
	set_fat(&mut fat, 4, 2, 0x0FFF_FFFF);
	set_fat(&mut fat, 4, 3, 0x0FFF_FFFF);
	bm[0] |= 0b11;
	push_exfat_bitmap(&mut root, 2, clusters.div_ceil(8) as u64);
	for f in files {
		let cluster = next;
		next += 1;
		set_fat(&mut fat, 4, cluster, 0x0FFF_FFFF);
		bm[0] |= 1 << (cluster - 2);
		let off = (heap + cluster - 2) * bps;
		img[off..off + f.data.len()].copy_from_slice(f.data);
		push_exfat_entry(&mut root, f.path, f.data.len() as u64, cluster as u32);
	}
	let bm_off = heap * bps;
	img[bm_off..bm_off + bm.len()].copy_from_slice(&bm);
	let root_off = (heap + 1) * bps;
	img[root_off..root_off + root.len()].copy_from_slice(&root);
	img[reserved * bps..reserved * bps + fat.len()].copy_from_slice(&fat);
	img[3..11].copy_from_slice(b"EXFAT   ");
	img[80..84].copy_from_slice(&(reserved as u32).to_le_bytes());
	img[84..88].copy_from_slice(&(fat_size as u32).to_le_bytes());
	img[88..92].copy_from_slice(&(heap as u32).to_le_bytes());
	img[92..96].copy_from_slice(&(clusters as u32).to_le_bytes());
	img[96..100].copy_from_slice(&3u32.to_le_bytes());
	img[108] = 9;
	img[109] = 0;
	img[110] = 1;
	img[510] = 0x55;
	img[511] = 0xAA;
	img
}

// A 0x81 allocation-bitmap entry: marks the bitmap's first cluster and byte length.
fn push_exfat_bitmap(dir: &mut Vec<u8>, cluster: u32, size: u64) {
	let mut e = [0u8; 32];
	e[0] = 0x81;
	e[20..24].copy_from_slice(&cluster.to_le_bytes());
	e[24..32].copy_from_slice(&size.to_le_bytes());
	dir.extend_from_slice(&e);
}

fn push_exfat_entry(dir: &mut Vec<u8>, name: &str, size: u64, cluster: u32) {
	let units: Vec<u16> = name.encode_utf16().collect();
	let name_frags = units.len().div_ceil(15);
	let mut file = [0u8; 32];
	file[0] = 0x85;
	file[1] = (1 + name_frags) as u8;
	let mut stream = [0u8; 32];
	stream[0] = 0xC0;
	stream[3] = units.len() as u8;
	stream[20..24].copy_from_slice(&cluster.to_le_bytes());
	stream[24..32].copy_from_slice(&size.to_le_bytes());
	dir.extend_from_slice(&file);
	dir.extend_from_slice(&stream);
	for f in 0..name_frags {
		let mut e = [0u8; 32];
		e[0] = 0xC1;
		for c in 0..15 {
			let idx = f * 15 + c;
			let v = if idx < units.len() { units[idx] } else { 0 };
			e[2 + c * 2..4 + c * 2].copy_from_slice(&v.to_le_bytes());
		}
		dir.extend_from_slice(&e);
	}
}

fn names(list: &[FileInfo]) -> Vec<String> {
	let mut n: Vec<String> = list.iter().map(|e| e.name.clone()).collect();
	n.sort();
	n
}

const ROOT: &[File] = &[File { path: "HELLO.TXT", data: b"Hello, FAT!" }, File { path: "readme.md", data: b"long name file" }, File { path: "DOCS/a.txt", data: b"in a subdir" }];

#[test]
fn mounts_and_lists_fat12() {
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat12, ROOT) }).unwrap();
	assert_eq!(names(&fs.list().unwrap()), ["DOCS", "HELLO.TXT", "readme.md"]);
}

#[test]
fn mounts_and_lists_fat16() {
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	assert_eq!(names(&fs.list().unwrap()), ["DOCS", "HELLO.TXT", "readme.md"]);
}

#[test]
fn mounts_and_lists_fat32() {
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat32, ROOT) }).unwrap();
	assert_eq!(names(&fs.list().unwrap()), ["DOCS", "HELLO.TXT", "readme.md"]);
}

#[test]
fn mounts_and_lists_exfat() {
	let mut fs = FatFs::mount(MemDisk { data: build_exfat(ROOT) }).unwrap();
	let list = fs.list().unwrap();
	assert!(list.iter().any(|e| e.name == "HELLO.TXT" && e.size == 11));
	assert!(list.iter().any(|e| e.name == "readme.md"));
}

#[test]
fn reads_a_file_off_each_family() {
	for kind in [Kind::Fat12, Kind::Fat16, Kind::Fat32] {
		let mut fs = FatFs::mount(MemDisk { data: build_fat(kind, ROOT) }).unwrap();
		assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), b"Hello, FAT!");
	}
	let mut fs = FatFs::mount(MemDisk { data: build_exfat(ROOT) }).unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), b"Hello, FAT!");
}

#[test]
fn resolves_a_long_file_name() {
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	assert_eq!(fs.read_file(b"readme.md").unwrap(), b"long name file");
}

#[test]
fn reads_a_file_in_a_subdirectory() {
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat32, ROOT) }).unwrap();
	assert_eq!(names(&fs.list_dir(b"DOCS").unwrap()), ["a.txt"]);
	assert_eq!(fs.read_file(b"DOCS/a.txt").unwrap(), b"in a subdir");
}

#[test]
fn lookup_is_case_insensitive() {
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	assert_eq!(fs.read_file(b"hello.txt").unwrap(), b"Hello, FAT!");
}

#[test]
fn a_missing_file_is_not_found() {
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	assert_eq!(fs.read_file(b"nope.txt"), Err(FsError::NotFound));
}

#[test]
fn an_unformatted_disk_does_not_mount() {
	assert!(FatFs::mount(MemDisk { data: vec![0u8; SECTOR_SIZE * 4] }).is_none());
}

#[test]
fn writes_a_new_file_then_reads_it_back() {
	for kind in [Kind::Fat12, Kind::Fat16, Kind::Fat32] {
		let mut fs = FatFs::mount(MemDisk { data: build_fat(kind, ROOT) }).unwrap();
		fs.write_file(b"NEW.TXT", b"fresh bytes").unwrap();
		assert_eq!(fs.read_file(b"NEW.TXT").unwrap(), b"fresh bytes");
		assert!(names(&fs.list().unwrap()).contains(&"NEW.TXT".to_string()));
	}
}

#[test]
fn writes_a_multi_cluster_file() {
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	let big: Vec<u8> = (0..1500u32).map(|i| i as u8).collect();
	fs.write_file(b"BIG.BIN", &big).unwrap();
	assert_eq!(fs.read_file(b"BIG.BIN").unwrap(), big);
}

#[test]
fn overwrites_an_existing_file() {
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat32, ROOT) }).unwrap();
	fs.write_file(b"HELLO.TXT", b"shorter").unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), b"shorter");
	let n: Vec<String> = fs.list().unwrap().iter().filter(|e| e.name == "HELLO.TXT").map(|e| e.name.clone()).collect();
	assert_eq!(n.len(), 1);
}

#[test]
fn removes_a_file() {
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	fs.remove(b"HELLO.TXT").unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::NotFound));
	assert!(!names(&fs.list().unwrap()).contains(&"HELLO.TXT".to_string()));
}

#[test]
fn writes_a_long_name_file() {
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat32, ROOT) }).unwrap();
	fs.write_file(b"a long note.txt", b"vfat").unwrap();
	assert_eq!(fs.read_file(b"a long note.txt").unwrap(), b"vfat");
}

#[test]
fn removing_a_missing_file_is_not_found() {
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	assert_eq!(fs.remove(b"nope.txt"), Err(FsError::NotFound));
}

#[test]
fn writes_an_exfat_file_then_reads_it_back() {
	let mut fs = FatFs::mount(MemDisk { data: build_exfat(ROOT) }).unwrap();
	fs.write_file(b"NEW.TXT", b"fresh exfat bytes").unwrap();
	assert_eq!(fs.read_file(b"NEW.TXT").unwrap(), b"fresh exfat bytes");
	assert!(names(&fs.list().unwrap()).contains(&"NEW.TXT".to_string()));
}

#[test]
fn writes_a_multi_cluster_exfat_file() {
	let mut fs = FatFs::mount(MemDisk { data: build_exfat(ROOT) }).unwrap();
	let big: Vec<u8> = (0..1500u32).map(|i| i as u8).collect();
	fs.write_file(b"BIG.BIN", &big).unwrap();
	assert_eq!(fs.read_file(b"BIG.BIN").unwrap(), big);
}

#[test]
fn overwrites_and_removes_an_exfat_file() {
	let mut fs = FatFs::mount(MemDisk { data: build_exfat(ROOT) }).unwrap();
	fs.write_file(b"HELLO.TXT", b"shorter").unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), b"shorter");
	fs.remove(b"HELLO.TXT").unwrap();
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::NotFound));
}
