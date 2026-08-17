// The world's error model: an `i32` where a NEGATIVE value is a status and a non-negative one is a
// byte count.
//
// It exists because every failure used to be `0`. `read` turned any negative into `0`, so an error
// and a legitimate end of input were the same answer; `write` returned a count where `0` meant
// failed, read-only and a genuine zero-byte write; `log` returned nothing at all. In a capability
// system "I am not allowed to write" is precisely the thing a component most needs to distinguish
// from "the write was empty".
//
// Nothing here calls the host, which is the point: this is the half of the SDK that can be tested
// without one, and the half a wrongly-clamped count or a mis-mapped status would break.

// The wire values. Part of the ABI: a host and a guest built apart must agree on them, and
// `src/user/services/core/src/component_host.rs` carries the same four.
pub const STATUS_DENIED: i32 = -1;
pub const STATUS_FAULT: i32 = -2;
pub const STATUS_IO: i32 = -3;
pub const STATUS_UNSUPPORTED: i32 = -4;

// The most bytes one world call may move, and part of the same ABI.
//
// `read`, `write` and `log` take an `i32` length and answer an `i32` that is a count when positive
// and a status when negative, so a length above this cannot be expressed - `buf.len() as i32` would
// wrap it into a negative number the HOST would then read as nonsense. The wrappers checked nothing
// and cast, which put the one boundary the SDK exists to be the safe side of on the wrong side of
// the cast.
//
// `src/wasm/src/world.rs` carries the same constant and refuses the same length with
// `STATUS_UNSUPPORTED`; the two have to be the same number, which is why both are named rather than
// spelled `i32::MAX as usize` at each use. Nothing can reach it today behind the four-page memory
// cap - a guest cannot hold two gigabytes to point at - and that is a reason to write the limit
// down rather than a reason to leave it implied.
pub const MAX_TRANSFER: usize = i32::MAX as usize;

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
	// The host answered something the world does not permit: a count larger than the buffer it was
	// given, or a nonzero result from a status-only call.
	//
	// SEPARATE FROM `Unknown`, which is a status this build has not heard of and a newer host may
	// legitimately send. This one is not forward compatibility - there is no future version of the
	// contract in which the host moved more bytes than it was offered room for. It is the other
	// side of the boundary being wrong, and a component that retries it will get the same answer.
	HostContract(i32),
}

impl Error {
	pub(crate) fn from_status(status: i32) -> Error {
		match status {
			STATUS_DENIED => Error::Denied,
			STATUS_FAULT => Error::Fault,
			STATUS_IO => Error::Io,
			STATUS_UNSUPPORTED => Error::Unsupported,
			other => Error::Unknown(other),
		}
	}
}

// Turn one host answer into a count, or an error.
//
// A COUNT LARGER THAN WHAT WAS OFFERED IS REFUSED, NOT CLAMPED - and the difference is the whole of
// this function. `write_output` used to return whatever the host said, so a host regression
// answering a million for a hundred-byte write handed a million straight to the application. That
// was replaced by `min(answer, capacity)`, which stopped the absurd number reaching the caller and
// replaced it with a plausible lie: `Ok(100)`.
//
// The host cannot have moved more bytes than it was given room for, so such an answer proves the
// other side of the boundary is broken - and the two ways of reporting a broken host are not equal.
// On a READ, `Ok(capacity)` tells the component that every byte of its buffer is freshly delivered
// when the host may have written none, so it goes on to process whatever was there before. On a
// WRITE, it lets the caller report the whole payload persisted on an answer already known to be
// impossible. Clamping converts a detectable contract violation into undetectable data.
//
// Exactly the capacity is fine, and so is zero. Anything above it is `HostContract`.
pub fn clamp_count(answer: i32, capacity: usize) -> Result<usize, Error> {
	if answer < 0 {
		return Err(Error::from_status(answer));
	}
	if answer as usize > capacity {
		return Err(Error::HostContract(answer));
	}
	Ok(answer as usize)
}
