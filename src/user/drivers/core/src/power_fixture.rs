// power_fixture - the in-guest power sources a gate needs: a HID-shaped UPS, and an ACPI-shaped
// battery, AC adapter and thermal zone, published as two `power-source` providers over the production
// provider wire - and a control endpoint for the one probe that drives the gate.
//
// DEVELOPMENT-ONLY. It is staged into the image a gate builds and into no shipping one, and nothing a
// client does can enable it: it binds to a QEMU test function at a pinned address, which a shipping
// machine does not have, and its control endpoint is a kind no scope minted for real hardware admits.
//
// WHAT IT CLAIMS AND WHAT IT DOES NOT. Its numbers are DECODED DATA, written out below: the fields a
// HID Power Device driver extracts from a UPS's reports, and the integers an ACPI driver reads out of
// `_BIF`, `_BST`, `_PSR` and `_TMP`. They reach PowerService through `power_model`'s adapters - the
// ones a real driver calls - so what this proves is the service and the normalisation. It is not a
// claim that QEMU attaches a UPS or evaluates battery AML, and nothing in it parses a report
// descriptor or a namespace.
//
// WHAT THE CONTROL ENDPOINT CAN DO. Change a decoded input, remove a source, withdraw a publication
// and publish it again under a new generation, make the UPS provider withhold its next replies, try a
// publication its registry entry does not allow, and read back every command either provider received
// and what the UPS's outlets are now. That last is what makes an operator's command observable: it
// reaches exactly one outlet here, or the gate fails.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use drivers::common;
use power_model::convert::Field;
use power_model::{acpi, hid};
use proto::system::{ControlOutcome, Error, FixtureCommand, FixtureField, ProviderCommand, ProviderSource, ProviderUpdate, ProviderUpdateKind, SourceState, power_fixture, power_provider};
use rt::*;

// The two provider publications, by the number the control endpoint names them with, and their names.
const UPS: u8 = 0;
const ACPI: u8 = 1;
const UPS_NAME: &[u8] = b"org.libersystem.power-fixture.ups";
const ACPI_NAME: &[u8] = b"org.libersystem.power-fixture.acpi";
const CONTROL_NAME: &[u8] = b"org.libersystem.power-fixture.control";
const EXTRA_NAME: &[u8] = b"org.libersystem.power-fixture.extra";
// The control endpoint's token. The two providers take 0 and 1 first, and fresh ones from 3 on.
const CONTROL_TOKEN: u16 = 2;
// Deep enough that a burst of changes the probe drives never waits on PowerService's reading.
const STREAM_DEPTH: u64 = 256;
// The command log is bounded, like everything a peer can make grow.
const MAX_COMMANDS: usize = 64;
// The UPS's switchable outlets.
const OUTLETS: usize = 2;

// The HID Power Device class's units, as its descriptors spell them.
const VOLT: u32 = 0x00f0_d121;
const AMPERE: u32 = 0x0010_0001;
const WATT: u32 = 0x0000_d121;
const KELVIN: u32 = 0x0001_0001;
const SECOND: u32 = 0x0000_1001;

fn field(bits: u8, logical_max: i32, unit: u32, exponent: i8) -> Field {
	Field { bits, logical_min: 0, logical_max, null_state: false, unit, exponent }
}

fn value(raw: u32, field: Field) -> Option<hid::Value> {
	Some(hid::Value { raw, field })
}

// ------------------------------------------------------------------ the decoded inputs

struct Model {
	ups: hid::Ups,
	battery: acpi::Battery,
	ac: acpi::Ac,
	// `_TMP`, tenths of a kelvin. The zone's trips are fixed below.
	temperature: u32,
	outlets: [bool; OUTLETS],
}

// A zone's trips: critical at 368.2 K, hot at 363.2 K, passive at 353.2 K, two active levels.
const ACTIVE: [u32; 2] = [3432, 3332];

impl Model {
	// A small line-interactive UPS on mains power, and a laptop battery charging from an adapter that
	// is on line, in a zone at 300.2 K.
	fn new() -> Self {
		let raw16 = field(16, 65_535, 0, 0);
		Self { ups: hid::Ups { capacity_mode: Some(0), remaining_capacity: value(6_300, raw16), full_charge_capacity: value(7_000, raw16), design_capacity: value(7_200, raw16), remaining_capacity_limit: value(700, raw16), relative_state_of_charge: None, run_time_to_empty: value(2_400, field(16, 65_535, SECOND, 0)), voltage: value(1_365, field(16, 65_535, VOLT, 5)), current: value(0, field(16, 65_535, AMPERE, -2)), active_power: value(180, field(16, 65_535, WATT, 7)), percent_load: value(36, field(8, 255, 0, 0)), temperature: value(300, field(16, 65_535, KELVIN, 0)), status: hid::Status { ac_present: Some(true), charging: Some(false), discharging: Some(false), below_remaining_capacity_limit: Some(false), need_replacement: Some(false), overload: Some(false), internal_failure: Some(false), over_temperature: None }, switchable_outlets: OUTLETS as u8, delay_before_shutdown: true, source_time: None }, battery: acpi::Battery { status: Some(0x1f), power_unit: 0, design_capacity: 50_000, last_full_capacity: 48_000, design_warning: 5_000, design_low: 2_000, state: 0b10, rate: 5_000, remaining: 24_000, voltage: 11_100 }, ac: acpi::Ac { status: Some(0x0f), power_source: Some(1) }, temperature: 3002, outlets: [true; OUTLETS] }
	}

	// One source's canonical state, through the adapters a real driver calls.
	fn state(&self, publication: u8, local: u32) -> Option<SourceState> {
		match (publication, local) {
			(UPS, 0) => Some(hid::ups(&self.ups)),
			(ACPI, 0) => Some(acpi::battery(&self.battery)),
			(ACPI, 1) => Some(acpi::ac(&self.ac)),
			(ACPI, 2) => Some(acpi::thermal(&acpi::Thermal { temperature: self.temperature, relative: false, critical: Some(3682), hot: Some(3632), passive: Some(3532), active: &ACTIVE })),
			_ => None,
		}
	}

	// Change one decoded input, answering which source it belongs to.
	fn set(&mut self, which: FixtureField, raw: u32) -> (u8, u32) {
		match which {
			FixtureField::UpsRemaining => {
				self.ups.remaining_capacity = value(raw, field(16, 65_535, 0, 0));
				(UPS, 0)
			}
			FixtureField::UpsVoltage => {
				self.ups.voltage = value(raw, field(16, 65_535, VOLT, 5));
				(UPS, 0)
			}
			FixtureField::UpsAcPresent => {
				// Off line, a UPS runs from its battery: the two flags say so together.
				self.ups.status.ac_present = Some(raw != 0);
				self.ups.status.discharging = Some(raw == 0);
				(UPS, 0)
			}
			FixtureField::UpsOverload => {
				self.ups.status.overload = Some(raw != 0);
				(UPS, 0)
			}
			FixtureField::BatteryRemaining => {
				self.battery.remaining = raw;
				(ACPI, 0)
			}
			FixtureField::BatteryState => {
				self.battery.state = raw;
				(ACPI, 0)
			}
			FixtureField::AcOnline => {
				self.ac.power_source = Some(raw);
				(ACPI, 1)
			}
			FixtureField::ThermalTemperature => {
				self.temperature = raw;
				(ACPI, 2)
			}
		}
	}
}

// ------------------------------------------------------------------ the publications

struct Publication {
	token: u16,
	live: bool,
	// The producer end of the update stream PowerService opened, 0 until it has.
	stream: u64,
	seq: u32,
	locals: u16,
	revision: u64,
}

struct Fixture {
	model: Model,
	publications: [Publication; 2],
	commands: Vec<FixtureCommand>,
	withhold_commands: u32,
	withhold_queries: u32,
	next_token: u16,
}

impl Fixture {
	fn publication_of(&self, token: u16) -> Option<u8> {
		self.publications.iter().position(|publication| publication.token == token).map(|at| at as u8)
	}

	// One frame on a publication's update stream, if PowerService has opened it. NEVER WAITED ON: the
	// stream is deep, and a fixture that blocked on its consumer would be testing itself.
	fn emit(&mut self, which: u8, update: &ProviderUpdate) {
		let publication = &mut self.publications[which as usize];
		if publication.stream == 0 {
			return;
		}
		let mut frame = [0u8; 2048];
		let mut handles = wire::Handles::new();
		if let Some(len) = power_provider::updates_frame(publication.seq, update, &mut frame, &mut handles) {
			match try_send_outcome(publication.stream, &frame[..len], 0) {
				SendOutcome::Delivered => publication.seq = publication.seq.wrapping_add(1),
				SendOutcome::Stalled => print(b"power-fixture: an update was dropped: the stream is full\n"),
				SendOutcome::Failed => {
					close(publication.stream);
					publication.stream = 0;
				}
			}
		}
	}

	fn updated(&mut self, which: u8, local: u32) {
		let publication = &self.publications[which as usize];
		if !publication.live || local >= 16 || publication.locals & (1u16 << local) == 0 {
			return;
		}
		let Some(state) = self.model.state(which, local) else { return };
		self.publications[which as usize].revision += 1;
		let update = ProviderUpdate { revision: self.publications[which as usize].revision, kind: ProviderUpdateKind::Updated, source: Some(ProviderSource { local, state }), gone: None };
		self.emit(which, &update);
	}

	// The snapshot a fresh update stream starts with: every live source at one revision, then its end.
	fn snapshot(&self, which: u8) -> Vec<ProviderUpdate> {
		let publication = &self.publications[which as usize];
		let revision = publication.revision;
		let mut updates: Vec<ProviderUpdate> = (0..16u32).filter(|local| publication.locals & (1u16 << *local) != 0).filter_map(|local| self.model.state(which, local).map(|state| ProviderUpdate { revision, kind: ProviderUpdateKind::Snapshot, source: Some(ProviderSource { local, state }), gone: None })).collect();
		updates.push(ProviderUpdate { revision, kind: ProviderUpdateKind::SnapshotEnd, source: None, gone: None });
		updates
	}

	fn log(&mut self, which: u8, command: &ProviderCommand) {
		if self.commands.len() >= MAX_COMMANDS {
			self.commands.remove(0);
		}
		self.commands.push(FixtureCommand { publication: which, local: command.local, kind: command.kind, outlet: command.outlet, on: command.on, delay_seconds: command.delay_seconds });
	}
}

// ------------------------------------------------------------------ the provider wire

// One publication's provider interface. `withheld` is set when the reply is to go unsent - the
// command still ARRIVED, and is logged as such, which is what lets the gate prove it was not replayed.
struct ProviderView<'a> {
	fixture: &'a mut Fixture,
	which: u8,
	withheld: bool,
}

impl power_provider::Service for ProviderView<'_> {
	fn updates(&mut self) -> Vec<ProviderUpdate> {
		self.fixture.snapshot(self.which)
	}

	fn command(&mut self, command: ProviderCommand) -> Result<ControlOutcome, Error> {
		self.fixture.log(self.which, &command);
		if self.fixture.withhold_commands > 0 {
			self.fixture.withhold_commands -= 1;
			self.withheld = true;
			print(b"power-fixture: a control reply is withheld\n");
		}
		// THE ACPI-SHAPED SOURCES ARE READ-ONLY, and say so if asked anyway.
		if self.which != UPS || command.local != 0 {
			return Err(Error::Unsupported);
		}
		match command.kind {
			proto::system::ProviderCommandKind::SetOutput => {
				let Some(outlet) = self.fixture.model.outlets.get_mut(command.outlet as usize) else { return Err(Error::Invalid) };
				*outlet = command.on;
			}
			proto::system::ProviderCommandKind::ScheduleOutputOff | proto::system::ProviderCommandKind::CancelOutputOff => {}
		}
		Ok(ControlOutcome::Done)
	}

	fn query(&mut self, local: u32) -> Result<SourceState, Error> {
		if self.fixture.withhold_queries > 0 {
			self.fixture.withhold_queries -= 1;
			self.withheld = true;
			print(b"power-fixture: a query reply is withheld\n");
		}
		let publication = &self.fixture.publications[self.which as usize];
		if publication.locals & (1 << local.min(15)) == 0 {
			return Err(Error::NotFound);
		}
		self.fixture.model.state(self.which, local).ok_or(Error::NotFound)
	}
}

// ------------------------------------------------------------------ the control endpoint

struct ControlView<'a> {
	fixture: &'a mut Fixture,
	serving: &'a mut common::Serving,
	bootstrap: u64,
	bind: &'a common::Bind,
}

impl power_fixture::Service for ControlView<'_> {
	fn set(&mut self, which: FixtureField, raw: u32) -> Result<(), Error> {
		let (publication, local) = self.fixture.model.set(which, raw);
		self.fixture.updated(publication, local);
		Ok(())
	}

	fn remove(&mut self, publication: u8, local: u32) -> Result<(), Error> {
		let Some(held) = self.fixture.publications.get_mut(publication as usize) else { return Err(Error::NotFound) };
		if local >= 16 || held.locals & (1 << local) == 0 {
			return Err(Error::NotFound);
		}
		held.locals &= !(1 << local);
		held.revision += 1;
		let update = ProviderUpdate { revision: held.revision, kind: ProviderUpdateKind::Removed, source: None, gone: Some(local) };
		self.fixture.emit(publication, &update);
		Ok(())
	}

	fn withdraw(&mut self, publication: u8) -> Result<(), Error> {
		let Some(held) = self.fixture.publications.get_mut(publication as usize) else { return Err(Error::NotFound) };
		if !held.live {
			return Err(Error::NotFound);
		}
		held.live = false;
		if held.stream != 0 {
			close(held.stream);
			held.stream = 0;
		}
		if !common::withdraw(self.bootstrap, self.bind, held.token) {
			return Err(Error::Io);
		}
		print(b"power-fixture: a publication is withdrawn\n");
		Ok(())
	}

	fn republish(&mut self, publication: u8) -> Result<(), Error> {
		let token = self.fixture.next_token;
		let Some(held) = self.fixture.publications.get_mut(publication as usize) else { return Err(Error::NotFound) };
		if held.live {
			return Err(Error::Invalid);
		}
		let Some((near, far)) = channel() else { return Err(Error::Exhausted) };
		if !self.serving.publish(token, near) {
			close(near);
			close(far);
			return Err(Error::Exhausted);
		}
		let name = if publication == UPS { UPS_NAME } else { ACPI_NAME };
		if !common::offer_named(self.bootstrap, self.bind, driver_protocol::provider::POWER_SOURCE, token, name, far) {
			return Err(Error::Io);
		}
		self.fixture.next_token = token + 1;
		// A NEW PUBLICATION, SO EVERY SOURCE AGAIN: PowerService sees other sources, under the new
		// generation DeviceManager gives it.
		held.token = token;
		held.live = true;
		held.locals = if publication == UPS { 0b1 } else { 0b111 };
		held.seq = 0;
		print(b"power-fixture: a publication is published again\n");
		Ok(())
	}

	fn commands(&mut self) -> Result<Vec<FixtureCommand>, Error> {
		Ok(self.fixture.commands.clone())
	}

	fn withhold(&mut self, commands: u32, queries: u32) -> Result<(), Error> {
		self.fixture.withhold_commands = commands;
		self.fixture.withhold_queries = queries;
		Ok(())
	}

	fn offer_extra(&mut self) -> Result<(), Error> {
		let token = self.fixture.next_token;
		self.fixture.next_token = token + 1;
		let Some((near, far)) = channel() else { return Err(Error::Exhausted) };
		// Never served: DeviceManager is to refuse it and close `far`, and the gate reads its refusal.
		close(near);
		if !common::offer_named(self.bootstrap, self.bind, driver_protocol::provider::POWER_SOURCE, token, EXTRA_NAME, far) {
			return Err(Error::Io);
		}
		print(b"power-fixture: offered a publication past its declaration\n");
		Ok(())
	}

	fn outlets(&mut self) -> Result<Vec<bool>, Error> {
		Ok(self.fixture.model.outlets.to_vec())
	}
}

// One request on one consumer connection. False when the connection is over.
fn serve(fixture: &mut Fixture, serving: &mut common::Serving, bootstrap: u64, bind: &common::Bind, token: u16, channel: u64, buf: &mut [u8]) -> bool {
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	if len < 2 {
		return true;
	}
	let op = u16::from_le_bytes([buf[0], buf[1]]);
	let mut reply = [0u8; 4096];
	let mut reply_handles = wire::Handles::new();
	if token == CONTROL_TOKEN {
		let mut view = ControlView { fixture: &mut *fixture, serving: &mut *serving, bootstrap, bind };
		if let Some(written) = power_fixture::dispatch(&mut view, &buf[..len], &mut handles, &mut reply, &mut reply_handles) {
			send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
		}
		return true;
	}
	let Some(which) = fixture.publication_of(token) else { return true };
	if op == power_provider::OP_UPDATES {
		let mut view = ProviderView { fixture: &mut *fixture, which, withheld: false };
		let Some((corr, items)) = power_provider::updates_open(&mut view, &buf[..len], &mut handles) else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		let publication = &mut fixture.publications[which as usize];
		if publication.stream != 0 {
			close(publication.stream);
		}
		publication.stream = producer;
		publication.seq = 0;
		for item in &items {
			fixture.emit(which, item);
		}
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		return true;
	}
	let mut view = ProviderView { fixture: &mut *fixture, which, withheld: false };
	let written = power_provider::dispatch(&mut view, &buf[..len], &mut handles, &mut reply, &mut reply_handles);
	if let Some(written) = written
		&& !view.withheld
	{
		send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
	}
	true
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, _resources) = common::handshake(bootstrap);
	let (Some((ups, ups_far)), Some((acpi, acpi_far)), Some((control, control_far))) = (channel(), channel(), channel()) else { exit() };
	common::online_named(
		bootstrap,
		&bind,
		b"driver.power-fixture: online (a HID-shaped UPS and ACPI-shaped power sources, from decoded data)",
		&[
			(driver_protocol::provider::POWER_SOURCE, ups_far, UPS_NAME),
			(driver_protocol::provider::POWER_SOURCE, acpi_far, ACPI_NAME),
			(driver_protocol::provider::FIXTURE_CONTROL, control_far, CONTROL_NAME),
		],
	);
	let mut serving = common::Serving::from_offers(&[(0, ups), (1, acpi), (CONTROL_TOKEN, control)]);
	let mut fixture = Fixture {
		model: Model::new(),
		publications: [
			Publication { token: 0, live: true, stream: 0, seq: 0, locals: 0b1, revision: 1 },
			Publication { token: 1, live: true, stream: 0, seq: 0, locals: 0b111, revision: 1 },
		],
		commands: Vec::new(),
		withhold_commands: 0,
		withhold_queries: 0,
		next_token: CONTROL_TOKEN + 1,
	};
	let mut buf = alloc::vec![0u8; 4096];
	loop {
		match common::wait_providers_or_answer(bootstrap, &bind, &mut serving, &[]) {
			None => {
				if common::stop_requested() {
					common::finish_stop(bootstrap, &bind, 0, true);
				}
				exit();
			}
			Some(common::ProviderReady::Connected(_)) | Some(common::ProviderReady::Device(_)) => {}
			Some(common::ProviderReady::Consumer(index)) => {
				let token = serving.token_at(index);
				let chan = serving.at(index);
				if !serve(&mut fixture, &mut serving, bootstrap, &bind, token, chan, &mut buf) {
					let token = serving.close_at(index);
					// The consumer is gone, and the stream it opened with it.
					if let Some(which) = fixture.publication_of(token) {
						let publication = &mut fixture.publications[which as usize];
						if publication.stream != 0 {
							close(publication.stream);
							publication.stream = 0;
						}
					}
					if !common::disconnected(bootstrap, &bind, token) {
						exit();
					}
				}
			}
		}
	}
}
