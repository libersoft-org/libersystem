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

use uefi::BootServices;

// Each arena taken from the firmware. Large enough that reading a kernel does not chain dozens of
// them, small enough not to fail on a machine with modest firmware memory.
const ARENA_BYTES: usize = 8 * 1024 * 1024;

// How many arenas can be tracked for release. 64 x 8 MiB is half a gigabyte of loader heap; a boot
// that needs more than that has a different problem.
const MAX_ARENAS: usize = 64;

struct Bump {
	// The firmware's boot services, or null once they are gone.
	services: *mut BootServices,
	// The current arena and how far into it we are.
	base: usize,
	next: usize,
	end: usize,
	// EVERY arena taken, so they can be given back. They used to be forgotten as soon as the next
	// one was taken: the loader's memory becomes `MEM_BOOTLOADER` in the map the kernel is handed,
	// the kernel's frame allocator seeds only `MEM_USABLE`, and so every page this heap ever
	// touched was reserved for the system's whole life. The comment above said reclaiming was
	// unnecessary because the loader ends at `ExitBootServices`; it does not end, it becomes
	// permanently reserved.
	arenas: [(u64, usize); MAX_ARENAS],
	arena_count: usize,
}

// The loader is single-threaded - the firmware calls `efi_main` on one processor and nothing here
// starts another - so the allocator needs no lock, only interior mutability.
struct Heap(UnsafeCell<Bump>);

unsafe impl Sync for Heap {}

#[global_allocator]
static HEAP: Heap = Heap(UnsafeCell::new(Bump { services: ptr::null_mut(), base: 0, next: 0, end: 0, arenas: [(0, 0); MAX_ARENAS], arena_count: 0 }));

// Hand the allocator the firmware's boot services. Must be called before the first allocation and
// before `ExitBootServices`; `retire` takes them away again.
pub(crate) fn init(services: *mut BootServices) {
	unsafe { (*HEAP.0.get()).services = services };
}

// Give every arena back to the firmware and return how many BYTES were returned.
//
// Called at the hand-off, after everything the kernel receives has been copied into pages of its
// own and BEFORE the final `GetMemoryMap`, so the map the kernel is handed describes this memory as
// usable rather than as the loader's. Anything still holding a heap allocation at this point is a
// bug that this makes loud rather than silent, which is the point.
pub(crate) fn release(services: *mut BootServices) -> usize {
	let bump = HEAP.0.get();
	let mut bytes = 0usize;
	unsafe {
		for i in 0..(*bump).arena_count {
			let (addr, pages) = (*bump).arenas[i];
			if addr != 0 && ((*services).free_pages)(addr, pages) == uefi::STATUS_SUCCESS {
				bytes += pages * crate::PAGE_SIZE as usize;
			}
		}
		(*bump).arena_count = 0;
		(*bump).base = 0;
		(*bump).next = 0;
		(*bump).end = 0;
		(*bump).services = ptr::null_mut();
	}
	bytes
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
		// CHECKED ARITHMETIC THROUGHOUT. In release these wrapped, and a wrapped `aligned + size`
		// can compare below `end` and return a pointer to a region that is not big enough - which
		// breaks the allocator's contract and makes ordinary safe Rust above it unsound. Null on
		// overflow is what `GlobalAlloc` asks for and what every caller here already handles.
		unsafe {
			let Some(aligned) = (*bump).next.checked_add(layout.align().wrapping_sub(1)).map(|v| v & !(layout.align() - 1)) else {
				return ptr::null_mut();
			};
			let Some(after) = aligned.checked_add(layout.size()) else {
				return ptr::null_mut();
			};
			if after <= (*bump).end {
				(*bump).next = after;
				return aligned as *mut u8;
			}
			// This arena is full. Take another, sized for the request when the request is the
			// larger - a file bigger than an arena must still be readable.
			let services = (*bump).services;
			if services.is_null() {
				return ptr::null_mut();
			}
			let Some(want) = layout.size().checked_add(layout.align()) else {
				return ptr::null_mut();
			};
			let bytes = if want > ARENA_BYTES { want } else { ARENA_BYTES };
			let pages = bytes.div_ceil(crate::PAGE_SIZE as usize);
			let Some(addr) = crate::alloc_pages(services, pages) else {
				return ptr::null_mut();
			};
			let Some(end) = (addr as usize).checked_add(pages * crate::PAGE_SIZE as usize) else {
				return ptr::null_mut();
			};
			// Recorded so it can be given back. An untrackable arena is refused rather than taken
			// and forgotten: a silent leak is what this whole change is about.
			if (*bump).arena_count == MAX_ARENAS {
				((*services).free_pages)(addr, pages);
				return ptr::null_mut();
			}
			(*bump).arenas[(*bump).arena_count] = (addr, pages);
			(*bump).arena_count += 1;
			(*bump).base = addr as usize;
			(*bump).next = addr as usize;
			(*bump).end = end;
			let Some(aligned) = (*bump).next.checked_add(layout.align().wrapping_sub(1)).map(|v| v & !(layout.align() - 1)) else {
				return ptr::null_mut();
			};
			let Some(after) = aligned.checked_add(layout.size()) else {
				return ptr::null_mut();
			};
			if after > end {
				return ptr::null_mut();
			}
			(*bump).next = after;
			aligned as *mut u8
		}
	}

	// Nothing is reclaimed. See the note at the top: the loader does not live long enough for
	// reuse to pay for the code that would do it.
	unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}
