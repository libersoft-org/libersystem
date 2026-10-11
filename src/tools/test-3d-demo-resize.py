#!/usr/bin/env python3
"""Socketpair protocol regressions; no QEMU or guest is started."""

import importlib.util
from pathlib import Path
import socket
import struct
import sys
import threading
import time
import unittest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("demo_resize", Path(__file__).with_name("3d-demo-resize.py"))
resize = importlib.util.module_from_spec(spec)
spec.loader.exec_module(resize)


def receive(stream, size):
    data = bytearray()
    while len(data) < size:
        part = stream.recv(size - len(data))
        if not part:
            raise AssertionError("client ended before its complete request")
        data.extend(part)
    return bytes(data)


def handshake(stream, original=(1280, 800)):
    stream.sendall(b"RFB 003.008\n")
    assert receive(stream, 12) == b"RFB 003.008\n"
    stream.sendall(b"\x01\x01")
    assert receive(stream, 1) == b"\x01"
    stream.sendall(bytes(4))
    assert receive(stream, 1) == b"\x01"  # Never exclusive.
    pixel_format = struct.pack(">BBBBHHHBBB3x", 32, 24, 0, 1, 255, 255, 255, 16, 8, 0)
    server_init = struct.pack(">HH", *original) + pixel_format + struct.pack(">I", 4) + b"QEMU"
    for byte in server_init:
        stream.sendall(bytes([byte]))  # Exercise partial reads.


def requested(stream, size=(480, 800)):
    # These are independent fixed RFB protocol values, not helper constants.
    assert receive(stream, 12) == struct.pack(">BBHii", 2, 0, 2, 0, -308)
    width, height = size
    assert receive(stream, 24) == struct.pack(">BBHHBBIHHHHI", 251, 0,
                                            width, height, 1, 0, 0, 0, 0, width, height, 0)


def event(width, height, status=0, screens=1, screen_width=None, encoding=-308):
    header = struct.pack(">BBHHHHHi", 0, 0, 1, 1 if status else 0, status, width, height, encoding)
    layout = struct.pack(">B3xIHHHHI", screens, 0, 0, 0,
                         width if screen_width is None else screen_width, height, 0)
    return header + layout


class RfbResizeTests(unittest.TestCase):
    def exercise(self, peer, action=None, timeout=0.4):
        client, server = socket.socketpair()
        failures = []

        def run_peer():
            try:
                with server:
                    server.settimeout(1)
                    peer(server)
            except Exception as error:
                failures.append(error)

        thread = threading.Thread(target=run_peer, daemon=True)
        thread.start()
        try:
            with client:
                protocol = resize.Client(client, time.monotonic() + timeout)
                return (action or (lambda c: c.resize(480, 800)))(protocol)
        finally:
            thread.join(2)
            self.assertFalse(thread.is_alive(), "fixture peer did not finish")
            if failures:
                raise failures[0]

    def test_forwarded_then_actual_resize_with_fragmented_handshake(self):
        def peer(stream):
            handshake(stream)
            requested(stream)
            stream.sendall(event(1280, 800, 4))
            stream.sendall(event(480, 800))

        result = self.exercise(peer)
        self.assertEqual(result["original"], [1280, 800])
        self.assertEqual(result["actual"], [480, 800])
        self.assertTrue(result["changed"])
        self.assertEqual([x["status"] for x in result["statuses"]], [4, 0])

    def test_forwarded_only_is_not_success_on_eof(self):
        def peer(stream):
            handshake(stream)
            requested(stream)
            stream.sendall(event(1280, 800, 4))

        with self.assertRaisesRegex(resize.ProtocolError, "closed"):
            self.exercise(peer)

    def test_forwarded_only_has_a_deadline(self):
        def peer(stream):
            handshake(stream)
            requested(stream)
            stream.sendall(event(1280, 800, 4))
            self.assertEqual(stream.recv(1), b"")

        with self.assertRaises(TimeoutError):
            self.exercise(peer, timeout=0.08)

    def test_old_extent_success_cannot_satisfy_requested_resize(self):
        def peer(stream):
            handshake(stream)
            requested(stream)
            stream.sendall(event(1280, 800))

        with self.assertRaisesRegex(resize.ProtocolError, "closed"):
            self.exercise(peer)

    def test_typed_rejection_fails(self):
        def peer(stream):
            handshake(stream)
            requested(stream)
            stream.sendall(event(1280, 800, 3))

        with self.assertRaisesRegex(resize.ProtocolError, "refused"):
            self.exercise(peer)

    def test_inconsistent_screen_layout_cannot_pass(self):
        def peer(stream):
            handshake(stream)
            requested(stream)
            stream.sendall(event(480, 800, screen_width=479))

        with self.assertRaisesRegex(resize.ProtocolError, "inconsistent"):
            self.exercise(peer)

    def test_unnegotiated_encoding_cannot_be_parsed_as_success(self):
        def peer(stream):
            handshake(stream)
            requested(stream)
            stream.sendall(event(480, 800, encoding=16))

        with self.assertRaisesRegex(resize.ProtocolError, "unrequested"):
            self.exercise(peer)

    def test_raw_pixels_bell_clipboard_are_not_confirmation(self):
        def peer(stream):
            handshake(stream)
            requested(stream)
            raw = struct.pack(">BBHHHHHi", 0, 0, 1, 0, 0, 2, 1, 0) + bytes(8)
            stream.sendall(b"\x02\x03\0\0\0" + struct.pack(">I", 3) + b"abc" + raw)
            stream.sendall(event(480, 800))

        self.assertTrue(self.exercise(peer)["changed"])

    def test_query_does_not_send_resize_or_framebuffer_requests(self):
        def peer(stream):
            handshake(stream)
            self.assertEqual(stream.recv(1), b"")

        self.assertEqual(self.exercise(peer, lambda c: c.handshake()), (1280, 800))

    def test_same_extent_is_an_explicit_noop_and_restoration_uses_saved_size(self):
        def unchanged(stream):
            handshake(stream, (480, 800))
            self.assertEqual(stream.recv(1), b"")

        self.assertFalse(self.exercise(unchanged)["changed"])

        def restored(stream):
            handshake(stream, (480, 800))
            requested(stream, (1280, 800))
            stream.sendall(event(480, 800, 4) + event(1280, 800))

        result = self.exercise(restored, lambda c: c.resize(1280, 800))
        self.assertEqual(result["actual"], [1280, 800])
        self.assertTrue(result["changed"])

    def test_invalid_requested_size_sends_nothing(self):
        def peer(stream):
            self.assertEqual(stream.recv(1), b"")

        with self.assertRaisesRegex(resize.ProtocolError, "unsupported"):
            self.exercise(peer, lambda c: c.resize(0, 800))

    def test_no_auth_is_required_before_any_resize(self):
        def peer(stream):
            stream.sendall(b"RFB 003.008\n")
            self.assertEqual(receive(stream, 12), b"RFB 003.008\n")
            stream.sendall(b"\x01\x02")
            self.assertEqual(stream.recv(1), b"")

        with self.assertRaisesRegex(resize.ProtocolError, "no-auth"):
            self.exercise(peer)

    def test_partial_traffic_does_not_restart_overall_deadline(self):
        def peer(stream):
            handshake(stream)
            requested(stream)
            stream.sendall(event(1280, 800, 4))
            for _ in range(10):
                try:
                    stream.sendall(b"\x02")
                except BrokenPipeError:
                    return
                time.sleep(0.02)
            self.fail("client ignored its overall deadline")

        with self.assertRaises(TimeoutError):
            self.exercise(peer, timeout=0.07)




class RfbOwnershipTests(unittest.TestCase):
    def inspect(self, arguments=None, group=71, actual_group=71, stale=False):
        import tempfile
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            state, proc = root / 'state', root / 'proc'
            state.mkdir()
            proc.mkdir()
            (state / 'lab-guest.pgid').write_text(str(group))
            process = proc / '123'
            process.mkdir()
            args = arguments or ['qemu-system-x86_64', '-vnc', 'unix:/tmp/owned/display.sock']
            (process / 'cmdline').write_bytes(b'\0'.join(arg.encode() for arg in args) + b'\0')
            with patch.object(resize, 'PROCESS_ROOT', proc), patch.object(resize.os, 'getpgid',
                    side_effect=ProcessLookupError() if stale else None, return_value=actual_group):
                return resize.owned_guest(state, '/tmp/owned/display.sock')

    def test_current_group_and_exact_private_endpoint_prove_ownership(self):
        self.assertEqual(self.inspect()['pgid'], 71)

    def test_other_group_does_not_prove_ownership(self):
        self.assertIsNone(self.inspect(actual_group=72))

    def test_other_endpoint_does_not_prove_ownership(self):
        self.assertIsNone(self.inspect(['qemu-system-x86_64', '-vnc', 'unix:/tmp/foreign/display.sock']))

    def test_endpoint_as_unrelated_argument_does_not_prove_ownership(self):
        self.assertIsNone(self.inspect(['qemu-system-x86_64', '-name', 'unix:/tmp/owned/display.sock']))

    def test_non_qemu_process_does_not_prove_ownership(self):
        self.assertIsNone(self.inspect(['python3', '-vnc', 'unix:/tmp/owned/display.sock']))

    def test_stale_group_record_does_not_prove_ownership(self):
        self.assertIsNone(self.inspect(stale=True))

    def test_nonpositive_group_is_not_a_process_group(self):
        self.assertIsNone(self.inspect(group=0, actual_group=0))


class RfbAdmissionTests(unittest.TestCase):
    """Exact production admission with proc/getpgid files substituted; no process is launched."""

    def inspect(self, kind="lab", recorded_start=900, actual_start=900, arguments=None,
                actual_group=71, gone=False, invalid=False):
        import json
        import tempfile
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            state, proc = root / "state", root / "proc"
            state.mkdir()
            proc.mkdir()
            if kind == "lab":
                (state / "lab-guest.pgid").write_text("invalid" if invalid else "71")
            elif kind == "development":
                (state / "dev-instance.lock").write_text("invalid" if invalid else json.dumps({"pgid": 71, "pgid_started": recorded_start}))
            if kind == "empty-development":
                (state / "dev-instance.lock").write_text("")
            for pid in (71, 123):
                directory = proc / str(pid)
                directory.mkdir()
                # Field 2 deliberately contains spaces/parentheses; field 22 is index19 after it.
                fields = ["S"] + ["0"] * 18 + [str(actual_start)]
                (directory / "stat").write_text(f"{pid} (qemu (name)) " + " ".join(fields))
                args = (["bash", "run.sh"] if pid == 71 else arguments or ["qemu-system-x86_64", "-smp", "32"])
                (directory / "cmdline").write_bytes(b"\0".join(arg.encode() for arg in args) + b"\0")
            def group(pid):
                if gone:
                    raise ProcessLookupError()
                return actual_group
            with patch.object(resize, "PROCESS_ROOT", proc), patch.object(resize.os, "getpgid", side_effect=group):
                return resize.live_guest(state)

    def test_ordinary_live_qemu_does_not_need_a_prompt_or_vnc(self):
        result = self.inspect()
        self.assertEqual((result["pid"], result["pgid"], result["started"], result["kind"]), (123, 71, 900, "lab"))

    def test_development_record_requires_its_exact_start_time(self):
        self.assertEqual(self.inspect(kind="development")["kind"], "development")
        self.assertIsNone(self.inspect(kind="development", recorded_start=899))

    def test_stale_and_foreign_groups_do_not_identify_a_guest(self):
        self.assertIsNone(self.inspect(gone=True))
        self.assertIsNone(self.inspect(actual_group=72))

    def test_non_qemu_process_is_not_a_guest(self):
        self.assertIsNone(self.inspect(arguments=["python3", "worker.py"]))

    def test_no_identity_is_not_a_guest(self):
        self.assertIsNone(self.inspect(kind="missing"))

    def test_empty_unowned_development_lock_is_not_a_guest(self):
        self.assertIsNone(self.inspect(kind="empty-development"))

    def test_malformed_record_is_refused_instead_of_claiming_absence(self):
        with self.assertRaises(resize.ProtocolError):
            self.inspect(invalid=True)
        with self.assertRaises(resize.ProtocolError):
            self.inspect(kind="development", invalid=True)


if __name__ == "__main__":
    unittest.main(verbosity=2)
