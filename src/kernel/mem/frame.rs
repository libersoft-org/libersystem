// Physical frame allocator.
//
// Free physical memory is kept as a sorted table of contiguous runs (base +
// length), seeded straight from the usable regions of the Limine memory map. A
// single-frame alloc takes the head page of the first run (O(1)); a contiguous
// alloc first-fits a whole run - DMA buffers need physically contiguous spans
// (virtqueue rings, block data stages, jumbo frames) - and a free re-coalesces
// with its neighbors, so runs re-form as buffers are released.
//
// The run table is a fixed array, deliberately heap-free: the frame allocator
// must work before the heap exists and inside exception contexts (demand-paged
// stack growth), where growing a Vec could re-enter it through the heap. If the
// pool ever fragments past the table (pathological), the freed page is leaked
// loudly rather than corrupting state.
//
// The allocator is global and guarded by a SpinLock, so it is safe to call from
// any core.

#![allow(dead_code)]

use limine::memory_map::EntryType;
use limine::response::MemoryMapResponse;

use alloc::vec::Vec;

use crate::sync::SpinLock;

pub const PAGE_SIZE: u64 = 4096;

// The most disjoint free runs the table holds. Boot starts with a handful (one
// per usable memory-map region); coalescing keeps the steady state small.
const MAX_RUNS: usize = 1024;

// One contiguous run of free frames: `pages` pages starting at physical `base`.
#[derive(Clone, Copy)]
struct Run {
	base: u64,
	pages: u64,
}

struct FrameAllocator {
	runs: [Run; MAX_RUNS],
	len: usize,
	free_count: usize,
	total_count: usize,
}

impl FrameAllocator {
	const fn new() -> Self {
		Self { runs: [Run { base: 0, pages: 0 }; MAX_RUNS], len: 0, free_count: 0, total_count: 0 }
	}

	// The index of the first run whose base is >= `base` (the insertion point).
	fn position(&self, base: u64) -> usize {
		self.runs[..self.len].partition_point(|r| r.base < base)
	}

	// Return `pages` frames at `base` to the pool, coalescing with the runs on
	// either side. A table overflow (pathological fragmentation) leaks the span
	// loudly rather than corrupting state.
	fn insert(&mut self, base: u64, pages: u64) {
		let at = self.position(base);
		// merge into the left neighbor when adjacent
		if at > 0 && self.runs[at - 1].base + self.runs[at - 1].pages * PAGE_SIZE == base {
			self.runs[at - 1].pages += pages;
			// and fold the right neighbor in if the gap just closed
			if at < self.len && self.runs[at - 1].base + self.runs[at - 1].pages * PAGE_SIZE == self.runs[at].base {
				self.runs[at - 1].pages += self.runs[at].pages;
				self.runs.copy_within(at + 1..self.len, at);
				self.len -= 1;
			}
			self.free_count += pages as usize;
			return;
		}
		// merge into the right neighbor when adjacent
		if at < self.len && base + pages * PAGE_SIZE == self.runs[at].base {
			self.runs[at].base = base;
			self.runs[at].pages += pages;
			self.free_count += pages as usize;
			return;
		}
		if self.len == MAX_RUNS {
			crate::serial_println!("frame: WARNING: free-run table full, leaking {} page(s) at {:#x}", pages, base);
			return;
		}
		self.runs.copy_within(at..self.len, at + 1);
		self.runs[at] = Run { base, pages };
		self.len += 1;
		self.free_count += pages as usize;
	}

	// Take one page off the head of the first run.
	fn take_one(&mut self) -> Option<u64> {
		if self.len == 0 {
			return None;
		}
		let base = self.runs[0].base;
		self.runs[0].base += PAGE_SIZE;
		self.runs[0].pages -= 1;
		if self.runs[0].pages == 0 {
			self.runs.copy_within(1..self.len, 0);
			self.len -= 1;
		}
		self.free_count -= 1;
		Some(base)
	}

	// First-fit a physically contiguous span of `pages`, taking it off the head
	// of the first run large enough.
	fn take_contiguous(&mut self, pages: u64) -> Option<u64> {
		for at in 0..self.len {
			if self.runs[at].pages >= pages {
				let base = self.runs[at].base;
				self.runs[at].base += pages * PAGE_SIZE;
				self.runs[at].pages -= pages;
				if self.runs[at].pages == 0 {
					self.runs.copy_within(at + 1..self.len, at);
					self.len -= 1;
				}
				self.free_count -= pages as usize;
				return Some(base);
			}
		}
		None
	}
}

static ALLOCATOR: SpinLock<FrameAllocator> = SpinLock::new(FrameAllocator::new());

// Populate the run table from the usable regions of the Limine memory map.
// Physical frame 0 is never handed out (0 doubles as "no frame" in several
// interfaces), so a region starting there is trimmed by one page.
pub fn init(memory_map: &MemoryMapResponse) {
	let mut allocator = ALLOCATOR.lock();
	for entry in memory_map.entries() {
		if entry.entry_type != EntryType::USABLE {
			continue;
		}
		let mut base = align_up(entry.base, PAGE_SIZE);
		let end = entry.base + entry.length;
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
