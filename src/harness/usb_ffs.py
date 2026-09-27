#!/usr/bin/env python3
"""USB functions the host's kernel has no gadget for, played through FunctionFS.

WHY THIS EXISTS. `usb-gadget.sh` builds its devices out of the kernel's own gadget functions - a serial port, a
HID device, a printer, a MIDI port - and three of the classes the guest's controller binds have none: a CCID
smart-card reader, a DFU firmware target and a Bluetooth controller's HCI transport. FunctionFS is the kernel's
way to let a process BE a function: the process writes the interface and endpoint descriptors, answers the
control requests addressed to its interface, and reads and writes its endpoints as files. So each of those
three is a small emulator here, written from its class specification.

WHAT THE HARNESS OWNS AND WHAT IT DOES NOT. `usb-gadget.sh` mounts the FunctionFS instance in its own state
directory, starts this program on it, waits for the endpoint files to appear and only then binds the gadget -
and at teardown stops this program before it unmounts anything. This program touches nothing outside the mount
it is given.

THE DEVICES ARE WHAT THE ORACLES CHECK AGAINST, so each one says, on stderr, what it received that matters.
"""

import argparse
import errno
import hashlib
import os
import struct
import sys
import threading
import time

FUNCTIONFS_DESCRIPTORS_MAGIC_V2 = 3
FUNCTIONFS_STRINGS_MAGIC = 2
HAS_FS_DESC = 1
HAS_HS_DESC = 2
ALL_CTRL_RECIP = 64

EVENT_BIND, EVENT_UNBIND, EVENT_ENABLE, EVENT_DISABLE, EVENT_SETUP, EVENT_SUSPEND, EVENT_RESUME = range(7)
EVENT_SIZE = 12


def say(name, message):
    print(f"usb-ffs {name}: {message}", file=sys.stderr, flush=True)


def interface(number, endpoints, cls, subclass, protocol):
    return bytes([9, 4, number, 0, endpoints, cls, subclass, protocol, 0])


def endpoint(address, attributes, packet, interval):
    return bytes([7, 5, address]) + bytes([attributes]) + struct.pack("<H", packet) + bytes([interval])


class Function:
    """One FunctionFS function: its descriptors, its control requests and its endpoint files."""

    name = "function"
    flags = 0

    def __init__(self, mount, ready=None):
        self.mount = mount
        self.ready = ready
        self.files = {}
        self.enabled = threading.Event()

    # Full-speed and high-speed descriptor lists; subclasses give both.
    def descriptors(self):
        raise NotImplementedError

    # A control request: bytes to answer an IN one, or None to stall it; an OUT one's data, once `accepts` said
    # it is taken.
    def setup(self, request_type, request, value, index, length, data):
        return None

    # Whether an OUT request is taken at all. One that is not is stalled before its data stage.
    def accepts(self, request_type, request, value, index, length):
        return False

    # The endpoint threads, started once the host first enables the function.
    def run_endpoints(self):
        pass

    def blob(self):
        full, high = self.descriptors()
        body = struct.pack("<II", len(full), len(high)) + b"".join(full) + b"".join(high)
        flags = HAS_FS_DESC | HAS_HS_DESC | self.flags
        return struct.pack("<III", FUNCTIONFS_DESCRIPTORS_MAGIC_V2, 12 + len(body), flags) + body

    def open_endpoint(self, number):
        if number not in self.files:
            self.files[number] = os.open(os.path.join(self.mount, f"ep{number}"), os.O_RDWR)
        return self.files[number]

    # Read and write that survive the function being disabled under them, which is what happens when the host
    # this device first appeared on configures it and `usb-host` then resets it for the guest.
    def read(self, number, length):
        while True:
            try:
                return os.read(self.open_endpoint(number), length)
            except OSError:
                time.sleep(0.05)

    # THE BULK PACKET SIZE AT THE SPEED THE DEVICE RUNS AT. A host sends no zero-length packet after data that ends
    # on a packet boundary - Bluetooth ACL and CCID messages carry their own lengths - so a read bigger than one
    # packet waits on a 512-byte message for bytes that belong to the next one. Reads are one packet, and the
    # emulators cut messages out by their headers.
    def packet_size(self):
        from gadget_state import state_value

        udc = state_value("udc")
        try:
            with open(f"/sys/class/udc/{udc}/current_speed") as handle:
                return 512 if handle.read().strip() in ("high-speed", "super-speed") else 64
        except OSError:
            return 64

    def write(self, number, data):
        while True:
            try:
                return os.write(self.open_endpoint(number), data)
            except OSError:
                time.sleep(0.05)

    # A HALT, THE WAY FUNCTIONFS MAKES ONE: I/O in the wrong direction on an endpoint file halts that endpoint and
    # fails with EBADMSG. The host then meets STALL on the endpoint until it clears the halt, and a transfer this
    # side queues meanwhile waits for that. Nothing may be queued on an IN endpoint when it is halted.
    def halt(self, number, inbound):
        try:
            if inbound:
                os.read(self.open_endpoint(number), 1)
            else:
                os.write(self.open_endpoint(number), b"\0")
        except OSError as error:
            return error.errno == errno.EBADMSG
        return False

    def serve(self):
        ep0 = os.open(os.path.join(self.mount, "ep0"), os.O_RDWR)
        os.write(ep0, self.blob())
        os.write(ep0, struct.pack("<IIII", FUNCTIONFS_STRINGS_MAGIC, 16, 0, 0))
        say(self.name, "descriptors written")
        # THE SIGN THE SETUP WAITS FOR, for a function with no endpoint files to appear: a DFU target has none.
        if self.ready:
            with open(self.ready, "w") as handle:
                handle.write("ready\n")
        started = False
        while True:
            events = os.read(ep0, EVENT_SIZE * 4)
            for at in range(0, len(events), EVENT_SIZE):
                event = events[at:at + EVENT_SIZE]
                kind = event[8]
                if kind == EVENT_ENABLE:
                    self.enabled.set()
                    if not started:
                        started = True
                        threading.Thread(target=self.run_endpoints, daemon=True).start()
                elif kind == EVENT_DISABLE:
                    self.enabled.clear()
                elif kind == EVENT_SETUP:
                    request_type, request, value, index, length = struct.unpack("<BBHHH", event[:8])
                    if request_type & 0x80:
                        answer = self.setup(request_type, request, value, index, length, b"")
                        if answer is None:
                            os.read(ep0, 0)
                        else:
                            os.write(ep0, bytes(answer)[:length])
                    elif self.accepts(request_type, request, value, index, length):
                        # A READ TAKES THE DATA STAGE AND ACKNOWLEDGES IT - of zero bytes when there is none.
                        data = os.read(ep0, length)
                        self.setup(request_type, request, value, index, length, data)
                    else:
                        # A WRITE WHILE AN OUT REQUEST IS PENDING IS HOW FUNCTIONFS STALLS ONE.
                        try:
                            os.write(ep0, b"")
                        except OSError:
                            pass


# ------------------------------------------------------------------ a CCID reader with a PIV card in it


def piv_atr():
    body = bytes([0x3B, 0x88, 0x80, 0x01]) + b"LIBERPIV"
    tck = 0
    for byte in body[1:]:
        tck ^= byte
    return body + bytes([tck])


PIV_AID = bytes([0xA0, 0x00, 0x00, 0x03, 0x08, 0x00, 0x00, 0x10, 0x00, 0x01, 0x00])
# The card holder unique identifier the oracle expects back, in its data object: a FASC-N-shaped run of known
# bytes, which is all the oracle compares.
CHUID = bytes([0x53, 0x19, 0x30, 0x17]) + bytes(range(0x10, 0x27))
READER_CLASS = bytes(
    [54, 0x21, 0x10, 0x01, 0x00, 0x07, 0x03, 0x00, 0x00, 0x00, 0xA0, 0x0F, 0x00, 0x00, 0xA0, 0x0F, 0x00, 0x00, 0x00, 0x80, 0x25, 0x00, 0x00, 0x80, 0x25, 0x00, 0x00, 0x00, 0xFE, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xBA, 0x04, 0x02, 0x00, 0x0F, 0x01, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x01]
)


class Ccid(Function):
    """A one-slot CCID reader holding a PIV card, as `usb_ccid_*` in the kernel's hardware suite expects it.

    After the host powers the card OFF following an exchange, the card is pulled out and, a second later, put
    back: two slot-change notifications, which the oracle reads as the card leaving and a new card arriving.
    """

    name = "ccid"

    def __init__(self, mount, ready=None):
        super().__init__(mount, ready)
        self.present = True
        self.powered = False
        self.exchanged = False
        self.notify_lock = threading.Lock()

    def descriptors(self):
        def one(bulk, interval):
            return [interface(0, 3, 0x0B, 0, 0), READER_CLASS, endpoint(0x01, 0x02, bulk, 0), endpoint(0x82, 0x02, bulk, 0), endpoint(0x83, 0x03, 8, interval)]

        return one(64, 16), one(512, 8)

    # ABORT (class, interface, OUT) is taken; the bulk Abort that follows is what answers it.
    def accepts(self, request_type, request, value, index, length):
        return request_type == 0x21 and request == 0x01

    def setup(self, request_type, request, value, index, length, data):
        say(self.name, f"ABORT slot {value & 0xff} seq {value >> 8}")
        return None

    def notify(self, present):
        with self.notify_lock:
            self.write(3, bytes([0x50, (0b10 | int(present))]))

    def status(self):
        if not self.present:
            return 0x02
        return 0x00 if self.powered else 0x01

    def reply(self, kind, slot, seq, status, error, specific, data=b""):
        return bytes([kind]) + struct.pack("<I", len(data)) + bytes([slot, seq, status, error, specific]) + data

    def apdu(self, command):
        if len(command) < 4:
            return bytes([0x67, 0x00])
        cla, ins, p1, p2 = command[:4]
        if ins == 0xA4 and p1 == 0x04:
            aid = command[5:5 + command[4]] if len(command) > 5 else b""
            if PIV_AID.startswith(aid) and aid:
                template = bytes([0x61, 0x11, 0x4F, 0x06, 0x00, 0x00, 0x10, 0x00, 0x01, 0x00, 0x79, 0x07, 0x4F, 0x05]) + PIV_AID[:5]
                return template + bytes([0x90, 0x00])
            return bytes([0x6A, 0x82])
        if ins == 0xCB and p1 == 0x3F and p2 == 0xFF:
            tag = command[5:5 + command[4]] if len(command) > 5 else b""
            if tag == bytes([0x5C, 0x03, 0x5F, 0xC1, 0x02]):
                return CHUID + bytes([0x90, 0x00])
            return bytes([0x6A, 0x82])
        return bytes([0x6D, 0x00])

    def message(self, request):
        kind = request[0]
        length = struct.unpack("<I", request[1:5])[0]
        slot, seq = request[5], request[6]
        data = request[10:10 + length]
        if slot != 0:
            return self.reply(0x81, slot, seq, 0x42, 5, 0)
        if kind == 0x62:
            if not self.present:
                return self.reply(0x80, slot, seq, 0x42, 0xFE, 0)
            self.powered = True
            say(self.name, "card powered on")
            return self.reply(0x80, slot, seq, 0x00, 0, 0, piv_atr())
        if kind == 0x63:
            was = self.powered
            self.powered = False
            say(self.name, "card powered off")
            if was and self.exchanged:
                threading.Thread(target=self.pull_and_replace, daemon=True).start()
            return self.reply(0x81, slot, seq, self.status(), 0, 0)
        if kind == 0x65:
            return self.reply(0x81, slot, seq, self.status(), 0, 0)
        if kind == 0x61:
            return self.reply(0x82, slot, seq, self.status(), 0, request[7], data)
        if kind == 0x6F:
            if not self.present or not self.powered:
                return self.reply(0x80, slot, seq, 0x40 | self.status(), 0xFE, 0)
            self.exchanged = True
            answer = self.apdu(data)
            say(self.name, f"APDU {data[:4].hex(' ')} -> {answer[-2:].hex(' ')}")
            return self.reply(0x80, slot, seq, 0x00, 0, 0, answer)
        if kind == 0x72:
            return self.reply(0x81, slot, seq, self.status(), 0, 0)
        return self.reply(0x81, slot, seq, 0x40 | self.status(), 0, 0)

    def pull_and_replace(self):
        time.sleep(0.3)
        self.present = False
        self.exchanged = False
        self.notify(False)
        say(self.name, "card pulled out")
        time.sleep(1.0)
        self.present = True
        self.notify(True)
        say(self.name, "card put back")

    def run_endpoints(self):
        threading.Thread(target=self.notify, args=(True,), daemon=True).start()
        held = bytearray()
        while True:
            held.extend(self.read(1, self.packet_size()))
            while len(held) >= 10:
                length = 10 + struct.unpack("<I", held[1:5])[0]
                if length > 10 + 4096:
                    held.clear()
                    break
                if len(held) < length:
                    break
                request = bytes(held[:length])
                del held[:length]
                self.write(2, self.message(request))


# ------------------------------------------------------------------ a DFU target in DFU mode


def firmware(seed, length):
    return bytes((n * 7 + seed * 13 + (n >> 8)) & 0xFF for n in range(length))


# THE ORACLE'S IMAGE - `usb_dfu_*` in the kernel's hardware suite downloads exactly this, and a device that
# receives anything else fails its manifestation with errVERIFY.
EXPECTED = hashlib.sha256(firmware(5, 3000)).digest()

DFU_IDLE, DFU_DNLOAD_SYNC, DFU_DNLOAD_IDLE, DFU_MANIFEST_SYNC, DFU_MANIFEST, DFU_ERROR = 2, 3, 5, 6, 7, 10
ERR_VERIFY, ERR_ADDRESS, ERR_NOTDONE = 0x07, 0x08, 0x09


class Dfu(Function):
    """A DFU 1.1 target already in DFU mode: download-capable, manifestation-tolerant, 1024-byte blocks.

    It keeps the blocks it is given, in order, and at manifestation compares their SHA-256 with the oracle's image:
    the same image manifests and the device goes back to idle; anything else is errVERIFY. So a download the guest
    reports as completed is one whose every byte arrived.
    """

    name = "dfu"

    def __init__(self, mount, ready=None):
        super().__init__(mount, ready)
        self.state = DFU_IDLE
        self.status = 0
        self.image = bytearray()
        self.block = 0

    def descriptors(self):
        functional = bytes([9, 0x21, 0x05, 0xFF, 0x00]) + struct.pack("<H", 1024) + struct.pack("<H", 0x0110)
        one = [interface(0, 0, 0xFE, 0x01, 0x02), functional]
        return one, list(one)

    def reset(self):
        self.image = bytearray()
        self.block = 0

    def accepts(self, request_type, request, value, index, length):
        # DNLOAD, CLRSTATUS and ABORT: class requests to the interface.
        return request_type == 0x21 and request in (1, 4, 6)

    def setup(self, request_type, request, value, index, length, data):
        if request_type == 0x21:
            if request == 1:
                self.download(value, data)
            elif request == 4:
                if self.state == DFU_ERROR:
                    self.state, self.status = DFU_IDLE, 0
                    self.reset()
            elif request == 6:
                self.state, self.status = DFU_IDLE, 0
                self.reset()
            return None
        if request_type == 0xA1 and request == 3:
            return self.get_status()
        if request_type == 0xA1 and request == 5:
            return bytes([self.state])
        return None

    def download(self, block, data):
        if self.state not in (DFU_IDLE, DFU_DNLOAD_IDLE):
            self.state, self.status = DFU_ERROR, ERR_NOTDONE
            return
        if not data:
            if self.state != DFU_DNLOAD_IDLE:
                self.state, self.status = DFU_ERROR, ERR_NOTDONE
                return
            self.state = DFU_MANIFEST_SYNC
            return
        if block != self.block:
            self.state, self.status = DFU_ERROR, ERR_ADDRESS
            return
        self.image.extend(data)
        self.block += 1
        self.state = DFU_DNLOAD_SYNC

    def get_status(self):
        poll = 0
        if self.state == DFU_DNLOAD_SYNC:
            self.state, poll = DFU_DNLOAD_IDLE, 10
        elif self.state == DFU_MANIFEST_SYNC:
            digest = hashlib.sha256(bytes(self.image)).digest()
            if digest == EXPECTED:
                say(self.name, f"{len(self.image)} bytes in {self.block} block(s), the expected image - manifesting")
                self.state, poll = DFU_MANIFEST, 50
            else:
                say(self.name, f"{len(self.image)} bytes in {self.block} block(s), NOT the expected image - errVERIFY")
                self.state, self.status = DFU_ERROR, ERR_VERIFY
            self.reset()
        elif self.state == DFU_MANIFEST:
            say(self.name, "manifested")
            self.state = DFU_IDLE
        return bytes([self.status]) + struct.pack("<I", poll)[:3] + bytes([self.state, 0])


# ------------------------------------------------------------------ a Bluetooth controller's HCI transport

BD_ADDR = bytes([0x02, 0x00, 0x00, 0xEE, 0xFF, 0xC0])


# The ACL handle whose packets make the controller halt its bulk pair, one way and then the other.
STALL_PROBE_HANDLE = 0x0EE
# A vendor command that makes the controller leave the bus: the emulator exits, which closes the function, and
# FunctionFS unbinds a gadget whose function closed - to the guest, the dongle pulled out.
UNPLUG_OPCODE = 0xFC99


class Bluetooth(Function):
    """A controller's primary interface: commands on the control pipe, events on the interrupt pipe, ACL data
    looped back on the bulk pair with a Number Of Completed Packets event for each packet taken.

    It answers the handful of commands the oracle sends - Reset, Read BD_ADDR, Read Local Version, Read Local
    Supported Commands (whose 70-byte completion spans several interrupt packets) and LE Read Buffer Size - and
    every other one with Unknown HCI Command, which is also what the host's own stack meets when this device
    appears on the host's bus before the guest takes it. A packet on `STALL_PROBE_HANDLE` makes it halt its bulk
    IN before looping that packet back and its bulk OUT after, so the host has to recover each pipe once; and
    the vendor command `UNPLUG_OPCODE` makes it leave the bus.
    """

    name = "bt"
    flags = ALL_CTRL_RECIP

    def __init__(self, mount, ready=None):
        super().__init__(mount, ready)
        import queue

        self.events = queue.Queue()

    def descriptors(self):
        def one(bulk, interval):
            return [interface(0, 3, 0xE0, 0x01, 0x01), endpoint(0x81, 0x03, 16, interval), endpoint(0x82, 0x02, bulk, 0), endpoint(0x02, 0x02, bulk, 0)]

        return one(64, 1), one(512, 4)

    # A command: class, host to device, the DEVICE recipient - which FunctionFS hands over only because this
    # function asked for every recipient.
    def accepts(self, request_type, request, value, index, length):
        return request_type == 0x20 and request == 0x00

    def complete(self, opcode, status, parameters=b""):
        body = bytes([1]) + struct.pack("<H", opcode) + bytes([status]) + parameters
        self.events.put(bytes([0x0E, len(body)]) + body)

    def setup(self, request_type, request, value, index, length, data):
        if len(data) < 3:
            return None
        opcode = struct.unpack("<H", data[:2])[0]
        if opcode == 0x0C03:
            self.complete(opcode, 0)
        elif opcode == 0x1009:
            self.complete(opcode, 0, BD_ADDR)
        elif opcode == 0x1001:
            self.complete(opcode, 0, bytes([0x0C, 0x01, 0x00, 0x0C, 0xFF, 0xFF, 0x01, 0x00]))
        elif opcode == 0x1002:
            self.complete(opcode, 0, bytes(range(64)))
        elif opcode == 0x2002:
            self.complete(opcode, 0, struct.pack("<HB", 251, 8))
        elif opcode == UNPLUG_OPCODE:
            say(self.name, "told to leave the bus - the function closes and the gadget goes with it")
            threading.Thread(target=self.leave, daemon=True).start()
            return None
        else:
            self.complete(opcode, 0x01)
        say(self.name, f"command {opcode:#06x}")
        return None

    def send_events(self):
        while True:
            self.write(1, self.events.get())

    # Once the command that asked for it has been acknowledged: every file closed at once, as a pulled cable does.
    def leave(self):
        time.sleep(0.2)
        os._exit(0)

    def run_endpoints(self):
        threading.Thread(target=self.send_events, daemon=True).start()
        held = bytearray()
        while True:
            held.extend(self.read(3, self.packet_size()))
            while len(held) >= 4:
                length = 4 + struct.unpack("<H", held[2:4])[0]
                if len(held) < length:
                    break
                packet = bytes(held[:length])
                del held[:length]
                handle = struct.unpack("<H", packet[:2])[0] & 0x0FFF
                # THE STALL PROBE: a packet on this handle is looped back through a bulk IN halted first, and the
                # bulk OUT is halted after it - before its completion event, so the host cannot send again before
                # the halt is there to meet it.
                probe = handle == STALL_PROBE_HANDLE
                if probe:
                    say(self.name, f"bulk IN halted: {self.halt(2, True)}")
                self.write(2, packet)
                if probe:
                    say(self.name, f"bulk OUT halted: {self.halt(3, False)}")
                self.events.put(bytes([0x13, 5, 1]) + struct.pack("<HH", handle, 1))
                say(self.name, f"ACL {len(packet)} bytes on handle {handle:#05x} looped back")


EMULATORS = {"bt": Bluetooth, "ccid": Ccid, "dfu": Dfu}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--emulate", required=True, choices=sorted(EMULATORS))
    parser.add_argument("--mount", required=True)
    parser.add_argument("--ready", help="a file to write once the descriptors are written")
    args = parser.parse_args()
    function = EMULATORS[args.emulate](args.mount, args.ready)
    try:
        function.serve()
    except OSError as error:
        say(function.name, f"stopped: {error}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
