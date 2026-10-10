// Exact production LE Audio advertising/bonded methods, privacy resolver, RSI parser
// and cryptography are inserted by prepare.py. Storage IPC and pair_le are boundary
// mocks. The companion production-pair regression must prove HCI/SMP dispatch.
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
const KIND_BREDR: u8 = 3;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Error {
	Again,
	NotFound,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Profile {
	Audio,
}
#[derive(Clone)]
struct Record {
	trusted: Vec<Profile>,
}
struct Controller {
	le: LeState,
}
struct LeDevice {
	at: usize,
	sirk: Option<[u8; 16]>,
	size: u8,
	members: Vec<Peer>,
	search_until: Option<u64>,
}
struct Stack {
	controllers: Vec<Controller>,
	le_devices: Vec<LeDevice>,
	records: RefCell<Vec<(Peer, Record)>>,
	reads: RefCell<Vec<Peer>>,
	writes: RefCell<Vec<Peer>>,
	pairs: Vec<(usize, Peer, bool)>,
	pair_fails: bool,
}
impl Stack {
	fn record(&self, _: usize, peer: &Peer) -> Option<Record> {
		self.reads.borrow_mut().push(*peer);
		self.records.borrow().iter().find(|(held, _)| held == peer).map(|(_, r)| r.clone())
	}
	fn rewrite_bond(&self, _: usize, peer: &Peer, change: impl FnOnce(&mut Record)) -> Result<(), Error> {
		let mut records = self.records.borrow_mut();
		let Some((_, record)) = records.iter_mut().find(|(held, _)| held == peer) else { return Err(Error::NotFound) };
		change(record);
		self.writes.borrow_mut().push(*peer);
		Ok(())
	}
	fn pair_le(&mut self, at: usize, connection_peer: Peer, legacy: bool) -> Result<(), Error> {
		self.pairs.push((at, connection_peer, legacy));
		if self.pair_fails { Err(Error::Again) } else { Ok(()) }
	}
}
fn print(_: &[u8]) {}
/* EXACT_PRODUCTION */
const IDENTITY: Peer = [1, 0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xc3];
const FIRST: Peer = [1, 0xc2, 1, 2, 3, 4, 5];
const IRK: [u8; 16] = [0xec, 0x02, 0x34, 0xa3, 0x57, 0xc8, 0xad, 0x05, 0x34, 0x10, 0x10, 0xa6, 0x0a, 0x39, 0x7d, 0x9b];
const SIRK: [u8; 16] = [0x61; 16];
fn raw_peer(known: bool) -> Peer {
	let address = bt_keys::private_address(if known { &IRK } else { &[0x87; 16] }, 0x123456);
	let mut peer = [0; 7];
	peer[0] = KIND_RANDOM;
	peer[1..].copy_from_slice(&address);
	peer
}
fn advertisement() -> Vec<u8> {
	let rsi = lea::make_rsi(&SIRK, 0x123456);
	let mut data = vec![7, lea::ad::RSI];
	data.extend(rsi);
	data
}
fn stack() -> Stack {
	let mut le = LeState::new();
	le.irks.push((IDENTITY, IRK));
	Stack { controllers: vec![Controller { le }], le_devices: vec![LeDevice { at: 0, sirk: Some(SIRK), size: 3, members: vec![FIRST], search_until: Some(100) }], records: RefCell::new(Vec::new()), reads: RefCell::new(Vec::new()), writes: RefCell::new(Vec::new()), pairs: Vec::new(), pair_fails: false }
}
fn advertise(s: &mut Stack, peer: Peer) {
	s.le_audio_advertised(0, &peer_to_wire(&peer), &advertisement());
}
fn known_bond_skips_repair() {
	let mut s = stack();
	s.records.borrow_mut().push((IDENTITY, Record { trusted: vec![Profile::Audio] }));
	advertise(&mut s, raw_peer(true));
	assert!(s.pairs.is_empty(), "known canonical bond must prevent redundant set pairing");
	assert_eq!(*s.reads.borrow(), [IDENTITY]);
	assert_eq!(s.le_devices[0].members, [FIRST]);
}
fn cached_member_skips_without_record() {
	let mut s = stack();
	s.le_devices[0].members.push(IDENTITY);
	advertise(&mut s, raw_peer(true));
	assert!(s.pairs.is_empty(), "canonical cached member must prevent pairing even when bond record is unavailable");
	assert!(s.reads.borrow().is_empty());
	assert_eq!(s.le_devices[0].members, [FIRST, IDENTITY]);
}
fn missing_record_preserves_pairing_and_callback() {
	let mut s = stack();
	let raw = raw_peer(true);
	advertise(&mut s, raw);
	assert_eq!(s.pairs, [(0, raw, false)], "set pairing must retain the raw connection address");
	assert_eq!(s.le_devices[0].members, [FIRST, IDENTITY], "pending set member must match canonical pairing bookkeeping");
	assert_eq!(*s.reads.borrow(), [IDENTITY]);
	s.records.borrow_mut().push((IDENTITY, Record { trusted: Vec::new() }));
	// Actual pair/on_connected regression proves that a known-RPA link uses IDENTITY.
	// The real bonded callback therefore receives that canonical old link peer.
	s.le_audio_bonded(0, &IDENTITY, &IDENTITY);
	assert_eq!(s.le_devices[0].members, [FIRST, IDENTITY]);
	assert_eq!(*s.writes.borrow(), [IDENTITY], "canonical member must receive existing set audio trust at actual bonded callback");
	assert_eq!(s.records.borrow()[0].1.trusted, [Profile::Audio]);
}
fn repeated_private_address_keeps_one_member() {
	let mut s = stack();
	s.le_devices[0].size = 4;
	advertise(&mut s, raw_peer(true));
	let mut rotated = raw_peer(true);
	rotated[1..].copy_from_slice(&bt_keys::private_address(&IRK, 0x765432));
	advertise(&mut s, rotated);
	assert_eq!(s.pairs.len(), 1, "rotated RPA must not enqueue a duplicate canonical set member");
	assert_eq!(s.le_devices[0].members, [FIRST, IDENTITY]);
}
fn unknown_rpa_preserves_raw_until_bonded() {
	let mut s = stack();
	let raw = raw_peer(false);
	advertise(&mut s, raw);
	assert_eq!(s.pairs, [(0, raw, false)]);
	assert_eq!(s.le_devices[0].members, [FIRST, raw]);
	let identity = [1, 0xc9, 1, 2, 3, 4, 5];
	s.records.borrow_mut().push((identity, Record { trusted: Vec::new() }));
	s.le_audio_bonded(0, &raw, &identity);
	assert_eq!(s.le_devices[0].members, [FIRST, identity]);
	assert_eq!(*s.writes.borrow(), [identity]);
}
fn public_rpa_bytes_do_not_resolve() {
	let mut s = stack();
	let mut peer = raw_peer(true);
	peer[0] = KIND_PUBLIC;
	s.records.borrow_mut().push((IDENTITY, Record { trusted: vec![Profile::Audio] }));
	advertise(&mut s, peer);
	assert_eq!(s.pairs, [(0, peer, false)], "public address bytes must not resolve as a random private address");
	assert_eq!(s.le_devices[0].members, [FIRST, peer]);
}
fn static_random_stays_static() {
	let mut s = stack();
	let peer = [1, 0xc8, 1, 2, 3, 4, 5];
	advertise(&mut s, peer);
	assert_eq!(s.pairs, [(0, peer, false)]);
	assert_eq!(s.le_devices[0].members, [FIRST, peer]);
}
fn rejected_pair_does_not_add_member() {
	let mut s = stack();
	s.pair_fails = true;
	let raw = raw_peer(true);
	advertise(&mut s, raw);
	assert_eq!(s.pairs, [(0, raw, false)]);
	assert_eq!(s.le_devices[0].members, [FIRST]);
}
fn actual_rsi_validation_retained() {
	for data in [
		vec![],
		vec![7, lea::ad::RSI, 0],
		{
			let rsi = lea::make_rsi(&[0x12; 16], 0x123456);
			let mut v = vec![7, lea::ad::RSI];
			v.extend(rsi);
			v
		},
		{
			let mut v = advertisement();
			v[7] |= 0xc0;
			v
		},
	] {
		let mut s = stack();
		s.le_audio_advertised(0, &peer_to_wire(&raw_peer(true)), &data);
		assert!(s.pairs.is_empty() && s.reads.borrow().is_empty());
		assert_eq!(s.le_devices[0].members, [FIRST]);
	}
}
fn controller_and_capacity_bounds_retained() {
	let mut s = stack();
	s.le_devices[0].at = 1;
	advertise(&mut s, raw_peer(true));
	assert!(s.pairs.is_empty());
	let mut s = stack();
	s.le_devices[0].size = 1;
	advertise(&mut s, raw_peer(true));
	assert!(s.pairs.is_empty());
}
fn main() {
	let cases: [(&str, fn()); 10] = [
		("known-bond", known_bond_skips_repair),
		("cached-member", cached_member_skips_without_record),
		("missing-record", missing_record_preserves_pairing_and_callback),
		("rotated-member", repeated_private_address_keeps_one_member),
		("unknown-rpa", unknown_rpa_preserves_raw_until_bonded),
		("public", public_rpa_bytes_do_not_resolve),
		("static", static_random_stays_static),
		("pair-failure", rejected_pair_does_not_add_member),
		("rsi", actual_rsi_validation_retained),
		("bounds", controller_and_capacity_bounds_retained),
	];
	let wanted = std::env::args().nth(1);
	let mut ran = 0;
	for (name, case) in cases {
		if wanted.as_deref().is_none_or(|want| want == name) {
			case();
			println!("PASS {name}");
			ran += 1;
		}
	}
	assert!(ran != 0, "unknown case");
	println!("{ran} exact LE Audio identity cases passed; pair/storage boundaries mocked");
}
