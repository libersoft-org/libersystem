//! What a service's error means to a guest of the `liber:vfs@1` / `liber:log@1` component world.
//!
//! THE DECISION IS THE ABI, which is why it is here and not in an adapter.
//!
//! `WorldHost` maps `Refused -> STATUS_DENIED` and `Failed -> STATUS_IO` identically for all three
//! operations, and that half is tested. What was never one place is the step BELOW it: turning a
//! `liber:base@1` `Error` into one of those outcomes. `WorldServices` is implemented twice -
//! `component_host` over real StorageService and LogService grants, `wasi_host` over a fixed file or
//! the file picker - and each wrote its own `match`. They disagreed: `component_host` answered
//! `Some(Err(_)) => Refused` and `wasi_host` threw the error away with `_ => return None` and
//! answered `Failed`, so `Error::Denied` from the same service on the same versioned import reached
//! the guest as `STATUS_DENIED` under one host and `STATUS_IO` under the other. That is the exact
//! class of failure a version on an interface exists to make impossible.
//!
//! It lives in THIS crate because this is the crate whose contract is "pure decisions, testable on
//! the host". Both hosts are `*-unknown-none` binaries linking `rt`, so nothing inside either of
//! them can run under `cargo test`; a rule with two copies and no test is a rule that drifts back
//! apart the moment somebody edits one of them.

use base_proto::generated::liber::base::v1::Error;
use wasm::world::{LogOutcome, ReadOutcome, WriteOutcome};

/// Whether an error is the GRANT refusing, or a fault on the way to the service.
///
/// `Denied` is the only one that means "you may not", and it is the only thing `STATUS_DENIED`
/// says. The other four all reach the guest as `STATUS_IO`, and each for its own reason:
///
/// - `Again` is "try later", and a guest told `STATUS_DENIED` will not.
/// - `Closed` is the peer going away: the machine, not the grant.
/// - `Invalid` is the host having asked wrongly - nobody's business but the host's, and certainly
///   not a statement about the guest's authority.
/// - `NotFound` is the interesting one, and the world has no "not there" to give it. The guest never
///   names a path: it asks for THE granted input or THE granted output, and which file that is was
///   decided by the host's manifest before the instance existed. So a granted path that is missing
///   is the HOST being misconfigured, and telling the guest `STATUS_DENIED` would blame it for a
///   wiring mistake it cannot see, let alone fix. The day the world gains a path argument, this
///   decision has to be made again.
///
/// Exhaustive rather than a wildcard, so a sixth variant added to the enum stops the build here -
/// at the one place that decides - instead of quietly joining whichever side the wildcard picked.
pub fn is_refusal(error: Error) -> bool {
	match error {
		Error::Denied => true,
		Error::NotFound | Error::Invalid | Error::Again | Error::Closed => false,
	}
}

/// The outcome for a `read` that the service answered with an error.
pub fn read_failure(error: Error) -> ReadOutcome {
	if is_refusal(error) { ReadOutcome::Refused } else { ReadOutcome::Failed }
}

/// The outcome for a `write` that the service answered with an error.
pub fn write_failure(error: Error) -> WriteOutcome {
	if is_refusal(error) { WriteOutcome::Refused } else { WriteOutcome::Failed }
}

/// The outcome for a `log` that the service answered with an error.
pub fn log_failure(error: Error) -> LogOutcome {
	if is_refusal(error) { LogOutcome::Refused } else { LogOutcome::Failed }
}

#[cfg(test)]
mod tests;
