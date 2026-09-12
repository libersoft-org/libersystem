//! Loading a provider, which opens nothing.
//!
//! `dlopen` BECOMES "TAKE A REFERENCE TO THE ALREADY-VERIFIED PROVIDER" AND `dlclose` BECOMES "DROP
//! IT". They can fail in exactly one way: by naming a provider that is not in the closure. That is
//! the whole difference between this and an ambient plugin load - there is no path that resolves to
//! a file, no directory that is searched, and nothing that can be added after the first thread
//! started, because the closure was built and verified before it.
//!
//! AND THERE IS NO `dlsym`. Symbol resolution is two well-known exports, resolved as ordinary
//! provider exports at load time; everything else the loader needs comes from the function pointers
//! those two return. A provider-scoped export query would need module provenance in the kernel's
//! export table and a new ring-3 syscall, which is a kernel redesign this milestone has no business
//! doing for a substrate audit.

use crate::record::{self, Icd};
use core::ffi::{c_char, c_void};
use core::ptr;

/// The two exports an ICD is required to have, and the only two this substrate will resolve.
pub const NEGOTIATE_SYMBOL: &[u8] = b"vk_icdNegotiateLoaderICDInterfaceVersion";
pub const GET_INSTANCE_PROC_ADDR_SYMBOL: &[u8] = b"vk_icdGetInstanceProcAddr";

/// The third export, refused BY NAME so the refusal is legible rather than looking like a lookup
/// that happened to fail.
pub const PHYSICAL_DEVICE_SYMBOL: &[u8] = b"vk_icdGetPhysicalDeviceProcAddr";

unsafe fn as_bytes(text: *const c_char) -> &'static [u8] {
	if text.is_null() {
		return b"";
	}
	let mut length = 0;
	while unsafe { *text.add(length) } != 0 {
		length += 1;
	}
	unsafe { core::slice::from_raw_parts(text as *const u8, length) }
}

/// Take a reference to the provider at `path`, if the record holds one there.
pub fn open(path: &[u8]) -> Option<&'static Icd> {
	record::installed()?.icd_by_library(path)
}

/// Resolve one of the two admitted exports on `icd`.
///
/// ANYTHING ELSE IS NULL, INCLUDING THE THIRD EXPORT. A loader that asked for the physical-device
/// function gets the same answer as one that asked for a symbol that does not exist, and that is
/// correct: within this profile it does not.
pub fn resolve(icd: &'static Icd, symbol: &[u8]) -> *mut c_void {
	match symbol {
		s if s == NEGOTIATE_SYMBOL => icd.negotiate as *mut c_void,
		s if s == GET_INSTANCE_PROC_ADDR_SYMBOL => icd.get_instance_proc_addr as *mut c_void,
		// THE THIRD EXPORT IS MATCHED AND REFUSED, rather than falling into the same arm as a
		// misspelling. Both answer NULL, but only one of them is a DECISION - and the decision is
		// what this substrate's closed surface means. A reader following the lookup that failed
		// should find the reason here rather than infer it from an absence.
		s if s == PHYSICAL_DEVICE_SYMBOL => ptr::null_mut(),
		_ => ptr::null_mut(),
	}
}

/// Why a symbol is not resolvable here, for a caller that wants to say something useful about it.
///
/// SEPARATE FROM `resolve` BECAUSE C HAS NOWHERE TO PUT IT. `dlsym` returns a pointer or NULL, and
/// the loader's own error path prints a string; this is what lets the substrate's gate assert on the
/// difference between "outside this profile" and "no such symbol".
pub fn refusal(symbol: &[u8]) -> Option<crate::version::Refusal> {
	match symbol {
		s if s == PHYSICAL_DEVICE_SYMBOL => Some(crate::version::Refusal::ThirdExportRequired),
		_ => None,
	}
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn loader_platform_open_library(path: *const c_char) -> *mut c_void {
	match open(unsafe { as_bytes(path) }) {
		Some(icd) => icd as *const Icd as *mut c_void,
		None => ptr::null_mut(),
	}
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn loader_platform_open_library_error(_path: *const c_char) -> *const c_char {
	// ONE MESSAGE, BECAUSE THERE IS ONE FAILURE. A `dlopen` that searched could fail a dozen ways
	// and its error string is how a reader tells them apart; this one cannot.
	c"the named provider is not in this launch's verified closure".as_ptr()
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn loader_platform_close_library(_library: *mut c_void) {
	// DROPPING A REFERENCE, AND THE REFERENCE IS A BORROW. Nothing is unmapped: the provider is in
	// the process's verified closure for the life of the process, and unloading it would be the
	// replacement this model forbids.
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn loader_platform_get_proc_address(library: *mut c_void, name: *const c_char) -> *mut c_void {
	if library.is_null() {
		return ptr::null_mut();
	}
	let icd = unsafe { &*(library as *const Icd) };
	resolve(icd, unsafe { as_bytes(name) })
}

/// `dladdr`, which the loader uses to ask where its own code lives.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn dladdr(_address: *const c_void, info: *mut c_void) -> i32 {
	let Some(record) = record::installed() else {
		return 0;
	};
	if info.is_null() {
		return 0;
	}
	// The `Dl_info` the sysroot declares: four pointer-sized fields, the first of which is
	// the one the loader reads.
	#[repr(C)]
	struct DlInfo {
		file_name: *const c_char,
		file_base: *mut c_void,
		symbol_name: *const c_char,
		symbol_address: *mut c_void,
	}
	// WRITTEN UNALIGNED, for the same reason `fstat` does: the caller's buffer alignment is the
	// caller's business and C gives this function no way to check it.
	unsafe {
		ptr::write_unaligned(info as *mut DlInfo, DlInfo { file_name: record.self_path.as_ptr() as *const c_char, file_base: ptr::null_mut(), symbol_name: ptr::null(), symbol_address: ptr::null_mut() });
	}
	1
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn loader_platform_executable_path(buffer: *mut c_char, size: usize) -> *mut c_char {
	let Some(record) = record::installed() else {
		return ptr::null_mut();
	};
	if buffer.is_null() || size == 0 {
		return ptr::null_mut();
	}
	let path = record.self_path;
	if path.len() + 1 > size {
		// REFUSED RATHER THAN TRUNCATED. A truncated path is a path to something else, and the
		// caller has no way to tell that is what it got.
		return ptr::null_mut();
	}
	unsafe {
		ptr::copy_nonoverlapping(path.as_ptr() as *const c_char, buffer, path.len());
		*buffer.add(path.len()) = 0;
	}
	buffer
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn loader_platform_is_path_absolute(path: *const c_char) -> bool {
	// THE SPELLING RULE IS UNCHANGED because the loader builds and compares these strings itself.
	// What it cannot do is turn one into a file.
	unsafe { as_bytes(path) }.first() == Some(&b'/')
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn loader_platform_file_exists(path: *const c_char) -> bool {
	let Some(record) = record::installed() else {
		return false;
	};
	let path = unsafe { as_bytes(path) };
	// "EXISTS" MEANS "IS IN THIS RECORD". There is nothing else for it to mean: a path that names
	// something outside the launch's closure does not exist as far as this process is concerned,
	// whatever some other process can see.
	let known = record.icd_by_manifest(path).is_some() || record.icd_by_library(path).is_some();
	if !known {
		crate::report(b"no such path ", path);
	}
	known
}

// `loader_platform_dirname` AND `loader_platform_get_proc_address_error` ARE NOT HERE, and their
// absence is the exact-surface rule doing its job rather than an oversight. The patch declares them,
// because the header's common-unix section does; the pinned configuration's objects never CALL
// them, so pass 1 does not name them, so nothing builds them. Writing them anyway would have been
// two functions nobody asked for, each looking perfectly reasonable, which is how a general POSIX
// layer arrives.
