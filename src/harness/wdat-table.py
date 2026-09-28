#!/usr/bin/env python3
# THE WATCHDOG GATE'S WDAT: an ACPI Watchdog Action Table over q35's ICH9 TCO, for QEMU's `-acpitable file=`.
#
# It describes the timer the TCO driver drives by hand - the reload register as the pet, the halt bit as the
# running state, the 10-bit count, the second-timeout status, and GCS's No-Reboot as the reboot setting - at
# 1200 ms per count, because both of the TCO's expiries run the same count and the reset comes at the second. The
# addresses are the ones OVMF assigns on q35: the ACPI PM block at 0x600 (the TCO block at +0x60) and the root
# complex at 0xFED1C000 (GCS at +0x3410); both can be given, and the gate checks the kernel read the same ones.
#
#   wdat-table.py OUTPUT [--pm-base 0x600] [--rcba 0xfed1c000]
import argparse
import struct

RESET = 0x01
QUERY_CURRENT_COUNTDOWN = 0x04
QUERY_COUNTDOWN = 0x05
SET_COUNTDOWN = 0x06
QUERY_RUNNING = 0x08
SET_RUNNING = 0x09
QUERY_STOPPED = 0x0A
SET_STOPPED = 0x0B
QUERY_REBOOT = 0x10
SET_REBOOT = 0x11
QUERY_STATUS = 0x20
SET_STATUS = 0x21

READ_VALUE = 0x00
READ_COUNTDOWN = 0x01
WRITE_VALUE = 0x02
WRITE_COUNTDOWN = 0x03
PRESERVE = 0x80

MEMORY = 0
IO = 1

ENABLED = 0x01
STOPPED_IN_SLEEP = 0x80


def entry(action, instruction, space, width, address, value, mask):
	# GAS: space, bit width, bit offset, access size (1 byte, 2 word, 3 dword), address.
	access = {8: 1, 16: 2, 32: 3}[width]
	return struct.pack('<BBHBBBBQII', action, instruction, 0, space, width, 0, access, address, value, mask)


def table(pm_base, rcba):
	tco = pm_base + 0x60
	rld, sts2, cnt1, tmr = tco + 0x00, tco + 0x06, tco + 0x08, tco + 0x12
	gcs = rcba + 0x3410
	entries = [
		# The pet: any write to the reload register.
		entry(RESET, WRITE_VALUE, IO, 16, rld, 1, 0x3FF),
		# The count: bits 9:0 of TCO_TMR, the rest kept.
		entry(QUERY_COUNTDOWN, READ_COUNTDOWN, IO, 16, tmr, 0, 0x3FF),
		entry(SET_COUNTDOWN, WRITE_COUNTDOWN | PRESERVE, IO, 16, tmr, 0, 0x3FF),
		# Running is the halt bit (TCO1_CNT bit 11) CLEAR: read as a value compared with 0.
		entry(QUERY_RUNNING, READ_VALUE, IO, 16, cnt1, 0, 0x800),
		entry(SET_RUNNING, WRITE_VALUE | PRESERVE, IO, 16, cnt1, 0, 0x800),
		entry(QUERY_STOPPED, READ_VALUE, IO, 16, cnt1, 0x800, 0x800),
		entry(SET_STOPPED, WRITE_VALUE | PRESERVE, IO, 16, cnt1, 0x800, 0x800),
		# The reboot setting is No-Reboot (GCS bit 5) CLEAR - SET_REBOOT is the action that clears it.
		entry(QUERY_REBOOT, READ_VALUE, MEMORY, 32, gcs, 0, 0x20),
		entry(SET_REBOOT, WRITE_VALUE | PRESERVE, MEMORY, 32, gcs, 0, 0x20),
		# The last reset: TCO2_STS's second-timeout bit, cleared by writing it back.
		entry(QUERY_STATUS, READ_VALUE, IO, 16, sts2, 2, 2),
		entry(SET_STATUS, WRITE_VALUE, IO, 16, sts2, 2, 2),
	]
	body = struct.pack('<IHBBBBBB', 32, 0xFFFF, 0xFF, 0xFF, 0xFF, 0, 0, 0)
	body += struct.pack('<IIIB3xI', 1200, 1023, 2, ENABLED | STOPPED_IN_SLEEP, len(entries))
	body += b''.join(entries)
	length = 36 + len(body)
	header = b'WDAT' + struct.pack('<IBB', length, 1, 0) + b'LIBER ' + b'LIBERWDT' + struct.pack('<I', 1) + b'LIBR' + struct.pack('<I', 1)
	raw = bytearray(header + body)
	raw[9] = (-sum(raw)) & 0xFF
	return bytes(raw)


def main():
	parser = argparse.ArgumentParser(description='Write a WDAT over q35\'s ICH9 TCO for -acpitable file=.')
	parser.add_argument('output')
	parser.add_argument('--pm-base', type=lambda text: int(text, 0), default=0x600)
	parser.add_argument('--rcba', type=lambda text: int(text, 0), default=0xFED1C000)
	args = parser.parse_args()
	data = table(args.pm_base, args.rcba)
	assert len(data) == 68 + 24 * 11 and sum(data) & 0xFF == 0
	with open(args.output, 'wb') as out:
		out.write(data)


if __name__ == '__main__':
	main()
