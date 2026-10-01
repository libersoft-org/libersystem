// THE ACPI BUTTONS AND THE LID: one binding per namespace node - a control-method power button (`PNP0C0C`), a
// control-method sleep button (`PNP0C0E`) or a lid (`PNP0C0D`) - on the node-scoped channel the ACPI service serves.
//
// A BUTTON'S PRESS IS ITS `Notify(0x80)`. The power button's is carried out as the fixed one's is: through the
// `system-power` connection DeviceManager hands the Power-key drivers, so it powers the machine off exactly as the
// fixed button does. The sleep button's asks ServiceManager for a suspend through the `system-sleep` connection
// DeviceManager mints for it - to RAM where the machine offers it, to idle otherwise - answered at acceptance, since
// this driver is a participant the transaction then waits on. `Notify(0x02)` is the button having WOKEN the machine:
// said, and never taken as a second request.
//
// THE LID publishes a `platform-switch` provider: `_LID` read at bind, at each `Notify(0x80)` and at every resume - a lid
// closed or opened while the machine slept is a change like any other - and every change sent to its consumer, the
// power-state service, whose policy decides what a closed lid does. This driver decides nothing about it.
//
// THE SLEEP: asked to arm wake, it answers that it did - the ACPI service arms the node's `_PRW` event in the platform's
// step - and holds nothing, since it has no device of its own to stop. Its pure parts are `drivers::acpi_button`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use drivers::acpi_button::{self, Class, Event};
use drivers::common;
use drivers::node_power;
use ipc_client::ChannelTransport;
use proto::system::{Error, SleepReason, SleepState, SwitchKind, SwitchState, acpi_node, platform_switch, system_power, system_sleep};
use rt::*;
use wire::Handles;

// How long one question to the node, or to ServiceManager, may take.
const TICKS: u64 = TICKS_PER_SECOND * 5;
// The lid's one publication.
const TOKEN: u16 = 0;
// Deep enough that a lid opened and closed faster than the policy reads never blocks this driver.
const STREAM_DEPTH: u64 = 32;
// How often a binding whose node ended looks for the one handed again.
const NODE_LOOK_TICKS: u64 = TICKS_PER_SECOND;

struct Button {
	name: String,
	class: Class,
	node: u64,
	// The Power key's and the sleep button's doors, as DeviceManager handed them.
	syspower: u64,
	syssleep: u64,
	// The lid's last reading, its change count, and the consumer's stream.
	closed: Option<bool>,
	sequence: u32,
	stream: u64,
}

impl Button {
	// THE NODE'S POWER STATE: one that could not be entered is said, and the device is served as it is.
	fn power_state(&self, state: u8) {
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

	fn state(&self) -> SwitchState {
		SwitchState { kind: SwitchKind::Lid, closed: self.closed.unwrap_or(false), sequence: self.sequence }
	}

	// One frame on the consumer's stream.
	fn emit(&mut self, state: &SwitchState) {
		if self.stream == 0 {
			return;
		}
		let mut frame = [0u8; 64];
		let mut handles = Handles::new();
		if let Some(len) = platform_switch::watch_frame(state.sequence, state, &mut frame, &mut handles)
			&& !try_send(self.stream, &frame[..len], 0)
		{
			self.say("the consumer's stream is full - the change waits in the state it reads next");
		}
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

	// A PRESS: the power button powers the machine off as the fixed one does; the sleep button asks for a suspend; the lid
	// is read again.
	fn pressed(&mut self) {
		match self.class {
			Class::PowerButton => {
				if self.syspower == 0 {
					self.say("pressed - this binding holds no system-power connection, so nothing is done");
					return;
				}
				self.say("pressed - asking the power service to stop the machine");
				let _ = system_power::Client::with_deadline(ChannelTransport { chan: self.syspower }, clock() + TICKS).power_off();
			}
			Class::SleepButton => {
				if self.syssleep == 0 {
					self.say("pressed - this binding holds no system-sleep connection, so nothing is done");
					return;
				}
				let state = if sleep_states() & (1 << SLEEP_STATE_RAM) != 0 { SleepState::Ram } else { SleepState::Idle };
				self.say("pressed - asking ServiceManager for a suspend");
				match system_sleep::Client::with_deadline(ChannelTransport { chan: self.syssleep }, clock() + TICKS).suspend(&state, &0, &SleepReason::SleepButton) {
					Some(Ok(())) => {}
					Some(Err(Error::Again)) => self.say("a sleep is already running - the press is answered by it"),
					Some(Err(error)) => self.say(&format!("ServiceManager refused the suspend - {error:?}")),
					None => self.say("ServiceManager did not answer"),
				}
			}
			Class::Lid => self.refresh_lid(),
		}
	}
}

// THE LID'S PROVIDER: its state now, then every change.
impl platform_switch::Service for Button {
	fn watch(&mut self) -> Vec<SwitchState> {
		if self.closed.is_some() { alloc::vec![self.state()] } else { Vec::new() }
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

// ONE REQUEST FROM THE LID'S CONSUMER: the watch opened - its snapshot sent first - or nothing else this contract has.
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
	let Some(class) = acpi_button::class_of(bind.info.platform.match_ids().iter().map(|id| id.text())) else {
		print(format!("driver.acpi-button: {name}: its ids name none of a power button, a sleep button or a lid\n").as_bytes());
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::UnsupportedDevice);
	};
	common::takes_sleep();
	// ONLINE FIRST: the node channel is answered only to a binding that is online. The lid publishes its switch.
	let report = format!("driver.acpi-button: {name}: online ({class:?})");
	let (mut serving, online) = if class == Class::Lid {
		let Some((producer, consumer)) = channel() else { exit() };
		(common::Serving::from_offers(&[(TOKEN, producer)]), common::online(bootstrap, &bind, report.as_bytes(), &[(driver_protocol::provider::PLATFORM_SWITCH, consumer)]))
	} else {
		(common::Serving::from_offers(&[]), common::online(bootstrap, &bind, report.as_bytes(), &[]))
	};
	if !online {
		exit();
	}
	if !common::request_node(bootstrap, &bind) || common::wait_node_or_answer(bootstrap, &bind, &[]).is_none() {
		if common::stop_requested() {
			common::finish_stop(bootstrap, &bind, 0, true);
		}
		exit();
	}
	let node = common::node().unwrap_or(0);
	let mut button = Button { name, class, node, syspower: resources.syspower, syssleep: resources.syssleep, closed: None, sequence: 0, stream: 0 };
	if node == 0 {
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
	let mut notifications = subscribe(node);
	let mut buf = alloc::vec![0u8; 1024];
	loop {
		let looking = if button.node == 0 { clock() + NODE_LOOK_TICKS } else { u64::MAX };
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
