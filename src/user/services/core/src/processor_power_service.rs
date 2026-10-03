// ProcessorPowerService - the one holder of the kernel's processor-power authority, and the thermal policy.
//
// WHAT IT HOLDS. The `ProcessorPower` privilege, which ServiceManager hands it at every start - duplicated from the copy
// it keeps - and through which alone a core's idle and performance tables are installed, its window set, its idle
// injected and CPPC's energy preference written. The kernel holds the governors and writes every register; this
// service decides what the kernel is to install and where each core's window lies.
//
// WHERE ITS INPUT COMES FROM. The processors' power objects, as the firmware states them, from the ACPI service's
// `processor-firmware` root (PROCESSORS), each processor keyed by namespace path and `_UID`, the `_UID` naming the
// kernel's core through the MADT; their `Notify` 0x80 (`_PPC`), 0x81 (`_CST`, `_LPI`) and 0x82 (`_TPC`), each read again
// and acknowledged through `_OST`. The thermal zones' cooling halves (`thermal-zone`) and the fans (`cooling-device`)
// through a catalogue connection minted for those kinds alone. The power source, for the default profile, as an
// ordinary subscriber of PowerService's state (POWERSTATE).
//
// WHAT IT DECIDES is `service_logic::processor_policy`'s: the window where the profile, `_PPC` and the thermal cap meet;
// passive cooling by ACPI's equation, sampled every `_TSP` while engaged; each fan's level from the active trips and,
// where the platform leaves the fan to the operating system, the owner's curve; and the critical trips - past `_HOT`
// hibernation through SLEEP, past `_CRT` (or where hibernation is refused) the kernel's forced power-off armed through
// SYSPOWER and then ServiceManager's orderly one through SHUTDOWN. NO PROFILE, CURVE OR OPERATOR VERB REACHES THE LAST.
//
// WHAT IT SERVES: the operator root (CONTROL, `processor-power-admin`) - the status, the profile and a fan's curve.
//
// TRANSPARENT AND RECONSTRUCTIBLE. What it installed stays in the kernel across its restart, and the replacement installs
// it again from a fresh evaluation; the zones and fans are adopted again from the catalogue, a zone past `_CRT` found so
// in the first reading. The owner's profile and curves are this instance's - a restart returns to the defaults.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{ChangeKind, CorePower, Error, PowerProfile, ProcessorPowerStatus, SourceId, SourceState, power, processor_power_admin};
use rt::*;
use service_logic::processor_policy::{self, Profile};
use wire::Handles;

include!(concat!(env!("OUT_DIR"), "/roles_processor_power_service.rs"));

#[path = "processor_power_service/cores.rs"]
mod cores;
#[path = "processor_power_service/thermal.rs"]
mod thermal;

// How long one question to the ACPI service, a driver, PowerService or ServiceManager may take: the boot's busiest
// seconds included, when the ACPI service is handing every driver its node.
const ASK_TICKS: u64 = TICKS_PER_SECOND * 10;
// The operator connections minted from CONTROL or from one of them: PermissionManager's own, a tool's, and a
// replacement's while the one it replaces is still closing.
const MAX_OPERATORS: usize = 8;

fn say(text: &str) {
	let line = format!("ProcessorPowerService: {text}\n");
	print(line.as_bytes());
}

pub(crate) struct Service {
	privilege: u64,
	firmware: u64,
	firmware_stream: u64,
	catalogue: u64,
	power_stream: u64,
	sleep: u64,
	syspower: u64,
	shutdown: u64,
	control_root: u64,
	operators: Vec<u64>,
	cores: Vec<cores::Core>,
	thermal: thermal::Thermal,
	// The profile the operator chose, and what the power sources said last.
	chosen: Option<Profile>,
	on_battery: bool,
	sources: Vec<(SourceId, SourceState)>,
}

impl Service {
	fn profile(&self) -> Profile {
		self.chosen.unwrap_or_else(|| processor_policy::default_profile(self.on_battery))
	}

	fn because(&self) -> String {
		match self.chosen {
			Some(_) => String::from("chosen by the operator"),
			None if self.on_battery => String::from("the default on battery"),
			None => String::from("the default on line power"),
		}
	}

	// EVERY CORE'S WINDOW, INJECTION AND PREFERENCE, after anything they depend on changed: the profile, a `_PPC` or
	// `_TPC`, a zone's passive limit.
	fn apply(&mut self) {
		let profile = self.profile();
		for core in &mut self.cores {
			let limit = self.thermal.limit_of(&core.path);
			core.apply(self.privilege, profile, limit);
		}
	}

	// THE PROFILE MAY HAVE CHANGED: the windows, and each zone's `_SCP`.
	fn profile_changed(&mut self) {
		let profile = self.profile();
		say(&format!("the profile is {} - {}", profile_name(profile), self.because()));
		self.apply();
		self.thermal.cooling_policy(processor_policy::cooling_mode(profile));
	}

	// ------------------------------------------------------------------ the power source

	fn subscribe_power(&mut self, chan: u64) {
		if chan == 0 {
			say("no power-state connection - the profile is the line-power default");
			return;
		}
		// THE CONNECTION IS KEPT for as long as the stream: the subscription is the connection's.
		let mut client = power::Client::with_deadline(ChannelTransport { chan }, clock() + ASK_TICKS);
		match client.subscribe() {
			Some(Ok(stream)) => self.power_stream = stream,
			Some(Err(error)) => say(&format!("PowerService refused the subscription - {error:?} ({:?}); the profile is the line-power default", client.last_error())),
			None => say("PowerService did not answer the subscription - the profile is the line-power default"),
		}
	}

	fn drain_power(&mut self, buf: &mut [u8]) {
		let was = self.on_battery;
		loop {
			match try_recv_caps(self.power_stream, buf) {
				PolledCaps::Message { len, handles } => {
					let mut frame = handles;
					let Some(change) = power::subscribe_read(&buf[..len], &mut frame) else { continue };
					for &leftover in frame.as_slice() {
						close(leftover);
					}
					match change.kind {
						ChangeKind::Snapshot | ChangeKind::Added | ChangeKind::Updated => {
							if let Some(source) = change.source {
								self.sources.retain(|(id, _)| *id != source.id);
								self.sources.push((source.id, source.state));
							}
						}
						ChangeKind::Removed => {
							if let Some(gone) = change.gone {
								self.sources.retain(|(id, _)| *id != gone);
							}
						}
						ChangeKind::SnapshotEnd => {}
					}
				}
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					close(self.power_stream);
					self.power_stream = 0;
					say("PowerService's state stream ended - the profile stays as it is");
					break;
				}
			}
		}
		self.on_battery = power_model::canon::supply(self.sources.iter().map(|(_, state)| state)).on_battery;
		if self.on_battery != was && self.chosen.is_none() {
			self.profile_changed();
		}
	}

	// ------------------------------------------------------------------ the critical trips

	// PAST `_CRT`: ServiceManager's orderly power-off, with no deadline armed after it - a zone that goes on heating meets
	// the immediate threshold above `_CRT` (`power_model::acpi::IMMEDIATE_MARGIN`), and that one does not wait.
	pub(crate) fn critical_power_off(&self, zone: &str) {
		say(&format!("{zone} is past _CRT - the machine powers off in order"));
		if self.shutdown == 0 || !matches!(proto::system::system_shutdown::Client::with_deadline(ChannelTransport { chan: self.shutdown }, clock() + ASK_TICKS).power_off(), Some(Ok(()))) {
			say("the orderly power-off was not taken - the immediate threshold above _CRT stands");
		} else {
			say("the orderly power-off is under way");
		}
	}

	// PAST THE IMMEDIATE THRESHOLD ABOVE `_CRT`: the machine off at once, through SystemManager's `system-power` - no
	// service is stopped first. The zone's own driver does the same at the same crossing, whichever comes first.
	pub(crate) fn critical_immediate(&self, zone: &str) {
		let margin = power_model::acpi::IMMEDIATE_MARGIN;
		say(&format!("{zone} is {}.{} degrees past _CRT - the machine powers off at once", margin / 10, margin % 10));
		match self.syspower {
			0 => say("no system-power connection to power the machine off at once - the zone's driver does it"),
			power => match proto::system::system_power::Client::with_deadline(ChannelTransport { chan: power }, clock() + ASK_TICKS).power_off() {
				Some(Ok(())) => say("the machine did not stop, and SystemManager reported no error"),
				Some(Err(error)) => say(&format!("SystemManager refused the immediate power-off: {error:?}")),
				None => say("SystemManager did not answer the immediate power-off"),
			},
		}
	}

	// PAST `_HOT`: hibernation where it is set up, and the `_CRT` sequence where it is refused.
	pub(crate) fn critical_hibernate(&self, zone: &str) {
		say(&format!("{zone} is past _HOT - asks for hibernation"));
		let taken = self.sleep != 0 && matches!(proto::system::system_sleep::Client::with_deadline(ChannelTransport { chan: self.sleep }, clock() + ASK_TICKS).hibernate(&false, &proto::system::SleepReason::Thermal), Some(Ok(())));
		if !taken {
			say("hibernation was refused - the _CRT sequence instead");
			self.critical_power_off(zone);
		}
	}

	// ------------------------------------------------------------------ the operator

	fn status(&self) -> ProcessorPowerStatus {
		let mut info = CpuIdleInfo::default();
		let mut cores = Vec::new();
		let mut index = 0u64;
		while cores.len() < 64 && cpu_idle_info(index, &mut info) > 0 {
			cores.push(CorePower { cpu: info.cpu, idle_states: info.state_count, perf_levels: info.perf_levels, perf_level: info.perf_level, window_cap: info.window_cap, window_floor: info.window_floor, inject_permille: info.inject_permille, latency_requests: info.latency_requests });
			index += 1;
		}
		ProcessorPowerStatus { profile: profile_wire(self.profile()), because: self.because(), cores, zones: self.thermal.zone_status(), fans: self.thermal.fan_status() }
	}
}

fn profile_name(profile: Profile) -> &'static str {
	match profile {
		Profile::Performance => "performance",
		Profile::Balanced => "balanced",
		Profile::PowerSaving => "power saving",
	}
}

fn profile_wire(profile: Profile) -> PowerProfile {
	match profile {
		Profile::Performance => PowerProfile::Performance,
		Profile::Balanced => PowerProfile::Balanced,
		Profile::PowerSaving => PowerProfile::PowerSaving,
	}
}

impl processor_power_admin::Service for Service {
	fn status(&mut self) -> Result<ProcessorPowerStatus, Error> {
		Ok(Service::status(self))
	}

	fn set_profile(&mut self, profile: PowerProfile) -> Result<(), Error> {
		self.chosen = match profile {
			PowerProfile::Performance => Some(Profile::Performance),
			PowerProfile::Balanced => Some(Profile::Balanced),
			PowerProfile::PowerSaving => Some(Profile::PowerSaving),
			// THE CHOICE GIVEN BACK: the power source's default again.
			PowerProfile::Automatic => None,
		};
		self.profile_changed();
		Ok(())
	}

	fn set_fan_curve(&mut self, fan: String, curve: Vec<proto::system::CurvePoint>) -> Result<(), Error> {
		let curve: Vec<processor_policy::CurvePoint> = curve.iter().map(|point| processor_policy::CurvePoint { temperature: point.temperature, percent: point.percent }).collect();
		self.thermal.set_curve(&fan, curve)
	}
}

// THE OPERATOR ROOT AND ITS CONNECTIONS.
fn serve_control(service: &mut Service, handle: u64, buf: &mut [u8], reply: &mut [u8]) {
	let is_root = handle == service.control_root;
	let (len, mut handles) = match try_recv_caps(handle, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return,
		PolledCaps::Closed => {
			close(handle);
			if is_root {
				service.control_root = 0;
			} else {
				service.operators.retain(|&operator| operator != handle);
			}
			return;
		}
	};
	let op = if len >= 2 { u16::from_le_bytes([buf[0], buf[1]]) } else { 0 };
	// THE HEARTBEAT AND CONNECT ON EVERY CONNECTION, NOT ONLY THE ROOT. PermissionManager keeps the connection the
	// broker minted for it and mints each launch's grant from that one (`connect_or_resolve`); a connection that
	// read CONNECT as an operator's malformed request was closed, and every `processor-power` grant refused.
	if op == HEARTBEAT_OP || op == CONNECT_OP {
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		if op == HEARTBEAT_OP {
			send_blocking(handle, b"PONG", 0);
		} else {
			let theirs = if service.operators.len() < MAX_OPERATORS {
				channel().map(|(mine, theirs)| {
					service.operators.push(mine);
					theirs
				})
			} else {
				None
			};
			send_blocking(handle, &[], theirs.unwrap_or(0));
		}
		return;
	}
	if is_root {
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		return;
	}
	let mut reply_handles = Handles::new();
	let written = processor_power_admin::dispatch(service, &buf[..len], &mut handles, reply, &mut reply_handles);
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	match written {
		Some(written) => {
			send_caps_blocking(handle, &reply[..written], reply_handles.as_slice());
		}
		None => {
			close(handle);
			service.operators.retain(|&operator| operator != handle);
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (privilege, firmware, catalogue, power_state, sleep, syspower, shutdown, control_root) = (roles[0], roles[1], roles[2], roles[3], roles[4], roles[5], roles[6], roles[7]);
	let mut service = Service { privilege, firmware, firmware_stream: 0, catalogue, power_stream: 0, sleep, syspower, shutdown, control_root, operators: Vec::new(), cores: Vec::new(), thermal: thermal::Thermal::new(), chosen: None, on_battery: false, sources: Vec::new() };
	if privilege == 0 {
		say("no ProcessorPower privilege was handed over - no table is installed and no window set");
	}
	if syspower == 0 || shutdown == 0 {
		say("no system-power or system-shutdown client - past _CRT the zone's own driver arms the forced power-off alone");
	}
	service.subscribe_power(power_state);
	let mut buf = alloc::vec![0u8; 8192];
	service.drain_power_now(&mut buf);
	cores::discover(&mut service);
	service.thermal.subscribe(catalogue);
	service.apply();
	let online = format!("ProcessorPowerService: online - {} core(s), {} with firmware tables, profile {}", service.cores.len(), service.cores.iter().filter(|core| core.has_tables()).count(), profile_name(service.profile()));
	print(online.as_bytes());
	print(b"\n");
	send_blocking(bootstrap, online.as_bytes(), 0);
	serve(&mut service, bootstrap, &mut buf)
}

impl Service {
	// THE SUBSCRIPTION'S SNAPSHOT, taken before the first window is set, so the first profile is the power source's.
	fn drain_power_now(&mut self, buf: &mut [u8]) {
		if self.power_stream == 0 {
			return;
		}
		let deadline = clock() + ASK_TICKS;
		while clock() < deadline {
			if wait_any(&[self.power_stream], deadline) < 0 {
				break;
			}
			self.drain_power(buf);
			if self.power_stream == 0 || !self.sources.is_empty() {
				break;
			}
		}
	}
}

// THE LOOP: the processors' notifications, the zones' and fans' publications and readings, the power source and the
// operator.
fn serve(service: &mut Service, bootstrap: u64, buf: &mut [u8]) -> ! {
	let mut reply = alloc::vec![0u8; 8192];
	let mut control = bootstrap;
	loop {
		let mut waitset: Vec<u64> = Vec::new();
		for handle in [control, service.firmware_stream, service.power_stream, service.control_root] {
			if handle != 0 {
				waitset.push(handle);
			}
		}
		waitset.extend(service.operators.iter().copied());
		waitset.extend(service.thermal.handles());
		waitset.truncate(MAX_WAIT_HANDLES);
		let ready = wait_any(&waitset, 0);
		if ready < 0 {
			continue;
		}
		let handle = waitset[ready as usize];
		if handle == control {
			match try_recv_caps(control, buf) {
				PolledCaps::Message { handles, .. } => {
					for &leftover in handles.as_slice() {
						close(leftover);
					}
				}
				PolledCaps::Empty => {}
				PolledCaps::Closed => control = 0,
			}
			continue;
		}
		if handle == service.firmware_stream {
			cores::drain_notifications(service, buf);
			continue;
		}
		if handle == service.power_stream {
			service.drain_power(buf);
			continue;
		}
		if handle == service.control_root || service.operators.contains(&handle) {
			serve_control(service, handle, buf, &mut reply);
			continue;
		}
		let catalogue = service.catalogue;
		service.thermal.serve(handle, catalogue, buf);
		service.after_readings();
	}
}

impl Service {
	// WHAT THE READINGS ASKED FOR: the critical trips first, then the windows a passive limit moved.
	fn after_readings(&mut self) {
		for (zone, action) in self.thermal.take_actions() {
			match action {
				processor_policy::Critical::Immediate => self.critical_immediate(&zone),
				processor_policy::Critical::PowerOff => self.critical_power_off(&zone),
				processor_policy::Critical::Hibernate => self.critical_hibernate(&zone),
				processor_policy::Critical::Nothing => {}
			}
		}
		if self.thermal.take_changed() {
			self.apply();
		}
	}
}
