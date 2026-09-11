//! `memcpy`, `memmove`, `memset`, `memcmp`.
//!
//! WHY THESE ARE HERE AT ALL, when `compiler_builtins` already provides them. The inventory names
//! them as undefined references in the pinned configuration's objects, and the rule this milestone
//! works under is that what the inventory names is what gets provided. Whether the converged link
//! resolves them here or in the compiler runtime is a PASS 2 question - the plan says the
//! compiler-runtime component is selected by that link - and providing them here does not decide it:
//! a definition the link does not need costs nothing, while a symbol nobody defined is a link that
//! fails after every other question has been answered.

use core::ffi::c_void;
use core::ptr;

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memcpy(destination: *mut c_void, source: *const c_void, count: usize) -> *mut c_void {
	unsafe { ptr::copy_nonoverlapping(source as *const u8, destination as *mut u8, count) };
	destination
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memmove(destination: *mut c_void, source: *const c_void, count: usize) -> *mut c_void {
	// `copy` RATHER THAN `copy_nonoverlapping`, which is the only difference between this and
	// `memcpy` and the entire reason both exist.
	unsafe { ptr::copy(source as *const u8, destination as *mut u8, count) };
	destination
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memset(destination: *mut c_void, value: i32, count: usize) -> *mut c_void {
	// THE VALUE IS AN `int` AND IS WRITTEN AS A BYTE, which C says explicitly: `memset(p, -1, n)`
	// fills with 0xff rather than doing anything with the sign.
	unsafe { ptr::write_bytes(destination as *mut u8, value as u8, count) };
	destination
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn memcmp(left: *const c_void, right: *const c_void, count: usize) -> i32 {
	let left = unsafe { core::slice::from_raw_parts(left as *const u8, count) };
	let right = unsafe { core::slice::from_raw_parts(right as *const u8, count) };
	for (a, b) in left.iter().zip(right.iter()) {
		if a != b {
			// THE COMPARISON IS UNSIGNED. Reading the bytes as signed makes 0x80 sort below 0x00,
			// which reverses the answer for half the byte range - and callers use the SIGN.
			return i32::from(*a) - i32::from(*b);
		}
	}
	0
}
