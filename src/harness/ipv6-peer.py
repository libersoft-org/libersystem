#!/usr/bin/env python3
"""A deterministic IPv6 Ethernet peer for the guest, over QEMU's socket netdev.

WHY THIS EXISTS. The harness otherwise builds only QEMU user-mode networking, and Slirp is not an
oracle: it answers what it likes, it will not emit a router advertisement this test wrote, and it
cannot be made to send a malformed one. Every case in the IPv6 milestone that says "a router
advertises X and the host must do Y" needs something on the other end of the wire that does exactly
X and records exactly what came back. That is this.

WHAT IT IS NOT. It is not a router, not a network stack, and not a model of one. It emits the frames
a scenario names, answers the handful of things a guest must have an answer to in order to make
progress, and writes down everything it saw in a form a shell gate can assert on. Anything it does
not understand is recorded and ignored rather than guessed at.

THE TRANSPORT is QEMU's socket netdev: a TCP stream carrying one Ethernet frame per record, each
prefixed by its length as four big-endian bytes. This program LISTENS and QEMU connects to it, so the
port can be chosen before the guest starts.

THE CAPTURE is the point of the whole thing. Every frame from the guest is written to the capture
file as one line naming what it was and the properties a test asserts on - the hop limit, the source
address, whether a Router Alert option was present - so the gate reads a line rather than parsing
packets a second time in shell.
"""

import argparse
import os
import socket
import struct
import sys
import time

ETHERTYPE_IPV6 = 0x86DD
ETHERTYPE_ARP = 0x0806
ETHERTYPE_IPV4 = 0x0800

NEXT_HOP_BY_HOP = 0
NEXT_ICMPV6 = 58
NEXT_UDP = 17

ICMP_DEST_UNREACHABLE = 1
ICMP_PACKET_TOO_BIG = 2
ICMP_TIME_EXCEEDED = 3
ICMP_PARAMETER_PROBLEM = 4
ICMP_ECHO_REQUEST = 128
ICMP_ECHO_REPLY = 129
IP_PROTO_ICMP = 1
IP_PROTO_TCP = 6
ICMP4_ECHO_REQUEST = 8
ICMP4_ECHO_REPLY = 0

# The guest's IPv4 address when no DHCP server answers, and the one this peer answers from. Both are
# fixed by the service's own static fallback, which is what makes the IPv4 path testable in a run
# that deliberately has no DHCP server in it.
GUEST_IPV4 = bytes([10, 0, 2, 15])
PEER_IPV4 = bytes([10, 0, 2, 99])

# THE PEER'S OWN GLOBAL ADDRESSES, all on the /64 it advertises. Several rather than one, because
# the cases that matter are about telling them apart: the resolver must answer only on the address
# the advertisement named, and a connection to the black hole must not be satisfied by the address
# next door.
PEER_GLOBAL = bytes.fromhex("20010db8000a") + bytes(9) + b"\x99"
DNS_ADDRESS = bytes.fromhex("20010db8000a") + bytes(9) + b"\x53"
# ANSWERS NEIGHBOUR SOLICITATIONS AND NOTHING ELSE. A destination that is resolvable and silent is
# what a black-holed path actually looks like; one that is unresolvable fails a step earlier and
# proves nothing about the connection timing.
BLACK_HOLE = bytes.fromhex("20010db8000a") + bytes(9) + b"\x66"

TCP_FIN = 0x01
TCP_SYN = 0x02
TCP_RST = 0x04
TCP_PSH = 0x08
TCP_ACK = 0x10

NEXT_TCP = 6
DNS_PORT = 53

# The names this peer resolves, and what it answers with. A name it does not hold gets NXDOMAIN,
# which is a different outcome from a timeout and the tools distinguish them.
DNS_ZONE = {
	b"ipv6.test": {"aaaa": [PEER_GLOBAL]},
	b"dual.test": {"aaaa": [PEER_GLOBAL], "a": [PEER_IPV4]},
	# ONLY REACHABLE OVER IPv4. The AAAA answer is a black hole, so a resolver that hands both to one
	# sequential open must fall through to the second candidate for the connection to work at all.
	b"fallback.test": {"aaaa": [BLACK_HOLE], "a": [PEER_IPV4]},
	# ANSWERED TRUNCATED OVER UDP AND IN FULL OVER TCP, which is the only way to observe that the
	# resolver actually retried rather than accepting a headerful of nothing.
	b"big.test": {"aaaa": [PEER_GLOBAL], "truncate_over_udp": True},
}

# Two link-local responders for the quoted-error cases. They are DIFFERENT hops complaining about
# successive packets to the same destination, which is the ordering an implementation keying on the
# destination rather than on the quotation gets wrong.
FIRST_RESPONDER = bytes.fromhex("fe80") + bytes(13) + b"\xa1"
SECOND_RESPONDER = bytes.fromhex("fe80") + bytes(13) + b"\xa2"
QUOTED_DESTINATION = bytes.fromhex("20010db8000a") + bytes(9) + b"\x99"

ICMP_MLD_QUERY = 130
ICMP_MLD_REPORT_V1 = 131
ICMP_MLD_DONE_V1 = 132
ICMP_ROUTER_SOLICITATION = 133
ICMP_ROUTER_ADVERTISEMENT = 134
ICMP_NEIGHBOUR_SOLICITATION = 135
ICMP_NEIGHBOUR_ADVERTISEMENT = 136
ICMP_MLD_REPORT_V2 = 143

ND_NAMES = {
	ICMP_ROUTER_SOLICITATION: "router-solicitation",
	ICMP_ROUTER_ADVERTISEMENT: "router-advertisement",
	ICMP_NEIGHBOUR_SOLICITATION: "neighbour-solicitation",
	ICMP_NEIGHBOUR_ADVERTISEMENT: "neighbour-advertisement",
	ICMP_ECHO_REQUEST: "echo-request",
	ICMP_ECHO_REPLY: "echo-reply",
	ICMP_MLD_QUERY: "mld-query",
	ICMP_MLD_REPORT_V1: "mld-report-v1",
	ICMP_MLD_DONE_V1: "mld-done-v1",
	ICMP_MLD_REPORT_V2: "mld-report-v2",
	ICMP_DEST_UNREACHABLE: "destination-unreachable",
	ICMP_PACKET_TOO_BIG: "packet-too-big",
	ICMP_TIME_EXCEEDED: "time-exceeded",
	ICMP_PARAMETER_PROBLEM: "parameter-problem",
}

UNSPECIFIED = bytes(16)
ALL_NODES = bytes.fromhex("ff020000000000000000000000000001")
ALL_ROUTERS = bytes.fromhex("ff020000000000000000000000000002")
ALL_MLDV2 = bytes.fromhex("ff020000000000000000000000000016")

PEER_MAC = bytes.fromhex("525400123499")


def text_address(raw):
	"""The canonical text form (RFC 5952), so a capture line reads the way a person writes one."""
	groups = [int.from_bytes(raw[index * 2:index * 2 + 2], "big") for index in range(8)]
	best_start, best_len, run_start, run_len = 0, 0, 0, 0
	for index, group in enumerate(groups):
		if group == 0:
			if run_len == 0:
				run_start = index
			run_len += 1
			if run_len > best_len:
				best_start, best_len = run_start, run_len
		else:
			run_len = 0
	if best_len < 2:
		return ":".join(f"{group:x}" for group in groups)
	head = ":".join(f"{group:x}" for group in groups[:best_start])
	tail = ":".join(f"{group:x}" for group in groups[best_start + best_len:])
	return f"{head}::{tail}"


def link_local_of(mac):
	"""The peer's own address, from its MAC. It is a fixture; nothing here claims privacy."""
	flipped = bytes([mac[0] ^ 0x02]) + mac[1:3] + b"\xff\xfe" + mac[3:]
	return bytes.fromhex("fe80") + bytes(6) + flipped


def solicited_node(address):
	return bytes.fromhex("ff0200000000000000000001ff") + address[13:16]


def multicast_mac(address):
	return b"\x33\x33" + address[12:16]


def ones_complement(data):
	"""The plain internet checksum, which IPv4 uses with no pseudo-header."""
	total = 0
	for index in range(0, len(data) - 1, 2):
		total += int.from_bytes(data[index:index + 2], "big")
	if len(data) % 2:
		total += data[-1] << 8
	while total >> 16:
		total = (total & 0xFFFF) + (total >> 16)
	folded = ~total & 0xFFFF
	return folded if folded else 0xFFFF


def checksum(source, destination, next_header, message):
	"""The upper-layer checksum over the IPv6 pseudo-header."""
	total = 0
	for chunk in (source, destination):
		for index in range(0, 16, 2):
			total += int.from_bytes(chunk[index:index + 2], "big")
	total += len(message) >> 16
	total += len(message) & 0xFFFF
	total += next_header
	for index in range(0, len(message) - 1, 2):
		total += int.from_bytes(message[index:index + 2], "big")
	if len(message) % 2:
		total += message[-1] << 8
	while total >> 16:
		total = (total & 0xFFFF) + (total >> 16)
	folded = (~total) & 0xFFFF
	return folded if folded else 0xFFFF


class Frame:
	"""One frame from the guest, decoded as far as this peer cares."""

	def __init__(self, raw):
		self.raw = raw
		self.ethertype = int.from_bytes(raw[12:14], "big") if len(raw) >= 14 else 0
		self.source_mac = raw[6:12] if len(raw) >= 12 else b""
		self.kind = "other"
		self.source = b""
		self.destination = b""
		self.hop_limit = 0
		self.icmp_type = None
		self.router_alert = False
		self.target = b""
		self.groups = []
		self.ipv4_source = b""
		self.ipv4_destination = b""
		self.source_port = 0
		self.destination_port = 0
		self.sequence = 0
		self.acknowledgement = 0
		self.flags = 0
		self.payload = b""
		if self.ethertype == ETHERTYPE_ARP:
			self.kind = "arp"
			return
		if self.ethertype == ETHERTYPE_IPV4:
			self.kind = "ipv4"
			self.ipv4_source = b""
			packet = raw[14:]
			if len(packet) >= 20 and packet[9] == IP_PROTO_TCP:
				header = (packet[0] & 0x0F) * 4
				self.ipv4_source = packet[12:16]
				self.ipv4_destination = packet[16:20]
				segment = packet[header:]
				if len(segment) >= 20:
					self.kind = "tcp4"
					self.source_port = int.from_bytes(segment[0:2], "big")
					self.destination_port = int.from_bytes(segment[2:4], "big")
					self.sequence = int.from_bytes(segment[4:8], "big")
					self.acknowledgement = int.from_bytes(segment[8:12], "big")
					self.flags = segment[13]
					self.payload = segment[(segment[12] >> 4) * 4:]
				return
			if len(packet) >= 20 and packet[9] == IP_PROTO_ICMP:
				header = (packet[0] & 0x0F) * 4
				self.ipv4_source = packet[12:16]
				self.ipv4_destination = packet[16:20]
				if len(packet) >= header + 8 and packet[header] == ICMP4_ECHO_REPLY:
					self.kind = "ipv4-echo-reply"
				if len(packet) >= header + 8 and packet[header] == ICMP4_ECHO_REQUEST:
					self.kind = "ipv4-echo-request"
					self.payload = packet[header:]
			return
		if self.ethertype != ETHERTYPE_IPV6 or len(raw) < 54:
			return
		packet = raw[14:]
		declared = int.from_bytes(packet[4:6], "big")
		body = packet[40:40 + declared]
		self.source = packet[8:24]
		self.destination = packet[24:40]
		self.hop_limit = packet[7]
		next_header = packet[6]
		offset = 0
		# One hop-by-hop header is all a listener message carries, and it is where Router Alert is.
		while next_header in (NEXT_HOP_BY_HOP, 60) and offset + 2 <= len(body):
			length = (body[offset + 1] + 1) * 8
			option = body[offset:offset + length]
			index = 2
			while index + 1 < len(option):
				if option[index] == 0:
					index += 1
					continue
				if option[index] == 5:
					self.router_alert = True
				index += 2 + option[index + 1]
			next_header = option[0] if option else 59
			offset += length
		payload = body[offset:]
		if next_header == NEXT_UDP and len(payload) >= 8:
			self.kind = "udp6"
			self.source_port = int.from_bytes(payload[0:2], "big")
			self.destination_port = int.from_bytes(payload[2:4], "big")
			self.payload = payload[8:int.from_bytes(payload[4:6], "big")]
			return
		if next_header == NEXT_TCP and len(payload) >= 20:
			self.kind = "tcp6"
			self.source_port = int.from_bytes(payload[0:2], "big")
			self.destination_port = int.from_bytes(payload[2:4], "big")
			self.sequence = int.from_bytes(payload[4:8], "big")
			self.acknowledgement = int.from_bytes(payload[8:12], "big")
			self.flags = payload[13]
			data_offset = (payload[12] >> 4) * 4
			self.payload = payload[data_offset:]
			return
		if next_header != NEXT_ICMPV6 or not payload:
			self.kind = f"ipv6-proto-{next_header}"
			return
		self.icmp_type = payload[0]
		self.kind = ND_NAMES.get(self.icmp_type, f"icmpv6-{self.icmp_type}")
		if self.icmp_type in (ICMP_NEIGHBOUR_SOLICITATION, ICMP_NEIGHBOUR_ADVERTISEMENT) and len(payload) >= 24:
			self.target = payload[8:24]
		if self.icmp_type == ICMP_MLD_REPORT_V2 and len(payload) >= 8:
			count = int.from_bytes(payload[6:8], "big")
			index = 8
			for _ in range(count):
				if index + 20 > len(payload):
					break
				self.groups.append(payload[index + 4:index + 20])
				index += 20 + int.from_bytes(payload[index + 2:index + 4], "big") * 16
		if self.icmp_type in (ICMP_MLD_REPORT_V1, ICMP_MLD_DONE_V1) and len(payload) >= 24:
			self.groups.append(payload[8:24])

	def describe(self):
		parts = [self.kind, f"src={text_address(self.source) if self.source else '-'}", f"dst={text_address(self.destination) if self.destination else '-'}", f"hop={self.hop_limit}", f"alert={'yes' if self.router_alert else 'no'}"]
		if self.target:
			parts.append(f"target={text_address(self.target)}")
		for group in self.groups:
			parts.append(f"group={text_address(group)}")
		if self.ipv4_source:
			parts.append(f"src4={'.'.join(str(byte) for byte in self.ipv4_source)}")
		if self.kind in ("tcp6", "udp6", "tcp4"):
			parts.append(f"sport={self.source_port}")
			parts.append(f"dport={self.destination_port}")
			parts.append(f"len={len(self.payload)}")
		if self.kind in ("tcp6", "tcp4"):
			names = "".join(letter for bit, letter in ((TCP_FIN, "F"), (TCP_SYN, "S"), (TCP_RST, "R"), (TCP_PSH, "P"), (TCP_ACK, "A")) if self.flags & bit)
			parts.append(f"flags={names or '-'}")
			parts.append(f"seq={self.sequence}")
			parts.append(f"ack={self.acknowledgement}")
		return " ".join(parts)


class Peer:
	def __init__(self, connection, capture, mac=PEER_MAC):
		self.connection = connection
		self.capture = capture
		self.mac = mac
		self.address = link_local_of(mac)
		self.started = time.monotonic()
		self.sent = 0

	def note(self, line):
		stamp = time.monotonic() - self.started
		self.capture.write(f"{stamp:9.3f} {line}\n")
		self.capture.flush()

	def read_frame(self, timeout):
		self.connection.settimeout(timeout)
		try:
			header = self.recv_exactly(4)
			if header is None:
				return None
			length = struct.unpack(">I", header)[0]
			if length == 0 or length > 65536:
				return None
			return self.recv_exactly(length)
		except socket.timeout:
			return None

	def recv_exactly(self, count):
		buffer = b""
		while len(buffer) < count:
			chunk = self.connection.recv(count - len(buffer))
			if not chunk:
				return None
			buffer += chunk
		return buffer

	def send_frame(self, frame):
		self.connection.sendall(struct.pack(">I", len(frame)) + frame)
		self.sent += 1

	def send_icmp(self, destination_mac, source, destination, hop_limit, message, hop_by_hop=b""):
		body = hop_by_hop + message
		next_header = NEXT_HOP_BY_HOP if hop_by_hop else NEXT_ICMPV6
		packet = bytes([0x60, 0, 0, 0]) + struct.pack(">H", len(body)) + bytes([next_header, hop_limit]) + source + destination
		self.send_frame(destination_mac + self.mac + struct.pack(">H", ETHERTYPE_IPV6) + packet + body)

	def with_checksum(self, source, destination, message):
		value = checksum(source, destination, NEXT_ICMPV6, message[:2] + b"\x00\x00" + message[4:])
		return message[:2] + struct.pack(">H", value) + message[4:]

	def neighbour_advertisement(self, to_mac, to_address, target):
		"""Answer a solicitation for our own address, so the guest can resolve us."""
		message = bytes([ICMP_NEIGHBOUR_ADVERTISEMENT, 0, 0, 0, 0xE0, 0, 0, 0]) + target + bytes([2, 1]) + self.mac
		self.send_icmp(to_mac, self.address, to_address, 255, self.with_checksum(self.address, to_address, message))
		self.note(f"sent neighbour-advertisement target={text_address(target)}")

	def router_advertisement(self, destination, options):
		"""An advertisement built exactly as the scenario asked, with no opinions of its own."""
		flags = options.get("flags", 0)
		lifetime = options.get("router_lifetime", 1800)
		message = bytes([ICMP_ROUTER_ADVERTISEMENT, 0, 0, 0, options.get("hop_limit", 64), flags]) + struct.pack(">H", lifetime) + struct.pack(">II", 30000, 1000)
		message += bytes([1, 1]) + self.mac
		if "mtu" in options:
			message += bytes([5, 1, 0, 0]) + struct.pack(">I", options["mtu"])
		for prefix in options.get("prefixes", []):
			prefix_flags = (0x80 if prefix.get("on_link", True) else 0) | (0x40 if prefix.get("autonomous", True) else 0)
			message += bytes([3, 4, prefix["length"], prefix_flags])
			message += struct.pack(">II", prefix.get("valid", 2592000), prefix.get("preferred", 604800))
			message += bytes(4) + prefix["base"]
		servers = options.get("rdnss", [])
		if servers:
			message += bytes([25, 1 + 2 * len(servers), 0, 0]) + struct.pack(">I", options.get("rdnss_lifetime", 600))
			for server in servers:
				message += server
		destination_mac = multicast_mac(destination) if destination[0] == 0xFF else options["mac"]
		self.send_icmp(destination_mac, self.address, destination, 255, self.with_checksum(self.address, destination, message))
		self.note(f"sent router-advertisement lifetime={lifetime} prefixes={len(options.get('prefixes', []))} rdnss={len(servers)}")

	def packet_too_big(self, to_mac, to_address, mtu, invoking):
		message = bytes([ICMP_PACKET_TOO_BIG, 0, 0, 0]) + struct.pack(">I", mtu) + invoking[:1232]
		self.send_icmp(to_mac, self.address, to_address, 64, self.with_checksum(self.address, to_address, message))
		self.note(f"sent packet-too-big mtu={mtu}")

	def quoted_echo_error(self, to_mac, to_address, responder, quoted_source, quoted_destination, message_type, code, identifier, sequence):
		"""An error quoting an Echo Request, addressed to the guest whatever the quotation names.

		The DESTINATION and the QUOTED SOURCE are separate arguments on purpose: an error addressed to
		this host that quotes somebody else's address is the forgery the validation exists to refuse,
		and one that conflates the two cannot express it - it would be refused a step earlier, for not
		being addressed to this host at all, and would prove nothing about the quotation."""
		echo = bytes([ICMP_ECHO_REQUEST, 0, 0, 0]) + struct.pack(">HH", identifier, sequence)
		quoted = bytes([0x60, 0, 0, 0]) + struct.pack(">H", len(echo)) + bytes([NEXT_ICMPV6, 64]) + quoted_source + quoted_destination
		message = bytes([message_type, code, 0, 0]) + bytes(4) + quoted + echo
		self.send_icmp(to_mac, responder, to_address, 64, self.with_checksum(responder, to_address, message))
		self.note(f"sent quoted-error type={message_type} responder={text_address(responder)} seq={sequence} quoting={text_address(quoted_source)}")

	def malformed(self, to_mac):
		"""A packet whose extension chain is a lie, for the parser's refusal paths."""
		body = bytes([NEXT_ICMPV6, 40]) + bytes(6)
		packet = bytes([0x60, 0, 0, 0]) + struct.pack(">H", len(body)) + bytes([NEXT_HOP_BY_HOP, 64]) + self.address + ALL_NODES
		self.send_frame(to_mac + self.mac + struct.pack(">H", ETHERTYPE_IPV6) + packet + body)
		self.note("sent malformed extension chain")

	def echo_request(self, to_mac, to_address, identifier, sequence):
		"""An echo request to the guest, which its ICMPv6 layer must answer without any transport."""
		message = bytes([ICMP_ECHO_REQUEST, 0, 0, 0]) + struct.pack(">HH", identifier, sequence) + b"peer-echo-payload"
		self.send_icmp(to_mac, self.address, to_address, 64, self.with_checksum(self.address, to_address, message))
		self.note(f"sent echo-request id={identifier} seq={sequence} to {text_address(to_address)}")

	def ipv4_echo_request(self, to_mac, identifier, sequence):
		"""An IPv4 ping to the guest, so one boot proves both families rather than one."""
		message = bytes([ICMP4_ECHO_REQUEST, 0, 0, 0]) + struct.pack(">HH", identifier, sequence) + b"peer-v4-echo"
		message = message[:2] + struct.pack(">H", ones_complement(message)) + message[4:]
		header = bytes([0x45, 0, 0, 0]) + struct.pack(">HHH", 0x1234, 0, 0x4001) + bytes(2) + PEER_IPV4 + GUEST_IPV4
		header = header[:2] + struct.pack(">H", 20 + len(message)) + header[4:]
		header = header[:10] + struct.pack(">H", ones_complement(header)) + header[12:]
		self.send_frame(to_mac + self.mac + struct.pack(">H", ETHERTYPE_IPV4) + header + message)
		self.note(f"sent ipv4-echo-request id={identifier} seq={sequence}")

	def ipv4_echo_reply(self, frame):
		"""Answer a ping the GUEST sent, so one boot proves both directions in both families."""
		message = bytes([ICMP4_ECHO_REPLY, 0, 0, 0]) + frame.payload[4:]
		message = message[:2] + struct.pack(">H", ones_complement(message)) + message[4:]
		header = bytes([0x45, 0, 0, 0]) + struct.pack(">HHH", 0x1234, 0, 0x4001) + bytes(2) + frame.ipv4_destination + frame.ipv4_source
		header = header[:2] + struct.pack(">H", 20 + len(message)) + header[4:]
		header = header[:10] + struct.pack(">H", ones_complement(header)) + header[12:]
		self.send_frame(frame.source_mac + self.mac + struct.pack(">H", ETHERTYPE_IPV4) + header + message)
		self.note(f"sent ipv4-echo-reply to {'.'.join(str(byte) for byte in frame.ipv4_source)}")

	def send_tcp4(self, to_mac, to_address, source, source_port, destination_port, sequence, acknowledgement, flags, payload=b"", options=b""):
		offset = (20 + len(options)) // 4
		segment = struct.pack(">HHII", source_port, destination_port, sequence, acknowledgement)
		segment += bytes([offset << 4, flags]) + struct.pack(">H", 65535) + bytes(4) + options + payload
		pseudo = source + to_address + bytes([0, IP_PROTO_TCP]) + struct.pack(">H", len(segment))
		segment = segment[:16] + struct.pack(">H", ones_complement(pseudo + segment)) + segment[18:]
		header = bytes([0x45, 0, 0, 0]) + struct.pack(">HHH", 0x4321, 0, 0x4006) + bytes(2) + source + to_address
		header = header[:2] + struct.pack(">H", 20 + len(segment)) + header[4:]
		header = header[:10] + struct.pack(">H", ones_complement(header)) + header[12:]
		self.send_frame(to_mac + self.mac + struct.pack(">H", ETHERTYPE_IPV4) + header + segment)
		names = "".join(letter for bit, letter in ((TCP_FIN, "F"), (TCP_SYN, "S"), (TCP_RST, "R"), (TCP_PSH, "P"), (TCP_ACK, "A")) if flags & bit)
		self.note(f"sent tcp4 sport={source_port} dport={destination_port} flags={names or '-'} seq={sequence} ack={acknowledgement} len={len(payload)}")

	def arp_reply(self, frame):
		"""IPv4 coexistence: answer for whatever address was asked about, so DHCP and ARP still work."""
		request = frame.raw[14:]
		if len(request) < 28 or int.from_bytes(request[6:8], "big") != 1:
			return
		sender_mac, sender_ip, target_ip = request[8:14], request[14:18], request[24:28]
		reply = frame.source_mac + self.mac + struct.pack(">H", ETHERTYPE_ARP)
		reply += bytes([0, 1, 8, 0, 6, 4, 0, 2]) + self.mac + target_ip + sender_mac + sender_ip
		self.send_frame(reply)
		self.note("sent arp-reply")


	def send_ipv6(self, destination_mac, source, destination, next_header, body, hop_limit=64):
		"""One IPv6 packet with a transport payload whose checksum is already in it."""
		packet = bytes([0x60, 0, 0, 0]) + struct.pack(">H", len(body)) + bytes([next_header, hop_limit]) + source + destination
		self.send_frame(destination_mac + self.mac + struct.pack(">H", ETHERTYPE_IPV6) + packet + body)

	def send_udp6(self, to_mac, to_address, source, source_port, destination_port, payload):
		datagram = struct.pack(">HHH", source_port, destination_port, 8 + len(payload)) + bytes(2) + payload
		value = checksum(source, to_address, NEXT_UDP, datagram)
		# A COMPUTED ZERO GOES OUT AS ALL ONES over IPv6: zero means "no checksum" in IPv4 and is
		# forbidden here, and a peer that sent it would be testing the guest's refusal path by
		# accident.
		datagram = datagram[:6] + struct.pack(">H", value or 0xFFFF) + datagram[8:]
		self.send_udp6_raw(to_mac, to_address, source, datagram)

	def send_udp6_raw(self, to_mac, to_address, source, datagram):
		self.send_ipv6(to_mac, source, to_address, NEXT_UDP, datagram)
		self.note(f"sent udp6 sport={int.from_bytes(datagram[0:2], 'big')} dport={int.from_bytes(datagram[2:4], 'big')} len={len(datagram) - 8}")

	def send_tcp6(self, to_mac, to_address, source, source_port, destination_port, sequence, acknowledgement, flags, payload=b"", options=b""):
		offset = (20 + len(options)) // 4
		segment = struct.pack(">HHII", source_port, destination_port, sequence, acknowledgement)
		segment += bytes([offset << 4, flags]) + struct.pack(">H", 65535) + bytes(4) + options + payload
		value = checksum(source, to_address, NEXT_TCP, segment)
		segment = segment[:16] + struct.pack(">H", value or 0xFFFF) + segment[18:]
		self.send_ipv6(to_mac, source, to_address, NEXT_TCP, segment)
		names = "".join(letter for bit, letter in ((TCP_FIN, "F"), (TCP_SYN, "S"), (TCP_RST, "R"), (TCP_PSH, "P"), (TCP_ACK, "A")) if flags & bit)
		self.note(f"sent tcp6 sport={source_port} dport={destination_port} flags={names or '-'} seq={sequence} ack={acknowledgement} len={len(payload)}")

	def packet_too_big_for(self, to_mac, to_address, source, mtu, invoking):
		"""A Packet Too Big quoting an arbitrary packet, from an arbitrary responder."""
		message = bytes([ICMP_PACKET_TOO_BIG, 0, 0, 0]) + struct.pack(">I", mtu) + invoking[:1232]
		self.send_icmp(to_mac, source, to_address, 64, self.with_checksum(source, to_address, message))
		self.note(f"sent packet-too-big mtu={mtu} responder={text_address(source)}")

	def time_exceeded_for(self, to_mac, to_address, responder, invoking):
		"""A hop saying the probe expired. The RESPONDER is the hop, not the destination."""
		message = bytes([ICMP_TIME_EXCEEDED, 0, 0, 0]) + bytes(4) + invoking[:1232]
		self.send_icmp(to_mac, responder, to_address, 64, self.with_checksum(responder, to_address, message))
		self.note(f"sent time-exceeded responder={text_address(responder)}")


def dns_name(labels):
	"""Encode a dotted name as length-prefixed labels."""
	out = b""
	for label in labels.split(b"."):
		out += bytes([len(label)]) + label
	return out + b"\x00"


def dns_question(query):
	"""The name and type a query asked about, or None when it is not one this peer can read."""
	if len(query) < 12:
		return None
	offset = 12
	labels = []
	while offset < len(query) and query[offset]:
		length = query[offset]
		if length > 63 or offset + 1 + length > len(query):
			return None
		labels.append(query[offset + 1:offset + 1 + length])
		offset += 1 + length
	if offset + 5 > len(query):
		return None
	qtype = int.from_bytes(query[offset + 1:offset + 3], "big")
	return int.from_bytes(query[0:2], "big"), b".".join(labels), qtype, query[12:offset + 5]


def dns_answer(query, over_tcp):
	"""An answer to `query`, or None when it is not a question this peer can read.

	TRUNCATION IS ONLY EVER OVER UDP. A truncated TCP answer would tell the resolver to retry over
	the transport it is already using, which is a loop rather than a test."""
	parsed = dns_question(query)
	if parsed is None:
		return None
	identity, name, qtype, question = parsed
	zone = DNS_ZONE.get(name)
	if zone is None:
		# NXDOMAIN. A name this peer does not hold is a definite "no", not silence.
		return struct.pack(">HHHHHH", identity, 0x8183, 1, 0, 0, 0) + question
	if zone.get("truncate_over_udp") and not over_tcp:
		# THE TC BIT AND NO ANSWERS AT ALL: the resolver must ask again over TCP rather than use what
		# little arrived.
		return struct.pack(">HHHHHH", identity, 0x8380, 1, 0, 0, 0) + question
	records = zone.get("aaaa", []) if qtype == 28 else zone.get("a", []) if qtype == 1 else []
	body = b""
	for record in records:
		body += dns_name(name) + struct.pack(">HHIH", qtype, 1, 60, len(record)) + record
	return struct.pack(">HHHHHH", identity, 0x8180, 1, len(records), 0, 0) + question + body


def prefix_option(text, length=64, **kwargs):
	base = bytes.fromhex(text)
	option = {"base": base + bytes(16 - len(base)), "length": length}
	option.update(kwargs)
	return option


# The scenarios. Each is a dict of what to answer and what to volunteer; keeping them as data rather
# than as code is what stops one scenario's behaviour leaking into another's.
SCENARIOS = {
	# Answer nothing. Proves the guest's own emissions: the listener report before detection, the
	# detection probe, the re-report, and the solicitation schedule widening.
	# It also pings over IPv4, because the same boot has to keep the other family working - and this
	# is the scenario the sub-1280 row runs, where IPv6 is refused outright and IPv4 is all there is.
	"quiet": {"answer_solicitations": False, "advertise": None, "ipv4_ping": True},
	# A router with one autonomous /64, a recursive server and a smaller MTU.
	"router": {
		"answer_solicitations": True,
		"advertise": {
			"router_lifetime": 1800,
			"flags": 0x08,
			"mtu": 1400,
			"prefixes": [prefix_option("20010db8000a", on_link=True, autonomous=True)],
			"rdnss": [bytes.fromhex("20010db8000a") + bytes(9) + b"\x53"],
		},
		"ipv4_ping": True,
	},
	# Everything a host must refuse: preferred beyond valid, an autonomous prefix that is not a /64,
	# and a router that withdraws itself.
	"hostile": {
		"answer_solicitations": True,
		"advertise": {
			"router_lifetime": 1800,
			"prefixes": [
				prefix_option("20010db8000b", valid=100, preferred=200),
				prefix_option("20010db8000c", length=56, on_link=False, autonomous=True),
			],
		},
		"then_withdraw": True,
	},
	# A ROUTER AND A SET OF PEERS BEHIND IT: a resolver, a web peer, and a black hole. This is the
	# scenario the guest-initiated rows run against - a name looked up, a connection opened, a probe
	# sent - none of which a peer can provoke and all of which it can answer.
	"transport": {
		"answer_solicitations": True,
		"advertise": {
			"router_lifetime": 1800,
			"flags": 0x08,
			"prefixes": [prefix_option("20010db8000a", on_link=True, autonomous=True)],
			"rdnss": [DNS_ADDRESS],
		},
		"own_addresses": [PEER_GLOBAL, DNS_ADDRESS, BLACK_HOLE],
		"dns": True,
		"tcp_server": True,
		"ipv4_dns": True,
		"ipv4_ping": True,
		"time_exceeded_for_probes": True,
	},
	# THE DECLARED FALLBACK, WITH BOTH ENDS REAL. `fallback.test` resolves to a black-holed IPv6
	# address and a working IPv4 one; the guest must try them in that order, give up on the first
	# inside the cap, and connect over the second.
	"fallback": {
		"answer_solicitations": True,
		"advertise": {"router_lifetime": 1800, "prefixes": [prefix_option("20010db8000a")], "rdnss": [DNS_ADDRESS]},
		"own_addresses": [PEER_GLOBAL, DNS_ADDRESS, BLACK_HOLE],
		"dns": True,
		"tcp_server": True,
		"ipv4_ping": True,
	},
	# THE SAME PEERS, PLUS A PATH THAT SHRINKS UNDER A LIVE CONNECTION. The reports quote the flow's
	# own sequence space, which is what separates a real resegmentation from a handshake that merely
	# completed.
	"pmtu": {
		"answer_solicitations": True,
		"advertise": {"router_lifetime": 1800, "prefixes": [prefix_option("20010db8000a")], "rdnss": [DNS_ADDRESS]},
		"own_addresses": [PEER_GLOBAL, DNS_ADDRESS, BLACK_HOLE],
		"dns": True,
		"tcp_server": True,
		"quote_acked_then_outstanding": 1300,
	},
	# AN INBOUND CONNECTION, AND AN ERROR THAT QUOTES SOMEBODY ELSE'S FLOW. The peer opens a TCP
	# connection TO the guest's listener, and every ICMPv6 transport error it then delivers quotes a
	# DIFFERENT live tuple. The quoted flow must be untouched: a validator matching on less than the
	# full tuple would resize or tear down a connection the error was never about.
	"hostile-quote": {
		"answer_solicitations": True,
		"advertise": {"router_lifetime": 1800, "prefixes": [prefix_option("20010db8000a")], "rdnss": [DNS_ADDRESS]},
		"own_addresses": [PEER_GLOBAL, DNS_ADDRESS, BLACK_HOLE],
		"dns": True,
		"tcp_server": True,
		"connect_in": 80,
		"quote_other_flow": 1300,
		"ipv4_ping": True,
	},
	# A LISTENER UNDER A FLOOD, in both families. M4's budgets are the subject: the guest must refuse
	# rather than grow, and must still be answering when the flood stops.
	"budgets": {
		"answer_solicitations": True,
		"advertise": {"router_lifetime": 1800, "prefixes": [prefix_option("20010db8000a")], "rdnss": [DNS_ADDRESS]},
		"own_addresses": [PEER_GLOBAL, DNS_ADDRESS, BLACK_HOLE],
		"dns": True,
		"tcp_server": True,
		"syn_flood": 96,
		"ipv4_syn_flood": 96,
		"ipv4_ping": True,
	},
	# A router, and then a Packet Too Big about whatever the guest sends.
	"ptb": {"answer_solicitations": True, "advertise": {"router_lifetime": 1800, "prefixes": [prefix_option("20010db8000a")]}, "packet_too_big": 1300},
	# A malformed-packet flood, for the bounded-refusal paths.
	"flood": {"answer_solicitations": True, "advertise": None, "flood": 64},
	# Echo traffic in both directions, and a Packet Too Big quoting what came back. The guest needs no
	# transport for this: ICMPv6 echo is part of the host, which is the whole point of the item that
	# put it there.
	"echo": {
		"answer_solicitations": True,
		"advertise": {"router_lifetime": 1800, "prefixes": [prefix_option("20010db8000a")]},
		"echo_after_dad": True,
		"packet_too_big": 1300,
		"quoted_errors": True,
	},
}


def run(peer, scenario, seconds):
	plan = SCENARIOS[scenario]
	deadline = time.monotonic() + seconds
	# LATE ENOUGH THAT THE SERVICE IS SERVING. The static fallback is only in place once DHCP has
	# given up, and a ping before that would be measuring the boot rather than the stack.
	ping_at = time.monotonic() + 20.0
	# LATE ENOUGH THAT THE LISTENER IS LISTENING. The flood is about a listener's budgets; arriving
	# before the guest has one measures a closed port instead, which every implementation passes.
	flood_at = time.monotonic() + 30.0
	# EARLY ENOUGH TO LAND IN THE MIDDLE OF THE GUEST'S WORK. The row after this one asserts that a
	# live flow SURVIVED the error, which is only a claim if the error arrived while it was live.
	connect_at = time.monotonic() + 25.0
	advertised = False
	flooded = 0
	# The guest's global address, learned from the detection probe it sends for it, and when it will
	# have finished proving it.
	guest_global = None
	guest_mac = None
	echo_at = None
	echoed = False
	pinged = False
	quoted = False
	# The quoted errors waiting to go out, one per idle pass, and who to send them to.
	quoted_queue = []
	quoted_to = None
	# One entry per TCP conversation this peer is holding up its end of.
	flows = {}
	connected_in = False
	inbound = None
	# A fresh source port per attempt, so an answer names the attempt it belongs to.
	inbound_port = 45000
	while time.monotonic() < deadline:
		raw = peer.read_frame(0.25)
		if raw is None:
			if plan.get("flood") and flooded < plan["flood"]:
				peer.malformed(b"\x33\x33\x00\x00\x00\x01")
				flooded += 1
			if echo_at is not None and not echoed and time.monotonic() >= echo_at and guest_global and guest_mac:
				peer.echo_request(guest_mac, guest_global, 0x4242, 1)
				echoed = True
			if plan.get("syn_flood") and guest_global and guest_mac and flooded < plan["syn_flood"] and time.monotonic() >= flood_at:
				# ONE PER IDLE PASS FROM A FRESH PORT. The point is a listener's budget, not the
				# receive ring: a hundred frames back to back measure the ring instead.
				peer.send_tcp6(guest_mac, guest_global, PEER_GLOBAL, 40000 + flooded, 80, 0x1000 + flooded, 0, TCP_SYN)
				if plan.get("ipv4_syn_flood"):
					peer.send_tcp4(guest_mac, GUEST_IPV4, PEER_IPV4, 40000 + flooded, 80, 0x2000 + flooded, 0, TCP_SYN)
				flooded += 1
			if plan.get("connect_in") and not connected_in and guest_global and guest_mac and time.monotonic() >= connect_at:
				# THE PEER OPENS THE CONNECTION THIS TIME. Everything else in these rows is the guest
				# reaching out; a listener is only proven by somebody reaching in.
				#
				# RETRIED RATHER THAN TIMED. The guest's listener is started by a shell command, and
				# when that lands depends on how long the boot took - so a single SYN at a guessed
				# instant lands before the listener exists and is dropped, which looks exactly like a
				# listener that refused it. Each attempt uses a fresh port, so the answer that does
				# come back is unambiguously to one of them.
				connect_at = time.monotonic() + 3.0
				inbound_port += 1
				peer.send_tcp6(guest_mac, guest_global, PEER_GLOBAL, inbound_port, plan["connect_in"], 0x5000, 0, TCP_SYN, options=bytes([2, 4]) + struct.pack(">H", 1220))
			if plan.get("ipv4_ping") and not pinged and guest_mac and time.monotonic() >= ping_at:
				peer.ipv4_echo_request(guest_mac, 0x5151, 1)
				pinged = True
			if quoted_queue and quoted_to is not None:
				responder, quoted_source, quoted_destination, kind, code, sequence = quoted_queue.pop(0)
				peer.quoted_echo_error(quoted_to[0], quoted_to[1], responder, quoted_source, quoted_destination, kind, code, 0x4242, sequence)
			continue
		frame = Frame(raw)
		peer.note(f"saw {frame.describe()}")
		if frame.source_mac:
			guest_mac = frame.source_mac
		# A detection probe from `::` for a global address is the guest proving the address it formed
		# from our prefix. Two seconds later it is its own, and can be echoed.
		if frame.icmp_type == ICMP_NEIGHBOUR_SOLICITATION and frame.source == UNSPECIFIED and frame.target[:2] == b"\x20\x01" and guest_global is None:
			guest_global = frame.target
			if plan.get("echo_after_dad"):
				echo_at = time.monotonic() + 2.0
			peer.note(f"learned the guest's global address {text_address(guest_global)}")
		if frame.icmp_type == ICMP_ECHO_REPLY and plan.get("packet_too_big"):
			peer.packet_too_big(frame.source_mac, frame.source, plan["packet_too_big"], raw[14:])
			if plan.get("quoted_errors") and not quoted:
				# REVERSE ORDER, ON PURPOSE: the second hop's complaint about sequence 2 arrives
				# before the first hop's about sequence 1. Everything that tells them apart is in the
				# quotation, so a layer that inferred either from arrival order gets both wrong.
				# THE ONE THAT MUST NOT REACH ANYBODY GOES FIRST, because the guest reports only when
				# something was delivered: a refusal that arrives after the last accepted error leaves
				# its counter true and unprinted, which is a test that cannot see what it asserts.
				# This one is addressed to this host, so it gets as far as the quotation, and the
				# address it quotes is one this interface does not hold.
				# ONE PER PASS, NOT A BURST. This row is about CORRELATION - which flow an error belongs
				# to - and four frames back to back measure the receive ring instead: the guest writes a
				# report line to the serial console between frames, and the last of a tight burst is
				# sometimes the one that is dropped, which made this assertion a coin flip. Burst survival
				# is the flood row's subject and has its own oracle there.
				quoted_to = (frame.source_mac, frame.source)
				quoted_queue = [
					(FIRST_RESPONDER, QUOTED_DESTINATION, frame.source, ICMP_TIME_EXCEEDED, 0, 3),
					(SECOND_RESPONDER, frame.source, QUOTED_DESTINATION, ICMP_DEST_UNREACHABLE, 3, 2),
					(FIRST_RESPONDER, frame.source, QUOTED_DESTINATION, ICMP_TIME_EXCEEDED, 0, 1),
				]
				quoted = True
		if frame.kind == "tcp4" and plan.get("tcp_server"):
			serve_tcp4(peer, plan, frame, flows)
			continue
		if frame.kind == "ipv4-echo-request":
			peer.ipv4_echo_reply(frame)
			continue
		if frame.kind == "arp":
			peer.arp_reply(frame)
			continue
		if frame.icmp_type == ICMP_NEIGHBOUR_SOLICITATION and plan["answer_solicitations"]:
			# Only for OUR addresses. A probe for the guest's own tentative address is a duplicate-
			# address detection probe, and answering it would tell the guest its address is taken.
			# THE BLACK HOLE IS ONE OF OURS AND IS ANSWERED HERE: a destination that cannot be
			# resolved fails a step earlier and proves nothing about a connection's timing.
			if frame.target == peer.address or frame.target in plan.get("own_addresses", []):
				peer.neighbour_advertisement(frame.source_mac, frame.source if frame.source != UNSPECIFIED else ALL_NODES, frame.target)
			continue
		if frame.icmp_type == ICMP_ROUTER_SOLICITATION and plan["advertise"] and not advertised:
			options = dict(plan["advertise"])
			options["mac"] = frame.source_mac
			peer.router_advertisement(ALL_NODES, options)
			advertised = True
			if plan.get("then_withdraw"):
				withdrawal = dict(options)
				withdrawal["router_lifetime"] = 0
				withdrawal["prefixes"] = []
				peer.router_advertisement(ALL_NODES, withdrawal)
			continue
		if frame.icmp_type == ICMP_ECHO_REQUEST and frame.destination in plan.get("own_addresses", []):
			# A HOP LIMIT OF ONE EXPIRES AT THE FIRST ROUTER, which is what a traceroute's first row
			# is. The responder is that router's own address and NOT the destination the probe named:
			# reporting the destination is how a trace comes to name the same host for every row.
			if plan.get("time_exceeded_for_probes") and frame.hop_limit <= 1:
				peer.time_exceeded_for(frame.source_mac, frame.source, FIRST_RESPONDER, raw[14:])
				continue
			echo = raw[14 + 40:]
			reply = bytes([ICMP_ECHO_REPLY, 0, 0, 0]) + echo[4:]
			peer.send_icmp(frame.source_mac, frame.destination, frame.source, 64, peer.with_checksum(frame.destination, frame.source, reply))
			peer.note(f"sent echo-reply id={int.from_bytes(echo[4:6], 'big')} seq={int.from_bytes(echo[6:8], 'big')} from {text_address(frame.destination)}")
			continue
		if frame.kind == "udp6" and frame.destination_port == DNS_PORT and plan.get("dns"):
			answer = dns_answer(frame.payload, over_tcp=False)
			if answer is not None:
				peer.send_udp6(frame.source_mac, frame.source, frame.destination, DNS_PORT, frame.source_port, answer)
			continue
		if frame.kind == "tcp6" and plan.get("connect_in") and frame.source_port == plan["connect_in"] and frame.destination_port == inbound_port:
			# The guest's listener answering the connection this peer opened.
			if frame.flags & TCP_SYN and frame.flags & TCP_ACK:
				connected_in = True
				inbound = (frame.acknowledgement, frame.sequence + 1)
				peer.send_tcp6(frame.source_mac, frame.source, frame.destination, inbound_port, plan["connect_in"], inbound[0], inbound[1], TCP_ACK)
				peer.note("inbound connection accepted by the guest")
				peer.send_tcp6(frame.source_mac, frame.source, frame.destination, inbound_port, plan["connect_in"], inbound[0], inbound[1], TCP_PSH | TCP_ACK, payload=b"GET /\r\n\r\n")
				if plan.get("quote_other_flow"):
					# A QUOTATION OF A TUPLE THAT IS NOT THIS ONE. Same addresses, different ports,
					# and a live flow of the guest's own: its resolver conversation. A validator
					# matching on less than the full tuple would resize that flow instead.
					other = struct.pack(">HHI", 51234, DNS_PORT, 0x1234) + bytes(12)
					quoted = bytes([0x60, 0, 0, 0]) + struct.pack(">H", len(other)) + bytes([NEXT_TCP, 64]) + frame.source + frame.destination
					peer.packet_too_big_for(frame.source_mac, frame.source, FIRST_RESPONDER, plan["quote_other_flow"], quoted + other)
			continue
		if frame.kind == "tcp6" and plan.get("tcp_server"):
			serve_tcp6(peer, plan, frame, flows)
			continue
		if plan.get("packet_too_big") and frame.kind.startswith("ipv6-proto-"):
			peer.packet_too_big(frame.source_mac, frame.source, plan["packet_too_big"], raw[14:])
	peer.note(f"done frames-sent={peer.sent}")


def serve_tcp4(peer, plan, frame, flows):
	"""The same peer over the other family, so a fallback has something to fall back TO."""
	key = ("v4", frame.ipv4_source, frame.source_port, frame.destination_port)
	if frame.flags & TCP_SYN and not frame.flags & TCP_ACK:
		flows[key] = {"peer_seq": 0x7000, "guest_seq": frame.sequence + 1, "served": False}
		options = bytes([2, 4]) + struct.pack(">H", 1460)
		peer.send_tcp4(frame.source_mac, frame.ipv4_source, frame.ipv4_destination, frame.destination_port, frame.source_port, flows[key]["peer_seq"], flows[key]["guest_seq"], TCP_SYN | TCP_ACK, options=options)
		flows[key]["peer_seq"] += 1
		return
	flow = flows.get(key)
	if flow is None:
		return
	if frame.payload:
		flow["guest_seq"] = frame.sequence + len(frame.payload)
		peer.note(f"received request bytes={len(frame.payload)} over ipv4 on dport={frame.destination_port}")
		if not flow["served"]:
			flow["served"] = True
			body = b"peer-body-v4-" + b"x" * 100
			peer.send_tcp4(frame.source_mac, frame.ipv4_source, frame.ipv4_destination, frame.destination_port, frame.source_port, flow["peer_seq"], flow["guest_seq"], TCP_PSH | TCP_ACK, payload=body)
			flow["peer_seq"] += len(body)
			peer.send_tcp4(frame.source_mac, frame.ipv4_source, frame.ipv4_destination, frame.destination_port, frame.source_port, flow["peer_seq"], flow["guest_seq"], TCP_FIN | TCP_ACK)
			flow["peer_seq"] += 1
			return
		peer.send_tcp4(frame.source_mac, frame.ipv4_source, frame.ipv4_destination, frame.destination_port, frame.source_port, flow["peer_seq"], flow["guest_seq"], TCP_ACK)
		return
	if frame.flags & TCP_FIN:
		flow["guest_seq"] = frame.sequence + 1
		peer.send_tcp4(frame.source_mac, frame.ipv4_source, frame.ipv4_destination, frame.destination_port, frame.source_port, flow["peer_seq"], flow["guest_seq"], TCP_ACK)


def serve_tcp6(peer, plan, frame, flows):
	"""A TCP peer that is only as much of TCP as these rows need.

	IT IS NOT A STACK. It completes a handshake, acknowledges what arrives, answers a request with a
	body and closes. Everything the guest's own TCP has to get right - windows, retransmission,
	resegmentation - is the GUEST's, and a peer that implemented its own would be asserting about
	itself."""
	key = (frame.source, frame.source_port, frame.destination_port)
	# A BLACK HOLE ANSWERS NOTHING AT ALL, not even a reset: a refusal is an answer, and the case
	# this exists for is a path that swallows the SYN.
	if frame.destination in (BLACK_HOLE,):
		peer.note(f"black-holed tcp6 sport={frame.source_port} dport={frame.destination_port}")
		return
	if frame.flags & TCP_SYN and not frame.flags & TCP_ACK:
		# THE SYN OCCUPIES ONE SEQUENCE NUMBER, so the acknowledgement is one past it.
		flows[key] = {"peer_seq": 0x9000, "guest_seq": frame.sequence + 1, "served": False}
		options = bytes([2, 4]) + struct.pack(">H", 1220)
		peer.send_tcp6(frame.source_mac, frame.source, frame.destination, frame.destination_port, frame.source_port, flows[key]["peer_seq"], flows[key]["guest_seq"], TCP_SYN | TCP_ACK, options=options)
		flows[key]["peer_seq"] += 1
		return
	flow = flows.get(key)
	if flow is None:
		return
	if frame.payload:
		flow["guest_seq"] = frame.sequence + len(frame.payload)
		peer.note(f"received request bytes={len(frame.payload)} on dport={frame.destination_port}")
		if not flow["served"]:
			flow["served"] = True
			body = plan.get("body", b"peer-body-" + b"x" * 200)
			if frame.destination_port == DNS_PORT:
				# DNS OVER TCP IS LENGTH-PREFIXED, and the retry has to be answered in full - that is
				# the whole point of having truncated the UDP answer.
				query = frame.payload[2:]
				answer = dns_answer(query, over_tcp=True) or b""
				body = struct.pack(">H", len(answer)) + answer
			peer.send_tcp6(frame.source_mac, frame.source, frame.destination, frame.destination_port, frame.source_port, flow["peer_seq"], flow["guest_seq"], TCP_PSH | TCP_ACK, payload=body)
			flow["peer_seq"] += len(body)
			peer.send_tcp6(frame.source_mac, frame.source, frame.destination, frame.destination_port, frame.source_port, flow["peer_seq"], flow["guest_seq"], TCP_FIN | TCP_ACK)
			flow["peer_seq"] += 1
			return
		peer.send_tcp6(frame.source_mac, frame.source, frame.destination, frame.destination_port, frame.source_port, flow["peer_seq"], flow["guest_seq"], TCP_ACK)
		return
	if frame.flags & TCP_FIN:
		flow["guest_seq"] = frame.sequence + 1
		peer.send_tcp6(frame.source_mac, frame.source, frame.destination, frame.destination_port, frame.source_port, flow["peer_seq"], flow["guest_seq"], TCP_ACK)


def main():
	parser = argparse.ArgumentParser(description="A deterministic IPv6 peer for the guest, over QEMU's socket netdev.")
	parser.add_argument("--port", type=int, required=True, help="the TCP port QEMU connects to")
	parser.add_argument("--scenario", choices=sorted(SCENARIOS), default="quiet")
	parser.add_argument("--capture", required=True, help="where to write the capture")
	parser.add_argument("--seconds", type=float, default=60.0, help="how long to serve before exiting")
	arguments = parser.parse_args()

	listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
	listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
	listener.bind(("127.0.0.1", arguments.port))
	listener.listen(1)
	# The port is open before this prints, so a caller that waits for the line cannot race the guest.
	print(f"ipv6-peer: listening on {arguments.port} scenario={arguments.scenario}", flush=True)
	listener.settimeout(arguments.seconds)
	try:
		connection, _ = listener.accept()
	except socket.timeout:
		print("ipv6-peer: the guest never connected", file=sys.stderr)
		return 1
	with open(arguments.capture, "w", encoding="utf-8") as capture:
		peer = Peer(connection, capture)
		capture.write(f"# scenario={arguments.scenario} peer={text_address(peer.address)} mac={peer.mac.hex()}\n")
		run(peer, arguments.scenario, arguments.seconds)
	connection.close()
	listener.close()
	return 0


if __name__ == "__main__":
	sys.exit(main())
