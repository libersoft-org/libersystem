// Host tests for the disk probe, run with `cd src/fs/partition && cargo test` (and by
// `./check.sh --gate host-tests`).
//
// Every case here is a disk the previous probe answered `RawDisk` for, which the storage
// service read as licence to format the whole device from sector zero. They are written as
// "what is on the medium" rather than "what the function returns", because the question the
// probe answers is a question about a disk.

use super::*;
use alloc::collections::BTreeMap;

// A sparse disk: sectors that were never written read back as zeros, which is what a fresh
// image gives. `capacity` is declared rather than derived, so a partition table that reaches
// past the end of the medium can be built.
struct Image {
	sectors: BTreeMap<u64, [u8; SECTOR_SIZE]>,
	capacity: Option<u64>,
	// sectors the device refuses, so an I/O failure can be told apart from a blank one.
	dead: BTreeMap<u64, ()>,
}

impl Image {
	fn new(capacity: u64) -> Image {
		Image { sectors: BTreeMap::new(), capacity: Some(capacity), dead: BTreeMap::new() }
	}

	fn put(&mut self, lba: u64, bytes: &[u8]) -> &mut Image {
		let mut sector = [0u8; SECTOR_SIZE];
		sector[..bytes.len()].copy_from_slice(bytes);
		self.sectors.insert(lba, sector);
		self
	}

	fn edit(&mut self, lba: u64, f: impl FnOnce(&mut [u8])) -> &mut Image {
		let sector = self.sectors.entry(lba).or_insert([0u8; SECTOR_SIZE]);
		f(sector);
		self
	}
}

impl Sectors for Image {
	fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
		if self.dead.contains_key(&lba) {
			return false;
		}
		if self.capacity.is_some_and(|c| lba >= c) {
			return false;
		}
		buf[..SECTOR_SIZE].copy_from_slice(self.sectors.get(&lba).unwrap_or(&[0u8; SECTOR_SIZE]));
		true
	}

	fn capacity(&mut self) -> Option<u64> {
		self.capacity
	}
}

const CAPACITY: u64 = 65_536;
const ENTRIES_LBA: u64 = 2;
const NUM_ENTRIES: u64 = 128;
const ENTRY_SIZE: usize = 128;
const ARRAY_SECTORS: u64 = NUM_ENTRIES * ENTRY_SIZE as u64 / SECTOR_SIZE as u64;
const FIRST_USABLE: u64 = ENTRIES_LBA + ARRAY_SECTORS;
const LAST_USABLE: u64 = CAPACITY - ARRAY_SECTORS - 2;

// One partition entry: a type GUID and an inclusive span.
struct Entry {
	guid: [u8; 16],
	first: u64,
	last: u64,
}

fn liberfs_entry(first: u64, last: u64) -> Entry {
	Entry { guid: LIBERFS_TYPE_GUID, first, last }
}

// A disk carrying a protective MBR and a complete, correctly checksummed GPT (primary and
// backup) naming `entries`. The starting point for every negative case below: each test
// takes this and breaks exactly one thing, so what it proves is about that one thing.
fn gpt_disk(entries: &[Entry]) -> Image {
	let mut img = Image::new(CAPACITY);
	img.put(0, &protective_mbr());
	write_gpt(&mut img, entries);
	write_backup_gpt(&mut img, entries);
	img
}

fn protective_mbr() -> [u8; SECTOR_SIZE] {
	let mut mbr = [0u8; SECTOR_SIZE];
	mbr[446 + 4] = 0xEE;
	mbr[446 + 12..446 + 16].copy_from_slice(&u32::MAX.to_le_bytes());
	mbr[510] = 0x55;
	mbr[511] = 0xAA;
	mbr
}

// Where a GPT's two copies and its entry array sit, and how wide the array is. Spelled out
// rather than fixed, because several of the cases below are precisely a header whose
// dimensions are impossible - and such a header is only worth testing if its own checksums
// still verify. An image whose declared width disagrees with the array it checksums fails
// on the CRC, which would prove nothing about the check under test.
struct Layout {
	header_lba: u64,
	backup_lba: u64,
	entries_lba: u64,
	num_entries: u64,
	entry_size: usize,
	first_usable: u64,
	last_usable: u64,
}

impl Layout {
	// the ordinary primary GPT of a 64 K-sector disk.
	fn primary() -> Layout {
		Layout { header_lba: 1, backup_lba: CAPACITY - 1, entries_lba: ENTRIES_LBA, num_entries: NUM_ENTRIES, entry_size: ENTRY_SIZE, first_usable: FIRST_USABLE, last_usable: LAST_USABLE }
	}

	// its counterpart at the far end of the medium.
	fn backup() -> Layout {
		Layout { header_lba: CAPACITY - 1, backup_lba: 1, entries_lba: CAPACITY - 1 - ARRAY_SECTORS, ..Layout::primary() }
	}
}

// The serialized entry array for `entries`, padded to the layout's full declared length.
fn entry_array(layout: &Layout, entries: &[Entry]) -> Vec<u8> {
	let mut array = vec![0u8; (layout.num_entries * layout.entry_size.max(1) as u64) as usize];
	for (i, e) in entries.iter().enumerate() {
		let off = i * layout.entry_size.max(1);
		// a layout too narrow or too short to hold the entry simply does not carry it: those
		// cases are about the DIMENSIONS, and the image still has to be self-consistent.
		if layout.entry_size < 48 || off + 48 > array.len() {
			continue;
		}
		array[off..off + 16].copy_from_slice(&e.guid);
		// a unique partition GUID; nothing reads it, but a real table has one.
		array[off + 16..off + 24].copy_from_slice(&(i as u64 + 1).to_le_bytes());
		array[off + 32..off + 40].copy_from_slice(&e.first.to_le_bytes());
		array[off + 40..off + 48].copy_from_slice(&e.last.to_le_bytes());
	}
	array
}

// Lay a GPT header and the entry array it names, checksumming both exactly as UEFI
// requires, so the image is internally consistent whatever the layout says.
fn write_header(img: &mut Image, layout: &Layout, entries: &[Entry]) {
	let array = entry_array(layout, entries);
	for (i, chunk) in array.chunks(SECTOR_SIZE).enumerate() {
		img.put(layout.entries_lba + i as u64, chunk);
	}
	let mut hdr = [0u8; SECTOR_SIZE];
	hdr[HDR_SIGNATURE..HDR_SIGNATURE + 8].copy_from_slice(b"EFI PART");
	hdr[HDR_REVISION..HDR_REVISION + 4].copy_from_slice(&0x0001_0000u32.to_le_bytes());
	hdr[HDR_SIZE..HDR_SIZE + 4].copy_from_slice(&92u32.to_le_bytes());
	hdr[HDR_CURRENT_LBA..HDR_CURRENT_LBA + 8].copy_from_slice(&layout.header_lba.to_le_bytes());
	hdr[HDR_BACKUP_LBA..HDR_BACKUP_LBA + 8].copy_from_slice(&layout.backup_lba.to_le_bytes());
	hdr[HDR_FIRST_USABLE..HDR_FIRST_USABLE + 8].copy_from_slice(&layout.first_usable.to_le_bytes());
	hdr[HDR_LAST_USABLE..HDR_LAST_USABLE + 8].copy_from_slice(&layout.last_usable.to_le_bytes());
	hdr[HDR_ENTRIES_LBA..HDR_ENTRIES_LBA + 8].copy_from_slice(&layout.entries_lba.to_le_bytes());
	hdr[HDR_NUM_ENTRIES..HDR_NUM_ENTRIES + 4].copy_from_slice(&(layout.num_entries as u32).to_le_bytes());
	hdr[HDR_ENTRY_SIZE..HDR_ENTRY_SIZE + 4].copy_from_slice(&(layout.entry_size as u32).to_le_bytes());
	hdr[HDR_ENTRIES_CRC..HDR_ENTRIES_CRC + 4].copy_from_slice(&crc32(&array).to_le_bytes());
	let size = u32::from_le_bytes(hdr[HDR_SIZE..HDR_SIZE + 4].try_into().unwrap()) as usize;
	let crc = crc32(&hdr[..size.min(SECTOR_SIZE)]);
	hdr[HDR_CRC..HDR_CRC + 4].copy_from_slice(&crc.to_le_bytes());
	img.put(layout.header_lba, &hdr);
}

fn write_gpt(img: &mut Image, entries: &[Entry]) {
	write_header(img, &Layout::primary(), entries);
}

fn write_backup_gpt(img: &mut Image, entries: &[Entry]) {
	write_header(img, &Layout::backup(), entries);
}

// A disk with a protective MBR and ONE correctly checksummed primary GPT of the given
// layout - no backup, so nothing else can answer for it and the case under test is the only
// thing that decides.
fn primary_only(layout: Layout, entries: &[Entry]) -> Image {
	let mut img = Image::new(CAPACITY);
	img.put(0, &protective_mbr());
	write_header(&mut img, &layout, entries);
	img
}

// The good case first, so every refusal below is known to be about what it changed.
#[test]
fn a_gpt_naming_a_liberfs_partition_is_found() {
	let mut img = gpt_disk(&[liberfs_entry(2048, 40959)]);
	assert_eq!(probe(&mut img), Disk::LiberFs { first: 2048, last: 40959 });
}

#[test]
fn a_disk_with_nothing_on_it_is_the_only_one_that_may_be_formatted() {
	// no MBR signature, no GPT, no filesystem: the one answer that licenses writing.
	let mut img = Image::new(CAPACITY);
	assert_eq!(probe(&mut img), Disk::Blank);
}

#[test]
fn an_mbr_partitioned_disk_is_not_a_blank_disk() {
	// The headline case. LBA 1 of an MBR disk holds whatever the boot loader put there,
	// which is not `EFI PART` - so the old probe answered `RawDisk` and the service laid a
	// filesystem over the partition table and every partition it named.
	let mut img = Image::new(CAPACITY);
	img.edit(0, |mbr| {
		mbr[446 + 4] = 0x83; // Linux
		mbr[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
		mbr[446 + 12..446 + 16].copy_from_slice(&60000u32.to_le_bytes());
		mbr[510] = 0x55;
		mbr[511] = 0xAA;
	});
	assert_eq!(probe(&mut img), Disk::MbrWithoutLiberFs);
}

#[test]
fn a_hybrid_mbr_reads_as_a_table_not_as_emptiness() {
	// a protective 0xEE entry BESIDE real ones. Whatever else is true of such a disk,
	// something claims it.
	let mut img = Image::new(CAPACITY);
	img.edit(0, |mbr| {
		mbr[446 + 4] = 0xEE;
		mbr[446 + 12..446 + 16].copy_from_slice(&2047u32.to_le_bytes());
		mbr[462 + 4] = 0x07; // NTFS
		mbr[462 + 12..462 + 16].copy_from_slice(&60000u32.to_le_bytes());
		mbr[510] = 0x55;
		mbr[511] = 0xAA;
	});
	assert_eq!(probe(&mut img), Disk::MbrWithoutLiberFs);
}

#[test]
fn an_empty_mbr_signature_alone_does_not_claim_the_disk() {
	// a boot signature with four empty slots is what a zeroed disk that has been through a
	// partitioner looks like. Nothing is named, so nothing is at stake.
	let mut img = Image::new(CAPACITY);
	img.edit(0, |mbr| {
		mbr[510] = 0x55;
		mbr[511] = 0xAA;
	});
	assert_eq!(probe(&mut img), Disk::Blank);
}

#[test]
fn a_superfloppy_carries_data_no_partition_table_can_see() {
	// FAT laid straight onto the medium at LBA 0 - a USB stick as most of the world ships
	// them. No partition table anywhere, so every table check passes and the disk was
	// formatted over somebody's files.
	let cases: [(&str, fn(&mut [u8])); 4] = [
		("FAT", |s: &mut [u8]| {
			s[0] = 0xEB;
			s[2] = 0x90;
			s[54..59].copy_from_slice(b"FAT16");
		}),
		("FAT32", |s: &mut [u8]| {
			s[0] = 0xEB;
			s[2] = 0x90;
			s[82..87].copy_from_slice(b"FAT32");
		}),
		("exFAT", |s: &mut [u8]| {
			s[0] = 0xEB;
			s[2] = 0x90;
			s[3..11].copy_from_slice(b"EXFAT   ");
		}),
		("NTFS", |s: &mut [u8]| {
			s[0] = 0xEB;
			s[2] = 0x90;
			s[3..11].copy_from_slice(b"NTFS    ");
		}),
	];
	for (name, build) in cases {
		let mut img = Image::new(CAPACITY);
		img.edit(0, build);
		assert_eq!(probe(&mut img), Disk::ForeignFilesystem { name }, "a {name} superfloppy is somebody's disk");
	}
}

#[test]
fn our_own_whole_device_volume_is_not_a_foreign_disk_and_not_a_blank_one() {
	// The fixed whole-device layout puts a LiberFS superblock at LBA 0 with no partition
	// table anywhere - which is what every system disk this project boots looks like on its
	// SECOND boot. It must not read as foreign (the service would refuse to mount its own
	// volume) and it must not read as blank (that word is what licenses a format).
	let mut img = Image::new(CAPACITY);
	img.edit(0, |s| s[0..8].copy_from_slice(b"LIBERFS1"));
	assert_eq!(probe(&mut img), Disk::LiberFsWholeDevice);
}

#[test]
fn a_damaged_gpt_signature_is_repaired_from_the_backup_not_formatted_over() {
	// the primary header's first eight bytes are gone and everything else - the protective
	// MBR, the entry array, the backup GPT - is intact. The old probe saw no `EFI PART`,
	// called the disk raw, and formatted it.
	let mut img = gpt_disk(&[liberfs_entry(2048, 40959)]);
	img.edit(1, |hdr| hdr[0..8].fill(0));
	assert_eq!(probe(&mut img), Disk::LiberFs { first: 2048, last: 40959 }, "the backup GPT is what a backup is for");

	// and with the backup gone too, the answer is damage - never emptiness.
	let mut img = gpt_disk(&[liberfs_entry(2048, 40959)]);
	img.edit(1, |hdr| hdr[0..8].fill(0));
	img.edit(CAPACITY - 1, |hdr| hdr[0..8].fill(0));
	assert_eq!(probe(&mut img), Disk::CorruptGpt);
}

#[test]
fn a_bad_header_crc_is_not_a_table_to_act_on() {
	// one flipped bit in a field the CRC covers. Every field below it may now be anything,
	// which is exactly why the CRC has to be checked before any of them are read.
	let mut img = gpt_disk(&[liberfs_entry(2048, 40959)]);
	img.edit(1, |hdr| hdr[HDR_LAST_USABLE] ^= 0x01);
	// the backup still verifies, so the disk is readable and its partition is found.
	assert_eq!(probe(&mut img), Disk::LiberFs { first: 2048, last: 40959 });

	// break both copies and it is damage.
	let mut img = gpt_disk(&[liberfs_entry(2048, 40959)]);
	img.edit(1, |hdr| hdr[HDR_LAST_USABLE] ^= 0x01);
	img.edit(CAPACITY - 1, |hdr| hdr[HDR_LAST_USABLE] ^= 0x01);
	assert_eq!(probe(&mut img), Disk::CorruptGpt);
}

#[test]
fn a_bad_entry_array_crc_is_not_a_table_to_act_on() {
	// the header verifies and the array it names does not, so the spans in it - the numbers
	// that decide where a filesystem gets written - are unverified. Nothing was checking
	// this at all.
	let mut img = gpt_disk(&[liberfs_entry(2048, 40959)]);
	img.edit(ENTRIES_LBA, |sector| sector[40] ^= 0x01);
	img.edit(CAPACITY - 1 - ARRAY_SECTORS, |sector| sector[40] ^= 0x01);
	assert_eq!(probe(&mut img), Disk::CorruptGpt);
}

#[test]
fn a_partition_entry_may_not_overlap_the_table_that_names_it() {
	// The scenario the audit spells out: an entry with the LiberFS type GUID, `first = 1`,
	// `last = 100000`. `first` is non-zero and `last` exceeds it, so every check the old
	// probe had passed - and the mount then targeted LBA 1, which is the primary GPT header
	// itself. Finding no superblock there, the service formatted on top of the partition
	// table that had named the partition.
	let mut img = gpt_disk(&[liberfs_entry(1, 40959)]);
	assert_eq!(probe(&mut img), Disk::GptWithoutLiberFs, "a span covering the header is not a partition");

	// the entry array, and the backup metadata at the far end, are equally out of bounds.
	let mut img = gpt_disk(&[liberfs_entry(ENTRIES_LBA, 40959)]);
	assert_eq!(probe(&mut img), Disk::GptWithoutLiberFs, "nor is one covering the entry array");
	let mut img = gpt_disk(&[liberfs_entry(2048, CAPACITY - 1)]);
	assert_eq!(probe(&mut img), Disk::GptWithoutLiberFs, "nor one covering the backup header");

	// And the one that ONLY the declared usable range refuses: a large span sitting entirely
	// past `last_usable`, touching no metadata this build knows the position of. A probe
	// reasoning from "the places I know tables live" would let it through; the header says
	// where partitions may go, and that is the answer.
	let layout = Layout { last_usable: 30_000, ..Layout::primary() };
	let mut img = primary_only(layout, &[liberfs_entry(30_001, 40_959)]);
	assert_eq!(probe(&mut img), Disk::GptWithoutLiberFs, "a partition has to lie inside the usable range the header declares");
}

#[test]
fn a_partition_may_not_reach_past_the_medium() {
	// A header that is internally consistent and correctly checksummed, and describes a
	// disk twice the size of the one it is on: `last_usable` and the backup copy are both
	// past the end. Every relation between the fields holds - only the DEVICE disagrees, and
	// nothing was asking the device. The partition inside such a range would be written to
	// sectors that are not there.
	let layout = Layout { backup_lba: CAPACITY * 2 + 1, last_usable: CAPACITY * 2, ..Layout::primary() };
	let mut img = primary_only(layout, &[liberfs_entry(2048, CAPACITY + 4096)]);
	assert_eq!(probe(&mut img), Disk::CorruptGpt);

	// and the entry array is bounded the same way: a header pointing it past the end of the
	// medium cannot have it verified, whatever the header's own checksum says.
	let layout = Layout { entries_lba: CAPACITY - 2, ..Layout::primary() };
	let mut img = primary_only(layout, &[liberfs_entry(2048, 40959)]);
	assert_eq!(probe(&mut img), Disk::CorruptGpt);
}

#[test]
fn a_device_that_will_not_say_its_size_bounds_nothing() {
	// every span in a GPT is bounded from outside by the medium alone. A device that cannot
	// answer leaves them unbounded, and an unbounded table may not be acted on.
	let mut img = gpt_disk(&[liberfs_entry(2048, 40959)]);
	img.capacity = None;
	assert_eq!(probe(&mut img), Disk::CorruptGpt);
}

#[test]
fn a_degenerate_span_is_skipped_and_the_real_one_still_found() {
	// a too-small entry, an inverted one and an unused slot ahead of the real partition:
	// the walk keeps going rather than taking the first LiberFS-typed entry it sees.
	let mut img = gpt_disk(&[liberfs_entry(2048, 2055), liberfs_entry(9000, 3000), Entry { guid: [0; 16], first: 0, last: 0 }, liberfs_entry(4096, 40959)]);
	assert_eq!(probe(&mut img), Disk::LiberFs { first: 4096, last: 40959 });
}

#[test]
fn a_gpt_that_names_no_liberfs_partition_belongs_to_somebody_else() {
	let mut img = gpt_disk(&[Entry { guid: [0x28; 16], first: 2048, last: 40959 }]);
	assert_eq!(probe(&mut img), Disk::GptWithoutLiberFs);
}

#[test]
fn a_header_claiming_an_impossible_entry_array_is_refused() {
	// Each of these is a header whose declared array dimensions ARE what the image carries,
	// so both checksums verify and the only thing wrong is that the dimensions cannot be
	// true. `entry_size = 0` is the sharp one: reached without a guard, an entry width of
	// zero divides by zero on the way to the first slot.
	let cases: [(&str, Layout); 6] = [
		("no entries at all", Layout { num_entries: 0, ..Layout::primary() }),
		("a zero-wide entry", Layout { entry_size: 0, ..Layout::primary() }),
		("an entry narrower than the spec fixes", Layout { entry_size: 64, ..Layout::primary() }),
		("an entry width that is not a power of two", Layout { entry_size: 100, ..Layout::primary() }),
		("an entry wider than the sector it lives in", Layout { entry_size: 1024, num_entries: 8, ..Layout::primary() }),
		("an array longer than this build walks", Layout { num_entries: MAX_ENTRIES + 1, ..Layout::primary() }),
	];
	for (what, layout) in cases {
		let mut img = primary_only(layout, &[liberfs_entry(2048, 40959)]);
		assert_eq!(probe(&mut img), Disk::CorruptGpt, "{what}");
	}
}

#[test]
fn a_header_whose_lbas_contradict_each_other_is_refused() {
	// each of these is a header that passes both its checksums and describes a disk that
	// cannot exist: the two copies in the same place, a usable range that runs backwards,
	// and metadata sitting inside the range it declares usable.
	let cases: [(&str, Layout); 6] = [
		("primary and backup at the same LBA", Layout { backup_lba: 1, ..Layout::primary() }),
		("usable range inverted", Layout { last_usable: 1, ..Layout::primary() }),
		("first usable at zero", Layout { first_usable: 0, ..Layout::primary() }),
		// the header declaring its OWN sector usable, with the entry array moved out past
		// `last_usable` so nothing else in the header objects to it. UEFI puts both copies
		// of the metadata outside the usable range, and that is what makes "a partition
		// inside the usable range cannot overlap the tables" true at all.
		("the header inside its own usable range", Layout { first_usable: 1, entries_lba: CAPACITY - 1 - ARRAY_SECTORS, ..Layout::primary() }),
		("the backup inside the usable range", Layout { last_usable: CAPACITY - 1, ..Layout::primary() }),
		("the entry array inside the usable range", Layout { entries_lba: FIRST_USABLE + 1, ..Layout::primary() }),
	];
	for (what, layout) in cases {
		let mut img = primary_only(layout, &[liberfs_entry(2048, 40959)]);
		assert_eq!(probe(&mut img), Disk::CorruptGpt, "{what}");
	}
}

#[test]
fn a_protective_mbr_with_no_gpt_at_all_is_damage_not_emptiness() {
	// the protective MBR says a GPT is supposed to be here. Its absence at both ends means
	// the table is gone, which is the moment a disk most needs not to be formatted.
	let mut img = Image::new(CAPACITY);
	img.put(0, &protective_mbr());
	assert_eq!(probe(&mut img), Disk::CorruptGpt);
}

#[test]
fn a_device_that_does_not_answer_says_nothing_about_what_is_on_it() {
	for dead in [0u64, 1] {
		let mut img = gpt_disk(&[liberfs_entry(2048, 40959)]);
		img.dead.insert(dead, ());
		assert_eq!(probe(&mut img), Disk::Io, "LBA {dead} unreadable");
	}
	// and a failure while the entry array is read is the same claim.
	let mut img = gpt_disk(&[liberfs_entry(2048, 40959)]);
	img.dead.insert(ENTRIES_LBA, ());
	img.dead.insert(CAPACITY - 1 - ARRAY_SECTORS, ());
	assert_eq!(probe(&mut img), Disk::CorruptGpt, "an array that cannot be read cannot be verified");
}

#[test]
fn the_crc32_is_the_one_uefi_specifies() {
	// GPT is checksummed with the reflected ISO-HDLC polynomial, NOT the CRC32C the
	// filesystem uses for its own blocks. Using the wrong one would refuse every genuine
	// GPT on the planet, and the check-vector is how that stays true.
	assert_eq!(crc32(b""), 0x0000_0000);
	assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
	assert_eq!(crc32(b"The quick brown fox jumps over the lazy dog"), 0x414F_A339);
}
