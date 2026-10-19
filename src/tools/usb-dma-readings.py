#!/usr/bin/env python3
"""THE DMA MEMORY A USB CONTROLLER'S DRIVER HOLDS, read from `lsdev` in a guest log, compared before and after.

usage: usb-dma-readings.py LOG MARK BASELINE READING:CONTROLLER...

The guest prints `MARK NAME` before each `lsdev` it runs. BASELINE names the reading taken before a device arrived;
each READING:CONTROLLER names a reading taken after that device left, and the controller - a driver artifact, `uhci`,
`ohci`, `ehci` or `xhci` - whose DMA memory must be what it was at the baseline. A UHCI on slot 1d is an EHCI's
companion, and is not the UHCI this reads.

Each binding line `lsdev` prints is the record's text form, `{index=.., bus=.., dev=.., ..., artifact=.., ...,
dma-used=..}`, its fields separated by commas. A controller with no reading, one that reads as holding nothing, or one
that holds more or less after the device left than before it came fails the check.
"""
import re
import sys


def readings(text, mark):
    found = {}
    pattern = re.escape(mark) + r' (\S+)\n(.*?)(?=' + re.escape(mark) + r' |\Z)'
    for name, body in re.findall(pattern, text, re.S):
        held = {}
        for line in body.splitlines():
            row = re.search(r'\bdev=(\d+),.*?\bartifact=([a-z0-9_]+),.*?\bdma-used=(\d+)\}', line)
            if not row:
                continue
            dev, artifact, dma = int(row.group(1)), row.group(2), int(row.group(3))
            if artifact == 'uhci' and dev == 0x1d:
                continue
            held.setdefault(artifact, dma)
        found[name] = held
    return found


def main():
    if len(sys.argv) < 5:
        sys.exit(__doc__)
    log, mark, baseline = sys.argv[1:4]
    text = re.sub(r'\x1b\[[0-9;]*m', '', open(log, errors='replace').read()).replace('\r', '')
    held = readings(text, mark)
    for pair in sys.argv[4:]:
        reading, controller = pair.split(':', 1)
        before = held.get(baseline, {}).get(controller)
        after = held.get(reading, {}).get(controller)
        if before is None or after is None:
            sys.exit(f'usb-dma: no {controller} DMA reading (before: {before}, after the device left: {after})')
        if before == 0:
            sys.exit(f'usb-dma: the {controller} driver reads as holding no DMA memory, which proves nothing')
        if after != before:
            sys.exit(f'usb-dma: the {controller} driver held {before} bytes of DMA memory before the device came and {after} after it left')
        print(f'usb-dma: the {controller} driver holds {after} bytes of DMA memory after the device left, as before it came')


if __name__ == '__main__':
    main()
