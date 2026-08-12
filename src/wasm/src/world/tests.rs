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
	assert_eq!(resolve("liber:vfs@1", "read", Some(&world())), Some(WorldFn::Read));
	assert_eq!(resolve("liber:vfs@1", "write", Some(&world())), Some(WorldFn::Write));
	assert_eq!(resolve("liber:log@1", "log", Some(&world())), Some(WorldFn::Log));

	// The import list IS the manifest of requested authority. A module asking for a camera is
	// refused when it is instantiated, not tolerated because the call site might be unreachable.
	assert_eq!(resolve("liber:camera@1", "capture", Some(&world())), None, "a capability the world does not offer");
	assert_eq!(resolve("liber:vfs@1", "unlink", Some(&world())), None, "an operation the world does not offer");
	assert_eq!(resolve("wasi_snapshot_preview1", "fd_read", Some(&world())), None, "somebody else's world");

	// AND THE VERSION IS PART OF THE NAME. The world was three unversioned strings, so an old
	// module and a new one would both import `liber.read` with nothing to tell them apart.
	assert_eq!(resolve("liber", "read", Some(&world())), None, "the unversioned name is not this world");
	assert_eq!(resolve("liber:vfs@2", "read", Some(&world())), None, "a version this host does not implement");
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
	for (what, bad) in &wrong {
		assert_eq!(resolve("liber:vfs@1", "read", Some(bad)), None, "{what}");
		assert_eq!(resolve("liber:log@1", "log", Some(bad)), None, "{what}");
	}

	// A module whose type index points nowhere has no signature to check, which is not a reason to
	// admit it.
	assert_eq!(resolve("liber:vfs@1", "read", None), None, "an import with no type is not resolvable");
}

#[test]
fn a_window_that_does_not_fit_is_refused_rather_than_clamped() {
	const MEM: usize = 64 * 1024;

	// The ordinary cases, including both degenerate ends.
	assert_eq!(window(&[Value::I32(0), Value::I32(256)], MEM), Some((0, 256)));
	assert_eq!(window(&[Value::I32(0), Value::I32(0)], MEM), Some((0, 0)), "an empty window is legal");
	assert_eq!(window(&[Value::I32(MEM as i32), Value::I32(0)], MEM), Some((MEM, MEM)), "an empty window at the very end is still inside");
	assert_eq!(window(&[Value::I32(MEM as i32 - 256), Value::I32(256)], MEM), Some((MEM - 256, MEM)), "exactly to the end fits");

	// One byte past the end is refused, not shortened. `end` used to be `.min(mem_len)`, so a
	// component asking for three hundred bytes near the top of its memory silently got fewer - and
	// a count it could not distinguish from a short file.
	assert_eq!(window(&[Value::I32(MEM as i32 - 256), Value::I32(257)], MEM), None, "one byte past the end");
	assert_eq!(window(&[Value::I32(MEM as i32), Value::I32(1)], MEM), None, "starting at the end");

	// A POINTER WITH THE HIGH BIT SET. A wasm32 address is a 32-bit pattern; read as a signed
	// integer it sign-extends into an enormous `usize`. Either reading refuses this window - the
	// point is that it is refused as OUT OF RANGE and not accepted as a small negative offset.
	assert_eq!(window(&[Value::I32(-1), Value::I32(1)], MEM), None, "0xFFFF_FFFF is not inside a 64 KiB memory");
	assert_eq!(window(&[Value::I32(i32::MIN), Value::I32(0)], MEM), None, "0x8000_0000 is not inside a 64 KiB memory");

	// And the arithmetic does not wrap. Two 32-bit values cannot overflow a 64-bit `usize`, so on
	// this host the sum is simply large - and a memory that large would legitimately contain it,
	// which is what this asserts rather than pretending otherwise. The `checked_add` is what keeps
	// the same code right where `usize` is 32 bits, and what keeps a future 64-bit `ptr` honest.
	assert_eq!(window(&[Value::I32(-1), Value::I32(-1)], usize::MAX), Some((0xFFFF_FFFF, 0x1_FFFF_FFFE)), "no wrap: the sum is exact");

	// Missing arguments are REFUSED rather than defaulted to zero. They used to read as `(0, 0)`,
	// which is an answer to a call that should not have happened - the world's signature is
	// `(i32, i32)` and `resolve` refuses anything else, so the wrong arity here means the
	// interpreter is broken and guessing is the wrong response.
	assert_eq!(window(&[], MEM), None, "no arguments is not an empty window");
	assert_eq!(window(&[Value::I32(8)], MEM), None, "one argument is not a window either");
	assert_eq!(window(&[Value::I32(0), Value::I32(0), Value::I32(0)], MEM), None, "nor is three");

	// AND THE TYPES ARE MATCHED, not converted. `Value::as_i32` accepts `I64`, `F32` and `F64`, and
	// there is no type validator below this yet - so a module can declare `(param i32 i32)` and push
	// two floats in a body nothing type-checked. `resolve` constrains what a module DECLARES; this
	// constrains what arrives.
	assert_eq!(window(&[Value::F64(0.0), Value::I32(4)], MEM), None, "a float where the pointer belongs");
	assert_eq!(window(&[Value::I32(0), Value::I64(4)], MEM), None, "an i64 where the length belongs");
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
	read: Option<&'static [u8]>,
	write: WriteOutcome,
	log: bool,
	// what the stub was actually asked to do, so a test can tell "refused" from "never called".
	wrote: Vec<u8>,
	logged: Vec<String>,
}

impl Stub {
	fn working() -> Stub {
		Stub { read: Some(b"input"), write: WriteOutcome::Wrote(0), log: true, wrote: Vec::new(), logged: Vec::new() }
	}
}

impl WorldServices for Stub {
	fn read(&mut self, dst: &mut [u8]) -> Option<usize> {
		let src = self.read?;
		let n = src.len().min(dst.len());
		dst[..n].copy_from_slice(&src[..n]);
		Some(n)
	}

	fn write(&mut self, bytes: &[u8]) -> WriteOutcome {
		self.wrote = bytes.to_vec();
		match self.write {
			// the count a real service reports is what it accepted, which is the whole request.
			WriteOutcome::Wrote(_) => WriteOutcome::Wrote(bytes.len()),
			other => other,
		}
	}

	fn log(&mut self, text: &str) -> bool {
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
	let dead = Stub { log: false, ..Stub::working() };
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

	let gone = Stub { read: None, ..Stub::working() };
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
	let dead = Stub { log: false, ..Stub::working() };
	let (out, host) = run(build(&spec), imports, dead);
	assert!(out.is_err(), "the trap does not depend on the log grant");
	assert!(!host.logged());
}
