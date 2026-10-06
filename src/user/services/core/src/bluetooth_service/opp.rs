// OBJECT PUSH: OBEX over L2CAP where the peer's record offers it and over RFCOMM otherwise - pushing one object for
// `btctl send`, and receiving the one object a bonded peer pushes while `btctl receive` waits.
//
// THIS SERVICE HOLDS NO STORAGE AUTHORITY. A push's bytes arrive on the `object-push` channel the operator's `send`
// returned, written by `btctl` from the stream the shell's redirection opened; a received object's bytes leave on the
// receiver's stream, which `btctl` writes to its output. Nothing here opens a file, and the peer's name for an object
// is handed on as its words.
//
// RECEIVING IS CONSENTED BY THE CALL. While a receiver waits, its peer's page and its channel to this host's Object
// Push record are admitted (`bt_policy`'s `receive_waits_for_it`), and nobody else's; the first object that peer
// pushes is the one taken, bounded by the receiver's `max-bytes` (`service_logic::obex::PushServer`), and the receiver
// ends with it, or after 180 seconds, or when its stream is closed.

use super::*;
use proto::system::{ReceivedKind, ReceivedObject, object_push};
use service_logic::obex::{self, Assembler, ClientOut, Failure, PushClient, PushServer, ServerEvent};
use service_logic::{bt_policy, sdp};

// How long a receiver waits for the peer's object.
const RECEIVE_TICKS: u64 = 180 * TICKS_PER_SECOND;
// How long a push may take to reach the peer's Object Push server - a page, the stored key, a search and a channel.
const REACH_TICKS: u64 = 30 * TICKS_PER_SECOND;
// Pushes under way at once, across every controller.
const MAX_PUSHES: usize = 4;
// The largest piece a receiver's stream message carries.
const PIECE: usize = 1024;

// Where an OBEX session runs: an RFCOMM server channel, or an L2CAP channel in enhanced retransmission mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Transport {
	Rfcomm(u8),
	L2cap(u16),
}

pub(crate) struct Pusher {
	pub chan: u64,
	at: usize,
	peer: Peer,
	client: PushClient,
	// The CONNECT, sent once the transport is open.
	connect: Option<Vec<u8>>,
	transport: Option<Transport>,
	assembler: Assembler,
	// A `finish` waiting for the peer's answer, and the push's outcome once it is known.
	finishing: Option<Vec<u8>>,
	outcome: Option<Result<u64, Error>>,
	// When a push that has not reached the peer's server is given up.
	reach_by: Option<u64>,
}

pub(crate) struct Receiver {
	stream: u64,
	pub at: usize,
	peer: Peer,
	server: PushServer,
	transport: Option<Transport>,
	assembler: Assembler,
	deadline: u64,
	received: u64,
}

// A DEFERRED FINISH'S ANSWER, encoded by running the generated dispatch again over the result.
struct Finished(Option<Result<u64, Error>>);

impl object_push::Service for Finished {
	fn write(&mut self, _data: Vec<u8>) -> Result<u64, Error> {
		Err(Error::Invalid)
	}

	fn finish(&mut self) -> Result<u64, Error> {
		self.0.take().unwrap_or(Err(Error::Invalid))
	}

	fn abort(&mut self) -> Result<(), Error> {
		Err(Error::Invalid)
	}
}

fn answer_finish(chan: u64, request: &[u8], result: Result<u64, Error>) {
	let mut out = [0u8; 64];
	let mut handles = wire::Handles::new();
	let mut reply_handles = wire::Handles::new();
	if let Some(len) = object_push::dispatch(&mut Finished(Some(result)), request, &mut handles, &mut out, &mut reply_handles) {
		send_blocking(chan, &out[..len], 0);
	}
}

// What a failed push says to its caller.
fn error_of(failure: Failure) -> Error {
	match failure {
		Failure::Refused(_) => Error::Denied,
		Failure::Malformed => Error::Io,
		Failure::Aborted => Error::Cancelled,
	}
}

// One event on a receiver's stream; false when its holder has gone.
fn tell(stream: u64, event: &ReceivedObject) -> bool {
	let mut frame = [0u8; PIECE + 512];
	let mut handles = wire::Handles::new();
	let Some(len) = bluetooth_operator::receive_frame(0, event, &mut frame, &mut handles) else { return true };
	!matches!(send_outcome(stream, &frame[..len]), SendOutcome::Failed)
}

// A receiver's stream is read by `btctl` as it writes its output: a stream that is full is waited on briefly, and a
// holder that does not drain it in that time has gone as far as this service is concerned.
fn send_outcome(stream: u64, frame: &[u8]) -> SendOutcome {
	send_deadline(stream, frame, 0, clock() + TICKS_PER_SECOND)
}

fn event(kind: ReceivedKind, received: u64) -> ReceivedObject {
	ReceivedObject { kind, name: String::new(), object_type: String::new(), declared: None, received, bytes: Vec::new() }
}

// THE OBJECT-PUSH CHANNEL'S OWN CALLS.
struct PushView<'a> {
	stack: &'a mut Stack,
	index: usize,
}

impl object_push::Service for PushView<'_> {
	fn write(&mut self, data: Vec<u8>) -> Result<u64, Error> {
		let pusher = &mut self.stack.pushes[self.index];
		if pusher.outcome.is_some() || pusher.client.done() {
			return Err(Error::Closed);
		}
		if pusher.client.room() == 0 {
			return Err(Error::Again);
		}
		let (taken, outs) = pusher.client.write(&data);
		self.stack.run_push(self.index, outs);
		Ok(taken as u64)
	}

	// Deferred: `serve_push` answers it.
	fn finish(&mut self) -> Result<u64, Error> {
		Err(Error::Invalid)
	}

	fn abort(&mut self) -> Result<(), Error> {
		let outs = self.stack.pushes[self.index].client.abort();
		self.stack.run_push(self.index, outs);
		Ok(())
	}
}

impl Stack {
	// ------------------------------------------------------------------ pushing

	// THE OPERATOR'S PUSH: the object-push channel at once, the peer reached behind it.
	pub(crate) fn push_object(&mut self, at: usize, peer: Peer, name: &str, length: Option<u64>) -> Result<u64, Error> {
		if !peer_is_classic_peer(&peer) {
			return Err(Error::Unsupported);
		}
		if name.is_empty() || name.contains('\0') {
			return Err(Error::Invalid);
		}
		if self.record(at, &peer).is_none() {
			return Err(Error::NotFound);
		}
		if self.pushes.len() >= MAX_PUSHES {
			return Err(Error::Exhausted);
		}
		let controller = &self.controllers[at];
		if !controller.powered || !controller.classic {
			return Err(Error::Closed);
		}
		let (mine, theirs) = channel().ok_or(Error::Exhausted)?;
		let length = length.and_then(|length| u32::try_from(length).ok());
		let (client, connect) = PushClient::new(name, None, length);
		self.pushes.push(Pusher { chan: mine, at, peer, client, connect: Some(connect), transport: None, assembler: Assembler::new(obex::MAX_PACKET), finishing: None, outcome: None, reach_by: Some(clock().saturating_add(REACH_TICKS)) });
		match self.controllers[at].link_to(&peer).map(|link| (link.handle, link.encrypted)) {
			Some((handle, true)) => self.start_push_search(at, handle),
			Some(_) => self.controllers[at].bredr_state.wants.push((peer, classic::Want::Push)),
			None => match self.page(at, peer) {
				Ok(()) => self.controllers[at].bredr_state.wants.push((peer, classic::Want::Push)),
				Err(error) => {
					let index = self.pushes.len() - 1;
					self.push_failed(index, error);
				}
			},
		}
		Ok(theirs)
	}

	// ONE REQUEST ON AN OBJECT-PUSH CHANNEL.
	pub(crate) fn serve_push(&mut self, index: usize, buf: &mut [u8]) {
		let chan = self.pushes[index].chan;
		let (len, handles) = match try_recv_caps(chan, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				// ITS HOLDER WENT: an unfinished push is abandoned.
				let outs = self.pushes[index].client.abort();
				self.run_push(index, outs);
				self.end_push(index);
				return;
			}
		};
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		let request = buf[..len].to_vec();
		if request.get(..2).map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]])) == Some(object_push::OP_FINISH) {
			let pusher = &mut self.pushes[index];
			if let Some(outcome) = pusher.outcome.clone() {
				answer_finish(chan, &request, outcome);
				return;
			}
			pusher.finishing = Some(request);
			let outs = pusher.client.finish();
			self.run_push(index, outs);
			return;
		}
		let mut out = [0u8; 64];
		let mut handles = wire::Handles::new();
		let mut reply_handles = wire::Handles::new();
		if let Some(len) = object_push::dispatch(&mut PushView { stack: self, index }, &request, &mut handles, &mut out, &mut reply_handles) {
			send_blocking(chan, &out[..len], 0);
		}
	}

	// WHAT A PUSH'S CLIENT ASKS FOR: packets to the peer, and its end.
	fn run_push(&mut self, index: usize, outs: Vec<ClientOut>) {
		for out in outs {
			match out {
				ClientOut::Send(packet) => self.push_send(index, &packet),
				ClientOut::Done(result) => {
					let result = result.map_err(error_of);
					let pusher = &mut self.pushes[index];
					pusher.outcome = Some(result.clone());
					if let Some(request) = pusher.finishing.take() {
						answer_finish(pusher.chan, &request, result);
					}
					self.push_close_transport(index);
				}
			}
		}
	}

	fn push_send(&mut self, index: usize, packet: &[u8]) {
		let (at, peer, transport) = (self.pushes[index].at, self.pushes[index].peer, self.pushes[index].transport);
		let Some(handle) = self.controllers[at].link_to(&peer).map(|link| link.handle) else { return };
		match transport {
			Some(Transport::Rfcomm(channel)) => self.rfcomm_send(at, handle, channel, packet),
			Some(Transport::L2cap(cid)) => self.ertm_send(at, handle, cid, packet),
			None => {}
		}
	}

	fn push_close_transport(&mut self, index: usize) {
		let (at, peer) = (self.pushes[index].at, self.pushes[index].peer);
		let Some(transport) = self.pushes[index].transport.take() else { return };
		let Some(handle) = self.controllers[at].link_to(&peer).map(|link| link.handle) else { return };
		match transport {
			Transport::Rfcomm(channel) => {
				let outs = match self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).and_then(|classic| classic.rfcomm.as_mut()) {
					Some(rfcomm) => rfcomm.session.disconnect(channel),
					None => return,
				};
				self.run_rfcomm(at, handle, outs);
			}
			Transport::L2cap(cid) => self.close_channel(at, handle, cid),
		}
	}

	// A PUSH THAT CANNOT GO ON: its caller told at its `finish`, or at once where it waits there.
	fn push_failed(&mut self, index: usize, error: Error) {
		let pusher = &mut self.pushes[index];
		if pusher.outcome.is_some() {
			return;
		}
		pusher.outcome = Some(Err(error.clone()));
		pusher.reach_by = None;
		if let Some(request) = pusher.finishing.take() {
			answer_finish(pusher.chan, &request, Err(error));
		}
		self.push_close_transport(index);
	}

	fn end_push(&mut self, index: usize) {
		self.push_close_transport(index);
		let pusher = self.pushes.remove(index);
		close(pusher.chan);
	}

	// THE LINK IS SECURED: the peer's records searched for Object Push.
	pub(crate) fn start_push_search(&mut self, at: usize, handle: u16) {
		self.start_search(at, handle, sdp::uuid::OBEX_OBJECT_PUSH, classic::Purpose::Push);
	}

	// WHAT THE SEARCH FOUND: L2CAP where the record offers it, RFCOMM otherwise - or no Object Push at all.
	pub(crate) fn push_found(&mut self, at: usize, handle: u16, records: &[sdp::Record]) {
		let Some(peer) = self.controllers[at].link(handle).map(|link| link.peer) else { return };
		let Some(index) = self.pushes.iter().position(|pusher| pusher.at == at && pusher.peer == peer && pusher.transport.is_none() && pusher.outcome.is_none()) else { return };
		let record = records.iter().find(|record| record.matches(&[sdp::Uuid::U16(sdp::uuid::OBEX_OBJECT_PUSH)]));
		let goep = record.and_then(|record| record.get(sdp::attribute::GOEP_L2CAP_PSM)).and_then(sdp::Element::uint).map(|psm| psm as u16);
		let channel = record.and_then(sdp::Record::rfcomm_channel);
		if let Some(psm) = goep {
			let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()) else { return };
			let Some((cid, signal)) = classic.channels.open(psm, true) else {
				self.push_failed(index, Error::Exhausted);
				return;
			};
			self.pushes[index].transport = Some(Transport::L2cap(cid));
			self.send_signal(at, handle, &signal);
		} else if let Some(channel) = channel {
			self.pushes[index].transport = Some(Transport::Rfcomm(channel));
			self.rfcomm_connect(at, handle, channel, None);
		} else {
			print(b"BluetoothService: the peer offers no Object Push\n");
			self.push_failed(index, Error::NotFound);
		}
	}

	// THE TRANSPORT IS OPEN: CONNECT.
	fn push_opened(&mut self, index: usize) {
		self.pushes[index].reach_by = None;
		if let Some(connect) = self.pushes[index].connect.take() {
			self.push_send(index, &connect);
		}
	}

	// BYTES FROM THE PEER'S SERVER: its responses, one at a time.
	fn push_data(&mut self, index: usize, bytes: &[u8]) {
		let packets = match self.pushes[index].assembler.push(bytes) {
			Ok(packets) => packets,
			Err(_) => {
				let outs = self.pushes[index].client.abort();
				self.run_push(index, outs);
				return;
			}
		};
		for packet in packets {
			let outs = self.pushes[index].client.receive(&packet);
			self.run_push(index, outs);
		}
	}

	// ------------------------------------------------------------------ receiving

	// THE OPERATOR'S RECEIVE: the peer's page and its channel admitted, for one object.
	pub(crate) fn receive_object(&mut self, at: usize, peer: Peer, max_bytes: u64) -> Result<(), Error> {
		if !peer_is_classic_peer(&peer) {
			return Err(Error::Unsupported);
		}
		if self.record(at, &peer).is_none() {
			return Err(Error::NotFound);
		}
		if self.receivers.iter().any(|receiver| receiver.at == at) {
			return Err(Error::Again);
		}
		let controller = &self.controllers[at];
		if !controller.powered || !controller.classic {
			return Err(Error::Closed);
		}
		let _ = max_bytes;
		Ok(())
	}

	// THE RECEIVER'S STREAM IS MADE: it waits from now.
	pub(crate) fn receiver_started(&mut self, at: usize, peer: Peer, max_bytes: u64, stream: u64) {
		self.receivers.push(Receiver { stream, at, peer, server: PushServer::new(max_bytes), transport: None, assembler: Assembler::new(obex::MAX_PACKET), deadline: clock().saturating_add(RECEIVE_TICKS), received: 0 });
		self.controllers[at].bredr_state.receive_waits = Some(peer);
		self.refresh_policy(at);
	}

	// THE PEER OPENED THIS HOST'S OBJECT PUSH CHANNEL - admitted only for the peer a receiver waits for.
	fn receive_opened(&mut self, at: usize, peer: &Peer, transport: Transport) {
		if let Some(receiver) = self.receivers.iter_mut().find(|receiver| receiver.at == at && receiver.peer == *peer && receiver.transport.is_none()) {
			receiver.transport = Some(transport);
		}
	}

	// A REQUEST FROM THE PEER'S CLIENT: answered, and what it carried told to the receiver.
	fn receive_data(&mut self, index: usize, handle: u16, bytes: &[u8]) {
		let packets = match self.receivers[index].assembler.push(bytes) {
			Ok(packets) => packets,
			Err(_) => {
				self.end_receiver(index, Some(event(ReceivedKind::Failed, self.receivers[index].received)));
				return;
			}
		};
		for packet in packets {
			let (reply, events) = self.receivers[index].server.receive(&packet);
			let at = self.receivers[index].at;
			match self.receivers[index].transport {
				Some(Transport::Rfcomm(channel)) => self.rfcomm_send(at, handle, channel, &reply),
				Some(Transport::L2cap(cid)) => self.ertm_send(at, handle, cid, &reply),
				None => {}
			}
			let stream = self.receivers[index].stream;
			for happened in events {
				let told = match happened {
					ServerEvent::Offered { name, kind, length } => {
						// THE PEER'S WORDS, cut to what the record carries and never read as a path.
						let name: String = name.chars().filter(|character| !character.is_control()).take(200).collect();
						let object_type: String = kind.unwrap_or_default().chars().filter(|character| character.is_ascii_graphic()).take(64).collect();
						tell(stream, &ReceivedObject { kind: ReceivedKind::Offered, name, object_type, declared: length.map(u64::from), received: 0, bytes: Vec::new() })
					}
					ServerEvent::Data(bytes) => {
						let mut ok = true;
						for piece in bytes.chunks(PIECE) {
							self.receivers[index].received += piece.len() as u64;
							let received = self.receivers[index].received;
							ok &= tell(stream, &ReceivedObject { bytes: piece.to_vec(), ..event(ReceivedKind::Data, received) });
						}
						ok
					}
					ServerEvent::Complete => {
						let received = self.receivers[index].received;
						self.end_receiver(index, Some(event(ReceivedKind::Complete, received)));
						return;
					}
					ServerEvent::Failed(_) => {
						let received = self.receivers[index].received;
						self.end_receiver(index, Some(event(ReceivedKind::Failed, received)));
						return;
					}
				};
				if !told {
					// THE RECEIVER WENT while the object arrived: the push is refused at its next packet.
					self.end_receiver(index, None);
					return;
				}
			}
		}
	}

	// THE RECEIVER ENDS: its last word, the peer no longer admitted, and its channel - where the peer still holds it -
	// closed.
	fn end_receiver(&mut self, index: usize, last: Option<ReceivedObject>) {
		let receiver = self.receivers.remove(index);
		if let Some(last) = last {
			tell(receiver.stream, &last);
		}
		close(receiver.stream);
		let at = receiver.at;
		if self.controllers.get(at).is_some_and(|controller| controller.bredr_state.receive_waits == Some(receiver.peer)) {
			self.controllers[at].bredr_state.receive_waits = None;
			self.refresh_policy(at);
		}
		let Some(handle) = self.controllers.get(at).and_then(|controller| controller.link_to(&receiver.peer)).map(|link| link.handle) else { return };
		match receiver.transport {
			Some(Transport::Rfcomm(channel)) => {
				let outs = match self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut()).and_then(|classic| classic.rfcomm.as_mut()) {
					Some(rfcomm) => rfcomm.session.disconnect(channel),
					None => return,
				};
				self.run_rfcomm(at, handle, outs);
			}
			Some(Transport::L2cap(cid)) => self.close_channel(at, handle, cid),
			None => {}
		}
	}

	// A RECEIVER'S STREAM CLOSED BY ITS HOLDER: it stops waiting.
	pub(crate) fn receiver_gone(&mut self, stream: u64) {
		if let Some(index) = self.receivers.iter().position(|receiver| receiver.stream == stream) {
			self.end_receiver(index, None);
		}
	}

	pub(crate) fn receiver_streams(&self) -> impl Iterator<Item = u64> + '_ {
		self.receivers.iter().map(|receiver| receiver.stream)
	}

	// ------------------------------------------------------------------ the transports

	// The RFCOMM channels this link's Object Push sessions run on, or will: what `run_rfcomm` hands here.
	pub(crate) fn opp_rfcomm_channels(&self, at: usize, handle: u16) -> Vec<u8> {
		let Some(peer) = self.controllers[at].link(handle).map(|link| link.peer) else { return Vec::new() };
		let pushes = self.pushes.iter().filter(|pusher| pusher.at == at && pusher.peer == peer).filter_map(|pusher| match pusher.transport {
			Some(Transport::Rfcomm(channel)) => Some(channel),
			_ => None,
		});
		let receiving = self.receivers.iter().any(|receiver| receiver.at == at && receiver.peer == peer);
		pushes.chain(receiving.then_some(bt_policy::OPP_CHANNEL)).collect()
	}

	// AN RFCOMM EVENT on an Object Push channel.
	pub(crate) fn opp_rfcomm(&mut self, at: usize, handle: u16, channel: u8, happened: RfEvent) {
		let Some(peer) = self.controllers[at].link(handle).map(|link| link.peer) else { return };
		if let Some(index) = self.pushes.iter().position(|pusher| pusher.at == at && pusher.peer == peer && pusher.transport == Some(Transport::Rfcomm(channel))) {
			match happened {
				RfEvent::Opened => self.push_opened(index),
				RfEvent::Data(bytes) => self.push_data(index, &bytes),
				RfEvent::Closed => {
					self.pushes[index].transport = None;
					self.push_failed(index, Error::Closed);
				}
			}
			return;
		}
		if channel != bt_policy::OPP_CHANNEL {
			return;
		}
		match happened {
			RfEvent::Opened => self.receive_opened(at, &peer, Transport::Rfcomm(channel)),
			RfEvent::Data(bytes) => {
				if let Some(index) = self.receivers.iter().position(|receiver| receiver.at == at && receiver.peer == peer && receiver.transport == Some(Transport::Rfcomm(channel))) {
					self.receive_data(index, handle, &bytes);
				}
			}
			RfEvent::Closed => {
				if let Some(index) = self.receivers.iter().position(|receiver| receiver.at == at && receiver.peer == peer && receiver.transport == Some(Transport::Rfcomm(channel))) {
					self.receivers[index].transport = None;
					let failed = self.receivers[index].server.lost();
					if !failed.is_empty() {
						let received = self.receivers[index].received;
						self.end_receiver(index, Some(event(ReceivedKind::Failed, received)));
					}
				}
			}
		}
	}

	// AN L2CAP CHANNEL OPENED: a push's, where it is one.
	pub(crate) fn opp_l2cap_opened(&mut self, at: usize, handle: u16, cid: u16) {
		let Some(peer) = self.controllers[at].link(handle).map(|link| link.peer) else { return };
		if let Some(index) = self.pushes.iter().position(|pusher| pusher.at == at && pusher.peer == peer && pusher.transport == Some(Transport::L2cap(cid))) {
			self.push_opened(index);
		}
	}

	// AN SDU ON AN L2CAP CHANNEL no profile reads: a push's, where it is one.
	pub(crate) fn opp_l2cap_data(&mut self, at: usize, handle: u16, cid: u16, payload: &[u8]) {
		let Some(peer) = self.controllers[at].link(handle).map(|link| link.peer) else { return };
		if let Some(index) = self.pushes.iter().position(|pusher| pusher.at == at && pusher.peer == peer && pusher.transport == Some(Transport::L2cap(cid))) {
			self.push_data(index, payload);
		}
	}

	pub(crate) fn opp_l2cap_closed(&mut self, at: usize, handle: u16, cid: u16) {
		let Some(peer) = self.controllers[at].link(handle).map(|link| link.peer) else { return };
		if let Some(index) = self.pushes.iter().position(|pusher| pusher.at == at && pusher.peer == peer && pusher.transport == Some(Transport::L2cap(cid))) {
			self.pushes[index].transport = None;
			self.push_failed(index, Error::Closed);
		}
	}

	// THE LINK WENT: pushes on it fail, and a receiver whose object was arriving says so.
	pub(crate) fn opp_link_gone(&mut self, at: usize, peer: &Peer) {
		for index in 0..self.pushes.len() {
			if self.pushes[index].at == at && self.pushes[index].peer == *peer && self.pushes[index].outcome.is_none() && self.pushes[index].reach_by.is_none() {
				self.pushes[index].transport = None;
				self.push_failed(index, Error::Closed);
			}
		}
		if let Some(index) = self.receivers.iter().position(|receiver| receiver.at == at && receiver.peer == *peer && receiver.transport.is_some()) {
			self.receivers[index].transport = None;
			if !self.receivers[index].server.lost().is_empty() {
				let received = self.receivers[index].received;
				self.end_receiver(index, Some(event(ReceivedKind::Failed, received)));
			}
		}
	}

	// ------------------------------------------------------------------ time

	pub(crate) fn opp_deadline(&self) -> Option<u64> {
		let pushes = self.pushes.iter().filter_map(|pusher| pusher.reach_by);
		let receivers = self.receivers.iter().map(|receiver| receiver.deadline);
		pushes.chain(receivers).min()
	}

	pub(crate) fn opp_timers(&mut self) {
		let now = clock();
		for index in 0..self.pushes.len() {
			if self.pushes[index].reach_by.is_some_and(|due| now >= due) {
				self.push_failed(index, Error::TimedOut);
			}
		}
		while let Some(index) = self.receivers.iter().position(|receiver| now >= receiver.deadline) {
			let received = self.receivers[index].received;
			let kind = if self.receivers[index].transport.is_some() { ReceivedKind::Failed } else { ReceivedKind::TimedOut };
			self.end_receiver(index, Some(event(kind, received)));
		}
	}
}

// What `run_rfcomm` hands an Object Push session.
pub(crate) enum RfEvent {
	Opened,
	Data(Vec<u8>),
	Closed,
}

fn peer_is_classic_peer(peer: &Peer) -> bool {
	peer[0] == KIND_BREDR
}
