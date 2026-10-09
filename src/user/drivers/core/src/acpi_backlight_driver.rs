// THE ACPI VIDEO BACKLIGHT: one binding per panel output node - the `video-output` class, a method-only device below a
// display adapter's companion with `_BCL` and `_BCM` - its methods evaluated over the node-scoped channel the ACPI
// service serves, published as ONE `backlight` provider to DisplayService alone.
//
// THE LEVELS ARE `_BCL`'S, normalised (`drivers::acpi_video::bcl`): the firmware's AC and battery defaults, then the
// levels, sorted and deduplicated, nothing above 100. `_BCM` sets; `_BQC` reads, a value off the list snapped to the
// nearest and logged once; without `_BQC` the level is the last one set, unknown until then.
//
// `_DOS` IS THE ADAPTER'S: evaluated on the parent at bind and at every resume with 0x04, so the firmware changes no
// brightness on a hotkey and only notifies. An adapter with `_DOD` and no `_DOS` still binds: the step is skipped and
// said once.
//
// THE NOTIFICATIONS ON THE OUTPUT NODE, the whole map: 0x85 to 0x88 are hotkeys - cycle, up, down, zero - forwarded
// for DisplayService to apply like keys, the floor kept; 0x89, display off, changes no level and is logged; anything
// else is logged and ignored. FIRMWARE THAT STEPS ANYWAY: `_BQC` is read before a step is forwarded, and a level that
// already moved that way since the last set is reported as the firmware's change instead - one press, one step.
//
// THE TARGET is the adapter's PCI function, which the namespace attached to this row as its parent: the join's
// `firmware-adapter` reason. An adapter that resolved to no function leaves the backlight unjoined.
//
// ACROSS A SLEEP: `SUSPEND` finishes the method in hand - this driver is single-threaded - and evaluates nothing until
// `RESUME`, which evaluates `_DOS` again and applies the level last set again with `_BCM`, because firmware may reset
// both. WHEN THE ACPI SERVICE RESTARTS its node channels end with it: the node is asked for again (`drivers::common`)
// and the same two steps run on the new one.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aml::wire::Value;
use drivers::acpi_video::{self, Hotkey, Levels, Notification};
use drivers::common;
use ipc_client::ChannelTransport;
use proto::system::{BacklightDescription, BacklightEvent, BacklightFunction, BacklightHotkey, BacklightScale, BacklightSource, BacklightTarget, Error, acpi_node, backlight};
use rt::*;
use wire::Handles;

// How long one question to the node may take.
const TICKS: u64 = TICKS_PER_SECOND * 5;
// The one publication's token.
const TOKEN: u16 = 0;
// How often a binding whose node ended looks for the one handed again.
const NODE_LOOK_TICKS: u64 = TICKS_PER_SECOND;
// Deep enough for a burst of hotkeys DisplayService has not read yet.
const STREAM_DEPTH: u64 = 16;

struct Panel {
	name: String,
	node: u64,
	levels: Levels,
	target: BacklightTarget,
	// The level this driver set last, applied again after a sleep and on a new node.
	set: Option<u32>,
	// Whether `_BQC` exists, and whether a value off the list was already said.
	has_bqc: bool,
	snapped_said: bool,
	// Whether the adapter has `_DOS`, once asked.
	has_dos: Option<bool>,
	events: u64,
	events_seq: u32,
}

impl Panel {
	fn say(&self, text: &str) {
		print(format!("driver.acpi-backlight: {}: {text}\n", self.name).as_bytes());
	}

	// A method with integer arguments, relative to the node (`^` for the parent's): its value, `None` when it is absent.
	fn evaluate(&self, method: &str, arguments: &[u64]) -> Result<Option<Value>, String> {
		let arguments = if arguments.is_empty() { Vec::new() } else { aml::wire::encode(&Value::Package(arguments.iter().map(|value| Value::Integer(*value)).collect())).map_err(|error| format!("{method}'s arguments did not encode - {error:?}"))? };
		match acpi_node::Client::with_deadline(ChannelTransport { chan: self.node }, clock() + TICKS).evaluate(method, &arguments) {
			Some(Ok(bytes)) => aml::wire::decode(&bytes).map(Some).map_err(|error| format!("{method} did not decode - {error:?}")),
			Some(Err(Error::NotFound)) => Ok(None),
			Some(Err(error)) => Err(format!("{method} was refused - {error:?}")),
			None => Err(format!("{method} was not answered")),
		}
	}

	// `_DOS` ON THE ADAPTER with 0x04, or the step skipped - said once - where the adapter has none.
	fn notify_only(&mut self) {
		match self.evaluate("^_DOS", &[acpi_video::DOS_NOTIFY_ONLY]) {
			Ok(Some(_)) => {
				if self.has_dos != Some(true) {
					self.say("the adapter's _DOS set to 0x04 - the firmware notifies hotkeys and changes no level itself");
				}
				self.has_dos = Some(true);
			}
			Ok(None) => {
				if self.has_dos.is_none() {
					self.say("the adapter has no _DOS - the step is skipped");
				}
				self.has_dos = Some(false);
			}
			Err(why) => self.say(&format!("the adapter's _DOS failed - {why}")),
		}
	}

	// THE LEVEL NOW: `_BQC`, snapped to the list; the last one set where there is no `_BQC`.
	fn level(&mut self) -> Option<u32> {
		if !self.has_bqc {
			return self.set;
		}
		match self.evaluate("_BQC", &[]) {
			Ok(Some(Value::Integer(raw))) => {
				let (level, snapped) = acpi_video::bqc(&self.levels.levels, raw);
				if snapped && !self.snapped_said {
					self.say(&format!("_BQC answered {raw}, which _BCL does not list - read as {level}"));
					self.snapped_said = true;
				}
				Some(level)
			}
			Ok(Some(_)) => {
				self.say("_BQC answered something that is not a level");
				None
			}
			Ok(None) => {
				self.has_bqc = false;
				self.set
			}
			Err(why) => {
				self.say(&why);
				None
			}
		}
	}

	fn apply(&mut self, level: u32) -> Result<u32, Error> {
		if !self.levels.levels.contains(&level) {
			return Err(Error::Invalid);
		}
		match self.evaluate("_BCM", &[u64::from(level)]) {
			Ok(_) => {
				self.set = Some(level);
				Ok(level)
			}
			Err(why) => {
				self.say(&why);
				Err(Error::Io)
			}
		}
	}

	// THE LEVEL LAST SET, applied again with `_DOS` - after a sleep, or on a node handed again.
	fn restore(&mut self, when: &str) {
		self.notify_only();
		if let Some(level) = self.set {
			match self.apply(level) {
				Ok(_) => self.say(&format!("at {when} level {level} applied again")),
				Err(_) => self.say(&format!("at {when} level {level} could not be applied again")),
			}
		}
	}

	fn emit(&mut self, event: &BacklightEvent) {
		if self.events == 0 {
			return;
		}
		let mut frame = [0u8; 64];
		let mut handles = Handles::new();
		let Some(len) = backlight::events_frame(self.events_seq, event, &mut frame, &mut handles) else { return };
		if try_send(self.events, &frame[..len], 0) {
			self.events_seq = self.events_seq.wrapping_add(1);
		} else {
			self.say("DisplayService's stream did not take an event - it reads the level on its next call");
		}
	}

	// ONE `Notify` ON THE OUTPUT NODE.
	fn notified(&mut self, value: u64) {
		match acpi_video::notification(value) {
			Notification::Hotkey(key) => {
				// FIRMWARE THAT STEPPED ANYWAY: the level it moved to is the change, and the key is not forwarded.
				let now = if key == Hotkey::Zero { None } else { self.level() };
				if acpi_video::already_moved(self.set, now, key)
					&& let Some(level) = now
				{
					self.say(&format!("the firmware stepped to {level} itself on a hotkey ({value:#04x}) - reported as its change"));
					self.set = Some(level);
					self.emit(&BacklightEvent::Level(level));
					return;
				}
				self.emit(&BacklightEvent::Hotkey(match key {
					Hotkey::Cycle => BacklightHotkey::Cycle,
					Hotkey::Up => BacklightHotkey::Up,
					Hotkey::Down => BacklightHotkey::Down,
					Hotkey::Zero => BacklightHotkey::Zero,
				}));
			}
			Notification::DisplayOff => self.say("the firmware asked for the display off (0x89) - no level changes; display power is not the backlight's"),
			Notification::Other(other) => self.say(&format!("a Notify of {other:#04x}, which an output does not raise, is ignored")),
		}
	}
}

impl backlight::Service for Panel {
	fn firmware_display_id(&mut self) -> Result<Option<u32>, Error> {
		match self.evaluate("_ADR", &[]) {
			Ok(Some(Value::Integer(value))) => Ok(u32::try_from(value).ok()),
			Ok(None) => Ok(None),
			_ => Err(Error::Unsupported),
		}
	}

	fn describe(&mut self) -> Result<BacklightDescription, Error> {
		Ok(BacklightDescription { source: BacklightSource::Firmware, key: self.name.clone(), scale: BacklightScale::Levels(self.levels.levels.clone()), ac_default: self.levels.ac_default, battery_default: self.levels.battery_default, target: self.target.clone() })
	}

	fn get(&mut self) -> Result<u32, Error> {
		self.level().ok_or(Error::Unsupported)
	}

	fn set(&mut self, level: u32) -> Result<u32, Error> {
		self.apply(level)
	}

	fn events(&mut self) -> Vec<BacklightEvent> {
		Vec::new()
	}
}

// THE ADAPTER'S FUNCTION, as the namespace attached it to this row: the row found by its identity, and its firmware
// node's parent.
fn parent_function(identity: &[u8]) -> Option<BacklightFunction> {
	for index in 0..device_count() {
		let mut info = DeviceInfo::default();
		if !device_info(index, &mut info) || info.platform.kind != ROW_KIND_PLATFORM || info.platform.identity() != identity {
			continue;
		}
		let mut node = FirmwareNode::default();
		let answer = unsafe { syscall(SYS_DEVICE_NODE, index, &mut node as *mut FirmwareNode as u64, core::mem::size_of::<FirmwareNode>() as u64, 0) } as i64;
		if answer != 0 || node.flags & FIRMWARE_NODE_PARENT == 0 {
			return None;
		}
		return Some(BacklightFunction { bus: u32::from(node.parent_bus), dev: u32::from(node.parent_dev), func: u32::from(node.parent_func) });
	}
	None
}

// THE SLEEP: nothing evaluated until the resume, and then `_DOS` and the level again.
struct Sleep<'a> {
	panel: &'a mut Panel,
	serving: &'a mut common::Serving,
}

impl common::SleepStep for Sleep<'_> {
	fn suspend(&mut self, _request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		self.panel.say("suspended");
		driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
	}

	fn resume(&mut self, _lost_power: bool) -> bool {
		self.panel.say("resumed");
		self.panel.restore("the resume");
		true
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(self.serving)
	}
}

// ONE REQUEST FROM DISPLAYSERVICE: the event stream opened, or a call.
fn serve(panel: &mut Panel, channel: u64, buf: &mut [u8]) -> bool {
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	if len >= 2 && u16::from_le_bytes([buf[0], buf[1]]) == backlight::OP_EVENTS {
		let Some((corr, _)) = backlight::events_open(panel, &buf[..len], &mut handles) else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		if panel.events != 0 {
			close(panel.events);
		}
		panel.events = producer;
		panel.events_seq = 0;
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		return true;
	}
	let mut reply = [0u8; 1024];
	let mut reply_handles = Handles::new();
	let written = backlight::dispatch(panel, &buf[..len], &mut handles, &mut reply, &mut reply_handles);
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	if let Some(written) = written {
		send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
	}
	true
}

// THE NODE'S `Notify` STREAM, 0 when the node gives none.
fn subscribe(node: u64) -> u64 {
	acpi_node::Client::with_deadline(ChannelTransport { chan: node }, clock() + TICKS).notifications().unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, _resources) = common::handshake(bootstrap);
	let name = String::from_utf8_lossy(bind.info.platform.identity()).into_owned();
	if !bind.info.platform.match_ids().iter().any(|id| id.text() == b"video-output") {
		print(format!("driver.acpi-backlight: {name}: its ids do not name a video output\n").as_bytes());
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::UnsupportedDevice);
	}
	common::takes_sleep();
	// ONLINE FIRST: the node channel is answered only to a binding that is online.
	let report = format!("driver.acpi-backlight: {name}: online");
	let Some((producer, consumer)) = channel() else { exit() };
	let mut serving = common::Serving::from_offers(&[(TOKEN, producer)]);
	if !common::online(bootstrap, &bind, report.as_bytes(), &[(driver_protocol::provider::BACKLIGHT, consumer)]) {
		exit();
	}
	if !common::request_node(bootstrap, &bind) || common::wait_node_or_answer(bootstrap, &bind, &[]).is_none() {
		if common::stop_requested() {
			common::finish_stop(bootstrap, &bind, 0, true);
		}
		exit();
	}
	let node = common::node().unwrap_or(0);
	let target = match parent_function(bind.info.platform.identity()) {
		Some(function) => BacklightTarget::Function(function),
		None => BacklightTarget::None,
	};
	let mut panel = Panel { name, node, levels: Levels { ac_default: None, battery_default: None, levels: Vec::new() }, target, set: None, has_bqc: true, snapped_said: false, has_dos: None, events: 0, events_seq: 0 };
	if node == 0 {
		panel.say("the firmware describes no node for it");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	// `_BCL`, ONCE: the levels this backlight is published with.
	match panel.evaluate("_BCL", &[]).and_then(|value| value.ok_or_else(|| String::from("there is no _BCL"))) {
		Ok(value) => match acpi_video::bcl(&value) {
			Ok(levels) => panel.levels = levels,
			Err(refusal) => {
				panel.say(&format!("_BCL is not a usable list - {refusal:?}; nothing is published"));
				common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::UnsupportedDevice);
			}
		},
		Err(why) => {
			panel.say(&format!("_BCL could not be read - {why}"));
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::DeviceNotResponding);
		}
	}
	panel.notify_only();
	let found = panel.level();
	let joined = match &panel.target {
		BacklightTarget::Function(function) => format!("the adapter at {:02x}:{:02x}.{}", function.bus, function.dev, function.func),
		_ => String::from("no adapter function"),
	};
	panel.say(&format!("{} level(s) {:?}, AC default {:?}, battery default {:?}, at {:?}; {joined}", panel.levels.levels.len(), panel.levels.levels, panel.levels.ac_default, panel.levels.battery_default, found));
	let mut notifications = subscribe(panel.node);
	let mut buf = alloc::vec![0u8; 1024];
	loop {
		let looking = if panel.node == 0 { clock() + NODE_LOOK_TICKS } else { u64::MAX };
		let mut handles: Vec<u64> = Vec::new();
		if notifications != 0 {
			handles.push(notifications);
		}
		let consumers_at = handles.len();
		handles.extend_from_slice(serving.as_slice());
		let Some(ready) = common::wait_or_answer_until(bootstrap, &bind, &handles, looking, Some(&mut serving)) else {
			if common::stop_requested() {
				common::finish_stop(bootstrap, &bind, 0, true);
			}
			exit();
		};
		// THE SLEEP, between two requests: a `SUSPEND` handed back as "nothing ready".
		if ready.is_none() && !common::take_sleep_step(bootstrap, &bind, &mut Sleep { panel: &mut panel, serving: &mut serving }) {
			if common::stop_requested() {
				common::finish_stop(bootstrap, &bind, 0, true);
			}
			exit();
		}
		match common::fresh_node() {
			Some(0) => panel.say("the firmware describes no node for it any more"),
			Some(node) => {
				panel.node = node;
				if notifications != 0 {
					close(notifications);
				}
				notifications = subscribe(node);
				panel.restore("a node handed again");
			}
			None => {}
		}
		let Some(at) = ready else { continue };
		if at >= consumers_at {
			let index = at - consumers_at;
			let channel = serving.at(index);
			if !serve(&mut panel, channel, &mut buf) {
				let token = serving.close_at(index);
				if panel.events != 0 {
					close(panel.events);
					panel.events = 0;
				}
				if !common::disconnected(bootstrap, &bind, token) {
					exit();
				}
			}
			continue;
		}
		loop {
			match try_recv_caps(notifications, &mut buf) {
				PolledCaps::Message { len, handles } => {
					for &leftover in handles.as_slice() {
						close(leftover);
					}
					let mut frame = Handles::new();
					let Some(notification) = acpi_node::notifications_read(&buf[..len], &mut frame) else { continue };
					panel.notified(u64::from(notification.value));
				}
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					close(notifications);
					notifications = 0;
					panel.node = 0;
					panel.say("its node's notifications ended - the ACPI service is gone; waiting for its node again");
					break;
				}
			}
		}
	}
}
