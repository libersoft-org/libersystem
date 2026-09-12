use super::*;

const DELAY: u64 = 1000;

#[test]
// AT MOST ONE IN FLIGHT: a second request while a scan is running is refused rather than queued, so
// two full directory passes can never overlap.
fn a_second_request_while_one_is_running_is_refused() {
	let mut allowance = Allowance::new(DELAY);
	assert_eq!(allowance.request(0, 1), Ok(()));
	assert!(allowance.in_flight());
	assert_eq!(allowance.request(1, 1), Err(Refusal::InFlight));
	allowance.completed(5, Completion::Unchanged);
	assert!(!allowance.in_flight());
}

#[test]
// THE CASE THE FIRST BOUND MISSED: two recovery scans of an UNCHANGED directory. The second is
// refused with `try-again-at` and the published generation is unchanged by either. A scan that finds
// nothing different is neither a publication nor a failure, and it still costs a full pass.
fn two_scans_of_an_unchanged_directory_are_paced_by_the_delay() {
	let mut allowance = Allowance::new(DELAY);
	assert_eq!(allowance.request(0, 3), Ok(()));
	allowance.completed(10, Completion::Unchanged);
	assert_eq!(allowance.request(11, 3), Err(Refusal::TryAgainAt(1010)), "the refusal says when, so an operator does not poll");
	assert_eq!(allowance.request(1009, 3), Err(Refusal::TryAgainAt(1010)));
	assert_eq!(allowance.request(1010, 3), Ok(()), "at the armed moment, not one tick later");
}

#[test]
// A FAILED SCAN RE-ARMS THE DELAY AND SPENDS NOTHING, so a second scan after an operator corrects
// the files succeeds. The first answer spent the generation's only attempt on the bad directory,
// which locked the operation out of the state it exists to repair.
fn a_failed_scan_does_not_consume_the_publication_allowance() {
	let mut allowance = Allowance::new(DELAY);
	assert_eq!(allowance.request(0, 7), Ok(()));
	allowance.completed(10, Completion::Failed);
	assert_eq!(allowance.request(10, 7), Err(Refusal::TryAgainAt(1010)), "a failure still costs a pass");
	assert_eq!(allowance.request(1010, 7), Ok(()), "and the second scan after a correction is allowed");
	allowance.completed(1020, Completion::Published { replaced: 7 });
	assert_eq!(allowance.next_allowed_at(), 2020);
}

#[test]
// THE CHURN BOUND: one publication per published generation. A publication advances the generation,
// so the ordinary sequence is never refused - and a publication that left the generation where it
// was would be, which is the invariant rather than the pacing.
fn one_publication_per_generation() {
	let mut allowance = Allowance::new(0);
	assert_eq!(allowance.request(0, 4), Ok(()));
	allowance.completed(1, Completion::Published { replaced: 4 });
	assert_eq!(allowance.request(1, 4), Err(Refusal::AlreadyPublished), "generation 4 has already been replaced once");
	assert_eq!(allowance.request(1, 5), Ok(()), "the generation it produced has its own allowance");
}
