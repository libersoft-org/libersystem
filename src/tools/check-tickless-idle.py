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
# at least the wake less a margin, the counter's next value after with its monotonic clock excluding the sleep
# and its boot-time clock including it, with the synchronous entry/resume ticks checked against host time - and the parking
# from the last sleep's record: no core woke for a device,
# cpu0 at most once for the timer, every other core at most once, for the IPI that ends its park.
#
# ONE GUEST AT A TIME: each target is booted, driven and torn down before the next one boots.
#
# usage: check-tickless-idle.py [--no-build] [x86_64|aarch64|riscv64 ...]   (default: all three)

import json
import os
import re
import select
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
# WITHOUT ITS PREFIX: the counter writes "sleepcheck: " and the rest of its line in two writes, and another program's
# lines - `sleepctl`'s record of the last sleep, then its prompt - can land between them.
COUNTER = re.compile(r'count (\d+) mono-ms (\d+) boot-ms (\d+)')
PARKED = re.compile(r'cpu(\d+): woke (\d+) time\(s\) for the timer, (\d+) for an IPI, (\d+) for a device')

ARMED = re.compile(rb'console: typed input raises interrupt (\d+)')
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


def sleep_clock_gap(rows, entered, resumed, reported_ms, margin_ms=IDLE_MARGIN_MS):
	"""Rows: (line index, host timestamp, count, mono ms, boot ms); boundaries: (index, stamp, tick)."""
	entered_at, entered_stamp, entered_tick = entered
	resumed_at, resumed_stamp, resumed_tick = resumed
	before = [row for row in rows if row[0] < entered_at]
	after = [row for row in rows if row[0] > resumed_at]
	if not before or not after:
		raise GateError('the same counter did not span the sleep')
	left, right = before[-1], after[0]
	mono, boot = right[3] - left[3], right[4] - left[4]
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
	sleep_clock_gap(long_preparation, (1, 4.0, 400), (2, 9.0, 400), 5000)
	# ConsoleService can deliver a queued counter batch later; synchronous kernel timing remains exact.
	sleep_clock_gap([ordinary[0], (3, 7.5, 2, 200, 5200)], entered, resumed, 5000)
	# A larger unrelated gap afterwards must not replace the pair that actually crosses the entry.
	left, right, _, _ = sleep_clock_gap(ordinary + [(4, 25.2, 3, 20200, 25200)], entered, resumed, 5000)
	if (left[2], right[2]) != (1, 2):
		raise GateError('the clock oracle selected an unrelated gap')
	cases = [
		('missing rebase', [ordinary[0], (3, 5.2, 2, 5200, 10200)], (2, 5.1, 510)),
		('double rebase with clamped ticks', [(0, 0.0, 1, 5000, 5000), (3, 5.2, 2, 200, 5200)], resumed),
		('backwards entry ticks', ordinary, (2, 5.1, 9)),
		('omitted boot sleep', [ordinary[0], (3, 5.2, 2, 200, 200)], resumed),
		('missing before', [ordinary[1]], resumed),
		('missing after', [ordinary[0]], resumed),
	]
	for name, rows, boundary in cases:
		try:
			sleep_clock_gap(rows, entered, boundary, 5000)
		except GateError:
			continue
		raise GateError(f'the clock oracle accepted {name}')
	note('clock oracle self-test PASS (four positive cases, six rejected false outcomes)')


def suspend_to_idle(target, serial, scale):
	# WHAT THE FIRMWARE OFFERS, said at boot - checked, not assumed.
	offered = SLEEP_OFFERED.search(bytes(serial.data))
	if not offered:
		raise GateError('the kernel never said what this firmware offers a sleep')
	note(f'{target}: {lab.strip_ansi(offered.group(0)).decode(errors="replace")}')
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
	rows = []
	for at, (stamp, line) in enumerate(lines):
		# A PROMPT THE SHELL PRINTED INTO A COUNTER LINE is taken out first: `sleepctl` ends at the serial shell while
		# the counter writes in the background, and the two share the console - "sleepcheck: " then the prompt then
		# "count 23 ..." is one counter line, not a missing value.
		found = COUNTER.search(PROMPT_TEXT.sub('', line))
		if found:
			rows.append((at, stamp, int(found.group(1)), int(found.group(2)), int(found.group(3))))
	inside = [row for row in rows if entered_at < row[0] < resumed_at]
	if inside:
		raise GateError(f'suspend to idle: {len(inside)} counter line(s) arrived while the machine slept, the first count {inside[0][2]}')
	if len(rows) < 2 or [row[2] for row in rows] != list(range(rows[0][2], rows[0][2] + len(rows))):
		raise GateError('suspend to idle: the counter skipped or repeated a value, or printed nothing')
	if reported < IDLE_WAKE_S * 1000 - IDLE_MARGIN_MS or reported > slept + IDLE_MARGIN_MS:
		raise GateError(f'suspend to idle: the reported {reported} ms sleep disagrees with its timed wake or the host interval {slept:.0f} ms')
	left, right, mono, boot = sleep_clock_gap(rows, entered, resumed[:3], reported)
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


def wakes_on(rows, identity):
	return sum(row['sources'].get(identity, 0) for row in rows.values())


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


def build(target):
	# THE DEVELOPMENT PROFILE, and the ordinary strip: `STRIP=none` re-stages every library unstripped over
	# a tree whose consumers recorded the stripped ones, and the build then refuses the mix.
	env = dict(os.environ, LIBER_DEVELOPMENT='1')
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
	qmp_path = os.path.join(lab.BUILD, f'qemu-qmp-cold-{target}.sock')
	log_path = os.path.join(lab.BUILD, f'tickless-{target}.log')
	runner_log = open(os.path.join(lab.BUILD, f'tickless-{target}-runner.log'), 'wb')
	kernel = os.path.join(lab.BUILD_ROOT, 'cargo', 'kernel', TRIPLES[target], 'debug', 'kernel')
	if os.path.exists(serial_path):
		os.unlink(serial_path)
	# THE COLD SCENARIO RUNNER'S GUEST, with the serial line on a socket this gate holds rather than in a
	# file: `server` without `nowait`, so QEMU waits for the connection and no boot byte is lost.
	env = dict(os.environ, LIBER_DEVELOPMENT='1', DEV_PROFILE='1', COLD='1', SERIAL=f'unix:{serial_path},server', SMP=os.environ.get('SMP', '4'), LIBER_RUN_MODE='development')
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
	if target != 'x86_64':
		serial.settle(2.0 * scale, 120 * scale)
		suspend_to_idle(target, serial, scale)
	return True


def main():
	args = sys.argv[1:]
	if args == ['--self-test']:
		clock_self_test()
		return 0
	skip_build = '--no-build' in args
	targets = [a for a in args if not a.startswith('--')] or list(TARGETS)
	for target in targets:
		if target not in TARGETS:
			print(f'check-tickless-idle: unknown target {target!r}', file=sys.stderr)
			return 2
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
