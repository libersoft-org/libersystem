// What a guest's panic handler should DO - not the handler itself.
//
// A guest has no unwinder: a panic aborts the instance. The host surfaces the trap to its caller,
// so the component never spins here in practice.
//
// WITH `dev-diagnostics`, it says what happened first. The default is silence, and for a shipping
// sandbox that is right - a component should not narrate its own failures to a log it does not own.
// During development it means every guest bug looks identical from outside, which is the difference
// between an SDK somebody can use and one they can only guess at. So the diagnostic is a feature,
// off by default, and it goes through the SAME granted log the component already has: no new
// capability, nothing to strip from a release build.
//
// THE `#[panic_handler]` ITSELF BELONGS TO THE PROGRAM, and it used to live here. A panic handler is
// a policy of the final binary, and there may be exactly one per program - so a library that
// declares one takes the choice away from every consumer that depends on it, and a component author
// wanting their own diagnostics, a different trap, or a linked-in handler could not have it while
// depending on this crate. That is not what a crate describing itself as "what somebody else's
// component depends on" may do.
//
// So `liber-sdk` provides the behaviour and `examples/liber_component` declares the policy, in one
// line that a component author copies along with the rest of the example.

// Report a panic through the granted log and trap, for a component whose `#[panic_handler]` calls
// this. Diverging, so a handler can be one expression.
pub fn report_panic(info: &core::panic::PanicInfo) -> ! {
	#[cfg(feature = "dev-diagnostics")]
	report(info);
	#[cfg(not(feature = "dev-diagnostics"))]
	let _ = info;
	core::arch::wasm32::unreachable()
}

// Write "panic at <file>:<line>:<col>: <message>" through the granted log, truncated to what one
// entry can hold. Best-effort by construction: if the log grant is not there the host answers a
// status and this is done either way, because the trap below is the real report.
#[cfg(feature = "dev-diagnostics")]
fn report(info: &core::panic::PanicInfo) {
	use core::fmt::Write;

	let mut out = Buf { bytes: [0u8; 256], len: 0 };
	let _ = out.write_str("panic at ");
	if let Some(at) = info.location() {
		let _ = write!(out, "{}:{}:{}", at.file(), at.line(), at.column());
	} else {
		// The location is optional in the contract; a panic without one still gets a line.
		let _ = out.write_str("an unknown location");
	}
	let _ = write!(out, ": {}", info.message());
	let _ = crate::world::log_message(out.as_str());
}

// A fixed-size sink for `core::fmt`. No allocator here, and a panic handler is the last place to
// want one - the panic may BE the allocator failing.
#[cfg(feature = "dev-diagnostics")]
struct Buf {
	bytes: [u8; 256],
	len: usize,
}

#[cfg(feature = "dev-diagnostics")]
impl Buf {
	fn as_str(&self) -> &str {
		// Every push below cut on a character boundary, so this is text.
		core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("panic (its message was not text)")
	}
}

#[cfg(feature = "dev-diagnostics")]
impl core::fmt::Write for Buf {
	fn write_str(&mut self, s: &str) -> core::fmt::Result {
		// TRUNCATE ON A CHARACTER BOUNDARY, not at the byte the buffer happens to end on: a
		// multi-byte character cut in half would make the whole entry unreadable rather than short,
		// and the entry is the only thing the developer gets.
		for c in s.chars() {
			let width = c.len_utf8();
			if self.len + width > self.bytes.len() {
				return Ok(());
			}
			c.encode_utf8(&mut self.bytes[self.len..]);
			self.len += width;
		}
		Ok(())
	}
}
