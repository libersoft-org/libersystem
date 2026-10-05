// AN APPLICATION'S GATT CLIENT: a grant PermissionManager mints per launch from the admin root, for one component,
// one bonded LE peer the operator gave an alias and trusted for GATT, and a set of services - the route a camera
// grant takes.
//
// WHAT A GRANT REACHES. The granted services the peer has, and nothing outside their handle ranges; never a service
// this stack drives itself - HID, GAP, GATT and the LE Audio services - whatever the grant named. A handle outside a
// granted range is refused before anything goes on the radio.
//
// ATT IS ONE REQUEST AT A TIME on a link, and the link may be walking its HID service too: so each operation is
// queued on the link and sent when the bearer is free, and the client's request is answered when its response
// arrives - the request kept, and answered by running the generated dispatch again over the result.
//
// A GRANT ENDS WITH ITS LAUNCH: the owner task's end closes it, its subscriptions with it.

use super::*;
use proto::system::{GattCharacteristic, GattService, GattValue, bluetooth_admin, bluetooth_gatt};
use service_logic::att::op;

// The services this stack drives itself, which no grant reaches: GAP, GATT, HID, and the LE Audio services.
const STACK_SERVICES: [u16; 11] = [0x1800, 0x1801, 0x1812, 0x184e, 0x184f, 0x1850, 0x1844, 0x1846, 0x184c, 0x1853, 0x1855];
// Grants alive at once, across every controller.
const MAX_GRANTS: usize = bt_bounds::MINTED_GRANTS;
// Operations a link holds queued.
const MAX_QUEUED_OPS: usize = 16;

pub(crate) struct Grant {
	pub chan: u64,
	pub owner: u64,
	at: usize,
	peer: Peer,
	services: Vec<u16>,
	// Notifications this grant holds: a value handle and the stream's producer.
	subscriptions: Vec<(u16, u64)>,
}

// What a queued operation is for, and the request it answers.
#[derive(Clone)]
pub(crate) enum Kind {
	Discover { from: u16, found: Vec<(u16, u16, u16)> },
	Characteristics { start: u16, end: u16, from: u16, found: Vec<GattCharacteristic> },
	Read { handle: u16 },
	Write { handle: u16, value: Vec<u8> },
	Configuration { handle: u16, end: u16 },
	Subscribe { handle: u16, cccd: u16 },
}

pub(crate) struct Op {
	grant: u64,
	request: Vec<u8>,
	kind: Kind,
}

// The ATT client state a link keeps for grants: the peer's services once walked, and the operations.
#[derive(Default)]
pub(crate) struct Client {
	services: Option<Vec<(u16, u16, u16)>>,
	queue: VecDeque<Op>,
	busy: Option<Op>,
}

// THE GENERATED DISPATCH, RUN AGAIN over a result: the reply a deferred request gets, encoded as the dispatch encodes.
struct Answered {
	result: Option<Result<Answer, Error>>,
}

enum Answer {
	Services(Vec<GattService>),
	Characteristics(Vec<GattCharacteristic>),
	Read(Vec<u8>),
	Written,
}

impl bluetooth_gatt::Service for Answered {
	fn services(&mut self) -> Result<Vec<GattService>, Error> {
		match self.result.take() {
			Some(Ok(Answer::Services(services))) => Ok(services),
			Some(Err(error)) => Err(error),
			_ => Err(Error::Invalid),
		}
	}

	fn characteristics(&mut self, _start: u16) -> Result<Vec<GattCharacteristic>, Error> {
		match self.result.take() {
			Some(Ok(Answer::Characteristics(found))) => Ok(found),
			Some(Err(error)) => Err(error),
			_ => Err(Error::Invalid),
		}
	}

	fn read(&mut self, _handle: u16) -> Result<Vec<u8>, Error> {
		match self.result.take() {
			Some(Ok(Answer::Read(value))) => Ok(value),
			Some(Err(error)) => Err(error),
			_ => Err(Error::Invalid),
		}
	}

	fn write(&mut self, _handle: u16, _value: Vec<u8>) -> Result<(), Error> {
		match self.result.take() {
			Some(Ok(Answer::Written)) => Ok(()),
			Some(Err(error)) => Err(error),
			_ => Err(Error::Invalid),
		}
	}

	fn subscribe(&mut self, _handle: u16) -> Result<Vec<GattValue>, Error> {
		Err(Error::Invalid)
	}
}

// THE REQUEST'S ARGUMENTS, read by the generated dispatch itself into a view that only keeps them.
#[derive(Default)]
struct Captured {
	op: u16,
	handle: u16,
	value: Vec<u8>,
}

impl bluetooth_gatt::Service for Captured {
	fn services(&mut self) -> Result<Vec<GattService>, Error> {
		self.op = bluetooth_gatt::OP_SERVICES;
		Err(Error::Again)
	}

	fn characteristics(&mut self, start: u16) -> Result<Vec<GattCharacteristic>, Error> {
		self.op = bluetooth_gatt::OP_CHARACTERISTICS;
		self.handle = start;
		Err(Error::Again)
	}

	fn read(&mut self, handle: u16) -> Result<Vec<u8>, Error> {
		self.op = bluetooth_gatt::OP_READ;
		self.handle = handle;
		Err(Error::Again)
	}

	fn write(&mut self, handle: u16, value: Vec<u8>) -> Result<(), Error> {
		self.op = bluetooth_gatt::OP_WRITE;
		self.handle = handle;
		self.value = value;
		Err(Error::Again)
	}

	fn subscribe(&mut self, handle: u16) -> Result<Vec<GattValue>, Error> {
		self.op = bluetooth_gatt::OP_SUBSCRIBE;
		self.handle = handle;
		Err(Error::Again)
	}
}

fn captured(request: &[u8]) -> Option<Captured> {
	let mut view = Captured::default();
	let mut handles = wire::Handles::new();
	if u16::from_le_bytes([*request.first()?, *request.get(1)?]) == bluetooth_gatt::OP_SUBSCRIBE {
		let _ = bluetooth_gatt::subscribe_open(&mut view, request, &mut handles)?;
	} else {
		let mut out = [0u8; 64];
		let mut reply_handles = wire::Handles::new();
		bluetooth_gatt::dispatch(&mut view, request, &mut handles, &mut out, &mut reply_handles)?;
	}
	(view.op != 0).then_some(view)
}

fn reply(chan: u64, request: &[u8], result: Result<Answer, Error>) {
	let mut out = alloc::vec![0u8; 1024];
	let mut handles = wire::Handles::new();
	let mut reply_handles = wire::Handles::new();
	if let Some(len) = bluetooth_gatt::dispatch(&mut Answered { result: Some(result) }, request, &mut handles, &mut out, &mut reply_handles) {
		send_blocking(chan, &out[..len], 0);
	}
}

fn request(code: u8, fields: &[u16]) -> Vec<u8> {
	let mut out = alloc::vec![code];
	for field in fields {
		out.extend_from_slice(&field.to_le_bytes());
	}
	out
}

// THE ADMIN ROOT'S ONE VERB, for PermissionManager.
pub(crate) struct AdminView<'a> {
	pub stack: &'a mut Stack,
}

impl bluetooth_admin::Service for AdminView<'_> {
	fn mint_gatt(&mut self, alias: String, services: Vec<proto::system::ServiceClass>, owner: u64) -> Result<u64, Error> {
		if owner == 0 {
			return Err(Error::Invalid);
		}
		if alias.is_empty() || self.stack.grants.len() >= MAX_GRANTS {
			if owner != 0 {
				close(owner);
			}
			return Err(if alias.is_empty() { Error::Invalid } else { Error::Exhausted });
		}
		// THE PEER THE ALIAS NAMES, bonded on LE and trusted for GATT - or nothing.
		let found = (0..self.stack.controllers.len()).find_map(|at| self.stack.records(at).into_iter().find(|record| record.alias == alias && record.radio == Radio::Le).map(|record| (at, record)));
		let Some((at, record)) = found else {
			close(owner);
			return Err(Error::NotFound);
		};
		if !record.trusted.contains(&Profile::Gatt) {
			close(owner);
			return Err(Error::Denied);
		}
		let Some(peer) = peer_from_wire(&record.peer) else {
			close(owner);
			return Err(Error::Invalid);
		};
		let granted: Vec<u16> = services.iter().map(|class| class.uuid).filter(|uuid| !STACK_SERVICES.contains(uuid)).collect();
		let Some((mine, theirs)) = channel() else {
			close(owner);
			return Err(Error::Exhausted);
		};
		self.stack.grants.push(Grant { chan: mine, owner, at, peer, services: granted, subscriptions: Vec::new() });
		self.stack.reconnect_bonded(at);
		Ok(theirs)
	}
}

impl Stack {
	// The link a grant's peer is connected on, encrypted - or why there is none.
	fn grant_link(&self, grant: &Grant) -> Result<u16, Error> {
		let controller = self.controllers.get(grant.at).ok_or(Error::Closed)?;
		match controller.link_to(&grant.peer) {
			Some(link) if link.encrypted => Ok(link.handle),
			_ => Err(Error::Closed),
		}
	}

	// THE RANGE A HANDLE IS IN, when it is inside a service this grant covers.
	fn granted_range(&self, grant: &Grant, handle: u16) -> Option<(u16, u16)> {
		let link = self.controllers.get(grant.at)?.link_to(&grant.peer)?;
		let services = link.gatt.services.as_ref()?;
		services.iter().find(|(uuid, start, end)| grant.services.contains(uuid) && !STACK_SERVICES.contains(uuid) && *start <= handle && handle <= *end).map(|(_, start, end)| (*start, *end))
	}

	// ONE REQUEST ON A GRANT: answered now where it can be, queued on the link where it needs the radio.
	pub(crate) fn serve_grant(&mut self, index: usize, buf: &mut [u8]) {
		let chan = self.grants[index].chan;
		let (len, handles) = match try_recv_caps(chan, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				let grant = self.grants.remove(index);
				end_grant(grant);
				return;
			}
		};
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		let request = buf[..len].to_vec();
		let Some(captured) = captured(&request) else { return };
		let (op_code, argument) = (captured.op, captured.handle);
		let grants = &self.grants;
		let handle = match self.grant_link(&grants[index]) {
			Ok(handle) => handle,
			Err(error) => {
				self.refuse(chan, &request, op_code, error);
				return;
			}
		};
		let kind = match op_code {
			bluetooth_gatt::OP_SERVICES => {
				let link = self.controllers[grants[index].at].link(handle).expect("the grant's link");
				if let Some(services) = link.gatt.services.as_ref() {
					let granted = granted_services(&grants[index], services);
					reply(chan, &request, Ok(Answer::Services(granted)));
					return;
				}
				Kind::Discover { from: 1, found: Vec::new() }
			}
			bluetooth_gatt::OP_CHARACTERISTICS => {
				let range = self.granted_range(&grants[index], argument).filter(|(start, _)| *start == argument);
				let Some((start, end)) = range else {
					reply(chan, &request, Err(Error::Denied));
					return;
				};
				Kind::Characteristics { start, end, from: start + 1, found: Vec::new() }
			}
			bluetooth_gatt::OP_READ | bluetooth_gatt::OP_WRITE | bluetooth_gatt::OP_SUBSCRIBE => {
				let Some((_, end)) = self.granted_range(&grants[index], argument) else {
					self.refuse(chan, &request, op_code, Error::Denied);
					return;
				};
				match op_code {
					bluetooth_gatt::OP_READ => Kind::Read { handle: argument },
					bluetooth_gatt::OP_WRITE => Kind::Write { handle: argument, value: captured.value },
					_ => Kind::Configuration { handle: argument, end },
				}
			}
			_ => return,
		};
		let at = self.grants[index].at;
		let Some(link) = self.controllers[at].link_mut(handle) else { return };
		if link.gatt.queue.len() >= MAX_QUEUED_OPS {
			self.refuse(chan, &request, op_code, Error::Again);
			return;
		}
		link.gatt.queue.push_back(Op { grant: chan, request, kind });
		self.next_gatt(at, handle);
	}

	fn refuse(&self, chan: u64, request: &[u8], op_code: u16, error: Error) {
		if op_code == bluetooth_gatt::OP_SUBSCRIBE {
			let corr = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
			let mut out = [0u8; 64];
			if let Some(len) = bluetooth_gatt::subscribe_reply_err(corr, &error, &mut out) {
				send_blocking(chan, &out[..len], 0);
			}
			return;
		}
		reply(chan, request, Err(error));
	}

	// THE NEXT QUEUED OPERATION GOES when the bearer is free: no walk of its own, and nothing outstanding.
	pub(crate) fn next_gatt(&mut self, at: usize, handle: u16) {
		let Some(link) = self.controllers[at].link_mut(handle) else { return };
		if link.walk.is_some() || link.gatt.busy.is_some() {
			return;
		}
		let Some(op) = link.gatt.queue.pop_front() else { return };
		let pdu = match &op.kind {
			Kind::Discover { from, .. } => request(op::READ_BY_GROUP_TYPE_REQUEST, &[*from, 0xffff, 0x2800]),
			Kind::Characteristics { from, end, .. } => request(op::READ_BY_TYPE_REQUEST, &[*from, *end, 0x2803]),
			Kind::Read { handle } => request(op::READ_REQUEST, &[*handle]),
			Kind::Write { handle, value } => {
				let mut out = request(op::WRITE_REQUEST, &[*handle]);
				out.extend_from_slice(value);
				out
			}
			Kind::Configuration { handle, end } => request(op::FIND_INFORMATION_REQUEST, &[*handle + 1, *end]),
			Kind::Subscribe { cccd, .. } => {
				let mut out = request(op::WRITE_REQUEST, &[*cccd]);
				out.extend_from_slice(&[0x01, 0x00]);
				out
			}
		};
		link.gatt.busy = Some(op);
		self.controllers[at].l2cap(handle, ATT_CID, &pdu);
	}

	// AN ATT RESPONSE for the operation outstanding: its result, or the next request of a walk.
	pub(crate) fn gatt_response(&mut self, at: usize, handle: u16, pdu: &[u8]) {
		let Some(op) = self.controllers[at].link_mut(handle).and_then(|link| link.gatt.busy.take()) else { return };
		let Some(grant_index) = self.grants.iter().position(|grant| grant.chan == op.grant) else {
			self.next_gatt(at, handle);
			return;
		};
		let error_code = (pdu.first() == Some(&op::ERROR_RESPONSE)).then(|| pdu.get(4).copied().unwrap_or(0));
		let chan = op.grant;
		match op.kind {
			Kind::Discover { mut found, .. } => {
				let mut next = None;
				if error_code.is_none() && pdu.len() > 2 && pdu[1] == 6 {
					for entry in pdu[2..].chunks_exact(6) {
						let (start, end, uuid) = (u16::from_le_bytes([entry[0], entry[1]]), u16::from_le_bytes([entry[2], entry[3]]), u16::from_le_bytes([entry[4], entry[5]]));
						if end < start || found.len() >= 32 {
							continue;
						}
						found.push((uuid, start, end));
						next = (end < 0xffff).then_some(end + 1);
					}
				}
				match next {
					Some(from) if found.len() < 32 => {
						if let Some(link) = self.controllers[at].link_mut(handle) {
							link.gatt.queue.push_front(Op { grant: chan, request: op.request, kind: Kind::Discover { from, found } });
						}
					}
					_ => {
						let granted = granted_services(&self.grants[grant_index], &found);
						if let Some(link) = self.controllers[at].link_mut(handle) {
							link.gatt.services = Some(found);
						}
						reply(chan, &op.request, Ok(Answer::Services(granted)));
					}
				}
			}
			Kind::Characteristics { start, end, mut found, .. } => {
				let mut next = None;
				if error_code.is_none() && pdu.len() > 2 && pdu[1] == 7 {
					for entry in pdu[2..].chunks_exact(7) {
						let declaration = u16::from_le_bytes([entry[0], entry[1]]);
						let value = u16::from_le_bytes([entry[3], entry[4]]);
						if declaration < start || declaration > end || value <= declaration || value > end || found.len() >= 32 {
							continue;
						}
						found.push(GattCharacteristic { handle: value, uuid: u16::from_le_bytes([entry[5], entry[6]]), properties: entry[2] });
						next = (value < end).then_some(value + 1);
					}
				}
				match next {
					Some(from) => {
						if let Some(link) = self.controllers[at].link_mut(handle) {
							link.gatt.queue.push_front(Op { grant: chan, request: op.request, kind: Kind::Characteristics { start, end, from, found } });
						}
					}
					None => reply(chan, &op.request, Ok(Answer::Characteristics(found))),
				}
			}
			Kind::Read { .. } => {
				let result = match (error_code, pdu.first()) {
					(None, Some(&op::READ_RESPONSE)) => Ok(Answer::Read(pdu[1..].to_vec())),
					(Some(0x02), _) | (Some(0x05), _) | (Some(0x0f), _) => Err(Error::Denied),
					_ => Err(Error::Io),
				};
				reply(chan, &op.request, result);
			}
			Kind::Write { .. } => {
				let result = match (error_code, pdu.first()) {
					(None, Some(&op::WRITE_RESPONSE)) => Ok(Answer::Written),
					(Some(0x03), _) | (Some(0x05), _) | (Some(0x0f), _) => Err(Error::Denied),
					_ => Err(Error::Io),
				};
				reply(chan, &op.request, result);
			}
			// THE CONFIGURATION DESCRIPTOR of the characteristic, then notifications on.
			Kind::Configuration { handle: value, .. } => {
				let cccd = (error_code.is_none() && pdu.len() > 2 && pdu[1] == 1).then(|| pdu[2..].chunks_exact(4).find(|entry| u16::from_le_bytes([entry[2], entry[3]]) == 0x2902).map(|entry| u16::from_le_bytes([entry[0], entry[1]]))).flatten();
				match cccd {
					Some(cccd) => {
						if let Some(link) = self.controllers[at].link_mut(handle) {
							link.gatt.queue.push_front(Op { grant: chan, request: op.request, kind: Kind::Subscribe { handle: value, cccd } });
						}
					}
					None => self.refuse(chan, &op.request, bluetooth_gatt::OP_SUBSCRIBE, Error::Unsupported),
				}
			}
			Kind::Subscribe { handle: value, .. } => {
				let corr = u32::from_le_bytes([op.request[2], op.request[3], op.request[4], op.request[5]]);
				if error_code.is_some() {
					self.refuse(chan, &op.request, bluetooth_gatt::OP_SUBSCRIBE, Error::Denied);
				} else if let Some((producer, consumer)) = channel_with_depth(32) {
					let mut out = [0u8; 64];
					if let Some(len) = bluetooth_gatt::subscribe_reply_ok(corr, &mut out)
						&& send_caps_blocking(chan, &out[..len], &[consumer])
					{
						self.grants[grant_index].subscriptions.push((value, producer));
					} else {
						close(producer);
						close(consumer);
					}
				} else {
					self.refuse(chan, &op.request, bluetooth_gatt::OP_SUBSCRIBE, Error::Exhausted);
				}
			}
		}
		self.next_gatt(at, handle);
	}

	// A NOTIFICATION, to every grant subscribed to its handle on this peer.
	pub(crate) fn gatt_notification(&mut self, at: usize, handle: u16, pdu: &[u8]) {
		if pdu.len() < 3 {
			return;
		}
		let Some(peer) = self.controllers[at].link(handle).map(|link| link.peer) else { return };
		let attribute = u16::from_le_bytes([pdu[1], pdu[2]]);
		let value = GattValue { handle: attribute, value: pdu[3..pdu.len().min(3 + 244)].to_vec() };
		for grant in self.grants.iter_mut().filter(|grant| grant.at == at && grant.peer == peer) {
			grant.subscriptions.retain(|&(subscribed, producer)| {
				if subscribed != attribute {
					return true;
				}
				let mut frame = [0u8; 300];
				let mut handles = wire::Handles::new();
				let Some(len) = bluetooth_gatt::subscribe_frame(0, &value, &mut frame, &mut handles) else { return true };
				let kept = !matches!(try_send_outcome(producer, &frame[..len], 0), SendOutcome::Failed);
				if !kept {
					close(producer);
				}
				kept
			});
		}
	}

	// A LINK GONE: its queued operations are answered closed.
	pub(crate) fn gatt_gone(&self, link: &mut Link) {
		let pending: Vec<Op> = link.gatt.busy.take().into_iter().chain(link.gatt.queue.drain(..)).collect();
		for op in pending {
			let op_code = u16::from_le_bytes([op.request[0], op.request[1]]);
			self.refuse(op.grant, &op.request, op_code, Error::Closed);
		}
		link.gatt.services = None;
	}
}

fn granted_services(grant: &Grant, found: &[(u16, u16, u16)]) -> Vec<GattService> {
	found.iter().filter(|(uuid, _, _)| grant.services.contains(uuid) && !STACK_SERVICES.contains(uuid)).map(|(uuid, start, end)| GattService { uuid: *uuid, start: *start, end: *end }).collect()
}

// A GRANT ENDED - its launch is over, or its holder closed it: its connection and its subscriptions go.
pub(crate) fn end_grant(grant: Grant) {
	close(grant.chan);
	close(grant.owner);
	for (_, producer) in grant.subscriptions {
		close(producer);
	}
}
