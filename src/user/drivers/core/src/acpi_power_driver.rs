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
// is said by name and that reading is not published. The cooling half of a thermal zone is not this driver's yet.
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
use proto::system::{ControlOutcome, Error, ProviderCommand, ProviderSource, ProviderUpdate, ProviderUpdateKind, SourceState, acpi_node, power_provider};
use rt::*;
use wire::Handles;

// How long one question to the node may take.
const TICKS: u64 = TICKS_PER_SECOND * 5;
// Deep enough that a run of changes never waits on PowerService's reading.
const STREAM_DEPTH: u64 = 64;
// The one publication's token.
const TOKEN: u16 = 0;
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

	// READ AGAIN, AND PUBLISH WHAT CHANGED - nothing while the node is gone, when the last reading stands.
	fn refresh(&mut self, information: bool) {
		if self.node == 0 {
			return;
		}
		match self.read(information) {
			Ok(state) => {
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

// THE NODE'S `Notify` STREAM, 0 when the node gives none.
fn subscribe(node: u64) -> u64 {
	acpi_node::Client::with_deadline(ChannelTransport { chan: node }, clock() + TICKS).notifications().unwrap_or(0)
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
	let (bind, _resources) = common::handshake(bootstrap);
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
	let mut power = Power { name, class, node, information: None, state: None, revision: 1, stream: 0, seq: 0, poll: None };
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
	let mut serving = common::Serving::from_offers(&[(TOKEN, producer)]);
	if !common::offer(bootstrap, &bind, driver_protocol::provider::POWER_SOURCE, TOKEN, consumer) {
		exit();
	}
	say(&power.name, &format!("publishes its {:?} state", power.class));
	let mut coalescer = Coalescer::default();
	let mut owed_information = false;
	let mut next_poll = power.poll.map(|interval| clock() + interval);
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
			next_poll = power.poll.map(|interval| now + interval);
		}
		match ready {
			None => {}
			Some(common::ProviderReady::Connected(_)) => {}
			Some(common::ProviderReady::Consumer(index)) => {
				let channel = serving.at(index);
				if !serve(&mut power, channel, &mut buf) {
					let token = serving.close_at(index);
					if power.stream != 0 {
						close(power.stream);
						power.stream = 0;
					}
					if !common::disconnected(bootstrap, &bind, token) {
						exit();
					}
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
