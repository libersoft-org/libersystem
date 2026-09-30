// driver.tcpci - A TYPE-C PORT CONTROLLER (TCPCI, revision 2.0) on another driver's bus, and the Power Delivery sink the
// system runs over it. Bound as a CHILD, as HID over I2C is: its row is the tree's `tcpci` node, or a namespace device
// matched by `PRP0001` with that `compatible` in `_DSD`, and it reaches the controller through the two connections
// DeviceManager minted on its controllers' publications and nothing else - the I2C address on the `i2c-bus`
// controller, which must serve plain I2C, and the alert line, level and active low, on the `gpio-lines` controller.
//
// THE ENGINE RUNS HERE (`usb_pd::engine`): the Type-C sink and the sink policy engine as one transition function of the
// controller's events and the clock. The path from the alert to the answer crosses no process but the two controllers'.
// Each alert is read whole - its status registers and a received message - cleared, and only then fed to the engine,
// whose actions are the controller's commands, messages, alarm thresholds and this process's timers.
//
// BRING-UP. The board's description (`drivers::typec_tcpci`) first: a connector described as a source or dual-role is
// refused before READY; one described as a sink runs Power Delivery with its sink PDOs; one not described sinks at the
// Type-C current alone. Then READY, then the controller: its identity read, its initialisation awaited for at most
// one second, a controller that cannot switch the sink path refused, and the registers set. A BIND THAT FINDS VBUS
// PRESENT trusts no contract it did not see made: the sink path is left as the registers show it, and Soft_Reset has
// the source advertise again, so the contract is negotiated anew without VBUS dropping.
//
// WHAT IT PUBLISHES: `typec-connector` (the one connector, to TypeCService) and `power-source` (that connector as a
// `usb-c` source, local id 0, to PowerService). No request is taken: the engine does no swap and enters no mode.
//
// ON STOP, the port is left as it is - the sink path among it - since a stop for a lost controller cannot reach it
// anyway, and the next bind reads the port afresh as above.
//
// THE SLEEP IS REFUSED WHILE THE SINK PATH IS ENABLED, naming the connector: a sink path nobody watches is one no alarm
// turns off. With no partner - or one the sink path is not on for - the port suspends with the sink path off and the
// alert unwatched, and its resume reads the port afresh, as a bind does: a charger attached in the sleep finds the sink
// path off, and the contract is made after the resume.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use drivers::common;
use drivers::typec_tcpci::{self, CONNECTOR, Described};
use hid_i2c::{BusError, I2cBus, SlaveAddress};
use i2c_client::ScopedBus;
use i2c_device_proto::generated::liber::i2c_device::v1::i2c_device;
use ipc_client::ChannelTransport;
use proto::generated::liber::typec::v1 as typec;
use proto::system::{ControlOutcome, Error, GpioTrigger, ProviderSource, ProviderUpdate, ProviderUpdateKind, SourceState, gpio_device, power_provider};
use rt::*;
use typec::{Answer, Outcome, Refusal, RefusedRequest, Request, typec_provider};
use usb_pd::engine::{Action, Engine, Event, Report, Transmitted};
use usb_pd::message::{self, Revision};
use usb_pd::pdo::{Selection, Sink, SinkPdo};
use usb_pd::tcpci;
use usb_pd::timer::{HARD_RESET_COUNT, Timer};
use wire::Handles;

const TOKEN_TYPEC: u16 = 0;
const TOKEN_POWER: u16 = 1;
// How long one question to the line's controller may take.
const TICKS: u64 = TICKS_PER_SECOND * 5;
const STREAM_DEPTH: u64 = 64;
// The controller's initialisation, awaited at most this long.
const INIT_TICKS: u64 = TICKS_PER_SECOND;
// Alerts read for one line event before the loop looks at its consumers again.
const ALERT_ROUNDS: usize = 16;
// The receive buffer's registers, whole: the byte count, the frame type, the header and seven objects.
const RECEIVE_BYTES: usize = 32;
// How long a failed transfer waits for the stop a lost controller brings before it is the controller's failure.
const LOST_TICKS: u64 = TICKS_PER_SECOND / 2;

const TIMERS: [Timer; 9] = [
	Timer::CcDebounce,
	Timer::PdDebounce,
	Timer::SinkWaitCap,
	Timer::SenderResponse,
	Timer::PsTransition,
	Timer::SinkRequest,
	Timer::Safe0V,
	Timer::SrcRecover,
	Timer::ErrorRecovery,
];

fn timer_index(timer: Timer) -> usize {
	TIMERS.iter().position(|each| *each == timer).unwrap_or(0)
}

struct Port {
	name: String,
	bootstrap: u64,
	bind: common::Bind,
	bus: ScopedBus<ChannelTransport>,
	address: SlaveAddress,
	line: u64,
	events: u64,
	engine: Engine,
	runs_pd: bool,
	measures: bool,
	// Each timer's deadline in ticks; zero when not armed.
	deadlines: [u64; TIMERS.len()],
	cc_status: u8,
	// MESSAGE_HEADER_INFO as last written.
	header: u8,
	power_control: u8,
	last_refusal: Option<RefusedRequest>,
	// What was last said and published, so each change is said once.
	said: Report,
	said_contract: Option<Selection>,
	said_transition: Option<Selection>,
	dirty: bool,
	serving: common::Serving,
	revision: u64,
	typec_stream: u64,
	typec_seq: u32,
	power_revision: u64,
	power_stream: u64,
	power_seq: u32,
	published: Option<SourceState>,
	buf: Vec<u8>,
	// What the board describes, kept for the engine a resume starts again.
	sink: Sink,
}

fn say(name: &str, text: &str) {
	let line = format!("driver.tcpci: {name}: {text}\n");
	print(line.as_bytes());
}

fn describe_sink(sink: &Sink) -> String {
	let mut text = String::new();
	for (at, pdo) in sink.pdos.iter().enumerate() {
		if at > 0 {
			text.push_str(", ");
		}
		text.push_str(&match *pdo {
			SinkPdo::Fixed { millivolts, milliamps } => format!("fixed {millivolts} mV {milliamps} mA"),
			SinkPdo::Variable { min_millivolts, max_millivolts, milliamps } => format!("variable {min_millivolts}-{max_millivolts} mV {milliamps} mA"),
			SinkPdo::Battery { min_millivolts, max_millivolts, milliwatts } => format!("battery {min_millivolts}-{max_millivolts} mV {milliwatts} mW"),
		});
	}
	format!("{text}; operating at {} uW", sink.operational_microwatts)
}

impl Port {
	fn say(&self, text: &str) {
		say(&self.name, text);
	}

	// ------------------------------------------------------------------ the controller's registers

	// A TRANSFER THAT FAILED: a lost controller brings a stop, which is waited for briefly and taken; otherwise the
	// controller stopped answering, and the binding fails.
	fn lost(&mut self, what: &str, error: BusError) -> ! {
		let deadline = clock() + LOST_TICKS;
		loop {
			match common::wait_or_answer_until(self.bootstrap, &self.bind, &[], deadline, Some(&mut self.serving)) {
				None => self.stop(),
				// A `SUSPEND` handed back is not the stop being waited for, nor the end of the wait.
				Some(None) if clock() < deadline => {}
				_ => break,
			}
		}
		if common::stop_requested() {
			self.stop();
		}
		self.say(&format!("the controller did not answer ({what}: {error:?})"));
		common::failed(self.bootstrap, &self.bind, driver_protocol::DriverFailureCode::DeviceNotResponding)
	}

	fn read(&mut self, register: u8, into: &mut [u8]) {
		let address = self.address;
		if let Err(error) = self.bus.write_read(address, &[register], into) {
			self.lost(&format!("read {register:#04x}"), error);
		}
	}

	fn read8(&mut self, register: u8) -> u8 {
		let mut byte = [0u8; 1];
		self.read(register, &mut byte);
		byte[0]
	}

	fn read16(&mut self, register: u8) -> u16 {
		let mut word = [0u8; 2];
		self.read(register, &mut word);
		u16::from_le_bytes(word)
	}

	fn write(&mut self, bytes: &[u8]) {
		let address = self.address;
		if let Err(error) = self.bus.write(address, bytes) {
			self.lost(&format!("write {:#04x}", bytes[0]), error);
		}
	}

	fn write8(&mut self, register: u8, value: u8) {
		self.write(&[register, value]);
	}

	fn write16(&mut self, register: u8, value: u16) {
		let [low, high] = value.to_le_bytes();
		self.write(&[register, low, high]);
	}

	fn vbus_millivolts(&mut self) -> Option<u32> {
		self.measures.then(|| self.read16(tcpci::VBUS_VOLTAGE)).map(tcpci::vbus_millivolts)
	}

	// ------------------------------------------------------------------ the engine's actions

	fn apply(&mut self, actions: Vec<Action>) {
		for action in actions {
			match action {
				Action::SinkPath(on) => {
					self.write8(tcpci::COMMAND, if on { tcpci::COMMAND_SINK_VBUS } else { tcpci::COMMAND_DISABLE_SINK_VBUS });
				}
				Action::Transmit(bytes) => {
					// THE REVISION THE CONTROLLER'S GoodCRC SPEAKS follows the message's own.
					let revision = if bytes.first().is_some_and(|low| (low >> 6) & 0x3 == Revision::R2 as u8) { 1 } else { 2 };
					let header = tcpci::header_info(revision);
					if header != self.header {
						self.header = header;
						self.write8(tcpci::MESSAGE_HEADER_INFO, header);
					}
					let mut buffer = alloc::vec![tcpci::TRANSMIT_BUFFER];
					buffer.extend_from_slice(&tcpci::transmit_buffer(&bytes));
					self.write(&buffer);
					self.write8(tcpci::TRANSMIT, tcpci::TRANSMIT_SOP);
				}
				// SENT, THEN SAID: a console line is milliseconds the source would otherwise wait on.
				Action::HardReset => {
					self.write8(tcpci::TRANSMIT, tcpci::TRANSMIT_HARD_RESET);
					let sent = self.engine.report().hard_resets;
					self.say(&format!("hard reset sent ({sent} of {HARD_RESET_COUNT})"));
				}
				Action::Alarms(window) if self.measures => {
					match window {
						Some((low, high)) => {
							self.write16(tcpci::VBUS_VOLTAGE_ALARM_HI, tcpci::alarm_threshold(high));
							self.write16(tcpci::VBUS_VOLTAGE_ALARM_LO, tcpci::alarm_threshold(low));
							self.power_control &= !tcpci::POWER_DISABLE_ALARMS;
						}
						None => self.power_control |= tcpci::POWER_DISABLE_ALARMS,
					}
					let control = self.power_control;
					self.write8(tcpci::POWER_CONTROL, control);
				}
				Action::Alarms(_) => {}
				Action::AutoDischarge(on) => {
					if on {
						self.power_control |= tcpci::POWER_AUTO_DISCHARGE;
					} else {
						self.power_control &= !tcpci::POWER_AUTO_DISCHARGE;
					}
					let control = self.power_control;
					self.write8(tcpci::POWER_CONTROL, control);
				}
				Action::Arm(timer) => self.deadlines[timer_index(timer)] = clock() + timer.ticks(),
				Action::Cancel(timer) => self.deadlines[timer_index(timer)] = 0,
				Action::CcOpen(open) => self.write8(tcpci::ROLE_CONTROL, if open { tcpci::ROLE_OPEN } else { tcpci::ROLE_SINK }),
				Action::Receive(on) => {
					if on {
						let flipped = tcpci::flipped(self.cc_status) == Some(true);
						self.write8(tcpci::TCPC_CONTROL, if flipped { tcpci::CONTROL_FLIPPED } else { 0 });
						self.write8(tcpci::RECEIVE_DETECT, tcpci::DETECT_SOP_AND_HARD_RESET);
					} else {
						self.write8(tcpci::RECEIVE_DETECT, 0);
					}
				}
				Action::Publish => self.dirty = true,
			}
		}
	}

	fn event(&mut self, event: Event) {
		let actions = self.engine.event(event);
		self.apply(actions);
		self.tell();
	}

	// WHAT CHANGED, SAID ONCE AND PUBLISHED.
	fn tell(&mut self) {
		let now = self.engine.report();
		let was = core::mem::replace(&mut self.said, now.clone());
		let was = &was;
		if now.attached != was.attached {
			if now.attached {
				let current = match now.typec_current {
					Some(usb_pd::engine::Rp::Default) => "default",
					Some(usb_pd::engine::Rp::Medium) => "1.5 A",
					Some(usb_pd::engine::Rp::High) => "3 A",
					None => "none",
				};
				self.say(&format!("attached - a source advertising {current} on CC{}", if tcpci::flipped(self.cc_status) == Some(true) { 2 } else { 1 }));
			} else {
				self.say("detached");
			}
		}
		// A CONTRACT IS SAID WHEN IT DIFFERS FROM THE ONE LAST SAID, so a source that renegotiates the same contract
		// again and again - the timing run's two hundred - costs the console nothing on the path being timed.
		match now.contract {
			Some((selection, _)) if now.in_transition => {
				if self.said_contract != Some(selection) && self.said_transition != Some(selection) {
					self.said_transition = Some(selection);
					self.say(&format!("offer {} accepted - {} mV in transition", selection.position, selection.millivolts));
				}
			}
			Some((selection, _)) => {
				self.said_transition = None;
				if self.said_contract != Some(selection) {
					self.said_contract = Some(selection);
					self.say(&format!("contract: offer {} at {} mV, {} mA{}", selection.position, selection.millivolts, selection.milliamps, if selection.mismatch { ", capability mismatch" } else { "" }));
				}
			}
			None => {
				self.said_transition = None;
				if self.said_contract.take().is_some() {
					self.say("no contract");
				}
			}
		}
		if was.resetting && !now.resetting && now.attached {
			self.say(&format!("VBUS is back after the hard reset - the partner stayed attached, hard resets so far: {}", now.hard_resets));
		}
		if now.fault && !was.fault {
			self.say("VBUS left the alarm window - the sink path is off, and the source is reset");
		}
		if now.attached && was.attached && !was.pd && now.pd {
			self.say(&format!("the source speaks Power Delivery {}", if now.revision == Revision::R2 { "2.0" } else { "3.x" }));
		}
		if self.dirty {
			self.dirty = false;
			self.publish();
		}
	}

	// ------------------------------------------------------------------ the alert

	// EVERY ALERT PENDING: its registers read and a received message taken, the bits cleared, and only then the engine
	// told - the connection first, then transmissions, then what was received, then the alarms.
	fn service(&mut self) {
		for _ in 0..ALERT_ROUNDS {
			let alert = self.read16(tcpci::ALERT) & tcpci::ALERT_ALL;
			if alert == 0 {
				return;
			}
			let mut events = Vec::new();
			if alert & tcpci::ALERT_CC_STATUS != 0 {
				self.cc_status = self.read8(tcpci::CC_STATUS);
				events.push(Event::Cc(tcpci::cc(self.cc_status)));
			}
			if alert & tcpci::ALERT_POWER_STATUS != 0 {
				let status = self.read8(tcpci::POWER_STATUS);
				events.push(Event::Vbus(status & tcpci::POWER_VBUS_PRESENT != 0));
			}
			let (success, failed, discarded) = (alert & tcpci::ALERT_TX_SUCCESS != 0, alert & tcpci::ALERT_TX_FAILED != 0, alert & tcpci::ALERT_TX_DISCARDED != 0);
			// BOTH AT ONCE REPORT A HARD RESET SENT, which the engine awaits nothing for.
			if !(success && failed) {
				if success {
					events.push(Event::Transmitted(Transmitted::Success));
				} else if discarded {
					events.push(Event::Transmitted(Transmitted::Discarded));
				} else if failed {
					events.push(Event::Transmitted(Transmitted::Failed));
				}
			}
			if alert & tcpci::ALERT_RX_HARD_RESET != 0 {
				events.push(Event::HardResetReceived);
			}
			if alert & tcpci::ALERT_RX_STATUS != 0 {
				let mut buffer = [0u8; RECEIVE_BYTES];
				self.read(tcpci::RECEIVE_BUFFER, &mut buffer);
				match tcpci::received(&buffer).map(message::decode) {
					Some(Ok(message)) => events.push(Event::Received(message)),
					Some(Err(refusal)) => {
						self.say(&format!("a message failed its check ({refusal:?}) - not believed"));
						events.push(Event::Malformed);
					}
					None => {
						self.say("the receive buffer held no message on SOP - not believed");
						events.push(Event::Malformed);
					}
				}
			}
			if alert & tcpci::ALERT_FAULT != 0 {
				let fault = self.read8(tcpci::FAULT_STATUS);
				self.say(&format!("the controller reports a fault ({fault:#04x})"));
				self.write8(tcpci::FAULT_STATUS, fault);
			}
			let alarm = alert & (tcpci::ALERT_VBUS_ALARM_HI | tcpci::ALERT_VBUS_ALARM_LO);
			if alarm != 0 {
				events.push(Event::Alarm);
			}
			self.write16(tcpci::ALERT, alert);
			for event in events {
				// AN ALARM IS ANSWERED FIRST - the sink path off - and said after, where it meant something: to an attached
				// port, after this alert's connection changes.
				if event == Event::Alarm {
					let report = self.engine.report();
					let open = tcpci::cc(self.cc_status) == usb_pd::engine::Cc::Open;
					self.event(event);
					if report.attached && !report.resetting {
						let millivolts = self.vbus_millivolts().unwrap_or(0);
						let side = if alarm & tcpci::ALERT_VBUS_ALARM_HI != 0 { "high" } else { "low" };
						self.say(&format!("a VBUS alarm ({side}) at {millivolts} mV{}", if open { " with CC open - the partner is going" } else { "" }));
					}
					continue;
				}
				let received = event == Event::HardResetReceived;
				self.event(event);
				if received {
					self.say("hard reset received");
				}
			}
		}
	}

	fn acknowledge(&self) {
		let _ = gpio_device::Client::with_deadline(ChannelTransport { chan: self.line }, clock() + TICKS).acknowledge();
	}

	// Every event frame queued on the line's stream, taken; false once the stream closed.
	fn drain_events(&self) -> bool {
		let mut frame = [0u8; 64];
		loop {
			match try_recv_caps(self.events, &mut frame) {
				PolledCaps::Message { len, handles } => {
					for &leftover in handles.as_slice() {
						close(leftover);
					}
					let mut none = Handles::new();
					let _ = gpio_device::events_read(&frame[..len], &mut none);
				}
				PolledCaps::Empty => return true,
				PolledCaps::Closed => return false,
			}
		}
	}

	// ------------------------------------------------------------------ the timers

	fn next_deadline(&self) -> u64 {
		self.deadlines.iter().copied().filter(|deadline| *deadline != 0).min().unwrap_or(0)
	}

	// EVERY TIMER DUE, earliest first.
	fn expire(&mut self) {
		loop {
			let now = clock();
			let due = (0..TIMERS.len()).filter(|at| self.deadlines[*at] != 0 && self.deadlines[*at] <= now).min_by_key(|at| self.deadlines[*at]);
			let Some(at) = due else { return };
			self.deadlines[at] = 0;
			self.event(Event::Timer(TIMERS[at]));
		}
	}

	// ------------------------------------------------------------------ publishing

	fn record(&self) -> typec::Connector {
		let report = self.engine.report();
		typec_tcpci::record(&report, tcpci::flipped(self.cc_status), self.runs_pd, self.last_refusal.clone())
	}

	fn source(&mut self) -> SourceState {
		let report = self.engine.report();
		let millivolts = if report.attached { self.vbus_millivolts() } else { None };
		power_model::usbc::usb_c(&typec_tcpci::supply(&report, millivolts, self.measures))
	}

	fn emit_typec(&mut self, update: &typec::ProviderUpdate) {
		if self.typec_stream == 0 {
			return;
		}
		let mut frame = [0u8; 2048];
		let mut handles = Handles::new();
		if let Some(len) = typec_provider::updates_frame(self.typec_seq, update, &mut frame, &mut handles) {
			match try_send_outcome(self.typec_stream, &frame[..len], 0) {
				SendOutcome::Delivered => self.typec_seq = self.typec_seq.wrapping_add(1),
				SendOutcome::Stalled => self.say("a connector update was dropped: the stream is full"),
				SendOutcome::Failed => {
					close(self.typec_stream);
					self.typec_stream = 0;
				}
			}
		}
	}

	fn emit_power(&mut self, update: &ProviderUpdate) {
		if self.power_stream == 0 {
			return;
		}
		let mut frame = [0u8; 2048];
		let mut handles = Handles::new();
		if let Some(len) = power_provider::updates_frame(self.power_seq, update, &mut frame, &mut handles) {
			match try_send_outcome(self.power_stream, &frame[..len], 0) {
				SendOutcome::Delivered => self.power_seq = self.power_seq.wrapping_add(1),
				SendOutcome::Stalled => self.say("a supply update was dropped: the stream is full"),
				SendOutcome::Failed => {
					close(self.power_stream);
					self.power_stream = 0;
				}
			}
		}
	}

	fn publish(&mut self) {
		self.revision += 1;
		let update = typec::ProviderUpdate { revision: self.revision, kind: typec::UpdateKind::Updated, connector: Some(self.record()), gone: None };
		self.emit_typec(&update);
		let state = self.source();
		if self.published.as_ref() != Some(&state) {
			self.published = Some(state.clone());
			self.power_revision += 1;
			let update = ProviderUpdate { revision: self.power_revision, kind: ProviderUpdateKind::Updated, source: Some(ProviderSource { local: u32::from(CONNECTOR) - 1, state }), gone: None };
			self.emit_power(&update);
		}
	}

	// A REQUEST: the engine takes no swap and enters no mode.
	fn request(&mut self, request: &Request) -> Answer {
		let reason = if request.connector == CONNECTOR { Refusal::TransportDoesNot } else { Refusal::ConnectorCannot };
		if request.connector == CONNECTOR {
			self.last_refusal = Some(RefusedRequest { kind: request.kind, reason, error: 0 });
			self.publish();
		}
		Answer { outcome: Outcome::Refused, reason: Some(reason), error: 0 }
	}

	fn stop(&mut self) -> ! {
		self.say("stopped - the port is left as it is, and the next bind reads it afresh");
		common::finish_stop(self.bootstrap, &self.bind, 0, true);
		exit()
	}

	// THE REGISTERS, alerts masked while they are set - at the bind, and again at a resume.
	fn configure(&mut self) {
		self.write16(tcpci::ALERT_MASK, 0);
		self.write16(tcpci::ALERT, 0xFFFF);
		self.write8(tcpci::FAULT_STATUS, 0xFF);
		if self.read8(tcpci::ROLE_CONTROL) != tcpci::ROLE_SINK {
			self.write8(tcpci::ROLE_CONTROL, tcpci::ROLE_SINK);
		}
		let control = self.read8(tcpci::POWER_CONTROL);
		self.power_control = if self.measures { (control & !tcpci::POWER_DISABLE_MONITORING) | tcpci::POWER_DISABLE_ALARMS } else { control | tcpci::POWER_DISABLE_ALARMS };
		let control = self.power_control;
		self.write8(tcpci::POWER_CONTROL, control);
		self.write8(tcpci::COMMAND, tcpci::COMMAND_ENABLE_VBUS_DETECT);
		self.write8(tcpci::POWER_STATUS_MASK, tcpci::POWER_VBUS_PRESENT);
		self.header = tcpci::header_info(2);
		let header = self.header;
		self.write8(tcpci::MESSAGE_HEADER_INFO, header);
		self.write8(tcpci::RECEIVE_DETECT, 0);
		self.write16(tcpci::ALERT_MASK, tcpci::ALERT_ALL);
	}

	// THE PORT AS FOUND, told to an engine that starts from nothing: a bind's, and a resume's. VBUS present trusts no
	// contract this engine did not see made.
	fn bind_afresh(&mut self) {
		self.engine = Engine::new(self.sink.clone());
		self.deadlines = [0; TIMERS.len()];
		self.cc_status = self.read8(tcpci::CC_STATUS);
		let status = self.read8(tcpci::POWER_STATUS);
		let (cc, vbus, sinking) = (tcpci::cc(self.cc_status), status & tcpci::POWER_VBUS_PRESENT != 0, status & tcpci::POWER_SINKING_VBUS != 0);
		if vbus && matches!(cc, usb_pd::engine::Cc::Rp(_)) {
			self.say(&format!("bound with VBUS present and the sink path {} - no contract is trusted{}", if sinking { "on" } else { "off" }, if self.runs_pd { "; it is negotiated anew through Soft_Reset" } else { "" }));
		}
		self.event(Event::Bound { cc, vbus, sinking });
	}
}

// THE SLEEP - see the head of this file.
impl common::SleepStep for Port {
	fn suspend(&mut self, _request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		if self.read8(tcpci::POWER_STATUS) & tcpci::POWER_SINKING_VBUS != 0 {
			self.say(&format!("the sleep is refused: connector {CONNECTOR} has its sink path enabled, and a sink path left unwatched is one no alarm can turn off"));
			return driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Refused(driver_protocol::DriverFailureCode::Busy), awake_by_ms: 0 };
		}
		self.write8(tcpci::COMMAND, tcpci::COMMAND_DISABLE_SINK_VBUS);
		self.write8(tcpci::RECEIVE_DETECT, 0);
		self.write16(tcpci::ALERT_MASK, 0);
		self.deadlines = [0; TIMERS.len()];
		self.say(&format!("suspended - connector {CONNECTOR}'s sink path is off and its alert unwatched until the resume"));
		driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
	}

	fn resume(&mut self, _lost_power: bool) -> bool {
		self.say("resumed - the port is read afresh, as a bind reads it");
		self.configure();
		self.bind_afresh();
		true
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(&mut self.serving)
	}
}

// ------------------------------------------------------------------ the two publications' views

struct TypecView<'a> {
	port: &'a mut Port,
}

impl typec_provider::Service for TypecView<'_> {
	fn updates(&mut self) -> Vec<typec::ProviderUpdate> {
		let revision = self.port.revision;
		alloc::vec![
			typec::ProviderUpdate { revision, kind: typec::UpdateKind::Snapshot, connector: Some(self.port.record()), gone: None },
			typec::ProviderUpdate { revision, kind: typec::UpdateKind::SnapshotEnd, connector: None, gone: None }
		]
	}
	fn request(&mut self, request: typec::Request) -> Result<Answer, Error> {
		Ok(self.port.request(&request))
	}
}

struct PowerView<'a> {
	port: &'a mut Port,
}

impl power_provider::Service for PowerView<'_> {
	fn updates(&mut self) -> Vec<ProviderUpdate> {
		let revision = self.port.power_revision;
		let state = self.port.source();
		self.port.published = Some(state.clone());
		alloc::vec![
			ProviderUpdate { revision, kind: ProviderUpdateKind::Snapshot, source: Some(ProviderSource { local: u32::from(CONNECTOR) - 1, state }), gone: None },
			ProviderUpdate { revision, kind: ProviderUpdateKind::SnapshotEnd, source: None, gone: None }
		]
	}
	// A `usb-c` source advertises no control.
	fn command(&mut self, _command: proto::system::ProviderCommand) -> Result<ControlOutcome, Error> {
		Err(Error::Unsupported)
	}
	fn query(&mut self, local: u32) -> Result<SourceState, Error> {
		if local != u32::from(CONNECTOR) - 1 {
			return Err(Error::NotFound);
		}
		Ok(self.port.source())
	}
}

// ONE REQUEST FROM A CONSUMER: a stream opened, or a request or a query answered.
fn serve(port: &mut Port, index: usize) -> bool {
	let channel = port.serving.at(index);
	let token = port.serving.token_at(index);
	let mut buf = core::mem::take(&mut port.buf);
	let polled = try_recv_caps(channel, &mut buf);
	let (len, mut handles) = match polled {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => {
			port.buf = buf;
			return true;
		}
		PolledCaps::Closed => {
			port.buf = buf;
			return false;
		}
	};
	let request = buf[..len].to_vec();
	port.buf = buf;
	let op = if request.len() >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
	if token == TOKEN_TYPEC && op == typec_provider::OP_UPDATES {
		let Some((corr, items)) = typec_provider::updates_open(&mut TypecView { port: &mut *port }, &request, &mut handles) else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		if port.typec_stream != 0 {
			close(port.typec_stream);
		}
		port.typec_stream = producer;
		port.typec_seq = 0;
		for item in &items {
			port.emit_typec(item);
		}
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		return true;
	}
	if token == TOKEN_POWER && op == power_provider::OP_UPDATES {
		let Some((corr, items)) = power_provider::updates_open(&mut PowerView { port: &mut *port }, &request, &mut handles) else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		if port.power_stream != 0 {
			close(port.power_stream);
		}
		port.power_stream = producer;
		port.power_seq = 0;
		for item in &items {
			port.emit_power(item);
		}
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		return true;
	}
	let mut reply = alloc::vec![0u8; 4096];
	let mut reply_handles = Handles::new();
	let written = match token {
		TOKEN_TYPEC => typec_provider::dispatch(&mut TypecView { port: &mut *port }, &request, &mut handles, &mut reply, &mut reply_handles),
		TOKEN_POWER => power_provider::dispatch(&mut PowerView { port: &mut *port }, &request, &mut handles, &mut reply, &mut reply_handles),
		_ => None,
	};
	for &handle in handles.as_slice() {
		close(handle);
	}
	match written {
		Some(written) => {
			send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
			true
		}
		None => false,
	}
}

fn refuse(bootstrap: u64, bind: &common::Bind, name: &str, why: &str, code: driver_protocol::DriverFailureCode) -> ! {
	say(name, why);
	common::failed(bootstrap, bind, code)
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
		refuse(bootstrap, &bind, &name, "it needs an I2C address and an alert line, and its firmware named no pair of them", driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	// THE BOARD'S DESCRIPTION: a connector that sources is refused before READY.
	let mut block = [0u8; MAX_DEVICE_PROPERTIES];
	let len = device_properties(resources.device, &mut block);
	let block = if len > 0 { &block[..(len as usize).min(MAX_DEVICE_PROPERTIES)] } else { &block[..0] };
	let (sink, why) = match typec_tcpci::describe(block, bind.info.platform.source == PLATFORM_SOURCE_ACPI) {
		Described::Sink(sink) => (sink, None),
		Described::TypeCOnly(why) => (Sink { pdos: Vec::new(), operational_microwatts: 0 }, Some(why)),
		Described::Refused(role) => refuse(bootstrap, &bind, &name, &format!("its connector is described as {role}: nothing here can source - refused"), driver_protocol::DriverFailureCode::UnsupportedDevice),
	};
	let bus = match ScopedBus::new(i2c_device::Client::new(ChannelTransport { chan: i2c })) {
		Ok(bus) => bus,
		Err(why) => refuse(bootstrap, &bind, &name, &format!("its I2C connection cannot carry register transfers - {why:?}"), driver_protocol::DriverFailureCode::ResourceUnusable),
	};
	let Some(address) = SlaveAddress::new(bus.address()) else { refuse(bootstrap, &bind, &name, "its I2C connection names a reserved address", driver_protocol::DriverFailureCode::ResourceUnusable) };
	// THE ALERT LINE: level and active low, as TCPCI's ALERT# is; its events are the loop's wake.
	match gpio_device::Client::with_deadline(ChannelTransport { chan: line }, clock() + TICKS).line() {
		Some(Ok(scoped)) if matches!(scoped.trigger, GpioTrigger::Low) => {}
		Some(Ok(scoped)) => refuse(bootstrap, &bind, &name, &format!("its alert line is scoped {:?}, and TCPCI's alert is level and active low", scoped.trigger), driver_protocol::DriverFailureCode::ResourceUnusable),
		_ => refuse(bootstrap, &bind, &name, "its alert line did not say how it is scoped", driver_protocol::DriverFailureCode::ResourceUnusable),
	}
	let events = gpio_device::Client::with_deadline(ChannelTransport { chan: line }, clock() + TICKS).events().unwrap_or(0);
	if events == 0 {
		refuse(bootstrap, &bind, &name, "its alert line gave no event stream", driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	let report = format!("driver.tcpci: {name}: online (address {:#04x})", address.get());
	if !common::online(bootstrap, &bind, report.as_bytes(), &[]) {
		exit();
	}
	let runs_pd = why.is_none();
	match why {
		None => say(&name, &format!("the board describes a sink: {}", describe_sink(&sink))),
		Some(why) => say(&name, &format!("{why} - it runs no Power Delivery and sinks at the Type-C current only")),
	}
	let said = Engine::new(Sink { pdos: Vec::new(), operational_microwatts: 0 }).report();
	let kept = sink.clone();
	let mut port = Port { name, bootstrap, bind, bus, address, line, events, engine: Engine::new(sink), runs_pd, measures: false, deadlines: [0; TIMERS.len()], cc_status: 0, header: 0, power_control: 0, last_refusal: None, said, said_contract: None, said_transition: None, dirty: false, serving: common::Serving::from_offers(&[]), revision: 1, typec_stream: 0, typec_seq: 0, power_revision: 1, power_stream: 0, power_seq: 0, published: None, buf: alloc::vec![0u8; 8192], sink: kept };
	// THE CONTROLLER: who it is, its initialisation awaited, and what it can do.
	let (vendor, product, tc, pd) = (port.read16(tcpci::VENDOR_ID), port.read16(tcpci::PRODUCT_ID), port.read16(tcpci::TC_REVISION), port.read16(tcpci::PD_REVISION));
	let started = clock();
	while port.read8(tcpci::POWER_STATUS) & tcpci::POWER_UNINITIALIZED != 0 {
		if clock() >= started + INIT_TICKS {
			port.say("the controller did not finish its initialisation within a second");
			common::failed(port.bootstrap, &port.bind, driver_protocol::DriverFailureCode::DeviceNotResponding);
		}
		if common::wait_or_answer_until(port.bootstrap, &port.bind, &[], clock() + 1, None).is_none() {
			port.stop();
		}
	}
	let capabilities = port.read16(tcpci::DEVICE_CAPABILITIES_1);
	if capabilities & tcpci::CAPABILITY_SINK_VBUS == 0 {
		port.say("the controller cannot switch the sink path, and the engine keeps none of its invariants without it - refused");
		common::failed(port.bootstrap, &port.bind, driver_protocol::DriverFailureCode::UnsupportedDevice);
	}
	port.measures = capabilities & tcpci::CAPABILITY_VBUS_MEASUREMENT != 0;
	port.say(&format!("controller {vendor:04x}:{product:04x}, Type-C revision {tc:#06x}, Power Delivery revision {pd:#06x}{}", if port.measures { ", VBUS measured with alarms" } else { ", VBUS not measured" }));
	port.configure();
	// THE PUBLICATIONS FIRST, before the engine starts: a negotiation the bind begins must not wait on the offers -
	// a source runs its SenderResponseTimer from its capabilities, and the offers are the manager's to answer.
	let mut ends: Vec<(u16, u64)> = Vec::new();
	let mut fars: Vec<(u16, u16, u64, &[u8])> = Vec::new();
	for (token, kind, published) in [
		(TOKEN_TYPEC, driver_protocol::provider::TYPEC_CONNECTOR, driver_protocol::provider::TYPEC_NAME),
		(TOKEN_POWER, driver_protocol::provider::POWER_SOURCE, driver_protocol::provider::TYPEC_POWER_NAME),
	] {
		let Some((near, far)) = channel() else { exit() };
		ends.push((token, near));
		fars.push((token, kind, far, published));
	}
	port.serving = common::Serving::from_offers(&ends);
	for (token, kind, far, published) in fars {
		if !common::offer_named(port.bootstrap, &port.bind, kind, token, published, far) {
			exit();
		}
	}
	port.say("publishes typec-connector and power-source");
	// THE PORT AS FOUND.
	port.bind_afresh();
	common::takes_sleep();
	// THE LOOP: the alert, the timers and the consumers.
	loop {
		let deadline = port.next_deadline();
		let events = port.events;
		match common::wait_providers_until(port.bootstrap, &port.bind, &mut port.serving, &[events], deadline) {
			None => port.stop(),
			// A TIMER, or the sleep's `SUSPEND` handed back.
			Some(None) => {
				let (bootstrap, bind) = (port.bootstrap, port.bind);
				if !common::take_sleep_step(bootstrap, &bind, &mut port) {
					port.stop();
				}
				port.expire();
			}
			Some(Some(common::ProviderReady::Connected(_))) => {}
			Some(Some(common::ProviderReady::Device(_))) => {
				if !port.drain_events() {
					if common::stop_requested() {
						port.stop();
					}
					port.say("its alert line's controller went away");
					common::failed(port.bootstrap, &port.bind, driver_protocol::DriverFailureCode::ResourceUnusable);
				}
				port.service();
				port.acknowledge();
			}
			Some(Some(common::ProviderReady::Consumer(index))) => {
				if !serve(&mut port, index) {
					let token = port.serving.close_at(index);
					if token == TOKEN_TYPEC && port.typec_stream != 0 {
						close(port.typec_stream);
						port.typec_stream = 0;
					}
					if token == TOKEN_POWER && port.power_stream != 0 {
						close(port.power_stream);
						port.power_stream = 0;
					}
					if !common::disconnected(port.bootstrap, &port.bind, token) {
						exit();
					}
				}
			}
		}
		// A TIMER DUE WHILE SOMETHING ELSE WOKE THE LOOP is not left for the next wait.
		port.expire();
	}
}
