//! `memcpy`, `memmove`, `memset`, `memcmp`.
//!
//! THEY ARE NOT EXPORTED, AND PASS 2 IS WHAT DECIDED THAT. This module used to publish all four
//! under their C names, on the reasoning that the inventory named them and a definition the link
//! does not need costs nothing. The converged link answered the question the other way: `lsrt.lslib`
//! already owns those four names in this image - it makes them visible on purpose, so that every
//! library can import them - and the audit-linked artifact links against it. Two owners for one
//! symbol is the export collision that the generic artifact check refuses, and it refused exactly
//! this one.
//!
//! SO THE COMPILER-RUNTIME COMPONENT FOR THE C HALF IS THE RUNTIME, like it is for every other
//! library staged here, and these implementations stay for the host fixtures that cover the
//! contracts - a C `memmove` that overlaps, a `memset` whose value is an `int` written as a byte -
//! which are worth keeping whoever ends up providing the symbol.

use core::ffi::c_void;
use core::ptr;

pub unsafe extern "C" fn memcpy(destination: *mut c_void, source: *const c_void, count: usize) -> *mut c_void {
	unsafe { ptr::copy_nonoverlapping(source as *const u8, destination as *mut u8, count) };
	destination
}

pub unsafe extern "C" fn memmove(destination: *mut c_void, source: *const c_void, count: usize) -> *mut c_void {
	// `copy` RATHER THAN `copy_nonoverlapping`, which is the only difference between this and
	// `memcpy` and the entire reason both exist.
	unsafe { ptr::copy(source as *const u8, destination as *mut u8, count) };
	destination
}

pub unsafe extern "C" fn memset(destination: *mut c_void, value: i32, count: usize) -> *mut c_void {
	// THE VALUE IS AN `int` AND IS WRITTEN AS A BYTE, which C says explicitly: `memset(p, -1, n)`
	// fills with 0xff rather than doing anything with the sign.
	unsafe { ptr::write_bytes(destination as *mut u8, value as u8, count) };
	destination
}

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
