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

ARMED = re.compile(rb'console: typed input raises interrupt (\d+)')
POLLED = re.compile(rb'console: typed input is polled - ([^\r\n]*)')
CORE_ROW = re.compile(r'\{cpu=(\d+), idle-ns=(\d+), halts=(\d+), wakes-timer=(\d+), wakes-ipi=(\d+), wakes-housekeeping=(\d+), wakes-device=(\d+), sources=\[([^\]]*)\]\}')
SOURCE = re.compile(r'\{source=(\d+), count=(\d+)\}')
ARRIVED = b'a device arrived in the slot behind'


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

	# Read what has arrived, waiting at most `wait` seconds for the first byte; whether anything came.
	def pump(self, wait):
		ready, _, _ = select.select([self.sock], [], [], wait)
		if not ready:
			return False
		chunk = self.sock.recv(65536)
		if not chunk:
			raise GateError('the guest closed its serial line')
		self.data += chunk
		self.log.write(chunk)
		self.log.flush()
		return True

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
	def wait_prompt(self, mark, timeout, what):
		started = time.monotonic()
		settle = lab.PROMPT_SETTLE * self.scale
		while True:
			if lab.PROMPT.search(self.text_since(mark)):
				if not self.pump(settle):
					return
				continue
			left = started + timeout - time.monotonic()
			if left <= 0:
				raise GateError(f'{what}: no shell prompt within {timeout:.0f} s')
			self.pump(min(left, 0.2))

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
	env = dict(os.environ, LIBER_DEVELOPMENT='1', STRIP='none')
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
	env = dict(os.environ, LIBER_DEVELOPMENT='1', STRIP='none', DEV_PROFILE='1', COLD='1', SERIAL=f'unix:{serial_path},server', SMP=os.environ.get('SMP', '4'), LIBER_RUN_MODE='development')
	if target != 'x86_64':
		env['UEFI'] = '1'
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
	# AN IDLE MACHINE, which is the claim: the boot's own output has stopped before anything is typed.
	serial.settle(2.0 * scale, 120 * scale)
	before = cores(serial, 60 * scale)
	latencies = []
	for burst in range(BURSTS):
		# Idle again before each burst, so each one lands on a halted boot processor.
		serial.settle(1.0 * scale, 30 * scale)
		text = f'echo tickless-{burst}'
		mark = len(serial.data)
		written = time.monotonic()
		serial.type(text.encode())
		serial.wait_for(mark, lambda seen, want=text.encode(): want in seen, 30 * scale, f'the echo of {text!r}')
		latency = time.monotonic() - written
		latencies.append(latency)
		if latency > ECHO_BOUND:
			raise GateError(f'{text!r} was echoed {latency * 1000:.0f} ms after the host wrote it, past the {ECHO_BOUND * 1000:.0f} ms bound')
		mark = len(serial.data)
		serial.type(b'\n')
		serial.wait_prompt(mark, 30 * scale, f'`{text}`')
	after = cores(serial, 60 * scale)
	raised = wakes_on(after, identity) - wakes_on(before, identity)
	if raised < BURSTS:
		raise GateError(f'{BURSTS} typed bursts woke the idle machine {raised} time(s) on interrupt {identity}: typed input is being found by a poll, not raised by the UART')
	echoes = ', '.join(f'{latency * 1000:.0f}' for latency in latencies)
	note(f'{target}: {BURSTS} typed bursts echoed in {echoes} ms, and woke the idle machine {raised} time(s) on the UART\'s interrupt {identity}')
	# THE IDLE RATE, from the same records: each core's wakes over a quiet interval. The `graph` read that
	# closes the interval wakes the machine too and is counted in, so the rate is an upper bound.
	serial.settle(2.0 * scale, 30 * scale)
	first = cores(serial, 60 * scale)
	started = time.monotonic()
	while time.monotonic() - started < IDLE_SAMPLE:
		serial.pump(max(0.0, started + IDLE_SAMPLE - time.monotonic()))
	last = cores(serial, 60 * scale)
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
	return True


def main():
	args = sys.argv[1:]
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
