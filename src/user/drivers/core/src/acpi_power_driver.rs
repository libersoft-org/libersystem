// THE ACPI BATTERY, AC ADAPTER AND THERMAL ZONE DRIVER: one binding per namespace node - a control-method battery
// (`PNP0C0A`), an AC adapter (`ACPI0003`) or a thermal zone (published under `THERMALZONE`) - reading the node's methods
// over the node-scoped channel the ACPI service serves, normalising them with `power_model`'s adapters, and publishing
// the result as ONE `power-source` provider to PowerService: a snapshot when its stream opens, an update whenever the
// state it reads differs from the last it published.
//
// WHEN IT READS: at bind; at each `Notify` the firmware raises on the node - 0x80 (status changed) reads the state
// again, 0x81 (information changed) also reads a battery's `_BIX`/`_BIF` again - with a storm bounded by
// `drivers::acpi_power::Coalescer`; and at a thermal zone's `_TZP` interval where its firmware asks to be polled. A
// method the node does not have is not a refusal (`_STA`, `_RTV`, the trips are optional); a result of the wrong shape
// is said by name and that reading is not published.
//
// A THERMAL ZONE'S COOLING HALF is a second publication of the same binding, `thermal-zone`, served to
// ProcessorPowerService alone: what passive and active cooling need - `_PSV`, `_CRT`, `_HOT`, `_TC1`, `_TC2`, `_TSP`,
// `_PSL`, every `_ACx` with its `_ALx`, and whether there is `_SCP` - every reading of `_TMP` as it is taken, `_TMP`
// read every `_TSP` while the policy asks for it, and `_SCP` set as the policy asks. AND THE FALLBACK: every reading is
// compared with `_CRT` here too - the over-temperature alarm the power model derives - and at the crossing the kernel's
// forced power-off is armed through the `system-power` connection DeviceManager hands a zone's binding, so a policy that
// is dead, restarting or stopped still leaves the machine off within the bound.
//
// WHEN THE ACPI SERVICE RESTARTS its node channels end with it: what was published stays until it is read again, the
// node is asked for again (`drivers::common`), and the one the new instance serves is subscribed to and read at once.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aml::wire::Value;
use drivers::acpi_power::{self, Class, Coalescer, Information, Status, Zone};
use drivers::common;
use ipc_client::ChannelTransport;
use proto::system::{ActiveTrip, AlarmKind, ControlOutcome, Error, ProviderCommand, ProviderSource, ProviderUpdate, ProviderUpdateKind, SourceState, Tristate, ZoneCooling, ZoneReading, acpi_node, power_provider, system_power, thermal_zone};
use rt::*;
use wire::Handles;

// How long one question to the node may take.
const TICKS: u64 = TICKS_PER_SECOND * 5;
// Deep enough that a run of changes never waits on PowerService's reading.
const STREAM_DEPTH: u64 = 64;
// The power-source publication's token, and a zone's cooling half's.
const TOKEN: u16 = 0;
const TOKEN_COOLING: u16 = 1;
// The fastest a policy may have `_TMP` read: every tenth of a second.
const MIN_SAMPLE_MS: u32 = 100;
// THE FORCED POWER-OFF'S BOUND past `_CRT`, the policy's own.
const FORCED_BOUND_SECONDS: u32 = 10;
// The most `_PSL` processors and `_ALx` fans a zone describes.
const MOST_DEVICES: usize = 8;
// The one source's local number.
const LOCAL: u32 = 0;
// `_TZP` is in tenths of a second; a zone asking for less than a second is polled once a second.
const MIN_POLL_TICKS: u64 = TICKS_PER_SECOND;
// What a node without `_STA` is taken to answer.
const STA_DEFAULT: u32 = 0x0F;
// How often a binding whose node ended looks for the one handed again.
const NODE_LOOK_TICKS: u64 = TICKS_PER_SECOND;

fn say(name: &str, text: &str) {
	let line = format!("driver.acpi-power: {name}: {text}\n");
	print(line.as_bytes());
}

struct Power {
	name: String,
	class: Class,
	node: u64,
	information: Option<Information>,
	state: Option<SourceState>,
	revision: u64,
	stream: u64,
	seq: u32,
	// A thermal zone's polling interval, when its firmware asks for polling.
	poll: Option<u64>,
	// A ZONE'S COOLING HALF: the last `_TMP`, the readings stream the policy opened and its sequence, the period the policy
	// asked `_TMP` to be read at (0 for the zone's own), the `system-power` connection the fallback arms through, and
	// whether the last reading was past `_CRT`.
	temperature: Option<u32>,
	readings: u64,
	reading_seq: u32,
	sample_ms: u32,
	syspower: u64,
	past_critical: bool,
}

impl Power {
	// A method of the node's: its value, `None` when the node does not have it.
	fn evaluate(&self, method: &str) -> Result<Option<Value>, String> {
		match acpi_node::Client::with_deadline(ChannelTransport { chan: self.node }, clock() + TICKS).evaluate(method, &[]) {
			Some(Ok(bytes)) => aml::wire::decode(&bytes).map(Some).map_err(|error| format!("{method} did not decode - {error:?}")),
			Some(Err(Error::NotFound)) => Ok(None),
			Some(Err(error)) => Err(format!("{method} was refused - {error:?}")),
			None => Err(format!("{method} was not answered")),
		}
	}

	fn integer(&self, method: &str) -> Result<Option<u32>, String> {
		match self.evaluate(method)? {
			Some(value) => acpi_power::integer(&value).map(Some).map_err(|refusal| format!("{method}: {refusal:?}")),
			None => Ok(None),
		}
	}

	fn required(&self, method: &str) -> Result<Value, String> {
		self.evaluate(method)?.ok_or_else(|| format!("the node has no {method}"))
	}

	// THE STATE, READ NOW - the battery's static description again when `information` says so.
	fn read(&mut self, information: bool) -> Result<SourceState, String> {
		match self.class {
			Class::Battery => {
				let status = self.integer("_STA")?;
				if information || self.information.is_none() {
					let described = match self.evaluate("_BIX")? {
						Some(value) => acpi_power::bix(&value),
						None => acpi_power::bif(&self.required("_BIF")?),
					};
					self.information = Some(described.map_err(|refusal| format!("{refusal:?}"))?);
				}
				// `_BST` IS UNDEFINED FOR AN EMPTY SLOT, and the adapter measures nothing of one.
				let present = status.is_none_or(|status| status & (1 << 4) != 0);
				let now = if present { acpi_power::bst(&self.required("_BST")?).map_err(|refusal| format!("{refusal:?}"))? } else { Status::default() };
				Ok(power_model::acpi::battery(&acpi_power::battery(status, &self.information.unwrap_or_default(), &now)))
			}
			Class::Ac => {
				// A DEVICE WITHOUT `_STA` IS PRESENT AND FUNCTIONING (ACPI 6.5, 6.3.7). A battery's presence is its bit 4,
				// which that default does not set, so a battery without one stays unknown rather than absent.
				let status = Some(self.integer("_STA")?.unwrap_or(STA_DEFAULT));
				let power_source = self.integer("_PSR")?;
				Ok(power_model::acpi::ac(&power_model::acpi::Ac { status, power_source }))
			}
			Class::ThermalZone => {
				let temperature = self.integer("_TMP")?.ok_or_else(|| String::from("the zone has no _TMP"))?;
				self.temperature = Some(temperature);
				let mut zone = Zone { temperature, relative: self.integer("_RTV")?.is_some_and(|relative| relative != 0), critical: self.integer("_CRT")?, hot: self.integer("_HOT")?, passive: self.integer("_PSV")?, active: Vec::new() };
				for index in 0..10u8 {
					match self.integer(&format!("_AC{index}"))? {
						Some(trip) => zone.active.push(trip),
						None => break,
					}
				}
				Ok(power_model::acpi::thermal(&zone.thermal()))
			}
		}
	}

	// ONE FRAME ON THE STREAM, if PowerService has opened it. Never waited on: the stream is deep, and a full one is said.
	fn emit(&mut self, update: &ProviderUpdate) {
		if self.stream == 0 {
			return;
		}
		let mut frame = [0u8; 2048];
		let mut handles = Handles::new();
		if let Some(len) = power_provider::updates_frame(self.seq, update, &mut frame, &mut handles) {
			match try_send_outcome(self.stream, &frame[..len], 0) {
				SendOutcome::Delivered => self.seq = self.seq.wrapping_add(1),
				SendOutcome::Stalled => say(&self.name, "an update was dropped: the stream is full"),
				SendOutcome::Failed => {
					close(self.stream);
					self.stream = 0;
				}
			}
		}
	}

	// READ AGAIN, AND PUBLISH WHAT CHANGED - nothing while the node is gone, when the last reading stands. A zone's
	// reading goes to the policy whatever else changed, and is checked against `_CRT`.
	fn refresh(&mut self, information: bool) {
		if self.node == 0 {
			return;
		}
		match self.read(information) {
			Ok(state) => {
				if self.class == Class::ThermalZone {
					self.cooling_reading(&state);
				}
				if self.state.as_ref() == Some(&state) {
					return;
				}
				self.revision += 1;
				self.state = Some(state.clone());
				let update = ProviderUpdate { revision: self.revision, kind: ProviderUpdateKind::Updated, source: Some(ProviderSource { local: LOCAL, state }), gone: None };
				self.emit(&update);
			}
			Err(why) => say(&self.name, &format!("its state could not be read - {why}")),
		}
	}

	// ONE READING FOR THE POLICY, and the fallback's comparison with `_CRT`.
	fn cooling_reading(&mut self, state: &SourceState) {
		let Some(temperature) = self.temperature else { return };
		if self.readings != 0 {
			self.reading_seq = self.reading_seq.wrapping_add(1);
			let mut frame = [0u8; 64];
			let mut handles = Handles::new();
			if let Some(len) = thermal_zone::readings_frame(self.reading_seq, &ZoneReading { temperature, sequence: self.reading_seq }, &mut frame, &mut handles) {
				match try_send_outcome(self.readings, &frame[..len], 0) {
					SendOutcome::Delivered => {}
					SendOutcome::Stalled => say(&self.name, "a reading was dropped: the policy's stream is full"),
					SendOutcome::Failed => {
						close(self.readings);
						self.readings = 0;
					}
				}
			}
		}
		// THE FALLBACK: at the crossing of `_CRT`, the forced power-off armed here, whatever the policy does.
		let past = state.alarms.iter().any(|alarm| alarm.kind == AlarmKind::OverTemperature && alarm.state == Tristate::Yes);
		if past && !self.past_critical {
			if self.syspower == 0 {
				say(&self.name, "the zone is past _CRT - and this binding holds no system-power connection to arm the forced power-off");
			} else {
				match system_power::Client::with_deadline(ChannelTransport { chan: self.syspower }, clock() + TICKS).power_off_within(&FORCED_BOUND_SECONDS) {
					Some(Ok(())) => say(&self.name, &format!("the zone is past _CRT - the forced power-off is armed, the machine is off within {FORCED_BOUND_SECONDS} s")),
					Some(Err(error)) => say(&self.name, &format!("the zone is past _CRT - the forced power-off was refused: {error:?}")),
					None => say(&self.name, "the zone is past _CRT - SystemManager did not answer the forced power-off"),
				}
			}
		}
		self.past_critical = past;
	}

	// THE PERIOD `_TMP` IS READ AT: the policy's while it asks, else the zone's own.
	fn interval(&self) -> Option<u64> {
		match self.sample_ms {
			0 => self.poll,
			ms => Some((u64::from(ms) * TICKS_PER_SECOND / 1000).max(1)),
		}
	}

	fn snapshot(&self) -> Vec<ProviderUpdate> {
		let mut updates = Vec::new();
		if let Some(state) = self.state.clone() {
			updates.push(ProviderUpdate { revision: self.revision, kind: ProviderUpdateKind::Snapshot, source: Some(ProviderSource { local: LOCAL, state }), gone: None });
		}
		updates.push(ProviderUpdate { revision: self.revision, kind: ProviderUpdateKind::SnapshotEnd, source: None, gone: None });
		updates
	}
}

impl power_provider::Service for Power {
	fn updates(&mut self) -> Vec<ProviderUpdate> {
		self.snapshot()
	}

	// READ-ONLY: a battery, an adapter and a zone advertise no control.
	fn command(&mut self, _command: ProviderCommand) -> Result<ControlOutcome, Error> {
		Err(Error::Unsupported)
	}

	fn query(&mut self, local: u32) -> Result<SourceState, Error> {
		if local != LOCAL {
			return Err(Error::NotFound);
		}
		self.refresh(false);
		self.state.clone().ok_or(Error::NotFound)
	}
}

impl thermal_zone::Service for Power {
	// THE ZONE'S COOLING OBJECTS, read now: an object the zone does not have is absent, one of the wrong shape refused.
	fn describe(&mut self) -> Result<ZoneCooling, Error> {
		let read = |power: &Power, method: &str| {
			power.integer(method).map_err(|why| {
				say(&power.name, &why);
				Error::Io
			})
		};
		let list = |power: &Power, method: &str| -> Result<Vec<String>, Error> {
			match power.evaluate(method) {
				Ok(Some(value)) => acpi_power::devices(&value, MOST_DEVICES).map_err(|refusal| {
					say(&power.name, &format!("{method}: {refusal:?}"));
					Error::Io
				}),
				Ok(None) => Ok(Vec::new()),
				Err(why) => {
					say(&power.name, &why);
					Err(Error::Io)
				}
			}
		};
		let mut active = Vec::new();
		for level in 0..10u8 {
			let Some(temperature) = read(self, &format!("_AC{level}"))? else { break };
			active.push(ActiveTrip { level, temperature, devices: list(self, &format!("_AL{level}"))? });
		}
		let scp = matches!(acpi_node::Client::with_deadline(ChannelTransport { chan: self.node }, clock() + TICKS).has("_SCP"), Some(Ok(true)));
		let zone = self.name.strip_prefix("acpi:").unwrap_or(&self.name);
		Ok(ZoneCooling { zone: acpi_power::namespace_path(zone), passive: read(self, "_PSV")?, critical: read(self, "_CRT")?, hot: read(self, "_HOT")?, tc1: read(self, "_TC1")?.unwrap_or(0), tc2: read(self, "_TC2")?.unwrap_or(0), tsp: read(self, "_TSP")?.unwrap_or(0), passive_processors: list(self, "_PSL")?, active, scp })
	}

	// THE READINGS FROM NOW ON, the last one first.
	fn readings(&mut self) -> Vec<ZoneReading> {
		self.temperature.map(|temperature| ZoneReading { temperature, sequence: self.reading_seq }).into_iter().collect()
	}

	fn sample_every(&mut self, milliseconds: u32) -> Result<(), Error> {
		self.sample_ms = if milliseconds == 0 { 0 } else { milliseconds.max(MIN_SAMPLE_MS) };
		say(&self.name, &if self.sample_ms == 0 { String::from("_TMP is read at the zone's own period again") } else { format!("_TMP is read every {} ms for the thermal policy", self.sample_ms) });
		Ok(())
	}

	// `_SCP(mode)`: 0 active cooling preferred, 1 passive.
	fn cooling_policy(&mut self, mode: u8) -> Result<(), Error> {
		if mode > 1 {
			return Err(Error::Invalid);
		}
		let arguments = aml::wire::encode(&Value::Package(alloc::vec![Value::Integer(u64::from(mode))])).map_err(|_| Error::Invalid)?;
		match acpi_node::Client::with_deadline(ChannelTransport { chan: self.node }, clock() + TICKS).evaluate("_SCP", &arguments) {
			Some(Ok(_)) => {
				say(&self.name, if mode == 0 { "_SCP: active cooling preferred" } else { "_SCP: passive cooling preferred" });
				Ok(())
			}
			Some(Err(Error::NotFound)) => Err(Error::Unsupported),
			Some(Err(error)) => Err(error),
			None => Err(Error::Io),
		}
	}
}

// THE NODE'S `Notify` STREAM, 0 when the node gives none.
fn subscribe(node: u64) -> u64 {
	acpi_node::Client::with_deadline(ChannelTransport { chan: node }, clock() + TICKS).notifications().unwrap_or(0)
}

// ONE REQUEST FROM PROCESSORPOWERSERVICE on the cooling half's connection: the readings stream opened, or a question
// answered.
fn serve_cooling(power: &mut Power, channel: u64, buf: &mut [u8]) -> bool {
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	if len < 2 {
		return true;
	}
	let op = u16::from_le_bytes([buf[0], buf[1]]);
	if op == thermal_zone::OP_READINGS {
		let Some((corr, items)) = thermal_zone::readings_open(power, &buf[..len], &mut handles) else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		if power.readings != 0 {
			close(power.readings);
		}
		power.readings = producer;
		let mut frame = [0u8; 64];
		for item in &items {
			let mut frame_handles = Handles::new();
			if let Some(written) = thermal_zone::readings_frame(item.sequence, item, &mut frame, &mut frame_handles) {
				let _ = try_send(producer, &frame[..written], 0);
			}
		}
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		return true;
	}
	let mut reply = [0u8; 2048];
	let mut reply_handles = Handles::new();
	if let Some(written) = thermal_zone::dispatch(power, &buf[..len], &mut handles, &mut reply, &mut reply_handles) {
		send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
	}
	true
}

// ONE REQUEST FROM POWERSERVICE on the publication's connection: the update stream opened, or a query answered.
fn serve(power: &mut Power, channel: u64, buf: &mut [u8]) -> bool {
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	if len < 2 {
		return true;
	}
	let op = u16::from_le_bytes([buf[0], buf[1]]);
	if op == power_provider::OP_UPDATES {
		let Some((corr, items)) = power_provider::updates_open(power, &buf[..len], &mut handles) else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		if power.stream != 0 {
			close(power.stream);
		}
		power.stream = producer;
		power.seq = 0;
		for item in &items {
			power.emit(item);
		}
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		return true;
	}
	let mut reply = [0u8; 4096];
	let mut reply_handles = Handles::new();
	if let Some(written) = power_provider::dispatch(power, &buf[..len], &mut handles, &mut reply, &mut reply_handles) {
		send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
	}
	true
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	let name = String::from_utf8_lossy(bind.info.platform.identity()).into_owned();
	let Some(class) = acpi_power::class_of(bind.info.platform.match_ids().iter().map(|id| id.text())) else {
		say(&name, "its ids name none of a battery, an AC adapter or a thermal zone");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::UnsupportedDevice);
	};
	// ONLINE FIRST, publishing nothing yet: the node channel is answered only to a binding that is online.
	let report = format!("driver.acpi-power: {name}: online ({class:?})");
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
		say(&name, "the firmware describes no node for it");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	let mut power = Power { name, class, node, information: None, state: None, revision: 1, stream: 0, seq: 0, poll: None, temperature: None, readings: 0, reading_seq: 0, sample_ms: 0, syspower: resources.syspower, past_critical: false };
	if let Err(why) = power.read(true).map(|state| power.state = Some(state)) {
		say(&power.name, &format!("its state could not be read - {why}"));
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::DeviceNotResponding);
	}
	// A ZONE THAT ASKS TO BE POLLED is polled at its interval, never faster than once a second.
	if class == Class::ThermalZone
		&& let Ok(Some(tenths)) = power.integer("_TZP")
		&& tenths != 0
	{
		power.poll = Some((tenths as u64 * TICKS_PER_SECOND / 10).max(MIN_POLL_TICKS));
	}
	let mut notifications = subscribe(node);
	let Some((producer, consumer)) = channel() else { exit() };
	// A ZONE PUBLISHES ITS COOLING HALF TOO, under a token of its own.
	let cooling = if class == Class::ThermalZone { channel() } else { None };
	let mut offers = alloc::vec![(TOKEN, producer)];
	if let Some((cooling_producer, _)) = cooling {
		offers.push((TOKEN_COOLING, cooling_producer));
	}
	let mut serving = common::Serving::from_offers(&offers);
	if !common::offer(bootstrap, &bind, driver_protocol::provider::POWER_SOURCE, TOKEN, consumer) {
		exit();
	}
	if let Some((_, cooling_consumer)) = cooling
		&& !common::offer(bootstrap, &bind, driver_protocol::provider::THERMAL_ZONE, TOKEN_COOLING, cooling_consumer)
	{
		exit();
	}
	say(&power.name, &format!("publishes its {:?} state{}", power.class, if cooling.is_some() { ", and its cooling half" } else { "" }));
	// THE FIRST READING IS CHECKED AGAINST `_CRT` TOO: a zone already past it at bind is a crossing.
	if let Some(state) = power.state.clone()
		&& class == Class::ThermalZone
	{
		power.cooling_reading(&state);
	}
	let mut coalescer = Coalescer::default();
	let mut owed_information = false;
	let mut next_poll = power.interval().map(|interval| clock() + interval);
	let mut interval = power.interval();
	// THE STORM REPORT, at one coalesced notification and at each doubling after: a storm is said, and bounded in lines.
	let mut report_at = 1;
	let mut buf = alloc::vec![0u8; 4096];
	loop {
		// THE WAKE: a notification, a request from PowerService, or the owed refresh's or the poll's deadline - and while
		// the node is gone, the look for the one handed again.
		let looking = (power.node == 0).then(|| clock() + NODE_LOOK_TICKS);
		let deadline = [coalescer.due_at(), next_poll, looking].into_iter().flatten().min().unwrap_or(u64::MAX);
		let mut handles: Vec<u64> = Vec::new();
		if notifications != 0 {
			handles.push(notifications);
		}
		let consumers_at = handles.len();
		handles.extend_from_slice(serving.as_slice());
		let ready = common::wait_or_answer_until(bootstrap, &bind, &handles, deadline, Some(&mut serving)).map(|ready| ready.map(|at| if at < consumers_at { common::ProviderReady::Device(at) } else { common::ProviderReady::Consumer(at - consumers_at) }));
		let Some(ready) = ready else {
			if common::stop_requested() {
				common::finish_stop(bootstrap, &bind, 0, true);
			}
			exit();
		};
		let now = clock();
		match common::fresh_node() {
			Some(0) => say(&power.name, "the firmware describes no node for it any more"),
			Some(node) => {
				power.node = node;
				if notifications != 0 {
					close(notifications);
				}
				notifications = subscribe(node);
				say(&power.name, "its node is handed again - reading it");
				power.refresh(true);
			}
			None => {}
		}
		if coalescer.take_due(now) {
			power.refresh(core::mem::take(&mut owed_information));
		}
		if let Some(at) = next_poll
			&& now >= at
		{
			power.refresh(false);
			next_poll = power.interval().map(|interval| now + interval);
		}
		match ready {
			None => {}
			Some(common::ProviderReady::Connected(_)) => {}
			Some(common::ProviderReady::Consumer(index)) => {
				let channel = serving.at(index);
				let cooling = serving.token_at(index) == TOKEN_COOLING;
				let kept = if cooling { serve_cooling(&mut power, channel, &mut buf) } else { serve(&mut power, channel, &mut buf) };
				if !kept {
					let token = serving.close_at(index);
					let stream = if cooling { &mut power.readings } else { &mut power.stream };
					if *stream != 0 {
						close(*stream);
						*stream = 0;
					}
					// THE POLICY WENT: the zone's own period again.
					if cooling {
						power.sample_ms = 0;
					}
					if !common::disconnected(bootstrap, &bind, token) {
						exit();
					}
				}
				// A PERIOD THE POLICY ASKED FOR (or gave back) takes effect from now.
				if power.interval() != interval {
					interval = power.interval();
					next_poll = interval.map(|interval| clock() + interval);
				}
			}
			Some(common::ProviderReady::Device(_)) => 'drain: loop {
				match try_recv_caps(notifications, &mut buf) {
					PolledCaps::Message { len, handles } => {
						for &leftover in handles.as_slice() {
							close(leftover);
						}
						let mut frame = Handles::new();
						let Some(notification) = acpi_node::notifications_read(&buf[..len], &mut frame) else { continue };
						let information = notification.value == 0x81;
						if coalescer.notified(clock()) {
							power.refresh(information);
						} else {
							owed_information |= information;
						}
					}
					PolledCaps::Empty => break,
					PolledCaps::Closed => {
						// THE SERVICE ENDED: its channels with it. What was published stands until the node the new
						// instance serves is read; the node itself is asked for again by the control path.
						close(notifications);
						notifications = 0;
						power.node = 0;
						say(&power.name, "its node's notifications ended - the ACPI service is gone; waiting for its node again");
						break 'drain;
					}
				}
			},
		}
		if coalescer.coalesced >= report_at {
			say(&power.name, &format!("{} notification(s) coalesced into later readings", coalescer.coalesced));
			report_at = coalescer.coalesced * 2;
		}
	}
}
