// The host side of the `liber:vfs@1` / `liber:log@1` world: which imports are granted, and how a
// guest's `(ptr, len)` becomes a slice of its own memory.
//
// HERE RATHER THAN IN THE HOST, so it can be tested. `component_host` is a userspace binary built
// for `*-unknown-none`, so nothing inside it can run on the host, and the only coverage the world
// had was one kernel end-to-end path - which needs a boot per assertion and can only exercise the
// happy case. Everything here is a pure decision over bytes and types, which is exactly the part
// where the interesting failures are: an unknown import, a wrongly typed one, a pointer at the top
// of the address space, a length that leaves the memory.
//
// This crate is where it lives because its own description says what it is - a WebAssembly parser
// and interpreter FOR THE LIBERSYSTEM HOST - and because a crate under `src/user` would have to be
// a staged library with a manifest row and a place in the image, which this is not: it is compiled
// into the host binary.
//
// The IPC half stays in `component_host`: reading a granted file, writing one, emitting a log entry.
// That half needs services, and its errors are already carried by the status codes below.

use crate::interp::Value;
use crate::module::{FuncType, ValType};

// The imports the host recognizes, resolved by name AND signature. Anything else is refused at
// instantiation - the component reaches nothing the host did not explicitly wire to a granted
// service.
//
// NAMED AND VERSIONED, because this is a compatibility boundary. The world's whole identity used to
// be the strings `liber.read`, `liber.write` and `liber.log`, so the day a signature changed an old
// module and a new one would both import `liber.read` with nothing to tell them apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WorldFn {
	Read,
	Write,
	Log,
}

// The world's status codes, returned in place of a byte count. They are ABI: a guest built apart
// from this host must agree on them, and `src/sdk/src/status.rs` carries the same four.
//
// This exists because every failure used to be `0`. "The file is empty", "the volume is read-only"
// and "the service did not answer" were one answer, on the boundary where a capability system most
// needs them apart.
pub const STATUS_DENIED: i32 = -1;
pub const STATUS_FAULT: i32 = -2;
pub const STATUS_IO: i32 = -3;
pub const STATUS_UNSUPPORTED: i32 = -4;

// Resolve one import to its world operation, BY NAME AND BY SIGNATURE. This is the whole authority
// surface: only these three names are wired, and only with the types the world declares.
//
// The signature was never consulted, and it matters more here than it would elsewhere because
// `Value::as_i32` converts `I64`, `F32` and `F64` as well as `I32` - so a module declaring
// `liber.read` with the wrong type did not get "incompatible import", it got a silent conversion at
// call time, on the boundary whose entire job is to be the place where the guest's word is not
// taken for anything.
//
// AN UNKNOWN IMPORT IS REFUSED AT INSTANTIATION, not tolerated until it is called. For a capability
// system the import list IS the manifest of requested authority: a module asking for `liber.camera`
// is asking for a camera, and "the call site might be unreachable" is not an answer to that.
pub fn resolve(module: &str, field: &str, signature: Option<&FuncType>) -> Option<WorldFn> {
	let (op, params, results): (WorldFn, &[ValType], &[ValType]) = match (module, field) {
		// read(ptr: u32, max: u32) -> i32 (count, or a negative status)
		("liber:vfs@1", "read") => (WorldFn::Read, &[ValType::I32, ValType::I32], &[ValType::I32]),
		// write(ptr: u32, len: u32) -> i32 (count, or a negative status)
		("liber:vfs@1", "write") => (WorldFn::Write, &[ValType::I32, ValType::I32], &[ValType::I32]),
		// log(ptr: u32, len: u32) -> i32 (0, or a negative status)
		("liber:log@1", "log") => (WorldFn::Log, &[ValType::I32, ValType::I32], &[ValType::I32]),
		_ => return None,
	};
	let signature = signature?;
	if signature.params != params || signature.results != results {
		return None;
	}
	Some(op)
}

// Resolve a (ptr, len) argument pair into a bounds-checked [ptr, end) memory window.
//
// A wasm32 address is a 32-BIT PATTERN, not a signed integer. Reading it as `i32 as usize`
// sign-extends anything at or above 0x8000_0000 into an enormous `usize`, which the bound below
// then refuses - so the ABI was silently capped at the low 2 GiB. Today's module has a small memory
// and cannot reach it; the conversion is still the wrong one.
//
// REFUSED RATHER THAN CLAMPED. `end` used to be `.min(mem_len)`, so a component asking to write
// three hundred bytes from an address near the top of its memory silently wrote fewer - the caller
// was told a count it could not distinguish from a short file. A window that does not fit is
// `Fault`, and the guest can say so.
pub fn window(args: &[Value], mem_len: usize) -> Option<(usize, usize)> {
	let ptr: usize = args.first().map(|v: &Value| v.as_i32() as u32 as usize).unwrap_or(0);
	let len: usize = args.get(1).map(|v: &Value| v.as_i32() as u32 as usize).unwrap_or(0);
	let end: usize = ptr.checked_add(len)?;
	if end > mem_len {
		return None;
	}
	Some((ptr, end))
}

#[cfg(test)]
mod tests;
