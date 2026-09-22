//! THE HCI TRANSPORT'S DECISIONS, which are about bounds and credits and nothing about radio.
//!
//! A Bluetooth controller is reached over a two-way packet pipe, and everything that makes the pipe
//! safe to hold is arithmetic: which packet kinds travel which way, how long each may be, how many
//! may be outstanding before the controller has answered, and which session a packet belongs to.
//! None of it needs a radio to be judged, and all of it is what a hostile or broken controller
//! reaches first.
//!
//! THE CEILINGS ARE THE SPECIFICATION'S, HEADERS INCLUDED, and a provider may advertise smaller
//! ones because a controller reports its own buffer sizes. A host bound by the specification's
//! ceiling alone hands a controller a packet it answers with a hardware error rather than with data,
//! so the bound that applies is the SMALLER of the two - which is a decision and is made here.
//!
//! A TRANSPORT SEND IS NOT A COMMAND COMPLETION. This is the rule the whole credit model exists for:
//! a host that treated the enqueue as the answer would issue its next command against credits the
//! controller has not returned, and the controller drops it - which from the host looks like a
//! command that was answered and then forgotten. `Credits` therefore moves on the controller's
//! events and never on a successful send.

/// Which of the four packet types this is, as the transport interface numbers them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	Command = 1,
	Event = 2,
	Acl = 3,
	Iso = 4,
}

impl Kind {
	/// The kind a wire number names, or `None` for one this vocabulary does not have.
	pub const fn from_wire(value: u16) -> Option<Kind> {
		match value {
			1 => Some(Kind::Command),
			2 => Some(Kind::Event),
			3 => Some(Kind::Acl),
			4 => Some(Kind::Iso),
			_ => None,
		}
	}

	/// Whether a HOST may send this kind. A host sending an event has confused its directions, which
	/// is a different mistake from being early and is refused differently.
	pub const fn host_may_send(self) -> bool {
		matches!(self, Kind::Command | Kind::Acl | Kind::Iso)
	}

	/// Whether a CONTROLLER may send this kind.
	pub const fn controller_may_send(self) -> bool {
		matches!(self, Kind::Event | Kind::Acl | Kind::Iso)
	}
}

/// The specification's ceilings, headers included.
pub const SPEC_MAX_COMMAND: u32 = 258;
pub const SPEC_MAX_EVENT: u32 = 257;
pub const SPEC_MAX_ACL: u32 = 1028;
pub const SPEC_MAX_ISO: u32 = 4100;

/// The largest packet of any kind, which is what a buffer holding one must be.
pub const SPEC_MAX_PACKET: u32 = SPEC_MAX_ISO;

/// Why a packet was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// A kind this vocabulary does not have.
	UnknownKind(u16),
	/// A kind this provider does not carry - ISO, on the controllers that have none.
	Unsupported(Kind),
	/// A kind that travels the other way.
	WrongDirection(Kind),
	/// Longer than the bound that applies, which is the smaller of the advertised and the
	/// specification's.
	TooLong { len: u32, bound: u32 },
	/// A packet with no bytes at all, which is not a packet: every kind has a header.
	Empty,
	/// The controller has answered none of the commands already sent.
	NoCommandCredit,
	/// The controller has no data buffer free.
	NoAclCredit,
	/// The transport's own queue is full, which is a different bound from the controller's.
	QueueFull,
}

/// What a provider said it will carry, narrowed to what the specification allows.
///
/// EVERY CEILING IS THE SMALLER OF THE TWO, and a provider advertising MORE than the specification
/// is not believed: a number larger than the format can express is a number that came from
/// somewhere other than the controller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
	pub iso: bool,
	pub command: u32,
	pub event: u32,
	pub acl: u32,
	pub iso_bytes: u32,
}

impl Limits {
	/// Narrow an advertisement. A ceiling of zero means the provider carries that kind with no room
	/// at all, which is refused as `Unsupported` at the ask rather than accepted as a bound nothing
	/// can satisfy.
	pub fn of(iso: bool, command: u32, event: u32, acl: u32, iso_bytes: u32) -> Limits {
		Limits { iso, command: command.min(SPEC_MAX_COMMAND), event: event.min(SPEC_MAX_EVENT), acl: acl.min(SPEC_MAX_ACL), iso_bytes: iso_bytes.min(SPEC_MAX_ISO) }
	}

	/// The ceiling that applies to this kind.
	pub const fn ceiling(&self, kind: Kind) -> u32 {
		match kind {
			Kind::Command => self.command,
			Kind::Event => self.event,
			Kind::Acl => self.acl,
			Kind::Iso => self.iso_bytes,
		}
	}

	/// Whether this provider carries the kind at all.
	pub const fn carries(&self, kind: Kind) -> bool {
		match kind {
			Kind::Iso => self.iso && self.iso_bytes > 0,
			Kind::Command => self.command > 0,
			Kind::Event => self.event > 0,
			Kind::Acl => self.acl > 0,
		}
	}
}

/// Whether a packet the HOST wants to send may be enqueued, on kind, direction and length alone.
///
/// THE CREDITS ARE A SEPARATE QUESTION and are asked separately: a packet may be well formed and
/// still have to wait, and a caller that could not tell the two apart would drop a good packet
/// because the controller was busy.
pub fn check_outbound(limits: &Limits, kind: u16, len: u32) -> Result<Kind, Refusal> {
	let Some(kind) = Kind::from_wire(kind) else { return Err(Refusal::UnknownKind(kind)) };
	if !kind.host_may_send() {
		return Err(Refusal::WrongDirection(kind));
	}
	if !limits.carries(kind) {
		return Err(Refusal::Unsupported(kind));
	}
	if len == 0 {
		return Err(Refusal::Empty);
	}
	let bound = limits.ceiling(kind);
	if len > bound {
		return Err(Refusal::TooLong { len, bound });
	}
	Ok(kind)
}

/// The same for a packet arriving FROM the controller.
///
/// A CONTROLLER IS INPUT. It chooses the kind and the length, so a packet longer than what it itself
/// advertised is refused here rather than read - the buffer it would be read into is the one sized
/// by that advertisement.
pub fn check_inbound(limits: &Limits, kind: u16, len: u32) -> Result<Kind, Refusal> {
	let Some(kind) = Kind::from_wire(kind) else { return Err(Refusal::UnknownKind(kind)) };
	if !kind.controller_may_send() {
		return Err(Refusal::WrongDirection(kind));
	}
	if !limits.carries(kind) {
		return Err(Refusal::Unsupported(kind));
	}
	if len == 0 {
		return Err(Refusal::Empty);
	}
	let bound = limits.ceiling(kind);
	if len > bound {
		return Err(Refusal::TooLong { len, bound });
	}
	Ok(kind)
}

/// What the controller and the transport will take right now.
///
/// THREE COUNTERS AND THEY ARE NOT THE SAME BOUND. Command credits are the controller's and come
/// back on its own completion events; ACL credits are the controller's buffers and come back on its
/// number-of-completed-packets events; the queue is the TRANSPORT'S and comes back when the wire
/// drains. A host that folded them together would stall on the wrong one and never learn which.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Credits {
	command: u32,
	command_outstanding: u32,
	acl: u32,
	acl_outstanding: u32,
	queue: u32,
	queued: u32,
}

impl Credits {
	pub const fn new(command: u32, acl: u32, queue: u32) -> Credits {
		Credits { command, command_outstanding: 0, acl, acl_outstanding: 0, queue, queued: 0 }
	}

	/// Take the room one outbound packet needs, or say which bound stopped it.
	///
	/// NOTHING IS CHARGED BY A REFUSED SEND. Every counter is checked before any is written, because
	/// a partial charge is a credit leak that shows up much later as a transport that refuses
	/// packets it has room for.
	pub fn take(&mut self, kind: Kind) -> Result<(), Refusal> {
		if self.queued >= self.queue {
			return Err(Refusal::QueueFull);
		}
		match kind {
			Kind::Command => {
				if self.command_outstanding >= self.command {
					return Err(Refusal::NoCommandCredit);
				}
				self.command_outstanding += 1;
			}
			Kind::Acl | Kind::Iso => {
				if self.acl_outstanding >= self.acl {
					return Err(Refusal::NoAclCredit);
				}
				self.acl_outstanding += 1;
			}
			// A host does not send events; `check_outbound` refuses one before this is reached, and
			// charging nothing is the right answer for a caller that skipped it.
			Kind::Event => return Err(Refusal::WrongDirection(Kind::Event)),
		}
		self.queued += 1;
		Ok(())
	}

	/// The transport took the packet off its own queue. THIS IS NOT A COMPLETION: the controller has
	/// not answered anything, and the credit it holds is still held.
	pub fn drained(&mut self) {
		self.queued = self.queued.saturating_sub(1);
	}

	/// The controller answered `n` commands, which is what a command-complete or command-status
	/// event reports.
	///
	/// A CONTROLLER THAT RETURNS MORE THAN IT OWES IS NOT BELIEVED INTO A LARGER WINDOW. Saturating
	/// at what is outstanding keeps a hostile or confused controller from widening the host's own
	/// sense of how many commands it may have in flight.
	pub fn commands_completed(&mut self, n: u32) {
		self.command_outstanding = self.command_outstanding.saturating_sub(n);
	}

	/// The controller freed `n` data buffers.
	pub fn acl_completed(&mut self, n: u32) {
		self.acl_outstanding = self.acl_outstanding.saturating_sub(n);
	}

	/// Everything in flight is gone: a reset or a fault ends the session and the controller owes
	/// nothing from it.
	pub fn reset(&mut self) {
		self.command_outstanding = 0;
		self.acl_outstanding = 0;
		self.queued = 0;
	}

	pub const fn commands_in_flight(&self) -> u32 {
		self.command_outstanding
	}

	pub const fn acl_in_flight(&self) -> u32 {
		self.acl_outstanding
	}

	pub const fn queued(&self) -> u32 {
		self.queued
	}
}

/// WHICH SESSION A PACKET BELONGS TO.
///
/// A reset advances the epoch, and a packet the controller had already queued arrives after it. Read
/// into the new session it is a reply to a command nobody sent, addressed to a link that no longer
/// exists - so the epoch is what makes a late packet discardable rather than confusing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Session {
	epoch: u32,
}

impl Session {
	pub const fn new(epoch: u32) -> Session {
		Session { epoch }
	}

	pub const fn epoch(&self) -> u32 {
		self.epoch
	}

	/// Begin a new session, answering its epoch. WRAPPING, because the epoch is an identity and not
	/// a count: a controller reset four billion times is a machine that has been running a very long
	/// time, and a saturating epoch would stop distinguishing sessions at the top rather than
	/// wrapping into one that is distinguishable from its neighbours.
	pub fn advance(&mut self) -> u32 {
		self.epoch = self.epoch.wrapping_add(1);
		self.epoch
	}

	/// Whether a packet stamped with this epoch belongs to the session now running.
	pub const fn admits(&self, epoch: u32) -> bool {
		self.epoch == epoch
	}
}

#[cfg(test)]
mod tests;
