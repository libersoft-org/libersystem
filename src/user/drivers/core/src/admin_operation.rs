//! AN ADMINISTRATIVE EXECUTOR'S PREPARED OPERATIONS: what it froze, and the one attempt it will make.
//!
//! PREPARATION COPIES. The payload is copied - at most 4096 bytes - into storage the executor owns, and its
//! SHA-256 is taken of that copy; the live target generation and the parameters are frozen beside it. So
//! nothing a requester can still write through an alias of its own reaches the effect, and the descriptor a
//! person confirms describes exactly what will be written.
//!
//! AT MOST ONE ATTEMPT. `start` is the start guard: the operation must exist, belong to this executor's epoch,
//! be neither cancelled nor started, still name the live target generation and be inside its lifetime - all
//! checked together - and then it is started, and never again. `revalidate` asks the same questions and
//! starts nothing. Cancellation after the start promises nothing: what began is not rolled back.

use alloc::vec::Vec;

pub const MAX_PAYLOAD: usize = 4096;
pub const MAX_PARAMETERS: usize = 256;
/// Preparations held at once. One active prompt needs one; the rest is slack for ones being released.
pub const MAX_OPERATIONS: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Past a bound: the payload, the parameters.
	Bounds,
	/// No such operation.
	NotFound,
	/// Every slot holds a live preparation.
	Busy,
	/// Another executor epoch, or a target generation that is no longer the live one.
	Stale,
	Cancelled,
	/// Its one attempt has been made.
	Started,
	/// Past its lifetime.
	Expired,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Prepared {
	pub operation: u64,
	pub generation: u64,
	pub parameters: Vec<u8>,
	/// The executor's own copy.
	pub payload: Vec<u8>,
	pub digest: [u8; 32],
	pub expires: u64,
	started: bool,
	cancelled: bool,
}

impl Prepared {
	fn live(&self) -> bool {
		!self.started && !self.cancelled
	}
}

pub struct Operations {
	pub epoch: u64,
	next: u64,
	held: Vec<Prepared>,
}

impl Operations {
	pub fn new(epoch: u64) -> Operations {
		Operations { epoch, next: 1, held: Vec::new() }
	}

	/// Freeze one operation against the live target generation. The payload is copied here - `payload` is
	/// read once and never again.
	pub fn prepare(&mut self, generation: u64, parameters: &[u8], payload: &[u8], now: u64, lifetime: u64) -> Result<&Prepared, Refusal> {
		if payload.is_empty() || payload.len() > MAX_PAYLOAD || parameters.len() > MAX_PARAMETERS {
			return Err(Refusal::Bounds);
		}
		// WHAT WILL NEVER RUN makes room first; a live preparation is never displaced.
		self.held.retain(Prepared::live);
		if self.held.len() >= MAX_OPERATIONS {
			return Err(Refusal::Busy);
		}
		let operation = self.next;
		self.next += 1;
		let payload = payload.to_vec();
		let digest = bootproto::sha256::digest(&payload);
		self.held.push(Prepared { operation, generation, parameters: parameters.to_vec(), payload, digest, expires: now + lifetime, started: false, cancelled: false });
		Ok(self.held.last().expect("just pushed"))
	}

	fn check(&self, operation: u64, epoch: u64, generation: u64, now: u64) -> Result<usize, Refusal> {
		if epoch != self.epoch {
			return Err(Refusal::Stale);
		}
		let at = self.held.iter().position(|held| held.operation == operation).ok_or(Refusal::NotFound)?;
		let held = &self.held[at];
		if held.cancelled {
			return Err(Refusal::Cancelled);
		}
		if held.started {
			return Err(Refusal::Started);
		}
		if held.generation != generation {
			return Err(Refusal::Stale);
		}
		if now >= held.expires {
			return Err(Refusal::Expired);
		}
		Ok(at)
	}

	/// Still the same live target, still unstarted: nothing is started by asking.
	pub fn revalidate(&self, operation: u64, generation: u64, now: u64) -> Result<(), Refusal> {
		self.check(operation, self.epoch, generation, now).map(|_| ())
	}

	/// THE START GUARD, and the only way to an attempt: every check, then started - once.
	pub fn start(&mut self, operation: u64, epoch: u64, generation: u64, now: u64) -> Result<&Prepared, Refusal> {
		let at = self.check(operation, epoch, generation, now)?;
		self.held[at].started = true;
		Ok(&self.held[at])
	}

	/// Withdraw a preparation that has not started. One that has is not recalled.
	pub fn cancel(&mut self, operation: u64) -> Result<(), Refusal> {
		let held = self.held.iter_mut().find(|held| held.operation == operation).ok_or(Refusal::NotFound)?;
		if held.started {
			return Err(Refusal::Started);
		}
		held.cancelled = true;
		Ok(())
	}
}

#[cfg(test)]
mod tests;
