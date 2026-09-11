// The three cases the frozen contract creates, and the guarantee they add up to.
//
// A consumer must either see an event for an identity or be told to resync. These check both halves
// of that sentence, and the third case checks the sentence itself: after a resync, the consumer's
// view equals the tables.

use super::*;
use crate::ipv6::Address;

fn interface() -> Interface {
	Interface::new(0, 1)
}

fn address(low: u16) -> Address {
	let mut bytes = [0u8; 16];
	bytes[0] = 0x20;
	bytes[1] = 0x01;
	bytes[14..16].copy_from_slice(&low.to_be_bytes());
	Address::new(bytes)
}

fn event(low: u16, change: Change, generation: u64) -> Invalidation {
	Invalidation { identity: Identity::Address { interface: interface(), address: address(low) }, change, generation }
}

#[test]
fn two_changes_to_one_identity_collapse_to_the_later_one() {
	let mut queue = InvalidationQueue::new();
	assert_eq!(queue.record(event(1, Change::Changed, 1)), Recorded::Queued);
	assert_eq!(queue.record(event(1, Change::Invalidated, 2)), Recorded::Coalesced);
	assert_eq!(queue.len(), 1, "one identity is one slot however often it changes");
	let drained = queue.drain();
	assert_eq!(drained.len(), 1);
	assert_eq!(drained[0].change, Change::Invalidated, "the later state wins");
	assert_eq!(drained[0].generation, 2);
	assert!(!queue.resync_required(), "coalescing loses nothing, so nothing is owed");
}

#[test]
fn a_flapping_identity_cannot_fill_the_queue() {
	let mut queue = InvalidationQueue::new();
	for round in 0..1000u64 {
		let change = if round % 2 == 0 { Change::Changed } else { Change::Invalidated };
		queue.record(event(7, change, round));
	}
	assert_eq!(queue.len(), 1);
	assert!(!queue.resync_required());
}

#[test]
fn the_thirty_third_distinct_identity_asks_for_a_resync_without_growing_the_queue() {
	let mut queue = InvalidationQueue::new();
	for index in 0..INVALIDATION_CAPACITY as u16 {
		assert_eq!(queue.record(event(index, Change::Changed, u64::from(index))), Recorded::Queued);
	}
	assert_eq!(queue.len(), INVALIDATION_CAPACITY);
	assert!(!queue.resync_required());

	assert_eq!(queue.record(event(999, Change::Invalidated, 100)), Recorded::ResyncRequired);
	assert_eq!(queue.len(), INVALIDATION_CAPACITY, "the queue does not grow");
	assert!(queue.resync_required());
	assert_eq!(queue.overflow_discards(), 1);

	// An identity ALREADY in the queue still coalesces while the flag is up: it costs no slot.
	assert_eq!(queue.record(event(3, Change::Invalidated, 101)), Recorded::Coalesced);
	assert_eq!(queue.overflow_discards(), 1);

	// Further new identities are discarded and counted, and the flag stays up.
	assert_eq!(queue.record(event(998, Change::Changed, 102)), Recorded::ResyncRequired);
	assert_eq!(queue.overflow_discards(), 2);
}

#[test]
fn draining_the_queue_never_clears_a_missed_invalidation() {
	let mut queue = InvalidationQueue::new();
	for index in 0..=INVALIDATION_CAPACITY as u16 {
		queue.record(event(index, Change::Changed, u64::from(index)));
	}
	assert!(queue.resync_required());
	let drained = queue.drain();
	assert_eq!(drained.len(), INVALIDATION_CAPACITY);
	assert!(queue.is_empty());
	assert!(queue.resync_required(), "an empty queue is not a synchronised consumer");
}

#[test]
fn only_an_installed_snapshot_at_least_as_new_as_the_miss_clears_the_flag() {
	let mut queue = InvalidationQueue::new();
	for index in 0..=INVALIDATION_CAPACITY as u16 {
		queue.record(event(index, Change::Changed, 50));
	}
	assert!(queue.resync_required());

	// A snapshot taken BEFORE the mutation that overflowed the queue proves nothing.
	assert!(!queue.snapshot_installed(49, 49));
	assert!(queue.resync_required());

	// A snapshot that raced a later mutation is retried rather than believed.
	assert!(!queue.snapshot_installed(50, 51));
	assert!(queue.resync_required());

	// A snapshot at the current generation, no older than the miss, clears it.
	assert!(queue.snapshot_installed(50, 50));
	assert!(!queue.resync_required());
}

#[test]
fn a_resync_leaves_the_consumer_holding_what_the_tables_hold() {
	// The consumer's view after a resync is the snapshot plus every event NEWER than it. Events the
	// snapshot already covers are discarded, because re-reading them would be re-reading what the
	// consumer has just read for itself.
	let mut queue = InvalidationQueue::new();
	for index in 0..=INVALIDATION_CAPACITY as u16 {
		queue.record(event(index, Change::Changed, 10));
	}
	// A change that happened after the snapshot generation.
	queue.record(event(5, Change::Invalidated, 11));
	assert!(queue.snapshot_installed(10, 10));
	let remaining = queue.drain();
	assert_eq!(remaining.len(), 1, "only what the snapshot could not have seen survives");
	assert_eq!(remaining[0].change, Change::Invalidated);
	assert_eq!(remaining[0].generation, 11);
}

#[test]
fn a_snapshot_when_nothing_was_missed_is_accepted_and_changes_nothing() {
	let mut queue = InvalidationQueue::new();
	queue.record(event(1, Change::Changed, 1));
	assert!(queue.snapshot_installed(1, 1), "no flag to clear");
	assert_eq!(queue.len(), 1, "an unforced snapshot does not discard queued events");
}

#[test]
fn every_identity_names_the_interface_it_belongs_to() {
	let other = Interface::new(0, 2);
	let first = Identity::Address { interface: interface(), address: address(1) };
	let after_replacement = Identity::Address { interface: other, address: address(1) };
	assert_ne!(first, after_replacement, "the same address on a replaced NIC is a different thing");
	assert_eq!(first.interface(), interface());
	assert_eq!(after_replacement.interface(), other);
	assert_eq!(Identity::InterfaceState { interface: other }.interface(), other);

	// And the two are separate slots in the queue rather than one coalescing pair.
	let mut queue = InvalidationQueue::new();
	queue.record(Invalidation { identity: first, change: Change::Changed, generation: 1 });
	queue.record(Invalidation { identity: after_replacement, change: Change::Changed, generation: 2 });
	assert_eq!(queue.len(), 2);
}

fn quoted(port: u16) -> QuotedError {
	QuotedError { interface: interface(), reporter: address(0xfffe), class: ErrorClass::PacketTooBig { mtu: 1300 }, quoted_source: address(1), quoted_destination: address(2), transport: QuotedTransport::Udp { source_port: port, destination_port: 53 } }
}

#[test]
fn the_advisory_queue_drops_the_newest_under_a_flood_and_counts_it() {
	let mut queue = QuotedErrorQueue::new();
	for index in 0..QUOTED_ERROR_CAPACITY as u16 {
		assert!(queue.offer(quoted(index)), "room for the first {QUOTED_ERROR_CAPACITY}");
	}
	assert!(!queue.offer(quoted(9999)), "the flood does not evict the diagnosis");
	assert_eq!(queue.dropped(), 1);
	assert_eq!(queue.len(), QUOTED_ERROR_CAPACITY);
	let drained = queue.drain();
	assert_eq!(drained[0].transport, QuotedTransport::Udp { source_port: 0, destination_port: 53 }, "the oldest survived");
	assert!(queue.is_empty());
	assert_eq!(queue.dropped(), 1, "the counter is not reset by draining");
}

#[test]
fn a_quoted_error_carries_the_field_its_consumer_needs() {
	assert_eq!(ErrorClass::PacketTooBig { mtu: 1300 }, ErrorClass::PacketTooBig { mtu: 1300 });
	assert_ne!(ErrorClass::PacketTooBig { mtu: 1300 }, ErrorClass::PacketTooBig { mtu: 1280 });
	let parameter = ErrorClass::ParameterProblem { code: 2, pointer: 42 };
	match parameter {
		ErrorClass::ParameterProblem { code, pointer } => {
			assert_eq!(code, 2);
			assert_eq!(pointer, 42, "the pointer is what makes the report actionable");
		}
		other => panic!("wrong class: {other:?}"),
	}
	// The TCP identity carries the sequence the consumer checks against its own send interval - the
	// check this layer cannot make.
	let tcp = QuotedTransport::Tcp { source_port: 1234, destination_port: 80, sequence: 0x1000 };
	assert_ne!(tcp, QuotedTransport::Tcp { source_port: 1234, destination_port: 80, sequence: 0x1001 });
}

#[test]
fn a_dropped_advisory_error_never_asks_for_a_resync() {
	// THE TWO QUEUES FAIL DIFFERENTLY, and this is the line between them. Losing a table
	// invalidation leaves a consumer holding state that is no longer true, so the queue says so;
	// losing an advisory error costs a diagnosis and nothing else, so it is counted and forgotten.
	// A listener that raised the resync flag here would make every flood of errors cost a full table
	// re-read.
	let mut invalidations = InvalidationQueue::new();
	let mut errors = QuotedErrorQueue::new();
	for index in 0..QUOTED_ERROR_CAPACITY as u16 {
		assert!(errors.offer(quoted(index)));
	}
	assert!(!errors.offer(quoted(9999)), "the thirty-third is dropped");
	assert_eq!(errors.dropped(), 1);
	assert!(!invalidations.resync_required(), "and the table queue is untouched by it");

	// Meanwhile the invalidation queue's own overflow does raise it, which is what makes the
	// difference visible rather than assumed.
	for index in 0..=INVALIDATION_CAPACITY as u16 {
		invalidations.record(event(index, Change::Changed, 1));
	}
	assert!(invalidations.resync_required());
	assert_eq!(errors.dropped(), 1, "and the error counter is not touched by the table queue either");
}
