// DISPLAY BRIGHTNESS, as DisplayService owns it: the backlights the provider catalogue publishes, which of them belongs to
// output 0 and why (`service_logic::brightness::join`), the floor, the two brightness keys InputService hands this
// service alone, the firmware's hotkeys, and the two roots - `BRIGHTNESS`, the read every holder of the `brightness`
// capability reaches, and `BRIGHTNESSCTL`, the set the brightness policy alone holds.
//
// Provider admission and level writes are issued without waiting. Each reply is correlated in the display wait set;
// deadlines remove an unresponsive provider from the join. A late reply cannot complete a newer request.
//
// EVERY HANDLE THIS MODULE WAITS ON IS DISPATCHED BY HANDLE, before the loop reads an index, and appended after every
// other wait - so nothing it adds or drops moves the positions the rest of the loop decodes.

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::codec::Handles;
use proto::system::{BacklightDescription, BacklightEvent, BacklightHotkey, BacklightScale, BacklightSource, BacklightState, BacklightTarget, BrightnessCause, BrightnessChange, BrightnessOutput, BrightnessRange, BrightnessScale, BrightnessSet, BrightnessTarget, Error, JoinReason, OutputSource, PciFunction, ProviderInfo, ProviderKind, SystemKey, backlight, display_brightness, display_brightness_control, provider_catalogue, system_keys};
use rt::*;
use service_logic::brightness as logic;
use wire::Sink;

// How long one backlight call may take before its provider is marked failed: a tenth of a second.
const CALL_TICKS: u64 = 10;
// How far a subscriber may fall behind before its stream is closed rather than losing a change silently.
const SUBSCRIBER_DEPTH: u64 = 64;

pub(super) struct Brightness {
	catalogue: u64,
	subscription: u64,
	backlights: Vec<Backlight>,
	source: logic::OutputSource,
	edid: Option<logic::Monitor>,
	pending: Vec<Pending>,
	next_corr: u32,
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
	firmware_display_id: Option<u32>,
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

enum Call {
	Open(ProviderInfo),
	Describe(ProviderInfo),
	DisplayId(ProviderInfo, BacklightDescription),
	Get(ProviderInfo, BacklightDescription, Option<u32>),
	Events(ProviderInfo, BacklightDescription, Option<u32>, Option<u32>),
	Set { client: Option<(u64, u32)>, cause: BrightnessCause },
}

struct Pending {
	chan: u64,
	corr: u32,
	deadline: u64,
	call: Call,
}

impl Pending {
	fn publication(&self) -> Option<&ProviderInfo> {
		match &self.call {
			Call::Open(info) | Call::Describe(info) | Call::DisplayId(info, _) | Call::Get(info, _, _) | Call::Events(info, _, _, _) => Some(info),
			Call::Set { .. } => None,
		}
	}
}

// Size a ready message from the channel itself: keys and level lists are variable-length wire fields.
// There is one receiver per channel, and neither the peek nor the receive waits.
fn receive(channel: u64) -> Result<Option<(Vec<u8>, Handles)>, ()> {
	let size = channel_peek(channel);
	if size == ERR_WOULD_BLOCK {
		return Ok(None);
	}
	if size < 0 {
		return Err(());
	}
	let mut bytes = Vec::new();
	bytes.try_reserve_exact(size as usize).map_err(|_| ())?;
	bytes.resize(size as usize, 0);
	match try_recv_caps(channel, &mut bytes) {
		PolledCaps::Message { len, handles } => {
			bytes.truncate(len);
			Ok(Some((bytes, handles)))
		}
		PolledCaps::Empty => Ok(None),
		PolledCaps::Closed => Err(()),
	}
}

fn same_publication(a: &ProviderInfo, b: &ProviderInfo) -> bool {
	a.slot == b.slot && a.provider_generation == b.provider_generation && a.binding_generation == b.binding_generation
}

impl Brightness {
	// `catalogue` is a separate backlight-only connection, so display RPCs cannot consume its replies; `keys` the client end of
	// InputService's `SYSKEYS` root. Every one of them may be zero, and brightness is then the smaller thing it can be.
	pub(super) fn new(catalogue: u64, read_root: u64, control_root: u64, keys: u64) -> Brightness {
		let subscription = if catalogue == 0 { 0 } else { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::Backlight).unwrap_or(0) };
		let keys = if keys == 0 { 0 } else { system_keys::Client::new(ChannelTransport { chan: keys }).watch().unwrap_or(0) };
		let mut brightness = Brightness { catalogue, subscription, backlights: Vec::new(), source: logic::OutputSource::BootFramebuffer(None), edid: None, pending: Vec::new(), next_corr: 1, places: Vec::new(), serial: 0, reads: if read_root != 0 { alloc::vec![read_root] } else { Vec::new() }, controls: if control_root != 0 { alloc::vec![control_root] } else { Vec::new() }, roots: [read_root, control_root], subscribers: Vec::new(), keys, last_step: logic::LastStep::default(), said_none: false };
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

	pub(super) fn set_edid(&mut self, edid: Option<logic::Monitor>) {
		if self.edid != edid {
			self.edid = edid;
			self.rejoin();
		}
	}

	pub(super) fn deadline(&self) -> u64 {
		self.pending.iter().map(|pending| pending.deadline).min().unwrap_or(0)
	}

	pub(super) fn poll(&mut self) {
		let now = clock();
		while let Some(at) = self.pending.iter().position(|pending| now >= pending.deadline) {
			let pending = self.pending.remove(at);
			self.cancel(pending, Error::TimedOut);
		}
	}

	fn setting(&self) -> bool {
		self.pending.iter().any(|pending| matches!(pending.call, Call::Set { .. }))
	}

	fn issue(&mut self, chan: u64, op: u16, call: Call, body: impl FnOnce(&mut wire::VecWriter) -> Option<()>) {
		let corr = self.next_corr;
		self.next_corr = if corr == 0 { 0 } else { corr.checked_add(1).unwrap_or(0) };
		let pending = Pending { chan, corr, deadline: clock().saturating_add(CALL_TICKS), call };
		let mut writer = wire::VecWriter::new();
		let bytes = (|| {
			writer.u16(op)?;
			writer.u32(corr)?;
			body(&mut writer)?;
			writer.into_inner()
		})();
		if corr != 0 && bytes.is_some_and(|bytes| try_send(chan, &bytes, 0)) {
			self.pending.push(pending);
		} else {
			self.cancel(pending, Error::Again);
		}
	}

	fn cancel(&mut self, pending: Pending, error: Error) {
		match pending.call {
			Call::Set { client, .. } => {
				if let Some(at) = self.backlights.iter().position(|backlight| backlight.chan == pending.chan) {
					self.fail(at);
				}
				self.answer(client, Err(error));
			}
			Call::DisplayId(info, description) if error == Error::TimedOut => {
				// Optional operation absent on an older provider. Its ordinary GET still has its own deadline.
				self.issue(pending.chan, backlight::OP_GET, Call::Get(info, description, None), |_| Some(()));
			}
			Call::Open(_) => {}
			_ => close(pending.chan),
		}
	}

	fn answer(&mut self, client: Option<(u64, u32)>, result: Result<BrightnessSet, Error>) {
		let Some((channel, corr)) = client else { return };
		// A client may close while its device is answering. Never address a subsequently minted channel as that client.
		if !self.controls.contains(&channel) {
			return;
		}
		let mut writer = wire::VecWriter::new();
		let encoded = (|| {
			writer.u32(corr)?;
			writer.u8(u8::from(result.is_ok()))?;
			match result {
				Ok(value) => value.write(&mut writer)?,
				Err(error) => error.write(&mut writer)?,
			}
			writer.into_inner()
		})();
		if !encoded.is_some_and(|bytes| try_send(channel, &bytes, 0)) && !self.roots.contains(&channel) {
			close(channel);
			self.controls.retain(|&held| held != channel);
		}
	}

	fn provider_reply(&mut self, channel: u64, _buf: &mut [u8]) {
		let (buf, mut handles) = match receive(channel) {
			Ok(None) => return,
			Err(()) => {
				while let Some(at) = self.pending.iter().position(|pending| pending.chan == channel) {
					let pending = self.pending.remove(at);
					self.cancel(pending, Error::Closed);
				}
				if channel == self.catalogue {
					close(channel);
					self.catalogue = 0;
				} else if let Some(at) = self.backlights.iter().position(|backlight| backlight.chan == channel) {
					self.remove(at);
				}
				return;
			}
			Ok(Some(message)) => message,
		};
		let len = buf.len();
		let corr = buf[..len].get(..4).map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()));
		if let Some(at) = self.pending.iter().position(|pending| pending.chan == channel && Some(pending.corr) == corr) {
			let pending = self.pending.remove(at);
			if clock() >= pending.deadline {
				self.cancel(pending, Error::TimedOut);
			} else {
				let mut reader = wire::Reader::with_handle_list(&buf[..len], &handles);
				let _ = reader.u32();
				match pending.call {
					Call::Open(info) => {
						let opened = (|| {
							if !reader.tag()? {
								return None;
							}
							let _ = reader.u32()?;
							let channel = reader.take_handle()?;
							reader.finish()?;
							Some(channel)
						})();
						if let Some(channel) = opened {
							handles.clear();
							self.issue(channel, backlight::OP_DESCRIBE, Call::Describe(info), |_| Some(()));
						}
					}
					Call::Describe(info) => {
						let description = (|| {
							if !reader.tag()? {
								return None;
							}
							let description = BacklightDescription::read(&mut reader)?;
							reader.finish()?;
							Some(description)
						})();
						if let Some(description) = description {
							if description.source == BacklightSource::Firmware {
								self.issue(channel, backlight::OP_FIRMWARE_DISPLAY_ID, Call::DisplayId(info, description), |_| Some(()));
							} else {
								self.issue(channel, backlight::OP_GET, Call::Get(info, description, None), |_| Some(()));
							}
						} else {
							close(channel);
						}
					}
					Call::DisplayId(info, description) => {
						let id = (|| {
							if !reader.tag()? {
								return None;
							}
							let value = if reader.tag()? { Some(reader.u32()?) } else { None };
							reader.finish()?;
							Some(value)
						})()
						.flatten();
						self.issue(channel, backlight::OP_GET, Call::Get(info, description, id), |_| Some(()));
					}
					Call::Get(info, description, display_id) => {
						let level = (|| {
							if !reader.tag()? {
								let _ = Error::read(&mut reader)?;
								reader.finish()?;
								return Some(None);
							}
							let level = reader.u32()?;
							reader.finish()?;
							Some(Some(level))
						})();
						if let Some(level) = level {
							self.issue(channel, backlight::OP_EVENTS, Call::Events(info, description, level, display_id), |_| Some(()));
						} else {
							close(channel);
						}
					}
					Call::Events(info, description, level, display_id) => {
						if len == 4 && handles.len() == 1 {
							let events = handles.take_first();
							self.admitted(info, channel, description, level, events, display_id);
						} else {
							close(channel);
						}
					}
					Call::Set { client, cause } => {
						let result = (|| {
							let result = if reader.tag()? { Ok(reader.u32()?) } else { Err(Error::read(&mut reader)?) };
							reader.finish()?;
							Some(result)
						})();
						if let Some(at) = self.backlights.iter().position(|backlight| backlight.chan == channel) {
							let result = match result {
								Some(Ok(level)) if self.backlights[at].scale.contains(level) => {
									self.backlights[at].level = Some(level);
									self.backlights[at].touched = true;
									Ok(BrightnessSet { level, serial: self.change(at, cause) })
								}
								Some(Err(error)) => Err(error),
								_ => {
									self.fail(at);
									Err(Error::Io)
								}
							};
							self.answer(client, result);
						} else {
							self.answer(client, Err(Error::Closed));
						}
					}
				}
			}
		}
		for &handle in handles.as_slice() {
			close(handle);
		}
	}

	// Every handle this module waits on.
	pub(super) fn handles(&self, out: &mut Vec<u64>) {
		if self.subscription != 0 {
			out.push(self.subscription);
		}
		if self.catalogue != 0 {
			out.push(self.catalogue);
		}
		for pending in &self.pending {
			if pending.chan != self.catalogue && !self.backlights.iter().any(|backlight| backlight.chan == pending.chan) {
				out.push(pending.chan);
			}
		}
		if self.keys != 0 && !self.setting() {
			out.push(self.keys);
		}
		for backlight in &self.backlights {
			if backlight.events != 0 && !self.setting() {
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
		} else if handle == self.catalogue || self.pending.iter().any(|pending| pending.chan == handle) || self.backlights.iter().any(|backlight| backlight.chan == handle) {
			self.provider_reply(handle, request);
		} else if let Some(at) = self.reads.iter().position(|&read| read == handle) {
			if !self.serve_read(handle, request, reply) {
				close(handle);
				self.reads.remove(at);
			}
		} else if let Some(at) = self.controls.iter().position(|&control| control == handle) {
			if !self.serve_control(handle, request, reply) {
				close(handle);
				self.controls.remove(at);
				for pending in &mut self.pending {
					if let Call::Set { client, .. } = &mut pending.call
						&& client.is_some_and(|(channel, _)| channel == handle)
					{
						*client = None;
					}
				}
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
			if !info.live {
				while let Some(at) = self.pending.iter().position(|pending| pending.publication().is_some_and(|held| same_publication(held, &info))) {
					let pending = self.pending.remove(at);
					self.cancel(pending, Error::Closed);
				}
			}
			let same = |held: &ProviderInfo| same_publication(held, &info);
			match (info.live, self.backlights.iter().position(|backlight| same(&backlight.info))) {
				(true, None) if !self.pending.iter().any(|pending| pending.publication().is_some_and(same)) && self.catalogue != 0 => self.add(info),
				(false, Some(at)) => self.remove(at),
				_ => {}
			}
		}
	}

	fn add(&mut self, info: ProviderInfo) {
		let request = info.clone();
		self.issue(self.catalogue, provider_catalogue::OP_OPEN, Call::Open(info), |writer| request.write(writer));
	}

	fn admitted(&mut self, info: ProviderInfo, chan: u64, description: BacklightDescription, level: Option<u32>, events: u64, firmware_display_id: Option<u32>) {
		let scale = match description.scale {
			BacklightScale::Levels(levels) => logic::Scale::new(levels),
			BacklightScale::Range(range) => logic::Scale::range(range.minimum, range.maximum),
		};
		let Some(scale) = scale else {
			print(b"DisplayService: a backlight described no levels it can be set to; it is left out\n");
			close(chan);
			close(events);
			return;
		};
		let level = level.filter(|level| scale.contains(*level));
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
		self.backlights.push(Backlight { info, chan, events, key: description.key, kind, target, firmware_display_id, scale, ac_default: description.ac_default, battery_default: description.battery_default, level, failed: false, touched: false });
		self.rejoin();
		let at = self.backlights.len() - 1;
		self.change(at, BrightnessCause::Appeared);
	}

	fn remove(&mut self, at: usize) {
		let gone = self.backlights.remove(at);
		while let Some(at) = self.pending.iter().position(|pending| pending.chan == gone.chan) {
			let pending = self.pending.remove(at);
			self.cancel(pending, Error::Closed);
		}
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
		let output = logic::Output { source: self.source, edid: self.edid };
		let candidates: Vec<logic::Candidate> = self.backlights.iter().map(|backlight| logic::Candidate { kind: backlight.kind, target: backlight.target, publisher: logic::Function { bus: backlight.info.bus, dev: backlight.info.dev, func: backlight.info.func }, failed: backlight.failed, firmware_display_id: backlight.firmware_display_id, stable_key: &backlight.key }).collect();
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

	fn set(&mut self, key: &str, target: logic::Target, allow_zero: bool, client: (u64, u32)) -> Result<(), Error> {
		let at = self.backlights.iter().position(|backlight| backlight.key == key).ok_or(Error::NotFound)?;
		// A SHADOWED BACKLIGHT IS REFUSED: two writers on one panel is what the join's one active backlight prevents.
		// The vocabulary has no `busy`; `denied` is the standing answer until the join changes.
		if matches!(self.places.get(at).map(|place| place.standing), Some(logic::Standing::Shadowed(_))) {
			return Err(Error::Denied);
		}
		if self.backlights[at].failed {
			return Err(Error::Closed);
		}
		if self.pending.iter().any(|pending| pending.chan == self.backlights[at].chan) {
			return Err(Error::Again);
		}
		let level = logic::resolve(&self.backlights[at].scale, self.backlights[at].level, target, allow_zero).map_err(|refusal| match refusal {
			logic::Refusal::Invalid => Error::Invalid,
			logic::Refusal::Unknown => Error::Again,
		})?;
		self.apply(at, level, BrightnessCause::Client, Some(client));
		Ok(())
	}

	fn apply(&mut self, at: usize, level: u32, cause: BrightnessCause, client: Option<(u64, u32)>) {
		self.issue(self.backlights[at].chan, backlight::OP_SET, Call::Set { client, cause }, |writer| writer.u32(level));
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
		self.apply(at, level, cause, None);
	}

	fn take_keys(&mut self, request: &mut [u8]) {
		loop {
			if self.setting() {
				return;
			}
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
			let key = system_keys::watch_read(&request[..len], &mut frame_handles);
			for &handle in frame_handles.as_slice() {
				close(handle);
			}
			let Some(key) = key else { continue };
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
			if self.setting() {
				return;
			}
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
			let event = backlight::events_read(&request[..len], &mut frame_handles);
			for &handle in frame_handles.as_slice() {
				close(handle);
			}
			let Some(event) = event else { continue };
			// A STREAM THAT SPEAKS IS A PROVIDER THAT ANSWERS again.
			if self.backlights[at].failed {
				self.backlights[at].failed = false;
				self.rejoin();
			}
			match event {
				BacklightEvent::Level(level) => {
					if !self.backlights[at].scale.contains(level) {
						continue;
					}
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
			edid: self.edid.map(|monitor| proto::system::EdidIdentity { manufacturer: monitor.manufacturer, product: monitor.product, serial: monitor.serial }),
		}
	}

	// ONE REQUEST ON A READ CHANNEL - the root or a connection minted from it. False when the channel closed.
	fn serve_read(&mut self, channel: u64, _request: &mut [u8], reply: &mut [u8]) -> bool {
		let (request, handles) = match receive(channel) {
			Ok(Some(message)) => message,
			Ok(None) => return true,
			Err(()) => return false,
		};
		let len = request.len();
		for &handle in handles.as_slice() {
			close(handle);
		}
		let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
		if op == HEARTBEAT_OP {
			try_send(channel, b"PONG", 0);
		} else if op == CONNECT_OP {
			match rt::channel() {
				Some((mine, theirs)) => {
					if try_send(channel, &[], theirs) {
						self.reads.push(mine);
					} else {
						close(mine);
						close(theirs);
					}
				}
				None => {
					try_send(channel, &[], 0);
				}
			}
		} else if op == display_brightness::OP_SUBSCRIBE {
			self.open_subscription(channel, &request[..len]);
		} else if op == display_brightness::OP_OUTPUTS || op == display_brightness::OP_BACKLIGHTS {
			let decoded = (|| {
				let mut reader = wire::Reader::new(&request);
				let _ = reader.u16()?;
				let corr = reader.u32()?;
				reader.finish()?;
				Some(corr)
			})();
			let Some(corr) = decoded else { return self.roots.contains(&channel) };
			let mut writer = wire::VecWriter::new();
			let encoded = (|| {
				writer.u32(corr)?;
				writer.u8(1)?;
				if op == display_brightness::OP_OUTPUTS {
					writer.u16(1)?;
					self.output().write(&mut writer)?;
				} else {
					let snapshot = self.snapshot();
					writer.u16(u16::try_from(snapshot.len()).ok()?)?;
					for state in snapshot {
						state.write(&mut writer)?;
					}
				}
				writer.into_inner()
			})();
			if !encoded.is_some_and(|bytes| try_send(channel, &bytes, 0)) {
				return self.roots.contains(&channel);
			}
		} else {
			let mut reply_handle = Handles::new();
			let mut request_handle = Handles::new();
			let snapshot = self.snapshot();
			let output = self.output();
			// A REQUEST THE READ DOES NOT CARRY - a set framed for the control root among them - ends the connection.
			let Some(n) = display_brightness::dispatch(&mut ReadCall { snapshot, output }, &request[..len], &mut request_handle, reply, &mut reply_handle) else { return self.roots.contains(&channel) };
			try_send(channel, &reply[..n], 0);
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
			try_send(channel, &corr.to_le_bytes(), 0);
			return;
		};
		let mut subscriber = Subscriber { stream: producer, seq: 0 };
		for state in &snapshot {
			let change = BrightnessChange { key: state.key.clone(), level: state.level, cause: BrightnessCause::Snapshot, serial: self.serial };
			if !send_change(&mut subscriber, &change) {
				close(producer);
				close(consumer);
				try_send(channel, &corr.to_le_bytes(), 0);
				return;
			}
		}
		if !try_send(channel, &corr.to_le_bytes(), consumer) {
			close(producer);
			close(consumer);
			return;
		}
		self.subscribers.push(subscriber);
	}

	fn serve_control(&mut self, channel: u64, _request: &mut [u8], reply: &mut [u8]) -> bool {
		let (request, handles) = match receive(channel) {
			Ok(Some(message)) => message,
			Ok(None) => return true,
			Err(()) => return false,
		};
		let len = request.len();
		for &handle in handles.as_slice() {
			close(handle);
		}
		let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
		if op == HEARTBEAT_OP {
			try_send(channel, b"PONG", 0);
		} else if op == CONNECT_OP {
			match rt::channel() {
				Some((mine, theirs)) => {
					if try_send(channel, &[], theirs) {
						self.controls.push(mine);
					} else {
						close(mine);
						close(theirs);
					}
				}
				None => {
					try_send(channel, &[], 0);
				}
			}
		} else {
			let mut reply_handle = Handles::new();
			let mut request_handle = Handles::new();
			let mut call = ControlCall { asked: None };
			let Some(_) = display_brightness_control::dispatch(&mut call, &request[..len], &mut request_handle, reply, &mut reply_handle) else { return self.roots.contains(&channel) };
			let Some((key, target, allow_zero)) = call.asked else { return self.roots.contains(&channel) };
			let corr = u32::from_le_bytes(request[2..6].try_into().unwrap());
			if let Err(error) = self.set(&key, target, allow_zero, (channel, corr)) {
				self.answer(Some((channel, corr)), Err(error));
			}
		}
		true
	}
}

fn send_change(subscriber: &mut Subscriber, change: &BrightnessChange) -> bool {
	let mut writer = wire::VecWriter::new();
	let encoded = (|| {
		writer.u32(subscriber.seq)?;
		change.write(&mut writer)?;
		writer.into_inner()
	})();
	let sent = encoded.is_some_and(|bytes| try_send(subscriber.stream, &bytes, 0));
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

struct ControlCall {
	asked: Option<(String, logic::Target, bool)>,
}

impl display_brightness_control::Service for ControlCall {
	fn set(&mut self, key: String, target: BrightnessTarget, allow_zero: bool) -> Result<BrightnessSet, Error> {
		let target = match target {
			BrightnessTarget::Level(level) => logic::Target::Level(level),
			BrightnessTarget::Percent(percent) => logic::Target::Percent(percent),
			BrightnessTarget::Steps(steps) => logic::Target::Steps(steps),
		};
		self.asked = Some((key, target, allow_zero));
		Err(Error::Again)
	}
}
