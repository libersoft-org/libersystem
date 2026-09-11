//! `getenv` and the user-identity calls, which the loader uses to decide what to trust.
//!
//! WHAT THE LOADER USES THESE FOR. It reads `VK_ICD_FILENAMES`, `VK_DRIVER_FILES`, `VK_LAYER_PATH`
//! and a dozen others to build a search path, and it checks whether the process is running with
//! elevated privilege to decide whether to trust them. Both halves of that are the ambient search
//! this milestone forbids.
//!
//! SO `getenv` ANSWERS NOTHING, ALWAYS, and that is a decision rather than a stub. An environment
//! this system did populate would be a search path arriving through a different door: the variables
//! exist precisely to let something outside the process point it at a driver, which is the authority
//! the selection slot holds instead.

use core::ffi::c_char;
use core::ptr;

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getenv(_name: *const c_char) -> *mut c_char {
	// NULL FOR EVERY NAME. The loader's own code treats an absent variable as "no override", which
	// is exactly the answer this system means, so no upstream branch has to be patched to get it.
	ptr::null_mut()
}

// THE IDENTITY CALLS ANSWER CONSISTENTLY, AND CONSISTENCY IS THE POINT. The loader compares the real
// and effective ids to decide whether the process is setuid, and trusts environment search paths
// only when it is not. Equal ids mean "not elevated" - which is true, and it is also the answer that
// keeps the loader on the code path this substrate has actually ported. Making them differ would
// send it down a hardened path that reads different variables, all of which are empty anyway, for no
// gain.
//
// ZERO RATHER THAN AN INVENTED NON-ZERO ID: this system has no user identities at all, and a made-up
// number would be a fact somebody later relies on.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getuid() -> u32 {
	0
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn geteuid() -> u32 {
	0
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getgid() -> u32 {
	0
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getegid() -> u32 {
	0
}
