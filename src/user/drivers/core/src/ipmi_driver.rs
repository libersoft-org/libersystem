// driver.ipmi - ONE IPMI SYSTEM INTERFACE to a baseboard management controller: KCS or BT on PCI (class 0x0C/0x07,
// interface 0x01 or 0x02, BAR 0 its register file) or on ISA (an `IPI0001` namespace device whose `_IFT` says which,
// its `_CRS` port range the registers), or SSIF on SMBus (an `IPI0001` child of an SMBus controller, reached through
// the one address DeviceManager scopes on the controller's binding). One process per bound interface.
//
// WHAT IT PUBLISHES, beside READY - which comes first, publishing nothing, because a namespace device's node is
// answered only to an online binding and what follows is bounded by the BMC rather than by the bind window:
//   `ipmi`            the `liber:bmc@1` provider contract, to the BMC service alone;
//   `power-source`    the BMC's temperature sensors (at most sixteen) as thermal zones, polled every five seconds;
//   `watchdog`        the BMC's watchdog, as `bmc` - from the KCS, BT and PCI forms, NEVER from SSIF: a child of the
//                     SMBus controller is suspended before it and resumed after it, and a watchdog's binding must be
//                     the last suspended and the first resumed;
//   `admin-executor`  two: erasing the BMC's event log, and its chassis control - which only AdminService reaches.
//
// ONE MESSAGE AT A TIME, AND THE PET FIRST. The interface carries one request at a time; before every transaction any
// watchdog request waiting is served first, so a pet never waits behind a read - at most one transaction and its
// recovery. EVERY FORM POLLS: the interrupt a claim may carry stays masked.
//
// THE BMC UNAVAILABLE: three transactions in a row that time out mark it so - the `ipmi` provider says it, the thermal
// zones become unknown - and Get Device ID every ten seconds brings it back; a different identity on return is a
// different BMC, whose repository is read again and whose preparations are no longer valid.
//
// THE SLEEP. A transaction is never in flight where a `SUSPEND` is read, and the queue is held until `RESUME`. The
// watchdog's half, when this binding publishes one: the BMC's timer is disarmed - the BMC allows it, and it counts on
// standby power through any sleep - and re-armed at the resume with the timeout it had, which catches a resume that
// hangs until the watchdog service restores its own. The resume aborts a KCS interface left busy, reads BT's or SSIF's
// capabilities again and identifies the BMC again, as bind does.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use drivers::admin_operation::{MAX_PAYLOAD, Operations, Refusal as OperationRefusal};
use drivers::common;
use drivers::ipmi_bmc::{self, Answer, Bmc, Outcome};
use ipc_client::ChannelTransport;
use ipmi::identity::{DeviceId, Identity};
use ipmi::{Availability, Change, Failure, Request, Response, bt, kcs, sdr, ssif};
use power_model::convert::Tagged;
use proto::generated::liber::bmc::v1 as bmc;
use proto::system::{AdminAction, AdminDescriptor, AdminPrepared, AdminRead, AdminResult, ControlOutcome, Error, ProviderCommand, ProviderSource, ProviderUpdate, ProviderUpdateKind, SourceState, WatchdogDescription, acpi_node, admin_executor, power_provider, watchdog};
use rt::*;
use wire::Handles;

// The publications' tokens.
const TOKEN_IPMI: u16 = 0;
const TOKEN_POWER: u16 = 1;
const TOKEN_WATCHDOG: u16 = 2;
const TOKEN_SEL: u16 = 3;
const TOKEN_CHASSIS: u16 = 4;

// The thermal zones' poll, and the look for a BMC that went away.
const POLL_TICKS: u64 = 5 * TICKS_PER_SECOND;
const PROBE_TICKS: u64 = 10 * TICKS_PER_SECOND;
// The most temperature sensors published as thermal zones: the first, in repository order.
const MAX_ZONES: usize = 16;
// How long one question to the node may take.
const NODE_TICKS: u64 = 5 * TICKS_PER_SECOND;
// How long a preparation is valid, and the deadlines its execution is given.
const LIFETIME_TICKS: u64 = 120 * TICKS_PER_SECOND;
const SEL_CLEAR_MS: u32 = 30_000;
const CHASSIS_MS: u32 = 10_000;
// Deep enough that a run of changes never waits on the consumer.
const STREAM_DEPTH: u64 = 64;

fn now_ms() -> u64 {
	clock() * 1000 / TICKS_PER_SECOND
}

// ------------------------------------------------------------------ the registers and the bus

// A REGISTER INTERFACE THROUGH CLAIMED PORTS: `spacing` ports between registers.
#[cfg(target_arch = "x86_64")]
struct Ports {
	base: u16,
	spacing: u16,
}

#[cfg(target_arch = "x86_64")]
impl ipmi::Registers for Ports {
	fn read(&mut self, index: u8) -> u8 {
		rt::port::inb(self.base + u16::from(index) * self.spacing)
	}

	fn write(&mut self, index: u8, value: u8) {
		rt::port::outb(self.base + u16::from(index) * self.spacing, value);
	}

	fn now_ms(&mut self) -> u64 {
		now_ms()
	}

	fn pause(&mut self) {
		sleep_until(clock() + 1);
	}
}

// A REGISTER INTERFACE IN A MAPPED MEMORY BAR.
struct Mmio {
	virt: u64,
	spacing: u64,
}

impl ipmi::Registers for Mmio {
	fn read(&mut self, index: u8) -> u8 {
		// SAFETY: inside the BAR window the claim mapped, at a register's offset.
		unsafe { core::ptr::read_volatile((self.virt + u64::from(index) * self.spacing) as *const u8) }
	}

	fn write(&mut self, index: u8, value: u8) {
		// SAFETY: as above.
		unsafe { core::ptr::write_volatile((self.virt + u64::from(index) * self.spacing) as *mut u8, value) };
	}

	fn now_ms(&mut self) -> u64 {
		now_ms()
	}

	fn pause(&mut self) {
		sleep_until(clock() + 1);
	}
}

// SSIF'S TWO SMBUS TRANSACTIONS, over the address-scoped connection: a NACK is the BMC not ready.
struct SmbusLink {
	smbus: i2c_client::Smbus<ChannelTransport>,
}

fn bus_error(error: i2c_client::SmbusError) -> ssif::BusError {
	match error {
		i2c_client::SmbusError::NoDevice => ssif::BusError::Nack,
		_ => ssif::BusError::Failed,
	}
}

impl ssif::Smbus for SmbusLink {
	fn block_write(&mut self, command: u8, data: &[u8]) -> Result<(), ssif::BusError> {
		self.smbus.block_write(command, data).map_err(bus_error)
	}

	fn block_read(&mut self, command: u8) -> Result<Vec<u8>, ssif::BusError> {
		self.smbus.block_read(command).map_err(bus_error)
	}

	fn now_ms(&mut self) -> u64 {
		now_ms()
	}

	fn pause(&mut self) {
		sleep_until(clock() + 1);
	}
}

fn smbus(chan: u64, pec: bool) -> Result<i2c_client::Smbus<ChannelTransport>, i2c_client::Refusal> {
	let needs = proto::system::I2cFunctionality { plain: false, max_transfer: 0, quick: false, byte: false, byte_data: false, word_data: false, block_write: true, block_read: true, i2c_block_read: false, pec };
	i2c_client::Smbus::new(i2c_device_proto::generated::liber::i2c_device::v1::i2c_device::Client::new(ChannelTransport { chan }), needs)
}

enum Interface {
	Kcs(Box<dyn ipmi::Registers>),
	Bt(bt::Bt, Box<dyn ipmi::Registers>),
	Ssif(ssif::Ssif, SmbusLink, u64),
}

impl Interface {
	fn kind(&self) -> bmc::InterfaceKind {
		match self {
			Interface::Kcs(_) => bmc::InterfaceKind::Kcs,
			Interface::Bt(..) => bmc::InterfaceKind::Bt,
			Interface::Ssif(..) => bmc::InterfaceKind::Ssif,
		}
	}

	fn transact(&mut self, request: &Request) -> Result<Response, Failure> {
		let deadline = now_ms() + ipmi::TRANSACTION_MS;
		match self {
			Interface::Kcs(registers) => kcs::transact(registers.as_mut(), request, deadline),
			Interface::Bt(state, registers) => state.transact(registers.as_mut(), request, deadline),
			Interface::Ssif(state, link, _) => state.transact(link, request, deadline),
		}
	}

	// THE INTERFACE MADE READY FOR ITS FIRST TRANSACTION: a KCS interface left busy aborted, BT's and SSIF's
	// capabilities read and put in force - PEC on SSIF only when both the BMC and the controller offer it.
	fn prepare(&mut self) -> Result<String, Failure> {
		let deadline = now_ms() + ipmi::TRANSACTION_MS;
		match self {
			Interface::Kcs(registers) => {
				if !kcs::idle(registers.as_mut()) {
					kcs::abort(registers.as_mut())?;
					return Ok(String::from("left busy - aborted"));
				}
				Ok(String::from("idle"))
			}
			Interface::Bt(state, registers) => {
				let found = state.read_capabilities(registers.as_mut(), deadline)?;
				Ok(format!("{} outstanding, buffers {} and {}", found.outstanding, found.input, found.output))
			}
			Interface::Ssif(state, link, chan) => {
				let found = state.read_capabilities(link, deadline)?;
				let mut pec = false;
				if found.pec
					&& let Ok(with) = smbus(*chan, true)
				{
					link.smbus = with;
					pec = true;
				}
				Ok(format!("{:?} parts, messages of {} and {} bytes, PEC {}", found.parts, found.input, found.output, if pec { "on" } else { "off" }))
			}
		}
	}
}

// ------------------------------------------------------------------ the driver's state

// A TEMPERATURE PUBLISHED AS A THERMAL ZONE: its record, and what was last published of it.
struct Zone {
	sensor: sdr::Sensor,
	published: Option<SourceState>,
}

// THE BMC WATCHDOG'S STATE: what bind found, and the bridge over a timer found running.
struct Wd {
	running_at_bind: bool,
	last_reset: bool,
	// Until when the driver itself pets a timer it found running, and the next pet.
	bridge_until: Option<u64>,
	next_pet: u64,
	// The countdown the timer runs with while it is armed, and None while it is not - what a sleep disarms and its
	// resume arms again.
	armed_ms: Option<u64>,
	// Disarmed for the sleep, with this countdown: armed again at the resume.
	slept_ms: Option<u64>,
}

struct Driver {
	name: String,
	binding: String,
	interface: Interface,
	availability: Availability,
	device: DeviceId,
	identity: Identity,
	// A NEW BMC ON THE SAME BINDING - a different identity after it came back - is a new generation: every preparation
	// made for the old one fails its revalidation.
	generation: u64,
	repository: ipmi_bmc::Repository,
	zones: Vec<Zone>,
	power_stream: u64,
	power_seq: u32,
	power_revision: u64,
	watchdog: Option<Wd>,
	sel_ops: Operations,
	sel_prepared: Vec<(u64, ipmi_bmc::SelClear)>,
	chassis_ops: Operations,
	chassis_prepared: Vec<(u64, ipmi::chassis::Control)>,
	serving: common::Serving,
	changes: u64,
	changes_seq: u32,
	// Serving a watchdog request now: the transactions it makes are not preceded by another.
	in_watchdog: bool,
	// The BMC answered after being unavailable: it is identified again once the request in hand is answered.
	back: bool,
	next_poll: u64,
	next_probe: u64,
	buf: Vec<u8>,
}

impl Driver {
	fn say(&self, text: &str) {
		let line = format!("{}: {text}\n", self.name);
		print(line.as_bytes());
	}

	fn selector(&self) -> Option<String> {
		ipmi_bmc::selector(&self.identity)
	}

	// A CHANGE TO THE BMC SERVICE'S STREAM, if it is open.
	fn change(&mut self, kind: bmc::ChangeKind) {
		if self.changes == 0 {
			return;
		}
		let item = bmc::Change { kind, name: self.identity.name() };
		let mut frame = [0u8; 512];
		let mut handles = Handles::new();
		if let Some(len) = bmc::ipmi_provider::changes_frame(self.changes_seq, &item, &mut frame, &mut handles) {
			match try_send_outcome(self.changes, &frame[..len], 0) {
				SendOutcome::Delivered => self.changes_seq = self.changes_seq.wrapping_add(1),
				SendOutcome::Stalled => self.say("a change was dropped: the stream is full"),
				SendOutcome::Failed => {
					close(self.changes);
					self.changes = 0;
				}
			}
		}
	}

	// THE PET FIRST: every watchdog request waiting, served before the transaction about to be made.
	fn watchdog_first(&mut self) {
		if self.watchdog.is_none() {
			return;
		}
		for index in 0..self.serving.as_slice().len() {
			if self.serving.token_at(index) != TOKEN_WATCHDOG {
				continue;
			}
			let channel = self.serving.at(index);
			let mut buf = [0u8; 256];
			let PolledCaps::Message { len, mut handles } = try_recv_caps(channel, &mut buf) else { continue };
			self.in_watchdog = true;
			let mut reply = [0u8; 256];
			let mut reply_handles = Handles::new();
			let written = watchdog::dispatch(&mut WatchdogView { driver: self }, &buf[..len], &mut handles, &mut reply, &mut reply_handles);
			self.in_watchdog = false;
			if let Some(written) = written {
				send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
			}
		}
	}

	// A BMC THAT ANSWERED AGAIN is identified again: a different one is a new generation, its repository read anew.
	fn came_back(&mut self) {
		self.say("the BMC answers again");
		let binding = self.binding.clone();
		match ipmi_bmc::identify(self, &binding) {
			Ok((device, identity)) if identity != self.identity => {
				self.say(&format!("it answers as another BMC ({}) - its repository is read again", identity.name()));
				self.device = device;
				self.identity = identity;
				self.generation += 1;
				self.read_repository();
				self.change(bmc::ChangeKind::Replaced);
			}
			_ => self.change(bmc::ChangeKind::Available),
		}
	}

	fn read_repository(&mut self) {
		self.repository = ipmi_bmc::read_repository(self);
		self.zones = self.repository.records.sensors().filter(|sensor| sensor.temperature()).take(MAX_ZONES).map(|sensor| Zone { sensor: sensor.clone(), published: None }).collect();
		let sensors = self.repository.records.sensors().count();
		self.say(&format!("its SDR repository holds {sensors} sensor(s), {} of them published as thermal zones{}{}", self.zones.len(), if self.repository.records.truncated { " - truncated at its bound" } else { "" }, if self.repository.records.refused > 0 { format!(", {} record(s) refused", self.repository.records.refused) } else { String::new() }));
	}

	// ------------------------------------------------------------ the thermal zones

	fn zone_state(&mut self, at: usize) -> SourceState {
		let sensor = self.zones[at].sensor.clone();
		let reading = if self.availability.available() { ipmi_bmc::read_sensor(self, &sensor).ok().map(|(reading, _)| reading) } else { None };
		let Some(temperature) = sensor.temperature_sensor(reading) else { return power_model::canon::unmeasured(proto::system::SourceKind::ThermalZone) };
		power_model::ipmi::thermal(&temperature)
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
				SendOutcome::Stalled => self.say("a thermal update was dropped: the stream is full"),
				SendOutcome::Failed => {
					close(self.power_stream);
					self.power_stream = 0;
				}
			}
		}
	}

	// EVERY ZONE READ AGAIN, and each one that changed published.
	fn poll_zones(&mut self) {
		for at in 0..self.zones.len() {
			let state = self.zone_state(at);
			if self.zones[at].published.as_ref() == Some(&state) {
				continue;
			}
			self.zones[at].published = Some(state.clone());
			self.power_revision += 1;
			let update = ProviderUpdate { revision: self.power_revision, kind: ProviderUpdateKind::Updated, source: Some(ProviderSource { local: at as u32, state }), gone: None };
			self.emit_power(&update);
		}
	}

	// ------------------------------------------------------------ the provider contract's answers

	fn status(answer: Answer) -> bmc::Status {
		match answer {
			Answer::Answered => bmc::Status { outcome: bmc::Outcome::Answered, cc: 0 },
			Answer::Refused(cc) => bmc::Status { outcome: bmc::Outcome::Refused, cc },
			Answer::Unavailable => bmc::Status { outcome: bmc::Outcome::Unavailable, cc: 0 },
			Answer::Malformed => bmc::Status { outcome: bmc::Outcome::Malformed, cc: 0 },
		}
	}

	fn binding_info(&self) -> bmc::BindingInfo {
		let device = &self.device;
		bmc::BindingInfo { name: self.identity.name(), guid: matches!(self.identity, Identity::Guid(_)), administrable: self.selector().is_some(), interface: self.interface.kind(), binding: self.binding.clone(), available: self.availability.available(), device: bmc::Device { device_id: device.device_id, device_revision: device.device_revision, firmware_major: device.firmware_major, firmware_minor: device.firmware_minor, ipmi_major: device.ipmi.0, ipmi_minor: device.ipmi.1, manufacturer: device.manufacturer, product: device.product, support: device.support }, sensor_count: self.repository.records.sensors().count() as u16, sdr_truncated: self.repository.records.truncated, watchdog: self.watchdog.is_some() }
	}

	fn sensor(&mut self, sensor: &sdr::Sensor) -> bmc::Sensor {
		let milli = |raw: Option<u8>| raw.and_then(|raw| sensor.linear.and_then(|linear| linear.value(raw, 3).known()));
		let mut out = bmc::Sensor {
			number: sensor.number,
			lun: sensor.lun,
			name: sensor.name.chars().take(32).collect(),
			kind: match sensor.kind {
				sdr::Kind::Full => bmc::RecordKind::Full,
				sdr::Kind::Compact => bmc::RecordKind::Compact,
				sdr::Kind::EventOnly => bmc::RecordKind::EventOnly,
			},
			sensor_type: sensor.sensor_type,
			event_type: sensor.event_type,
			entity_id: sensor.entity.0,
			entity_instance: sensor.entity.1,
			units: sensor.units.0,
			base_unit: sensor.units.1,
			modifier_unit: sensor.units.2,
			state: bmc::ReadingState::Unknown,
			raw: 0,
			value: 0,
			bits: 0,
			upper_non_recoverable: milli(sensor.thresholds.unr()),
			upper_critical: milli(sensor.thresholds.uc()),
			upper_non_critical: milli(sensor.thresholds.unc()),
			lower_non_critical: milli(sensor.thresholds.lnc()),
			lower_critical: milli(sensor.thresholds.lc()),
			lower_non_recoverable: milli(sensor.thresholds.lnr()),
		};
		if sensor.kind == sdr::Kind::EventOnly {
			out.state = bmc::ReadingState::EventOnly;
			return out;
		}
		if !sensor.readable() {
			out.state = bmc::ReadingState::NotOwned;
			return out;
		}
		let Ok((reading, bits)) = ipmi_bmc::read_sensor(self, sensor) else { return out };
		out.raw = reading.raw;
		out.bits = bits;
		out.state = match sensor.value(&reading) {
			Tagged::Known(value) => {
				out.value = value;
				bmc::ReadingState::Known
			}
			Tagged::Unknown => bmc::ReadingState::Unknown,
			// A DISCRETE SENSOR has no value, and its state bits are what it read.
			Tagged::Unsupported => bmc::ReadingState::Unsupported,
			Tagged::Invalid(_) => bmc::ReadingState::Invalid,
		};
		out
	}

	fn sel_entry(record: &ipmi::sel::Record) -> bmc::SelEntry {
		let ours = match ipmi::event::ours(record) {
			Some(ipmi::event::Ours::Boot) => bmc::Ours::Boot,
			Some(ipmi::event::Ours::Shutdown) => bmc::Ours::Shutdown,
			None => bmc::Ours::None,
		};
		match record {
			ipmi::sel::Record::System { id, timestamp, generator, sensor_type, sensor, event_type, deassertion, data, .. } => bmc::SelEntry { id: *id, record_type: 0x02, timestamp: *timestamp, generator: *generator, sensor_type: *sensor_type, sensor: *sensor, event_type: *event_type, deassertion: *deassertion, data: data.to_vec().into(), ours },
			ipmi::sel::Record::OemTimestamped { id, record_type, timestamp, data, .. } => bmc::SelEntry { id: *id, record_type: *record_type, timestamp: *timestamp, generator: 0, sensor_type: 0, sensor: 0, event_type: 0, deassertion: false, data: data.clone().into(), ours },
			ipmi::sel::Record::Oem { id, record_type, data } => bmc::SelEntry { id: *id, record_type: *record_type, timestamp: 0, generator: 0, sensor_type: 0, sensor: 0, event_type: 0, deassertion: false, data: data.clone().into(), ours },
		}
	}

	// THE FRU DEVICES the locator records name - and the BMC's own, device 0, when it reports FRU inventory and no
	// locator names it - at most eight.
	fn fru_devices(&self) -> Vec<(u8, String)> {
		let mut devices: Vec<(u8, String)> = self.repository.records.fru_devices().map(|locator| (locator.device, locator.name.clone())).collect();
		if self.device.support & ipmi::identity::support::FRU != 0 && !devices.iter().any(|(device, _)| *device == 0) {
			devices.insert(0, (0, String::from("BMC")));
		}
		devices.truncate(ipmi::fru::MAX_DEVICES);
		devices
	}
}

impl Bmc for Driver {
	fn ask(&mut self, request: &Request) -> Result<Response, Failure> {
		if !self.in_watchdog {
			self.watchdog_first();
		}
		let outcome = self.interface.transact(request);
		// AN ANSWER REFUSED IS SAID: past the bound, not the request's, or breaking the interface's protocol.
		if let Err(failure @ (Failure::TooLong | Failure::Mismatch | Failure::Protocol(_) | Failure::Interface(_))) = &outcome {
			self.say(&format!("an answer to netfn {:#04x} command {:#04x} was refused - {failure:?}", request.netfn, request.cmd));
		}
		match self.availability.observe(&outcome) {
			Change::Lost => {
				self.say("the BMC is unavailable - three transactions in a row went unanswered; its thermal zones are unknown until it answers");
				self.next_probe = clock() + PROBE_TICKS;
				self.change(bmc::ChangeKind::Unavailable);
			}
			Change::Back => self.back = true,
			Change::None => {}
		}
		outcome
	}

	fn now_ms(&mut self) -> u64 {
		now_ms()
	}

	fn pause(&mut self) {
		sleep_until(clock() + 1);
	}
}

// ------------------------------------------------------------------ the watchdog

struct WatchdogView<'a> {
	driver: &'a mut Driver,
}

fn bmc_error(answer: Answer) -> Error {
	match answer {
		Answer::Unavailable => Error::TimedOut,
		Answer::Refused(_) | Answer::Malformed => Error::Io,
		Answer::Answered => Error::Io,
	}
}

impl watchdog::Service for WatchdogView<'_> {
	fn describe(&mut self) -> Result<WatchdogDescription, Error> {
		let wd = self.driver.watchdog.as_ref().ok_or(Error::Unsupported)?;
		Ok(WatchdogDescription { device: String::from("bmc"), min_timeout_ms: ipmi::watchdog::MIN_TIMEOUT_MS as u32, max_timeout_ms: ipmi::watchdog::MAX_TIMEOUT_MS as u32, granularity_ms: 100, can_disarm: true, survives_reset: true, stops_in_suspend_to_idle: false, stops_in_s3: false, running_at_bind: wd.running_at_bind, last_reset_was_watchdog: wd.last_reset })
	}

	// ARMED WITH ITS TIMEOUT, THEN STARTED: Set Watchdog Timer and Reset Watchdog Timer. Below the minimum is refused.
	fn arm(&mut self, timeout_ms: u32) -> Result<u32, Error> {
		let effective = ipmi::watchdog::effective_ms(u64::from(timeout_ms)).ok_or(Error::Invalid)?;
		let request = ipmi::watchdog::arm(effective, 0).ok_or(Error::Invalid)?;
		ipmi_bmc::data(self.driver, &request).map_err(bmc_error)?;
		ipmi_bmc::data(self.driver, &ipmi::watchdog::pet()).map_err(bmc_error)?;
		if let Some(wd) = self.driver.watchdog.as_mut() {
			wd.bridge_until = None;
			wd.armed_ms = Some(effective);
		}
		Ok(effective as u32)
	}

	fn pet(&mut self) -> Result<(), Error> {
		if let Some(wd) = self.driver.watchdog.as_mut() {
			wd.bridge_until = None;
		}
		ipmi_bmc::data(self.driver, &ipmi::watchdog::pet()).map(|_| ()).map_err(bmc_error)
	}

	fn disarm(&mut self) -> Result<(), Error> {
		ipmi_bmc::data(self.driver, &ipmi::watchdog::disarm(0)).map_err(bmc_error)?;
		if let Some(wd) = self.driver.watchdog.as_mut() {
			wd.bridge_until = None;
			wd.armed_ms = None;
		}
		Ok(())
	}
}

// ------------------------------------------------------------------ the sleep

impl common::SleepStep for Driver {
	fn suspend(&mut self, _request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		let armed = self.watchdog.as_ref().and_then(|wd| wd.armed_ms);
		if let Some(timeout) = armed {
			self.in_watchdog = true;
			let disarmed = ipmi_bmc::data(self, &ipmi::watchdog::disarm(0));
			self.in_watchdog = false;
			if disarmed.is_err() {
				self.say("the sleep is refused: the BMC's watchdog did not take the disarm, and a timer left counting would reset the machine in its sleep");
				return driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Refused(driver_protocol::DriverFailureCode::DeviceNotResponding), awake_by_ms: 0 };
			}
			if let Some(wd) = self.watchdog.as_mut() {
				wd.slept_ms = Some(timeout);
				wd.bridge_until = None;
			}
			self.say(&format!("suspended - the BMC's watchdog is disarmed for the sleep (it ran with {timeout} ms)"));
		} else {
			self.say("suspended - the queue is held until the resume");
		}
		driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
	}

	fn resume(&mut self, _lost_power: bool) -> bool {
		match self.interface.prepare() {
			Ok(found) => self.say(&format!("resumed - the interface is {found}")),
			Err(failure) => self.say(&format!("resumed - the interface did not prepare ({failure:?}); the BMC is asked again as it answers")),
		}
		// THE WATCHDOG FIRST, with the countdown it had but at most the bridge bound: what it had is the longest the watchdog
		// service set at the announcement, and a resume that hangs after the drivers is caught within the bound instead of
		// an hour and a half later. The service's resume notice restores its configured timeout.
		if let Some(timeout) = self.watchdog.as_mut().and_then(|wd| wd.slept_ms.take()) {
			self.in_watchdog = true;
			let timeout = timeout.min(u64::from(drivers::watchdog::RESUME_WATCH_MS));
			let armed = ipmi::watchdog::arm(timeout, 0).map(|request| ipmi_bmc::data(self, &request).is_ok() && ipmi_bmc::data(self, &ipmi::watchdog::pet()).is_ok());
			self.in_watchdog = false;
			self.say(if armed == Some(true) { "the BMC's watchdog is armed again" } else { "the BMC's watchdog did not take its arm again" });
		}
		self.came_back();
		true
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(&mut self.serving)
	}
}

// ------------------------------------------------------------------ the provider contract

struct ProviderView<'a> {
	driver: &'a mut Driver,
}

impl bmc::ipmi_provider::Service for ProviderView<'_> {
	fn describe(&mut self) -> Result<bmc::BindingInfo, Error> {
		Ok(self.driver.binding_info())
	}

	fn changes(&mut self) -> Vec<bmc::Change> {
		Vec::new()
	}

	fn sensors(&mut self) -> Result<bmc::Sensors, Error> {
		let records: Vec<sdr::Sensor> = self.driver.repository.records.sensors().cloned().collect();
		let mut sensors = Vec::with_capacity(records.len());
		for sensor in &records {
			sensors.push(self.driver.sensor(sensor));
		}
		let answer = if self.driver.availability.available() { self.driver.repository.answer.unwrap_or(Answer::Answered) } else { Answer::Unavailable };
		Ok(bmc::Sensors { status: Driver::status(answer), sensors: sensors.into(), truncated: self.driver.repository.records.truncated, refused: self.driver.repository.records.refused })
	}

	fn sel_info(&mut self) -> Result<bmc::SelInfo, Error> {
		Ok(match ipmi_bmc::sel_info(self.driver) {
			Ok(info) => bmc::SelInfo { status: Driver::status(Answer::Answered), version: info.version, entries: info.entries, free: info.free, last_addition: info.last_addition, last_erase: info.last_erase, overflow: info.overflow, reserve: info.reserve },
			Err(answer) => bmc::SelInfo { status: Driver::status(answer), version: 0, entries: 0, free: 0, last_addition: 0, last_erase: 0, overflow: false, reserve: false },
		})
	}

	fn sel_page(&mut self, first: u16) -> Result<bmc::SelPage, Error> {
		Ok(match ipmi_bmc::sel_page(self.driver, first) {
			Ok((records, next, refused)) => bmc::SelPage { status: Driver::status(Answer::Answered), entries: records.iter().map(Driver::sel_entry).collect::<Vec<_>>().into(), next, refused: refused.iter().map(|refusal| bmc::SelRefused { id: refusal.id, record_type: refusal.record_type }).collect::<Vec<_>>().into() },
			Err(answer) => bmc::SelPage { status: Driver::status(answer), entries: Vec::new().into(), next: ipmi::sel::LAST, refused: Vec::new().into() },
		})
	}

	fn fru(&mut self) -> Result<bmc::Fru, Error> {
		let mut devices = Vec::new();
		let mut answer = Answer::Answered;
		for (device, name) in self.driver.fru_devices() {
			let text = |field: &Option<ipmi::fru::Field>| field.as_ref().map(|field| field.text().chars().take(64).collect::<String>());
			let mut out = bmc::FruDevice { device, name: name.chars().take(32).collect(), status: Driver::status(Answer::Answered), chassis_type: 0, chassis_part: None, chassis_serial: None, manufactured: 0, board_manufacturer: None, board_product: None, board_serial: None, board_part: None, product_manufacturer: None, product_name: None, product_part: None, product_version: None, product_serial: None, product_asset: None, multirecord: false, refused: Vec::new().into() };
			match ipmi_bmc::fru_bytes(self.driver, device) {
				Ok(bytes) => match ipmi::fru::inventory(&bytes) {
					Ok(inventory) => {
						if let Some(chassis) = &inventory.chassis {
							out.chassis_type = chassis.chassis_type;
							out.chassis_part = text(&chassis.part);
							out.chassis_serial = text(&chassis.serial);
						}
						if let Some(board) = &inventory.board {
							out.manufactured = board.manufactured;
							out.board_manufacturer = text(&board.manufacturer);
							out.board_product = text(&board.product);
							out.board_serial = text(&board.serial);
							out.board_part = text(&board.part);
						}
						if let Some(product) = &inventory.product {
							out.product_manufacturer = text(&product.manufacturer);
							out.product_name = text(&product.name);
							out.product_part = text(&product.part);
							out.product_version = text(&product.version);
							out.product_serial = text(&product.serial);
							out.product_asset = text(&product.asset);
						}
						out.multirecord = inventory.multirecord;
						out.refused = inventory.refused.iter().take(4).map(|refusal| format!("{refusal:?}")).collect::<Vec<_>>().into();
					}
					Err(refusal) => {
						out.status = Driver::status(Answer::Malformed);
						out.refused = alloc::vec![format!("{refusal:?}")].into();
					}
				},
				Err(found) => {
					out.status = Driver::status(found);
					if found == Answer::Unavailable {
						answer = found;
					}
				}
			}
			devices.push(out);
		}
		Ok(bmc::Fru { status: Driver::status(answer), devices: devices.into() })
	}

	fn chassis(&mut self) -> Result<bmc::Chassis, Error> {
		let empty = |status| bmc::Chassis { status, power_on: false, overload: false, interlock: false, power_fault: false, control_fault: false, restore_policy: 0, last_event: 0, intrusion: false, drive_fault: false, cooling_fault: false, identify: None };
		Ok(match ipmi_bmc::data(self.driver, &ipmi::chassis::status_request()) {
			Ok(bytes) => match ipmi::chassis::status(&bytes) {
				Some(found) => bmc::Chassis { status: Driver::status(Answer::Answered), power_on: found.power_on, overload: found.overload, interlock: found.interlock, power_fault: found.power_fault, control_fault: found.control_fault, restore_policy: found.restore_policy, last_event: found.last_event, intrusion: found.intrusion, drive_fault: found.drive_fault, cooling_fault: found.cooling_fault, identify: found.identify },
				None => empty(Driver::status(Answer::Malformed)),
			},
			Err(answer) => empty(Driver::status(answer)),
		})
	}

	fn identify(&mut self, seconds: u8) -> Result<bmc::Status, Error> {
		let answer = match ipmi_bmc::data(self.driver, &ipmi::chassis::identify(seconds)) {
			Ok(_) => Answer::Answered,
			Err(answer) => answer,
		};
		self.driver.say(&format!("identify {} - {answer:?}", if seconds == 0 { String::from("off") } else { format!("on for {seconds} s") }));
		Ok(Driver::status(answer))
	}

	fn lan(&mut self) -> Result<bmc::Lan, Error> {
		Ok(match ipmi_bmc::lan_channels(self.driver) {
			Ok(channels) => bmc::Lan { status: Driver::status(Answer::Answered), channels: channels.into_iter().map(|(lan, users)| bmc::LanChannel { channel: lan.channel, source: lan.source, address: lan.address.map(|bytes| bytes.to_vec()).unwrap_or_default().into(), mask: lan.mask.map(|bytes| bytes.to_vec()).unwrap_or_default().into(), gateway: lan.gateway.map(|bytes| bytes.to_vec()).unwrap_or_default().into(), mac: lan.mac.map(|bytes| bytes.to_vec()).unwrap_or_default().into(), vlan: lan.vlan.flatten(), users: users.into_iter().map(|user| bmc::LanUser { id: user.id, name: user.name.chars().take(16).collect(), enabled: user.enabled, privilege: user.privilege }).collect::<Vec<_>>().into() }).collect::<Vec<_>>().into() },
			Err(answer) => bmc::Lan { status: Driver::status(answer), channels: Vec::new().into() },
		})
	}

	// THE SYSTEM'S OWN EVENTS, built here: generator 0x41, OS Boot or OS Stop/Shutdown.
	fn system_event(&mut self, kind: bmc::SystemEventKind) -> Result<bmc::Status, Error> {
		let (request, what) = match kind {
			bmc::SystemEventKind::BootCompleted => (ipmi::event::boot_completed(), "boot completed"),
			bmc::SystemEventKind::GracefulShutdown => (ipmi::event::graceful_shutdown(), "an orderly shutdown"),
		};
		let answer = match ipmi_bmc::data(self.driver, &request) {
			Ok(_) => Answer::Answered,
			Err(answer) => answer,
		};
		self.driver.say(&format!("the system's event '{what}' was sent to the BMC's log - {answer:?}"));
		Ok(Driver::status(answer))
	}
}

// ------------------------------------------------------------------ the power sources

struct PowerView<'a> {
	driver: &'a mut Driver,
}

impl power_provider::Service for PowerView<'_> {
	fn updates(&mut self) -> Vec<ProviderUpdate> {
		let revision = self.driver.power_revision;
		let mut updates = Vec::new();
		for at in 0..self.driver.zones.len() {
			let state = match self.driver.zones[at].published.clone() {
				Some(state) => state,
				None => {
					let state = self.driver.zone_state(at);
					self.driver.zones[at].published = Some(state.clone());
					state
				}
			};
			updates.push(ProviderUpdate { revision, kind: ProviderUpdateKind::Snapshot, source: Some(ProviderSource { local: at as u32, state }), gone: None });
		}
		updates.push(ProviderUpdate { revision, kind: ProviderUpdateKind::SnapshotEnd, source: None, gone: None });
		updates
	}

	// READ-ONLY: a temperature advertises no control.
	fn command(&mut self, _command: ProviderCommand) -> Result<ControlOutcome, Error> {
		Err(Error::Unsupported)
	}

	fn query(&mut self, local: u32) -> Result<SourceState, Error> {
		let at = local as usize;
		if at >= self.driver.zones.len() {
			return Err(Error::NotFound);
		}
		Ok(self.driver.zone_state(at))
	}
}

// ------------------------------------------------------------------ the administrative executors

fn refused(refusal: OperationRefusal) -> Error {
	match refusal {
		OperationRefusal::Bounds => Error::Invalid,
		OperationRefusal::NotFound => Error::NotFound,
		OperationRefusal::Busy => Error::Again,
		OperationRefusal::Stale | OperationRefusal::Expired => Error::Stale,
		OperationRefusal::Cancelled | OperationRefusal::Started => Error::Denied,
	}
}

fn prepare_refused(refusal: ipmi_bmc::Refusal) -> Error {
	match refusal {
		ipmi_bmc::Refusal::Payload | ipmi_bmc::Refusal::Parameters | ipmi_bmc::Refusal::Count => Error::Invalid,
		ipmi_bmc::Refusal::NoReserve | ipmi_bmc::Refusal::Operation => Error::Unsupported,
		ipmi_bmc::Refusal::Bmc(answer) => bmc_error(answer),
	}
}

// The requester's bytes, copied out of its object: exactly `length` of them, at most the bound.
fn copy_payload(payload: u64, length: u32) -> Option<Vec<u8>> {
	let length = length as usize;
	if length == 0 || length > MAX_PAYLOAD || object_info(payload).is_none_or(|info| (info.size as usize) < length) {
		return None;
	}
	let addr = unsafe { map_object(payload) }?;
	let copied = unsafe { core::slice::from_raw_parts(addr as *const u8, length) }.to_vec();
	unmap_object(payload);
	Some(copied)
}

struct ExecutorView<'a> {
	driver: &'a mut Driver,
	action: AdminAction,
}

impl admin_executor::Service for ExecutorView<'_> {
	fn prepare(&mut self, action: AdminAction, target: String, parameters: Vec<u8>, payload_length: u32, payload: u64) -> Result<AdminPrepared, Error> {
		let copied = copy_payload(payload, payload_length);
		close(payload);
		if action != self.action {
			return Err(Error::Unsupported);
		}
		// A REQUEST NAMING ANOTHER BMC - or a BMC whose name is past the selector's bound - is not this executor's.
		let Some(selector) = self.driver.selector().filter(|selector| *selector == target) else { return Err(Error::NotFound) };
		let copied = copied.ok_or(Error::Invalid)?;
		let generation = self.driver.generation;
		let now = clock();
		let (executor, deadline_ms, prepared) = match action {
			AdminAction::BmcSelClear => {
				let clear = ipmi_bmc::prepare_sel_clear(self.driver, selector.as_bytes(), &parameters, &copied).map_err(prepare_refused)?;
				let prepared = self.driver.sel_ops.prepare(generation, &parameters, &copied, now, LIFETIME_TICKS).map_err(refused)?.clone();
				self.driver.sel_prepared.retain(|(operation, _)| *operation != prepared.operation);
				self.driver.sel_prepared.push((prepared.operation, clear));
				self.driver.say(&format!("a clear of its {} SEL record(s) is prepared under reservation {:#06x}", clear.count, clear.reservation));
				(driver_protocol::provider::BMC_SEL_NAME, SEL_CLEAR_MS, prepared)
			}
			AdminAction::BmcChassisControl => {
				let operation = ipmi_bmc::prepare_chassis(selector.as_bytes(), &parameters, &copied).map_err(prepare_refused)?;
				let prepared = self.driver.chassis_ops.prepare(generation, &parameters, &copied, now, LIFETIME_TICKS).map_err(refused)?.clone();
				self.driver.chassis_prepared.retain(|(held, _)| *held != prepared.operation);
				self.driver.chassis_prepared.push((prepared.operation, operation));
				self.driver.say(&format!("a chassis {operation:?} is prepared"));
				(driver_protocol::provider::BMC_CHASSIS_NAME, CHASSIS_MS, prepared)
			}
			_ => return Err(Error::Unsupported),
		};
		let epoch = if action == AdminAction::BmcSelClear { self.driver.sel_ops.epoch } else { self.driver.chassis_ops.epoch };
		let descriptor = AdminDescriptor { version: 1, action, executor: String::from_utf8_lossy(executor).into_owned(), executor_epoch: epoch, target: selector, target_generation: prepared.generation, parameters: prepared.parameters.clone(), payload_length, payload_digest: prepared.digest.to_vec() };
		Ok(AdminPrepared { operation: prepared.operation, descriptor, deadline_ms })
	}

	fn revalidate(&mut self, operation: u64) -> Result<(), Error> {
		let generation = self.driver.generation;
		let operations = if self.action == AdminAction::BmcSelClear { &self.driver.sel_ops } else { &self.driver.chassis_ops };
		operations.revalidate(operation, generation, clock()).map_err(refused)
	}

	fn execute(&mut self, operation: u64, epoch: u64) -> Result<AdminResult, Error> {
		let generation = self.driver.generation;
		let outcome = match self.action {
			AdminAction::BmcSelClear => {
				self.driver.sel_ops.start(operation, epoch, generation, clock()).map_err(refused)?;
				let Some(&(_, clear)) = self.driver.sel_prepared.iter().find(|(held, _)| *held == operation) else { return Err(Error::NotFound) };
				let outcome = ipmi_bmc::execute_sel_clear(self.driver, &clear, now_ms() + u64::from(SEL_CLEAR_MS));
				self.driver.say(&format!(
					"the SEL clear under reservation {:#06x} - {}",
					clear.reservation,
					match outcome {
						Outcome::Completed => "completed: the log is erased",
						Outcome::Failed => "failed: the BMC refused it, and nothing was erased",
						Outcome::Unknown => "its end was not observed",
					}
				));
				outcome
			}
			AdminAction::BmcChassisControl => {
				self.driver.chassis_ops.start(operation, epoch, generation, clock()).map_err(refused)?;
				let Some(&(_, control)) = self.driver.chassis_prepared.iter().find(|(held, _)| *held == operation) else { return Err(Error::NotFound) };
				self.driver.say(&format!("the chassis {control:?} is sent to the BMC"));
				let (outcome, cc) = ipmi_bmc::execute_chassis(self.driver, control);
				if let Some(cc) = cc {
					self.driver.say(&format!("the BMC refused the chassis {control:?} with {cc:#04x}"));
				}
				outcome
			}
			_ => return Err(Error::Unsupported),
		};
		Ok(match outcome {
			Outcome::Completed => AdminResult::Completed,
			Outcome::Failed => AdminResult::Failed,
			Outcome::Unknown => AdminResult::OutcomeUnknown,
		})
	}

	fn cancel(&mut self, operation: u64) -> Result<(), Error> {
		let operations = if self.action == AdminAction::BmcSelClear { &mut self.driver.sel_ops } else { &mut self.driver.chassis_ops };
		operations.cancel(operation).map_err(refused)
	}

	// A BMC'S ACTIONS READ NOTHING OUT.
	fn execute_read(&mut self, _operation: u64, _epoch: u64) -> Result<AdminRead, Error> {
		Err(Error::Unsupported)
	}
}

// ------------------------------------------------------------------ serving

// ONE REQUEST on the endpoint at `index`; false when its consumer has gone.
fn serve(driver: &mut Driver, index: usize) -> bool {
	let channel = driver.serving.at(index);
	let token = driver.serving.token_at(index);
	let mut buf = core::mem::take(&mut driver.buf);
	let polled = try_recv_caps(channel, &mut buf);
	let (len, mut handles) = match polled {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => {
			driver.buf = buf;
			return true;
		}
		PolledCaps::Closed => {
			driver.buf = buf;
			return false;
		}
	};
	let request = buf[..len].to_vec();
	driver.buf = buf;
	let op = if request.len() >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
	// THE TWO STREAMS: the BMC service's changes, PowerService's updates.
	if token == TOKEN_IPMI && op == bmc::ipmi_provider::OP_CHANGES {
		let Some((corr, _)) = bmc::ipmi_provider::changes_open(&mut ProviderView { driver: &mut *driver }, &request, &mut handles) else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		if driver.changes != 0 {
			close(driver.changes);
		}
		driver.changes = producer;
		driver.changes_seq = 0;
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		return true;
	}
	if token == TOKEN_POWER && op == power_provider::OP_UPDATES {
		let Some((corr, items)) = power_provider::updates_open(&mut PowerView { driver: &mut *driver }, &request, &mut handles) else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		if driver.power_stream != 0 {
			close(driver.power_stream);
		}
		driver.power_stream = producer;
		driver.power_seq = 0;
		for item in &items {
			driver.emit_power(item);
		}
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		return true;
	}
	let mut reply = alloc::vec![0u8; 65536];
	let mut reply_handles = Handles::new();
	let written = match token {
		TOKEN_IPMI => bmc::ipmi_provider::dispatch(&mut ProviderView { driver: &mut *driver }, &request, &mut handles, &mut reply, &mut reply_handles),
		TOKEN_POWER => power_provider::dispatch(&mut PowerView { driver: &mut *driver }, &request, &mut handles, &mut reply, &mut reply_handles),
		TOKEN_WATCHDOG => {
			driver.in_watchdog = true;
			let written = watchdog::dispatch(&mut WatchdogView { driver: &mut *driver }, &request, &mut handles, &mut reply, &mut reply_handles);
			driver.in_watchdog = false;
			written
		}
		TOKEN_SEL => admin_executor::dispatch(&mut ExecutorView { driver: &mut *driver, action: AdminAction::BmcSelClear }, &request, &mut handles, &mut reply, &mut reply_handles),
		TOKEN_CHASSIS => admin_executor::dispatch(&mut ExecutorView { driver: &mut *driver, action: AdminAction::BmcChassisControl }, &request, &mut handles, &mut reply, &mut reply_handles),
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
		// A REQUEST THIS PUBLICATION DOES NOT CARRY ends that connection, and only that one.
		None => false,
	}
}

// ------------------------------------------------------------------ bind

#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
	Pci,
	Isa,
	Smbus,
}

fn binding_of(bind: &common::Bind) -> String {
	if bind.info.transport == TRANSPORT_PLATFORM { String::from_utf8_lossy(bind.info.platform.identity()).into_owned() } else { format!("pci:0000:{:02x}:{:02x}.{}", bind.info.bus, bind.info.dev, bind.info.func) }
}

// A namespace method's integer, through the node channel.
fn node_integer(node: u64, method: &str) -> Option<u64> {
	match acpi_node::Client::with_deadline(ChannelTransport { chan: node }, clock() + NODE_TICKS).evaluate(method, &[]) {
		Some(Ok(bytes)) => match aml::wire::decode(&bytes) {
			Ok(aml::wire::Value::Integer(value)) => Some(value),
			_ => None,
		},
		_ => None,
	}
}

// THE REGISTERS OF A PORT-RANGE FORM: its range mapped, the spacing its length gives over the interface's registers.
#[cfg(target_arch = "x86_64")]
fn port_registers(bind: &common::Bind, resources: &common::Resources, registers: u16, source: u8) -> Option<Box<dyn ipmi::Registers>> {
	let at = (0..bind.info.port_count as usize).find(|&at| bind.info.ports[at].source == source && (source != PORT_SOURCE_IO_BAR || bind.info.ports[at].index == 0))?;
	if at >= resources.port_range_count || port_range_map(resources.port_ranges[at]) < 0 {
		return None;
	}
	let port = bind.info.ports[at];
	// PCI FORMS ARE SPACED ONE APART, as QEMU and Linux have them; an ISA range's spacing is its length over the
	// interface's register count.
	let spacing = if source == PORT_SOURCE_IO_BAR { 1 } else { (port.len / registers).max(1) };
	Some(Box::new(Ports { base: port.base, spacing }))
}

#[cfg(not(target_arch = "x86_64"))]
fn port_registers(_bind: &common::Bind, _resources: &common::Resources, _registers: u16, _source: u8) -> Option<Box<dyn ipmi::Registers>> {
	None
}

// A MEMORY BAR 0, the resolved window the claim mapped.
fn mmio_registers(resources: &common::Resources) -> Option<Box<dyn ipmi::Registers>> {
	if resources.device == 0 {
		return None;
	}
	let virt = unsafe { syscall(SYS_DEVICE_MEMORY_MAP, resources.device, 0, 0, 0) };
	((virt as i64) > 0).then(|| Box::new(Mmio { virt, spacing: 1 }) as Box<dyn ipmi::Registers>)
}

fn fail(bootstrap: u64, bind: &common::Bind, name: &str, why: &str) -> ! {
	let line = format!("{name}: {why}\n");
	print(line.as_bytes());
	common::failed(bootstrap, bind, driver_protocol::DriverFailureCode::ResourceUnusable)
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	let binding = binding_of(&bind);
	let name = format!("driver.ipmi: {binding}");
	let form = if bind.info.transport != TRANSPORT_PLATFORM {
		Form::Pci
	} else if bind.info.platform.connections().iter().any(|connection| connection.kind == CONNECTION_I2C) {
		Form::Smbus
	} else {
		Form::Isa
	};
	// ONLINE FIRST, publishing nothing yet.
	let report = format!("{name}: online");
	if !common::online(bootstrap, &bind, report.as_bytes(), &[]) {
		exit();
	}
	// A NAMESPACE DEVICE SAYS WHICH INTERFACE IT IS: `_IFT` (1 KCS, 3 BT, 4 SSIF; 2, SMIC, refused), and `_SRV` the
	// specification revision.
	let interface_type = match form {
		Form::Pci => match bind.info.prog_if {
			PCI_PROG_IF_IPMI_KCS => 1,
			PCI_PROG_IF_IPMI_BT => 3,
			_ => fail(bootstrap, &bind, &name, "an IPMI function of an interface this driver does not speak"),
		},
		Form::Isa | Form::Smbus => {
			let node = if common::request_node(bootstrap, &bind) && common::wait_node_or_answer(bootstrap, &bind, &[]).is_some() { common::node().unwrap_or(0) } else { 0 };
			if common::stop_requested() {
				common::finish_stop(bootstrap, &bind, 0, true);
			}
			if node == 0 {
				fail(bootstrap, &bind, &name, "its firmware describes no node for it");
			}
			let kind = node_integer(node, "_IFT").unwrap_or(0);
			let revision = node_integer(node, "_SRV").unwrap_or(0);
			print(format!("{name}: _IFT {kind}, _SRV {revision:#06x}\n").as_bytes());
			kind
		}
	};
	let interface = match (form, interface_type) {
		(Form::Smbus, 4) | (Form::Isa, 4) => {
			let chan = bind.info.platform.connections().iter().enumerate().find(|(_, connection)| connection.kind == CONNECTION_I2C).map(|(at, _)| at).filter(|&at| at < resources.connection_count).map(|at| resources.connections[at]).unwrap_or(0);
			if chan == 0 {
				fail(bootstrap, &bind, &name, "an SSIF interface with no SMBus address to reach it through");
			}
			match smbus(chan, false) {
				Ok(link) => Interface::Ssif(ssif::Ssif::default(), SmbusLink { smbus: link }, chan),
				Err(why) => fail(bootstrap, &bind, &name, &format!("its SMBus connection cannot carry SSIF - {why:?}")),
			}
		}
		(Form::Pci, 1) | (Form::Isa, 1) => {
			let registers = port_registers(&bind, &resources, 2, if matches!(form, Form::Pci) { PORT_SOURCE_IO_BAR } else { PORT_SOURCE_PLATFORM }).or_else(|| mmio_registers(&resources));
			let Some(registers) = registers else { fail(bootstrap, &bind, &name, "its KCS registers could not be reached") };
			Interface::Kcs(registers)
		}
		(Form::Pci, 3) | (Form::Isa, 3) => {
			let registers = port_registers(&bind, &resources, 3, if matches!(form, Form::Pci) { PORT_SOURCE_IO_BAR } else { PORT_SOURCE_PLATFORM }).or_else(|| mmio_registers(&resources));
			let Some(registers) = registers else { fail(bootstrap, &bind, &name, "its BT registers could not be reached") };
			Interface::Bt(bt::Bt::default(), registers)
		}
		(_, 2) => fail(bootstrap, &bind, &name, "an SMIC interface, which this driver does not speak"),
		(_, kind) => fail(bootstrap, &bind, &name, &format!("an interface type {kind} this driver does not know")),
	};
	let ssif_form = matches!(interface, Interface::Ssif(..));
	let mut driver = Driver { name: name.clone(), binding: binding.clone(), interface, availability: Availability::default(), device: DeviceId::default(), identity: Identity::Fallback { manufacturer: 0, product: 0, device_id: 0, binding: binding.clone() }, generation: 1, repository: ipmi_bmc::Repository::default(), zones: Vec::new(), power_stream: 0, power_seq: 0, power_revision: 1, watchdog: None, sel_ops: Operations::new(bind.generation), sel_prepared: Vec::new(), chassis_ops: Operations::new(bind.generation), chassis_prepared: Vec::new(), serving: common::Serving::from_offers(&[]), changes: 0, changes_seq: 0, in_watchdog: false, back: false, next_poll: 0, next_probe: 0, buf: alloc::vec![0u8; 4096] };
	match driver.interface.prepare() {
		Ok(said) => driver.say(&format!("{:?} interface: {said}", driver.interface.kind())),
		Err(failure) => driver.say(&format!("{:?} interface did not come ready - {failure:?}", driver.interface.kind())),
	}
	// WHO THE BMC IS.
	match ipmi_bmc::identify(&mut driver, &binding) {
		Ok((device, identity)) => {
			driver.say(&format!("{} - device {:#04x} revision {}, firmware {}.{:02x}, IPMI {}.{}, manufacturer {:#07x} product {:#06x}{}", identity.name(), device.device_id, device.device_revision, device.firmware_major, device.firmware_minor, device.ipmi.0, device.ipmi.1, device.manufacturer, device.product, if identity.selector().is_none() { " - its name is past the administrative selector's 64 bytes, so it cannot be administered" } else { "" }));
			driver.device = device;
			driver.identity = identity;
		}
		Err(answer) => driver.say(&format!("Get Device ID was not answered - {answer:?}; the BMC is looked for again every ten seconds")),
	}
	driver.read_repository();
	// THE WATCHDOG, on every form but SSIF: what the last reset was, and a timer found running taken over.
	if !ssif_form {
		match ipmi_bmc::data(&mut driver, &ipmi::watchdog::get()).ok().and_then(|bytes| ipmi::watchdog::state(&bytes)) {
			Some(state) => {
				let running = state.running;
				let flags = state.expiration_flags;
				let mut wd = Wd { running_at_bind: running, last_reset: state.expired(), bridge_until: None, next_pet: 0, armed_ms: running.then_some(state.initial_ms.max(ipmi::watchdog::MIN_TIMEOUT_MS)), slept_ms: None };
				if running {
					// TAKEN OVER: its own countdown kept, its flags cleared, and petted at once - then fed until the watchdog
					// service's first pet, at most the bridge bound.
					if let Some(request) = ipmi::watchdog::arm(state.initial_ms.max(ipmi::watchdog::MIN_TIMEOUT_MS), flags) {
						let _ = ipmi_bmc::data(&mut driver, &request);
					}
					let _ = ipmi_bmc::data(&mut driver, &ipmi::watchdog::pet());
					wd.bridge_until = Some(clock() + drivers::watchdog::BRIDGE_TICKS);
					wd.next_pet = clock() + drivers::watchdog::BRIDGE_PET_TICKS;
				} else if flags != 0 {
					let _ = ipmi_bmc::data(&mut driver, &ipmi::watchdog::disarm(flags));
				}
				driver.say(&format!("watchdog {} at bind, initial countdown {} ms{}", if running { "running" } else { "stopped" }, state.initial_ms, if wd.last_reset { ", the last reset was its own" } else { "" }));
				driver.watchdog = Some(wd);
			}
			None => driver.say("its watchdog did not answer Get Watchdog Timer - none is published"),
		}
	}
	// THE PUBLICATIONS.
	let mut offers: Vec<(u16, u16, u64, &[u8])> = alloc::vec![(TOKEN_IPMI, driver_protocol::provider::IPMI, 0, driver_protocol::provider::IPMI_NAME)];
	if !driver.zones.is_empty() {
		offers.push((TOKEN_POWER, driver_protocol::provider::POWER_SOURCE, 0, driver_protocol::provider::BMC_POWER_NAME));
	}
	if driver.watchdog.is_some() {
		offers.push((TOKEN_WATCHDOG, driver_protocol::provider::WATCHDOG, 0, driver_protocol::provider::BMC_WATCHDOG_NAME));
	}
	offers.push((TOKEN_SEL, driver_protocol::provider::ADMIN_EXECUTOR, 0, driver_protocol::provider::BMC_SEL_NAME));
	offers.push((TOKEN_CHASSIS, driver_protocol::provider::ADMIN_EXECUTOR, 0, driver_protocol::provider::BMC_CHASSIS_NAME));
	let mut ends: Vec<(u16, u64)> = Vec::new();
	let mut fars: Vec<(u16, u16, u64, &[u8])> = Vec::new();
	for (token, kind, _, published) in offers {
		let Some((near, far)) = channel() else { exit() };
		ends.push((token, near));
		fars.push((token, kind, far, published));
	}
	driver.serving = common::Serving::from_offers(&ends);
	for (token, kind, far, published) in fars {
		if !common::offer_named(bootstrap, &bind, kind, token, published, far) {
			exit();
		}
	}
	driver.say(&format!("publishes ipmi{}{}, and the two administrative executors", if driver.zones.is_empty() { "" } else { ", power-source" }, if driver.watchdog.is_some() { ", watchdog" } else { "" }));
	let timer: u64 = match timer_create() {
		t if t > 0 => t as u64,
		_ => exit(),
	};
	driver.next_poll = clock() + POLL_TICKS;
	common::takes_sleep();
	loop {
		// THE NEXT THING DUE: a zone poll, a bridge pet, the look for a BMC that went away.
		let mut due = if driver.zones.is_empty() { u64::MAX } else { driver.next_poll };
		if let Some(wd) = &driver.watchdog
			&& wd.bridge_until.is_some()
		{
			due = due.min(wd.next_pet);
		}
		if !driver.availability.available() {
			due = due.min(driver.next_probe);
		}
		timer_set(timer, due);
		match common::wait_providers_until(bootstrap, &bind, &mut driver.serving, &[timer], 0) {
			None => {
				// A PLANNED STOP WRITES NOTHING TO THE WATCHDOG: a watchdog nobody feeds is a watchdog doing its work.
				if common::stop_requested() {
					common::finish_stop(bootstrap, &bind, 0, true);
				}
				exit();
			}
			// THE SLEEP, between transactions.
			Some(None) => {
				if !common::take_sleep_step(bootstrap, &bind, &mut driver) {
					if common::stop_requested() {
						common::finish_stop(bootstrap, &bind, 0, true);
					}
					exit();
				}
			}
			Some(Some(common::ProviderReady::Connected(_))) | Some(Some(common::ProviderReady::Device(_))) => {}
			Some(Some(common::ProviderReady::Consumer(index))) => {
				if !serve(&mut driver, index) {
					let token = driver.serving.close_at(index);
					if token == TOKEN_POWER && driver.power_stream != 0 {
						close(driver.power_stream);
						driver.power_stream = 0;
					}
					if token == TOKEN_IPMI && driver.changes != 0 {
						close(driver.changes);
						driver.changes = 0;
					}
					if !common::disconnected(bootstrap, &bind, token) {
						exit();
					}
				}
			}
		}
		let now = clock();
		// THE BRIDGE: fed until the consumer's first word, at most its bound.
		let mut bridge_pet = false;
		if let Some(wd) = driver.watchdog.as_mut()
			&& let Some(until) = wd.bridge_until
		{
			if now >= until {
				wd.bridge_until = None;
			} else if now >= wd.next_pet {
				wd.next_pet = now + drivers::watchdog::BRIDGE_PET_TICKS;
				bridge_pet = true;
			}
		}
		if bridge_pet {
			driver.in_watchdog = true;
			let _ = ipmi_bmc::data(&mut driver, &ipmi::watchdog::pet());
			driver.in_watchdog = false;
		}
		if !driver.availability.available() && now >= driver.next_probe {
			driver.next_probe = now + PROBE_TICKS;
			let _ = ipmi_bmc::data(&mut driver, &Request::new(ipmi::netfn::APP, ipmi::identity::GET_DEVICE_ID, &[]));
		}
		// ANY ANSWER after the BMC went away brings it back - the probe's or a consumer's - and it is identified again.
		if driver.back {
			driver.back = false;
			driver.came_back();
		}
		if !driver.zones.is_empty() && now >= driver.next_poll {
			driver.next_poll = now + POLL_TICKS;
			driver.poll_zones();
		}
	}
}
