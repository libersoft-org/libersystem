// BrightnessPolicy - what the screen's brightness does on its own: a stored level restored when a backlight appears, the
// firmware's default where nothing is stored, the active backlight dimmed when nobody has touched the machine for the
// idle timeout and put back at the first input, and - once someone turns it on - automatic brightness following an
// ambient-light sensor. DisplayService owns every backlight, the keys and the floor; this service only asks it.
// `service_logic::brightness::Policy` decides; this reads what it is told and carries out what it decides.
//
// A SERVICE OF ITS OWN BECAUSE THE SCREEN MUST NOT WAIT FOR A VOLUME: restoring a level needs ConfigService, which starts
// after StorageService, while DisplayService starts without either and keeps no configuration. Without this service the
// keys, the firmware's hotkeys and the floor still work, and nothing is restored or dimmed.
//
// WHAT IT HOLDS: DisplayService's `BRIGHTNESS` read, whose subscription tells it every change and who made it, and
// `BRIGHTNESSCTL`, the set no other component holds; ConfigService, for the stored levels and the two settings;
// PowerService's read, for line power against battery; InputService's activity signal at the idle timeout; and a
// catalogue connection for `ambient-light` alone. It serves `CONTROL`, which the `brightness` tool reaches through the
// `brightness-control` capability: a set, forwarded with the caller's explicit zero so the floor holds the same, and the
// two settings.
//
// ITS OWN CHANGES ARE NEVER STORED. A set answers the serial of the change it produced, and the policy remembers the
// serials of the sets it made for a restore, a default, a dim or automatic brightness; every other change - a key, a
// hotkey, the device, a set through `CONTROL` - is stored once the level has been still for two seconds.
//
// TRANSPARENT AND RECONSTRUCTIBLE, SO NO ROLE IS EXCLUSIVE: settings and levels are ConfigService's and everything else
// is read again. A restart loses a dim in progress, whose level stays until a key or a set moves it, and a pause of
// automatic brightness, which resumes. DisplayService says which backlights anything has moved since they appeared, and
// only the others are restored - so a restarted policy leaves a level alone.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{ActivityEdge, BacklightStanding, BacklightState, BrightnessCause, BrightnessScale, BrightnessSet, BrightnessSettings, BrightnessTarget, ChangeKind, ConfigEntry, Error, ProviderInfo, ProviderKind, SourceId, SourceState, ambient_light, brightness_policy, config, display_brightness, display_brightness_control, input_activity, power, provider_catalogue};
use rt::*;
use service_logic::brightness::{self as logic, Action, ActiveBacklight, Appearing, CurvePoint, Policy, Settings};
use wire::Handles;

include!(concat!(env!("OUT_DIR"), "/roles_brightness_policy.rs"));

// How long one question to DisplayService, ConfigService, PowerService, InputService or a sensor may take.
const ASK_TICKS: u64 = TICKS_PER_SECOND * 2;
// How long a level must hold before it is stored.
const SETTLE_TICKS: u64 = TICKS_PER_SECOND * logic::SETTLE_SECONDS;
// The most `CONTROL` connections at once.
const MAX_CLIENTS: usize = 16;

fn say(text: &str) {
	let line = format!("BrightnessPolicy: {text}\n");
	print(line.as_bytes());
}

// ONE AMBIENT-LIGHT SENSOR followed: its curve - the firmware's, or empty for the policy's default - and its stream.
struct Sensor {
	info: ProviderInfo,
	chan: u64,
	stream: u64,
	curve: Vec<CurvePoint>,
}

struct Service {
	policy: Policy,
	read: u64,
	changes: u64,
	control: u64,
	config: u64,
	activity: u64,
	activity_stream: u64,
	power_stream: u64,
	sources: Vec<(SourceId, SourceState)>,
	catalogue: u64,
	sensors_subscription: u64,
	sensors: Vec<Sensor>,
	backlights: Vec<BacklightState>,
}

impl Service {
	// ------------------------------------------------------------------ settings and the tree

	// THE SETTINGS, from ConfigService's tree - a key absent or a tree that does not answer keeps its default, and a
	// value refused is named.
	fn read_settings(&self) -> Settings {
		let mut settings = Settings::default();
		if self.config == 0 {
			say("no configuration tree - the default settings stand and no level is restored or stored");
			return settings;
		}
		let mut client = config::Client::with_deadline(ChannelTransport { chan: self.config }, clock() + ASK_TICKS);
		if let Some(Ok(text)) = client.get(logic::AUTO_KEY) {
			match logic::parse_automatic(&text) {
				Some(on) => settings.automatic = on,
				None => say(&format!("the value of {} is not one it takes - its default stands", logic::AUTO_KEY)),
			}
		}
		client.set_deadline(clock() + ASK_TICKS);
		if let Some(Ok(text)) = client.get(logic::IDLE_KEY) {
			match logic::parse_idle(&text) {
				Some(seconds) => settings.idle_seconds = seconds,
				None => say(&format!("the value of {} is not one it takes - its default stands", logic::IDLE_KEY)),
			}
		}
		settings
	}

	fn stored(&self, key: &str) -> Option<u32> {
		if self.config == 0 {
			return None;
		}
		match config::Client::with_deadline(ChannelTransport { chan: self.config }, clock() + ASK_TICKS).get(&logic::level_key(key)) {
			Some(Ok(text)) => logic::parse_level(&text),
			_ => None,
		}
	}

	fn store(&self, key: &str, value: String) -> bool {
		if self.config == 0 {
			return false;
		}
		matches!(config::Client::with_deadline(ChannelTransport { chan: self.config }, clock() + ASK_TICKS).set(&ConfigEntry { key: String::from(key), value }), Some(Ok(())))
	}

	// ------------------------------------------------------------------ what DisplayService says

	fn refresh(&mut self) {
		if self.read == 0 {
			return;
		}
		if let Some(Ok(backlights)) = display_brightness::Client::with_deadline(ChannelTransport { chan: self.read }, clock() + ASK_TICKS).backlights() {
			self.backlights = backlights;
		}
	}

	fn find(&self, key: &str) -> Option<&BacklightState> {
		self.backlights.iter().find(|backlight| backlight.key == key)
	}

	// OUTPUT 0'S ACTIVE BACKLIGHT, the one idle dimming and automatic brightness act on.
	fn active_key(&self) -> Option<String> {
		self.backlights.iter().find(|backlight| backlight.standing == BacklightStanding::Active && backlight.output == Some(0)).map(|backlight| backlight.key.clone())
	}

	fn with_active<T>(&self, then: impl FnOnce(&mut Policy, Option<ActiveBacklight<'_>>) -> T, policy: &mut Policy) -> T {
		let key = self.active_key();
		let held = key.as_deref().and_then(|key| self.find(key)).and_then(|backlight| scale(&backlight.scale).map(|scale| (backlight.key.as_str(), scale, backlight.level)));
		match &held {
			Some((key, scale, level)) => then(policy, Some(ActiveBacklight { key, scale, level: *level })),
			None => then(policy, None),
		}
	}

	// ONE CHANGE from the subscription: a backlight appearing - or listed in the snapshot - gets its first level; a
	// level moved is the policy's to note; a backlight leaving is forgotten.
	fn change(&mut self, key: &str, level: Option<u32>, cause: BrightnessCause, serial: u64) {
		match cause {
			BrightnessCause::Snapshot | BrightnessCause::Appeared => {
				self.refresh();
				self.appeared(key);
			}
			BrightnessCause::Left => {
				self.policy.left(key);
				self.backlights.retain(|backlight| backlight.key != key);
			}
			BrightnessCause::Key | BrightnessCause::FirmwareHotkey | BrightnessCause::Device | BrightnessCause::Client => {
				if let Some(backlight) = self.backlights.iter_mut().find(|backlight| backlight.key == key) {
					backlight.level = level;
					backlight.touched = true;
				}
				self.policy.changed(key, level, serial, clock(), SETTLE_TICKS);
			}
		}
	}

	fn appeared(&mut self, key: &str) {
		let Some(backlight) = self.find(key) else { return };
		let Some(levels) = scale(&backlight.scale) else { return };
		let appearing = Appearing {
			standing: match backlight.standing {
				BacklightStanding::Active => logic::Standing::Active,
				BacklightStanding::Shadowed => logic::Standing::Shadowed(0),
				BacklightStanding::Unjoined => logic::Standing::Unjoined,
			},
			scale: levels,
			level: backlight.level,
			touched: backlight.touched,
			ac_default: backlight.ac_default,
			battery_default: backlight.battery_default,
			stored: self.stored(key),
		};
		let on_battery = self.on_battery();
		if let Some(level) = logic::first_level(&appearing, on_battery) {
			let why = if appearing.stored.is_some() {
				"its stored level"
			} else if on_battery {
				"the firmware's battery default"
			} else {
				"the firmware's AC default"
			};
			say(&format!("{key} set to {level}, {why}"));
			self.carry_out(Action::Set { key: String::from(key), level });
		}
	}

	// ------------------------------------------------------------------ carrying out

	fn carry_out(&mut self, action: Action) {
		match action {
			Action::Set { key, level } => self.set_own(&key, BrightnessTarget::Level(level)),
			Action::Percent { key, percent } => self.set_own(&key, BrightnessTarget::Percent(percent)),
			Action::Store { key, level } => {
				if self.store(&logic::level_key(&key), format!("{level}")) {
					say(&format!("{key} stored at {level}"));
				} else {
					say(&format!("{key} could not be stored at {level}"));
				}
			}
		}
	}

	// A SET OF THE POLICY'S OWN: its serial noted, so its change is neither stored nor taken for someone's hand.
	fn set_own(&mut self, key: &str, target: BrightnessTarget) {
		if let Ok(set) = self.forward(key, &target, false) {
			self.policy.answered(set.serial);
		}
	}

	fn forward(&self, key: &str, target: &BrightnessTarget, allow_zero: bool) -> Result<BrightnessSet, Error> {
		if self.control == 0 {
			return Err(Error::Closed);
		}
		match display_brightness_control::Client::with_deadline(ChannelTransport { chan: self.control }, clock() + ASK_TICKS).set(key, target, &allow_zero) {
			Some(answer) => answer,
			None => Err(Error::TimedOut),
		}
	}

	// ------------------------------------------------------------------ the power source

	fn on_battery(&self) -> bool {
		power_model::canon::supply(self.sources.iter().map(|(_, state)| state)).on_battery
	}

	fn drain_power(&mut self, buf: &mut [u8]) {
		loop {
			match try_recv_caps(self.power_stream, buf) {
				PolledCaps::Message { len, handles } => {
					let mut frame = handles;
					let Some(change) = power::subscribe_read(&buf[..len], &mut frame) else { continue };
					for &leftover in frame.as_slice() {
						close(leftover);
					}
					match change.kind {
						ChangeKind::Snapshot | ChangeKind::Added | ChangeKind::Updated => {
							if let Some(source) = change.source {
								self.sources.retain(|(id, _)| *id != source.id);
								self.sources.push((source.id, source.state));
							}
						}
						ChangeKind::Removed => {
							if let Some(gone) = change.gone {
								self.sources.retain(|(id, _)| *id != gone);
							}
						}
						ChangeKind::SnapshotEnd => {}
					}
				}
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					close(self.power_stream);
					self.power_stream = 0;
					break;
				}
			}
		}
	}

	// THE SUBSCRIPTION'S SNAPSHOT, read before the first level is chosen, so a default follows the power source.
	fn drain_power_now(&mut self, buf: &mut [u8]) {
		let deadline = clock() + ASK_TICKS;
		while self.power_stream != 0 && clock() < deadline {
			if wait_any(&[self.power_stream], deadline) < 0 {
				break;
			}
			self.drain_power(buf);
			if !self.sources.is_empty() {
				break;
			}
		}
	}

	// ------------------------------------------------------------------ activity

	// THE ACTIVITY SIGNAL AT THE IDLE TIMEOUT, watched again whenever it changes; none while it is off.
	fn watch_activity(&mut self) {
		if self.activity_stream != 0 {
			close(self.activity_stream);
			self.activity_stream = 0;
		}
		let Some(seconds) = self.policy.settings.idle_seconds else { return };
		if self.activity == 0 {
			return;
		}
		self.activity_stream = input_activity::Client::with_deadline(ChannelTransport { chan: self.activity }, clock() + ASK_TICKS).watch(&seconds.saturating_mul(1000)).unwrap_or(0);
		if self.activity_stream == 0 {
			say("InputService did not answer the activity watch - nothing is dimmed");
		}
	}

	fn take_activity(&mut self, buf: &mut [u8]) {
		loop {
			let (len, handles) = match try_recv_caps(self.activity_stream, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					close(self.activity_stream);
					self.activity_stream = 0;
					return;
				}
			};
			let mut frame = handles;
			let Some(edge) = input_activity::watch_read(&buf[..len], &mut frame) else { continue };
			let action = match edge {
				ActivityEdge::Idle => {
					self.refresh();
					let mut policy = core::mem::take(&mut self.policy);
					let action = self.with_active(|policy, active| policy.idle(active), &mut policy);
					self.policy = policy;
					action
				}
				ActivityEdge::Active => self.policy.active(),
			};
			if let Some(action) = action {
				self.carry_out(action);
			}
		}
	}

	// ------------------------------------------------------------------ ambient light

	fn follow_sensors(&mut self, buf: &mut [u8]) {
		while self.sensors_subscription != 0 {
			let (len, handles) = match try_recv_caps(self.sensors_subscription, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					close(self.sensors_subscription);
					self.sensors_subscription = 0;
					break;
				}
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut frame_handles = Handles::new();
			let Some(info) = provider_catalogue::subscribe_read(&buf[..len], &mut frame_handles) else { continue };
			if info.kind != ProviderKind::AmbientLight {
				continue;
			}
			let same = |held: &ProviderInfo| held.slot == info.slot && held.provider_generation == info.provider_generation && held.binding_generation == info.binding_generation;
			match (info.live, self.sensors.iter().position(|sensor| same(&sensor.info))) {
				(true, None) => self.add_sensor(info),
				(false, Some(at)) => {
					let gone = self.sensors.remove(at);
					close(gone.stream);
					close(gone.chan);
				}
				_ => {}
			}
		}
	}

	fn add_sensor(&mut self, info: ProviderInfo) {
		let chan = match provider_catalogue::Client::with_deadline(ChannelTransport { chan: self.catalogue }, clock() + ASK_TICKS).open(&info) {
			Some(Ok(chan)) => chan,
			_ => {
				say("the catalogue gave no connection to an ambient-light sensor it published");
				return;
			}
		};
		let mut client = ambient_light::Client::with_deadline(ChannelTransport { chan }, clock() + ASK_TICKS);
		let curve = match client.describe() {
			Some(Ok(description)) => logic::firmware_curve(description.curve.iter().map(|point| (point.adjustment, point.illuminance))),
			_ => Vec::new(),
		};
		client.set_deadline(clock() + ASK_TICKS);
		let Some(stream) = client.events() else {
			say("an ambient-light sensor opened no reading stream; it is left out");
			close(chan);
			return;
		};
		say(if curve.is_empty() { "an ambient-light sensor appeared - automatic brightness follows the default curve" } else { "an ambient-light sensor appeared - automatic brightness follows the firmware's curve" });
		self.sensors.push(Sensor { info, chan, stream, curve });
	}

	// A READING. Only the first sensor's moves a level: two in one room would fight.
	fn take_reading(&mut self, at: usize, buf: &mut [u8]) {
		let stream = self.sensors[at].stream;
		let mut latest = None;
		loop {
			match try_recv_caps(stream, buf) {
				PolledCaps::Message { len, handles } => {
					let mut frame = handles;
					if let Some(reading) = ambient_light::events_read(&buf[..len], &mut frame) {
						latest = Some(reading.milli_lux);
					}
				}
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					let gone = self.sensors.remove(at);
					close(gone.stream);
					close(gone.chan);
					return;
				}
			}
		}
		let (Some(milli_lux), 0) = (latest, at) else { return };
		let curve = self.sensors[0].curve.clone();
		let mut policy = core::mem::take(&mut self.policy);
		let action = self.with_active(|policy, active| policy.illuminance(active, milli_lux, &curve), &mut policy);
		self.policy = policy;
		if let Some(action) = action {
			self.carry_out(action);
		}
	}
}

fn scale(scale: &BrightnessScale) -> Option<logic::Scale> {
	match scale {
		BrightnessScale::Levels(levels) => logic::Scale::new(levels.clone()),
		BrightnessScale::Range(range) => logic::Scale::range(range.minimum, range.maximum),
	}
}

// ------------------------------------------------------------------ the control root

struct Control<'a> {
	service: &'a mut Service,
	retarget: bool,
}

impl brightness_policy::Service for Control<'_> {
	// A SET, FORWARDED: an empty key is output 0's active backlight. Its change comes back through the subscription as
	// anyone's would, and is stored once it settles.
	fn set(&mut self, key: String, target: BrightnessTarget, allow_zero: bool) -> Result<BrightnessSet, Error> {
		let key = if key.is_empty() {
			self.service.refresh();
			self.service.active_key().ok_or(Error::NotFound)?
		} else {
			key
		};
		self.service.forward(&key, &target, allow_zero)
	}

	fn settings(&mut self) -> Result<BrightnessSettings, Error> {
		let settings = self.service.policy.settings;
		Ok(BrightnessSettings { automatic: settings.automatic, idle_seconds: settings.idle_seconds })
	}

	fn set_automatic(&mut self, on: bool) -> Result<(), Error> {
		if !self.service.store(logic::AUTO_KEY, String::from(logic::automatic_text(on))) {
			return Err(Error::Io);
		}
		self.service.policy.set_automatic(on);
		say(if on { "automatic brightness on" } else { "automatic brightness off" });
		Ok(())
	}

	fn set_idle(&mut self, seconds: Option<u32>) -> Result<(), Error> {
		if seconds == Some(0) {
			return Err(Error::Invalid);
		}
		if !self.service.store(logic::IDLE_KEY, logic::idle_text(seconds)) {
			return Err(Error::Io);
		}
		if let Some(action) = self.service.policy.set_idle(seconds) {
			self.service.carry_out(action);
		}
		self.retarget = true;
		say(&format!("the idle timeout is {}", logic::idle_text(seconds)));
		Ok(())
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	// THE ROLES: the ambient-light catalogue, DisplayService's read and set, the tree, PowerService's read, InputService's
	// activity signal, and the root the tool's connections are minted on.
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let [catalogue, read, control, config_client, power_client, activity, root] = roles;
	let mut buf = alloc::vec![0u8; 8192];
	let mut reply = alloc::vec![0u8; 8192];

	let mut service = Service { policy: Policy::default(), read, changes: 0, control, config: config_client, activity, activity_stream: 0, power_stream: 0, sources: Vec::new(), catalogue, sensors_subscription: 0, sensors: Vec::new(), backlights: Vec::new() };
	service.policy = Policy::new(service.read_settings());
	if power_client != 0 {
		// THE CONNECTION IS KEPT for as long as the stream: the subscription is the connection's.
		match power::Client::with_deadline(ChannelTransport { chan: power_client }, clock() + ASK_TICKS).subscribe() {
			Some(Ok(stream)) => service.power_stream = stream,
			_ => say("PowerService gave no state stream - the firmware's AC default stands"),
		}
	}
	service.drain_power_now(&mut buf);
	service.watch_activity();
	if catalogue != 0 {
		service.sensors_subscription = provider_catalogue::Client::with_deadline(ChannelTransport { chan: catalogue }, clock() + ASK_TICKS).subscribe(&ProviderKind::AmbientLight).unwrap_or(0);
	}
	if read != 0 {
		service.changes = display_brightness::Client::with_deadline(ChannelTransport { chan: read }, clock() + ASK_TICKS).subscribe().unwrap_or(0);
	}
	if service.changes == 0 || control == 0 {
		say("DisplayService's brightness is not reachable - nothing is restored, dimmed or followed");
	}
	let settings = service.policy.settings;
	say(&format!("automatic brightness {}, the idle timeout {}", logic::automatic_text(settings.automatic), logic::idle_text(settings.idle_seconds)));
	print(b"BrightnessPolicy: online\n");
	send_blocking(bootstrap, b"BrightnessPolicy: online", 0);

	let mut clients: Vec<u64> = Vec::new();
	let timer: u64 = match timer_create() {
		t if t > 0 => t as u64,
		_ => exit(),
	};
	loop {
		for action in service.policy.settled(clock()) {
			service.carry_out(action);
		}
		timer_set(timer, service.policy.next_due().unwrap_or(u64::MAX));
		let mut waitset: Vec<u64> = alloc::vec![timer];
		for handle in [root, service.changes, service.activity_stream, service.power_stream, service.sensors_subscription] {
			if handle != 0 {
				waitset.push(handle);
			}
		}
		waitset.extend(service.sensors.iter().map(|sensor| sensor.stream));
		waitset.extend(clients.iter().copied());
		let ready = wait_any(&waitset, 0);
		if ready < 0 {
			continue;
		}
		let handle = waitset[ready as usize];
		if handle == timer {
			continue;
		}
		if handle == service.changes {
			loop {
				let (len, handles) = match try_recv_caps(service.changes, &mut buf) {
					PolledCaps::Message { len, handles } => (len, handles),
					PolledCaps::Empty => break,
					// A SUBSCRIBER THAT FELL BEHIND HAS ITS STREAM CLOSED, and subscribes again for a fresh snapshot.
					PolledCaps::Closed => {
						close(service.changes);
						service.changes = display_brightness::Client::with_deadline(ChannelTransport { chan: service.read }, clock() + ASK_TICKS).subscribe().unwrap_or(0);
						break;
					}
				};
				let mut frame = handles;
				let Some(change) = display_brightness::subscribe_read(&buf[..len], &mut frame) else { continue };
				service.change(&change.key, change.level, change.cause, change.serial);
			}
			continue;
		}
		if handle == service.activity_stream {
			service.take_activity(&mut buf);
			continue;
		}
		if handle == service.power_stream {
			service.drain_power(&mut buf);
			continue;
		}
		if handle == service.sensors_subscription {
			service.follow_sensors(&mut buf);
			continue;
		}
		if let Some(at) = service.sensors.iter().position(|sensor| sensor.stream == handle) {
			service.take_reading(at, &mut buf);
			continue;
		}
		// THE ROOT mints a connection; a CLIENT speaks `brightness-policy`.
		let is_root = handle == root;
		let (len, mut handles) = match try_recv_caps(handle, &mut buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => continue,
			PolledCaps::Closed => {
				if !is_root {
					clients.retain(|&client| client != handle);
					close(handle);
				}
				continue;
			}
		};
		let op: u16 = if len >= 2 { u16::from_le_bytes([buf[0], buf[1]]) } else { 0 };
		if op == HEARTBEAT_OP {
			send_blocking(handle, b"PONG", 0);
			continue;
		}
		if op == CONNECT_OP {
			match channel().filter(|_| clients.len() < MAX_CLIENTS) {
				Some((mine, theirs)) => {
					clients.push(mine);
					send_blocking(handle, &[], theirs);
				}
				None => {
					send_blocking(handle, &[], 0);
				}
			}
			continue;
		}
		if is_root {
			for &leftover in handles.as_slice() {
				close(leftover);
			}
			continue;
		}
		let request = buf[..len].to_vec();
		let mut reply_handles = Handles::new();
		let mut call = Control { service: &mut service, retarget: false };
		let written = brightness_policy::dispatch(&mut call, &request, &mut handles, &mut reply, &mut reply_handles);
		let retarget = call.retarget;
		match written {
			Some(written) => {
				send_caps_blocking(handle, &reply[..written], reply_handles.as_slice());
			}
			// A REQUEST THE INTERFACE DOES NOT CARRY ends that connection.
			None => {
				clients.retain(|&client| client != handle);
				close(handle);
			}
		}
		if retarget {
			service.watch_activity();
		}
	}
}
