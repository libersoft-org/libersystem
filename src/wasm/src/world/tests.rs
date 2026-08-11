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

	// Missing arguments default to zero rather than panicking: the interpreter should never call an
	// import with the wrong arity, and "should never" is not a bound.
	assert_eq!(window(&[], MEM), Some((0, 0)));
	assert_eq!(window(&[Value::I32(8)], MEM), Some((8, 8)));
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
