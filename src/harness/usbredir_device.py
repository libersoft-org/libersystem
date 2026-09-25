#!/usr/bin/env python3
"""USB devices played over QEMU's `usb-redir`: the usbredir protocol's usb-host side, in one process.

WHY THIS AND NOT A GADGET. A gadget on `dummy_hcd` is a device the host's own kernel plays, and `dummy_hcd` has
no isochronous endpoints - so an audio source, the one device the audio driver still lacked, cannot be built
that way at all (`usb_f_uac2` refuses to bind to it). QEMU's `usb-redir` tunnels a USB device over a socket
in the usbredir protocol, and whatever answers on the other end of that socket IS the device as far as the
guest can tell: its descriptors, its control transfers, its isochronous packets. This program is that other
end. It loads no module, writes no configfs and binds no controller; what it leaves in the host is a socket in
a directory the run made.

THE PROTOCOL, version 0.7 (spice `usbredir`, `docs/usb-redirection-protocol.md`), as this side speaks it:
  - every packet is a header - type, length of what follows, id - and a body; integers little-endian, structs
    packed. Both sides say hello first, with their capabilities, in a header whose id is 32 bits; every header
    after the hellos has a 64-bit id, because both sides offer it and QEMU requires it on an xHCI port;
  - the device is announced by `ep_info`, then `interface_info`, then `device_connect`, in that order - and
    `ep_info` and `interface_info` again, before the status, after every configuration or alternate setting,
    because they say which endpoints exist NOW;
  - QEMU answers SET_ADDRESS itself and turns SET/GET_CONFIGURATION and SET/GET_INTERFACE into packets of their
    own; every other control request arrives as a control packet and is answered with the same id;
  - an isochronous IN stream starts when QEMU asks for it, and from then its packets are SENT, unasked, one per
    service interval, their ids counting up from zero; QEMU buffers them and hands them to the guest's
    transfers.

THE DEVICES, chosen with `--emulate`:
  mic         a UAC1 microphone: 48 kHz, stereo, 16-bit, 192 bytes every millisecond on isochronous IN 0x81. Its
              samples are a counter - the left channel is the frame's number, the right its complement - so every
              byte a guest captures says where in the stream it came from.
  dfu-reset   a DFU 1.1 target that starts in RUNTIME mode and waits, after DFU_DETACH, for the host to reset it -
              and comes back from that reset in DFU mode as another product with the same serial number, which is
              the device a gadget cannot play: it changes what it is without leaving the bus.
  dfu-detach  the same target with `bitWillDetach`: after DFU_DETACH it leaves the bus by itself and comes back.
"""

import argparse
import hashlib
import os
import select
import socket
import struct
import sys
import time

HELLO, DEVICE_CONNECT, DEVICE_DISCONNECT, RESET, INTERFACE_INFO, EP_INFO = 0, 1, 2, 3, 4, 5
SET_CONFIGURATION, GET_CONFIGURATION, CONFIGURATION_STATUS = 6, 7, 8
SET_ALT_SETTING, GET_ALT_SETTING, ALT_SETTING_STATUS = 9, 10, 11
START_ISO_STREAM, STOP_ISO_STREAM, ISO_STREAM_STATUS = 12, 13, 14
START_INTERRUPT_RECEIVING, STOP_INTERRUPT_RECEIVING, INTERRUPT_RECEIVING_STATUS = 15, 16, 17
CANCEL_DATA_PACKET = 21
FILTER_REJECT, FILTER_FILTER, DEVICE_DISCONNECT_ACK = 22, 23, 24
CONTROL_PACKET, BULK_PACKET, ISO_PACKET, INTERRUPT_PACKET = 100, 101, 102, 103

SUCCESS, CANCELLED, INVAL, IOERROR, STALL = 0, 1, 2, 3, 4
TYPE_CONTROL, TYPE_ISO, TYPE_BULK, TYPE_INTERRUPT, TYPE_INVALID = 0, 1, 2, 3, 255
SPEED_FULL, SPEED_HIGH = 1, 2

CAP_CONNECT_DEVICE_VERSION, CAP_DEVICE_DISCONNECT_ACK, CAP_EP_INFO_MAX_PACKET_SIZE = 1, 3, 4
CAP_64BITS_IDS, CAP_32BITS_BULK_LENGTH = 5, 6
# QEMU REFUSES A DEVICE ON AN xHCI PORT whose far side lacks the last three: "usb-redir-host lacks capabilities
# needed for use with XHCI". Sixty-four-bit ids change every header after the hellos; the long bulk length changes
# only bulk packets, which no device here has.
OUR_CAPS = 1 << CAP_CONNECT_DEVICE_VERSION | 1 << CAP_DEVICE_DISCONNECT_ACK | 1 << CAP_EP_INFO_MAX_PACKET_SIZE | 1 << CAP_64BITS_IDS | 1 << CAP_32BITS_BULK_LENGTH

VENDOR_ID, PRODUCT_ID = 0x1D6B, 0x0104
DFU_PRODUCT_ID = 0x0105
DT_DEVICE, DT_CONFIG, DT_STRING, DT_INTERFACE, DT_ENDPOINT = 1, 2, 3, 4, 5
REQ_GET_STATUS, REQ_CLEAR_FEATURE, REQ_SET_FEATURE, REQ_GET_DESCRIPTOR = 0, 1, 3, 6


def say(name, message):
    print(f"usbredir {name}: {message}", file=sys.stderr, flush=True)


def string_descriptor(text):
    encoded = text.encode("utf-16-le")
    return bytes([2 + len(encoded), DT_STRING]) + encoded


def settings_of(configuration):
    """Every interface setting of a configuration descriptor: (number, alternate, class, subclass, protocol,
    [(address, attributes, max_packet, interval)])."""
    settings = []
    at = 0
    while at + 2 <= len(configuration):
        length, kind = configuration[at], configuration[at + 1]
        if length < 2:
            break
        record = configuration[at:at + length]
        if kind == DT_INTERFACE and length >= 9:
            settings.append((record[2], record[3], record[5], record[6], record[7], []))
        elif kind == DT_ENDPOINT and length >= 7 and settings:
            settings[-1][5].append((record[2], record[3], struct.unpack("<H", record[4:6])[0], record[6]))
        at += length
    return settings


class Device:
    """What a device model gives the protocol: descriptors, its state, its class requests and its stream data."""

    name = "device"
    speed = SPEED_FULL
    device_class = (0, 0, 0)
    bcd_device = 0x0100
    product = PRODUCT_ID
    strings = {1: "LiberSystem harness", 2: "device", 3: "LIBER0001"}

    def __init__(self):
        self.configuration = 0
        self.alternates = {}
        # Set by a model that has to leave the bus (and, if `returns`, come back as what it is then).
        self.leaving = None

    def device_descriptor(self):
        cls, sub, proto = self.device_class
        return struct.pack("<BBHBBBBHHHBBBB", 18, DT_DEVICE, 0x0110 if self.speed == SPEED_FULL else 0x0200, cls, sub, proto, 64, VENDOR_ID, self.product, self.bcd_device, 1, 2, 3, 1)

    def configuration_descriptor(self):
        raise NotImplementedError

    # The settings the device is in now: each interface's current alternate, with its endpoints.
    def current(self):
        if self.configuration == 0:
            return []
        return [setting for setting in settings_of(self.configuration_descriptor()) if setting[1] == self.alternates.get(setting[0], 0)]

    def has_setting(self, interface, alternate):
        return any(setting[0] == interface and setting[1] == alternate for setting in settings_of(self.configuration_descriptor()))

    # A class or vendor request: (status, bytes). Stalled unless a model answers it.
    def control(self, request_type, request, value, index, length, data):
        return STALL, b""

    def on_configuration(self, value):
        pass

    def on_alternate(self, interface, alternate):
        pass

    def on_reset(self):
        pass

    # One isochronous IN packet's data, for the stream on `address`.
    def iso_in(self, address):
        return b""

    def iso_started(self, address):
        pass

    def iso_stopped(self, address):
        pass


class Microphone(Device):
    """A UAC1 microphone: an audio-control interface with a microphone input terminal and a USB-streaming output
    terminal, and a streaming interface whose alternate 1 carries isochronous IN 0x81 - 192 bytes a millisecond,
    48 kHz, stereo, 16-bit. Frame n is left = n, right = the complement of n, both sixteen bits."""

    name = "mic"
    strings = {1: "LiberSystem harness", 2: "microphone", 3: "LIBERMIC0001"}
    FRAMES_PER_PACKET = 48
    PACKET = FRAMES_PER_PACKET * 4

    def __init__(self):
        super().__init__()
        self.frame = 0
        self.packets = 0

    def configuration_descriptor(self):
        header = bytes([9, 0x24, 0x01, 0x00, 0x01]) + struct.pack("<H", 9 + 12 + 9) + bytes([1, 1])
        microphone = bytes([12, 0x24, 0x02, 1]) + struct.pack("<H", 0x0201) + bytes([0, 2]) + struct.pack("<H", 0x0003) + bytes([0, 0])
        streaming = bytes([9, 0x24, 0x03, 2]) + struct.pack("<H", 0x0101) + bytes([0, 1, 0])
        control = bytes([9, DT_INTERFACE, 0, 0, 0, 1, 1, 0, 0]) + header + microphone + streaming
        idle = bytes([9, DT_INTERFACE, 1, 0, 0, 1, 2, 0, 0])
        active = bytes([9, DT_INTERFACE, 1, 1, 1, 1, 2, 0, 0])
        general = bytes([7, 0x24, 0x01, 2, 1]) + struct.pack("<H", 0x0001)
        format_one = bytes([11, 0x24, 0x02, 0x01, 2, 2, 16, 1]) + (48000).to_bytes(3, "little")
        # Isochronous, asynchronous: the device's own clock paces what it sends, and a source needs no feedback.
        endpoint = bytes([9, DT_ENDPOINT, 0x81, 0x05]) + struct.pack("<H", self.PACKET) + bytes([1, 0, 0])
        endpoint_general = bytes([7, 0x25, 0x01, 0x00, 0x00]) + struct.pack("<H", 0)
        body = control + idle + active + general + format_one + endpoint + endpoint_general
        return bytes([9, DT_CONFIG]) + struct.pack("<H", 9 + len(body)) + bytes([2, 1, 0, 0x80, 50]) + body

    # The sampling frequency, on the endpoint: SET_CUR is taken if it is the one rate there is, GET_CUR answers it.
    def control(self, request_type, request, value, index, length, data):
        if request_type == 0x22 and request == 0x01 and value >> 8 == 0x01:
            return (SUCCESS, b"") if int.from_bytes(data[:3], "little") == 48000 else (STALL, b"")
        if request_type == 0xA2 and request == 0x81 and value >> 8 == 0x01:
            return SUCCESS, (48000).to_bytes(3, "little")
        return STALL, b""

    def iso_started(self, address):
        say(self.name, f"streaming from frame {self.frame}")

    def iso_stopped(self, address):
        say(self.name, f"stopped after {self.packets} packets, at frame {self.frame}")

    def iso_in(self, address):
        out = bytearray()
        for _ in range(self.FRAMES_PER_PACKET):
            n = self.frame & 0xFFFF
            out += struct.pack("<HH", n, n ^ 0xFFFF)
            self.frame += 1
        self.packets += 1
        return bytes(out)


def firmware(seed, length):
    """The oracle's images, by the formula `usb_ffs.py` and the kernel's hardware suite share."""
    return bytes((n * 7 + seed * 13 + (n >> 8)) & 0xFF for n in range(length))


# THE IMAGE THIS TARGET VERIFIES AT MANIFESTATION: anything else is errVERIFY.
DFU_EXPECTED = hashlib.sha256(firmware(5, 3000)).digest()
# AN IMAGE THAT BEGINS WITH THIS makes the target leave the bus after its first block, for good.
DFU_UNPLUG = b"UNPLUG"
DFU_IDLE, DFU_DNLOAD_SYNC, DFU_DNLOAD_IDLE, DFU_MANIFEST_SYNC, DFU_MANIFEST, DFU_ERROR = 2, 3, 5, 6, 7, 10
APP_IDLE, APP_DETACH = 0, 1
ERR_VERIFY, ERR_ADDRESS, ERR_NOTDONE = 0x07, 0x08, 0x09


class Dfu(Device):
    """A firmware target in RUNTIME mode that DFU_DETACH turns into a DFU-mode device: product 0x0105 instead of
    0x0104, the same serial number, a DFU-mode interface that takes 1024-byte blocks and verifies the image at
    manifestation, manifestation-tolerant - it stays in DFU mode, idle, after it.

    `will_detach` says which way the mode changes. Without it the target waits for the host's bus reset after the
    detach and changes AT the reset, without leaving the bus - QEMU passes the reset on as a `reset` packet. With it
    the target leaves the bus by itself and comes back.
    """

    strings = {1: "LiberSystem harness", 2: "firmware target", 3: "LIBERDFU0001"}

    def __init__(self, will_detach):
        super().__init__()
        self.will_detach = will_detach
        self.name = "dfu-detach" if will_detach else "dfu-reset"
        self.mode_dfu = False
        self.detach_requested = False
        self.state = APP_IDLE
        self.status = 0
        self.image = bytearray()
        self.block = 0

    @property
    def product(self):
        return DFU_PRODUCT_ID if self.mode_dfu else PRODUCT_ID

    def configuration_descriptor(self):
        protocol = 0x02 if self.mode_dfu else 0x01
        # bmAttributes: download-capable and manifestation-tolerant, and in runtime mode whether it leaves by itself.
        attributes = 0x01 | 0x04 | (0x08 if self.will_detach and not self.mode_dfu else 0)
        functional = bytes([9, 0x21, attributes]) + struct.pack("<HHH", 500, 1024, 0x0110)
        body = bytes([9, DT_INTERFACE, 0, 0, 0, 0xFE, 0x01, protocol, 0]) + functional
        return bytes([9, DT_CONFIG]) + struct.pack("<H", 9 + len(body)) + bytes([1, 1, 0, 0x80, 50]) + body

    def become_dfu(self, how):
        self.mode_dfu = True
        self.detach_requested = False
        self.state, self.status = DFU_IDLE, 0
        self.image, self.block = bytearray(), 0
        say(self.name, f"now in DFU mode as 1d6b:{DFU_PRODUCT_ID:04x}, {how}")

    def on_reset(self):
        # THE RESET A DETACHED TARGET WAITS FOR is the moment it becomes the other device.
        if self.detach_requested and not self.mode_dfu:
            self.become_dfu("after the host's reset")

    def control(self, request_type, request, value, index, length, data):
        if not self.mode_dfu:
            if request_type == 0x21 and request == 0:
                self.detach_requested = True
                self.state = APP_DETACH
                say(self.name, f"DFU_DETACH, the host allows {value} ms")
                if self.will_detach:
                    self.leaving = "return"
                return SUCCESS, b""
            if request_type == 0xA1 and request == 3:
                return SUCCESS, bytes([0, 0, 0, 0, self.state, 0])
            if request_type == 0xA1 and request == 5:
                return SUCCESS, bytes([self.state])
            return STALL, b""
        if request_type == 0x21 and request == 1:
            self.download(value, data)
            return SUCCESS, b""
        if request_type == 0x21 and request == 4:
            if self.state == DFU_ERROR:
                self.state, self.status = DFU_IDLE, 0
                self.image, self.block = bytearray(), 0
            return SUCCESS, b""
        if request_type == 0x21 and request == 6:
            self.state, self.status = DFU_IDLE, 0
            self.image, self.block = bytearray(), 0
            return SUCCESS, b""
        if request_type == 0xA1 and request == 3:
            return SUCCESS, self.get_status()
        if request_type == 0xA1 and request == 5:
            return SUCCESS, bytes([self.state])
        return STALL, b""

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
        if block == 0 and bytes(data).startswith(DFU_UNPLUG):
            say(self.name, "an image that says UNPLUG - leaving the bus in the middle of its download")
            self.leaving = "gone"

    def get_status(self):
        poll = 0
        if self.state == DFU_DNLOAD_SYNC:
            self.state, poll = DFU_DNLOAD_IDLE, 10
        elif self.state == DFU_MANIFEST_SYNC:
            digest = hashlib.sha256(bytes(self.image)).digest()
            if digest == DFU_EXPECTED:
                say(self.name, f"{len(self.image)} bytes in {self.block} block(s), the expected image - manifesting")
                self.state, poll = DFU_MANIFEST, 50
            else:
                say(self.name, f"{len(self.image)} bytes in {self.block} block(s), NOT the expected image - errVERIFY")
                self.state, self.status = DFU_ERROR, ERR_VERIFY
            self.image, self.block = bytearray(), 0
        elif self.state == DFU_MANIFEST:
            say(self.name, "manifested")
            self.state = DFU_IDLE
        return bytes([self.status]) + struct.pack("<I", poll)[:3] + bytes([self.state, 0])


class Redirection:
    """The protocol over one connection, for one device."""

    def __init__(self, connection, device):
        self.connection = connection
        self.device = device
        self.received = bytearray()
        self.peer_caps = 0
        self.greeted = False
        # Whether headers carry 64-bit ids: never the hellos', and every other one once both sides offered them.
        self.id64 = False
        # Isochronous IN streams: address -> [interval in seconds, when the next packet is due, packets sent].
        self.streams = {}
        # When a device that left the bus by itself comes back, if it is going to.
        self.returning = None

    def send(self, kind, ident, body=b""):
        header = struct.pack("<IIQ", kind, len(body), ident) if self.id64 else struct.pack("<III", kind, len(body), ident)
        self.connection.sendall(header + body)

    def hello(self):
        self.send(HELLO, 0, b"liber usbredir device 1".ljust(64, b"\0") + struct.pack("<I", OUR_CAPS))

    def both(self, cap):
        return bool(self.peer_caps & (1 << cap) and OUR_CAPS & (1 << cap))

    # WHAT EXISTS NOW: endpoint 0 both ways, and every endpoint of every interface's current setting. The index is
    # the endpoint's number for OUT and sixteen more for IN.
    def ep_info(self):
        types, intervals, interfaces, packets = [TYPE_INVALID] * 32, [0] * 32, [0] * 32, [0] * 32
        types[0] = types[16] = TYPE_CONTROL
        packets[0] = packets[16] = 64
        for number, _alternate, _cls, _sub, _proto, endpoints in self.device.current():
            for address, attributes, packet, interval in endpoints:
                index = (address & 0x0F) + (16 if address & 0x80 else 0)
                types[index] = attributes & 0x03
                intervals[index] = max(interval, 1)
                interfaces[index] = number
                packets[index] = packet & 0x07FF
        body = bytes(types) + bytes(intervals) + bytes(interfaces)
        if self.both(CAP_EP_INFO_MAX_PACKET_SIZE):
            body += struct.pack("<32H", *packets)
        self.send(EP_INFO, 0, body)

    def interface_info(self):
        settings = self.device.current() or [s for s in settings_of(self.device.configuration_descriptor()) if s[1] == 0]
        numbers = [s[0] for s in settings][:32]
        pad = lambda values: bytes(values + [0] * (32 - len(values)))
        self.send(INTERFACE_INFO, 0, struct.pack("<I", len(numbers)) + pad(numbers) + pad([s[2] for s in settings][:32]) + pad([s[3] for s in settings][:32]) + pad([s[4] for s in settings][:32]))

    def connect(self):
        self.ep_info()
        self.interface_info()
        cls, sub, proto = self.device.device_class
        body = struct.pack("<BBBBHH", self.device.speed, cls, sub, proto, VENDOR_ID, self.device.product)
        if self.both(CAP_CONNECT_DEVICE_VERSION):
            body += struct.pack("<H", self.device.bcd_device)
        self.send(DEVICE_CONNECT, 0, body)
        say(self.device.name, "connected")

    # THE STANDARD REQUESTS QEMU PASSES ON: descriptors, status and features. Everything else is the model's.
    def standard(self, request_type, request, value, index, length, data):
        if request_type == 0x80 and request == REQ_GET_DESCRIPTOR:
            kind, number = value >> 8, value & 0xFF
            if kind == DT_DEVICE:
                return SUCCESS, self.device.device_descriptor()
            if kind == DT_CONFIG and number == 0:
                return SUCCESS, self.device.configuration_descriptor()
            if kind == DT_STRING:
                if number == 0:
                    return SUCCESS, bytes([4, DT_STRING, 0x09, 0x04])
                text = self.device.strings.get(number)
                return (SUCCESS, string_descriptor(text)) if text is not None else (STALL, b"")
            # A full-speed device has no device qualifier, and says so by stalling the request for one.
            return STALL, b""
        if request_type & 0x60 == 0 and request == REQ_GET_STATUS:
            return SUCCESS, b"\0\0"
        if request_type & 0x60 == 0 and request in (REQ_CLEAR_FEATURE, REQ_SET_FEATURE):
            return SUCCESS, b""
        return None

    def control_packet(self, ident, body):
        endpoint, request, request_type, _status, value, index, length = struct.unpack("<BBBBHHH", body[:10])
        data = body[10:]
        answer = self.standard(request_type, request, value, index, length, data) if request_type & 0x60 == 0 else None
        status, reply = answer if answer is not None else self.device.control(request_type, request, value, index, length, data)
        if request_type & 0x80:
            reply = reply[:length]
            self.send(CONTROL_PACKET, ident, struct.pack("<BBBBHHH", endpoint, request, request_type, status, value, index, len(reply)) + reply)
        else:
            self.send(CONTROL_PACKET, ident, struct.pack("<BBBBHHH", endpoint, request, request_type, status, value, index, length if status == SUCCESS else 0))

    def stop_streams(self, interface=None):
        current = {address: number for number, _a, _c, _s, _p, endpoints in self.device.current() for address, *_ in endpoints}
        for address in list(self.streams):
            if interface is None or current.get(address) == interface or address not in current:
                del self.streams[address]
                self.device.iso_stopped(address)

    def handle(self, kind, ident, body):
        device = self.device
        if kind == HELLO:
            self.peer_caps = struct.unpack("<I", body[64:68])[0] if len(body) >= 68 else 0
            self.greeted = True
            self.id64 = self.both(CAP_64BITS_IDS)
            self.connect()
        elif kind == RESET:
            self.stop_streams()
            device.configuration = 0
            device.alternates = {}
            device.on_reset()
        elif kind == SET_CONFIGURATION:
            value = body[0]
            self.stop_streams()
            ok = value in (0, 1)
            if ok:
                device.configuration = value
                device.alternates = {}
                device.on_configuration(value)
            self.ep_info()
            self.interface_info()
            self.send(CONFIGURATION_STATUS, ident, bytes([SUCCESS if ok else STALL, device.configuration]))
        elif kind == GET_CONFIGURATION:
            self.send(CONFIGURATION_STATUS, ident, bytes([SUCCESS, device.configuration]))
        elif kind == SET_ALT_SETTING:
            interface, alternate = body[0], body[1]
            ok = device.configuration != 0 and device.has_setting(interface, alternate)
            if ok:
                self.stop_streams(interface)
                device.alternates[interface] = alternate
                device.on_alternate(interface, alternate)
            self.ep_info()
            self.interface_info()
            self.send(ALT_SETTING_STATUS, ident, bytes([SUCCESS if ok else STALL, interface, device.alternates.get(interface, 0)]))
        elif kind == GET_ALT_SETTING:
            interface = body[0]
            self.send(ALT_SETTING_STATUS, ident, bytes([SUCCESS, interface, device.alternates.get(interface, 0)]))
        elif kind == START_ISO_STREAM:
            address = body[0]
            interval = next((i for _n, _a, _c, _s, _p, endpoints in device.current() for a, attributes, _packet, i in endpoints if a == address and attributes & 3 == TYPE_ISO), None)
            if interval is None or not address & 0x80:
                self.send(ISO_STREAM_STATUS, ident, bytes([INVAL, address]))
                return
            # A full-speed interval is frames; a high-speed one is 2^(n-1) microframes.
            seconds = interval / 1000 if device.speed == SPEED_FULL else (1 << (interval - 1)) / 8000
            self.streams[address] = [seconds, time.monotonic(), 0]
            self.send(ISO_STREAM_STATUS, ident, bytes([SUCCESS, address]))
            device.iso_started(address)
        elif kind == STOP_ISO_STREAM:
            address = body[0]
            if self.streams.pop(address, None) is not None:
                device.iso_stopped(address)
            self.send(ISO_STREAM_STATUS, ident, bytes([SUCCESS, address]))
        elif kind == CONTROL_PACKET:
            self.control_packet(ident, body)
            # A MODEL THAT HAS TO LEAVE does so after its answer has gone: a detach is acknowledged before the
            # device drops off the bus, as a real one's is.
            if device.leaving is not None:
                self.leave()
        elif kind == DEVICE_DISCONNECT_ACK:
            if self.returning is not None:
                self.come_back()
        elif kind in (CANCEL_DATA_PACKET, FILTER_FILTER):
            # Nothing is ever left pending here - every data packet is answered as it arrives - and no filter
            # is kept, so these change nothing.
            pass
        elif kind in (START_INTERRUPT_RECEIVING, STOP_INTERRUPT_RECEIVING):
            self.send(INTERRUPT_RECEIVING_STATUS, ident, bytes([INVAL, body[0]]))
        else:
            say(device.name, f"a packet of type {kind} this device does not answer")

    # LEAVING THE BUS: the device is disconnected, and a model that comes back does so once the guest's side has
    # seen it go - its acknowledgment, or half a second for a side that does not send one.
    def leave(self):
        how = self.device.leaving
        self.device.leaving = None
        self.streams.clear()
        self.send(DEVICE_DISCONNECT, 0)
        if how == "return":
            say(self.device.name, "left the bus by itself after the detach")
            self.returning = time.monotonic() + 0.5
        else:
            say(self.device.name, "left the bus for good")

    def come_back(self):
        self.returning = None
        self.device.become_dfu("after leaving the bus by itself")
        self.device.configuration = 0
        self.device.alternates = {}
        self.connect()

    # THE ISOCHRONOUS IN STREAMS, PACED BY THE CLOCK: every packet due by now is sent, and none ahead of it. A
    # stream that fell behind catches up in bursts of at most a few packets a turn rather than skipping frames -
    # the counter in the samples must never jump, so late is the only way this side may be wrong.
    def pump(self):
        now = time.monotonic()
        for address, stream in self.streams.items():
            seconds, due, sent = stream
            burst = 0
            while due <= now and burst < 16:
                data = self.device.iso_in(address)
                self.send(ISO_PACKET, sent, struct.pack("<BBH", address, SUCCESS, len(data)) + data)
                sent += 1
                due += seconds
                burst += 1
            stream[1], stream[2] = due, sent

    def next_due(self):
        if not self.streams:
            return None
        return max(0.0, min(stream[1] for stream in self.streams.values()) - time.monotonic())

    def serve(self):
        self.hello()
        while True:
            if self.returning is not None and time.monotonic() >= self.returning:
                self.come_back()
            wait = self.next_due()
            ready, _, _ = select.select([self.connection], [], [], 0.25 if wait is None else min(wait, 0.25))
            if ready:
                chunk = self.connection.recv(65536)
                if not chunk:
                    say(self.device.name, "the guest's side closed the connection")
                    return 0
                self.received += chunk
                while True:
                    # The peer's hello is the first packet and has a 32-bit id; every packet after it has the width
                    # both sides agreed on.
                    size = 16 if self.id64 else 12
                    if len(self.received) < size:
                        break
                    if self.id64:
                        kind, length, ident = struct.unpack("<IIQ", self.received[:16])
                    else:
                        kind, length, ident = struct.unpack("<III", self.received[:12])
                    if len(self.received) < size + length:
                        break
                    body = bytes(self.received[size:size + length])
                    del self.received[:size + length]
                    self.handle(kind, ident, body)
            self.pump()


DEVICES = {"mic": Microphone, "dfu-reset": lambda: Dfu(False), "dfu-detach": lambda: Dfu(True)}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--emulate", required=True, choices=sorted(DEVICES))
    parser.add_argument("--socket", required=True, help="the Unix socket QEMU's chardev connects to")
    parser.add_argument("--ready", help="a file to write once the socket is listening")
    args = parser.parse_args()
    device = DEVICES[args.emulate]()
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(args.socket)
    listener.listen(1)
    if args.ready:
        with open(args.ready, "w") as handle:
            handle.write("ready\n")
    say(device.name, f"listening on {args.socket}")
    connection, _ = listener.accept()
    listener.close()
    try:
        return Redirection(connection, device).serve()
    except (OSError, struct.error) as error:
        say(device.name, f"stopped: {error}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
