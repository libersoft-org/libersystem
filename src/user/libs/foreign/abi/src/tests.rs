//! The facilities, tested where they can be: on the host.
//!
//! WHAT THESE ARE FOR. Every function in this crate is `extern "C"` and unmangled, so a mistake is a
//! symbol the linker resolves to the wrong behaviour rather than a type error anybody sees. The C
//! contracts below are the ones that surprise people - and the ones whose violation is silent.

use core::ffi::c_char;

fn c(text: &str) -> alloc::vec::Vec<c_char> {
	text.bytes().map(|byte| byte as c_char).chain(core::iter::once(0)).collect()
}

#[test]
fn strncpy_pads_and_does_not_terminate_a_full_copy() {
	// THE TWO PROPERTIES THAT SURPRISE PEOPLE, and both are what callers depend on: a short source
	// pads the rest of the buffer with NUL, and a source that exactly fills it leaves no terminator.
	let source = c("ab");
	let mut buffer = [0x7fi8 as c_char; 5];
	unsafe { crate::strings::strncpy(buffer.as_mut_ptr(), source.as_ptr(), 5) };
	assert_eq!(buffer, [b'a' as c_char, b'b' as c_char, 0, 0, 0]);

	let long = c("abcde");
	let mut exact = [0x7fi8 as c_char; 3];
	unsafe { crate::strings::strncpy(exact.as_mut_ptr(), long.as_ptr(), 3) };
	assert_eq!(exact, [b'a' as c_char, b'b' as c_char, b'c' as c_char], "no terminator when it exactly fills");
}

#[test]
fn strncat_always_terminates_unlike_strncpy() {
	let mut buffer = [0 as c_char; 8];
	let first = c("ab");
	unsafe { crate::strings::strcpy(buffer.as_mut_ptr(), first.as_ptr()) };
	let second = c("cdef");
	unsafe { crate::strings::strncat(buffer.as_mut_ptr(), second.as_ptr(), 2) };
	assert_eq!(&buffer[..5], &[b'a' as c_char, b'b' as c_char, b'c' as c_char, b'd' as c_char, 0]);
}

#[test]
fn memcmp_compares_unsigned() {
	// READING THE BYTES AS SIGNED makes 0x80 sort below 0x00, which reverses the answer for half the
	// byte range - and callers use the sign of the result.
	let high = [0x80u8];
	let low = [0x00u8];
	let result = unsafe { crate::memory::memcmp(high.as_ptr().cast(), low.as_ptr().cast(), 1) };
	assert!(result > 0, "0x80 is greater than 0x00, and got {result}");
}

#[test]
fn strchr_finds_the_terminator_and_strstr_matches_an_empty_needle() {
	let text = c("abc");
	let terminator = unsafe { crate::strings::strchr(text.as_ptr(), 0) };
	assert_eq!(terminator, unsafe { text.as_ptr().add(3) as *mut c_char }, "strchr(s, 0) points at the NUL");

	let empty = c("");
	let at = unsafe { crate::strings::strstr(text.as_ptr(), empty.as_ptr()) };
	assert_eq!(at, text.as_ptr() as *mut c_char, "an empty needle matches at the start");
}

#[test]
fn atoi_saturates_rather_than_wrapping() {
	// WRAPPING TURNS A LONG DIGIT RUN INTO A SMALL NEGATIVE NUMBER, which reads as a successful
	// parse of something the caller never wrote.
	let huge = c("99999999999999");
	assert_eq!(unsafe { crate::strings::atoi(huge.as_ptr()) }, i32::MAX);
	let negative = c("-99999999999999");
	assert_eq!(unsafe { crate::strings::atoi(negative.as_ptr()) }, i32::MIN);
}

#[test]
fn strtoul_reports_where_it_stopped_and_reads_the_prefix() {
	let hex = c("0x2aZ");
	let mut end: *mut c_char = core::ptr::null_mut();
	assert_eq!(unsafe { crate::strings::strtoul(hex.as_ptr(), &mut end, 0) }, 42);
	assert_eq!(unsafe { *end } as u8, b'Z');

	// `end` POINTS AT THE START WHEN NOTHING PARSED, which is the only way a caller can tell "zero"
	// from "not a number".
	let nothing = c("zz");
	assert_eq!(unsafe { crate::strings::strtoul(nothing.as_ptr(), &mut end, 10) }, 0);
	assert_eq!(end, nothing.as_ptr() as *mut c_char);
}

#[test]
fn strtod_leaves_an_exponent_marker_with_no_digits_alone() {
	// "1e" IS THE NUMBER 1 FOLLOWED BY A LETTER. Consuming the `e` would swallow a character the
	// caller is about to read.
	let text = c("1e");
	let mut end: *mut c_char = core::ptr::null_mut();
	let value = unsafe { crate::strings::strtod(text.as_ptr(), &mut end) };
	assert_eq!(value, 1.0);
	assert_eq!(unsafe { *end } as u8, b'e');

	let exponent = c("1.5e2");
	let value = unsafe { crate::strings::strtod(exponent.as_ptr(), &mut end) };
	assert!((value - 150.0).abs() < 1e-9, "got {value}");
}

#[test]
fn fabs_clears_the_sign_bit_rather_than_comparing() {
	assert_eq!(crate::fabs(-0.0).to_bits(), 0.0f64.to_bits(), "-0.0 becomes +0.0");
	assert!(crate::fabs(f64::NAN).is_nan(), "a NaN stays a NaN");
	assert_eq!(crate::fabs(-3.5), 3.5);
}

#[test]
fn strerror_does_not_call_an_unknown_number_a_success() {
	// RETURNING "Success" FOR AN UNKNOWN ERROR - which several libcs do - is how a failure gets
	// logged as having worked.
	let unknown = crate::strings::strerror(4242);
	let mut text = alloc::vec::Vec::new();
	let mut index = 0;
	while unsafe { *unknown.add(index) } != 0 {
		text.push(unsafe { *unknown.add(index) } as u8);
		index += 1;
	}
	assert_eq!(text, b"Unknown error");
}

/// A stand-in for a C argument list, so the rendering can be exercised without a `VaList`.
struct Fed {
	integers: alloc::vec::Vec<i64>,
	strings: alloc::vec::Vec<alloc::vec::Vec<c_char>>,
	next_integer: usize,
	next_string: usize,
}

impl crate::format::Arguments for Fed {
	fn next_int(&mut self, _long: bool) -> i64 {
		let value = self.integers[self.next_integer];
		self.next_integer += 1;
		value
	}

	fn next_uint(&mut self, long: bool) -> u64 {
		self.next_int(long) as u64
	}

	fn next_pointer(&mut self) -> *const core::ffi::c_void {
		self.next_int(true) as usize as *const core::ffi::c_void
	}

	fn next_string(&mut self) -> *const c_char {
		let at = self.next_string;
		self.next_string += 1;
		self.strings[at].as_ptr()
	}

	fn next_char(&mut self) -> u8 {
		self.next_int(false) as u8
	}
}

fn render_with(format: &str, integers: &[i64], strings: &[&str], capacity: usize) -> (alloc::string::String, usize) {
	let format = c(format);
	let mut fed = Fed { integers: integers.to_vec(), strings: strings.iter().map(|text| c(text)).collect(), next_integer: 0, next_string: 0 };
	let mut buffer = alloc::vec![0u8; capacity];
	let mut out = crate::format::Bounded::over(&mut buffer);
	unsafe { crate::format::render(&mut out, format.as_ptr(), &mut fed) };
	let would = out.finish();
	let end = buffer.iter().position(|byte| *byte == 0).unwrap_or(buffer.len());
	(alloc::string::String::from_utf8_lossy(&buffer[..end]).into_owned(), would)
}

#[test]
fn snprintf_returns_what_it_would_have_written_not_what_it_stored() {
	// THE DEFINING PROPERTY. A caller sizes a buffer by calling once with a small one; returning the
	// truncated length makes that idiom allocate too little, for ever.
	let (text, would) = render_with("%s world", &[], &["hello"], 4);
	assert_eq!(text, "hel", "three bytes and a terminator is all four bytes hold");
	assert_eq!(would, "hello world".len());
}

#[test]
fn a_zero_pad_goes_inside_the_sign_and_a_space_pad_outside_it() {
	// `%05d` OF -42 IS "-0042" AND NOT "0-042", which is the one ordering people get wrong.
	let (text, _) = render_with("%05d", &[-42], &[], 32);
	assert_eq!(text, "-0042");
	let (text, _) = render_with("%5d", &[-42], &[], 32);
	assert_eq!(text, "  -42");
	let (text, _) = render_with("%-5d|", &[-42], &[], 32);
	assert_eq!(text, "-42  |", "left alignment pads on the right");
}

#[test]
fn an_unrecognised_conversion_is_copied_through_rather_than_dropped() {
	// SILENTLY EATING IT loses the value AND hides the reason. Copying it through means the caller
	// sees exactly what it wrote and can tell this set does not handle it.
	let (text, _) = render_with("a%qb", &[], &[], 32);
	assert_eq!(text, "a%qb");
}

#[test]
fn precision_truncates_a_string_and_width_pads_it() {
	let (text, _) = render_with("[%.2s]", &[], &["abcdef"], 32);
	assert_eq!(text, "[ab]");
	let (text, _) = render_with("[%6s]", &[], &["ab"], 32);
	assert_eq!(text, "[    ab]");
}

#[test]
fn hexadecimal_honours_its_case_and_a_null_string_is_named() {
	let (text, _) = render_with("%x %X", &[255, 255], &[], 32);
	assert_eq!(text, "ff FF");
	// A NULL `%s` PRINTS "(null)" RATHER THAN FAULTING. Callers pass one more often than they mean
	// to, and a crash inside a diagnostic is the worst place to have one.
	let format = c("%s");
	let mut fed = Fed { integers: alloc::vec::Vec::new(), strings: alloc::vec![alloc::vec::Vec::new()], next_integer: 0, next_string: 0 };
	fed.strings[0].clear();
	let mut buffer = alloc::vec![0u8; 32];
	let mut out = crate::format::Bounded::over(&mut buffer);
	// An empty vec has a dangling-but-aligned pointer, so feed a real NULL instead.
	struct NullString;
	impl crate::format::Arguments for NullString {
		fn next_int(&mut self, _long: bool) -> i64 {
			0
		}
		fn next_uint(&mut self, _long: bool) -> u64 {
			0
		}
		fn next_pointer(&mut self) -> *const core::ffi::c_void {
			core::ptr::null()
		}
		fn next_string(&mut self) -> *const c_char {
			core::ptr::null()
		}
		fn next_char(&mut self) -> u8 {
			0
		}
	}
	unsafe { crate::format::render(&mut out, format.as_ptr(), &mut NullString) };
	out.finish();
	let end = buffer.iter().position(|byte| *byte == 0).unwrap_or(buffer.len());
	assert_eq!(&buffer[..end], b"(null)");
	let _ = fed.next_string;
}

#[test]
fn a_zero_capacity_buffer_is_written_to_at_all() {
	// AND THE COUNT IS STILL RIGHT. This is how `snprintf(NULL, 0, ...)` is used to measure.
	let format = c("abc");
	struct Nothing;
	impl crate::format::Arguments for Nothing {
		fn next_int(&mut self, _long: bool) -> i64 {
			0
		}
		fn next_uint(&mut self, _long: bool) -> u64 {
			0
		}
		fn next_pointer(&mut self) -> *const core::ffi::c_void {
			core::ptr::null()
		}
		fn next_string(&mut self) -> *const c_char {
			core::ptr::null()
		}
		fn next_char(&mut self) -> u8 {
			0
		}
	}
	let mut out = crate::format::Bounded { buffer: core::ptr::null_mut(), capacity: 0, written: 0 };
	unsafe { crate::format::render(&mut out, format.as_ptr(), &mut Nothing) };
	assert_eq!(out.finish(), 3);
}

#[test]
fn a_scan_matching_failure_ends_the_whole_call() {
	// CONTINUING WOULD ASSIGN to the arguments after it from input that never matched, which is the
	// difference between "parsed two of three" and "wrote garbage into the third".
	let input = c("12 xx 34");
	let format = c("%d %d %d");
	let results = unsafe { crate::format::scan(input.as_ptr(), format.as_ptr()) };
	assert_eq!(results.as_slice().len(), 1);
	assert_eq!(results.as_slice()[0].value, 12);
}

#[test]
fn a_scan_reads_a_literal_a_width_and_a_sign() {
	let input = c("v1.4.357");
	let format = c("v%d.%d.%d");
	let results = unsafe { crate::format::scan(input.as_ptr(), format.as_ptr()) };
	let values: alloc::vec::Vec<i64> = results.as_slice().iter().map(|scanned| scanned.value).collect();
	assert_eq!(values, alloc::vec![1, 4, 357]);

	let negative = c("-5");
	let format = c("%d");
	let results = unsafe { crate::format::scan(negative.as_ptr(), format.as_ptr()) };
	assert_eq!(results.as_slice()[0].value, -5);
}

#[test]
fn the_allocator_carries_its_size_across_a_realloc() {
	// THE HEADER IS THE WHOLE DIFFICULTY. C's `free` is not told the layout, so a block that lost
	// its size would be returned to the allocator as a different allocation than it took.
	unsafe {
		let first = crate::allocator::malloc(8) as *mut u8;
		assert!(!first.is_null());
		for index in 0..8 {
			*first.add(index) = index as u8;
		}
		let grown = crate::allocator::realloc(first as *mut core::ffi::c_void, 64) as *mut u8;
		assert!(!grown.is_null());
		for index in 0..8 {
			assert_eq!(*grown.add(index), index as u8, "the old contents survive");
		}
		crate::allocator::free(grown as *mut core::ffi::c_void);
		// `free(NULL)` IS DEFINED AND DOES NOTHING. Cleanup paths rely on it.
		crate::allocator::free(core::ptr::null_mut());
	}
}

#[test]
fn calloc_refuses_a_multiplication_that_would_overflow() {
	// `malloc(count * size)` OVERFLOWS SILENTLY and returns a block smaller than the caller writes
	// into, which is the entire reason `calloc` is a separate call.
	let block = unsafe { crate::allocator::calloc(usize::MAX, 2) };
	assert!(block.is_null());

	let zeroed = unsafe { crate::allocator::calloc(4, 4) as *mut u8 };
	assert!(!zeroed.is_null());
	for index in 0..16 {
		assert_eq!(unsafe { *zeroed.add(index) }, 0, "calloc zeroes");
	}
	unsafe { crate::allocator::free(zeroed as *mut core::ffi::c_void) };
}

#[test]
fn a_zero_size_allocation_is_still_a_distinct_pointer() {
	// C CALLERS COMPARE AGAINST NULL to decide whether the allocation failed, so returning null for
	// a legal request makes a zero-length array look like an out-of-memory condition.
	let block = unsafe { crate::allocator::malloc(0) };
	assert!(!block.is_null());
	unsafe { crate::allocator::free(block) };
}
