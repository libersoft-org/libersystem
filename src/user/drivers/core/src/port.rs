// What a port-status change means, as pure decisions, tested on the host through the crate's seam.
//
// THE DEBT THAT LIVES HERE (DRV-008). The driver has one event ring and several places that wait on
// it: a control transfer, a command completion, the service loop. The two SYNCHRONOUS waits took
// every event off the ring and dropped the ones they were not waiting for - so a device plugged in
// while a block read was in flight raised its port-status event into a wait that discarded it, and
// nothing reconciled the ports afterwards. The device was invisible until something unrelated woke
// the loop, which on an idle machine is never.
//
// AND THE ANSWER IS NOT A QUEUE OF EVENTS. A port-status event carries no state worth keeping: the
// port's own register says what is true NOW, so what has to survive is the single fact that SOMETHING
// changed. A flag cannot grow under a storm of events, and reconciling from the registers produces
// exactly one attach and one detach for a connect and a disconnect delivered in the same window -
// which a queue of events would produce twice if the same port appeared in it twice.

// What to do about one root port, from its current connection state and what the driver has recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortAction {
	// Connected and nothing recorded: a fresh attach to enumerate.
	Attach,
	// Recorded and no longer connected: a detach to tear down.
	Detach,
	// Connected and known, or empty and unknown: nothing to do.
	Settled,
}

pub fn port_action(connected: bool, known: bool) -> PortAction {
	match (connected, known) {
		(true, false) => PortAction::Attach,
		(false, true) => PortAction::Detach,
		_ => PortAction::Settled,
	}
}

// THE PENDING CHANGE, as one bit.
//
// BOUNDED BY CONSTRUCTION. However many port-status events arrive between two reconciles - a device
// that bounces, a hub that powers a row of ports, a cable with a bad contact - this is one flag, and
// the reconcile that follows reads every port's register once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortSignal {
	pending: bool,
}

impl PortSignal {
	pub const fn new() -> PortSignal {
		PortSignal { pending: false }
	}

	// A port-status event arrived. Called from the ONE place every event passes through, so a
	// synchronous wait that is not interested in it still cannot lose it.
	pub fn record(&mut self) {
		self.pending = true;
	}

	// Take the pending change, leaving none.
	pub fn take(&mut self) -> bool {
		core::mem::take(&mut self.pending)
	}

	pub fn is_pending(&self) -> bool {
		self.pending
	}
}

#[cfg(test)]
mod tests;
