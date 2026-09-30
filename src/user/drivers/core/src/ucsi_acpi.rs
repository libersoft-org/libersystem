// driver.ucsi-acpi - A UCSI PLATFORM POLICY MANAGER over ACPI: the platform device whose `_HID` or `_CID` is `PNP0CA0`
// (firmware built on Intel's reference design gives it its own `_HID`, `USBC000`, and names UCSI by `_CID`). The
// platform's PPM runs Power Delivery; this driver is UCSI's operating-system policy manager, and one process serves
// every connector the PPM reports.
//
// THE TRANSPORT. The mailbox is the claim's `_CRS` memory range, SHARED with the node's own AML, which copies between
// it and the embedded controller inside its `_DSM` and before its `Notify`. `_DSM` function 1 after every write of
// CONTROL and MESSAGE_OUT, so the platform takes them; function 2, which has the platform refresh the mailbox, ONLY
// where no notification has - VERSION at bind, and the reset's polled completion - since firmware may spoil its own
// copy once it has notified. `Notify(0x80)` on the node is the PPM's signal. The discipline is the `ucsi` library's.
//
// WHAT IT PUBLISHES, beside READY - which comes first, once the mailbox is mapped, because a namespace device's node is
// answered only to an online binding and a slow first reset must not hold the bind:
//   `typec-connector`  every connector, to TypeCService, and the operator's requests;
//   `power-source`     every connector that can sink, as a `usb-c` source, to PowerService.
// Both are offered once every connector has been read.
//
// WHAT IT CONFIGURES after every reset - the proposed default, the owner's to confirm: swaps accepted in both
// directions through `SET_UOR` and `SET_PDR`, on a connector that can swap; the operation modes left as the PPM reports
// them. NO COMMAND THAT CHANGES POWER is issued: on UCSI the platform owns power.
//
// RECOVERY. A command or an acknowledgement not completed within ten seconds is a silent PPM: every connector is
// reported as not answering, the PPM is reset and configured and every connector read again. Two failed resets in a
// row end the binding with a driver-reported failure, and DeviceManager's restart policy takes over.
//
// THE SLEEP. A `SUSPEND` arriving while a command waits is held until that command completes within its ten-second
// bound; then no command is sent until `RESUME`. The resume reports every connector as not answering, enables the
// notifications again - a PPM that lost its power lost them - and reads every connector again, each answering again
// as it is read; a PPM that does not answer is recovered as above.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use drivers::common;
use drivers::typec_ucsi::{self, DISPLAYPORT_SVID, MAX_MODES, MAX_OFFERS, Port};
use ipc_client::ChannelTransport;
use proto::generated::liber::typec::v1 as typec;
use proto::system::{ControlOutcome, Error, ProviderSource, ProviderUpdate, ProviderUpdateKind, SourceState, acpi_node, power_provider};
use rt::*;
use typec::{Answer, Outcome, Refusal, RefusedRequest, Request, RequestKind, typec_provider};
use ucsi::answer::{self, Capability, ConnectorCapability, ConnectorStatus};
use ucsi::command::{self, Command, Recipient};
use ucsi::{Cci, Completion, Exchange, Failure, Layout, Ppm};
use wire::Handles;

const TOKEN_TYPEC: u16 = 0;
const TOKEN_POWER: u16 = 1;
// UCSI's `_DSM`.
const DSM_UUID: [u8; 16] = uuid(*b"6f8398c27ca411e4ad36631042b5008f");
// How long one question to the node may take - a `_DSM` included.
const NODE_TICKS: u64 = 5 * TICKS_PER_SECOND;
// The connector change that reports a swap's result follows its command within five seconds.
const RESULT_MS: u64 = 5_000;
// Deep enough that a run of changes never waits on the consumer.
const STREAM_DEPTH: u64 = 64;
// The most connectors served: the sixteen PowerService's local identities reach.
const MAX_CONNECTORS: u8 = 16;

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

fn now_ms() -> u64 {
	clock() * 1000 / TICKS_PER_SECOND
}

fn ticks_at(ms: u64) -> u64 {
	(ms * TICKS_PER_SECOND).div_ceil(1000)
}

// An empty package, the `_DSM`'s fourth argument.
const NO_ARGUMENTS: [u8; 5] = [0x04, 0, 0, 0, 0];

struct Ucsi {
	name: String,
	bootstrap: u64,
	bind: common::Bind,
	mailbox: u64,
	node: u64,
	notify: u64,
	layout: Layout,
	capability: Option<Capability>,
	ports: Vec<Port>,
	// Connector changes the PPM indicated while something else ran, to read next.
	changes: Vec<u8>,
	// Whether a stop came while a command waited.
	stopping: bool,
	serving: common::Serving,
	revision: u64,
	typec_stream: u64,
	typec_seq: u32,
	power_revision: u64,
	power_stream: u64,
	power_seq: u32,
	// What each connector's source last published, so an unchanged one is not published again.
	published: Vec<Option<SourceState>>,
	buf: Vec<u8>,
}

impl Ucsi {
	fn say(&self, text: &str) {
		let line = format!("{}: {text}\n", self.name);
		print(line.as_bytes());
	}

	fn node_client(&self) -> acpi_node::Client<ChannelTransport> {
		acpi_node::Client::with_deadline(ChannelTransport { chan: self.node }, clock() + NODE_TICKS)
	}

	fn dsm(&self, function: u64) -> Result<Vec<u8>, Failure> {
		match self.node_client().dsm(&DSM_UUID, &1, &function, &NO_ARGUMENTS) {
			Some(Ok(bytes)) => Ok(bytes),
			_ => Err(Failure::Transport),
		}
	}

	fn read32(&self, offset: usize) -> u32 {
		// SAFETY: inside the mailbox the claim mapped, at a field's offset the layout checked against its length.
		unsafe { core::ptr::read_volatile((self.mailbox + offset as u64) as *const u32) }
	}

	fn read8(&self, offset: usize) -> u8 {
		// SAFETY: as above.
		unsafe { core::ptr::read_volatile((self.mailbox + offset as u64) as *const u8) }
	}

	fn write8(&self, offset: usize, value: u8) {
		// SAFETY: as above.
		unsafe { core::ptr::write_volatile((self.mailbox + offset as u64) as *mut u8, value) };
	}

	fn write64(&self, offset: usize, value: u64) {
		// SAFETY: as above; CONTROL is eight-byte aligned in every layout.
		unsafe { core::ptr::write_volatile((self.mailbox + offset as u64) as *mut u64, value) };
	}

	fn connectors(&self) -> u8 {
		self.capability.map_or(0, |capability| capability.connectors)
	}

	// ONE COMMAND under the discipline; a connector change it saw is kept for the loop to read.
	fn run(&mut self, command: Command, change_ack: bool) -> Result<Exchange, Failure> {
		let layout = self.layout;
		let connectors = self.connectors();
		let exchange = ucsi::execute(self, &layout, command, &[], connectors, change_ack)?;
		if let Some(number) = exchange.change
			&& !change_ack
			&& !self.changes.contains(&number)
		{
			self.changes.push(number);
		}
		Ok(exchange)
	}

	// A command's answer, when it gave one; `None` for a command the platform does not support.
	fn read(&mut self, command: Command) -> Result<Option<Vec<u8>>, Failure> {
		match self.run(command, false)?.completion {
			Completion::Answered(data) => Ok(Some(data)),
			Completion::NotSupported => Ok(None),
			Completion::Error => {
				let error = self.error_status()?;
				self.say(&format!("command {:#04x} completed with an error ({error:#x})", command.code()));
				Ok(None)
			}
		}
	}

	fn error_status(&mut self) -> Result<u32, Failure> {
		match self.run(command::get_error_status(), false)?.completion {
			Completion::Answered(data) => Ok(answer::error_status(&data).unwrap_or(0)),
			_ => Ok(0),
		}
	}

	// ------------------------------------------------------------------ the PPM's state

	// RESET, CONFIGURE, READ: what bind does, and what recovery does again.
	fn start(&mut self) -> Result<(), Failure> {
		ucsi::reset(self)?;
		match self.run(command::set_notification_enable(command::NOTIFY_ALL), false)?.completion {
			Completion::Answered(_) => {}
			_ => return Err(Failure::Malformed),
		}
		let data = self.read(command::get_capability())?.ok_or(Failure::Malformed)?;
		let capability = Capability::decode(&data).ok_or(Failure::Malformed)?;
		if capability.connectors > MAX_CONNECTORS {
			self.say(&format!("the PPM reports {} connectors; the first {MAX_CONNECTORS} are served and the rest are not", capability.connectors));
		}
		self.capability = Some(capability);
		let count = capability.connectors.min(MAX_CONNECTORS);
		let mut ports = Vec::new();
		for number in 1..=count {
			let data = self.read(command::get_connector_capability(number))?.ok_or(Failure::Malformed)?;
			let connector = ConnectorCapability::decode(&data).ok_or(Failure::Malformed)?;
			// THE CONFIGURATION: swaps accepted both ways where the connector can swap, and nothing about power.
			if connector.host() && connector.device() {
				self.configure(command::set_uor(number, connector.host(), connector.device(), true), number, "SET_UOR")?;
			}
			if connector.provider && connector.consumer {
				self.configure(command::set_pdr(number, connector.provider, connector.consumer, true), number, "SET_PDR")?;
			}
			let mut port = Port::new(number, connector);
			// A reset's last refusal is carried across it: it says what an operator was last told.
			if let Some(old) = self.ports.iter().find(|old| old.number == number) {
				port.last_refusal = old.last_refusal.clone();
				port.pin = old.pin;
			}
			ports.push(port);
		}
		self.ports = ports;
		self.changes.clear();
		for at in 0..self.ports.len() {
			self.read_port(at, false)?;
		}
		Ok(())
	}

	// BACK FROM A SLEEP: see the head of this file.
	fn wake(&mut self) -> Result<(), Failure> {
		// Whatever the PPM said while the machine slept is read again below, as a whole.
		let mut buf = [0u8; 64];
		while self.notify != 0 && matches!(try_recv_caps(self.notify, &mut buf), PolledCaps::Message { .. }) {}
		match self.run(command::set_notification_enable(command::NOTIFY_ALL), false)?.completion {
			Completion::Answered(_) => {}
			_ => return Err(Failure::Malformed),
		}
		self.changes.clear();
		for at in 0..self.ports.len() {
			self.read_port(at, false)?;
			self.publish_port(at);
		}
		Ok(())
	}

	fn configure(&mut self, command: Command, number: u8, what: &str) -> Result<(), Failure> {
		match self.run(command, false)?.completion {
			Completion::Answered(_) => Ok(()),
			Completion::NotSupported => {
				self.say(&format!("connector {number}: {what} is not supported - its swaps stay as the platform set them"));
				Ok(())
			}
			Completion::Error => {
				let error = self.error_status()?;
				self.say(&format!("connector {number}: {what} was refused ({error:#x}) - its swaps stay as the platform set them"));
				Ok(())
			}
		}
	}

	// ONE CONNECTOR READ: its status - acknowledging a change with it when `change_ack` - and, with a partner, the
	// facts whose commands the platform offers.
	fn read_port(&mut self, at: usize, change_ack: bool) -> Result<(), Failure> {
		let number = self.ports[at].number;
		let version = self.layout.version;
		let status = match self.run(command::get_connector_status(number), change_ack)?.completion {
			Completion::Answered(data) => ConnectorStatus::decode(&data, version).ok_or(Failure::Malformed)?,
			_ => return Err(Failure::Malformed),
		};
		let capability = self.capability.ok_or(Failure::Malformed)?;
		let mut offers = Vec::new();
		let mut cable = None;
		let mut modes = Vec::new();
		let mut own_modes = Vec::new();
		let mut current = None;
		let mut attention = None;
		if status.connected {
			if capability.has(Capability::PDO_DETAILS) {
				while offers.len() < MAX_OFFERS {
					let count = (MAX_OFFERS - offers.len()).min(4) as u8;
					let Some(data) = self.read(command::get_pdos(number, true, offers.len() as u8, count, true))? else { break };
					let got = answer::pdos(&data).ok_or(Failure::Malformed)?;
					let more = got.len() == usize::from(count);
					offers.extend(got);
					if !more {
						break;
					}
				}
			}
			if capability.has(Capability::CABLE_DETAILS)
				&& let Some(data) = self.read(command::get_cable_property(number))?
			{
				cable = Some(answer::CableProperty::decode(&data).ok_or(Failure::Malformed)?);
			}
			if capability.has(Capability::ALT_MODE_DETAILS) {
				modes = self.alternate_modes(Recipient::Partner, number)?;
				own_modes = self.alternate_modes(Recipient::Connector, number)?;
				if let Some(data) = self.read(command::get_current_cam(number))? {
					current = data.first().copied().filter(|offset| usize::from(*offset) < own_modes.len());
				}
				if current.and_then(|offset| own_modes.get(usize::from(offset))).is_some_and(|mode| mode.svid == DISPLAYPORT_SVID)
					&& let Some(data) = self.read(command::get_attention_vdo(number))?
				{
					attention = answer::attention_vdo(&data);
				}
			}
		}
		let port = &mut self.ports[at];
		port.status = Some(status);
		port.offers = offers;
		port.cable = cable;
		port.modes = modes;
		port.own_modes = own_modes;
		if current.is_none() {
			port.pin = None;
		}
		port.current = current;
		port.attention = attention;
		port.answering = true;
		Ok(())
	}

	fn alternate_modes(&mut self, recipient: Recipient, number: u8) -> Result<Vec<answer::AlternateMode>, Failure> {
		let mut modes = Vec::new();
		while modes.len() < MAX_MODES {
			let Some(data) = self.read(command::get_alternate_modes(recipient, number, modes.len() as u8, 2))? else { break };
			let got = answer::alternate_modes(&data).ok_or(Failure::Malformed)?;
			let more = got.len() == 2;
			modes.extend(got);
			if !more {
				break;
			}
		}
		Ok(modes)
	}

	// ------------------------------------------------------------------ publishing

	fn record(&self, at: usize) -> Option<typec::Connector> {
		Some(typec_ucsi::record(&self.ports[at], &self.capability?, self.layout.version))
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

	fn source(&self, at: usize) -> Option<SourceState> {
		let port = &self.ports[at];
		port.sinks().then(|| power_model::usbc::usb_c(&typec_ucsi::supply(port, self.layout.version)))
	}

	// EVERY CONNECTOR PUBLISHED AS IT NOW IS: an update to TypeCService, and to PowerService where its supply changed.
	fn publish(&mut self) {
		for at in 0..self.ports.len() {
			self.publish_port(at);
		}
	}

	fn publish_port(&mut self, at: usize) {
		if let Some(connector) = self.record(at) {
			self.revision += 1;
			let update = typec::ProviderUpdate { revision: self.revision, kind: typec::UpdateKind::Updated, connector: Some(connector), gone: None };
			self.emit_typec(&update);
		}
		let state = self.source(at);
		if state.is_some() && self.published.get(at) != Some(&state) {
			if self.published.len() <= at {
				self.published.resize(at + 1, None);
			}
			self.published[at] = state.clone();
			self.power_revision += 1;
			let local = u32::from(self.ports[at].number) - 1;
			let update = ProviderUpdate { revision: self.power_revision, kind: ProviderUpdateKind::Updated, source: state.map(|state| ProviderSource { local, state }), gone: None };
			self.emit_power(&update);
		}
	}

	// ------------------------------------------------------------------ recovery

	// THE PPM WENT SILENT, OR ANSWERED WHAT CANNOT BE AN ANSWER: every connector reported as not answering, then reset
	// and read again - twice at most in a row before the binding ends.
	fn recover(&mut self, why: Failure) {
		self.say(&format!("the PPM failed ({why:?}) - every connector is reported as not answering, and it is reset"));
		for port in self.ports.iter_mut() {
			port.answering = false;
		}
		self.publish();
		for attempt in 1..=2 {
			if self.stopping {
				return;
			}
			match self.start() {
				Ok(()) => {
					self.say("the PPM answers again after its reset; every connector was read again");
					self.publish();
					return;
				}
				Err(failure) => self.say(&format!("reset {attempt} of 2 failed ({failure:?})")),
			}
		}
		self.say("two resets in a row failed - the binding ends");
		common::failed(self.bootstrap, &self.bind, driver_protocol::DriverFailureCode::DeviceNotResponding);
	}

	// The PPM notified while nothing waited: a connector changed.
	fn on_notify(&mut self) {
		let mut buf = [0u8; 64];
		let mut notified = false;
		loop {
			match try_recv_caps(self.notify, &mut buf) {
				PolledCaps::Message { len, mut handles } => {
					if acpi_node::notifications_read(&buf[..len], &mut handles).is_some_and(|notification| notification.value == 0x80) {
						notified = true;
					}
				}
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					close(self.notify);
					self.notify = 0;
					break;
				}
			}
		}
		if notified
			&& let Some(number) = Cci(self.read32(ucsi::CCI)).connector()
			&& !self.changes.contains(&number)
		{
			self.changes.push(number);
		}
		self.read_changes();
	}

	// EVERY CONNECTOR CHANGE WAITING, read with its status and acknowledged with it.
	fn read_changes(&mut self) {
		while let Some(number) = self.changes.pop() {
			let Some(at) = self.ports.iter().position(|port| port.number == number) else { continue };
			match self.read_port(at, true) {
				Ok(()) => self.publish_port(at),
				Err(failure) => {
					self.recover(failure);
					return;
				}
			}
		}
	}

	// ------------------------------------------------------------------ requests

	fn refuse(&mut self, at: usize, kind: RequestKind, reason: Refusal, error: u32) -> Answer {
		self.ports[at].last_refusal = Some(RefusedRequest { kind, reason, error });
		self.publish_port(at);
		Answer { outcome: Outcome::Refused, reason: Some(reason), error }
	}

	// ONE OPERATOR REQUEST: refused before any command where the rules say so, then the command, then - for a swap -
	// the connector change that reports its result, within five seconds.
	fn request(&mut self, request: &Request) -> Answer {
		let Some(at) = self.ports.iter().position(|port| port.number == request.connector) else { return Answer { outcome: Outcome::Refused, reason: Some(Refusal::ConnectorCannot), error: 0 } };
		let Some(capability) = self.capability else { return Answer { outcome: Outcome::Indeterminate, reason: None, error: 0 } };
		let number = request.connector;
		let (command, pin) = match request.kind {
			RequestKind::DataRoleSwap => {
				let Some(role) = request.data_role else { return self.refuse(at, request.kind, Refusal::ConnectorCannot, 0) };
				if let Some(reason) = typec_ucsi::data_swap_refusal(&self.ports[at], role) {
					return self.refuse(at, request.kind, reason, 0);
				}
				(command::set_uor(number, role == typec::DataRole::Host, role == typec::DataRole::Device, true), None)
			}
			RequestKind::PowerRoleSwap => {
				let Some(role) = request.power_role else { return self.refuse(at, request.kind, Refusal::ConnectorCannot, 0) };
				if let Some(reason) = typec_ucsi::power_swap_refusal(&self.ports[at], role) {
					return self.refuse(at, request.kind, reason, 0);
				}
				(command::set_pdr(number, role == typec::PowerRole::Source, role == typec::PowerRole::Sink, true), None)
			}
			RequestKind::EnterMode | RequestKind::ExitMode => {
				let enter = request.kind == RequestKind::EnterMode;
				let offset = match typec_ucsi::mode_offset(&self.ports[at], &capability, request.svid) {
					Ok(offset) => offset,
					Err(reason) => return self.refuse(at, request.kind, reason, 0),
				};
				let pin = if enter && request.svid == DISPLAYPORT_SVID {
					match typec_ucsi::pin_assignment(request.vdo) {
						Some(pin) => Some(pin),
						None => return self.refuse(at, request.kind, Refusal::ConnectorCannot, 0),
					}
				} else {
					None
				};
				(command::set_new_cam(number, enter, offset, pin.map_or(0, typec_ucsi::dp_configuration)), pin)
			}
		};
		let completion = match self.run(command, false) {
			Ok(exchange) => exchange.completion,
			Err(failure) => {
				self.recover(failure);
				return Answer { outcome: Outcome::Indeterminate, reason: None, error: 0 };
			}
		};
		match completion {
			Completion::NotSupported => return self.refuse(at, request.kind, Refusal::NotOffered, 0),
			Completion::Error => {
				let error = match self.error_status() {
					Ok(error) => error,
					Err(failure) => {
						self.recover(failure);
						return Answer { outcome: Outcome::Indeterminate, reason: None, error: 0 };
					}
				};
				return self.refuse(at, request.kind, Refusal::RefusedByPlatform, error);
			}
			Completion::Answered(_) => {}
		}
		if request.kind == RequestKind::EnterMode {
			self.ports[at].pin = pin;
		}
		// THE RESULT: the connector change the PPM reports, read with its status.
		let deadline = now_ms() + RESULT_MS;
		let reached = |port: &Port| match request.kind {
			RequestKind::DataRoleSwap => port.data_role() == request.data_role,
			RequestKind::PowerRoleSwap => port.power_role() == request.power_role,
			RequestKind::EnterMode => port.entered_svid() == Some(request.svid),
			RequestKind::ExitMode => port.entered_svid() != Some(request.svid),
		};
		loop {
			self.read_changes();
			if reached(&self.ports[at]) {
				self.publish_port(at);
				return Answer { outcome: Outcome::Done, reason: None, error: 0 };
			}
			if now_ms() >= deadline || self.stopping {
				return Answer { outcome: Outcome::Indeterminate, reason: None, error: 0 };
			}
			match self.wait_notify(deadline) {
				Ok(true) => {
					if let Some(number) = Cci(self.read32(ucsi::CCI)).connector()
						&& !self.changes.contains(&number)
					{
						self.changes.push(number);
					}
				}
				Ok(false) => return Answer { outcome: Outcome::Indeterminate, reason: None, error: 0 },
				Err(_) => return Answer { outcome: Outcome::Indeterminate, reason: None, error: 0 },
			}
		}
	}
}

// THE SLEEP - see the head of this file. `SUSPEND` reaches this only between commands.
impl common::SleepStep for Ucsi {
	fn suspend(&mut self, _request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		self.say("suspended - no command is sent until the resume");
		driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
	}

	fn resume(&mut self, _lost_power: bool) -> bool {
		for port in self.ports.iter_mut() {
			port.answering = false;
		}
		self.publish();
		match self.wake() {
			Ok(()) => self.say("resumed - notifications enabled again and every connector read again"),
			Err(failure) => self.recover(failure),
		}
		true
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(&mut self.serving)
	}
}

// THE PPM AS THE DISCIPLINE REACHES IT.
impl Ppm for Ucsi {
	fn send(&mut self, control: u64, message_out: &[u8]) -> Result<(), Failure> {
		for (at, byte) in message_out.iter().take(self.layout.message_size).enumerate() {
			self.write8(self.layout.message_out + at, *byte);
		}
		self.write64(ucsi::CONTROL, control);
		self.dsm(1).map(|_| ())
	}

	// WAITING ANSWERS THE MANAGER: a heartbeat or a stop while the PPM thinks is taken as it comes.
	fn wait_notify(&mut self, deadline_ms: u64) -> Result<bool, Failure> {
		let mut buf = [0u8; 64];
		loop {
			if self.notify == 0 {
				return Err(Failure::Transport);
			}
			match try_recv_caps(self.notify, &mut buf) {
				PolledCaps::Message { len, mut handles } => {
					if acpi_node::notifications_read(&buf[..len], &mut handles).is_some_and(|notification| notification.value == 0x80) {
						return Ok(true);
					}
					continue;
				}
				PolledCaps::Closed => {
					close(self.notify);
					self.notify = 0;
					return Err(Failure::Transport);
				}
				PolledCaps::Empty => {}
			}
			if now_ms() >= deadline_ms {
				return Ok(false);
			}
			match common::wait_or_answer_until(self.bootstrap, &self.bind, &[self.notify], ticks_at(deadline_ms), None) {
				Some(_) => {}
				None => {
					self.stopping = true;
					return Err(Failure::Transport);
				}
			}
		}
	}

	fn refresh(&mut self) -> Result<(), Failure> {
		self.dsm(2).map(|_| ())
	}

	fn cci(&mut self) -> Cci {
		Cci(self.read32(ucsi::CCI))
	}

	fn message_in(&mut self, out: &mut [u8]) {
		for (at, byte) in out.iter_mut().enumerate().take(self.layout.message_size) {
			*byte = self.read8(self.layout.message_in + at);
		}
	}

	fn now_ms(&mut self) -> u64 {
		now_ms()
	}

	fn pause(&mut self) {
		sleep_until(clock() + 1);
	}
}

// ------------------------------------------------------------------ the two publications' views

struct TypecView<'a> {
	ucsi: &'a mut Ucsi,
}

impl typec_provider::Service for TypecView<'_> {
	fn updates(&mut self) -> Vec<typec::ProviderUpdate> {
		let revision = self.ucsi.revision;
		let mut items: Vec<typec::ProviderUpdate> = (0..self.ucsi.ports.len()).filter_map(|at| self.ucsi.record(at)).map(|connector| typec::ProviderUpdate { revision, kind: typec::UpdateKind::Snapshot, connector: Some(connector), gone: None }).collect();
		items.push(typec::ProviderUpdate { revision, kind: typec::UpdateKind::SnapshotEnd, connector: None, gone: None });
		items
	}
	fn request(&mut self, request: typec::Request) -> Result<Answer, Error> {
		Ok(self.ucsi.request(&request))
	}
}

struct PowerView<'a> {
	ucsi: &'a mut Ucsi,
}

impl power_provider::Service for PowerView<'_> {
	fn updates(&mut self) -> Vec<ProviderUpdate> {
		let revision = self.ucsi.power_revision;
		let mut items = Vec::new();
		for at in 0..self.ucsi.ports.len() {
			if let Some(state) = self.ucsi.source(at) {
				items.push(ProviderUpdate { revision, kind: ProviderUpdateKind::Snapshot, source: Some(ProviderSource { local: u32::from(self.ucsi.ports[at].number) - 1, state }), gone: None });
			}
		}
		items.push(ProviderUpdate { revision, kind: ProviderUpdateKind::SnapshotEnd, source: None, gone: None });
		items
	}
	// A `usb-c` source advertises no control.
	fn command(&mut self, _command: proto::system::ProviderCommand) -> Result<ControlOutcome, Error> {
		Err(Error::Unsupported)
	}
	fn query(&mut self, local: u32) -> Result<SourceState, Error> {
		let at = self.ucsi.ports.iter().position(|port| u32::from(port.number) == local + 1).ok_or(Error::NotFound)?;
		self.ucsi.source(at).ok_or(Error::NotFound)
	}
}

// ONE REQUEST FROM A CONSUMER: a stream opened, or a request or a query answered.
fn serve(ucsi: &mut Ucsi, index: usize) -> bool {
	let channel = ucsi.serving.at(index);
	let token = ucsi.serving.token_at(index);
	let mut buf = core::mem::take(&mut ucsi.buf);
	let polled = try_recv_caps(channel, &mut buf);
	let (len, mut handles) = match polled {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => {
			ucsi.buf = buf;
			return true;
		}
		PolledCaps::Closed => {
			ucsi.buf = buf;
			return false;
		}
	};
	let request = buf[..len].to_vec();
	ucsi.buf = buf;
	let op = if request.len() >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
	if token == TOKEN_TYPEC && op == typec_provider::OP_UPDATES {
		let Some((corr, items)) = typec_provider::updates_open(&mut TypecView { ucsi: &mut *ucsi }, &request, &mut handles) else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		if ucsi.typec_stream != 0 {
			close(ucsi.typec_stream);
		}
		ucsi.typec_stream = producer;
		ucsi.typec_seq = 0;
		for item in &items {
			ucsi.emit_typec(item);
		}
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		return true;
	}
	if token == TOKEN_POWER && op == power_provider::OP_UPDATES {
		let Some((corr, items)) = power_provider::updates_open(&mut PowerView { ucsi: &mut *ucsi }, &request, &mut handles) else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		if ucsi.power_stream != 0 {
			close(ucsi.power_stream);
		}
		ucsi.power_stream = producer;
		ucsi.power_seq = 0;
		for item in &items {
			ucsi.emit_power(item);
		}
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		return true;
	}
	let mut reply = alloc::vec![0u8; 4096];
	let mut reply_handles = Handles::new();
	let written = match token {
		TOKEN_TYPEC => typec_provider::dispatch(&mut TypecView { ucsi: &mut *ucsi }, &request, &mut handles, &mut reply, &mut reply_handles),
		TOKEN_POWER => power_provider::dispatch(&mut PowerView { ucsi: &mut *ucsi }, &request, &mut handles, &mut reply, &mut reply_handles),
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

fn fail(bootstrap: u64, bind: &common::Bind, name: &str, why: &str, code: driver_protocol::DriverFailureCode) -> ! {
	let line = format!("{name}: {why}\n");
	print(line.as_bytes());
	common::failed(bootstrap, bind, code)
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	let name = format!("driver.ucsi-acpi: {}", String::from_utf8_lossy(bind.info.platform.identity()));
	// THE MAILBOX, mapped as the claim minted it.
	let range = bind.info.platform.mmio().first().map_or(0, |range| range.len as usize);
	let mailbox = if resources.device != 0 { unsafe { syscall(SYS_DEVICE_MEMORY_MAP, resources.device, 0, 0, 0) } } else { 0 };
	if (mailbox as i64) <= 0 || range == 0 {
		fail(bootstrap, &bind, &name, "its _CRS names no mailbox the claim could map", driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	// READY FIRST: the node is answered only to an online binding, and a slow first reset must not hold the bind.
	let report = format!("{name}: online (mailbox of {range} bytes)");
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
	if node == 0 {
		fail(bootstrap, &bind, &name, "the firmware describes no node for it", driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	let mut ucsi = Ucsi { name, bootstrap, bind, mailbox, node, notify: 0, layout: Layout { version: 0, message_in: 16, message_out: 32, message_size: 16 }, capability: None, ports: Vec::new(), changes: Vec::new(), stopping: false, serving: common::Serving::from_offers(&[]), revision: 1, typec_stream: 0, typec_seq: 0, power_revision: 1, power_stream: 0, power_seq: 0, published: Vec::new(), buf: alloc::vec![0u8; 8192] };
	// THE `_DSM`: function 0's mask names functions 1 and 2, or this is no UCSI mailbox the driver can drive.
	let mask = ucsi.dsm(0).ok().and_then(|bytes| aml::wire::decode(&bytes).ok()).and_then(|value| match value {
		aml::wire::Value::Buffer(buffer) => buffer.first().copied(),
		aml::wire::Value::Integer(value) => Some(value as u8),
		_ => None,
	});
	if !mask.is_some_and(|mask| mask & 0b110 == 0b110) {
		fail(ucsi.bootstrap, &ucsi.bind, &ucsi.name, &format!("its _DSM offers no UCSI functions 1 and 2 (function 0 answered {mask:?})"), driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	// VERSION, refreshed by function 2 - no notification has refreshed anything yet.
	if ucsi.dsm(2).is_err() {
		fail(ucsi.bootstrap, &ucsi.bind, &ucsi.name, "its _DSM function 2 did not answer", driver_protocol::DriverFailureCode::DeviceNotResponding);
	}
	let version = u16::from(ucsi.read8(ucsi::VERSION)) | u16::from(ucsi.read8(ucsi::VERSION + 1)) << 8;
	ucsi.layout = match Layout::of(version, range) {
		Ok(layout) => layout,
		Err(refusal) => fail(ucsi.bootstrap, &ucsi.bind, &ucsi.name, &format!("its mailbox is refused - {refusal:?}"), driver_protocol::DriverFailureCode::UnsupportedDevice),
	};
	// THE NOTIFICATIONS, subscribed before the reset so none is lost.
	ucsi.notify = acpi_node::Client::with_deadline(ChannelTransport { chan: node }, clock() + NODE_TICKS).notifications().unwrap_or(0);
	if ucsi.notify == 0 {
		fail(ucsi.bootstrap, &ucsi.bind, &ucsi.name, "its node gives no notifications", driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	ucsi.say(&format!("UCSI {}.{}, messages of {} bytes", version >> 8, (version >> 4) & 0xF, ucsi.layout.message_size));
	let started = now_ms();
	let mut first = ucsi.start();
	if first.is_err() && !ucsi.stopping {
		ucsi.say(&format!("the first reset failed ({:?}) - once more", first.as_ref().err()));
		first = ucsi.start();
	}
	if let Err(failure) = first {
		if ucsi.stopping || common::stop_requested() {
			common::finish_stop(ucsi.bootstrap, &ucsi.bind, 0, true);
		}
		fail(ucsi.bootstrap, &ucsi.bind, &ucsi.name, &format!("the PPM did not come up ({failure:?})"), driver_protocol::DriverFailureCode::DeviceNotResponding);
	}
	let capability = ucsi.capability.expect("started");
	ucsi.say(&format!("{} connector(s) read in {} ms; features {:#08x}{}", ucsi.ports.len(), now_ms() - started, capability.features, if capability.has(Capability::ALT_MODE_OVERRIDE) { ", alternate-mode override" } else { "" }));
	// THE PUBLICATIONS, now that every connector has been read.
	let sinks = ucsi.ports.iter().any(|port| port.sinks());
	let mut offers: Vec<(u16, u16, &[u8])> = alloc::vec![(TOKEN_TYPEC, driver_protocol::provider::TYPEC_CONNECTOR, driver_protocol::provider::TYPEC_NAME)];
	if sinks {
		offers.push((TOKEN_POWER, driver_protocol::provider::POWER_SOURCE, driver_protocol::provider::TYPEC_POWER_NAME));
	}
	let mut ends: Vec<(u16, u64)> = Vec::new();
	let mut fars: Vec<(u16, u16, u64, &[u8])> = Vec::new();
	for (token, kind, published) in offers {
		let Some((near, far)) = channel() else { exit() };
		ends.push((token, near));
		fars.push((token, kind, far, published));
	}
	ucsi.serving = common::Serving::from_offers(&ends);
	for (token, kind, far, published) in fars {
		if !common::offer_named(ucsi.bootstrap, &ucsi.bind, kind, token, published, far) {
			exit();
		}
	}
	ucsi.say(&format!("publishes typec-connector{}", if sinks { " and power-source" } else { "" }));
	common::takes_sleep();
	loop {
		// THE SLEEP, between commands - a `SUSPEND` a command's wait took is held until that command completed.
		let (bootstrap, bind) = (ucsi.bootstrap, ucsi.bind);
		if !common::take_sleep_step(bootstrap, &bind, &mut ucsi) {
			if common::stop_requested() {
				common::finish_stop(ucsi.bootstrap, &ucsi.bind, 0, true);
			}
			exit();
		}
		// A node handed again - the ACPI service restarted - is taken, and its notifications with it.
		if let Some(fresh) = common::fresh_node()
			&& fresh != 0
		{
			ucsi.node = fresh;
			if ucsi.notify != 0 {
				close(ucsi.notify);
			}
			ucsi.notify = acpi_node::Client::with_deadline(ChannelTransport { chan: fresh }, clock() + NODE_TICKS).notifications().unwrap_or(0);
			ucsi.say("its node is handed again - read again");
			let failure = (0..ucsi.ports.len()).find_map(|at| ucsi.read_port(at, false).err());
			match failure {
				Some(failure) => ucsi.recover(failure),
				None => ucsi.publish(),
			}
		}
		let devices: Vec<u64> = if ucsi.notify != 0 { alloc::vec![ucsi.notify] } else { Vec::new() };
		match common::wait_providers_until(ucsi.bootstrap, &ucsi.bind, &mut ucsi.serving, &devices, 0) {
			None => {
				if common::stop_requested() {
					common::finish_stop(ucsi.bootstrap, &ucsi.bind, 0, true);
				}
				exit();
			}
			// The sleep's `SUSPEND`, taken at the top of the loop.
			Some(None) | Some(Some(common::ProviderReady::Connected(_))) => {}
			Some(Some(common::ProviderReady::Device(_))) => ucsi.on_notify(),
			Some(Some(common::ProviderReady::Consumer(index))) => {
				if !serve(&mut ucsi, index) {
					let token = ucsi.serving.close_at(index);
					if token == TOKEN_TYPEC && ucsi.typec_stream != 0 {
						close(ucsi.typec_stream);
						ucsi.typec_stream = 0;
					}
					if token == TOKEN_POWER && ucsi.power_stream != 0 {
						close(ucsi.power_stream);
						ucsi.power_stream = 0;
					}
					if !common::disconnected(ucsi.bootstrap, &ucsi.bind, token) {
						exit();
					}
				}
			}
		}
		if ucsi.stopping {
			common::finish_stop(ucsi.bootstrap, &ucsi.bind, 0, true);
		}
	}
}
