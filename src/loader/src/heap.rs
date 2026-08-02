// A heap for the loader, so the filesystem crates can be linked.
//
// LiberFS and FAT are `no_std` but not allocation-free: between them they use `Vec`, `Box` and
// `String` in nearly two hundred places, because a directory walk and a file read are naturally
// sized at runtime. The loader had no allocator at all, which is why reading a real filesystem
// needed this before it needed a block device.
//
// It is a bump allocator over pages taken from the firmware, and it never reuses memory. That is
// the right shape here rather than a shortcut: the loader runs once, reads a handful of files and
// stops existing at `ExitBootServices`, so a free list would cost code and bugs to reclaim memory
// nothing lives long enough to need. When an arena runs out, another is taken - so a large kernel
// costs more arenas rather than a failure.
//
// It is deliberately NOT available after `ExitBootServices`: the firmware's allocator is gone by
// then, so `alloc` past that point aborts rather than quietly handing out memory the loader no
// longer owns.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr;

use crate::uefi::BootServices;

// Each arena taken from the firmware. Large enough that reading a kernel does not chain dozens of
// them, small enough not to fail on a machine with modest firmware memory.
const ARENA_BYTES: usize = 8 * 1024 * 1024;

struct Bump {
	// The firmware's boot services, or null once they are gone.
	services: *mut BootServices,
	// The current arena and how far into it we are.
	base: usize,
	next: usize,
	end: usize,
}

// The loader is single-threaded - the firmware calls `efi_main` on one processor and nothing here
// starts another - so the allocator needs no lock, only interior mutability.
struct Heap(UnsafeCell<Bump>);

unsafe impl Sync for Heap {}

#[global_allocator]
static HEAP: Heap = Heap(UnsafeCell::new(Bump { services: ptr::null_mut(), base: 0, next: 0, end: 0 }));

// Hand the allocator the firmware's boot services. Must be called before the first allocation and
// before `ExitBootServices`; `retire` takes them away again.
pub(crate) fn init(services: *mut BootServices) {
	unsafe { (*HEAP.0.get()).services = services };
}

// Give up the firmware allocator. After this the heap cannot grow, so anything still holding an
// allocation must have finished with it - which is why the hand-off copies what the kernel needs
// into pages of its own before calling this.
pub(crate) fn retire() {
	let bump = HEAP.0.get();
	unsafe {
		(*bump).services = ptr::null_mut();
		(*bump).next = (*bump).end;
	}
}

unsafe impl GlobalAlloc for Heap {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		let bump = self.0.get();
		unsafe {
			let aligned = ((*bump).next + layout.align() - 1) & !(layout.align() - 1);
			if aligned + layout.size() <= (*bump).end {
				(*bump).next = aligned + layout.size();
				return aligned as *mut u8;
			}
			// This arena is full. Take another, sized for the request when the request is the
			// larger - a file bigger than an arena must still be readable.
			let services = (*bump).services;
			if services.is_null() {
				return ptr::null_mut();
			}
			let want = layout.size() + layout.align();
			let bytes = if want > ARENA_BYTES { want } else { ARENA_BYTES };
			let pages = bytes.div_ceil(crate::PAGE_SIZE as usize);
			let Some(addr) = crate::alloc_pages(services, pages) else {
				return ptr::null_mut();
			};
			(*bump).base = addr as usize;
			(*bump).next = addr as usize;
			(*bump).end = addr as usize + pages * crate::PAGE_SIZE as usize;
			let aligned = ((*bump).next + layout.align() - 1) & !(layout.align() - 1);
			(*bump).next = aligned + layout.size();
			aligned as *mut u8
		}
	}

	// Nothing is reclaimed. See the note at the top: the loader does not live long enough for
	// reuse to pay for the code that would do it.
	unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}
