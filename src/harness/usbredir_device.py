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
  speaker-async
              a UAC1 speaker whose isochronous OUT data endpoint is ASYNCHRONOUS: 48 kHz stereo 16-bit, packets up to
              50 frames, and a feedback endpoint (IN 0x82, refreshed every 8 ms) reporting a clock 0.2 % fast - 48.096
              frames a millisecond in 10.14. It counts the frames it receives each second against its own clock, and
              the silent ones.
  speaker-uac2
              a HIGH-SPEED UAC2 speaker: one clock source (0x10) offering 44.1 kHz and 48 to 96 kHz in steps of 48, which
              comes up at 44.1; its terminals run on it; the streaming interface's alternate 1 carries isochronous OUT
              0x01 every microframe, asynchronous, with its feedback endpoint (IN 0x81) every millisecond. Its clock runs
              0.2 % fast of WHATEVER RATE IS SET and the feedback says so in 16.16 frames a microframe - so a host that
              never set 48 kHz is asked for 44.1 kHz's rate, and plays at it. It counts the frames it receives each
              second against its own clock.
  bt-sco      a Bluetooth controller with VOICE: interface 0's event pipe (interrupt IN 0x81) and ACL pair (bulk 0x82
              and 0x02), commands on the control pipe answered as the gadget emulator answers them, ACL looped back;
              interface 1's alternates 1-5 carry isochronous IN 0x83 and OUT 0x03 of 9, 17, 25, 33 and 49 bytes, and
              every SCO packet that arrives on the OUT pipe goes back out on the IN pipe in pieces of the setting's
              size - except one on handle 0xffe, whose second piece goes back LOST (an error status, no data).
  midi2       a USB MIDI 2.0 device: its MIDI 1.0 face on alternate 0 (bulk 0x01 and 0x81, two cables each way, packets
              looped back) and its UMP face on alternate 1 (the same endpoints, words looped back), two Group Terminal
              Blocks over four groups - "Keys" on groups 0-1 and "Pads" on 2-3, both ways - read with GET_DESCRIPTOR,
              and an answer to UMP Endpoint Discovery: Endpoint Info (two blocks, MIDI 1.0 and 2.0) and its name.
  uvc-iso     a UVC 1.1 camera streaming 64x48 YUY2 ISOCHRONOUSLY: alternate zero carries no endpoint, alternates 1-3
              carry isochronous IN 0x81 of 256, 512 and 1023 bytes, and the probe answers a 512-byte payload. Frames
              are the gadget camera's - the number in the first eight bytes, the shared pattern after - cut into
              payloads with FID and EOF; frame 2 has one packet LOST (sent with an error status and no data) and frame
              3 carries the error bit; the second stream committed ends with the camera leaving the bus after its
              first frame and a tenth of a second of empty payloads.
  dfu-upload  the `dfu-reset` target with `bitCanUpload` in both modes: in DFU mode DFU_UPLOAD reads its firmware - a
              factory image of 2500 bytes by the shared formula, seed 3, until an image is manifested in its place - in
              1024-byte blocks, the last one short.
"""

import argparse
import collections
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

    # One isochronous OUT packet the host sent on `address`.
    def iso_out(self, address, data):
        pass

    # A bulk OUT transfer's data, and the bytes queued for a bulk IN endpoint and an interrupt IN one - each taken as
    # the host asks for it.
    def bulk_out(self, address, data):
        pass

    def bulk_in(self, address, length):
        return b""

    def interrupt_in(self, address):
        return None

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
class SpeakerAsync(Device):
    """A UAC1 speaker with an asynchronous data endpoint and an explicit feedback endpoint - see the head of this file."""

    name = "speaker-async"
    strings = {1: "LiberSystem harness", 2: "asynchronous speaker", 3: "LIBERSPK0001"}
    # ITS CLOCK: 48.096 kHz, 0.2 % fast - in 10.14 frames a millisecond, as the feedback endpoint sends it.
    RATE = 48_096
    FEEDBACK = round(RATE * 16384 / 1000)
    MAX_FRAMES = 50

    def __init__(self):
        super().__init__()
        self.second = None
        self.frames = 0
        self.silent = 0
        self.total = 0
        self.total_silent = 0

    def configuration_descriptor(self):
        header = bytes([9, 0x24, 0x01, 0x00, 0x01]) + struct.pack("<H", 9 + 12 + 9) + bytes([1, 1])
        streaming_in = bytes([12, 0x24, 0x02, 1]) + struct.pack("<H", 0x0101) + bytes([0, 2]) + struct.pack("<H", 0x0003) + bytes([0, 0])
        speaker = bytes([9, 0x24, 0x03, 2]) + struct.pack("<H", 0x0301) + bytes([0, 1, 0])
        control = bytes([9, DT_INTERFACE, 0, 0, 0, 1, 1, 0, 0]) + header + streaming_in + speaker
        idle = bytes([9, DT_INTERFACE, 1, 0, 0, 1, 2, 0, 0])
        active = bytes([9, DT_INTERFACE, 1, 1, 2, 1, 2, 0, 0])
        general = bytes([7, 0x24, 0x01, 1, 1]) + struct.pack("<H", 0x0001)
        format_one = bytes([11, 0x24, 0x02, 0x01, 2, 2, 16, 1]) + (48000).to_bytes(3, "little")
        # ISOCHRONOUS, ASYNCHRONOUS, naming its feedback endpoint 0x82.
        data = bytes([9, DT_ENDPOINT, 0x01, 0x05]) + struct.pack("<H", self.MAX_FRAMES * 4) + bytes([1, 0, 0x82])
        data_general = bytes([7, 0x25, 0x01, 0x00, 0x00]) + struct.pack("<H", 0)
        # FEEDBACK USAGE, refreshed every 2^3 milliseconds.
        feedback = bytes([9, DT_ENDPOINT, 0x82, 0x11]) + struct.pack("<H", 3) + bytes([1, 3, 0])
        body = control + idle + active + general + format_one + data + data_general + feedback
        return bytes([9, DT_CONFIG]) + struct.pack("<H", 9 + len(body)) + bytes([2, 1, 0, 0x80, 50]) + body

    def control(self, request_type, request, value, index, length, data):
        if request_type == 0x22 and request == 0x01 and value >> 8 == 0x01:
            return (SUCCESS, b"") if int.from_bytes(data[:3], "little") == 48000 else (STALL, b"")
        if request_type == 0xA2 and request == 0x81 and value >> 8 == 0x01:
            return SUCCESS, (48000).to_bytes(3, "little")
        return STALL, b""

    def on_alternate(self, interface, alternate):
        say(self.name, f"alternate {alternate}" + (" - zero bandwidth" if alternate == 0 else ""))

    def iso_started(self, address):
        say(self.name, f"{'feedback' if address & 0x80 else 'data'} stream on 0x{address:02x} started")

    def iso_stopped(self, address):
        say(self.name, f"stream on 0x{address:02x} stopped - {self.total} frames received, {self.total_silent} of them silent")

    # THE FEEDBACK: its clock's rate, every time the host reads it.
    def iso_in(self, address):
        return self.FEEDBACK.to_bytes(3, "little")

    # EACH SECOND'S FRAMES, counted against this device's own clock - the host's monotonic one, here.
    def iso_out(self, address, data):
        now = time.monotonic()
        if self.second is None:
            self.second = now
        frames = len(data) // 4
        silent = frames if data and not any(data) else 0
        self.frames += frames
        self.silent += silent
        self.total += frames
        self.total_silent += silent
        if now - self.second >= 1.0:
            say(self.name, f"received {self.frames} frames in the last second ({self.silent} silent); its clock asks {self.RATE}")
            self.second, self.frames, self.silent = now, 0, 0


class SpeakerUac2(Device):
    """A high-speed UAC2 speaker on a settable clock - see the head of this file."""

    name = "speaker-uac2"
    speed = SPEED_HIGH
    # THE ASSOCIATION CLASS: a UAC2 function groups its interfaces with an interface association descriptor.
    device_class = (0xEF, 0x02, 0x01)
    strings = {1: "LiberSystem harness", 2: "UAC2 speaker", 3: "LIBERSPK0002"}
    CLOCK = 0x10
    RANGES = ((44_100, 44_100, 0), (48_000, 96_000, 48_000))
    # SEVEN FRAMES A MICROFRAME AT MOST: six at 48 kHz, and the seventh for a fast clock's carry.
    MAX_PACKET = 7 * 4

    def __init__(self):
        super().__init__()
        self.rate = 44_100
        self.second = None
        self.frames = 0
        self.total = 0

    def offered(self, rate):
        return any(low <= rate <= high and (step == 0 or (rate - low) % step == 0) for low, high, step in self.RANGES)

    def configuration_descriptor(self):
        association = bytes([8, 0x0B, 0, 2, 1, 0, 0x20, 0])
        control = bytes([9, DT_INTERFACE, 0, 0, 0, 1, 1, 0x20, 0])
        clock = bytes([8, 0x24, 0x0A, self.CLOCK, 0x03, 0x07, 0, 0])
        streaming_in = bytes([17, 0x24, 0x02, 1]) + struct.pack("<H", 0x0101) + bytes([0, self.CLOCK, 2]) + struct.pack("<I", 3) + bytes([0]) + struct.pack("<H", 0) + bytes([0])
        speaker = bytes([12, 0x24, 0x03, 3]) + struct.pack("<H", 0x0301) + bytes([0, 1, self.CLOCK]) + struct.pack("<H", 0) + bytes([0])
        topology = clock + streaming_in + speaker
        header = bytes([9, 0x24, 0x01]) + struct.pack("<H", 0x0200) + bytes([0x01]) + struct.pack("<H", 9 + len(topology)) + bytes([0])
        idle = bytes([9, DT_INTERFACE, 1, 0, 0, 1, 2, 0x20, 0])
        active = bytes([9, DT_INTERFACE, 1, 1, 2, 1, 2, 0x20, 0])
        general = bytes([16, 0x24, 0x01, 1, 0, 1]) + struct.pack("<I", 1) + bytes([2]) + struct.pack("<I", 3) + bytes([0])
        format_two = bytes([6, 0x24, 0x02, 0x01, 2, 16])
        # ISOCHRONOUS, ASYNCHRONOUS, every microframe - and no bSynchAddress, which UAC2 endpoints do not carry.
        data = bytes([7, DT_ENDPOINT, 0x01, 0x05]) + struct.pack("<H", self.MAX_PACKET) + bytes([1])
        data_general = bytes([8, 0x25, 0x01, 0, 0, 0]) + struct.pack("<H", 0)
        # FEEDBACK USAGE, every 2^(4-1) microframes: a millisecond.
        feedback = bytes([7, DT_ENDPOINT, 0x81, 0x11]) + struct.pack("<H", 4) + bytes([4])
        body = association + control + header + topology + idle + active + general + format_two + data + data_general + feedback
        return bytes([9, DT_CONFIG]) + struct.pack("<H", 9 + len(body)) + bytes([2, 1, 0, 0x80, 50]) + body

    # THE CLOCK'S SAMPLING-FREQUENCY CONTROL, through the audio-control interface: RANGE and CUR read, CUR written -
    # a rate the clock does not offer is stalled.
    def control(self, request_type, request, value, index, length, data):
        if value >> 8 != 0x01 or index != (self.CLOCK << 8):
            return STALL, b""
        if request_type == 0xA1 and request == 0x02:
            answer = struct.pack("<H", len(self.RANGES)) + b"".join(struct.pack("<III", *triple) for triple in self.RANGES)
            return SUCCESS, answer[:length]
        if request_type == 0xA1 and request == 0x01:
            return SUCCESS, struct.pack("<I", self.rate)[:length]
        if request_type == 0x21 and request == 0x01 and len(data) >= 4:
            rate = struct.unpack("<I", data[:4])[0]
            if not self.offered(rate):
                say(self.name, f"refused a clock of {rate} Hz")
                return STALL, b""
            self.rate = rate
            say(self.name, f"the clock runs at {rate} Hz")
            return SUCCESS, b""
        return STALL, b""

    def on_alternate(self, interface, alternate):
        say(self.name, f"alternate {alternate}" + (" - zero bandwidth" if alternate == 0 else f", the clock at {self.rate} Hz"))

    def iso_started(self, address):
        say(self.name, f"{'feedback' if address & 0x80 else 'data'} stream on 0x{address:02x} started")

    def iso_stopped(self, address):
        say(self.name, f"stream on 0x{address:02x} stopped - {self.total} frames received at {self.rate} Hz")

    # THE FEEDBACK: the clock's rate, 0.2 % fast, in 16.16 frames a microframe.
    def iso_in(self, address):
        return round(self.rate * 1.002 * 65536 / 8000).to_bytes(4, "little")

    def iso_out(self, address, data):
        now = time.monotonic()
        if self.second is None:
            self.second = now
        frames = len(data) // 4
        self.frames += frames
        self.total += frames
        if now - self.second >= 1.0:
            say(self.name, f"received {self.frames} frames in the last second; its clock asks {round(self.rate * 1.002)}")
            self.second, self.frames = now, 0


class BtSco(Device):
    """A Bluetooth controller with voice settings - see the head of this file."""

    name = "bt-sco"
    device_class = (0xE0, 0x01, 0x01)
    strings = {1: "LiberSystem harness", 2: "Bluetooth controller with voice", 3: "LIBERBT0002"}
    BD_ADDR = bytes([0x02, 0x00, 0x00, 0xEE, 0xFF, 0xC0])
    SIZES = (0, 9, 17, 25, 33, 49)
    TORN_HANDLE = 0xFFE
    EVENT_PACKET = 16

    def __init__(self):
        super().__init__()
        self.events = collections.deque()
        self.acl_held = bytearray()
        self.acl_back = bytearray()
        self.sco_held = bytearray()
        self.sco_back = collections.deque()

    def configuration_descriptor(self):
        def setting(number, alternate, endpoints):
            return bytes([9, DT_INTERFACE, number, alternate, len(endpoints), 0xE0, 0x01, 0x01, 0]) + b"".join(endpoints)

        def endpoint(address, attributes, packet, interval):
            return bytes([7, DT_ENDPOINT, address, attributes]) + struct.pack("<H", packet) + bytes([interval])

        primary = setting(0, 0, [endpoint(0x81, 0x03, self.EVENT_PACKET, 1), endpoint(0x82, 0x02, 64, 0), endpoint(0x02, 0x02, 64, 0)])
        voice = b"".join(setting(1, alternate, [endpoint(0x83, 0x01, size, 1), endpoint(0x03, 0x01, size, 1)]) for alternate, size in enumerate(self.SIZES))
        body = primary + voice
        return bytes([9, DT_CONFIG]) + struct.pack("<H", 9 + len(body)) + bytes([2, 1, 0, 0x80, 50]) + body

    def on_alternate(self, interface, alternate):
        say(self.name, f"interface {interface} alternate {alternate}" + (" - zero bandwidth" if alternate == 0 else f", pieces of {self.SIZES[alternate]} bytes"))
        if interface == 1:
            self.sco_held.clear()
            self.sco_back.clear()

    def complete(self, opcode, status, parameters=b""):
        body = bytes([1]) + struct.pack("<H", opcode) + bytes([status]) + parameters
        self.event(bytes([0x0E, len(body)]) + body)

    # AN EVENT GOES UP THE INTERRUPT PIPE IN PACKETS OF ITS SIZE, as a real controller's does.
    def event(self, event):
        for at in range(0, len(event), self.EVENT_PACKET):
            self.events.append(event[at:at + self.EVENT_PACKET])

    # A command: class, host to device, the DEVICE recipient - answered as the gadget emulator answers it.
    def control(self, request_type, request, value, index, length, data):
        if request_type != 0x20 or request != 0x00 or len(data) < 3:
            return STALL, b""
        opcode = struct.unpack("<H", data[:2])[0]
        if opcode == 0x0C03:
            self.complete(opcode, 0)
        elif opcode == 0x1009:
            self.complete(opcode, 0, self.BD_ADDR)
        else:
            self.complete(opcode, 0x01)
        say(self.name, f"command {opcode:#06x}")
        return SUCCESS, b""

    def interrupt_in(self, address):
        return self.events.popleft() if self.events else None

    # ACL LOOPED BACK on the bulk pair, with a Number Of Completed Packets event for each packet taken.
    def bulk_out(self, address, data):
        self.acl_held += data
        while len(self.acl_held) >= 4:
            length = 4 + struct.unpack("<H", self.acl_held[2:4])[0]
            if len(self.acl_held) < length:
                break
            packet = bytes(self.acl_held[:length])
            del self.acl_held[:length]
            self.acl_back += packet
            handle = struct.unpack("<H", packet[:2])[0] & 0x0FFF
            self.event(bytes([0x13, 5, 1]) + struct.pack("<HH", handle, 1))
            say(self.name, f"ACL {len(packet)} bytes on handle {handle:#05x} looped back")

    def bulk_in(self, address, length):
        data = bytes(self.acl_back[:length])
        del self.acl_back[:length]
        return data

    # SCO LOOPED BACK: whole packets cut out of the OUT pieces by their own headers, each sent back in pieces of the
    # setting's size - a packet starting a piece, as the host's transport expects.
    def iso_out(self, address, data):
        self.sco_held += data
        size = self.SIZES[self.alternates.get(1, 0)]
        while len(self.sco_held) >= 3 and size:
            length = 3 + self.sco_held[2]
            if len(self.sco_held) < length:
                break
            packet = bytes(self.sco_held[:length])
            del self.sco_held[:length]
            handle = struct.unpack("<H", packet[:2])[0] & 0x0FFF
            pieces = [packet[at:at + size] for at in range(0, len(packet), size)]
            for index, piece in enumerate(pieces):
                self.sco_back.append(None if handle == self.TORN_HANDLE and index == 1 else piece)
            say(self.name, f"SCO {len(packet)} bytes on handle {handle:#05x} looped back in {len(pieces)} pieces" + (", the second LOST" if handle == self.TORN_HANDLE else ""))

    # AN INTERVAL WITH NOTHING TO SEND sends an empty piece; a lost one goes with an error status.
    def iso_in(self, address):
        return self.sco_back.popleft() if self.sco_back else b""

    def iso_started(self, address):
        say(self.name, f"{'IN' if address & 0x80 else 'OUT'} voice stream on 0x{address:02x} started")

    def iso_stopped(self, address):
        say(self.name, f"voice stream on 0x{address:02x} stopped")


class Midi2(Device):
    """A USB MIDI 2.0 device with both faces - see the head of this file."""

    name = "midi2"
    strings = {1: "LiberSystem harness", 2: "MIDI 2.0 device", 3: "LIBERMIDI0002", 4: "Keys", 5: "Pads"}
    # id, name string, first group, groups.
    BLOCKS = ((1, 4, 0, 2), (2, 5, 2, 2))
    ENDPOINT_NAME = b"Liber UMP"

    def __init__(self):
        super().__init__()
        self.back = bytearray()
        self.held = bytearray()

    def configuration_descriptor(self):
        def cs_endpoint(subtype, ids):
            return bytes([4 + len(ids), 0x25, subtype, len(ids)]) + bytes(ids)

        control = bytes([9, DT_INTERFACE, 0, 0, 0, 1, 1, 0, 0]) + bytes([9, 0x24, 0x01, 0x00, 0x01]) + struct.pack("<H", 9) + bytes([1, 1])
        midi1 = bytes([9, DT_INTERFACE, 1, 0, 2, 1, 3, 0, 0]) + bytes([7, 0x24, 0x01, 0x00, 0x01]) + struct.pack("<H", 7)
        midi1 += bytes([9, DT_ENDPOINT, 0x01, 0x02]) + struct.pack("<H", 64) + bytes([0, 0, 0]) + cs_endpoint(0x01, [1, 2])
        midi1 += bytes([9, DT_ENDPOINT, 0x81, 0x02]) + struct.pack("<H", 64) + bytes([0, 0, 0]) + cs_endpoint(0x01, [3, 4])
        midi2 = bytes([9, DT_INTERFACE, 1, 1, 2, 1, 3, 0, 0]) + bytes([7, 0x24, 0x01, 0x00, 0x02]) + struct.pack("<H", 7)
        midi2 += bytes([7, DT_ENDPOINT, 0x01, 0x02]) + struct.pack("<H", 64) + bytes([0]) + cs_endpoint(0x02, [1, 2])
        midi2 += bytes([7, DT_ENDPOINT, 0x81, 0x02]) + struct.pack("<H", 64) + bytes([0]) + cs_endpoint(0x02, [1, 2])
        body = control + midi1 + midi2
        return bytes([9, DT_CONFIG]) + struct.pack("<H", 9 + len(body)) + bytes([2, 1, 0, 0x80, 50]) + body

    # THE GROUP TERMINAL BLOCKS, asked for as a standard GET_DESCRIPTOR to the interface: type 0x26, alternate 1.
    def control(self, request_type, request, value, index, length, data):
        if request_type == 0x81 and request == 0x06 and value == 0x2601 and index == 1:
            blocks = b"".join(bytes([13, 0x26, 0x02, ident, 0, first, count, string, 0x11, 0, 0, 0, 0]) for ident, string, first, count in self.BLOCKS)
            return SUCCESS, bytes([5, 0x26, 0x01]) + struct.pack("<H", 5 + len(blocks)) + blocks
        return STALL, b""

    def on_alternate(self, interface, alternate):
        say(self.name, f"alternate {alternate} - " + ("UMP" if alternate == 1 else "MIDI 1.0"))
        self.back.clear()
        self.held.clear()

    def ump_face(self):
        return self.alternates.get(1, 0) == 1

    # UMP ENDPOINT DISCOVERY answered with Endpoint Info - UMP 1.1, two static function blocks, MIDI 1.0 and 2.0 - and
    # the endpoint's name in one complete message; every other message looped back word for word.
    def bulk_out(self, address, data):
        if not self.ump_face():
            self.back += data
            say(self.name, f"MIDI 1.0 packets looped back: {len(data) // 4}")
            return
        self.held += data
        while len(self.held) >= 4:
            first = struct.unpack("<I", self.held[:4])[0]
            size = 4 * (1, 1, 1, 2, 2, 4, 1, 1, 2, 2, 2, 3, 3, 4, 4, 4)[first >> 28]
            if len(self.held) < size:
                break
            message = bytes(self.held[:size])
            del self.held[:size]
            if first >> 28 == 0xF and (first >> 16) & 0x3FF == 0x000:
                info = (0xF0010101, 0x80000000 | len(self.BLOCKS) << 24 | 0x300, 0, 0)
                name = self.ENDPOINT_NAME.ljust(14, b"\0")
                words = (0xF0030000 | name[0] << 8 | name[1], *struct.unpack(">III", name[2:14]))
                self.back += struct.pack("<4I", *info) + struct.pack("<4I", *words)
                say(self.name, "Endpoint Discovery answered: two function blocks, and the name")
            else:
                self.back += message
                say(self.name, f"UMP of {size // 4} word(s) on group {(first >> 24) & 0xF} looped back")

    def bulk_in(self, address, length):
        data = bytes(self.back[:length])
        del self.back[:length]
        return data


class UvcIso(Device):
    """A UVC 1.1 camera streaming 64x48 YUY2 over isochronous IN 0x81 - see the head of this file."""

    name = "uvc-iso"
    strings = {1: "LiberSystem harness", 2: "isochronous camera", 3: "LIBERCAM0001"}
    WIDTH, HEIGHT = 64, 48
    FRAME = 64 * 48 * 2
    INTERVAL = 333_333
    PACKETS = (256, 512, 1023)
    PAYLOAD = 512
    LOST_FRAME, LOST_PACKET, BAD_FRAME = 2, 4, 3

    def __init__(self):
        super().__init__()
        self.probe = self.default_probe()
        self.commits = 0
        self.number = 0
        self.queue = []
        self.packets = 0
        self.lost = 0
        self.filled = False

    def default_probe(self):
        probe = bytearray(34)
        probe[0] = 1
        probe[2], probe[3] = 1, 1
        struct.pack_into("<I", probe, 4, self.INTERVAL)
        struct.pack_into("<I", probe, 18, self.FRAME)
        struct.pack_into("<I", probe, 22, self.PAYLOAD)
        probe[31] = 1
        return probe

    def configuration_descriptor(self):
        control = bytes([13, 0x24, 0x01, 0x10, 0x01, 13, 0]) + struct.pack("<I", 6_000_000) + bytes([1, 1])
        streaming = bytes([14, 0x24, 0x01, 1, 77, 0, 0x81, 0, 2, 0, 0, 0, 1, 0])
        yuy2 = bytes([0x59, 0x55, 0x59, 0x32, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71])
        streaming += bytes([27, 0x24, 0x04, 1, 1]) + yuy2 + bytes([16, 1, 0, 0, 0, 0])
        streaming += bytes([30, 0x24, 0x05, 1, 0]) + struct.pack("<HHIIIIB", self.WIDTH, self.HEIGHT, 1, 1, self.FRAME, self.INTERVAL, 1) + struct.pack("<I", self.INTERVAL)
        streaming += bytes([6, 0x24, 0x0D, 1, 1, 4])
        body = bytes([9, DT_INTERFACE, 0, 0, 0, 0x0E, 0x01, 0x00, 0]) + control
        body += bytes([9, DT_INTERFACE, 1, 0, 0, 0x0E, 0x02, 0x00, 0]) + streaming
        for at, packet in enumerate(self.PACKETS):
            body += bytes([9, DT_INTERFACE, 1, at + 1, 1, 0x0E, 0x02, 0x00, 0])
            body += bytes([7, DT_ENDPOINT, 0x81, 0x05]) + struct.pack("<H", packet) + bytes([1])
        return bytes([9, DT_CONFIG]) + struct.pack("<H", 9 + len(body)) + bytes([2, 1, 0, 0x80, 50]) + body

    # PROBE AND COMMIT, on the streaming interface: what the host asked for, with this camera's frame and payload sizes.
    def control(self, request_type, request, value, index, length, data):
        selector = value >> 8
        if request_type == 0x21 and request == 0x01 and selector == 1:
            probe = bytearray(self.default_probe())
            probe[:min(len(data), 8)] = data[:min(len(data), 8)]
            struct.pack_into("<I", probe, 18, self.FRAME)
            struct.pack_into("<I", probe, 22, self.PAYLOAD)
            self.probe = probe
            return SUCCESS, b""
        if request_type == 0x21 and request == 0x01 and selector == 2:
            self.commits += 1
            self.number = 0
            self.queue = []
            self.filled = False
            say(self.name, f"committed stream {self.commits} - a {self.PAYLOAD}-byte payload")
            return SUCCESS, b""
        if request_type == 0xA1 and selector in (1, 2):
            if request == 0x85:
                return SUCCESS, struct.pack("<H", 34)
            if request == 0x86:
                return SUCCESS, bytes([3])
            return SUCCESS, bytes(self.probe if request == 0x81 else self.default_probe())[:length]
        return STALL, b""

    def on_alternate(self, interface, alternate):
        if interface == 1:
            say(self.name, f"alternate {alternate}" + (" - zero bandwidth" if alternate == 0 else f", {self.PACKETS[alternate - 1]} bytes an interval"))

    def iso_started(self, address):
        say(self.name, f"streaming from frame {self.number}")

    def iso_stopped(self, address):
        say(self.name, f"stopped after {self.packets} packets, {self.lost} of them lost on purpose, at frame {self.number}")

    def frame(self, number):
        data = struct.pack("<Q", number) + bytes((at * 7 + number * 13) % 251 for at in range(8, self.FRAME))
        room = self.PAYLOAD - 2
        chunks = [data[at:at + room] for at in range(0, len(data), room)]
        out = []
        for at, chunk in enumerate(chunks):
            flags = 0x80 | (number & 1) | (0x02 if at == len(chunks) - 1 else 0)
            if number == self.BAD_FRAME and at == 0:
                flags |= 0x40
            out.append(None if number == self.LOST_FRAME and at == self.LOST_PACKET else bytes([2, flags]) + chunk)
        return out

    # ONE SERVICE INTERVAL'S PAYLOAD - or `None`, a packet lost, which goes out with an error status and no data.
    def iso_in(self, address):
        if not self.queue:
            # THE SECOND STREAM ENDS WITH THE CAMERA LEAVING after its first frame - and not at once: QEMU prefills
            # its buffer of isochronous IN packets before it hands any to the guest and drops what it holds when the
            # device goes. So the first frame is followed by a tenth of a second of empty payloads still carrying ITS
            # FID - which the class says belong to no frame - and then the camera leaves.
            if self.commits >= 2 and self.number == 1 and not self.filled:
                self.filled = True
                self.queue = [bytes([2, 0x80])] * 100
            elif self.commits >= 2 and self.filled:
                say(self.name, "leaving the bus in the middle of the second stream")
                self.leaving = "gone"
                return b""
            else:
                self.queue = self.frame(self.number)
            self.number += 1
        self.packets += 1
        packet = self.queue.pop(0)
        if packet is None:
            self.lost += 1
        return packet


# THE FIRMWARE AN UPLOAD-CAPABLE TARGET HOLDS BEFORE ANYTHING IS MANIFESTED.
DFU_FACTORY = firmware(3, 2500)
# AN IMAGE THAT BEGINS WITH THIS makes the target leave the bus after its first block, for good.
DFU_UNPLUG = b"UNPLUG"
DFU_IDLE, DFU_DNLOAD_SYNC, DFU_DNLOAD_IDLE, DFU_MANIFEST_SYNC, DFU_MANIFEST, DFU_UPLOAD_IDLE, DFU_ERROR = 2, 3, 5, 6, 7, 9, 10
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

    def __init__(self, will_detach, can_upload=False):
        super().__init__()
        self.will_detach = will_detach
        self.can_upload = can_upload
        self.name = "dfu-upload" if can_upload else "dfu-detach" if will_detach else "dfu-reset"
        # What an upload reads: the factory image, then whatever was manifested.
        self.held = DFU_FACTORY
        self.uploaded = 0
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
        attributes = 0x01 | 0x04 | (0x08 if self.will_detach and not self.mode_dfu else 0) | (0x02 if self.can_upload else 0)
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
            if self.state == DFU_UPLOAD_IDLE:
                say(self.name, f"DFU_ABORT during an upload, after {self.uploaded} bytes")
            self.state, self.status = DFU_IDLE, 0
            self.image, self.block = bytearray(), 0
            return SUCCESS, b""
        if request_type == 0xA1 and request == 2:
            return self.upload(value, length)
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

    def upload(self, block, length):
        if not self.can_upload or self.state not in (DFU_IDLE, DFU_UPLOAD_IDLE):
            return STALL, b""
        if self.state == DFU_IDLE:
            if block != 0:
                return STALL, b""
            self.uploaded = 0
            say(self.name, f"DFU_UPLOAD begins, {length}-byte blocks")
        data = self.held[block * length:(block + 1) * length]
        self.uploaded += len(data)
        if len(data) < length:
            say(self.name, f"DFU_UPLOAD ended - {self.uploaded} bytes in {block + 1} block(s)")
            self.state = DFU_IDLE
        else:
            self.state = DFU_UPLOAD_IDLE
        return SUCCESS, data

    def get_status(self):
        poll = 0
        if self.state == DFU_DNLOAD_SYNC:
            self.state, poll = DFU_DNLOAD_IDLE, 10
        elif self.state == DFU_MANIFEST_SYNC:
            digest = hashlib.sha256(bytes(self.image)).digest()
            if digest == DFU_EXPECTED:
                say(self.name, f"{len(self.image)} bytes in {self.block} block(s), the expected image - manifesting")
                self.state, poll = DFU_MANIFEST, 50
                self.held = bytes(self.image)
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
        # Isochronous OUT streams the host started.
        self.outs = set()
        # Bulk IN requests waiting for data, oldest first: (id, endpoint, length). Interrupt IN endpoints receiving,
        # and the id of the next interrupt packet.
        self.bulk_waiting = []
        self.interrupts = set()
        self.interrupt_id = 0
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
        for address in list(self.outs):
            if interface is None or current.get(address) == interface or address not in current:
                self.outs.discard(address)
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
            if interval is None:
                self.send(ISO_STREAM_STATUS, ident, bytes([INVAL, address]))
                return
            # AN OUT STREAM IS THE HOST'S TO PACE: its packets arrive and are handed to the model as they come.
            if not address & 0x80:
                self.outs.add(address)
                self.send(ISO_STREAM_STATUS, ident, bytes([SUCCESS, address]))
                device.iso_started(address)
                return
            # A full-speed interval is frames; a high-speed one is 2^(n-1) microframes.
            seconds = interval / 1000 if device.speed == SPEED_FULL else (1 << (interval - 1)) / 8000
            self.streams[address] = [seconds, time.monotonic(), 0]
            self.send(ISO_STREAM_STATUS, ident, bytes([SUCCESS, address]))
            device.iso_started(address)
        elif kind == STOP_ISO_STREAM:
            address = body[0]
            if self.streams.pop(address, None) is not None or address in self.outs:
                self.outs.discard(address)
                device.iso_stopped(address)
            self.send(ISO_STREAM_STATUS, ident, bytes([SUCCESS, address]))
        elif kind == ISO_PACKET:
            # AN ISOCHRONOUS OUT PACKET: endpoint, status, length, then the data. Nothing answers it.
            address, _status, length = struct.unpack("<BBH", body[:4])
            if address in self.outs:
                device.iso_out(address, bytes(body[4:4 + length]))
        elif kind == CONTROL_PACKET:
            self.control_packet(ident, body)
            # A MODEL THAT HAS TO LEAVE does so after its answer has gone: a detach is acknowledged before the
            # device drops off the bus, as a real one's is.
            if device.leaving is not None:
                self.leave()
        elif kind == DEVICE_DISCONNECT_ACK:
            if self.returning is not None:
                self.come_back()
        elif kind == BULK_PACKET:
            # endpoint, status, length, stream id and the length's high half - then, going OUT, the data.
            endpoint, _status, low, _stream, high = struct.unpack("<BBHIH", body[:10])
            length = low | (high << 16 if self.both(CAP_32BITS_BULK_LENGTH) else 0)
            if endpoint & 0x80:
                # AN IN REQUEST WAITS for data, and is answered when the model has some.
                self.bulk_waiting.append((ident, endpoint, length))
            else:
                device.bulk_out(endpoint, bytes(body[10:10 + length]))
                self.send(BULK_PACKET, ident, struct.pack("<BBHIH", endpoint, SUCCESS, length & 0xFFFF, 0, length >> 16))
        elif kind == CANCEL_DATA_PACKET:
            # A WAITING BULK REQUEST the host gave up on is answered as cancelled; nothing else is ever left pending.
            for waiting in [w for w in self.bulk_waiting if w[0] == ident]:
                self.bulk_waiting.remove(waiting)
                self.send(BULK_PACKET, ident, struct.pack("<BBHIH", waiting[1], CANCELLED, 0, 0, 0))
        elif kind == FILTER_FILTER:
            pass
        elif kind == START_INTERRUPT_RECEIVING:
            self.interrupts.add(body[0])
            self.send(INTERRUPT_RECEIVING_STATUS, ident, bytes([SUCCESS, body[0]]))
        elif kind == STOP_INTERRUPT_RECEIVING:
            self.interrupts.discard(body[0])
            self.send(INTERRUPT_RECEIVING_STATUS, ident, bytes([SUCCESS, body[0]]))
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
        # THE DATA THE MODEL HAS FOR THE HOST: interrupt packets as they come, and waiting bulk requests answered, each
        # with as much as it asked for and no more.
        for address in sorted(self.interrupts):
            while (data := self.device.interrupt_in(address)) is not None:
                self.send(INTERRUPT_PACKET, self.interrupt_id, struct.pack("<BBH", address, SUCCESS, len(data)) + data)
                self.interrupt_id += 1
        for waiting in list(self.bulk_waiting):
            ident, endpoint, length = waiting
            data = self.device.bulk_in(endpoint, length)
            if data:
                self.bulk_waiting.remove(waiting)
                self.send(BULK_PACKET, ident, struct.pack("<BBHIH", endpoint, SUCCESS, len(data) & 0xFFFF, 0, len(data) >> 16) + data)
        now = time.monotonic()
        for address, stream in self.streams.items():
            seconds, due, sent = stream
            burst = 0
            while due <= now and burst < 16:
                data = self.device.iso_in(address)
                # A PACKET THE MODEL LOSES goes out with an error status and nothing in it.
                if data is None:
                    self.send(ISO_PACKET, sent, struct.pack("<BBH", address, IOERROR, 0))
                else:
                    self.send(ISO_PACKET, sent, struct.pack("<BBH", address, SUCCESS, len(data)) + data)
                sent += 1
                due += seconds
                burst += 1
                if self.device.leaving is not None:
                    break
            stream[1], stream[2] = due, sent
            if self.device.leaving is not None:
                self.leave()
                return

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
            # A MODEL WITH DATA PENDING is looked at again soon, whatever the streams' schedule.
            if self.bulk_waiting or self.interrupts:
                wait = 0.001 if wait is None else min(wait, 0.001)
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


DEVICES = {"bt-sco": BtSco, "mic": Microphone, "midi2": Midi2, "speaker-async": SpeakerAsync, "speaker-uac2": SpeakerUac2, "uvc-iso": UvcIso, "dfu-reset": lambda: Dfu(False), "dfu-detach": lambda: Dfu(True), "dfu-upload": lambda: Dfu(False, True)}


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
