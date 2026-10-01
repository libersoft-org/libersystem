// THE ACPI FAN DRIVER: one binding per `PNP0C0B` node, its methods evaluated over the node-scoped channel the ACPI
// service serves, and the fan published as ONE `cooling-device` provider to ProcessorPowerService alone, on whose
// command alone it changes the fan's speed.
//
// TWO KINDS OF FAN. An ACPI 4.0 fan has `_FIF` - whether the platform leaves its speed to the operating system in fine
// steps - `_FPS`'s levels, `_FSL` to set one and `_FST` to read it; it is set by `_FSL`. An ACPI 1.0 fan has none of them
// and is switched by its device power state: D0 on, D3hot off, asked for on the node channel, whose power resources the
// ACPI service counts.
//
// ACROSS A SLEEP: `SUSPEND` finishes the evaluation in hand - this driver is single-threaded, so there is none past the
// one answered - and takes no command until `RESUME`, then puts the node in the state the sleep lets it enter.
// `RESUME` brings the node back to D0, reads `_FST` again and applies the level last commanded again - firmware may
// reset a fan across a sleep - saying the level it found.
//
// WHEN THE ACPI SERVICE RESTARTS its node channels end with it: the node is asked for again (`drivers::common`) and the
// level last commanded applied again on the one the new instance serves.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aml::wire::Value;
use drivers::acpi_fan;
use drivers::{common, node_power};
use ipc_client::ChannelTransport;
use proto::system::{Error, FanDescription, FanLevel, FanStatus, acpi_node, cooling_device};
use rt::*;
use wire::Handles;

// How long one question to the node may take.
const TICKS: u64 = TICKS_PER_SECOND * 5;
// The one publication's token.
const TOKEN: u16 = 0;
// How often a binding whose node ended looks for the one handed again.
const NODE_LOOK_TICKS: u64 = TICKS_PER_SECOND;

struct Fan {
	name: String,
	node: u64,
	description: FanDescription,
	// The level the policy commanded last, applied again after a sleep and on a new node.
	commanded: Option<u32>,
}

impl Fan {
	fn say(&self, text: &str) {
		print(format!("driver.acpi-fan: {}: {text}\n", self.name).as_bytes());
	}

	// A method of the node's with integer arguments: its value, `None` when the node does not have it.
	fn evaluate(&self, method: &str, arguments: &[u64]) -> Result<Option<Value>, String> {
		let arguments = if arguments.is_empty() { Vec::new() } else { aml::wire::encode(&Value::Package(arguments.iter().map(|value| Value::Integer(*value)).collect())).map_err(|error| format!("{method}'s arguments did not encode - {error:?}"))? };
		match acpi_node::Client::with_deadline(ChannelTransport { chan: self.node }, clock() + TICKS).evaluate(method, &arguments) {
			Some(Ok(bytes)) => aml::wire::decode(&bytes).map(Some).map_err(|error| format!("{method} did not decode - {error:?}")),
			Some(Err(Error::NotFound)) => Ok(None),
			Some(Err(error)) => Err(format!("{method} was refused - {error:?}")),
			None => Err(format!("{method} was not answered")),
		}
	}

	// THE FAN AS ITS OBJECTS DESCRIBE IT: `_FIF` and `_FPS` for an ACPI 4.0 fan, neither for an ACPI 1.0 one.
	fn read_description(&self, path: String) -> Result<FanDescription, String> {
		let interface = match self.evaluate("_FIF", &[])? {
			Some(value) => Some(acpi_fan::fif(&value).map_err(|refusal| format!("_FIF: {refusal:?}"))?),
			None => None,
		};
		let levels = match self.evaluate("_FPS", &[])? {
			Some(value) => acpi_fan::fps(&value).map_err(|refusal| format!("_FPS: {refusal:?}"))?,
			None => Vec::new(),
		};
		let by_power_state = interface.is_none() && levels.is_empty();
		Ok(FanDescription { path, by_power_state, fine_grain: interface.is_some_and(|interface| interface.fine_grain), step_size: interface.map_or(0, |interface| interface.step), levels: levels.iter().map(|level| FanLevel { control: level.control, speed_rpm: level.speed_rpm, noise: level.noise, power_mw: level.power_mw }).collect() })
	}

	// `_FST` - or, for a fan switched by its power state, the state last commanded.
	fn read_status(&self) -> Result<FanStatus, String> {
		if self.description.by_power_state {
			return Ok(FanStatus { control: self.commanded.unwrap_or(0), speed_rpm: 0 });
		}
		let value = self.evaluate("_FST", &[])?.ok_or_else(|| String::from("the fan has no _FST"))?;
		let state = acpi_fan::fst(&value).map_err(|refusal| format!("_FST: {refusal:?}"))?;
		Ok(FanStatus { control: state.control, speed_rpm: state.speed_rpm })
	}

	// A LEVEL APPLIED: `_FSL`, or D0 for any level above zero and D3hot for zero.
	fn apply(&self, level: u32) -> Result<FanStatus, String> {
		if self.description.by_power_state {
			node_power::set(self.node, if level > 0 { node_power::D0 } else { node_power::D3_HOT })?;
			return Ok(FanStatus { control: u32::from(level > 0), speed_rpm: 0 });
		}
		self.evaluate("_FSL", &[u64::from(level)])?.ok_or_else(|| String::from("the fan has no _FSL"))?;
		self.read_status()
	}
}

impl cooling_device::Service for Fan {
	fn describe(&mut self) -> Result<FanDescription, Error> {
		Ok(self.description.clone())
	}

	fn set_level(&mut self, level: u32) -> Result<FanStatus, Error> {
		if self.description.fine_grain && level > 100 {
			return Err(Error::Invalid);
		}
		if !self.description.by_power_state && !self.description.fine_grain && !self.description.levels.iter().any(|known| known.control == level) {
			return Err(Error::Invalid);
		}
		match self.apply(level) {
			Ok(status) => {
				self.commanded = Some(level);
				Ok(status)
			}
			Err(why) => {
				self.say(&why);
				Err(Error::Io)
			}
		}
	}

	fn status(&mut self) -> Result<FanStatus, Error> {
		self.read_status().map_err(|why| {
			self.say(&why);
			Error::Io
		})
	}
}

// THE SLEEP: the node in the state the sleep lets it enter, and at the resume the level last commanded again.
struct Sleep<'a> {
	fan: &'a mut Fan,
	serving: &'a mut common::Serving,
}

impl common::SleepStep for Sleep<'_> {
	fn suspend(&mut self, request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		if let Err(why) = node_power::set(self.fan.node, node_power::for_sleep(self.fan.node, request.state, false)) {
			self.fan.say(&why);
		}
		driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
	}

	fn resume(&mut self, _lost_power: bool) -> bool {
		if let Err(why) = node_power::set(self.fan.node, node_power::D0) {
			self.fan.say(&why);
		}
		self.fan.restore("the resume");
		true
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(self.serving)
	}
}

impl Fan {
	// WHAT THE FAN IS AT, read, and the level last commanded applied again - after a sleep, or on a node handed again.
	fn restore(&mut self, when: &str) {
		let found = match self.read_status() {
			Ok(status) => format!("level {} ({} rpm)", status.control, status.speed_rpm),
			Err(why) => format!("nothing readable ({why})"),
		};
		match self.commanded {
			Some(level) => match self.apply(level) {
				Ok(_) => self.say(&format!("at {when} it was found at {found} - level {level} applied again")),
				Err(why) => self.say(&format!("at {when} it was found at {found} - level {level} could not be applied again: {why}")),
			},
			None => self.say(&format!("at {when} it was found at {found} - no level was commanded")),
		}
	}
}

// ONE REQUEST FROM THE POLICY.
fn serve(fan: &mut Fan, channel: u64, buf: &mut [u8]) -> bool {
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	let mut reply = [0u8; 1024];
	let mut reply_handles = Handles::new();
	let written = cooling_device::dispatch(fan, &buf[..len], &mut handles, &mut reply, &mut reply_handles);
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	if let Some(written) = written {
		send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
	}
	true
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, _resources) = common::handshake(bootstrap);
	let name = String::from_utf8_lossy(bind.info.platform.identity()).into_owned();
	if !bind.info.platform.match_ids().iter().any(|id| id.text() == b"PNP0C0B") {
		print(format!("driver.acpi-fan: {name}: its ids do not name a fan\n").as_bytes());
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::UnsupportedDevice);
	}
	common::takes_sleep();
	// ONLINE FIRST: the node channel is answered only to a binding that is online.
	let report = format!("driver.acpi-fan: {name}: online");
	let Some((producer, consumer)) = channel() else { exit() };
	let mut serving = common::Serving::from_offers(&[(TOKEN, producer)]);
	if !common::online(bootstrap, &bind, report.as_bytes(), &[(driver_protocol::provider::COOLING_DEVICE, consumer)]) {
		exit();
	}
	if !common::request_node(bootstrap, &bind) || common::wait_node_or_answer(bootstrap, &bind, &[]).is_none() {
		if common::stop_requested() {
			common::finish_stop(bootstrap, &bind, 0, true);
		}
		exit();
	}
	let node = common::node().unwrap_or(0);
	let path = drivers::acpi_power::namespace_path(name.strip_prefix("acpi:").unwrap_or(&name));
	let mut fan = Fan { name, node, description: FanDescription { path: path.clone(), by_power_state: true, fine_grain: false, step_size: 0, levels: Vec::new() }, commanded: None };
	if node == 0 {
		fan.say("the firmware describes no node for it");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	match fan.read_description(path) {
		Ok(description) => fan.description = description,
		Err(why) => {
			fan.say(&format!("it could not be described - {why}"));
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::DeviceNotResponding);
		}
	}
	fan.say(&if fan.description.by_power_state { String::from("an ACPI 1.0 fan, switched by its device power state") } else { format!("{} level(s){}", fan.description.levels.len(), if fan.description.fine_grain { format!(", fine-grain control in steps of {} %", fan.description.step_size) } else { String::new() }) });
	let mut buf = alloc::vec![0u8; 1024];
	loop {
		let looking = if fan.node == 0 { clock() + NODE_LOOK_TICKS } else { u64::MAX };
		let handles: Vec<u64> = serving.as_slice().to_vec();
		let Some(ready) = common::wait_or_answer_until(bootstrap, &bind, &handles, looking, Some(&mut serving)) else {
			if common::stop_requested() {
				common::finish_stop(bootstrap, &bind, 0, true);
			}
			exit();
		};
		// THE SLEEP, between two commands: a `SUSPEND` handed back as "nothing ready".
		if ready.is_none() && !common::take_sleep_step(bootstrap, &bind, &mut Sleep { fan: &mut fan, serving: &mut serving }) {
			if common::stop_requested() {
				common::finish_stop(bootstrap, &bind, 0, true);
			}
			exit();
		}
		match common::fresh_node() {
			Some(0) => fan.say("the firmware describes no node for it any more"),
			Some(node) => {
				fan.node = node;
				fan.restore("a node handed again");
			}
			None => {}
		}
		let Some(index) = ready else { continue };
		let channel = serving.at(index);
		if !serve(&mut fan, channel, &mut buf) {
			let token = serving.close_at(index);
			if !common::disconnected(bootstrap, &bind, token) {
				exit();
			}
		}
	}
}
