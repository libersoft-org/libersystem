//! SPOOLSERVICE'S JOBS: admission, staging, the one explicit submission, transmission by acknowledged
//! prefixes, deadlines, and how every job ends.
//!
//! A JOB MOVES ONE WAY. `writing -> queued -> active -> transferred | failed | cancelled`. Only `submit` makes
//! bytes eligible for transmission, and only when exactly the declared length is staged; a second submit
//! answers the state it already has and sends nothing again. Nothing is written after submission, and
//! nothing a printer might have received is ever sent a second time - not after an error, not after a reset,
//! not to a replacement.
//!
//! EVERY BYTE IS RESERVED BEFORE IT IS ADMITTED. At most 4 MB a job and 16 MB across every job, 16 live jobs
//! and two a client; a job's whole declared length is reserved at creation, fallibly, and a request that
//! cannot be reserved is `exhausted` before anything is admitted. A write takes a whole frame of at most
//! 4096 bytes or none of it: past the declared length it takes none and FAILS the job with `size-limit`.
//!
//! TRANSMISSION IS BY ACKNOWLEDGED PREFIX. One job at a time per printer, oldest submission first, one write
//! of at most 4096 bytes outstanding. The backend's count advances the job by exactly that much and the rest
//! is offered again; `again` is none. Thirty seconds without an accepted byte ends the job `stalled`; ten
//! minutes from submission ends it `lifetime`. An ending while a write was outstanding says delivery is
//! UNCERTAIN - more may have reached the printer than was acknowledged.
//!
//! A TERMINAL JOB GIVES ITS BYTES BACK AT ONCE and keeps a small status record, still charged to its client,
//! until its channel closes; a record nobody watches any more is retired immediately.
//!
//! Time is in the system's 100 Hz ticks.

use alloc::collections::TryReserveError;
use alloc::vec::Vec;

pub const MAX_JOB_BYTES: u32 = 4 * 1024 * 1024;
pub const MAX_RESERVED: u64 = 16 * 1024 * 1024;
pub const MAX_JOBS: usize = 16;
pub const JOBS_PER_CLIENT: usize = 2;
pub const MAX_CLIENTS: usize = 16;
pub const MAX_PRINTERS: usize = 8;
pub const FRAME: usize = 4096;
pub const STALL_TICKS: u64 = 3000;
pub const LIFETIME_TICKS: u64 = 60_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
	Writing,
	Queued,
	Active,
	Transferred,
	Failed,
	Cancelled,
}

impl State {
	pub fn terminal(self) -> bool {
		matches!(self, State::Transferred | State::Failed | State::Cancelled)
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cause {
	SizeLimit,
	Unplugged,
	BackendError,
	Stalled,
	Lifetime,
	Reset,
	Abandoned,
	Cancelled,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// A language this printer or this system does not take.
	Unsupported,
	/// Past a bound, or the reservation could not be made.
	Exhausted,
	/// Zero or too long, a frame too large, a write past the declaration, a submit short of it, a write after
	/// submission.
	Invalid,
	/// No such job.
	NotFound,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Status {
	pub state: State,
	pub printer: u32,
	pub attachment: u64,
	pub declared: u32,
	pub staged: u32,
	pub acknowledged: u32,
	pub cause: Option<Cause>,
	pub uncertain: bool,
}

struct Job {
	id: u32,
	client: u32,
	printer: u32,
	attachment: u64,
	declared: u32,
	staging: Vec<u8>,
	// Bytes written into the reservation, which the status keeps after the reservation is gone.
	staged: u32,
	state: State,
	acknowledged: u32,
	cause: Option<Cause>,
	uncertain: bool,
	submitted: u64,
	progress: u64,
	// The length of the write outstanding at the backend.
	in_flight: Option<u32>,
	// Somebody still holds the job's channel.
	watched: bool,
}

/// A frame to hand the backend: which job, where in it, and the bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Frame<'a> {
	pub job: u32,
	pub offset: u32,
	pub bytes: &'a [u8],
}

pub struct Spool {
	jobs: Vec<Job>,
	reserved: u64,
	next_id: u32,
}

impl Default for Spool {
	fn default() -> Self {
		Self::new()
	}
}

fn reserve(declared: u32) -> Result<Vec<u8>, TryReserveError> {
	let mut staging = Vec::new();
	staging.try_reserve_exact(declared as usize)?;
	Ok(staging)
}

impl Spool {
	pub fn new() -> Self {
		Self { jobs: Vec::new(), reserved: 0, next_id: 1 }
	}

	pub fn reserved(&self) -> u64 {
		self.reserved
	}

	/// Live job records, terminal ones still watched included.
	pub fn len(&self) -> usize {
		self.jobs.len()
	}

	pub fn is_empty(&self) -> bool {
		self.jobs.is_empty()
	}

	pub fn charged(&self, client: u32) -> usize {
		self.jobs.iter().filter(|job| job.client == client).count()
	}

	fn at(&self, id: u32) -> Result<usize, Refusal> {
		self.jobs.iter().position(|job| job.id == id).ok_or(Refusal::NotFound)
	}

	/// Admission. `language` says whether this printer takes the declared language.
	pub fn create(&mut self, client: u32, printer: u32, attachment: u64, language: bool, length: u32) -> Result<u32, Refusal> {
		if !language {
			return Err(Refusal::Unsupported);
		}
		if length == 0 || length > MAX_JOB_BYTES {
			return Err(Refusal::Invalid);
		}
		if self.jobs.len() >= MAX_JOBS || self.charged(client) >= JOBS_PER_CLIENT || self.reserved + u64::from(length) > MAX_RESERVED {
			return Err(Refusal::Exhausted);
		}
		// FALLIBLY: memory pressure is `exhausted` before the job exists, never a failure halfway through it.
		let staging = reserve(length).map_err(|_| Refusal::Exhausted)?;
		let id = self.next_id;
		self.next_id = self.next_id.wrapping_add(1).max(1);
		self.reserved += u64::from(length);
		self.jobs.push(Job { id, client, printer, attachment, declared: length, staging, staged: 0, state: State::Writing, acknowledged: 0, cause: None, uncertain: false, submitted: 0, progress: 0, in_flight: None, watched: true });
		Ok(id)
	}

	// A job's ending: its bytes go back at once, and a record nobody watches is retired.
	fn end(&mut self, at: usize, state: State, cause: Option<Cause>) {
		let job = &mut self.jobs[at];
		if job.state.terminal() {
			return;
		}
		job.state = state;
		job.cause = cause;
		if job.in_flight.take().is_some() && state != State::Transferred {
			job.uncertain = true;
		}
		self.reserved -= u64::from(job.declared);
		job.staging = Vec::new();
		if !job.watched {
			self.jobs.remove(at);
		}
	}

	/// A whole frame into the reservation, or none of it.
	pub fn write(&mut self, id: u32, bytes: &[u8]) -> Result<u32, Refusal> {
		let at = self.at(id)?;
		let job = &mut self.jobs[at];
		if job.state != State::Writing || bytes.len() > FRAME {
			return Err(Refusal::Invalid);
		}
		if job.staging.len() + bytes.len() > job.declared as usize {
			self.end(at, State::Failed, Some(Cause::SizeLimit));
			return Err(Refusal::Invalid);
		}
		job.staging.extend_from_slice(bytes);
		job.staged += bytes.len() as u32;
		Ok(bytes.len() as u32)
	}

	/// Eligible for transmission - once, and only with exactly the declared length staged.
	pub fn submit(&mut self, id: u32, now: u64) -> Result<Status, Refusal> {
		let at = self.at(id)?;
		let job = &mut self.jobs[at];
		if job.state == State::Writing {
			if job.staging.len() != job.declared as usize {
				return Err(Refusal::Invalid);
			}
			job.state = State::Queued;
			job.submitted = now;
			job.progress = now;
		}
		Ok(self.status_of(at))
	}

	fn status_of(&self, at: usize) -> Status {
		let job = &self.jobs[at];
		Status { state: job.state, printer: job.printer, attachment: job.attachment, declared: job.declared, staged: job.staged, acknowledged: job.acknowledged, cause: job.cause, uncertain: job.uncertain }
	}

	pub fn status(&self, id: u32) -> Result<Status, Refusal> {
		Ok(self.status_of(self.at(id)?))
	}

	/// Cancel: a job not yet active is removed from the queue; an active one stops, keeping what was
	/// acknowledged and saying whether more may have gone.
	pub fn cancel(&mut self, id: u32) -> Result<Status, Refusal> {
		let at = self.at(id)?;
		if !self.jobs[at].state.terminal() {
			self.end(at, State::Cancelled, Some(Cause::Cancelled));
		}
		self.status(id)
	}

	/// The job's channel closed. A job still being written is abandoned and refunded - closing never
	/// submits; a submitted one goes on unwatched; a finished one is retired.
	pub fn close(&mut self, id: u32) {
		let Ok(at) = self.at(id) else { return };
		self.jobs[at].watched = false;
		match self.jobs[at].state {
			State::Writing => self.end(at, State::Failed, Some(Cause::Abandoned)),
			state if state.terminal() => {
				self.jobs.remove(at);
			}
			_ => {}
		}
	}

	/// The next frame for a printer: its active job's unacknowledged bytes, or its oldest queued job promoted.
	/// Nothing while a write is outstanding.
	pub fn next_frame(&mut self, printer: u32, attachment: u64, now: u64) -> Option<Frame<'_>> {
		if !self.jobs.iter().any(|job| job.printer == printer && job.state == State::Active) {
			let oldest = self.jobs.iter().enumerate().filter(|(_, job)| job.printer == printer && job.state == State::Queued).min_by_key(|(_, job)| (job.submitted, job.id)).map(|(at, _)| at)?;
			// A JOB ADMITTED FOR ANOTHER ATTACHMENT is never sent to this one.
			if self.jobs[oldest].attachment != attachment {
				self.end(oldest, State::Failed, Some(Cause::Reset));
				return None;
			}
			self.jobs[oldest].state = State::Active;
			self.jobs[oldest].progress = now;
		}
		let job = self.jobs.iter_mut().find(|job| job.printer == printer && job.state == State::Active)?;
		if job.in_flight.is_some() {
			return None;
		}
		let offset = job.acknowledged as usize;
		let end = (offset + FRAME).min(job.declared as usize);
		job.in_flight = Some((end - offset) as u32);
		Some(Frame { job: job.id, offset: offset as u32, bytes: &job.staging[offset..end] })
	}

	/// The backend accepted `accepted` bytes of the frame it was offered: exactly that much is advanced.
	pub fn written(&mut self, id: u32, accepted: u32, now: u64) -> Result<(), Refusal> {
		let at = self.at(id)?;
		let job = &mut self.jobs[at];
		let Some(offered) = job.in_flight else { return Err(Refusal::Invalid) };
		// A COUNT PAST WHAT WAS OFFERED is not an acknowledgement of anything: the job fails, uncertain.
		if accepted > offered {
			self.end(at, State::Failed, Some(Cause::BackendError));
			return Err(Refusal::Invalid);
		}
		job.in_flight = None;
		if accepted > 0 {
			job.acknowledged += accepted;
			job.progress = now;
		}
		if job.acknowledged == job.declared {
			self.end(at, State::Transferred, None);
		}
		Ok(())
	}

	/// The backend refused the write, or did not answer it: the job ends, and whether more may have reached
	/// the printer depends on whether it could have.
	pub fn write_failed(&mut self, id: u32, uncertain: bool) {
		let Ok(at) = self.at(id) else { return };
		// A JOB ALREADY OVER keeps the account it ended with.
		if self.jobs[at].state.terminal() {
			return;
		}
		self.jobs[at].uncertain |= uncertain;
		self.jobs[at].in_flight = None;
		self.end(at, State::Failed, Some(Cause::BackendError));
	}

	/// The printer went away: everything bound to it ends as unplugged, with whatever was acknowledged.
	pub fn printer_lost(&mut self, printer: u32) {
		self.fail_printer(printer, Cause::Unplugged);
	}

	/// The printer was reset: its attachment and every job on it are over, and nothing is replayed.
	pub fn printer_reset(&mut self, printer: u32) {
		self.fail_printer(printer, Cause::Reset);
	}

	fn fail_printer(&mut self, printer: u32, cause: Cause) {
		let ids: Vec<u32> = self.jobs.iter().filter(|job| job.printer == printer && !job.state.terminal()).map(|job| job.id).collect();
		for id in ids {
			if let Ok(at) = self.at(id) {
				self.end(at, State::Failed, Some(cause));
			}
		}
	}

	/// Deadlines: thirty seconds without progress for an active job, ten minutes from submission for any
	/// submitted one.
	pub fn tick(&mut self, now: u64) {
		let ids: Vec<(u32, Cause)> = self
			.jobs
			.iter()
			.filter_map(|job| match job.state {
				State::Active if now >= job.progress + STALL_TICKS => Some((job.id, Cause::Stalled)),
				State::Queued | State::Active if now >= job.submitted + LIFETIME_TICKS => Some((job.id, Cause::Lifetime)),
				_ => None,
			})
			.collect();
		for (id, cause) in ids {
			if let Ok(at) = self.at(id) {
				self.end(at, State::Failed, Some(cause));
			}
		}
	}

	pub fn next_deadline(&self) -> Option<u64> {
		self.jobs
			.iter()
			.filter_map(|job| match job.state {
				State::Active => Some((job.progress + STALL_TICKS).min(job.submitted + LIFETIME_TICKS)),
				State::Queued => Some(job.submitted + LIFETIME_TICKS),
				_ => None,
			})
			.min()
	}

	/// Whether a printer has anything to send.
	pub fn pending(&self, printer: u32) -> bool {
		self.jobs.iter().any(|job| job.printer == printer && matches!(job.state, State::Queued | State::Active) && job.in_flight.is_none())
	}

	pub fn queued(&self, printer: u32) -> usize {
		self.jobs.iter().filter(|job| job.printer == printer && job.state == State::Queued).count()
	}

	pub fn active(&self, printer: u32) -> bool {
		self.jobs.iter().any(|job| job.printer == printer && job.state == State::Active)
	}
}

#[cfg(test)]
mod tests;
