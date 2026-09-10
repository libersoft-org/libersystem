// The rule these bounds exist to enforce: a hostile link cannot make this host allocate, and a
// refusal costs the caller a typed answer rather than a live record.

use super::*;
use crate::ipv6::Address;
use alloc::vec;

fn interface() -> Interface {
	Interface::new(0, 1)
}

fn neighbour(low: u16) -> Address {
	let mut bytes = [0u8; 16];
	bytes[0] = 0xfe;
	bytes[1] = 0x80;
	bytes[14..16].copy_from_slice(&low.to_be_bytes());
	Address::new(bytes)
}

#[test]
fn every_resource_is_in_the_list_and_carries_the_capacity_the_plan_fixed() {
	assert_eq!(RESOURCES.len(), 17);
	for (index, resource) in RESOURCES.iter().enumerate() {
		assert_eq!(resource.index(), index, "the enum order is the counter order");
	}
	assert_eq!(Resource::Interfaces.limit(), 1);
	assert_eq!(Resource::UnicastAddresses.limit(), 16);
	assert_eq!(Resource::Prefixes.limit(), 15);
	assert_eq!(Resource::Routes.limit(), 32);
	assert_eq!(Resource::DefaultRouters.limit(), 8);
	assert_eq!(Resource::AdvertisersPerPrefix.limit(), 8);
	assert_eq!(Resource::Rdnss.limit(), 4);
	assert_eq!(Resource::Neighbours.limit(), 64);
	assert_eq!(Resource::PathMtu.limit(), 64);
	assert_eq!(Resource::MldGroups.limit(), 32);
	assert_eq!(Resource::MldSourcesPerRecord.limit(), 64);
	assert_eq!(Resource::InvalidationEvents.limit(), 32);
	assert_eq!(Resource::QuotedErrorEvents.limit(), 32);
	assert_eq!(Resource::PendingPerNeighbour.limit(), 4);
	assert_eq!(Resource::PendingPerInterface.limit(), 32);
	assert_eq!(Resource::PendingBytes.limit(), 65536);
	assert_eq!(Resource::DueActionsPerIteration.limit(), 16);
}

#[test]
fn refusal_counters_saturate_rather_than_wrap() {
	let mut refusals = Refusals::new();
	for _ in 0..5 {
		refusals.record(Resource::Prefixes);
	}
	assert_eq!(refusals.get(Resource::Prefixes), 5);
	assert_eq!(refusals.get(Resource::Routes), 0, "one resource's refusals are not another's");
	assert_eq!(refusals.total(), 5);
	// A counter that wrapped would report no refusals at exactly the wrong moment.
	let mut saturated = Refusals::new();
	for _ in 0..3 {
		saturated.record(Resource::Neighbours);
	}
	assert_eq!(saturated.get(Resource::Neighbours), 3);
}

#[test]
fn a_single_unanswering_neighbour_holds_four_packets_and_no_more() {
	let mut queue = PendingQueue::new();
	for index in 0..4 {
		queue.admit(interface(), neighbour(1), vec![index as u8; 100]).expect("within the per-neighbour budget");
	}
	let refused = queue.admit(interface(), neighbour(1), vec![0; 100]).expect_err("the fifth is refused");
	assert_eq!(refused.resource, Resource::PendingPerNeighbour);
	assert_eq!(refused.limit, 4);
	assert_eq!(queue.len(), 4, "the refusal did not evict a live packet");
	assert_eq!(queue.bytes(), 400, "and charged nothing");
	assert_eq!(queue.refusals().get(Resource::PendingPerNeighbour), 1);

	// ANOTHER NEIGHBOUR IS ANOTHER BUDGET. One peer that never answers must not stop the link.
	queue.admit(interface(), neighbour(2), vec![0; 100]).expect("a different neighbour has its own room");
	assert_eq!(queue.len(), 5);
}

#[test]
fn the_interface_budget_stops_a_link_full_of_unanswering_neighbours() {
	let mut queue = PendingQueue::new();
	// Eight neighbours with four packets each is thirty-two, which is the interface limit.
	for peer in 0..8u16 {
		for _ in 0..4 {
			queue.admit(interface(), neighbour(peer), vec![0; 10]).expect("within both budgets");
		}
	}
	assert_eq!(queue.len(), 32);
	let refused = queue.admit(interface(), neighbour(99), vec![0; 10]).expect_err("the interface is full");
	assert_eq!(refused.resource, Resource::PendingPerInterface);
	assert_eq!(refused.limit, 32);
	assert_eq!(queue.len(), 32);
}

#[test]
fn the_byte_budget_stops_a_few_large_packets() {
	let mut queue = PendingQueue::new();
	// Sixteen frames of 4096 bytes is 65536: the whole budget, inside both count budgets.
	for peer in 0..4u16 {
		for _ in 0..4 {
			queue.admit(interface(), neighbour(peer), vec![0; 4096]).expect("within the budget");
		}
	}
	assert_eq!(queue.bytes(), 65536);
	let refused = queue.admit(interface(), neighbour(9), vec![0; 1]).expect_err("not one more byte");
	assert_eq!(refused.resource, Resource::PendingBytes);
	assert_eq!(refused.limit, 65536);
	assert_eq!(queue.bytes(), 65536, "a refusal charges nothing");
}

#[test]
fn resolution_releases_the_charges_and_hands_the_packets_back_oldest_first() {
	let mut queue = PendingQueue::new();
	let first = queue.admit(interface(), neighbour(1), vec![1; 50]).expect("admitted");
	let second = queue.admit(interface(), neighbour(1), vec![2; 60]).expect("admitted");
	queue.admit(interface(), neighbour(2), vec![3; 70]).expect("admitted");
	assert_eq!(queue.bytes(), 180);

	let taken = queue.take_for(interface(), neighbour(1));
	assert_eq!(taken.len(), 2);
	assert_eq!(taken[0].token, first, "FIFO: the first admitted goes first");
	assert_eq!(taken[1].token, second);
	assert_eq!(queue.len(), 1, "the other neighbour's packet is untouched");
	assert_eq!(queue.bytes(), 70, "exactly the released charges came off");
}

#[test]
fn a_cancelled_packet_releases_once_and_cannot_be_sent_afterwards() {
	let mut queue = PendingQueue::new();
	let token = queue.admit(interface(), neighbour(1), vec![0; 100]).expect("admitted");
	queue.admit(interface(), neighbour(1), vec![0; 100]).expect("admitted");
	assert_eq!(queue.bytes(), 200);

	assert_eq!(queue.cancel(token), Some(Completion::Cancelled { token }));
	assert_eq!(queue.bytes(), 100, "released exactly once");
	assert_eq!(queue.cancel(token), None, "and cancelling again releases nothing");
	assert_eq!(queue.bytes(), 100);

	// The cancelled packet is not among the ones resolution hands back.
	let taken = queue.take_for(interface(), neighbour(1));
	assert_eq!(taken.len(), 1);
	assert_ne!(taken[0].token, token);
	assert_eq!(queue.bytes(), 0);
}

#[test]
fn a_failure_completes_every_retained_packet_with_its_cause() {
	let mut queue = PendingQueue::new();
	let first = queue.admit(interface(), neighbour(1), vec![0; 10]).expect("admitted");
	let second = queue.admit(interface(), neighbour(1), vec![0; 10]).expect("admitted");
	let completions = queue.fail_for(interface(), neighbour(1), FailureCause::ResolutionFailed);
	assert_eq!(completions, vec![Completion::Failed { token: first, cause: FailureCause::ResolutionFailed }, Completion::Failed { token: second, cause: FailureCause::ResolutionFailed }]);
	assert!(queue.is_empty());
	assert_eq!(queue.bytes(), 0);
	assert_eq!(queue.resolution_failures(), 1);

	// Interface teardown completes everything on it, whatever the neighbour.
	queue.admit(interface(), neighbour(1), vec![0; 10]).expect("admitted");
	queue.admit(interface(), neighbour(2), vec![0; 10]).expect("admitted");
	let other_link = Interface::new(1, 1);
	queue.admit(other_link, neighbour(3), vec![0; 10]).expect("admitted");
	let torn = queue.fail_interface(interface(), FailureCause::InterfaceTorn);
	assert_eq!(torn.len(), 2);
	assert_eq!(queue.len(), 1, "the other link keeps its packet");
	assert_eq!(queue.bytes(), 10);
	assert_eq!(queue.resolution_failures(), 1, "a teardown is not a resolution failure");
}

#[test]
fn a_replaced_nic_is_a_different_interface_for_the_budgets() {
	let mut queue = PendingQueue::new();
	let old = Interface::new(0, 1);
	let new = Interface::new(0, 2);
	for _ in 0..4 {
		queue.admit(old, neighbour(1), vec![0; 10]).expect("admitted");
	}
	// The same address on the new generation has its own per-neighbour budget, and taking for the
	// new one does not take the old one's packets.
	queue.admit(new, neighbour(1), vec![0; 10]).expect("a new generation is a new neighbour");
	let taken = queue.take_for(new, neighbour(1));
	assert_eq!(taken.len(), 1);
	assert_eq!(queue.len(), 4);
}

#[test]
fn a_snapshot_reports_used_limit_and_refusals_for_every_resource() {
	let mut queue = PendingQueue::new();
	for _ in 0..4 {
		queue.admit(interface(), neighbour(1), vec![0; 100]).expect("admitted");
	}
	queue.admit(interface(), neighbour(1), vec![0; 100]).expect_err("refused");

	let usage: Vec<Usage> = RESOURCES
		.iter()
		.map(|resource| {
			let used = match resource {
				Resource::PendingPerInterface => queue.len() as u32,
				Resource::PendingBytes => queue.bytes(),
				_ => 0,
			};
			Usage { resource: *resource, used, limit: resource.limit(), refusals: queue.refusals().get(*resource) }
		})
		.collect();
	let snapshot = Snapshot { usage, resolution_failures: queue.resolution_failures(), quoted_errors_dropped: 0, icmp_errors_rate_limited: 0, resync_required: false };

	assert_eq!(snapshot.usage.len(), 17, "every resource has a line");
	let pending = snapshot.get(Resource::PendingPerInterface).expect("a line");
	assert_eq!(pending.used, 4);
	assert_eq!(pending.limit, 32);
	assert_eq!(snapshot.get(Resource::PendingBytes).expect("a line").used, 400);
	assert_eq!(snapshot.get(Resource::PendingPerNeighbour).expect("a line").refusals, 1);
	assert!(!snapshot.any_saturated());

	let full = Snapshot { usage: vec![Usage { resource: Resource::Rdnss, used: 4, limit: 4, refusals: 0 }], resolution_failures: 0, quoted_errors_dropped: 0, icmp_errors_rate_limited: 0, resync_required: false };
	assert!(full.any_saturated(), "one answer, not seventeen");
}
