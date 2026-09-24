// MediaImportService - objects read from a camera's Picture Transfer Protocol storage, for components
// PermissionManager granted `media-import`.
//
// A SOURCE AND NOTHING ELSE. It consumes every `ptp-transport` publication through a catalogue connection
// minted for that kind alone, and is each one's only consumer. It speaks the read-only subset of PTP - device
// and storage information, one bounded handle snapshot per enumeration, object metadata by page, and an
// object's bytes in order - and it holds no storage to write into, no command to forward, no delete and no
// capture. A client decides where imported bytes go, and publishes them only after a complete ending.
//
// THE PROTOCOL IS VALIDATED HERE, ONCE. The transport moves bytes; every container, transaction ID, session,
// dataset and data/response order is checked by `service_logic::ptp`, and every admission, charge, identity
// and ending is decided by `service_logic::media_import`. Nothing a device declares is allocated before it is
// checked against a bound.
//
// NOTHING WAITS ON A DEVICE. Every transport request goes out without blocking, one outstanding per device, and
// is answered in the loop; a device answers one transaction at a time, and a client asking for a busy one hears
// `again`. A transaction that is abandoned is cancelled; a device that is out of step is reset; either has two
// seconds to come back in step before the device is unavailable. A reset, a lost session, a withdrawal or a
// reported change stales every identity issued before it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{Error, ImportCause, ImportChunk, ImportCursorGrant, ImportDevice, ImportDeviceId, ImportEntry, ImportEnumeration, ImportInterpretation, ImportLimits, ImportObject, ImportObjectId, ImportPage, ImportParentId, ImportRead, ImportStorage, ImportStorageId, ImportTime, ImportTransferState, ImportTransferStatus, ProviderInfo, ProviderKind, PtpAttach, PtpEvent, import_cursor, import_transfer, media_import, provider_catalogue, ptp_transport};
use rt::*;
use service_logic::media_import as mi;
use service_logic::ptp;
use wire::{Handles, Reader, Sink, Transport, TransportError};

include!(concat!(env!("OUT_DIR"), "/roles_media_import_service.rs"));

const VERSION: u32 = 1;
// How long a transport request may go unanswered: two seconds.
const ANSWER_TICKS: u64 = 200;
// After a pull answered `again`: the next tick.
const RETRY_TICKS: u64 = 1;
const OPEN_TICKS: u64 = 100;
// THE LARGEST FRAMES THIS SERVICE READS OR ANSWERS, ENVELOPES INCLUDED: a pull's answer (correlation, tag, list
// length and a 4096-byte chunk), a page (8192 with its envelope, by construction), and the storage list - 32
// records of a 48-byte device identity, the storage ID, three codes, two sizes and two 64-byte strings.
const PULL_ANSWER: usize = 4 + 1 + 2 + mi::CHUNK;
const STORAGE_RECORD: usize = 48 + 4 + 3 * 2 + 2 * 8 + 2 * (2 + 64);
const STORAGES_ANSWER: usize = 4 + 1 + 2 + mi::MAX_STORAGES * STORAGE_RECORD;
const BUF_BYTES: usize = 8192;
const _: () = assert!(BUF_BYTES >= PULL_ANSWER && BUF_BYTES >= mi::PAGE_BYTES && BUF_BYTES >= STORAGES_ANSWER);
// WHAT EVERY ADOPTED DEVICE HOLDS FOR ITS LIFE, charged when it is adopted: a command container, eight event
// containers and the one pending reply it may owe.
const DEVICE_BUFFERS: u64 = ptp::MAX_SHORT as u64 + 8 * 32 + 64;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ask {
	Attach,
	Events,
	Command,
	Pull,
	Cancel,
	Reset,
}

// What a transaction is for, and whom it answers.
enum Job {
	// THE SURVEY, in order: the session, what the device is, its storages.
	OpenSession,
	DeviceInfo,
	StorageIds,
	StorageInfo { ids: Vec<u32>, at: usize, found: Vec<(u32, ptp::StorageInfo)> },
	// A client's enumeration, and the snapshot it is taking.
	Enumerate { client: u32, chan: u64, corr: u32, storage: u32, snapshot: Option<mi::Snapshot>, end: Option<mi::SnapshotEnd> },
	// A page's metadata reads.
	Page { cursor: u32, corr: u32, handles: Vec<u32>, at: usize, entries: Vec<ImportEntry>, sizes: Vec<usize> },
	// A read's revalidation of the object it names.
	Revalidate { client: u32, chan: u64, corr: u32, object: ImportObjectId },
	// The object itself.
	Object { transfer: u32 },
}

struct Op {
	inbound: ptp::Inbound,
	job: Job,
	// When the device last moved this transaction on: a command accepted, bytes pulled.
	progress_at: u64,
	// A dataset as it arrives, charged as its container declared it.
	data: Vec<u8>,
	charged: u64,
	response: Option<ptp::Response>,
}

struct Device {
	key: u32,
	info: ProviderInfo,
	chan: u64,
	events: u64,
	link: mi::Link,
	about: Option<ptp::DeviceInfo>,
	storages: Vec<(u32, ptp::StorageInfo)>,
	// The storages are as the device last reported them; false after a change until surveyed again.
	fresh: bool,
	asked: Option<(u32, Ask, u64)>,
	op: Option<Op>,
	// Pull again at this tick, after `again`.
	retry_at: Option<u64>,
	next_corr: u32,
}

struct Client {
	id: u32,
	chan: u64,
}

struct CursorEntry {
	id: u32,
	chan: u64,
	cursor: mi::Cursor,
	// A page read in progress: the device transaction answers it.
	paging: bool,
}

struct TransferEntry {
	id: u32,
	chan: u64,
	object: ImportObjectId,
	transfer: mi::Transfer,
	// The read waiting for bytes: its correlation.
	pending: Option<u32>,
	// A chunk that arrived with the final response, delivered before the ending.
	held: Option<(u64, Vec<u8>)>,
}

struct Service {
	incarnation: u64,
	catalogue: u64,
	devices: Vec<Device>,
	clients: Vec<Client>,
	cursors: Vec<CursorEntry>,
	transfers: Vec<TransferEntry>,
	budget: mi::Budget,
	next_key: u32,
	next_client: u32,
	next_id: u32,
}

// ------------------------------------------------------------------ the wire, by hand where it must be

struct Capture {
	bytes: Vec<u8>,
}

impl Transport for &mut Capture {
	fn call(&mut self, request: &[u8], _request_handles: &[u64], _reply_handles: &mut Handles, _deadline: u64) -> Result<Vec<u8>, TransportError> {
		self.bytes = request.to_vec();
		Err(TransportError::TimedOut)
	}
	fn discard_handles(&mut self, _handles: &[u64]) {}
}

fn captured(encode: impl FnOnce(&mut ptp_transport::Client<&mut Capture>), corr: u32) -> Option<Vec<u8>> {
	let mut capture = Capture { bytes: Vec::new() };
	encode(&mut ptp_transport::Client::new(&mut capture));
	if capture.bytes.len() < 6 {
		return None;
	}
	capture.bytes[2..6].copy_from_slice(&corr.to_le_bytes());
	Some(capture.bytes)
}

// An answer on a client's channel, without waiting: a client that does not read its answers loses them.
fn answer(chan: u64, bytes: &[u8], handles: &[u64]) {
	if !matches!(try_send_caps_outcome(chan, bytes, handles), SendOutcome::Delivered) {
		for &handle in handles {
			close(handle);
		}
	}
}

// A deferred answer: `[corr][tag][value | error]`, and the capabilities the value carries.
fn reply(chan: u64, corr: u32, result: Result<(), Error>, write: impl FnOnce(&mut wire::VecWriter) -> Option<()>) {
	let mut writer = wire::VecWriter::new();
	let encoded = (|| {
		writer.u32(corr)?;
		match result {
			Ok(()) => {
				writer.u8(1)?;
				write(&mut writer)
			}
			Err(error) => {
				writer.u8(0)?;
				error.write(&mut writer)
			}
		}
	})();
	let (bytes, handles) = writer.into_message();
	if encoded.is_some() {
		answer(chan, &bytes, handles.as_slice());
	} else {
		for &handle in handles.as_slice() {
			close(handle);
		}
	}
}

fn failed(chan: u64, corr: u32, error: Error) {
	reply(chan, corr, Err(error), |_| Some(()));
}

fn refused(refusal: mi::Refusal) -> Error {
	match refusal {
		mi::Refusal::Exhausted => Error::Exhausted,
		mi::Refusal::Denied => Error::Denied,
		mi::Refusal::Again => Error::Again,
		mi::Refusal::Stale => Error::Stale,
		mi::Refusal::NotFound => Error::NotFound,
		mi::Refusal::Invalid => Error::Invalid,
		mi::Refusal::Unsupported => Error::Unsupported,
	}
}

fn same(a: &ProviderInfo, b: &ProviderInfo) -> bool {
	a.slot == b.slot && a.provider_generation == b.provider_generation && a.binding_generation == b.binding_generation
}

fn cause(cause: mi::Cause) -> ImportCause {
	match cause {
		mi::Cause::Removed => ImportCause::Removed,
		mi::Cause::TimedOut => ImportCause::TimedOut,
		mi::Cause::Cancelled => ImportCause::Cancelled,
		mi::Cause::Corrupt => ImportCause::Corrupt,
		mi::Cause::Stale => ImportCause::Stale,
		mi::Cause::DeviceError => ImportCause::DeviceError,
	}
}

fn transfer_status(entry: &TransferEntry) -> ImportTransferStatus {
	let status = entry.transfer.status();
	ImportTransferStatus {
		state: match status.state {
			mi::State::Opening => ImportTransferState::Opening,
			mi::State::Reading => ImportTransferState::Reading,
			mi::State::Complete => ImportTransferState::Complete,
			mi::State::Partial => ImportTransferState::Partial,
		},
		object: entry.object.clone(),
		expected: status.expected,
		delivered: status.delivered,
		cause: status.cause.map(cause),
	}
}

impl Service {
	fn at(&self, key: u32) -> Option<usize> {
		self.devices.iter().position(|device| device.key == key)
	}

	fn device_id(&self, device: &Device, context: u32) -> ImportDeviceId {
		ImportDeviceId { slot: device.info.slot, generation: device.info.provider_generation, binding_generation: device.info.binding_generation, attachment: device.link.epoch.attachment, session: device.link.epoch.session, content_epoch: device.link.epoch.content, incarnation: self.incarnation, context }
	}

	fn named(&self, device: &Device, context: u32) -> mi::Named {
		mi::Named { slot: device.info.slot, generation: device.info.provider_generation, binding: device.info.binding_generation, epoch: device.link.epoch, incarnation: self.incarnation, context }
	}

	// A device identity a client named, resolved for that client: the device, or why not. Decided before
	// anything is sent to a device.
	fn resolve(&self, id: &ImportDeviceId, context: u32) -> Result<usize, Error> {
		let named = mi::Named { slot: id.slot, generation: id.generation, binding: id.binding_generation, epoch: mi::Epoch { attachment: id.attachment, session: id.session, content: id.content_epoch }, incarnation: id.incarnation, context: id.context };
		let at = self.devices.iter().position(|device| (device.info.slot, device.info.provider_generation, device.info.binding_generation) == (id.slot, id.generation, id.binding_generation)).ok_or(Error::NotFound)?;
		let device = &self.devices[at];
		mi::resolve(&self.named(device, context), &named, context).map_err(refused)?;
		if device.link.phase == mi::Phase::Unavailable {
			return Err(Error::Io);
		}
		match device.about.as_ref() {
			Some(about) if about.usable() => Ok(at),
			Some(_) => Err(Error::Unsupported),
			None => Err(Error::Again),
		}
	}

	fn corr(&mut self, at: usize) -> u32 {
		let device = &mut self.devices[at];
		let corr = device.next_corr;
		device.next_corr = device.next_corr.wrapping_add(1).max(1);
		corr
	}

	// One transport request, without waiting; it replaces whatever was outstanding.
	fn ask(&mut self, at: usize, ask: Ask, encode: impl FnOnce(&mut ptp_transport::Client<&mut Capture>)) -> bool {
		let corr = self.corr(at);
		let device = &mut self.devices[at];
		let sent = captured(encode, corr).is_some_and(|bytes| try_send(device.chan, &bytes, 0));
		device.asked = if sent { Some((corr, ask, clock())) } else { None };
		sent
	}

	// ONE COMMAND, for a transaction that has begun. A command that could not be sent was never sent.
	fn command(&mut self, at: usize, operation: u16, transaction: u32, params: &[u32], data: bool, limit: u32, job: Job) {
		let attachment = self.devices[at].link.epoch.attachment;
		let Some(container) = ptp::command(operation, transaction, params) else { return };
		self.devices[at].op = Some(Op { inbound: ptp::Inbound::new(operation, transaction, data, limit), job, progress_at: clock(), data: Vec::new(), charged: 0, response: None });
		if !self.ask(at, Ask::Command, |client| {
			let _ = client.command(&attachment, &container);
		}) {
			self.lose(self.devices[at].key, b"a command could not be sent to it");
		}
	}

	fn pull(&mut self, at: usize) {
		let attachment = self.devices[at].link.epoch.attachment;
		self.devices[at].retry_at = None;
		if !self.ask(at, Ask::Pull, |client| {
			let _ = client.pull(&attachment, &(mi::CHUNK as u16));
		}) {
			self.lose(self.devices[at].key, b"a pull could not be sent to it");
		}
	}

	fn adopt(&mut self, info: ProviderInfo) {
		if self.devices.iter().any(|device| same(&device.info, &info)) {
			return;
		}
		if self.devices.len() >= mi::MAX_PROVIDERS || self.budget.charge(DEVICE_BUFFERS).is_err() {
			print(b"MediaImportService: a camera was refused: this service holds eight, or has no budget left (resource exhausted)\n");
			return;
		}
		let Some(Ok(chan)) = provider_catalogue::Client::with_deadline(ChannelTransport { chan: self.catalogue }, clock() + OPEN_TICKS).open(&info) else {
			self.budget.release(DEVICE_BUFFERS);
			print(b"MediaImportService: a published camera could not be opened\n");
			return;
		};
		let key = self.next_key;
		self.next_key = self.next_key.wrapping_add(1).max(1);
		self.devices.push(Device { key, info, chan, events: 0, link: mi::Link::new(), about: None, storages: Vec::new(), fresh: false, asked: None, op: None, retry_at: None, next_corr: 1 });
		let at = self.devices.len() - 1;
		if !self.ask(at, Ask::Attach, |client| {
			let _ = client.attach(&VERSION);
		}) {
			self.lose(key, b"its attach could not be sent");
		}
	}

	// A device is gone. Every transfer on it ends as removed with what was delivered, every cursor on it closes,
	// and an answer it owed is told it is closed. A replacement is another device.
	fn lose(&mut self, key: u32, why: &[u8]) {
		let Some(at) = self.at(key) else { return };
		let mut device = self.devices.remove(at);
		self.budget.release(DEVICE_BUFFERS);
		close(device.chan);
		if device.events != 0 {
			close(device.events);
		}
		if let Some(op) = device.op.take() {
			self.budget.release(op.charged);
			self.orphan(op.job, Error::Closed);
		}
		for entry in self.transfers.iter_mut().filter(|entry| entry.transfer.holder.device == key) {
			entry.transfer.fail(mi::Cause::Removed);
		}
		self.settle_transfers();
		let gone: Vec<u32> = self.cursors.iter().filter(|entry| entry.cursor.holder.device == key).map(|entry| entry.id).collect();
		for id in gone {
			self.drop_cursor(id);
		}
		print(b"MediaImportService: a camera is gone: ");
		print(why);
		print(b"\n");
	}

	// A job that will not finish: whoever it owed an answer is told so.
	fn orphan(&mut self, job: Job, error: Error) {
		match job {
			Job::Enumerate { chan, corr, snapshot, .. } => {
				if let Some(snapshot) = snapshot {
					self.budget.release_snapshot(snapshot.charged);
				}
				failed(chan, corr, error);
			}
			Job::Page { cursor, corr, .. } => {
				if let Some(entry) = self.cursors.iter_mut().find(|entry| entry.id == cursor) {
					entry.paging = false;
					failed(entry.chan, corr, error);
				}
				self.budget.release(mi::PAGE_BYTES as u64);
			}
			Job::Revalidate { chan, corr, .. } => failed(chan, corr, error),
			Job::Object { transfer } => {
				if let Some(entry) = self.transfers.iter_mut().find(|entry| entry.id == transfer) {
					entry.transfer.fail(match error {
						Error::Closed => mi::Cause::Removed,
						Error::Corrupt => mi::Cause::Corrupt,
						Error::TimedOut => mi::Cause::TimedOut,
						Error::Cancelled => mi::Cause::Cancelled,
						Error::Stale => mi::Cause::Stale,
						_ => mi::Cause::DeviceError,
					});
				}
				self.settle_transfers();
			}
			_ => {}
		}
	}

	// Answer every read waiting on a transfer that has ended.
	fn settle_transfers(&mut self) {
		for entry in self.transfers.iter_mut() {
			if entry.transfer.terminal()
				&& entry.held.is_none()
				&& let Some(corr) = entry.pending.take()
			{
				let status = transfer_status(entry);
				reply(entry.chan, corr, Ok(()), |w| ImportRead::End(status).write(w));
			}
		}
	}

	fn drop_cursor(&mut self, id: u32) {
		let Some(at) = self.cursors.iter().position(|entry| entry.id == id) else { return };
		let entry = self.cursors.remove(at);
		self.budget.release_snapshot(entry.cursor.charged);
		close(entry.chan);
	}

	fn drop_transfer(&mut self, id: u32) {
		let Some(at) = self.transfers.iter().position(|entry| entry.id == id) else { return };
		let entry = self.transfers.remove(at);
		self.budget.release(mi::CHUNK as u64);
		close(entry.chan);
	}

	// ABANDON the transaction in progress: the device is cancelled, and what the transaction was for ends.
	fn abandon(&mut self, at: usize, error: Error) {
		if let Some(op) = self.devices[at].op.take() {
			self.budget.release(op.charged);
			self.orphan(op.job, error);
		}
		let attachment = self.devices[at].link.epoch.attachment;
		self.devices[at].link.cancel(clock());
		self.devices[at].retry_at = None;
		if !self.ask(at, Ask::Cancel, |client| {
			let _ = client.cancel(&attachment);
		}) {
			self.recover(at, Error::Io);
		}
	}

	// RECOVER a device that is out of step: reset it, and stale everything named before.
	fn recover(&mut self, at: usize, error: Error) {
		if let Some(op) = self.devices[at].op.take() {
			self.budget.release(op.charged);
			self.orphan(op.job, error);
		}
		let key = self.devices[at].key;
		for entry in self.transfers.iter_mut().filter(|entry| entry.transfer.holder.device == key) {
			entry.transfer.fail(mi::Cause::DeviceError);
		}
		self.settle_transfers();
		let attachment = self.devices[at].link.epoch.attachment;
		self.devices[at].link.reset(clock());
		self.devices[at].retry_at = None;
		if !self.ask(at, Ask::Reset, |client| {
			let _ = client.reset(&attachment);
		}) {
			self.unavailable(at);
		}
	}

	fn unavailable(&mut self, at: usize) {
		if let Some(op) = self.devices[at].op.take() {
			self.budget.release(op.charged);
			self.orphan(op.job, Error::Io);
		}
		self.devices[at].link.fail();
		self.devices[at].asked = None;
		self.devices[at].retry_at = None;
		print(b"MediaImportService: a camera could not be brought back in step - it is unavailable\n");
	}

	// A change the device reported, or events it could not deliver: the content epoch moves on, cursors and
	// objects named before are stale, a transfer in progress ends stale, and the storages are read again.
	fn changed(&mut self, at: usize) {
		self.devices[at].link.changed();
		self.devices[at].fresh = false;
		let key = self.devices[at].key;
		let mut abandon = false;
		for entry in self.transfers.iter_mut().filter(|entry| entry.transfer.holder.device == key && !entry.transfer.terminal()) {
			entry.transfer.fail(mi::Cause::Stale);
			abandon = true;
		}
		self.settle_transfers();
		if abandon && matches!(self.devices[at].op.as_ref().map(|op| &op.job), Some(Job::Object { .. })) {
			self.abandon(at, Error::Stale);
		}
	}

	// ------------------------------------------------------------------ the survey

	fn attached(&mut self, at: usize, attach: Option<PtpAttach>) {
		let Some(attach) = attach.filter(|attach| attach.version == VERSION) else {
			self.unavailable(at);
			return;
		};
		let Ok(session) = self.devices[at].link.attached(attach.attachment) else {
			self.unavailable(at);
			return;
		};
		if self.devices[at].events == 0 {
			if !self.ask(at, Ask::Events, |client| {
				let _ = client.events();
			}) {
				self.unavailable(at);
			}
			return;
		}
		self.open_session(at, session);
	}

	// OPENSESSION goes out as transaction zero, under a session ID this device has not had.
	fn open_session(&mut self, at: usize, session: u32) {
		self.command(at, ptp::OPEN_SESSION, 0, &[session], false, 0, Job::OpenSession);
	}

	// The survey's next step after one ends; the last one makes the device ready.
	fn survey(&mut self, at: usize, next: Job) {
		let transaction = self.devices[at].link.survey_transaction();
		match next {
			Job::DeviceInfo => self.command(at, ptp::GET_DEVICE_INFO, transaction, &[], true, mi::DEVICE_INFO_BYTES, Job::DeviceInfo),
			Job::StorageIds => self.command(at, ptp::GET_STORAGE_IDS, transaction, &[], true, 4 + 4 * mi::MAX_STORAGES as u32, Job::StorageIds),
			Job::StorageInfo { ids, at: index, found } => {
				let id = ids[index];
				self.command(at, ptp::GET_STORAGE_INFO, transaction, &[id], true, mi::STORAGE_INFO_BYTES, Job::StorageInfo { ids, at: index, found });
			}
			_ => {}
		}
	}

	// Storages again, after a change: a transaction like any other, when the device is free.
	fn refresh(&mut self, at: usize) {
		if self.devices[at].fresh || self.devices[at].link.phase != mi::Phase::Ready || self.devices[at].about.as_ref().is_none_or(|about| !about.usable()) {
			return;
		}
		let Ok(transaction) = self.devices[at].link.begin() else { return };
		self.command(at, ptp::GET_STORAGE_IDS, transaction, &[], true, 4 + 4 * mi::MAX_STORAGES as u32, Job::StorageIds);
	}

	// ------------------------------------------------------------------ answers from a transport

	fn on_answer(&mut self, key: u32, buf: &mut [u8]) -> Result<(), &'static [u8]> {
		loop {
			let Some(at) = self.at(key) else { return Ok(()) };
			let (len, mut handles) = match try_recv_caps(self.devices[at].chan, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return Ok(()),
				PolledCaps::Closed => return Err(b"its connection closed"),
			};
			let mut reader = Reader::new(&buf[..len]);
			let asked = self.devices[at].asked;
			let Some((corr, ask, _)) = asked.filter(|(corr, _, _)| reader.u32() == Some(*corr)) else {
				for &handle in handles.as_slice() {
					close(handle);
				}
				continue;
			};
			let _ = corr;
			self.devices[at].asked = None;
			if ask == Ask::Events {
				let stream = handles.take_first();
				for &handle in handles.as_slice() {
					close(handle);
				}
				if stream == 0 {
					self.unavailable(at);
					continue;
				}
				self.devices[at].events = stream;
				let session = self.devices[at].link.epoch.session;
				self.open_session(at, session);
				continue;
			}
			for &handle in handles.as_slice() {
				close(handle);
			}
			let ok = reader.tag();
			match ask {
				Ask::Attach | Ask::Reset => {
					let attach = if ok == Some(true) { PtpAttach::read(&mut reader) } else { None };
					if ask == Ask::Reset && attach.is_some() {
						print(b"MediaImportService: a camera was reset\n");
					}
					self.attached(at, attach);
				}
				Ask::Cancel => {
					let success = ok == Some(true);
					self.devices[at].link.cancelled(success, clock());
					if !success {
						self.recover(at, Error::Io);
					}
				}
				Ask::Command => {
					// AN UNCERTAIN SEND ENDS THE SESSION: it may have left, so it is never sent again.
					if ok != Some(true) {
						self.recover(at, Error::Io);
						continue;
					}
					// The object's bytes are pulled on demand - now, if a read is already waiting; everything else
					// straight away.
					let waiting = match self.devices[at].op.as_ref().map(|op| &op.job) {
						Some(Job::Object { transfer }) => self.transfers.iter().any(|entry| entry.id == *transfer && entry.pending.is_some()),
						_ => true,
					};
					if waiting {
						self.pull(at);
					}
				}
				Ask::Pull => match ok {
					Some(true) => {
						let Some(bytes) = reader.bytes_lp() else { return Err(b"a pull answer did not decode") };
						let bytes = bytes.to_vec();
						self.pulled(at, &bytes);
					}
					Some(false) if Error::read(&mut reader) == Some(Error::Again) => {
						self.devices[at].retry_at = Some(clock() + RETRY_TICKS);
					}
					_ => self.recover(at, Error::Io),
				},
				Ask::Events => {}
			}
		}
	}

	// Bytes a pull returned, through the transaction's stream parser.
	fn pulled(&mut self, at: usize, bytes: &[u8]) {
		let Some(mut op) = self.devices[at].op.take() else { return };
		let now = clock();
		op.progress_at = now;
		let mut declared: Option<u32> = None;
		let mut payload: Vec<u8> = Vec::new();
		let mut response: Option<ptp::Response> = None;
		let fed = op.inbound.feed(bytes, &mut |piece| match piece {
			ptp::Piece::Data(length) => declared = Some(length),
			ptp::Piece::Payload(bytes) => payload.extend_from_slice(bytes),
			ptp::Piece::Response(answer) => response = Some(answer),
		});
		if let Err(fault) = fed {
			if let ptp::Fault::TooLarge(length) = fault {
				// MORE THAN A TRANSACTION ADMITS is an answer, not a fault: the transaction is cancelled, and the
				// device is not reset for it.
				match &mut op.job {
					// A handle list longer than an enumeration admits: its count, and nothing kept.
					Job::Enumerate { chan, corr, .. } => {
						let count = mi::declared_count(length);
						reply(*chan, *corr, Ok(()), |w| ImportEnumeration::OverLimit(count).write(w));
						op.job = Job::OpenSession;
					}
					// A record whose metadata is past its bound: refused by name in the page, never truncated.
					Job::Page { cursor, corr, handles, at: index, entries, sizes } => {
						let (cursor, corr) = (*cursor, *corr);
						let context = self.cursors.iter().find(|entry| entry.id == cursor).map_or(0, |entry| entry.cursor.holder.client);
						let id = ImportObjectId { device: self.device_id(&self.devices[at], context), handle: handles[*index], revision: 0 };
						let refusal = ImportEntry::Unrepresentable(id);
						sizes.push(refusal.encode_vec().map_or(usize::MAX, |bytes| bytes.len()));
						entries.push(refusal);
						let (entries, sizes) = (core::mem::take(entries), core::mem::take(sizes));
						op.job = Job::OpenSession;
						self.answer_page(cursor, corr, entries, sizes);
					}
					Job::Revalidate { chan, corr, .. } => {
						failed(*chan, *corr, Error::Unsupported);
						op.job = Job::OpenSession;
					}
					_ => {
						self.devices[at].op = Some(op);
						self.recover(at, Error::Corrupt);
						return;
					}
				}
				self.devices[at].op = Some(op);
				self.abandon(at, Error::Exhausted);
				return;
			}
			self.devices[at].op = Some(op);
			self.recover(at, Error::Corrupt);
			return;
		}
		op.response = op.response.or(response);
		match &mut op.job {
			Job::Object { transfer } => {
				let id = *transfer;
				let Some(entry) = self.transfers.iter_mut().find(|entry| entry.id == id) else {
					self.devices[at].op = Some(op);
					return;
				};
				if let Some(length) = declared {
					entry.transfer.framed(length, now);
				}
				if !payload.is_empty() {
					match entry.transfer.delivered(payload.len(), now) {
						Some(offset) => entry.held = Some((offset, payload)),
						None => entry.held = None,
					}
				}
				if let Some(response) = op.response {
					entry.transfer.answered(response.code == ptp::OK);
				}
				let over = op.response.is_some() || entry.transfer.terminal();
				self.devices[at].op = Some(op);
				self.deliver(id);
				if over {
					self.finish_object(at);
				} else if self.transfers.iter().any(|entry| entry.id == id && entry.pending.is_some()) {
					self.pull(at);
				}
			}
			_ => {
				if let Some(length) = declared {
					// CHARGED AS DECLARED, before a byte of it is kept.
					let charge = if matches!(op.job, Job::Enumerate { .. }) { 0 } else { u64::from(length) };
					if self.budget.charge(charge).is_err() {
						self.devices[at].op = Some(op);
						self.abandon(at, Error::Exhausted);
						return;
					}
					op.charged += charge;
					if let Job::Enumerate { snapshot, end, .. } = &mut op.job {
						match mi::Snapshot::new(length) {
							Ok(started) => *snapshot = Some(started),
							Err(finished) => *end = Some(finished),
						}
					}
				}
				if !payload.is_empty() {
					if let Job::Enumerate { snapshot, end, .. } = &mut op.job {
						if end.is_none()
							&& let Some(taking) = snapshot.as_mut()
							&& let Err(finished) = taking.feed(&payload, &mut self.budget)
						{
							*end = Some(finished);
						}
					} else {
						op.data.extend_from_slice(&payload);
					}
				}
				let ended = matches!(&op.job, Job::Enumerate { end: Some(_), .. });
				if ended {
					let Job::Enumerate { chan, corr, end, snapshot, .. } = &mut op.job else { return };
					let (chan, corr) = (*chan, *corr);
					if let Some(snapshot) = snapshot.take() {
						self.budget.release_snapshot(snapshot.charged);
					}
					let error = if *end == Some(mi::SnapshotEnd::Exhausted) { Error::Exhausted } else { Error::Corrupt };
					op.job = Job::OpenSession;
					self.devices[at].op = Some(op);
					failed(chan, corr, error);
					if error == Error::Corrupt {
						self.recover(at, Error::Corrupt);
					} else {
						self.abandon(at, Error::Exhausted);
					}
					return;
				}
				if op.response.is_some() {
					self.budget.release(op.charged);
					op.charged = 0;
					self.finished(at, op);
				} else {
					self.devices[at].op = Some(op);
					self.pull(at);
				}
			}
		}
	}

	// Hand a transfer its chunk, or its ending, if a read is waiting.
	fn deliver(&mut self, id: u32) {
		let Some(entry) = self.transfers.iter_mut().find(|entry| entry.id == id) else { return };
		let Some(corr) = entry.pending else { return };
		if let Some((offset, bytes)) = entry.held.take() {
			entry.pending = None;
			entry.transfer.reading = false;
			reply(entry.chan, corr, Ok(()), |w| ImportRead::Chunk(ImportChunk { offset, bytes }).write(w));
			return;
		}
		if entry.transfer.settle() {
			entry.pending = None;
			let status = transfer_status(entry);
			reply(entry.chan, corr, Ok(()), |w| ImportRead::End(status).write(w));
		}
	}

	// The object's transaction is over: its ending is settled, and the device is free.
	fn finish_object(&mut self, at: usize) {
		let Some(op) = self.devices[at].op.take() else { return };
		if let Job::Object { transfer } = op.job
			&& let Some(entry) = self.transfers.iter_mut().find(|entry| entry.id == transfer)
			&& entry.held.is_none()
		{
			// An ending with nothing held is settled now; one with a chunk still to hand over is settled when the
			// next read comes.
			entry.transfer.settle();
		}
		if op.response.is_none() {
			// OVER WITHOUT THE DEVICE'S FINAL WORD: the transfer has failed, and its transaction is cancelled.
			self.devices[at].op = Some(Op { job: Job::OpenSession, ..op });
			self.abandon(at, Error::Io);
			return;
		}
		self.devices[at].link.end();
		self.refresh(at);
	}

	// A transaction other than the object's ended with its response.
	fn finished(&mut self, at: usize, op: Op) {
		let response = op.response.expect("finished only once answered");
		let success = response.code == ptp::OK;
		match op.job {
			Job::OpenSession => {
				if !self.devices[at].link.opened(success) {
					self.recover(at, Error::Io);
					return;
				}
				self.survey(at, Job::DeviceInfo);
			}
			Job::DeviceInfo => match ptp::device_info(&op.data).filter(|_| success) {
				Some(about) => {
					let usable = about.usable();
					self.devices[at].about = Some(about);
					if usable {
						self.survey(at, Job::StorageIds);
					} else {
						// LISTED AND NOT USABLE: everything but the list is `unsupported`.
						self.devices[at].link.surveyed();
						print(b"MediaImportService: a camera does not offer the read-only subset - it is listed and not usable\n");
					}
				}
				None => self.recover(at, Error::Corrupt),
			},
			Job::StorageIds => match ptp::ids(&op.data, mi::MAX_STORAGES).filter(|_| success) {
				Some(ids) if ids.is_empty() => {
					self.devices[at].storages = Vec::new();
					self.surveyed(at);
				}
				Some(ids) => self.survey(at, Job::StorageInfo { ids, at: 0, found: Vec::new() }),
				None => self.recover(at, Error::Corrupt),
			},
			Job::StorageInfo { ids, at: index, mut found } => match ptp::storage_info(&op.data).filter(|_| success) {
				Some(info) => {
					found.push((ids[index], info));
					if index + 1 < ids.len() {
						self.survey(at, Job::StorageInfo { ids, at: index + 1, found });
					} else {
						self.devices[at].storages = found;
						self.surveyed(at);
					}
				}
				None => self.recover(at, Error::Corrupt),
			},
			Job::Enumerate { client, chan, corr, storage, snapshot, .. } => {
				self.devices[at].link.end();
				let taken = match (success, snapshot) {
					(true, Some(snapshot)) => snapshot.finish().map_err(|_| Error::Corrupt),
					(true, None) => Err(Error::Corrupt),
					(false, snapshot) => {
						if let Some(snapshot) = snapshot {
							self.budget.release_snapshot(snapshot.charged);
						}
						Err(if response.code == ptp::INVALID_STORAGE_ID { Error::NotFound } else { Error::Io })
					}
				};
				match taken {
					Ok((handles, charged)) => self.open_cursor(at, client, chan, corr, storage, handles, charged),
					Err(error) => failed(chan, corr, error),
				}
				self.refresh(at);
			}
			Job::Page { cursor, corr, handles, at: index, mut entries, mut sizes } => {
				// A MISSING OBJECT IS STALE, not an empty success - and the device changed without saying so.
				if !success {
					self.devices[at].link.end();
					self.budget.release(mi::PAGE_BYTES as u64);
					if let Some(entry) = self.cursors.iter_mut().find(|entry| entry.id == cursor) {
						entry.paging = false;
						failed(entry.chan, corr, if response.code == ptp::INVALID_OBJECT_HANDLE { Error::Stale } else { Error::Io });
					}
					if response.code == ptp::INVALID_OBJECT_HANDLE {
						self.changed(at);
					}
					self.refresh(at);
					return;
				}
				let context = self.cursors.iter().find(|entry| entry.id == cursor).map_or(0, |entry| entry.cursor.holder.client);
				let entry = self.record(&self.devices[at], context, handles[index], &op.data);
				sizes.push(entry.encode_vec().map_or(usize::MAX, |bytes| bytes.len()));
				entries.push(entry);
				if index + 1 < handles.len() {
					let transaction = self.devices[at].link.survey_transaction();
					let handle = handles[index + 1];
					self.command(at, ptp::GET_OBJECT_INFO, transaction, &[handle], true, mi::OBJECT_INFO_BYTES, Job::Page { cursor, corr, handles, at: index + 1, entries, sizes });
					return;
				}
				self.devices[at].link.end();
				self.answer_page(cursor, corr, entries, sizes);
				self.refresh(at);
			}
			Job::Revalidate { client, chan, corr, object } => {
				let info = ptp::object_info(&op.data).filter(|_| success);
				let current = ptp::revision(&op.data);
				match info {
					Some(info) if current == object.revision => self.start_object(at, client, chan, corr, object, info),
					_ => {
						// GONE OR CHANGED SINCE IT WAS NAMED: stale, and the device changed without saying so.
						self.devices[at].link.end();
						failed(chan, corr, Error::Stale);
						self.changed(at);
						self.refresh(at);
					}
				}
			}
			Job::Object { .. } => {}
		}
	}

	fn surveyed(&mut self, at: usize) {
		let first = self.devices[at].link.phase != mi::Phase::Busy;
		self.devices[at].fresh = true;
		if first {
			self.devices[at].link.surveyed();
			print(b"MediaImportService: a camera was attached\n");
		} else {
			self.devices[at].link.end();
		}
	}

	// One object's page entry: its typed fields and its original dataset within the record budget, or the
	// refusal that says it does not fit - nothing is truncated.
	fn record(&self, device: &Device, context: u32, handle: u32, dataset: &[u8]) -> ImportEntry {
		let id = ImportObjectId { device: self.device_id(device, context), handle, revision: ptp::revision(dataset) };
		let info = ptp::object_info(dataset).unwrap_or_default();
		let parsed = ptp::object_info(dataset).is_some();
		let object = ImportObject { id: id.clone(), format: info.format, format_known: ptp::format_known(info.format), size: info.size.map(u64::from), captured: info.captured.map(|time| ImportTime { year: time.year, month: time.month, day: time.day, hour: time.hour, minute: time.minute, second: time.second, utc_offset_minutes: time.offset_minutes }), parent: info.parent.map(|parent| ImportParentId { device: self.device_id(device, context), handle: parent }), filename: info.filename, interpretation: if parsed && info.complete { ImportInterpretation::Complete } else { ImportInterpretation::Partial }, original: dataset.to_vec() };
		let entry = ImportEntry::Object(object);
		if entry.encode_vec().is_some_and(|bytes| bytes.len() <= mi::RECORD_BYTES) { entry } else { ImportEntry::Unrepresentable(id) }
	}

	fn answer_page(&mut self, cursor: u32, corr: u32, mut entries: Vec<ImportEntry>, sizes: Vec<usize>) {
		self.budget.release(mi::PAGE_BYTES as u64);
		let Some(entry) = self.cursors.iter_mut().find(|entry| entry.id == cursor) else { return };
		entry.paging = false;
		// THE ENVELOPE: correlation, tag, the list length and the remaining count.
		let count = mi::fits(4 + 1 + 2 + 4, &sizes);
		entries.truncate(count);
		entry.cursor.advance(count, clock());
		let page = ImportPage { entries, remaining: entry.cursor.remaining() };
		reply(entry.chan, corr, Ok(()), |w| page.write(w));
	}

	fn open_cursor(&mut self, at: usize, client: u32, chan: u64, corr: u32, storage: u32, handles: Vec<u32>, charged: u64) {
		let holder = mi::Holder { client, device: self.devices[at].key };
		let Some((mine, theirs)) = channel() else {
			self.budget.release_snapshot(charged);
			failed(chan, corr, Error::Exhausted);
			return;
		};
		let count = handles.len() as u32;
		let id = self.next_id;
		self.next_id = self.next_id.wrapping_add(1).max(1);
		let cursor = mi::Cursor::new(holder, self.devices[at].link.epoch, storage, handles, charged, clock());
		self.cursors.push(CursorEntry { id, chan: mine, cursor, paging: false });
		reply(chan, corr, Ok(()), |w| ImportEnumeration::Opened(ImportCursorGrant { cursor: theirs, count }).write(w));
	}

	// The object revalidated: a transfer exists, its chunk is charged, and GetObject goes out as its own
	// transaction - the answer is the transfer's channel.
	fn start_object(&mut self, at: usize, client: u32, chan: u64, corr: u32, object: ImportObjectId, info: ptp::ObjectInfo) {
		self.devices[at].link.end();
		let holder = mi::Holder { client, device: self.devices[at].key };
		let transfer = match mi::Transfer::new(holder, self.devices[at].link.epoch, object.handle, object.revision, info.size, clock()) {
			Ok(transfer) => transfer,
			Err(refusal) => {
				failed(chan, corr, refused(refusal));
				return;
			}
		};
		if self.budget.charge(mi::CHUNK as u64).is_err() {
			failed(chan, corr, Error::Exhausted);
			return;
		}
		let Some((mine, theirs)) = channel() else {
			self.budget.release(mi::CHUNK as u64);
			failed(chan, corr, Error::Exhausted);
			return;
		};
		let Ok(transaction) = self.devices[at].link.begin() else {
			self.budget.release(mi::CHUNK as u64);
			close(mine);
			close(theirs);
			failed(chan, corr, Error::Again);
			return;
		};
		let id = self.next_id;
		self.next_id = self.next_id.wrapping_add(1).max(1);
		self.transfers.push(TransferEntry { id, chan: mine, object: object.clone(), transfer, pending: None, held: None });
		reply(chan, corr, Ok(()), |w| {
			w.set_handle(theirs)?;
			w.u32(0)
		});
		self.command(at, ptp::GET_OBJECT, transaction, &[object.handle], true, u32::MAX, Job::Object { transfer: id });
	}

	// ------------------------------------------------------------------ events and deadlines

	fn on_events(&mut self, key: u32, buf: &mut [u8]) -> Result<(), &'static [u8]> {
		loop {
			let Some(at) = self.at(key) else { return Ok(()) };
			let (len, handles) = match try_recv_caps(self.devices[at].events, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return Ok(()),
				PolledCaps::Closed => return Err(b"its event stream closed"),
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut frame_handles = Handles::new();
			let Some(event) = ptp_transport::events_read(&buf[..len], &mut frame_handles) else { return Err(b"an event did not decode") };
			let attachment = self.devices[at].link.epoch.attachment;
			match event {
				// DROPPED EVENTS ARE CHANGES NOBODY SAW: everything named before is stale.
				PtpEvent::Overflow(on) if on == attachment => self.changed(at),
				PtpEvent::Container(container) if container.attachment == attachment => match ptp::event(&container.bytes).map(|event| mi::change(&event)) {
					Some(mi::Change::Content) => self.changed(at),
					Some(mi::Change::Session) | Some(mi::Change::Cancelled) => self.recover(at, Error::Io),
					Some(mi::Change::None) => {}
					None => return Err(b"an event container was malformed"),
				},
				_ => {}
			}
		}
	}

	fn tick(&mut self) {
		let now = clock();
		for at in 0..self.devices.len() {
			if self.devices[at].link.expired(now) {
				self.unavailable(at);
				continue;
			}
			if let Some((_, _, sent)) = self.devices[at].asked
				&& now >= sent + ANSWER_TICKS
			{
				self.devices[at].asked = None;
				match self.devices[at].link.phase {
					mi::Phase::Resetting | mi::Phase::Attaching => self.unavailable(at),
					_ => self.recover(at, Error::TimedOut),
				}
				continue;
			}
			// A TRANSACTION THE DEVICE STOPPED MOVING ON is abandoned after thirty seconds - a transfer by its own
			// deadlines below, everything else here.
			if let Some(op) = self.devices[at].op.as_ref()
				&& !matches!(op.job, Job::Object { .. })
				&& now >= op.progress_at + mi::TRANSFER_IDLE_TICKS
			{
				self.recover(at, Error::TimedOut);
				continue;
			}
			if let Some(retry) = self.devices[at].retry_at
				&& now >= retry
				&& self.devices[at].asked.is_none()
			{
				self.pull(at);
			}
		}
		// THIRTY SECONDS without device progress or client demand ends a transfer, and its transaction. The devices
		// are collected before the transfers fail rather than pushed one by one: a `Vec<u32>` grown by `push` imports
		// its growth routine from whichever loaded crate exports one, and which crate that is differs between targets.
		let idle: Vec<u32> = self.transfers.iter().filter(|entry| entry.transfer.timed_out(now)).map(|entry| entry.transfer.holder.device).collect();
		for entry in self.transfers.iter_mut().filter(|entry| entry.transfer.timed_out(now)) {
			entry.transfer.fail(mi::Cause::TimedOut);
		}
		self.settle_transfers();
		for device in idle {
			if let Some(at) = self.at(device)
				&& matches!(self.devices[at].op.as_ref().map(|op| &op.job), Some(Job::Object { .. }))
			{
				self.abandon(at, Error::TimedOut);
			}
		}
		// SIXTY SECONDS untouched ends a cursor.
		let expired: Vec<u32> = self.cursors.iter().filter(|entry| !entry.paging && entry.cursor.expired(now)).map(|entry| entry.id).collect();
		for id in expired {
			self.drop_cursor(id);
		}
	}

	fn next_deadline(&self) -> Option<u64> {
		let devices = self.devices.iter().flat_map(|device| {
			[
				device.link.deadline(),
				device.asked.map(|(_, _, sent)| sent + ANSWER_TICKS),
				device.retry_at,
				device.op.as_ref().filter(|op| !matches!(op.job, Job::Object { .. })).map(|op| op.progress_at + mi::TRANSFER_IDLE_TICKS),
			]
		});
		let transfers = self.transfers.iter().map(|entry| entry.transfer.deadline());
		let cursors = self.cursors.iter().map(|entry| Some(entry.cursor.deadline()));
		devices.chain(transfers).chain(cursors).flatten().min()
	}

	fn prune(&mut self) {
		let (cursors, transfers) = (&self.cursors, &self.transfers);
		self.clients.retain(|client| client.chan != 0 || cursors.iter().any(|entry| entry.cursor.holder.client == client.id) || transfers.iter().any(|entry| entry.transfer.holder.client == client.id));
	}
}

// ------------------------------------------------------------------ the views the generated code calls

// What a client asked for that has to wait for a device.
enum Deferred {
	Enumerate { at: usize, storage: u32, parent: u32 },
	Read { at: usize, object: ImportObjectId },
}

struct ImportView<'a> {
	service: &'a mut Service,
	client: u32,
	deferred: Option<Deferred>,
}

impl media_import::Service for ImportView<'_> {
	fn limits(&mut self) -> Result<ImportLimits, Error> {
		Ok(ImportLimits { providers: mi::MAX_PROVIDERS as u8, clients: mi::MAX_CLIENTS as u8, cursors: mi::MAX_CURSORS as u8, transfers: mi::MAX_TRANSFERS as u8, cursors_per_client: 1, transfers_per_client: 1, cursors_per_device: 1, transfers_per_device: 1, data_budget: mi::DATA_BUDGET as u32, snapshot_budget: mi::SNAPSHOT_BUDGET as u32, max_handles: mi::MAX_HANDLES, page_records: mi::PAGE_RECORDS as u8, page_bytes: mi::PAGE_BYTES as u32, record_bytes: mi::RECORD_BYTES as u32, chunk_bytes: mi::CHUNK as u32, cursor_idle_seconds: (mi::CURSOR_IDLE_TICKS / 100) as u32, transfer_idle_seconds: (mi::TRANSFER_IDLE_TICKS / 100) as u32 })
	}

	fn devices(&mut self) -> Result<Vec<ImportDevice>, Error> {
		let service = &*self.service;
		Ok(service
			.devices
			.iter()
			.filter_map(|device| {
				let about = device.about.as_ref()?;
				Some(ImportDevice { id: service.device_id(device, self.client), manufacturer: about.manufacturer.clone(), model: about.model.clone(), serial: about.serial.clone(), usable: about.usable() && device.link.phase != mi::Phase::Unavailable, busy: device.link.phase != mi::Phase::Ready })
			})
			.collect())
	}

	fn storages(&mut self, device: ImportDeviceId) -> Result<Vec<ImportStorage>, Error> {
		let at = self.service.resolve(&device, self.client)?;
		let held = &self.service.devices[at];
		if !held.fresh {
			return Err(Error::Again);
		}
		let id = self.service.device_id(held, self.client);
		Ok(held.storages.iter().map(|(storage, info)| ImportStorage { id: ImportStorageId { device: id.clone(), storage: *storage }, storage_type: info.storage_type, filesystem_type: info.filesystem_type, access: info.access, capacity: info.capacity, free: info.free, description: info.description.clone(), label: info.label.clone() }).collect())
	}

	fn open_enumeration(&mut self, storage: ImportStorageId, parent: Option<ImportParentId>) -> Result<ImportEnumeration, Error> {
		let at = self.service.resolve(&storage.device, self.client)?;
		if let Some(parent) = &parent
			&& parent.device != storage.device
		{
			return Err(Error::Invalid);
		}
		let device = &self.service.devices[at];
		if !device.storages.iter().any(|(id, _)| *id == storage.storage) {
			return Err(Error::NotFound);
		}
		// AN ENUMERATION STILL BEING TAKEN is held like the cursor it will become.
		let pending = self.service.devices.iter().filter_map(|device| match device.op.as_ref().map(|op| &op.job) {
			Some(Job::Enumerate { client, .. }) => Some(mi::Holder { client: *client, device: device.key }),
			_ => None,
		});
		let holders: Vec<mi::Holder> = self.service.cursors.iter().map(|entry| entry.cursor.holder).chain(pending).collect();
		mi::admit(&holders, mi::Holder { client: self.client, device: device.key }, mi::MAX_CURSORS).map_err(refused)?;
		self.deferred = Some(Deferred::Enumerate { at, storage: storage.storage, parent: parent.map_or(0, |parent| parent.handle) });
		Err(Error::Again)
	}

	fn open_read(&mut self, object: ImportObjectId) -> Result<u64, Error> {
		let at = self.service.resolve(&object.device, self.client)?;
		let pending = self.service.devices.iter().filter_map(|device| match device.op.as_ref().map(|op| &op.job) {
			Some(Job::Revalidate { client, .. }) => Some(mi::Holder { client: *client, device: device.key }),
			_ => None,
		});
		let holders: Vec<mi::Holder> = self.service.transfers.iter().map(|entry| entry.transfer.holder).chain(pending).collect();
		mi::admit(&holders, mi::Holder { client: self.client, device: self.service.devices[at].key }, mi::MAX_TRANSFERS).map_err(refused)?;
		self.deferred = Some(Deferred::Read { at, object });
		Err(Error::Again)
	}
}

struct CursorView {
	next: bool,
	closing: bool,
}

impl import_cursor::Service for CursorView {
	fn next(&mut self) -> Result<ImportPage, Error> {
		self.next = true;
		Err(Error::Again)
	}
	fn close(&mut self) -> Result<(), Error> {
		self.closing = true;
		Ok(())
	}
}

enum TransferAsk {
	Read,
	Cancel,
}

struct TransferView<'a> {
	service: &'a Service,
	transfer: u32,
	asked: Option<TransferAsk>,
}

impl import_transfer::Service for TransferView<'_> {
	fn read(&mut self) -> Result<ImportRead, Error> {
		self.asked = Some(TransferAsk::Read);
		Err(Error::Again)
	}
	fn status(&mut self) -> Result<ImportTransferStatus, Error> {
		self.service.transfers.iter().find(|entry| entry.id == self.transfer).map(transfer_status).ok_or(Error::Closed)
	}
	fn cancel(&mut self) -> Result<ImportTransferStatus, Error> {
		self.asked = Some(TransferAsk::Cancel);
		Err(Error::Again)
	}
}

impl Service {
	// A client's request that has to wait for a device: the transaction begins now, or the device is busy.
	fn defer(&mut self, client: u32, chan: u64, corr: u32, deferred: Deferred) {
		match deferred {
			Deferred::Enumerate { at, storage, parent } => {
				let transaction = match self.devices[at].link.begin() {
					Ok(transaction) => transaction,
					Err(refusal) => return failed(chan, corr, refused(refusal)),
				};
				self.command(at, ptp::GET_OBJECT_HANDLES, transaction, &[storage, 0, parent], true, mi::HANDLES_LIMIT, Job::Enumerate { client, chan, corr, storage, snapshot: None, end: None });
			}
			Deferred::Read { at, object } => {
				let transaction = match self.devices[at].link.begin() {
					Ok(transaction) => transaction,
					Err(refusal) => return failed(chan, corr, refused(refusal)),
				};
				let handle = object.handle;
				self.command(at, ptp::GET_OBJECT_INFO, transaction, &[handle], true, mi::OBJECT_INFO_BYTES, Job::Revalidate { client, chan, corr, object });
			}
		}
	}

	// A page: stale if the device moved on, empty at the end, and otherwise two metadata reads at most.
	fn page(&mut self, cursor: u32, corr: u32) {
		let Some(entry) = self.cursors.iter().find(|entry| entry.id == cursor) else { return };
		let chan = entry.chan;
		if entry.paging {
			return failed(chan, corr, Error::Again);
		}
		let Some(at) = self.at(entry.cursor.holder.device) else { return failed(chan, corr, Error::Stale) };
		if mi::scoped(self.devices[at].link.epoch, entry.cursor.epoch).is_err() {
			return failed(chan, corr, Error::Stale);
		}
		let handles = entry.cursor.upcoming().to_vec();
		if handles.is_empty() {
			let page = ImportPage { entries: Vec::new(), remaining: 0 };
			return reply(chan, corr, Ok(()), |w| page.write(w));
		}
		let transaction = match self.devices[at].link.begin() {
			Ok(transaction) => transaction,
			Err(refusal) => return failed(chan, corr, refused(refusal)),
		};
		// THE PAGE'S BYTES ARE CHARGED before the reads that will fill it, and released when it is answered.
		if self.budget.charge(mi::PAGE_BYTES as u64).is_err() {
			self.devices[at].link.end();
			return failed(chan, corr, Error::Exhausted);
		}
		if let Some(entry) = self.cursors.iter_mut().find(|entry| entry.id == cursor) {
			entry.paging = true;
			entry.cursor.last_used = clock();
		}
		let first = handles[0];
		self.command(at, ptp::GET_OBJECT_INFO, transaction, &[first], true, mi::OBJECT_INFO_BYTES, Job::Page { cursor, corr, handles, at: 0, entries: Vec::new(), sizes: Vec::new() });
	}

	// A read: the held chunk, the ending, or the next pull - on demand, one at a time.
	fn read(&mut self, transfer: u32, corr: u32) {
		let Some(entry) = self.transfers.iter_mut().find(|entry| entry.id == transfer) else { return };
		if entry.pending.is_some() {
			return failed(entry.chan, corr, Error::Again);
		}
		entry.pending = Some(corr);
		entry.transfer.demanded(clock());
		let device = entry.transfer.holder.device;
		if entry.held.is_some() || entry.transfer.terminal() {
			self.deliver(transfer);
			self.settle_transfers();
			return;
		}
		if let Some(at) = self.at(device)
			&& matches!(self.devices[at].op.as_ref().map(|op| &op.job), Some(Job::Object { transfer: id }) if *id == transfer)
			&& self.devices[at].asked.is_none()
			&& self.devices[at].retry_at.is_none()
		{
			self.pull(at);
		}
	}

	// Cancel a transfer: it ends partial with what was delivered, and its transaction is cancelled.
	fn cancel(&mut self, transfer: u32) -> Option<ImportTransferStatus> {
		let entry = self.transfers.iter_mut().find(|entry| entry.id == transfer)?;
		let device = entry.transfer.holder.device;
		let live = !entry.transfer.terminal();
		entry.transfer.fail(mi::Cause::Cancelled);
		entry.held = None;
		let status = transfer_status(entry);
		self.settle_transfers();
		if live
			&& let Some(at) = self.at(device)
			&& matches!(self.devices[at].op.as_ref().map(|op| &op.job), Some(Job::Object { transfer: id }) if *id == transfer)
		{
			self.abandon(at, Error::Cancelled);
		}
		Some(status)
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (serve_root, catalogue) = (roles[0], roles[1]);
	// NO DEVICE IS NOT AN ERROR: with no catalogue, or nothing published, the service lists nothing.
	let subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::PtpTransport).unwrap_or(0) } else { 0 };
	let mut drawn = [0u8; 8];
	random_get(&mut drawn);
	let mut service = Service { incarnation: u64::from_le_bytes(drawn) | 1, catalogue, devices: Vec::new(), clients: Vec::new(), cursors: Vec::new(), transfers: Vec::new(), budget: mi::Budget::default(), next_key: 1, next_client: 1, next_id: 1 };
	send_blocking(bootstrap, b"MediaImportService: online", 0);

	let mut buf = alloc::vec![0u8; BUF_BYTES];
	let mut reply_buf = alloc::vec![0u8; BUF_BYTES];
	let mut subscribed = subscription != 0;
	loop {
		let mut waitset: Vec<u64> = Vec::new();
		if serve_root != 0 {
			waitset.push(serve_root);
		}
		if subscribed {
			waitset.push(subscription);
		}
		waitset.extend(service.clients.iter().filter(|client| client.chan != 0).map(|client| client.chan));
		waitset.extend(service.cursors.iter().map(|entry| entry.chan));
		waitset.extend(service.transfers.iter().map(|entry| entry.chan));
		for device in &service.devices {
			waitset.push(device.chan);
			if device.events != 0 {
				waitset.push(device.events);
			}
		}
		let now = clock();
		let deadline = service.next_deadline().map_or(0, |deadline| deadline.max(now + 1));
		let ready = wait_any(&waitset, deadline);
		if ready >= 0 {
			serve(&mut service, waitset[ready as usize], serve_root, subscription, &mut subscribed, &mut buf, &mut reply_buf);
		}
		service.tick();
		for at in 0..service.devices.len() {
			service.refresh(at);
		}
		service.prune();
	}
}

fn serve(service: &mut Service, handle: u64, serve_root: u64, subscription: u64, subscribed: &mut bool, buf: &mut [u8], reply_buf: &mut [u8]) {
	if let Some(key) = service.devices.iter().find(|device| device.chan == handle).map(|device| device.key) {
		if let Err(why) = service.on_answer(key, buf) {
			service.lose(key, why);
		}
		return;
	}
	if let Some(key) = service.devices.iter().find(|device| device.events == handle && handle != 0).map(|device| device.key) {
		if let Err(why) = service.on_events(key, buf) {
			service.lose(key, why);
		}
		return;
	}
	if let Some(at) = service.cursors.iter().position(|entry| entry.chan == handle) {
		let id = service.cursors[at].id;
		let (len, mut handles) = match try_recv_caps(handle, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				service.drop_cursor(id);
				return;
			}
		};
		let mut reply_handles = Handles::new();
		let mut view = CursorView { next: false, closing: false };
		let written = import_cursor::dispatch(&mut view, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		match written {
			Some(_) if view.next => service.page(id, u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]])),
			Some(written) => {
				answer(handle, &reply_buf[..written], reply_handles.as_slice());
				if view.closing {
					service.drop_cursor(id);
				}
			}
			None => service.drop_cursor(id),
		}
		return;
	}
	if let Some(at) = service.transfers.iter().position(|entry| entry.chan == handle) {
		let id = service.transfers[at].id;
		let (len, mut handles) = match try_recv_caps(handle, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			// THE CONSUMER IS GONE: the transfer is cancelled, and everything it held is released.
			PolledCaps::Closed => {
				service.cancel(id);
				service.drop_transfer(id);
				return;
			}
		};
		let mut reply_handles = Handles::new();
		let mut view = TransferView { service, transfer: id, asked: None };
		let written = import_transfer::dispatch(&mut view, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
		let asked = view.asked.take();
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		let corr = u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]);
		match (written, asked) {
			(Some(_), Some(TransferAsk::Read)) => service.read(id, corr),
			(Some(_), Some(TransferAsk::Cancel)) => {
				if let Some(status) = service.cancel(id) {
					reply(handle, corr, Ok(()), |w| status.write(w));
				}
			}
			(Some(written), None) => answer(handle, &reply_buf[..written], reply_handles.as_slice()),
			(None, _) => {
				service.cancel(id);
				service.drop_transfer(id);
			}
		}
		return;
	}
	if let Some(at) = service.clients.iter().position(|client| client.chan == handle) {
		let client = service.clients[at].id;
		let (len, mut handles) = match try_recv_caps(handle, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				close(handle);
				service.clients[at].chan = 0;
				return;
			}
		};
		let mut reply_handles = Handles::new();
		let mut view = ImportView { service, client, deferred: None };
		let written = media_import::dispatch(&mut view, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
		let deferred = view.deferred.take();
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		match (written, deferred) {
			(Some(_), Some(deferred)) => {
				let corr = u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]);
				service.defer(client, handle, corr, deferred);
			}
			(Some(written), None) => answer(handle, &reply_buf[..written], reply_handles.as_slice()),
			// A request that does not decode, or an operation this interface does not have, ends the connection.
			(None, _) => {
				close(handle);
				if let Some(at) = service.clients.iter().position(|client| client.chan == handle) {
					service.clients[at].chan = 0;
				}
			}
		}
		return;
	}
	if *subscribed && handle == subscription {
		loop {
			let (len, handles) = match try_recv_caps(subscription, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					*subscribed = false;
					break;
				}
			};
			for &leftover in handles.as_slice() {
				close(leftover);
			}
			let mut frame_handles = Handles::new();
			let Some(info) = provider_catalogue::subscribe_read(&buf[..len], &mut frame_handles) else { continue };
			if info.live {
				service.adopt(info);
			} else if let Some(key) = service.devices.iter().find(|device| same(&device.info, &info)).map(|device| device.key) {
				service.lose(key, b"its publication was withdrawn");
			}
		}
		return;
	}
	if handle != serve_root {
		return;
	}
	let (len, handles) = match try_recv_caps(handle, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty | PolledCaps::Closed => return,
	};
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	if len < 2 {
		return;
	}
	match u16::from_le_bytes([buf[0], buf[1]]) {
		HEARTBEAT_OP => {
			send_blocking(handle, b"PONG", 0);
		}
		// A FRESH CLIENT CONTEXT, sixteen at most, one kept while anything is still held under it.
		CONNECT_OP => match channel().filter(|_| service.clients.len() < mi::MAX_CLIENTS) {
			Some((mine, theirs)) => {
				let id = service.next_client;
				service.next_client = service.next_client.wrapping_add(1).max(1);
				service.clients.push(Client { id, chan: mine });
				send_blocking(handle, &[], theirs);
			}
			None => {
				send_blocking(handle, &[], 0);
			}
		},
		_ => {}
	}
}
