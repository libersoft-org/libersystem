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
// ONE MORE IS STILL TO COME, added by the milestone that first PRODUCES it: `Disabled`, where an
// operator first has a way to set it. `DependencyPending` arrived when a declared requirement first
// had something to wait for. They belong to this enum wherever they are added - one enum, in one
// file.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BindingState {
	// Where a node starts, and what invites a bind. A FAILED bind must never land here: it would be
	// a bind that immediately happens again with nothing recorded about why the last one did not
	// work.
	Unbound,
	// A declared requirement is not published yet. A STATE, NOT A FAILURE: a machine without a NIC
	// is a machine, not a broken image, and a node here is waiting rather than broken.
	//
	// There is no edge back to `Unbound`. A node waiting for a provider that then goes away is
	// waiting HARDER, not waiting less - and `Unbound` invites an immediate bind that would fail for
	// the reason the node is already recording.
	DependencyPending,
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
			BindingState::DependencyPending => b"waiting for a dependency",
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
		matches!(
			(self, to),
			(BindingState::Unbound, BindingState::Binding)
				| (BindingState::Binding, BindingState::Online)
				| (BindingState::Binding, BindingState::Backoff)
				| (BindingState::Binding, BindingState::Failed)
				| (BindingState::Binding, BindingState::Stopping)
				| (BindingState::Online, BindingState::Stopping)
				| (BindingState::Stopping, BindingState::Backoff)
				| (BindingState::Stopping, BindingState::Failed)
				| (BindingState::Stopping, BindingState::Quarantined)
				| (BindingState::Backoff, BindingState::Binding)
				| (BindingState::Backoff, BindingState::Failed)
				| (BindingState::Failed, BindingState::Binding)
				// The dependency edges. A node enters this state from wherever it learns a declared
				// requirement is missing, and leaves it only for a bind - when EVERY requirement is
				// published, not the first, which would start a bind that fails on the second.
				| (BindingState::Unbound, BindingState::DependencyPending)
				| (BindingState::DependencyPending, BindingState::Binding)
				| (BindingState::Binding, BindingState::DependencyPending)
				| (BindingState::Backoff, BindingState::DependencyPending)
				| (BindingState::Stopping, BindingState::DependencyPending)
				| (BindingState::Failed, BindingState::DependencyPending)
		)
	}
}

// WHY A NODE IS IN `Stopping`, which is what decides where it goes next.
//
// P02M0162's table sends a CONFIRMED teardown on to `Backoff` and then back to `Binding` - which is
// right for a driver that died and exactly wrong for one that was asked to stop: the operator stops
// it and it starts again. The intent is what `Stopping` resolves against, and there are four.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum StopIntent {
	// It crashed, exited, or stopped answering. The ordinary path, and the default.
	#[default]
	Fault,
	// A provider it declared in `requires` went away.
	DependencyLost,
	// An operator disabled it. Where it lands is P02M0166's `Disabled`.
	OperatorDisable,
	// The machine is going down. No further state is entered at all - the manager is going away, so
	// there is no next binding to describe.
	Shutdown,
}

impl StopIntent {
	// Where a CONFIRMED teardown under this intent lands, or None for one that describes no next
	// state because nothing will read it.
	//
	// AN UNCONFIRMED TEARDOWN IS NOT HERE, and that is the point: it ends at `Quarantined` whatever
	// the intent was, because what is unknown is whether the device is still live and no intent
	// changes that.
	pub fn confirmed_lands_at(self, attempts_left: bool) -> Option<BindingState> {
		match self {
			StopIntent::Fault => Some(if attempts_left { BindingState::Backoff } else { BindingState::Failed }),
			StopIntent::DependencyLost => Some(BindingState::DependencyPending),
			// P02M0166 adds `Disabled` and this arm with it; until then an operator disable lands
			// where a spent budget does, which is terminal and honest rather than a state that does
			// not exist yet.
			StopIntent::OperatorDisable => Some(BindingState::Failed),
			StopIntent::Shutdown => None,
		}
	}

	pub fn name(self) -> &'static [u8] {
		match self {
			StopIntent::Fault => b"a fault",
			StopIntent::DependencyLost => b"a dependency it declared went away",
			StopIntent::OperatorDisable => b"an operator disabled it",
			StopIntent::Shutdown => b"the machine is going down",
		}
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

// ------------------------------------------------- one ordered queue per device node

// How many events one node may hold before a new one is refused.
//
// A node's events are the frames of one handshake plus its terminal ones, so the bound is the
// protocol's own: at most `MAX_INITIAL_OFFERS` offers, one terminal frame, one exit, one claim
// answer, and a timeout - and a driver that sends more is a driver whose offers are already being
// refused past the bound.
pub const MAX_NODE_EVENTS: usize = driver_protocol::MAX_INITIAL_OFFERS + 4;

// WHAT HAPPENED TO A BINDING, carrying the generation it is about.
//
// The generation is ON the event rather than checked where the frame arrived, because an event may
// be read long after the frame that produced it: the exit of a driver arrives while its binding is
// being torn down, and the next binding's `READY` arrives on the same device afterwards.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BindingEvent {
	// The driver said it is up.
	Ready { generation: u64 },
	// It said it failed, and this is what it said.
	Failed { generation: u64, code: DriverFailureCode },
	// It offered a provider. The handle itself is held elsewhere, unpublished until the bind commits.
	Offered { generation: u64 },
	// Its process ended - however it ended.
	Exited { generation: u64 },
	// Its channel closed with nothing terminal on it.
	Closed { generation: u64 },
	// This attempt spent its share of the boot window.
	TimedOut { generation: u64 },
	// A provider this driver published under `token` is going away. NOT terminal: the driver stays
	// bound and its other publications stay published.
	Withdrawn { generation: u64, token: u16 },
	// It answered a `PING` with this sequence. Whether that COUNTS is not this event's business:
	// an answer echoing a number nobody asked with is still an answer that arrived, and the reader
	// is what decides it does not reset the watchdog.
	Ponged { generation: u64, sequence: u32 },
	// Its control path stopped answering inside the deadline its registry entry declared.
	Wedged { generation: u64 },
	// It answered a `STOP`: everything it accepted is finished or abandoned and its device is quiet.
	// A PLANNED stop completing, which is a different fact from a channel that simply closed.
	Stopped { generation: u64 },
}

impl BindingEvent {
	pub fn generation(self) -> u64 {
		match self {
			BindingEvent::Ready { generation } | BindingEvent::Failed { generation, .. } | BindingEvent::Offered { generation } | BindingEvent::Exited { generation } | BindingEvent::Closed { generation } | BindingEvent::TimedOut { generation } | BindingEvent::Withdrawn { generation, .. } | BindingEvent::Ponged { generation, .. } | BindingEvent::Wedged { generation } | BindingEvent::Stopped { generation } => generation,
		}
	}
}

// A DEVICE NODE'S QUEUE, NOT A BINDING'S.
//
// A binding is not the thing that outlives its own events. A queue owned by a binding has nowhere
// to put the events on either side of it, and two consecutive bindings on one device would each
// hold a queue with the interesting moment falling between them.
//
// HERE RATHER THAN IN THE MANAGER, for the reason `BindingState` is here: a ring buffer with a
// generation filter inside a `no_std` binary is one nobody can drive on a host, and "an exit racing
// a restart" is exactly the case that has to be driven rather than reasoned about.
pub struct BindingQueue {
	events: [Option<BindingEvent>; MAX_NODE_EVENTS],
	head: usize,
	count: usize,
}

impl Default for BindingQueue {
	fn default() -> Self {
		Self::new()
	}
}

impl BindingQueue {
	pub const fn new() -> Self {
		Self { events: [None; MAX_NODE_EVENTS], head: 0, count: 0 }
	}

	pub fn len(&self) -> usize {
		self.count
	}

	pub fn is_empty(&self) -> bool {
		self.count == 0
	}

	// Append one event. A full queue REFUSES the new one rather than dropping the oldest: the oldest
	// is the one whose ordering matters - an exit before a `READY` is a different story from an exit
	// after it - and a queue that silently forgets its front is a queue that reorders.
	pub fn push(&mut self, event: BindingEvent) -> bool {
		if self.count == MAX_NODE_EVENTS {
			return false;
		}
		let at = (self.head + self.count) % MAX_NODE_EVENTS;
		self.events[at] = Some(event);
		self.count += 1;
		true
	}

	// Take the oldest event that is about `generation`, dropping anything that is not.
	//
	// `generation` of 0 means the node holds no binding, and then nothing in the queue can be about
	// anything - so the queue drains and answers None. That is the state a node is in between a
	// teardown and the next attempt, and it is where a dying driver's last events land.
	//
	// STALE EVENTS ARE DROPPED HERE AND NOWHERE ELSE, so every reader gets the same answer to "is
	// this about the binding I am holding".
	pub fn pop(&mut self, generation: u64) -> Option<BindingEvent> {
		while self.count > 0 {
			let event = self.events[self.head].take();
			self.head = (self.head + 1) % MAX_NODE_EVENTS;
			self.count -= 1;
			let event = event?;
			if generation == 0 || event.generation() != generation {
				continue;
			}
			return Some(event);
		}
		None
	}
}

// ------------------------------------------------------- what a binding IS about

// A FUNCTION'S CANONICAL IDENTITY, paired with the generation of the binding it names.
//
// The BDF is the identity: the ABI has no PCI domain and all three implementations are
// single-segment, so bus/device/function names a function on this machine and nothing else does.
//
// THE ROW NUMBER IS NOT PART OF IT. P02M0098's `ClaimKey { device_index, generation }` addresses the
// same binding by the kernel's row number, and two identities for one thing with no stated way
// between them is how a revocation ends up unable to find what it is revoking. The answer is a
// LOOKUP, not a wider name: a node holds its row number as its own field, absent for a function the
// kernel's resolver never gave a row. A name that grows a field for every table that wants to find
// it is not a name.
//
// THE GENERATION IS LOAD-BEARING FROM THE FIRST CRASH-RESTART, not from some future hotplug. A node
// rebound after its driver died keeps its BDF and takes a NEW claim, and P02M0098 gives that claim a
// new generation - so a message stamped with the previous one is refused by arithmetic rather than
// by anyone remembering to check.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct BindingId {
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
	pub generation: u64,
}

impl BindingId {
	pub const fn new(bus: u8, dev: u8, func: u8, generation: u64) -> Self {
		Self { bus, dev, func, generation }
	}

	// Whether this names the same FUNCTION as `other`, whatever binding either is about.
	//
	// The question a rebind asks: "is this the device I was driving", which is about the location
	// and not about the binding. `==` answers the other question - "is this the same binding" - and
	// the two must not be one operator, because a rebind that compared whole identities would
	// conclude a device it just rebound is a different device.
	pub fn same_function(self, other: BindingId) -> bool {
		self.bus == other.bus && self.dev == other.dev && self.func == other.func
	}

	// The same function, one binding later.
	pub fn rebound(self, generation: u64) -> Self {
		Self { generation, ..self }
	}
}

// ------------------------------------------------------- what a driver publishes

// A PROVIDER'S IDENTITY, WHICH THE MANAGER ASSIGNS AND A DRIVER NEVER CHOOSES.
//
// Handing one channel to two subscribers is not two connections - it is two consumers competing
// over one reply queue - so a connection is minted per consumer and the catalogue entry is what they
// are minted from. A driver that could choose its own identity could advertise itself as the system
// disk, which is why the manager mints these and the driver only ever sees its own token.
//
// `slot` is the manager's index for this publication and `generation` distinguishes reuse of the
// slot, so an id that named a withdrawn provider cannot silently name its replacement.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct ProviderId {
	pub binding: BindingId,
	pub slot: u16,
	pub generation: u32,
}

impl ProviderId {
	pub const fn new(binding: BindingId, slot: u16, generation: u32) -> Self {
		Self { binding, slot, generation }
	}
}
