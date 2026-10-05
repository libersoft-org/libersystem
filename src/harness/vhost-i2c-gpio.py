#!/usr/bin/env python3
"""The device side of a virtio-i2c controller and a virtio-gpio controller, over vhost-user.

QEMU has no HID-over-I2C device and none of the I2C controllers laptops carry, but it does have
`vhost-user-i2c-pci` and `vhost-user-gpio-pci`: virtio devices whose DEVICE SIDE is an external process
speaking vhost-user. This is that process. Whatever answers here IS the device as far as the guest can tell,
so a register device at an I2C address and the GPIO line it raises are models in this file, with nothing
loaded into the host's kernel.

ONE PROCESS, BOTH SOCKETS, AND A CONTROL SOCKET through which a gate or another fixture raises and lowers a
line, turns the register device's PEC on, or picks a script:

    raise LINE | lower LINE | level LINE 0|1 | pec on|off | status
    hid script|hold|lose|status NAME | hid malformed NAME on|off      (with --hid)
    tcpc attach PROFILE [flipped] | tcpc detach | tcpc script NAME | tcpc caps [less] | tcpc hard-reset
    tcpc vbus MV | tcpc send MESSAGE | tcpc malformed KIND | tcpc negotiate N | tcpc timing | tcpc mark
    tcpc status | tcpc requests | tcpc answers | tcpc hard-resets | tcpc responses | tcpc violations  (with --tcpc)

and each answers one line, `ok ...` or `error ...`. The same commands reach it IN-BAND, through the register
device's control mailbox (register 0xE0 at 0x50), for a test in the guest, which cannot reach a host socket.

THE IOMMU. Each device is attached with `iommu_platform=on` whenever the machine has a virtio-iommu, so the
backend offers VIRTIO_F_ACCESS_PLATFORM with the reply-ack and backend-request protocol features (QEMU refuses
an IOMMU device whose backend lacks them), translates every ring and buffer address through the IOTLB
entries QEMU sends - each update and invalidation acknowledged - and on a miss asks on the backend channel
and serves the main channel until the update arrives. Without the IOMMU the addresses are the guest's own and
the memory table translates them.

`--tcpc` adds A TYPE-C PORT CONTROLLER at 0x52, its alert on line 5, and the Power Delivery source on its cable
(`tcpc_partner.py`): the TCPCI gate's fixture, whose partner the `tcpc` commands drive. `--stretch F` multiplies the
source's two timers that wait on the sink, for the emulated ports.

`--self-test` runs the host suite of the parts a host can check without QEMU: the memory table's
translation, the IOTLB's updates, misses and invalidations, and the device models.
"""

import argparse
import array
import mmap
import os
import select
import socket
import struct
import sys
import time
import unittest

import tcpc_partner

# One line per request and per control-plane message, on stderr, with `--trace`: the device side's half of any
# bus failure, which the guest cannot see.
TRACE = False


def trace(message):
    if TRACE:
        print(f"{time.monotonic():.6f} {message}", file=sys.stderr, flush=True)


# ------------------------------------------------------------------------------------------ vhost-user

GET_FEATURES = 1
SET_FEATURES = 2
SET_OWNER = 3
RESET_OWNER = 4
SET_MEM_TABLE = 5
SET_VRING_NUM = 8
SET_VRING_ADDR = 9
SET_VRING_BASE = 10
GET_VRING_BASE = 11
SET_VRING_KICK = 12
SET_VRING_CALL = 13
SET_VRING_ERR = 14
GET_PROTOCOL_FEATURES = 15
SET_PROTOCOL_FEATURES = 16
GET_QUEUE_NUM = 17
SET_VRING_ENABLE = 18
SET_BACKEND_REQ_FD = 21
IOTLB_MSG = 22
GET_CONFIG = 24
SET_CONFIG = 25

BACKEND_IOTLB_MSG = 1

FLAG_VERSION = 0x1
FLAG_REPLY = 0x4
FLAG_NEED_REPLY = 0x8

F_INDIRECT_DESC = 1 << 28
F_VERSION_1 = 1 << 32
F_ACCESS_PLATFORM = 1 << 33
F_PROTOCOL_FEATURES = 1 << 30

PROTOCOL_MQ = 1 << 0
PROTOCOL_REPLY_ACK = 1 << 3
PROTOCOL_BACKEND_REQ = 1 << 5
PROTOCOL_CONFIG = 1 << 9

IOTLB_MISS = 1
IOTLB_UPDATE = 2
IOTLB_INVALIDATE = 3
IOTLB_ACCESS_FAIL = 4

PERM_RO = 1
PERM_WO = 2
PERM_RW = 3

VRING_NOFD = 0x100
VRING_INDEX = 0xFF

DESC_NEXT = 1
DESC_WRITE = 2
DESC_INDIRECT = 4

HEADER = struct.Struct("<III")


class Miss(Exception):
    """An address the IOTLB does not translate yet."""

    def __init__(self, iova, write):
        super().__init__(f"IOTLB miss at {iova:#x}")
        self.iova = iova
        self.write = write


class Memory:
    """The guest's RAM as QEMU shares it: regions of (guest physical, size, QEMU's virtual address) and the
    file each is mapped from."""

    def __init__(self):
        self.regions = []

    def replace(self, regions):
        """A new table: `regions` is (guest_phys, size, user_addr, view, backing) each - `view` a memoryview of
        `size` bytes, `backing` the mapping it is a view of, or None."""
        self.close()
        self.regions = regions

    def close(self):
        for _, _, _, view, backing in self.regions:
            view.release()
            if backing is not None:
                backing.close()
        self.regions = []

    def _find(self, addr, length, key):
        for region in self.regions:
            base = region[key]
            size = region[1]
            if base <= addr and addr + length <= base + size:
                return region[3][addr - base:addr - base + length]
        raise ValueError(f"no region holds {addr:#x}+{length}")

    def gpa(self, addr, length):
        """`length` bytes at guest physical address `addr`."""
        return self._find(addr, length, 0)

    def uva(self, addr, length):
        """`length` bytes at QEMU's virtual address `addr`."""
        return self._find(addr, length, 2)


class Iotlb:
    """The translations QEMU sent: IOVA ranges to QEMU virtual addresses, with permissions."""

    def __init__(self):
        self.entries = []

    def update(self, iova, size, uaddr, perm):
        self.invalidate(iova, size)
        self.entries.append((iova, size, uaddr, perm))

    def invalidate(self, iova, size):
        kept = []
        for entry in self.entries:
            start, length = entry[0], entry[1]
            if start + length <= iova or iova + size <= start:
                kept.append(entry)
        self.entries = kept

    def translate(self, iova, length, write):
        """QEMU's virtual address for `length` bytes at `iova`, or `Miss`. A range crossing two entries is
        refused rather than stitched: nothing this backend reads spans more than one mapping."""
        need = PERM_WO if write else PERM_RO
        for start, size, uaddr, perm in self.entries:
            if start <= iova < start + size:
                if iova + length > start + size or perm & need != need:
                    raise Miss(iova + (start + size - iova if iova + length > start + size else 0), write)
                return uaddr + (iova - start)
        raise Miss(iova, write)


class Space:
    """Where the rings' and buffers' addresses point: guest physical addresses, or IOVAs through the IOTLB
    and then QEMU's virtual addresses. The rings' own addresses are always QEMU virtual addresses without an
    IOMMU and IOVAs with one."""

    def __init__(self, memory, iotlb, translated, ask):
        self.memory = memory
        self.iotlb = iotlb
        self.translated = translated
        self.ask = ask

    def view(self, addr, length, write=False):
        if length == 0:
            return memoryview(b"")
        if not self.translated:
            return self.memory.gpa(addr, length)
        while True:
            try:
                return self.memory.uva(self.iotlb.translate(addr, length, write), length)
            except Miss as miss:
                self.ask(miss)

    def ring(self, addr, length, write=False):
        if not self.translated:
            return self.memory.uva(addr, length)
        return self.view(addr, length, write)


class Vring:
    """A split virtqueue, from the device's side."""

    def __init__(self):
        self.num = 0
        self.desc = self.avail = self.used = 0
        self.last_avail = 0
        self.kick = -1
        self.call = -1
        self.enabled = False
        self.ready = False

    def chains(self, space):
        """Every descriptor chain the driver made available since the last call: (head, [(addr, len,
        writable)])."""
        avail_idx = load_u16(space.ring(self.avail + 2, 2))
        while self.last_avail != avail_idx:
            slot = self.last_avail % self.num
            head = load_u16(space.ring(self.avail + 4 + 2 * slot, 2))
            parts = []
            index = head
            for _ in range(self.num):
                addr, length, flags, nxt = struct.unpack_from("<QIHH", space.ring(self.desc + 16 * index, 16))
                if flags & DESC_INDIRECT:
                    # AN INDIRECT TABLE: the chain lives in the table the descriptor points at, which is in
                    # the guest's memory like any buffer.
                    table = bytes(space.view(addr, length))
                    at = 0
                    for _ in range(length // 16):
                        taddr, tlength, tflags, tnext = struct.unpack_from("<QIHH", table, 16 * at)
                        parts.append((taddr, tlength, bool(tflags & DESC_WRITE)))
                        if not tflags & DESC_NEXT:
                            break
                        at = tnext
                    break
                parts.append((addr, length, bool(flags & DESC_WRITE)))
                if not flags & DESC_NEXT:
                    break
                index = nxt
            self.last_avail = (self.last_avail + 1) & 0xFFFF
            yield head, parts

    def put_used(self, space, head, written):
        used_idx = load_u16(space.ring(self.used + 2, 2))
        slot = used_idx % self.num
        element = space.ring(self.used + 4 + 8 * slot, 8, True).cast("I")
        element[0] = head
        element[1] = written
        # THE INDEX LAST, AND IN ONE STORE: the driver polls it.
        store_u16(space.ring(self.used + 2, 2, True), (used_idx + 1) & 0xFFFF)

    def signal(self):
        if self.call >= 0:
            os.write(self.call, struct.pack("<Q", 1))


# A RING INDEX IS READ AND WRITTEN IN ONE ACCESS. The driver polls the used index while this process writes it,
# and writes the available index while this process reads it; `struct.pack_into` ZERO-FILLS its destination
# before it writes the value a byte at a time, so a driver polling the used index saw it fall to zero between
# two completions and - rightly - refused the completion as more than it had asked for. A memoryview cast to
# `H` moves the two bytes with one 16-bit access (both indexes are 2-byte aligned).
def load_u16(view):
    return view.cast("H")[0]


def store_u16(view, value):
    view.cast("H")[0] = value


def recv_message(sock):
    """One vhost-user message: (request, flags, payload, fds), or None at the end of the connection."""
    fds = array.array("i")
    data, ancillary, _, _ = sock.recvmsg(HEADER.size, socket.CMSG_LEN(8 * 4))
    if not data:
        return None
    for level, kind, payload in ancillary:
        if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
            fds.frombytes(payload[:len(payload) - len(payload) % fds.itemsize])
    request, flags, size = HEADER.unpack(data)
    body = b""
    while len(body) < size:
        chunk, ancillary, _, _ = sock.recvmsg(size - len(body), socket.CMSG_LEN(8 * 4))
        if not chunk:
            return None
        for level, kind, payload in ancillary:
            if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                fds.frombytes(payload[:len(payload) - len(payload) % fds.itemsize])
        body += chunk
    return request, flags, body, list(fds)


class Device:
    """One vhost-user device: the protocol, the memory, the rings, and a model that serves the queues."""

    def __init__(self, name, sock, model):
        self.name = name
        self.sock = sock
        self.model = model
        self.memory = Memory()
        self.iotlb = Iotlb()
        self.features = 0
        self.protocol = 0
        self.backend = None
        self.rings = [Vring() for _ in range(model.queues)]
        self.space = Space(self.memory, self.iotlb, False, self.ask)
        # Rings a message asked to have drained, drained once its reply is out.
        self.pending = []

    def offered(self):
        return F_VERSION_1 | F_ACCESS_PLATFORM | F_PROTOCOL_FEATURES | F_INDIRECT_DESC | self.model.features

    def reply(self, request, payload):
        self.sock.sendall(HEADER.pack(request, FLAG_VERSION | FLAG_REPLY, len(payload)) + payload)

    def ask(self, miss):
        """AN IOTLB MISS: ask on the backend channel, then serve the main channels until the translation is
        there - the update QEMU sends in answer arrives on this device's main channel.

        EVERY DEVICE'S MAIN CHANNEL, not only this one's: QEMU's one thread may be blocked waiting for another
        device's reply, and it reads no backend channel until it has that reply - so serving only this socket
        would wait for an update QEMU cannot send. A message served here never drains a ring (`pending`), which
        is also why a ring is never drained inside the handling of a message QEMU waits on."""
        if self.backend is None:
            raise RuntimeError(f"{self.name}: an IOTLB miss at {miss.iova:#x} and no backend channel to ask on")
        perm = PERM_WO if miss.write else PERM_RO
        body = struct.pack("<QQQBB6x", miss.iova, 1, 0, perm, IOTLB_MISS)
        trace(f"{self.name}: IOTLB miss at {miss.iova:#x}")
        self.backend.sendall(HEADER.pack(BACKEND_IOTLB_MSG, FLAG_VERSION, len(body)) + body)
        for _ in range(1000):
            peers = [device for device in DEVICES.values() if device.sock is not None] or [self]
            ready, _, _ = select.select([device.sock for device in peers], [], [], 5.0)
            if not ready:
                break
            for device in peers:
                if device.sock in ready and not device.serve_one():
                    raise EOFError(f"{device.name}: the front end went away during an IOTLB miss")
            try:
                self.iotlb.translate(miss.iova, 1, miss.write)
                return
            except Miss:
                continue
        raise RuntimeError(f"{self.name}: no IOTLB update arrived for {miss.iova:#x}")

    def run_pending(self):
        """The drains a message asked for, now that its reply is sent."""
        while self.pending:
            self.drain(self.pending.pop(0))

    def serve_one(self):
        """Handle one message on the main channel; False when the connection ended."""
        message = recv_message(self.sock)
        if message is None:
            return False
        request, flags, body, fds = message
        trace(f"{self.name}: message {request} ({len(body)} bytes, {len(fds)} fd(s))")
        ack = bool(flags & FLAG_NEED_REPLY) and self.protocol & PROTOCOL_REPLY_ACK
        answered = self.handle(request, body, fds)
        if ack and not answered:
            self.reply(request, struct.pack("<Q", 0))
        return True

    def handle(self, request, body, fds):
        """Returns True when the handler already replied."""
        if request == GET_FEATURES:
            self.reply(request, struct.pack("<Q", self.offered()))
            return True
        if request == SET_FEATURES:
            self.features = struct.unpack_from("<Q", body)[0]
            self.space.translated = bool(self.features & F_ACCESS_PLATFORM)
            return False
        if request == GET_PROTOCOL_FEATURES:
            offered = PROTOCOL_REPLY_ACK | PROTOCOL_BACKEND_REQ | PROTOCOL_MQ | (PROTOCOL_CONFIG if self.model.config() is not None else 0)
            self.reply(request, struct.pack("<Q", offered))
            return True
        if request == SET_PROTOCOL_FEATURES:
            self.protocol = struct.unpack_from("<Q", body)[0]
            return False
        if request == GET_QUEUE_NUM:
            self.reply(request, struct.pack("<Q", len(self.rings)))
            return True
        if request in (SET_OWNER, RESET_OWNER):
            return False
        if request == SET_BACKEND_REQ_FD:
            self.backend = socket.socket(fileno=fds[0])
            return False
        if request == SET_MEM_TABLE:
            count = struct.unpack_from("<I", body)[0]
            regions = []
            for at in range(count):
                guest, size, user, offset = struct.unpack_from("<QQQQ", body, 8 + 32 * at)
                backing = mmap.mmap(fds[at], size + offset, mmap.MAP_SHARED, mmap.PROT_READ | mmap.PROT_WRITE)
                os.close(fds[at])
                regions.append((guest, size, user, memoryview(backing)[offset:offset + size], backing))
            self.memory.replace(regions)
            return False
        if request == IOTLB_MSG:
            iova, size, uaddr, perm, kind = struct.unpack_from("<QQQBB", body)
            trace(f"{self.name}: IOTLB {'update' if kind == IOTLB_UPDATE else 'invalidate' if kind == IOTLB_INVALIDATE else kind} {iova:#x}+{size:#x} -> {uaddr:#x} perm {perm}")
            if kind == IOTLB_UPDATE:
                self.iotlb.update(iova, size, uaddr, perm)
            elif kind == IOTLB_INVALIDATE:
                self.iotlb.invalidate(iova, size)
            return False
        if request == SET_VRING_NUM:
            index, num = struct.unpack_from("<II", body)
            self.rings[index].num = num
            return False
        if request == SET_VRING_BASE:
            index, num = struct.unpack_from("<II", body)
            self.rings[index].last_avail = num
            return False
        if request == GET_VRING_BASE:
            index = struct.unpack_from("<I", body)[0]
            ring = self.rings[index]
            ring.ready = False
            # A STOPPED DEVICE KEEPS NO TRANSLATION. QEMU sends no invalidation for the mappings a driver's domain
            # loses once the device's vhost side is stopped - the reset that ends a binding stops it first - so
            # the next binding, in a new domain at the same IOVAs, would read the last one's freed pages. The
            # IOTLB is a cache: emptied here, every access after the next start misses and QEMU answers it.
            self.iotlb.entries = []
            trace(f"{self.name}: ring {index} stopped, IOTLB emptied")
            # AND WHAT THE MODEL HELD FROM THAT RING IS GONE WITH IT: a buffer the driver queued before the stop is
            # not the driver's any more - a reset, or a guest suspended to RAM, which QEMU stops every vhost device
            # for - and completing it later wrote through an IOTLB that no longer answers.
            stopped = getattr(self.model, "ring_stopped", None)
            if stopped is not None:
                stopped(index)
            self.reply(request, struct.pack("<II", index, ring.last_avail))
            return True
        if request == SET_VRING_ADDR:
            index, _flags, desc, used, avail, _log = struct.unpack_from("<IIQQQQ", body)
            trace(f"{self.name}: ring {index} at desc {desc:#x} avail {avail:#x} used {used:#x}")
            ring = self.rings[index]
            ring.desc, ring.used, ring.avail = desc, used, avail
            ring.ready = True
            return False
        if request in (SET_VRING_KICK, SET_VRING_CALL, SET_VRING_ERR):
            value = struct.unpack_from("<Q", body)[0]
            index = value & VRING_INDEX
            fd = -1 if value & VRING_NOFD else fds[0]
            ring = self.rings[index]
            if request == SET_VRING_KICK:
                if ring.kick >= 0:
                    os.close(ring.kick)
                ring.kick = fd
                # Without the protocol-features feature a ring starts the moment it has a kick.
                if not self.features & F_PROTOCOL_FEATURES:
                    ring.enabled = True
            elif request == SET_VRING_CALL:
                if ring.call >= 0:
                    os.close(ring.call)
                ring.call = fd
            elif fd >= 0:
                os.close(fd)
            return False
        if request == SET_VRING_ENABLE:
            index, enable = struct.unpack_from("<II", body)
            self.rings[index].enabled = bool(enable)
            # DRAINED AFTER THE REPLY, never here: a buffer's address may miss the IOTLB, and QEMU answers a
            # miss only once it has the reply to this message.
            if enable and index not in self.pending:
                self.pending.append(index)
            return False
        if request == GET_CONFIG:
            offset, size, flags = struct.unpack_from("<III", body)
            config = self.model.config() or b""
            chunk = config[offset:offset + size].ljust(size, b"\0")
            self.reply(request, struct.pack("<III", offset, size, flags) + chunk)
            return True
        if request == SET_CONFIG:
            return False
        for fd in fds:
            os.close(fd)
        return False

    def kicks(self):
        return [(ring.kick, index) for index, ring in enumerate(self.rings) if ring.kick >= 0]

    def drain(self, index):
        """Serve every chain the driver made available on ring `index`."""
        ring = self.rings[index]
        if not (ring.enabled and ring.ready and ring.num):
            return
        signalled = False
        for head, parts in ring.chains(self.space):
            written = self.model.serve(index, head, parts, self.space)
            trace(f"{self.name}: ring {index} chain {head} of {len(parts)} part(s) -> {'held' if written is None else f'{written} byte(s) written'}")
            if written is None:
                # Held: the model completes this chain later (a GPIO event buffer).
                continue
            ring.put_used(self.space, head, written)
            signalled = True
        if signalled:
            ring.signal()

    def complete(self, index, head, written):
        """Complete a chain the model held."""
        ring = self.rings[index]
        ring.put_used(self.space, head, written)
        ring.signal()


# ------------------------------------------------------------------------------------------ the I2C bus

I2C_FLAG_FAIL_NEXT = 1 << 0
I2C_FLAG_READ = 1 << 1
I2C_OK = 0
I2C_ERR = 1


def crc8(data, crc=0):
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = ((crc << 1) ^ 0x07) & 0xFF if crc & 0x80 else (crc << 1) & 0xFF
    return crc


class RegisterDevice:
    """A register device at an I2C address: 256 byte registers and a pointer. A write sets the pointer from
    its first byte and stores the rest from there; a read returns from the pointer on.

    Register 0xF0 is a SMBUS BLOCK: read, it answers the count the device holds and then that many bytes,
    which is the block read with the device's count a controller that fixes each read's length cannot take.

    Register 0xFE is the PEC MODE, so a test in the guest can drive it without the control socket: 0 no PEC,
    1 PEC (every write's last byte is checked, and a wrong one is refused; every read answers one more byte,
    the CRC-8 of the transaction as it crossed the bus), 2 the same with every read's PEC WRONG. A write to it
    is never checked, since it is what turns the checking on or off.

    Register 0xE0 is the CONTROL MAILBOX: the control socket's own commands, carried in-band for a test in the
    guest, which has no way to reach a socket on the host. A write of `0xE0` and then a command's text runs
    that command through the same `command` the socket does - so a line the guest raises this way is raised
    by the same code a gate's is - and a read from 0xE0 answers the reply's text. Never checked for PEC,
    like the mode."""

    BLOCK = 0xF0
    MODE = 0xFE
    CONTROL = 0xE0

    def __init__(self, address, control=None):
        self.address = address
        self.registers = bytearray(range(256))
        self.pointer = 0
        self.pec = 0
        self.block = b"LIBER"
        self.last_write = b""
        self.control = control
        self.reply = b""

    def write(self, data, combined=False):
        """`combined` is the write half of a write-then-read: SMBus puts that transaction's PEC at the end of
        its read, so the write carries none to check."""
        if not data:
            return True
        if data[0] == self.MODE and len(data) >= 2:
            self.pec = data[1]
            self.pointer = self.MODE
            self.last_write = bytes(data[:1])
            return True
        if data[0] == self.CONTROL:
            text = bytes(data[1:]).decode(errors="replace").strip()
            self.reply = (self.control(text) if self.control else "error no control").encode()
            self.pointer = self.CONTROL
            self.last_write = bytes(data[:1])
            return True
        if self.pec and not combined:
            if len(data) < 2 or data[-1] != crc8(bytes([self.address << 1]) + bytes(data[:-1])):
                return False
            data = data[:-1]
        self.pointer = data[0]
        for at, value in enumerate(data[1:]):
            self.registers[(self.pointer + at) & 0xFF] = value
        self.last_write = bytes(data)
        return True

    def read(self, length, after_write):
        wanted = length - 1 if self.pec else length
        if self.pointer == self.CONTROL:
            return self.reply.ljust(length, b"\0")[:length]
        if self.pointer == self.BLOCK:
            body = (bytes([len(self.block)]) + self.block).ljust(wanted, b"\xff")[:wanted]
        else:
            body = bytes(self.registers[(self.pointer + at) & 0xFF] for at in range(wanted))
        if self.pec:
            prefix = bytes([self.address << 1]) + self.last_write if after_write else b""
            crc = crc8(prefix + bytes([self.address << 1 | 1]) + body)
            body += bytes([crc ^ 0xFF if self.pec == 2 else crc])
        return body


# ------------------------------------------------------------------------------------------ HID over I2C

HID_RESET = 0x01
HID_GET_REPORT = 0x02
HID_SET_REPORT = 0x03
HID_SET_POWER = 0x08
HID_FEATURE = 3

# A PRECISION TOUCHPAD'S REPORT DESCRIPTOR: a Mouse collection (report 1: two buttons, relative X and Y), a Touch Pad
# collection (report 2: one finger's tip, contact id and absolute X and Y, and the contact count) and a Device
# Configuration collection (feature report 3: Input Mode). It powers on in mouse mode and reports through report 1
# alone; Input Mode 3 moves it to report 2 and 0 back.
TOUCHPAD_REPORT_DESCRIPTOR = bytes([
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01,  # Generic Desktop, Mouse, Application
    0x85, 0x01,  # Report ID 1
    0x09, 0x01, 0xA1, 0x00,  # Pointer, Physical
    0x05, 0x09, 0x19, 0x01, 0x29, 0x02, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x02, 0x81, 0x02,  # buttons 1-2
    0x95, 0x06, 0x81, 0x03,  # padding
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06,  # X, Y relative
    0xC0, 0xC0,
    0x05, 0x0D, 0x09, 0x05, 0xA1, 0x01,  # Digitizers, Touch Pad, Application
    0x85, 0x02,  # Report ID 2
    0x09, 0x22, 0xA1, 0x02,  # Finger, Logical
    0x09, 0x42, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02,  # Tip Switch
    0x95, 0x07, 0x81, 0x03,  # padding
    0x09, 0x51, 0x25, 0x0F, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02,  # Contact Identifier
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xFF, 0x0F, 0x75, 0x10, 0x95, 0x02, 0x81, 0x02,  # X, Y 0..4095
    0xC0,
    0x05, 0x0D, 0x09, 0x54, 0x15, 0x00, 0x25, 0x05, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02,  # Contact Count
    0xC0,
    0x05, 0x0D, 0x09, 0x0E, 0xA1, 0x01,  # Digitizers, Device Configuration, Application
    0x85, 0x03,  # Report ID 3
    0x09, 0x22, 0xA1, 0x02,  # Finger, Logical
    0x09, 0x52, 0x15, 0x00, 0x25, 0x0A, 0x75, 0x08, 0x95, 0x01, 0xB1, 0x02,  # Input Mode, Feature
    0xC0, 0xC0,
])

# A TOUCHSCREEN'S: a Touch Screen collection (report 1: two fingers, each a tip, a contact id and absolute X and Y, and
# the contact count), reporting its contacts in the mode it powers on in.
_FINGER = [
    0x05, 0x0D, 0x09, 0x22, 0xA1, 0x02,  # Digitizers, Finger, Logical
    0x09, 0x42, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02,  # Tip Switch
    0x95, 0x07, 0x81, 0x03,  # padding
    0x09, 0x51, 0x25, 0x0F, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02,  # Contact Identifier
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xFF, 0x0F, 0x75, 0x10, 0x95, 0x02, 0x81, 0x02,  # X, Y 0..4095
    0xC0,
]
TOUCHSCREEN_REPORT_DESCRIPTOR = bytes([0x05, 0x0D, 0x09, 0x04, 0xA1, 0x01, 0x85, 0x01] + _FINGER + _FINGER + [0x05, 0x0D, 0x09, 0x54, 0x15, 0x00, 0x25, 0x02, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0xC0])


def finger(tip, contact, x, y):
    return struct.pack("<BBHH", 1 if tip else 0, contact, x, y)


class HidDevice:
    """A HID-OVER-I2C DEVICE at an I2C address, speaking the register protocol: its HID descriptor at the register the
    firmware names, the report descriptor, the command and data registers, and the input register a plain read
    empties. Its interrupt is a GPIO line, SIGNALLED BY LEVEL AND ACTIVE LOW: asserted (0) while an input report - or
    the reset indication - waits, released (1) once the last is read.

    It honours RESET (the zero-length reset indication, then mouse mode, powered on), SET_POWER (asleep, it reports
    nothing until it is set on) and SET_REPORT of its Input Mode feature where it has one. `lose` is a POWER LOSS - as a
    device across an S3 - after which it reports nothing until a RESET; `hold` asserts its line with no report behind
    it, until a RESET; `malformed` corrupts its HID descriptor's version."""

    def __init__(self, name, address, registers, report_descriptor, line, script, set_line):
        self.name = name
        self.address = address
        self.descriptor_register, self.report_descriptor_register, self.input_register, self.output_register, self.command_register, self.data_register = registers
        self.report_descriptor = report_descriptor
        self.line = line
        self.script = script
        self.set_line = set_line
        self.max_input = 2 + 1 + 16
        self.powered = True
        self.lost = False
        self.held = False
        self.malformed = False
        self.mode = 0
        self.queue = []
        self.resets = 0
        self.pointer = None
        self.feature = None

    def descriptor(self):
        version = 0x0200 if self.malformed else 0x0100
        return struct.pack("<HHHHHHHHHHHHHI", 30, version, len(self.report_descriptor), self.report_descriptor_register, self.input_register, self.max_input, self.output_register, 0, self.command_register, self.data_register, 0x1D6B, 0x4C00 + self.address, 1, 0)

    def signal(self):
        """The line follows what waits: asserted (0) while a report, the reset indication or a hold does."""
        self.set_line(self.line, 0 if (self.queue or self.held) else 1)

    def deliverable(self):
        return self.powered and not self.lost

    def run_script(self):
        if not self.deliverable():
            return f"ok {self.name} reports nothing - {'its power was lost' if self.lost else 'it is asleep'}"
        for report in self.script(self):
            self.queue.append(struct.pack("<H", 2 + len(report)) + report)
        self.signal()
        return f"ok {self.name} queued {len(self.queue)} report(s) in mode {self.mode}"

    def reset(self):
        self.resets += 1
        self.powered = True
        self.lost = False
        self.held = False
        self.mode = 0
        self.queue = [b"\0\0"]
        self.signal()

    def lose_power(self):
        self.lost = True
        self.queue = []
        self.held = False
        self.signal()

    def write(self, data, combined=False):
        if len(data) < 2:
            return True
        register = data[0] | data[1] << 8
        if combined:
            # THE WRITE HALF OF A REGISTER READ: the register, or a GET_REPORT command and the data register after it.
            self.pointer = register
            if register == self.command_register and len(data) >= 6 and data[3] & 0x0F == HID_GET_REPORT:
                self.pointer = ("get", (data[2] >> 4) & 0x3, data[2] & 0x0F)
            return True
        if register != self.command_register or len(data) < 4:
            return True
        opcode = data[3] & 0x0F
        if opcode == HID_RESET:
            self.reset()
        elif opcode == HID_SET_POWER:
            self.powered = (data[2] & 0x03) == 0
            if self.powered:
                self.signal()
        elif opcode == HID_SET_REPORT:
            kind, report = (data[2] >> 4) & 0x3, data[2] & 0x0F
            at = 4
            if report == 0x0F:
                report = data[at]
                at += 1
            body = data[at + 2 + 2:]
            if kind == HID_FEATURE and report == 3 and self.name == "touchpad" and len(body) >= 2:
                self.mode = body[1]
        return True

    def read(self, length, after_write):
        if after_write and self.pointer == self.descriptor_register:
            return self.descriptor().ljust(length, b"\0")[:length]
        if after_write and self.pointer == self.report_descriptor_register:
            return self.report_descriptor.ljust(length, b"\0")[:length]
        if after_write and isinstance(self.pointer, tuple):
            _, kind, report = self.pointer
            answer = struct.pack("<HBB", 4, report, self.mode) if (kind == HID_FEATURE and report == 3 and self.name == "touchpad") else b"\0\0"
            return answer.ljust(length, b"\0")[:length]
        # THE INPUT REGISTER: the oldest report waiting, or a zero length when none does.
        report = self.queue.pop(0) if self.queue else b"\0\0"
        self.signal()
        return report.ljust(length, b"\0")[:length]


# THE TOUCHPAD'S MOVES: sixteen reports of (+127, +64) counts, which a relative pointer folds into the normalised
# grid as a move of a few text cells - enough for a client to see the cursor go somewhere.
TOUCHPAD_MOVES = 16


def touchpad_script(device):
    """Moves and a click, through the ONE collection the mode selects: the mouse report in mouse mode, the finger
    report in touch pad mode - so a driver that switched the mode sees no moves."""
    if device.mode == 3:
        return [bytes([2]) + finger(True, 0, 1000 + 40 * step, 1000 + 20 * step) + bytes([1]) for step in range(3)] + [bytes([2]) + finger(False, 0, 1080, 1040) + bytes([0])]
    return [bytes([1, 0, 127, 64])] * TOUCHPAD_MOVES + [bytes([1, 1, 0, 0]), bytes([1, 0, 0, 0])]


def touchscreen_script(_device):
    """A two-finger contact, and the lift."""
    return [bytes([1]) + finger(True, 0, 1024, 1024) + finger(True, 1, 3072, 3072) + bytes([2]), bytes([1]) + finger(False, 0, 1024, 1024) + finger(False, 1, 3072, 3072) + bytes([2])]


# THE TWO MODELS: (name, address, registers - descriptor, report descriptor, input, output, command, data - report
# descriptor, GPIO line, script).
HID_MODELS = [
    ("touchpad", 0x2C, (0x20, 0x21, 0x22, 0x23, 0x24, 0x25), TOUCHPAD_REPORT_DESCRIPTOR, 0, touchpad_script),
    ("touchscreen", 0x10, (0x01, 0x02, 0x03, 0x04, 0x05, 0x06), TOUCHSCREEN_REPORT_DESCRIPTOR, 1, touchscreen_script),
]


def hid_devices(set_line):
    return [HidDevice(name, address, registers, descriptor, line, script, set_line) for name, address, registers, descriptor, line, script in HID_MODELS]


class I2cModel:
    """Virtio-i2c's one request queue, over the models at each address."""

    queues = 1
    features = 1 << 0  # zero-length requests

    def __init__(self, devices):
        self.devices = {device.address: device for device in devices}
        self.failed_next = False

    def config(self):
        return None

    def serve(self, _index, _head, parts, space):
        """One request: out header, an optional buffer, the in header. Answers the bytes written."""
        header = bytes(space.view(parts[0][0], 8))
        address = struct.unpack_from("<H", header)[0] >> 1
        flags = struct.unpack_from("<I", header, 4)[0]
        status_addr, status_len, _ = parts[-1]
        buffer = parts[1] if len(parts) == 3 else None
        device = self.devices.get(address)
        ok = device is not None and not self.failed_next
        written = 0
        if ok and buffer is not None:
            addr, length, writable = buffer
            if flags & I2C_FLAG_READ and writable:
                data = device.read(length, after_write=self._previous_was_write)
                space.view(addr, length, True)[:] = data
                written = length
            elif not writable:
                ok = device.write(bytes(space.view(addr, length)), combined=bool(flags & I2C_FLAG_FAIL_NEXT))
        if ok and buffer is None and device is not None:
            ok = True
        # A READ AFTER A WRITE IN THE SAME TRANSFER - a repeated start, which FAIL_NEXT on the write says - is
        # what the PEC of a write-then-read covers both halves of; a read on its own covers itself alone.
        self._previous_was_write = bool(ok and buffer is not None and not flags & I2C_FLAG_READ and flags & I2C_FLAG_FAIL_NEXT)
        # FAIL_NEXT: a failed request fails the one after it; a request that does not carry the flag ends the
        # transfer.
        self.failed_next = (not ok) and bool(flags & I2C_FLAG_FAIL_NEXT)
        space.view(status_addr, 1, True)[0] = I2C_OK if ok else I2C_ERR
        trace(f"i2c: {address:#04x} flags {flags:#x} {'no buffer' if buffer is None else f'{buffer[1]} byte(s)'} -> {'ok' if ok else 'failed'}")
        return written + status_len

    _previous_was_write = False


# ------------------------------------------------------------------------------------------ the GPIO lines

GPIO_GET_NAMES = 1
GPIO_GET_DIRECTION = 2
GPIO_SET_DIRECTION = 3
GPIO_GET_VALUE = 4
GPIO_SET_VALUE = 5
GPIO_SET_IRQ_TYPE = 6
GPIO_OK = 0
GPIO_ERR = 1
IRQ_NONE = 0
IRQ_RISING = 1
IRQ_FALLING = 2
IRQ_BOTH = 3
IRQ_HIGH = 4
IRQ_LOW = 8
EVENT_VALID = 1
EVENT_INVALID = 0


class GpioModel:
    """Virtio-gpio's request queue and event queue, over a set of named input lines whose levels the control
    socket sets. An armed line with its buffer queued completes the buffer when its trigger is met; a level
    trigger met while the buffer comes back completes it again at once."""

    queues = 2
    features = 1 << 0  # the event queue

    def __init__(self, names):
        self.names = names
        self.levels = [0] * len(names)
        self.directions = [0] * len(names)
        self.triggers = [IRQ_NONE] * len(names)
        self.pending = {}
        self.device = None

    def config(self):
        names = b"".join(name.encode() + b"\0" for name in self.names)
        return struct.pack("<HxxI", len(self.names), len(names))

    def names_blob(self):
        return b"".join(name.encode() + b"\0" for name in self.names)

    def serve(self, index, head, parts, space):
        if index == 1:
            # AN EVENT BUFFER: completed at once - invalid for a line that is not armed, valid for a level
            # line already asserted - or held until its line fires.
            line = struct.unpack_from("<H", bytes(space.view(parts[0][0], 2)))[0]
            status_addr = parts[-1][0]
            if line >= len(self.names) or self.triggers[line] == IRQ_NONE:
                space.view(status_addr, 1, True)[0] = EVENT_INVALID
                return 1
            if self.level_met(line):
                space.view(status_addr, 1, True)[0] = EVENT_VALID
                return 1
            self.pending[line] = (head, status_addr)
            return None
        kind, line, value = struct.unpack_from("<HHI", bytes(space.view(parts[0][0], 8)))
        answer_addr, answer_len, _ = parts[-1]
        status, reply = GPIO_OK, b""
        if kind == GPIO_GET_NAMES:
            reply = self.names_blob()
        elif line >= len(self.names):
            status = GPIO_ERR
        elif kind == GPIO_GET_DIRECTION:
            reply = bytes([self.directions[line]])
        elif kind == GPIO_SET_DIRECTION:
            self.directions[line] = value
        elif kind == GPIO_GET_VALUE:
            reply = bytes([self.levels[line]])
        elif kind == GPIO_SET_VALUE:
            status = GPIO_ERR  # input lines only
        elif kind == GPIO_SET_IRQ_TYPE:
            self.triggers[line] = value
            if line in self.pending and (value == IRQ_NONE or self.level_met(line)):
                self.fire(line, space, EVENT_VALID if value != IRQ_NONE else EVENT_INVALID)
        else:
            status = GPIO_ERR
        answer = bytes([status]) + reply
        space.view(answer_addr, answer_len, True)[:] = answer.ljust(answer_len, b"\0")[:answer_len]
        trace(f"gpio: request {kind} line {line} value {value:#x} -> status {status}, {answer_len} byte(s)")
        return answer_len

    def ring_stopped(self, index):
        """THE EVENT QUEUE STOPPED: every buffer held for a line is dropped, and every line disarmed - the driver that
        starts the queue again arms what it wants. The lines keep their levels: a level line still asserted fires at
        the next buffer its driver queues, and an edge that comes while the queue is stopped is not delivered, as a
        controller that is powered down delivers none - the control socket's `raise` answers it was not fired."""
        if index == 1:
            if self.pending:
                trace(f"gpio: the event queue stopped - {len(self.pending)} held buffer(s) dropped")
            self.pending.clear()
            self.triggers = [IRQ_NONE] * len(self.names)

    def level_met(self, line):
        trigger = self.triggers[line]
        return (trigger == IRQ_HIGH and self.levels[line]) or (trigger == IRQ_LOW and not self.levels[line])

    def fire(self, line, space, status=EVENT_VALID):
        """Complete the buffer held for `line` with `status`."""
        head, status_addr = self.pending.pop(line)
        space.view(status_addr, 1, True)[0] = status
        if self.device is not None:
            self.device.complete(1, head, 1)

    def set_level(self, line, level, space):
        """The control socket moved a line. Answers whether an event completed."""
        old, self.levels[line] = self.levels[line], level
        trigger = self.triggers[line]
        edge = (old != level) and ((trigger == IRQ_RISING and level) or (trigger == IRQ_FALLING and not level) or trigger == IRQ_BOTH)
        if line in self.pending and (edge or self.level_met(line)):
            self.fire(line, space)
            return True
        return False


# ------------------------------------------------------------------------------------------ the loop

# Every connected device, by name - which an IOTLB miss serves while it waits (see `Device.ask`).
DEVICES = {}


def listen(path):
    try:
        os.unlink(path)
    except FileNotFoundError:
        pass
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(path)
    server.listen(1)
    return server


def serve(args):
    devices = DEVICES
    register = RegisterDevice(0x50, control=lambda text: command(text, register, gpio_model, devices, hid))
    # LINE 8 IS THE BRIGHTNESS GATE'S: `acpi-fixture.py --brightness` puts its `_E08` there, so the panel's and the light
    # sensor's notifications have a line no other gate's table uses.
    gpio_model = GpioModel(["hid-touchpad", "hid-touchscreen", "acpi-aei", "spare-3", "spare-4", "tcpc-alert", "spare-6", "spare-7", "acpi-brightness"])

    def set_line(line, level):
        space = devices["gpio"].space if "gpio" in devices else None
        if space is not None:
            gpio_model.set_level(line, level, space)
        else:
            gpio_model.levels[line] = level

    # `--hid`: THE TWO HID MODELS beside the register device, their lines released - high, since they are active low.
    hid = hid_devices(set_line) if args.hid else []
    for device in hid:
        gpio_model.levels[device.line] = 1
    # `--tcpc`: THE PORT CONTROLLER AND ITS PARTNER, the alert released.
    tcpc, scheduler = None, None
    if args.tcpc:
        log = open(args.tcpc_log, "a", buffering=1) if args.tcpc_log else sys.stderr
        started = time.monotonic()

        def tcpc_log(text):
            log.write(f"{time.monotonic() - started:10.4f} {text}\n")

        tcpc, scheduler = tcpc_partner.build(TCPC_ADDRESS, TCPC_LINE, set_line, tcpc_log, stretch=args.stretch)
        gpio_model.levels[TCPC_LINE] = 1
    TCPC_MODEL[0] = tcpc
    i2c_model = I2cModel([register] + hid + ([tcpc] if tcpc else []))
    listeners = {"i2c": listen(args.i2c), "gpio": listen(args.gpio)}
    control = listen(args.control)
    if args.ready:
        with open(args.ready, "w") as ready:
            ready.write("ready\n")
    control_clients = []
    while True:
        watched = list(listeners.values()) + [control] + control_clients
        kicks = {}
        for name, device in devices.items():
            watched.append(device.sock)
            for fd, index in device.kicks():
                kicks[fd] = (name, index)
                watched.append(fd)
        # THE PARTNER'S TIMERS bound the wait, and run once the wait is over.
        due = scheduler.next_due() if scheduler else None
        timeout = None if due is None else max(0.0, due - time.monotonic())
        ready, _, _ = select.select(watched, [], [], timeout)
        if scheduler:
            scheduler.run_due()
        for item in ready:
            # STILL READY? An IOTLB miss served earlier in this pass may have read a socket's message already, and
            # a read of it now would block until QEMU sent another.
            if not select.select([item], [], [], 0)[0]:
                continue
            if item in listeners.values():
                name = next(key for key, value in listeners.items() if value is item)
                conn, _ = item.accept()
                model = i2c_model if name == "i2c" else gpio_model
                devices[name] = Device(name, conn, model)
                if name == "gpio":
                    gpio_model.device = devices[name]
            elif item is control:
                conn, _ = control.accept()
                control_clients.append(conn)
            elif item in control_clients:
                line = item.recv(256)
                if not line:
                    control_clients.remove(item)
                    item.close()
                    continue
                item.sendall((command(line.decode().strip(), register, gpio_model, devices, hid) + "\n").encode())
            elif isinstance(item, int):
                name, index = kicks[item]
                os.read(item, 8)
                trace(f"{name}: kick on ring {index}")
                devices[name].drain(index)
            else:
                device = next((d for d in devices.values() if d.sock is item), None)
                if device is None:
                    continue
                if not device.serve_one():
                    device.memory.close()
                    del devices[device.name]
                    continue
            for device in list(devices.values()):
                device.run_pending()
        if scheduler:
            scheduler.run_due()


# THE PORT CONTROLLER, when `--tcpc` made one: its address, its alert line, and the model the control commands reach.
TCPC_ADDRESS = 0x52
TCPC_LINE = 5
TCPC_MODEL = [None]


def command(text, register, gpio, devices, hid=()):
    words = text.split()
    try:
        if not words:
            return "error empty"
        if words[0] == "tcpc":
            if TCPC_MODEL[0] is None:
                return "error no port controller - the backend runs without --tcpc"
            if len(words) < 2:
                return "error tcpc takes a command"
            return TCPC_MODEL[0].partner.command(words[1:])
        if words[0] == "hid":
            device = next((device for device in hid if device.name == words[2]), None)
            if device is None:
                return f"error no HID model {words[2]}"
            if words[1] == "script":
                return device.run_script()
            if words[1] == "hold":
                device.held = True
                device.signal()
                return f"ok {device.name} line {device.line} held asserted with no report"
            if words[1] == "lose":
                device.lose_power()
                return f"ok {device.name} lost its power"
            if words[1] == "malformed":
                device.malformed = words[3] == "on"
                return f"ok {device.name} descriptor {'malformed' if device.malformed else 'well-formed'}"
            if words[1] == "status":
                return f"ok {device.name} power {'on' if device.powered else 'sleep'} lost {int(device.lost)} mode {device.mode} resets {device.resets} queued {len(device.queue)} held {int(device.held)}"
            return f"error unknown hid command {words[1]}"
        if words[0] in ("raise", "lower", "level"):
            line = int(words[1])
            level = 1 if words[0] == "raise" else 0 if words[0] == "lower" else int(words[2])
            space = devices["gpio"].space if "gpio" in devices else None
            fired = gpio.set_level(line, level, space) if space is not None else False
            return f"ok line {line} level {level}{' fired' if fired else ''}"
        if words[0] == "pec":
            register.pec = {"off": 0, "on": 1, "wrong": 2}[words[1]]
            return f"ok pec {words[1]}"
        if words[0] == "status":
            return f"ok devices {','.join(sorted(devices))} levels {''.join(str(level) for level in gpio.levels)}"
        return f"error unknown command {words[0]}"
    except (IndexError, ValueError, KeyError) as error:
        return f"error {error}"


# ------------------------------------------------------------------------------------------ the host suite


class FakeSpace:
    """A space over one bytearray, addressed from zero - enough for the models."""

    def __init__(self, size=4096):
        self.data = bytearray(size)

    def view(self, addr, length, write=False):
        return memoryview(self.data)[addr:addr + length]


class SelfTest(unittest.TestCase):
    def test_the_memory_table_translates_guest_and_qemu_addresses(self):
        memory = Memory()
        backing = bytearray(8192)
        memory.replace([(0x100000, 4096, 0x7F0000000000, memoryview(backing)[0:4096], None), (0x200000, 4096, 0x7F0000100000, memoryview(backing)[4096:8192], None)])
        memory.gpa(0x100010, 4)[:] = b"ABCD"
        self.assertEqual(bytes(backing[16:20]), b"ABCD")
        self.assertEqual(bytes(memory.uva(0x7F0000100000, 2)), bytes(backing[4096:4098]))
        with self.assertRaises(ValueError):
            memory.gpa(0x100FFE, 4)  # runs off the end of a region

    def test_the_iotlb_translates_misses_and_forgets_what_is_invalidated(self):
        iotlb = Iotlb()
        with self.assertRaises(Miss):
            iotlb.translate(0x1000, 4, False)
        iotlb.update(0x1000, 0x1000, 0x7F0000000000, PERM_RO)
        self.assertEqual(iotlb.translate(0x1010, 4, False), 0x7F0000000010)
        with self.assertRaises(Miss):
            iotlb.translate(0x1010, 4, True)  # read-only, asked for a write
        with self.assertRaises(Miss):
            iotlb.translate(0x1FFE, 4, False)  # runs past the entry
        iotlb.update(0x1000, 0x1000, 0x7F0000200000, PERM_RW)
        self.assertEqual(iotlb.translate(0x1000, 4, True), 0x7F0000200000, "an update replaces what it overlaps")
        iotlb.invalidate(0x1800, 0x10)
        with self.assertRaises(Miss):
            iotlb.translate(0x1000, 4, False)

    def test_a_translated_space_asks_and_retries_until_the_update_arrives(self):
        memory = Memory()
        backing = bytearray(4096)
        memory.replace([(0, 4096, 0x5000_0000, memoryview(backing), None)])
        iotlb = Iotlb()
        asked = []

        def ask(miss):
            asked.append(miss.iova)
            iotlb.update(0x9000, 0x1000, 0x5000_0000, PERM_RW)

        space = Space(memory, iotlb, True, ask)
        space.view(0x9004, 2, True)[:] = b"hi"
        self.assertEqual(asked, [0x9004])
        self.assertEqual(bytes(backing[4:6]), b"hi")

    def test_a_ring_reads_a_chain_through_its_indirect_table(self):
        space = FakeSpace(8192)
        ring = Vring()
        ring.num, ring.desc, ring.avail, ring.used = 4, 0, 64, 128
        # Ring slot 0 points at a table at 1024 holding two descriptors.
        struct.pack_into("<QIHH", space.data, 0, 1024, 32, DESC_INDIRECT, 0)
        struct.pack_into("<QIHH", space.data, 1024, 0x5000, 8, DESC_NEXT, 1)
        struct.pack_into("<QIHH", space.data, 1040, 0x6000, 1, DESC_WRITE, 0)
        struct.pack_into("<HHH", space.data, 64, 0, 1, 0)

        class Plain:
            translated = False

            def ring(self, addr, length, write=False):
                return space.view(addr, length)

            def view(self, addr, length, write=False):
                return space.view(addr, length)

        chains = list(ring.chains(Plain()))
        self.assertEqual(chains, [(0, [(0x5000, 8, False), (0x6000, 1, True)])])

    def test_the_register_device_reads_writes_its_block_and_its_pec(self):
        device = RegisterDevice(0x50)
        device.write(bytes([0x10, 0xAA, 0xBB]))
        self.assertEqual(device.read(2, after_write=False), b"\xaa\xbb")
        device.write(bytes([RegisterDevice.BLOCK]))
        self.assertEqual(device.read(6, after_write=True), b"\x05LIBER", "the block answers its own count first")
        device.write(bytes([RegisterDevice.MODE, 1]))
        self.assertEqual(device.pec, 1, "the mode register turns PEC on from the guest")
        self.assertFalse(device.write(bytes([0x10, 0x00])), "a write whose PEC is wrong is refused")
        self.assertTrue(device.write(bytes([0x10, crc8(bytes([0xA0, 0x10]))])))
        answer = device.read(2, after_write=True)
        self.assertEqual(answer[0], 0xAA)
        self.assertEqual(answer[1], crc8(bytes([0xA0, 0x10, 0xA1, 0xAA])), "the PEC covers both address bytes")
        self.assertTrue(device.write(bytes([0x10]), combined=True), "a write-then-read's write half carries no PEC to check")
        self.assertEqual(device.read(2, after_write=True)[1], crc8(bytes([0xA0, 0x10, 0xA1, 0xAA])), "and the read's PEC covers both halves")
        device.write(bytes([RegisterDevice.MODE, 2]))
        device.write(bytes([0x10, crc8(bytes([0xA0, 0x10]))]))
        self.assertNotEqual(device.read(2, after_write=True)[1], crc8(bytes([0xA0, 0x10, 0xA1, 0xAA])), "mode 2 answers a wrong PEC")

    def test_the_i2c_model_fails_an_empty_address_and_the_request_after_a_failed_one(self):
        space = FakeSpace()
        model = I2cModel([RegisterDevice(0x50)])
        # A request to 0x51: header at 0, status at 100.
        space.data[0:8] = struct.pack("<HHI", 0x51 << 1, 0, I2C_FLAG_FAIL_NEXT)
        model.serve(0, 0, [(0, 8, False), (100, 1, True)], space)
        self.assertEqual(space.data[100], I2C_ERR)
        # The next request, to the present device, fails with it: FAIL_NEXT.
        space.data[0:8] = struct.pack("<HHI", 0x50 << 1, 0, I2C_FLAG_READ)
        model.serve(0, 0, [(0, 8, False), (200, 2, True), (100, 1, True)], space)
        self.assertEqual(space.data[100], I2C_ERR)
        # And one after that succeeds.
        model.serve(0, 0, [(0, 8, False), (200, 2, True), (100, 1, True)], space)
        self.assertEqual(space.data[100], I2C_OK)

    def test_the_gpio_model_holds_an_event_buffer_until_its_line_fires(self):
        model = GpioModel(["a", "b"])
        completed = []

        class Recorder:
            def complete(self, index, head, written):
                completed.append((index, head))

        model.device = Recorder()
        space = FakeSpace()
        space.data[0:8] = struct.pack("<HHI", GPIO_SET_IRQ_TYPE, 1, IRQ_RISING)
        model.serve(0, 0, [(0, 8, False), (64, 1, True)], space)
        space.data[16:18] = struct.pack("<H", 1)
        self.assertIsNone(model.serve(1, 7, [(16, 2, False), (80, 1, True)], space), "held until the line fires")
        self.assertTrue(model.set_level(1, 1, space))
        self.assertEqual(completed, [(1, 7)])
        self.assertEqual(space.data[80], EVENT_VALID)
        self.assertFalse(model.set_level(1, 0, space), "no buffer queued: nothing to complete")

    def test_a_stopped_event_queue_drops_its_held_buffers_and_a_raise_then_fires_nothing(self):
        # A GUEST SUSPENDED TO RAM: QEMU stops the device, and a line raised then must not complete a buffer through an
        # IOTLB that no longer answers - which crashed the backend, and QEMU with it.
        model = GpioModel(["a", "b"])
        completed = []

        class Recorder:
            def complete(self, index, head, written):
                completed.append((index, head))

        device = Device("gpio", None, model)
        device.reply = lambda request, payload: None
        model.device = Recorder()
        space = FakeSpace()
        space.data[0:8] = struct.pack("<HHI", GPIO_SET_IRQ_TYPE, 1, IRQ_RISING)
        model.serve(0, 0, [(0, 8, False), (64, 1, True)], space)
        space.data[16:18] = struct.pack("<H", 1)
        self.assertIsNone(model.serve(1, 7, [(16, 2, False), (80, 1, True)], space))
        self.assertTrue(device.handle(GET_VRING_BASE, struct.pack("<I", 1), []))
        self.assertFalse(model.set_level(1, 1, space), "nothing held: the raise fires nothing")
        self.assertEqual(completed, [])
        self.assertEqual(model.triggers[1], IRQ_NONE, "disarmed until a driver arms it again")
        self.assertEqual(model.levels[1], 1, "the level is kept")

    def test_a_stopped_ring_empties_the_iotlb(self):
        device = Device("i2c", None, I2cModel([]))
        sent = []
        device.reply = lambda request, payload: sent.append(request)
        device.iotlb.update(0x2000, 0x1000, 0x7F0000000000, PERM_RW)
        self.assertTrue(device.handle(GET_VRING_BASE, struct.pack("<I", 0), []))
        self.assertEqual(sent, [GET_VRING_BASE])
        with self.assertRaises(Miss):
            device.iotlb.translate(0x2000, 8, False)

    def test_a_ring_enabled_by_a_message_is_drained_after_the_reply_and_not_inside_it(self):
        # QEMU answers an IOTLB miss only once it has the reply to the message it is waiting on, so a drain -
        # which may miss - inside the handling of SET_VRING_ENABLE would wait for an update that cannot come.
        device = Device("gpio", None, GpioModel(["a"]))
        drained = []
        device.drain = drained.append
        self.assertFalse(device.handle(SET_VRING_ENABLE, struct.pack("<II", 1, 1), []))
        self.assertTrue(device.rings[1].enabled)
        self.assertEqual(drained, [], "nothing is drained while the message is being handled")
        self.assertEqual(device.pending, [1])
        device.run_pending()
        self.assertEqual(drained, [1], "and the ring is drained once the reply is out")
        self.assertEqual(device.pending, [])

    def test_a_hid_model_speaks_the_register_protocol_and_its_line_follows_what_waits(self):
        lines = {}
        pad, screen = hid_devices(lambda line, level: lines.__setitem__(line, level))
        # THE DESCRIPTOR at the register the firmware names, read with a repeated start.
        self.assertTrue(pad.write(bytes([0x20, 0x00]), combined=True))
        descriptor = pad.read(30, after_write=True)
        length, version, report_len, report_reg, input_reg, max_input = struct.unpack_from("<HHHHHH", descriptor)
        self.assertEqual((length, version, report_len, report_reg, input_reg), (30, 0x0100, len(TOUCHPAD_REPORT_DESCRIPTOR), 0x21, 0x22))
        pad.write(bytes([0x21, 0x00]), combined=True)
        self.assertEqual(pad.read(report_len, after_write=True), TOUCHPAD_REPORT_DESCRIPTOR)
        # RESET: the zero-length indication waits on an asserted line, and reading it releases the line.
        pad.write(bytes([0x24, 0x00, 0x00, HID_RESET]))
        self.assertEqual((lines[0], pad.resets), (0, 1))
        self.assertEqual(pad.read(max_input, after_write=False)[:2], b"\0\0")
        self.assertEqual(lines[0], 1)
        # THE SCRIPT IN MOUSE MODE: four mouse reports, the line asserted until the last is read.
        self.assertTrue(pad.run_script().startswith(f"ok touchpad queued {TOUCHPAD_MOVES + 2}"))
        reports = []
        while lines[0] == 0:
            reports.append(pad.read(max_input, after_write=False))
        self.assertEqual([r[:6] for r in reports], [bytes([6, 0, 1, 0, 127, 64])] * TOUCHPAD_MOVES + [bytes([6, 0, 1, 1, 0, 0]), bytes([6, 0, 1, 0, 0, 0])])
        # INPUT MODE 3 through SET_REPORT: the same script now goes out through the touch pad collection alone.
        pad.write(bytes([0x24, 0x00, 0x33, HID_SET_REPORT, 0x25, 0x00, 0x04, 0x00, 0x03, 0x03]))
        self.assertEqual(pad.mode, 3)
        self.assertTrue(pad.run_script().startswith("ok touchpad queued 4 report(s) in mode 3"))
        self.assertEqual({pad.read(max_input, after_write=False)[2] for _ in range(4)}, {2})
        # ASLEEP IT REPORTS NOTHING; after a power loss nothing until a RESET.
        pad.write(bytes([0x24, 0x00, 0x01, HID_SET_POWER]))
        self.assertIn("asleep", pad.run_script())
        pad.write(bytes([0x24, 0x00, 0x00, HID_SET_POWER]))
        pad.lose_power()
        self.assertIn("power was lost", pad.run_script())
        pad.write(bytes([0x24, 0x00, 0x00, HID_SET_POWER]))
        self.assertIn("power was lost", pad.run_script(), "SET_POWER on does not bring back a device that lost its power")
        pad.write(bytes([0x24, 0x00, 0x00, HID_RESET]))
        pad.read(max_input, after_write=False)
        self.assertEqual(pad.mode, 0, "a RESET is mouse mode again")
        # A HELD LINE stays asserted with nothing behind it, until a RESET.
        pad.held = True
        pad.signal()
        self.assertEqual(pad.read(max_input, after_write=False)[:2], b"\0\0")
        self.assertEqual(lines[0], 0)
        pad.write(bytes([0x24, 0x00, 0x00, HID_RESET]))
        pad.read(max_input, after_write=False)
        self.assertEqual(lines[0], 1)
        # A MALFORMED DESCRIPTOR names a version this protocol does not define.
        screen.malformed = True
        screen.write(bytes([0x01, 0x00]), combined=True)
        self.assertEqual(struct.unpack_from("<H", screen.read(30, after_write=True), 2)[0], 0x0200)
        # THE TOUCHSCREEN'S SCRIPT: a two-finger contact and the lift, on its own line.
        screen.run_script()
        self.assertEqual(lines[1], 0)
        down = screen.read(19, after_write=False)
        self.assertEqual((down[2], down[3], down[9], down[15]), (1, 1, 1, 2))

    def test_the_control_mailbox_runs_the_control_sockets_own_commands(self):
        gpio = GpioModel(["a", "b", "c"])
        completed = []

        class Recorder:
            space = FakeSpace()

            def complete(self, index, head, written):
                completed.append((index, head))

        devices = {"gpio": Recorder()}
        gpio.device = devices["gpio"]
        register = RegisterDevice(0x50, control=lambda text: command(text, register, gpio, devices))
        # Line 2 armed for a rising edge, with its event buffer held.
        space = devices["gpio"].space
        space.data[0:8] = struct.pack("<HHI", GPIO_SET_IRQ_TYPE, 2, IRQ_RISING)
        gpio.serve(0, 0, [(0, 8, False), (64, 1, True)], space)
        space.data[16:18] = struct.pack("<H", 2)
        self.assertIsNone(gpio.serve(1, 9, [(16, 2, False), (80, 1, True)], space))
        # PEC on does not check the mailbox, as it does not check the mode.
        register.write(bytes([RegisterDevice.MODE, 1]))
        self.assertTrue(register.write(bytes([RegisterDevice.CONTROL]) + b"raise 2"))
        self.assertEqual(gpio.levels[2], 1, "the command ran")
        self.assertEqual(completed, [(1, 9)], "and fired the line through the same path the socket's does")
        self.assertTrue(register.read(32, after_write=True).startswith(b"ok line 2 level 1 fired"), "the reply is read back from the mailbox")
        self.assertTrue(register.write(bytes([RegisterDevice.CONTROL]) + b"bogus"))
        self.assertTrue(register.read(32, after_write=True).startswith(b"error unknown command"), "and a refusal is read back the same way")


class TcpcRig:
    """THE PORT CONTROLLER AND ITS PARTNER under a virtual clock, with a scripted sink that reaches them only as the
    driver does - register writes and write-then-reads."""

    def __init__(self, stretch=1.0):
        self.now = 0.0
        self.levels = {}
        self.said = []
        self.tcpc, self.scheduler = tcpc_partner.build(0x52, 5, lambda line, level: self.levels.__setitem__(line, level), self.said.append, stretch=stretch, clock=lambda: self.now)
        self.partner = self.tcpc.partner

    def advance(self, seconds):
        end = self.now + seconds
        while self.now < end:
            due = self.scheduler.next_due()
            self.now = min(end, due) if due is not None and due > self.now else (self.now if due is not None and due <= self.now else end)
            self.scheduler.run_due()

    def write(self, register, *values):
        return self.tcpc.write(bytes([register, *values]))

    def read(self, register, length):
        self.tcpc.write(bytes([register]), combined=True)
        return self.tcpc.read(length, True)

    def alert(self):
        return struct.unpack("<H", self.read(tcpc_partner.ALERT, 2))[0]

    def clear(self, bits):
        self.write(tcpc_partner.ALERT, bits & 0xFF, bits >> 8)

    def sink_ready(self):
        """What the driver writes at bind, and RECEIVE_DETECT as the engine sets it at attach."""
        self.write(tcpc_partner.ALERT_MASK, 0xFF, 0x07)
        self.write(tcpc_partner.POWER_CONTROL, tcpc_partner.CONTROL_DISABLE_ALARMS)
        self.write(tcpc_partner.COMMAND, tcpc_partner.COMMAND_ENABLE_VBUS_DETECT)
        self.write(tcpc_partner.POWER_STATUS_MASK, tcpc_partner.POWER_VBUS_PRESENT)
        self.write(tcpc_partner.RECEIVE_DETECT, tcpc_partner.DETECT_SOP | tcpc_partner.DETECT_HARD_RESET)

    def received(self):
        """The message in the receive buffer, taken and its alert cleared."""
        frame = self.read(tcpc_partner.RECEIVE_BUFFER, 32)
        message = frame[2:1 + frame[0]]
        self.clear(tcpc_partner.ALERT_RX_STATUS)
        return tcpc_partner.parse(message)

    def send(self, data):
        self.tcpc.write(bytes([tcpc_partner.TRANSMIT_BUFFER, len(data)]) + data)
        self.write(tcpc_partner.TRANSMIT, 0x30)

    def request(self, position, milliamps, message_id=0):
        rdo = position << 28 | 1 << 24 | (milliamps // 10) << 10 | milliamps // 10
        self.send(struct.pack("<HI", 2 | 2 << 6 | (message_id & 7) << 9 | 1 << 12, rdo))


class TcpcTest(unittest.TestCase):
    def attached(self, profile="charger31"):
        """Attached, and the first capabilities just in the receive buffer - VBUS on after 150 ms, the capabilities
        100 ms later, and SenderResponseTimer not yet run out."""
        rig = TcpcRig()
        rig.sink_ready()
        rig.partner.command(["attach", profile])
        rig.advance(0.26)
        return rig

    def test_a_charger_offers_its_capabilities_and_moves_its_supply_only_after_accept(self):
        rig = self.attached()
        self.assertEqual(rig.partner.vbus_mv, 5000)
        self.assertEqual(rig.levels[5], 0, "the alert asserted, active low")
        alert = rig.alert()
        self.assertTrue(alert & tcpc_partner.ALERT_CC_STATUS and alert & tcpc_partner.ALERT_POWER_STATUS and alert & tcpc_partner.ALERT_RX_STATUS)
        self.assertEqual(rig.read(tcpc_partner.CC_STATUS, 1)[0] & 0xF, 3, "Rp 3.0 A on CC1")
        kind, is_data, count, _, revision, objects = rig.received()
        self.assertEqual((kind, is_data, count, revision), (tcpc_partner.SOURCE_CAPABILITIES, True, 6, tcpc_partner.REVISION_3))
        rig.clear(0x7FF)
        alerted = rig.partner.pending_caps_alert
        rig.now += 0.004
        rig.request(4, 2000)
        self.assertTrue(rig.alert() & tcpc_partner.ALERT_TX_SUCCESS, "GoodCRC")
        rig.clear(0x7FF)
        rig.advance(0.01)
        self.assertEqual(rig.received()[0], tcpc_partner.ACCEPT)
        self.assertEqual(rig.partner.vbus_mv, 5000, "not before tSrcTransition")
        rig.advance(0.1)
        self.assertEqual(rig.partner.vbus_mv, 15000)
        self.assertEqual(rig.received()[0], tcpc_partner.PS_RDY)
        self.assertEqual(rig.partner.contract, (4, 15000))
        self.assertEqual(rig.partner.requests, [(4, 15000, 2000, 2000, 0)])
        # FROM THE CAPABILITIES' ALERT TO THE REQUEST'S TRANSMIT: the rig stood 10 ms past the alert, then took 4 ms.
        self.assertAlmostEqual(alerted, 0.25, places=6)
        self.assertAlmostEqual(rig.partner.responses[0], 14.0, places=3)
        self.assertEqual(struct.unpack("<H", rig.read(tcpc_partner.VBUS_VOLTAGE, 2))[0], 600, "15 V in 25 mV units")
        self.assertEqual(rig.partner.violations, [])

    def test_what_a_sink_must_never_do_is_a_violation(self):
        rig = self.attached()
        rig.received()
        rig.request(6, 2000)
        rig.advance(0.01)
        rig.request(4, 3500, message_id=1)
        rig.advance(0.01)
        rig.request(7, 1000, message_id=2)
        rig.advance(0.01)
        self.assertEqual(len(rig.partner.violations), 3, rig.partner.violations)
        self.assertIn("not fixed", rig.partner.violations[0])
        self.assertIn("3500/3500 mA", rig.partner.violations[1])
        self.assertIn("position 7", rig.partner.violations[2])
        rig.partner.command(["detach"])
        rig.write(tcpc_partner.COMMAND, tcpc_partner.COMMAND_SINK_VBUS)
        self.assertEqual(rig.partner.violations[3:], ["the sink path enabled with VBUS at 0 mV"])

    def test_a_hard_reset_takes_vbus_to_zero_and_back_or_keeps_it_off(self):
        rig = self.attached()
        rig.received()
        rig.write(tcpc_partner.COMMAND, tcpc_partner.COMMAND_SINK_VBUS)
        rig.write(tcpc_partner.TRANSMIT, 5)
        self.assertIn("the sink sent a hard reset with its sink path on", rig.partner.violations)
        self.assertEqual(rig.alert() & 0x50, 0x50, "a hard reset sent: both transmission bits")
        self.assertEqual(rig.read(tcpc_partner.RECEIVE_DETECT, 1)[0], 0, "detection reset")
        rig.write(tcpc_partner.COMMAND, tcpc_partner.COMMAND_DISABLE_SINK_VBUS)
        rig.advance(0.05)
        self.assertEqual(rig.partner.vbus_mv, 0)
        rig.advance(0.75)
        self.assertEqual(rig.partner.vbus_mv, 5000, "back after tSrcRecover")
        rig.partner.command(["script", "vbus-stays-off"])
        rig.write(tcpc_partner.TRANSMIT, 5)
        rig.advance(2.0)
        self.assertEqual(rig.partner.vbus_mv, 0, "kept off")
        # A SOURCE WITHOUT POWER DELIVERY never sees it.
        plain = self.attached("typec15")
        plain.write(tcpc_partner.TRANSMIT, 5)
        plain.advance(1.0)
        self.assertEqual(plain.partner.vbus_mv, 5000)
        self.assertEqual(plain.read(tcpc_partner.CC_STATUS, 1)[0] & 0xF, 2, "Rp 1.5 A")

    def test_the_alarms_fire_outside_the_thresholds_the_sink_set(self):
        rig = self.attached()
        rig.clear(0x7FF)
        rig.write(tcpc_partner.VBUS_VOLTAGE_ALARM_HI, 5750 // 25 & 0xFF, 5750 // 25 >> 8)
        rig.write(tcpc_partner.VBUS_VOLTAGE_ALARM_LO, 3750 // 25 & 0xFF, 3750 // 25 >> 8)
        rig.write(tcpc_partner.POWER_CONTROL, 0)
        self.assertEqual(rig.alert() & 0x180, 0, "5 V is inside")
        rig.partner.command(["vbus", "9000"])
        self.assertTrue(rig.alert() & tcpc_partner.ALERT_VBUS_ALARM_HI)
        rig.clear(0x7FF)
        rig.partner.command(["vbus", "3000"])
        self.assertTrue(rig.alert() & tcpc_partner.ALERT_VBUS_ALARM_LO)

    def test_capabilities_unanswered_bring_a_hard_reset_and_the_stretch_widens_the_wait(self):
        for stretch, answered in ((1.0, False), (100.0, True)):
            rig = TcpcRig(stretch)
            rig.sink_ready()
            rig.partner.command(["attach", "charger31"])
            rig.advance(0.26)
            rig.received()
            rig.advance(0.05)
            self.assertEqual(rig.partner.response_timeouts == 0, answered, f"stretch {stretch}")
            self.assertEqual(bool(rig.alert() & tcpc_partner.ALERT_RX_HARD_RESET), not answered)

    def test_nothing_is_acknowledged_until_the_sink_receives_and_the_capabilities_are_sent_again(self):
        rig = TcpcRig()
        rig.write(tcpc_partner.COMMAND, tcpc_partner.COMMAND_ENABLE_VBUS_DETECT)
        rig.partner.command(["attach", "charger20"])
        rig.advance(0.5)
        self.assertGreaterEqual(rig.partner.caps_count, 2, "sent every SourceCapabilityTimer, unacknowledged")
        rig.write(tcpc_partner.RECEIVE_DETECT, tcpc_partner.DETECT_SOP)
        rig.advance(0.06)
        self.assertEqual(rig.received()[4], tcpc_partner.REVISION_2)
        for kind in ("count", "reserved", "extended"):
            self.assertTrue(rig.partner.malformed(kind).startswith("ok"))
            frame = rig.read(tcpc_partner.RECEIVE_BUFFER, 32)
            rig.clear(tcpc_partner.ALERT_RX_STATUS)
            self.assertTrue(frame[0] >= 3)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--i2c", help="the virtio-i2c vhost-user socket to listen on")
    parser.add_argument("--gpio", help="the virtio-gpio vhost-user socket to listen on")
    parser.add_argument("--control", help="the control socket to listen on")
    parser.add_argument("--ready", help="a file written once every socket listens")
    parser.add_argument("--hid", action="store_true", help="the HID-over-I2C models: a touchpad at 0x2C and a touchscreen at 0x10")
    parser.add_argument("--tcpc", action="store_true", help="a Type-C port controller at 0x52, its alert on line 5, and the Power Delivery source on its cable")
    parser.add_argument("--tcpc-log", help="the port controller's and its partner's record")
    parser.add_argument("--stretch", type=float, default=1.0, help="the factor on the source's SenderResponseTimer and tSrcTransition (emulated ports)")
    parser.add_argument("--self-test", action="store_true", help="run the host suite and exit")
    parser.add_argument("--trace", action="store_true", help="one line per request and control message, on stderr")
    args = parser.parse_args()
    global TRACE
    TRACE = args.trace
    if args.self_test:
        suite = unittest.TestSuite([unittest.defaultTestLoader.loadTestsFromTestCase(SelfTest), unittest.defaultTestLoader.loadTestsFromTestCase(TcpcTest)])
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        sys.exit(0 if result.wasSuccessful() else 1)
    if not (args.i2c and args.gpio and args.control):
        parser.error("--i2c, --gpio and --control are required")
    try:
        serve(args)
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
