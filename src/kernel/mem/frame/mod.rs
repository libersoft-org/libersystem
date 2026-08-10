// Physical frame allocator.
//
// Free physical memory is kept as a sorted table of contiguous runs (base +
// length), seeded straight from the usable regions of the loader's memory map. A
// single-frame alloc takes the head page of the first run (O(1)); a contiguous
// alloc first-fits a whole run - DMA buffers need physically contiguous spans
// (virtqueue rings, block data stages, jumbo frames) - and a free re-coalesces
// with its neighbors, so runs re-form as buffers are released.
//
// The run table has two lives. Before the heap exists (the frame allocator is
// what brings the heap up) it is a small fixed seed array holding the boot
// memory-map regions. Right after heap::init, mem::init upgrades it to a
// heap-backed Vec whose capacity is reserved ONCE, outside this lock, and never
// grown again.
//
// Never grown, because growing it here would deadlock. This comment used to say
// the opposite - that the kernel heap never allocates frames at runtime, so
// growing the Vec under the frame lock was safe - and it was wrong: `heap::grow`
// calls `frame::allocate` whenever a request does not fit its mapped window.
// Every allocation this file performs therefore happens with the lock RELEASED,
// and the rule has been broken twice since it was written, each time presenting
// as a core spinning forever rather than as anything that names this file.
//
// From `upgrade_to_heap` onward the run table is empty and the buddy allocator
// in `buddy.rs` answers everything; the table remains only as the fallback for a
// machine whose bitmap could not be reserved.
//
// A free is checked against the pool: a span overlapping an existing free run is
// a double free and is refused loudly, because honoring it would let the same
// frame be handed out twice later.
//
// The allocator is global and guarded by a SpinLock, so it is safe to call from
// any core.

#![allow(dead_code)]

use bootproto::MemRegion;

use alloc::vec::Vec;

use crate::sync::SpinLock;

mod buddy;
use buddy::Buddy;

// The table's floor, for a pool too small for the worst-case sizing below to matter. The real bound
// is computed from the pool - see `worst_case_runs`.
const MIN_RUNS: usize = 8192;

// How many disjoint free runs a pool of `pages` frames can possibly hold.
//
// Every other page free and the ones between them out on loan: that is the most fragmented a pool
// can be, and it is `pages / 2` runs, rounded up. There is no arrangement with more, because two
// free runs must have at least one allocated page between them.
//
// This is the number the run table has to be able to hold, and it used to be 8192 - "sized well past
// what a healthy pool fragments into", which is a statement about healthy pools rather than about
// what is possible. Past it, `insert_at` refused and the freed span was simply LOST: the machine got
// smaller, a warning went by, and nothing added it up until an allocation failed weeks later with no
// cause attached.
//
// Sizing for the worst case is what makes "a free cannot fail" true rather than usually true. It
// costs 16 bytes per two pages - about 0.2% of the pool, one megabyte on a 512 MB machine - reserved
// once at boot and never grown.
fn worst_case_runs(pages: usize) -> usize {
	pages.div_ceil(2).saturating_add(1).max(MIN_RUNS)
}

pub const PAGE_SIZE: u64 = 4096;

// The most disjoint free runs the pre-heap seed table holds: enough for one run
// per usable boot memory-map region with headroom. Once the table is heap-backed
// there is no bound beyond memory.
const SEED_RUNS: usize = 128;

// One contiguous run of free frames: `pages` pages starting at physical `base`.
#[derive(Clone, Copy)]
struct Run {
	base: u64,
	pages: u64,
}

struct FrameAllocator {
	// The fixed pre-heap table (boot seeding only) and its live length.
	//
	// It exists for one reason: the frame allocator is what brings the heap up, so the first frames
	// are handed out before there is anywhere to put a bitmap. A handful of runs is all boot needs.
	seed: [Run; SEED_RUNS],
	seed_len: usize,
	// The buddy allocator, built at `upgrade_to_heap` and the only allocator from then on. While
	// this is `Some`, the run table above is empty and unused - every operation delegates here.
	buddy: Option<Buddy>,
	// The heap-backed run table. Kept for the window between the heap coming up and the buddy
	// being built, and as the fallback if the buddy's metadata cannot be reserved.
	heap: Option<Vec<Run>>,
	free_count: usize,
	total_count: usize,
	// Debug-build ownership record: one bit per frame in [owned_base, owned_base + owned_frames),
	// set while the frame is out on loan. See `check_owned_free`.
	#[cfg(debug_assertions)]
	owned: Option<Vec<u64>>,
	#[cfg(debug_assertions)]
	owned_base: u64,
	#[cfg(debug_assertions)]
	owned_frames: u64,
}

impl FrameAllocator {
	const fn new() -> Self {
		Self {
			seed: [Run { base: 0, pages: 0 }; SEED_RUNS],
			seed_len: 0,
			buddy: None,
			heap: None,
			free_count: 0,
			total_count: 0,
			#[cfg(debug_assertions)]
			owned: None,
			#[cfg(debug_assertions)]
			owned_base: 0,
			#[cfg(debug_assertions)]
			owned_frames: 0,
		}
	}

	// Mark a span as out on loan. No-op until the bitmap exists (boot seeding runs before
	// the heap) and in release builds.
	#[cfg(debug_assertions)]
	fn mark_owned(&mut self, base: u64, pages: u64, owned: bool) {
		let (record_base, record_frames) = (self.owned_base, self.owned_frames);
		let Some(bits) = self.owned.as_mut() else { return };
		for page in 0..pages {
			let phys = base + page * PAGE_SIZE;
			if phys < record_base {
				continue;
			}
			let index = (phys - record_base) / PAGE_SIZE;
			if index >= record_frames {
				continue;
			}
			let (word, bit) = ((index / 64) as usize, index % 64);
			if owned {
				bits[word] |= 1 << bit;
			} else {
				bits[word] &= !(1 << bit);
			}
		}
	}

	#[cfg(not(debug_assertions))]
	fn mark_owned(&mut self, _base: u64, _pages: u64, _owned: bool) {}

	// Is every frame of this span currently out on loan from THIS allocator?
	//
	// The overlap test in `insert` already refuses a span that overlaps the free pool, which
	// catches the double free of a frame that is still free. It cannot catch the two cases
	// this does: a frame that was never handed out at all (an MMIO address, a bootloader
	// reservation, a number off the stack), which would quietly add non-RAM to the pool; and
	// a frame freed twice with an allocation of it in between, where the second free lands
	// while the frame is legitimately out on loan to someone else.
	//
	// Returns true when there is no record to consult - during boot seeding, and in release
	// builds - so this is a check that can only ever refuse a free it is certain about.
	#[cfg(debug_assertions)]
	fn check_owned_free(&self, base: u64, pages: u64) -> bool {
		let Some(bits) = self.owned.as_ref() else { return true };
		if base % PAGE_SIZE != 0 {
			crate::serial_println!("frame: WARNING: free refused - {base:#x} is not page-aligned");
			return false;
		}
		for page in 0..pages {
			let phys = base + page * PAGE_SIZE;
			let index = phys.checked_sub(self.owned_base).map(|offset| offset / PAGE_SIZE);
			let Some(index) = index.filter(|index| *index < self.owned_frames) else {
				crate::serial_println!("frame: WARNING: free refused - {phys:#x} is not a frame this allocator owns");
				return false;
			};
			if bits[(index / 64) as usize] & (1 << (index % 64)) == 0 {
				crate::serial_println!("frame: WARNING: free refused - {phys:#x} is not currently allocated");
				return false;
			}
		}
		true
	}

	#[cfg(not(debug_assertions))]
	fn check_owned_free(&self, _base: u64, _pages: u64) -> bool {
		true
	}

	// The mirror of `check_owned_free`, and the one the run table never needed: is this frame
	// already out on loan at the moment we are about to hand it out AGAIN?
	//
	// The run table could not hand the same frame out twice - a frame was in exactly one run and
	// leaving the run removed it. A bitmap allocator can, and the failure is silent: two owners
	// write the same page, and whoever reads it second finds the other's bytes. That surfaces
	// arbitrarily far away - as a corrupt ELF header, a filesystem that fails its own checksum, a
	// hang - and none of those point back here.
	//
	// So the record answers it directly, in debug builds, at the instant it happens.
	#[cfg(debug_assertions)]
	fn check_not_owned(&self, base: u64, pages: u64) -> bool {
		let Some(bits) = self.owned.as_ref() else { return true };
		for page in 0..pages {
			let phys = base + page * PAGE_SIZE;
			let Some(index) = phys.checked_sub(self.owned_base).map(|offset| offset / PAGE_SIZE).filter(|index| *index < self.owned_frames) else {
				continue;
			};
			if bits[(index / 64) as usize] & (1 << (index % 64)) != 0 {
				return false;
			}
		}
		true
	}

	#[cfg(not(debug_assertions))]
	fn check_not_owned(&self, _base: u64, _pages: u64) -> bool {
		true
	}

	// The current run table, whichever backing it lives in.
	fn runs(&self) -> &[Run] {
		match &self.heap {
			Some(v) => v,
			None => &self.seed[..self.seed_len],
		}
	}

	fn runs_mut(&mut self) -> &mut [Run] {
		match &mut self.heap {
			Some(v) => v,
			None => &mut self.seed[..self.seed_len],
		}
	}

	fn remove_at(&mut self, at: usize) {
		match &mut self.heap {
			Some(v) => {
				v.remove(at);
			}
			None => {
				self.seed.copy_within(at + 1..self.seed_len, at);
				self.seed_len -= 1;
			}
		}
	}

	// Insert a run at `at`, growing the heap-backed table as needed. False only
	// when the pre-heap seed table is full (boot seeding is a handful of runs, so
	// this does not happen in practice).
	fn insert_at(&mut self, at: usize, run: Run) -> bool {
		match &mut self.heap {
			Some(v) => {
				// Never grows: the capacity was reserved once, outside this lock, because growing
				// it here would call the global allocator, whose `grow` calls `frame::allocate` -
				// straight back into the lock this thread already holds.
				//
				// UNREACHABLE, and kept anyway. The capacity is `worst_case_runs(total)`, which is
				// more runs than a pool of that many pages can be split into, so a free cannot find
				// this table full. That is an argument, and an argument is not a guarantee: if the
				// sizing is ever wrong the alternative to this branch is writing past the end of a
				// `Vec`. Losing a span and saying so loudly is the safe way to be wrong.
				if v.len() == v.capacity() {
					crate::serial_println!("frame: run table full at {} runs, which the worst-case sizing says cannot happen - a freed span is being LOST", v.len());
					return false;
				}
				v.insert(at, run);
				true
			}
			None => {
				if self.seed_len == SEED_RUNS {
					return false;
				}
				self.seed.copy_within(at..self.seed_len, at + 1);
				self.seed[at] = run;
				self.seed_len += 1;
				true
			}
		}
	}

	// The index of the first run whose base is >= `base` (the insertion point).
	fn position(&self, base: u64) -> usize {
		self.runs().partition_point(|r| r.base < base)
	}

	// Return `pages` frames at `base` to the pool, coalescing with the runs on
	// either side. A span that overlaps an existing free run is a double free and
	// is refused loudly - accepting it would corrupt the pool (the overlapping
	// frames would be handed out twice).
	fn insert(&mut self, base: u64, pages: u64) {
		if pages == 0 {
			return;
		}
		if self.buddy.is_some() {
			// Refuse a span this allocator did not hand out, exactly as the run table does: the
			// ownership record is the check, and it is the same one either way.
			if !self.check_owned_free(base, pages) {
				note_refused_free();
				return;
			}
			self.mark_owned(base, pages, false);
			// A double free of a span that is still FREE is what the run table caught with its
			// overlap test. The buddy has no overlap test - it has a bitmap - so the same question
			// is asked directly, and asking it is what keeps a freed-twice page from being handed
			// to two owners.
			let already = (0..pages).any(|page| self.buddy.as_ref().is_some_and(|buddy| buddy.is_free_page(base + page * PAGE_SIZE)));
			if already {
				note_refused_free();
				crate::serial_println!("frame: WARNING: double free refused - {pages} page(s) at {base:#x} are already free");
				return;
			}
			let taken = self.buddy.as_mut().expect("checked").free_span(base, pages);
			self.free_count += taken as usize;
			if taken != pages {
				// The buddy could not frame all of it - the span reaches outside its extent. The
				// pages are gone: marked not-owned above and not free below. Counted and said,
				// because `free_count` claiming memory the bitmap does not hold is how a pool comes
				// to report free frames that no allocation can find.
				LOST_PAGES.fetch_add(pages - taken, core::sync::atomic::Ordering::AcqRel);
				crate::serial_println!("frame: WARNING: {} of {pages} page(s) at {base:#x} fell outside the buddy's extent and are LOST", pages - taken);
			}
			return;
		}
		// Refuse a span this allocator did not hand out before touching the run table, so a
		// bad address cannot become free memory. Boot seeding passes this trivially - the
		// record does not exist yet.
		if !self.check_owned_free(base, pages) {
			note_refused_free();
			return;
		}
		self.mark_owned(base, pages, false);
		let at = self.position(base);
		let end = base + pages * PAGE_SIZE;
		let len = self.runs().len();
		let overlaps_right = at < len && end > self.runs()[at].base;
		let overlaps_left = at > 0 && {
			let left = self.runs()[at - 1];
			left.base + left.pages * PAGE_SIZE > base
		};
		if overlaps_right || overlaps_left {
			crate::serial_println!("frame: WARNING: double free refused - {} page(s) at {:#x} overlap the free pool", pages, base);
			note_refused_free();
			return;
		}
		let left_adjacent = at > 0 && {
			let left = self.runs()[at - 1];
			left.base + left.pages * PAGE_SIZE == base
		};
		let right_adjacent = at < len && end == self.runs()[at].base;
		match (left_adjacent, right_adjacent) {
			// The freed span closes the gap between two runs: fold all three together.
			(true, true) => {
				let right_pages = self.runs()[at].pages;
				self.runs_mut()[at - 1].pages += pages + right_pages;
				self.remove_at(at);
			}
			(true, false) => self.runs_mut()[at - 1].pages += pages,
			(false, true) => {
				let run = &mut self.runs_mut()[at];
				run.base = base;
				run.pages += pages;
			}
			(false, false) => {
				if !self.insert_at(at, Run { base, pages }) {
					// THE loss. Not a refusal - this span was this allocator's, it is being handed
					// back correctly, and there is nowhere to record it. The frames are gone and
					// nothing references them.
					note_lost_pages(pages);
					crate::serial_println!("frame: WARNING: pre-heap free-run table full, LOSING {} page(s) at {:#x} ({} lost so far)", pages, base, lost_pages());
					return;
				}
			}
		}
		self.free_count += pages as usize;
	}

	// Take one page off the head of the first run.
	fn take_one(&mut self) -> Option<u64> {
		if let Some(buddy) = self.buddy.as_mut() {
			// The injection frees the block straight back, so the SAME address comes out of the
			// next `alloc` while this one is still being handed to a caller. That is precisely the
			// state a mis-set bit produces, reached without a mis-set bit.
			//
			// The credit below is not bookkeeping tidiness. Without it the injected free leaves the
			// buddy holding one more page than `free_count` believes, forever - and the test that
			// exhausts the pool then subtracts past zero, several tests later, in a place that has
			// nothing to do with this. An injection that leaves the system inconsistent tests the
			// inconsistency instead of the thing it was aimed at.
			let base = buddy.alloc(0)?;
			let duplicated = injected_duplicate();
			if duplicated {
				buddy.free(base, 0);
			}
			if duplicated {
				self.free_count += 1;
			}
			if !self.check_not_owned(base, 1) {
				crate::serial_println!("frame: DOUBLE ALLOCATION - the buddy handed out {base:#x}, which is already on loan");
				DOUBLE_ALLOCATIONS.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
			}
			self.free_count -= 1;
			self.mark_owned(base, 1, true);
			return Some(base);
		}
		if self.runs().is_empty() {
			return None;
		}
		let base = {
			let run = &mut self.runs_mut()[0];
			let base = run.base;
			run.base += PAGE_SIZE;
			run.pages -= 1;
			base
		};
		if self.runs()[0].pages == 0 {
			self.remove_at(0);
		}
		self.free_count -= 1;
		self.mark_owned(base, 1, true);
		Some(base)
	}

	// First-fit a physically contiguous span of `pages`, taking it off the head
	// of the first run large enough.
	fn take_contiguous(&mut self, pages: u64) -> Option<u64> {
		if let Some(buddy) = self.buddy.as_mut() {
			let base = buddy.alloc_contiguous(pages)?;
			if !self.check_not_owned(base, pages) {
				crate::serial_println!("frame: DOUBLE ALLOCATION - the buddy handed out {pages} page(s) at {base:#x}, part of which is already on loan");
				DOUBLE_ALLOCATIONS.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
			}
			self.free_count -= pages as usize;
			self.mark_owned(base, pages, true);
			return Some(base);
		}
		for at in 0..self.runs().len() {
			if self.runs()[at].pages >= pages {
				let base = {
					let run = &mut self.runs_mut()[at];
					let base = run.base;
					run.base += pages * PAGE_SIZE;
					run.pages -= pages;
					base
				};
				if self.runs()[at].pages == 0 {
					self.remove_at(at);
				}
				self.free_count -= pages as usize;
				self.mark_owned(base, pages, true);
				return Some(base);
			}
		}
		None
	}
}

static ALLOCATOR: SpinLock<FrameAllocator> = SpinLock::new(FrameAllocator::new());

// Populate the run table from the usable regions of the loader's memory map.
// Physical frame 0 is never handed out (0 doubles as "no frame" in several
// interfaces), so a region starting there is trimmed by one page.
pub fn init(regions: &[MemRegion]) {
	let mut allocator = ALLOCATOR.lock();
	for region in regions {
		if region.kind != bootproto::MEM_USABLE {
			continue;
		}
		let mut base = align_up(region.base, PAGE_SIZE);
		let end = region.base + region.length;
		if base == 0 {
			base = PAGE_SIZE;
		}
		if base + PAGE_SIZE <= end {
			let pages = (end - base) / PAGE_SIZE;
			allocator.insert(base, pages);
		}
	}
	// Everything inserted so far is the machine's usable frame pool: fix the total
	// here so `totals` can report used = total - free for the rest of the run.
	allocator.total_count = allocator.free_count;
	// And fix the pool's ADDRESS extent here too, while the free runs still are the whole
	// pool. After the first allocation they are not, so this is the only moment the span
	// the ownership record has to cover can be read off the table.
	#[cfg(debug_assertions)]
	{
		let extent = {
			let runs = allocator.runs();
			runs.first().zip(runs.last()).map(|(first, last)| (first.base, last.base + last.pages * PAGE_SIZE))
		};
		if let Some((low, high)) = extent {
			allocator.owned_base = low;
			allocator.owned_frames = (high - low) / PAGE_SIZE;
		}
	}
}

// Move the run table onto the heap. Called by mem::init right after heap::init:
// the boot seeding above runs before the heap exists, but from here on the table
// grows with fragmentation instead of leaking past a fixed size. Growing the Vec
// while the frame lock is held cannot re-enter this allocator - the kernel heap
// never allocates frames at runtime (its window is mapped once at boot).
pub fn upgrade_to_heap() {
	// THE BUDDY FIRST, and the run table only if the buddy cannot be built.
	//
	// It used to be the other way round: reserve a worst-case run table, and if that reservation
	// failed, print a line and return - before the buddy's metadata had been sized at all. The two
	// are not the same size. On a 4 GiB machine the run table is `worst_case_runs` entries, about
	// 8 MiB, and the buddy's bitmap is 383 KiB; so the one machine where the run table cannot be
	// reserved is a machine where the buddy easily could be, and it was the one machine that never
	// got one. It came up on the 128-run seed table instead - where a free CAN be silently lost,
	// which is the defect this whole milestone exists to remove.
	//
	// Reserving the buddy first also means the run table is never allocated on the path that
	// succeeds, so the 8 MiB is not merely emptied afterwards, it is never taken.
	//
	// Every allocation here happens with the lock RELEASED. Reserving calls the global allocator,
	// whose `grow` calls `frame::allocate`, which takes the lock this thread would be holding; on a
	// SpinLock that is a core spinning forever. The rule has been broken twice.
	//
	// The extent can only SHRINK while the metadata is being reserved - reserving takes frames, it
	// never adds them - so a buddy sized from this read still covers the pool that is seeded into it
	// under the lock below.
	let (extent, total, owned_words) = {
		let allocator = ALLOCATOR.lock();
		let runs = allocator.runs();
		let extent = runs.first().zip(runs.last()).map(|(first, last)| (first.base, (last.base + last.pages * PAGE_SIZE - first.base) / PAGE_SIZE));
		#[cfg(debug_assertions)]
		let words = (allocator.owned_frames as usize).div_ceil(64);
		#[cfg(not(debug_assertions))]
		let words = 0usize;
		(extent, allocator.total_count, words)
	};
	let buddy = extent.and_then(|(base, pages)| Buddy::new(base, pages));

	// The run table, reserved ONLY as the fallback. Sized for the worst case the pool can reach
	// rather than for what a healthy one looks like - see `worst_case_runs` - because that is what
	// makes a free unable to fail on the path that has to use it.
	let mut runs: Option<Vec<Run>> = None;
	if buddy.is_none() {
		let wanted = worst_case_runs(total);
		let mut table = Vec::new();
		if table.try_reserve_exact(wanted).is_err() {
			// NEITHER allocator can be built, so the invariant this milestone is named for cannot be
			// held: on the 128-run seed table a free that does not fit is DROPPED, the machine gets
			// quietly smaller, and the first sign is an allocation failure weeks later with no cause
			// attached.
			//
			// A kernel that knows it cannot keep that promise must not keep going as though it
			// could. Printing a line and running anyway was the old answer, and a warning at boot on
			// a machine nobody is watching is not an answer - it is the defect with a footnote.
			//
			// Reachable only on a machine too short of memory to reserve a few hundred kilobytes at
			// boot, which is a machine that is not going to run this system anyway. Stopping here
			// says so, in one place, instead of somewhere unrelated in an hour.
			panic!("frame: neither the buddy allocator's metadata nor a worst-case run table of {wanted} runs could be reserved; this kernel cannot guarantee that a free is never lost, and will not run pretending it can");
		}
		crate::serial_println!("frame: could not size the buddy allocator's metadata; falling back to a worst-case run table of {wanted} runs");
		runs = Some(table);
	}

	// The debug ownership record: one bit per frame of the pool, 128 KiB on a 4 GiB machine,
	// in debug builds only. `owned_frames` was fixed at init and does not change, so sizing
	// this outside the lock is sound.
	#[cfg(debug_assertions)]
	let bits = {
		let mut bits: Vec<u64> = Vec::new();
		if bits.try_reserve_exact(owned_words).is_err() {
			crate::serial_println!("frame: could not reserve the ownership record; frees will not be checked against it");
			Vec::new()
		} else {
			// Start from "every frame is out on loan" and clear what is free below. That is
			// exactly right: whatever is not free at this moment was handed out before the
			// record existed - the heap window, the boot page tables - and freeing it later
			// is legitimate, so it has to read as owned.
			bits.resize(owned_words, u64::MAX);
			bits
		}
	};
	#[cfg(not(debug_assertions))]
	let _ = owned_words;

	let mut allocator = ALLOCATOR.lock();
	if allocator.heap.is_some() || allocator.buddy.is_some() {
		return;
	}
	if let Some(mut table) = runs {
		table.extend_from_slice(&allocator.seed[..allocator.seed_len]);
		allocator.heap = Some(table);
	}
	#[cfg(debug_assertions)]
	if !bits.is_empty() {
		allocator.owned = Some(bits);
		for index in 0..allocator.runs().len() {
			let run = allocator.runs()[index];
			allocator.mark_owned(run.base, run.pages, false);
		}
	}

	// Move the pool into the buddy and empty the seed table behind it. From here every allocation
	// and every free goes through the bitmap, and a free cannot be lost because there is no table
	// to fill - which is the property P02M0120 exists for.
	if let Some(mut buddy) = buddy {
		// Seeded from the table as it stands NOW, under the lock that installs the result, with
		// nothing allocating in between. `free_span` only writes bits that are already reserved,
		// so this whole block is allocation-free - which is the property that makes it safe to do
		// here rather than outside.
		//
		// A run that falls outside the extent cannot happen (the extent came from this same table
		// and frames only leave it), but `free` refuses one rather than trusting the argument, and
		// a refusal shows up as a shortfall in the count below.
		let mut offered = 0u64;
		let mut seeded = 0u64;
		for index in 0..allocator.runs().len() {
			let run = allocator.runs()[index];
			seeded += buddy.free_span(run.base, run.pages);
			offered += run.pages;
		}
		if seeded != offered {
			crate::serial_println!("frame: the buddy took {seeded} of {offered} free frames; the rest fell outside its extent and are lost");
		}
		let free = buddy.free_pages();
		let metadata = buddy.metadata_bytes();
		allocator.buddy = Some(buddy);
		allocator.free_count = free as usize;
		allocator.seed_len = 0;
		crate::serial_println!("memory: buddy allocator up - {free} free frames, {} KiB of metadata", metadata / 1024);
	}
}

// How many runs the live table can hold, and whether it is the heap-backed one. For the test that
// pins the worst-case sizing; nothing else needs to know.
#[cfg(test)]
pub fn run_capacity() -> usize {
	let allocator = ALLOCATOR.lock();
	match &allocator.heap {
		Some(v) => v.capacity(),
		None => SEED_RUNS,
	}
}

#[cfg(test)]
pub fn on_heap() -> bool {
	ALLOCATOR.lock().heap.is_some()
}

// Is the buddy allocator the one serving allocations? For the test that says so; nothing else
// needs to know, because every caller goes through the same four functions either way.
#[cfg(test)]
pub fn on_buddy() -> bool {
	ALLOCATOR.lock().buddy.is_some()
}

// Bytes of buddy metadata, or 0 when the run table is still in charge.
#[cfg(test)]
pub fn buddy_metadata_bytes() -> usize {
	ALLOCATOR.lock().buddy.as_ref().map(|buddy| buddy.metadata_bytes()).unwrap_or(0)
}

// The frame pool's totals: (total usable frames fixed at init, frames currently free).
pub fn totals() -> (usize, usize) {
	let allocator = ALLOCATOR.lock();
	(allocator.total_count, allocator.free_count)
}

// The number of frames currently free.
pub fn free_count() -> usize {
	ALLOCATOR.lock().free_count
}

// How many frees this allocator has REFUSED, and how many pages it has LOST.
//
// The two are different facts and were one, half of it compiled out. `refused_frees` counted a free
// this allocator DECLINES - an address it never handed out, a span that overlaps the free pool -
// which is a caller's bug and costs nothing: the frames were never the allocator's to lose. It was
// `#[cfg(test)]` because a test needed it, and that was the whole reason it existed.
//
// `lost_pages` counts the other thing, and it is the one this kernel actually has to know: a
// perfectly good free that could not be RECORDED. The run table is bounded on purpose (it was
// bounded to fix a deadlock) and `insert` says outright that a span which does not fit is not being
// tracked; the quarantine drops a page whose shootdown cannot complete. Both paths print a warning
// and move on, so under fragmentation the machine gets slowly smaller and nothing adds it up. The
// symptom arrives weeks later as an allocation failure with no cause attached.
//
// So: counted always, not only under test, because a number nobody can read in production is not a
// measurement. This is the cheap half of P02M0120 and the thing that says whether the buddy allocator
// below it actually fixed anything - measure first, and be willing to record that it did not pay.
pub fn refused_frees() -> u64 {
	REFUSED_FREES.load(core::sync::atomic::Ordering::Acquire)
}

// Pages this allocator was handed back and could not put anywhere. Memory the machine no longer
// has, with nothing holding it.
pub fn lost_pages() -> u64 {
	LOST_PAGES.load(core::sync::atomic::Ordering::Acquire)
}

static REFUSED_FREES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static LOST_PAGES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
// Frames handed out while already on loan. Zero is the only acceptable value and a test says so.
static DOUBLE_ALLOCATIONS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// How many times a frame was handed out to a second owner while the first still had it. The
// allocator cannot repair this - by the time it is noticed both owners hold the address - so the
// counter exists to make it VISIBLE rather than to recover, and the test that reads it is the only
// thing between a bitmap bug and a corruption that surfaces somewhere else entirely.
pub fn double_allocations() -> u64 {
	DOUBLE_ALLOCATIONS.load(core::sync::atomic::Ordering::Acquire)
}

fn note_refused_free() {
	REFUSED_FREES.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
}

// A valid free that could not be tracked. `pages` is what the machine just lost.
fn note_lost_pages(pages: u64) {
	LOST_PAGES.fetch_add(pages, core::sync::atomic::Ordering::AcqRel);
}

// Deterministic out-of-frames injection, for tests only.
//
// Draining the pool to test an OOM path works and tests the wrong thing: it fails the FIRST
// allocation, so every rollback that matters - the second page of a three-page map, the
// intermediate table three levels down, the segment after the one that succeeded - is never
// reached. What those paths need is a failure at a CHOSEN allocation with the pool otherwise
// healthy, and that is what this does.
//
// Armed with a count: that many allocations succeed, and every one after returns None until
// it is disarmed. `#[cfg(test)]` because a kernel that can be told to fail allocations is not
// a kernel to ship.
#[cfg(test)]
mod inject {
	use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

	// Whether any test has ever armed this. Checked FIRST, and the reason is boot: the frame
	// allocator runs long before the per-CPU blocks exist, and asking which core we are on
	// down there dereferences a null pointer. Nothing arms this until a test does, by which
	// time every core is up, so the boot path pays one relaxed load and never asks.
	static EVER_ARMED: AtomicBool = AtomicBool::new(false);

	// Per CPU, not global. A global switch would fail allocations on every other core for as
	// long as it was armed, and the other cores are running the rest of the system - so a test
	// arming one would be injecting faults into the scheduler, the drivers and whatever else
	// happened to allocate in that window. Armed on the core that arms it and nowhere else.
	//
	// A test thread migrating between arming and allocating would run un-injected. That shows
	// up as "the failure never happened", which every test here asserts against, so it is a
	// loud flake rather than a quiet pass.
	#[allow(clippy::declare_interior_mutable_const)]
	const DISARMED: AtomicUsize = AtomicUsize::new(usize::MAX);
	static BUDGET: [AtomicUsize; crate::mem::tlb::MAX_CPUS] = [DISARMED; crate::mem::tlb::MAX_CPUS];

	fn me() -> usize {
		(crate::arch::percpu::this_cpu().cpu_id() as usize).min(crate::mem::tlb::MAX_CPUS - 1)
	}

	// Let `successes` more allocations through on THIS core, then fail every one until
	// `disarm`.
	pub fn fail_after(successes: usize) {
		BUDGET[me()].store(successes, Ordering::Release);
		EVER_ARMED.store(true, Ordering::Release);
	}

	pub fn disarm() {
		BUDGET[me()].store(usize::MAX, Ordering::Release);
	}

	// Make the NEXT allocation on this core hand out a frame that is already on loan.
	//
	// A bitmap allocator's worst failure is silent: nothing refuses, nothing warns, and two owners
	// write the same page until one of them reads the other's bytes somewhere else entirely. The
	// detector in `take_one` exists for exactly that, and a detector nothing ever triggers is a
	// detector nobody knows is wired up - so this triggers it, deliberately, from one test.
	//
	// Per core and one-shot, for the same reason `fail_after` is: a global switch would be handing
	// duplicate frames to the scheduler and the drivers for as long as it was armed.
	#[allow(clippy::declare_interior_mutable_const)]
	const NOT_DUPLICATING: AtomicBool = AtomicBool::new(false);
	static DUPLICATE: [AtomicBool; crate::mem::tlb::MAX_CPUS] = [NOT_DUPLICATING; crate::mem::tlb::MAX_CPUS];

	pub fn duplicate_next() {
		DUPLICATE[me()].store(true, Ordering::Release);
		EVER_ARMED.store(true, Ordering::Release);
	}

	// True once, for the core that armed it.
	pub(super) fn should_duplicate() -> bool {
		if !EVER_ARMED.load(Ordering::Acquire) {
			return false;
		}
		DUPLICATE[me()].swap(false, Ordering::AcqRel)
	}

	// True when this allocation is one to refuse. `usize::MAX` is the disarmed value and is
	// never decremented, so an un-armed core pays one relaxed load and nothing else.
	pub(super) fn should_fail() -> bool {
		if !EVER_ARMED.load(Ordering::Acquire) {
			return false;
		}
		let budget = &BUDGET[me()];
		budget
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| match remaining {
				usize::MAX => None,
				0 => None,
				n => Some(n - 1),
			})
			.is_err_and(|remaining| remaining == 0)
	}
}

#[cfg(test)]
pub use inject::{disarm as stop_failing_allocations, duplicate_next as duplicate_next_allocation, fail_after as fail_allocations_after};

#[cfg(not(test))]
fn injected_duplicate() -> bool {
	false
}

#[cfg(test)]
fn injected_duplicate() -> bool {
	inject::should_duplicate()
}

#[cfg(not(test))]
fn injected_failure() -> bool {
	false
}

#[cfg(test)]
fn injected_failure() -> bool {
	inject::should_fail()
}

// Allocate one physical frame, returning its physical address.
pub fn allocate() -> Option<u64> {
	if injected_failure() {
		return None;
	}
	ALLOCATOR.lock().take_one()
}

// Allocate `pages` physically CONTIGUOUS frames, returning the base address of
// the span - the allocation DMA buffers ride, so a device sees one run. None if
// no free run is large enough.
pub fn allocate_contiguous(pages: usize) -> Option<u64> {
	if pages == 0 {
		return None;
	}
	if injected_failure() {
		return None;
	}
	ALLOCATOR.lock().take_contiguous(pages as u64)
}

// Return a physical frame to the pool (re-coalescing with its neighbors, so
// contiguous runs re-form as buffers are released).
//
// It CANNOT LOSE THE FRAME. The run table is reserved at boot for the most fragmented the pool can
// ever be, so there is always room to record the span; a free that arrives is a free that is
// tracked. That property is what `lost_pages()` counts and what
// `a_fragmenting_workload_loses_no_pages_and_says_so` asserts.
//
// It can still REFUSE - a double free, or an address this allocator never handed out - and that is
// a different thing entirely: those frames were never the allocator's to lose, and refusing them is
// the point. `refused_frees()` counts those separately for exactly that reason.
//
// This is the raw, un-owned form of a free: a bare integer with no proof attached that the
// caller has the right to give it back. Getting it wrong does not go wrong here - it goes
// wrong later, when the frame is handed out a second time and two owners write through the
// same memory - which is exactly the shape `unsafe` exists to mark. In debug builds the
// allocator checks the claim against its ownership record and refuses a free it can prove
// is not the caller's; in release builds nothing checks it, and this signature is the only
// thing standing between a wrong address and the free pool.
//
// # Safety
//
// `phys` must be a frame previously obtained from `allocate` (or a page of an
// `allocate_contiguous` span), still owned by the caller, freed exactly once, and no longer
// mapped anywhere it could be written through.
pub unsafe fn deallocate(phys: u64) {
	ALLOCATOR.lock().insert(phys, 1);
}

// The number of whole pages needed to hold `bytes` (at least one).
pub fn pages_for(bytes: usize) -> usize {
	bytes.div_ceil(PAGE_SIZE as usize).max(1)
}

// Allocate `pages` physical frames, returning their addresses, or None if not
// enough are available (any frames already taken are returned on failure). The
// shared multi-frame allocation the frame-backed kernel objects use. The frames
// need not be contiguous (they are mapped page by page), but they are returned
// in ascending physical order so adjacent frames stay adjacent virtually (and a
// device fed the layout can coalesce them into runs).
pub fn allocate_pages(pages: usize) -> Option<Vec<u64>> {
	// FALLIBLE. `Vec::with_capacity` answers a request the machine cannot meet by aborting, and
	// `pages` comes from a syscall argument by way of `pages_for` - so the one place a caller's
	// number reached an infallible allocation was the metadata for the frames, before a single
	// frame had been taken.
	let mut frames: Vec<u64> = Vec::new();
	if frames.try_reserve_exact(pages).is_err() {
		return None;
	}
	for _ in 0..pages {
		match allocate() {
			Some(phys) => frames.push(phys),
			None => {
				// SAFETY: every frame in `frames` came from `allocate` above, is owned by
				// this call and has not been handed anywhere else.
				unsafe { free_pages(&frames) };
				return None;
			}
		}
	}
	frames.sort_unstable();
	Some(frames)
}

// Return a set of frames to the pool.
//
// # Safety
//
// Every address in `frames` must satisfy `deallocate`'s contract.
// Return frames that were MAPPED, once every core is known to have dropped its translation.
//
// The one door back to the allocator for anything a page table ever pointed at. It was not one
// door: the loader's rollback and process teardown did a shootdown and then freed, while
// `MemoryObject::drop`, `DmaBuffer::drop` and the kernel-stack teardown freed straight away. A
// frame handed out again while another core still has a stale translation is a physical
// use-after-free, and it does not need a hostile process to arrive - it needs two cores and a
// mapping that outlived its unmap by a few microseconds.
//
// A shootdown that could not be completed does NOT free. The span goes into quarantine and the next
// successful shootdown takes it out. Holding memory is a cost; handing out memory somebody else can
// still read is not a cost, it is a defect.
//
// SAFETY: the caller owns every frame in `frames` and has removed every mapping of them.
pub unsafe fn retire(frames: &[u64]) {
	if frames.is_empty() {
		return;
	}
	// What the queue could not take, to be dealt with the expensive way rather than lost.
	let mut unqueued: [u64; 8] = [0; 8];
	let mut unqueued_len = 0usize;
	let drain = {
		let mut quarantine = QUARANTINE.lock();
		for &phys in frames {
			// The queue is a heap allocation, and the heap grows by taking FRAMES - so the one
			// moment it cannot grow is when frames are short, which is exactly when this path runs.
			// The first version answered that by dropping the page with a warning, and a test that
			// refuses allocations found it at once: nine pages lost on one failed load, and the
			// quarantine empty, so it was not even a deferral.
			//
			// Nothing is lost now. A page that cannot be queued is retired the expensive way below -
			// one shootdown for the batch of them - which is slow and correct, and slow only when
			// the machine is already out of memory.
			if quarantine.try_reserve(1).is_err() {
				if unqueued_len < unqueued.len() {
					unqueued[unqueued_len] = phys;
					unqueued_len += 1;
				} else {
					// More than the small batch below can hold, with a heap that will not grow.
					// Freed after its own shootdown rather than queued or dropped.
					drop(quarantine);
					if crate::mem::tlb::shootdown() {
						unsafe { deallocate(phys) };
					} else {
						note_lost_pages(1);
						crate::serial_println!("frame: WARNING: {phys:#x} could not be queued and its shootdown did not complete; the page is not being tracked ({} lost so far)", lost_pages());
					}
					quarantine = QUARANTINE.lock();
				}
				continue;
			}
			quarantine.push(phys);
		}
		quarantine.len() >= QUARANTINE_DRAIN
	};
	if unqueued_len > 0 {
		// One shootdown for all of them, and it also covers whatever is queued - a flush is not
		// per-address - so the drain below finds the queue already retired.
		if crate::mem::tlb::shootdown() {
			for &phys in unqueued.iter().take(unqueued_len) {
				unsafe { deallocate(phys) };
			}
			unsafe { drain_quarantine() };
			return;
		}
		note_lost_pages(unqueued_len as u64);
		crate::serial_println!("frame: WARNING: {unqueued_len} page(s) could not be queued and their shootdown did not complete; they are not being tracked ({} lost so far)", lost_pages());
	}
	if drain {
		unsafe { drain_quarantine() };
	}
}

// Shoot down once, and free everything the shootdown covered.
//
// BATCHED, and that is not an optimisation. The first version did a shootdown per retired span, and
// a shootdown is an IPI to every other core and a spin until they all answer - which on an emulated
// guest is frequently a core that does not answer at all, because `service_pending` runs from the
// wake-IPI handler and the idle loop and a core grinding through a long computation reaches neither.
// Every `MemoryObject` and `DmaBuffer` and kernel stack going away then paid a full timeout, and the
// riscv64 suite went from finishing to not finishing.
//
// A flush is not per-address: one completed shootdown retires every translation every core holds, so
// one covers the whole quarantine however it was filled. The cost is amortised over
// `QUARANTINE_DRAIN` spans and the invariant is unchanged - nothing is freed without a completed
// shootdown that covers it.
//
// SAFETY: every frame in the quarantine was owned by whoever retired it and had its mappings
// removed before it got here.
pub unsafe fn drain_quarantine() {
	if QUARANTINE.lock().is_empty() {
		return;
	}
	if !crate::mem::tlb::shootdown() {
		// A core did not answer. The spans stay where they are and the next drain tries again;
		// freeing them now is the physical use-after-free the whole mechanism exists to prevent.
		return;
	}
	let held = core::mem::take(&mut *QUARANTINE.lock());
	for phys in held {
		unsafe { deallocate(phys) };
	}
}

// How many frames may wait before a drain is worth its shootdown.
//
// Low enough that the quarantine is a queue rather than a leak, high enough that a service churning
// buffers does not IPI every core for each one. The drain is also forced at the points where a
// caller knows a lot has just been freed - process teardown and loader rollback.
const QUARANTINE_DRAIN: usize = 64;

// Frames whose shootdown did not complete. Drained by the next one that does.
static QUARANTINE: crate::sync::SpinLock<Vec<u64>> = crate::sync::SpinLock::new(Vec::new());

// Compare the two views of the pool: every page the buddy calls free must be a page the ownership
// record calls not-owned, and the other way round.
//
// They are built from the same run list and maintained by the same two functions, so a disagreement
// is not a caller misbehaving - it is this file handing one page to two owners, or losing one to
// nobody. Neither shows up where it happens. A bitmap allocator's worst failure is that both views
// stay plausible while they drift apart, and the only cheap moment to notice is before anything has
// been built on top of the drift.
//
// Returns the number of pages they disagree about and the first such address, or None when there is
// nothing to compare - a release build, or a machine still on the run table.
//
// O(pages): 130k iterations on a 512 MB machine, a few milliseconds. Cheap enough for the test
// runner to call after every test, which is what turns "something diverged during this boot" into
// "this test diverged it".
#[cfg(debug_assertions)]
pub fn audit() -> Option<(u64, u64)> {
	let allocator = ALLOCATOR.lock();
	let buddy = allocator.buddy.as_ref()?;
	let bits = allocator.owned.as_ref()?;
	let (record_base, record_frames) = (allocator.owned_base, allocator.owned_frames);
	// Counts first, and the walk only when they differ. The record's bits are a popcount over
	// `record_frames / 64` words - 16k words on a 4 GiB machine - against a walk that asks
	// `is_free_page` a million times and costs a second and a half per test. Every divergence this
	// is looking for moves a page from one side to the other, so it moves the count too.
	// A POPCOUNT over words, which is what the comment above always claimed and the first version
	// did not do: it tested one bit at a time, 131,072 times on a 512 MB machine, with the allocator
	// LOCK HELD - and this runs after every test. A core spinning on that lock cannot answer a TLB
	// shootdown, so the cost did not stay inside the audit.
	//
	// The tail word is MASKED, and the first version of this was not - which cost the audit its
	// fast path on every call. The record is created with `resize(words, u64::MAX)`, so every bit
	// starts set and only real frames are ever cleared: the padding above `record_frames` stays 1
	// forever. Counting it made the two views disagree every time, which sent the audit down the
	// per-page walk it exists to avoid, on every test.
	let words = (record_frames as usize).div_ceil(64);
	let mut owned_pages: u64 = 0;
	for (index, word) in bits.iter().take(words).enumerate() {
		let value = if index + 1 == words && record_frames % 64 != 0 { word & ((1u64 << (record_frames % 64)) - 1) } else { *word };
		owned_pages += value.count_ones() as u64;
	}
	let free_by_record = record_frames - owned_pages;
	if free_by_record == buddy.free_pages() {
		return None;
	}
	// They disagree. Now find WHERE, because the count alone names nothing.
	let mut wrong = 0u64;
	let mut first = 0u64;
	for index in 0..record_frames {
		let phys = record_base + index * PAGE_SIZE;
		let owned = bits[(index / 64) as usize] & (1 << (index % 64)) != 0;
		if buddy.is_free_page(phys) == owned {
			if wrong == 0 {
				first = phys;
			}
			wrong += 1;
		}
	}
	(wrong > 0).then_some((wrong.max(1), first))
}

#[cfg(not(debug_assertions))]
pub fn audit() -> Option<(u64, u64)> {
	None
}

// How many frames are waiting on a shootdown that did not complete, for tests.
#[cfg(test)]
pub fn quarantined() -> usize {
	QUARANTINE.lock().len()
}

// Drain until the quarantine is empty, or give up after `attempts`.
//
// What "wait for the shootdown" looks like from a test. One drain is not enough: a shootdown gives
// up on a core that did not answer, and under a loaded host - three emulated guests on one
// machine - that happens often enough to fail a test measuring frames that ARE coming back, just
// not yet. Retrying is not hiding anything: the property under test is that the frames return, and
// this is how a test waits for them.
#[cfg(test)]
pub fn drain_quarantine_fully(attempts: usize) -> bool {
	for _ in 0..attempts {
		if QUARANTINE.lock().is_empty() {
			return true;
		}
		// SAFETY: every frame in the quarantine was owned by whoever retired it and had its
		// mappings removed before it got there.
		unsafe { drain_quarantine() };
	}
	QUARANTINE.lock().is_empty()
}

pub unsafe fn free_pages(frames: &[u64]) {
	for &phys in frames {
		unsafe { deallocate(phys) };
	}
}

const fn align_up(value: u64, align: u64) -> u64 {
	(value + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests;
