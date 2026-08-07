use super::paging;
use crate::{mem, object::address_space::AddressSpace, sched, smp};

crate::tagged_test!(concurrent_maps_on_shared_tables_strand_nothing, [Paging, Memory, Stress]);
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

crate::tagged_test!(paging_map_unmap, [Paging, Memory]);
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
