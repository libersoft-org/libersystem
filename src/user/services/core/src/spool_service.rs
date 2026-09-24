// SpoolService - print jobs an application stages and explicitly submits, sent to a printer by acknowledged
// prefixes, with an honest account of what the transport took.
//
// WHAT IT HOLDS AND WHAT IT DOES NOT. It consumes every `printer` publication through a catalogue connection
// minted for that kind alone, and is each one's only consumer: a backend - the USB printer class module, or the
// kernel's sink fixture before it - reports its IEEE 1284 device ID and its port status and accepts a prefix of
// each write. It serves one root, SERVE, and every connection minted from it is a GRANT CONTEXT: the jobs made
// on it are charged to it, two at most, until each job's own channel is gone. A job is a channel of its own -
// write, submit, status, cancel - and nothing on either reaches a catalogue, a backend, a reset or a raw
// endpoint. It renders nothing, sniffs nothing and keeps nothing across a restart.
//
// THE RULES ARE `service_logic::spool_jobs`'s. Admission reserves a job's whole declared length before the job
// exists; only an explicit submit of exactly that length makes a byte eligible to send; one job per printer at
// a time in submission order, one write outstanding, advanced by exactly the count the backend acknowledged;
// thirty seconds without an accepted byte, or ten minutes from submission, end a job; and a job that ends with
// a write outstanding says its delivery is uncertain. Nothing is ever sent twice.
//
// NOTHING WAITS ON A PRINTER. Every backend request goes out without blocking and is answered in the loop, so a
// printer that stops answering holds up its own jobs and nothing else - not another printer, not a client, not
// the supervisor's heartbeat. A backend that fails a write, acknowledges more than it was offered or does not
// answer is reset; a reset ends every job on the printer and never replays a byte, and a reset that fails
// leaves the printer unavailable until it is withdrawn.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{DocumentLanguage, Error, JobCause, JobState, JobStatus, Observation, PortReading, PortStatus, PrinterAttach, PrinterId, PrinterInfo, ProviderInfo, ProviderKind, job, printer_backend, provider_catalogue, spool};
use rt::*;
use service_logic::printer_status as evidence;
use service_logic::spool_jobs as jobs;
use wire::{Handles, Reader, Sink, Transport, TransportError};

include!(concat!(env!("OUT_DIR"), "/roles_spool_service.rs"));

const VERSION: u32 = 1;
// How long an attach, a status read or a reset may go unanswered: two seconds.
const ANSWER_TICKS: u64 = 200;
// After `again`: a tenth of a second, then a status read, then the frame again.
const RETRY_TICKS: u64 = 10;
// THE LARGEST FRAMES THIS SERVICE READS, ENVELOPES INCLUDED: a job's `write` - opcode, correlation, list length
// and a 4096-byte frame - and a backend's answer to an attach or a reset - correlation, result tag, version,
// attachment, list length and a 4096-byte device ID. The buffer holds either.
const JOB_WRITE_FRAME: usize = 2 + 4 + 2 + jobs::FRAME;
const ATTACH_ANSWER_FRAME: usize = 4 + 1 + 4 + 8 + 2 + evidence::MAX_DEVICE_ID;
const BUF_BYTES: usize = 8192;
const _: () = assert!(BUF_BYTES >= JOB_WRITE_FRAME && BUF_BYTES >= ATTACH_ANSWER_FRAME);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
	// Waiting for the backend's attach, or for its reset's.
	Attaching,
	Ready,
	// Its attach or its reset failed: nothing is admitted or sent until it is withdrawn.
	Unavailable,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Asked {
	Attach,
	Status,
	Write { job: u32, offered: u32 },
	Reset,
}

struct Printer {
	key: u32,
	info: ProviderInfo,
	chan: u64,
	phase: Phase,
	// The attachment generation the backend answered with; a reset is a new one.
	attachment: u64,
	model: String,
	// Its device ID names PostScript. Anything short of that - no command set, a malformed ID, two answers - is
	// no evidence, and a printer without evidence takes no language at all.
	postscript: bool,
	port: evidence::Port,
	// THE ONE REQUEST OUTSTANDING AT THE BACKEND: its correlation, what it asked, and when it was sent.
	asked: Option<(u32, Asked, u64)>,
	// After `again`: when to read the port and offer the frame again.
	retry_at: Option<u64>,
	next_corr: u32,
}

// A grant context: one connection from SERVE, and the jobs made on it. It outlives its connection while any of
// its jobs is still charged - closing the connection refunds nothing whose job channel is alive.
struct Client {
	id: u32,
	chan: u64,
}

// A job's channel, and the printer identity it was admitted for.
struct JobChannel {
	job: u32,
	chan: u64,
	printer: PrinterId,
}

struct Service {
	incarnation: u64,
	catalogue: u64,
	printers: Vec<Printer>,
	clients: Vec<Client>,
	channels: Vec<JobChannel>,
	spool: jobs::Spool,
	next_key: u32,
	next_client: u32,
}

// A generated client's request, captured instead of sent: the encoding is the generator's, the sending is this
// service's own - non-blocking, under a correlation of its choosing.
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

fn captured(encode: impl FnOnce(&mut printer_backend::Client<&mut Capture>), corr: u32) -> Option<Vec<u8>> {
	let mut capture = Capture { bytes: Vec::new() };
	encode(&mut printer_backend::Client::new(&mut capture));
	if capture.bytes.len() < 6 {
		return None;
	}
	capture.bytes[2..6].copy_from_slice(&corr.to_le_bytes());
	Some(capture.bytes)
}

// Answer on a client's channel without waiting: a client that does not read its answers loses them and holds
// up nothing but itself. A capability in an answer that could not be delivered is closed here.
fn answer(chan: u64, bytes: &[u8], handles: &[u64]) {
	if !matches!(try_send_caps_outcome(chan, bytes, handles), SendOutcome::Delivered) {
		for &handle in handles {
			close(handle);
		}
	}
}

fn refuse(chan: u64, request: &[u8], error: Error) {
	let mut writer = wire::VecWriter::new();
	let encoded = (|| {
		writer.u32(u32::from_le_bytes([request[2], request[3], request[4], request[5]]))?;
		writer.u8(0)?;
		error.write(&mut writer)
	})();
	if encoded.is_some()
		&& let Some(bytes) = writer.into_inner()
	{
		answer(chan, &bytes, &[]);
	}
}

fn same(a: &ProviderInfo, b: &ProviderInfo) -> bool {
	a.slot == b.slot && a.provider_generation == b.provider_generation && a.binding_generation == b.binding_generation
}

fn observation(observed: evidence::Observation) -> Observation {
	match observed {
		evidence::Observation::Yes => Observation::Yes,
		evidence::Observation::No => Observation::No,
		evidence::Observation::Unavailable => Observation::Unavailable,
		evidence::Observation::Unsupported => Observation::Unsupported,
	}
}

fn port_status(port: evidence::Port) -> PortStatus {
	PortStatus { paper_empty: observation(port.paper_empty), selected: observation(port.selected), error: observation(port.error), cover_open: observation(port.cover_open), jam: observation(port.jam) }
}

fn refused(refusal: jobs::Refusal) -> Error {
	match refusal {
		jobs::Refusal::Unsupported => Error::Unsupported,
		jobs::Refusal::Exhausted => Error::Exhausted,
		jobs::Refusal::Invalid => Error::Invalid,
		jobs::Refusal::NotFound => Error::NotFound,
	}
}

fn job_status(status: jobs::Status, printer: &PrinterId) -> JobStatus {
	JobStatus {
		state: match status.state {
			jobs::State::Writing => JobState::Writing,
			jobs::State::Queued => JobState::Queued,
			jobs::State::Active => JobState::Active,
			jobs::State::Transferred => JobState::Transferred,
			jobs::State::Failed => JobState::Failed,
			jobs::State::Cancelled => JobState::Cancelled,
		},
		printer: printer.clone(),
		declared: status.declared,
		staged: status.staged,
		acknowledged: status.acknowledged,
		cause: status.cause.map(|cause| match cause {
			jobs::Cause::SizeLimit => JobCause::SizeLimit,
			jobs::Cause::Unplugged => JobCause::Unplugged,
			jobs::Cause::BackendError => JobCause::BackendError,
			jobs::Cause::Stalled => JobCause::Stalled,
			jobs::Cause::Lifetime => JobCause::Lifetime,
			jobs::Cause::Reset => JobCause::Reset,
			jobs::Cause::Abandoned => JobCause::Abandoned,
			jobs::Cause::Cancelled => JobCause::Cancelled,
		}),
		delivery_uncertain: status.uncertain,
	}
}

impl Service {
	fn at(&self, key: u32) -> Option<usize> {
		self.printers.iter().position(|printer| printer.key == key)
	}

	fn printer_id(&self, printer: &Printer) -> PrinterId {
		PrinterId { slot: printer.info.slot, generation: printer.info.provider_generation, binding_generation: printer.info.binding_generation, attachment: printer.attachment, incarnation: self.incarnation }
	}

	fn printer_info(&self, printer: &Printer) -> PrinterInfo {
		PrinterInfo { id: self.printer_id(printer), model: printer.model.clone(), languages: if printer.postscript { alloc::vec![DocumentLanguage::Postscript] } else { Vec::new() }, status: port_status(printer.port), available: printer.phase == Phase::Ready, queued: self.spool.queued(printer.key).min(usize::from(u8::MAX)) as u8, active: self.spool.active(printer.key), max_job_bytes: jobs::MAX_JOB_BYTES }
	}

	// One request to a backend, sent without waiting. It replaces whatever was outstanding: a late answer to
	// that carries a correlation nothing is waiting for any more, and is dropped.
	fn ask(&mut self, at: usize, asked: Asked, encode: impl FnOnce(&mut printer_backend::Client<&mut Capture>)) -> bool {
		let printer = &mut self.printers[at];
		let corr = printer.next_corr;
		printer.next_corr = printer.next_corr.wrapping_add(1).max(1);
		let sent = captured(encode, corr).is_some_and(|bytes| try_send(printer.chan, &bytes, 0));
		printer.asked = if sent { Some((corr, asked, clock())) } else { None };
		sent
	}

	fn read_port(&mut self, at: usize) {
		let attachment = self.printers[at].attachment;
		if !self.ask(at, Asked::Status, |client| {
			let _ = client.port_status(&attachment);
		}) {
			self.printers[at].port = evidence::Port::UNAVAILABLE;
		}
	}

	fn adopt(&mut self, info: ProviderInfo) {
		if self.printers.iter().any(|printer| same(&printer.info, &info)) {
			return;
		}
		if self.printers.len() >= jobs::MAX_PRINTERS {
			print(b"SpoolService: a printer was refused: this service holds eight (resource exhausted)\n");
			return;
		}
		let Some(Ok(chan)) = provider_catalogue::Client::new(ChannelTransport { chan: self.catalogue }).open(&info) else {
			print(b"SpoolService: a published printer could not be opened\n");
			return;
		};
		let key = self.next_key;
		self.next_key = self.next_key.wrapping_add(1).max(1);
		self.printers.push(Printer { key, info, chan, phase: Phase::Attaching, attachment: 0, model: String::new(), postscript: false, port: evidence::Port::UNAVAILABLE, asked: None, retry_at: None, next_corr: 1 });
		let at = self.printers.len() - 1;
		if !self.ask(at, Asked::Attach, |client| {
			let _ = client.attach(&VERSION);
		}) {
			self.lose(key, b"its attach could not be sent");
		}
	}

	// A printer is gone - withdrawn, or its backend's connection closed. Every job bound to it ends as unplugged
	// with whatever was acknowledged; a replacement is another printer, and nothing is replayed to it.
	fn lose(&mut self, key: u32, why: &[u8]) {
		let Some(at) = self.at(key) else { return };
		let printer = self.printers.remove(at);
		close(printer.chan);
		self.spool.printer_lost(key);
		print(b"SpoolService: a printer is gone: ");
		print(why);
		print(b"\n");
	}

	// RESET: every job on the printer ends - none is replayed, not even the prefix a reset may have undone - and
	// its attachment is over. Only a successful reset's new attachment makes it eligible again, and that says
	// nothing about the paper or the printer's language state.
	fn recover(&mut self, at: usize) {
		self.spool.printer_reset(self.printers[at].key);
		self.printers[at].phase = Phase::Attaching;
		self.printers[at].retry_at = None;
		let attachment = self.printers[at].attachment;
		if !self.ask(at, Asked::Reset, |client| {
			let _ = client.reset(&attachment);
		}) {
			self.printers[at].phase = Phase::Unavailable;
			print(b"SpoolService: a printer's reset could not be sent - it is unavailable\n");
		}
	}

	// An attach or a reset answered. A new attachment generation, the version this service speaks, and a device
	// ID that is length-checked before anything in it is believed.
	fn attached(&mut self, at: usize, asked: Asked, attached: Option<PrinterAttach>) {
		let previous = self.printers[at].attachment;
		let Some(attached) = attached.filter(|attached| attached.version == VERSION && attached.attachment != 0 && attached.attachment != previous) else {
			self.printers[at].phase = Phase::Unavailable;
			print(if asked == Asked::Reset { b"SpoolService: a printer's reset failed - it is unavailable until it is withdrawn\n" as &[u8] } else { b"SpoolService: a printer did not attach - it is unavailable\n" });
			return;
		};
		let (model, postscript) = match evidence::parse(&attached.device_id) {
			Ok(id) => (id.model.unwrap_or_default(), id.postscript),
			Err(_) => {
				print(b"SpoolService: a printer's device ID was malformed - it takes no language\n");
				(String::new(), false)
			}
		};
		let printer = &mut self.printers[at];
		printer.attachment = attached.attachment;
		printer.model = model;
		printer.postscript = postscript;
		printer.phase = Phase::Ready;
		print(if asked == Asked::Reset { b"SpoolService: a printer was reset and attached again\n" as &[u8] } else { b"SpoolService: a printer was attached\n" });
		self.read_port(at);
	}

	// A write answered: the job advances by exactly the count acknowledged, `again` is none, and anything the
	// backend should not have said ends the job and resets the printer.
	fn wrote(&mut self, at: usize, job: u32, offered: u32, result: Option<Result<u32, Error>>) {
		let now = clock();
		match result {
			Some(Ok(accepted)) if accepted > offered => {
				// PAST WHAT WAS OFFERED IS AN ACKNOWLEDGEMENT OF NOTHING: the job fails, uncertain.
				let _ = self.spool.written(job, accepted, now);
				print(b"SpoolService: a printer acknowledged more than it was offered - the printer is reset\n");
				self.recover(at);
			}
			Some(Ok(accepted)) if accepted > 0 => {
				let _ = self.spool.written(job, accepted, now);
			}
			Some(Ok(_)) | Some(Err(Error::Again)) => {
				let _ = self.spool.written(job, 0, now);
				self.printers[at].retry_at = Some(now + RETRY_TICKS);
			}
			_ => {
				self.spool.write_failed(job, true);
				print(b"SpoolService: a printer failed a write - the printer is reset\n");
				self.recover(at);
			}
		}
	}

	// A backend's answers, each matched to the one request outstanding; anything else answers a request this
	// service already gave up on, and is dropped.
	fn on_answer(&mut self, key: u32, buf: &mut [u8]) -> Result<(), &'static [u8]> {
		loop {
			let Some(at) = self.at(key) else { return Ok(()) };
			let (len, handles) = match try_recv_caps(self.printers[at].chan, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return Ok(()),
				PolledCaps::Closed => return Err(b"its connection closed"),
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut reader = Reader::new(&buf[..len]);
			let Some((corr, asked, _)) = self.printers[at].asked else { continue };
			if reader.u32() != Some(corr) {
				continue;
			}
			self.printers[at].asked = None;
			let ok = reader.tag();
			match asked {
				Asked::Attach | Asked::Reset => {
					let attached = if ok == Some(true) { PrinterAttach::read(&mut reader) } else { None };
					self.attached(at, asked, attached);
				}
				Asked::Status => {
					let reading = if ok == Some(true) { PortReading::read(&mut reader) } else { None };
					self.printers[at].port = reading.map_or(evidence::Port::UNAVAILABLE, |reading| evidence::port(reading.bits, reading.cover_open, reading.jam));
				}
				Asked::Write { job, offered } => {
					let result = match ok {
						Some(true) => reader.u32().map(Ok),
						Some(false) => Error::read(&mut reader).map(Err),
						None => None,
					};
					self.wrote(at, job, offered, result);
				}
			}
		}
	}

	// Offer every ready, idle printer its next frame.
	fn pump(&mut self) {
		let now = clock();
		let mut gone = Vec::new();
		for at in 0..self.printers.len() {
			let printer = &self.printers[at];
			if printer.phase != Phase::Ready || printer.asked.is_some() || printer.retry_at.is_some() || !self.spool.pending(printer.key) {
				continue;
			}
			let (key, attachment, chan, corr) = (printer.key, printer.attachment, printer.chan, printer.next_corr);
			let Some(frame) = self.spool.next_frame(key, attachment, now) else { continue };
			let (job, offered) = (frame.job, frame.bytes.len() as u32);
			let bytes = captured(
				|client| {
					let _ = client.write(&attachment, frame.bytes);
				},
				corr,
			);
			let printer = &mut self.printers[at];
			printer.next_corr = printer.next_corr.wrapping_add(1).max(1);
			if bytes.is_some_and(|bytes| try_send(chan, &bytes, 0)) {
				printer.asked = Some((corr, Asked::Write { job, offered }, now));
			} else {
				// THE FRAME NEVER LEFT, so nothing of it is uncertain: it is taken back, and a backend that
				// cannot take one request is gone.
				let _ = self.spool.written(job, 0, now);
				gone.push(key);
			}
		}
		for key in gone {
			self.lose(key, b"a write could not be sent to it");
		}
	}

	// Deadlines: the jobs' own, a write that has gone unanswered as long as a job may go without progress, an
	// attach or a reset past its answer time, and the reads that follow `again`.
	fn tick(&mut self) {
		let now = clock();
		self.spool.tick(now);
		for at in 0..self.printers.len() {
			match self.printers[at].asked {
				Some((_, Asked::Write { .. }, sent)) if now >= sent + jobs::STALL_TICKS => {
					print(b"SpoolService: a printer left a write unanswered for thirty seconds - it is reset\n");
					self.recover(at);
				}
				Some((_, Asked::Attach | Asked::Reset, sent)) if now >= sent + ANSWER_TICKS => {
					self.printers[at].asked = None;
					self.printers[at].phase = Phase::Unavailable;
					print(b"SpoolService: a printer did not answer an attach or a reset - it is unavailable\n");
				}
				Some((_, Asked::Status, sent)) if now >= sent + ANSWER_TICKS => {
					self.printers[at].asked = None;
					self.printers[at].port = evidence::Port::UNAVAILABLE;
				}
				_ => {}
			}
			if let Some(retry) = self.printers[at].retry_at
				&& now >= retry
				&& self.printers[at].asked.is_none()
			{
				self.printers[at].retry_at = None;
				if self.printers[at].phase == Phase::Ready {
					self.read_port(at);
				}
			}
		}
	}

	fn next_deadline(&self) -> Option<u64> {
		let printers = self.printers.iter().flat_map(|printer| {
			let asked = printer.asked.map(|(_, asked, sent)| sent + if matches!(asked, Asked::Write { .. }) { jobs::STALL_TICKS } else { ANSWER_TICKS });
			[asked, printer.retry_at]
		});
		printers.flatten().chain(self.spool.next_deadline()).min()
	}

	// A grant context whose connection is gone is kept exactly as long as something is still charged to it.
	fn prune(&mut self) {
		let spool = &self.spool;
		self.clients.retain(|client| client.chan != 0 || spool.charged(client.id) > 0);
	}
}

// ------------------------------------------------------------------ the views the generated code calls

// One grant context's connection: the printers, and admission.
struct SpoolView<'a> {
	service: &'a mut Service,
	client: u32,
}

impl spool::Service for SpoolView<'_> {
	fn printers(&mut self) -> Result<Vec<PrinterInfo>, Error> {
		let service = &*self.service;
		Ok(service.printers.iter().map(|printer| service.printer_info(printer)).collect())
	}

	// ADMISSION. A printer named by its whole identity - publication, attachment generation and this
	// incarnation - that is attached now and whose device ID names the language, and the whole declared length
	// reserved before the job exists.
	fn create(&mut self, printer: PrinterId, language: DocumentLanguage, length: u32) -> Result<u64, Error> {
		let service = &mut *self.service;
		let Some(at) = service.printers.iter().position(|held| held.info.slot == printer.slot && held.info.provider_generation == printer.generation && held.info.binding_generation == printer.binding_generation) else { return Err(Error::NotFound) };
		if printer.incarnation != service.incarnation || printer.attachment != service.printers[at].attachment {
			return Err(Error::Stale);
		}
		let held = &service.printers[at];
		match held.phase {
			Phase::Ready => {}
			Phase::Attaching => return Err(Error::Again),
			Phase::Unavailable => return Err(Error::Io),
		}
		// NO RAW-LANGUAGE BYPASS AND NO SNIFFING: the declared language, and the printer's own word for it.
		let takes = language == DocumentLanguage::Postscript && held.postscript;
		let (key, attachment) = (held.key, held.attachment);
		let id = service.printer_id(held);
		let job = service.spool.create(self.client, key, attachment, takes, length).map_err(refused)?;
		let Some((mine, theirs)) = channel() else {
			service.spool.close(job);
			return Err(Error::Exhausted);
		};
		service.channels.push(JobChannel { job, chan: mine, printer: id });
		Ok(theirs)
	}
}

// One job's channel.
struct JobView<'a> {
	service: &'a mut Service,
	job: u32,
	printer: &'a PrinterId,
}

impl job::Service for JobView<'_> {
	fn write(&mut self, bytes: Vec<u8>) -> Result<u32, Error> {
		self.service.spool.write(self.job, &bytes).map_err(refused)
	}
	fn submit(&mut self) -> Result<JobStatus, Error> {
		let status = self.service.spool.submit(self.job, clock()).map_err(refused)?;
		Ok(job_status(status, self.printer))
	}
	fn status(&mut self) -> Result<JobStatus, Error> {
		let status = self.service.spool.status(self.job).map_err(refused)?;
		Ok(job_status(status, self.printer))
	}
	fn cancel(&mut self) -> Result<JobStatus, Error> {
		let status = self.service.spool.cancel(self.job).map_err(refused)?;
		Ok(job_status(status, self.printer))
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (serve_root, catalogue) = (roles[0], roles[1]);
	// NO PRINTER IS NOT AN ERROR. With no catalogue, or nothing published, the service serves an empty list.
	let subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::Printer).unwrap_or(0) } else { 0 };
	let mut drawn = [0u8; 8];
	random_get(&mut drawn);
	let mut service = Service { incarnation: u64::from_le_bytes(drawn) | 1, catalogue, printers: Vec::new(), clients: Vec::new(), channels: Vec::new(), spool: jobs::Spool::new(), next_key: 1, next_client: 1 };
	send_blocking(bootstrap, b"SpoolService: online", 0);

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
		waitset.extend(service.channels.iter().map(|channel| channel.chan));
		waitset.extend(service.printers.iter().map(|printer| printer.chan));
		let now = clock();
		let deadline = service.next_deadline().map_or(0, |deadline| deadline.max(now + 1));
		let ready = wait_any(&waitset, deadline);
		if ready >= 0 {
			serve(&mut service, waitset[ready as usize], serve_root, subscription, &mut subscribed, &mut buf, &mut reply_buf);
		}
		service.tick();
		service.pump();
		service.prune();
	}
}

fn serve(service: &mut Service, handle: u64, serve_root: u64, subscription: u64, subscribed: &mut bool, buf: &mut [u8], reply_buf: &mut [u8]) {
	if let Some(key) = service.printers.iter().find(|printer| printer.chan == handle).map(|printer| printer.key) {
		if let Err(why) = service.on_answer(key, buf) {
			service.lose(key, why);
		}
		return;
	}
	if let Some(at) = service.channels.iter().position(|channel| channel.chan == handle) {
		serve_job(service, at, buf, reply_buf);
		return;
	}
	if let Some(at) = service.clients.iter().position(|client| client.chan == handle) {
		let (len, mut handles) = match try_recv_caps(handle, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				close(handle);
				service.clients[at].chan = 0;
				return;
			}
		};
		let client = service.clients[at].id;
		let mut reply_handles = Handles::new();
		let written = spool::dispatch(&mut SpoolView { service, client }, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		match written {
			Some(written) => answer(handle, &reply_buf[..written], reply_handles.as_slice()),
			// A request the dispatch cannot decode ends the connection; its jobs keep their own channels.
			None => {
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
			} else if let Some(key) = service.printers.iter().find(|printer| same(&printer.info, &info)).map(|printer| printer.key) {
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
		// A FRESH GRANT CONTEXT. Sixteen at most, a context whose connection closed still counting while its jobs
		// are charged.
		CONNECT_OP => match channel().filter(|_| service.clients.len() < jobs::MAX_CLIENTS) {
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

fn serve_job(service: &mut Service, at: usize, buf: &mut [u8], reply_buf: &mut [u8]) {
	let chan = service.channels[at].chan;
	let (len, mut handles) = match try_recv_caps(chan, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return,
		// CLOSING NEVER SUBMITS. A job still being written is abandoned and refunded; a submitted one goes on,
		// unwatched; a finished one's record goes.
		PolledCaps::Closed => {
			let channel = service.channels.remove(at);
			close(channel.chan);
			service.spool.close(channel.job);
			return;
		}
	};
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	handles = Handles::new();
	// AN OVERSIZED FRAME IS REFUSED BEFORE DISPATCH, and the job it was meant for is untouched.
	if len > JOB_WRITE_FRAME {
		if len >= 6 {
			refuse(chan, &buf[..len], Error::Invalid);
		}
		return;
	}
	let channel = &service.channels[at];
	let (job, printer) = (channel.job, channel.printer.clone());
	let mut reply_handles = Handles::new();
	let written = job::dispatch(&mut JobView { service, job, printer: &printer }, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
	match written {
		Some(written) => answer(chan, &reply_buf[..written], reply_handles.as_slice()),
		// A request that does not decode is a client that is not speaking this interface: its channel closes,
		// exactly as if it had closed it.
		None => {
			if let Some(at) = service.channels.iter().position(|channel| channel.chan == chan) {
				let channel = service.channels.remove(at);
				close(channel.chan);
				service.spool.close(channel.job);
			}
		}
	}
}
