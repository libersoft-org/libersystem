//! MEDIAIMPORTSERVICE'S DECISIONS: what may be admitted, what it costs, what an identity still names, how a
//! snapshot is taken and paged, and how a transfer ends.
//!
//! EVERYTHING IS CHARGED BEFORE IT EXISTS. One 4 MB budget holds snapshots (at most 2 MB of them), pages,
//! datasets, chunks and pending replies; a request that does not fit is `exhausted` before anything is
//! allocated, and `again` is kept for the one thing it means here - another transaction on the same device is
//! still completing.
//!
//! A SNAPSHOT IS ONE TRANSACTION. The handle list arrives whole in one GetObjectHandles; its count is checked
//! against the container's own length and against 65 536 before a single ID is kept, and a list longer than
//! that is an explicit over-limit answer, never a truncated one.
//!
//! IDENTITY IS AN EPOCH. A device's attachment, session and content epoch are in every identity handed out;
//! when any of them moves, whatever was named under the old values is stale. An object also carries the
//! revision of its metadata as fetched, and a read revalidates it first.
//!
//! A TRANSFER ENDS COMPLETE ONLY ON EVIDENCE: exactly the expected bytes, framed as one data container of that
//! length, and the matching successful final response. Every other ending is partial, with the count and the
//! cause, and nothing is inferred from a short chunk.
//!
//! Time is in the system's 100 Hz ticks.

use crate::ptp;
use alloc::vec::Vec;

pub const MAX_PROVIDERS: usize = 8;
pub const MAX_CLIENTS: usize = 16;
pub const MAX_CURSORS: usize = 8;
pub const MAX_TRANSFERS: usize = 8;
pub const DATA_BUDGET: u64 = 4 * 1024 * 1024;
pub const SNAPSHOT_BUDGET: u64 = 2 * 1024 * 1024;
pub const MAX_HANDLES: u32 = 65_536;
pub const MAX_STORAGES: usize = 32;
pub const DEVICE_INFO_BYTES: u32 = 8192;
pub const STORAGE_INFO_BYTES: u32 = 8192;
pub const OBJECT_INFO_BYTES: u32 = 4096;
pub const PAGE_RECORDS: usize = 2;
pub const PAGE_BYTES: usize = 8192;
pub const RECORD_BYTES: usize = 4096;
pub const CHUNK: usize = 4096;
/// The largest object: what one data container can frame.
pub const MAX_OBJECT: u64 = u32::MAX as u64 - ptp::HEADER as u64;
pub const CURSOR_IDLE_TICKS: u64 = 6000;
pub const TRANSFER_IDLE_TICKS: u64 = 3000;
/// Cancellation and session recovery, together.
pub const RECOVERY_TICKS: u64 = 200;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// A hard limit: a count, the budget, the snapshot budget.
	Exhausted,
	/// An identity issued to another client context.
	Denied,
	/// Another transaction on the device is completing.
	Again,
	/// An identity whose epoch has moved on.
	Stale,
	/// No such device, storage or object.
	NotFound,
	Invalid,
	Unsupported,
}

// ------------------------------------------------------------------ the budget

/// The one aggregate data budget, and the part of it snapshots may hold.
#[derive(Default, Debug)]
pub struct Budget {
	used: u64,
	snapshots: u64,
}

impl Budget {
	pub fn used(&self) -> u64 {
		self.used
	}

	pub fn snapshots(&self) -> u64 {
		self.snapshots
	}

	pub fn charge(&mut self, bytes: u64) -> Result<(), Refusal> {
		if self.used + bytes > DATA_BUDGET {
			return Err(Refusal::Exhausted);
		}
		self.used += bytes;
		Ok(())
	}

	pub fn release(&mut self, bytes: u64) {
		self.used -= bytes.min(self.used);
	}

	pub fn charge_snapshot(&mut self, bytes: u64) -> Result<(), Refusal> {
		if self.snapshots + bytes > SNAPSHOT_BUDGET {
			return Err(Refusal::Exhausted);
		}
		self.charge(bytes)?;
		self.snapshots += bytes;
		Ok(())
	}

	pub fn release_snapshot(&mut self, bytes: u64) {
		self.snapshots -= bytes.min(self.snapshots);
		self.release(bytes);
	}
}

// ------------------------------------------------------------------ admission

/// Who holds a cursor or a transfer: the client context and the device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Holder {
	pub client: u32,
	pub device: u32,
}

/// Eight of each at most, one of each per client and per device.
pub fn admit(held: &[Holder], wanted: Holder, most: usize) -> Result<(), Refusal> {
	if held.len() >= most || held.iter().any(|holder| holder.client == wanted.client || holder.device == wanted.device) {
		return Err(Refusal::Exhausted);
	}
	Ok(())
}

// ------------------------------------------------------------------ identity

/// What a device's identities are scoped to, besides its publication.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Epoch {
	pub attachment: u64,
	pub session: u32,
	pub content: u64,
}

/// A named epoch against the current one: the same, or stale.
pub fn scoped(current: Epoch, named: Epoch) -> Result<(), Refusal> {
	if current == named { Ok(()) } else { Err(Refusal::Stale) }
}

/// Everything a device identity names: its publication, the epoch, the service incarnation, and the client
/// context it was issued to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Named {
	pub slot: u32,
	pub generation: u32,
	pub binding: u64,
	pub epoch: Epoch,
	pub incarnation: u64,
	pub context: u32,
}

/// A named device against one this service holds, for the client context asking. Another publication is
/// another device; another context's identity is not this client's to use; anything older is stale. All of
/// it is decided before anything is sent to a device.
pub fn resolve(current: &Named, named: &Named, context: u32) -> Result<(), Refusal> {
	if (current.slot, current.generation, current.binding) != (named.slot, named.generation, named.binding) {
		return Err(Refusal::NotFound);
	}
	if named.context != context {
		return Err(Refusal::Denied);
	}
	if named.incarnation != current.incarnation {
		return Err(Refusal::Stale);
	}
	scoped(current.epoch, named.epoch)
}

// ------------------------------------------------------------------ the link to one device

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
	/// Waiting for the transport's attach.
	Attaching,
	/// OpenSession sent, under a session ID not used before on this device.
	Opening,
	/// Reading what the device is: its DeviceInfo, its storages.
	Surveying,
	Ready,
	/// A transaction is in progress.
	Busy,
	/// The transaction in progress is being cancelled.
	Cancelling,
	/// The device is being reset.
	Resetting,
	/// Recovery failed or ran out of time: nothing is asked of it until it is withdrawn.
	Unavailable,
}

/// One device's side of the service: what it is doing, the epoch its identities carry, the session and its
/// transaction IDs, and how long recovery has had.
#[derive(Debug)]
pub struct Link {
	pub phase: Phase,
	pub epoch: Epoch,
	pub session: Session,
	sessions: u32,
	recovering_since: Option<u64>,
}

impl Default for Link {
	fn default() -> Self {
		Self::new()
	}
}

impl Link {
	pub fn new() -> Link {
		Link { phase: Phase::Attaching, epoch: Epoch { attachment: 0, session: 0, content: 1 }, session: Session::new(0), sessions: 0, recovering_since: None }
	}

	/// Attached, or reset: a new attachment, and a session ID this device has not had.
	pub fn attached(&mut self, attachment: u64) -> Result<u32, Refusal> {
		if attachment == 0 || attachment == self.epoch.attachment {
			self.phase = Phase::Unavailable;
			return Err(Refusal::Invalid);
		}
		self.sessions += 1;
		self.epoch.attachment = attachment;
		self.epoch.session = self.sessions;
		self.epoch.content += 1;
		self.session = Session::new(self.sessions);
		self.phase = Phase::Opening;
		Ok(self.sessions)
	}

	/// OpenSession answered: survey the device, or recover it.
	pub fn opened(&mut self, success: bool) -> bool {
		if success {
			self.phase = Phase::Surveying;
		}
		success
	}

	/// The survey is done: the device serves clients, and recovery, if it was one, is over.
	pub fn surveyed(&mut self) {
		self.phase = Phase::Ready;
		self.recovering_since = None;
	}

	/// One transaction at a time: a transaction ID, or `again` while another is completing.
	pub fn begin(&mut self) -> Result<u32, Refusal> {
		match self.phase {
			Phase::Ready => {
				self.phase = Phase::Busy;
				Ok(self.session.transaction())
			}
			Phase::Unavailable => Err(Refusal::Unsupported),
			_ => Err(Refusal::Again),
		}
	}

	/// An internal transaction during the survey, which does not leave it.
	pub fn survey_transaction(&mut self) -> u32 {
		self.session.transaction()
	}

	pub fn end(&mut self) {
		if self.phase == Phase::Busy {
			self.phase = Phase::Ready;
		}
	}

	/// The transaction is abandoned - a client cancelled, left, or asked for more than is admitted. The
	/// device is cancelled first; recovery's two seconds start now.
	pub fn cancel(&mut self, now: u64) {
		self.phase = Phase::Cancelling;
		self.recovering_since.get_or_insert(now);
	}

	/// The cancel answered: back in step, or out of step and reset.
	pub fn cancelled(&mut self, success: bool, now: u64) {
		if success {
			self.phase = Phase::Ready;
			self.recovering_since = None;
		} else {
			self.reset(now);
		}
	}

	/// The device is out of step - a malformed stream, a command whose sending is uncertain, a lost session:
	/// reset it. Every identity from before is stale once it attaches again.
	pub fn reset(&mut self, now: u64) {
		self.phase = Phase::Resetting;
		self.recovering_since.get_or_insert(now);
		self.epoch.content += 1;
	}

	/// Recovery failed.
	pub fn fail(&mut self) {
		self.phase = Phase::Unavailable;
		self.recovering_since = None;
	}

	/// Recovery has had its two seconds.
	pub fn expired(&self, now: u64) -> bool {
		self.recovering_since.is_some_and(|since| now >= since + RECOVERY_TICKS)
	}

	pub fn deadline(&self) -> Option<u64> {
		self.recovering_since.map(|since| since + RECOVERY_TICKS)
	}

	/// The device reported a change, or events were lost: every object and cursor named before is stale.
	pub fn changed(&mut self) {
		self.epoch.content += 1;
	}
}

/// What an event means for the device's identities.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Change {
	/// Nothing this service's identities depend on.
	None,
	/// Objects or storages changed: the content epoch moves on.
	Content,
	/// The device lost its session: it has to be recovered.
	Session,
	/// The device cancelled the transaction in progress.
	Cancelled,
}

pub fn change(event: &ptp::Event) -> Change {
	match event.code {
		ptp::OBJECT_ADDED | ptp::OBJECT_REMOVED | ptp::STORE_ADDED | ptp::STORE_REMOVED | ptp::OBJECT_INFO_CHANGED | ptp::DEVICE_INFO_CHANGED | ptp::STORAGE_INFO_CHANGED | ptp::STORE_FULL | ptp::UNREPORTED_STATUS => Change::Content,
		ptp::DEVICE_RESET => Change::Session,
		ptp::CANCEL_TRANSACTION => Change::Cancelled,
		_ => Change::None,
	}
}

/// A session's transaction IDs: from 1, never 0 and never the all-ones value.
#[derive(Debug)]
pub struct Session {
	pub id: u32,
	next: u32,
}

impl Session {
	pub fn new(id: u32) -> Session {
		Session { id, next: 1 }
	}

	pub fn transaction(&mut self) -> u32 {
		let id = self.next;
		self.next = match self.next.wrapping_add(1) {
			0 | u32::MAX => 1,
			next => next,
		};
		id
	}
}

// ------------------------------------------------------------------ the snapshot

/// A GetObjectHandles data phase, taken as it arrives.
#[derive(Debug)]
pub struct Snapshot {
	// The payload length the container declared.
	declared: u32,
	head: [u8; 4],
	have_head: usize,
	partial: [u8; 4],
	have_partial: usize,
	ids: Vec<u32>,
	// What the snapshot was charged, which its holder releases.
	pub charged: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SnapshotEnd {
	/// More objects than one enumeration admits: their count, by the container's own arithmetic.
	OverLimit(u32),
	/// A count that disagrees with the container's length, or payload past it.
	Corrupt,
	Exhausted,
}

/// The payload limit to hand the stream parser: the count and 65 536 IDs. A longer container is refused from
/// its header and reported as over the limit.
pub const HANDLES_LIMIT: u32 = 4 + 4 * MAX_HANDLES;

/// What an over-long container's header says the count is.
pub fn declared_count(payload: u32) -> u32 {
	payload.saturating_sub(4) / 4
}

impl Snapshot {
	/// From the data container's header. Its length must be a count and whole IDs.
	pub fn new(declared: u32) -> Result<Snapshot, SnapshotEnd> {
		if declared < 4 || (declared - 4) % 4 != 0 {
			return Err(SnapshotEnd::Corrupt);
		}
		if declared > HANDLES_LIMIT {
			return Err(SnapshotEnd::OverLimit(declared_count(declared)));
		}
		Ok(Snapshot { declared, head: [0; 4], have_head: 0, partial: [0; 4], have_partial: 0, ids: Vec::new(), charged: 0 })
	}

	/// Payload bytes. The count is checked against the container's length before the IDs are reserved, and
	/// the reservation is charged first.
	pub fn feed(&mut self, mut bytes: &[u8], budget: &mut Budget) -> Result<(), SnapshotEnd> {
		if self.have_head < 4 {
			let take = bytes.len().min(4 - self.have_head);
			self.head[self.have_head..self.have_head + take].copy_from_slice(&bytes[..take]);
			self.have_head += take;
			bytes = &bytes[take..];
			if self.have_head < 4 {
				return Ok(());
			}
			let count = u32::from_le_bytes(self.head);
			if count != declared_count(self.declared) {
				return Err(SnapshotEnd::Corrupt);
			}
			let bytes_needed = u64::from(count) * 4;
			budget.charge_snapshot(bytes_needed).map_err(|_| SnapshotEnd::Exhausted)?;
			self.charged = bytes_needed;
			if self.ids.try_reserve_exact(count as usize).is_err() {
				return Err(SnapshotEnd::Exhausted);
			}
		}
		for byte in bytes {
			self.partial[self.have_partial] = *byte;
			self.have_partial += 1;
			if self.have_partial == 4 {
				if self.ids.len() == self.ids.capacity() {
					return Err(SnapshotEnd::Corrupt);
				}
				self.ids.push(u32::from_le_bytes(self.partial));
				self.have_partial = 0;
			}
		}
		Ok(())
	}

	/// The whole list, once the data phase is over. The final response is the caller's to have checked.
	pub fn finish(self) -> Result<(Vec<u32>, u64), SnapshotEnd> {
		if self.have_head < 4 || self.have_partial != 0 || self.ids.len() as u64 * 4 + 4 != u64::from(self.declared) {
			return Err(SnapshotEnd::Corrupt);
		}
		Ok((self.ids, self.charged))
	}
}

// ------------------------------------------------------------------ cursors and pages

/// A snapshot being paged: its handles, where the next page starts, and when it was last asked for.
#[derive(Debug)]
pub struct Cursor {
	pub holder: Holder,
	pub epoch: Epoch,
	pub storage: u32,
	handles: Vec<u32>,
	next: usize,
	pub last_used: u64,
	pub charged: u64,
}

impl Cursor {
	pub fn new(holder: Holder, epoch: Epoch, storage: u32, handles: Vec<u32>, charged: u64, now: u64) -> Cursor {
		Cursor { holder, epoch, storage, handles, next: 0, last_used: now, charged }
	}

	pub fn count(&self) -> u32 {
		self.handles.len() as u32
	}

	pub fn remaining(&self) -> u32 {
		(self.handles.len() - self.next) as u32
	}

	/// The handles the next page reads metadata for: two at most, and each page at most two reads.
	pub fn upcoming(&self) -> &[u32] {
		&self.handles[self.next..(self.next + PAGE_RECORDS).min(self.handles.len())]
	}

	/// The page was answered with `count` entries.
	pub fn advance(&mut self, count: usize, now: u64) {
		self.next = (self.next + count).min(self.handles.len());
		self.last_used = now;
	}

	pub fn expired(&self, now: u64) -> bool {
		now >= self.last_used + CURSOR_IDLE_TICKS
	}

	pub fn deadline(&self) -> u64 {
		self.last_used + CURSOR_IDLE_TICKS
	}
}

/// How many of a page's encoded entries fit: at most two, each within the record budget or replaced by its
/// refusal, and the whole page within 8192 bytes with its envelope. At least one always does - a refusal is
/// small - so a cursor never stops moving.
pub fn fits(envelope: usize, entries: &[usize]) -> usize {
	let mut total = envelope;
	let mut count = 0;
	for &size in entries.iter().take(PAGE_RECORDS) {
		if count > 0 && total + size > PAGE_BYTES {
			break;
		}
		total += size;
		count += 1;
	}
	count
}

// ------------------------------------------------------------------ transfers

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
	Opening,
	Reading,
	Complete,
	Partial,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cause {
	Removed,
	TimedOut,
	Cancelled,
	Corrupt,
	Stale,
	DeviceError,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Status {
	pub state: State,
	pub expected: u64,
	pub delivered: u64,
	pub cause: Option<Cause>,
}

/// One object's GetObject, from the command to its ending.
#[derive(Debug)]
pub struct Transfer {
	pub holder: Holder,
	pub epoch: Epoch,
	pub handle: u32,
	pub revision: u32,
	pub expected: u64,
	pub state: State,
	pub delivered: u64,
	pub cause: Option<Cause>,
	// The data container's declared payload, once its header arrived.
	framed: bool,
	// When the client last asked for bytes, and when the device last delivered some.
	pub demand_at: u64,
	pub progress_at: u64,
	// A read is waiting for bytes.
	pub reading: bool,
	// The response arrived: whether it was the matching success.
	answered: Option<bool>,
}

impl Transfer {
	/// A size the metadata gave, or refused: the sentinel is no size, and nothing past what one container
	/// frames is admitted.
	pub fn new(holder: Holder, epoch: Epoch, handle: u32, revision: u32, size: Option<u32>, now: u64) -> Result<Transfer, Refusal> {
		let expected = u64::from(size.ok_or(Refusal::Unsupported)?);
		if expected > MAX_OBJECT {
			return Err(Refusal::Unsupported);
		}
		Ok(Transfer { holder, epoch, handle, revision, expected, state: State::Opening, delivered: 0, cause: None, framed: false, demand_at: now, progress_at: now, reading: false, answered: None })
	}

	pub fn status(&self) -> Status {
		Status { state: self.state, expected: self.expected, delivered: self.delivered, cause: self.cause }
	}

	pub fn terminal(&self) -> bool {
		matches!(self.state, State::Complete | State::Partial)
	}

	/// End it: partial, with the count and the cause. A transfer already over keeps its ending.
	pub fn fail(&mut self, cause: Cause) {
		if !self.terminal() {
			self.state = State::Partial;
			self.cause = Some(cause);
			self.reading = false;
		}
	}

	/// The data container's header: exactly the expected length, or the transfer is corrupt.
	pub fn framed(&mut self, payload: u32, now: u64) {
		if self.terminal() {
			return;
		}
		if u64::from(payload) != self.expected || self.framed {
			self.fail(Cause::Corrupt);
			return;
		}
		self.framed = true;
		self.state = State::Reading;
		self.progress_at = now;
	}

	/// Payload bytes arrived: their offset in the object. Past the expected length is corruption.
	pub fn delivered(&mut self, count: usize, now: u64) -> Option<u64> {
		if self.terminal() || !self.framed || self.delivered + count as u64 > self.expected {
			self.fail(Cause::Corrupt);
			return None;
		}
		let offset = self.delivered;
		self.delivered += count as u64;
		self.progress_at = now;
		Some(offset)
	}

	/// The final response, and whether it is this transaction's success.
	pub fn answered(&mut self, success: bool) {
		self.answered = Some(success);
	}

	/// Settle the ending once nothing more will be delivered to the client: complete only with every byte,
	/// the framing, and the matching successful response. A zero-byte object needs the response too, framed
	/// or not.
	pub fn settle(&mut self) -> bool {
		let Some(success) = self.answered else { return false };
		if self.terminal() {
			return true;
		}
		if success && self.delivered == self.expected && (self.framed || self.expected == 0) {
			self.state = State::Complete;
			self.reading = false;
		} else {
			self.fail(if success { Cause::Corrupt } else { Cause::DeviceError });
		}
		true
	}

	/// The client asked for bytes.
	pub fn demanded(&mut self, now: u64) {
		self.demand_at = now;
		self.reading = true;
	}

	/// Thirty seconds without the device delivering while a read waited, or without the client asking.
	pub fn timed_out(&self, now: u64) -> bool {
		!self.terminal() && if self.reading { now >= self.progress_at.max(self.demand_at) + TRANSFER_IDLE_TICKS } else { now >= self.demand_at + TRANSFER_IDLE_TICKS }
	}

	pub fn deadline(&self) -> Option<u64> {
		if self.terminal() {
			return None;
		}
		Some(if self.reading { self.progress_at.max(self.demand_at) } else { self.demand_at } + TRANSFER_IDLE_TICKS)
	}
}

#[cfg(test)]
mod tests;
