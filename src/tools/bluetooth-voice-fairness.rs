// Exact-source insertions are made by bluetooth_voice_fairness.py. No guest/kernel IPC is run.
#![allow(dead_code, unused_variables, unused_mut)]
extern crate alloc;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
/* EXACT_DEPENDENCIES */
use service_logic::hci::{Kind, Limits, Session};
const PACKETS: u64 = 101;
const CONTROL: u64 = 102;
const TRANSPORT: u64 = 103;
const WATCHER: u64 = 104;
const CLIENT: u64 = 201;
const PCM: u64 = 301;
const SUBSCRIBER: u64 = 401;
const MAX_CONTROLLERS: usize = 4;
const ARRIVAL_NS: u64 = 8_563_400;
const WORKLOAD_NS: u64 = 8_609_400;
const HCI_BYTES: usize = 123;
const USB_PIECE: usize = 17;

#[derive(Clone)]
enum Message {
	Data(Vec<u8>, Vec<u64>),
	Closed,
}
#[derive(Default)]
struct Model {
	queues: BTreeMap<u64, VecDeque<Message>>,
	closed: Vec<u64>,
	replies: Vec<(u64, Vec<u8>)>,
	clock: u64,
	next_arrival: u64,
	generated: u8,
	pressure: bool,
	packet_reads: usize,
	previous_packet_len: Option<usize>,
	decoded_drops: usize,
	sco: Vec<u8>,
	event: Vec<u8>,
	acl: Vec<u8>,
	iso: usize,
	controls: usize,
	played: Vec<Vec<i16>>,
	timer_calls: usize,
	ended_epochs: Vec<u32>,
	voice_down: usize,
	voice_calls: usize,
	selected: Vec<u64>,
	waitsets: Vec<Vec<u64>>,
}
thread_local! { static MODEL: RefCell<Model> = RefCell::new(Model::default()); }
fn with<R>(f: impl FnOnce(&mut Model) -> R) -> R {
	MODEL.with(|m| f(&mut m.borrow_mut()))
}
fn reset() {
	with(|m| *m = Model::default());
}
fn queue(m: &mut Model, handle: u64, message: Message) {
	m.queues.entry(handle).or_default().push_back(message);
}
fn put(handle: u64, bytes: Vec<u8>, handles: Vec<u64>) {
	with(|m| queue(m, handle, Message::Data(bytes, handles)));
}
fn packet(kind: u8, epoch: u32, id: u8) -> Vec<u8> {
	let mut bytes = vec![kind];
	bytes.extend(epoch.to_le_bytes());
	let payload = match kind {
		1 => vec![0x0e, 1, id],
		2 => vec![1, 0, 1, 0, id],
		_ => {
			let mut v = vec![id; HCI_BYTES];
			v[..3].copy_from_slice(&[1, 0, 120]);
			v
		}
	};
	bytes.extend(payload);
	bytes
}
fn pcm_period() -> Vec<u8> {
	(0..80).flat_map(|i| (if i % 2 == 0 { 4000i16 } else { -4000i16 }).to_le_bytes()).collect()
}
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
enum PolledCaps {
	Message { len: usize, handles: wire::Handles },
	Empty,
	Closed,
}
fn try_recv_caps(handle: u64, buf: &mut [u8]) -> PolledCaps {
	with(|m| {
		// Observe the actual caller's reused transport buffer before any subsequent IPC overwrite.
		if let Some(len) = m.previous_packet_len.take() {
			assert!(buf[..len].iter().all(|&b| b == 0), "transport buffer was not scrubbed before reuse");
		}
		if handle == PACKETS {
			m.packet_reads += 1;
			assert!(!m.pressure || m.packet_reads <= 64, "VOICE_FAIRNESS_DRAIN_STARVATION: one drain never returns while SCO replies replenish its receive stream");
		}
		match m.queues.entry(handle).or_default().pop_front() {
			None => PolledCaps::Empty,
			Some(Message::Closed) => PolledCaps::Closed,
			Some(Message::Data(bytes, handles)) => {
				buf[..bytes.len()].copy_from_slice(&bytes);
				if handle == PACKETS {
					m.previous_packet_len = Some(bytes.len());
				}
				PolledCaps::Message { len: bytes.len(), handles: wire::Handles(handles) }
			}
		}
	})
}
fn close(handle: u64) {
	with(|m| m.closed.push(handle));
}
fn send_blocking(handle: u64, bytes: &[u8], cap: u64) {
	with(|m| m.replies.push((handle, bytes.to_vec())));
}
fn send_caps_blocking(handle: u64, bytes: &[u8], caps: &[u64]) -> bool {
	send_blocking(handle, bytes, 0);
	true
}
fn clock() -> u64 {
	with(|m| m.clock)
}
#[derive(Clone, Copy)]
enum HciPacketKind {
	Event,
	Acl,
	Iso,
	Sco,
	Command,
}
struct HciPacket {
	kind: HciPacketKind,
	epoch: u32,
	bytes: Vec<u8>,
}
impl Drop for HciPacket {
	fn drop(&mut self) {
		assert!(self.bytes.iter().all(|&b| b == 0), "decoded HCI bytes were not scrubbed before drop");
		with(|m| m.decoded_drops += 1);
	}
}
mod hci_transport {
	use super::*;
	// IPC decoder boundary: deliberately a small test encoding, not claimed as production wire.
	pub fn receive_read(bytes: &[u8], handles: &mut wire::Handles) -> Option<HciPacket> {
		if bytes.len() < 5 || bytes[0] == 255 {
			return None;
		}
		let kind = match bytes[0] {
			1 => HciPacketKind::Event,
			2 => HciPacketKind::Acl,
			3 => HciPacketKind::Sco,
			4 => HciPacketKind::Iso,
			_ => HciPacketKind::Command,
		};
		Some(HciPacket { kind, epoch: u32::from_le_bytes(bytes[1..5].try_into().unwrap()), bytes: bytes[5..].to_vec() })
	}
}
/* EXACT_SCRUB */

#[derive(Clone)]
struct ProviderInfo {
	slot: u32,
	provider_generation: u32,
}
struct Bredr {
	watcher: u64,
}
impl Bredr {
	fn detach_watcher(&mut self) {
		if self.watcher != 0 {
			close(core::mem::replace(&mut self.watcher, 0));
		}
	}
}
struct Controller {
	packets: u64,
	control: u64,
	transport: u64,
	bredr_state: Bredr,
	session: Session,
	limits: Limits,
	sco_bytes: u32,
	unclassified: u64,
	info: ProviderInfo,
	pumps: usize,
}
impl Controller {
	fn pump(&mut self) {
		self.pumps += 1;
	}
	fn link(&self, handle: u16) -> Option<()> {
		None
	}
	fn end_session(&mut self) {
		self.session.advance();
		with(|m| m.ended_epochs.push(self.session.epoch()));
	}
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum AudioEndpointKind {
	Output,
	Input,
	Route,
	Voice,
}
#[derive(Clone, Copy)]
enum BtCallState {
	None,
}
struct TimerPacer;
impl TimerPacer {
	fn take(&mut self, frames: u64) {}
}
mod a2dp {
	use super::*;
	pub fn pacer(rate: u32) -> TimerPacer {
		TimerPacer
	}
	pub fn ack_tick(p: &TimerPacer) -> u64 {
		clock()
	}
}
mod driver_protocol {
	pub mod audio {
		pub const CMD_FORMAT: u8 = 1;
		pub const CMD_CAPTURE: u8 = 2;
		pub const CMD_CAPTURE_STOP: u8 = 3;
		pub const OK: &[u8] = b"OK";
		#[derive(Clone, Copy)]
		pub struct PcmFormat {
			pub rate: u32,
			pub channels: u8,
		}
		pub struct DeviceFormat {
			pub output: Option<PcmFormat>,
			pub input: Option<PcmFormat>,
			pub period_bytes: u32,
			pub latency_us: u32,
			pub hardware_volume: bool,
			pub volume: u8,
		}
		impl DeviceFormat {
			pub fn encode(&self) -> [u8; 1] {
				[0]
			}
		}
	}
}
use driver_protocol::audio::{CMD_CAPTURE, CMD_CAPTURE_STOP, CMD_FORMAT, DeviceFormat, PcmFormat};
/* EXACT_DECLARATIONS */
struct AudioRoot {
	subscriber: u64,
	subscriber_failed: bool,
	call: BtCallState,
	pcm: Vec<Pcm>,
}
struct Grant {
	chan: u64,
	owner: u64,
}
struct Stack {
	controllers: Vec<Controller>,
	audio: AudioRoot,
	grants: Vec<Grant>,
	serials: Vec<Grant>,
	pushes: Vec<Grant>,
}
impl Stack {
	/* EXACT_METHODS */
	// Profile/transport boundaries below. Actual admission, dispatch and PCM conversion are inserted above.
	fn on_sco(&mut self, at: usize, bytes: &[u8]) {
		assert_eq!(bytes.len(), HCI_BYTES);
		with(|m| {
			m.sco.push(bytes[3]);
			if !m.pressure {
				return;
			}
			if m.sco.len() == 1 {
				// Control and PCM arrive during the first synchronous reply, after controller selection.
				queue(m, CLIENT, Message::Data(vec![0x42], vec![902]));
				queue(m, PCM, Message::Data(pcm_period(), vec![903]));
			}
			m.clock += WORKLOAD_NS;
			while m.next_arrival <= m.clock {
				let id = m.generated;
				m.generated += 1;
				queue(m, PACKETS, Message::Data(packet(3, 7, id), vec![]));
				m.next_arrival += ARRIVAL_NS;
			}
		});
	}
	fn on_event(&mut self, at: usize, bytes: &[u8]) {
		with(|m| m.event.push(*bytes.last().unwrap()));
	}
	fn on_acl(&mut self, at: usize, bytes: &[u8]) {
		with(|m| m.acl.push(*bytes.last().unwrap()));
	}
	fn on_iso(&mut self, at: usize, bytes: &[u8]) {
		with(|m| m.iso += 1);
	}
	fn iso_handle(&self, at: usize, handle: u16) -> bool {
		false
	}
	fn voice_play(&mut self, at: usize, handle: u16, samples: &[i16]) -> bool {
		assert_eq!((at, handle), (0, 1));
		with(|m| m.played.push(samples.to_vec()));
		true
	}
	fn voice_down(&mut self, at: usize, handle: u16) {
		with(|m| m.voice_down += 1);
	}
	fn voice_call(&mut self, state: BtCallState) {
		with(|m| m.voice_calls += 1);
	}
	fn le_device_of(&self, id: u32) -> Option<()> {
		None
	}
	fn broadcast_owns(&self, id: u32) -> bool {
		false
	}
	fn le_audio_close(&mut self, id: u32) {}
	fn a2dp_stop(&mut self, at: usize, handle: u16) {}
	fn take_period(&mut self, id: u32, at: usize, handle: u16, kind: AudioEndpointKind, samples: usize) -> Option<Vec<u8>> {
		None
	}
	fn le_audio_play(&mut self, id: u32, samples: &[i16]) {}
	fn a2dp_play(&mut self, at: usize, handle: u16, samples: &[i16]) {}
	fn pan_channels(&self) -> Vec<u64> {
		vec![]
	}
	fn receiver_streams(&self) -> std::vec::IntoIter<u64> {
		vec![].into_iter()
	}
	fn serial_deadline(&self) -> Option<u64> {
		None
	}
	fn opp_deadline(&self) -> Option<u64> {
		None
	}
	fn broadcast_deadline(&self) -> Option<u64> {
		None
	}
	fn le_audio_deadline(&self) -> Option<u64> {
		None
	}
	fn gatt_deadline(&self) -> Option<u64> {
		None
	}
	fn next_deadline(&self) -> u64 {
		clock() + 1_000_000
	}
	fn run_timers(&mut self) {
		with(|m| m.timer_calls += 1);
	}
	fn broadcast_timers(&mut self) {
		with(|m| m.timer_calls += 1);
	}
	fn le_audio_timers(&mut self) {
		with(|m| m.timer_calls += 1);
	}
	fn serial_timers(&mut self) {
		with(|m| m.timer_calls += 1);
	}
	fn gatt_timers(&mut self) {
		with(|m| m.timer_calls += 1);
	}
	fn opp_timers(&mut self) {
		with(|m| m.timer_calls += 1);
	}
	fn drain_control(&mut self, at: usize, buf: &mut [u8]) -> bool {
		!matches!(try_recv_caps(self.controllers[at].control, buf), PolledCaps::Closed)
	}
	fn drain_watcher(&mut self, at: usize, buf: &mut [u8]) {}
	fn serve_grant(&mut self, index: usize, buf: &mut [u8]) {
		panic!("unused grant boundary");
	}
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Interface {
	Read,
	Operator,
	Profile,
	Admin,
	Audio,
	Network,
}
struct Client {
	chan: u64,
	interface: Interface,
}
struct ReadView<'a> {
	stack: &'a mut Stack,
}
struct OperatorView<'a> {
	stack: &'a mut Stack,
}
struct ProfileView<'a> {
	stack: &'a mut Stack,
	target: Option<()>,
}
mod gatt {
	use super::*;
	pub struct AdminView<'a> {
		pub stack: &'a mut Stack,
	}
}
mod audio {
	use super::*;
	pub struct AudioView<'a> {
		pub stack: &'a mut Stack,
	}
}
mod pan {
	use super::*;
	pub struct NetworkView<'a> {
		pub stack: &'a mut Stack,
	}
}
// Protocol dispatch is the IPC boundary, reached only by the extracted real routing/recv/reply blocks.
macro_rules! protocol {
	($name:ident, $operator:expr) => {
		mod $name {
			use super::*;
			pub fn dispatch<T>(view: &mut T, request: &[u8], handles: &mut wire::Handles, reply: &mut [u8], output: &mut wire::Handles) -> Option<usize> {
				assert!($operator, "only operator protocol is exercised");
				assert_eq!(request, [0x42]);
				with(|m| m.controls += 1);
				reply[..2].copy_from_slice(b"OK");
				Some(2)
			}
		}
	};
}
protocol!(bluetooth, false);
protocol!(bluetooth_operator, true);
protocol!(bluetooth_profile, false);
protocol!(bluetooth_admin, false);
protocol!(bluetooth_audio, false);
protocol!(bluetooth_network, false);
mod proto {
	pub mod system {
		pub(crate) use crate::{bluetooth_admin, bluetooth_audio, bluetooth_network};
	}
}

fn object_ready(handle: &u64) -> bool {
	with(|m| m.queues.get(handle).is_some_and(|q| !q.is_empty()))
}
fn kernel_first_ready(objects: &[Option<u64>], n: usize) -> i64 {
	/* EXACT_FIRST_READY */
	-1
}
fn wait_any(handles: &[u64], deadline: u64) -> i64 {
	// Object construction/readiness/blocking is mocked; first-ready selection is literal kernel source.
	let objects: Vec<Option<u64>> = handles.iter().copied().map(Some).collect();
	let selected = kernel_first_ready(&objects, objects.len());
	with(|m| {
		m.waitsets.push(handles.to_vec());
		if selected >= 0 {
			m.selected.push(handles[selected as usize]);
		} else {
			m.clock = m.clock.max(deadline);
		}
	});
	selected
}
fn turns(mut stack: &mut Stack, count: usize) {
	let (read_root, operator_root, profile_root, admin_root, audio_root, network_root) = (0, 0, 0, 0, 0, 0);
	let subscribed = false;
	let subscription = 0;
	let mut clients = vec![Client { chan: CLIENT, interface: Interface::Operator }];
	let mut buf = vec![0u8; 8192];
	let mut reply = vec![0u8; 8192];
	for _ in 0..count {
		stack.serve_audio_subscriber(&mut buf);
		/* EXACT_WAITSET */
		/* EXACT_DISPATCH */
		// Catalogue/other profile branches are unreachable here (no such roots/grants/streams).
		/* EXACT_CLIENT */
		// The one-byte request cannot enter the original >=2-byte special-op branches.
		/* EXACT_REPLY */
	}
	// A final yielded packet has no next recv to observe its transport buffer; check it here.
	with(|m| {
		if let Some(len) = m.previous_packet_len.take() {
			assert!(buf[..len].iter().all(|&b| b == 0), "transport buffer dirty at turn boundary");
		}
	});
}
fn new_stack() -> Stack {
	Stack { controllers: vec![Controller { packets: PACKETS, control: CONTROL, transport: TRANSPORT, bredr_state: Bredr { watcher: WATCHER }, session: Session::new(7), limits: Limits::of(false, 258, 257, 1028, 0), sco_bytes: 123, unclassified: 0, info: ProviderInfo { slot: 1, provider_generation: 2 }, pumps: 0 }], audio: AudioRoot { subscriber: 0, subscriber_failed: false, call: BtCallState::None, pcm: vec![Pcm { chan: PCM, endpoint: 1, at: 0, handle: 1, kind: AudioEndpointKind::Voice, rate: 8000, channels: 1, latency_us: 10000, hardware_volume: false, volume: 100, pacer: None, ack_at: None, ack_held: false, capture_waiting: false }] }, grants: vec![], serials: vec![], pushes: vec![] }
}
fn fairness() {
	reset();
	assert_eq!(HCI_BYTES.div_ceil(USB_PIECE), 8);
	assert!(WORKLOAD_NS >= HCI_BYTES.div_ceil(USB_PIECE) as u64 * 1_000_000);
	assert!(WORKLOAD_NS >= ARRIVAL_NS); // This modeled cadence sustains at least one pending SCO per reply.
	let mut stack = new_stack();
	with(|m| {
		m.pressure = true;
		m.next_arrival = ARRIVAL_NS;
		m.generated = 1;
		queue(m, PACKETS, Message::Data(packet(3, 7, 0), vec![901]));
		assert!(!m.queues.contains_key(&CLIENT) && !m.queues.contains_key(&PCM));
	});
	turns(&mut stack, 12);
	with(|m| {
		assert_eq!((m.controls, m.played.len()), (1, 1), "VOICE_FAIRNESS_WAITSET_STARVATION: controller wins first-ready turns while control/PCM remain ready");
		assert_eq!(&m.selected[..3], &[PACKETS, CLIENT, PCM]);
		assert_eq!(m.played[0], pcm_period().chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect::<Vec<_>>());
		assert!(m.played[0].iter().all(|&s| s != 0));
		assert!(m.replies.contains(&(CLIENT, b"OK".to_vec())) && m.replies.contains(&(PCM, b"OK".to_vec())));
		assert!(m.closed.contains(&901) && m.closed.contains(&902) && m.closed.contains(&903));
		assert_eq!(m.timer_calls, 12 * 6);
		assert_eq!(m.sco, (0..m.sco.len() as u8).collect::<Vec<_>>());
		assert_eq!(m.sco.len() + m.queues[&PACKETS].len(), usize::from(m.generated));
		assert_eq!(m.decoded_drops, m.sco.len());
	});
}
fn invariants() {
	// One SCO per turn retains later packets, closes all unexpected caps and scrubs both copies.
	reset();
	let mut stack = new_stack();
	let mut buf = vec![0u8; 8192];
	for id in 0..3 {
		put(PACKETS, packet(3, 7, id), vec![800 + u64::from(id)]);
	}
	for count in 1..=3 {
		assert!(stack.drain_packets(0, &mut buf));
		with(|m| {
			assert_eq!(m.sco.len(), count);
			assert_eq!(m.queues[&PACKETS].len(), 3 - count);
		});
		assert!(buf.iter().all(|&b| b == 0));
	}
	assert!(stack.drain_packets(0, &mut buf));
	with(|m| {
		assert_eq!(m.sco, [0, 1, 2]);
		assert_eq!(m.closed, [800, 801, 802]);
		assert_eq!(m.decoded_drops, 3);
	});

	// Malformed decode, stale epoch and oversized packets stay refused; non-SCO drains to Empty.
	reset();
	let mut stack = new_stack();
	put(PACKETS, vec![255], vec![810]);
	put(PACKETS, packet(1, 6, 9), vec![811]);
	put(PACKETS, packet(1, 7, 10), vec![812]);
	put(PACKETS, packet(2, 7, 11), vec![813]);
	assert!(stack.drain_packets(0, &mut buf));
	with(|m| {
		assert_eq!(m.event, [10]);
		assert_eq!(m.acl, [11]);
		assert_eq!(m.closed, [810, 811, 812, 813]);
		assert_eq!(m.decoded_drops, 3);
		assert!(m.queues[&PACKETS].is_empty());
	});
	assert!(buf.iter().all(|&b| b == 0));
	put(PACKETS, packet(3, 6, 12), vec![814]);
	assert!(stack.drain_packets(0, &mut buf));
	let mut oversized = packet(3, 7, 13);
	oversized.push(99);
	put(PACKETS, oversized, vec![815]);
	assert!(stack.drain_packets(0, &mut buf));
	with(|m| {
		assert!(m.sco.is_empty());
		assert_eq!(m.closed, [810, 811, 812, 813, 814, 815]);
	});
	assert!(buf.iter().all(|&b| b == 0));
	// Resolve the observation of the last scrub before using another buffer.
	assert!(stack.drain_packets(0, &mut buf));

	// Actual main closure path removes the controller and invokes session/end/handle retirement.
	reset();
	let mut stack = new_stack();
	with(|m| queue(m, PACKETS, Message::Closed));
	turns(&mut stack, 1);
	assert!(stack.controllers.is_empty());
	with(|m| {
		assert_eq!(m.ended_epochs, [8]);
		assert_eq!(m.closed, [WATCHER, PACKETS, CONTROL, TRANSPORT]);
	});
	reset();
	let mut stack = new_stack();
	with(|m| queue(m, CONTROL, Message::Closed));
	turns(&mut stack, 1);
	assert!(stack.controllers.is_empty());

	// A held PCM request is omitted until the extracted timer acknowledgment releases it.
	reset();
	let mut stack = new_stack();
	stack.audio.pcm[0].ack_at = Some(1_000_000);
	put(PCM, pcm_period(), vec![]);
	turns(&mut stack, 2);
	with(|m| {
		assert!(!m.waitsets[0].contains(&PCM) && m.waitsets[1].contains(&PCM));
		assert_eq!(m.played.len(), 1);
		assert_eq!(m.replies.iter().filter(|(h, _)| *h == PCM).count(), 2);
		assert_eq!(m.timer_calls, 12);
	});
	reset();
	let mut stack = new_stack();
	stack.audio.pcm[0].ack_held = true;
	put(PCM, pcm_period(), vec![]);
	turns(&mut stack, 1);
	with(|m| {
		assert!(!m.waitsets[0].contains(&PCM));
		assert!(m.played.is_empty());
	});
	stack.audio.pcm[0].ack_held = false;
	stack.audio.pcm[0].capture_waiting = true;
	turns(&mut stack, 1);
	with(|m| assert!(!m.waitsets[1].contains(&PCM)));

	// Subscriber loss retires even held PCM before waiting, with actual ownership cleanup methods.
	reset();
	let mut stack = new_stack();
	stack.audio.subscriber = SUBSCRIBER;
	stack.audio.pcm[0].ack_held = true;
	with(|m| queue(m, SUBSCRIBER, Message::Closed));
	turns(&mut stack, 1);
	assert!(stack.audio.pcm.is_empty());
	assert_eq!(stack.audio.subscriber, 0);
	with(|m| {
		assert_eq!(m.closed, [SUBSCRIBER, PCM]);
		assert_eq!(m.voice_down, 1);
		assert_eq!(m.voice_calls, 1);
	});
	reset();
	let mut stack = new_stack();
	with(|m| queue(m, PCM, Message::Closed));
	turns(&mut stack, 1);
	assert!(stack.audio.pcm.is_empty());
	with(|m| assert_eq!(m.closed, [PCM]));
}
fn main() {
	if std::env::args().nth(1).as_deref() != Some("fairness") {
		invariants();
	}
	fairness();
	println!("VOICE_FAIRNESS_COMPLETE");
}
