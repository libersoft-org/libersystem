// BluetoothService - the host stack above HCI, for the boot mouse slice and nothing more.
//
// WHAT RUNS HERE AND WHAT DOES NOT. This program parses a stranger's radio packets, so it is built to
// hold as little as possible while it does: no device claim, no MMIO, no DMA and no link key at rest.
// The controller is reached over a `bluetooth-hci` provider from the catalogue, the keys over a
// private capability to the bond store, and the pointer it eventually produces leaves over a typed
// report channel to InputService. Its Domain is sized by the supervisor and holds zero DMA.
//
// EVERY PROTOCOL DECISION IS IN `service_logic`, held by host tests - the HCI codec and credits,
// L2CAP reassembly, the ATT walks, the SMP initiator and its derivations against the specification's
// own sample data, the GATT discovery and the boot report. What is here is the IO around them: which
// packet goes where, what waits on what, and what a client is told.
//
// THE SLICE IS DELIBERATELY NARROW. One LE central link per controller, public and static-random
// peers, Just Works with LE Secure Connections and no downgrade, a boot mouse over HOGP. Anything
// else a peer offers is a typed "unsupported" rather than an attempt.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{BondRecord, BondedPeer, ControllerInfo, EnabledPeer, Error, HciAttachment, HciControlKind, HciPacketKind, MouseReport, PairingProgress, PairingState, PeerAddress, PeerKind, ProviderInfo, ProviderKind, ScanHandle, ScanResult, SecurityLevel, bluetooth, bluetooth_bond_store, bluetooth_operator, bluetooth_profile, hci_transport, provider_catalogue};
use rt::*;
use service_logic::gatt_mouse::{Discovery, Next};
use service_logic::hci::{Credits, Kind, Limits, Session};
use service_logic::hci_codec::{self, Event, opcode};
use service_logic::l2cap::{Fed, Reassembly};
use service_logic::smp_pairing::{Initiator, Step};

include!(concat!(env!("OUT_DIR"), "/roles_bluetooth_service.rs"));

// ------------------------------------------------------------------ the bounds the milestone states

// Two controllers, one link each.
const MAX_CONTROLLERS: usize = 2;
// Client connections across all three interfaces.
const MAX_CLIENTS: usize = 16;
// Commands and data queued for one controller before a send is refused.
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
// The two fixed L2CAP channels this slice uses.
const ATT_CID: u16 = 0x0004;
const SMP_CID: u16 = 0x0006;
// Why a link is disconnected: remote user terminated, and authentication failure.
const REASON_USER: u8 = 0x13;
const REASON_AUTHENTICATION: u8 = 0x05;

// --------------------------------------------------------------------------------- the state

// Which interface a client connection speaks. The ROOT a connection was minted from decides this,
// and nothing the client sends can change it - which is the whole of the three-authority design.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Interface {
	Read,
	Operator,
	Profile,
}

struct Client {
	chan: u64,
	interface: Interface,
}

// A scan and what it has found, deduplicated by address.
struct Scan {
	id: u32,
	deadline: u64,
	running: bool,
	results: Vec<ScanResult>,
}

// A pairing attempt's progress, for the operator's `progress`.
struct Attempt {
	peer: [u8; 7],
	deadline: u64,
	state: PairingState,
	security: SecurityLevel,
}

// One link to one peer.
struct Link {
	handle: u16,
	// The peer's type byte and address, most significant first - the form every derivation takes.
	peer: [u8; 7],
	reassembly: Reassembly,
	pairing: Option<Initiator>,
	discovery: Option<Discovery>,
	encrypted: bool,
	security: SecurityLevel,
	report: Option<u16>,
	// Buttons last reported, so a loss can release what the peer was holding.
	buttons: u8,
	streams: Vec<u64>,
	// The key this link was encrypted with when it came from the bond store rather than a pairing,
	// so a failure to encrypt can be told apart from a pairing failure.
	reconnecting: bool,
}

// The initialisation sequence, one command at a time.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Init {
	Reset,
	ReadAddress,
	ReadCommands,
	ReadBuffer,
	EventMask,
	LeEventMask,
	PublicKey,
	Ready,
}

struct Controller {
	info: ProviderInfo,
	transport: u64,
	packets: u64,
	control: u64,
	limits: Limits,
	credits: Credits,
	session: Session,
	address: [u8; 6],
	powered: bool,
	secure_connections: bool,
	init: Init,
	pending: Vec<(u16, Vec<u8>)>,
	outstanding: Option<u16>,
	public_key: Option<[u8; 64]>,
	scan: Option<Scan>,
	attempt: Option<Attempt>,
	link: Option<Link>,
	// A bonded, enabled peer this controller should connect to without pairing.
	reconnect: Option<[u8; 7]>,
}

struct Stack {
	controllers: Vec<Controller>,
	bonds: u64,
	next_scan: u32,
}

// ------------------------------------------------------------------ address forms

fn peer_to_wire(peer: &[u8; 7]) -> PeerAddress {
	PeerAddress { kind: if peer[0] == 0 { PeerKind::Public } else { PeerKind::RandomStatic }, bytes: peer[1..].to_vec() }
}

fn peer_from_wire(address: &PeerAddress) -> Option<[u8; 7]> {
	if address.bytes.len() != 6 {
		return None;
	}
	let mut out = [0u8; 7];
	out[0] = match address.kind {
		PeerKind::Public => 0,
		PeerKind::RandomStatic => 1,
	};
	out[1..].copy_from_slice(&address.bytes);
	Some(out)
}

fn local_address(controller: &Controller) -> [u8; 7] {
	let mut out = [0u8; 7];
	out[1..].copy_from_slice(&controller.address);
	out
}

fn local_wire(controller: &Controller) -> PeerAddress {
	peer_to_wire(&local_address(controller))
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
		self.pending.push((op, params.to_vec()));
		true
	}

	// Send the next command if the controller will take one. AT MOST ONE IS OUTSTANDING, and the
	// credit comes back on the controller's own completion - never on the transport's acceptance,
	// which is the defect `hci::Credits` exists to rule out.
	fn pump(&mut self) {
		if self.outstanding.is_some() || self.pending.is_empty() {
			return;
		}
		if self.credits.take(Kind::Command).is_err() {
			return;
		}
		let (op, params) = self.pending.remove(0);
		let Some(bytes) = hci_codec::command(op, &params) else {
			self.credits.commands_completed(1);
			self.credits.drained();
			return;
		};
		let sent = hci_transport::Client::new(ChannelTransport { chan: self.transport }).send(&HciPacketKind::Command, &bytes);
		self.credits.drained();
		match sent {
			Some(Ok(_)) => self.outstanding = Some(op),
			_ => {
				// A SEND THE TRANSPORT REFUSED IS NOT OUTSTANDING, and the credit it took is given
				// back: the controller never saw it and will never answer it.
				self.credits.commands_completed(1);
				print(b"BluetoothService: the transport refused a command\n");
			}
		}
	}

	// Send L2CAP data on the link, as one ACL packet. The boot mouse slice never sends a PDU larger
	// than one controller buffer, so there is no fragmentation on the way out.
	fn l2cap(&mut self, cid: u16, payload: &[u8]) -> bool {
		let Some(link) = self.link.as_ref() else { return false };
		let mut pdu = Vec::with_capacity(4 + payload.len());
		pdu.extend_from_slice(&(payload.len() as u16).to_le_bytes());
		pdu.extend_from_slice(&cid.to_le_bytes());
		pdu.extend_from_slice(payload);
		if pdu.len() as u32 > self.limits.ceiling(Kind::Acl).saturating_sub(4) {
			return false;
		}
		if self.credits.take(Kind::Acl).is_err() {
			print(b"BluetoothService: no controller buffer for link data; a packet is dropped\n");
			return false;
		}
		let packet = hci_codec::acl(link.handle, 0b00, &pdu);
		let sent = hci_transport::Client::new(ChannelTransport { chan: self.transport }).send(&HciPacketKind::Acl, &packet);
		self.credits.drained();
		if !matches!(sent, Some(Ok(_))) {
			self.credits.acl_completed(1);
			return false;
		}
		true
	}

	// Begin (or begin again) the initialisation sequence.
	fn start_init(&mut self) {
		self.init = Init::Reset;
		self.powered = false;
		self.public_key = None;
		self.pending.clear();
		self.outstanding = None;
		self.credits.reset();
		self.command(opcode::RESET, &[]);
	}

	// The next step of initialisation, after the previous one completed.
	fn advance_init(&mut self) {
		self.init = match self.init {
			Init::Reset => {
				self.command(opcode::READ_BD_ADDR, &[]);
				Init::ReadAddress
			}
			Init::ReadAddress => {
				self.command(opcode::READ_LOCAL_SUPPORTED_COMMANDS, &[]);
				Init::ReadCommands
			}
			Init::ReadCommands => {
				self.command(opcode::LE_READ_BUFFER_SIZE, &[]);
				Init::ReadBuffer
			}
			Init::ReadBuffer => {
				// The default mask and LE meta events, which is where everything this slice reads
				// about a link arrives.
				self.command(opcode::SET_EVENT_MASK, &0x2000_1fff_ffff_ffffu64.to_le_bytes());
				Init::EventMask
			}
			Init::EventMask => {
				// Connection complete, advertising report, and the two key-agreement completions.
				self.command(opcode::LE_SET_EVENT_MASK, &0x0000_0000_0000_01ffu64.to_le_bytes());
				Init::LeEventMask
			}
			Init::LeEventMask => {
				// A CONTROLLER WITHOUT THE P-256 COMMANDS IS READY AND CANNOT PAIR, and it says so in
				// `controllers` rather than failing halfway through a pairing later.
				if self.secure_connections {
					self.command(opcode::LE_READ_LOCAL_P256_PUBLIC_KEY, &[]);
					Init::PublicKey
				} else {
					self.powered = true;
					Init::Ready
				}
			}
			Init::PublicKey => {
				self.powered = true;
				Init::Ready
			}
			Init::Ready => Init::Ready,
		};
	}

	// End whatever this controller's session held: links, scans, attempts and report streams. A
	// reset or a fault does this, and so does powering the radio off.
	fn end_session(&mut self) {
		if let Some(mut link) = self.link.take() {
			release_streams(&mut link);
		}
		if let Some(scan) = self.scan.as_mut() {
			scan.running = false;
		}
		if let Some(attempt) = self.attempt.as_mut()
			&& matches!(attempt.state, PairingState::Connecting | PairingState::Pairing)
		{
			attempt.state = PairingState::Failed;
		}
		self.pending.clear();
		self.outstanding = None;
		self.credits.reset();
	}
}

// A lost link releases what its peer was holding and ends every report stream. A final report with
// no buttons comes first, so a consumer that stops hearing from this peer is not left with a button
// held down for ever.
fn release_streams(link: &mut Link) {
	if link.buttons != 0 {
		let released = MouseReport { dx: 0, dy: 0, wheel: 0, buttons: 0 };
		for &stream in &link.streams {
			write_report(stream, &released);
		}
		link.buttons = 0;
	}
	for stream in link.streams.drain(..) {
		close(stream);
	}
}

// One report onto a stream. A stream whose consumer stopped draining is not waited for: the report
// is dropped for that consumer, which is the one behaviour that keeps a slow reader from stopping
// the radio.
fn write_report(stream: u64, report: &MouseReport) -> bool {
	let mut frame = [0u8; 64];
	let mut handles = wire::Handles::new();
	let Some(len) = bluetooth_profile::open_mouse_frame(0, report, &mut frame, &mut handles) else { return false };
	!matches!(try_send_outcome(stream, &frame[..len], 0), SendOutcome::Failed)
}

// ------------------------------------------------------------------ the bond store

impl Stack {
	fn bonds(&self) -> bluetooth_bond_store::Client<ChannelTransport> {
		bluetooth_bond_store::Client::new(ChannelTransport { chan: self.bonds })
	}

	// The stored record for this controller and peer, key included.
	fn bond(&self, at: usize, peer: &[u8; 7]) -> Option<BondRecord> {
		if self.bonds == 0 {
			return None;
		}
		match self.bonds().lookup(&local_wire(&self.controllers[at]), &peer_to_wire(peer)) {
			Some(Ok(record)) if record.key.len() == 16 => Some(record),
			_ => None,
		}
	}

	// The first bonded peer on this controller an operator has made an input source, which is the
	// peer this controller connects to on its own after a start, a restart or a reset.
	fn enabled_bond(&self, at: usize) -> Option<[u8; 7]> {
		if self.bonds == 0 {
			return None;
		}
		match self.bonds().list(&local_wire(&self.controllers[at])) {
			Some(Ok(records)) => records.iter().find(|record| record.enabled).and_then(|record| peer_from_wire(&record.peer)),
			_ => None,
		}
	}
}

// ------------------------------------------------------------------ events from the controller

impl Stack {
	fn on_event(&mut self, at: usize, bytes: &[u8]) {
		let event = match hci_codec::event(bytes) {
			Ok(event) => event,
			// AN EVENT THIS HOST DOES NOT READ IS IGNORED, and a malformed one is too - but the second
			// is said, because a controller sending events whose lengths disagree with their bytes is a
			// controller whose every later event deserves suspicion.
			Err(hci_codec::Refusal::Unhandled { .. }) => return,
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
				let steps = {
					let Some(link) = self.controllers[at].link.as_mut() else { return };
					let Some(pairing) = link.pairing.as_mut() else { return };
					if status == 0 && key.len() == 32 {
						let mut wire = [0u8; 32];
						wire.copy_from_slice(key);
						pairing.on_dhkey(&wire)
					} else {
						pairing.on_dhkey_failed()
					}
				};
				self.run_smp(at, steps);
			}
			Event::Connected { status, handle, peer_kind, peer } => self.on_connected(at, status, handle, peer_kind, peer),
			Event::Disconnected { handle, .. } => self.on_disconnected(at, handle),
			Event::EncryptionChange { status, handle, enabled } => self.on_encryption(at, status, handle, enabled),
			Event::CompletedPackets { count, rest, .. } => {
				let controller = &mut self.controllers[at];
				controller.credits.acl_completed(count as u32);
				for entry in rest.chunks_exact(4) {
					controller.credits.acl_completed(u16::from_le_bytes([entry[2], entry[3]]) as u32);
				}
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
			if status != 0 {
				print(b"BluetoothService: the controller refused an initialisation command; it is not used\n");
				controller.powered = false;
				return;
			}
			match op {
				opcode::READ_BD_ADDR if params.len() >= 7 => {
					let mut wire = [0u8; 6];
					wire.copy_from_slice(&params[1..7]);
					controller.address = hci_codec::address_from_wire(&wire);
				}
				opcode::READ_LOCAL_SUPPORTED_COMMANDS => {
					controller.secure_connections = params.get(1 + hci_codec::P256_OCTET).is_some_and(|octet| octet & hci_codec::P256_BITS == hci_codec::P256_BITS);
				}
				opcode::LE_READ_BUFFER_SIZE if params.len() >= 4 => {
					// THE CONTROLLER'S OWN BUFFER COUNT, bounded by the transport's queue: a
					// controller that reports zero shares its classic buffers, which this slice does
					// not read, so the attachment's advertised count stands.
					let buffers = params[3] as u32;
					if buffers > 0 {
						controller.credits = Credits::new(1, buffers, MAX_QUEUED as u32);
					}
				}
				_ => {}
			}
			let expected = matches!((controller.init, op), (Init::Reset, opcode::RESET) | (Init::ReadAddress, opcode::READ_BD_ADDR) | (Init::ReadCommands, opcode::READ_LOCAL_SUPPORTED_COMMANDS) | (Init::ReadBuffer, opcode::LE_READ_BUFFER_SIZE) | (Init::EventMask, opcode::SET_EVENT_MASK) | (Init::LeEventMask, opcode::LE_SET_EVENT_MASK));
			if expected {
				controller.advance_init();
				if controller.init == Init::Ready {
					self.after_ready(at);
				}
			}
			return;
		}
		if op == opcode::LE_SET_SCAN_ENABLE && status != 0 {
			if let Some(scan) = controller.scan.as_mut() {
				scan.running = false;
			}
		}
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
			opcode::LE_CREATE_CONNECTION => {
				if let Some(attempt) = controller.attempt.as_mut() {
					attempt.state = PairingState::Failed;
				}
				controller.reconnect = None;
			}
			opcode::LE_GENERATE_DHKEY => {
				let steps = match controller.link.as_mut().and_then(|link| link.pairing.as_mut()) {
					Some(pairing) => pairing.on_dhkey_failed(),
					None => return,
				};
				self.run_smp(at, steps);
			}
			opcode::LE_ENABLE_ENCRYPTION => self.encryption_failed(at),
			_ => {}
		}
	}

	// A controller that finished initialising connects to the peer an operator enabled, if there is
	// one - which is what makes a bond survive a restart and a reboot without pairing again.
	fn after_ready(&mut self, at: usize) {
		if !self.controllers[at].powered || self.controllers[at].link.is_some() {
			return;
		}
		if let Some(peer) = self.enabled_bond(at) {
			let controller = &mut self.controllers[at];
			controller.reconnect = Some(peer);
			let mut address = [0u8; 6];
			address.copy_from_slice(&peer[1..]);
			controller.command(opcode::LE_CREATE_CONNECTION, &hci_codec::create_connection(peer[0], &address));
		}
	}

	fn on_connected(&mut self, at: usize, status: u8, handle: u16, peer_kind: u8, address: [u8; 6]) {
		let mut peer = [0u8; 7];
		peer[0] = peer_kind;
		peer[1..].copy_from_slice(&address);
		let controller = &mut self.controllers[at];
		if status != 0 {
			if let Some(attempt) = controller.attempt.as_mut()
				&& attempt.peer == peer
			{
				attempt.state = PairingState::Failed;
			}
			controller.reconnect = None;
			return;
		}
		// ONE LINK PER CONTROLLER. A second connection is a peer this slice did not ask for, and it is
		// disconnected rather than held.
		if controller.link.is_some() {
			controller.command(opcode::DISCONNECT, &hci_codec::disconnect(handle, REASON_USER));
			return;
		}
		let pairing_peer = controller.attempt.as_ref().is_some_and(|attempt| attempt.peer == peer && attempt.state == PairingState::Connecting);
		let reconnecting = controller.reconnect == Some(peer);
		if !pairing_peer && !reconnecting {
			controller.command(opcode::DISCONNECT, &hci_codec::disconnect(handle, REASON_USER));
			return;
		}
		controller.link = Some(Link { handle, peer, reassembly: Reassembly::new(), pairing: None, discovery: None, encrypted: false, security: SecurityLevel::None, report: None, buttons: 0, streams: Vec::new(), reconnecting });
		if pairing_peer {
			self.start_pairing(at);
		} else {
			// RECONNECT: the key comes from the store, and encryption is PROVED before input is
			// enabled - a link that will not encrypt with the stored key is a peer that is not the one
			// that bonded, or one that forgot the bond, and either way it is not an input source.
			match self.bond(at, &peer) {
				Some(record) => {
					let mut ltk = [0u8; 16];
					ltk.copy_from_slice(&record.key);
					self.controllers[at].command(opcode::LE_ENABLE_ENCRYPTION, &hci_codec::enable_encryption(handle, &ltk));
				}
				None => {
					let controller = &mut self.controllers[at];
					controller.command(opcode::DISCONNECT, &hci_codec::disconnect(handle, REASON_AUTHENTICATION));
				}
			}
		}
	}

	fn start_pairing(&mut self, at: usize) {
		let controller = &mut self.controllers[at];
		let Some(public_key) = controller.public_key else { return };
		// HEALTHY SYSTEM RANDOMNESS OR NO PAIRING. A nonce from a predictable source is a pairing an
		// attacker can reproduce; `random_insecure` is refused by name here, and a machine without a
		// healthy source refuses to pair rather than pairing badly.
		let mut na = [0u8; 16];
		if random_get(&mut na) != na.len() {
			print(b"BluetoothService: no healthy random source; pairing is refused\n");
			if let Some(attempt) = controller.attempt.as_mut() {
				attempt.state = PairingState::Failed;
			}
			if let Some(link) = controller.link.as_ref() {
				let handle = link.handle;
				controller.command(opcode::DISCONNECT, &hci_codec::disconnect(handle, REASON_AUTHENTICATION));
			}
			return;
		}
		let local = local_address(controller);
		let Some(link) = controller.link.as_mut() else { return };
		let (pairing, first) = Initiator::start(local, link.peer, public_key, na);
		na.fill(0);
		link.pairing = Some(pairing);
		if let Some(attempt) = controller.attempt.as_mut() {
			attempt.state = PairingState::Pairing;
		}
		self.run_smp(at, alloc::vec![first]);
	}

	fn run_smp(&mut self, at: usize, steps: Vec<Step>) {
		for step in steps {
			let controller = &mut self.controllers[at];
			match step {
				Step::Send(pdu) => {
					controller.l2cap(SMP_CID, &pdu);
				}
				Step::GenerateDhKey(key) => {
					controller.command(opcode::LE_GENERATE_DHKEY, &hci_codec::generate_dhkey(&key));
				}
				Step::Encrypt(ltk) => {
					if let Some(link) = controller.link.as_ref() {
						let handle = link.handle;
						controller.command(opcode::LE_ENABLE_ENCRYPTION, &hci_codec::enable_encryption(handle, &ltk));
					}
				}
				Step::Failed(_) => {
					if let Some(attempt) = controller.attempt.as_mut() {
						attempt.state = PairingState::Failed;
					}
					if let Some(link) = controller.link.as_ref() {
						let handle = link.handle;
						controller.command(opcode::DISCONNECT, &hci_codec::disconnect(handle, REASON_AUTHENTICATION));
					}
				}
			}
		}
	}

	fn on_encryption(&mut self, at: usize, status: u8, handle: u16, enabled: bool) {
		let Some(link) = self.controllers[at].link.as_ref() else { return };
		if link.handle != handle {
			return;
		}
		if status != 0 || !enabled {
			self.encryption_failed(at);
			return;
		}
		let peer = link.peer;
		let reconnecting = link.reconnecting;
		let pairing_key = link.pairing.as_ref().and_then(|pairing| pairing.ltk());
		if !reconnecting {
			let Some(mut ltk) = pairing_key else {
				self.encryption_failed(at);
				return;
			};
			// BONDED ONLY AFTER THE DURABLE COMMIT. The store answers after its writer session has
			// committed, and a pairing is not reported as bonded before that answer - a bond that
			// exists only in memory is one a reboot silently removes. A store that cannot write ENDS
			// the attempt: there is no transient bond to fall back to.
			let local = local_wire(&self.controllers[at]);
			let record = BondRecord { version: service_logic::bond_store::VERSION, local, peer: peer_to_wire(&peer), key: ltk.to_vec(), security: SecurityLevel::EncryptedUnauthenticated, name: alloc::string::String::new(), enabled: false };
			let stored = self.bonds != 0 && matches!(self.bonds().store(&record), Some(Ok(())));
			ltk.fill(0);
			let controller = &mut self.controllers[at];
			if let Some(link) = controller.link.as_mut() {
				// The key has done its work in this process; the store holds the copy that outlives it.
				link.pairing = None;
			}
			if !stored {
				print(b"BluetoothService: the bond could not be stored durably; the pairing is abandoned\n");
				if let Some(attempt) = controller.attempt.as_mut() {
					attempt.state = PairingState::Failed;
				}
				controller.command(opcode::DISCONNECT, &hci_codec::disconnect(handle, REASON_AUTHENTICATION));
				return;
			}
			if let Some(attempt) = controller.attempt.as_mut() {
				attempt.state = PairingState::Bonded;
				attempt.security = SecurityLevel::EncryptedUnauthenticated;
			}
		}
		let controller = &mut self.controllers[at];
		if let Some(link) = controller.link.as_mut() {
			link.encrypted = true;
			link.security = SecurityLevel::EncryptedUnauthenticated;
		}
		if self.bond(at, &peer).is_some_and(|record| record.enabled) {
			self.start_discovery(at);
		}
	}

	fn encryption_failed(&mut self, at: usize) {
		let controller = &mut self.controllers[at];
		if let Some(attempt) = controller.attempt.as_mut()
			&& matches!(attempt.state, PairingState::Pairing | PairingState::Connecting)
		{
			attempt.state = PairingState::Failed;
		}
		controller.reconnect = None;
		if let Some(link) = controller.link.as_mut() {
			link.pairing = None;
			let handle = link.handle;
			controller.command(opcode::DISCONNECT, &hci_codec::disconnect(handle, REASON_AUTHENTICATION));
		}
	}

	fn on_disconnected(&mut self, at: usize, handle: u16) {
		let controller = &mut self.controllers[at];
		if controller.link.as_ref().is_none_or(|link| link.handle != handle) {
			return;
		}
		if let Some(mut link) = controller.link.take() {
			link.pairing = None;
			release_streams(&mut link);
		}
		if let Some(attempt) = controller.attempt.as_mut()
			&& matches!(attempt.state, PairingState::Connecting | PairingState::Pairing)
		{
			attempt.state = PairingState::Failed;
		}
		controller.reconnect = None;
	}

	fn on_advertising(&mut self, at: usize, count: u8, reports: &[u8]) {
		let Ok(found) = hci_codec::reports(count, reports) else {
			print(b"BluetoothService: an advertising report that runs past its event was refused\n");
			return;
		};
		let controller = &mut self.controllers[at];
		let Some(scan) = controller.scan.as_mut().filter(|scan| scan.running) else { return };
		for advertisement in found {
			// Only the two address kinds this slice speaks; a resolvable private address is one it
			// cannot bond to.
			let kind = match advertisement.kind {
				0 => PeerKind::Public,
				1 if advertisement.address[0] & 0xc0 == 0xc0 => PeerKind::RandomStatic,
				_ => continue,
			};
			let address = PeerAddress { kind, bytes: advertisement.address.to_vec() };
			if scan.results.iter().any(|result| result.address == address) {
				continue;
			}
			let (name, human_interface) = hci_codec::advertised(advertisement.data());
			let name = name.and_then(|bytes| core::str::from_utf8(&bytes[..bytes.len().min(48)]).ok()).unwrap_or("");
			scan.results.push(ScanResult { address, name: alloc::string::String::from(name), rssi: advertisement.rssi as i32, human_interface });
			if scan.results.len() >= MAX_SCAN_RESULTS {
				scan.running = false;
				controller.command(opcode::LE_SET_SCAN_ENABLE, &hci_codec::scan_enable(false));
				return;
			}
		}
	}

	// ------------------------------------------------------------------ link data

	fn on_acl(&mut self, at: usize, bytes: &[u8]) {
		let Some((handle, boundary, data)) = hci_codec::acl_header(bytes) else { return };
		let now = clock();
		let payload: Vec<u8>;
		let cid;
		{
			let Some(link) = self.controllers[at].link.as_mut() else { return };
			if link.handle != handle {
				return;
			}
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
		match cid {
			SMP_CID => {
				let steps = match self.controllers[at].link.as_mut().and_then(|link| link.pairing.as_mut()) {
					Some(pairing) => pairing.on_pdu(&payload),
					None => return,
				};
				self.run_smp(at, steps);
			}
			ATT_CID => self.on_att(at, &payload),
			// A channel this slice does not open carries nothing it reads.
			_ => {}
		}
	}

	fn on_att(&mut self, at: usize, pdu: &[u8]) {
		// A NOTIFICATION IS NOT AN ANSWER. It arrives whenever the peer has a report, including in the
		// middle of discovery, and feeding it to the discovery as the answer to its last request would
		// end the procedure with a refusal it did not earn.
		if pdu.first() == Some(&service_logic::att::op::HANDLE_VALUE_NOTIFICATION) {
			self.on_notification(at, pdu);
			return;
		}
		let next = {
			let Some(discovery) = self.controllers[at].link.as_mut().and_then(|link| link.discovery.as_mut()) else { return };
			discovery.on_answer(pdu)
		};
		self.run_gatt(at, next);
	}

	fn start_discovery(&mut self, at: usize) {
		let Some(link) = self.controllers[at].link.as_mut() else { return };
		if link.discovery.is_some() || !link.encrypted {
			return;
		}
		let (discovery, first) = Discovery::start(service_logic::att::DEFAULT_MTU);
		link.discovery = Some(discovery);
		self.run_gatt(at, first);
	}

	fn run_gatt(&mut self, at: usize, next: Next) {
		let controller = &mut self.controllers[at];
		match next {
			Next::Request(pdu) => {
				controller.l2cap(ATT_CID, &pdu);
			}
			Next::Command(pdu, then) => {
				controller.l2cap(ATT_CID, &pdu);
				self.run_gatt(at, *then);
			}
			Next::Ready { report } => {
				if let Some(link) = controller.link.as_mut() {
					link.report = Some(report);
				}
			}
			// THE PEER IS NOT A BOOT MOUSE THIS SLICE CAN DRIVE, and saying so is the whole answer: the
			// bond stands, the link stays encrypted, and nothing is published as input.
			Next::Unsupported(_) => print(b"BluetoothService: the peer is not a boot mouse this service can drive\n"),
			Next::Failed(_) | Next::ServerError(_) => print(b"BluetoothService: the peer's attribute table could not be walked\n"),
		}
	}

	fn on_notification(&mut self, at: usize, pdu: &[u8]) {
		if pdu.len() < 3 {
			return;
		}
		let Some(link) = self.controllers[at].link.as_mut() else { return };
		let handle = u16::from_le_bytes([pdu[1], pdu[2]]);
		if link.report != Some(handle) || !link.encrypted {
			return;
		}
		let Ok(decoded) = service_logic::hogp::report(&pdu[3..]) else { return };
		link.buttons = decoded.buttons;
		let report = MouseReport { dx: decoded.dx, dy: decoded.dy, wheel: decoded.wheel, buttons: decoded.buttons };
		link.streams.retain(|&stream| {
			let kept = write_report(stream, &report);
			if !kept {
				close(stream);
			}
			kept
		});
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
		}
		soonest
	}

	fn run_timers(&mut self) {
		let now = clock();
		for controller in &mut self.controllers {
			if let Some(scan) = controller.scan.as_mut()
				&& scan.running
				&& now >= scan.deadline
			{
				scan.running = false;
				controller.command(opcode::LE_SET_SCAN_ENABLE, &hci_codec::scan_enable(false));
			}
			if let Some(attempt) = controller.attempt.as_mut()
				&& matches!(attempt.state, PairingState::Connecting | PairingState::Pairing)
				&& now >= attempt.deadline
			{
				// SIXTY SECONDS, WHATEVER THE PEER DOES. A connection that never completes is cancelled
				// and a link that stalled mid-exchange is disconnected; either way the attempt is over.
				attempt.state = PairingState::Failed;
				match controller.link.as_ref() {
					Some(link) => {
						let handle = link.handle;
						controller.command(opcode::DISCONNECT, &hci_codec::disconnect(handle, REASON_USER));
					}
					None => {
						controller.command(opcode::LE_CREATE_CONNECTION_CANCEL, &[]);
					}
				}
			}
			if let Some(link) = controller.link.as_mut()
				&& link.reassembly.expire(now)
			{
				print(b"BluetoothService: an incomplete packet on a link was given up at its deadline\n");
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
		Ok(self.stack.controllers.iter().map(|controller| ControllerInfo { address: local_wire(controller), powered: controller.powered, secure_connections: controller.secure_connections, epoch: controller.session.epoch() }).collect())
	}

	fn scan(&mut self, at: u32, deadline_ms: u32) -> Result<ScanHandle, Error> {
		let id = self.stack.next_scan;
		let controller = self.stack.controllers.get_mut(at as usize).ok_or(Error::NotFound)?;
		if !controller.powered {
			return Err(Error::Closed);
		}
		if controller.scan.as_ref().is_some_and(|scan| scan.running) {
			return Err(Error::Again);
		}
		// THE CALLER'S DEADLINE IS CAPPED, NOT BELIEVED. A scan is a radio running, and a client asking
		// for an hour of it is a client deciding how the machine behaves.
		let ticks = (deadline_ms.min(MAX_SCAN_MS) as u64).saturating_mul(TICKS_PER_SECOND) / 1000;
		if !controller.command(opcode::LE_SET_SCAN_PARAMETERS, &hci_codec::scan_parameters()) || !controller.command(opcode::LE_SET_SCAN_ENABLE, &hci_codec::scan_enable(true)) {
			return Err(Error::Exhausted);
		}
		controller.scan = Some(Scan { id, deadline: clock().saturating_add(ticks.max(1)), running: true, results: Vec::new() });
		self.stack.next_scan = self.stack.next_scan.wrapping_add(1);
		Ok(ScanHandle { id })
	}

	fn results(&mut self, scan: ScanHandle) -> Result<Vec<ScanResult>, Error> {
		self.stack.controllers.iter().find_map(|controller| controller.scan.as_ref().filter(|found| found.id == scan.id)).map(|found| found.results.clone()).ok_or(Error::NotFound)
	}

	fn scanning(&mut self, scan: ScanHandle) -> Result<bool, Error> {
		self.stack.controllers.iter().find_map(|controller| controller.scan.as_ref().filter(|found| found.id == scan.id)).map(|found| found.running).ok_or(Error::NotFound)
	}

	fn cancel(&mut self, scan: ScanHandle) -> Result<(), Error> {
		for controller in &mut self.stack.controllers {
			if let Some(found) = controller.scan.as_mut().filter(|found| found.id == scan.id) {
				if found.running {
					found.running = false;
					controller.command(opcode::LE_SET_SCAN_ENABLE, &hci_codec::scan_enable(false));
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
		if let Some(link) = controller.link.as_ref() {
			let handle = link.handle;
			controller.command(opcode::DISCONNECT, &hci_codec::disconnect(handle, REASON_USER));
		}
		if controller.scan.as_ref().is_some_and(|scan| scan.running) {
			controller.command(opcode::LE_SET_SCAN_ENABLE, &hci_codec::scan_enable(false));
		}
		controller.pump();
		controller.end_session();
		controller.powered = false;
		controller.reconnect = None;
		Ok(())
	}

	fn pair(&mut self, at: u32, peer: PeerAddress) -> Result<(), Error> {
		let peer = peer_from_wire(&peer).ok_or(Error::Invalid)?;
		let controller = self.stack.controllers.get_mut(at as usize).ok_or(Error::NotFound)?;
		if !controller.powered {
			return Err(Error::Closed);
		}
		if !controller.secure_connections || controller.public_key.is_none() {
			return Err(Error::Unsupported);
		}
		// ONE ATTEMPT PER CONTROLLER IS LIVE, and a second is refused rather than queued - a queued
		// pairing is one an operator has stopped watching.
		if controller.attempt.as_ref().is_some_and(|attempt| matches!(attempt.state, PairingState::Connecting | PairingState::Pairing)) || controller.link.is_some() {
			return Err(Error::Again);
		}
		let mut address = [0u8; 6];
		address.copy_from_slice(&peer[1..]);
		if !controller.command(opcode::LE_CREATE_CONNECTION, &hci_codec::create_connection(peer[0], &address)) {
			return Err(Error::Exhausted);
		}
		controller.attempt = Some(Attempt { peer, deadline: clock().saturating_add(PAIRING_TICKS), state: PairingState::Connecting, security: SecurityLevel::None });
		Ok(())
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
		match controller.link.as_mut() {
			Some(link) => {
				link.pairing = None;
				let handle = link.handle;
				controller.command(opcode::DISCONNECT, &hci_codec::disconnect(handle, REASON_USER));
			}
			None => {
				controller.command(opcode::LE_CREATE_CONNECTION_CANCEL, &[]);
			}
		}
		Ok(())
	}

	fn bonded(&mut self, at: u32) -> Result<Vec<BondedPeer>, Error> {
		let controller = self.stack.controllers.get(at as usize).ok_or(Error::NotFound)?;
		if self.stack.bonds == 0 {
			return Err(Error::Closed);
		}
		let records = self.stack.bonds().list(&local_wire(controller)).ok_or(Error::Closed)??;
		Ok(records.into_iter().map(|record| BondedPeer { address: record.peer, name: record.name, enabled: record.enabled, security: record.security }).collect())
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
		if let Some(link) = controller.link.as_ref().filter(|link| link.peer == peer) {
			let handle = link.handle;
			controller.command(opcode::DISCONNECT, &hci_codec::disconnect(handle, REASON_USER));
		}
		Ok(())
	}

	fn enable(&mut self, at: u32, peer: PeerAddress, on: bool) -> Result<(), Error> {
		let peer = peer_from_wire(&peer).ok_or(Error::Invalid)?;
		if at as usize >= self.stack.controllers.len() {
			return Err(Error::NotFound);
		}
		let Some(mut record) = self.stack.bond(at as usize, &peer) else { return Err(Error::NotFound) };
		record.enabled = on;
		let stored = matches!(self.stack.bonds().store(&record), Some(Ok(())));
		record.key.fill(0);
		if !stored {
			return Err(Error::Io);
		}
		let controller = &mut self.stack.controllers[at as usize];
		if on {
			// An enabled peer that is already connected and encrypted becomes an input source now.
			if controller.link.as_ref().is_some_and(|link| link.peer == peer && link.encrypted) {
				self.stack.start_discovery(at as usize);
			}
		} else if let Some(link) = controller.link.as_mut().filter(|link| link.peer == peer) {
			// DISABLING CLOSES WHATEVER THE INPUT SERVICE HOLDS FOR IT, releasing held buttons first.
			release_streams(link);
			link.report = None;
			link.discovery = None;
		}
		Ok(())
	}
}

// ------------------------------------------------------------------ the profile interface

struct ProfileView<'a> {
	stack: &'a mut Stack,
	// Which controller the accepted stream is for, set by `open_mouse` for the caller to attach the
	// stream's producer to.
	target: Option<usize>,
}

impl bluetooth_profile::Service for ProfileView<'_> {
	fn open_mouse(&mut self, at: u32, peer: PeerAddress) -> Result<Vec<MouseReport>, Error> {
		let peer_address = peer_from_wire(&peer).ok_or(Error::Invalid)?;
		if at as usize >= self.stack.controllers.len() {
			return Err(Error::NotFound);
		}
		// ONLY OVER A PEER AN OPERATOR ENABLED. The input service cannot reach a peer that is merely
		// bonded, and a peer an operator has not approved is not an input source whatever it sends.
		let Some(record) = self.stack.bond(at as usize, &peer_address) else { return Err(Error::NotFound) };
		if !record.enabled {
			return Err(Error::Denied);
		}
		let controller = &self.stack.controllers[at as usize];
		match controller.link.as_ref() {
			Some(link) if link.peer == peer_address && link.streams.len() >= MAX_STREAMS => return Err(Error::Exhausted),
			Some(link) if link.peer == peer_address => {}
			// The peer is bonded and enabled and not connected right now: the stream is refused with
			// `closed`, which is the same answer a lost link gives, so a consumer retries the same way.
			_ => return Err(Error::Closed),
		}
		self.target = Some(at as usize);
		Ok(Vec::new())
	}

	// The peers `open-mouse` would accept: bonded AND enabled, on every controller this service has.
	fn enabled(&mut self) -> Result<Vec<EnabledPeer>, Error> {
		if self.stack.bonds == 0 {
			return Err(Error::Closed);
		}
		let mut out = Vec::new();
		for (at, controller) in self.stack.controllers.iter().enumerate() {
			let Some(Ok(records)) = self.stack.bonds().list(&local_wire(controller)) else { continue };
			out.extend(records.into_iter().filter(|record| record.enabled).map(|record| EnabledPeer { controller: at as u32, peer: record.peer }));
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
		let mut controller = Controller { info, transport, packets, control, limits, credits: Credits::new(attachment.command_credits.clamp(1, 1), attachment.acl_credits, attachment.acl_queue.min(MAX_QUEUED as u32)), session: Session::new(attachment.epoch), address: [0; 6], powered: false, secure_connections: false, init: Init::Reset, pending: Vec::new(), outstanding: None, public_key: None, scan: None, attempt: None, link: None, reconnect: None };
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
			let Some(packet) = hci_transport::receive_read(&buf[..len], &mut frame_handles) else { continue };
			if !self.controllers[at].session.admits(packet.epoch) {
				continue;
			}
			let kind = match packet.kind {
				HciPacketKind::Event => Kind::Event,
				HciPacketKind::Acl => Kind::Acl,
				_ => continue,
			};
			if service_logic::hci::check_inbound(&self.controllers[at].limits, kind as u16, packet.bytes.len() as u32).is_err() {
				continue;
			}
			match kind {
				Kind::Event => self.on_event(at, &packet.bytes),
				Kind::Acl => self.on_acl(at, &packet.bytes),
				_ => {}
			}
			self.controllers[at].pump();
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
					let controller = &mut self.controllers[at];
					controller.end_session();
					controller.session.advance();
					controller.start_init();
					controller.pump();
				}
				HciControlKind::Fault => {
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
	let (catalogue, bonds, read_root, operator_root, profile_root) = (roles[0], roles[1], roles[2], roles[3], roles[4]);

	// 2. subscribe to the controllers this machine publishes. A machine with none has none, which is
	//    a subscription that stays quiet rather than a failure.
	let subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::BluetoothHci).unwrap_or(0) } else { 0 };
	let mut stack = Stack { controllers: Vec::new(), bonds, next_scan: 1 };
	send_blocking(bootstrap, b"BluetoothService: online", 0);

	let mut clients: Vec<Client> = Vec::new();
	let mut buf = alloc::vec![0u8; 8192];
	let mut reply = alloc::vec![0u8; 8192];
	let mut subscribed = subscription != 0;
	loop {
		let mut waitset: Vec<u64> = Vec::with_capacity(8 + clients.len() + 2 * MAX_CONTROLLERS);
		for root in [read_root, operator_root, profile_root] {
			if root != 0 {
				waitset.push(root);
			}
		}
		if subscribed {
			waitset.push(subscription);
		}
		for controller in &stack.controllers {
			waitset.push(controller.packets);
			waitset.push(controller.control);
		}
		waitset.extend(clients.iter().map(|client| client.chan));
		let ready = wait_any(&waitset, stack.next_deadline());
		stack.run_timers();
		for controller in &mut stack.controllers {
			controller.pump();
		}
		if ready < 0 {
			continue;
		}
		let handle = waitset[ready as usize];

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
			if interface == Interface::Profile && op == bluetooth_profile::OP_OPEN_MOUSE {
				serve_open_mouse(&mut stack, handle, &buf[..len], &mut handles, &mut reply);
				continue;
			}
		}
		let mut reply_handles = wire::Handles::new();
		let written = match interface {
			Interface::Read => bluetooth::dispatch(&mut ReadView { stack: &mut stack }, &buf[..len], &mut handles, &mut reply, &mut reply_handles),
			Interface::Operator => bluetooth_operator::dispatch(&mut OperatorView { stack: &mut stack }, &buf[..len], &mut handles, &mut reply, &mut reply_handles),
			Interface::Profile => bluetooth_profile::dispatch(&mut ProfileView { stack: &mut stack, target: None }, &buf[..len], &mut handles, &mut reply, &mut reply_handles),
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

// `open-mouse` is a stream: validated by the service trait, then answered with the CONSUMER end of a
// fresh pair whose producer is kept on the link - which is where reports are written as they arrive.
fn serve_open_mouse(stack: &mut Stack, channel: u64, request: &[u8], handles: &mut wire::Handles, reply: &mut [u8]) {
	let mut view = ProfileView { stack, target: None };
	let Some((corr, result)) = bluetooth_profile::open_mouse_open(&mut view, request, handles) else { return };
	let target = view.target;
	let answer = match (result, target) {
		(Ok(_), Some(at)) => match channel_with_depth(64) {
			Some((producer, consumer)) => match stack.controllers[at].link.as_mut() {
				Some(link) => {
					link.streams.push(producer);
					if let Some(len) = bluetooth_profile::open_mouse_reply_ok(corr, reply)
						&& send_caps_blocking(channel, &reply[..len], &[consumer])
					{
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
	if let Some(len) = bluetooth_profile::open_mouse_reply_err(corr, &answer, reply) {
		send_blocking(channel, &reply[..len], 0);
	}
}
