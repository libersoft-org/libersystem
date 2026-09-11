//! The string and conversion functions the inventory names.
//!
//! EVERY ONE OF THESE IS A C STRING WALK, which means it trusts a NUL the caller promised is there.
//! That is the C contract and this crate cannot improve on it; what it can do is not add its own
//! mistakes on top, which is why each walk below is bounded by the same thing C says bounds it and
//! nothing is assumed about the length in advance.

use core::ffi::{c_char, c_void};
use core::ptr;

/// How many bytes precede the terminating NUL.
unsafe fn length(text: *const c_char) -> usize {
	let mut count = 0;
	while unsafe { *text.add(count) } != 0 {
		count += 1;
	}
	count
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strlen(text: *const c_char) -> usize {
	unsafe { length(text) }
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strcmp(left: *const c_char, right: *const c_char) -> i32 {
	let mut index = 0;
	loop {
		let (a, b) = unsafe { (*left.add(index) as u8, *right.add(index) as u8) };
		if a != b {
			return i32::from(a) - i32::from(b);
		}
		if a == 0 {
			return 0;
		}
		index += 1;
	}
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strncmp(left: *const c_char, right: *const c_char, count: usize) -> i32 {
	for index in 0..count {
		let (a, b) = unsafe { (*left.add(index) as u8, *right.add(index) as u8) };
		if a != b {
			return i32::from(a) - i32::from(b);
		}
		if a == 0 {
			return 0;
		}
	}
	0
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strcpy(destination: *mut c_char, source: *const c_char) -> *mut c_char {
	let count = unsafe { length(source) };
	unsafe { ptr::copy_nonoverlapping(source, destination, count + 1) };
	destination
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strncpy(destination: *mut c_char, source: *const c_char, count: usize) -> *mut c_char {
	let available = unsafe { length(source) }.min(count);
	unsafe { ptr::copy_nonoverlapping(source, destination, available) };
	// `strncpy` PADS WITH NUL TO `count` AND DOES NOT TERMINATE A FULL COPY, both of which surprise
	// people and both of which callers depend on. Doing either differently changes what the caller
	// reads back.
	for index in available..count {
		unsafe { *destination.add(index) = 0 };
	}
	destination
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strncat(destination: *mut c_char, source: *const c_char, count: usize) -> *mut c_char {
	let end = unsafe { length(destination) };
	let available = unsafe { length(source) }.min(count);
	unsafe {
		ptr::copy_nonoverlapping(source, destination.add(end), available);
		// `strncat` ALWAYS TERMINATES, unlike `strncpy`. The two differ here and the difference is
		// the usual source of a one-byte overrun.
		*destination.add(end + available) = 0;
	}
	destination
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strchr(text: *const c_char, character: i32) -> *mut c_char {
	let wanted = character as u8;
	let mut index = 0;
	loop {
		let byte = unsafe { *text.add(index) as u8 };
		if byte == wanted {
			// THE TERMINATOR IS FINDABLE. `strchr(s, 0)` returns a pointer to the NUL, which is
			// defined behaviour callers use to find the end.
			return unsafe { text.add(index) as *mut c_char };
		}
		if byte == 0 {
			return ptr::null_mut();
		}
		index += 1;
	}
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strrchr(text: *const c_char, character: i32) -> *mut c_char {
	let wanted = character as u8;
	let mut found = ptr::null_mut();
	let mut index = 0;
	loop {
		let byte = unsafe { *text.add(index) as u8 };
		if byte == wanted {
			found = unsafe { text.add(index) as *mut c_char };
		}
		if byte == 0 {
			return found;
		}
		index += 1;
	}
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strstr(haystack: *const c_char, needle: *const c_char) -> *mut c_char {
	let needle_len = unsafe { length(needle) };
	// AN EMPTY NEEDLE MATCHES AT THE START, which C specifies and which a loop written the obvious
	// way returns NULL for.
	if needle_len == 0 {
		return haystack as *mut c_char;
	}
	let haystack_len = unsafe { length(haystack) };
	if needle_len > haystack_len {
		return ptr::null_mut();
	}
	for start in 0..=(haystack_len - needle_len) {
		let mut matched = true;
		for offset in 0..needle_len {
			if unsafe { *haystack.add(start + offset) } != unsafe { *needle.add(offset) } {
				matched = false;
				break;
			}
		}
		if matched {
			return unsafe { haystack.add(start) as *mut c_char };
		}
	}
	ptr::null_mut()
}

/// The re-entrant `strtok`, which is what the pinned sources actually call.
///
/// THE STATE IS THE CALLER'S, which is the whole difference from `strtok` and the reason this crate
/// does not provide that one: `strtok` keeps a static cursor, and a static cursor is state this
/// crate would own on behalf of code it cannot see.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn thread_safe_strtok(text: *mut c_char, delimiters: *const c_char, state: *mut *mut c_char) -> *mut c_void {
	let mut cursor = match text.is_null() {
		true => unsafe { *state },
		false => text,
	};
	if cursor.is_null() {
		return ptr::null_mut();
	}
	let is_delimiter = |byte: u8| -> bool {
		let mut index = 0;
		loop {
			let candidate = unsafe { *delimiters.add(index) as u8 };
			if candidate == 0 {
				return false;
			}
			if candidate == byte {
				return true;
			}
			index += 1;
		}
	};
	// Skip the leading delimiters; a run of them is one separator, not several empty tokens.
	while unsafe { *cursor } != 0 && is_delimiter(unsafe { *cursor as u8 }) {
		cursor = unsafe { cursor.add(1) };
	}
	if unsafe { *cursor } == 0 {
		unsafe { *state = cursor };
		return ptr::null_mut();
	}
	let token = cursor;
	while unsafe { *cursor } != 0 && !is_delimiter(unsafe { *cursor as u8 }) {
		cursor = unsafe { cursor.add(1) };
	}
	if unsafe { *cursor } != 0 {
		unsafe { *cursor = 0 };
		cursor = unsafe { cursor.add(1) };
	}
	unsafe { *state = cursor };
	token as *mut c_void
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn tolower(character: i32) -> i32 {
	match character {
		// THE ASCII RANGE ONLY, deliberately. A locale-aware `tolower` needs a locale, and this
		// system has none; pretending otherwise would map bytes nobody asked it to.
		0x41..=0x5a => character + 0x20,
		_ => character,
	}
}

/// Parse a decimal integer, C's rules: leading space skipped, optional sign, stop at the first
/// non-digit, and no way to report an error.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn atoi(text: *const c_char) -> i32 {
	let mut index = 0;
	while matches!(unsafe { *text.add(index) as u8 }, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
		index += 1;
	}
	let negative = match unsafe { *text.add(index) as u8 } {
		b'-' => {
			index += 1;
			true
		}
		b'+' => {
			index += 1;
			false
		}
		_ => false,
	};
	let mut value: i64 = 0;
	loop {
		let byte = unsafe { *text.add(index) as u8 };
		let Some(digit) = (byte as char).to_digit(10) else {
			break;
		};
		// SATURATING RATHER THAN WRAPPING. C says overflow here is undefined; wrapping would turn a
		// long digit run into a small negative number, which reads as a successful parse.
		value = value.saturating_mul(10).saturating_add(i64::from(digit));
		index += 1;
	}
	let signed = match negative {
		true => -value,
		false => value,
	};
	signed.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Parse a decimal floating-point number. Enough of C's grammar for the pinned sources: an optional
/// sign, digits, a fractional part and an exponent.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn strtod(text: *const c_char, end: *mut *mut c_char) -> f64 {
	let mut index = 0;
	while matches!(unsafe { *text.add(index) as u8 }, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
		index += 1;
	}
	let start = index;
	let negative = match unsafe { *text.add(index) as u8 } {
		b'-' => {
			index += 1;
			true
		}
		b'+' => {
			index += 1;
			false
		}
		_ => false,
	};
	let mut whole: f64 = 0.0;
	let mut digits = 0;
	while let Some(digit) = (unsafe { *text.add(index) as u8 } as char).to_digit(10) {
		whole = whole * 10.0 + f64::from(digit);
		digits += 1;
		index += 1;
	}
	if unsafe { *text.add(index) as u8 } == b'.' {
		index += 1;
		let mut scale = 0.1;
		while let Some(digit) = (unsafe { *text.add(index) as u8 } as char).to_digit(10) {
			whole += f64::from(digit) * scale;
			scale *= 0.1;
			digits += 1;
			index += 1;
		}
	}
	if digits == 0 {
		// NOTHING PARSED. `end` goes back to the start so the caller can tell, and the value is zero
		// because C has nowhere else to put "no".
		if !end.is_null() {
			unsafe { *end = text as *mut c_char };
		}
		return 0.0;
	}
	if matches!(unsafe { *text.add(index) as u8 }, b'e' | b'E') {
		let mark = index;
		index += 1;
		let exponent_negative = match unsafe { *text.add(index) as u8 } {
			b'-' => {
				index += 1;
				true
			}
			b'+' => {
				index += 1;
				false
			}
			_ => false,
		};
		let mut exponent: i32 = 0;
		let mut exponent_digits = 0;
		while let Some(digit) = (unsafe { *text.add(index) as u8 } as char).to_digit(10) {
			exponent = exponent.saturating_mul(10).saturating_add(digit as i32);
			exponent_digits += 1;
			index += 1;
		}
		match exponent_digits {
			// AN `e` WITH NO DIGITS IS NOT PART OF THE NUMBER. "1e" parses as 1 and leaves `e` for
			// the caller; consuming it would swallow a character the caller is about to read.
			0 => index = mark,
			_ => {
				let steps = match exponent_negative {
					true => -exponent,
					false => exponent,
				};
				let mut power: f64 = 1.0;
				for _ in 0..steps.abs() {
					power *= 10.0;
				}
				whole = match steps < 0 {
					true => whole / power,
					false => whole * power,
				};
			}
		}
	}
	if !end.is_null() {
		unsafe { *end = text.add(index) as *mut c_char };
	}
	let _ = start;
	match negative {
		true => -whole,
		false => whole,
	}
}

/// The message for an `errno` value. The set is the one the bootstrap sysroot's `<errno.h>` defines,
/// because that is the set the pinned sources can produce.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn strerror(number: i32) -> *mut c_char {
	let text: &'static [u8] = match number {
		1 => b"Operation not permitted\0",
		2 => b"No such file or directory\0",
		4 => b"Interrupted system call\0",
		5 => b"Input/output error\0",
		9 => b"Bad file descriptor\0",
		11 => b"Resource temporarily unavailable\0",
		12 => b"Cannot allocate memory\0",
		13 => b"Permission denied\0",
		14 => b"Bad address\0",
		16 => b"Device or resource busy\0",
		17 => b"File exists\0",
		19 => b"No such device\0",
		20 => b"Not a directory\0",
		21 => b"Is a directory\0",
		22 => b"Invalid argument\0",
		23 => b"Too many open files in system\0",
		24 => b"Too many open files\0",
		28 => b"No space left on device\0",
		34 => b"Numerical result out of range\0",
		38 => b"Function not implemented\0",
		75 => b"Value too large for defined data type\0",
		95 => b"Operation not supported\0",
		// A NUMBER NOBODY DEFINED GETS A HONEST MESSAGE rather than a fabricated one. Returning
		// "Success" for an unknown error - which several libcs do - is how a failure gets logged as
		// having worked.
		_ => b"Unknown error\0",
	};
	text.as_ptr() as *mut c_char
}
