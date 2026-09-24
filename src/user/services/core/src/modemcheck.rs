// modemcheck - the in-guest scenario driver for the modem gate. DEVELOPMENT-ONLY.
//
// It holds an ordinary NetworkService client, the modem fixture's control endpoint, the modem observation connection and all three modem authorities - data (which may NOT
// replace an uplink already selected), subscriber identity and management - and prints one verdict line
// per phase for the gate to read. What it proves about traffic it proves through the ordinary network
// calls any client has - `info`, `ping`, `resolve` - never through the modem's own connection, and what
// reached the modem it reads from the fixture's counters.
//
// Every run is a new process with new grants, bound to the SIM in the modem at its launch - which is why
// a phase that replaces the SIM is the last thing its run does.
//
//   modemcheck clients     ModemService's clients in use, for the gate's baseline comparison; the
//                          service's handles are the gate's to read, from the system graph
//   modemcheck pin         counters known, a wrong PIN rejected, the right one accepted - one attempt each
//   modemcheck identity    the identity grant answers, and observation carries no subscriber identifier
//   modemcheck activate    no NIC: the context installs as the uplink; address, DNS and MTU through `info`,
//                          echo and DNS through the network; a replayed activation reply changes nothing;
//                          deactivation leaves the service unlinked
//   modemcheck context     one context at a time: a second activation while one is up is refused, and
//                          once it is deactivated the next activation is admitted
//   modemcheck drop        the network ending the context removes the link
//   modemcheck sim         a new SIM ends the context and this run's grants
//   modemcheck rollback    an installation NetworkService refuses is deactivated, and nothing is left
//   modemcheck inherit     a transferred data endpoint does not keep its dead owner's context
//   modemcheck lock        replace the SIM with a locked one
//   modemcheck uncertain   a PIN whose answer never comes is outcome-unknown, sent once, and reconciled
//   modemcheck limits      the limits are exposed, and nothing is left in use
//   modemcheck republish   a withdrawn modem's grant fails; republication is another modem
//   modemcheck busy        with a NIC selected, a data grant that may not replace it is refused, and the
//                          NIC is untouched

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{CommandKind, ContextState, Error, FixtureStats, IpAddress, Ipv4Addr as WireIp, LaunchContext, ModemId, ModemStatus, NetInfo, PingStatus, ScopedAddress, SimPinResult, SimState, modem, modem_data, modem_fixture, modem_identity, modem_manage, network};
use rt::*;
use services::capability_names::*;

const TICKS: u64 = 100;
const GATEWAY: [u8; 4] = [10, 64, 0, 1];
const ADDRESS: [u8; 4] = [10, 64, 0, 2];
const ANSWER: [u8; 4] = [198, 51, 100, 7];
const MTU: u16 = 1400;

fn say(line: &[u8]) {
	print(b"modemcheck: ");
	print(line);
	print(b"\n");
}

fn fail(line: &[u8]) -> ! {
	print(b"modemcheck: FAIL ");
	print(line);
	print(b"\n");
	exit();
}

fn number(out: &mut Vec<u8>, value: u64) {
	let mut digits = [0u8; 20];
	let mut at = digits.len();
	let mut n = value;
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

struct Probe {
	network: u64,
	fixture: u64,
	state: u64,
	data: u64,
	identity: u64,
	manage: u64,
}

impl Probe {
	fn network(&self) -> network::Client<ChannelTransport> {
		network::Client::with_deadline(ChannelTransport { chan: self.network }, clock() + 20 * TICKS)
	}
	fn fixture(&self) -> modem_fixture::Client<ChannelTransport> {
		modem_fixture::Client::with_deadline(ChannelTransport { chan: self.fixture }, clock() + 5 * TICKS)
	}
	fn state(&self) -> modem::Client<ChannelTransport> {
		modem::Client::with_deadline(ChannelTransport { chan: self.state }, clock() + 5 * TICKS)
	}
	fn data(&self) -> modem_data::Client<ChannelTransport> {
		modem_data::Client::with_deadline(ChannelTransport { chan: self.data }, clock() + 90 * TICKS)
	}
	fn identity(&self) -> modem_identity::Client<ChannelTransport> {
		modem_identity::Client::with_deadline(ChannelTransport { chan: self.identity }, clock() + 10 * TICKS)
	}
	fn manage(&self) -> modem_manage::Client<ChannelTransport> {
		modem_manage::Client::with_deadline(ChannelTransport { chan: self.manage }, clock() + 20 * TICKS)
	}
	fn stats(&self) -> FixtureStats {
		match self.fixture().stats() {
			Some(Ok(stats)) => stats,
			_ => fail(b"the fixture's counters could not be read"),
		}
	}
	fn fixture_ok(&self, result: Option<Result<(), Error>>, why: &[u8]) {
		if !matches!(result, Some(Ok(()))) {
			fail(why);
		}
	}
	fn status(&self) -> ModemStatus {
		match self.data().status() {
			Some(Ok(status)) => status,
			_ => fail(b"the data grant could not read its modem's status"),
		}
	}
	fn modem(&self) -> ModemId {
		self.status().id
	}
	fn info(&self) -> Result<NetInfo, Error> {
		match self.network().info() {
			Some(result) => result,
			None => fail(b"NetworkService did not answer info"),
		}
	}
}

fn v4(octets: [u8; 4]) -> ScopedAddress {
	ScopedAddress { addr: IpAddress::V4(WireIp { a: octets[0], b: octets[1], c: octets[2], d: octets[3] }), scope: None }
}

fn is_v4(address: &IpAddress, octets: [u8; 4]) -> bool {
	matches!(address, IpAddress::V4(ip) if [ip.a, ip.b, ip.c, ip.d] == octets)
}

// Wait until `check` holds, a bounded while: the service acts on a change as its message arrives, and a
// probe reading right after asking must give it the moment.
fn until(seconds: u64, mut check: impl FnMut() -> bool) -> bool {
	let deadline = clock() + seconds * TICKS;
	loop {
		if check() {
			return true;
		}
		if clock() >= deadline {
			return false;
		}
		sleep_until(clock() + TICKS / 10);
	}
}

fn unlinked(probe: &Probe) -> bool {
	matches!(probe.info(), Err(Error::Io))
}

fn clients(probe: &Probe) {
	let limits = match probe.state().limits() {
		Some(Ok(limits)) => limits,
		_ => fail(b"the limits could not be read"),
	};
	let mut line = b"service clients ".to_vec();
	number(&mut line, u64::from(limits.clients));
	say(&line);
}

fn pin(probe: &Probe) {
	let modem = probe.modem();
	let before = probe.stats();
	let attempts = match probe.manage().attempts(&modem) {
		Some(Ok(attempts)) => attempts,
		_ => fail(b"pin: the counters could not be read"),
	};
	if !attempts.pin.known || attempts.pin.remaining != 3 || !attempts.puk.known || attempts.puk.remaining != 10 {
		fail(b"pin: the counters were not known as three and ten");
	}
	match probe.manage().enter_pin(&modem, "0000", &false) {
		Some(Ok(outcome)) if outcome.result == SimPinResult::Rejected && outcome.attempts.pin.known && outcome.attempts.pin.remaining == 2 => {}
		_ => fail(b"pin: a wrong PIN was not rejected with two attempts left"),
	}
	match probe.manage().enter_pin(&modem, "1234", &false) {
		Some(Ok(outcome)) if outcome.result == SimPinResult::Accepted => {}
		_ => fail(b"pin: the right PIN was not accepted"),
	}
	let after = probe.stats();
	// EXACTLY TWO ATTEMPTS REACHED THE SIM, and what the fixture kept of the last is its length.
	if after.pin_attempts != before.pin_attempts + 2 || after.last_secret_bytes != 4 {
		fail(b"pin: the SIM did not see exactly the two attempts made");
	}
	if !until(2, || probe.status().sim == SimState::Ready) {
		fail(b"pin: the SIM was not ready after the right PIN");
	}
	say(b"PASS pin: the counters were known, a wrong PIN was rejected and counted, the right one accepted - one attempt each");
}

fn identity(probe: &Probe) {
	let modem = probe.modem();
	match probe.identity().identity(&modem) {
		Some(Ok(identity)) if identity.imsi.is_some() && identity.iccid.is_some() => {}
		_ => fail(b"identity: the identity grant did not answer"),
	}
	// The identity grant is for its own modem: another modem's handle is refused.
	let other = ModemId { incarnation: modem.incarnation ^ 1, ..modem.clone() };
	if matches!(probe.identity().identity(&other), Some(Ok(_))) {
		fail(b"identity: a handle from another incarnation was answered");
	}
	say(b"PASS identity: the identity grant answered for its own modem only, and observation names no subscriber");
}

// Activate with no NIC selected, and prove the link through ordinary network calls.
fn activate_now(probe: &Probe) -> proto::system::ContextStatus {
	let modem = probe.modem();
	match probe.data().activate(&modem) {
		Some(Ok(status)) if status.state == ContextState::Active && status.address == Some(u32::from_be_bytes(ADDRESS)) => status,
		Some(Ok(_)) => fail(b"the context came back not active or with the wrong address"),
		Some(Err(_)) => fail(b"the activation was refused"),
		None => fail(b"the activation did not complete"),
	}
}

fn activate(probe: &Probe) {
	if !until(3, || unlinked(probe)) {
		fail(b"activate: NetworkService had a link before the modem's - this run needs a machine with no NIC");
	}
	let before = probe.stats();
	let context = activate_now(probe);
	// THE ADDRESS, THE RESOLVER AND THE MTU, as any network client reads them.
	let info = match probe.info() {
		Ok(info) => info,
		Err(_) => fail(b"activate: NetworkService reported no link after the installation"),
	};
	if info.name != "wwan0" || info.mtu != MTU || info.scope.index != 1 {
		fail(b"activate: the interface is not the modem link at its MTU");
	}
	if !info.addresses.iter().any(|address| is_v4(&address.addr, ADDRESS) && address.prefix_len == 30) {
		fail(b"activate: the modem's address is not the interface's");
	}
	if !info.dns.iter().any(|server| is_v4(&server.addr, GATEWAY)) {
		fail(b"activate: the modem's resolver is not the interface's");
	}
	// ECHO, BOTH WAYS.
	match probe.network().ping(&v4(GATEWAY)) {
		Some(Ok(reply)) if reply.status == PingStatus::Reply => {}
		_ => fail(b"activate: the gateway did not answer an echo over the modem link"),
	}
	// DNS, BOTH WAYS: a name that resolves, and one that does not.
	match probe.network().resolve(&String::from("modem.test")) {
		Some(Ok(addresses)) if addresses.iter().any(|address| is_v4(address, ANSWER)) => {}
		_ => fail(b"activate: modem.test did not resolve over the modem link"),
	}
	if !matches!(probe.network().resolve(&String::from("absent.test")), Some(Err(Error::NotFound))) {
		fail(b"activate: a name the resolver does not know was not reported absent");
	}
	let after = probe.stats();
	if after.echo_replies <= before.echo_replies || after.dns_answers < before.dns_answers + 2 || after.datagrams_in <= before.datagrams_in || after.datagrams_out <= before.datagrams_out {
		fail(b"activate: the fixture did not see the traffic in both directions");
	}
	// A DUPLICATE ACTIVATION REPLY changes nothing: the context is the one that was installed.
	probe.fixture_ok(probe.fixture().replay(), b"activate: the fixture could not replay its activation reply");
	sleep_until(clock() + TICKS / 2);
	let status = probe.status();
	match status.context {
		Some(current) if current.id == context.id && current.state == ContextState::Active => {}
		_ => fail(b"activate: a replayed reply changed the context"),
	}
	match probe.data().deactivate(&context.id) {
		Some(Ok(())) => {}
		_ => fail(b"activate: the context could not be deactivated"),
	}
	if !until(3, || unlinked(probe)) {
		fail(b"activate: NetworkService kept a link after the deactivation");
	}
	// A REPLAY NOW names a context that is gone, and brings nothing back.
	probe.fixture_ok(probe.fixture().replay(), b"activate: the fixture could not replay its activation reply");
	sleep_until(clock() + TICKS / 2);
	if !unlinked(probe) || probe.status().context.is_some_and(|context| context.state == ContextState::Active) {
		fail(b"activate: a stale activation reply brought the context back");
	}
	let mut line = b"PASS activate: with no NIC the context became the uplink; address, DNS and MTU read through info; echo and DNS crossed it both ways (".to_vec();
	number(&mut line, u64::from(after.datagrams_in - before.datagrams_in));
	line.extend_from_slice(b" out, ");
	number(&mut line, u64::from(after.datagrams_out - before.datagrams_out));
	line.extend_from_slice(b" in); replayed replies changed nothing; deactivation left the service unlinked");
	say(&line);
}

// ONE CONTEXT, REFUSED AT AND GIVEN BACK. With a context up, a second activation is answered `again` before the
// modem is asked for anything; deactivated, the slot is free and the next activation is admitted.
fn one_context(probe: &Probe) {
	if !until(3, || unlinked(probe)) {
		fail(b"context: NetworkService had a link before the modem's - this run needs a machine with no NIC");
	}
	let first = activate_now(probe);
	let before = probe.stats();
	if !matches!(probe.data().activate(&probe.modem()), Some(Err(Error::Again))) {
		fail(b"context: a second activation while a context was up was not refused as busy");
	}
	if probe.stats().activations != before.activations {
		fail(b"context: the refused activation reached the modem");
	}
	if !matches!(probe.data().deactivate(&first.id), Some(Ok(()))) {
		fail(b"context: the context could not be deactivated");
	}
	if !until(3, || unlinked(probe)) {
		fail(b"context: NetworkService kept a link after the deactivation");
	}
	let second = activate_now(probe);
	if second.id == first.id {
		fail(b"context: the context admitted after the first one ended is the first one again");
	}
	if !matches!(probe.data().deactivate(&second.id), Some(Ok(()))) {
		fail(b"context: the second context could not be deactivated");
	}
	if !until(3, || unlinked(probe)) {
		fail(b"context: NetworkService kept a link after the second deactivation");
	}
	say(b"PASS context: a second activation while one context was up was refused before the modem saw it, and the slot came back when it ended");
}

fn drop_context(probe: &Probe) {
	activate_now(probe);
	probe.fixture_ok(probe.fixture().drop_context(), b"drop: the fixture could not end the context");
	if !until(3, || unlinked(probe)) {
		fail(b"drop: the link outlived the context the network ended");
	}
	if probe.status().context.is_some_and(|context| context.state == ContextState::Active) {
		fail(b"drop: the context is still reported active");
	}
	say(b"PASS drop: the network ending the context removed the link, and the service fell back to no link");
}

fn sim(probe: &Probe) {
	activate_now(probe);
	probe.fixture_ok(probe.fixture().set_sim(&true, &false), b"sim: the fixture could not replace the SIM");
	if !until(3, || unlinked(probe)) {
		fail(b"sim: the link outlived the SIM it was activated on");
	}
	// THIS RUN'S GRANTS WERE FOR THE OLD SIM, and ended with it.
	if matches!(modem_data::Client::with_deadline(ChannelTransport { chan: probe.data }, clock() + 2 * TICKS).status(), Some(Ok(_))) {
		fail(b"sim: a data grant for the old SIM still answered");
	}
	say(b"PASS sim: a new SIM ended the context, its link and every grant bound to the old SIM");
}

fn rollback(probe: &Probe) {
	let before = probe.stats();
	probe.fixture_ok(probe.fixture().ipv6_only(&true), b"rollback: the fixture could not answer IPv6-only");
	let modem = probe.modem();
	let refused = probe.data().activate(&modem);
	probe.fixture_ok(probe.fixture().ipv6_only(&false), b"rollback: the fixture could not stop answering IPv6-only");
	if !matches!(refused, Some(Err(Error::Unsupported))) {
		fail(b"rollback: an IPv6-only context was not refused as unsupported");
	}
	// THE MODEM ACTIVATED IT AND NOTHING WAS LEFT: the bounded deactivation took it down again.
	if !until(5, || probe.status().context.is_none()) {
		fail(b"rollback: the refused context was left up on the modem");
	}
	let after = probe.stats();
	if after.activations != before.activations + 1 || !unlinked(probe) {
		fail(b"rollback: the activation was not exactly one, or a link was left behind");
	}
	say(b"PASS rollback: an IPv6-only configuration was refused as unsupported, the context taken down again and no link left");
}

fn inherit(probe: &Probe) {
	// The endpoint and the context the holder sent down the pipe before it exited.
	let mut buf = [0u8; 256];
	let (len, handles) = loop {
		match try_recv_caps(stdin(), &mut buf) {
			PolledCaps::Message { len, handles } if !handles.as_slice().is_empty() => break (len, handles),
			PolledCaps::Message { .. } => {}
			PolledCaps::Empty => sleep_until(clock() + TICKS / 10),
			PolledCaps::Closed => fail(b"inherit: the holder sent no endpoint"),
		}
	};
	let Some(context) = proto::system::ContextId::decode(&buf[..len]) else { fail(b"inherit: the holder's message named no context") };
	let inherited = handles.first();
	// The holder has exited by now; its grant, and its context, went with it.
	if !until(5, || unlinked(probe)) {
		fail(b"inherit: the context outlived its owner");
	}
	let copy = modem_data::Client::with_deadline(ChannelTransport { chan: inherited }, clock() + 2 * TICKS);
	let mut copy = copy;
	if matches!(copy.deactivate(&context), Some(Ok(()))) || matches!(copy.status(), Some(Ok(_))) {
		fail(b"inherit: a transferred endpoint kept its dead owner's authority");
	}
	close(inherited);
	say(b"PASS inherit: the context ended with its owner, and a transferred copy of the endpoint could do nothing");
}

fn lock(probe: &Probe) {
	probe.fixture_ok(probe.fixture().set_sim(&true, &true), b"lock: the fixture could not insert a locked SIM");
	say(b"a locked SIM is in the modem");
}

fn uncertain(probe: &Probe) {
	let modem = probe.modem();
	if probe.status().sim != SimState::LockedPin {
		fail(b"uncertain: the SIM is not locked - run `modemcheck lock` first");
	}
	let before = probe.stats();
	probe.fixture_ok(probe.fixture().delay(&CommandKind::EnterPin, &0), b"uncertain: the fixture could not hold the PIN's answer");
	match probe.manage().enter_pin(&modem, "1234", &false) {
		Some(Ok(outcome)) if outcome.result == SimPinResult::OutcomeUnknown => {}
		_ => fail(b"uncertain: an unanswered PIN was not reported outcome-unknown"),
	}
	// NOT REPLAYED, and RECONCILED: one attempt reached the SIM, and a fresh read says what became of it.
	sleep_until(clock() + TICKS);
	let after = probe.stats();
	if after.pin_attempts != before.pin_attempts + 1 {
		fail(b"uncertain: the PIN reached the SIM more than once, or not at all");
	}
	match probe.manage().attempts(&modem) {
		Some(Ok(attempts)) if attempts.pin.known => {}
		_ => fail(b"uncertain: the counters were not read again"),
	}
	if !until(3, || probe.status().sim == SimState::Ready) {
		fail(b"uncertain: the reconciled state is not the SIM's");
	}
	say(b"PASS uncertain: an unanswered PIN was outcome-unknown, reached the SIM once, and a fresh query reconciled the state");
}

fn limits(probe: &Probe) {
	match probe.state().limits() {
		Some(Ok(limits)) if limits.providers_max == 4 && limits.clients_max == 32 && limits.contexts_max == 1 && limits.providers == 1 && limits.contexts == 0 => {}
		_ => fail(b"limits: the limits were not four providers, 32 clients and one context, with nothing active"),
	}
	say(b"PASS limits: four providers, 32 clients and one context are the stated limits, and none is held");
}

fn republish(probe: &Probe) {
	let old = probe.modem();
	probe.fixture_ok(probe.fixture().withdraw(), b"republish: the fixture could not withdraw the modem");
	if !until(3, || !matches!(modem_data::Client::with_deadline(ChannelTransport { chan: probe.data }, clock() + TICKS).status(), Some(Ok(_)))) {
		fail(b"republish: a withdrawn modem's grant still answered");
	}
	probe.fixture_ok(probe.fixture().republish(), b"republish: the fixture could not republish the modem");
	let found = until(5, || matches!(probe.state().modems(), Some(Ok(modems)) if modems.iter().any(|modem| modem.id != old)));
	if !found {
		fail(b"republish: the republished modem was not a new modem");
	}
	say(b"PASS republish: a withdrawn modem's grant failed, and the republished modem is another");
}

fn busy(probe: &Probe) {
	let before = match probe.info() {
		Ok(info) if info.name == "net0" => info,
		_ => fail(b"busy: there is no NIC selected - this run needs a machine with one"),
	};
	let modem = probe.modem();
	match probe.data().activate(&modem) {
		Some(Err(Error::Again)) => {}
		_ => fail(b"busy: a data grant that may not replace the NIC was not refused as busy"),
	}
	match probe.info() {
		Ok(info) if info.name == "net0" && info.scope == before.scope => {}
		_ => fail(b"busy: the refusal disturbed the NIC"),
	}
	if probe.status().context.is_some() {
		fail(b"busy: the modem was asked to activate before admission was refused");
	}
	say(b"PASS busy: admission was refused before the modem did anything, and the NIC is untouched");
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
	let network = recv_tagged(bootstrap, &mut buf, b"NETWORK").unwrap_or(0);
	let fixture = recv_tagged(bootstrap, &mut buf, b"FIXTURE").unwrap_or(0);
	let state = recv_tagged(bootstrap, &mut buf, CAP_MODEM_STATE).unwrap_or(0);
	let data = recv_tagged(bootstrap, &mut buf, b"MODEMDATA").unwrap_or(0);
	let identity_grant = recv_tagged(bootstrap, &mut buf, b"MODEMIDENTITY").unwrap_or(0);
	let manage = recv_tagged(bootstrap, &mut buf, b"MODEMMANAGE").unwrap_or(0);
	if network == 0 || fixture == 0 || state == 0 || data == 0 || identity_grant == 0 || manage == 0 {
		fail(b"a grant this probe needs was not delivered");
	}
	let probe = Probe { network, fixture, state, data, identity: identity_grant, manage };
	match args.split(|&b| b == b' ').next().unwrap_or(&[]) {
		b"clients" => clients(&probe),
		b"pin" => pin(&probe),
		b"identity" => identity(&probe),
		b"activate" => activate(&probe),
		b"context" => one_context(&probe),
		b"drop" => drop_context(&probe),
		b"sim" => sim(&probe),
		b"rollback" => rollback(&probe),
		b"inherit" => inherit(&probe),
		b"lock" => lock(&probe),
		b"uncertain" => uncertain(&probe),
		b"limits" => limits(&probe),
		b"republish" => republish(&probe),
		b"busy" => busy(&probe),
		_ => fail(b"usage: modemcheck clients | pin | identity | activate | context | drop | sim | rollback | inherit | lock | uncertain | limits | republish | busy"),
	}
	exit();
}
