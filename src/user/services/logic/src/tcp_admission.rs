//! What inbound TCP state costs, and the point at which it is refused.
//!
//! CHARGED BEFORE IT IS PUBLISHED, which is the whole item in one sentence. A handle ceiling does not
//! bound any of this: a half-open control block is expressly the state with no socket channel, and
//! unacknowledged transmit bytes say nothing about receive storage. So the budgets are here, they are
//! numbers rather than decisions to be made later, and every one of them is charged while the state
//! is still provisional.
//!
//! THE REFUSAL POINT IS THE SYN, AND THAT IS NOT A PREFERENCE. Refusing after the handshake completes
//! means the peer has had the SYN-ACK and sent its final ACK: it is ESTABLISHED and will retransmit
//! DATA, never the handshake. With the control block gone the tuple is CLOSED, and RFC 9293 section
//! 3.10.7 then requires a RESET in answer - so refusing late is a choice between stranding the peer
//! and sending exactly the reset that refusing late was supposed to avoid. Refusing on the SYN costs
//! nothing, charges nothing, and gives the peer the retry story: a SYN is the segment it
//! retransmits, so its next attempt finds the queue drained.
//!
//! AND NOTHING ADMITTED IS EVER EVICTED. A budget that is reached refuses the NEW state. A flood that
//! evicted established connections would be the same denial of service by another route.

use alloc::vec::Vec;

/// Half-open control blocks, service-wide.
pub const MAX_HALF_OPEN: u32 = 64;

/// The largest effective backlog one listener may have.
pub const MAX_BACKLOG_PER_LISTENER: u16 = 32;

/// Occupied backlog slots across every listener: reserved half-open plus established-unaccepted.
pub const MAX_BACKLOG_TOTAL: u32 = 64;

/// Control blocks in total, accepted, outbound and closing included.
pub const MAX_TCBS: u32 = 128;

/// Receive storage across all connections.
pub const MAX_RECEIVE_BYTES: usize = 2 * 1024 * 1024;

/// What each admitted control block starts with, charged before it is allocated.
///
/// SIXTEEN KILOBYTES, AND THE OLD NUMBER COULD NOT SURVIVE THIS BUDGET. A control block used to
/// allocate 65535 bytes and grow unconditionally to 262140 the moment window scaling was offered;
/// sixty-four base buffers alone would be 4 MB against a 2 MB cap. Negotiating the scale therefore
/// RECORDS it without growing storage, and growth afterwards is fallible and charged.
pub const RECEIVE_BASE_BYTES: usize = 16_384;

/// The per-flow receive ceiling without window scaling, and with the scale of two this profile uses.
pub const RECEIVE_CEILING_UNSCALED: usize = 65_535;
pub const RECEIVE_CEILING_SCALED: usize = 262_140;

/// Why an admission was refused.
///
/// ONE REASON EVEN WHEN SEVERAL LIMITS ARE FULL, which is what the fixed precedence below buys: an
/// operator reading the counters can tell "this listener is slow" from "this machine is full".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// The service-wide occupied-slot count is reached. This machine is full.
	ServiceBacklog,
	/// This listener's own effective backlog is reached. This listener is slow.
	ListenerBacklog,
	/// Half-open control blocks are exhausted.
	HalfOpen,
	/// Control blocks are exhausted.
	ControlBlocks,
	/// Receive storage is exhausted.
	ReceiveBytes,
	/// There is no such listener - it was withdrawn between the segment arriving and this check.
	NoListener,
}

/// Why a backlog request was refused outright.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BacklogRefusal {
	/// Zero. A listener that may hold no unaccepted connection cannot work, and promoting it to one
	/// silently would be a second meaning for a value the caller wrote.
	Zero,
}

/// The effective backlog for a requested one.
///
/// CLAMPED, NOT REFUSED. A request above the cap succeeds AT the cap, which is what every caller of
/// this kind of API expects and what keeps the service's own bound authoritative: a refusal would
/// make a portable program guess a number this file chose and may change.
pub fn effective_backlog(requested: u16) -> Result<u16, BacklogRefusal> {
	match requested {
		0 => Err(BacklogRefusal::Zero),
		value => Ok(value.min(MAX_BACKLOG_PER_LISTENER)),
	}
}

/// One listener's occupancy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ListenerSlots {
	pub id: u32,
	pub effective: u16,
	/// Admitted SYNs whose handshake has not completed.
	pub reserved: u16,
	/// Completed handshakes nobody has accepted yet.
	pub queued: u16,
}

impl ListenerSlots {
	/// The invariant this listener is held to: `reserved + queued <= effective`.
	pub fn occupied(&self) -> u16 {
		self.reserved + self.queued
	}
}

/// How many of each refusal has happened. Saturating, and never keyed by anything a peer chooses.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Refusals {
	pub service_backlog: u32,
	pub listener_backlog: u32,
	pub half_open: u32,
	pub control_blocks: u32,
	pub receive_bytes: u32,
}

impl Refusals {
	fn record(&mut self, refusal: Refusal) {
		let counter = match refusal {
			Refusal::ServiceBacklog => &mut self.service_backlog,
			Refusal::ListenerBacklog => &mut self.listener_backlog,
			Refusal::HalfOpen => &mut self.half_open,
			Refusal::ControlBlocks => &mut self.control_blocks,
			Refusal::ReceiveBytes => &mut self.receive_bytes,
			Refusal::NoListener => return,
		};
		*counter = counter.saturating_add(1);
	}
}

/// What the service is currently holding.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Occupancy {
	pub half_open: u32,
	pub control_blocks: u32,
	pub receive_bytes: usize,
	pub reserved: u32,
	pub queued: u32,
}

/// The service's inbound accounting.
#[derive(Debug, Default)]
pub struct Admission {
	half_open: u32,
	control_blocks: u32,
	receive_bytes: usize,
	listeners: Vec<ListenerSlots>,
	refusals: Refusals,
}

impl Admission {
	pub fn new() -> Admission {
		Admission::default()
	}

	pub fn refusals(&self) -> Refusals {
		self.refusals
	}

	pub fn occupancy(&self) -> Occupancy {
		Occupancy { half_open: self.half_open, control_blocks: self.control_blocks, receive_bytes: self.receive_bytes, reserved: self.listeners.iter().map(|held| u32::from(held.reserved)).sum(), queued: self.listeners.iter().map(|held| u32::from(held.queued)).sum() }
	}

	/// Occupied slots across every listener: reserved plus established-unaccepted.
	pub fn occupied_slots(&self) -> u32 {
		self.listeners.iter().map(|held| u32::from(held.occupied())).sum()
	}

	pub fn listener(&self, id: u32) -> Option<&ListenerSlots> {
		self.listeners.iter().find(|held| held.id == id)
	}

	/// Start accounting for a listener. The caller has already clamped its backlog.
	pub fn register_listener(&mut self, id: u32, effective: u16) {
		if self.listeners.iter().any(|held| held.id == id) {
			return;
		}
		self.listeners.push(ListenerSlots { id, effective, reserved: 0, queued: 0 });
	}

	/// Withdraw a listener. Returns how many of its connections are aborted with it.
	///
	/// WITHDRAWAL ABORTS WHAT IT WAS HOLDING, because no caller can accept them afterwards. A socket
	/// already handed off has its own lifecycle and is untouched.
	pub fn withdraw_listener(&mut self, id: u32) -> u32 {
		let Some(index) = self.listeners.iter().position(|held| held.id == id) else {
			return 0;
		};
		let slots = self.listeners.remove(index);
		let aborted: u32 = u32::from(slots.occupied());
		self.half_open = self.half_open.saturating_sub(u32::from(slots.reserved));
		self.control_blocks = self.control_blocks.saturating_sub(aborted);
		self.receive_bytes = self.receive_bytes.saturating_sub(aborted as usize * RECEIVE_BASE_BYTES);
		aborted
	}

	/// Admit a new SYN to `listener`, charging everything it needs before anything is published.
	///
	/// THE PRECEDENCE IS FIXED so a refusal has exactly one reason: the service-wide occupied-slot
	/// count first, then this listener's own backlog, then half-open, control blocks and receive
	/// storage. Nothing is charged unless all of them pass.
	pub fn admit_syn(&mut self, listener: u32) -> Result<(), Refusal> {
		let Some(index) = self.listeners.iter().position(|held| held.id == listener) else {
			return Err(Refusal::NoListener);
		};
		let refusal: Option<Refusal> = if self.occupied_slots() >= MAX_BACKLOG_TOTAL {
			Some(Refusal::ServiceBacklog)
		} else if self.listeners[index].occupied() >= self.listeners[index].effective {
			Some(Refusal::ListenerBacklog)
		} else if self.half_open >= MAX_HALF_OPEN {
			Some(Refusal::HalfOpen)
		} else if self.control_blocks >= MAX_TCBS {
			Some(Refusal::ControlBlocks)
		} else if self.receive_bytes + RECEIVE_BASE_BYTES > MAX_RECEIVE_BYTES {
			Some(Refusal::ReceiveBytes)
		} else {
			None
		};
		if let Some(refusal) = refusal {
			self.refusals.record(refusal);
			return Err(refusal);
		}
		self.listeners[index].reserved += 1;
		self.half_open += 1;
		self.control_blocks += 1;
		self.receive_bytes += RECEIVE_BASE_BYTES;
		Ok(())
	}

	/// A handshake completed: the reservation becomes an established-unaccepted connection.
	///
	/// OCCUPANCY DOES NOT CHANGE AND NO SECOND ADMISSION DECISION IS MADE. The slot was taken when the
	/// SYN was admitted; this only says which kind of thing is in it.
	pub fn handshake_complete(&mut self, listener: u32) -> bool {
		let Some(slots) = self.listeners.iter_mut().find(|held| held.id == listener) else {
			return false;
		};
		if slots.reserved == 0 {
			return false;
		}
		slots.reserved -= 1;
		slots.queued += 1;
		self.half_open = self.half_open.saturating_sub(1);
		true
	}

	/// An accept handed the socket capability over successfully.
	///
	/// THE SLOT IS HELD UNTIL THE HANDOFF SUCCEEDS. Removing a queue entry before a fallible handoff
	/// is not release: a failed channel allocation leaves the connection queued and charged, and the
	/// listener stays live.
	pub fn accepted(&mut self, listener: u32) -> bool {
		let Some(slots) = self.listeners.iter_mut().find(|held| held.id == listener) else {
			return false;
		};
		if slots.queued == 0 {
			return false;
		}
		slots.queued -= 1;
		// The control block and its receive storage stay charged: the connection is still here, it
		// just belongs to its new owner now.
		true
	}

	/// A half-open connection expired, or the peer reset it. Releases its reservation exactly once.
	pub fn release_half_open(&mut self, listener: u32) -> bool {
		let Some(slots) = self.listeners.iter_mut().find(|held| held.id == listener) else {
			return false;
		};
		if slots.reserved == 0 {
			return false;
		}
		slots.reserved -= 1;
		self.half_open = self.half_open.saturating_sub(1);
		self.control_blocks = self.control_blocks.saturating_sub(1);
		self.receive_bytes = self.receive_bytes.saturating_sub(RECEIVE_BASE_BYTES);
		true
	}

	/// An outbound connection: a control block and its receive storage, and no backlog slot.
	pub fn open_outbound(&mut self) -> Result<(), Refusal> {
		let refusal: Option<Refusal> = if self.control_blocks >= MAX_TCBS {
			Some(Refusal::ControlBlocks)
		} else if self.receive_bytes + RECEIVE_BASE_BYTES > MAX_RECEIVE_BYTES {
			Some(Refusal::ReceiveBytes)
		} else {
			None
		};
		if let Some(refusal) = refusal {
			self.refusals.record(refusal);
			return Err(refusal);
		}
		self.control_blocks += 1;
		self.receive_bytes += RECEIVE_BASE_BYTES;
		Ok(())
	}

	/// A control block is gone, with `receive` bytes of storage.
	///
	/// FREEING RELEASES THE ALLOCATION rather than keeping an uncharged base buffer in a reusable
	/// slot, which is how a pool comes to hold memory no budget knows about.
	pub fn close(&mut self, receive: usize) {
		self.control_blocks = self.control_blocks.saturating_sub(1);
		self.receive_bytes = self.receive_bytes.saturating_sub(receive);
	}

	/// Try to grow one connection's receive storage from `current` to `wanted`.
	///
	/// CHARGED BEFORE THE CREDIT IS ADVERTISED, and BOTH buffers are charged while the copy happens -
	/// a growth that counted only the new one would be understating the peak by exactly the old
	/// buffer. A refusal keeps the current buffer and leaves every other connection alone: nothing is
	/// evicted to grow somebody else.
	pub fn grow_receive(&mut self, current: usize, wanted: usize, scaled: bool) -> Option<usize> {
		let ceiling: usize = match scaled {
			true => RECEIVE_CEILING_SCALED,
			false => RECEIVE_CEILING_UNSCALED,
		};
		let target: usize = wanted.min(ceiling);
		if target <= current {
			return None;
		}
		// The peak is the old buffer and the new one at once.
		if self.receive_bytes + target > MAX_RECEIVE_BYTES {
			self.refusals.record(Refusal::ReceiveBytes);
			return None;
		}
		self.receive_bytes = self.receive_bytes - current + target;
		Some(target)
	}
}

#[cfg(test)]
mod tests;
