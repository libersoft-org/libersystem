// A binary buddy allocator over a bitmap sized from the memory map, once.
//
// The run table it replaces answered three things badly, and M0150 names them:
//
//   - its metadata grew with FRAGMENTATION rather than with memory, so the bound had to be
//     guessed and a free past the guess was lost;
//   - coalescing depended on that table having room, so the one operation that must never
//     fail could;
//   - a contiguous multi-page allocation walked the run list, so a DMA buffer cost a scan
//     proportional to how splintered the pool had become.
//
// A buddy answers all three with arithmetic. A block of 2^k pages at index `i` has exactly one
// buddy, `i ^ 1` at that order, found by xor rather than by searching; merging is testing one bit
// and clearing it. There is no table to fill, so a free cannot fail - which is the property the
// whole milestone exists for.
//
// WHY BITMAPS AND NOT FREE LISTS. The textbook buddy keeps a list per order so allocation is O(1)
// instead of a scan. Two ways to build one here and both cost more than they return:
//
//   - Intrusive lists, threading `next` through the free pages themselves, are what a general
//     kernel does. They need every free page reachable through the direct map at the moment it is
//     freed, on all three architectures, forever. That is a correctness dependency on something
//     this allocator currently does not care about at all.
//   - Side arrays cost 8 bytes per block per order - about 16 bytes per page once summed - which
//     is twice what the run table cost, to remove a scan that is already small.
//
// Because the scan is small. Allocation at order k reads the order-k bitmap, which holds
// `pages >> k` bits: a 64-page DMA buffer on a 512 MB machine scans 32 words, against a run list
// that could hold 65536 entries. The thing the milestone asked to remove is removed; what is left
// is not a linear scan over the pool, it is a handful of `u64` loads with a hint in front of them.
//
// Metadata is one bit per block per order, summed over orders: 2 bits per page, `pages / 4` bytes.
// 128 KiB on a 4 GiB machine, reserved once and never grown.

use alloc::vec::Vec;

use super::PAGE_SIZE;

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
	pub fn new(base: u64, pages: u64) -> Option<Self> {
		let mut offsets = [0usize; MAX_ORDER + 1];
		let mut words = [0usize; MAX_ORDER + 1];
		let mut total = 0usize;
		for order in 0..=MAX_ORDER {
			let blocks = (pages >> order).max(1);
			let w = (blocks as usize).div_ceil(64);
			offsets[order] = total;
			words[order] = w;
			total += w;
		}
		let mut bits: Vec<u64> = Vec::new();
		bits.try_reserve_exact(total).ok()?;
		bits.resize(total, 0);
		Some(Self { base, pages, bits, offsets, words, hint: [usize::MAX; MAX_ORDER + 1], counts: [0; MAX_ORDER + 1], free_pages: 0 })
	}

	pub fn free_pages(&self) -> u64 {
		self.free_pages
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
	// that is `check_owned_free`'s job one level up.
	pub fn free(&mut self, phys: u64, order: usize) -> bool {
		if order > MAX_ORDER || phys < self.base {
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
	#[must_use]
	pub fn free_span(&mut self, phys: u64, pages: u64) -> u64 {
		let mut at = phys;
		let mut left = pages;
		let mut taken_total = 0u64;
		while left > 0 {
			let page = (at - self.base) / PAGE_SIZE;
			// The largest order whose alignment `page` satisfies and whose size fits in what is
			// left. `trailing_zeros` of the index is the alignment; `left` bounds the size.
			let by_alignment = if page == 0 { MAX_ORDER } else { (page.trailing_zeros() as usize).min(MAX_ORDER) };
			let by_size = (63 - left.leading_zeros() as usize).min(MAX_ORDER);
			let order = by_alignment.min(by_size);
			let taken = 1u64 << order;
			if self.free(at, order) {
				taken_total += taken;
			}
			at += taken * PAGE_SIZE;
			left -= taken;
		}
		taken_total
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
mod tests {
	use super::*;

	// A buddy over a made-up extent, so the arithmetic can be exercised without touching the
	// machine's own pool. `base` is a plausible physical address and nothing is ever dereferenced.
	fn fixture(pages: u64) -> Buddy {
		Buddy::new(0x4000_0000, pages).expect("metadata for the fixture")
	}

	crate::tagged_test!(a_buddy_merges_back_to_one_block_however_it_is_freed, [Frame, Memory], covers = ["kernel"]);
	fn a_buddy_merges_back_to_one_block_however_it_is_freed() {
		// The property the run table could not give: coalescing that depends on arithmetic rather
		// than on a table having room. Free every page of a span individually, in an order chosen
		// to be awkward, and the whole span must come back as ONE block - which is only observable
		// by then allocating it whole.
		const PAGES: u64 = 64;
		let mut buddy = fixture(PAGES);
		buddy.free_span(0x4000_0000, PAGES);
		assert_eq!(buddy.free_pages(), PAGES);

		// Take it all as one block, to prove the seed coalesced.
		let whole = buddy.alloc_contiguous(PAGES).expect("a freshly seeded extent is one block");
		assert_eq!(whole, 0x4000_0000);
		assert_eq!(buddy.free_pages(), 0);

		// Now hand it back one page at a time, odd pages first - the order that defeats a
		// neighbour-merging run table until the very last insert.
		for page in (1..PAGES).step_by(2) {
			assert!(buddy.free(0x4000_0000 + page * PAGE_SIZE, 0), "page {page} is a legitimate free");
		}
		for page in (0..PAGES).step_by(2) {
			assert!(buddy.free(0x4000_0000 + page * PAGE_SIZE, 0), "page {page} is a legitimate free");
		}
		assert_eq!(buddy.free_pages(), PAGES, "every page came back");

		// And the merge really happened: the span allocates whole again. A pool that had kept 64
		// separate one-page blocks would refuse this.
		let again = buddy.alloc_contiguous(PAGES).expect("the pages merged back into one block");
		assert_eq!(again, 0x4000_0000);
	}

	crate::tagged_test!(a_buddy_hands_back_the_rounding_of_a_contiguous_request, [Frame, Memory], covers = ["kernel"]);
	fn a_buddy_hands_back_the_rounding_of_a_contiguous_request() {
		// A buddy allocates in powers of two, so a 3-page request takes a 4-page block. Keeping
		// the fourth would be a quarter of that allocation lost for as long as it lives, and on a
		// pool that churns DMA buffers it compounds into memory nobody can account for.
		const PAGES: u64 = 64;
		let mut buddy = fixture(PAGES);
		buddy.free_span(0x4000_0000, PAGES);

		let base = buddy.alloc_contiguous(3).expect("three pages");
		assert_eq!(buddy.free_pages(), PAGES - 3, "the request costs THREE pages, not the four its block holds");

		// The spare page is genuinely available: ask for it.
		let spare = buddy.alloc_contiguous(1).expect("the rounded-off page is back in the pool");
		assert_eq!(spare, base + 3 * PAGE_SIZE, "and it is the page immediately after the request");
	}

	crate::tagged_test!(a_pool_that_is_not_a_power_of_two_never_hands_out_the_gap, [Frame, Memory], covers = ["kernel"]);
	fn a_pool_that_is_not_a_power_of_two_never_hands_out_the_gap() {
		// Real memory maps are not powers of two, and the tail is where a buddy goes wrong: a block
		// whose buddy would lie past the end of the extent has no buddy, and merging into one would
		// hand out memory the machine does not have.
		const PAGES: u64 = 100;
		let mut buddy = fixture(PAGES);
		buddy.free_span(0x4000_0000, PAGES);
		assert_eq!(buddy.free_pages(), PAGES, "a hundred pages is a hundred pages");

		// Drain it completely, one page at a time, and count. If the tail merged into a block that
		// runs off the end, this hands out more than there is.
		let mut taken = 0u64;
		while buddy.alloc(0).is_some() {
			taken += 1;
			assert!(taken <= PAGES, "the allocator handed out more pages than the extent holds");
		}
		assert_eq!(taken, PAGES, "and it handed out every page it was given");
		assert_eq!(buddy.free_pages(), 0);
	}

	crate::tagged_test!(a_buddy_refuses_a_free_it_cannot_frame, [Frame, Memory], covers = ["kernel"]);
	fn a_buddy_refuses_a_free_it_cannot_frame() {
		// `free` cannot fail for want of ROOM - that is the whole point - but it can refuse an
		// argument that is not a block: an address below the extent, past it, or misaligned for the
		// order claimed. Those are broken callers rather than a full table, and silently accepting
		// one corrupts the tree.
		const PAGES: u64 = 64;
		let mut buddy = fixture(PAGES);
		assert!(!buddy.free(0x3fff_f000, 0), "below the extent");
		assert!(!buddy.free(0x4000_0000 + PAGES * PAGE_SIZE, 0), "past the extent");
		assert!(!buddy.free(0x4000_0000 + PAGE_SIZE, 1), "a two-page block must be two-page aligned");
		assert_eq!(buddy.free_pages(), 0, "and none of them added anything to the pool");
	}
}
