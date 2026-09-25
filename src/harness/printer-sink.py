#!/usr/bin/env python3
"""The far end of the printer gadget: the paper, played by a process.

WHAT IT IS FOR. The USB printer oracle prints two documents through the guest's printer class module, and
the only honest judge of what a printer received is the printer. So this process reads `/dev/g_printer0`
- the gadget side of the function `usb-gadget.sh` built - and answers through the one channel a printer
has back to its host, the port status byte:

  1. It reports SELECTED and NOT ERROR, as an idle printer does.
  2. It reads the first document, PAUSING once after the first page, long enough that the guest's writes
     have to wait on it: the gadget's queue is two requests deep, so the host stops taking bytes within a
     few pages and the guest's transfer sits until this process reads again. That is backpressure, felt.
  3. It compares every byte with the document the oracle sends (the formula below is the oracle's), and
     raises PAPER EMPTY - the oracle's signal that the job is over - with NOT ERROR cleared if anything
     differed. A byte lost, doubled or reordered anywhere in the guest's stack is an error the guest reads.
  4. It reads the start of a second document and then UNBINDS THE GADGET, which to the guest is the
     printer's cable pulled out mid-job. What it unbinds is the gadget `usb-gadget.sh` recorded as its
     own, and nothing else - the teardown then finds it already unbound, which it handles.

It exits when the device is gone or when the run's teardown stops it.
"""

import argparse
import errno
import fcntl
import os
import sys
import time

# linux/usb/g_printer.h: the status bits and the ioctl that sets them. The argument is the status itself.
GADGET_SET_PRINTER_STATUS = 0xC0016722
NOT_ERROR = 0x08
SELECTED = 0x10
PAPER_EMPTY = 0x20

# THE ORACLE'S DOCUMENTS - `usb_printer_*` in the kernel's hardware suite sends exactly these.
FIRST_SEED = 1
FIRST_LENGTH = 49152
SECOND_SEED = 2
PAUSE_AFTER = 4096
PAUSE_SECONDS = 2.0
UNPLUG_AFTER = 8192


def pattern(seed, n):
    return (n * 7 + seed * 13 + (n >> 8)) & 0xFF


def say(message):
    print(f"printer-sink: {message}", file=sys.stderr, flush=True)


def gadget_udc():
    state = os.environ.get("LIBER_GADGET_STATE") or os.path.join(os.environ.get("TMPDIR", "/tmp"), "liber-usb-gadget")
    try:
        with open(os.path.join(state, "name")) as handle:
            name = handle.read().strip()
    except OSError:
        return None
    if not name.startswith("liber"):
        return None
    return os.path.join("/sys/kernel/config/usb_gadget", name, "UDC")


def open_device(path, seconds):
    deadline = time.monotonic() + seconds
    while True:
        try:
            return os.open(path, os.O_RDWR | os.O_NONBLOCK)
        except OSError as error:
            if error.errno not in (errno.ENOENT, errno.ENODEV, errno.EBUSY) or time.monotonic() > deadline:
                raise
            time.sleep(0.2)


def set_status(fd, bits):
    fcntl.ioctl(fd, GADGET_SET_PRINTER_STATUS, bits)


def read_some(fd, count, seconds):
    """Up to `count` bytes, waiting out the time before the guest configures the device.

    NON-BLOCKING, AND POLLED, BECAUSE OF WHERE THE FUNCTION QUEUES ITS RECEIVES: `f_printer` hands its OUT
    requests to the controller only from inside `read()`, and one made before the host configured the device
    finds the endpoint disabled, gives the requests back and sleeps - and configuring the device later queues
    nothing and wakes nobody. A blocking reader started before the guest booted therefore waits for ever while
    the guest's first write waits on it. Each poll is a `read()`, so the first one after the configuration is
    the one that queues the requests.
    """
    deadline = time.monotonic() + seconds
    while True:
        try:
            chunk = os.read(fd, count)
        except OSError as error:
            if error.errno not in (errno.ENODEV, errno.EAGAIN, errno.EINTR) or time.monotonic() > deadline:
                raise
            time.sleep(0.02)
            continue
        if chunk:
            return chunk
        if time.monotonic() > deadline:
            raise TimeoutError("nothing arrived")
        time.sleep(0.05)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--device", default="/dev/g_printer0")
    parser.add_argument("--patience", type=float, default=900.0, help="seconds to wait for the guest")
    args = parser.parse_args()

    fd = open_device(args.device, args.patience)
    set_status(fd, SELECTED | NOT_ERROR)
    say("idle: selected, no error")

    received = bytearray()
    paused = False
    wrong_at = None
    while len(received) < FIRST_LENGTH:
        if not paused and len(received) >= PAUSE_AFTER:
            say(f"pausing {PAUSE_SECONDS}s after {len(received)} bytes, so the guest's writes wait on this host")
            time.sleep(PAUSE_SECONDS)
            paused = True
        chunk = read_some(fd, min(8192, FIRST_LENGTH - len(received)), args.patience)
        for byte in chunk:
            if wrong_at is None and byte != pattern(FIRST_SEED, len(received)):
                wrong_at = len(received)
            received.append(byte)
    if wrong_at is None:
        set_status(fd, SELECTED | NOT_ERROR | PAPER_EMPTY)
        say(f"the first document arrived whole: {len(received)} bytes, every one of them the oracle's")
    else:
        set_status(fd, SELECTED | PAPER_EMPTY)
        say(f"the first document DIFFERED at byte {wrong_at} - the error bit is raised")

    second = bytearray()
    while len(second) < UNPLUG_AFTER:
        chunk = read_some(fd, UNPLUG_AFTER - len(second), args.patience)
        second.extend(chunk)
    matches = all(byte == pattern(SECOND_SEED, at) for at, byte in enumerate(second))
    say(f"{len(second)} bytes of the second document ({'as sent' if matches else 'NOT as sent'}) - pulling the cable")
    udc = gadget_udc()
    if udc is None or not os.path.exists(udc):
        say("no gadget of this harness's to unbind - the unplug is not exercised")
        return 1
    os.close(fd)
    with open(udc, "w") as handle:
        handle.write("\n")
    say("unbound: the printer has left the guest's bus")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, TimeoutError) as error:
        say(f"stopped: {error}")
        sys.exit(1)
