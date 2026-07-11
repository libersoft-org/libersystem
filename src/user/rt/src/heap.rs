//! The userspace heap: the global allocator every program that needs `alloc`
//! links through rt.
//!
//! On the first allocation the heap maps a fixed-size MemoryObject into the
//! process (memory_object_create + memory_map), then hands out memory from a
//! linked-list first-fit free list - the same design as the kernel heap, minus
//! coalescing (correct, but can fragment; good enough for the foundation). The
//! init is lazy, so a program that never allocates never maps a heap and the
//! existing heap-free services are unaffected.

use crate::{sys_is_err, syscall, SYS_MEMORY_MAP, SYS_MEMORY_OBJECT_CREATE};
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::mem;
use core::ops::{Deref, DerefMut};
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

// Per-process heap size, mapped on first use. The heap grows by another such region
// whenever an allocation cannot be satisfied, so a program that never allocates maps
// nothing and a small program stays at one region, while a large consumer (e.g.
// StorageService seeding the system volume from a multi-megabyte factory archive) grows
// on demand instead of every process reserving a big heap up front.
const HEAP_SIZE: usize = 1024 * 1024; // 1 MB
const PAGE_SIZE: usize = 4096;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::new();

// A node in the free list, stored in-place at the start of each free block.
struct FreeRegion {
	size: usize,
	next: Option<&'static mut FreeRegion>,
}

impl FreeRegion {
	const fn new(size: usize) -> FreeRegion {
		FreeRegion { size, next: None }
	}

	fn start_addr(&self) -> usize {
		self as *const FreeRegion as usize
	}

	fn end_addr(&self) -> usize {
		self.start_addr() + self.size
	}
}

struct Heap {
	head: FreeRegion,
	initialized: bool,
}

impl Heap {
	const fn empty() -> Heap {
		Heap { head: FreeRegion::new(0), initialized: false }
	}

	// Map the backing region on first use; after this the free list owns
	// [base, base + HEAP_SIZE). A failed map leaves the heap empty (allocs return
	// null, which the alloc machinery turns into an abort).
	fn ensure_init(&mut self) {
		if self.initialized {
			return;
		}
		self.initialized = true;
		unsafe {
			let handle = syscall(SYS_MEMORY_OBJECT_CREATE, HEAP_SIZE as u64, 0, 0, 0);
			if sys_is_err(handle) {
				return;
			}
			let base = syscall(SYS_MEMORY_MAP, handle, 0, 0, 0);
			if sys_is_err(base) {
				return;
			}
			self.add_free_region(base as usize, HEAP_SIZE);
		}
	}

	// Map another backing region and add it to the free list when no existing region
	// fits a request. The new region is at least HEAP_SIZE, or large enough for this one
	// allocation (plus a node header), page-rounded. Returns false if the map fails, in
	// which case the allocation returns null (which the alloc machinery turns into an
	// abort).
	fn grow(&mut self, need: usize) -> bool {
		let size: usize = align_up(need.saturating_add(mem::size_of::<FreeRegion>()).max(HEAP_SIZE), PAGE_SIZE);
		unsafe {
			let handle = syscall(SYS_MEMORY_OBJECT_CREATE, size as u64, 0, 0, 0);
			if sys_is_err(handle) {
				return false;
			}
			let base = syscall(SYS_MEMORY_MAP, handle, 0, 0, 0);
			if sys_is_err(base) {
				return false;
			}
			self.add_free_region(base as usize, size);
		}
		true
	}

	// SAFETY: `addr` must be valid for writes and large enough to hold a node.
	unsafe fn add_free_region(&mut self, addr: usize, size: usize) {
		unsafe {
			let mut region = FreeRegion::new(size);
			region.next = self.head.next.take();
			let region_ptr = addr as *mut FreeRegion;
			region_ptr.write(region);
			self.head.next = Some(&mut *region_ptr);
		}
	}

	// Find the first free region that fits, unlink it, and return it with the
	// aligned allocation start.
	fn find_region(&mut self, size: usize, align: usize) -> Option<(&'static mut FreeRegion, usize)> {
		let mut current = &mut self.head;
		while let Some(ref mut region) = current.next {
			if let Ok(alloc_start) = Heap::alloc_from_region(region, size, align) {
				let next = region.next.take();
				let ret = Some((current.next.take().unwrap(), alloc_start));
				current.next = next;
				return ret;
			}
			current = current.next.as_mut().unwrap();
		}
		None
	}

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

	fn size_align(layout: Layout) -> Option<(usize, usize)> {
		let layout = layout.align_to(mem::align_of::<FreeRegion>()).ok()?.pad_to_align();
		let size = layout.size().max(mem::size_of::<FreeRegion>());
		Some((size, layout.align()))
	}
}

// A spin-locked heap: the global allocator must be Sync and a program may be
// multi-threaded, so the free list is guarded by a simple test-and-set lock.
struct LockedHeap {
	locked: AtomicBool,
	heap: UnsafeCell<Heap>,
}

unsafe impl Sync for LockedHeap {}

impl LockedHeap {
	const fn new() -> LockedHeap {
		LockedHeap { locked: AtomicBool::new(false), heap: UnsafeCell::new(Heap::empty()) }
	}

	fn lock(&self) -> HeapGuard<'_> {
		while self.locked.swap(true, Ordering::Acquire) {
			core::hint::spin_loop();
		}
		HeapGuard { owner: self }
	}
}

struct HeapGuard<'a> {
	owner: &'a LockedHeap,
}

impl Deref for HeapGuard<'_> {
	type Target = Heap;
	fn deref(&self) -> &Heap {
		unsafe { &*self.owner.heap.get() }
	}
}

impl DerefMut for HeapGuard<'_> {
	fn deref_mut(&mut self) -> &mut Heap {
		unsafe { &mut *self.owner.heap.get() }
	}
}

impl Drop for HeapGuard<'_> {
	fn drop(&mut self) {
		self.owner.locked.store(false, Ordering::Release);
	}
}

unsafe impl GlobalAlloc for LockedHeap {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		unsafe {
			let mut heap = self.lock();
			heap.ensure_init();
			// A degenerate layout (align/size overflow) can never be satisfied: return
			// null (the GlobalAlloc contract) rather than panicking the process.
			let (size, align) = match Heap::size_align(layout) {
				Some(sa) => sa,
				None => return ptr::null_mut(),
			};
			let region = match heap.find_region(size, align) {
				Some(found) => Some(found),
				// no region fits: map another and retry once.
				None => {
					if heap.grow(size) {
						heap.find_region(size, align)
					} else {
						None
					}
				}
			};
			match region {
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
			// A layout `alloc` would have rejected was never handed out, so there is
			// nothing to reclaim.
			if let Some((size, _)) = Heap::size_align(layout) {
				self.lock().add_free_region(ptr as usize, size);
			}
		}
	}
}

const fn align_up(value: usize, align: usize) -> usize {
	(value + align - 1) & !(align - 1)
}
