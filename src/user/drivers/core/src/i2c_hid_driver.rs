// THE HID-OVER-I2C DRIVER: a touchpad or a touchscreen on another driver's bus, bound as a CHILD. Its row is the
// firmware's `PNP0C50`/`ACPI0C50` node or the tree's `hid-over-i2c` node, and it reaches its device through the two
// connections DeviceManager minted on its controllers' publications and nothing else: the I2C address, on the
// `i2c-bus` controller, and the interrupt line - level-triggered and active low, as the firmware describes it - on the
// `gpio-lines` controller.
//
// BRING-UP, AFTER READY. READY comes first and publishes nothing: the node channel is answered only to a binding that
// is online, and what follows is bounded by the device rather than by the bind window. Then the HID descriptor register,
// from the node's `_DSM` (HID over I2C's, function 1) or the tree's `hid-descr-addr`; D0 asked on the node channel;
// the thirty-byte HID descriptor read and checked, a malformed one failing the binding with nothing published; SET_POWER
// on, RESET, and the reset indication - a zero-length input report - awaited on the line within `RESET_BOUND_TICKS`, the
// RESET sent once more and then the binding failed; then the report descriptor, handed to the generic HID parser, whose
// application collections decide what is published (`drivers::i2c_hid`), each publication offered as it is decided. No
// Input Mode or Device Mode feature report is ever sent.
//
// THE LOOP. On the line's event: input reports are read until the line is quiet, each decoded by the collection its
// report id belongs to - a pointer frame, or contact frames - and sent to that provider's consumers; then the event is
// acknowledged, so the line may fire again. A line event with nothing behind it resets the device once, and the next
// fails the binding. On STOP: SET_POWER sleep, then D3cold asked on the node channel.
//
// THE SLEEP, between two line events: SET_POWER sleep and the state the sleep lets the node enter - it arms no wake, so
// the deepest - and the line unwatched; the resume asks D0, SET_POWER on and RESET with its indication awaited, as bind
// does - a device that lost its power comes back through the same handshake - and a device that does not answer it did
// not come back. THE POWER STATES ARE ASKED, NEVER EVALUATED: `_PSx` and the power resources a state shares with other
// devices are the ACPI service's (`drivers::node_power`).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use drivers::common;
use drivers::hid;
use drivers::i2c_hid::{self, Pointer, ResetHandshake, ResetNext, Route, Storm, StormAnswer};
use drivers::node_power;
use hid_i2c::{Device, Input, PowerState, SlaveAddress};
use i2c_client::ScopedBus;
use i2c_device_proto::generated::liber::i2c_device::v1::i2c_device;
use ipc_client::ChannelTransport;
use proto::system::{GpioTrigger, acpi_node, gpio_device};
use rt::*;
use wire::Handles;

// How long one question to a controller or to the node may take.
const TICKS: u64 = TICKS_PER_SECOND * 5;
// The most reports one line event is read for before the loop looks at its control channel again.
const REPORTS_PER_EVENT: usize = 64;
// The two publications, by the token each is offered under.
const POINTER: u16 = 0;
const TOUCH: u16 = 1;

const DSM_UUID: [u8; 16] = uuid(*b"3cdff6f742674555ad05b30a3d8938de");

// `ToUUID`'s layout: the first three fields little-endian.
const fn uuid(hex: [u8; 32]) -> [u8; 16] {
	const fn nibble(byte: u8) -> u8 {
		if byte >= b'a' { byte - b'a' + 10 } else { byte - b'0' }
	}
	let mut raw = [0u8; 16];
	let mut at = 0;
	while at < 16 {
		raw[at] = nibble(hex[2 * at]) << 4 | nibble(hex[2 * at + 1]);
		at += 1;
	}
	[raw[3], raw[2], raw[1], raw[0], raw[5], raw[4], raw[7], raw[6], raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15]]
}

// What one bounded wait on the line came to.
enum Waited {
	Event,
	Timeout,
	// The manager asked for a stop, or dropped the channel.
	Ended,
	// The line's event stream closed: its controller's binding ended.
	LineLost,
}

struct Hid {
	bootstrap: u64,
	bind: common::Bind,
	name: String,
	bus: ScopedBus<ChannelTransport>,
	device: Device,
	line: u64,
	events: u64,
	// The level at which the line is asserted: low for an active-low line.
	asserted: bool,
	node: u64,
	serving: common::Serving,
	layout: hid::Layout,
	pointer: Pointer,
	input: Vec<u8>,
}

// ONE LINE, WRITTEN WHOLE, so another process's output never lands inside it.
fn say(name: &str, text: &str) {
	let line = format!("driver.i2c-hid: {name}: {text}\n");
	print(line.as_bytes());
}

impl Hid {
	fn say(&self, text: &str) {
		say(&self.name, text);
	}

	fn gpio(&self) -> gpio_device::Client<ChannelTransport> {
		gpio_device::Client::with_deadline(ChannelTransport { chan: self.line }, clock() + TICKS)
	}

	fn acknowledge(&self) {
		let _ = self.gpio().acknowledge();
	}

	// Whether the line is released: the device has nothing more to be read.
	fn quiet(&self) -> bool {
		!matches!(self.gpio().level(), Some(Ok(level)) if level == self.asserted)
	}

	// Every event frame queued on the stream, taken; false when the stream closed.
	fn drain_events(&self) -> bool {
		let mut buf = [0u8; 64];
		loop {
			match try_recv_caps(self.events, &mut buf) {
				PolledCaps::Message { len, handles } => {
					for &leftover in handles.as_slice() {
						close(leftover);
					}
					let mut frame = Handles::new();
					let _ = gpio_device::events_read(&buf[..len], &mut frame);
				}
				PolledCaps::Empty => return true,
				PolledCaps::Closed => return false,
			}
		}
	}

	fn wait_line(&mut self, deadline: u64) -> Waited {
		loop {
			match common::wait_or_answer_until(self.bootstrap, &self.bind, &[self.events], deadline, Some(&mut self.serving)) {
				None => return Waited::Ended,
				// A `SUSPEND` handed back before the deadline is not the deadline: it is taken in the loop, once the
				// handshake in hand is over.
				Some(None) if clock() < deadline => {}
				Some(None) => return Waited::Timeout,
				Some(Some(_)) => return if self.drain_events() { Waited::Event } else { Waited::LineLost },
			}
		}
	}

	fn power_state(&self, state: u8) {
		power_state(&self.name, self.node, state);
	}

	// RESET, and the indication awaited within the bound - sent once more, then refused.
	fn reset(&mut self) -> Result<(), &'static str> {
		let mut handshake = ResetHandshake::new();
		loop {
			if self.device.reset(&mut self.bus).is_err() {
				return Err("RESET could not be sent");
			}
			let deadline = clock() + i2c_hid::RESET_BOUND_TICKS;
			loop {
				match self.wait_line(deadline) {
					Waited::Event => {
						let answered = matches!(self.device.read_input(&mut self.bus, &mut self.input), Ok(Input::ResetComplete));
						self.acknowledge();
						if answered {
							return Ok(());
						}
					}
					Waited::Timeout => break,
					Waited::Ended => return Err("the binding ended during a RESET"),
					Waited::LineLost => return Err("the interrupt line's controller went away during a RESET"),
				}
			}
			match handshake.timed_out() {
				ResetNext::Again => self.say("the reset indication did not come in time - RESET once more"),
				ResetNext::Refuse => return Err("the device answered RESET with no reset indication, twice"),
			}
		}
	}

	// One report, to the consumers of the provider its collection publishes.
	fn deliver(&mut self, report: &[u8]) {
		let Some((id, body)) = i2c_hid::split(&self.layout, report) else { return };
		let (token, frames, count) = match i2c_hid::route(&self.layout, id) {
			Route::Pointer => match self.pointer.feed(&self.layout, id, body) {
				Some(frame) => {
					let mut frames = [[0u8; 6]; hid::MAX_CONTACTS];
					frames[0] = frame;
					(POINTER, frames, 1)
				}
				None => return,
			},
			Route::Touch => {
				let mut frames = [[0u8; 6]; hid::MAX_CONTACTS];
				let count = i2c_hid::contact_frames(&self.layout, id, body, &mut frames);
				(TOUCH, frames, count)
			}
			Route::Nothing => return,
		};
		// NON-BLOCKING: a consumer that stopped reading must not hold the line's loop, and a dropped frame of a gesture is
		// recovered by the next. A consumer that went away is noticed by the loop's wait, which reports it.
		for at in 0..self.serving.as_slice().len() {
			if self.serving.token_at(at) != token {
				continue;
			}
			for frame in &frames[..count] {
				let _ = try_send(self.serving.at(at), frame, 0);
			}
		}
	}

	// ONE LINE EVENT: reports read until the line is quiet, then the event acknowledged. False when the storm rule fails
	// the binding.
	fn event(&mut self, storm: &mut Storm) -> Result<(), &'static str> {
		let mut delivered = 0usize;
		for _ in 0..REPORTS_PER_EVENT {
			let report = match self.device.read_input(&mut self.bus, &mut self.input) {
				Ok(Input::Report(body)) => Some(Vec::from(body)),
				Ok(Input::ResetComplete) => None,
				Err(error) => {
					self.say(&format!("an input report could not be read - {error:?}"));
					None
				}
			};
			match report {
				Some(report) => {
					self.deliver(&report);
					delivered += 1;
				}
				None => break,
			}
			if self.quiet() {
				break;
			}
		}
		if delivered != 0 {
			self.acknowledge();
			return Ok(());
		}
		// THE LINE ASSERTED WITH NOTHING BEHIND IT.
		self.acknowledge();
		match storm.empty_event() {
			StormAnswer::Reset => {
				self.say("its line is asserted with no report behind it - resetting the device, once");
				self.power_on_and_reset()
			}
			StormAnswer::Fail => Err("its line is asserted with no report behind it again, after the reset"),
		}
	}

	fn power_on_and_reset(&mut self) -> Result<(), &'static str> {
		if self.device.set_power(&mut self.bus, PowerState::On).is_err() {
			return Err("SET_POWER on could not be sent");
		}
		self.reset()
	}

	// THE SLEEP'S HALF OF WHAT A STOP DOES, and its way back - see the head of this file.
	fn sleep_now(&mut self, state: driver_protocol::SleepState) -> bool {
		let asleep = self.device.set_power(&mut self.bus, PowerState::Sleep).is_ok();
		if !asleep {
			self.say("SET_POWER sleep could not be sent");
		}
		self.power_state(node_power::for_sleep(self.node, state, false));
		asleep
	}

	// THE STOP: the device put to sleep, then D3cold.
	fn stop(&mut self) -> ! {
		let asleep = self.device.set_power(&mut self.bus, PowerState::Sleep).is_ok();
		if !asleep {
			self.say("SET_POWER sleep could not be sent");
		}
		self.power_state(node_power::D3_COLD);
		common::finish_stop(self.bootstrap, &self.bind, 0, true);
		exit()
	}

	fn fail(&mut self, why: &str, code: driver_protocol::DriverFailureCode) -> ! {
		self.say(why);
		common::failed(self.bootstrap, &self.bind, code)
	}
}

impl common::SleepStep for Hid {
	fn suspend(&mut self, request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		if !self.sleep_now(request.state) {
			return driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Refused(driver_protocol::DriverFailureCode::DeviceNotResponding), awake_by_ms: 0 };
		}
		self.say("suspended - SET_POWER sleep");
		driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
	}

	fn resume(&mut self, _lost_power: bool) -> bool {
		self.power_state(node_power::D0);
		match self.power_on_and_reset() {
			Ok(()) => {
				self.say("resumed - SET_POWER on and RESET answered");
				true
			}
			Err(why) => {
				self.say(&format!("did not come back from the sleep - {why}"));
				false
			}
		}
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(&mut self.serving)
	}
}

// THE NODE'S POWER STATE, asked on its channel: a node with no `_PSx` and no power resources answers with nothing done,
// and one that could not enter the state is said and carried on from - the HID handshake is what says whether the
// device answers.
fn power_state(name: &str, node: u64, state: u8) {
	if let Err(why) = node_power::set(node, state) {
		say(name, &why);
	}
}

// THE NODE'S CHANNEL, for an ACPI device: asked for, and waited for - DeviceManager answers once the ACPI service's
// namespace is loaded. Zero when the firmware describes no node, or the binding ended first.
fn node_channel(bootstrap: u64, bind: &common::Bind) -> Option<u64> {
	if !common::request_node(bootstrap, bind) {
		return None;
	}
	common::wait_node_or_answer(bootstrap, bind, &[])?;
	common::node()
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	let name = String::from_utf8_lossy(bind.info.platform.identity()).into_owned();
	// THE TWO CONNECTIONS, by the kinds of the row's connections they were minted for.
	let (mut i2c, mut line) = (0u64, 0u64);
	for (at, connection) in bind.info.platform.connections().iter().enumerate() {
		let handle = if at < resources.connection_count { resources.connections[at] } else { 0 };
		match connection.kind {
			CONNECTION_I2C if i2c == 0 && handle != 0 => i2c = handle,
			CONNECTION_GPIO_LINE if line == 0 && handle != 0 => line = handle,
			_ if handle != 0 => close(handle),
			_ => {}
		}
	}
	if i2c == 0 || line == 0 {
		say(&name, "it needs an I2C address and an interrupt line, and its firmware named no pair of them");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	let report = format!("driver.i2c-hid: {name}: online");
	if !common::online(bootstrap, &bind, report.as_bytes(), &[]) {
		exit();
	}
	// THE DESCRIPTOR REGISTER: the node's `_DSM`, or the tree's `hid-descr-addr`.
	let acpi = bind.info.platform.source == PLATFORM_SOURCE_ACPI;
	let node = if acpi { node_channel(bootstrap, &bind).unwrap_or(0) } else { 0 };
	if common::stop_requested() {
		common::finish_stop(bootstrap, &bind, 0, true);
		exit();
	}
	let register = if acpi {
		match (node != 0).then(|| acpi_node::Client::with_deadline(ChannelTransport { chan: node }, clock() + TICKS).dsm(&DSM_UUID, &i2c_hid::DESCRIPTOR_DSM_REVISION, &i2c_hid::DESCRIPTOR_DSM_FUNCTION, &[0x04u8, 0, 0, 0, 0])) {
			Some(Some(Ok(answer))) => i2c_hid::dsm_register(&answer),
			_ => None,
		}
	} else {
		let mut block = [0u8; MAX_DEVICE_PROPERTIES];
		let len = device_properties(resources.device, &mut block);
		if len > 0 { i2c_hid::tree_descriptor_register(&block[..(len as usize).min(block.len())]) } else { None }
	};
	let Some(register) = register else {
		say(&name, "its firmware names no HID descriptor register");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	};
	// THE BUS, over the address-scoped connection - refused when the controller serves no plain I2C.
	let mut bus = match ScopedBus::new(i2c_device::Client::new(ChannelTransport { chan: i2c })) {
		Ok(bus) => bus,
		Err(why) => {
			say(&name, &format!("its I2C connection cannot carry HID over I2C - {why:?}"));
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
		}
	};
	let Some(address) = SlaveAddress::new(bus.address()) else {
		say(&name, "its I2C connection names a reserved address");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	};
	// THE LINE: its trigger says which level is asserted, and its events are the report loop's wake.
	let mut gpio = gpio_device::Client::with_deadline(ChannelTransport { chan: line }, clock() + TICKS);
	let asserted = match gpio.line() {
		Some(Ok(scoped)) => !matches!(scoped.trigger, GpioTrigger::Low | GpioTrigger::Falling),
		_ => {
			say(&name, "its interrupt line did not say how it is scoped");
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
		}
	};
	let events = gpio_device::Client::with_deadline(ChannelTransport { chan: line }, clock() + TICKS).events().unwrap_or(0);
	if events == 0 {
		say(&name, "its interrupt line gave no event stream");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	// POWER, THEN THE DESCRIPTOR - which is what a device still held in reset, or one that is not HID over I2C, fails.
	power_state(&name, node, node_power::D0);
	let device = match Device::probe(&mut bus, address, register) {
		Ok(device) => device,
		Err(error) => {
			say(&name, &format!("its HID descriptor at register {register:#x} is refused - {error:?}"));
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::UnsupportedDevice);
		}
	};
	let input = alloc::vec![0u8; device.descriptor().max_input_len as usize];
	let mut hid = Hid { bootstrap, bind, name, bus, device, line, events, asserted, node, serving: common::Serving::from_offers(&[]), layout: hid::Layout::empty(), pointer: Pointer::default(), input };
	hid.say(&format!("its HID descriptor is read (address {:#04x}, descriptor register {register:#x}, vendor {:04x} product {:04x})", address.get(), hid.device.descriptor().vendor, hid.device.descriptor().product));
	if let Err(why) = hid.power_on_and_reset() {
		if common::stop_requested() {
			hid.stop();
		}
		hid.fail(why, driver_protocol::DriverFailureCode::DeviceNotResponding);
	}
	// THE REPORT DESCRIPTOR, AND WHAT IT PUBLISHES.
	let mut descriptor = alloc::vec![0u8; hid.device.descriptor().report_descriptor_len as usize];
	if let Err(error) = hid.device.report_descriptor(&mut hid.bus, &mut descriptor) {
		hid.fail(&format!("its report descriptor could not be read - {error:?}"), driver_protocol::DriverFailureCode::DeviceNotResponding);
	}
	hid.layout = hid::parse(&descriptor);
	let publishes = i2c_hid::publications(&hid.layout);
	for (wanted, kind, token, what) in [(publishes.pointer, driver_protocol::provider::POINTER, POINTER, "pointer"), (publishes.touch, driver_protocol::provider::TOUCH, TOUCH, "touch")] {
		if !wanted {
			continue;
		}
		let Some((producer, consumer)) = channel() else { continue };
		if hid.serving.publish(token, producer) && common::offer(bootstrap, &hid.bind, kind, token, consumer) {
			hid.say(&format!("publishes {what}"));
		} else {
			close(producer);
			close(consumer);
		}
	}
	if !publishes.pointer && !publishes.touch {
		hid.say("its report descriptor has neither a mouse nor a touch screen collection - it publishes nothing");
	}
	// THE LOOP.
	let mut storm = Storm::default();
	common::takes_sleep();
	loop {
		let events = hid.events;
		let Some(ready) = common::wait_providers_until(bootstrap, &hid.bind, &mut hid.serving, &[events], 0) else { hid.stop() };
		let Some(ready) = ready else {
			let bind = hid.bind;
			if !common::take_sleep_step(bootstrap, &bind, &mut hid) {
				hid.stop();
			}
			continue;
		};
		match ready {
			common::ProviderReady::Consumer(at) => {
				let mut unexpected = [0u8; 16];
				match try_recv_caps(hid.serving.at(at), &mut unexpected) {
					PolledCaps::Closed => {
						let token = hid.serving.close_at(at);
						common::disconnected(bootstrap, &hid.bind, token);
					}
					PolledCaps::Message { handles, .. } => {
						for &handle in handles.as_slice() {
							close(handle);
						}
					}
					PolledCaps::Empty => {}
				}
			}
			common::ProviderReady::Connected(_) => {}
			common::ProviderReady::Device(_) => {
				if !hid.drain_events() {
					hid.fail("its interrupt line's controller went away", driver_protocol::DriverFailureCode::ResourceUnusable);
				}
				if let Err(why) = hid.event(&mut storm) {
					if common::stop_requested() {
						hid.stop();
					}
					hid.fail(why, driver_protocol::DriverFailureCode::DeviceNotResponding);
				}
			}
		}
	}
}
