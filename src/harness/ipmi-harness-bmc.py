#!/usr/bin/env python3
"""The IPMI gate's BMCs: the SDR and FRU files QEMU's simulated BMC (`ipmi-bmc-sim`) is given, and a BMC of the
harness's own behind QEMU's `ipmi-bmc-extern`.

THE FILES. `--sdr-out` writes the repository the gate's simulated BMCs load (`sdrfile=`): two temperatures in full
records - which QEMU's simulator lists and does not read, since it makes sensors of compact records alone - a power
supply and a chassis intrusion in compact records it does read, an event-only record, and a FRU device locator naming
device 0. `--fru-out` writes the FRU device (`frudatafile=`): a chassis, a board and a product area, every checksum
right. The gate compares what the guest prints against `--describe`, which prints the same values.

THE HARNESS BMC (`--serve`): QEMU's external-BMC protocol over a Unix socket in the run's directory - OpenIPMI's
framing (a message ends at 0xA0, a hardware command at 0xA1, 0xAA escapes, a two's-complement checksum), the message
ID QEMU puts first, and its version and capabilities commands on connect. It answers from a script: device ID (0x20, or
`--device-id`), product ID (0x1234, or `--product-id`) and GUID, an SDR repository with readings the control socket
changes, the SEL (its reservation cancelled by every record added, as the specification requires), FRU, chassis status
and identify, LAN configuration and users, and the watchdog. It KEEPS A RECORD OF EVERY COMMAND IT RECEIVED, in order,
one line each (`--record`), with each control command between them where it arrived, which a gate reads.

ITS HOSTILE MODES, set on the control socket: `oversize` answers the next Get Sensor Reading with 280 bytes - past the
driver's 272-byte bound and inside QEMU's 300-byte KCS buffer, so QEMU passes it through; `silent` answers nothing
(QEMU answers 0xC3 after four seconds); `close` drops the connection (QEMU answers 0xD2 until it reconnects); and
`--malformed` serves an SDR record whose length lies, a FRU device whose board area's checksum is wrong and a SEL record
of an undefined type, beside good ones.

It proves decoding against the specification's layouts and the refusals; it is not a second implementation of IPMI.

  ipmi-harness-bmc.py --sdr-out FILE          the simulated BMCs' repository
  ipmi-harness-bmc.py --fru-out FILE          their FRU device
  ipmi-harness-bmc.py --describe              the values both carry, one per line
  ipmi-harness-bmc.py --serve SOCKET --control SOCKET --record FILE [--guid HEX] [--device-id N] [--product-id N]
                      [--malformed]
  ipmi-harness-bmc.py --self-test             the encodings and the protocol, checked
"""

import argparse
import os
import select
import socket
import struct
import sys
import time

# ------------------------------------------------------------------------------------------ records


def type_length(text):
    """An eight-bit ASCII type/length field."""
    data = text.encode('ascii')
    return bytes([0xC0 | len(data)]) + data


def sdr(record_id, record_type, body):
    """A record: its header - ID, version 0x51, type, the body's length - and the body."""
    return struct.pack('<HBBB', record_id, 0x51, record_type, len(body)) + body


def full(record_id, number, name, sensor_type=0x01, base_unit=1, m=1, b=0, r_exp=0, b_exp=0, thresholds=(70, 85, 95), entity=(0x03, 1)):
    """A full sensor record at the specification's offsets: a linear threshold sensor, its upper non-critical, critical
    and non-recoverable thresholds readable."""
    unc, uc, unr = thresholds
    body = bytearray(42)
    body[0] = 0x20  # owned by the BMC
    body[1] = 0x00
    body[2] = number
    body[3], body[4] = entity
    body[5] = 0x7F  # initialisation: scanning, events, thresholds
    body[6] = 0x68  # capabilities: thresholds readable
    body[7] = sensor_type
    body[8] = 0x01  # threshold
    body[13] = 0b0011_1000  # UNR, UC and UNC readable
    body[14] = 0b0011_1000
    body[15] = 0x00  # units 1: unsigned
    body[16] = base_unit
    body[18] = 0x00  # linear
    body[19] = m & 0xFF
    body[20] = ((m >> 8) & 0x3) << 6
    body[21] = b & 0xFF
    body[22] = ((b >> 8) & 0x3) << 6
    body[24] = ((r_exp & 0xF) << 4) | (b_exp & 0xF)
    body[31] = unr
    body[32] = uc
    body[33] = unc
    body = bytes(body[:42]) + type_length(name)
    return sdr(record_id, 0x01, body)


def compact(record_id, number, name, sensor_type, reading_type=0x6F):
    """A compact sensor record: a discrete sensor QEMU's simulator makes a sensor of."""
    body = bytearray(26)
    body[0] = 0x20
    body[2] = number
    body[3], body[4] = 0x0A, 1
    body[5] = 0x63  # scanning and events on
    body[7] = sensor_type
    body[8] = reading_type
    body[9] = 0x0F
    body = bytes(body) + type_length(name)
    return sdr(record_id, 0x02, body)


def event_only(record_id, number, name, sensor_type):
    body = bytearray(11)
    body[0] = 0x20
    body[2] = number
    body[5] = sensor_type
    body[6] = 0x6F
    body = bytes(body) + type_length(name)
    return sdr(record_id, 0x03, body)


def fru_locator(record_id, device, name):
    body = bytearray(10)
    body[0] = 0x20
    body[1] = device
    body[2] = 0x80  # logical
    body[5] = 0x10
    body[7] = 0x07  # entity: system board
    body[8] = 1
    body = bytes(body) + type_length(name)
    return sdr(record_id, 0x11, body)


# THE GATE'S REPOSITORY, and what the guest prints of it.
SENSORS = [
    ('full', 0x30, 'CPU Temp', 0x01, (70, 85, 95)),
    ('full', 0x31, 'Inlet Temp', 0x01, (40, 45, 50)),
    ('compact', 0x40, 'PS1 Status', 0x08, None),
    ('compact', 0x41, 'Intrusion', 0x05, None),
    ('event-only', 0x50, 'OS Boot', 0x1F, None),
]


def repository(malformed=False):
    records = []
    for at, (kind, number, name, sensor_type, thresholds) in enumerate(SENSORS):
        if kind == 'full':
            records.append(full(at, number, name, sensor_type, thresholds=thresholds))
        elif kind == 'compact':
            records.append(compact(at, number, name, sensor_type))
        else:
            records.append(event_only(at, number, name, sensor_type))
    records.append(fru_locator(len(records), 0, 'Base Board'))
    if malformed:
        # A RECORD WHOSE LENGTH LIES: its header says four bytes more than it carries.
        bad = bytearray(compact(len(records), 0x42, 'Liar', 0x05))
        bad[4] += 4
        records.append(bytes(bad))
    return records


def area(fields, head, end=True):
    """A FRU area: its version, its length in eights, its head bytes, its fields, the end marker, pad, checksum."""
    data = bytearray([0x01, 0x00]) + bytes(head)
    for field in fields:
        data += type_length(field)
    if end:
        data.append(0xC1)
    while (len(data) + 1) % 8:
        data.append(0)
    data[1] = (len(data) + 1) // 8
    data.append((-sum(data)) & 0xFF)
    return bytes(data)


FRU = {
    'chassis': ('LS-CHASSIS-1', 'CH0001'),
    'board': ('LiberSoft', 'Gate Board', 'BRD0001', 'LS-IPMI-1'),
    'product': ('LiberSoft', 'Gate Node', 'LS-NODE-1', '1.0', 'PRD0001', 'ASSET-42'),
}


def fru_image(malformed=False):
    chassis = area(FRU['chassis'], [0x17])
    board = area(FRU['board'], [0x19, 0x10, 0x20, 0x30])
    if malformed:
        board = board[:-1] + bytes([(board[-1] + 1) & 0xFF])
    product = area(FRU['product'], [0x19])
    chassis_at = 1
    board_at = chassis_at + len(chassis) // 8
    product_at = board_at + len(board) // 8
    header = bytearray([0x01, 0x00, chassis_at, board_at, product_at, 0x00, 0x00])
    header.append((-sum(header)) & 0xFF)
    return bytes(header) + chassis + board + product


def describe():
    lines = []
    for kind, number, name, sensor_type, thresholds in SENSORS:
        line = f'sensor {number:#04x} {name} type {sensor_type:#04x} {kind}'
        if thresholds:
            line += ' unc {} uc {} unr {}'.format(*thresholds)
        lines.append(line)
    lines.append('fru board manufacturer ' + FRU['board'][0])
    lines.append('fru board product ' + FRU['board'][1])
    lines.append('fru board serial ' + FRU['board'][2])
    lines.append('fru product name ' + FRU['product'][1])
    lines.append('fru product serial ' + FRU['product'][4])
    lines.append('fru chassis serial ' + FRU['chassis'][1])
    return lines


# ------------------------------------------------------------------------------------------ the protocol

MSG = 0xA0
CMD = 0xA1
ESCAPE = 0xAA
CMD_VERSION = 0xFF
CMD_CAPABILITIES = 0x08
CMD_RESET = 0x04


def escape(data):
    out = bytearray()
    for byte in data:
        if byte in (MSG, CMD, ESCAPE):
            out += bytes([ESCAPE, byte | 0x10])
        else:
            out.append(byte)
    return bytes(out)


def checksum(data):
    return (-sum(data)) & 0xFF


def frame_message(message):
    """A message as the harness sends it: its bytes and checksum, escaped, and the end marker."""
    return escape(message + bytes([checksum(message)])) + bytes([MSG])


class Parser:
    """QEMU's side of the framing, read: messages and hardware commands as they end."""

    def __init__(self):
        self.buf = bytearray()
        self.escaped = False

    def feed(self, data):
        out = []
        for byte in data:
            if byte == MSG:
                out.append(('msg', bytes(self.buf)))
                self.buf.clear()
            elif byte == CMD:
                out.append(('cmd', bytes(self.buf)))
                self.buf.clear()
            elif byte == ESCAPE:
                self.escaped = True
            else:
                if self.escaped:
                    byte &= ~0x10
                    self.escaped = False
                self.buf.append(byte)
        return out


# ------------------------------------------------------------------------------------------ the BMC


class Bmc:
    def __init__(self, guid=None, malformed=False, device_id=0x20, product_id=0x1234):
        self.guid = guid
        self.device_id = device_id
        self.product_id = product_id
        self.malformed = malformed
        self.records = repository(malformed)
        self.readings = {0x30: 40, 0x31: 25, 0x40: 0, 0x41: 0}
        self.sel = []
        self.next_sel = 1
        self.sel_reservation = 1
        self.sdr_reservation = 1
        self.fru = fru_image(malformed)
        self.identify = 0
        self.watchdog = {'use': 0, 'action': 0, 'timeout': 0, 'running': False, 'expired': 0, 'deadline': None}
        self.oversize = False
        self.log = []
        if malformed:
            self.add_sel(record_type=0x10)

    # A RECORD ADDED CANCELS THE SEL'S RESERVATION, as the specification requires.
    def add_sel(self, record_type=0x02, data=None):
        record = bytearray(16)
        struct.pack_into('<H', record, 0, self.next_sel)
        record[2] = record_type
        struct.pack_into('<I', record, 3, int(time.time()) & 0xFFFFFFFF)
        if data:
            record[7:7 + len(data)] = data
        self.sel.append(bytes(record))
        self.next_sel += 1
        self.sel_reservation = (self.sel_reservation % 0xFFFF) + 1
        self.last_addition = int(time.time()) & 0xFFFFFFFF
        return record

    last_addition = 0

    def answer(self, netfn, cmd, data):
        """(completion code, data) for one request."""
        self.log.append(f'{netfn:#04x} {cmd:#04x} {data.hex()}')
        app, storage, sensor, chassis, transport = 0x06, 0x0A, 0x04, 0x00, 0x0C
        if (netfn, cmd) == (app, 0x01):
            return 0, bytes([self.device_id, 0x81, 0x02, 0x05, 0x02, 0xBF, 0x57, 0x01, 0x00]) + struct.pack('<H', self.product_id)
        if (netfn, cmd) == (app, 0x08):
            return (0, self.guid) if self.guid else (0xC1, b'')
        if (netfn, cmd) == (app, 0x22):
            wd = self.watchdog
            if wd['timeout'] == 0 and wd['use'] == 0:
                return 0x80, b''
            wd['running'] = True
            wd['deadline'] = time.monotonic() + wd['timeout'] / 10
            return 0, b''
        if (netfn, cmd) == (app, 0x24):
            wd = self.watchdog
            wd['use'], wd['action'], wd['expired'] = data[0], data[1], wd['expired'] & ~data[3]
            wd['timeout'] = struct.unpack_from('<H', data, 4)[0]
            if not (data[0] & 0x40):
                wd['running'] = False
                wd['deadline'] = None
            elif wd['running']:
                wd['deadline'] = time.monotonic() + wd['timeout'] / 10
            return 0, b''
        if (netfn, cmd) == (app, 0x25):
            wd = self.watchdog
            present = 0
            if wd['running'] and wd['deadline']:
                present = max(0, int((wd['deadline'] - time.monotonic()) * 10))
            return 0, bytes([wd['use'] | (0x40 if wd['running'] else 0), wd['action'], 0, wd['expired']]) + struct.pack('<HH', wd['timeout'], present)
        if (netfn, cmd) == (app, 0x42):
            channel = data[0] & 0x0F
            medium = 0x04 if channel == 1 else 0x00
            return (0, bytes([channel, medium, 0x01, 0x80, 0xF2, 0x1B, 0x00, 0, 0])) if channel in (1, 2) else (0xCC, b'')
        if (netfn, cmd) == (app, 0x44):
            user = data[1] & 0x3F
            names = {1: 0x04, 2: 0x03}
            privilege = names.get(user, 0x0F)
            return 0, bytes([4, 2, 0, 0x10 | privilege if privilege != 0x0F else privilege])
        if (netfn, cmd) == (app, 0x46):
            names = {1: 'admin', 2: 'operator'}
            return 0, names.get(data[0] & 0x3F, '').encode().ljust(16, b'\0')
        if (netfn, cmd) == (transport, 0x02):
            parameter = data[1]
            values = {3: bytes([10, 0, 2, 15]), 4: bytes([1]), 5: bytes([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]), 6: bytes([255, 255, 255, 0]), 12: bytes([10, 0, 2, 2]), 20: struct.pack('<H', 0x8000 | 42)}
            return (0, bytes([0x11]) + values[parameter]) if parameter in values else (0x80, b'')
        if (netfn, cmd) == (chassis, 0x01):
            return 0, bytes([0x21, 0x00, 0x40 | (0x10 if self.identify else 0)])
        if (netfn, cmd) == (chassis, 0x04):
            self.identify = data[0] if data else 15
            return 0, b''
        if (netfn, cmd) == (sensor, 0x2D):
            number = data[0]
            if number not in self.readings:
                return 0xCB, b''
            if self.oversize:
                self.oversize = False
                return 0, bytes([0xAB] * 277)
            return 0, bytes([self.readings[number], 0xC0, 0xC0])
        if (netfn, cmd) == (sensor, 0x02):
            # A PLATFORM EVENT MESSAGE from system software names the generator's software ID alone: the record's
            # second generator byte - the channel and LUN it arrived on, 0 for the system interface - is the BMC's.
            if len(data) != 8:
                return 0xC7, b''
            self.add_sel(0x02, bytes(data[:1]) + b'\x00' + bytes(data[1:8]))
            return 0, b''
        if (netfn, cmd) == (storage, 0x22):
            return 0, struct.pack('<H', self.sdr_reservation)
        if (netfn, cmd) == (storage, 0x23):
            reservation, record_id, offset, count = struct.unpack_from('<HHBB', data)
            if record_id >= len(self.records):
                return 0xCB, b''
            record = self.records[record_id]
            nxt = record_id + 1 if record_id + 1 < len(self.records) else 0xFFFF
            count = len(record) if count == 0xFF else count
            return 0, struct.pack('<H', nxt) + record[offset:offset + count]
        if (netfn, cmd) == (storage, 0x10):
            return 0, struct.pack('<HB', len(self.fru), 0)
        if (netfn, cmd) == (storage, 0x11):
            offset, count = struct.unpack_from('<HB', data, 1)
            chunk = self.fru[offset:offset + count]
            return 0, bytes([len(chunk)]) + chunk
        if (netfn, cmd) == (storage, 0x40):
            return 0, bytes([0x51]) + struct.pack('<HH', len(self.sel), 1024 - 16 * len(self.sel)) + struct.pack('<II', self.last_addition, 0) + bytes([0x02])
        if (netfn, cmd) == (storage, 0x42):
            return 0, struct.pack('<H', self.sel_reservation)
        if (netfn, cmd) == (storage, 0x43):
            record_id = struct.unpack_from('<H', data, 2)[0]
            ids = [struct.unpack_from('<H', record)[0] for record in self.sel]
            if not ids:
                return 0xCB, b''
            at = 0 if record_id == 0 else (ids.index(record_id) if record_id in ids else None)
            if at is None:
                return 0xCB, b''
            nxt = ids[at + 1] if at + 1 < len(ids) else 0xFFFF
            return 0, struct.pack('<H', nxt) + self.sel[at]
        if (netfn, cmd) == (storage, 0x47):
            reservation = struct.unpack_from('<H', data)[0]
            if reservation != self.sel_reservation:
                return 0xC5, b''
            if data[5] == 0xAA:
                self.sel.clear()
            return 0, bytes([1])
        return 0xC1, b''

    def tick(self):
        """The watchdog: an expiry sets its flag and resets the machine. Returns the hardware command to send."""
        wd = self.watchdog
        if wd['running'] and wd['deadline'] and time.monotonic() >= wd['deadline']:
            wd['running'] = False
            wd['deadline'] = None
            wd['expired'] |= 1 << (wd['use'] & 7)
            return CMD_RESET if (wd['action'] & 7) == 1 else None
        return None


def serve(args):
    bmc = Bmc(bytes.fromhex(args.guid) if args.guid else None, args.malformed, int(args.device_id, 0), int(args.product_id, 0))
    for path in (args.serve, args.control):
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass
    server = socket.socket(socket.AF_UNIX)
    server.bind(args.serve)
    server.listen(1)
    control = socket.socket(socket.AF_UNIX)
    control.bind(args.control)
    control.listen(4)
    record = open(args.record, 'a', buffering=1)
    if args.ready:
        open(args.ready, 'w').close()
    conn = None
    parser = Parser()
    mode = 'normal'
    controls = []
    while True:
        watched = [server, control] + controls + ([conn] if conn else [])
        ready, _, _ = select.select(watched, [], [], 0.1)
        for item in ready:
            if item is server:
                if conn:
                    conn.close()
                conn, _ = server.accept()
                parser = Parser()
                record.write('connected\n')
            elif item is control:
                client, _ = control.accept()
                controls.append(client)
            elif item in controls:
                line = item.recv(256).decode().strip()
                if not line:
                    controls.remove(item)
                    item.close()
                    continue
                words = line.split()
                reply = 'ok'
                # IN THE RECORD, in order with the commands: a gate reads where the host changed something.
                record.write(f'control {line}\n')
                if words[0] == 'reading':
                    bmc.readings[int(words[1], 0)] = int(words[2], 0)
                elif words[0] == 'sel-add':
                    bmc.add_sel(0x02, bytes([0x20, 0x00, 0x04, 0x01, 0x30, 0x01, 0x07, 0xFF, 0xFF]))
                elif words[0] == 'mode':
                    mode = words[1]
                    if mode == 'oversize':
                        bmc.oversize = True
                        mode = 'normal'
                    if mode == 'close' and conn:
                        conn.close()
                        conn = None
                        record.write('closed\n')
                elif words[0] == 'status':
                    reply = f'ok mode {mode} sel {len(bmc.sel)} identify {bmc.identify} watchdog {bmc.watchdog}'
                else:
                    reply = 'error unknown command'
                item.sendall((reply + '\n').encode())
            elif item is conn:
                data = conn.recv(4096)
                if not data:
                    conn.close()
                    conn = None
                    continue
                for kind, body in parser.feed(data):
                    if kind == 'cmd' or len(body) < 4:
                        continue
                    if (sum(body) & 0xFF) != 0:
                        record.write('checksum refused\n')
                        continue
                    msg_id, netfn_lun, cmd, request = body[0], body[1], body[2], body[3:-1]
                    netfn = netfn_lun >> 2
                    cc, answer = bmc.answer(netfn, cmd, request)
                    record.write(bmc.log[-1] + f' -> {cc:#04x}\n')
                    if mode in ('silent', 'close'):
                        continue
                    conn.sendall(frame_message(bytes([msg_id, (netfn + 1) << 2 | (netfn_lun & 3), cmd, cc]) + answer))
        hardware = bmc.tick()
        if hardware is not None and conn:
            record.write('watchdog expired - reset\n')
            conn.sendall(escape(bytes([hardware])) + bytes([CMD]))


# ------------------------------------------------------------------------------------------ self-test


def self_test():
    failures = []

    def check(what, got, want):
        if got != want:
            failures.append(f'{what}: {got!r} != {want!r}')

    records = repository()
    check('a full record is 48 bytes and its name', len(records[0]), 5 + 42 + 1 + len('CPU Temp'))
    check('its record type', records[0][3], 0x01)
    check('its length names its body', records[0][4], len(records[0]) - 5)
    check('the upper critical threshold at byte 38 (1-based)', records[0][37], 85)
    check('a compact record type', records[2][3], 0x02)
    check('the FRU locator names device 0 as logical', (records[-1][3], records[-1][6], records[-1][7] & 0x80), (0x11, 0, 0x80))
    image = fru_image()
    check('the common header sums to zero', sum(image[:8]) & 0xFF, 0)
    for offset in image[2:5]:
        start = offset * 8
        length = image[start + 1] * 8
        check(f'the area at {offset} sums to zero', sum(image[start:start + length]) & 0xFF, 0)
    bad = fru_image(malformed=True)
    check('the malformed board area does not', sum(bad[bad[3] * 8:bad[3] * 8 + bad[bad[3] * 8 + 1] * 8]) & 0xFF != 0, True)
    framed = frame_message(bytes([1, 0x1C, 0x01, 0x00, 0xA0]))
    check('an end marker in a message is escaped', b'\xaa\xb0' in framed, True)
    parsed = Parser().feed(framed)
    check('and parsed back', parsed[0][1][:5], bytes([1, 0x1C, 0x01, 0x00, 0xA0]))
    check('with a zero-sum checksum', sum(parsed[0][1]) & 0xFF, 0)
    bmc = Bmc(guid=bytes(range(16)))
    check('Get Device ID answers', bmc.answer(0x06, 0x01, b'')[0], 0)
    bmc.add_sel()
    reservation = bmc.answer(0x0A, 0x42, b'')[1]
    bmc.add_sel()
    check('a record added cancels the reservation', bmc.answer(0x0A, 0x47, reservation + b'CLR\xaa')[0], 0xC5)
    check('the log is untouched', len(bmc.sel), 2)
    reservation = bmc.answer(0x0A, 0x42, b'')[1]
    check('a clear under the live reservation', bmc.answer(0x0A, 0x47, reservation + b'CLR\xaa'), (0, b'\x01'))
    check('erases', len(bmc.sel), 0)
    check('a platform event message is taken', bmc.answer(0x04, 0x02, bytes([0x41, 0x04, 0x1F, 0x00, 0x6F, 0x06, 0xFF, 0xFF]))[0], 0)
    check('and logged with the channel byte after the software ID', bmc.sel[-1][7:16], bytes([0x41, 0x00, 0x04, 0x1F, 0x00, 0x6F, 0x06, 0xFF, 0xFF]))
    check('a platform event message of another length is refused', bmc.answer(0x04, 0x02, bytes(9))[0], 0xC7)
    bmc.oversize = True
    check('the oversize answer is past the bound', len(bmc.answer(0x04, 0x2D, b'\x30')[1]) + 3 > 272, True)
    if failures:
        for failure in failures:
            print(f'ipmi-harness-bmc: {failure}', file=sys.stderr)
        return 1
    print('ipmi-harness-bmc: every encoding checked')
    return 0


def main():
    parser = argparse.ArgumentParser(description='the IPMI gate\'s BMCs')
    parser.add_argument('--sdr-out')
    parser.add_argument('--fru-out')
    parser.add_argument('--describe', action='store_true')
    parser.add_argument('--serve')
    parser.add_argument('--control')
    parser.add_argument('--record')
    parser.add_argument('--ready')
    parser.add_argument('--guid')
    parser.add_argument('--device-id', default='0x20')
    parser.add_argument('--product-id', default='0x1234')
    parser.add_argument('--malformed', action='store_true')
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    if args.sdr_out:
        with open(args.sdr_out, 'wb') as out:
            out.write(b''.join(repository()))
    if args.fru_out:
        with open(args.fru_out, 'wb') as out:
            out.write(fru_image())
    if args.describe:
        print('\n'.join(describe()))
    if args.serve:
        if not (args.control and args.record):
            parser.error('--serve needs --control and --record')
        serve(args)
    return 0


if __name__ == '__main__':
    sys.exit(main())
