// THE ACPI BUTTONS AND THE LID: one binding per namespace node - a control-method power button (`PNP0C0C`), a
// control-method sleep button (`PNP0C0E`) or a lid (`PNP0C0D`) - on the node-scoped channel the ACPI service serves; and
// one per fixed-hardware button the kernel declares a row for (`LNXPWRBN`, `LNXSLPBN`), which has no node.
//
// EVERY ONE PUBLISHES A `platform-switch` PROVIDER, whose consumer - the power-state service - decides what a press or
// a closed lid does. This driver decides nothing about either while a consumer watches.
//
// A BUTTON'S PRESS IS ITS `Notify(0x80)`, or for a fixed one DeviceManager's `PRESSED` - the PM1 event the kernel
// decoded. Each press is one frame `closed` with the sequence advanced; the button's state is open otherwise, and the
// watch opens on that. WITH NO CONSUMER - none watches, or its stream is full or gone - the driver carries the press out
// itself, as it did before any policy existed, so a press never does nothing: the power button through the
// `system-power` connection DeviceManager hands the Power-key drivers, which powers the machine off; the sleep button by
// asking ServiceManager for a suspend through the `system-sleep` connection DeviceManager mints for it - to RAM where
// the machine offers it, to idle otherwise - answered at acceptance, since this driver is a participant the transaction
// then waits on. `Notify(0x02)` is the button having WOKEN the machine: said, and never taken as a second request.
//
// THE LID: `_LID` read at bind, at each `Notify(0x80)` and at every resume - a lid closed or opened while the machine
// slept is a change like any other - and every change sent to its consumer.
//
// THE SLEEP: asked to arm wake, it answers that it did - the ACPI service arms the node's `_PRW` event in the platform's
// step - and holds nothing, since it has no device of its own to stop. Its pure parts are `drivers::acpi_button`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use drivers::acpi_button::{self, Actor, Class, Event, Source};
use drivers::common;
use drivers::node_power;
use ipc_client::ChannelTransport;
use proto::system::{Error, SleepReason, SleepState, SwitchKind, SwitchState, acpi_node, platform_switch, system_power, system_sleep};
use rt::*;
use wire::Handles;

// How long one question to the node, or to ServiceManager, may take.
const TICKS: u64 = TICKS_PER_SECOND * 5;
// The switch's one publication.
const TOKEN: u16 = 0;
// Deep enough that a lid opened and closed, or a button pressed, faster than the policy reads never blocks this driver.
const STREAM_DEPTH: u64 = 32;
// How often a binding whose node ended looks for the one handed again.
const NODE_LOOK_TICKS: u64 = TICKS_PER_SECOND;

struct Button {
	name: String,
	class: Class,
	source: Source,
	// The node's channel; 0 for a fixed button, which has none.
	node: u64,
	// The Power key's and the sleep button's doors, as DeviceManager handed them.
	syspower: u64,
	syssleep: u64,
	// The lid's last reading, the change count - a button's presses - and the consumer's stream.
	closed: Option<bool>,
	sequence: u32,
	stream: u64,
}

impl Button {
	// THE NODE'S POWER STATE: one that could not be entered is said, and the device is served as it is. A fixed button
	// has no node, and no power state of its own.
	fn power_state(&self, state: u8) {
		if self.node == 0 {
			return;
		}
		if let Err(why) = node_power::set(self.node, state) {
			self.say(&why);
		}
	}

	fn say(&self, text: &str) {
		let line = format!("driver.acpi-button: {}: {text}\n", self.name);
		print(line.as_bytes());
	}

	fn read_lid(&self) -> Result<bool, String> {
		match acpi_node::Client::with_deadline(ChannelTransport { chan: self.node }, clock() + TICKS).evaluate("_LID", &[]) {
			Some(Ok(bytes)) => match aml::wire::decode(&bytes) {
				Ok(value) => acpi_button::lid_closed(&value).map_err(String::from),
				Err(error) => Err(format!("_LID did not decode - {error:?}")),
			},
			Some(Err(Error::NotFound)) => Err(String::from("the node has no _LID")),
			Some(Err(error)) => Err(format!("_LID was refused - {error:?}")),
			None => Err(String::from("the ACPI service did not answer _LID")),
		}
	}

	fn kind(&self) -> SwitchKind {
		match self.class {
			Class::Lid => SwitchKind::Lid,
			Class::PowerButton => SwitchKind::PowerButton,
			Class::SleepButton => SwitchKind::SleepButton,
		}
	}

	// THE STATE NOW: the lid as last read; a button open - a press is a frame of its own.
	fn state(&self) -> SwitchState {
		SwitchState { kind: self.kind(), closed: self.class == Class::Lid && self.closed.unwrap_or(false), sequence: self.sequence }
	}

	// One frame on the consumer's stream: whether it was taken.
	fn emit(&mut self, state: &SwitchState) -> bool {
		if self.stream == 0 {
			return false;
		}
		let mut frame = [0u8; 64];
		let mut handles = Handles::new();
		let Some(len) = platform_switch::watch_frame(state.sequence, state, &mut frame, &mut handles) else { return false };
		if try_send(self.stream, &frame[..len], 0) {
			return true;
		}
		if self.class == Class::Lid {
			self.say("the consumer's stream is full - the change waits in the state it reads next");
		}
		false
	}

	// THE LID READ AGAIN, and a change sent: at a `Notify`, at the resume, and at a node handed again.
	fn refresh_lid(&mut self) {
		let closed = match self.read_lid() {
			Ok(closed) => closed,
			Err(why) => {
				self.say(&format!("the lid could not be read - {why}"));
				return;
			}
		};
		if self.closed == Some(closed) {
			return;
		}
		self.closed = Some(closed);
		self.sequence = self.sequence.wrapping_add(1);
		self.say(if closed { "the lid is closed" } else { "the lid is open" });
		let state = self.state();
		self.emit(&state);
	}

	// A PRESS: told to the consumer, which decides; with none, the power button powers the machine off and the sleep button
	// asks for a suspend, as before any policy existed. The lid is read again.
	fn pressed(&mut self) {
		if self.class == Class::Lid {
			self.refresh_lid();
			return;
		}
		self.sequence = self.sequence.wrapping_add(1);
		let press = SwitchState { kind: self.kind(), closed: true, sequence: self.sequence };
		let watched = self.stream != 0;
		let delivered = self.emit(&press);
		let why = match acpi_button::actor(watched, delivered) {
			Actor::Consumer => {
				self.say("pressed - told the power-state policy, which decides what it does");
				return;
			}
			Actor::Driver if watched => "the policy's stream did not take it",
			Actor::Driver => "no policy watches it",
		};
		match self.class {
			Class::PowerButton => {
				if self.syspower == 0 {
					self.say(&format!("pressed - {why}, and this binding holds no system-power connection, so nothing is done"));
					return;
				}
				self.say(&format!("pressed - {why}; asking the power service to stop the machine"));
				let _ = system_power::Client::with_deadline(ChannelTransport { chan: self.syspower }, clock() + TICKS).power_off();
			}
			Class::SleepButton => {
				if self.syssleep == 0 {
					self.say(&format!("pressed - {why}, and this binding holds no system-sleep connection, so nothing is done"));
					return;
				}
				let state = if sleep_states() & (1 << SLEEP_STATE_RAM) != 0 { SleepState::Ram } else { SleepState::Idle };
				self.say(&format!("pressed - {why}; asking ServiceManager for a suspend"));
				match system_sleep::Client::with_deadline(ChannelTransport { chan: self.syssleep }, clock() + TICKS).suspend(&state, &0, &SleepReason::SleepButton) {
					Some(Ok(())) => {}
					Some(Err(Error::Again)) => self.say("a sleep is already running - the press is answered by it"),
					Some(Err(error)) => self.say(&format!("ServiceManager refused the suspend - {error:?}")),
					None => self.say("ServiceManager did not answer"),
				}
			}
			Class::Lid => {}
		}
	}
}

// THE SWITCH'S PROVIDER: its state now - a lid once it was read, a button always, open - then every change.
impl platform_switch::Service for Button {
	fn watch(&mut self) -> Vec<SwitchState> {
		if self.class != Class::Lid || self.closed.is_some() { alloc::vec![self.state()] } else { Vec::new() }
	}
}

// THE SLEEP: wake armed when asked - the platform's step arms the node's `_PRW` event - and the node put in the state the
// sleep lets it wake from; D0 again at the resume, before the lid is read.
struct Sleep<'a> {
	button: &'a mut Button,
	serving: &'a mut common::Serving,
}

impl common::SleepStep for Sleep<'_> {
	fn suspend(&mut self, request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		let outcome = if request.arm_wake { driver_protocol::SuspendOutcome::DoneWakeArmed } else { driver_protocol::SuspendOutcome::Done };
		self.button.power_state(node_power::for_sleep(self.button.node, request.state, request.arm_wake));
		driver_protocol::Suspended { outcome, awake_by_ms: 0 }
	}

	fn resume(&mut self, _lost_power: bool) -> bool {
		self.button.power_state(node_power::D0);
		// THE LID MAY HAVE MOVED WHILE THE MACHINE SLEPT: read again.
		if self.button.class == Class::Lid {
			self.button.refresh_lid();
		}
		true
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(self.serving)
	}
}

// THE NODE'S `Notify` STREAM, 0 when the node gives none.
fn subscribe(node: u64) -> u64 {
	acpi_node::Client::with_deadline(ChannelTransport { chan: node }, clock() + TICKS).notifications().unwrap_or(0)
}

// ONE REQUEST FROM THE SWITCH'S CONSUMER: the watch opened - its snapshot sent first - or nothing else this contract has.
fn serve(button: &mut Button, channel: u64, buf: &mut [u8]) -> bool {
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	if len >= 2 && u16::from_le_bytes([buf[0], buf[1]]) == platform_switch::OP_WATCH {
		let Some((corr, items)) = platform_switch::watch_open(button, &buf[..len], &mut handles) else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		if button.stream != 0 {
			close(button.stream);
		}
		button.stream = producer;
		for item in &items {
			button.emit(item);
		}
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		return true;
	}
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	true
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	let name = String::from_utf8_lossy(bind.info.platform.identity()).into_owned();
	let Some((class, source)) = acpi_button::class_of(bind.info.platform.match_ids().iter().map(|id| id.text())) else {
		print(format!("driver.acpi-button: {name}: its ids name none of a power button, a sleep button or a lid\n").as_bytes());
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::UnsupportedDevice);
	};
	common::takes_sleep();
	// ONLINE FIRST: the node channel is answered only to a binding that is online. Every one publishes its switch.
	let report = if source == Source::Fixed { format!("driver.acpi-button: {name}: online ({class:?}, fixed hardware)") } else { format!("driver.acpi-button: {name}: online ({class:?})") };
	let Some((producer, consumer)) = channel() else { exit() };
	let mut serving = common::Serving::from_offers(&[(TOKEN, producer)]);
	if !common::online(bootstrap, &bind, report.as_bytes(), &[(driver_protocol::provider::PLATFORM_SWITCH, consumer)]) {
		exit();
	}
	// A FIXED BUTTON HAS NO NODE TO ASK FOR: its presses arrive on the control channel.
	if source == Source::Node && (!common::request_node(bootstrap, &bind) || common::wait_node_or_answer(bootstrap, &bind, &[]).is_none()) {
		if common::stop_requested() {
			common::finish_stop(bootstrap, &bind, 0, true);
		}
		exit();
	}
	let node = if source == Source::Node { common::node().unwrap_or(0) } else { 0 };
	let mut button = Button { name, class, source, node, syspower: resources.syspower, syssleep: resources.syssleep, closed: None, sequence: 0, stream: 0 };
	if source == Source::Node && node == 0 {
		button.say("the firmware describes no node for it");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	button.power_state(node_power::D0);
	if class == Class::Lid {
		button.refresh_lid();
		if button.closed.is_none() {
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::DeviceNotResponding);
		}
	}
	let mut notifications = if node != 0 { subscribe(node) } else { 0 };
	let mut buf = alloc::vec![0u8; 1024];
	loop {
		let looking = if button.node == 0 && button.source == Source::Node { clock() + NODE_LOOK_TICKS } else { u64::MAX };
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
		// THE SLEEP, between two notifications: a `SUSPEND` handed back as "nothing ready".
		if ready.is_none() && !common::take_sleep_step(bootstrap, &bind, &mut Sleep { button: &mut button, serving: &mut serving }) {
			if common::stop_requested() {
				common::finish_stop(bootstrap, &bind, 0, true);
			}
			exit();
		}
		// A FIXED BUTTON'S PRESSES, as DeviceManager handed them.
		for _ in 0..common::presses() {
			button.pressed();
		}
		match common::fresh_node() {
			Some(0) => button.say("the firmware describes no node for it any more"),
			Some(node) => {
				button.node = node;
				// A RESTARTED SERVICE COUNTS FROM NOTHING: the state asked again, so the resources it shares are held.
				button.power_state(node_power::D0);
				if notifications != 0 {
					close(notifications);
				}
				notifications = subscribe(node);
				if class == Class::Lid {
					button.say("its node is handed again - reading it");
					button.refresh_lid();
				}
			}
			None => {}
		}
		let Some(at) = ready else { continue };
		if at >= consumers_at {
			let index = at - consumers_at;
			let channel = serving.at(index);
			if !serve(&mut button, channel, &mut buf) {
				let token = serving.close_at(index);
				if button.stream != 0 {
					close(button.stream);
					button.stream = 0;
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
					match acpi_button::event_of(notification.value) {
						Event::Pressed => button.pressed(),
						Event::Woke => button.say("it woke the machine"),
						Event::Other(value) => button.say(&format!("a Notify of {value:#04x}, which it does not raise, is ignored")),
					}
				}
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					close(notifications);
					notifications = 0;
					button.node = 0;
					button.say("its node's notifications ended - the ACPI service is gone; waiting for its node again");
					break;
				}
			}
		}
	}
}
