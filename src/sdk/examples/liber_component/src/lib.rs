// The component SDK's example: a Rust guest built against the `liber:vfs@1` / `liber:log@1` world.
//
// `just sdk` compiles this crate for wasm32-unknown-unknown, and the package builder stages it into
// the system volume where StorageService serves
// vol://system/components/liber_component/app.wasm. The kernel's component_host loads it from
// storage, wires its three imports to typed services with no ambient authority, and invokes its
// exports.
//
// `run` exercises the whole world: it reads its one granted file, transforms the bytes (ASCII
// upper-casing, proving the guest touched them), logs the result, and writes it back - control flow
// plus memory plus all three host calls. `score` exercises the float path on real toolchain output
// (f64 multiply / add / truncate).
//
// Everything reusable lives in `liber-sdk`, which is what makes this an EXAMPLE rather than the SDK
// itself: a component author copies this file and depends on that crate.

// `no_std` only where there is no `std`: on the host this crate is built by `cargo test` for its
// own arithmetic, and a `no_std` crate with no panic handler cannot be a test target. The guest
// build is the one that matters and it is unaffected - `target_arch = "wasm32"` is exactly the
// build the toolchain produces for the component.
#![cfg_attr(target_arch = "wasm32", no_std)]
#![cfg_attr(target_arch = "wasm32", no_main)]

// THE PANIC POLICY, declared by the program rather than by the SDK it depends on.
//
// `liber-sdk` used to carry the `#[panic_handler]`, which meant every consumer inherited it and no
// consumer could declare its own - there is one per program. The behaviour is still the SDK's
// (`report_panic` writes file, line, column and message through the already-granted log under
// `dev-diagnostics`, then traps); the DECLARATION is this line, and a component author copies it
// with the rest of this file.
#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
	liber_sdk::report_panic(info)
}

// Read the granted input, upper-case its ASCII letters in place, log the result,
// write it back, and return the number of bytes processed.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn run() -> i32 {
	use liber_sdk::{STATUS_IO, log_message, read_input, write_output};

	// A LOCAL, not a static. `static mut BUF` plus a hand-made `&mut` through `addr_of_mut!` was
	// sound here - the instance is single-threaded and the host never re-enters `run` - and this is
	// an EXAMPLE, so it is the pattern somebody copies into code where neither of those holds. The
	// guest stack is 64 kB and the buffer is 256 bytes.
	let mut buf: [u8; 256] = [0u8; 256];
	// EVERY WORLD CALL'S ANSWER IS READ. They all used to be thrown away, so a refused read, a
	// refused log and a short write were all invisible - on both sides of a boundary whose whole
	// purpose is to report exactly those. A negative return from `run` is this component saying
	// which call failed, and the host reports it.
	// THREE WORLD CALLS, ONE PATTERN, AND THE PATTERN IS A FUNCTION.
	//
	// `Denied` is the grant answering - a read the component may not do, a log it was not given -
	// and this file used to collapse everything else into `STATUS_IO`. That was already better than
	// the version before it, and it still discarded one the ABI names outright: `Unsupported` is an
	// operation the host does not implement, which is a different thing from a machine that failed,
	// and a component author copying this file would have thrown it away.
	//
	// Written once as `status_of` below rather than three times inline, because three copies of a
	// mapping is how the two halves of this file came to disagree in the first place. Not reachable
	// in this example, whose grants are wired before it starts, and that is beside the point: this
	// file is the thing a component author copies.
	let n: usize = match read_input(&mut buf) {
		Ok(n) => n,
		Err(error) => return status_of(error),
	};
	for byte in &mut buf[..n] {
		if byte.is_ascii_lowercase() {
			*byte -= 32;
		}
	}
	// The log takes TEXT. The transform above only upper-cases ASCII, so anything that was not text
	// on the way in is not text now either - and a component that hands the host bytes and lets it
	// substitute replacement characters has decided something the caller should have.
	if let Ok(text) = core::str::from_utf8(&buf[..n]) {
		match log_message(text) {
			Ok(()) => {}
			Err(error) => return status_of(error),
		}
	}
	// A SHORT WRITE IS A FAILURE, and saying so is the point of the count. `Denied` is what a
	// read-only grant answers, and it is not the same as having written nothing.
	match write_output(&buf[..n]) {
		Ok(written) if written == n => n as i32,
		Ok(_) => STATUS_IO,
		Err(error) => status_of(error),
	}
}

// One error, one status, for every world call - which is what the ABI already distinguishes and
// what a component that collapses them throws away.
//
// `Unknown(x)` passes the host's own number straight back: it is a status this SDK did not know how
// to name, and inventing one here would lose what the host actually said.
fn status_of(error: liber_sdk::Error) -> i32 {
	use liber_sdk::{Error, STATUS_DENIED, STATUS_FAULT, STATUS_IO, STATUS_UNSUPPORTED};
	match error {
		Error::Denied => STATUS_DENIED,
		Error::Fault => STATUS_FAULT,
		Error::Io => STATUS_IO,
		Error::Unsupported => STATUS_UNSUPPORTED,
		Error::Unknown(status) => status,
		// A HOST THAT BROKE THE CONTRACT IS REPORTED AS `IO`, and deliberately not passed through.
		// `Unknown` carries the host's own number back because it may mean something this build has
		// not heard of; `HostContract` carries a number that is not a status at all - it is the
		// impossible count, or the nonzero answer to a status-only call - so returning it would put
		// a positive value where the caller reads a status. The component was reached and could not
		// complete, which is what `IO` says.
		Error::HostContract(_) => STATUS_IO,
	}
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

#[cfg(test)]
mod tests {
	// The example's own arithmetic, on the host. It cost one kernel boot per check to learn
	// anything about `score` before this, and the boundary cases - where truncation and flooring
	// disagree - are exactly the ones the end-to-end test cannot enumerate.
	#[test]
	fn score_truncates_toward_zero() {
		assert_eq!(super::score(10), 17, "the case the kernel test has always asserted");
		assert_eq!(super::score(-3), -2, "-2.5 truncates to -2; flooring would say -3");
		assert_eq!(super::score(0), 2);
		assert_eq!(super::score(-1), 0, "0.5 truncates to 0");
		assert_eq!(super::score(1), 3, "3.5 truncates to 3");
		assert_eq!(super::score(-2), -1, "-1.0 is exact, so both roundings agree");
	}
}

// A guest that PANICS, exported so a test can run the real thing.
//
// The host-side panic test hand-assembles `call log; drop; unreachable` and places a
// "panic at <file>:<line>:<col>: ..." string in memory - "in the shape `dev-diagnostics` produces",
// as its own comment says. Nothing connected that shape to `liber_sdk::report_panic`: change the
// handler's format, or make it call `write` instead of `log`, or drop the trap, and that test still
// passes.
//
// This is the other half. It is built by the toolchain, links the SDK's real handler, and panics for
// real - so what a test observes is what a guest actually does.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn panic_now() -> i32 {
	panic!("the input was not what the component expected");
}
