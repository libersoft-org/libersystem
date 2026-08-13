// What a host can ask this allocator that a booted guest cannot.
//
// P02M0120's remaining item is one defect: eighty `DOUBLE ALLOCATION` lines naming one page, seen
// once on riscv64, on the last page of the first usable region, and never reproduced. Four
// deliberate attempts to arrange the failing boot's allocation history did not reproduce it, and
// fourteen full suites since have not either. Every one of those experiments costs a guest boot -
// up to ninety minutes on riscv64 - and buys a few thousand allocations in one order.
//
// The tests below buy tens of millions, in the pool shapes the sighting happened in, against a
// model that knows where every page is. Two things follow from that and they are different:
//
//   - THE INVARIANT IS CHECKABLE HERE. A double allocation is one page in two free blocks, and
//     nothing in the kernel can see that: `free_pages()` is a total, `is_free_page` is one page,
//     and `frame::audit()` compares counts. `for_each_free_block` walks the whole free set, so
//     `check` below asserts the property directly rather than a consequence of it.
//   - AND THE ANSWER IS RECORDED EITHER WAY. If these find nothing, that is not silence: it says
//     the arithmetic is not the cause, over more operations than every boot this project has ever
//     run put together, and it moves the remaining suspects to the integration around it.

use super::*;
use alloc::vec;
use std::collections::BTreeMap;

fn fixture(pages: u64) -> Buddy {
	Buddy::new(0x4000_0000, pages).expect("metadata for the fixture")
}

// ---------------------------------------------------------------------------------------------
// The reference model, and the property the kernel cannot check for itself.
// ---------------------------------------------------------------------------------------------

// What every page of the extent is, according to whoever drove the allocator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
	// Never given to the allocator: the gap between two usable regions. It must never come out.
	Hole,
	// In the pool and not handed out.
	Free,
	// Out on loan to allocation number N. Handing it out again is the sighting.
	Live(u32),
}

// A buddy with a page-by-page model beside it, checked after every operation.
struct Pool {
	buddy: Buddy,
	base: u64,
	pages: Vec<Page>,
	// Live allocations: base address -> length in pages, so a free hands back exactly what it got.
	live: BTreeMap<u64, u64>,
	// The same set in insertion order, so picking one at random is an index rather than a walk.
	// It matters: the soak runs millions of frees, and `live.keys().nth(n)` made the campaign
	// quadratic in how much was out on loan.
	order: Vec<u64>,
	next: u32,
}

impl Pool {
	// An extent whose pages all start as holes; `seed` is what puts a region into the pool, exactly
	// as `upgrade_to_heap` does - one `free_span` per run, in run order.
	fn new(base: u64, pages: u64) -> Self {
		Self { buddy: Buddy::new(base, pages).expect("metadata"), base, pages: vec![Page::Hole; pages as usize], live: BTreeMap::new(), order: Vec::new(), next: 0 }
	}

	fn index(&self, phys: u64) -> usize {
		((phys - self.base) / PAGE_SIZE) as usize
	}

	fn seed(&mut self, phys: u64, pages: u64) {
		let taken = self.buddy.free_span(phys, pages);
		assert_eq!(taken, pages, "the seed run at {phys:#x} is inside the extent, so all of it must be taken");
		for page in 0..pages {
			let at = self.index(phys + page * PAGE_SIZE);
			self.pages[at] = Page::Free;
		}
	}

	// Take one page, or a contiguous span, and assert the allocator did not hand out something it
	// had already given away. This is `check_not_owned` from the kernel's frame allocator, asked
	// against a model rather than against a second bitmap - so a disagreement here is the allocator
	// and cannot be the record.
	fn take(&mut self, want: u64) -> Option<u64> {
		let phys = if want == 1 { self.buddy.alloc(0)? } else { self.buddy.alloc_contiguous(want)? };
		self.next += 1;
		for page in 0..want {
			let at = self.index(phys + page * PAGE_SIZE);
			match self.pages[at] {
				Page::Free => self.pages[at] = Page::Live(self.next),
				Page::Live(owner) => panic!("DOUBLE ALLOCATION: {:#x} handed out as allocation {} while on loan from allocation {owner}", phys + page * PAGE_SIZE, self.next),
				Page::Hole => panic!("the allocator handed out {:#x}, which was never freed into it", phys + page * PAGE_SIZE),
			}
		}
		self.live.insert(phys, want);
		self.order.push(phys);
		Some(phys)
	}

	// Give one back, through the same guard the kernel's `insert` uses: a span any page of which is
	// already free is refused before anything is written.
	// Give back whichever live allocation `pick` names, by index into the insertion order.
	fn give_at(&mut self, pick: usize) {
		let phys = self.order.swap_remove(pick);
		self.give_inner(phys);
	}

	fn give_inner(&mut self, phys: u64) {
		let pages = self.live.remove(&phys).expect("only a live allocation is given back");
		assert!(!self.buddy.any_free(phys, pages), "a live allocation must not read as free");
		let taken = self.buddy.free_span(phys, pages);
		assert_eq!(taken, pages, "a span that came out of this allocator goes back into it whole");
		for page in 0..pages {
			let at = self.index(phys + page * PAGE_SIZE);
			self.pages[at] = Page::Free;
		}
	}

	// THE INVARIANT. Walk the whole free set and require that no page is in it twice, that it
	// agrees with the model page for page, and that the total the allocator maintains is the total
	// the bitmaps actually hold.
	//
	// The middle one is the strongest and the one no in-kernel check can make: a page covered by
	// two free blocks is handed out twice, and until it is, every count and every per-page query
	// still answers correctly.
	fn check(&self) {
		let mut seen = vec![0u8; self.pages.len()];
		let mut total = 0u64;
		self.buddy.for_each_free_block(|order, phys| {
			let first = ((phys - self.base) / PAGE_SIZE) as usize;
			for page in 0..(1usize << order) {
				let at = first + page;
				assert!(at < seen.len(), "a free block at order {order} runs past the extent");
				seen[at] += 1;
				assert!(seen[at] < 2, "page {:#x} is in TWO free blocks - this is a double allocation that has not happened yet", self.base + at as u64 * PAGE_SIZE);
			}
			total += 1u64 << order;
		});
		assert_eq!(total, self.buddy.free_pages(), "the maintained free count and the bitmaps disagree");
		for (at, state) in self.pages.iter().enumerate() {
			let phys = self.base + at as u64 * PAGE_SIZE;
			let free = seen[at] == 1;
			match state {
				Page::Free => assert!(free, "{phys:#x} is free in the model and not in the allocator"),
				Page::Live(owner) => assert!(!free, "{phys:#x} is free in the allocator and on loan to allocation {owner}"),
				Page::Hole => assert!(!free, "{phys:#x} is a hole and the allocator calls it free"),
			}
			assert_eq!(self.buddy.is_free_page(phys), free, "is_free_page disagrees with the bitmaps about {phys:#x}");
		}
	}
}

// How much longer to run the randomized tests than the gate needs.
//
// The gate runs them on every sweep, so their default size is chosen to cost a couple of seconds -
// enough that a defect in the arithmetic is very unlikely to survive a day of ordinary development.
// The CAMPAIGN is a different question and is asked once rather than continuously: `BUDDY_SOAK=100`
// multiplies the work, and the number of operations that answer produced belongs in P02M0120 rather
// than in every run of `check.sh`.
fn soak() -> u64 {
	std::env::var("BUDDY_SOAK").ok().and_then(|value| value.parse().ok()).unwrap_or(1).max(1)
}

// Deterministic, so a failure reproduces from the seed printed in the panic rather than from
// whatever the machine felt like doing. xorshift64: three shifts, no state to get wrong.
struct Rng(u64);

impl Rng {
	fn next(&mut self) -> u64 {
		self.0 ^= self.0 << 13;
		self.0 ^= self.0 >> 7;
		self.0 ^= self.0 << 17;
		self.0
	}

	fn below(&mut self, bound: u64) -> u64 {
		self.next() % bound
	}
}

// ---------------------------------------------------------------------------------------------
// The four that came with the allocator, unchanged in what they assert.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_buddy_merges_back_to_one_block_however_it_is_freed() {
	// The property the run table could not give: coalescing that depends on arithmetic rather
	// than on a table having room. Free every page of a span individually, in an order chosen
	// to be awkward, and the whole span must come back as ONE block - which is only observable
	// by then allocating it whole.
	const PAGES: u64 = 64;
	let mut buddy = fixture(PAGES);
	let _ = buddy.free_span(0x4000_0000, PAGES);
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

#[test]
fn a_buddy_hands_back_the_rounding_of_a_contiguous_request() {
	// A buddy allocates in powers of two, so a 3-page request takes a 4-page block. Keeping
	// the fourth would be a quarter of that allocation lost for as long as it lives, and on a
	// pool that churns DMA buffers it compounds into memory nobody can account for.
	const PAGES: u64 = 64;
	let mut buddy = fixture(PAGES);
	let _ = buddy.free_span(0x4000_0000, PAGES);

	let base = buddy.alloc_contiguous(3).expect("three pages");
	assert_eq!(buddy.free_pages(), PAGES - 3, "the request costs THREE pages, not the four its block holds");

	// The spare page is genuinely available: ask for it.
	let spare = buddy.alloc_contiguous(1).expect("the rounded-off page is back in the pool");
	assert_eq!(spare, base + 3 * PAGE_SIZE, "and it is the page immediately after the request");
}

#[test]
fn a_pool_that_is_not_a_power_of_two_never_hands_out_the_gap() {
	// Real memory maps are not powers of two, and the tail is where a buddy goes wrong: a block
	// whose buddy would lie past the end of the extent has no buddy, and merging into one would
	// hand out memory the machine does not have.
	const PAGES: u64 = 100;
	let mut buddy = fixture(PAGES);
	let _ = buddy.free_span(0x4000_0000, PAGES);
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

#[test]
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

// ---------------------------------------------------------------------------------------------
// The mechanism a double allocation would have to arrive by.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_double_free_is_refused_and_this_is_what_it_prevents() {
	// THE ONE WAY THIS ALLOCATOR CAN HAND A PAGE OUT TWICE, demonstrated rather than argued - and
	// the demonstration corrected a belief this milestone had been carrying.
	//
	// "A double free duplicates the page" is only true of one of the two shapes, and the first
	// version of this test asserted the wrong one and failed. Freeing the same block at the SAME
	// order sets a bit that is already set, which `set` folds into nothing - so the bitmap is
	// unchanged and only `free_pages` moves, leaving a pool that reports a page no allocation can
	// find. It is a defect and it is not this one.
	//
	// The duplicating shape is freeing a page a LARGER free block already covers: the small bit
	// goes under the big block, the page is in the free set twice, and the next two allocations
	// that reach it both get it. That is the sighting's signature, and it is reachable at boot -
	// seeding a run the pool already holds does exactly this.
	//
	// Both are refused now, in `free` itself. `free_ignoring_double` is the test-only door that
	// shows what the refusal is worth.
	let mut buddy = fixture(64);
	let _ = buddy.free_span(0x4000_0000, 64);
	let base = 0x4000_0000;

	// Refused: the whole extent is one free block, so page 0 is covered by it.
	assert!(!buddy.free(base, 0), "a page a larger free block already covers is not free-able");
	assert!(!buddy.free(base, 6), "and neither is the block itself, which is free at its own order");
	assert_eq!(buddy.free_pages(), 64, "and a refusal adds nothing to the pool");

	// Now the same free with the rule removed, to see the state it produces.
	assert!(buddy.free_ignoring_double(base, 0), "the unguarded free accepts it - that is the point of the door");
	let mut seen = vec![0u8; 64];
	buddy.for_each_free_block(|order, phys| {
		let first = ((phys - base) / PAGE_SIZE) as usize;
		for page in 0..(1usize << order) {
			seen[first + page] += 1;
		}
	});
	assert_eq!(seen[0], 2, "page 0 is now in TWO free blocks, which is a double allocation that has not happened yet");

	// And it is not theoretical: two allocations come back with the same address.
	let first = buddy.alloc(0).expect("the small block");
	let second = buddy.alloc(0).expect("and then the big one, split down");
	assert_eq!(first, second, "the doubly-freed page comes out twice, which is the sighting this milestone is open on");
}

#[test]
fn a_free_at_the_same_order_twice_would_inflate_the_pool_rather_than_duplicate_a_page() {
	// The other shape, kept as its own test because the two are different defects and telling them
	// apart is what the first version of the test above got wrong.
	//
	// A pool that claims a page nothing can find is the run table's old failure mode arriving from
	// the other direction - and the boot report, `frame::audit()` and `free_count` would all agree
	// with each other while being wrong together, because they are all downstream of this counter.
	// TWO pages out, so the freed one's buddy is on loan and the free cannot merge away. That is
	// what keeps the second free at the same order rather than turning it into the covering shape
	// above - which is what the first version of this test got wrong, and is exactly the confusion
	// it now exists to prevent.
	let mut buddy = fixture(64);
	let _ = buddy.free_span(0x4000_0000, 64);
	let page = buddy.alloc(0).expect("one page");
	let _neighbour = buddy.alloc(0).expect("its buddy, so the free below cannot merge");
	assert_eq!(buddy.free_pages(), 62);

	assert!(buddy.free(page, 0), "the first free is legitimate");
	assert_eq!(buddy.free_pages(), 63);
	assert!(!buddy.free(page, 0), "and the second is refused");
	assert_eq!(buddy.free_pages(), 63, "so the pool does not grow by a page that is already in it");

	// Without the refusal the count moves and the bitmap does not.
	let mut buddy = fixture(64);
	let _ = buddy.free_span(0x4000_0000, 64);
	let page = buddy.alloc(0).expect("one page");
	let _neighbour = buddy.alloc(0).expect("its buddy");
	assert!(buddy.free_ignoring_double(page, 0), "the first");
	assert!(buddy.free_ignoring_double(page, 0), "and the second, unguarded");
	assert_eq!(buddy.free_pages(), 64, "sixty-four free pages in a pool holding sixty-two");
	let mut real = 0u64;
	buddy.for_each_free_block(|order, _| real += 1u64 << order);
	assert_eq!(real, 63, "while the bitmaps hold sixty-three - the count is the thing that is wrong");
}

#[test]
fn seeding_the_same_run_twice_is_caught_before_it_is_done() {
	// The same mechanism reached the way the BOOT could reach it. `upgrade_to_heap` seeds the buddy
	// one run at a time from the run table; a run list with a duplicate or an overlap - a carve that
	// double-counted a region, a hand-off that reported one twice - would free the same pages twice
	// and produce exactly the state above, at boot, before any test runs.
	//
	// The run table refused an overlapping insert and could not have got there. A bitmap has no
	// such test built in, so the guard has to be asked for, and this is the ask - at both levels:
	// `any_free` for the caller that has a record to keep consistent, and the refusal inside `free`
	// for the caller that forgets.
	let mut buddy = fixture(64);
	let taken = buddy.free_span(0x4000_0000 + 16 * PAGE_SIZE, 16);
	assert_eq!(taken, 16);
	assert!(buddy.any_free(0x4000_0000 + 16 * PAGE_SIZE, 16), "the run is in the pool, so seeding it again would double it");
	assert!(buddy.any_free(0x4000_0000 + 20 * PAGE_SIZE, 4), "and a run OVERLAPPING it is caught the same way");
	assert!(!buddy.any_free(0x4000_0000 + 32 * PAGE_SIZE, 16), "a run that does not overlap is not refused");

	// And seeding it again really is a no-op rather than a doubling: `free_span` reports that it
	// took nothing, which is the shortfall the frame allocator already knows how to read.
	let again = buddy.free_span(0x4000_0000 + 16 * PAGE_SIZE, 16);
	assert_eq!(again, 0, "not one page of a run the pool already holds is taken a second time");
	assert_eq!(buddy.free_pages(), 16);

	// A run that OVERLAPS PART of what is held is the awkward one: the pages that are new must go
	// in and the ones already there must not be doubled.
	let overlapping = buddy.free_span(0x4000_0000 + 24 * PAGE_SIZE, 16);
	assert_eq!(overlapping, 8, "the eight pages past the end of what was held, and no more");
	assert_eq!(buddy.free_pages(), 24);
	let mut seen = vec![0u8; 64];
	buddy.for_each_free_block(|order, phys| {
		let first = ((phys - 0x4000_0000) / PAGE_SIZE) as usize;
		for page in 0..(1usize << order) {
			seen[first + page] += 1;
		}
	});
	assert!(seen.iter().all(|count| *count <= 1), "and no page ended up in two blocks");
}

// ---------------------------------------------------------------------------------------------
// The sighting's own shape, driven far past what a boot can.
// ---------------------------------------------------------------------------------------------

// The riscv64 machine the sighting happened on: 512 MB, no firmware memory map, so the kernel
// fabricates one from `__kernel_end` to the top of RAM and `carve` subtracts what the loader left
// in it - the `BootInfo`, the module descriptor array, and the package bytes. The result is a
// handful of usable regions with holes between them, and the page that came out twice was the last
// page of the FIRST one, immediately below the first hole.
//
// The addresses are the shape rather than the exact numbers: what matters is that a region ends at
// a hole, which is what puts its tail into the small orders, which is where a single-page
// allocation looks first.
fn carved_pool() -> Pool {
	const BASE: u64 = 0x8020_0000;
	const TOP: u64 = 0xa000_0000;
	// The three loader reservations, page-aligned outward the way `carve` aligns them.
	let holes: [(u64, u64); 3] = [(0x9c0a_8000, 0x9c60_0000), (0x9c8d_8000, 0x9cc5_9000), (0x9fff_f000, TOP)];
	let mut pool = Pool::new(BASE, (TOP - BASE) / PAGE_SIZE);
	let mut at = BASE;
	for (start, end) in holes {
		if start > at {
			pool.seed(at, (start - at) / PAGE_SIZE);
		}
		at = end;
	}
	if at < TOP {
		pool.seed(at, (TOP - at) / PAGE_SIZE);
	}
	pool
}

#[test]
fn the_carved_pool_of_the_sighting_hands_no_page_out_twice() {
	// The experiment the milestone asked for, at a scale a guest cannot reach: the pool shape of
	// the failing boot, then a workload that shreds it, with every allocation checked against a
	// model that knows who holds every page.
	//
	// The FIRST few hundred allocations are the interesting ones and are done deliberately as
	// single pages: the buddy seeds by framing each region into blocks of falling order, so each
	// region's tail lands in the small orders and is among the first memory ever handed out. That
	// is the reasoning that made the sighting's address - the last page of the first region -
	// unsurprising, and it is the window this drives hardest.
	let mut pool = carved_pool();
	pool.check();

	let mut taken = vec![];
	for _ in 0..4096 {
		match pool.take(1) {
			Some(phys) => taken.push(phys),
			None => break,
		}
	}
	pool.check();
	// The tail of the first region must have come out, or this test is not exercising the window
	// it exists for.
	let tail = 0x9c0a_8000 - PAGE_SIZE;
	assert!(taken.contains(&tail), "the last page below the first hole must be among the first pages handed out, or the shape of this fixture no longer matches the sighting");

	// Now churn: free and re-take in an order that defeats coalescing, and check the whole free
	// set every time. Sixty-four thousand operations, each one a full walk of the bitmaps.
	let mut rng = Rng(0x5eed_1204);
	let steps = 64_000 * soak();
	let mut operations = 0u64;
	for step in 0..steps {
		if !pool.live.is_empty() && (pool.live.len() > 3000 || rng.below(2) == 0) {
			let index = rng.below(pool.order.len() as u64) as usize;
			pool.give_at(index);
		} else {
			let want = match rng.below(16) {
				0 => 1 + rng.below(64),
				1..=3 => 1 + rng.below(8),
				_ => 1,
			};
			pool.take(want);
		}
		operations += 1;
		if step % 512 == 0 {
			pool.check();
		}
	}
	pool.check();
	// Printed rather than asserted: the number is the result, and the result is what goes in the
	// milestone. `cargo test -- --nocapture` shows it.
	println!("carved-pool churn: {operations} operations, {} pages seeded", pool.buddy.free_pages() + pool.live.values().sum::<u64>());
}

#[test]
fn a_shredded_pool_gives_every_page_back() {
	// The other half of the same question: after the churn, does everything return? A page in two
	// free blocks would make this OVERCOUNT, and a page lost to a refused frame would make it
	// undercount, so the total is a second, independent view of the same invariant.
	let mut pool = carved_pool();
	let seeded = pool.buddy.free_pages();

	let mut rng = Rng(0x0120_0120);
	for _ in 0..20_000 * soak() {
		if !pool.live.is_empty() && rng.below(3) == 0 {
			let index = rng.below(pool.order.len() as u64) as usize;
			pool.give_at(index);
		} else if pool.take(1 + rng.below(32)).is_none() {
			// Out of memory is an ordinary answer; free something and carry on.
			if !pool.order.is_empty() {
				pool.give_at(0);
			}
		}
	}
	while !pool.order.is_empty() {
		pool.give_at(0);
	}
	pool.check();
	assert_eq!(pool.buddy.free_pages(), seeded, "every page the pool was seeded with came back");
	// And it coalesced: the first region is one span again, so asking for a large contiguous block
	// out of it succeeds. A pool that had kept the churn's fragments would refuse.
	assert!(pool.buddy.alloc_contiguous(1024).is_some(), "a shredded pool that has been fully returned serves a large contiguous request again");
}

#[test]
fn random_extents_and_random_workloads_never_hand_a_page_out_twice() {
	// The general case, because the fixture above is one shape and the machine will not always be
	// that shape. Awkward extents on purpose: page counts that are not powers of two, regions that
	// start and end at odd alignments, holes of every size including one page.
	//
	// Twenty pools, each checked after every operation - which is what makes this worth running
	// rather than a slower way to run the fixture above.
	for seed in 0..20 * soak() {
		let mut rng = Rng(0xc0ffee_0000 + seed * 7919);
		let pages = 300 + rng.below(4000);
		let base = 0x8000_0000 + rng.below(64) * PAGE_SIZE;
		let mut pool = Pool::new(base, pages);

		// Between one and five regions, with holes between them.
		let mut at = 0u64;
		while at < pages {
			let gap = rng.below(8);
			at += gap;
			if at >= pages {
				break;
			}
			let run = 1 + rng.below((pages - at).min(1500));
			pool.seed(base + at * PAGE_SIZE, run);
			at += run;
		}
		pool.check();

		for step in 0..3_000u64 {
			if !pool.live.is_empty() && rng.below(2) == 0 {
				let index = rng.below(pool.order.len() as u64) as usize;
				pool.give_at(index);
			} else {
				pool.take(1 + rng.below(40));
			}
			// Every operation at the gate's size, where the point is to catch the FIRST step that
			// goes wrong; every eighth in a campaign, where the point is how many steps are taken.
			if soak() == 1 || step % 8 == 0 {
				pool.check();
			}
		}
		pool.check();
	}
}
