// BluetoothService - the host stack above HCI, for both radios.
//
// WHAT RUNS HERE AND WHAT DOES NOT. This program parses a stranger's radio packets, so it is built to
// hold as little as possible while it does: no device claim, no MMIO, no DMA and no link key at rest.
// The controller is reached over a `bluetooth-hci` provider from the catalogue, the keys over a
// private capability to the bond store, and what it produces leaves over typed channels to the
// services that own it. Its Domain is sized by the supervisor and holds zero DMA.
//
// EVERY PROTOCOL DECISION IS IN `service_logic`, held by host tests - the HCI codecs for both radios
// and the credits, L2CAP reassembly and BR/EDR signalling, enhanced retransmission, RFCOMM, SDP, the
// ATT walks, the SMP initiator and the key functions against the specification's own sample data,
// the pairing rules and levels, and the inbound policy. What is here is the IO around them: which
// packet goes where, what waits on what, and what a client is told.
//
// THE BOUNDS ARE `service_logic::bt_bounds`: two controllers, at most eight links on each across both
// radios and seven of them BR/EDR, sixteen L2CAP channels and eight RFCOMM channels a link, and a
// queue of 32 ACL packets a link. The BR/EDR half - inquiry, paging, Secure Simple Pairing's host
// answers, link keys, L2CAP, SDP and RFCOMM - is `classic`; the prompts both radios' pairings raise
// are there too, since every one so far is a BR/EDR one.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{BondLevel, BondRecord, BondedPeer, ControllerInfo, DeviceStatus, EnabledPeer, Error, HciAttachment, HciControlKind, HciPacket, HciPacketKind, InputReport, KeyAgreement, MouseReport, PairingProgress, PairingState, PeerAddress, PeerKind, Profile, PromptReply, ProviderInfo, ProviderKind, Radio, ScanHandle, ScanResult, SecurityLevel, ServiceClass, bluetooth, bluetooth_bond_store, bluetooth_operator, bluetooth_profile, hci_transport, provider_catalogue};
use rt::*;
use service_logic::bt_bounds;
use service_logic::bt_pairing::{Agreement, Level};
use service_logic::gatt_mouse;
use service_logic::gatt_server;
use service_logic::hci::{Credits, Kind, Limits, Session};
use service_logic::hci_codec::{self, Event, opcode};
use service_logic::hogp_map;
use service_logic::l2cap::{Fed, Reassembly};
use service_logic::smp_pairing::Initiator;
use wire::Transport;

include!(concat!(env!("OUT_DIR"), "/roles_bluetooth_service.rs"));

#[path = "bluetooth_service/a2dp.rs"]
mod a2dp;
#[path = "bluetooth_service/audio.rs"]
mod audio;
#[path = "bluetooth_service/broadcast.rs"]
mod broadcast;
#[path = "bluetooth_service/classic.rs"]
mod classic;
#[path = "bluetooth_service/gatt.rs"]
mod gatt;
#[path = "bluetooth_service/input.rs"]
mod input;
#[path = "bluetooth_service/le.rs"]
mod le;
#[path = "bluetooth_service/le_audio.rs"]
mod le_audio;
#[path = "bluetooth_service/opp.rs"]
mod opp;
#[path = "bluetooth_service/pan.rs"]
mod pan;
#[path = "bluetooth_service/serial.rs"]
mod serial;
#[path = "bluetooth_service/voice.rs"]
mod voice;

// ------------------------------------------------------------------ the bounds the milestone states

const MAX_CONTROLLERS: usize = bt_bounds::CONTROLLERS;
// Client connections across all three interfaces.
const MAX_CLIENTS: usize = bt_bounds::CLIENT_CONNECTIONS;
// Commands queued for one controller before a send is refused.
const MAX_QUEUED: usize = 16;
// Open report streams per link.
const MAX_STREAMS: usize = 4;
// A scan: the caller's deadline is capped, and so is what it may find.
const MAX_SCAN_MS: u32 = 10_000;
const MAX_SCAN_RESULTS: usize = 64;
// A pairing attempt, whatever the peer does.
const PAIRING_TICKS: u64 = 60 * TICKS_PER_SECOND;
// The one timer every deadline here is in: a hundred ticks is a second.
const TICKS_PER_SECOND: u64 = 100;
// The version of the HCI transport this host speaks.
const HCI_VERSION: u32 = 1;
// The two fixed LE L2CAP channels the LE half uses.
const ATT_CID: u16 = 0x0004;
const SMP_CID: u16 = 0x0006;
// Why a link is disconnected: remote user terminated, and authentication failure.
const REASON_USER: u8 = 0x13;
const REASON_AUTHENTICATION: u8 = 0x05;
// One `hci-transport.send` frame: its operation, correlation, kind and length ahead of a command of at most
// 258 bytes.
const COMMAND_FRAME: usize = 272;
// One bond-store request. A record is well inside it, and the store reads a request into this much itself.
const BOND_FRAME: usize = 1024;
// The correlation the requests encoded here carry. Each is answered before the next goes out, so one number
// is enough; the reply is still checked against it.
const CORR: u32 = 1;

// The kind byte this service keys a peer by, ahead of its address: the two LE kinds as LE commands carry them,
// and a BR/EDR device address.
const KIND_PUBLIC: u8 = 0;
const KIND_RANDOM: u8 = 1;
const KIND_BREDR: u8 = 3;

// A peer: its kind byte and its address, most significant first - the form every derivation takes.
type Peer = [u8; 7];

// --------------------------------------------------------------------------------- the state

// Which interface a client connection speaks. The ROOT a connection was minted from decides this,
// and nothing the client sends can change it - which is the whole of the three-authority design.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Interface {
	Read,
	Operator,
	Profile,
	Admin,
	Audio,
	Network,
}

struct Client {
	chan: u64,
	interface: Interface,
}

// A scan and what it has found on both radios, deduplicated by address. Inquiry's page scan repetition mode
// and clock offset are kept beside a classic result, so paging what a scan found is fast.
struct Scan {
	id: u32,
	deadline: u64,
	running: bool,
	inquiring: bool,
	results: Vec<ScanResult>,
	paging: Vec<(Peer, u8, u16)>,
}

impl Scan {
	fn active(&self) -> bool {
		self.running || self.inquiring
	}
}

// A pairing attempt's progress, for the operator's `progress`.
struct Attempt {
	peer: Peer,
	deadline: u64,
	state: PairingState,
	security: SecurityLevel,
}

// The HOGP walk under way: the report map's first, and the boot mouse's for a device that has none.
enum Walk {
	Map(hogp_map::Discovery),
	Boot(gatt_mouse::Discovery),
}

// One link to one peer, on either radio.
struct Link {
	handle: u16,
	peer: Peer,
	// This host's own address on the link, as SMP's derivations take it: its private address on LE.
	local: Peer,
	// The GATT server a peer acting as a client reads: GAP and GATT, made on its first request.
	server: Option<gatt_server::Server>,
	// The ATT client applications' grants use: the peer's services, and the operations queued.
	gatt: gatt::Client,
	reassembly: Reassembly,
	encrypted: bool,
	security: SecurityLevel,
	// ACL packets made and waiting for a controller buffer: at most `QUEUED_ACL_PER_LINK`.
	outbox: VecDeque<Vec<u8>>,
	// LE: the pairing, and the HOGP walk - the report map's, or the boot mouse's where the device has no map.
	pairing: Option<Initiator>,
	walk: Option<Walk>,
	// The boot mouse report's handle, when that is what the walk found.
	report: Option<u16>,
	// The report map's input reports, when that is what it found.
	map: Option<hogp_map::Map>,
	// BOTH RADIOS: the decoder an input device's reports go through, and the input streams they go to.
	decoder: Option<input::Decoder>,
	// Buttons last reported by a boot mouse, so a loss can release what the peer was holding.
	buttons: u8,
	streams: Vec<u64>,
	// The key this link was encrypted with came from the bond store rather than a pairing, so a failure to
	// encrypt can be told apart from a pairing failure.
	reconnecting: bool,
	// BR/EDR: everything above ACL.
	classic: Option<Box<classic::ClassicLink>>,
	// LE Audio: the BAP client walking, or driving, the peer's stream endpoints.
	le_audio: Option<le_audio::LeLink>,
}

impl Link {
	fn new(handle: u16, peer: Peer) -> Link {
		Link { handle, peer, local: [0; 7], server: None, gatt: gatt::Client::default(), reassembly: Reassembly::new(), encrypted: false, security: SecurityLevel::None, outbox: VecDeque::new(), pairing: None, walk: None, report: None, map: None, decoder: None, buttons: 0, streams: Vec::new(), reconnecting: false, classic: None, le_audio: None }
	}

	fn is_classic(&self) -> bool {
		self.peer[0] == KIND_BREDR
	}
}

// Where initialisation is: running its steps, waiting for the P-256 key, or done.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Init {
	Running,
	PublicKey,
	Ready,
}

// One initialisation command. A TOLERANT one may be refused - a controller without BR/EDR, or without
// Secure Connections on it, answers some with "unknown command" - and initialisation goes on; any other
// refusal leaves the controller unused. A CLASSIC one is dropped when the controller turns out to have no
// BR/EDR radio.
struct InitStep {
	op: u16,
	params: Vec<u8>,
	tolerant: bool,
	classic: bool,
}

struct Controller {
	info: ProviderInfo,
	transport: u64,
	packets: u64,
	control: u64,
	limits: Limits,
	// Command credits and LE data buffers; and the BR/EDR data buffers, which a controller counts apart.
	credits: Credits,
	bredr: Credits,
	le_bytes: u32,
	bredr_bytes: u32,
	// The largest voice packet the transport carries, its header included: zero for one that carries none.
	sco_bytes: u32,
	// Whether the controller has extended advertising - which makes every LE scan and connection the extended
	// commands' - and the CIS central role; its ISO buffers free, the ISO packets dropped for want of one, and the
	// inbound SDUs being put together.
	extended: bool,
	le_audio_capable: bool,
	iso_free: u32,
	iso_dropped: u64,
	iso_reassembly: service_logic::le_iso::Reassembly,
	// Data packets for handles no table names, dropped.
	unclassified: u64,
	// A controller that reports no LE buffers of its own shares the BR/EDR ones with LE.
	le_shares: bool,
	session: Session,
	address: [u8; 6],
	powered: bool,
	secure_connections: bool,
	// Whether it has a BR/EDR radio, and whether that radio took Secure Connections host support.
	classic: bool,
	classic_sc: bool,
	init: Init,
	steps: VecDeque<InitStep>,
	waiting_step: Option<(u16, bool)>,
	pending: Vec<(u16, Vec<u8>)>,
	outstanding: Option<u16>,
	public_key: Option<[u8; 64]>,
	scan: Option<Scan>,
	attempt: Option<Attempt>,
	links: Vec<Link>,
	// A bonded, enabled LE peer this controller is connecting to without pairing.
	reconnect: Option<Peer>,
	// The BR/EDR side: what it pages, its inbound policy and its prompts.
	bredr_state: classic::ControllerState,
	// The LE side: this host's privacy, and the bonded peripherals' reconnection.
	le: le::LeState,
}

struct Stack {
	controllers: Vec<Controller>,
	bonds: u64,
	next_scan: u32,
	// Applications' GATT grants and serial ports, each for one launch.
	grants: Vec<gatt::Grant>,
	serials: Vec<serial::Serial>,
	// Object pushes under way, receivers waiting for an object, and a receive the operator view validated for the
	// stream `serve_receive` makes.
	pushes: Vec<opp::Pusher>,
	receivers: Vec<opp::Receiver>,
	receive_args: Option<(usize, Peer, u64)>,
	// AudioService's endpoints, and the call it relays.
	audio: audio::AudioRoot,
	// NetworkService's PAN links.
	network: pan::NetworkRoot,
	// LE Audio: the unicast devices, and the broadcast sink.
	le_devices: Vec<le_audio::LeDevice>,
	broadcast: broadcast::Sink,
}

// ------------------------------------------------------------------ address forms

// AN ADVERTISER'S ADDRESS KIND, by its type and its top bits: a static or a resolvable random address, or a public one;
// a non-resolvable private address names nobody this host could find again.
fn advertised_kind(kind: u8, address: &[u8; 6]) -> Option<PeerKind> {
	match kind {
		0 => Some(PeerKind::Public),
		1 if address[0] & 0xc0 == 0xc0 => Some(PeerKind::RandomStatic),
		1 if address[0] & 0xc0 == 0x40 => Some(PeerKind::Resolvable),
		_ => None,
	}
}

fn peer_to_wire(peer: &Peer) -> PeerAddress {
	let kind = match peer[0] {
		KIND_PUBLIC => PeerKind::Public,
		KIND_BREDR => PeerKind::Bredr,
		// THE TOP TWO BITS of a random address say which kind it is: `11` static, `01` resolvable.
		_ if peer[1] >> 6 == 0b01 => PeerKind::Resolvable,
		_ => PeerKind::RandomStatic,
	};
	PeerAddress { kind, bytes: peer[1..].to_vec() }
}

fn peer_from_wire(address: &PeerAddress) -> Option<Peer> {
	if address.bytes.len() != 6 {
		return None;
	}
	let mut out = [0u8; 7];
	out[0] = match address.kind {
		PeerKind::Public => KIND_PUBLIC,
		PeerKind::RandomStatic | PeerKind::Resolvable => KIND_RANDOM,
		PeerKind::Bredr => KIND_BREDR,
	};
	out[1..].copy_from_slice(&address.bytes);
	Some(out)
}

fn radio_of(peer: &Peer) -> Radio {
	if peer[0] == KIND_BREDR { Radio::Classic } else { Radio::Le }
}

fn local_address(controller: &Controller) -> Peer {
	let mut out = [0u8; 7];
	out[1..].copy_from_slice(&controller.address);
	out
}

fn local_wire(controller: &Controller) -> PeerAddress {
	peer_to_wire(&local_address(controller))
}

// The level's two axes, in the logic's terms and the wire's.
fn level_to_wire(level: &Level) -> BondLevel {
	let agreement = match level.agreement {
		Agreement::Legacy => KeyAgreement::Legacy,
		Agreement::LeLegacy => KeyAgreement::LeLegacy,
		Agreement::P192 => KeyAgreement::P192,
		Agreement::SecureConnections => KeyAgreement::SecureConnections,
	};
	BondLevel { agreement, authenticated: level.authenticated }
}

fn level_from_wire(level: &BondLevel) -> Level {
	let agreement = match level.agreement {
		KeyAgreement::Legacy => Agreement::Legacy,
		KeyAgreement::LeLegacy => Agreement::LeLegacy,
		KeyAgreement::P192 => Agreement::P192,
		KeyAgreement::SecureConnections => Agreement::SecureConnections,
	};
	Level { agreement, authenticated: level.authenticated }
}

fn security_of(level: &Level) -> SecurityLevel {
	if level.authenticated { SecurityLevel::EncryptedAuthenticated } else { SecurityLevel::EncryptedUnauthenticated }
}

fn profile_to_logic(profile: Profile) -> service_logic::bt_policy::Profile {
	use service_logic::bt_policy::Profile as P;
	match profile {
		Profile::Input => P::Input,
		Profile::Audio => P::Audio,
		Profile::Voice => P::Voice,
		Profile::Pan => P::Pan,
		Profile::Spp => P::Spp,
		Profile::Gatt => P::Gatt,
	}
}

fn trust_of(trusted: &[Profile]) -> service_logic::bt_policy::Trust {
	trusted.iter().fold(service_logic::bt_policy::Trust::default(), |trust, profile| trust.with(profile_to_logic(*profile), true))
}

// Zero a buffer that held key material, with writes the compiler may not drop as dead because the buffer
// is about to be freed.
fn scrub(bytes: &mut [u8]) {
	for byte in bytes.iter_mut() {
		// SAFETY: a valid, aligned byte of this slice.
		unsafe { core::ptr::write_volatile(byte, 0) };
	}
}

// Zero both keys a record may carry.
fn scrub_record(record: &mut BondRecord) {
	scrub(&mut record.key);
	scrub(&mut record.link_key);
	scrub(&mut record.irk);
}

// A request encoded here, sent and answered over one channel. The reply comes back in a vector this service
// owns, for the caller to zero once it has read it; no key rides in a capability, so any that arrive are closed.
fn exchange(chan: u64, request: &[u8]) -> Option<Vec<u8>> {
	let mut handles = wire::Handles::new();
	let reply = ChannelTransport { chan }.call(request, &[], &mut handles, 0).ok();
	for &handle in handles.as_slice() {
		close(handle);
	}
	reply
}

// A `result` reply to one of those requests: `None` for one that is not an answer to it, `Some(Err)` for a
// refusal, and otherwise what `read` takes from after the tag.
fn answered<T>(reply: &[u8], read: impl FnOnce(&mut wire::Reader) -> Option<T>) -> Option<Result<T, Error>> {
	let mut reader = wire::Reader::new(reply);
	if reader.u32()? != CORR {
		return None;
	}
	let value = if reader.tag()? { Ok(read(&mut reader)?) } else { Err(Error::read(&mut reader)?) };
	reader.finish()?;
	Some(value)
}

// `hci-transport.send` of one packet, FROM A FRAME THIS SERVICE OWNS AND ZEROES. An encryption's parameters
// carry the LTK, a link key reply carries the link key, and the generated client would copy the packet into
// a vector of its own and free that - and every smaller one it outgrew - with the key still in it. The reply
// carries a count and nothing secret.
fn send_packet(chan: u64, kind: HciPacketKind, packet: &[u8]) -> bool {
	let mut frame = alloc::vec![0u8; packet.len() + 16];
	let len = (|| {
		let length = u16::try_from(packet.len()).ok()?;
		frame[..2].copy_from_slice(&hci_transport::OP_SEND.to_le_bytes());
		frame[2..6].copy_from_slice(&CORR.to_le_bytes());
		let at = 6 + kind.encode(&mut frame[6..])?;
		frame.get_mut(at..at + 2)?.copy_from_slice(&length.to_le_bytes());
		frame.get_mut(at + 2..at + 2 + packet.len())?.copy_from_slice(packet);
		Some(at + 2 + packet.len())
	})();
	let reply = len.and_then(|len| exchange(chan, &frame[..len]));
	scrub(&mut frame);
	reply.and_then(|reply| answered(&reply, |reader| reader.u32())).is_some_and(|result| result.is_ok())
}

fn send_command(chan: u64, command: &[u8]) -> bool {
	if command.len() + 16 > COMMAND_FRAME {
		return false;
	}
	send_packet(chan, HciPacketKind::Command, command)
}

// ------------------------------------------------------------------ the controller

impl Controller {
	// Queue one command. REFUSED RATHER THAN GROWN past the bound: a host that queued without one
	// would let a stuck controller take this service's memory.
	fn command(&mut self, op: u16, params: &[u8]) -> bool {
		if self.pending.len() >= MAX_QUEUED {
			print(b"BluetoothService: the command queue is full; a command is refused\n");
			return false;
		}
		// THE CONTROLLER'S OWN MAXIMUM, checked before the command is queued rather than learnt from a
		// transport that refuses it later.
		if service_logic::hci::check_outbound(&self.limits, Kind::Command as u16, (3 + params.len()) as u32).is_err() {
			print(b"BluetoothService: a command is longer than the controller accepts and is refused\n");
			return false;
		}
		self.pending.push((op, params.to_vec()));
		true
	}

	// THE EXTENDED-MODE RULE: a controller with extended advertising refuses the legacy scan and connection commands once
	// any extended one was sent - and the broadcast sink sends them - so on such a controller every scan and every
	// connection is the extended command, carrying the same parameters.
	fn le_scan_parameters(&mut self) -> bool {
		let legacy = hci_codec::scan_parameters_from(self.le.own_type());
		if self.extended {
			return self.command(service_logic::le_iso::opcode::LE_SET_EXTENDED_SCAN_PARAMETERS, &service_logic::le_iso::extended_scan_parameters_from(&legacy));
		}
		self.command(opcode::LE_SET_SCAN_PARAMETERS, &legacy)
	}

	fn le_scan_enable(&mut self, on: bool) -> bool {
		if self.extended {
			return self.command(service_logic::le_iso::opcode::LE_SET_EXTENDED_SCAN_ENABLE, &service_logic::le_iso::extended_scan_enable(on));
		}
		self.command(opcode::LE_SET_SCAN_ENABLE, &hci_codec::scan_enable(on))
	}

	fn le_create_connection(&mut self, legacy: &[u8; 25]) -> bool {
		if self.extended {
			return self.command(service_logic::le_iso::opcode::LE_EXTENDED_CREATE_CONNECTION, &service_logic::le_iso::extended_create_connection(legacy));
		}
		self.command(opcode::LE_CREATE_CONNECTION, legacy)
	}

	// Queue `LE Enable Encryption` for a link. The parameters carry the LTK, so the copy built here is
	// zeroed once the queue holds its own - and the queue's is zeroed when it is sent or dropped.
	fn encrypt(&mut self, handle: u16, ltk: &[u8; 16]) {
		let mut params = hci_codec::enable_encryption(handle, ltk);
		self.command(opcode::LE_ENABLE_ENCRYPTION, &params);
		scrub(&mut params);
	}

	// `LE Enable Encryption` with an LE legacy key and the EDIV and Rand that name it.
	fn encrypt_legacy(&mut self, handle: u16, ltk: &[u8; 16], rand: &[u8; 8], ediv: u16) {
		let mut params = hci_codec::enable_encryption_legacy(handle, ltk, rand, ediv);
		self.command(opcode::LE_ENABLE_ENCRYPTION, &params);
		scrub(&mut params);
	}

	fn disconnect(&mut self, handle: u16, reason: u8) {
		self.command(opcode::DISCONNECT, &hci_codec::disconnect(handle, reason));
	}

	// Drop every queued command. One of them may carry a key, so each is zeroed first rather than freed as
	// it is.
	fn clear_pending(&mut self) {
		for (_, params) in self.pending.iter_mut() {
			scrub(params);
		}
		self.pending.clear();
	}

	// Send the next command if the controller will take one. AT MOST ONE IS OUTSTANDING, and the
	// credit comes back on the controller's own completion - never on the transport's acceptance,
	// which is the defect `hci::Credits` exists to rule out. Then whatever link data the buffers allow.
	fn pump(&mut self) {
		self.flush_links();
		if self.outstanding.is_some() || self.pending.is_empty() {
			return;
		}
		if self.credits.take(Kind::Command).is_err() {
			return;
		}
		let (op, mut params) = self.pending.remove(0);
		let Some(mut bytes) = hci_codec::command(op, &params) else {
			scrub(&mut params);
			self.credits.commands_completed(1);
			self.credits.drained();
			return;
		};
		let sent = send_command(self.transport, &bytes);
		// The command has gone, or failed to; either way neither copy is read again, and a key-carrying one's
		// parameters are zeroed here.
		scrub(&mut params);
		scrub(&mut bytes);
		self.credits.drained();
		match sent {
			true => self.outstanding = Some(op),
			false => {
				// A SEND THE TRANSPORT REFUSED IS NOT OUTSTANDING, and the credit it took is given
				// back: the controller never saw it and will never answer it.
				self.credits.commands_completed(1);
				print(b"BluetoothService: the transport refused a command\n");
			}
		}
	}

	// The data buffers a link's packets take, and their size.
	fn pool(&mut self, classic: bool) -> (&mut Credits, u32) {
		if classic || self.le_shares { (&mut self.bredr, self.bredr_bytes) } else { (&mut self.credits, self.le_bytes) }
	}

	// L2CAP data on a link: one PDU, FRAGMENTED to the controller's buffer size and queued on the link, sent
	// as buffers free. False when the link's queue cannot take the whole PDU - which drops it whole rather
	// than sending a start whose continuations will never follow.
	fn l2cap(&mut self, handle: u16, cid: u16, payload: &[u8]) -> bool {
		let mut pdu = Vec::with_capacity(4 + payload.len());
		pdu.extend_from_slice(&(payload.len() as u16).to_le_bytes());
		pdu.extend_from_slice(&cid.to_le_bytes());
		pdu.extend_from_slice(payload);
		self.l2cap_pdu(handle, pdu)
	}

	// A PDU whose basic header is already on it - an enhanced retransmission frame - queued the same way.
	fn l2cap_pdu(&mut self, handle: u16, pdu: Vec<u8>) -> bool {
		let Some(at) = self.links.iter().position(|link| link.handle == handle) else { return false };
		let classic = self.links[at].is_classic();
		let (_, size) = self.pool(classic);
		let size = (size as usize).clamp(27, bt_bounds::ACL_PACKET - 4);
		let fragments = pdu.len().div_ceil(size);
		let link = &mut self.links[at];
		if link.outbox.len() + fragments > bt_bounds::QUEUED_ACL_PER_LINK {
			print(b"BluetoothService: a link's data queue is full; a packet is dropped\n");
			return false;
		}
		for (index, chunk) in pdu.chunks(size).enumerate() {
			// THE FIRST FRAGMENT IS A START (`00`, not automatically flushable) and every other a continuation.
			link.outbox.push_back(hci_codec::acl(handle, if index == 0 { 0b00 } else { 0b01 }, chunk));
		}
		self.flush_links();
		true
	}

	// Send queued link data while the buffers last, a link at a time from the front of its queue.
	fn flush_links(&mut self) {
		for at in 0..self.links.len() {
			loop {
				if self.links[at].outbox.is_empty() {
					break;
				}
				let classic = self.links[at].is_classic();
				let transport = self.transport;
				let (pool, _) = self.pool(classic);
				if pool.take(Kind::Acl).is_err() {
					break;
				}
				let packet = self.links[at].outbox.pop_front().unwrap_or_default();
				let sent = send_packet(transport, HciPacketKind::Acl, &packet);
				let (pool, _) = self.pool(classic);
				pool.drained();
				if !sent {
					pool.acl_completed(1);
					print(b"BluetoothService: the transport refused link data\n");
					break;
				}
			}
		}
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

	fn link_to_mut(&mut self, peer: &Peer) -> Option<&mut Link> {
		self.links.iter_mut().find(|link| &link.peer == peer)
	}

	fn le_links(&self) -> usize {
		self.links.iter().filter(|link| !link.is_classic()).count()
	}

	// Begin (or begin again) the initialisation sequence. THE STEPS ARE A QUEUE, and the BR/EDR ones leave
	// it when the controller says it has no BR/EDR radio.
	fn start_init(&mut self) {
		self.init = Init::Running;
		self.powered = false;
		self.public_key = None;
		// WHAT THE CONTROLLER IS, learnt again: a controller that answers no features this time has none.
		self.extended = false;
		self.le_audio_capable = false;
		self.iso_free = 0;
		self.clear_pending();
		self.outstanding = None;
		self.credits.reset();
		self.bredr.reset();
		self.steps.clear();
		let step = |op: u16, params: &[u8], tolerant: bool, classic: bool| InitStep { op, params: params.to_vec(), tolerant, classic };
		let steps = [
			step(opcode::READ_BD_ADDR, &[], false, false),
			step(opcode::READ_LOCAL_SUPPORTED_COMMANDS, &[], false, false),
			step(service_logic::hci_bredr::opcode::READ_LOCAL_SUPPORTED_FEATURES, &[], true, false),
			step(service_logic::hci_bredr::opcode::READ_BUFFER_SIZE, &[], true, true),
			step(opcode::LE_READ_BUFFER_SIZE, &[], false, false),
			// THE LE FEATURES: extended advertising decides the scan's and the connection's commands, the CIS central
			// role whether LE Audio unicast runs.
			step(service_logic::le_iso::opcode::LE_READ_LOCAL_SUPPORTED_FEATURES, &[], true, false),
			// The event mask is chosen when this step is sent, by whether the controller has BR/EDR.
			step(opcode::SET_EVENT_MASK, &[], false, false),
			// Connection complete, advertising report, and the two key-agreement completions; and LE Audio's own.
			step(opcode::LE_SET_EVENT_MASK, &service_logic::le_iso::LE_EVENT_MASK.to_le_bytes(), false, false),
		];
		self.steps.extend(steps);
		// A TRANSPORT THAT CARRIES ISO: the controller's ISO buffers, and isochronous channels on for this host.
		if self.limits.carries(service_logic::hci::Kind::Iso) {
			self.steps.push_back(step(service_logic::le_iso::opcode::LE_READ_BUFFER_SIZE_V2, &[], true, false));
			self.steps.push_back(step(service_logic::le_iso::opcode::LE_SET_HOST_FEATURE, &service_logic::le_iso::set_host_feature(service_logic::le_iso::ISOCHRONOUS_CHANNELS_HOST_SUPPORT, true), true, false));
		}
		self.steps.extend(classic::init_steps());
		self.waiting_step = Some((opcode::RESET, false));
		self.command(opcode::RESET, &[]);
	}

	// The next initialisation step, after the previous one completed.
	fn advance_init(&mut self) {
		while let Some(mut step) = self.steps.pop_front() {
			if step.classic && !self.classic {
				continue;
			}
			if step.op == opcode::SET_EVENT_MASK {
				// EVERY EVENT A HOST OF BOTH RADIOS READS - the BR/EDR pairing events up to keypress
				// notification - and LE meta; without BR/EDR the original mask.
				let mask: u64 = if self.classic { 0x3fff_ffff_ffff_ffff } else { 0x2000_1fff_ffff_ffff };
				step.params = mask.to_le_bytes().to_vec();
			}
			self.waiting_step = Some((step.op, step.tolerant));
			self.command(step.op, &step.params);
			return;
		}
		self.waiting_step = None;
		// A CONTROLLER WITHOUT THE P-256 COMMANDS IS READY AND CANNOT PAIR ON LE, and it says so in
		// `controllers` rather than failing halfway through a pairing later.
		if self.secure_connections && self.init != Init::PublicKey {
			self.init = Init::PublicKey;
			self.command(opcode::LE_READ_LOCAL_P256_PUBLIC_KEY, &[]);
			return;
		}
		self.powered = true;
		self.init = Init::Ready;
	}

	// End whatever this controller's session held: links, scans, attempts and report streams. A
	// reset or a fault does this, and so does powering the radio off. THE SESSION'S SCAN GOES WITH IT:
	// its handle answers `not-found`, and nothing it found can be chosen for a pairing in the next one.
	fn end_session(&mut self) {
		self.iso_free = 0;
		self.iso_reassembly = service_logic::le_iso::Reassembly::default();
		for mut link in self.links.drain(..) {
			release_streams(&mut link);
			link.pairing = None;
		}
		self.scan = None;
		if let Some(attempt) = self.attempt.as_mut()
			&& matches!(attempt.state, PairingState::Connecting | PairingState::Pairing)
		{
			attempt.state = PairingState::Failed;
		}
		self.bredr_state.end_session();
		self.le.end_session();
		self.clear_pending();
		self.outstanding = None;
		self.credits.reset();
		self.bredr.reset();
	}

	fn fail_attempt(&mut self, peer: &Peer) {
		if let Some(attempt) = self.attempt.as_mut()
			&& &attempt.peer == peer
			&& matches!(attempt.state, PairingState::Connecting | PairingState::Pairing)
		{
			attempt.state = PairingState::Failed;
		}
	}
}

// A lost link releases what its peer was holding and ends every input stream. What is held goes up
// first - every key, every button, every gamepad - so a consumer that stops hearing from this peer is
// not left with anything held down for ever.
fn release_streams(link: &mut Link) {
	let mut releases = link.decoder.as_mut().map(input::Decoder::releases).unwrap_or_default();
	if link.buttons != 0 {
		releases.push(InputReport::Pointer(MouseReport { dx: 0, dy: 0, wheel: 0, buttons: 0 }));
		link.buttons = 0;
	}
	for report in &releases {
		for &stream in &link.streams {
			write_input(stream, report);
		}
	}
	for stream in link.streams.drain(..) {
		close(stream);
	}
}

// One report onto a stream. A stream whose consumer stopped draining is not waited for: the report
// is dropped for that consumer, which is the one behaviour that keeps a slow reader from stopping
// the radio.
fn write_input(stream: u64, report: &InputReport) -> bool {
	let mut frame = [0u8; 256];
	let mut handles = wire::Handles::new();
	let Some(len) = bluetooth_profile::open_input_frame(0, report, &mut frame, &mut handles) else { return false };
	!matches!(try_send_outcome(stream, &frame[..len], 0), SendOutcome::Failed)
}

// Reports onto every stream a link has, dropping a stream whose consumer is gone.
fn deliver(link: &mut Link, reports: &[InputReport]) {
	if reports.is_empty() {
		return;
	}
	link.streams.retain(|&stream| {
		let kept = reports.iter().all(|report| write_input(stream, report));
		if !kept {
			close(stream);
		}
		kept
	});
}

// ------------------------------------------------------------------ the bond store

impl Stack {
	fn bonds(&self) -> bluetooth_bond_store::Client<ChannelTransport> {
		bluetooth_bond_store::Client::new(ChannelTransport { chan: self.bonds })
	}

	// The stored record for this controller and peer, KEYS INCLUDED - so only the callers that need a key or
	// rewrite the record ask for it, and each zeroes it once it has. THE REPLY CARRYING IT is read from a
	// vector this service owns and zeroed once read; the generated client would free its own with the key in
	// it. The request names two addresses and nothing secret.
	fn bond(&self, at: usize, peer: &Peer) -> Option<BondRecord> {
		if self.bonds == 0 {
			return None;
		}
		let mut request = [0u8; 64];
		request[..2].copy_from_slice(&bluetooth_bond_store::OP_LOOKUP.to_le_bytes());
		request[2..6].copy_from_slice(&CORR.to_le_bytes());
		let local = local_wire(&self.controllers[at]).encode(&mut request[6..])?;
		let peer = peer_to_wire(peer).encode(&mut request[6 + local..])?;
		let mut reply = exchange(self.bonds, &request[..6 + local + peer])?;
		let answer = answered(&reply, BondRecord::read);
		scrub(&mut reply);
		match answer {
			Some(Ok(record)) if record.key.len() == 16 || record.link_key.len() == 16 => Some(record),
			Some(Ok(mut record)) => {
				scrub_record(&mut record);
				None
			}
			_ => None,
		}
	}

	// Store or replace a record, answered only after the store's durable commit. THE REQUEST CARRIES THE KEY,
	// so it is encoded into a buffer this service owns and zeroed after the send, not into the generated
	// client's own vector.
	fn store_bond(&self, record: &BondRecord) -> bool {
		if self.bonds == 0 {
			return false;
		}
		let mut request = [0u8; BOND_FRAME];
		request[..2].copy_from_slice(&bluetooth_bond_store::OP_STORE.to_le_bytes());
		request[2..6].copy_from_slice(&CORR.to_le_bytes());
		let len = record.encode(&mut request[6..]).map(|body| 6 + body);
		let reply = len.and_then(|len| exchange(self.bonds, &request[..len]));
		scrub(&mut request);
		reply.and_then(|reply| answered(&reply, |_| Some(()))).is_some_and(|result| result.is_ok())
	}

	// Every bond on this controller, WITHOUT KEYS - the store's listing carries none - and without the record that names
	// this host itself, which holds its identity resolving key and is no bond.
	fn records(&self, at: usize) -> Vec<BondRecord> {
		if self.bonds == 0 {
			return Vec::new();
		}
		let local = local_wire(&self.controllers[at]);
		match self.bonds().list(&local) {
			Some(Ok(records)) => records.into_iter().filter(|record| record.peer != local).collect(),
			_ => Vec::new(),
		}
	}

	// One peer's record without its keys.
	fn record(&self, at: usize, peer: &Peer) -> Option<BondRecord> {
		let wire = peer_to_wire(peer);
		self.records(at).into_iter().find(|record| record.peer == wire)
	}

	// Rewrite one record through `change`, keys and all, durably. False when there is no such bond or the
	// store could not commit.
	fn rewrite_bond(&self, at: usize, peer: &Peer, change: impl FnOnce(&mut BondRecord)) -> Result<(), Error> {
		let Some(mut record) = self.bond(at, peer) else { return Err(Error::NotFound) };
		change(&mut record);
		let stored = self.store_bond(&record);
		scrub_record(&mut record);
		if stored { Ok(()) } else { Err(Error::Io) }
	}

	// Whether this peer is bonded on this controller and an operator made it an input source - read from
	// the store's listing, which carries no key.
	fn enabled_peer(&self, at: usize, peer: &Peer) -> Option<bool> {
		self.record(at, peer).map(|record| record.enabled)
	}
}

// ------------------------------------------------------------------ events from the controller

impl Stack {
	fn on_event(&mut self, at: usize, bytes: &[u8]) {
		let event = match hci_codec::event(bytes) {
			Ok(event) => event,
			// AN EVENT THE LE CODEC DOES NOT READ goes to the BR/EDR one, and one neither reads is ignored. A
			// malformed one is said, because a controller sending events whose lengths disagree with their
			// bytes is a controller whose every later event deserves suspicion.
			Err(hci_codec::Refusal::Unhandled { .. }) => {
				// LE AUDIO'S SUBEVENTS - extended reports, periodic sync, CISes, BIGs.
				if bytes.first() == Some(&hci_codec::event::LE_META)
					&& let Some(event) = bytes.get(2).and_then(|&subevent| service_logic::le_iso::event(subevent, &bytes[3..]))
				{
					self.on_le_audio_event(at, event);
					return;
				}
				if let Some(event) = service_logic::hci_bredr::decode(bytes) {
					self.on_classic_event(at, event);
				}
				return;
			}
			Err(_) => {
				print(b"BluetoothService: a malformed event from the controller was refused\n");
				return;
			}
		};
		match event {
			Event::CommandComplete { opcode: op, params, .. } => self.on_complete(at, op, params),
			Event::CommandStatus { status, opcode: op, .. } => self.on_status(at, status, op),
			Event::LocalPublicKey { status, key } => {
				let controller = &mut self.controllers[at];
				if status == 0 && key.len() == 64 {
					let mut public = [0u8; 64];
					public.copy_from_slice(key);
					controller.public_key = Some(public);
				} else {
					controller.secure_connections = false;
				}
				if controller.init == Init::PublicKey {
					controller.advance_init();
					self.after_ready(at);
				}
			}
			Event::DhKey { status, key } => {
				let Some(handle) = self.controllers[at].links.iter().find(|link| link.pairing.is_some()).map(|link| link.handle) else { return };
				let steps = {
					let Some(pairing) = self.controllers[at].link_mut(handle).and_then(|link| link.pairing.as_mut()) else { return };
					if status == 0 && key.len() == 32 {
						let mut wire = [0u8; 32];
						wire.copy_from_slice(key);
						let steps = pairing.on_dhkey(&wire);
						scrub(&mut wire);
						steps
					} else {
						pairing.on_dhkey_failed()
					}
				};
				self.run_smp(at, handle, steps);
			}
			Event::Connected { status, handle, peer_kind, peer } => self.on_connected(at, status, handle, peer_kind, peer),
			Event::Disconnected { handle, .. } => self.on_disconnected(at, handle),
			Event::EncryptionChange { status, handle, enabled } => {
				if self.controllers[at].link(handle).is_some_and(Link::is_classic) {
					self.classic_encryption(at, status, handle, enabled);
				} else {
					self.on_encryption(at, status, handle, enabled);
				}
			}
			Event::CompletedPackets { handle, count, rest } => {
				let mut entries = Vec::with_capacity(1 + rest.len() / 4);
				entries.push((handle, count));
				for entry in rest.chunks_exact(4) {
					entries.push((u16::from_le_bytes([entry[0], entry[1]]) & 0x0fff, u16::from_le_bytes([entry[2], entry[3]])));
				}
				// EACH HANDLE'S COUNT GOES BACK TO ITS OWN RADIO'S BUFFERS; a handle no longer held returns to the
				// pool its radio would use, which is LE's unless it shares.
				for (handle, count) in entries {
					// AN ISOCHRONOUS STREAM'S BUFFERS are their own.
					if self.iso_handle(at, handle) {
						let controller = &mut self.controllers[at];
						controller.iso_free = controller.iso_free.saturating_add(u32::from(count));
						continue;
					}
					let controller = &mut self.controllers[at];
					let classic = controller.link(handle).is_some_and(Link::is_classic);
					let (pool, _) = controller.pool(classic);
					pool.acl_completed(count as u32);
				}
				self.controllers[at].flush_links();
			}
			Event::Advertising { count, reports } => self.on_advertising(at, count, reports),
		}
	}

	fn on_complete(&mut self, at: usize, op: u16, params: &[u8]) {
		let controller = &mut self.controllers[at];
		if controller.outstanding == Some(op) {
			controller.outstanding = None;
			controller.credits.commands_completed(1);
		}
		let status = params.first().copied().unwrap_or(0xff);
		if controller.init != Init::Ready {
			let Some((waiting, tolerant)) = controller.waiting_step else { return };
			if waiting != op {
				return;
			}
			if status != 0 && !tolerant {
				// NOT USED, AND NOT STUCK: left as a controller that is ready and off, so an operator's
				// power-on runs initialisation again from the reset rather than answering for nothing.
				print(b"BluetoothService: the controller refused an initialisation command; it is not used\n");
				controller.powered = false;
				controller.init = Init::Ready;
				controller.waiting_step = None;
				return;
			}
			if status == 0 {
				match op {
					opcode::READ_BD_ADDR if params.len() >= 7 => {
						let mut wire = [0u8; 6];
						wire.copy_from_slice(&params[1..7]);
						controller.address = hci_codec::address_from_wire(&wire);
					}
					opcode::READ_LOCAL_SUPPORTED_COMMANDS => {
						controller.secure_connections = params.get(1 + hci_codec::P256_OCTET).is_some_and(|octet| octet & hci_codec::P256_BITS == hci_codec::P256_BITS);
					}
					service_logic::le_iso::opcode::LE_READ_LOCAL_SUPPORTED_FEATURES if params.len() >= 9 => {
						let mut bytes = [0u8; 8];
						bytes.copy_from_slice(&params[1..9]);
						let features = u64::from_le_bytes(bytes);
						controller.extended = features & service_logic::le_iso::feature::EXTENDED_ADVERTISING != 0;
						controller.le_audio_capable = features & service_logic::le_iso::feature::CIS_CENTRAL != 0;
					}
					service_logic::le_iso::opcode::LE_READ_BUFFER_SIZE_V2 => {
						if let Some((_, (_, count))) = service_logic::le_iso::buffer_sizes_v2(params) {
							controller.iso_free = u32::from(count);
						}
					}
					opcode::LE_READ_BUFFER_SIZE if params.len() >= 4 => {
						// THE CONTROLLER'S OWN BUFFER COUNT, bounded by the transport's queue: a controller that
						// reports zero shares its BR/EDR buffers with LE.
						let size = u16::from_le_bytes([params[1], params[2]]) as u32;
						let buffers = params[3] as u32;
						if buffers > 0 {
							controller.credits = Credits::new(1, buffers, MAX_QUEUED as u32);
							controller.le_bytes = size.max(27);
						} else {
							controller.le_shares = controller.classic;
						}
					}
					_ => classic::on_init_complete(controller, op, params),
				}
			} else if op == service_logic::hci_bredr::opcode::READ_LOCAL_SUPPORTED_FEATURES {
				controller.classic = false;
			} else if op == service_logic::hci_bredr::opcode::WRITE_SECURE_CONNECTIONS_HOST_SUPPORT {
				controller.classic_sc = false;
			}
			controller.advance_init();
			if controller.init == Init::Ready {
				self.after_ready(at);
			}
			return;
		}
		if (op == opcode::LE_SET_SCAN_ENABLE || op == service_logic::le_iso::opcode::LE_SET_EXTENDED_SCAN_ENABLE) && status != 0 {
			if let Some(scan) = controller.scan.as_mut() {
				scan.running = false;
			}
		}
		if op == service_logic::le_iso::opcode::LE_SET_CIG_PARAMETERS {
			self.le_audio_cig(at, params);
		}
		self.classic_complete(at, op, status, params);
	}

	fn on_status(&mut self, at: usize, status: u8, op: u16) {
		let controller = &mut self.controllers[at];
		if controller.outstanding == Some(op) {
			controller.outstanding = None;
			controller.credits.commands_completed(1);
		}
		if status == 0 {
			return;
		}
		match op {
			opcode::LE_READ_LOCAL_P256_PUBLIC_KEY => {
				controller.secure_connections = false;
				if controller.init == Init::PublicKey {
					controller.advance_init();
					self.after_ready(at);
				}
			}
			opcode::LE_CREATE_CONNECTION | service_logic::le_iso::opcode::LE_EXTENDED_CREATE_CONNECTION => {
				controller.le.accepting = false;
				if let Some(peer) = controller.reconnect.take() {
					controller.fail_attempt(&peer);
				}
				if let Some(attempt) = controller.attempt.as_mut()
					&& attempt.peer[0] != KIND_BREDR
					&& attempt.state == PairingState::Connecting
				{
					attempt.state = PairingState::Failed;
				}
			}
			opcode::LE_GENERATE_DHKEY => {
				let Some(handle) = controller.links.iter().find(|link| link.pairing.is_some()).map(|link| link.handle) else { return };
				let steps = match controller.link_mut(handle).and_then(|link| link.pairing.as_mut()) {
					Some(pairing) => pairing.on_dhkey_failed(),
					None => return,
				};
				self.run_smp(at, handle, steps);
			}
			opcode::LE_ENABLE_ENCRYPTION => {
				if let Some(handle) = controller.links.iter().find(|link| !link.is_classic() && !link.encrypted).map(|link| link.handle) {
					self.encryption_failed(at, handle);
				}
			}
			_ => self.classic_status(at, status, op),
		}
	}

	// A controller that finished initialising connects to the LE peer an operator enabled, if there is one -
	// which is what makes a bond survive a restart and a reboot without pairing again - and takes the BR/EDR
	// inbound policy it should have now.
	fn after_ready(&mut self, at: usize) {
		if !self.controllers[at].powered {
			return;
		}
		self.refresh_policy(at);
		self.le_ready(at);
	}

	fn on_connected(&mut self, at: usize, status: u8, handle: u16, peer_kind: u8, address: [u8; 6]) {
		// A RESOLVED IDENTITY - types 2 and 3 - is the bonded peer's identity address, public or static random.
		let mut peer = [0u8; 7];
		peer[0] = peer_kind & 0x01;
		peer[1..].copy_from_slice(&address);
		let controller = &mut self.controllers[at];
		let through_list = core::mem::take(&mut controller.le.accepting);
		let relist = through_list && core::mem::take(&mut controller.le.relist);
		if status != 0 {
			controller.fail_attempt(&peer);
			controller.reconnect = None;
			// A STANDING ATTEMPT CANCELLED FOR A NEW LIST is taken up again with it now.
			if relist {
				self.reconnect_bonded(at);
			}
			return;
		}
		// THE LINK BOUND: eight links across both radios. A connection past it is refused rather than held.
		if controller.links.len() >= bt_bounds::LINKS_PER_CONTROLLER || controller.link_to(&peer).is_some() {
			controller.disconnect(handle, REASON_USER);
			return;
		}
		let pairing_peer = controller.attempt.as_ref().is_some_and(|attempt| attempt.peer == peer && attempt.state == PairingState::Connecting);
		// A DEVICE THE ACCEPT LIST LET IN is a bonded one: the list holds nothing else.
		let reconnecting = controller.reconnect == Some(peer) || (through_list && !pairing_peer);
		controller.reconnect = None;
		// A connection this host did not ask for is a peer it does not know, and LE never advertises here: it
		// is disconnected rather than held.
		if !pairing_peer && !reconnecting {
			controller.disconnect(handle, REASON_USER);
			return;
		}
		let mut link = Link::new(handle, peer);
		link.reconnecting = reconnecting;
		link.local = controller.le.local(local_address(controller));
		controller.links.push(link);
		if pairing_peer {
			self.start_le_pairing(at, handle);
		} else {
			// RECONNECT: the key comes from the store, and encryption is PROVED before input is
			// enabled - a link that will not encrypt with the stored key is a peer that is not the one
			// that bonded, or one that forgot the bond, and either way it is not an input source.
			match self.bond(at, &peer) {
				Some(mut record) if record.key.len() == 16 => {
					let mut ltk = [0u8; 16];
					ltk.copy_from_slice(&record.key);
					let legacy = (record.rand.len() == 8).then(|| {
						let mut rand = [0u8; 8];
						rand.copy_from_slice(&record.rand);
						(rand, record.ediv)
					});
					scrub_record(&mut record);
					match legacy {
						Some((rand, ediv)) => self.controllers[at].encrypt_legacy(handle, &ltk, &rand, ediv),
						None => self.controllers[at].encrypt(handle, &ltk),
					}
					scrub(&mut ltk);
				}
				Some(mut record) => {
					scrub_record(&mut record);
					self.controllers[at].disconnect(handle, REASON_AUTHENTICATION);
				}
				None => self.controllers[at].disconnect(handle, REASON_AUTHENTICATION),
			}
		}
	}

	fn on_encryption(&mut self, at: usize, status: u8, handle: u16, enabled: bool) {
		let Some(link) = self.controllers[at].link_mut(handle) else { return };
		if status != 0 || !enabled {
			self.encryption_failed(at, handle);
			return;
		}
		let peer = link.peer;
		link.encrypted = true;
		// A PAIRING'S ENCRYPTION is the start of its key distribution: the bond is made when the keys are in, and
		// stored durably before it is reported.
		if !link.reconnecting {
			let steps = link.pairing.as_mut().map(Initiator::on_encrypted).unwrap_or_default();
			if link.pairing.is_none() {
				self.encryption_failed(at, handle);
				return;
			}
			self.run_smp(at, handle, steps);
			return;
		}
		let security = self.record(at, &peer).map(|record| security_of(&level_from_wire(&record.level))).unwrap_or(SecurityLevel::EncryptedUnauthenticated);
		if let Some(link) = self.controllers[at].link_mut(handle) {
			link.security = security;
		}
		if self.enabled_peer(at, &peer) == Some(true) {
			self.start_discovery(at, handle);
		}
		self.battery_read(at, handle);
		self.le_audio_start(at, handle);
	}

	fn encryption_failed(&mut self, at: usize, handle: u16) {
		let controller = &mut self.controllers[at];
		let Some(link) = controller.link_mut(handle) else { return };
		link.pairing = None;
		let peer = link.peer;
		controller.fail_attempt(&peer);
		controller.disconnect(handle, REASON_AUTHENTICATION);
	}

	fn on_disconnected(&mut self, at: usize, handle: u16) {
		if self.voice_disconnected(at, handle) || self.le_audio_disconnected(at, handle) {
			return;
		}
		let controller = &mut self.controllers[at];
		let Some(position) = controller.links.iter().position(|link| link.handle == handle) else { return };
		let mut link = controller.links.remove(position);
		link.pairing = None;
		release_streams(&mut link);
		self.gatt_gone(&mut link);
		self.le_audio_link_gone(at, &mut link);
		let controller = &mut self.controllers[at];
		controller.fail_attempt(&link.peer);
		if link.is_classic() {
			self.classic_gone(at, link);
		} else {
			// A BONDED PERIPHERAL GONE IS WAITED FOR AGAIN, through the accept list.
			self.reconnect_bonded(at);
		}
	}

	fn on_advertising(&mut self, at: usize, count: u8, reports: &[u8]) {
		let Ok(found) = hci_codec::reports(count, reports) else {
			print(b"BluetoothService: an advertising report that runs past its event was refused\n");
			return;
		};
		// A COORDINATED SET'S OTHER MEMBER, by its RSI - whether a scan lists it or not.
		for advertisement in &found {
			if let Some(kind) = advertised_kind(advertisement.kind, &advertisement.address) {
				self.le_audio_advertised(at, &PeerAddress { kind, bytes: advertisement.address.to_vec() }, advertisement.data());
			}
		}
		self.on_advertisements(at, found);
	}

	// AN EXTENDED REPORT THAT IS NOT A BROADCAST'S: its whole data looked through for a set member's RSI - an LE Audio
	// device's announcement runs past a legacy report's 31 bytes - then read as the ordinary scan reads a legacy one,
	// whose first 31 bytes are where a name and service classes are.
	pub(crate) fn extended_advertising(&mut self, at: usize, report: &service_logic::le_iso::ExtendedReport) {
		let address = hci_codec::address_from_wire(&report.wire_address);
		if let Some(kind) = advertised_kind(report.address_type & 1, &address) {
			self.le_audio_advertised(at, &PeerAddress { kind, bytes: address.to_vec() }, &report.data);
		}
		let len = report.data.len().min(31);
		let mut data = [0u8; 31];
		data[..len].copy_from_slice(&report.data[..len]);
		let advertisement = hci_codec::Advertisement { kind: report.address_type & 1, address, rssi: report.rssi, data, data_len: len as u8 };
		self.on_advertisements(at, alloc::vec![advertisement]);
	}

	fn on_advertisements(&mut self, at: usize, found: Vec<hci_codec::Advertisement>) {
		let controller = &mut self.controllers[at];
		let Some(scan) = controller.scan.as_mut().filter(|scan| scan.running) else { return };
		for advertisement in found {
			// A STATIC OR A RESOLVABLE RANDOM ADDRESS, or a public one; a non-resolvable private address names
			// nobody this host could find again.
			let Some(kind) = advertised_kind(advertisement.kind, &advertisement.address) else { continue };
			// A PRIVATE ADDRESS A BOND RESOLVES is reported as that bond's identity.
			let address = match (kind, controller.le.resolve(&advertisement.address)) {
				(PeerKind::Resolvable, Some(identity)) => peer_to_wire(&identity),
				_ => PeerAddress { kind, bytes: advertisement.address.to_vec() },
			};
			if scan.results.iter().any(|result| result.address == address) {
				continue;
			}
			if scan.results.len() >= MAX_SCAN_RESULTS {
				scan.running = false;
				controller.le_scan_enable(false);
				return;
			}
			let (name, human_interface) = hci_codec::advertised(advertisement.data());
			let name = name.and_then(|bytes| core::str::from_utf8(&bytes[..bytes.len().min(48)]).ok()).unwrap_or("");
			let services = if human_interface { alloc::vec![ServiceClass { uuid: 0x1812 }] } else { Vec::new() };
			scan.results.push(ScanResult { address, name: String::from(name), rssi: advertisement.rssi as i32, human_interface, radio: Radio::Le, class_of_device: 0, services });
		}
	}

	// ------------------------------------------------------------------ link data

	fn on_acl(&mut self, at: usize, bytes: &[u8]) {
		let Some((handle, boundary, data)) = hci_codec::acl_header(bytes) else { return };
		let now = clock();
		let payload: Vec<u8>;
		let cid;
		let classic;
		{
			let Some(link) = self.controllers[at].link_mut(handle) else { return };
			classic = link.is_classic();
			match link.reassembly.feed(boundary, data, now) {
				Ok(Fed::Complete { cid: channel, .. }) => {
					cid = channel;
					payload = link.reassembly.payload().to_vec();
				}
				Ok(Fed::More { .. }) => return,
				Err(_) => {
					print(b"BluetoothService: a malformed fragment on a link was refused\n");
					return;
				}
			}
		}
		if classic {
			self.classic_pdu(at, handle, cid, &payload);
			return;
		}
		match cid {
			SMP_CID => {
				let steps = match self.controllers[at].link_mut(handle).and_then(|link| link.pairing.as_mut()) {
					Some(pairing) => pairing.on_pdu(&payload),
					None => return,
				};
				self.run_smp(at, handle, steps);
			}
			ATT_CID => self.on_att(at, handle, &payload),
			// A channel the LE half does not open carries nothing it reads.
			_ => {}
		}
	}

	fn on_att(&mut self, at: usize, handle: u16, pdu: &[u8]) {
		// A REQUEST FROM THE PEER, acting as a client, is this host's GATT server's to answer.
		if pdu.first().is_some_and(|code| matches!(*code, 0x02 | 0x04 | 0x06 | 0x08 | 0x0a | 0x0c | 0x0e | 0x10 | 0x12 | 0x16 | 0x18 | 0x20 | 0x52 | 0xd2)) {
			let call = self.audio.call;
			let answer = {
				let Some(link) = self.controllers[at].link_mut(handle) else { return };
				link.server.get_or_insert_with(|| le_audio::host_server(call)).answer(pdu)
			};
			if let Some(answer) = answer {
				self.controllers[at].l2cap(handle, ATT_CID, &answer);
			}
			self.le_server_writes(at, handle);
			return;
		}
		// A NOTIFICATION IS NOT AN ANSWER. It arrives whenever the peer has a report, including in the
		// middle of discovery, and feeding it to the discovery as the answer to its last request would
		// end the procedure with a refusal it did not earn.
		if pdu.first() == Some(&service_logic::att::op::HANDLE_VALUE_NOTIFICATION) {
			self.on_notification(at, handle, pdu);
			return;
		}
		// NOR IS AN INDICATION: confirmed - the peer sends nothing more until it is - and read as a notification.
		if pdu.first() == Some(&0x1d) {
			self.controllers[at].l2cap(handle, ATT_CID, &[0x1e]);
			self.on_notification(at, handle, pdu);
			return;
		}
		enum Stepped {
			Map(hogp_map::Next),
			Boot(gatt_mouse::Next),
		}
		let stepped = match self.controllers[at].link_mut(handle).and_then(|link| link.walk.as_mut()) {
			Some(Walk::Map(discovery)) => Stepped::Map(discovery.on_answer(pdu)),
			Some(Walk::Boot(discovery)) => Stepped::Boot(discovery.on_answer(pdu)),
			// NO WALK: the response is an application's operation's.
			None => {
				self.gatt_response(at, handle, pdu);
				return;
			}
		};
		match stepped {
			Stepped::Map(next) => self.run_map(at, handle, next),
			Stepped::Boot(next) => self.run_boot(at, handle, next),
		}
		// A WALK THAT ENDED frees the bearer for what applications queued.
		if self.controllers[at].link(handle).is_some_and(|link| link.walk.is_none()) {
			self.next_gatt(at, handle);
		}
	}

	// THE HOGP WALK, on an encrypted link of a peer trusted for input: the report map's first.
	fn start_discovery(&mut self, at: usize, handle: u16) {
		let Some(link) = self.controllers[at].link_mut(handle) else { return };
		if link.walk.is_some() || link.decoder.is_some() || !link.encrypted {
			return;
		}
		let (discovery, first) = hogp_map::Discovery::start(service_logic::att::DEFAULT_MTU);
		link.walk = Some(Walk::Map(discovery));
		self.run_map(at, handle, first);
	}

	fn run_map(&mut self, at: usize, handle: u16, next: hogp_map::Next) {
		let controller = &mut self.controllers[at];
		match next {
			hogp_map::Next::Request(pdu) => {
				controller.l2cap(handle, ATT_CID, &pdu);
			}
			hogp_map::Next::Command(pdu, then) => {
				controller.l2cap(handle, ATT_CID, &pdu);
				self.run_map(at, handle, *then);
			}
			hogp_map::Next::Ready(map) => {
				let Some(link) = controller.link_mut(handle) else { return };
				link.walk = None;
				match input::Decoder::new(&map.descriptor) {
					Some(decoder) => {
						let arrivals = decoder.arrivals();
						link.decoder = Some(decoder);
						link.map = Some(map);
						deliver(link, &arrivals);
					}
					None => print(b"BluetoothService: the peer's report map describes nothing this system takes as input\n"),
				}
			}
			// A DEVICE WITHOUT A REPORT MAP is a boot device: the boot mouse's walk takes over.
			hogp_map::Next::Unsupported(hogp_map::Unsupported::NoReportMap) => {
				let (discovery, first) = gatt_mouse::Discovery::start(service_logic::att::DEFAULT_MTU);
				if let Some(link) = controller.link_mut(handle) {
					link.walk = Some(Walk::Boot(discovery));
				}
				self.run_boot(at, handle, first);
			}
			hogp_map::Next::Unsupported(_) => {
				print(b"BluetoothService: the peer's human-interface service has nothing this host can turn on\n");
				if let Some(link) = controller.link_mut(handle) {
					link.walk = None;
				}
			}
			hogp_map::Next::Failed(_) | hogp_map::Next::ServerError(_) => {
				print(b"BluetoothService: the peer's attribute table could not be walked\n");
				if let Some(link) = controller.link_mut(handle) {
					link.walk = None;
				}
			}
		}
	}

	fn run_boot(&mut self, at: usize, handle: u16, next: gatt_mouse::Next) {
		let controller = &mut self.controllers[at];
		match next {
			gatt_mouse::Next::Request(pdu) => {
				controller.l2cap(handle, ATT_CID, &pdu);
			}
			gatt_mouse::Next::Command(pdu, then) => {
				controller.l2cap(handle, ATT_CID, &pdu);
				self.run_boot(at, handle, *then);
			}
			gatt_mouse::Next::Ready { report } => {
				if let Some(link) = controller.link_mut(handle) {
					link.report = Some(report);
					link.walk = None;
				}
			}
			// THE PEER IS NOT A BOOT MOUSE THIS SERVICE CAN DRIVE, and saying so is the whole answer: the
			// bond stands, the link stays encrypted, and nothing is published as input.
			gatt_mouse::Next::Unsupported(_) => print(b"BluetoothService: the peer is not a boot mouse this service can drive\n"),
			gatt_mouse::Next::Failed(_) | gatt_mouse::Next::ServerError(_) => print(b"BluetoothService: the peer's attribute table could not be walked\n"),
		}
	}

	fn on_notification(&mut self, at: usize, handle: u16, pdu: &[u8]) {
		if pdu.len() < 3 {
			return;
		}
		self.gatt_notification(at, handle, pdu);
		let attribute = u16::from_le_bytes([pdu[1], pdu[2]]);
		if self.le_audio_notification(at, handle, attribute, &pdu[3..]) {
			return;
		}
		let Some(link) = self.controllers[at].link_mut(handle) else { return };
		if !link.encrypted {
			return;
		}
		// A REPORT-MAP DEVICE: the notification's handle says which report, and so its id.
		if let Some(id) = link.map.as_ref().and_then(|map| map.id_of(attribute)) {
			let reports = link.decoder.as_mut().map(|decoder| decoder.report_with_id(id, &pdu[3..])).unwrap_or_default();
			deliver(link, &reports);
			return;
		}
		if link.report != Some(attribute) {
			return;
		}
		let Ok(decoded) = service_logic::hogp::report(&pdu[3..]) else { return };
		link.buttons = decoded.buttons;
		let report = InputReport::Pointer(MouseReport { dx: decoded.dx, dy: decoded.dy, wheel: decoded.wheel, buttons: decoded.buttons });
		deliver(link, &[report]);
	}

	// ------------------------------------------------------------------ timers

	// The nearest deadline anything here is waiting for, so the loop wakes for it.
	fn next_deadline(&self) -> u64 {
		let mut soonest = clock().saturating_add(TICKS_PER_SECOND);
		for controller in &self.controllers {
			if let Some(scan) = controller.scan.as_ref().filter(|scan| scan.running) {
				soonest = soonest.min(scan.deadline);
			}
			if let Some(attempt) = controller.attempt.as_ref().filter(|attempt| matches!(attempt.state, PairingState::Connecting | PairingState::Pairing)) {
				soonest = soonest.min(attempt.deadline);
			}
			if let Some(due) = controller.bredr_state.next_deadline(&controller.links) {
				soonest = soonest.min(due);
			}
		}
		soonest
	}

	fn run_timers(&mut self) {
		let now = clock();
		for at in 0..self.controllers.len() {
			let controller = &mut self.controllers[at];
			if let Some(scan) = controller.scan.as_mut()
				&& scan.running
				&& now >= scan.deadline
			{
				scan.running = false;
				controller.le_scan_enable(false);
			}
			if let Some(attempt) = controller.attempt.as_mut()
				&& matches!(attempt.state, PairingState::Connecting | PairingState::Pairing)
				&& now >= attempt.deadline
			{
				// SIXTY SECONDS, WHATEVER THE PEER DOES. A connection that never completes is cancelled
				// and a link that stalled mid-exchange is disconnected; either way the attempt is over.
				attempt.state = PairingState::Failed;
				let peer = attempt.peer;
				match controller.link_to(&peer).map(|link| link.handle) {
					Some(handle) => controller.disconnect(handle, REASON_USER),
					None if peer[0] == KIND_BREDR => {
						controller.command(service_logic::hci_bredr::opcode::CREATE_CONNECTION_CANCEL, &peer[1..].iter().rev().copied().collect::<Vec<u8>>());
					}
					None => {
						controller.command(opcode::LE_CREATE_CONNECTION_CANCEL, &[]);
					}
				}
			}
			for link in controller.links.iter_mut() {
				if link.reassembly.expire(now) {
					print(b"BluetoothService: an incomplete packet on a link was given up at its deadline\n");
				}
			}
			self.classic_timers(at, now);
			// A NEW PRIVATE ADDRESS when the old one's time is up - not while a connection attempt is out, which the
			// controller would refuse.
			let controller = &self.controllers[at];
			if controller.powered && controller.le.own.is_some() && now >= controller.le.rotate_at && !controller.le.accepting && controller.attempt.as_ref().is_none_or(|attempt| attempt.state != PairingState::Connecting) {
				self.rotate_address(at);
			}
		}
	}
}

// ------------------------------------------------------------------ the read interface

struct ReadView<'a> {
	stack: &'a mut Stack,
}

impl bluetooth::Service for ReadView<'_> {
	fn controllers(&mut self) -> Result<Vec<ControllerInfo>, Error> {
		Ok(self
			.stack
			.controllers
			.iter()
			.map(|controller| {
				let (connectable, discoverable) = controller.bredr_state.scan_flags();
				ControllerInfo { address: local_wire(controller), powered: controller.powered, secure_connections: controller.secure_connections, epoch: controller.session.epoch(), classic: controller.classic, connectable, discoverable, pairable: controller.bredr_state.watcher != 0 }
			})
			.collect())
	}

	fn scan(&mut self, at: u32, deadline_ms: u32) -> Result<ScanHandle, Error> {
		let id = self.stack.next_scan;
		let controller = self.stack.controllers.get_mut(at as usize).ok_or(Error::NotFound)?;
		if !controller.powered {
			return Err(Error::Closed);
		}
		if controller.scan.as_ref().is_some_and(Scan::active) || self.stack.broadcast.scanning.is_some_and(|(held, _)| held == at as usize) {
			return Err(Error::Again);
		}
		// THE CALLER'S DEADLINE IS CAPPED, NOT BELIEVED. A scan is a radio running, and a client asking
		// for an hour of it is a client deciding how the machine behaves.
		let ms = deadline_ms.min(MAX_SCAN_MS);
		let ticks = (ms as u64).saturating_mul(TICKS_PER_SECOND) / 1000;
		if !controller.le_scan_parameters() || !controller.le_scan_enable(true) {
			return Err(Error::Exhausted);
		}
		// BOTH RADIOS IN ONE SCAN: inquiry runs beside the LE scan for as long, in its own units of 1.28 s.
		let inquiring = controller.classic && controller.command(service_logic::hci_bredr::opcode::INQUIRY, &service_logic::hci_bredr::inquiry(ms.div_ceil(1280) as u8, 0));
		controller.scan = Some(Scan { id, deadline: clock().saturating_add(ticks.max(1)), running: true, inquiring, results: Vec::new(), paging: Vec::new() });
		self.stack.next_scan = self.stack.next_scan.wrapping_add(1);
		Ok(ScanHandle { id })
	}

	fn results(&mut self, scan: ScanHandle) -> Result<Vec<ScanResult>, Error> {
		self.stack.controllers.iter().find_map(|controller| controller.scan.as_ref().filter(|found| found.id == scan.id)).map(|found| found.results.clone()).ok_or(Error::NotFound)
	}

	fn scanning(&mut self, scan: ScanHandle) -> Result<bool, Error> {
		self.stack.controllers.iter().find_map(|controller| controller.scan.as_ref().filter(|found| found.id == scan.id)).map(Scan::active).ok_or(Error::NotFound)
	}

	fn cancel(&mut self, scan: ScanHandle) -> Result<(), Error> {
		for controller in &mut self.stack.controllers {
			if let Some(found) = controller.scan.as_mut().filter(|found| found.id == scan.id) {
				let (running, inquiring) = (found.running, found.inquiring);
				found.running = false;
				found.inquiring = false;
				if running {
					controller.le_scan_enable(false);
				}
				if inquiring {
					controller.command(service_logic::hci_bredr::opcode::INQUIRY_CANCEL, &[]);
				}
				return Ok(());
			}
		}
		Err(Error::NotFound)
	}
}

// ------------------------------------------------------------------ the operator interface

struct OperatorView<'a> {
	stack: &'a mut Stack,
}

impl bluetooth_operator::Service for OperatorView<'_> {
	// THE PLATFORM'S OWN ALLOW OR DENY FOR A DEVICE IS DEVICEMANAGER'S PERSISTENT POLICY, and this
	// service invents no radio policy beside it. A controller an operator disabled there has no
	// binding, publishes no provider and is not in this list at all - so a request naming it is
	// `not-found`, which is the refusal that setting produces. What `power` controls is whether THIS
	// host uses a controller it has: off ends every link, scan and attempt on it and stops the host
	// driving the radio; on runs initialisation again.
	fn power(&mut self, at: u32, on: bool) -> Result<(), Error> {
		let controller = self.stack.controllers.get_mut(at as usize).ok_or(Error::NotFound)?;
		if on {
			if !controller.powered && controller.init == Init::Ready {
				controller.start_init();
			}
			return Ok(());
		}
		// OFF IS A CONTROLLER RESET, NOT A DISCONNECT AND A SCAN-DISABLE QUEUED BEHIND WHATEVER IS IN FLIGHT.
		// This host forgets the session at once, so commands still waiting their turn would be dropped with
		// it and leave the controller connected and scanning against the operator's request. `HCI_Reset`
		// ends every link and scan in the controller itself; it goes out alone, ahead of anything the
		// forgotten session was waiting for.
		// Ready and off, even when this arrives halfway through initialisation: the next power-on starts
		// it again from the reset.
		self.stack.le_audio_reset(at as usize);
		let controller = &mut self.stack.controllers[at as usize];
		controller.end_session();
		controller.powered = false;
		controller.init = Init::Ready;
		controller.waiting_step = None;
		controller.reconnect = None;
		controller.command(opcode::RESET, &[]);
		controller.pump();
		Ok(())
	}

	fn pair(&mut self, at: u32, peer: PeerAddress) -> Result<(), Error> {
		self.stack.pair(at as usize, &peer, false)
	}

	fn progress(&mut self, at: u32) -> Result<PairingProgress, Error> {
		let controller = self.stack.controllers.get(at as usize).ok_or(Error::NotFound)?;
		match controller.attempt.as_ref() {
			Some(attempt) => Ok(PairingProgress { state: attempt.state, security: attempt.security, address: peer_to_wire(&attempt.peer) }),
			None => Ok(PairingProgress { state: PairingState::Idle, security: SecurityLevel::None, address: peer_to_wire(&[0; 7]) }),
		}
	}

	fn cancel(&mut self, at: u32) -> Result<(), Error> {
		let controller = self.stack.controllers.get_mut(at as usize).ok_or(Error::NotFound)?;
		let Some(attempt) = controller.attempt.as_mut() else { return Err(Error::NotFound) };
		if !matches!(attempt.state, PairingState::Connecting | PairingState::Pairing) {
			return Err(Error::NotFound);
		}
		attempt.state = PairingState::Failed;
		let peer = attempt.peer;
		match controller.link_to_mut(&peer) {
			Some(link) => {
				link.pairing = None;
				let handle = link.handle;
				controller.disconnect(handle, REASON_USER);
			}
			None if peer[0] == KIND_BREDR => {
				let wire: Vec<u8> = peer[1..].iter().rev().copied().collect();
				controller.command(service_logic::hci_bredr::opcode::CREATE_CONNECTION_CANCEL, &wire);
			}
			None => {
				controller.command(opcode::LE_CREATE_CONNECTION_CANCEL, &[]);
			}
		}
		Ok(())
	}

	fn bonded(&mut self, at: u32) -> Result<Vec<BondedPeer>, Error> {
		if at as usize >= self.stack.controllers.len() {
			return Err(Error::NotFound);
		}
		if self.stack.bonds == 0 {
			return Err(Error::Closed);
		}
		let records = self.stack.bonds().list(&local_wire(&self.stack.controllers[at as usize])).ok_or(Error::Closed)??;
		Ok(records.into_iter().map(|record| BondedPeer { address: record.peer, name: record.name, enabled: record.enabled, security: record.security, radio: record.radio, level: record.level, trusted: record.trusted, alias: record.alias }).collect())
	}

	// DURABLY, AND BEFORE THIS ANSWERS: the store's delete commits before it replies, and only then is
	// the live link to that peer terminated and the operator told. A forget that reported success and
	// left the record on the volume would be a device that reconnects after a reboot.
	fn forget(&mut self, at: u32, peer: PeerAddress) -> Result<(), Error> {
		let peer = peer_from_wire(&peer).ok_or(Error::Invalid)?;
		let controller = self.stack.controllers.get(at as usize).ok_or(Error::NotFound)?;
		if self.stack.bonds == 0 {
			return Err(Error::Closed);
		}
		self.stack.bonds().delete(&local_wire(controller), &peer_to_wire(&peer)).ok_or(Error::Closed)??;
		let controller = &mut self.stack.controllers[at as usize];
		if controller.reconnect == Some(peer) {
			controller.reconnect = None;
		}
		if let Some(handle) = controller.link_to(&peer).map(|link| link.handle) {
			controller.disconnect(handle, REASON_USER);
		}
		self.stack.refresh_policy(at as usize);
		if !peer_is_classic(&peer) {
			self.stack.relist(at as usize);
		}
		Ok(())
	}

	// THE SAME AS TRUSTING IT FOR INPUT, and the record says both.
	fn enable(&mut self, at: u32, peer: PeerAddress, on: bool) -> Result<(), Error> {
		let peer = peer_from_wire(&peer).ok_or(Error::Invalid)?;
		self.stack.set_trust(at as usize, &peer, Profile::Input, on)
	}

	fn prompts(&mut self, at: u32) -> Result<Vec<proto::system::PairingPrompt>, Error> {
		// Validated here; the stream itself is made by `serve_prompts`, which owns the channel.
		let controller = self.stack.controllers.get(at as usize).ok_or(Error::NotFound)?;
		if controller.bredr_state.watcher != 0 {
			return Err(Error::Again);
		}
		Ok(Vec::new())
	}

	fn answer(&mut self, at: u32, prompt: u32, reply: PromptReply) -> Result<(), Error> {
		if at as usize >= self.stack.controllers.len() {
			return Err(Error::NotFound);
		}
		self.stack.answer(at as usize, prompt, reply)
	}

	fn discoverable(&mut self, at: u32, seconds: u32) -> Result<(), Error> {
		let controller = self.stack.controllers.get_mut(at as usize).ok_or(Error::NotFound)?;
		if !controller.classic {
			return Err(Error::Unsupported);
		}
		if !controller.powered {
			return Err(Error::Closed);
		}
		controller.bredr_state.discoverable_until = (seconds > 0).then(|| clock() + service_logic::bt_policy::discoverable_until(0, seconds) * TICKS_PER_SECOND / 1000);
		self.stack.refresh_policy(at as usize);
		Ok(())
	}

	fn trust(&mut self, at: u32, peer: PeerAddress, profile: Profile, on: bool) -> Result<(), Error> {
		let peer = peer_from_wire(&peer).ok_or(Error::Invalid)?;
		self.stack.set_trust(at as usize, &peer, profile, on)
	}

	fn alias(&mut self, at: u32, peer: PeerAddress, alias: String) -> Result<(), Error> {
		let peer = peer_from_wire(&peer).ok_or(Error::Invalid)?;
		if at as usize >= self.stack.controllers.len() {
			return Err(Error::NotFound);
		}
		// AN ALIAS IS A NAME A GRANT CAN CARRY: printable, no separator a policy row splits on, and one peer
		// per alias on a controller.
		if alias.len() > 32 || !alias.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_') {
			return Err(Error::Invalid);
		}
		if !alias.is_empty() && self.stack.records(at as usize).iter().any(|record| record.alias == alias && peer_from_wire(&record.peer) != Some(peer)) {
			return Err(Error::Again);
		}
		self.stack.rewrite_bond(at as usize, &peer, |record| record.alias = alias)
	}

	fn devices(&mut self, at: u32) -> Result<Vec<DeviceStatus>, Error> {
		if at as usize >= self.stack.controllers.len() {
			return Err(Error::NotFound);
		}
		Ok(self.stack.devices(at as usize))
	}

	fn connect(&mut self, at: u32, peer: PeerAddress, profile: Profile) -> Result<(), Error> {
		let peer = peer_from_wire(&peer).ok_or(Error::Invalid)?;
		if at as usize >= self.stack.controllers.len() {
			return Err(Error::NotFound);
		}
		self.stack.connect_profile(at as usize, &peer, profile)
	}

	fn disconnect(&mut self, at: u32, peer: PeerAddress, profile: Profile) -> Result<(), Error> {
		let peer = peer_from_wire(&peer).ok_or(Error::Invalid)?;
		if at as usize >= self.stack.controllers.len() {
			return Err(Error::NotFound);
		}
		self.stack.disconnect_profile(at as usize, &peer, profile)
	}

	fn pair_legacy(&mut self, at: u32, peer: PeerAddress) -> Result<(), Error> {
		self.stack.pair(at as usize, &peer, true)
	}

	// THE REMOTE CONTROL'S BUTTONS, toward a phone playing to this system.
	fn connect_pan(&mut self, at: u32, peer: PeerAddress, replace_uplink: bool) -> Result<(), Error> {
		let peer = peer_from_wire(&peer).ok_or(Error::Invalid)?;
		if at as usize >= self.stack.controllers.len() {
			return Err(Error::NotFound);
		}
		self.stack.connect_pan(at as usize, &peer, replace_uplink)
	}

	fn broadcast_scan(&mut self, at: u32, seconds: u32) -> Result<(), Error> {
		self.stack.broadcast_scan(at as usize, seconds)
	}

	fn broadcasts(&mut self, at: u32) -> Result<Vec<proto::system::BroadcastSource>, Error> {
		self.stack.broadcasts(at as usize)
	}

	fn broadcast_play(&mut self, at: u32, broadcast_id: u32, code: Vec<u8>) -> Result<(), Error> {
		if at as usize >= self.stack.controllers.len() {
			return Err(Error::NotFound);
		}
		let result = self.stack.broadcast_play(at as usize, broadcast_id, &code);
		let mut code = code;
		scrub(&mut code);
		result
	}

	fn broadcast_stop(&mut self, at: u32) -> Result<(), Error> {
		self.stack.broadcast_stop(at as usize)
	}

	fn send(&mut self, at: u32, peer: PeerAddress, name: String, length: Option<u64>) -> Result<u64, Error> {
		let peer = peer_from_wire(&peer).ok_or(Error::Invalid)?;
		if at as usize >= self.stack.controllers.len() {
			return Err(Error::NotFound);
		}
		self.stack.push_object(at as usize, peer, &name, length)
	}

	// Validated here; the stream itself is made by `serve_receive`, which owns the channel.
	fn receive(&mut self, at: u32, peer: PeerAddress, max_bytes: u64) -> Result<Vec<proto::system::ReceivedObject>, Error> {
		let peer = peer_from_wire(&peer).ok_or(Error::Invalid)?;
		if at as usize >= self.stack.controllers.len() {
			return Err(Error::NotFound);
		}
		self.stack.receive_object(at as usize, peer, max_bytes)?;
		self.stack.receive_args = Some((at as usize, peer, max_bytes));
		Ok(Vec::new())
	}

	fn media(&mut self, at: u32, peer: PeerAddress, command: proto::system::MediaCommand) -> Result<(), Error> {
		use service_logic::avrcp::Operation;
		let peer = peer_from_wire(&peer).ok_or(Error::Invalid)?;
		if at as usize >= self.stack.controllers.len() || !peer_is_classic(&peer) {
			return Err(Error::NotFound);
		}
		let operation = match command {
			proto::system::MediaCommand::Play => Operation::Play,
			proto::system::MediaCommand::Pause => Operation::Pause,
			proto::system::MediaCommand::Next => Operation::Forward,
			proto::system::MediaCommand::Previous => Operation::Backward,
		};
		self.stack.media_command(at as usize, &peer, operation)
	}
}

impl Stack {
	// PAIR WITH WHAT THE RADIO REPORTED: an address this controller's current scan did not find is not a
	// choice made from a scan, whatever the operator typed. `legacy` is the operator's word that this device
	// cannot do better than a PIN.
	fn pair(&mut self, at: usize, peer: &PeerAddress, legacy: bool) -> Result<(), Error> {
		let peer = peer_from_wire(peer).ok_or(Error::Invalid)?;
		if at >= self.controllers.len() {
			return Err(Error::NotFound);
		}
		// LE LEGACY IS NEVER RUN OVER A SECURE CONNECTIONS BOND, whatever the scan heard.
		if legacy && peer[0] != KIND_BREDR && self.record(at, &peer).is_some_and(|record| level_from_wire(&record.level).agreement == Agreement::SecureConnections) {
			return Err(Error::Denied);
		}
		let controller = &mut self.controllers[at];
		if !controller.powered {
			return Err(Error::Closed);
		}
		// ONE ATTEMPT PER CONTROLLER IS LIVE, and a second is refused rather than queued - a queued
		// pairing is one an operator has stopped watching.
		if controller.attempt.as_ref().is_some_and(|attempt| matches!(attempt.state, PairingState::Connecting | PairingState::Pairing)) {
			return Err(Error::Again);
		}
		let wire = peer_to_wire(&peer);
		if !controller.scan.as_ref().is_some_and(|scan| scan.results.iter().any(|result| result.address == wire)) {
			return Err(Error::NotFound);
		}
		if peer[0] == KIND_BREDR {
			return self.pair_classic(at, peer, legacy);
		}
		self.pair_le(at, peer, legacy)
	}

	// TRUST, per peer and per profile, durably; input trust and `enabled` are the same thing.
	fn set_trust(&mut self, at: usize, peer: &Peer, profile: Profile, on: bool) -> Result<(), Error> {
		if at >= self.controllers.len() {
			return Err(Error::NotFound);
		}
		self.rewrite_bond(at, peer, |record| {
			record.trusted.retain(|held| *held != profile);
			if on {
				record.trusted.push(profile);
			}
			if profile == Profile::Input {
				record.enabled = on;
			}
		})?;
		let controller = &mut self.controllers[at];
		if profile == Profile::Input
			&& !on && let Some(link) = controller.link_to_mut(peer)
			&& peer_is_classic(peer)
		{
			// A CLASSIC DEVICE NO LONGER TRUSTED FOR INPUT: its streams end, what it held released first.
			release_streams(link);
		}
		if profile == Profile::Input
			&& let Some(handle) = controller.link_to(peer).map(|link| link.handle)
			&& !peer_is_classic(peer)
		{
			if on {
				// An enabled peer that is already connected and encrypted becomes an input source now.
				if controller.link(handle).is_some_and(|link| link.encrypted) {
					self.start_discovery(at, handle);
				}
			} else if let Some(link) = controller.link_mut(handle) {
				// DISABLING CLOSES WHATEVER THE INPUT SERVICE HOLDS FOR IT, releasing what is held first.
				release_streams(link);
				link.report = None;
				link.map = None;
				link.decoder = None;
				link.walk = None;
			}
		}
		self.refresh_policy(at);
		// AN LE PEER'S RECONNECTION is what input, GATT and audio trust admit: the accept list is made again.
		if !peer_is_classic(peer) && matches!(profile, Profile::Input | Profile::Gatt | Profile::Audio) {
			self.relist(at);
		}
		// AN LE PEER TRUSTED FOR AUDIO, already connected: its table is walked now.
		if profile == Profile::Audio
			&& on && !peer_is_classic(peer)
			&& let Some(handle) = self.controllers[at].link_to(peer).map(|link| link.handle)
		{
			self.le_audio_start(at, handle);
		}
		Ok(())
	}

	// Every device this controller knows, on both radios: its bonds, its links and what its current scan found.
	fn devices(&self, at: usize) -> Vec<DeviceStatus> {
		let controller = &self.controllers[at];
		let mut out: Vec<DeviceStatus> = Vec::new();
		for record in self.records(at) {
			let Some(peer) = peer_from_wire(&record.peer) else { continue };
			let link = controller.link_to(&peer);
			out.push(DeviceStatus { address: record.peer, radio: record.radio, name: record.name, alias: record.alias, bonded: true, connected: link.is_some(), level: Some(record.level), trusted: record.trusted, connected_profiles: link.map(classic::connected_profiles).unwrap_or_default(), battery: link.and_then(battery_of) });
		}
		for link in &controller.links {
			let wire = peer_to_wire(&link.peer);
			if out.iter().any(|device| device.address == wire) {
				continue;
			}
			let name = controller.bredr_state.name_of(&link.peer).unwrap_or_default();
			out.push(DeviceStatus { address: wire, radio: radio_of(&link.peer), name, alias: String::new(), bonded: false, connected: true, level: None, trusted: Vec::new(), connected_profiles: classic::connected_profiles(link), battery: battery_of(link) });
		}
		if let Some(scan) = controller.scan.as_ref() {
			for result in &scan.results {
				if out.iter().any(|device| device.address == result.address) {
					continue;
				}
				out.push(DeviceStatus { address: result.address.clone(), radio: result.radio, name: result.name.clone(), alias: String::new(), bonded: false, connected: false, level: None, trusted: Vec::new(), connected_profiles: Vec::new(), battery: None });
			}
		}
		out
	}
}

// The battery a peer reported: a headset over its voice gateway's HF indicator, an LE peer in its Battery Service.
fn battery_of(link: &Link) -> Option<u8> {
	link.classic.as_ref().and_then(|classic| classic.voice.as_ref()?.battery).or(link.gatt.battery)
}

fn peer_is_classic(peer: &Peer) -> bool {
	peer[0] == KIND_BREDR
}

// ------------------------------------------------------------------ the profile interface

struct ProfileView<'a> {
	stack: &'a mut Stack,
	// Which controller and link the accepted stream is for, set by `open_mouse` for the caller to attach the
	// stream's producer to.
	target: Option<(usize, u16)>,
}

impl bluetooth_profile::Service for ProfileView<'_> {
	fn open_input(&mut self, at: u32, peer: PeerAddress) -> Result<Vec<InputReport>, Error> {
		let peer_address = peer_from_wire(&peer).ok_or(Error::Invalid)?;
		if at as usize >= self.stack.controllers.len() {
			return Err(Error::NotFound);
		}
		// ONLY OVER A PEER AN OPERATOR TRUSTED FOR INPUT. The input service cannot reach a peer that is merely
		// bonded, and a peer an operator has not approved is not an input source whatever it sends.
		match self.stack.enabled_peer(at as usize, &peer_address) {
			Some(true) => {}
			Some(false) => return Err(Error::Denied),
			None => return Err(Error::NotFound),
		}
		let controller = &self.stack.controllers[at as usize];
		match controller.link_to(&peer_address) {
			Some(link) if link.streams.len() >= MAX_STREAMS => return Err(Error::Exhausted),
			Some(link) => self.target = Some((at as usize, link.handle)),
			// The peer is bonded and enabled and not connected right now: the stream is refused with
			// `closed`, which is the same answer a lost link gives, so a consumer retries the same way.
			None => return Err(Error::Closed),
		}
		Ok(Vec::new())
	}

	// The peers `open-input` would accept: bonded AND trusted for input, on every controller and both radios.
	fn enabled(&mut self) -> Result<Vec<EnabledPeer>, Error> {
		if self.stack.bonds == 0 {
			return Err(Error::Closed);
		}
		let mut out = Vec::new();
		for at in 0..self.stack.controllers.len() {
			out.extend(self.stack.records(at).into_iter().filter(|record| record.enabled).map(|record| EnabledPeer { controller: at as u32, peer: record.peer }));
		}
		Ok(out)
	}
}

// ------------------------------------------------------------------ the catalogue and a controller's life

impl Stack {
	// A controller the catalogue published: open it, attach, and begin initialising.
	fn adopt(&mut self, catalogue: u64, info: ProviderInfo) {
		if self.controllers.len() >= MAX_CONTROLLERS {
			print(b"BluetoothService: a third controller was published; this service drives two\n");
			return;
		}
		let Some(Ok(transport)) = provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(&info) else {
			print(b"BluetoothService: a published controller could not be opened\n");
			return;
		};
		let mut client = hci_transport::Client::new(ChannelTransport { chan: transport });
		let attachment: HciAttachment = match client.attach(&HCI_VERSION) {
			Some(Ok(attachment)) => attachment,
			_ => {
				print(b"BluetoothService: a controller refused the transport version this host speaks\n");
				close(transport);
				return;
			}
		};
		let packets = client.receive().unwrap_or(0);
		let control = client.control().unwrap_or(0);
		if packets == 0 || control == 0 {
			close(transport);
			return;
		}
		let limits = Limits::of(attachment.iso, attachment.max_command, attachment.max_event, attachment.max_acl, attachment.max_iso);
		let queue = attachment.acl_queue.min(MAX_QUEUED as u32);
		let acl_bytes = attachment.max_acl.saturating_sub(4).max(27);
		let mut controller = Controller { info, transport, packets, control, limits, credits: Credits::new(attachment.command_credits.clamp(1, 1), attachment.acl_credits, queue), bredr: Credits::new(1, attachment.acl_credits, queue), le_bytes: acl_bytes, bredr_bytes: acl_bytes, sco_bytes: if attachment.sco { attachment.max_sco.min(258) } else { 0 }, extended: false, le_audio_capable: false, iso_free: 0, iso_dropped: 0, iso_reassembly: service_logic::le_iso::Reassembly::default(), unclassified: 0, le_shares: false, session: Session::new(attachment.epoch), address: [0; 6], powered: false, secure_connections: false, classic: false, classic_sc: false, init: Init::Running, steps: VecDeque::new(), waiting_step: None, pending: Vec::new(), outstanding: None, public_key: None, scan: None, attempt: None, links: Vec::new(), reconnect: None, bredr_state: classic::ControllerState::new(), le: le::LeState::new() };
		controller.start_init();
		controller.pump();
		self.controllers.push(controller);
	}

	// A controller the catalogue withdrew is gone: this publication is over, and a replacement arrives
	// as a new publication and is initialised from the start.
	fn withdraw(&mut self, info: &ProviderInfo) {
		let Some(at) = self.controllers.iter().position(|controller| controller.info.slot == info.slot && controller.info.provider_generation == info.provider_generation) else { return };
		let mut controller = self.controllers.remove(at);
		controller.end_session();
		controller.bredr_state.detach_watcher();
		close(controller.packets);
		close(controller.control);
		close(controller.transport);
	}

	// Packets from one controller. A PACKET STAMPED WITH AN EPOCH THIS HOST HAS LEFT IS DISCARDED: it
	// is a reply to a command nobody sent, addressed to a link that no longer exists.
	fn drain_packets(&mut self, at: usize, buf: &mut [u8]) -> bool {
		loop {
			let (len, handles) = match try_recv_caps(self.controllers[at].packets, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return true,
				PolledCaps::Closed => return false,
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut frame_handles = wire::Handles::new();
			let Some(mut packet) = hci_transport::receive_read(&buf[..len], &mut frame_handles) else {
				scrub(&mut buf[..len]);
				continue;
			};
			self.on_packet(at, &packet);
			// A PACKET CAN CARRY KEY MATERIAL - the Diffie-Hellman key and a link key notification arrive in
			// events - so both copies of it are zeroed once it has been handled, however that went: the decoded
			// bytes are freed next, and `buf` is reused for every message after this one.
			scrub(&mut packet.bytes);
			scrub(&mut buf[..len]);
		}
	}

	fn on_packet(&mut self, at: usize, packet: &HciPacket) {
		if !self.controllers[at].session.admits(packet.epoch) {
			return;
		}
		let kind = match packet.kind {
			HciPacketKind::Event => Kind::Event,
			// A TRANSPORT THAT CARRIES ISO delivers it with ACL: told apart by the handle, each then held to its own kind's
			// ceiling - and a handle neither table names dropped and counted.
			HciPacketKind::Acl if self.controllers[at].limits.iso => {
				let controller = &self.controllers[at];
				let class = service_logic::le_iso::classify(&packet.bytes, |handle| self.iso_handle(at, handle), |handle| controller.link(handle).is_some(), controller.limits.acl, controller.limits.iso_bytes);
				match class {
					service_logic::le_iso::Class::Acl => Kind::Acl,
					service_logic::le_iso::Class::Iso => {
						self.on_iso(at, &packet.bytes);
						self.controllers[at].pump();
						return;
					}
					service_logic::le_iso::Class::Unknown | service_logic::le_iso::Class::TooLong => {
						let controller = &mut self.controllers[at];
						controller.unclassified = controller.unclassified.saturating_add(1);
						return;
					}
				}
			}
			HciPacketKind::Acl => Kind::Acl,
			// A TRANSPORT THAT CARRIES ISO APART - a UART's own packet type - is held to the ISO ceiling, and only an
			// ISO stream's handle is taken.
			HciPacketKind::Iso => {
				let controller = &self.controllers[at];
				if controller.limits.iso && service_logic::hci::check_inbound(&controller.limits, Kind::Iso as u16, packet.bytes.len() as u32).is_ok() && packet.bytes.len() >= 2 && self.iso_handle(at, u16::from_le_bytes([packet.bytes[0], packet.bytes[1]]) & 0x0fff) {
					self.on_iso(at, &packet.bytes);
					self.controllers[at].pump();
				} else {
					let controller = &mut self.controllers[at];
					controller.unclassified = controller.unclassified.saturating_add(1);
				}
				return;
			}
			// VOICE is bounded by what the transport said it carries, and only by a transport that carries it.
			HciPacketKind::Sco => {
				if packet.bytes.len() as u32 <= self.controllers[at].sco_bytes {
					self.on_sco(at, &packet.bytes);
				}
				return;
			}
			_ => return,
		};
		if service_logic::hci::check_inbound(&self.controllers[at].limits, kind as u16, packet.bytes.len() as u32).is_err() {
			return;
		}
		match kind {
			Kind::Event => self.on_event(at, &packet.bytes),
			Kind::Acl => self.on_acl(at, &packet.bytes),
			_ => {}
		}
		self.controllers[at].pump();
	}

	// ONE ISO PACKET IN: a whole SDU, once its fragments are together, to the stream that owns its handle.
	fn on_iso(&mut self, at: usize, bytes: &[u8]) {
		let Some(sdu) = self.controllers[at].iso_reassembly.push(bytes) else { return };
		if self.broadcast_handle(at, sdu.handle) {
			self.broadcast_sdu(&sdu);
		} else {
			self.le_audio_sdu(at, &sdu);
		}
	}

	// What happened to one controller's transport.
	fn drain_control(&mut self, at: usize, buf: &mut [u8]) -> bool {
		loop {
			let (len, handles) = match try_recv_caps(self.controllers[at].control, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return true,
				PolledCaps::Closed => return false,
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut frame_handles = wire::Handles::new();
			let Some(event) = hci_transport::control_read(&buf[..len], &mut frame_handles) else { continue };
			match event.kind {
				// A RESET IS A NEW SESSION: links, scans and profile handles from the old one are gone,
				// and initialisation runs again from the start.
				HciControlKind::Reset => {
					self.le_audio_reset(at);
					let controller = &mut self.controllers[at];
					controller.end_session();
					controller.session.advance();
					controller.start_init();
					controller.pump();
				}
				HciControlKind::Fault => {
					self.le_audio_reset(at);
					let controller = &mut self.controllers[at];
					controller.end_session();
					controller.powered = false;
				}
				HciControlKind::Removed => return false,
			}
		}
	}
}

// ------------------------------------------------------------------ the loop

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	// 1. the roles the plan says this service is handed: a catalogue connection scoped to the HCI
	//    kind, the private bond-store endpoint, and the three roots its clients reach it on.
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (catalogue, bonds, read_root, operator_root, profile_root, admin_root, audio_root, network_root) = (roles[0], roles[1], roles[2], roles[3], roles[4], roles[5], roles[6], roles[7]);

	// 2. subscribe to the controllers this machine publishes. A machine with none has none, which is
	//    a subscription that stays quiet rather than a failure.
	let subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::BluetoothHci).unwrap_or(0) } else { 0 };
	let mut stack = Stack { controllers: Vec::new(), bonds, next_scan: 1, grants: Vec::new(), serials: Vec::new(), pushes: Vec::new(), receivers: Vec::new(), receive_args: None, audio: audio::AudioRoot::new(), network: pan::NetworkRoot::new(), le_devices: Vec::new(), broadcast: broadcast::Sink::default() };
	send_blocking(bootstrap, b"BluetoothService: online", 0);

	let mut clients: Vec<Client> = Vec::new();
	let mut buf = alloc::vec![0u8; 8192];
	let mut reply = alloc::vec![0u8; 8192];
	let mut subscribed = subscription != 0;
	loop {
		stack.serve_audio_subscriber(&mut buf);
		let mut waitset: Vec<u64> = Vec::with_capacity(8 + clients.len() + 3 * MAX_CONTROLLERS);
		for root in [read_root, operator_root, profile_root, admin_root, audio_root, network_root] {
			if root != 0 {
				waitset.push(root);
			}
		}
		// EVERY GRANT, and the task each is for: its end ends the grant.
		for grant in &stack.grants {
			waitset.push(grant.chan);
			waitset.push(grant.owner);
		}
		for grant in &stack.serials {
			waitset.push(grant.chan);
			waitset.push(grant.owner);
		}
		// OBJECT PUSHES' CHANNELS, and the receivers' streams - sending nothing, waited on so that a holder that closes
		// one is seen at once.
		waitset.extend(stack.pushes.iter().map(|pusher| pusher.chan));
		// NETWORKSERVICE'S FRAMES on the PAN links it opened.
		waitset.extend(stack.pan_channels());
		waitset.extend(stack.receiver_streams());
		if subscribed {
			waitset.push(subscription);
		}
		for controller in &stack.controllers {
			waitset.push(controller.packets);
			waitset.push(controller.control);
			// A PROMPT WATCHER sends nothing; its end is waited on so that its closing - `btctl` ending - is
			// seen at once and the controller stops being pairable.
			if controller.bredr_state.watcher != 0 {
				waitset.push(controller.bredr_state.watcher);
			}
		}
		waitset.extend(clients.iter().map(|client| client.chan));
		// The subscriber sends no requests here; its closure ends all of its PCM ownership.
		if stack.audio.subscriber != 0 {
			waitset.push(stack.audio.subscriber);
		}
		// THE OPENED ENDPOINTS' CHANNELS, each read only while it holds no unanswered request.
		waitset.extend(stack.audio.pcm.iter().filter(|pcm| !pcm.holding()).map(|pcm| pcm.chan));
		let deadline = [stack.pcm_deadline(), stack.serial_deadline(), stack.opp_deadline(), stack.broadcast_deadline(), stack.le_audio_deadline(), stack.gatt_deadline()].into_iter().flatten().fold(stack.next_deadline(), u64::min);
		let ready = wait_any(&waitset, deadline);
		stack.run_timers();
		stack.broadcast_timers();
		stack.le_audio_timers();
		stack.pcm_timers();
		stack.serial_timers();
		stack.gatt_timers();
		stack.opp_timers();
		for controller in &mut stack.controllers {
			controller.pump();
		}
		if ready < 0 {
			continue;
		}
		let handle = waitset[ready as usize];
		if handle == stack.audio.subscriber {
			stack.serve_audio_subscriber(&mut buf);
			continue;
		}

		// A CONTROLLER'S PACKETS OR ITS CONTROL STREAM.
		if let Some(at) = stack.controllers.iter().position(|controller| controller.packets == handle) {
			if !stack.drain_packets(at, &mut buf) {
				let info = stack.controllers[at].info.clone();
				stack.withdraw(&info);
			}
			continue;
		}
		if let Some(at) = stack.controllers.iter().position(|controller| controller.control == handle) {
			if !stack.drain_control(at, &mut buf) {
				let info = stack.controllers[at].info.clone();
				stack.withdraw(&info);
			}
			continue;
		}
		if let Some(at) = stack.controllers.iter().position(|controller| controller.bredr_state.watcher == handle) {
			stack.drain_watcher(at, &mut buf);
			continue;
		}
		if let Some(index) = stack.grants.iter().position(|grant| grant.chan == handle) {
			stack.serve_grant(index, &mut buf);
			continue;
		}
		if let Some(index) = stack.audio.pcm.iter().position(|pcm| pcm.chan == handle) {
			stack.serve_pcm(index, &mut buf);
			for controller in &mut stack.controllers {
				controller.pump();
			}
			continue;
		}
		if let Some(index) = stack.grants.iter().position(|grant| grant.owner == handle) {
			// THE LAUNCH IS OVER: its grant goes with it.
			let grant = stack.grants.remove(index);
			gatt::end_grant(grant);
			continue;
		}
		if let Some(index) = stack.serials.iter().position(|grant| grant.chan == handle) {
			stack.serve_serial(index, &mut buf);
			continue;
		}
		if let Some(index) = stack.serials.iter().position(|grant| grant.owner == handle) {
			stack.end_serial(index);
			continue;
		}
		if let Some(index) = stack.pushes.iter().position(|pusher| pusher.chan == handle) {
			stack.serve_push(index, &mut buf);
			continue;
		}
		if stack.pan_channels().contains(&handle) {
			stack.serve_pan(handle, &mut buf);
			for controller in &mut stack.controllers {
				controller.pump();
			}
			continue;
		}
		if stack.receiver_streams().any(|stream| stream == handle) {
			if matches!(try_recv_caps(handle, &mut buf), PolledCaps::Closed) {
				stack.receiver_gone(handle);
			}
			continue;
		}

		// THE CATALOGUE: a controller arriving or leaving.
		if subscribed && handle == subscription {
			loop {
				let (len, handles) = match try_recv_caps(subscription, &mut buf) {
					PolledCaps::Message { len, handles } => (len, handles),
					PolledCaps::Empty => break,
					PolledCaps::Closed => {
						subscribed = false;
						break;
					}
				};
				for &leftover in handles.as_slice() {
					close(leftover);
				}
				let mut frame_handles = wire::Handles::new();
				let Some(info) = provider_catalogue::subscribe_read(&buf[..len], &mut frame_handles) else { continue };
				if info.live {
					stack.adopt(catalogue, info);
				} else {
					stack.withdraw(&info);
				}
			}
			continue;
		}

		// A ROOT: mint a connection for the interface this root serves. THE ROOT DECIDES THE
		// INTERFACE, and nothing the client later sends can change it.
		let root_interface = if handle == read_root {
			Some(Interface::Read)
		} else if handle == operator_root {
			Some(Interface::Operator)
		} else if handle == profile_root {
			Some(Interface::Profile)
		} else if handle == admin_root {
			Some(Interface::Admin)
		} else if handle == audio_root {
			Some(Interface::Audio)
		} else if handle == network_root {
			Some(Interface::Network)
		} else {
			None
		};
		let (interface, is_root) = match root_interface {
			Some(interface) => (interface, true),
			None => match clients.iter().find(|client| client.chan == handle) {
				Some(client) => (client.interface, false),
				None => continue,
			},
		};
		let (len, mut handles) = match try_recv_caps(handle, &mut buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => continue,
			PolledCaps::Closed => {
				if !is_root {
					clients.retain(|client| client.chan != handle);
					close(handle);
				}
				continue;
			}
		};
		if len >= 2 {
			let op = u16::from_le_bytes([buf[0], buf[1]]);
			if op == HEARTBEAT_OP {
				send_blocking(handle, b"PONG", 0);
				continue;
			}
			if op == CONNECT_OP {
				// BOUNDED: a server that mints a channel per request without a bound is one a client
				// can exhaust.
				if clients.len() >= MAX_CLIENTS {
					send_blocking(handle, &[], 0);
					continue;
				}
				match channel() {
					Some((mine, theirs)) => {
						clients.push(Client { chan: mine, interface });
						send_blocking(handle, &[], theirs);
					}
					None => {
						send_blocking(handle, &[], 0);
					}
				}
				continue;
			}
			if interface == Interface::Profile && op == bluetooth_profile::OP_OPEN_INPUT {
				serve_open_input(&mut stack, handle, &buf[..len], &mut handles, &mut reply);
				continue;
			}
			if interface == Interface::Operator && op == bluetooth_operator::OP_PROMPTS {
				serve_prompts(&mut stack, handle, &buf[..len], &mut handles, &mut reply);
				continue;
			}
			if interface == Interface::Operator && op == bluetooth_operator::OP_RECEIVE {
				serve_receive(&mut stack, handle, &buf[..len], &mut handles, &mut reply);
				continue;
			}
			if interface == Interface::Network && op == proto::system::bluetooth_network::OP_LINKS {
				pan::serve_links(&mut stack, handle, &buf[..len], &mut handles, &mut reply);
				continue;
			}
			if interface == Interface::Audio && op == proto::system::bluetooth_audio::OP_ENDPOINTS {
				audio::serve_endpoints(&mut stack, handle, &buf[..len], &mut handles, &mut reply);
				continue;
			}
		}
		let mut reply_handles = wire::Handles::new();
		let written = match interface {
			Interface::Read => bluetooth::dispatch(&mut ReadView { stack: &mut stack }, &buf[..len], &mut handles, &mut reply, &mut reply_handles),
			Interface::Operator => bluetooth_operator::dispatch(&mut OperatorView { stack: &mut stack }, &buf[..len], &mut handles, &mut reply, &mut reply_handles),
			Interface::Profile => bluetooth_profile::dispatch(&mut ProfileView { stack: &mut stack, target: None }, &buf[..len], &mut handles, &mut reply, &mut reply_handles),
			Interface::Admin => proto::system::bluetooth_admin::dispatch(&mut gatt::AdminView { stack: &mut stack }, &buf[..len], &mut handles, &mut reply, &mut reply_handles),
			Interface::Audio => proto::system::bluetooth_audio::dispatch(&mut audio::AudioView { stack: &mut stack }, &buf[..len], &mut handles, &mut reply, &mut reply_handles),
			Interface::Network => proto::system::bluetooth_network::dispatch(&mut pan::NetworkView { stack: &mut stack }, &buf[..len], &mut handles, &mut reply, &mut reply_handles),
		};
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		if let Some(written) = written
			&& !send_caps_blocking(handle, &reply[..written], reply_handles.as_slice())
		{
			for &leftover in reply_handles.as_slice() {
				close(leftover);
			}
		}
		for controller in &mut stack.controllers {
			controller.pump();
		}
	}
}

// `open-input` is a stream: validated by the service trait, then answered with the CONSUMER end of a
// fresh pair whose producer is kept on the link - which is where reports are written as they arrive. A
// device whose decoder knows its gamepads tells the new stream of them first.
fn serve_open_input(stack: &mut Stack, channel: u64, request: &[u8], handles: &mut wire::Handles, reply: &mut [u8]) {
	let mut view = ProfileView { stack, target: None };
	let Some((corr, result)) = bluetooth_profile::open_input_open(&mut view, request, handles) else { return };
	let target = view.target;
	let answer = match (result, target) {
		(Ok(_), Some((at, link_handle))) => match channel_with_depth(64) {
			Some((producer, consumer)) => match stack.controllers[at].link_mut(link_handle) {
				Some(link) => {
					link.streams.push(producer);
					if let Some(len) = bluetooth_profile::open_input_reply_ok(corr, reply)
						&& send_caps_blocking(channel, &reply[..len], &[consumer])
					{
						for report in link.decoder.as_ref().map(input::Decoder::arrivals).unwrap_or_default() {
							write_input(producer, &report);
						}
						return;
					}
					link.streams.retain(|&stream| stream != producer);
					close(producer);
					close(consumer);
					return;
				}
				None => {
					close(producer);
					close(consumer);
					Error::Closed
				}
			},
			None => Error::Exhausted,
		},
		(Err(error), _) => error,
		(Ok(_), None) => Error::Invalid,
	};
	if let Some(len) = bluetooth_profile::open_input_reply_err(corr, &answer, reply) {
		send_blocking(channel, &reply[..len], 0);
	}
}

// `receive` is a stream: validated by the operator view, then answered with the consumer end of a fresh pair whose
// producer the receiver keeps - which is where the object is told as it arrives.
fn serve_receive(stack: &mut Stack, channel: u64, request: &[u8], handles: &mut wire::Handles, reply: &mut [u8]) {
	let mut view = OperatorView { stack };
	let Some((corr, result)) = bluetooth_operator::receive_open(&mut view, request, handles) else { return };
	let answer = match (result, stack.receive_args.take()) {
		(Ok(_), Some((at, peer, max_bytes))) => match channel_with_depth(32) {
			Some((producer, consumer)) => {
				if let Some(len) = bluetooth_operator::receive_reply_ok(corr, reply)
					&& send_caps_blocking(channel, &reply[..len], &[consumer])
				{
					stack.receiver_started(at, peer, max_bytes, producer);
					return;
				}
				close(producer);
				close(consumer);
				return;
			}
			None => Error::Exhausted,
		},
		(Err(error), _) => error,
		(Ok(_), None) => Error::Invalid,
	};
	if let Some(len) = bluetooth_operator::receive_reply_err(corr, &answer, reply) {
		send_blocking(channel, &reply[..len], 0);
	}
}

// `prompts` is a stream too: the watcher's producer is kept on the controller, and while it is there the
// controller is pairable and declares the IO capabilities a person can answer.
fn serve_prompts(stack: &mut Stack, channel: u64, request: &[u8], handles: &mut wire::Handles, reply: &mut [u8]) {
	let mut view = OperatorView { stack };
	let Some((corr, result)) = bluetooth_operator::prompts_open(&mut view, request, handles) else { return };
	// The controller the request named, read back from its first argument after the operation and correlation.
	let at = request.get(6..10).map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize);
	let answer = match (result, at) {
		(Ok(_), Some(at)) if at < stack.controllers.len() => match channel_with_depth(16) {
			Some((producer, consumer)) => {
				if let Some(len) = bluetooth_operator::prompts_reply_ok(corr, reply)
					&& send_caps_blocking(channel, &reply[..len], &[consumer])
				{
					stack.controllers[at].bredr_state.watcher = producer;
					stack.refresh_policy(at);
					return;
				}
				close(producer);
				close(consumer);
				return;
			}
			None => Error::Exhausted,
		},
		(Err(error), _) => error,
		(Ok(_), _) => Error::Invalid,
	};
	if let Some(len) = bluetooth_operator::prompts_reply_err(corr, &answer, reply) {
		send_blocking(channel, &reply[..len], 0);
	}
}
