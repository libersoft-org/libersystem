#![no_std]

// WHERE A BINDING IS, AND WHY IT GOT THERE.
//
// A state and a reason are not alternatives - "quarantined" and "the teardown did not confirm" are
// two halves of one answer - so the record carries both, and the transitions between states are a
// TABLE rather than whatever each call site decided. A graph with a hole in it is one that gets
// closed by whoever hits the hole first.
//
// DEFINED HERE rather than where it is finally rendered, because five milestones would otherwise
// each invent their own numbers and strings for the same thing.

use driver_protocol::DriverFailureCode;

// The states a device node's binding can be in.
//
// THERE IS NO `Removed`. Nothing rescans, so no removal event exists to enter it, and a state
// nothing can produce is a state every reader has to reason about for nothing.
//
// Two more are named by later milestones and are added by the milestone that first PRODUCES one, for
// the same reason: `DependencyPending`, where a declared requirement first has something to wait for,
// and `Disabled`, where an operator first has a way to set it. They belong to this enum wherever they
// are added - one enum, in one file.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BindingState {
	// Where a node starts, and what invites a bind. A FAILED bind must never land here: it would be
	// a bind that immediately happens again with nothing recorded about why the last one did not
	// work.
	Unbound,
	// The transaction is open: a driver has been selected and resources are being assembled.
	Binding,
	// The driver said `READY` with the current generation.
	Online,
	// There is a device to quieten. The only way out is through the teardown.
	Stopping,
	// Waiting to try again, with attempts left.
	Backoff,
	// Not bound, and not going to be without something changing. Carries the cause.
	Failed,
	// The teardown could not be CONFIRMED. Terminal for the boot: the device's frames, vectors and
	// grants stay charged and out of circulation, because the alternative is handing back memory a
	// device may still be writing to.
	Quarantined,
}

impl BindingState {
	// A name for the log and for whatever renders this later. Deliberately not `Debug`: these
	// strings are read by people and one of them is a boot line.
	pub fn name(self) -> &'static [u8] {
		match self {
			BindingState::Unbound => b"unbound",
			BindingState::Binding => b"binding",
			BindingState::Online => b"online",
			BindingState::Stopping => b"stopping",
			BindingState::Backoff => b"backoff",
			BindingState::Failed => b"failed",
			BindingState::Quarantined => b"quarantined",
		}
	}

	// THE TABLE, ENFORCED RATHER THAN IMPLIED. Anything not here is refused and logged as a
	// transition that was ATTEMPTED, because a state machine that silently ignores an illegal event
	// is one nobody can debug.
	//
	// | from | to | on |
	// | `Unbound` | `Binding` | a driver is selected and the transaction opens |
	// | `Binding` | `Online` | `READY` arrives with the current generation |
	// | `Binding` | `Backoff` | the transaction failed BEFORE the claim was taken, attempts left |
	// | `Binding` | `Failed` | the same, for a reason retrying cannot change OR the budget is spent |
	// | `Binding` | `Stopping` | the transaction failed AFTER the claim was taken |
	// | `Online` | `Stopping` | the driver exited |
	// | `Stopping` | `Backoff` | teardown confirmed, attempts left |
	// | `Stopping` | `Failed` | teardown confirmed, and retrying cannot help OR the budget is spent |
	// | `Stopping` | `Quarantined` | teardown NOT confirmed - the device may still be live |
	// | `Backoff` | `Binding` | the delay expired |
	// | `Backoff` | `Failed` | the absolute TIME budget expired |
	// | `Failed` | `Binding` | an operator retry |
	// | `Quarantined` | - | terminal for the boot |
	pub fn may_move_to(self, to: BindingState) -> bool {
		matches!((self, to), (BindingState::Unbound, BindingState::Binding) | (BindingState::Binding, BindingState::Online) | (BindingState::Binding, BindingState::Backoff) | (BindingState::Binding, BindingState::Failed) | (BindingState::Binding, BindingState::Stopping) | (BindingState::Online, BindingState::Stopping) | (BindingState::Stopping, BindingState::Backoff) | (BindingState::Stopping, BindingState::Failed) | (BindingState::Stopping, BindingState::Quarantined) | (BindingState::Backoff, BindingState::Binding) | (BindingState::Backoff, BindingState::Failed) | (BindingState::Failed, BindingState::Binding))
	}
}

// WHY A BINDING IS NOT UP.
//
// EVERY VARIANT ANSWERS WHETHER IT IS RETRYABLE, in one place. A flag some variants leave unset is a
// flag each call site guesses at, and the table's `Binding -> Backoff` versus `Binding -> Failed`
// reads this rather than deciding again.
//
// The driver-reported one CARRIES the driver's code rather than flattening it away: "the driver said
// it failed" without saying what it said is a cause that explains nothing - and the retryability
// comes from the driver's own closed set, which is the only party that can honestly answer it about
// itself.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FailureCause {
	// The registry names a driver the image does not contain.
	DriverMissing,
	// Its ELF note declares a protocol version this build does not implement.
	ProtocolMismatch,
	// Somebody else holds the device. NOT retryable: waiting changes nothing this milestone
	// controls.
	ClaimRefused,
	// The driver's DMA policy demands translation on a machine that is not enforcing it.
	IommuRequired,
	// A quota or an allocation refused.
	ResourceExhausted,
	// The process could not be started. RETRYABLE: usually a transient shortage.
	SpawnFailed,
	// The driver was sent `BIND` and never reached a terminal frame inside the window.
	HandshakeTimeout,
	// It exited without saying anything.
	DriverExited,
	// It said `FAILED`, and this is what it said.
	DriverReported(DriverFailureCode),
	// The release did not confirm. NOT IN EITHER COLUMN: it never reaches the question, because it
	// ends at `Quarantined`.
	TeardownUnconfirmed,
}

impl FailureCause {
	pub fn retryable(self) -> bool {
		match self {
			FailureCause::HandshakeTimeout | FailureCause::DriverExited | FailureCause::SpawnFailed => true,
			FailureCause::DriverReported(code) => code.retryable(),
			FailureCause::DriverMissing | FailureCause::ProtocolMismatch | FailureCause::ClaimRefused | FailureCause::IommuRequired | FailureCause::ResourceExhausted => false,
			// Asked and answered `false` so the match is total, but the state machine never gets
			// here: this cause ends at `Quarantined`, which has no edge out.
			FailureCause::TeardownUnconfirmed => false,
		}
	}

	pub fn name(self) -> &'static [u8] {
		match self {
			FailureCause::DriverMissing => b"driver-missing",
			FailureCause::ProtocolMismatch => b"protocol-mismatch",
			FailureCause::ClaimRefused => b"claim-refused",
			FailureCause::IommuRequired => b"iommu-required",
			FailureCause::ResourceExhausted => b"resource-exhausted",
			FailureCause::SpawnFailed => b"spawn-failed",
			FailureCause::HandshakeTimeout => b"handshake-timeout",
			FailureCause::DriverExited => b"driver-exited",
			FailureCause::DriverReported(_) => b"driver-reported",
			FailureCause::TeardownUnconfirmed => b"teardown-unconfirmed",
		}
	}
}

// One device node's binding record: where it is, why, and which binding this is.
pub struct BindingRecord {
	pub state: BindingState,
	pub failure: Option<FailureCause>,
	// P02M0098's claim generation for the CURRENT binding, or 0 for a node that has never been
	// claimed. Every event carries the generation it is about, and one that does not match is
	// dropped - which is what stops a dying driver from touching its replacement's state.
	pub generation: u64,
	// Automatic attempts spent on THIS incident. An operator retry is not one of them.
	pub attempts: u32,
}

impl BindingRecord {
	pub fn new() -> Self {
		Self { state: BindingState::Unbound, failure: None, generation: 0, attempts: 0 }
	}

	// Move, if the table allows it. Answers false for an illegal transition, which the caller LOGS -
	// refusing quietly would be the same defect as ignoring it.
	//
	// A DUPLICATE EVENT CHANGES NOTHING, and that falls out of the table rather than needing a rule:
	// no state has an edge to itself, so a second arrival of the event that already moved the node
	// is refused like any other illegal transition.
	pub fn move_to(&mut self, to: BindingState, failure: Option<FailureCause>) -> bool {
		if !self.state.may_move_to(to) {
			return false;
		}
		self.state = to;
		// The cause is kept while it explains the state and cleared when it stops doing so: a node
		// that is `Online` carrying the reason its LAST attempt failed reads as a node that is
		// broken and running.
		self.failure = match to {
			BindingState::Online | BindingState::Binding => None,
			_ => failure.or(self.failure),
		};
		true
	}
}

#[cfg(test)]
mod tests;
