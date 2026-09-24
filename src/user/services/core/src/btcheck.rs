// btcheck - the in-guest scenario driver for the Bluetooth gate. DEVELOPMENT-ONLY.
//
// It holds three grants - the pointer service, and Bluetooth's read and operator authorities - and
// drives the host stack against the emulated controller and mouse `bt_fixture` publishes, printing one
// verdict line per phase for the gate to read. It proves what a client can observe and nothing else:
// that the stack answers, that a pairing reaches a durable bond at the security level it reports, and
// that the pointer a live client receives actually moves and clicks.
//
//   btcheck pair     scan, pair, enable, and watch the cursor move and the button go down and up
//   btcheck reuse    after a restart or a cold reboot: the bond is still there, nothing is pairing,
//                    and the cursor moves - which it can only do over the stored key
//   btcheck forget   forget the bond, and watch the cursor NOT move
//   btcheck limits   both services run in Domains whose every limit is finite and the stated one
//   btcheck refund   stop the service through the supervisor: its Domain is gone, not merely idle,
//                    and the machine's own pointer, moved and clicked from the host, still arrives
//   btcheck exhaust  fill the service's client table, see the refusal, give it back, and see the
//                    pointer service still answer
//   btcheck loss     take the controller away while a scan is running, see the scan end and the
//                    controller leave, give it back, see the bond reconnect and the old scan refused
//   btcheck deny     an operator verb on a read connection, and the bond store's own lookup on a read
//                    and on an operator connection: none is answered as what it claims to be

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{LaunchContext, PairingState, PeerAddress, PeerKind, PolicyOutcome, PolicyVerb, ResourceType, SecurityLevel, bluetooth, bluetooth_bond_store, bluetooth_operator, device, device_policy_admin, input, process};
use rt::*;
use services::capability_names::*;
use wire::{Handles, Transport, TransportError};

const TICKS: u64 = 100;
// The fixture mouse's address, most significant first, and its kind.
const MOUSE: [u8; 6] = [0xc0, 0xff, 0xee, 0x00, 0x00, 0x01];

fn mouse() -> PeerAddress {
	PeerAddress { kind: PeerKind::RandomStatic, bytes: MOUSE.to_vec() }
}

fn say(line: &[u8]) {
	print(b"btcheck: ");
	print(line);
	print(b"\n");
}

fn fail(line: &[u8]) -> ! {
	print(b"btcheck: FAIL ");
	print(line);
	print(b"\n");
	exit();
}

// Wait until the stack has a powered controller that can pair. It initialises one command at a time,
// so this is seconds after boot rather than instant.
fn powered_controller(read: u64) {
	let deadline = clock() + 20 * TICKS;
	loop {
		if let Some(Ok(controllers)) = bluetooth::Client::new(ChannelTransport { chan: read }).controllers()
			&& controllers.first().is_some_and(|c| c.powered && c.secure_connections)
		{
			return;
		}
		if clock() >= deadline {
			fail(b"no powered controller with Secure Connections within twenty seconds");
		}
		sleep_until(clock() + TICKS / 4);
	}
}

// What a pointer client saw over a window: whether the cell moved, and whether a button went down and
// came back up.
struct Seen {
	moved: bool,
	pressed: bool,
	released: bool,
}

// The pointer service's recent events, as one subscription delivers them: a bounded SNAPSHOT of its
// ring, which ends on its own - not a live stream.
fn snapshot(input_client: u64) -> Vec<(u16, u16, u8)> {
	let Some(stream) = input::Client::new(ChannelTransport { chan: input_client }).subscribe() else { fail(b"the pointer subscription was refused") };
	let mut events = Vec::new();
	let mut buf = [0u8; 64];
	loop {
		match recv_caps_deadline(stream, &mut buf, clock() + 2 * TICKS) {
			DeadlineCaps::Message { len, mut handles } => {
				if let Some(event) = input::subscribe_read(&buf[..len], &mut handles) {
					events.push((event.col, event.row, event.buttons));
				}
				for &handle in handles.as_slice() {
					close(handle);
				}
			}
			DeadlineCaps::Closed => break,
			DeadlineCaps::TimedOut => fail(b"a pointer snapshot did not end"),
		}
	}
	close(stream);
	events
}

// WHAT HAPPENED OVER A WINDOW, from snapshots taken through it. The ring slides, so a later snapshot is
// the tail of the earlier one followed by what arrived since: the new events are what follows the
// longest overlap. What was already in the ring when the window opened is not counted - a mouse that
// moved before is not a mouse that moves now.
fn watch(input_client: u64, ticks: u64) -> Seen {
	let deadline = clock() + ticks;
	let mut seen = Seen { moved: false, pressed: false, released: false };
	let mut last = snapshot(input_client);
	let mut at = last.last().map(|&(col, row, _)| (col, row));
	while clock() < deadline && !(seen.moved && seen.pressed && seen.released) {
		sleep_until(clock() + TICKS / 5);
		let now = snapshot(input_client);
		let overlap = (0..=last.len().min(now.len())).rev().find(|&n| last[last.len() - n..] == now[..n]).unwrap_or(0);
		for &(col, row, buttons) in &now[overlap..] {
			if at.is_some_and(|previous| previous != (col, row)) {
				seen.moved = true;
			}
			at = Some((col, row));
			if buttons & 1 != 0 {
				seen.pressed = true;
			} else if seen.pressed {
				seen.released = true;
			}
		}
		last = now;
	}
	seen
}

fn pair(read: u64, operator: u64, input_client: u64) {
	powered_controller(read);
	say(b"a powered controller with Secure Connections is present");
	let mut client = bluetooth::Client::new(ChannelTransport { chan: read });
	let Some(Ok(scan)) = client.scan(&0, &5000) else { fail(b"the scan was refused") };
	let deadline = clock() + 7 * TICKS;
	let mut found = false;
	while clock() < deadline && !found {
		if let Some(Ok(results)) = client.results(&scan) {
			found = results.iter().any(|r| r.address == mouse() && r.human_interface);
		}
		sleep_until(clock() + TICKS / 4);
	}
	if !found {
		fail(b"the fixture mouse was not found by a scan");
	}
	say(b"the scan found the fixture mouse, advertising the human-interface service");
	let mut op = bluetooth_operator::Client::new(ChannelTransport { chan: operator });
	if !matches!(op.pair(&0, &mouse()), Some(Ok(()))) {
		fail(b"the pairing was refused");
	}
	let deadline = clock() + 60 * TICKS;
	let progress = loop {
		match op.progress(&0) {
			Some(Ok(p)) if matches!(p.state, PairingState::Bonded | PairingState::Failed) => break p,
			_ => {}
		}
		if clock() >= deadline {
			fail(b"the pairing did not finish inside sixty seconds");
		}
		sleep_until(clock() + TICKS / 4);
	};
	if progress.state != PairingState::Bonded {
		fail(b"the pairing failed");
	}
	if progress.security != SecurityLevel::EncryptedUnauthenticated {
		fail(b"a Just Works bond reported a security level other than encrypted-unauthenticated");
	}
	say(b"bonded, and the security level says encrypted and NOT authenticated");
	if !matches!(op.enable(&0, &mouse(), &true), Some(Ok(()))) {
		fail(b"the bonded peer could not be made an input source");
	}
	let seen = watch(input_client, 20 * TICKS);
	if !seen.moved {
		fail(b"the cursor did not move");
	}
	if !(seen.pressed && seen.released) {
		fail(b"the button did not go down and come back up");
	}
	say(b"PASS pair: the cursor moved and the left button went down and up");
}

fn reuse(read: u64, operator: u64, input_client: u64) {
	powered_controller(read);
	let mut op = bluetooth_operator::Client::new(ChannelTransport { chan: operator });
	let Some(Ok(bonded)) = op.bonded(&0) else { fail(b"the bond list could not be read") };
	if !bonded.iter().any(|b| b.address == mouse() && b.enabled) {
		fail(b"the bond is not there, or not enabled, after the restart");
	}
	if !matches!(op.progress(&0), Some(Ok(p)) if p.state == PairingState::Idle) {
		fail(b"a pairing attempt is live - the stack paired again instead of reusing the bond");
	}
	let seen = watch(input_client, 25 * TICKS);
	if !seen.moved {
		fail(b"the cursor did not move over the reused bond");
	}
	say(b"PASS reuse: the stored bond is there, nothing paired again, and the cursor moved");
}

fn forget(operator: u64, input_client: u64) {
	let mut op = bluetooth_operator::Client::new(ChannelTransport { chan: operator });
	if !matches!(op.forget(&0, &mouse()), Some(Ok(()))) {
		fail(b"the bond could not be forgotten");
	}
	let Some(Ok(bonded)) = op.bonded(&0) else { fail(b"the bond list could not be read") };
	if bonded.iter().any(|b| b.address == mouse()) {
		fail(b"the forgotten bond is still listed");
	}
	// The link is gone with the bond; give it a moment to drop, then watch for movement that must not
	// come.
	sleep_until(clock() + 2 * TICKS);
	let seen = watch(input_client, 6 * TICKS);
	if seen.moved {
		fail(b"the cursor moved after the bond was forgotten");
	}
	say(b"PASS forget: the bond is gone and the mouse no longer moves the cursor");
}

// THE FIGURES THE SUPERVISOR STATES, which this probe checks the kernel's counters against rather than
// against themselves: (memory, handles, threads, ipc queue, stack, dma).
const MB: u64 = 1024 * 1024;
const SERVICE_LIMITS: [u64; 6] = [64 * MB, 256, 4, 2 * MB, 2 * MB, 0];
const STORE_LIMITS: [u64; 6] = [16 * MB, 64, 2, 256 * 1024, MB, 0];

// One service's six (used, limit) pairs as ProcessService reports them, in the order above.
fn budget_of(process_client: u64, name: &str) -> Option<[(u64, u64); 6]> {
	let budgets = process::Client::new(ChannelTransport { chan: process_client }).accounting()?.ok()?;
	let budget = budgets.iter().find(|b| b.name.strip_suffix(abi::EXECUTABLE_SUFFIX) == Some(name))?;
	let pick = |kind: ResourceType| budget.usage.iter().find(|u| u.r#type == kind).map(|u| (u.used, u.limit));
	Some([
		pick(ResourceType::Memory)?,
		pick(ResourceType::Handles)?,
		pick(ResourceType::Threads)?,
		pick(ResourceType::IpcQueue)?,
		pick(ResourceType::Stack)?,
		pick(ResourceType::Dma)?,
	])
}

fn limits(process_client: u64) {
	for (name, stated) in [("bluetooth_service", SERVICE_LIMITS), ("bluetooth_bond_store", STORE_LIMITS)] {
		let Some(budget) = budget_of(process_client, name) else {
			print(b"btcheck: FAIL ");
			print(name.as_bytes());
			print(b" has no Domain of its own in ProcessService's accounting\n");
			exit();
		};
		for (at, &(used, limit)) in budget.iter().enumerate() {
			// FINITE, AND THE STATED FIGURE. A limit left at no-limit is a footprint nobody decided, and
			// a different figure is a launch that did not take the one the supervisor stated.
			if limit == u64::MAX || limit != stated[at] {
				fail(b"a Domain limit is not the finite figure the supervisor states");
			}
			if used > limit {
				fail(b"a Domain reports more in use than its own limit");
			}
		}
	}
	say(b"PASS limits: both services run in Domains whose six limits are finite and the stated figures");
}

fn refund(process_client: u64, supervisor: u64, input_client: u64, read: u64) {
	// AFTER A STOP THE DOMAIN IS GONE, not idle: ProcessService forgets a launch and its Domain when the
	// process ends, and a stopped service still charged would be one whose budget a restart doubles.
	//
	// THIS PROBE MAKES THE STOP ITSELF, through the supervisor's admin channel as `stop` does. Once the
	// service is stopped no probe holding its grants can be launched to look, and a look taken by
	// anything started earlier could not say whether it came before or after the replacement started.
	if budget_of(process_client, "bluetooth_service").is_none() {
		fail(b"refund: the running service has no Domain to give back");
	}
	if !send_blocking(supervisor, b"bluetooth_service", 0) {
		fail(b"refund: the supervisor could not be asked to stop the service");
	}
	let mut reply = [0u8; 512];
	match recv_blocking(supervisor, &mut reply) {
		Received::Message { len, .. } if reply[..len].starts_with(b"STOPPED\n") => {}
		_ => fail(b"refund: the supervisor did not stop the service"),
	}
	match process::Client::new(ChannelTransport { chan: process_client }).accounting() {
		Some(Ok(budgets)) if !budgets.iter().any(|b| b.name.strip_suffix(abi::EXECUTABLE_SUFFIX) == Some("bluetooth_service")) => {}
		Some(Ok(_)) => fail(b"the stopped service still has a Domain charged"),
		_ => fail(b"refund: ProcessService's accounting could not be read"),
	}
	// A CONNECTION TO THE STOPPED INSTANCE ANSWERS NOTHING: it went with the process that served it, and no
	// replacement takes it over - a client of the new instance holds a new grant.
	if matches!(bluetooth::Client::new(ChannelTransport { chan: read }).controllers(), Some(Ok(_))) {
		fail(b"refund: a connection to the stopped instance still answered");
	}
	// AND POINTER INPUT FROM AN UNRELATED SOURCE STILL WORKS with the stack gone: the gate moves and clicks
	// the machine's own tablet from the host once this line is out, and a live pointer client must see it.
	// Nothing Bluetooth can be the source while the service is stopped.
	say(b"the Bluetooth service is stopped; move another pointer now");
	let seen = watch(input_client, 10 * TICKS);
	if !(seen.moved && seen.pressed && seen.released) {
		fail(b"refund: with the Bluetooth service stopped, another pointer's motion and button did not arrive");
	}
	say(b"PASS refund: the stopped service's Domain is gone, its connections answer nothing, and another pointer still moves and clicks");
}

fn exhaust(read: u64, input_client: u64) {
	// THE SERVICE'S CLIENT TABLE IS A CONFIGURED BOUND, and a bound is only a bound if something is
	// refused at it. Connections are minted from this probe's own until the service says no.
	let mut minted: Vec<u64> = Vec::new();
	let refused = loop {
		match service_connect(read) {
			Some(connection) if connection != 0 => minted.push(connection),
			_ => break true,
		}
		if minted.len() > 64 {
			break false;
		}
	};
	if !refused {
		fail(b"the service minted more than sixty-four connections; its client table has no bound");
	}
	let held = minted.len();
	for connection in minted.drain(..) {
		close(connection);
	}
	// THE REFUND: a connection is admitted again once the ones that filled the table are gone. The
	// service takes a pass to notice them close, so this waits briefly rather than asking once.
	let deadline = clock() + 2 * TICKS;
	let admitted = loop {
		if let Some(connection) = service_connect(read)
			&& connection != 0
		{
			close(connection);
			break true;
		}
		if clock() >= deadline {
			break false;
		}
		sleep_until(clock() + 10);
	};
	if !admitted {
		fail(b"after the connections were closed the service still refused a new one");
	}
	// AND AN UNRELATED SERVICE STILL ANSWERS A TYPED REQUEST: the pointer service opens a subscription.
	let Some(stream) = input::Client::new(ChannelTransport { chan: input_client }).subscribe() else { fail(b"the pointer service did not answer after the Bluetooth stack was exhausted") };
	close(stream);
	let _ = held;
	say(b"PASS exhaust: the client table refused at its bound, gave the slots back, and the pointer service still answered");
}

fn loss(read: u64, device: u64, policy: u64) {
	powered_controller(read);
	let mut client = bluetooth::Client::new(ChannelTransport { chan: read });
	let Some(Ok(scan)) = client.scan(&0, &10_000) else { fail(b"the scan was refused") };
	// THE FIXTURE'S BINDING, found by the artifact the manager bound to it - read through DeviceService,
	// which forwards DeviceManager's own snapshot rather than deriving one.
	let Some(Ok(bindings)) = device::Client::new(ChannelTransport { chan: device }).bindings() else { fail(b"the device bindings could not be read") };
	let Some(index) = bindings.iter().find(|b| b.artifact == "bt_fixture").map(|b| b.index) else { fail(b"the fixture's device is not in the binding list") };
	let mut admin = device_policy_admin::Client::new(ChannelTransport { chan: policy });
	if !matches!(admin.apply(&index, &PolicyVerb::Disable, ""), Some(Ok(PolicyOutcome::Accepted))) {
		fail(b"the fixture's device could not be disabled");
	}
	// THE PENDING SCAN ENDS WITH THE CONTROLLER, rather than running on a transport that is gone.
	let deadline = clock() + 10 * TICKS;
	loop {
		let gone = !matches!(client.scanning(&scan), Some(Ok(true)));
		let empty = matches!(client.controllers(), Some(Ok(ref c)) if c.is_empty());
		if gone && empty {
			break;
		}
		if clock() >= deadline {
			fail(b"after the transport was lost the scan still ran or the controller was still listed");
		}
		sleep_until(clock() + TICKS / 4);
	}
	say(b"the transport was lost mid-scan; the scan ended and the controller left");
	// BUSY UNTIL THE DISABLE HAS LANDED, which is the answer the policy gives while its teardown is still in
	// flight - and the operator's move is to ask again once the node is disabled, not to take it as a no.
	let deadline = clock() + 10 * TICKS;
	loop {
		match admin.apply(&index, &PolicyVerb::Enable, "") {
			Some(Ok(PolicyOutcome::Accepted)) => break,
			Some(Ok(PolicyOutcome::Busy)) if clock() < deadline => sleep_until(clock() + TICKS / 4),
			Some(Ok(PolicyOutcome::Busy)) => fail(b"the fixture's device was still busy ten seconds after its disable"),
			Some(Ok(_)) => fail(b"the fixture's device could not be enabled again: the policy refused it"),
			_ => fail(b"the fixture's device could not be enabled again: the policy did not answer"),
		}
	}
	let _ = admin.apply(&index, &PolicyVerb::Retry, "");
	powered_controller(read);
	// THE SCAN FROM BEFORE THE LOSS IS STALE: it belonged to a controller session that ended, and the new
	// publication's controller answers for nothing that session held.
	if !matches!(client.results(&scan), Some(Err(_))) || !matches!(client.scanning(&scan), Some(Err(_))) {
		fail(b"loss: a scan handle from before the transport was lost still answered");
	}
	say(b"PASS loss: the controller came back as a new publication and was initialised again, and the scan from before it is refused");
}

// A request encoded by an interface's own client and sent by hand, so it can go down a connection of another
// interface.
#[derive(Default)]
struct Capture {
	bytes: Vec<u8>,
}

impl Transport for &mut Capture {
	fn call(&mut self, request: &[u8], _request_handles: &[u64], _reply_handles: &mut Handles, _deadline: u64) -> Result<Vec<u8>, TransportError> {
		self.bytes = request.to_vec();
		Err(TransportError::TimedOut)
	}
	fn discard_handles(&mut self, _handles: &[u64]) {}
}

// NO OPERATOR VERB AND NO KEY THROUGH A CLIENT'S CONNECTIONS. A radio power-off encoded as the operator interface
// encodes it, and the bond store's own `lookup` for the paired mouse, go down connections minted from the read and
// the operator grants. None is answered as what it claims to be - each connection answers its own interface's next
// request, which shows it was alive to answer - the radio stays on, and no connection a client holds hands out a
// key. The store's capability itself is BluetoothService's alone: no grant names it, so this is the only way a
// client could even try.
fn deny(read: u64, operator: u64) {
	powered_controller(read);
	let Some(Ok(controllers)) = bluetooth::Client::new(ChannelTransport { chan: read }).controllers() else { fail(b"deny: the controllers could not be listed") };
	let Some(local) = controllers.first().map(|controller| controller.address.clone()) else { fail(b"deny: there is no controller") };
	let mut power_off = Capture::default();
	let _ = bluetooth_operator::Client::new(&mut power_off).power(&0, &false);
	let mut lookup = Capture::default();
	let _ = bluetooth_bond_store::Client::new(&mut lookup).lookup(&local, &mouse());
	for (grant, request, is_operator) in [(read, &power_off.bytes, false), (read, &lookup.bytes, false), (operator, &lookup.bytes, true)] {
		let Some(connection) = service_connect(grant).filter(|&connection| connection != 0) else { fail(b"deny: a connection could not be minted") };
		if request.is_empty() || !send_blocking(connection, request, 0) {
			fail(b"deny: a forged request could not be sent");
		}
		if wait(connection, clock() + TICKS) >= 0 {
			fail(b"deny: a request of another interface was answered");
		}
		let alive = if is_operator { matches!(bluetooth_operator::Client::new(ChannelTransport { chan: connection }).bonded(&0), Some(Ok(bonds)) if bonds.iter().any(|bond| bond.address == mouse())) } else { matches!(bluetooth::Client::new(ChannelTransport { chan: connection }).controllers(), Some(Ok(controllers)) if controllers.first().is_some_and(|controller| controller.powered)) };
		close(connection);
		if !alive {
			fail(b"deny: after a forged request the connection did not answer its own interface, or the radio was off");
		}
	}
	say(b"PASS deny: a read connection powered nothing, and neither a read nor an operator connection answered the bond store's lookup");
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
	// The grants, in the order PermissionManager walks its vocabulary: the device inventory and the
	// operator's device policy, the pointer, ProcessService's accounting, the supervisor's admin channel,
	// then the two Bluetooth authorities last.
	let device = recv_tagged(bootstrap, &mut buf, b"DEVICE").unwrap_or(0);
	let policy = recv_tagged(bootstrap, &mut buf, b"DEVPOLICY").unwrap_or(0);
	let input_client = recv_tagged(bootstrap, &mut buf, b"INPUT").unwrap_or(0);
	let process_client = recv_tagged(bootstrap, &mut buf, b"PROCESS").unwrap_or(0);
	let supervisor = recv_tagged(bootstrap, &mut buf, b"SUPERVISOR").unwrap_or(0);
	let read = recv_tagged(bootstrap, &mut buf, CAP_BT_READ).unwrap_or(0);
	let operator = recv_tagged(bootstrap, &mut buf, CAP_BT_OPERATOR).unwrap_or(0);
	if device == 0 || policy == 0 || input_client == 0 || process_client == 0 || supervisor == 0 || read == 0 || operator == 0 {
		fail(b"a grant this probe needs was not delivered");
	}
	match args.split(|&b| b == b' ').next().unwrap_or(&[]) {
		b"pair" => pair(read, operator, input_client),
		b"reuse" => reuse(read, operator, input_client),
		b"forget" => forget(operator, input_client),
		b"limits" => limits(process_client),
		b"refund" => refund(process_client, supervisor, input_client, read),
		b"exhaust" => exhaust(read, input_client),
		b"loss" => loss(read, device, policy),
		b"deny" => deny(read, operator),
		_ => fail(b"usage: btcheck pair | reuse | forget | limits | refund | exhaust | loss | deny"),
	}
	exit();
}
