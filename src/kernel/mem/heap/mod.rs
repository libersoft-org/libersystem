// Kernel heap: enables `alloc` (Box, Vec, ...).
//
// A dedicated higher-half virtual region is backed by physical frames mapped in
// on init, and a linked-list first-fit allocator hands out memory within it.
// The free list is kept sorted by address and freed blocks are coalesced with
// their immediate neighbours, so contiguous free memory is merged back into one
// region - this keeps the heap from fragmenting under churn (without it a long
// run of allocations/frees could leave no single block big enough for a large
// contiguous request - e.g. a 16 kB kernel thread stack - even with plenty of
// total free space).

use core::alloc::{GlobalAlloc, Layout};
use core::mem;
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::paging;
use crate::mem::frame;
use crate::mem::frame::PAGE_SIZE;
use crate::sync::SpinLock;

// Heap virtual window: well clear of both the HHDM and the kernel image. The heap
// starts at one region and maps another whenever an allocation cannot be satisfied,
// so it is never a fixed budget - the physical frame pool is the real bound.
//
// x86_64 / aarch64 have a 48-bit VA space; riscv64's Sv39 is 39-bit, so its heap sits
// in the Sv39 high canonical half (past the 8 GiB direct map at KERNEL_VA_OFFSET).
#[cfg(not(target_arch = "riscv64"))]
const HEAP_START: u64 = 0xffff_e000_0000_0000;
#[cfg(target_arch = "riscv64")]
const HEAP_START: u64 = 0xffff_ffd0_0000_0000;
const HEAP_REGION: u64 = 2 * 1024 * 1024; // the initial size and the growth unit

// How far the heap may grow.
//
// The bump had no bound at all, which read as "the frame pool is the real limit" - true for
// physical memory and false for the page tables above it. Every top-level entry the window
// crosses has to exist BEFORE the first address space is created, because address spaces copy
// the kernel half rather than share it and a later entry is invisible to the ones already made.
// A window that cannot be enumerated cannot be reserved, so it gets a size.
//
// 512 GiB is one PML4 entry on x86_64 and aarch64. riscv64's Sv39 top level covers 1 GiB, so
// the window is stated in whole entries there and kept well inside the 64 GiB gap between the
// heap line and the kernel mmap window above it.
#[cfg(not(target_arch = "riscv64"))]
pub(crate) const HEAP_WINDOW: u64 = 512 * 1024 * 1024 * 1024;
#[cfg(target_arch = "riscv64")]
pub(crate) const HEAP_WINDOW: u64 = 8 * 1024 * 1024 * 1024;

// The virtual base the next growth region maps at (bumped under the allocator lock).
static NEXT_REGION: AtomicU64 = AtomicU64::new(0);

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

// Map the heap window frame-by-frame, then hand the region to the allocator.
pub fn init() {
	let mut virt = HEAP_START;
	let end = HEAP_START + HEAP_REGION;
	while virt < end {
		let phys = frame::allocate().expect("out of frames: kernel heap");
		paging::map_page(virt, phys, paging::WRITABLE | paging::NO_EXECUTE);
		virt += PAGE_SIZE;
	}
	NEXT_REGION.store(end, Ordering::Relaxed);
	unsafe { ALLOCATOR.lock().init(HEAP_START as usize, HEAP_REGION as usize) };
}

// Give the whole heap window its top-level page-table entries, so no later growth creates one.
// Called at boot, before any address space exists to be copied from the kernel's.
pub fn reserve_window() {
	paging::reserve_kernel_top_level(HEAP_START, HEAP_WINDOW);
}

// Map another heap region (at least `at_least` bytes, in HEAP_REGION multiples) and
// hand it to the free list. Called under the allocator lock when an allocation finds
// no fitting region; false when the frame pool is exhausted (the real OOM).
fn grow(heap: &mut Heap, at_least: usize) -> bool {
	let bytes: u64 = (at_least as u64 + PAGE_SIZE).next_multiple_of(HEAP_REGION);
	let base: u64 = NEXT_REGION.fetch_add(bytes, Ordering::Relaxed);
	// Past the reserved window there are no top-level entries, and creating one here would be
	// invisible to every address space already made - a triple fault the next time one of them
	// is switched to. Refuse instead, and give the range back so the bump does not run away.
	if base + bytes > HEAP_START + HEAP_WINDOW {
		let _ = NEXT_REGION.compare_exchange(base + bytes, base, Ordering::Relaxed, Ordering::Relaxed);
		return false;
	}
	let mut virt = base;
	while virt < base + bytes {
		let phys = match frame::allocate() {
			Some(p) => p,
			// Out of frames partway through. Everything claimed so far has to go back, or
			// running out of memory would COST memory: the virtual range was claimed by the
			// bump above and the frames below `virt` are mapped into it, and a bare `return
			// false` handed the range to nobody and left those frames unreachable for the life
			// of the boot. The next grow would then start past a hole it can never use.
			//
			// Unwinding rather than reserving the range at the end, because the frames must be
			// returned either way - the ordering of the bump only decides whether the virtual
			// range leaks as well.
			None => {
				unwind(base, virt, base + bytes);
				return false;
			}
		};
		paging::map_page(virt, phys, paging::WRITABLE | paging::NO_EXECUTE);
		virt += PAGE_SIZE;
	}
	unsafe { heap.add_free_region(base as usize, bytes as usize) };
	true
}

// Give back a partial growth region: unmap every page from `base` up to (not including) `mapped`,
// free the frame each held, and release the virtual range.
//
// The range is released with a compare-exchange rather than a subtracting `fetch_sub`: only the
// caller that is still the LAST claimant may take it back. `grow` runs under the allocator lock so
// today there is no second claimant, but a rollback that assumes it is last is the kind of
// arithmetic that silently overlaps two regions the moment that stops being true. If the exchange
// fails, the virtual range stays claimed - address space, of which there is 2^48, rather than
// frames, of which there are not.
fn unwind(base: u64, mapped: u64, end: u64) {
	let mut virt = base;
	while virt < mapped {
		if let Some(phys) = paging::unmap_page(virt) {
			// SAFETY: the frame came from this growth's own allocation and has just been
			// unmapped from the only place it was ever mapped.
			unsafe { frame::deallocate(phys) };
		}
		virt += PAGE_SIZE;
	}
	let _ = NEXT_REGION.compare_exchange(end, base, Ordering::Relaxed, Ordering::Relaxed);
}

// The heap's totals: (total bytes mapped so far, bytes currently free), the free
// list summed under the lock - the walk stays short (regions coalesce).
pub fn stats() -> (u64, u64) {
	let heap = ALLOCATOR.lock();
	let mut free: usize = 0;
	let mut current = &heap.head;
	while let Some(ref next) = current.next {
		free += next.size;
		current = next;
	}
	(NEXT_REGION.load(Ordering::Relaxed) - HEAP_START, free as u64)
}

// A node in the free list, stored in-place at the start of each free block.
struct FreeRegion {
	size: usize,
	next: Option<&'static mut FreeRegion>,
}

impl FreeRegion {
	const fn new(size: usize) -> Self {
		Self { size, next: None }
	}

	fn start_addr(&self) -> usize {
		self as *const Self as usize
	}

	fn end_addr(&self) -> usize {
		self.start_addr() + self.size
	}
}

struct Heap {
	head: FreeRegion,
}

impl Heap {
	const fn empty() -> Self {
		Self { head: FreeRegion::new(0) }
	}

	// SAFETY: the caller must give an unused, mapped region [start, start+size)
	// and call this exactly once.
	unsafe fn init(&mut self, start: usize, size: usize) {
		unsafe {
			self.add_free_region(start, size);
		}
	}

	// SAFETY: `addr` must be valid for writes and large enough to hold a node.
	//
	// Inserts the freed block into the address-sorted free list, then coalesces it
	// with the immediately adjacent neighbours (right first, then left) so touching
	// free blocks are merged into one. The free list is always maximally coalesced,
	// so a single insert can merge with at most its left and right neighbour.
	unsafe fn add_free_region(&mut self, addr: usize, size: usize) {
		unsafe {
			assert_eq!(align_up(addr, mem::align_of::<FreeRegion>()), addr);
			assert!(size >= mem::size_of::<FreeRegion>());

			// Walk to the insertion point: `current` is the last node whose start
			// address is <= addr, so the new block belongs between `current` and
			// `current.next`. The list stays sorted by ascending start address.
			let mut current = &mut self.head;
			while let Some(ref next) = current.next {
				if next.start_addr() > addr {
					break;
				}
				current = current.next.as_mut().unwrap();
			}

			// Link the new node in between `current` and the rest of the list.
			let mut region = FreeRegion::new(size);
			region.next = current.next.take();
			let region_ptr = addr as *mut FreeRegion;
			region_ptr.write(region);
			current.next = Some(&mut *region_ptr);

			// Coalesce the new node with its right neighbour if they touch.
			let new_node = current.next.as_mut().unwrap();
			let merge_right = match &new_node.next {
				Some(next) => new_node.end_addr() == next.start_addr(),
				None => false,
			};
			if merge_right {
				let absorbed = new_node.next.take().unwrap();
				new_node.size += absorbed.size;
				new_node.next = absorbed.next.take();
			}

			// Coalesce `current` (the left neighbour) with the new node if they
			// touch. The dummy `head` lives in the kernel image, never adjacent to a
			// heap block, so this address check naturally skips it.
			if current.end_addr() == addr {
				let absorbed = current.next.take().unwrap();
				current.size += absorbed.size;
				current.next = absorbed.next.take();
			}
		}
	}

	// Find the first free region that fits `size`/`align`, remove it from the
	// list, and return it together with the allocation start address.
	fn find_region(&mut self, size: usize, align: usize) -> Option<(&'static mut FreeRegion, usize)> {
		let mut current = &mut self.head;
		while let Some(ref mut region) = current.next {
			if let Ok(alloc_start) = Self::alloc_from_region(region, size, align) {
				let next = region.next.take();
				let ret = Some((current.next.take().unwrap(), alloc_start));
				current.next = next;
				return ret;
			}
			current = current.next.as_mut().unwrap();
		}
		None
	}

	// Check whether `size`/`align` fit in `region`; if so return the aligned
	// allocation start. Any leftover at the end must be big enough to hold a node.
	fn alloc_from_region(region: &FreeRegion, size: usize, align: usize) -> Result<usize, ()> {
		let alloc_start = align_up(region.start_addr(), align);
		let alloc_end = alloc_start.checked_add(size).ok_or(())?;
		if alloc_end > region.end_addr() {
			return Err(());
		}
		let excess = region.end_addr() - alloc_end;
		if excess > 0 && excess < mem::size_of::<FreeRegion>() {
			return Err(());
		}
		Ok(alloc_start)
	}

	// Normalize a layout to something the free list can store and align.
	fn size_align(layout: Layout) -> (usize, usize) {
		let layout = layout.align_to(mem::align_of::<FreeRegion>()).expect("alignment overflow").pad_to_align();
		let size = layout.size().max(mem::size_of::<FreeRegion>());
		(size, layout.align())
	}
}

pub struct LockedHeap(SpinLock<Heap>);

impl LockedHeap {
	const fn empty() -> Self {
		Self(SpinLock::new(Heap::empty()))
	}

	fn lock(&self) -> crate::sync::SpinLockGuard<'_, Heap> {
		self.0.lock()
	}
}

unsafe impl GlobalAlloc for LockedHeap {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		unsafe {
			let (size, align) = Heap::size_align(layout);
			let mut heap = self.lock();
			let mut found = heap.find_region(size, align);
			if found.is_none() && grow(&mut *heap, size + align) {
				found = heap.find_region(size, align);
			}
			match found {
				Some((region, alloc_start)) => {
					let alloc_end = alloc_start.checked_add(size).expect("alloc overflow");
					let excess = region.end_addr() - alloc_end;
					if excess > 0 {
						heap.add_free_region(alloc_end, excess);
					}
					alloc_start as *mut u8
				}
				None => ptr::null_mut(),
			}
		}
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe {
			let (size, _) = Heap::size_align(layout);
			self.lock().add_free_region(ptr as usize, size);
		}
	}
}

const fn align_up(value: usize, align: usize) -> usize {
	(value + align - 1) & !(align - 1)
}

// Ask the allocator for memory and get back null on failure, instead of the abort that `Box` and
// `Vec` take on OOM. Deliberately test-only: production code that cannot get memory has nothing
// useful to do with the news, and the whole point of the growth path is that it is invisible.
#[cfg(test)]
pub fn try_alloc(layout: Layout) -> *mut u8 {
	unsafe { ALLOCATOR.alloc(layout) }
}

#[cfg(test)]
pub fn dealloc(pointer: *mut u8, layout: Layout) {
	unsafe { ALLOCATOR.dealloc(pointer, layout) }
}

#[cfg(test)]
mod tests;
