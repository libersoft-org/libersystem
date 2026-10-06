// THE PHONE'S NETWORK ACCESS POINT: BNEP's NAP role on L2CAP's BNEP PSM, and behind it the smallest network a PAN user
// can be tested against - an address to lease, a gateway that answers ARP and ping.
//
// A TEST FIXTURE, DEVELOPMENT-ONLY, AND ITS PROTOCOLS ARE ITS OWN: BNEP's setup and its four frame forms, ARP, DHCP's
// offer and acknowledgement and ICMP's echo, written from their specifications apart from the host's `service_logic::
// bnep` and NetworkService's stack. The owner's preferred oracle - a harness access point bridged to a host network
// where `dnsmasq` answers DHCP - waits on adopting Bumble; until then the addressing a PAN user receives is this
// fixture's, and said so.
//
//   the setup     a setup request for NAP from PANU is answered success; any other, "not allowed"
//   the network   192.168.44.0/24: the phone 192.168.44.1, the gateway and the resolver; the host leased .2 for an hour
//   what it says  the lease it gave, and each ping it answered

use super::*;

// BNEP's PSM, and the L2CAP MTU its channel needs for a whole Ethernet frame.
pub(super) const BNEP_PSM: u16 = 0x000f;
pub(super) const BNEP_MTU: u16 = 1691;
const GATEWAY: [u8; 4] = [192, 168, 44, 1];
const LEASED: [u8; 4] = [192, 168, 44, 2];

// The internet checksum: the ones' complement of the ones' complement sum of 16-bit words.
fn checksum(bytes: &[u8]) -> u16 {
	let mut sum: u32 = 0;
	for pair in bytes.chunks(2) {
		let word = if pair.len() == 2 { u16::from_be_bytes([pair[0], pair[1]]) } else { u16::from(pair[0]) << 8 };
		sum += u32::from(word);
	}
	while sum >> 16 != 0 {
		sum = (sum & 0xffff) + (sum >> 16);
	}
	!(sum as u16)
}

// An IPv4 packet from `source` to `destination` carrying `protocol`'s `payload`, its header's checksum made.
fn ipv4(source: [u8; 4], destination: [u8; 4], protocol: u8, payload: &[u8]) -> Vec<u8> {
	let total = (20 + payload.len()) as u16;
	let mut header = alloc::vec![0x45, 0];
	header.extend_from_slice(&total.to_be_bytes());
	header.extend_from_slice(&[0, 0, 0x40, 0, 64, protocol, 0, 0]);
	header.extend_from_slice(&source);
	header.extend_from_slice(&destination);
	let sum = checksum(&header);
	header[10..12].copy_from_slice(&sum.to_be_bytes());
	header.extend_from_slice(payload);
	header
}

impl World {
	// WHAT THE HOST SENT on the access point's channel: its setup, or a frame of one of BNEP's four forms - answered with
	// what the little network behind the phone says back, in BNEP's general form.
	pub(super) fn nap_receive(&mut self, at: usize, remote: u16, packet: &[u8]) -> Vec<Out> {
		let mut out = Vec::new();
		let Some(&first) = packet.first() else { return out };
		let phone = self.devices[at].spec.address;
		let name = self.short(at);
		let host = self.devices[at].link.as_ref().map(|link| link.nap_host).unwrap_or_default();
		let (destination, source, rest): ([u8; 6], [u8; 6], &[u8]) = match first & 0x7f {
			0x01 => {
				// THE SETUP: NAP asked for, by PANU, in 16-bit UUIDs.
				let answer: u16 = if packet.get(1..7) == Some(&[0x01, 0x02, 0x11, 0x16, 0x11, 0x15]) {
					0x0000
				} else if packet.get(1) == Some(&0x01) {
					0x0004
				} else {
					return out;
				};
				if answer == 0 {
					self.say(format!("{name}'s access point took the host's PAN setup"));
				}
				let mut reply = alloc::vec![0x01, 0x02];
				reply.extend_from_slice(&answer.to_be_bytes());
				out.extend(self.send_pdu(at, remote, &reply));
				return out;
			}
			0x00 if packet.len() >= 13 => (packet[1..7].try_into().unwrap_or_default(), packet[7..13].try_into().unwrap_or_default(), &packet[13..]),
			0x02 => (phone, host, &packet[1..]),
			0x03 if packet.len() >= 7 => (phone, packet[1..7].try_into().unwrap_or_default(), &packet[7..]),
			0x04 if packet.len() >= 7 => (packet[1..7].try_into().unwrap_or_default(), host, &packet[7..]),
			_ => return out,
		};
		let _ = destination;
		if rest.len() < 2 {
			return out;
		}
		let ethertype = u16::from_be_bytes([rest[0], rest[1]]);
		let body = &rest[2..];
		let mut replies: Vec<(u16, Vec<u8>)> = Vec::new();
		let mut lines = Vec::new();
		let mut learned = None;
		match ethertype {
			// ARP: a request for the gateway's address, answered with the phone's.
			0x0806 if body.len() >= 28 && body[7] == 1 && body[24..28] == GATEWAY => {
				let mut reply = alloc::vec![0, 1, 8, 0, 6, 4, 0, 2];
				reply.extend_from_slice(&phone);
				reply.extend_from_slice(&GATEWAY);
				reply.extend_from_slice(&body[8..18]);
				learned = Some(<[u8; 6]>::try_from(&body[8..14]).unwrap_or_default());
				replies.push((0x0806, reply));
			}
			0x0800 if body.len() >= 20 => {
				let length = usize::from(body[0] & 0x0f) * 4;
				let protocol = body[9];
				let payload = body.get(length..usize::from(u16::from_be_bytes([body[2], body[3]])).min(body.len())).unwrap_or(&[]);
				let source_ip: [u8; 4] = body[12..16].try_into().unwrap_or_default();
				let destination_ip: [u8; 4] = body[16..20].try_into().unwrap_or_default();
				match protocol {
					// DHCP: a discover offered the lease, a request for it acknowledged.
					17 if payload.len() >= 8 + 240 && u16::from_be_bytes([payload[2], payload[3]]) == 67 => {
						let dhcp = &payload[8..];
						let message = dhcp_message_type(dhcp);
						let kind = match message {
							Some(1) => 2,
							Some(3) => 5,
							_ => 0,
						};
						if kind != 0 {
							let client: [u8; 6] = dhcp[28..34].try_into().unwrap_or_default();
							learned = Some(client);
							let mut reply = alloc::vec![2, 1, 6, 0];
							reply.extend_from_slice(&dhcp[4..8]);
							reply.extend_from_slice(&[0; 8]);
							reply.extend_from_slice(&LEASED);
							reply.extend_from_slice(&GATEWAY);
							reply.extend_from_slice(&[0; 4]);
							reply.extend_from_slice(&dhcp[28..44]);
							reply.extend_from_slice(&[0; 192]);
							reply.extend_from_slice(&[99, 130, 83, 99, 53, 1, kind, 54, 4]);
							reply.extend_from_slice(&GATEWAY);
							reply.extend_from_slice(&[51, 4, 0, 0, 0x0e, 0x10, 1, 4, 255, 255, 255, 0, 3, 4]);
							reply.extend_from_slice(&GATEWAY);
							reply.extend_from_slice(&[6, 4]);
							reply.extend_from_slice(&GATEWAY);
							reply.push(255);
							let mut udp = 67u16.to_be_bytes().to_vec();
							udp.extend_from_slice(&68u16.to_be_bytes());
							udp.extend_from_slice(&((8 + reply.len()) as u16).to_be_bytes());
							udp.extend_from_slice(&[0, 0]);
							udp.extend_from_slice(&reply);
							replies.push((0x0800, ipv4(GATEWAY, [255; 4], 17, &udp)));
							if kind == 5 {
								lines.push(format!("{name}'s access point leased 192.168.44.2 to the host"));
							}
						}
					}
					// ICMP ECHO to the gateway, answered.
					1 if destination_ip == GATEWAY && payload.first() == Some(&8) && payload.len() >= 8 => {
						let mut reply = payload.to_vec();
						reply[0] = 0;
						reply[2] = 0;
						reply[3] = 0;
						let sum = checksum(&reply);
						reply[2..4].copy_from_slice(&sum.to_be_bytes());
						replies.push((0x0800, ipv4(GATEWAY, source_ip, 1, &reply)));
						lines.push(format!("{name}'s access point answered a ping from {}.{}.{}.{}", source_ip[0], source_ip[1], source_ip[2], source_ip[3]));
					}
					_ => {}
				}
			}
			_ => {}
		}
		let host = learned.unwrap_or(if source == [0; 6] { host } else { source });
		if let Some(link) = self.devices[at].link.as_mut() {
			link.nap_host = host;
		}
		for line in lines {
			self.say(line);
		}
		// EVERY ANSWER IN THE GENERAL FORM, to the host's own address - broadcast where it has none yet.
		for (ethertype, payload) in replies {
			let mut packet = alloc::vec![0x00];
			packet.extend_from_slice(&if host == [0; 6] { [0xff; 6] } else { host });
			packet.extend_from_slice(&phone);
			packet.extend_from_slice(&ethertype.to_be_bytes());
			packet.extend_from_slice(&payload);
			out.extend(self.send_pdu(at, remote, &packet));
		}
		out
	}
}

// The DHCP message type, from the options past the magic cookie.
fn dhcp_message_type(dhcp: &[u8]) -> Option<u8> {
	if dhcp.get(236..240) != Some(&[99, 130, 83, 99]) {
		return None;
	}
	let mut at = 240;
	while let Some(&code) = dhcp.get(at) {
		match code {
			255 => return None,
			0 => at += 1,
			_ => {
				let len = usize::from(*dhcp.get(at + 1)?);
				if code == 53 {
					return dhcp.get(at + 2).copied();
				}
				at += 2 + len;
			}
		}
	}
	None
}
