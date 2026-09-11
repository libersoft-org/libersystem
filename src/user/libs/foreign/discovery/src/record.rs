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
