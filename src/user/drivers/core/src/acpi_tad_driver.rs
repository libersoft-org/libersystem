// THE ACPI TIME AND ALARM DEVICE (`ACPI000E`), WHOLE - one binding, on the node-scoped channel the ACPI service serves.
//
// ITS CLOCK: `_GRT`, read at bind and in every `RESUME` step, and handed to the kernel as a wall-clock base under the
// clock source's privilege DeviceManager hands this driver alone. Where the kernel drives no RTC of its own - the FADT
// says the CMOS clock is absent, or the development switch does - `SYS_CLOCK_RTC` then answers that base counted forward
// on the boot-time clock, so TimeService, StorageService's stamps and every other reader keep the one kernel read; and
// after a suspend to RAM the base handed at the resume is what the sleep's length is taken from. Where the kernel reads
// its CMOS clock the base is kept and not used. `_SRT` stays unused: nothing in this system writes a hardware clock.
//
// ITS WAKE TIMERS: `_GCP` says which it has - the AC timer, the DC timer. A `SUSPEND` asked for a timed wake clears each
// timer's status with `_CWS` and programs it with `_STV` (whole seconds, rounded up), and answers wake armed, so the
// platform's step arms the device's `_PRW` event; asked for none, each timer is disabled. The resume reads `_GWS` - whether
// a timer woke the machine - clears it, disables the timers, and hands the clock again. Its pure parts are
// `drivers::acpi_tad`.
//
// ITS POWER STATE, asked on the node channel: D0 once the node is handed, and again when a restarted service hands it
// again; for a sleep, the state that sleep lets it enter - where it still wakes the machine when a timer is armed, the
// deepest when none is - and D0 at the resume, before its timers are read.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aml::wire::Value;
use drivers::acpi_tad::{self, Capabilities, DISABLED, TIMER_AC, TIMER_DC};
use drivers::common;
use drivers::node_power;
use ipc_client::ChannelTransport;
use proto::system::{Error, acpi_node};
use rt::*;

// How long one question to the node may take.
const TICKS: u64 = TICKS_PER_SECOND * 5;

struct Tad {
	name: String,
	node: u64,
	capabilities: Capabilities,
	clock_source: u64,
	// The timers this sleep armed, for the resume to read and disarm.
	armed: Vec<u64>,
}

impl Tad {
	fn say(&self, text: &str) {
		let line = format!("driver.acpi-tad: {}: {text}\n", self.name);
		print(line.as_bytes());
	}

	// THE NODE'S POWER STATE: one that could not be entered is said, and the device is driven as it is.
	fn power_state(&self, state: u8) {
		if let Err(why) = node_power::set(self.node, state) {
			self.say(&why);
		}
	}

	// A method of the node's with integer arguments: its value, `None` when the node does not have it.
	fn evaluate(&self, method: &str, arguments: &[u64]) -> Result<Option<Value>, String> {
		// THE ARGUMENTS AS ONE PACKAGE OF INTEGERS, the node channel's encoding; none for a method that takes none.
		let arguments = if arguments.is_empty() { Vec::new() } else { aml::wire::encode(&Value::Package(arguments.iter().map(|value| Value::Integer(*value)).collect())).map_err(|error| format!("{method}'s arguments did not encode - {error:?}"))? };
		match acpi_node::Client::with_deadline(ChannelTransport { chan: self.node }, clock() + TICKS).evaluate(method, &arguments) {
			Some(Ok(bytes)) => aml::wire::decode(&bytes).map(Some).map_err(|error| format!("{method} did not decode - {error:?}")),
			Some(Err(Error::NotFound)) => Ok(None),
			Some(Err(error)) => Err(format!("{method} was refused - {error:?}")),
			None => Err(format!("the ACPI service did not answer {method}")),
		}
	}

	fn integer(&self, method: &str, arguments: &[u64]) -> Result<u64, String> {
		match self.evaluate(method, arguments)? {
			Some(Value::Integer(value)) => Ok(value),
			Some(other) => Err(format!("{method} answered {other:?}, not an integer")),
			None => Err(format!("the node has no {method}")),
		}
	}

	// THE CLOCK: `_GRT` read and handed to the kernel.
	fn hand_clock(&self) {
		if !self.capabilities.real_time {
			return;
		}
		let unix = match self.evaluate("_GRT", &[]) {
			Ok(Some(Value::Buffer(buffer))) => match acpi_tad::unix_of_grt(&buffer) {
				Ok(unix) => unix,
				Err(why) => {
					self.say(why);
					return;
				}
			},
			Ok(other) => {
				self.say(&format!("_GRT answered {other:?}, not a buffer"));
				return;
			}
			Err(why) => {
				self.say(&why);
				return;
			}
		};
		if self.clock_source == 0 {
			self.say(&format!("its clock reads {unix} - no clock source was handed to this binding, so the kernel is not told"));
			return;
		}
		match clock_base(self.clock_source, unix) {
			0 => self.say(&format!("its clock reads {unix} and is handed to the kernel")),
			error => self.say(&format!("the kernel refused its clock ({error})")),
		}
	}

	// The timers `_GCP` says it has.
	fn timers(&self) -> Vec<u64> {
		let mut timers = Vec::new();
		if self.capabilities.ac_wake {
			timers.push(TIMER_AC);
		}
		if self.capabilities.dc_wake {
			timers.push(TIMER_DC);
		}
		timers
	}
}

// THE SLEEP: the timed wake programmed into each timer, or each timer disabled - and at the resume, what woke it read,
// cleared and the timers disabled, and the clock handed again.
struct Sleep<'a> {
	tad: &'a mut Tad,
}

impl common::SleepStep for Sleep<'_> {
	fn suspend(&mut self, request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		let done = |outcome| driver_protocol::Suspended { outcome, awake_by_ms: 0 };
		let seconds = match acpi_tad::timer_seconds(request.timed_wake_ms) {
			Ok(seconds) => seconds,
			Err(why) => {
				self.tad.say(why);
				return done(driver_protocol::SuspendOutcome::Refused(driver_protocol::DriverFailureCode::ResourceUnusable));
			}
		};
		self.tad.armed.clear();
		for timer in self.tad.timers() {
			let value = seconds.unwrap_or(DISABLED);
			if seconds.is_some() {
				let _ = self.tad.evaluate("_CWS", &[timer]);
			}
			match self.tad.integer("_STV", &[timer, value]) {
				Ok(0) => {
					if seconds.is_some() {
						self.tad.armed.push(timer);
					}
				}
				Ok(code) => {
					self.tad.say(&format!("_STV({timer}) answered {code} - the timer is not set"));
					return done(driver_protocol::SuspendOutcome::Refused(driver_protocol::DriverFailureCode::DeviceNotResponding));
				}
				Err(why) => {
					self.tad.say(&why);
					return done(driver_protocol::SuspendOutcome::Refused(driver_protocol::DriverFailureCode::DeviceNotResponding));
				}
			}
		}
		let wakes = request.arm_wake && seconds.is_some() && !self.tad.armed.is_empty();
		self.tad.power_state(node_power::for_sleep(self.tad.node, request.state, wakes));
		match seconds {
			Some(seconds) if !self.tad.armed.is_empty() => {
				self.tad.say(&format!("its timer is set to wake the machine in {seconds} s"));
				done(if wakes { driver_protocol::SuspendOutcome::DoneWakeArmed } else { driver_protocol::SuspendOutcome::Done })
			}
			_ => done(driver_protocol::SuspendOutcome::Done),
		}
	}

	fn resume(&mut self, _lost_power: bool) -> bool {
		self.tad.power_state(node_power::D0);
		for timer in core::mem::take(&mut self.tad.armed) {
			if let Ok(status) = self.tad.integer("_GWS", &[timer])
				&& status & 2 != 0
			{
				self.tad.say(&format!("its timer {timer} woke the machine"));
			}
			let _ = self.tad.evaluate("_CWS", &[timer]);
			let _ = self.tad.evaluate("_STV", &[timer, DISABLED]);
		}
		self.tad.hand_clock();
		true
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	let name = String::from_utf8_lossy(bind.info.platform.identity()).into_owned();
	common::takes_sleep();
	let report = format!("driver.acpi-tad: {name}: online");
	if !common::online(bootstrap, &bind, report.as_bytes(), &[]) {
		exit();
	}
	if !common::request_node(bootstrap, &bind) || common::wait_node_or_answer(bootstrap, &bind, &[]).is_none() {
		if common::stop_requested() {
			common::finish_stop(bootstrap, &bind, 0, true);
		}
		exit();
	}
	let node = common::node().unwrap_or(0);
	let mut tad = Tad { name, node, capabilities: Capabilities::default(), clock_source: resources.clock_source, armed: Vec::new() };
	if node == 0 {
		tad.say("the firmware describes no node for it");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	tad.power_state(node_power::D0);
	match tad.integer("_GCP", &[]) {
		Ok(gcp) => tad.capabilities = acpi_tad::capabilities(gcp),
		Err(why) => {
			tad.say(&format!("its capabilities could not be read - {why}"));
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::DeviceNotResponding);
		}
	}
	tad.say(&format!("has {}{}{}", if tad.capabilities.real_time { "a clock" } else { "no clock" }, if tad.capabilities.ac_wake { ", an AC wake timer" } else { "" }, if tad.capabilities.dc_wake { ", a DC wake timer" } else { "" }));
	tad.hand_clock();
	loop {
		let Some(ready) = common::wait_or_answer_until(bootstrap, &bind, &[], u64::MAX, None) else {
			if common::stop_requested() {
				common::finish_stop(bootstrap, &bind, 0, true);
			}
			exit();
		};
		if ready.is_none() && !common::take_sleep_step(bootstrap, &bind, &mut Sleep { tad: &mut tad }) {
			if common::stop_requested() {
				common::finish_stop(bootstrap, &bind, 0, true);
			}
			exit();
		}
		if let Some(node) = common::fresh_node()
			&& node != 0
		{
			tad.node = node;
			tad.power_state(node_power::D0);
			tad.say("its node is handed again - reading its clock");
			tad.hand_clock();
		}
	}
}
