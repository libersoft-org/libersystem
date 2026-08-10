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
