// DRV-008's negatives: the events a synchronous wait used to eat, and the storm that must not grow a
// queue.
use super::{PortAction, PortSignal, port_action};

#[test]
// A CONNECT AND A DISCONNECT IN THE SAME STATUS WINDOW PRODUCE EXACTLY ONE OF EACH. The reconcile
// reads every port's own register rather than replaying a list of events, so two events on one port
// are one answer and one event on two ports is two.
fn a_window_of_changes_produces_one_action_per_port() {
	// port 1: a device arrived; port 2: the one that was there has gone; port 3: unchanged.
	let ports = [(true, false), (false, true), (true, true), (false, false)];
	let actions: std::vec::Vec<PortAction> = ports.iter().map(|&(connected, known)| port_action(connected, known)).collect();
	assert_eq!(actions, std::vec![PortAction::Attach, PortAction::Detach, PortAction::Settled, PortAction::Settled]);
	assert_eq!(actions.iter().filter(|action| **action == PortAction::Attach).count(), 1);
	assert_eq!(actions.iter().filter(|action| **action == PortAction::Detach).count(), 1);
}

#[test]
// A SYNCHRONOUS WAIT USED TO EAT THE EVENT AND RETURN. The fact that something changed has to
// survive a wait that was not interested in it, or a device plugged in during a block read is
// invisible until something unrelated wakes the loop - which on an idle machine is never.
fn a_change_seen_by_a_wait_survives_until_the_loop_reads_it() {
	let mut signal = PortSignal::new();
	assert!(!signal.is_pending());
	// The control transfer's wait sees it and is not interested.
	signal.record();
	assert!(signal.is_pending(), "the wait may ignore the event; it may not lose it");
	// The service loop reads it once, and it is gone.
	assert!(signal.take());
	assert!(!signal.take(), "one change is reconciled once");
}

#[test]
// A STORM IS BOUNDED RATHER THAN GROWING A QUEUE. A bouncing cable can raise hundreds of port-status
// events between two reconciles; what survives is one bit, and the reconcile that follows reads every
// port once.
fn a_storm_of_changes_is_one_pending_change() {
	let mut signal = PortSignal::new();
	for _ in 0..10_000 {
		signal.record();
	}
	assert!(signal.take());
	assert!(!signal.is_pending());
	assert_eq!(core::mem::size_of::<PortSignal>(), 1, "the pending state is one byte and cannot grow");
}
