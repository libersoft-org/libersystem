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
		// Checked, because an allocator's safety should not be a property of its callers.
		//
		// `len.div_ceil(PAGE_SIZE) * PAGE_SIZE` and `self.next + len` both wrap in release, and
		// today's syscall limits happen to stop the values that would reach them. That is a
		// guarantee held somewhere else, by code that has no idea it is holding it.
		let Some(len) = len.checked_next_multiple_of(PAGE_SIZE) else {
			return 0;
		};
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
		let Some(after) = self.next.checked_add(len) else {
			return 0;
		};
		if after > self.end {
			return 0;
		}
		let base = self.next;
		self.next += len;
		base
	}

	// Return a range to the pool, merging it with adjacent free ranges. A range
	// ending at the bump cursor folds back into the bump instead.
	pub fn free(&mut self, base: u64, len: u64) {
		let Some(len) = len.checked_next_multiple_of(PAGE_SIZE) else {
			return;
		};
		let Some(end) = base.checked_add(len) else {
			return;
		};
		let _ = end;
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
		// FALLIBLY, and a refusal LEAKS THE RANGE rather than aborting the kernel.
		//
		// Ring 3 reaches this on every unmap, and a freed range that touches no neighbour is a new
		// hole - so the list grows, and `Vec::insert` reallocates. The heap module makes the same
		// trade on its own rollback path and states it: address space, of which there is 2^48,
		// rather than frames, of which there are not. A hole that is never reused costs a range no
		// mapping will ever be given; the alternative costs the machine.
		if self.free.try_reserve(1).is_err() {
			return;
		}
		self.free.insert(pos, (base, len));
	}
}

#[cfg(test)]
mod tests {
	use super::VaPool;
	use crate::mem::frame::PAGE_SIZE;

	crate::tagged_test!(a_released_range_is_reused_and_adjacent_ones_coalesce, [Memory], id = "kernel.mem.vapool.a_released_range_is_reused_and_adjacent_ones_coalesce", covers = ["kernel"]);
	fn a_released_range_is_reused_and_adjacent_ones_coalesce() {
		// On a pool of its own, so the addresses are the pool's rules and nothing else's.
		// Asserting this through the kernel mmap window instead was what it used to do, and
		// that window is shared with every kernel thread stack in the system - so the answers
		// depended on what had run before.
		const BASE: u64 = 0x1000_0000;
		let mut pool = VaPool::new(BASE, BASE + 0x1_0000 * PAGE_SIZE);

		// Reuse: a range released comes straight back.
		let first = pool.alloc(PAGE_SIZE);
		assert_eq!(first, BASE, "the first allocation starts the window");
		pool.free(first, PAGE_SIZE);
		assert_eq!(pool.alloc(PAGE_SIZE), first, "a released range is handed out again");
		pool.free(first, PAGE_SIZE);

		// Adjacency: consecutive allocations pack.
		let a = pool.alloc(PAGE_SIZE);
		let b = pool.alloc(PAGE_SIZE);
		assert_eq!(b, a + PAGE_SIZE, "consecutive allocations are adjacent");

		// Coalescing, released low-then-high: the merged hole takes a mapping neither half
		// could have held.
		pool.free(a, PAGE_SIZE);
		pool.free(b, PAGE_SIZE);
		let merged = pool.alloc(2 * PAGE_SIZE);
		assert_eq!(merged, a, "two adjacent ranges released in order merge into one");
		pool.free(merged, 2 * PAGE_SIZE);

		// And released high-then-low, which is the case that needs the left neighbour
		// checked as well as the right.
		let a = pool.alloc(PAGE_SIZE);
		let b = pool.alloc(PAGE_SIZE);
		pool.free(b, PAGE_SIZE);
		pool.free(a, PAGE_SIZE);
		assert_eq!(pool.alloc(2 * PAGE_SIZE), a, "two adjacent ranges released in reverse merge too");
		pool.free(a, 2 * PAGE_SIZE);

		// A hole between two live ranges merges with BOTH when the middle comes back.
		let one = pool.alloc(PAGE_SIZE);
		let two = pool.alloc(PAGE_SIZE);
		let three = pool.alloc(PAGE_SIZE);
		pool.free(one, PAGE_SIZE);
		pool.free(three, PAGE_SIZE);
		pool.free(two, PAGE_SIZE);
		assert_eq!(pool.alloc(3 * PAGE_SIZE), one, "a range closing the gap between two free ones folds all three together");
		pool.free(one, 3 * PAGE_SIZE);
	}

	crate::tagged_test!(an_exhausted_window_refuses_rather_than_running_past_its_end, [Memory], id = "kernel.mem.vapool.an_exhausted_window_refuses_rather_than_running_past_its_end", covers = ["kernel"]);
	fn an_exhausted_window_refuses_rather_than_running_past_its_end() {
		// Four pages, and a fifth must be refused rather than handed out above the window -
		// which would be an address in whatever lies beyond it.
		const BASE: u64 = 0x2000_0000;
		let mut pool = VaPool::new(BASE, BASE + 4 * PAGE_SIZE);
		let mut taken = [0u64; 4];
		for slot in taken.iter_mut() {
			*slot = pool.alloc(PAGE_SIZE);
			assert!(*slot >= BASE && *slot < BASE + 4 * PAGE_SIZE, "every allocation lies inside the window");
		}
		assert_eq!(pool.alloc(PAGE_SIZE), 0, "a fifth page must be refused");
		assert_eq!(pool.alloc(64 * PAGE_SIZE), 0, "so must a request larger than the whole window");
		// Giving one back makes exactly one available again.
		pool.free(taken[1], PAGE_SIZE);
		assert_eq!(pool.alloc(PAGE_SIZE), taken[1], "the released page is the one handed out");
		assert_eq!(pool.alloc(PAGE_SIZE), 0, "and the window is full again");
		// A request rounds up to whole pages: a byte still costs a page, and zero costs
		// nothing at all rather than a zero-length range someone would have to free.
		assert_eq!(VaPool::new(BASE, BASE + PAGE_SIZE).alloc(0), 0, "a zero-length request is refused");
		let mut small = VaPool::new(BASE, BASE + PAGE_SIZE);
		assert_eq!(small.alloc(1), BASE, "one byte takes one page");
		assert_eq!(small.alloc(1), 0, "and the window is then empty");
	}
}
