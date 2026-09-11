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
ICMP4_ECHO_REQUEST = 8
ICMP4_ECHO_REPLY = 0

# The guest's IPv4 address when no DHCP server answers, and the one this peer answers from. Both are
# fixed by the service's own static fallback, which is what makes the IPv4 path testable in a run
# that deliberately has no DHCP server in it.
GUEST_IPV4 = bytes([10, 0, 2, 15])
PEER_IPV4 = bytes([10, 0, 2, 99])

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
		if self.ethertype == ETHERTYPE_ARP:
			self.kind = "arp"
			return
		if self.ethertype == ETHERTYPE_IPV4:
			self.kind = "ipv4"
			self.ipv4_source = b""
			packet = raw[14:]
			if len(packet) >= 20 and packet[9] == IP_PROTO_ICMP:
				header = (packet[0] & 0x0F) * 4
				self.ipv4_source = packet[12:16]
				if len(packet) >= header + 8 and packet[header] == ICMP4_ECHO_REPLY:
					self.kind = "ipv4-echo-reply"
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
	while time.monotonic() < deadline:
		raw = peer.read_frame(0.25)
		if raw is None:
			if plan.get("flood") and flooded < plan["flood"]:
				peer.malformed(b"\x33\x33\x00\x00\x00\x01")
				flooded += 1
			if echo_at is not None and not echoed and time.monotonic() >= echo_at and guest_global and guest_mac:
				peer.echo_request(guest_mac, guest_global, 0x4242, 1)
				echoed = True
			if plan.get("ipv4_ping") and not pinged and guest_mac and time.monotonic() >= ping_at:
				peer.ipv4_echo_request(guest_mac, 0x5151, 1)
				pinged = True
			continue
		frame = Frame(raw)
		peer.note(f"saw {frame.describe()}")
		if frame.source_mac:
			guest_mac = frame.source_mac
		# A detection probe from `::` for a global address is the guest proving the address it formed
		# from our prefix. Two seconds later it is its own, and can be echoed.
		if plan.get("echo_after_dad") and frame.icmp_type == ICMP_NEIGHBOUR_SOLICITATION and frame.source == UNSPECIFIED and frame.target[:2] == b"\x20\x01" and guest_global is None:
			guest_global = frame.target
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
				peer.quoted_echo_error(frame.source_mac, frame.source, FIRST_RESPONDER, QUOTED_DESTINATION, frame.source, ICMP_TIME_EXCEEDED, 0, 0x4242, 3)
				peer.quoted_echo_error(frame.source_mac, frame.source, SECOND_RESPONDER, frame.source, QUOTED_DESTINATION, ICMP_DEST_UNREACHABLE, 3, 0x4242, 2)
				peer.quoted_echo_error(frame.source_mac, frame.source, FIRST_RESPONDER, frame.source, QUOTED_DESTINATION, ICMP_TIME_EXCEEDED, 0, 0x4242, 1)
				quoted = True
		if frame.kind == "arp":
			peer.arp_reply(frame)
			continue
		if frame.icmp_type == ICMP_NEIGHBOUR_SOLICITATION and plan["answer_solicitations"]:
			# Only for OUR address. A probe for the guest's own tentative address is a duplicate-
			# address detection probe, and answering it would tell the guest its address is taken.
			if frame.target == peer.address:
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
		if plan.get("packet_too_big") and frame.kind.startswith("ipv6-proto-"):
			peer.packet_too_big(frame.source_mac, frame.source, plan["packet_too_big"], raw[14:])
	peer.note(f"done frames-sent={peer.sent}")


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
