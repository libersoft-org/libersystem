#!/usr/bin/env python3
"""The far end of the UPS gadget: its firmware, played by a process.

The kernel's HID function answers GET_REPORT from reports registered with it and hands SET_REPORT to this process
through its read side; the interrupt pipe carries whatever this process writes. So this is the UPS:

  1. It registers its three reports - on mains and charging at 80 % with an hour left, 100 % full and design
     capacity, a 20 % limit, 13.80 V, and no turn-off scheduled - so a GET_REPORT is answered before anything
     else happens.
  2. Once the guest has configured the device it sends input report 1, as a UPS does when a host starts
     listening.
  3. A SET_REPORT of DelayBeforeShutdown is a command: a delay puts it on battery (mains gone, discharging, 79 %,
     half an hour left) and minus one, a cancel, puts it back on mains. Each change is an input report, which is
     what the oracle reads - so a state the guest sees change is a command this process received.

It runs until the run's teardown stops it.
"""

import errno
import os
import select
import struct
import sys
import time

from gadget_state import wait_for_guest_configuration

# linux/usb/g_hid.h: GADGET_HID_WRITE_GET_REPORT, _IOW('g', 0x42, struct usb_hidg_report) - a report ID, whether
# userspace answers each request (0: answer from this data at once), a length, 64 bytes of data and 4 of padding.
GADGET_HID_WRITE_GET_REPORT = 0x40486742
DEVICE = "/dev/hidg0"

MAINS = 0b0_0011  # AC present, charging
BATTERY = 0b0_0100  # discharging


def say(message):
    print(f"ups-sim: {message}", file=sys.stderr, flush=True)


def input_report(flags, capacity, runtime):
    return bytes([1, flags, capacity]) + struct.pack("<H", runtime)


FEATURE_2 = bytes([2, 2, 100, 100, 20]) + struct.pack("<H", 1380)


def feature_3(delay):
    return bytes([3]) + struct.pack("<h", delay)


def register(fd, report):
    import fcntl

    data = report.ljust(64, b"\0")
    fcntl.ioctl(fd, GADGET_HID_WRITE_GET_REPORT, struct.pack("<BBH64s4x", report[0], 0, len(report), data))


def send(fd, report):
    deadline = time.monotonic() + 30
    while True:
        try:
            os.write(fd, report)
            return True
        except OSError as error:
            if error.errno not in (errno.EAGAIN, errno.EINTR) or time.monotonic() > deadline:
                raise
            time.sleep(0.02)


def main():
    deadline = time.monotonic() + 900
    fd = None
    while fd is None:
        try:
            fd = os.open(DEVICE, os.O_RDWR | os.O_NONBLOCK)
        except OSError as error:
            if error.errno not in (errno.ENOENT, errno.ENODEV) or time.monotonic() > deadline:
                raise
            time.sleep(0.2)
    state = input_report(MAINS, 80, 3600)
    register(fd, state)
    register(fd, FEATURE_2)
    register(fd, feature_3(-1))
    say("reports registered: on mains, charging, 80 %, an hour; 13.80 V; no turn-off scheduled")
    if not wait_for_guest_configuration():
        say("the guest never configured the device")
        return 1
    time.sleep(0.5)
    send(fd, state)
    say("input report sent")
    while True:
        ready, _, _ = select.select([fd], [], [], 1.0)
        if not ready:
            continue
        try:
            written = os.read(fd, 64)
        except OSError as error:
            if error.errno in (errno.EAGAIN, errno.EINTR):
                continue
            # THE HOST TOOK THE DEVICE BACK - the guest is gone, and the function dropped what it held for this
            # side. Nothing more will come; this waits to be stopped rather than calling it a failure.
            if error.errno in (errno.ENOMEM, errno.ESHUTDOWN, errno.ENODEV):
                time.sleep(1.0)
                continue
            raise
        if len(written) < 3 or written[0] != 3:
            say(f"a SET_REPORT this UPS has no control for: {written.hex(' ')}")
            continue
        delay = struct.unpack("<h", written[1:3])[0]
        register(fd, feature_3(delay))
        if delay >= 0:
            state = input_report(BATTERY, 79, 1800)
            say(f"a turn-off is scheduled in {delay} s - mains gone, on battery")
        else:
            state = input_report(MAINS, 79, 3600)
            say("the turn-off is cancelled - back on mains")
        register(fd, state)
        send(fd, state)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except OSError as error:
        say(f"stopped: {error}")
        sys.exit(1)
