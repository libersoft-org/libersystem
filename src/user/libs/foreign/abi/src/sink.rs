//! Where this crate's diagnostics go, and how it stops.
//!
//! WHY A SINK AND NOT A DIRECT CALL INTO THE RUNTIME. A crate that reaches `rt` cannot be tested on
//! the host at all: `rt` defines the `panic_impl` lang item and so does `std`, so `cargo test`
//! fails with a duplicate lang item before a single test runs. That is not a hypothetical - it is
//! why `service-logic` exists as its own crate, and its contract says so in as many words. Every
//! function in THIS crate implements a C contract whose violations are silent, which makes host
//! fixtures worth more here than almost anywhere, so the dependency goes the other way: the
//! substrate installs what to call, and this crate calls it.
//!
//! THE DEFAULT IS TO PANIC RATHER THAN TO DISCARD. A diagnostic that goes nowhere is a message
//! somebody spends an afternoon looking for, and this crate says that about `fputs` in the same
//! breath - so it would be a poor thing to do quietly to its own. The substrate installs the sink
//! before any foreign code runs, at the point pass 1 named: `loader_init_library`.

use core::sync::atomic::{AtomicUsize, Ordering};

/// Write a diagnostic line. Never returns an error: there is nowhere to report one to.
pub type Reporter = fn(&[u8]);

/// Stop the process. Diverges.
pub type Stopper = fn() -> !;

// THE POINTERS ARE STORED AS `usize` BECAUSE A FUNCTION POINTER HAS NO ATOMIC TYPE. Relaxed ordering
// is right: these are written once before any foreign code runs and only read afterwards, so there
// is nothing for an ordering to establish.
static REPORTER: AtomicUsize = AtomicUsize::new(0);
static STOPPER: AtomicUsize = AtomicUsize::new(0);

/// Install the sink. The substrate calls this once, before foreign code runs.
pub fn install(reporter: Reporter, stopper: Stopper) {
	REPORTER.store(reporter as usize, Ordering::Relaxed);
	STOPPER.store(stopper as usize, Ordering::Relaxed);
}

pub(crate) fn report(bytes: &[u8]) {
	let installed = REPORTER.load(Ordering::Relaxed);
	assert!(installed != 0, "the foreign ABI diagnostic sink was not installed before foreign code ran");
	let reporter: Reporter = unsafe { core::mem::transmute::<usize, Reporter>(installed) };
	reporter(bytes);
}

pub(crate) fn stop() -> ! {
	let installed = STOPPER.load(Ordering::Relaxed);
	assert!(installed != 0, "the foreign ABI stop hook was not installed before foreign code ran");
	let stopper: Stopper = unsafe { core::mem::transmute::<usize, Stopper>(installed) };
	stopper()
}
