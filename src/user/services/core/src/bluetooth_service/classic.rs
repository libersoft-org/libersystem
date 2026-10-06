// THE BR/EDR HALF: inquiry and paging, Secure Simple Pairing's host answers and the prompts they raise,
// link keys and their levels, the inbound policy, and above ACL the L2CAP channels, SDP on both sides and
// RFCOMM.
//
// WHAT THE CONTROLLER DOES AND WHAT THIS HOST DOES. The controller runs the key agreement over LMP. This
// host answers its questions - the IO capability, a person's confirmation of six digits, a PIN, a stored
// link key - stores the link key the controller hands back, and turns encryption on. Every answer that needs
// a person is a PROMPT on the operator authority's watcher stream, with the protocol's 25-second deadline,
// after which this host answers no itself.
//
// THE OTHER RADIO'S KEY. A Secure Connections link key on a link this host is the central of is the source of an LE
// key for the same device: once the link is encrypted, the peer's fixed channels are asked for, and where it has the
// BR/EDR Security Manager the LTK is derived over it (`smp_pairing::Initiator::over_bredr`) and kept as the device's
// LE bond at the link key's own level. A peer that is the central asks this host instead, and `BredrResponder`
// answers. Nothing weaker than a P-256 key is ever a source.
//
// THE LEVELS ARE NEVER DOWNGRADED. A bonded peer that pairs again lower on either axis - a P-256 bond offered
// P-192, an authenticated one offered Just Works, an SSP one offered a PIN - is refused and keeps its bond.

use super::*;
use service_logic::bt_pairing::{self, Prompt, Question, Reply};
use service_logic::bt_policy::{self, Decision, Inbound};
use service_logic::hci_bredr::{self, Event as Bredr, IoCapability, KeyType};
use service_logic::hfp;
use service_logic::l2cap_bredr::{self, Channels, Ertm, ErtmOut, Mode, Out as L2Out, Signal, psm};
use service_logic::rfcomm::{self, Out as RfOut, Session};
use service_logic::sdp::{self, Answer, Search, Server, Uuid};
use service_logic::smp_pairing::{self, BredrResponder};

// How long a link may be idle before this host asks for sniff mode: an idle keyboard should not hold the
// radio awake, and two seconds is longer than any burst a person types.
const SNIFF_IDLE_TICKS: u64 = 2 * TICKS_PER_SECOND;
// This host's name on the radio.
const LOCAL_NAME: &[u8] = b"LiberSystem";
// Names remembered from inquiry and remote name requests.
const MAX_NAMES: usize = 32;
// Data a DLC may hold for a consumer that has not taken it, before the oldest is dropped: room for what a paused serial
// port's peer may still send on the credits it holds, twice over, so a grant's reader that falls behind loses nothing.
const MAX_HELD_BYTES: usize = 16 * 1024;
// The reason a pairing a person refused or the policy forbids ends with: pairing not allowed.
const REASON_PAIRING_NOT_ALLOWED: u8 = 0x18;
// The reason a connection is rejected with: unacceptable device address, and limited resources.
const REJECT_UNACCEPTABLE: u8 = 0x0F;
const REJECT_RESOURCES: u8 = 0x0D;
// The page timeout: 5.12 seconds, in 0.625 ms slots.
const PAGE_TIMEOUT_SLOTS: u16 = 0x2000;

// --------------------------------------------------------------------------------- state

// A prompt in flight: whom it is about, and the question with its clock.
pub(crate) struct Pending {
	pub id: u32,
	pub peer: Peer,
	pub prompt: Prompt,
	pub name: String,
}

// What a profile connection waits for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Want {
	// The link, then encryption, then the profile's own steps.
	Profile(Profile),
	// The link, then encryption, then an Object Push's search.
	Push,
}

// What an SDP search this host runs is for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Purpose {
	Profile(Profile),
	Push,
}

pub(crate) struct ControllerState {
	// The prompt watcher's producer end, zero when none is attached.
	pub watcher: u64,
	pub prompt: Option<Pending>,
	pub next_prompt: u32,
	// When `btctl discoverable` ends, in ticks.
	pub discoverable_until: Option<u64>,
	// The scan mode last written, so it is written only when it changes.
	pub written_scan: Option<u8>,
	// The peer the operator asked a legacy PIN pairing for.
	pub legacy: Option<Peer>,
	// The peer being paged: one page at a time.
	pub paging: Option<Peer>,
	// The records this host's SDP server offers.
	pub sdp: Server,
	pub names: Vec<(Peer, String)>,
	// Classes of device, from inquiry and from pages: what says a boot device is a keyboard or a pointer.
	pub classes: Vec<(Peer, u32)>,
	// The peer a waiting `btctl receive` names.
	pub receive_waits: Option<Peer>,
	// Profile connections the operator asked for, waiting on their link.
	pub wants: Vec<(Peer, Want)>,
	pub transaction: u16,
}

impl ControllerState {
	pub fn new() -> ControllerState {
		// THE ROLES THIS SYSTEM OFFERS, each with its record: A2DP's source (a player) and sink (headphones), the remote
		// control's target - claiming no player category, there being no media session - and controller, the voice
		// gateway a headset connects to, by the hands-free and the headset profile, and the Object Push server a
		// receiver's peer pushes to - on RFCOMM; the policy admits it only while a receiver waits.
		let mut sdp = Server::new();
		sdp.offer(sdp::a2dp(0x0001_0001, false, 0x0001));
		sdp.offer(sdp::a2dp(0x0001_0002, true, 0x0001));
		sdp.offer(sdp::avrcp(0x0001_0003, true, 0x0000));
		sdp.offer(sdp::avrcp(0x0001_0004, false, 0x0001));
		sdp.offer(sdp::hfp_audio_gateway(0x0001_0005, bt_policy::HFP_CHANNEL, hfp::SDP_FEATURES));
		sdp.offer(sdp::hsp_audio_gateway(0x0001_0006, bt_policy::HSP_CHANNEL));
		sdp.offer(sdp::opp_server(0x0001_0007, bt_policy::OPP_CHANNEL, None));
		ControllerState { watcher: 0, prompt: None, next_prompt: 1, discoverable_until: None, written_scan: None, legacy: None, paging: None, sdp, names: Vec::new(), classes: Vec::new(), receive_waits: None, wants: Vec::new(), transaction: 1 }
	}

	// A session's end takes the page, the prompt and the scan mode with it; the watcher, the records and the
	// names outlive it.
	pub fn end_session(&mut self) {
		self.prompt = None;
		self.paging = None;
		self.written_scan = None;
		self.legacy = None;
		self.wants.clear();
	}

	pub fn detach_watcher(&mut self) {
		if self.watcher != 0 {
			close(self.watcher);
			self.watcher = 0;
		}
	}

	pub fn scan_flags(&self) -> (bool, bool) {
		let mode = self.written_scan.unwrap_or(0);
		(mode & 2 != 0, mode & 1 != 0)
	}

	pub fn name_of(&self, peer: &Peer) -> Option<String> {
		self.names.iter().find(|(held, _)| held == peer).map(|(_, name)| name.clone())
	}

	fn remember_class(&mut self, peer: Peer, class: u32) {
		self.classes.retain(|(held, _)| *held != peer);
		if self.classes.len() >= MAX_NAMES {
			self.classes.remove(0);
		}
		self.classes.push((peer, class));
	}

	// A peripheral whose minor class says pointing device, and not keyboard: a boot mouse.
	fn is_pointer(&self, peer: &Peer) -> bool {
		self.classes.iter().find(|(held, _)| held == peer).is_some_and(|(_, class)| (class >> 8) & 0x1F == 0x05 && (class >> 6) & 0x03 == 0b10)
	}

	fn remember_name(&mut self, peer: Peer, name: &[u8]) {
		if name.is_empty() {
			return;
		}
		let name = String::from(core::str::from_utf8(&name[..name.len().min(48)]).unwrap_or(""));
		if let Some(held) = self.names.iter_mut().find(|(held, _)| *held == peer) {
			held.1 = name;
			return;
		}
		if self.names.len() >= MAX_NAMES {
			self.names.remove(0);
		}
		self.names.push((peer, name));
	}

	pub fn next_deadline(&self, links: &[Link]) -> Option<u64> {
		let mut soonest: Option<u64> = None;
		let mut take = |at: u64| soonest = Some(soonest.map_or(at, |held| held.min(at)));
		if let Some(pending) = self.prompt.as_ref() {
			take(pending.prompt.deadline() / 10);
		}
		if let Some(until) = self.discoverable_until {
			take(until);
		}
		for link in links {
			if let Some(classic) = link.classic.as_ref() {
				if !classic.sniff && classic.sniff_allowed() {
					take(classic.last_activity + SNIFF_IDLE_TICKS);
				}
				for (_, ertm) in &classic.ertm {
					if let Some(due) = ertm.deadline() {
						take(due / 10);
					}
				}
			}
		}
		soonest
	}
}

// An SDP search this host runs as a client, and what it is for.
pub(crate) struct SdpClient {
	pub cid: u16,
	pub search: Option<Search>,
	pub uuid: u16,
	pub purpose: Purpose,
}

// THE HID HOST'S CHANNELS on a link - control and interrupt, by local channel id - and whether the device's report
// descriptor has been asked for.
#[derive(Default)]
pub(crate) struct Hid {
	pub control: Option<u16>,
	pub interrupt: Option<u16>,
	pub searched: bool,
	// This host opens the channels itself, once the descriptor is known: the operator's `connect`.
	pub outgoing: bool,
}

// The HID protocol's message types, as the first byte's high nibble.
const HIDP_HANDSHAKE: u8 = 0x00;
const HIDP_SET_PROTOCOL_BOOT: u8 = 0x70;
const HIDP_DATA_INPUT: u8 = 0xA1;
// The HID descriptor list's attribute, and a report descriptor's class in it.
const HID_DESCRIPTOR_LIST: u16 = 0x0206;
const REPORT_DESCRIPTOR: u128 = 0x22;

// The one RFCOMM session on a link, over its L2CAP channel, and the DLCs it carries by server channel.
pub(crate) struct Rfcomm {
	pub cid: u16,
	pub session: Session,
	// DLCs this side asked for: a profile's, or, with none, an Object Push session's.
	pub opening: Vec<(u8, Option<Profile>)>,
	pub open: Vec<(u8, Profile)>,
	// What arrived on each DLC and nobody has taken yet: bounded, the oldest dropped.
	pub held: Vec<(u8, VecDeque<u8>)>,
}

pub(crate) struct ClassicLink {
	pub channels: Channels,
	pub ertm: Vec<(u16, Ertm)>,
	// Inbound connection requests waiting for the link's encryption.
	pub held: Vec<Signal>,
	pub sdp_client: Option<SdpClient>,
	pub rfcomm: Option<Rfcomm>,
	pub hid: Hid,
	pub ours: IoCapability,
	pub theirs: Option<IoCapability>,
	// A pairing, as opposed to an authentication with a stored key, is under way.
	pub pairing: bool,
	// This host refused the key the link was paired with: it is being disconnected, and nothing opens on it.
	pub refused: bool,
	pub authenticating: bool,
	pub level: Option<Level>,
	pub features: Option<[u8; 8]>,
	pub last_activity: u64,
	pub sniff: bool,
	// The profiles connected over it: bt_policy's trust bits.
	pub profiles: bt_policy::Trust,
	// A2DP's stream and AVRCP's remote control on the link.
	pub a2dp: super::a2dp::A2dp,
	// This host is the link's central - it paged, or the peer let it take the role - which is what runs the
	// cross-transport derivation from this side.
	pub central: bool,
	// A Secure Connections link key this link's pairing just made, as HCI carries it, and whether it is authenticated:
	// the source of the LE key, held until the peer's fixed channels say whether it can be derived.
	pub derive: Option<([u8; 16], bool)>,
	// The derivation a central peer started, answered from this side.
	pub responder: Option<BredrResponder>,
	// The voice gateway a headset opened on the link, and its synchronous link.
	pub voice: Option<super::voice::VoiceLink>,
	// The PAN link this host is a user of, over BNEP.
	pub pan: Option<super::pan::Pan>,
}

impl ClassicLink {
	fn new(central: bool) -> ClassicLink {
		ClassicLink { channels: Channels::new(), ertm: Vec::new(), held: Vec::new(), sdp_client: None, rfcomm: None, hid: Hid::default(), ours: IoCapability::NoInputNoOutput, theirs: None, pairing: false, refused: false, authenticating: false, level: None, features: None, last_activity: clock(), sniff: false, profiles: bt_policy::Trust::default(), a2dp: super::a2dp::A2dp::default(), central, derive: None, responder: None, voice: None, pan: None }
	}

	// The source key goes the moment it is no longer wanted.
	fn drop_derive(&mut self) {
		if let Some((key, _)) = self.derive.as_mut() {
			scrub(key);
		}
		self.derive = None;
	}

	// Sniff only where the peer has it, and never while a pairing or a channel's setup is under way.
	fn sniff_allowed(&self) -> bool {
		self.features.is_some_and(|features| hci_bredr::feature::sniff(&features)) && !self.pairing && !self.authenticating && self.sdp_client.is_none()
	}
}

// The profiles a link carries now, for `devices`.
pub(crate) fn connected_profiles(link: &Link) -> Vec<Profile> {
	let Some(classic) = link.classic.as_ref() else {
		return if link.report.is_some() || link.decoder.is_some() { alloc::vec![Profile::Input] } else { Vec::new() };
	};
	[Profile::Input, Profile::Audio, Profile::Voice, Profile::Pan, Profile::Spp, Profile::Gatt].into_iter().filter(|profile| classic.profiles.has(profile_to_logic(*profile))).collect()
}

// ------------------------------------------------------------------ initialisation

// The BR/EDR initialisation steps, after the LE ones: Secure Simple Pairing and Secure Connections on, this
// host's class and name, extended inquiry results, role switch and sniff allowed on every link, and the page
// timeout. The scan mode is written by the policy once the controller is ready.
pub(crate) fn init_steps() -> Vec<InitStep> {
	let step = |op: u16, params: Vec<u8>, tolerant: bool| InitStep { op, params, tolerant, classic: true };
	alloc::vec![
		step(hci_bredr::opcode::WRITE_SIMPLE_PAIRING_MODE, alloc::vec![1], false),
		step(hci_bredr::opcode::WRITE_SECURE_CONNECTIONS_HOST_SUPPORT, alloc::vec![1], true),
		step(hci_bredr::opcode::WRITE_CLASS_OF_DEVICE, hci_bredr::class_of_device(hci_bredr::CLASS_OF_DEVICE), true),
		step(hci_bredr::opcode::WRITE_LOCAL_NAME, hci_bredr::local_name(LOCAL_NAME), true),
		step(hci_bredr::opcode::WRITE_INQUIRY_MODE, alloc::vec![2], true),
		step(hci_bredr::opcode::WRITE_DEFAULT_LINK_POLICY_SETTINGS, hci_bredr::LINK_POLICY.to_le_bytes().to_vec(), true),
		step(hci_bredr::opcode::WRITE_PAGE_TIMEOUT, PAGE_TIMEOUT_SLOTS.to_le_bytes().to_vec(), true),
	]
}

pub(crate) fn on_init_complete(controller: &mut Controller, op: u16, params: &[u8]) {
	match op {
		// LMP features page 0: byte 4 bit 5 says BR/EDR is NOT supported, byte 6 bit 3 that Secure Simple
		// Pairing is. A controller without SSP is one this host does not drive on BR/EDR.
		hci_bredr::opcode::READ_LOCAL_SUPPORTED_FEATURES if params.len() >= 9 => {
			let features = &params[1..9];
			controller.classic = features[4] & (1 << 5) == 0 && features[6] & (1 << 3) != 0;
		}
		// The BR/EDR data buffers: their size and how many.
		hci_bredr::opcode::READ_BUFFER_SIZE if params.len() >= 8 => {
			let size = u16::from_le_bytes([params[1], params[2]]) as u32;
			let count = u16::from_le_bytes([params[4], params[5]]) as u32;
			if count > 0 && size > 0 {
				controller.bredr = Credits::new(1, count, MAX_QUEUED as u32);
				controller.bredr_bytes = size;
			}
		}
		hci_bredr::opcode::WRITE_SECURE_CONNECTIONS_HOST_SUPPORT => controller.classic_sc = true,
		_ => {}
	}
}

// ------------------------------------------------------------------ address forms

pub(crate) fn bredr(wire: &[u8; 6]) -> Peer {
	let mut peer = [0u8; 7];
	peer[0] = KIND_BREDR;
	peer[1..].copy_from_slice(&hci_codec::address_from_wire(wire));
	peer
}

fn wire_of(peer: &Peer) -> [u8; 6] {
	let mut address = [0u8; 6];
	address.copy_from_slice(&peer[1..]);
	hci_codec::address_to_wire(&address)
}

fn question_wire(question: &Question) -> (proto::system::PromptQuestion, u32) {
	use proto::system::PromptQuestion as Q;
	match *question {
		Question::Compare(value) => (Q::Compare, value),
		Question::ShowPasskey(value) => (Q::ShowPasskey, value),
		Question::EnterPasskey => (Q::EnterPasskey, 0),
		Question::EnterPin => (Q::EnterPin, 0),
		Question::Consent => (Q::Consent, 0),
	}
}

fn now_ms() -> u64 {
	clock() * (1000 / TICKS_PER_SECOND)
}

// ------------------------------------------------------------------ events

impl Stack {
	pub(crate) fn on_classic_event(&mut self, at: usize, event: Bredr) {
		match event {
			Bredr::InquiryComplete { .. } => {
				if let Some(scan) = self.controllers[at].scan.as_mut() {
					scan.inquiring = false;
				}
			}
			Bredr::Inquiry(found) => self.on_inquiry(at, found),
			Bredr::ConnectionComplete { status, handle, address, acl, .. } => self.on_classic_connected(at, status, handle, &address, acl),
			Bredr::ConnectionRequest { address, class, link } => {
				self.controllers[at].bredr_state.remember_class(bredr(&address), class);
				self.on_connection_request(at, &address, link);
			}
			Bredr::AuthenticationComplete { status, handle } => self.on_authenticated(at, status, handle),
			Bredr::RemoteName { status, address, name } => {
				if status == 0 {
					self.controllers[at].bredr_state.remember_name(bredr(&address), &name);
				}
			}
			Bredr::RemoteFeatures { status, handle, features } => {
				if status == 0
					&& let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut())
				{
					classic.features = Some(features);
				}
			}
			Bredr::ModeChange { handle, mode, .. } => {
				if let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) {
					// Mode 2 is sniff; 0 is active.
					classic.sniff = mode == 2;
				}
			}
			Bredr::RoleChange { status: 0, address, central } => {
				if let Some(classic) = self.controllers[at].link_to_mut(&bredr(&address)).and_then(|link| link.classic.as_mut()) {
					classic.central = central;
				}
			}
			Bredr::SynchronousComplete { status, handle, address, .. } => self.voice_connected(at, status, handle, &address),
			Bredr::RoleChange { .. } | Bredr::EncryptionKeyRefresh { .. } | Bredr::RemoteExtendedFeatures { .. } | Bredr::EncryptionChange { .. } => {}
			Bredr::LinkKeyRequest { address } => self.on_link_key_request(at, &address),
			Bredr::LinkKeyNotification { address, mut key, kind } => {
				self.on_link_key(at, &address, &key, kind);
				scrub(&mut key);
			}
			Bredr::PinCodeRequest { address } => self.on_pin_request(at, &address),
			Bredr::IoCapabilityRequest { address } => self.on_io_capability_request(at, &address),
			Bredr::IoCapabilityResponse { address, capability, .. } => {
				let peer = bredr(&address);
				if let Some(classic) = self.controllers[at].link_to_mut(&peer).and_then(|link| link.classic.as_mut()) {
					classic.theirs = IoCapability::from_value(capability);
					classic.pairing = true;
				}
			}
			Bredr::UserConfirmationRequest { address, value } => self.on_confirmation(at, &address, value),
			Bredr::UserPasskeyRequest { address } => {
				// THIS HOST NEVER TYPES A PASSKEY ON BR/EDR: it declares DisplayYesNo, which has no keyboard.
				self.controllers[at].command(hci_bredr::opcode::USER_PASSKEY_REQUEST_NEGATIVE_REPLY, &hci_bredr::address_only(&address));
			}
			Bredr::UserPasskeyNotification { address, passkey } => {
				let peer = bredr(&address);
				self.raise(at, peer, Question::ShowPasskey(passkey % 1_000_000));
			}
			Bredr::KeypressNotification { address, .. } => {
				let peer = bredr(&address);
				let state = &mut self.controllers[at].bredr_state;
				if let Some(pending) = state.prompt.as_mut().filter(|pending| pending.peer == peer) {
					pending.prompt.keypresses = pending.prompt.keypresses.saturating_add(1);
					self.send_prompt(at);
				}
			}
			Bredr::SimplePairingComplete { status, address } => {
				// THE PAIRING IS OVER, whichever way: a prompt still showing - a passkey being typed - goes with it.
				let peer = bredr(&address);
				let controller = &mut self.controllers[at];
				if controller.bredr_state.prompt.as_ref().is_some_and(|pending| pending.peer == peer) {
					controller.bredr_state.prompt = None;
				}
				if status != 0 {
					controller.fail_attempt(&peer);
				}
			}
		}
	}

	fn on_inquiry(&mut self, at: usize, found: Vec<hci_bredr::Found>) {
		let controller = &mut self.controllers[at];
		for device in found {
			let peer = bredr(&device.address);
			controller.bredr_state.remember_name(peer, &device.eir.name);
			controller.bredr_state.remember_class(peer, device.class);
			let Some(scan) = controller.scan.as_mut().filter(|scan| scan.inquiring) else { continue };
			let address = peer_to_wire(&peer);
			scan.paging.retain(|(held, _, _)| *held != peer);
			scan.paging.push((peer, device.repetition, device.clock_offset));
			if scan.results.iter().any(|result| result.address == address) {
				continue;
			}
			if scan.results.len() >= MAX_SCAN_RESULTS {
				continue;
			}
			let name = core::str::from_utf8(&device.eir.name[..device.eir.name.len().min(48)]).unwrap_or("");
			let services: Vec<proto::system::ServiceClass> = device.eir.uuids.iter().take(16).map(|&uuid| proto::system::ServiceClass { uuid }).collect();
			// THE HID SERVICE CLASS or the peripheral major class is what says "human interface" on BR/EDR.
			let human_interface = device.eir.uuids.contains(&sdp::uuid::HUMAN_INTERFACE_DEVICE) || (device.class >> 8) & 0x1F == 0x05;
			scan.results.push(ScanResult { address, name: String::from(name), rssi: device.rssi.map_or(0, i32::from), human_interface, radio: Radio::Classic, class_of_device: device.class & 0x00FF_FFFF, services });
		}
	}

	pub(crate) fn pair_classic(&mut self, at: usize, peer: Peer, legacy: bool) -> Result<(), Error> {
		let controller = &mut self.controllers[at];
		if !controller.classic {
			return Err(Error::Unsupported);
		}
		if let Some(handle) = controller.link_to(&peer).map(|link| link.handle) {
			// A LINK THAT IS THERE ALREADY is authenticated anew; the stored key is refused for the operator's
			// pairing, so this is a fresh pairing at whatever level the two sides reach.
			controller.attempt = Some(Attempt { peer, deadline: clock().saturating_add(PAIRING_TICKS), state: PairingState::Pairing, security: SecurityLevel::None });
			controller.bredr_state.legacy = legacy.then_some(peer);
			if let Some(classic) = controller.link_mut(handle).and_then(|link| link.classic.as_mut()) {
				classic.authenticating = false;
			}
			self.authenticate(at, handle);
			return Ok(());
		}
		self.page(at, peer)?;
		let controller = &mut self.controllers[at];
		controller.bredr_state.legacy = legacy.then_some(peer);
		controller.attempt = Some(Attempt { peer, deadline: clock().saturating_add(PAIRING_TICKS), state: PairingState::Connecting, security: SecurityLevel::None });
		Ok(())
	}

	// Page a peer: with inquiry's repetition mode and clock offset when the current scan found it.
	pub(crate) fn page(&mut self, at: usize, peer: Peer) -> Result<(), Error> {
		let controller = &mut self.controllers[at];
		if controller.bredr_state.paging.is_some() {
			return Err(Error::Again);
		}
		let classic_links = controller.links.iter().filter(|link| link.is_classic()).count();
		if classic_links >= bt_bounds::BREDR_LINKS_PER_CONTROLLER || controller.links.len() >= bt_bounds::LINKS_PER_CONTROLLER {
			return Err(Error::Exhausted);
		}
		let (repetition, clock_offset) = controller.scan.as_ref().and_then(|scan| scan.paging.iter().find(|(held, _, _)| *held == peer)).map_or((1, 0), |(_, repetition, offset)| (*repetition, *offset));
		if !controller.command(hci_bredr::opcode::CREATE_CONNECTION, &hci_bredr::create_connection(&wire_of(&peer), repetition, clock_offset)) {
			return Err(Error::Exhausted);
		}
		controller.bredr_state.paging = Some(peer);
		Ok(())
	}

	fn on_classic_connected(&mut self, at: usize, status: u8, handle: u16, address: &[u8; 6], acl: bool) {
		let peer = bredr(address);
		let controller = &mut self.controllers[at];
		let outgoing = controller.bredr_state.paging == Some(peer);
		if outgoing {
			controller.bredr_state.paging = None;
		}
		if !acl {
			// A synchronous link is the voice path's, which this host does not set up from here.
			if status == 0 {
				controller.disconnect(handle, REASON_USER);
			}
			return;
		}
		if status != 0 {
			controller.fail_attempt(&peer);
			controller.bredr_state.wants.retain(|(held, _)| *held != peer);
			return;
		}
		let classic_links = controller.links.iter().filter(|link| link.is_classic()).count();
		if classic_links >= bt_bounds::BREDR_LINKS_PER_CONTROLLER || controller.links.len() >= bt_bounds::LINKS_PER_CONTROLLER || controller.link_to(&peer).is_some() {
			controller.disconnect(handle, REASON_USER);
			return;
		}
		let mut link = Link::new(handle, peer);
		link.local = local_address(controller);
		link.classic = Some(Box::new(ClassicLink::new(outgoing)));
		controller.links.push(link);
		controller.command(hci_bredr::opcode::WRITE_LINK_POLICY_SETTINGS, &hci_bredr::write_link_policy(handle, hci_bredr::LINK_POLICY));
		controller.command(hci_bredr::opcode::READ_REMOTE_SUPPORTED_FEATURES, &hci_bredr::handle_only(handle));
		if !controller.bredr_state.names.iter().any(|(held, _)| *held == peer) {
			controller.command(hci_bredr::opcode::REMOTE_NAME_REQUEST, &hci_bredr::remote_name_request(address, 1, 0));
		}
		let pairing = controller.attempt.as_ref().is_some_and(|attempt| attempt.peer == peer && attempt.state == PairingState::Connecting);
		if pairing && let Some(attempt) = controller.attempt.as_mut() {
			attempt.state = PairingState::Pairing;
		}
		let wanted = controller.bredr_state.wants.iter().any(|(held, _)| *held == peer);
		// SECURITY MODE 4, ENFORCED BY THIS HOST: a link this host paged for a pairing or a profile is authenticated
		// and encrypted at once. A link a peer paged is secured when the peer asks for a channel that needs it -
		// most authenticate first by themselves - and a peer that is not bonded opens nothing but SDP.
		if pairing || wanted {
			self.authenticate(at, handle);
		}
	}

	fn authenticate(&mut self, at: usize, handle: u16) {
		let controller = &mut self.controllers[at];
		let Some(classic) = controller.link_mut(handle).and_then(|link| link.classic.as_mut()) else { return };
		if classic.authenticating {
			return;
		}
		classic.authenticating = true;
		controller.command(hci_bredr::opcode::AUTHENTICATION_REQUESTED, &hci_bredr::handle_only(handle));
	}

	// AN INBOUND CONNECTION, admitted by the policy: from a bonded peer trusted for a profile served inbound,
	// from the peer a waiting receive names, or from anyone while a prompt watcher makes this host pairable.
	fn on_connection_request(&mut self, at: usize, address: &[u8; 6], link: u8) {
		let peer = bredr(address);
		if link != 1 {
			// A SYNCHRONOUS LINK THE PEER ASKS FOR is refused: the voice gateway sets its link up itself, for a session.
			self.controllers[at].command(hci_bredr::opcode::REJECT_SYNCHRONOUS_CONNECTION_REQUEST, &hci_bredr::reject_connection(address, REJECT_RESOURCES));
			return;
		}
		let trusted = self.record(at, &peer).is_some_and(|record| trust_of(&record.trusted).any_inbound());
		let controller = &mut self.controllers[at];
		let classic_links = controller.links.iter().filter(|link| link.is_classic()).count();
		if classic_links >= bt_bounds::BREDR_LINKS_PER_CONTROLLER || controller.links.len() >= bt_bounds::LINKS_PER_CONTROLLER {
			controller.command(hci_bredr::opcode::REJECT_CONNECTION_REQUEST, &hci_bredr::reject_connection(address, REJECT_RESOURCES));
			return;
		}
		let admitted = trusted || controller.bredr_state.receive_waits == Some(peer) || controller.bredr_state.watcher != 0;
		if admitted {
			// THIS HOST TAKES THE CENTRAL ROLE where the peer lets it, so one piconet holds every link it has.
			controller.command(hci_bredr::opcode::ACCEPT_CONNECTION_REQUEST, &hci_bredr::accept_connection(address, true));
		} else {
			controller.command(hci_bredr::opcode::REJECT_CONNECTION_REQUEST, &hci_bredr::reject_connection(address, REJECT_UNACCEPTABLE));
		}
	}

	fn on_authenticated(&mut self, at: usize, status: u8, handle: u16) {
		let controller = &mut self.controllers[at];
		let Some(link) = controller.link_mut(handle) else { return };
		let peer = link.peer;
		if let Some(classic) = link.classic.as_mut() {
			classic.authenticating = false;
			classic.pairing = false;
		}
		if status != 0 {
			// THE PEER DID NOT AUTHENTICATE: a pairing that failed, or a bonded peer that lost its key. Nothing
			// that carries data opens on this link.
			controller.fail_attempt(&peer);
			controller.bredr_state.wants.retain(|(held, _)| *held != peer);
			controller.disconnect(handle, REASON_AUTHENTICATION);
			return;
		}
		controller.command(hci_bredr::opcode::SET_CONNECTION_ENCRYPTION, &hci_bredr::set_encryption(handle, true));
	}

	pub(crate) fn classic_encryption(&mut self, at: usize, status: u8, handle: u16, enabled: bool) {
		let controller = &mut self.controllers[at];
		let Some(link) = controller.link_mut(handle) else { return };
		let peer = link.peer;
		if link.classic.as_ref().is_some_and(|classic| classic.refused) {
			return;
		}
		if status != 0 || !enabled {
			link.encrypted = false;
			controller.fail_attempt(&peer);
			controller.disconnect(handle, REASON_AUTHENTICATION);
			return;
		}
		link.encrypted = true;
		let level = link.classic.as_ref().and_then(|classic| classic.level);
		let level = level.or_else(|| self.record(at, &peer).map(|record| level_from_wire(&record.level)));
		let controller = &mut self.controllers[at];
		let Some(link) = controller.link_mut(handle) else { return };
		let security = level.as_ref().map_or(SecurityLevel::EncryptedUnauthenticated, security_of);
		link.security = security;
		if let Some(classic) = link.classic.as_mut() {
			classic.level = level;
		}
		if let Some(attempt) = controller.attempt.as_mut()
			&& attempt.peer == peer
			&& attempt.state == PairingState::Pairing
		{
			attempt.state = PairingState::Bonded;
			attempt.security = security;
		}
		if controller.bredr_state.legacy == Some(peer) {
			controller.bredr_state.legacy = None;
		}
		// THE LE KEY'S DERIVATION BEGINS by asking the peer's fixed channels, where this host is the central and the
		// pairing just made a Secure Connections key; a peripheral's derivation is the peer's to start.
		let ask_fixed = match controller.link_mut(handle).and_then(|link| link.classic.as_mut()) {
			Some(classic) if classic.derive.is_some() && classic.central => Some(classic.channels.information_request(l2cap_bredr::information::FIXED_CHANNELS)),
			Some(classic) => {
				classic.drop_derive();
				None
			}
			None => None,
		};
		if let Some(signal) = ask_fixed {
			self.send_signal(at, handle, &signal);
		}
		let controller = &mut self.controllers[at];
		// WHAT WAITED FOR ENCRYPTION GOES NOW: the peer's held connection requests, decided with the link
		// encrypted, and the operator's profile connections.
		let held = controller.link_mut(handle).and_then(|link| link.classic.as_mut()).map(|classic| core::mem::take(&mut classic.held)).unwrap_or_default();
		for signal in held {
			self.on_signal(at, handle, signal);
		}
		let wanted: Vec<Want> = self.controllers[at].bredr_state.wants.iter().filter(|(held, _)| *held == peer).map(|(_, want)| *want).collect();
		self.controllers[at].bredr_state.wants.retain(|(held, _)| *held != peer);
		for want in wanted {
			match want {
				Want::Profile(profile) => self.start_profile(at, handle, profile),
				Want::Push => self.start_push_search(at, handle),
			}
		}
	}

	pub(crate) fn classic_gone(&mut self, at: usize, mut link: Link) {
		self.a2dp_gone(&mut link);
		self.voice_link_gone(at, &mut link);
		self.serial_closed(at, &link.peer, Error::Closed);
		self.opp_link_gone(at, &link.peer);
		self.pan_link_gone(&mut link);
		let state = &mut self.controllers[at].bredr_state;
		if state.prompt.as_ref().is_some_and(|pending| pending.peer == link.peer) {
			state.prompt = None;
		}
		state.wants.retain(|(held, _)| *held != link.peer);
	}

	// ------------------------------------------------------------------ keys and pairing

	fn on_link_key_request(&mut self, at: usize, address: &[u8; 6]) {
		let peer = bredr(address);
		// THE OPERATOR'S PAIRING IS A NEW KEY: the stored one is refused so that the two sides pair, and the
		// result is held to the old bond's level when it arrives.
		let operator_pairs = self.controllers[at].attempt.as_ref().is_some_and(|attempt| attempt.peer == peer && attempt.state == PairingState::Pairing);
		let record = if operator_pairs { None } else { self.bond(at, &peer) };
		let controller = &mut self.controllers[at];
		match record {
			Some(mut record) if record.link_key.len() == 16 => {
				let mut key = [0u8; 16];
				key.copy_from_slice(&record.link_key);
				let level = level_from_wire(&record.level);
				scrub_record(&mut record);
				let mut params = hci_bredr::link_key_reply(address, &key);
				controller.command(hci_bredr::opcode::LINK_KEY_REQUEST_REPLY, &params);
				scrub(&mut params);
				scrub(&mut key);
				if let Some(classic) = controller.link_to_mut(&peer).and_then(|link| link.classic.as_mut()) {
					classic.level = Some(level);
				}
			}
			Some(mut record) => {
				scrub_record(&mut record);
				controller.command(hci_bredr::opcode::LINK_KEY_REQUEST_NEGATIVE_REPLY, &hci_bredr::address_only(address));
			}
			None => {
				controller.command(hci_bredr::opcode::LINK_KEY_REQUEST_NEGATIVE_REPLY, &hci_bredr::address_only(address));
			}
		}
	}

	// WHETHER A PAIRING MAY RUN AT ALL: the operator's own, or an incoming one while a watcher is attached.
	fn pairing_allowed(&self, at: usize, peer: &Peer) -> bool {
		let controller = &self.controllers[at];
		let operator_pairs = controller.attempt.as_ref().is_some_and(|attempt| attempt.peer == *peer && attempt.state == PairingState::Pairing);
		operator_pairs || bt_pairing::pairable(controller.bredr_state.watcher != 0)
	}

	fn on_io_capability_request(&mut self, at: usize, address: &[u8; 6]) {
		let peer = bredr(address);
		if !self.pairing_allowed(at, &peer) {
			print(b"BluetoothService: an incoming pairing was refused - nothing is watching for prompts\n");
			self.controllers[at].command(hci_bredr::opcode::IO_CAPABILITY_REQUEST_NEGATIVE_REPLY, &hci_bredr::io_capability_negative(address));
			return;
		}
		let controller = &mut self.controllers[at];
		let watcher = controller.bredr_state.watcher != 0;
		let bt_pairing::Declared::Classic(capability) = bt_pairing::declared(bt_pairing::Radio::Classic, watcher) else { return };
		let authentication = if watcher { hci_bredr::AUTH_MITM_BONDING } else { hci_bredr::AUTH_BONDING };
		if let Some(classic) = controller.link_to_mut(&peer).and_then(|link| link.classic.as_mut()) {
			classic.ours = capability;
			classic.pairing = true;
		}
		controller.command(hci_bredr::opcode::IO_CAPABILITY_REQUEST_REPLY, &hci_bredr::io_capability_reply(address, capability, authentication));
	}

	fn on_confirmation(&mut self, at: usize, address: &[u8; 6], value: u32) {
		let peer = bredr(address);
		let held = self.record(at, &peer).map(|record| level_from_wire(&record.level));
		let controller = &mut self.controllers[at];
		let (ours, theirs) = controller.link_to(&peer).and_then(|link| link.classic.as_ref()).map_or((IoCapability::NoInputNoOutput, IoCapability::NoInputNoOutput), |classic| (classic.ours, classic.theirs.unwrap_or(IoCapability::NoInputNoOutput)));
		let outgoing = controller.attempt.as_ref().is_some_and(|attempt| attempt.peer == peer && attempt.state == PairingState::Pairing);
		let question = bt_pairing::classic_confirmation(ours, theirs, value, outgoing);
		// NO DOWNGRADE ON THE AUTHENTICATION AXIS: an authenticated bond is not replaced through Just Works.
		let just_works = !matches!(question, Some(Question::Compare(_)));
		if just_works && held.is_some_and(|held| held.authenticated) {
			print(b"BluetoothService: a peer bonded with authentication offered Just Works; refused, and the bond is kept\n");
			controller.command(hci_bredr::opcode::USER_CONFIRMATION_REQUEST_NEGATIVE_REPLY, &hci_bredr::address_only(address));
			return;
		}
		match question {
			// AN OUTGOING JUST WORKS PAIRING this host started is accepted without asking: the operator asked.
			None => {
				controller.command(hci_bredr::opcode::USER_CONFIRMATION_REQUEST_REPLY, &hci_bredr::address_only(address));
			}
			Some(question) => self.raise(at, peer, question),
		}
	}

	fn on_pin_request(&mut self, at: usize, address: &[u8; 6]) {
		let peer = bredr(address);
		let held = self.record(at, &peer).map(|record| level_from_wire(&record.level));
		let controller = &self.controllers[at];
		let asked = controller.bredr_state.legacy == Some(peer);
		// LEGACY PIN PAIRING ONLY WHEN THE OPERATOR ASKED, for this device, and never over a better bond.
		if !bt_pairing::legacy_allowed(asked, held.as_ref()) || controller.bredr_state.watcher == 0 {
			print(b"BluetoothService: a legacy PIN pairing was refused - the operator did not ask for one, or a better bond exists\n");
			self.controllers[at].command(hci_bredr::opcode::PIN_CODE_REQUEST_NEGATIVE_REPLY, &hci_bredr::address_only(address));
			return;
		}
		self.raise(at, peer, Question::EnterPin);
	}

	// THE KEY THE CONTROLLER MADE, held to the level rules and stored durably before the link is used.
	fn on_link_key(&mut self, at: usize, address: &[u8; 6], key: &[u8; 16], kind: KeyType) {
		let peer = bredr(address);
		let Some(handle) = self.controllers[at].link_to(&peer).map(|link| link.handle) else { return };
		let Some(level) = Level::of_link_key(kind) else {
			// A DEBUG KEY IS NO KEY: its private half is published in the specification.
			print(b"BluetoothService: a pairing produced a debug or unknown link key; refused\n");
			self.refuse_link(at, handle);
			return;
		};
		let held = self.record(at, &peer);
		if let Some(held) = held.as_ref()
			&& !level.may_replace(&level_from_wire(&held.level))
		{
			print(b"BluetoothService: a bonded peer paired again at a lower level; the bond is kept and the link refused\n");
			self.refuse_link(at, handle);
			return;
		}
		let local = local_wire(&self.controllers[at]);
		let name = self.controllers[at].bredr_state.name_of(&peer).unwrap_or_default();
		let (enabled, trusted, alias, name) = held.map(|held| (held.enabled, held.trusted, held.alias, if held.name.is_empty() { name.clone() } else { held.name })).unwrap_or((false, Vec::new(), String::new(), name));
		let mut record = BondRecord { version: service_logic::bond_store::VERSION, local, peer: peer_to_wire(&peer), key: Vec::new(), security: security_of(&level), name, enabled, radio: Radio::Classic, link_key: key.to_vec(), link_key_type: kind.value(), level: level_to_wire(&level), trusted, alias, irk: Vec::new(), ediv: 0, rand: Vec::new() };
		// BONDED ONLY AFTER THE DURABLE COMMIT, as on LE: a store that cannot write ends the attempt.
		let stored = self.store_bond(&record);
		scrub_record(&mut record);
		let controller = &mut self.controllers[at];
		if !stored {
			print(b"BluetoothService: the bond could not be stored durably; the pairing is abandoned\n");
			controller.fail_attempt(&peer);
			controller.disconnect(handle, REASON_AUTHENTICATION);
			return;
		}
		if let Some(classic) = controller.link_mut(handle).and_then(|link| link.classic.as_mut()) {
			classic.level = Some(level);
			// A SECURE CONNECTIONS KEY IS A SOURCE for the LE key, derived once the link is encrypted; anything older
			// is not, and the LE side keeps whatever it has.
			classic.drop_derive();
			if level.agreement == Agreement::SecureConnections {
				classic.derive = Some((*key, level.authenticated));
			}
		}
		if controller.attempt.as_ref().is_some_and(|attempt| attempt.peer == peer) {
			if let Some(attempt) = controller.attempt.as_mut() {
				attempt.security = security_of(&level);
			}
		} else {
			// AN INCOMING PAIRING the person consented to is an attempt too, so `progress` reports it.
			controller.attempt = Some(Attempt { peer, deadline: clock().saturating_add(PAIRING_TICKS), state: PairingState::Pairing, security: security_of(&level) });
		}
		self.refresh_policy(at);
	}

	// A KEY THIS HOST REFUSED: the link is disconnected, and an encryption the peer turns on with that key before the
	// disconnection lands opens nothing.
	fn refuse_link(&mut self, at: usize, handle: u16) {
		let controller = &mut self.controllers[at];
		let Some(link) = controller.link_mut(handle) else { return };
		let peer = link.peer;
		if let Some(classic) = link.classic.as_mut() {
			classic.refused = true;
			classic.held.clear();
		}
		controller.fail_attempt(&peer);
		controller.disconnect(handle, REASON_AUTHENTICATION);
	}

	// ------------------------------------------------------------------ prompts

	// RAISE A PROMPT on the watcher: one at a time per controller, and with no watcher, or one already in
	// flight, the answer is no at once.
	pub(crate) fn raise(&mut self, at: usize, peer: Peer, question: Question) {
		let controller = &mut self.controllers[at];
		if controller.bredr_state.watcher == 0 || controller.bredr_state.prompt.is_some() {
			self.act(at, peer, question, Reply::No);
			return;
		}
		let id = controller.bredr_state.next_prompt;
		controller.bredr_state.next_prompt = id.wrapping_add(1).max(1);
		let name = controller.bredr_state.name_of(&peer).unwrap_or_default();
		controller.bredr_state.prompt = Some(Pending { id, peer, prompt: Prompt::new(question, now_ms()), name });
		self.send_prompt(at);
	}

	fn send_prompt(&mut self, at: usize) {
		let state = &mut self.controllers[at].bredr_state;
		let Some(pending) = state.prompt.as_ref() else { return };
		let (question, value) = question_wire(&pending.prompt.question);
		let left = pending.prompt.deadline().saturating_sub(now_ms()).min(u64::from(u32::MAX)) as u32;
		let record = proto::system::PairingPrompt { id: pending.id, controller: at as u32, peer: peer_to_wire(&pending.peer), radio: radio_of(&pending.peer), question, value, keypresses: pending.prompt.keypresses, deadline_ms: left, name: pending.name.clone() };
		let mut frame = [0u8; 256];
		let mut handles = wire::Handles::new();
		let Some(len) = bluetooth_operator::prompts_frame(0, &record, &mut frame, &mut handles) else { return };
		if matches!(try_send_outcome(state.watcher, &frame[..len], 0), SendOutcome::Failed) {
			// THE WATCHER IS GONE: nobody can answer, so the answer is no.
			state.detach_watcher();
			let Some(pending) = state.prompt.take() else { return };
			self.act(at, pending.peer, pending.prompt.question, Reply::No);
			self.refresh_policy(at);
		}
	}

	// The watcher's end: it sends nothing, so what arrives is its closing.
	pub(crate) fn drain_watcher(&mut self, at: usize, buf: &mut [u8]) {
		let watcher = self.controllers[at].bredr_state.watcher;
		loop {
			match try_recv_caps(watcher, buf) {
				PolledCaps::Message { handles, .. } => {
					for &handle in handles.as_slice() {
						close(handle);
					}
				}
				PolledCaps::Empty => return,
				PolledCaps::Closed => break,
			}
		}
		let state = &mut self.controllers[at].bredr_state;
		state.detach_watcher();
		if let Some(pending) = state.prompt.take() {
			self.act(at, pending.peer, pending.prompt.question, Reply::No);
		}
		self.refresh_policy(at);
	}

	pub(crate) fn answer(&mut self, at: usize, id: u32, reply: PromptReply) -> Result<(), Error> {
		let state = &mut self.controllers[at].bredr_state;
		let Some(pending) = state.prompt.as_ref().filter(|pending| pending.id == id) else { return Err(Error::NotFound) };
		let mut reply = match reply {
			PromptReply::Yes => Reply::Yes,
			PromptReply::No => Reply::No,
			PromptReply::Passkey(value) => Reply::Passkey(value),
			PromptReply::Pin(pin) => Reply::Pin(pin),
		};
		if pending.prompt.expired(now_ms()) {
			if let Reply::Pin(pin) = &mut reply {
				scrub(pin);
			}
			let Some(pending) = state.prompt.take() else { return Err(Error::NotFound) };
			self.act(at, pending.peer, pending.prompt.question, Reply::No);
			return Err(Error::TimedOut);
		}
		if !pending.prompt.fits(&reply) {
			if let Reply::Pin(pin) = &mut reply {
				scrub(pin);
			}
			return Err(Error::Invalid);
		}
		// A PASSKEY BEING TYPED stays on the watcher after a yes, so the keypresses the peer reports reach the person
		// typing; the pairing's end takes it away.
		if matches!(pending.prompt.question, Question::ShowPasskey(_)) && reply == Reply::Yes {
			return Ok(());
		}
		let Some(pending) = state.prompt.take() else { return Err(Error::NotFound) };
		self.act(at, pending.peer, pending.prompt.question, reply);
		Ok(())
	}

	// WHAT AN ANSWER DOES, by the question it answers - to the LE pairing that asked it, or the controller's BR/EDR one.
	fn act(&mut self, at: usize, peer: Peer, question: Question, mut reply: Reply) {
		if !peer_is_classic(&peer) {
			self.le_act(at, peer, &reply);
			if let Reply::Pin(pin) = &mut reply {
				scrub(pin);
			}
			return;
		}
		let address = wire_of(&peer);
		let controller = &mut self.controllers[at];
		match (question, &mut reply) {
			(Question::Compare(_) | Question::Consent, Reply::Yes) => {
				controller.command(hci_bredr::opcode::USER_CONFIRMATION_REQUEST_REPLY, &hci_bredr::address_only(&address));
			}
			(Question::Compare(_) | Question::Consent, _) => {
				controller.command(hci_bredr::opcode::USER_CONFIRMATION_REQUEST_NEGATIVE_REPLY, &hci_bredr::address_only(&address));
			}
			// The peer types the passkey; a yes acknowledges it, and a no ends the pairing.
			(Question::ShowPasskey(_), Reply::Yes) => {}
			(Question::ShowPasskey(_), _) => {
				if let Some(handle) = controller.link_to(&peer).map(|link| link.handle) {
					controller.disconnect(handle, REASON_PAIRING_NOT_ALLOWED);
				}
			}
			(Question::EnterPin, Reply::Pin(pin)) => {
				if let Some(mut params) = hci_bredr::pin_code_reply(&address, pin) {
					controller.command(hci_bredr::opcode::PIN_CODE_REQUEST_REPLY, &params);
					scrub(&mut params);
				}
				scrub(pin);
			}
			(Question::EnterPin, _) => {
				controller.command(hci_bredr::opcode::PIN_CODE_REQUEST_NEGATIVE_REPLY, &hci_bredr::address_only(&address));
			}
			(Question::EnterPasskey, Reply::Passkey(value)) => {
				let value = *value;
				controller.command(hci_bredr::opcode::USER_PASSKEY_REQUEST_REPLY, &hci_bredr::passkey_reply(&address, value));
			}
			(Question::EnterPasskey, _) => {
				controller.command(hci_bredr::opcode::USER_PASSKEY_REQUEST_NEGATIVE_REPLY, &hci_bredr::address_only(&address));
			}
		}
	}

	// ------------------------------------------------------------------ the inbound policy

	// THE SCAN MODE THE POLICY SAYS, written when it changes: connectable while a bonded peer is trusted for
	// an inbound profile, a receive waits or a watcher makes this host pairable; discoverable only under
	// `btctl discoverable`.
	pub(crate) fn refresh_policy(&mut self, at: usize) {
		let controller = &self.controllers[at];
		if !controller.classic || !controller.powered {
			return;
		}
		let any_trusted = self.records(at).iter().any(|record| record.radio == Radio::Classic && trust_of(&record.trusted).any_inbound());
		let controller = &mut self.controllers[at];
		let state = &mut controller.bredr_state;
		let now = clock();
		if state.discoverable_until.is_some_and(|until| now >= until) {
			state.discoverable_until = None;
		}
		let scan = bt_policy::scan(true, any_trusted, state.receive_waits.is_some(), state.watcher != 0, state.discoverable_until.map(|_| 1), 0);
		let value = scan.value();
		if state.written_scan != Some(value) {
			state.written_scan = Some(value);
			controller.command(hci_bredr::opcode::WRITE_SCAN_ENABLE, &[value]);
		}
	}

	pub(crate) fn classic_complete(&mut self, _at: usize, _op: u16, _status: u8, _params: &[u8]) {}

	pub(crate) fn classic_status(&mut self, at: usize, _status: u8, op: u16) {
		let controller = &mut self.controllers[at];
		match op {
			hci_bredr::opcode::INQUIRY => {
				if let Some(scan) = controller.scan.as_mut() {
					scan.inquiring = false;
				}
			}
			hci_bredr::opcode::CREATE_CONNECTION => {
				if let Some(peer) = controller.bredr_state.paging.take() {
					controller.fail_attempt(&peer);
					controller.bredr_state.wants.retain(|(held, _)| *held != peer);
				}
			}
			hci_bredr::opcode::AUTHENTICATION_REQUESTED | hci_bredr::opcode::SET_CONNECTION_ENCRYPTION => {
				if let Some(handle) = controller.links.iter().find(|link| link.classic.as_ref().is_some_and(|classic| classic.authenticating) || (link.is_classic() && !link.encrypted)).map(|link| link.handle) {
					let peer = controller.link(handle).map(|link| link.peer).unwrap_or_default();
					controller.fail_attempt(&peer);
					controller.disconnect(handle, REASON_AUTHENTICATION);
				}
			}
			_ => {}
		}
	}

	pub(crate) fn classic_timers(&mut self, at: usize, now: u64) {
		// A PROMPT PAST ITS DEADLINE is answered no by this host.
		if let Some(pending) = self.controllers[at].bredr_state.prompt.as_ref()
			&& pending.prompt.expired(now * (1000 / TICKS_PER_SECOND))
		{
			let Some(pending) = self.controllers[at].bredr_state.prompt.take() else { return };
			self.act(at, pending.peer, pending.prompt.question, Reply::No);
		}
		if self.controllers[at].bredr_state.discoverable_until.is_some_and(|until| now >= until) {
			self.refresh_policy(at);
		}
		let controller = &mut self.controllers[at];
		let mut sniff = Vec::new();
		let mut ertm_due = Vec::new();
		for link in controller.links.iter_mut() {
			let handle = link.handle;
			let Some(classic) = link.classic.as_mut() else { continue };
			if !classic.sniff && classic.sniff_allowed() && now >= classic.last_activity + SNIFF_IDLE_TICKS {
				// Asked once; the controller's mode change says whether it took.
				classic.last_activity = u64::MAX / 2;
				sniff.push(handle);
			}
			for (cid, ertm) in classic.ertm.iter_mut() {
				if ertm.deadline().is_some_and(|due| due <= now * 10) {
					ertm_due.push((handle, *cid, ertm.tick(now * 10)));
				}
			}
		}
		for handle in sniff {
			controller.command(hci_bredr::opcode::SNIFF_MODE, &hci_bredr::sniff_mode(handle, hci_bredr::SNIFF_MAX_SLOTS, hci_bredr::SNIFF_MIN_SLOTS * 4));
		}
		for (handle, cid, outs) in ertm_due {
			self.run_ertm(at, handle, cid, outs);
		}
	}

	// ------------------------------------------------------------------ L2CAP

	pub(crate) fn send_signal(&mut self, at: usize, handle: u16, signal: &Signal) {
		let bytes = l2cap_bredr::encode(signal);
		self.controllers[at].l2cap(handle, l2cap_bredr::SIGNALLING_CID, &bytes);
	}

	// ONE BASIC-MODE L2CAP PDU from a BR/EDR link: signalling, or a dynamic channel's data.
	pub(crate) fn classic_pdu(&mut self, at: usize, handle: u16, cid: u16, payload: &[u8]) {
		if let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) {
			classic.last_activity = clock();
		}
		if cid == l2cap_bredr::SECURITY_MANAGER_CID {
			self.bredr_smp(at, handle, payload);
			return;
		}
		if cid == l2cap_bredr::SIGNALLING_CID {
			match l2cap_bredr::decode(payload) {
				Ok(signals) => {
					for signal in signals {
						self.on_signal(at, handle, signal);
					}
				}
				Err(_) => print(b"BluetoothService: a malformed signalling PDU was refused\n"),
			}
			return;
		}
		let Some(channel) = self.controllers[at].link(handle).and_then(|link| link.classic.as_ref()).and_then(|classic| classic.channels.get(cid)).copied() else { return };
		if channel.state != l2cap_bredr::State::Open {
			return;
		}
		if matches!(channel.mode, Mode::Ertm(_)) {
			let mut pdu = Vec::with_capacity(4 + payload.len());
			pdu.extend_from_slice(&(payload.len() as u16).to_le_bytes());
			pdu.extend_from_slice(&cid.to_le_bytes());
			pdu.extend_from_slice(payload);
			let now = now_ms();
			let outs = match self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).and_then(|classic| classic.ertm.iter_mut().find(|(held, _)| *held == cid)) {
				Some((_, ertm)) => ertm.receive(&pdu, now),
				None => return,
			};
			self.run_ertm(at, handle, cid, outs);
			return;
		}
		self.channel_data(at, handle, cid, channel.psm, payload);
	}

	fn run_ertm(&mut self, at: usize, handle: u16, cid: u16, outs: Vec<ErtmOut>) {
		let psm = self.controllers[at].link(handle).and_then(|link| link.classic.as_ref()).and_then(|classic| classic.channels.get(cid)).map(|channel| channel.psm).unwrap_or(0);
		for out in outs {
			match out {
				ErtmOut::Send(pdu) => {
					self.controllers[at].l2cap_pdu(handle, pdu);
				}
				ErtmOut::Deliver(sdu) => self.channel_data(at, handle, cid, psm, &sdu),
				ErtmOut::Fail => self.close_channel(at, handle, cid),
			}
		}
	}

	pub(crate) fn close_channel(&mut self, at: usize, handle: u16, cid: u16) {
		let signal = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).and_then(|classic| classic.channels.close(cid));
		if let Some(signal) = signal {
			self.send_signal(at, handle, &signal);
		}
	}

	// A channel's SDU, by the protocol it carries.
	fn channel_data(&mut self, at: usize, handle: u16, cid: u16, channel_psm: u16, payload: &[u8]) {
		match channel_psm {
			psm::SDP => {
				let client = self.controllers[at].link(handle).and_then(|link| link.classic.as_ref()).and_then(|classic| classic.sdp_client.as_ref()).is_some_and(|client| client.cid == cid);
				if client {
					self.sdp_answer(at, handle, cid, payload);
				} else {
					// THIS HOST'S SDP SERVER answers any connected peer.
					let answer = self.controllers[at].bredr_state.sdp.answer(payload);
					self.send_on(at, handle, cid, &answer);
				}
			}
			psm::RFCOMM => self.rfcomm_data(at, handle, payload),
			psm::BNEP => self.pan_data(at, handle, cid, payload),
			psm::AVDTP | psm::AVCTP => self.a2dp_data(at, handle, cid, payload),
			psm::HID_INTERRUPT => {
				// INPUT DATA: the header byte, then the report as the device's descriptor lays it out.
				if payload.first() != Some(&HIDP_DATA_INPUT) {
					return;
				}
				let Some(link) = self.controllers[at].link_mut(handle) else { return };
				if !link.encrypted {
					return;
				}
				let reports = link.decoder.as_mut().map(|decoder| decoder.report(&payload[1..])).unwrap_or_default();
				deliver(link, &reports);
			}
			// The control channel's handshakes say a request was taken; nothing this host asks needs the answer.
			psm::HID_CONTROL if payload.first().is_some_and(|byte| byte >> 4 == HIDP_HANDSHAKE >> 4) => {}
			// A CHANNEL NO PROFILE READS: an Object Push session's on L2CAP, where it is one; otherwise nothing this host
			// acts on.
			_ => self.opp_l2cap_data(at, handle, cid, payload),
		}
	}

	// Data on a basic-mode channel: to the peer's channel id.
	pub(crate) fn send_on(&mut self, at: usize, handle: u16, cid: u16, payload: &[u8]) {
		let Some(remote) = self.controllers[at].link(handle).and_then(|link| link.classic.as_ref()).and_then(|classic| classic.channels.get(cid)).map(|channel| channel.remote_cid) else { return };
		self.controllers[at].l2cap(handle, remote, payload);
	}

	// ONE SIGNAL FROM THE PEER. An inbound connection request is decided by the policy with the peer's
	// standing; one on a bonded peer's link that is not encrypted yet waits for it.
	fn on_signal(&mut self, at: usize, handle: u16, signal: Signal) {
		let Some(link) = self.controllers[at].link(handle) else { return };
		let peer = link.peer;
		let encrypted = link.encrypted;
		let securing = link.classic.as_ref().is_some_and(|classic| classic.authenticating || classic.pairing);
		// The peer's bond is read only for what the policy decides: a connection request.
		let record = if matches!(signal.command, l2cap_bredr::Command::ConnectionRequest { .. }) { self.record(at, &peer) } else { None };
		// A CHANNEL THAT NEEDS SECURITY on a bonded peer's link that is not encrypted yet waits for it, and this
		// host secures the link now if nothing is doing so already. An unbonded peer is decided as it stands.
		if let l2cap_bredr::Command::ConnectionRequest { psm: asked, .. } = signal.command
			&& asked != psm::SDP
			&& !encrypted
			&& (securing || record.is_some())
		{
			if let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut())
				&& classic.held.len() < bt_bounds::CHANNELS_PER_LINK
			{
				classic.held.push(signal);
			}
			if !securing {
				self.authenticate(at, handle);
			}
			return;
		}
		let standing = bt_policy::Peer { trust: record.as_ref().map(|record| trust_of(&record.trusted)).unwrap_or_default(), receive_waits_for_it: self.controllers[at].bredr_state.receive_waits == Some(peer), encrypted };
		let served = |inbound: Inbound| -> bool {
			match inbound {
				Inbound::Sdp | Inbound::Rfcomm | Inbound::Profile(bt_policy::Profile::Input | bt_policy::Profile::Audio) => true,
				// A profile channel opens only where a profile is there to read it.
				Inbound::Profile(_) | Inbound::Opp | Inbound::None => false,
			}
		};
		let mut admit = |asked: u16| {
			let inbound = bt_policy::inbound_by_psm(asked);
			match bt_policy::decide(inbound, &standing) {
				Decision::Accept if served(inbound) => l2cap_bredr::Admission::Accept,
				Decision::Accept | Decision::NotServed => l2cap_bredr::Admission::NotSupported,
				Decision::Refused => l2cap_bredr::Admission::Security,
			}
		};
		let outs = match self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) {
			Some(classic) => classic.channels.on_signal(signal, &mut admit),
			None => return,
		};
		for out in outs {
			match out {
				L2Out::Send(signal) => self.send_signal(at, handle, &signal),
				L2Out::Opened(cid) => self.channel_opened(at, handle, cid),
				L2Out::Refused(cid, _) | L2Out::Closed(cid) => self.channel_closed(at, handle, cid),
				L2Out::Information { info_type, result, data } => {
					if info_type == l2cap_bredr::information::FIXED_CHANNELS {
						self.derive_le_key(at, handle, result == l2cap_bredr::information::SUCCESS && l2cap_bredr::peer_has_security_manager(&data));
					}
				}
			}
		}
	}

	// ------------------------------------------------------------------ the other radio's key

	// THE PEER'S FIXED CHANNELS ARE KNOWN: with the BR/EDR Security Manager, the LTK is derived over it from the key
	// this link's pairing made; without it, the source key is let go and the LE side keeps what it has.
	fn derive_le_key(&mut self, at: usize, handle: u16, has_security_manager: bool) {
		let irk = if has_security_manager { self.own_irk(at) } else { None };
		let controller = &mut self.controllers[at];
		let identity = local_address(controller);
		let Some(link) = controller.link_mut(handle) else { return };
		let peer = link.peer;
		let Some(classic) = link.classic.as_mut() else { return };
		let source = classic.derive.take();
		let (Some((mut key, authenticated)), Some(irk)) = (source, irk) else {
			if let Some((mut key, _)) = source {
				scrub(&mut key);
			}
			return;
		};
		// The peer's address as SMP takes it: a BR/EDR device address is public.
		let mut peer_public = peer;
		peer_public[0] = KIND_PUBLIC;
		let options = smp_pairing::Options { io: smp_pairing::IO_NO_INPUT_NO_OUTPUT, legacy: false, irk, identity, cross_transport: true };
		let (pairing, first) = smp_pairing::Initiator::over_bredr(identity, peer_public, key, authenticated, options);
		scrub(&mut key);
		link.pairing = Some(pairing);
		self.run_bredr_smp(at, handle, alloc::vec![first]);
	}

	// ONE PDU ON THE BR/EDR SECURITY MANAGER'S CHANNEL: to this host's derivation where it started one, or a central
	// peer's request answered - only on a link encrypted with a Secure Connections key this host holds.
	fn bredr_smp(&mut self, at: usize, handle: u16, payload: &[u8]) {
		let Some(link) = self.controllers[at].link_mut(handle) else { return };
		if let Some(pairing) = link.pairing.as_mut() {
			let steps = pairing.on_pdu(payload);
			self.run_bredr_smp(at, handle, steps);
			return;
		}
		if let Some(responder) = link.classic.as_mut().and_then(|classic| classic.responder.as_mut()) {
			let steps = responder.on_pdu(payload);
			self.run_bredr_smp(at, handle, steps);
			return;
		}
		if payload.first() != Some(&smp_pairing::code::PAIRING_REQUEST) {
			return;
		}
		let (peer, encrypted) = (link.peer, link.encrypted);
		let source = if encrypted { self.bond(at, &peer) } else { None };
		let source = source.and_then(|mut record| {
			let level = level_from_wire(&record.level);
			let key = (record.link_key.len() == 16 && level.agreement == Agreement::SecureConnections).then(|| {
				let mut key = [0u8; 16];
				key.copy_from_slice(&record.link_key);
				(key, level.authenticated)
			});
			scrub_record(&mut record);
			key
		});
		let irk = self.own_irk(at);
		let identity = local_address(&self.controllers[at]);
		let (Some((mut key, authenticated)), Some(irk)) = (source, irk) else {
			// NOTHING TO DERIVE FROM: no Secure Connections key, or the link is not encrypted with it.
			self.controllers[at].l2cap(handle, l2cap_bredr::SECURITY_MANAGER_CID, &[smp_pairing::code::PAIRING_FAILED, smp_pairing::reason::AUTHENTICATION_REQUIREMENTS]);
			return;
		};
		let options = smp_pairing::Options { io: smp_pairing::IO_NO_INPUT_NO_OUTPUT, legacy: false, irk, identity, cross_transport: true };
		let (responder, steps) = BredrResponder::on_request(payload, key, authenticated, &options);
		scrub(&mut key);
		if let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) {
			classic.responder = responder;
		}
		self.run_bredr_smp(at, handle, steps);
	}

	fn run_bredr_smp(&mut self, at: usize, handle: u16, mut steps: Vec<smp_pairing::Step>) {
		use smp_pairing::Step;
		for step in steps.iter_mut() {
			match step {
				Step::Send(pdu) => {
					self.controllers[at].l2cap(handle, l2cap_bredr::SECURITY_MANAGER_CID, pdu);
				}
				Step::Bonded(bonded) => {
					self.end_bredr_smp(at, handle);
					self.store_derived_le(at, handle, bonded);
				}
				// A DERIVATION THAT DID NOT HAPPEN ends there: the BR/EDR link and its bond are untouched.
				Step::Failed(_) => {
					self.end_bredr_smp(at, handle);
					print(b"BluetoothService: no LE key was derived from the BR/EDR link key\n");
				}
				// Nothing else is asked over BR/EDR: no person, no Diffie-Hellman key, no encryption to start.
				Step::GenerateDhKey(_) | Step::Encrypt(_) | Step::Ask(_) => {}
			}
		}
	}

	fn end_bredr_smp(&mut self, at: usize, handle: u16) {
		if let Some(link) = self.controllers[at].link_mut(handle) {
			link.pairing = None;
			if let Some(classic) = link.classic.as_mut() {
				classic.responder = None;
			}
		}
	}

	// THE DERIVED LE KEY, kept as the device's LE bond under the identity it gave - its public address otherwise - at its
	// source's level, and never over a better LE bond. Its name and alias come from the BR/EDR bond; trust is per radio
	// and starts empty.
	fn store_derived_le(&mut self, at: usize, handle: u16, bonded: &mut smp_pairing::Bonded) {
		let Some(peer) = self.controllers[at].link(handle).map(|link| link.peer) else {
			scrub(&mut bonded.ltk);
			return;
		};
		let mut public = peer;
		public[0] = KIND_PUBLIC;
		let identity = bonded.identity.unwrap_or(public);
		let level = Level { agreement: Agreement::SecureConnections, authenticated: bonded.authenticated };
		if let Some(held) = self.record(at, &identity)
			&& !level.may_replace(&level_from_wire(&held.level))
		{
			print(b"BluetoothService: the LE key derived from the BR/EDR link key is below the LE bond already held; it is not kept\n");
			scrub(&mut bonded.ltk);
			return;
		}
		let (name, alias) = self.record(at, &peer).map(|record| (record.name, record.alias)).unwrap_or_default();
		let local = local_wire(&self.controllers[at]);
		let mut record = BondRecord { version: service_logic::bond_store::VERSION, local, peer: peer_to_wire(&identity), key: bonded.ltk.to_vec(), security: security_of(&level), name, enabled: false, radio: Radio::Le, link_key: Vec::new(), link_key_type: 0, level: level_to_wire(&level), trusted: Vec::new(), alias, irk: bonded.irk.map(|irk| irk.to_vec()).unwrap_or_default(), ediv: 0, rand: Vec::new() };
		let stored = self.store_bond(&record);
		scrub_record(&mut record);
		scrub(&mut bonded.ltk);
		if !stored {
			print(b"BluetoothService: the LE key derived from the BR/EDR link key could not be stored\n");
			return;
		}
		if let Some(irk) = bonded.irk {
			let le = &mut self.controllers[at].le;
			le.irks.retain(|(held, _)| *held != identity);
			le.irks.push((identity, irk));
		}
		print(b"BluetoothService: an LE key was derived from the BR/EDR link key and kept at its level\n");
	}

	fn channel_opened(&mut self, at: usize, handle: u16, cid: u16) {
		let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) else { return };
		let Some(channel) = classic.channels.get(cid).copied() else { return };
		if let Mode::Ertm(parameters) = channel.mode {
			classic.ertm.retain(|(held, _)| *held != cid);
			classic.ertm.push((cid, Ertm::new(channel.remote_cid, parameters, usize::from(channel.our_mtu))));
		}
		match channel.psm {
			psm::HID_CONTROL | psm::HID_INTERRUPT => {
				if channel.psm == psm::HID_CONTROL {
					classic.hid.control = Some(cid);
				} else {
					classic.hid.interrupt = Some(cid);
				}
				// THE DESCRIPTOR IS ASKED FOR ONCE BOTH ARE UP: the device's own record says how its reports read.
				let (both, outgoing) = (classic.hid.control.is_some() && classic.hid.interrupt.is_some(), classic.hid.outgoing);
				if channel.psm == psm::HID_CONTROL && outgoing {
					// THIS HOST OPENED CONTROL: interrupt follows, as the HID profile orders them.
					if let Some((_, signal)) = classic.channels.open(psm::HID_INTERRUPT, false) {
						self.send_signal(at, handle, &signal);
					}
					return;
				}
				let decoded = self.controllers[at].link(handle).is_some_and(|link| link.decoder.is_some());
				let searched = self.controllers[at].link(handle).and_then(|link| link.classic.as_ref()).is_some_and(|classic| classic.hid.searched);
				if both && !decoded && !searched {
					self.start_profile(at, handle, Profile::Input);
				} else if both && decoded {
					self.hid_ready(at, handle);
				}
			}
			psm::SDP if classic.sdp_client.as_ref().is_some_and(|client| client.cid == cid) => {
				let transaction = self.controllers[at].bredr_state.transaction;
				self.controllers[at].bredr_state.transaction = transaction.wrapping_add(8);
				let Some(client) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).and_then(|classic| classic.sdp_client.as_mut()) else { return };
				let (search, request) = Search::new(transaction, Uuid::U16(client.uuid));
				client.search = Some(search);
				self.send_on(at, handle, cid, &request);
			}
			psm::AVDTP | psm::AVCTP => self.a2dp_channel_opened(at, handle, cid, channel.psm),
			psm::BNEP => self.pan_opened(at, handle, cid),
			psm::RFCOMM => {
				let peer_mtu = usize::from(channel.peer_mtu);
				let initiator = classic.rfcomm.as_ref().is_some_and(|rfcomm| rfcomm.cid == cid);
				if !initiator {
					// THE PEER OPENED THE SESSION: one RFCOMM session a link, and this side answers it.
					classic.rfcomm = Some(Rfcomm { cid, session: Session::new(false, peer_mtu), opening: Vec::new(), open: Vec::new(), held: Vec::new() });
					return;
				}
				let Some(rfcomm) = classic.rfcomm.as_mut() else { return };
				rfcomm.session = Session::new(true, peer_mtu);
				let mut outs = Vec::new();
				for (server_channel, _) in rfcomm.opening.clone() {
					match rfcomm.session.connect(server_channel) {
						Some(more) => outs.extend(more),
						None => outs.push(RfOut::Closed(server_channel)),
					}
				}
				self.run_rfcomm(at, handle, outs);
			}
			_ => self.opp_l2cap_opened(at, handle, cid),
		}
	}

	fn channel_closed(&mut self, at: usize, handle: u16, cid: u16) {
		self.a2dp_channel_closed(at, handle, cid);
		self.opp_l2cap_closed(at, handle, cid);
		self.pan_closed(at, handle, cid);
		let Some(link) = self.controllers[at].link_mut(handle) else { return };
		// THE INTERRUPT CHANNEL GOING ENDS THE DEVICE'S INPUT: what it held is let go on its streams.
		if link.classic.as_ref().is_some_and(|classic| classic.hid.interrupt == Some(cid) || classic.hid.control == Some(cid)) {
			let releases = link.decoder.as_mut().map(input::Decoder::releases).unwrap_or_default();
			deliver(link, &releases);
			if let Some(classic) = link.classic.as_mut() {
				if classic.hid.interrupt == Some(cid) {
					classic.hid.interrupt = None;
				}
				if classic.hid.control == Some(cid) {
					classic.hid.control = None;
				}
				classic.profiles = classic.profiles.with(bt_policy::Profile::Input, false);
			}
		}
		let Some(classic) = link.classic.as_mut() else { return };
		classic.ertm.retain(|(held, _)| *held != cid);
		if classic.sdp_client.as_ref().is_some_and(|client| client.cid == cid) {
			let client = classic.sdp_client.take();
			if let Some(client) = client
				&& client.search.is_some()
			{
				print(b"BluetoothService: a peer's SDP channel closed before its answer\n");
			}
		}
		if classic.rfcomm.as_ref().is_some_and(|rfcomm| rfcomm.cid == cid)
			&& let Some(mut rfcomm) = classic.rfcomm.take()
		{
			for (_, profile) in rfcomm.open.drain(..) {
				classic.profiles = classic.profiles.with(profile_to_logic(profile), false);
			}
		}
	}

	// ------------------------------------------------------------------ SDP as a client

	fn sdp_answer(&mut self, at: usize, handle: u16, cid: u16, payload: &[u8]) {
		let answer = match self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).and_then(|classic| classic.sdp_client.as_mut()).and_then(|client| client.search.as_mut()) {
			Some(search) => search.answer(payload),
			None => return,
		};
		match answer {
			Answer::More(request) => self.send_on(at, handle, cid, &request),
			Answer::Records(records) => {
				let Some(client) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).and_then(|classic| classic.sdp_client.take()) else { return };
				self.close_channel(at, handle, cid);
				match client.purpose {
					Purpose::Profile(profile) => self.found_service(at, handle, profile, &records),
					Purpose::Push => self.push_found(at, handle, &records),
				}
			}
			Answer::Failed(_) => {
				self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).map(|classic| classic.sdp_client.take());
				self.close_channel(at, handle, cid);
				print(b"BluetoothService: a peer's SDP server did not answer the search\n");
			}
		}
	}

	// WHAT A SEARCH FOUND, put to its purpose: the transport the profile's record names.
	fn found_service(&mut self, at: usize, handle: u16, purpose: Profile, records: &[sdp::Record]) {
		match purpose {
			Profile::Input => {
				// THE DEVICE'S REPORT DESCRIPTOR, from its HID descriptor list; a device without one is driven in
				// boot protocol, as a boot keyboard.
				let descriptor = records.iter().find_map(hid_descriptor);
				let decoder = descriptor.as_deref().and_then(input::Decoder::new);
				let boot = decoder.is_none();
				let pointer = self.controllers[at].link(handle).is_some_and(|link| self.controllers[at].bredr_state.is_pointer(&link.peer));
				let Some(link) = self.controllers[at].link_mut(handle) else { return };
				link.decoder = Some(decoder.unwrap_or_else(|| if pointer { input::Decoder::boot_mouse() } else { input::Decoder::boot_keyboard() }));
				let Some(classic) = link.classic.as_mut() else { return };
				if boot && let Some(control) = classic.hid.control.and_then(|cid| classic.channels.get(cid)).map(|channel| channel.remote_cid) {
					self.controllers[at].l2cap(handle, control, &[HIDP_SET_PROTOCOL_BOOT]);
				}
				let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) else { return };
				if classic.hid.outgoing && classic.hid.control.is_none() {
					// THE OPERATOR'S CONNECT: control first, interrupt once it is up.
					if let Some((_, signal)) = classic.channels.open(psm::HID_CONTROL, false) {
						self.send_signal(at, handle, &signal);
					}
					return;
				}
				self.hid_ready(at, handle);
			}
			// HEADPHONES OR A SPEAKER: a sink's record found, the stream is set up over AVDTP.
			Profile::Audio => {
				if records.is_empty() {
					print(b"BluetoothService: the device offers no audio sink\n");
					return;
				}
				self.a2dp_connect(at, handle);
			}
			// A PHONE'S NETWORK ACCESS POINT: BNEP's channel follows.
			Profile::Pan => self.pan_found(at, handle, records),
			Profile::Spp => {
				let Some(server_channel) = records.iter().find_map(sdp::Record::rfcomm_channel) else {
					print(b"BluetoothService: the peer offers no serial port\n");
					if let Some(peer) = self.controllers[at].link(handle).map(|link| link.peer) {
						self.serial_closed(at, &peer, Error::NotFound);
					}
					return;
				};
				self.rfcomm_connect(at, handle, server_channel, Some(purpose));
			}
			_ => {}
		}
	}

	// ------------------------------------------------------------------ RFCOMM

	// A DLC TO THE PEER'S CHANNEL, for a profile - or, with none, for an Object Push session.
	pub(crate) fn rfcomm_connect(&mut self, at: usize, handle: u16, server_channel: u8, profile: Option<Profile>) {
		let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) else { return };
		if let Some(rfcomm) = classic.rfcomm.as_mut() {
			if rfcomm.session.is_open() || rfcomm.session.dlc(server_channel).is_some() || !rfcomm.opening.is_empty() {
				rfcomm.opening.push((server_channel, profile));
				let outs = rfcomm.session.connect(server_channel).unwrap_or_else(|| alloc::vec![RfOut::Closed(server_channel)]);
				self.run_rfcomm(at, handle, outs);
			} else {
				rfcomm.opening.push((server_channel, profile));
			}
			return;
		}
		let Some((cid, signal)) = classic.channels.open(psm::RFCOMM, false) else {
			print(b"BluetoothService: no L2CAP channel left on the link for RFCOMM\n");
			return;
		};
		classic.rfcomm = Some(Rfcomm { cid, session: Session::new(true, usize::from(l2cap_bredr::DEFAULT_MTU)), opening: alloc::vec![(server_channel, profile)], open: Vec::new(), held: Vec::new() });
		self.send_signal(at, handle, &signal);
	}

	fn rfcomm_data(&mut self, at: usize, handle: u16, payload: &[u8]) {
		let Some(link) = self.controllers[at].link(handle) else { return };
		let peer = link.peer;
		let encrypted = link.encrypted;
		let record = self.record(at, &peer);
		let standing = bt_policy::Peer { trust: record.as_ref().map(|record| trust_of(&record.trusted)).unwrap_or_default(), receive_waits_for_it: self.controllers[at].bredr_state.receive_waits == Some(peer), encrypted };
		let offered: Vec<u8> = self.controllers[at].bredr_state.sdp.records().iter().filter_map(sdp::Record::rfcomm_channel).collect();
		// EACH CHANNEL A PEER OPENS is admitted only for a profile this host serves on it now, and the peer's
		// standing for that profile.
		let mut admit = |server_channel: u8| {
			let inbound = bt_policy::inbound_by_channel(server_channel);
			if offered.contains(&server_channel) && bt_policy::decide(inbound, &standing) == Decision::Accept { rfcomm::Admission::Accept } else { rfcomm::Admission::Refuse }
		};
		let outs = match self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).and_then(|classic| classic.rfcomm.as_mut()) {
			Some(rfcomm) => rfcomm.session.receive(payload, &mut admit),
			None => return,
		};
		self.run_rfcomm(at, handle, outs);
	}

	pub(crate) fn run_rfcomm(&mut self, at: usize, handle: u16, outs: Vec<RfOut>) {
		// What the serial port's grants and the Object Push sessions hear of it, once the session's steps are taken.
		let mut serial_data = false;
		let mut serial_opened = false;
		let mut serial_closed = false;
		let mut opp_channels = self.opp_rfcomm_channels(at, handle);
		let mut opp_events: Vec<(u8, super::opp::RfEvent)> = Vec::new();
		for out in outs {
			let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) else { return };
			let Some(rfcomm) = classic.rfcomm.as_mut() else { return };
			match out {
				RfOut::Send(frame) => {
					let cid = rfcomm.cid;
					self.send_on(at, handle, cid, &frame);
				}
				RfOut::Opened(server_channel) => {
					let asked = rfcomm.opening.iter().position(|(held, _)| *held == server_channel).map(|position| rfcomm.opening.remove(position).1);
					// AN OBJECT PUSH SESSION'S CHANNEL: one this side asked for, or a peer's to this host's server.
					if matches!(asked, Some(None)) || (asked.is_none() && server_channel == bt_policy::OPP_CHANNEL) {
						opp_channels.push(server_channel);
						opp_events.push((server_channel, super::opp::RfEvent::Opened));
						continue;
					}
					let profile = asked.flatten();
					// A HEADSET OPENED THE VOICE GATEWAY'S CHANNEL - admitted by the policy for a peer trusted for voice.
					let inbound_voice = profile.is_none() && matches!(server_channel, bt_policy::HFP_CHANNEL | bt_policy::HSP_CHANNEL);
					if let Some(profile) = profile.or(inbound_voice.then_some(Profile::Voice)) {
						rfcomm.open.push((server_channel, profile));
						classic.profiles = classic.profiles.with(profile_to_logic(profile), true);
					}
					serial_opened |= profile == Some(Profile::Spp);
					if inbound_voice {
						self.voice_opened(at, handle, server_channel);
					}
				}
				RfOut::Data(server_channel, bytes) if classic.voice.as_ref().is_some_and(|voice| voice.channel == server_channel) => self.voice_data(at, handle, &bytes),
				RfOut::Data(server_channel, bytes) if opp_channels.contains(&server_channel) => opp_events.push((server_channel, super::opp::RfEvent::Data(bytes))),
				RfOut::Data(server_channel, bytes) => {
					// WHAT ARRIVES FOR A CONSUMER that has not taken it is held, bounded; the credits went back
					// with the session's own top-up, so a peer is never stalled by a consumer that is absent.
					let held = match rfcomm.held.iter_mut().find(|(held, _)| *held == server_channel) {
						Some((_, held)) => held,
						None => {
							rfcomm.held.push((server_channel, VecDeque::new()));
							let last = rfcomm.held.len() - 1;
							&mut rfcomm.held[last].1
						}
					};
					held.extend(bytes);
					while held.len() > MAX_HELD_BYTES {
						held.pop_front();
					}
					serial_data |= rfcomm.open.iter().any(|(held, profile)| *held == server_channel && *profile == Profile::Spp);
				}
				RfOut::Closed(server_channel) => {
					if opp_channels.contains(&server_channel) || rfcomm.opening.iter().any(|(held, profile)| *held == server_channel && profile.is_none()) {
						opp_events.push((server_channel, super::opp::RfEvent::Closed));
					}
					serial_closed |= rfcomm.opening.iter().any(|(held, profile)| *held == server_channel && *profile == Some(Profile::Spp));
					rfcomm.opening.retain(|(held, _)| *held != server_channel);
					if let Some(position) = rfcomm.open.iter().position(|(held, _)| *held == server_channel) {
						let (_, profile) = rfcomm.open.remove(position);
						classic.profiles = classic.profiles.with(profile_to_logic(profile), false);
						serial_closed |= profile == Profile::Spp;
					}
					rfcomm.held.retain(|(held, _)| *held != server_channel);
					if classic.voice.as_ref().is_some_and(|voice| voice.channel == server_channel) {
						self.voice_gone(at, handle);
					}
				}
				RfOut::SessionOpen => {}
				RfOut::SessionClosed => {
					let cid = rfcomm.cid;
					serial_closed |= rfcomm.opening.iter().any(|(_, profile)| *profile == Some(Profile::Spp)) || rfcomm.open.iter().any(|(_, profile)| *profile == Profile::Spp);
					for channel in rfcomm.opening.iter().filter(|(_, profile)| profile.is_none()).map(|(channel, _)| *channel).chain(opp_channels.iter().copied()) {
						opp_events.push((channel, super::opp::RfEvent::Closed));
					}
					for (_, profile) in rfcomm.open.drain(..) {
						classic.profiles = classic.profiles.with(profile_to_logic(profile), false);
					}
					classic.rfcomm = None;
					self.voice_gone(at, handle);
					self.close_channel(at, handle, cid);
				}
			}
		}
		if serial_opened {
			self.serial_opened(at, handle);
		}
		if serial_data {
			self.serial_deliver(at, handle);
		}
		if serial_closed && let Some(peer) = self.controllers[at].link(handle).map(|link| link.peer) {
			self.serial_closed(at, &peer, Error::Closed);
		}
		for (channel, happened) in opp_events {
			self.opp_rfcomm(at, handle, channel, happened);
		}
	}

	// ONE SDU ON AN ENHANCED RETRANSMISSION CHANNEL: segmented, numbered and kept until the peer acknowledges it.
	pub(crate) fn ertm_send(&mut self, at: usize, handle: u16, cid: u16, sdu: &[u8]) {
		let now = now_ms();
		let outs = match self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).and_then(|classic| classic.ertm.iter_mut().find(|(held, _)| *held == cid)) {
			Some((_, ertm)) => ertm.send(sdu, now),
			None => return,
		};
		self.run_ertm(at, handle, cid, outs);
	}

	// Bytes to the peer on one open DLC.
	pub(crate) fn rfcomm_send(&mut self, at: usize, handle: u16, server_channel: u8, bytes: &[u8]) {
		let outs = match self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).and_then(|classic| classic.rfcomm.as_mut()) {
			Some(rfcomm) => rfcomm.session.write(server_channel, bytes).unwrap_or_default(),
			None => return,
		};
		self.run_rfcomm(at, handle, outs);
	}

	// ------------------------------------------------------------------ profiles

	// THE OPERATOR'S CONNECT: the link, its encryption, then the profile's own steps. Only a bonded peer.
	pub(crate) fn connect_profile(&mut self, at: usize, peer: &Peer, profile: Profile) -> Result<(), Error> {
		if !peer_is_classic(peer) {
			return Err(Error::Unsupported);
		}
		if !matches!(profile, Profile::Spp | Profile::Input | Profile::Audio | Profile::Pan) {
			return Err(Error::Unsupported);
		}
		if self.record(at, peer).is_none() {
			return Err(Error::NotFound);
		}
		let controller = &mut self.controllers[at];
		if !controller.powered || !controller.classic {
			return Err(Error::Closed);
		}
		if profile == Profile::Input {
			// THIS HOST OPENS THE HID CHANNELS ITSELF once the descriptor is known.
			if let Some(classic) = controller.link_to_mut(peer).and_then(|link| link.classic.as_mut()) {
				classic.hid.outgoing = true;
			}
		}
		match controller.link_to(peer).map(|link| (link.handle, link.encrypted)) {
			Some((handle, true)) => self.start_profile(at, handle, profile),
			Some(_) => controller.bredr_state.wants.push((*peer, Want::Profile(profile))),
			None => {
				self.page(at, *peer)?;
				self.controllers[at].bredr_state.wants.push((*peer, Want::Profile(profile)));
			}
		}
		Ok(())
	}

	pub(crate) fn disconnect_profile(&mut self, at: usize, peer: &Peer, profile: Profile) -> Result<(), Error> {
		if profile == Profile::Pan {
			return self.disconnect_pan(at, peer);
		}
		let controller = &mut self.controllers[at];
		let Some(handle) = controller.link_to(peer).map(|link| link.handle) else { return Err(Error::NotFound) };
		let Some(classic) = controller.link_mut(handle).and_then(|link| link.classic.as_mut()) else { return Err(Error::Unsupported) };
		let Some(rfcomm) = classic.rfcomm.as_mut() else { return Err(Error::NotFound) };
		let Some(&(server_channel, _)) = rfcomm.open.iter().find(|(_, held)| *held == profile) else { return Err(Error::NotFound) };
		let outs = rfcomm.session.disconnect(server_channel);
		self.run_rfcomm(at, handle, outs);
		Ok(())
	}

	// THE DEVICE'S INPUT IS UP: its gamepads are told to the streams already open, and it counts as connected for input.
	fn hid_ready(&mut self, at: usize, handle: u16) {
		let Some(link) = self.controllers[at].link_mut(handle) else { return };
		let arrivals = link.decoder.as_ref().map(input::Decoder::arrivals).unwrap_or_default();
		deliver(link, &arrivals);
		if let Some(classic) = link.classic.as_mut()
			&& classic.hid.interrupt.is_some()
		{
			classic.profiles = classic.profiles.with(bt_policy::Profile::Input, true);
		}
	}

	// A profile's first step on an encrypted link: the SDP search that finds its transport.
	fn start_profile(&mut self, at: usize, handle: u16, profile: Profile) {
		let uuid = match profile {
			Profile::Spp => sdp::uuid::SERIAL_PORT,
			Profile::Input => sdp::uuid::HUMAN_INTERFACE_DEVICE,
			Profile::Audio => sdp::uuid::AUDIO_SINK,
			Profile::Pan => sdp::uuid::NAP,
			_ => return,
		};
		if profile == Profile::Input
			&& let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut())
		{
			classic.hid.searched = true;
		}
		self.start_search(at, handle, uuid, Purpose::Profile(profile));
	}

	// ONE SEARCH OF THE PEER'S RECORDS for a class, and what it is for. One at a time on a link.
	pub(crate) fn start_search(&mut self, at: usize, handle: u16, uuid: u16, purpose: Purpose) {
		let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) else { return };
		if classic.sdp_client.is_some() {
			print(b"BluetoothService: an SDP search is already running on the link; the connection is refused\n");
			return;
		}
		let Some((cid, signal)) = classic.channels.open(psm::SDP, false) else { return };
		classic.sdp_client = Some(SdpClient { cid, search: None, uuid, purpose });
		self.send_signal(at, handle, &signal);
	}
}

// The report descriptor in a HID record's descriptor list: a sequence of (class, bytes) pairs, the report class's.
fn hid_descriptor(record: &sdp::Record) -> Option<Vec<u8>> {
	let list = record.get(HID_DESCRIPTOR_LIST)?.elements()?;
	list.iter().find_map(|pair| {
		let pair = pair.elements()?;
		if pair.first()?.uint()? != REPORT_DESCRIPTOR {
			return None;
		}
		match pair.get(1)? {
			sdp::Element::Text(bytes) => Some(bytes.clone()),
			_ => None,
		}
	})
}
