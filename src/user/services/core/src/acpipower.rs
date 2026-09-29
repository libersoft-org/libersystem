// acpipower - the ACPI gate's live power client. DEVELOPMENT-ONLY.
//
// It holds PowerService's read authority and nothing else, and reads what the `acpi_power` driver publishes from the
// fixture SSDT's battery, AC adapter and thermal zone - the values the gate writes into the harness's pages, read by
// the firmware's own methods. Each phase prints one verdict line for the gate to read, and each phase that waits on the
// host prints its cue first, so nothing is changed ahead of a client that is not listening.
//
//   acpipower list    the three sources, in canonical units, at the fixture's first values
//   acpipower watch   subscribed: the battery discharging, the adapter off line and the zone warmer, each arriving as
//                     an update after the snapshot once the host has changed the pages and raised the power line
//   acpipower storm   subscribed: the host raises the power line many times, changing the zone's reading before
//                     each; the reading it wrote last arrives, and the updates that carried the storm are counted

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{AlarmKind, ChangeKind, LaunchContext, PowerChange, Provenance, Quantity, SourceKind, SourceSnapshot, TemperatureReference, TripKind, Tristate, ValueState, power};
use rt::*;
use services::capability_names::*;

const TICKS: u64 = TICKS_PER_SECOND;
// THE STORM'S LAST READING, which the host writes after every other one: 330.0 K.
const STORM_FINAL_MC: i64 = 56_850;

fn say(line: &[u8]) {
	print(b"acpipower: ");
	print(line);
	print(b"\n");
}

fn fail(line: &[u8]) -> ! {
	print(b"acpipower: FAIL ");
	print(line);
	print(b"\n");
	exit();
}

fn number(out: &mut Vec<u8>, value: i64) {
	if value < 0 {
		out.push(b'-');
	}
	let mut n = value.unsigned_abs();
	let mut digits = [0u8; 20];
	let mut at = digits.len();
	loop {
		at -= 1;
		digits[at] = b'0' + (n % 10) as u8;
		n /= 10;
		if n == 0 {
			break;
		}
	}
	out.extend_from_slice(&digits[at..]);
}

// One line of named values: `prefix name value unit ...`.
fn say_fields(prefix: &[u8], fields: &[(&[u8], i64, &[u8])]) {
	let mut out = prefix.to_vec();
	for (name, value, unit) in fields {
		out.push(b' ');
		out.extend_from_slice(name);
		out.push(b' ');
		number(&mut out, *value);
		out.extend_from_slice(unit);
	}
	say(&out);
}

fn tristate(value: Tristate) -> i64 {
	match value {
		Tristate::Yes => 1,
		Tristate::No => 0,
		_ => -1,
	}
}

// ------------------------------------------------------------------ reading

fn sources(state: u64) -> Vec<SourceSnapshot> {
	match power::Client::new(ChannelTransport { chan: state }).sources() {
		Some(Ok(sources)) => sources,
		_ => fail(b"the source list was refused"),
	}
}

// THE THREE SOURCES, once the driver's three bindings have published: each binds after the ACPI service reports its node.
fn all_three(state: u64) -> (SourceSnapshot, SourceSnapshot, SourceSnapshot) {
	let deadline = clock() + 60 * TICKS;
	loop {
		let listed = sources(state);
		let one = |kind: SourceKind| {
			let mut of_kind = listed.iter().filter(|source| source.state.kind == kind);
			match (of_kind.next(), of_kind.next()) {
				(Some(source), None) => Some(source.clone()),
				(Some(_), Some(_)) => fail(b"more than one source of a kind the fixture describes once"),
				_ => None,
			}
		};
		if let (Some(battery), Some(ac), Some(zone)) = (one(SourceKind::Battery), one(SourceKind::Ac), one(SourceKind::ThermalZone)) {
			return (battery, ac, zone);
		}
		if clock() >= deadline {
			fail(b"the battery, the adapter and the zone did not all appear within sixty seconds");
		}
		sleep_until(clock() + TICKS / 4);
	}
}

fn subscribe(state: u64) -> u64 {
	match power::Client::new(ChannelTransport { chan: state }).subscribe() {
		Some(Ok(stream)) => stream,
		_ => fail(b"a subscription was refused"),
	}
}

enum Read {
	Frame(PowerChange),
	Closed,
	Quiet,
}

fn read(stream: u64, deadline: u64) -> Read {
	let mut buf = [0u8; 4096];
	loop {
		match try_recv_caps(stream, &mut buf) {
			PolledCaps::Message { len, mut handles } => {
				return match power::subscribe_read(&buf[..len], &mut handles) {
					Some(change) => Read::Frame(change),
					None => fail(b"a subscription frame did not decode"),
				};
			}
			PolledCaps::Closed => return Read::Closed,
			PolledCaps::Empty => {
				if clock() >= deadline {
					return Read::Quiet;
				}
				let _ = wait_any(&[stream], deadline);
			}
		}
	}
}

// The snapshot a subscription starts with, up to its end marker: the sources and the revision.
fn snapshot(stream: u64) -> (Vec<SourceSnapshot>, u64) {
	let mut held = Vec::new();
	loop {
		match read(stream, clock() + 5 * TICKS) {
			Read::Frame(change) => match change.kind {
				ChangeKind::Snapshot => held.extend(change.source),
				ChangeKind::SnapshotEnd => return (held, change.revision),
				_ => fail(b"a change arrived before the snapshot ended"),
			},
			_ => fail(b"a subscription ended before its snapshot did"),
		}
	}
}

fn trip(zone: &SourceSnapshot, kind: TripKind, index: u8) -> i64 {
	zone.state.trips.iter().find(|trip| trip.kind == kind && trip.index == index).map_or(i64::MIN, |trip| trip.temperature.value)
}

fn alarm(source: &SourceSnapshot, kind: AlarmKind, provenance: Provenance) -> Option<Tristate> {
	source.state.alarms.iter().find(|alarm| alarm.kind == kind && alarm.provenance == provenance).map(|alarm| alarm.state)
}

fn say_battery(prefix: &[u8], battery: &SourceSnapshot) {
	let b = &battery.state;
	say_fields(
		prefix,
		&[
			(b"present", tristate(b.present), b""),
			(b"remaining", b.remaining.value as i64, b" uWh"),
			(b"full", b.full.value as i64, b" uWh"),
			(b"design", b.design.value as i64, b" uWh"),
			(b"soc", b.state_of_charge.value as i64, b" bp"),
			(b"voltage", b.voltage.value as i64, b" uV"),
			(b"power", b.power.value, b" uW"),
		],
	);
}

fn say_ac(prefix: &[u8], ac: &SourceSnapshot) {
	say_fields(prefix, &[(b"present", tristate(ac.state.present), b""), (b"online", tristate(ac.state.online), b"")]);
}

fn say_zone(prefix: &[u8], zone: &SourceSnapshot) {
	say_fields(
		prefix,
		&[
			(b"temperature", zone.state.temperature.value, b" mC"),
			(b"critical", trip(zone, TripKind::Critical, 0), b" mC"),
			(b"hot", trip(zone, TripKind::Hot, 0), b" mC"),
			(b"passive", trip(zone, TripKind::Passive, 0), b" mC"),
			(b"active0", trip(zone, TripKind::Active, 0), b" mC"),
		],
	);
}

// ------------------------------------------------------------------ the phases

// THE FIRST VALUES, in canonical units, from the numbers the fixture's methods answer: `_BIX` in milliwatt hours, 50000
// design and 48000 last full; `_BST` charging at 5000 mW with 24000 mWh left at 11100 mV; `_PSR` 1 and no `_STA`, which
// the specification reads as present; `_TMP` 3002 tenths of a kelvin, 27.05 C; `_CRT` 3682, `_HOT` 3632, `_PSV` 3532 and
// `_AC0` 3432. AWAITED, for thirty seconds: after the ACPI service restarts, the bindings read their nodes again a
// moment after the new instance hands them over, and until then the last readings stand.
fn list(state: u64) {
	let deadline = clock() + 30 * TICKS;
	loop {
		let (battery, ac, zone) = all_three(state);
		let b = &battery.state;
		let battery_ok = b.present == Tristate::Yes && b.remaining.quantity == Quantity::Energy && b.remaining.value == 24_000_000 && b.full.value == 48_000_000 && b.design.value == 50_000_000 && b.state_of_charge.value == 5000 && b.voltage.value == 11_100_000 && b.power.value == 5_000_000 && b.current.state == ValueState::Unsupported && !b.controls.set_output && alarm(&battery, AlarmKind::LowCapacity, Provenance::Derived) == Some(Tristate::No);
		let ac_ok = ac.state.present == Tristate::Yes && ac.state.online == Tristate::Yes && !ac.state.controls.set_output;
		let z = &zone.state;
		let zone_ok = z.temperature.value == 27_050 && z.temperature.reference == TemperatureReference::Absolute && trip(&zone, TripKind::Critical, 0) == 95_050 && trip(&zone, TripKind::Hot, 0) == 90_050 && trip(&zone, TripKind::Passive, 0) == 80_050 && trip(&zone, TripKind::Active, 0) == 70_050 && alarm(&zone, AlarmKind::OverTemperature, Provenance::Derived) == Some(Tristate::No);
		let late = clock() >= deadline;
		if (battery_ok && ac_ok && zone_ok) || late {
			say_battery(b"battery", &battery);
			say_ac(b"ac", &ac);
			say_zone(b"thermal-zone", &zone);
		}
		if !battery_ok && late {
			fail(b"list: the battery's canonical values are not the ones its _BIX and _BST state");
		}
		if !ac_ok && late {
			fail(b"list: the adapter is not present and on line");
		}
		if !zone_ok && late {
			fail(b"list: the zone's reading or trips are not the ones its methods state");
		}
		if battery_ok && ac_ok && zone_ok {
			say(b"PASS list: the firmware's battery, adapter and zone enumerate with exact units");
			return;
		}
		sleep_until(clock() + TICKS / 4);
	}
}

// THE HOST'S CHANGE, as the pages then hold it: discharging at 7000 mW with 20000 mWh left, `_PSR` 0, `_TMP` 3182.
fn watch(state: u64) {
	let (battery, ac, zone) = all_three(state);
	let stream = subscribe(state);
	let (_, revision) = snapshot(stream);
	say(b"watching - change the pages and raise the power line now");
	let (mut discharged, mut offline, mut warmer) = (false, false, false);
	let deadline = clock() + 60 * TICKS;
	while !(discharged && offline && warmer) {
		let change = match read(stream, deadline) {
			Read::Frame(change) => change,
			Read::Closed => fail(b"watch: the subscription closed"),
			Read::Quiet => fail(b"watch: the change the host made did not arrive within sixty seconds"),
		};
		let Some(source) = change.source.as_ref().filter(|_| change.kind == ChangeKind::Updated) else { continue };
		if change.revision <= revision {
			fail(b"watch: a change carried a revision not after its subscription's snapshot");
		}
		if source.id == battery.id && source.state.power.value == -7_000_000 && source.state.remaining.value == 20_000_000 && !discharged {
			discharged = true;
			say_battery(b"updated battery", source);
		} else if source.id == ac.id && source.state.online == Tristate::No && !offline {
			offline = true;
			say_ac(b"updated ac", source);
		} else if source.id == zone.id && source.state.temperature.value == 45_050 && !warmer {
			warmer = true;
			say_zone(b"updated thermal-zone", source);
		}
	}
	close(stream);
	say(b"PASS watch: the battery discharging, the adapter off line and the zone at 45050 mC arrived as updates");
}

fn storm(state: u64) {
	let (_, _, zone) = all_three(state);
	let stream = subscribe(state);
	snapshot(stream);
	say(b"storming - raise the power line now");
	let mut updates: i64 = 0;
	let deadline = clock() + 120 * TICKS;
	loop {
		let change = match read(stream, deadline) {
			Read::Frame(change) => change,
			Read::Closed => fail(b"storm: the subscription closed"),
			Read::Quiet => fail(b"storm: the zone's last reading did not arrive"),
		};
		let Some(source) = change.source.as_ref().filter(|source| change.kind == ChangeKind::Updated && source.id == zone.id) else { continue };
		updates += 1;
		if source.state.temperature.value == STORM_FINAL_MC {
			break;
		}
	}
	close(stream);
	say_fields(b"storm:", &[(b"zone updates", updates, b","), (b"the last at", STORM_FINAL_MC, b" mC")]);
	say(b"PASS storm: the zone's last reading arrived after the storm");
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let state = recv_tagged(bootstrap, &mut buf, CAP_POWER_STATE).unwrap_or(0);
	if state == 0 {
		fail(b"the read authority was not granted");
	}
	match context.arguments.as_str() {
		"list" => list(state),
		"watch" => watch(state),
		"storm" => storm(state),
		_ => fail(b"usage: acpipower list | watch | storm"),
	}
	exit();
}
