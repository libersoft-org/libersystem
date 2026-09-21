#!/usr/bin/env python3
# THE HOST END OF THE CDC-ACM ORACLE.
#
# A driver that binds a serial adapter and reports a state is not a driver that moves bytes, and this
# item's own text refuses "the module reached its class binding" as evidence. The other end of a USB
# serial adapter is whatever is plugged into it - here, the gadget side of the device the harness
# built, which the host sees as an ordinary tty. This process opens it and echoes.
#
# WHY IT IS NOT `cat /dev/ttyGS0 > /dev/ttyGS0`. A tty is line-disciplined by default: it would echo
# on its own, translate newlines, buffer until a line ended and interpret control characters - so a
# guest writing three bytes with no newline would see nothing, and a guest writing 0x03 would signal
# rather than be echoed. What the contract above this carries is BYTES, so the discipline is turned
# off and the port is read and written raw.
#
# It is deliberately tiny and deliberately not a service: it holds no state, it answers one port, and
# it says on stderr what it saw so a failing run has something to read.

import argparse
import os
import sys
import termios
import time
import tty


def raw(fd):
    # EVERY TRANSFORMATION OFF, which is what "bytes" means. `cfmakeraw` clears the input mapping,
    # the output post-processing, the echo and the canonical mode in one go; the two control
    # characters say "return whatever has arrived, and do not wait for more".
    attrs = termios.tcgetattr(fd)
    tty.setraw(fd)
    attrs = termios.tcgetattr(fd)
    attrs[6][termios.VMIN] = 0
    attrs[6][termios.VTIME] = 1
    termios.tcsetattr(fd, termios.TCSANOW, attrs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tty", default="/dev/ttyGS0", help="the gadget side of the adapter")
    parser.add_argument("--wait", type=float, default=10.0, help="how long to wait for it to appear")
    args = parser.parse_args()

    # THE DEVICE APPEARS WHEN THE GADGET BINDS, and the harness starts this beside the setup rather
    # than after it, so a short wait here is the ordinary case and not a symptom.
    deadline = time.monotonic() + args.wait
    while not os.path.exists(args.tty):
        if time.monotonic() >= deadline:
            print(f"serial-echo: {args.tty} did not appear", file=sys.stderr)
            return 1
        time.sleep(0.05)

    # REOPENED UNTIL IT HAS SEEN A BYTE, WHICH IS ABOUT WHO THE HOST OF THE CABLE IS.
    #
    # `dummy_hcd` gives this machine BOTH ends: the gadget side is this tty, and the host side is an
    # ordinary USB device the machine's own `cdc_acm` binds. The harness starts this process beside
    # the gadget, which is BEFORE QEMU starts - so the port opened here is opened while LINUX is
    # still the host, and a few seconds later QEMU claims the device, the kernel driver is detached
    # and the function is torn down and brought back up under a new host. A descriptor held across
    # that sees nothing afterwards.
    #
    # So the port is reopened while nothing has arrived. Once a byte has, the guest is the host and
    # the descriptor is the right one; from then on it is kept, because reopening a working port
    # would lose whatever arrived between the close and the open.
    fd = os.open(args.tty, os.O_RDWR | os.O_NOCTTY)
    raw(fd)
    print(f"serial-echo: echoing on {args.tty}", file=sys.stderr)
    seen = 0
    quiet = 0.0
    try:
        while True:
            try:
                chunk = os.read(fd, 4096)
            except OSError:
                # The guest closed the port or the gadget went away. Neither is this process's
                # failure and neither is worth a traceback.
                break
            if not chunk:
                if seen == 0:
                    quiet += 0.1
                    if quiet >= 1.0:
                        quiet = 0.0
                        os.close(fd)
                        fd = os.open(args.tty, os.O_RDWR | os.O_NOCTTY)
                        raw(fd)
                continue
            seen += len(chunk)
            # SAID AS IT HAPPENS AND NOT ONLY AT THE END. The exit line below is printed from a
            # `finally`, which a harness that kills this process never reaches - so a run whose
            # guest saw nothing could not tell "the host never received it" from "the host received
            # it and the echo did not come back", which are different defects in different halves.
            print(f"serial-echo: echoed {len(chunk)} byte(s)", file=sys.stderr, flush=True)
            # WRITTEN WHOLE OR NOT AT ALL IS NOT AVAILABLE ON A TTY, so the loop is the write: a
            # short write here would silently drop the tail of whatever the guest sent, which is
            # exactly the failure an echo oracle cannot have.
            at = 0
            while at < len(chunk):
                at += os.write(fd, chunk[at:])
    except KeyboardInterrupt:
        pass
    finally:
        os.close(fd)
        print(f"serial-echo: {seen} byte(s) echoed", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
