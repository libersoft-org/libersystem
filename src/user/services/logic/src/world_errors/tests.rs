// The half of the world's error semantics that lived below the seam and had no test.
//
// `src/wasm/src/world/tests.rs` covers everything from `WorldServices` upward: a `Refused` is
// `STATUS_DENIED` and a `Failed` is `STATUS_IO`, for all three operations, whichever host is behind
// it. That is why `both_hosts_answer_a_dead_service_the_same_way` passed while the two adapters
// disagreed underneath it - a dead service is `Failed` on both sides, and a REFUSED one was not.
//
// These assert the step the seam cannot see.

use super::*;

#[test]
fn only_denied_is_the_grant_saying_no() {
	assert!(is_refusal(Error::Denied), "the one error that means 'you may not'");

	// The other four are the machine, and each would be a different lie as `STATUS_DENIED`.
	assert!(!is_refusal(Error::Again), "'try later' - and a guest told DENIED will not");
	assert!(!is_refusal(Error::Closed), "the peer went away: the machine, not the grant");
	assert!(!is_refusal(Error::Invalid), "the host asked wrongly - not a statement about the guest");
	// The decision, not a lookup: the guest never names a path, so a granted file that is not there
	// is the host's manifest being wrong. See the reasoning on `is_refusal`.
	assert!(!is_refusal(Error::NotFound), "a granted path that is missing is the host's misconfiguration");
}

#[test]
fn all_three_operations_answer_one_error_the_same_way() {
	// The invariant the two adapters broke, stated as one assertion: for every error the services
	// can return, `read`, `write` and `log` agree on whether it was the grant or the machine.
	//
	// This is what makes the vocabulary ONE vocabulary. `write_file` already matched `Denied`
	// explicitly and `read_file` and `emit_log` used wildcards that happened to differ from it, so
	// the same volume refusing a read and refusing a write reached the guest as two different
	// statuses from within a single host.
	let all: [Error; 5] = [Error::Denied, Error::NotFound, Error::Invalid, Error::Again, Error::Closed];
	for error in all {
		let refused = is_refusal(error);
		assert_eq!(read_failure(error) == ReadOutcome::Refused, refused, "read disagrees about {error:?}");
		assert_eq!(write_failure(error) == WriteOutcome::Refused, refused, "write disagrees about {error:?}");
		assert_eq!(log_failure(error) == LogOutcome::Refused, refused, "log disagrees about {error:?}");
		// And the non-refusal is `Failed` rather than anything else: `Unsupported` means this host
		// does not offer the operation at all, which is not something a service error can say.
		assert_eq!(read_failure(error) == ReadOutcome::Failed, !refused, "read: a non-refusal is a fault on the way there");
		assert_eq!(write_failure(error) == WriteOutcome::Failed, !refused, "write: a non-refusal is a fault on the way there");
		assert_eq!(log_failure(error) == LogOutcome::Failed, !refused, "log: a non-refusal is a fault on the way there");
	}
}
