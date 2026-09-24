use super::*;

const SIM: u64 = 7;

fn device() -> Device {
	let mut device = Device::open(3, MAX_PENDING, SIM);
	// Counters read once, as a query would.
	device.indication(1, SIM, (Some(3), Some(10)), false, 0);
	device
}

fn reply_to(command: Command, status: Outcome) -> Reply {
	Reply { connection: command.connection, transaction: command.transaction, kind: command.kind, status, sim_generation: command.sim_generation, context_generation: command.context_generation, counters: None, context_active: None }
}

#[test]
fn a_reply_answers_only_the_command_it_names() {
	let mut device = device();
	let query = device.submit(Kind::Query, 1, SIM, false, 0).unwrap();
	// Another connection, another transaction, another kind: dropped, and the command still pending.
	assert_eq!(device.reply(Reply { connection: 4, ..reply_to(query, Outcome::Done) }, 1), None);
	assert_eq!(device.reply(Reply { transaction: query.transaction + 9, ..reply_to(query, Outcome::Done) }, 1), None);
	assert_eq!(device.reply(Reply { kind: Kind::Identity, ..reply_to(query, Outcome::Done) }, 1), None);
	assert_eq!(device.pending(), 1);
	assert_eq!(device.reply(reply_to(query, Outcome::Done), 1).map(|done| done.caller), Some(1));
	// A DUPLICATE of an answered reply is dropped.
	assert_eq!(device.reply(reply_to(query, Outcome::Done), 2), None);
}

#[test]
fn transaction_numbers_restart_only_with_a_new_connection() {
	let mut device = device();
	let first = device.submit(Kind::Query, 1, SIM, false, 0).unwrap();
	let second = device.submit(Kind::Query, 2, SIM, false, 0).unwrap();
	assert!(second.transaction > first.transaction);
	let reopened = Device::open(4, MAX_PENDING, SIM);
	assert_eq!(reopened.connection(), 4);
	// A reply for the old connection's transaction cannot answer the new one.
	let mut reopened = reopened;
	let fresh = reopened.submit(Kind::Query, 3, SIM, false, 0).unwrap();
	assert_eq!(fresh.transaction, first.transaction, "numbers start again...");
	assert_eq!(reopened.reply(reply_to(first, Outcome::Done), 1), None, "...under a generation the old reply does not name");
}

#[test]
fn eight_pending_and_one_state_change_at_a_time() {
	let mut device = device();
	for caller in 0..MAX_PENDING as u64 {
		device.submit(Kind::Query, caller, SIM, false, 0).unwrap();
	}
	assert_eq!(device.submit(Kind::Query, 99, SIM, false, 0), Err(Refusal::Busy));
	let mut device = super::tests::device();
	device.submit(Kind::Activate, 1, SIM, false, 0).unwrap();
	assert_eq!(device.submit(Kind::EnterPin, 2, SIM, false, 0), Err(Refusal::Busy), "one state change in flight");
	assert!(device.submit(Kind::Query, 3, SIM, false, 0).is_ok(), "queries still go");
	// A negotiated limit below eight is the limit.
	let mut small = Device::open(1, 2, SIM);
	small.submit(Kind::Query, 1, SIM, false, 0).unwrap();
	small.submit(Kind::Query, 2, SIM, false, 0).unwrap();
	assert_eq!(small.submit(Kind::Query, 3, SIM, false, 0), Err(Refusal::Busy));
}

#[test]
fn an_expired_state_change_is_outcome_unknown_never_replayed_and_blocks_until_a_query_reconciles() {
	let mut device = device();
	let pin = device.submit(Kind::EnterPin, 1, SIM, false, 0).unwrap();
	let query = device.submit(Kind::Query, 2, SIM, false, 0).unwrap();
	let expired = device.tick(COMMAND_TICKS);
	assert_eq!(expired.len(), 2);
	assert_eq!(expired.iter().find(|done| done.caller == 1).map(|done| done.result), Some(Outcome::OutcomeUnknown));
	assert_eq!(expired.iter().find(|done| done.caller == 2).map(|done| done.result), Some(Outcome::Failed), "a query that expired has simply failed");
	assert_eq!(device.pending(), 0, "the slots are free");
	// The late reply to the PIN answers nobody.
	assert_eq!(device.reply(reply_to(pin, Outcome::Done), COMMAND_TICKS + 1), None);
	assert_eq!(device.reply(reply_to(query, Outcome::Done), COMMAND_TICKS + 1), None);
	// NOTHING THAT CHANGES STATE until a read-only query has answered.
	assert_eq!(device.submit(Kind::EnterPin, 3, SIM, true, COMMAND_TICKS + 2), Err(Refusal::Reconcile));
	assert_eq!(device.submit(Kind::Activate, 3, SIM, false, COMMAND_TICKS + 2), Err(Refusal::Reconcile));
	let reconcile = device.submit(Kind::Query, 4, SIM, false, COMMAND_TICKS + 2).unwrap();
	device.reply(Reply { counters: Some((Some(2), Some(10))), ..reply_to(reconcile, Outcome::Done) }, COMMAND_TICKS + 3);
	assert!(!device.uncertain());
	assert!(device.submit(Kind::EnterPin, 5, SIM, false, COMMAND_TICKS + 4).is_ok());
}

#[test]
fn an_activation_has_sixty_seconds_and_its_expiry_leaves_the_context_unknown() {
	let mut device = device();
	let activate = device.submit(Kind::Activate, 1, SIM, false, 0).unwrap();
	assert!(device.tick(COMMAND_TICKS).is_empty(), "not after five seconds");
	let expired = device.tick(ACTIVATE_TICKS);
	assert_eq!(expired[0].result, Outcome::OutcomeUnknown);
	assert_eq!(device.context(), (ContextState::Unknown, activate.context_generation), "the context that may exist is the one it named");
}

#[test]
fn a_query_reconciles_an_unknown_context_to_what_the_device_reports() {
	// Reported down: inactive, and nothing to deactivate.
	let mut device = device();
	device.submit(Kind::Activate, 1, SIM, false, 0).unwrap();
	device.tick(ACTIVATE_TICKS);
	let query = device.submit(Kind::Query, 2, SIM, false, ACTIVATE_TICKS + 1).unwrap();
	device.reply(Reply { context_active: Some(false), ..reply_to(query, Outcome::Done) }, ACTIVATE_TICKS + 2);
	assert_eq!(device.context(), (ContextState::Inactive, 0));
	assert!(!device.uncertain());
	// Reported up: active under the generation the activation named, which can now be deactivated.
	let mut device = super::tests::device();
	let activate = device.submit(Kind::Activate, 1, SIM, false, 0).unwrap();
	device.tick(ACTIVATE_TICKS);
	let query = device.submit(Kind::Query, 2, SIM, false, ACTIVATE_TICKS + 1).unwrap();
	device.reply(Reply { context_active: Some(true), ..reply_to(query, Outcome::Done) }, ACTIVATE_TICKS + 2);
	assert_eq!(device.context(), (ContextState::Active, activate.context_generation));
	assert!(device.submit(Kind::Deactivate, 3, SIM, false, ACTIVATE_TICKS + 3).is_ok());
	// A reply with no state leaves it unknown, and still reconciles the modem.
	let mut device = super::tests::device();
	device.submit(Kind::Activate, 1, SIM, false, 0).unwrap();
	device.tick(ACTIVATE_TICKS);
	let query = device.submit(Kind::Query, 2, SIM, false, ACTIVATE_TICKS + 1).unwrap();
	device.reply(reply_to(query, Outcome::Done), ACTIVATE_TICKS + 2);
	assert_eq!(device.context().0, ContextState::Unknown);
	assert!(!device.uncertain());
}

#[test]
fn a_stale_or_late_activation_reply_cannot_bring_a_context_back() {
	let mut device = device();
	let first = device.submit(Kind::Activate, 1, SIM, false, 0).unwrap();
	// The reply names another context generation: stale, and nothing activated.
	let stale = device.reply(Reply { context_generation: first.context_generation + 5, ..reply_to(first, Outcome::Done) }, 1).unwrap();
	assert_eq!(stale.result, Outcome::Stale);
	assert_eq!(device.context(), (ContextState::Inactive, 0));
	// A real activation, then its duplicate: the duplicate is dropped.
	let second = device.submit(Kind::Activate, 2, SIM, false, 2).unwrap();
	assert!(second.context_generation > first.context_generation, "a new activation is a new generation");
	assert_eq!(device.reply(reply_to(second, Outcome::Done), 3).unwrap().result, Outcome::Done);
	assert_eq!(device.context(), (ContextState::Active, second.context_generation));
	assert_eq!(device.reply(reply_to(first, Outcome::Done), 4), None, "the first activation's late reply");
	assert_eq!(device.reply(reply_to(second, Outcome::Done), 4), None, "and the second's duplicate");
}

#[test]
fn a_retry_counter_is_known_or_needs_acknowledgement() {
	let mut device = Device::open(3, MAX_PENDING, SIM);
	assert_eq!(device.counters().0, Count::Unknown);
	assert_eq!(device.submit(Kind::EnterPin, 1, SIM, false, 0), Err(Refusal::UnknownCount));
	assert!(device.submit(Kind::EnterPin, 1, SIM, true, 0).is_ok(), "acknowledged");
	let mut known = super::tests::device();
	assert_eq!(known.counters().0, Count::Known { remaining: 3, sim_generation: SIM, observed: 0 });
	assert!(known.submit(Kind::EnterPin, 1, SIM, false, 0).is_ok());
}

#[test]
fn a_new_sim_ends_everything_bound_to_the_old_one() {
	let mut device = device();
	let activate = device.submit(Kind::Activate, 1, SIM, false, 0).unwrap();
	device.reply(reply_to(activate, Outcome::Done), 1);
	assert_eq!(device.indication(2, SIM + 1, (Some(3), Some(10)), true, 2), Some(SimChange::Replaced));
	assert_eq!(device.context(), (ContextState::Inactive, 0));
	assert_eq!(device.counters(), (Count::Unknown, Count::Unknown), "a new SIM's counters are not the old one's");
	// A grant for the old SIM is stale.
	assert_eq!(device.submit(Kind::Activate, 2, SIM, false, 3), Err(Refusal::Stale));
	assert!(device.submit(Kind::Query, 3, SIM + 1, false, 3).is_ok());
	// An old revision changes nothing.
	assert_eq!(device.indication(1, SIM, (None, None), false, 4), None);
}

#[test]
fn the_network_ending_a_context_is_a_loss_not_a_live_context() {
	let mut device = device();
	let activate = device.submit(Kind::Activate, 1, SIM, false, 0).unwrap();
	device.reply(reply_to(activate, Outcome::Done), 1);
	assert_eq!(device.indication(2, SIM, (Some(3), Some(10)), true, 2), Some(SimChange::Updated));
	assert_eq!(device.indication(3, SIM, (Some(3), Some(10)), false, 3), Some(SimChange::ContextLost));
	assert_eq!(device.context(), (ContextState::Inactive, 0));
	assert_eq!(device.submit(Kind::Deactivate, 2, SIM, false, 4), Err(Refusal::NoContext));
}

#[test]
fn a_watch_is_never_more_than_one_snapshot_behind_and_says_when_it_coalesced() {
	let mut watch = Watch::default();
	assert_eq!(watch.take(), None);
	watch.changed();
	assert_eq!(watch.take(), Some(WatchKind::Snapshot));
	watch.changed();
	watch.changed();
	watch.changed();
	assert_eq!(watch.take(), Some(WatchKind::Resync), "three changes, one newest snapshot, marked");
	assert_eq!(watch.take(), None);
	// A snapshot that could not be delivered is due again as what it was...
	watch.changed();
	let taken = watch.take().unwrap();
	watch.requeue(taken);
	assert_eq!(watch.take(), Some(WatchKind::Snapshot), "nothing was missed: still a snapshot");
	watch.changed();
	watch.changed();
	let resync = watch.take().unwrap();
	watch.requeue(resync);
	assert_eq!(watch.take(), Some(WatchKind::Resync), "a resync stays a resync");
	// ...and a change arriving before it goes makes it a resync.
	watch.changed();
	let taken = watch.take().unwrap();
	watch.changed();
	watch.requeue(taken);
	assert_eq!(watch.take(), Some(WatchKind::Resync));
}

#[test]
fn a_packet_queue_is_bounded_in_both_count_and_bytes() {
	let mut queue = PacketQueue::default();
	for _ in 0..QUEUE_PACKETS {
		queue.push(&[0u8; 100], 1400).unwrap();
	}
	assert_eq!(queue.push(&[0u8; 100], 1400), Err(PacketRefusal::Busy), "sixty-five packets");
	assert_eq!(queue.push(&[0u8; 1401], 1400), Err(PacketRefusal::Oversized));
	assert_eq!(queue.push(&[0u8; 100], 5000).err(), Some(PacketRefusal::Busy), "a larger MTU is still capped at 4096 and the queue is still full");
	// A received datagram that does not fit is dropped and counted.
	assert!(!queue.receive(&[0u8; 100], 1400));
	assert_eq!(queue.dropped(), 1);
	queue.clear();
	assert!(queue.is_empty());
	// The byte bound: 64 packets of 4096 are exactly 256 kB, so it binds with larger packets first.
	let mut bytes = PacketQueue::default();
	for _ in 0..QUEUE_PACKETS {
		bytes.push(&[0u8; 4096], 4096).unwrap();
	}
	assert_eq!(bytes.push(&[0u8; 1], 4096), Err(PacketRefusal::Busy));
}
