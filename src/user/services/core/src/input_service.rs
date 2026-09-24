// InputService - capability-scoped pointer and raw-key input.
//
// ServiceManager starts this program from the init package and hands it a bootstrap
// channel, then over it a "SERVE" channel (the one clients reach it on), an "INPUT"
// and an "INPUT2" channel (the raw pointer-event streams the virtio_input pointer
// driver and the xhci driver feed it; a handle is 0 when that pointer source is
// absent this boot), a merged raw-key "KEYS" channel, private "FOCUS" / "KILL"
// channels to DisplayService, and a "FORWARD" channel to ConsoleService.
// The drivers send normalized [x u16][y u16][buttons u8][wheel i8] events; InputService
// maps each to the text-cell grid and keeps a bounded ring of the recent ones for the
// typed `subscribe` API, and forwards the raw bytes to ConsoleService. Keyboard drivers
// send canonical HID usage transitions in parallel with the cooked console path;
// `subscribe-keys` accepts only a one-shot proof minted for the active display surface,
// streams transitions live without allowing client backpressure to stall input, and
// synchronously suppresses the cooked console while the graphical surface has focus.
//
// When the supervisor that started it drops the bootstrap channel (no clients this
// boot), the service exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use keys::KeyState;
use proto::codec::Handles;
use proto::system::input;
use proto::system::input_admin::{self, Service as AdminService};
use proto::system::input_trusted;
use proto::system::{ContactEvent, Error, KeyEvent, PointerEvent, ProviderKind, TrustedInput, TrustedInputKind, provider_catalogue};
use rt::*;
use service_logic::trusted_keys::{Arming, Seen, Watch};
use services::capability_names::*;

// The default text-cell grid the normalized pointer position maps onto: the boot
// framebuffer (1280x800) with 16x16 cells. A resize-aware grid (consulting
// ConsoleService) is a later refinement; this is the plumbing.
const COLS: u32 = 80;
const ROWS: u32 = 50;
// The pointer driver scales each axis into 0..=NORM_MAX; one past that is the span we
// divide the grid across.
const NORM_SPAN: u32 = 0x1_0000;
// The bounded ring of recent mapped pointer events a `subscribe` snapshot returns.
const RING_CAP: usize = 32;
// A raw pointer event from the driver: [x u16 LE][y u16 LE][buttons u8][wheel i8]. The
// first five bytes are the minimum to map a cell position; the wheel byte is forwarded
// to ConsoleService but not part of the typed snapshot.
const RAW_LEN: usize = 5;
// A raw contact from a touch driver: [id u8][tip u8][x u16 LE][y u16 LE]. ONE CONTACT PER MESSAGE
// rather than a batch, because a batch needs a count and a count is a second thing that can disagree
// with the bytes beside it - and the stream this service publishes is per contact anyway.
const RAW_CONTACT_LEN: usize = 6;
// How long the key-focus notice waits for ConsoleService to take it. Ten ticks is a tenth of a
// second: far longer than a console that is running needs, and short enough that a console which is
// not cannot hold this service - which DisplayService is synchronously waiting on. See
// `notify_console`.
const CONSOLE_NOTICE_TICKS: u64 = 10;

// The recent pointer events, mapped to the text-cell grid - the bounded source a
// `subscribe` stream snapshots.
struct Input {
	recent: Vec<PointerEvent>,
	keys: KeyState,
	focus_peer: u64,
	kill_control: u64,
	key_stream: Option<KeyStream>,
	/// The live contact stream, and which contacts are still down on it - so focus loss can release
	/// a finger the way it releases a held key, rather than leaving the next foreground application
	/// to inherit one.
	contact_stream: Option<ContactStream>,
	contacts_down: Vec<u8>,
	proof_nonce: u64,
	/// The secure-attention chord is down on the ordinary path: its keys go nowhere there.
	chord_held: bool,
	/// A PROTECTED SESSION IS UP: nothing on the ordinary path is delivered anywhere - no key, no contact, no
	/// pointer event - and the cooked console is suppressed, until AdminService ends it.
	protected: bool,
}

// THE PROTECTED INPUT PATH: the trusted keyboard's own sink, what it is holding, the session AdminService
// armed, and the one stream AdminService reads.
struct Trusted {
	watch: Watch,
	arming: Arming,
	producer: u64,
	seq: u32,
	/// Secure attention was noticed and AdminService has not ended the session it opened.
	protected: bool,
}

impl Trusted {
	fn emit(&mut self, kind: TrustedInputKind, epoch: u64, usage: u16, down: bool) {
		if self.producer == 0 {
			return;
		}
		let event = TrustedInput { kind, epoch, usage, down };
		let mut frame: [u8; 48] = [0; 48];
		let mut handles = Handles::new();
		let sent = match input_trusted::events_frame(self.seq, &event, &mut frame, &mut handles) {
			Some(len) => try_send(self.producer, &frame[..len], 0),
			None => false,
		};
		if sent {
			self.seq = self.seq.wrapping_add(1);
		} else {
			// A READER THAT STOPPED READING has lost the path; the next `events` opens a fresh one.
			self.reader_lost();
		}
	}

	// AdminService's stream is gone: whatever session it held ends here, and the ordinary path comes back.
	fn reader_lost(&mut self) {
		if self.producer != 0 {
			close(self.producer);
			self.producer = 0;
		}
		self.arming = Arming::Off;
		self.protected = false;
	}

	// One transition from the trusted keyboard: secure attention, a key for an armed session, and the arming
	// that follows the last key's release.
	fn record(&mut self, raw: &[u8]) {
		if raw.len() != 3 || raw[2] > 1 {
			return;
		}
		let usage = u16::from_le_bytes([raw[0], raw[1]]);
		let down = raw[2] != 0;
		match self.watch.record(usage, down) {
			// ONLY WITH A READER, and only once per session: the chord again while one is up changes nothing.
			Seen::Attention => {
				if self.producer != 0 && !self.protected {
					self.protected = true;
					self.emit(TrustedInputKind::Attention, 0, usage, true);
				}
			}
			Seen::Key { usage, down } => {
				if let Some(epoch) = self.arming.armed() {
					self.emit(TrustedInputKind::Key, epoch, usage, down);
				}
			}
			Seen::Nothing => {}
		}
		if let Some(epoch) = self.arming.settle(&self.watch) {
			self.emit(TrustedInputKind::Armed, epoch, 0, false);
		}
	}
}

impl input_trusted::Service for Trusted {
	fn events(&mut self) -> Vec<TrustedInput> {
		Vec::new()
	}
	// ARMED ONLY ONCE NOTHING IS HELD: the chord that opened the session is still down now.
	fn arm(&mut self, epoch: u64) -> Result<(), Error> {
		self.arming = Arming::arm(epoch, &self.watch);
		if let Some(epoch) = self.arming.armed() {
			self.emit(TrustedInputKind::Armed, epoch, 0, false);
		}
		Ok(())
	}
	// THE SESSION ENDS: the ordinary path comes back. A disarm naming another session's epoch ends nothing.
	fn disarm(&mut self, epoch: u64) -> Result<(), Error> {
		if matches!(self.arming, Arming::Waiting(held) | Arming::Armed(held) if held != epoch) {
			return Err(Error::Stale);
		}
		self.arming = Arming::Off;
		self.protected = false;
		Ok(())
	}
}

struct KeyStream {
	owner: u64,
	producer: u64,
	seq: u32,
}

struct ContactStream {
	owner: u64,
	producer: u64,
	seq: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
	Full,
	Keys,
}

struct Client {
	chan: u64,
	scope: Scope,
}

struct AdminCall<'a> {
	clients: &'a mut Vec<Client>,
}

impl AdminService for AdminCall<'_> {
	fn open_keys(&mut self) -> Result<u64, Error> {
		let (server, client): (u64, u64) = channel().ok_or(Error::Again)?;
		self.clients.push(Client { chan: server, scope: Scope::Keys });
		Ok(client)
	}
}

impl Input {
	fn new(kill_control: u64) -> Input {
		Input { recent: Vec::new(), keys: KeyState::new(), focus_peer: 0, kill_control, key_stream: None, contact_stream: None, contacts_down: Vec::new(), proof_nonce: 0, chord_held: false, protected: false }
	}

	// SECURE ATTENTION WAS NOTICED: application key and contact focus is revoked - held keys and contacts
	// released to their owners first - queued pointer events are discarded, and the cooked console is told
	// it does not hold the keyboard. From here nothing on the ordinary path is delivered until the session
	// AdminService opened ends.
	fn protect(&mut self, forward: u64) {
		if self.protected {
			return;
		}
		self.protected = true;
		self.set_focus(0);
		self.recent.clear();
		self.notify_console(false, forward);
	}

	// Record one mapped event, dropping the oldest once the ring is full.
	fn record(&mut self, event: PointerEvent) {
		if self.protected {
			return;
		}
		self.recent.push(event);
		if self.recent.len() > RING_CAP {
			self.recent.remove(0);
		}
	}

	fn record_key(&mut self, raw: &[u8]) {
		let Some(event) = self.keys.record_raw(raw) else { return };
		// HELD STATE IS STILL TRACKED, so nothing is stuck when the session ends; nothing is delivered.
		if self.protected {
			return;
		}
		// SECURE ATTENTION IS RESERVED BEFORE ORDINARY DELIVERY: F12 pressed under Ctrl and Alt, and its
		// release, reach no application. The chord itself is acted on only from the trusted keyboard.
		const F12: u16 = service_logic::trusted_keys::F12;
		if event.code == F12 {
			if event.pressed && self.keys.is_held(keys::usage::LEFT_CTRL) | self.keys.is_held(keys::usage::RIGHT_CTRL) && self.keys.is_held(keys::usage::LEFT_ALT) | self.keys.is_held(keys::usage::RIGHT_ALT) {
				self.chord_held = true;
				return;
			}
			if !event.pressed && core::mem::take(&mut self.chord_held) {
				return;
			}
		}
		let emergency = self.keys.emergency_chord(&event);
		self.send_key(event);
		if emergency {
			if self.kill_control != 0 {
				let _ = send_blocking(self.kill_control, b"KILL", 0);
			}
			self.set_focus(0);
		}
	}

	fn send_key(&mut self, event: KeyEvent) -> bool {
		let Some(stream) = self.key_stream.as_mut() else { return false };
		let mut frame: [u8; 32] = [0; 32];
		let mut frame_handles = Handles::new();
		let sent: bool = match input::subscribe_keys_frame(stream.seq, &event, &mut frame, &mut frame_handles) {
			Some(len) => try_send_caps(stream.producer, &frame[..len], frame_handles.as_slice()),
			None => false,
		};
		if sent {
			stream.seq = stream.seq.wrapping_add(1);
			true
		} else {
			for handle in frame_handles.as_slice() {
				close(*handle);
			}
			let dead: KeyStream = self.key_stream.take().unwrap();
			close(dead.producer);
			false
		}
	}

	fn close_key_stream(&mut self, release_held: bool) {
		if release_held {
			for event in self.keys.synthetic_releases() {
				if !self.send_key(event) {
					break;
				}
			}
		}
		if let Some(stream) = self.key_stream.take() {
			close(stream.producer);
		}
	}

	// Deliver one contact to the live stream, answering whether it went.
	//
	// THE SAME NON-BLOCKING RULE THE KEY STREAM KEEPS: a consumer that stopped reading must not be
	// able to stall this service, which DisplayService is synchronously waiting on for every focus
	// change. A stream that will not take a contact is a stream whose consumer is gone.
	fn send_contact(&mut self, event: ContactEvent) -> bool {
		let Some(stream) = self.contact_stream.as_mut() else { return false };
		let mut frame: [u8; 32] = [0; 32];
		let mut frame_handles = Handles::new();
		let sent: bool = match input::subscribe_contacts_frame(stream.seq, &event, &mut frame, &mut frame_handles) {
			Some(len) => try_send_caps(stream.producer, &frame[..len], frame_handles.as_slice()),
			None => false,
		};
		if sent {
			stream.seq = stream.seq.wrapping_add(1);
			true
		} else {
			for handle in frame_handles.as_slice() {
				close(*handle);
			}
			let dead: ContactStream = self.contact_stream.take().unwrap();
			close(dead.producer);
			false
		}
	}

	// Record one contact and pass it on, keeping track of which are still down.
	//
	// A CONTACT THAT LIFTS IS REPORTED AND THEN FORGOTTEN, and one that never lifts is what the
	// release below exists for: a finger is held state exactly as a key is, and a foreground
	// application that inherits one is a program that starts with a touch it never saw begin.
	fn record_contact(&mut self, event: ContactEvent) {
		if self.protected {
			return;
		}
		if event.tip {
			if !self.contacts_down.contains(&event.id) {
				self.contacts_down.push(event.id);
			}
		} else if let Some(at) = self.contacts_down.iter().position(|id| *id == event.id) {
			self.contacts_down.swap_remove(at);
		}
		self.send_contact(event);
	}

	fn close_contact_stream(&mut self, release_held: bool) {
		if release_held {
			// EVERY FINGER STILL DOWN IS LIFTED BEFORE THE STREAM GOES, which is the contact half of
			// what `close_key_stream` does for keys.
			for id in core::mem::take(&mut self.contacts_down) {
				if !self.send_contact(ContactEvent { id, tip: false, x: 0, y: 0 }) {
					break;
				}
			}
		}
		self.contacts_down.clear();
		if let Some(stream) = self.contact_stream.take() {
			close(stream.producer);
		}
	}

	fn set_focus(&mut self, peer: u64) {
		self.close_contact_stream(true);
		self.close_key_stream(true);
		if self.focus_peer != 0 {
			close(self.focus_peer);
		}
		self.focus_peer = peer;
	}

	// Tell ConsoleService whether the graphical surface holds the keyboard.
	//
	// BOUNDED, AND THIS SERVICE IS THE ONE THAT MUST NEVER PARK.
	//
	// It used to be `send_blocking`, on a channel to ConsoleService - and this is called from inside
	// the FOCUS handler, which DisplayService is sitting in `recv_blocking` waiting for the `OK` of.
	// So the chain was: DisplayService waits for InputService, InputService waits for room in
	// ConsoleService's queue, and ConsoleService is in the middle of a present that waits for
	// DisplayService. Three services, each holding what the next one needs, and nothing in any of
	// them wrong on its own.
	//
	// It surfaces at exactly one moment: the instant a full-screen program gives the display back.
	// That is when the focus hand-back, the console's repaint and the surface teardown all happen at
	// once - and what a person sees is a shell that printed its prompt and then answered nothing for
	// seconds, on a machine where every service is running.
	//
	// A DEADLINE RATHER THAN A DROP, because the message decides whether the console feeds keystrokes
	// to the shell while a surface owns them, and a dropped one leaves it wrong until the next focus
	// change. The bound is short - the console is one wake away from draining this whenever it is not
	// itself blocked - and the loss is reported rather than silent.
	fn notify_console(&self, focused: bool, forward: u64) {
		if forward == 0 {
			return;
		}
		let mut message: [u8; 9] = [0; 9];
		message[..8].copy_from_slice(b"KEYFOCUS");
		message[8] = focused as u8;
		if let SendOutcome::Stalled = send_deadline(forward, &message, 0, clock() + CONSOLE_NOTICE_TICKS) {
			print(b"InputService: ConsoleService did not take the key-focus notice inside its deadline; the console keeps whatever focus it last heard\n");
		}
	}

	fn validate_focus(&mut self, proof: u64) -> bool {
		if proof == 0 || self.focus_peer == 0 {
			return false;
		}
		self.proof_nonce = self.proof_nonce.wrapping_add(1);
		let challenge: [u8; 8] = self.proof_nonce.to_le_bytes();
		if !try_send(proof, &challenge, 0) {
			return false;
		}
		let mut received: [u8; 8] = [0; 8];
		matches!(try_recv(self.focus_peer, &mut received), Polled::Message { len: 8, handle: 0 } if received == challenge)
	}
}

// The generated Input service contract: `subscribe` returns the recent pointer
// events, which the serve loop streams frame by frame over a fresh sub-channel.
impl input::Service for Input {
	fn subscribe(&mut self) -> Vec<PointerEvent> {
		self.recent.clone()
	}

	fn subscribe_keys(&mut self, focus: u64) -> Vec<KeyEvent> {
		if focus != 0 {
			close(focus);
		}
		Vec::new()
	}

	// THE SAME STUB AS `subscribe_keys` AND FOR THE SAME REASON. The generated contract answers with
	// a snapshot; these two are LIVE streams, served by `stream_subscribe_keys` and
	// `stream_subscribe_contacts` off the serve loop, which is where the focus proof is spent. A
	// caller reaching this arm handed a proof that is closed here rather than kept.
	fn subscribe_contacts(&mut self, focus: u64) -> Vec<ContactEvent> {
		if focus != 0 {
			close(focus);
		}
		Vec::new()
	}
}

// Map a raw contact from a touch driver, or None if it is not one.
//
// THE AXES PASS THROUGH UNSCALED, which is the difference from the pointer path above and is the
// whole reason a touch surface is not published as a pointer: the driver already normalised them
// across the surface, and this service's job is to carry that rather than to flatten it onto a
// text-cell grid a finger moves within.
fn contact_from(raw: &[u8]) -> Option<ContactEvent> {
	if raw.len() < RAW_CONTACT_LEN {
		return None;
	}
	Some(ContactEvent { id: raw[0], tip: raw[1] != 0, x: u16::from_le_bytes([raw[2], raw[3]]), y: u16::from_le_bytes([raw[4], raw[5]]) })
}

// Map a raw normalized pointer event to a text-cell PointerEvent, or None if it is
// too short. The x/y axes (0..=NORM_MAX) scale onto the COLS x ROWS grid.
// THE FIRST LIVE PROVIDER OF ONE KIND THE SUBSCRIPTION HAS ALREADY QUEUED, connected to.
//
// POLLED, NEVER BLOCKED: the catalogue registers a subscriber and sends it everything published in
// one step, so what is here is here, and waiting for more would hang the boot of every machine that
// has no pointing device of that kind. Zero for a boot that granted no catalogue connection, for a
// catalogue that refuses the subscription, and for a machine with nothing of that kind published -
// all three of which are the state a zero handle used to be, and none of which is a failure.
//
// The subscription is CLOSED before returning. This service takes what exists at bootstrap and does
// not follow a replacement, so an open stream would be a handle nothing reads and a subscriber slot
// the catalogue could not give to a consumer that does.
fn take_published_pointer(catalogue: u64, kind: &ProviderKind) -> u64 {
	if catalogue == 0 {
		return 0;
	}
	let Some(providers) = provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(kind) else {
		print(b"InputService: the catalogue refused a pointer subscription\n");
		return 0;
	};
	let mut buf: [u8; 256] = [0; 256];
	let mut opened: u64 = 0;
	loop {
		let PolledCaps::Message { len, handles } = try_recv_caps(providers, &mut buf) else { break };
		for &handle in handles.as_slice() {
			close(handle);
		}
		let mut frame_handles = wire::Handles::new();
		let Some(info) = provider_catalogue::subscribe_read(&buf[..len], &mut frame_handles) else {
			print(b"InputService: a provider frame did not decode\n");
			continue;
		};
		if !info.live {
			continue;
		}
		match provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(&info) {
			Some(Ok(handle)) => {
				opened = handle;
				break;
			}
			Some(Err(_)) => print(b"InputService: the catalogue refused a connection to a pointer provider it published\n"),
			None => print(b"InputService: the catalogue did not answer the connection it published\n"),
		}
	}
	close(providers);
	opened
}

fn map_event(raw: &[u8]) -> Option<PointerEvent> {
	if raw.len() < RAW_LEN {
		return None;
	}
	let x: u32 = u16::from_le_bytes([raw[0], raw[1]]) as u32;
	let y: u32 = u16::from_le_bytes([raw[2], raw[3]]) as u32;
	let buttons: u8 = raw[4];
	let col: u16 = ((x * COLS) / NORM_SPAN) as u16;
	let row: u16 = ((y * ROWS) / NORM_SPAN) as u16;
	Some(PointerEvent { col, row, buttons })
}

// THE BLUETOOTH MOUSE, AS ONE MORE POINTER SOURCE.
//
// Every other pointer reaches this service already folded by its driver into an absolute, normalised
// position. A Bluetooth mouse has no driver in that sense - its reports arrive decoded from
// BluetoothService over the profile channel - so the fold happens here, with the drivers' own range,
// and what comes out is the same five-byte record every other pointer produces. From there it takes
// the SAME path: the cell mapping, the ring, and the forward to ConsoleService. There is no second
// pointer path for it to diverge from.
//
// REACHED BY NAME THROUGH THE BROKER, NOT HANDED OVER AT BRING-UP - and that is not a preference. A
// role from a service is a dependency on it in this manifest, with no optional form, and a dependency
// is two things this slot must not be: a Bluetooth stack that never starts would be an input service
// that never starts, and stopping the Bluetooth stack would stop this service and everything above it,
// because a stop takes the whole reverse-dependency closure. The broker hands out a connection to
// whatever instance is live, which is also exactly what reconnecting after a restart needs.
//
// AND THE RESOLVE IS ASYNCHRONOUS. The broker is the supervisor, which is busy during bring-up and
// answers a resolve only once it supervises; a blocking resolve here would park the pointer path until
// then, or for ever on a harness with no broker at all. So the request is sent, the bootstrap channel
// joins the wait, and the answer is taken when it comes. Until then - and on a machine whose broker
// never answers - this slot is simply empty, and every other pointer works as it always did.
struct Bluetooth {
	// The bootstrap channel, which is also the broker channel a resolve travels on.
	broker: u64,
	profile: u64,
	stream: u64,
	pointer: service_logic::hogp::Pointer,
	retry_at: u64,
	// A resolve has been sent and not answered.
	resolving: bool,
	// The broker refused the name: this service is not granted it, and asking again would be asking
	// the same question for the same answer.
	refused: bool,
}

// How long to wait before trying again: two seconds. Long enough that a peer or a stack that is not
// there costs almost nothing, short enough that a person switching a mouse on does not wonder whether
// it worked.
const BLUETOOTH_RETRY_TICKS: u64 = 200;

impl Bluetooth {
	fn wanted(&self) -> bool {
		!self.refused
	}

	// The retry: ask the broker for the profile authority if there is none, or open a stream from the
	// first enabled peer if there is.
	fn retry(&mut self) {
		self.retry_at = clock().saturating_add(BLUETOOTH_RETRY_TICKS);
		if self.profile == 0 {
			if !self.resolving {
				let mut request = Vec::with_capacity(2 + CAP_BT_PROFILE.len());
				request.extend_from_slice(&RESOLVE_OP.to_le_bytes());
				request.extend_from_slice(CAP_BT_PROFILE);
				self.resolving = send_blocking(self.broker, &request, 0);
			}
			return;
		}
		let mut client = proto::system::bluetooth_profile::Client::new(ChannelTransport { chan: self.profile });
		let peers = match client.enabled() {
			Some(Ok(peers)) => peers,
			// THE CONNECTION ITSELF IS DEAD: BluetoothService was restarted and this channel was to the
			// instance that ended. The next retry asks the broker for one to the live instance. A failed
			// transport answers as an error VALUE like any refusal, so it is told apart by the client's
			// own record of what the transport did, not by the shape of the answer.
			_ if client.last_error().is_some() => {
				close(self.profile);
				self.profile = 0;
				return;
			}
			_ => return,
		};
		let Some(first) = peers.first() else { return };
		if let Some(Ok(stream)) = client.open_mouse(&first.controller, &first.peer) {
			self.stream = stream;
		}
	}

	// The broker's answer to a resolve. Anything else on this channel is not this slot's and is left
	// as it came, capability closed.
	fn answered(&mut self) {
		let mut reply = [0u8; 16];
		match try_recv(self.broker, &mut reply) {
			Polled::Message { len, handle } => {
				if len >= 2 && &reply[..2] == b"OK" && handle != 0 {
					self.profile = handle;
					self.resolving = false;
					// Open straight away rather than a retry period later.
					self.retry_at = clock();
				} else {
					if handle != 0 {
						close(handle);
					}
					if len >= 6 && &reply[..6] == b"DENIED" {
						self.refused = true;
					}
					self.resolving = false;
				}
			}
			Polled::Empty => {}
			Polled::Closed => {
				self.resolving = false;
				self.refused = true;
			}
		}
	}

	// Drain the stream into the one pointer path, as the same five-byte record a driver sends.
	fn drain(&mut self, state: &mut Input, forward: u64, buf: &mut [u8]) {
		loop {
			match try_recv_caps(self.stream, buf) {
				PolledCaps::Message { len, handles } => {
					for &handle in handles.as_slice() {
						close(handle);
					}
					let mut frame = Handles::new();
					let Some(report) = proto::system::bluetooth_profile::open_mouse_read(&buf[..len], &mut frame) else { continue };
					let (x, y) = self.pointer.fold(report.dx, report.dy);
					let raw: [u8; RAW_LEN] = [x.to_le_bytes()[0], x.to_le_bytes()[1], y.to_le_bytes()[0], y.to_le_bytes()[1], report.buttons & 0x07];
					if let Some(event) = map_event(&raw) {
						state.record(event);
					}
					if forward != 0 && !state.protected {
						send_blocking(forward, &raw, 0);
					}
				}
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					close(self.stream);
					self.stream = 0;
					self.retry_at = clock().saturating_add(BLUETOOTH_RETRY_TICKS);
					return;
				}
			}
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];

	// 1. the serve channel clients reach us on, and the raw pointer-event channels the
	//    pointer drivers feed us ("INPUT" = the virtio pointer, "INPUT2" = the xhci
	//    driver's USB pointer; a handle is 0 when that source is absent).
	let service: u64 = recv_tagged(bootstrap, &mut buf, b"SERVE").unwrap_or_else(|| fail_bootstrap(bootstrap, b"serve", b"missing serve channel"));
	// ConsoleService's pointer sink: we forward every raw event to it so it can drive
	// selection, scrollback, and mouse reports (handle 0 = no console this boot).
	let forward: u64 = match recv_blocking(bootstrap, &mut buf) {
		Received::Message { len, handle } if len >= 7 && &buf[..7] == b"FORWARD" => handle,
		_ => 0,
	};
	let keys: u64 = match recv_blocking(bootstrap, &mut buf) {
		Received::Message { len, handle } if len >= 4 && &buf[..4] == b"KEYS" => handle,
		_ => 0,
	};
	let focus: u64 = match recv_blocking(bootstrap, &mut buf) {
		Received::Message { len, handle } if len >= 5 && &buf[..5] == b"FOCUS" => handle,
		_ => 0,
	};
	let kill: u64 = match recv_blocking(bootstrap, &mut buf) {
		Received::Message { len, handle } if len >= 4 && &buf[..4] == b"KILL" => handle,
		_ => 0,
	};
	let admin: u64 = match recv_blocking(bootstrap, &mut buf) {
		Received::Message { len, handle } if len >= 5 && &buf[..5] == b"ADMIN" => handle,
		_ => 0,
	};
	// THE POINTERS ARE DISCOVERED, NOT HANDED OVER.
	//
	// They used to arrive as `INPUT` and `INPUT2` - two slots in DeviceManager, one for the virtio
	// pointer and one for the xHCI driver's USB pointer - which is both the per-kind injection the
	// provider catalogue replaces AND a count of pointing devices compiled into the manager. This
	// service asks the catalogue for the input and pointer kinds and takes what it finds: a machine
	// with neither has neither, which is the state a pair of zero handles used to be.
	//
	// LAST IN THE ROLE LIST, because the bootstrap is read POSITIONALLY at every hop.
	let catalogue: u64 = recv_tagged(bootstrap, &mut buf, b"CATALOGUE").unwrap_or(0);
	// THE TRUSTED KEYBOARD'S OWN SINK, apart from the merged one: DeviceManager hands it to the physical
	// keyboard drivers alone, so nothing else can put a key in it. And the protected-input root, which only
	// AdminService is handed. Both optional; a boot without them has no protected path. LAST, like every
	// addition to a positional bootstrap.
	let trusted_keys: u64 = match recv_blocking(bootstrap, &mut buf) {
		Received::Message { len, handle } if len >= 11 && &buf[..11] == b"TRUSTEDKEYS" => handle,
		_ => 0,
	};
	let trusted_root: u64 = recv_tagged(bootstrap, &mut buf, b"TRUSTED").unwrap_or(0);
	let raw: u64 = take_published_pointer(catalogue, &ProviderKind::Input);
	let raw2: u64 = take_published_pointer(catalogue, &ProviderKind::Pointer);
	// A TOUCH SURFACE IS ITS OWN KIND, discovered the same way. A machine with none has none, which
	// is what a zero handle already says.
	let touch: u64 = take_published_pointer(catalogue, &ProviderKind::Touch);
	// The Bluetooth slot starts empty and asks the broker once this service is online - see `Bluetooth`.
	let mut bluetooth = Bluetooth { broker: bootstrap, profile: 0, stream: 0, pointer: service_logic::hogp::Pointer::new(), retry_at: clock(), resolving: false, refused: false };

	// 2. report in to the supervisor that started us.
	{
		send_blocking(bootstrap, b"InputService: online", 0);
	}

	// 3. serve until the client side closes.
	let mut state: Input = Input::new(kill);
	let mut trusted = Trusted { watch: Watch::new(), arming: Arming::Off, producer: 0, seq: 0, protected: false };
	serve(service, admin, [raw, raw2], touch, forward, keys, focus, [trusted_keys, trusted_root], &mut trusted, &mut bluetooth, &mut state);
	exit();
}

// Serve loop: wait on the serve channel and the raw pointer-event channels at once.
// On each wake, drain every queued raw event into the cell-mapped ring, then handle
// one client request. Returns when the client side closes (no more clients). Once
// a raw channel closes (its pointer driver retired), it is dropped from the wait
// set so a peer-closed channel cannot spin the loop.
#[allow(clippy::too_many_arguments)]
fn serve(service: u64, admin: u64, raws: [u64; 2], touch: u64, forward: u64, keys: u64, focus: u64, [trusted_keys, trusted_root]: [u64; 2], trusted: &mut Trusted, bluetooth: &mut Bluetooth, state: &mut Input) {
	let mut req: [u8; 64] = [0u8; 64];
	let mut open: [bool; 2] = [raws[0] != 0, raws[1] != 0];
	let mut clients: Vec<Client> = alloc::vec![Client { chan: service, scope: Scope::Full }];
	let mut keys_open: bool = keys != 0;
	let mut touch_open: bool = touch != 0;
	let mut focus_open: bool = focus != 0;
	let mut trusted_keys_open: bool = trusted_keys != 0;
	let mut trusted_open: bool = trusted_root != 0;
	loop {
		let mut waitset: Vec<u64> = Vec::with_capacity(clients.len() + 5);
		if focus_open {
			waitset.push(focus);
		}
		if keys_open {
			waitset.push(keys);
		}
		if touch_open {
			waitset.push(touch);
		}
		for (i, &raw) in raws.iter().enumerate() {
			if open[i] {
				waitset.push(raw);
			}
		}
		if admin != 0 {
			waitset.push(admin);
		}
		if bluetooth.stream != 0 {
			waitset.push(bluetooth.stream);
		}
		if bluetooth.resolving {
			waitset.push(bluetooth.broker);
		}
		if trusted_keys_open {
			waitset.push(trusted_keys);
		}
		if trusted_open {
			waitset.push(trusted_root);
		}
		// AND ADMINSERVICE'S STREAM, whose reader going away ends any session it held.
		if trusted.producer != 0 {
			waitset.push(trusted.producer);
		}
		waitset.extend(clients.iter().map(|client| client.chan));
		// A BLUETOOTH SLOT WITH NOTHING OPEN WAKES THIS LOOP ON ITS RETRY DEADLINE - unless a resolve is
		// already in flight, whose answer wakes it instead, or the broker has refused the name.
		let pending_retry: bool = bluetooth.stream == 0 && !bluetooth.resolving && bluetooth.wanted();
		let ready: i64 = wait_any(&waitset, if pending_retry { bluetooth.retry_at } else { 0 });
		if pending_retry && clock() >= bluetooth.retry_at {
			bluetooth.retry();
		}
		if ready < 0 {
			continue;
		}
		let ready_handle: u64 = waitset[ready as usize];
		// THE TRUSTED KEYBOARD, noticed before anything else is delivered: secure attention, and the keys of
		// an armed session, go to AdminService's stream and nowhere else.
		if trusted_keys_open && ready_handle == trusted_keys {
			loop {
				match try_recv(trusted_keys, &mut req) {
					Polled::Message { len, handle } => {
						if handle != 0 {
							close(handle);
						}
						trusted.record(&req[..len]);
						// SECURE ATTENTION TAKES THE ORDINARY PATH AWAY BEFORE ANYTHING ELSE IS DELIVERED.
						if trusted.protected {
							state.protect(forward);
						}
					}
					Polled::Empty => break,
					Polled::Closed => {
						trusted_keys_open = false;
						trusted.watch.clear();
						trusted.emit(TrustedInputKind::Lost, 0, 0, false);
						break;
					}
				}
			}
			continue;
		}
		if trusted_open && ready_handle == trusted_root {
			match recv_caps_blocking(trusted_root, &mut req) {
				ReceivedCaps::Message { len, handles } => {
					let mut handles = handles;
					let op: u16 = if len >= 2 { u16::from_le_bytes([req[0], req[1]]) } else { 0 };
					if op == input_trusted::OP_EVENTS {
						// ONE STREAM: a new one replaces the old, whose reader is gone or starting over.
						if let Some((corr, _)) = input_trusted::events_open(trusted, &req[..len], &mut handles)
							&& let Some((producer, consumer)) = channel()
						{
							if trusted.producer != 0 {
								close(trusted.producer);
							}
							trusted.producer = producer;
							trusted.seq = 0;
							if !send_blocking(trusted_root, &corr.to_le_bytes(), consumer) {
								close(consumer);
							}
							if !trusted_keys_open {
								trusted.emit(TrustedInputKind::Lost, 0, 0, false);
							}
						}
					} else {
						let mut reply: [u8; 64] = [0; 64];
						let mut reply_handles = Handles::new();
						if let Some(n) = input_trusted::dispatch(trusted, &req[..len], &mut handles, &mut reply, &mut reply_handles) {
							send_blocking(trusted_root, &reply[..n], 0);
						}
						// A DISARM gives the ordinary path back.
						if !trusted.protected {
							state.protected = false;
						}
					}
					for &leftover in handles.as_slice() {
						close(leftover);
					}
				}
				ReceivedCaps::Closed => {
					trusted_open = false;
					trusted.reader_lost();
					state.protected = false;
				}
			}
			continue;
		}
		if trusted.producer != 0 && ready_handle == trusted.producer {
			match try_recv(trusted.producer, &mut req) {
				Polled::Closed => {
					trusted.reader_lost();
					state.protected = false;
				}
				Polled::Message { handle, .. } if handle != 0 => close(handle),
				_ => {}
			}
			continue;
		}
		if bluetooth.resolving && ready_handle == bluetooth.broker {
			bluetooth.answered();
			continue;
		}
		if bluetooth.stream != 0 && ready_handle == bluetooth.stream {
			let mut frame_buf: [u8; 64] = [0u8; 64];
			bluetooth.drain(state, forward, &mut frame_buf);
			continue;
		}
		if focus_open && ready_handle == focus {
			let acknowledged: bool = match recv_blocking(focus, &mut req) {
				Received::Message { len, handle } if len >= 3 && &req[..3] == b"SET" && handle != 0 => {
					state.notify_console(false, forward);
					state.set_focus(handle);
					true
				}
				Received::Message { len, handle } if len >= 7 && &req[..7] == b"CONSOLE" => {
					if handle != 0 {
						close(handle);
					}
					state.set_focus(0);
					state.notify_console(true, forward);
					true
				}
				Received::Message { handle, .. } => {
					if handle != 0 {
						close(handle);
					}
					state.set_focus(0);
					state.notify_console(false, forward);
					true
				}
				Received::Closed => {
					focus_open = false;
					state.set_focus(0);
					false
				}
			};
			if acknowledged {
				send_blocking(focus, b"OK", 0);
			}
			continue;
		}
		if touch_open && ready_handle == touch {
			loop {
				match try_recv(touch, &mut req) {
					Polled::Message { len, handle } => {
						if handle != 0 {
							close(handle);
						}
						if let Some(contact) = contact_from(&req[..len]) {
							state.record_contact(contact);
						}
					}
					Polled::Empty => break,
					Polled::Closed => {
						touch_open = false;
						break;
					}
				}
			}
			continue;
		}
		if keys_open && ready_handle == keys {
			loop {
				match try_recv(keys, &mut req) {
					Polled::Message { len, handle } => {
						if handle != 0 {
							close(handle);
						}
						state.record_key(&req[..len]);
					}
					Polled::Empty => break,
					Polled::Closed => {
						keys_open = false;
						break;
					}
				}
			}
			continue;
		}
		for (i, &raw) in raws.iter().enumerate() {
			if !open[i] || ready_handle != raw {
				continue;
			}
			loop {
				match try_recv(raw, &mut req) {
					Polled::Message { len, .. } => {
						if let Some(event) = map_event(&req[..len]) {
							state.record(event);
						}
						if forward != 0 && !state.protected {
							send_blocking(forward, &req[..len], 0);
						}
					}
					Polled::Empty => break,
					Polled::Closed => {
						open[i] = false;
						break;
					}
				}
			}
			continue;
		}
		if admin != 0 && ready_handle == admin {
			match recv_caps_blocking(admin, &mut req) {
				ReceivedCaps::Message { len, handles: caps } => {
					let mut reply: [u8; 64] = [0; 64];
					let mut reply_handle = proto::codec::Handles::new();
					// EVERY CAPABILITY THE MESSAGE CARRIED. This was `Handles::from_slice(&[handle])`
					// over the single-handle receive, which keeps the first and drops the rest - so a
					// client sending stdin, stdout and stderr had two destroyed before dispatch.
					let mut handle = caps;
					let mut call = AdminCall { clients: &mut clients };
					if let Some(n) = input_admin::dispatch(&mut call, &req[..len], &mut handle, &mut reply, &mut reply_handle) {
						if !send_caps_blocking(admin, &reply[..n], reply_handle.as_slice()) {
							for &leftover in reply_handle.as_slice() {
								close(leftover);
							}
						}
					} else {
						for &leftover in reply_handle.as_slice() {
							close(leftover);
						}
					}
					for &unclaimed in handle.as_slice() {
						close(unclaimed);
					}
				}
				ReceivedCaps::Closed => return,
			}
			continue;
		}
		let Some(client_index) = clients.iter().position(|client| client.chan == ready_handle) else { continue };
		let client: u64 = clients[client_index].chan;
		let scope: Scope = clients[client_index].scope;
		match recv_blocking(client, &mut req) {
			Received::Message { len, mut handle } => {
				let op: u16 = if len >= 2 { u16::from_le_bytes([req[0], req[1]]) } else { 0 };
				if op == CONNECT_OP && scope == Scope::Full {
					if let Some((mine, theirs)) = channel() {
						clients.push(Client { chan: mine, scope });
						send_blocking(client, &[], theirs);
					}
				} else if op == input::OP_SUBSCRIBE && scope == Scope::Full {
					stream_subscribe(client, &req[..len], state);
				} else if op == input::OP_SUBSCRIBE_KEYS {
					stream_subscribe_keys(client, &req[..len], &mut handle, state);
				} else if op == input::OP_SUBSCRIBE_CONTACTS {
					stream_subscribe_contacts(client, &req[..len], &mut handle, state);
				} else if len >= 6 {
					send_blocking(client, &req[2..6], 0);
				}
				if handle != 0 {
					close(handle);
				}
			}
			Received::Closed => {
				if state.contact_stream.as_ref().is_some_and(|stream| stream.owner == client) {
					state.close_contact_stream(false);
				}
				if state.key_stream.as_ref().is_some_and(|stream| stream.owner == client) {
					state.close_key_stream(false);
				}
				if client_index == 0 {
					return;
				}
				close(client);
				clients.swap_remove(client_index);
			}
		}
	}
}

fn stream_subscribe_keys(service: u64, request: &[u8], request_handle: &mut u64, state: &mut Input) {
	if request.len() != 10 || *request_handle == 0 {
		return;
	}
	let corr: u32 = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
	let proof: u64 = core::mem::take(request_handle);
	let valid: bool = state.validate_focus(proof);
	close(proof);
	if !valid {
		send_blocking(service, &corr.to_le_bytes(), 0);
		return;
	}
	state.close_key_stream(true);
	let (producer, consumer): (u64, u64) = match channel() {
		Some(pair) => pair,
		None => return,
	};
	if send_blocking(service, &corr.to_le_bytes(), consumer) {
		state.key_stream = Some(KeyStream { owner: service, producer, seq: 0 });
	} else {
		close(producer);
		close(consumer);
	}
}

// The contact half of `stream_subscribe_keys`, on the same one-shot proof and with the same
// refusal: a caller whose proof does not name the active surface gets the correlation id back and no
// capability, which is a refusal it can tell from a service that is not there.
fn stream_subscribe_contacts(service: u64, request: &[u8], request_handle: &mut u64, state: &mut Input) {
	if request.len() != 10 || *request_handle == 0 {
		return;
	}
	let corr: u32 = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
	let proof: u64 = core::mem::take(request_handle);
	let valid: bool = state.validate_focus(proof);
	close(proof);
	if !valid {
		send_blocking(service, &corr.to_le_bytes(), 0);
		return;
	}
	state.close_contact_stream(true);
	let (producer, consumer): (u64, u64) = match channel() {
		Some(pair) => pair,
		None => return,
	};
	if send_blocking(service, &corr.to_le_bytes(), consumer) {
		state.contact_stream = Some(ContactStream { owner: service, producer, seq: 0 });
	} else {
		close(producer);
		close(consumer);
	}
}

// Serve one `subscribe` request: gather the bounded snapshot, then stream the mapped
// pointer events to the client over a fresh sub-channel. The reply on the service
// channel carries the correlation id and the consumer endpoint (out-of-band); each
// event then travels as its own framed message on the producer endpoint, and closing
// the producer marks end-of-stream.
fn stream_subscribe(service: u64, request: &[u8], state: &mut Input) {
	let mut request_handle = proto::codec::Handles::new();
	let (corr, items): (u32, Vec<PointerEvent>) = match input::subscribe_open(state, request, &mut request_handle) {
		Some(v) => v,
		None => return,
	};
	let (producer, consumer): (u64, u64) = match channel() {
		Some(pair) => pair,
		None => return,
	};
	let corr_bytes: [u8; 4] = corr.to_le_bytes();
	send_blocking(service, &corr_bytes, consumer);
	let mut frame: [u8; 32] = [0u8; 32];
	for (seq, item) in items.iter().enumerate() {
		let mut frame_handles = Handles::new();
		if let Some(n) = input::subscribe_frame(seq as u32, item, &mut frame, &mut frame_handles) {
			if !send_caps_blocking(producer, &frame[..n], frame_handles.as_slice()) {
				for handle in frame_handles.as_slice() {
					close(*handle);
				}
			}
		} else {
			for handle in frame_handles.as_slice() {
				close(*handle);
			}
		}
	}
	close(producer);
}
