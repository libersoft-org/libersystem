//! Exactly the C-ABI facilities the pinned foreign configuration's derived inventory names.
//!
//! THE LIST IS NOT A DESIGN, IT IS A MEASUREMENT. Every symbol here appears because
//! `src/foreign/INVENTORY-pass1.json` - derived from the object closure of the pinned configuration
//! on all three targets - names it as an undefined reference. A facility the inventory does not name
//! is not built, and that rule is the whole point: every symbol outside the closure is a general
//! POSIX layer arriving one function at a time, and the arrival is always reasonable-looking.
//!
//! WHAT THIS CRATE DELIBERATELY DOES NOT HOLD. The ambient surface - opening a file, reading a
//! directory, asking for a user id, finding and loading a provider - is twenty-one further symbols
//! the inventory also names, and they belong to the DISCOVERY item rather than to this one. They are
//! not here because answering them is a decision about what a provider IS on this system, not a
//! translation of a C function.
//!
//! AND NOTHING HERE TOUCHES A FILE, A PROCESS OR A DEVICE. The one output path is `stderr`, which is
//! the process's own diagnostic stream and not an ambient file.
//!
//! THE SINGLE-THREADED CONTRACT, STATED WHERE THE PRIMITIVES ARE. The pinned configuration creates
//! no threads - that is measured, not assumed - so the mutual exclusion below is a counter rather
//! than a concurrency implementation, and it says so at the point of use. Nobody should later mistake
//! these for locks that work.

#![no_std]
#![cfg_attr(target_os = "none", feature(c_variadic))]

// THE EXPORTED NAMES ARE SUPPRESSED UNDER `cfg(test)`, and the reason is not cosmetic. This crate
// defines `malloc`, `free`, `memcpy`, `strlen` and the rest as unmangled C symbols; in a host test
// binary those OVERRIDE the platform libc's, so the test harness's own allocation calls land in an
// allocator that is itself implemented over the Rust allocator over that same libc. The process
// segfaults before the first test prints its name - which is exactly how this was found.
//
// `cfg_attr` RATHER THAN `cfg`, so the functions still exist and are still `extern "C"`: the tests
// exercise the real bodies with the real ABI, and only the linker-visible name is withheld.

extern crate alloc;

pub mod allocator;
pub mod format;
pub mod memory;
pub mod process;
pub mod sink;
pub mod strings;
pub mod sync;

#[cfg(test)]
mod tests;

/// `fabs`, the one math function the inventory names.
///
/// IT IS HERE RATHER THAN IN A `math` MODULE because one function is not a module, and a module
/// named for a domain invites the next person to add the rest of the domain to it - which is exactly
/// the "one function at a time" growth the facilities rule forbids.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn fabs(value: f64) -> f64 {
	// `abs` ON A FLOAT IS A SIGN-BIT CLEAR, not a comparison: it has to give `+0.0` for `-0.0` and
	// leave a NaN a NaN, which `if value < 0.0 { -value }` does not.
	f64::from_bits(value.to_bits() & !(1u64 << 63))
}

pub use format::attach_streams;
