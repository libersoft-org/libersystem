// admincheck - the in-guest requester and scenario driver for the administrative-path gate. DEVELOPMENT-ONLY.
//
// It holds an `admin-request` connection PermissionManager minted for this launch - the probe write on the
// probe executor's targets and nothing else - the probe executor's witness, the operator's journal view and
// AdminService's test controls. It has no way to approve anything: every grant it reports came from a person
// pressing keys on the emulated keyboard while the protected screen showed its request.
//
//   admincheck effects               the witness's counts, for the gate to compare across the run
//   admincheck unavailable           with the path to a person taken away, a request is declined
//   admincheck forged                a channel of its own and a malformed call reach no executor
//   admincheck wrong-target          a target the executor does not serve is declined, with nobody asked
//   admincheck storage               with every journal write failing, a request is declined
//   admincheck request CASE          one request, answered by the person at the keyboard:
//     approve      granted; executed once, completed; a replay refused; the witness wrote what was confirmed
//     refuse       declined
//     substitute   rewrites its own buffer after preparation; what is written is what was confirmed
//     replace      the target is replaced after preparation; declined at revalidation
//     expire       granted, and redeemed after the grant expired: refused
//     die          ends while its request is on the protected screen
//     block        an outcome that cannot be recorded blocks approvals until storage recovers
//     orphan       granted, and redeemed after AdminService was restarted: refused
//   admincheck delegate dead|live    sends its request connection and its grant down stdout, and ends or waits
//   admincheck hold approval|consumption
//                                    holds the journal's acknowledgment, then ends while it is held
//   admincheck journal               reads the whole journal back

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{AdminAction, AdminAnswer, AdminEvent, AdminGrant, AdminJournalFault, AdminProbeEffects, AdminProbeFault, AdminRequestArgs, AdminResult, Error, LaunchContext, admin_authority, admin_journal, admin_probe_witness, admin_request, admin_test};
use rt::*;
use services::capability_names::*;
use wire::{Handles, Reader, Transport, TransportError};

const TICKS: u64 = 100;
const TARGET: &str = "probe-target";
const PAYLOAD: u32 = 512;

// ON THE TERMINAL, WHICH IS STDERR: in a pipeline this program's stdout is the pipe to the next stage,
// and a line printed there is read as data and never seen.
fn say(line: &str) {
	eprint(b"admincheck: ");
	eprint(line.as_bytes());
	eprint(b"\n");
}

fn fail(line: &str) -> ! {
	eprint(b"admincheck: FAIL ");
	eprint(line.as_bytes());
	eprint(b"\n");
	exit();
}

struct Probe {
	witness: u64,
	request: u64,
	audit: u64,
	test: u64,
}

impl Probe {
	fn witness(&self) -> admin_probe_witness::Client<ChannelTransport> {
		admin_probe_witness::Client::with_deadline(ChannelTransport { chan: self.witness }, clock() + 5 * TICKS)
	}

	fn effects(&self) -> AdminProbeEffects {
		match self.witness().effects() {
			Some(Ok(effects)) => effects,
			_ => fail("the witness did not answer"),
		}
	}

	fn test(&self) -> admin_test::Client<ChannelTransport> {
		admin_test::Client::with_deadline(ChannelTransport { chan: self.test }, clock() + 5 * TICKS)
	}

	fn journal_fault(&self, fault: AdminJournalFault) {
		if !matches!(self.test().journal(&fault), Some(Ok(()))) {
			fail("the journal fault could not be set");
		}
	}

	fn held(&self) -> u32 {
		match self.test().held() {
			Some(Ok(held)) => held,
			_ => fail("the held acknowledgments could not be counted"),
		}
	}

	// Every retained record, oldest first.
	fn journal(&self) -> Vec<proto::system::AdminRecord> {
		let mut records = Vec::new();
		let mut from = 0;
		loop {
			let page = match admin_journal::Client::with_deadline(ChannelTransport { chan: self.audit }, clock() + 10 * TICKS).read(&from) {
				Some(Ok(page)) => page,
				_ => fail("the journal could not be read"),
			};
			if page.records.is_empty() {
				return records;
			}
			from = page.next;
			records.extend(page.records);
		}
	}
}

// A request captured and sent by hand, so this program can act while it waits for the person.
struct Capture {
	bytes: Vec<u8>,
	handles: Vec<u64>,
}

impl Transport for Capture {
	fn call(&mut self, request: &[u8], request_handles: &[u64], _reply_handles: &mut Handles, _deadline: u64) -> Result<Vec<u8>, TransportError> {
		self.bytes = request.to_vec();
		self.handles = request_handles.to_vec();
		Err(TransportError::TimedOut)
	}
	fn discard_handles(&mut self, _handles: &[u64]) {}
}

impl Transport for &mut Capture {
	fn call(&mut self, request: &[u8], request_handles: &[u64], reply_handles: &mut Handles, deadline: u64) -> Result<Vec<u8>, TransportError> {
		(**self).call(request, request_handles, reply_handles, deadline)
	}
	fn discard_handles(&mut self, handles: &[u64]) {
		(**self).discard_handles(handles)
	}
}

// The requester's payload: an object it keeps a WRITABLE mapping of, and a read-only copy of the handle
// that goes with the request.
struct Payload {
	handle: u64,
	addr: u64,
}

impl Payload {
	fn new(fill: u8) -> Payload {
		let handle = memory_object_create(4096);
		if handle < 0 {
			fail("the payload could not be allocated");
		}
		let handle = handle as u64;
		let Some(addr) = (unsafe { map_object(handle) }) else { fail("the payload could not be mapped") };
		let payload = Payload { handle, addr };
		payload.fill(fill);
		payload
	}

	fn fill(&self, byte: u8) {
		unsafe { core::ptr::write_bytes(self.addr as *mut u8, byte, PAYLOAD as usize) };
	}

	fn digest(&self) -> [u8; 32] {
		bootproto::sha256::digest(unsafe { core::slice::from_raw_parts(self.addr as *const u8, PAYLOAD as usize) })
	}

	fn shared(&self) -> u64 {
		let shared = duplicate(self.handle, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		if shared < 0 {
			fail("the payload could not be shared");
		}
		shared as u64
	}
}

fn args(target: &str, label: &str) -> AdminRequestArgs {
	AdminRequestArgs { action: AdminAction::ProbeWrite, target: String::from(target), parameters: alloc::vec![1, 2, 3], payload_length: PAYLOAD, label: String::from(label) }
}

// Send a request without waiting for its answer.
fn ask(connection: u64, args: &AdminRequestArgs, payload: &Payload) {
	let mut capture = Capture { bytes: Vec::new(), handles: Vec::new() };
	let shared = payload.shared();
	let _ = admin_request::Client::new(&mut capture).request(args, &shared);
	if capture.bytes.len() < 6 || !send_caps_blocking(connection, &capture.bytes, &capture.handles) {
		fail("the request could not be sent");
	}
}

// Wait for the answer to the one request in flight.
fn answer(connection: u64, seconds: u64) -> Result<AdminAnswer, Error> {
	let mut buf = alloc::vec![0u8; 8192];
	let until = clock() + seconds * TICKS;
	loop {
		if wait(connection, until) < 0 {
			fail("no answer came");
		}
		let (len, handles) = match try_recv_caps(connection, &mut buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => continue,
			PolledCaps::Closed => return Err(Error::Closed),
		};
		let mut reader = Reader::with_handle_list(&buf[..len], &handles);
		let decoded = (|| {
			reader.u32()?;
			if reader.tag()? { Some(Ok(AdminAnswer::read(&mut reader)?)) } else { Some(Err(Error::read(&mut reader)?)) }
		})();
		match decoded {
			Some(result) => return result,
			None => fail("an answer did not decode"),
		}
	}
}

fn granted(connection: u64, seconds: u64, case: &str) -> AdminGrant {
	match answer(connection, seconds) {
		Ok(AdminAnswer::Granted(grant)) => {
			say(&format!("granted ({case})"));
			grant
		}
		Ok(AdminAnswer::Declined) => fail(&format!("{case}: declined where a grant was expected")),
		Err(error) => fail(&format!("{case}: the request failed: {error:?}")),
	}
}

fn declined(connection: u64, seconds: u64, case: &str) {
	match answer(connection, seconds) {
		Ok(AdminAnswer::Declined) => {}
		Ok(AdminAnswer::Granted(grant)) => {
			close(grant.grant);
			fail(&format!("{case}: granted where a decline was expected"))
		}
		Err(error) => fail(&format!("{case}: the request failed: {error:?}")),
	}
}

fn execute(grant: u64) -> Option<Result<AdminResult, Error>> {
	admin_authority::Client::with_deadline(ChannelTransport { chan: grant }, clock() + 10 * TICKS).execute()
}

fn wait_for_person(case: &str) {
	say(&format!("waiting for a person ({case})"));
}

fn request(probe: &Probe, case: &str) {
	let before = probe.effects();
	let payload = Payload::new(0x5a);
	let confirmed = payload.digest();
	ask(probe.request, &args(TARGET, &format!("probe write, {case}")), &payload);
	match case {
		"approve" => {
			wait_for_person(case);
			let grant = granted(probe.request, 120, case);
			if grant.descriptor.payload_digest != confirmed || grant.descriptor.target != TARGET || grant.descriptor.payload_length != PAYLOAD {
				fail("approve: the descriptor is not the operation that was asked for");
			}
			if !matches!(execute(grant.grant), Some(Ok(AdminResult::Completed))) {
				fail("approve: the one attempt did not complete");
			}
			// REPLAY.
			if matches!(execute(grant.grant), Some(Ok(_))) {
				fail("approve: a second execute on the same grant was admitted");
			}
			let after = probe.effects();
			if after.writes != before.writes + 1 || after.dispatches != before.dispatches + 1 || after.digest != confirmed {
				fail(&format!("approve: expected exactly one matching write; writes {}->{}, dispatches {}->{}", before.writes, after.writes, before.dispatches, after.dispatches));
			}
			say("PASS approve: one confirmation, one matching write, and the replay reached nothing");
		}
		"refuse" => {
			wait_for_person(case);
			declined(probe.request, 120, case);
			unchanged(probe, &before, case);
			say("PASS refuse: declined, and nothing reached the executor");
		}
		"substitute" => {
			// THE SUBSTITUTION: once the executor has prepared, the requester rewrites its own buffer.
			sleep_until(clock() + 2 * TICKS);
			payload.fill(0xa5);
			wait_for_person(case);
			let grant = granted(probe.request, 120, case);
			if grant.descriptor.payload_digest != confirmed {
				fail("substitute: the descriptor names the substituted bytes");
			}
			if !matches!(execute(grant.grant), Some(Ok(AdminResult::Completed))) {
				fail("substitute: the attempt did not complete");
			}
			let after = probe.effects();
			if after.writes != before.writes + 1 || after.digest != confirmed || after.digest == payload.digest() {
				fail("substitute: what was written is not what was confirmed");
			}
			say("PASS substitute: the confirmed bytes were written, and the substituted ones never were");
		}
		"replace" => {
			sleep_until(clock() + 2 * TICKS);
			if !matches!(probe.witness().replace_target(), Some(Ok(_))) {
				fail("replace: the target could not be replaced");
			}
			wait_for_person(case);
			declined(probe.request, 120, case);
			unchanged(probe, &before, case);
			say("PASS replace: a target replaced after preparation was declined at revalidation");
		}
		"expire" => {
			wait_for_person(case);
			let grant = granted(probe.request, 120, case);
			sleep_until(clock() + 31 * TICKS);
			if matches!(execute(grant.grant), Some(Ok(_))) {
				fail("expire: an expired grant was admitted");
			}
			unchanged(probe, &before, case);
			say("PASS expire: a grant redeemed after its thirty seconds reached nothing");
		}
		"die" => {
			wait_for_person(case);
			sleep_until(clock() + 8 * TICKS);
			say("ends with its request on the protected screen");
			exit();
		}
		"block" => block(probe, &before, &payload),
		"orphan" => {
			wait_for_person(case);
			let grant = granted(probe.request, 120, case);
			say("holds a grant; restart AdminService now");
			sleep_until(clock() + 25 * TICKS);
			if matches!(execute(grant.grant), Some(Ok(_))) {
				fail("orphan: a grant outlived the instance that issued it");
			}
			unchanged(probe, &before, case);
			say("PASS orphan: a grant from an instance that ended reached nothing");
		}
		_ => fail("usage: admincheck request approve | refuse | substitute | replace | expire | die | block | orphan"),
	}
}

fn unchanged(probe: &Probe, before: &AdminProbeEffects, case: &str) {
	let after = probe.effects();
	if after.writes != before.writes || after.dispatches != before.dispatches {
		fail(&format!("{case}: the executor was reached; writes {}->{}, dispatches {}->{}", before.writes, after.writes, before.dispatches, after.dispatches));
	}
}

// AN OUTCOME THAT CANNOT BE RECORDED. The executor performs the write and loses its answer; the journal
// fails before the unknown outcome is written; approvals stop until storage recovers.
fn block(probe: &Probe, before: &AdminProbeEffects, _payload: &Payload) {
	wait_for_person("block");
	let grant = granted(probe.request, 120, "block");
	let since = probe.journal().last().map_or(0, |record| record.sequence);
	if !matches!(probe.witness().inject(&AdminProbeFault::LoseReply), Some(Ok(()))) {
		fail("block: the fault could not be injected");
	}
	let mut capture = Capture { bytes: Vec::new(), handles: Vec::new() };
	let _ = admin_authority::Client::new(&mut capture).execute();
	if !send_caps_blocking(grant.grant, &capture.bytes, &capture.handles) {
		fail("block: the execute could not be sent");
	}
	// THE CONSUMPTION IS COMMITTED, then storage goes.
	let until = clock() + 5 * TICKS;
	while !probe.journal().iter().any(|record| record.sequence > since && record.event == AdminEvent::Consumed) {
		if clock() >= until {
			fail("block: the consumption was never recorded");
		}
		sleep_until(clock() + TICKS / 10);
	}
	probe.journal_fault(AdminJournalFault::Fail);
	let mut buf = [0u8; 64];
	if wait(grant.grant, clock() + 10 * TICKS) < 0 {
		fail("block: the redemption was never answered");
	}
	let _ = try_recv_caps(grant.grant, &mut buf);
	let after = probe.effects();
	if after.writes != before.writes + 1 {
		fail("block: the write was not performed exactly once");
	}
	// BLOCKED: the next request is declined with nobody asked. THE LAST ONE LETS GO OF THIS CONNECTION ON THE
	// SERVICE'S NEXT PASS: its unknown outcome is tried against the journal first, and until then the connection
	// still has a request and is answered `again` - asked again for that reason alone, within the bound.
	let payload = Payload::new(0x33);
	let until = clock() + 10 * TICKS;
	loop {
		ask(probe.request, &args(TARGET, "while an outcome is unrecorded"), &payload);
		match answer(probe.request, 30) {
			Ok(AdminAnswer::Declined) => break,
			Err(Error::Again) if clock() < until => sleep_until(clock() + TICKS / 10),
			Ok(AdminAnswer::Granted(grant)) => {
				close(grant.grant);
				fail("block: granted where a decline was expected");
			}
			Err(error) => fail(&format!("block: the request failed: {error:?}")),
		}
	}
	probe.journal_fault(AdminJournalFault::None);
	// RECOVERY: the ring writes what storage refused, and the unknown outcome is on the volume.
	let until = clock() + 10 * TICKS;
	while !probe.journal().iter().any(|record| record.sequence > since && record.event == AdminEvent::OutcomeUnknown) {
		if clock() >= until {
			fail("block: the unrecorded outcome was never written once storage returned");
		}
		sleep_until(clock() + TICKS / 5);
	}
	say("PASS block: an outcome storage refused blocked approvals, and was written once storage returned");
}

fn delegate(probe: &Probe, live: bool) {
	let payload = Payload::new(0x42);
	ask(probe.request, &args(TARGET, "a delegated probe write"), &payload);
	wait_for_person("delegate");
	let grant = granted(probe.request, 120, "delegate");
	// BOTH ENDPOINTS GO: the request connection and the unused grant. Neither rebinds ownership.
	if !send_caps_blocking(stdout(), b"ADMINDELEGATE", &[probe.request, grant.grant]) {
		fail("delegate: the endpoints could not be sent");
	}
	if live {
		say("delegated both endpoints, and stays alive");
		sleep_until(clock() + 20 * TICKS);
		say("ends after its delegate had the grant");
	} else {
		say("delegated both endpoints, and ends");
	}
	exit();
}

fn hold(probe: &Probe, consumption: bool) {
	let payload = Payload::new(0x77);
	let since = probe.journal().last().map_or(0, |record| record.sequence);
	ask(probe.request, &args(TARGET, "a probe write whose record is held"), &payload);
	if !consumption {
		// THE REQUEST IS RECORDED FIRST, and only then are acknowledgments held: the one held is the approval's.
		let until = clock() + 10 * TICKS;
		while !probe.journal().iter().any(|record| record.sequence > since && record.event == AdminEvent::Requested) {
			if clock() >= until {
				fail("hold: the request was never recorded");
			}
			sleep_until(clock() + TICKS / 10);
		}
		probe.journal_fault(AdminJournalFault::Hold);
	}
	wait_for_person("hold");
	if consumption {
		let grant = granted(probe.request, 120, "hold");
		probe.journal_fault(AdminJournalFault::Hold);
		let mut capture = Capture { bytes: Vec::new(), handles: Vec::new() };
		let _ = admin_authority::Client::new(&mut capture).execute();
		if !send_caps_blocking(grant.grant, &capture.bytes, &capture.handles) {
			fail("hold: the execute could not be sent");
		}
	}
	let until = clock() + 120 * TICKS;
	while probe.held() == 0 {
		if clock() >= until {
			fail("hold: no acknowledgment was ever held");
		}
		sleep_until(clock() + TICKS / 20);
	}
	say(if consumption { "the consumption's record is held, and this requester ends" } else { "the approval's record is held, and this requester ends" });
	exit();
}

fn journal(probe: &Probe) {
	let records = probe.journal();
	let count = |event: AdminEvent| records.iter().filter(|record| record.event == event).count();
	let epochs: Vec<u64> = records.iter().map(|record| record.broker_epoch).collect();
	say(&format!("journal records {} requested {} granted {} declined {} consumed {} completed {} failed {} unknown {} epochs {}..{}", records.len(), count(AdminEvent::Requested), count(AdminEvent::Granted), count(AdminEvent::Declined), count(AdminEvent::Consumed), count(AdminEvent::Completed), count(AdminEvent::Failed), count(AdminEvent::OutcomeUnknown), epochs.iter().min().copied().unwrap_or(0), epochs.iter().max().copied().unwrap_or(0)));
	// NO PAYLOAD AND NO SECRET: every record's digest is a digest, and nothing else carries bytes.
	if records.iter().any(|record| record.digest.len() != 32 && !record.digest.is_empty()) {
		fail("journal: a record carries something other than a digest");
	}
	say("PASS journal");
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let witness = recv_tagged(bootstrap, &mut buf, b"FIXTURE").unwrap_or(0);
	let request_connection = recv_tagged(bootstrap, &mut buf, b"ADMINREQUEST").unwrap_or(0);
	let audit = recv_tagged(bootstrap, &mut buf, CAP_ADMIN_AUDIT).unwrap_or(0);
	let test = recv_tagged(bootstrap, &mut buf, CAP_ADMIN_TEST).unwrap_or(0);
	if witness == 0 || request_connection == 0 || audit == 0 || test == 0 {
		fail("a grant this probe needs was not delivered");
	}
	let probe = Probe { witness, request: request_connection, audit, test };
	let mut words = context.arguments.split(' ');
	match words.next().unwrap_or("") {
		"effects" => {
			let effects = probe.effects();
			say(&format!("effects writes {} dispatches {} generation {}", effects.writes, effects.dispatches, effects.generation));
		}
		"unavailable" => {
			let before = probe.effects();
			if !matches!(probe.test().path(&false), Some(Ok(()))) {
				fail("unavailable: the path could not be taken away");
			}
			let payload = Payload::new(1);
			ask(probe.request, &args(TARGET, "with no path to a person"), &payload);
			declined(probe.request, 30, "unavailable");
			if !matches!(probe.test().path(&true), Some(Ok(()))) {
				fail("unavailable: the path could not be given back");
			}
			unchanged(&probe, &before, "unavailable");
			say("PASS unavailable: with no path to a person the request was declined, and nothing reached the executor");
		}
		"forged" => {
			let before = probe.effects();
			// A CHANNEL OF ITS OWN is no grant: nothing serves it.
			let Some((mine, theirs)) = channel() else { fail("forged: no channel") };
			if matches!(admin_authority::Client::with_deadline(ChannelTransport { chan: mine }, clock() + TICKS).execute(), Some(Ok(_))) {
				fail("forged: a self-made channel answered as a grant");
			}
			close(mine);
			close(theirs);
			// AN EXECUTE SENT ON THE REQUEST CONNECTION is a malformed request, not a redemption.
			if matches!(admin_authority::Client::with_deadline(ChannelTransport { chan: probe.request }, clock() + 2 * TICKS).execute(), Some(Ok(_))) {
				fail("forged: the request connection executed something");
			}
			unchanged(&probe, &before, "forged");
			say("PASS forged: neither a channel of its own nor a request connection reached the executor");
		}
		"wrong-target" => {
			let before = probe.effects();
			let payload = Payload::new(2);
			ask(probe.request, &args("probe-elsewhere", "a target nobody serves"), &payload);
			declined(probe.request, 30, "wrong-target");
			unchanged(&probe, &before, "wrong-target");
			say("PASS wrong-target: a target the executor does not serve was declined with nobody asked");
		}
		"storage" => {
			let before = probe.effects();
			probe.journal_fault(AdminJournalFault::Fail);
			let payload = Payload::new(3);
			ask(probe.request, &args(TARGET, "with the journal failing"), &payload);
			declined(probe.request, 30, "storage");
			probe.journal_fault(AdminJournalFault::None);
			unchanged(&probe, &before, "storage");
			say("PASS storage: a request that could not be recorded was declined, and nothing reached the executor");
		}
		"request" => request(&probe, words.next().unwrap_or("")),
		"delegate" => delegate(&probe, words.next() == Some("live")),
		"hold" => hold(&probe, words.next() == Some("consumption")),
		"journal" => journal(&probe),
		_ => fail("usage: admincheck effects | unavailable | forged | wrong-target | storage | request CASE | delegate dead|live | hold approval|consumption | journal"),
	}
	exit();
}
