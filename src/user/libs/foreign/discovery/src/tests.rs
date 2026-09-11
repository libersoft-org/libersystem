//! The discovery model, tested where it can be.
//!
//! WHAT THESE ARE FOR. Every function in this crate answers a question the upstream loader asks of a
//! filesystem, and the value of the answers is what they REFUSE: a manifest that is not in the
//! record, a provider that is not in the closure, a symbol outside the closed surface, an interface
//! version whose export shape two symbols cannot satisfy. A model that admitted any of those would
//! still pass a test that only checked the happy path.

use crate::record::{self, Icd, RECORD_VERSION, Record};
use crate::version::{self, Negotiated, Refusal};
use core::ffi::{c_char, c_void};

unsafe extern "C" fn negotiate(_version: *mut u32) -> i32 {
	0
}

unsafe extern "C" fn get_instance_proc_addr(_instance: *mut c_void, _name: *const c_char) -> *mut c_void {
	core::ptr::null_mut()
}

static MANIFEST: &[u8] = br#"{"file_format_version":"1.0.0","ICD":{"library_path":"vol://system/lib/icd.lsexe","api_version":"1.4.357"}}"#;

static ICD: Icd = Icd { manifest_path: b"vol://system/share/vulkan/icd.d/icd.json", manifest: MANIFEST, library_path: b"vol://system/lib/icd.lsexe", negotiate, get_instance_proc_addr };

static RECORD: Record = Record { version: RECORD_VERSION, icds: [Some(&ICD), None, None, None], self_path: b"vol://system/bin/consumer.lsexe" };

/// Install the fixture record and clear the process-wide pools.
///
/// THE POOLS ARE PROCESS-WIDE AND THE FIXTURES SHARE THEM, so a test that left a walk open would
/// decide the next test's answer. Cargo runs these on several threads; each one resets what it uses
/// before it uses it, and no test asserts on a pool's occupancy.
fn install() {
	unsafe { record::install(&RECORD) }.expect("the fixture record is at the current version");
	crate::directory::reset();
	crate::stream::reset();
}

fn c(text: &[u8]) -> CString {
	let mut out = CString::new();
	out.extend(text);
	out.push(0);
	out
}

/// A NUL-terminated copy on the stack, because this crate has no allocator and the fixtures need
/// C strings to pass in.
struct CString {
	bytes: [u8; 256],
	length: usize,
}

impl CString {
	fn new() -> CString {
		CString { bytes: [0; 256], length: 0 }
	}

	fn extend(&mut self, source: &[u8]) {
		for byte in source {
			self.push(*byte);
		}
	}

	fn push(&mut self, byte: u8) {
		self.bytes[self.length] = byte;
		self.length += 1;
	}

	fn as_ptr(&self) -> *const c_char {
		self.bytes.as_ptr() as *const c_char
	}
}

#[test]
fn a_record_at_the_wrong_version_is_refused_rather_than_interpreted() {
	// TWO SIDES THAT DISAGREE ABOUT THE SHAPE OF A RECORD and carry on are two sides reading
	// different fields of the same bytes.
	static WRONG: Record = Record { version: RECORD_VERSION + 1, icds: [None; record::MAX_ICDS], self_path: b"" };
	assert_eq!(unsafe { record::install(&WRONG) }, Err(RECORD_VERSION + 1));
}

#[test]
fn getenv_answers_nothing_for_every_name() {
	// THE VARIABLES EXIST PRECISELY TO LET SOMETHING OUTSIDE THE PROCESS point it at a driver, which
	// is the authority the selection slot holds instead.
	install();
	for name in [&b"VK_ICD_FILENAMES"[..], b"VK_DRIVER_FILES", b"VK_LAYER_PATH", b"PATH", b"HOME"] {
		let name = c(name);
		assert!(unsafe { crate::environment::getenv(name.as_ptr()) }.is_null(), "an environment search path is not a thing here");
	}
}

#[test]
fn a_directory_contains_exactly_what_the_record_names_under_it() {
	install();
	let real = c(b"vol://system/share/vulkan/icd.d");
	let handle = unsafe { crate::directory::opendir(real.as_ptr()) };
	assert!(!handle.is_null(), "the record names a manifest under this directory");
	let first = unsafe { crate::directory::readdir(handle) };
	assert!(!first.is_null());
	// THE SECOND READ ENDS THE WALK. One manifest is in the record, so one is what a walk finds -
	// there is no filesystem underneath to have more.
	assert!(unsafe { crate::directory::readdir(handle) }.is_null());
	assert_eq!(unsafe { crate::directory::closedir(handle) }, 0);
}

#[test]
fn a_directory_the_record_does_not_name_does_not_open() {
	install();
	// THE UPSTREAM SEARCH PATH'S OWN DIRECTORIES, which on a Linux host would hold real drivers.
	for path in [&b"/usr/share/vulkan/icd.d"[..], b"/etc/vulkan/icd.d", b".", b"/"] {
		let path = c(path);
		assert!(unsafe { crate::directory::opendir(path.as_ptr()) }.is_null(), "nothing outside the record exists to this process");
	}
}

#[test]
fn a_manifest_in_the_record_reads_back_and_one_outside_it_does_not_open() {
	install();
	let path = c(b"vol://system/share/vulkan/icd.d/icd.json");
	let stream = unsafe { crate::stream::fopen(path.as_ptr(), c(b"r").as_ptr()) };
	assert!(!stream.is_null());

	// `fstat` REPORTS THE SIZE, which is what the loader allocates its buffer from.
	let descriptor = unsafe { crate::stream::fileno(stream) };
	assert!(descriptor >= 0);
	let mut stat = [0u8; 128];
	assert_eq!(unsafe { crate::stream::fstat(descriptor, stat.as_mut_ptr() as *mut c_void) }, 0);

	let mut buffer = [0u8; 256];
	let read = unsafe { crate::stream::fread(buffer.as_mut_ptr() as *mut c_void, 1, buffer.len(), stream) };
	assert_eq!(read, MANIFEST.len());
	assert_eq!(&buffer[..read], MANIFEST);
	assert_eq!(unsafe { crate::stream::fclose(stream) }, 0);

	let elsewhere = c(b"/usr/share/vulkan/icd.d/other.json");
	assert!(unsafe { crate::stream::fopen(elsewhere.as_ptr(), c(b"r").as_ptr()) }.is_null());
}

#[test]
fn a_write_mode_is_refused_rather_than_ignored() {
	// A LOADER THAT BELIEVED IT HAD OPENED A MANIFEST FOR WRITING would report success for a write
	// that went nowhere.
	install();
	let path = c(b"vol://system/share/vulkan/icd.d/icd.json");
	for mode in [&b"w"[..], b"a", b"w+", b"rb+"] {
		let handle = unsafe { crate::stream::fopen(path.as_ptr(), c(mode).as_ptr()) };
		match mode.starts_with(b"r") {
			true => assert!(!handle.is_null(), "a read mode with a suffix is still a read"),
			false => assert!(handle.is_null(), "{:?} is not a mode this record can serve", core::str::from_utf8(mode)),
		}
		if !handle.is_null() {
			unsafe { crate::stream::fclose(handle) };
		}
	}
}

#[test]
fn opening_a_provider_is_finding_it_in_the_closure_and_nothing_else() {
	install();
	let real = c(b"vol://system/lib/icd.lsexe");
	let handle = unsafe { crate::library::loader_platform_open_library(real.as_ptr()) };
	assert!(!handle.is_null());

	// THE ONE WAY IT CAN FAIL is by naming a provider that is not in the closure. There is no path
	// that resolves to a file, so nothing else is even expressible.
	for path in [&b"/usr/lib/libvulkan_radeon.so"[..], b"./evil.so", b"vol://system/lib/other.lsexe"] {
		let path = c(path);
		assert!(unsafe { crate::library::loader_platform_open_library(path.as_ptr()) }.is_null());
	}
	unsafe { crate::library::loader_platform_close_library(handle) };
}

#[test]
fn exactly_two_exports_resolve_and_the_third_is_refused_by_name() {
	install();
	let handle = unsafe { crate::library::loader_platform_open_library(c(b"vol://system/lib/icd.lsexe").as_ptr()) };
	assert!(!handle.is_null());

	for symbol in [crate::library::NEGOTIATE_SYMBOL, crate::library::GET_INSTANCE_PROC_ADDR_SYMBOL] {
		let name = c(symbol);
		assert!(!unsafe { crate::library::loader_platform_get_proc_address(handle, name.as_ptr()) }.is_null(), "an admitted export resolves");
	}

	// THE THIRD EXPORT IS A DECISION, NOT A LOOKUP THAT FAILED, and the two are distinguishable.
	let third = c(crate::library::PHYSICAL_DEVICE_SYMBOL);
	assert!(unsafe { crate::library::loader_platform_get_proc_address(handle, third.as_ptr()) }.is_null());
	assert_eq!(crate::library::refusal(crate::library::PHYSICAL_DEVICE_SYMBOL), Some(Refusal::ThirdExportRequired));
	assert_eq!(crate::library::refusal(b"vkCreateInstance"), None, "an ordinary symbol is simply absent");

	// AND NOTHING ELSE RESOLVES, including the entry points a `dlsym` would have found.
	for symbol in [&b"vkCreateInstance"[..], b"vkGetInstanceProcAddr", b"", b"vk_icdNegotiate"] {
		let name = c(symbol);
		assert!(unsafe { crate::library::loader_platform_get_proc_address(handle, name.as_ptr()) }.is_null());
	}
}

#[test]
fn the_admitted_version_set_is_the_one_this_substrate_decided() {
	assert_eq!(version::ADMITTED, [2, 3, 4, 5, 6]);
	for version in version::ADMITTED {
		assert_eq!(version::negotiate(version, false), Negotiated::At(version));
		assert!(version::is_admitted(version));
	}
}

#[test]
fn each_refused_version_is_refused_for_its_own_reason() {
	// COLLAPSING THESE INTO ONE "unsupported" would leave whoever hits it guessing which fact about
	// the interface stopped them.
	assert_eq!(version::negotiate(0, false), Negotiated::Refused(Refusal::MultiExportBootstrap));
	assert_eq!(version::negotiate(1, false), Negotiated::Refused(Refusal::NoNegotiationFunction));
	// 7 AND ABOVE ARE NOT REFUSED OUTRIGHT - they are negotiated DOWN to 6, which is what upstream's
	// own rule says and what keeps two exports sufficient.
	assert_eq!(version::negotiate(7, false), Negotiated::At(6));
	assert_eq!(version::negotiate(99, false), Negotiated::At(6));
	assert!(!version::is_admitted(7), "but 7 itself is not a version this substrate runs at");
	// AND THE THIRD EXPORT IS REFUSED WHATEVER VERSION IS OFFERED, because it is a fact about the
	// driver rather than about the interface revision.
	for offered in [2, 4, 6, 7] {
		assert_eq!(version::negotiate(offered, true), Negotiated::Refused(Refusal::ThirdExportRequired), "offered {offered}");
	}
}

#[test]
fn the_loaders_own_path_comes_from_the_record_and_is_refused_rather_than_truncated() {
	install();
	let mut buffer = [0 as c_char; 64];
	let answer = unsafe { crate::library::loader_platform_executable_path(buffer.as_mut_ptr(), buffer.len()) };
	assert!(!answer.is_null());

	// A TRUNCATED PATH IS A PATH TO SOMETHING ELSE, and the caller has no way to tell that is what
	// it got.
	let mut tiny = [0 as c_char; 4];
	assert!(unsafe { crate::library::loader_platform_executable_path(tiny.as_mut_ptr(), tiny.len()) }.is_null());
}

#[test]
fn existence_means_being_in_this_record() {
	install();
	for path in [&b"vol://system/share/vulkan/icd.d/icd.json"[..], b"vol://system/lib/icd.lsexe"] {
		assert!(unsafe { crate::library::loader_platform_file_exists(c(path).as_ptr()) });
	}
	for path in [&b"/etc/passwd"[..], b"vol://system/lib/other.lsexe", b""] {
		assert!(!unsafe { crate::library::loader_platform_file_exists(c(path).as_ptr()) });
	}
}
