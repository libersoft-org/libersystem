// THE CORES: each processor the ACPI service reports, its firmware tables installed in the kernel, and the window,
// injection and preference the policy sets.
//
// WHAT IS INSTALLED. The idle states (`_LPI`, else `_CST`) as the kernel's idle table - less any state that wants
// bus-master arbitration, which the kernel refuses and would refuse the whole table for - and the performance table:
// CPPC where `_CPC` has one, otherwise `_PSS` through `_PCT`'s registers, with `_PSD`'s domain and the throttling states
// through `_PTC`. A table the kernel refuses is said and the core keeps what it had; nothing here retries a refusal.
//
// A `Notify`: 0x80 reads `_PPC` again, 0x81 the idle states, 0x82 `_TPC` - the whole processor read again, whatever
// changed installed again, the window set again - and each is acknowledged through `_OST` where the processor has it.

use super::{ASK_TICKS, Service, say};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{Error, ProcessorPower, ProcessorRegister, processor_firmware};
use rt::*;
use service_logic::processor_policy::{self, Profile};

// The notifications a processor raises, acknowledged as handled.
const OST_SUCCESS: u32 = 0;

pub(crate) struct Core {
	pub(crate) path: String,
	cpu: u32,
	// What the firmware said last.
	read: ProcessorPower,
	// The performance table installed: its levels, how many are performance states, each one's share of the fastest,
	// and whether it names a preference register.
	levels: u32,
	performance_levels: u32,
	capacities: Vec<u32>,
	preference: bool,
	// What was set last, so an unchanged decision makes no call.
	applied: Option<(u32, u32)>,
	injected: u32,
	preferred: Option<u8>,
}

fn register_abi(register: &ProcessorRegister) -> abi::ProcessorRegister {
	abi::ProcessorRegister { space: register.space, bits: register.bits, _pad: [0; 6], address: register.address }
}

// THE IDLE TABLE, as the kernel takes it: shallowest first, a state wanting bus-master arbitration left out.
fn idle_table(path: &str, read: &ProcessorPower) -> Vec<abi::ProcessorIdleState> {
	let mut out = Vec::new();
	for (index, state) in read.idle.iter().enumerate() {
		if state.flags & abi::IDLE_BUS_MASTER_ARBITRATION != 0 {
			say(&format!("{path}: idle state {index} wants bus-master arbitration, which the kernel does not control - left out"));
			continue;
		}
		let (entry, parameter) = match state.entry {
			0 => (abi::IDLE_ENTRY_HALT, 0),
			1 => (abi::IDLE_ENTRY_MWAIT, state.hint),
			_ => (abi::IDLE_ENTRY_REGISTER, 0),
		};
		out.push(abi::ProcessorIdleState { entry, flags: state.flags & (abi::IDLE_LOSES_CONTEXT | abi::IDLE_STOPS_TIMER), parameter, exit_latency_us: state.latency_us, target_residency_us: state.residency_us, _pad: 0, register: register_abi(&state.register) });
		if out.len() == abi::PROCESSOR_MAX_IDLE_STATES {
			break;
		}
	}
	out
}

// THE PERFORMANCE TABLE, as the kernel takes it - None where the processor states neither CPPC nor `_PSS` with `_PCT`.
fn perf_table(read: &ProcessorPower) -> Option<abi::ProcessorPerfTable> {
	let mut table = abi::ProcessorPerfTable::default();
	if let Some(cppc) = &read.cppc {
		table.control_kind = abi::PERF_TABLE_CPPC;
		table.control = register_abi(&cppc.desired);
		table.status = cppc.minimum.as_ref().map(register_abi).unwrap_or_default();
		table.maximum = cppc.maximum.as_ref().map(register_abi).unwrap_or_default();
		table.preference = cppc.preference.as_ref().map(register_abi).unwrap_or_default();
		table.highest = cppc.highest;
		table.nominal = cppc.nominal;
		table.lowest = cppc.lowest;
	} else if let (false, Some(control), Some(status)) = (read.performance.is_empty(), &read.pct_control, &read.pct_status) {
		table.control_kind = abi::PERF_TABLE_STATES;
		table.control = register_abi(control);
		table.status = register_abi(status);
		for (at, state) in read.performance.iter().take(abi::PROCESSOR_MAX_PERF_STATES).enumerate() {
			table.states[at] = abi::ProcessorPerfState { core_mhz: state.core_mhz, power_mw: state.power_mw, latency_us: state.latency_us, control: state.control, status: state.status };
			table.state_count = at as u32 + 1;
		}
	} else {
		return None;
	}
	if let Some(domain) = &read.psd {
		table.domain = domain.domain;
		table.coordination = domain.coordination;
		table.processors = domain.processors;
	}
	if let (false, Some(control)) = (read.throttling.is_empty(), &read.ptc_control) {
		table.throttle_control = register_abi(control);
		for (at, state) in read.throttling.iter().take(abi::PROCESSOR_MAX_PERF_STATES).enumerate() {
			table.throttle[at] = abi::ProcessorThrottleState { percent: state.percent, latency_us: state.latency_us, control: state.control };
			table.throttle_count = at as u32 + 1;
		}
	}
	Some(table)
}

// EACH LEVEL'S SHARE OF THE FASTEST, in thousandths: a performance state's clock (CPPC's performance value) over the
// fastest one's, and a throttling level that share of the slowest state's.
fn capacities(table: &procpower::perf::Table) -> Vec<u32> {
	let performance = table.performance_levels().max(1);
	let value = |level: u32| match &table.control {
		procpower::perf::Control::States { states, .. } => u64::from(states[level as usize].core_mhz),
		procpower::perf::Control::Cppc { .. } => u64::from(table.setting(level).performance),
	};
	let top = value(0).max(1);
	let slowest = (value(performance - 1) * 1000 / top) as u32;
	(0..table.levels())
		.map(|level| {
			if level < performance {
				(value(level) * 1000 / top) as u32
			} else {
				let percent = table.throttle.as_ref().map_or(100, |throttle| throttle.states[(level - performance + 1) as usize].percent);
				slowest * percent / 100
			}
		})
		.collect()
}

impl Core {
	// Whether the firmware gave this core a table the kernel holds.
	pub(crate) fn has_tables(&self) -> bool {
		self.levels != 0 || !self.read.idle.is_empty()
	}

	// THE TABLES INSTALLED for the processor as it reads now; `before` the last reading, so what did not change is not
	// installed again.
	fn install(privilege: u64, path: &str, cpu: u32, read: ProcessorPower, before: Option<&Core>) -> Core {
		let mut core = Core { path: String::from(path), cpu, read, levels: 0, performance_levels: 0, capacities: Vec::new(), preference: false, applied: None, injected: u32::MAX, preferred: None };
		for why in &core.read.refused {
			say(&format!("{path}: {why}"));
		}
		// THE IDLE TABLE, installed where the firmware states one or a table stood before - a processor with neither keeps
		// the halt without a call.
		let states = idle_table(path, &core.read);
		if before.is_none_or(|before| before.read.idle != core.read.idle) && (!states.is_empty() || before.is_some()) {
			let answer = unsafe { syscall(abi::SYS_PROCESSOR_IDLE_TABLE, privilege, u64::from(cpu), states.as_ptr() as u64, states.len() as u64) } as i64;
			if answer < 0 {
				say(&format!("{path}: core {cpu}'s idle table of {} state(s) was refused - {answer}", states.len()));
			} else {
				say(&format!("{path}: core {cpu}'s idle table installed - {} state(s)", states.len()));
			}
		}
		// THE PERFORMANCE TABLE, installed again only where it changed: the kernel resets the window with a new one.
		let table = perf_table(&core.read);
		let same = before.is_some_and(|before| before.levels != 0 && perf_table(&before.read) == table);
		match &table {
			Some(raw) => {
				let answer = if same { 0 } else { (unsafe { syscall(abi::SYS_PROCESSOR_PERF_TABLE, privilege, u64::from(cpu), raw as *const abi::ProcessorPerfTable as u64, 0) }) as i64 };
				match (answer, procpower::records::perf_table_of(raw).ok()) {
					(0, Some(read)) => {
						core.levels = read.levels();
						core.performance_levels = read.performance_levels();
						core.capacities = capacities(&read);
						core.preference = matches!(read.control, procpower::perf::Control::Cppc { preference: Some(_), .. });
						if same {
							core.applied = before.and_then(|before| before.applied);
						} else {
							say(&format!("{path}: core {cpu}'s performance table installed - {} level(s)", core.levels));
						}
					}
					(answer, _) => say(&format!("{path}: core {cpu}'s performance table was refused - {answer}")),
				}
			}
			None => {
				if before.is_some_and(|before| before.levels != 0) {
					let _ = unsafe { syscall(abi::SYS_PROCESSOR_PERF_TABLE, privilege, u64::from(cpu), 0, 0) };
				}
			}
		}
		if let Some(before) = before {
			core.injected = before.injected;
			core.preferred = before.preferred;
		}
		core
	}

	// THE WINDOW, THE INJECTION AND THE PREFERENCE for `profile` and the thermal `limit` (thousandths of full
	// performance), each set only when it changed.
	pub(crate) fn apply(&mut self, privilege: u64, profile: Profile, limit: u32) {
		let (thermal, inject) = processor_policy::limit_level(limit, &self.capacities);
		if self.levels != 0 {
			// `_TPC` IS OBEYED ABOVE ANY PROFILE: no throttling state faster than it, which in one level order is the slowest
			// performance state throttled to it.
			let tpc = match self.read.tpc {
				0 => 0,
				tpc => (self.performance_levels - 1 + tpc).min(self.levels - 1),
			};
			let window = processor_policy::window(profile, self.levels, self.performance_levels, self.read.ppc, thermal.max(tpc));
			if self.applied != Some((window.cap, window.floor)) {
				let answer = unsafe { syscall(abi::SYS_PROCESSOR_PERF_WINDOW, privilege, u64::from(self.cpu), u64::from(window.cap), u64::from(window.floor)) } as i64;
				if answer < 0 {
					say(&format!("{}: core {}'s window {}..{} was refused - {answer}", self.path, self.cpu, window.cap, window.floor));
				} else {
					say(&format!("{}: core {}'s window is levels {}..{}", self.path, self.cpu, window.cap, window.floor));
					self.applied = Some((window.cap, window.floor));
				}
			}
		}
		if self.injected != inject {
			let answer = unsafe { syscall(abi::SYS_PROCESSOR_IDLE_INJECT, privilege, u64::from(self.cpu), u64::from(inject), 0) } as i64;
			if answer >= 0 {
				if inject != 0 || self.injected != u32::MAX {
					say(&format!("{}: core {} injects {}.{} % idle", self.path, self.cpu, inject / 10, inject % 10));
				}
				self.injected = inject;
			}
		}
		if self.preference {
			let value = processor_policy::energy_preference(profile);
			if self.preferred != Some(value) {
				let answer = unsafe { syscall(abi::SYS_PROCESSOR_PERF_PREFERENCE, privilege, u64::from(self.cpu), u64::from(value), 0) } as i64;
				if answer >= 0 {
					self.preferred = Some(value);
				}
			}
		}
	}
}

fn firmware(service: &Service) -> processor_firmware::Client<ChannelTransport> {
	processor_firmware::Client::with_deadline(ChannelTransport { chan: service.firmware }, clock() + ASK_TICKS)
}

// EVERY PROCESSOR THE ACPI SERVICE REPORTS that is a running core: read, and its tables installed.
pub(crate) fn discover(service: &mut Service) {
	if service.firmware == 0 {
		say("no processor-firmware connection - the kernel keeps the halt and no performance table");
		return;
	}
	let mut client = firmware(service);
	let processors = match client.processors() {
		Some(Ok(processors)) => processors,
		Some(Err(error)) => {
			say(&format!("the processors could not be listed - {error:?} ({:?})", client.last_error()));
			return;
		}
		None => {
			say("the ACPI service did not answer for the processors");
			return;
		}
	};
	let mut absent: Vec<String> = Vec::new();
	for id in processors {
		if id.cpu == u32::MAX {
			absent.push(id.path);
			continue;
		}
		match firmware(service).power(&id.path) {
			Some(Ok(read)) => {
				let core = Core::install(service.privilege, &id.path, id.cpu, read, None);
				service.cores.retain(|held| held.cpu != id.cpu);
				service.cores.push(core);
			}
			Some(Err(error)) => say(&format!("{}'s power objects could not be read - {error:?}", id.path)),
			None => say(&format!("the ACPI service did not answer for {}", id.path)),
		}
	}
	// THE PROCESSORS NO RUNNING CORE IS - a container, or a core this machine could add - said once, together.
	if !absent.is_empty() {
		say(&format!("{} processor(s) of the namespace are no running core, so nothing is installed for them ({} first)", absent.len(), absent[0]));
	}
	service.firmware_stream = firmware(service).notifications().unwrap_or(0);
	if service.firmware_stream == 0 {
		say("the processors' notifications could not be opened - a _PPC change is not followed");
	}
}

// THE PROCESSORS' `Notify` VALUES: each processor read again, what changed installed again, and acknowledged.
pub(crate) fn drain_notifications(service: &mut Service, buf: &mut [u8]) {
	loop {
		match try_recv_caps(service.firmware_stream, buf) {
			PolledCaps::Message { len, handles } => {
				let mut frame = handles;
				let Some(notification) = processor_firmware::notifications_read(&buf[..len], &mut frame) else { continue };
				for &leftover in frame.as_slice() {
					close(leftover);
				}
				notified(service, &notification.path, notification.value);
			}
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				close(service.firmware_stream);
				service.firmware_stream = 0;
				say("the processors' notifications ended - the ACPI service is gone; what is installed stands");
				return;
			}
		}
	}
}

fn notified(service: &mut Service, path: &str, value: u32) {
	let Some(at) = service.cores.iter().position(|core| core.path == path) else { return };
	let what = match value {
		0x80 => "its performance limit (_PPC)",
		0x81 => "its idle states",
		_ => "its throttling limit (_TPC)",
	};
	say(&format!("{path} notified {value:#x} - {what} read again"));
	let read = match firmware(service).power(path) {
		Some(Ok(read)) => read,
		Some(Err(error)) => {
			say(&format!("{path} could not be read again - {error:?}"));
			acknowledge(service, path, value, 1);
			return;
		}
		None => {
			say(&format!("the ACPI service did not answer for {path}"));
			return;
		}
	};
	let before = service.cores.remove(at);
	let cpu = before.cpu;
	let core = Core::install(service.privilege, path, cpu, read, Some(&before));
	service.cores.insert(at, core);
	service.apply();
	acknowledge(service, path, value, OST_SUCCESS);
}

// `_OST` WHERE THE PROCESSOR HAS IT: a processor without one is answered `not-found`, which is not a failure.
fn acknowledge(service: &Service, path: &str, value: u32, status: u32) {
	match firmware(service).ost(path, &value, &status) {
		Some(Ok(())) => say(&format!("{path}: _OST({value:#x}, {status}) acknowledged")),
		Some(Err(Error::NotFound)) | None => {}
		Some(Err(error)) => say(&format!("{path}: _OST failed - {error:?}")),
	}
}
