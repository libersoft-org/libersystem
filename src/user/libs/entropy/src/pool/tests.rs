// WHAT A POOL MUST REFUSE, AND WHAT IT MUST NEVER REPEAT.
//
// The first four tests are refusals: an unseeded pool answers nothing, a submission is worth less
// than its length, one submission cannot be worth an unbounded amount, and credit does not grow for
// ever. The last three are the properties a seed source exists for: the output is deterministic
// given a known state (which is what makes a test possible at all), it does not repeat across
// draws, and it does not repeat when a restarted driver hands the same device bytes over again.

use super::*;

#[test]
fn a_pool_that_has_not_been_seeded_answers_nothing_at_all() {
	let mut pool: Pool = Pool::new();
	let mut out: [u8; 32] = [0u8; 32];
	assert!(!pool.seeded());
	assert!(!pool.draw(&mut out), "an unseeded pool must refuse rather than answer weakly");
	assert_eq!(out, [0u8; 32], "a refused draw writes nothing, so a caller that ignored the answer gets zeros rather than plausible bytes");
	// One byte short of the threshold is still short. 127 bytes at a quarter rate is 254 bits.
	assert_eq!(pool.absorb(&[0xA5u8; 127], Source::Paravirtual), 254);
	assert!(!pool.seeded());
	assert!(!pool.draw(&mut out));
	// And the byte that reaches it does.
	assert_eq!(pool.absorb(&[0x5Au8; 1], Source::Paravirtual), 2);
	assert!(pool.seeded());
	assert!(pool.draw(&mut out));
}

#[test]
fn a_submission_is_credited_below_its_own_length_and_by_its_kind() {
	// A quarter for a paravirtual device: everything behind it is the host's choice.
	assert_eq!(credit_for(Source::Paravirtual, 64), 128);
	// A half for the machine's own silicon, which is still not full rate.
	assert_eq!(credit_for(Source::Hardware, 64), 256);
	// Never at or above one bit per bit, which is the claim the whole crate exists to avoid making.
	for len in [1usize, 7, 33, 64, 4096] {
		assert!(credit_for(Source::Paravirtual, len) < (len as u32).saturating_mul(8).max(1));
		assert!(credit_for(Source::Hardware, len) < (len as u32).saturating_mul(8).max(1));
	}
}

#[test]
fn one_submission_cannot_be_worth_more_than_the_bound_however_large_it_is() {
	// A device that answers with a megabyte has proved that it can produce bytes quickly, and
	// nothing else.
	assert_eq!(credit_for(Source::Paravirtual, 1024 * 1024), MAX_SUBMISSION_BITS);
	assert_eq!(credit_for(Source::Hardware, 1024 * 1024), MAX_SUBMISSION_BITS);
	// The bound bites exactly where the arithmetic says it does and not before.
	assert_eq!(credit_for(Source::Paravirtual, 256), MAX_SUBMISSION_BITS);
	assert_eq!(credit_for(Source::Paravirtual, 255), 510);
}

#[test]
fn credit_stops_at_the_ceiling_rather_than_growing_for_ever() {
	let mut pool: Pool = Pool::new();
	for _ in 0..64 {
		pool.absorb(&[0x11u8; 256], Source::Paravirtual);
	}
	let health: Health = pool.health();
	assert_eq!(health.credited_bits, CREDIT_CEILING_BITS, "a device running for a month is not an arbitrarily well seeded machine");
	assert_eq!(health.submissions, 64);
	assert_eq!(health.paravirtual_submissions, 64);
	assert_eq!(health.hardware_submissions, 0);
	// AND IT REPORTS CREDIT AND PROVENANCE, NEVER A VERDICT. `seeded` is the one boolean, and it
	// says the threshold was reached - not that this machine is cryptographically healthy.
	assert!(health.seeded);
}

#[test]
fn a_known_state_produces_a_known_draw() {
	// DETERMINISTIC TEST INJECTION, which is what the plan asks for and what makes every other
	// property here checkable. The expected bytes were computed by an INDEPENDENT SHA-256 - the
	// construction written out in Python against `hashlib` - so this asserts that the pool builds
	// the chain it documents rather than that it agrees with itself.
	let mut pool: Pool = Pool::new();
	pool.absorb(&[0xA5u8; 128], Source::Paravirtual);
	assert!(pool.seeded());
	let mut out: [u8; 32] = [0u8; 32];
	assert!(pool.draw(&mut out));
	assert_eq!(
		out,
		[
			0xf0,
			0x1e,
			0x08,
			0xd5,
			0x3d,
			0xc3,
			0x02,
			0x7d,
			0x8d,
			0x6e,
			0xa1,
			0x97,
			0x7b,
			0x80,
			0x06,
			0xdc,
			0xf6,
			0xa7,
			0xa8,
			0x6c,
			0x61,
			0x40,
			0x12,
			0x8e,
			0x02,
			0x27,
			0x44,
			0x0d,
			0x93,
			0x4e,
			0x76,
			0x19
		]
	);
}

#[test]
fn two_draws_never_answer_the_same_bytes() {
	let mut pool: Pool = Pool::new();
	pool.absorb(&[0xA5u8; 128], Source::Paravirtual);
	let mut first: [u8; 32] = [0u8; 32];
	let mut second: [u8; 32] = [0u8; 32];
	assert!(pool.draw(&mut first));
	assert!(pool.draw(&mut second));
	assert_ne!(first, second);
	// The second draw is the one the rekey stands between, and it is pinned too: a rekey that did
	// nothing would still produce different bytes here (the counter moved), so only the value
	// proves the key changed.
	assert_eq!(&second[..8], &[0xd3, 0x04, 0x01, 0x08, 0x7a, 0x86, 0x24, 0x96]);
}

#[test]
fn a_restarted_driver_handing_over_the_same_bytes_does_not_repeat_the_output() {
	// A device backed by a file, or a guest resumed from a snapshot, hands over exactly what it
	// handed over before. The pool's counter never rewinds, so the same submission twice is two
	// different states - and that is the difference between a seed source being useless and being
	// merely unprovable.
	let mut pool: Pool = Pool::new();
	pool.absorb(&[0xA5u8; 128], Source::Paravirtual);
	let mut first: [u8; 32] = [0u8; 32];
	assert!(pool.draw(&mut first));
	pool.absorb(&[0xA5u8; 128], Source::Paravirtual);
	let mut second: [u8; 32] = [0u8; 32];
	assert!(pool.draw(&mut second));
	assert_ne!(first, second);

	// And a pool that absorbed the same thing from scratch is a different pool only because its
	// history differs, which is the honest statement: two identical histories DO agree, and that is
	// what makes the deterministic test above possible.
	let mut fresh: Pool = Pool::new();
	fresh.absorb(&[0xA5u8; 128], Source::Paravirtual);
	let mut repeat: [u8; 32] = [0u8; 32];
	assert!(fresh.draw(&mut repeat));
	assert_eq!(first, repeat);
}

#[test]
fn an_empty_submission_is_worth_nothing_and_still_moves_the_pool() {
	// A device that answered a request with nothing has said something, and a pool that ignored it
	// entirely would produce the same bytes after a failing device as before one.
	let mut pool: Pool = Pool::new();
	pool.absorb(&[0xA5u8; 128], Source::Paravirtual);
	let before: Health = pool.health();
	assert_eq!(pool.absorb(&[], Source::Paravirtual), 0);
	let after: Health = pool.health();
	assert_eq!(after.credited_bits, before.credited_bits);
	assert_eq!(after.submissions, before.submissions + 1);
	let mut with_empty: [u8; 32] = [0u8; 32];
	assert!(pool.draw(&mut with_empty));

	let mut without: Pool = Pool::new();
	without.absorb(&[0xA5u8; 128], Source::Paravirtual);
	let mut plain: [u8; 32] = [0u8; 32];
	assert!(without.draw(&mut plain));
	assert_ne!(with_empty, plain, "a failed request must leave a mark, or a failing device makes the pool repeatable");
}
