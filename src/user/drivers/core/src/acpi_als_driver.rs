// THE ACPI AMBIENT-LIGHT SENSOR: one binding per `ACPI0008` node, its methods evaluated over the node-scoped channel the
// ACPI service serves, published as ONE `ambient-light` provider to the brightness policy alone.
//
// `_ALI` IS THE READING, in lux; `_ALR` THE RESPONSE CURVE, pairs of the display adjustment and the illuminance it
// applies at, which the policy follows; `_ALP` THE POLLING INTERVAL, where the firmware sends no notification. A
// `Notify` of 0x80 says the illuminance changed - `_ALI` is read and sent - and 0x82 that the curve did - `_ALR` is read
// again. The colour methods `_ALC` and `_ALT` are not read.
//
// THE STREAM'S FIRST ITEM IS THE READING NOW, so the policy never waits for a change to learn the light.
//
// ACROSS A SLEEP: `SUSPEND` stops the polling; `RESUME` reads `_ALI` again, sends it, and polls again. WHEN THE ACPI
// SERVICE RESTARTS its node channels end with it: the node is asked for again and read again.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aml::wire::Value;
use drivers::acpi_video::{self, SensorNotification};
use drivers::common;
use ipc_client::ChannelTransport;
use proto::system::{AmbientLightDescription, Error, Illuminance, LightResponse, acpi_node, ambient_light};
use rt::*;
use wire::Handles;

const TICKS: u64 = TICKS_PER_SECOND * 5;
const TOKEN: u16 = 0;
const NODE_LOOK_TICKS: u64 = TICKS_PER_SECOND;
const STREAM_DEPTH: u64 = 16;

struct Sensor {
	name: String,
	node: u64,
	curve: Vec<LightResponse>,
	poll: Option<u64>,
	next_poll: u64,
	suspended: bool,
	last: Option<u64>,
	stream: u64,
	stream_seq: u32,
}

impl Sensor {
	fn say(&self, text: &str) {
		print(format!("driver.acpi-als: {}: {text}\n", self.name).as_bytes());
	}

	fn evaluate(&self, method: &str) -> Result<Option<Value>, String> {
		match acpi_node::Client::with_deadline(ChannelTransport { chan: self.node }, clock() + TICKS).evaluate(method, &[]) {
			Some(Ok(bytes)) => aml::wire::decode(&bytes).map(Some).map_err(|error| format!("{method} did not decode - {error:?}")),
			Some(Err(Error::NotFound)) => Ok(None),
			Some(Err(error)) => Err(format!("{method} was refused - {error:?}")),
			None => Err(format!("{method} was not answered")),
		}
	}

	fn read_curve(&mut self) {
		self.curve = match self.evaluate("_ALR") {
			Ok(Some(value)) => match acpi_video::alr(&value) {
				Ok(points) => points.iter().map(|point| LightResponse { adjustment: point.adjustment, illuminance: point.illuminance }).collect(),
				Err(refusal) => {
					self.say(&format!("_ALR is not a curve - {refusal:?}; the policy follows its own"));
					Vec::new()
				}
			},
			Ok(None) => Vec::new(),
			Err(why) => {
				self.say(&why);
				Vec::new()
			}
		};
	}

	fn read_poll(&mut self) {
		self.poll = match self.evaluate("_ALP") {
			Ok(Some(value)) => acpi_video::alp_ticks(&value, TICKS_PER_SECOND).unwrap_or(None),
			_ => None,
		};
		self.next_poll = self.poll.map_or(u64::MAX, |every| clock() + every);
	}

	// `_ALI`, read and - when it changed or `always` - sent.
	fn reading(&mut self, always: bool) {
		let reading = match self.evaluate("_ALI") {
			Ok(Some(value)) => acpi_video::ali(&value).unwrap_or(None),
			Ok(None) => None,
			Err(why) => {
				self.say(&why);
				None
			}
		};
		let Some(milli_lux) = reading else { return };
		if always || self.last != Some(milli_lux) {
			self.last = Some(milli_lux);
			self.send(milli_lux);
		}
	}

	fn send(&mut self, milli_lux: u64) {
		if self.stream == 0 {
			return;
		}
		let mut frame = [0u8; 64];
		let mut handles = Handles::new();
		let Some(len) = ambient_light::events_frame(self.stream_seq, &Illuminance { milli_lux }, &mut frame, &mut handles) else { return };
		if try_send(self.stream, &frame[..len], 0) {
			self.stream_seq = self.stream_seq.wrapping_add(1);
			return;
		}
		// A STREAM THAT CANNOT TAKE A READING IS CLOSED: the policy opens it again and starts from the reading then.
		close(self.stream);
		self.stream = 0;
	}
}

impl ambient_light::Service for Sensor {
	fn describe(&mut self) -> Result<AmbientLightDescription, Error> {
		Ok(AmbientLightDescription { curve: self.curve.clone() })
	}

	fn events(&mut self) -> Vec<Illuminance> {
		Vec::new()
	}
}

struct Sleep<'a> {
	sensor: &'a mut Sensor,
	serving: &'a mut common::Serving,
}

impl common::SleepStep for Sleep<'_> {
	fn suspend(&mut self, _request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		self.sensor.suspended = true;
		self.sensor.next_poll = u64::MAX;
		driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
	}

	fn resume(&mut self, _lost_power: bool) -> bool {
		self.sensor.suspended = false;
		self.sensor.reading(true);
		self.sensor.next_poll = self.sensor.poll.map_or(u64::MAX, |every| clock() + every);
		true
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(self.serving)
	}
}

fn serve(sensor: &mut Sensor, channel: u64, buf: &mut [u8]) -> bool {
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	if len >= 2 && u16::from_le_bytes([buf[0], buf[1]]) == ambient_light::OP_EVENTS {
		let Some((corr, _)) = ambient_light::events_open(sensor, &buf[..len], &mut handles) else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		if sensor.stream != 0 {
			close(sensor.stream);
		}
		sensor.stream = producer;
		sensor.stream_seq = 0;
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		// THE READING NOW, first.
		sensor.reading(true);
		return true;
	}
	let mut reply = [0u8; 1024];
	let mut reply_handles = Handles::new();
	let written = ambient_light::dispatch(sensor, &buf[..len], &mut handles, &mut reply, &mut reply_handles);
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	if let Some(written) = written {
		send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
	}
	true
}

fn subscribe(node: u64) -> u64 {
	acpi_node::Client::with_deadline(ChannelTransport { chan: node }, clock() + TICKS).notifications().unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, _resources) = common::handshake(bootstrap);
	let name = String::from_utf8_lossy(bind.info.platform.identity()).into_owned();
	if !bind.info.platform.match_ids().iter().any(|id| id.text() == b"ACPI0008") {
		print(format!("driver.acpi-als: {name}: its ids do not name a light sensor\n").as_bytes());
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::UnsupportedDevice);
	}
	common::takes_sleep();
	let report = format!("driver.acpi-als: {name}: online");
	let Some((producer, consumer)) = channel() else { exit() };
	let mut serving = common::Serving::from_offers(&[(TOKEN, producer)]);
	if !common::online(bootstrap, &bind, report.as_bytes(), &[(driver_protocol::provider::AMBIENT_LIGHT, consumer)]) {
		exit();
	}
	if !common::request_node(bootstrap, &bind) || common::wait_node_or_answer(bootstrap, &bind, &[]).is_none() {
		if common::stop_requested() {
			common::finish_stop(bootstrap, &bind, 0, true);
		}
		exit();
	}
	let node = common::node().unwrap_or(0);
	let mut sensor = Sensor { name, node, curve: Vec::new(), poll: None, next_poll: u64::MAX, suspended: false, last: None, stream: 0, stream_seq: 0 };
	if node == 0 {
		sensor.say("the firmware describes no node for it");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	sensor.read_curve();
	sensor.read_poll();
	sensor.reading(false);
	sensor.say(&format!("{} response point(s), {}, reading {:?} mlx", sensor.curve.len(), sensor.poll.map_or(String::from("notified"), |every| format!("polled every {every} tick(s)")), sensor.last));
	let mut notifications = subscribe(sensor.node);
	let mut buf = alloc::vec![0u8; 1024];
	loop {
		let looking = if sensor.node == 0 { clock() + NODE_LOOK_TICKS } else { sensor.next_poll };
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
		if ready.is_none() && !common::take_sleep_step(bootstrap, &bind, &mut Sleep { sensor: &mut sensor, serving: &mut serving }) {
			if common::stop_requested() {
				common::finish_stop(bootstrap, &bind, 0, true);
			}
			exit();
		}
		match common::fresh_node() {
			Some(0) => sensor.say("the firmware describes no node for it any more"),
			Some(node) => {
				sensor.node = node;
				if notifications != 0 {
					close(notifications);
				}
				notifications = subscribe(node);
				sensor.read_curve();
				sensor.reading(true);
			}
			None => {}
		}
		// THE POLL, where the firmware notifies nothing.
		if !sensor.suspended && sensor.node != 0 && clock() >= sensor.next_poll {
			sensor.reading(false);
			sensor.next_poll = sensor.poll.map_or(u64::MAX, |every| clock() + every);
		}
		let Some(at) = ready else { continue };
		if at >= consumers_at {
			let index = at - consumers_at;
			let channel = serving.at(index);
			if !serve(&mut sensor, channel, &mut buf) {
				let token = serving.close_at(index);
				if sensor.stream != 0 {
					close(sensor.stream);
					sensor.stream = 0;
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
					match acpi_video::sensor_notification(u64::from(notification.value)) {
						SensorNotification::Illuminance => sensor.reading(false),
						SensorNotification::Response => {
							sensor.read_curve();
							sensor.say(&format!("the firmware changed its response curve - {} point(s)", sensor.curve.len()));
						}
						SensorNotification::Other(value) => sensor.say(&format!("a Notify of {value:#04x}, which a light sensor does not raise, is ignored")),
					}
				}
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					close(notifications);
					notifications = 0;
					sensor.node = 0;
					sensor.say("its node's notifications ended - the ACPI service is gone; waiting for its node again");
					break;
				}
			}
		}
	}
}
