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
	build_exfat_tree(files, nfc_files, &[])
}

// The subdirectory-aware variant: each named directory gets one FAT-chained empty
// cluster (exFAT directories carry no dot entries) and a directory-attributed entry
// set in the root, for the directory-grow tests.
fn build_exfat_tree(files: &[File], nfc_files: &[File], dirs: &[&str]) -> Vec<u8> {
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
	for d in dirs {
		let cluster = next;
		next += 1;
		set_fat(&mut fat, 4, cluster, 0x0FFF_FFFF);
		let idx = cluster - 2;
		bm[idx / 8] |= 1 << (idx % 8);
		push_exfat_entry_ex(&mut root, d, bps as u64, cluster as u32, false, true);
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
	push_exfat_entry_ex(dir, name, size, cluster, nfc, false);
}

fn push_exfat_entry_ex(dir: &mut Vec<u8>, name: &str, size: u64, cluster: u32, nfc: bool, is_dir: bool) {
	let units: Vec<u16> = name.encode_utf16().collect();
	let name_frags = units.len().div_ceil(15);
	let mut set: Vec<u8> = Vec::new();
	let mut file = [0u8; 32];
	file[0] = 0x85;
	file[1] = (1 + name_frags) as u8;
	if is_dir {
		file[4] = 0x10;
	}
	let mut stream = [0u8; 32];
	stream[0] = 0xC0;
	stream[1] = if nfc { 0x03 } else { 0x01 };
	stream[3] = units.len() as u8;
	stream[8..16].copy_from_slice(&size.to_le_bytes());
	stream[20..24].copy_from_slice(&cluster.to_le_bytes());
	stream[24..32].copy_from_slice(&size.to_le_bytes());
	set.extend_from_slice(&file);
	set.extend_from_slice(&stream);
	for f in 0..name_frags {
		let mut e = [0u8; 32];
		e[0] = 0xC1;
		for c in 0..15 {
			let idx = f * 15 + c;
			let v = if idx < units.len() { units[idx] } else { 0 };
			e[2 + c * 2..4 + c * 2].copy_from_slice(&v.to_le_bytes());
		}
		set.extend_from_slice(&e);
	}
	// stamp the set checksum, as a real formatter would - the parser verifies it.
	let sum = exfat_set_checksum(&set);
	set[2..4].copy_from_slice(&sum.to_le_bytes());
	dir.extend_from_slice(&set);
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
fn allocated_clusters<D: BlockDevice>(fs: &mut FatFs<D>) -> usize {
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
	let img = build_exfat_nfc(&[], &[File { path: "backup.img", data: leaked }]);
	let heap = 25usize; // 24 reserved + 1 FAT sector
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	assert_eq!(fs.read_file(b"backup.img").unwrap(), data);
	fs.remove(b"backup.img").unwrap();
	assert_eq!(fs.read_file(b"backup.img"), Err(FsError::NotFound));
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
	// the "next free cluster" hint must track the allocation too (its last cluster,
	// the spec's convention) instead of going stale: root 2, ROOT took 3..=6, the
	// three fresh clusters are 7, 8, 9.
	let hint = u32::from_le_bytes(fs.dev.data[free_at + 4..free_at + 8].try_into().unwrap());
	assert_eq!(hint, 9, "the next-free hint must follow the allocation");
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
	// and overflow the cluster arithmetic. An adversary authoring the volume offline
	// computes a VALID set checksum, so the size gate must hold behind the checksum
	// gate. Both paths must refuse it as Invalid.
	let img = build_exfat_nfc(&[], &[File { path: "backup.img", data: b"real bytes" }]);
	let heap = 25usize; // 24 reserved + 1 FAT sector
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	// the root: the 0x81 bitmap entry, then the 0x85 file and its 0xC0 stream entry,
	// whose data length lives at byte 24; restamp the set checksum after the forgery.
	let set_at = (heap + 1) * 512 + 32;
	let stream = set_at + 32;
	fs.dev.data[stream + 24..stream + 32].copy_from_slice(&u64::MAX.to_le_bytes());
	let count = fs.dev.data[set_at + 1] as usize + 1;
	let sum = exfat_set_checksum(&fs.dev.data[set_at..set_at + count * 32]);
	fs.dev.data[set_at + 2..set_at + 4].copy_from_slice(&sum.to_le_bytes());
	assert_eq!(fs.read_file(b"backup.img"), Err(FsError::Invalid));
	// the remove is durable (the entry clears) but its release refuses the forged
	// run: the clusters stay marked - a bounded leak, never a foreign free.
	fs.remove(b"backup.img").unwrap();
	assert_eq!(fs.read_file(b"backup.img"), Err(FsError::NotFound));
	assert_eq!(fs.dev.data[heap * 512], 0b111, "no bitmap bit may change under a refused release");
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

// A device that fails exactly one write (the `until_fail`-th), then recovers - the
// fault injection the mid-allocation unwind needs.
struct FlakyDisk {
	inner: MemDisk,
	until_fail: usize,
	failed: bool,
}

impl BlockDevice for FlakyDisk {
	fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		self.inner.read_sector(lba, buf)
	}

	fn write_sector(&mut self, lba: u64, buf: &[u8]) -> bool {
		if !self.failed {
			if self.until_fail == 0 {
				self.failed = true;
				return false;
			}
			self.until_fail -= 1;
		}
		self.inner.write_sector(lba, buf)
	}
}

#[test]
fn a_corrupt_chain_never_escapes_the_volume() {
	// Cluster values off the medium used to become sector and FAT offsets unchecked: a
	// corrupt next pointing outside the heap made read_chain read foreign device bytes
	// into a file, and free_chain WRITE a FAT slot whose offset lands in the volume's
	// own data (or, on a device larger than the volume, beyond the volume entirely).
	// The reads must refuse and the free must stop - no byte outside the FAT and the
	// root region may change.
	let mut img = build_fat(Kind::Fat16, ROOT);
	let volume_end = img.len();
	img.extend(core::iter::repeat_n(0xEEu8, 100 * 512)); // foreign bytes past the volume
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	fs.write_file(b"BIG.BIN", &[0x42u8; 700]).unwrap(); // clusters 6, 7
	let fat_off = 512; // reserved = 1 sector
	img_set_fat16(&mut fs.dev.data, fat_off, 6, 0xF000); // out of the heap, not an end marker
	assert_eq!(fs.read_file(b"BIG.BIN"), Err(FsError::Invalid), "a foreign cluster must never be read");
	let before = fs.dev.data.clone();
	fs.remove(b"BIG.BIN").unwrap(); // best-effort free: stops at the corrupt link
	assert_eq!(fs.read_file(b"BIG.BIN"), Err(FsError::NotFound));
	// only the FAT and the fixed root region (sectors 1..53) may differ; the boot
	// sector, the whole data region, and the bytes past the volume must be untouched.
	let allowed = 512..53 * 512;
	for (i, (a, b)) in before.iter().zip(&fs.dev.data).enumerate() {
		if !allowed.contains(&i) {
			assert_eq!(a, b, "byte {i:#x} changed outside the FAT and root region (volume ends at {volume_end:#x})");
		}
	}
}

// Set a FAT16 entry directly in an image, for corrupting chains under test.
fn img_set_fat16(img: &mut [u8], fat_off: usize, cluster: usize, val: u16) {
	img[fat_off + cluster * 2..fat_off + cluster * 2 + 2].copy_from_slice(&val.to_le_bytes());
}

#[test]
fn a_full_exfat_root_directory_grows() {
	// The exFAT root is a FAT chain like any directory: once its cluster fills with
	// entry sets, a write must grow it by a cluster (the root has no parent record to
	// update), not refuse with NoSpace.
	let mut fs = FatFs::mount(MemDisk { data: build_exfat(&[]) }).unwrap();
	for i in 0..8u32 {
		let name = alloc::format!("F{i}.TXT");
		let body = alloc::format!("body {i}");
		fs.write_file(name.as_bytes(), body.as_bytes()).unwrap();
	}
	assert_eq!(fs.list().unwrap().len(), 8);
	for i in 0..8u32 {
		let name = alloc::format!("F{i}.TXT");
		let body = alloc::format!("body {i}");
		assert_eq!(fs.read_file(name.as_bytes()).unwrap(), body.as_bytes());
	}
}

#[test]
fn a_full_exfat_subdirectory_grows_and_updates_its_parent_record() {
	// Growing an exFAT subdirectory must also grow the DataLength / ValidDataLength
	// recorded in its entry set in the PARENT, and restamp the set checksum - or other
	// systems see a directory shorter than its chain.
	let heap = 25usize; // 24 reserved + 1 FAT sector
	let mut fs = FatFs::mount(MemDisk { data: build_exfat_tree(&[], &[], &["SUB"]) }).unwrap();
	for i in 0..6u32 {
		let name = alloc::format!("SUB/F{i}.TXT");
		fs.write_file(name.as_bytes(), b"in the subdir").unwrap();
	}
	assert_eq!(fs.list_dir(b"SUB").unwrap().len(), 6);
	for i in 0..6u32 {
		let name = alloc::format!("SUB/F{i}.TXT");
		assert_eq!(fs.read_file(name.as_bytes()).unwrap(), b"in the subdir");
	}
	// SUB's entry set sits right after the bitmap entry in the root: 0x85 at 32, the
	// 0xC0 stream at 64. Both recorded lengths must now be two clusters, and the set
	// checksum must match a recomputation.
	let root_off = (heap + 1) * 512;
	let stream = root_off + 64;
	let valid = u64::from_le_bytes(fs.dev.data[stream + 8..stream + 16].try_into().unwrap());
	let data = u64::from_le_bytes(fs.dev.data[stream + 24..stream + 32].try_into().unwrap());
	assert_eq!((valid, data), (1024, 1024), "the parent record must grow with the directory");
	let stored = u16::from_le_bytes(fs.dev.data[root_off + 34..root_off + 36].try_into().unwrap());
	assert_eq!(stored, exfat_set_checksum(&fs.dev.data[root_off + 32..root_off + 128]), "the set checksum must be restamped");
}

#[test]
fn illegal_long_name_characters_are_refused() {
	// The characters illegal in a long name on the media's home systems must never
	// reach the LFN / 0xC1 fragments - a written file must stay openable there.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	for name in [b"bad*.txt".as_slice(), b"a:b.txt", b"q?.txt", b"lt<.txt", b"gt>.txt", b"pi|pe.txt", b"qu\"ote.txt", b"back\\slash.txt", b"ctrl\x01.txt"] {
		assert_eq!(fs.write_file(name, b"x"), Err(FsError::Invalid), "{name:?} must be refused");
	}
	let mut ex = FatFs::mount(MemDisk { data: build_exfat(ROOT) }).unwrap();
	assert_eq!(ex.write_file(b"bad*.txt", b"x"), Err(FsError::Invalid));
}

#[test]
fn degenerate_boot_pointers_do_not_mount() {
	// A boot sector whose pointers cannot form a volume mounts as an empty or
	// piecemeal-failing volume today's checks miss - refuse each at mount.
	let mut fat32_root0 = build_fat(Kind::Fat32, ROOT);
	fat32_root0[44..48].copy_from_slice(&0u32.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: fat32_root0 }).is_none(), "a FAT32 root below the heap");
	let mut fat32_root1 = build_fat(Kind::Fat32, ROOT);
	fat32_root1[44..48].copy_from_slice(&1u32.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: fat32_root1 }).is_none());
	let mut ex_fat0 = build_exfat(ROOT);
	ex_fat0[84..88].copy_from_slice(&0u32.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: ex_fat0 }).is_none(), "an exFAT with no FAT");
	let mut ex_off0 = build_exfat(ROOT);
	ex_off0[80..84].copy_from_slice(&0u32.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: ex_off0 }).is_none(), "an exFAT FAT in the boot region");
	let mut ex_root1 = build_exfat(ROOT);
	ex_root1[96..100].copy_from_slice(&1u32.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: ex_root1 }).is_none(), "an exFAT root below the heap");
	// roots past the heap fail only at the first read today - refuse them at mount
	// like every other out-of-range geometry field.
	let mut fat32_root_high = build_fat(Kind::Fat32, ROOT);
	fat32_root_high[44..48].copy_from_slice(&70000u32.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: fat32_root_high }).is_none(), "a FAT32 root past the heap");
	let mut ex_root_high = build_exfat(ROOT);
	ex_root_high[96..100].copy_from_slice(&70000u32.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: ex_root_high }).is_none(), "an exFAT root past the heap");
	// a classic volume with no root region (the 16-bit FAT size keeps it classic, so
	// the FAT32 shape rule does not claim it) - nothing could ever live in its root.
	let mut zero_root = build_fat(Kind::Fat16, ROOT);
	zero_root[17..19].copy_from_slice(&0u16.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: zero_root }).is_none(), "a classic volume with no root region");
	// a sectors-per-cluster the specification does not allow (a power of two up to
	// 128 sectors only).
	let mut odd_spc = build_fat(Kind::Fat16, ROOT);
	odd_spc[13] = 3;
	assert!(FatFs::mount(MemDisk { data: odd_spc }).is_none(), "a non-power-of-two spc");
	odd_spc = build_fat(Kind::Fat16, ROOT);
	odd_spc[13] = 200;
	assert!(FatFs::mount(MemDisk { data: odd_spc }).is_none(), "a 200-sector cluster");
	// a cluster count past the spec ceiling would make the BAD-cluster marker a
	// "valid" cluster index the chain walks would follow as data.
	let mut huge_count = build_exfat(ROOT);
	huge_count[92..96].copy_from_slice(&0x0FFF_FFF4u32.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: huge_count }).is_none(), "a cluster count past the spec ceiling");
	// and a logical sector size the specification does not allow (not a power of two).
	let mut odd_bps = build_fat(Kind::Fat16, ROOT);
	odd_bps[11..13].copy_from_slice(&3584u16.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: odd_bps }).is_none(), "a non-power-of-two sector size");
}

#[test]
fn a_failed_link_write_unwinds_the_allocation() {
	// An I/O failure while linking a fresh chain used to leave the already-written FAT
	// slots behind - orphan clusters no directory entry names. The allocation must
	// unwind them.
	let inner = MemDisk { data: build_fat(Kind::Fat16, ROOT) };
	let mut fs = FatFs::mount(FlakyDisk { inner, until_fail: usize::MAX, failed: true }).unwrap();
	let before = allocated_clusters(&mut fs);
	// the first writes of a write_file are the chain links (2 sectors per entry): let
	// the first entry land and fail the second, mid-loop.
	fs.dev.failed = false;
	fs.dev.until_fail = 2;
	assert_eq!(fs.write_file(b"BIG.BIN", &[0x11u8; 1500]), Err(FsError::Io));
	assert_eq!(allocated_clusters(&mut fs), before, "a failed allocation must leak nothing");
	assert_eq!(fs.read_file(b"BIG.BIN"), Err(FsError::NotFound));
}

#[test]
fn written_entries_carry_the_volume_clock() {
	// Entries used to carry create/write time 0 - an invalid DOS date (day 0, month
	// 0). With the clock set they must carry its DOS encoding, and without it the
	// valid epoch date 1980-01-01.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	fs.write_file(b"EPOCH.TXT", b"unset clock").unwrap();
	fs.set_clock(946_684_800); // 2000-01-01 00:00:00 UTC
	fs.write_file(b"STAMP.TXT", b"set clock").unwrap();
	let date_2000 = ((2000u16 - 1980) << 9) | (1 << 5) | 1;
	assert_eq!(root_entry_dates(&fs.dev.data, b"EPOCH   TXT"), ((1 << 5) | 1, (1 << 5) | 1), "an unset clock must still yield 1980-01-01");
	assert_eq!(root_entry_dates(&fs.dev.data, b"STAMP   TXT"), (date_2000, date_2000));
	// the exFAT form: the 32-bit timestamp (date high, time low), marked UTC.
	let mut ex = FatFs::mount(MemDisk { data: build_exfat(&[]) }).unwrap();
	ex.set_clock(946_684_800);
	ex.write_file(b"S.TXT", b"stamped").unwrap();
	let heap = 25usize;
	let root_off = (heap + 1) * 512;
	let e = &ex.dev.data[root_off + 32..root_off + 64]; // the set after the bitmap entry
	assert_eq!(e[0], 0x85);
	let ts = (date_2000 as u32) << 16;
	assert_eq!(u32::from_le_bytes(e[8..12].try_into().unwrap()), ts, "the exFAT create timestamp");
	assert_eq!(u32::from_le_bytes(e[12..16].try_into().unwrap()), ts, "the exFAT modify timestamp");
	assert_eq!(e[22] & 0x80, 0x80, "the timestamp must be marked UTC");
}

// The (create date, write date) of the fixed-root entry whose 8.3 field is `short`.
fn root_entry_dates(img: &[u8], short: &[u8; 11]) -> (u16, u16) {
	let root_off = 21 * 512; // reserved 1 + FAT 20 sectors
	let mut i = root_off;
	while img[i] != 0x00 {
		if &img[i..i + 11] == short {
			return (u16::from_le_bytes([img[i + 16], img[i + 17]]), u16::from_le_bytes([img[i + 24], img[i + 25]]));
		}
		i += 32;
	}
	panic!("entry {short:?} not found");
}

#[test]
fn a_degenerate_exfat_entry_set_is_skipped() {
	// A bare 0x85 with no secondaries (or a forged zero name length) is noise, never a
	// real file - it must not surface as an empty-named entry in a listing.
	let mut img = build_exfat(ROOT);
	let heap = 25usize;
	let root_off = (heap + 1) * 512;
	// the root holds the bitmap entry plus three 3-record file sets = 10 slots; plant
	// the bare 0x85 in the free slot after them.
	img[root_off + 10 * 32] = 0x85;
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	let list = fs.list().unwrap();
	assert!(list.iter().all(|e| !e.name.is_empty()), "an empty-named entry surfaced: {list:?}");
	assert!(list.iter().any(|e| e.name == "HELLO.TXT"));
}

#[test]
fn orphan_lfn_fragments_never_corrupt_a_neighbors_name() {
	// A non-LFN-aware tool deletes only the 8.3 record and leaves the fragments
	// behind: unchecked, the orphans merged with the NEXT file's fragments into one
	// garbage name. Orphans must be discarded and the neighbor keeps its real name.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, &[]) }).unwrap();
	fs.write_file(b"Alpha file.txt", b"alpha").unwrap();
	// plant an orphan fragment (a mid-set sequence with a bogus checksum) in the free
	// slot right after Alpha's set, then write Beta - its set lands after the orphan.
	let root_off = 21 * 512; // reserved 1 + FAT 20 sectors
	let slot = root_off + 3 * 32; // Alpha's set is 2 fragments + the 8.3 entry
	fs.dev.data[slot] = 0x03;
	fs.dev.data[slot + 11] = 0x0F;
	fs.dev.data[slot + 13] = 0xAB;
	fs.write_file(b"Beta file.txt", b"beta").unwrap();
	assert_eq!(fs.read_file(b"Beta file.txt").unwrap(), b"beta");
	assert_eq!(fs.read_file(b"Alpha file.txt").unwrap(), b"alpha");
	assert_eq!(names(&fs.list().unwrap()), ["Alpha file.txt", "Beta file.txt"]);
	// and a real set whose fragment checksum is tampered falls back to its 8.3 name,
	// which still resolves - the file is never lost, only its long form.
	fs.dev.data[root_off + 13] ^= 0x55;
	let after = names(&fs.list().unwrap());
	assert!(after.contains(&"ALPHA_~1.TXT".to_string()), "{after:?}");
	assert!(!after.contains(&"Alpha file.txt".to_string()), "{after:?}");
	assert_eq!(fs.read_file(b"ALPHA_~1.TXT").unwrap(), b"alpha");
}

#[test]
fn a_torn_exfat_entry_set_is_skipped_not_trusted() {
	// A power cut can tear an entry set half old / half new: the stored checksum no
	// longer matches, and trusting the set would serve garbage metadata. It must be
	// skipped, the healthy neighbors unaffected.
	let mut fs = FatFs::mount(MemDisk { data: build_exfat(ROOT) }).unwrap();
	let root_off = (25 + 1) * 512;
	// HELLO.TXT's set follows the bitmap entry; corrupt one byte of its stream record
	// without restamping the set checksum.
	fs.dev.data[root_off + 64 + 24] ^= 0x01;
	assert_eq!(fs.read_file(b"HELLO.TXT"), Err(FsError::NotFound));
	assert_eq!(fs.read_file(b"readme.md").unwrap(), b"long name file");
	assert!(!names(&fs.list().unwrap()).contains(&"HELLO.TXT".to_string()));
}

#[test]
fn a_zero_reserved_bpb_and_an_overlapping_exfat_fat_do_not_mount() {
	// A zero reserved count puts the FAT region at the boot sector (the first FAT
	// write would overwrite it), and an exFAT FAT running into the cluster heap makes
	// a FAT-slot write clobber file data - both layouts are refused at mount.
	let mut zero_res = build_fat(Kind::Fat16, ROOT);
	zero_res[14..16].copy_from_slice(&0u16.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: zero_res }).is_none(), "a FAT region at the boot sector");
	let mut overlap = build_exfat(ROOT);
	overlap[84..88].copy_from_slice(&100u32.to_le_bytes()); // the FAT runs into the heap at 25
	assert!(FatFs::mount(MemDisk { data: overlap }).is_none(), "a FAT overlapping the cluster heap");
}

// A device that fails the first armed write to one specific LBA (after letting `skip`
// earlier armed writes to it pass), then heals - for pinning write-ordering guarantees.
struct FailAt {
	inner: MemDisk,
	lba: u64,
	armed: bool,
	skip: usize,
}

impl BlockDevice for FailAt {
	fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		self.inner.read_sector(lba, buf)
	}

	fn write_sector(&mut self, lba: u64, buf: &[u8]) -> bool {
		if self.armed && lba == self.lba {
			if self.skip == 0 {
				self.armed = false;
				return false;
			}
			self.skip -= 1;
		}
		self.inner.write_sector(lba, buf)
	}
}

#[test]
fn a_grow_cluster_reaches_the_chain_only_zeroed() {
	// grow links a fresh cluster into the directory chain; its stale on-device bytes
	// must be zeroed BEFORE the link, or a failure of the later directory write leaves
	// garbage the parser reads as entries (and a remove could then free foreign
	// clusters). Fail the directory write right after a grow and inspect the tail.
	let inner = MemDisk { data: build_fat(Kind::Fat16, ROOT) };
	let mut fs = FatFs::mount(FailAt { inner, lba: 0, armed: false, skip: 0 }).unwrap();
	// fill DOCS to exactly one 16-entry cluster: ".", "..", the LFN + 8.3 pair of
	// a.txt, plus 12 more.
	for i in 0..12u32 {
		let name = alloc::format!("DOCS/F{i}.TXT");
		fs.write_file(name.as_bytes(), b"x").unwrap();
	}
	// plant entry-like garbage in the free clusters the next write will allocate from.
	let max = fs.max_cluster();
	let free: Vec<u32> = (2..=max).filter(|&c| fs.next_cluster(c).unwrap() == 0).take(4).collect();
	for &c in &free {
		let at = fs.cluster_fs_sector(c) as usize * 512;
		fs.dev.inner.data[at..at + 512].fill(b'A');
	}
	// the next write grows DOCS: the data chain takes the first free cluster, the grow
	// the second. Let the grow's zeroing write to it pass and fail the directory
	// content write that follows - the tail stays linked with only the zeros.
	fs.dev.lba = fs.cluster_fs_sector(free[1]);
	fs.dev.skip = 1;
	fs.dev.armed = true;
	assert_eq!(fs.write_file(b"DOCS/F12.TXT", b"y"), Err(FsError::Io));
	fs.dev.armed = false;
	// the tail cluster is linked but zeroed - the listing shows only the real entries.
	let listed = names(&fs.list_dir(b"DOCS").unwrap());
	assert_eq!(listed.len(), 13, "garbage entries surfaced in the grown tail: {listed:?}");
	assert!(listed.iter().all(|n| n == "a.txt" || (n.starts_with('F') && n.ends_with(".TXT"))), "{listed:?}");
}

// A device that counts its sector reads, for pinning I/O-cost bounds.
struct CountingDisk {
	inner: MemDisk,
	reads: usize,
}

impl BlockDevice for CountingDisk {
	fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		self.reads += 1;
		self.inner.read_sector(lba, buf)
	}

	fn write_sector(&mut self, lba: u64, buf: &[u8]) -> bool {
		self.inner.write_sector(lba, buf)
	}
}

#[test]
fn a_write_on_a_full_volume_reads_the_fat_once_not_per_cluster() {
	// The allocation scan used to read the FAT off the device per candidate cluster -
	// two sectors for each of the thousands of allocated clusters it skips on a
	// fuller volume. A small write must cost on the order of one FAT image read.
	let inner = MemDisk { data: build_fat(Kind::Fat16, ROOT) };
	let mut fs = FatFs::mount(CountingDisk { inner, reads: 0 }).unwrap();
	let chunk = vec![0x5Au8; 500 * 512];
	for i in 0..8u32 {
		let name = alloc::format!("FILL{i}.BIN");
		fs.write_file(name.as_bytes(), &chunk).unwrap();
	}
	fs.dev.reads = 0;
	fs.write_file(b"SMALL.TXT", b"tiny").unwrap();
	assert!(fs.dev.reads < 1000, "a small write cost {} sector reads", fs.dev.reads);
}

#[test]
fn an_all_spaces_classic_entry_is_skipped() {
	// An 8.3 field of nothing but padding decodes to an empty name - noise on hostile
	// media, and `read_file(b"")` used to match it.
	let mut img = build_fat(Kind::Fat16, ROOT);
	let root_off = 21 * 512;
	let slot = root_off + 4 * 32; // the first free slot past ROOT's four records
	img[slot..slot + 11].fill(0x20);
	img[slot + 11] = 0x20; // attributes: an ordinary file
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	assert!(names(&fs.list().unwrap()).iter().all(|n| !n.is_empty()));
	assert_eq!(fs.read_file(b""), Err(FsError::NotFound));
}

#[test]
fn a_lowercase_exfat_name_carries_the_upcased_hash() {
	// The NameHash is defined over the UP-CASED name: the media's home systems
	// recompute it on lookup and skip a mismatched set, so a hash over the name as
	// written left every lowercase-named file listable but unopenable by name there.
	let mut fs = FatFs::mount(MemDisk { data: build_exfat(&[]) }).unwrap();
	fs.write_file(b"hello.txt", b"cased").unwrap();
	let heap = 25usize; // 24 reserved + 1 FAT sector
	let root_off = (heap + 1) * 512;
	// the set follows the bitmap entry: 0x85 at 32, the 0xC0 stream at 64; the hash
	// sits at stream bytes 4..6. Compare against an independent computation over the
	// up-cased UTF-16LE name.
	let stream = root_off + 64;
	assert_eq!(fs.dev.data[stream], 0xC0);
	let stored = u16::from_le_bytes(fs.dev.data[stream + 4..stream + 6].try_into().unwrap());
	let mut expect: u16 = 0;
	for u in "HELLO.TXT".encode_utf16() {
		for b in u.to_le_bytes() {
			expect = expect.rotate_right(1).wrapping_add(b as u16);
		}
	}
	assert_eq!(stored, expect, "the NameHash must be over the up-cased name");
	assert_eq!(fs.read_file(b"hello.txt").unwrap(), b"cased");
}

#[test]
fn a_non_utf8_name_is_refused_not_stored_unreachable() {
	// A latin-1 0xE9 passes the byte gates but is not valid UTF-8: the long-name forms
	// store UTF-16, so the name would be stored lossily (U+FFFD) and the file never
	// found again by the bytes it was created with - a write that succeeds must stay
	// reachable, so the name is refused instead.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	assert_eq!(fs.write_file(b"caf\xE9.txt", b"x"), Err(FsError::Invalid));
	let mut ex = FatFs::mount(MemDisk { data: build_exfat(ROOT) }).unwrap();
	assert_eq!(ex.write_file(b"caf\xE9.txt", b"x"), Err(FsError::Invalid));
}

// A device that records every written LBA, for pinning which sectors an operation
// touches.
struct WriteLog {
	inner: MemDisk,
	writes: Vec<u64>,
}

impl BlockDevice for WriteLog {
	fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		self.inner.read_sector(lba, buf)
	}

	fn write_sector(&mut self, lba: u64, buf: &[u8]) -> bool {
		self.writes.push(lba);
		self.inner.write_sector(lba, buf)
	}
}

#[test]
fn a_fat12_slot_write_touches_only_its_sectors() {
	// The FAT12 read-modify-write used to touch two sectors even for a slot wholly
	// inside one - when the slot sat in the FAT's last sector, the RMW rewrote the
	// sector PAST the FAT (the root region's first): a torn-write window on a region
	// the operation never meant to touch. One sector, unless the slot straddles.
	let inner = MemDisk { data: build_fat_sized(Kind::Fat12, ROOT, 1000) };
	let mut fs = FatFs::mount(WriteLog { inner, writes: Vec::new() }).unwrap();
	fs.set_fat_entry(2, 0x123).unwrap(); // byte offset 3: wholly inside the first FAT sector
	assert_eq!(fs.dev.writes, [1], "a non-straddling slot must touch one sector");
	fs.dev.writes.clear();
	fs.set_fat_entry(341, 0x456).unwrap(); // byte offset 511: straddles into the second
	assert_eq!(fs.dev.writes, [1, 2], "a straddling slot needs exactly the pair");
}

#[test]
fn a_volume_claiming_more_than_the_device_does_not_mount() {
	// The geometry is the medium's own claim: a forged BPB whose total (or FAT size)
	// reaches past the real device used to mount - internally consistent regions - and
	// the first write attempt then built the whole claimed FAT image in memory. The
	// claimed volume end must exist on the device, or the mount is refused.
	let mut big_total = build_fat(Kind::Fat16, ROOT);
	big_total[19..21].copy_from_slice(&0u16.to_le_bytes());
	big_total[32..36].copy_from_slice(&60000u32.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: big_total }).is_none(), "a total past the device end");
	// a huge claimed FAT (the size whose in-memory image the allocator builds),
	// with a total sized to keep the layout internally consistent.
	let mut big_fat = build_fat(Kind::Fat32, ROOT);
	big_fat[36..40].copy_from_slice(&0x00FF_FFFFu32.to_le_bytes());
	big_fat[32..36].copy_from_slice(&0x0110_0000u32.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: big_fat }).is_none(), "a FAT past the device end");
	let mut big_heap = build_exfat(ROOT);
	big_heap[92..96].copy_from_slice(&1_000_000u32.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: big_heap }).is_none(), "a heap past the device end");
	// an honestly sized volume still mounts - the probe reads its very last sector.
	assert!(FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).is_some());
}

#[test]
fn a_one_entry_mutation_writes_only_the_clusters_it_touches() {
	// A directory mutation used to rewrite the WHOLE directory - write amplification,
	// and a power cut mid-rewrite could tear entries unrelated to the operation.
	// Removing an entry in the first cluster of a two-cluster directory must write
	// that cluster and never rewrite the second.
	let inner = MemDisk { data: build_fat(Kind::Fat16, ROOT) };
	let mut fs = FatFs::mount(WriteLog { inner, writes: Vec::new() }).unwrap();
	// DOCS starts with 4 slots (".", "..", a.txt's LFN pair); 14 more entries span
	// two 16-slot clusters.
	for i in 0..14u32 {
		let name = alloc::format!("DOCS/F{i}.TXT");
		fs.write_file(name.as_bytes(), b"x").unwrap();
	}
	let docs = fs.resolve_dir(b"DOCS").unwrap();
	let second = fs.next_cluster(docs.cluster).unwrap();
	assert!(second >= 2 && !fs.is_end(second), "DOCS must span two clusters");
	let (lba1, lba2) = (fs.cluster_fs_sector(docs.cluster), fs.cluster_fs_sector(second));
	fs.dev.writes.clear();
	fs.remove(b"DOCS/F0.TXT").unwrap();
	assert!(fs.dev.writes.contains(&lba1), "the touched cluster must be written: {:?}", fs.dev.writes);
	assert!(!fs.dev.writes.contains(&lba2), "the untouched cluster must not be rewritten: {:?}", fs.dev.writes);
}

#[test]
fn a_non_ascii_name_resolves_by_its_exact_bytes() {
	// Case folding is deliberately ASCII-only (the media's home systems fold the full
	// range through their upcase table); a non-ASCII name must always resolve by the
	// exact bytes it was written with.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	let name = "Caf\u{E9}.txt".as_bytes();
	fs.write_file(name, b"accented").unwrap();
	assert_eq!(fs.read_file(name).unwrap(), b"accented");
	assert!(fs.list().unwrap().iter().any(|e| e.name.as_bytes() == name));
}

// Set the ValidDataLength of the exFAT entry set at `set_at` and restamp its checksum.
fn restamp_vdl(img: &mut [u8], set_at: usize, vdl: u64) {
	img[set_at + 32 + 8..set_at + 32 + 16].copy_from_slice(&vdl.to_le_bytes());
	let count = img[set_at + 1] as usize + 1;
	let sum = exfat_set_checksum(&img[set_at..set_at + count * 32]);
	img[set_at + 2..set_at + 4].copy_from_slice(&sum.to_le_bytes());
}

#[test]
fn a_preallocated_exfat_tail_reads_as_zeros() {
	// The VDL..DataLength range is undefined on disk and the media's home systems
	// serve it as zeros: a preallocated tail (SetEndOfFile, download managers) must
	// never leak stale cluster content - it can hold someone else's deleted data.
	let data: Vec<u8> = (0..1500u32).map(|i| (i * 3) as u8).collect();
	let leaked: &'static [u8] = Box::leak(data.clone().into_boxed_slice());
	let img = build_exfat_nfc(&[File { path: "HELLO.TXT", data: b"Hello, FAT!" }], &[File { path: "backup.img", data: leaked }]);
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	// HELLO.TXT's chained set follows the bitmap entry; backup.img's NoFatChain set
	// follows it. Cut each VDL below the DataLength and restamp the checksums.
	let root_off = (25 + 1) * 512;
	restamp_vdl(&mut fs.dev.data, root_off + 32, 5);
	restamp_vdl(&mut fs.dev.data, root_off + 32 + 96, 700);
	let hello = fs.read_file(b"HELLO.TXT").unwrap();
	assert_eq!(hello.len(), 11);
	assert_eq!(&hello[..5], b"Hello");
	assert!(hello[5..].iter().all(|&b| b == 0), "the chained tail must read as zeros: {hello:?}");
	let backup = fs.read_file(b"backup.img").unwrap();
	assert_eq!(backup.len(), 1500);
	assert_eq!(&backup[..700], &data[..700]);
	assert!(backup[700..].iter().all(|&b| b == 0), "the NoFatChain tail must read as zeros");
}

#[test]
fn a_chained_exfat_directory_reads_by_its_recorded_length() {
	// Windows reads a chained directory by its recorded DataLength; a chain longer
	// than the record (inconsistent foreign media) must not surface extra entries.
	let mut fs = FatFs::mount(MemDisk { data: build_exfat_tree(&[], &[], &["SUB"]) }).unwrap();
	fs.write_file(b"SUB/real.txt", b"real").unwrap();
	// forge: link one more cluster onto SUB's chain and plant a checksum-valid entry
	// set in it, leaving the recorded DataLength at one cluster.
	let heap = 25usize;
	let sub = 4u32; // the builder's first directory cluster
	let ghost = 30u32;
	let mut set: Vec<u8> = Vec::new();
	push_exfat_entry(&mut set, "GHOST.TXT", 0, 0, false);
	let at = (heap + ghost as usize - 2) * 512;
	fs.dev.data[at..at + set.len()].copy_from_slice(&set);
	let fat = 24 * 512;
	fs.dev.data[fat + sub as usize * 4..fat + sub as usize * 4 + 4].copy_from_slice(&ghost.to_le_bytes());
	fs.dev.data[fat + ghost as usize * 4..fat + ghost as usize * 4 + 4].copy_from_slice(&0x0FFF_FFF8u32.to_le_bytes());
	let listed = names(&fs.list_dir(b"SUB").unwrap());
	assert_eq!(listed, ["real.txt"], "the ghost entry past the record must not surface");
}

#[test]
fn a_zero_length_read_reads_no_data_cluster() {
	// An empty file whose entry carries a nonzero first cluster (foreign media) used
	// to read one whole cluster and discard it - the read must cost only the
	// directory scan.
	let inner = MemDisk { data: build_fat(Kind::Fat16, ROOT) };
	let mut fs = FatFs::mount(CountingDisk { inner, reads: 0 }).unwrap();
	// HELLO.TXT is the first root entry: claim size 0, keep its first cluster.
	let root_off = 21 * 512;
	fs.dev.inner.data[root_off + 28..root_off + 32].copy_from_slice(&0u32.to_le_bytes());
	fs.dev.reads = 0;
	assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), b"");
	assert_eq!(fs.dev.reads, 32, "only the 32-sector root region may be read");
}

#[test]
fn a_directory_lists_with_size_zero() {
	// The FileInfo contract: a directory reports a length of zero. The exFAT entry
	// records the directory's DataLength there - it must not leak into the listing.
	let mut fs = FatFs::mount(MemDisk { data: build_exfat_tree(&[], &[], &["SUB"]) }).unwrap();
	let list = fs.list().unwrap();
	let sub = list.iter().find(|e| e.name == "SUB").unwrap();
	assert!(sub.is_dir);
	assert_eq!(sub.size, 0, "a directory must list with size zero");
}

#[test]
fn nt_case_flags_render_a_lowercase_short_name() {
	// A short-only lowercase name is stored by the media's home systems as an
	// uppercase 8.3 field plus the NT case flags (byte 12), not as a long-name set -
	// the listing must render what they display.
	let mut img = build_fat(Kind::Fat16, ROOT);
	let root_off = 21 * 512;
	let slot = root_off + 4 * 32; // the first free slot past ROOT's four records
	img[slot..slot + 11].copy_from_slice(b"NOTES   TXT");
	img[slot + 11] = 0x20;
	img[slot + 12] = 0x18; // NT flags: lowercase base + lowercase extension
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	let listed = names(&fs.list().unwrap());
	assert!(listed.contains(&"notes.txt".to_string()), "{listed:?}");
	// the lookup stays case-insensitive in both directions.
	assert_eq!(fs.read_file(b"NOTES.TXT").unwrap(), b"");
	assert_eq!(fs.read_file(b"notes.txt").unwrap(), b"");
}

#[test]
fn a_failed_free_of_the_old_chain_does_not_fail_a_durable_write() {
	// Once the new content and its entry are on disk (or the entry is cleared, for a
	// remove), the operation is durable - a device failing during the OLD chain's
	// free must cost at most lost clusters, never a false failure.
	let inner = MemDisk { data: build_fat(Kind::Fat16, ROOT) };
	let mut fs = FatFs::mount(FlakyDisk { inner, until_fail: usize::MAX, failed: true }).unwrap();
	fs.write_file(b"OLD.BIN", &[0x22u8; 3 * 512]).unwrap();
	// the overwrite writes the new FAT link, the data cluster, and the directory
	// sector, then frees the old chain - fail that free's first FAT write.
	fs.dev.failed = false;
	fs.dev.until_fail = 3;
	fs.write_file(b"OLD.BIN", b"new content").unwrap();
	assert!(fs.dev.failed, "the injected failure must have fired");
	assert_eq!(fs.read_file(b"OLD.BIN").unwrap(), b"new content");
	// and a remove whose free fails is still a durable remove.
	fs.write_file(b"GONE.BIN", &[0x33u8; 3 * 512]).unwrap();
	fs.dev.failed = false;
	fs.dev.until_fail = 1; // the directory write passes, the free's first write fails
	fs.remove(b"GONE.BIN").unwrap();
	assert!(fs.dev.failed, "the injected failure must have fired");
	assert_eq!(fs.read_file(b"GONE.BIN"), Err(FsError::NotFound));
}
