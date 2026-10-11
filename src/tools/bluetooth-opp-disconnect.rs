// Exact production receiver and protocol code is inserted by bluetooth_opp_disconnect.py.
// Only IPC delivery, link lookup, policy refresh and the L2CAP frame carrier are modeled.
// No guest/kernel/Bumble/RF outcome is invented by these in-memory boundaries.
#![allow(dead_code)]
extern crate alloc;
use std::cell::RefCell;
use std::collections::VecDeque;
/* EXACT_DEPENDENCIES */
use obex::{Assembler, PushClient, PushServer, ServerEvent};
use rfcomm::{Out as RfOut, Session};
mod service_logic {
	pub use crate::rfcomm;
}

const TICKS_PER_SECOND: u64 = 1000;
const KIND_BREDR: u8 = 1;
const HANDLE: u16 = 42;
const STREAM: u64 = 77;
type Peer = [u8; 7];
const PEER: Peer = [KIND_BREDR, 1, 2, 3, 4, 5, 6];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Error {
	Unsupported,
	NotFound,
	Again,
	Closed,
	TimedOut,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceivedKind {
	Offered,
	Data,
	Complete,
	Failed,
	TimedOut,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReceivedObject {
	kind: ReceivedKind,
	name: String,
	object_type: String,
	declared: Option<u64>,
	received: u64,
	bytes: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Trace {
	Event(ReceivedKind),
	Closed,
	Reply(u8),
	Disc,
	PeerDisc,
	Policy(bool),
}
#[derive(Default)]
struct Boundary {
	now: u64,
	owner_gone: bool,
	closed: bool,
	events: Vec<ReceivedObject>,
	trace: Vec<Trace>,
}
thread_local! { static BOUNDARY: RefCell<Boundary> = RefCell::new(Boundary::default()); }
fn clock() -> u64 {
	BOUNDARY.with_borrow(|b| b.now)
}
fn tell(stream: u64, event: &ReceivedObject) -> bool {
	assert_eq!(stream, STREAM);
	BOUNDARY.with_borrow_mut(|b| {
		assert!(!b.closed, "send on closed service stream");
		if b.owner_gone {
			return false;
		}
		b.trace.push(Trace::Event(event.kind));
		b.events.push(event.clone());
		true
	})
}
fn close(stream: u64) {
	assert_eq!(stream, STREAM);
	BOUNDARY.with_borrow_mut(|b| {
		assert!(!b.closed, "stream double close");
		b.closed = true;
		b.trace.push(Trace::Closed);
	});
}

struct Rfcomm {
	session: Session,
}
struct Classic {
	rfcomm: Option<Rfcomm>,
}
struct Link {
	peer: Peer,
	classic: Option<Classic>,
}
struct Bredr {
	receive_waits: Option<Peer>,
}
struct Controller {
	powered: bool,
	classic: bool,
	bredr_state: Bredr,
	link: Link,
}
impl Controller {
	fn link(&self, handle: u16) -> Option<&Link> {
		(handle == HANDLE).then_some(&self.link)
	}
	fn link_mut(&mut self, handle: u16) -> Option<&mut Link> {
		(handle == HANDLE).then_some(&mut self.link)
	}
	fn link_to(&self, peer: &Peer) -> Option<LinkRef> {
		(*peer == self.link.peer).then_some(LinkRef { handle: HANDLE })
	}
}
struct LinkRef {
	handle: u16,
}
struct Stack {
	controllers: Vec<Controller>,
	receivers: Vec<Receiver>,
	pushes: Vec<Pusher>,
	wire: VecDeque<Vec<u8>>,
	known: bool,
}
/* EXACT_DECLARATIONS */
impl Stack {
	/* EXACT_METHODS */

	fn record(&self, at: usize, peer: &Peer) -> Option<()> {
		(at == 0 && self.known && *peer == PEER).then_some(())
	}
	fn refresh_policy(&mut self, at: usize) {
		let enabled = self.controllers[at].bredr_state.receive_waits.is_some();
		BOUNDARY.with_borrow_mut(|b| b.trace.push(Trace::Policy(enabled)));
	}
	fn push_opened(&mut self, _: usize) {
		panic!("unexpected outbound push")
	}
	fn push_data(&mut self, _: usize, _: &[u8]) {
		panic!("unexpected outbound push")
	}
	fn push_failed(&mut self, _: usize, _: Error) {
		panic!("unexpected outbound push")
	}
	fn ertm_send(&mut self, _: usize, _: u16, _: u16, _: &[u8]) {
		panic!("OPP receiving is served over RFCOMM")
	}
	fn close_channel(&mut self, _: usize, _: u16, _: u16) {
		panic!("unexpected L2CAP receiver")
	}

	// Modeled L2CAP carrier and service event dispatcher. Frames and events come from the real Session.
	fn run_rfcomm(&mut self, at: usize, handle: u16, outs: Vec<RfOut>) {
		let mut events = Vec::new();
		for out in outs {
			match out {
				RfOut::Send(frame) => self.wire.push_back(frame),
				RfOut::Opened(channel) => events.push((channel, RfEvent::Opened)),
				RfOut::Data(channel, data) => events.push((channel, RfEvent::Data(data))),
				RfOut::Closed(channel) => events.push((channel, RfEvent::Closed)),
				RfOut::SessionOpen => {}
				RfOut::SessionClosed => {
					let channels = self.opp_rfcomm_channels(at, handle);
					self.controllers[at].link.classic.as_mut().unwrap().rfcomm = None;
					for channel in channels {
						events.push((channel, RfEvent::Closed));
					}
				}
			}
		}
		for (channel, event) in events {
			self.opp_rfcomm(at, handle, channel, event);
		}
		/* EXACT_FLUSH_HOOK */
	}
	fn accept(&self, peer: Peer, encrypted: bool) -> bool {
		let standing = bt_policy::Peer { trust: bt_policy::Trust::default(), receive_waits_for_it: self.controllers[0].bredr_state.receive_waits == Some(peer), encrypted };
		bt_policy::decide(bt_policy::inbound_by_channel(bt_policy::OPP_CHANNEL), &standing) == bt_policy::Decision::Accept
	}
	fn frame(&mut self, bytes: &[u8]) {
		let allowed = self.accept(PEER, true);
		let mut admit = |channel| if allowed && channel == bt_policy::OPP_CHANNEL { rfcomm::Admission::Accept } else { rfcomm::Admission::Refuse };
		let out = self.controllers[0].link.classic.as_mut().unwrap().rfcomm.as_mut().unwrap().session.receive(bytes, &mut admit);
		self.run_rfcomm(0, HANDLE, out);
	}
}

struct Exchange {
	stack: Stack,
	peer: Session,
	pending: VecDeque<RfOut>,
	assembler: Assembler,
	responses: Vec<Vec<u8>>,
	peer_closed: usize,
}
impl Exchange {
	fn new(limit: u64, open: bool) -> Self {
		BOUNDARY.with_borrow_mut(|b| *b = Boundary { now: 100, ..Boundary::default() });
		let mut result = Self { stack: Stack { controllers: vec![Controller { powered: true, classic: true, bredr_state: Bredr { receive_waits: None }, link: Link { peer: PEER, classic: Some(Classic { rfcomm: Some(Rfcomm { session: Session::new(false, 256) }) }) } }], receivers: Vec::new(), pushes: Vec::new(), wire: VecDeque::new(), known: true }, peer: Session::new(true, 256), pending: VecDeque::new(), assembler: Assembler::new(obex::MAX_PACKET), responses: Vec::new(), peer_closed: 0 };
		assert_eq!(result.stack.receive_object(0, PEER, limit), Ok(()));
		result.stack.receiver_started(0, PEER, limit, STREAM);
		assert_eq!(result.stack.opp_deadline(), Some(100 + 180 * TICKS_PER_SECOND));
		assert_eq!(result.stack.receive_object(0, PEER, limit), Err(Error::Again));
		assert!(!result.stack.accept([KIND_BREDR, 6, 5, 4, 3, 2, 1], true));
		assert!(!result.stack.accept(PEER, false));
		if open {
			result.pending.extend(result.peer.connect(bt_policy::OPP_CHANNEL).unwrap());
			result.pump();
			assert_eq!(result.stack.receivers[0].transport, Some(Transport::Rfcomm(bt_policy::OPP_CHANNEL)));
		}
		result
	}
	fn pump(&mut self) {
		for _ in 0..1000 {
			if let Some(out) = self.pending.pop_front() {
				match out {
					RfOut::Send(frame) => {
						let decoded = rfcomm::decode(&frame, |_| true, rfcomm::MAX_FRAME).unwrap();
						if decoded.kind == rfcomm::Kind::Disc {
							BOUNDARY.with_borrow_mut(|b| b.trace.push(Trace::PeerDisc));
						}
						self.stack.frame(&frame);
					}
					RfOut::Data(channel, bytes) => {
						assert_eq!(channel, bt_policy::OPP_CHANNEL);
						for packet in self.assembler.push(&bytes).unwrap() {
							BOUNDARY.with_borrow_mut(|b| b.trace.push(Trace::Reply(packet[0])));
							self.responses.push(packet);
						}
					}
					RfOut::Closed(channel) => {
						assert_eq!(channel, bt_policy::OPP_CHANNEL);
						self.peer_closed += 1;
					}
					RfOut::Opened(channel) => assert_eq!(channel, bt_policy::OPP_CHANNEL),
					RfOut::SessionOpen => {}
					RfOut::SessionClosed => panic!("unexpected peer multiplexer close"),
				}
				continue;
			}
			if let Some(frame) = self.stack.wire.pop_front() {
				let decoded = rfcomm::decode(&frame, |_| true, rfcomm::MAX_FRAME).unwrap();
				if decoded.kind == rfcomm::Kind::Disc && decoded.dlci != 0 {
					BOUNDARY.with_borrow_mut(|b| b.trace.push(Trace::Disc));
				}
				self.pending.extend(self.peer.receive(&frame, &mut |_| rfcomm::Admission::Refuse));
				continue;
			}
			return;
		}
		panic!("RFCOMM exchange did not drain");
	}
	fn request(&mut self, bytes: &[u8], expected: u8) {
		let before = self.responses.len();
		let out = self.peer.write(bt_policy::OPP_CHANNEL, bytes).expect("open peer DLC");
		self.pending.extend(out);
		self.pump();
		assert_eq!(self.responses.len(), before + 1, "exactly one actual OBEX response");
		assert_eq!(self.responses[before][0], expected);
	}
	fn connect(&mut self) {
		let packet = obex::Packet { code: obex::opcode::CONNECT, connect: Some((obex::VERSION, 0, 4096)), headers: Vec::new() }.encode();
		self.request(&packet, obex::response::SUCCESS);
	}
	fn headers(&mut self, declared: Option<u32>, expected: u8) {
		let mut headers = vec![obex::Header::Text(obex::header::NAME, "../../untrusted-name.txt".into())];
		if let Some(size) = declared {
			headers.push(obex::Header::Word(obex::header::LENGTH, size));
		}
		self.request(&obex::Packet::new(obex::opcode::PUT, headers).encode(), expected);
	}
	fn body(&mut self, bytes: &[u8], final_piece: bool, expected: u8) {
		self.request(&obex::Packet::new(if final_piece { obex::opcode::PUT_FINAL } else { obex::opcode::PUT }, vec![obex::Header::Bytes(if final_piece { obex::header::END_OF_BODY } else { obex::header::BODY }, bytes.to_vec())]).encode(), expected);
	}
	fn complete_object(&mut self) -> Vec<u8> {
		let bytes = ("independent-bumble-object-".repeat(37) + "\n").into_bytes();
		assert_eq!(bytes.len(), 963);
		self.connect();
		self.headers(Some(bytes.len() as u32), obex::response::CONTINUE);
		self.body(&bytes[..512], false, obex::response::CONTINUE);
		self.body(&bytes[512..], true, obex::response::SUCCESS);
		assert_eq!(self.stack.receivers.len(), 1, "OPP_FINAL_PUT_RETAINED");
		assert_eq!(self.stack.opp_deadline(), Some(100 + 180 * TICKS_PER_SECOND));
		assert_eq!(self.peer_closed, 0, "final PUT must not close its RFCOMM transport");
		assert!(events().iter().all(|event| event.kind != ReceivedKind::Complete));
		assert_eq!(received(), bytes);
		bytes
	}
	fn disconnect(&mut self) {
		self.request(&obex::Packet::new(obex::opcode::DISCONNECT, Vec::new()).encode(), obex::response::SUCCESS);
	}
	fn close_multiplexer(&mut self) {
		// The actual Bumble Client.shutdown sends DISC on DLCI0 before closing L2CAP.
		self.pending.push_back(RfOut::Send(rfcomm::encode(&rfcomm::Frame { dlci: 0, cr: true, kind: rfcomm::Kind::Disc, pf: true, credits: None, info: Vec::new() })));
		self.pump();
	}
	fn cleaned(&self, kind: Option<ReceivedKind>, count: usize) {
		assert!(self.stack.receivers.is_empty());
		assert_eq!(self.stack.receiver_streams().count(), 0);
		assert_eq!(self.stack.opp_deadline(), None);
		assert_eq!(self.stack.controllers[0].bredr_state.receive_waits, None);
		assert!(!self.stack.accept(PEER, true));
		assert!(self.stack.opp_rfcomm_channels(0, HANDLE).is_empty());
		BOUNDARY.with_borrow(|b| assert!(b.closed));
		let terminal: Vec<_> = events().into_iter().filter(|event| matches!(event.kind, ReceivedKind::Complete | ReceivedKind::Failed | ReceivedKind::TimedOut)).collect();
		match kind {
			Some(kind) => {
				assert_eq!(terminal.len(), 1);
				assert_eq!(terminal[0].kind, kind);
				assert_eq!(terminal[0].received, count as u64);
			}
			None => assert!(terminal.is_empty()),
		}
	}
}
fn events() -> Vec<ReceivedObject> {
	BOUNDARY.with_borrow(|b| b.events.clone())
}
fn received() -> Vec<u8> {
	events().into_iter().filter(|event| event.kind == ReceivedKind::Data).flat_map(|event| event.bytes).collect()
}

fn successful_disconnect_and_refusals() {
	let mut x = Exchange::new(4096, true);
	let bytes = x.complete_object();
	let final_reply = BOUNDARY.with_borrow(|b| b.trace.iter().rposition(|trace| *trace == Trace::Reply(obex::response::SUCCESS)).unwrap());
	let before = events();
	x.body(b"forbidden second object", true, obex::response::UNAVAILABLE);
	assert_eq!(events(), before, "a second object admits no data or event");
	// Length is internally consistent, but its variable header overruns the packet.
	x.request(&[0x81, 0, 6, 0x48, 0, 9], obex::response::BAD_REQUEST);
	assert_eq!(x.stack.receivers.len(), 1, "OPP_MALFORMED_DISCONNECT_RETAINED");
	assert_eq!(received(), bytes);
	x.disconnect();
	x.cleaned(Some(ReceivedKind::Complete), bytes.len());
	assert_eq!(x.peer_closed, 1, "the no-close fixture's completed session is retired without waiting180 seconds");
	BOUNDARY.with_borrow(|b| {
		let reply = b.trace.iter().rposition(|trace| *trace == Trace::Reply(obex::response::SUCCESS)).unwrap();
		let disc = b.trace.iter().position(|trace| *trace == Trace::Disc).unwrap();
		assert!(final_reply < reply && reply < disc, "final PUT and DISCONNECT success precede RFCOMM close");
	});
	x.close_multiplexer();
	x.cleaned(Some(ReceivedKind::Complete), bytes.len());
	assert_eq!(events().iter().filter(|event| event.kind == ReceivedKind::Complete).count(), 1);
}

fn queued_disconnect_response() {
	let mut x = Exchange::new(4096, true);
	// Use actual flow-control operations, not a mutation of Session's credit fields.
	x.pending.extend(x.peer.pause(bt_policy::OPP_CHANNEL, true));
	x.pump();
	for _ in 0..3 {
		x.connect();
	}
	let bytes = x.complete_object();
	let dlc = x.stack.controllers[0].link.classic.as_ref().unwrap().rfcomm.as_ref().unwrap().session.dlc(bt_policy::OPP_CHANNEL).unwrap();
	assert_eq!(dlc.tx_credits, 0, "seven actual responses consumed the seven granted credits");
	assert!(dlc.queue.is_empty());
	let previous = x.responses.len();
	let packet = obex::Packet::new(obex::opcode::DISCONNECT, Vec::new()).encode();
	x.pending.extend(x.peer.write(bt_policy::OPP_CHANNEL, &packet).unwrap());
	x.pump();
	assert_eq!(x.stack.receivers.len(), 1, "OPP_DISCONNECT_REPLY_QUEUED");
	assert_eq!(x.responses.len(), previous, "no invented reply while credits are absent");
	let dlc = x.stack.controllers[0].link.classic.as_ref().unwrap().rfcomm.as_ref().unwrap().session.dlc(bt_policy::OPP_CHANNEL).unwrap();
	assert_eq!(dlc.state, rfcomm::DlcState::Open);
	assert_eq!(dlc.queue.len(), 1);
	assert_eq!(x.peer_closed, 0);
	let extra = obex::Packet::new(obex::opcode::PUT_FINAL, vec![obex::Header::Bytes(obex::header::END_OF_BODY, b"after disconnect".to_vec())]).encode();
	x.pending.extend(x.peer.write(bt_policy::OPP_CHANNEL, &extra).unwrap());
	x.pump();
	assert_eq!(x.stack.receivers.len(), 1);
	assert_eq!(received(), bytes, "no request after DISCONNECT can append bytes");
	assert_eq!(x.stack.controllers[0].link.classic.as_ref().unwrap().rfcomm.as_ref().unwrap().session.dlc(bt_policy::OPP_CHANNEL).unwrap().queue.len(), 1);
	x.pending.extend(x.peer.pause(bt_policy::OPP_CHANNEL, false));
	x.pump();
	assert_eq!(x.responses.len(), previous + 1);
	assert_eq!(x.responses.last().unwrap(), &[obex::response::SUCCESS, 0, 3]);
	x.cleaned(Some(ReceivedKind::Complete), bytes.len());
	assert_eq!(x.peer_closed, 1, "credit-only input flushed the reply and retired the receiver");
	x.close_multiplexer();
}

fn limits_and_owner_close() {
	let mut unsolicited = Exchange::new(64, false);
	unsolicited.stack.receiver_gone(STREAM);
	unsolicited.pending.extend(unsolicited.peer.connect(bt_policy::OPP_CHANNEL).unwrap());
	unsolicited.pump();
	assert_eq!(unsolicited.peer_closed, 1, "real RFCOMM DM refuses an unsolicited channel");
	unsolicited.cleaned(None, 0);
	let mut declared = Exchange::new(64, true);
	declared.connect();
	declared.headers(Some(512), obex::response::TOO_LARGE);
	declared.cleaned(Some(ReceivedKind::Failed), 0);
	assert!(received().is_empty());
	let mut running = Exchange::new(64, true);
	running.connect();
	running.headers(None, obex::response::CONTINUE);
	running.body(&[1; 32], false, obex::response::CONTINUE);
	running.body(&[2; 33], true, obex::response::TOO_LARGE);
	running.cleaned(Some(ReceivedKind::Failed), 32);
	assert_eq!(received(), vec![1; 32]);
	let mut aborted = Exchange::new(64, true);
	aborted.connect();
	aborted.headers(None, obex::response::CONTINUE);
	aborted.body(&[4; 12], false, obex::response::CONTINUE);
	aborted.request(&obex::Packet::new(obex::opcode::ABORT, Vec::new()).encode(), obex::response::SUCCESS);
	aborted.cleaned(Some(ReceivedKind::Failed), 12);
	for complete in [false, true] {
		let mut x = Exchange::new(4096, true);
		if complete {
			x.complete_object();
		}
		BOUNDARY.with_borrow_mut(|b| b.owner_gone = true);
		x.stack.receiver_gone(STREAM);
		x.pump();
		x.cleaned(None, 0);
		assert_eq!(x.peer_closed, 1, "owner-close retires its DLC immediately");
	}
}

fn deadline_and_loss() {
	for state in 0..3 {
		let mut x = Exchange::new(4096, state != 0);
		let count = if state == 2 { x.complete_object().len() } else { 0 };
		let due = 100 + 180 * TICKS_PER_SECOND;
		BOUNDARY.with_borrow_mut(|b| b.now = due - 1);
		x.stack.opp_timers();
		assert_eq!(x.stack.receivers.len(), 1);
		BOUNDARY.with_borrow_mut(|b| b.now = due);
		x.stack.opp_timers();
		x.pump();
		x.cleaned(
			Some(match state {
				0 => ReceivedKind::TimedOut,
				1 => ReceivedKind::Failed,
				_ => ReceivedKind::Complete,
			}),
			count,
		);
	}
	for complete in [false, true] {
		for link in [false, true] {
			let mut x = Exchange::new(4096, true);
			let count = if complete {
				x.complete_object().len()
			} else {
				x.connect();
				x.headers(None, obex::response::CONTINUE);
				x.body(&[3; 21], false, obex::response::CONTINUE);
				21
			};
			if link {
				x.stack.opp_link_gone(0, &PEER);
			} else {
				x.pending.extend(x.peer.disconnect(bt_policy::OPP_CHANNEL));
				x.pump();
			}
			x.cleaned(Some(if complete { ReceivedKind::Complete } else { ReceivedKind::Failed }), count);
		}
	}
	let mut malformed = Exchange::new(4096, true);
	let count = malformed.complete_object().len();
	malformed.pending.extend(malformed.peer.write(bt_policy::OPP_CHANNEL, &[0x81, 0, 2]).unwrap());
	malformed.pump();
	malformed.cleaned(Some(ReceivedKind::Complete), count);
}

fn main() {
	if std::env::args().nth(1).as_deref() == Some("credits") {
		queued_disconnect_response();
		println!("OPP_DISCONNECT_COMPLETE: queued reply drained before peer close");
		return;
	}
	successful_disconnect_and_refusals();
	queued_disconnect_response();
	limits_and_owner_close();
	deadline_and_loss();
	println!("OPP_DISCONNECT_COMPLETE: final PUT retained, valid DISCONNECT response before DLC close, second/malformed refusal, exact bytes, owner-close, bounds, original deadline and transport/link loss");
}
