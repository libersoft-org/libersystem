//! `malloc`, `calloc`, `realloc`, `free` over this process's own heap.
//!
//! THE SIZE HAS TO BE CARRIED, and that is the whole difficulty. Rust's allocator is told the layout
//! of what it frees; C's is not, so every block here is handed out with a header in front of it
//! recording the layout it was allocated with. A free that guessed the layout would be returning a
//! different allocation than it took.

use alloc::alloc::{Layout, alloc, alloc_zeroed, dealloc};
use core::ffi::c_void;
use core::ptr;

/// What precedes every block handed to C. It carries the layout `dealloc` will need, and its own
/// alignment is what keeps the payload aligned for anything the caller stores there.
#[repr(C)]
struct Header {
	size: usize,
	align: usize,
}

/// THE ALIGNMENT C CALLERS ARE ENTITLED TO. `malloc` must return memory suitable for any type with
/// a fundamental alignment, which on all three of this system's targets is sixteen bytes - the
/// alignment of `long double` and of the vector types the ABI passes in registers.
const MALLOC_ALIGN: usize = 16;

const HEADER: usize = core::mem::size_of::<Header>();

/// The offset from the block start to the payload, which is the header rounded up to the payload's
/// alignment so the payload itself lands aligned.
fn payload_offset(align: usize) -> usize {
	HEADER.next_multiple_of(align)
}

fn layout_for(size: usize, align: usize) -> Option<(Layout, usize)> {
	let offset = payload_offset(align);
	let total = offset.checked_add(size)?;
	Layout::from_size_align(total, align).ok().map(|layout| (layout, offset))
}

/// Allocate `size` bytes at `align`, writing the header the free path reads back.
unsafe fn allocate(size: usize, align: usize, zeroed: bool) -> *mut c_void {
	let Some((layout, offset)) = layout_for(size, align) else {
		return ptr::null_mut();
	};
	// A ZERO-SIZE REQUEST STILL RETURNS A DISTINCT POINTER, because C callers compare the result
	// against NULL to decide whether the allocation failed. Returning null for a legal request would
	// make a zero-length array look like an out-of-memory condition.
	let block = unsafe {
		match zeroed {
			true => alloc_zeroed(layout),
			false => alloc(layout),
		}
	};
	if block.is_null() {
		return ptr::null_mut();
	}
	unsafe {
		ptr::write(block as *mut Header, Header { size, align });
		block.add(offset) as *mut c_void
	}
}

/// Read back the header in front of a payload this module handed out.
unsafe fn header_of(payload: *mut c_void) -> (*mut u8, Header) {
	// THE ALIGNMENT IS NOT KNOWN YET, so the header cannot be found by subtracting a fixed offset.
	// It is found by subtracting the header size from the payload and reading the alignment from
	// there - which works because `payload_offset` rounds UP, so the header always ends exactly
	// where the padding begins, and the last `HEADER` bytes before the payload are always it.
	let header_start = unsafe { (payload as *mut u8).sub(HEADER) };
	let header = unsafe { ptr::read(header_start as *const Header) };
	let block = unsafe { (payload as *mut u8).sub(payload_offset(header.align)) };
	(block, header)
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
	unsafe { allocate(size, MALLOC_ALIGN, false) }
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn calloc(count: usize, size: usize) -> *mut c_void {
	// THE MULTIPLICATION IS CHECKED, which is the entire reason `calloc` exists as a separate call:
	// `malloc(count * size)` overflows silently and returns a block smaller than the caller will
	// write into.
	let Some(total) = count.checked_mul(size) else {
		return ptr::null_mut();
	};
	unsafe { allocate(total, MALLOC_ALIGN, true) }
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn realloc(pointer: *mut c_void, size: usize) -> *mut c_void {
	if pointer.is_null() {
		return unsafe { malloc(size) };
	}
	let (_, header) = unsafe { header_of(pointer) };
	let fresh = unsafe { allocate(size, header.align, false) };
	if fresh.is_null() {
		// THE ORIGINAL IS STILL THE CALLER'S. A failed `realloc` that had freed its input would
		// leave every caller that checks the result holding a dangling pointer it believes is live.
		return ptr::null_mut();
	}
	let keep = header.size.min(size);
	unsafe {
		ptr::copy_nonoverlapping(pointer as *const u8, fresh as *mut u8, keep);
		free(pointer);
	}
	fresh
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn free(pointer: *mut c_void) {
	// `free(NULL)` IS DEFINED AND DOES NOTHING. Callers rely on it in cleanup paths.
	if pointer.is_null() {
		return;
	}
	let (block, header) = unsafe { header_of(pointer) };
	let Some((layout, _)) = layout_for(header.size, header.align) else {
		return;
	};
	unsafe { dealloc(block, layout) };
}
