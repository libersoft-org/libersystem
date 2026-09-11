//! `abort`, the assertion hook, and the `errno` accessor.

use core::ffi::c_char;
use core::sync::atomic::AtomicI32;

/// THE PROCESS'S `errno`, AND THERE IS EXACTLY ONE OF IT. A per-thread `errno` is what thread-local
/// storage is normally for, and the pinned configuration has no TLS at all - pass 1 measured zero
/// TLS symbols and zero TLS relocations on all three targets. A single location is therefore correct
/// here rather than a simplification, and the TLS item is where that stops being true if it ever
/// does.
static ERRNO: AtomicI32 = AtomicI32::new(0);

#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn __liber_errno_location() -> *mut i32 {
	ERRNO.as_ptr()
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn abort() -> ! {
	// ABORT IS NOT AN EXIT CODE, it is a fault. The foreign code that calls it has decided its own
	// invariants are broken, and continuing past that point runs code whose assumptions are already
	// known to be false.
	crate::sink::report(b"foreign: abort\n");
	crate::sink::stop()
}

/// What the bootstrap sysroot's `assert` expands to when `NDEBUG` is not set.
///
/// THE EXPRESSION, THE FILE AND THE LINE ARE ALL PRINTED, because an assertion that says only that
/// one failed sends the reader back to the source to find out which - and the whole value of an
/// assertion is that it names the thing that was not true.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn __liber_assert_failed(expression: *const c_char, file: *const c_char, line: i32) -> ! {
	crate::sink::report(b"foreign: assertion failed: ");
	unsafe { print_c_string(expression) };
	crate::sink::report(b" at ");
	unsafe { print_c_string(file) };
	crate::sink::report(b":");
	let mut digits = [0u8; 12];
	let text = format_decimal(line, &mut digits);
	crate::sink::report(text);
	crate::sink::report(b"\n");
	crate::sink::stop()
}

unsafe fn print_c_string(text: *const c_char) {
	if text.is_null() {
		crate::sink::report(b"(null)");
		return;
	}
	let mut length = 0;
	while unsafe { *text.add(length) } != 0 {
		length += 1;
	}
	crate::sink::report(unsafe { core::slice::from_raw_parts(text as *const u8, length) });
}

fn format_decimal(value: i32, buffer: &mut [u8; 12]) -> &[u8] {
	if value == 0 {
		buffer[0] = b'0';
		return &buffer[..1];
	}
	let negative = value < 0;
	let mut remaining = value.unsigned_abs();
	let mut end = buffer.len();
	while remaining > 0 {
		end -= 1;
		buffer[end] = b'0' + (remaining % 10) as u8;
		remaining /= 10;
	}
	if negative {
		end -= 1;
		buffer[end] = b'-';
	}
	&buffer[end..]
}

const _: () = {
	// A COMPILE-TIME REMINDER that the decimal buffer holds the widest `i32` plus its sign. The
	// buffer is twelve bytes and "-2147483648" is eleven.
	assert!(12 > "-2147483648".len());
};
