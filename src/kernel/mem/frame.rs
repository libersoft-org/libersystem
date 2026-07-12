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
// heap-backed Vec, so fragmentation is bounded by memory rather than by a
// compile-time table. Growth is safe in every context the allocator runs in:
// the exception paths (demand-paged stack growth) only ever allocate, which
// never grows the table, and the kernel heap never allocates frames at runtime
// (its window is mapped once at boot), so growing the Vec under the frame lock
// cannot re-enter this allocator.
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
	seed: [Run; SEED_RUNS],
	seed_len: usize,
	// The heap-backed table that replaces the seed once the heap is up.
	heap: Option<Vec<Run>>,
	free_count: usize,
	total_count: usize,
}

impl FrameAllocator {
	const fn new() -> Self {
		Self { seed: [Run { base: 0, pages: 0 }; SEED_RUNS], seed_len: 0, heap: None, free_count: 0, total_count: 0 }
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
					crate::serial_println!("frame: WARNING: pre-heap free-run table full, leaking {} page(s) at {:#x}", pages, base);
					return;
				}
			}
		}
		self.free_count += pages as usize;
	}

	// Take one page off the head of the first run.
	fn take_one(&mut self) -> Option<u64> {
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
		Some(base)
	}

	// First-fit a physically contiguous span of `pages`, taking it off the head
	// of the first run large enough.
	fn take_contiguous(&mut self, pages: u64) -> Option<u64> {
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
}

// Move the run table onto the heap. Called by mem::init right after heap::init:
// the boot seeding above runs before the heap exists, but from here on the table
// grows with fragmentation instead of leaking past a fixed size. Growing the Vec
// while the frame lock is held cannot re-enter this allocator - the kernel heap
// never allocates frames at runtime (its window is mapped once at boot).
pub fn upgrade_to_heap() {
	let mut allocator = ALLOCATOR.lock();
	if allocator.heap.is_some() {
		return;
	}
	let mut runs = Vec::with_capacity((allocator.seed_len * 2).max(64));
	runs.extend_from_slice(&allocator.seed[..allocator.seed_len]);
	allocator.heap = Some(runs);
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

// Allocate one physical frame, returning its physical address.
pub fn allocate() -> Option<u64> {
	ALLOCATOR.lock().take_one()
}

// Allocate `pages` physically CONTIGUOUS frames, returning the base address of
// the span - the allocation DMA buffers ride, so a device sees one run. None if
// no free run is large enough.
pub fn allocate_contiguous(pages: usize) -> Option<u64> {
	if pages == 0 {
		return None;
	}
	ALLOCATOR.lock().take_contiguous(pages as u64)
}

// Return a physical frame to the pool (re-coalescing with its neighbors, so
// contiguous runs re-form as buffers are released).
//
// SAFETY: `phys` must be a frame previously obtained from `allocate` (or part of
// an `allocate_contiguous` span) that is no longer in use (and no longer mapped
// anywhere it could be written through).
pub fn deallocate(phys: u64) {
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
	let mut frames = Vec::with_capacity(pages);
	for _ in 0..pages {
		match allocate() {
			Some(phys) => frames.push(phys),
			None => {
				free_pages(&frames);
				return None;
			}
		}
	}
	frames.sort_unstable();
	Some(frames)
}

// Return a set of frames to the pool.
pub fn free_pages(frames: &[u64]) {
	for &phys in frames {
		deallocate(phys);
	}
}

const fn align_up(value: u64, align: u64) -> u64 {
	(value + align - 1) & !(align - 1)
}
