// MidiService - MIDI 1.0 receive endpoints, as ordered bounded chunks with host receipt time and cable, for
// components PermissionManager granted one endpoint.
//
// WHAT IT HOLDS AND WHAT IT DOES NOT. It consumes every `midi` publication through a catalogue connection
// minted for that kind alone; a provider - the USB MIDI class module, the in-guest fixture - delivers raw
// USB-MIDI 1.0 packet batches, and this service decodes them with the driver library's staged decoder and
// queues the result for exactly one reader per endpoint. It serves inventory on SERVE, which lists endpoints
// and opens nothing, and on ADMIN the minting endpoint PermissionManager alone reaches. It is not a broker, a
// router or a sequencer: there is no fan-out, no output and no scheduling.
//
// BOUNDED EVERYWHERE, AND LOSS IS NEVER SILENT. Each receiver has one `service_logic::bounded_event` queue:
// 256 events and 32 kB, integrity records included. A batch that does not fit ENDS the receiver with
// `overflow` and discards what it held; a provider's report of lost input ends it with
// `source-discontinuity`; removal, revocation and the owner ending end it too - and the end is what the next
// read returns. Reads pull; nothing is pushed into a client that is not reading, and a slow reader holds up
// nothing but itself.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use drivers::usb_midi;
use ipc_client::ChannelTransport;
use proto::system::{Error, EventEnd, EventEndReason, EventHeader, EventSource, MidiAbort, MidiAbortReason, MidiBatch, MidiChunk, MidiChunkKind, MidiDeviceEvent, MidiDirection, MidiEndpoint, MidiEndpointId, MidiEvent, MidiFault, MidiFaultCode, MidiGrant, MidiItem, MidiProtocol, MidiReceiverStatus, ProviderInfo, ProviderKind, midi, midi_admin, midi_device, midi_input, provider_catalogue};
use rt::*;
use service_logic::bounded_event as be;
use wire::{Handles, Reader, Sink, Transport, TransportError};

include!(concat!(env!("OUT_DIR"), "/roles_midi_service.rs"));

// THE CONFIGURED MIDI DEVICES: an alias a policy row may name, and the provider metadata it binds. A shipping
// build configures none, so no receiver can be minted until somebody configures one.
#[cfg(feature = "development")]
const ALIASES: &[(&str, &[u8])] = &[("fixture", b"org.libersystem.midi-fixture")];
#[cfg(not(feature = "development"))]
const ALIASES: &[(&str, &[u8])] = &[];

const MAX_PROVIDERS: usize = 4;
const MAX_ENDPOINTS: usize = 8;
const MAX_CLIENTS: usize = 32;
const MAX_ADMINS: usize = 4;
const MAX_READ: usize = 64;
const VERSION: u32 = 1;
const OPEN_TICKS: u64 = 100;
const BUF_BYTES: usize = 8192;

struct Provider {
	key: u32,
	info: ProviderInfo,
	chan: u64,
	events: u64,
	// Index, name and cable count of each receive endpoint.
	endpoints: Vec<(u32, String, u8)>,
	sent: Vec<u32>,
	next_corr: u32,
}

// One receiver: the grant's connection, the owner it lives as long as, and its bounded stream.
struct Receiver {
	chan: u64,
	owner: u64,
	provider: u32,
	endpoint: u32,
	generation: u64,
	source: EventSource,
	// Still receiving: it holds its endpoint's one slot, and its provider is delivering.
	active: bool,
	// Each event with the host time its batch was received at, which it keeps however late it is read.
	queue: be::Queue<(MidiItem, u64)>,
	decoder: usb_midi::Decoder,
	// A read waiting for its first event: its correlation, how many it takes, and until when.
	reading: Option<(u32, usize, u64)>,
}

struct Service {
	incarnation: u64,
	catalogue: u64,
	providers: Vec<Provider>,
	receivers: Vec<Receiver>,
	observers: Vec<u64>,
	admins: Vec<u64>,
	next_key: u32,
	next_generation: u64,
}

// A generated client's request, captured instead of sent: the encoding is the generator's, the sending is
// this service's own - non-blocking, under a correlation of its choosing.
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

fn captured(encode: impl FnOnce(&mut midi_device::Client<&mut Capture>), corr: u32) -> Option<Vec<u8>> {
	let mut capture = Capture { bytes: Vec::new() };
	encode(&mut midi_device::Client::new(&mut capture));
	if capture.bytes.len() < 6 {
		return None;
	}
	capture.bytes[2..6].copy_from_slice(&corr.to_le_bytes());
	Some(capture.bytes)
}

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

fn same(a: &ProviderInfo, b: &ProviderInfo) -> bool {
	a.slot == b.slot && a.provider_generation == b.provider_generation && a.binding_generation == b.binding_generation
}

fn end_reason(reason: be::Reason) -> EventEndReason {
	match reason {
		be::Reason::Overflow => EventEndReason::Overflow,
		be::Reason::SourceDiscontinuity => EventEndReason::SourceDiscontinuity,
		be::Reason::Removed => EventEndReason::Removed,
		be::Reason::Revoked => EventEndReason::Revoked,
		be::Reason::Exhausted => EventEndReason::Exhausted,
		be::Reason::Stopped => EventEndReason::Stopped,
		be::Reason::Shutdown => EventEndReason::Shutdown,
	}
}

fn end_of(end: be::End) -> EventEnd {
	EventEnd { reason: end_reason(end.reason), next_sequence: end.next_sequence }
}

// What the decoder said, in the public vocabulary.
fn item(output: usb_midi::Output) -> MidiItem {
	match output {
		usb_midi::Output::Chunk(chunk) => MidiItem::Chunk(MidiChunk {
			cable: chunk.cable,
			kind: match chunk.kind {
				usb_midi::Kind::Short => MidiChunkKind::Short,
				usb_midi::Kind::SysexStart => MidiChunkKind::SysexStart,
				usb_midi::Kind::SysexContinue => MidiChunkKind::SysexContinue,
				usb_midi::Kind::SysexEnd => MidiChunkKind::SysexEnd,
				usb_midi::Kind::Raw => MidiChunkKind::Raw,
			},
			bytes: chunk.bytes().to_vec(),
			sysex_message: chunk.message,
			start: chunk.start,
			end: chunk.end,
		}),
		usb_midi::Output::Abort { cable, message, reason } => MidiItem::Aborted(MidiAbort {
			cable,
			message,
			reason: match reason {
				usb_midi::AbortReason::Cap => MidiAbortReason::Cap,
				usb_midi::AbortReason::Malformed => MidiAbortReason::Malformed,
				usb_midi::AbortReason::Interrupted => MidiAbortReason::Interrupted,
				usb_midi::AbortReason::Inactivity => MidiAbortReason::Inactivity,
				usb_midi::AbortReason::Restarted => MidiAbortReason::Restarted,
				usb_midi::AbortReason::Reset => MidiAbortReason::Reset,
			},
		}),
		usb_midi::Output::Fault { cable, code } => MidiItem::Fault(MidiFault {
			cable,
			code: match code {
				usb_midi::FaultCode::Alignment => MidiFaultCode::Alignment,
				usb_midi::FaultCode::Cable => MidiFaultCode::Cable,
				usb_midi::FaultCode::Reserved => MidiFaultCode::Reserved,
				usb_midi::FaultCode::Status => MidiFaultCode::Status,
				usb_midi::FaultCode::Padding => MidiFaultCode::Padding,
			},
		}),
	}
}

impl Receiver {
	// Queue what the decoder produced, stamped with the receipt time it came with. An event that does not fit
	// ends the stream - and integrity records are events like any other, so an abort that cannot be queued
	// ends it too.
	fn enqueue(&mut self, outputs: Vec<usb_midi::Output>, received_ns: u64) -> Result<(), be::End> {
		for output in outputs {
			let event = item(output);
			let header = EventHeader { sequence: self.queue.next_sequence(), received_ns };
			let bytes = MidiEvent { header, item: event.clone() }.encode_vec().map_or(usize::MAX, |encoded| encoded.len());
			self.queue.push((event, received_ns), bytes)?;
		}
		Ok(())
	}

	fn batch(&mut self, max: usize) -> MidiBatch {
		let pulled = self.queue.pull(max.min(MAX_READ));
		let events = pulled.events.into_iter().map(|(sequence, (item, received_ns))| MidiEvent { header: EventHeader { sequence, received_ns }, item }).collect();
		MidiBatch { source: self.source.clone(), events, end: pulled.end.map(end_of) }
	}
}

impl Service {
	fn endpoint_id(&self, provider: &Provider, endpoint: u32) -> MidiEndpointId {
		MidiEndpointId { slot: provider.info.slot, generation: provider.info.provider_generation, binding_generation: provider.info.binding_generation, endpoint, incarnation: self.incarnation }
	}

	fn endpoint_info(&self, provider: &Provider, endpoint: &(u32, String, u8)) -> MidiEndpoint {
		let receiving = self.receivers.iter().any(|receiver| receiver.active && receiver.provider == provider.key && receiver.endpoint == endpoint.0);
		MidiEndpoint { id: self.endpoint_id(provider, endpoint.0), name: endpoint.1.clone(), protocol: MidiProtocol::Midi1, direction: MidiDirection::Receive, cables: endpoint.2, receiving }
	}

	fn clients(&self) -> usize {
		self.observers.len() + self.receivers.len()
	}

	fn send(&mut self, provider: u32, encode: impl FnOnce(&mut midi_device::Client<&mut Capture>)) -> bool {
		let Some(at) = self.providers.iter().position(|held| held.key == provider) else { return false };
		let corr = self.providers[at].next_corr;
		self.providers[at].next_corr = self.providers[at].next_corr.wrapping_add(1).max(1);
		let sent = captured(encode, corr).is_some_and(|bytes| try_send(self.providers[at].chan, &bytes, 0));
		if sent {
			if self.providers[at].sent.len() >= 32 {
				self.providers[at].sent.remove(0);
			}
			self.providers[at].sent.push(corr);
		}
		sent
	}

	// END A RECEIVER: everything queued and every partial SysEx discarded, the end kept for its next read, its
	// endpoint's slot freed and its provider told to stop delivering.
	fn end(&mut self, at: usize, reason: be::Reason) {
		let receiver = &mut self.receivers[at];
		receiver.queue.terminate(reason);
		let mut discarded = Vec::new();
		receiver.decoder.reset(usb_midi::AbortReason::Reset, &mut discarded);
		if receiver.active {
			receiver.active = false;
			let (provider, endpoint, generation) = (receiver.provider, receiver.endpoint, receiver.generation);
			self.send(provider, |client| {
				let _ = client.stop(&endpoint, &generation);
			});
		}
		self.answer_read(at);
	}

	// A waiting read is answered once there is something to answer with.
	fn answer_read(&mut self, at: usize) {
		let receiver = &mut self.receivers[at];
		let Some((corr, max, _)) = receiver.reading else { return };
		if receiver.queue.is_empty() && receiver.queue.end().is_none() {
			return;
		}
		receiver.reading = None;
		let batch = receiver.batch(max);
		reply(receiver.chan, corr, Ok(batch), |batch, w| batch.write(w));
	}

	// A receiver is gone for good: its connection and its owner's observer let go.
	fn retire(&mut self, at: usize, reason: be::Reason) {
		self.end(at, reason);
		let receiver = self.receivers.remove(at);
		close(receiver.chan);
		close(receiver.owner);
	}

	fn adopt(&mut self, info: ProviderInfo) {
		if self.providers.iter().any(|provider| same(&provider.info, &info)) {
			return;
		}
		if self.providers.len() >= MAX_PROVIDERS {
			print(b"MidiService: a MIDI device was refused: this service holds four (resource exhausted)\n");
			return;
		}
		let Some(Ok(chan)) = provider_catalogue::Client::new(ChannelTransport { chan: self.catalogue }).open(&info) else {
			print(b"MidiService: a published MIDI device could not be opened\n");
			return;
		};
		let mut client = midi_device::Client::with_deadline(ChannelTransport { chan }, clock() + OPEN_TICKS);
		let bounded = matches!(client.open(&VERSION), Some(Ok(opened)) if opened.bounds.version == VERSION && usize::from(opened.bounds.max_packets) <= usb_midi::MAX_PACKETS);
		let endpoints = match client.endpoints() {
			Some(Ok(endpoints)) if bounded && endpoints.len() <= MAX_ENDPOINTS && endpoints.iter().all(|endpoint| (1..=usb_midi::MAX_CABLES).contains(&endpoint.cables)) => endpoints,
			_ => {
				print(b"MidiService: a MIDI device did not open within its bounds\n");
				close(chan);
				return;
			}
		};
		let events = client.events().unwrap_or(0);
		if events == 0 {
			close(chan);
			return;
		}
		let key = self.next_key;
		self.next_key = self.next_key.wrapping_add(1).max(1);
		self.providers.push(Provider { key, info, chan, events, endpoints: endpoints.into_iter().map(|endpoint| (endpoint.index, endpoint.name, endpoint.cables)).collect(), sent: Vec::new(), next_corr: 1 });
		print(b"MidiService: a MIDI device was admitted\n");
	}

	// A device is gone: every receiver on it ends as removed. Their connections stay, so the end is readable;
	// a replacement is another device and needs a fresh grant.
	fn lose(&mut self, key: u32, why: &[u8]) {
		let Some(at) = self.providers.iter().position(|provider| provider.key == key) else { return };
		let provider = self.providers.remove(at);
		close(provider.events);
		close(provider.chan);
		for at in 0..self.receivers.len() {
			if self.receivers[at].provider == key {
				self.receivers[at].active = false;
				self.end(at, be::Reason::Removed);
			}
		}
		print(b"MidiService: a MIDI device is gone: ");
		print(why);
		print(b"\n");
	}

	fn on_reply(&mut self, key: u32, buf: &mut [u8]) -> Result<(), &'static [u8]> {
		loop {
			let Some(at) = self.providers.iter().position(|provider| provider.key == key) else { return Ok(()) };
			let (len, handles) = match try_recv_caps(self.providers[at].chan, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return Ok(()),
				PolledCaps::Closed => return Err(b"its connection closed"),
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut reader = Reader::new(&buf[..len]);
			let Some(corr) = reader.u32() else { return Err(b"a reply carried no correlation") };
			let Some(place) = self.providers[at].sent.iter().position(|sent| *sent == corr) else { continue };
			self.providers[at].sent.remove(place);
			// A start the provider refused ends that receiver: it will never deliver.
			if reader.tag() == Some(false) {
				print(b"MidiService: a MIDI device refused to start or stop an endpoint\n");
			}
		}
	}

	fn on_events(&mut self, key: u32, buf: &mut [u8]) -> Result<(), &'static [u8]> {
		loop {
			let Some(at) = self.providers.iter().position(|provider| provider.key == key) else { return Ok(()) };
			let (len, handles) = match try_recv_caps(self.providers[at].events, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return Ok(()),
				PolledCaps::Closed => return Err(b"its event stream closed"),
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut frame_handles = Handles::new();
			let Some(event) = midi_device::events_read(&buf[..len], &mut frame_handles) else { return Err(b"an event did not decode") };
			match event {
				MidiDeviceEvent::Batch(batch) => {
					// A LATE BATCH FOR A RETIRED RECEIVER is ignored: nothing it carries reaches a new one.
					let Some(at) = self.receivers.iter().position(|receiver| receiver.active && receiver.provider == key && receiver.endpoint == batch.endpoint && receiver.generation == batch.receiver_generation) else { continue };
					if batch.received_ns > clock_ns() {
						return Err(b"a batch arrived from the future");
					}
					let mut outputs = Vec::new();
					self.receivers[at].decoder.decode(&batch.packets, clock(), &mut outputs);
					if self.receivers[at].enqueue(outputs, batch.received_ns).is_err() {
						print(b"MidiService: a receiver overflowed and was stopped\n");
						self.end(at, be::Reason::Overflow);
						continue;
					}
					self.answer_read(at);
				}
				MidiDeviceEvent::Lost(lost) => {
					let Some(at) = self.receivers.iter().position(|receiver| receiver.active && receiver.provider == key && receiver.endpoint == lost.endpoint && receiver.generation == lost.receiver_generation) else { continue };
					print(b"MidiService: a MIDI device lost input - the receiver ends with a discontinuity\n");
					self.end(at, be::Reason::SourceDiscontinuity);
				}
			}
		}
	}

	// Inactivity deadlines of open SysEx messages, and waiting reads that ran out.
	fn tick(&mut self) {
		let now = clock();
		for at in 0..self.receivers.len() {
			if !self.receivers[at].active {
				continue;
			}
			let mut outputs = Vec::new();
			self.receivers[at].decoder.tick(now, &mut outputs);
			if !outputs.is_empty() {
				if self.receivers[at].enqueue(outputs, clock_ns()).is_err() {
					self.end(at, be::Reason::Overflow);
					continue;
				}
				self.answer_read(at);
			}
			if let Some((corr, max, deadline)) = self.receivers[at].reading
				&& now >= deadline
			{
				let receiver = &mut self.receivers[at];
				receiver.reading = None;
				let batch = receiver.batch(max);
				reply(receiver.chan, corr, Ok(batch), |batch, w| batch.write(w));
			}
		}
	}

	fn next_deadline(&self) -> Option<u64> {
		self.receivers.iter().flat_map(|receiver| [receiver.decoder.next_deadline(), receiver.reading.map(|(_, _, deadline)| deadline)]).flatten().min()
	}
}

// ------------------------------------------------------------------ the views the generated code calls

// PermissionManager's minting endpoint.
struct AdminView<'a> {
	service: &'a mut Service,
}

impl midi_admin::Service for AdminView<'_> {
	// THE ALIAS RESOLVES TO EXACTLY ONE CURRENT PUBLICATION, AND THE INDEX TO ONE OF ITS RECEIVE ENDPOINTS, OR
	// THE MINT FAILS. One receiver per endpoint: a second is busy, whoever asks.
	fn mint(&mut self, alias: String, endpoint: u32, owner: u64) -> Result<MidiGrant, Error> {
		let refuse = |error: Error| {
			close(owner);
			Err(error)
		};
		let Some(&(_, bound)) = ALIASES.iter().find(|(name, _)| *name == alias) else { return refuse(Error::NotFound) };
		let service = &mut *self.service;
		let matching: Vec<usize> = service.providers.iter().enumerate().filter(|(_, provider)| provider.info.name.as_bytes() == bound).map(|(at, _)| at).collect();
		let at = match matching.as_slice() {
			[at] => *at,
			[] => return refuse(Error::NotFound),
			_ => return refuse(Error::Invalid),
		};
		let Some(&(_, _, cables)) = service.providers[at].endpoints.iter().find(|(index, _, _)| *index == endpoint) else { return refuse(Error::NotFound) };
		let key = service.providers[at].key;
		if service.receivers.iter().any(|receiver| receiver.active && receiver.provider == key && receiver.endpoint == endpoint) {
			print(b"MidiService: a second receiver on an endpoint was refused as busy - there is no fan-out\n");
			return refuse(Error::Again);
		}
		if service.clients() >= MAX_CLIENTS {
			return refuse(Error::Exhausted);
		}
		let Some((mine, theirs)) = channel() else { return refuse(Error::Exhausted) };
		let generation = service.next_generation;
		service.next_generation += 1;
		let info = &service.providers[at].info;
		let source = EventSource { incarnation: service.incarnation, slot: info.slot, generation: info.provider_generation, binding_generation: info.binding_generation, endpoint, receiver_generation: generation };
		if !service.send(key, |client| {
			let _ = client.start(&endpoint, &generation);
		}) {
			close(mine);
			close(theirs);
			return refuse(Error::Io);
		}
		service.receivers.push(Receiver { chan: mine, owner, provider: key, endpoint, generation, source: source.clone(), active: true, queue: be::Queue::new(), decoder: usb_midi::Decoder::new(cables), reading: None });
		Ok(MidiGrant { connection: theirs, source })
	}

	fn revoke(&mut self, endpoint: MidiEndpointId) -> Result<u32, Error> {
		let service = &mut *self.service;
		let Some(key) = service.providers.iter().find(|provider| service.endpoint_id(provider, endpoint.endpoint) == endpoint).map(|provider| provider.key) else { return Ok(0) };
		let mut revoked = 0;
		for at in 0..service.receivers.len() {
			if service.receivers[at].active && service.receivers[at].provider == key && service.receivers[at].endpoint == endpoint.endpoint {
				service.end(at, be::Reason::Revoked);
				revoked += 1;
			}
		}
		Ok(revoked)
	}
}

// Inventory: endpoints, and an `open` that says no precisely.
struct InventoryView<'a> {
	service: &'a Service,
}

impl midi::Service for InventoryView<'_> {
	fn endpoints(&mut self) -> Result<Vec<MidiEndpoint>, Error> {
		let service = self.service;
		Ok(service.providers.iter().flat_map(|provider| provider.endpoints.iter().map(move |endpoint| service.endpoint_info(provider, endpoint))).collect())
	}
	fn open(&mut self, endpoint: MidiEndpointId, direction: MidiDirection, protocol: MidiProtocol) -> Result<(), Error> {
		// OUTPUT AND UMP DO NOT EXIST HERE YET, and saying so is the answer - nothing is silently accepted.
		if direction == MidiDirection::Transmit || protocol == MidiProtocol::Ump {
			return Err(Error::Unsupported);
		}
		let known = self.service.providers.iter().any(|provider| provider.endpoints.iter().any(|held| self.service.endpoint_id(provider, held.0) == endpoint));
		if !known {
			return Err(Error::NotFound);
		}
		// RECEIVING NEEDS A GRANT: inventory can name an endpoint and never open one.
		Err(Error::Denied)
	}
}

// A receiver's connection. `read` may wait, so it is answered by hand.
enum Asked {
	Read(u16, u32),
	Stop,
}

struct InputView<'a> {
	service: &'a Service,
	receiver: usize,
	asked: Option<Asked>,
}

impl midi_input::Service for InputView<'_> {
	fn endpoint(&mut self) -> Result<MidiEndpoint, Error> {
		let receiver = &self.service.receivers[self.receiver];
		let provider = self.service.providers.iter().find(|provider| provider.key == receiver.provider).ok_or(Error::Closed)?;
		let endpoint = provider.endpoints.iter().find(|endpoint| endpoint.0 == receiver.endpoint).ok_or(Error::Closed)?;
		Ok(self.service.endpoint_info(provider, endpoint))
	}
	fn read(&mut self, max: u16, wait_ms: u32) -> Result<MidiBatch, Error> {
		self.asked = Some(Asked::Read(max, wait_ms));
		Err(Error::Again)
	}
	fn status(&mut self) -> Result<MidiReceiverStatus, Error> {
		let receiver = &self.service.receivers[self.receiver];
		Ok(MidiReceiverStatus { source: receiver.source.clone(), queued: receiver.queue.len() as u16, queued_bytes: receiver.queue.bytes() as u32, end: receiver.queue.end().map(end_of) })
	}
	fn stop(&mut self) -> Result<(), Error> {
		self.asked = Some(Asked::Stop);
		Err(Error::Again)
	}
}

impl Service {
	fn input(&mut self, at: usize, request: &[u8], handles: &mut Handles, reply_buf: &mut [u8]) -> bool {
		let chan = self.receivers[at].chan;
		let mut view = InputView { service: self, receiver: at, asked: None };
		let mut reply_handles = Handles::new();
		let written = midi_input::dispatch(&mut view, request, handles, reply_buf, &mut reply_handles);
		let asked = view.asked.take();
		let Some(written) = written else { return false };
		let Some(asked) = asked else {
			if !send_caps_blocking(chan, &reply_buf[..written], reply_handles.as_slice()) {
				for &leftover in reply_handles.as_slice() {
					close(leftover);
				}
			}
			return true;
		};
		let corr = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
		match asked {
			Asked::Read(max, wait_ms) => {
				let receiver = &mut self.receivers[at];
				if receiver.reading.is_some() {
					reply::<()>(chan, corr, Err(Error::Again), |_, _| Some(()));
					return true;
				}
				let max = usize::from(max).clamp(1, MAX_READ);
				receiver.reading = Some((corr, max, clock() + u64::from(wait_ms).div_ceil(10)));
				self.answer_read(at);
			}
			Asked::Stop => {
				self.end(at, be::Reason::Stopped);
				reply(chan, corr, Ok(()), |_, _| Some(()));
			}
		}
		true
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (catalogue, serve_root, admin_root) = (roles[0], roles[1], roles[2]);
	let subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::Midi).unwrap_or(0) } else { 0 };
	let mut drawn = [0u8; 8];
	random_get(&mut drawn);
	let mut service = Service { incarnation: u64::from_le_bytes(drawn) | 1, catalogue, providers: Vec::new(), receivers: Vec::new(), observers: Vec::new(), admins: Vec::new(), next_key: 1, next_generation: 1 };
	send_blocking(bootstrap, b"MidiService: online", 0);

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
		for provider in &service.providers {
			waitset.extend([provider.chan, provider.events]);
		}
		for receiver in &service.receivers {
			waitset.extend([receiver.chan, receiver.owner]);
		}
		let now = clock();
		let deadline = service.next_deadline().map_or(0, |deadline| deadline.max(now + 1));
		let ready = wait_any(&waitset, deadline);
		service.tick();
		if ready >= 0 {
			serve(&mut service, waitset[ready as usize], serve_root, admin_root, subscription, &mut subscribed, &mut buf, &mut reply_buf);
		}
	}
}

#[allow(clippy::too_many_arguments)]
fn serve(service: &mut Service, handle: u64, serve_root: u64, admin_root: u64, subscription: u64, subscribed: &mut bool, buf: &mut [u8], reply_buf: &mut [u8]) {
	if let Some(key) = service.providers.iter().find(|provider| provider.chan == handle).map(|provider| provider.key) {
		if let Err(why) = service.on_reply(key, buf) {
			service.lose(key, why);
		}
		return;
	}
	if let Some(key) = service.providers.iter().find(|provider| provider.events == handle).map(|provider| provider.key) {
		if let Err(why) = service.on_events(key, buf) {
			service.lose(key, why);
		}
		return;
	}
	// A RECEIVER'S OWNER ENDED: it ends with it, whatever copies of its endpoint live on.
	if let Some(at) = service.receivers.iter().position(|receiver| receiver.owner == handle) {
		print(b"MidiService: a receiver's owner ended - its receiver is retired\n");
		service.retire(at, be::Reason::Revoked);
		return;
	}
	if let Some(at) = service.receivers.iter().position(|receiver| receiver.chan == handle) {
		let (len, mut handles) = match try_recv_caps(handle, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				service.retire(at, be::Reason::Stopped);
				return;
			}
		};
		let understood = len >= 6 && service.input(at, &buf[..len], &mut handles, reply_buf);
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		if !understood && let Some(at) = service.receivers.iter().position(|receiver| receiver.chan == handle) {
			service.retire(at, be::Reason::Stopped);
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
			match channel().filter(|_| service.clients() < MAX_CLIENTS) {
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
		let written = midi::dispatch(&mut InventoryView { service }, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
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
			} else if let Some(key) = service.providers.iter().find(|provider| same(&provider.info, &info)).map(|provider| provider.key) {
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
			let full = if is_serve { service.clients() >= MAX_CLIENTS } else { service.admins.len() >= MAX_ADMINS };
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
	let written = midi_admin::dispatch(&mut AdminView { service }, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
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
