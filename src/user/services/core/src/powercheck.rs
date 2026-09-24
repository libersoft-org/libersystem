// powercheck - the in-guest scenario driver for the power gate. DEVELOPMENT-ONLY.
//
// It holds three grants: PowerService's state and control authorities, and the power fixture's
// control endpoint. It drives the service against the HID-shaped UPS and ACPI-shaped sources
// `power_fixture` publishes, and prints one verdict line per phase for the gate to read. What it
// proves is what a client can observe: exact canonical units, coherent subscriptions that coalesce
// and close as the milestone says, controls that reach one outlet or are refused, and a withheld
// reply that completes as indeterminate and is never replayed.
//
//   powercheck list [exact]  every source, in canonical units; `exact` checks the fixture's first values
//   powercheck control       an operator command reaches exactly one outlet, and nothing reached any before
//   powercheck denied        forged identities, a read-only source, an outlet and a delay out of range
//   powercheck watch         a change made during a subscription arrives after its snapshot
//   powercheck alarm         an alarm transition is delivered as one
//   powercheck coalesce      a slow reader receives the latest measurement, not every one
//   powercheck overflow      transitions a reader does not take close its subscription; a new one is current
//   powercheck withhold      a withheld reply completes as indeterminate, is not replayed, blocks conflicts
//                            until reconciled, and leaves the other provider served
//   powercheck extra         a publication past the fixture's declaration reaches nothing
//   powercheck remove        removal and withdrawal erase live state; a replacement is other sources

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{AlarmKind, ChangeKind, ControlOutcome, Error, FixtureField, LaunchContext, PowerChange, Provenance, ProviderCommandKind, SourceId, SourceKind, SourceSnapshot, TemperatureReference, TripKind, Tristate, ValueState, power, power_control, power_fixture};
use rt::*;
use services::capability_names::*;

const TICKS: u64 = 100;
const UPS: u8 = 0;
const ACPI: u8 = 1;

fn say(line: &[u8]) {
	print(b"powercheck: ");
	print(line);
	print(b"\n");
}

fn fail(line: &[u8]) -> ! {
	print(b"powercheck: FAIL ");
	print(line);
	print(b"\n");
	exit();
}

fn number(out: &mut Vec<u8>, value: i64) {
	if value < 0 {
		out.push(b'-');
	}
	unsigned(out, value.unsigned_abs());
}

fn unsigned(out: &mut Vec<u8>, mut n: u64) {
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

fn line(parts: &[&[u8]], values: &[i64]) -> Vec<u8> {
	let mut out = Vec::new();
	for (at, part) in parts.iter().enumerate() {
		out.extend_from_slice(part);
		if let Some(value) = values.get(at) {
			number(&mut out, *value);
		}
	}
	out
}

// ------------------------------------------------------------------ reading

fn sources(state: u64) -> Vec<SourceSnapshot> {
	match power::Client::new(ChannelTransport { chan: state }).sources() {
		Some(Ok(sources)) => sources,
		_ => fail(b"the source list was refused"),
	}
}

// Wait until PowerService has every fixture source: the providers bind and describe themselves a
// moment after boot.
fn all_sources(state: u64, count: usize) -> Vec<SourceSnapshot> {
	let deadline = clock() + 20 * TICKS;
	loop {
		let listed = sources(state);
		if listed.len() >= count {
			return listed;
		}
		if clock() >= deadline {
			fail(b"the fixture's sources did not all appear within twenty seconds");
		}
		sleep_until(clock() + TICKS / 4);
	}
}

fn find(sources: &[SourceSnapshot], kind: SourceKind) -> SourceSnapshot {
	match sources.iter().find(|source| source.state.kind == kind) {
		Some(source) => source.clone(),
		None => fail(b"a source kind the fixture publishes is missing"),
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
fn snapshot(stream: u64) -> (Vec<SourceSnapshot>, u64, u64) {
	let mut held = Vec::new();
	loop {
		match read(stream, clock() + 5 * TICKS) {
			Read::Frame(change) => match change.kind {
				ChangeKind::Snapshot => held.extend(change.source),
				ChangeKind::SnapshotEnd => return (held, change.epoch, change.revision),
				_ => fail(b"a change arrived before the snapshot ended"),
			},
			_ => fail(b"a subscription ended before its snapshot did"),
		}
	}
}

fn alarm(source: &SourceSnapshot, kind: AlarmKind, provenance: Provenance) -> Option<Tristate> {
	source.state.alarms.iter().find(|alarm| alarm.kind == kind && alarm.provenance == provenance).map(|alarm| alarm.state)
}

// ------------------------------------------------------------------ the phases

fn list(state: u64, exact: bool) {
	let listed = all_sources(state, 4);
	let (_, epoch, _) = {
		let stream = subscribe(state);
		let taken = snapshot(stream);
		close(stream);
		taken
	};
	// AN EPOCH IS A u64 AND IS PRINTED AS ONE: it is random, and as an `i64` half of them read negative.
	let mut said = b"epoch ".to_vec();
	unsigned(&mut said, epoch);
	said.extend_from_slice(b" sources ");
	unsigned(&mut said, listed.len() as u64);
	say(&said);
	let ups = find(&listed, SourceKind::Ups);
	let battery = find(&listed, SourceKind::Battery);
	let ac = find(&listed, SourceKind::Ac);
	let zone = find(&listed, SourceKind::ThermalZone);
	let u = &ups.state;
	say(&line(
		&[
			b"ups remaining ",
			b" uAh full ",
			b" uAh design ",
			b" uAh soc ",
			b" bp voltage ",
			b" uV power ",
			b" uW runtime ",
			b" s load ",
			b" bp temperature ",
			b" mC outlets ",
		],
		&[
			u.remaining.value as i64,
			u.full.value as i64,
			u.design.value as i64,
			u.state_of_charge.value as i64,
			u.voltage.value as i64,
			u.power.value,
			u.runtime.value as i64,
			u.load.value as i64,
			u.temperature.value,
			u.controls.outlets as i64,
		],
	));
	let b = &battery.state;
	say(&line(&[b"battery remaining ", b" uWh full ", b" uWh design ", b" uWh soc ", b" bp voltage ", b" uV power ", b" uW"], &[b.remaining.value as i64, b.full.value as i64, b.design.value as i64, b.state_of_charge.value as i64, b.voltage.value as i64, b.power.value]));
	let z = &zone.state;
	let trip = |kind: TripKind, index: u8| z.trips.iter().find(|trip| trip.kind == kind && trip.index == index).map_or(i64::MIN, |trip| trip.temperature.value);
	say(&line(&[b"thermal-zone temperature ", b" mC critical ", b" mC hot ", b" mC passive ", b" mC active0 ", b" mC active1 ", b" mC"], &[z.temperature.value, trip(TripKind::Critical, 0), trip(TripKind::Hot, 0), trip(TripKind::Passive, 0), trip(TripKind::Active, 0), trip(TripKind::Active, 1)]));
	if !exact {
		say(b"PASS list: four sources are enumerated");
		return;
	}
	// THE FIXTURE'S FIRST VALUES, IN CANONICAL UNITS, stated here from the decoded data it publishes:
	// 6300 of 7000 mAh is ninety percent; 1365 hundredths of a volt; 180 W delivered is out of the
	// source; 300 K is 26.85 C; 24 of 48 Wh is half; 5 W charging is into storage; 300.2 K is 27.05 C.
	let ups_ok = u.remaining.quantity == proto::system::Quantity::Charge && u.remaining.value == 6_300_000 && u.full.value == 7_000_000 && u.design.value == 7_200_000 && u.state_of_charge.value == 9000 && u.voltage.value == 13_650_000 && u.power.value == -180_000_000 && u.runtime.value == 2400 && u.load.value == 3600 && u.temperature.value == 26_850 && u.temperature.reference == TemperatureReference::Absolute && u.controls.set_output && u.controls.outlets == 2 && u.controls.schedule_off;
	let battery_ok = b.remaining.quantity == proto::system::Quantity::Energy && b.remaining.value == 24_000_000 && b.full.value == 48_000_000 && b.design.value == 50_000_000 && b.state_of_charge.value == 5000 && b.voltage.value == 11_100_000 && b.power.value == 5_000_000 && b.current.state == ValueState::Unsupported && !b.controls.set_output;
	let ac_ok = ac.state.present == Tristate::Yes && ac.state.online == Tristate::Yes && !ac.state.controls.set_output;
	let zone_ok = z.temperature.value == 27_050 && trip(TripKind::Critical, 0) == 95_050 && trip(TripKind::Hot, 0) == 90_050 && trip(TripKind::Passive, 0) == 80_050 && trip(TripKind::Active, 0) == 70_050 && trip(TripKind::Active, 1) == 60_050 && alarm(&zone, AlarmKind::OverTemperature, Provenance::Derived) == Some(Tristate::No);
	if !(ups_ok && battery_ok && ac_ok && zone_ok) {
		fail(b"list: a source's canonical values are not the ones its decoded data states");
	}
	say(b"PASS list: the HID-shaped UPS and the ACPI-shaped battery, adapter and zone enumerate with exact units");
}

fn set_output(control: u64, source: &SourceId, outlet: u8, on: bool) -> Result<ControlOutcome, Error> {
	match power_control::Client::with_deadline(ChannelTransport { chan: control }, clock() + 10 * TICKS).set_output(source, &outlet, &on) {
		Some(result) => result,
		None => fail(b"a control call did not complete"),
	}
}

fn commands(fixture: u64) -> Vec<proto::system::FixtureCommand> {
	match power_fixture::Client::new(ChannelTransport { chan: fixture }).commands() {
		Some(Ok(commands)) => commands,
		_ => fail(b"the fixture's command log could not be read"),
	}
}

fn outlets(fixture: u64) -> Vec<bool> {
	match power_fixture::Client::new(ChannelTransport { chan: fixture }).outlets() {
		Some(Ok(outlets)) => outlets,
		_ => fail(b"the fixture's outlets could not be read"),
	}
}

fn set(fixture: u64, field: FixtureField, value: u32) {
	if !matches!(power_fixture::Client::new(ChannelTransport { chan: fixture }).set(&field, &value), Some(Ok(()))) {
		fail(b"the fixture refused a change");
	}
}

fn control(state: u64, control: u64, fixture: u64) {
	let ups = find(&all_sources(state, 4), SourceKind::Ups);
	// NOTHING HAS REACHED THE FIXTURE BEFORE THIS: the read client's attempt, which the gate ran first,
	// arrived nowhere.
	if !commands(fixture).is_empty() {
		fail(b"control: a command reached the fixture before any operator sent one");
	}
	if set_output(control, &ups.id, 1, false) != Ok(ControlOutcome::Done) {
		fail(b"control: the operator's command did not complete");
	}
	let log = commands(fixture);
	let exact = log.len() == 1 && log[0].publication == UPS && log[0].local == 0 && log[0].kind == ProviderCommandKind::SetOutput && log[0].outlet == 1 && !log[0].on;
	if !exact || outlets(fixture) != [true, false] {
		fail(b"control: the command did not reach exactly outlet 1 of the UPS");
	}
	if set_output(control, &ups.id, 1, true) != Ok(ControlOutcome::Done) || outlets(fixture) != [true, true] {
		fail(b"control: the outlet could not be switched back");
	}
	say(b"PASS control: the operator's command reached outlet 1 of the UPS and nothing else, and nothing reached it before");
}

fn denied(state: u64, control: u64) {
	let listed = all_sources(state, 4);
	let ups = find(&listed, SourceKind::Ups);
	let zone = find(&listed, SourceKind::ThermalZone);
	let forged_generation = SourceId { generation: ups.id.generation.wrapping_add(1), ..ups.id.clone() };
	let forged_local = SourceId { local: 7, ..ups.id.clone() };
	let forged_binding = SourceId { binding_generation: ups.id.binding_generation.wrapping_add(1), ..ups.id.clone() };
	for forged in [&forged_generation, &forged_local, &forged_binding] {
		if set_output(control, forged, 0, false) != Err(Error::Denied) {
			fail(b"denied: a forged source identity was not refused");
		}
	}
	if set_output(control, &zone.id, 0, false) != Err(Error::Unsupported) {
		fail(b"denied: a read-only thermal zone accepted a control");
	}
	if set_output(control, &ups.id, 5, false) != Err(Error::Invalid) {
		fail(b"denied: an outlet the UPS does not have was accepted");
	}
	let too_long = power_control::Client::with_deadline(ChannelTransport { chan: control }, clock() + 10 * TICKS).schedule_output_off(&ups.id, &86_401);
	if too_long != Some(Err(Error::Invalid)) {
		fail(b"denied: a delay past a day was accepted");
	}
	say(b"PASS denied: forged generations, locals and bindings, a read-only source, an absent outlet and an over-long delay are refused");
}

fn watch(state: u64, fixture: u64) {
	let stream = subscribe(state);
	let (held, _, revision) = snapshot(stream);
	let battery = find(&held, SourceKind::Battery);
	set(fixture, FixtureField::BatteryRemaining, 23_000);
	let deadline = clock() + 5 * TICKS;
	loop {
		match read(stream, deadline) {
			Read::Frame(change) => {
				if let Some(source) = change.source.as_ref()
					&& change.kind == ChangeKind::Updated
					&& source.id == battery.id
					&& source.state.remaining.value == 23_000_000
				{
					if change.revision <= revision {
						fail(b"watch: a change carried a revision not after its subscription's snapshot");
					}
					say(b"PASS watch: a change made during a subscription arrived after its snapshot, at 23000000 uWh");
					close(stream);
					return;
				}
			}
			_ => fail(b"watch: the change made during the subscription never arrived"),
		}
	}
}

fn wait_for(stream: u64, what: &[u8], matches: impl Fn(&PowerChange) -> bool) {
	let deadline = clock() + 5 * TICKS;
	loop {
		match read(stream, deadline) {
			Read::Frame(change) if matches(&change) => return,
			Read::Frame(_) => {}
			_ => fail(what),
		}
	}
}

fn alarm_phase(state: u64, fixture: u64) {
	let stream = subscribe(state);
	snapshot(stream);
	set(fixture, FixtureField::UpsAcPresent, 0);
	wait_for(stream, b"alarm: the on-battery transition was not delivered", |change| change.source.as_ref().is_some_and(|source| source.state.kind == SourceKind::Ups && alarm(source, AlarmKind::OnBattery, Provenance::Reported) == Some(Tristate::Yes)));
	set(fixture, FixtureField::UpsAcPresent, 1);
	wait_for(stream, b"alarm: the transition back was not delivered", |change| change.source.as_ref().is_some_and(|source| source.state.kind == SourceKind::Ups && alarm(source, AlarmKind::OnBattery, Provenance::Reported) == Some(Tristate::No)));
	close(stream);
	say(b"PASS alarm: an on-battery transition and its return were each delivered");
}

fn coalesce(state: u64, fixture: u64) {
	let stream = subscribe(state);
	snapshot(stream);
	const CHANGES: u32 = 50;
	for step in 1..=CHANGES {
		set(fixture, FixtureField::UpsVoltage, 1300 + step);
	}
	// A slow reader: nothing is taken until well after the last change.
	sleep_until(clock() + TICKS);
	let mut seen: u32 = 0;
	let mut latest: u64 = 0;
	let mut last_revision: u64 = 0;
	loop {
		match read(stream, clock() + TICKS / 2) {
			Read::Frame(change) => {
				if change.revision <= last_revision {
					fail(b"coalesce: revisions went backwards");
				}
				last_revision = change.revision;
				if let Some(source) = change.source.as_ref()
					&& source.state.kind == SourceKind::Ups
				{
					seen += 1;
					latest = source.state.voltage.value;
				}
			}
			Read::Quiet => break,
			Read::Closed => fail(b"coalesce: ordinary measurements closed the subscription"),
		}
	}
	// 1350 hundredths of a volt is 13.5 V: the last value set.
	if seen == 0 || seen >= CHANGES || latest != 13_500_000 {
		fail(&line(&[b"coalesce: ", b" updates for 50 changes, latest "], &[seen as i64, latest as i64]));
	}
	say(&line(&[b"PASS coalesce: 50 changes arrived as ", b" updates, the latest 13500000 uV"], &[seen as i64]));
	close(stream);
}

fn overflow(state: u64, fixture: u64) {
	let stream = subscribe(state);
	// NOTHING IS READ while eighty-one alarm transitions are made: each one is kept, none coalesces,
	// and the queue fills.
	for step in 1..=81u32 {
		set(fixture, FixtureField::UpsOverload, step % 2);
	}
	let mut frames: u32 = 0;
	loop {
		match read(stream, clock() + 3 * TICKS) {
			Read::Frame(_) => frames += 1,
			Read::Closed => break,
			Read::Quiet => fail(b"overflow: transitions past the queue did not close the subscription"),
		}
	}
	close(stream);
	say(&line(&[b"overflow closed the subscription after ", b" frames"], &[frames as i64]));
	// A NEW SUBSCRIPTION IS CURRENT: its snapshot has the eighty-first transition, overload active.
	let fresh = subscribe(state);
	let (held, _, _) = snapshot(fresh);
	let ups = find(&held, SourceKind::Ups);
	if alarm(&ups, AlarmKind::Overload, Provenance::Reported) != Some(Tristate::Yes) {
		fail(b"overflow: a new subscription's snapshot is not the latest state");
	}
	close(fresh);
	set(fixture, FixtureField::UpsOverload, 0);
	say(b"PASS overflow: transitions a reader did not take closed its subscription, and a new one started from the latest state");
}

fn withhold(state: u64, control: u64, fixture: u64) {
	let listed = all_sources(state, 4);
	let ups = find(&listed, SourceKind::Ups);
	let battery = find(&listed, SourceKind::Battery);
	let before = commands(fixture).len();
	if !matches!(power_fixture::Client::new(ChannelTransport { chan: fixture }).withhold(&1, &1), Some(Ok(()))) {
		fail(b"withhold: the fixture refused to withhold");
	}
	let started = clock();
	if set_output(control, &ups.id, 0, false) != Ok(ControlOutcome::Indeterminate) {
		fail(b"withhold: an unanswered control did not complete as indeterminate");
	}
	let took = clock() - started;
	// FIVE SECONDS, AND BOUNDED: not before the deadline, and not long after it.
	if !(5 * TICKS - 10..=6 * TICKS + 50).contains(&took) {
		fail(&line(&[b"withhold: the indeterminate answer took ", b" ticks"], &[took as i64]));
	}
	// NO CONFLICTING CONTROL before reconciliation - and the query that reconciles is withheld too.
	if set_output(control, &ups.id, 0, true) != Err(Error::Again) {
		fail(b"withhold: a conflicting control was admitted before reconciliation");
	}
	// NO REPLAY: exactly one command arrived.
	if commands(fixture).len() != before + 1 {
		fail(b"withhold: the unanswered command was sent again");
	}
	// THE OTHER PROVIDER IS STILL SERVED while this one is silent.
	let stream = subscribe(state);
	snapshot(stream);
	set(fixture, FixtureField::BatteryRemaining, 22_000);
	let battery_id = battery.id.clone();
	wait_for(stream, b"withhold: the other provider was not served while this one was silent", move |change| change.source.as_ref().is_some_and(|source| source.id == battery_id && source.state.remaining.value == 22_000_000));
	close(stream);
	// The reconciliation query's own deadline passes: the UPS's controls are unavailable, and asking
	// starts a fresh query - which the fixture now answers.
	sleep_until(started + 11 * TICKS + 50);
	if set_output(control, &ups.id, 0, true) != Err(Error::Io) {
		fail(b"withhold: a source whose reconciliation went unanswered did not report its controls unavailable");
	}
	sleep_until(clock() + TICKS);
	if set_output(control, &ups.id, 0, true) != Ok(ControlOutcome::Done) {
		fail(b"withhold: the source did not come back once its provider answered a fresh query");
	}
	if commands(fixture).len() != before + 2 || outlets(fixture) != [true, true] {
		fail(b"withhold: the commands the fixture received are not the two that were sent");
	}
	say(&line(
		&[
			b"PASS withhold: an unanswered control completed as indeterminate after ",
			b" ticks, was never replayed, blocked a conflict until a fresh query reconciled it, and the other provider was served throughout",
		],
		&[took as i64],
	));
}

fn extra(state: u64, fixture: u64) {
	let count = all_sources(state, 4).len();
	if !matches!(power_fixture::Client::new(ChannelTransport { chan: fixture }).offer_extra(), Some(Ok(()))) {
		fail(b"extra: the fixture could not make the offer");
	}
	sleep_until(clock() + TICKS);
	if sources(state).len() != count {
		fail(b"extra: a publication past the declaration reached PowerService");
	}
	say(b"PASS extra: a publication past the fixture's declaration reached nothing");
}

fn remove(state: u64, fixture: u64) {
	let stream = subscribe(state);
	let (held, _, _) = snapshot(stream);
	let zone = find(&held, SourceKind::ThermalZone);
	let battery = find(&held, SourceKind::Battery);
	let fixture_client = || power_fixture::Client::new(ChannelTransport { chan: fixture });
	if !matches!(fixture_client().remove(&ACPI, &2), Some(Ok(()))) {
		fail(b"remove: the fixture could not remove the zone");
	}
	let zone_id = zone.id.clone();
	wait_for(stream, b"remove: the zone's removal was not delivered", move |change| change.kind == ChangeKind::Removed && change.gone.as_ref() == Some(&zone_id));
	if !matches!(fixture_client().withdraw(&ACPI), Some(Ok(()))) {
		fail(b"remove: the fixture could not withdraw its ACPI publication");
	}
	let battery_id = battery.id.clone();
	wait_for(stream, b"remove: the battery's removal was not delivered", move |change| change.kind == ChangeKind::Removed && change.gone.as_ref() == Some(&battery_id));
	let left = sources(state);
	if left.len() != 1 || left[0].state.kind != SourceKind::Ups {
		fail(b"remove: a withdrawn publication's state is still live");
	}
	if !matches!(fixture_client().republish(&ACPI), Some(Ok(()))) {
		fail(b"remove: the fixture could not publish again");
	}
	let back = all_sources(state, 4);
	let replacement = find(&back, SourceKind::Battery);
	if replacement.id.generation == battery.id.generation && replacement.id.slot == battery.id.slot {
		fail(b"remove: the replacement publication's sources have the old identity");
	}
	close(stream);
	say(b"PASS remove: a removed source and a withdrawn publication left no live state, and the replacement is other sources");
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let args: Vec<u8> = context.arguments.clone().into_bytes();
	// The grants, in the order PermissionManager walks its vocabulary.
	let state = recv_tagged(bootstrap, &mut buf, CAP_POWER_STATE).unwrap_or(0);
	let control_chan = recv_tagged(bootstrap, &mut buf, CAP_POWER_CONTROL).unwrap_or(0);
	let fixture = recv_tagged(bootstrap, &mut buf, b"FIXTURE").unwrap_or(0);
	if state == 0 || control_chan == 0 || fixture == 0 {
		fail(b"a grant this probe needs was not delivered");
	}
	let mut words = args.split(|&b| b == b' ');
	match words.next().unwrap_or(&[]) {
		b"list" => list(state, words.next() == Some(b"exact")),
		b"control" => control(state, control_chan, fixture),
		b"denied" => denied(state, control_chan),
		b"watch" => watch(state, fixture),
		b"alarm" => alarm_phase(state, fixture),
		b"coalesce" => coalesce(state, fixture),
		b"overflow" => overflow(state, fixture),
		b"withhold" => withhold(state, control_chan, fixture),
		b"extra" => extra(state, fixture),
		b"remove" => remove(state, fixture),
		_ => fail(b"usage: powercheck list [exact] | control | denied | watch | alarm | coalesce | overflow | withhold | extra | remove"),
	}
	exit();
}
