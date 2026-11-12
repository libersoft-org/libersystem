#!/usr/bin/env python3
# check-tickless-idle.py - an idle machine that takes no tick still answers a key and a plugged device.
#
# WHAT NO KERNEL TEST CAN SHOW. Interrupt-driven serial receive and its enable, the shell loop, the idle
# hook, hot-plug settling and riscv64's `WIRED_EID` with its slot arming are compiled into the production
# kernel only, so a kernel test can call `console_input::feed_serial` itself and would prove nothing about
# a receive interrupt or a shared identity. This boots the development build of each target and drives it
# from outside, the way a person at its console would:
#
#   1. BYTES TYPED ON THE GUEST'S UART AT AN IDLE MACHINE ARE ECHOED BY THE SHELL WITHIN ONE SECOND of the
#      host's write, timed here from the write to the echo - the one interval the host can observe.
#   2. THE LATENCY ALONE DOES NOT SHOW THE RECEIVE INTERRUPT. The machine carries a PCIe root port, whose
#      error reporting the boot processor polls, so an idle boot processor still wakes at the housekeeping
#      bound, its console pass reads the UART, and typed bytes would arrive within 100 ms with no receive
#      interrupt at all. So the per-core idle records the system graph publishes, read before and after
#      the typing, must attribute at least one wake per typed burst to the console UART's receive line -
#      by the identity the kernel said it armed at boot. A kernel that never armed the line records none.
#   3. A DEVICE PLUGGED OVER QMP INTO THE MACHINE'S EMPTY HOT-PLUG PORT WHILE IT IS IDLE IS SEEN BY THE
#      KERNEL. In the same boot and after the typing, on every target: on riscv64 the UART and the slots
#      are answered under one identity, and a slot event before the typing would be counted as the UART's.
#   4. AND THE OPPOSITE OF IDLE, on x86_64: a command typed while a program floods the terminal is answered
#      before the flood ends (`typed_under_flood`) - the property the ports' sleep below types `sleepctl` over a
#      counter with.
#
# And it says how often each idle core woke, per second, from the same records - the number the part is
# for, which `docs/PERF.md` carries.
#
# ON aarch64 AND riscv64 THE TREE ALSO CARRIES IDLE STATES, which QEMU's own does not: the harness adds three
# (`IDLE_FIXTURE=1`, `fdt_edit.py idle-fixture`) - two retention states the firmware takes as a wait and one that loses
# the core's context. The kernel reads them itself at boot and installs every core's table; the gate reads the boot's
# lines and the per-core records, and checks the retention states entered through the firmware over the idle sample -
# PSCI's CPU_SUSPEND or the SBI's HART_SUSPEND, none refused - and the one that loses the context never.
#
# AND THE PORTS' SLEEP, which is suspend to idle (neither firmware here offers more - the kernel says what it found at
# boot): `sleepctl suspend idle` with the timed wake, the host's oracle the serial line's timing as on x86_64 - a
# counter printing every 100 ms SILENT between the kernel's `sleep: entered` and `sleep: resumed` lines, that interval
# at least the wake less a margin, the one pair of consecutive counts that spans the freeze with its monotonic clock
# excluding the sleep and its boot-time clock including it, with the synchronous entry/resume ticks checked against
# host time - and the parking
# from the last sleep's record: no core woke for a device,
# cpu0 at most once for the timer, every other core at most once, for the IPI that ends its park.
# On ARM, first request the RAM state this firmware does not offer: admission must refuse it before
# any transaction or entry, and its last record must name that missing state rather than an idle sleep.
#
# ONE GUEST AT A TIME: each target is booted, driven and torn down before the next one boots.
#
# usage: check-tickless-idle.py [--no-build] [x86_64|aarch64|riscv64 ...]   (default: all three)

import itertools
import json
import os
import re
import select
import shutil
import signal
import socket
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(SRC, 'harness'))
import lab  # noqa: E402

TARGETS = ('x86_64', 'aarch64', 'riscv64')
TRIPLES = {'x86_64': 'x86_64-unknown-none', 'aarch64': 'aarch64-unknown-none', 'riscv64': 'riscv64gc-unknown-none-elf'}

# The plan's bound on a typed byte's trip, host write to shell echo.
ECHO_BOUND = 1.0
BURSTS = 3
# How long the idle-rate sample runs.
IDLE_SAMPLE = 10.0
# The ports' suspend to idle: its timed wake, and how far the host's measurement of it may fall short.
IDLE_WAKE_S = 5
IDLE_MARGIN_MS = 300
SLEEP_OFFERED = re.compile(rb'sleep: [^\r\n]* is (offered|not offered) by this firmware')
# THE SHELL'S PROMPT, as it reads once the colours are stripped - for taking it out of a line it landed in.
PROMPT_TEXT = re.compile(r'vol://[^\r\n>]*> ')
# WITHOUT ITS PREFIX, which can be parted from the rest: the counter wrote "sleepcheck: ", its text and its newline as
# three messages, and other programs' lines - `sleepctl`'s record of the last sleep, then its prompt - landed between
# them, until it wrote each line whole (2026-11-12); the kernel's lines still can.
# AND TO ITS END OF LINE, because the kernel can land inside it. The kernel writes its lines straight to the port while
# the counter's go through the console path, so "boot-ms 55575", then "DeviceManager: resumed console-uart ...", then
# "4" on a line of its own was count 53's boot clock, 555754 - read as 55575, a clock eight minutes behind. A counter
# line is read only whole; one another writer cut is rejoined with the rest of its own text when that arrives
# (`counter_rows`).
COUNTER_LINE = re.compile(r'count (\d+) mono-ms (\d+) boot-ms (\d+)\r?$')
COUNTER_SHAPE = ('count ', None, ' mono-ms ', None, ' boot-ms ', None)
PARKED = re.compile(r'cpu(\d+): woke (\d+) time\(s\) for the timer, (\d+) for an IPI, (\d+) for a device')

ARMED = re.compile(rb'console: typed input raises interrupt (\d+)')
# WHO HOLDS THE LINE NOW: a claim that took the UART and armed its source to an identity of its own (riscv64 says so),
# or the kernel again, once the UART was handed back.
HOLDER = re.compile(rb"console: typed input raises interrupt (\d+) now, through the claim that holds the UART|console: [^\r\n]* is the kernel's again")
POLLED = re.compile(rb'console: typed input is polled - ([^\r\n]*)')
# THE ROW'S WAKE FIELDS, which come first: the processor-power fields that follow them are not this gate's.
CORE_ROW = re.compile(r'\{cpu=(\d+), idle-ns=(\d+), halts=(\d+), wakes-timer=(\d+), wakes-ipi=(\d+), wakes-housekeeping=(\d+), wakes-device=(\d+), sources=\[([^\]]*)\][,}]')
SOURCE = re.compile(r'\{source=(\d+), count=(\d+)\}')
ARRIVED = b'a device arrived in the slot behind'
# THE TREE'S IDLE STATES, on the device-tree ports.
TREE_TABLE = re.compile(rb"processor: core (\d+)'s idle table from the device tree - (\d+) state\(s\)")
# THE STATE THAT LOSES THE CONTEXT is also `local-timer-stop`, as a power-down state is, and the timer is the reason checked first.
TREE_LOST = re.compile(rb"processor: core (\d+)'s idle state (\d+) \((\d+) us out\) is not entered - it (?:stops the core's timer|loses the core's context)")
FIRMWARE_REFUSED = re.compile(rb'processor: (PSCI refused CPU_SUSPEND|the SBI refused HART_SUSPEND)[^\r\n]*')
CORE_STATES = re.compile(r'\{cpu=(\d+), idle-ns=\d+, [^\[]*sources=\[[^\]]*\], states=\[((?:\{[^}]*\}(?:, )?)*)\]')
STATE = re.compile(r'\{entry=(\d+), unenterable=(\d+), exit-latency-us=(\d+), target-residency-us=(\d+), entries=(\d+), residency-ns=(\d+)\}')
# The table each core installs from the fixture: the halt, then by wake latency - the retention state (20 + 40 us),
# the one that loses the context (100 + 250 us), the deep retention state (500 + 1500 us).
TREE_LATENCIES = [1, 60, 350, 2000]


class GateError(Exception):
	pass


def note(message):
	print(f'check-tickless-idle: {message}', flush=True)


# THE GUEST'S SERIAL LINE, owned here: every byte the guest writes is kept and logged, and nothing is
# written to it but what the gate types.
class Serial:
	def __init__(self, sock, log, scale):
		self.sock = sock
		self.log = log
		self.scale = scale
		self.data = bytearray()
		# (the length of `data` after a chunk, the host time it arrived) - what a line is timed by.
		self.stamps = []

	# Read what has arrived, waiting at most `wait` seconds for the first byte; whether anything came.
	def pump(self, wait):
		ready, _, _ = select.select([self.sock], [], [], wait)
		if not ready:
			return False
		chunk = self.sock.recv(65536)
		if not chunk:
			raise GateError('the guest closed its serial line')
		self.data += chunk
		self.stamps.append((len(self.data), time.time()))
		self.log.write(chunk)
		self.log.flush()
		return True

	# EVERY LINE FROM `mark` ON, with the host time its last byte arrived.
	def lines_since(self, mark):
		out = []
		start = mark
		text = bytes(self.data)
		for end, stamp in self.stamps:
			if end <= mark:
				continue
			while True:
				newline = text.find(b'\n', start, end)
				if newline < 0:
					break
				out.append((stamp, lab.strip_ansi(text[start:newline]).decode(errors='replace')))
				start = newline + 1
		return out

	def text_since(self, mark):
		return lab.strip_ansi(bytes(self.data[mark:]))

	# Until `found(text since mark)` holds, or `timeout` seconds; the time it took.
	def wait_for(self, mark, found, timeout, what):
		started = time.monotonic()
		while not found(self.text_since(mark)):
			left = started + timeout - time.monotonic()
			if left <= 0:
				raise GateError(f'{what} did not happen within {timeout:.0f} s')
			self.pump(min(left, 0.05))
		return time.monotonic() - started

	# THE PROMPT, SETTLED: a prompt at the end of the output with nothing after it for a quarter of a second
	# (scaled), which is `lab`'s own rule - a program can print a prompt-shaped line and keep going.
	#
	# AND A PROMPT A LATE LINE BURIED IS ASKED FOR AGAIN, as `lab`'s boot wait asks: the shell printed its prompt and
	# a line written on its own clock - the network's address configuration, on riscv64 - landed after it, and nothing
	# prints another. Once the output has been quiet a while past a prompt this command produced, an empty line is
	# typed, and the shell answers it with one.
	def wait_prompt(self, mark, timeout, what):
		started = time.monotonic()
		settle = lab.PROMPT_SETTLE * self.scale
		nudge_from = mark
		quiet_since = time.monotonic()
		while True:
			if lab.PROMPT.search(self.text_since(mark)):
				if not self.pump(settle):
					return
				continue
			if time.monotonic() - quiet_since >= lab.BOOT_NUDGE_QUIET * self.scale and lab.PROMPT_ANYWHERE.search(lab.strip_ansi(bytes(self.data[nudge_from:]))):
				self.type(b'\n')
				nudge_from = len(self.data)
				quiet_since = time.monotonic()
			left = started + timeout - time.monotonic()
			if left <= 0:
				raise GateError(f'{what}: no shell prompt within {timeout:.0f} s')
			if self.pump(min(left, 0.2)):
				quiet_since = time.monotonic()

	# Wait until the guest has written nothing for `quiet` seconds - an idle machine - or `limit` passes.
	def settle(self, quiet, limit):
		end = time.monotonic() + limit
		while time.monotonic() < end:
			if not self.pump(quiet):
				return True
		return False

	def type(self, data):
		self.sock.sendall(data)

	# One shell command, typed and run to its prompt; its output.
	def run(self, command, timeout):
		mark = len(self.data)
		self.type(command.encode() + b'\n')
		self.wait_prompt(mark, timeout, f'`{command}`')
		return self.text_since(mark).decode(errors='replace')


# THE PER-CORE IDLE RECORDS, as `graph` prints them: cpu -> (halts, the four wake counts, {source: count}).
def cores(serial, timeout):
	output = serial.run('graph', timeout)
	rows = {}
	for match in CORE_ROW.finditer(output):
		cpu, _idle_ns, halts, timer, ipi, housekeeping, device, sources = match.groups()
		rows[int(cpu)] = {'halts': int(halts), 'timer': int(timer), 'ipi': int(ipi), 'housekeeping': int(housekeeping), 'device': int(device), 'sources': {int(s): int(c) for s, c in SOURCE.findall(sources)}}
	if not rows:
		raise GateError('`graph` printed no per-core idle record')
	return rows


def idle_states(serial, timeout):
	output = serial.run('graph', timeout)
	cores = {}
	for match in CORE_STATES.finditer(output):
		cores[int(match.group(1))] = [{'entry': int(m.group(1)), 'unenterable': int(m.group(2)), 'exit': int(m.group(3)), 'entries': int(m.group(5))} for m in STATE.finditer(match.group(2))]
	if not cores:
		raise GateError('`graph` printed no per-core idle state')
	return cores


# The transaction's own steps, as ServiceManager prints them when each begins.
FREEZE_STEP = re.compile(r'ServiceManager: sleep: step (flush|thaw) at tick (\d+)')

# How far boot-minus-monotonic may move without a sleep, as a level that holds: the counter's timestamps are taken to
# the millisecond and its two clocks read one after the other.
CLOCK_READ_JITTER_MS = 100
# How many counts a level has to hold to be one. A sample read across a preemption is one count high and back - the
# counter reads the monotonic clock first and the boot-time clock second (`sleepcheck`'s `stamp`), so a thread stopped
# between the two shows boot-time late and never early - while a sleep moves every count after it.
LEVEL_COUNTS = 3


def freeze_window(lines):
	"""The ticks the transaction's flush and thaw steps began at. Lines: (stamp, line)."""
	ticks = {}
	for _, line in lines:
		if found := FREEZE_STEP.search(line):
			if found.group(1) in ticks:
				raise GateError(f'the sleep transaction began its {found.group(1)} step twice')
			ticks[found.group(1)] = int(found.group(2))
	if set(ticks) != {'flush', 'thaw'} or ticks['thaw'] < ticks['flush']:
		raise GateError('the sleep transaction did not report its flush and thaw steps, in that order')
	return ticks['flush'], ticks['thaw']


def counter_head(text):
	"""The part of `text`, from its `count `, that follows the counter's format - where another writer's text began."""
	start = text.find('count ')
	if start < 0:
		return ''
	at = start
	for part in COUNTER_SHAPE:
		if part is None:
			while at < len(text) and text[at].isdigit():
				at += 1
			continue
		for char in part:
			if at < len(text) and text[at] == char:
				at += 1
			else:
				return text[start:at]
	return text[start:at]


def counter_rows(lines):
	"""(line index, stamp, count, mono ms, boot ms) for each counter line, the index and stamp where it began.
	Lines: (stamp, line)."""
	rows = []
	cut = None
	for at, (stamp, line) in enumerate(lines):
		text = PROMPT_TEXT.sub('', line)
		if cut is not None:
			found = COUNTER_LINE.match(cut[2] + text)
			if found:
				rows.append((cut[0], cut[1], *map(int, found.groups())))
				cut = None
				continue
		if found := COUNTER_LINE.search(text):
			if cut is not None:
				raise GateError(f'a counter line was cut at "{cut[2]}" and its end never arrived')
			rows.append((at, stamp, *map(int, found.groups())))
			continue
		# CUT ONLY IF IT REACHED ITS FIELDS: the typed `sleepcheck count 30 &` and the closing `count done` are not
		# counter lines, and a line cut before " m" is lost rather than guessed - which the caller's unbroken-run check
		# then refuses.
		head = counter_head(text)
		if re.match(r'count \d+ m', head):
			if cut is not None:
				raise GateError(f'a counter line was cut at "{cut[2]}" and its end never arrived')
			cut = (at, stamp, head)
	if cut is not None:
		raise GateError(f'a counter line was cut at "{cut[2]}" and its end never arrived')
	return rows


def sleep_clock_gap(rows, entered, resumed, reported_ms, margin_ms=IDLE_MARGIN_MS, frozen=None):
	"""Rows: (line index, host timestamp, count, mono ms, boot ms); boundaries: (index, stamp, tick);
	frozen: (flush tick, thaw tick), the window the counter could not sample in."""
	entered_at, entered_stamp, entered_tick = entered
	resumed_at, resumed_stamp, resumed_tick = resumed
	if not any(row[0] < entered_at for row in rows) or not any(row[0] > resumed_at for row in rows):
		raise GateError('the same counter did not span the sleep')
	# THE PAIR THAT SPANS THE SLEEP IS FOUND BY THE COUNTER, NOT BY THE WIRE. The counter's lines reach the serial
	# line through ConsoleService and the console driver, which the transaction suspends, while the kernel's
	# `sleep: entered` and `sleep: resumed` are written straight to the port. Under emulation the counter ran seconds
	# behind: counts sampled before the freeze arrived after `sleep: resumed`, so "the last line before the entry and
	# the first after the resume" were two samples from before the sleep, and the ports' check read a 0 ms step
	# between them in every run while the kernel's clocks were right. So: the counts in their own order, and the one
	# place where boot-minus-monotonic moved to a new level and held it.
	#
	# A LEVEL, NOT A STEP BETWEEN TWO SAMPLES. Compared count to count, one sample read across a preemption is a step
	# up and a step back down; under a sealed release's load on riscv64 that was 113 ms, and it failed the check
	# beside a sleep measured to the millisecond (2026-11-07). A preempted read only ever shows boot-time late, so a
	# level is the lowest offset over its counts: the sleep is the split where the level after it stands highest
	# above the level before it, the first such split, and its size is the difference of the two levels. Exactly
	# one: no stretch of LEVEL_COUNTS or more counts on either side holds a level of its own.
	#
	# THE FIRST SPLIT WITHIN THE NOISE OF THE BEST, not the best: the two clocks are printed to the millisecond, so a
	# split later in the run can stand a millisecond higher than the true one only because its suffix missed a
	# rounding - and taking it put the sleep inside the level after it.
	ordered = sorted(rows, key=lambda row: row[2])
	offsets = [row[4] - row[3] for row in ordered]
	lowest_before = list(itertools.accumulate(offsets, min))
	lowest_after = list(itertools.accumulate(reversed(offsets), min))[::-1]
	rises = [lowest_after[split + 1] - lowest_before[split] for split in range(len(offsets) - 1)]
	at = next(split for split, rise in enumerate(rises) if rise >= max(rises) - CLOCK_READ_JITTER_MS)
	before, after = lowest_before[at], lowest_after[at + 1]
	held_before = [min(offsets[start:at + 1]) - before for start in range(1, at + 2 - LEVEL_COUNTS)]
	held_after = [min(offsets[start:]) - after for start in range(at + 2, len(offsets) + 1 - LEVEL_COUNTS)]
	if any(abs(level) > CLOCK_READ_JITTER_MS for level in held_before + held_after):
		raise GateError('the boot-time clock left the monotonic one at more than one point')
	left, right = ordered[at], ordered[at + 1]
	# WHAT THE WIRE STILL PROVES, which is one direction: a sample that counts the sleep was taken after it, so it
	# cannot have reached the line before the kernel said it resumed. (A sample from before can arrive any time.)
	early = [row for row in ordered[at + 1:] if row[0] < resumed_at]
	if early:
		raise GateError(f'count {early[0][2]} counts the sleep and reached the serial line before `sleep: resumed`')
	mono = right[3] - left[3]
	boot = mono + after - before
	# AND THE PAIR SPANS THE FREEZE: the counter cannot sample between the transaction's flush (which begins after
	# the freeze is answered) and its thaw, so the pair either side of the sleep is at least that far apart on the
	# monotonic clock, which the ticks share. Two tick boundaries are the slack.
	if frozen is not None:
		frozen_ms = (frozen[1] - frozen[0]) * 10
		if mono < frozen_ms - 20:
			raise GateError(f'counts {left[2]} and {right[2]} are {mono} ms apart, less than the {frozen_ms} ms the counter was frozen - they do not span the sleep')
	# The application is frozen during awake driver preparation and restoration too. That interval
	# need not be shorter than the sleep. Counter output may also queue at ConsoleService after thaw,
	# so its receive timestamp is not its sample time. The kernel's entry/resume lines, by contrast,
	# are synchronously on the wire. Their ticks may include only the host-measured awake remainder.
	# boot-minus-mono alone is insufficient: boot is derived from mono plus the sleep total.
	host_sleep = (resumed_stamp - entered_stamp) * 1000
	tick_ms = (resumed_tick - entered_tick) * 10  # The ABI clock has 100 ticks per second.
	if mono < 0 or tick_ms < 0 or tick_ms > host_sleep - reported_ms + margin_ms:
		raise GateError(f'the monotonic clock did not exclude sleep: entry/resume ticks +{tick_ms} ms, host {host_sleep:.0f} ms, sleep {reported_ms} ms, counter +{mono} ms')
	if abs((boot - mono) - reported_ms) > 100:
		raise GateError(f'the clocks differ by {boot - mono} ms across sleep, not the reported {reported_ms} ms')
	return left, right, mono, boot


def clock_self_test():
	ordinary = [(0, 0.0, 1, 0, 0), (3, 5.2, 2, 200, 5200)]
	long_preparation = [(0, 0.0, 1, 0, 0), (3, 10.0, 2, 5000, 10000)]
	entered, resumed = (1, 0.1, 10), (2, 5.1, 10)
	sleep_clock_gap(ordinary, entered, resumed, 5000)
	sleep_clock_gap(ordinary, entered, resumed, 5000, frozen=(10, 20))
	sleep_clock_gap(long_preparation, (1, 4.0, 400), (2, 9.0, 400), 5000)
	# ConsoleService can deliver a queued counter batch later; synchronous kernel timing remains exact.
	sleep_clock_gap([ordinary[0], (3, 7.5, 2, 200, 5200)], entered, resumed, 5000)
	# A larger unrelated gap afterwards must not replace the pair that actually crosses the entry.
	left, right, _, _ = sleep_clock_gap(ordinary + [(4, 25.2, 3, 20200, 25200)], entered, resumed, 5000)
	if (left[2], right[2]) != (1, 2):
		raise GateError('the clock oracle selected an unrelated gap')
	# THE PORTS' CASE: count 2 was sampled before the freeze and reached the wire after the resume. The pair is 2 and
	# 3, which wire order would never have chosen - it took 1 and 2 and read a 0 ms step.
	late = [(0, 0.0, 1, 0, 0), (3, 5.2, 2, 100, 100), (4, 5.3, 3, 1100, 6100)]
	left, right, _, _ = sleep_clock_gap(late, entered, resumed, 5000, frozen=(10, 100))
	if (left[2], right[2]) != (2, 3):
		raise GateError('the clock oracle did not find the pair that spans the sleep among late counter lines')
	# A SAMPLE READ ACROSS A PREEMPTION, one count high and back: away from the sleep (the sealed release's 113 ms on
	# riscv64), on the last count before it and on the first after it. Each is still the one sleep, at its size.
	def run(offsets, before_entry):
		return [(at if at < before_entry else at + 2, at * 0.1 if at < before_entry else 5.0 + at * 0.1, at + 1, at * 100, at * 100 + offset) for at, offset in enumerate(offsets)]
	quiet, slept = [0] * 6, [5000] * 6
	# AND THE MILLISECOND THE TWO CLOCKS ARE ROUNDED TO: a level after the sleep that reads 5001 everywhere but once.
	for name, offsets in [('away from the sleep', [0, 0, 300, 0, 0, 0] + slept), ('on the last count before it', quiet[:5] + [400] + slept), ('on the first count after it', quiet + [5400] + slept[1:]), ('as a rounded millisecond', quiet + [5000] + [5001] * 8)]:
		left, right, _, boot = sleep_clock_gap(run(offsets, 6), (6, 0.6, 10), (7, 5.6, 10), 5000)
		if (left[2], right[2]) != (6, 7) or abs(boot - (right[3] - left[3]) - 5000) > 1:
			raise GateError(f'the clock oracle took a preempted read {name} for part of the sleep')
	early = (2, 0.1, 10), (3, 5.1, 10)
	cases = [
		('missing rebase', [ordinary[0], (3, 5.2, 2, 5200, 10200)], (entered, (2, 5.1, 510)), None),
		('double rebase with clamped ticks', [(0, 0.0, 1, 5000, 5000), (3, 5.2, 2, 200, 5200)], (entered, resumed), None),
		('backwards entry ticks', ordinary, (entered, (2, 5.1, 9)), None),
		('omitted boot sleep', [ordinary[0], (3, 5.2, 2, 200, 200)], (entered, resumed), None),
		('missing before', [ordinary[1]], (entered, resumed), None),
		('missing after', [ordinary[0]], (entered, resumed), None),
		('a step of the wrong size', [ordinary[0], (3, 5.2, 2, 200, 3200)], (entered, resumed), None),
		('the clocks parting twice', ordinary + [(4, 5.3, 3, 300, 5800)], (entered, resumed), None),
		('a second level held after the sleep', run(quiet + slept + [5900] * 4, 6), ((6, 0.6, 10), (7, 5.6, 10)), None),
		('a level held before the sleep', run([0] * 4 + [700] * 4 + slept, 8), ((8, 0.8, 10), (9, 5.8, 10)), None),
		('a sample counting the sleep before the resume', [(0, 0.0, 1, 0, 0), (1, 0.05, 2, 200, 5200), (4, 5.2, 3, 300, 5300)], early, None),
		('a pair that does not span the freeze', ordinary, (entered, resumed), (10, 100)),
	]
	for name, rows, (entry, resume), frozen in cases:
		try:
			sleep_clock_gap(rows, entry, resume, 5000, frozen=frozen)
		except GateError:
			continue
		raise GateError(f'the clock oracle accepted {name}')
	# THE KERNEL INSIDE A COUNTER LINE, as riscv64 printed it on 2026-11-02, and a cut earlier in the line.
	rows = counter_rows([(0.0, 'sleepcheck count 30 &\r'), (0.1, 'sleepcheck: count 52 mono-ms 555713 boot-ms 555713\r'), (0.2, 'sleepcheck: count 53 mono-ms 555754 boot-ms 55575DeviceManager: resumed console-uart (platform device 22)\r'), (0.3, '4\r'), (0.4, 'count 54 mono-ms ServiceManager: sleep: step thaw at tick 1\r'), (0.5, 'DeviceManager: the sleep\'s resume step is done\r'), (0.6, '555808 boot-ms 555808\r'), (0.7, 'sleepcheck: count done\r')])
	if [row[2:] for row in rows] != [(52, 555713, 555713), (53, 555754, 555754), (54, 555808, 555808)] or [row[0] for row in rows] != [1, 2, 4]:
		raise GateError(f'counter lines cut by another writer were not rejoined where they began: {rows}')
	for name, lines in [('a cut whose end never arrives', [(0.0, 'count 7 mono-ms 70 boot-ms 7DeviceManager: x'), (0.1, 'count 8 mono-ms 80 boot-ms 80')]), ('a cut at the end of the transcript', [(0.0, 'count 7 mono-ms 70 boot-ms 7DeviceManager: x')])]:
		try:
			counter_rows(lines)
		except GateError:
			continue
		raise GateError(f'the counter reader accepted {name}')
	window = freeze_window([(0.0, 'ServiceManager: sleep: step flush at tick 7'), (5.2, 'ServiceManager: sleep: step thaw at tick 19')])
	if window != (7, 19):
		raise GateError('the freeze window was not read from the transaction\'s steps')
	for name, lines in [('no thaw', [(0.0, 'ServiceManager: sleep: step flush at tick 7')]), ('thaw before flush', [(0.0, 'ServiceManager: sleep: step flush at tick 9'), (1.0, 'ServiceManager: sleep: step thaw at tick 8')])]:
		try:
			freeze_window(lines)
		except GateError:
			continue
		raise GateError(f'the freeze window accepted {name}')
	note('clock oracle self-test PASS (ten positive cases, twelve rejected false outcomes, cut counter lines rejoined and two unrejoinable ones refused, the freeze window read and two malformed ones refused)')


def suspend_to_idle(target, serial, scale):
	# WHAT THE FIRMWARE OFFERS, said at boot - checked, not assumed.
	offered = SLEEP_OFFERED.search(bytes(serial.data))
	if not offered:
		raise GateError('the kernel never said what this firmware offers a sleep')
	note(f'{target}: {lab.strip_ansi(offered.group(0)).decode(errors="replace")}')
	if target == 'aarch64':
		if b'sleep: PSCI SYSTEM_SUSPEND is not offered by this firmware' not in serial.data:
			raise GateError('ARM firmware no longer reports SYSTEM_SUSPEND absent; this platform needs its offered-state proof')
		status = serial.run('sleepctl status', 60 * scale)
		states = re.search(r'^states: ([^\r\n]*)', status, re.M)
		if states is None or 'suspend to RAM' in states.group(1) or 'suspend to idle' not in states.group(1):
			raise GateError(f'ARM sleep admission disagrees with the absent firmware feature: {status}')
		mark = len(serial.data)
		request = serial.run(f'sleepctl suspend ram {IDLE_WAKE_S}', 60 * scale)
		record = serial.run('sleepctl last', 60 * scale)
		check_arm_ram_refusal(request, record, serial.text_since(mark).decode(errors='replace'))
		note('aarch64: RAM refused as Unsupported before any transaction or kernel entry; last record names the absent state')
	mark = len(serial.data)
	serial.type(b'sleepcheck count 30 &\n')
	serial.wait_for(mark, lambda seen: b'sleepcheck: count 1 ' in seen, 60 * scale, 'the counter in the background')
	serial.pump(2.0)
	serial.type(f'sleepctl suspend idle {IDLE_WAKE_S}\n'.encode())
	serial.wait_for(mark, lambda seen: b'ServiceManager: sleep: the transaction ended' in seen, 300 * scale, 'the suspend to idle')
	serial.wait_for(mark, lambda seen: b'sleepcheck: count done' in seen, 120 * scale, 'the counter\'s end')
	lines = serial.lines_since(mark)
	entered = next(((at, stamp, int(found.group(1))) for at, (stamp, line) in enumerate(lines) if (found := re.search(r'sleep: entered \(suspend to idle, at tick (\d+)\)', line))), None)
	resumed = next(((at, stamp, int(found.group(2)), int(found.group(1))) for at, (stamp, line) in enumerate(lines) if (found := re.search(r'sleep: resumed \(the timed wake, after (\d+) ms, at tick (\d+)\)', line))), None)
	if entered is None or resumed is None:
		raise GateError('suspend to idle: no `sleep: entered` or no `sleep: resumed` naming the timed wake')
	entered_at, entered_stamp, _ = entered
	resumed_at, resumed_stamp, _, reported = resumed
	slept = (resumed_stamp - entered_stamp) * 1000
	if slept < IDLE_WAKE_S * 1000 - IDLE_MARGIN_MS:
		raise GateError(f'suspend to idle: the host measured {slept:.0f} ms between the kernel\'s lines, under the {IDLE_WAKE_S} s wake')
	# A PROMPT THE SHELL PRINTED BESIDE A COUNTER LINE is taken out first: `sleepctl` ends at the serial shell while
	# the counter writes in the background, and the two share the console - the prompt then "sleepcheck: count 23 ..."
	# is one counter line, and so was "sleepcheck: ", the prompt, then "count 23 ..." while the counter wrote its lines
	# in three messages. A kernel line inside one is rejoined around.
	rows = counter_rows(lines)
	inside = [row for row in rows if entered_at < row[0] < resumed_at]
	if inside:
		raise GateError(f'suspend to idle: {len(inside)} counter line(s) arrived while the machine slept, the first count {inside[0][2]}')
	if len(rows) < 2 or [row[2] for row in rows] != list(range(rows[0][2], rows[0][2] + len(rows))):
		raise GateError('suspend to idle: the counter skipped or repeated a value, or printed nothing')
	if reported < IDLE_WAKE_S * 1000 - IDLE_MARGIN_MS or reported > slept + IDLE_MARGIN_MS:
		raise GateError(f'suspend to idle: the reported {reported} ms sleep disagrees with its timed wake or the host interval {slept:.0f} ms')
	left, right, mono, boot = sleep_clock_gap(rows, entered, resumed[:3], reported, frozen=freeze_window(lines))
	record = serial.run('sleepctl last', 60 * scale)
	if 'woken by the timed wake' not in record:
		raise GateError(f'suspend to idle: the record does not name the timed wake: {record}')
	parked = 0
	for found in PARKED.finditer(record):
		parked += 1
		cpu, timer, ipi, device = map(int, found.groups())
		if device:
			raise GateError(f'suspend to idle: cpu{cpu} woke {device} time(s) for a device')
		if (cpu == 0 and (timer > 1 or ipi > 1)) or (cpu != 0 and (timer or ipi > 1)):
			raise GateError(f'suspend to idle: cpu{cpu} woke {timer} time(s) for the timer and {ipi} for an IPI')
	if not parked:
		raise GateError('suspend to idle: the record lists no core')
	note(f'{target}: suspend to idle - the host measured {slept:.0f} ms for a {IDLE_WAKE_S} s wake with the counter silent, count {left[2]} then {right[2]} (monotonic +{mono} ms, boot-time +{boot} ms), {parked} cores parked with no wake but the wake')


def check_arm_ram_refusal(request, record, trace):
	if 'sleepctl: the sleep was refused - Unsupported' not in request:
		raise GateError(f'ARM RAM request had no explicit unsupported-state refusal: {request}')
	if any(token in trace for token in ('ServiceManager: sleep: accepted ', 'ServiceManager: sleep: step ', 'sleep: entered (')):
		raise GateError('ARM unsupported RAM request started a sleep transaction or entered a sleep')
	if 'last sleep: suspend to RAM (Requested)' not in record or not re.search(
		r'^\s*refused before anything was frozen:  - this machine does not offer suspend to RAM\s*$', record, re.M
	):
		raise GateError(f'ARM RAM refusal did not leave the matching pre-freeze record: {record}')


def arm_refusal_self_test():
	request = 'sleepctl: the sleep was refused - Unsupported\n'
	record = 'last sleep: suspend to RAM (Requested)\n  refused before anything was frozen:  - this machine does not offer suspend to RAM\n'
	check_arm_ram_refusal(request, record, request + record)
	cases = [
		('unrelated refusal', request.replace('Unsupported', 'AccessDenied'), record, request + record),
		('missing reply', '', record, record),
		('admitted transaction', request, record, request + 'ServiceManager: sleep: accepted suspend to RAM\n'),
		('freeze before refusal', request, record, request + 'ServiceManager: sleep: step freeze at tick 10\n'),
		('entered idle instead', request, record, request + 'sleep: entered (suspend to idle, at tick 10)\n'),
		('stale different state', request, record.replace('last sleep: suspend to RAM', 'last sleep: suspend to idle'), request),
		('refused for another reason', request, record.replace('this machine does not offer suspend to RAM', 'a driver refused'), request),
		('unwound after entry', request, record.replace('refused before anything was frozen', 'unwound at drivers'), request),
	]
	for name, response, last, trace in cases:
		try:
			check_arm_ram_refusal(response, last, trace)
		except GateError:
			continue
		raise GateError(f'the ARM early-refusal oracle accepted {name}')
	note('ARM refusal oracle self-test PASS (unsupported accepted, eight false outcomes rejected)')


def wakes_on(rows, identity):
	return sum(row['sources'].get(identity, 0) for row in rows.values())


# 4. A COMMAND TYPED WHILE A PROGRAM FLOODS THE TERMINAL IS ANSWERED WHILE THE FLOOD RUNS.
#
# ConsoleService waits on the keyboard, every terminal's output, the display's events and the serial line's input at
# once and serves one of them a pass; `wait_any` answers the LOWEST ready index, and the serial line's input was the
# last - so output that was ready at every wait kept a typed command unread until the output stopped. On riscv64 a
# counter printing every 100 ms did that for thirty seconds (`system-suspend-riscv64`, 2026-11-08): each pass took
# longer than the counter's period, and the `sleepctl` typed over it was read only after the counter's last line.
# `sleepcheck flood` is that output at its worst - ready at every wait on any machine - and `uname` typed into it has to
# be answered before it ends: until ConsoleService took its channels in turn, the echo arrived after `flood done`.
# ConsoleService is the same program on every target; x86_64 runs it fastest, so it is asked there.
FLOOD_S = 45
# The program's own lines, not the echo of the command that started it - which says `flood 45` too.
FLOOD_LINE = b'sleepcheck: flood '
# How many of them are on the wire before the command is typed: the console is busy with the flood by then.
FLOOD_STARTED = 50
FLOOD_ANSWER = b'LiberSystem'
FLOOD_END = b'flood done'


# The verdict on where the answer and the flood's last line landed, as offsets into the serial output: None when the
# command was answered while the flood ran, the failure in words otherwise.
def flood_verdict(typed, answered, ended):
	if ended is not None and ended < typed:
		return 'the flood\'s output reached the serial line only once it had ended - the console mirrored nothing while a program printed faster than it rendered'
	if answered is None:
		return '`uname`, typed while `sleepcheck flood` printed, was never answered'
	if ended is not None and answered > ended:
		return '`uname`, typed while `sleepcheck flood` printed, was answered only after the flood ended - typed input waited behind a terminal that kept printing'
	return None


def flood_self_test():
	if flood_verdict(100, 150, 900) is not None:
		raise GateError('the flood verdict refused an answer that came while the flood ran')
	for name, (typed, answered, ended) in [('an answer after the flood', (100, 950, 900)), ('a flood that ended before the command was typed', (100, 950, 50)), ('no answer at all', (100, None, 900))]:
		if flood_verdict(typed, answered, ended) is None:
			raise GateError(f'the flood verdict accepted {name}')
	note('flood verdict self-test PASS (an answer inside the flood accepted, three false outcomes refused)')


def typed_under_flood(target, serial, scale):
	flood_self_test()
	mark = len(serial.data)
	serial.type(f'sleepcheck flood {FLOOD_S} &\n'.encode())
	serial.wait_for(mark, lambda seen: seen.count(FLOOD_LINE) >= FLOOD_STARTED, 60 * scale, f'{FLOOD_STARTED} lines of `sleepcheck flood`')
	typed = len(serial.data)
	typed_at = time.time()
	serial.type(b'uname\n')
	# SEARCHED AS IT ARRIVES, each token from where the last look ended less its own length: the flood is megabytes, and
	# `wait_for` reads everything since its mark again at every look.
	found = {}
	scanned = typed
	deadline = time.monotonic() + (FLOOD_S + 120) * scale
	while FLOOD_ANSWER not in found or FLOOD_END not in found:
		for token, start in ((FLOOD_ANSWER, typed), (FLOOD_END, mark)):
			if token not in found:
				at = serial.data.find(token, max(start, scanned - len(token)) if scanned > typed else start)
				if at >= 0:
					found[token] = at
		scanned = len(serial.data)
		if FLOOD_END in found and found[FLOOD_END] < typed:
			break
		if time.monotonic() >= deadline:
			break
		serial.pump(0.05)
	failure = flood_verdict(typed, found.get(FLOOD_ANSWER), found.get(FLOOD_END))
	if failure is not None:
		raise GateError(failure)
	if FLOOD_END not in found:
		raise GateError(f'`sleepcheck flood {FLOOD_S}` did not end within {FLOOD_S + 120} s')
	answered_at = next(stamp for end, stamp in serial.stamps if end > found[FLOOD_ANSWER])
	lines = serial.data.count(b'\n', mark, found[FLOOD_END])
	note(f'{target}: `uname` typed into a {FLOOD_S} s flood of output ({lines} lines) was answered {(answered_at - typed_at) * 1000:.0f} ms later, while the flood ran')
	mark = len(serial.data)
	serial.type(b'\n')
	serial.wait_prompt(mark, 60 * scale, 'the prompt after the flood')


# THE IDENTITY TYPED INPUT RAISES NOW, which is not always the one the boot armed (corrected 2026-10-26). On riscv64
# a claim that takes the console UART arms its APLIC source to an identity of the claim's own window, and every typed
# byte from then on wakes the machine under that identity - so counting the boot's identity found none of them, and
# the gate failed on a kernel that did exactly what it should. The last holder the console named is the line's.
def console_identity(log, armed):
	identity = armed
	for event in HOLDER.finditer(log):
		identity = int(event.group(1)) if event.group(1) else armed
	return identity


def woke(row):
	return row['timer'] + row['ipi'] + row['housekeeping'] + row['device']


# ONE QMP COMMAND, over the cold guest's own QMP socket: the plan asks for a QMP `device_add`, and QMP says
# whether it failed in a field rather than in prose.
def qmp(path, command, arguments, timeout):
	conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
	conn.settimeout(timeout)
	conn.connect(path)
	stream = conn.makefile('rwb')

	def answer():
		while True:
			line = stream.readline()
			if not line:
				raise GateError('QMP closed the connection')
			message = json.loads(line)
			# Events arrive on the same stream; only a reply ends a command.
			if 'return' in message or 'error' in message or 'QMP' in message:
				return message

	answer()
	for execute, args in (('qmp_capabilities', None), (command, arguments)):
		request = {'execute': execute}
		if args is not None:
			request['arguments'] = args
		stream.write(json.dumps(request).encode() + b'\n')
		stream.flush()
		reply = answer()
		if 'error' in reply:
			raise GateError(f'QMP refused {execute}: {reply["error"].get("desc", reply["error"])}')
	conn.close()


# EVERY TARGET A GATE BOOTS IS ASKED BEFORE THE FIRST BOOT, under the runner: one whose staging is not current in the
# development profile refuses the whole step with the commands that make all of them current (`staging-state.sh`),
# which the runner runs before it runs the step again - so this gate builds nothing and only reads, as a cold run.
def refuse_unless_current(gate, targets):
	if os.environ.get('LIBER_STAGING_RUNNER') != '1':
		return
	env = dict(os.environ, LIBER_DEVELOPMENT='1')
	stale = [line for target in targets for line in lab.staging_not_current(target, env)]
	if stale:
		for line in stale:
			print(f'{gate}: REFUSED: {line}', file=sys.stderr, flush=True)
		sys.exit(lab.STATE_REFUSED)


# A RUN-PRIVATE STATE DIRECTORY for one guest, where the runner binds its channel, monitor and QMP sockets: under
# the cold names a guest of this gate and a cold run of the same target took one set of sockets between them.
def guest_state(name):
	state = os.path.join(lab.BUILD_ROOT, 'tmp', f'{name}.{os.getpid()}')
	os.makedirs(state, exist_ok=True)
	return state


def build(target):
	# THE DEVELOPMENT PROFILE, and the ordinary strip: `STRIP=none` re-stages every library unstripped over
	# a tree whose consumers recorded the stripped ones, and the build then refuses the mix.
	env = dict(os.environ, LIBER_DEVELOPMENT='1')
	# NOTHING IS BUILT WHEN WHAT THE GUEST BOOTS IS CURRENT, as for a cold run (`lab.staging_not_current`).
	stale = lab.staging_not_current(target, env)
	if not stale:
		note(f'the {target} staging is current in the development profile; nothing is built')
		return
	for line in stale:
		note(line)
	note(f'building {target} with the development profile')
	if subprocess.run(lab.build_command(target), cwd=SRC, env=env).returncode != 0:
		raise GateError(f'the {target} system did not build')
	# THE BOOTABLE VOLUME the x86_64 medium carries, which the build above does not write - the reason
	# `lab scenario-cold` builds it too.
	if target == 'x86_64':
		volume = [lab.BUILD_SH, '--arch', 'x86_64', '--kernel-on-volume', '--dma-mode', 'enforcing-required', '--part', 'volume']
		if subprocess.run(volume, cwd=SRC, env=env).returncode != 0:
			raise GateError('the x86_64 bootable volume did not build')


def drive(target):
	scale = 1.0 if target == 'x86_64' else 10.0
	serial_path = os.path.join(lab.BUILD, f'tickless-serial-{target}.sock')
	state = guest_state(f'tickless-{target}')
	qmp_path = os.path.join(state, 'qemu-qmp.sock')
	log_path = os.path.join(lab.BUILD, f'tickless-{target}.log')
	runner_log = open(os.path.join(lab.BUILD, f'tickless-{target}-runner.log'), 'wb')
	kernel = os.path.join(lab.BUILD_ROOT, 'cargo', 'kernel', TRIPLES[target], 'debug', 'kernel')
	if os.path.exists(serial_path):
		os.unlink(serial_path)
	# THE COLD SCENARIO RUNNER'S GUEST, with the serial line on a socket this gate holds rather than in a
	# file: `server` without `nowait`, so QEMU waits for the connection and no boot byte is lost.
	env = dict(os.environ, LIBER_DEVELOPMENT='1', DEV_PROFILE='1', COLD='1', SERIAL=f'unix:{serial_path},server', SMP=os.environ.get('SMP', '4'), LIBER_RUN_MODE='development', LIBER_DEV_STATE=state)
	if target == 'x86_64':
		env['HOSTFWD_PORT'] = os.environ.get('HOSTFWD_PORT') or lab.free_port()
	if target != 'x86_64':
		env['UEFI'] = '1'
		env['IDLE_FIXTURE'] = '1'
	note(f'booting {target}; serial log {os.path.relpath(log_path, SRC)}')
	guest = subprocess.Popen(['bash', 'harness/qemu-run.sh', target, kernel], cwd=SRC, env=env, stdout=runner_log, stderr=runner_log, start_new_session=True)
	try:
		sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
		deadline = time.monotonic() + 300
		while True:
			if guest.poll() is not None:
				raise GateError(f'the {target} guest exited before it opened its serial line (see {runner_log.name})')
			try:
				sock.connect(serial_path)
				break
			except OSError:
				if time.monotonic() > deadline:
					raise GateError(f'the {target} guest never opened its serial line') from None
				time.sleep(0.5)
		with open(log_path, 'wb') as log:
			return check(target, Serial(sock, log, scale), qmp_path, scale)
	finally:
		# The runner's whole group - `qemu-run.sh` and the QEMU it execs - so no guest outlives the check.
		try:
			os.killpg(guest.pid, signal.SIGTERM)
			guest.wait(timeout=20)
		except (OSError, subprocess.TimeoutExpired):
			try:
				os.killpg(guest.pid, signal.SIGKILL)
			except OSError:
				pass
			guest.wait()
		runner_log.close()
		shutil.rmtree(state, ignore_errors=True)


def check(target, serial, qmp_path, scale):
	# A BOOT TO A SHELL, and on an emulated target that is minutes, said as it goes.
	note(f'waiting for the {target} shell')
	serial.wait_prompt(0, 1800 if scale > 1 else 300, f'the {target} boot')
	boot = bytes(serial.data)
	polled = POLLED.search(boot)
	if polled:
		raise GateError(f'the kernel polls the console UART - {polled.group(1).decode(errors="replace")} - so no typed byte can raise an interrupt')
	armed = ARMED.search(boot)
	if not armed:
		raise GateError('the kernel never said which interrupt typed input raises')
	identity = int(armed.group(1))
	smp = int(os.environ.get('SMP', '4'))
	if target != 'x86_64':
		# THE TREE'S TABLES, installed at boot on every core, and the state that loses the context held out on each.
		tables = {int(cpu): int(count) for cpu, count in TREE_TABLE.findall(boot)}
		if sorted(tables) != list(range(smp)) or any(count != len(TREE_LATENCIES) for count in tables.values()):
			raise GateError(f'the kernel did not install the tree\'s idle states on every core: {tables} (wanted {len(TREE_LATENCIES)} states on cores 0..{smp - 1})')
		lost = {int(cpu) for cpu, _index, _us in TREE_LOST.findall(boot)}
		if lost != set(range(smp)):
			raise GateError(f'the state that loses the core\'s context was not said held out on every core: {sorted(lost)}')
		note(f'{target}: every core installed the tree\'s {len(TREE_LATENCIES)} idle states, the one that loses the context held out')
	# AN IDLE MACHINE, which is the claim: the boot's own output has stopped before anything is typed.
	serial.settle(2.0 * scale, 120 * scale)
	held = console_identity(bytes(serial.data), identity)
	if held != identity:
		note(f'{target}: the console UART was handed to its driver, whose claim raises interrupt {held} where the boot armed {identity}')
		identity = held
	# THE CONTROL: two reads with nothing typed between them but the second `graph`. Typing that command is
	# typing too, and its own wakes are in every later read - so they are measured here and taken off.
	first = cores(serial, 60 * scale)
	before = cores(serial, 60 * scale)
	control = wakes_on(before, identity) - wakes_on(first, identity)
	latencies = []
	for burst in range(BURSTS):
		# Idle again before each burst, so it lands on a machine with nothing to do.
		serial.settle(1.0 * scale, 30 * scale)
		text = f'echo tickless-{burst}'
		# TYPED AS A PERSON TYPES IT, a key at a time, each timed from the host's write to the shell's echo:
		# the interval the host can observe. A whole line written at once lands in the UART's FIFO as one
		# interrupt, and whether that one found the boot processor halted is a coin the housekeeping it
		# still wakes for tosses; fifteen keys are fifteen interrupts.
		for typed in range(1, len(text) + 1):
			mark = len(serial.data)
			written = time.monotonic()
			serial.type(text[typed - 1].encode())
			serial.wait_for(mark, lambda seen, want=text[typed - 1].encode(): want in seen, 30 * scale, f'the echo of key {typed} of {text!r}')
			latency = time.monotonic() - written
			latencies.append(latency)
			if latency > ECHO_BOUND:
				raise GateError(f'key {typed} of {text!r} was echoed {latency * 1000:.0f} ms after the host wrote it, past the {ECHO_BOUND * 1000:.0f} ms bound')
			# A person's pace, so the machine is idle again between keys.
			serial.settle(0.05 * scale, 1.0 * scale)
		mark = len(serial.data)
		serial.type(b'\n')
		serial.wait_prompt(mark, 30 * scale, f'`{text}`')
	after = cores(serial, 60 * scale)
	raised = wakes_on(after, identity) - wakes_on(before, identity) - control
	if raised < BURSTS:
		raise GateError(f'{BURSTS} typed bursts woke the idle machine {raised} time(s) on interrupt {identity} beyond what typing `graph` does ({control}): typed input is being found by a poll, not raised by the UART')
	slowest = max(latencies)
	note(f'{target}: {BURSTS} bursts, {len(latencies)} keys, each echoed within {slowest * 1000:.0f} ms (median {sorted(latencies)[len(latencies) // 2] * 1000:.0f} ms), and they woke the idle machine {raised} time(s) on the UART\'s interrupt {identity} (typing `graph` alone: {control})')
	# THE IDLE RATE, from the same records: each core's wakes over a quiet interval. The `graph` read that
	# closes the interval wakes the machine too and is counted in, so the rate is an upper bound.
	serial.settle(2.0 * scale, 30 * scale)
	first = cores(serial, 60 * scale)
	states_first = idle_states(serial, 60 * scale) if target != 'x86_64' else {}
	started = time.monotonic()
	while time.monotonic() - started < IDLE_SAMPLE:
		serial.pump(max(0.0, started + IDLE_SAMPLE - time.monotonic()))
	last = cores(serial, 60 * scale)
	if target != 'x86_64':
		# THE STATES OVER THE SAME QUIET INTERVAL: the retention states entered through the firmware, the other never.
		states_last = idle_states(serial, 60 * scale)
		retained = 0
		for cpu in sorted(states_last):
			now, then = states_last[cpu], states_first.get(cpu, [])
			if [state['exit'] for state in now] != TREE_LATENCIES or len(then) != len(now):
				raise GateError(f'core {cpu}\'s record does not carry the tree\'s states: {now}')
			if now[2]['unenterable'] == 0 or now[2]['entries'] != then[2]['entries']:
				raise GateError(f'core {cpu} entered the state that loses its context, or did not hold it out: {now[2]}')
			retained += (now[1]['entries'] - then[1]['entries']) + (now[3]['entries'] - then[3]['entries'])
		if retained == 0:
			raise GateError('over the idle sample no core entered either retention state')
		refused = FIRMWARE_REFUSED.search(bytes(serial.data))
		if refused:
			raise GateError(f'the firmware refused a state: {refused.group(0).decode(errors="replace")}')
		note(f'{target}: the idle cores entered the tree\'s retention states {retained} time(s) over the sample, through the firmware, and the state that loses the context never')
	elapsed = time.monotonic() - started
	rates = []
	for cpu in sorted(last):
		if cpu in first:
			rates.append(f'cpu{cpu} {(woke(last[cpu]) - woke(first[cpu])) / elapsed:.1f}/s')
	note(f'{target}: an idle core woke {", ".join(rates)} over {elapsed:.0f} s')
	# THE PLUGGED DEVICE, last: on riscv64 its slot event arrives under the UART's identity.
	mark = len(serial.data)
	qmp(qmp_path, 'device_add', {'driver': 'virtio-serial-pci', 'bus': 'hotplug0', 'id': 'tickless0'}, 30)
	took = serial.wait_for(mark, lambda seen: ARRIVED in seen, 60 * scale, 'the arrival of a device plugged into the idle machine')
	note(f'{target}: a device plugged into the idle machine was seen {took * 1000:.0f} ms later')
	if target == 'x86_64':
		serial.settle(1.0 * scale, 30 * scale)
		typed_under_flood(target, serial, scale)
	if target != 'x86_64':
		serial.settle(2.0 * scale, 120 * scale)
		suspend_to_idle(target, serial, scale)
	return True


def main():
	args = sys.argv[1:]
	if args == ['--self-test']:
		clock_self_test()
		arm_refusal_self_test()
		flood_self_test()
		return 0
	skip_build = '--no-build' in args
	targets = [a for a in args if not a.startswith('--')] or list(TARGETS)
	for target in targets:
		if target not in TARGETS:
			print(f'check-tickless-idle: unknown target {target!r}', file=sys.stderr)
			return 2
	if not skip_build:
		refuse_unless_current('check-tickless-idle', targets)
	failed = []
	for target in targets:
		try:
			if not skip_build:
				build(target)
			drive(target)
		except GateError as error:
			print(f'check-tickless-idle: {target}: FAIL - {error}', file=sys.stderr, flush=True)
			failed.append(target)
	if failed:
		print(f'check-tickless-idle: FAIL on {", ".join(failed)}', file=sys.stderr)
		return 1
	note(f'PASS on {", ".join(targets)}')
	return 0


if __name__ == '__main__':
	sys.exit(main())
