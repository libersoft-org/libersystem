#!/usr/bin/env python3
"""The far end of the monitor gadget: its firmware, played by a process.

The kernel's HID function answers GET_REPORT from reports registered with it and hands SET_REPORT to this process
through its read side; the interrupt pipe carries whatever this process writes. So this is the monitor:

  1. It registers its reports - the start of an EDID naming manufacturer 0x10ac, product 0x4050, serial 7; a
     brightness of 50; and an illuminance - so a GET_REPORT is answered before anything else happens.
  2. A SET_REPORT of the brightness is recorded on its log, `brightness set to N`, and registered as the current one,
     so the host reads back what it wrote - which is the oracle: a level the gate sees set is a value this process
     received.
  3. Once the guest has configured the device it steps the light, DARK then BRIGHT then DARK and round again, an input
     report at each step: ten lux and two thousand, five seconds each. Automatic brightness, once the gate turns it
     on, follows them.

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

DARK = 10
BRIGHT = 2000
STEP_SECONDS = 5.0

EDID = bytes([0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x10, 0xAC, 0x50, 0x40]) + struct.pack("<I", 7) + bytes(16)


def say(message):
    print(f"monitor-sim: {message}", file=sys.stderr, flush=True)


def brightness_report(level):
    return bytes([2, level])


def light_report(lux):
    return bytes([3]) + struct.pack("<H", lux)


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
    lux = DARK
    register(fd, bytes([1]) + EDID)
    register(fd, brightness_report(50))
    register(fd, light_report(lux))
    say("reports registered: EDID 10ac:4050 serial 7, brightness 50, ten lux")
    if not wait_for_guest_configuration():
        say("the guest never configured the device")
        return 1
    say("the guest configured the device")
    next_step = time.monotonic() + STEP_SECONDS
    while True:
        now = time.monotonic()
        if now >= next_step:
            lux = BRIGHT if lux == DARK else DARK
            register(fd, light_report(lux))
            try:
                send(fd, light_report(lux))
                say(f"illuminance {lux} lux")
            except OSError as error:
                say(f"the illuminance report was not taken: {error}")
            next_step = now + STEP_SECONDS
        ready, _, _ = select.select([fd], [], [], max(0.0, min(1.0, next_step - time.monotonic())))
        if not ready:
            continue
        try:
            written = os.read(fd, 64)
        except OSError as error:
            if error.errno in (errno.EAGAIN, errno.EINTR):
                continue
            # THE HOST TOOK THE DEVICE BACK - the guest is gone. Nothing more will come; this waits to be stopped.
            if error.errno in (errno.ENOMEM, errno.ESHUTDOWN, errno.ENODEV):
                time.sleep(1.0)
                continue
            raise
        if len(written) < 2 or written[0] != 2:
            say(f"a SET_REPORT this monitor has no control for: {written.hex(' ')}")
            continue
        level = written[1]
        register(fd, brightness_report(level))
        say(f"brightness set to {level}")


if __name__ == "__main__":
    try:
        sys.exit(main())
    except OSError as error:
        say(f"stopped: {error}")
        sys.exit(1)
