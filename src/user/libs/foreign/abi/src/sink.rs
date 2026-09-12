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

/// Install the sink from outside Rust.
///
/// WHY THERE IS A C ENTRY POINT AT ALL, when `install` above is the one the substrate calls. The
/// substrate that installs it is not always a Rust crate that can name this one: the quarantine
/// consumer this milestone's guest gate launches is an ordinary program in this image, built by the
/// ordinary build, and it reaches the audit-linked artifact the only way anything reaches anything
/// here - as unmangled provider exports.
///
/// IT IS NOT INVENTORY SURFACE, and the gate that holds this crate to the inventory knows it by
/// name. Nothing in the pinned configuration references it; it is how a LAUNCH hands the substrate
/// what it needs, which is the opposite direction from every other symbol here.
///
/// # Safety
/// Both pointers must be valid C functions for the life of the process.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn liber_foreign_install_sink(report: unsafe extern "C" fn(*const u8, usize), stop: unsafe extern "C" fn() -> !) {
	// THE POINTERS ARE STORED AS THE C ONES AND CALLED AS THE C ONES. A Rust `fn(&[u8])` and a C
	// `void(const uint8_t*, size_t)` are not the same type, and transmuting one into the other is
	// how a call arrives with the wrong shape on some architecture and not on the one it was tried
	// on. The two shims below are the conversion, written once.
	C_REPORTER.store(report as usize, Ordering::Relaxed);
	C_STOPPER.store(stop as usize, Ordering::Relaxed);
	install(c_report, c_stop);
	// AND THE STREAM THE DIAGNOSTICS GO TO. Installing a sink and attaching `stderr` are one act:
	// `stderr` is a pointer that cannot be given its value at compile time under
	// position-independent linking, so until this runs it is null and every `fputs` is refused. A
	// substrate that had been told where to report and still refused every report would be the
	// quietest possible failure.
	crate::format::attach_streams();
}

static C_REPORTER: AtomicUsize = AtomicUsize::new(0);
static C_STOPPER: AtomicUsize = AtomicUsize::new(0);

fn c_report(bytes: &[u8]) {
	let installed = C_REPORTER.load(Ordering::Relaxed);
	if installed == 0 {
		return;
	}
	let report: unsafe extern "C" fn(*const u8, usize) = unsafe { core::mem::transmute::<usize, unsafe extern "C" fn(*const u8, usize)>(installed) };
	unsafe { report(bytes.as_ptr(), bytes.len()) };
}

fn c_stop() -> ! {
	let installed = C_STOPPER.load(Ordering::Relaxed);
	assert!(installed != 0, "the foreign ABI stop hook was not installed before foreign code ran");
	let stop: unsafe extern "C" fn() -> ! = unsafe { core::mem::transmute::<usize, unsafe extern "C" fn() -> !>(installed) };
	unsafe { stop() }
}
