#!/usr/bin/env python3
# The development-control protocol's conformance suite, run against a live development
# instance from the host.
#
# It is here rather than in the kernel test binary for the same reason the scenarios are: it
# tests a host-facing protocol over a host socket, so the guest side of it is the guest that is
# already running. Nothing here is staged, relinked or rebooted, and a case that is wrong is a
# line of Python rather than a kernel image.
#
# What it asserts is the whole taxonomy: every rejection the protocol defines, in the exact
# form it defines - an opcode and a status - and every bound checked at the guest rather than
# merely advertised to the host. That distinction is the point of the suite. A bound that only
# the client enforces is a convention; the cases below send what a well-behaved client never
# would, and require the guest to name what it refused.
#
# The invariant every group is written around: the registry never touches the installed system
# artifacts. `untouched` measures that directly, by asking the guest itself what its volume
# holds before and after a session that publishes, rolls back, resets, is refused several ways
# and is fuzzed.
#
# Cases are grouped so a failure names a subject, and any subset can be run by name:
#
#   boot/proto-test.py                     every group
#   boot/proto-test.py registry publication  those two
#
# Two groups deliberately wait out a guest deadline (the idle session, the abandoned
# publication) and say so while they do it. They are what proves those deadlines exist.
#
# Two things this suite does not test, and why, so their absence is a decision rather than an
# oversight. Registry exhaustion cannot be reached from the protocol: one artifact is capped at
# 32 MB, only verified images are retained, and the whole staged library set is a few megabytes,
# so the 64 MB budget is a second line of defence with no first line able to approach it. What
# is testable about it is the accounting the budget is computed from, and `registry` asserts
# that to the byte. And the crash of an ordinary guest process is the kernel's own subject,
# tested there; the only process whose death could touch the registry is the agent that holds
# it in its memory, and that is a scenario (`agent-restart`), not a protocol case.

import hashlib
import os
import random
import socket
import struct
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

# The instance's own tooling: the socket paths, the guest helpers and the console log are
# already defined there, and a second copy of them would be a second thing to keep true.
import lab

# The wire format, mirrored from `dev_protocol.rs`. Written out here rather than derived from
# the guest, because a suite that read its expectations from the thing under test would agree
# with it whatever either of them did.
HEADER = struct.Struct('<HBBIIHH')
MAGIC = 0x444C
MAGIC_BYTES = struct.pack('<H', MAGIC)
VERSION = 1
MAX_PAYLOAD = 65536 - HEADER.size
MAX_ARTIFACT = 32 * 1024 * 1024
MAX_REGISTRY = 64 * 1024 * 1024
MAX_GENERATIONS = 3
MAX_TERM_INPUT = 4096
MAX_NAME = 48

HELLO, HELLO_ACK, PING, PONG = 0x01, 0x02, 0x03, 0x04
BEGIN, CHUNK, COMMIT, ABORT, PUB_ACK = 0x10, 0x11, 0x12, 0x13, 0x14
LIST, LIST_REPLY, ROLLBACK, ROLLBACK_ACK = 0x15, 0x16, 0x17, 0x18
TERM, TERM_ACK, RESET, RESET_ACK = 0x20, 0x21, 0x22, 0x23
LAUNCH, LAUNCH_ACK, LAUNCH_OUTPUT, LAUNCH_BYTES = 0x30, 0x31, 0x32, 0x33
LAUNCH_STOP, LAUNCH_STOP_ACK = 0x34, 0x35
ERROR = 0xFF

# Statuses, by the name the protocol gives each refusal.
OK = 0
BAD_VERSION, BAD_OPCODE, OVERSIZED, MALFORMED = 1, 2, 3, 4
HANDSHAKE_REQUIRED, DUPLICATE_REQUEST, TIMED_OUT, BUSY, BAD_GENERATION = 5, 6, 7, 8, 9
INCOMPLETE, DIGEST_MISMATCH, NO_SPACE, TERM_REFUSED = 10, 11, 12, 13
NOT_AN_IMAGE, WRONG_TARGET, NO_IDENTITY, NOT_OWNED = 14, 15, 16, 17
NOTHING_TO_ROLL_BACK, NOT_DECLARED = 20, 22
NO_LAUNCHER, LAUNCH_REFUSED, NO_LAUNCH = 23, 24, 25

# The staged tree the real images come from. A publication that is expected to commit has to
# be a real image: the guest verifies the ELF, its target, its identity record and that the
# record names the artifact it was published as.
STAGE = os.path.join(lab.REPO, '.build', 'image', 'x86_64-unknown-none')

# The guest deadlines two groups wait out, in seconds, taken from the protocol's own constants
# with a margin. Waiting is the only way to observe a deadline from outside.
IDLE_SESSION_WAIT = 32
PUBLICATION_WAIT = 13


def staged_library(name):
	for root, _, files in os.walk(STAGE):
		if f'{name}.lslib' in files:
			with open(os.path.join(root, f'{name}.lslib'), 'rb') as handle:
				return handle.read()
	raise SystemExit(f'proto-test: no staged {name}.lslib under {STAGE}; build the tree first')


# An image that declares another machine, made by moving the two bytes the ELF header keeps it
# in. Derived rather than taken from another staged target, so this suite needs only the tree
# it is testing: what is being tested is that the guest reads the declared machine and refuses,
# and one image cannot be for two machines whichever way it came to say so.
def foreign_target(image):
	return image[:18] + struct.pack('<H', 0xB7) + image[20:]


# The same image with the toolchain it was built with altered: publishable, and incompatible
# under the written rule, because different code under an unchanged name is exactly what a
# process that has already resolved against it cannot be handed.
#
# The toolchain is the field to move because every record carries it and its shape is fixed -
# forty hex characters - so the alteration is the same length and stays well formed whatever
# artifact it is applied to. The feature set would do as well for some artifacts and not for
# others, which is not a property a test fixture should depend on.
def incompatible(image):
	key = b'rustc-commit='
	at = image.find(key)
	if at < 0 or image.count(key) != 1:
		raise SystemExit('proto-test: the identity record does not carry one toolchain to alter')
	first = at + len(key)
	return image[:first] + (b'0' if image[first:first + 1] != b'0' else b'1') + image[first + 1:]


class Peer:
	# One connection, handshaken. Every case takes its own, because a session is exactly what
	# several of them are testing the lifetime of.
	def __init__(self, timeout=5, handshake=True):
		self.socket = self.connect(timeout)
		self.buffer = bytearray()
		self.request = 1
		self.bounds = None
		if handshake:
			# Retried and matched on the request id, the way the real client is: a previous case
			# can leave the guest holding a fragment it will never see the rest of, and until its
			# deadline expires this handshake is swallowed as that fragment's payload.
			end = time.monotonic() + 20
			while time.monotonic() < end:
				self.socket.sendall(frame(HELLO, 1))
				reply = self.read(3)
				if reply and reply.opcode == HELLO_ACK and reply.request == 1:
					self.bounds = reply.payload
					return
			raise SystemExit('proto-test: the guest never completed a handshake')

	@staticmethod
	def connect(timeout):
		for _ in range(50):
			handle = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
			handle.settimeout(max(timeout, 1))
			try:
				handle.connect(lab.DEV_CHANNEL_SOCK)
				return handle
			except OSError:
				handle.close()
				time.sleep(0.1)
		raise SystemExit(f'proto-test: cannot reach {lab.DEV_CHANNEL_SOCK}; is `./dev.sh up` running?')

	# Read one frame, resynchronising on the magic. Junk before a frame is skipped rather than
	# fatal, because the x86_64 channel really does carry a firmware preamble nobody framed.
	def read(self, timeout=5):
		end = time.monotonic() + timeout
		while True:
			at = self.buffer.find(MAGIC_BYTES)
			if at >= 0:
				del self.buffer[:at]
				if len(self.buffer) >= HEADER.size:
					_, _, opcode, request, generation, length, status = HEADER.unpack(bytes(self.buffer[:HEADER.size]))
					if len(self.buffer) >= HEADER.size + length:
						payload = bytes(self.buffer[HEADER.size:HEADER.size + length])
						del self.buffer[:HEADER.size + length]
						return Reply(opcode, request, generation, status, payload)
			else:
				del self.buffer[:max(len(self.buffer) - 1, 0)]
			left = end - time.monotonic()
			if left <= 0:
				return None
			self.socket.settimeout(left)
			try:
				more = self.socket.recv(65536)
			except socket.timeout:
				return None
			if not more:
				return None
			self.buffer += more

	def send(self, data):
		self.socket.sendall(data)

	# One request, one reply, with the request id advanced. Replies to earlier ids are skipped
	# rather than mistaken for this one.
	def call(self, opcode, payload=b'', generation=0, timeout=10):
		self.request += 1
		self.socket.sendall(frame(opcode, self.request, payload, generation))
		while True:
			reply = self.read(timeout)
			if reply is None or reply.request == self.request:
				return reply

	def begin(self, name, blob, total=None, digest=None):
		total = len(blob) if total is None else total
		digest = hashlib.sha256(blob).digest() if digest is None else digest
		return self.call(BEGIN, struct.pack('<I', total) + digest + bytes([len(name)]) + name)

	def feed(self, blob, generation, at=0):
		reply = None
		while at < len(blob):
			part = blob[at:at + MAX_PAYLOAD]
			reply = self.call(CHUNK, part, generation)
			at += len(part)
		return reply

	# Publish `blob` as `name` and return the commit's reply, or the first refusal.
	def publish(self, name, blob):
		reply = self.begin(name, blob)
		if reply is None or reply.opcode != PUB_ACK:
			return reply
		self.feed(blob, reply.generation)
		return self.call(COMMIT, b'', reply.generation, timeout=30)

	# The registry as {name: [(generation, length, verdict)]}, plus the bytes it holds.
	def registry(self):
		reply = self.call(LIST, timeout=30)
		count, total = struct.unpack('<HI', reply.payload[:6])
		at, artifacts = 6, {}
		for _ in range(count):
			name_len = reply.payload[at]
			name = reply.payload[at + 1:at + 1 + name_len].decode()
			at += 1 + name_len
			held = reply.payload[at]
			at += 1
			generations = []
			for _ in range(held):
				generation, length = struct.unpack('<II', reply.payload[at:at + 8])
				verdict = reply.payload[at + 48]
				detail_len = reply.payload[at + 49]
				generations.append((generation, length, verdict))
				at += 50 + detail_len
			artifacts[name] = generations
		return artifacts, total

	def close(self):
		self.socket.close()
		# The guest ends a session when its peer goes, and several cases below depend on
		# starting from a closed one rather than racing the close.
		time.sleep(0.3)


class Reply:
	__slots__ = ('opcode', 'request', 'generation', 'status', 'payload')

	def __init__(self, opcode, request, generation, status, payload):
		self.opcode, self.request, self.generation, self.status, self.payload = opcode, request, generation, status, payload

	# What every case compares: what came back, and why.
	def outcome(self):
		return (self.opcode, self.status)


# A launch request's payload: three typed fields, never a command line. Encoded here rather
# than borrowed from `lab`, because the shape of a request is the thing under test.
def launch_payload(name, args, cwd):
	return bytes([len(name)]) + name + struct.pack('<H', len(args)) + args + struct.pack('<H', len(cwd)) + cwd


def frame(opcode, request, payload=b'', generation=0, status=0, version=VERSION, length=None):
	declared = len(payload) if length is None else length
	return HEADER.pack(MAGIC, version, opcode, request, generation, declared, status) + payload


class Suite:
	def __init__(self):
		self.results = []
		self.group = ''

	def start(self, group):
		self.group = group
		print(f'-- {group}')

	def check(self, name, got, want):
		ok = got == want
		print(f'{"ok  " if ok else "FAIL"} {name}: {got}' + ('' if ok else f'   expected {want}'))
		self.results.append((self.group, name, ok))
		return ok

	def note(self, text):
		print(f'     {text}')

	def failures(self):
		return [(group, name) for group, name, ok in self.results if not ok]


# ---- taxonomy: every rejection the protocol defines --------------------------------------
#
# One connection per case, because several of these end the session and the next case must not
# inherit it.
def group_taxonomy(suite):
	def case(name, sent, expected, timeout=3, handshake=True, pause=0.0):
		peer = Peer(handshake=handshake)
		try:
			if pause:
				time.sleep(pause)
			for chunk in sent:
				peer.send(chunk)
			seen = []
			for _ in expected:
				reply = peer.read(timeout)
				if reply is None:
					break
				seen.append(reply.outcome())
			suite.check(name, seen, list(expected))
		finally:
			peer.close()

	pong = (PONG, OK)
	# A length past the payload bound is still expressible in the u16 field, so it has to be
	# refused rather than assumed away.
	case('oversized payload refused', [frame(PING, 2, b'x' * 65535)], [(ERROR, OVERSIZED)])
	case('unknown opcode refused', [frame(0x77, 2)], [(ERROR, BAD_OPCODE)])
	# Request id 0 and a generation on a session-scoped operation are both malformed in v1.
	case('request id zero refused', [frame(PING, 0)], [(ERROR, MALFORMED)])
	case('generation on a session operation refused', [frame(PING, 2, generation=7)], [(ERROR, MALFORMED)])
	# Ids must be strictly increasing, which rejects a duplicate and a replay with one word of
	# state instead of a table of in-flight ids.
	case('duplicate request id refused', [frame(PING, 2), frame(PING, 2)], [pong, (ERROR, DUPLICATE_REQUEST)])
	case('request id that went backwards refused', [frame(PING, 5), frame(PING, 3)], [pong, (ERROR, DUPLICATE_REQUEST)])
	case('unknown version refused', [frame(PING, 2, version=9)], [(ERROR, BAD_VERSION)])
	# A frame that stops halfway dies on its own deadline rather than holding the parser open.
	case('partial frame expires', [frame(PING, 2, length=64) + b'half'], [(ERROR, TIMED_OUT)], timeout=6)
	# Junk before a valid frame is resynchronised past: the x86_64 port really does carry one.
	case('resynchronises past junk', [b'UEFI noise\r\n' * 20 + frame(PING, 2, b'hi')], [pong])
	# A rejection is not a session ending: the id after it is served normally.
	case('session survives a rejection', [frame(PING, 2), frame(PING, 2), frame(PING, 3, b'ok')], [pong, (ERROR, DUPLICATE_REQUEST), pong])
	# The session belongs to the port, not to a host connection: the socket is the wire, and
	# reconnecting to it does not start a new session or reset the request watermark. So the
	# only way to observe an operation outside a session is to let one expire, which is what
	# the next case does - and it asserts both things at once.
	suite.note(f'waiting out the {IDLE_SESSION_WAIT} s idle-session deadline...')
	case('an operation outside a session is refused once it expires', [frame(PING, 2)], [(ERROR, HANDSHAKE_REQUIRED)], pause=IDLE_SESSION_WAIT)
	case('the next handshake reopens it', [frame(PING, 2, b'again')], [pong])


# ---- publication: the streaming protocol and every way it is specified to fail ------------
def group_publication(suite):
	blob = staged_library('lsrt')
	if len(blob) <= MAX_PAYLOAD:
		raise SystemExit('proto-test: the multi-chunk cases need an image larger than one frame')

	peer = Peer()
	reply = peer.publish(b'lsrt', blob)
	suite.check('a publication larger than one frame commits', reply.outcome(), (PUB_ACK, OK))
	artifacts, _ = peer.registry()
	suite.check('the registry holds it', 'lsrt' in artifacts, True)
	peer.close()

	# Bytes that do not match the digest the candidate declared.
	peer = Peer()
	reply = peer.begin(b'lsrt', blob, digest=bytes(32))
	generation = reply.generation
	peer.feed(blob, generation)
	suite.check('a digest mismatch is refused', peer.call(COMMIT, b'', generation, timeout=30).outcome(), (ERROR, DIGEST_MISMATCH))
	suite.check('and the candidate went with the refusal', peer.call(COMMIT, b'', generation).outcome(), (ERROR, BAD_GENERATION))
	peer.close()

	# A commit before every declared byte arrived, and the recovery from it.
	peer = Peer()
	generation = peer.begin(b'lsrt', blob).generation
	peer.call(CHUNK, blob[:100], generation)
	reply = peer.call(COMMIT, b'', generation)
	suite.check('a commit before the last byte is refused', (reply.opcode, reply.status, struct.unpack('<I', reply.payload[:4])[0]), (ERROR, INCOMPLETE, 100))
	peer.feed(blob, generation, at=100)
	suite.check('the candidate survives it, so the host can finish', peer.call(COMMIT, b'', generation, timeout=30).outcome(), (PUB_ACK, OK))
	peer.close()

	# One candidate at a time, and abort as the way to give the slot back.
	peer = Peer()
	generation = peer.begin(b'lsrt', blob).generation
	suite.check('a second candidate is refused', peer.begin(b'base-proto', blob).outcome(), (ERROR, BUSY))
	suite.check('abort is accepted', peer.call(ABORT, b'', generation).outcome(), (PUB_ACK, OK))
	suite.check('aborting an aborted candidate is refused', peer.call(ABORT, b'', generation).outcome(), (ERROR, BAD_GENERATION))
	reply = peer.begin(b'lsrt', blob)
	suite.check('abort freed the slot', reply.outcome(), (PUB_ACK, OK))
	peer.call(ABORT, b'', reply.generation)
	peer.close()

	# Chunks past what was declared, and chunks against no candidate at all.
	peer = Peer()
	generation = peer.begin(b'lsrt', blob[:1000]).generation
	suite.check('a chunk past the declared length is refused', peer.call(CHUNK, blob[:2000], generation).outcome(), (ERROR, OVERSIZED))
	suite.check('the overrun released the candidate', peer.call(CHUNK, b'x', generation).outcome(), (ERROR, BAD_GENERATION))
	suite.check('a chunk against no candidate is refused', peer.call(CHUNK, b'x', 0).outcome(), (ERROR, BAD_GENERATION))
	peer.close()

	# The declared bounds, checked at the guest rather than trusted to the host.
	peer = Peer()
	suite.check('a size past the artifact bound is refused', peer.begin(b'lsrt', b'', total=MAX_ARTIFACT + 1, digest=bytes(32)).outcome(), (ERROR, OVERSIZED))
	suite.check('a zero-length declaration is refused', peer.begin(b'lsrt', b'', total=0, digest=bytes(32)).outcome(), (ERROR, OVERSIZED))
	# A name is a bounded identifier naming a registry slot, never a path.
	suite.check('an empty name is refused', peer.begin(b'', blob).outcome(), (ERROR, MALFORMED))
	suite.check('a name with a separator is refused', peer.begin(b'../escape', blob).outcome(), (ERROR, MALFORMED))
	suite.check('a name starting with a dot is refused', peer.begin(b'.hidden', blob).outcome(), (ERROR, MALFORMED))
	suite.check('a name past the length bound is refused', peer.begin(b'x' * (MAX_NAME + 1), blob).outcome(), (ERROR, MALFORMED))
	suite.check('a ping carrying a generation is refused', peer.call(PING, b'', 5).outcome(), (ERROR, MALFORMED))
	suite.check('a list carrying a generation is refused', peer.call(LIST, b'', 5).outcome(), (ERROR, MALFORMED))
	peer.close()

	# A candidate abandoned mid-stream is dropped on its own deadline, before the session's.
	peer = Peer()
	generation = peer.begin(b'lsrt', blob).generation
	peer.call(CHUNK, blob[:100], generation)
	suite.note(f'waiting out the {PUBLICATION_WAIT} s publication deadline...')
	time.sleep(PUBLICATION_WAIT)
	suite.check('an abandoned candidate expires', peer.call(CHUNK, blob[100:60000], generation).outcome(), (ERROR, BAD_GENERATION))
	reply = peer.call(PING, b'alive')
	suite.check('the session outlived the candidate', (reply.opcode, reply.status, reply.payload), (PONG, OK, b'alive'))
	peer.call(RESET)
	peer.close()


# ---- terminal: typing into the guest's console, and reset ---------------------------------
def group_terminal(suite):
	peer = Peer()
	bounds = struct.unpack('<IIHHIHH', peer.bounds[:20])
	suite.check('the handshake reports the terminal bound', bounds[6], MAX_TERM_INPUT)

	# How much of a line the console takes depends on how full its queue is at that moment,
	# which the protocol does not promise. What it does promise is that the reply accounts for
	# every byte: taken whole, or refused partway with the count that landed.
	line = b'echo protocol-typed-this\r'
	reply = peer.call(TERM, line)
	accounted = struct.unpack('<H', reply.payload[:2])[0] if reply.payload else 0
	suite.check('a typed line is fully accounted for', (reply.opcode == TERM_ACK and accounted == len(line)) or (reply.opcode == ERROR and reply.status == TERM_REFUSED and accounted < len(line)), True)
	# BOUNDED. A guest that keeps accounting for zero bytes - a console whose queue never drains -
	# left this loop spinning forever, inside a suite that is otherwise deadline-bounded throughout.
	# A retry that cannot end is not a retry, it is a hang with a progress condition.
	sent = accounted
	deadline = time.monotonic() + 30
	stalled = 0
	while sent < len(line) and time.monotonic() < deadline:
		reply = peer.call(TERM, line[sent:])
		took = struct.unpack('<H', reply.payload[:2])[0]
		sent += took
		stalled = 0 if took else stalled + 1
		if stalled >= 100:
			break
		if reply.opcode == ERROR:
			time.sleep(0.05)
	suite.check('resuming from the count delivers the rest', sent, len(line))

	suite.check('empty input is refused', peer.call(TERM, b'').outcome(), (ERROR, MALFORMED))
	suite.check('input past the terminal bound is refused', peer.call(TERM, b'x' * (MAX_TERM_INPUT + 1)).outcome(), (ERROR, OVERSIZED))
	reply = peer.call(TERM, b'x' * MAX_TERM_INPUT)
	accepted = struct.unpack('<H', reply.payload[:2])[0]
	if reply.opcode == TERM_ACK:
		suite.check('input at the bound is accounted for', (reply.status, accepted), (OK, MAX_TERM_INPUT))
	else:
		suite.check('input at the bound is refused with a resume point', (reply.opcode, reply.status, accepted < MAX_TERM_INPUT), (ERROR, TERM_REFUSED, True))
		suite.note(f'the console took {accepted} of {MAX_TERM_INPUT} B before refusing')
	suite.check('typed input carrying a generation is refused', peer.call(TERM, b'x', 9).outcome(), (ERROR, MALFORMED))

	# Reset drops the registry and any open candidate, and says what it dropped. Start from a
	# known registry: it outlives sessions by design, so whatever an earlier group published
	# would otherwise decide these counts.
	peer.call(RESET)
	blob = staged_library('base-proto')
	for _ in range(2):
		peer.publish(b'base-proto', blob)
	generation = peer.begin(b'lsrt', blob).generation
	peer.call(CHUNK, blob[:5], generation)
	reply = peer.call(RESET)
	suite.check('reset reports what it dropped', (reply.opcode, reply.status, struct.unpack('<H', reply.payload[:2])[0], reply.payload[2]), (RESET_ACK, OK, 2, 1))
	artifacts, spent = peer.registry()
	suite.check('the registry is empty afterwards', (len(artifacts), spent), (0, 0))
	suite.check('the open candidate went with it', peer.call(CHUNK, blob[5:], generation).outcome(), (ERROR, BAD_GENERATION))
	reply = peer.call(RESET)
	suite.check('resetting an empty registry is still ok', (reply.opcode, reply.status, struct.unpack('<H', reply.payload[:2])[0], reply.payload[2]), (RESET_ACK, OK, 0, 0))
	suite.check('reset carrying a generation is refused', peer.call(RESET, b'', 3).outcome(), (ERROR, MALFORMED))
	reply = peer.call(PING, b'alive')
	suite.check('the session outlives a reset', (reply.opcode, reply.status, reply.payload), (PONG, OK, b'alive'))
	peer.close()


# ---- registry: verification, retention, accounting and rollback ---------------------------
def group_registry(suite):
	base = staged_library('base-proto')
	config = staged_library('config-proto')

	peer = Peer()
	peer.call(RESET)
	bounds = struct.unpack('<IIHHIHHIB', peer.bounds[:25])
	suite.check('the handshake reports the stated limits', (bounds[4], bounds[7], bounds[5]), (MAX_ARTIFACT, MAX_REGISTRY, MAX_GENERATIONS))
	# The registry is development-profile-only and the handshake says so, so a host fails here
	# rather than after streaming an artifact into a boot that could not keep it.
	suite.check('the handshake reports the registry is available', bounds[8], 1)

	suite.check('a valid library commits', peer.publish(b'base-proto', base).status, OK)
	# The verdict compares against the INSTALLED artifact, so republishing what is installed is
	# compatible by construction - and that is the comparison a launch will make.
	artifacts, _ = peer.registry()
	suite.check('the verdict compares against the installed artifact', artifacts['base-proto'][-1][2], 1)
	suite.check('an incompatible generation is kept and said to be so', peer.publish(b'base-proto', incompatible(base)).status, OK)
	artifacts, _ = peer.registry()
	suite.check('and its verdict names the cold path', artifacts['base-proto'][-1][2], 2)

	# Each verification check refuses with its own status, and none of them retains the bytes.
	suite.check('junk is not a readable image', peer.publish(b'base-proto', b'x' * 5000).status, NOT_AN_IMAGE)
	suite.check('an image for another machine is the wrong target', peer.publish(b'base-proto', foreign_target(base)).status, WRONG_TARGET)
	# The manifest stays the authority for artifact names: one it does not declare has nothing
	# to shadow and is refused before anything else is looked at.
	suite.check('an undeclared artifact name is refused', peer.publish(b'notathing', base).status, NOT_DECLARED)
	# A declared name the image is not is a different refusal: the record has to name the
	# artifact it is being published as.
	suite.check('an image published as another declared artifact is not owned', peer.publish(b'config-proto', base).status, NOT_OWNED)
	artifacts, spent = peer.registry()
	suite.check('only the verified publications are retained', sorted(artifacts), ['base-proto'])
	suite.check('refused candidates cost no registry bytes', spent, 2 * len(base))

	# Retention is per artifact and by count.
	peer.call(RESET)
	for _ in range(MAX_GENERATIONS + 1):
		peer.publish(b'base-proto', base)
	peer.publish(b'config-proto', config)
	artifacts, spent = peer.registry()
	suite.check('at most three generations per artifact', len(artifacts['base-proto']), MAX_GENERATIONS)
	suite.check('another artifact keeps its own history', len(artifacts['config-proto']), 1)
	suite.check('accounting follows what is retained', spent, MAX_GENERATIONS * len(base) + len(config))

	# Rollback is a named operation, and it releases the bytes it discards.
	before = artifacts['base-proto']
	reply = peer.call(ROLLBACK, bytes([len(b'base-proto')]) + b'base-proto')
	now, dropped = struct.unpack('<II', reply.payload[:8])
	suite.check('rollback returns the previous generation', (reply.opcode, now, dropped), (ROLLBACK_ACK, before[-2][0], before[-1][0]))
	_, after = peer.registry()
	suite.check('rollback releases the discarded bytes', after, spent - len(base))
	peer.call(ROLLBACK, bytes([len(b'base-proto')]) + b'base-proto')
	suite.check('rollback past the first generation is refused', peer.call(ROLLBACK, bytes([len(b'base-proto')]) + b'base-proto').outcome(), (ERROR, NOTHING_TO_ROLL_BACK))
	suite.check('rollback of an unknown artifact is refused', peer.call(ROLLBACK, bytes([len(b'nosuch')]) + b'nosuch').outcome(), (ERROR, NOTHING_TO_ROLL_BACK))

	peer.call(RESET)
	artifacts, spent = peer.registry()
	suite.check('reset empties the registry and its accounting together', (len(artifacts), spent), (0, 0))
	peer.close()


# ---- launch: starting, reading and ending a program through the launcher ------------------
#
# The agent starts with a fresh restart, because "nothing has been launched" is a state only a
# new agent is reliably in: a launch outlives the session that made it, so a previous group or
# a previous run would otherwise decide what the first case here sees.
def group_launch(suite):
	suite.note('restarting the agent, so nothing is launched...')
	guest = lab.LabGuest(60)
	if not guest.restart(60):
		raise SystemExit('proto-test: the agent did not restart')
	peer = Peer()
	suite.check('stopping when nothing was launched is refused', peer.call(LAUNCH_STOP).outcome(), (ERROR, NO_LAUNCH))
	suite.check('reading output when nothing was launched is refused', peer.call(LAUNCH_OUTPUT).outcome(), (ERROR, NO_LAUNCH))
	# A component the permission manifest does not cover cannot be launched. That is the
	# boundary working: the launcher stays the authority, and the agent asks rather than loads.
	suite.check('an unlaunchable component is refused', peer.call(LAUNCH, launch_payload(b'echo', b'', b'vol://system')).outcome(), (ERROR, LAUNCH_REFUSED))
	suite.check('a name past the launch bound is refused', peer.call(LAUNCH, launch_payload(b'x' * 65, b'', b'vol://system')).outcome(), (ERROR, MALFORMED))

	reply = peer.call(LAUNCH, launch_payload(b'uname', b'', b'vol://system'), timeout=30)
	suite.check('a declared component launches', reply.outcome(), (LAUNCH_ACK, OK))
	printed, exited = b'', False
	deadline = time.monotonic() + 30
	while not exited and time.monotonic() < deadline:
		answer = peer.call(LAUNCH_OUTPUT, timeout=30)
		if answer.opcode != LAUNCH_BYTES:
			break
		exited = bool(answer.payload[0])
		printed += answer.payload[2:]
	suite.check('its own output comes back, and its end with it', (b'LiberSystem' in printed, exited), (True, True))
	# Reading consumes, so a second read of a finished program is empty and still says it ended.
	answer = peer.call(LAUNCH_OUTPUT)
	suite.check('reading again consumes nothing and still reports the exit', (answer.opcode, answer.payload[0], answer.payload[2:]), (LAUNCH_BYTES, 1, b''))
	# ONE REQUEST, ONE ASSERTION. This called `LAUNCH_STOP` TWICE and combined the first reply's
	# outcome with the SECOND reply's payload, so a wrong "signalled something" byte in the first
	# was hidden by the second - and stopping is state-changing, so the two requests are not
	# interchangeable readings of one fact.
	stopped = peer.call(LAUNCH_STOP)
	suite.check('stopping a program that already finished signals nothing', (stopped.outcome(), stopped.payload[0]), ((LAUNCH_STOP_ACK, OK), 0))

	# One that does not end on its own: it holds a terminal until something ends it, which is
	# the case the operation exists for.
	peer.call(LAUNCH, launch_payload(b'ps', b'-i', b'vol://system'), timeout=30)
	time.sleep(1)
	reply = peer.call(LAUNCH_STOP)
	suite.check('stopping a running program signals it', (reply.opcode, reply.status, reply.payload[0]), (LAUNCH_STOP_ACK, OK, 1))
	answer = peer.call(LAUNCH_OUTPUT)
	suite.check('and it is reported as ended afterwards', (answer.opcode, answer.payload[0]), (LAUNCH_BYTES, 1))
	peer.close()


# ---- pipelining: the advertised outstanding bound, actually outstanding -------------------
#
# Written to read while it writes, which is what pipelining is. A host that sends the whole
# burst first deadlocks against its own socket buffer at this size, and no guest can rescue it
# from that - so this is also the shape the bound is documented to require.
def group_pipeline(suite):
	count, size = 16, 32768
	peer = Peer()
	payload = bytes((index * 7) & 0xFF for index in range(size))
	burst = b''.join(frame(PING, 2 + index, payload) for index in range(count))
	started = time.monotonic()
	peer.socket.setblocking(False)
	at, deadline = 0, time.monotonic() + 60
	while at < len(burst) and time.monotonic() < deadline:
		try:
			at += peer.socket.send(burst[at:at + 65536])
		except BlockingIOError:
			pass
		try:
			more = peer.socket.recv(65536)
			if more:
				peer.buffer += more
		except BlockingIOError:
			pass
		except OSError:
			break
	peer.socket.setblocking(True)
	seen = []
	while len(seen) < count:
		reply = peer.read(30)
		if reply is None:
			break
		seen.append((reply.opcode, reply.request, reply.status, reply.payload == payload))
	elapsed = (time.monotonic() - started) * 1000
	# Compared in four parts rather than as one long list, so a failure says which property
	# broke: how many came back, that each is a clean pong, that they are in the order they
	# were asked, and that each carries back the payload it was sent.
	got = (len(seen), {(opcode, status) for opcode, _, status, _ in seen}, [request for _, request, _, _ in seen] == [2 + index for index in range(count)], all(echoed for _, _, _, echoed in seen))
	suite.check(f'{count} pipelined requests are answered in order, {size} B each', got, (count, {(PONG, OK)}, True, True))
	suite.note(f'round trip for the whole burst: {elapsed:.0f} ms')
	peer.close()


# Three rounds of hostile input, returning what the last of them was answered with. Separate
# from the group below because `untouched` puts the guest through the same rounds as pressure
# on a claim of its own, and a shared body is what keeps the two from drifting apart.
def fuzz_rounds(seed):
	random.seed(seed)
	# Round one: random bytes with no framing at all.
	peer = Peer()
	for _ in range(200):
		peer.send(bytes(random.getrandbits(8) for _ in range(random.randint(1, 3000))))
	time.sleep(0.5)
	peer.close()

	# Round two: well-formed headers with random fields, which is the harder case - every one
	# of these reaches the parser instead of being skipped as junk.
	peer = Peer()
	for _ in range(400):
		opcode = random.choice([random.getrandbits(8), BEGIN, CHUNK, COMMIT, LIST, PING, RESET])
		length = random.choice([0, random.randint(1, 200), 65535])
		body = bytes(random.getrandbits(8) for _ in range(min(length, 400)))
		peer.send(HEADER.pack(random.choice([MAGIC, MAGIC, random.getrandbits(16)]), random.choice([VERSION, VERSION, random.getrandbits(8)]), opcode, random.getrandbits(32), random.getrandbits(32), length, random.getrandbits(16)) + body)
	time.sleep(0.5)
	peer.close()

	# Round three: one frame delivered a byte at a time, so the parser is seen in every partial
	# state a stream can leave it in.
	peer = Peer()
	for byte in frame(PING, 2, bytes(64)):
		peer.send(bytes([byte]))
		time.sleep(0.005)
	reply = peer.read(10)
	peer.close()
	return reply


# ---- fuzz: malformed input cannot escape the registry or take the guest down --------------
def group_fuzz(suite, seed=20260727):
	base = staged_library('base-proto')
	peer = Peer()
	peer.call(RESET)
	peer.publish(b'base-proto', base)
	before = peer.registry()
	suite.check('a known registry is in place', (len(before[0]), before[1]), (1, len(base)))
	peer.close()

	reply = fuzz_rounds(seed)
	suite.check('a frame delivered one byte at a time is still answered', reply and (reply.opcode, reply.request, reply.status), (PONG, 2, OK))

	time.sleep(1)
	peer = Peer()
	reply = peer.call(PING, b'still here')
	suite.check('the guest still answers', (reply.opcode, reply.status, reply.payload), (PONG, OK, b'still here'))
	suite.check('the registry is exactly as it was', peer.registry(), before)
	peer.call(RESET)
	peer.close()


# ---- untouched: the installed system artifacts, asked of the guest ------------------------
#
# The clause this whole facility rests on, measured rather than argued: the registry only ever
# reads the volume. It is asked of the guest itself, because the guest is the only observer
# whose answer is about the volume rather than about what the host believes it built.
def group_untouched(suite):
	watched = ('vol://system/lib/protocol', 'vol://system/lib/runtime', 'vol://system/libexec', 'vol://system/drivers')
	guest = lab.LabGuest(30)

	def listing(path):
		# Clear the shell's line editor first: terminal input is a real terminal, and an earlier
		# group that typed without a newline leaves its characters in the buffer.
		guest.type_text('\x03', False, 10)
		time.sleep(0.3)
		at = guest.serial_size()
		guest.type_text(f'ls {path}', True, 10)
		deadline = time.monotonic() + 30
		while time.monotonic() < deadline:
			time.sleep(0.5)
			text = guest.serial_since(at)
			if 'bytes total' in text:
				body = text[text.index('\n'):text.index('bytes total')]
				# Names and sizes only. A re-read of an untouched file must not depend on the
				# clock, and the timestamps are the only part of a listing that does.
				#
				# WHAT THIS DOES NOT COVER, stated because the group's name overstates it: a
				# mutation that preserves a file's length is invisible here, as is a change to
				# anything on the volume outside these four directories. Covering it needs a
				# guest-side digest of each file, and this system ships no such program - `cat`,
				# `hexdump` and `wc` are what there is, and hexdumping every installed library over
				# a serial console is not a check anyone would run. The claim this group can honestly
				# make is that the installed SET did not change, and that is the claim it makes.
				return '\n'.join(' '.join(line.split()[:2]) for line in body.splitlines() if line.strip())
		raise SystemExit(f'proto-test: the guest did not list {path}')

	before = {path: listing(path) for path in watched}

	# Everything the registry can be asked to do, including every way it can refuse.
	peer = Peer()
	peer.call(RESET)
	for name in (b'base-proto', b'config-proto', b'session-proto'):
		peer.publish(name, staged_library(name.decode()))
	peer.publish(b'base-proto', staged_library('base-proto'))
	peer.publish(b'base-proto', incompatible(staged_library('base-proto')))
	peer.call(ROLLBACK, bytes([len(b'base-proto')]) + b'base-proto')
	peer.publish(b'lsrt', staged_library('lsrt'))
	peer.publish(b'notathing', staged_library('base-proto'))
	peer.publish(b'base-proto', foreign_target(staged_library('base-proto')))
	peer.publish(b'base-proto', b'not an image at all')
	peer.close()
	fuzz_rounds(seed=20260728)
	peer = Peer()
	peer.call(RESET)
	peer.close()

	after = {path: listing(path) for path in watched}
	for path in watched:
		entries = len(before[path].splitlines())
		if not suite.check(f'{path} is untouched ({entries} entries)', after[path] == before[path], True):
			suite.note(f'before:\n{before[path]}\nafter:\n{after[path]}')
	suite.check('the guest still takes terminal input', guest.type_text('echo still-here', True, 10), True)
	suite.check('the shell prompt still comes back', guest.wait_prompt(20), True)


GROUPS = {
	'taxonomy': group_taxonomy,
	'publication': group_publication,
	'terminal': group_terminal,
	'registry': group_registry,
	'launch': group_launch,
	'pipeline': group_pipeline,
	'fuzz': group_fuzz,
	'untouched': group_untouched,
}


def main(argv):
	wanted = argv or list(GROUPS)
	unknown = [name for name in wanted if name not in GROUPS]
	if unknown:
		raise SystemExit(f'proto-test: unknown group(s) {", ".join(unknown)}; known: {", ".join(GROUPS)}')
	if not os.path.exists(lab.DEV_CHANNEL_SOCK):
		raise SystemExit('proto-test: no development instance (run `./dev.sh up` first)')
	suite = Suite()
	started = time.monotonic()
	for name in wanted:
		suite.start(name)
		GROUPS[name](suite)
	failures = suite.failures()
	elapsed = time.monotonic() - started
	print(f'\nproto-test: {len(suite.results) - len(failures)}/{len(suite.results)} cases behaved as specified in {elapsed:.0f} s')
	for group, name in failures:
		print(f'     failed: {group}: {name}')
	return 1 if failures else 0


if __name__ == '__main__':
	sys.exit(main(sys.argv[1:]))
