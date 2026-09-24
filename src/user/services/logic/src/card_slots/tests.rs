use super::*;

// 3B 88 80 01 "LIBERPIV" TCK: T=0 first, T=1 offered, so a check byte.
fn atr() -> Vec<u8> {
	let mut bytes = alloc::vec![0x3b, 0x88, 0x80, 0x01];
	bytes.extend_from_slice(b"LIBERPIV");
	let tck = bytes[1..].iter().fold(0u8, |sum, byte| sum ^ byte);
	bytes.push(tck);
	bytes
}

const READER: u32 = 7;
const A: u32 = 1;
const B: u32 = 2;

fn pinpad() -> ReaderSpec {
	ReaderSpec { slots: 2, short_apdu: true, pinpad: true, protocols: 0b11 }
}

fn all() -> Operations {
	Operations { read: true, transact: true, authenticate: true }
}

fn call(grant: u32, corr: u32) -> Call {
	Call { grant, corr }
}

fn sends(effects: &[Effect]) -> Vec<(u64, u8, Request)> {
	effects
		.iter()
		.filter_map(|effect| match effect {
			Effect::Send { request, slot, what, .. } => Some((*request, *slot, what.clone())),
			_ => None,
		})
		.collect()
}

fn done(request: u64, slot: u8, generation: u64, response: &[u8]) -> Answer {
	Answer { request, slot, generation, outcome: ProviderOutcome::Done, response: response.to_vec(), atr: Vec::new(), offer: None }
}

// What the service hands over beside the ATR: the shared parser's reading of it.
fn offer(atr: &[u8]) -> Option<Offer> {
	smartcard_model::atr::parse(atr).ok().map(|atr| Offer { protocols: atr.protocols, first: atr.first })
}

// Answer a reset from its first request to idle, checking each step is the one the order requires.
fn complete_reset(cards: &mut Cards, first: &[Effect], slot: u8, generation: u64, now: u64) -> Vec<Effect> {
	let (request, _, what) = sends(first).pop().expect("the reset's first request");
	assert_eq!(what, Request::PowerOff);
	let effects = cards.answered(READER, done(request, slot, generation, &[]), now);
	let (request, _, what) = sends(&effects).pop().expect("power on");
	assert_eq!(what, Request::PowerOn);
	let mut on = done(request, slot, generation, &[]);
	on.atr = atr();
	on.offer = offer(&on.atr);
	let effects = cards.answered(READER, on, now);
	let (request, _, what) = sends(&effects).pop().expect("protocol");
	assert_eq!(what, Request::Protocol(0), "the protocol the card offers first");
	let effects = cards.answered(READER, done(request, slot, generation, &[]), now);
	let (request, _, what) = sends(&effects).pop().expect("select");
	assert_eq!(what, Request::Exchange(piv::SELECT_PIV.to_vec()));
	cards.answered(READER, done(request, slot, generation, &[0x90, 0x00]), now)
}

// A reader with a card in slot 0 at generation 5, reset and idle, and two grants.
fn ready() -> Cards {
	let mut cards = Cards::new();
	let first = cards.add_reader(READER, pinpad(), &[SlotReport { slot: 0, present: true, generation: 5, atr: Vec::new() }, SlotReport { slot: 1, present: false, generation: 0, atr: Vec::new() }], 0).unwrap();
	complete_reset(&mut cards, &first, 0, 5, 0);
	assert!(cards.add_grant(A, READER, all()));
	assert!(cards.add_grant(B, READER, all()));
	cards
}

fn acquired(effects: &[Effect]) -> Acquired {
	match effects {
		[Effect::Acquired { result: Ok(acquired), .. }] => *acquired,
		other => panic!("an acquisition, not {other:?}"),
	}
}

#[test]
fn a_reader_past_the_bounds_is_refused_rather_than_truncated() {
	let mut cards = Cards::new();
	assert_eq!(cards.add_reader(1, ReaderSpec { slots: 5, ..pinpad() }, &[], 0).err(), Some(Refusal::Exhausted), "five slots");
	for key in 0..MAX_READERS as u32 {
		assert!(cards.add_reader(key, pinpad(), &[], 0).is_ok());
	}
	assert_eq!(cards.add_reader(99, pinpad(), &[], 0).err(), Some(Refusal::Exhausted), "a ninth reader");
}

#[test]
fn a_new_session_resets_a_present_card_before_anything_else() {
	let mut cards = Cards::new();
	let first = cards.add_reader(READER, pinpad(), &[SlotReport { slot: 0, present: true, generation: 5, atr: Vec::new() }], 0).unwrap();
	assert!(cards.add_grant(A, READER, all()));
	// Before the reset completes, an acquisition waits.
	assert!(cards.acquire(call(A, 1), 0, 100, 0, 0).is_empty());
	let effects = complete_reset(&mut cards, &first, 0, 5, 1);
	assert_eq!(acquired(&effects).generation, 5, "the waiter is served once the slot is idle");
	assert_eq!(cards.slots(READER)[0].atr, atr(), "and the ATR the card gave is the slot's");
}

#[test]
fn only_the_allowlist_reaches_the_card() {
	let mut cards = ready();
	let transaction = acquired(&cards.acquire(call(A, 1), 0, 0, 0, 0)).transaction;
	// A PIN-bearing VERIFY is refused before any request is made.
	let verify = cards.exchange(call(A, 2), transaction, &[0x00, 0x20, 0x00, 0x80, 0x08, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0xff, 0xff], 0);
	assert_eq!(verify, alloc::vec![Effect::Exchanged { call: call(A, 2), result: Err(Refusal::Denied) }]);
	let other = cards.exchange(call(A, 3), transaction, &[0x00, 0xa4, 0x04, 0x00, 0x05, 0xa0, 0x00, 0x00, 0x00, 0x62, 0x00], 0);
	assert_eq!(other, alloc::vec![Effect::Exchanged { call: call(A, 3), result: Err(Refusal::Unsupported) }]);
	// The canonical GET DATA goes, and a long answer is continued by the service.
	let effects = cards.exchange(call(A, 4), transaction, &piv::GET_AUTH_CERTIFICATE, 0);
	let (request, _, what) = sends(&effects).pop().unwrap();
	assert_eq!(what, Request::Exchange(piv::GET_AUTH_CERTIFICATE.to_vec()));
	let mut first = alloc::vec![0x53; 256];
	first.extend_from_slice(&[0x61, 0x20]);
	let effects = cards.answered(READER, done(request, 0, 5, &first), 1);
	let (request, _, what) = sends(&effects).pop().expect("a GET RESPONSE");
	assert_eq!(what, Request::Exchange(alloc::vec![0x00, 0xc0, 0x00, 0x00, 0x20]));
	let mut last = alloc::vec![0x54; 32];
	last.extend_from_slice(&[0x90, 0x00]);
	match cards.answered(READER, done(request, 0, 5, &last), 2).as_slice() {
		[Effect::Exchanged { result: Ok(exchanged), .. }] => {
			assert_eq!((exchanged.outcome, exchanged.data.len(), exchanged.sw), (Outcome::Done, 288, Some((0x90, 0x00))));
		}
		other => panic!("the continued answer, not {other:?}"),
	}
}

#[test]
fn a_transaction_is_its_owners_and_carries_its_grants_operations() {
	let mut cards = ready();
	assert!(cards.add_grant(3, READER, Operations { read: true, transact: false, authenticate: false }));
	assert_eq!(cards.acquire(call(3, 1), 0, 0, 0, 0), alloc::vec![Effect::Acquired { call: call(3, 1), result: Err(Refusal::Denied) }], "a read-only grant cannot transact");
	let transaction = acquired(&cards.acquire(call(A, 1), 0, 0, 0, 0)).transaction;
	assert_eq!(cards.exchange(call(B, 2), transaction, &piv::SELECT_PIV, 0), alloc::vec![Effect::Exchanged { call: call(B, 2), result: Err(Refusal::Denied) }], "another grant's transaction");
	assert_eq!(cards.exchange(call(A, 2), transaction + 50, &piv::SELECT_PIV, 0), alloc::vec![Effect::Exchanged { call: call(A, 2), result: Err(Refusal::NotFound) }]);
	assert_eq!(cards.acquire(call(A, 3), 5, 0, 0, 0), alloc::vec![Effect::Acquired { call: call(A, 3), result: Err(Refusal::Invalid) }], "a slot the reader does not have");
	assert_eq!(cards.acquire(call(A, 4), 1, 0, 0, 0), alloc::vec![Effect::Acquired { call: call(A, 4), result: Err(Refusal::NotFound) }], "an empty slot");
	// ONE IN FLIGHT.
	assert_eq!(sends(&cards.exchange(call(A, 5), transaction, &piv::SELECT_PIV, 0)).len(), 1);
	assert_eq!(cards.exchange(call(A, 6), transaction, &piv::GET_DISCOVERY, 0), alloc::vec![Effect::Exchanged { call: call(A, 6), result: Err(Refusal::Busy) }]);
}

#[test]
fn a_queued_client_gets_the_slot_only_after_a_full_reset_and_inherits_no_verification() {
	let mut cards = ready();
	let first = acquired(&cards.acquire(call(A, 1), 0, 0, 0, 0)).transaction;
	// A verifies its PIN.
	let (request, _, what) = sends(&cards.verify(call(A, 2), first, 0, 0)).pop().unwrap();
	assert_eq!(what, Request::Verify);
	assert_eq!(cards.answered(READER, done(request, 0, 5, &[0x90, 0x00]), 1), alloc::vec![Effect::Verified { call: call(A, 2), result: Ok(Verified::Verified) }]);
	// B queues.
	assert!(cards.acquire(call(B, 3), 0, 1000, 0, 1).is_empty());
	// A releases: the slot resets before B hears anything.
	let effects = cards.finish(call(A, 4), first, 2).unwrap();
	assert!(!effects.iter().any(|effect| matches!(effect, Effect::Acquired { .. })), "no handoff before the reset");
	let effects = complete_reset(&mut cards, &effects, 0, 5, 3);
	let second = acquired(&effects).transaction;
	// B CANNOT AUTHENTICATE ON A'S VERIFICATION.
	assert_eq!(cards.authenticate(call(B, 5), second, &[0u8; 32], 4), alloc::vec![Effect::Authenticated { call: call(B, 5), result: Err(Refusal::Denied) }]);
	// And A's old transaction names nothing.
	assert_eq!(cards.exchange(call(A, 6), first, &piv::SELECT_PIV, 4), alloc::vec![Effect::Exchanged { call: call(A, 6), result: Err(Refusal::NotFound) }]);
}

#[test]
fn queues_and_the_service_are_bounded_and_a_waiter_that_times_out_never_touches_the_card() {
	let mut cards = ready();
	let _held = acquired(&cards.acquire(call(A, 1), 0, 0, 0, 0));
	assert_eq!(cards.acquire(call(B, 2), 0, 0, 0, 0), alloc::vec![Effect::Acquired { call: call(B, 2), result: Err(Refusal::TimedOut) }], "no wait asked, none given");
	for corr in 0..MAX_WAITERS as u32 {
		assert!(cards.acquire(call(B, 10 + corr), 0, 50, 0, 0).is_empty());
	}
	assert_eq!(cards.acquire(call(B, 99), 0, 50, 0, 0), alloc::vec![Effect::Acquired { call: call(B, 99), result: Err(Refusal::Exhausted) }], "a ninth waiter");
	let effects = cards.tick(50);
	assert_eq!(effects.len(), MAX_WAITERS);
	assert!(effects.iter().all(|effect| matches!(effect, Effect::Acquired { result: Err(Refusal::TimedOut), .. })));
	assert!(sends(&effects).is_empty(), "timing out a waiter sends the card nothing");
}

#[test]
fn a_deadline_wins_once_and_a_late_reply_is_dropped() {
	let mut cards = ready();
	let transaction = acquired(&cards.acquire(call(A, 1), 0, 0, 0, 0)).transaction;
	let (request, _, _) = sends(&cards.exchange(call(A, 2), transaction, &piv::GET_DISCOVERY, 0)).pop().unwrap();
	let effects = cards.tick(APDU_TICKS);
	assert!(effects.contains(&Effect::Exchanged { call: call(A, 2), result: Ok(Exchanged { outcome: Outcome::TimedOut, data: Vec::new(), sw: None }) }));
	let (abort, _, what) = sends(&effects).pop().unwrap();
	assert_eq!(what, Request::Abort);
	// THE LATE REPLY TO THE TIMED-OUT REQUEST changes nothing and answers nobody.
	assert!(cards.answered(READER, done(request, 0, 5, &[0x90, 0x00]), APDU_TICKS + 1).is_empty());
	// The abort's completion is what lets the reset begin.
	let effects = cards.answered(READER, Answer { outcome: ProviderOutcome::Aborted, ..done(abort, 0, 5, &[]) }, APDU_TICKS + 2);
	assert_eq!(sends(&effects).pop().unwrap().2, Request::PowerOff);
}

#[test]
fn an_abort_that_is_never_proven_makes_the_slot_unavailable_until_its_provider_recovers() {
	let mut cards = ready();
	let transaction = acquired(&cards.acquire(call(A, 1), 0, 0, 0, 0)).transaction;
	sends(&cards.exchange(call(A, 2), transaction, &piv::GET_DISCOVERY, 0));
	assert!(cards.acquire(call(B, 3), 0, 5000, 0, 0).is_empty());
	cards.tick(APDU_TICKS);
	let effects = cards.tick(APDU_TICKS + RECOVERY_TICKS);
	assert!(effects.contains(&Effect::Acquired { call: call(B, 3), result: Err(Refusal::Unavailable) }), "its waiters fail");
	assert!(effects.contains(&Effect::Event { reader: READER, kind: EventKind::Unavailable, slot: 0 }));
	assert!(!cards.slots(READER)[0].available);
	assert_eq!(cards.acquire(call(B, 4), 0, 100, 0, APDU_TICKS + RECOVERY_TICKS), alloc::vec![Effect::Acquired { call: call(B, 4), result: Err(Refusal::Unavailable) }]);
	// The provider's own recovery, and then a full reset.
	let effects = cards.quiescent(READER, 0, 2000);
	assert!(effects.contains(&Effect::Event { reader: READER, kind: EventKind::Available, slot: 0 }));
	complete_reset(&mut cards, &effects, 0, 5, 2001);
	assert!(cards.slots(READER)[0].available);
}

#[test]
fn removal_is_terminal_and_a_rapid_reinsertion_waits_for_the_drain() {
	let mut cards = ready();
	let transaction = acquired(&cards.acquire(call(A, 1), 0, 0, 0, 0)).transaction;
	sends(&cards.exchange(call(A, 2), transaction, &piv::GET_DISCOVERY, 0));
	assert!(cards.acquire(call(B, 3), 0, 5000, 0, 0).is_empty());
	// Removed and reinserted before anything else: two events, a distinct terminal result, an abort.
	let effects = cards.presence(READER, SlotReport { slot: 0, present: false, generation: 5, atr: Vec::new() }, 1);
	assert!(effects.contains(&Effect::Exchanged { call: call(A, 2), result: Ok(Exchanged { outcome: Outcome::CardRemoved, data: Vec::new(), sw: None }) }));
	let (abort, _, what) = sends(&effects).pop().unwrap();
	assert_eq!(what, Request::Abort);
	let effects = cards.presence(READER, SlotReport { slot: 0, present: true, generation: 6, atr: Vec::new() }, 2);
	assert_eq!(effects, alloc::vec![Effect::Event { reader: READER, kind: EventKind::Inserted, slot: 0 }], "no reset and no handoff while the old abort drains");
	assert_eq!(cards.exchange(call(A, 4), transaction, &piv::GET_DISCOVERY, 2), alloc::vec![Effect::Exchanged { call: call(A, 4), result: Err(Refusal::NotFound) }], "the removed card's transaction is over");
	let effects = cards.answered(READER, Answer { outcome: ProviderOutcome::Aborted, ..done(abort, 0, 5, &[]) }, 3);
	let effects = complete_reset(&mut cards, &effects, 0, 6, 4);
	assert_eq!(acquired(&effects).generation, 6, "the waiter gets the NEW card, after its reset");
}

#[test]
fn a_pin_is_one_attempt_on_the_pinpad_and_never_anywhere_else() {
	let mut cards = ready();
	let transaction = acquired(&cards.acquire(call(A, 1), 0, 0, 0, 0)).transaction;
	let (request, _, what) = sends(&cards.verify(call(A, 2), transaction, 0, 0)).pop().unwrap();
	assert_eq!(what, Request::Verify, "the request carries no PIN: the template is the provider contract's own");
	let effects = cards.answered(READER, done(request, 0, 5, &[0x63, 0xc2]), 1);
	assert_eq!(effects, alloc::vec![Effect::Verified { call: call(A, 2), result: Ok(Verified::Incorrect(2)) }], "answered once, and nothing sent again");
	// A reader with no usable pinpad says so, and sends nothing.
	let mut plain = Cards::new();
	let first = plain.add_reader(READER, ReaderSpec { pinpad: false, ..pinpad() }, &[SlotReport { slot: 0, present: true, generation: 1, atr: Vec::new() }], 0).unwrap();
	complete_reset(&mut plain, &first, 0, 1, 0);
	plain.add_grant(A, READER, all());
	let transaction = acquired(&plain.acquire(call(A, 1), 0, 0, 0, 0)).transaction;
	assert_eq!(plain.verify(call(A, 2), transaction, 0, 0), alloc::vec![Effect::Verified { call: call(A, 2), result: Ok(Verified::TrustedInputUnavailable) }]);
}

#[test]
fn authentication_follows_a_verification_in_the_same_transaction() {
	let mut cards = ready();
	let transaction = acquired(&cards.acquire(call(A, 1), 0, 0, 0, 0)).transaction;
	assert_eq!(cards.authenticate(call(A, 2), transaction, &[0u8; 32], 0), alloc::vec![Effect::Authenticated { call: call(A, 2), result: Err(Refusal::Denied) }], "not before verification");
	let (request, _, _) = sends(&cards.verify(call(A, 3), transaction, 0, 0)).pop().unwrap();
	cards.answered(READER, done(request, 0, 5, &[0x90, 0x00]), 1);
	assert_eq!(cards.authenticate(call(A, 4), transaction, &[0u8; 31], 1), alloc::vec![Effect::Authenticated { call: call(A, 4), result: Err(Refusal::Invalid) }]);
	let challenge = [0x42u8; 32];
	let (request, _, what) = sends(&cards.authenticate(call(A, 5), transaction, &challenge, 1)).pop().unwrap();
	assert_eq!(what, Request::Exchange(piv::general_authenticate(&challenge).to_vec()));
	// 7C 49 82 47 { DER signature } 90 00.
	let mut der = alloc::vec![0x30, 0x45, 0x02, 0x21, 0x00];
	der.extend_from_slice(&[0x80; 32]);
	der.extend_from_slice(&[0x02, 0x20]);
	der.extend_from_slice(&[0x11; 32]);
	let mut response = alloc::vec![0x7c, 0x49, 0x82, 0x47];
	response.extend_from_slice(&der);
	response.extend_from_slice(&[0x90, 0x00]);
	assert_eq!(cards.answered(READER, done(request, 0, 5, &response), 2), alloc::vec![Effect::Authenticated { call: call(A, 5), result: Ok(Authenticated { outcome: Outcome::Done, signature: Some(der) }) }]);
	// A key the card does not have is unsupported.
	let (request, _, _) = sends(&cards.authenticate(call(A, 6), transaction, &challenge, 3)).pop().unwrap();
	assert_eq!(cards.answered(READER, done(request, 0, 5, &[0x6a, 0x86]), 4), alloc::vec![Effect::Authenticated { call: call(A, 6), result: Err(Refusal::Unsupported) }]);
}

#[test]
fn a_lease_ends_its_transaction_even_mid_operation() {
	let mut cards = ready();
	let transaction = acquired(&cards.acquire(call(A, 1), 0, 0, 300, 0)).transaction;
	sends(&cards.exchange(call(A, 2), transaction, &piv::GET_DISCOVERY, 0));
	let effects = cards.tick(300);
	assert!(effects.contains(&Effect::Exchanged { call: call(A, 2), result: Ok(Exchanged { outcome: Outcome::TimedOut, data: Vec::new(), sw: None }) }), "the operation was capped at the lease");
	assert_eq!(sends(&effects).pop().unwrap().2, Request::Abort);
	assert_eq!(cards.exchange(call(A, 3), transaction, &piv::GET_DISCOVERY, 301), alloc::vec![Effect::Exchanged { call: call(A, 3), result: Err(Refusal::NotFound) }], "never renewed: the lapsed transaction names nothing");
	// A lease asked for past sixty seconds is sixty seconds.
	let mut capped = ready();
	assert_eq!(acquired(&capped.acquire(call(A, 1), 0, 0, 10 * LEASE_TICKS, 0)).lease_ticks, LEASE_TICKS);
}

#[test]
fn a_grant_that_dies_takes_its_transactions_and_its_queue_places_with_it() {
	let mut cards = ready();
	let transaction = acquired(&cards.acquire(call(A, 1), 0, 0, 0, 0)).transaction;
	sends(&cards.exchange(call(A, 2), transaction, &piv::GET_DISCOVERY, 0));
	assert!(cards.acquire(call(A, 3), 0, 1000, 0, 0).is_empty());
	let effects = cards.drop_grant(A, 1);
	assert_eq!(sends(&effects).pop().unwrap().2, Request::Abort, "the operation in flight is aborted");
	assert_eq!(cards.transaction_count(), 0, "its transaction and its queued acquisition are both gone");
	assert_eq!(cards.grant_count(), 1);
}

#[test]
fn a_withdrawn_reader_answers_everything_closed_and_closes_its_grants() {
	let mut cards = ready();
	let transaction = acquired(&cards.acquire(call(A, 1), 0, 0, 0, 0)).transaction;
	sends(&cards.exchange(call(A, 2), transaction, &piv::GET_DISCOVERY, 0));
	assert!(cards.acquire(call(B, 3), 0, 1000, 0, 0).is_empty());
	let effects = cards.remove_reader(READER);
	assert!(effects.contains(&Effect::Exchanged { call: call(A, 2), result: Err(Refusal::Closed) }));
	assert!(effects.contains(&Effect::Acquired { call: call(B, 3), result: Err(Refusal::Closed) }));
	assert!(effects.contains(&Effect::CloseGrant(A)) && effects.contains(&Effect::CloseGrant(B)));
	assert_eq!((cards.grant_count(), cards.transaction_count()), (0, 0));
}

#[test]
fn a_client_that_falls_sixteen_events_behind_is_closed_and_nothing_is_coalesced() {
	let mut queue: EventQueue<(EventKind, u64)> = EventQueue::default();
	for generation in 0..MAX_EVENTS as u64 / 2 {
		assert!(queue.push((EventKind::Removed, generation)));
		assert!(queue.push((EventKind::Inserted, generation + 1)), "a reinsertion is its own event");
	}
	assert_eq!(queue.len(), MAX_EVENTS);
	assert!(!queue.push((EventKind::Removed, 99)), "the seventeenth overflows");
	assert!(queue.overflowed() && queue.is_empty(), "and the stream is to be closed, with nothing left to send");
	assert!(!queue.push((EventKind::Inserted, 100)), "for ever: a new subscription is a new snapshot");
}
