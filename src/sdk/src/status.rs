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

// Turn one host answer into a count or an error, CLAMPED to what the caller offered.
//
// The clamp is not defensive decoration. `write_output` returned whatever the host said, so a host
// regression answering a million for a hundred-byte write handed a million straight to the
// application - through the wrapper whose whole job is to be the safe side of that boundary. A
// count larger than what was offered is not a count: the host cannot have moved more bytes than it
// was given room for.
pub fn clamp_count(answer: i32, capacity: usize) -> Result<usize, Error> {
	if answer < 0 {
		return Err(Error::from_status(answer));
	}
	Ok((answer as usize).min(capacity))
}
