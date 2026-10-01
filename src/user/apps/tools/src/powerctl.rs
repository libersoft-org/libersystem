// powerctl - the processors' and the thermal policy's operator command: the power sources PowerService reports, the
// profile and why it is the one, each core's idle states with their entries and residency, its performance level and
// window, the idle injected and the live latency requests, each thermal zone with its trip points, and each fan; and the
// two things an operator may change - the profile, and a fan's curve where the platform leaves the fan to the operating
// system.
//
// THE ONE SHIPPING HOLDER OF `processor-power`, ProcessorPowerService's operator root, and a reader of `power-state`.
// Nothing it holds reaches `_CRT` or `_HOT`: those are not a policy anybody may turn off.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{ChargeState, CurvePoint, LaunchContext, PowerProfile, SourceKind, Tristate, ValueState, power, processor_power_admin};
use rt::*;
use tools::{parse_u64, split_args};

const USAGE: &[u8] = b"usage: powerctl [status | profile performance|balanced|power-saving | curve FAN CELSIUS:PERCENT...]\n";

fn say(text: &str) {
	print(text.as_bytes());
	print(b"\n");
}

fn usage() -> ! {
	print(USAGE);
	exit()
}

// Tenths of a kelvin, as Celsius with one decimal.
fn celsius(tenths_kelvin: u32) -> String {
	let tenths = i64::from(tenths_kelvin) - 2732;
	format!("{}.{} C", tenths / 10, (tenths % 10).abs())
}

fn tristate(value: Tristate) -> &'static str {
	match value {
		Tristate::Yes => "yes",
		Tristate::No => "no",
		Tristate::Unknown => "unknown",
	}
}

fn sources(state: u64) {
	if state == 0 {
		say("power sources: the power-state authority was not granted");
		return;
	}
	match power::Client::new(ChannelTransport { chan: state }).sources() {
		Some(Ok(sources)) if sources.is_empty() => say("power sources: none"),
		Some(Ok(sources)) => {
			say("power sources:");
			for source in &sources {
				let state = &source.state;
				let kind = match state.kind {
					SourceKind::Ac => "AC adapter",
					SourceKind::Battery => "battery",
					SourceKind::Ups => "UPS",
					SourceKind::Load => "load",
					SourceKind::ThermalZone => "thermal zone",
					SourceKind::UsbC => "USB-C supply",
				};
				let charge = match state.charge {
					ChargeState::Charging => ", charging",
					ChargeState::Discharging => ", discharging",
					ChargeState::Idle => ", idle",
					ChargeState::Unknown | ChargeState::Invalid => "",
				};
				let soc = if state.state_of_charge.state == ValueState::Known { format!(", {}.{:02} %", state.state_of_charge.value / 100, state.state_of_charge.value % 100) } else { String::new() };
				say(&format!("  {kind} {}: present {}, online {}{charge}{soc}", source.id.slot, tristate(state.present), tristate(state.online)));
			}
		}
		Some(Err(error)) => say(&format!("power sources: refused - {error:?}")),
		None => say("power sources: PowerService did not answer"),
	}
}

// EACH CORE'S IDLE STATES, read from the free per-core record the kernel answers: entries and residency per state.
fn idle_states(cpu: u32) {
	let mut info = CpuIdleInfo::default();
	if cpu_idle_info(u64::from(cpu), &mut info) < 0 {
		return;
	}
	for (at, state) in info.states.iter().take(info.state_count as usize).enumerate() {
		let entry = match state.entry {
			IDLE_ENTRY_HALT => "halt",
			IDLE_ENTRY_MWAIT => "mwait",
			IDLE_ENTRY_REGISTER => "register",
			IDLE_ENTRY_PSCI => "psci",
			_ => "sbi",
		};
		let why = match state.unenterable {
			IDLE_ENTERABLE => "",
			IDLE_NO_MWAIT => " (not entered: no MWAIT)",
			IDLE_COUNTER_MAY_STOP => " (not entered: no invariant TSC)",
			IDLE_TIMER_STOPS => " (not entered: its timer stops)",
			_ => " (not entered: it loses context)",
		};
		say(&format!("      state {at}: {entry}, {} us out, {} us residency to pay - {} entries, {} ms in it{why}", state.exit_latency_us, state.target_residency_us, state.entries, state.residency_ns / 1_000_000));
	}
	if info.latency_requests != 0 {
		say(&format!("      {} live latency request(s), the smallest {} us", info.latency_requests, info.latency_bound_us));
	}
}

fn status(admin: u64) {
	let answer = processor_power_admin::Client::new(ChannelTransport { chan: admin }).status();
	let status = match answer {
		Some(Ok(status)) => status,
		Some(Err(error)) => {
			say(&format!("processor power: refused - {error:?}"));
			return;
		}
		None => {
			say("processor power: ProcessorPowerService did not answer");
			return;
		}
	};
	say(&format!("profile: {} ({})", profile_name(status.profile), status.because));
	say("cores:");
	for core in &status.cores {
		let perf = if core.perf_levels == 0 { String::from("no performance table") } else { format!("level {} of {}, window {}..{}", core.perf_level, core.perf_levels, core.window_cap, core.window_floor) };
		let inject = if core.inject_permille == 0 { String::new() } else { format!(", {}.{} % idle injected", core.inject_permille / 10, core.inject_permille % 10) };
		say(&format!("  core {}: {} idle state(s), {perf}{inject}", core.cpu, core.idle_states));
		idle_states(core.cpu);
	}
	if status.zones.is_empty() {
		say("zones: none");
	} else {
		say("zones:");
	}
	for zone in &status.zones {
		let trip = |name: &str, value: Option<u32>| value.map(|value| format!(", {name} {}", celsius(value))).unwrap_or_default();
		let passive = if zone.passive_engaged { format!(" - passive cooling engaged, its processors held to {} %", zone.cap_percent) } else { String::new() };
		say(&format!("  {}: {}{}{}{}{passive}", zone.zone, celsius(zone.temperature), trip("_PSV", zone.passive), trip("_HOT", zone.hot), trip("_CRT", zone.critical)));
	}
	if status.fans.is_empty() {
		say("fans: none");
	} else {
		say("fans:");
	}
	for fan in &status.fans {
		let curve = if fan.curve.is_empty() { String::new() } else { format!(", curve {}", fan.curve.iter().map(|point| format!("{}:{}%", celsius(point.temperature), point.percent)).collect::<Vec<_>>().join(" ")) };
		say(&format!("  {}: level {}, {} rpm{curve}", fan.path, fan.control, fan.speed_rpm));
	}
}

fn profile_name(profile: PowerProfile) -> &'static str {
	match profile {
		PowerProfile::Performance => "performance",
		PowerProfile::Balanced => "balanced",
		PowerProfile::PowerSaving => "power-saving",
	}
}

fn set_profile(admin: u64, name: &[u8]) {
	let profile = match name {
		b"performance" => PowerProfile::Performance,
		b"balanced" => PowerProfile::Balanced,
		b"power-saving" => PowerProfile::PowerSaving,
		_ => usage(),
	};
	match processor_power_admin::Client::new(ChannelTransport { chan: admin }).set_profile(&profile) {
		Some(Ok(())) => say(&format!("powerctl: the profile is {}", profile_name(profile))),
		Some(Err(error)) => say(&format!("powerctl: the profile was refused - {error:?}")),
		None => say("powerctl: ProcessorPowerService did not answer"),
	}
}

// `CELSIUS:PERCENT`, the temperature in whole degrees.
fn point(text: &[u8]) -> Option<CurvePoint> {
	let at = text.iter().position(|&byte| byte == b':')?;
	let degrees = parse_u64(&text[..at])?;
	let percent = parse_u64(&text[at + 1..])?;
	(degrees <= 200 && percent <= 100).then(|| CurvePoint { temperature: (degrees * 10 + 2732) as u32, percent: percent as u8 })
}

fn set_curve(admin: u64, fan: &[u8], points: &[&[u8]]) {
	let Ok(fan) = core::str::from_utf8(fan) else { usage() };
	let curve: Vec<CurvePoint> = points.iter().map(|text| point(text).unwrap_or_else(|| usage())).collect();
	match processor_power_admin::Client::new(ChannelTransport { chan: admin }).set_fan_curve(fan, &curve) {
		Some(Ok(())) => say(&format!("powerctl: {fan} follows the curve")),
		Some(Err(error)) => say(&format!("powerctl: the curve was refused - {error:?}")),
		None => say("powerctl: ProcessorPowerService did not answer"),
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let state = recv_tagged(bootstrap, &mut buf, b"POWERSTATE").unwrap_or(0);
	let admin = recv_tagged(bootstrap, &mut buf, b"PROCESSORPOWER").unwrap_or(0);
	let args: Vec<&[u8]> = split_args(context.arguments.as_bytes()).collect();
	if admin == 0 && !matches!(args.as_slice(), [] | [b"status"]) {
		say("powerctl: the processor-power authority was not granted");
		exit();
	}
	match args.as_slice() {
		[] | [b"status"] => {
			sources(state);
			if admin == 0 {
				say("processor power: the processor-power authority was not granted");
			} else {
				status(admin);
			}
		}
		[b"profile", name] => set_profile(admin, name),
		[b"curve", fan, points @ ..] if !points.is_empty() => set_curve(admin, fan, points),
		_ => usage(),
	}
	exit()
}
