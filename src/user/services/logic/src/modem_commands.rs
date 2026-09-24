//! MODEMSERVICE'S COMMANDS: what may be sent to a modem now, which reply answers what, and what is known
//! when a reply never comes.
//!
//! EVERY COMMAND IS NAMED TWICE. By the provider connection's generation, and by a transaction number
//! that connection never reuses - numbers start again only with a new connection. A reply that names
//! another connection, a transaction that is not outstanding, or another kind of command is dropped; a
//! reply whose SIM or context generation is not the one the command was prepared against is STALE, and
//! answers its caller so without acting on anything. A late or duplicated activation reply therefore
//! cannot bring a replaced context back.
//!
//! BOUNDED, AND NEVER REPLAYED. At most eight commands are pending per modem and at most one of them
//! changes state. A query that times out has simply failed; a PIN, a PUK, an activation or a
//! deactivation that times out MAY HAVE REACHED THE DEVICE, so it completes as OUTCOME-UNKNOWN, its slot
//! is freed, and the modem is UNCERTAIN: no further state change is admitted until a read-only query has
//! answered. Nothing is ever sent twice on its own.
//!
//! A RETRY COUNTER IS KNOWN OR IT IS NOT. A count is kept with the SIM generation it was read for and when;
//! a new SIM makes it unknown, and an attempt against an unknown count needs the caller's explicit
//! acknowledgement.
//!
//! Time is in the system's 100 Hz ticks.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// The contract's maximum of pending commands per modem; a provider may negotiate fewer.
pub const MAX_PENDING: u8 = 8;
/// Queries, PIN, PUK, identity and deactivation have five seconds; an activation has sixty. They bound
/// the waiting, not what the hardware does.
pub const COMMAND_TICKS: u64 = 500;
pub const ACTIVATE_TICKS: u64 = 6000;
/// A raw-IP datagram's largest size, and each direction's queue per active context.
pub const MAX_MTU: u16 = 4096;
pub const QUEUE_PACKETS: usize = 64;
pub const QUEUE_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	Query,
	EnterPin,
	EnterPuk,
	Activate,
	Deactivate,
	Identity,
}

impl Kind {
	pub fn changes_state(self) -> bool {
		matches!(self, Kind::EnterPin | Kind::EnterPuk | Kind::Activate | Kind::Deactivate)
	}

	fn ticks(self) -> u64 {
		if self == Kind::Activate { ACTIVATE_TICKS } else { COMMAND_TICKS }
	}
}

/// A retry counter: known with its value, the SIM it was read for and when; or unknown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Count {
	Known { remaining: u8, sim_generation: u64, observed: u64 },
	Unknown,
}

/// What a caller is told a command came to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
	Done,
	Rejected,
	Failed,
	/// The SIM or context the command was prepared against is gone.
	Stale,
	/// It may have reached the device; nothing is assumed, and nothing is retried.
	OutcomeUnknown,
}

/// Why a command was not sent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Eight pending, or a state change already in flight.
	Busy,
	/// An earlier state change's outcome is unknown: a query has to answer before another is admitted.
	Reconcile,
	/// The remaining count is unknown and the caller did not acknowledge attempting anyway.
	UnknownCount,
	/// The caller's grant is for a SIM that is no longer in the modem.
	Stale,
	/// Deactivating a context that is not the current one.
	NoContext,
}

/// A command to send.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Command {
	pub connection: u64,
	pub transaction: u64,
	pub kind: Kind,
	pub sim_generation: u64,
	pub context_generation: u64,
}

/// A reply as the provider sent it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reply {
	pub connection: u64,
	pub transaction: u64,
	pub kind: Kind,
	pub status: Outcome,
	pub sim_generation: u64,
	pub context_generation: u64,
	/// The retry counters the reply's state carried, when it carried a state.
	pub counters: Option<(Option<u8>, Option<u8>)>,
	/// Whether the reply's state said the context is up, when it carried a state.
	pub context_active: Option<bool>,
}

/// A command answered - by its reply, or by its deadline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Completion {
	pub caller: u64,
	pub kind: Kind,
	pub result: Outcome,
	pub context_generation: u64,
}

struct Pending {
	command: Command,
	caller: u64,
	deadline: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ContextState {
	Inactive,
	Activating,
	Active,
	Deactivating,
	Unknown,
}

pub struct Device {
	connection: u64,
	next_transaction: u64,
	limit: u8,
	pending: Vec<Pending>,
	sim_generation: u64,
	context_generation: u64,
	next_context: u64,
	context: ContextState,
	uncertain: bool,
	pin: Count,
	puk: Count,
	revision: u64,
}

impl Device {
	/// A new provider connection, with the pending limit it negotiated down to.
	pub fn open(connection: u64, limit: u8, sim_generation: u64) -> Self {
		Self { connection, next_transaction: 1, limit: limit.clamp(1, MAX_PENDING), pending: Vec::new(), sim_generation, context_generation: 0, next_context: 1, context: ContextState::Inactive, uncertain: false, pin: Count::Unknown, puk: Count::Unknown, revision: 0 }
	}

	pub fn connection(&self) -> u64 {
		self.connection
	}

	pub fn sim_generation(&self) -> u64 {
		self.sim_generation
	}

	pub fn context(&self) -> (ContextState, u64) {
		(self.context, self.context_generation)
	}

	pub fn counters(&self) -> (Count, Count) {
		(self.pin, self.puk)
	}

	pub fn uncertain(&self) -> bool {
		self.uncertain
	}

	pub fn pending(&self) -> usize {
		self.pending.len()
	}

	/// The newest indication revision taken.
	pub fn revision(&self) -> u64 {
		self.revision
	}

	/// Admit one command, or say why not. `sim_generation` is the one the caller's grant is bound to.
	pub fn submit(&mut self, kind: Kind, caller: u64, sim_generation: u64, acknowledge_unknown: bool, now: u64) -> Result<Command, Refusal> {
		if sim_generation != self.sim_generation {
			return Err(Refusal::Stale);
		}
		if self.pending.len() >= usize::from(self.limit) {
			return Err(Refusal::Busy);
		}
		if kind.changes_state() {
			if self.pending.iter().any(|pending| pending.command.kind.changes_state()) {
				return Err(Refusal::Busy);
			}
			if self.uncertain {
				return Err(Refusal::Reconcile);
			}
		}
		let counter = match kind {
			Kind::EnterPin => Some(self.pin),
			Kind::EnterPuk => Some(self.puk),
			_ => None,
		};
		if counter == Some(Count::Unknown) && !acknowledge_unknown {
			return Err(Refusal::UnknownCount);
		}
		let context_generation = match kind {
			Kind::Activate => {
				let generation = self.next_context;
				self.next_context += 1;
				self.context = ContextState::Activating;
				generation
			}
			Kind::Deactivate => {
				if self.context_generation == 0 {
					return Err(Refusal::NoContext);
				}
				self.context = ContextState::Deactivating;
				self.context_generation
			}
			_ => self.context_generation,
		};
		let command = Command { connection: self.connection, transaction: self.next_transaction, kind, sim_generation: self.sim_generation, context_generation };
		self.next_transaction += 1;
		self.pending.push(Pending { command, caller, deadline: now + kind.ticks() });
		Ok(command)
	}

	/// A reply. DROPPED when it names another connection, a transaction not outstanding, or another kind;
	/// STALE when its SIM or context generation is not the command's.
	pub fn reply(&mut self, reply: Reply, now: u64) -> Option<Completion> {
		if reply.connection != self.connection {
			return None;
		}
		let at = self.pending.iter().position(|pending| pending.command.transaction == reply.transaction && pending.command.kind == reply.kind)?;
		let pending = self.pending.remove(at);
		let command = pending.command;
		if reply.sim_generation != command.sim_generation || reply.context_generation != command.context_generation {
			if command.kind == Kind::Activate && self.context == ContextState::Activating {
				self.context = ContextState::Inactive;
			}
			return Some(Completion { caller: pending.caller, kind: command.kind, result: Outcome::Stale, context_generation: command.context_generation });
		}
		if let Some((pin, puk)) = reply.counters {
			self.pin = count(pin, self.sim_generation, now);
			self.puk = count(puk, self.sim_generation, now);
		}
		match (command.kind, reply.status) {
			// A READ-ONLY ANSWER RECONCILES: what the device reports now is what is true - including
			// whether a context whose activation or deactivation was never confirmed is up.
			(Kind::Query, Outcome::Done) => {
				self.uncertain = false;
				if self.context == ContextState::Unknown {
					match reply.context_active {
						Some(true) => self.context = ContextState::Active,
						Some(false) => {
							self.context_generation = 0;
							self.context = ContextState::Inactive;
						}
						None => {}
					}
				}
			}
			(Kind::Activate, Outcome::Done) => {
				self.context_generation = command.context_generation;
				self.context = ContextState::Active;
			}
			(Kind::Activate, _) => self.context = if self.context_generation != 0 { ContextState::Active } else { ContextState::Inactive },
			(Kind::Deactivate, Outcome::Done) => {
				self.context_generation = 0;
				self.context = ContextState::Inactive;
			}
			(Kind::Deactivate, _) => self.context = ContextState::Active,
			_ => {}
		}
		Some(Completion { caller: pending.caller, kind: command.kind, result: reply.status, context_generation: command.context_generation })
	}

	/// Deadlines. A query that expired has failed; a state change that expired may have happened, is
	/// OUTCOME-UNKNOWN, and leaves the modem uncertain until a query answers.
	pub fn tick(&mut self, now: u64) -> Vec<Completion> {
		let (expired, kept): (Vec<Pending>, Vec<Pending>) = core::mem::take(&mut self.pending).into_iter().partition(|pending| pending.deadline <= now);
		self.pending = kept;
		expired
			.into_iter()
			.map(|pending| {
				let kind = pending.command.kind;
				let result = if kind.changes_state() {
					self.uncertain = true;
					if matches!(kind, Kind::Activate | Kind::Deactivate) {
						// THE CONTEXT THAT MAY EXIST is the one this command named: an activation that may
						// have happened is that generation, and so is a deactivation that may not have.
						self.context_generation = pending.command.context_generation;
						self.context = ContextState::Unknown;
					}
					Outcome::OutcomeUnknown
				} else {
					Outcome::Failed
				};
				Completion { caller: pending.caller, kind, result, context_generation: pending.command.context_generation }
			})
			.collect()
	}

	pub fn next_deadline(&self) -> Option<u64> {
		self.pending.iter().map(|pending| pending.deadline).min()
	}

	/// An unsolicited indication. Only a newer revision counts. A NEW SIM GENERATION ENDS EVERYTHING BOUND TO
	/// THE OLD ONE: the context is gone, the counters are unknown, and every grant for that SIM is stale.
	/// Answers whether the SIM changed, and whether the device says its context is up.
	pub fn indication(&mut self, revision: u64, sim_generation: u64, counters: (Option<u8>, Option<u8>), context_active: bool, now: u64) -> Option<SimChange> {
		if revision <= self.revision {
			return None;
		}
		self.revision = revision;
		if sim_generation != self.sim_generation {
			self.sim_generation = sim_generation;
			self.context_generation = 0;
			self.context = ContextState::Inactive;
			self.pin = Count::Unknown;
			self.puk = Count::Unknown;
			return Some(SimChange::Replaced);
		}
		self.pin = count(counters.0, sim_generation, now);
		self.puk = count(counters.1, sim_generation, now);
		// THE NETWORK ENDED THE CONTEXT: reported as lost, not left looking active.
		if self.context == ContextState::Active && !context_active {
			self.context_generation = 0;
			self.context = ContextState::Inactive;
			return Some(SimChange::ContextLost);
		}
		Some(SimChange::Updated)
	}
}

/// What an indication changed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SimChange {
	Updated,
	Replaced,
	ContextLost,
}

fn count(value: Option<u8>, sim_generation: u64, now: u64) -> Count {
	match value {
		Some(remaining) => Count::Known { remaining, sim_generation, observed: now },
		None => Count::Unknown,
	}
}

/// A watcher of modem state that is never more than one snapshot behind: a change it has not taken yet
/// is replaced by the newest, and when that happens the next snapshot says RESYNC - it did not see every
/// transition, and is told so.
#[derive(Default)]
pub struct Watch {
	pending: bool,
	coalesced: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WatchKind {
	Snapshot,
	Resync,
}

impl Watch {
	pub fn changed(&mut self) {
		if self.pending {
			self.coalesced = true;
		}
		self.pending = true;
	}

	pub fn is_pending(&self) -> bool {
		self.pending
	}

	/// Whether a snapshot is due, and of which kind.
	pub fn take(&mut self) -> Option<WatchKind> {
		if !self.pending {
			return None;
		}
		self.pending = false;
		Some(if core::mem::take(&mut self.coalesced) { WatchKind::Resync } else { WatchKind::Snapshot })
	}

	/// A snapshot that was taken and could not be delivered is due again, AS WHAT IT WAS: a resync stays a
	/// resync, and a change that arrives before it goes makes it one.
	pub fn requeue(&mut self, kind: WatchKind) {
		if self.pending || kind == WatchKind::Resync {
			self.coalesced = true;
		}
		self.pending = true;
	}
}

/// One direction of an active context's datagrams: at most 64 packets and 256 kB, each within the MTU.
#[derive(Default)]
pub struct PacketQueue {
	packets: VecDeque<Vec<u8>>,
	bytes: usize,
	dropped: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PacketRefusal {
	/// The queue is full: a transmit is answered busy.
	Busy,
	/// Past the MTU.
	Oversized,
}

impl PacketQueue {
	pub fn push(&mut self, packet: &[u8], mtu: u16) -> Result<(), PacketRefusal> {
		if packet.len() > usize::from(mtu.min(MAX_MTU)) || packet.is_empty() {
			return Err(PacketRefusal::Oversized);
		}
		if self.packets.len() >= QUEUE_PACKETS || self.bytes + packet.len() > QUEUE_BYTES {
			return Err(PacketRefusal::Busy);
		}
		self.bytes += packet.len();
		self.packets.push_back(packet.to_vec());
		Ok(())
	}

	/// A received datagram with nowhere to go is DROPPED AND COUNTED, never queued without bound.
	pub fn receive(&mut self, packet: &[u8], mtu: u16) -> bool {
		if self.push(packet, mtu).is_err() {
			self.dropped += 1;
			return false;
		}
		true
	}

	pub fn front(&self) -> Option<&[u8]> {
		self.packets.front().map(Vec::as_slice)
	}

	pub fn pop(&mut self) -> Option<Vec<u8>> {
		let packet = self.packets.pop_front()?;
		self.bytes -= packet.len();
		Some(packet)
	}

	pub fn dropped(&self) -> u64 {
		self.dropped
	}

	pub fn len(&self) -> usize {
		self.packets.len()
	}

	pub fn is_empty(&self) -> bool {
		self.packets.is_empty()
	}

	/// The context is gone: everything queued goes with it.
	pub fn clear(&mut self) {
		self.packets.clear();
		self.bytes = 0;
	}
}

#[cfg(test)]
mod tests;
