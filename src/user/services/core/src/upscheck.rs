// upscheck - the UPS gate's live power client and operator. DEVELOPMENT-ONLY.
//
// It holds PowerService's read and control authorities and reads the UPS the xHCI driver's HID Power Device class
// publishes - a USB device the harness builds in the host (`usb-gadget.sh setup ups`, `ups-sim.py` its firmware) - so
// every value it asserts is one the DEVICE reported, carried through the real PowerService. Each phase prints one
// verdict line for the gate to read.
//
//   upscheck list      the one UPS, in canonical units, as the device first reports it, with exactly the controls
//                      its report descriptor carries
//   upscheck control   subscribed: an output switch the UPS does not advertise refused by the service; a scheduled
//                      turn-off done, and the device's next report - mains gone, discharging, the on-battery alarm -
//                      arriving as an update; the cancel done, and the device back on mains in the next

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{AlarmKind, ChangeKind, ChargeState, ControlOutcome, Error, LaunchContext, PowerChange, SourceKind, SourceSnapshot, Tristate, ValueState, power, power_control};
use rt::*;
use services::capability_names::*;

const TICKS: u64 = TICKS_PER_SECOND;

fn say(line: &[u8]) {
	print(b"upscheck: ");
	print(line);
	print(b"\n");
}

fn fail(line: &[u8]) -> ! {
	print(b"upscheck: FAIL ");
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

fn tristate(value: Tristate) -> i64 {
	match value {
		Tristate::Yes => 1,
		Tristate::No => 0,
		_ => -1,
	}
}

fn on_battery(ups: &SourceSnapshot) -> Tristate {
	ups.state.alarms.iter().find(|alarm| alarm.kind == AlarmKind::OnBattery).map_or(Tristate::Unknown, |alarm| alarm.state)
}

// One line of the UPS's state.
fn say_ups(prefix: &[u8], ups: &SourceSnapshot) {
	let u = &ups.state;
	let mut out = prefix.to_vec();
	for (name, value, unit) in [
		(&b"online"[..], tristate(u.online), &b""[..]),
		(b"discharging", i64::from(u.charge == ChargeState::Discharging), b""),
		(b"soc", u.state_of_charge.value as i64, b" bp"),
		(b"runtime", u.runtime.value as i64, b" s"),
		(b"voltage", u.voltage.value as i64, b" uV"),
		(b"on-battery", tristate(on_battery(ups)), b""),
	] {
		out.push(b' ');
		out.extend_from_slice(name);
		out.push(b' ');
		number(&mut out, value);
		out.extend_from_slice(unit);
	}
	say(&out);
}

// THE ONE UPS, once the xHCI driver has bound the device and published it: a USB device enumerates after boot.
fn the_ups(state: u64) -> SourceSnapshot {
	let deadline = clock() + 120 * TICKS;
	loop {
		if let Some(Ok(sources)) = power::Client::new(ChannelTransport { chan: state }).sources() {
			let mut upses = sources.iter().filter(|source| source.state.kind == SourceKind::Ups);
			match (upses.next(), upses.next()) {
				(Some(ups), None) => return ups.clone(),
				(Some(_), Some(_)) => fail(b"more than one UPS is published, and the host built one"),
				_ => {}
			}
		}
		if clock() >= deadline {
			fail(b"no UPS was published within two minutes");
		}
		sleep_until(clock() + TICKS / 4);
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

// Past the snapshot a subscription starts with: its revision.
fn past_snapshot(stream: u64) -> u64 {
	loop {
		match read(stream, clock() + 5 * TICKS) {
			Read::Frame(change) => match change.kind {
				ChangeKind::Snapshot => {}
				ChangeKind::SnapshotEnd => return change.revision,
				_ => fail(b"a change arrived before the snapshot ended"),
			},
			_ => fail(b"a subscription ended before its snapshot did"),
		}
	}
}

// The next update of `ups` the live subscription carries that `wanted` accepts.
fn await_update(stream: u64, ups: &SourceSnapshot, after: u64, what: &[u8], wanted: impl Fn(&SourceSnapshot) -> bool) -> SourceSnapshot {
	let deadline = clock() + 20 * TICKS;
	loop {
		match read(stream, deadline) {
			Read::Frame(change) => {
				if change.kind == ChangeKind::Removed {
					fail(b"control: the UPS was removed while it was being driven");
				}
				let Some(source) = change.source.filter(|source| change.kind == ChangeKind::Updated && source.id == ups.id) else { continue };
				if change.revision <= after {
					fail(b"control: an update carried a revision not after its subscription's snapshot");
				}
				if wanted(&source) {
					return source;
				}
			}
			Read::Closed => fail(b"control: the subscription closed"),
			Read::Quiet => fail(what),
		}
	}
}

// ------------------------------------------------------------------ the phases

// AS THE DEVICE FIRST REPORTS ITSELF (`ups-sim.py`): on mains and charging at 80 % with an hour left, 13.80 V from its
// feature report with the unit exponent applied, and a report descriptor carrying the delayed turn-off and nothing
// that switches an outlet.
fn list(state: u64) {
	let ups = the_ups(state);
	say_ups(b"ups", &ups);
	let u = &ups.state;
	if !(u.present == Tristate::Yes && u.online == Tristate::Yes && u.charge == ChargeState::Charging) {
		fail(b"list: the UPS is not present, on mains and charging, as its first report says");
	}
	if !(u.state_of_charge.state == ValueState::Known && u.state_of_charge.value == 8000 && u.runtime.value == 3600 && u.voltage.value == 13_800_000) {
		fail(b"list: the UPS's charge, runtime or voltage is not the one its reports state");
	}
	if !(u.controls.schedule_off && u.controls.cancel_off && !u.controls.set_output) {
		fail(b"list: the UPS advertises controls other than the ones its report descriptor carries");
	}
	say(b"PASS list: the device's UPS reached PowerService with exact units and its own controls");
}

fn control(state: u64, operator: u64) {
	let ups = the_ups(state);
	let stream = match power::Client::new(ChannelTransport { chan: state }).subscribe() {
		Some(Ok(stream)) => stream,
		_ => fail(b"control: the subscription was refused"),
	};
	let revision = past_snapshot(stream);
	let operate = || power_control::Client::with_deadline(ChannelTransport { chan: operator }, clock() + 15 * TICKS);
	// 1. NOT ADVERTISED: refused by the service, with nothing sent to the device.
	match operate().set_output(&ups.id, &0, &false) {
		Some(Err(Error::Unsupported)) => say(b"an output switch the UPS does not advertise was refused"),
		_ => fail(b"control: an output switch the UPS does not advertise was not refused as unsupported"),
	}
	// 2. A TURN-OFF, SCHEDULED: done, and the device's next report says it went onto battery.
	match operate().schedule_output_off(&ups.id, &60) {
		Some(Ok(ControlOutcome::Done)) => {}
		_ => fail(b"control: the scheduled turn-off was not done"),
	}
	let gone = await_update(stream, &ups, revision, b"control: the device's report of going onto battery never arrived", |source| source.state.online == Tristate::No && on_battery(source) == Tristate::Yes);
	say_ups(b"updated ups", &gone);
	if !(gone.state.charge == ChargeState::Discharging && gone.state.state_of_charge.value == 7900 && gone.state.runtime.value == 1800) {
		fail(b"control: on battery, the UPS is not discharging at 79 % with half an hour left, as its report says");
	}
	// 3. AND CANCELLED: back on mains.
	match operate().cancel_output_off(&ups.id) {
		Some(Ok(ControlOutcome::Done)) => {}
		_ => fail(b"control: the cancel was not done"),
	}
	let back = await_update(stream, &ups, revision, b"control: the device's report of mains coming back never arrived", |source| source.state.online == Tristate::Yes);
	say_ups(b"updated ups", &back);
	if !(back.state.charge == ChargeState::Charging && back.state.runtime.value == 3600 && on_battery(&back) == Tristate::No) {
		fail(b"control: back on mains, the UPS is not charging with an hour left and no on-battery alarm");
	}
	close(stream);
	say(b"PASS control: the turn-off and its cancel reached the device, and its reports reached a live subscriber");
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	// The grants, in the order PermissionManager walks its vocabulary.
	let state = recv_tagged(bootstrap, &mut buf, CAP_POWER_STATE).unwrap_or(0);
	let operator = recv_tagged(bootstrap, &mut buf, CAP_POWER_CONTROL).unwrap_or(0);
	if state == 0 || operator == 0 {
		fail(b"a grant this probe needs was not delivered");
	}
	match context.arguments.as_str() {
		"list" => list(state),
		"control" => control(state, operator),
		_ => fail(b"usage: upscheck list | control"),
	}
	exit();
}
