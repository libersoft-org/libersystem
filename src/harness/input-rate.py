#!/usr/bin/env python3
"""The host half of `hidcheck rate keys|pointer`: the input figures' events, sent through QEMU's own input path.

THE GUEST TIMES, THIS ONLY SENDS. The probe writes a cue straight into the kernel's console ring - `hidcheck-cue N` for
each of its single events, `hidcheck-cue stream` before the stream - and this answers each one as it appears on the
development instance's serial log, with QMP `input-send-event`: a key transition, or a tablet move. Every figure is the
guest's clock; what this prints is only what it sent and how long the host took to send it, so a stream the host could
not send faster is told apart from one the guest could not take.

A KEY is the one key `--key` names, pressed on odd cues and released on even ones; the stream alternates the same way,
`--batch` transitions to one QMP call. A TABLET MOVE lands at the centre of a cell of InputService's 80x50 grid: the cued
ones along its last row, and the stream's move n at column n mod 80 of row n / 80 - which is how the probe numbers them,
since its only view of the pointer is a snapshot of the last thirty-two events. One move to a call, so each is one
event group the device can fold or deliver; `--count` stays under 3920 so the stream never reaches the last row.

QEMU feeds the device of each kind that became active last: `hidcheck route virtio|usb` chooses the keyboard, and the
monitor's `mouse_set` the tablet. Run beside the probe, against the persistent instance:

  src/harness/input-rate.py --kind keys --count 2000 & ./lab.sh sh --timeout 120 hidcheck rate keys
"""

import argparse
import json
import os
import socket
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
BOOT = os.path.join(HERE, "..", "..", ".build", "boot")
# QEMU's absolute axis span, whatever the guest's resolution is.
ABS_SPAN = 32768
GRID_COLUMNS, GRID_ROWS = 80, 50


class Qmp:
	def __init__(self, path):
		self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
		self.sock.connect(path)
		self.buf = b""
		self.ident = 0
		self.read()
		self.call("qmp_capabilities")

	def read(self):
		while b"\n" not in self.buf:
			more = self.sock.recv(65536)
			if not more:
				sys.exit("input-rate: the QMP socket closed")
			self.buf += more
		line, _, self.buf = self.buf.partition(b"\n")
		return json.loads(line)

	# THE ANSWER WITH THIS REQUEST'S ID, past any event QEMU sends in between.
	def call(self, execute, arguments=None):
		self.ident += 1
		request = {"execute": execute, "id": self.ident}
		if arguments is not None:
			request["arguments"] = arguments
		self.sock.sendall(json.dumps(request).encode() + b"\n")
		while True:
			answer = self.read()
			if answer.get("id") == self.ident:
				if "error" in answer:
					sys.exit(f"input-rate: QMP refused {execute}: {answer['error']}")
				return answer


class Log:
	"""The serial log from where it ends now: a cue printed before this started is not this run's."""

	def __init__(self, path):
		self.fd = os.open(path, os.O_RDONLY)
		os.lseek(self.fd, 0, os.SEEK_END)
		self.pending = b""

	# THE CUE, AND THEN SOMETHING THAT IS NOT A DIGIT: `cue 1` is not `cue 12`, and a CR or a LF may follow.
	def wait_for(self, needle, timeout):
		deadline = time.monotonic() + timeout
		while True:
			at = self.pending.find(needle)
			while at >= 0 and at + len(needle) < len(self.pending) and self.pending[at + len(needle)] in b"0123456789":
				at = self.pending.find(needle, at + 1)
			if at >= 0 and at + len(needle) < len(self.pending):
				self.pending = self.pending[at + len(needle):]
				return True
			data = os.read(self.fd, 65536)
			if data:
				self.pending += data
				continue
			if time.monotonic() > deadline:
				return False
			time.sleep(0.0002)


def key(down, name):
	return [{"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": name}}}]


# The centre of one cell of the grid, in QEMU's absolute range.
def cell(column, row):
	return [{"type": "abs", "data": {"axis": "x", "value": (2 * column + 1) * ABS_SPAN // (2 * GRID_COLUMNS)}}, {"type": "abs", "data": {"axis": "y", "value": (2 * row + 1) * ABS_SPAN // (2 * GRID_ROWS)}}]


def main():
	parser = argparse.ArgumentParser(description="the host half of hidcheck rate")
	parser.add_argument("--kind", choices=("keys", "pointer"), required=True)
	parser.add_argument("--serial", default=os.path.join(BOOT, "dev-serial.log"))
	parser.add_argument("--qmp", default=os.path.join(BOOT, "qemu-qmp.sock"))
	parser.add_argument("--rounds", type=int, default=32, help="the probe's cued events (its ROUNDS)")
	parser.add_argument("--count", type=int, default=2000, help="events in the stream")
	parser.add_argument("--rate", type=float, default=0.0, help="events a second; 0 sends as fast as QMP answers")
	parser.add_argument("--batch", type=int, default=1, help="key transitions to one input-send-event call")
	parser.add_argument("--key", default="shift")
	parser.add_argument("--timeout", type=float, default=120.0)
	args = parser.parse_args()
	if args.kind == "pointer" and (args.batch != 1 or args.count >= GRID_COLUMNS * (GRID_ROWS - 1)):
		sys.exit("input-rate: a pointer stream is one move to a call, and fewer moves than the grid has cells above its last row")
	log = Log(args.serial)
	qmp = Qmp(args.qmp)
	for cue in range(1, args.rounds + 1):
		if not log.wait_for(f"hidcheck-cue {cue}".encode(), args.timeout):
			sys.exit(f"input-rate: no cue {cue} within {args.timeout} s")
		qmp.call("input-send-event", {"events": key(cue % 2 == 1, args.key) if args.kind == "keys" else cell(cue * 7 % GRID_COLUMNS, GRID_ROWS - 1)})
	if not log.wait_for(b"hidcheck-cue stream", args.timeout):
		sys.exit("input-rate: no stream cue")
	period = 1.0 / args.rate if args.rate > 0 else 0.0
	started = time.monotonic()
	sent, calls = 0, 0
	while sent < args.count:
		while period and time.monotonic() < started + sent * period:
			pass
		if args.kind == "keys":
			events = [event for n in range(sent, min(args.count, sent + args.batch)) for event in key(n % 2 == 0, args.key)]
		else:
			move = sent + 1
			events = cell(move % GRID_COLUMNS, move // GRID_COLUMNS)
		qmp.call("input-send-event", {"events": events})
		calls += 1
		sent += args.batch if args.kind == "keys" else 1
	took = time.monotonic() - started
	print(f"input-rate: {args.kind}: {args.rounds} cued event(s) answered; a stream of {args.count} sent in {calls} call(s) in {took * 1000:.1f} ms ({args.count / took:.0f} a second)")


if __name__ == "__main__":
	main()
