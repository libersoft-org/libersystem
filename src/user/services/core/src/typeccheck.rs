// typeccheck - the Type-C gates' live client and their operator. DEVELOPMENT-ONLY.
//
// It holds PowerService's read authority, TypeCService's read root and its operator root - the one holder of the last -
// and waits for what the gates need to see, or makes the requests they need made. Each wait prints its cue before it
// watches, so a harness that changes something on a cue changes it in front of a client already listening.
//
//   typeccheck await N WHAT [ARG]   subscribed: connector N reaching WHAT within a minute - `partner KIND`, `contract
//                                   MV` (a contract at MV millivolts, made), `current LEVEL` (default, medium, high),
//                                   `detached`, `silent`, `answering`, `entered SVID`, `data ROLE`, `refused KIND`
//   typeccheck source N WHAT [MV]   subscribed to PowerService: connector N's `usb-c` source `online` (at MV millivolts,
//                                   when given), `offline`, or with a reported `fault`
//   typeccheck beside-ac N          PowerService's sources: an ACPI adapter on line and connector N's `usb-c` source
//                                   online, two sources of two publications, neither merged into the other
//   typeccheck data N ROLE          ask for a data-role swap to `host` or `device`, and print the answer
//   typeccheck power N ROLE         ask for a power-role swap to `source` or `sink`, and print the answer
//   typeccheck enter N SVID VDO     ask for an alternate mode, and print the answer
//   typeccheck exit N SVID VDO      ask to leave one, and print the answer
//   typeccheck list                 every connector, one line each
//   typeccheck stalled              unread enumeration/subscribe replies close their clients; another reader works
//   typeccheck steady N SECONDS     subscribed: connector N attached throughout, never reported detached, for SECONDS
//   typeccheck cycle                the virtio-i2c binding disabled through the device policy - every `tcpci` binding
//                                   stopped as a lost dependency - then enabled again and every one online again

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::generated::liber::typec::v1 as typec;
use proto::system::{BindingState, ChangeKind as PowerChangeKind, LaunchContext, PolicyOutcome, PolicyVerb, SourceKind, Tristate, ValueState, device, device_policy_admin, power};
use rt::*;
use services::capability_names::*;
use typec::{Answer, ChangeKind, Connector, ConnectorId, DataRole, Outcome, PartnerKind, PowerRole, Refusal, TypecCurrent};

const WAIT: u64 = 60 * TICKS_PER_SECOND;

fn say(line: &str) {
	print(b"typeccheck: ");
	print(line.as_bytes());
	print(b"\n");
}

fn fail(line: &str) -> ! {
	say(&format!("FAIL {line}"));
	exit();
}

fn number(word: &str) -> u8 {
	word.parse().ok().filter(|number| (1..=16).contains(number)).unwrap_or_else(|| fail("a connector is 1 to 16"))
}

fn hex(word: &str) -> u32 {
	let digits = word.trim_start_matches("0x");
	u32::from_str_radix(digits, 16).unwrap_or_else(|_| fail("a mode's SVID and VDO are hexadecimal"))
}

fn summary(c: &Connector) -> String {
	let contract = match &c.contract {
		Some(contract) => format!("contract offer {} object {:#010x} operating {} max {}{}{}", contract.offer.position, contract.offer.object, contract.operating, contract.maximum, if contract.capability_mismatch { " mismatch" } else { "" }, if contract.in_transition { " in-transition" } else { "" }),
		None => String::from("no contract"),
	};
	format!("connector {} partner {:?} power {:?} data {:?} mode {:?} current {:?} offers {} {contract} modes {} answering {}", c.number, c.partner, c.power_role, c.data_role, c.operation_mode, c.typec_current, c.offers.len(), c.modes.iter().map(|mode| format!("{:#06x}{}", mode.svid, if mode.entered { "*" } else { "" })).collect::<Vec<_>>().join(","), c.answering)
}

// ------------------------------------------------------------------ TypeCService

fn subscribe_typec(connection: u64) -> u64 {
	match typec::typec::Client::new(ChannelTransport { chan: connection }).subscribe() {
		Some(Ok(stream)) => stream,
		_ => fail("TypeCService refused the subscription"),
	}
}

fn next_typec(stream: u64, deadline: u64) -> Option<typec::TypecChange> {
	let mut buf = alloc::vec![0u8; 4096];
	loop {
		match try_recv_caps(stream, &mut buf) {
			PolledCaps::Message { len, mut handles } => return typec::typec::subscribe_read(&buf[..len], &mut handles),
			PolledCaps::Closed => fail("the Type-C subscription closed"),
			PolledCaps::Empty => {
				if clock() >= deadline {
					return None;
				}
				let _ = wait_any(&[stream], deadline);
			}
		}
	}
}

fn partner_kind(word: &str) -> PartnerKind {
	match word {
		"none" => PartnerKind::None,
		"charger" => PartnerKind::Charger,
		"device" => PartnerKind::Device,
		"host" => PartnerKind::Host,
		"cable" => PartnerKind::PoweredCable,
		"audio" => PartnerKind::AudioAccessory,
		"debug" => PartnerKind::DebugAccessory,
		_ => fail("a partner is none, charger, device, host, cable, audio or debug"),
	}
}

fn refusal_kind(word: &str) -> Refusal {
	match word {
		"cannot" => Refusal::ConnectorCannot,
		"not-offered" => Refusal::NotOffered,
		"platform-modes" => Refusal::PlatformEntersModes,
		"platform" => Refusal::RefusedByPlatform,
		"transport" => Refusal::TransportDoesNot,
		_ => fail("a refusal is cannot, not-offered, platform-modes, platform or transport"),
	}
}

// Millivolts of a fixed power data object.
fn fixed_millivolts(object: u32) -> u32 {
	((object >> 10) & 0x3FF) * 50
}

// WHETHER CONNECTOR `c` IS WHAT THE GATE WAITS FOR.
fn reached(c: &Connector, what: &str, arg: Option<&str>) -> bool {
	match (what, arg) {
		("partner", Some(kind)) => c.partner == partner_kind(kind),
		("contract", Some(mv)) => c.contract.as_ref().is_some_and(|contract| !contract.in_transition && fixed_millivolts(contract.offer.object) == mv.parse::<u32>().unwrap_or_else(|_| fail("a contract is named by its millivolts"))),
		("current", Some(level)) => {
			let wanted = match level {
				"default" => TypecCurrent::Default,
				"medium" => TypecCurrent::Medium,
				"high" => TypecCurrent::High,
				_ => fail("a Type-C current is default, medium or high"),
			};
			c.typec_current == wanted && c.contract.is_none()
		}
		("detached", None) => c.partner == PartnerKind::None && c.contract.is_none(),
		("silent", None) => !c.answering,
		("answering", None) => c.answering,
		("entered", Some(svid)) => c.modes.iter().any(|mode| mode.entered && u32::from(mode.svid) == hex(svid)),
		("data", Some(role)) => c.data_role == Some(if role == "host" { DataRole::Host } else { DataRole::Device }),
		("refused", Some(kind)) => c.last_refusal.as_ref().is_some_and(|last| last.reason == refusal_kind(kind)),
		_ => fail("usage: typeccheck await N partner KIND | contract MV | current LEVEL | detached | silent | answering | entered SVID | data ROLE | refused KIND"),
	}
}

fn await_connector(connection: u64, wanted: u8, what: &str, arg: Option<&str>) {
	let stream = subscribe_typec(connection);
	let started = clock();
	let deadline = started + WAIT;
	let mut cued = false;
	let asked = match arg {
		Some(arg) => format!("{what} {arg}"),
		None => String::from(what),
	};
	loop {
		let Some(change) = next_typec(stream, deadline) else { fail(&format!("connector {wanted} did not reach {asked} within a minute")) };
		if change.kind == ChangeKind::SnapshotEnd && !cued {
			say(&format!("watching connector {wanted} for {asked}"));
			cued = true;
			continue;
		}
		let Some(snapshot) = change.connector else { continue };
		if snapshot.connector.number != wanted {
			continue;
		}
		if reached(&snapshot.connector, what, arg) {
			if !cued {
				say(&format!("watching connector {wanted} for {asked}"));
			}
			say(&format!("PASS await {wanted} {asked} after {} ms: {}", (clock() - started) * 1000 / TICKS_PER_SECOND, summary(&snapshot.connector)));
			close(stream);
			return;
		}
	}
}

// CONNECTOR `wanted` ATTACHED THROUGHOUT: no change reports it detached before `seconds` pass.
fn steady(connection: u64, wanted: u8, seconds: u64) {
	let stream = subscribe_typec(connection);
	let deadline = clock() + seconds * TICKS_PER_SECOND;
	let mut seen = 0usize;
	loop {
		let Some(change) = next_typec(stream, deadline) else { break };
		if change.kind == ChangeKind::SnapshotEnd {
			say(&format!("watching connector {wanted} stay attached for {seconds} s"));
			continue;
		}
		let Some(snapshot) = change.connector else { continue };
		if snapshot.connector.number != wanted {
			continue;
		}
		seen += 1;
		if snapshot.connector.partner == PartnerKind::None {
			fail(&format!("connector {wanted} was reported detached: {}", summary(&snapshot.connector)));
		}
	}
	close(stream);
	say(&format!("PASS steady {wanted}: attached throughout {seconds} s, {seen} update(s)"));
}

// ------------------------------------------------------------------ the controller cycled

fn bindings(device_client: u64) -> Vec<proto::system::BindingRecord> {
	match device::Client::new(ChannelTransport { chan: device_client }).bindings() {
		Some(Ok(bindings)) => bindings,
		_ => fail("the device bindings could not be read"),
	}
}

fn apply(policy: u64, index: u32, verb: PolicyVerb, what: &str) {
	let deadline = clock() + 10 * TICKS_PER_SECOND;
	loop {
		match device_policy_admin::Client::new(ChannelTransport { chan: policy }).apply(&index, &verb, "") {
			Some(Ok(PolicyOutcome::Accepted)) => return,
			Some(Ok(PolicyOutcome::Busy)) if clock() < deadline => sleep_until(clock() + TICKS_PER_SECOND / 4),
			_ => fail(what),
		}
	}
}

// Every `artifact` binding in `wanted`, and at least one of them, within twenty seconds.
fn await_states(device_client: u64, artifact: &str, wanted: BindingState, what: &str) {
	let deadline = clock() + 20 * TICKS_PER_SECOND;
	loop {
		let states: Vec<BindingState> = bindings(device_client).iter().filter(|binding| binding.artifact == artifact).map(|binding| binding.state).collect();
		if !states.is_empty() && states.iter().all(|state| *state == wanted) {
			return;
		}
		if clock() >= deadline {
			fail(&format!("{what} (the {artifact} bindings are {states:?})"));
		}
		sleep_until(clock() + TICKS_PER_SECOND / 4);
	}
}

fn cycle(device_client: u64, policy: u64) {
	let Some(controller) = bindings(device_client).iter().find(|binding| binding.artifact == "virtio_i2c").map(|binding| binding.index) else { fail("cycle: no virtio-i2c binding") };
	apply(policy, controller, PolicyVerb::Disable, "cycle: the virtio-i2c binding could not be disabled");
	await_states(device_client, "tcpci", BindingState::DependencyPending, "cycle: the port controller's binding did not stop as a lost dependency");
	say("the port controller's binding stopped as a lost dependency");
	apply(policy, controller, PolicyVerb::Enable, "cycle: the virtio-i2c binding could not be enabled again");
	await_states(device_client, "tcpci", BindingState::Online, "cycle: the port controller's binding did not bind again");
	say("PASS cycle: stopped as a lost dependency, and bound again");
}

// ------------------------------------------------------------------ PowerService

fn await_source(state: u64, wanted: u8, what: &str, millivolts: Option<&str>) {
	let stream = match power::Client::new(ChannelTransport { chan: state }).subscribe() {
		Some(Ok(stream)) => stream,
		_ => fail("PowerService refused the subscription"),
	};
	let deadline = clock() + WAIT;
	let mut buf = [0u8; 4096];
	loop {
		let change = loop {
			match try_recv_caps(stream, &mut buf) {
				PolledCaps::Message { len, mut handles } => break power::subscribe_read(&buf[..len], &mut handles),
				PolledCaps::Closed => fail("the power subscription closed"),
				PolledCaps::Empty => {
					if clock() >= deadline {
						fail(&format!("connector {wanted}'s usb-c source did not become {what} within a minute"));
					}
					let _ = wait_any(&[stream], deadline);
				}
			}
		};
		let Some(change) = change else { fail("a power change did not decode") };
		if change.kind == PowerChangeKind::SnapshotEnd {
			say(&format!("watching connector {wanted}'s usb-c source for {what}"));
			continue;
		}
		let Some(source) = change.source else { continue };
		if source.state.kind != SourceKind::UsbC || source.id.local != u32::from(wanted) - 1 {
			continue;
		}
		let s = &source.state;
		let hit = match what {
			"online" => s.online == Tristate::Yes && millivolts.is_none_or(|mv| s.voltage.state == ValueState::Known && s.voltage.value / 1000 == mv.parse::<u64>().unwrap_or(0)),
			"offline" => s.online == Tristate::No,
			"fault" => s.alarms.iter().any(|alarm| alarm.kind == proto::system::AlarmKind::SourceFault && alarm.state == Tristate::Yes),
			_ => fail("usage: typeccheck source N online [MV] | offline | fault"),
		};
		if hit {
			say(&format!("PASS source {wanted} {what}: present {:?} online {:?} voltage {:?} {} current {:?} {}", s.present, s.online, s.voltage.state, s.voltage.value, s.current.state, s.current.value));
			close(stream);
			return;
		}
	}
}

// TWO REPORTS, NOT ONE SOURCE: the adapter says the machine is on line power, the connector which partner supplies it.
fn beside_ac(state: u64, wanted: u8) {
	let deadline = clock() + WAIT;
	loop {
		let sources = match power::Client::new(ChannelTransport { chan: state }).sources() {
			Some(Ok(sources)) => sources,
			_ => fail("PowerService did not list its sources"),
		};
		let ac = sources.iter().find(|source| source.state.kind == SourceKind::Ac && source.state.online == Tristate::Yes);
		let usb = sources.iter().find(|source| source.state.kind == SourceKind::UsbC && source.id.local == u32::from(wanted) - 1 && source.state.online == Tristate::Yes);
		if let (Some(ac), Some(usb)) = (ac, usb) {
			if ac.id.slot == usb.id.slot {
				fail("the adapter and the connector were published as one publication");
			}
			say(&format!("PASS beside-ac: the adapter (slot {} local {}) on line and connector {wanted}'s usb-c source (slot {} local {}) online - two sources, merged with nothing; {} source(s) in all", ac.id.slot, ac.id.local, usb.id.slot, usb.id.local, sources.len()));
			return;
		}
		if clock() >= deadline {
			fail(&format!("no adapter on line beside connector {wanted}'s online usb-c source within a minute"));
		}
		sleep_until(clock() + TICKS_PER_SECOND / 2);
	}
}

// ------------------------------------------------------------------ requests

fn identity(connection: u64, wanted: u8) -> ConnectorId {
	let connectors = match typec::typec::Client::new(ChannelTransport { chan: connection }).connectors() {
		Some(Ok(connectors)) => connectors,
		_ => fail("TypeCService did not list its connectors"),
	};
	connectors.into_iter().find(|snapshot| snapshot.connector.number == wanted).map(|snapshot| snapshot.id).unwrap_or_else(|| fail(&format!("TypeCService holds no connector {wanted}")))
}

fn stalled_reader(reader: u64) {
	let before = typec::typec::Client::with_deadline(ChannelTransport { chan: reader }, clock() + WAIT).connectors().and_then(Result::ok).unwrap_or_else(|| fail("stalled: the initial connector list could not be read"));
	for op in [typec::typec::OP_CONNECTORS, typec::typec::OP_SUBSCRIBE] {
		let Some(stalled) = service_connect(reader) else { fail("stalled: a read connection could not be opened") };
		let mut request = [0u8; 6];
		request[..2].copy_from_slice(&op.to_le_bytes());
		let deadline = clock() + WAIT;
		let mut sent = 0u32;
		loop {
			request[2..].copy_from_slice(&sent.to_le_bytes());
			match try_send_outcome(stalled, &request, 0) {
				SendOutcome::Delivered => sent += 1,
				SendOutcome::Failed => break,
				SendOutcome::Stalled => {}
			}
			if clock() >= deadline {
				fail("stalled: unread replies blocked the service instead of closing the client");
			}
			yield_now();
		}
		close(stalled);
		let after = typec::typec::Client::with_deadline(ChannelTransport { chan: reader }, clock() + WAIT).connectors().and_then(Result::ok).unwrap_or_else(|| fail("stalled: another reader could not enumerate after the stalled connection closed"));
		if sent < 2 || after.len() != before.len() {
			fail("stalled: the connector list changed or the request queue was never exercised");
		}
	}
	say("PASS stalled: unread enumeration and subscribe replies closed their clients and another reader stayed serviceable");
}

fn report(asked: &str, answer: Option<Result<Answer, proto::system::Error>>) {
	match answer {
		Some(Ok(answer)) => match answer.outcome {
			Outcome::Done => say(&format!("answer {asked}: done")),
			Outcome::Refused => say(&format!("answer {asked}: refused {:?} error {:#x}", answer.reason, answer.error)),
			Outcome::Indeterminate => say(&format!("answer {asked}: indeterminate")),
		},
		Some(Err(error)) => say(&format!("answer {asked}: error {error:?}")),
		None => say(&format!("answer {asked}: unanswered")),
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
	let device_client = recv_tagged(bootstrap, &mut buf, b"DEVICE").unwrap_or(0);
	let policy = recv_tagged(bootstrap, &mut buf, b"DEVPOLICY").unwrap_or(0);
	let state = recv_tagged(bootstrap, &mut buf, CAP_POWER_STATE).unwrap_or(0);
	let reader = recv_tagged(bootstrap, &mut buf, CAP_TYPEC).unwrap_or(0);
	let operator = recv_tagged(bootstrap, &mut buf, CAP_TYPEC_CONTROL).unwrap_or(0);
	if device_client == 0 || policy == 0 || state == 0 || reader == 0 || operator == 0 {
		fail("a grant this probe needs was not delivered");
	}
	let words: Vec<&str> = context.arguments.split_whitespace().collect();
	let control = || typec::typec_control::Client::with_deadline(ChannelTransport { chan: operator }, clock() + 20 * TICKS_PER_SECOND);
	match words.as_slice() {
		["stalled"] => stalled_reader(reader),
		["await", n, what] => await_connector(reader, number(n), what, None),
		["await", n, what, arg] => await_connector(reader, number(n), what, Some(arg)),
		["source", n, what] => await_source(state, number(n), what, None),
		["source", n, what, mv] => await_source(state, number(n), what, Some(mv)),
		["beside-ac", n] => beside_ac(state, number(n)),
		["data", n, role] => {
			let id = identity(reader, number(n));
			let role = if *role == "host" { DataRole::Host } else { DataRole::Device };
			report(&format!("data {n}"), control().data_role_swap(&id, &role));
		}
		["power", n, role] => {
			let id = identity(reader, number(n));
			let role = if *role == "source" { PowerRole::Source } else { PowerRole::Sink };
			report(&format!("power {n}"), control().power_role_swap(&id, &role));
		}
		["enter", n, svid, vdo] => {
			let id = identity(reader, number(n));
			report(&format!("enter {n}"), control().enter_mode(&id, &(hex(svid) as u16), &hex(vdo)));
		}
		["exit", n, svid, vdo] => {
			let id = identity(reader, number(n));
			report(&format!("exit {n}"), control().exit_mode(&id, &(hex(svid) as u16), &hex(vdo)));
		}
		["list"] => match typec::typec::Client::new(ChannelTransport { chan: reader }).connectors() {
			Some(Ok(connectors)) => {
				for snapshot in &connectors {
					say(&summary(&snapshot.connector));
				}
				say(&format!("{} connector(s)", connectors.len()));
			}
			_ => fail("TypeCService did not list its connectors"),
		},
		["steady", n, seconds] => steady(reader, number(n), seconds.parse().unwrap_or_else(|_| fail("steady takes whole seconds"))),
		["cycle"] => cycle(device_client, policy),
		_ => fail("usage: typeccheck await N WHAT [ARG] | source N WHAT [MV] | beside-ac N | data N ROLE | power N ROLE | enter N SVID VDO | exit N SVID VDO | list | stalled | steady N SECONDS | cycle"),
	}
	exit();
}
