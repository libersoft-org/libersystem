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

and each answers one line, `ok ...` or `error ...`. The same commands reach it IN-BAND, through the register
device's control mailbox (register 0xE0 at 0x50), for a test in the guest, which cannot reach a host socket.

THE IOMMU. Each device is attached with `iommu_platform=on` whenever the machine has a virtio-iommu, so the
backend offers VIRTIO_F_ACCESS_PLATFORM with the reply-ack and backend-request protocol features (QEMU refuses
an IOMMU device whose backend lacks them), translates every ring and buffer address through the IOTLB
entries QEMU sends - each update and invalidation acknowledged - and on a miss asks on the backend channel
and serves the main channel until the update arrives. Without the IOMMU the addresses are the guest's own and
the memory table translates them.

`--self-test` runs the host suite of the parts a host can check without QEMU: the memory table's
translation, the IOTLB's updates, misses and invalidations, and the two device models.
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
    register = RegisterDevice(0x50, control=lambda text: command(text, register, gpio_model, devices))
    i2c_model = I2cModel([register])
    gpio_model = GpioModel(["hid-touchpad", "hid-touchscreen", "acpi-aei", "spare-3", "spare-4", "spare-5", "spare-6", "spare-7"])
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
        ready, _, _ = select.select(watched, [], [])
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
                item.sendall((command(line.decode().strip(), register, gpio_model, devices) + "\n").encode())
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


def command(text, register, gpio, devices):
    words = text.split()
    try:
        if not words:
            return "error empty"
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


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--i2c", help="the virtio-i2c vhost-user socket to listen on")
    parser.add_argument("--gpio", help="the virtio-gpio vhost-user socket to listen on")
    parser.add_argument("--control", help="the control socket to listen on")
    parser.add_argument("--ready", help="a file written once every socket listens")
    parser.add_argument("--self-test", action="store_true", help="run the host suite and exit")
    parser.add_argument("--trace", action="store_true", help="one line per request and control message, on stderr")
    args = parser.parse_args()
    global TRACE
    TRACE = args.trace
    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(SelfTest)
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
