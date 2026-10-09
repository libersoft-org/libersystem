#!/usr/bin/env python3
"""Check monitor metadata at its DisplayService destination, then acknowledged presentation."""

import argparse
import pathlib
import re

PRESENT = b"DisplayService: a frame reached the display through the provider it adopted"
ABSENT = b"DisplayService: monitor metadata unavailable"
DESCRIPTION = re.compile(
    rb"DisplayService: monitor identity=([0-9a-f]+):([0-9a-f]+):(\d+) "
    rb"physical-mm=(\d+)x(\d+) modes=(\d+) preferred=(\d+)x(\d+)@(\d+)"
)


def verify(log, enabled):
    if enabled:
        found = DESCRIPTION.search(log)
        if not found:
            raise ValueError("DisplayService never adopted a monitor description")
        manufacturer, product = (int(value, 16) for value in found.groups()[:2])
        serial, width_mm, height_mm, modes, width, height, refresh = (
            int(value) for value in found.groups()[2:]
        )
        # Independent QEMU defaults (hw/display/edid-generate.c), for the gate's 1600x900:
        # RHT, product 0x1234, serial zero, 100 dpi physical size, preferred 75 Hz.
        if (manufacturer, product, serial) != (0x4914, 0x1234, 0):
            raise ValueError("the bound QEMU monitor's identity did not reach DisplayService")
        if (width_mm, height_mm, width, height) != (406, 228, 1600, 900):
            raise ValueError("physical size or preferred resolution is not QEMU's configured EDID")
        if not 1 <= modes <= 29 or not 74_000 <= refresh <= 76_000:
            raise ValueError("advertised base timings or preferred refresh are missing/invalid")
        after = found.end()
    else:
        if DESCRIPTION.search(log):
            raise ValueError("metadata was invented when the EDID feature was disabled")
        if ABSENT not in log:
            raise ValueError("DisplayService did not report unavailable metadata")
        after = log.index(ABSENT) + len(ABSENT)
    if PRESENT not in log[after:]:
        raise ValueError("no acknowledged provider-backed presentation followed discovery")


def self_test():
    description = (
        b"DisplayService: monitor identity=4914:1234:0 physical-mm=406x228 "
        b"modes=18 preferred=1600x900@74999\n"
    )
    valid = description + PRESENT
    verify(valid, True)
    verify(ABSENT + b"\n" + PRESENT, False)
    rejected = [
        (PRESENT, True),
        (description, True),
        (PRESENT + description, True),
        (valid.replace(b"4914", b"4a14"), True),
        (valid.replace(b"406x228", b"0x0"), True),
        (valid.replace(b"modes=18", b"modes=0"), True),
        (valid.replace(b"modes=18", b"modes=30"), True),
        (valid.replace(b"1600x900", b"1280x800"), True),
        (valid.replace(b"74999", b"0"), True),
        (valid + ABSENT + PRESENT, False),
        (ABSENT, False),
        (PRESENT, False),
    ]
    for log, enabled in rejected:
        try:
            verify(log, enabled)
        except ValueError:
            continue
        raise AssertionError("oracle accepted incomplete or wrong monitor evidence")
    print("edid-oracle: PASS enabled/disabled evidence and 12 incomplete/wrong variants")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--log", type=pathlib.Path)
    parser.add_argument("--edid", choices=("on", "off"))
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if args.log is None or args.edid is None:
        parser.error("--log and --edid are required")
    try:
        verify(args.log.read_bytes(), args.edid == "on")
    except ValueError as error:
        raise SystemExit(f"edid-oracle: FAIL {error}") from error
    print(f"edid-oracle: PASS EDID {args.edid}, DisplayService discovery then presentation")


if __name__ == "__main__":
    main()
