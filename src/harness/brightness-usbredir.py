#!/usr/bin/env python3
"""The brightness fixtures through QEMU usb-redir, without host USB devices.

The report descriptors are usb-gadget.sh's monitor and consumer-keys descriptors.
The firmware constants, report bytes and log oracles come from the existing gadget
simulators. This transport exercises the guest's real HID control/interrupt paths;
it does not exercise Linux usb_f_hid or usb-host.
"""

import argparse
from collections import deque
import importlib.util
import os
from pathlib import Path
import signal
import socket
import struct
import sys
import time

from usbredir_device import Device, Redirection, STALL, SUCCESS


def firmware(name):
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


monitor = firmware('monitor-sim')
keys = firmware('keys-sim')

MONITOR_REPORT = bytes.fromhex(
    '05 80 09 01 a1 01 85 01 09 02 15 00 26 ff 00 75 08 95 20 b2 02 01 '
    '05 82 85 02 09 10 15 00 25 64 75 08 95 01 b1 02 85 04 09 10 81 02 c0 '
    '05 20 09 41 a1 01 85 03 0a d1 04 15 00 27 ff ff 00 00 75 10 95 01 55 00 81 02 c0')
KEYS_REPORT = bytes.fromhex('05 0c 09 01 a1 01 15 00 26 ff 03 19 00 2a ff 03 75 10 95 01 81 00 c0')


class Hid(Device):
    """A non-boot HID interface with a single interrupt-IN endpoint."""

    def __init__(self):
        super().__init__()
        self.reports = deque()

    def hid_descriptor(self):
        return struct.pack('<BBHBBBH', 9, 0x21, 0x0111, 0, 1, 0x22, len(self.report_descriptor))

    def configuration_descriptor(self):
        body = bytes([9, 4, 0, 0, 1, 3, 0, 0, 0]) + self.hid_descriptor()
        body += struct.pack('<BBBBHB', 7, 5, 0x81, 3, self.packet_size, 10)
        return struct.pack('<BBHBBBBB', 9, 2, 9 + len(body), 1, 1, 0, 0x80, 50) + body

    def control(self, request_type, request, value, index, length, data):
        if request_type == 0x81 and request == 6 and index == 0 and not data:
            if value == 0x2200:
                return SUCCESS, self.report_descriptor
            if value == 0x2100:
                return SUCCESS, self.hid_descriptor()
        return STALL, b''

    def on_configuration(self, value):
        self.reports.clear()

    def on_reset(self):
        self.on_configuration(0)

    def interrupt_in(self, address):
        if self.configuration and address == 0x81 and self.reports:
            return self.reports.popleft()
        return None


class Monitor(Hid):
    name = 'monitor'
    report_descriptor = MONITOR_REPORT
    packet_size = 33
    strings = {1: 'LiberSystem harness', 2: 'monitor', 3: 'LIBER0001'}

    def __init__(self, button=None):
        super().__init__()
        self.level = 50
        self.lux = monitor.DARK
        self.button = button
        self.button_at = None
        self.next_light = None
        monitor.say('reports registered: EDID 10ac:4050 serial 7, brightness 50, ten lux')

    def on_configuration(self, value):
        super().on_configuration(value)
        # USB reset changes transport state, not the running firmware's level,
        # illuminance or once-only button action.
        if value and self.next_light is None:
            self.next_light = time.monotonic() + monitor.STEP_SECONDS
        if value:
            monitor.say('the guest configured the device')

    def control(self, request_type, request, value, index, length, data):
        if index == 0 and self.configuration:
            if request_type == 0xa1 and request == 1 and not data:
                reports = {0x0301: bytes([1]) + monitor.EDID,
                           0x0302: monitor.brightness_report(self.level),
                           0x0103: monitor.light_report(self.lux),
                           0x0104: bytes([4, self.level])}
                if value in reports:
                    return SUCCESS, reports[value]
            if request_type == 0x21 and request == 9:
                if value == 0x0302 and length == 2 and len(data) == 2 and data[0] == 2 and data[1] <= 100:
                    self.level = data[1]
                    monitor.say(f'brightness set to {self.level}')
                    if self.button is not None and self.level == self.button[0] and self.button_at is None:
                        self.button_at = time.monotonic() + 2.0
                    return SUCCESS, b''
                monitor.say(f"a SET_REPORT this monitor has no control for: {data.hex(' ')}")
        return super().control(request_type, request, value, index, length, data)

    def poll(self):
        if not self.configuration:
            return
        now = time.monotonic()
        if self.button_at is not None and now >= self.button_at:
            self.level = self.button[1]
            self.reports.append(bytes([4, self.level]))
            monitor.say(f'device button changed brightness to {self.level}')
            self.button = None
            self.button_at = None
        if self.next_light is not None and now >= self.next_light:
            self.lux = monitor.BRIGHT if self.lux == monitor.DARK else monitor.DARK
            self.reports.append(monitor.light_report(self.lux))
            monitor.say(f'illuminance {self.lux} lux')
            self.next_light = now + monitor.STEP_SECONDS


class ConsumerKeys(Hid):
    name = 'consumer-keys'
    report_descriptor = KEYS_REPORT
    packet_size = 2
    strings = {1: 'LiberSystem harness', 2: 'consumer-keys', 3: 'LIBER0001'}

    def __init__(self, trigger=None, delay=60):
        super().__init__()
        self.trigger = trigger
        self.delay = delay
        self.next_key = None
        self.step = 0
        self.pressed = False
        self.done = False

    def on_configuration(self, value):
        super().on_configuration(value)
        # A USB reset drops pending packets, not the running firmware's sequence.
        # Preserve both a pending release and the completed state across QMP reset.
        if value and self.next_key is None:
            self.next_key = time.monotonic() + (0 if self.trigger else self.delay)
        if value and not self.done:
            message = f'once {self.trigger} exists' if self.trigger else f'in {self.delay:.0f} s'
            keys.say(f'the guest configured the device; pressing {message}')

    def poll(self):
        if not self.configuration or self.done or self.next_key is None:
            return
        now = time.monotonic()
        if now < self.next_key or (self.trigger and not os.path.exists(self.trigger)):
            return
        if self.step == 3:
            self.done = True
            keys.say('all three presses made')
            return
        name, usage = (('up', keys.UP), ('up', keys.UP), ('down', keys.DOWN))[self.step]
        if self.pressed:
            self.reports.append(struct.pack('<H', 0))
            keys.say(f'brightness {name} pressed and released')
            self.step += 1
            self.next_key = now + 2.0
        else:
            self.reports.append(struct.pack('<H', usage))
            self.next_key = now + 0.1
        self.pressed = not self.pressed


def serve(device, path, ready=None):
    """Own only the socket and readiness file created by this invocation."""
    owned = {}
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        listener.bind(path)
        stat = os.lstat(path)
        owned[path] = (stat.st_dev, stat.st_ino)
        listener.listen(1)
        if ready:
            with open(ready, 'x') as output:
                stat = os.fstat(output.fileno())
                owned[ready] = (stat.st_dev, stat.st_ino)
                output.write('ready\n')
        print(f'brightness-usbredir: {device.name} listening on {path}', file=sys.stderr, flush=True)
        connection, _ = listener.accept()
        with connection:
            return Redirection(connection, device).serve()
    finally:
        listener.close()
        for name, identity in owned.items():
            try:
                stat = os.lstat(name)
                if (stat.st_dev, stat.st_ino) == identity:
                    os.unlink(name)
            except FileNotFoundError:
                pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--emulate', required=True, choices=('monitor', 'consumer-keys'))
    parser.add_argument('--socket', required=True)
    parser.add_argument('--ready')
    parser.add_argument('--button-after', help='monitor: after SET A, report a device button change to B, once (A:B)')
    args = parser.parse_args()
    button = None
    if args.button_after:
        try:
            button = tuple(map(int, args.button_after.split(':')))
        except ValueError:
            parser.error('--button-after needs two levels in 0..100')
        if args.emulate != 'monitor' or len(button) != 2 or not all(0 <= n <= 100 for n in button):
            parser.error('--button-after needs two monitor levels in 0..100')
    device = Monitor(button) if args.emulate == 'monitor' else ConsumerKeys(
        os.environ.get('KEYS_SIM_TRIGGER'), float(os.environ.get('KEYS_SIM_DELAY', '60')))

    def stop(signum, _frame):
        raise SystemExit(128 + signum)

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        return serve(device, args.socket, args.ready)
    except (OSError, struct.error) as error:
        print(f'brightness-usbredir: stopped: {error}', file=sys.stderr, flush=True)
        return 1


if __name__ == '__main__':
    sys.exit(main())
