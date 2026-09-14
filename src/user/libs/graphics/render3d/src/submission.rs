//! ASYNCHRONOUS-CAPABLE SUBMISSION, DEFINED BEFORE EITHER BACKEND EXISTS.
//!
//! WHY BEFORE. A software backend finishes its work inside `submit` and a GPU one does not, and an
//! API shaped around the first has no place to put the second: the completion becomes a return value,
//! the resources are released at the end of the call, and the first pending submission needs a
//! different function. Defining the shape now means the software backend implements the asynchronous
//! contract with an already-ready completion, which is a legal value of it rather than a special case.
//!
//! `prepare -> submit -> Submission { completion, status }`. Preparation is where everything that can
//! fail deterministically fails - validation, resource resolution, storage reservation - so a
//! `submit` refuses only for the reasons a running system has: too many submissions in flight, a lost
//! backend. A caller that prepared successfully knows the work is well formed.
//!
//! THE COMPLETION STORAGE IS RESERVED WHEN THE BACKEND IS CREATED AND NOT PER FRAME. A renderer that
//! allocated one completion per submission would allocate every frame for ever; the bound on
//! outstanding submissions is what makes a fixed reservation possible, and it is the same bound that
//! makes "refuse rather than queue for ever" answerable.
//!
//! A SUBMISSION OWNS ITS RESOURCES UNTIL ITS COMPLETION IS OBSERVABLE, which is the profile's rule
//! and is why `Submission` carries the retained set rather than a caller being asked to keep them
//! alive. Releasing them is what `settle` does, in submission order.

use alloc::vec::Vec;

use crate::error::Error;

/// Where a submission has got to.
///
/// A PENDING SUBMISSION IS NOT A FAILED ONE AND NOT A FINISHED ONE, and the three are separate
/// values rather than an `Option<Result>` - which reads as "no answer yet" and "no answer ever" with
/// the same shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
	/// The work has not finished. A caller waits on the progress source rather than polling.
	Pending,
	/// It finished and every command was executed.
	Complete,
	/// The caller cancelled it. Whatever had executed stays executed - cancellation is not a
	/// rollback, and saying so is the difference between a caller that re-submits and one that
	/// assumes the attachments are untouched.
	Cancelled,
	/// The backend is gone. Every submission on it is this, including the ones that had finished:
	/// their results are no longer readable, which is the thing a caller needs to know.
	BackendLost,
	/// The work itself refused at execution. Preparation catches what can be caught deterministically;
	/// this is what a running system found.
	Failed(Error),
}

impl Status {
	/// Whether this is a terminal answer. A caller waiting on a submission is waiting for this to be
	/// true, and nothing else.
	pub const fn terminal(self) -> bool {
		!matches!(self, Self::Pending)
	}
}

/// A submission's completion, as a token the caller OWNS.
///
/// OWNERSHIP-CONSUMING, which is what makes "waited for exactly once" a property of the type rather
/// than a convention: `wait` takes the token by value, so a second wait on the same submission does
/// not compile. The same shape the 2D profile's presentation completion has, for the same reason.
pub struct Completion {
	/// Which submission this is the completion of, in submission order.
	pub serial: u64,
	/// The waitable source a caller blocks on. THIS LAYER DOES NOT OWN IT: it is the backend's
	/// channel or event, named here so the shape is fixed and the backend supplies the value.
	pub source: u32,
}

impl Completion {
	/// Consume the token and answer the terminal status.
	///
	/// TAKES `self` BY VALUE. A completion waited on twice is a caller that has lost track of which
	/// frame it is on, and the second wait would answer for a submission whose resources are already
	/// released.
	pub fn wait(self, status: Status) -> Result<Status, Error> {
		if !status.terminal() {
			return Err(Error::InvalidRenderState { reason: "a completion answered while its submission is still pending" });
		}
		Ok(status)
	}
}

/// One submitted command list and what it holds until it settles.
pub struct Submission {
	pub completion: Completion,
	pub status: Status,
	/// The resources this submission owns until its completion is observable. Buffers and textures,
	/// as the caller's own identifiers.
	retained: Vec<u32>,
}

impl Submission {
	pub fn retained(&self) -> &[u32] {
		&self.retained
	}

	/// Whether a resource is owned by this submission, which is what a host write is refused
	/// against.
	pub fn owns(&self, resource: u32) -> bool {
		self.retained.contains(&resource)
	}
}

/// A readback ticket, which uses THE SAME completion and status contract as a submission.
///
/// THE SAME AND NOT A SECOND ONE. A readback is a command IN a list and completes with it - a
/// readback issued outside a list would need its own ordering rules against the lists around it - so
/// its ticket is the submission's completion with a destination attached.
pub struct ReadbackTicket {
	pub completion: Completion,
	pub status: Status,
	/// Where the bytes land. The caller's own identifier for a readback buffer.
	pub destination: u32,
}

/// The queue: bounded, ordered, and the owner of every submission's retained set.
pub struct Queue {
	/// How many submissions may be outstanding. THE BOUND IS WHAT MAKES A FIXED RESERVATION
	/// POSSIBLE and what makes "refuse rather than queue for ever" answerable.
	capacity: usize,
	in_flight: Vec<Submission>,
	next_serial: u64,
	lost: bool,
}

impl Queue {
	/// Reserve the completion storage ONCE, when the backend is created.
	pub fn with_capacity(capacity: usize) -> Result<Self, Error> {
		if capacity == 0 {
			return Err(Error::InvalidRenderState { reason: "a queue that can hold no submissions can accept none" });
		}
		let mut in_flight = Vec::new();
		in_flight.try_reserve_exact(capacity).map_err(|_| Error::OutOfMemory { bytes: (capacity * core::mem::size_of::<Submission>()) as u64 })?;
		Ok(Self { capacity, in_flight, next_serial: 0, lost: false })
	}

	pub fn in_flight(&self) -> usize {
		self.in_flight.len()
	}

	/// Submit a prepared list.
	///
	/// REFUSES OVERFLOW WITHOUT PARTIAL PUBLICATION: a submission that does not fit is not queued at
	/// all, so the caller's resources are still its own and the serial is not spent. A queue that
	/// half-accepted would leave a caller unable to tell which.
	pub fn submit(&mut self, source: u32, retained: &[u32]) -> Result<u64, Error> {
		if self.lost {
			return Err(Error::InvalidRenderState { reason: "a submission to a backend that is gone" });
		}
		if self.in_flight.len() >= self.capacity {
			return Err(Error::LimitExceeded { limit: "submissions in flight", ceiling: self.capacity as u64, asked: self.in_flight.len() as u64 + 1 });
		}
		let mut owned = Vec::new();
		owned.try_reserve_exact(retained.len()).map_err(|_| Error::OutOfMemory { bytes: (retained.len() * core::mem::size_of::<u32>()) as u64 })?;
		owned.extend_from_slice(retained);
		let serial = self.next_serial;
		self.next_serial += 1;
		self.in_flight.push(Submission { completion: Completion { serial, source }, status: Status::Pending, retained: owned });
		Ok(serial)
	}

	/// Whether any submission in flight owns this resource, which is what a host write is refused
	/// against - AT THE WRITE, so the report names the thing that is wrong.
	pub fn owned_by_a_submission(&self, resource: u32) -> bool {
		self.in_flight.iter().any(|submission| submission.owns(resource))
	}

	/// Mark a submission finished. RESOURCES ARE RELEASED IN SUBMISSION ORDER and not as each
	/// finishes: a backend may complete two out of order where nothing observes it, and releasing
	/// out of order would let a caller reuse a buffer an earlier submission still holds.
	pub fn settle(&mut self, serial: u64, status: Status) -> Result<Vec<u32>, Error> {
		if !status.terminal() {
			return Err(Error::InvalidRenderState { reason: "a submission settled with a status that is not terminal" });
		}
		let Some(at) = self.in_flight.iter().position(|submission| submission.completion.serial == serial) else {
			return Err(Error::InvalidRenderState { reason: "a submission settled that is not in flight" });
		};
		self.in_flight[at].status = status;
		// Release every finished submission from the FRONT, stopping at the first that is still
		// pending - which is what makes the release order the submission order.
		let mut released = Vec::new();
		while self.in_flight.first().is_some_and(|submission| submission.status.terminal()) {
			let submission = self.in_flight.remove(0);
			released.extend_from_slice(&submission.retained);
		}
		Ok(released)
	}

	/// Cancel a submission. NOT A ROLLBACK: whatever had executed stays executed, and the status
	/// says so.
	pub fn cancel(&mut self, serial: u64) -> Result<(), Error> {
		self.settle(serial, Status::Cancelled).map(|_| ())
	}

	/// The backend is gone. EVERY submission becomes `BackendLost`, including the finished ones,
	/// because their results are no longer readable - which is the thing a caller needs to know.
	pub fn lose_backend(&mut self) -> Vec<u32> {
		self.lost = true;
		let mut released = Vec::new();
		for submission in &mut self.in_flight {
			submission.status = Status::BackendLost;
			released.extend_from_slice(&submission.retained);
		}
		self.in_flight.clear();
		released
	}

	pub fn is_lost(&self) -> bool {
		self.lost
	}
}
