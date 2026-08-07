// A virtual-address window's allocator.
//
// A bump cursor over [next, end) with a sorted, coalesced free list of released ranges, so a
// long-lived window that maps and unmaps forever reuses addresses instead of walking off its
// span. Allocation is first-fit from the free list (splitting a larger range), falling back to
// the bump; releasing a range merges it with its neighbours, so churn cannot shatter the list
// into unusable slivers.
//
// The kernel keeps one of these for its own mmap window, shared by every kernel thread because
// the window itself is shared. Each user address space keeps its OWN, because two address spaces
// may safely hand out the same virtual address - it is only the same NUMBER, not the same memory.
// One global user pool made them contend for a resource they do not share, let one process
// exhaust the window for every other, and tied a range's lifetime to nothing in particular. A
// pool that lives inside the address space dies with it.

use alloc::vec::Vec;

use crate::mem::frame::PAGE_SIZE;

pub struct VaPool {
	next: u64,
	end: u64,
	free: Vec<(u64, u64)>,
}

impl VaPool {
	pub const fn new(base: u64, end: u64) -> VaPool {
		VaPool { next: base, end, free: Vec::new() }
	}

	// Hand out a page-aligned range of at least `len` bytes, or 0 when the window
	// is exhausted (free list and bump both).
	pub fn alloc(&mut self, len: u64) -> u64 {
		let len = len.div_ceil(PAGE_SIZE) * PAGE_SIZE;
		if len == 0 {
			return 0;
		}
		for i in 0..self.free.len() {
			let (base, flen) = self.free[i];
			if flen >= len {
				if flen == len {
					self.free.remove(i);
				} else {
					self.free[i] = (base + len, flen - len);
				}
				return base;
			}
		}
		if self.next + len > self.end {
			return 0;
		}
		let base = self.next;
		self.next += len;
		base
	}

	// Return a range to the pool, merging it with adjacent free ranges. A range
	// ending at the bump cursor folds back into the bump instead.
	pub fn free(&mut self, base: u64, len: u64) {
		let len = len.div_ceil(PAGE_SIZE) * PAGE_SIZE;
		if len == 0 {
			return;
		}
		let mut base = base;
		let mut len = len;
		// find the insert position in the sorted list and coalesce both neighbors.
		let pos = self.free.partition_point(|&(b, _)| b < base);
		if pos < self.free.len() && base + len == self.free[pos].0 {
			len += self.free[pos].1;
			self.free.remove(pos);
		}
		if pos > 0 && self.free[pos - 1].0 + self.free[pos - 1].1 == base {
			base = self.free[pos - 1].0;
			len += self.free[pos - 1].1;
			self.free.remove(pos - 1);
		}
		if base + len == self.next {
			self.next = base;
			return;
		}
		let pos = self.free.partition_point(|&(b, _)| b < base);
		self.free.insert(pos, (base, len));
	}
}
