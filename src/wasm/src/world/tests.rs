// The world's negative cases. Every one of these was unreachable before this module existed: the
// host is a userspace binary, so the only way to exercise it was a kernel boot running the one real
// component, which by construction only takes the happy path.

use super::*;
use alloc::vec;
use alloc::vec::Vec;

fn signature(params: &[ValType], results: &[ValType]) -> FuncType {
	FuncType { params: params.to_vec(), results: results.to_vec() }
}

fn world() -> FuncType {
	signature(&[ValType::I32, ValType::I32], &[ValType::I32])
}

#[test]
fn only_the_world_is_granted() {
	assert_eq!(resolve("liber:vfs@1", "read", Some(&world())), Ok(WorldFn::Read));
	assert_eq!(resolve("liber:vfs@1", "write", Some(&world())), Ok(WorldFn::Write));
	assert_eq!(resolve("liber:log@1", "log", Some(&world())), Ok(WorldFn::Log));

	// The import list IS the manifest of requested authority. A module asking for a camera is
	// refused when it is instantiated, not tolerated because the call site might be unreachable.
	//
	// AND THE REFUSAL SAYS WHICH KIND IT IS. These were all `None`, together with the wrongly-typed
	// imports below, so a caller that had to report which of the two happened - the build-time
	// packaging gate does - could only find out by keeping its own copy of the world.
	assert_eq!(resolve("liber:camera@1", "capture", Some(&world())), Err(ImportError::Unknown), "a capability the world does not offer");
	assert_eq!(resolve("liber:vfs@1", "unlink", Some(&world())), Err(ImportError::Unknown), "an operation the world does not offer");
	assert_eq!(resolve("wasi_snapshot_preview1", "fd_read", Some(&world())), Err(ImportError::Unknown), "somebody else's world");

	// AND THE VERSION IS PART OF THE NAME. The world was three unversioned strings, so an old
	// module and a new one would both import `liber.read` with nothing to tell them apart. An
	// unknown VERSION is an unknown import and not a signature mismatch - `liber:vfs@2 read` may
	// have any shape at all, and this host does not know which.
	assert_eq!(resolve("liber", "read", Some(&world())), Err(ImportError::Unknown), "the unversioned name is not this world");
	assert_eq!(resolve("liber:vfs@2", "read", Some(&world())), Err(ImportError::Unknown), "a version this host does not implement");
}

#[test]
fn an_import_with_the_wrong_signature_is_not_the_import() {
	// This matters more here than it would elsewhere: `Value::as_i32` converts `I64`, `F32` and
	// `F64` as well as `I32`, so a wrongly typed import used to get a silent conversion at call
	// time rather than "incompatible import" - on the boundary whose entire job is to be the place
	// where the guest's word is not taken for anything.
	let wrong = [
		("too few parameters", signature(&[ValType::I32], &[ValType::I32])),
		("too many parameters", signature(&[ValType::I32, ValType::I32, ValType::I32], &[ValType::I32])),
		("a 64-bit pointer", signature(&[ValType::I64, ValType::I32], &[ValType::I32])),
		("a float length", signature(&[ValType::I32, ValType::F64], &[ValType::I32])),
		("no result", signature(&[ValType::I32, ValType::I32], &[])),
		("a 64-bit result", signature(&[ValType::I32, ValType::I32], &[ValType::I64])),
		("two results", signature(&[ValType::I32, ValType::I32], &[ValType::I32, ValType::I32])),
	];
	// The world's own declaration, carried back so a caller can report what was EXPECTED and not
	// only that something was wrong - which is what lets the packaging gate keep both of its
	// messages without keeping a second copy of the world to write them from.
	let declared = ImportError::Signature { params: &[ValType::I32, ValType::I32], results: &[ValType::I32] };
	for (what, bad) in &wrong {
		assert_eq!(resolve("liber:vfs@1", "read", Some(bad)), Err(declared), "{what}");
		assert_eq!(resolve("liber:log@1", "log", Some(bad)), Err(declared), "{what}");
	}

	// A module whose type index points nowhere has no signature to check, which is not a reason to
	// admit it. A SIGNATURE failure rather than an unknown name, because the name was found: the
	// world offers this operation and this module never said what shape of it it wanted.
	assert_eq!(resolve("liber:vfs@1", "read", None), Err(declared), "an import with no type is not resolvable");
}

#[test]
fn a_window_that_does_not_fit_is_refused_rather_than_clamped() {
	const MEM: usize = 64 * 1024;

	// The ordinary cases, including both degenerate ends.
	assert_eq!(window(&[Value::I32(0), Value::I32(256)], MEM), Ok((0, 256)));
	assert_eq!(window(&[Value::I32(0), Value::I32(0)], MEM), Ok((0, 0)), "an empty window is legal");
	assert_eq!(window(&[Value::I32(MEM as i32), Value::I32(0)], MEM), Ok((MEM, MEM)), "an empty window at the very end is still inside");
	assert_eq!(window(&[Value::I32(MEM as i32 - 256), Value::I32(256)], MEM), Ok((MEM - 256, MEM)), "exactly to the end fits");

	// One byte past the end is refused, not shortened. `end` used to be `.min(mem_len)`, so a
	// component asking for three hundred bytes near the top of its memory silently got fewer - and
	// a count it could not distinguish from a short file.
	assert_eq!(window(&[Value::I32(MEM as i32 - 256), Value::I32(257)], MEM), Err(WindowError::OutOfBounds), "one byte past the end");
	assert_eq!(window(&[Value::I32(MEM as i32), Value::I32(1)], MEM), Err(WindowError::OutOfBounds), "starting at the end");

	// A POINTER WITH THE HIGH BIT SET. A wasm32 address is a 32-bit pattern; read as a signed
	// integer it sign-extends into an enormous `usize`. Either reading refuses this window - the
	// point is that it is refused as OUT OF RANGE and not accepted as a small negative offset.
	assert_eq!(window(&[Value::I32(-1), Value::I32(1)], MEM), Err(WindowError::OutOfBounds), "0xFFFF_FFFF is not inside a 64 KiB memory");
	assert_eq!(window(&[Value::I32(i32::MIN), Value::I32(0)], MEM), Err(WindowError::OutOfBounds), "0x8000_0000 is not inside a 64 KiB memory");

	// And the arithmetic does not wrap. Two 32-bit values cannot overflow a 64-bit `usize`, so on
	// this host the sum is simply large - and a memory that large would legitimately contain it,
	// which is what this asserts rather than pretending otherwise. The `checked_add` is what keeps
	// the same code right where `usize` is 32 bits, and what keeps a future 64-bit `ptr` honest.
	assert_eq!(window(&[Value::I32(-1), Value::I32(i32::MAX)], usize::MAX), Ok((0xFFFF_FFFF, 0x1_7FFF_FFFE)), "no wrap: the sum is exact");

	// AND THE ABI'S OWN CEILING. `read` and `write` answer an `i32` that carries a byte count when
	// positive and a status when negative, so a count above `i32::MAX` cannot be told apart from a
	// failure - `n as i32` would wrap it into one. Nothing reaches it today, because a component's
	// memory is capped far below it; the limit is written down and checked here rather than left to
	// depend on that cap, because a limit nobody wrote down is a limit somebody finds.
	assert_eq!(window(&[Value::I32(0), Value::I32(i32::MAX)], usize::MAX), Ok((0, i32::MAX as usize)), "the largest expressible count is allowed");
	assert_eq!(window(&[Value::I32(0), Value::I32(i32::MIN)], usize::MAX), Err(WindowError::TooLarge), "0x8000_0000 bytes is one more than the result can carry - and it is TOO LARGE, not out of bounds: the address is legal and the length is not");
	assert_eq!(window(&[Value::I32(0), Value::I32(-1)], usize::MAX), Err(WindowError::TooLarge), "and so is 0xFFFF_FFFF");

	// Missing arguments are REFUSED rather than defaulted to zero. They used to read as `(0, 0)`,
	// which is an answer to a call that should not have happened - the world's signature is
	// `(i32, i32)` and `resolve` refuses anything else, so the wrong arity here means the
	// interpreter is broken and guessing is the wrong response.
	assert_eq!(window(&[], MEM), Err(WindowError::OutOfBounds), "no arguments is not an empty window");
	assert_eq!(window(&[Value::I32(8)], MEM), Err(WindowError::OutOfBounds), "one argument is not a window either");
	assert_eq!(window(&[Value::I32(0), Value::I32(0), Value::I32(0)], MEM), Err(WindowError::OutOfBounds), "nor is three");

	// AND THE TYPES ARE MATCHED, not converted. `Value::as_i32` accepts `I64`, `F32` and `F64`, and
	// there is no type validator below this yet - so a module can declare `(param i32 i32)` and push
	// two floats in a body nothing type-checked. `resolve` constrains what a module DECLARES; this
	// constrains what arrives.
	assert_eq!(window(&[Value::F64(0.0), Value::I32(4)], MEM), Err(WindowError::OutOfBounds), "a float where the pointer belongs");
	assert_eq!(window(&[Value::I32(0), Value::I64(4)], MEM), Err(WindowError::OutOfBounds), "an i64 where the length belongs");

	// AND THE TWO REFUSALS ARE DIFFERENT REFUSALS, which is the whole point of the enum. This
	// returned `Option`, and the dispatch turned every `None` into `STATUS_FAULT` - documented by
	// the SDK as "(ptr, len) is not in guest memory". An in-bounds length past `i32::MAX` is not
	// that; it is `Unsupported`, "the argument is outside what the host accepts". A guest told
	// `Fault` for an address it can legally read goes looking for a pointer bug it does not have.
	assert_eq!(window(&[Value::I32(0), Value::I32(i32::MIN)], usize::MAX).unwrap_err(), WindowError::TooLarge, "a legal address with an over-long length is not a fault");
	assert_eq!(window(&[Value::I32(MEM as i32), Value::I32(1)], MEM).unwrap_err(), WindowError::OutOfBounds, "and an address outside the memory is");
}

#[test]
fn the_hosts_status_codes_and_the_sdks_are_the_same_four() {
	// They exist TWICE - here and in `src/sdk/src/status.rs` - and the test below asserted one copy
	// against the literals `-1..-4`, which says nothing about whether the two copies agree. They are
	// ABI for a world that is explicitly versioned, so a guest built apart from this host depends on
	// them matching.
	//
	// One definition would be better and is not available: the SDK is a `wasm32` guest crate pinned
	// to its own toolchain and deliberately depends on nothing, so it cannot import the host's. What
	// it can do is be compared against them, and the comparison has to be mechanical or it is the
	// same promise the two copies already broke once.
	const HOST: &str = include_str!("../world.rs");
	const SDK: &str = include_str!("../../../sdk/src/status.rs");
	let constants = |source: &str| -> Vec<(alloc::string::String, i32)> {
		let mut out: Vec<(alloc::string::String, i32)> = Vec::new();
		for line in source.lines() {
			let Some(rest) = line.trim().strip_prefix("pub const STATUS_") else { continue };
			let Some((name, value)) = rest.split_once(": i32 = ") else { continue };
			let Some(number) = value.trim().strip_suffix(';').and_then(|n| n.trim().parse::<i32>().ok()) else { continue };
			out.push((alloc::format!("STATUS_{name}"), number));
		}
		out.sort();
		out
	};
	let host = constants(HOST);
	let sdk = constants(SDK);
	assert_eq!(host.len(), 4, "the host declares the four the world defines: {host:?}");
	assert_eq!(host, sdk, "the host's status codes and the SDK's have drifted apart");

	// AND THE TRANSFER LIMIT, which this comparison used to leave out.
	//
	// It is the same kind of value as the four above - a number both sides declare separately and
	// must agree on - and it was the one the guard did not cover. Nothing catches a divergence in
	// it: the guest would refuse a length the host accepts, or hand over one the host refuses, and
	// the symptom is an `Unsupported` from whichever side happens to be stricter. That reads as a
	// host that does not implement the call rather than as two builds disagreeing about a bound.
	let transfer = |source: &str| -> Option<alloc::string::String> { source.lines().find_map(|line| line.trim().strip_prefix("pub const MAX_TRANSFER: usize = ")?.trim().strip_suffix(';').map(alloc::string::String::from)) };
	let host_transfer = transfer(HOST).expect("the host declares MAX_TRANSFER");
	let sdk_transfer = transfer(SDK).expect("the SDK declares MAX_TRANSFER");
	assert_eq!(host_transfer, sdk_transfer, "the host's transfer limit and the SDK's have drifted apart");
	assert_eq!(host_transfer, "i32::MAX as usize", "and it is still the bound the i32 count/status split implies");
}

#[test]
fn the_status_codes_are_the_ones_the_guest_was_built_against() {
	// They are ABI. The SDK carries the same four in `src/sdk/src/status.rs`, and a guest built
	// apart from this host agrees with it only because both sides pin the numbers.
	assert_eq!([STATUS_DENIED, STATUS_FAULT, STATUS_IO, STATUS_UNSUPPORTED], [-1, -2, -3, -4]);
	// Every status is negative and every count is not, which is what makes one `i32` able to carry
	// both without a second return value the interpreter would have to support.
	let statuses: Vec<i32> = vec![STATUS_DENIED, STATUS_FAULT, STATUS_IO, STATUS_UNSUPPORTED];
	assert!(statuses.iter().all(|status| *status < 0), "a status must never be mistakable for a count");
}

// ---------------------------------------------------------------------------------------------
// The dispatch, against a stub that answers whatever the case is about.
//
// These four outcomes - a write refused, a write that fails, a service that never answers, and a
// log grant that does not respond - were listed as needing "a mocked service somebody has to build
// first" for as long as the dispatch lived inside `component_host`, a `*-unknown-none` binary. The
// only way to reach any of it was a kernel boot running the one real component, and a boot only
// ever takes the happy path. `WorldServices` is the seam; this is the stub.

use crate::interp::{Instance, Trap};
use crate::tests::{I32, Spec, build};
use alloc::string::{String, ToString};

// What each of the three operations should answer. Scripted rather than simulated: the point is to
// pin what the guest is TOLD, and every one of these is a real service outcome.
struct Stub {
	read: ReadOutcome,
	read_bytes: &'static [u8],
	write: WriteOutcome,
	log: LogOutcome,
	// what the stub was actually asked to do, so a test can tell "refused" from "never called".
	wrote: Vec<u8>,
	logged: Vec<String>,
	// A DELIBERATELY LYING ADAPTER: report this count instead of the honest one.
	//
	// `window` bounds the length the GUEST asked for and says nothing about what the adapter
	// answers, and the host used to cast that answer straight into an `i32`. So an adapter
	// returning more than the buffer - or more than `i32::MAX` - produced a number the guest reads
	// as a status. Every adapter in the tree is honest, which is what makes this a boundary rather
	// than a reachable bug, and a boundary nothing exercises is a boundary that decays.
	lie: Option<usize>,
}

impl Stub {
	fn working() -> Stub {
		Stub { read: ReadOutcome::Read(0), read_bytes: b"input", write: WriteOutcome::Wrote(0), log: LogOutcome::Logged, wrote: Vec::new(), logged: Vec::new(), lie: None }
	}
}

impl WorldServices for Stub {
	fn read(&mut self, dst: &mut [u8]) -> ReadOutcome {
		if let Some(count) = self.lie {
			return ReadOutcome::Read(count);
		}
		match self.read {
			// the count a real service reports is what it copied, which is what fits.
			ReadOutcome::Read(_) => {
				let n = self.read_bytes.len().min(dst.len());
				dst[..n].copy_from_slice(&self.read_bytes[..n]);
				ReadOutcome::Read(n)
			}
			other => other,
		}
	}

	fn write(&mut self, bytes: &[u8]) -> WriteOutcome {
		self.wrote = bytes.to_vec();
		if let Some(count) = self.lie {
			return WriteOutcome::Wrote(count);
		}
		match self.write {
			// the count a real service reports is what it accepted, which is the whole request.
			WriteOutcome::Wrote(_) => WriteOutcome::Wrote(bytes.len()),
			other => other,
		}
	}

	fn log(&mut self, text: &str) -> LogOutcome {
		self.logged.push(text.to_string());
		self.log
	}
}

// A module that calls one import with a (ptr, len) the test chooses and returns its status, plus a
// 64 KiB memory holding `data` at offset 0. `which` picks the import: 0 read, 1 write, 2 log.
//
// The three imports are all declared, so the resolved table has the same shape the real component
// gives the host and an index is not accidentally the only one there is.
fn caller(which: u32, ptr: i32, len: i32, data: &[u8]) -> (Vec<u8>, Vec<WorldFn>) {
	let mut body: Vec<u8> = Vec::new();
	body.push(0x41); // i32.const ptr
	body.extend_from_slice(&crate::tests::sleb(ptr as i64));
	body.push(0x41); // i32.const len
	body.extend_from_slice(&crate::tests::sleb(len as i64));
	body.push(0x10); // call
	body.extend_from_slice(&crate::tests::leb(which));
	body.push(0x0b); // end
	let spec = Spec { types: &[(&[I32, I32], &[I32]), (&[], &[I32])], imports: &[("liber:vfs@1", "read", 0), ("liber:vfs@1", "write", 0), ("liber:log@1", "log", 0)], funcs: &[1], mem_pages: 1, globals: &[], data: &[(0, data)], exports: &[("run", 0x00, 3)], codes: &[(&[], &body)] };
	(build(&spec), alloc::vec![WorldFn::Read, WorldFn::Write, WorldFn::Log])
}

// Run `run` on a module against `stub`, answering (status, the host).
fn run(module: Vec<u8>, imports: Vec<WorldFn>, stub: Stub) -> (Result<Vec<Value>, Trap>, WorldHost<Stub>) {
	let parsed = crate::parse(&module).expect("the test module parses");
	let validated = crate::validate(parsed).expect("the test module validates");
	let mut instance = Instance::new(&validated).expect("the test module instantiates");
	let mut host = WorldHost::new(stub, imports);
	let out = instance.invoke("run", &[], &mut host);
	(out, host)
}

#[test]
fn a_refused_write_and_a_failed_one_are_not_the_same_answer() {
	// THE DISTINCTION THE ERROR MODEL EXISTS FOR. Every write failure used to be `STATUS_DENIED`:
	// a buffer the host could not create, a service that never answered, and a volume that said no
	// were one answer, on the boundary where a capability system most needs them apart. A guest
	// told `Denied` stops asking; a guest told `Io` may retry or report upwards.
	let (module, imports) = caller(1, 0, 5, b"hello");

	let (ok, host) = run(module.clone(), imports.clone(), Stub::working());
	assert_eq!(ok.expect("a write is not a trap"), alloc::vec![Value::I32(5)], "an accepted write answers its count");
	assert_eq!(host.output(), b"hello", "the bytes the guest handed to `write` are captured whatever the volume did");

	let refused = Stub { write: WriteOutcome::Refused, ..Stub::working() };
	let (out, _) = run(module.clone(), imports.clone(), refused);
	assert_eq!(out.expect("a refusal is not a trap"), alloc::vec![Value::I32(STATUS_DENIED)], "the volume said no");

	// A service that never answers and a host that could not build the request are both `Failed`,
	// and both are `STATUS_IO`: neither is "you may not", and neither is something the guest can
	// fix by asking differently.
	let failed = Stub { write: WriteOutcome::Failed, ..Stub::working() };
	let (out, host) = run(module, imports, failed);
	assert_eq!(out.expect("a failure is not a trap"), alloc::vec![Value::I32(STATUS_IO)], "the service did not, or could not, answer");
	assert_eq!(host.output(), b"hello", "and the output is still what the guest produced");
}

#[test]
fn a_log_grant_that_does_not_respond_is_reported_to_the_guest() {
	let (module, imports) = caller(2, 0, 5, b"hello");

	let (ok, host) = run(module.clone(), imports.clone(), Stub::working());
	assert_eq!(ok.expect("a log is not a trap"), alloc::vec![Value::I32(0)]);
	assert!(host.logged(), "the grant was reached and the entry accepted");

	// The log used to return NOTHING, so a component could not tell whether its one diagnostic
	// channel had worked - and the host reported the grant as live either way.
	let dead = Stub { log: LogOutcome::Failed, ..Stub::working() };
	let (out, host) = run(module, imports, dead);
	assert_eq!(out.expect("a dead log service is not a trap"), alloc::vec![Value::I32(STATUS_IO)]);
	assert!(!host.logged(), "a grant that did not answer must not be reported as reached");
}

#[test]
fn bytes_that_are_not_text_are_the_guests_mistake_and_not_the_services() {
	// The world's `log` takes TEXT. Reporting a guest's invalid UTF-8 as `STATUS_IO` blames the
	// service for the component's mistake, and `from_utf8_lossy` answers a question nobody asked.
	let (module, imports) = caller(2, 0, 2, &[0xff, 0xfe]);
	let (out, mut host) = run(module, imports, Stub::working());
	assert_eq!(out.expect("not a trap"), alloc::vec![Value::I32(STATUS_UNSUPPORTED)]);
	assert!(!host.logged());
	assert!(host.services_mut().logged.is_empty(), "the service was never asked to emit it");
}

#[test]
fn a_read_the_granted_volume_cannot_serve_is_a_status_not_a_trap() {
	// The component asked a legitimate question and the answer is that the granted file could not
	// be read. Killing the instance tells it nothing and gives it no chance to say so to its caller.
	let (module, imports) = caller(0, 0, 16, b"");
	let (out, _) = run(module.clone(), imports.clone(), Stub::working());
	assert_eq!(out.expect("not a trap"), alloc::vec![Value::I32(5)], "the count the volume served");

	let gone = Stub { read: ReadOutcome::Failed, ..Stub::working() };
	let (out, _) = run(module, imports, gone);
	assert_eq!(out.expect("a dead storage service is not a trap"), alloc::vec![Value::I32(STATUS_IO)]);
}

#[test]
fn a_window_outside_the_guests_memory_is_a_fault_and_the_service_is_never_asked() {
	// The bound is checked before the service is reached, on every one of the three imports - so a
	// guest cannot use a bad pointer to make the host read or write on its behalf.
	for (which, what) in [(0u32, "read"), (1, "write"), (2, "log")] {
		let (module, imports) = caller(which, 65530, 16, b"hello");
		let (out, mut host) = run(module, imports, Stub::working());
		assert_eq!(out.expect("not a trap"), alloc::vec![Value::I32(STATUS_FAULT)], "{what}");
		assert!(host.services_mut().wrote.is_empty(), "{what}: nothing reached the volume");
		assert!(host.services_mut().logged.is_empty(), "{what}: nothing reached the log");
	}
}

#[test]
fn a_guest_that_panics_reaches_the_host_as_a_trap_with_its_diagnostic_already_logged() {
	// END TO END, in the shape `dev-diagnostics` produces: the SDK's panic handler writes
	// "panic at <file>:<line>:<col>: <message>" through the log the component was ALREADY granted,
	// and THEN executes `unreachable`. So the host must see both halves - the diagnostic through
	// the grant, and the trap - and the trap must not lose the line that explains it.
	//
	// This is the feature this milestone added and never watched work under failure. It could not
	// be watched before: the guest half is a `wasm32` crate and the host half was a binary that
	// only runs under a boot.
	const PANIC: &[u8] = b"panic at src/lib.rs:12:5: the input was not what the component expected";
	let mut body: Vec<u8> = Vec::new();
	body.push(0x41);
	body.extend_from_slice(&crate::tests::sleb(0));
	body.push(0x41);
	body.extend_from_slice(&crate::tests::sleb(PANIC.len() as i64));
	body.push(0x10); // call the log import
	body.extend_from_slice(&crate::tests::leb(2));
	body.push(0x1a); // drop its status - the panic handler is best-effort by construction
	body.push(0x00); // unreachable, which is what `core::arch::wasm32::unreachable()` compiles to
	body.push(0x0b);
	let spec = Spec { types: &[(&[I32, I32], &[I32]), (&[], &[])], imports: &[("liber:vfs@1", "read", 0), ("liber:vfs@1", "write", 0), ("liber:log@1", "log", 0)], funcs: &[1], mem_pages: 1, globals: &[], data: &[(0, PANIC)], exports: &[("run", 0x00, 3)], codes: &[(&[], &body)] };
	let imports = alloc::vec![WorldFn::Read, WorldFn::Write, WorldFn::Log];
	let (out, host) = run(build(&spec), imports.clone(), Stub::working());
	assert!(out.is_err(), "a panicking guest must reach the host as a trap, not as a result");
	assert!(host.logged(), "the log grant was reached before the guest died");
	let mut host = host;
	assert_eq!(host.services_mut().logged, alloc::vec![String::from_utf8(PANIC.to_vec()).unwrap()], "the diagnostic arrived through the grant, whole");

	// AND WITH THE GRANT NOT ANSWERING, the trap still happens. The diagnostic is best-effort - the
	// trap is the real report - so a log service that is gone must not turn a guest panic into
	// something else, and must not be reported as reached.
	let dead = Stub { log: LogOutcome::Failed, ..Stub::working() };
	let (out, host) = run(build(&spec), imports, dead);
	assert!(out.is_err(), "the trap does not depend on the log grant");
	assert!(!host.logged());
}

#[test]
fn a_refused_read_and_a_refused_log_are_denied_rather_than_io() {
	// The symmetry `WriteOutcome` had and the other two did not. `read` was `Option<usize>` and
	// `log` was `bool`, so a grant that REFUSED and a service that was gone were one answer -
	// `STATUS_IO` - and the component was told to retry something it may not do. That is the same
	// argument the comment above `WriteOutcome` makes, and it is an argument about the trait.
	let (module, imports) = caller(0, 0, 8, b"");
	let (out, _) = run(module, imports, Stub { read: ReadOutcome::Refused, ..Stub::working() });
	assert_eq!(out.expect("the guest returns a status"), alloc::vec![Value::I32(STATUS_DENIED)], "a refused read is DENIED, not IO");

	let (module, imports) = caller(0, 0, 8, b"");
	let (out, _) = run(module, imports, Stub { read: ReadOutcome::Failed, ..Stub::working() });
	assert_eq!(out.expect("status"), alloc::vec![Value::I32(STATUS_IO)], "and a service that did not answer is IO");

	let (module, imports) = caller(2, 0, 2, b"hi");
	let (out, host) = run(module, imports, Stub { log: LogOutcome::Refused, ..Stub::working() });
	assert_eq!(out.expect("status"), alloc::vec![Value::I32(STATUS_DENIED)], "a refused log is DENIED, not IO");
	assert!(!host.logged(), "and nothing was recorded as logged");

	let (module, imports) = caller(2, 0, 2, b"hi");
	let (out, _) = run(module, imports, Stub { log: LogOutcome::Failed, ..Stub::working() });
	assert_eq!(out.expect("status"), alloc::vec![Value::I32(STATUS_IO)], "and a log grant that did not answer is IO");
}

#[test]
fn both_hosts_answer_a_dead_service_the_same_way() {
	// ONE ABI, ONE ANSWER. `wasi_host` and `component_host` are two hosts of `liber:vfs@1` in one
	// image, and a failed read used to end the instance in one and return `STATUS_IO` in the other -
	// so which semantics a component got depended on which host launched it, which is what a version
	// on an interface exists to make impossible.
	//
	// Both are `WorldHost<S>` now, so the assertion is that the SEAM answers one way: whatever a
	// `WorldServices` reports, the status the guest sees is the world's and not the host's.
	let (module, imports) = caller(0, 0, 8, b"");
	let (out, _) = run(module, imports, Stub { read: ReadOutcome::Failed, ..Stub::working() });
	assert_eq!(out.expect("the guest gets a status, not a trap"), vec![Value::I32(STATUS_IO)], "a service that did not answer is IO, whichever host is behind the seam");

	// And the trap is reserved for what it is for: a guest that broke the contract, not a service
	// that failed. A window outside memory is the guest's mistake and answers `STATUS_FAULT`.
	let (module, imports) = caller(0, 0x7fff_0000u32 as i32, 8, b"");
	let (out, _) = run(module, imports, Stub::working());
	assert_eq!(out.expect("still a status"), vec![Value::I32(STATUS_FAULT)], "an out-of-bounds window is the guest's error and is reported as one");
}

#[test]
fn both_hosts_answer_a_refused_service_the_same_way() {
	// THE SECOND HALF OF THE ONE ABOVE, and the half that was actually broken.
	//
	// A dead service is `Failed` in both adapters, which is why the dead-service test passed while
	// the two disagreed underneath it: `component_host` answered a volume's `Err(_)` with `Refused`
	// and `wasi_host` threw the error away and answered `Failed`, so the same `Error::Denied` on the
	// same versioned import reached the guest as `STATUS_DENIED` under one host and `STATUS_IO`
	// under the other.
	//
	// This asserts the seam's half of the invariant - a `Refused` is `STATUS_DENIED` for all three
	// operations, and nothing about which host produced it. The adapters' half, where the defect
	// lived, is `src/user/services/logic/src/world_errors/tests.rs`: it cannot be asserted here
	// because both hosts are `*-unknown-none` binaries linking `rt`, which is why the classification
	// moved out of them into a crate a host test can reach.
	for which in 0..3u32 {
		let (module, imports) = caller(which, 0, 2, b"hi");
		let stub = Stub { read: ReadOutcome::Refused, write: WriteOutcome::Refused, log: LogOutcome::Refused, ..Stub::working() };
		let (out, host) = run(module, imports, stub);
		assert_eq!(out.expect("the guest gets a status, not a trap"), vec![Value::I32(STATUS_DENIED)], "import {which}: a service that answered and refused is DENIED, whichever host is behind the seam");
		assert!(!host.logged(), "import {which}: a refused log is not a logged one");
	}

	// AND "THIS HOST DOES NOT DO THIS" IS NOT "YOU MAY NOT". `wasi_host` grants only the read half
	// of the world and answered `Refused` for the other two, which reads as a grant it could have
	// been given and was not - there is no write path behind that host for any grant. The world has
	// a status for exactly that, and until these variants existed there was nothing to map to it.
	for which in 0..3u32 {
		let (module, imports) = caller(which, 0, 2, b"hi");
		let stub = Stub { read: ReadOutcome::Unsupported, write: WriteOutcome::Unsupported, log: LogOutcome::Unsupported, ..Stub::working() };
		let (out, _) = run(module, imports, stub);
		assert_eq!(out.expect("still a status"), vec![Value::I32(STATUS_UNSUPPORTED)], "import {which}: an operation the host does not offer is UNSUPPORTED, not DENIED");
	}
}

// IGNORED BY DEFAULT because it needs a wasm32 artifact this crate does not build. The gate runs it
// with `--include-ignored`; a bare `cargo test` reports it as ignored, which is a line in the
// summary rather than a captured message nobody sees.
#[test]
#[ignore = "needs the SDK artifact from `./build.sh --part sdk`; the host-tests gate runs it"]
fn the_sdks_own_panic_handler_reaches_the_host_as_a_trap_with_its_line_logged() {
	// THE OTHER HALF of `a_guest_that_panics_...`, which hand-assembles the shape
	// `dev-diagnostics` is assumed to produce. Nothing tied that shape to `liber_sdk::report_panic`:
	// change the handler's format, make it call `write` instead of `log`, or drop the trap, and that
	// test still passes.
	//
	// This runs the real thing - the toolchain's own output, linking the SDK's handler, panicking
	// for real - so what is observed is what a guest actually does.
	//
	// SKIPPED LOUDLY rather than failing when the artifact is not there: it is built by
	// `./build.sh --part sdk` / the image build, and a unit test that requires a wasm32 toolchain run would make
	// `cargo test` in this crate depend on one. Its absence is said, not passed over in silence.
	// ANCHORED TO THE CRATE, not to whatever directory `cargo test` happened to be run from - which
	// is how this skipped silently the first time it ran from the repository root, and a test that
	// reports a skip nobody reads is the shape this tree has a gate against elsewhere.
	let (trapped, logged, bytes) = panic_the_real_guest("sdk");
	// TWO ASSERTIONS WITH DIFFERENT PRECONDITIONS, said apart rather than skipped together.
	//
	// The TRAP is unconditional: `report_panic` ends in `unreachable()` whether or not
	// `dev-diagnostics` is on, so a toolchain-built guest that panics must reach the host as a trap
	// in every build. The LOG is the feature's, and THIS artifact is the shipping build, which does
	// not enable it - a component should not narrate its own failures to a log it does not own.
	//
	// The first version of this checked the binary for the diagnostic string and skipped when it was
	// absent, which made "built without the feature" indistinguishable from "the handler stopped
	// logging" - it reported SKIPPED for a handler that had been bypassed entirely. A precondition
	// that hides the regression it guards is worse than no test.
	//
	// The branch is KEPT rather than made unconditional, and it is deliberately the negative one
	// that runs: `build.sh` stages this artifact with no `--features`, so "a shipping build says
	// nothing on its way down" is what this file is here to prove. The positive half has its own
	// artifact and its own test below, so neither half depends on how the other happened to be
	// built.
	assert!(trapped, "a panicking guest reaches the host as a trap, not as a result");
	// AND THE SHIPPING ARTIFACT IS ASSERTED TO BE A SHIPPING ARTIFACT, rather than the test working
	// out which kind it was handed and grading itself accordingly.
	//
	// This branched on whether the bytes contained "panic at" and asserted whichever half matched,
	// so a build that accidentally turned the feature on - a changed default, a `--features` added
	// to the shipping step - would have switched the test's expectation and stayed green. A test
	// that adapts to the change it exists to catch is not a test of anything.
	//
	// `build.sh` stages this artifact with no `--features`, and the policy is that a shipping
	// component says nothing on its way down. That is what is asserted; the positive half has its
	// own artifact and its own test below.
	assert!(!bytes.windows(8).any(|w| w == b"panic at"), "a shipping artifact carries no panic diagnostics - if this fails, the shipping build gained the dev-diagnostics feature");
	assert!(logged.is_empty(), "and says nothing through its log on the way down");
}

// IGNORED BY DEFAULT because it needs a wasm32 artifact this crate does not build. The gate runs it
// with `--include-ignored`; a bare `cargo test` reports it as ignored, which is a line in the
// summary rather than a captured message nobody sees.
#[test]
#[ignore = "needs the SDK artifact from `./build.sh --part sdk`; the host-tests gate runs it"]
fn with_dev_diagnostics_the_real_guests_panic_reaches_the_log_it_was_granted() {
	// THE OTHER HALF OF THE FEATURE, against its own artifact, with no condition on the assertion.
	//
	// Everything under `#[cfg(feature = "dev-diagnostics")]` in `src/sdk/src/panic.rs` - the
	// formatting of file, line, column and message, the character-boundary truncation, the call
	// through the granted log - had no automatic coverage against a real guest at all. Nothing in
	// the tree passed `--features`, so the branch above always took the silent path, and the only
	// thing exercising the diagnostic half was a hand-assembled module asserting a shape the real
	// handler is merely ASSUMED to produce. One artifact can only be one of the two builds, so
	// `build.sh` now produces both and each is asserted for what it is.
	let (trapped, logged, _) = panic_the_real_guest("sdk-dev");
	assert!(trapped, "the trap is unconditional: `report_panic` ends in `unreachable()` in both builds");
	assert_eq!(logged.len(), 1, "with dev-diagnostics a real panic goes through the granted log before it traps");
	// AND IT IS THE DIAGNOSTIC, not merely a log entry. `report_panic` promises
	// "panic at <file>:<line>:<col>: <message>", and a test that asserted only that SOMETHING was
	// logged would stay green if that promise were quietly replaced by a bare word.
	// AND THE MESSAGE ITSELF, which this did not check. The three assertions were
	// `starts_with("panic at ")`, `contains(".rs:")` and `contains(": ")` - which is the FORMAT and
	// not the content, so a `report_panic` that emitted a location and an empty message passed all
	// three. The message is the only part of the line a person reads.
	//
	// `panic_now` in `sdk/examples/liber_component` panics with a known string, so there is nothing
	// to guess at: it is asserted verbatim.
	let line = &logged[0];
	assert!(line.starts_with("panic at "), "the entry is the diagnostic the SDK documents: {line}");
	assert!(line.contains(".rs:"), "with the file it happened in: {line}");
	assert!(line.ends_with(": the input was not what the component expected"), "and the guest's own panic message after the location, not merely a colon and a space: {line}");
	// The location is REAL - a file, a line and a column - rather than a fixed prefix followed by
	// the message. `panic at src/lib.rs:149:2: ...` has two colons inside the location.
	let location = line.trim_start_matches("panic at ").split(": ").next().unwrap_or("");
	let parts: Vec<&str> = location.split(':').collect();
	assert_eq!(parts.len(), 3, "the location is <file>:<line>:<col>, and this is {location:?}");
	assert!(parts[1].parse::<u32>().is_ok() && parts[2].parse::<u32>().is_ok(), "with a numeric line and column: {location:?}");
}

// Load a toolchain-built `liber_component.wasm` from one of the two target directories, invoke its
// `panic_now` export, and report whether it trapped and whether it logged - plus the bytes, for the
// caller that inspects them.
//
// A MISSING ARTIFACT IS A FAILURE, AND NOT RUNNING IS SPELLED `#[ignore]`.
//
// This printed "SKIPPED" and returned, and the caller returned too - so the test PASSED. The
// comment said the skip was loud; the Rust harness captures stdout and stderr of passing tests, so
// on the gate it was silent. A component whose panic handler had been removed, or a tree where the
// SDK never built, produced a green suite with nobody told.
//
// The two halves of the problem need different answers. "This machine has no wasm32 toolchain" is a
// reason not to RUN the test, and the harness has a word for that: `#[ignore]` is reported in the
// summary as ignored, which is visible where a captured `eprintln!` is not. "The artifact should be
// there and is not" is a reason to FAIL, because it is the difference between a check that did not
// run and a check that ran against nothing.
//
// ANCHORED TO THE CRATE, not to whatever directory `cargo test` happened to be run from - which is
// how this skipped silently the first time it ran from the repository root.
// Refuse an artifact that predates the sources it claims to have been built from.
//
// Absent sidecar is a FAILURE and not a skip, for the reason the whole of `panic_the_real_guest`
// exists: "the digest could not be checked" and "the digest matched" must not look alike. A tree
// built before this existed has to rebuild, which is one `./build.sh --part sdk`.
fn check_artifact_is_current(artifact: &str, features: &str) {
	let sidecar = alloc::format!("{artifact}.inputs");
	let recorded = std::fs::read_to_string(&sidecar).unwrap_or_else(|error| panic!("{sidecar} is missing ({error}), so nothing says whether {artifact} was built from the sources in this tree - rebuild with `./build.sh --part sdk`"));
	let script = alloc::format!("{}/../tools/sdk-inputs.sh", env!("CARGO_MANIFEST_DIR"));
	let out = std::process::Command::new(&script).arg(features).output().unwrap_or_else(|error| panic!("{script} could not be run ({error}), so the artifact's freshness could not be established either way"));
	assert!(out.status.success(), "{script} failed: {}", String::from_utf8_lossy(&out.stderr));
	let current = String::from_utf8_lossy(&out.stdout).trim().to_string();
	assert_eq!(recorded.trim(), current, "{artifact} was built from different sources than the ones in this tree ({} recorded, {current} now), so this test would be checking an implementation that no longer exists - rebuild with `./build.sh --part sdk`", recorded.trim());
}

fn panic_the_real_guest(target_dir: &str) -> (bool, Vec<String>, Vec<u8>) {
	let built = alloc::format!("{}/../../.build/cargo/{target_dir}/wasm32-unknown-unknown/release/liber_component.wasm", env!("CARGO_MANIFEST_DIR"));
	let bytes = std::fs::read(&built).unwrap_or_else(|error| panic!("{built} is not built ({error}), so the SDK's own panic handler is not exercised - build it with `./build.sh --part sdk` or run this test with the image build"));
	// AND IT WAS BUILT FROM THESE SOURCES, which nothing checked.
	//
	// A missing artifact fails and a STALE one passed: build the SDK, change `report_panic()`, run
	// the host-tests gate on its own, and this test went green against an implementation that no
	// longer exists. It is the same false-green class as the missing artifact, one step further
	// along - and slower to notice, because everything looks like it ran.
	//
	// `build.sh` writes a sidecar digest of what the artifact was built from (SDK sources, the
	// toolchain pin's CONTENT, and the feature set) and this recomputes it with the SAME SCRIPT.
	// Hashing the inputs again in Rust would be two implementations of one answer, which is how a
	// check comes to agree with itself and with nothing else.
	check_artifact_is_current(&built, if target_dir == "sdk-dev" { "dev-diagnostics" } else { "default" });
	let module = crate::parse(&bytes).expect("the toolchain's own component parses");
	let validated = crate::validate(module).expect("and validates");
	let mut instance = Instance::new(&validated).expect("and instantiates");
	// The imports in the order the module declares them, resolved through the same `resolve` the
	// real host uses - so this test cannot pass over a component whose world has drifted.
	let imports: Vec<WorldFn> = validated
		.module()
		.imports
		.iter()
		.map(|import| {
			let signature = validated.module().types.get(import.type_index as usize);
			resolve(&import.module, &import.field, signature).expect("every import resolves in the world the host offers")
		})
		.collect();
	let mut host = WorldHost::new(Stub::working(), imports);
	let out = instance.invoke("panic_now", &[], &mut host);
	// THE LINES, NOT ONLY WHETHER THERE WERE ANY. `host.logged()` answers a bool, and a test built
	// on it passes for a guest that logs anything at all - so a panic handler rewritten to log
	// "oops" and trap would keep it green while the diagnostic it exists to produce disappeared.
	// The stub records the text; this hands it back so the caller can say what it wanted to see.
	let lines = host.services_mut().logged.clone();
	(out.is_err(), lines, bytes)
}

#[test]
fn an_adapter_that_reports_more_than_it_moved_is_refused_rather_than_forwarded() {
	// `ReadOutcome::Read(n) => n as i32` and `WriteOutcome::Wrote(n) => n as i32`, with nothing in
	// between. `window` bounds what the GUEST asked for; the adapter's answer was taken on trust and
	// cast - so a count above the buffer reached the guest as an impossible number, and a count
	// above `i32::MAX` wrapped into a negative one the guest reads as a status code.
	//
	// Every adapter in this tree returns bounded counts, which is why this is a boundary rather than
	// a reachable bug. It is also why it needs a deliberately lying stub: without one, nothing here
	// can ever produce the answer the check exists to refuse.
	let (module, imports) = caller(0, 0, 8, b"");
	let (out, _) = run(module, imports, Stub { lie: Some(9), ..Stub::working() });
	assert_eq!(out.expect("a lying adapter is not a trap"), alloc::vec![Value::I32(STATUS_IO)], "a read that claims nine bytes into an eight-byte window is refused");

	let (module, imports) = caller(1, 0, 8, b"hello123");
	let (out, _) = run(module, imports, Stub { lie: Some(usize::MAX), ..Stub::working() });
	assert_eq!(out.expect("a lying adapter is not a trap"), alloc::vec![Value::I32(STATUS_IO)], "and a write claiming more than the ABI can express is refused rather than wrapped into a status");

	// The honest boundary case still passes: exactly the window is a legal answer.
	let (module, imports) = caller(1, 0, 8, b"hello123");
	let (out, _) = run(module, imports, Stub { lie: Some(8), ..Stub::working() });
	assert_eq!(out.expect("an honest adapter is not a trap"), alloc::vec![Value::I32(8)], "exactly the window is a count, not a fault");
}

#[test]
fn an_over_long_window_is_unsupported_and_an_out_of_bounds_one_is_a_fault() {
	// The two refusals the dispatch used to fold together. `window` answered `Option` and every
	// `None` became `STATUS_FAULT` - "(ptr, len) is not in guest memory" - including for a pair that
	// IS in guest memory and is merely longer than one call may move. The SDK's vocabulary already
	// separates them, and a guest told `Fault` for a legal address goes looking for a pointer bug it
	// does not have.
	//
	// The module's memory is one page, so an out-of-bounds pair is easy; an over-long LENGTH is
	// checked against `MAX_TRANSFER` before the memory bound is consulted at all, so it is reachable
	// here even though a real guest could never hold that much.
	let (module, imports) = caller(0, 0x1_0000, 1, b"");
	let (out, _) = run(module, imports, Stub::working());
	assert_eq!(out.expect("a refused window is not a trap"), alloc::vec![Value::I32(STATUS_FAULT)], "one byte past a one-page memory is a fault");

	let (module, imports) = caller(0, 0, i32::MIN, b"");
	let (out, _) = run(module, imports, Stub::working());
	assert_eq!(out.expect("a refused window is not a trap"), alloc::vec![Value::I32(STATUS_UNSUPPORTED)], "0x8000_0000 bytes is more than one call may move, which is not a fault");
}
