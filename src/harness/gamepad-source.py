#!/usr/bin/env python3
"""The far end of the gamepad-pair gadget: two players' hands, played by a process.

WHAT IT IS FOR. The USB gamepad proof binds a composite device with two HID gamepad functions and plays
InputService's side of the `gamepad` provider; what the two pads report has to come from somewhere that is not
the guest. This process writes the two functions' reports:

  1. LEVEL ONE, held: interface 0 holds button 1 with X at 0 and its hat at the null value 15; interface 1
     holds button 16, its hat east and Rz at 255. Every other axis is at 128.
  2. LEVEL TWO, held: both released, both hats at the null value 15.
  3. It UNBINDS THE GADGET, which to the guest is the device unplugged - both gamepads leave at once.

LEVELS AND NOT EDGES, each held for two hundred reports. A write to a HID function waits until the guest has
taken the report before it, so the holds are paced by the guest's polling and not by this host's clock: a
level the guest was slow to start reading is still there when it reads.

Each function's device node is found by the `dev` attribute of its configfs directory rather than by assuming
the `/dev/hidgN` numbering, and nothing is written before the GUEST has configured the device - see
`gadget_state`. It exits when the device is gone or when the run's teardown stops it.
"""

import errno
import os
import sys
import time

from gadget_state import state_value, wait_for_guest_configuration

# THE GADGET'S REPORT: X, Y, Z, Rz, sixteen buttons, a hat nibble and four bits of padding - see
# `usb-gadget.sh hid-gamepad`.
HOLD = 200
NULL_HAT = 15
EAST = 2


def say(message):
    print(f"gamepad-source: {message}", file=sys.stderr, flush=True)


def report(x, y, z, rz, buttons, hat):
    return bytes([x, y, z, rz, buttons & 0xFF, buttons >> 8, hat & 0x0F])


def gadget_dir():
    name = state_value("name")
    if not name or not name.startswith("liber"):
        return None
    return os.path.join("/sys/kernel/config/usb_gadget", name)


def device_node(function, seconds):
    """The character device of one HID function, by the `major:minor` its configfs directory states."""
    base = gadget_dir()
    if base is None:
        return None
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            with open(os.path.join(base, "functions", function, "dev")) as handle:
                major, minor = (int(part) for part in handle.read().strip().split(":"))
            for entry in os.listdir("/dev"):
                path = os.path.join("/dev", entry)
                try:
                    status = os.stat(path)
                except OSError:
                    continue
                if os.major(status.st_rdev) == major and os.minor(status.st_rdev) == minor and entry.startswith("hidg"):
                    return path
        except (OSError, ValueError):
            pass
        time.sleep(0.2)
    return None


def hold(devices, reports, count):
    """Write each function's report `count` times, alternating, each write paced by the guest."""
    for _ in range(count):
        for fd, level in zip(devices, reports):
            os.write(fd, level)


def unplug():
    base = gadget_dir()
    if base is None:
        return False
    with open(os.path.join(base, "UDC"), "w") as handle:
        handle.write("\n")
    return True


def main():
    nodes = [device_node(function, 30) for function in ("hid.usb0", "hid.usb1")]
    if None in nodes:
        say("the two gamepad functions' device nodes did not appear")
        return 1
    if not wait_for_guest_configuration():
        say("the guest never configured the device")
        return 1
    devices = [os.open(node, os.O_WRONLY) for node in nodes]
    try:
        rest = report(128, 128, 128, 128, 0, NULL_HAT)
        hold(devices, [report(0, 128, 128, 128, 0x0001, NULL_HAT), report(128, 128, 128, 255, 0x8000, EAST)], HOLD)
        say("level one held")
        hold(devices, [rest, rest], HOLD)
        say("level two held")
    except OSError as error:
        if error.errno in (errno.ESHUTDOWN, errno.ENODEV, errno.EPIPE):
            say("the device went away before the levels were played")
            return 1
        raise
    finally:
        for fd in devices:
            os.close(fd)
    if unplug():
        say("unplugged")
    return 0


if __name__ == "__main__":
    sys.exit(main())
