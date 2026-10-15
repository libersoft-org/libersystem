//! The discovery model, tested where it can be.
//!
//! WHAT THESE ARE FOR. Every function in this crate answers a question the upstream loader asks of a
//! filesystem, and the value of the answers is what they REFUSE: a manifest that is not in the
//! record, a provider that is not in the closure, a symbol outside the closed surface, an interface
//! version whose export shape two symbols cannot satisfy. A model that admitted any of those would
//! still pass a test that only checked the happy path.

extern crate std;

use crate::record::{self, Icd, NegotiateFn, RECORD_VERSION, Record};
use crate::version::{self, Negotiated, Refusal};
use core::ffi::{c_char, c_void};
use core::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};

unsafe extern "C" fn negotiate(_version: *mut u32) -> i32 {
	0
}

unsafe extern "C" fn get_instance_proc_addr(_instance: *mut c_void, _name: *const c_char) -> *mut c_void {
	core::ptr::null_mut()
}

static MANIFEST: &[u8] = br#"{"file_format_version":"1.0.0","ICD":{"library_path":"vol://system/lib/icd.lsexe","api_version":"1.4.357"}}"#;

static ICD: Icd = Icd { manifest_path: b"vol://system/share/vulkan/icd.d/icd.json", manifest: MANIFEST, library_path: b"vol://system/lib/icd.lsexe", negotiate, get_instance_proc_addr };

static RECORD: Record = Record { version: RECORD_VERSION, icds: [Some(&ICD), None, None, None], self_path: b"vol://system/bin/consumer.lsexe" };

/// Held by every test that installs a record, for as long as it relies on it.
///
/// THE RECORD IS PROCESS-WIDE AND CARGO RUNS TESTS ON SEVERAL THREADS. While every test installed the
/// same record that did not matter; the driver fixtures below each need a record of their own, and a
/// test that found another test's record installed would be asserting on drivers it never chose.
static RECORD_IN_USE: Mutex<()> = Mutex::new(());

/// Install `record` and clear the process-wide pools, holding the record until the guard drops.
///
/// THE POOLS ARE PROCESS-WIDE AND THE FIXTURES SHARE THEM, so a test that left a walk open would
/// decide the next test's answer. Each test resets what it uses before it uses it, and no test
/// asserts on a pool's occupancy.
fn install_record(record: &'static Record) -> MutexGuard<'static, ()> {
	// A FAILED TEST POISONS THE LOCK and the next one should still run: what the lock protects is
	// re-established from scratch below, so nothing the failure left behind is read.
	let guard = RECORD_IN_USE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
	unsafe { record::install(record) }.expect("the fixture record is at the current version");
	crate::directory::reset();
	crate::stream::reset();
	guard
}

/// Install the ordinary fixture record.
fn install() -> MutexGuard<'static, ()> {
	install_record(&RECORD)
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
fn a_directory_contains_exactly_what_the_record_names_under_it() {
	let _record = install();
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
	let _record = install();
	// THE UPSTREAM SEARCH PATH'S OWN DIRECTORIES, which on a Linux host would hold real drivers.
	for path in [&b"/usr/share/vulkan/icd.d"[..], b"/etc/vulkan/icd.d", b".", b"/"] {
		let path = c(path);
		assert!(unsafe { crate::directory::opendir(path.as_ptr()) }.is_null(), "nothing outside the record exists to this process");
	}
}

#[test]
fn a_manifest_in_the_record_reads_back_and_one_outside_it_does_not_open() {
	let _record = install();
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
	let _record = install();
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
	let _record = install();
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
	let _record = install();
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
	let _record = install();
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
	let _record = install();
	for path in [&b"vol://system/share/vulkan/icd.d/icd.json"[..], b"vol://system/lib/icd.lsexe"] {
		assert!(unsafe { crate::library::loader_platform_file_exists(c(path).as_ptr()) });
	}
	for path in [&b"/etc/passwd"[..], b"vol://system/lib/other.lsexe", b""] {
		assert!(!unsafe { crate::library::loader_platform_file_exists(c(path).as_ptr()) });
	}
}

// THE DRIVERS THE SUBSTRATE HAS TO REFUSE. Each is a negotiation entry point shaped the way a real
// driver's could be, and the four the decision names are here: an ICD answering 0, one answering only
// 1, one answering 7, and one offering the physical-device function through its own lookup. They are
// reached the way the pinned loader reaches a driver - through the substrate's provider lookup, asking
// for 7 - so what is asserted is what the loader would be told, not what a fixture says about itself.

/// What the pinned loader asks every driver for: its `CURRENT_LOADER_ICD_INTERFACE_VERSION`.
const LOADER_ASKS: u32 = 7;

/// A conforming driver whose own range tops out at `HIGHEST`: it answers the lower of the offer and
/// its ceiling, and declines an offer below 2, where there is no negotiation to have.
unsafe extern "C" fn conforming<const HIGHEST: u32>(version: *mut u32) -> i32 {
	let offered = unsafe { *version };
	if offered < 2 {
		return version::INCOMPATIBLE_DRIVER;
	}
	unsafe { *version = offered.min(HIGHEST) };
	version::SUCCESS
}

/// A driver that answers `ANSWER` whatever it is offered.
unsafe extern "C" fn answers<const ANSWER: u32>(version: *mut u32) -> i32 {
	unsafe { *version = ANSWER };
	version::SUCCESS
}

/// A driver that speaks 7 and nothing below it, conforming: offered 6, it declines.
unsafe extern "C" fn only_seven(version: *mut u32) -> i32 {
	if unsafe { *version } < 7 {
		return version::INCOMPATIBLE_DRIVER;
	}
	unsafe { *version = 7 };
	version::SUCCESS
}

/// A driver returning an error the pinned loader would have taken as an agreement: it treats only
/// `VK_ERROR_INCOMPATIBLE_DRIVER` as a failure.
unsafe extern "C" fn fails_oddly(version: *mut u32) -> i32 {
	unsafe { *version = 6 };
	// VK_ERROR_INITIALIZATION_FAILED.
	-3
}

/// The offer the recording driver last saw, or `u32::MAX` when it was not called.
static OFFER_SEEN: AtomicU32 = AtomicU32::new(u32::MAX);

/// A driver that records what it was offered and agrees to it.
unsafe extern "C" fn records_the_offer(version: *mut u32) -> i32 {
	OFFER_SEEN.store(unsafe { *version }, Ordering::Relaxed);
	version::SUCCESS
}

/// A version-7-style driver's lookup: it hands out its physical-device function by name, which is
/// how a driver at 7 and above may offer the third function instead of exporting it.
unsafe extern "C" fn offers_the_third_function(_instance: *mut c_void, name: *const c_char) -> *mut c_void {
	if unsafe { core::ffi::CStr::from_ptr(name) }.to_bytes() == crate::library::PHYSICAL_DEVICE_SYMBOL {
		return offers_the_third_function as *mut c_void;
	}
	core::ptr::null_mut()
}

const fn driver(manifest_path: &'static [u8], library_path: &'static [u8], negotiate: NegotiateFn, get_instance_proc_addr: record::GetInstanceProcAddrFn) -> Icd {
	Icd { manifest_path, manifest: MANIFEST, library_path, negotiate, get_instance_proc_addr }
}

static LOWEST_ONLY: Icd = driver(b"vol://fixture/icd.d/two.json", b"vol://fixture/two.lsexe", conforming::<2>, get_instance_proc_addr);
static BEYOND_SIX: Icd = driver(b"vol://fixture/icd.d/nine.json", b"vol://fixture/nine.lsexe", conforming::<9>, get_instance_proc_addr);
static ANSWERS_ZERO: Icd = driver(b"vol://fixture/icd.d/zero.json", b"vol://fixture/zero.lsexe", answers::<0>, get_instance_proc_addr);
static ANSWERS_ONE: Icd = driver(b"vol://fixture/icd.d/one.json", b"vol://fixture/one.lsexe", answers::<1>, get_instance_proc_addr);
static ANSWERS_SEVEN: Icd = driver(b"vol://fixture/icd.d/seven.json", b"vol://fixture/seven.lsexe", answers::<7>, get_instance_proc_addr);
static ONLY_SEVEN: Icd = driver(b"vol://fixture/icd.d/only-seven.json", b"vol://fixture/only-seven.lsexe", only_seven, get_instance_proc_addr);
static FAILS_ODDLY: Icd = driver(b"vol://fixture/icd.d/odd.json", b"vol://fixture/odd.lsexe", fails_oddly, get_instance_proc_addr);
static RECORDS_THE_OFFER: Icd = driver(b"vol://fixture/icd.d/recorder.json", b"vol://fixture/recorder.lsexe", records_the_offer, get_instance_proc_addr);
static THIRD_FUNCTION: Icd = driver(b"vol://fixture/icd.d/third.json", b"vol://fixture/third.lsexe", conforming::<7>, offers_the_third_function);

/// Four drivers in four slots, each settling differently - which is also what shows each slot's
/// entry point speaks for its own driver and not for whichever was resolved last.
static ENDS_AND_FLOOR: Record = Record { version: RECORD_VERSION, icds: [Some(&LOWEST_ONLY), Some(&BEYOND_SIX), Some(&ANSWERS_ZERO), Some(&ANSWERS_ONE)], self_path: b"vol://system/bin/consumer.lsexe" };
static CEILING: Record = Record { version: RECORD_VERSION, icds: [Some(&ANSWERS_SEVEN), Some(&ONLY_SEVEN), Some(&FAILS_ODDLY), Some(&RECORDS_THE_OFFER)], self_path: b"vol://system/bin/consumer.lsexe" };
static THIRD: Record = Record { version: RECORD_VERSION, icds: [Some(&THIRD_FUNCTION), None, None, None], self_path: b"vol://system/bin/consumer.lsexe" };

/// The negotiation entry point the loader is handed for the ICD at `library`.
fn negotiation_entry(library: &[u8]) -> *mut c_void {
	let handle = unsafe { crate::library::loader_platform_open_library(c(library).as_ptr()) };
	assert!(!handle.is_null(), "{:?} is in the installed record", core::str::from_utf8(library));
	unsafe { crate::library::loader_platform_get_proc_address(handle, c(crate::library::NEGOTIATE_SYMBOL).as_ptr()) }
}

/// Negotiate exactly as the pinned loader does: resolve the entry point through the substrate's
/// provider lookup, write what the loader supports, call it, and read back the result and the value.
fn negotiate_as_the_loader(library: &[u8], asks: u32) -> (i32, u32) {
	let entry = negotiation_entry(library);
	// NEVER NULL, and that matters as much as what it does: a null answer is what sends the loader to
	// the driver's own lookup for a negotiation function, around the substrate.
	assert!(!entry.is_null(), "the negotiation export must always resolve");
	let negotiate = unsafe { core::mem::transmute::<*mut c_void, NegotiateFn>(entry) };
	let mut version = asks;
	let result = unsafe { negotiate(&raw mut version) };
	(result, version)
}

#[test]
fn the_loader_is_never_handed_a_drivers_own_negotiation_function() {
	let _record = install_record(&ENDS_AND_FLOOR);
	let mut entries = [core::ptr::null_mut(); record::MAX_ICDS];
	for (slot, icd) in ENDS_AND_FLOOR.icds().enumerate() {
		entries[slot] = negotiation_entry(icd.library_path);
		assert_ne!(entries[slot], icd.negotiate as *mut c_void, "slot {slot}: the loader got the driver's own function, so nothing bounds what it agrees to");
	}
	for first in 0..entries.len() {
		for second in first + 1..entries.len() {
			assert_ne!(entries[first], entries[second], "two slots share one entry point, so one of them speaks for the wrong driver");
		}
	}
}

#[test]
fn the_admitted_range_holds_at_both_ends_through_the_substrate() {
	let _record = install_record(&ENDS_AND_FLOOR);
	// THE LOWEST: a driver whose range ends at 2 agrees at 2.
	assert_eq!(negotiate_as_the_loader(LOWEST_ONLY.library_path, LOADER_ASKS), (version::SUCCESS, 2));
	// THE HIGHEST, AND AN ICD OFFERING 7 AND ABOVE: brought down to 6 rather than agreed at its own
	// ceiling, because it is never offered more.
	assert_eq!(negotiate_as_the_loader(BEYOND_SIX.library_path, LOADER_ASKS), (version::SUCCESS, 6));
}

#[test]
fn an_icd_answering_zero_or_one_is_refused_by_the_substrate() {
	let _record = install_record(&ENDS_AND_FLOOR);
	// REFUSED WITH THE ONE RESULT THE LOADER TREATS AS A FAILURE, and the loader's own value is left
	// as it wrote it - the pinned loader would otherwise have run the first at 0 and the second at 1.
	assert_eq!(negotiate_as_the_loader(ANSWERS_ZERO.library_path, LOADER_ASKS), (version::INCOMPATIBLE_DRIVER, LOADER_ASKS));
	assert_eq!(negotiate_as_the_loader(ANSWERS_ONE.library_path, LOADER_ASKS), (version::INCOMPATIBLE_DRIVER, LOADER_ASKS));
}

#[test]
fn an_icd_answering_seven_is_refused_and_one_that_needs_seven_declines() {
	let _record = install_record(&CEILING);
	// A DRIVER THAT ANSWERS 7 ANYWAY - which the pinned loader would have accepted at 7, and then
	// asked the driver's own lookup for the physical-device function - is refused.
	assert_eq!(negotiate_as_the_loader(ANSWERS_SEVEN.library_path, LOADER_ASKS), (version::INCOMPATIBLE_DRIVER, LOADER_ASKS));
	// A CONFORMING DRIVER THAT SPEAKS ONLY 7 declines the offer of 6, and the decline is a refusal.
	assert_eq!(negotiate_as_the_loader(ONLY_SEVEN.library_path, LOADER_ASKS), (version::INCOMPATIBLE_DRIVER, LOADER_ASKS));
	// AND ANY ERROR IS ONE: the loader would have run this driver at the 6 it wrote beside its error.
	assert_eq!(negotiate_as_the_loader(FAILS_ODDLY.library_path, LOADER_ASKS), (version::INCOMPATIBLE_DRIVER, LOADER_ASKS));
}

#[test]
fn the_driver_is_never_offered_more_than_six() {
	let _record = install_record(&CEILING);
	// THE LOADER ASKS FOR 7 AND THE DRIVER IS TOLD 6.
	OFFER_SEEN.store(u32::MAX, Ordering::Relaxed);
	assert_eq!(negotiate_as_the_loader(RECORDS_THE_OFFER.library_path, LOADER_ASKS), (version::SUCCESS, 6));
	assert_eq!(OFFER_SEEN.load(Ordering::Relaxed), 6, "the driver was offered more than the substrate admits");
	// BELOW THE CEILING THE LOADER'S OWN NUMBER STANDS - the bound lowers an offer and never raises one.
	assert_eq!(negotiate_as_the_loader(RECORDS_THE_OFFER.library_path, 4), (version::SUCCESS, 4));
	assert_eq!(OFFER_SEEN.load(Ordering::Relaxed), 4);
	// AND BELOW THE FLOOR THERE IS NOTHING TO OFFER: refused, and the driver is never called.
	OFFER_SEEN.store(u32::MAX, Ordering::Relaxed);
	assert_eq!(negotiate_as_the_loader(RECORDS_THE_OFFER.library_path, 1), (version::INCOMPATIBLE_DRIVER, 1));
	assert_eq!(OFFER_SEEN.load(Ordering::Relaxed), u32::MAX, "a driver was called with nothing admitted to offer it");
}

#[test]
fn an_icd_offering_the_physical_device_function_cannot_reach_the_loader_with_it() {
	let _record = install_record(&THIRD);
	// THE FIXTURE REALLY DOES OFFER IT, through its own lookup - the way a driver at 7 may.
	let name = c(crate::library::PHYSICAL_DEVICE_SYMBOL);
	assert!(!unsafe { offers_the_third_function(core::ptr::null_mut(), name.as_ptr()) }.is_null());
	// THE LOADER ASKS THAT LOOKUP ONLY AT 7 AND ABOVE, and the substrate agrees no higher than 6 even
	// with a driver that would go to 7 - so that path is never taken.
	assert_eq!(negotiate_as_the_loader(THIRD_FUNCTION.library_path, LOADER_ASKS), (version::SUCCESS, 6));
	// AND AT 3 TO 6 THE LOADER ASKS THE SUBSTRATE INSTEAD, which refuses the third export by name.
	let handle = unsafe { crate::library::loader_platform_open_library(c(THIRD_FUNCTION.library_path).as_ptr()) };
	assert!(unsafe { crate::library::loader_platform_get_proc_address(handle, name.as_ptr()) }.is_null());
	assert_eq!(crate::library::refusal(crate::library::PHYSICAL_DEVICE_SYMBOL), Some(Refusal::ThirdExportRequired));
}

#[test]
fn an_icd_without_a_negotiation_function_is_refused_at_install() {
	// THE VERSION-1 SHAPE: a lookup and no negotiation function. It is refused before anything is
	// installed, so the record a launch already installed is still the one in force.
	let _record = install();
	let (manifest_path, manifest, library_path, self_path) = (c(b"/m.json"), c(b"{}"), c(b"/icd.lslib"), c(b"/self.lsexe"));
	let refused = unsafe { record::liber_foreign_install_icd(manifest_path.as_ptr() as *const u8, manifest.as_ptr() as *const u8, library_path.as_ptr() as *const u8, self_path.as_ptr() as *const u8, None, Some(get_instance_proc_addr)) };
	assert_eq!(refused, -1);
	let refused = unsafe { record::liber_foreign_install_icd(manifest_path.as_ptr() as *const u8, manifest.as_ptr() as *const u8, library_path.as_ptr() as *const u8, self_path.as_ptr() as *const u8, Some(negotiate), None) };
	assert_eq!(refused, -1, "and a negotiation function with no lookup is no ICD either");
	assert!(core::ptr::eq(record::installed().unwrap(), &RECORD), "a refused install replaced the record in force");
}

#[test]
fn the_exchange_agrees_with_the_rule_for_every_conforming_driver() {
	// THE CLOSED FORM AND THE MECHANISM ARE ONE RULE. `negotiate` says what a driver whose range tops
	// out at a version settles on; `exchange` is what actually runs. For every ceiling - the refused
	// ones below the floor, the admitted ones, and the ones above 6 - they must agree.
	let drivers: [(u32, NegotiateFn); 11] = [
		(0, conforming::<0>),
		(1, conforming::<1>),
		(2, conforming::<2>),
		(3, conforming::<3>),
		(4, conforming::<4>),
		(5, conforming::<5>),
		(6, conforming::<6>),
		(7, conforming::<7>),
		(8, conforming::<8>),
		(9, conforming::<9>),
		(99, conforming::<99>),
	];
	for (ceiling, driver) in drivers {
		let exchange = unsafe { version::exchange(driver, LOADER_ASKS) };
		assert_eq!(exchange.offered, Some(6), "ceiling {ceiling}: the driver was offered something other than 6");
		assert_eq!(exchange.outcome, version::negotiate(ceiling, false), "ceiling {ceiling}");
	}
}

#[test]
fn the_offer_and_the_judgement_are_the_decided_rule() {
	assert_eq!(version::offer(7), Some(6));
	assert_eq!(version::offer(u32::MAX), Some(6));
	assert_eq!(version::offer(6), Some(6));
	assert_eq!(version::offer(2), Some(2));
	assert_eq!(version::offer(1), None);
	assert_eq!(version::offer(0), None);

	for agreed in version::ADMITTED {
		assert_eq!(version::judge(6, version::SUCCESS, agreed), Negotiated::At(agreed));
	}
	// EACH WAY OF MISSING IS ITS OWN REFUSAL, so whoever hits one is told which fact stopped them.
	assert_eq!(version::judge(6, version::SUCCESS, 0), Negotiated::Refused(Refusal::MultiExportBootstrap));
	assert_eq!(version::judge(6, version::SUCCESS, 1), Negotiated::Refused(Refusal::NoNegotiationFunction));
	assert_eq!(version::judge(6, version::SUCCESS, 7), Negotiated::Refused(Refusal::QueryableRatherThanExported));
	assert_eq!(version::judge(6, version::SUCCESS, u32::MAX), Negotiated::Refused(Refusal::QueryableRatherThanExported));
	assert_eq!(version::judge(4, version::SUCCESS, 5), Negotiated::Refused(Refusal::NotOffered));
	assert_eq!(version::judge(6, version::INCOMPATIBLE_DRIVER, 6), Negotiated::Refused(Refusal::DriverDeclined));
	assert_eq!(version::judge(6, -3, 6), Negotiated::Refused(Refusal::DriverDeclined));
}
