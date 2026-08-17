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
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		let start = lba as usize * SECTOR_SIZE;
		let Some(src) = self.data.get(start..start + SECTOR_SIZE) else {
			return false;
		};
		buf.copy_from_slice(src);
		true
	}

	fn write_block(&mut self, lba: u64, buf: &[u8]) -> bool {
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
#[derive(Clone)]
struct File {
	path: &'static str,
	data: &'static [u8],
}

// Build a classic FAT image (12 / 16 / 32 chosen by `kind`) holding `files`. spc 1, one
// FAT; clusters are handed out per file/dir, FAT chains and directory entries written so
// the reader walks them exactly as it would a real disk. Subdirectories get "." / "..".
// Where a classic volume's fixed root region starts, read off the BPB rather than restated as a
// constant. Six tests carried `21 * 512 // reserved 1 + FAT 20 sectors` and every one of them broke
// the moment the fixtures grew their second FAT - a comment describing the layout is not the layout.
fn classic_root_off(img: &[u8]) -> usize {
	let bps = u16::from_le_bytes([img[11], img[12]]) as usize;
	let reserved = u16::from_le_bytes([img[14], img[15]]) as usize;
	let fats = img[16] as usize;
	let fat_size = u16::from_le_bytes([img[22], img[23]]) as usize;
	(reserved + fats * fat_size) * bps
}

// How many FAT copies the fixtures carry. Every classic formatter writes two and mirrors them.
const FATS: usize = 2;

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
	// TWO FATs, because that is what every formatter of this family writes and what the driver's
	// mirroring path exists for. A one-FAT image is legal and nothing in the wild produces it, so
	// building one meant the loop over copies had never run against the crate's own media.
	let first_data = reserved + FATS * fat_size + root_sectors;
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
		let root_off = (reserved + FATS * fat_size) * bps;
		img[root_off..root_off + root.len()].copy_from_slice(&root);
	}
	for copy in 0..FATS {
		let at = (reserved + copy * fat_size) * bps;
		img[at..at + fat.len()].copy_from_slice(&fat);
	}
	write_bpb(&mut img, bps, spc, reserved, FATS, fat_size, root_entries, total, root_cluster);
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
	let first_data = reserved + FATS * fat_size + root_sectors;
	let total = first_data + clusters;
	let mut img = vec![0u8; total * bps];
	let mut fat = vec![0u8; fat_size * bps];
	let mut next = 2;
	let mut root: Vec<u8> = Vec::new();
	for f in files {
		place_classic(&mut img, &mut fat, &mut next, &mut root, f, first_data, 12, Kind::Fat12);
	}
	let root_off = (reserved + FATS * fat_size) * bps;
	img[root_off..root_off + root.len()].copy_from_slice(&root);
	for copy in 0..FATS {
		let at = (reserved + copy * fat_size) * bps;
		img[at..at + fat.len()].copy_from_slice(&fat);
	}
	write_bpb(&mut img, bps, 1, reserved, FATS, fat_size, root_entries, total, 0);
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
		// AS MANY CLUSTERS AS THE DATA NEEDS, chained. This laid every file in one cluster and wrote
		// the full length into the entry, so a 1536-byte file was a 512-byte chain claiming three
		// times its size - media no formatter produces, and a shape the read path silently served
		// as a short file until it started checking.
		let first = *next;
		let clusters = f.data.len().div_ceil(bps).max(1);
		for k in 0..clusters {
			let cluster = first + k;
			let link = if k + 1 < clusters { (cluster + 1) as u32 } else { end_marker(kind) };
			set_fat(fat, ent, cluster, link);
			let off = (first_data + cluster - 2) * bps;
			let from = k * bps;
			let to = (from + bps).min(f.data.len());
			if from < to {
				img[off..off + (to - from)].copy_from_slice(&f.data[from..to]);
			}
		}
		*next += clusters;
		push_entry(dir, f.path, false, f.data.len() as u32, first as u32);
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

fn write_bpb(img: &mut [u8], bps: usize, spc: usize, reserved: usize, fats: usize, fat_size: usize, root_entries: usize, total: usize, root_cluster: usize) {
	img[11..13].copy_from_slice(&(bps as u16).to_le_bytes());
	img[13] = spc as u8;
	img[14..16].copy_from_slice(&(reserved as u16).to_le_bytes());
	img[16] = fats as u8;
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
	let mut next = 5;
	let mut root: Vec<u8> = Vec::new();
	// the allocation bitmap lives in cluster 2; the root in cluster 3; both stay allocated.
	// exFAT's end-of-chain is 0xFFFFFFFF. These fixtures wrote FAT32's 0x0FFF_FFFF, which is
	// precisely why an internal round trip could not see the driver writing the same wrong value.
	set_fat(&mut fat, 4, 2, 0xFFFF_FFFF);
	set_fat(&mut fat, 4, 3, 0xFFFF_FFFF);
	set_fat(&mut fat, 4, 4, 0xFFFF_FFFF);
	// A BITMAP, not a byte. `bm[0] |= 1 << (cluster - 2)` shifted past a `u8` the moment a fixture
	// had more than eight clusters, which is a limit of the builder that read as a limit of the
	// driver: the crash sweep could not use an exFAT directory large enough to span two sectors, so
	// the interleaving its publish protocol is about never happened.
	let mark = |bm: &mut Vec<u8>, cluster: usize| {
		let bit = cluster - 2;
		bm[bit / 8] |= 1 << (bit % 8);
	};
	mark(&mut bm, 2);
	mark(&mut bm, 3);
	mark(&mut bm, 4);
	push_exfat_bitmap(&mut root, 2, clusters.div_ceil(8) as u64);
	// The Up-case Table in cluster 4, and its 0x82 entry right after the bitmap's - the order a
	// formatter writes the two required entries in.
	let upcase = exfat_upcase_table();
	assert!(upcase.len() <= bps, "the fixture's table must fit the one cluster it is given");
	push_exfat_upcase(&mut root, 4, &upcase);
	// ONE CLUSTER PER FILE was the assumption, and it is not one this builder may make. A file
	// larger than a cluster was written past its own and over whatever came next, while its FAT
	// chain still said it ended at the first - so the file did not read back and the files after it
	// were corrupted. The `nfc_files` branch below already spanned clusters; this one did not, and
	// every fixture that used it happened to be small enough not to notice.
	for f in files {
		let first = next;
		let span = f.data.len().div_ceil(bps).max(1);
		next += span;
		for i in 0..span {
			let cluster = first + i;
			let link: u32 = if i + 1 < span { (cluster + 1) as u32 } else { 0xFFFF_FFFF };
			set_fat(&mut fat, 4, cluster, link);
			mark(&mut bm, cluster);
		}
		let off = (heap + first - 2) * bps;
		img[off..off + f.data.len()].copy_from_slice(f.data);
		push_exfat_entry(&mut root, f.path, f.data.len() as u64, first as u32, false);
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
		set_fat(&mut fat, 4, cluster, 0xFFFF_FFFF);
		let idx = cluster - 2;
		bm[idx / 8] |= 1 << (idx % 8);
		push_exfat_entry_ex(&mut root, d, bps as u64, cluster as u32, false, true);
	}
	let up_off = (heap + 2) * bps;
	img[up_off..up_off + upcase.len()].copy_from_slice(&upcase);
	// THE ROOT MAY NEED MORE THAN ONE CLUSTER, and this wrote it as if it never could: one
	// `copy_from_slice` at cluster 3, which for a directory larger than a cluster ran straight over
	// the upcase table beside it and produced an image that would not mount. A fixture that cannot
	// hold a directory bigger than one sector cannot exercise a publish protocol, because the
	// interleaving only exists across two.
	let root_clusters = root.len().div_ceil(bps).max(1);
	let mut root_chain: Vec<usize> = alloc::vec![3];
	for _ in 1..root_clusters {
		root_chain.push(next);
		next += 1;
	}
	for (i, &cluster) in root_chain.iter().enumerate() {
		let link: u32 = if i + 1 < root_chain.len() { root_chain[i + 1] as u32 } else { 0xFFFF_FFFF };
		set_fat(&mut fat, 4, cluster, link);
		mark(&mut bm, cluster);
		let at = i * bps;
		if at >= root.len() {
			break;
		}
		let end = (at + bps).min(root.len());
		let off = (heap + cluster - 2) * bps;
		img[off..off + (end - at)].copy_from_slice(&root[at..end]);
	}
	// THE BITMAP IS WRITTEN LAST, and that is the whole of the three-cluster defect.
	//
	// It used to be copied into the image before the root's chain was built, so every cluster the
	// root needed beyond its first was marked in `bm` after the copy and stayed FREE on the medium.
	// The driver then did exactly what a driver must: it allocated the first cluster the bitmap
	// offered, which was the root's own second one, wrote the new file's data over the entries
	// living there, and the terminator that data left behind scrubbed the third cluster away.
	// A directory of three clusters therefore "lost its tail" on an ordinary write - on an image
	// whose bitmap said two of those clusters belonged to nobody.
	let bm_off = heap * bps;
	img[bm_off..bm_off + bm.len()].copy_from_slice(&bm);
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
	// The fields a real formatter writes and these fixtures did not: the revision (major 1), the
	// volume length, and the boot checksum in sector 11. Without them the images were exFAT only in
	// the sense that they said so - and every validation the driver gained had to be turned off or
	// the crate's own media would fail it.
	img[104] = 0; // VersionMinor
	img[105] = 1; // VersionMajor
	// exFAT's floor is 2^20 bytes, so a 45 KiB image is not an exFAT volume however plausible its
	// other fields are. The heap keeps its size and the volume is padded out to the floor - which
	// is also what a formatter does when asked to make a filesystem smaller than it allows.
	let volume_sectors = total.max((1 << 20) / bps);
	img.resize(volume_sectors * bps, 0);
	img[72..80].copy_from_slice(&(volume_sectors as u64).to_le_bytes());
	img[510] = 0x55;
	img[511] = 0xAA;
	// Last, because it sums every byte before it - including the signature just written.
	stamp_exfat_boot_checksum(&mut img, bps);
	img
}

// Compute the exFAT boot checksum over sectors 0..11 and write it across sector 11, the way a
// formatter finishes a volume. Sectors 1..9 are the extended boot sectors; each ends with the
// 0xAA550000 signature, which is part of what is summed.
fn stamp_exfat_boot_checksum(img: &mut [u8], bps: usize) {
	for sector in 1..9 {
		let end = (sector + 1) * bps;
		img[end - 4..end].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
	}
	let mut sum: u32 = 0;
	for at in 0..11 * bps {
		if at == 106 || at == 107 || at == 112 {
			continue;
		}
		sum = (sum >> 1) | (sum << 31);
		sum = sum.wrapping_add(img[at] as u32);
	}
	for chunk in img[11 * bps..12 * bps].chunks_exact_mut(4) {
		chunk.copy_from_slice(&sum.to_le_bytes());
	}
}

// A 0x81 allocation-bitmap entry: marks the bitmap's first cluster and byte length.
// The Up-case Table a formatter writes, in the compressed form the specification defines: 0xFFFF
// followed by a count means "this many characters map to themselves". This one folds ASCII and
// U+00E0..U+00FE onto their capitals, so a fixture can prove the driver is reading the VOLUME's
// table rather than applying its own ASCII rule.
//
// These images had no 0x82 entry at all, which is not a legal exFAT volume - the table is required
// - and it meant every name decision the driver made was untested against a real one.
// Where the first file entry set begins in an exFAT root, found rather than counted. Eight tests
// wrote `root + 32` ("the set after the bitmap entry") and every one of them moved when the
// fixtures gained the Up-case Table the format requires.
fn exfat_first_set(img: &[u8], root_off: usize) -> usize {
	let mut at = root_off;
	while at + 32 <= img.len() {
		match img[at] {
			0x85 => return at,
			0x00 => break,
			_ => at += 32,
		}
	}
	panic!("no entry set in the root at {root_off:#x}");
}

fn exfat_upcase_table() -> Vec<u8> {
	let mut units: Vec<u16> = Vec::new();
	let identity = |units: &mut Vec<u16>, n: u16| {
		units.push(0xFFFF);
		units.push(n);
	};
	identity(&mut units, 0x61); // 0x0000..0x0061 map to themselves
	units.extend((0x61u16..=0x7A).map(|c| c - 0x20)); // a-z -> A-Z
	identity(&mut units, 0xE0 - 0x7B); // 0x007B..0x00E0
	units.extend((0xE0u16..=0xF6).map(|c| c - 0x20)); // latin-1 lowercase -> capitals
	units.push(0xF7); // division sign, itself
	units.extend((0xF8u16..=0xFE).map(|c| c - 0x20));
	// AND THE REST OF THE PLANE, which this fixture did not describe at all.
	//
	// It stopped at 0x00FF, so it expanded to 255 mappings and left 0x0100-0xFFFF undescribed -
	// which the decoder used to accept, after which `up()` returned every remaining character
	// unchanged because a lookup past the end returns its input. The format requires a custom table
	// to cover 0000h-FFFFh unless the implementation restricts create and rename to the first 128
	// characters, and this one accepts Unicode names.
	//
	// Confirmed against `mkfs.exfat`: a real table is 5836 bytes compressed and expands to exactly
	// 65536 mappings. A fixture that would not survive its own driver's rule was not describing a
	// volume any formatter writes - the same lesson the UDF fixtures learned at the File Set
	// Descriptor.
	// 65536 - 255 already described: the table ends at 0x00FE, so 0xFF01 units map to themselves.
	identity(&mut units, 0xFF01); // 0x00FF..0xFFFF map to themselves
	let mut out: Vec<u8> = Vec::new();
	for u in units {
		out.extend_from_slice(&u.to_le_bytes());
	}
	out
}

fn push_exfat_upcase(dir: &mut Vec<u8>, cluster: u32, table: &[u8]) {
	let mut e = [0u8; 32];
	e[0] = 0x82;
	let mut sum: u32 = 0;
	for &b in table {
		sum = sum.rotate_right(1).wrapping_add(b as u32);
	}
	e[4..8].copy_from_slice(&sum.to_le_bytes());
	e[20..24].copy_from_slice(&cluster.to_le_bytes());
	e[24..32].copy_from_slice(&(table.len() as u64).to_le_bytes());
	dir.extend_from_slice(&e);
}

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

// The hash over the name folded through the table `exfat_upcase_table` writes - ASCII and Latin-1.
// Deliberately a separate implementation from the driver's, so the two can disagree.
fn fixture_name_hash(units: &[u16]) -> u16 {
	let mut hash: u16 = 0;
	for &u in units {
		let up = match u {
			0x61..=0x7A => u - 0x20,
			0xE0..=0xF6 | 0xF8..=0xFE => u - 0x20,
			_ => u,
		};
		for b in up.to_le_bytes() {
			hash = hash.rotate_right(1).wrapping_add(b as u16);
		}
	}
	hash
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
	// The NameHash, which these fixtures left at zero. It is the volume's index of the name: the
	// system that wrote the medium recomputes it on every lookup and skips a set that disagrees, so
	// an image full of zero hashes is one no implementation but this one could open by name.
	stream[4..6].copy_from_slice(&fixture_name_hash(&units).to_le_bytes());
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

// EVERY BUILDER IN THIS FILE, ASKED WHETHER ITS IMAGE HOLDS WHAT IT SAYS IT HOLDS.
//
// This milestone recorded five harnesses that proved nothing, and every one of them for the same
// reason: the fixture did not contain the thing the assertions were about, so they passed without
// reaching the code. The fifth - `an_exfat_directory_spanning_three_clusters_survives_an_ordinary_write`
// - was read as a driver defect for a day, and it was an allocation bitmap copied into the image
// before the root's own clusters were marked in it.
//
// So the check that stops the sixth is a test rather than a habit: mount what each builder
// produces, list it, and read every seed file back. A builder that grows a new limit fails HERE,
// named, instead of making some unrelated assertion vacuous.
#[test]
fn every_fixture_this_suite_builds_holds_what_it_claims() {
	// Deliberately past the shapes the builders' first callers used: names that need long-name
	// entries, a file larger than one cluster, a directory, and enough entries that the root spans
	// more than one sector on every format.
	let seed: &[File] = &[
		File { path: "HELLO.TXT", data: b"Hello, FAT!" },
		File { path: "readme.md", data: b"a long name, so the entry needs LFN fragments" },
		File { path: "DOCS/a.txt", data: b"in a subdir" },
		File { path: "BIG.BIN", data: &[0x5Au8; 3 * 512] },
		File { path: "P0.TXT", data: b"padding, so the root needs a second sector" },
		File { path: "P1.TXT", data: b"padding, so the root needs a second sector" },
		File { path: "P2.TXT", data: b"padding, so the root needs a second sector" },
		File { path: "P3.TXT", data: b"padding, so the root needs a second sector" },
	];
	// exFAT has no subdirectory-with-a-file form in these builders, so its seed is the flat one.
	let flat: Vec<File> = seed.iter().filter(|f| !f.path.contains('/')).cloned().collect();
	let mut cases: Vec<(String, Vec<u8>, Vec<File>)> = Vec::new();
	for (label, kind) in [("fat12", Kind::Fat12), ("fat16", Kind::Fat16), ("fat32", Kind::Fat32)] {
		cases.push((label.into(), build_fat(kind, seed), seed.to_vec()));
	}
	cases.push(("fat12-sized".into(), build_fat12(seed, 1000), seed.to_vec()));
	cases.push(("fat32-small".into(), build_fat_sized(Kind::Fat32, seed, 4000), seed.to_vec()));
	cases.push(("exfat".into(), build_exfat(&flat), flat.clone()));
	// The NoFatChain leg: contiguous clusters and no FAT entries at all, which is how Windows
	// commonly writes a file - a builder limit here made every NFC assertion vacuous.
	let nfc: Vec<File> = alloc::vec![File { path: "NFC.BIN", data: &[0x77u8; 2 * 512] }];
	let mut with_nfc = flat.clone();
	with_nfc.extend(nfc.iter().cloned());
	cases.push(("exfat-nfc".into(), build_exfat_nfc(&flat, &nfc), with_nfc));
	cases.push(("exfat-tree".into(), build_exfat_tree(&flat, &[], &["SUB"]), flat.clone()));

	for (label, image, expect) in cases {
		let mut fs = FatFs::mount(MemDisk { data: image }).unwrap_or_else(|| panic!("{label}: the fixture does not mount"));
		let listed = fs.list().unwrap_or_else(|e| panic!("{label}: the fixture does not list ({e:?})"));
		assert!(!listed.is_empty(), "{label}: the fixture lists nothing");
		for f in &expect {
			let got = fs.read_file(f.path.as_bytes()).unwrap_or_else(|e| panic!("{label}: {} is not in the image this builder produced ({e:?})", f.path));
			assert_eq!(got, f.data, "{label}: {} reads back as different bytes than were put in it", f.path);
		}
		// And the free-space model has to agree the image is a volume.
		let (total, free) = (fs.total_bytes(), fs.free_bytes().unwrap_or_else(|e| panic!("{label}: free space is unreadable ({e:?})")));
		assert!(free <= total, "{label}: {free} bytes free on a {total}-byte volume");
		// THEN ONE ORDINARY WRITE, and everything read again.
		//
		// A fixture whose allocation record disagrees with its own layout reads back perfectly
		// until something allocates: the driver takes the first cluster the record offers, which is
		// a cluster the image is already using, and the damage looks like a driver defect. That is
		// exactly what the three-cluster case cost, so the check is not "does it read" but "does it
		// still read after the volume has been asked for space".
		fs.write_file(b"FIXCHK.BIN", &[0xA3u8; 2 * 512]).unwrap_or_else(|e| panic!("{label}: the fixture will not accept a write ({e:?})"));
		for f in &expect {
			let got = fs.read_file(f.path.as_bytes()).unwrap_or_else(|e| panic!("{label}: {} is gone after one ordinary write - the image's allocation record does not describe the image ({e:?})", f.path));
			assert_eq!(got, f.data, "{label}: {} changed under an unrelated write", f.path);
		}
		assert_eq!(fs.read_file(b"FIXCHK.BIN").unwrap_or_else(|e| panic!("{label}: the file just written is unreadable ({e:?})")), &[0xA3u8; 2 * 512]);
	}
}

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
	for (label, kind) in [("fat12", Kind::Fat12), ("fat16", Kind::Fat16), ("fat32", Kind::Fat32)] {
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
fn reports_total_and_free_bytes_across_writes() {
	// every family reports a plausible pool: free fits inside total, and writing a
	// multi-cluster file shrinks free by at least the file's size while total holds.
	for build in [build_fat(Kind::Fat12, ROOT), build_fat(Kind::Fat16, ROOT), build_fat(Kind::Fat32, ROOT), build_exfat(ROOT)] {
		let mut fs = FatFs::mount(MemDisk { data: build }).unwrap();
		let total = fs.total_bytes();
		let before = fs.free_bytes().unwrap();
		assert!(total > 0 && before > 0 && before <= total);
		let big: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
		fs.write_file(b"POOL.BIN", &big).unwrap();
		let after = fs.free_bytes().unwrap();
		assert!(after < before && before - after >= big.len() as u64);
		assert_eq!(fs.total_bytes(), total);
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
	// the file's run is freed; the three clusters the volume's own structures live in - bitmap,
	// root and Up-case Table - stay allocated.
	assert_eq!(fs.dev.data[heap * 512], 0b111, "the NoFatChain run's bitmap bits must be cleared");
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
	write_bpb(&mut img, bps, 1, 1, 1, fat_size, 512, total, 0);
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
	let set_at = exfat_first_set(&fs.dev.data, (heap + 1) * 512);
	let stream = set_at + 32;
	// UNDER the read ceiling and far past the volume, so the RUN check is what refuses it.
	//
	// This forged `u64::MAX`, which `read_file`'s ceiling now catches first as `TooLarge` - a
	// perfectly good refusal, and one that would have left the run validation below untested. Both
	// answers are asserted: this size exercises the run, and the ceiling gets its own case.
	const FORGED: u64 = 200 * 1024 * 1024;
	fs.dev.data[stream + 24..stream + 32].copy_from_slice(&FORGED.to_le_bytes());
	let count = fs.dev.data[set_at + 1] as usize + 1;
	let sum = exfat_set_checksum(&fs.dev.data[set_at..set_at + count * 32]);
	fs.dev.data[set_at + 2..set_at + 4].copy_from_slice(&sum.to_le_bytes());
	assert_eq!(fs.read_file(b"backup.img"), Err(FsError::Invalid), "a run the heap cannot hold");
	// And a size past what this reader will allocate at all is `TooLarge`, which is the bound the
	// caller can raise or lower with `read_file_bounded` - the point being that a number the medium
	// writes no longer names the size of an allocation.
	fs.dev.data[stream + 24..stream + 32].copy_from_slice(&u64::MAX.to_le_bytes());
	let sum = exfat_set_checksum(&fs.dev.data[set_at..set_at + count * 32]);
	fs.dev.data[set_at + 2..set_at + 4].copy_from_slice(&sum.to_le_bytes());
	assert_eq!(fs.read_file(b"backup.img"), Err(FsError::TooLarge), "a length past the reader's ceiling");
	assert_eq!(fs.read_file_bounded(b"backup.img", 4096), Err(FsError::TooLarge), "and a caller may set its own");
	// Back to the forged-but-plausible size for the removal below, which is about the release path.
	fs.dev.data[stream + 24..stream + 32].copy_from_slice(&FORGED.to_le_bytes());
	let sum = exfat_set_checksum(&fs.dev.data[set_at..set_at + count * 32]);
	fs.dev.data[set_at + 2..set_at + 4].copy_from_slice(&sum.to_le_bytes());
	// the remove is durable (the entry clears) but its release refuses the forged
	// run: the clusters stay marked - a bounded leak, never a foreign free.
	fs.remove(b"backup.img").unwrap();
	assert_eq!(fs.read_file(b"backup.img"), Err(FsError::NotFound));
	assert_eq!(fs.dev.data[heap * 512], 0b1111, "no bitmap bit may change under a refused release");
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
	let root_off = classic_root_off(&fs.dev.data);
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
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		self.inner.read_block(lba, buf)
	}

	fn write_block(&mut self, lba: u64, buf: &[u8]) -> bool {
		if !self.failed {
			if self.until_fail == 0 {
				self.failed = true;
				return false;
			}
			self.until_fail -= 1;
		}
		self.inner.write_block(lba, buf)
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
	let root_off = exfat_first_set(&fs.dev.data, (heap + 1) * 512);
	let stream = root_off + 32;
	let valid = u64::from_le_bytes(fs.dev.data[stream + 8..stream + 16].try_into().unwrap());
	let data = u64::from_le_bytes(fs.dev.data[stream + 24..stream + 32].try_into().unwrap());
	assert_eq!((valid, data), (1024, 1024), "the parent record must grow with the directory");
	let stored = u16::from_le_bytes(fs.dev.data[root_off + 2..root_off + 4].try_into().unwrap());
	assert_eq!(stored, exfat_set_checksum(&fs.dev.data[root_off..root_off + 96]), "the set checksum must be restamped");
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
	// a TexFAT volume whose second FAT is active - we would read the wrong table.
	let mut ex_active = build_exfat(ROOT);
	ex_active[106] |= 0x01;
	assert!(FatFs::mount(MemDisk { data: ex_active }).is_none(), "a TexFAT second-active-FAT volume");
	// a FAT32 whose ExtFlags name an active copy past the copy count. The fixtures carry two
	// copies, so copy 1 is a legal choice and only copy 2 is past the end - the value has to be
	// derived from what the image says, not from what one earlier fixture happened to have.
	let mut bad_active = build_fat(Kind::Fat32, ROOT);
	let past_end = 0x0080u16 | u16::from(bad_active[16]);
	bad_active[40..42].copy_from_slice(&past_end.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: bad_active }).is_none(), "an active FAT past the copy count");
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
	let root_off = exfat_first_set(&ex.dev.data, (heap + 1) * 512);
	let e = &ex.dev.data[root_off..root_off + 32];
	assert_eq!(e[0], 0x85);
	let ts = (date_2000 as u32) << 16;
	assert_eq!(u32::from_le_bytes(e[8..12].try_into().unwrap()), ts, "the exFAT create timestamp");
	assert_eq!(u32::from_le_bytes(e[12..16].try_into().unwrap()), ts, "the exFAT modify timestamp");
	assert_eq!(e[22] & 0x80, 0x80, "the timestamp must be marked UTC");
}

// The (create date, write date) of the fixed-root entry whose 8.3 field is `short`.
fn root_entry_dates(img: &[u8], short: &[u8; 11]) -> (u16, u16) {
	let root_off = classic_root_off(img);
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
	let root_off = classic_root_off(&fs.dev.data);
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
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		self.inner.read_block(lba, buf)
	}

	fn write_block(&mut self, lba: u64, buf: &[u8]) -> bool {
		if self.armed && lba == self.lba {
			if self.skip == 0 {
				self.armed = false;
				return false;
			}
			self.skip -= 1;
		}
		self.inner.write_block(lba, buf)
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
	// `CommitUncertain`, not `Io`. The write failed AFTER the swap's first directory write, so the
	// new entry set may be on the medium - and `Io` reaches a caller as "try again", which is the
	// one thing that must not happen when the first attempt may have landed. This asserted the
	// device's error where the contract's is what the caller acts on.
	assert_eq!(fs.write_file(b"DOCS/F12.TXT", b"y"), Err(FsError::CommitUncertain));
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
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		self.reads += 1;
		self.inner.read_block(lba, buf)
	}

	fn write_block(&mut self, lba: u64, buf: &[u8]) -> bool {
		self.inner.write_block(lba, buf)
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
	let root_off = classic_root_off(&img);
	let slot = root_off + 4 * 32; // the first free slot past ROOT's four records
	img[slot..slot + 11].fill(0x20);
	img[slot + 11] = 0x20; // attributes: an ordinary file
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	assert!(names(&fs.list().unwrap()).iter().all(|n| !n.is_empty()));
	// An empty path is malformed rather than absent - the parser refuses it before any directory is
	// searched, which is the stronger answer: it cannot match an empty name however one got stored.
	assert_eq!(fs.read_file(b""), Err(FsError::BadName));
}

#[test]
fn a_lowercase_exfat_name_carries_the_upcased_hash() {
	// The NameHash is defined over the UP-CASED name: the media's home systems
	// recompute it on lookup and skip a mismatched set, so a hash over the name as
	// written left every lowercase-named file listable but unopenable by name there.
	let mut fs = FatFs::mount(MemDisk { data: build_exfat(&[]) }).unwrap();
	fs.write_file(b"hello.txt", b"cased").unwrap();
	let heap = 25usize; // 24 reserved + 1 FAT sector
	// the stream record follows the 0x85 that starts the set, and the hash sits at its bytes 4..6.
	// Compare against an independent computation over the up-cased UTF-16LE name.
	let stream = exfat_first_set(&fs.dev.data, (heap + 1) * 512) + 32;
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
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		self.inner.read_block(lba, buf)
	}

	fn write_block(&mut self, lba: u64, buf: &[u8]) -> bool {
		self.writes.push(lba);
		self.inner.write_block(lba, buf)
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
	// The fixture is mirrored, so each copy is touched in turn: copy 0 starts at sector 1 and copy
	// 1 at 1 + fat_size. What must not grow is the number of sectors touched WITHIN a copy.
	let copy1 = (fs.geo.reserved_sectors + fs.geo.fat_size) as u64;
	fs.set_fat_entry(2, 0x123).unwrap(); // byte offset 3: wholly inside each copy's first sector
	assert_eq!(fs.dev.writes, [1, copy1], "a non-straddling slot must touch one sector per copy");
	fs.dev.writes.clear();
	fs.set_fat_entry(341, 0x456).unwrap(); // byte offset 511: straddles into the next sector
	assert_eq!(fs.dev.writes, [1, 2, copy1, copy1 + 1], "a straddling slot needs exactly the pair, per copy");
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
	let first = exfat_first_set(&fs.dev.data, (25 + 1) * 512);
	restamp_vdl(&mut fs.dev.data, first, 5);
	restamp_vdl(&mut fs.dev.data, first + 96, 700);
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
	let root_off = classic_root_off(&fs.dev.inner.data);
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
	let root_off = classic_root_off(&img);
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
fn an_overwrite_via_the_short_alias_keeps_the_long_name() {
	// The 8.3 short form names the same file - an overwrite through it must not
	// rename the file to the alias: the long name survives, as the media's home
	// systems keep the directory entry on an in-place overwrite.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, &[]) }).unwrap();
	fs.write_file(b"Alpha file.txt", b"one").unwrap();
	fs.write_file(b"ALPHA_~1.TXT", b"two").unwrap();
	assert_eq!(fs.read_file(b"Alpha file.txt").unwrap(), b"two");
	let listed = names(&fs.list().unwrap());
	assert!(listed.contains(&"Alpha file.txt".to_string()), "the long name must survive: {listed:?}");
	assert_eq!(listed.len(), 1, "{listed:?}");
}

#[test]
fn a_long_non_ascii_name_within_255_units_is_accepted() {
	// The length ceiling is 255 UTF-16 units, not UTF-8 bytes: a 204-unit name of
	// two-byte characters (408 bytes) is legal on the media's home systems and must
	// round-trip on both families; 256 units must refuse.
	let path = ("\u{10D}".repeat(200) + ".txt").into_bytes();
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	fs.write_file(&path, b"diacritics").unwrap();
	assert_eq!(fs.read_file(&path).unwrap(), b"diacritics");
	assert!(fs.list().unwrap().iter().any(|e| e.name.as_bytes() == path.as_slice()));
	let mut ex = FatFs::mount(MemDisk { data: build_exfat(&[]) }).unwrap();
	ex.write_file(&path, b"diacritics").unwrap();
	assert_eq!(ex.read_file(&path).unwrap(), b"diacritics");
	let too_long = ("\u{10D}".repeat(252) + ".txt").into_bytes();
	assert_eq!(fs.write_file(&too_long, b"x"), Err(FsError::TooLong));
	assert_eq!(ex.write_file(&too_long, b"x"), Err(FsError::TooLong));
}

#[test]
fn a_failed_free_of_the_old_chain_does_not_fail_a_durable_write() {
	// Once the new content and its entry are on disk (or the entry is cleared, for a
	// remove), the operation is durable - a device failing during the OLD chain's
	// free must cost at most lost clusters, never a false failure.
	let inner = MemDisk { data: build_fat(Kind::Fat16, ROOT) };
	let mut fs = FatFs::mount(FlakyDisk { inner, until_fail: usize::MAX, failed: true }).unwrap();
	fs.write_file(b"OLD.BIN", &[0x22u8; 3 * 512]).unwrap();
	// the overwrite writes the new FAT link (once per mirrored copy), the data cluster, and the
	// directory sector, then frees the old chain - fail that free's first FAT write.
	fs.dev.failed = false;
	fs.dev.until_fail = 4;
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

#[test]
fn a_non_mirrored_fat32_volume_uses_its_active_copy() {
	// ExtFlags bit 7 disables FAT mirroring and bits 0-3 name the only current copy -
	// the others are stale by specification. Reads must follow the active copy (the
	// stale one truncates chains and calls allocated clusters free, cross-linking real
	// data), and writes must leave the stale copy alone.
	let bps = 512usize;
	let (reserved, clusters) = (3usize, 100usize);
	let heap = reserved + 2; // two one-sector FAT copies
	let total = heap + clusters;
	let mut img = vec![0u8; total * bps];
	img[11..13].copy_from_slice(&512u16.to_le_bytes());
	img[13] = 1;
	img[14..16].copy_from_slice(&(reserved as u16).to_le_bytes());
	img[16] = 2;
	img[32..36].copy_from_slice(&(total as u32).to_le_bytes());
	img[36..40].copy_from_slice(&1u32.to_le_bytes());
	img[40..42].copy_from_slice(&0x0081u16.to_le_bytes()); // mirroring off, copy 1 active
	img[44..48].copy_from_slice(&2u32.to_le_bytes());
	img[510] = 0x55;
	img[511] = 0xAA;
	// the ACTIVE copy 1: root = cluster 2, HELLO.TXT = clusters 3 -> 4.
	let f1 = 4 * bps;
	for (c, v) in [(2usize, 0xFFFF_FFFFu32), (3, 4), (4, 0xFFFF_FFFF)] {
		img[f1 + c * 4..f1 + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
	}
	// the STALE copy 0 calls clusters 3 and 4 free - reading it would truncate the
	// file and hand its clusters out again.
	let f0 = 3 * bps;
	img[f0 + 2 * 4..f0 + 2 * 4 + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
	let mut root: Vec<u8> = Vec::new();
	push_entry(&mut root, "HELLO.TXT", false, 700, 3);
	img[heap * bps..heap * bps + root.len()].copy_from_slice(&root);
	let data: Vec<u8> = (0..700u32).map(|i| (i * 7) as u8).collect();
	let d = (heap + 1) * bps; // cluster 3, contiguously into cluster 4
	img[d..d + 700].copy_from_slice(&data);
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	assert_eq!(fs.kind_name(), "fat32");
	assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), data, "the chain must follow the ACTIVE copy");
	let stale_before = fs.dev.data[f0..f0 + bps].to_vec();
	fs.write_file(b"NEW.TXT", b"fresh").unwrap();
	assert_eq!(fs.read_file(b"NEW.TXT").unwrap(), b"fresh");
	assert_eq!(fs.read_file(b"HELLO.TXT").unwrap(), data, "the allocation must not cross-link the file");
	assert_eq!(fs.dev.data[f0..f0 + bps], stale_before[..], "the stale copy must stay untouched");
}

#[test]
fn an_overwrite_preserves_the_creation_stamp_and_name_case() {
	// The media's home systems preserve the original name case and the creation time
	// on an in-place overwrite - a rewritten file must not "get younger" or change
	// its displayed case.
	let (d1, _) = dos_datetime(946_684_800); // 2000-01-01
	let (d2, _) = dos_datetime(1_075_680_000);
	assert_ne!(d1, d2);
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	fs.set_clock(946_684_800);
	fs.write_file(b"Mixed.txt", b"one").unwrap();
	fs.set_clock(1_075_680_000);
	fs.write_file(b"MIXED.TXT", b"two").unwrap();
	assert_eq!(fs.read_file(b"Mixed.txt").unwrap(), b"two");
	let listed = names(&fs.list().unwrap());
	assert!(listed.contains(&"Mixed.txt".to_string()), "the original case must survive: {listed:?}");
	assert_eq!(root_entry_dates(&fs.dev.data, b"MIXED   TXT"), (d1, d2), "created then, written now");
	// the exFAT form: the create timestamp carries over, the modify stamp is fresh.
	let mut ex = FatFs::mount(MemDisk { data: build_exfat(&[]) }).unwrap();
	ex.set_clock(946_684_800);
	ex.write_file(b"Case.txt", b"one").unwrap();
	ex.set_clock(1_075_680_000);
	ex.write_file(b"CASE.TXT", b"two").unwrap();
	assert!(names(&ex.list().unwrap()).contains(&"Case.txt".to_string()));
	let set_at = exfat_first_set(&ex.dev.data, (25 + 1) * 512);
	let e = &ex.dev.data[set_at..set_at + 96];
	assert_eq!(e[0], 0x85);
	assert_eq!(u32::from_le_bytes(e[8..12].try_into().unwrap()), (d1 as u32) << 16, "the exFAT create timestamp must carry over");
	assert_eq!(u32::from_le_bytes(e[12..16].try_into().unwrap()), (d2 as u32) << 16, "the exFAT modify timestamp must be fresh");
	assert_eq!(u16::from_le_bytes(e[2..4].try_into().unwrap()), exfat_set_checksum(&e[..96]), "the set checksum must cover the carried stamp");
}

#[test]
fn growing_a_conforming_exfat_chain_writes_a_cluster_pointer() {
	// The bad one. exFAT's FAT shared FAT32's branch everywhere, so a real 0xFFFFFFFF terminator was
	// read as a 28-bit value and written back as `(cur & 0xF000_0000) | val` - which, extending a
	// chain to cluster 5, is 0xF0000005. That is not a cluster pointer at all, and it is what this
	// driver did to a volume some other implementation had formatted correctly.
	let mut img = build_exfat(&[File { path: "A.TXT", data: b"a" }]);
	let bps = 512usize;
	// Where build_exfat puts the FAT, read off FatOffset rather than guessed. This said sector 3,
	// which is inside the boot region and not the FAT at all - the edit did nothing and the test
	// passed on the driver's own behaviour instead. Adding the boot checksum is what surfaced it.
	let fat0 = u32::from_le_bytes(img[80..84].try_into().unwrap()) as usize * bps;
	// A conforming terminator on cluster 2, as a real formatter writes it.
	img[fat0 + 2 * 4..fat0 + 2 * 4 + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
	let mut fs = FatFs::mount(MemDisk { data: img }).expect("mount");
	// Extend the chain: cluster 2 now points at cluster 5.
	fs.set_fat_entry_for_test(2, 5).expect("the entry is writable");
	let entry = fs.fat_entry_for_test(2);
	assert_eq!(entry, 5, "an exFAT FAT entry is a full 32-bit cluster number, got {entry:#x}");
	assert_ne!(entry & 0xF000_0000, 0xF000_0000, "no top nibble is preserved on exFAT");
}

// A device that counts barriers and records the order writes reached it, so a commit protocol can
// be checked rather than described.
struct OrderedDisk {
	data: Vec<u8>,
	flushes: usize,
	// The LBA of every write, in issue order, with a marker for each barrier.
	log: Vec<(u64, bool)>,
}

impl BlockDevice for OrderedDisk {
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		let start = lba as usize * SECTOR_SIZE;
		let Some(src) = self.data.get(start..start + SECTOR_SIZE) else { return false };
		buf.copy_from_slice(src);
		true
	}
	fn write_block(&mut self, lba: u64, buf: &[u8]) -> bool {
		let start = lba as usize * SECTOR_SIZE;
		let Some(dst) = self.data.get_mut(start..start + SECTOR_SIZE) else { return false };
		dst.copy_from_slice(buf);
		self.log.push((lba, false));
		true
	}
	fn flush(&mut self) -> bool {
		self.flushes += 1;
		self.log.push((0, true));
		true
	}
}

#[test]
fn a_write_asks_the_device_for_its_barriers() {
	// `flush()` was never called in this crate - not once - so "the data was written before the
	// directory entry" described the order the CPU issued the writes and not the order they become
	// durable. A device may write back in either order, and the entry landing first is a directory
	// pointing at clusters whose contents never arrived.
	let mut fs = FatFs::mount(OrderedDisk { data: build_fat(Kind::Fat32, &[File { path: "A.TXT", data: b"a" }]), flushes: 0, log: Vec::new() }).expect("mount");
	fs.write_file(b"B.TXT", b"bb").expect("the write succeeds");
	let disk = fs.device_for_test();
	assert!(disk.flushes >= 2, "a publish needs a barrier before the entry and one after it, got {}", disk.flushes);
	// And the ORDER: at least one write, then a barrier, then at least one more write.
	let first_flush = disk.log.iter().position(|(_, barrier)| *barrier).expect("a barrier was requested");
	assert!(first_flush > 0, "the data is written before the first barrier");
	assert!(disk.log[first_flush + 1..].iter().any(|(_, barrier)| !*barrier), "the directory entry is written after the barrier");
}

// A device whose Nth write fails and whose later writes succeed - one bad sector, not a dead disk.
//
// The distinction decides whether a rollback bug is observable at all. A device that fails
// EVERYTHING after the Nth write also fails the rollback, so an erroneous free is refused by the
// medium and the volume comes out looking correct; the harness then proves only that a dead disk
// cannot be corrupted further. A transient error is both the more common failure and the one that
// lets the recovery path run and do its damage.
struct FailNthWrite {
	data: Vec<u8>,
	seen: usize,
	fail_at: usize,
	// A FAILING FLUSH is the other half, and there was no way to ask for one.
	//
	// `flush` defaulted to `true` here and in `OrderedDisk`, so every barrier in this driver
	// succeeded in every test - and the branch a barrier failure takes was unreachable. That is
	// where the publish protocol decides what the caller may free, so it is exactly the branch the
	// crash work is about. 0 = never fail one.
	flushes: usize,
	fail_flush_at: usize,
}

impl FailNthWrite {
	fn cut_write(data: Vec<u8>, at: usize) -> Self {
		FailNthWrite { data, seen: 0, fail_at: at, flushes: 0, fail_flush_at: 0 }
	}

	fn cut_flush(data: Vec<u8>, at: usize) -> Self {
		FailNthWrite { data, seen: 0, fail_at: 0, flushes: 0, fail_flush_at: at }
	}
}

impl BlockDevice for FailNthWrite {
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		let start = lba as usize * SECTOR_SIZE;
		let Some(src) = self.data.get(start..start + SECTOR_SIZE) else { return false };
		buf.copy_from_slice(src);
		true
	}
	fn write_block(&mut self, lba: u64, buf: &[u8]) -> bool {
		self.seen += 1;
		if self.seen == self.fail_at {
			return false;
		}
		let start = lba as usize * SECTOR_SIZE;
		let Some(dst) = self.data.get_mut(start..start + SECTOR_SIZE) else { return false };
		dst.copy_from_slice(buf);
		true
	}
	// A flush that fails does NOT undo the writes before it. The bytes are in the image either way,
	// which is the whole reason a failed barrier leaves the commit ambiguous rather than absent.
	fn flush(&mut self) -> bool {
		self.flushes += 1;
		self.flushes != self.fail_flush_at
	}
}

#[test]
fn an_interrupted_swap_never_frees_clusters_a_live_entry_names() {
	// The defect: the sector holding the NEW directory entry lands, the later sector deleting the
	// OLD entry fails, `swap_entry` returns `Err`, and the caller frees the new data chain - leaving
	// a live entry pointing at clusters that are back in the free pool for the next allocation.
	//
	// Two conditions have to hold at once for the interleaving to exist at all, and finding them
	// took three harnesses that proved nothing:
	//
	//   * the replacement must land in a DIFFERENT byte range from the entry it replaces. Left to
	//     itself `free_run` reuses the slots the old entry just vacated, both images come out
	//     identical, and the second write has nothing left to do. An earlier hole - here from a
	//     removed file - is what sends the new entry somewhere else.
	//   * every file must actually be ON the medium. `build_fat` fills one root sector and drops the
	//     rest, so a fixture with fifty fillers has no A.TXT to replace and `write_file` quietly
	//     becomes a create, which has no second phase and cannot be interrupted between two.
	//
	// And the failure has to be TRANSIENT. Against a device that refuses everything after the Nth
	// write, the erroneous free is refused too, the volume comes out consistent, and the harness
	// proves only that a dead disk cannot be corrupted further.
	//
	// Interrupting at every write count in turn is what finds the dangerous one; the property
	// asserted after each is the same, and two live entries is NOT a violation of it - an
	// interrupted swap may legitimately leave both, each naming its own intact clusters. What may
	// never happen is a live entry reading back as somebody else's data.
	const FILLERS: [&str; 8] = ["F0.TXT", "F1.TXT", "F2.TXT", "F3.TXT", "F4.TXT", "F5.TXT", "F6.TXT", "F7.TXT"];
	for budget in 1..30 {
		let mut files: Vec<File> = FILLERS.iter().map(|name| File { path: name, data: b"x" }).collect();
		files.push(File { path: "A.TXT", data: b"aaaa" });
		let base = build_fat(Kind::Fat32, &files);
		let mut prep = FatFs::mount(MemDisk { data: base }).expect("mount");
		prep.remove(b"F0.TXT").expect("an early hole for the replacement to land in");
		let holed = core::mem::take(&mut prep.device_for_test().data);
		let mut fs = FatFs::mount(FailNthWrite::cut_write(holed, budget)).expect("mount");
		let _ = fs.write_file(b"A.TXT", b"bbbbbbbb");

		// Whatever happened, re-read the volume - and then ALLOCATE, because that is what turns a
		// freed-but-still-named cluster into a cross-link. Without this step the damage is latent:
		// the entry names free clusters whose contents nobody has overwritten yet.
		let image = core::mem::take(&mut fs.device_for_test().data);
		let mut check = FatFs::mount(MemDisk { data: image }).expect("the volume still mounts");
		let _ = check.write_file(b"C.TXT", &[0xCCu8; 4096]);
		let Ok(bytes) = check.read_file(b"A.TXT") else { continue };
		assert!(bytes == b"aaaa" || bytes == b"bbbbbbbb", "after failing at write {budget}, A.TXT reads as {:?} - a live entry naming clusters that were freed and handed out again", bytes);
	}
}

#[test]
fn a_refused_write_to_the_second_fat_copy_leaves_the_copies_identical() {
	// A mirrored volume has more than one current FAT, and the specification lets a foreign driver
	// believe any of them. Writing an entry copy by copy is therefore not one operation: land copy
	// 0, fail copy 1, and the two tables disagree about whether a cluster is free - so whether the
	// next allocation hands it out depends on which driver mounts the volume next.
	//
	// The write must be all-or-nothing across copies. Fail the second copy's sector and the first
	// copy has to come back to what it was.
	let inner = MemDisk { data: build_fat(Kind::Fat16, ROOT) };
	let mut fs = FatFs::mount(FailAt { inner, lba: 0, armed: false, skip: 0 }).unwrap();
	assert!(fs.geo.mirror && fs.geo.num_fats >= 2, "the fixture must have mirrored copies for this to mean anything");
	let copy_bytes = fs.geo.fat_size as usize * fs.geo.bytes_per_sector as usize;
	let first_at = fs.geo.reserved_sectors as usize * fs.geo.bytes_per_sector as usize;
	let second_at = first_at + copy_bytes;
	let before = fs.dev.inner.data[first_at..first_at + copy_bytes].to_vec();

	// The second copy's first sector, which is where cluster 2's entry lives.
	fs.dev.lba = (fs.geo.reserved_sectors + fs.geo.fat_size) as u64;
	fs.dev.armed = true;
	assert_eq!(fs.write_file(b"NEW.TXT", b"z"), Err(FsError::Io), "the failing copy must fail the write");
	fs.dev.armed = false;

	let first = &fs.dev.inner.data[first_at..first_at + copy_bytes];
	let second = &fs.dev.inner.data[second_at..second_at + copy_bytes];
	assert_eq!(first, second, "the FAT copies disagree after a refused write - which one is believed decides whether a cluster is free");
	assert_eq!(first, &before[..], "the surviving copy kept an allocation the volume never completed");
}

#[test]
fn a_fat12_entry_across_a_sector_boundary_is_not_left_torn() {
	// A FAT12 entry is twelve bits, so one entry can straddle two sectors - cluster 341 on a
	// 512-byte sector ends at byte 511 and continues at byte 512. `write_fs_sectors` writes a
	// sector at a time, so a failure between them leaves half the entry updated: four bits of the
	// new value and eight of the old, which is a cluster number nobody wrote and the chain walks
	// into whatever it names.
	let inner = MemDisk { data: build_fat(Kind::Fat12, ROOT) };
	let mut fs = FatFs::mount(FailAt { inner, lba: 0, armed: false, skip: 0 }).unwrap();
	// The straddling cluster, derived rather than assumed: 12 bits means byte offset cluster*3/2.
	let bps = fs.geo.bytes_per_sector as u64;
	let straddler = (2..=fs.max_cluster()).find(|&c| {
		let off = c as u64 + c as u64 / 2;
		off % bps == bps - 1
	});
	let Some(cluster) = straddler else { return }; // a volume too small to have one proves nothing
	let base = fs.geo.reserved_sectors as u64;
	let off = cluster as u64 + cluster as u64 / 2;
	let before = fs.dev.inner.data.clone();

	// Fail the SECOND of the two sectors the entry spans: the first lands, the second does not.
	fs.dev.lba = base + off / bps + 1;
	fs.dev.armed = true;
	assert_eq!(fs.set_fat_entry_for_test(cluster, 0x123), Err(FsError::Io));
	fs.dev.armed = false;

	assert_eq!(fs.fat_entry_for_test(cluster), 0, "the entry is torn: half the new value, half the old");
	let start = (base * bps) as usize + off as usize;
	assert_eq!(&fs.dev.inner.data[start..start + 2], &before[start..start + 2], "the landed half of a straddling entry was not put back");
}

#[test]
fn a_grow_whose_parent_record_write_fails_gives_the_cluster_back() {
	// Growing an exFAT subdirectory is two changes to the medium: the cluster is linked into the
	// directory's chain, and the DataLength recorded in the parent's entry set is raised to match.
	// If the second fails, the first is not just useless - the chain is now longer than its own
	// recorded length, so the extra cluster is marked in use and no traversal from the parent ever
	// reaches it. It leaks, permanently, and every retry leaks another.
	//
	// The grow must come back out: the terminator returns to the old tail and the cluster is freed.
	let heap = 25u64; // 24 reserved + 1 FAT sector
	let inner = MemDisk { data: build_exfat_tree(&[], &[], &["SUB"]) };
	let mut fs = FatFs::mount(FailAt { inner, lba: 0, armed: false, skip: 0 }).unwrap();
	// The parent of SUB is the root, the first heap cluster past the bitmap. Nothing else writes
	// there while files are created inside SUB, so arming it now traps exactly the parent-record
	// update - and which file triggers the grow is the fixture's business, not the test's.
	fs.dev.lba = heap + 1;
	fs.dev.armed = true;
	let mut free_before = fs.free_bytes().unwrap();
	let mut refused = None;
	for i in 0..8u32 {
		let name = alloc::format!("SUB/F{i}.TXT");
		match fs.write_file(name.as_bytes(), b"in the subdir") {
			Ok(()) => free_before = fs.free_bytes().unwrap(),
			Err(error) => {
				refused = Some((i, error));
				break;
			}
		}
	}
	let Some((i, error)) = refused else { panic!("the injected failure never fired - the fixture no longer grows here") };
	assert_eq!(error, FsError::Io);
	assert!(!fs.dev.armed);

	assert_eq!(fs.free_bytes().unwrap(), free_before, "the grow kept its cluster after the parent record refused to move");
	// And the volume is still usable: the retry succeeds and finds the directory as it was.
	let name = alloc::format!("SUB/F{i}.TXT");
	fs.write_file(name.as_bytes(), b"second attempt").unwrap();
	assert_eq!(fs.read_file(name.as_bytes()).unwrap(), b"second attempt");
	assert_eq!(fs.list_dir(b"SUB").unwrap().len(), i as usize + 1);
}

#[test]
fn a_directory_grown_by_a_failed_write_is_grown_at_most_once() {
	// A classic directory is grown before the entry is written, so a failing entry write leaves the
	// directory a cluster longer than it was. That is not corruption - the tail was zeroed before
	// it was linked, so it parses as free slots - and the question worth answering is whether it
	// COMPOUNDS: an operation that grows the directory on every failed retry turns a flaky device
	// into a directory that eats the volume.
	//
	// It does not, and the reason is structural rather than lucky: a grow only happens when no free
	// run exists, and the tail it leaves behind IS a free run. The next attempt finds it and
	// writes into it. This pins that to one cluster, so a later change that grows unconditionally
	// (or frees the tail on failure and re-grows on the retry) has to answer for it.
	let inner = MemDisk { data: build_fat(Kind::Fat16, ROOT) };
	let mut fs = FatFs::mount(FailAt { inner, lba: 0, armed: false, skip: 0 }).unwrap();
	for i in 0..12u32 {
		let name = alloc::format!("DOCS/F{i}.TXT");
		fs.write_file(name.as_bytes(), b"x").unwrap();
	}
	let full = fs.free_bytes().unwrap();

	// Fail the directory content write that follows the grow.
	let max = fs.max_cluster();
	let free: Vec<u32> = (2..=max).filter(|&c| fs.next_cluster(c).unwrap() == 0).take(2).collect();
	fs.dev.lba = fs.cluster_fs_sector(free[1]);
	fs.dev.skip = 1; // let the grow's zeroing pass, fail the directory write after it
	fs.dev.armed = true;
	// `CommitUncertain`, and the mount stops accepting mutations with it.
	//
	// The failure is the FIRST of the swap's two writes, so the new entry set may be on the medium.
	// `Io` told the caller to retry, and a retry may allocate over clusters a live entry already
	// names - so the answer is the one `fs-core` defines for a commit that may have landed, and the
	// volume goes read-only, which is what makes the refusal stick past this call.
	assert_eq!(fs.write_file(b"DOCS/G0.TXT", b"y"), Err(FsError::CommitUncertain));
	assert!(!fs.dev.armed, "the injected failure never fired");

	// TWO clusters, not one, since 2026-08-12, and the extra one is the publish protocol working.
	//
	// The failing write is the FIRST of the swap's two, and its failure now carries `placed: true`:
	// an entry set can straddle a sector boundary, so a write that fails part-way may already have
	// put the live half on the medium. The caller therefore does not free the new data chain, and
	// the cost of the failure is the directory's grown cluster PLUS that chain.
	//
	// A leaked cluster against a cross-linked one is not a close comparison, and the bound this test
	// exists for still holds: it is a fixed cost per failure, and the retry below pays for nothing
	// further.
	let cluster_bytes = (fs.geo.sectors_per_cluster * fs.geo.bytes_per_sector) as u64;
	assert_eq!(full - fs.free_bytes().unwrap(), cluster_bytes * 2, "a failed publish costs the grown cluster and the chain it may already have named");

	// THERE IS NO RETRY, and that is the change. This test used to arm the trap again and assert
	// that a second attempt grew the directory no further - a bound worth having while a retry was
	// the expected response. It is not the expected response any more: the first attempt may have
	// published, so a second one is exactly what `CommitUncertain` exists to prevent, and the
	// degraded mount refuses it.
	//
	// The bound the test was written for survives in a stronger form: the cost of an uncertain
	// commit is fixed at what the failure already spent, because nothing further is permitted.
	assert_eq!(fs.write_file(b"DOCS/G0.TXT", b"y"), Err(FsError::ReadOnly), "an uncertain commit refuses every later mutation");
	assert_eq!(fs.remove(b"DOCS/A.TXT"), Err(FsError::ReadOnly), "and not only a retry of the same write");
	assert_eq!(fs.free_bytes().unwrap(), full - cluster_bytes * 2, "and it costs nothing further");

	// Reads still answer: the volume's data is intact, and refusing them would lose the operator
	// the thing they most need after an uncertain commit - a look at what is actually there.
	assert!(fs.list_dir(b"DOCS").is_ok(), "a degraded mount still reads");
}

#[test]
fn a_chain_shorter_than_its_entry_is_corruption_not_a_short_file() {
	// The directory entry states the length; the FAT states where the bytes are. When they
	// disagree the file is damaged, and the only wrong answer is to serve the prefix as though it
	// were the whole file - a backup tool copying it writes a truncated file it believes is intact.
	//
	// Two shapes of the same lie: a chain that terminates early, and a first cluster of zero on an
	// entry with a nonzero size.
	let mut img = build_fat(Kind::Fat16, &[File { path: "BIG.BIN", data: &[0xABu8; 4 * 512] }]);
	let fat_off = 512; // one reserved sector
	let root_off = classic_root_off(&img);
	let first = u16::from_le_bytes([img[root_off + 26], img[root_off + 27]]) as usize;
	assert_eq!(&img[root_off..root_off + 3], b"BIG", "the fixture's first root entry must be the file under test");

	// Terminate the chain after its first cluster while the entry still claims four.
	let mut cut = img.clone();
	img_set_fat16(&mut cut, fat_off, first, 0xFFFF);
	let mut fs = FatFs::mount(MemDisk { data: cut }).unwrap();
	assert_eq!(fs.read_file(b"BIG.BIN"), Err(FsError::Invalid), "a chain that ends early was served as a short file");

	// And a chain that runs into a free entry, which is the same claim by another route.
	let mut freed = img.clone();
	img_set_fat16(&mut freed, fat_off, first, 0);
	let mut fs = FatFs::mount(MemDisk { data: freed }).unwrap();
	assert_eq!(fs.read_file(b"BIG.BIN"), Err(FsError::Invalid));

	// A first cluster of zero with a size that says otherwise.
	img[root_off + 26..root_off + 28].copy_from_slice(&0u16.to_le_bytes());
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	assert_eq!(fs.read_file(b"BIG.BIN"), Err(FsError::Invalid), "an entry with no clusters and a nonzero size read as an empty file");
}

#[test]
fn a_cyclic_chain_is_refused_rather_than_repeated() {
	// A cycle in the FAT does not look like corruption to a walk that counts steps: a cluster
	// pointing at itself, read for a file twice its size, is two steps and returns the same 512
	// bytes twice as `Ok`. Nothing about the result says it is not the file.
	fn build(data: &'static [u8]) -> Vec<u8> {
		build_fat(Kind::Fat16, &[File { path: "LOOP.BIN", data }])
	}
	let fat_off = 512;

	// The self-loop, read for two clusters.
	let mut img = build(&[0xABu8; 2 * 512]);
	let root_off = classic_root_off(&img);
	let first = u16::from_le_bytes([img[root_off + 26], img[root_off + 27]]) as usize;
	img_set_fat16(&mut img, fat_off, first, first as u16);
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	assert_eq!(fs.read_file(b"LOOP.BIN"), Err(FsError::Invalid), "a self-pointing cluster was read twice and returned as the file");

	// And a two-cluster cycle, read for four - the shape a step budget is least able to see.
	let mut img = build(&[0xABu8; 4 * 512]);
	let first = u16::from_le_bytes([img[root_off + 26], img[root_off + 27]]) as usize;
	img_set_fat16(&mut img, fat_off, first, first as u16 + 1);
	img_set_fat16(&mut img, fat_off, first + 1, first as u16);
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	assert_eq!(fs.read_file(b"LOOP.BIN"), Err(FsError::Invalid), "a two-cluster cycle was returned as A,B,A,B");
}

#[test]
fn an_exfat_boot_region_that_fails_its_own_checks_does_not_mount() {
	// The specification requires the boot checksum to be confirmed before any boot field is used,
	// and this driver read all of them without ever computing it. Each of these is a single field
	// changed on an otherwise plausible volume - the shape a forged or bit-rotted boot region takes.
	let good = build_exfat(ROOT);
	assert!(FatFs::mount(MemDisk { data: good.clone() }).is_some(), "the fixture itself must be a conforming volume");

	let refused = |what: &str, edit: &dyn Fn(&mut Vec<u8>)| {
		let mut img = good.clone();
		edit(&mut img);
		assert!(FatFs::mount(MemDisk { data: img }).is_none(), "{what}");
	};

	// The checksum itself: one byte of the boot region moved, nothing else.
	refused("a boot region whose checksum no longer matches", &|img| img[100] ^= 0xFF);
	// And a volume whose checksum sector does not even agree with itself.
	refused("a checksum sector with mismatched copies", &|img| img[11 * 512 + 4] ^= 0x01);
	// The boot signature, which was checked on the classic path only.
	refused("an exFAT volume with no 0xAA55 signature", &|img| {
		img[511] = 0;
		stamp_exfat_boot_checksum(img, 512);
	});
	// MustBeZero: a region a formatter that thought it was writing a BPB would have filled.
	refused("a volume with a classic BPB in its MustBeZero region", &|img| {
		img[13] = 8;
		stamp_exfat_boot_checksum(img, 512);
	});
	// A revision this driver has never seen: its fields may not mean what they mean here.
	refused("a filesystem revision from the future", &|img| {
		img[105] = 2;
		stamp_exfat_boot_checksum(img, 512);
	});
	// A FAT starting inside the boot region - the concrete case the finding names, where the first
	// FAT write would go through the backup boot sector.
	refused("a FAT that starts inside the boot region", &|img| {
		img[80..84].copy_from_slice(&1u32.to_le_bytes());
		stamp_exfat_boot_checksum(img, 512);
	});
	// A FAT too short to address the heap the volume claims. It used to mount as a quietly smaller
	// filesystem, because max_cluster takes the minimum of the two.
	//
	// The cluster count has to stay inside the image: raise it past the volume and the mount is
	// refused because the device is too small, which proves the device check and nothing else. 2023
	// clusters from sector 25 end exactly at the volume's last sector, and want a 16-sector FAT.
	refused("a FAT too short for the declared cluster count", &|img| {
		img[92..96].copy_from_slice(&2023u32.to_le_bytes());
		stamp_exfat_boot_checksum(img, 512);
	});
	// A VolumeLength that does not contain the heap it describes.
	refused("a volume shorter than its own cluster heap", &|img| {
		img[72..80].copy_from_slice(&64u64.to_le_bytes());
		stamp_exfat_boot_checksum(img, 512);
	});
	// TexFAT: two FATs with the first active mounted, and the geometry then recorded one - so the
	// second was never maintained and the system that understands TexFAT would read a stale copy.
	//
	// The fixture's heap begins immediately after its single FAT, so a second one would not fit and
	// the overlap rule refuses the volume before the FAT count is ever considered. Moving the heap
	// one sector out makes room, and the same layout with one FAT must still mount - otherwise this
	// proves nothing about the count.
	// Making room means moving the heap, and the heap has contents - the bitmap, the root and the
	// Up-case Table, all of which mount now reads. Shifting the declaration alone leaves a volume
	// that fails for the wrong reason, so the clusters move with it.
	let with_room = |img: &mut Vec<u8>, fats: u8| {
		img.copy_within(25 * 512..(25 + 64) * 512, 26 * 512);
		img[88..92].copy_from_slice(&26u32.to_le_bytes());
		img[110] = fats;
		stamp_exfat_boot_checksum(img, 512);
	};
	let mut one = good.clone();
	with_room(&mut one, 1);
	assert!(FatFs::mount(MemDisk { data: one }).is_some(), "the layout with room for a second FAT must mount when it declares one");
	refused("a TexFAT volume declaring two FATs", &|img| with_room(img, 2));
}

#[test]
fn an_exfat_name_folds_through_the_volumes_own_upcase_table() {
	// exFAT does not define case-insensitivity - the VOLUME does, in the Up-case Table its
	// formatter wrote. A driver that folds `a-z` and passes everything else through computes a
	// different NameHash and a different collision set than the system the medium came from: a
	// non-ASCII name is written, listed, and then not found there by the name it was given.
	//
	// The fixture's table folds Latin-1 as well as ASCII, so a name outside ASCII proves the table
	// is being read rather than a rule being applied.
	let mut fs = FatFs::mount(MemDisk { data: build_exfat(&[]) }).unwrap();
	fs.write_file("café.txt".as_bytes(), b"one").unwrap();

	// The hash the driver stored must be the one computed over the name folded through the table:
	// é (U+00E9) upcases to É (U+00C9), which ASCII-only folding leaves alone.
	let stream = exfat_first_set(&fs.dev.data, 26 * 512) + 32;
	let stored = u16::from_le_bytes(fs.dev.data[stream + 4..stream + 6].try_into().unwrap());
	let hash = |name: &str| {
		let mut h: u16 = 0;
		for u in name.encode_utf16() {
			for b in u.to_le_bytes() {
				h = h.rotate_right(1).wrapping_add(b as u16);
			}
		}
		h
	};
	assert_eq!(stored, hash("CAFÉ.TXT"), "the NameHash must be over the name folded through the volume's table");
	assert_ne!(hash("CAFÉ.TXT"), hash("CAFé.TXT"), "the two foldings must differ, or this test proves nothing");

	// And the fold governs lookup the same way, in both directions.
	assert_eq!(fs.read_file("CAFÉ.TXT".as_bytes()).unwrap(), b"one");
	assert_eq!(fs.read_file("café.txt".as_bytes()).unwrap(), b"one");
	// Which makes it a collision: the same name by the volume's rule is the same file.
	fs.write_file("CAFÉ.TXT".as_bytes(), b"two").unwrap();
	assert_eq!(fs.list().unwrap().len(), 1, "two spellings the volume's table folds together must be one file");
	assert_eq!(fs.read_file("café.txt".as_bytes()).unwrap(), b"two");
}

#[test]
fn an_exfat_volume_without_a_usable_upcase_table_does_not_mount() {
	// The table is required, and a driver that shrugs and uses its own rule makes every name
	// decision on the volume - lookup, collision, the hash it writes - its own opinion.
	let good = build_exfat(ROOT);
	let refused = |what: &str, edit: &dyn Fn(&mut Vec<u8>)| {
		let mut img = good.clone();
		edit(&mut img);
		assert!(FatFs::mount(MemDisk { data: img }).is_none(), "{what}");
	};
	// No 0x82 entry at all: the root's second required entry, removed.
	refused("a root with no Up-case Table entry", &|img| img[26 * 512 + 32] = 0x00);
	// A table whose bytes no longer match the checksum recorded beside them.
	refused("a table that fails its own checksum", &|img| img[27 * 512] ^= 0xFF);
	// A DataLength describing more characters than exist.
	refused("a table longer than the character set it maps", &|img| {
		let at = 26 * 512 + 32;
		img[at + 24..at + 32].copy_from_slice(&(4 * 0x1_0000u64).to_le_bytes());
	});
}

#[test]
fn a_checksum_valid_but_ungrammatical_entry_set_is_not_a_file() {
	// The set checksum says the bytes are the ones that were written. It says nothing about whether
	// they describe a file, and the parser used to take any 0xC0 anywhere among the secondaries and
	// any 0xC1 anywhere. Each of these is checksum-valid and structurally impossible, and each one
	// used to surface in a listing as a file whose clusters something could later free.
	let restamp = |img: &mut Vec<u8>, at: usize| {
		let count = img[at + 1] as usize + 1;
		let sum = exfat_set_checksum(&img[at..at + count * 32]);
		img[at + 2..at + 4].copy_from_slice(&sum.to_le_bytes());
	};
	let listed_after = |edit: &dyn Fn(&mut Vec<u8>, usize)| {
		let mut img = build_exfat(&[File { path: "GOOD.TXT", data: b"g" }, File { path: "NEXT.TXT", data: b"n" }]);
		let at = exfat_first_set(&img, 26 * 512);
		edit(&mut img, at);
		restamp(&mut img, at);
		let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
		names(&fs.list().unwrap())
	};

	// A name fragment where the Stream Extension must be: the length that governs the name is not
	// there, and the fragments are being read before anything declared how many there are.
	let no_stream = listed_after(&|img, at| img[at + 32] = 0xC1);
	assert_eq!(no_stream, ["NEXT.TXT"], "a set with no Stream Extension immediately after the File entry");

	// A NameLength that does not match the fragments present. Truncating it silently renamed the
	// file to a prefix of itself - a different name, with the same clusters.
	let short_len = listed_after(&|img, at| img[at + 32 + 3] = 3);
	assert_eq!(short_len, ["NEXT.TXT"], "a NameLength the fragments do not support");

	// A hash that does not match the name beside it. The system that wrote the medium skips this
	// set on lookup, so accepting it means opening - and deleting - a file nothing else can find.
	let bad_hash = listed_after(&|img, at| img[at + 32 + 4] ^= 0xFF);
	assert_eq!(bad_hash, ["NEXT.TXT"], "a set whose NameHash does not match its own name");

	// ValidDataLength past DataLength, which would make the zero-fill read past the file.
	let bad_vdl = listed_after(&|img, at| img[at + 32 + 8..at + 32 + 16].copy_from_slice(&u64::MAX.to_le_bytes()));
	assert_eq!(bad_vdl, ["NEXT.TXT"], "a ValidDataLength past the DataLength");

	// A bare File entry claiming secondaries it does not have room to describe.
	// (the count alone - zeroing the leftover fragments would terminate the directory instead,
	// which is a different refusal and would make the assertion below pass for the wrong reason)
	let no_name = listed_after(&|img, at| img[at + 1] = 1);
	assert_eq!(no_name, ["NEXT.TXT"], "a set with a stream and no name entries");

	// A record where a name fragment must be. A vendor extension is legal AFTER the name entries
	// and not in the middle of them, and reading one as a fragment invents fifteen units of name.
	let vendor_fragment = listed_after(&|img, at| img[at + 64] = 0xE0);
	assert_eq!(vendor_fragment, ["NEXT.TXT"], "a non-0xC1 record among the name entries");

	// And the healthy fixture must list both, or every assertion above is vacuous.
	let untouched = listed_after(&|_, _| {});
	assert_eq!(untouched, ["GOOD.TXT", "NEXT.TXT"]);
}

#[test]
fn an_overwrite_keeps_the_attributes_and_read_only_refuses_one() {
	// The attribute byte is the file's, not the writer's. Rewriting an entry with a fresh 0x20
	// cleared read-only, hidden and system - flags a user or another system set, silently dropped
	// by a write that succeeded. And read-only was decoration: nothing on the write or remove path
	// looked at it, so the one flag whose entire purpose is to refuse a write did not.
	for (label, kind) in [("fat16", Kind::Fat16), ("fat32", Kind::Fat32)] {
		let mut fs = FatFs::mount(MemDisk { data: build_fat(kind, ROOT) }).unwrap();
		fs.write_file(b"FLAGS.TXT", b"one").unwrap();
		let root_off = classic_root_off(&fs.dev.data);
		let at = (0..).map(|k| root_off + k * 32).find(|&at| &fs.dev.data[at..at + 11] == b"FLAGS   TXT").expect("the entry");

		// hidden + system, set by whoever owns the medium.
		fs.dev.data[at + 11] |= 0x02 | 0x04;
		fs.write_file(b"FLAGS.TXT", b"two").unwrap();
		let after = fs.dev.data[at + 11];
		assert_eq!(after & 0x06, 0x06, "{label}: an overwrite dropped hidden/system");
		assert_eq!(after & 0x20, 0x20, "{label}: an overwrite must still mark the file archived");

		// and read-only refuses both mutations, without disturbing the file.
		fs.dev.data[at + 11] |= 0x01;
		assert_eq!(fs.write_file(b"FLAGS.TXT", b"three"), Err(FsError::ReadOnly), "{label}: a read-only file was overwritten");
		assert_eq!(fs.remove(b"FLAGS.TXT"), Err(FsError::ReadOnly), "{label}: a read-only file was removed");
		assert_eq!(fs.read_file(b"FLAGS.TXT").unwrap(), b"two");
	}
}

#[test]
fn an_exfat_overwrite_keeps_the_attributes_and_read_only_refuses_one() {
	// The same rule on the other family, where the flags live in the File entry's FileAttributes.
	let mut fs = FatFs::mount(MemDisk { data: build_exfat(&[]) }).unwrap();
	fs.write_file(b"FLAGS.TXT", b"one").unwrap();
	let at = exfat_first_set(&fs.dev.data, 26 * 512);
	let restamp = |img: &mut Vec<u8>, at: usize| {
		let count = img[at + 1] as usize + 1;
		let sum = exfat_set_checksum(&img[at..at + count * 32]);
		img[at + 2..at + 4].copy_from_slice(&sum.to_le_bytes());
	};

	fs.dev.data[at + 4] |= 0x02 | 0x04;
	restamp(&mut fs.dev.data, at);
	fs.write_file(b"FLAGS.TXT", b"two").unwrap();
	let after = fs.dev.data[exfat_first_set(&fs.dev.data, 26 * 512) + 4];
	assert_eq!(after & 0x06, 0x06, "an exFAT overwrite dropped hidden/system");
	assert_eq!(after & 0x20, 0x20, "an exFAT overwrite must still mark the file archived");

	let at = exfat_first_set(&fs.dev.data, 26 * 512);
	fs.dev.data[at + 4] |= 0x01;
	restamp(&mut fs.dev.data, at);
	assert_eq!(fs.write_file(b"FLAGS.TXT", b"three"), Err(FsError::ReadOnly));
	assert_eq!(fs.remove(b"FLAGS.TXT"), Err(FsError::ReadOnly));
	assert_eq!(fs.read_file(b"FLAGS.TXT").unwrap(), b"two");
}

// A device that records the exFAT VolumeFlags of every boot-sector write, so the dirty flag's
// lifetime across an operation can be observed rather than assumed.
struct FlagLog {
	inner: MemDisk,
	flags: Vec<u16>,
}

impl BlockDevice for FlagLog {
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		self.inner.read_block(lba, buf)
	}

	fn write_block(&mut self, lba: u64, buf: &[u8]) -> bool {
		if lba == 0 {
			self.flags.push(u16::from_le_bytes([buf[106], buf[107]]));
		}
		self.inner.write_block(lba, buf)
	}
}

#[test]
fn an_exfat_mutation_runs_between_a_raised_and_a_cleared_dirty_flag() {
	// VolumeDirty is the recovery signal the format defines, and it was not maintained "to save
	// sector writes". A volume that lost power mid-write then looked exactly like one that did not.
	//
	// The two fields sit in the three bytes the boot checksum skips, precisely so a running system
	// can keep them current - maintaining them cannot invalidate the checksum, which is the reason
	// the saving was not worth taking.
	let inner = MemDisk { data: build_exfat(&[]) };
	let mut fs = FatFs::mount(FlagLog { inner, flags: Vec::new() }).unwrap();
	fs.write_file(b"A.TXT", b"one").unwrap();
	assert_eq!(fs.dev.flags.first().map(|f| f & 0x02), Some(0x02), "the flag must be raised before the transaction");
	assert_eq!(fs.dev.flags.last().map(|f| f & 0x02), Some(0), "and cleared after it");
	assert_eq!(fs.dev.inner.data[112], 0xFF, "PercentInUse must say unknown rather than a stale number");

	fs.dev.flags.clear();
	fs.remove(b"A.TXT").unwrap();
	assert_eq!(fs.dev.flags.first().map(|f| f & 0x02), Some(0x02), "a remove is a metadata transaction too");
	assert_eq!(fs.dev.flags.last().map(|f| f & 0x02), Some(0));

	// The boot checksum still holds, which is the whole reason these fields are exempt from it.
	let image = core::mem::take(&mut fs.dev.inner.data);
	assert!(FatFs::mount(MemDisk { data: image }).is_some(), "maintaining the flags must not invalidate the boot checksum");
}

#[test]
fn a_volume_left_dirty_mounts_read_only() {
	// A volume whose last writer never cleared the flag may be mid-transaction. Writing over it is
	// how a recoverable inconsistency becomes an unrecoverable one, so the mount refuses mutations
	// and leaves the repair to something that can do it.
	let mut img = build_exfat(&[File { path: "A.TXT", data: b"a" }]);
	img[106] |= 0x02;
	let mut fs = FatFs::mount(MemDisk { data: img }).expect("a dirty volume still mounts - for reading");
	assert_eq!(fs.read_file(b"A.TXT").unwrap(), b"a", "reading is exactly what a dirty mount is for");
	assert_eq!(fs.write_file(b"B.TXT", b"b"), Err(FsError::ReadOnly));
	assert_eq!(fs.remove(b"A.TXT"), Err(FsError::ReadOnly));
}

#[test]
fn a_directory_write_touches_only_the_sectors_that_changed() {
	// The old rule was "the span between the first and last differing byte", written cluster by
	// cluster. Two changes at opposite ends of a directory therefore rewrote everything between
	// them: write amplification on the medium whose writes are the scarce resource, and - because a
	// directory swap is not atomic - a crash window as wide as the span where two sectors would do.
	//
	// Measured on the function itself. The two-phase publish added for the swap already keeps each
	// of ITS two writes narrow by construction, so going through `write_file` would measure that
	// fix rather than this one; `write_dir_dirty` is still reached with distant changes by the
	// terminator scrub and by any caller that edits two places at once.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat32, &[]) }).unwrap();
	for i in 0..100u32 {
		let name = alloc::format!("F{i:03}.TXT");
		fs.write_file(name.as_bytes(), b"x").unwrap();
	}
	let root = Dir::at(fs.geo.root_cluster);
	let orig = fs.read_dir_bytes(&root).unwrap();
	let spc = fs.geo.sectors_per_cluster as usize;
	let bps = fs.geo.bytes_per_sector as usize;
	assert!(orig.len() / (spc * bps) >= 4, "the fixture must span several clusters: {} bytes", orig.len());

	// One byte changed near the front and one near the back, with everything between untouched.
	let mut bytes = orig.clone();
	bytes[8] = b'Z';
	let last = bytes.len() - 1 - 24;
	bytes[last] = 0x20;

	let image = core::mem::take(&mut fs.dev.data);
	let mut fs = FatFs::mount(WriteLog { inner: MemDisk { data: image }, writes: Vec::new() }).unwrap();
	fs.write_dir_dirty(&root, &bytes, &orig).unwrap();
	let touched = fs.dev.writes.len();
	assert_eq!(touched, 2, "two changed sectors cost {touched} writes - the span between them, not the changes");

	// Both changes actually landed: narrowing must not mean skipping.
	let back = fs.read_dir_bytes(&root).unwrap();
	assert_eq!(back[8], b'Z');
	assert_eq!(back[last], 0x20);
	assert_eq!(&back[9..last], &orig[9..last], "an untouched byte between the two changes must be untouched on the medium too");
}

#[test]
fn a_path_is_parsed_once_and_an_error_says_which_kind_of_wrong() {
	// Two parsers with different opinions and a vocabulary of one error. `resolve_dir` dropped
	// empty segments so `foo//bar` resolved; `split_parent` stripped a trailing slash so
	// `write_file("foo/")` created the FILE `foo`; reading a directory said NotFound and a path
	// through a file said NotFound too, when `FsError` already distinguishes all of them.
	let mut fs = FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, ROOT) }).unwrap();
	assert!(fs.list_dir(b"DOCS").is_ok(), "the fixture must have the directory these assertions use");
	assert_eq!(fs.read_file(b"DOCS/a.txt").unwrap(), b"in a subdir");

	// Malformed paths, refused as malformed rather than repaired.
	assert_eq!(fs.read_file(b"DOCS//a.txt"), Err(FsError::BadName), "a doubled separator is not a path");
	assert_eq!(fs.write_file(b"NEW.TXT/", b"x"), Err(FsError::BadName), "a trailing slash names a directory, not a file to create");
	assert_eq!(fs.read_file(b""), Err(FsError::BadName));

	// The wrong kind of thing, named as such.
	assert_eq!(fs.read_file(b"DOCS"), Err(FsError::IsDir), "reading a directory is not a missing file");
	assert_eq!(fs.remove(b"DOCS"), Err(FsError::IsDir), "removing a directory is not an invalid argument");
	assert_eq!(fs.read_file(b"HELLO.TXT/inner"), Err(FsError::NotDir), "a path through a file is not a missing file");
	assert_eq!(fs.list_dir(b"HELLO.TXT"), Err(FsError::NotDir));
	// And list_dir, which takes a path directly, is judged by the same rule rather than its own.
	assert_eq!(fs.list_dir(b"DOCS//"), Err(FsError::BadName));
	assert!(fs.list_dir(b"/DOCS").is_ok(), "a leading slash is an absolute path, not an empty segment");

	// And what is genuinely absent still says so.
	assert_eq!(fs.read_file(b"NOPE.TXT"), Err(FsError::NotFound));
	assert_eq!(fs.read_file(b"NOPE/a.txt"), Err(FsError::NotFound));
}

#[test]
fn a_structurally_impossible_long_name_falls_back_to_the_short_one() {
	// The sequence numbers and the checksum are most of the value and they were already checked.
	// What was not is the structural remainder: a fragment can satisfy both and still describe
	// something the format cannot hold, and the parser then reads its bytes as name units.
	//
	// In every case the file is not lost - the 8.3 alias is right there and still names it. What
	// must not happen is a name assembled out of padding, reserved fields or a sequence longer than
	// 255 units, because that name is the one a caller then stores and looks up by.
	let planted = |edit: &dyn Fn(&mut Vec<u8>, usize)| {
		let mut img = build_fat(Kind::Fat16, &[]);
		let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
		fs.write_file(b"Long name file.txt", b"x").unwrap();
		img = core::mem::take(&mut fs.dev.data);
		let root_off = classic_root_off(&img);
		edit(&mut img, root_off);
		let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
		names(&fs.list().unwrap())
	};

	// Untouched, the long name is what the listing shows - so every assertion below is a change.
	assert_eq!(planted(&|_, _| {}), ["Long name file.txt"]);

	// A sequence number past 20: twenty fragments of thirteen units is 260, and 255 is the limit.
	let over_length = planted(&|img, at| img[at] = 0x40 | 21);
	assert_eq!(over_length.len(), 1);
	assert!(over_length[0].ends_with(".TXT") && over_length[0] != "Long name file.txt", "a sequence past the format's limit was still assembled: {over_length:?}");

	// The reserved byte 12, and the first-cluster field a fragment must leave at zero: a fragment
	// carrying a cluster number is a record something else may follow.
	let reserved = planted(&|img, at| img[at + 12] = 1);
	assert_ne!(reserved[0], "Long name file.txt", "a fragment with a non-zero reserved byte was trusted");
	let cluster = planted(&|img, at| img[at + 26] = 2);
	assert_ne!(cluster[0], "Long name file.txt", "a fragment carrying a first cluster was trusted");

	// Data after the terminator in the final fragment: the units past a 0x0000 must all be 0xFFFF,
	// and reading them as name invents characters that were never written.
	let after_terminator = planted(&|img, at| {
		// the final fragment's last two name slots are at bytes 28 and 30
		img[at + 30..at + 32].copy_from_slice(&0x0041u16.to_le_bytes());
	});
	assert_ne!(after_terminator[0], "Long name file.txt", "a fragment with data past its terminator was trusted");
}

// Cross-checks against exfatprogs: read an image this tree did not build, and hand one this tree
// wrote to an independent checker. The second direction is the one that catches a driver and its
// fixtures agreeing with each other, which is exactly how the exFAT terminator stayed wrong.
//
// Skipped, loudly, when the tools are absent - a test that quietly passes on a machine without
// them would be worse than no test.
#[cfg(test)]
fn exfatprogs_available() -> bool {
	let ok = std::process::Command::new("mkfs.exfat").arg("-V").output().is_ok() && std::process::Command::new("fsck.exfat").arg("-V").output().is_ok();
	if !ok {
		std::println!("SKIPPED: exfatprogs (mkfs.exfat / fsck.exfat) is not installed - the independent cross-check did not run");
	}
	ok
}

#[test]
fn an_image_from_an_independent_formatter_mounts_and_reads() {
	if !exfatprogs_available() {
		return;
	}
	let dir = std::env::temp_dir().join("fat-gold-read");
	let _ = std::fs::create_dir_all(&dir);
	let path = dir.join("gold.img");
	let _ = std::fs::remove_file(&path);
	std::fs::write(&path, alloc::vec![0u8; 8 << 20]).expect("a blank image");
	let made = std::process::Command::new("mkfs.exfat").arg("-s").arg("512").arg("-c").arg("4096").arg(&path).output().expect("mkfs.exfat");
	assert!(made.status.success(), "mkfs.exfat failed: {}", String::from_utf8_lossy(&made.stderr));

	// Everything this driver validates at mount - the boot checksum, the revision, the FAT bounds,
	// the Up-case Table and its checksum - is being checked against a volume nothing in this tree
	// produced. A fixture cannot make this pass by sharing a mistake.
	let image = std::fs::read(&path).expect("the formatted image");
	let mut fs = FatFs::mount(MemDisk { data: image }).expect("an exfatprogs volume must mount");
	assert_eq!(fs.kind_name(), "exfat");
	assert!(fs.list().unwrap().is_empty(), "a fresh volume has no files");

	// And it is writable: the write path's idea of the layout has to match the formatter's too.
	fs.write_file(b"FROM-US.TXT", b"written by this driver").unwrap();
	assert_eq!(fs.read_file(b"FROM-US.TXT").unwrap(), b"written by this driver");
}

#[test]
fn what_this_driver_writes_passes_an_independent_checker() {
	if !exfatprogs_available() {
		return;
	}
	let dir = std::env::temp_dir().join("fat-gold-write");
	let _ = std::fs::create_dir_all(&dir);
	let path = dir.join("ours.img");
	let _ = std::fs::remove_file(&path);
	std::fs::write(&path, alloc::vec![0u8; 8 << 20]).expect("a blank image");
	let made = std::process::Command::new("mkfs.exfat").arg("-s").arg("512").arg("-c").arg("4096").arg(&path).output().expect("mkfs.exfat");
	assert!(made.status.success(), "mkfs.exfat failed: {}", String::from_utf8_lossy(&made.stderr));

	let image = std::fs::read(&path).expect("the formatted image");
	let mut fs = FatFs::mount(MemDisk { data: image }).expect("mount");
	fs.write_file(b"SMALL.TXT", b"one cluster").unwrap();
	fs.write_file(b"BIG.BIN", &alloc::vec![0xA5u8; 40 * 4096]).unwrap();
	fs.write_file("dlouhé jméno.txt".as_bytes(), b"a name outside ASCII").unwrap();
	fs.write_file(b"GONE.TXT", b"removed again").unwrap();
	fs.remove(b"GONE.TXT").unwrap();
	fs.write_file(b"SMALL.TXT", b"overwritten, which frees the old chain").unwrap();
	std::fs::write(&path, &fs.dev.data).expect("write the volume back");

	// The judgement that matters: an implementation with no stake in this one's assumptions.
	let checked = std::process::Command::new("fsck.exfat").arg("-n").arg(&path).output().expect("fsck.exfat");
	let report = alloc::format!("{}{}", String::from_utf8_lossy(&checked.stdout), String::from_utf8_lossy(&checked.stderr));
	assert!(checked.status.success(), "fsck.exfat rejected a volume this driver wrote:\n{report}");
	assert!(report.contains("clean"), "fsck.exfat did not call the volume clean:\n{report}");
}

#[cfg(test)]
fn mtools_available() -> bool {
	let ok = std::process::Command::new("mformat").arg("-v").output().is_ok() && std::process::Command::new("mdir").arg("-V").output().is_ok();
	if !ok {
		std::println!("SKIPPED: mtools (mformat / mdir / mtype) is not installed - the classic-family cross-check did not run");
	}
	ok
}

#[test]
fn what_this_driver_writes_to_a_classic_volume_another_implementation_reads() {
	// The same cross-check on the other family. mtools has no fsck, so the independent judgement is
	// a read: an implementation with no stake in this one's assumptions has to find the files, see
	// their long names, and get their bytes back.
	//
	// The volume is formatted by mformat, so the layout is not this tree's either - which is the
	// half that catches a driver and its fixtures agreeing with each other.
	if !mtools_available() {
		return;
	}
	let dir = std::env::temp_dir().join("fat-gold-classic");
	let _ = std::fs::create_dir_all(&dir);
	let path = dir.join("classic.img");
	let _ = std::fs::remove_file(&path);
	std::fs::write(&path, alloc::vec![0u8; 8 << 20]).expect("a blank image");
	let made = std::process::Command::new("mformat").arg("-i").arg(&path).arg("-F").arg("::").output().expect("mformat");
	assert!(made.status.success(), "mformat failed: {}", String::from_utf8_lossy(&made.stderr));

	let image = std::fs::read(&path).expect("the formatted image");
	let mut fs = FatFs::mount(MemDisk { data: image }).expect("an mformat volume must mount");
	fs.write_file(b"SHORT.TXT", b"eight point three").unwrap();
	fs.write_file(b"A long name with spaces.txt", b"a VFAT long name set").unwrap();
	fs.write_file(b"BIG.BIN", &alloc::vec![0x5Au8; 9000]).unwrap();
	fs.write_file(b"GONE.TXT", b"removed").unwrap();
	fs.remove(b"GONE.TXT").unwrap();
	std::fs::write(&path, &fs.dev.data).expect("write the volume back");

	let listed = std::process::Command::new("mdir").arg("-i").arg(&path).arg("-a").arg("::").output().expect("mdir");
	let report = alloc::format!("{}{}", String::from_utf8_lossy(&listed.stdout), String::from_utf8_lossy(&listed.stderr));
	assert!(listed.status.success(), "mdir could not read a directory this driver wrote:\n{report}");
	assert!(report.contains("A long name with spaces.txt"), "the long name did not survive an independent reader:\n{report}");
	assert!(report.contains("SHORT"), "{report}");
	assert!(!report.contains("GONE"), "a removed file is still listed by an independent reader:\n{report}");

	// And the bytes, not just the names.
	let typed = std::process::Command::new("mtype").arg("-i").arg(&path).arg("::A long name with spaces.txt").output().expect("mtype");
	assert_eq!(String::from_utf8_lossy(&typed.stdout).trim_end(), "a VFAT long name set", "an independent reader got different bytes back");
	let big = std::process::Command::new("mtype").arg("-i").arg(&path).arg("::BIG.BIN").output().expect("mtype");
	assert_eq!(big.stdout.len(), 9000, "a multi-cluster file read back at the wrong length");
	assert!(big.stdout.iter().all(|&b| b == 0x5A), "a multi-cluster file read back with the wrong contents");
}

// Every cluster each live file names, and which file named it - the shape both cross-link
// invariants are stated over. Only the classic families, where the root region can be parsed
// without following a chain; the exFAT paths get their coverage from fsck.exfat above.
#[cfg(test)]
fn owned_clusters<D: BlockDevice>(fs: &mut FatFs<D>) -> Result<Vec<(String, Vec<u32>)>, String> {
	// THE ROOT OF THE VOLUME IN FRONT OF IT, and by the parser that format uses.
	//
	// This read `Dir::at(0)` and parsed it with `parse_fat_dir` whatever the volume was. On exFAT
	// cluster 0 is not the root - it is the fixed root REGION, which exFAT does not have - and the
	// classic parser cannot read an entry set in any case. So every exFAT leg of the crash sweep
	// examined an empty list of files and found nothing wrong with it: the invariants were checked
	// against no entries at all. Watched: with the exFAT publish protocol's `placed` flag inverted,
	// the sweep still passed.
	let root = Dir::at(fs.root_cluster());
	let bytes = fs.read_dir_bytes(&root).map_err(|e| alloc::format!("the root is unreadable: {e:?}"))?;
	let entries = match fs.geo.kind {
		Kind::ExFat => parse_exfat_dir(&bytes, &fs.upcase, crate::dir::Location::Root).map_err(|e| alloc::format!("the root does not parse: {e:?}"))?,
		_ => parse_fat_dir(&bytes).map_err(|e| alloc::format!("the root does not parse: {e:?}"))?,
	};
	let max = fs.max_cluster();
	let mut out: Vec<(String, Vec<u32>)> = Vec::new();
	for e in entries.iter().filter(|e| !e.is_dir && e.first_cluster != 0) {
		let mut chain = Vec::new();
		// A NoFatChain extent has no FAT entries by design, so walking one would report every
		// cluster of it as free. Its clusters are the contiguous run its recorded size implies.
		if e.no_fat_chain {
			let cluster_bytes = fs.geo.sectors_per_cluster as u64 * fs.geo.bytes_per_sector as u64;
			let count = e.size.div_ceil(cluster_bytes.max(1)).max(1) as u32;
			for i in 0..count {
				chain.push(e.first_cluster + i);
			}
			out.push((e.name.clone(), chain));
			continue;
		}
		let mut c = e.first_cluster;
		let mut guard = 0;
		while c >= 2 && c <= max && !fs.is_end(c) {
			chain.push(c);
			c = fs.next_cluster(c).map_err(|err| alloc::format!("{}: unreadable chain: {err:?}", e.name))?;
			guard += 1;
			if guard > max {
				return Err(alloc::format!("{}: a chain that never ends", e.name));
			}
		}
		out.push((e.name.clone(), chain));
	}
	Ok(out)
}

// The invariants a volume must satisfy after ANY interrupted operation, whatever it was.
#[cfg(test)]
fn crash_invariants<D: BlockDevice>(fs: &mut FatFs<D>, note: &str) {
	let owned = match owned_clusters(fs) {
		Ok(owned) => owned,
		Err(why) => panic!("{note}: {why}"),
	};
	let mut seen: Vec<(u32, String)> = Vec::new();
	for (name, chain) in &owned {
		for &c in chain {
			// A live entry naming a cluster the FAT calls free is the cross-link waiting to happen:
			// the next allocation hands it out and two files share it.
			let link = fs.next_cluster(c).unwrap_or(0);
			if link == 0 {
				panic!("{note}: {name} names cluster {c}, which the FAT says is free");
			}
			if let Some((_, other)) = seen.iter().find(|(x, _)| *x == c) {
				panic!("{note}: cluster {c} is named by both {other} and {name}");
			}
			seen.push((c, name.clone()));
		}
	}
	// And every file the volume still lists must be readable: an entry that survives an interrupted
	// write pointing at something unreadable is the same damage by another route.
	for (name, _) in &owned {
		if let Err(e) = fs.read_file(name.as_bytes()) {
			panic!("{note}: {name} is listed and cannot be read: {e:?}");
		}
	}
}

#[test]
fn an_exfat_directory_spanning_three_clusters_survives_an_ordinary_write() {
	// FOUND BY WIDENING THE CRASH SWEEP, and it is not a crash defect at all - there is no
	// interruption anywhere in this test. A root directory large enough to need three clusters
	// lost the entries in its last one the first time anything was written into it.
	//
	// It surfaced because the sweep's fixture had to grow: exFAT entry sets are ~96 bytes, so the
	// old four-file fixture sat inside ONE sector and the publish protocol's two-write interleaving
	// could not occur. Widening it to twelve files reached this instead, which is worth more than
	// what was being looked for.
	//
	// AND THE DEFECT WAS THE FIXTURE'S, which is why it is worth keeping this test after the fix:
	// `build_exfat_tree` copied its allocation bitmap into the image before it built the root's
	// cluster chain, so every cluster the root needed beyond its first was marked free on the
	// medium. The driver allocated the root's own second cluster for the new file's data, and the
	// terminator that data left behind scrubbed the third away. The driver behaved correctly on an
	// image that lied to it - the fifth harness in this milestone to prove nothing for a reason
	// that had nothing to do with the code under test.
	//
	// It stays as a regression test for the fixture: it fails the moment the bitmap stops
	// describing the image it is written into.
	let seed: &[File] = &[
		File { path: "P0.TXT", data: b"padding" },
		File { path: "P1.TXT", data: b"padding" },
		File { path: "P2.TXT", data: b"padding" },
		File { path: "P3.TXT", data: b"padding" },
		File { path: "P4.TXT", data: b"padding" },
		File { path: "P5.TXT", data: b"padding" },
		File { path: "P6.TXT", data: b"padding" },
		File { path: "P7.TXT", data: b"padding" },
		File { path: "P8.TXT", data: b"padding" },
		File { path: "P9.TXT", data: b"padding" },
		File { path: "KEEP.TXT", data: b"the entry in the last cluster" },
	];
	let mut fs = FatFs::mount(MemDisk { data: build_exfat(seed) }).expect("mount");
	assert_eq!(fs.read_file(b"KEEP.TXT").expect("the fixture holds it"), b"the entry in the last cluster");
	fs.write_file(b"NEW.TXT", b"an ordinary write, nothing interrupted").expect("write");
	assert_eq!(fs.read_file(b"KEEP.TXT").expect("and it is still there afterwards"), b"the entry in the last cluster");
}

#[test]
fn every_mutating_operation_survives_being_cut_at_any_write() {
	// The generic form of the harness that found the directory-swap defect. Each operation is run
	// once per failure point, against a device whose Nth write fails and whose later writes
	// succeed - the transient error, which is both the commoner one and the only one that lets a
	// recovery path run and do its damage.
	//
	// After each cut the volume is remounted and asked the questions that do not depend on which
	// operation was interrupted: does it still mount, does every live entry name clusters the FAT
	// agrees are allocated, does any cluster have two owners, and does every listed file read.
	// Then an allocation is forced, because that is what turns a freed-but-still-named cluster from
	// latent damage into a cross-link.
	// HOLE.TXT FIRST, and it is removed before each run. The order is the whole fixture: a
	// replacement entry goes into the earliest free run, so with the hole after the file being
	// replaced it lands back in that file's own vacated slots - one write, nothing to interrupt
	// between. With the hole before it, the new entry and the old one's deletion are two separate
	// ranges and the publish protocol has two writes to be cut between.
	//
	// AND THE DIRECTORY HAS TO SPAN MORE THAN ONE SECTOR, which the four-file fixture did not. A
	// publish that fits in one write has no second sector to fail on, so the interleaving the
	// protocol exists for never occurs and every format passes for the same empty reason. That is
	// the fixture-shape trap this milestone already recorded once, met again on the format that had
	// no protocol at all: exFAT's entry sets are ~96 bytes, so four files sat inside one sector and
	// its missing two-phase publish could not be reached.
	//
	// The padding sits BETWEEN the hole and the files being replaced, which is the point of it: the
	// new entry goes into the earliest free run - the hole, in the first sector - and the old
	// entry's deletion is in a later one, so the two writes of the publish protocol land in
	// different sectors and there is something to interrupt between them.
	let seed: &[File] = &[
		File { path: "HOLE.TXT", data: b"removed to make an early hole" },
		File { path: "P0.TXT", data: b"padding, so the directory needs a second sector" },
		File { path: "P1.TXT", data: b"padding, so the directory needs a second sector" },
		File { path: "P2.TXT", data: b"padding, so the directory needs a second sector" },
		File { path: "P3.TXT", data: b"padding, so the directory needs a second sector" },
		File { path: "KEEP.TXT", data: b"untouched by any of this" },
		File { path: "OLD.TXT", data: b"the file being replaced" },
		File { path: "BIG.BIN", data: &[0x11u8; 3 * 512] },
	];
	type Op = (&'static str, fn(&mut FatFs<FailNthWrite>) -> Result<(), FsError>);
	let operations: &[Op] = &[
		("create", |fs| fs.write_file(b"NEW.TXT", b"a file that did not exist")),
		("overwrite-smaller", |fs| fs.write_file(b"BIG.BIN", b"much shorter now")),
		("overwrite-larger", |fs| fs.write_file(b"OLD.TXT", &[0x22u8; 4 * 512])),
		("remove", |fs| fs.remove(b"BIG.BIN")),
		("create-into-a-hole", |fs| fs.write_file(b"INTO.TXT", b"lands where HOLE.TXT was")),
	];

	// EVERY FORMAT, and both ways of being cut.
	//
	// The sweep built `Kind::Fat16` and injected `write_block` failures, and the two defects the
	// re-audit found sit on precisely the axes it did not cover: exFAT never got the two-phase
	// publish at all, and the classic path's `placed` flag is wrong on a failing BARRIER. Neither
	// could be caught by a harness that crashes one format one way, which is why the axes come
	// before the fixes.
	type Build = (&'static str, fn(&[File]) -> Vec<u8>);
	let formats: &[Build] = &[
		("fat12", |files| build_fat(Kind::Fat12, files)),
		("fat16", |files| build_fat(Kind::Fat16, files)),
		("fat32", |files| build_fat(Kind::Fat32, files)),
		// exFAT is swept with the SAME fixture as the rest, which it was not while the
		// three-cluster defect stood. That defect turned out to be the fixture's own bitmap and is
		// fixed, so the narrowing it forced is gone: every format here now has a root spanning
		// more than one sector, which is the only shape in which the two-phase publish has an
		// interleaving to be cut between.
		("exfat", build_exfat),
	];
	// A flush failure is scarcer than a write failure - a write per sector, a barrier per phase -
	// so its budgets are counted separately and stop sooner.
	enum Cut {
		Write(usize),
		Flush(usize),
	}
	let cuts: Vec<Cut> = (1..40usize).map(Cut::Write).chain((1..8usize).map(Cut::Flush)).collect();

	// THE FIXTURE FIRST. A harness whose image does not already hold what it is about to check is
	// the trap this milestone met four times: every assertion afterwards passes for a reason that
	// has nothing to do with the code.
	for (format, build) in formats {
		let mut check = FatFs::mount(MemDisk { data: build(seed) }).unwrap_or_else(|| panic!("{format}: the fixture does not mount"));
		for file in seed {
			assert!(check.read_file(file.path.as_bytes()).is_ok(), "{format}: the fixture does not contain {}, so nothing below is about it", file.path);
		}
	}

	for (format, build) in formats {
		for (label, operation) in operations {
			let mut cut_at_least_one = false;
			for cut in &cuts {
				let base = build(seed);
				let mut prep = FatFs::mount(MemDisk { data: base }).unwrap_or_else(|| panic!("{format}: the fixture this test just built does not mount"));
				prep.remove(b"HOLE.TXT").unwrap_or_else(|_| panic!("{format}: the early hole"));
				let holed = core::mem::take(&mut prep.device_for_test().data);

				let (device, how) = match cut {
					Cut::Write(at) => (FailNthWrite::cut_write(holed, *at), alloc::format!("write {at}")),
					Cut::Flush(at) => (FailNthWrite::cut_flush(holed, *at), alloc::format!("flush {at}")),
				};
				let mut fs = FatFs::mount(device).expect("mount");
				let outcome = operation(&mut fs);
				cut_at_least_one |= outcome.is_err();
				let image = core::mem::take(&mut fs.device_for_test().data);

				let note = alloc::format!("{format} {label} cut at {how}");
				let mut check = FatFs::mount(MemDisk { data: image }).unwrap_or_else(|| panic!("{note}: the volume no longer mounts"));
				crash_invariants(&mut check, &note);
				// KEEP.TXT is never the subject of any operation: whatever happened, it is intact.
				assert_eq!(check.read_file(b"KEEP.TXT").unwrap_or_else(|e| panic!("{note}: KEEP.TXT is gone ({e:?})")), b"untouched by any of this", "{note}: an uninvolved file was damaged");

				// Force an allocation, then ask again - this is what makes latent damage visible.
				let _ = check.write_file(b"BAIT.BIN", &[0xCCu8; 4 * 512]);
				crash_invariants(&mut check, &alloc::format!("{note}, after a later allocation"));
				assert_eq!(check.read_file(b"KEEP.TXT").unwrap_or_else(|e| panic!("{note}: KEEP.TXT is gone after a later allocation ({e:?})")), b"untouched by any of this", "{note}: an uninvolved file was damaged by a later allocation");
			}
			assert!(cut_at_least_one, "{format} {label}: no budget in the sweep interrupted it, so nothing was tested");
		}
	}
}

#[test]
fn scanning_the_fat_does_not_cost_more_reads_than_reading_it_whole() {
	// The whole-table read was justified by round trips: a per-candidate device read made allocation
	// O(volume). The window has to keep that property or it trades a memory problem for an I/O one -
	// each FAT sector read once, plus at most one extra where an entry straddles a boundary.
	for (label, kind) in [("fat12", Kind::Fat12), ("fat16", Kind::Fat16), ("fat32", Kind::Fat32)] {
		let inner = MemDisk { data: build_fat(kind, ROOT) };
		let mut fs = FatFs::mount(CountingDisk { inner, reads: 0 }).unwrap();
		let fat_sectors = fs.geo.fat_size as usize;
		fs.dev.reads = 0;
		fs.free_bytes().unwrap();
		let reads = fs.dev.reads;
		assert!(reads <= fat_sectors * 2 + 4, "{label}: counting free space cost {reads} reads over a {fat_sectors}-sector FAT");
		assert!(reads >= fat_sectors, "{label}: {reads} reads cannot have covered a {fat_sectors}-sector FAT");
	}
}

#[test]
fn an_ambiguous_commit_is_not_a_retryable_error() {
	// `FsError::CommitUncertain` existed in `fs-core`, StorageService already mapped it to a refusal
	// rather than to `Again`, and LiberFS already raised it - and this backend, which has the
	// clearest case for it in the tree, had zero occurrences. After `SwapFailure { placed: true }`
	// it answered `Io`, which reaches a caller as "try again": repeat a create whose first attempt
	// may already be on the medium.
	//
	// The dangerous half was already closed - a placed failure does not free the new chain, so a
	// retry cannot cross-link against clusters handed back to the free pool. What was left is that
	// the caller was told the opposite of the truth and the mount stayed writable.
	//
	// SWEPT over the write count rather than aimed at one, because which write of the swap a given
	// geometry fails on is not a number worth encoding in a test: every budget either completes, or
	// fails before publishing (an ordinary error), or fails after it (uncertain). What is asserted
	// is that the third case never answers anything else and never leaves the mount writable.
	let mut uncertain = 0u32;
	for until_fail in 0..24usize {
		let inner = MemDisk { data: build_fat(Kind::Fat16, &[File { path: "A.TXT", data: b"a" }]) };
		let mut fs = FatFs::mount(FlakyDisk { inner, until_fail, failed: false }).expect("mount");
		match fs.write_file(b"B.TXT", b"b") {
			Ok(()) => {}
			Err(FsError::CommitUncertain) => {
				uncertain += 1;
				assert_eq!(fs.write_file(b"C.TXT", b"c"), Err(FsError::ReadOnly), "budget {until_fail}: an uncertain commit refuses every later mutation");
				assert!(fs.list_dir(b"").is_ok(), "budget {until_fail}: and a degraded mount still reads");
			}
			// Failures before anything is published stay ordinary errors: the caller may retry, and
			// the chain it allocated has been given back.
			Err(FsError::Io) => {}
			Err(other) => panic!("budget {until_fail}: unexpected {other:?}"),
		}
	}
	assert!(uncertain > 0, "no budget in the sweep reached a published-then-failed write, so nothing here was exercised");
}

#[test]
fn an_oversized_directory_chain_is_refused_before_it_is_allocated() {
	// `MAX_DIR_BYTES` is the specification's own 256 MiB bound on an exFAT directory, and it was
	// checked on the RESULT: `read_chain(first, usize::MAX)` walked the whole chain and the length
	// was compared afterwards. A long acyclic chain therefore allocated all of it and was then
	// refused - the limit existed logically and not resource-wise, which is the difference between
	// a ceiling and a receipt.
	//
	// The observable is how many clusters were READ, not how long the refusal took: a bounded walk
	// stops at the ceiling, an unbounded one runs to the chain's end. `CountingDisk` is what makes
	// that visible.
	let img = build_fat(Kind::Fat16, &[File { path: "A.TXT", data: b"a" }]);
	let mut fs = FatFs::mount(MemDisk { data: img }).expect("mount");
	// A chain of every free cluster, linked head to tail: far more than a small ceiling allows and
	// far fewer than the real one, so the refusal is the ceiling's rather than the medium's.
	let max = fs.max_cluster();
	let free: Vec<u32> = (2..=max).filter(|&c| fs.next_cluster(c).unwrap() == 0).collect();
	assert!(free.len() > 8, "the fixture needs a chain to walk");
	for pair in free.windows(2) {
		fs.set_fat_entry_for_test(pair[0], pair[1]).expect("link");
	}
	fs.set_fat_entry_for_test(*free.last().unwrap(), 0xFFFF).expect("terminate");

	let cluster_bytes = (fs.geo.sectors_per_cluster * fs.geo.bytes_per_sector) as usize;
	// A ceiling of two clusters over a chain of many: refused, and refused as TooLarge rather than
	// as a corrupt volume - the chain is legal, it is this reader that will not hold it.
	assert_eq!(fs.read_chain_to_end_for_test(free[0], cluster_bytes * 2), Err(FsError::TooLarge));
	// And a ceiling that covers the chain still reads it, or the assertion above proves only that
	// the fixture is broken.
	let whole = fs.read_chain_to_end_for_test(free[0], cluster_bytes * free.len()).expect("a chain inside the ceiling reads");
	assert_eq!(whole.len(), cluster_bytes * free.len());
}

#[test]
fn an_exfat_set_with_a_trailing_vendor_entry_is_handled_whole() {
	// The parser anticipated records after the File Name entries - "A vendor extension may follow
	// them" - accepted sets containing them, and then handed the caller an `ent_off` that stopped
	// before them. `exfat_mark_unlinked` clears the in-use bit over `set_off..=ent_off`, so a remove
	// left those records in use inside a set whose primary was not; a Vendor Allocation entry owns
	// its own clusters and nothing released them.
	//
	// Tested at the PARSER, which is where the decision is: building a patched image to reach it
	// would prove the same thing through three more layers of fixture.
	//
	// THREE CASES, ANSWERED THE SAME WAY. This used to answer benign and critical differently, on
	// the reasoning that a benign record is "preserved by being left alone" - and the case that
	// matters is `0xE1`, Vendor Allocation: a BENIGN secondary that owns its own clusters, which an
	// overwrite drops and a remove leaks. So any record this driver does not emit makes the set
	// read-only, whichever side of bit 5 it falls on.
	//
	// `0xE1` is the one the fixture set used to bracket without covering: `0xE0` owns nothing and
	// `0x9A` was already refused.
	for (label, kind) in [("benign vendor extension", 0xE0u8), ("vendor ALLOCATION, which owns clusters", 0xE1u8), ("unknown critical secondary", 0x9Au8)] {
		let read_only = true;
		let name: Vec<u16> = "A".encode_utf16().collect();
		let upcase = crate::dir::Upcase::ascii();
		let mut set: Vec<u8> = alloc::vec![0u8; 32 * 4];
		set[0] = 0x85; // File
		set[1] = 3; // stream + one name + the extra record
		set[4] = 0x20; // attributes: archive
		set[32] = 0xC0; // Stream Extension
		// AllocationPossible, and a FAT chain. This fixture wrote a bare zero, which the parser now
		// refuses: bit 0 is `AllocationPossible` and the specification fixes it to 1 for a Stream
		// Extension, so a record without it is not one this reader may take cluster numbers from.
		set[32 + 1] = 0x01;
		set[32 + 3] = name.len() as u8;
		let hash = crate::dir::exfat_name_hash_for_test(&name, &upcase);
		set[32 + 4..32 + 6].copy_from_slice(&hash.to_le_bytes());
		set[32 + 20..32 + 24].copy_from_slice(&5u32.to_le_bytes());
		set[32 + 24..32 + 32].copy_from_slice(&1u64.to_le_bytes());
		set[32 + 8..32 + 16].copy_from_slice(&1u64.to_le_bytes());
		set[64] = 0xC1; // File Name
		set[64 + 2..64 + 4].copy_from_slice(&name[0].to_le_bytes());
		set[96] = kind; // the record the old parser stopped before
		let sum = crate::dir::exfat_set_checksum(&set);
		set[2..4].copy_from_slice(&sum.to_le_bytes());

		let entries = crate::dir::parse_exfat_dir(&set, &upcase, crate::dir::Location::Root).expect("the set parses");
		assert_eq!(entries.len(), 1, "{label}: one file");
		let e = &entries[0];
		assert_eq!(e.name, "A", "{label}: the name still reads");
		// THE WHOLE SET, so an unlink clears every record the checksum covered.
		assert_eq!(e.ent_off, 96, "{label}: the entry's range reaches the last record of the set");
		assert_eq!(e.attr & crate::dir::ATTR_READ_ONLY != 0, read_only, "{label}: whether this reader may modify the set");
	}
}

#[test]
fn a_ranged_read_answers_what_the_whole_file_read_refuses() {
	// `fs-core` documents `TooLarge` as the error whose answer is a ranged read, and this crate had
	// none - so a file past `MAX_FILE_BYTES` was unreadable by any means it offered, which for
	// removable media is a size an ordinary disc carries.
	//
	// Swept over every offset and length across a multi-cluster file, on all four filesystems and on
	// both exFAT allocation forms, because the interesting cases are the boundaries: a read starting
	// mid-cluster, one ending mid-cluster, one spanning several, and one past the end.
	static DATA: [u8; 3000] = {
		let mut d = [0u8; 3000];
		let mut i = 0;
		while i < 3000 {
			d[i] = (i % 251) as u8;
			i += 1;
		}
		d
	};
	let data: &'static [u8] = &DATA;
	let mut swept = 0u32;
	for (label, kind) in [("fat12", Kind::Fat12), ("fat16", Kind::Fat16), ("fat32", Kind::Fat32), ("exfat", Kind::ExFat)] {
		let img = build_fat(kind, &[File { path: "BIG.BIN", data }]);
		let Some(mut fs) = FatFs::mount(MemDisk { data: img }) else {
			// A geometry this fixture cannot hold the file in. Skipping is honest; asserting over
			// a volume that does not exist is not.
			continue;
		};
		assert_eq!(fs.read_file(b"BIG.BIN").expect("whole"), data, "{label}: the whole-file read still answers");
		swept += 1;
		for offset in [0u64, 1, 511, 512, 513, 1023, 1024, 2999, 3000, 4096] {
			for len in [1usize, 7, 512, 1000, 4096] {
				let mut buf = alloc::vec![0xAAu8; len];
				let n = fs.read_file_range(b"BIG.BIN", offset, &mut buf).expect("ranged");
				let start = (offset as usize).min(data.len());
				let expect = &data[start..(start + len).min(data.len())];
				assert_eq!(n, expect.len(), "{label}: offset {offset} len {len} count");
				assert_eq!(&buf[..n], expect, "{label}: offset {offset} len {len} bytes");
			}
		}
	}
	assert!(swept >= 2, "the sweep reached only {swept} filesystems");
}

#[test]
fn a_ranged_read_refuses_a_cyclic_chain_the_way_a_whole_read_does() {
	// THE INVARIANT THE NEW READER DID NOT INHERIT. `read_file` has refused cycles since the first
	// audit and has the test above; `read_file_range` was written beside it with Floyd detection on
	// the SKIP and none on the read loop, and its comment said the walk was "bounded by the same
	// Floyd cycle detection every other walk in this file uses" - which was true of half of it.
	//
	// At offset 0 the skip does not execute at all, so the half that had the detection never ran:
	// a self-loop was read twice and reported as two clusters of file. The offsets below are the two
	// that matter - before the cycle, and inside it.
	fn build(data: &'static [u8]) -> Vec<u8> {
		build_fat(Kind::Fat16, &[File { path: "LOOP.BIN", data }])
	}
	let fat_off = 512;
	let mut buffer = alloc::vec![0u8; 4 * 512];

	// The self-loop, read for two clusters.
	let mut img = build(&[0xABu8; 2 * 512]);
	let root_off = classic_root_off(&img);
	let first = u16::from_le_bytes([img[root_off + 26], img[root_off + 27]]) as usize;
	img_set_fat16(&mut img, fat_off, first, first as u16);
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	assert!(matches!(fs.read_file_range(b"LOOP.BIN", 0, &mut buffer[..2 * 512]), Err(FsError::Invalid | FsError::Corrupt)), "a self-pointing cluster read from offset 0 handed back the same cluster twice");
	assert!(matches!(fs.read_file_range(b"LOOP.BIN", 512, &mut buffer[..512]), Err(FsError::Invalid | FsError::Corrupt)), "and from inside the cycle");

	// And the two-cluster cycle, which is the shape a step budget is least able to see.
	let mut img = build(&[0xABu8; 4 * 512]);
	let first = u16::from_le_bytes([img[root_off + 26], img[root_off + 27]]) as usize;
	img_set_fat16(&mut img, fat_off, first, first as u16 + 1);
	img_set_fat16(&mut img, fat_off, first + 1, first as u16);
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	assert!(matches!(fs.read_file_range(b"LOOP.BIN", 0, &mut buffer[..4 * 512]), Err(FsError::Invalid | FsError::Corrupt)), "A -> B -> A from offset 0");
	assert!(matches!(fs.read_file_range(b"LOOP.BIN", 2 * 512, &mut buffer[..2 * 512]), Err(FsError::Invalid | FsError::Corrupt)), "and from inside it");
}

#[test]
fn an_exfat_root_carrying_a_critical_primary_this_driver_does_not_know_refuses_at_mount() {
	// THE REFUSAL THAT WAS IN A FUNCTION THE MOUNT DID NOT CALL. `exfat_bitmap` held it, and its own
	// comment said "refusing at the mount is where every other hostile-media decision in this file
	// is made, and `exfat_bitmap` is the first thing the mount asks for" - which the mount did not
	// do. It read the boot sector, loaded the up-case table and tested the dirty flag, so a volume
	// whose root carried `0x84` mounted, listed and read, and was refused only when something needed
	// free space.
	let good = build_exfat(ROOT);
	assert!(FatFs::mount(MemDisk { data: good.clone() }).is_some(), "the fixture itself must mount");

	// The root's first free slot, after the entries the fixture writes.
	// The root is cluster 3, which is one cluster past the heap start (cluster 2 is the bitmap).
	// 24 reserved sectors + 1 FAT sector, 512-byte clusters, matching `build_exfat_tree`.
	let heap = 25usize;
	let root_off = (heap + 1) * 512;
	// ASSERTED RATHER THAN ASSUMED: a wrong offset would put the damage below the root, and every
	// assertion below would pass over a volume nothing had been done to.
	assert_eq!(good[root_off], 0x81, "the root starts with the Allocation Bitmap entry");
	assert_eq!(good[root_off + 32], 0x82, "and the Up-case Table follows it");
	for (label, kind) in [("an unknown critical primary", 0x84u8), ("another one", 0x90u8)] {
		let mut img = good.clone();
		let mut at = root_off;
		while img[at] != 0x00 {
			at += 32;
		}
		img[at] = kind;
		assert!(FatFs::mount(MemDisk { data: img }).is_none(), "{label} in the root refuses the mount");
	}

	// And a second Up-case Table, which used to be resolved by position - the same defect the
	// bitmap fix was written for, one entry type over.
	let mut img = good.clone();
	let mut at = root_off;
	while img[at] != 0x00 {
		at += 32;
	}
	let mut upcase = root_off;
	while img[upcase] != 0x82 {
		upcase += 32;
	}
	let (source, destination) = (upcase, at);
	let record: [u8; 32] = img[source..source + 32].try_into().expect("one entry");
	img[destination..destination + 32].copy_from_slice(&record);
	assert!(FatFs::mount(MemDisk { data: img }).is_none(), "two Up-case Tables is a volume outside its own format, not a choice to make");
}

#[test]
fn a_cycle_that_closes_exactly_on_the_declared_length_is_still_a_cycle() {
	// THE LINK OUT OF THE LAST CLUSTER, which a bounded read never looked at. `read_chain` broke out
	// of its loop before the advance that validates the link it had just followed, and that advance
	// is where Floyd fires - so `A -> B -> A` read for exactly three clusters came back as
	// `Ok([A, B, A])`, a file made of one cluster served twice.
	//
	// The existing cycle test declares a longer file, so the walk takes one more step and the error
	// appears. The length that lands exactly on the repeat is the case nothing stated.
	//
	// Both readers, because the defect was that they DISAGREED: `read_file_range` advances inside its
	// loop body rather than behind an early break, so it refused the chain that `read_file` returned.
	let mut img = build_fat(Kind::Fat16, &[File { path: "LOOP.BIN", data: &[0xABu8; 3 * 512] }]);
	let fat_off = 512; // one reserved sector
	let root_off = classic_root_off(&img);
	let first = u16::from_le_bytes([img[root_off + 26], img[root_off + 27]]) as usize;
	img_set_fat16(&mut img, fat_off, first, first as u16 + 1);
	img_set_fat16(&mut img, fat_off, first + 1, first as u16);
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	assert_eq!(fs.read_file(b"LOOP.BIN"), Err(FsError::Invalid), "three clusters of A -> B -> A read back as a file");
	let mut buffer = alloc::vec![0u8; 3 * 512];
	assert_eq!(fs.read_file_range(b"LOOP.BIN", 0, &mut buffer), Err(FsError::Invalid), "and the two readers must agree about it");
}

#[test]
fn a_file_ending_on_a_cluster_the_fat_calls_free_is_not_a_file() {
	// The same break, the other thing it hid. A file whose size ends exactly on its first cluster
	// never had that cluster's FAT entry read, so an entry holding 0 - which says the cluster is
	// FREE - read back as a perfectly good file. `alloc_chain` hands out any entry holding 0, so the
	// next write on this volume cross-links the two.
	//
	// `a_chain_shorter_than_its_entry_is_corruption_not_a_short_file` covers the free link in the
	// MIDDLE of a chain; the last link is the one the read stopped before.
	let mut img = build_fat(Kind::Fat16, &[File { path: "ONE.BIN", data: &[0xCDu8; 512] }]);
	let fat_off = 512;
	let root_off = classic_root_off(&img);
	let first = u16::from_le_bytes([img[root_off + 26], img[root_off + 27]]) as usize;
	img_set_fat16(&mut img, fat_off, first, 0);
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	assert_eq!(fs.read_file(b"ONE.BIN"), Err(FsError::Invalid), "a file naming a cluster the FAT says is free");
	let mut buffer = alloc::vec![0u8; 512];
	assert_eq!(fs.read_file_range(b"ONE.BIN", 0, &mut buffer), Err(FsError::Invalid), "and the ranged reader agrees");
}

#[test]
fn a_chain_that_runs_off_the_volume_did_not_end() {
	// `ChainCursor::current` answered with one error for three states - below the first data cluster,
	// past the last, and the end-of-chain marker - and `read_chain_to_end` looped `while let Ok(..)`,
	// so a chain running off the volume ended its loop exactly as a terminator would. What pays for
	// that is `scan_exfat_root`: the root is the directory with no recorded length, so it is read
	// through this path, and a root scanned as far as the damage and then called whole is a root
	// whose second Allocation Bitmap - or second Up-case Table - was never seen by the rules that
	// exist to refuse them.
	let mut img = build_fat(Kind::Fat16, &[File { path: "TWO.BIN", data: &[0x5Au8; 2 * 512] }]);
	let fat_off = 512;
	let root_off = classic_root_off(&img);
	let first = u16::from_le_bytes([img[root_off + 26], img[root_off + 27]]) as usize;
	img_set_fat16(&mut img, fat_off, first + 1, 0xF000); // out of the heap, and not an end marker
	let mut fs = FatFs::mount(MemDisk { data: img }).unwrap();
	assert_eq!(fs.read_chain_to_end_for_test(first as u32, 1 << 20), Err(FsError::Invalid), "the walk stopped at the damage and reported the prefix as the whole chain");
}

#[test]
fn an_exfat_record_of_a_known_type_in_an_impossible_place_is_not_a_file() {
	// `0xC0` and `0xC1` were exempted by TYPE CODE from the classification of trailing records, in a
	// range that begins after the one Stream Extension and after every File Name entry the name
	// length calls for. So `85 C0 C1 C0`, with the checksum recomputed over it, parsed as an ordinary
	// WRITABLE file.
	//
	// The second Stream Extension carries its own FirstCluster and DataLength; `Raw` holds one
	// allocation, taken from the first. A remove clears the in-use bit across the whole set and
	// frees the first stream's chain, leaving the second stream's clusters allocated with nothing
	// naming them - the exact leak the Vendor Allocation refusal was written for, reached through a
	// type code this driver claims to know.
	//
	// Read-only would be the wrong answer here: that is what an unrecognised record earns, and this
	// is not a record the driver failed to understand but a set the format forbids.
	for (label, kind) in [("a second Stream Extension", 0xC0u8), ("a File Name entry past the count NameLength calls for", 0xC1u8)] {
		let name: Vec<u16> = "A".encode_utf16().collect();
		let upcase = crate::dir::Upcase::ascii();
		let mut set: Vec<u8> = alloc::vec![0u8; 32 * 4];
		set[0] = 0x85; // File
		set[1] = 3; // stream + one name + the record that may not be there
		set[4] = 0x20; // attributes: archive
		set[32] = 0xC0; // Stream Extension
		set[32 + 3] = name.len() as u8;
		let hash = crate::dir::exfat_name_hash_for_test(&name, &upcase);
		set[32 + 4..32 + 6].copy_from_slice(&hash.to_le_bytes());
		set[32 + 20..32 + 24].copy_from_slice(&5u32.to_le_bytes());
		set[32 + 24..32 + 32].copy_from_slice(&1u64.to_le_bytes());
		set[32 + 8..32 + 16].copy_from_slice(&1u64.to_le_bytes());
		set[64] = 0xC1; // File Name
		set[64 + 2..64 + 4].copy_from_slice(&name[0].to_le_bytes());
		set[96] = kind;
		// The second stream names clusters of its own, which is what makes accepting the set a leak.
		set[96 + 20..96 + 24].copy_from_slice(&9u32.to_le_bytes());
		set[96 + 24..96 + 32].copy_from_slice(&1u64.to_le_bytes());
		let sum = crate::dir::exfat_set_checksum(&set);
		set[2..4].copy_from_slice(&sum.to_le_bytes());

		let entries = crate::dir::parse_exfat_dir(&set, &upcase, crate::dir::Location::Root).expect("the parse itself must not fail");
		assert!(entries.is_empty(), "{label}: surfaced as a file");
	}
}

#[test]
fn an_upcase_table_is_read_as_well_as_checksummed() {
	// Structure and checksum were all this table was held to, and neither says what it MEANS. The
	// specification fixes the first 128 mappings - identity except a-z, which map to A-Z - and this
	// driver folds every name on the volume through whatever it finds: lookup, the NameHash it
	// writes, the collision it refuses. A table this driver accepts and no other implementation
	// would makes all three of those its own opinion, which is the defect the name-hash check one
	// level up already refuses.
	let good = build_exfat(ROOT);
	assert!(FatFs::mount(MemDisk { data: good.clone() }).is_some(), "the fixture itself must mount");
	let entry = 26 * 512 + 32;
	assert_eq!(good[entry], 0x82, "the fixture's second root entry is the Up-case Table");
	let table_off = 27 * 512;
	let table_len = exfat_upcase_table().len();

	// `a` mapped to itself, with the checksum recomputed so that the only thing wrong is the meaning.
	let mut img = good.clone();
	img[table_off + 4..table_off + 6].copy_from_slice(&0x0061u16.to_le_bytes());
	let mut sum: u32 = 0;
	for &b in &img[table_off..table_off + table_len] {
		sum = sum.rotate_right(1).wrapping_add(b as u32);
	}
	img[entry + 4..entry + 8].copy_from_slice(&sum.to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: img }).is_none(), "a table in which 'a' does not up-case to 'A'");

	// And the reserved fields, to the standard the Allocation Bitmap entry beside it is already held
	// to: three bytes below the checksum and twelve between it and the FirstCluster.
	for at in [1usize, 3, 8, 19] {
		let mut img = good.clone();
		img[entry + at] = 1;
		assert!(FatFs::mount(MemDisk { data: img }).is_none(), "a 0x82 entry with a nonzero reserved byte at {at}");
	}
}

// P02M0125, sixth round: the volumes this backend was never asked to look at.
//
// Every test above builds a well-formed image or interrupts this driver's own operation. What none
// of them does is hand it a volume that was ALREADY inconsistent - which is the premise every crash
// invariant here rests on and the one thing nothing checked. Each case below is one field of an
// otherwise healthy image.

// The exFAT fixtures' layout, named once: 24 reserved sectors, one FAT sector, then the heap. The
// bitmap is cluster 2, the root cluster 3, the Up-case Table cluster 4, and files start at 5.
const EX_HEAP: usize = 25;
const EX_BPS: usize = 512;

fn ex_cluster(cluster: usize) -> usize {
	(EX_HEAP + cluster - 2) * EX_BPS
}

#[test]
fn a_volume_that_already_cross_links_two_files_mounts_read_only() {
	// `unlink_in` deletes the entry durably and then frees the chain, with nothing asking whether
	// another live entry owns it. On a FAT16 volume where two files share a cluster, removing the
	// first marks the second's clusters free and the next allocation hands them to a third file - a
	// correct, durable, crash-safe operation over an ownership map that was a lie before this
	// driver touched it.
	let mut img = build_fat(Kind::Fat16, &[File { path: "A.TXT", data: &[b'a'; 600] }, File { path: "B.TXT", data: &[b'b'; 600] }]);
	// A.TXT occupies clusters 2..=3 and B.TXT 4..=5 in a 512-byte-cluster image. Point B's second
	// cluster at A's first, which is the shape a torn FAT entry leaves behind - and do it in BOTH
	// copies, so the mirror check has nothing of its own to say.
	let bps = 512usize;
	let fat_size = (5000 * 2usize).div_ceil(bps) * bps;
	for copy in 0..FATS {
		let at = bps + copy * fat_size;
		img[at + 5 * 2..at + 5 * 2 + 2].copy_from_slice(&2u16.to_le_bytes());
	}

	let mut fs = FatFs::mount(MemDisk { data: img }).expect("a cross-linked volume still mounts - the data is readable");
	assert!(fs.is_degraded(), "and it mounts READ-ONLY: its ownership map contradicts itself before this driver writes anything");
	// The refusal is what protects B.TXT: without it, removing A frees clusters B still names.
	assert_eq!(fs.remove(b"A.TXT"), Err(FsError::ReadOnly), "a remove over a lying ownership map is refused rather than performed correctly");
	assert_eq!(fs.write_file(b"C.TXT", b"x"), Err(FsError::ReadOnly), "and so is an allocation that would be handed the disputed clusters");
}

#[test]
fn a_healthy_volume_is_not_degraded_by_the_audit() {
	// The other half, and the one that says the audit is a check rather than a refusal: every
	// family this driver mounts passes it, with a subdirectory in the walk.
	for (label, kind) in [("fat12", Kind::Fat12), ("fat16", Kind::Fat16), ("fat32", Kind::Fat32)] {
		let img = build_fat(kind, &[File { path: "A.TXT", data: b"alpha" }, File { path: "SUB/B.TXT", data: b"beta" }]);
		let mut fs = FatFs::mount(MemDisk { data: img }).unwrap_or_else(|| panic!("{label} mounts"));
		assert!(!fs.is_degraded(), "{label}: a well-formed volume is not degraded by the ownership audit");
		assert_eq!(fs.write_file(b"C.TXT", b"gamma"), Ok(()), "{label}: and it is still writable");
	}
	let img = build_exfat(&[File { path: "A.TXT", data: b"alpha" }]);
	let mut fs = FatFs::mount(MemDisk { data: img }).expect("exfat mounts");
	assert!(!fs.is_degraded(), "exfat: a well-formed volume is not degraded by the ownership audit");
	assert_eq!(fs.write_file(b"C.TXT", b"gamma"), Ok(()), "exfat: and it is still writable");
}

#[test]
fn an_exfat_bitmap_that_calls_a_live_files_clusters_free_mounts_read_only() {
	// `exfat_alloc` takes the first CLEAR bit from cluster 2 upward and the mount never inspected
	// the bitmap's content, so a cleanly-flagged image whose bitmap marks a live file's clusters as
	// free has an ordinary `write_file` hand them out again - two files over one cluster, with
	// every checksum and flag on the volume correct.
	let mut img = build_exfat(&[File { path: "A.TXT", data: b"alpha" }]);
	let bitmap = ex_cluster(2);
	let bit = 5 - 2; // the first file's cluster; the bitmap indexes from cluster 2
	img[bitmap + bit / 8] &= !(1u8 << (bit % 8));

	let mut fs = FatFs::mount(MemDisk { data: img }).expect("the volume still mounts, and its data is readable");
	assert!(fs.is_degraded(), "a bitmap that disagrees with the namespace it describes is not an allocation map this driver may write through");
	assert_eq!(fs.write_file(b"B.TXT", b"beta"), Err(FsError::ReadOnly), "so the allocation that would collide is refused");
	// AND THE DATA IS STILL THERE, which is why this is read-only rather than a refused mount.
	assert_eq!(fs.read_file(b"A.TXT"), Ok(b"alpha".to_vec()), "an operator can still copy the volume off");
}

#[test]
fn an_exfat_bitmap_that_calls_a_system_chain_free_mounts_read_only() {
	// The same rule reaching the chains the volume itself is made of: the bitmap's own clusters and
	// the root. A bit clear on either means the next allocation overwrites volume metadata.
	for (label, cluster) in [("the bitmap's own chain", 2usize), ("the root", 3)] {
		let mut img = build_exfat(&[File { path: "A.TXT", data: b"alpha" }]);
		let bitmap = ex_cluster(2);
		let bit = cluster - 2;
		img[bitmap + bit / 8] &= !(1u8 << (bit % 8));
		let mut fs = FatFs::mount(MemDisk { data: img }).unwrap_or_else(|| panic!("{label}: the volume still mounts"));
		assert!(fs.is_degraded(), "{label} marked free is a volume whose next write overwrites its own metadata");
	}
}

#[test]
fn a_directory_whose_valid_length_stops_short_of_its_size_is_not_a_directory() {
	// `resolve_dir` stores `size` for a directory, and the only check was `valid_len > size` - so an
	// entry claiming `ValidDataLength = 0` with `DataLength = 512` had this driver interpret the
	// whole cluster of undefined data as directory entries. Stale-but-plausible entry sets left in
	// that tail become phantom files that path resolution reaches and that `read_file`, `remove` and
	// overwrite act on.
	let mut img = build_exfat_tree(&[], &[], &["SUB"]);
	let set = exfat_first_set(&img, ex_cluster(3));
	let stream = set + 32;
	img[stream + 8..stream + 16].copy_from_slice(&0u64.to_le_bytes());
	let checksum = exfat_set_checksum(&img[set..set + (img[set + 1] as usize + 1) * 32]);
	img[set + 2..set + 4].copy_from_slice(&checksum.to_le_bytes());

	let mut fs = FatFs::mount(MemDisk { data: img }).expect("the volume mounts");
	// The set is refused outright, so the directory is not there at all - which is the honest answer
	// for a record whose two lengths contradict each other.
	assert_eq!(fs.list_dir(b"SUB"), Err(FsError::NotFound), "a directory with an undefined tail is not a directory this driver will walk");
}

#[test]
fn a_critical_primary_in_a_subdirectory_makes_it_invalid() {
	// `if e[0] != 0x85 { i += 32; continue; }` was one rule for both places. In the ROOT the mount
	// expects the Allocation Bitmap, the Up-case Table and the Volume Label; in a SUBDIRECTORY none
	// of them may appear, and any critical primary that is not a File entry makes the containing
	// directory invalid - so `0x81` inside one was stepped over and the directory kept listing,
	// resolving and mutating.
	let mut img = build_exfat_tree(&[], &[], &["SUB"]);
	// The directory gets the first cluster after the bitmap, the root and the Up-case Table.
	img[ex_cluster(5)] = 0x81; // an Allocation Bitmap entry, inside a subdirectory

	let mut fs = FatFs::mount(MemDisk { data: img }).expect("the volume mounts - the root is intact");
	assert_eq!(fs.list_dir(b"SUB"), Err(FsError::Corrupt), "a critical primary this driver does not expect here makes the directory invalid, not something to step over");
	// And the root still lists, because the rule is about WHERE the record is.
	assert!(fs.list().is_ok(), "the root is unaffected");
}

#[test]
fn an_upcase_table_that_ends_early_is_refused() {
	// The size was bounded from above - `size > 2 * 0x1_0000` - and not from below, so a table that
	// expands to 128 mappings was accepted and `up()` then treated every character above them as an
	// identity mapping, because a lookup past the end returns its input. An implementation may
	// ignore the non-mandatory part only if it restricts create and rename to the first 128
	// characters; this one accepts Unicode names and uses the volume's table for their hash and
	// comparison.
	//
	// Confirmed against `mkfs.exfat`: a real table is 5836 bytes and expands to exactly 65536.
	let short: Vec<u16> = alloc::vec![0xFFFF, 0x61, 0xFFFF, 0x1F];
	let mut raw: Vec<u8> = Vec::new();
	for u in short {
		raw.extend_from_slice(&u.to_le_bytes());
	}
	assert!(crate::dir::Upcase::decode(&raw).is_none(), "a table that describes 128 characters and stops is not a table for a volume with Unicode names on it");

	// And the one the fixtures build, which covers the plane, still decodes - so this is a rule
	// about coverage and not a rejection of the compressed form.
	assert!(crate::dir::Upcase::decode(&exfat_upcase_table()).is_some(), "a table that covers 0000h-FFFFh decodes");
}

#[test]
fn a_directory_with_no_first_cluster_is_not_the_root() {
	// `resolve_dir` turned EVERY directory entry whose `first_cluster` is zero into the root
	// cluster - unconditionally, for both FAT families. The rule it was reaching for is a classic
	// one: a `..` entry pointing at the root records cluster zero, because FAT12/16 keep the root
	// outside the cluster area and have no number for it. Applied to an ordinary directory it
	// ALIASES THE ROOT, so a path resolved to a different directory than the one it names - and
	// then created, listed and removed files there.
	//
	// A FAT32 image with a subdirectory, whose entry is patched to cluster zero. exFAT reaches the
	// same line through valid metadata (its specification permits a zero-length directory with no
	// allocation and it has no dot entries at all), but a damaged classic entry gets there too and
	// is far cheaper to construct.
	let img = build_fat(Kind::Fat32, &[File { path: "DOCS/a.txt", data: b"in a subdir" }, File { path: "ROOTMARK.TXT", data: b"at the top" }]);

	let mut patched = img.clone();
	// The 8.3 entry for `DOCS` in the root directory: cluster high at offset 20, low at 26.
	let at = patched.windows(11).position(|w| w == b"DOCS       ").expect("the fixture writes a DOCS directory entry");
	patched[at + 20..at + 22].fill(0);
	patched[at + 26..at + 28].fill(0);

	let mut fs = FatFs::mount(MemDisk { data: patched }).expect("the patched image still mounts");
	// THE POINT: whatever this answers, it must not be the ROOT's contents. Listing the root
	// through a path that names something else is the failure, and an error is the honest answer
	// for a directory this reader cannot follow.
	match fs.list_dir(b"DOCS") {
		Err(_) => {}
		Ok(entries) => {
			let names: alloc::vec::Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
			panic!("a directory with no first cluster resolved to something listable: {names:?}");
		}
	}

	// And the unpatched image is ordinary, so this refuses the aliasing rather than the directory.
	let mut good = FatFs::mount(MemDisk { data: img }).expect("mount");
	assert!(good.list_dir(b"DOCS").is_ok(), "an allocated subdirectory still lists");
	assert_eq!(good.read_file(b"DOCS/a.txt").expect("read"), b"in a subdir");
	// `..` STILL REACHES THE ROOT on a classic volume, which is the case the normalisation exists
	// for and the one that must not be broken by narrowing it.
	assert_eq!(good.read_file(b"DOCS/../ROOTMARK.TXT").expect("parent traversal"), b"at the top");
}

#[test]
fn a_classic_fat_too_short_for_its_own_heap_is_refused() {
	// `max_cluster` takes the SMALLER of the table's capacity and the data region's, so a BPB
	// declaring a large heap and a FAT that can address only its first part mounted writable as a
	// quietly smaller filesystem. `total_bytes()` then reports the whole declared heap while
	// allocation and free-space scanning stop at the implicit bound - premature `NoSpace` on a
	// volume the caller was told had room. The exFAT parser has always refused the same
	// inconsistency; the classic one did not.
	for (label, kind) in [("fat12", Kind::Fat12), ("fat16", Kind::Fat16), ("fat32", Kind::Fat32)] {
		let good = build_fat(kind, &[File { path: "HELLO.TXT", data: b"hi" }]);
		assert!(FatFs::mount(MemDisk { data: good.clone() }).is_some(), "{label}: the unmodified fixture mounts");

		// Halve the declared FAT size, leaving the data region's own claim untouched.
		let mut short = good;
		let fat16_size = u16::from_le_bytes([short[22], short[23]]);
		if fat16_size != 0 {
			short[22..24].copy_from_slice(&(fat16_size / 2).to_le_bytes());
		} else {
			let fat32_size = u32::from_le_bytes([short[36], short[37], short[38], short[39]]);
			short[36..40].copy_from_slice(&(fat32_size / 2).to_le_bytes());
		}
		assert!(FatFs::mount(MemDisk { data: short }).is_none(), "{label}: a FAT that cannot address the heap it declares is not a volume to mount");
	}
}

#[test]
fn a_flush_that_fails_after_the_commit_does_not_report_the_write_as_failed() {
	// `swap_entry` returns `Ok` only after barriering twice - once behind the new entry set and once
	// behind the old one's retirement - so by the time its caller runs, the namespace change is
	// already durable. The caller then barriered again with `?`, which turned a failing POST-COMMIT
	// flush into `FsError::Io` for a write that HAD committed.
	//
	// StorageService maps `Io` to `Again` and drops the mount, so the caller retries an operation
	// known to have landed: another overwrite on classic FAT, leaking the old chain, and on exFAT a
	// `VolumeDirty` left set so the remount is read-only over a consistent namespace.
	//
	// The flush index is found by counting. The LAST flush of the write was already best-effort -
	// it is the cleanup one - so the flush that matters is the one BEFORE it: the barrier that
	// stood between the commit and freeing the old chain, and the one that used to be propagated
	// with `?`.
	let img = build_fat(Kind::Fat32, &[File { path: "A.TXT", data: b"first" }]);
	let mut fs = FatFs::mount(FailNthWrite::cut_flush(img.clone(), 0)).expect("mount");
	fs.write_file(b"A.TXT", b"second").expect("a healthy overwrite");
	let flushes = fs.device_for_test().flushes;
	assert!(flushes >= 3, "the write barriers more than once: {flushes}");

	// Now fail exactly the last one.
	let mut fs = FatFs::mount(FailNthWrite::cut_flush(img, flushes - 1)).expect("mount");
	let outcome = fs.write_file(b"A.TXT", b"second");
	assert!(outcome.is_ok(), "a flush failing after the commit is not the write failing: {outcome:?}");
	assert_eq!(fs.read_file(b"A.TXT").expect("read back"), b"second", "and the committed contents are what the file holds");
}

#[test]
fn mirrored_fat_copies_that_disagree_do_not_stay_writable() {
	// `set_fat_entry` keeps future writes identical across every copy, which is not the same as
	// checking what is already on the medium. With mirroring enabled every read and every ownership
	// decision uses copy 0 and nothing ever looked at the others - so two copies carrying different
	// but individually in-range chains both pass their own audit, this driver and a foreign
	// implementation reading another copy disagree about where a file's data is, and the mount
	// stayed WRITABLE over that disagreement.
	for (label, kind) in [("fat16", Kind::Fat16), ("fat32", Kind::Fat32)] {
		let img = build_fat(kind, &[File { path: "HELLO.TXT", data: b"hi" }]);
		let fs = FatFs::mount(MemDisk { data: img.clone() }).expect("mount");
		assert!(!fs.is_degraded(), "{label}: the unmodified fixture mounts writable");

		// Find where the second copy starts and change one byte of it - a difference that is not
		// otherwise detectable, because copy 0 is what everything reads.
		let bytes_per_sector = u16::from_le_bytes([img[11], img[12]]) as usize;
		let reserved = u16::from_le_bytes([img[14], img[15]]) as usize;
		let fat16_size = u16::from_le_bytes([img[22], img[23]]) as usize;
		let fat_size = if fat16_size != 0 { fat16_size } else { u32::from_le_bytes([img[36], img[37], img[38], img[39]]) as usize };
		let second = (reserved + fat_size) * bytes_per_sector;

		let mut skewed = img;
		skewed[second + 8] ^= 0xFF;
		let fs = FatFs::mount(MemDisk { data: skewed }).expect("it still mounts - the volume is readable through the copy this build uses");
		assert!(fs.is_degraded(), "{label}: a volume whose two records of the allocation map differ is not one to write to");
	}
}

#[test]
fn a_name_two_entries_claim_may_be_read_but_not_written() {
	// Neither directory parser checks uniqueness, so two entries can fold to one name - through the
	// volume's up-casing table or a classic 8.3 alias - while owning different chains. `find_entry`,
	// remove and overwrite each take the FIRST match, so a remove leaves the other file's clusters
	// allocated with nothing naming them, and an overwrite replaces one of two things the caller
	// cannot tell apart.
	//
	// READS STILL WORK, and that is not an oversight: classic FAT's two-phase publish deliberately
	// leaves both names live after an uncertain commit, and classic FAT has no durable dirty marker,
	// so a volume can legitimately be mounted in this state after a reboot. A file that survived a
	// crash has to stay readable. What must not happen is a WRITE choosing one of two by position.
	let img = build_fat(Kind::Fat16, &[File { path: "SAME.TXT", data: b"first" }, File { path: "OTHER.TXT", data: b"second" }]);

	// Rename the second entry's 8.3 name to the first's, so two live entries claim one name.
	let at = img.windows(11).position(|w| w == b"OTHER   TXT").expect("the fixture writes OTHER.TXT");
	let mut collided = img;
	collided[at..at + 11].copy_from_slice(b"SAME    TXT");

	let mut fs = FatFs::mount(MemDisk { data: collided }).expect("the volume still mounts");
	assert!(fs.read_file(b"SAME.TXT").is_ok(), "a duplicated name is still readable - a crash can produce this state");
	assert_eq!(fs.write_file(b"SAME.TXT", b"third"), Err(FsError::Corrupt), "but an overwrite would replace one of two by position");
	assert_eq!(fs.remove(b"SAME.TXT"), Err(FsError::Corrupt), "and a remove would leave the other file's clusters behind");
}

#[test]
fn a_stream_extension_that_breaks_its_own_invariants_is_not_a_file() {
	// The parser read only `NoFatChain` out of `GeneralSecondaryFlags`. The specification fixes bit
	// 0 - `AllocationPossible` - to 1 for a Stream Extension and reserves bits 2..7, and it fixes
	// the relation between `FirstCluster` and `DataLength`: a nonzero length with no first cluster
	// describes bytes that live nowhere, and a `NoFatChain` run - followed by ARITHMETIC rather than
	// by the FAT - has no anchor without a real first cluster and a nonzero length.
	//
	// A record that breaks any of these is not a Stream Extension this reader may take cluster
	// numbers from, and taking them anyway means acting on numbers the format does not vouch for.
	let upcase = crate::dir::Upcase::ascii();
	let build = |flags: u8, cluster: u32, size: u64| -> Vec<u8> {
		let mut set = alloc::vec![0u8; 96];
		let name: Vec<u16> = "A".encode_utf16().collect();
		set[0] = 0x85;
		set[1] = 2;
		set[4] = 0x20;
		set[32] = 0xC0;
		set[32 + 1] = flags;
		set[32 + 3] = name.len() as u8;
		let hash = crate::dir::exfat_name_hash_for_test(&name, &upcase);
		set[32 + 4..32 + 6].copy_from_slice(&hash.to_le_bytes());
		set[32 + 20..32 + 24].copy_from_slice(&cluster.to_le_bytes());
		set[32 + 24..32 + 32].copy_from_slice(&size.to_le_bytes());
		set[32 + 8..32 + 16].copy_from_slice(&size.to_le_bytes());
		set[64] = 0xC1;
		set[64 + 2..64 + 4].copy_from_slice(&name[0].to_le_bytes());
		let sum = crate::dir::exfat_set_checksum(&set);
		set[2..4].copy_from_slice(&sum.to_le_bytes());
		set
	};

	// The well-formed shape first, so every refusal below is known to be about what it changed.
	assert_eq!(crate::dir::parse_exfat_dir(&build(0x01, 5, 1), &upcase, crate::dir::Location::Root).expect("parses").len(), 1, "an ordinary chained file");
	assert_eq!(crate::dir::parse_exfat_dir(&build(0x03, 5, 1), &upcase, crate::dir::Location::Root).expect("parses").len(), 1, "and an ordinary contiguous one");

	for (flags, cluster, size, why) in [
		(0x00u8, 5u32, 1u64, "AllocationPossible is not set"),
		(0x05, 5, 1, "a reserved flag bit is set"),
		(0x80, 5, 1, "the top reserved bit is set"),
		(0x01, 0, 1, "a nonzero length with no first cluster"),
		(0x03, 0, 1, "a contiguous run with no first cluster"),
		(0x03, 5, 0, "a contiguous run of no length"),
	] {
		let entries = crate::dir::parse_exfat_dir(&build(flags, cluster, size), &upcase, crate::dir::Location::Root).expect("the directory still parses");
		assert!(entries.is_empty(), "{why}: flags {flags:#04x} cluster {cluster} size {size} -> {} entries", entries.len());
	}
}

#[test]
fn a_mount_says_why_it_failed_rather_than_only_that_it_did() {
	// `mount` answered `Option`, so boot-sector I/O, absent media, an unsupported layout, a failed
	// checksum, truncated geometry and a refused allocation were ONE word - and StorageService
	// turns that word into `NotFound`, telling an operator the volume is not there when the truth
	// may be a cable. Each of these sends somebody somewhere different.
	let img = build_fat(Kind::Fat16, &[File { path: "HELLO.TXT", data: b"hi" }]);
	assert!(FatFs::mount_checked(MemDisk { data: img.clone() }).is_ok(), "the fixture mounts");

	// Not FAT at all: no boot signature. A "try another backend" answer.
	let mut blank = img.clone();
	blank[510] = 0;
	blank[511] = 0;
	assert_eq!(FatFs::mount_checked(MemDisk { data: blank }).err(), Some(MountError::NotFat), "no boot signature is not a damaged FAT volume");

	// A geometry this build cannot act on: zero sectors per cluster.
	let mut impossible = img.clone();
	impossible[13] = 0;
	assert_eq!(FatFs::mount_checked(MemDisk { data: impossible }).err(), Some(MountError::Unsupported), "an impossible geometry is not a missing volume");

	// Truncated media: the geometry claims a sector the device will not answer for.
	let mut short = img;
	short.truncate(SECTOR_SIZE * 4);
	assert_eq!(FatFs::mount_checked(MemDisk { data: short }).err(), Some(MountError::Io), "a device that will not answer is the DEVICE");

	// And `mount` is still the convenience over it, so no caller had to change.
	assert!(FatFs::mount(MemDisk { data: build_fat(Kind::Fat16, &[File { path: "A.TXT", data: b"x" }]) }).is_some());
}

#[test]
fn an_exfat_bitmap_too_short_for_the_heap_is_not_a_volume_to_write_to() {
	// Any `DataLength` was accepted, and the ownership audit only inspects bits for clusters that
	// are actually claimed - so if every system and live allocation falls inside the covered prefix,
	// a SHORT bitmap passes and the mount stays writable. `free_bytes` and `exfat_alloc` then treat
	// every cluster past the prefix as unavailable, so the volume reports less free space than its
	// own geometry contains and can answer `NoSpace` while most of the heap is simply unreachable.
	let img = build_exfat(&[File { path: "HELLO.TXT", data: b"hi" }]);
	assert!(FatFs::mount(MemDisk { data: img.clone() }).is_some(), "the fixture mounts");

	// Halve the Allocation Bitmap's declared length. Its entry is the `0x81` record in the root.
	let root = img.windows(1).position(|w| w[0] == 0x81).expect("the fixture writes an allocation bitmap entry");
	let mut short = img;
	let size = u64::from_le_bytes(short[root + 24..root + 32].try_into().unwrap());
	short[root + 24..root + 32].copy_from_slice(&(size / 2).to_le_bytes());
	assert!(FatFs::mount(MemDisk { data: short }).is_none(), "a bitmap that cannot describe the heap it belongs to is not a volume this driver acts on");
}

// NOT TESTED, AND SAID SO: a chain whose LAST FAT value is 0 or 1.
//
// `claim_chain` walks while the value is at least 2, so a free or reserved entry after the last
// live cluster exits the loop - and the final check was `cluster >= 2 && !is_end(cluster)`, which
// skipped both. That is corrected, and the comment above the check already stated the rule it
// failed to enforce.
//
// A test was written and withdrawn because it passed either way: patching an end marker in the raw
// image degrades the mount through a DIFFERENT path - the walk sees a cluster outside the pool -
// so it could not tell the corrected condition from the old one. Reaching the case needs a fixture
// that ends a chain at 0 or 1 while every other invariant still holds, which `build_fat` cannot
// produce. Left as a coverage gap rather than as a test that proves nothing.

#[test]
fn the_ownership_audit_descends_into_classic_subdirectories() {
	// The audit pushed `rec_len: Some(entry.size)` for every subdirectory regardless of family, and
	// a CLASSIC directory entry records a size of ZERO for a directory - the convention every FAT
	// implementation follows. `read_dir_bytes` then saw `Some(0)`, `read_chain(.., 0)` returned an
	// empty vector without touching a cluster, and the walk claimed the subdirectory's own chain and
	// descended into nothing. Every file and every nested directory below the root went unclaimed,
	// so the audit's premise - a cluster belongs to at most one chain - held for the ROOT alone.
	//
	// Two files in a subdirectory sharing a cluster is the condition the audit exists to catch. It
	// is invisible from the root, so a volume carrying it mounted WRITABLE.
	let img = build_fat(Kind::Fat16, &[File { path: "DOCS/a.txt", data: &[0xAA; 2048] }, File { path: "DOCS/b.txt", data: &[0xBB; 2048] }]);
	let fs = FatFs::mount(MemDisk { data: img.clone() }).expect("mount");
	assert!(!fs.is_degraded(), "the unmodified fixture mounts writable");

	// Point b.txt's directory entry at a.txt's first cluster: two files, one chain, inside a
	// subdirectory.
	let a_at = img.windows(11).position(|w| w == b"A       TXT").expect("the fixture writes a.txt");
	let b_at = img.windows(11).position(|w| w == b"B       TXT").expect("the fixture writes b.txt");
	let mut crossed = img;
	let a_lo = [crossed[a_at + 26], crossed[a_at + 27]];
	let a_hi = [crossed[a_at + 20], crossed[a_at + 21]];
	crossed[b_at + 26] = a_lo[0];
	crossed[b_at + 27] = a_lo[1];
	crossed[b_at + 20] = a_hi[0];
	crossed[b_at + 21] = a_hi[1];

	let fs = FatFs::mount(MemDisk { data: crossed }).expect("it still mounts - the volume is readable");
	assert!(fs.is_degraded(), "two files in a SUBDIRECTORY sharing a chain is what the audit is for");
}

// NOT TESTED, AND SAID SO: a file allocated over the Up-case Table.
//
// The fix is that `audit_ownership` claims the table's chain - `scan_exfat_root` validated it at
// mount and then discarded the allocation, so nothing stopped a file cross-linking the one
// structure every name on the volume is compared through.
//
// A test was written and withdrawn because it could not be made to reach the case: patching the
// file's Stream Extension to point at the table's first cluster needs the root directory located
// exactly, and a byte scan for the record markers finds `0xC0` and `0x82` inside the boot region
// and the FAT long before the directory. `build_exfat` exposes no way to place a deliberately
// cross-linked entry. Left as a coverage gap rather than as a test that passes for the wrong reason.
