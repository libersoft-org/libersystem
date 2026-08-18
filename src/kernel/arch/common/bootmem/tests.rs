use super::*;

crate::tagged_test!(the_frame_pool_is_carved_around_what_the_boot_left_in_memory, [Kernel, Memory], id = "kernel.arch.common.bootmem.the_frame_pool_is_carved_around_what_the_boot_left_in_memory", covers = ["kernel"]);
fn the_frame_pool_is_carved_around_what_the_boot_left_in_memory() {
	// The shape that cost a milestone: one usable range with the boot packages sitting inside it.
	//
	// A pool declared over them is not wrong until something allocates that far, which is why the
	// old allocator lived with it for years - it handed out the bottom of RAM first and never
	// climbed. The answer must not depend on which allocator is downstream, so this asserts the
	// range itself: the hole is not in any region, and everything else still is.
	const BASE: u64 = 0x4008_0000;
	const LEN: u64 = 0x1FF8_0000;
	let mut holes = [Hole { start: 0x5000_0000, end: 0x5038_0000 }];
	let mut out = [bootproto::MemRegion { base: 0, length: 0, kind: 0, _pad: 0 }; 4];
	let count = carve(BASE, LEN, &mut holes, &mut out);
	assert_eq!(count, 2, "one hole in the middle leaves the piece below it and the piece above");
	assert_eq!((out[0].base, out[0].length), (BASE, 0x5000_0000 - BASE));
	assert_eq!((out[1].base, out[1].length), (0x5038_0000, BASE + LEN - 0x5038_0000));
	assert_eq!(out[0].length + out[1].length + 0x38_0000, LEN, "nothing is lost but the hole");

	// Page alignment goes OUTWARD. A hole covering one byte of a page makes the whole page
	// unusable, and rounding the other way is the bug rather than a tidier number: the page holding
	// the first byte of the boot archive is as fatal to hand out as the page holding all of it.
	let mut ragged = [Hole { start: 0x5000_0001, end: 0x5000_1001 }];
	let count = carve(BASE, LEN, &mut ragged, &mut out);
	assert_eq!(count, 2);
	assert_eq!(out[0].base + out[0].length, 0x5000_0000, "the region below stops at the page the hole starts in");
	assert_eq!(out[1].base, 0x5000_2000, "and the region above starts after the page the hole ends in");

	// Unsorted, overlapping and adjacent all at once, which is what a hand-off actually looks like:
	// a `BootInfo` and its module array are usually in the same page, and the packages come in the
	// order the loader wrote them rather than in address order.
	let mut messy = [
		Hole { start: 0x5200_0000, end: 0x5201_0000 },
		Hole { start: 0x5000_0000, end: 0x5002_0000 },
		Hole { start: 0x5001_0000, end: 0x5003_0000 },
		Hole { start: 0x5003_0000, end: 0x5004_0000 },
	];
	let count = carve(BASE, LEN, &mut messy, &mut out);
	assert_eq!(count, 3, "the three touching holes merge into one gap");
	assert_eq!(out[0].base + out[0].length, 0x5000_0000);
	assert_eq!(out[1].base, 0x5004_0000, "the merged gap ends where the last of the three does");
	assert_eq!(out[1].base + out[1].length, 0x5200_0000);
	assert_eq!(out[2].base, 0x5201_0000);

	// A hole entirely outside the range, an empty one, and one that swallows everything.
	let mut outside = [Hole { start: 0x1000_0000, end: 0x2000_0000 }, Hole { start: 0x6000_0000, end: 0x7000_0000 }, Hole { start: 0x5000_0000, end: 0x5000_0000 }];
	let count = carve(BASE, LEN, &mut outside, &mut out);
	assert_eq!(count, 1, "nothing inside the range was removed");
	assert_eq!((out[0].base, out[0].length), (BASE, LEN));
	let mut everything = [Hole { start: 0, end: 0x8000_0000 }];
	assert_eq!(carve(BASE, LEN, &mut everything, &mut out), 0, "a hole over the whole range leaves no pool at all");

	// And the bound: more pieces than `out` can hold must LOSE memory, never cover a hole. The
	// alternative - writing the last region across the remaining holes - would hand out the boot
	// archive to save a line of arithmetic.
	let mut many = [Hole { start: 0x4100_0000, end: 0x4101_0000 }, Hole { start: 0x4200_0000, end: 0x4201_0000 }, Hole { start: 0x4300_0000, end: 0x4301_0000 }];
	let mut small = [bootproto::MemRegion { base: 0, length: 0, kind: 0, _pad: 0 }; 2];
	let count = carve(BASE, LEN, &mut many, &mut small);
	assert_eq!(count, 2);
	assert!(small[1].base + small[1].length <= 0x4300_0000, "the last region it had room for stops before the hole it could not describe");
}

crate::tagged_test!(a_direct_boot_archive_states_its_own_length, [Kernel, Memory], id = "kernel.arch.common.bootmem.a_direct_boot_archive_states_its_own_length", covers = ["kernel"]);
fn a_direct_boot_archive_states_its_own_length() {
	// The direct aarch64 hand-off agrees on a START address and nothing else, so the length is read
	// out of the archive. If this reader is wrong, the frame pool is carved around the wrong range
	// and the allocator hands out part of the archive every program's ELF image comes from.
	let package = abi::bootstrap::build_package(&[(b"init".as_slice(), b"0123456789".as_slice()), (b"agent".as_slice(), b"abc".as_slice())]).expect("the writer builds an archive");
	assert_eq!(unsafe { archive_len(package.as_ptr()) }, Some(package.len() as u64), "the extent read out of the table is the file");

	// AND NOTHING ELSE MAY LOOK LIKE ONE. This runs against an address nothing has promised holds an
	// archive: on a boot where the runner loaded none it is ordinary RAM, and every one of these
	// must answer None rather than a length.
	let zeros = [0u8; 256];
	assert_eq!(unsafe { archive_len(zeros.as_ptr()) }, None, "blank RAM is not an archive");
	let mut wrong_magic = package.clone();
	wrong_magic[7] = b'2';
	assert_eq!(unsafe { archive_len(wrong_magic.as_ptr()) }, None, "the magic is eight bytes of exact match");
	// The reserved word is part of the format and a writer may not fill it, so a non-zero one is a
	// different format rather than one to read past.
	let mut reserved = package.clone();
	reserved[12] = 1;
	assert_eq!(unsafe { archive_len(reserved.as_ptr()) }, None, "the reserved word must be zero");
	// A count past the reader's ceiling is refused BEFORE the table is walked - walking it would be
	// a long read through memory that has not been shown to be an archive at all.
	let mut huge = package.clone();
	huge[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
	assert_eq!(unsafe { archive_len(huge.as_ptr()) }, None, "a count past MAX_PACKAGE_ENTRIES is refused");
	let mut empty = package.clone();
	empty[8..12].copy_from_slice(&0u32.to_le_bytes());
	assert_eq!(unsafe { archive_len(empty.as_ptr()) }, None, "an archive with no entries carries no userspace");
	// A blob claiming to start inside the header or the entry table is malformed, and taking its
	// extent anyway would understate the range that has to be carved out of the pool.
	let mut overlapping = package.clone();
	overlapping[abi::PKG_HEADER_LEN + abi::PKG_NAME_LEN..abi::PKG_HEADER_LEN + abi::PKG_NAME_LEN + 4].copy_from_slice(&4u32.to_le_bytes());
	assert_eq!(unsafe { archive_len(overlapping.as_ptr()) }, None, "a blob inside the table is not a shorter archive");
}

crate::tagged_test!(the_boot_archive_range_prefers_what_the_device_tree_says, [Kernel, Memory], id = "kernel.arch.common.bootmem.the_boot_archive_range_prefers_what_the_device_tree_says", covers = ["kernel"]);
fn the_boot_archive_range_prefers_what_the_device_tree_says() {
	// riscv64 gets an exact range from `/chosen/linux,initrd-start` / `-end`, because its tree comes
	// from the invocation that boots it. That is authoritative and is taken as given - it is also
	// what real firmware writes, so this is the path a machine that is not QEMU takes.
	assert_eq!(unsafe { boot_archive_range(0x9020_0000, 0x9043_50A0, 0, |phys| phys) }, Some((0x9020_0000, 0x0023_50A0)), "the tree's range is used as it stands");

	// A tree with no initrd range falls back to the runner's fixed address, which is the aarch64
	// path: its tree is dumped from a separate invocation and can never carry one.
	let package = abi::bootstrap::build_package(&[(b"init".as_slice(), b"payload".as_slice())]).expect("the writer builds an archive");
	let at = package.as_ptr() as u64;
	assert_eq!(unsafe { boot_archive_range(0, 0, at, |phys| phys) }, Some((at, package.len() as u64)), "no tree range means the probe address");
	// And an empty or inverted range is no range: an end at or below the start would otherwise be
	// taken as an archive of zero or wrapped length rather than as a boot that handed over nothing.
	assert_eq!(unsafe { boot_archive_range(0x9020_0000, 0x9020_0000, 0, |phys| phys) }, None, "start == end is no archive");
	assert_eq!(unsafe { boot_archive_range(0x9020_0000, 0x9000_0000, 0, |phys| phys) }, None, "end below start is no archive");
	// Nothing at the probe address, which is the direct boot of a kernel the runner handed no
	// package: it must come up and say so rather than adopt whatever was in RAM.
	let zeros = [0u8; 256];
	assert_eq!(unsafe { boot_archive_range(0, 0, zeros.as_ptr() as u64, |phys| phys) }, None, "no archive at the probe address is no archive");
}
