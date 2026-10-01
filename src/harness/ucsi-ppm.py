#!/usr/bin/env python3
"""THE TYPE-C GATE'S UCSI PLATFORM POLICY MANAGER - the PPM behind the fixture SSDT's `\\_SB.UCSI`.

It answers ONLY through the fixture's staging areas, as an embedded controller behind shipping firmware does: the
`_DSM`'s function 1 copies CONTROL and MESSAGE_OUT into the OUTBOUND area and rings the doorbell; this program answers
into the INBOUND area; the `_DSM`'s function 2 - or line 4's `_E04`, before its `Notify(0x80)` - copies that into the
mailbox. A reset's completion is written to the inbound area alone, with notifications off, so a driver that polls
without function 2 never sees it.

THE PROFILES:
  v12       VERSION 1.2, alternate-mode override, the alternate-mode, PDO and cable details
  v21       VERSION 2.1 without the override, with power readings, refusing SET_PDR
  slow      v21 whose first reset and the two commands after it each take longer than five seconds and less than ten
  spoiling  v21 that spoils its inbound area once it has notified, as some shipping firmware spoils its copy

THE CONNECTORS: two, each dual-role for power and data; connector 2 offers DisplayPort. Partners are attached,
detached and driven from the control socket (`--control`), one text command a line:
  attach N charger|host|dp     detach N          ppm-enter N (the PPM enters DisplayPort itself)
  malformed length|connector   (the next connector status answers past its size, or names connector 7)
  silent SECONDS               (nothing answered for that long; the last doorbell answered when it ends)
  silent-next SECONDS          (the same, from the next command on - so a command is in flight when it starts)
  status                       violations                 log

THE COMMAND LOG (`--log`), one line per command and per notification, and a VIOLATION line whenever the OPM breaks the
discipline: a second command while a completion is unacknowledged, a completion left unacknowledged, a write of
CONTROL not followed by function 1, function 2 evaluated after a notification and before the next command, a
connector change acknowledged other than beside the status that read it.

  ucsi-ppm.py --memory FILE --gpio-control SOCKET --control SOCKET --log FILE --profile NAME [--ready FILE]
  ucsi-ppm.py --self-test
"""

import argparse
import mmap
import os
import select
import socket
import struct
import sys
import time

# The fixture's layout (acpi-fixture.py's UCSI_* constants).
UCSI_RANGE = 0x6000
PRESENT = 0x1000
DOORBELL = 0x1004
REFRESHES = 0x1008
NOTIFIES = 0x100C
CONTROL = 0x1010
VERSION = 0x1018
CCI = 0x101C
MESSAGE_OUT = 0x1100
MESSAGE_IN = 0x1200
LINE = 4

NOT_SUPPORTED = 1 << 25
RESET_COMPLETE = 1 << 27
BUSY = 1 << 28
ACK_COMPLETE = 1 << 29
ERROR = 1 << 30
COMMAND_COMPLETE = 1 << 31

PPM_RESET, ACK_CC_CI, SET_NOTIFICATION_ENABLE, GET_CAPABILITY, GET_CONNECTOR_CAPABILITY = 0x01, 0x04, 0x05, 0x06, 0x07
SET_CCOM, SET_UOR, SET_PDM, SET_PDR = 0x08, 0x09, 0x0A, 0x0B
GET_ALTERNATE_MODES, GET_CAM_SUPPORTED, GET_CURRENT_CAM, SET_NEW_CAM, GET_PDOS = 0x0C, 0x0D, 0x0E, 0x0F, 0x10
GET_CABLE_PROPERTY, GET_CONNECTOR_STATUS, GET_ERROR_STATUS, GET_ATTENTION_VDO = 0x11, 0x12, 0x13, 0x16
NAMES = {PPM_RESET: 'PPM_RESET', ACK_CC_CI: 'ACK_CC_CI', SET_NOTIFICATION_ENABLE: 'SET_NOTIFICATION_ENABLE', GET_CAPABILITY: 'GET_CAPABILITY', GET_CONNECTOR_CAPABILITY: 'GET_CONNECTOR_CAPABILITY', SET_CCOM: 'SET_CCOM', SET_UOR: 'SET_UOR', SET_PDM: 'SET_PDM', SET_PDR: 'SET_PDR', GET_ALTERNATE_MODES: 'GET_ALTERNATE_MODES', GET_CAM_SUPPORTED: 'GET_CAM_SUPPORTED', GET_CURRENT_CAM: 'GET_CURRENT_CAM', SET_NEW_CAM: 'SET_NEW_CAM', GET_PDOS: 'GET_PDOS', GET_CABLE_PROPERTY: 'GET_CABLE_PROPERTY', GET_CONNECTOR_STATUS: 'GET_CONNECTOR_STATUS', GET_ERROR_STATUS: 'GET_ERROR_STATUS', GET_ATTENTION_VDO: 'GET_ATTENTION_VDO'}

# bmOptionalFeatures bits.
SET_CCOM_SUPPORTED, ALT_MODE_DETAILS, ALT_MODE_OVERRIDE, PDO_DETAILS, CABLE_DETAILS = 1 << 0, 1 << 2, 1 << 3, 1 << 4, 1 << 5
# GET_ERROR_STATUS: the partner rejected the swap; a policy conflict.
PARTNER_REJECTED_SWAP = 1 << 9
POLICY_CONFLICT = 1 << 11

DISPLAYPORT = 0xFF01
# The partner's DisplayPort capabilities: UFP_D capable, DP signalling, a receptacle offering pin assignments C and D.
PARTNER_DP_VDO = 0x000C0045
# The connector's own: DFP_D capable, DP signalling, pins C, D and E on a plug.
CONNECTOR_DP_VDO = 0x001C06
# HPD high, UFP_D connected.
ATTENTION_HPD = 0x82


def fixed(millivolts, milliamps, usb=False, unconstrained=False):
	return (int(usb) << 26) | (int(unconstrained) << 27) | ((millivolts // 50) << 10) | (milliamps // 10)


CHARGER_OFFERS = [fixed(5000, 3000, unconstrained=True), fixed(9000, 3000), fixed(15000, 3000), fixed(20000, 3000)]
HOST_OFFERS = [fixed(5000, 3000, usb=True), fixed(9000, 2000, usb=True)]

PROFILES = {
	'v12': {'version': 0x0120, 'features': SET_CCOM_SUPPORTED | ALT_MODE_DETAILS | ALT_MODE_OVERRIDE | PDO_DETAILS | CABLE_DETAILS, 'refuse_pdr': False, 'slow': 0, 'spoil': False},
	'v21': {'version': 0x0210, 'features': ALT_MODE_DETAILS | PDO_DETAILS | CABLE_DETAILS, 'refuse_pdr': True, 'slow': 0, 'spoil': False},
	'slow': {'version': 0x0210, 'features': ALT_MODE_DETAILS | PDO_DETAILS | CABLE_DETAILS, 'refuse_pdr': True, 'slow': 3, 'spoil': False},
	'spoiling': {'version': 0x0210, 'features': ALT_MODE_DETAILS | PDO_DETAILS | CABLE_DETAILS, 'refuse_pdr': True, 'slow': 0, 'spoil': True},
}
SLOW_SECONDS = 7.0


def bits(fields, size):
	"""A little-endian bit string of `size` bytes from (offset, width, value) triples."""
	value = 0
	for at, width, field in fields:
		value |= (field & ((1 << width) - 1)) << at
	return value.to_bytes(size, 'little')


class Partner:
	def __init__(self, kind):
		self.kind = kind
		# The partner's type (1 DFP, 2 UFP) and whether this machine sources.
		self.dfp = kind in ('charger', 'host')
		self.source = kind == 'dp'
		self.offers = CHARGER_OFFERS if kind == 'charger' else HOST_OFFERS if kind == 'host' else []
		self.usb = kind != 'charger'
		self.modes = [(DISPLAYPORT, PARTNER_DP_VDO)] if kind == 'dp' else []
		self.cam = None
		self.configuration = None

	def status(self, version):
		"""GET_CONNECTOR_STATUS's answer, as the version lays it out."""
		pd = bool(self.offers)
		mode = 3 if pd else 4
		rdo = (2 << 28) | (200 << 10) | 300 if pd and len(self.offers) >= 2 else 0
		fields = [(14, 1, 1), (16, 3, mode), (19, 1, 1), (20, 1, int(self.source)), (21, 1, int(self.usb)), (22, 1, int(self.cam is not None)), (29, 3, 1 if self.dfp else 2), (32, 32, rdo)]
		if version < 0x0200:
			return bits(fields, 9)
		if version >= 0x0210 and pd:
			# THE READINGS: 9.02 V and 2 A at five millivolts and five milliamps a unit.
			fields += [(89, 1, 1), (90, 3, 1), (109, 16, 400), (125, 4, 1), (129, 16, 1804)]
		return bits(fields, 19)


def empty_status(version):
	return bits([(14, 1, 1)], 9 if version < 0x0200 else 19)


class Ppm:
	def __init__(self, profile, log=None, now=time.monotonic):
		self.profile = PROFILES[profile]
		self.version = self.profile['version']
		self.log_file = log
		self.now = now
		self.notifications = 0
		self.partners = {1: None, 2: None}
		self.pending = []
		self.outstanding = None
		self.last_status = None
		self.error = 0
		self.slow_left = self.profile['slow']
		self.malformed = None
		self.silent_until = 0.0
		self.silent_next = None
		self.violations = []
		self.lines = []

	def record(self, line):
		self.lines.append(line)
		if self.log_file:
			# STAMPED WITH THE HOST'S TIME, so a gate can place a command against the guest's serial lines - the sleep
			# case's "no command between SUSPENDED and RESUMED".
			self.log_file.write(f'{time.time():.3f} {line}\n')
			self.log_file.flush()

	def violate(self, what):
		self.violations.append(what)
		self.record(f'VIOLATION {what}')

	def change(self, number):
		if number not in self.pending:
			self.pending.append(number)

	def indicator(self):
		return (self.pending[0] << 1) if self.pending else 0

	def answer(self, control, message_out=b''):
		"""(cci, message, notify, delay) for one command: what goes into the inbound area, whether a notification
		follows, and after how long."""
		code = control & 0xFF
		delay = 0.0
		if self.slow_left and code in (PPM_RESET, SET_NOTIFICATION_ENABLE, GET_CAPABILITY):
			self.slow_left -= 1
			delay = SLOW_SECONDS - (1.0 if code != PPM_RESET else 0.0)
		self.record(f'command {NAMES.get(code, hex(code))} control {control:#018x}')
		if code == ACK_CC_CI:
			command_ack = bool(control & (1 << 17))
			change_ack = bool(control & (1 << 16))
			if not command_ack:
				self.violate('a connector change acknowledged alone')
			if change_ack:
				if self.outstanding != GET_CONNECTOR_STATUS or self.last_status is None:
					self.violate('a connector change acknowledged beside something other than its status')
				elif self.last_status in self.pending:
					self.pending.remove(self.last_status)
			self.outstanding = None
			return ACK_COMPLETE | self.indicator(), b'', True, delay
		# A RESET MAY COME AT ANY TIME - it is how an OPM recovers a PPM it gave up on - and clears everything.
		if code == PPM_RESET:
			self.notifications = 0
			self.outstanding = None
			self.pending = []
			return RESET_COMPLETE, b'', False, delay
		if self.outstanding is not None:
			self.violate(f'{NAMES.get(code, hex(code))} sent while the completion of {NAMES.get(self.outstanding, hex(self.outstanding))} was unacknowledged')
		self.outstanding = code
		number = (control >> 16) & 0x7F
		connector = self.partners.get(number)
		data = b''
		cci = COMMAND_COMPLETE
		if code == SET_NOTIFICATION_ENABLE:
			self.notifications = (control >> 16) & 0xFFFF
		elif code == GET_CAPABILITY:
			data = struct.pack('<IB', 1 << 2, 2) + (self.profile['features']).to_bytes(3, 'little') + bytes([1, 0]) + struct.pack('<HHH', 0x0120, 0x0300, 0x0200)
		elif code == GET_CONNECTOR_CAPABILITY:
			data = bytes([0x04 | 0x80, 0x01 | 0x02 | 0x04 | 0x08 | 0x10 | 0x20])
		elif code == GET_CONNECTOR_STATUS:
			if number not in self.partners:
				cci |= ERROR
				self.error = 1 << 1
			else:
				self.last_status = number
				data = connector.status(self.version) if connector else empty_status(self.version)
				if self.malformed == 'length':
					data = data + bytes(1 + (19 if self.version >= 0x0200 else 9) - len(data))
					self.malformed = None
				elif self.malformed == 'connector':
					cci |= 7 << 1
					self.malformed = None
		elif code == GET_PDOS:
			partner = bool(control & (1 << 23))
			offset = (control >> 24) & 0xFF
			count = ((control >> 32) & 0x3) + 1
			offers = connector.offers if (connector and partner) else []
			data = b''.join(struct.pack('<I', offer) for offer in offers[offset:offset + count])
		elif code == GET_CABLE_PROPERTY:
			data = bytes([0x0F, 0x00, 60, 0x01 | 0x10, 0]) if connector else b''
			if not connector:
				cci |= ERROR
				self.error = 1 << 3
		elif code == GET_ALTERNATE_MODES:
			recipient = (control >> 16) & 0x7
			number = (control >> 24) & 0x7F
			offset = (control >> 32) & 0xFF
			count = ((control >> 40) & 0x3) + 1
			connector = self.partners.get(number)
			if recipient == 0:
				modes = [(DISPLAYPORT, CONNECTOR_DP_VDO)] if number == 2 else []
			else:
				modes = connector.modes if connector else []
			data = b''.join(struct.pack('<HI', svid, vdo) for svid, vdo in modes[offset:offset + count])
		elif code == GET_CURRENT_CAM:
			data = bytes([connector.cam if connector and connector.cam is not None else 0xFF])
		elif code == GET_ATTENTION_VDO:
			data = struct.pack('<I', ATTENTION_HPD if connector and connector.cam is not None else 0)
		elif code == SET_NEW_CAM:
			if not self.profile['features'] & ALT_MODE_OVERRIDE:
				cci |= NOT_SUPPORTED
			elif connector is None or not connector.modes:
				cci |= ERROR
				self.error = 1 << 3
			else:
				enter = bool(control & (1 << 23))
				offset = (control >> 24) & 0xFF
				configuration = (control >> 32) & 0xFFFFFFFF
				self.record(f'SET_NEW_CAM connector {number} {"enter" if enter else "exit"} offset {offset} configuration {configuration:#010x}')
				connector.cam = offset if enter else None
				connector.configuration = configuration if enter else None
				self.change(number)
		elif code in (SET_UOR, SET_PDR):
			roles = (control >> 23) & 0x7
			if code == SET_PDR and self.profile['refuse_pdr']:
				cci |= ERROR
				self.error = POLICY_CONFLICT
			elif (roles & 0x3) in (1, 2) and connector is not None:
				# A SWAP ASKED FOR: the data role is taken, the power role is rejected by the partner.
				if code == SET_UOR:
					want_host = roles & 1
					connector.dfp = not want_host
					self.change(number)
				else:
					cci |= ERROR
					self.error = PARTNER_REJECTED_SWAP
		elif code == GET_ERROR_STATUS:
			data = struct.pack('<H', self.error)
			self.error = 0
		elif code in (SET_CCOM, SET_PDM, GET_CAM_SUPPORTED):
			data = b''
		else:
			cci |= NOT_SUPPORTED
		cci |= len(data) << 8
		cci |= self.indicator() if not (cci & (0x7F << 1)) else 0
		return cci, data, True, delay

	# ---------------------------------------------------------------- driving the partners

	def attach(self, number, kind):
		self.partners[number] = Partner(kind)
		self.change(number)

	def detach(self, number):
		self.partners[number] = None
		self.change(number)

	def ppm_enter(self, number):
		partner = self.partners.get(number)
		if partner and partner.modes:
			partner.cam = 0
			self.change(number)
			return True
		return False


class Mailbox:
	"""The fixture's pages over the ivshmem backing file, shared with the guest."""

	def __init__(self, path):
		self.file = open(path, 'r+b')
		self.map = mmap.mmap(self.file.fileno(), 0)

	def u32(self, at):
		return struct.unpack_from('<I', self.map, at)[0]

	def u64(self, at):
		return struct.unpack_from('<Q', self.map, at)[0]

	def put(self, at, data):
		self.map[at:at + len(data)] = data

	def answer(self, version, cci, message):
		self.put(VERSION, struct.pack('<H', version))
		self.put(MESSAGE_IN, message.ljust(256, b'\0')[:256])
		self.put(CCI, struct.pack('<I', cci))

	def spoil(self):
		self.put(CCI, struct.pack('<I', 0xFFFFFFFF))
		self.put(MESSAGE_IN, b'\xEE' * 256)


class Line:
	"""Line 4 through the GPIO backend's control socket: lowered and raised until an event fires."""

	def __init__(self, path):
		self.sock = socket.socket(socket.AF_UNIX)
		self.sock.connect(path)
		self.sock.settimeout(5)

	def ask(self, text):
		self.sock.sendall((text + '\n').encode())
		return self.sock.recv(4096).decode()

	def notify(self, deadline_seconds=5.0):
		end = time.monotonic() + deadline_seconds
		while time.monotonic() < end:
			self.ask(f'lower {LINE}')
			if 'fired' in self.ask(f'raise {LINE}'):
				return True
			time.sleep(0.005)
		return False


def serve(args):
	ppm = Ppm(args.profile, open(args.log, 'a', buffering=1))
	box = Mailbox(args.memory)
	line = Line(args.gpio_control)
	for path in (args.control,):
		try:
			os.unlink(path)
		except FileNotFoundError:
			pass
	control = socket.socket(socket.AF_UNIX)
	control.bind(args.control)
	control.listen(4)
	clients = []
	if args.ready:
		open(args.ready, 'w').close()
	doorbell = box.u32(DOORBELL)
	refreshes = box.u32(REFRESHES)
	notifies = box.u32(NOTIFIES)
	since_notify = False
	last_control = box.u64(CONTROL)
	mailbox_control = box.u64(UCSI_RANGE + 8)
	changed_at = None
	queued = []
	held = None
	unacked_since = None
	ppm.record(f'profile {args.profile} version {ppm.version:#06x}')

	def notify_now():
		nonlocal notifies
		before = box.u32(NOTIFIES)
		if not line.notify():
			ppm.record('a notification did not fire: the guest has not armed line 4')
			return
		# THE COPY IS THE GUEST'S, in `_E04`, counted before its `Notify`: waited for before anything spoils it.
		end = time.monotonic() + 2
		while box.u32(NOTIFIES) == before and time.monotonic() < end:
			time.sleep(0.001)
		ppm.record('notify')
		if ppm.profile['spoil']:
			box.spoil()

	while True:
		now = time.monotonic()
		# THE DOORBELL: function 1 ran.
		current = box.u32(DOORBELL)
		if current != doorbell:
			doorbell = current
			since_notify = False
			last_control = box.u64(CONTROL)
			changed_at = None
			if ppm.silent_next is not None:
				ppm.silent_until = now + ppm.silent_next
				ppm.silent_next = None
				ppm.record(f'silent from {NAMES.get(last_control & 0xFF, hex(last_control & 0xFF))} for {ppm.silent_until - now:.0f} s')
			if now < ppm.silent_until:
				held = last_control
			else:
				queued.append((now, last_control))
		# FUNCTION 2 AFTER A NOTIFICATION, before the next command.
		current = box.u32(REFRESHES)
		if current != refreshes:
			refreshes = current
			if since_notify:
				ppm.violate('function 2 evaluated after a notification and before the next command')
		current = box.u32(NOTIFIES)
		if current != notifies:
			notifies = current
			since_notify = True
		# A WRITE OF CONTROL NOT FOLLOWED BY FUNCTION 1 within two seconds - the function runs in the guest's ACPI
		# service, a round trip away from the driver that wrote.
		mailbox = box.u64(UCSI_RANGE + 8)
		if mailbox != mailbox_control:
			mailbox_control = mailbox
			changed_at = now if mailbox != last_control else None
		if changed_at is not None and now - changed_at > 2.0:
			ppm.violate(f'CONTROL {mailbox:#018x} written and not followed by function 1')
			changed_at = None
		# SILENCE OVER: the last doorbell is answered.
		if held is not None and now >= ppm.silent_until:
			queued.append((now, held))
			held = None
		while queued:
			at, value = queued[0]
			cci, message, notify, delay = ppm.answer(value) if not getattr(ppm, '_pending_answer', None) else ppm._pending_answer
			if now < at + delay:
				ppm._pending_answer = (cci, message, notify, delay)
				break
			ppm._pending_answer = None
			queued.pop(0)
			box.answer(ppm.version, cci, message)
			if cci & COMMAND_COMPLETE:
				unacked_since = time.monotonic()
			if cci & ACK_COMPLETE:
				unacked_since = None
			if notify and ppm.notifications:
				notify_now()
		if unacked_since is not None and ppm.outstanding is not None and time.monotonic() - unacked_since > 12:
			ppm.violate(f'the completion of {NAMES.get(ppm.outstanding, hex(ppm.outstanding))} was left unacknowledged')
			unacked_since = None
		# A CONNECTOR CHANGE while idle is notified at once.
		if ppm.pending and ppm.outstanding is None and not queued and ppm.notifications and now >= ppm.silent_until and getattr(ppm, 'announced', None) != tuple(ppm.pending):
			ppm.announced = tuple(ppm.pending)
			box.answer(ppm.version, ppm.indicator(), b'')
			ppm.record(f'change {ppm.pending}')
			notify_now()
		if not ppm.pending:
			ppm.announced = None
		readable, _, _ = select.select([control] + clients, [], [], 0.001)
		for item in readable:
			if item is control:
				client, _ = control.accept()
				clients.append(client)
				continue
			text = item.recv(4096).decode().strip()
			if not text:
				clients.remove(item)
				item.close()
				continue
			item.sendall((command(ppm, text) + '\n').encode())


def command(ppm, text):
	words = text.split()
	try:
		if words[0] == 'attach':
			ppm.attach(int(words[1]), words[2])
		elif words[0] == 'detach':
			ppm.detach(int(words[1]))
		elif words[0] == 'ppm-enter':
			if not ppm.ppm_enter(int(words[1])):
				return 'error no partner with a mode'
		elif words[0] == 'malformed':
			ppm.malformed = words[1]
		elif words[0] == 'silent':
			ppm.silent_until = time.monotonic() + float(words[1])
		elif words[0] == 'silent-next':
			ppm.silent_next = float(words[1])
		elif words[0] == 'status':
			return f'ok outstanding {ppm.outstanding} pending {ppm.pending} notifications {ppm.notifications:#06x} violations {len(ppm.violations)}'
		elif words[0] == 'violations':
			return 'ok ' + ('; '.join(ppm.violations) or 'none')
		else:
			return f'error unknown command {words[0]}'
		ppm.record(f'control {text}')
		return 'ok'
	except (IndexError, ValueError, KeyError) as error:
		return f'error {error}'


def self_test():
	failures = []

	def check(what, got, want):
		if got != want:
			failures.append(f'{what}: {got!r}, not {want!r}')

	ppm = Ppm('v21')
	cci, data, notify, _ = ppm.answer(PPM_RESET)
	check('a reset completes without a notification', (cci, notify), (RESET_COMPLETE, False))
	cci, data, notify, _ = ppm.answer(SET_NOTIFICATION_ENABLE | 0xDBE7 << 16)
	check('notifications on', (cci & COMMAND_COMPLETE != 0, ppm.notifications), (True, 0xDBE7))
	ppm.answer(ACK_CC_CI | 1 << 17)
	cci, data, _, _ = ppm.answer(GET_CAPABILITY)
	check('the capability is sixteen bytes', ((cci >> 8) & 0xFF, data[4], int.from_bytes(data[5:8], 'little')), (16, 2, ALT_MODE_DETAILS | PDO_DETAILS | CABLE_DETAILS))
	ppm.answer(GET_CONNECTOR_STATUS | 1 << 16)
	check('a second command before the acknowledgement is a violation', len(ppm.violations), 1)
	ppm = Ppm('v21')
	ppm.attach(1, 'charger')
	cci, data, _, _ = ppm.answer(GET_CONNECTOR_STATUS | 1 << 16)
	check('a change is indicated beside the completion', (cci >> 1) & 0x7F, 1)
	check('the 2.1 status is nineteen bytes, PD, connected, sinking from a DFP', (len(data), int.from_bytes(data[2:4], 'little') & 0x1F, (int.from_bytes(data[2:4], 'little') >> 13) & 7), (19, 0b01011, 1))
	check('the readings are ready', data[11] >> 1 & 1, 1)
	cci, _, _, _ = ppm.answer(ACK_CC_CI | 1 << 16 | 1 << 17)
	check('the change is acknowledged with its status', (cci & ACK_COMPLETE != 0, ppm.pending), (True, []))
	ppm.answer(ACK_CC_CI | 1 << 16)
	check('a lone connector-change acknowledgement is a violation', any('alone' in v for v in ppm.violations), True)
	ppm = Ppm('v21')
	cci, _, _, _ = ppm.answer(SET_PDR | 1 << 16 | 0x7 << 23)
	check('the 2.1 profile refuses SET_PDR', cci & ERROR != 0, True)
	ppm.answer(ACK_CC_CI | 1 << 17)
	cci, data, _, _ = ppm.answer(GET_ERROR_STATUS)
	check('with its error', struct.unpack('<H', data)[0], POLICY_CONFLICT)
	ppm = Ppm('v12')
	ppm.attach(2, 'dp')
	ppm.answer(GET_CONNECTOR_STATUS | 2 << 16)
	ppm.answer(ACK_CC_CI | 1 << 16 | 1 << 17)
	cci, _, _, _ = ppm.answer(SET_NEW_CAM | 2 << 16 | 1 << 23 | 0 << 24 | 0x0805 << 32)
	check('DisplayPort entered under the override', (cci & COMMAND_COMPLETE != 0, ppm.partners[2].cam, ppm.partners[2].configuration), (True, 0, 0x0805))
	check('the 1.2 status is nine bytes', len(ppm.partners[2].status(0x0120)), 9)
	check('the slow profile delays three commands', PROFILES['slow']['slow'], 3)
	if failures:
		for failure in failures:
			print(f'ucsi-ppm: {failure}', file=sys.stderr)
		return 1
	print('ucsi-ppm: every answer checked')
	return 0


def main():
	parser = argparse.ArgumentParser(description='the Type-C gate\'s UCSI PPM')
	parser.add_argument('--memory')
	parser.add_argument('--gpio-control')
	parser.add_argument('--control')
	parser.add_argument('--log')
	parser.add_argument('--profile', choices=sorted(PROFILES), default='v21')
	parser.add_argument('--ready')
	parser.add_argument('--self-test', action='store_true')
	args = parser.parse_args()
	if args.self_test:
		return self_test()
	return serve(args)


if __name__ == '__main__':
	sys.exit(main())
