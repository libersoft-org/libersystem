// TETHERING: this host as a PAN user of a bonded peer's network access point, over BNEP, and the link offered to
// NetworkService as a NIC is - a frame channel speaking a NIC provider's frame-plus-link wire.
//
// THE OPERATOR CONNECTS, NETWORKSERVICE DECIDES. `btctl connect ADDRESS pan [replace]` pages, secures the link, finds
// the peer's NAP record and opens BNEP's L2CAP channel; the access point's answer to the setup offers the link on the
// `bluetooth-network` root's stream, with whether it may replace a selected uplink. NetworkService - the one
// subscriber, resolving the root by name through the broker - opens it, and from then on Ethernet frames cross: what
// NetworkService writes is sent in BNEP's most compressed form its addresses allow (`service_logic::bnep`), and what
// the peer sends is handed back whole.
//
// THE LINK'S MAC IS THE CONTROLLER'S ADDRESS, as BNEP addresses a PAN user, and its MTU Ethernet's 1500 bytes.

use super::*;
use proto::system::{NetworkEvent, NetworkLink, bluetooth_network};
use service_logic::bnep::{self, Panu, Received};
use service_logic::l2cap_bredr::psm;

// What the links stream holds for a subscriber that has not read it.
const LINKS_DEPTH: usize = 16;

pub(crate) struct Pan {
	pub cid: u16,
	pub panu: Option<Panu>,
	pub replace: bool,
	// The id the link is offered under, once its setup is answered.
	pub id: Option<u32>,
	// This service's end of the frame channel NetworkService opened, zero until it does.
	pub chan: u64,
}

pub(crate) struct NetworkRoot {
	// The producer end of the one subscriber's links stream, zero with none.
	pub subscriber: u64,
	pub next_id: u32,
	// What the operator asked of a connection not yet made: whether it may replace a selected uplink.
	pub wanted: Vec<(Peer, bool)>,
}

impl NetworkRoot {
	pub const fn new() -> NetworkRoot {
		NetworkRoot { subscriber: 0, next_id: 1, wanted: Vec::new() }
	}

	fn send(&mut self, event: &NetworkEvent) {
		if self.subscriber == 0 {
			return;
		}
		let mut frame = [0u8; 256];
		let mut handles = wire::Handles::new();
		let Some(len) = bluetooth_network::links_frame(0, event, &mut frame, &mut handles) else { return };
		if matches!(try_send_outcome(self.subscriber, &frame[..len], 0), SendOutcome::Failed) {
			close(self.subscriber);
			self.subscriber = 0;
		}
	}
}

impl Stack {
	fn pan(&mut self, at: usize, handle: u16) -> Option<&mut Pan> {
		self.controllers[at].link_mut(handle)?.classic.as_mut()?.pan.as_mut()
	}

	// THE OPERATOR'S CONNECT, with whether the link may replace a selected uplink.
	pub(crate) fn connect_pan(&mut self, at: usize, peer: &Peer, replace: bool) -> Result<(), Error> {
		self.network.wanted.retain(|(held, _)| held != peer);
		self.network.wanted.push((*peer, replace));
		self.connect_profile(at, peer, Profile::Pan)
	}

	// THE PEER'S NAP RECORD FOUND: BNEP's channel, at the MTU a whole Ethernet frame needs.
	pub(crate) fn pan_found(&mut self, at: usize, handle: u16, records: &[sdp::Record]) {
		if records.is_empty() {
			print(b"BluetoothService: the device offers no network access point\n");
			return;
		}
		let Some(link) = self.controllers[at].link_mut(handle) else { return };
		let peer = link.peer;
		let Some(classic) = link.classic.as_mut() else { return };
		if classic.pan.is_some() {
			return;
		}
		let Some((cid, signal)) = classic.channels.open(psm::BNEP, false) else {
			print(b"BluetoothService: no L2CAP channel left on the link for BNEP\n");
			return;
		};
		let replace = self.network.wanted.iter().find(|(held, _)| *held == peer).is_some_and(|(_, replace)| *replace);
		self.network.wanted.retain(|(held, _)| *held != peer);
		if let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) {
			classic.pan = Some(Pan { cid, panu: None, replace, id: None, chan: 0 });
		}
		self.send_signal(at, handle, &signal);
	}

	// BNEP'S CHANNEL IS OPEN: the setup, NAP asked of a PANU.
	pub(crate) fn pan_opened(&mut self, at: usize, handle: u16, cid: u16) {
		let local = self.controllers[at].address;
		let Some(peer) = self.controllers[at].link(handle).map(|link| link.peer) else { return };
		let mut remote = [0u8; 6];
		remote.copy_from_slice(&peer[1..]);
		let Some(pan) = self.pan(at, handle).filter(|pan| pan.cid == cid) else { return };
		let (panu, request) = Panu::new(local, remote);
		pan.panu = Some(panu);
		self.send_on(at, handle, cid, &request);
	}

	// A BNEP PACKET FROM THE ACCESS POINT.
	pub(crate) fn pan_data(&mut self, at: usize, handle: u16, cid: u16, payload: &[u8]) {
		let Some(pan) = self.pan(at, handle).filter(|pan| pan.cid == cid) else { return };
		let Some(panu) = pan.panu.as_mut() else { return };
		let received = match panu.receive(payload) {
			Ok(received) => received,
			Err(_) => {
				print(b"BluetoothService: a BNEP packet from the access point was refused\n");
				return;
			}
		};
		match received {
			Received::Setup(bnep::State::Open) => self.pan_offer(at, handle),
			Received::Setup(_) => {
				print(b"BluetoothService: the access point refused the PAN setup\n");
				self.close_channel(at, handle, cid);
			}
			Received::Frame(frame) => {
				let chan = pan.chan;
				if chan != 0 && matches!(try_send_outcome(chan, &frame, 0), SendOutcome::Failed) {
					close(chan);
					if let Some(pan) = self.pan(at, handle) {
						pan.chan = 0;
					}
				}
			}
			Received::Reply(reply) => self.send_on(at, handle, cid, &reply),
			Received::Nothing => {}
		}
	}

	// THE LINK IS UP: offered to NetworkService, and connected for PAN.
	fn pan_offer(&mut self, at: usize, handle: u16) {
		let Some(link) = self.controllers[at].link(handle) else { return };
		let peer = link.peer;
		let name = self.controllers[at].bredr_state.name_of(&peer).unwrap_or_default();
		let id = self.network.next_id;
		self.network.next_id = self.network.next_id.wrapping_add(1).max(1);
		let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) else { return };
		let Some(pan) = classic.pan.as_mut() else { return };
		pan.id = Some(id);
		let replace = pan.replace;
		classic.profiles = classic.profiles.with(bt_policy::Profile::Pan, true);
		self.network.send(&NetworkEvent::Arrived(NetworkLink { id, peer: peer_to_wire(&peer), name, replace_uplink: replace }));
		print(b"BluetoothService: a PAN link is offered to NetworkService\n");
	}

	// THE LINK'S CHANNEL CLOSED, or the link: the offer withdrawn, NetworkService's channel with it.
	pub(crate) fn pan_closed(&mut self, at: usize, handle: u16, cid: u16) {
		let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) else { return };
		if classic.pan.as_ref().is_none_or(|pan| pan.cid != cid) {
			return;
		}
		let Some(pan) = classic.pan.take() else { return };
		classic.profiles = classic.profiles.with(bt_policy::Profile::Pan, false);
		self.pan_withdraw(pan);
	}

	pub(crate) fn pan_link_gone(&mut self, link: &mut Link) {
		if let Some(pan) = link.classic.as_mut().and_then(|classic| classic.pan.take()) {
			self.pan_withdraw(pan);
		}
	}

	fn pan_withdraw(&mut self, pan: Pan) {
		if pan.chan != 0 {
			close(pan.chan);
		}
		if let Some(id) = pan.id {
			self.network.send(&NetworkEvent::Departed(id));
			print(b"BluetoothService: a PAN link is withdrawn from NetworkService\n");
		}
	}

	// THE OPERATOR'S DISCONNECT.
	pub(crate) fn disconnect_pan(&mut self, at: usize, peer: &Peer) -> Result<(), Error> {
		let handle = self.controllers[at].link_to(peer).map(|link| link.handle).ok_or(Error::NotFound)?;
		let cid = self.pan(at, handle).map(|pan| pan.cid).ok_or(Error::NotFound)?;
		self.close_channel(at, handle, cid);
		self.pan_closed(at, handle, cid);
		Ok(())
	}

	// ------------------------------------------------------------------ NetworkService's frames

	// The frame channels NetworkService holds, for the wait.
	pub(crate) fn pan_channels(&self) -> Vec<u64> {
		let mut out = Vec::new();
		for controller in &self.controllers {
			for link in &controller.links {
				if let Some(pan) = link.classic.as_ref().and_then(|classic| classic.pan.as_ref()).filter(|pan| pan.chan != 0) {
					out.push(pan.chan);
				}
			}
		}
		out
	}

	// FRAMES NETWORKSERVICE WROTE, each sent to the access point in BNEP's form; a closed channel is let go.
	pub(crate) fn serve_pan(&mut self, chan: u64, buf: &mut [u8]) {
		let mut found = None;
		for (at, controller) in self.controllers.iter().enumerate() {
			if let Some(link) = controller.links.iter().find(|link| link.classic.as_ref().and_then(|classic| classic.pan.as_ref()).is_some_and(|pan| pan.chan == chan)) {
				found = Some((at, link.handle));
			}
		}
		let Some((at, handle)) = found else { return };
		loop {
			let len = match try_recv_caps(chan, buf) {
				PolledCaps::Message { len, handles } => {
					for &leftover in handles.as_slice() {
						close(leftover);
					}
					len
				}
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					close(chan);
					if let Some(pan) = self.pan(at, handle) {
						pan.chan = 0;
					}
					return;
				}
			};
			let Some(pan) = self.pan(at, handle) else { return };
			let cid = pan.cid;
			let Some(packet) = pan.panu.as_ref().and_then(|panu| panu.send(&buf[..len])) else { continue };
			self.send_on(at, handle, cid, &packet);
		}
	}
}

// THE SUBSCRIBER'S CALLS.
pub(crate) struct NetworkView<'a> {
	pub stack: &'a mut Stack,
}

impl bluetooth_network::Service for NetworkView<'_> {
	// Validated here; the stream itself is made by `serve_links`, which owns the channel.
	fn links(&mut self) -> Result<Vec<NetworkEvent>, Error> {
		if self.stack.network.subscriber != 0 {
			return Err(Error::Again);
		}
		let mut snapshot = Vec::new();
		for controller in &self.stack.controllers {
			for link in &controller.links {
				if let Some((id, replace)) = link.classic.as_ref().and_then(|classic| classic.pan.as_ref()).and_then(|pan| pan.id.map(|id| (id, pan.replace))) {
					let name = controller.bredr_state.name_of(&link.peer).unwrap_or_default();
					snapshot.push(NetworkEvent::Arrived(NetworkLink { id, peer: peer_to_wire(&link.peer), name, replace_uplink: replace }));
				}
			}
		}
		Ok(snapshot)
	}

	// THE FRAME CHANNEL: one holder, greeted at once with the link's MAC - the controller's address - and its MTU.
	fn open(&mut self, id: u32) -> Result<u64, Error> {
		let mut found = None;
		for (at, controller) in self.stack.controllers.iter().enumerate() {
			if let Some(link) = controller.links.iter().find(|link| link.classic.as_ref().and_then(|classic| classic.pan.as_ref()).is_some_and(|pan| pan.id == Some(id))) {
				found = Some((at, link.handle));
			}
		}
		let (at, handle) = found.ok_or(Error::NotFound)?;
		let mac = self.stack.controllers[at].address;
		let pan = self.stack.pan(at, handle).ok_or(Error::NotFound)?;
		if pan.chan != 0 {
			return Err(Error::Again);
		}
		let (mine, theirs) = channel_with_depth(64).ok_or(Error::Exhausted)?;
		let mut greeting = b"MAC".to_vec();
		greeting.extend_from_slice(&mac);
		greeting.extend_from_slice(&(bnep::MTU as u16).to_le_bytes());
		send_blocking(mine, &greeting, 0);
		pan.chan = mine;
		Ok(theirs)
	}
}

// `links` IS A STREAM: the snapshot, then every change, on the consumer end of a fresh pair whose producer this service
// keeps.
pub(crate) fn serve_links(stack: &mut Stack, channel: u64, request: &[u8], handles: &mut wire::Handles, reply: &mut [u8]) {
	let mut view = NetworkView { stack };
	let Some((corr, result)) = bluetooth_network::links_open(&mut view, request, handles) else { return };
	let answer = match result {
		Ok(snapshot) => match channel_with_depth(LINKS_DEPTH) {
			Some((producer, consumer)) => {
				if let Some(len) = bluetooth_network::links_reply_ok(corr, reply)
					&& send_caps_blocking(channel, &reply[..len], &[consumer])
				{
					stack.network.subscriber = producer;
					for event in &snapshot {
						stack.network.send(event);
					}
					return;
				}
				close(producer);
				close(consumer);
				return;
			}
			None => Error::Exhausted,
		},
		Err(error) => error,
	};
	if let Some(len) = bluetooth_network::links_reply_err(corr, &answer, reply) {
		send_blocking(channel, &reply[..len], 0);
	}
}
