use super::*;

const LIFETIME: u64 = 1000;

#[test]
fn a_preparation_is_a_copy_and_a_write_through_the_requesters_alias_changes_nothing() {
	let mut operations = Operations::new(7);
	// THE REQUESTER'S BUFFER, which it can still write after handing it over.
	let mut alias = alloc::vec![0x11u8; 64];
	let prepared = operations.prepare(1, &[1, 2], &alias, 0, LIFETIME).unwrap().clone();
	let confirmed = prepared.digest;
	assert_eq!(confirmed, bootproto::sha256::digest(&[0x11u8; 64]));
	// SUBSTITUTION AFTER PREPARATION: the requester rewrites its buffer.
	alias.iter_mut().for_each(|byte| *byte = 0x22);
	let started = operations.start(prepared.operation, 7, 1, 1).unwrap();
	assert_eq!(started.payload, [0x11u8; 64], "what runs is the copy");
	assert_eq!(started.digest, confirmed, "and it is what was confirmed");
	assert_ne!(bootproto::sha256::digest(&alias), confirmed);
}

#[test]
fn the_start_guard_admits_one_attempt_and_only_on_the_live_target() {
	let mut operations = Operations::new(7);
	let operation = operations.prepare(1, &[], &[1; 16], 0, LIFETIME).unwrap().operation;
	assert_eq!(operations.revalidate(operation, 1, 1), Ok(()));
	// Another executor epoch - a restarted executor - is not this preparation's.
	assert_eq!(operations.start(operation, 8, 1, 1).map(|_| ()), Err(Refusal::Stale));
	// A REPLACED TARGET: the generation moved, and both revalidation and the start refuse.
	assert_eq!(operations.revalidate(operation, 2, 1), Err(Refusal::Stale));
	assert_eq!(operations.start(operation, 7, 2, 1).map(|_| ()), Err(Refusal::Stale));
	// The one attempt.
	assert!(operations.start(operation, 7, 1, 1).is_ok());
	assert_eq!(operations.start(operation, 7, 1, 1).map(|_| ()), Err(Refusal::Started), "never a second");
	assert_eq!(operations.revalidate(operation, 1, 1), Err(Refusal::Started));
	assert_eq!(operations.cancel(operation), Err(Refusal::Started), "and a started one is not recalled");
	// Cancelled, expired and unknown operations start nothing.
	let cancelled = operations.prepare(1, &[], &[1; 16], 0, LIFETIME).unwrap().operation;
	operations.cancel(cancelled).unwrap();
	assert_eq!(operations.start(cancelled, 7, 1, 1).map(|_| ()), Err(Refusal::Cancelled));
	let late = operations.prepare(1, &[], &[1; 16], 0, LIFETIME).unwrap().operation;
	assert_eq!(operations.start(late, 7, 1, LIFETIME).map(|_| ()), Err(Refusal::Expired));
	assert_eq!(operations.start(99, 7, 1, 1).map(|_| ()), Err(Refusal::NotFound));
}

#[test]
fn bounds_and_capacity_refuse_before_anything_is_held() {
	let mut operations = Operations::new(1);
	assert_eq!(operations.prepare(1, &[], &[], 0, LIFETIME).map(|_| ()), Err(Refusal::Bounds), "an empty payload");
	assert_eq!(operations.prepare(1, &[], &[0; MAX_PAYLOAD + 1], 0, LIFETIME).map(|_| ()), Err(Refusal::Bounds));
	assert_eq!(operations.prepare(1, &[0; MAX_PARAMETERS + 1], &[0; 1], 0, LIFETIME).map(|_| ()), Err(Refusal::Bounds));
	assert!(operations.prepare(1, &[], &[0; MAX_PAYLOAD], 0, LIFETIME).is_ok(), "exactly the bound is admitted");
	let mut live = Vec::new();
	while let Ok(prepared) = operations.prepare(1, &[], &[1], 0, LIFETIME) {
		live.push(prepared.operation);
	}
	assert_eq!(live.len() + 1, MAX_OPERATIONS);
	assert_eq!(operations.prepare(1, &[], &[1], 0, LIFETIME).map(|_| ()), Err(Refusal::Busy), "a live preparation is never displaced");
	operations.cancel(live[0]).unwrap();
	assert!(operations.prepare(1, &[], &[1], 0, LIFETIME).is_ok(), "a cancelled one makes room");
	// AND SO DO PREPARATIONS PAST THEIR LIFETIME, which nobody may be left to release: the slots are full of
	// live ones, and once their lifetime has passed the next preparation finds room.
	assert_eq!(operations.prepare(1, &[], &[1], LIFETIME - 1, LIFETIME).map(|_| ()), Err(Refusal::Busy), "still inside their lifetime");
	assert!(operations.prepare(1, &[], &[1], LIFETIME, LIFETIME).is_ok(), "expired ones make room");
}

#[test]
fn what_the_executor_let_go_of_it_no_longer_holds() {
	// AN EXECUTOR'S OWN RECORD OF WHAT IT PREPARED follows this answer, so it must turn false for exactly what the seam
	// dropped to make room - a cancelled, a started and an expired preparation - and stay true for a live one.
	let mut operations = Operations::new(7);
	let cancelled = operations.prepare(1, &[], &[1], 0, LIFETIME).unwrap().operation;
	let started = operations.prepare(1, &[], &[1], 0, LIFETIME).unwrap().operation;
	let live = operations.prepare(1, &[], &[1], 0, LIFETIME).unwrap().operation;
	let expiring = operations.prepare(1, &[], &[1], 0, 10).unwrap().operation;
	operations.cancel(cancelled).unwrap();
	operations.start(started, 7, 1, 1).unwrap();
	assert!([cancelled, started, live, expiring].iter().all(|&operation| operations.holds(operation)), "nothing is let go of before room is made");
	let next = operations.prepare(1, &[], &[1], LIFETIME / 2, LIFETIME).unwrap().operation;
	assert!(!operations.holds(cancelled) && !operations.holds(started) && !operations.holds(expiring), "what made room is gone");
	assert!(operations.holds(live) && operations.holds(next));
	assert!(!operations.holds(99), "and an operation never prepared was never held");
}
