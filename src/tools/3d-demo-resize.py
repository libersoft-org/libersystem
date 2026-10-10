#!/usr/bin/env python3
"""Resize the Core visual gate's owned QEMU through its private VNC socket.

This deliberately supports only QEMU's RFB 3.8, no-auth, single-screen frontend.
SetDesktopSize forwards a real UIInfo event to virtio-gpu; status 4 means only
forwarded. Completion requires a status-0 ExtendedDesktopSize with the requested
extent. The gate separately checks captured pixels, Configure/rebuild and exit.
The caller saves --query's extent before changing it and restores that extent in
its cleanup. --owner-state proves the gate's existing PGID/private VNC association
from live process arguments; this helper never starts/stops guests or removes sockets.

Wire constants and status semantics: QEMU v10.0.13 ui/vnc.h and ui/vnc.c.
"""

import argparse
import json
import math
import os
from pathlib import Path
import socket
import struct
import sys
import time


EXTENDED_DESKTOP_SIZE = -308
SET_DESKTOP_SIZE = 251
MAX_WIDTH, MAX_HEIGHT = 5120, 2160  # QEMU VNC's documented implementation bounds.


PROCESS_ROOT = Path("/proc")


def owned_guest(state, endpoint):
    """Prove this gate's live QEMU against the lab's PGID and its private VNC argv."""
    try:
        group = int((Path(state) / "lab-guest.pgid").read_text().strip())
        if group <= 0:
            return None
    except (OSError, ValueError):
        return None
    for process in PROCESS_ROOT.iterdir():
        if not process.name.isdecimal():
            continue
        try:
            if os.getpgid(int(process.name)) != group:
                continue
            args = [value.decode() for value in (process / "cmdline").read_bytes().split(b"\0") if value]
        except (OSError, UnicodeError):
            continue
        if args and Path(args[0]).name.startswith("qemu-system-") and any(
                first == "-vnc" and second == "unix:" + str(endpoint)
                for first, second in zip(args, args[1:])):
            return {"pid": int(process.name), "pgid": group, "arguments": args}
    return None


def process_start(pid):
    """The existing lab identity's /proc stat field 22, including names with spaces."""
    try:
        data = (PROCESS_ROOT / str(pid) / "stat").read_bytes()
    except FileNotFoundError:
        return None
    closing = data.rfind(b")")
    fields = data[closing + 2:].split() if closing >= 0 else []
    if len(fields) < 20 or fields[0] == b"Z":
        return None
    try:
        return int(fields[19])
    except ValueError:
        return None


def live_guest(state):
    """Read-only admission: a missing shell prompt never means an existing guest is gone.

    Ordinary lab ownership is its recorded process group plus an actual QEMU in that
    group. Development records additionally carry the group leader's immutable start
    time. This only refuses replacement; it never grants authority to stop a process.
    """
    state = Path(state)
    records = []
    try:
        group = int((state / "lab-guest.pgid").read_text().strip())
        if group <= 0:
            raise ValueError("nonpositive lab PGID")
        records.append(("lab", group, None))
    except FileNotFoundError:
        pass
    except ValueError as error:
        raise ProtocolError("unreadable lab ownership identity") from error
    try:
        # The lab's existing lock probe may leave an empty, unowned lock file.
        # A live ordinary/development QEMU is still caught by lab-guest.pgid.
        identity = json.loads((state / "dev-instance.lock").read_text().strip() or "{}")
        if not isinstance(identity, dict):
            raise ValueError("invalid development identity")
        if identity.get("pgid"):
            group, started = identity["pgid"], identity.get("pgid_started")
            if not isinstance(group, int) or group <= 0 or not isinstance(started, int) or started < 0:
                raise ValueError("unverifiable development identity")
            records.append(("development", group, started))
    except FileNotFoundError:
        pass
    except ValueError as error:
        raise ProtocolError("unreadable development ownership identity") from error
    for kind, group, expected_start in records:
        if expected_start is not None and process_start(group) != expected_start:
            continue
        for process in PROCESS_ROOT.iterdir():
            if not process.name.isdecimal():
                continue
            pid = int(process.name)
            try:
                if os.getpgid(pid) != group:
                    continue
                started = process_start(pid)
                args = [value.decode() for value in (process / "cmdline").read_bytes().split(b"\0") if value]
                if (started is not None and args and Path(args[0]).name.startswith("qemu-system-")
                        and process_start(pid) == started and os.getpgid(pid) == group
                        and (expected_start is None or process_start(group) == expected_start)):
                    return {"kind": kind, "pid": pid, "started": started, "pgid": group, "arguments": args}
            except (FileNotFoundError, ProcessLookupError):
                continue
            except UnicodeError as error:
                raise ProtocolError("unreadable live process arguments") from error
    return None


class ProtocolError(RuntimeError):
    pass


def extent(width, height):
    if not (0 < width <= MAX_WIDTH and 0 < height <= MAX_HEIGHT):
        raise ProtocolError(f"unsupported QEMU VNC extent {width}x{height}")
    return width, height


class Client:
    def __init__(self, stream, deadline):
        self.stream = stream
        self.deadline = deadline

    def arm(self):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("VNC handshake/resize deadline expired")
        self.stream.settimeout(remaining)

    def read(self, size):
        data = bytearray()
        while len(data) < size:
            self.arm()
            part = self.stream.recv(size - len(data))
            if not part:
                raise ProtocolError("VNC closed before the requested resize completed")
            data.extend(part)
        return bytes(data)

    def send(self, data):
        self.arm()
        self.stream.sendall(data)

    def handshake(self):
        if self.read(12) != b"RFB 003.008\n":
            raise ProtocolError("the gate requires QEMU RFB 3.8")
        self.send(b"RFB 003.008\n")
        count = self.read(1)[0]
        if count == 0 or 1 not in self.read(count):
            raise ProtocolError("the gate's private VNC does not offer no-auth")
        self.send(b"\x01")
        if struct.unpack(">I", self.read(4))[0] != 0:
            raise ProtocolError("VNC authentication was refused")
        self.send(b"\x01")  # Shared: never evict another client.
        header = self.read(24)
        width, height = struct.unpack_from(">HH", header)
        self.bytes_per_pixel = header[4] // 8
        if header[4] not in (8, 16, 32) or header[7] != 1:
            raise ProtocolError("unsupported VNC pixel format")
        name_size = struct.unpack_from(">I", header, 20)[0]
        if name_size > 4096:
            raise ProtocolError("oversized VNC server name")
        self.read(name_size)
        return extent(width, height)

    def resize(self, width, height):
        requested = extent(width, height)
        original = self.handshake()
        result = {"original": list(original), "requested": list(requested),
                  "actual": list(original), "changed": False, "statuses": []}
        if original == requested:
            return result
        self.send(struct.pack(">BBHii", 2, 0, 2, 0, EXTENDED_DESKTOP_SIZE))
        self.send(struct.pack(">BBHHBBIHHHHI", SET_DESKTOP_SIZE, 0,
                              width, height, 1, 0, 0, 0, 0, width, height, 0))
        # QEMU sends resize events without framebuffer polling. No pixel stream
        # is requested: the independent screenshot path owns visual evidence.
        while True:
            message = self.read(1)[0]
            if message == 2:  # Bell has no payload.
                continue
            if message == 3:  # Ordinary server clipboard, not negotiated extensions.
                size = struct.unpack(">I", self.read(7)[3:])[0]
                if size > 65536:
                    raise ProtocolError("oversized VNC clipboard message")
                self.read(size)
                continue
            if message != 0:
                raise ProtocolError(f"unexpected VNC server message {message}")
            rectangles = struct.unpack(">H", self.read(3)[1:])[0]
            if rectangles > 4096:
                raise ProtocolError("oversized VNC rectangle list")
            completed = False
            for _ in range(rectangles):
                reason, status, current_w, current_h, encoding = struct.unpack(">HHHHi", self.read(12))
                if encoding == 0:
                    # A queued raw rectangle is harmless, but may not allocate
                    # an unbounded response or count as resize confirmation.
                    extent(current_w, current_h)
                    size = current_w * current_h * self.bytes_per_pixel
                    while size:
                        chunk = min(size, 65536)
                        self.read(chunk)
                        size -= chunk
                    continue
                if encoding != EXTENDED_DESKTOP_SIZE:
                    raise ProtocolError(f"unrequested VNC encoding {encoding}")
                screens = self.read(4)[0]
                if screens != 1:
                    raise ProtocolError("the gate requires one VNC screen")
                _screen_id, x, y, screen_w, screen_h, _flags = struct.unpack(">IHHHHI", self.read(16))
                if (x, y, screen_w, screen_h) != (0, 0, current_w, current_h):
                    raise ProtocolError("inconsistent VNC screen extent")
                extent(current_w, current_h)
                result["statuses"].append({"reason": reason, "status": status,
                                           "width": current_w, "height": current_h})
                if reason not in (0, 1, 2) or status not in (0, 4):
                    raise ProtocolError(f"VNC resize refused: reason={reason}, status={status}")
                if status == 0 and (current_w, current_h) == requested:
                    result["actual"] = [current_w, current_h]
                    result["changed"] = True
                    completed = True
            if completed:
                return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket")
    parser.add_argument("--live-state", help="read-only existing lab/development guest identity, without a shell request")
    parser.add_argument("--query", action="store_true")
    parser.add_argument("--owner-state", help="gate-private lab PGID directory; inspect ownership without connecting")
    parser.add_argument("--width", type=int)
    parser.add_argument("--height", type=int)
    parser.add_argument("--timeout", type=float, default=30)
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or not 0 < args.timeout <= 120:
        parser.error("--timeout must be finite and between 0 and 120 seconds")
    if args.live_state is not None:
        if args.socket or args.owner_state or args.query or args.width is not None or args.height is not None:
            parser.error("--live-state cannot be combined with socket/ownership/query/resize")
        try:
            guest = live_guest(args.live_state)
            print(json.dumps(guest, sort_keys=True))
            return 0 if guest is not None else 1
        except (OSError, ProtocolError) as error:
            print(f"3d-demo-resize: {error}", file=sys.stderr)
            return 2
    if not args.socket:
        parser.error("--socket is required for ownership/query/resize")
    if args.owner_state is not None:
        if args.query or args.width is not None or args.height is not None:
            parser.error("--owner-state cannot be combined with query/resize")
        owner = owned_guest(args.owner_state, args.socket)
        print(json.dumps(owner, sort_keys=True))
        return 0 if owner is not None else 1
    if args.query == (args.width is not None or args.height is not None):
        parser.error("choose --query or both --width and --height")
    if not args.query and (args.width is None or args.height is None):
        parser.error("--width and --height are both required")
    try:
        if not args.query:
            extent(args.width, args.height)
        deadline = time.monotonic() + args.timeout
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
            stream.settimeout(args.timeout)
            stream.connect(args.socket)
            client = Client(stream, deadline)
            if args.query:
                width, height = client.handshake()
                result = {"width": width, "height": height}
            else:
                result = client.resize(args.width, args.height)
        print(json.dumps(result, sort_keys=True))
        return 0
    except (OSError, ProtocolError) as error:
        print(f"3d-demo-resize: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
