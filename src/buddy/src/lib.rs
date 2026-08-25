//! A binary buddy allocator over a bitmap sized from the memory map, once.
//!
//! SEPARATE FROM THE KERNEL SO IT CAN BE DRIVEN HARD. This is the kernel's physical frame
//! allocator and it used to live inside `kernel::mem::frame`, where the only way to exercise it was
//! to boot a guest: a few thousand allocations per architecture, in whatever order that boot
//! happened to produce, at up to ninety minutes a run on riscv64. This crate exists for a defect
//! seen ONCE in exactly that setting - eighty double allocations of one page, on the last page of the
//! first usable region - and "run the suite again and hope" is not a way to find it.
//!
//! It is CLOSED (2026-08-14), and on a stronger statement than an explanation of the original event.
//! The arithmetic was searched here over 14.4 million host operations against a reference model
//! without ever producing the signature, and the one mechanism that could produce it - a free
//! overlapping a block that is already free - is now refused inside the allocator at its source. So
//! the signature is unreachable from any caller, which is a different claim from "we could not
//! reproduce it": the first says the code cannot do it, the second says we did not see it do it.
//!
//! Here the same code takes tens of millions of operations in under a second, against a reference
//! model that knows where every page is, over pool shapes chosen to be the shape of the sighting.
//! Nothing about the allocator changed in the move; what changed is how many times it can be asked.
//!
//! The run table it replaces answered three things badly:
//!
//!   - its metadata grew with FRAGMENTATION rather than with memory, so the bound had to be
//!     guessed and a free past the guess was lost;
//!   - coalescing depended on that table having room, so the one operation that must never
//!     fail could;
//!   - a contiguous multi-page allocation walked the run list, so a DMA buffer cost a scan
//!     proportional to how splintered the pool had become.
//!
//! A buddy answers all three with arithmetic. A block of 2^k pages at index `i` has exactly one
//! buddy, `i ^ 1`, found by xor rather than by searching; merging is testing one bit and clearing
//! it. There is no table to fill, so a free cannot fail - which is the property the whole milestone
//! exists for.
//!
//! WHY BITMAPS AND NOT FREE LISTS. The textbook buddy keeps a list per order so allocation is O(1)
//! instead of a scan. Two ways to build one here and both cost more than they return:
//!
//!   - Intrusive lists, threading `next` through the free pages themselves, are what a general
//!     kernel does. They need every free page reachable through the direct map at the moment it is
//!     freed, on all three architectures, forever. That is a correctness dependency on something
//!     this allocator currently does not care about at all.
//!   - Side arrays cost 8 bytes per block per order - about 16 bytes per page once summed - which
//!     is twice what the run table cost, to remove a scan that is already small.
//!
//! Because the scan is small. Allocation at order k reads the order-k bitmap, which holds
//! `pages >> k` bits: a 64-page DMA buffer on a 512 MB machine scans 32 words, against a run list
//! that could hold 65536 entries. The thing the milestone asked to remove is removed; what is left
//! is not a linear scan over the pool, it is a handful of `u64` loads with a hint in front of them.
//!
//! Metadata is one bit per block per order, summed over orders: 2 bits per page, `pages / 4` bytes.
//! 256 KiB on a 4 GiB machine, reserved once and never grown.
//!
//! That figure said 128 KiB and was wrong by a factor of two: a 4 GiB extent is 1,048,576 pages at
//! `PAGE_SIZE`, so `pages / 4` is 262,144 bytes. The arithmetic above it was right and only the
//! example was wrong, which is the kind of error nothing catches - the implementation reports the
//! real cost, so only a person planning boot memory from the prose would ever be misled.
//! `the_metadata_cost_is_what_the_module_says_it_is` now derives it from `metadata_bytes()`.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

// The page size the addresses in this file are in. The kernel's `frame::PAGE_SIZE` is the same
// number; it is repeated here rather than imported because this crate is the arithmetic and has no
// reason to depend on the kernel it serves.
pub const PAGE_SIZE: u64 = 4096;

// The largest block the allocator tracks: 2^18 pages, one gigabyte. Well past any DMA request in
// this system, and the metadata for the higher orders is a geometric tail that costs nothing - the
// cap exists so the order loops are bounded by a constant rather than by the pool.
pub const MAX_ORDER: usize = 18;

pub struct Buddy {
	// Physical address of page index 0. Every index below is relative to this.
	base: u64,
	// Pages in the extent, INCLUDING any holes between usable regions. Holes are simply never
	// freed into the allocator, so they read as allocated forever and no block that overlaps one
	// can merge across it.
	pages: u64,
	// One bit per block per order, packed order after order. Set means "this block is free AND is
	// a whole free block at this order" - a page inside a larger free block is NOT set at order 0.
	bits: Vec<u64>,
	// Where each order's bitmap starts, in words.
	offsets: [usize; MAX_ORDER + 1],
	// Words per order.
	words: [usize; MAX_ORDER + 1],
	// The first word of each order's bitmap that might hold a set bit. Everything below it is
	// known empty, so a scan starts here rather than at zero. Lowered on free, raised on scan.
	hint: [usize; MAX_ORDER + 1],
	// How many free blocks each order holds.
	//
	// Without it the hint alone is not enough, and the shape of a buddy is what makes that bite. A
	// freshly seeded pool has every page merged into a few huge blocks, so orders 0..17 are EMPTY -
	// and an empty order can only be proved empty by reading its whole bitmap. Every second
	// single-page allocation did exactly that: split a big block (which lowers the low orders'
	// hints), consume the piece, then scan sixteen thousand words to rediscover that order 0 is
	// empty again. It cost the x86_64 suite 117 s -> 483 s, and it is the difference between a
	// buddy and a linear scan wearing a buddy's clothes.
	counts: [u64; MAX_ORDER + 1],
	// Free pages, maintained rather than counted.
	free_pages: u64,
}

impl Buddy {
	// Build the metadata for an extent of `pages` frames starting at `base`. Everything starts
	// ALLOCATED: the caller frees the usable spans in, which is also what leaves the holes between
	// them permanently out of the pool.
	//
	// Fallible, because it is sized from the memory map and the map comes from the machine.
	//
	// AND THE EXTENT IS VALIDATED, WHICH IT WAS NOT. This accepted any `base` and any `pages`: an
	// unaligned base, zero pages, or a byte length that overflows the address space. Each of those
	// makes a SAFE method later manufacture a physical address that is not a frame - `free` checks
	// alignment RELATIVE to `self.base`, so an unaligned base lets an equally unaligned address
	// through, and `Buddy::new(1, 1)` can hand out physical address 1 as a page. The kernel's caller
	// supplies page-granular runs today, so nothing was reachable; but `new` returns `Option`
	// precisely to be the boundary that establishes this, and a boundary that establishes nothing
	// is where the next caller gets it wrong.
	pub fn new(base: u64, pages: u64) -> Option<Self> {
		if pages == 0 || base % PAGE_SIZE != 0 {
			return None;
		}
		// The whole extent has to exist: `pages * PAGE_SIZE` must not overflow, and neither must the
		// end address. Every later address reconstruction is `base + index * size * PAGE_SIZE`, and
		// this is what makes those additions provably in range rather than merely usually so.
		let extent_bytes = pages.checked_mul(PAGE_SIZE)?;
		base.checked_add(extent_bytes)?;
		let mut offsets = [0usize; MAX_ORDER + 1];
		let mut words = [0usize; MAX_ORDER + 1];
		let mut total = 0usize;
		for order in 0..=MAX_ORDER {
			let blocks = (pages >> order).max(1);
			let w = usize::try_from(blocks).ok()?.div_ceil(64);
			offsets[order] = total;
			words[order] = w;
			// Checked, for the same reason the extent is: this is a sum over a caller-supplied page
			// count, and `total` sizes the one allocation the type makes.
			total = total.checked_add(w)?;
		}
		let mut bits: Vec<u64> = Vec::new();
		bits.try_reserve_exact(total).ok()?;
		bits.resize(total, 0);
		Some(Self { base, pages, bits, offsets, words, hint: [usize::MAX; MAX_ORDER + 1], counts: [0; MAX_ORDER + 1], free_pages: 0 })
	}

	pub fn free_pages(&self) -> u64 {
		self.free_pages
	}

	// The extent this allocator frames, as (first physical address, pages). The kernel's audit and
	// the tests both want to walk it, and both were reaching for the private fields.
	pub fn extent(&self) -> (u64, u64) {
		(self.base, self.pages)
	}

	// How many bytes of metadata this allocator holds. For the boot report and the test that
	// pins the sizing.
	pub fn metadata_bytes(&self) -> usize {
		self.bits.len() * 8
	}

	fn blocks_at(&self, order: usize) -> u64 {
		(self.pages >> order).max(1)
	}

	fn get(&self, order: usize, index: u64) -> bool {
		if index >= self.blocks_at(order) {
			return false;
		}
		let word = self.offsets[order] + (index / 64) as usize;
		self.bits[word] & (1 << (index % 64)) != 0
	}

	// `set` and `clear` do NOT range-check the way `get` does, and the asymmetry is deliberate: a
	// read of a block that does not exist has an obvious answer (it is not free), while a WRITE to
	// one is a bug in this file - the index would land in the next order's bitmap and free memory
	// that is already out on loan. Every caller derives its index from a block that fits inside the
	// extent, which `free` enforces and the split loop preserves, so the assertion below states the
	// invariant rather than defending against callers.
	fn set(&mut self, order: usize, index: u64) {
		debug_assert!(index < self.blocks_at(order), "set past the end of order {order}: block {index} of {}", self.blocks_at(order));
		let word = self.offsets[order] + (index / 64) as usize;
		if self.bits[word] & (1 << (index % 64)) == 0 {
			self.bits[word] |= 1 << (index % 64);
			self.counts[order] += 1;
		}
		let w = (index / 64) as usize;
		if w < self.hint[order] {
			self.hint[order] = w;
		}
	}

	fn clear(&mut self, order: usize, index: u64) {
		debug_assert!(index < self.blocks_at(order), "clear past the end of order {order}: block {index} of {}", self.blocks_at(order));
		let word = self.offsets[order] + (index / 64) as usize;
		if self.bits[word] & (1 << (index % 64)) != 0 {
			self.bits[word] &= !(1 << (index % 64));
			self.counts[order] -= 1;
		}
	}

	// Is any bit in `[start, start + count)` of this order's bitmap set? Word at a time with the
	// ends masked, because the range can be the whole of a low order's bitmap when a large block is
	// being freed and asking bit by bit would make a seed of a 512 MB pool read a million bits.
	fn any_bit_in_range(&self, order: usize, start: u64, count: u64) -> bool {
		let end = (start + count).min(self.blocks_at(order));
		if start >= end {
			return false;
		}
		let (first_word, last_word) = ((start / 64) as usize, ((end - 1) / 64) as usize);
		for word in first_word..=last_word {
			let mut value = self.bits[self.offsets[order] + word];
			if word == first_word {
				value &= u64::MAX << (start % 64);
			}
			if word == last_word && (end % 64) != 0 {
				value &= u64::MAX >> (64 - end % 64);
			}
			if value != 0 {
				return true;
			}
		}
		false
	}

	// Does the block of 2^order pages at page index `page` overlap anything already free?
	//
	// Three ways it can, and a check that asks only one of them is a check that lets the other two
	// through: the block itself may be free; a LARGER block may cover it; or a smaller free block
	// may sit INSIDE it, which is the one a per-page `is_free_page` on the first page misses
	// entirely. All three end the same way - a page in the free set twice.
	fn block_touches_free(&self, page: u64, order: usize) -> bool {
		// This order and every larger one: exactly one block covers `page` at each.
		if (order..=MAX_ORDER).any(|at| self.get(at, page >> at)) {
			return true;
		}
		// And every smaller order, where the block spans many.
		(0..order).any(|at| self.any_bit_in_range(at, page >> at, 1u64 << (order - at)))
	}

	// The first free block at `order`, if there is one. Scans from the hint and moves it forward
	// over words that turned out to be empty, so a run of allocations does not re-read them.
	fn first_free(&mut self, order: usize) -> Option<u64> {
		// The count answers the common question - "is this order empty" - without reading a single
		// word. See the field's comment: proving emptiness by scanning is what made this slow.
		if self.counts[order] == 0 {
			return None;
		}
		let start = self.hint[order].min(self.words[order]);
		for word in start..self.words[order] {
			let value = self.bits[self.offsets[order] + word];
			if value == 0 {
				continue;
			}
			self.hint[order] = word;
			let index = (word as u64) * 64 + value.trailing_zeros() as u64;
			return (index < self.blocks_at(order)).then_some(index);
		}
		self.hint[order] = self.words[order];
		None
	}

	// Take a block of exactly 2^order pages, splitting a larger one if that is all there is.
	// Returns the physical address of the block's first page.
	pub fn alloc(&mut self, order: usize) -> Option<u64> {
		if order > MAX_ORDER {
			return None;
		}
		let mut found = None;
		for larger in order..=MAX_ORDER {
			if let Some(index) = self.first_free(larger) {
				found = Some((larger, index));
				break;
			}
		}
		let (mut at_order, mut index) = found?;
		self.clear(at_order, index);
		// Split down, freeing the half that is not wanted at each step. The kept half is always
		// the lower one, so the address arithmetic below stays the block's own.
		while at_order > order {
			at_order -= 1;
			index *= 2;
			// The upper half becomes a free block of the smaller order.
			self.set(at_order, index + 1);
		}
		self.free_pages -= 1u64 << order;
		Some(self.base + index * (1u64 << order) * PAGE_SIZE)
	}

	// Give back a block of exactly 2^order pages, merging with its buddy for as long as the buddy
	// is free. CANNOT FAIL: merging is clearing a bit, and the bit is always there.
	//
	// The caller must hand back a block it got from `alloc` at the same order, or a span it framed
	// itself with `free_span`. A misaligned index is refused rather than corrupting the tree -
	// which is a check about this allocator's own invariants, not about the caller's ownership;
	// that is the frame allocator's `check_owned_free` one level up.
	//
	// A FREE OF SOMETHING ALREADY FREE IS REFUSED HERE, not only by the caller. It used to be left
	// entirely to the frame allocator above, which asks `any_free` before it touches its ownership
	// record - and that is still the right place for the DECISION, because the caller has state to
	// keep consistent and this does not. But the two outcomes of forgetting it are both silent and
	// both are this file's own invariant coming apart:
	//
	//   - freeing a page that a LARGER free block already covers sets a bit under that block, so the
	//     page is in the free set twice and the next two allocations that reach it both get it. That
	//     is a double allocation, arriving with nothing to point at;
	//   - freeing the same block at the SAME order sets a bit that is already set, which `set` folds
	//     into nothing while `free_pages` still counts it - a pool that reports memory no allocation
	//     can find;
	//   - and freeing a block with a SMALLER free block inside it does the first of those from the
	//     other direction, which is the case a check on the block's first page misses entirely.
	//
	// None of them is a caller's business to survive, and refusing costs a bounded scan rather than
	// a walk of the pool: one bit per order for the blocks that could cover this one, and a masked
	// word scan for the smaller ones it could contain. So the rule lives where it cannot be
	// forgotten. `a_double_free_is_refused_and_this_is_what_it_prevents` is the demonstration,
	// through the test-only door below.
	pub fn free(&mut self, phys: u64, order: usize) -> bool {
		self.free_inner(phys, order, true)
	}

	// `free` with the already-free refusal removed, so one test can show the state that refusal
	// prevents. Crate-private and test-only: there is no legitimate caller, and the whole point of
	// the rule above is that there is no way to ask for it by accident.
	#[cfg(test)]
	pub(crate) fn free_ignoring_double(&mut self, phys: u64, order: usize) -> bool {
		self.free_inner(phys, order, false)
	}

	fn free_inner(&mut self, phys: u64, order: usize, refuse_double: bool) -> bool {
		if order > MAX_ORDER || phys < self.base {
			return false;
		}
		// The ADDRESS has to be page-aligned, not just the page index block-aligned.
		//
		// The check below tests `page % (1 << order)`, which for order 0 is vacuous - so
		// `free(base + 1, 0)` divided down to page 0 and was accepted as a free of the first page.
		// A frame allocator that accepts an address it never handed out is one whose ownership
		// record can be made to disagree with reality by an ordinary caller bug.
		if (phys - self.base) % PAGE_SIZE != 0 {
			return false;
		}
		let page = (phys - self.base) / PAGE_SIZE;
		if page % (1u64 << order) != 0 || page >= self.pages {
			return false;
		}
		// The whole BLOCK has to be inside the extent, not just its first page. A pool whose page
		// count is not a power of two has a tail where the higher orders do not fit, and setting a
		// bit for a block that runs off the end would hand out memory that is not there.
		if page + (1u64 << order) > self.pages {
			return false;
		}
		// Already free - as a whole, under a larger block that covers it, or in part. See `free`'s
		// comment: every one of those is silent and every one is this allocator's own invariant
		// coming apart.
		if refuse_double && self.block_touches_free(page, order) {
			return false;
		}
		let mut index = page >> order;
		let mut at = order;
		// Merge upward. `index ^ 1` is the buddy at this order, which is the whole point of a
		// buddy allocator: no search, no table, one xor.
		while at < MAX_ORDER {
			let buddy = index ^ 1;
			if !self.get(at, buddy) {
				break;
			}
			// A buddy that would take the block past the end of the extent is not a buddy: the
			// tail of a pool whose page count is not a power of two has blocks with no partner.
			if ((index & !1) << at) + (2u64 << at) > self.pages {
				break;
			}
			self.clear(at, buddy);
			index >>= 1;
			at += 1;
		}
		self.set(at, index);
		self.free_pages += 1u64 << order;
		true
	}

	// Free an arbitrary span, framed into the largest aligned blocks it contains.
	//
	// This is how the pool is seeded and how a multi-page allocation is returned: a span of 300
	// pages is not a buddy block, but it is a handful of them. Freeing page by page would work and
	// would cost 300 merge walks; framing costs one per block.
	// Returns how many pages the buddy actually TOOK. That is not always `pages`: a block whose
	// address falls outside the extent, or which would run off the end of it, is refused, and the
	// caller must not count those as free or the pool will report memory that no allocation can
	// find. Silently returning nothing was the earlier shape and it made a divergence between
	// `free_count` and the bitmap invisible.
	// A REFUSED BLOCK IS SUBDIVIDED, NOT SKIPPED. This is the defect this milestone is named for,
	// reached through the seeding path rather than through a double free.
	//
	// `free` refuses a block that overlaps ANY existing free block, and this loop used to advance by
	// the whole block whether the free was accepted or not. So one already-free page inside a chosen
	// order-3 block discarded the other SEVEN - seven pages that overlapped nothing, that no
	// allocation can ever find again, with only an aggregate shortfall in the return value to show
	// for it. Seed page 4 of an otherwise allocated 8-page extent and `free_span(base, 8)` returned
	// zero. That contradicted the overlapping-seed contract the tests describe, whose fixture
	// happens to put the overlap on a boundary where the greedy framing separates old from new.
	//
	// Splitting is the answer rather than an all-or-nothing precondition, because partial insertion
	// is what the contract promises and what a seeder actually needs: an order-0 block that is
	// refused really is already free, and every other page of the span still belongs in the pool.
	// The recursion is bounded by the order, so a block is split at most `MAX_ORDER` times.
	#[must_use]
	pub fn free_span(&mut self, phys: u64, pages: u64) -> u64 {
		// `at - self.base` below is computed before anything establishes `at >= base`, which wraps
		// in release and panics in debug. Bounded here, once, rather than trusted from every caller.
		if phys < self.base || (phys - self.base) % PAGE_SIZE != 0 {
			return 0;
		}
		// AND BOUNDED BY THE EXTENT. The loop ran until the caller's count reached zero, however far
		// past the end of the allocator that took it - unchecked `at +=` all the way - so a wrong
		// count became unbounded work with the frame allocator's lock held instead of a bounded
		// refusal. Nothing outside the extent could ever be taken anyway.
		let first = (phys - self.base) / PAGE_SIZE;
		if first >= self.pages {
			return 0;
		}
		let mut left = pages.min(self.pages - first);
		let mut at = phys;
		let mut taken_total = 0u64;
		while left > 0 {
			let page = (at - self.base) / PAGE_SIZE;
			// The largest order whose alignment `page` satisfies and whose size fits in what is
			// left. `trailing_zeros` of the index is the alignment; `left` bounds the size.
			let by_alignment = if page == 0 { MAX_ORDER } else { (page.trailing_zeros() as usize).min(MAX_ORDER) };
			let by_size = (63 - left.leading_zeros() as usize).min(MAX_ORDER);
			let order = by_alignment.min(by_size);
			let taken = 1u64 << order;
			taken_total += self.free_block_or_split(at, order);
			at += taken * PAGE_SIZE;
			left -= taken;
		}
		taken_total
	}

	// Free one aligned block, or - when it is refused - its two halves, recursively. Answers how
	// many pages actually entered the pool.
	//
	// An order-0 refusal is final and correct: a single page that `free` will not take is a page
	// that is already free, or outside the extent, and there is nothing left to split.
	fn free_block_or_split(&mut self, phys: u64, order: usize) -> u64 {
		if self.free(phys, order) {
			return 1u64 << order;
		}
		if order == 0 {
			return 0;
		}
		let half = order - 1;
		let second = phys + (1u64 << half) * PAGE_SIZE;
		self.free_block_or_split(phys, half) + self.free_block_or_split(second, half)
	}

	// Take `pages` CONTIGUOUS frames - the DMA allocation. Rounded up to a whole block, and the
	// remainder given straight back, so a 3-page request costs a 4-page block and returns one.
	pub fn alloc_contiguous(&mut self, pages: u64) -> Option<u64> {
		if pages == 0 {
			return None;
		}
		let order = order_for(pages);
		if order > MAX_ORDER {
			return None;
		}
		let phys = self.alloc(order)?;
		// Hand back what the rounding took. Without this a 65-page request would hold 128 pages
		// for as long as it lived, which on a fragmenting pool compounds.
		let taken = 1u64 << order;
		if taken > pages {
			// The remainder is the upper part of a block this allocator just handed itself, so it
			// is aligned, inside the extent and framable by construction. If it were ever not,
			// `alloc` would have debited the whole block and the return would credit back less,
			// and the pool would shrink a little on every rounded request.
			let handed_back = self.free_span(phys + pages * PAGE_SIZE, taken - pages);
			debug_assert_eq!(handed_back, taken - pages, "the rounding remainder of a block must fit back into the pool it came from");
		}
		Some(phys)
	}

	// Is this page currently free? For the ownership cross-check and the tests.
	pub fn is_free_page(&self, phys: u64) -> bool {
		if phys < self.base {
			return false;
		}
		let page = (phys - self.base) / PAGE_SIZE;
		if page >= self.pages {
			return false;
		}
		// A page is free if it is the head of a free block at any order that covers it.
		(0..=MAX_ORDER).any(|order| self.get(order, page >> order))
	}

	// Is ANY page of this span already free? The double-free question, asked before the free.
	//
	// It lives here rather than at the call site because it is a statement about this allocator's
	// representation - "already free" means free at ANY order covering the page, which only this
	// file knows - and because it is the guard the whole milestone rests on. The run table caught a
	// double free with an overlap test it could not avoid making; a bitmap has no such test built
	// in, so this is the one that has to be remembered, and a rule that has to be remembered belongs
	// next to the code that would otherwise forget it.
	// BOUNDED BY THE EXTENT, NOT BY THE CALLER'S COUNT. This ran one iteration per page of whatever
	// `pages` it was handed and computed each address with an unchecked `phys + page * PAGE_SIZE`,
	// so a caller error - `u64::MAX` pages against a one-page allocator - became either an overflow
	// panic or a wrapped address that revisits the pool low down and keeps going for as long as the
	// count says, with the frame allocator's lock held. The question is only ever about pages inside
	// this allocator, so the intersection is taken first and the walk is over that.
	pub fn any_free(&self, phys: u64, pages: u64) -> bool {
		let Some(first) = self.page_index(phys) else { return false };
		// The last page the caller named, clamped to the extent. Saturating rather than checked: a
		// count that runs off the end is a caller error, and the answer for the part that IS inside
		// the extent is still exactly right.
		let last = first.saturating_add(pages.saturating_sub(1)).min(self.pages.saturating_sub(1));
		if pages == 0 || first >= self.pages {
			return false;
		}
		(first..=last).any(|page| (0..=MAX_ORDER).any(|order| self.get(order, page >> order)))
	}

	// The index of `phys` in this extent, or None when it is outside it or not page-aligned.
	fn page_index(&self, phys: u64) -> Option<u64> {
		if phys < self.base {
			return None;
		}
		let offset = phys - self.base;
		if offset % PAGE_SIZE != 0 {
			return None;
		}
		let page = offset / PAGE_SIZE;
		if page >= self.pages { None } else { Some(page) }
	}

	// Every free block, as (order, first physical address). Walks the bitmaps rather than
	// reconstructing anything, so it sees exactly what the allocator will hand out next.
	//
	// This is what makes the one invariant a bitmap allocator can violate CHECKABLE: a page must be
	// covered by at most one free block. Two free blocks over one page is two allocations of that
	// page, and it is invisible to `free_pages()`, to `is_free_page` and to every existing test,
	// because each of them answers about one page or one total rather than about the whole set.
	pub fn for_each_free_block(&self, mut visit: impl FnMut(usize, u64)) {
		for order in 0..=MAX_ORDER {
			let blocks = self.blocks_at(order);
			for word in 0..self.words[order] {
				let mut value = self.bits[self.offsets[order] + word];
				while value != 0 {
					let bit = value.trailing_zeros() as u64;
					value &= value - 1;
					let index = (word as u64) * 64 + bit;
					if index < blocks {
						visit(order, self.base + index * (1u64 << order) * PAGE_SIZE);
					}
				}
			}
		}
	}
}

// The smallest order whose block holds `pages`.
pub fn order_for(pages: u64) -> usize {
	let mut order = 0usize;
	while (1u64 << order) < pages {
		order += 1;
		if order > MAX_ORDER {
			break;
		}
	}
	order
}

#[cfg(test)]
mod tests;
