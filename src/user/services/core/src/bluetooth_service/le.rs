// THE LE HALF BEYOND ONE MOUSE: this host's privacy, the bonded peripherals' reconnection, and every LE pairing
// model through the prompts.
//
// PRIVACY BOTH WAYS. A bonded peer that rotates its address is known by the identity resolving key it gave at
// bonding: the controller's resolving list resolves it on connection, and this host resolves what a scan hears.
// This host's own address is a resolvable private address made from ITS identity resolving key - kept in the bond
// store beside the bonds, as the one record that names this host itself - and rotated every fifteen minutes; the
// public address is given only to a bonded peer, as the identity it resolves to.
//
// RECONNECTION THROUGH THE FILTER ACCEPT LIST: this host never advertises, so a bonded peripheral trusted for input
// reconnects by advertising and this host connecting to whichever device on the list it hears - one standing
// connection attempt, set aside while the operator pairs something else and taken up again after.
//
// EVERY LE MODEL A PERSON ANSWERS is a prompt on the same watcher as BR/EDR's: Numeric Comparison, Passkey Entry
// either way, and Just Works; LE legacy pairing only on `pair-legacy`, never over a Secure Connections bond.

use super::*;
use service_logic::bt_pairing::{Question, Reply};
use service_logic::hci_bredr::KeyType;
use service_logic::smp_pairing::{self, Answer, Bonded, Options};

// How long one private address is used.
const ROTATE_TICKS: u64 = 15 * 60 * TICKS_PER_SECOND;
// The LE IO capabilities this host declares.
const IO_KEYBOARD_DISPLAY: u8 = smp_pairing::IO_KEYBOARD_DISPLAY;
const IO_NO_INPUT_NO_OUTPUT: u8 = smp_pairing::IO_NO_INPUT_NO_OUTPUT;
// Own address types LE commands take: public, and random - this host's private address.
const OWN_PUBLIC: u8 = 0;
const OWN_RANDOM: u8 = 1;

// The LE privacy and reconnection state a controller keeps.
pub(crate) struct LeState {
	// This host's identity resolving key, and the private address made from it.
	pub irk: Option<[u8; 16]>,
	pub own: Option<[u8; 6]>,
	pub rotate_at: u64,
	// Bonded peers' identity resolving keys, by identity.
	pub irks: Vec<(Peer, [u8; 16])>,
	// The standing connection attempt through the accept list is out.
	pub accepting: bool,
	// The peer the operator asked a legacy pairing for.
	pub legacy: Option<Peer>,
	// The standing attempt was cancelled because the bonds it waits for changed: when its cancellation lands, it is
	// taken up again with the new list.
	pub relist: bool,
}

impl LeState {
	pub fn new() -> LeState {
		LeState { irk: None, own: None, rotate_at: 0, irks: Vec::new(), accepting: false, legacy: None, relist: false }
	}

	// THE ADDRESS THIS HOST USES on a new LE link, as SMP's derivations take it.
	pub fn local(&self, public: Peer) -> Peer {
		match self.own {
			Some(address) => {
				let mut out = [0u8; 7];
				out[0] = KIND_RANDOM;
				out[1..].copy_from_slice(&address);
				out
			}
			None => public,
		}
	}

	pub fn own_type(&self) -> u8 {
		if self.own.is_some() { OWN_RANDOM } else { OWN_PUBLIC }
	}

	pub fn resolve(&self, address: &[u8; 6]) -> Option<Peer> {
		self.irks.iter().find(|(_, irk)| service_logic::bt_keys::resolves(irk, address)).map(|(identity, _)| *identity)
	}

	pub fn end_session(&mut self) {
		self.own = None;
		self.accepting = false;
	}
}

fn question_of(question: smp_pairing::Question) -> Question {
	match question {
		smp_pairing::Question::Compare(value) => Question::Compare(value),
		smp_pairing::Question::ShowPasskey(value) => Question::ShowPasskey(value),
		smp_pairing::Question::EnterPasskey => Question::EnterPasskey,
	}
}

impl Stack {
	// THIS HOST'S IDENTITY RESOLVING KEY: read from the record that names this host itself, or made once and kept there.
	pub(crate) fn own_irk(&mut self, at: usize) -> Option<[u8; 16]> {
		if let Some(irk) = self.controllers[at].le.irk {
			return Some(irk);
		}
		let local = local_address(&self.controllers[at]);
		if let Some(mut record) = self.bond(at, &local) {
			let irk = (record.irk.len() == 16).then(|| {
				let mut irk = [0u8; 16];
				irk.copy_from_slice(&record.irk);
				irk
			});
			scrub_record(&mut record);
			self.controllers[at].le.irk = irk;
			return irk;
		}
		let (mut irk, mut filler) = ([0u8; 16], [0u8; 16]);
		if random_get(&mut irk) != 16 || random_get(&mut filler) != 16 {
			return None;
		}
		let wire = local_wire(&self.controllers[at]);
		let level = Level { agreement: Agreement::SecureConnections, authenticated: true };
		let mut record = BondRecord { version: service_logic::bond_store::VERSION, local: wire.clone(), peer: wire, key: filler.to_vec(), security: SecurityLevel::EncryptedAuthenticated, name: String::from("this host"), enabled: false, radio: Radio::Le, link_key: Vec::new(), link_key_type: 0, level: level_to_wire(&level), trusted: Vec::new(), alias: String::new(), irk: irk.to_vec(), ediv: 0, rand: Vec::new() };
		let stored = self.store_bond(&record);
		scrub_record(&mut record);
		scrub(&mut filler);
		if !stored {
			scrub(&mut irk);
			return None;
		}
		self.controllers[at].le.irk = Some(irk);
		Some(irk)
	}

	// A READY CONTROLLER'S LE SIDE: its private address, the bonded peers' resolving keys, and the reconnection.
	pub(crate) fn le_ready(&mut self, at: usize) {
		if self.own_irk(at).is_some() {
			self.rotate_address(at);
		}
		self.load_irks(at);
		self.reconnect_bonded(at);
	}

	fn load_irks(&mut self, at: usize) {
		let mut irks = Vec::new();
		for record in self.records(at).into_iter().filter(|record| record.radio == Radio::Le) {
			let Some(peer) = peer_from_wire(&record.peer) else { continue };
			let Some(mut keyed) = self.bond(at, &peer) else { continue };
			if keyed.irk.len() == 16 {
				let mut irk = [0u8; 16];
				irk.copy_from_slice(&keyed.irk);
				irks.push((peer, irk));
			}
			scrub_record(&mut keyed);
		}
		self.controllers[at].le.irks = irks;
	}

	pub(crate) fn rotate_address(&mut self, at: usize) {
		let Some(irk) = self.controllers[at].le.irk else { return };
		let mut random = [0u8; 4];
		if random_get(&mut random) != 4 {
			return;
		}
		let address = service_logic::bt_keys::private_address(&irk, u32::from_le_bytes(random));
		let controller = &mut self.controllers[at];
		if controller.command(opcode::LE_SET_RANDOM_ADDRESS, &hci_codec::random_address(&address)) {
			controller.le.own = Some(address);
			controller.le.rotate_at = clock().saturating_add(ROTATE_TICKS);
		}
	}

	// THE STANDING ATTEMPT: every bonded LE peer trusted for input and not connected on the accept list, those with a
	// resolving key on the resolving list too, and one connection attempt through the list.
	pub(crate) fn reconnect_bonded(&mut self, at: usize) {
		let controller = &self.controllers[at];
		if !controller.powered || controller.le.accepting || controller.reconnect.is_some() || controller.attempt.as_ref().is_some_and(|attempt| attempt.peer[0] != KIND_BREDR && attempt.state == PairingState::Connecting) {
			return;
		}
		let local = local_address(controller);
		let wanted: Vec<Peer> = self.records(at).into_iter().filter(|record| record.radio == Radio::Le && (record.enabled || record.trusted.contains(&Profile::Gatt) || record.trusted.contains(&Profile::Audio))).filter_map(|record| peer_from_wire(&record.peer)).filter(|peer| *peer != local && self.controllers[at].link_to(peer).is_none()).collect();
		if wanted.is_empty() || self.controllers[at].le_links() >= bt_bounds::LINKS_PER_CONTROLLER {
			return;
		}
		let own_irk = self.controllers[at].le.irk.unwrap_or([0; 16]);
		let irks = self.controllers[at].le.irks.clone();
		let controller = &mut self.controllers[at];
		controller.command(opcode::LE_SET_ADDRESS_RESOLUTION_ENABLE, &[0]);
		controller.command(opcode::LE_CLEAR_FILTER_ACCEPT_LIST, &[]);
		controller.command(opcode::LE_CLEAR_RESOLVING_LIST, &[]);
		for peer in &wanted {
			let mut address = [0u8; 6];
			address.copy_from_slice(&peer[1..]);
			controller.command(opcode::LE_ADD_DEVICE_TO_FILTER_ACCEPT_LIST, &hci_codec::accept_list_entry(peer[0], &address));
			if let Some((_, irk)) = irks.iter().find(|(held, _)| held == peer) {
				let mut entry = hci_codec::resolving_list_entry(peer[0], &address, irk, &own_irk);
				controller.command(opcode::LE_ADD_DEVICE_TO_RESOLVING_LIST, &entry);
				scrub(&mut entry);
			}
		}
		controller.command(opcode::LE_SET_ADDRESS_RESOLUTION_ENABLE, &[1]);
		let own = controller.le.own_type();
		if controller.le_create_connection(&hci_codec::create_connection_accepted(own)) {
			controller.le.accepting = true;
		}
	}

	// THE ACCEPT LIST FOLLOWS THE BONDS: a peer trusted, no longer trusted or forgotten changes what the standing attempt
	// waits for, so an attempt that is out is cancelled and taken up again with the new list once its cancellation
	// lands - not at once, since the cancelled attempt's own completion is still to come and would end the new one.
	pub(crate) fn relist(&mut self, at: usize) {
		let controller = &mut self.controllers[at];
		if controller.le.accepting {
			if !controller.le.relist && controller.command(opcode::LE_CREATE_CONNECTION_CANCEL, &[]) {
				controller.le.relist = true;
			}
			return;
		}
		self.reconnect_bonded(at);
	}

	// SET THE STANDING ATTEMPT ASIDE for one the operator asked for; it is taken up again when that one ends.
	fn set_aside(&mut self, at: usize) {
		let controller = &mut self.controllers[at];
		if controller.le.accepting {
			controller.command(opcode::LE_CREATE_CONNECTION_CANCEL, &[]);
			controller.le.accepting = false;
		}
	}

	// THE OPERATOR'S LE PAIRING: a connection to what the scan heard, from this host's private address.
	pub(crate) fn pair_le(&mut self, at: usize, peer: Peer, legacy: bool) -> Result<(), Error> {
		let held = self.record(at, &peer).map(|record| level_from_wire(&record.level));
		let controller = &self.controllers[at];
		if legacy {
			// LE LEGACY ONLY FOR A DEVICE THAT CANNOT DO BETTER, and never over a Secure Connections bond.
			if held.is_some_and(|held| held.agreement == Agreement::SecureConnections) {
				return Err(Error::Denied);
			}
		} else if !controller.secure_connections || controller.public_key.is_none() {
			return Err(Error::Unsupported);
		}
		if controller.link_to(&peer).is_some() || controller.links.len() >= bt_bounds::LINKS_PER_CONTROLLER || controller.reconnect.is_some() {
			return Err(Error::Again);
		}
		self.set_aside(at);
		let controller = &mut self.controllers[at];
		let mut address = [0u8; 6];
		address.copy_from_slice(&peer[1..]);
		let own = controller.le.own_type();
		if !controller.le_create_connection(&hci_codec::create_connection_from(peer[0], &address, own)) {
			return Err(Error::Exhausted);
		}
		controller.le.legacy = legacy.then_some(peer);
		controller.attempt = Some(Attempt { peer, deadline: clock().saturating_add(PAIRING_TICKS), state: PairingState::Connecting, security: SecurityLevel::None });
		Ok(())
	}

	// THE BR/EDR LINK KEY DERIVED FROM AN LE SECURE CONNECTIONS PAIRING, kept as the device's BR/EDR bond where its
	// identity is a public address - the only kind a BR/EDR device has - at the LTK's level, never over a better
	// BR/EDR bond. Its type is the P-256 one its level is: authenticated or not.
	fn store_derived_link_key(&mut self, at: usize, identity: Peer, link_key: &mut [u8; 16], level: Level, name: String, alias: String) {
		if identity[0] != KIND_PUBLIC || !self.controllers[at].classic {
			scrub(link_key);
			return;
		}
		let mut peer = identity;
		peer[0] = KIND_BREDR;
		if let Some(held) = self.record(at, &peer)
			&& !level.may_replace(&level_from_wire(&held.level))
		{
			print(b"BluetoothService: the BR/EDR key derived from the LE pairing is below the BR/EDR bond already held; it is not kept\n");
			scrub(link_key);
			return;
		}
		let kind = if level.authenticated { KeyType::AuthenticatedP256 } else { KeyType::UnauthenticatedP256 };
		let local = local_wire(&self.controllers[at]);
		let mut record = BondRecord { version: service_logic::bond_store::VERSION, local, peer: peer_to_wire(&peer), key: Vec::new(), security: security_of(&level), name, enabled: false, radio: Radio::Classic, link_key: link_key.to_vec(), link_key_type: kind.value(), level: level_to_wire(&level), trusted: Vec::new(), alias, irk: Vec::new(), ediv: 0, rand: Vec::new() };
		let stored = self.store_bond(&record);
		scrub_record(&mut record);
		scrub(link_key);
		if stored {
			print(b"BluetoothService: a BR/EDR key was derived from the LE pairing and kept at its level\n");
			self.refresh_policy(at);
		}
	}

	pub(crate) fn start_le_pairing(&mut self, at: usize, handle: u16) {
		let irk = self.own_irk(at).unwrap_or([0; 16]);
		let controller = &mut self.controllers[at];
		// HEALTHY SYSTEM RANDOMNESS OR NO PAIRING: every nonce, the passkey and legacy's random value come from it.
		let mut seed = [0u8; 16];
		if random_get(&mut seed) != seed.len() {
			print(b"BluetoothService: no healthy random source; pairing is refused\n");
			controller.fail_attempt(&controller.link(handle).map(|link| link.peer).unwrap_or_default());
			controller.disconnect(handle, REASON_AUTHENTICATION);
			return;
		}
		let watcher = controller.bredr_state.watcher != 0;
		let identity = local_address(controller);
		let public_key = controller.public_key;
		let Some(peer) = controller.link(handle).map(|link| link.peer) else { return };
		let legacy = controller.le.legacy == Some(peer);
		// A DUAL-MODE CONTROLLER WITH SECURE CONNECTIONS ON BR/EDR asks for the BR/EDR key to be derived as well.
		let cross_transport = controller.classic && controller.classic_sc;
		let options = Options { io: if watcher { IO_KEYBOARD_DISPLAY } else { IO_NO_INPUT_NO_OUTPUT }, legacy, irk, identity, cross_transport };
		let Some(link) = controller.link_mut(handle) else { return };
		let (pairing, first) = smp_pairing::Initiator::start(link.local, link.peer, if legacy { None } else { public_key }, seed, options);
		seed.fill(0);
		link.pairing = Some(pairing);
		if let Some(attempt) = controller.attempt.as_mut() {
			attempt.state = PairingState::Pairing;
		}
		self.run_smp(at, handle, alloc::vec![first]);
	}

	pub(crate) fn run_smp(&mut self, at: usize, handle: u16, mut steps: Vec<smp_pairing::Step>) {
		use smp_pairing::Step;
		for step in steps.iter_mut() {
			let controller = &mut self.controllers[at];
			match step {
				Step::Send(pdu) => {
					controller.l2cap(handle, SMP_CID, pdu);
				}
				Step::GenerateDhKey(key) => {
					controller.command(opcode::LE_GENERATE_DHKEY, &hci_codec::generate_dhkey(key));
				}
				// The key rides in the step list, which is freed after this loop: zeroed in place first.
				Step::Encrypt(key) => {
					controller.encrypt(handle, key);
					scrub(key);
				}
				Step::Ask(question) => {
					let Some(peer) = controller.link(handle).map(|link| link.peer) else { continue };
					let question = question_of(*question);
					self.raise(at, peer, question);
				}
				Step::Bonded(bonded) => {
					self.le_bonded(at, handle, bonded);
				}
				Step::Failed(_) => {
					if let Some(attempt) = controller.attempt.as_mut() {
						attempt.state = PairingState::Failed;
					}
					controller.disconnect(handle, REASON_AUTHENTICATION);
				}
			}
		}
	}

	// A PERSON'S ANSWER to an LE prompt, to the pairing that asked it.
	pub(crate) fn le_act(&mut self, at: usize, peer: Peer, reply: &Reply) {
		let answer = match reply {
			Reply::Yes => Answer::Yes,
			Reply::Passkey(value) => Answer::Passkey(*value),
			_ => Answer::No,
		};
		let Some(handle) = self.controllers[at].link_to(&peer).map(|link| link.handle) else { return };
		let steps = match self.controllers[at].link_mut(handle).and_then(|link| link.pairing.as_mut()) {
			Some(pairing) => pairing.answer(answer),
			None => return,
		};
		self.run_smp(at, handle, steps);
	}

	// THE BOND, once the keys are distributed: kept under the peer's identity where it gave one, at the level the
	// model earned, and never over a better one. BONDED ONLY AFTER THE DURABLE COMMIT.
	fn le_bonded(&mut self, at: usize, handle: u16, bonded: &mut Bonded) {
		let Some(link_peer) = self.controllers[at].link(handle).map(|link| link.peer) else { return };
		let identity = bonded.identity.unwrap_or(link_peer);
		let level = Level { agreement: if bonded.secure_connections { Agreement::SecureConnections } else { Agreement::LeLegacy }, authenticated: bonded.authenticated };
		let held = self.record(at, &identity);
		if let Some(held) = held.as_ref()
			&& !level.may_replace(&level_from_wire(&held.level))
		{
			print(b"BluetoothService: a bonded LE peer paired again at a lower level; the bond is kept and the link refused\n");
			scrub(&mut bonded.ltk);
			self.encryption_failed(at, handle);
			return;
		}
		let wire = peer_to_wire(&link_peer);
		let advertised = self.controllers[at].scan.as_ref().and_then(|scan| scan.results.iter().find(|result| result.address == wire)).map(|result| result.name.clone()).unwrap_or_default();
		let (enabled, trusted, alias, name) = held.map(|held| (held.enabled, held.trusted, held.alias, if held.name.is_empty() { advertised.clone() } else { held.name })).unwrap_or((false, Vec::new(), String::new(), advertised));
		let local = local_wire(&self.controllers[at]);
		let mut record = BondRecord { version: service_logic::bond_store::VERSION, local, peer: peer_to_wire(&identity), key: bonded.ltk.to_vec(), security: security_of(&level), name, enabled, radio: Radio::Le, link_key: Vec::new(), link_key_type: 0, level: level_to_wire(&level), trusted, alias, irk: bonded.irk.map(|irk| irk.to_vec()).unwrap_or_default(), ediv: bonded.ediv, rand: if bonded.secure_connections { Vec::new() } else { bonded.rand.to_vec() } };
		let derived_names = bonded.link_key.is_some().then(|| (record.name.clone(), record.alias.clone()));
		let stored = self.store_bond(&record);
		scrub_record(&mut record);
		scrub(&mut bonded.ltk);
		if let Some(mut link_key) = bonded.link_key.take() {
			match derived_names {
				Some((name, alias)) if stored => self.store_derived_link_key(at, identity, &mut link_key, level, name, alias),
				_ => scrub(&mut link_key),
			}
		}
		let controller = &mut self.controllers[at];
		if let Some(link) = controller.link_mut(handle) {
			// The keys have done their work in this process; the store holds the copies that outlive it.
			link.pairing = None;
			link.peer = identity;
			link.security = security_of(&level);
		}
		if !stored {
			print(b"BluetoothService: the bond could not be stored durably; the pairing is abandoned\n");
			controller.fail_attempt(&link_peer);
			controller.disconnect(handle, REASON_AUTHENTICATION);
			return;
		}
		if let Some(attempt) = controller.attempt.as_mut()
			&& (attempt.peer == link_peer || attempt.peer == identity)
		{
			attempt.state = PairingState::Bonded;
			attempt.security = security_of(&level);
			attempt.peer = identity;
		}
		controller.le.legacy = None;
		if let Some(irk) = bonded.irk {
			controller.le.irks.retain(|(held, _)| *held != identity);
			controller.le.irks.push((identity, irk));
		}
		if self.enabled_peer(at, &identity) == Some(true) {
			self.start_discovery(at, handle);
		}
		self.battery_read(at, handle);
		// A COORDINATED SET'S MEMBER, bonded as the set's: trusted for audio, and walked.
		self.le_audio_bonded(at, &link_peer, &identity);
		self.le_audio_start(at, handle);
		self.reconnect_bonded(at);
	}
}
