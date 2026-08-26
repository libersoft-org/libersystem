#!/usr/bin/env python3
# perf-trace.py - latency tracer for the LiberSystem console path.
#
# The kernel and the ring-3 services emit perf markers to the debug serial as lines
# of the form "\x1ePERF <label> <tsc>" (see rt::perf_mark and apic/tsc). Every marker
# carries a raw time-stamp-counter value; the TSC is a single global cycle clock
# shared by the kernel and every process, so markers from the shell, ConsoleService
# and the gpu driver all sit on one timeline. The kernel publishes its calibrated TSC
# frequency once at boot as "\x1ePERF tsc_hz <hz>", which converts cycles to time.
#
# DEV_PROFILE=1 IS REQUIRED, and it is what makes the guest emit that anchor. It used to be printed
# on every boot, which meant every ordinary boot showed one line addressed to this program - so the
# kernel now emits it only when a profile is named over fw_cfg, which is the condition "a harness is
# watching".
#
# This tool connects to the QEMU serial unix socket, captures the boot tsc_hz anchor,
# sends a command (default "help"), collects the markers it triggers, and prints a
# timeline in milliseconds plus a per-phase breakdown (shell produce -> console render
# -> gpu present). It pinpoints where a slow console command actually spends its time.
#
# Usage:
#   harness/perf-trace.py [--sock /tmp/ls-ser.sock] [--cmd help] [--window 3.0]
#
# Typical session (start a guest of your own; do NOT pattern-kill QEMU - this used to say
# `pkill -9 qemu-system-x86`, which takes down every QEMU the user owns, including ones this has
# nothing to do with):
#   DEV_PROFILE=1 SERIAL="unix:/tmp/ls-ser.sock,server,nowait" DISPLAYS=vnc VNC_ADDR=127.0.0.1:9 \
#     harness/qemu-run.sh x86_64 ../.build/cargo/kernel/x86_64-unknown-none/debug/kernel >/tmp/qemu.log 2>&1 &
#   harness/perf-trace.py            # connects, waits for boot, runs `help`, prints the trace
#
# WHAT IT REFUSES TO MEASURE. A trace is only a measurement of the command if the guest was up
# before the command was sent and the command's own markers bound the window. This used to print a
# plausible-looking latency report when neither held: the boot loop set a flag it never checked, so
# a guest still booting produced a "trace" made of boot markers; the only acceptance condition was
# that at least one marker of any kind had been captured; and a missing phase endpoint printed
# `n/a` and carried on. Each of those is a report about work nobody asked for, and performance work
# was then aimed by it. Every one of them is now an error that says which condition failed.

import argparse
import os
import socket
import sys
import time

RS = 0x1e  # ASCII record separator that prefixes every perf line


def connect(sock_path: str, timeout: float) -> socket.socket:
	"""Wait for the serial unix socket to appear and connect to it."""
	deadline = time.time() + timeout
	while time.time() < deadline:
		if os.path.exists(sock_path):
			try:
				s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
				s.connect(sock_path)
				s.setblocking(False)
				return s
			except (FileNotFoundError, ConnectionRefusedError):
				pass
		time.sleep(0.05)
	sys.exit(f"perf-trace: serial socket {sock_path} did not appear within {timeout}s")


def drain_lines(buf: bytearray):
	"""Split a byte buffer into complete lines, keeping the trailing partial line."""
	lines = []
	while True:
		nl = buf.find(b"\n")
		if nl < 0:
			break
		lines.append(bytes(buf[:nl]))
		del buf[: nl + 1]
	return lines


def parse_marker(line: bytes):
	"""Return (label, tsc, val) for a perf line, or None for ordinary console output.

	A marker is "\x1ePERF <label> <tsc>" with an optional trailing decimal <val>
	(emitted by rt::perf_mark_val, e.g. a queue-submit spin count). val is None when absent.
	"""
	i = line.find(RS)
	if i < 0:
		return None
	body = line[i + 1 :].split()
	if len(body) not in (3, 4) or body[0] != b"PERF":
		return None
	try:
		label = body[1].decode("ascii", "replace")
		tsc = int(body[2])
		val = int(body[3]) if len(body) == 4 else None
		return label, tsc, val
	except ValueError:
		return None


def main() -> None:
	ap = argparse.ArgumentParser(description="LiberSystem console latency tracer")
	ap.add_argument("--sock", default="/tmp/ls-ser.sock", help="QEMU serial unix socket")
	ap.add_argument("--cmd", default="help", help="command to send and measure")
	ap.add_argument("--window", type=float, default=3.0, help="seconds to collect markers after the command")
	ap.add_argument("--boot-timeout", type=float, default=30.0, help="seconds to wait for boot")
	ap.add_argument("--require", action="append", default=None, help="marker label the trace must contain (repeatable); default: the command's own start/end pair")
	args = ap.parse_args()
	if args.window <= 0:
		sys.exit("perf-trace: --window must be positive; a window of zero collects nothing and would report it as a trace")
	if args.boot_timeout <= 0:
		sys.exit("perf-trace: --boot-timeout must be positive")
	# The command's own boundary markers, unless the caller names others. `help` emits
	# `shell_help_start` / `shell_help_end`, and a trace that has neither is a trace of something
	# else that happened to be running.
	required = args.require if args.require is not None else [f"shell_{args.cmd.split()[0]}_start", f"shell_{args.cmd.split()[0]}_end"]

	s = connect(args.sock, args.boot_timeout)
	buf = bytearray()
	tsc_hz = 0

	# Phase 1: read the boot stream until the shell is ready, capturing the tsc_hz anchor.
	#
	# Monotonic, because a wall clock that steps during a boot moves a deadline this depends on.
	deadline = time.monotonic() + args.boot_timeout
	booted = False
	while time.monotonic() < deadline and not booted:
		try:
			chunk = s.recv(65536)
			if not chunk:
				sys.exit("perf-trace: the serial connection closed before the guest was ready - the guest is gone, not slow")
			buf += chunk
		except BlockingIOError:
			time.sleep(0.02)
			continue
		for line in drain_lines(buf):
			m = parse_marker(line)
			if m and m[0] == "tsc_hz":
				tsc_hz = m[1]
			if b"boot OK" in line or b"entering the userspace shell" in line:
				booted = True
	# AND THIS IS CHECKED. The flag was set and never read: execution fell through to sending the
	# command into a guest that had not reached a shell, and whatever markers the boot was still
	# emitting became the command's trace.
	if not booted:
		sys.exit(f"perf-trace: no ready marker within {args.boot_timeout} s - nothing was measured")
	# Settle, then flush anything still queued so only post-command markers are collected.
	time.sleep(0.3)
	try:
		while True:
			s.recv(65536)
	except BlockingIOError:
		pass
	buf.clear()

	if tsc_hz:
		print(f"perf-trace: tsc_hz = {tsc_hz} ({tsc_hz / 1e9:.3f} GHz)")
	else:
		print("perf-trace: WARNING tsc_hz anchor not seen; will self-calibrate from host wall-clock")

	# Phase 2: send the command and collect the markers it triggers.
	markers = []  # (label, tsc, host_recv_time, val)
	s.sendall(args.cmd.encode() + b"\n")
	end = time.monotonic() + args.window
	closed = False
	while time.monotonic() < end:
		try:
			chunk = s.recv(65536)
			if not chunk:
				closed = True
				break
			buf += chunk
		except BlockingIOError:
			time.sleep(0.01)
			continue
		now = time.monotonic()
		for line in drain_lines(buf):
			m = parse_marker(line)
			if m:
				markers.append((m[0], m[1], now, m[2]))
	s.close()
	if closed:
		sys.exit("perf-trace: the serial connection closed while the command was running")

	if not markers:
		sys.exit("perf-trace: no markers captured (is PERF enabled? did the command run?)")
	# THE COMMAND'S OWN MARKERS, not any markers. One captured marker of any label used to be the
	# whole acceptance condition, so background activity in an idle guest produced a trace of a
	# command that had never been accepted.
	seen = {label for label, _, _, _ in markers}
	missing = [label for label in required if label not in seen]
	if missing:
		sys.exit(f"perf-trace: the command produced no {', '.join(missing)} - this is not a trace of {args.cmd!r} (captured: {', '.join(sorted(seen))})")

	# Order by TSC (the true timeline; serial delivery order can differ).
	markers.sort(key=lambda x: x[1])
	tsc0 = markers[0][1]

	# Convert cycles to ms. Prefer the kernel's calibrated tsc_hz; otherwise self-calibrate
	# from the host wall-clock span across the captured markers (good to a few percent).
	if tsc_hz:
		cyc_to_ms = 1e3 / tsc_hz
	else:
		span_cyc = markers[-1][1] - markers[0][1]
		span_s = markers[-1][2] - markers[0][2]
		# A calibration that cannot be performed is its own failure, not a scale of zero. With
		# `cyc_to_ms = 0` every duration below printed as 0.000 ms, which reads as an
		# extraordinarily fast system rather than as no measurement at all.
		if span_cyc <= 0 or span_s <= 0:
			sys.exit("perf-trace: no tsc_hz anchor and the captured markers span no time - nothing can be converted to milliseconds")
		cyc_to_ms = span_s * 1e3 / span_cyc

	def ms(tsc: int) -> float:
		return (tsc - tsc0) * cyc_to_ms

	# Timeline: each marker's absolute offset and the delta from the previous one.
	print(f"\nperf-trace: command {args.cmd!r}, {len(markers)} markers\n")
	print(f"{'t (ms)':>9}  {'+dt (ms)':>9}  label")
	print("  " + "-" * 40)
	prev = tsc0
	for label, tsc, _, val in markers:
		val_str = f"  = {val}" if val is not None else ""
		print(f"{ms(tsc):9.3f}  {(tsc - prev) * cyc_to_ms:9.3f}  {label}{val_str}")
		prev = tsc

	# Phase breakdown: named spans between the first occurrence of each boundary marker.
	first = {}
	last = {}
	for label, tsc, _, _ in markers:
		first.setdefault(label, tsc)
		last[label] = tsc

	def span(a: str, b: str):
		if a in first and b in last and last[b] >= first[a]:
			return (last[b] - first[a]) * cyc_to_ms
		return None

	print("\nperf-trace: phase breakdown")
	phases = [
		("shell produces output (help_start -> help_end)", "shell_help_start", "shell_help_end"),
		("shell_end -> console wakes", "shell_help_end", "con_vt_wake"),
		("console renders grid (wake -> drain)", "con_vt_wake", "con_vt_drain"),
		("console -> FLUSH sent (drain -> present_done)", "con_vt_drain", "con_present_done"),
		("gpu present (transfer + flush)", "gpu_present_start", "gpu_present_end"),
		("TOTAL help_start -> last gpu present_end", "shell_help_start", "gpu_present_end"),
	]
	unmeasured = []
	for name, a, b in phases:
		v = span(a, b)
		print(f"  {name:<48} {v:8.3f} ms" if v is not None else f"  {name:<48} {'n/a':>8}")
		if v is None:
			unmeasured.append(name)

	# How many gpu presents the command caused (1 = ideal coalescing).
	n_present = sum(1 for label, _, _, _ in markers if label == "gpu_present_end")
	print(f"\nperf-trace: gpu presents for this command = {n_present} (1 = fully coalesced)")

	# AND AN INCOMPLETE TRACE IS A FAILURE. A phase whose endpoints were never seen printed `n/a`
	# and the tool exited 0, so a broken input path or missing instrumentation was reported as a
	# successful measurement with some gaps in it.
	if unmeasured:
		sys.exit("perf-trace: these phases were never observed, so this trace does not measure the console path: " + "; ".join(unmeasured))
	if n_present == 0:
		sys.exit("perf-trace: the command caused no gpu present - nothing reached the display, so there is no console latency here")


if __name__ == "__main__":
	main()
