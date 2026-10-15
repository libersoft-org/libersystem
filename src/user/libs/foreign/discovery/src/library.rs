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

use crate::record::{self, Icd, NegotiateFn};
use crate::version::{self, Exchange, Negotiated};
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
///
/// AND THE NEGOTIATION EXPORT IS NOT THE DRIVER'S. What the loader gets for it is the substrate's
/// bounded entry point for the slot this ICD occupies, which offers the driver at most 6 and refuses
/// any answer outside 2..=6 before the loader hears of it. The loader asks every driver for 7 and
/// accepts anything down to 0, so handing it the driver's own function made the admitted set a
/// property of whichever driver happened to clamp itself. Because this answer is never null, the
/// loader's fallback - asking the driver's own `vk_icdGetInstanceProcAddr` for a negotiation
/// function - is never taken; and because nothing above 6 is ever agreed, neither is its version-7
/// path that asks that same lookup for the physical-device function.
pub fn resolve(icd: &'static Icd, symbol: &[u8]) -> *mut c_void {
	match symbol {
		s if s == NEGOTIATE_SYMBOL => match slot_of(icd) {
			Some(slot) => BOUNDED[slot] as *mut c_void,
			// AN ICD THE INSTALLED RECORD DOES NOT HOLD has no slot and so no bound to run under.
			// `open` only ever hands out the record's own, so this is a caller with a stale handle.
			None => ptr::null_mut(),
		},
		s if s == GET_INSTANCE_PROC_ADDR_SYMBOL => icd.get_instance_proc_addr as *mut c_void,
		// THE THIRD EXPORT IS MATCHED AND REFUSED, rather than falling into the same arm as a
		// misspelling. Both answer NULL, but only one of them is a DECISION - and the decision is
		// what this substrate's closed surface means. A reader following the lookup that failed
		// should find the reason here rather than infer it from an absence.
		s if s == PHYSICAL_DEVICE_SYMBOL => ptr::null_mut(),
		_ => ptr::null_mut(),
	}
}

/// The record slot `icd` occupies, by identity rather than by name.
fn slot_of(icd: &'static Icd) -> Option<usize> {
	record::installed()?.icds.iter().position(|held| held.is_some_and(|held| ptr::eq(held, icd)))
}

/// The negotiation entry point the loader is handed for the ICD in record slot `SLOT`.
///
/// ONE PER SLOT, because the interface gives a negotiation function nothing but the version pointer
/// - no handle, no context - so the function itself has to know which driver it speaks for. The
/// driver is looked up when the call is made, from the record, so what runs is always what the
/// record holds.
///
/// ON A REFUSAL THE LOADER'S VALUE IS LEFT AS IT WROTE IT. Reporting a version that was never agreed
/// is how a loader comes to call an entry point that is not there; the result it gets is the one
/// failure it recognises, and the reason is a line in the diagnostic stream.
unsafe extern "C" fn bounded_negotiate<const SLOT: usize>(version: *mut u32) -> i32 {
	let Some(icd) = record::installed().and_then(|record| record.icds.get(SLOT).copied().flatten()) else {
		return version::INCOMPATIBLE_DRIVER;
	};
	if version.is_null() {
		return version::INCOMPATIBLE_DRIVER;
	}
	// SAFETY: the loader passes a live pointer to its own local, checked non-null above.
	let asked = unsafe { *version };
	// SAFETY: `negotiate` is the entry point of the provider the launch bound into this slot.
	let exchange = unsafe { version::exchange(icd.negotiate, asked) };
	report_exchange(&exchange);
	match exchange.outcome {
		Negotiated::At(agreed) => {
			// SAFETY: as above.
			unsafe { *version = agreed };
			version::SUCCESS
		}
		Negotiated::Refused(_) => version::INCOMPATIBLE_DRIVER,
	}
}

/// The bounded entry points, indexed by record slot. Its length IS the record's bound: a record that
/// grew a slot without a bounded entry point for it would not compile.
static BOUNDED: [NegotiateFn; record::MAX_ICDS] = [bounded_negotiate::<0>, bounded_negotiate::<1>, bounded_negotiate::<2>, bounded_negotiate::<3>];

/// Say what one negotiation settled, through the substrate's diagnostic stream.
///
/// AN AGREEMENT IS REPORTED TOO, not only a refusal, because it is the one place the bound can be
/// SEEN working: the loader asked for 7, and the line says what the driver was offered instead and
/// what came back.
fn report_exchange(exchange: &Exchange) {
	let mut line = Line::new();
	line.text(b"icd interface: loader asked ").number(exchange.asked);
	if let Some(offered) = exchange.offered {
		line.text(b", substrate offered ").number(offered);
		match exchange.outcome {
			Negotiated::Refused(version::Refusal::DriverDeclined) => line.text(b", driver declined"),
			_ => line.text(b", driver answered ").number(exchange.answered),
		};
	}
	match exchange.outcome {
		Negotiated::At(agreed) => line.text(b" - agreed ").number(agreed),
		Negotiated::Refused(refusal) => line.text(b" - refused: ").text(refusal.reason()),
	};
	crate::report(line.as_bytes(), b"");
}

/// A diagnostic line built in place, because this runs inside the loader's driver scan and must not
/// allocate. Too long a line is cut rather than refused: it is a diagnostic, not a value.
struct Line {
	bytes: [u8; 192],
	len: usize,
}

impl Line {
	fn new() -> Line {
		Line { bytes: [0; 192], len: 0 }
	}

	fn text(&mut self, text: &[u8]) -> &mut Line {
		for byte in text {
			if self.len < self.bytes.len() {
				self.bytes[self.len] = *byte;
				self.len += 1;
			}
		}
		self
	}

	fn number(&mut self, value: u32) -> &mut Line {
		let mut digits = [0u8; 10];
		let mut at = digits.len();
		let mut left = value;
		loop {
			at -= 1;
			digits[at] = b'0' + (left % 10) as u8;
			left /= 10;
			if left == 0 {
				break;
			}
		}
		self.text(&digits[at..])
	}

	fn as_bytes(&self) -> &[u8] {
		&self.bytes[..self.len]
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
