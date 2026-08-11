// The `liber` world bindings - the reusable part of the component SDK.
//
// A LiberSystem component is a WebAssembly module that imports this small,
// capability-oriented world and exports an entry point. The host
// (src/user/services/core/src/component_host.rs) resolves each import by name AND signature and
// wires it to a typed system service - never to ambient authority:
//
//   liber:vfs@1.read(ptr: u32, max: u32) -> i32     a granted file, read through StorageService
//   liber:vfs@1.write(ptr: u32, len: u32) -> i32    a granted file, written through StorageService
//   liber:log@1.log(ptr: u32, len: u32) -> i32      one structured entry, emitted to LogService
//
// The component never names a path, a channel, or a service: it only sees these
// three functions, and reaches exactly the capabilities the host was granted.
//
// NAMED AND VERSIONED, because this is a compatibility boundary. The world's whole identity used to
// be the strings `liber.read`, `liber.write` and `liber.log` - so the day a signature changed, an
// old module and a new one would both import `liber.read` and nothing could tell them apart. An
// interface gets its identity from its first external user, not from the first release.
//
// The status codes below are the other half of that: an `i32` where a NEGATIVE value is a status
// and a non-negative one is a byte count. `read` used to turn every failure into 0, so "the file is
// empty" and "you are not allowed to read it" were the same answer - which in a capability system
// is precisely the distinction a component most needs.

// The raw host imports. The module names are what the host matches on, together with the exact
// signature; the host that instantiates the module supplies the implementations.
#[link(wasm_import_module = "liber:vfs@1")]
unsafe extern "C" {
	safe fn read(ptr: i32, max: i32) -> i32;
	safe fn write(ptr: i32, len: i32) -> i32;
}

#[link(wasm_import_module = "liber:log@1")]
unsafe extern "C" {
	#[link_name = "log"]
	safe fn log_raw(ptr: i32, len: i32) -> i32;
}

// Why a world call did not do what was asked. Negative returns from the host, named.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
	// The grant does not permit this: a read-only output, a capability not held.
	Denied,
	// The (ptr, len) the guest passed is not inside its own memory.
	Fault,
	// The service was reached and did not complete: a failing volume, a closed channel.
	Io,
	// The host does not implement this call, or the argument is outside what it accepts.
	Unsupported,
	// A status this build of the SDK does not know. Forward compatibility: a newer host may define
	// codes an older guest has never heard of, and inventing a meaning for them is worse than
	// saying so.
	Unknown(i32),
}

impl Error {
	fn from_status(status: i32) -> Error {
		match status {
			STATUS_DENIED => Error::Denied,
			STATUS_FAULT => Error::Fault,
			STATUS_IO => Error::Io,
			STATUS_UNSUPPORTED => Error::Unsupported,
			other => Error::Unknown(other),
		}
	}
}

// The wire values. Part of the ABI: a host and a guest built apart must agree on them.
pub const STATUS_DENIED: i32 = -1;
pub const STATUS_FAULT: i32 = -2;
pub const STATUS_IO: i32 = -3;
pub const STATUS_UNSUPPORTED: i32 = -4;

// Read up to `buf.len()` bytes of the granted input into `buf`; `Ok(n)` is how many bytes the host
// delivered, and `Ok(0)` means the input really was empty.
pub fn read_input(buf: &mut [u8]) -> Result<usize, Error> {
	let n: i32 = read(buf.as_mut_ptr() as i32, buf.len() as i32);
	if n < 0 {
		return Err(Error::from_status(n));
	}
	// CLAMPED. The host cannot have delivered more than it was given room for, and this wrapper is
	// the safe side of that boundary.
	Ok((n as usize).min(buf.len()))
}

// Write `buf` to the granted output; `Ok(n)` is how many bytes the host persisted. A read-only
// grant is `Err(Denied)` rather than `Ok(0)`, which is the difference this world exists to carry.
pub fn write_output(buf: &[u8]) -> Result<usize, Error> {
	let n: i32 = write(buf.as_ptr() as i32, buf.len() as i32);
	if n < 0 {
		return Err(Error::from_status(n));
	}
	// CLAMPED, like `read_input` above. This returned whatever the host said, so a host regression
	// answering a million for a hundred-byte write handed a million straight to the application -
	// through the wrapper whose whole job is to be the safe side of that boundary. A count larger
	// than what was offered is not a count; the host cannot have written more than it was given.
	Ok((n as usize).min(buf.len()))
}

// Emit `msg` as one structured log entry through the granted LogService.
//
// TEXT, and the signature says so. This took `&[u8]` while the host did `String::from_utf8_lossy`,
// so bytes that were not text became replacement characters somewhere the caller could not see -
// the wrong answer to either contract. A caller that really has bytes can decide for itself how to
// render them.
pub fn log_message(msg: &str) -> Result<(), Error> {
	let status: i32 = log_raw(msg.as_ptr() as i32, msg.len() as i32);
	if status < 0 { Err(Error::from_status(status)) } else { Ok(()) }
}

// A guest has no unwinder: a panic aborts the instance. The host surfaces the trap
// to its caller, so the component never spins here in practice.
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
	core::arch::wasm32::unreachable()
}
