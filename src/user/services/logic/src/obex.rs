//! OBEX OBJECT PUSH, both roles, as pure state machines: the packets, the headers OPP uses, a client pushing one
//! object and a server receiving one.
//!
//! WHAT OBEX IS HERE. A request-response protocol, one request outstanding: CONNECT settles the largest packet each
//! side takes, PUT carries the object's NAME, TYPE and LENGTH and its BODY in as many packets as it needs - the last
//! with END-OF-BODY and the final bit - and DISCONNECT ends the session. Every response carries the final bit. The
//! transport - RFCOMM, or L2CAP where the peer's record offers it - is the caller's; a packet split across its frames
//! is put back together by `Assembler`, bounded by the largest packet this side said it takes.
//!
//! THE NAME IS THE PEER'S WORDS. It is decoded from UTF-16 and handed on as text; nothing here, and nothing that
//! reads it, treats it as a path.
//!
//! BOUNDS. A server is told the most it may accept: an object that declares more is refused before a byte of it
//! is taken (`Request Entity Too Large`), and one that passes it while arriving is refused at that packet.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

/// The OBEX version this side speaks: 1.0.
pub const VERSION: u8 = 0x10;
/// The smallest maximum packet a peer may state, and the largest this side states.
pub const MIN_PACKET: usize = 255;
pub const MAX_PACKET: usize = 4096;

pub mod opcode {
	pub const CONNECT: u8 = 0x80;
	pub const DISCONNECT: u8 = 0x81;
	pub const PUT: u8 = 0x02;
	pub const PUT_FINAL: u8 = 0x82;
	pub const ABORT: u8 = 0xff;
}

pub mod response {
	pub const CONTINUE: u8 = 0x90;
	pub const SUCCESS: u8 = 0xa0;
	pub const BAD_REQUEST: u8 = 0xc0;
	pub const FORBIDDEN: u8 = 0xc3;
	pub const NOT_ACCEPTABLE: u8 = 0xc6;
	pub const TOO_LARGE: u8 = 0xcd;
	pub const UNAVAILABLE: u8 = 0xd3;
}

pub mod header {
	pub const NAME: u8 = 0x01;
	pub const TYPE: u8 = 0x42;
	pub const LENGTH: u8 = 0xc3;
	pub const BODY: u8 = 0x48;
	pub const END_OF_BODY: u8 = 0x49;
	pub const CONNECTION_ID: u8 = 0xcb;
}

/// One header: its id, and its value by the id's top two bits - text, bytes, a byte or four.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Header {
	Text(u8, String),
	Bytes(u8, Vec<u8>),
	Byte(u8, u8),
	Word(u8, u32),
}

impl Header {
	pub fn id(&self) -> u8 {
		match self {
			Header::Text(id, _) | Header::Bytes(id, _) | Header::Byte(id, _) | Header::Word(id, _) => *id,
		}
	}

	fn encode(&self, out: &mut Vec<u8>) {
		match self {
			Header::Text(id, text) => {
				let units: Vec<u16> = text.encode_utf16().chain(core::iter::once(0)).collect();
				out.push(*id);
				out.extend_from_slice(&((3 + units.len() * 2) as u16).to_be_bytes());
				for unit in units {
					out.extend_from_slice(&unit.to_be_bytes());
				}
			}
			Header::Bytes(id, bytes) => {
				out.push(*id);
				out.extend_from_slice(&((3 + bytes.len()) as u16).to_be_bytes());
				out.extend_from_slice(bytes);
			}
			Header::Byte(id, value) => out.extend_from_slice(&[*id, *value]),
			Header::Word(id, value) => {
				out.push(*id);
				out.extend_from_slice(&value.to_be_bytes());
			}
		}
	}

	fn encoded_len(&self) -> usize {
		match self {
			Header::Text(_, text) => 3 + (text.encode_utf16().count() + 1) * 2,
			Header::Bytes(_, bytes) => 3 + bytes.len(),
			Header::Byte(..) => 2,
			Header::Word(..) => 5,
		}
	}
}

/// A packet: a request's opcode or a response's code, the CONNECT fields where it has them, and its headers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Packet {
	pub code: u8,
	/// CONNECT's version, flags and the largest packet its sender takes.
	pub connect: Option<(u8, u8, u16)>,
	pub headers: Vec<Header>,
}

/// Why a packet was not read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	Short,
	Length,
	Header,
}

impl Packet {
	pub fn new(code: u8, headers: Vec<Header>) -> Packet {
		Packet { code, connect: None, headers }
	}

	pub fn encode(&self) -> Vec<u8> {
		let mut out = alloc::vec![self.code, 0, 0];
		if let Some((version, flags, max)) = self.connect {
			out.push(version);
			out.push(flags);
			out.extend_from_slice(&max.to_be_bytes());
		}
		for header in &self.headers {
			header.encode(&mut out);
		}
		let len = out.len() as u16;
		out[1..3].copy_from_slice(&len.to_be_bytes());
		out
	}

	/// A whole packet. `connect` says whether it carries CONNECT's fields: a CONNECT request, or the response to one.
	pub fn decode(bytes: &[u8], connect: bool) -> Result<Packet, Refusal> {
		if bytes.len() < 3 {
			return Err(Refusal::Short);
		}
		let len = usize::from(u16::from_be_bytes([bytes[1], bytes[2]]));
		if len != bytes.len() || len < 3 {
			return Err(Refusal::Length);
		}
		let mut at = 3;
		let fields = if connect {
			if len < 7 {
				return Err(Refusal::Short);
			}
			at = 7;
			Some((bytes[3], bytes[4], u16::from_be_bytes([bytes[5], bytes[6]])))
		} else {
			None
		};
		let mut headers = Vec::new();
		while at < len {
			let id = bytes[at];
			let header = match id >> 6 {
				0 | 1 => {
					let size = usize::from(u16::from_be_bytes([*bytes.get(at + 1).ok_or(Refusal::Header)?, *bytes.get(at + 2).ok_or(Refusal::Header)?]));
					if size < 3 || at + size > len {
						return Err(Refusal::Header);
					}
					let value = &bytes[at + 3..at + size];
					at += size;
					if id >> 6 == 0 {
						if value.len() % 2 != 0 {
							return Err(Refusal::Header);
						}
						let units: Vec<u16> = value.chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).take_while(|&unit| unit != 0).collect();
						Header::Text(id, String::from_utf16_lossy(&units))
					} else {
						Header::Bytes(id, value.to_vec())
					}
				}
				2 => {
					let value = *bytes.get(at + 1).ok_or(Refusal::Header)?;
					at += 2;
					Header::Byte(id, value)
				}
				_ => {
					let value = bytes.get(at + 1..at + 5).ok_or(Refusal::Header)?;
					at += 5;
					Header::Word(id, u32::from_be_bytes([value[0], value[1], value[2], value[3]]))
				}
			};
			headers.push(header);
		}
		Ok(Packet { code: bytes[0], connect: fields, headers })
	}

	fn get(&self, id: u8) -> Option<&Header> {
		self.headers.iter().find(|header| header.id() == id)
	}

	fn body(&self) -> Option<(&[u8], bool)> {
		self.headers.iter().find_map(|header| match header {
			Header::Bytes(header::BODY, bytes) => Some((bytes.as_slice(), false)),
			Header::Bytes(header::END_OF_BODY, bytes) => Some((bytes.as_slice(), true)),
			_ => None,
		})
	}
}

/// PACKETS OUT OF A BYTE STREAM: a transport's frames put back together by the packet's own length, bounded.
#[derive(Default)]
pub struct Assembler {
	bytes: Vec<u8>,
	limit: usize,
}

impl Assembler {
	pub fn new(limit: usize) -> Assembler {
		Assembler { bytes: Vec::new(), limit }
	}

	/// The whole packets these bytes complete - or `Err` for a packet longer than the bound, after which the stream
	/// is not read again.
	pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, Refusal> {
		self.bytes.extend_from_slice(bytes);
		let mut out = Vec::new();
		while self.bytes.len() >= 3 {
			let len = usize::from(u16::from_be_bytes([self.bytes[1], self.bytes[2]]));
			if len < 3 || len > self.limit {
				self.bytes.clear();
				return Err(Refusal::Length);
			}
			if self.bytes.len() < len {
				break;
			}
			out.push(self.bytes.drain(..len).collect());
		}
		Ok(out)
	}
}

// ------------------------------------------------------------------ the client

/// Why a push did not end in success.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Failure {
	/// The peer answered with this response code.
	Refused(u8),
	/// The peer's packet could not be read.
	Malformed,
	/// The pusher abandoned it.
	Aborted,
}

/// What a pushing client asks its caller to do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ClientOut {
	/// Send this packet on the transport.
	Send(Vec<u8>),
	/// The object is through and the session ended: the bytes the peer accepted.
	Done(Result<u64, Failure>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ClientState {
	Connecting,
	// Waiting for data, or for the end of it, with nothing outstanding.
	Idle,
	// A PUT is outstanding.
	Putting,
	// The final PUT is outstanding.
	Finishing,
	Disconnecting,
	Done,
}

/// ONE OBJECT PUSHED: CONNECT, a PUT in as many packets as the body needs, DISCONNECT.
pub struct PushClient {
	state: ClientState,
	peer_max: usize,
	name: String,
	kind: Option<String>,
	length: Option<u32>,
	connection: Option<u32>,
	headers_sent: bool,
	pending: VecDeque<u8>,
	ended: bool,
	sent: u64,
	result: Option<Result<u64, Failure>>,
}

/// What a client holds of the caller's data before it says no more: two packets' worth.
pub const CLIENT_BUFFER: usize = 2 * MAX_PACKET;

impl PushClient {
	/// A push of `name`, of `kind` where the caller knows it and `length` bytes where it does: the CONNECT to send.
	pub fn new(name: &str, kind: Option<&str>, length: Option<u32>) -> (PushClient, Vec<u8>) {
		let client = PushClient { state: ClientState::Connecting, peer_max: MIN_PACKET, name: String::from(name), kind: kind.map(String::from), length, connection: None, headers_sent: false, pending: VecDeque::new(), ended: false, sent: 0, result: None };
		let connect = Packet { code: opcode::CONNECT, connect: Some((VERSION, 0, MAX_PACKET as u16)), headers: Vec::new() };
		(client, connect.encode())
	}

	/// How many more bytes `write` takes now.
	pub fn room(&self) -> usize {
		if self.ended || self.state == ClientState::Done { 0 } else { CLIENT_BUFFER.saturating_sub(self.pending.len()) }
	}

	/// The caller's bytes of the object, as many as there is room for; and a packet to send where the session was
	/// waiting for them.
	pub fn write(&mut self, bytes: &[u8]) -> (usize, Vec<ClientOut>) {
		let take = bytes.len().min(self.room());
		self.pending.extend(&bytes[..take]);
		(take, self.next())
	}

	/// The object's end: the final PUT goes once what is queued has.
	pub fn finish(&mut self) -> Vec<ClientOut> {
		self.ended = true;
		self.next()
	}

	/// ABANDON IT: an ABORT where a PUT is under way, and the push ends failed.
	pub fn abort(&mut self) -> Vec<ClientOut> {
		let mut out = Vec::new();
		if matches!(self.state, ClientState::Idle | ClientState::Putting) && self.headers_sent {
			out.push(ClientOut::Send(Packet::new(opcode::ABORT, self.connection_header()).encode()));
		}
		self.state = ClientState::Done;
		out.push(ClientOut::Done(Err(Failure::Aborted)));
		out
	}

	fn connection_header(&self) -> Vec<Header> {
		self.connection.map(|id| alloc::vec![Header::Word(header::CONNECTION_ID, id)]).unwrap_or_default()
	}

	// THE NEXT PACKET, where nothing is outstanding and there is something to say: the headers not yet sent, a full
	// packet's worth of body, or the end.
	fn next(&mut self) -> Vec<ClientOut> {
		if self.state != ClientState::Idle {
			return Vec::new();
		}
		let mut headers = self.connection_header();
		if !self.headers_sent {
			headers.push(Header::Text(header::NAME, self.name.clone()));
			if let Some(kind) = self.kind.as_ref() {
				let mut bytes = kind.as_bytes().to_vec();
				bytes.push(0);
				headers.push(Header::Bytes(header::TYPE, bytes));
			}
			if let Some(length) = self.length {
				headers.push(Header::Word(header::LENGTH, length));
			}
		}
		let used = 3 + headers.iter().map(Header::encoded_len).sum::<usize>();
		let room = self.peer_max.saturating_sub(used + 3);
		let last = self.ended && self.pending.len() <= room;
		if self.headers_sent && !last && self.pending.len() < room {
			return Vec::new();
		}
		let take = self.pending.len().min(room);
		let chunk: Vec<u8> = self.pending.drain(..take).collect();
		self.sent += take as u64;
		if last {
			headers.push(Header::Bytes(header::END_OF_BODY, chunk));
		} else if !chunk.is_empty() {
			headers.push(Header::Bytes(header::BODY, chunk));
		}
		self.headers_sent = true;
		self.state = if last { ClientState::Finishing } else { ClientState::Putting };
		alloc::vec![ClientOut::Send(Packet::new(if last { opcode::PUT_FINAL } else { opcode::PUT }, headers).encode())]
	}

	/// THE PEER'S RESPONSE to what is outstanding.
	pub fn receive(&mut self, bytes: &[u8]) -> Vec<ClientOut> {
		// A CONNECT'S SUCCESS carries the fields; its refusal does not.
		let connecting = self.state == ClientState::Connecting && bytes.first() == Some(&response::SUCCESS);
		let Ok(packet) = Packet::decode(bytes, connecting) else {
			return self.fail(Failure::Malformed);
		};
		match self.state {
			ClientState::Connecting => {
				if packet.code != response::SUCCESS {
					return self.fail(Failure::Refused(packet.code));
				}
				let (_, _, max) = packet.connect.unwrap_or((VERSION, 0, MIN_PACKET as u16));
				self.peer_max = usize::from(max).clamp(MIN_PACKET, MAX_PACKET);
				self.connection = packet.get(header::CONNECTION_ID).and_then(|header| if let Header::Word(_, id) = header { Some(*id) } else { None });
				self.state = ClientState::Idle;
				self.next()
			}
			ClientState::Putting => {
				if packet.code != response::CONTINUE {
					return self.fail(Failure::Refused(packet.code));
				}
				self.state = ClientState::Idle;
				self.next()
			}
			ClientState::Finishing => {
				if packet.code != response::SUCCESS {
					return self.fail(Failure::Refused(packet.code));
				}
				self.result = Some(Ok(self.sent));
				self.state = ClientState::Disconnecting;
				alloc::vec![ClientOut::Send(Packet::new(opcode::DISCONNECT, self.connection_header()).encode())]
			}
			ClientState::Disconnecting => {
				self.state = ClientState::Done;
				alloc::vec![ClientOut::Done(self.result.take().unwrap_or(Ok(self.sent)))]
			}
			ClientState::Idle | ClientState::Done => Vec::new(),
		}
	}

	fn fail(&mut self, failure: Failure) -> Vec<ClientOut> {
		let mut out = Vec::new();
		// A SESSION THAT IS STILL UP is ended politely; one whose CONNECT failed has nothing to end.
		if !matches!(self.state, ClientState::Connecting | ClientState::Done | ClientState::Disconnecting) {
			out.push(ClientOut::Send(Packet::new(opcode::DISCONNECT, self.connection_header()).encode()));
		}
		self.state = ClientState::Done;
		out.push(ClientOut::Done(Err(failure)));
		out
	}

	pub fn done(&self) -> bool {
		self.state == ClientState::Done
	}
}

// ------------------------------------------------------------------ the server

/// What a receiving server tells its caller.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ServerEvent {
	/// An object begins: the peer's name for it, its type and the length it declared, as the peer gave them.
	Offered { name: String, kind: Option<String>, length: Option<u32> },
	/// The next bytes of its body.
	Data(Vec<u8>),
	/// It arrived whole.
	Complete,
	/// It did not: the response it was refused with, or the peer's ABORT, or the session ended under it.
	Failed(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ServerState {
	Disconnected,
	Connected,
	Receiving,
	// One object was taken or refused: this server takes no other.
	Over,
}

/// ONE OBJECT RECEIVED, at most `max_bytes` of it.
pub struct PushServer {
	state: ServerState,
	max_bytes: u64,
	received: u64,
	offered: bool,
}

impl PushServer {
	pub fn new(max_bytes: u64) -> PushServer {
		PushServer { state: ServerState::Disconnected, max_bytes, received: 0, offered: false }
	}

	/// Whether the next packet is a CONNECT, which carries its own fields.
	pub fn expects_connect(&self) -> bool {
		self.state == ServerState::Disconnected
	}

	/// ONE REQUEST: the response to send, and what it meant.
	pub fn receive(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<ServerEvent>) {
		let connecting = bytes.first() == Some(&opcode::CONNECT);
		let Ok(packet) = Packet::decode(bytes, connecting) else {
			return (Packet::new(response::BAD_REQUEST, Vec::new()).encode(), self.failed(response::BAD_REQUEST));
		};
		let mut events = Vec::new();
		let reply = match packet.code {
			opcode::CONNECT => {
				self.state = if self.state == ServerState::Disconnected { ServerState::Connected } else { self.state };
				Packet { code: response::SUCCESS, connect: Some((VERSION, 0, MAX_PACKET as u16)), headers: Vec::new() }
			}
			opcode::DISCONNECT => {
				if self.state == ServerState::Receiving {
					events.extend(self.failed(response::BAD_REQUEST));
				}
				Packet::new(response::SUCCESS, Vec::new())
			}
			opcode::ABORT => {
				if self.state == ServerState::Receiving {
					events.extend(self.failed(opcode::ABORT));
				}
				Packet::new(response::SUCCESS, Vec::new())
			}
			opcode::PUT | opcode::PUT_FINAL => {
				if !matches!(self.state, ServerState::Connected | ServerState::Receiving) {
					return (Packet::new(if self.state == ServerState::Over { response::UNAVAILABLE } else { response::BAD_REQUEST }, Vec::new()).encode(), Vec::new());
				}
				let code = self.put(&packet, &mut events);
				Packet::new(code, Vec::new())
			}
			_ => Packet::new(response::BAD_REQUEST, Vec::new()),
		};
		(reply.encode(), events)
	}

	fn put(&mut self, packet: &Packet, events: &mut Vec<ServerEvent>) -> u8 {
		if !self.offered {
			let name = match packet.get(header::NAME) {
				Some(Header::Text(_, name)) if !name.is_empty() => name.clone(),
				// AN OBJECT WITH NO NAME is not one Object Push carries.
				_ => {
					events.extend(self.failed(response::BAD_REQUEST));
					return response::BAD_REQUEST;
				}
			};
			let kind = match packet.get(header::TYPE) {
				Some(Header::Bytes(_, bytes)) => Some(String::from_utf8_lossy(bytes.strip_suffix(&[0]).unwrap_or(bytes)).into_owned()),
				_ => None,
			};
			let length = match packet.get(header::LENGTH) {
				Some(Header::Word(_, length)) => Some(*length),
				_ => None,
			};
			// AN OBJECT THAT DECLARES MORE THAN THE BOUND is refused before a byte of it is taken.
			if length.is_some_and(|length| u64::from(length) > self.max_bytes) {
				events.extend(self.failed(response::TOO_LARGE));
				return response::TOO_LARGE;
			}
			self.offered = true;
			self.state = ServerState::Receiving;
			events.push(ServerEvent::Offered { name, kind, length });
		}
		let (body, _) = packet.body().unwrap_or((&[], false));
		self.received += body.len() as u64;
		// ONE THAT PASSES THE BOUND WHILE ARRIVING is refused at that packet.
		if self.received > self.max_bytes {
			events.extend(self.failed(response::TOO_LARGE));
			return response::TOO_LARGE;
		}
		if !body.is_empty() {
			events.push(ServerEvent::Data(body.to_vec()));
		}
		if packet.code != opcode::PUT_FINAL {
			return response::CONTINUE;
		}
		self.state = ServerState::Over;
		events.push(ServerEvent::Complete);
		response::SUCCESS
	}

	// THE OBJECT FAILED: said to the caller where one was offered or expected, and this server takes no other.
	fn failed(&mut self, code: u8) -> Vec<ServerEvent> {
		let live = matches!(self.state, ServerState::Connected | ServerState::Receiving);
		self.state = ServerState::Over;
		if live { alloc::vec![ServerEvent::Failed(code)] } else { Vec::new() }
	}

	/// The transport went: an object under way did not arrive whole.
	pub fn lost(&mut self) -> Vec<ServerEvent> {
		if self.state == ServerState::Receiving { self.failed(response::UNAVAILABLE) } else { Vec::new() }
	}

	pub fn received(&self) -> u64 {
		self.received
	}

	pub fn over(&self) -> bool {
		self.state == ServerState::Over
	}
}

#[cfg(test)]
mod tests;
