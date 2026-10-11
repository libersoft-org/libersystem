#!/usr/bin/env python3
"""Short HID control/interrupt and owned-cleanup checks; no guest or Linux USB claims."""

from contextlib import redirect_stderr
import importlib.util
import io
import os
from pathlib import Path
import re
import select
import socket
import struct
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('brightness_usbredir', HERE / 'brightness-usbredir.py')
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
import usbredir_device as usb


class Wire:
    """The real redirection encoder and dispatch, observed over a Unix socketpair."""

    def __init__(self, case, device):
        self.case = case
        self.host, self.peer = socket.socketpair()
        self.peer.settimeout(1)
        case.addCleanup(self.host.close)
        case.addCleanup(self.peer.close)
        self.transport = usb.Redirection(self.host, device)

    def exact(self, length):
        result = b''
        while len(result) < length:
            chunk = self.peer.recv(length - len(result))
            self.case.assertTrue(chunk, 'redirection closed before the response completed')
            result += chunk
        return result

    def packet(self):
        kind, length, ident = struct.unpack('<III', self.exact(12))
        return kind, ident, self.exact(length)

    def configure(self, value=1):
        self.transport.handle(usb.SET_CONFIGURATION, 7, bytes([value]))
        self.case.assertEqual(self.packet()[0], usb.EP_INFO)
        self.case.assertEqual(self.packet()[0], usb.INTERFACE_INFO)
        self.case.assertEqual(self.packet(), (usb.CONFIGURATION_STATUS, 7, bytes([usb.SUCCESS, value])))

    def control(self, request_type, request, value, length=64, data=b'', index=0):
        endpoint = 0x80 if request_type & 0x80 else 0
        body = struct.pack('<BBBBHHH', endpoint, request, request_type, 0, value, index, length) + data
        self.transport.handle(usb.CONTROL_PACKET, 11, body)
        kind, ident, reply = self.packet()
        self.case.assertEqual((kind, ident), (usb.CONTROL_PACKET, 11))
        header = struct.unpack('<BBBBHHH', reply[:10])
        self.case.assertEqual(header[:3], (endpoint, request, request_type))
        self.case.assertEqual(header[4:6], (value, index))
        return header[3], header[6], reply[10:]

    def listen(self):
        self.transport.handle(usb.START_INTERRUPT_RECEIVING, 13, bytes([0x81]))
        self.case.assertEqual(self.packet(), (usb.INTERRUPT_RECEIVING_STATUS, 13, bytes([usb.SUCCESS, 0x81])))

    def report(self, expected):
        kind, _ident, body = self.packet()
        self.case.assertEqual(kind, usb.INTERRUPT_PACKET)
        self.case.assertEqual(body, struct.pack('<BBH', 0x81, usb.SUCCESS, len(expected)) + expected)

    def quiet(self):
        self.case.assertFalse(select.select([self.peer], [], [], 0)[0], 'unexpected report/control response')


class Reports(unittest.TestCase):
    def setUp(self):
        self.log = io.StringIO()
        self.capture = redirect_stderr(self.log)
        self.capture.__enter__()
        self.addCleanup(self.capture.__exit__, None, None, None)

    def test_descriptors_match_the_existing_gadget_and_control_endpoint(self):
        source = (HERE / 'usb-gadget.sh').read_text()
        for shape, model in (('hid-monitor', fixture.Monitor), ('hid-consumer', fixture.ConsumerKeys)):
            with self.subTest(shape=shape):
                literal = re.search(r'\n\s*' + shape + r"\)\n\s*printf '%b' '([^']+)'", source)[1]
                original = bytes(int(part, 16) for part in re.findall(r'\\x([0-9a-f]{2})', literal))
                device = model()
                wire = Wire(self, device)
                self.assertEqual(wire.control(0x81, 6, 0x2200, len(original)), (usb.SUCCESS, len(original), original))
                config = device.configuration_descriptor()
                self.assertEqual(struct.unpack_from('<H', config, 2)[0], len(config))
                self.assertEqual(usb.settings_of(config), [(0, 0, 3, 0, 0, [(0x81, 3, device.packet_size, 10)])])
                self.assertEqual(wire.control(0x81, 6, 0x2200, 4), (usb.SUCCESS, 4, original[:4]))
                self.assertEqual(wire.control(0x81, 6, 0x2200, index=1)[0], usb.STALL)
                self.assertEqual(wire.control(0x81, 6, 0x2201)[0], usb.STALL)

    def test_feature_reports_read_back_only_accepted_control_writes(self):
        device = fixture.Monitor()
        wire = Wire(self, device)
        self.assertEqual(wire.control(0xa1, 1, 0x0302)[0], usb.STALL)
        wire.configure()
        self.assertEqual(wire.control(0xa1, 1, 0x0301), (usb.SUCCESS, 33, b'\x01' + fixture.monitor.EDID))
        self.assertEqual(wire.control(0xa1, 1, 0x0302), (usb.SUCCESS, 2, b'\x02\x32'))
        for value, data, length, index in (
            (0x0302, b'\x03\x28', 2, 0), (0x0302, b'\x02', 2, 0),
            (0x0302, b'\x02\x28\x00', 3, 0), (0x0302, b'\x02\x65', 2, 0),
            (0x0301, b'\x02\x28', 2, 0), (0x0202, b'\x02\x28', 2, 0),
            (0x0302, b'\x02\x28', 2, 1)):
            with self.subTest(value=value, data=data, length=length, index=index):
                self.assertEqual(wire.control(0x21, 9, value, length, data, index), (usb.STALL, 0, b''))
                self.assertEqual(wire.control(0xa1, 1, 0x0302), (usb.SUCCESS, 2, b'\x02\x32'))
        for level in (0, 40, 100):
            self.assertEqual(wire.control(0x21, 9, 0x0302, 2, bytes([2, level])), (usb.SUCCESS, 2, b''))
            self.assertEqual(wire.control(0xa1, 1, 0x0302), (usb.SUCCESS, 2, bytes([2, level])))
        self.assertEqual(wire.control(0xa1, 1, 0x0102)[0], usb.STALL)
        self.assertEqual(wire.control(0xa1, 1, 0x0302, 1), (usb.SUCCESS, 1, b'\x02'))
        self.assertEqual(wire.control(0xa1, 2, 0x0302)[0], usb.STALL)

    def test_button_readback_no_echo_and_lux_use_real_interrupt_packets(self):
        with mock.patch.object(fixture.time, 'monotonic', return_value=0) as clock:
            device = fixture.Monitor((55, 63))
            wire = Wire(self, device)
            wire.configure()
            wire.listen()
            self.assertEqual(wire.control(0x21, 9, 0x0302, 2, b'\x02\x37')[0], usb.SUCCESS)
            clock.return_value = 1
            wire.transport.pump()
            wire.quiet()
            # A repeated SET must not push the already pending button event back.
            wire.control(0x21, 9, 0x0302, 2, b'\x02\x37')
            clock.return_value = 2
            wire.transport.pump()
            wire.report(b'\x04\x3f')
            wire.quiet()
            self.assertEqual(wire.control(0xa1, 1, 0x0302), (usb.SUCCESS, 2, b'\x02\x3f'))
            self.assertIn('device button changed brightness to 63', self.log.getvalue())
            self.assertNotIn('brightness set to 63', self.log.getvalue())
            for when, lux in ((5, 2000), (10, 10)):
                clock.return_value = when
                wire.transport.pump()
                wire.report(bytes([3]) + struct.pack('<H', lux))
                self.assertEqual(wire.control(0xa1, 1, 0x0103), (usb.SUCCESS, 3, bytes([3]) + struct.pack('<H', lux)))
            wire.transport.handle(usb.RESET, 0, b'')
            wire.configure()
            self.assertEqual(wire.control(0xa1, 1, 0x0302), (usb.SUCCESS, 2, b'\x02\x3f'))
            self.assertEqual(wire.control(0xa1, 1, 0x0103), (usb.SUCCESS, 3, b'\x03\x0a\x00'))
            wire.control(0x21, 9, 0x0302, 2, b'\x02\x37')
            clock.return_value = 12
            wire.transport.pump()
            wire.quiet()  # A completed button action does not rearm across reset.

    def test_pending_button_action_survives_a_bus_reset_once(self):
        with mock.patch.object(fixture.time, 'monotonic', return_value=0) as clock:
            device = fixture.Monitor((55, 63))
            wire = Wire(self, device)
            wire.configure()
            wire.listen()
            wire.control(0x21, 9, 0x0302, 2, b'\x02\x37')
            clock.return_value = 1
            wire.transport.handle(usb.RESET, 0, b'')
            clock.return_value = 3
            wire.transport.pump()
            wire.quiet()
            wire.configure()
            wire.transport.pump()
            wire.report(b'\x04\x3f')
            wire.transport.handle(usb.RESET, 0, b'')
            wire.configure()
            wire.transport.pump()
            wire.quiet()
            self.assertEqual(self.log.getvalue().count('device button changed brightness to 63'), 1)

    def test_unconfigure_and_bus_reset_drop_pending_input_until_reconfigured(self):
        with mock.patch.object(fixture.time, 'monotonic', return_value=0) as clock:
            device = fixture.Monitor((55, 63))
            wire = Wire(self, device)
            wire.configure()
            wire.listen()
            clock.return_value = 5
            device.poll()  # A due report not yet handed to the transport.
            self.assertTrue(device.reports)
            wire.transport.handle(usb.RESET, 0, b'')
            clock.return_value = 100
            wire.transport.pump()
            wire.quiet()
            self.assertFalse(device.reports)
            self.assertEqual(wire.control(0xa1, 1, 0x0302)[0], usb.STALL)
            wire.configure()
            clock.return_value = 105
            wire.transport.pump()
            wire.report(bytes([3]) + struct.pack('<H', 10))
            wire.configure(0)
            clock.return_value = 200
            wire.transport.pump()
            wire.quiet()

    def test_consumer_trigger_press_release_order_and_no_replay_after_reset(self):
        with tempfile.TemporaryDirectory() as work, mock.patch.object(fixture.time, 'monotonic', return_value=0) as clock:
            trigger = Path(work) / 'press'
            device = fixture.ConsumerKeys(str(trigger))
            wire = Wire(self, device)
            wire.listen()
            clock.return_value = 100
            wire.transport.pump()
            wire.quiet()
            wire.configure()
            wire.transport.pump()
            wire.quiet()
            trigger.touch()
            for when, usage in ((100, 0x6f), (100.11, 0), (102.12, 0x6f), (102.23, 0), (104.24, 0x70), (104.35, 0)):
                clock.return_value = when
                wire.transport.pump()
                wire.report(struct.pack('<H', usage))
                wire.quiet()
            clock.return_value = 106.36
            wire.transport.pump()
            wire.quiet()
            self.assertEqual(self.log.getvalue().count('brightness up pressed and released'), 2)
            self.assertEqual(self.log.getvalue().count('brightness down pressed and released'), 1)
            self.assertEqual(self.log.getvalue().count('all three presses made'), 1)
            wire.transport.handle(usb.RESET, 0, b'')
            wire.configure()
            clock.return_value = 1000
            wire.transport.pump()
            wire.quiet()
            self.assertTrue(device.done)

    def test_consumer_delay_and_interrupted_sequence_reset(self):
        with mock.patch.object(fixture.time, 'monotonic', return_value=0) as clock:
            device = fixture.ConsumerKeys(delay=60)
            wire = Wire(self, device)
            wire.configure()
            wire.listen()
            clock.return_value = 59
            wire.transport.pump()
            wire.quiet()
            clock.return_value = 60
            wire.transport.pump()
            wire.report(b'\x6f\x00')
            wire.transport.handle(usb.RESET, 0, b'')
            wire.configure()
            clock.return_value = 60.05
            wire.transport.pump()
            wire.quiet()
            clock.return_value = 60.11
            wire.transport.pump()
            wire.report(b'\x00\x00')  # Release the first press; do not replay it.
            clock.return_value = 62.12
            wire.transport.pump()
            wire.report(b'\x6f\x00')


class OwnedCleanup(unittest.TestCase):
    def start_fixture(self, work):
        path, ready = Path(work) / 'device.sock', Path(work) / 'ready'
        log = open(Path(work) / 'firmware.log', 'wb')
        self.addCleanup(log.close)
        process = subprocess.Popen([sys.executable, str(HERE / 'brightness-usbredir.py'), '--emulate', 'monitor',
                                    '--socket', str(path), '--ready', str(ready)], stderr=log,
                                   env={**os.environ, 'PYTHONDONTWRITEBYTECODE': '1'})

        def stop():
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
        self.addCleanup(stop)
        deadline = time.monotonic() + 5
        while not ready.exists() and process.poll() is None and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertTrue(ready.exists(), (Path(work) / 'firmware.log').read_text())
        self.assertIsNone(process.poll())
        return process, path, ready

    def test_eof_and_malformed_control_clean_owned_files(self):
        for malformed in (False, True):
            with self.subTest(malformed=malformed), tempfile.TemporaryDirectory() as work:
                process, path, ready = self.start_fixture(work)
                with socket.socket(socket.AF_UNIX) as peer:
                    peer.settimeout(2)
                    peer.connect(str(path))
                    self.assertTrue(peer.recv(4096))  # Read the real HELLO before closing.
                    if malformed:
                        peer.sendall(struct.pack('<III', usb.CONTROL_PACKET, 0, 1))
                        self.assertEqual(peer.recv(4096), b'')
                self.assertEqual(process.wait(timeout=3), 1 if malformed else 0)
                self.assertFalse(path.exists())
                self.assertFalse(ready.exists())

    def test_sigterm_cleans_only_owned_files(self):
        with tempfile.TemporaryDirectory() as work:
            process, path, ready = self.start_fixture(work)
            path.rename(path.with_suffix('.old'))
            path.write_text('a replacement owned by someone else\n')
            process.terminate()
            self.assertEqual(process.wait(timeout=3), 143)
            self.assertEqual(path.read_text(), 'a replacement owned by someone else\n')
            self.assertFalse(ready.exists())

    def test_existing_ready_file_is_not_overwritten_or_deleted(self):
        with tempfile.TemporaryDirectory() as work:
            path, ready = Path(work) / 'device.sock', Path(work) / 'ready'
            ready.write_text('existing owner\n')
            with self.assertRaises(FileExistsError):
                fixture.serve(fixture.Monitor(), str(path), str(ready))
            self.assertFalse(path.exists())
            self.assertEqual(ready.read_text(), 'existing owner\n')


if __name__ == '__main__':
    unittest.main()
