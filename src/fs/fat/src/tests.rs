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
	let clusters: usize = match kind {
		Kind::Fat12 => 1000,
		Kind::Fat16 => 5000,
		_ => 66000,
	};
	build_fat_sized(kind, files, clusters)
}

// The sized variant of `build_fat`: the cluster count is the caller's, so a FAT32
// image can be built small (inside the FAT16 cluster range) the way mtools formats
// a stick - the layout the BPB-shape detection exists for.
fn build_fat_sized(kind: Kind, files: &[File], clusters: usize) -> Vec<u8> {
	let bps: usize = 512;
	let spc: usize = 1;
	let reserved: usize = if kind == Kind::Fat32 { 32 } else { 1 };
	let root_entries: usize = if kind == Kind::Fat32 { 0 } else { 512 };
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
		// an FSInfo sector at sector 1 (inside the reserved region), seeded with a
		// known free count so the allocate/free upkeep is observable.
		img[48..50].copy_from_slice(&1u16.to_le_bytes());
		let fi = bps;
		img[fi..fi + 4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
		img[fi + 484..fi + 488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
		img[fi + 488..fi + 492].copy_from_slice(&1000u32.to_le_bytes());
		img[fi + 508..fi + 512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
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
	build_exfat_nfc(files, &[])
}

// The NoFatChain-aware variant: `nfc_files` are laid out as Windows commonly writes
// them - contiguous clusters, the stream entry's NoFatChain flag set, and NOTHING
// written into the FAT for them (the bitmap alone records the allocation).
fn build_exfat_nfc(files: &[File], nfc_files: &[File]) -> Vec<u8> {
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
		push_exfat_entry(&mut root, f.path, f.data.len() as u64, cluster as u32, false);
	}
	for f in nfc_files {
		let cluster = next;
		let span = f.data.len().div_ceil(bps).max(1);
		next += span;
		for i in 0..span {
			let idx = cluster + i - 2;
			bm[idx / 8] |= 1 << (idx % 8);
		}
		let off = (heap + cluster - 2) * bps;
		img[off..off + f.data.len()].copy_from_slice(f.data);
		push_exfat_entry(&mut root, f.path, f.data.len() as u64, cluster as u32, true);
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

fn push_exfat_entry(dir: &mut Vec<u8>, name: &str, size: u64, cluster: u32, nfc: bool) {
	let units: Vec<u16> = name.encode_utf16().collect();
	let name_frags = units.len().div_ceil(15);
	let mut file = [0u8; 32];
	file[0] = 0x85;
	file[1] = (1 + name_frags) as u8;
	let mut stream = [0u8; 32];
	stream[0] = 0xC0;
	stream[1] = if nfc { 0x03 } else { 0x01 };
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
fn mounts_small_fat32_by_bpb_shape() {
	// A FAT32 volume whose cluster count sits inside the FAT16 range - the layout
	// mtools formats a small stick with. The cluster-count thresholds alone would
	// misclassify it as FAT16 (and read an empty fixed root region that does not
	// exist); the BPB shape (no root entries, the FAT size in the 32-bit field)
	// must classify it as FAT32 and resolve its files.
	let mut fs = FatFs::mount(MemDisk { data: build_fat_sized(Kind::Fat32, ROOT, 20000) }).unwrap();
	assert_eq!(names(&fs.list().unwrap()), ["DOCS", "HELLO.TXT", "readme.md"]);
	assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), b"Hello, FAT!");
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

// Count the allocated FAT entries, for leak assertions across write/remove cycles.
fn allocated_clusters(fs: &mut FatFs<MemDisk>) -> usize {
	let max = fs.max_cluster();
	(2..=max).filter(|&c| fs.next_cluster(c).unwrap() != 0).count()
}

#[test]
fn overwriting_and_removing_a_long_name_file_leaks_nothing() {
	// An LFN-named file must unlink by its LONG name: an overwrite may not leave a
	// duplicate entry, a remove must find it, and neither may leak clusters.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	let before = allocated_clusters(&mut fs);
	fs.write_file(b"my document.txt", b"first version").unwrap();
	fs.write_file(b"my document.txt", b"the second version").unwrap();
	let hits: Vec<String> = fs.list().unwrap().iter().filter(|e| e.name == "my document.txt").map(|e| e.name.clone()).collect();
	assert_eq!(hits.len(), 1, "an overwrite must not duplicate the entry");
	assert_eq!(fs.read_file(b"my document.txt").unwrap(), b"the second version");
	fs.remove(b"my document.txt").unwrap();
	assert_eq!(fs.read_file(b"my document.txt"), Err(FsError::NotFound));
	assert_eq!(allocated_clusters(&mut fs), before, "the cycle must free every cluster it allocated");
}

#[test]
fn reads_and_frees_a_nofatchain_exfat_file() {
	// The contiguous NoFatChain form Windows commonly writes: multi-cluster data with
	// NOTHING in the FAT. It must read back whole (not truncated at the first cluster)
	// and a remove must clear its bitmap bits.
	let data: Vec<u8> = (0..1500u32).map(|i| (i * 7) as u8).collect();
	let leaked: &'static [u8] = Box::leak(data.clone().into_boxed_slice());
	let img = build_exfat_nfc(&[], &[File { path: "movie.mkv", data: leaked }]);
	let heap = 25usize; // 24 reserved + 1 FAT sector
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	assert_eq!(fs.read_file(b"movie.mkv").unwrap(), data);
	fs.remove(b"movie.mkv").unwrap();
	assert_eq!(fs.read_file(b"movie.mkv"), Err(FsError::NotFound));
	// clusters 4, 5, 6 (bitmap bits 2..=4) freed; the bitmap + root (bits 0, 1) stay.
	assert_eq!(fs.dev.data[heap * 512], 0b11, "the NoFatChain run's bitmap bits must be cleared");
}

#[test]
fn a_failed_overwrite_leaves_the_old_file_intact() {
	// The new chain is allocated and written BEFORE the directory entry swaps and the
	// old chain is freed - so an overwrite that cannot allocate must leave the old
	// content readable and leak nothing.
	let mut fs = FatFs::mount(MemDisk { data: build_fat_sized(Kind::Fat12, ROOT, 1000) }).unwrap();
	fs.write_file(b"KEEP.TXT", b"the original bytes").unwrap();
	let before = allocated_clusters(&mut fs);
	let huge = vec![0xA5u8; 1200 * 512];
	assert_eq!(fs.write_file(b"KEEP.TXT", &huge), Err(FsError::NoSpace));
	assert_eq!(fs.read_file(b"KEEP.TXT").unwrap(), b"the original bytes");
	assert_eq!(allocated_clusters(&mut fs), before, "a failed overwrite must not leak clusters");
}

#[test]
fn a_malformed_boot_sector_is_refused_not_panicked() {
	// Forged boot sectors off hostile media: insane exFAT shift exponents, BPB region
	// arithmetic past the sector count, and a missing boot signature - each must
	// refuse the mount, never panic, wrap, or accept a garbage geometry.
	let mut exfat_shift = vec![0u8; 512];
	exfat_shift[3..11].copy_from_slice(b"EXFAT   ");
	exfat_shift[108] = 255;
	exfat_shift[109] = 0;
	exfat_shift[110] = 1;
	assert!(FatFs::mount(MemDisk { data: exfat_shift.clone() }).is_none());
	exfat_shift[108] = 9;
	exfat_shift[109] = 200;
	assert!(FatFs::mount(MemDisk { data: exfat_shift }).is_none());
	// a BPB whose reserved + FAT regions exceed the total sector count (u32 overflow
	// bait in num_fats * fat_size and underflow bait in total - first_data).
	let mut bpb = vec![0u8; 512];
	bpb[11..13].copy_from_slice(&512u16.to_le_bytes());
	bpb[13] = 1;
	bpb[14..16].copy_from_slice(&1u16.to_le_bytes());
	bpb[16] = 255;
	bpb[17..19].copy_from_slice(&512u16.to_le_bytes());
	bpb[19..21].copy_from_slice(&64u16.to_le_bytes());
	bpb[22..24].copy_from_slice(&0xFFFFu16.to_le_bytes());
	bpb[510] = 0x55;
	bpb[511] = 0xAA;
	assert!(FatFs::mount(MemDisk { data: bpb }).is_none());
	// a BPB whose data region rounds to zero clusters is degenerate - refused, like
	// exFAT's cluster_count == 0, instead of mounting and failing piecemeal.
	let mut zeroc = vec![0u8; 512];
	zeroc[11..13].copy_from_slice(&512u16.to_le_bytes());
	zeroc[13] = 4;
	zeroc[14..16].copy_from_slice(&1u16.to_le_bytes());
	zeroc[16] = 1;
	zeroc[17..19].copy_from_slice(&16u16.to_le_bytes());
	zeroc[19..21].copy_from_slice(&5u16.to_le_bytes());
	zeroc[22..24].copy_from_slice(&1u16.to_le_bytes());
	zeroc[510] = 0x55;
	zeroc[511] = 0xAA;
	assert!(FatFs::mount(MemDisk { data: zeroc }).is_none());
	// plausible numbers but no 0x55AA boot signature: not a FAT volume.
	let mut unsigned = build_fat(Kind::Fat16, ROOT);
	unsigned[510] = 0;
	unsigned[511] = 0;
	assert!(FatFs::mount(MemDisk { data: unsigned }).is_none());
}

#[test]
fn a_corrupt_chain_cannot_hang_or_overwrite_the_media_descriptor() {
	// last_cluster is the append/grow walk: a cyclic chain must error out (not hang),
	// and a chain hitting a FREE entry must refuse (not walk to cluster 0, whose FAT
	// slot is the media descriptor the old code would then overwrite).
	let mut img = build_fat(Kind::Fat16, ROOT);
	let fat_off = 512; // reserved = 1 sector
	img[fat_off + 40 * 2..fat_off + 40 * 2 + 2].copy_from_slice(&41u16.to_le_bytes());
	img[fat_off + 41 * 2..fat_off + 41 * 2 + 2].copy_from_slice(&40u16.to_le_bytes());
	img[fat_off + 50 * 2..fat_off + 50 * 2 + 2].copy_from_slice(&0u16.to_le_bytes());
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	assert_eq!(fs.last_cluster(40), Err(FsError::Invalid));
	assert_eq!(fs.last_cluster(50), Err(FsError::Invalid));
}

#[test]
fn a_long_name_grows_a_full_directory_without_panicking() {
	// A 255-byte name is a 21-record entry set (672 bytes) - larger than one 512-byte
	// cluster, the exact shape whose one-cluster grow used to slice out of bounds.
	// The directory must grow by as many clusters as the set needs.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	let mut long = vec![b'n'; 251];
	long.extend_from_slice(b".txt");
	let mut path = b"DOCS/".to_vec();
	path.extend_from_slice(&long);
	fs.write_file(&path, b"grown into place").unwrap();
	assert_eq!(fs.read_file(&path).unwrap(), b"grown into place");
	let listed = fs.list_dir(b"DOCS").unwrap();
	assert!(listed.iter().any(|e| e.name.as_bytes() == long.as_slice()));
}

#[test]
fn reads_a_chain_longer_than_the_old_guard() {
	// FAT12 holds 341 entries per 512-byte FAT sector; the old loop guard assumed 128
	// and falsely refused a legitimate long chain. A 500-cluster file must read whole.
	let mut fs = FatFs::mount(MemDisk { data: build_fat_sized(Kind::Fat12, ROOT, 1000) }).unwrap();
	let big: Vec<u8> = (0..500 * 512u32).map(|i| (i * 13) as u8).collect();
	fs.write_file(b"BIG.BIN", &big).unwrap();
	assert_eq!(fs.read_file(b"BIG.BIN").unwrap(), big);
}

#[test]
fn allocation_never_leaves_the_data_region() {
	// The FAT's byte size has slack entries past the real cluster count; allocating
	// from the slack would write outside the volume (an Io error on an exactly-sized
	// device). Filling the volume must end in a clean NoSpace instead.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	let chunk = vec![0x5Au8; 500 * 512];
	let mut wrote = 0usize;
	let err = loop {
		let name = alloc::format!("FILL{}.BIN", wrote);
		match fs.write_file(name.as_bytes(), &chunk) {
			Ok(()) => wrote += 1,
			Err(e) => break e,
		}
	};
	assert_eq!(err, FsError::NoSpace, "exhaustion must be NoSpace, never an out-of-volume Io");
	assert!(wrote >= 9, "the volume should have fit ~9 such files, fit {wrote}");
	let name = alloc::format!("FILL{}.BIN", wrote - 1);
	assert_eq!(fs.read_file(name.as_bytes()).unwrap(), chunk);
}

#[test]
fn generated_short_names_are_unique_and_legal() {
	// Two long names with a common prefix must get DISTINCT numeric-tailed 8.3 forms,
	// and 8.3-illegal bytes (and a leading dot) must never reach the short field.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	fs.write_file(b"longfilename one.txt", b"one").unwrap();
	fs.write_file(b"longfilename two.txt", b"two").unwrap();
	fs.write_file(b".gitignore", b"dots").unwrap();
	fs.write_file(b"we;ird[name].txt", b"weird").unwrap();
	assert_eq!(fs.read_file(b"longfilename one.txt").unwrap(), b"one");
	assert_eq!(fs.read_file(b"longfilename two.txt").unwrap(), b"two");
	assert_eq!(fs.read_file(b".gitignore").unwrap(), b"dots");
	assert_eq!(fs.read_file(b"we;ird[name].txt").unwrap(), b"weird");
	let bytes = fs.read_dir_bytes(&Dir::at(0)).unwrap();
	let shorts = existing_shorts(&bytes);
	let mut seen: Vec<[u8; 11]> = Vec::new();
	for s in &shorts {
		assert!(!seen.contains(s), "duplicate short entry {:?}", s);
		seen.push(*s);
		assert!(s[0] != 0x20, "a short name must not start with a space: {:?}", s);
		for &b in s.iter() {
			assert!(b == 0x20 || short_char(b).0 == b, "illegal byte {b:#x} in short entry {:?}", s);
		}
	}
	let tailed = shorts.iter().filter(|s| s.contains(&b'~')).count();
	assert!(tailed >= 4, "the lossy names must carry numeric tails, found {tailed}");
}

#[test]
fn fat32_reserved_bits_survive_a_fat_write() {
	// The top nibble of a FAT32 entry is reserved: a write must read-modify-write it
	// through unchanged, per the specification.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat32, ROOT) }).unwrap();
	let fat_off = 32 * 512; // reserved = 32 sectors
	fs.dev.data[fat_off + 40 * 4..fat_off + 40 * 4 + 4].copy_from_slice(&0xF000_0000u32.to_le_bytes());
	fs.set_fat_entry(40, 3).unwrap();
	let raw = u32::from_le_bytes(fs.dev.data[fat_off + 40 * 4..fat_off + 40 * 4 + 4].try_into().unwrap());
	assert_eq!(raw, 0xF000_0003, "the reserved top nibble must be preserved");
}

#[test]
fn fsinfo_free_count_tracks_allocate_and_free() {
	// FAT32's FSInfo free-cluster count must follow allocation and freeing, so other
	// systems reading media we wrote see a truthful number.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat32, ROOT) }).unwrap();
	let free_at = 512 + 488; // FSInfo sector 1, seeded with 1000 by the builder
	fs.write_file(b"THREE.BIN", &[0x77u8; 3 * 512]).unwrap();
	let after_alloc = u32::from_le_bytes(fs.dev.data[free_at..free_at + 4].try_into().unwrap());
	assert_eq!(after_alloc, 997);
	fs.remove(b"THREE.BIN").unwrap();
	let after_free = u32::from_le_bytes(fs.dev.data[free_at..free_at + 4].try_into().unwrap());
	assert_eq!(after_free, 1000);
}

#[test]
fn dot_dot_resolves_to_the_root_on_fat32() {
	// A `..` entry pointing at the root carries first cluster 0; on FAT32 that means
	// the root cluster, not the FAT12/16 fixed region (which does not exist there).
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat32, ROOT) }).unwrap();
	let up = names(&fs.list_dir(b"DOCS/..").unwrap());
	assert_eq!(up, ["DOCS", "HELLO.TXT", "readme.md"]);
}

#[test]
fn a_1024_byte_sector_volume_reads_and_writes() {
	// FAT logical sectors are not always 512 bytes. On a bps=1024 volume the data
	// reads used to scale the sector address by the ratio TWICE (once in the cluster
	// address, once in the device expansion), landing every cluster read on the wrong
	// device sectors - the volume mounted and then read as garbage, while the
	// once-scaled writes went elsewhere. Reads and writes must agree.
	let bps = 1024usize;
	let clusters = 5000usize;
	let fat_size = (clusters * 2).div_ceil(bps);
	let root_sectors = (512 * 32) / bps;
	let first_data = 1 + fat_size + root_sectors;
	let total = first_data + clusters;
	let mut img = vec![0u8; total * bps];
	// one file at cluster 2: an end-of-chain FAT entry and an 8.3 root record.
	img[bps + 4..bps + 6].copy_from_slice(&0xFFF8u16.to_le_bytes());
	let data_off = first_data * bps;
	img[data_off..data_off + 11].copy_from_slice(b"Hello, FAT!");
	let mut root: Vec<u8> = Vec::new();
	push_entry(&mut root, "HELLO.TXT", false, 11, 2);
	let root_off = (1 + fat_size) * bps;
	img[root_off..root_off + root.len()].copy_from_slice(&root);
	write_bpb(&mut img, bps, 1, 1, fat_size, 512, total, 0);
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	assert_eq!(fs.kind_name(), "fat16");
	assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), b"Hello, FAT!");
	let big: Vec<u8> = (0..3000u32).map(|i| (i * 11) as u8).collect();
	fs.write_file(b"BIG.BIN", &big).unwrap();
	assert_eq!(fs.read_file(b"BIG.BIN").unwrap(), big);
	assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), b"Hello, FAT!");
}

#[test]
fn a_forged_nofatchain_size_is_refused() {
	// The NoFatChain length is the medium's own claim: a forged huge size used to hang
	// the free walk for ~4.5e15 iterations, grow the read allocation without bound,
	// and overflow the cluster arithmetic. Both paths must refuse it as Invalid.
	let img = build_exfat_nfc(&[], &[File { path: "movie.mkv", data: b"real bytes" }]);
	let heap = 25usize; // 24 reserved + 1 FAT sector
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	// the root: the 0x81 bitmap entry, then the 0x85 file and its 0xC0 stream entry,
	// whose data length lives at byte 24.
	let stream = (heap + 1) * 512 + 64;
	fs.dev.data[stream + 24..stream + 32].copy_from_slice(&u64::MAX.to_le_bytes());
	assert_eq!(fs.read_file(b"movie.mkv"), Err(FsError::Invalid));
	assert_eq!(fs.remove(b"movie.mkv"), Err(FsError::Invalid));
}

#[test]
fn a_name_leading_with_byte_0xe5_survives_a_write_cycle() {
	// U+5BB6 encodes as 0xE5 0xAE 0xB6: an 8.3 field starting with the raw 0xE5 reads
	// back as DELETED (the parser skips it and the file silently vanishes). The spec
	// stores a leading 0xE5 as 0x05; the whole cycle must work and leak nothing.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	let before = allocated_clusters(&mut fs);
	let name = "\u{5BB6}.txt".as_bytes();
	assert_eq!(name[0], 0xE5);
	fs.write_file(name, b"kanji-led bytes").unwrap();
	assert_eq!(fs.read_file(name).unwrap(), b"kanji-led bytes");
	assert!(fs.list().unwrap().iter().any(|e| e.name.as_bytes() == name));
	fs.remove(name).unwrap();
	assert_eq!(fs.read_file(name), Err(FsError::NotFound));
	assert_eq!(allocated_clusters(&mut fs), before);
}

#[test]
fn an_entry_never_lands_past_the_terminator() {
	// Everything from the first 0x00 entry is free space by spec, but stale non-free
	// garbage past it used to push a new entry set beyond the terminator - written
	// where the parser (which stops there) never looks: a silently lost file.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	let root_off = 21 * 512; // reserved 1 + FAT 20 sectors
						  // ROOT is four records, so slot 4 is the terminator - plant garbage in slot 5.
	fs.dev.data[root_off + 5 * 32] = b'X';
	fs.write_file(b"a long note.txt", b"visible").unwrap();
	assert_eq!(fs.read_file(b"a long note.txt").unwrap(), b"visible");
	assert!(names(&fs.list().unwrap()).contains(&"a long note.txt".to_string()));
}

#[test]
fn dot_only_and_trailing_dot_or_space_names_are_refused() {
	// A name of dots alone would collide with the dot-entry semantics (its short basis
	// strips to nothing), and trailing dots or spaces are invalid on the media's home
	// systems. All must refuse cleanly, on exFAT through the same gate.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	for name in [b".".as_slice(), b"..", b"...", b"note.", b"note ", b"DOCS/."] {
		assert_eq!(fs.write_file(name, b"x"), Err(FsError::Invalid), "{name:?} must be refused");
	}
	let mut ex = FatFs::mount(MemDisk { data: build_exfat(ROOT) }).unwrap();
	assert_eq!(ex.write_file(b"..", b"x"), Err(FsError::Invalid));
}
