// DISPLAY BRIGHTNESS, as DisplayService owns it: the backlights the provider catalogue publishes, which of them belongs to
// output 0 and why (`service_logic::brightness::join`), the floor, the two brightness keys InputService hands this
// service alone, the firmware's hotkeys, and the two roots - `BRIGHTNESS`, the read every holder of the `brightness`
// capability reaches, and `BRIGHTNESSCTL`, the set the brightness policy alone holds.
//
// A BACKLIGHT CALL IS BOUNDED, NOT ASYNCHRONOUS. The plan asked for each call to be issued and its answer awaited in the
// loop's wait set; the generated clients are synchronous, so a call waits here - for `CALL_TICKS` at the most, which no
// driver answering a register write comes near - and a provider that misses it is marked failed and left out of the join
// until its stream speaks again. The loop is held for a tenth of a second by a broken provider, never for ever.
//
// EVERY HANDLE THIS MODULE WAITS ON IS DISPATCHED BY HANDLE, before the loop reads an index, and appended after every
// other wait - so nothing it adds or drops moves the positions the rest of the loop decodes.

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::codec::Handles;
use proto::system::{BacklightEvent, BacklightHotkey, BacklightScale, BacklightSource, BacklightState, BacklightTarget, BrightnessCause, BrightnessChange, BrightnessOutput, BrightnessRange, BrightnessScale, BrightnessSet, BrightnessTarget, Error, JoinReason, OutputSource, PciFunction, ProviderInfo, ProviderKind, SystemKey, backlight, display_brightness, display_brightness_control, provider_catalogue, system_keys};
use rt::*;
use service_logic::brightness as logic;

// How long one backlight call may take before its provider is marked failed: a tenth of a second.
const CALL_TICKS: u64 = 10;
// How far a subscriber may fall behind before its stream is closed rather than losing a change silently.
const SUBSCRIBER_DEPTH: u64 = 64;

pub(super) struct Brightness {
	catalogue: u64,
	subscription: u64,
	backlights: Vec<Backlight>,
	source: logic::OutputSource,
	places: Vec<logic::Place>,
	serial: u64,
	reads: Vec<u64>,
	controls: Vec<u64>,
	// The two roots themselves, which a request they do not carry never closes.
	roots: [u64; 2],
	subscribers: Vec<Subscriber>,
	keys: u64,
	last_step: logic::LastStep,
	said_none: bool,
}

struct Backlight {
	info: ProviderInfo,
	chan: u64,
	events: u64,
	key: String,
	kind: logic::Kind,
	target: logic::BacklightTarget,
	scale: logic::Scale,
	ac_default: Option<u32>,
	battery_default: Option<u32>,
	level: Option<u32>,
	failed: bool,
	// Whether anything has moved the level since the backlight appeared.
	touched: bool,
}

struct Subscriber {
	stream: u64,
	seq: u32,
}

impl Brightness {
	// `catalogue` is this service's catalogue connection, which also answers the display kind; `keys` the client end of
	// InputService's `SYSKEYS` root. Every one of them may be zero, and brightness is then the smaller thing it can be.
	pub(super) fn new(catalogue: u64, read_root: u64, control_root: u64, keys: u64) -> Brightness {
		let subscription = if catalogue == 0 { 0 } else { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::Backlight).unwrap_or(0) };
		let keys = if keys == 0 { 0 } else { system_keys::Client::new(ChannelTransport { chan: keys }).watch().unwrap_or(0) };
		let mut brightness = Brightness { catalogue, subscription, backlights: Vec::new(), source: logic::OutputSource::BootFramebuffer(None), places: Vec::new(), serial: 0, reads: if read_root != 0 { alloc::vec![read_root] } else { Vec::new() }, controls: if control_root != 0 { alloc::vec![control_root] } else { Vec::new() }, roots: [read_root, control_root], subscribers: Vec::new(), keys, last_step: logic::LastStep::default(), said_none: false };
		brightness.follow_catalogue();
		brightness
	}

	// THE OUTPUT'S SOURCE CHANGED - a provider adopted, a rebind, the fall-through to the boot framebuffer - and the join
	// runs again.
	pub(super) fn set_source(&mut self, source: logic::OutputSource) {
		if self.source != source {
			self.source = source;
			self.rejoin();
		}
	}

	// Every handle this module waits on.
	pub(super) fn handles(&self, out: &mut Vec<u64>) {
		if self.subscription != 0 {
			out.push(self.subscription);
		}
		if self.keys != 0 {
			out.push(self.keys);
		}
		for backlight in &self.backlights {
			if backlight.events != 0 {
				out.push(backlight.events);
			}
			out.push(backlight.chan);
		}
		out.extend(self.reads.iter().copied());
		out.extend(self.controls.iter().copied());
	}

	// One ready handle, if it is one of this module's: answers whether it was.
	pub(super) fn ready(&mut self, handle: u64, request: &mut [u8], reply: &mut [u8]) -> bool {
		if handle == self.subscription {
			self.follow_catalogue();
		} else if handle == self.keys {
			self.take_keys(request);
		} else if let Some(at) = self.backlights.iter().position(|backlight| backlight.events == handle) {
			self.take_events(at, request);
		} else if let Some(at) = self.backlights.iter().position(|backlight| backlight.chan == handle) {
			// NOTHING ARRIVES HERE UNASKED: an answer is read by the call that made it. What this wait notices is the
			// provider going away.
			if matches!(try_recv(handle, request), Polled::Closed) {
				self.remove(at);
			}
		} else if let Some(at) = self.reads.iter().position(|&read| read == handle) {
			if !self.serve_read(handle, request, reply) {
				close(handle);
				self.reads.remove(at);
			}
		} else if let Some(at) = self.controls.iter().position(|&control| control == handle) {
			if !self.serve_control(handle, request, reply) {
				close(handle);
				self.controls.remove(at);
			}
		} else {
			return false;
		}
		true
	}

	// ------------------------------------------------------------------------------------------ the catalogue

	// Every publication the subscription has queued: a backlight appearing is opened and described, one withdrawn
	// leaves.
	fn follow_catalogue(&mut self) {
		let mut buf = [0u8; 512];
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
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut frame_handles = Handles::new();
			let Some(info) = provider_catalogue::subscribe_read(&buf[..len], &mut frame_handles) else { continue };
			if info.kind != ProviderKind::Backlight {
				continue;
			}
			let same = |held: &ProviderInfo| held.slot == info.slot && held.provider_generation == info.provider_generation && held.binding_generation == info.binding_generation;
			match (info.live, self.backlights.iter().position(|backlight| same(&backlight.info))) {
				(true, None) => self.add(info),
				(false, Some(at)) => self.remove(at),
				_ => {}
			}
		}
	}

	fn add(&mut self, info: ProviderInfo) {
		let chan = super::open_provider(self.catalogue, &info);
		if chan == 0 {
			return;
		}
		let mut client = backlight::Client::with_deadline(ChannelTransport { chan }, clock() + CALL_TICKS);
		let Some(Ok(description)) = client.describe() else {
			print(b"DisplayService: a backlight did not describe itself in time; it is left out\n");
			close(chan);
			return;
		};
		let scale = match description.scale {
			BacklightScale::Levels(levels) => logic::Scale::new(levels),
			BacklightScale::Range(range) => logic::Scale::range(range.minimum, range.maximum),
		};
		let Some(scale) = scale else {
			print(b"DisplayService: a backlight described no levels it can be set to; it is left out\n");
			close(chan);
			return;
		};
		client.set_deadline(clock() + CALL_TICKS);
		let level = match client.get() {
			Some(Ok(level)) => Some(level),
			_ => None,
		};
		client.set_deadline(clock() + CALL_TICKS);
		let events = client.events().unwrap_or(0);
		let kind = match description.source {
			BacklightSource::Firmware => logic::Kind::Firmware,
			BacklightSource::UsbMonitor => logic::Kind::UsbMonitor,
			BacklightSource::Native => logic::Kind::Native,
		};
		let target = match description.target {
			BacklightTarget::Function(function) => logic::BacklightTarget::Function(logic::Function { bus: function.bus, dev: function.dev, func: function.func }),
			BacklightTarget::Monitor(monitor) => logic::BacklightTarget::Monitor(logic::Monitor { manufacturer: monitor.manufacturer, product: monitor.product, serial: monitor.serial }),
			BacklightTarget::None => logic::BacklightTarget::None,
		};
		print(b"DisplayService: backlight ");
		print(description.key.as_bytes());
		print(b" appeared\n");
		self.backlights.push(Backlight { info, chan, events, key: description.key, kind, target, scale, ac_default: description.ac_default, battery_default: description.battery_default, level, failed: false, touched: false });
		self.rejoin();
		let at = self.backlights.len() - 1;
		self.change(at, BrightnessCause::Appeared);
	}

	fn remove(&mut self, at: usize) {
		let gone = self.backlights.remove(at);
		close(gone.chan);
		if gone.events != 0 {
			close(gone.events);
		}
		self.serial += 1;
		let change = BrightnessChange { key: gone.key, level: None, cause: BrightnessCause::Left, serial: self.serial };
		self.rejoin();
		self.publish(&change);
	}

	// THE JOIN, AGAIN: whenever a backlight appears, leaves or fails, or the output's source changes.
	fn rejoin(&mut self) {
		let output = logic::Output { source: self.source, edid: None };
		let candidates: Vec<logic::Candidate> = self.backlights.iter().map(|backlight| logic::Candidate { kind: backlight.kind, target: backlight.target, publisher: logic::Function { bus: backlight.info.bus, dev: backlight.info.dev, func: backlight.info.func }, failed: backlight.failed }).collect();
		self.places = logic::join(&output, &candidates);
	}

	fn active(&self) -> Option<usize> {
		self.places.iter().position(|place| place.standing == logic::Standing::Active)
	}

	// ------------------------------------------------------------------------------------------ changes

	// Number one backlight's change and send it to every subscriber.
	fn change(&mut self, at: usize, cause: BrightnessCause) -> u64 {
		self.serial += 1;
		let change = BrightnessChange { key: self.backlights[at].key.clone(), level: self.backlights[at].level, cause, serial: self.serial };
		self.publish(&change);
		self.serial
	}

	// A SUBSCRIBER THAT CANNOT TAKE A CHANGE HAS ITS STREAM CLOSED, rather than losing one backlight's last change
	// without a word: it subscribes again for a fresh snapshot.
	fn publish(&mut self, change: &BrightnessChange) {
		for subscriber in self.subscribers.iter_mut() {
			if !send_change(subscriber, change) {
				close(subscriber.stream);
				subscriber.stream = 0;
			}
		}
		self.subscribers.retain(|subscriber| subscriber.stream != 0);
	}

	// ------------------------------------------------------------------------------------------ setting

	fn set(&mut self, key: &str, target: logic::Target, allow_zero: bool, cause: BrightnessCause) -> Result<BrightnessSet, Error> {
		let at = self.backlights.iter().position(|backlight| backlight.key == key).ok_or(Error::NotFound)?;
		// A SHADOWED BACKLIGHT IS REFUSED: two writers on one panel is what the join's one active backlight prevents.
		// The vocabulary has no `busy`; `denied` is the standing answer until the join changes.
		if matches!(self.places.get(at).map(|place| place.standing), Some(logic::Standing::Shadowed(_))) {
			return Err(Error::Denied);
		}
		if self.backlights[at].failed {
			return Err(Error::Closed);
		}
		let level = logic::resolve(&self.backlights[at].scale, self.backlights[at].level, target, allow_zero).map_err(|refusal| match refusal {
			logic::Refusal::Invalid => Error::Invalid,
			logic::Refusal::Unknown => Error::Again,
		})?;
		self.apply(at, level, cause)
	}

	fn apply(&mut self, at: usize, level: u32, cause: BrightnessCause) -> Result<BrightnessSet, Error> {
		let chan = self.backlights[at].chan;
		match backlight::Client::with_deadline(ChannelTransport { chan }, clock() + CALL_TICKS).set(&level) {
			Some(Ok(set)) => {
				self.backlights[at].level = Some(set);
				self.backlights[at].touched = true;
				let serial = self.change(at, cause);
				Ok(BrightnessSet { level: set, serial })
			}
			Some(Err(error)) => Err(error),
			None => {
				self.fail(at);
				Err(Error::TimedOut)
			}
		}
	}

	// A PROVIDER THAT MISSED ITS DEADLINE is left out of the join until its stream speaks again.
	fn fail(&mut self, at: usize) {
		if !self.backlights[at].failed {
			self.backlights[at].failed = true;
			print(b"DisplayService: backlight ");
			print(self.backlights[at].key.as_bytes());
			print(b" did not answer in time; it is left out until it speaks again\n");
			self.rejoin();
		}
	}

	// A STEP ON OUTPUT 0'S ACTIVE BACKLIGHT, from a key or a firmware hotkey: one press, one step, the second source's
	// copy of the same press dropped.
	fn step(&mut self, source: logic::Source, key: logic::Hotkey, cause: BrightnessCause) {
		let up = !matches!(key, logic::Hotkey::Down | logic::Hotkey::Zero);
		if self.last_step.duplicate(source, up, clock()) {
			return;
		}
		let Some(at) = self.active() else {
			if !self.said_none {
				print(b"DisplayService: a brightness step with no backlight on the output; nothing to do\n");
				self.said_none = true;
			}
			return;
		};
		let Some(level) = logic::hotkey(&self.backlights[at].scale, self.backlights[at].level, key) else { return };
		let _ = self.apply(at, level, cause);
	}

	fn take_keys(&mut self, request: &mut [u8]) {
		loop {
			let (len, handles) = match try_recv_caps(self.keys, request) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					close(self.keys);
					self.keys = 0;
					return;
				}
			};
			let mut frame_handles = handles;
			let Some(key) = system_keys::watch_read(&request[..len], &mut frame_handles) else { continue };
			let key = match key {
				SystemKey::BrightnessUp => logic::Hotkey::Up,
				SystemKey::BrightnessDown => logic::Hotkey::Down,
			};
			self.step(logic::Source::Key, key, BrightnessCause::Key);
		}
	}

	fn take_events(&mut self, at: usize, request: &mut [u8]) {
		let events = self.backlights[at].events;
		loop {
			let (len, handles) = match try_recv_caps(events, request) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					close(events);
					self.backlights[at].events = 0;
					return;
				}
			};
			let mut frame_handles = handles;
			let Some(event) = backlight::events_read(&request[..len], &mut frame_handles) else { continue };
			// A STREAM THAT SPEAKS IS A PROVIDER THAT ANSWERS again.
			if self.backlights[at].failed {
				self.backlights[at].failed = false;
				self.rejoin();
			}
			match event {
				BacklightEvent::Level(level) => {
					self.backlights[at].level = Some(level);
					self.backlights[at].touched = true;
					self.change(at, BrightnessCause::Device);
				}
				// A FIRMWARE HOTKEY ACTS ON THE ACTIVE BACKLIGHT, whichever reported it: the firmware raises it on its
				// panel's output, which is the active one wherever the join is right.
				BacklightEvent::Hotkey(key) => {
					let key = match key {
						BacklightHotkey::Up => logic::Hotkey::Up,
						BacklightHotkey::Down => logic::Hotkey::Down,
						BacklightHotkey::Cycle => logic::Hotkey::Cycle,
						BacklightHotkey::Zero => logic::Hotkey::Zero,
					};
					self.step(logic::Source::Hotkey, key, BrightnessCause::FirmwareHotkey);
				}
			}
		}
	}

	// ------------------------------------------------------------------------------------------ the roots

	fn snapshot(&self) -> Vec<BacklightState> {
		self.backlights
			.iter()
			.enumerate()
			.map(|(at, backlight)| {
				let place = self.places.get(at).copied().unwrap_or(logic::Place { reason: None, standing: logic::Standing::Unjoined });
				BacklightState {
					key: backlight.key.clone(),
					kind: match backlight.kind {
						logic::Kind::Firmware => proto::system::BacklightKind::Firmware,
						logic::Kind::UsbMonitor => proto::system::BacklightKind::UsbMonitor,
						logic::Kind::Native => proto::system::BacklightKind::Native,
					},
					output: place.reason.map(|_| 0),
					reason: place.reason.map(|reason| match reason {
						logic::Reason::Native => JoinReason::Native,
						logic::Reason::FirmwareAdapter => JoinReason::FirmwareAdapter,
						logic::Reason::Edid => JoinReason::Edid,
						logic::Reason::SingleOutput => JoinReason::SingleOutput,
					}),
					standing: match place.standing {
						logic::Standing::Active => proto::system::BacklightStanding::Active,
						logic::Standing::Shadowed(_) => proto::system::BacklightStanding::Shadowed,
						logic::Standing::Unjoined => proto::system::BacklightStanding::Unjoined,
					},
					shadowed_by: match place.standing {
						logic::Standing::Shadowed(winner) => self.backlights.get(winner).map(|winner| winner.key.clone()),
						_ => None,
					},
					scale: match &backlight.scale {
						logic::Scale::Levels(levels) => BrightnessScale::Levels(levels.clone()),
						logic::Scale::Range { minimum, maximum } => BrightnessScale::Range(BrightnessRange { minimum: *minimum, maximum: *maximum }),
					},
					level: backlight.level,
					floor: backlight.scale.floor(),
					ac_default: backlight.ac_default,
					battery_default: backlight.battery_default,
					failed: backlight.failed,
					touched: backlight.touched,
				}
			})
			.collect()
	}

	fn output(&self) -> BrightnessOutput {
		let function = |function: logic::Function| PciFunction { bus: function.bus, dev: function.dev, func: function.func };
		BrightnessOutput {
			id: 0,
			source: match self.source {
				logic::OutputSource::Provider(publisher) => OutputSource::Provider(function(publisher)),
				logic::OutputSource::BootFramebuffer(decoder) => OutputSource::BootFramebuffer(decoder.map(function)),
			},
			edid: None,
		}
	}

	// ONE REQUEST ON A READ CHANNEL - the root or a connection minted from it. False when the channel closed.
	fn serve_read(&mut self, channel: u64, request: &mut [u8], reply: &mut [u8]) -> bool {
		let Received::Message { len, handle } = recv_blocking(channel, request) else { return false };
		if handle != 0 {
			close(handle);
		}
		let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
		if op == HEARTBEAT_OP {
			send_blocking(channel, b"PONG", 0);
		} else if op == CONNECT_OP {
			match rt::channel() {
				Some((mine, theirs)) => {
					self.reads.push(mine);
					send_blocking(channel, &[], theirs);
				}
				None => {
					send_blocking(channel, &[], 0);
				}
			}
		} else if op == display_brightness::OP_SUBSCRIBE {
			self.open_subscription(channel, &request[..len]);
		} else {
			let mut reply_handle = Handles::new();
			let mut request_handle = Handles::new();
			let snapshot = self.snapshot();
			let output = self.output();
			// A REQUEST THE READ DOES NOT CARRY - a set framed for the control root among them - ends the connection.
			let Some(n) = display_brightness::dispatch(&mut ReadCall { snapshot, output }, &request[..len], &mut request_handle, reply, &mut reply_handle) else { return self.roots.contains(&channel) };
			send_blocking(channel, &reply[..n], 0);
		}
		true
	}

	// A SUBSCRIPTION: one `snapshot` change per backlight on a fresh stream, then every change as it happens.
	fn open_subscription(&mut self, channel: u64, request: &[u8]) {
		let mut request_handle = Handles::new();
		let snapshot = self.snapshot();
		let mut asked = ReadCall { snapshot: Vec::new(), output: self.output() };
		let Some((corr, _)) = display_brightness::subscribe_open(&mut asked, request, &mut request_handle) else { return };
		let Some((producer, consumer)) = channel_with_depth(SUBSCRIBER_DEPTH) else {
			send_blocking(channel, &corr.to_le_bytes(), 0);
			return;
		};
		let mut subscriber = Subscriber { stream: producer, seq: 0 };
		for state in &snapshot {
			let change = BrightnessChange { key: state.key.clone(), level: state.level, cause: BrightnessCause::Snapshot, serial: self.serial };
			if !send_change(&mut subscriber, &change) {
				break;
			}
		}
		if !send_blocking(channel, &corr.to_le_bytes(), consumer) {
			close(producer);
			return;
		}
		self.subscribers.push(subscriber);
	}

	fn serve_control(&mut self, channel: u64, request: &mut [u8], reply: &mut [u8]) -> bool {
		let Received::Message { len, handle } = recv_blocking(channel, request) else { return false };
		if handle != 0 {
			close(handle);
		}
		let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
		if op == HEARTBEAT_OP {
			send_blocking(channel, b"PONG", 0);
		} else if op == CONNECT_OP {
			match rt::channel() {
				Some((mine, theirs)) => {
					self.controls.push(mine);
					send_blocking(channel, &[], theirs);
				}
				None => {
					send_blocking(channel, &[], 0);
				}
			}
		} else {
			let mut reply_handle = Handles::new();
			let mut request_handle = Handles::new();
			let Some(n) = display_brightness_control::dispatch(&mut ControlCall { brightness: self }, &request[..len], &mut request_handle, reply, &mut reply_handle) else { return self.roots.contains(&channel) };
			send_blocking(channel, &reply[..n], 0);
		}
		true
	}
}

fn send_change(subscriber: &mut Subscriber, change: &BrightnessChange) -> bool {
	let mut frame = [0u8; 512];
	let mut frame_handles = Handles::new();
	let sent = display_brightness::subscribe_frame(subscriber.seq, change, &mut frame, &mut frame_handles).is_some_and(|n| try_send(subscriber.stream, &frame[..n], 0));
	subscriber.seq = subscriber.seq.wrapping_add(1);
	sent
}

struct ReadCall {
	snapshot: Vec<BacklightState>,
	output: BrightnessOutput,
}

impl display_brightness::Service for ReadCall {
	fn outputs(&mut self) -> Result<Vec<BrightnessOutput>, Error> {
		Ok(alloc::vec![self.output.clone()])
	}

	fn backlights(&mut self) -> Result<Vec<BacklightState>, Error> {
		Ok(core::mem::take(&mut self.snapshot))
	}

	fn subscribe(&mut self) -> Vec<BrightnessChange> {
		Vec::new()
	}
}

struct ControlCall<'a> {
	brightness: &'a mut Brightness,
}

impl display_brightness_control::Service for ControlCall<'_> {
	fn set(&mut self, key: String, target: BrightnessTarget, allow_zero: bool) -> Result<BrightnessSet, Error> {
		let target = match target {
			BrightnessTarget::Level(level) => logic::Target::Level(level),
			BrightnessTarget::Percent(percent) => logic::Target::Percent(percent),
			BrightnessTarget::Steps(steps) => logic::Target::Steps(steps),
		};
		self.brightness.set(&key, target, allow_zero, BrightnessCause::Client)
	}
}
