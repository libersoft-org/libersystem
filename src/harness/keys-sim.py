#!/usr/bin/env python3
"""The far end of the consumer-keys gadget: a person's finger, played by a process.

Once the guest has configured the device - and, where `KEYS_SIM_TRIGGER` names a file, once that file exists, so the
gate decides when; otherwise after `KEYS_SIM_DELAY` seconds - it presses brightness up twice and brightness down once,
each a press and a release in input reports, two seconds apart, and logs each. The usages are the Consumer page's
brightness increment (0x6f) and decrement (0x70).

It runs until the run's teardown stops it.
"""

import errno
import os
import struct
import sys
import time

from gadget_state import wait_for_guest_configuration

DEVICE = "/dev/hidg0"
UP = 0x6F
DOWN = 0x70


def say(message):
    print(f"keys-sim: {message}", file=sys.stderr, flush=True)


def send(fd, usage):
    deadline = time.monotonic() + 30
    report = struct.pack("<H", usage)
    while True:
        try:
            os.write(fd, report)
            return
        except OSError as error:
            if error.errno not in (errno.EAGAIN, errno.EINTR) or time.monotonic() > deadline:
                raise
            time.sleep(0.02)


def main():
    pause = float(os.environ.get("KEYS_SIM_DELAY", "60"))
    deadline = time.monotonic() + 900
    fd = None
    while fd is None:
        try:
            fd = os.open(DEVICE, os.O_RDWR | os.O_NONBLOCK)
        except OSError as error:
            if error.errno not in (errno.ENOENT, errno.ENODEV) or time.monotonic() > deadline:
                raise
            time.sleep(0.2)
    if not wait_for_guest_configuration():
        say("the guest never configured the device")
        return 1
    trigger = os.environ.get("KEYS_SIM_TRIGGER")
    if trigger:
        say(f"the guest configured the device; pressing once {trigger} exists")
        while not os.path.exists(trigger):
            time.sleep(0.2)
    else:
        say(f"the guest configured the device; pressing in {pause:.0f} s")
        time.sleep(pause)
    for name, usage in (("up", UP), ("up", UP), ("down", DOWN)):
        send(fd, usage)
        time.sleep(0.1)
        send(fd, 0)
        say(f"brightness {name} pressed and released")
        time.sleep(2.0)
    say("all three presses made")
    while True:
        time.sleep(60)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except OSError as error:
        say(f"stopped: {error}")
        sys.exit(1)
