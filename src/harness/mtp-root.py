#!/usr/bin/env python3
"""The files QEMU's `usb-mtp` presents to the PTP oracle, written into a directory this run made.

The oracle reads them back through the guest's still image class module and compares every byte, so the
contents come from the formula it knows - and the sizes are chosen for what they exercise: one object far
larger than a page, so a GetObject spans many receives and a cancel can land in the middle of it, and one
whose data container ends exactly on a packet boundary, which is where a device sends a zero-length packet.
"""

import os
import sys

# THE ORACLE'S OBJECTS - `usb_ptp_*` in the kernel's hardware suite expects exactly these.
OBJECTS = (("photo-a.bin", 3, 150001), ("exact.bin", 4, 8180))


def pattern(seed, n):
    return (n * 7 + seed * 13 + (n >> 8)) & 0xFF


def main():
    root = sys.argv[1]
    if not os.path.isdir(root) or os.listdir(root):
        print(f"mtp-root: {root} is not an empty directory this run made - nothing is written", file=sys.stderr)
        return 1
    for name, seed, length in OBJECTS:
        with open(os.path.join(root, name), "wb") as handle:
            handle.write(bytes(pattern(seed, n) for n in range(length)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
