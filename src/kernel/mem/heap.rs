// Kernel heap: enables `alloc` (Box, Vec, ...).
//
// A dedicated higher-half virtual region is backed by physical frames mapped in
// on init, and a linked-list first-fit allocator hands out memory within it.
// Freed blocks are pushed back onto the free list and reused (no coalescing in
// M1, which is correct but can fragment - good enough for the foundation).

use core::alloc::{GlobalAlloc, Layout};
use core::mem;
use core::ptr;

use crate::arch::paging;
use crate::mem::frame;
use crate::mem::frame::PAGE_SIZE;
use crate::sync::SpinLock;

// Heap virtual window: well clear of both the HHDM and the kernel image.
const HEAP_START: u64 = 0xffff_e000_0000_0000;
const HEAP_SIZE: u64 = 1024 * 1024; // 1 MiB

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

// Map the heap window frame-by-frame, then hand the region to the allocator.
pub fn init() {
	let mut virt = HEAP_START;
	let end = HEAP_START + HEAP_SIZE;
	while virt < end {
		let phys = frame::allocate().expect("out of frames: kernel heap");
		paging::map_page(virt, phys, paging::WRITABLE);
		virt += PAGE_SIZE;
	}
	unsafe { ALLOCATOR.lock().init(HEAP_START as usize, HEAP_SIZE as usize) };
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
	unsafe fn add_free_region(&mut self, addr: usize, size: usize) {
		unsafe {
			assert_eq!(align_up(addr, mem::align_of::<FreeRegion>()), addr);
			assert!(size >= mem::size_of::<FreeRegion>());

			let mut region = FreeRegion::new(size);
			region.next = self.head.next.take();
			let region_ptr = addr as *mut FreeRegion;
			region_ptr.write(region);
			self.head.next = Some(&mut *region_ptr);
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
			match heap.find_region(size, align) {
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
