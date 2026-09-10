// The property the whole seam rests on: whatever the loop is blocked on, it does not block past the
// next thing that has to happen, and a large due set is bounded without being starved.

use super::*;
use crate::ipv6::{Address, Interface, Prefix};
use crate::ipv6_events::Identity;

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

fn identity(low: u16) -> Identity {
	Identity::Address { interface: interface(), address: address(low) }
}

fn timer(kind: TimerKind, low: u16, deadline: u64) -> Timer {
	Timer { kind, identity: identity(low), deadline }
}

#[test]
fn the_aggregation_covers_every_kind_of_timer_this_layer_arms() {
	assert_eq!(TIMER_KINDS.len(), 11);
	let mut timers = Timers::new();
	for (index, kind) in TIMER_KINDS.iter().enumerate() {
		timers.set(Timer { kind: *kind, identity: identity(index as u16), deadline: 1000 - index as u64 });
	}
	assert_eq!(timers.len(), 11, "each kind armed one timer");
	// The two an earlier version of the contract left out of its "exhaustive" list.
	assert_eq!(timers.count_of(TimerKind::MldResponseDelay), 1);
	assert_eq!(timers.count_of(TimerKind::MldStateChangeRetransmit), 1);
	assert_eq!(timers.next_deadline(), Some(990), "the earliest of all of them, whatever kind it is");
}

#[test]
fn re_arming_moves_the_deadline_rather_than_adding_a_second_timer() {
	let mut timers = Timers::new();
	timers.set(timer(TimerKind::RouterLifetime, 1, 500));
	timers.set(timer(TimerKind::RouterLifetime, 1, 900));
	assert_eq!(timers.len(), 1, "a refreshed lifetime is one timer");
	assert_eq!(timers.next_deadline(), Some(900));

	// A different kind about the same identity is a different timer.
	timers.set(timer(TimerKind::AddressLifetime, 1, 400));
	assert_eq!(timers.len(), 2);
	assert_eq!(timers.next_deadline(), Some(400));
}

#[test]
fn clearing_takes_one_timer_an_identity_or_a_whole_interface() {
	let mut timers = Timers::new();
	timers.set(timer(TimerKind::DuplicateAddress, 1, 100));
	timers.set(timer(TimerKind::AddressLifetime, 1, 200));
	timers.set(timer(TimerKind::AddressLifetime, 2, 300));
	assert!(timers.clear(TimerKind::DuplicateAddress, identity(1)));
	assert!(!timers.clear(TimerKind::DuplicateAddress, identity(1)), "clearing twice is not an error and not a second removal");
	assert_eq!(timers.len(), 2);

	assert_eq!(timers.clear_identity(identity(1)), 1, "everything about one address goes together");
	assert_eq!(timers.len(), 1);

	let other = Interface::new(1, 1);
	timers.set(Timer { kind: TimerKind::PrefixLifetime, identity: Identity::Prefix { interface: other, prefix: Prefix::link_local() }, deadline: 50 });
	assert_eq!(timers.clear_interface(interface()), 1, "teardown takes only its own link");
	assert_eq!(timers.len(), 1);
	assert_eq!(timers.next_deadline(), Some(50));
}

#[test]
fn the_wait_is_zero_when_something_is_already_due() {
	let mut timers = Timers::new();
	assert_eq!(timers.next_deadline(), None, "nothing armed is no deadline at all");
	assert_eq!(timers.wait_from(1000), None);
	assert!(!timers.any_due(1000));

	timers.set(timer(TimerKind::NeighbourRetry, 1, 1500));
	assert_eq!(timers.wait_from(1000), Some(500));
	assert!(!timers.any_due(1000));

	timers.set(timer(TimerKind::Unreachability, 2, 900));
	assert_eq!(timers.wait_from(1000), Some(0), "a loop must not wait at all when work is due");
	assert!(timers.any_due(1000));
}

#[test]
fn a_due_timer_comes_back_disarmed() {
	let mut timers = Timers::new();
	timers.set(timer(TimerKind::NeighbourRetry, 1, 100));
	let fired = timers.due(100);
	assert_eq!(fired.len(), 1);
	assert_eq!(fired[0].kind, TimerKind::NeighbourRetry);
	assert!(timers.is_empty(), "a retransmission that nobody re-arms stops, rather than firing forever");
	assert_eq!(timers.due(100).len(), 0);
}

#[test]
fn only_due_timers_fire_and_the_rest_stay_armed() {
	let mut timers = Timers::new();
	timers.set(timer(TimerKind::DuplicateAddress, 1, 100));
	timers.set(timer(TimerKind::RouterSolicitation, 2, 200));
	timers.set(timer(TimerKind::PathMtuExpiry, 3, 300));
	let fired = timers.due(200);
	assert_eq!(fired.len(), 2);
	assert_eq!(timers.len(), 1);
	assert_eq!(timers.next_deadline(), Some(300));
}

#[test]
fn a_large_due_set_is_bounded_per_iteration_and_none_of_it_is_starved() {
	let mut timers = Timers::new();
	for index in 0..100u16 {
		timers.set(timer(TimerKind::Unreachability, index, 10));
	}
	assert_eq!(timers.len(), 100);

	let mut seen = alloc::vec::Vec::new();
	let mut iterations = 0;
	while !timers.is_empty() {
		let fired = timers.due(10);
		assert!(fired.len() <= 16, "no iteration runs more than the bound");
		assert!(!fired.is_empty(), "and a due set always makes progress");
		for entry in fired {
			seen.push(entry.identity);
		}
		iterations += 1;
		assert!(iterations < 20, "a hundred timers in sixteens is seven passes, not an unbounded loop");
	}
	assert_eq!(seen.len(), 100, "every one of them ran");
	// And each ran exactly once: no identity appears twice.
	for index in 0..100u16 {
		assert_eq!(seen.iter().filter(|held| **held == identity(index)).count(), 1, "identity {index}");
	}

	// The aggregated deadline stays immediate while due work remains, which is what keeps the loop
	// coming back to it rather than blocking on the next frame.
	for index in 0..100u16 {
		timers.set(timer(TimerKind::Unreachability, index, 10));
	}
	timers.due(10);
	assert_eq!(timers.wait_from(10), Some(0), "still due, so still no wait");
}

#[test]
fn due_work_is_interleaved_rather_than_run_to_completion() {
	// Seventeen due and one not: the pass takes sixteen, leaves the seventeenth due, and does not
	// touch the future one.
	let mut timers = Timers::new();
	for index in 0..17u16 {
		timers.set(timer(TimerKind::MldStateChangeRetransmit, index, 5));
	}
	timers.set(timer(TimerKind::MldResponseDelay, 999, 5000));
	let fired = timers.due(5);
	assert_eq!(fired.len(), 16);
	assert!(fired.iter().all(|entry| entry.kind == TimerKind::MldStateChangeRetransmit));
	assert_eq!(timers.len(), 2);
	assert!(timers.any_due(5), "the seventeenth is still due");
	let rest = timers.due(5);
	assert_eq!(rest.len(), 1);
	assert_eq!(timers.len(), 1, "the future timer was never in the pass");
	assert_eq!(timers.next_deadline(), Some(5000));
}
