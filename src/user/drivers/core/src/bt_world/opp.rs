// OBJECT PUSH: an OBEX server on a device's own Object Push record that takes what the host pushes, and the phone's
// client that pushes an object to the host on the gate's word.
//
// A TEST FIXTURE, DEVELOPMENT-ONLY, AND ITS OBEX IS ITS OWN: packets read by their own length field and headers by
// the two bits of their id, written from the specification apart from the host's `service_logic::obex` - so an object
// both agree arrived whole is one two implementations agree on. The same holds for the enhanced retransmission mode
// its L2CAP channel runs in, written apart from `service_logic::l2cap_bredr::Ertm`: every I-frame acknowledged at once,
// a poll answered with the final bit, the frame check sequence checked and made - and no retransmission, the link
// being reliable.
//
//   receiving   CONNECT answered with a 1024-byte packet; each PUT's name, length and body taken, CONTINUE until the
//               final one, then SUCCESS; what arrived said with its length and an FNV-1a digest of its bytes. The phone's
//               record offers it on L2CAP (its GOEP PSM) as well as RFCOMM, the serial device's on RFCOMM alone
//   pushing     five seconds after the gate's word - time for a receiver to start waiting - the phone pages the host
//               where it has no link, opens the host's Object Push channel, and pushes `fixture-note.txt`, `text/plain`,
//               of the size the gate asked: numbered lines, so a short or reordered copy shows

use super::*;

// The host's Object Push channel, as its record names it.
pub(super) const HOST_CHANNEL: u8 = 9;
// The largest packet a device takes.
const DEVICE_MAX: u16 = 1024;
// When a push the gate asked for starts: five seconds of 10 ms ticks.
const PUSH_DELAY: u64 = 500;
const NAME: &str = "fixture-note.txt";

#[derive(Default)]
pub(super) struct Opp {
	// The device's server: a packet arriving in pieces, and the object so far.
	pub assembling: Vec<u8>,
	pub name: String,
	pub body: Vec<u8>,
	// The phone's client: the object it pushes, how far it got, and the host's packet size.
	pub push: Option<Push>,
}

pub(super) struct Push {
	pub body: Vec<u8>,
	pub sent: usize,
	pub host_max: usize,
	pub stage: Stage,
	pub assembling: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Stage {
	Channel,
	Connect,
	Put,
	Final,
	Disconnect,
}

// ONE ENHANCED RETRANSMISSION CHANNEL's state on this side: the next sequence number it sends, the next it expects,
// and an SDU arriving in segments.
#[derive(Default)]
pub(super) struct Ertm {
	pub tx_seq: u8,
	pub expected: u8,
	pub sdu: Vec<u8>,
}

// FNV-1a, 32 bits: what the gate compares a copy against.
pub fn digest(bytes: &[u8]) -> u32 {
	bytes.iter().fold(0x811c_9dc5u32, |hash, &byte| (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193))
}

// The object the phone pushes: numbered lines, cut to `size` bytes.
pub fn note(size: usize) -> Vec<u8> {
	let mut out = Vec::with_capacity(size + 64);
	let mut line = 1;
	while out.len() < size {
		out.extend_from_slice(format!("line {line:05} of the phone's note\n").as_bytes());
		line += 1;
	}
	out.truncate(size);
	out
}

// L2CAP's frame check sequence: CRC-16, x^16 + x^15 + x^2 + 1, least significant bit first, from zero.
pub fn fcs16(bytes: &[u8]) -> u16 {
	let mut crc: u16 = 0;
	for &byte in bytes {
		crc ^= u16::from(byte);
		for _ in 0..8 {
			crc = if crc & 1 != 0 { (crc >> 1) ^ 0xa001 } else { crc >> 1 };
		}
	}
	crc
}

// AN ERTM FRAME'S PAYLOAD for `send_pdu`: its control field, its information and its check over the basic header too.
pub(super) fn ertm_payload(remote: u16, control: u16, information: &[u8]) -> Vec<u8> {
	let len = (2 + information.len() + 2) as u16;
	let mut covered = len.to_le_bytes().to_vec();
	covered.extend_from_slice(&remote.to_le_bytes());
	covered.extend_from_slice(&control.to_le_bytes());
	covered.extend_from_slice(information);
	let check = fcs16(&covered);
	let mut out = covered[4..].to_vec();
	out.extend_from_slice(&check.to_le_bytes());
	out
}

// One header of a packet: its id and its value.
fn headers(mut bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
	let mut out = Vec::new();
	while let Some(&id) = bytes.first() {
		let size = match id >> 6 {
			0 | 1 if bytes.len() >= 3 => usize::from(u16::from_be_bytes([bytes[1], bytes[2]])),
			2 => 2,
			3 => 5,
			_ => return out,
		};
		if size < 2 || size > bytes.len() {
			return out;
		}
		let value = if id >> 6 <= 1 { bytes[3..size].to_vec() } else { bytes[1..size].to_vec() };
		out.push((id, value));
		bytes = &bytes[size..];
	}
	out
}

fn packet(code: u8, body: &[u8]) -> Vec<u8> {
	let mut out = alloc::vec![code, 0, 0];
	out.extend_from_slice(body);
	let len = (out.len() as u16).to_be_bytes();
	out[1] = len[0];
	out[2] = len[1];
	out
}

fn unicode(text: &str) -> Vec<u8> {
	let mut out = Vec::new();
	for unit in text.encode_utf16().chain(core::iter::once(0)) {
		out.extend_from_slice(&unit.to_be_bytes());
	}
	out
}

// Whole packets out of what arrived, by their own length.
fn take_packets(assembling: &mut Vec<u8>, bytes: &[u8]) -> Vec<Vec<u8>> {
	assembling.extend_from_slice(bytes);
	let mut out = Vec::new();
	while assembling.len() >= 3 {
		let len = usize::from(u16::from_be_bytes([assembling[1], assembling[2]]));
		if len < 3 {
			assembling.clear();
			break;
		}
		if assembling.len() < len {
			break;
		}
		out.push(assembling.drain(..len).collect());
	}
	out
}

impl World {
	fn opp(&mut self, at: usize) -> Option<&mut Opp> {
		self.devices[at].link.as_mut().map(|link| &mut link.opp)
	}

	// ------------------------------------------------------------------ receiving

	// WHAT THE HOST SENT TO THE DEVICE'S SERVER: each request's response, to be sent on whichever transport it came by.
	pub(super) fn opp_serve(&mut self, at: usize, bytes: &[u8], transport: &str) -> Vec<Vec<u8>> {
		let name = self.short(at);
		let Some(opp) = self.opp(at) else { return Vec::new() };
		let mut replies = Vec::new();
		let mut lines = Vec::new();
		for request in take_packets(&mut opp.assembling, bytes) {
			match request[0] {
				0x80 => {
					opp.name.clear();
					opp.body.clear();
					let max = DEVICE_MAX.to_be_bytes();
					replies.push(packet(0xa0, &[0x10, 0, max[0], max[1]]));
				}
				0x02 | 0x82 => {
					for (id, value) in headers(&request[3..]) {
						match id {
							0x01 => {
								let units: Vec<u16> = value.chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).take_while(|&unit| unit != 0).collect();
								opp.name = String::from_utf16_lossy(&units);
							}
							0x48 | 0x49 => opp.body.extend_from_slice(&value),
							_ => {}
						}
					}
					if request[0] == 0x82 {
						lines.push(format!("{name} received {} over {transport}: {} bytes, digest {:08x}", opp.name, opp.body.len(), digest(&opp.body)));
						replies.push(packet(0xa0, &[]));
					} else {
						replies.push(packet(0x90, &[]));
					}
				}
				0x81 | 0xff => replies.push(packet(0xa0, &[])),
				_ => replies.push(packet(0xc0, &[])),
			}
		}
		for line in lines {
			self.say(line);
		}
		replies
	}

	// AN SDU ON THE DEVICE'S OBJECT PUSH L2CAP CHANNEL, in enhanced retransmission mode: acknowledged, put together, and
	// served; the responses go back as I-frames.
	pub(super) fn opp_l2cap(&mut self, at: usize, local: u16, remote: u16, payload: &[u8]) -> Vec<Out> {
		let mut out = Vec::new();
		if payload.len() < 4 {
			return out;
		}
		let mut covered = (payload.len() as u16).to_le_bytes().to_vec();
		covered.extend_from_slice(&local.to_le_bytes());
		covered.extend_from_slice(&payload[..payload.len() - 2]);
		let check = u16::from_le_bytes([payload[payload.len() - 2], payload[payload.len() - 1]]);
		if fcs16(&covered) != check {
			let name = self.short(at);
			self.say(format!("l2cap {name} dropped an ERTM frame with a bad check"));
			return out;
		}
		let control = u16::from_le_bytes([payload[0], payload[1]]);
		let information = &payload[2..payload.len() - 2];
		let Some(ertm) = self.devices[at].link.as_mut().and_then(|link| link.chans.iter_mut().find(|chan| chan.local == local)).and_then(|chan| chan.ertm.as_mut()) else { return out };
		let mut complete = None;
		let mut answer = None;
		if control & 1 == 0 {
			// AN I-FRAME: in sequence, its segment kept; acknowledged with a receiver-ready either way.
			let tx_seq = ((control >> 1) & 0x3f) as u8;
			let sar = control >> 14;
			if tx_seq == ertm.expected {
				ertm.expected = (ertm.expected + 1) & 0x3f;
				match sar {
					0 => complete = Some(information.to_vec()),
					1 => ertm.sdu = information.get(2..).unwrap_or(&[]).to_vec(),
					2 => {
						ertm.sdu.extend_from_slice(information);
						complete = Some(core::mem::take(&mut ertm.sdu));
					}
					_ => ertm.sdu.extend_from_slice(information),
				}
			}
			answer = Some(1 | (u16::from(ertm.expected) << 8));
		} else if control & 0x10 != 0 {
			// A POLL: answered with the final bit.
			answer = Some(1 | 0x80 | (u16::from(ertm.expected) << 8));
		}
		if let Some(control) = answer {
			out.extend(self.send_pdu(at, remote, &ertm_payload(remote, control, &[])));
		}
		let Some(sdu) = complete else { return out };
		for reply in self.opp_serve(at, &sdu, "L2CAP") {
			let Some(ertm) = self.devices[at].link.as_mut().and_then(|link| link.chans.iter_mut().find(|chan| chan.local == local)).and_then(|chan| chan.ertm.as_mut()) else { break };
			let control = (u16::from(ertm.tx_seq) << 1) | (u16::from(ertm.expected) << 8);
			ertm.tx_seq = (ertm.tx_seq + 1) & 0x3f;
			out.extend(self.send_pdu(at, remote, &ertm_payload(remote, control, &reply)));
		}
		out
	}

	// ------------------------------------------------------------------ pushing

	// THE GATE'S WORD: a push of `size` bytes, five seconds from now.
	pub(super) fn opp_push(&mut self, at: usize, size: u32) -> Result<Vec<Out>, &'static str> {
		if self.devices[at].spec.opp.is_none() || !self.devices[at].spec.dual {
			return Err("only the phone pushes objects");
		}
		let due = self.now + PUSH_DELAY;
		self.devices[at].opp_wanted = Some((size, due));
		self.say(format!("{} will push {NAME} of {size} bytes", self.short(at)));
		Ok(Vec::new())
	}

	// ITS TIME: the link - paged where there is none - then the host's channel.
	pub(super) fn opp_tick(&mut self, now: u64) -> Vec<Out> {
		let mut out = Vec::new();
		for at in 0..self.devices.len() {
			let Some((size, due)) = self.devices[at].opp_wanted else { continue };
			if now < due {
				continue;
			}
			if self.devices[at].link.is_none() {
				if !self.devices[at].paging {
					match self.act(FIRST_DEVICE + at as u8, 1, 0) {
						Ok((0, outs)) => out.extend(outs),
						// A HOST THAT TAKES NO PAGE takes no push.
						_ => {
							self.devices[at].opp_wanted = None;
							self.say(format!("{}'s push was refused: this host is not connectable", self.short(at)));
						}
					}
				}
				continue;
			}
			self.devices[at].opp_wanted = None;
			let body = note(size as usize);
			if let Some(opp) = self.opp(at) {
				opp.push = Some(Push { body, sent: 0, host_max: 255, stage: Stage::Channel, assembling: Vec::new() });
			}
			out.extend(self.open_channel(at, 0x0003, Purpose::Rfcomm(HOST_CHANNEL)).unwrap_or_default());
		}
		out
	}

	// THE HOST'S CHANNEL IS OPEN: CONNECT.
	pub(super) fn opp_opened(&mut self, at: usize) -> Vec<Out> {
		let Some(push) = self.opp(at).and_then(|opp| opp.push.as_mut()).filter(|push| push.stage == Stage::Channel) else { return Vec::new() };
		push.stage = Stage::Connect;
		let max = DEVICE_MAX.to_be_bytes();
		self.rfcomm_write(at, HOST_CHANNEL, &packet(0x80, &[0x10, 0, max[0], max[1]]))
	}

	// THE CHANNEL WAS REFUSED - the host had no receiver waiting.
	pub(super) fn opp_refused(&mut self, at: usize) {
		if self.opp(at).and_then(|opp| opp.push.take()).is_some() {
			self.say(format!("{}'s push was refused: the host's channel did not open", self.short(at)));
		}
	}

	// THE HOST'S ANSWER to what the push sent last: the next packet, or the end.
	pub(super) fn opp_answer(&mut self, at: usize, bytes: &[u8]) -> Vec<Out> {
		let name = self.short(at);
		let Some(push) = self.opp(at).and_then(|opp| opp.push.as_mut()) else { return Vec::new() };
		let mut sends = Vec::new();
		let mut said = None;
		let mut done = false;
		for answer in take_packets(&mut push.assembling, bytes) {
			let code = answer[0];
			match push.stage {
				Stage::Connect if code == 0xa0 && answer.len() >= 7 => {
					push.host_max = usize::from(u16::from_be_bytes([answer[5], answer[6]])).clamp(255, 4096);
					push.stage = Stage::Put;
				}
				Stage::Put if code == 0x90 => {}
				Stage::Final if code == 0xa0 => {
					said = Some(format!("{name} pushed {NAME}: {} bytes, digest {:08x}, answered success", push.body.len(), digest(&push.body)));
					push.stage = Stage::Disconnect;
					sends.push(packet(0x81, &[]));
					continue;
				}
				Stage::Disconnect => {
					done = true;
					continue;
				}
				_ => {
					said = Some(format!("{name}'s push was answered {code:#04x} after {} bytes", push.sent));
					push.stage = Stage::Disconnect;
					sends.push(packet(0x81, &[]));
					continue;
				}
			}
			// THE NEXT PUT: the headers with the first, the body in what the host's packet takes, the last final.
			let mut headers = Vec::new();
			if push.sent == 0 {
				let name = unicode(NAME);
				headers.push(0x01);
				headers.extend_from_slice(&((3 + name.len()) as u16).to_be_bytes());
				headers.extend_from_slice(&name);
				let kind = b"text/plain\0";
				headers.push(0x42);
				headers.extend_from_slice(&((3 + kind.len()) as u16).to_be_bytes());
				headers.extend_from_slice(kind);
				headers.push(0xc3);
				headers.extend_from_slice(&(push.body.len() as u32).to_be_bytes());
			}
			let room = push.host_max - 3 - headers.len() - 3;
			let take = (push.body.len() - push.sent).min(room);
			let last = push.sent + take == push.body.len();
			headers.push(if last { 0x49 } else { 0x48 });
			headers.extend_from_slice(&((3 + take) as u16).to_be_bytes());
			headers.extend_from_slice(&push.body[push.sent..push.sent + take]);
			push.sent += take;
			if last {
				push.stage = Stage::Final;
			}
			sends.push(packet(if last { 0x82 } else { 0x02 }, &headers));
		}
		if done && let Some(opp) = self.opp(at) {
			opp.push = None;
		}
		if let Some(line) = said {
			self.say(line);
		}
		let mut out = Vec::new();
		for send in sends {
			out.extend(self.rfcomm_write(at, HOST_CHANNEL, &send));
		}
		out
	}
}
