// CameraService - cameras, their formats, and capture into buffers the client owns, for components
// PermissionManager granted capture of one camera.
//
// WHAT IT HOLDS AND WHAT IT DOES NOT. It consumes every `camera` publication through a catalogue
// connection minted for that kind alone; a provider - the USB Video class module, the in-guest fixture -
// normalizes descriptors and writes frames, and this service owns who may capture, what was negotiated
// and which buffer is whose. It serves inventory on SERVE, which lists and describes and can start nothing,
// and on ADMIN the minting endpoint PermissionManager alone reaches. It allocates no frame storage: every
// buffer is a memory object the client created and keeps paying for, and what the service holds of it is
// a handle's identity and size.
//
// EVERY DECISION IS IN `service_logic::camera_streams`: advertisement and selection checks, the buffer
// and lease states, the stop deadline, the observation sequence and the device clock, all host-tested.
// What is here is the IO around them, and the rule that nothing in it waits on anybody but `wait_any`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{CameraDeviceEvent, CameraDropReason, CameraId, CameraInfo, CaptureEvent, CaptureGrant, CaptureStatus, DeviceTime, Error, FormatInfo, FormatPage, Frame, FrameSize, FrameType, Intervals, Negotiated, ProviderInfo, ProviderKind, StreamRequest, camera, camera_admin, camera_capture, camera_device, provider_catalogue};
use rt::*;
use service_logic::camera_streams as cs;
use wire::{Handles, Reader, Sink, Transport, TransportError};

include!(concat!(env!("OUT_DIR"), "/roles_camera_service.rs"));

// THE CONFIGURED CAMERAS: an alias a policy row may name, and the provider metadata it binds. The one
// alias is the development fixture's, and a configuration that ships binds no fixture, so there it names
// nothing and no capture grant can be minted until somebody configures a camera.
//
// NOT `cfg`-GATED: this service is built once, into the shared image both configurations stage, and that
// build has no development feature - a gated alias was absent from the development image too.
const ALIASES: &[(&str, &[u8])] = &[("fixture", b"org.libersystem.camera-fixture")];

const MAX_ADMINS: usize = 4;
const VERSION: u32 = 1;
const OPEN_TICKS: u64 = 100;
// A capture event stream holds the four completions a stream can have outstanding and room for the
// coalesced status beside them.
const EVENT_DEPTH: u64 = 8;
const BUF_BYTES: usize = 16 * 1024;
// The rights a provider is handed a buffer with: map, read, write, transfer - and nothing else.
const PRODUCER_RIGHTS: u32 = RIGHT_MAP | RIGHT_READ | RIGHT_WRITE | RIGHT_TRANSFER;

// ------------------------------------------------------------------ what the service holds

// A provider call in flight, and who waits for it.
enum Pending {
	Negotiate { grant: u32, corr: u32, expected: cs::Selected, generation: u64 },
	Register { grant: u32, corr: u32, generation: u64, buffer: u8 },
	Queue,
	Start { grant: u32, corr: u32, generation: u64 },
	Stop { generation: u64 },
	// A stream that never ran, let go: the producer releases its mappings, and nobody waits for the answer.
	Discard,
}

// The one stream a camera may carry, and the grant it belongs to.
struct Run {
	grant: u32,
	stream: cs::Stream,
	clock: cs::DeviceClock,
	// The client call waiting for `stop`'s confirmation.
	stopping: Option<u32>,
	ended: Option<Error>,
	status_due: bool,
}

struct CameraConn {
	key: u32,
	info: ProviderInfo,
	chan: u64,
	events: u64,
	name: String,
	generation: u64,
	formats: Vec<cs::Format>,
	wire_formats: Vec<FormatInfo>,
	wire_sizes: Vec<Vec<FrameSize>>,
	run: Option<Run>,
	sent: Vec<(u32, Pending)>,
	next_corr: u32,
}

struct GrantConn {
	id: u32,
	chan: u64,
	owner: u64,
	camera: u32,
	camera_id: CameraId,
	events: u64,
	event_seq: u32,
	// Completions its stream had no room for, oldest first. At most one per registered buffer of the running
	// stream: `completion` lets a newer one for the same buffer, or one from a newer stream, replace it.
	frames: Vec<CaptureEvent>,
}

struct Service {
	incarnation: u64,
	catalogue: u64,
	cameras: Vec<CameraConn>,
	grants: Vec<GrantConn>,
	observers: Vec<u64>,
	admins: Vec<u64>,
	next_key: u32,
	next_grant: u32,
	next_stream: u64,
}

// ------------------------------------------------------------------ the wire, written by hand

// A generated client's request, captured instead of sent - with the capabilities it carried, which are
// still this service's to send or to close.
struct Capture {
	bytes: Vec<u8>,
	handles: Vec<u64>,
}

impl Transport for &mut Capture {
	fn call(&mut self, request: &[u8], request_handles: &[u64], _reply_handles: &mut Handles, _deadline: u64) -> Result<Vec<u8>, TransportError> {
		self.bytes = request.to_vec();
		self.handles = request_handles.to_vec();
		Err(TransportError::TimedOut)
	}
	fn discard_handles(&mut self, _handles: &[u64]) {}
}

fn captured(encode: impl FnOnce(&mut camera_device::Client<&mut Capture>), corr: u32) -> Option<(Vec<u8>, Vec<u64>)> {
	let mut capture = Capture { bytes: Vec::new(), handles: Vec::new() };
	encode(&mut camera_device::Client::new(&mut capture));
	if capture.bytes.len() < 6 {
		for &handle in &capture.handles {
			close(handle);
		}
		return None;
	}
	capture.bytes[2..6].copy_from_slice(&corr.to_le_bytes());
	Some((capture.bytes, capture.handles))
}

// `result<T, error>` under a correlation, sent without waiting.
fn reply<T>(chan: u64, corr: u32, result: Result<T, Error>, write: impl FnOnce(&T, &mut wire::VecWriter) -> Option<()>) {
	let mut writer = wire::VecWriter::new();
	let encoded = (|| {
		writer.u32(corr)?;
		match &result {
			Ok(value) => {
				writer.u8(1)?;
				write(value, &mut writer)
			}
			Err(error) => {
				writer.u8(0)?;
				error.write(&mut writer)
			}
		}
	})();
	if encoded.is_some()
		&& let Some(bytes) = writer.into_inner()
	{
		let _ = try_send(chan, &bytes, 0);
	}
}

fn refusal(refused: cs::Refusal) -> Error {
	match refused {
		cs::Refusal::Unsupported => Error::Unsupported,
		cs::Refusal::TooLarge => Error::Exhausted,
		cs::Refusal::Busy => Error::Again,
		cs::Refusal::Stale => Error::Stale,
		cs::Refusal::Invalid => Error::Invalid,
		cs::Refusal::NotFound => Error::NotFound,
		cs::Refusal::Unavailable => Error::Io,
	}
}

fn same(a: &ProviderInfo, b: &ProviderInfo) -> bool {
	a.slot == b.slot && a.provider_generation == b.provider_generation && a.binding_generation == b.binding_generation
}

fn kind_of(frame_type: FrameType) -> cs::Kind {
	match frame_type {
		FrameType::Yuy2 => cs::Kind::Yuy2,
		FrameType::Mjpeg => cs::Kind::Mjpeg,
	}
}

fn intervals_of(intervals: &Intervals) -> cs::Intervals {
	match intervals {
		Intervals::Discrete(list) => cs::Intervals::Discrete(list.values.iter().map(|interval| (interval.numerator, interval.denominator)).collect()),
		Intervals::Stepwise(range) => cs::Intervals::Stepwise { minimum: (range.minimum.numerator, range.minimum.denominator), maximum: (range.maximum.numerator, range.maximum.denominator), step: (range.step.numerator, range.step.denominator) },
	}
}

fn selected_of(negotiated: &Negotiated) -> cs::Selected {
	cs::Selected { format: negotiated.format, kind: kind_of(negotiated.frame_type), width: negotiated.width, height: negotiated.height, interval: (negotiated.interval.numerator, negotiated.interval.denominator), max_bytes: negotiated.max_bytes, stride: negotiated.stride, plane_offset: negotiated.plane_offset }
}

// ------------------------------------------------------------------ inventory and grants

impl Service {
	fn camera_id(&self, camera: &CameraConn) -> CameraId {
		CameraId { slot: camera.info.slot, generation: camera.info.provider_generation, binding_generation: camera.info.binding_generation, incarnation: self.incarnation }
	}

	fn info(&self, camera: &CameraConn) -> CameraInfo {
		let run = camera.run.as_ref();
		let streaming = run.is_some_and(|run| matches!(run.stream.phase(), cs::Phase::Streaming | cs::Phase::Stopping { .. }));
		let available = !run.is_some_and(|run| run.stream.phase() == cs::Phase::Quarantined);
		CameraInfo { id: self.camera_id(camera), name: camera.name.clone(), formats: camera.wire_formats.clone(), generation: camera.generation, streaming, available }
	}

	fn page(&self, camera: &CameraConn, generation: u64, format: u8, offset: u8) -> Result<FormatPage, Error> {
		if generation != camera.generation {
			return Err(Error::Stale);
		}
		let (found, sizes) = cs::page(&camera.formats, format, offset).map_err(refusal)?;
		let at = camera.formats.iter().position(|candidate| candidate.index == found.index).ok_or(Error::NotFound)?;
		let start = usize::from(offset);
		let page: Vec<FrameSize> = camera.wire_sizes[at][start..start + sizes.len()].to_vec();
		Ok(FormatPage { generation, format: camera.wire_formats[at].clone(), offset, sizes: page, total: camera.wire_sizes[at].len() as u8 })
	}

	fn clients(&self) -> usize {
		self.observers.len() + self.grants.len()
	}

	fn grant_at(&self, id: u32) -> Option<&GrantConn> {
		self.grants.iter().find(|grant| grant.id == id)
	}

	fn answer<T>(&self, grant: u32, corr: u32, result: Result<T, Error>, write: impl FnOnce(&T, &mut wire::VecWriter) -> Option<()>) {
		if let Some(grant) = self.grant_at(grant) {
			reply(grant.chan, corr, result, write);
		}
	}

	fn status(run: &Run) -> CaptureStatus {
		let stream = &run.stream;
		CaptureStatus { stream_generation: stream.generation, streaming: stream.phase() == cs::Phase::Streaming, next_sequence: stream.next_sequence(), dropped_no_buffer: stream.dropped_no_buffer, dropped_device: stream.dropped_device, unknown_discontinuities: stream.unknown_discontinuities, ended: run.ended }
	}

	// An event to a grant's stream, without waiting. A full stream is the client's own backlog: frames are
	// bounded by its buffers, and a status that does not fit is sent again when there is room.
	fn event(&mut self, grant: u32, event: &CaptureEvent) -> bool {
		let Some(at) = self.grants.iter().position(|held| held.id == grant) else { return false };
		let grant = &mut self.grants[at];
		if grant.events == 0 {
			return false;
		}
		let mut frame = [0u8; 512];
		let mut handles = Handles::new();
		let Some(len) = camera_capture::events_frame(grant.event_seq, event, &mut frame, &mut handles) else { return false };
		match try_send_outcome(grant.events, &frame[..len], 0) {
			SendOutcome::Delivered => {
				grant.event_seq = grant.event_seq.wrapping_add(1);
				true
			}
			SendOutcome::Stalled => false,
			SendOutcome::Failed => {
				close(grant.events);
				grant.events = 0;
				false
			}
		}
	}

	// A COMPLETION IS NEVER DROPPED WHILE IT NAMES A BUFFER THE CLIENT HOLDS: a client that never heard of it
	// could never give that buffer back - the stream would lose it for good. One that does not fit waits on
	// the grant and goes out before anything else does.
	//
	// AND AT MOST ONE WAITS PER BUFFER OF THE RUNNING STREAM. A new completion supersedes a waiting one for
	// the same buffer - the camera refilled it, so the client gave that lease back without reading of it -
	// and any waiting from an earlier stream, whose leases nothing can return any more. Without that, a
	// client that returns leases it guessed, or renegotiates, while never reading its events grows this
	// list for as long as it keeps doing so.
	fn completion(&mut self, grant: u32, event: CaptureEvent) {
		let Some(at) = self.grants.iter().position(|held| held.id == grant) else { return };
		if let CaptureEvent::Frame(frame) = &event {
			self.grants[at].frames.retain(|waiting| !matches!(waiting, CaptureEvent::Frame(old) if old.buffer == frame.buffer || old.stream_generation != frame.stream_generation));
		}
		self.grants[at].frames.push(event);
		self.flush(grant);
	}

	// Waiting completions, oldest first, until one does not fit. True when none is left waiting. A stream
	// that is gone takes its waiting completions with it: nothing will read them.
	fn flush(&mut self, grant: u32) -> bool {
		loop {
			let Some(at) = self.grants.iter().position(|held| held.id == grant) else { return true };
			if self.grants[at].events == 0 {
				self.grants[at].frames.clear();
				return true;
			}
			let Some(next) = self.grants[at].frames.first().cloned() else { return true };
			if !self.event(grant, &next) {
				return false;
			}
			if let Some(at) = self.grants.iter().position(|held| held.id == grant) {
				self.grants[at].frames.remove(0);
			}
		}
	}

	// Send whatever status is due, coalesced to the newest - AFTER every waiting completion, and only once
	// none is left. The status is a snapshot kept outside the stream (`status` reads it too), so a saturated
	// stream delays it and loses nothing, while a status sent ahead of a completion took that completion's
	// room.
	fn statuses(&mut self) {
		let grants: Vec<u32> = self.grants.iter().filter(|grant| !grant.frames.is_empty()).map(|grant| grant.id).collect();
		let waiting: Vec<u32> = grants.into_iter().filter(|&grant| !self.flush(grant)).collect();
		let due: Vec<(u32, u32, CaptureStatus)> = self.cameras.iter().filter_map(|camera| camera.run.as_ref().filter(|run| run.status_due && !waiting.contains(&run.grant)).map(|run| (camera.key, run.grant, Service::status(run)))).collect();
		for (key, grant, status) in due {
			if self.event(grant, &CaptureEvent::Status(status))
				&& let Some(run) = self.cameras.iter_mut().find(|camera| camera.key == key).and_then(|camera| camera.run.as_mut())
			{
				run.status_due = false;
			}
		}
	}

	fn statuses_due(&self) -> bool {
		self.cameras.iter().any(|camera| camera.run.as_ref().is_some_and(|run| run.status_due)) || self.grants.iter().any(|grant| !grant.frames.is_empty())
	}

	// A GRANT ENDS: its connection closes, its owner's observer is let go, and its stream stops - the
	// same stop a client asks for, answered to nobody. Whatever copies of its endpoint live on name
	// nothing afterwards.
	fn retire(&mut self, id: u32, reason: Error) {
		let Some(at) = self.grants.iter().position(|grant| grant.id == id) else { return };
		let grant = self.grants.remove(at);
		for handle in [grant.chan, grant.owner, grant.events] {
			if handle != 0 {
				close(handle);
			}
		}
		if let Some(camera) = self.cameras.iter().position(|camera| camera.key == grant.camera) {
			self.end_stream(camera, id, reason);
		}
	}

	// Stop the stream a grant holds on a camera, internally: no answer is owed to anybody.
	fn end_stream(&mut self, camera: usize, grant: u32, reason: Error) {
		let Some(run) = self.cameras[camera].run.as_mut() else { return };
		if run.grant != grant {
			return;
		}
		run.ended = Some(reason);
		match run.stream.phase() {
			cs::Phase::Streaming => {
				let generation = run.stream.generation;
				let _ = run.stream.stop(clock());
				self.send(camera, Pending::Stop { generation }, |client| {
					let _ = client.stop(&generation);
				});
			}
			// Never started: nothing the producer writes to, but it maps what was registered.
			cs::Phase::Ready => {
				let generation = run.stream.generation;
				self.cameras[camera].run = None;
				self.discard(camera, generation);
			}
			// Already over: the stop that ended it released everything.
			cs::Phase::Retired => self.cameras[camera].run = None,
			// Still waiting for the producer: the stream stays until it confirms or the camera goes.
			cs::Phase::Stopping { .. } | cs::Phase::Quarantined => {}
		}
	}

	// A STREAM THAT ENDS BEFORE IT STARTED is still one the producer mapped buffers for, and a producer told
	// nothing keeps them - a dead client's memory among them - until somebody negotiates on this camera
	// again. `stop` is answered once every mapping is released, and needs no running stream.
	fn discard(&mut self, camera: usize, generation: u64) {
		self.send(camera, Pending::Discard, |client| {
			let _ = client.stop(&generation);
		});
	}

	// ------------------------------------------------------------------ providers

	// One provider call, without waiting. NOT SENT IS ANSWERED AT ONCE through the same path a refusal
	// takes, and a capability it carried is closed.
	fn send(&mut self, camera: usize, pending: Pending, encode: impl FnOnce(&mut camera_device::Client<&mut Capture>)) {
		let corr = self.cameras[camera].next_corr;
		self.cameras[camera].next_corr = self.cameras[camera].next_corr.wrapping_add(1).max(1);
		let chan = self.cameras[camera].chan;
		let sent = match captured(encode, corr) {
			Some((bytes, handles)) => {
				let carried = handles.first().copied().unwrap_or(0);
				for &extra in handles.iter().skip(1) {
					close(extra);
				}
				let delivered = try_send(chan, &bytes, carried);
				if !delivered && carried != 0 {
					close(carried);
				}
				delivered
			}
			None => false,
		};
		if sent {
			self.cameras[camera].sent.push((corr, pending));
		} else {
			self.provider_answer(camera, pending, Err(Error::Io));
		}
	}

	// A publication: open a session and read its formats - bounded, and every one of them checked before
	// it is offered.
	fn adopt(&mut self, info: ProviderInfo) {
		if self.cameras.iter().any(|camera| same(&camera.info, &info)) {
			return;
		}
		if self.cameras.len() >= cs::MAX_CAMERAS {
			print(b"CameraService: a camera was refused: this service holds four (resource exhausted)\n");
			return;
		}
		let Some(Ok(chan)) = provider_catalogue::Client::new(ChannelTransport { chan: self.catalogue }).open(&info) else {
			print(b"CameraService: a published camera could not be opened\n");
			return;
		};
		let mut client = camera_device::Client::with_deadline(ChannelTransport { chan }, clock() + OPEN_TICKS);
		let opened = match client.open(&VERSION) {
			Some(Ok(opened)) => opened,
			_ => {
				print(b"CameraService: a camera did not open a session at this contract's version\n");
				close(chan);
				return;
			}
		};
		let wire_formats = match client.formats() {
			Some(Ok(formats)) if formats.len() <= cs::MAX_FORMATS => formats,
			_ => {
				print(b"CameraService: a camera did not list its formats within the bound\n");
				close(chan);
				return;
			}
		};
		let mut wire_sizes: Vec<Vec<FrameSize>> = Vec::new();
		let mut formats: Vec<cs::Format> = Vec::new();
		for format in &wire_formats {
			let sizes = match client.sizes(&format.index) {
				Some(Ok(sizes)) if sizes.len() == usize::from(format.sizes) => sizes,
				_ => {
					print(b"CameraService: a camera's frame sizes did not match its formats\n");
					close(chan);
					return;
				}
			};
			formats.push(cs::Format { index: format.index, kind: kind_of(format.frame_type), sizes: sizes.iter().map(|size| cs::Size { index: size.index, width: size.width, height: size.height, max_bytes: size.max_bytes, intervals: intervals_of(&size.intervals) }).collect() });
			wire_sizes.push(sizes);
		}
		// CHECKED, NOT BELIEVED: the advertisement is held to the normalizer's own bounds.
		if cs::validate(&formats).is_err() || wire_formats.iter().any(|format| (format.frame_type == FrameType::Mjpeg) != format.compressed) {
			print(b"CameraService: a camera's advertisement broke its bounds and was refused\n");
			close(chan);
			return;
		}
		let events = client.events().unwrap_or(0);
		if events == 0 {
			close(chan);
			return;
		}
		let key = self.next_key;
		self.next_key = self.next_key.wrapping_add(1).max(1);
		self.cameras.push(CameraConn { key, info, chan, events, name: opened.name, generation: opened.generation, formats, wire_formats, wire_sizes, run: None, sent: Vec::new(), next_corr: 1 });
		print(b"CameraService: a camera was admitted\n");
	}

	// A camera is gone - withdrawn, or its connection closed. Its stream ends and every grant bound to it:
	// a replacement is a new camera and a new authority decision, and a stale handle rebinds to nothing.
	fn lose(&mut self, key: u32, why: &[u8]) {
		let Some(at) = self.cameras.iter().position(|camera| camera.key == key) else { return };
		let camera = self.cameras.remove(at);
		close(camera.events);
		close(camera.chan);
		// Whoever waited for a stop is answered: the producer is dead, which is the confirmation.
		if let Some(run) = camera.run.as_ref()
			&& let Some(corr) = run.stopping
		{
			self.answer::<()>(run.grant, corr, Ok(()), |_, _| Some(()));
		}
		print(b"CameraService: a camera is gone: ");
		print(why);
		print(b"\n");
		let bound: Vec<u32> = self.grants.iter().filter(|grant| grant.camera == key).map(|grant| grant.id).collect();
		for id in bound {
			self.retire(id, Error::Closed);
		}
	}

	// A provider's answer to a call, or the refusal a call that never went out is given.
	fn provider_answer(&mut self, camera: usize, pending: Pending, result: Result<Option<Negotiated>, Error>) {
		match pending {
			Pending::Negotiate { grant, corr, expected, generation } => {
				let answer = match result {
					// EXACTLY WHAT WAS ASKED, OR NOTHING: a provider that selects something else - or answers
					// for another stream - has selected nothing, and capture stays stopped.
					Ok(Some(negotiated)) if negotiated.stream_generation != generation => Err(Error::Invalid),
					Ok(Some(negotiated)) => match cs::verify(&expected, &selected_of(&negotiated)) {
						Ok(()) if self.cameras[camera].run.is_some() => Err(Error::Again),
						Ok(()) => {
							self.cameras[camera].run = Some(Run { grant, stream: cs::Stream::new(generation, expected), clock: cs::DeviceClock::default(), stopping: None, ended: None, status_due: false });
							Ok(negotiated)
						}
						Err(refused) => Err(refusal(refused)),
					},
					Ok(None) => Err(Error::Io),
					Err(error) => Err(error),
				};
				self.answer(grant, corr, answer, |negotiated, w| negotiated.write(w));
			}
			Pending::Register { grant, corr, generation, buffer } => {
				if result.is_err()
					&& let Some(run) = self.cameras[camera].run.as_mut()
					&& run.stream.generation == generation
				{
					run.stream.forget(buffer);
				}
				self.answer(grant, corr, result.map(|_| buffer), |buffer, w| w.u8(*buffer));
			}
			Pending::Queue | Pending::Discard => {}
			Pending::Start { grant, corr, generation } => {
				if let Err(error) = result {
					// A START THE PRODUCER REFUSED unwinds: nothing was written, and every buffer comes back -
					// and the producer lets go of the mappings it was given for them.
					if let Some(run) = self.cameras[camera].run.as_mut()
						&& run.stream.generation == generation
					{
						run.stream.stopped();
						self.discard(camera, generation);
					}
					self.answer::<()>(grant, corr, Err(error), |_, _| Some(()));
					return;
				}
				self.answer(grant, corr, Ok(()), |_, _| Some(()));
			}
			Pending::Stop { generation } => {
				let Some(run) = self.cameras[camera].run.as_mut() else { return };
				if run.stream.generation != generation {
					return;
				}
				// CONFIRMED - on time, or late after a quarantine: every buffer is back.
				run.stream.stopped();
				run.status_due = true;
				let (grant, stopping) = (run.grant, run.stopping.take());
				if let Some(corr) = stopping {
					self.answer::<()>(grant, corr, Ok(()), |_, _| Some(()));
				}
				self.statuses();
				// A grant that is gone leaves nothing to keep.
				if self.grant_at(grant).is_none() {
					self.cameras[camera].run = None;
				}
			}
		}
	}

	fn on_reply(&mut self, key: u32, buf: &mut [u8]) -> Result<(), &'static [u8]> {
		loop {
			let Some(camera) = self.cameras.iter().position(|camera| camera.key == key) else { return Ok(()) };
			let (len, handles) = match try_recv_caps(self.cameras[camera].chan, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return Ok(()),
				PolledCaps::Closed => return Err(b"its connection closed"),
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut reader = Reader::new(&buf[..len]);
			let Some(corr) = reader.u32() else { return Err(b"a reply carried no correlation") };
			let Some(at) = self.cameras[camera].sent.iter().position(|(sent, _)| *sent == corr) else { continue };
			let (_, pending) = self.cameras[camera].sent.remove(at);
			let Some(ok) = reader.tag() else { return Err(b"a reply did not decode") };
			let result = if !ok {
				Err(Error::read(&mut reader).unwrap_or(Error::Io))
			} else if matches!(pending, Pending::Negotiate { .. }) {
				match Negotiated::read(&mut reader) {
					Some(negotiated) => Ok(Some(negotiated)),
					None => return Err(b"a negotiation did not decode"),
				}
			} else {
				Ok(None)
			};
			self.provider_answer(camera, pending, result);
		}
	}

	// What the producer delivered, dropped or lost, checked against the stream before a client hears of it.
	fn on_events(&mut self, key: u32, buf: &mut [u8]) -> Result<(), &'static [u8]> {
		loop {
			let Some(camera) = self.cameras.iter().position(|camera| camera.key == key) else { return Ok(()) };
			let (len, handles) = match try_recv_caps(self.cameras[camera].events, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return Ok(()),
				PolledCaps::Closed => return Err(b"its event stream closed"),
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut frame_handles = Handles::new();
			let Some(event) = camera_device::events_read(&buf[..len], &mut frame_handles) else { return Err(b"an event did not decode") };
			let Some(run) = self.cameras[camera].run.as_mut() else { continue };
			match event {
				CameraDeviceEvent::Frame(frame) => {
					if frame.stream_generation != run.stream.generation {
						continue;
					}
					// FROM THE FUTURE IS FROM NOWHERE: a host arrival time is one already past.
					if frame.arrival_ns > clock_ns() {
						return Err(b"a frame arrived from the future");
					}
					let Ok(delivered) = run.stream.completed(frame.buffer, frame.lease, frame.sequence, frame.valid_bytes) else { continue };
					let device = match frame.device {
						Some(time) => {
							let reading = cs::DeviceTime { ticks: time.ticks, width_bits: time.width_bits, frequency_hz: time.frequency_hz, clock_domain: time.clock_domain, reset_generation: time.reset_generation };
							if cs::DeviceClock::valid(&reading) {
								let generation = run.clock.observe(reading);
								Some(DeviceTime { reset_generation: generation, ..time })
							} else {
								None
							}
						}
						None => None,
					};
					let grant = run.grant;
					let event = CaptureEvent::Frame(Frame { stream_generation: frame.stream_generation, sequence: delivered.sequence, buffer: delivered.buffer, lease: delivered.lease, valid_bytes: delivered.valid_bytes, arrival_ns: frame.arrival_ns, device });
					run.status_due = true;
					self.completion(grant, event);
				}
				CameraDeviceEvent::Dropped(drop) => {
					if drop.stream_generation != run.stream.generation {
						continue;
					}
					let reason = if drop.reason == CameraDropReason::NoBuffer { cs::DropReason::NoBuffer } else { cs::DropReason::Device };
					if run.stream.dropped(drop.first_sequence, drop.count, reason).is_ok() {
						run.status_due = true;
					}
				}
				CameraDeviceEvent::Gap(gap) => {
					if gap.stream_generation != run.stream.generation {
						continue;
					}
					if run.stream.gap(gap.next_sequence).is_ok() {
						run.clock.discontinuity();
						run.status_due = true;
					}
				}
			}
		}
	}
}

// ------------------------------------------------------------------ the views the generated code calls

// PermissionManager's minting endpoint.
struct AdminView<'a> {
	service: &'a mut Service,
}

impl camera_admin::Service for AdminView<'_> {
	// THE ALIAS RESOLVES TO EXACTLY ONE CURRENT PUBLICATION OR THE MINT FAILS. Minting starts nothing.
	fn mint(&mut self, alias: String, owner: u64) -> Result<CaptureGrant, Error> {
		let refuse = |error: Error| {
			close(owner);
			Err(error)
		};
		let Some(&(_, bound)) = ALIASES.iter().find(|(name, _)| *name == alias) else { return refuse(Error::NotFound) };
		let service = &mut *self.service;
		let matching: Vec<usize> = service.cameras.iter().enumerate().filter(|(_, camera)| camera.info.name.as_bytes() == bound).map(|(at, _)| at).collect();
		let at = match matching.as_slice() {
			[at] => *at,
			[] => return refuse(Error::NotFound),
			_ => return refuse(Error::Invalid),
		};
		if service.clients() >= cs::MAX_CLIENTS {
			return refuse(Error::Exhausted);
		}
		let Some((mine, theirs)) = channel() else { return refuse(Error::Exhausted) };
		let id = service.next_grant;
		service.next_grant = service.next_grant.wrapping_add(1).max(1);
		let camera_id = service.camera_id(&service.cameras[at]);
		let key = service.cameras[at].key;
		service.grants.push(GrantConn { id, chan: mine, owner, camera: key, camera_id: camera_id.clone(), events: 0, event_seq: 0, frames: Vec::new() });
		Ok(CaptureGrant { connection: theirs, camera: camera_id })
	}

	fn revoke(&mut self, camera: CameraId) -> Result<u32, Error> {
		let service = &mut *self.service;
		let revoked: Vec<u32> = service.grants.iter().filter(|grant| grant.camera_id == camera).map(|grant| grant.id).collect();
		for &id in &revoked {
			service.retire(id, Error::Denied);
		}
		Ok(revoked.len() as u32)
	}
}

// Inventory: a list and pages of formats, and nothing that starts or reaches a frame.
struct InventoryView<'a> {
	service: &'a Service,
}

impl camera::Service for InventoryView<'_> {
	fn cameras(&mut self) -> Result<Vec<CameraInfo>, Error> {
		Ok(self.service.cameras.iter().map(|camera| self.service.info(camera)).collect())
	}
	fn sizes(&mut self, camera: CameraId, generation: u64, format: u8, offset: u8) -> Result<FormatPage, Error> {
		let found = self.service.cameras.iter().find(|candidate| self.service.camera_id(candidate) == camera).ok_or(Error::NotFound)?;
		self.service.page(found, generation, format, offset)
	}
}

// What a capture connection asked for that is answered once the provider has.
enum Asked {
	Negotiate(StreamRequest),
	Register(u64),
	Start,
	Release(u8, u64),
	Stop,
}

struct CaptureView<'a> {
	service: &'a Service,
	grant: usize,
	asked: Option<Asked>,
}

impl CaptureView<'_> {
	fn camera(&self) -> Result<&CameraConn, Error> {
		let key = self.service.grants[self.grant].camera;
		self.service.cameras.iter().find(|camera| camera.key == key).ok_or(Error::Closed)
	}
}

impl camera_capture::Service for CaptureView<'_> {
	fn camera(&mut self) -> Result<CameraInfo, Error> {
		Ok(self.service.info(CaptureView::camera(self)?))
	}
	fn sizes(&mut self, generation: u64, format: u8, offset: u8) -> Result<FormatPage, Error> {
		self.service.page(CaptureView::camera(self)?, generation, format, offset)
	}
	fn negotiate(&mut self, request: StreamRequest) -> Result<Negotiated, Error> {
		self.asked = Some(Asked::Negotiate(request));
		Err(Error::Again)
	}
	fn register(&mut self, buffer: u64) -> Result<u8, Error> {
		self.asked = Some(Asked::Register(buffer));
		Err(Error::Again)
	}
	fn start(&mut self) -> Result<(), Error> {
		self.asked = Some(Asked::Start);
		Err(Error::Again)
	}
	fn events(&mut self) -> Vec<CaptureEvent> {
		Vec::new()
	}
	fn release(&mut self, buffer: u8, lease: u64) -> Result<(), Error> {
		self.asked = Some(Asked::Release(buffer, lease));
		Err(Error::Again)
	}
	fn stop(&mut self) -> Result<(), Error> {
		self.asked = Some(Asked::Stop);
		Err(Error::Again)
	}
	fn status(&mut self) -> Result<CaptureStatus, Error> {
		let id = self.service.grants[self.grant].id;
		let run = CaptureView::camera(self)?.run.as_ref().filter(|run| run.grant == id).ok_or(Error::NotFound)?;
		Ok(Service::status(run))
	}
}

impl Service {
	// A request on a grant's connection. False when the connection cannot carry it, which closes it.
	fn capture(&mut self, at: usize, request: &[u8], handles: &mut Handles, reply_buf: &mut [u8]) -> bool {
		let (chan, id, key) = (self.grants[at].chan, self.grants[at].id, self.grants[at].camera);
		if request.len() >= 2 && u16::from_le_bytes([request[0], request[1]]) == camera_capture::OP_EVENTS {
			let mut view = CaptureView { service: self, grant: at, asked: None };
			let Some((corr, _)) = camera_capture::events_open(&mut view, request, handles) else { return false };
			let Some((producer, consumer)) = channel_with_depth(EVENT_DEPTH) else { return true };
			let grant = &mut self.grants[at];
			if grant.events != 0 {
				close(grant.events);
			}
			grant.events = producer;
			grant.event_seq = 0;
			send_caps_blocking(chan, &corr.to_le_bytes(), &[consumer]);
			return true;
		}
		let mut view = CaptureView { service: self, grant: at, asked: None };
		let mut reply_handles = Handles::new();
		let written = camera_capture::dispatch(&mut view, request, handles, reply_buf, &mut reply_handles);
		let asked = view.asked.take();
		let Some(written) = written else {
			if let Some(Asked::Register(buffer)) = asked {
				close(buffer);
			}
			return false;
		};
		let Some(asked) = asked else {
			if !send_caps_blocking(chan, &reply_buf[..written], reply_handles.as_slice()) {
				for &leftover in reply_handles.as_slice() {
					close(leftover);
				}
			}
			return true;
		};
		let corr = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
		let Some(camera) = self.cameras.iter().position(|camera| camera.key == key) else {
			if let Asked::Register(buffer) = asked {
				close(buffer);
			}
			reply::<()>(chan, corr, Err(Error::Closed), |_, _| Some(()));
			return true;
		};
		let result: Result<(), Error> = match asked {
			Asked::Negotiate(request) => self.negotiate(camera, id, corr, request),
			Asked::Register(buffer) => self.register(camera, id, corr, buffer),
			Asked::Start => self.start(camera, id, corr),
			Asked::Release(buffer, lease) => self.release(camera, id, corr, buffer, lease),
			Asked::Stop => self.stop(camera, id, corr),
		};
		if let Err(error) = result {
			reply::<()>(chan, corr, Err(error), |_, _| Some(()));
		}
		true
	}

	// ONE STREAM PER CAMERA, for anybody; and renegotiating needs the previous stream retired first.
	fn negotiate(&mut self, camera: usize, grant: u32, corr: u32, request: StreamRequest) -> Result<(), Error> {
		if let Some(run) = self.cameras[camera].run.as_ref() {
			match run.stream.phase() {
				cs::Phase::Quarantined => return Err(Error::Io),
				// A RETIRED STREAM IS OVER, whoever ran it: its buffers are back and nothing writes them.
				cs::Phase::Retired => self.cameras[camera].run = None,
				// Anything else is a live stream - another grant's, or this one's own not yet stopped.
				_ => return Err(Error::Again),
			}
		}
		if self.cameras[camera].sent.iter().any(|(_, pending)| matches!(pending, Pending::Negotiate { .. })) {
			return Err(Error::Again);
		}
		let wanted = cs::Request { format: request.format, size: request.size, interval: (request.interval.numerator, request.interval.denominator) };
		let expected = cs::expect(&self.cameras[camera].formats, wanted).map_err(refusal)?;
		// NUMBERED BEFORE IT IS ASKED FOR, so the provider's answer names the stream it is for.
		let generation = self.next_stream;
		self.next_stream += 1;
		self.send(camera, Pending::Negotiate { grant, corr, expected, generation }, |client| {
			let _ = client.negotiate(&generation, &request);
		});
		Ok(())
	}

	// THE CLIENT'S OWN MEMORY, CHECKED before a capability of it goes anywhere: a memory object, its real
	// size, and rights narrowed to what the producer needs - never execute, never more.
	fn register(&mut self, camera: usize, grant: u32, corr: u32, buffer: u64) -> Result<(), Error> {
		let refuse = |error: Error| {
			close(buffer);
			Err(error)
		};
		let Some(object) = object_info(buffer) else { return refuse(Error::Invalid) };
		if object.object_type != OBJECT_TYPE_MEMORY_OBJECT {
			return refuse(Error::Invalid);
		}
		let elsewhere: u64 = self.cameras.iter().enumerate().filter(|(at, _)| *at != camera).filter_map(|(_, other)| other.run.as_ref()).map(|run| run.stream.registered()).sum();
		let Some(run) = self.cameras[camera].run.as_mut().filter(|run| run.grant == grant) else { return refuse(Error::NotFound) };
		let id = match run.stream.register(object.koid, object.size, elsewhere) {
			Ok(id) => id,
			Err(refused) => return refuse(refusal(refused)),
		};
		let generation = run.stream.generation;
		// NARROWED to map, read, write and transfer. A handle that carries more and cannot be narrowed is
		// refused rather than handed on as it is.
		let forwarded = if object.rights & !PRODUCER_RIGHTS == 0 {
			buffer
		} else if object.rights & RIGHT_DUPLICATE != 0 {
			let narrowed = duplicate(buffer, PRODUCER_RIGHTS);
			close(buffer);
			if narrowed < 0 {
				run.stream.forget(id);
				return Err(Error::Invalid);
			}
			narrowed as u64
		} else {
			run.stream.forget(id);
			return refuse(Error::Invalid);
		};
		self.send(camera, Pending::Register { grant, corr, generation, buffer: id }, |client| {
			let _ = client.register(&generation, &id, &forwarded);
		});
		Ok(())
	}

	fn start(&mut self, camera: usize, grant: u32, corr: u32) -> Result<(), Error> {
		// A CLIENT WITH NOWHERE TO HEAR OF A FRAME could never give one back: the events stream first.
		if self.grant_at(grant).is_none_or(|held| held.events == 0) {
			return Err(Error::Invalid);
		}
		let Some(run) = self.cameras[camera].run.as_mut().filter(|run| run.grant == grant) else { return Err(Error::NotFound) };
		let generation = run.stream.generation;
		let queued = run.stream.start().map_err(refusal)?;
		for (buffer, lease) in queued {
			self.send(camera, Pending::Queue, |client| {
				let _ = client.queue(&generation, &buffer, &lease);
			});
		}
		self.send(camera, Pending::Start { grant, corr, generation }, |client| {
			let _ = client.start(&generation);
		});
		Ok(())
	}

	fn release(&mut self, camera: usize, grant: u32, corr: u32, buffer: u8, lease: u64) -> Result<(), Error> {
		let Some(run) = self.cameras[camera].run.as_mut().filter(|run| run.grant == grant) else { return Err(Error::NotFound) };
		let generation = run.stream.generation;
		let requeue = run.stream.release(buffer, lease).map_err(refusal)?;
		if let Some((buffer, lease)) = requeue {
			self.send(camera, Pending::Queue, |client| {
				let _ = client.queue(&generation, &buffer, &lease);
			});
		}
		self.answer(grant, corr, Ok(()), |_, _| Some(()));
		Ok(())
	}

	// STOP IS ANSWERED ONCE EVERY BUFFER IS BACK - or, at the two-second deadline, as timed out with the
	// camera quarantined.
	fn stop(&mut self, camera: usize, grant: u32, corr: u32) -> Result<(), Error> {
		let Some(run) = self.cameras[camera].run.as_mut().filter(|run| run.grant == grant) else { return Err(Error::NotFound) };
		match run.stream.phase() {
			// NEVER STARTED: answered at once, since the producer never wrote to it - and the producer is told
			// to let go of what it mapped.
			cs::Phase::Ready => {
				let generation = run.stream.generation;
				let _ = run.stream.stop(clock());
				self.discard(camera, generation);
				self.answer(grant, corr, Ok(()), |_, _| Some(()));
				Ok(())
			}
			cs::Phase::Retired => {
				self.answer(grant, corr, Ok(()), |_, _| Some(()));
				Ok(())
			}
			cs::Phase::Streaming => {
				let generation = run.stream.generation;
				let _ = run.stream.stop(clock());
				run.stopping = Some(corr);
				self.send(camera, Pending::Stop { generation }, |client| {
					let _ = client.stop(&generation);
				});
				Ok(())
			}
			cs::Phase::Stopping { .. } => Err(Error::Again),
			cs::Phase::Quarantined => Err(Error::Io),
		}
	}

	// The stop deadline: a producer that did not confirm in time leaves its camera quarantined.
	fn tick(&mut self) {
		let now = clock();
		for at in 0..self.cameras.len() {
			let Some(run) = self.cameras[at].run.as_mut() else { continue };
			if run.stream.tick(now) {
				print(b"CameraService: a stop was not confirmed in time - the camera is quarantined\n");
				run.status_due = true;
				let (grant, stopping) = (run.grant, run.stopping.take());
				if let Some(corr) = stopping {
					self.answer::<()>(grant, corr, Err(Error::TimedOut), |_, _| Some(()));
				}
			}
		}
	}

	fn next_deadline(&self) -> Option<u64> {
		self.cameras.iter().filter_map(|camera| camera.run.as_ref().and_then(|run| run.stream.deadline())).min()
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	// The roles: a catalogue connection minted for `camera` alone, the inventory root, and the minting root
	// PermissionManager reaches through the broker. Nothing else.
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (catalogue, serve_root, admin_root) = (roles[0], roles[1], roles[2]);
	let subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::Camera).unwrap_or(0) } else { 0 };
	let mut drawn = [0u8; 8];
	random_get(&mut drawn);
	let mut service = Service { incarnation: u64::from_le_bytes(drawn) | 1, catalogue, cameras: Vec::new(), grants: Vec::new(), observers: Vec::new(), admins: Vec::new(), next_key: 1, next_grant: 1, next_stream: 1 };
	send_blocking(bootstrap, b"CameraService: online", 0);

	let mut buf = alloc::vec![0u8; BUF_BYTES];
	let mut reply_buf = alloc::vec![0u8; BUF_BYTES];
	let mut subscribed = subscription != 0;
	loop {
		let mut waitset: Vec<u64> = Vec::new();
		for root in [serve_root, admin_root] {
			if root != 0 {
				waitset.push(root);
			}
		}
		if subscribed {
			waitset.push(subscription);
		}
		waitset.extend(service.admins.iter().copied());
		waitset.extend(service.observers.iter().copied());
		for camera in &service.cameras {
			waitset.extend([camera.chan, camera.events]);
		}
		for grant in &service.grants {
			waitset.extend([grant.chan, grant.owner]);
			if grant.events != 0 {
				waitset.push(grant.events);
			}
		}
		let now = clock();
		let deadlines = [service.next_deadline(), service.statuses_due().then_some(now + 1)];
		let deadline = deadlines.into_iter().flatten().min().map_or(0, |deadline| deadline.max(now + 1));
		let ready = wait_any(&waitset, deadline);
		service.tick();
		if ready >= 0 {
			serve(&mut service, waitset[ready as usize], serve_root, admin_root, subscription, &mut subscribed, &mut buf, &mut reply_buf);
		}
		service.statuses();
	}
}

#[allow(clippy::too_many_arguments)]
fn serve(service: &mut Service, handle: u64, serve_root: u64, admin_root: u64, subscription: u64, subscribed: &mut bool, buf: &mut [u8], reply_buf: &mut [u8]) {
	if let Some(key) = service.cameras.iter().find(|camera| camera.chan == handle).map(|camera| camera.key) {
		if let Err(why) = service.on_reply(key, buf) {
			service.lose(key, why);
		}
		return;
	}
	if let Some(key) = service.cameras.iter().find(|camera| camera.events == handle).map(|camera| camera.key) {
		if let Err(why) = service.on_events(key, buf) {
			service.lose(key, why);
		}
		return;
	}
	// A GRANT'S OWNER ENDED: its stream stops, whatever copies of its endpoint live on.
	if let Some(id) = service.grants.iter().find(|grant| grant.owner == handle).map(|grant| grant.id) {
		print(b"CameraService: a grant's owner ended - its grant is retired\n");
		service.retire(id, Error::Closed);
		return;
	}
	if let Some(at) = service.grants.iter().position(|grant| grant.events == handle) {
		if let PolledCaps::Closed = try_recv_caps(handle, buf) {
			close(handle);
			service.grants[at].events = 0;
		}
		return;
	}
	if let Some(at) = service.grants.iter().position(|grant| grant.chan == handle) {
		let (len, mut handles) = match try_recv_caps(handle, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				let id = service.grants[at].id;
				service.retire(id, Error::Closed);
				return;
			}
		};
		let understood = len >= 6 && service.capture(at, &buf[..len], &mut handles, reply_buf);
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		if !understood && let Some(id) = service.grants.get(at).map(|grant| grant.id) {
			service.retire(id, Error::Invalid);
		}
		return;
	}
	if let Some(at) = service.observers.iter().position(|&observer| observer == handle) {
		let (len, mut handles) = match try_recv_caps(handle, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				close(service.observers.remove(at));
				return;
			}
		};
		// AN INVENTORY CONNECTION MINTS ANOTHER, as the root does: a resolver keeps the connection the broker
		// minted for it and mints its own from that one.
		if len >= 2 && u16::from_le_bytes([buf[0], buf[1]]) == CONNECT_OP {
			for &leftover in handles.as_slice() {
				close(leftover);
			}
			match channel().filter(|_| service.clients() < cs::MAX_CLIENTS) {
				Some((mine, theirs)) => {
					service.observers.push(mine);
					send_blocking(handle, &[], theirs);
				}
				None => {
					send_blocking(handle, &[], 0);
				}
			}
			return;
		}
		let mut reply_handles = Handles::new();
		let written = camera::dispatch(&mut InventoryView { service }, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		match written {
			Some(written) => {
				if !send_caps_blocking(handle, &reply_buf[..written], reply_handles.as_slice()) {
					for &leftover in reply_handles.as_slice() {
						close(leftover);
					}
				}
			}
			// A REQUEST INVENTORY CANNOT CARRY - a start, a buffer - closes the connection.
			None => {
				close(service.observers.remove(at));
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
			} else if let Some(key) = service.cameras.iter().find(|camera| same(&camera.info, &info)).map(|camera| camera.key) {
				service.lose(key, b"its publication was withdrawn");
			}
		}
		return;
	}
	let is_serve = handle == serve_root;
	let is_admin_root = handle == admin_root;
	if !is_serve && !is_admin_root && !service.admins.contains(&handle) {
		return;
	}
	let (len, mut handles) = match try_recv_caps(handle, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return,
		PolledCaps::Closed => {
			if !is_serve && !is_admin_root {
				service.admins.retain(|&admin| admin != handle);
				close(handle);
			}
			return;
		}
	};
	if len >= 2 {
		let op = u16::from_le_bytes([buf[0], buf[1]]);
		if op == HEARTBEAT_OP {
			send_blocking(handle, b"PONG", 0);
			return;
		}
		// FROM THE ROOT OR FROM A CONNECTION IT MINTED: a resolver keeps the connection the broker minted
		// for it and mints its own from that one, as every root's connections allow - refusing it here
		// refused every grant the resolver was asked for.
		if op == CONNECT_OP {
			let full = if is_serve { service.clients() >= cs::MAX_CLIENTS } else { service.admins.len() >= MAX_ADMINS };
			match channel().filter(|_| !full) {
				Some((mine, theirs)) => {
					if is_serve {
						service.observers.push(mine);
					} else {
						service.admins.push(mine);
					}
					send_blocking(handle, &[], theirs);
				}
				None => {
					send_blocking(handle, &[], 0);
				}
			}
			return;
		}
	}
	if is_serve || is_admin_root {
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		return;
	}
	let mut reply_handles = Handles::new();
	let written = camera_admin::dispatch(&mut AdminView { service }, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	match written {
		Some(written) => {
			if !send_caps_blocking(handle, &reply_buf[..written], reply_handles.as_slice()) {
				for &leftover in reply_handles.as_slice() {
					close(leftover);
				}
			}
		}
		None => {
			service.admins.retain(|&admin| admin != handle);
			close(handle);
		}
	}
}
