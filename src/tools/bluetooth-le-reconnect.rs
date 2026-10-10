// Exact production connection handler/resolver and crypto/HCI parser are inserted
// by bluetooth_le_reconnect.py. Bond-store IPC and controller commands are mocked;
// these tests do not claim independent radio, kernel IPC or completed encryption.
#![allow(dead_code)]
extern crate alloc;
use std::cell::RefCell;

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
}
impl Controller {
	fn command(&mut self, opcode: u16, _: &[u8]) -> bool {
		self.actions.push(Action::Command(opcode));
		true
	}
	fn le_create_connection(&mut self, parameters: &[u8]) -> bool {
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
		self.controllers[at].actions.push(Action::Relist);
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
	Stack { controllers: vec![Controller { le, reconnect: None, attempt: None, links: Vec::new(), actions: Vec::new(), bredr_state: BredrState { watcher: 1 }, public_key: Some([0; 64]), classic: false, classic_sc: false, powered: true, secure_connections: true, scan: None }], bonds: 1, records: RefCell::new(vec![(identity, Record { key: vec![0x42; 16], rand: if legacy { vec![0x19; 8] } else { Vec::new() }, ediv: 0x1234, level: Level { agreement: if legacy { Agreement::Legacy } else { Agreement::SecureConnections } } })]), delete_fails: false, reads: RefCell::new(Vec::new()) }
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
fn reused(stack: &Stack, identity: Peer, legacy: bool) {
	assert_eq!(*stack.reads.borrow(), [identity], "only stored identity may select its bond");
	assert_eq!(stack.controllers[0].links.len(), 1);
	assert_eq!(stack.controllers[0].links[0].peer, identity);
	assert!(stack.controllers[0].links[0].reconnecting);
	let action = if legacy { Action::Legacy(7, [0x42; 16], [0x19; 8], 0x1234) } else { Action::Secure(7, [0x42; 16]) };
	assert_eq!(stack.controllers[0].actions, [action], "reconnect must request encryption with the stored key");
}
fn refused(stack: &Stack, reason: u8) {
	assert_eq!(stack.controllers[0].actions, [Action::Disconnect(7, reason)]);
}

fn connection_cases() {
	let rpa = bt_keys::private_address(&IRK, 0x12_3456);
	for legacy in [true, false] {
		let mut s = stack(IDENTITY, legacy);
		connect(&mut s, 0, 1, rpa);
		reused(&s, IDENTITY, legacy);
	}
	// Static random and already-resolved random identities preserve their kind.
	for kind in [1, 3] {
		let mut s = stack(IDENTITY, true);
		connect(&mut s, 0, kind, address(IDENTITY));
		reused(&s, IDENTITY, true);
	}
	// Public and controller-resolved public identities remain public.
	let public = [0, 0xda, 0x4c, 0x10, 0xde, 0, 0x0b];
	for kind in [0, 2] {
		let mut s = stack(public, false);
		connect(&mut s, 0, kind, address(public));
		reused(&s, public, false);
	}
	// Public-address bytes that happen to look like a valid RPA are not resolved.
	let mut s = stack(IDENTITY, true);
	connect(&mut s, 0, 0, rpa);
	refused(&s, REASON_AUTHENTICATION);
	assert_eq!(*s.reads.borrow(), [as_peer(0, rpa)]);
	let unknown = bt_keys::private_address(&[0x87; 16], 0x12_3456);
	let mut s = stack(IDENTITY, true);
	connect(&mut s, 0, 1, unknown);
	refused(&s, REASON_AUTHENTICATION);
	assert_eq!(*s.reads.borrow(), [as_peer(1, unknown)]);
	// A stale resolving entry cannot resurrect a forgotten bond or an invalid key.
	let mut s = stack(IDENTITY, true);
	s.records.borrow_mut().clear();
	connect(&mut s, 0, 1, rpa);
	refused(&s, REASON_AUTHENTICATION);
	let mut s = stack(IDENTITY, true);
	s.records.borrow_mut()[0].1.key.pop();
	connect(&mut s, 0, 1, rpa);
	refused(&s, REASON_AUTHENTICATION);
	// Resolve before duplicate admission, so the same identity cannot take a second slot.
	let mut s = stack(IDENTITY, true);
	s.controllers[0].links.push(Link::new(6, IDENTITY));
	connect(&mut s, 0, 1, rpa);
	refused(&s, REASON_USER);
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
	refused(&s, REASON_USER);
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
	reused(&s, IDENTITY, true);
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
	assert!(pairing.legacy && !pairing.public_key_present, "legacy policy is keyed by canonical identity");
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
		assert_eq!(pairing.public_key_present, !legacy);
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
		assert_eq!(s.controllers[0].actions, if relist { vec![Action::Relist] } else { Vec::new() });
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
		assert!(s.controllers[0].actions.is_empty() && s.controllers[0].links.is_empty());
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
fn main() {
	if !std::env::args().any(|arg| arg == "--forget-only") {
		connection_cases();
	}
	forget_cases();
	if !std::env::args().any(|arg| arg == "--forget-only") {
		pair_completion_cases();
	}
}
