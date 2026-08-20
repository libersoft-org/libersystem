use super::paging;
use crate::{mem, object::address_space::AddressSpace, sched, smp};

crate::tagged_test!(concurrent_maps_on_shared_tables_strand_nothing, [Paging, Memory, Stress], id = "kernel.arch.concurrent_maps_on_shared_tables_strand_nothing", covers = ["kernel"]);
fn concurrent_maps_on_shared_tables_strand_nothing() {
	use core::sync::atomic::{AtomicU64, Ordering};
	use mem::frame;

	// The PT_LOCK stress test: two cores hammer map/unmap on virtual addresses
	// that share an intermediate page-table level, recreating the geometry of the
	// historical riscv64 race - two CPUs both observe a missing leaf table, both
	// allocate one, one write wins, and the loser's leaf lands in an orphaned table
	// (a lost mapping) while the orphan leaks a frame. Every ROUND the two workers
	// rendezvous on a barrier and map into the SAME fresh 2 MiB group (a new leaf
	// table under a shared mid-level on all three arches), then each unmap must
	// return exactly the frame that worker mapped - a stranded leaf returns None.
	// After the space drops, the pool must get at least one table frame back per
	// round: an orphaned (unlinked) table would not be reclaimed by
	// free_address_space, so the delta exposes the leak.
	const ROUNDS: u64 = 128;
	const BASE: u64 = 0x4000_0000;
	const GROUP: u64 = 0x20_0000; // 2 MiB: one leaf page table on x86 / aarch64 / riscv64
	static ROOT: AtomicU64 = AtomicU64::new(0);
	static FRAMES: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
	static ARRIVE: AtomicU64 = AtomicU64::new(0);
	static STRANDED: AtomicU64 = AtomicU64::new(0);
	static DONE: AtomicU64 = AtomicU64::new(0);

	extern "C" fn worker(which: u64) {
		let root = ROOT.load(Ordering::SeqCst);
		let frame = FRAMES[which as usize].load(Ordering::SeqCst);
		let flags = paging::PRESENT | paging::WRITABLE | paging::NO_EXECUTE;
		for round in 0..ROUNDS {
			// Rendezvous: both workers enter the round together, so the two maps
			// race on creating the same fresh leaf table.
			ARRIVE.fetch_add(1, Ordering::SeqCst);
			let mut spins = 0u64;
			while ARRIVE.load(Ordering::SeqCst) < 2 * (round + 1) {
				core::hint::spin_loop();
				spins += 1;
				if spins > 2_000_000_000 {
					STRANDED.fetch_add(1, Ordering::SeqCst);
					DONE.fetch_add(1, Ordering::SeqCst);
					return;
				}
			}
			let virt = BASE + round * GROUP + which * frame::PAGE_SIZE;
			if paging::try_map_page_in(root, virt, frame, flags).is_err() {
				STRANDED.fetch_add(1, Ordering::SeqCst);
				break;
			}
			// The leaf must still be our mapping: a stranded leaf (lost to an
			// orphaned table) reads back as unmapped here.
			if paging::unmap_page_in(root, virt) != Some(frame) {
				STRANDED.fetch_add(1, Ordering::SeqCst);
				break;
			}
		}
		DONE.fetch_add(1, Ordering::SeqCst);
	}

	// The stress needs two workers truly in parallel on their own cores; the test
	// topologies always boot with more (x86 nproc, aarch64 8, riscv64 4).
	if smp::cpu_count() < 3 {
		return;
	}
	let space = AddressSpace::create().expect("a scratch address space");
	ROOT.store(space.cr3(), Ordering::SeqCst);
	FRAMES[0].store(frame::allocate().expect("worker frame 0"), Ordering::SeqCst);
	FRAMES[1].store(frame::allocate().expect("worker frame 1"), Ordering::SeqCst);
	ARRIVE.store(0, Ordering::SeqCst);
	STRANDED.store(0, Ordering::SeqCst);
	DONE.store(0, Ordering::SeqCst);
	sched::spawn_on(1, worker, 0);
	sched::spawn_on(2, worker, 1);
	let mut spins = 0u64;
	while DONE.load(Ordering::SeqCst) < 2 {
		core::hint::spin_loop();
		spins += 1;
		assert!(spins < 20_000_000_000, "the PT stress workers did not finish");
	}
	assert_eq!(STRANDED.load(Ordering::SeqCst), 0, "a concurrent map on a shared intermediate level stranded a leaf");
	// Every round linked one fresh leaf table into the tree; dropping the space must
	// hand them all back (an orphaned table would stay allocated - the frame leak).
	let before_drop = frame::free_count();
	drop(space);
	let reclaimed = frame::free_count() - before_drop;
	assert!(reclaimed as u64 >= ROUNDS, "dropping the space reclaimed {reclaimed} frames, expected at least {ROUNDS} leaf tables - an intermediate table leaked");
	unsafe { frame::deallocate(FRAMES[0].load(Ordering::SeqCst)) };
	unsafe { frame::deallocate(FRAMES[1].load(Ordering::SeqCst)) };
}

crate::tagged_test!(paging_map_unmap, [Paging, Memory], id = "kernel.arch.paging_map_unmap", covers = ["kernel"]);
fn paging_map_unmap() {
	let phys = mem::frame::allocate().expect("scratch frame");
	// Sv39 (riscv64) only has a 39-bit canonical VA range, so the 48-bit x86/aarch64
	// scratch address below is non-canonical there and faults; use a free canonical
	// kernel-half VA just past the riscv mmap window (KERNEL_MMAP_BASE + 64 GiB).
	#[cfg(not(target_arch = "riscv64"))]
	let virt: u64 = 0xffff_f000_0000_0000;
	#[cfg(target_arch = "riscv64")]
	let virt: u64 = 0xffff_fff0_0000_0000;
	paging::map_page(virt, phys, paging::WRITABLE);
	let ptr = virt as *mut u64;
	unsafe {
		ptr.write_volatile(0xdead_beef);
		assert_eq!(ptr.read_volatile(), 0xdead_beef);
	}
	let unmapped = paging::unmap_page(virt).expect("was mapped");
	assert_eq!(unmapped, phys);
	unsafe { mem::frame::deallocate(phys) };
}

crate::tagged_test!(a_shootdown_makes_another_core_stop_using_the_old_translation, [Paging, Memory, Smp], id = "kernel.arch.a_shootdown_makes_another_core_stop_using_the_old_translation", covers = ["kernel"]);
fn a_shootdown_makes_another_core_stop_using_the_old_translation() {
	use core::sync::atomic::{AtomicU64, Ordering};
	use mem::frame;

	// The property the shootdown exists for, observed rather than inferred. Asking whether the
	// translation is gone with `translate` proves nothing: it walks the page tables, which the
	// unmap edited on this core - the question is what the OTHER core's translation buffer
	// still answers. So the test reads THROUGH the address instead, with a different frame
	// mapped at it, and looks at which frame's bytes come back.
	//
	// Without the shootdown, CPU 1 keeps its cached translation and reads the OLD frame's
	// marker - the exact shape of the bug this milestone fixed, where a frame was returned to
	// the allocator while another core still wrote through it.
	//
	// A kernel-half address, which every core shares, so a stale entry is the only thing that
	// could differ between them.
	#[cfg(not(target_arch = "riscv64"))]
	const SHARED_VA: u64 = 0xffff_f000_0010_0000;
	#[cfg(target_arch = "riscv64")]
	const SHARED_VA: u64 = 0xffff_fff0_0010_0000;
	const FIRST: u64 = 0x1111_1111_1111_1111;
	const SECOND: u64 = 0x2222_2222_2222_2222;

	static STAGE: AtomicU64 = AtomicU64::new(0);
	static SAW_FIRST: AtomicU64 = AtomicU64::new(0);
	static SAW_SECOND: AtomicU64 = AtomicU64::new(0);

	extern "C" fn reader(_arg: u64) {
		// Read once to load this core's translation buffer, wait for the remap and the
		// shootdown, then read again.
		while STAGE.load(Ordering::Acquire) < 1 {
			core::hint::spin_loop();
		}
		SAW_FIRST.store(unsafe { (SHARED_VA as *const u64).read_volatile() }, Ordering::Release);
		STAGE.store(2, Ordering::Release);
		while STAGE.load(Ordering::Acquire) < 3 {
			core::hint::spin_loop();
		}
		SAW_SECOND.store(unsafe { (SHARED_VA as *const u64).read_volatile() }, Ordering::Release);
		STAGE.store(4, Ordering::Release);
	}

	if smp::cpu_count() < 2 {
		return;
	}
	let first = frame::allocate().expect("the first frame");
	let second = frame::allocate().expect("the second frame");
	let hhdm = crate::mem::hhdm_offset();
	unsafe {
		((hhdm + first) as *mut u64).write_volatile(FIRST);
		((hhdm + second) as *mut u64).write_volatile(SECOND);
	}
	STAGE.store(0, Ordering::Release);
	paging::map_page(SHARED_VA, first, paging::WRITABLE);
	sched::spawn_on(1, reader, 0);

	// Let the reader cache a translation for the first frame.
	STAGE.store(1, Ordering::Release);
	let mut spins = 0u64;
	while STAGE.load(Ordering::Acquire) < 2 {
		core::hint::spin_loop();
		spins += 1;
		assert!(spins < 20_000_000_000, "the reader never took its first read");
	}

	// Remap the same address to the other frame and tell every other core to forget what it
	// knew. This is the ordering the real callers use: the tables are edited first, the
	// shootdown waits for acknowledgement, and only then is the old frame reusable.
	assert_eq!(paging::unmap_page(SHARED_VA), Some(first), "the address was mapped to the first frame");
	paging::map_page(SHARED_VA, second, paging::WRITABLE);
	crate::mem::tlb::shootdown();

	STAGE.store(3, Ordering::Release);
	let mut spins = 0u64;
	while STAGE.load(Ordering::Acquire) < 4 {
		core::hint::spin_loop();
		spins += 1;
		assert!(spins < 20_000_000_000, "the reader never took its second read");
	}

	assert_eq!(SAW_FIRST.load(Ordering::Acquire), FIRST, "the reader's first read should see the first frame");
	assert_eq!(SAW_SECOND.load(Ordering::Acquire), SECOND, "after the shootdown the reader must see the NEW frame, not the translation it had cached");

	assert_eq!(paging::unmap_page(SHARED_VA), Some(second));
	crate::mem::tlb::shootdown();
	unsafe {
		frame::deallocate(first);
		frame::deallocate(second);
	}
}

crate::tagged_test!(ring_three_cannot_write_the_gs_base, [Paging, Memory, Kernel], id = "kernel.arch.ring_three_cannot_write_the_gs_base", covers = ["kernel"]);
fn ring_three_cannot_write_the_gs_base() {
	// KERN-ARCH-024. The x86_64 syscall entry stub reaches the kernel stack pointer through `gs:`
	// WITHOUT `swapgs`, and the argument for that - in `usermode.rs` - is that "the user thread
	// keeps the kernel's GS base". That is true only while ring 3 cannot CHANGE the GS base, and
	// `WRGSBASE` is a ring-3 instruction whenever CR4.FSGSBASE is set.
	//
	// The kernel never touched the bit, so whether user code could redirect GS was the firmware's
	// decision. On a machine whose firmware left it on, user code could point GS at memory of its
	// own choosing and the next syscall would take its kernel stack pointer from there: a ring-0
	// stack pivot, on a path with no other check.
	//
	// It is cleared on every core now, and this is what says so. Asserted on the CONTROL REGISTER
	// rather than by executing `wrgsbase` from ring 3, because the failure being prevented is the
	// kernel following a user-chosen pointer - a test that reproduced it would not report anything.
	#[cfg(target_arch = "x86_64")]
	{
		assert!(!crate::arch::paging::user_can_write_gs_base(), "CR4.FSGSBASE is set, so ring 3 can redirect the GS base the syscall stub trusts");
	}
	// The other two backends reach per-CPU state through TPIDR_EL1 and the `sscratch`/`tp` pair,
	// neither of which ring 3 can write, so there is no equivalent bit to establish.
}

crate::tagged_test!(
	#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
	the_direct_map_has_no_writable_executable_alias,
	[Paging, Memory, Kernel],
	id = "kernel.arch.the_direct_map_has_no_writable_executable_alias",
	covers = ["kernel"]
);
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn the_direct_map_has_no_writable_executable_alias() {
	// KERN-ARCH-006. Both boot stubs map RAM with 1 GiB blocks that are writable AND executable,
	// and the kernel runs out of that map - so W^X, which this tree advertises, held on one target
	// of three: every page of RAM was executable through the direct map, and the kernel's own text
	// was writable through it.
	//
	// `harden_direct_map` splits those blocks at 2 MiB and gives each part the permissions its
	// contents want. This walks the LIVE tables afterwards and requires that no leaf is both - the
	// property itself, read out of the hardware's own descriptors rather than out of the code that
	// wrote them.
	if let Some(at) = crate::arch::paging::writable_executable_block() {
		panic!("the direct map is still writable and executable at {at:#x}");
	}
}

crate::tagged_test!(mapping_a_4_kb_page_over_a_large_leaf_is_refused, [Paging, Memory, Kernel], id = "kernel.arch.mapping_a_4_kb_page_over_a_large_leaf_is_refused", covers = ["kernel"]);
fn mapping_a_4_kb_page_over_a_large_leaf_is_refused() {
	// KERN-ARCH-021. Every 4 kB mapper walked its levels reading "valid" as "table" and following
	// the address field down. At the 1 GiB and 2 MiB levels a valid entry can just as well BE the
	// mapping, and then that address field names ordinary memory - so the walk wrote a page-table
	// entry into it and mapped the caller's page into a table that was never a table.
	//
	// The direct map is where every target keeps such leaves: 2 MiB pages from the loader on
	// x86_64, and 2 MiB blocks from `harden_direct_map` on the other two. This asks for one 4 kB
	// page at the direct-map address of physical 0 - covered by a large leaf on all three - and
	// requires a refusal rather than a walk into the memory that leaf describes.
	let over_a_large_leaf = crate::mem::hhdm_offset();
	let frame = crate::mem::frame::allocate().expect("a frame to try to map");
	let result = crate::arch::paging::try_map_page(over_a_large_leaf, frame, crate::arch::paging::WRITABLE | crate::arch::paging::NO_EXECUTE);
	unsafe { crate::mem::frame::deallocate(frame) };
	assert!(result.is_err(), "a 4 kB map at {over_a_large_leaf:#x} walked into a large leaf instead of refusing");
}
