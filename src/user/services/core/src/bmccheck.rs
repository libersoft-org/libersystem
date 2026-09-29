// bmccheck - the IPMI gate's live clients. DEVELOPMENT-ONLY.
//
// It holds PowerService's read authority and the BMC service's `bmc` connection, and watches what the tool cannot: a
// BMC's temperatures as a PowerService subscriber sees them, and the BMC service's followed log. Each phase prints its
// cue before it watches, so the host changes nothing ahead of a client that is not listening.
//
//   bmccheck zones MC        subscribed: a BMC thermal zone reaching MC millidegrees, within two polling periods
//   bmccheck unknown         subscribed: every BMC thermal zone becoming unknown - the BMC went away - and then known
//   bmccheck follow BMC      the BMC service's follow of BMC's log: an entry added after the cue arriving
//   bmccheck booted          every BMC the service holds with this boot's event in its log - the administrative
//                            scenarios' wait, since a record added after a clear's preparation cancels it

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::generated::liber::bmc::v1 as bmc;
use proto::system::{ChangeKind, LaunchContext, PowerChange, SourceKind, ValueState, power};
use rt::*;
use services::capability_names::*;

// The driver polls its zones every five seconds: two periods, and a margin for the poll's own reads.
const TWO_PERIODS: u64 = 12 * TICKS_PER_SECOND;

fn say(line: &str) {
	print(b"bmccheck: ");
	print(line.as_bytes());
	print(b"\n");
}

fn fail(line: &str) -> ! {
	say(&format!("FAIL {line}"));
	exit();
}

fn subscribe(state: u64) -> u64 {
	match power::Client::new(ChannelTransport { chan: state }).subscribe() {
		Some(Ok(stream)) => stream,
		_ => fail("a subscription was refused"),
	}
}

fn read(stream: u64, deadline: u64) -> Option<PowerChange> {
	let mut buf = [0u8; 4096];
	loop {
		match try_recv_caps(stream, &mut buf) {
			PolledCaps::Message { len, mut handles } => return power::subscribe_read(&buf[..len], &mut handles),
			PolledCaps::Closed => fail("the subscription closed"),
			PolledCaps::Empty => {
				if clock() >= deadline {
					return None;
				}
				let _ = wait_any(&[stream], deadline);
			}
		}
	}
}

// Past the snapshot: the zones it held, by source.
fn snapshot(stream: u64) -> Vec<proto::system::SourceSnapshot> {
	let mut zones = Vec::new();
	loop {
		match read(stream, clock() + 5 * TICKS_PER_SECOND) {
			Some(change) if change.kind == ChangeKind::Snapshot => zones.extend(change.source.filter(|source| source.state.kind == SourceKind::ThermalZone)),
			Some(change) if change.kind == ChangeKind::SnapshotEnd => return zones,
			Some(_) => {}
			None => fail("the snapshot did not end"),
		}
	}
}

fn zones(state: u64, want: i64) {
	let stream = subscribe(state);
	let held = snapshot(stream);
	if held.is_empty() {
		fail("no thermal zone is published");
	}
	for zone in &held {
		say(&format!("zone {} at {} mC", zone.id.local, zone.state.temperature.value));
	}
	say("watching - change the reading now");
	let started = clock();
	loop {
		let Some(change) = read(stream, started + TWO_PERIODS) else { fail("the reading did not arrive within two polling periods") };
		let Some(source) = change.source.filter(|source| change.kind == ChangeKind::Updated && source.state.kind == SourceKind::ThermalZone) else { continue };
		if source.state.temperature.state == ValueState::Known && source.state.temperature.value == want {
			say(&format!("PASS zones: zone {} reached {want} mC {} ms after the cue", source.id.local, (clock() - started) * 1000 / TICKS_PER_SECOND));
			close(stream);
			return;
		}
	}
}

fn unknown(state: u64) {
	let stream = subscribe(state);
	let held = snapshot(stream);
	let count = held.len();
	if count == 0 {
		fail("no thermal zone is published");
	}
	say("watching - take the BMC away now");
	let mut unknown: Vec<u32> = Vec::new();
	let deadline = clock() + 90 * TICKS_PER_SECOND;
	while unknown.len() < count {
		let Some(change) = read(stream, deadline) else { fail("the zones did not all become unknown") };
		let Some(source) = change.source.filter(|source| change.kind == ChangeKind::Updated && source.state.kind == SourceKind::ThermalZone) else { continue };
		if source.state.temperature.state == ValueState::Unknown && !unknown.contains(&source.id.local) {
			unknown.push(source.id.local);
		}
	}
	say("every zone is unknown - bring the BMC back now");
	let deadline = clock() + 90 * TICKS_PER_SECOND;
	let mut known: Vec<u32> = Vec::new();
	while known.len() < count {
		let Some(change) = read(stream, deadline) else { fail("the zones did not come back") };
		let Some(source) = change.source.filter(|source| change.kind == ChangeKind::Updated && source.state.kind == SourceKind::ThermalZone) else { continue };
		if source.state.temperature.state == ValueState::Known && !known.contains(&source.id.local) {
			known.push(source.id.local);
		}
	}
	close(stream);
	say("PASS unknown: every zone went unknown with the BMC and came back with it");
}

// THE BOOT'S EVENT IN EVERY LOG: each BMC the service lists read until an entry of this system's boot is in it, within
// three minutes - the BMC service writes it once the boot has settled.
fn booted(connection: u64) {
	let mut client = bmc::bmc::Client::new(ChannelTransport { chan: connection });
	let deadline = clock() + 180 * TICKS_PER_SECOND;
	loop {
		let bmcs = match client.list() {
			Some(Ok(bmcs)) => bmcs,
			_ => fail("the BMC service did not list its BMCs"),
		};
		let mut waiting = Vec::new();
		for summary in bmcs.iter() {
			if !logged_boot(&mut client, &summary.name) {
				waiting.push(summary.name.clone());
			}
		}
		if !bmcs.is_empty() && waiting.is_empty() {
			for summary in bmcs.iter() {
				say(&format!("the boot's event is in the log of {}", summary.name));
			}
			return;
		}
		if clock() >= deadline {
			fail(&format!("the boot's event did not reach every log within three minutes (waiting on {} of {})", waiting.len(), bmcs.len()));
		}
		sleep_until(clock() + TICKS_PER_SECOND);
	}
}

fn logged_boot(client: &mut bmc::bmc::Client<ChannelTransport>, name: &str) -> bool {
	let mut first = 0u16;
	loop {
		let Some(Ok(page)) = client.sel_page(name, &first) else { return false };
		if page.entries.iter().any(|entry| entry.ours == bmc::Ours::Boot) {
			return true;
		}
		if page.next == 0xFFFF || page.entries.is_empty() {
			return false;
		}
		first = page.next;
	}
}

fn follow(connection: u64, name: &str) {
	let Some(stream) = bmc::bmc::Client::new(ChannelTransport { chan: connection }).sel_follow(name) else { fail("the follow was refused") };
	if stream == 0 {
		fail("the service follows no such BMC");
	}
	say("following - add an event now");
	let mut buf = [0u8; 1024];
	let deadline = clock() + 30 * TICKS_PER_SECOND;
	loop {
		match try_recv_caps(stream, &mut buf) {
			PolledCaps::Message { len, mut handles } => {
				let Some(entry) = bmc::bmc::sel_follow_read(&buf[..len], &mut handles) else { fail("a followed entry did not decode") };
				say(&format!("PASS follow: entry {:#06x} of type {:#04x} arrived through the service's follow", entry.id, entry.record_type));
				return;
			}
			PolledCaps::Closed => fail("the follow closed"),
			PolledCaps::Empty => {
				if clock() >= deadline {
					fail("no entry arrived within thirty seconds");
				}
				let _ = wait_any(&[stream], deadline);
			}
		}
	}
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
	let connection = recv_tagged(bootstrap, &mut buf, CAP_BMC).unwrap_or(0);
	if state == 0 || connection == 0 {
		fail("a grant this probe needs was not delivered");
	}
	let words: Vec<&str> = context.arguments.split_whitespace().collect();
	match words.as_slice() {
		["zones", want] => zones(state, want.parse().unwrap_or_else(|_| fail("zones takes millidegrees"))),
		["unknown"] => unknown(state),
		["follow", name] => follow(connection, name),
		["booted"] => booted(connection),
		_ => fail("usage: bmccheck zones MC | unknown | follow BMC | booted"),
	}
	exit();
}
