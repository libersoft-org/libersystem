// The component SDK example: a Rust guest built against the `liber` world.
//
// `just sdk` compiles this crate for wasm32-unknown-unknown, and the package builder
// stages it into the system volume where StorageService serves vol://system/components/liber_component/app.wasm. The
// kernel's component_host loads it from storage, wires its three imports to typed
// services with no ambient authority, and invokes its exports.
//
// `run` exercises the whole world: it reads its one granted file, transforms the
// bytes (ASCII upper-casing, proving the guest touched them), logs the result, and
// writes it back - control flow plus memory plus all three host calls. `score`
// exercises the float path on real toolchain output (f64 multiply / add / truncate).

#![no_std]
#![no_main]

mod world;

// Read the granted input, upper-case its ASCII letters in place, log the result,
// write it back, and return the number of bytes processed.
#[unsafe(no_mangle)]
pub extern "C" fn run() -> i32 {
	// A LOCAL, not a static. `static mut BUF` plus a hand-made `&mut` through `addr_of_mut!` was
	// sound here - the instance is single-threaded and the host never re-enters `run` - and this is
	// an EXAMPLE, so it is the pattern somebody copies into code where neither of those holds. The
	// guest stack is 64 kB and the buffer is 256 bytes.
	let mut buf: [u8; 256] = [0u8; 256];
	let n: usize = world::read_input(&mut buf);
	for byte in &mut buf[..n] {
		if byte.is_ascii_lowercase() {
			*byte -= 32;
		}
	}
	// The log takes TEXT. The transform above only upper-cases ASCII, so anything that was not text
	// on the way in is not text now either - and a component that hands the host bytes and lets it
	// substitute replacement characters has decided something the caller should have.
	if let Ok(text) = core::str::from_utf8(&buf[..n]) {
		world::log_message(text);
	}
	world::write_output(&buf[..n]);
	n as i32
}

// A pure float computation, exercising f64 arithmetic and the float-to-int conversion in genuine
// toolchain output: score(x) = trunc(x * 1.5 + 2.0), rounding TOWARD ZERO.
//
// The comment said `floor`, and a float-to-int cast truncates - so for `x = -3` the comment said -3
// and the code said -2. The only test was `score(10) == 17`, where the two agree, which is how a
// comment and its code go on disagreeing. Truncation is what the cast does and what the
// interpreter's conversion is being exercised for, so the comment moved.
#[unsafe(no_mangle)]
pub extern "C" fn score(x: i32) -> i32 {
	((x as f64) * 1.5 + 2.0) as i32
}
