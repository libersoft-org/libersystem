use super::*;

#[test]
fn while_locked_only_the_locks_surface_may_be_visible() {
	let mut lock = Lock::new();
	assert!(lock.admits(10) && lock.admits(11), "unlocked: anything");
	lock.take(1, 50, 10).unwrap();
	assert!(lock.admits(50));
	assert!(!lock.admits(10) && !lock.admits(11), "not the prior surface, not a new one");
	assert_eq!(lock.take(2, 60, 50), Err(Refusal::Held), "one session at a time");
}

#[test]
fn release_restores_the_prior_surface_or_falls_back() {
	let mut lock = Lock::new();
	lock.take(1, 50, 10).unwrap();
	assert_eq!(lock.release(2, 3), Err(Refusal::Epoch), "another epoch releases nothing");
	assert_eq!(lock.release(1, 3), Ok(10));
	assert!(lock.admits(10));
	lock.take(2, 51, 10).unwrap();
	lock.forget(10);
	assert_eq!(lock.release(2, 3), Ok(3), "the prior surface went away: the console");
	lock.take(3, 52, 0).unwrap();
	assert_eq!(lock.end(0), 0, "nothing to restore at all");
	assert_eq!(lock.end(3), 0, "and ending twice ends nothing");
}
