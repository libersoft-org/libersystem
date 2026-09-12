//! The bounded, versioned record the substrate installs before any foreign code runs.
//!
//! THIS IS THE WHOLE OF WHAT THE LOADER MAY DISCOVER. Everything the ambient calls answer comes from
//! here: the manifests that exist, the provider each names, and the two entry points that provider
//! exports. Nothing enumerates, nothing searches, and nothing can be added after the first thread
//! starts - the closure was built and verified before it.
//!
//! THE METADATA SELECTS AMONG A DECLARED SET AND CANNOT WIDEN IT. That is what makes discovery a
//! policy input rather than an authority: an operator may choose which admitted ICD runs, and cannot
//! introduce one the consumer was not built against. The set itself is signed into the consumer's
//! identity record; by the time this crate sees a record, that check has already happened.

use core::sync::atomic::{AtomicPtr, Ordering};

/// `vk_icdNegotiateLoaderICDInterfaceVersion`.
pub type NegotiateFn = unsafe extern "C" fn(*mut u32) -> i32;

/// `vk_icdGetInstanceProcAddr`.
pub type GetInstanceProcAddrFn = unsafe extern "C" fn(*mut core::ffi::c_void, *const core::ffi::c_char) -> *mut core::ffi::c_void;

/// One resolved ICD: a provider already in the verified closure, and the two symbols it exports.
///
/// THERE IS NO PATH HERE THAT ANYTHING OPENS. `manifest_path` and `library_path` are the names the
/// loader will compare against, because it was built to work with paths; they select among what this
/// record holds and they reach no file system.
pub struct Icd {
	pub manifest_path: &'static [u8],
	pub manifest: &'static [u8],
	pub library_path: &'static [u8],
	pub negotiate: NegotiateFn,
	pub get_instance_proc_addr: GetInstanceProcAddrFn,
}

/// THE MOST ICDs ONE LAUNCH MAY RESOLVE. One selection slot resolves to one provider; the bound is
/// here because the loader's own code paths iterate, and an iteration over an unbounded set is a
/// scan by another name.
pub const MAX_ICDS: usize = 4;

/// What a launch hands the foreign substrate.
pub struct Record {
	/// The record format's own version, so a substrate and a ProcessService that disagree fail
	/// rather than misread each other.
	pub version: u32,
	pub icds: [Option<&'static Icd>; MAX_ICDS],
	/// The consumer's own image path, which the loader asks for when it reports where it is.
	pub self_path: &'static [u8],
}

/// The format this crate understands. A record at any other version is refused.
pub const RECORD_VERSION: u32 = 1;

static INSTALLED: AtomicPtr<Record> = AtomicPtr::new(core::ptr::null_mut());

/// Install the record. The substrate calls this once, before foreign code runs.
///
/// # Safety
/// `record` must outlive every foreign call, which it does because the substrate holds it for the
/// life of the process.
pub unsafe fn install(record: &'static Record) -> Result<(), u32> {
	if record.version != RECORD_VERSION {
		// A VERSION MISMATCH IS REFUSED RATHER THAN INTERPRETED. Two sides that disagree about the
		// shape of a record and carry on are two sides reading different fields of the same bytes.
		return Err(record.version);
	}
	INSTALLED.store(record as *const Record as *mut Record, Ordering::Relaxed);
	Ok(())
}

/// The installed record, or `None` before the substrate installs one.
pub fn installed() -> Option<&'static Record> {
	let pointer = INSTALLED.load(Ordering::Relaxed);
	match pointer.is_null() {
		true => None,
		false => Some(unsafe { &*pointer }),
	}
}

/// Forget the installed record. Test support only: the statics are process-wide, and a fixture that
/// left one behind would decide the next fixture's answers.
#[cfg(test)]
pub fn clear() {
	INSTALLED.store(core::ptr::null_mut(), Ordering::Relaxed);
}

impl Record {
	/// Every ICD this record holds, in order.
	pub fn icds(&self) -> impl Iterator<Item = &'static Icd> + '_ {
		self.icds.iter().filter_map(|slot| *slot)
	}

	/// The ICD whose manifest is at `path`, if this record holds one.
	pub fn icd_by_manifest(&self, path: &[u8]) -> Option<&'static Icd> {
		self.icds().find(|icd| icd.manifest_path == path)
	}

	/// The ICD whose library is at `path`, if this record holds one.
	pub fn icd_by_library(&self, path: &[u8]) -> Option<&'static Icd> {
		self.icds().find(|icd| icd.library_path == path)
	}
}

/// Install a record holding ONE ICD, from outside Rust.
///
/// WHY THERE IS A C ENTRY POINT. The substrate that installs a record is not always a Rust crate
/// that can name `install`: the quarantine consumer this milestone's guest gate launches is an
/// ordinary program in this image, and it reaches the audit-linked artifact the only way anything
/// reaches anything here - as unmangled provider exports. What it hands over is what a launch knows
/// and this crate cannot: which provider was bound into the closure, and the two entry points that
/// provider exports.
///
/// IT IS NOT INVENTORY SURFACE, and the gate that holds this crate to the inventory knows it by
/// name. Nothing in the pinned configuration references it; it is how a LAUNCH hands the substrate
/// its record, which is the opposite direction from every other symbol here.
///
/// ONE ICD AND NOT A LIST, because one selection slot resolves to one provider. A caller with two
/// would be a caller whose consumer declared two slots, which is a record this entry point would
/// have to grow a shape for rather than guess one.
///
/// # Safety
/// Every pointer must be valid and NUL-terminated for the life of the process, and the two function
/// pointers must be the provider's own exports.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn liber_foreign_install_icd(manifest_path: *const u8, manifest: *const u8, library_path: *const u8, self_path: *const u8, negotiate: NegotiateFn, get_instance_proc_addr: GetInstanceProcAddrFn) -> i32 {
	// THE STRINGS ARE BORROWED AND NOT COPIED. A record outlives every foreign call, and the caller
	// is a program whose own image holds these literals for as long as it runs; copying them would
	// need an allocator this crate does not have and must not acquire.
	unsafe fn borrow(pointer: *const u8) -> Option<&'static [u8]> {
		if pointer.is_null() {
			return None;
		}
		let mut len = 0usize;
		// A BOUND, because a missing terminator is a walk with no end. Every path this substrate
		// answers is a package-owned name, and 4096 is past anything one can be.
		while len < 4096 && unsafe { *pointer.add(len) } != 0 {
			len += 1;
		}
		if len == 4096 {
			return None;
		}
		Some(unsafe { core::slice::from_raw_parts(pointer, len) })
	}
	let (Some(manifest_path), Some(manifest), Some(library_path), Some(self_path)) = (unsafe { borrow(manifest_path) }, unsafe { borrow(manifest) }, unsafe { borrow(library_path) }, unsafe { borrow(self_path) }) else {
		return -1;
	};
	// THE RECORD AND THE ICD ARE PROCESS-WIDE STATICS, written once. There is no allocator here and
	// nothing to free: a record that could be replaced would be a discovery that happens after the
	// closure was verified, which is the thing this whole design refuses.
	static mut ICD: Option<Icd> = None;
	static mut RECORD: Option<Record> = None;
	// ONCE, AND THE FLAG IS WHAT SAYS SO. Reading the statics to find out would be a shared reference
	// to a mutable static, which is the thing this process has no second thread to race over and
	// still must not write: a raw-pointer write and an atomic flag say the same thing without
	// claiming a reference that does not hold.
	static INSTALLED_ONCE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
	if INSTALLED_ONCE.swap(true, Ordering::Relaxed) {
		// A second install is a caller that thinks it is the first one.
		return -2;
	}
	unsafe {
		(&raw mut ICD).write(Some(Icd { manifest_path, manifest, library_path, negotiate, get_instance_proc_addr }));
		let icd: &'static Icd = (*(&raw const ICD)).as_ref().unwrap_unchecked();
		(&raw mut RECORD).write(Some(Record { version: RECORD_VERSION, icds: [Some(icd), None, None, None], self_path }));
		let record: &'static Record = (*(&raw const RECORD)).as_ref().unwrap_unchecked();
		match install(record) {
			Ok(()) => 0,
			Err(_) => -3,
		}
	}
}
