use super::*;

#[derive(Clone, PartialEq, Debug)]
struct State {
	value: u32,
	alarm: bool,
	controls: Controls,
}

impl Payload for State {
	fn alarm_transition(&self, before: &Self) -> bool {
		self.alarm != before.alarm
	}
	fn controls(&self) -> Controls {
		self.controls
	}
}

fn state(value: u32) -> State {
	State { value, alarm: false, controls: Controls::default() }
}

fn ups(value: u32) -> State {
	State { value, alarm: false, controls: Controls { set_output: true, schedule_off: true, cancel_off: true, outlets: 2 } }
}

fn publication(slot: u32, generation: u32) -> Publication {
	Publication { slot, generation, binding_generation: 7 }
}

fn key(slot: u32, generation: u32, local: u32) -> SourceKey {
	SourceKey { slot, generation, binding_generation: 7, local }
}

// A registry with one live provider describing `locals`, at revision 10 of its own.
fn live(locals: &[u32]) -> Registry<State> {
	let mut registry = Registry::new(1);
	assert!(registry.add_provider(ProviderId(1), publication(3, 1), 0));
	for &local in locals {
		registry.frame(ProviderId(1), Frame::Snapshot { revision: 10, local, state: ups(local) }, 0).unwrap();
	}
	registry.frame(ProviderId(1), Frame::SnapshotEnd { revision: 10 }, 0).unwrap();
	registry
}

// Everything a subscriber can take now, with a channel that takes all of it.
fn take(registry: &mut Registry<State>, id: SubscriberId, now: u64) -> Vec<Change<State>> {
	let mut out = Vec::new();
	registry
		.drain(id, now, |change| {
			out.push(change.clone());
			true
		})
		.unwrap();
	out
}

#[test]
fn a_providers_snapshot_is_admitted_whole_and_enumerated_in_identity_order() {
	let mut registry = Registry::new(1);
	registry.add_provider(ProviderId(1), publication(3, 1), 0);
	registry.frame(ProviderId(1), Frame::Snapshot { revision: 4, local: 2, state: state(20) }, 0).unwrap();
	registry.frame(ProviderId(1), Frame::Snapshot { revision: 4, local: 0, state: state(0) }, 0).unwrap();
	assert_eq!(registry.source_count(), 0, "nothing is visible until the snapshot ends");
	let admitted = registry.frame(ProviderId(1), Frame::SnapshotEnd { revision: 4 }, 0).unwrap();
	assert_eq!(admitted, Admitted { sources: 2, exhausted: 0 });
	let keys: Vec<SourceKey> = registry.sources().iter().map(|(key, _, _)| *key).collect();
	assert_eq!(keys, alloc::vec![key(3, 1, 0), key(3, 1, 2)]);
	assert_eq!(registry.revision(), 2, "one revision per admitted source");
}

#[test]
fn a_frame_out_of_its_protocol_ends_the_provider() {
	let mut registry = Registry::new(1);
	registry.add_provider(ProviderId(1), publication(3, 1), 0);
	assert_eq!(registry.frame(ProviderId(1), Frame::Updated { revision: 1, local: 0, state: state(1) }, 0), Err(Refusal::Order), "a change before the snapshot ends");
	registry.frame(ProviderId(1), Frame::Snapshot { revision: 4, local: 0, state: state(0) }, 0).unwrap();
	assert_eq!(registry.frame(ProviderId(1), Frame::Snapshot { revision: 4, local: 0, state: state(0) }, 0), Err(Refusal::Duplicate));
	assert_eq!(registry.frame(ProviderId(1), Frame::Snapshot { revision: 5, local: 1, state: state(0) }, 0), Err(Refusal::Order), "one snapshot has one revision");
	assert_eq!(registry.frame(ProviderId(1), Frame::Snapshot { revision: 4, local: 16, state: state(0) }, 0), Err(Refusal::Local));
	registry.frame(ProviderId(1), Frame::SnapshotEnd { revision: 4 }, 0).unwrap();
	assert_eq!(registry.frame(ProviderId(1), Frame::Snapshot { revision: 5, local: 1, state: state(0) }, 0), Err(Refusal::Order), "a snapshot after it ended");
	assert_eq!(registry.frame(ProviderId(1), Frame::Updated { revision: 4, local: 0, state: state(1) }, 0), Err(Refusal::Regression), "a revision not after the snapshot's");
	assert_eq!(registry.frame(ProviderId(1), Frame::Updated { revision: 5, local: 9, state: state(1) }, 0), Err(Refusal::Unregistered));
	registry.frame(ProviderId(1), Frame::Removed { revision: 6, local: 0 }, 0).unwrap();
	// STALE AFTER REMOVAL: an update for what was removed, and the same local identity described again.
	assert_eq!(registry.frame(ProviderId(1), Frame::Updated { revision: 7, local: 0, state: state(1) }, 0), Err(Refusal::Unregistered));
	assert_eq!(registry.frame(ProviderId(1), Frame::Added { revision: 8, local: 0, state: state(1) }, 0), Err(Refusal::Duplicate));
	assert_eq!(registry.frame(ProviderId(9), Frame::SnapshotEnd { revision: 1 }, 0), Err(Refusal::Gone));
}

#[test]
fn a_provider_that_goes_takes_every_source_with_it_and_a_replacement_is_another_source() {
	let mut registry = live(&[0, 1]);
	let subscriber = registry.subscribe().unwrap();
	registry.remove_provider(ProviderId(1));
	assert_eq!(registry.source_count(), 0, "the last healthy value does not stay live");
	let changes = take(&mut registry, subscriber, 0);
	assert_eq!(changes.len(), 2);
	assert!(changes.iter().all(|change| matches!(change, Change::Removed { .. })));
	// The replacement publication has a new generation, so its local 0 is a different source.
	registry.add_provider(ProviderId(2), publication(3, 2), 0);
	registry.frame(ProviderId(2), Frame::Snapshot { revision: 1, local: 0, state: state(5) }, 0).unwrap();
	registry.frame(ProviderId(2), Frame::SnapshotEnd { revision: 1 }, 0).unwrap();
	assert_eq!(registry.sources()[0].0, key(3, 2, 0));
	assert_ne!(registry.sources()[0].0, key(3, 1, 0));
}

#[test]
fn the_limits_refuse_rather_than_evict() {
	let mut registry: Registry<State> = Registry::new(1);
	for provider in 0..MAX_PROVIDERS as u32 {
		assert!(registry.add_provider(ProviderId(provider), publication(provider, 1), 0));
		for local in 0..MAX_LOCAL_SOURCES {
			registry.frame(ProviderId(provider), Frame::Snapshot { revision: 1, local, state: state(local) }, 0).unwrap();
		}
		assert_eq!(registry.frame(ProviderId(provider), Frame::SnapshotEnd { revision: 1 }, 0).unwrap().sources, MAX_LOCAL_SOURCES as usize);
	}
	// Eight providers of sixteen sources each ARE the service's 128, and the ninth provider is refused
	// without anybody's source going.
	assert_eq!(registry.source_count(), MAX_SOURCES);
	assert!(!registry.provider_room());
	assert!(!registry.add_provider(ProviderId(99), publication(99, 1), 0));
	assert_eq!(registry.source_count(), MAX_SOURCES);
	// And one publication is held once.
	let mut single: Registry<State> = Registry::new(1);
	assert!(single.add_provider(ProviderId(1), publication(3, 1), 0));
	assert!(!single.add_provider(ProviderId(2), publication(3, 1), 0));
	for _ in 0..MAX_SUBSCRIBERS {
		assert!(single.subscribe().is_some());
	}
	assert!(single.subscribe().is_none(), "the seventeenth subscriber is refused");
}

#[test]
fn a_subscription_starts_after_its_snapshot_and_changes_carry_larger_revisions() {
	let mut registry = live(&[0]);
	let at = registry.revision();
	let subscriber = registry.subscribe().unwrap();
	registry.frame(ProviderId(1), Frame::Updated { revision: 11, local: 0, state: ups(42) }, 5).unwrap();
	let changes = take(&mut registry, subscriber, 5);
	assert_eq!(changes.len(), 1);
	match &changes[0] {
		Change::Updated { revision, received, state, .. } => {
			assert!(*revision > at);
			assert_eq!((*received, state.value), (5, 42));
		}
		other => panic!("an update, not {other:?}"),
	}
}

#[test]
fn ordinary_measurements_coalesce_to_the_latest_and_go_at_most_once_per_hundred_milliseconds() {
	let mut registry = live(&[0, 1]);
	let subscriber = registry.subscribe().unwrap();
	registry.frame(ProviderId(1), Frame::Updated { revision: 11, local: 0, state: ups(1) }, 0).unwrap();
	assert_eq!(take(&mut registry, subscriber, 0).len(), 1, "the first goes at once");
	// Five more within the window: they coalesce to one record carrying the latest state.
	for (n, value) in (2..7).enumerate() {
		registry.frame(ProviderId(1), Frame::Updated { revision: 12 + n as u64, local: 0, state: ups(value) }, 1).unwrap();
	}
	assert_eq!(registry.queued(subscriber), 1);
	assert!(take(&mut registry, subscriber, 5).is_empty(), "held until its source's window passes");
	assert_eq!(registry.next_deadline(), Some(COALESCE_TICKS));
	let released = take(&mut registry, subscriber, COALESCE_TICKS);
	assert_eq!(released.len(), 1);
	match &released[0] {
		Change::Updated { state, revision, .. } => {
			assert_eq!(state.value, 6, "the latest state");
			assert_eq!(*revision, registry.revision(), "revisions skipped: five changes, one record");
		}
		other => panic!("an update, not {other:?}"),
	}
	// Another source's window is its own.
	registry.frame(ProviderId(1), Frame::Updated { revision: 30, local: 1, state: ups(9) }, COALESCE_TICKS).unwrap();
	assert_eq!(take(&mut registry, subscriber, COALESCE_TICKS).len(), 1);
}

#[test]
fn alarm_transitions_additions_and_removals_are_never_coalesced_away() {
	let mut registry = live(&[0, 1]);
	let subscriber = registry.subscribe().unwrap();
	let mut alarmed = ups(0);
	for revision in 11..15u64 {
		alarmed.alarm = !alarmed.alarm;
		registry.frame(ProviderId(1), Frame::Updated { revision, local: 0, state: alarmed.clone() }, 0).unwrap();
	}
	assert_eq!(registry.queued(subscriber), 4, "four transitions, four records");
	// An ordinary update behind them still coalesces with its own kind only.
	registry.frame(ProviderId(1), Frame::Updated { revision: 15, local: 0, state: alarmed.clone() }, 0).unwrap();
	registry.frame(ProviderId(1), Frame::Updated { revision: 16, local: 0, state: alarmed.clone() }, 0).unwrap();
	assert_eq!(registry.queued(subscriber), 5);
	// A removal supersedes the unread ordinary update and nothing else.
	registry.frame(ProviderId(1), Frame::Removed { revision: 17, local: 0 }, 0).unwrap();
	assert_eq!(registry.queued(subscriber), 5, "four transitions and the removal");
	let changes = take(&mut registry, subscriber, 0);
	assert!(matches!(changes.last(), Some(Change::Removed { .. })));
	let revisions: Vec<u64> = changes
		.iter()
		.map(|change| match change {
			Change::Added { revision, .. } | Change::Updated { revision, .. } | Change::Removed { revision, .. } => *revision,
		})
		.collect();
	assert!(revisions.windows(2).all(|pair| pair[0] < pair[1]), "delivered in revision order: {revisions:?}");
}

#[test]
fn transitions_that_fill_the_queue_close_the_subscription_and_a_new_one_starts_from_the_latest() {
	let mut registry = live(&[0]);
	let subscriber = registry.subscribe().unwrap();
	let mut alarmed = ups(0);
	for revision in 11..11 + QUEUE_RECORDS as u64 + 1 {
		alarmed.alarm = !alarmed.alarm;
		alarmed.value = revision as u32;
		registry.frame(ProviderId(1), Frame::Updated { revision, local: 0, state: alarmed.clone() }, 0).unwrap();
	}
	// The producer was never refused or blocked; the subscriber is what was closed.
	assert_eq!(registry.drain(subscriber, 0, |_| true), Err(Closed::Overflow));
	registry.unsubscribe(subscriber);
	assert_eq!(registry.subscriber_count(), 0, "everything charged to it is released");
	let fresh = registry.subscribe().unwrap();
	assert_eq!(registry.sources()[0].2, alarmed, "a new subscription's snapshot is the latest state");
	assert!(take(&mut registry, fresh, 0).is_empty());
}

#[test]
fn a_reader_that_takes_nothing_for_five_seconds_is_closed() {
	let mut registry = live(&[0]);
	let subscriber = registry.subscribe().unwrap();
	registry.frame(ProviderId(1), Frame::Updated { revision: 11, local: 0, state: ups(1) }, 100).unwrap();
	assert_eq!(registry.drain(subscriber, 100, |_| false), Ok(()), "a full channel is waited on");
	assert_eq!(registry.next_deadline(), Some(100 + DRAIN_TICKS));
	assert_eq!(registry.drain(subscriber, 100 + DRAIN_TICKS - 1, |_| false), Ok(()));
	assert_eq!(registry.drain(subscriber, 100 + DRAIN_TICKS, |_| false), Err(Closed::Stalled));
	// A reader that takes something in time is not closed.
	let mut again = live(&[0]);
	let reader = again.subscribe().unwrap();
	again.frame(ProviderId(1), Frame::Updated { revision: 11, local: 0, state: ups(1) }, 0).unwrap();
	again.drain(reader, 0, |_| false).unwrap();
	again.drain(reader, DRAIN_TICKS - 1, |_| true).unwrap();
	again.frame(ProviderId(1), Frame::Updated { revision: 12, local: 0, state: ups(2) }, DRAIN_TICKS).unwrap();
	assert_eq!(again.drain(reader, DRAIN_TICKS + COALESCE_TICKS, |_| false), Ok(()), "the clock restarted when it read");
}

#[test]
fn a_control_is_checked_against_the_live_source_and_what_it_advertises() {
	let mut registry = live(&[0]);
	registry.add_provider(ProviderId(2), publication(4, 1), 0);
	registry.frame(ProviderId(2), Frame::Snapshot { revision: 1, local: 0, state: state(0) }, 0).unwrap();
	registry.frame(ProviderId(2), Frame::SnapshotEnd { revision: 1 }, 0).unwrap();
	let on = Action::SetOutput { outlet: 1, on: true };
	// FORGED: a generation that is not the publication's, a local it never described.
	assert_eq!(registry.control(key(3, 2, 0), on, 0), Err(ControlRefusal::Denied));
	assert_eq!(registry.control(key(3, 1, 5), on, 0), Err(ControlRefusal::Denied));
	assert_eq!(registry.control(SourceKey { binding_generation: 8, ..key(3, 1, 0) }, on, 0), Err(ControlRefusal::Denied));
	// A read-only source advertises nothing.
	assert_eq!(registry.control(key(4, 1, 0), on, 0), Err(ControlRefusal::Unsupported));
	assert_eq!(registry.control(key(4, 1, 0), Action::CancelOff, 0), Err(ControlRefusal::Unsupported));
	// An outlet it does not have; a delay past a day.
	assert_eq!(registry.control(key(3, 1, 0), Action::SetOutput { outlet: 2, on: true }, 0), Err(ControlRefusal::Invalid));
	assert_eq!(registry.control(key(3, 1, 0), Action::ScheduleOff { delay_seconds: MAX_DELAY_SECONDS + 1 }, 0), Err(ControlRefusal::Invalid));
	let dispatch = registry.control(key(3, 1, 0), Action::ScheduleOff { delay_seconds: MAX_DELAY_SECONDS }, 0).unwrap();
	assert_eq!((dispatch.provider, dispatch.local), (ProviderId(1), 0));
	// ONE OUTSTANDING PER PROVIDER.
	assert_eq!(registry.control(key(3, 1, 0), on, 1), Err(ControlRefusal::Busy));
	assert!(!registry.control_answered(ProviderId(1), dispatch.corr + 100), "an answer to something else is not this one");
	assert!(registry.control_answered(ProviderId(1), dispatch.corr));
	assert!(!registry.control_answered(ProviderId(1), dispatch.corr), "and it is answered once");
	assert!(registry.control(key(3, 1, 0), on, 2).is_ok());
}

#[test]
fn an_unanswered_control_is_indeterminate_is_never_resent_and_blocks_conflicts_until_reconciled() {
	let mut registry = live(&[0]);
	let off = Action::SetOutput { outlet: 0, on: false };
	let dispatch = registry.control(key(3, 1, 0), off, 0).unwrap();
	assert!(registry.tick(CONTROL_TICKS - 1).is_empty());
	let effects = registry.tick(CONTROL_TICKS);
	assert_eq!(effects.len(), 2);
	assert_eq!(effects[0], Effect::Indeterminate { corr: dispatch.corr });
	let Effect::Query(query) = effects[1] else { panic!("a reconciliation query, not {:?}", effects[1]) };
	assert_eq!((query.provider, query.local), (ProviderId(1), 0));
	// NO REPLAY: time passing produces no second dispatch of the control.
	assert!(registry.tick(CONTROL_TICKS + 1).is_empty());
	// NO CONFLICTING CONTROL before the reconciliation answers.
	assert_eq!(registry.control(key(3, 1, 0), Action::SetOutput { outlet: 0, on: true }, CONTROL_TICKS + 1), Err(ControlRefusal::Busy));
	// The late reply to the original control is dropped rather than delivered.
	assert!(!registry.control_answered(ProviderId(1), dispatch.corr));
	// Fresh state reconciles, is published, and controls are admitted again.
	let subscriber = registry.subscribe().unwrap();
	assert!(registry.query_answered(ProviderId(1), query.corr, Some(ups(77)), CONTROL_TICKS + 2));
	assert_eq!(take(&mut registry, subscriber, CONTROL_TICKS + 2).len(), 1);
	assert!(registry.control(key(3, 1, 0), Action::SetOutput { outlet: 0, on: true }, CONTROL_TICKS + 3).is_ok());
}

#[test]
fn a_silent_reconciliation_leaves_controls_unavailable_until_the_provider_answers_again() {
	let mut registry = live(&[0]);
	let dispatch = registry.control(key(3, 1, 0), Action::CancelOff, 0).unwrap();
	let effects = registry.tick(CONTROL_TICKS);
	assert_eq!(effects[0], Effect::Indeterminate { corr: dispatch.corr });
	assert!(registry.tick(2 * CONTROL_TICKS).is_empty(), "the query's deadline passes silently");
	let at = 2 * CONTROL_TICKS + 1;
	assert_eq!(registry.control(key(3, 1, 0), Action::CancelOff, at), Err(ControlRefusal::Unavailable));
	// Refusing started a fresh query - a query, never the control.
	let effects = registry.tick(at);
	assert_eq!(effects.len(), 1);
	let Effect::Query(query) = effects[0] else { panic!("a query, not {:?}", effects[0]) };
	assert_eq!(registry.control(key(3, 1, 0), Action::CancelOff, at), Err(ControlRefusal::Unavailable), "while it is outstanding, and no second query");
	assert!(registry.tick(at).is_empty());
	assert!(registry.query_answered(ProviderId(1), query.corr, Some(ups(0)), at + 1));
	assert!(registry.control(key(3, 1, 0), Action::CancelOff, at + 2).is_ok());
	// A refusal to answer the query leaves the controls unavailable too, and an ordinary update does not
	// settle them: only a query's answer says what the control did.
	let mut refused = live(&[0]);
	refused.control(key(3, 1, 0), Action::CancelOff, 0).unwrap();
	let Effect::Query(query) = refused.tick(CONTROL_TICKS)[1] else { panic!("a query") };
	assert!(refused.query_answered(ProviderId(1), query.corr, None, CONTROL_TICKS + 1));
	refused.frame(ProviderId(1), Frame::Updated { revision: 11, local: 0, state: ups(3) }, CONTROL_TICKS + 2).unwrap();
	assert_eq!(refused.control(key(3, 1, 0), Action::CancelOff, CONTROL_TICKS + 3), Err(ControlRefusal::Unavailable));
	let Effect::Query(again) = refused.tick(CONTROL_TICKS + 3)[0] else { panic!("a fresh query") };
	assert!(refused.query_answered(ProviderId(1), again.corr, Some(ups(4)), CONTROL_TICKS + 4));
	assert!(refused.control(key(3, 1, 0), Action::CancelOff, CONTROL_TICKS + 5).is_ok());
}

#[test]
fn only_the_outstanding_query_settles_a_source() {
	let mut registry = live(&[0]);
	registry.control(key(3, 1, 0), Action::CancelOff, 0).unwrap();
	let Effect::Query(first) = registry.tick(CONTROL_TICKS)[1] else { panic!("a query") };
	// The answer arrives for a query that is no longer the one outstanding: the provider was asked
	// again in between. Only the outstanding one counts.
	assert!(!registry.query_answered(ProviderId(1), first.corr + 1, Some(ups(1)), CONTROL_TICKS + 1));
	assert_eq!(registry.control(key(3, 1, 0), Action::CancelOff, CONTROL_TICKS + 1), Err(ControlRefusal::Busy));
}

#[test]
fn a_provider_lost_mid_control_is_indeterminate_and_another_provider_carries_on() {
	let mut registry = live(&[0]);
	registry.add_provider(ProviderId(2), publication(4, 1), 0);
	registry.frame(ProviderId(2), Frame::Snapshot { revision: 1, local: 0, state: ups(0) }, 0).unwrap();
	registry.frame(ProviderId(2), Frame::SnapshotEnd { revision: 1 }, 0).unwrap();
	let lost = registry.control(key(3, 1, 0), Action::CancelOff, 0).unwrap();
	assert_eq!(registry.remove_provider(ProviderId(1)), alloc::vec![Effect::Indeterminate { corr: lost.corr }]);
	assert!(registry.control(key(4, 1, 0), Action::CancelOff, 1).is_ok(), "the other provider's controls were never tied to it");
	assert_eq!(registry.control(key(3, 1, 0), Action::CancelOff, 1), Err(ControlRefusal::Denied), "and the lost one's source is gone");
}

#[test]
fn a_provider_that_never_finishes_its_snapshot_is_ended() {
	let mut registry: Registry<State> = Registry::new(1);
	registry.add_provider(ProviderId(1), publication(3, 1), 0);
	registry.add_provider(ProviderId(2), publication(4, 1), 0);
	registry.frame(ProviderId(2), Frame::SnapshotEnd { revision: 1 }, 0).unwrap();
	assert_eq!(registry.next_deadline(), Some(SNAPSHOT_TICKS));
	assert!(registry.tick(SNAPSHOT_TICKS - 1).is_empty());
	assert_eq!(registry.tick(SNAPSHOT_TICKS), alloc::vec![Effect::ProviderFailed(ProviderId(1))]);
	assert!(!registry.has_provider(ProviderId(1)));
	assert!(registry.has_provider(ProviderId(2)), "a provider that described itself in time is untouched");
}
