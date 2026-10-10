// Test boundaries only. bluetooth_audio_lifecycle.py inserts the exact production methods below.
// IPC is an in-memory bounded duplex queue; profile operations are traceable hooks. The wire stub
// keeps complete typed event values, so this is lifecycle evidence, not codec, radio or kernel IPC proof.
#![allow(dead_code)]
use bluetooth_audio::Service;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Error {
	Again,
	Closed,
	NotFound,
	Exhausted,
	Invalid,
	Unsupported,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AudioEndpointKind {
	Output,
	Input,
	Voice,
	Route,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BtCallState {
	None,
	Incoming,
	Outgoing,
	Active,
	Held,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BtCallCommand {
	Answer,
	HangUp,
	Reject,
	Redial,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct AudioEndpoint {
	id: u32,
	peer: [u8; 6],
	name: String,
	kind: AudioEndpointKind,
	rate: u32,
	channels: u8,
	latency_us: u32,
	hardware_volume: bool,
	volume: u8,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct EndpointVolume {
	id: u32,
	volume: u8,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum AudioEvent {
	Arrived(AudioEndpoint),
	Departed(u32),
	Volume(EndpointVolume),
	Command(BtCallCommand),
	MicrophoneVolume(EndpointVolume),
}
struct TimerPacer;

mod wire {
	#[derive(Default)]
	pub struct Handles(pub Vec<u64>);
	impl Handles {
		pub fn new() -> Self {
			Self::default()
		}
		pub fn as_slice(&self) -> &[u64] {
			&self.0
		}
	}
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SendOutcome {
	Delivered,
	Stalled,
	Closed,
}
enum PolledCaps {
	Closed,
	Empty,
	Message { len: usize, handles: wire::Handles },
}
struct Message {
	bytes: Vec<u8>,
	handles: Vec<u64>,
}
struct End {
	peer: u64,
	depth: usize,
	open: bool,
	queue: VecDeque<Message>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Trace {
	Sent(u64, SendOutcome),
	Closed(u64),
	Hook(&'static str, u32, Vec<u32>, u64, BtCallState),
}
#[derive(Default)]
struct Harness {
	next: u64,
	ends: BTreeMap<u64, End>,
	encoded: Vec<AudioEvent>,
	trace: Vec<Trace>,
}
thread_local! { static IPC: RefCell<Harness> = RefCell::new(Harness::default()); }
fn channel_with_depth(depth: usize) -> Option<(u64, u64)> {
	IPC.with_borrow_mut(|h| {
		let a = h.next + 1;
		let b = a + 1;
		h.next = b;
		h.ends.insert(a, End { peer: b, depth, open: true, queue: VecDeque::new() });
		h.ends.insert(b, End { peer: a, depth, open: true, queue: VecDeque::new() });
		Some((a, b))
	})
}
fn channel() -> Option<(u64, u64)> {
	channel_with_depth(16)
}
fn send(chan: u64, bytes: &[u8], handles: &[u64]) -> SendOutcome {
	IPC.with_borrow_mut(|h| {
		let end = h.ends.get(&chan).unwrap();
		let peer = end.peer;
		let outcome = if !end.open || !h.ends[&peer].open {
			SendOutcome::Closed
		} else if h.ends[&peer].queue.len() == h.ends[&peer].depth {
			SendOutcome::Stalled
		} else {
			h.ends.get_mut(&peer).unwrap().queue.push_back(Message { bytes: bytes.to_vec(), handles: handles.to_vec() });
			SendOutcome::Delivered
		};
		h.trace.push(Trace::Sent(chan, outcome));
		outcome
	})
}
fn try_send_outcome(chan: u64, bytes: &[u8], xfer: u64) -> SendOutcome {
	assert_eq!(xfer, 0);
	send(chan, bytes, &[])
}
fn send_caps_blocking(chan: u64, bytes: &[u8], handles: &[u64]) -> bool {
	send(chan, bytes, handles) == SendOutcome::Delivered
}
fn send_blocking(chan: u64, bytes: &[u8], xfer: u64) -> bool {
	try_send_outcome(chan, bytes, xfer) == SendOutcome::Delivered
}
fn close(chan: u64) {
	IPC.with_borrow_mut(|h| {
		let end = h.ends.get_mut(&chan).expect("closed unknown handle");
		assert!(end.open, "handle closed twice: {chan}");
		end.open = false;
		h.trace.push(Trace::Closed(chan));
	});
}
fn try_recv_caps(chan: u64, out: &mut [u8]) -> PolledCaps {
	IPC.with_borrow_mut(|h| {
		let end = h.ends.get_mut(&chan).unwrap();
		assert!(end.open, "read closed owned handle");
		let peer = end.peer;
		if let Some(message) = end.queue.pop_front() {
			assert!(message.bytes.len() <= out.len());
			out[..message.bytes.len()].copy_from_slice(&message.bytes);
			PolledCaps::Message { len: message.bytes.len(), handles: wire::Handles(message.handles) }
		} else if !h.ends[&peer].open {
			PolledCaps::Closed
		} else {
			PolledCaps::Empty
		}
	})
}
fn events(consumer: u64) -> Vec<AudioEvent> {
	let mut result = Vec::new();
	let mut bytes = [0; 256];
	loop {
		match try_recv_caps(consumer, &mut bytes) {
			PolledCaps::Message { len: 4, handles } => {
				assert!(handles.as_slice().is_empty());
				let id = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
				result.push(IPC.with_borrow(|h| h.encoded[id].clone()));
			}
			PolledCaps::Empty | PolledCaps::Closed => return result,
			_ => panic!("malformed stub event"),
		}
	}
}
mod bluetooth_audio {
	use super::*;
	pub trait Service {
		fn endpoints(&mut self) -> Result<Vec<AudioEvent>, Error>;
		fn open(&mut self, id: u32) -> Result<u64, Error>;
		fn set_volume(&mut self, id: u32, volume: u8) -> Result<(), Error>;
		fn microphone_volume(&mut self, id: u32) -> Result<u8, Error>;
		fn set_microphone_volume(&mut self, id: u32, volume: u8) -> Result<u8, Error>;
		fn set_call(&mut self, state: BtCallState) -> Result<(), Error>;
	}
	pub fn endpoints_frame(_: u32, event: &AudioEvent, out: &mut [u8], _: &mut wire::Handles) -> Option<usize> {
		let id = IPC.with_borrow_mut(|h| {
			let id = h.encoded.len() as u32;
			h.encoded.push(event.clone());
			id
		});
		out[..4].copy_from_slice(&id.to_le_bytes());
		Some(4)
	}
	pub fn endpoints_open(view: &mut impl Service, request: &[u8], _: &mut wire::Handles) -> Option<(u32, Result<Vec<AudioEvent>, Error>)> {
		assert_eq!(request, b"subscribe");
		Some((1, view.endpoints()))
	}
	pub fn endpoints_reply_ok(_: u32, out: &mut [u8]) -> Option<usize> {
		out[0] = 1;
		Some(1)
	}
	pub fn endpoints_reply_err(_: u32, error: &Error, out: &mut [u8]) -> Option<usize> {
		out[0] = 2;
		out[1] = *error as u8;
		Some(2)
	}
}

struct Stack {
	audio: AudioRoot,
	sources: BTreeMap<u32, Source>,
	microphones: BTreeMap<(usize, u16), u8>,
}
impl Stack {
	fn new() -> Self {
		Self { audio: AudioRoot::new(), sources: BTreeMap::new(), microphones: BTreeMap::new() }
	}
	fn hook(&self, name: &'static str, id: u32) {
		IPC.with_borrow_mut(|h| h.trace.push(Trace::Hook(name, id, self.audio.pcm.iter().map(|pcm| pcm.endpoint).collect(), self.audio.subscriber, self.audio.call)));
	}
	fn le_device_of(&self, id: u32) -> Option<usize> {
		(self.sources.get(&id) == Some(&Source::LeAudio)).then_some(0)
	}
	fn broadcast_owns(&self, id: u32) -> bool {
		self.sources.get(&id) == Some(&Source::Broadcast)
	}
	fn le_audio_open(&mut self, id: u32) {
		self.hook("le-open", id);
	}
	fn le_audio_close(&mut self, id: u32) {
		self.hook("le-close", id);
	}
	fn a2dp_stop(&mut self, _: usize, handle: u16) {
		self.hook("a2dp-stop", u32::from(handle));
	}
	fn voice_up(&mut self, _: usize, handle: u16) {
		self.hook("voice-up", u32::from(handle));
	}
	fn voice_down(&mut self, _: usize, handle: u16) {
		self.hook("voice-down", u32::from(handle));
	}
	fn voice_call(&mut self, _: BtCallState) {
		self.hook("voice-call", 0);
	}
	fn le_audio_volume(&mut self, _: u32, _: u8) -> Result<(), Error> {
		Ok(())
	}
	fn a2dp_set_volume(&mut self, _: usize, _: u16, _: u8) -> Result<(), Error> {
		Ok(())
	}
	fn voice_set_volume(&mut self, _: usize, _: u16, _: u8) -> Result<(), Error> {
		Ok(())
	}
	fn voice_microphone_volume(&mut self, at: usize, handle: u16) -> Result<u8, Error> {
		self.microphones.get(&(at, handle)).copied().ok_or(Error::NotFound)
	}
	fn voice_set_microphone_volume(&mut self, at: usize, handle: u16, volume: u8) -> Result<(), Error> {
		self.microphones.insert((at, handle), volume);
		Ok(())
	}
}

/* EXACT_PRODUCTION */

fn subscribe(stack: &mut Stack) -> Result<u64, Error> {
	let (server, client) = channel().unwrap();
	let mut reply = [0; 256];
	serve_endpoints(stack, server, b"subscribe", &mut wire::Handles::new(), &mut reply);
	let result = match try_recv_caps(client, &mut reply) {
		PolledCaps::Message { len: 1, handles } => {
			assert_eq!(reply[0], 1);
			assert_eq!(handles.as_slice().len(), 1);
			Ok(handles.as_slice()[0])
		}
		PolledCaps::Message { len: 2, handles } => {
			assert_eq!(reply, {
				let mut x = [0; 256];
				x[0] = 2;
				x[1] = Error::Again as u8;
				x
			});
			assert!(handles.as_slice().is_empty());
			Err(Error::Again)
		}
		_ => panic!("subscribe produced no result"),
	};
	close(server);
	close(client);
	result
}
fn offer(stack: &mut Stack, kind: AudioEndpointKind, source: Source, volume: u8) -> u32 {
	let handle = 100 + stack.audio.next_id as u16;
	let endpoint = AudioEndpoint { id: 0, peer: [handle as u8; 6], name: format!("endpoint-{handle}"), kind, rate: 16_000, channels: 1, latency_us: 40_000, hardware_volume: true, volume };
	let id = stack.audio.offer(endpoint, 0, handle).unwrap();
	stack.sources.insert(id, source);
	if kind == AudioEndpointKind::Voice && source == Source::Classic {
		stack.microphones.insert((0, handle), 27);
	}
	id
}
fn sends(producer: u64) -> Vec<SendOutcome> {
	IPC.with_borrow(|h| {
		h.trace
			.iter()
			.filter_map(|entry| match entry {
				Trace::Sent(id, outcome) if *id == producer => Some(*outcome),
				_ => None,
			})
			.collect()
	})
}
fn assert_retired(stack: &Stack, producer: u64, channels: &[u64], expected_hooks: &[(&'static str, u32)]) {
	assert_eq!(stack.audio.subscriber, 0);
	assert_eq!(stack.audio.call, BtCallState::None);
	assert!(stack.audio.pcm.is_empty());
	IPC.with_borrow(|h| {
		assert_eq!(h.trace.first(), Some(&Trace::Closed(producer)), "the retired stream closes before profile cleanup");
		assert_eq!(h.trace.get(1), Some(&Trace::Hook("voice-call", 0, channels.iter().enumerate().map(|(i, _)| i as u32 + 1).collect(), 0, BtCallState::None)));
		let mut offset = 2;
		for (index, channel) in channels.iter().enumerate() {
			assert_eq!(h.trace.get(offset), Some(&Trace::Closed(*channel)), "PCM closes before its profile hook");
			offset += 1;
			if let Some((name, id)) = expected_hooks.get(index).filter(|(name, _)| !name.is_empty()) {
				assert_eq!(h.trace.get(offset), Some(&Trace::Hook(name, *id, vec![], 0, BtCallState::None)), "all ownership must be gone before any profile close can restart LE music");
				offset += 1;
			}
		}
		assert_eq!(h.trace.len(), offset, "cleanup closes each held channel and profile exactly once");
	});
}
fn overflow_and_replacement() {
	IPC.with_borrow_mut(|h| *h = Harness::default());
	let mut stack = Stack::new();
	let output = offer(&mut stack, AudioEndpointKind::Output, Source::Classic, 38);
	let voice = offer(&mut stack, AudioEndpointKind::Voice, Source::Classic, 67);
	let le_music = offer(&mut stack, AudioEndpointKind::Output, Source::LeAudio, 44);
	let le_voice = offer(&mut stack, AudioEndpointKind::Voice, Source::LeAudio, 44);
	let broadcast = offer(&mut stack, AudioEndpointKind::Route, Source::Broadcast, 81);
	let consumer = subscribe(&mut stack).unwrap();
	let producer = stack.audio.subscriber;
	let initial_snapshot = sends(producer).len();
	assert_eq!(initial_snapshot, 6, "five offers plus the classic microphone state");
	assert_eq!(subscribe(&mut stack), Err(Error::Again), "one live subscriber owns the endpoints");
	for id in [output, voice, le_music, le_voice, broadcast] {
		AudioView { stack: &mut stack }.open(id).unwrap();
	}
	let channels: Vec<_> = stack.audio.pcm.iter().map(|pcm| pcm.chan).collect();
	stack.audio.pcm[0].ack_at = Some(999);
	stack.audio.pcm[1].ack_held = true;
	stack.audio.pcm[2].capture_waiting = true;
	assert!(stack.audio.pcm[..3].iter().all(Pcm::holding));
	AudioView { stack: &mut stack }.set_call(BtCallState::Active).unwrap();
	for _ in initial_snapshot..64 {
		stack.audio.volume_changed(voice, 73);
	}
	assert_eq!(sends(producer), vec![SendOutcome::Delivered; 64]);
	assert_eq!(stack.audio.call, BtCallState::Active, "64 deliveries do not retire a subscriber");
	let owner = stack.audio.owner(voice).unwrap();
	stack.microphones.insert(owner, 33);
	stack.audio.microphone_volume_changed(voice, 33); // Real send method reaches the bounded queue's 65th refusal.
	let attempted = sends(producer);
	assert_eq!(attempted.len(), 65);
	assert_eq!(attempted.last(), Some(&SendOutcome::Stalled));
	assert_eq!(stack.audio.call, BtCallState::None);
	assert_eq!(stack.audio.pcm.len(), 5, "send failure delegates profile cleanup to Stack");
	stack.audio.volume_changed(voice, 87);
	stack.microphones.insert(owner, 47);
	stack.audio.microphone_volume_changed(voice, 47);
	assert_eq!(sends(producer).len(), 65, "a failed stream must suppress later event sends");
	let offered = stack.audio.endpoints.clone();
	let owners = stack.audio.owners.clone();
	let next_id = stack.audio.next_id;
	IPC.with_borrow_mut(|h| h.trace.clear());
	assert_eq!(AudioView { stack: &mut stack }.open(output), Err(Error::Closed), "stale queued Arrived must not reopen PCM after retirement");
	assert_retired(&stack, producer, &channels, &[("a2dp-stop", 101), ("voice-down", 102), ("le-close", le_music), ("le-close", le_voice), ("", 0)]);
	assert_eq!(stack.audio.endpoints, offered);
	assert_eq!(stack.audio.owners, owners);
	assert_eq!(stack.audio.next_id, next_id);
	assert_eq!(stack.microphones[&owner], 47, "retirement cannot reset advertised/profile gain state");
	let stale = events(consumer);
	assert_eq!(stale.len(), 64);
	assert!(matches!(&stale[0], AudioEvent::Arrived(endpoint) if endpoint.id == output));
	assert!(matches!(try_recv_caps(consumer, &mut [0; 256]), PolledCaps::Closed));
	let before = IPC.with_borrow(|h| h.trace.clone());
	stack.serve_audio_subscriber(&mut [0; 256]);
	assert_eq!(IPC.with_borrow(|h| h.trace.clone()), before, "retirement is idempotent");
	let replacement = subscribe(&mut stack).unwrap();
	assert_ne!(stack.audio.subscriber, producer);
	let snapshot = events(replacement);
	let expected: Vec<_> = offered
		.iter()
		.flat_map(|endpoint| {
			let mut events = vec![AudioEvent::Arrived(endpoint.clone())];
			if endpoint.id == voice {
				events.push(AudioEvent::MicrophoneVolume(EndpointVolume { id: voice, volume: 47 }));
			}
			events
		})
		.collect();
	assert_eq!(snapshot, expected, "fresh snapshot preserves ids/format, latest speaker87 and mic47, including updates after overflow");
	assert_eq!(AudioView { stack: &mut stack }.microphone_volume(voice), Ok(47));
	for id in [output, voice, le_music, le_voice, broadcast] {
		assert!(AudioView { stack: &mut stack }.open(id).is_ok());
	}
	assert_eq!(AudioView { stack: &mut stack }.open(voice), Err(Error::Again));
	stack.audio.microphone_volume_changed(voice, 47);
	assert_eq!(events(replacement), vec![AudioEvent::MicrophoneVolume(EndpointVolume { id: voice, volume: 47 })]);
}
fn peer_close_and_unexpected_caps() {
	IPC.with_borrow_mut(|h| *h = Harness::default());
	let mut stack = Stack::new();
	let id = offer(&mut stack, AudioEndpointKind::Voice, Source::Classic, 53);
	let consumer = subscribe(&mut stack).unwrap();
	let producer = stack.audio.subscriber;
	events(consumer);
	AudioView { stack: &mut stack }.open(id).unwrap();
	stack.audio.pcm[0].capture_waiting = true;
	let pcm = stack.audio.pcm[0].chan;
	let (unexpected, _) = channel().unwrap();
	assert!(send_caps_blocking(consumer, b"unexpected", &[unexpected]));
	IPC.with_borrow_mut(|h| h.trace.clear());
	stack.serve_audio_subscriber(&mut [0; 256]);
	assert_eq!(stack.audio.subscriber, producer);
	assert_eq!(IPC.with_borrow(|h| h.trace.clone()), vec![Trace::Closed(unexpected)]);
	close(consumer);
	IPC.with_borrow_mut(|h| h.trace.clear());
	stack.serve_audio_subscriber(&mut [0; 256]);
	assert_retired(&stack, producer, &[pcm], &[("voice-down", 101)]);
	assert_eq!(stack.audio.endpoints.len(), 1);
	assert_eq!(AudioView { stack: &mut stack }.open(id), Err(Error::Closed));
	let new = subscribe(&mut stack).unwrap();
	assert_eq!(events(new).len(), 2);
	assert!(AudioView { stack: &mut stack }.open(id).is_ok());
}
fn main() {
	overflow_and_replacement();
	peer_close_and_unexpected_caps();
	println!("production Bluetooth audio lifecycle assertions passed; IPC/profile/wire boundaries are host stubs");
}
