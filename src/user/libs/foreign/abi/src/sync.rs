//! The mutual-exclusion primitives, UNDER A DOCUMENTED SINGLE-THREADED CONTRACT.
//!
//! READ THIS BEFORE USING ANY OF IT. These are not locks. The pinned configuration creates no
//! threads - pass 1 measured zero thread-creation symbols on all three targets, and the stop
//! condition on the full closure is what keeps that true - so what these need to do is exactly what
//! an uncontended lock does: count. A `lock` that finds the mutex already held has discovered that
//! the single-threaded contract has been broken, and it says so rather than deadlocking or
//! pretending.
//!
//! WHY NOT IMPLEMENT REAL LOCKS ANYWAY. Because a real lock here would be a concurrency
//! implementation nobody has reviewed, backing an interface nobody calls concurrently, and its first
//! actual contended use would be the first time anybody found out whether it worked. A counter that
//! REFUSES is honest about what it is; a spin loop would look like it worked right up until it did
//! not.
//!
//! THE LOADER IS THREAD-SAFE BY SPECIFICATION, which is what makes it take these at all - it
//! serialises its own dispatch so a multi-threaded caller is safe. Thread SAFETY does not imply
//! thread CREATION, and conflating the two is what previously made this milestone depend on a thread
//! runtime it may never need.

use core::ffi::c_void;

/// The shape the platform header declares: one word of state.
#[repr(C)]
pub struct Mutex {
	state: usize,
}

const UNLOCKED: usize = 0;
const LOCKED: usize = 1;
/// Written over a mutex that has been deleted, so a use-after-delete is a distinguishable state
/// rather than whatever the freed memory happened to hold.
const DELETED: usize = 2;

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn loader_platform_thread_create_mutex(mutex: *mut c_void) {
	let mutex = mutex as *mut Mutex;
	unsafe { (*mutex).state = UNLOCKED };
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn loader_platform_thread_lock_mutex(mutex: *mut c_void) {
	let mutex = mutex as *mut Mutex;
	match unsafe { (*mutex).state } {
		UNLOCKED => unsafe { (*mutex).state = LOCKED },
		LOCKED => {
			// THE CONTRACT HAS BEEN BROKEN AND THAT IS WORTH SAYING OUT LOUD. Under the
			// single-threaded contract nothing can be holding this, so either a second thread exists
			// - which the closure scan is supposed to make impossible - or the same code path took
			// the lock twice. Both are faults, and both are silent if this quietly succeeds.
			crate::sink::report(b"foreign: a mutex was taken twice under the single-threaded contract\n");
			crate::sink::stop()
		}
		_ => {
			crate::sink::report(b"foreign: a deleted or uninitialised mutex was locked\n");
			crate::sink::stop()
		}
	}
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn loader_platform_thread_unlock_mutex(mutex: *mut c_void) {
	let mutex = mutex as *mut Mutex;
	match unsafe { (*mutex).state } {
		LOCKED => unsafe { (*mutex).state = UNLOCKED },
		_ => {
			crate::sink::report(b"foreign: a mutex that was not held was unlocked\n");
			crate::sink::stop()
		}
	}
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn loader_platform_thread_delete_mutex(mutex: *mut c_void) {
	let mutex = mutex as *mut Mutex;
	unsafe { (*mutex).state = DELETED };
}
