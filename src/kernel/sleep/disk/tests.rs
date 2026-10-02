use alloc::vec::Vec;

use abi::{ERR_INVALID, SNAPSHOT_BATCH};

use crate::mem::frame;

// What the allocator may move by on its own while a test runs - every other thread keeps running - against the tens of
// thousands of frames a snapshot or a restore holds.
const DRIFT: usize = 512;

fn fill(phys: u64, byte: u8) {
	// SAFETY: a frame the test allocated and owns, through the direct map.
	unsafe { core::ptr::write_bytes(super::bytes(phys), byte, super::PAGE as usize) };
}

// Every page the held snapshot lists, in its order.
fn listed(pages: u64) -> Vec<u64> {
	let mut out = Vec::new();
	let mut first = 0u64;
	while first < pages {
		let count = (pages - first).min(SNAPSHOT_BATCH);
		let frames = count as usize * 8;
		super::read(first, count, |offset, bytes| {
			if offset < frames {
				out.push(u64::from_le_bytes(bytes.try_into().unwrap()));
			}
			Ok(())
		})
		.expect("every batch of a held snapshot reads");
		first += count;
	}
	out
}

crate::tagged_test!(a_snapshot_holds_every_page_in_use_as_it_was_at_the_copy_and_gives_every_frame_back, [Kernel], id = "kernel.sleep.disk.a_snapshot_holds_every_page_in_use_as_it_was_at_the_copy_and_gives_every_frame_back", covers = ["kernel"]);
fn a_snapshot_holds_every_page_in_use_as_it_was_at_the_copy_and_gives_every_frame_back() {
	let marked = frame::allocate().expect("a frame to mark");
	fill(marked, 0xA5);
	let free_before = frame::free_count();
	let used = super::prepare(crate::arch::sleep::context()).expect("the test kernel's memory holds a copy of what it uses");
	assert!(used > 0);
	assert!(super::info().is_err(), "nothing is copied yet, so there is nothing to read");
	assert!(super::copy_now(), "the pages in use fit the copies prepared with their slack");
	// CHANGED AFTER THE COPY: the snapshot keeps what was there.
	fill(marked, 0x5A);
	let pages = super::taken();
	assert!(pages > 0 && pages <= used + super::SLACK);
	let info = super::info().expect("a snapshot taken is read");
	assert_eq!(info.pages, pages);
	assert_eq!(info.context, crate::arch::sleep::context(), "the context the snapshot carries is this kernel's");
	assert_eq!(info.system, super::fingerprint().system);
	let list = listed(pages);
	assert_eq!(list.len() as u64, pages);
	assert!(list.windows(2).all(|pair| pair[0] < pair[1]), "in address order, each page once");
	assert!(list.iter().all(|&page| page % super::PAGE == 0 && page != 0 && super::image_ram(page, &None)), "only RAM of an image's classes, never page zero");
	let at = list.iter().position(|&page| page == marked).expect("a page the kernel holds is in the snapshot") as u64;
	let mut seen = Vec::new();
	super::read(at, 1, |offset, bytes| {
		if offset >= 8 {
			seen.extend_from_slice(bytes);
		}
		Ok(())
	})
	.unwrap();
	assert!(seen.len() == super::PAGE as usize && seen.iter().all(|&byte| byte == 0xA5), "the page as it was at the copy, not as it is now");
	// THE BOUNDS: past the end, an empty batch, a batch over the most one call reads.
	assert_eq!(super::read(pages, 1, |_, _| Ok(())), Err(ERR_INVALID));
	assert_eq!(super::read(0, 0, |_, _| Ok(())), Err(ERR_INVALID));
	assert_eq!(super::read(0, SNAPSHOT_BATCH + 1, |_, _| Ok(())), Err(ERR_INVALID));
	assert!(super::release(), "the snapshot given back");
	assert!(!super::release(), "and only once");
	assert!(super::info().is_err());
	let free_after = frame::free_count();
	assert!(free_after + DRIFT >= free_before, "every copy, list and bitmap frame came back ({free_before} free before, {free_after} after)");
	// SAFETY: the test's own frame.
	unsafe { frame::deallocate(marked) };
}

crate::tagged_test!(a_restore_refuses_a_foreign_context_and_frames_that_are_not_ram_or_named_twice, [Kernel], id = "kernel.sleep.disk.a_restore_refuses_a_foreign_context_and_frames_that_are_not_ram_or_named_twice", covers = ["kernel"]);
fn a_restore_refuses_a_foreign_context_and_frames_that_are_not_ram_or_named_twice() {
	let context = crate::arch::sleep::context();
	let held = frame::allocate().expect("a frame");
	let other = frame::allocate().expect("a frame");
	let list = [held, other];
	assert!(super::begin(2, context, |index| Ok(list[index as usize])).is_ok(), "two RAM frames, each once");
	assert!(super::abandon());
	let mut foreign = context;
	foreign[0] ^= 1;
	assert_eq!(super::begin(2, foreign, |index| Ok(list[index as usize])), Err(ERR_INVALID), "a context another kernel wrote");
	let mut moved = context;
	moved[40] ^= 0x10;
	assert_eq!(super::begin(2, moved, |index| Ok(list[index as usize])), Err(ERR_INVALID), "another top of RAM");
	assert_eq!(super::begin(0, context, |_| Ok(held)), Err(ERR_INVALID), "no pages");
	assert_eq!(super::begin(2, context, |_| Ok(held)), Err(ERR_INVALID), "one frame named twice");
	assert_eq!(super::begin(1, context, |_| Ok(held + 8)), Err(ERR_INVALID), "not a page");
	assert_eq!(super::begin(1, context, |_| Ok(0)), Err(ERR_INVALID), "page zero");
	assert_eq!(super::begin(1, context, |_| Ok(super::ram_top())), Err(ERR_INVALID), "past the end of RAM");
	// A LIST THAT CHANGES BETWEEN THE TWO READS: the second pass holds it to the first.
	let mut reads = 0u64;
	let changing = super::begin(2, context, |index| {
		reads += 1;
		Ok(if reads > 2 { list[(index as usize + 1) % 2] + super::PAGE * 4096 } else { list[index as usize] })
	});
	assert_eq!(changing, Err(ERR_INVALID));
	assert!(!super::abandon(), "a refused begin holds nothing");
	// SAFETY: the test's own frames.
	unsafe {
		frame::deallocate(held);
		frame::deallocate(other);
	}
}

crate::tagged_test!(a_restore_keeps_every_frame_it_takes_off_the_frames_the_image_goes_to, [Kernel], id = "kernel.sleep.disk.a_restore_keeps_every_frame_it_takes_off_the_frames_the_image_goes_to", covers = ["kernel"]);
fn a_restore_keeps_every_frame_it_takes_off_the_frames_the_image_goes_to() {
	// THE TARGETS ARE FREE FRAMES, just given back - the ones the allocator hands out next - so the restore's own
	// allocations meet them and must set them aside.
	const COUNT: usize = 96;
	let mut targets: Vec<u64> = (0..COUNT).map(|_| frame::allocate().expect("a frame")).collect();
	// SAFETY: the test's own frames, given back.
	unsafe { frame::free_pages(&targets) };
	targets.sort_unstable();
	let free_before = frame::free_count();
	super::begin(COUNT as u64, crate::arch::sleep::context(), |index| Ok(targets[index as usize])).expect("free RAM frames are targets");
	assert_eq!(super::write(1, 1, |_, _| Ok(())), Err(ERR_INVALID), "pages are written in order");
	let mut first = 0u64;
	while first < COUNT as u64 {
		let count = (COUNT as u64 - first).min(40);
		super::write(first, count, |offset, into| {
			// SAFETY: the frame the restore allocated for this page, one page long.
			unsafe { core::ptr::write_bytes(into, (first as usize + offset / super::PAGE as usize) as u8, super::PAGE as usize) };
			Ok(())
		})
		.expect("the pages are written");
		first += count;
	}
	assert_eq!(super::write(COUNT as u64, 1, |_, _| Ok(())), Err(ERR_INVALID), "no page past the image");
	{
		let held = super::RESTORE.lock();
		let restore = held.as_ref().expect("the restore is held");
		assert_eq!(restore.written, COUNT as u64);
		for index in 0..COUNT as u64 {
			let [target, holding] = restore.lists.get(index);
			assert_eq!(target, targets[index as usize], "each page goes to the frame the list named");
			assert!(holding != 0 && !restore.targets.has(holding), "a page is held in a frame outside the image");
			// SAFETY: the holding frame, read through the direct map.
			assert_eq!(unsafe { super::bytes(holding).read() }, index as u8, "holding what was written for it");
		}
		assert!(restore.lists.pages.iter().all(|&page| !restore.targets.has(page)), "and the lists too");
		assert!(!restore.aside.is_empty(), "the allocator handed out targets, and they were set aside rather than used");
		assert!(restore.aside.iter().all(|&frame| restore.targets.has(frame)));
	}
	assert!(super::abandon(), "the restore dropped");
	assert!(!super::abandon());
	let free_after = frame::free_count();
	assert!(free_after + DRIFT >= free_before, "every frame it took came back ({free_before} free before, {free_after} after)");
}

crate::tagged_test!(the_system_digest_covers_the_kernel_and_every_module_by_name_bytes_and_order, [Kernel], id = "kernel.sleep.disk.the_system_digest_covers_the_kernel_and_every_module_by_name_bytes_and_order", covers = ["kernel"]);
fn the_system_digest_covers_the_kernel_and_every_module_by_name_bytes_and_order() {
	let kernel = [1u8, 2, 3, 4];
	let (a, b) = (*b"first module bytes", *b"second module");
	let base = super::digest_of(&kernel, [(&b"init"[..], &a[..]), (&b"shell"[..], &b[..])].into_iter(), &[]);
	assert_eq!(base, super::digest_of(&kernel, [(&b"init"[..], &a[..]), (&b"shell"[..], &b[..])].into_iter(), &[]), "the same parts, the same digest");
	let mut changed = a;
	changed[3] ^= 1;
	assert_ne!(base, super::digest_of(&kernel, [(&b"init"[..], &changed[..]), (&b"shell"[..], &b[..])].into_iter(), &[]), "a module's byte");
	assert_ne!(base, super::digest_of(&kernel, [(&b"inix"[..], &a[..]), (&b"shell"[..], &b[..])].into_iter(), &[]), "a module's name");
	assert_ne!(base, super::digest_of(&kernel, [(&b"shell"[..], &b[..]), (&b"init"[..], &a[..])].into_iter(), &[]), "the modules' order");
	assert_ne!(base, super::digest_of(&kernel, [(&b"init"[..], &a[..])].into_iter(), &[]), "a module fewer");
	assert_ne!(base, super::digest_of(&[1, 2, 3, 5], [(&b"init"[..], &a[..]), (&b"shell"[..], &b[..])].into_iter(), &[]), "the kernel's byte");
	assert_ne!(base, super::digest_of(&kernel, [(&b"init"[..], &a[..]), (&b"shell"[..], &b[..])].into_iter(), b"2"), "a development variant");
	// THIS BOOT'S: the same whenever asked.
	let now = super::fingerprint();
	assert_eq!(now.system, super::fingerprint().system);
	assert_eq!(now.hardware, super::fingerprint().hardware, "the hardware's digest reads nothing that moves while the machine runs");
}
