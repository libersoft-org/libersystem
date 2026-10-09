// AudioService: the DEVICE MODEL. Every catalogue `audio` provider and every Bluetooth endpoint is a device with an
// identity, a label, its directions in its own PCM format, its latency and its level; the routing rule keeps one
// default output, one default input and one default voice device (`service_logic::audio_routing`); streams follow the
// default or the device they named, and NEVER END because a device left - they move, and with no device at all they keep
// accepting and the gap is silence, counted; voice sessions run duplex at a voice rate with the call relay; and a
// phone's stream is played to the default output through a jitter buffer, never offered as an input.
//
// ONE OPERATION AT A TIME PER DEVICE. A provider is synchronous - it blocks on its device for the period it plays or
// fills - so each device has exactly one request outstanding, and `Pending` says which. A `write` with no room and a
// `read` with no period are DEFERRED: the raw request is kept and re-dispatched when its answer can be given, so
// nothing about one device or one client waits on another.
//
// THE DEVICE-SIDE CONTRACT is `driver_protocol::audio`: a provider is asked its format first (`CMD_FORMAT`; one that
// refuses is the fixed 48 kHz stereo every provider spoke before), and a Bluetooth endpoint states its format when it
// is offered, its period ten milliseconds of it. Either is driven the same way after that, its period acknowledgment
// its clock.
//
// BLUETOOTH IS REACHED BY NAME THROUGH THE BROKER (`bluetooth-audio`), never as a role: stopping the radio's stack stops
// no audio, and nothing here waits for a radio to start. The resolve is asynchronous and retried, as InputService's
// Bluetooth slot is; when the stack's instance ends its endpoints leave, and the routing rule applies.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use driver_protocol::audio::{CMD_CAPTURE, CMD_CAPTURE_STOP, CMD_FORMAT, CMD_STATS, CMD_VOLUME, DeviceFormat, MAX_PERIOD_BYTES, PcmFormat, PlaybackStats};
use ipc_client::ChannelTransport;
use pcm::Format;
use pcm::encode::{Remix, Resample};
use proto::codec::Buffer;
use proto::system::audio::{self, Service as AudioService};
use proto::system::audio_admin::{self, Service as AdminService};
use proto::system::audio_control::{self, Service as ControlService};
use proto::system::audio_stats::{self, Service as StatsService};
use proto::system::pcm_capture::{self, Service as PcmCaptureService};
use proto::system::pcm_stream::{self, Service as PcmService};
use proto::system::voice_session::{self, Service as VoiceService};
use proto::system::{AudioCounters, AudioDevice, AudioDirection, AudioFormat, AudioResources, AudioStreamInfo, AudioTransport, CallCommand, CallState, Error};
use proto::system::{AudioEndpoint, AudioEndpointKind, AudioEvent, BtCallCommand, BtCallState, bluetooth_audio};
use proto::system::{ProviderInfo, ProviderKind, provider_catalogue};
use rt::*;
use service_logic::audio_routing::{self, Direction, Jitter, Routing, TimerPacer};
use services::capability_names::CAP_BT_AUDIO;

// THE IDLE LATENCY A PLAYING DEVICE CAN TAKE: a tenth of a 48 kHz period, so a core waking from the deepest state it
// may enter still answers the period's interrupt with the period to spare.
const LATENCY_BOUND_US: u64 = 1_000;
// What one stream may hold queued, and the bounds on what this service holds at all.
const MAX_QUEUED_FRAMES: usize = 4_096;
const MAX_STREAMS: usize = 16;
// RECORDERS: each device's captured period is handed to every recorder on it, so several may record one microphone.
const MAX_CAPTURES: usize = 4;
const MAX_VOICES: usize = 2;
const MAX_TONES: usize = 8;
const AMP: i16 = 6_000;
const REQUEST_MAX: usize = 128;
// A capture `read` answers with a whole converted period inline: at most a device period, so the largest the wire has.
const REPLY_MAX: usize = MAX_PERIOD_BYTES as usize + 128;
// How many refused periods in a row mean a device is not playing anything.
const REFUSAL_LIMIT: u32 = 8;
// A device's counters are read again after this many periods played, and once more when it stops.
const STATS_EVERY: u32 = 16;
// THE PHONE'S STREAM: a jitter buffer bounded at 200 ms.
const ROUTE_JITTER_MS: u32 = 200;
// A Bluetooth endpoint's period: ten milliseconds of its own format.
const ENDPOINT_PERIOD_MS: u32 = 10;
// How long before the broker is asked again for Bluetooth's audio authority.
const BLUETOOTH_RETRY_TICKS: u64 = 200;
// A STREAM WITH NO DEVICE is drained by the host's timer, and this service wakes every tick while one holds frames.
const IDLE_TICKS: u64 = 1;
// A hundred clock ticks are a second: the timer a stream with no device plays on counts in them, the clock this
// service also waits on.
const NS_PER_TICK: u64 = 10_000_000;

struct PendingWrite {
	request: Vec<u8>,
	caps: proto::codec::Handles,
}

// WHERE A STREAM OR A RECORDER'S REQUESTS COME FROM AND ITS REPLIES GO: its own channel, a voice session's - whose one
// channel carries both halves - or nowhere, for the phone's stream this service plays itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Front {
	Own,
	Voice(u64),
	Route(u32),
}

struct Stream {
	chan: u64,
	front: Front,
	format: Format,
	samples: Vec<i16>,
	read_frame: usize,
	phase: u32,
	closing: bool,
	pending: Option<PendingWrite>,
	// The device it named, and the one it plays on now.
	named: Option<u32>,
	device: Option<u32>,
	// The frames played into no device, and the clock that played them while it had none.
	silent_frames: u64,
	idle: Option<TimerPacer>,
}

impl Stream {
	fn new(chan: u64, front: Front, format: Format, named: Option<u32>) -> Stream {
		Stream { chan, front, format, samples: Vec::new(), read_frame: 0, phase: 0, closing: false, pending: None, named, device: None, silent_frames: 0, idle: None }
	}

	fn queued_frames(&self) -> usize {
		self.samples.len() / self.format.channels() as usize - self.read_frame
	}

	fn capacity(&self) -> usize {
		MAX_QUEUED_FRAMES.saturating_sub(self.queued_frames())
	}

	// The next frame as stereo, the stream stepped at the rate of the device it plays on.
	fn next_frame(&mut self, device_rate: u32) -> Option<(i16, i16)> {
		if self.read_frame >= self.samples.len() / self.format.channels() as usize {
			return None;
		}
		let frame = self.format.stereo_frame(&self.samples, self.read_frame)?;
		self.format.advance_to(device_rate, &mut self.phase, &mut self.read_frame);
		Some(frame)
	}

	// Drop `frames` source frames: played into no device.
	fn skip(&mut self, frames: usize) -> usize {
		let skipped = frames.min(self.queued_frames());
		self.read_frame += skipped;
		self.compact();
		skipped
	}

	fn compact(&mut self) {
		let channels = self.format.channels() as usize;
		let consumed: usize = self.read_frame * channels;
		if consumed != 0 && (self.read_frame >= self.samples.len() / channels || consumed >= 2_048) {
			self.samples.drain(..consumed);
			self.read_frame = 0;
		}
	}

	fn append(&mut self, samples: &[i16]) {
		self.samples.extend_from_slice(samples);
	}

	fn write_buffer(&mut self, data: Buffer) -> Result<u32, Error> {
		let handle: u64 = data.handle;
		let result: Result<u32, Error> = (|| {
			if self.closing || handle == 0 {
				return Err(Error::Invalid);
			}
			let requested: usize = self.format.frames_in(data.len).ok_or(Error::Invalid)?;
			let info: ObjectInfo = object_info(handle).ok_or(Error::Invalid)?;
			if data.len > info.size {
				return Err(Error::Invalid);
			}
			let accepted: usize = requested.min(self.capacity());
			if accepted == 0 {
				return Err(Error::Again);
			}
			let mapped: u64 = unsafe { map_object(handle) }.ok_or(Error::Invalid)?;
			let byte_count: usize = accepted * self.format.frame_bytes() as usize;
			let bytes: &[u8] = unsafe { core::slice::from_raw_parts(mapped as *const u8, byte_count) };
			self.format.append_i16_le(bytes, accepted, &mut self.samples).ok_or(Error::Invalid)?;
			unmap_object(handle);
			Ok(accepted as u32)
		})();
		close(handle);
		result
	}

	fn release(&mut self) {
		if let Some(pending) = self.pending.take() {
			for &handle in pending.caps.as_slice() {
				close(handle);
			}
		}
		if self.chan != 0 && self.front == Front::Own {
			close(self.chan);
		}
		self.chan = 0;
	}
}

struct Tone {
	remaining: u32,
	frame: u32,
	half_period: u32,
}

impl Tone {
	fn next_frame(&mut self) -> Option<(i16, i16)> {
		if self.remaining == 0 {
			return None;
		}
		let sample: i16 = if (self.frame / self.half_period) % 2 == 0 { AMP } else { -AMP };
		self.frame += 1;
		self.remaining -= 1;
		Some((sample, sample))
	}
}

// A RECORDER: the conversion from its device's input format to what it asked for, and the two halves of the deferral -
// the request waiting for a period and the period waiting for a request.
struct Capture {
	chan: u64,
	front: Front,
	rate: u32,
	channels: u8,
	named: Option<u32>,
	device: Option<u32>,
	// Built for the device's input format whenever the recorder moves.
	conversion: Option<(Remix, Resample)>,
	pending: Option<Vec<u8>>,
	ready: Option<Vec<u8>>,
	closing: bool,
}

// A VOICE SESSION: its channel, the call it declares, and the stream its holder reads the voice device's call
// commands from.
struct Voice {
	chan: u64,
	call: CallState,
	commands: u64,
	closing: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Origin {
	// A catalogue publication, identified as the catalogue identifies it.
	Provider { slot: u32, generation: u32 },
	// An endpoint on the `bluetooth-audio` connection.
	Bluetooth { endpoint: u32 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pending {
	None,
	Format,
	Volume,
	Period,
	Stop,
	Capture,
	CaptureStop,
	Stats,
}

struct Device {
	id: u32,
	origin: Origin,
	label: String,
	// Zero for a voice endpoint while no session holds its link up.
	chan: u64,
	format: DeviceFormat,
	// The provider has answered what it is; until then it is in no default.
	known: bool,
	voice: bool,
	route: bool,
	volume: u8,
	volume_due: bool,
	// None retains the original combined level for software providers until the operator opts into a separate mic.
	microphone_volume: Option<u8>,
	hardware_microphone_volume: bool,
	pending: Pending,
	running: bool,
	capture_running: bool,
	// The device refused to capture: its recorders are answered not-found.
	input_refused: bool,
	refusals: u32,
	counters: Option<PlaybackStats>,
	stats_due: bool,
	periods_since_stats: u32,
	period: Vec<u8>,
	jitter: Option<Jitter>,
}

impl Device {
	fn new(id: u32, origin: Origin, label: String, chan: u64, format: DeviceFormat) -> Device {
		Device { id, origin, label, chan, format, known: false, voice: false, route: false, volume: format.volume, volume_due: false, microphone_volume: None, hardware_microphone_volume: false, pending: Pending::None, running: false, capture_running: false, input_refused: false, refusals: 0, counters: None, stats_due: false, periods_since_stats: 0, period: Vec::new(), jitter: None }
	}

	fn microphone_level(&self) -> u8 {
		self.microphone_volume.unwrap_or(if self.format.hardware_volume { 100 } else { self.volume })
	}

	fn output(&self) -> Option<PcmFormat> {
		self.format.output
	}

	fn input(&self) -> Option<PcmFormat> {
		self.format.input
	}

	fn directions(&self) -> Vec<Direction> {
		let mut out = Vec::with_capacity(2);
		if self.voice {
			out.push(Direction::Voice);
			return out;
		}
		if self.route {
			return out;
		}
		if self.output().is_some() {
			out.push(Direction::Output);
		}
		if self.input().is_some() {
			out.push(Direction::Input);
		}
		out
	}

	fn period_frames(&self) -> usize {
		let channels = self.output().map_or(2, |format| u32::from(format.channels));
		(self.format.period_bytes / (channels * 2)) as usize
	}
}

// What a connection may ask for. `Full` is the service channel ServiceManager holds; the others are what
// `audio-admin` mints for a launcher, one authority each.
//
// CAPTURE IS NOT A SUBSET OF PLAYBACK, AND A VOICE SESSION IS BOTH: a program granted `audio-voice` may open a duplex
// session and nothing else - neither a stream nor a recorder of its own.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
	Full,
	StreamOnly,
	CaptureOnly,
	VoiceOnly,
}

struct Client {
	chan: u64,
	scope: Scope,
}

// BLUETOOTH'S AUDIO: the broker channel a resolve travels on, the connection it answered, and the endpoints stream.
struct Bluetooth {
	broker: u64,
	client: u64,
	events: u64,
	retry_at: u64,
	resolving: bool,
	refused: bool,
	// The call state last told to the stack.
	relayed: BtCallState,
}

struct Audio {
	devices: Vec<Device>,
	next_id: u32,
	routing: Routing,
	streams: Vec<Stream>,
	captures: Vec<Capture>,
	voices: Vec<Voice>,
	tones: Vec<Tone>,
	latency_privilege: u64,
	latency: u64,
	moves: u64,
	silent_frames: u64,
	bt: Bluetooth,
}

fn transport_of(origin: Origin) -> AudioTransport {
	match origin {
		Origin::Provider { .. } => AudioTransport::Provider,
		Origin::Bluetooth { .. } => AudioTransport::Bluetooth,
	}
}

fn format_of(format: Option<PcmFormat>) -> Option<AudioFormat> {
	format.map(|format| AudioFormat { rate: format.rate, channels: format.channels })
}

// A device's input format as the conversion takes it.
fn conversion_for(input: PcmFormat, rate: u32, channels: u8) -> Option<(Remix, Resample)> {
	Some((Remix::new(input.channels, channels)?, Resample::new(input.rate, rate, channels)?))
}

impl Audio {
	fn new(broker: u64) -> Audio {
		Audio { devices: Vec::new(), next_id: 1, routing: Routing::new(), streams: Vec::new(), captures: Vec::new(), voices: Vec::new(), tones: Vec::new(), latency_privilege: 0, latency: 0, moves: 0, silent_frames: 0, bt: Bluetooth { broker, client: 0, events: 0, retry_at: clock(), resolving: false, refused: false, relayed: BtCallState::None } }
	}

	fn index_of(&self, id: u32) -> Option<usize> {
		self.devices.iter().position(|device| device.id == id)
	}

	fn device(&self, id: u32) -> Option<&Device> {
		self.devices.iter().find(|device| device.id == id)
	}

	// ------------------------------------------------------------------ arrivals and departures

	// A CATALOGUE PROVIDER WAS PUBLISHED and this service connected to it: a device, asked its format before anything
	// else. It joins the defaults when it answers.
	fn provider_arrived(&mut self, info: &ProviderInfo, chan: u64) {
		if self.devices.len() >= audio_routing::MAX_DEVICES {
			print(b"AudioService: the inventory is full; an audio provider was left unopened\n");
			close(chan);
			return;
		}
		let id = self.next_id;
		self.next_id = self.next_id.wrapping_add(1).max(1);
		// WHAT THE PUBLISHER CALLS IT, or where it is: two cards of one kind are told apart by their address.
		let label = if info.name.is_empty() { alloc::format!("sound device at {:02x}:{:02x}.{}", info.bus, info.dev, info.func) } else { info.name.clone() };
		let mut device = Device::new(id, Origin::Provider { slot: info.slot, generation: info.provider_generation }, label, chan, DeviceFormat::LEGACY);
		device.stats_due = true;
		self.devices.push(device);
	}

	// THE PROVIDER ANSWERED WHAT IT IS - or refused, which is the fixed format every provider spoke before it was asked.
	fn format_answered(&mut self, index: usize, reply: &[u8]) {
		let device = &mut self.devices[index];
		device.pending = Pending::None;
		device.format = DeviceFormat::decode(reply).unwrap_or(DeviceFormat::LEGACY);
		device.volume = device.format.volume;
		device.known = true;
		device.period = alloc::vec![0; device.format.period_bytes as usize];
		let (id, directions) = (device.id, device.directions());
		self.routing.arrive(id, &directions);
		print(b"AudioService: a device arrived and is the default for what it serves\n");
	}

	// A PUBLICATION WAS WITHDRAWN: the device it names leaves.
	fn provider_withdrawn(&mut self, slot: u32, generation: u32) {
		if let Some(index) = self.devices.iter().position(|device| device.origin == Origin::Provider { slot, generation }) {
			self.remove_device(index);
		}
	}

	// A BLUETOOTH ENDPOINT WAS OFFERED: a device whose format is the offer's, opened at once unless it is a voice link -
	// which is opened only while a session holds it up.
	fn endpoint_arrived(&mut self, endpoint: &AudioEndpoint) {
		if self.devices.len() >= audio_routing::MAX_DEVICES || self.devices.iter().any(|device| device.origin == Origin::Bluetooth { endpoint: endpoint.id }) {
			return;
		}
		let Some(pcm) = Format::new(endpoint.rate, endpoint.channels).map(|_| PcmFormat { rate: endpoint.rate, channels: endpoint.channels }) else { return };
		let period_bytes = (endpoint.rate * ENDPOINT_PERIOD_MS / 1000) * u32::from(endpoint.channels) * 2;
		let (output, input) = match endpoint.kind {
			AudioEndpointKind::Output => (Some(pcm), None),
			AudioEndpointKind::Input | AudioEndpointKind::Route => (None, Some(pcm)),
			AudioEndpointKind::Voice => (Some(pcm), Some(pcm)),
		};
		let format = DeviceFormat { output, input, period_bytes, latency_us: endpoint.latency_us, hardware_volume: endpoint.hardware_volume, volume: endpoint.volume.min(100) };
		let id = self.next_id;
		self.next_id = self.next_id.wrapping_add(1).max(1);
		let mut device = Device::new(id, Origin::Bluetooth { endpoint: endpoint.id }, endpoint.name.clone(), 0, format);
		device.known = true;
		device.voice = endpoint.kind == AudioEndpointKind::Voice;
		device.route = endpoint.kind == AudioEndpointKind::Route;
		device.period = alloc::vec![0; period_bytes as usize];
		if !device.voice {
			device.chan = self.open_endpoint(endpoint.id);
			if device.chan == 0 {
				print(b"AudioService: a Bluetooth endpoint was offered and would not open\n");
				return;
			}
		}
		// THE PHONE'S STREAM: played to the default output through its jitter buffer, read as capture is.
		if device.route {
			device.jitter = Some(Jitter::new(pcm.rate, pcm.channels, ROUTE_JITTER_MS));
			if let Some(format) = Format::new(pcm.rate, pcm.channels) {
				self.streams.push(Stream::new(0, Front::Route(id), format, None));
			}
		}
		let directions = device.directions();
		self.devices.push(device);
		self.routing.arrive(id, &directions);
		print(b"AudioService: a Bluetooth audio endpoint arrived\n");
	}

	fn open_endpoint(&mut self, endpoint: u32) -> u64 {
		if self.bt.client == 0 {
			return 0;
		}
		match bluetooth_audio::Client::new(ChannelTransport { chan: self.bt.client }).open(&endpoint) {
			Some(Ok(chan)) => chan,
			_ => 0,
		}
	}

	fn endpoint_departed(&mut self, endpoint: u32) {
		if let Some(index) = self.devices.iter().position(|device| device.origin == Origin::Bluetooth { endpoint }) {
			self.remove_device(index);
		}
	}

	// A DEVICE LEFT: it is the default for nothing, what it displaced returns, and every stream on it moves - with no
	// device at all, it keeps accepting and plays into silence. Its recorders move too, or say there is nothing to
	// record from.
	fn remove_device(&mut self, index: usize) {
		let device = self.devices.remove(index);
		if device.chan != 0 {
			close(device.chan);
		}
		self.routing.leave(device.id);
		if device.route {
			self.streams.retain(|stream| stream.front != Front::Route(device.id));
		}
		print(b"AudioService: a device left; what played on it moves by the routing rule\n");
	}

	// A DEVICE'S CHANNEL FAILED: a provider whose driver went away, or an endpoint the stack closed.
	fn device_failed(&mut self, index: usize) {
		// A VOICE LINK'S CHANNEL closing is the link going down, not the headset leaving: it is still offered.
		if self.devices[index].voice {
			let device = &mut self.devices[index];
			close(device.chan);
			device.chan = 0;
			device.pending = Pending::None;
			device.running = false;
			device.capture_running = false;
			return;
		}
		self.remove_device(index);
	}

	// ------------------------------------------------------------------ the routing rule, applied

	// WHERE EVERYTHING PLAYS AND RECORDS NOW. Cheap, and run every turn: the rule is the routing table's, and this only
	// follows it - counting a stream that changed devices as a move and resetting what is tied to the device it left.
	fn place_all(&mut self) {
		let (voice_out, voice_in) = audio_routing::place_voice(&self.routing);
		let default_output = self.routing.default(Direction::Output);
		for stream in &mut self.streams {
			let target = match stream.front {
				Front::Own => audio_routing::place(&self.routing, stream.named, Direction::Output),
				Front::Voice(_) => voice_out,
				Front::Route(_) => default_output,
			};
			if target != stream.device {
				if stream.device.is_some() && target.is_some() {
					self.moves = self.moves.saturating_add(1);
				}
				// A STREAM THAT NAMED A DEVICE WHICH LEFT follows the default from now on.
				if stream.named.is_some() && target != stream.named {
					stream.named = None;
				}
				stream.device = target;
				stream.idle = None;
			}
		}
		for capture in &mut self.captures {
			let target = match capture.front {
				Front::Voice(_) => voice_in,
				_ => audio_routing::place(&self.routing, capture.named, Direction::Input),
			};
			if target != capture.device {
				if capture.named.is_some() && target != capture.named {
					capture.named = None;
				}
				capture.device = target;
				capture.conversion = target.and_then(|id| self.devices.iter().find(|device| device.id == id)).and_then(Device::input).and_then(|input| conversion_for(input, capture.rate, capture.channels));
				capture.ready = None;
			}
		}
	}

	// A STREAM WITH NO DEVICE AT ALL keeps playing - into silence, on the host's timer at its own rate - so its writes
	// are answered as they would be, and the frames it played into nothing are counted.
	fn pace_idle(&mut self) -> bool {
		let now = clock().saturating_mul(NS_PER_TICK);
		let mut waiting = false;
		for stream in &mut self.streams {
			if stream.device.is_some() || matches!(stream.front, Front::Route(_)) {
				continue;
			}
			if stream.queued_frames() == 0 {
				stream.idle = None;
				continue;
			}
			let pacer = stream.idle.get_or_insert_with(|| TimerPacer::new(stream.format.rate(), now));
			let due = pacer.due(now);
			pacer.take(due);
			let skipped = stream.skip(due as usize) as u64;
			stream.silent_frames = stream.silent_frames.saturating_add(skipped);
			self.silent_frames = self.silent_frames.saturating_add(skipped);
			waiting |= stream.queued_frames() != 0;
		}
		waiting
	}

	// THE PHONE'S STREAM IS TOPPED UP from its jitter buffer before a period is mixed: a period at a time while the
	// stream holds less than two of the default output's.
	fn feed_routes(&mut self) {
		let want = self.routing.default(Direction::Output).and_then(|id| self.device(id)).map_or(0, |device| device.period_frames() * 2);
		for index in 0..self.devices.len() {
			if !self.devices[index].route {
				continue;
			}
			let id = self.devices[index].id;
			let Some(stream) = self.streams.iter_mut().find(|stream| stream.front == Front::Route(id)) else { continue };
			let Some(jitter) = self.devices[index].jitter.as_mut() else { continue };
			while stream.queued_frames() < want.max(1) {
				match jitter.pop() {
					Some(period) => stream.append(&period),
					None => break,
				}
			}
		}
	}

	// THE VOICE LINK FOLLOWS THE SESSIONS: the default voice device's channel is opened while any session is open - which
	// is what brings a headset's audio link up - and closed when the last one ends.
	fn voice_links(&mut self) {
		let wanted = !self.voices.is_empty();
		let voice = self.routing.default(Direction::Voice);
		for index in 0..self.devices.len() {
			if !self.devices[index].voice {
				continue;
			}
			let up = self.devices[index].chan != 0;
			let should = wanted && Some(self.devices[index].id) == voice;
			if should && !up {
				let Origin::Bluetooth { endpoint } = self.devices[index].origin else { continue };
				let chan = self.open_endpoint(endpoint);
				self.devices[index].chan = chan;
			} else if !should && up {
				let device = &mut self.devices[index];
				close(device.chan);
				device.chan = 0;
				device.pending = Pending::None;
				device.running = false;
				device.capture_running = false;
			}
		}
	}

	// THE CALL RELAY: the call the sessions declare - the most pressing of theirs - told to the voice device's stack when
	// it changes. With no session declaring one, the stack hears there is none and refuses a headset's commands itself.
	fn relay_call(&mut self) {
		let declared = self.voices.iter().map(|voice| voice.call).max_by_key(|call| match call {
			CallState::Active => 4,
			CallState::Incoming => 3,
			CallState::Outgoing => 2,
			CallState::Held => 1,
			CallState::None => 0,
		});
		let state = match declared.unwrap_or(CallState::None) {
			CallState::None => BtCallState::None,
			CallState::Incoming => BtCallState::Incoming,
			CallState::Outgoing => BtCallState::Outgoing,
			CallState::Active => BtCallState::Active,
			CallState::Held => BtCallState::Held,
		};
		if state == self.bt.relayed || self.bt.client == 0 {
			return;
		}
		if matches!(bluetooth_audio::Client::new(ChannelTransport { chan: self.bt.client }).set_call(&state), Some(Ok(()))) {
			self.bt.relayed = state;
		}
	}

	// A HEADSET'S CALL COMMAND, to every session that declares a call.
	fn call_command(&mut self, command: BtCallCommand) {
		let command = match command {
			BtCallCommand::Answer => CallCommand::Answer,
			BtCallCommand::HangUp => CallCommand::HangUp,
			BtCallCommand::Reject => CallCommand::Reject,
			BtCallCommand::Redial => CallCommand::Redial,
		};
		for voice in &mut self.voices {
			if voice.call == CallState::None || voice.commands == 0 {
				continue;
			}
			let mut frame = [0u8; 32];
			let mut handles = wire::Handles::new();
			let Some(len) = voice_session::commands_frame(0, &command, &mut frame, &mut handles) else { continue };
			if matches!(try_send_outcome(voice.commands, &frame[..len], 0), SendOutcome::Failed) {
				close(voice.commands);
				voice.commands = 0;
			}
		}
	}

	// ------------------------------------------------------------------ the devices' one request at a time

	// THE NEXT REQUEST TO A DEVICE, if it has none outstanding: its format, its level, its counters, a captured period
	// a recorder or the phone's stream is waiting for, a period to play, or the end of what it was doing.
	//
	// CAPTURE IS ASKED FOR BEFORE PLAYBACK. A device fills a period on its own clock and a recorder that is late loses
	// audio; a playback period that is late is one the device's own queue covers.
	fn pump(&mut self, index: usize) {
		let id = self.devices[index].id;
		let device = &self.devices[index];
		if device.chan == 0 || device.pending != Pending::None {
			return;
		}
		if !device.known {
			self.send_to(index, &[CMD_FORMAT], Pending::Format);
			return;
		}
		if device.volume_due {
			self.devices[index].volume_due = false;
			let level = self.devices[index].volume;
			self.send_to(index, &[CMD_VOLUME, level], Pending::Volume);
			return;
		}
		if device.stats_due && !device.route && device.output().is_some() {
			let device = &mut self.devices[index];
			device.stats_due = false;
			device.periods_since_stats = 0;
			self.send_to(index, &[CMD_STATS], Pending::Stats);
			return;
		}
		// THE PHONE'S STREAM IS ALWAYS READ: it arrives on the phone's clock, and what the jitter buffer cannot hold is its
		// overflow to drop and count, not the phone's to wait for.
		let wants_capture = if device.route { true } else { !device.input_refused && self.captures.iter().any(|capture| capture.device == Some(id) && capture.pending.is_some() && capture.ready.is_none()) };
		if wants_capture && device.input().is_some() {
			self.devices[index].capture_running = true;
			self.send_to(index, &[CMD_CAPTURE], Pending::Capture);
			return;
		}
		let has_audio = self.has_audio(id);
		let device = &self.devices[index];
		if device.output().is_some() && !device.route && has_audio {
			self.fill_period(index);
			let period = core::mem::take(&mut self.devices[index].period);
			// A SEND THAT FAILS TAKES THE DEVICE AWAY: nothing at this index is touched after it.
			if self.send_to(index, &period, Pending::Period) {
				self.devices[index].period = period;
				self.devices[index].running = true;
			}
			return;
		}
		let device = &self.devices[index];
		if device.running {
			self.send_to(index, &[], Pending::Stop);
			return;
		}
		let recording = device.route || self.captures.iter().any(|capture| capture.device == Some(id));
		if device.capture_running && !recording {
			self.devices[index].capture_running = false;
			self.send_to(index, &[CMD_CAPTURE_STOP], Pending::CaptureStop);
		}
	}

	// One request to a device: true when it went, and false when the device's channel failed - which removes the device,
	// or for a voice link takes it down - so the caller touches nothing at `index` after a false.
	fn send_to(&mut self, index: usize, bytes: &[u8], pending: Pending) -> bool {
		if send_blocking(self.devices[index].chan, bytes, 0) {
			self.devices[index].pending = pending;
			true
		} else {
			self.device_failed(index);
			false
		}
	}

	fn has_audio(&self, id: u32) -> bool {
		let default_output = self.routing.default(Direction::Output) == Some(id);
		self.streams.iter().any(|stream| stream.device == Some(id) && stream.queued_frames() != 0) || (default_output && self.tones.iter().any(|tone| tone.remaining != 0))
	}

	// ONE PERIOD OF A DEVICE'S OUTPUT: every stream on it stepped at the device's rate, the tones on the default output,
	// mixed with saturation, the level applied where the device does not apply it itself, and the device's channel count.
	fn fill_period(&mut self, index: usize) {
		let (id, format, frames, hardware, volume) = {
			let device = &self.devices[index];
			let Some(format) = device.output() else { return };
			(device.id, format, device.period_frames(), device.format.hardware_volume, device.volume)
		};
		let default_output = self.routing.default(Direction::Output) == Some(id);
		let gain = if hardware { audio_routing::gain_q15(100) } else { audio_routing::gain_q15(volume) };
		let mut period = core::mem::take(&mut self.devices[index].period);
		period.resize(self.devices[index].format.period_bytes as usize, 0);
		for frame in 0..frames {
			let mut left: i32 = 0;
			let mut right: i32 = 0;
			for stream in self.streams.iter_mut().filter(|stream| stream.device == Some(id)) {
				if let Some((l, r)) = stream.next_frame(format.rate) {
					left += l as i32;
					right += r as i32;
				}
			}
			if default_output {
				for tone in &mut self.tones {
					if let Some((l, r)) = tone.next_frame() {
						left += l as i32;
						right += r as i32;
					}
				}
			}
			let left = audio_routing::scale(left.clamp(i16::MIN as i32, i16::MAX as i32) as i16, gain);
			let right = audio_routing::scale(right.clamp(i16::MIN as i32, i16::MAX as i32) as i16, gain);
			if format.channels == 2 {
				let offset = frame * 4;
				period[offset..offset + 2].copy_from_slice(&left.to_le_bytes());
				period[offset + 2..offset + 4].copy_from_slice(&right.to_le_bytes());
			} else {
				let offset = frame * 2;
				let mono = ((left as i32 + right as i32) / 2) as i16;
				period[offset..offset + 2].copy_from_slice(&mono.to_le_bytes());
			}
		}
		self.devices[index].period = period;
		for stream in &mut self.streams {
			stream.compact();
		}
		if default_output {
			self.tones.retain(|tone| tone.remaining != 0);
		}
	}

	// THE DEVICE ANSWERED what it was asked.
	fn device_reply(&mut self, index: usize, reply: &[u8]) {
		let pending = self.devices[index].pending;
		match pending {
			Pending::Format => self.format_answered(index, reply),
			Pending::Volume => {
				let device = &mut self.devices[index];
				device.pending = Pending::None;
				// A DEVICE WITH NO LEVEL OF ITS OWN is scaled here from now on.
				if reply != driver_protocol::audio::OK {
					device.format.hardware_volume = false;
				}
			}
			Pending::Stats => {
				let device = &mut self.devices[index];
				device.pending = Pending::None;
				device.counters = PlaybackStats::decode(reply);
			}
			Pending::Capture => self.capture_answered(index, reply),
			Pending::CaptureStop => self.devices[index].pending = Pending::None,
			Pending::Period | Pending::Stop => {
				let played = !reply.is_empty();
				let device = &mut self.devices[index];
				device.pending = Pending::None;
				match pending {
					Pending::Stop => {
						device.running = false;
						device.stats_due = true;
					}
					_ if played => {
						device.periods_since_stats += 1;
						if device.periods_since_stats >= STATS_EVERY {
							device.stats_due = true;
						}
					}
					_ => {}
				}
				// A REFUSAL IS AN EMPTY REPLY; a run of them is a device that is not playing anything.
				if played {
					device.refusals = 0;
				} else {
					device.running = false;
					device.refusals = device.refusals.saturating_add(1);
					if device.refusals >= REFUSAL_LIMIT {
						self.device_failed(index);
					}
				}
			}
			Pending::None => {}
		}
	}

	// ONE CAPTURED PERIOD, in the device's input format: to the phone's jitter buffer, or converted for every recorder
	// on the device that has not got one waiting - at the device's level where it does not apply it itself. An EMPTY
	// answer is the device saying it cannot capture.
	fn capture_answered(&mut self, index: usize, period: &[u8]) {
		let device = &mut self.devices[index];
		device.pending = Pending::None;
		if period.is_empty() {
			device.input_refused = true;
			return;
		}
		// A headset applies its microphone gain itself. Software gain is independent once explicitly selected;
		// before that the old combined level remains, including routes which are not microphones.
		let gain = audio_routing::gain_q15(if device.hardware_microphone_volume { 100 } else { device.microphone_level() });
		let samples: Vec<i16> = period.chunks_exact(2).map(|pair| audio_routing::scale(i16::from_le_bytes([pair[0], pair[1]]), gain)).collect();
		if let Some(jitter) = device.jitter.as_mut() {
			jitter.push(samples);
			return;
		}
		let id = device.id;
		for capture in self.captures.iter_mut().filter(|capture| capture.device == Some(id) && capture.ready.is_none()) {
			let Some((remix, resample)) = capture.conversion.as_mut() else { continue };
			// MIXED DOWN FIRST, then resampled: the resampler is built for the recorder's channel count.
			let mut remixed: Vec<i16> = Vec::new();
			if remix.apply(&samples, &mut remixed).is_none() {
				continue;
			}
			let mut converted: Vec<i16> = Vec::new();
			if resample.push(&remixed, &mut converted).is_none() {
				continue;
			}
			capture.ready = Some(converted.iter().flat_map(|sample| sample.to_le_bytes()).collect());
		}
	}

	// ------------------------------------------------------------------ latency, counters and the inventory

	fn hold_latency(&mut self) {
		let running = self.devices.iter().any(|device| device.running || device.capture_running);
		if running && self.latency == 0 && self.latency_privilege != 0 {
			let answer = unsafe { syscall(SYS_LATENCY_REQUEST, self.latency_privilege, LATENCY_BOUND_US, 0, 0) } as i64;
			if answer > 0 {
				self.latency = answer as u64;
			} else {
				print(b"AudioService: the kernel refused the idle-latency request - the device plays unbounded\n");
			}
		} else if !running && self.latency != 0 {
			close(self.latency);
			self.latency = 0;
		}
	}

	// WHAT THE SYSTEM GRAPH IS ANSWERED: the default output's counters as last read.
	fn resources(&self) -> AudioResources {
		let counted = self.routing.default(Direction::Output).and_then(|id| self.device(id)).and_then(|device| device.counters);
		match counted {
			Some(counted) => AudioResources { counted: true, underruns: counted.underruns, silent_frames: counted.silent_frames, feedback_q16: counted.feedback_q16, feedback_ignored: counted.feedback_ignored },
			None => AudioResources { counted: false, underruns: 0, silent_frames: 0, feedback_q16: 0, feedback_ignored: 0 },
		}
	}

	fn inventory(&self) -> Vec<AudioDevice> {
		let (output, input, voice) = (self.routing.default(Direction::Output), self.routing.default(Direction::Input), self.routing.default(Direction::Voice));
		self.devices.iter().filter(|device| device.known).map(|device| AudioDevice { id: device.id, label: device.label.clone(), transport: transport_of(device.origin), output: format_of(device.output()), input: format_of(device.input()), voice: device.voice, route: device.route, latency_us: device.format.latency_us, volume: device.volume, hardware_volume: device.format.hardware_volume, default_output: output == Some(device.id), default_input: input == Some(device.id), default_voice: voice == Some(device.id) }).collect()
	}

	// A STREAM'S LATENCY: what it holds queued at its own rate, and its device's own.
	fn stream_latency(&self, stream: &Stream) -> u32 {
		let queued = (stream.queued_frames() as u64 * 1_000_000 / u64::from(stream.format.rate().max(1))) as u32;
		queued.saturating_add(stream.device.and_then(|id| self.device(id)).map_or(0, |device| device.format.latency_us))
	}

	fn stream_list(&self) -> Vec<AudioStreamInfo> {
		self.streams.iter().map(|stream| AudioStreamInfo { device: stream.device, named: stream.named, format: AudioFormat { rate: stream.format.rate(), channels: stream.format.channels() }, latency_us: self.stream_latency(stream), silent_frames: stream.silent_frames, voice: matches!(stream.front, Front::Voice(_)), route: matches!(stream.front, Front::Route(_)) }).collect()
	}

	fn counters(&self) -> AudioCounters {
		let (overflows, underruns) = self.devices.iter().filter_map(|device| device.jitter.as_ref()).fold((0u64, 0u64), |(o, u), jitter| (o + jitter.overflows, u + jitter.underruns));
		AudioCounters { moves: self.moves, silent_frames: self.silent_frames, route_overflows: overflows, route_underruns: underruns }
	}

	// THE OPERATOR'S LEVEL: sent to a device that applies it itself, or kept for the samples to be scaled here.
	fn set_volume(&mut self, id: u32, volume: u8) -> Result<(), Error> {
		if volume > audio_routing::MAX_LEVEL {
			return Err(Error::Invalid);
		}
		let index = self.index_of(id).ok_or(Error::NotFound)?;
		let device = &mut self.devices[index];
		device.volume = volume;
		if !device.format.hardware_volume {
			return Ok(());
		}
		match device.origin {
			Origin::Provider { .. } => device.volume_due = true,
			Origin::Bluetooth { endpoint } => {
				if self.bt.client == 0 || !matches!(bluetooth_audio::Client::new(ChannelTransport { chan: self.bt.client }).set_volume(&endpoint, &volume), Some(Ok(()))) {
					return Err(Error::Closed);
				}
			}
		}
		Ok(())
	}

	fn microphone_volume(&self, id: u32) -> Result<u8, Error> {
		let device = self.device(id).ok_or(Error::NotFound)?;
		if device.input().is_none() || device.route {
			return Err(Error::Invalid);
		}
		// A legacy Bluetooth stack never reports its headset microphone gain. A speaker level is not evidence of it.
		if matches!(device.origin, Origin::Bluetooth { .. }) && device.format.hardware_volume && device.microphone_volume.is_none() {
			return Err(Error::Unsupported);
		}
		Ok(device.microphone_level())
	}

	fn set_microphone_volume(&mut self, id: u32, volume: u8) -> Result<(), Error> {
		if volume > audio_routing::MAX_LEVEL {
			return Err(Error::Invalid);
		}
		let index = self.index_of(id).ok_or(Error::NotFound)?;
		if self.devices[index].input().is_none() || self.devices[index].route {
			return Err(Error::Invalid);
		}
		let (level, hardware) = match self.devices[index].origin {
			Origin::Provider { .. } => (volume, false),
			Origin::Bluetooth { endpoint } => {
				if self.bt.client == 0 {
					return Err(Error::Closed);
				}
				let mut client = bluetooth_audio::Client::with_deadline(ChannelTransport { chan: self.bt.client }, clock().saturating_add(2 * TICKS_PER_SECOND));
				let answer = client.set_microphone_volume(&endpoint, &volume);
				if answer.is_none() || client.last_error().is_some() {
					// An old stack may not answer the appended operation. Never block all sound indefinitely,
					// and never let its late reply satisfy a later call with a reused correlation number.
					self.bluetooth_lost();
					return Err(answer.and_then(Result::err).unwrap_or(Error::Closed));
				}
				match answer {
					Some(Ok(level)) => (level, true),
					Some(Err(Error::Unsupported)) => (volume, false),
					Some(Err(error)) => return Err(error),
					None => return Err(Error::Closed),
				}
			}
		};
		let device = &mut self.devices[index];
		device.microphone_volume = Some(level);
		device.hardware_microphone_volume = hardware;
		Ok(())
	}

	// ------------------------------------------------------------------ streams, recorders and sessions

	fn remove_stream(&mut self, index: usize) {
		let mut stream = self.streams.swap_remove(index);
		stream.release();
	}

	fn remove_capture(&mut self, index: usize) {
		let capture = self.captures.swap_remove(index);
		if capture.chan != 0 && capture.front == Front::Own {
			close(capture.chan);
		}
	}

	fn remove_voice(&mut self, chan: u64) {
		let Some(at) = self.voices.iter().position(|voice| voice.chan == chan) else { return };
		let voice = self.voices.swap_remove(at);
		if voice.commands != 0 {
			close(voice.commands);
		}
		self.streams.retain_mut(|stream| {
			let mine = stream.front == Front::Voice(chan);
			if mine {
				stream.release();
			}
			!mine
		});
		self.captures.retain(|capture| capture.front != Front::Voice(chan));
		close(chan);
	}

	fn dispatch_stream(&mut self, index: usize, request: &[u8], caps: proto::codec::Handles) {
		let front = self.streams[index].front;
		let mut reply: Vec<u8> = alloc::vec![0; REQUEST_MAX];
		let mut reply_handle = proto::codec::Handles::new();
		let mut request_handle = caps;
		let latency = self.stream_latency(&self.streams[index]);
		let (chan, len) = match front {
			Front::Voice(chan) => {
				let Some(voice) = self.voices.iter_mut().position(|voice| voice.chan == chan) else { return };
				let mut call = VoiceCall { audio: self, voice, stream: Some(index), latency };
				(chan, voice_session::dispatch(&mut call, request, &mut request_handle, &mut reply, &mut reply_handle))
			}
			_ => {
				let chan = self.streams[index].chan;
				let mut call = StreamCall { stream: &mut self.streams[index], latency };
				(chan, pcm_stream::dispatch(&mut call, request, &mut request_handle, &mut reply, &mut reply_handle))
			}
		};
		answer(chan, len.map(|len| &reply[..len]), &reply_handle);
		for &unclaimed in request_handle.as_slice() {
			close(unclaimed);
		}
	}

	fn dispatch_capture(&mut self, index: usize, request: &[u8]) {
		let front = self.captures[index].front;
		let mut reply: Vec<u8> = alloc::vec![0; REPLY_MAX];
		let mut reply_handle = proto::codec::Handles::new();
		let mut request_handle = proto::codec::Handles::new();
		let available = self.capture_available(index);
		let (chan, len) = match front {
			Front::Voice(chan) => {
				let Some(voice) = self.voices.iter().position(|voice| voice.chan == chan) else { return };
				let latency = self.streams.iter().find(|stream| stream.front == Front::Voice(chan)).map_or(0, |stream| self.stream_latency(stream));
				let mut call = VoiceCall { audio: self, voice, stream: None, latency };
				(chan, voice_session::dispatch(&mut call, request, &mut request_handle, &mut reply, &mut reply_handle))
			}
			_ => {
				let chan = self.captures[index].chan;
				let latency = self.captures[index].device.and_then(|id| self.device(id)).map_or(0, |device| device.format.latency_us);
				let mut call = CaptureCall { capture: &mut self.captures[index], available, latency };
				(chan, pcm_capture::dispatch(&mut call, request, &mut request_handle, &mut reply, &mut reply_handle))
			}
		};
		answer(chan, len.map(|len| &reply[..len]), &reply_handle);
		for &unclaimed in request_handle.as_slice() {
			close(unclaimed);
		}
	}

	// WHETHER A RECORDER CAN BE GIVEN A PERIOD AT ALL: it has a device, and that device has not refused to capture.
	fn capture_available(&self, index: usize) -> bool {
		self.captures[index].device.and_then(|id| self.device(id)).is_some_and(|device| !device.input_refused && device.input().is_some())
	}

	// A `write` with no room, or a `read` with no period and a device that will give one, is DEFERRED.
	fn take_stream_request(&mut self, index: usize, request: &[u8], handles: proto::codec::Handles) {
		let op: u16 = if request.len() >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
		let write = match self.streams[index].front {
			Front::Voice(_) => voice_session::OP_WRITE,
			_ => pcm_stream::OP_WRITE,
		};
		if op == write && self.streams[index].capacity() == 0 && !handles.is_empty() {
			self.streams[index].pending = Some(PendingWrite { request: request.to_vec(), caps: handles });
		} else {
			self.dispatch_stream(index, request, handles);
		}
	}

	fn take_capture_request(&mut self, index: usize, request: &[u8]) {
		let op: u16 = if request.len() >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
		let read = match self.captures[index].front {
			Front::Voice(_) => voice_session::OP_READ,
			_ => pcm_capture::OP_READ,
		};
		if op == read && self.captures[index].ready.is_none() && self.capture_available(index) {
			self.captures[index].pending = Some(request.to_vec());
		} else {
			self.dispatch_capture(index, request);
		}
	}

	fn service_pending(&mut self) {
		let mut index = 0;
		while index < self.streams.len() {
			if self.streams[index].capacity() != 0
				&& let Some(pending) = self.streams[index].pending.take()
			{
				self.dispatch_stream(index, &pending.request, pending.caps);
			}
			index += 1;
		}
		index = 0;
		while index < self.captures.len() {
			let answerable = self.captures[index].ready.is_some() || !self.capture_available(index);
			if answerable && let Some(request) = self.captures[index].pending.take() {
				self.dispatch_capture(index, &request);
			}
			index += 1;
		}
	}

	fn cleanup_drained(&mut self) {
		let mut index: usize = 0;
		while index < self.captures.len() {
			if self.captures[index].closing && self.captures[index].pending.is_none() && self.captures[index].front == Front::Own {
				self.remove_capture(index);
			} else {
				index += 1;
			}
		}
		index = 0;
		while index < self.streams.len() {
			let stream = &self.streams[index];
			if stream.closing && stream.queued_frames() == 0 && stream.pending.is_none() && stream.front == Front::Own {
				self.remove_stream(index);
			} else {
				index += 1;
			}
		}
		let ended: Vec<u64> = self.voices.iter().filter(|voice| voice.closing).map(|voice| voice.chan).collect();
		for chan in ended {
			let drained = self.streams.iter().filter(|stream| stream.front == Front::Voice(chan)).all(|stream| stream.queued_frames() == 0 && stream.pending.is_none());
			if drained {
				self.remove_voice(chan);
			}
		}
	}

	// ------------------------------------------------------------------ Bluetooth

	// THE RETRY: ask the broker for Bluetooth's audio authority if there is none, or for the endpoints stream if the
	// authority has none open.
	fn bluetooth_retry(&mut self) {
		self.bt.retry_at = clock().saturating_add(BLUETOOTH_RETRY_TICKS);
		if self.bt.refused || self.bt.events != 0 {
			return;
		}
		if self.bt.client == 0 {
			if !self.bt.resolving && self.bt.broker != 0 {
				let mut request = Vec::with_capacity(2 + CAP_BT_AUDIO.len());
				request.extend_from_slice(&RESOLVE_OP.to_le_bytes());
				request.extend_from_slice(CAP_BT_AUDIO);
				self.bt.resolving = send_blocking(self.bt.broker, &request, 0);
			}
			return;
		}
		let mut client = bluetooth_audio::Client::new(ChannelTransport { chan: self.bt.client });
		match client.endpoints() {
			Some(Ok(stream)) => {
				self.bt.events = stream;
				self.bt.relayed = BtCallState::None;
			}
			_ if client.last_error().is_some() => {
				close(self.bt.client);
				self.bt.client = 0;
			}
			_ => {}
		}
	}

	fn bluetooth_answered(&mut self) {
		let mut reply = [0u8; 16];
		match try_recv(self.bt.broker, &mut reply) {
			Polled::Message { len, handle } => {
				self.bt.resolving = false;
				if len >= 2 && &reply[..2] == b"OK" && handle != 0 {
					self.bt.client = handle;
					self.bt.retry_at = clock();
				} else {
					if handle != 0 {
						close(handle);
					}
					if len >= 6 && &reply[..6] == b"DENIED" {
						self.bt.refused = true;
					}
				}
			}
			Polled::Empty => {}
			Polled::Closed => {
				self.bt.resolving = false;
				self.bt.refused = true;
			}
		}
	}

	fn bluetooth_lost(&mut self) {
		close(self.bt.events);
		self.bt.events = 0;
		close(self.bt.client);
		self.bt.client = 0;
		while let Some(index) = self.devices.iter().position(|device| matches!(device.origin, Origin::Bluetooth { .. })) {
			self.remove_device(index);
		}
		self.bt.retry_at = clock().saturating_add(BLUETOOTH_RETRY_TICKS);
	}

	// THE ENDPOINTS STREAM: arrivals, departures, a device's own level, a headset's call command - or its end, which is
	// the stack's instance ending: every endpoint leaves, and the broker is asked again for the live one.
	fn bluetooth_events(&mut self) {
		let mut buf = [0u8; 512];
		loop {
			match try_recv_caps(self.bt.events, &mut buf) {
				PolledCaps::Message { len, handles } => {
					for &handle in handles.as_slice() {
						close(handle);
					}
					let mut frame = wire::Handles::new();
					let Some(event) = bluetooth_audio::endpoints_read(&buf[..len], &mut frame) else { continue };
					match event {
						AudioEvent::Arrived(endpoint) => self.endpoint_arrived(&endpoint),
						AudioEvent::Departed(endpoint) => self.endpoint_departed(endpoint),
						AudioEvent::Volume(level) => {
							if let Some(device) = self.devices.iter_mut().find(|device| device.origin == Origin::Bluetooth { endpoint: level.id }) {
								device.volume = level.volume.min(audio_routing::MAX_LEVEL);
							}
						}
						AudioEvent::MicrophoneVolume(level) => {
							if let Some(device) = self.devices.iter_mut().find(|device| device.origin == Origin::Bluetooth { endpoint: level.id }) {
								device.microphone_volume = Some(level.volume.min(audio_routing::MAX_LEVEL));
								device.hardware_microphone_volume = true;
							}
						}
						AudioEvent::Command(command) => self.call_command(command),
					}
				}
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					self.bluetooth_lost();
					return;
				}
			}
		}
	}
}

// Send a reply - or close what it would have carried when it cannot go.
fn answer(chan: u64, reply: Option<&[u8]>, handles: &proto::codec::Handles) {
	match reply {
		Some(bytes) if send_caps_blocking(chan, bytes, handles.as_slice()) => {}
		_ => {
			for &leftover in handles.as_slice() {
				close(leftover);
			}
		}
	}
}

// ------------------------------------------------------------------ the interfaces

struct RootCall<'a> {
	audio: &'a mut Audio,
	scope: Scope,
}

impl RootCall<'_> {
	fn stream_to(&mut self, named: Option<u32>, rate: u32, channels: u8) -> Result<u64, Error> {
		if !matches!(self.scope, Scope::Full | Scope::StreamOnly) {
			return Err(Error::Denied);
		}
		let format: Format = Format::new(rate, channels).ok_or(Error::Invalid)?;
		if let Some(id) = named
			&& !self.audio.routing.serves(id, Direction::Output)
		{
			return Err(Error::NotFound);
		}
		if self.audio.streams.len() >= MAX_STREAMS {
			return Err(Error::Again);
		}
		let (server, client): (u64, u64) = channel().ok_or(Error::Again)?;
		self.audio.streams.push(Stream::new(server, Front::Own, format, named));
		Ok(client)
	}

	// WHETHER A DEVICE CAN CAPTURE IS NOT ASKED HERE - that would be a blocking round trip to a driver inside a dispatch.
	// A recorder with no input device, or on one that refuses, opens and is answered not-found on its `read`, which is
	// where it finds out either way.
	fn capture_from(&mut self, named: Option<u32>, rate: u32, channels: u8) -> Result<u64, Error> {
		if !matches!(self.scope, Scope::Full | Scope::CaptureOnly) {
			return Err(Error::Denied);
		}
		Format::new(rate, channels).ok_or(Error::Invalid)?;
		if let Some(id) = named
			&& !self.audio.routing.serves(id, Direction::Input)
		{
			return Err(Error::NotFound);
		}
		if self.audio.captures.iter().filter(|capture| capture.front == Front::Own).count() >= MAX_CAPTURES {
			return Err(Error::Again);
		}
		let (server, client): (u64, u64) = channel().ok_or(Error::Again)?;
		self.audio.captures.push(Capture { chan: server, front: Front::Own, rate, channels, named, device: None, conversion: None, pending: None, ready: None, closing: false });
		Ok(client)
	}
}

impl AudioService for RootCall<'_> {
	fn beep(&mut self, freq: u16, millis: u32) -> Result<(), Error> {
		// No restricted scope may beep - see `audio-admin` in the IDL.
		if self.scope != Scope::Full {
			return Err(Error::Denied);
		}
		let Some(rate) = self.audio.routing.default(Direction::Output).and_then(|id| self.audio.device(id)).and_then(Device::output).map(|format| format.rate) else { return Err(Error::NotFound) };
		if self.audio.tones.len() >= MAX_TONES {
			return Err(Error::Again);
		}
		let freq: u32 = (freq as u32).clamp(20, 20_000);
		let millis: u32 = millis.clamp(1, 5_000);
		let remaining: u32 = ((rate as u64 * millis as u64) / 1_000).max(1) as u32;
		self.audio.tones.push(Tone { remaining, frame: 0, half_period: (rate / (2 * freq)).max(1) });
		Ok(())
	}

	fn open_stream(&mut self, rate: u32, channels: u8) -> Result<u64, Error> {
		self.stream_to(None, rate, channels)
	}

	fn open_capture(&mut self, rate: u32, channels: u8) -> Result<u64, Error> {
		self.capture_from(None, rate, channels)
	}

	fn devices(&mut self) -> Vec<AudioDevice> {
		self.audio.inventory()
	}

	fn open_stream_to(&mut self, device: u32, rate: u32, channels: u8) -> Result<u64, Error> {
		self.stream_to(Some(device), rate, channels)
	}

	fn open_capture_from(&mut self, device: u32, rate: u32, channels: u8) -> Result<u64, Error> {
		self.capture_from(Some(device), rate, channels)
	}

	// A DUPLEX SESSION AT A VOICE RATE, minted only from a connection allowed both ways.
	fn open_voice(&mut self, rate: u32) -> Result<u64, Error> {
		if !matches!(self.scope, Scope::Full | Scope::VoiceOnly) {
			return Err(Error::Denied);
		}
		if rate != 8_000 && rate != 16_000 {
			return Err(Error::Invalid);
		}
		let format = Format::new(rate, 1).ok_or(Error::Invalid)?;
		if self.audio.voices.len() >= MAX_VOICES || self.audio.streams.len() >= MAX_STREAMS + MAX_VOICES {
			return Err(Error::Again);
		}
		let (server, client): (u64, u64) = channel().ok_or(Error::Again)?;
		self.audio.voices.push(Voice { chan: server, call: CallState::None, commands: 0, closing: false });
		self.audio.streams.push(Stream::new(0, Front::Voice(server), format, None));
		self.audio.captures.push(Capture { chan: 0, front: Front::Voice(server), rate, channels: 1, named: None, device: None, conversion: None, pending: None, ready: None, closing: false });
		Ok(client)
	}
}

// THE OBSERVATION ROOT'S CALLS, which can read the counters and change nothing.
struct StatsCall<'a> {
	audio: &'a Audio,
}

impl StatsService for StatsCall<'_> {
	fn resources(&mut self) -> AudioResources {
		self.audio.resources()
	}
}

// THE OPERATOR'S CALLS.
struct ControlCall<'a> {
	audio: &'a mut Audio,
}

impl ControlService for ControlCall<'_> {
	fn devices(&mut self) -> Vec<AudioDevice> {
		self.audio.inventory()
	}

	fn set_default(&mut self, device: u32, direction: AudioDirection) -> Result<(), Error> {
		if self.audio.device(device).is_none() {
			return Err(Error::NotFound);
		}
		let direction = match direction {
			AudioDirection::Output => Direction::Output,
			AudioDirection::Input => Direction::Input,
			AudioDirection::Voice => Direction::Voice,
		};
		if self.audio.routing.choose(device, direction) { Ok(()) } else { Err(Error::Invalid) }
	}

	fn set_volume(&mut self, device: u32, volume: u8) -> Result<(), Error> {
		self.audio.set_volume(device, volume)
	}

	fn microphone_volume(&mut self, device: u32) -> Result<u8, Error> {
		self.audio.microphone_volume(device)
	}

	fn set_microphone_volume(&mut self, device: u32, volume: u8) -> Result<(), Error> {
		self.audio.set_microphone_volume(device, volume)
	}

	fn streams(&mut self) -> Vec<AudioStreamInfo> {
		self.audio.stream_list()
	}

	fn counters(&mut self) -> AudioCounters {
		self.audio.counters()
	}
}

struct AdminCall<'a> {
	clients: &'a mut Vec<Client>,
}

impl AdminCall<'_> {
	fn mint(&mut self, scope: Scope) -> Result<u64, Error> {
		let (server, client): (u64, u64) = channel().ok_or(Error::Again)?;
		self.clients.push(Client { chan: server, scope });
		Ok(client)
	}
}

impl AdminService for AdminCall<'_> {
	fn open_streams(&mut self) -> Result<u64, Error> {
		self.mint(Scope::StreamOnly)
	}

	fn open_captures(&mut self) -> Result<u64, Error> {
		self.mint(Scope::CaptureOnly)
	}

	fn open_voices(&mut self) -> Result<u64, Error> {
		self.mint(Scope::VoiceOnly)
	}
}

struct StreamCall<'a> {
	stream: &'a mut Stream,
	latency: u32,
}

impl PcmService for StreamCall<'_> {
	fn write(&mut self, data: Buffer) -> Result<u32, Error> {
		self.stream.write_buffer(data)
	}

	fn close(&mut self) -> Result<(), Error> {
		self.stream.closing = true;
		Ok(())
	}

	fn latency(&mut self) -> u32 {
		self.latency
	}
}

struct CaptureCall<'a> {
	capture: &'a mut Capture,
	available: bool,
	latency: u32,
}

impl PcmCaptureService for CaptureCall<'_> {
	// One period, or the reason there is not one. A `read` with neither is deferred until the device has answered.
	fn read(&mut self) -> Result<Vec<u8>, Error> {
		if let Some(period) = self.capture.ready.take() {
			return Ok(period);
		}
		if !self.available {
			return Err(Error::NotFound);
		}
		Err(Error::Again)
	}

	fn close(&mut self) -> Result<(), Error> {
		self.capture.closing = true;
		Ok(())
	}

	fn latency(&mut self) -> u32 {
		self.latency
	}
}

// A VOICE SESSION'S CALLS: its playback half on its stream, its capture half on its recorder, and the call it declares.
struct VoiceCall<'a> {
	audio: &'a mut Audio,
	voice: usize,
	stream: Option<usize>,
	latency: u32,
}

impl VoiceCall<'_> {
	fn chan(&self) -> u64 {
		self.audio.voices[self.voice].chan
	}
}

impl VoiceService for VoiceCall<'_> {
	fn write(&mut self, data: Buffer) -> Result<u32, Error> {
		let chan = self.chan();
		let index = self.stream.or_else(|| self.audio.streams.iter().position(|stream| stream.front == Front::Voice(chan)));
		match index {
			Some(index) => self.audio.streams[index].write_buffer(data),
			None => {
				close(data.handle);
				Err(Error::Closed)
			}
		}
	}

	fn read(&mut self) -> Result<Vec<u8>, Error> {
		let chan = self.chan();
		let Some(index) = self.audio.captures.iter().position(|capture| capture.front == Front::Voice(chan)) else { return Err(Error::Closed) };
		if let Some(period) = self.audio.captures[index].ready.take() {
			return Ok(period);
		}
		if !self.audio.capture_available(index) {
			return Err(Error::NotFound);
		}
		Err(Error::Again)
	}

	fn set_call(&mut self, state: CallState) -> Result<(), Error> {
		self.audio.voices[self.voice].call = state;
		Ok(())
	}

	// Validated here; the stream itself is made by `serve_commands`, which owns the channel.
	fn commands(&mut self) -> Result<Vec<CallCommand>, Error> {
		if self.audio.voices[self.voice].commands != 0 {
			return Err(Error::Again);
		}
		Ok(Vec::new())
	}

	fn latency(&mut self) -> u32 {
		self.latency
	}

	fn close(&mut self) -> Result<(), Error> {
		let chan = self.chan();
		self.audio.voices[self.voice].closing = true;
		for stream in self.audio.streams.iter_mut().filter(|stream| stream.front == Front::Voice(chan)) {
			stream.closing = true;
		}
		Ok(())
	}
}

// THE COMMANDS STREAM OF A VOICE SESSION: a channel whose producer end this service keeps.
fn serve_commands(audio: &mut Audio, voice: usize, request: &[u8], handles: &mut wire::Handles) {
	let chan = audio.voices[voice].chan;
	let mut reply = [0u8; 64];
	let mut call = VoiceCall { audio, voice, stream: None, latency: 0 };
	let Some((corr, result)) = voice_session::commands_open(&mut call, request, handles) else { return };
	let error = match result {
		Ok(_) => match channel_with_depth(8) {
			Some((producer, consumer)) => {
				if let Some(len) = voice_session::commands_reply_ok(corr, &mut reply)
					&& send_caps_blocking(chan, &reply[..len], &[consumer])
				{
					audio.voices[voice].commands = producer;
					return;
				}
				close(producer);
				close(consumer);
				return;
			}
			None => Error::Exhausted,
		},
		Err(error) => error,
	};
	if let Some(len) = voice_session::commands_reply_err(corr, &error, &mut reply) {
		send_blocking(chan, &reply[..len], 0);
	}
}

// ------------------------------------------------------------------ the service

pub fn run(bootstrap: u64) -> ! {
	let mut bootstrap_buf: [u8; 256] = [0; 256];
	let admin: u64 = recv_tagged(bootstrap, &mut bootstrap_buf, b"ADMIN").unwrap_or_else(|| fail_bootstrap(bootstrap, b"admin", b"audio admin channel not delivered"));
	let root: u64 = recv_tagged(bootstrap, &mut bootstrap_buf, b"SERVE").unwrap_or_else(|| fail_bootstrap(bootstrap, b"serve", b"missing serve channel"));
	// THE DEVICES ARE DISCOVERED, NOT HANDED OVER: a connection to the provider catalogue, subscribed to the audio kind,
	// so a sound card bound after this service started reaches it and a machine with two has two.
	//
	// THE ROLES ARE READ POSITIONALLY at every hop, in the order the manifest lists them.
	let catalogue: u64 = recv_tagged(bootstrap, &mut bootstrap_buf, b"CATALOGUE").unwrap_or(0);
	let latency_privilege: u64 = recv_tagged(bootstrap, &mut bootstrap_buf, b"LATENCY").unwrap_or(0);
	// THE OBSERVATION ROOT: what the System Graph reads the counters through, and nothing else.
	let stats_root: u64 = recv_tagged(bootstrap, &mut bootstrap_buf, b"STATS").unwrap_or(0);
	// THE OPERATOR ROOT, LAST: the inventory, the defaults and the levels, for `audioctl` through PermissionManager.
	let control_root: u64 = recv_tagged(bootstrap, &mut bootstrap_buf, b"CONTROL").unwrap_or(0);
	send_blocking(bootstrap, b"AudioService: online", 0);
	// The subscription is opened BEFORE anything is served, so the snapshot and the stream are one operation.
	let providers: u64 = if catalogue == 0 { 0 } else { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::Audio).unwrap_or(0) };
	if providers == 0 {
		print(b"AudioService: no provider subscription - this instance serves no sound card\n");
	}
	let mut audio = Audio::new(bootstrap);
	audio.latency_privilege = latency_privilege;
	serve(root, admin, stats_root, control_root, catalogue, providers, audio);
}

// OPEN A CONNECTION TO ONE PUBLISHED PROVIDER, or answer zero.
fn open_provider(catalogue: u64, info: &ProviderInfo) -> u64 {
	if catalogue == 0 {
		return 0;
	}
	match provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(info) {
		Some(Ok(handle)) => handle,
		// A REFUSAL IS SAID, and so is a transport that did not answer: neither is an absent device.
		Some(Err(_)) => {
			print(b"AudioService: the catalogue refused a connection to an audio provider it published\n");
			0
		}
		None => {
			print(b"AudioService: the catalogue did not answer the connection it published\n");
			0
		}
	}
}

// A ROOT OR A CONNECTION MINTED FROM IT that answers only the heartbeat, the connect and one interface.
fn serve_minted(chans: &mut Vec<u64>, ready: u64, request: &mut [u8], answer_call: &mut dyn FnMut(&[u8], &mut [u8]) -> Option<usize>) {
	let Some(which) = chans.iter().position(|held| *held == ready) else { return };
	match recv_blocking(ready, request) {
		Received::Message { len, handle } => {
			if handle != 0 {
				close(handle);
			}
			let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
			if op == HEARTBEAT_OP {
				send_blocking(ready, b"PONG", 0);
			} else if op == CONNECT_OP {
				match channel() {
					Some((mine, theirs)) => {
						chans.push(mine);
						send_blocking(ready, &[], theirs);
					}
					None => {
						send_blocking(ready, &[], 0);
					}
				}
			} else {
				let mut reply: Vec<u8> = alloc::vec![0; 4096];
				let bytes = request[..len].to_vec();
				if let Some(n) = answer_call(&bytes, &mut reply) {
					send_blocking(ready, &reply[..n], 0);
				}
			}
		}
		// A connection went away, which changes nothing about playing sound - nor does the root going away.
		Received::Closed => {
			close(ready);
			chans.remove(which);
		}
	}
}

fn serve(root: u64, admin: u64, stats_root: u64, control_root: u64, catalogue: u64, mut providers: u64, mut state: Audio) -> ! {
	let mut clients: Vec<Client> = alloc::vec![Client { chan: root, scope: Scope::Full }];
	// THE OBSERVATION AND OPERATOR ROOTS AND THE CONNECTIONS MINTED FROM THEM, each its own channel: two holders sharing
	// one would take each other's replies.
	let mut stats: Vec<u64> = if stats_root != 0 { alloc::vec![stats_root] } else { Vec::new() };
	let mut controls: Vec<u64> = if control_root != 0 { alloc::vec![control_root] } else { Vec::new() };
	let mut request: [u8; REQUEST_MAX] = [0; REQUEST_MAX];
	let mut device_buf: Vec<u8> = alloc::vec![0; MAX_PERIOD_BYTES as usize + 64];
	loop {
		// THE TURN: requests already waiting are taken, deferred ones answered where they can be, the routing rule
		// followed, the clocks with no device advanced, and each device given its next request.
		poll_clients(&mut state);
		state.place_all();
		state.service_pending();
		state.cleanup_drained();
		state.voice_links();
		state.relay_call();
		let idle = state.pace_idle();
		state.feed_routes();
		for index in (0..state.devices.len()).rev() {
			if index < state.devices.len() {
				state.pump(index);
			}
		}
		state.hold_latency();

		let mut waits: Vec<u64> = Vec::with_capacity(state.devices.len() + clients.len() + stats.len() + controls.len() + state.streams.len() + state.captures.len() + state.voices.len() + 5);
		// AN IDLE DEVICE'S CHANNEL CAN CLOSE TOO, and is observed before a replacement publication is consumed.
		waits.extend(state.devices.iter().filter(|device| device.chan != 0).map(|device| device.chan));
		if providers != 0 {
			waits.push(providers);
		}
		if state.bt.events != 0 {
			waits.push(state.bt.events);
		}
		if state.bt.resolving {
			waits.push(state.bt.broker);
		}
		waits.push(admin);
		waits.extend(stats.iter().copied());
		waits.extend(controls.iter().copied());
		waits.extend(clients.iter().map(|client| client.chan));
		waits.extend(state.streams.iter().filter(|stream| stream.front == Front::Own && stream.chan != 0 && stream.pending.is_none()).map(|stream| stream.chan));
		waits.extend(state.captures.iter().filter(|capture| capture.front == Front::Own && capture.chan != 0 && capture.pending.is_none()).map(|capture| capture.chan));
		for voice in &state.voices {
			let busy = state.streams.iter().any(|stream| stream.front == Front::Voice(voice.chan) && stream.pending.is_some()) || state.captures.iter().any(|capture| capture.front == Front::Voice(voice.chan) && capture.pending.is_some());
			if !busy {
				waits.push(voice.chan);
			}
		}
		// THE DEADLINE: the next tick while a stream with no device holds frames, and Bluetooth's retry while it is wanted.
		let bt_wanted = !state.bt.refused && state.bt.events == 0 && !state.bt.resolving && state.bt.broker != 0;
		let mut deadline = if idle { clock().saturating_add(IDLE_TICKS) } else { 0 };
		if bt_wanted {
			deadline = if deadline == 0 { state.bt.retry_at } else { deadline.min(state.bt.retry_at) };
		}
		if bt_wanted && clock() >= state.bt.retry_at {
			state.bluetooth_retry();
			continue;
		}
		let ready: i64 = wait_any(&waits, deadline);
		if ready < 0 {
			continue;
		}
		let ready_chan: u64 = waits[ready as usize];

		// A DEVICE ANSWERED, or its channel closed.
		if let Some(index) = state.devices.iter().position(|device| device.chan == ready_chan) {
			if state.devices[index].pending == Pending::None {
				// An idle device has nothing to say: a message here is stray, and a close is the device going.
				match recv_blocking(ready_chan, &mut device_buf) {
					Received::Message { handle, .. } => {
						if handle != 0 {
							close(handle);
						}
					}
					Received::Closed => state.device_failed(index),
				}
				continue;
			}
			match recv_blocking(ready_chan, &mut device_buf) {
				Received::Message { len, handle } => {
					if handle != 0 {
						close(handle);
					}
					let reply = device_buf[..len].to_vec();
					state.device_reply(index, &reply);
				}
				Received::Closed => state.device_failed(index),
			}
			continue;
		}
		// A PUBLICATION OR A WITHDRAWAL, told apart by `live`.
		if providers != 0 && ready_chan == providers {
			let mut frame: [u8; 256] = [0; 256];
			match recv_blocking(providers, &mut frame) {
				Received::Message { len, handle } => {
					if handle != 0 {
						close(handle);
					}
					let mut frame_handles = wire::Handles::new();
					let Some(info) = provider_catalogue::subscribe_read(&frame[..len], &mut frame_handles) else {
						print(b"AudioService: a provider frame did not decode\n");
						continue;
					};
					if info.live {
						let opened = open_provider(catalogue, &info);
						if opened != 0 {
							state.provider_arrived(&info, opened);
						}
					} else {
						state.provider_withdrawn(info.slot, info.provider_generation);
					}
				}
				// THE SUBSCRIPTION ENDED: the devices already held stay, and no new ones are expected.
				Received::Closed => {
					close(providers);
					providers = 0;
				}
			}
			continue;
		}
		if state.bt.events != 0 && ready_chan == state.bt.events {
			state.bluetooth_events();
			continue;
		}
		if state.bt.resolving && ready_chan == state.bt.broker {
			state.bluetooth_answered();
			continue;
		}
		if ready_chan == admin {
			match recv_caps_blocking(admin, &mut request) {
				ReceivedCaps::Message { len, handles: caps } => {
					let mut reply_handle = proto::codec::Handles::new();
					let mut reply: [u8; 64] = [0; 64];
					let mut handle = caps;
					let mut call = AdminCall { clients: &mut clients };
					let len = audio_admin::dispatch(&mut call, &request[..len], &mut handle, &mut reply, &mut reply_handle);
					answer(admin, len.map(|len| &reply[..len]), &reply_handle);
					for &unclaimed in handle.as_slice() {
						close(unclaimed);
					}
				}
				ReceivedCaps::Closed => exit(),
			}
			continue;
		}
		if stats.contains(&ready_chan) {
			let state_ref = &state;
			serve_minted(&mut stats, ready_chan, &mut request, &mut |bytes, reply| {
				let mut call = StatsCall { audio: state_ref };
				let mut reply_handle = proto::codec::Handles::new();
				let mut request_handle = proto::codec::Handles::new();
				audio_stats::dispatch(&mut call, bytes, &mut request_handle, reply, &mut reply_handle)
			});
			continue;
		}
		if controls.contains(&ready_chan) {
			let state_ref = &mut state;
			serve_minted(&mut controls, ready_chan, &mut request, &mut |bytes, reply| {
				let mut call = ControlCall { audio: state_ref };
				let mut reply_handle = proto::codec::Handles::new();
				let mut request_handle = proto::codec::Handles::new();
				audio_control::dispatch(&mut call, bytes, &mut request_handle, reply, &mut reply_handle)
			});
			continue;
		}
		if let Some(index) = clients.iter().position(|client| client.chan == ready_chan) {
			let scope: Scope = clients[index].scope;
			match recv_caps_blocking(ready_chan, &mut request) {
				ReceivedCaps::Message { len, handles: caps } => {
					let mut handle = caps;
					let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
					if op == HEARTBEAT_OP {
						send_blocking(ready_chan, b"PONG", 0);
					} else if op == CONNECT_OP && scope == Scope::Full {
						if let Some((server, client)) = channel() {
							clients.push(Client { chan: server, scope });
							send_blocking(ready_chan, &[], client);
						}
					} else {
						let mut reply_handle = proto::codec::Handles::new();
						let mut reply: Vec<u8> = alloc::vec![0; 4096];
						let mut call = RootCall { audio: &mut state, scope };
						let len = audio::dispatch(&mut call, &request[..len], &mut handle, &mut reply, &mut reply_handle);
						answer(ready_chan, len.map(|len| &reply[..len]), &reply_handle);
					}
					for &unclaimed in handle.as_slice() {
						close(unclaimed);
					}
				}
				ReceivedCaps::Closed => {
					if index == 0 {
						exit();
					}
					close(ready_chan);
					clients.swap_remove(index);
				}
			}
			continue;
		}
		if let Some(at) = state.voices.iter().position(|voice| voice.chan == ready_chan) {
			take_voice(&mut state, at, &mut request);
			continue;
		}
		if let Some(index) = state.captures.iter().position(|capture| capture.chan == ready_chan && capture.front == Front::Own) {
			match recv_caps_blocking(ready_chan, &mut request) {
				ReceivedCaps::Message { len, handles } => {
					for &unclaimed in handles.as_slice() {
						close(unclaimed);
					}
					let bytes = request[..len].to_vec();
					state.take_capture_request(index, &bytes);
				}
				ReceivedCaps::Closed => state.remove_capture(index),
			}
			continue;
		}
		let Some(index) = state.streams.iter().position(|stream| stream.chan == ready_chan && stream.front == Front::Own) else { continue };
		match recv_caps_blocking(ready_chan, &mut request) {
			ReceivedCaps::Message { len, handles } => {
				let bytes = request[..len].to_vec();
				state.take_stream_request(index, &bytes, handles);
			}
			ReceivedCaps::Closed => {
				// AN EXPLICIT CLOSE DRAINS what was accepted; a dropped channel drops it.
				if state.streams[index].closing {
					close(ready_chan);
					state.streams[index].chan = 0;
				} else {
					state.remove_stream(index);
				}
			}
		}
	}
}

// ONE REQUEST ON A VOICE SESSION: its write to the session's stream, its read to the session's recorder - each deferred
// as its own kind is - the commands stream, or anything else answered now.
fn take_voice(state: &mut Audio, at: usize, request: &mut [u8; REQUEST_MAX]) {
	let chan = state.voices[at].chan;
	match recv_caps_blocking(chan, request) {
		ReceivedCaps::Message { len, handles } => {
			let bytes = request[..len].to_vec();
			let op: u16 = if len >= 2 { u16::from_le_bytes([bytes[0], bytes[1]]) } else { 0 };
			if op == voice_session::OP_WRITE
				&& let Some(index) = state.streams.iter().position(|stream| stream.front == Front::Voice(chan))
			{
				state.take_stream_request(index, &bytes, handles);
			} else if op == voice_session::OP_READ
				&& let Some(index) = state.captures.iter().position(|capture| capture.front == Front::Voice(chan))
			{
				for &unclaimed in handles.as_slice() {
					close(unclaimed);
				}
				state.take_capture_request(index, &bytes);
			} else if op == voice_session::OP_COMMANDS {
				let mut handles = handles;
				serve_commands(state, at, &bytes, &mut handles);
				for &unclaimed in handles.as_slice() {
					close(unclaimed);
				}
			} else {
				let index = state.streams.iter().position(|stream| stream.front == Front::Voice(chan));
				let latency = index.map_or(0, |index| state.stream_latency(&state.streams[index]));
				let mut reply: Vec<u8> = alloc::vec![0; 256];
				let mut reply_handle = proto::codec::Handles::new();
				let mut request_handle = handles;
				let mut call = VoiceCall { audio: state, voice: at, stream: index, latency };
				let len = voice_session::dispatch(&mut call, &bytes, &mut request_handle, &mut reply, &mut reply_handle);
				answer(chan, len.map(|len| &reply[..len]), &reply_handle);
				for &unclaimed in request_handle.as_slice() {
					close(unclaimed);
				}
			}
		}
		// A SESSION WHOSE HOLDER WENT AWAY ends: its link comes down with the last one.
		ReceivedCaps::Closed => state.remove_voice(chan),
	}
}

// REQUESTS ALREADY WAITING on streams and recorders that are not deferred, taken without blocking.
fn poll_clients(state: &mut Audio) {
	let mut request: [u8; REQUEST_MAX] = [0; REQUEST_MAX];
	let mut index: usize = 0;
	while index < state.streams.len() {
		let stream = &state.streams[index];
		if stream.front != Front::Own || stream.chan == 0 || stream.pending.is_some() {
			index += 1;
			continue;
		}
		let chan = stream.chan;
		match try_recv_caps(chan, &mut request) {
			PolledCaps::Message { len, handles } => {
				let bytes = request[..len].to_vec();
				state.take_stream_request(index, &bytes, handles);
				index += 1;
			}
			PolledCaps::Empty => index += 1,
			PolledCaps::Closed => {
				if state.streams[index].closing {
					close(chan);
					state.streams[index].chan = 0;
					index += 1;
				} else {
					state.remove_stream(index);
				}
			}
		}
	}
	index = 0;
	while index < state.captures.len() {
		let capture = &state.captures[index];
		if capture.front != Front::Own || capture.chan == 0 || capture.pending.is_some() {
			index += 1;
			continue;
		}
		let chan = capture.chan;
		match try_recv_caps(chan, &mut request) {
			PolledCaps::Message { len, handles } => {
				for &unclaimed in handles.as_slice() {
					close(unclaimed);
				}
				let bytes = request[..len].to_vec();
				state.take_capture_request(index, &bytes);
				index += 1;
			}
			PolledCaps::Empty => index += 1,
			PolledCaps::Closed => state.remove_capture(index),
		}
	}
}
