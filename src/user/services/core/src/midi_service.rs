// MidiService - MIDI 1.0 endpoints: received as ordered bounded chunks with host receipt time and cable, and
// sent as the same chunks the other way, for components PermissionManager granted one endpoint.
//
// WHAT IT HOLDS AND WHAT IT DOES NOT. It consumes every `midi` publication through a catalogue connection
// minted for that kind alone; a provider - the USB MIDI class module, the in-guest fixture - delivers raw
// USB-MIDI 1.0 packet batches, and this service decodes them with the driver library's staged decoder and
// queues the result for exactly one reader per endpoint. It serves inventory on SERVE, which lists endpoints
// and opens nothing, and on ADMIN the minting endpoint PermissionManager alone reaches. It is not a broker, a
// router or a sequencer: there is no fan-out and no scheduling.
//
// SENDING IS CHECKED WHOLE AND PACED BY THE DEVICE. A sender's batch is encoded with the driver library's
// encoder - every chunk a packet, or nothing sent at all - and handed to the provider as one batch, and the
// sender's answer waits for the provider's, which waits for the device to take the packets. So one batch is in
// flight per sender, and a slow device slows its sender and nothing else.
//
// UMP BESIDE MIDI 1.0. A `ump` endpoint's batches are Universal MIDI Packet words, cut into messages and their SysEx7
// and SysEx8 counted with the driver library's UMP layer; a receiver reads what it asked for - a MIDI 1.0 endpoint as
// MIDI 1.0 in UMP, a UMP endpoint as MIDI 1.0 chunks through the translation and the same staged decoder - and a
// sender sends either on either the same way. A batch that grows past what one provider send carries goes as several,
// one after another, and the sender is answered when the last is taken.
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
use drivers::{ump, usb_midi};
use ipc_client::ChannelTransport;
use proto::system::{Error, EventEnd, EventEndReason, EventHeader, EventSource, MidiAbort, MidiAbortReason, MidiBatch, MidiBlock, MidiBlockDirection, MidiChunk, MidiChunkKind, MidiDeviceBlockDirection, MidiDeviceDirection, MidiDeviceEvent, MidiDeviceProtocol, MidiDirection, MidiEndpoint, MidiEndpointId, MidiEvent, MidiFault, MidiFaultCode, MidiGrant, MidiItem, MidiProtocol, MidiReceiverStatus, MidiUmp, ProviderInfo, ProviderKind, midi, midi_admin, midi_device, midi_input, midi_output, provider_catalogue};
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
// The most one provider `send` carries.
const SEND_BYTES: usize = usb_midi::MAX_PACKETS * usb_midi::PACKET;

// One endpoint of a provider: as it listed itself, and - for a UMP one - its function blocks.
struct Endpoint {
	index: u32,
	name: String,
	cables: u8,
	direction: MidiDeviceDirection,
	ump: bool,
	blocks: Vec<MidiBlock>,
}

struct Provider {
	key: u32,
	info: ProviderInfo,
	chan: u64,
	events: u64,
	endpoints: Vec<Endpoint>,
	// Calls not yet answered, by correlation.
	sent: Vec<(u32, Pending)>,
	next_corr: u32,
}

// What a provider call was for, so its answer reaches the right place.
#[derive(Clone, Copy)]
enum Pending {
	// A `start` - its endpoint and receiver generation, so a refusal can end the receiver it refused - or a `stop`.
	Control(Option<(u32, u64)>),
	// A `send`, for the sender of this generation, whose own answer waits for it.
	Send(u64),
}

// One sender: the grant's connection, the owner it lives as long as, and the messages it has open.
struct Sender {
	chan: u64,
	owner: u64,
	provider: u32,
	endpoint: u32,
	generation: u64,
	active: bool,
	encoder: usb_midi::Encoder,
	// The sender's `send` waiting for the provider: its correlation - and the batches still to go after the one in
	// flight, when what it sent grew past one provider send.
	waiting: Option<u32>,
	outbox: Vec<Vec<u8>>,
	// Whether its endpoint carries UMP; and what keeps a UMP sender's SysEx counted, and MIDI 1.0 in UMP cut back at
	// threes for a MIDI 1.0 endpoint.
	ump: bool,
	cables: u8,
	tracker: ump::Tracker,
	to_packets: ump::ToPackets,
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
	// Whether its endpoint carries UMP, and the protocol it reads - the endpoint's own unless it asked for the other;
	// the UMP layer's SysEx counting, and the translation down to packets for a MIDI 1.0 reading of UMP.
	ump: bool,
	reads: MidiProtocol,
	cables: u8,
	tracker: ump::Tracker,
	to_packets: ump::ToPackets,
}

struct Service {
	incarnation: u64,
	catalogue: u64,
	providers: Vec<Provider>,
	receivers: Vec<Receiver>,
	senders: Vec<Sender>,
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

// A UMP message in the public vocabulary.
fn ump_item(message: &ump::Ump) -> MidiItem {
	MidiItem::Ump(MidiUmp { group: message.group(), words: message.words().to_vec() })
}

// A decoded MIDI 1.0 chunk as MIDI 1.0 in UMP - through the packet that carries it, so the translation is the one the
// library states. A raw byte belongs to no message and has no UMP form.
fn ump_of_chunk(chunk: &usb_midi::Chunk) -> Option<MidiItem> {
	let bytes = chunk.bytes();
	let packet = match chunk.kind {
		usb_midi::Kind::Raw => return None,
		usb_midi::Kind::Short => ump::packet_of_message(chunk.cable, bytes),
		_ => {
			let cin = if chunk.end { *[0x5u8, 0x6, 0x7].get(bytes.len().checked_sub(1)?)? } else { 0x4 };
			let mut packet = [chunk.cable << 4 | cin, 0, 0, 0];
			packet[1..1 + bytes.len()].copy_from_slice(bytes);
			packet
		}
	};
	ump::ump_of_packet(packet).map(|message| ump_item(&message))
}

fn abort_reason(reason: ump::Abort) -> MidiAbortReason {
	match reason {
		ump::Abort::Cap => MidiAbortReason::Cap,
		ump::Abort::Malformed => MidiAbortReason::Malformed,
		ump::Abort::Restarted => MidiAbortReason::Restarted,
	}
}

fn word_fault(fault: ump::Fault) -> MidiItem {
	MidiItem::Fault(MidiFault { cable: None, code: if fault == ump::Fault::Alignment { MidiFaultCode::Alignment } else { MidiFaultCode::Truncated } })
}

impl Receiver {
	// ONE BATCH, as this receiver reads it: the endpoint's packets decoded in its own protocol or translated into the
	// other. A UMP batch's words are cut into messages and their SysEx counted; read as MIDI 1.0 they go down to event
	// packets and through the same decoder a MIDI 1.0 endpoint's do.
	fn decode(&mut self, bytes: &[u8], now: u64) -> Vec<MidiItem> {
		let mut outputs = Vec::new();
		if !self.ump {
			self.decoder.decode(bytes, now, &mut outputs);
			return outputs
				.into_iter()
				.filter_map(|output| match (output, self.reads) {
					(usb_midi::Output::Chunk(chunk), MidiProtocol::Ump) => ump_of_chunk(&chunk),
					(output, _) => Some(item(output)),
				})
				.collect();
		}
		let (messages, fault) = match ump::words_of_bytes(bytes) {
			Ok(words) => ump::split(&words),
			Err(fault) => (Vec::new(), Some(fault)),
		};
		let mut items = Vec::new();
		if self.reads == MidiProtocol::Ump {
			for message in messages {
				for tracked in self.tracker.take(message) {
					match tracked {
						ump::Tracked::Message(message, _) => items.push(ump_item(&message)),
						ump::Tracked::Aborted { group, number, reason } => items.push(MidiItem::Aborted(MidiAbort { cable: group, message: number, reason: abort_reason(reason) })),
						// A PART WITH NO MESSAGE TO BELONG TO is discarded, as a MIDI 1.0 decoder discards one.
						ump::Tracked::Stray(_) => {}
					}
				}
			}
		} else {
			let packets: Vec<u8> = messages.iter().flat_map(|message| self.to_packets.take(message)).flatten().collect();
			self.decoder.decode(&packets, now, &mut outputs);
			items.extend(outputs.into_iter().map(item));
		}
		items.extend(fault.map(word_fault));
		items
	}

	// Queue what was decoded, stamped with the receipt time it came with. An event that does not fit ends the stream
	// - and integrity records are events like any other, so an abort that cannot be queued ends it too.
	fn enqueue(&mut self, items: Vec<MidiItem>, received_ns: u64) -> Result<(), be::End> {
		for event in items {
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

	fn endpoint_info(&self, provider: &Provider, endpoint: &Endpoint) -> MidiEndpoint {
		let held = |held_provider: u32, held_endpoint: u32| held_provider == provider.key && held_endpoint == endpoint.index;
		let receiving = self.receivers.iter().any(|receiver| receiver.active && held(receiver.provider, receiver.endpoint)) || self.senders.iter().any(|sender| sender.active && held(sender.provider, sender.endpoint));
		let direction = if endpoint.direction == MidiDeviceDirection::Transmit { MidiDirection::Transmit } else { MidiDirection::Receive };
		let protocol = if endpoint.ump { MidiProtocol::Ump } else { MidiProtocol::Midi1 };
		MidiEndpoint { id: self.endpoint_id(provider, endpoint.index), name: endpoint.name.clone(), protocol, direction, cables: endpoint.cables, receiving, blocks: endpoint.blocks.clone() }
	}

	fn clients(&self) -> usize {
		self.observers.len() + self.receivers.len() + self.senders.len()
	}

	fn send(&mut self, provider: u32, start: Option<(u32, u64)>, encode: impl FnOnce(&mut midi_device::Client<&mut Capture>)) -> bool {
		self.call(provider, Pending::Control(start), encode)
	}

	fn call(&mut self, provider: u32, pending: Pending, encode: impl FnOnce(&mut midi_device::Client<&mut Capture>)) -> bool {
		let Some(at) = self.providers.iter().position(|held| held.key == provider) else { return false };
		let corr = self.providers[at].next_corr;
		self.providers[at].next_corr = self.providers[at].next_corr.wrapping_add(1).max(1);
		let sent = captured(encode, corr).is_some_and(|bytes| try_send(self.providers[at].chan, &bytes, 0));
		if sent {
			if self.providers[at].sent.len() >= 32 {
				self.providers[at].sent.remove(0);
			}
			self.providers[at].sent.push((corr, pending));
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
			self.send(provider, None, |client| {
				let _ = client.stop(&endpoint, &generation);
			});
		}
		self.answer_read(at);
	}

	// END A SENDER: its endpoint's slot freed, and a send it is waiting on answered - a batch the provider takes
	// after this is the provider's to finish, and nobody waits for it.
	fn end_sender(&mut self, at: usize, result: Result<(), Error>) {
		let sender = &mut self.senders[at];
		sender.active = false;
		if let Some(corr) = sender.waiting.take() {
			reply(sender.chan, corr, result, |_, _| Some(()));
		}
	}

	fn retire_sender(&mut self, at: usize) {
		self.end_sender(at, Err(Error::Closed));
		let sender = self.senders.remove(at);
		close(sender.chan);
		close(sender.owner);
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
		// A UMP ENDPOINT'S FUNCTION BLOCKS, asked for once as the device is admitted: what it says about itself.
		let mut held = Vec::new();
		for endpoint in endpoints {
			let ump = endpoint.protocol == MidiDeviceProtocol::Ump;
			let blocks = if ump {
				match client.blocks(&endpoint.index) {
					Some(Ok(blocks)) => blocks
						.into_iter()
						.map(|block| MidiBlock {
							id: block.id,
							name: block.name,
							first_group: block.first_group,
							groups: block.groups,
							direction: match block.direction {
								MidiDeviceBlockDirection::Receives => MidiBlockDirection::Receives,
								MidiDeviceBlockDirection::Sends => MidiBlockDirection::Sends,
								MidiDeviceBlockDirection::Both => MidiBlockDirection::Both,
							},
							protocol: block.protocol,
						})
						.collect(),
					_ => {
						print(b"MidiService: a MIDI device did not list a UMP endpoint's function blocks\n");
						close(chan);
						return;
					}
				}
			} else {
				Vec::new()
			};
			held.push(Endpoint { index: endpoint.index, name: endpoint.name, cables: endpoint.cables, direction: endpoint.direction, ump, blocks });
		}
		let events = client.events().unwrap_or(0);
		if events == 0 {
			close(chan);
			return;
		}
		let key = self.next_key;
		self.next_key = self.next_key.wrapping_add(1).max(1);
		self.providers.push(Provider { key, info, chan, events, endpoints: held, sent: Vec::new(), next_corr: 1 });
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
		// A SENDER ON IT ENDS TOO: what it waits on will never be taken, and a replacement needs a fresh grant.
		for at in 0..self.senders.len() {
			if self.senders[at].provider == key && self.senders[at].active {
				self.end_sender(at, Err(Error::Io));
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
			let Some(place) = self.providers[at].sent.iter().position(|(sent, _)| *sent == corr) else { continue };
			let (_, pending) = self.providers[at].sent.remove(place);
			let tag = reader.tag();
			let start = match pending {
				Pending::Control(start) => start,
				// A SEND'S ANSWER IS ITS SENDER'S: the provider's error as it gave it, or `io` if it gave none.
				Pending::Send(generation) => {
					let result = match tag {
						Some(true) => Ok(()),
						Some(false) => Err(Error::read(&mut reader).unwrap_or(Error::Io)),
						None => Err(Error::Io),
					};
					let Some(sender) = self.senders.iter().position(|sender| sender.active && sender.generation == generation) else { continue };
					// THE NEXT BATCH OF THE SAME SEND, if the provider took this one and there is one.
					if result.is_ok() && !self.senders[sender].outbox.is_empty() {
						let batch = self.senders[sender].outbox.remove(0);
						let endpoint = self.senders[sender].endpoint;
						if self.call(key, Pending::Send(generation), |client| {
							let _ = client.send(&endpoint, &batch);
						}) {
							continue;
						}
					}
					self.senders[sender].outbox.clear();
					if let Some(corr) = self.senders[sender].waiting.take() {
						reply(self.senders[sender].chan, corr, result, |_, _| Some(()));
					}
					continue;
				}
			};
			if tag != Some(false) {
				continue;
			}
			print(b"MidiService: a MIDI device refused to start or stop an endpoint\n");
			// A START THE PROVIDER REFUSED ENDS THAT RECEIVER: it will never deliver, and left active it would
			// hold its endpoint's one slot while its reader waited on nothing. Ended as removed - its source
			// failed - and with no stop sent for a start that never took.
			if let Some((endpoint, generation)) = start
				&& let Some(at) = self.receivers.iter().position(|receiver| receiver.active && receiver.provider == key && receiver.endpoint == endpoint && receiver.generation == generation)
			{
				self.receivers[at].active = false;
				self.end(at, be::Reason::Removed);
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
					let items = self.receivers[at].decode(&batch.packets, clock());
					if self.receivers[at].enqueue(items, batch.received_ns).is_err() {
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
				if self.receivers[at].enqueue(outputs.into_iter().map(item).collect(), clock_ns()).is_err() {
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
		let Some((cables, direction, ump)) = service.providers[at].endpoints.iter().find(|held| held.index == endpoint).map(|held| (held.cables, held.direction, held.ump)) else { return refuse(Error::NotFound) };
		// A RECEIVER ON A TRANSMIT ENDPOINT would wait for ever on a device that sends nothing there.
		if direction != MidiDeviceDirection::Receive {
			return refuse(Error::Invalid);
		}
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
		if !service.send(key, Some((endpoint, generation)), |client| {
			let _ = client.start(&endpoint, &generation);
		}) {
			close(mine);
			close(theirs);
			return refuse(Error::Io);
		}
		let reads = if ump { MidiProtocol::Ump } else { MidiProtocol::Midi1 };
		service.receivers.push(Receiver { chan: mine, owner, provider: key, endpoint, generation, source: source.clone(), active: true, queue: be::Queue::new(), decoder: usb_midi::Decoder::new(cables), reading: None, ump, reads, cables, tracker: ump::Tracker::new(), to_packets: ump::ToPackets::new() });
		Ok(MidiGrant { connection: theirs, source })
	}

	// THE SAME RESOLUTION FOR A SENDER, onto a TRANSMIT endpoint: one per endpoint, and nothing is sent until the
	// sender sends - there is nothing to start.
	fn mint_output(&mut self, alias: String, endpoint: u32, owner: u64) -> Result<MidiGrant, Error> {
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
		let Some((cables, direction, ump)) = service.providers[at].endpoints.iter().find(|held| held.index == endpoint).map(|held| (held.cables, held.direction, held.ump)) else { return refuse(Error::NotFound) };
		if direction != MidiDeviceDirection::Transmit {
			return refuse(Error::Invalid);
		}
		let key = service.providers[at].key;
		if service.senders.iter().any(|sender| sender.active && sender.provider == key && sender.endpoint == endpoint) {
			print(b"MidiService: a second sender on an endpoint was refused as busy\n");
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
		service.senders.push(Sender { chan: mine, owner, provider: key, endpoint, generation, active: true, encoder: usb_midi::Encoder::new(cables), waiting: None, outbox: Vec::new(), ump, cables, tracker: ump::Tracker::new(), to_packets: ump::ToPackets::new() });
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
		for at in 0..service.senders.len() {
			if service.senders[at].active && service.senders[at].provider == key && service.senders[at].endpoint == endpoint.endpoint {
				service.end_sender(at, Err(Error::Closed));
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
	// EITHER PROTOCOL ON EITHER ENDPOINT, the other one through the translation - so the protocol is never the reason.
	fn open(&mut self, endpoint: MidiEndpointId, direction: MidiDirection, _protocol: MidiProtocol) -> Result<(), Error> {
		let service = self.service;
		let Some(held) = service.providers.iter().find_map(|provider| provider.endpoints.iter().find(|held| service.endpoint_id(provider, held.index) == endpoint)) else {
			return Err(Error::NotFound);
		};
		// A DIRECTION THE ENDPOINT DOES NOT HAVE is not a question of permission.
		let transmit = held.direction == MidiDeviceDirection::Transmit;
		if transmit != (direction == MidiDirection::Transmit) {
			return Err(Error::Invalid);
		}
		// RECEIVING AND SENDING EACH NEED A GRANT: inventory can name an endpoint and never open one.
		Err(Error::Denied)
	}
}

// A receiver's connection. `read` may wait, so it is answered by hand.
enum Asked {
	Read(u16, u32),
	Stop,
	Protocol(MidiProtocol),
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
		let endpoint = provider.endpoints.iter().find(|endpoint| endpoint.index == receiver.endpoint).ok_or(Error::Closed)?;
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
	fn protocol(&mut self, protocol: MidiProtocol) -> Result<(), Error> {
		self.asked = Some(Asked::Protocol(protocol));
		Err(Error::Again)
	}
}

// A sender's connection. `send` waits on the provider, so it is answered by hand.
enum Told {
	Send(Vec<MidiChunk>),
	SendUmp(Vec<MidiUmp>),
	Stop,
}

struct OutputView<'a> {
	service: &'a Service,
	sender: usize,
	told: Option<Told>,
}

impl midi_output::Service for OutputView<'_> {
	fn endpoint(&mut self) -> Result<MidiEndpoint, Error> {
		let sender = &self.service.senders[self.sender];
		let provider = self.service.providers.iter().find(|provider| provider.key == sender.provider).ok_or(Error::Closed)?;
		let endpoint = provider.endpoints.iter().find(|endpoint| endpoint.index == sender.endpoint).ok_or(Error::Closed)?;
		Ok(self.service.endpoint_info(provider, endpoint))
	}
	fn send(&mut self, chunks: Vec<MidiChunk>) -> Result<(), Error> {
		self.told = Some(Told::Send(chunks));
		Err(Error::Again)
	}
	fn send_ump(&mut self, messages: Vec<MidiUmp>) -> Result<(), Error> {
		self.told = Some(Told::SendUmp(messages));
		Err(Error::Again)
	}
	fn stop(&mut self) -> Result<(), Error> {
		self.told = Some(Told::Stop);
		Err(Error::Again)
	}
}

// A chunk on the wire's vocabulary, as the encoder takes it; `None` for one that cannot even be read as one.
fn outgoing(chunk: &MidiChunk) -> Option<usb_midi::Chunk> {
	if chunk.bytes.len() > 3 {
		return None;
	}
	let kind = match chunk.kind {
		MidiChunkKind::Short => usb_midi::Kind::Short,
		MidiChunkKind::SysexStart => usb_midi::Kind::SysexStart,
		MidiChunkKind::SysexContinue => usb_midi::Kind::SysexContinue,
		MidiChunkKind::SysexEnd => usb_midi::Kind::SysexEnd,
		MidiChunkKind::Raw => usb_midi::Kind::Raw,
	};
	let mut out = usb_midi::Chunk { cable: chunk.cable, kind, bytes: [0; 3], len: chunk.bytes.len() as u8, message: chunk.sysex_message, start: chunk.start, end: chunk.end };
	out.bytes[..chunk.bytes.len()].copy_from_slice(&chunk.bytes);
	Some(out)
}

impl Service {
	fn output(&mut self, at: usize, request: &[u8], handles: &mut Handles, reply_buf: &mut [u8]) -> bool {
		let chan = self.senders[at].chan;
		let mut view = OutputView { service: self, sender: at, told: None };
		let mut reply_handles = Handles::new();
		let written = midi_output::dispatch(&mut view, request, handles, reply_buf, &mut reply_handles);
		let told = view.told.take();
		let Some(written) = written else { return false };
		let Some(told) = told else {
			if !send_caps_blocking(chan, &reply_buf[..written], reply_handles.as_slice()) {
				for &leftover in reply_handles.as_slice() {
					close(leftover);
				}
			}
			return true;
		};
		let corr = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
		match told {
			Told::Send(_) | Told::SendUmp(_) => {
				let sender = &mut self.senders[at];
				if !sender.active {
					reply::<()>(chan, corr, Err(Error::Closed), |_, _| Some(()));
					return true;
				}
				if sender.waiting.is_some() {
					reply::<()>(chan, corr, Err(Error::Again), |_, _| Some(()));
					return true;
				}
				let encoded = match told {
					Told::Send(chunks) => sender.chunks(&chunks),
					Told::SendUmp(messages) => sender.messages(&messages),
					Told::Stop => unreachable!(),
				};
				let Some(mut batches) = encoded else {
					reply::<()>(chan, corr, Err(Error::Invalid), |_, _| Some(()));
					return true;
				};
				let first = batches.remove(0);
				sender.outbox = batches;
				let (provider, endpoint, generation) = (sender.provider, sender.endpoint, sender.generation);
				if self.call(provider, Pending::Send(generation), |client| {
					let _ = client.send(&endpoint, &first);
				}) {
					self.senders[at].waiting = Some(corr);
				} else {
					self.senders[at].outbox.clear();
					reply::<()>(chan, corr, Err(Error::Io), |_, _| Some(()));
				}
			}
			Told::Stop => {
				self.end_sender(at, Err(Error::Closed));
				reply(chan, corr, Ok(()), |_, _| Some(()));
			}
		}
		true
	}

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
			// THE OTHER PROTOCOL FROM HERE ON: a SysEx open in the old reading is discarded with it, as a reset is.
			Asked::Protocol(protocol) => {
				let receiver = &mut self.receivers[at];
				receiver.reads = protocol;
				receiver.decoder = usb_midi::Decoder::new(receiver.cables);
				receiver.tracker = ump::Tracker::new();
				receiver.to_packets = ump::ToPackets::new();
				reply(chan, corr, Ok(()), |_, _| Some(()));
			}
		}
		true
	}
}

impl Sender {
	// A MIDI 1.0 SENDER'S CHUNKS, checked whole and encoded - for a UMP endpoint, each packet then as MIDI 1.0 in UMP -
	// and cut into provider sends. `None` refuses the batch and moves nothing.
	fn chunks(&mut self, chunks: &[MidiChunk]) -> Option<Vec<Vec<u8>>> {
		let outgoing = chunks.iter().map(outgoing).collect::<Option<Vec<_>>>()?;
		let mut packets = Vec::new();
		if outgoing.is_empty() || self.encoder.encode_all(&outgoing, &mut packets).is_err() {
			return None;
		}
		if !self.ump {
			return Some(batches(packets.chunks_exact(usb_midi::PACKET).map(<[u8]>::to_vec)));
		}
		Some(batches(packets.chunks_exact(usb_midi::PACKET).filter_map(|packet| ump::ump_of_packet([packet[0], packet[1], packet[2], packet[3]])).map(|message| message.bytes())))
	}

	// A UMP SENDER'S MESSAGES, checked whole: each as many words as its type says, its group the one its words carry and
	// one the endpoint has, and its SysEx parts fitting the messages they continue. For a MIDI 1.0 endpoint each goes
	// down by the translation, and one with no MIDI 1.0 form refuses the batch. `None` moves nothing.
	fn messages(&mut self, messages: &[MidiUmp]) -> Option<Vec<Vec<u8>>> {
		if messages.is_empty() {
			return None;
		}
		let mut tracker = self.tracker.clone();
		let mut to_packets = self.to_packets.clone();
		let mut pieces = Vec::new();
		for message in messages {
			let parsed = ump::Ump::new(&message.words)?;
			if parsed.group() != message.group || parsed.group().is_some_and(|group| group >= self.cables) {
				return None;
			}
			if self.ump {
				if !tracker.take(parsed).iter().all(|tracked| matches!(tracked, ump::Tracked::Message(..))) {
					return None;
				}
				pieces.push(parsed.bytes());
			} else {
				let packets = to_packets.take(&parsed);
				// A SYSEX7 PART MAY GIVE NO PACKET YET - its bytes wait for threes - but anything else that gives none has
				// no MIDI 1.0 form.
				if packets.is_empty() && parsed.message_type() != ump::MT_DATA64 {
					return None;
				}
				pieces.extend(packets.into_iter().map(|packet| packet.to_vec()));
			}
		}
		self.tracker = tracker;
		self.to_packets = to_packets;
		Some(batches(pieces.into_iter()))
	}
}

// Messages or packets gathered into provider sends of at most `SEND_BYTES`, none cut across two.
fn batches(pieces: impl Iterator<Item = Vec<u8>>) -> Vec<Vec<u8>> {
	let mut out: Vec<Vec<u8>> = Vec::new();
	for piece in pieces {
		match out.last_mut() {
			Some(last) if last.len() + piece.len() <= SEND_BYTES => last.extend(piece),
			_ => out.push(piece),
		}
	}
	out
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (catalogue, mut serve_root, mut admin_root) = (roles[0], roles[1], roles[2]);
	let subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::Midi).unwrap_or(0) } else { 0 };
	let mut drawn = [0u8; 8];
	random_get(&mut drawn);
	let mut service = Service { incarnation: u64::from_le_bytes(drawn) | 1, catalogue, providers: Vec::new(), receivers: Vec::new(), senders: Vec::new(), observers: Vec::new(), admins: Vec::new(), next_key: 1, next_generation: 1 };
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
		for sender in &service.senders {
			waitset.extend([sender.chan, sender.owner]);
		}
		let now = clock();
		let deadline = service.next_deadline().map_or(0, |deadline| deadline.max(now + 1));
		let ready = wait_any(&waitset, deadline);
		service.tick();
		if ready >= 0 {
			serve(&mut service, waitset[ready as usize], &mut serve_root, &mut admin_root, subscription, &mut subscribed, &mut buf, &mut reply_buf);
		}
	}
}

#[allow(clippy::too_many_arguments)]
fn serve(service: &mut Service, handle: u64, serve_root: &mut u64, admin_root: &mut u64, subscription: u64, subscribed: &mut bool, buf: &mut [u8], reply_buf: &mut [u8]) {
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
	// A SENDER'S OWNER ENDED, or its connection spoke: the same two cases as a receiver's.
	if let Some(at) = service.senders.iter().position(|sender| sender.owner == handle) {
		print(b"MidiService: a sender's owner ended - its sender is retired\n");
		service.retire_sender(at);
		return;
	}
	if let Some(at) = service.senders.iter().position(|sender| sender.chan == handle) {
		let (len, mut handles) = match try_recv_caps(handle, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				service.retire_sender(at);
				return;
			}
		};
		let understood = len >= 6 && service.output(at, &buf[..len], &mut handles, reply_buf);
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		if !understood && let Some(at) = service.senders.iter().position(|sender| sender.chan == handle) {
			service.retire_sender(at);
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
	let is_serve = handle == *serve_root;
	let is_admin_root = handle == *admin_root;
	if !is_serve && !is_admin_root && !service.admins.contains(&handle) {
		return;
	}
	let (len, mut handles) = match try_recv_caps(handle, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return,
		PolledCaps::Closed => {
			// A CLOSED ROOT IS NOT WAITED ON AGAIN. It stays readable for ever, so waiting on it returned at
			// once and this loop spun a core; the connections it already minted are still served.
			if is_serve {
				*serve_root = 0;
			} else if is_admin_root {
				*admin_root = 0;
			} else {
				service.admins.retain(|&admin| admin != handle);
			}
			close(handle);
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
