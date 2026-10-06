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
// GAMEPADS are followed rather than taken once: the catalogue subscription for the `gamepad` kind stays
// open, every live provider of it is adopted (up to four) and every withdrawn one dropped, and each
// gamepad on them gets an id of this service's own. Who sees them is decided here and nowhere else - a
// graphical application holding display focus through `subscribe-gamepads`, or a console program granted
// `input-gamepad` through `observe-gamepads` while no surface holds focus - and a protected session
// closes both.
//
// When the supervisor that started it drops the bootstrap channel (no clients this
// boot), the service exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use driver_protocol::gamepad as pad_wire;
use ipc_client::ChannelTransport;
use keys::KeyState;
use proto::codec::Handles;
use proto::system::input;
use proto::system::input_admin::{self, Service as AdminService};
use proto::system::input_trusted;
use proto::system::{ContactEvent, Error, Gamepad, GamepadAxis, GamepadEvent, GamepadState, KeyEvent, PointerEvent, ProviderInfo, ProviderKind, TrustedInput, TrustedInputKind, provider_catalogue};
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
// THE GAMEPAD BOUNDS: providers adopted, and gamepads tracked across all of them.
const MAX_GAMEPAD_PROVIDERS: usize = 4;
const MAX_GAMEPADS: usize = 8;
// A STREAM'S PENDING STATES ARE RETRIED ON THIS DEADLINE, on a housekeeping wait: one tick. The loop cannot
// wait for room in a channel it writes, and a reader that never drains must not keep a settling scheduler
// from settling.
const GAMEPAD_RETRY_TICKS: u64 = 1;
// The largest frame of a gamepad stream - a `present` with every count at its most - with room over.
const GAMEPAD_FRAME: usize = 256;
// THE POINTER AND TOUCH BOUNDS: four pointer providers attached at once, their events merged into the one cursor; ONE
// touch surface at a time, because a contact event names no surface and each surface normalises its axes across itself -
// two surfaces in one stream would be contacts in two coordinate spaces under colliding identities.
const MAX_POINTER_PROVIDERS: usize = 4;
const MAX_TOUCH_SURFACES: usize = 1;

// A publication as a line names it: the device that published it, and the catalogue's slot and generation.
fn publication_text(info: &ProviderInfo) -> String {
	match info.platform {
		Some(row) => format!("platform device {row}, slot {} generation {}", info.slot, info.provider_generation),
		None => format!("{:02x}:{:02x}.{}, slot {} generation {}", info.bus, info.dev, info.func, info.slot, info.provider_generation),
	}
}

// ONE KIND OF PROVIDER, FOLLOWED - `pointer` or `touch`. The catalogue subscription stays OPEN for this service's life,
// and every live provider it announces, at bootstrap or later, is attached up to the kind's bound; one past it waits,
// said once, and is attached when one of its kind is detached. A provider is detached on its withdrawal or when its
// connection closes, and is not opened again until it is published again.
struct Followed {
	catalogue: u64,
	subscription: u64,
	what: &'static str,
	bound: usize,
	attached: Vec<(ProviderInfo, u64)>,
	waiting: Vec<ProviderInfo>,
	// A provider waited past the bound, and that was said.
	told: bool,
}

impl Followed {
	// Subscribed, and what is published already attached - polled, never blocked: the catalogue sends a new subscriber
	// everything published in the same step, and a machine with nothing of the kind must not hang this service's start.
	fn subscribe(catalogue: u64, kind: &ProviderKind, what: &'static str, bound: usize) -> Followed {
		let subscription = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(kind).unwrap_or(0) } else { 0 };
		if catalogue != 0 && subscription == 0 {
			print(format!("InputService: the catalogue refused the {what} subscription\n").as_bytes());
		}
		let mut followed = Followed { catalogue, subscription, what, bound, attached: Vec::new(), waiting: Vec::new(), told: false };
		followed.drain();
		followed
	}

	fn connections(&self) -> impl Iterator<Item = u64> + '_ {
		self.attached.iter().map(|(_, chan)| *chan)
	}

	fn holds(&self, chan: u64) -> bool {
		self.attached.iter().any(|(_, held)| *held == chan)
	}

	// Every frame the subscription holds: a publication attached or waiting, a withdrawal detached. Answers whether
	// anything was detached.
	fn drain(&mut self) -> bool {
		let mut detached = false;
		let mut buf = [0u8; 256];
		while self.subscription != 0 {
			let (len, handles) = match try_recv_caps(self.subscription, &mut buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					close(self.subscription);
					self.subscription = 0;
					break;
				}
			};
			for &leftover in handles.as_slice() {
				close(leftover);
			}
			let mut frame_handles = wire::Handles::new();
			let Some(info) = provider_catalogue::subscribe_read(&buf[..len], &mut frame_handles) else {
				print(b"InputService: a provider frame did not decode\n");
				continue;
			};
			if info.live {
				self.adopt(info);
			} else if let Some(chan) = self.attached.iter().find(|(held, _)| same_publication(held, &info)).map(|(_, chan)| *chan) {
				self.detach(chan);
				detached = true;
			} else {
				self.waiting.retain(|held| !same_publication(held, &info));
			}
		}
		detached
	}

	fn adopt(&mut self, info: ProviderInfo) {
		if self.attached.iter().any(|(held, _)| same_publication(held, &info)) || self.waiting.iter().any(|held| same_publication(held, &info)) {
			return;
		}
		if self.attached.len() >= self.bound {
			if !self.told {
				print(format!("InputService: a {} provider waits - this service attaches {} at a time\n", self.what, self.bound).as_bytes());
				self.told = true;
			}
			self.waiting.push(info);
			return;
		}
		match provider_catalogue::Client::new(ChannelTransport { chan: self.catalogue }).open(&info) {
			Some(Ok(chan)) => {
				let named = publication_text(&info);
				self.attached.push((info, chan));
				print(format!("InputService: a {} provider is attached - {named} ({} attached)\n", self.what, self.attached.len()).as_bytes());
			}
			Some(Err(_)) => print(format!("InputService: the catalogue refused a connection to a {} provider it published\n", self.what).as_bytes()),
			None => print(format!("InputService: the catalogue did not answer the connection to a {} provider\n", self.what).as_bytes()),
		}
	}

	// A provider detached - withdrawn, or its connection closed - and the first one waiting attached in its place.
	fn detach(&mut self, chan: u64) {
		let Some(at) = self.attached.iter().position(|(_, held)| *held == chan) else { return };
		let (info, _) = self.attached.remove(at);
		close(chan);
		print(format!("InputService: a {} provider is detached - {} ({} attached)\n", self.what, publication_text(&info), self.attached.len()).as_bytes());
		while self.attached.len() < self.bound && !self.waiting.is_empty() {
			let next = self.waiting.remove(0);
			self.adopt(next);
		}
	}
}

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
	gamepads: Gamepads,
}

// ONE GAMEPAD PROVIDER: its publication, and the connection its frames arrive on.
struct PadSource {
	info: ProviderInfo,
	chan: u64,
}

// ONE GAMEPAD, keyed by the connection it arrived on and the publisher's handle for it - two controllers both
// numbering from one cannot collide - and known to consumers by the id this service gave it.
struct Pad {
	id: u32,
	source: u64,
	handle: u32,
	shape: pad_wire::Shape,
	state: pad_wire::State,
}

impl Pad {
	fn record(&self) -> Gamepad {
		Gamepad { id: self.id, label: String::from_utf8_lossy(self.shape.label()).into_owned(), axes: self.shape.axes().iter().map(|axis| GamepadAxis { usage: axis.usage, minimum: axis.minimum, maximum: axis.maximum }).collect(), buttons: self.shape.buttons(), hats: self.shape.hats() }
	}

	fn event(&self, state: &pad_wire::State) -> GamepadEvent {
		GamepadEvent::State(GamepadState { id: self.id, buttons: state.buttons, hats: state.hats[..self.shape.hats() as usize].to_vec(), axes: state.axes[..self.shape.axes().len()].to_vec() })
	}

	// What a reader is left holding when the stream goes: nothing pressed, every hat centred, the axes where
	// they are.
	fn released(&self) -> pad_wire::State {
		pad_wire::State { buttons: 0, hats: [pad_wire::CENTRED; pad_wire::MAX_HATS as usize], axes: self.state.axes }
	}

	fn holds_anything(&self) -> bool {
		self.state.buttons != 0 || self.state.hats.iter().any(|hat| *hat != pad_wire::CENTRED)
	}
}

// THE GAMEPADS A STREAM IS OWED A STATE OF, oldest first: at most one entry per gamepad, so at most
// `MAX_GAMEPADS`. A FIXED LIST AND NOT A VECTOR OF IDS: a `Vec<u32>`'s growth routine is a generic this program
// would import from whichever shared library happens to export it first, which is a provider it does not
// declare.
#[derive(Default)]
struct Owed {
	ids: [u32; MAX_GAMEPADS],
	len: usize,
}

impl Owed {
	fn first(&self) -> Option<u32> {
		(self.len > 0).then(|| self.ids[0])
	}

	fn contains(&self, id: u32) -> bool {
		self.ids[..self.len].contains(&id)
	}

	fn is_empty(&self) -> bool {
		self.len == 0
	}

	fn push(&mut self, id: u32) {
		if self.len < MAX_GAMEPADS && !self.contains(id) {
			self.ids[self.len] = id;
			self.len += 1;
		}
	}

	fn pop_first(&mut self) {
		if self.len > 0 {
			self.ids.copy_within(1..self.len, 0);
			self.len -= 1;
		}
	}

	fn remove(&mut self, id: u32) {
		if let Some(at) = self.ids[..self.len].iter().position(|held| *held == id) {
			self.ids.copy_within(at + 1..self.len, at);
			self.len -= 1;
		}
	}
}

// How one frame of a gamepad stream went.
enum Fed {
	Went,
	Full,
	Gone,
}

// ONE GAMEPAD STREAM, and the gamepads whose state it is owed: AT MOST ONE PER GAMEPAD, and what goes when
// there is room is that gamepad's state at that moment - so a reader that falls behind is sent fewer
// intermediate states and never a stale last one.
struct PadStream {
	owner: u64,
	producer: u64,
	seq: u32,
	pending: Owed,
}

impl PadStream {
	// One frame, never blocking. A full channel is not a gone reader: the caller decides what a full one means.
	fn feed(&mut self, event: &GamepadEvent) -> Fed {
		let mut frame: [u8; GAMEPAD_FRAME] = [0; GAMEPAD_FRAME];
		let mut frame_handles = Handles::new();
		let Some(len) = input::subscribe_gamepads_frame(self.seq, event, &mut frame, &mut frame_handles) else { return Fed::Gone };
		match try_send_outcome(self.producer, &frame[..len], 0) {
			SendOutcome::Delivered => {
				self.seq = self.seq.wrapping_add(1);
				Fed::Went
			}
			SendOutcome::Stalled => Fed::Full,
			SendOutcome::Failed => Fed::Gone,
		}
	}

	// What the stream is owed, oldest first, as room allows. `false` when its reader is gone.
	fn flush(&mut self, pads: &[Pad]) -> bool {
		while let Some(id) = self.pending.first() {
			let Some(pad) = pads.iter().find(|pad| pad.id == id) else {
				self.pending.pop_first();
				continue;
			};
			match self.feed(&pad.event(&pad.state)) {
				Fed::Went => {
					self.pending.pop_first();
				}
				Fed::Full => return true,
				Fed::Gone => return false,
			}
		}
		true
	}

	// One gamepad's current state, after what the stream is owed - and owed itself when there is no room.
	// `false` when the reader is gone.
	fn offer_state(&mut self, pads: &[Pad], id: u32) -> bool {
		if !self.flush(pads) {
			return false;
		}
		if self.pending.contains(id) {
			return true;
		}
		if !self.pending.is_empty() {
			self.pending.push(id);
			return true;
		}
		let Some(pad) = pads.iter().find(|pad| pad.id == id) else { return true };
		match self.feed(&pad.event(&pad.state)) {
			Fed::Went => true,
			Fed::Full => {
				self.pending.push(id);
				true
			}
			Fed::Gone => false,
		}
	}

	// A `present`, an `arrived` or a `departed`: NEVER COALESCED, and sent only after everything the stream is
	// owed. `false` when it could not all go, and the stream then closes - a reader missing one of these holds
	// the wrong set of gamepads.
	fn offer_whole(&mut self, pads: &[Pad], event: &GamepadEvent) -> bool {
		if !self.flush(pads) || !self.pending.is_empty() {
			return false;
		}
		matches!(self.feed(event), Fed::Went)
	}

	// Close the stream - releasing to it first, when asked, every gamepad holding a button or a hat off centre
	// and every one it is owed a state of: the release replaces that pending state. A reader that cannot take a
	// release sees the stream close, and a closed stream means every gamepad it carried is released and gone.
	fn close(mut self, pads: &[Pad], release: bool) {
		if release {
			for pad in pads {
				if (self.pending.contains(pad.id) || pad.holds_anything()) && !matches!(self.feed(&pad.event(&pad.released())), Fed::Went) {
					break;
				}
			}
		}
		close(self.producer);
	}
}

// A new stream for `service`'s request `corr`: the reply, then a `present` and a `state` for every gamepad
// there now. `None` when the reply could not go or the opening could not all be delivered.
fn open_pad_stream(service: u64, corr: u32, pads: &[Pad]) -> Option<PadStream> {
	let Some((producer, consumer)) = channel() else {
		send_blocking(service, &corr.to_le_bytes(), 0);
		return None;
	};
	if !send_blocking(service, &corr.to_le_bytes(), consumer) {
		close(producer);
		close(consumer);
		return None;
	}
	let mut stream = PadStream { owner: service, producer, seq: 0, pending: Owed::default() };
	for pad in pads {
		if !stream.offer_whole(pads, &GamepadEvent::Present(pad.record())) || !stream.offer_state(pads, pad.id) {
			close(stream.producer);
			return None;
		}
	}
	Some(stream)
}

// Two providers' publications are the same one.
fn same_publication(a: &ProviderInfo, b: &ProviderInfo) -> bool {
	a.slot == b.slot && a.provider_generation == b.provider_generation && a.binding_generation == b.binding_generation
}

// EVERY GAMEPAD, where it came from, and the two streams that may see them: a graphical application's, on its
// focus proof, and a console program's, while no surface holds focus.
struct Gamepads {
	catalogue: u64,
	/// The `gamepad` catalogue subscription, KEPT OPEN: publications after bootstrap are adopted from it.
	subscription: u64,
	sources: Vec<PadSource>,
	pads: Vec<Pad>,
	next_id: u32,
	focused: Option<PadStream>,
	console: Option<PadStream>,
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
	/// A console program granted `input-gamepad`: `observe-gamepads` and nothing else.
	Gamepad,
	/// The activity signal's root and its connections: `input-activity.watch` and nothing else.
	Activity,
	/// The system keys' root and its connections: `system-keys.watch` and nothing else - DisplayService's alone.
	SystemKeys,
}

// THE SYSTEM KEYS (`liber:input@1/system-keys`): the consumer page's brightness pair, taken off the raw key stream
// BEFORE ordinary delivery and before the protected session's check - they never enter `key-event`, and the person at
// the protected screen can still make it readable - and streamed to DisplayService, which owns the brightness. A
// press is one item; releases are not sent.
#[derive(Default)]
struct SystemKeys {
	watchers: Vec<(u64, u32)>,
}

// How deep a watcher's stream is: presses at human rate, and a reader that stops is cut off rather than queued for.
const SYSTEM_KEY_DEPTH: u64 = 16;

impl SystemKeys {
	fn press(&mut self, key: proto::system::SystemKey) {
		for (stream, seq) in self.watchers.iter_mut() {
			let mut frame = [0u8; 32];
			let mut frame_handles = Handles::new();
			let sent = proto::system::system_keys::watch_frame(*seq, &key, &mut frame, &mut frame_handles).is_some_and(|n| try_send(*stream, &frame[..n], 0));
			*seq = seq.wrapping_add(1);
			if !sent && matches!(try_recv(*stream, &mut [0u8; 8]), Polled::Closed) {
				close(*stream);
				*stream = 0;
			}
		}
		self.watchers.retain(|(stream, _)| *stream != 0);
	}

	fn open(&mut self, client: u64, request: &[u8]) {
		let mut request_handle = Handles::new();
		let Some((corr, _)) = proto::system::system_keys::watch_open(&mut SystemKeysWatch, request, &mut request_handle) else { return };
		let Some((producer, consumer)) = channel_with_depth(SYSTEM_KEY_DEPTH) else {
			send_blocking(client, &corr.to_le_bytes(), 0);
			return;
		};
		if !send_blocking(client, &corr.to_le_bytes(), consumer) {
			close(producer);
			return;
		}
		self.watchers.push((producer, 0));
	}
}

struct SystemKeysWatch;

impl proto::system::system_keys::Service for SystemKeysWatch {
	fn watch(&mut self) -> Vec<proto::system::SystemKey> {
		Vec::new()
	}
}

// THE FIVE-BYTE RAW FRAME of a system key - `[page u16][usage u16][state u8]`, beside the keyboard page's three-byte one -
// as the key, when it is one of the set and pressed. A release is `Some(None)`: taken off the stream, and nothing sent.
fn system_key(raw: &[u8]) -> Option<Option<proto::system::SystemKey>> {
	if raw.len() != 5 {
		return None;
	}
	let page = u16::from_le_bytes([raw[0], raw[1]]);
	let usage = u16::from_le_bytes([raw[2], raw[3]]);
	let key = match (page, usage) {
		(0x0c, 0x6f) => proto::system::SystemKey::BrightnessUp,
		(0x0c, 0x70) => proto::system::SystemKey::BrightnessDown,
		_ => return Some(None),
	};
	Some((raw[4] != 0).then_some(key))
}

// THE ACTIVITY SIGNAL (`liber:input@1/input-activity`): the last input from ANY source - keyboards, pointers, touch
// surfaces, gamepads, the Bluetooth mouse and the trusted keyboard, the protected session's included - and each watcher's
// edges, which `service_logic::activity` decides. Nothing about what the input was reaches a watcher.
struct Activity {
	last: u64,
	watchers: Vec<Watcher>,
}

struct Watcher {
	stream: u64,
	watch: service_logic::activity::Watch,
	seq: u32,
}

// How deep a watcher's stream is: it carries edges, two per idle period at the most.
const ACTIVITY_DEPTH: u64 = 8;

impl Activity {
	// INPUT SEEN: the clock of the last input moves, and every idle watcher is told `active`.
	fn seen(&mut self) {
		self.last = clock();
		for at in 0..self.watchers.len() {
			if let Some(edge) = self.watchers[at].watch.input() {
				Activity::emit(&mut self.watchers[at], edge);
			}
		}
		self.watchers.retain(|watcher| watcher.stream != 0);
	}

	// TIME PASSED: every watcher whose interval has run out since the last input is told `idle`.
	fn tick(&mut self) {
		let now = clock();
		let last = self.last;
		for at in 0..self.watchers.len() {
			if let Some(edge) = self.watchers[at].watch.tick(last, now) {
				Activity::emit(&mut self.watchers[at], edge);
			}
		}
		self.watchers.retain(|watcher| watcher.stream != 0);
	}

	// When the loop must next wake for an edge; zero for never.
	fn due(&self) -> u64 {
		self.watchers.iter().filter_map(|watcher| watcher.watch.due(self.last)).min().unwrap_or(0)
	}

	// ONE EDGE ON A WATCHER'S STREAM; a reader gone is the watcher gone.
	fn emit(watcher: &mut Watcher, edge: service_logic::activity::Edge) {
		let edge = match edge {
			service_logic::activity::Edge::Idle => proto::system::ActivityEdge::Idle,
			service_logic::activity::Edge::Active => proto::system::ActivityEdge::Active,
		};
		let mut frame = [0u8; 32];
		let mut frame_handles = Handles::new();
		let sent = proto::system::input_activity::watch_frame(watcher.seq, &edge, &mut frame, &mut frame_handles).is_some_and(|n| try_send(watcher.stream, &frame[..n], 0));
		watcher.seq = watcher.seq.wrapping_add(1);
		if !sent && matches!(try_recv(watcher.stream, &mut [0u8; 8]), Polled::Closed) {
			close(watcher.stream);
			watcher.stream = 0;
		}
	}
}

// `watch(idle-after-ms)`: the snapshot is empty - an edge is told when it happens, and a watcher whose interval has already
// run out hears `idle` at the next tick.
struct ActivityWatch {
	idle_after: u64,
}

impl proto::system::input_activity::Service for ActivityWatch {
	fn watch(&mut self, idle_after_ms: u32) -> Vec<proto::system::ActivityEdge> {
		self.idle_after = u64::from(idle_after_ms).saturating_mul(TICKS_PER_SECOND) / 1000;
		Vec::new()
	}
}

// A WATCH OPENED on an activity connection: refused - answered with no stream - for an interval outside a second to a day.
fn stream_watch_activity(client: u64, request: &[u8], activity: &mut Activity) {
	let mut request_handle = Handles::new();
	let mut asked = ActivityWatch { idle_after: 0 };
	let Some((corr, _)) = proto::system::input_activity::watch_open(&mut asked, request, &mut request_handle) else { return };
	let Some(watch) = service_logic::activity::Watch::new(asked.idle_after) else {
		send_blocking(client, &corr.to_le_bytes(), 0);
		return;
	};
	let Some((producer, consumer)) = channel_with_depth(ACTIVITY_DEPTH) else {
		send_blocking(client, &corr.to_le_bytes(), 0);
		return;
	};
	if !send_blocking(client, &corr.to_le_bytes(), consumer) {
		close(producer);
		return;
	}
	activity.watchers.push(Watcher { stream: producer, watch, seq: 0 });
	activity.tick();
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

	fn open_gamepads(&mut self) -> Result<u64, Error> {
		let (server, client): (u64, u64) = channel().ok_or(Error::Again)?;
		self.clients.push(Client { chan: server, scope: Scope::Gamepad });
		Ok(client)
	}
}

impl Input {
	fn new(kill_control: u64, gamepads: Gamepads) -> Input {
		Input { recent: Vec::new(), keys: KeyState::new(), focus_peer: 0, kill_control, key_stream: None, contact_stream: None, contacts_down: Vec::new(), proof_nonce: 0, chord_held: false, protected: false, gamepads }
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
		// AND BOTH GAMEPAD STREAMS, released and closed exactly as key and contact focus is revoked. The gamepads
		// are still tracked, so nothing is stuck when the session ends.
		self.close_console_pads();
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

	// A DETACHED SURFACE'S CONTACTS STILL DOWN ARE LIFTED, as focus loss lifts them: the fingers of a surface that is gone
	// can never lift themselves.
	fn lift_contacts(&mut self) {
		for id in core::mem::take(&mut self.contacts_down) {
			if !self.send_contact(ContactEvent { id, tip: false, x: 0, y: 0 }) {
				break;
			}
		}
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
		// THE FOCUSED GAMEPAD STREAM GOES WITH THE FOCUS, every held button and hat released to it first.
		if let Some(stream) = self.gamepads.focused.take() {
			stream.close(&self.gamepads.pads, true);
		}
		if self.focus_peer != 0 {
			close(self.focus_peer);
		}
		self.focus_peer = peer;
	}

	// The console program's gamepad stream, released and closed: a surface took focus, or a protected session
	// began.
	fn close_console_pads(&mut self) {
		if let Some(stream) = self.gamepads.console.take() {
			stream.close(&self.gamepads.pads, true);
		}
	}

	// Every wake: what the gamepad streams are owed, as room allows. A stream whose reader is gone is dropped.
	fn flush_pads(&mut self) {
		let pads = &self.gamepads.pads;
		for slot in [&mut self.gamepads.focused, &mut self.gamepads.console] {
			if let Some(stream) = slot.as_mut()
				&& !stream.flush(pads)
				&& let Some(gone) = slot.take()
			{
				close(gone.producer);
			}
		}
	}

	fn pads_pending(&self) -> bool {
		self.gamepads.focused.as_ref().is_some_and(|stream| !stream.pending.is_empty()) || self.gamepads.console.as_ref().is_some_and(|stream| !stream.pending.is_empty())
	}

	// A PUBLISHED GAMEPAD PROVIDER, adopted - up to four, and never twice.
	fn adopt_pad_source(&mut self, info: ProviderInfo) {
		if self.gamepads.sources.iter().any(|source| same_publication(&source.info, &info)) {
			return;
		}
		if self.gamepads.sources.len() >= MAX_GAMEPAD_PROVIDERS {
			print(b"InputService: a gamepad provider was refused: this service adopts four\n");
			return;
		}
		match provider_catalogue::Client::new(ChannelTransport { chan: self.gamepads.catalogue }).open(&info) {
			Some(Ok(chan)) => self.gamepads.sources.push(PadSource { info, chan }),
			_ => print(b"InputService: a published gamepad provider could not be opened\n"),
		}
	}

	// A PROVIDER IS GONE - withdrawn, or its connection closed - and every one of its gamepads departs. A closed
	// connection is not opened again: the publisher never closes one, so the driver's end is gone, and its
	// replacement is a publication of its own.
	fn lose_pad_source(&mut self, chan: u64) {
		let Some(at) = self.gamepads.sources.iter().position(|source| source.chan == chan) else { return };
		let source = self.gamepads.sources.remove(at);
		close(source.chan);
		// One at a time, because each departure changes the table it was found in.
		while let Some(handle) = self.gamepads.pads.iter().find(|pad| pad.source == chan).map(|pad| pad.handle) {
			self.pad_departed(chan, handle);
		}
	}

	// One frame from a provider. A frame the wire refuses, a STATE for a gamepad this provider does not hold or
	// of another shape, and a second ARRIVAL for a handle held change nothing.
	fn pad_frame(&mut self, chan: u64, bytes: &[u8]) {
		match pad_wire::decode(bytes) {
			Some(pad_wire::Frame::Arrival { handle, shape }) => self.pad_arrived(chan, handle, shape),
			Some(pad_wire::Frame::State(frame)) => self.pad_state(chan, &frame),
			Some(pad_wire::Frame::Departure { handle }) => self.pad_departed(chan, handle),
			None => {}
		}
	}

	fn pad_arrived(&mut self, source: u64, handle: u32, shape: pad_wire::Shape) {
		if self.gamepads.pads.iter().any(|pad| pad.source == source && pad.handle == handle) {
			return;
		}
		if self.gamepads.pads.len() >= MAX_GAMEPADS {
			print(b"InputService: a gamepad was refused: this service tracks eight\n");
			return;
		}
		let id = self.gamepads.next_id;
		self.gamepads.next_id = self.gamepads.next_id.wrapping_add(1).max(1);
		// AT REST UNTIL ITS FIRST REPORT: no buttons, every hat centred, each axis at its midpoint.
		let pad = Pad { id, source, handle, shape, state: shape.initial() };
		let arrived = GamepadEvent::Arrived(pad.record());
		self.gamepads.pads.push(pad);
		if self.protected {
			return;
		}
		let pads = &self.gamepads.pads;
		for slot in [&mut self.gamepads.focused, &mut self.gamepads.console] {
			if let Some(stream) = slot.as_mut()
				&& !(stream.offer_whole(pads, &arrived) && stream.offer_state(pads, id))
				&& let Some(stream) = slot.take()
			{
				stream.close(pads, true);
			}
		}
	}

	fn pad_state(&mut self, source: u64, frame: &pad_wire::StateFrame) {
		let Some(at) = self.gamepads.pads.iter().position(|pad| pad.source == source && pad.handle == frame.handle) else { return };
		let Some(state) = self.gamepads.pads[at].shape.state(frame) else { return };
		if self.gamepads.pads[at].state == state {
			return;
		}
		self.gamepads.pads[at].state = state;
		// TRACKED WHILE A PROTECTED SESSION IS UP, and delivered nowhere.
		if self.protected {
			return;
		}
		let id = self.gamepads.pads[at].id;
		let pads = &self.gamepads.pads;
		for slot in [&mut self.gamepads.focused, &mut self.gamepads.console] {
			if let Some(stream) = slot.as_mut()
				&& !stream.offer_state(pads, id)
				&& let Some(gone) = slot.take()
			{
				close(gone.producer);
			}
		}
	}

	fn pad_departed(&mut self, source: u64, handle: u32) {
		let Some(at) = self.gamepads.pads.iter().position(|pad| pad.source == source && pad.handle == handle) else { return };
		let pad = self.gamepads.pads.remove(at);
		if self.protected {
			return;
		}
		let pads = &self.gamepads.pads;
		for slot in [&mut self.gamepads.focused, &mut self.gamepads.console] {
			// A DEPARTING GAMEPAD'S PENDING STATE IS DROPPED FIRST: the reader releases it whole.
			if let Some(stream) = slot.as_mut() {
				stream.pending.remove(pad.id);
			}
			if let Some(stream) = slot.as_mut()
				&& !stream.offer_whole(pads, &GamepadEvent::Departed(pad.id))
				&& let Some(stream) = slot.take()
			{
				stream.close(pads, true);
			}
		}
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

	// AND THE TWO GAMEPAD STREAMS, served live off the serve loop by `stream_subscribe_gamepads` and
	// `stream_observe_gamepads` for the same reason.
	fn subscribe_gamepads(&mut self, focus: u64) -> Vec<GamepadEvent> {
		if focus != 0 {
			close(focus);
		}
		Vec::new()
	}

	fn observe_gamepads(&mut self) -> Vec<GamepadEvent> {
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

// THE BLUETOOTH INPUT DEVICES, AS MORE SOURCES OF EVERY KIND.
//
// Every other pointer reaches this service already folded by its driver into an absolute, normalised
// position, and every other keyboard's keys arrive on the merged key sink. A Bluetooth device has no driver
// in that sense - its reports arrive decoded from BluetoothService over the profile channel, one stream a
// device trusted for input, at most eight - so the fold and the routing happen here, and what comes out is
// what every other device produces: the pointer's five-byte record on the one pointer path, a key on the
// ordinary key path (NEVER the trusted sink: a Bluetooth keyboard's Ctrl+Alt+F12 stays reserved and is neither
// secure attention nor delivered), a brightness key on the system-key path, and a gamepad on the gamepad set,
// this stream its source.
//
// AND THE TEXT CONSOLE, which the ordinary key stream does not reach: its keystrokes come from the kernel's
// console input, which only a holder of a ConsoleInputSource feeds. This service holds one for the Bluetooth
// keyboards and feeds it through the drivers' own cooking - `drivers::keys`, the layout, the locks, the escapes
// and the reserved keys - one modifier and lock state a device, as each USB keyboard has its own. It holds no
// SystemPower connection, so a Bluetooth keyboard's Ctrl+Alt+Delete and Power key act on nothing. Nothing is
// fed while a protected session is up.
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
// never answers - this slot is simply empty, and every other device works as it always did.
struct Bluetooth {
	// The bootstrap channel, which is also the broker channel a resolve travels on.
	broker: u64,
	profile: u64,
	inputs: Vec<BluetoothInput>,
	retry_at: u64,
	// A resolve has been sent and not answered.
	resolving: bool,
	// The broker refused the name: this service is not granted it, and asking again would be asking
	// the same question for the same answer.
	refused: bool,
}

// One device's stream, and what this service keeps for it: its own pointer fold, its own modifier and lock
// state for the console, and the keyboard-page keys it holds, so its loss lets every one of them go.
struct BluetoothInput {
	controller: u32,
	peer: proto::system::PeerAddress,
	stream: u64,
	pointer: service_logic::hogp::Pointer,
	mods: drivers::keys::Mods,
	held: [u16; MAX_HELD],
	holding: usize,
}

// At most eight devices, as the milestone states; and the keys one of them may hold down at once.
const MAX_BLUETOOTH_INPUTS: usize = 8;
const MAX_HELD: usize = 16;

// How long to wait before trying again: two seconds. Long enough that a peer or a stack that is not
// there costs almost nothing, short enough that a person switching a device on does not wonder whether
// it worked.
const BLUETOOTH_RETRY_TICKS: u64 = 200;

impl Bluetooth {
	fn wanted(&self) -> bool {
		!self.refused
	}

	// The retry: ask the broker for the profile authority if there is none, or open a stream from every
	// trusted peer that has none.
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
		for peer in peers {
			if self.inputs.len() >= MAX_BLUETOOTH_INPUTS {
				break;
			}
			if self.inputs.iter().any(|input| input.controller == peer.controller && input.peer == peer.peer) {
				continue;
			}
			if let Some(Ok(stream)) = client.open_input(&peer.controller, &peer.peer) {
				self.inputs.push(BluetoothInput { controller: peer.controller, peer: peer.peer, stream, pointer: service_logic::hogp::Pointer::new(), mods: drivers::keys::Mods::default(), held: [0; MAX_HELD], holding: 0 });
			}
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

	// Drain one device's stream onto the paths every other device's input takes.
	fn drain(&mut self, at: usize, state: &mut Input, system_keys: &mut SystemKeys, forward: u64, buf: &mut [u8]) {
		let stream = self.inputs[at].stream;
		loop {
			match try_recv_caps(stream, buf) {
				PolledCaps::Message { len, handles } => {
					for &handle in handles.as_slice() {
						close(handle);
					}
					let mut frame = Handles::new();
					let Some(report) = proto::system::bluetooth_profile::open_input_read(&buf[..len], &mut frame) else { continue };
					self.inputs[at].deliver(report, state, system_keys, forward);
				}
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					// THE DEVICE IS GONE: every key it held goes up, its locks and modifiers with it, and each gamepad it
					// brought departs.
					let mut input = self.inputs.remove(at);
					input.release(state);
					close(input.stream);
					while let Some(handle) = state.gamepads.pads.iter().find(|pad| pad.source == stream).map(|pad| pad.handle) {
						state.pad_departed(stream, handle);
					}
					self.retry_at = clock().saturating_add(BLUETOOTH_RETRY_TICKS);
					return;
				}
			}
		}
	}
}

impl BluetoothInput {
	fn deliver(&mut self, report: proto::system::InputReport, state: &mut Input, system_keys: &mut SystemKeys, forward: u64) {
		use proto::system::InputReport;
		match report {
			InputReport::Pointer(report) => {
				let (x, y) = self.pointer.fold(report.dx, report.dy);
				let raw: [u8; RAW_LEN] = [x.to_le_bytes()[0], x.to_le_bytes()[1], y.to_le_bytes()[0], y.to_le_bytes()[1], report.buttons & 0x07];
				if let Some(event) = map_event(&raw) {
					state.record(event);
				}
				if forward != 0 && !state.protected {
					send_blocking(forward, &raw, 0);
				}
			}
			InputReport::Key(key) => {
				// THE KEYBOARD PAGE on the ordinary key path, held keys counted for the release a loss needs.
				if key.page == 0x07 {
					self.track(key.usage, key.down);
					state.record_key(&[key.usage as u8, (key.usage >> 8) as u8, u8::from(key.down)]);
				}
				// A SYSTEM KEY - the consumer page's brightness pair - on the system-key path, as a USB keyboard's.
				if key.page == 0x0c
					&& let Some(frame) = drivers::keys::system_key_frame(key.usage, key.down)
					&& let Some(Some(pressed)) = system_key(&frame)
				{
					system_keys.press(pressed);
				}
				// THE CONSOLE, through the drivers' own cooking - never while a protected session is up.
				if !state.protected {
					let code = drivers::keys::usage_keycode((u32::from(key.page) << 16) | u32::from(key.usage));
					if code != 0 {
						drivers::keys::feed_key(code, u32::from(key.down), &mut self.mods);
					}
				}
			}
			InputReport::Gamepad(frame) => state.pad_frame(self.stream, &frame.bytes),
		}
	}

	fn track(&mut self, usage: u16, down: bool) {
		let held = &mut self.held[..self.holding];
		match (down, held.iter().position(|key| *key == usage)) {
			(true, None) if self.holding < MAX_HELD => {
				self.held[self.holding] = usage;
				self.holding += 1;
			}
			(false, Some(at)) => {
				self.held[at] = self.held[self.holding - 1];
				self.holding -= 1;
			}
			_ => {}
		}
	}

	fn release(&mut self, state: &mut Input) {
		for at in 0..self.holding {
			let usage = self.held[at];
			state.record_key(&[usage as u8, (usage >> 8) as u8, 0]);
		}
		self.holding = 0;
		self.mods.release_all();
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
	// THE ACTIVITY SIGNAL'S ROOT: the sleep policy and the brightness policy watch it. Optional, and LAST.
	let activity_root: u64 = recv_tagged(bootstrap, &mut buf, b"ACTIVITY").unwrap_or(0);
	// THE SYSTEM KEYS' ROOT: DisplayService holds it, for the brightness. Optional, and LAST.
	let system_keys_root: u64 = recv_tagged(bootstrap, &mut buf, b"SYSKEYS").unwrap_or(0);
	// THE CONSOLE INPUT PRIVILEGE, delegated for the Bluetooth keyboards alone: the drivers' key cooking feeds the
	// console under it. Optional, and LAST; without it a Bluetooth keyboard types into no console.
	drivers::keys::set_console_input(recv_tagged(bootstrap, &mut buf, b"CONSOLE").unwrap_or(0));
	// THE `input` KIND IS FOLLOWED TOO - the virtio tablet's, whose driver publishes it - and it was taken ONCE, here,
	// and the subscription closed (found 2026-10-06, measuring the input drivers). A virtio-input pointer bound again -
	// after a crash, a kill, an operator's disable and enable - publishes a new provider, and nothing attached it: the
	// tablet moved nothing for the rest of the boot, and its driver, sending into a connection nobody read, parked in
	// its send once the queue held sixty-four events and stopped answering its heartbeat.
	let raw_pointers = Followed::subscribe(catalogue, &ProviderKind::Input, "raw pointer", MAX_POINTER_PROVIDERS);
	// POINTERS AND TOUCH SURFACES ARE FOLLOWED: both subscriptions stay open, and every provider they announce - at
	// bootstrap or later - is attached, up to four pointers and one surface. Three of the catalogue's subscriber places,
	// with the one above, held for this service's life, as the gamepad subscription below holds its own.
	let pointers = Followed::subscribe(catalogue, &ProviderKind::Pointer, "pointer", MAX_POINTER_PROVIDERS);
	let touch = Followed::subscribe(catalogue, &ProviderKind::Touch, "touch", MAX_TOUCH_SURFACES);
	// GAMEPADS ARE FOLLOWED, NOT TAKEN ONCE: this subscription stays open, and every gamepad provider it
	// announces - at bootstrap or later - is adopted from it in the serve loop.
	let pad_subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::Gamepad).unwrap_or(0) } else { 0 };
	// The Bluetooth slot starts empty and asks the broker once this service is online - see `Bluetooth`.
	let mut bluetooth = Bluetooth { broker: bootstrap, profile: 0, inputs: Vec::new(), retry_at: clock(), resolving: false, refused: false };

	// 2. report in to the supervisor that started us.
	{
		send_blocking(bootstrap, b"InputService: online", 0);
	}

	// 3. serve until the client side closes.
	let mut state: Input = Input::new(kill, Gamepads { catalogue, subscription: pad_subscription, sources: Vec::new(), pads: Vec::new(), next_id: 1, focused: None, console: None });
	let mut trusted = Trusted { watch: Watch::new(), arming: Arming::Off, producer: 0, seq: 0, protected: false };
	let mut activity = Activity { last: clock(), watchers: Vec::new() };
	serve(service, admin, raw_pointers, pointers, touch, forward, keys, focus, [trusted_keys, trusted_root, activity_root, system_keys_root], &mut trusted, &mut bluetooth, &mut state, &mut activity);
	exit();
}

// Serve loop: wait on the serve channel and the raw pointer-event channels at once.
// On each wake, drain every queued raw event into the cell-mapped ring, then handle
// one client request. Returns when the client side closes (no more clients). Once
// a raw channel closes (its pointer driver retired), it is detached and dropped from
// the wait set so a peer-closed channel cannot spin the loop.
#[allow(clippy::too_many_arguments)]
fn serve(service: u64, admin: u64, mut raw_pointers: Followed, mut pointers: Followed, mut touch: Followed, forward: u64, keys: u64, focus: u64, [trusted_keys, trusted_root, activity_root, system_keys_root]: [u64; 4], trusted: &mut Trusted, bluetooth: &mut Bluetooth, state: &mut Input, activity: &mut Activity) {
	let mut req: [u8; 64] = [0u8; 64];
	let mut clients: Vec<Client> = alloc::vec![Client { chan: service, scope: Scope::Full }];
	// THE ACTIVITY ROOT IS A CLIENT CHANNEL OF ITS OWN SCOPE, after the service root, whose index the loop keeps at zero.
	if activity_root != 0 {
		clients.push(Client { chan: activity_root, scope: Scope::Activity });
	}
	if system_keys_root != 0 {
		clients.push(Client { chan: system_keys_root, scope: Scope::SystemKeys });
	}
	let mut system_keys = SystemKeys::default();
	let mut keys_open: bool = keys != 0;
	let mut focus_open: bool = focus != 0;
	let mut trusted_keys_open: bool = trusted_keys != 0;
	let mut trusted_open: bool = trusted_root != 0;
	let mut pad_buf: [u8; GAMEPAD_FRAME] = [0; GAMEPAD_FRAME];
	loop {
		// EVERY WAKE SENDS WHAT THE GAMEPAD STREAMS ARE OWED, as room allows.
		state.flush_pads();
		let mut waitset: Vec<u64> = Vec::with_capacity(clients.len() + 5);
		if focus_open {
			waitset.push(focus);
		}
		if keys_open {
			waitset.push(keys);
		}
		// EVERY ATTACHED SURFACE AND POINTER, and their subscriptions, which announce the next.
		waitset.extend(touch.connections());
		waitset.extend(raw_pointers.connections());
		waitset.extend(pointers.connections());
		for subscription in [raw_pointers.subscription, pointers.subscription, touch.subscription] {
			if subscription != 0 {
				waitset.push(subscription);
			}
		}
		if admin != 0 {
			waitset.push(admin);
		}
		waitset.extend(bluetooth.inputs.iter().map(|input| input.stream));
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
		// THE GAMEPAD SUBSCRIPTION, EVERY GAMEPAD PROVIDER, and both gamepad streams - whose readers going away
		// is noticed here rather than at the next frame.
		if state.gamepads.subscription != 0 {
			waitset.push(state.gamepads.subscription);
		}
		waitset.extend(state.gamepads.sources.iter().map(|source| source.chan));
		waitset.extend(state.gamepads.focused.iter().chain(state.gamepads.console.iter()).map(|stream| stream.producer));
		waitset.extend(clients.iter().map(|client| client.chan));
		// A BLUETOOTH SLOT WITH NOTHING OPEN WAKES THIS LOOP ON ITS RETRY DEADLINE - unless a resolve is
		// already in flight, whose answer wakes it instead, or the broker has refused the name. With devices
		// open it still looks for more, on a housekeeping wake below.
		let pending_retry: bool = bluetooth.inputs.is_empty() && !bluetooth.resolving && bluetooth.wanted();
		let more_wanted: bool = !bluetooth.inputs.is_empty() && bluetooth.inputs.len() < MAX_BLUETOOTH_INPUTS && !bluetooth.resolving && bluetooth.wanted();
		// AND A GAMEPAD STREAM OWED A STATE WAKES IT A TICK LATER, on a HOUSEKEEPING wait: a reader that never
		// drains must not keep a settling scheduler from settling.
		let pad_retry: u64 = if state.pads_pending() { clock() + GAMEPAD_RETRY_TICKS } else { 0 };
		// AND THE ACTIVITY SIGNAL'S NEXT EDGE, a housekeeping wake: a watcher waiting minutes for idleness must not keep a
		// settling scheduler from settling.
		let idle_due: u64 = activity.due();
		let earliest = |a: u64, b: u64| {
			if a == 0 {
				b
			} else if b == 0 {
				a
			} else {
				a.min(b)
			}
		};
		let more_due: u64 = if more_wanted { bluetooth.retry_at } else { 0 };
		let ready: i64 = if pad_retry != 0 && (!pending_retry || pad_retry < bluetooth.retry_at) {
			wait_any_periodic(&waitset, earliest(earliest(pad_retry, idle_due), more_due))
		} else if pending_retry {
			wait_any(&waitset, earliest(bluetooth.retry_at, idle_due))
		} else if idle_due != 0 || more_due != 0 {
			wait_any_periodic(&waitset, earliest(idle_due, more_due))
		} else {
			wait_any(&waitset, 0)
		};
		activity.tick();
		if (pending_retry || more_wanted) && clock() >= bluetooth.retry_at {
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
						activity.seen();
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
		if state.gamepads.subscription != 0 && ready_handle == state.gamepads.subscription {
			loop {
				let (len, handles) = match try_recv_caps(state.gamepads.subscription, &mut pad_buf) {
					PolledCaps::Message { len, handles } => (len, handles),
					PolledCaps::Empty => break,
					PolledCaps::Closed => {
						close(state.gamepads.subscription);
						state.gamepads.subscription = 0;
						break;
					}
				};
				for &leftover in handles.as_slice() {
					close(leftover);
				}
				let mut frame_handles = wire::Handles::new();
				let Some(info) = provider_catalogue::subscribe_read(&pad_buf[..len], &mut frame_handles) else { continue };
				if info.live {
					state.adopt_pad_source(info);
				} else if let Some(chan) = state.gamepads.sources.iter().find(|source| same_publication(&source.info, &info)).map(|source| source.chan) {
					state.lose_pad_source(chan);
				}
			}
			continue;
		}
		if state.gamepads.sources.iter().any(|source| source.chan == ready_handle) {
			loop {
				match try_recv(ready_handle, &mut pad_buf) {
					Polled::Message { len, handle } => {
						if handle != 0 {
							close(handle);
						}
						state.pad_frame(ready_handle, &pad_buf[..len]);
						activity.seen();
					}
					Polled::Empty => break,
					Polled::Closed => {
						state.lose_pad_source(ready_handle);
						break;
					}
				}
			}
			continue;
		}
		// A GAMEPAD STREAM'S READER WENT AWAY: the stream is dropped, with nobody left to release anything to.
		let mut was_pad_stream = false;
		for slot in [&mut state.gamepads.focused, &mut state.gamepads.console] {
			if slot.as_ref().is_some_and(|stream| stream.producer == ready_handle) {
				was_pad_stream = true;
				match try_recv(ready_handle, &mut pad_buf) {
					Polled::Closed => {
						if let Some(gone) = slot.take() {
							close(gone.producer);
						}
					}
					Polled::Message { handle, .. } if handle != 0 => close(handle),
					_ => {}
				}
			}
		}
		if was_pad_stream {
			continue;
		}
		if let Some(at) = bluetooth.inputs.iter().position(|input| input.stream == ready_handle) {
			activity.seen();
			let mut frame_buf: [u8; 256] = [0u8; 256];
			bluetooth.drain(at, state, &mut system_keys, forward, &mut frame_buf);
			continue;
		}
		if focus_open && ready_handle == focus {
			let acknowledged: bool = match recv_blocking(focus, &mut req) {
				Received::Message { len, handle } if len >= 3 && &req[..3] == b"SET" && handle != 0 => {
					state.notify_console(false, forward);
					state.set_focus(handle);
					// A SURFACE TOOK FOCUS: a console program's gamepad stream is released and closed, so a
					// background program cannot watch a game's input.
					state.close_console_pads();
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
		// A PUBLICATION OR A WITHDRAWAL: a pointer attached or detached; a surface detached lifts what it held.
		if pointers.subscription != 0 && ready_handle == pointers.subscription {
			pointers.drain();
			continue;
		}
		if raw_pointers.subscription != 0 && ready_handle == raw_pointers.subscription {
			raw_pointers.drain();
			continue;
		}
		if touch.subscription != 0 && ready_handle == touch.subscription {
			if touch.drain() {
				state.lift_contacts();
			}
			continue;
		}
		if touch.holds(ready_handle) {
			loop {
				match try_recv(ready_handle, &mut req) {
					Polled::Message { len, handle } => {
						if handle != 0 {
							close(handle);
						}
						if let Some(contact) = contact_from(&req[..len]) {
							state.record_contact(contact);
						}
						activity.seen();
					}
					Polled::Empty => break,
					Polled::Closed => {
						touch.detach(ready_handle);
						state.lift_contacts();
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
						// A SYSTEM KEY GOES TO ITS WATCHER AND NOWHERE ELSE, before anything ordinary delivery checks.
						match system_key(&req[..len]) {
							Some(Some(key)) => system_keys.press(key),
							Some(None) => {}
							None => state.record_key(&req[..len]),
						}
						activity.seen();
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
		// A POINTER'S EVENTS - an attached `input` kind's or `pointer` kind's - merged into the one cursor.
		if raw_pointers.holds(ready_handle) || pointers.holds(ready_handle) {
			loop {
				match try_recv(ready_handle, &mut req) {
					Polled::Message { len, .. } => {
						if let Some(event) = map_event(&req[..len]) {
							state.record(event);
						}
						activity.seen();
						if forward != 0 && !state.protected {
							send_blocking(forward, &req[..len], 0);
						}
					}
					Polled::Empty => break,
					Polled::Closed => {
						if raw_pointers.holds(ready_handle) {
							raw_pointers.detach(ready_handle);
						} else {
							pointers.detach(ready_handle);
						}
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
				if op == CONNECT_OP && (scope == Scope::Full || scope == Scope::Activity || scope == Scope::SystemKeys) {
					if let Some((mine, theirs)) = channel() {
						clients.push(Client { chan: mine, scope });
						send_blocking(client, &[], theirs);
					}
				} else if scope == Scope::SystemKeys {
					// THE SYSTEM KEYS' CONNECTIONS answer their one operation and nothing else.
					if op == proto::system::system_keys::OP_WATCH {
						system_keys.open(client, &req[..len]);
					} else if len >= 6 {
						send_blocking(client, &req[2..6], 0);
					}
				} else if scope == Scope::Activity {
					// THE ACTIVITY SIGNAL'S CONNECTIONS answer its one operation and nothing else.
					if op == proto::system::input_activity::OP_WATCH {
						stream_watch_activity(client, &req[..len], activity);
					} else if len >= 6 {
						send_blocking(client, &req[2..6], 0);
					}
				} else if op == input::OP_SUBSCRIBE && scope == Scope::Full {
					stream_subscribe(client, &req[..len], state);
				} else if op == input::OP_SUBSCRIBE_KEYS && scope != Scope::Gamepad {
					stream_subscribe_keys(client, &req[..len], &mut handle, state);
				} else if op == input::OP_SUBSCRIBE_CONTACTS && scope != Scope::Gamepad {
					stream_subscribe_contacts(client, &req[..len], &mut handle, state);
				} else if op == input::OP_SUBSCRIBE_GAMEPADS && scope != Scope::Gamepad {
					stream_subscribe_gamepads(client, &req[..len], &mut handle, state);
				} else if op == input::OP_OBSERVE_GAMEPADS && scope == Scope::Gamepad {
					stream_observe_gamepads(client, &req[..len], state);
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
				for slot in [&mut state.gamepads.focused, &mut state.gamepads.console] {
					if slot.as_ref().is_some_and(|stream| stream.owner == client)
						&& let Some(gone) = slot.take()
					{
						close(gone.producer);
					}
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

// THE GAMEPADS OF A GRAPHICAL APPLICATION, on the same one-shot proof `subscribe-keys` takes and with the
// same refusal - the correlation id and no capability. One stream is live: a valid subscription replaces it,
// the old one released first. Refused while a protected session is up.
fn stream_subscribe_gamepads(service: u64, request: &[u8], request_handle: &mut u64, state: &mut Input) {
	if request.len() != 10 || *request_handle == 0 {
		return;
	}
	let corr: u32 = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
	let proof: u64 = core::mem::take(request_handle);
	let valid: bool = !state.protected && state.validate_focus(proof);
	close(proof);
	if !valid {
		send_blocking(service, &corr.to_le_bytes(), 0);
		return;
	}
	if let Some(old) = state.gamepads.focused.take() {
		old.close(&state.gamepads.pads, true);
	}
	state.gamepads.focused = open_pad_stream(service, corr, &state.gamepads.pads);
}

// THE GAMEPADS OF A CONSOLE PROGRAM, on its gamepad-scope connection: refused while a graphical surface holds
// focus, while a protected session is up, and while another console stream is open - which it is never
// displaced by.
fn stream_observe_gamepads(service: u64, request: &[u8], state: &mut Input) {
	if request.len() != 6 {
		return;
	}
	let corr: u32 = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
	if state.protected || state.focus_peer != 0 || state.gamepads.console.is_some() {
		send_blocking(service, &corr.to_le_bytes(), 0);
		return;
	}
	state.gamepads.console = open_pad_stream(service, corr, &state.gamepads.pads);
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
