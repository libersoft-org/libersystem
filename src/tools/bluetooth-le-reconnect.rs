// Exact production connection handler/resolver and crypto/HCI parser are inserted
// by bluetooth_le_reconnect.py. Bond-store IPC and controller commands are mocked;
// these tests do not claim independent radio, kernel IPC or completed encryption.
extern crate alloc;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

/* EXACT_MODULES */
mod service_logic {
	pub use crate::bt_keys;
}
type Peer = [u8; 7];
const KIND_PUBLIC: u8 = 0;
const KIND_RANDOM: u8 = 1;
/* EXACT_KIND_BREDR */
const OWN_PUBLIC: u8 = 0;
const OWN_RANDOM: u8 = 1;
const MAX_SCAN_RESULTS: usize = 64;
const PAIRING_TICKS: u64 = 1000;
use hci_codec::opcode;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Error {
	Invalid,
	NotFound,
	Closed,
	Denied,
	Again,
	Unsupported,
	Exhausted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeerKind {
	Public,
	RandomStatic,
	Resolvable,
	Bredr,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct PeerAddress {
	kind: PeerKind,
	bytes: Vec<u8>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Radio {
	Le,
	Classic,
}
struct ServiceClass {
	uuid: u32,
}
struct ScanResult {
	address: PeerAddress,
	name: String,
	rssi: i32,
	human_interface: bool,
	radio: Radio,
	class_of_device: u32,
	services: Vec<ServiceClass>,
}
struct Scan {
	running: bool,
	results: Vec<ScanResult>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Agreement {
	Legacy,
	SecureConnections,
}
#[derive(Clone, Copy)]
struct Level {
	agreement: Agreement,
}
#[derive(Clone, Copy)]
enum SecurityLevel {
	None,
}
fn level_from_wire(level: &Level) -> Level {
	*level
}
fn peer_is_classic(peer: &Peer) -> bool {
	peer[0] == KIND_BREDR
}
fn clock() -> u64 {
	0
}
fn local_wire(controller: &Controller) -> PeerAddress {
	peer_to_wire(&local_address(controller))
}
struct OperatorView<'a> {
	stack: &'a mut Stack,
}
struct BondStore<'a> {
	records: &'a RefCell<Vec<(Peer, Record)>>,
	fail: bool,
}
impl BondStore<'_> {
	fn delete(&self, _: &PeerAddress, peer: &PeerAddress) -> Option<Result<(), Error>> {
		if self.fail {
			return Some(Err(Error::Closed));
		}
		let peer = peer_from_wire(peer).unwrap();
		self.records.borrow_mut().retain(|(held, _)| *held != peer);
		Some(Ok(()))
	}
}
thread_local! { static SCRUBBED: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) }; }
// Whether a case expects the private address rotated; any other rotation is a defect.
thread_local! { static ROTATING: Cell<bool> = const { Cell::new(false) }; }
const REASON_USER: u8 = 0x13;
const REASON_AUTHENTICATION: u8 = 0x05;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PairingState {
	Connecting,
	Pairing,
	Failed,
}
struct Attempt {
	peer: Peer,
	state: PairingState,
	deadline: u64,
	security: SecurityLevel,
}
#[derive(Clone, Debug)]
struct Link {
	peer: Peer,
	handle: u16,
	reconnecting: bool,
	local: Peer,
	pairing: Option<smp_pairing::Initiator>,
}
impl Link {
	fn new(handle: u16, peer: Peer) -> Self {
		Self { handle, peer, reconnecting: false, local: [0; 7], pairing: None }
	}
}
#[derive(Clone)]
struct Record {
	key: Vec<u8>,
	rand: Vec<u8>,
	ediv: u16,
	level: Level,
}
#[derive(Debug, PartialEq, Eq)]
enum Action {
	Disconnect(u16, u8),
	Secure(u16, [u8; 16]),
	Legacy(u16, [u8; 16], [u8; 8], u16),
	Pair(u16, Peer),
	Failed(Peer),
	Relist,
	Command(u16),
	Create(Vec<u8>),
	Rotate,
	Refused(u16),
}
struct BredrState {
	watcher: u64,
}
struct Controller {
	le: LeState,
	reconnect: Option<Peer>,
	attempt: Option<Attempt>,
	links: Vec<Link>,
	actions: Vec<Action>,
	bredr_state: BredrState,
	public_key: Option<[u8; 64]>,
	classic: bool,
	classic_sc: bool,
	powered: bool,
	secure_connections: bool,
	scan: Option<Scan>,
	// THE CONTROLLER'S COMMAND QUEUE, as production bounds it: a send past `MAX_QUEUED` is refused.
	pending: Vec<(u16, Vec<u8>)>,
}
impl Controller {
	fn command(&mut self, opcode: u16, parameters: &[u8]) -> bool {
		if self.pending.len() >= MAX_QUEUED {
			self.actions.push(Action::Refused(opcode));
			return false;
		}
		self.pending.push((opcode, parameters.to_vec()));
		self.actions.push(Action::Command(opcode));
		true
	}
	fn le_create_connection(&mut self, parameters: &[u8]) -> bool {
		if self.pending.len() >= MAX_QUEUED {
			self.actions.push(Action::Refused(0x200d));
			return false;
		}
		self.pending.push((0x200d, parameters.to_vec()));
		self.actions.push(Action::Create(parameters.to_vec()));
		true
	}
	fn le_scan_enable(&mut self, _: bool) -> bool {
		true
	}
	fn link(&self, handle: u16) -> Option<&Link> {
		self.links.iter().find(|link| link.handle == handle)
	}
	fn link_mut(&mut self, handle: u16) -> Option<&mut Link> {
		self.links.iter_mut().find(|link| link.handle == handle)
	}
	fn link_to(&self, peer: &Peer) -> Option<&Link> {
		self.links.iter().find(|link| &link.peer == peer)
	}

	fn disconnect(&mut self, handle: u16, reason: u8) {
		self.actions.push(Action::Disconnect(handle, reason));
	}
	fn encrypt(&mut self, handle: u16, key: &[u8; 16]) {
		self.actions.push(Action::Secure(handle, *key));
	}
	fn encrypt_legacy(&mut self, handle: u16, key: &[u8; 16], rand: &[u8; 8], ediv: u16) {
		self.actions.push(Action::Legacy(handle, *key, *rand, ediv));
	}
}
struct Stack {
	controllers: Vec<Controller>,
	bonds: u64,
	records: RefCell<Vec<(Peer, Record)>>,
	delete_fails: bool,
	reads: RefCell<Vec<Peer>>,
}
impl Stack {
	fn bonds(&self) -> BondStore<'_> {
		BondStore { records: &self.records, fail: self.delete_fails }
	}
	fn record(&self, _: usize, peer: &Peer) -> Option<Record> {
		self.records.borrow().iter().find(|(held, _)| held == peer).map(|(_, record)| record.clone())
	}
	fn refresh_policy(&mut self, _: usize) {}
	fn relist(&mut self, at: usize) {
		self.controllers[at].actions.push(Action::Relist);
	}
	fn pair_classic(&mut self, _: usize, _: Peer, _: bool) -> Result<(), Error> {
		panic!("unexpected classic pairing")
	}
	fn bond(&self, at: usize, peer: &Peer) -> Option<Record> {
		assert_eq!(at, 0);
		self.reads.borrow_mut().push(*peer);
		self.records.borrow().iter().find(|(held, _)| held == peer).map(|(_, record)| record.clone())
	}
	fn reconnect_bonded(&mut self, at: usize) {
		// PRODUCTION'S GUARD, kept by the stand-in: nothing while the operator's own LE attempt is still connecting.
		if self.controllers[at].attempt.as_ref().is_some_and(|attempt| attempt.peer[0] != KIND_BREDR && attempt.state == PairingState::Connecting) {
			return;
		}
		self.controllers[at].actions.push(Action::Relist);
	}
	fn rotate_address(&mut self, at: usize) {
		if !ROTATING.with(Cell::get) {
			panic!("unexpected private address rotation")
		}
		self.controllers[at].actions.push(Action::Rotate);
	}
	fn own_irk(&mut self, _: usize) -> Option<[u8; 16]> {
		Some(IRK)
	}
	fn run_smp(&mut self, at: usize, handle: u16, steps: Vec<smp_pairing::Step>) {
		assert_eq!(steps.len(), 1);
		let peer = self.controllers[at].link(handle).unwrap().pairing.as_ref().unwrap().peer;
		self.controllers[at].actions.push(Action::Pair(handle, peer));
	}
}
fn local_address(_: &Controller) -> Peer {
	[0; 7]
}
fn random_get(bytes: &mut [u8]) -> usize {
	bytes.fill(0x35);
	bytes.len()
}
fn print(_: &[u8]) {}
const IO_KEYBOARD_DISPLAY: u8 = 4;
const IO_NO_INPUT_NO_OUTPUT: u8 = 3;
struct Options {
	io: u8,
	legacy: bool,
	irk: [u8; 16],
	identity: Peer,
	cross_transport: bool,
}
mod smp_pairing {
	use super::*;
	#[derive(Clone, Debug)]
	pub struct Initiator {
		pub peer: Peer,
		pub legacy: bool,
		pub public_key_present: bool,
	}
	pub struct Step;
	impl Initiator {
		pub fn start(_: Peer, peer: Peer, public_key: Option<[u8; 64]>, _: [u8; 16], options: Options) -> (Self, Step) {
			(Self { peer, legacy: options.legacy, public_key_present: public_key.is_some() }, Step)
		}
	}
}
fn scrub(bytes: &mut [u8]) {
	SCRUBBED.with(|held| held.borrow_mut().push(bytes.to_vec()));
	bytes.fill(0);
}
fn scrub_record(record: &mut Record) {
	scrub(&mut record.key);
	scrub(&mut record.rand);
}

/* EXACT_PRODUCTION */

const IDENTITY: Peer = [1, 0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xc3];
const IRK: [u8; 16] = [0xec, 0x02, 0x34, 0xa3, 0x57, 0xc8, 0xad, 0x05, 0x34, 0x10, 0x10, 0xa6, 0x0a, 0x39, 0x7d, 0x9b];
fn address(peer: Peer) -> [u8; 6] {
	peer[1..].try_into().unwrap()
}
fn as_peer(kind: u8, address: [u8; 6]) -> Peer {
	let mut peer = [0; 7];
	peer[0] = kind;
	peer[1..].copy_from_slice(&address);
	peer
}
fn stack(identity: Peer, legacy: bool) -> Stack {
	let mut le = LeState::new();
	le.accepting = true;
	le.irks.push((identity, IRK));
	Stack { controllers: vec![Controller { le, reconnect: None, attempt: None, links: Vec::new(), actions: Vec::new(), bredr_state: BredrState { watcher: 1 }, public_key: Some([0; 64]), classic: false, classic_sc: false, powered: true, secure_connections: true, scan: None, pending: Vec::new() }], bonds: 1, records: RefCell::new(vec![(identity, Record { key: vec![0x42; 16], rand: if legacy { vec![0x19; 8] } else { Vec::new() }, ediv: 0x1234, level: Level { agreement: if legacy { Agreement::Legacy } else { Agreement::SecureConnections } } })]), delete_fails: false, reads: RefCell::new(Vec::new()) }
}
fn connect(stack: &mut Stack, status: u8, kind: u8, address: [u8; 6]) {
	// Actual legacy HCI parser, including little-endian address conversion, then
	// the exact production handler. The event carries the raw rotated address.
	let mut body = vec![status, 7, 0, 0, kind];
	body.extend(address.iter().rev());
	body.extend([0; 7]);
	let mut event = vec![0x3e, 19, 1];
	event.extend(body);
	let hci_codec::Event::Connected { status, handle, peer_kind, peer } = hci_codec::event(&event).unwrap() else { panic!("not a connection event") };
	stack.on_connected(0, status, handle, peer_kind, peer);
}
// A CONNECTION THROUGH THE ACCEPT LIST takes the standing attempt up again at once, for the bonded peers still absent -
// behind what is decided about the device it connected, its encryption above all, which goes first.
fn rearmed(through_list: bool, then: Action) -> Vec<Action> {
	if through_list { vec![then, Action::Relist] } else { vec![then] }
}
fn reused(stack: &Stack, identity: Peer, legacy: bool, through_list: bool) {
	assert_eq!(*stack.reads.borrow(), [identity], "only stored identity may select its bond");
	assert_eq!(stack.controllers[0].links.len(), 1);
	assert_eq!(stack.controllers[0].links[0].peer, identity);
	assert!(stack.controllers[0].links[0].reconnecting);
	let action = if legacy { Action::Legacy(7, [0x42; 16], [0x19; 8], 0x1234) } else { Action::Secure(7, [0x42; 16]) };
	assert_eq!(stack.controllers[0].actions, rearmed(through_list, action), "reconnect must request encryption with the stored key");
}
fn refused(stack: &Stack, through_list: bool, reason: u8) {
	assert_eq!(stack.controllers[0].actions, rearmed(through_list, Action::Disconnect(7, reason)));
}

fn connection_cases() {
	let rpa = bt_keys::private_address(&IRK, 0x12_3456);
	for legacy in [true, false] {
		let mut s = stack(IDENTITY, legacy);
		connect(&mut s, 0, 1, rpa);
		reused(&s, IDENTITY, legacy, true);
	}
	// Static random and already-resolved random identities preserve their kind.
	for kind in [1, 3] {
		let mut s = stack(IDENTITY, true);
		connect(&mut s, 0, kind, address(IDENTITY));
		reused(&s, IDENTITY, true, true);
	}
	// Public and controller-resolved public identities remain public.
	let public = [0, 0xda, 0x4c, 0x10, 0xde, 0, 0x0b];
	for kind in [0, 2] {
		let mut s = stack(public, false);
		connect(&mut s, 0, kind, address(public));
		reused(&s, public, false, true);
	}
	// Public-address bytes that happen to look like a valid RPA are not resolved.
	let mut s = stack(IDENTITY, true);
	connect(&mut s, 0, 0, rpa);
	refused(&s, true, REASON_AUTHENTICATION);
	assert_eq!(*s.reads.borrow(), [as_peer(0, rpa)]);
	let unknown = bt_keys::private_address(&[0x87; 16], 0x12_3456);
	let mut s = stack(IDENTITY, true);
	connect(&mut s, 0, 1, unknown);
	refused(&s, true, REASON_AUTHENTICATION);
	assert_eq!(*s.reads.borrow(), [as_peer(1, unknown)]);
	// A stale resolving entry cannot resurrect a forgotten bond or an invalid key.
	let mut s = stack(IDENTITY, true);
	s.records.borrow_mut().clear();
	connect(&mut s, 0, 1, rpa);
	refused(&s, true, REASON_AUTHENTICATION);
	let mut s = stack(IDENTITY, true);
	s.records.borrow_mut()[0].1.key.pop();
	connect(&mut s, 0, 1, rpa);
	refused(&s, true, REASON_AUTHENTICATION);
	// Resolve before duplicate admission, so the same identity cannot take a second slot.
	let mut s = stack(IDENTITY, true);
	s.controllers[0].links.push(Link::new(6, IDENTITY));
	connect(&mut s, 0, 1, rpa);
	refused(&s, false, REASON_USER);
	assert_eq!(s.controllers[0].links.len(), 1);
	assert!(s.reads.borrow().is_empty());
	// Failed completion retains cancellation/relist semantics without bond-store lookup.
	let mut s = stack(IDENTITY, true);
	s.controllers[0].le.relist = true;
	connect(&mut s, 2, 1, rpa);
	assert_eq!(s.controllers[0].actions, [Action::Relist]);
	assert!(s.controllers[0].attempt.is_none());
	assert!(s.controllers[0].links.is_empty() && s.reads.borrow().is_empty());
	// Knowing an IRK is not authority to admit an unsolicited link.
	let mut s = stack(IDENTITY, true);
	s.controllers[0].le.accepting = false;
	connect(&mut s, 0, 1, rpa);
	refused(&s, false, REASON_USER);
	assert!(s.reads.borrow().is_empty());
	// First pairing to an unknown RPA keeps its explicit operator attempt.
	let mut s = stack(IDENTITY, true);
	s.controllers[0].le.accepting = false;
	s.controllers[0].attempt = Some(Attempt { peer: as_peer(1, unknown), state: PairingState::Connecting, deadline: 1000, security: SecurityLevel::None });
	connect(&mut s, 0, 1, unknown);
	assert_eq!(s.controllers[0].actions, [Action::Pair(7, as_peer(1, unknown))]);
	assert!(s.reads.borrow().is_empty());
	// An explicit reconnect also selects the canonical stored identity.
	let mut s = stack(IDENTITY, true);
	s.controllers[0].le.accepting = false;
	s.controllers[0].reconnect = Some(IDENTITY);
	connect(&mut s, 0, 1, rpa);
	reused(&s, IDENTITY, true, false);
	// Re-pairing an existing resolved identity keeps identity bookkeeping, while
	// the exact pairing-start method passes raw RPA/type to SMP c1/f5/f6.
	let mut s = stack(IDENTITY, true);
	s.controllers[0].le.accepting = false;
	s.controllers[0].le.legacy = Some(IDENTITY);
	s.controllers[0].attempt = Some(Attempt { peer: IDENTITY, state: PairingState::Connecting, deadline: 1000, security: SecurityLevel::None });
	connect(&mut s, 0, 1, rpa);
	assert_eq!(s.controllers[0].links[0].peer, IDENTITY);
	assert_eq!(s.controllers[0].actions, [Action::Pair(7, as_peer(1, rpa))], "SMP must receive the raw connection address");
	let pairing = s.controllers[0].links[0].pairing.as_ref().unwrap();
	assert!(pairing.legacy && pairing.public_key_present, "legacy policy is keyed by canonical identity, and Secure Connections is still offered beside it");
	assert_eq!(s.controllers[0].attempt.as_ref().unwrap().state, PairingState::Pairing);
	assert!(s.reads.borrow().is_empty());
	println!("16 actual-handler connection identity/security/compatibility cases passed");
}

fn fresh_scan(stack: &mut Stack, rpa: [u8; 6]) -> PeerAddress {
	stack.controllers[0].scan = Some(Scan { running: true, results: Vec::new() });
	stack.on_advertisements(0, vec![hci_codec::Advertisement { kind: 1, address: rpa, rssi: -30, data: [0; 31], data_len: 0 }]);
	let results = &stack.controllers[0].scan.as_ref().unwrap().results;
	assert_eq!(results.len(), 1);
	results[0].address.clone()
}
fn forget_cases() {
	let rpa = bt_keys::private_address(&IRK, 0x12_3456);
	let mut s = stack(IDENTITY, true);
	let other = [1, 0xc0, 1, 2, 3, 4, 5];
	let other_irk = [0x73; 16];
	s.controllers[0].le.irks.push((other, other_irk));
	let record = s.records.borrow()[0].1.clone();
	s.records.borrow_mut().push((other, record));
	assert_eq!(fresh_scan(&mut s, rpa), peer_to_wire(&IDENTITY));
	SCRUBBED.with(|held| held.borrow_mut().clear());
	assert_eq!(OperatorView { stack: &mut s }.forget(0, peer_to_wire(&IDENTITY)), Ok(()));
	assert!(s.record(0, &IDENTITY).is_none() && s.record(0, &other).is_some());
	assert_eq!(s.controllers[0].le.irks, [(other, other_irk)], "successful forget must retire only the matching cached IRK");
	SCRUBBED.with(|held| assert_eq!(*held.borrow(), [IRK.to_vec()], "forgotten IRK must be scrubbed before removal"));
	let scanned = fresh_scan(&mut s, rpa);
	assert_eq!(scanned, peer_to_wire(&as_peer(1, rpa)), "fresh scan after forget must return the actual private address");
	// A rebuilt controller list excludes the deleted identity. Capture the actual
	// direct create-connection bytes; the model does not simulate a radio match.
	s.controllers[0].le.accepting = false;
	s.controllers[0].actions.clear();
	assert_eq!(s.pair(0, &scanned, true), Ok(()));
	assert_eq!(s.controllers[0].attempt.as_ref().unwrap().peer, as_peer(1, rpa));
	assert_eq!(s.controllers[0].actions, [Action::Create(hci_codec::create_connection_from(1, &rpa, OWN_PUBLIC).to_vec())], "re-pair must address the freshly advertised RPA");
	let mut s = stack(IDENTITY, true);
	s.delete_fails = true;
	SCRUBBED.with(|held| held.borrow_mut().clear());
	assert_eq!(OperatorView { stack: &mut s }.forget(0, peer_to_wire(&IDENTITY)), Err(Error::Closed), "failed durable deletion must fail before changing live state");
	assert_eq!(s.controllers[0].le.irks, [(IDENTITY, IRK)], "failed durable deletion must retain the cached IRK");
	assert!(s.record(0, &IDENTITY).is_some() && s.controllers[0].actions.is_empty());
	SCRUBBED.with(|held| assert!(held.borrow().is_empty(), "failed deletion must not scrub the retained key"));
	assert_eq!(fresh_scan(&mut s, rpa), peer_to_wire(&IDENTITY));
	println!("2 actual-handler successful/failed forget, scan and pairing cases passed");
}
fn retained_raw_scan(stack: &mut Stack, raw: [u8; 6]) -> PeerAddress {
	let irks = std::mem::take(&mut stack.controllers[0].le.irks);
	let scanned = fresh_scan(stack, raw);
	stack.controllers[0].le.irks = irks;
	scanned
}
fn pair_completion_cases() {
	let rpa = bt_keys::private_address(&IRK, 0x12_3456);
	for legacy in [false, true] {
		let mut s = stack(IDENTITY, legacy);
		s.controllers[0].le.accepting = false;
		let scanned = retained_raw_scan(&mut s, rpa);
		assert_eq!(s.pair(0, &scanned, legacy), Ok(()));
		assert_eq!(s.controllers[0].attempt.as_ref().unwrap().peer, IDENTITY, "raw-RPA scan choice must create a canonical pairing attempt");
		assert_eq!(s.controllers[0].actions, [Action::Create(hci_codec::create_connection_from(1, &rpa, OWN_PUBLIC).to_vec())]);
		s.controllers[0].actions.clear();
		connect(&mut s, 0, 1, rpa);
		assert_eq!(s.controllers[0].actions, [Action::Pair(7, as_peer(1, rpa))], "retained raw scan must start fresh SMP with the raw connection peer");
		assert_eq!(s.controllers[0].links[0].peer, IDENTITY);
		let pairing = s.controllers[0].links[0].pairing.as_ref().unwrap();
		assert_eq!(pairing.legacy, legacy);
		assert!(pairing.public_key_present, "Secure Connections is offered whether or not the operator asked for legacy");
		assert_eq!(s.controllers[0].attempt.as_ref().unwrap().state, PairingState::Pairing);
	}
	let mut s = stack(IDENTITY, false);
	s.controllers[0].le.accepting = false;
	let scanned = retained_raw_scan(&mut s, rpa);
	assert_eq!(s.pair(0, &scanned, true), Err(Error::Denied), "raw scan must not bypass legacy-over-SC bond policy");
	assert!(s.controllers[0].attempt.is_none() && s.controllers[0].actions.is_empty());
	let mut s = stack(IDENTITY, true);
	s.controllers[0].le.accepting = false;
	let scanned = retained_raw_scan(&mut s, rpa);
	s.controllers[0].links.push(Link::new(6, IDENTITY));
	assert_eq!(s.pair(0, &scanned, true), Err(Error::Again));
	assert!(s.controllers[0].attempt.is_none() && s.controllers[0].actions.is_empty());
	for relist in [false, true] {
		let mut s = stack(IDENTITY, true);
		s.controllers[0].le.accepting = false;
		let scanned = retained_raw_scan(&mut s, rpa);
		assert_eq!(s.pair(0, &scanned, true), Ok(()));
		s.controllers[0].actions.clear();
		s.controllers[0].le.accepting = relist;
		s.controllers[0].le.relist = relist;
		connect(&mut s, 2, 1, rpa);
		assert_eq!(s.controllers[0].attempt.as_ref().unwrap().state, PairingState::Failed, "failed raw-RPA completion must end the canonical attempt");
		assert!(s.controllers[0].links.is_empty() && s.reads.borrow().is_empty());
		// THE STANDING ATTEMPT IS TAKEN UP AGAIN EITHER WAY: a cancellation for a new list, or the operator's attempt it was
		// set aside for ending without a link.
		assert_eq!(s.controllers[0].actions, vec![Action::Relist]);
	}
	let unknown = as_peer(1, bt_keys::private_address(&[0x87; 16], 0x12_3456));
	let public = [0, 0xda, 0x4c, 0x10, 0xde, 0, 0x0b];
	for (choice, kind) in [(unknown, 1), (public, 0), (IDENTITY, 1), (public, 2), (IDENTITY, 3), (as_peer(0, rpa), 0)] {
		let mut s = stack(IDENTITY, true);
		s.controllers[0].le.accepting = false;
		let scanned = peer_to_wire(&choice);
		s.controllers[0].scan = Some(Scan { running: false, results: vec![ScanResult { address: scanned.clone(), name: String::new(), rssi: -30, human_interface: true, radio: Radio::Le, class_of_device: 0, services: Vec::new() }] });
		assert_eq!(s.pair(0, &scanned, true), Ok(()));
		assert_eq!(s.controllers[0].attempt.as_ref().unwrap().peer, choice);
		s.controllers[0].actions.clear();
		connect(&mut s, 2, kind, address(choice));
		assert_eq!(s.controllers[0].attempt.as_ref().unwrap().state, PairingState::Failed);
		// The operator's attempt ended without a link: the standing attempt it was set aside for is taken up again.
		assert!(s.controllers[0].actions == [Action::Relist] && s.controllers[0].links.is_empty());
	}
	let mut s = stack(IDENTITY, true);
	s.controllers[0].le.accepting = false;
	let scanned = retained_raw_scan(&mut s, rpa);
	assert_eq!(s.pair(0, &scanned, true), Ok(()));
	s.controllers[0].actions.clear();
	connect(&mut s, 2, 1, address(unknown));
	assert_eq!(s.controllers[0].attempt.as_ref().unwrap().state, PairingState::Connecting, "unrelated completion must not fail the watched attempt");
	assert!(s.controllers[0].actions.is_empty());
	println!("13 actual pair-to-completion identity/policy/failure cases passed");
}
// A ROTATION WITH THE ATTEMPT OUT, its cancellation overtaken by a connection through the list: the rotate mark is
// consumed - left set, it stopped every later rotation while the attempt it re-armed was out - the address is rotated,
// and only then is the attempt taken up again.
fn rotation_cases() {
	let rpa = bt_keys::private_address(&IRK, 0x12_3456);
	let mut s = stack(IDENTITY, false);
	s.controllers[0].le.relist = true;
	s.controllers[0].le.rotate = true;
	ROTATING.with(|rotating| rotating.set(true));
	connect(&mut s, 0, 1, rpa);
	ROTATING.with(|rotating| rotating.set(false));
	assert_eq!(s.controllers[0].actions, [Action::Secure(7, [0x42; 16]), Action::Rotate, Action::Relist], "the overtaken rotation is made before the attempt is taken up again");
	assert!(!s.controllers[0].le.rotate && !s.controllers[0].le.relist, "and both marks are consumed");
	// AND WHEN THAT CONNECTION IS REFUSED - the link bound reached, or the peer already linked - the rotation is still
	// made: left set, the mark stopped every later rotation. The attempt is not taken up again at the bound.
	let mut s = stack(IDENTITY, true);
	s.controllers[0].le.rotate = true;
	s.controllers[0].links.push(Link::new(6, IDENTITY));
	ROTATING.with(|rotating| rotating.set(true));
	connect(&mut s, 0, 1, rpa);
	ROTATING.with(|rotating| rotating.set(false));
	assert_eq!(s.controllers[0].actions, [Action::Disconnect(7, REASON_USER), Action::Rotate], "a refused list connection still makes the rotation it held off");
	assert!(!s.controllers[0].le.rotate, "and consumes its mark");
	println!("2 actual rotation-overtaken cases passed");
}
// THE STANDING ATTEMPT'S REBUILD, through production's `feed_rebuild` and a queue bounded as the controller's is: seven
// absent bonded peers are eighteen commands, more than the queue holds. Fed as it has room, the rebuild keeps
// `REBUILD_HEADROOM` places free for a link's encryption, none of it is refused, and the attempt is queued behind the
// lists - in one burst the queue overran and the attempt, queued last, was the command refused.
fn rebuild_cases() {
	let mut s = stack(IDENTITY, false);
	let controller = &mut s.controllers[0];
	controller.le.rebuild.push_back((0x202d, vec![0]));
	controller.le.rebuild.push_back((0x2010, Vec::new()));
	controller.le.rebuild.push_back((0x2029, Vec::new()));
	for peer in 0..7u8 {
		controller.le.rebuild.push_back((0x2011, vec![peer; 7]));
		controller.le.rebuild.push_back((0x2027, vec![peer; 39]));
	}
	controller.le.rebuild.push_back((0x202d, vec![1]));
	controller.le.arming = true;
	let wanted = controller.le.rebuild.len();
	controller.feed_rebuild();
	assert!(controller.pending.len() + REBUILD_HEADROOM <= MAX_QUEUED, "the first feed leaves room: {} queued", controller.pending.len());
	assert!(controller.command(0x2019, &[0; 28]), "a link's encryption is queued while the rebuild is fed");
	let mut rounds = 0;
	while (controller.le.arming || !controller.le.rebuild.is_empty()) && rounds < 16 {
		controller.pending.clear();
		controller.feed_rebuild();
		assert!(controller.pending.len() <= MAX_QUEUED);
		rounds += 1;
	}
	assert!(!controller.actions.iter().any(|action| matches!(action, Action::Refused(_))), "nothing the rebuild sent was refused: {:?}", controller.actions);
	let commands = controller.actions.iter().filter(|action| matches!(action, Action::Command(op) if *op != 0x2019)).count();
	assert_eq!(commands, wanted, "every list command was queued");
	assert!(matches!(controller.actions.last(), Some(Action::Create(_))) && controller.le.accepting, "the attempt is queued behind the lists: {:?}", controller.actions.last());
	println!("1 actual rebuild case passed");
}
fn main() {
	if !std::env::args().any(|arg| arg == "--forget-only") {
		connection_cases();
		rotation_cases();
		rebuild_cases();
	}
	forget_cases();
	if !std::env::args().any(|arg| arg == "--forget-only") {
		pair_completion_cases();
	}
}
