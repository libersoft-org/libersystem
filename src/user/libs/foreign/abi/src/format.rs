//! `snprintf`, `vsnprintf`, `sscanf`, `fputs`, `fputc` and the `stderr` stream.
//!
//! THE ONE OUTPUT PATH IS THE PROCESS'S OWN DIAGNOSTIC STREAM. `stderr` here is a sentinel rather
//! than a file: this system has no ambient file namespace, and the whole reason the discovery item
//! exists is that a foreign stack must not reach one. `fputs` to anything else is refused rather
//! than quietly discarded, because a write that silently goes nowhere is a message somebody spent an
//! afternoon looking for.
//!
//! THE CONVERSION SET IS THE ONE THE PINNED SOURCES USE, and it is bounded on purpose. A complete
//! `printf` is a large surface with its own parsing bugs, and every conversion beyond the measured
//! set is a general C library arriving one `%` at a time - the same rule the facilities item states
//! for symbols, applied inside one of them. An unrecognised conversion is COPIED THROUGH verbatim
//! rather than skipped, so a caller sees what it asked for instead of losing it.

use core::ffi::{c_char, c_int, c_void};

/// Where a conversion's value comes from.
///
/// AN INTERFACE RATHER THAN A `VaList` DIRECTLY, and the reason is that the format parsing is where
/// the bugs are and `VaList` cannot be constructed by a test. Field widths, precision, the sign and
/// zero-pad interaction, an unrecognised conversion - all of that is decided before any argument is
/// read, and all of it is now reachable from a host fixture. The `VaList` implementation below is
/// four forwarding lines.
pub trait Arguments {
	fn next_int(&mut self, long: bool) -> i64;
	fn next_uint(&mut self, long: bool) -> u64;
	fn next_pointer(&mut self) -> *const c_void;
	fn next_string(&mut self) -> *const c_char;
	fn next_char(&mut self) -> u8;
}

/// The sentinel `stderr` points at. Its address is the only thing that matters: nothing dereferences
/// it, and the write path compares against it.
// THE NAME IS A C NAME AND THE LINT IS A RUST CONVENTION. Suppressed here, at the two statics it
// applies to, rather than crate-wide - the rest of this crate should still be held to it.
#[allow(non_upper_case_globals)]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub static mut __liber_stderr_stream: u8 = 0;

/// `stderr` itself - a pointer-sized object holding the sentinel's address, which is what a C
/// `FILE *` is from the caller's side.
#[allow(non_upper_case_globals)]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub static mut stderr: *mut c_void = core::ptr::null_mut();

/// Fill in `stderr` before anything uses it.
///
/// A STATIC INITIALISER CANNOT TAKE ANOTHER STATIC'S ADDRESS at compile time in a way that survives
/// position-independent linking, so the pointer is set here. The substrate calls this once before it
/// hands control to foreign code, which is the same point the pinned configuration's own
/// `loader_init_library` runs - a fact pass 1 recorded by name.
pub fn attach_streams() {
	unsafe { stderr = &raw mut __liber_stderr_stream as *mut c_void };
}

fn is_stderr(stream: *mut c_void) -> bool {
	core::ptr::eq(stream as *const u8, &raw const __liber_stderr_stream)
}

/// A bounded writer over the caller's buffer that counts what it WOULD have written.
///
/// THE COUNT IS NOT THE BYTES STORED, and that is `snprintf`'s defining property: it returns the
/// length the output would have had, so a caller can size a buffer by calling once with a small one.
/// Returning the truncated length instead makes that idiom allocate too little, for ever.
pub struct Bounded {
	pub buffer: *mut u8,
	pub capacity: usize,
	pub written: usize,
}

impl Bounded {
	fn push(&mut self, byte: u8) {
		// THE LAST BYTE IS RESERVED FOR THE TERMINATOR, which is why this compares against
		// `capacity - 1` rather than `capacity`.
		if !self.buffer.is_null() && self.capacity > 0 && self.written + 1 < self.capacity {
			unsafe { *self.buffer.add(self.written) = byte };
		}
		self.written += 1;
	}

	fn push_all(&mut self, bytes: &[u8]) {
		for byte in bytes {
			self.push(*byte);
		}
	}

	fn push_repeated(&mut self, byte: u8, count: usize) {
		for _ in 0..count {
			self.push(byte);
		}
	}

	pub fn terminate(&mut self) {
		if !self.buffer.is_null() && self.capacity > 0 {
			let at = self.written.min(self.capacity - 1);
			unsafe { *self.buffer.add(at) = 0 };
		}
	}
}

/// What a conversion asked for, as far as this set reads it.
pub struct Conversion {
	long: u8,
	size_t: bool,
	zero_pad: bool,
	left: bool,
	width: usize,
	precision: Option<usize>,
}

impl Bounded {
	/// A writer over a caller's buffer.
	pub fn over(buffer: &mut [u8]) -> Bounded {
		Bounded { buffer: buffer.as_mut_ptr(), capacity: buffer.len(), written: 0 }
	}

	/// Finish and answer what `snprintf` would: the length the output WOULD have had.
	pub fn finish(&mut self) -> usize {
		self.terminate();
		self.written
	}
}

pub fn digits_into(buffer: &mut [u8; 32], mut value: u64, radix: u64, upper: bool) -> usize {
	if value == 0 {
		buffer[buffer.len() - 1] = b'0';
		return buffer.len() - 1;
	}
	let alphabet: &[u8] = match upper {
		true => b"0123456789ABCDEF",
		false => b"0123456789abcdef",
	};
	let mut end = buffer.len();
	while value > 0 {
		end -= 1;
		buffer[end] = alphabet[(value % radix) as usize];
		value /= radix;
	}
	end
}

pub fn emit_padded(out: &mut Bounded, body: &[u8], sign: Option<u8>, spec: &Conversion) {
	let length = body.len() + usize::from(sign.is_some());
	let pad = spec.width.saturating_sub(length);
	// THE ORDER OF THE THREE PIECES IS THE WHOLE OF FIELD PADDING: spaces go outside a sign and
	// zeroes go inside it, so `%05d` of -42 is "-0042" and not "0-042".
	if !spec.left && !spec.zero_pad {
		out.push_repeated(b' ', pad);
	}
	if let Some(byte) = sign {
		out.push(byte);
	}
	if !spec.left && spec.zero_pad {
		out.push_repeated(b'0', pad);
	}
	out.push_all(body);
	if spec.left {
		out.push_repeated(b' ', pad);
	}
}

pub unsafe fn c_string(text: *const c_char) -> &'static [u8] {
	if text.is_null() {
		return b"(null)";
	}
	let mut length = 0;
	while unsafe { *text.add(length) } != 0 {
		length += 1;
	}
	unsafe { core::slice::from_raw_parts(text as *const u8, length) }
}

/// Render `format` into `out`, taking values from `args`.
///
/// PUBLIC BECAUSE THE FIXTURES CALL IT. The variadic entry points are compiled only for the target,
/// so on the host this IS the function under test - which is the right one to test anyway: the
/// parsing, the padding and the unrecognised-conversion path all decide before an argument is read.
pub unsafe fn render(out: &mut Bounded, format: *const c_char, args: &mut dyn Arguments) {
	let mut index = 0;
	loop {
		let byte = unsafe { *format.add(index) as u8 };
		if byte == 0 {
			break;
		}
		if byte != b'%' {
			out.push(byte);
			index += 1;
			continue;
		}
		let start = index;
		index += 1;
		let mut spec = Conversion { long: 0, size_t: false, zero_pad: false, left: false, width: 0, precision: None };
		loop {
			match unsafe { *format.add(index) as u8 } {
				b'-' => spec.left = true,
				b'0' => spec.zero_pad = true,
				b'+' | b' ' | b'#' => {}
				_ => break,
			}
			index += 1;
		}
		while let Some(digit) = (unsafe { *format.add(index) as u8 } as char).to_digit(10) {
			spec.width = spec.width * 10 + digit as usize;
			index += 1;
		}
		if unsafe { *format.add(index) as u8 } == b'.' {
			index += 1;
			let mut value = 0;
			while let Some(digit) = (unsafe { *format.add(index) as u8 } as char).to_digit(10) {
				value = value * 10 + digit as usize;
				index += 1;
			}
			spec.precision = Some(value);
		}
		loop {
			match unsafe { *format.add(index) as u8 } {
				b'l' => spec.long += 1,
				b'z' => spec.size_t = true,
				b'h' | b'j' | b't' => {}
				_ => break,
			}
			index += 1;
		}
		let conversion = unsafe { *format.add(index) as u8 };
		index += 1;
		let mut buffer = [0u8; 32];
		match conversion {
			b'%' => out.push(b'%'),
			b'c' => {
				out.push(args.next_char());
			}
			b's' => {
				let text: *const c_char = args.next_string();
				let bytes = unsafe { c_string(text) };
				let shown = match spec.precision {
					Some(limit) => &bytes[..limit.min(bytes.len())],
					None => bytes,
				};
				emit_padded(out, shown, None, &spec);
			}
			b'd' | b'i' => {
				let value: i64 = args.next_int(spec.long > 0 || spec.size_t);
				let sign = (value < 0).then_some(b'-');
				let at = digits_into(&mut buffer, value.unsigned_abs(), 10, false);
				emit_padded(out, &buffer[at..], sign, &spec);
			}
			b'u' => {
				let value: u64 = args.next_uint(spec.long > 0 || spec.size_t);
				let at = digits_into(&mut buffer, value, 10, false);
				emit_padded(out, &buffer[at..], None, &spec);
			}
			b'x' | b'X' => {
				let value: u64 = args.next_uint(spec.long > 0 || spec.size_t);
				let at = digits_into(&mut buffer, value, 16, conversion == b'X');
				emit_padded(out, &buffer[at..], None, &spec);
			}
			b'p' => {
				let value: *const c_void = args.next_pointer();
				out.push_all(b"0x");
				let at = digits_into(&mut buffer, value as usize as u64, 16, false);
				out.push_all(&buffer[at..]);
			}
			_ => {
				// AN UNRECOGNISED CONVERSION IS COPIED THROUGH, not dropped. The caller then sees
				// exactly what it wrote and can tell that this set does not handle it; silently
				// eating it loses the value AND hides the reason.
				for back in start..index {
					out.push(unsafe { *format.add(back) as u8 });
				}
			}
		}
	}
}

/// One value `scan` parsed, and how wide the caller's pointer for it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Scanned {
	pub value: i64,
	pub long: bool,
}

/// THE MOST CONVERSIONS ONE `sscanf` CALL MAY ASSIGN. The pinned sources parse a version triple at
/// most; a bound makes the scan allocation-free and makes a format string with a hundred
/// conversions a refusal rather than a surprise.
pub const MAX_SCANNED: usize = 8;

/// Parse `input` against `format`, returning the values in order.
///
/// SEPARATE FROM THE VARIADIC ENTRY POINT so the parsing can be exercised on the host, which is
/// where its rules live: a matching failure ends the WHOLE call, whitespace in the format matches any
/// run of it, and a literal must match exactly.
pub unsafe fn scan(input: *const c_char, format: *const c_char) -> ScanResults {
	let mut results = ScanResults { values: [Scanned { value: 0, long: false }; MAX_SCANNED], count: 0 };
	let mut at = 0;
	let mut index = 0;
	loop {
		let byte = unsafe { *format.add(index) as u8 };
		if byte == 0 {
			break;
		}
		if byte.is_ascii_whitespace() {
			while unsafe { *input.add(at) as u8 }.is_ascii_whitespace() {
				at += 1;
			}
			index += 1;
			continue;
		}
		if byte != b'%' {
			if unsafe { *input.add(at) as u8 } != byte {
				break;
			}
			at += 1;
			index += 1;
			continue;
		}
		index += 1;
		let mut long = false;
		while matches!(unsafe { *format.add(index) as u8 }, b'l' | b'h' | b'z' | b'j') {
			long |= unsafe { *format.add(index) as u8 } == b'l';
			index += 1;
		}
		let conversion = unsafe { *format.add(index) as u8 };
		index += 1;
		while unsafe { *input.add(at) as u8 }.is_ascii_whitespace() {
			at += 1;
		}
		let radix = match conversion {
			b'd' | b'i' | b'u' => 10,
			b'x' => 16,
			_ => break,
		};
		let negative = conversion != b'u' && unsafe { *input.add(at) as u8 } == b'-';
		if negative {
			at += 1;
		}
		let start = at;
		let mut value: u64 = 0;
		while let Some(digit) = (unsafe { *input.add(at) as u8 } as char).to_digit(radix) {
			value = value.saturating_mul(u64::from(radix)).saturating_add(u64::from(digit));
			at += 1;
		}
		if at == start {
			// A MATCHING FAILURE ENDS THE WHOLE CALL, which C says and which matters: continuing
			// would assign to the arguments after it from input that never matched.
			break;
		}
		if results.count == MAX_SCANNED {
			break;
		}
		let signed = match negative {
			true => (value as i64).wrapping_neg(),
			false => value as i64,
		};
		results.values[results.count] = Scanned { value: signed, long };
		results.count += 1;
	}
	results
}

/// What `scan` returns: a bounded run of values, iterable in order.
pub struct ScanResults {
	values: [Scanned; MAX_SCANNED],
	count: usize,
}

impl ScanResults {
	pub fn as_slice(&self) -> &[Scanned] {
		&self.values[..self.count]
	}
}

impl IntoIterator for ScanResults {
	type Item = Scanned;
	type IntoIter = core::iter::Take<core::array::IntoIter<Scanned, MAX_SCANNED>>;

	fn into_iter(self) -> Self::IntoIter {
		self.values.into_iter().take(self.count)
	}
}

// THE VARIADIC ENTRY POINTS, AND THEY ARE ONLY COMPILED FOR THE FREESTANDING TARGET.
//
// `c_variadic` IS UNSTABLE, and the host test build runs on the pinned STABLE toolchain - the same
// one every other host suite in this tree runs on. Requesting the feature unconditionally makes this
// crate unbuildable there, which would trade the fixtures for the four lines below.
//
// `target_os = "none"` RATHER THAN `not(test)`, and the difference was found by running the gate:
// the host suite builds the LIB as well as the test harness, and the lib build has no `test` cfg -
// so gating on it left the unstable feature requested on stable anyway.
//
// What the fixtures exercise instead is `render` and `scan` - where the format parsing, the padding,
// the unrecognised conversion and the matching-failure rule all live, which is where the bugs are.
// Everything below is forwarding.
#[cfg(target_os = "none")]
mod variadic {
	use super::{Arguments, Bounded, render};
	use core::ffi::{VaList, c_char, c_int, c_void};

	/// Four forwarding lines over the real C argument list.
	struct VaArguments<'a, 'f>(&'a mut VaList<'f>);

	impl Arguments for VaArguments<'_, '_> {
		fn next_int(&mut self, long: bool) -> i64 {
			match long {
				true => unsafe { self.0.next_arg::<i64>() },
				false => i64::from(unsafe { self.0.next_arg::<i32>() }),
			}
		}

		fn next_uint(&mut self, long: bool) -> u64 {
			match long {
				true => unsafe { self.0.next_arg::<u64>() },
				false => u64::from(unsafe { self.0.next_arg::<u32>() }),
			}
		}

		fn next_pointer(&mut self) -> *const c_void {
			unsafe { self.0.next_arg() }
		}

		fn next_string(&mut self) -> *const c_char {
			unsafe { self.0.next_arg() }
		}

		fn next_char(&mut self) -> u8 {
			unsafe { self.0.next_arg::<c_int>() as u8 }
		}
	}

	fn finish(out: &mut Bounded) -> c_int {
		out.terminate();
		// THE COUNT IS WHAT WOULD HAVE BEEN WRITTEN, not what was stored. That is `snprintf`'s
		// defining property: a caller sizes a buffer by calling once with a small one, and returning
		// the truncated length makes that idiom allocate too little for ever.
		out.written.min(c_int::MAX as usize) as c_int
	}

	#[unsafe(no_mangle)]
	pub unsafe extern "C" fn vsnprintf(buffer: *mut c_char, size: usize, format: *const c_char, mut args: VaList<'_>) -> c_int {
		let mut out = Bounded { buffer: buffer as *mut u8, capacity: size, written: 0 };
		unsafe { render(&mut out, format, &mut VaArguments(&mut args)) };
		finish(&mut out)
	}

	#[unsafe(no_mangle)]
	pub unsafe extern "C" fn snprintf(buffer: *mut c_char, size: usize, format: *const c_char, mut args: ...) -> c_int {
		let mut out = Bounded { buffer: buffer as *mut u8, capacity: size, written: 0 };
		unsafe { render(&mut out, format, &mut VaArguments(&mut args)) };
		finish(&mut out)
	}

	/// `sscanf`, for the conversions the pinned sources use: whitespace-separated integers.
	///
	/// THE RETURN VALUE IS THE COUNT OF ASSIGNMENTS, which is how a caller tells a partial parse from
	/// a complete one. Returning the count ATTEMPTED would make every failure look like a success.
	#[unsafe(no_mangle)]
	pub unsafe extern "C" fn sscanf(input: *const c_char, format: *const c_char, mut args: ...) -> c_int {
		let mut assigned = 0;
		for value in unsafe { super::scan(input, format) } {
			match value.long {
				true => unsafe { *args.next_arg::<*mut i64>() = value.value },
				false => unsafe { *args.next_arg::<*mut i32>() = value.value as i32 },
			}
			assigned += 1;
		}
		assigned
	}
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fputs(text: *const c_char, stream: *mut c_void) -> c_int {
	if !is_stderr(stream) {
		// REFUSED RATHER THAN DISCARDED. This system has no ambient file namespace; a write that
		// silently went nowhere is a message somebody spends an afternoon looking for.
		return -1;
	}
	crate::sink::report(unsafe { c_string(text) });
	0
}

#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn fputc(character: c_int, stream: *mut c_void) -> c_int {
	if !is_stderr(stream) {
		return -1;
	}
	crate::sink::report(&[character as u8]);
	character
}
