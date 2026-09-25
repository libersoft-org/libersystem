#!/usr/bin/env python3
"""USB devices whose descriptors the kernel's gadget functions cannot carry, played through gadgetfs.

WHY GADGETFS AND NOT FUNCTIONFS. A mobile-broadband modem and a video camera describe themselves with
class-specific interface descriptors - CDC's header, union and MBIM records, UVC's control and streaming
records - and FunctionFS refuses every one of them (it accepts class records for HID, CCID and DFU only). The
legacy gadgetfs takes the WHOLE configuration descriptor from the process and serves it as written, so a device
of any class can be played - at the cost of this process answering the standard requests the composite
framework would otherwise answer, which is why the framework below is longer than `usb_ffs.py`'s.

THE RULES gadgetfs PLAYS BY, read from `drivers/usb/gadget/legacy/inode.c` and not guessed:
  - the first write to the controller's file is a zero tag, the full-speed configuration, the high-speed one
    and the device descriptor; the gadget binds then;
  - requests the kernel does not answer itself reach this process only for a device whose class is 0xff -
    so the device descriptor says vendor-specific, and the interfaces say what the device is;
  - an OUT request with no data is acknowledged by a zero-length read, its data is collected by a read, and a
    request is stalled by I/O in the wrong direction;
  - an endpoint is enabled by writing tag 1 and its full- and high-speed descriptors to the endpoint's file,
    whose name is the controller's own endpoint name - which fixes the address the descriptors must use.

`usb-gadget.sh` mounts gadgetfs in its own state directory, starts this program, waits for it to say the
descriptors are written, and at teardown stops it - which closes the controller file and unbinds the gadget -
before unmounting.
"""

import argparse
import os
import queue
import struct
import sys
import threading
import time

EVENT_NOP, EVENT_CONNECT, EVENT_DISCONNECT, EVENT_SETUP, EVENT_SUSPEND = range(5)
EVENT_SIZE = 12
VENDOR_ID, PRODUCT_ID = 0x1D6B, 0x0104


def say(name, message):
    print(f"usb-gadgetfs {name}: {message}", file=sys.stderr, flush=True)


def endpoint(address, attributes, packet, interval):
    return bytes([7, 5, address, attributes]) + struct.pack("<H", packet) + bytes([interval])


def configuration(interfaces, body):
    return bytes([9, 2]) + struct.pack("<H", 9 + len(body)) + bytes([interfaces, 1, 0, 0xC0, 1]) + body


class Endpoint:
    """One endpoint file: enabled when the host selects what carries it, and read or written only then."""

    def __init__(self, device, name, full, high):
        self.device = device
        self.name = name
        self.full = full
        self.high = high
        self.fd = None
        self.lock = threading.Lock()
        self.enabled = threading.Event()

    def enable(self):
        with self.lock:
            if self.fd is not None:
                return
            fd = os.open(os.path.join(self.device.mount, self.name), os.O_RDWR)
            os.write(fd, struct.pack("<I", 1) + self.full + self.high)
            self.fd = fd
            self.enabled.set()

    def disable(self):
        with self.lock:
            self.enabled.clear()
            if self.fd is not None:
                try:
                    os.close(self.fd)
                except OSError:
                    pass
                self.fd = None

    def read(self, length):
        while True:
            self.enabled.wait()
            fd = self.fd
            if fd is None:
                continue
            try:
                return os.read(fd, length)
            except OSError:
                time.sleep(0.02)

    def write(self, data):
        while True:
            self.enabled.wait()
            fd = self.fd
            if fd is None:
                continue
            try:
                return os.write(fd, data)
            except OSError:
                time.sleep(0.02)


class Device:
    name = "device"

    def __init__(self, mount, ready):
        self.mount = mount
        self.ready = ready
        self.endpoints = {}
        self.configured = threading.Event()
        self.speed_high = True

    # The configurations (full speed, high speed) and the endpoints by the controller's names.
    def descriptors(self):
        raise NotImplementedError

    def device_descriptor(self):
        return bytes([18, 1, 0x00, 0x02, 0xFF, 0x00, 0x00, 64]) + struct.pack("<HHH", VENDOR_ID, PRODUCT_ID, 0x0100) + bytes([0, 0, 0, 1])

    # What SET_CONFIGURATION enables, and SET_INTERFACE: overridden per device.
    def on_configured(self, value):
        pass

    def on_interface(self, interface, alternate):
        pass

    # A class request: bytes for an IN one or None to stall it; an OUT one's data after `accepts` took it.
    def setup(self, request_type, request, value, index, length, data):
        return None

    def accepts(self, request_type, request, value, index, length):
        return False

    def start(self):
        pass

    def serve(self):
        controller = [name for name in os.listdir(self.mount) if not name.startswith("ep")]
        if not controller:
            raise OSError("no controller file in the gadgetfs mount")
        ep0 = os.open(os.path.join(self.mount, controller[0]), os.O_RDWR)
        full, high = self.descriptors()
        os.write(ep0, struct.pack("<I", 0) + full + high + self.device_descriptor())
        say(self.name, "descriptors written")
        if self.ready:
            with open(self.ready, "w") as handle:
                handle.write("ready\n")
        self.start()
        while True:
            events = os.read(ep0, EVENT_SIZE * 4)
            for at in range(0, len(events), EVENT_SIZE):
                event = events[at:at + EVENT_SIZE]
                kind = struct.unpack("<I", event[8:12])[0]
                if kind == EVENT_CONNECT:
                    self.speed_high = struct.unpack("<I", event[:4])[0] >= 3
                elif kind == EVENT_DISCONNECT:
                    self.configured.clear()
                    for ep in self.endpoints.values():
                        ep.disable()
                elif kind == EVENT_SETUP:
                    self.control(ep0, *struct.unpack("<BBHHH", event[:8]))

    def ack(self, ep0):
        os.read(ep0, 0)

    def stall(self, ep0, request_type):
        try:
            if request_type & 0x80:
                os.read(ep0, 0)
            else:
                os.write(ep0, b"")
        except OSError:
            pass

    def control(self, ep0, request_type, request, value, index, length):
        if request_type & 0x60 == 0:
            if request == 9 and request_type == 0x00:
                for ep in self.endpoints.values():
                    ep.disable()
                self.on_configured(value)
                if value:
                    self.configured.set()
                else:
                    self.configured.clear()
                self.ack(ep0)
                return
            if request == 11 and request_type == 0x01:
                self.on_interface(index, value)
                self.ack(ep0)
                return
            if request == 10 and request_type == 0x81:
                os.write(ep0, bytes([self.alternate_of(index)])[:length])
                return
            if request == 1 and request_type == 0x02:
                # CLEAR_FEATURE(ENDPOINT_HALT): the host resetting a pipe's toggle.
                self.on_clear_halt(index)
                self.ack(ep0)
                return
            self.stall(ep0, request_type)
            return
        if request_type & 0x80:
            answer = self.setup(request_type, request, value, index, length, b"")
            if answer is None:
                self.stall(ep0, request_type)
            else:
                os.write(ep0, bytes(answer)[:length])
        elif self.accepts(request_type, request, value, index, length):
            data = os.read(ep0, length)
            self.setup(request_type, request, value, index, length, data)
        else:
            self.stall(ep0, request_type)

    def alternate_of(self, interface):
        return 0

    def on_clear_halt(self, address):
        pass


# ------------------------------------------------------------------ a mobile-broadband modem, over MBIM

BASIC_CONNECT = bytes([0xA2, 0x89, 0xCC, 0x33, 0xBC, 0xBB, 0x8B, 0x4F, 0xB6, 0xB0, 0x13, 0x3E, 0xC2, 0xAA, 0xE6, 0xDF])
CONTEXT_INTERNET = bytes([0x7E, 0x5E, 0x2A, 0x7E, 0x4E, 0x6F, 0x72, 0x72, 0x73, 0x6B, 0x65, 0x6E, 0x7E, 0x5E, 0x2A, 0x7E])
PIN, PUK = "1234", "12345678"
ADDRESS, GATEWAY = bytes([10, 64, 0, 2]), bytes([10, 64, 0, 1])
PREFIX, MTU = 30, 1400
IMSI, ICCID, MSISDN = "001010000000001", "8900100000000000001", "+15550100"
# THE SMALLEST CONTROL MESSAGE THE CLASS ALLOWS, and the reason it is this small: every answer longer than 44
# bytes of body - the capabilities, the subscriber's identities, the registration - then goes in fragments, and
# so do the host's PIN and connect commands. A guest that cannot put fragments back together fails the oracle.
MAX_CONTROL = 64
NTH16, IPS0 = 0x484D434E, 0x00535049


class Info:
    """An information buffer: fixed fields, then strings referenced by offset and size."""

    def __init__(self, fixed):
        self.fixed_len = fixed
        self.fixed = b""
        self.data = b""

    def u32(self, value):
        self.fixed += struct.pack("<I", value & 0xFFFFFFFF)
        return self

    def raw(self, data):
        self.fixed += data
        return self

    def string(self, text):
        if not text:
            return self.u32(0).u32(0)
        while len(self.data) % 4:
            self.data += b"\0"
        encoded = text.encode("utf-16-le")
        offset = self.fixed_len + len(self.data)
        self.data += encoded
        return self.u32(offset).u32(len(encoded))

    def bytes(self):
        return self.fixed + self.data


def utf16(info, at):
    offset, size = struct.unpack("<II", info[at:at + 8])
    return info[offset:offset + size].decode("utf-16-le") if size else ""


def checksum(data):
    if len(data) % 2:
        data += b"\0"
    total = sum(struct.unpack(f"!{len(data) // 2}H", data))
    while total >> 16:
        total = (total & 0xFFFF) + (total >> 16)
    return ~total & 0xFFFF


class Mbim(Device):
    """A modem with a SIM locked by PIN 1234, a home network called LiberNet and one Internet context.

    The context's IPv4 is 10.64.0.2/30 behind a gateway at 10.64.0.1 that answers ICMP echo, which is what the
    oracle's datagrams reach. Every state change it makes - the SIM unlocked, the context up or down - it
    indicates, as a modem does.
    """

    name = "mbim"

    def __init__(self, mount, ready):
        super().__init__(mount, ready)
        self.responses = queue.Queue()
        self.locked = True
        self.pin_tries = 3
        self.active = False
        self.alternate = 0
        self.sequence = 0
        # The largest control transfer the host said it takes, at open; never more than this device's own.
        self.max_control = MAX_CONTROL
        # A command arriving in fragments: its transaction, how many it said, and what arrived so far.
        self.assembly = None
        self.said = set()

    def descriptors(self):
        def one(bulk, interval):
            body = bytes([9, 4, 0, 0, 1, 0x02, 0x0E, 0x00, 0])
            body += bytes([5, 0x24, 0x00, 0x10, 0x01]) + bytes([5, 0x24, 0x06, 0, 1])
            body += bytes([12, 0x24, 0x1B, 0x00, 0x01]) + struct.pack("<H", MAX_CONTROL) + bytes([16, 128]) + struct.pack("<H", 1500) + bytes([0x20])
            body += endpoint(0x85, 0x03, 16, interval)
            body += bytes([9, 4, 1, 0, 0, 0x0A, 0x00, 0x02, 0]) + bytes([9, 4, 1, 1, 2, 0x0A, 0x00, 0x02, 0])
            body += endpoint(0x81, 0x02, bulk, 0) + endpoint(0x02, 0x02, bulk, 0)
            return configuration(2, body)

        self.endpoints = {
            "notify": Endpoint(self, "ep5in-int", endpoint(0x85, 0x03, 16, 16), endpoint(0x85, 0x03, 16, 9)),
            "in": Endpoint(self, "ep1in-bulk", endpoint(0x81, 0x02, 64, 0), endpoint(0x81, 0x02, 512, 0)),
            "out": Endpoint(self, "ep2out-bulk", endpoint(0x02, 0x02, 64, 0), endpoint(0x02, 0x02, 512, 0)),
        }
        return one(64, 16), one(512, 9)

    def drain(self):
        while True:
            try:
                self.responses.get_nowait()
            except queue.Empty:
                return

    # A NEW CONFIGURATION IS A NEW HOST: whatever this host's own stack left queued - it met the device first - is
    # not the guest's to read.
    def on_configured(self, value):
        self.alternate = 0
        self.drain()
        if value:
            self.endpoints["notify"].enable()

    def on_interface(self, interface, alternate):
        if interface == 1:
            self.alternate = alternate
            for name in ("in", "out"):
                if alternate == 1:
                    self.endpoints[name].enable()
                else:
                    self.endpoints[name].disable()

    def alternate_of(self, interface):
        return self.alternate if interface == 1 else 0

    def accepts(self, request_type, request, value, index, length):
        # SEND_ENCAPSULATED_COMMAND, SET_NTB_INPUT_SIZE, RESET_FUNCTION.
        return request_type == 0x21 and request in (0x00, 0x86, 0x05)

    def setup(self, request_type, request, value, index, length, data):
        if request_type == 0x21:
            if request == 0x00:
                self.message(data)
            return None
        if request_type == 0xA1 and request == 0x01:
            try:
                return self.responses.get_nowait()
            except queue.Empty:
                return b""
        if request_type == 0xA1 and request == 0x80:
            return struct.pack("<HHIHHHHIHHHH", 28, 1, 4096, 4, 0, 4, 0, 4096, 4, 0, 4, 0)
        return None

    def respond(self, *transfers):
        for transfer in transfers:
            self.responses.put(transfer)
        threading.Thread(target=self.endpoints["notify"].write, args=(bytes([0xA1, 0x01, 0, 0, 0, 0, 0, 0]),), daemon=True).start()

    def once(self, message):
        if message not in self.said:
            self.said.add(message)
            say(self.name, message)

    # A MESSAGE LONGER THAN THE HOST TAKES GOES IN FRAGMENTS, each with its own length and its place in the whole:
    # the fragment header is the message's own total and this piece's number.
    def fragmented(self, kind, transaction, body):
        room = self.max_control - 20
        pieces = [body[at:at + room] for at in range(0, len(body), room)] or [b""]
        if len(pieces) > 1:
            self.once(f"an answer went in {len(pieces)} fragments")
        return [struct.pack("<IIIII", kind, 20 + len(piece), transaction, len(pieces), current) + piece for current, piece in enumerate(pieces)]

    def done(self, transaction, cid, status, info):
        self.respond(*self.fragmented(0x80000003, transaction, BASIC_CONNECT + struct.pack("<III", cid, status, len(info)) + info))

    def indicate(self, cid, info):
        self.respond(*self.fragmented(0x80000007, 0, BASIC_CONNECT + struct.pack("<II", cid, len(info)) + info))

    # A COMMAND IN FRAGMENTS, put back together: the first says how many, and each after it must be the next of
    # the same transaction - anything else abandons the command, as a device does.
    def reassembled(self, data):
        length, transaction = struct.unpack("<II", data[4:12])
        total, current = struct.unpack("<II", data[12:20])
        piece = data[20:length]
        if current == 0:
            self.assembly = (transaction, total, 1, bytearray(piece)) if total > 1 else None
            return None if total > 1 else data[:length]
        if self.assembly is None or self.assembly[0] != transaction or self.assembly[1] != total or self.assembly[2] != current:
            say(self.name, f"fragment {current} of {total} out of order - the command is abandoned")
            self.assembly = None
            return None
        whole = self.assembly[3] + piece
        if current + 1 < total:
            self.assembly = (transaction, total, current + 1, whole)
            return None
        self.assembly = None
        self.once(f"a command arrived in {total} fragments")
        return struct.pack("<IIIII", 3, 20 + len(whole), transaction, 1, 0) + bytes(whole)

    def subscriber(self):
        state = 6 if self.locked else 1
        info = Info(36).u32(state).string(IMSI if not self.locked else "").string(ICCID).u32(0).u32(1 if not self.locked else 0)
        if not self.locked:
            info.string(MSISDN)
        else:
            info.u32(0).u32(0)
        return info.bytes()

    def register(self):
        state = 3 if not self.locked else 1
        return Info(48).u32(0).u32(state).u32(1).u32(0).u32(0).string("00101" if not self.locked else "").string("LiberNet" if not self.locked else "").string("").u32(0).bytes()

    def connect_info(self):
        return struct.pack("<IIII", 0, 1 if self.active else 3, 0, 1) + CONTEXT_INTERNET + struct.pack("<I", 0)

    def message(self, data):
        if len(data) < 12:
            return
        kind, length, transaction = struct.unpack("<III", data[:12])
        if kind == 1:
            self.active = False
            self.assembly = None
            self.drain()
            (asked,) = struct.unpack("<I", data[12:16]) if len(data) >= 16 else (MAX_CONTROL,)
            self.max_control = max(64, min(MAX_CONTROL, asked))
            self.respond(struct.pack("<IIII", 0x80000001, 16, transaction, 0))
            say(self.name, f"opened, control transfers of at most {self.max_control} bytes")
            return
        if kind == 2:
            self.respond(struct.pack("<IIII", 0x80000002, 16, transaction, 0))
            return
        if kind != 3 or len(data) < 20:
            return
        data = self.reassembled(data)
        if data is None or len(data) < 48:
            return
        cid, command_type, info_length = struct.unpack("<III", data[36:48])
        info = data[48:48 + info_length]
        if cid == 1:
            caps = Info(64).u32(3).u32(1).u32(1).u32(2).u32(0x3F).u32(0).u32(1).u32(1).u32(0).u32(0).string("490154203237518").string("1.0").string("LiberSystem harness modem").bytes()
            self.done(transaction, cid, 0, caps)
        elif cid == 2:
            self.done(transaction, cid, 0, self.subscriber())
        elif cid == 4:
            if command_type == 1:
                pin_type, _operation = struct.unpack("<II", info[:8])
                entered = utf16(info, 8)
                if pin_type == 2 and self.locked and entered == PIN:
                    self.locked = False
                    self.pin_tries = 3
                    say(self.name, "PIN accepted - the SIM is ready")
                    self.done(transaction, cid, 0, struct.pack("<III", 2, 0, 3))
                    self.indicate(2, self.subscriber())
                    self.indicate(9, self.register())
                else:
                    if self.locked:
                        self.pin_tries = max(0, self.pin_tries - 1)
                    say(self.name, f"PIN refused - {self.pin_tries} tries left")
                    # MBIM_STATUS_FAILURE, with the PIN state still locked.
                    self.done(transaction, cid, 2, struct.pack("<III", 2, 1, self.pin_tries))
            else:
                state = (2, 1, self.pin_tries) if self.locked else (0, 0, 0)
                self.done(transaction, cid, 0, struct.pack("<III", *state))
        elif cid == 9:
            self.done(transaction, cid, 0, self.register())
        elif cid == 11:
            self.done(transaction, cid, 0, struct.pack("<IIIII", 20, 99, 5, 0, 0))
        elif cid == 12:
            if command_type == 1:
                _session, activate = struct.unpack("<II", info[:8])
                apn = utf16(info, 8)
                if self.locked:
                    self.done(transaction, cid, 2, self.connect_info())
                    return
                self.active = bool(activate)
                say(self.name, f"context {'activated' if self.active else 'deactivated'} (APN {apn!r})")
                self.done(transaction, cid, 0, self.connect_info())
                self.indicate(12, self.connect_info())
            else:
                self.done(transaction, cid, 0, self.connect_info())
        elif cid == 15:
            if not self.active:
                self.done(transaction, cid, 0, struct.pack("<15I", *([0] * 15)))
                return
            fixed = struct.pack("<IIIIIIIIIIIIIII", 0, 0x0F, 0, 1, 60, 0, 0, 68, 0, 1, 72, 0, 0, MTU, 0)
            self.done(transaction, cid, 0, fixed + struct.pack("<I", PREFIX) + ADDRESS + GATEWAY + GATEWAY)
        else:
            # MBIM_STATUS_NO_DEVICE_SUPPORT.
            self.done(transaction, cid, 9, b"")

    # ------------------------------------------------------------------ the data pipe

    def block(self, datagrams):
        out = bytearray(12)
        pointers = []
        for datagram in datagrams:
            while len(out) % 4:
                out.append(0)
            pointers.append((len(out), len(datagram)))
            out += datagram
        while len(out) % 4:
            out.append(0)
        ndp = len(out)
        ndp_length = max(16, 8 + (len(pointers) + 1) * 4)
        out += struct.pack("<IHH", IPS0, ndp_length, 0)
        for index, length in pointers:
            out += struct.pack("<HH", index, length)
        out += b"\0\0\0\0"
        while len(out) < ndp + ndp_length:
            out.append(0)
        # NEVER A MULTIPLE OF THE PACKET SIZE: the block would then end on a packet boundary with nothing to
        # say so, and the host's receive would wait for the next block's bytes.
        packet = 512 if self.speed_high else 64
        if len(out) % packet == 0:
            out += b"\0\0\0\0"
        self.sequence = (self.sequence + 1) & 0xFFFF
        struct.pack_into("<IHHHH", out, 0, NTH16, 12, self.sequence, len(out), ndp)
        return bytes(out)

    def datagrams(self, block):
        if len(block) < 12 or struct.unpack("<I", block[:4])[0] != NTH16:
            return []
        length, ndp = struct.unpack("<HH", block[8:12])
        block = block[:length]
        found = []
        while ndp and ndp + 16 <= len(block):
            signature, ndp_length, next_ndp = struct.unpack("<IHH", block[ndp:ndp + 8])
            pointer = ndp + 8
            while pointer + 4 <= ndp + ndp_length:
                index, size = struct.unpack("<HH", block[pointer:pointer + 4])
                pointer += 4
                if index == 0 and size == 0:
                    break
                found.append(bytes(block[index:index + size]))
            ndp = next_ndp
        return found

    def answer(self, packet):
        # ICMP echo to the gateway, answered by it.
        if len(packet) < 28 or packet[0] >> 4 != 4 or packet[9] != 1 or packet[16:20] != GATEWAY:
            return None
        header = (packet[0] & 0x0F) * 4
        icmp = bytearray(packet[header:])
        if icmp[0] != 8:
            return None
        icmp[0] = 0
        icmp[2:4] = b"\0\0"
        icmp[2:4] = struct.pack("!H", checksum(bytes(icmp)))
        ip = bytearray(packet[:header])
        ip[12:16], ip[16:20] = packet[16:20], packet[12:16]
        ip[10:12] = b"\0\0"
        ip[10:12] = struct.pack("!H", checksum(bytes(ip)))
        return bytes(ip) + bytes(icmp)

    def data_loop(self):
        packet = lambda: 512 if self.speed_high else 64
        held = bytearray()
        while True:
            held += self.endpoints["out"].read(packet())
            while len(held) >= 12:
                if struct.unpack("<I", held[:4])[0] != NTH16:
                    held.clear()
                    break
                length = struct.unpack("<H", held[8:10])[0]
                if len(held) < length:
                    break
                block = bytes(held[:length])
                del held[:length]
                replies = [reply for reply in (self.answer(datagram) for datagram in self.datagrams(block)) if reply]
                if replies and self.active:
                    self.endpoints["in"].write(self.block(replies))
                    say(self.name, f"{len(replies)} echo repl{'y' if len(replies) == 1 else 'ies'} sent")

    def start(self):
        threading.Thread(target=self.data_loop, daemon=True).start()


# ------------------------------------------------------------------ a bulk-streaming video camera

YUY2 = bytes([0x59, 0x55, 0x59, 0x32, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71])
WIDTH, HEIGHT, INTERVAL = 64, 48, 333333
FRAME_BYTES = WIDTH * HEIGHT * 2
PAYLOAD = 4096
# THE FRAME THE DEVICE MARKS BAD: its first payload carries the error bit, and the guest must drop it.
BAD_FRAME = 3


def frame_bytes(number):
    """Frame `number`: its number in its first eight bytes, then the oracle's pattern."""
    return struct.pack("<Q", number) + bytes((at * 7 + number * 13) % 251 for at in range(8, FRAME_BYTES))


class Uvc(Device):
    """A UVC 1.1 camera streaming 64x48 YUY2 over bulk: the probe and commit a host negotiates with, then frames
    of one known pattern each, cut into payloads of at most 4096 bytes whose headers toggle FID per frame and set
    EOF on each frame's last - and frame 3 marked with the error bit, which the guest has to drop. The SECOND
    stream a host commits ends differently: once its first frame has gone, the camera leaves the bus.
    """

    name = "uvc"

    def __init__(self, mount, ready):
        super().__init__(mount, ready)
        self.probe = self.default_probe()
        self.streaming = threading.Event()
        self.generation = 0
        self.commits = 0

    def default_probe(self):
        probe = bytearray(34)
        probe[0] = 1
        probe[2], probe[3] = 1, 1
        struct.pack_into("<I", probe, 4, INTERVAL)
        struct.pack_into("<I", probe, 18, FRAME_BYTES)
        struct.pack_into("<I", probe, 22, PAYLOAD)
        probe[31] = 1
        return probe

    def descriptors(self):
        def one(bulk):
            control = bytes([13, 0x24, 0x01, 0x10, 0x01, 40, 0]) + struct.pack("<I", 6_000_000) + bytes([1, 1])
            control += bytes([18, 0x24, 0x02, 1, 0x01, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0])
            control += bytes([9, 0x24, 0x03, 2, 0x01, 0x01, 0, 1, 0])
            streaming = bytes([14, 0x24, 0x01, 1, 77, 0, 0x81, 0, 2, 0, 0, 0, 1, 0])
            streaming += bytes([27, 0x24, 0x04, 1, 1]) + YUY2 + bytes([16, 1, 0, 0, 0, 0])
            streaming += bytes([30, 0x24, 0x05, 1, 0]) + struct.pack("<HHIIIIB", WIDTH, HEIGHT, 1, 1, FRAME_BYTES, INTERVAL, 1) + struct.pack("<I", INTERVAL)
            streaming += bytes([6, 0x24, 0x0D, 1, 1, 4])
            body = bytes([9, 4, 0, 0, 0, 0x0E, 0x01, 0x00, 0]) + control
            body += bytes([9, 4, 1, 0, 1, 0x0E, 0x02, 0x00, 0]) + streaming + endpoint(0x81, 0x02, bulk, 0)
            return configuration(2, body)

        self.endpoints = {"video": Endpoint(self, "ep1in-bulk", endpoint(0x81, 0x02, 64, 0), endpoint(0x81, 0x02, 512, 0))}
        return one(64), one(512)

    def on_configured(self, value):
        self.stop()
        if value:
            self.endpoints["video"].enable()

    def on_clear_halt(self, address):
        if address & 0xFF == 0x81:
            self.stop()
            say(self.name, "the host stopped the stream")

    def stop(self):
        self.streaming.clear()
        self.generation += 1

    def accepts(self, request_type, request, value, index, length):
        return request_type == 0x21 and request == 0x01 and value >> 8 in (1, 2)

    def setup(self, request_type, request, value, index, length, data):
        selector = value >> 8
        if request_type == 0x21:
            if selector == 1:
                probe = bytearray(self.default_probe())
                probe[:min(len(data), 8)] = data[:min(len(data), 8)]
                struct.pack_into("<I", probe, 18, FRAME_BYTES)
                struct.pack_into("<I", probe, 22, PAYLOAD)
                self.probe = probe
            elif selector == 2:
                self.start_stream()
            return None
        if request_type == 0xA1 and selector in (1, 2):
            if request == 0x85:
                return struct.pack("<H", 34)
            if request == 0x86:
                return bytes([3])
            return bytes(self.probe if request == 0x81 else self.default_probe())
        return None

    def start_stream(self):
        self.stop()
        self.commits += 1
        self.streaming.set()
        threading.Thread(target=self.stream, args=(self.generation,), daemon=True).start()
        say(self.name, "committed - streaming")

    def stream(self, generation):
        number = 0
        video = self.endpoints["video"]
        while self.streaming.is_set() and self.generation == generation:
            data = frame_bytes(number)
            fid = number & 1
            room = PAYLOAD - 2
            chunks = [data[at:at + room] for at in range(0, len(data), room)]
            for at, chunk in enumerate(chunks):
                flags = 0x80 | fid | (0x02 if at == len(chunks) - 1 else 0)
                if number == BAD_FRAME and at == 0:
                    flags |= 0x40
                if not (self.streaming.is_set() and self.generation == generation):
                    return
                video.write(bytes([2, flags]) + chunk)
            # THE CABLE PULLED IN THE MIDDLE OF THE SECOND STREAM: every file closed at once, which closes the
            # controller file, and gadgetfs disconnects a gadget whose controller file closed.
            if self.commits >= 2:
                say(self.name, "leaving the bus in the middle of the second stream")
                os._exit(0)
            number += 1
            time.sleep(INTERVAL / 10_000_000)


EMULATORS = {"mbim": Mbim, "uvc": Uvc}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--emulate", required=True, choices=sorted(EMULATORS))
    parser.add_argument("--mount", required=True)
    parser.add_argument("--ready")
    args = parser.parse_args()
    device = EMULATORS[args.emulate](args.mount, args.ready)
    try:
        device.serve()
    except OSError as error:
        say(device.name, f"stopped: {error}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
