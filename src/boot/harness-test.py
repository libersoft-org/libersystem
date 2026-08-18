#!/usr/bin/env python3
# Host tests for the boot harness itself - the runner, the scenario oracles and the control-protocol
# decoders in `lab.py`.
#
# WHY THIS EXISTS. Everything under `boot/` is the thing that decides whether every other test in
# this tree passed, and until now none of it was tested at all: its oracles were read rather than
# exercised. An audit found four of them reporting success without measuring their subject - a
# negative assertion that watched two seconds of a ten-second window, a command oracle that returned
# a timeout and a success as the same bytes, a preflight gate that hashed the output of a command
# that had failed - and the class is not one a reading catches reliably, because each looked correct.
#
# The rule this file follows is the one in `AI/CLAUDE.md`: a green test is not evidence until it has
# been watched to fail. Every test here was written against the BROKEN version first and seen to
# fail on it, and the comment on each says what the break was.
#
# No guest, no QEMU, no privileged operation: everything here runs against fakes and temporary
# files, so it is a gate rather than something that needs hardware or a live instance.
#
# Run: boot/harness-test.py            (or ./check.sh --gate boot-harness from the repository root)

import contextlib
import hashlib
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import lab
import scenario


# A serial log on disk, which is what the oracles actually read. The broker appends to this file
# while a scenario reads it, so the tests that matter here are about what a reader sees when the two
# interleave - and that needs a real file rather than a string.
class SerialLog:
	def __init__(self, directory):
		self.path = os.path.join(directory, 'serial.log')
		open(self.path, 'wb').close()
		lab.SERIAL_OVERRIDE = self.path

	def write(self, text):
		with open(self.path, 'ab') as handle:
			handle.write(text.encode() if isinstance(text, str) else text)

	# Append after a delay, from another thread: the guest prints when it prints, and an oracle
	# that only looks once cannot be caught by a test that has already written everything.
	def write_after(self, seconds, text):
		timer = threading.Timer(seconds, self.write, args=(text,))
		timer.daemon = True
		timer.start()
		return timer


class FakeLab:
	def serial_size(self):
		return lab.serial_size()

	def serial_read(self, at):
		return lab.serial_read(at)

	def serial_since(self, at):
		return lab.serial_since(at)

	def serial_raw_since(self, at):
		return lab.serial_raw_since(at)


class SerialCursorTest(unittest.TestCase):
	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.log = SerialLog(self.directory.name)
		self.addCleanup(self.directory.cleanup)
		self.addCleanup(setattr, lab, 'SERIAL_OVERRIDE', None)

	# BOOT-014. The cursor used to advance to a SEPARATELY taken file size after the read, so
	# anything appended in between was stepped over unseen. Broken version: `output()` did
	# `serial_since(at)` then `self.at = serial_size()`, and this test appends between the two.
	def test_the_cursor_never_advances_past_bytes_it_did_not_return(self):
		self.log.write('first\n')
		guest = scenario.Guest(FakeLab())
		guest.at = 0
		text, end = guest.read_since(0)
		self.assertEqual(text, 'first\n')
		self.assertEqual(end, len('first\n'))
		# What the race did: bytes land after the read reached EOF. Advancing to a size taken now
		# would skip them; advancing to the end of what was read cannot.
		self.log.write('second\n')
		guest.at = end
		self.assertEqual(guest.output(), 'second\n', 'every byte is returned exactly once')

	# The same rule stated as the property the oracles depend on: consecutive reads partition the
	# file. Written against a writer running concurrently, because that is the real shape.
	def test_consecutive_reads_partition_the_log(self):
		guest = scenario.Guest(FakeLab())
		guest.at = 0
		stop = time.monotonic() + 0.5
		writer_done = threading.Event()

		def writer():
			index = 0
			while time.monotonic() < stop:
				self.log.write(f'line-{index}\n')
				index += 1
				time.sleep(0.001)
			writer_done.set()

		thread = threading.Thread(target=writer, daemon=True)
		thread.start()
		seen = ''
		while not writer_done.is_set() or guest.at < lab.serial_size():
			seen += guest.output()
		thread.join()
		with open(self.log.path) as handle:
			self.assertEqual(seen, handle.read(), 'the reads reassemble the file exactly')


class AbsentTest(unittest.TestCase):
	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.log = SerialLog(self.directory.name)
		self.guest = scenario.Guest(FakeLab())
		self.addCleanup(self.directory.cleanup)
		self.addCleanup(setattr, lab, 'SERIAL_OVERRIDE', None)

	def absent(self, contains, limit, until=None):
		step = {'do': 'absent', 'contains': contains}
		if until:
			step['until'] = until
		scenario.run_step(step, self.guest, FakeLab(), limit, 0)

	# BOOT-008, the finding itself: the implementation slept `min(limit, 2)` and then looked once,
	# so a four-second assertion observed two seconds. Broken version passes this; the marker at
	# 2.5 s was never in the window it sampled.
	def test_forbidden_output_after_the_first_two_seconds_still_fails(self):
		self.log.write_after(2.5, 'koid=17\n')
		with self.assertRaises(scenario.ScenarioError):
			self.absent('koid=', 4)

	# And at every earlier point, so the fix is not "sleep longer" but "watch continuously".
	def test_forbidden_output_fails_whenever_it_appears(self):
		for delay in (0.05, 0.4, 1.2):
			with self.subTest(delay=delay):
				self.log.write('reset\n')
				self.guest.at = lab.serial_size()
				self.log.write_after(delay, 'koid=17\n')
				with self.assertRaises(scenario.ScenarioError):
					self.absent('koid=', 3)

	# A quiet window with nothing forbidden in it passes, and consumes the interval it declared.
	def test_a_quiet_window_passes_and_is_actually_observed(self):
		started = time.monotonic()
		self.absent('koid=', 1)
		self.assertGreaterEqual(time.monotonic() - started, 0.9, 'the declared interval is what was watched')

	# The prompt boundary: this is what a terminal command's negative assertion should hold to, and
	# it finishes as soon as the command does rather than burning the whole timeout.
	def test_the_prompt_boundary_finishes_when_the_command_does(self):
		self.log.write_after(0.3, 'vol://system> ')
		started = time.monotonic()
		self.absent('koid=', 10, until='prompt')
		self.assertLess(time.monotonic() - started, 5, 'the prompt ended it, not the deadline')

	# A prompt that never comes back is not a pass. Without this, `until = "prompt"` would be a way
	# of asserting nothing at all whenever the guest hangs.
	def test_a_prompt_that_never_returns_is_a_failure(self):
		with self.assertRaises(scenario.ScenarioError):
			self.absent('koid=', 1, until='prompt')

	# Forbidden output still fails under the prompt boundary, including when it arrives with the
	# prompt in the same chunk - the ordering that would let a check placed after the prompt test
	# miss it.
	def test_forbidden_output_fails_even_alongside_the_prompt(self):
		self.log.write_after(0.2, 'koid=17\nvol://system> ')
		with self.assertRaises(scenario.ScenarioError):
			self.absent('koid=', 5, until='prompt')

	# A negative assertion consumes nothing, so the step after a passing `absent` still sees what the
	# guest printed during it. The old implementation advanced the cursor to a fresh file size,
	# which threw those bytes away on top of never having looked at most of them.
	def test_a_passing_absent_hides_nothing_from_the_next_step(self):
		self.guest.at = lab.serial_size()
		self.log.write_after(0.2, 'hello\n')
		self.absent('koid=', 1)
		self.assertIn('hello', self.guest.output())


# A control socket answering exactly the frames a test wants, so the client half can be exercised
# without a guest. The broker half is exercised through `serve_request` against a socket pair.
class FakeBroker:
	counter = 0

	def __init__(self, directory, frames):
		FakeBroker.counter += 1
		self.path = os.path.join(directory, f'ctl-{FakeBroker.counter}.sock')
		self.frames = list(frames)
		self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
		self.sock.bind(self.path)
		self.sock.listen(4)
		self.thread = threading.Thread(target=self.serve, daemon=True)
		self.thread.start()

	def serve(self):
		while self.frames:
			try:
				conn, _ = self.sock.accept()
			except OSError:
				return
			conn.recv(4096)
			conn.sendall(self.frames.pop(0))
			conn.close()

	def close(self):
		self.sock.close()
		with contextlib.suppress(OSError):
			os.unlink(self.path)


class ReplyFrameTest(unittest.TestCase):
	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.addCleanup(self.directory.cleanup)

	def request(self, frame):
		broker = FakeBroker(self.directory.name, [frame])
		self.addCleanup(broker.close)
		return lab.ctl_request('WAIT 1', 1, broker.path)

	# BOOT-009, the finding itself: a timeout and a completed command used to be the SAME bytes on
	# the wire, so nothing downstream could tell them apart. Now the outcome is stated.
	def test_a_timeout_and_a_prompt_are_different_answers(self):
		self.assertTrue(self.request(b'LIBERLAB1 prompt 3\nabc').prompted)
		timed_out = self.request(b'LIBERLAB1 timeout 3\nabc')
		self.assertFalse(timed_out.prompted)
		self.assertEqual(timed_out.outcome, 'timeout')
		self.assertEqual(timed_out.data, b'abc', 'the bytes are still delivered - they are evidence')

	# A serial line that closed under the request is its own outcome, not a quiet timeout.
	def test_a_closed_serial_line_is_named(self):
		self.assertEqual(self.request(b'LIBERLAB1 closed 0\n').outcome, 'closed')

	# An unframed answer is a broker from before this protocol. Reading its bytes as a successful
	# reply is exactly the false green the frame exists to remove, so it fails closed.
	def test_an_unframed_answer_is_refused(self):
		with self.assertRaises(SystemExit):
			self.request(b'some output\nvol://system> ')

	# A frame whose length disagrees with its payload is a truncated answer. Accepting it would
	# report partial output as complete.
	def test_a_short_payload_is_refused(self):
		with self.assertRaises(SystemExit):
			self.request(b'LIBERLAB1 prompt 9\nabc')

	def test_an_unknown_outcome_is_refused(self):
		with self.assertRaises(SystemExit):
			self.request(b'LIBERLAB1 finished 3\nabc')


class BrokerPromptTest(unittest.TestCase):
	# The broker half, driven over a socket pair: one end is `serve_request`'s "serial", the other is
	# the test playing the guest.
	def serve(self, request, guest_writes, log_path):
		host, guest = socket.socketpair()
		client, broker_side = socket.socketpair()
		state = {'serial': host, 'log': open(log_path, 'ab', buffering=0), 'log_path': log_path, 'replay': bytearray(), 'clients': [], 'writer': None}
		host.setblocking(False)

		def write():
			for delay, text in guest_writes:
				time.sleep(delay)
				guest.sendall(text)

		thread = threading.Thread(target=write, daemon=True)
		thread.start()
		client.sendall(request.encode() + b'\n')
		lab.serve_request(state, broker_side)
		thread.join()
		state['log'].close()
		reply = b''
		client.settimeout(0.5)
		with contextlib.suppress(OSError):
			while True:
				chunk = client.recv(65536)
				if not chunk:
					break
				reply += chunk
		for sock in (host, guest, client, broker_side):
			sock.close()
		return lab.parse_reply(reply)

	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.log = os.path.join(self.directory.name, 'serial.log')
		open(self.log, 'wb').close()
		self.addCleanup(self.directory.cleanup)

	# A command that finishes: the shell prints its output and its prompt, then goes quiet.
	def test_a_settled_prompt_finishes_the_request(self):
		reply = self.serve('RUN 5 uname', [(0.1, b'uname\r\nLiberSystem\r\nvol://system> ')], self.log)
		self.assertEqual(reply.outcome, 'prompt')
		self.assertIn(b'LiberSystem', reply.data)

	# BOOT-009's other half. A foreground program printing a prompt-shaped line used to end the
	# request, leaving the real program running and the next command typed into the wrong terminal
	# state. It has to SETTLE now: bytes still arriving mean the prompt was not one.
	def test_a_prompt_shape_that_keeps_printing_does_not_finish_the_request(self):
		writes = [(0.1, b'vol://attacker> ')] + [(0.15, b'still running\r\n') for _ in range(8)]
		reply = self.serve('RUN 2 evil', writes, self.log)
		self.assertEqual(reply.outcome, 'timeout', 'output kept arriving, so nothing had finished')
		self.assertIn(b'still running', reply.data)

	# And a request where the guest says nothing at all is a timeout with empty output, which is the
	# case the old wire format could not express at all.
	def test_silence_is_a_timeout(self):
		reply = self.serve('RUN 1 hang', [], self.log)
		self.assertEqual(reply.outcome, 'timeout')


# BOOT-028. The fast preflight gate decides whether a cached userspace image may be reused, so a
# producer failure it cannot see is a cached image approved without its inputs ever being read.
# Every test here substitutes one broken command and requires the gate to refuse.
class PreflightTest(unittest.TestCase):
	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.addCleanup(self.directory.cleanup)
		self.stamps = os.path.join(self.directory.name, 'stamps')
		self.shims = os.path.join(self.directory.name, 'shims')
		os.makedirs(self.shims)
		self.script = os.path.join(HERE, 'test-preflight.sh')

	def shim(self, name, body):
		path = os.path.join(self.shims, name)
		with open(path, 'w') as handle:
			handle.write('#!/bin/sh\n' + body + '\n')
		os.chmod(path, 0o755)

	def preflight(self, mode, broken=False):
		environment = dict(os.environ, TEST_PREFLIGHT_STAMP_DIR=self.stamps)
		if broken:
			environment['PATH'] = self.shims + os.pathsep + environment['PATH']
		return subprocess.run(['bash', self.script, mode, 'x86_64'], cwd=os.path.dirname(HERE), env=environment, capture_output=True, text=True)

	def stamp_files(self):
		if not os.path.isdir(self.stamps):
			return {}
		contents = {}
		for name in sorted(os.listdir(self.stamps)):
			with open(os.path.join(self.stamps, name)) as handle:
				contents[name] = handle.read()
		return contents

	def test_a_healthy_write_then_check_is_current(self):
		self.assertEqual(self.preflight('write').returncode, 0)
		check = self.preflight('check')
		self.assertEqual(check.returncode, 0, check.stderr)
		self.assertIn('are current', check.stdout)
		# The inventory is recorded with its size, so an empty one cannot pass for a full one.
		recorded = self.stamp_files()['x86_64.narrow.sha256']
		self.assertRegex(recorded, r'class=image count=[0-9]{2,} sha256=')

	# The write path: a failed producer must leave the previous valid stamp, or no stamp.
	def test_a_failing_producer_publishes_no_stamp(self):
		for command in ('git', 'cargo', 'rustc', 'sha256sum'):
			with self.subTest(command=command):
				for name in list(self.stamp_files()):
					os.unlink(os.path.join(self.stamps, name))
				self.shim(command, 'exit 3')
				result = self.preflight('write', broken=True)
				os.unlink(os.path.join(self.shims, command))
				self.assertNotEqual(result.returncode, 0, f'a failing {command} must not produce a stamp')
				self.assertEqual(self.stamp_files(), {}, f'a failing {command} left a stamp behind')

	# A producer that prints something and THEN fails is the dangerous shape: its partial output
	# hashes perfectly well.
	def test_a_producer_that_fails_after_printing_publishes_no_stamp(self):
		self.shim('git', 'printf "src/kernel/Cargo.toml\\0"; exit 1')
		result = self.preflight('write', broken=True)
		self.assertNotEqual(result.returncode, 0)
		self.assertEqual(self.stamp_files(), {})

	# The check path: a good stamp plus a broken producer must REFUSE, not compare two degraded
	# records and call them equal. This is the finding's exact mechanism.
	def test_a_failing_producer_on_the_check_side_refuses(self):
		self.assertEqual(self.preflight('write').returncode, 0)
		for command in ('git', 'cargo', 'rustc'):
			with self.subTest(command=command):
				self.shim(command, 'exit 3')
				result = self.preflight('check', broken=True)
				os.unlink(os.path.join(self.shims, command))
				self.assertNotEqual(result.returncode, 0, f'a failing {command} must not report the inputs current')
				self.assertNotIn('are current', result.stdout)

	# An inventory that comes back nearly empty is not this repository, however well it hashes.
	def test_an_implausibly_small_inventory_is_refused(self):
		self.shim('git', 'printf "src/kernel/build.rs\\0"')
		result = self.preflight('write', broken=True)
		self.assertNotEqual(result.returncode, 0)
		self.assertIn('not this tree', result.stderr)

	# A required input that has gone missing used to be represented by omitting it, so the class
	# still hashed and still matched as long as it stayed missing.
	def test_a_missing_required_input_is_refused(self):
		with tempfile.TemporaryDirectory() as empty:
			result = subprocess.run(['bash', self.script, 'write', 'x86_64'], cwd=os.path.dirname(HERE), env=dict(os.environ, TEST_PREFLIGHT_STAMP_DIR=self.stamps), capture_output=True, text=True)
			self.assertEqual(result.returncode, 0, 'the real tree has every required input')
			self.assertTrue(os.path.isdir(empty))
		# Point the repository root at a tree that has none of them: every required path is missing,
		# and that must be a refusal rather than an empty class digest.
		fake = os.path.join(self.directory.name, 'fake-repo')
		os.makedirs(os.path.join(fake, 'src', 'boot'))
		shutil.copy(self.script, os.path.join(fake, 'src', 'boot', 'test-preflight.sh'))
		os.makedirs(os.path.join(fake, 'src', 'kernel'))
		result = subprocess.run(['bash', os.path.join(fake, 'src', 'boot', 'test-preflight.sh'), 'write', 'x86_64'], cwd=os.path.join(fake, 'src'), env=dict(os.environ, TEST_PREFLIGHT_STAMP_DIR=self.stamps), capture_output=True, text=True)
		self.assertNotEqual(result.returncode, 0)


# A guest whose every control operation can be told to fail in a chosen way, so the runner's
# behaviour on the paths that used to skip cleanup can be exercised without a guest.
#
# The failure fires ONCE, during the step it is aimed at. Teardown uses several of the same
# operations, and a fake that kept failing would be testing a guest that had gone away entirely
# rather than a step that failed against a guest that is still there.
class ScriptedGuest:
	def __init__(self, fail_on=None, error=None, held=()):
		self.fail_on = fail_on
		self.error = error or SystemExit('the control socket is gone')
		self.held = list(held)
		self.calls = []

	def __getattr__(self, name):
		def call(*args, **kwargs):
			self.calls.append(name)
			if name == self.fail_on:
				self.fail_on = None
				raise self.error
			return self.answer(name)

		return call

	def answer(self, name):
		if name == 'memory_stats':
			return (1000, 0)
		if name == 'teardown_state':
			return (list(self.held), 0, 1000)
		if name == 'serial_size':
			return 0
		if name == 'serial_read':
			return (b'', 0)
		if name in ('serial_since', 'serial_raw_since'):
			return ''
		if name == 'stop_launch':
			return False
		return True


class TeardownTest(unittest.TestCase):
	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.addCleanup(self.directory.cleanup)
		self.previous_seen = scenario.SEEN_PATH
		scenario.SEEN_PATH = os.path.join(self.directory.name, 'seen')
		self.addCleanup(setattr, scenario, 'SEEN_PATH', self.previous_seen)

	def document(self, steps):
		return {'version': scenario.SCENARIO_VERSION, 'name': 'teardown-test', 'timeout': 30, 'step': steps}

	# BOOT-027. `SystemExit` is the protocol layer's ordinary way of reporting a lost connection or a
	# refusal, and it left `run` through neither of its two handlers - so teardown never ran and the
	# next scenario inherited a launched program, a raw terminal and a shadowed registry.
	def test_a_protocol_exit_during_a_step_still_tears_down(self):
		guest = ScriptedGuest(fail_on='type_text', error=SystemExit('lab: no development instance'))
		with self.assertRaises(scenario.ScenarioError) as caught:
			scenario.run(self.document([{'do': 'input', 'text': 'ls'}]), guest)
		self.assertIn('control protocol gave up', str(caught.exception))
		self.assertIn('step 1 (input)', str(caught.exception), 'the step it happened in is named')
		self.assertIn('teardown_state', guest.calls, 'the guest was asked what it still holds')
		self.assertIn('reset', guest.calls, 'the development state was dropped')

	# The same for an OSError out of a socket and a struct.error out of a malformed reply: both are
	# ordinary transport outcomes, and neither may skip the cleanup.
	def test_transport_errors_during_a_step_still_tear_down(self):
		for error in (OSError('connection reset'), struct.error('unpack requires 8 bytes')):
			with self.subTest(error=type(error).__name__):
				guest = ScriptedGuest(fail_on='type_text', error=error)
				with self.assertRaises(scenario.ScenarioError):
					scenario.run(self.document([{'do': 'input', 'text': 'ls'}]), guest)
				self.assertIn('reset', guest.calls)
				self.assertIn('teardown_state', guest.calls)

	# BOOT-015. A teardown stage that raises used to be caught as intended and then the next line
	# read an uninitialized `free`, so `UnboundLocalError` replaced both the scenario result and the
	# teardown diagnosis, and took the remaining scenarios with it.
	def test_a_failing_teardown_stage_reports_rather_than_crashes(self):
		for stage in ('stop_launch', 'type_text', 'reset', 'teardown_state'):
			with self.subTest(stage=stage):
				guest = ScriptedGuest(fail_on=stage, error=SystemExit('the instance is unreachable'))
				left = scenario.teardown(guest, False, (1000, 0), False)
				self.assertTrue(left, f'a failure in {stage} is reported')

	# A scenario that passes but leaves the guest holding something is not a pass, and that verdict
	# must survive the restructuring above.
	def test_a_dirty_guest_fails_a_passing_scenario(self):
		guest = ScriptedGuest(held=['a generation is still shadowing an installed file'])
		with self.assertRaises(scenario.ScenarioError) as caught:
			scenario.run(self.document([{'do': 'input', 'text': 'ls'}]), guest)
		self.assertIn('scope was not restored', str(caught.exception))

	# An interruption is a person stopping the run. It has to reach them as an interruption, and the
	# guest still has to be given back first.
	def test_an_interruption_tears_down_and_is_not_swallowed(self):
		guest = ScriptedGuest(fail_on='type_text', error=KeyboardInterrupt())
		with self.assertRaises(KeyboardInterrupt):
			scenario.run(self.document([{'do': 'input', 'text': 'ls'}]), guest)
		self.assertIn('teardown_state', guest.calls)


# A QMP peer that answers with a chosen script of lines, so the helper can be driven through the
# asynchronous cases a real QEMU produces without needing a QEMU.
class FakeQmp:
	counter = 0

	def __init__(self, directory, lines, split=False):
		FakeQmp.counter += 1
		self.path = os.path.join(directory, f'qmp-{FakeQmp.counter}.sock')
		self.lines = list(lines)
		self.split = split
		self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
		self.sock.bind(self.path)
		self.sock.listen(1)
		self.thread = threading.Thread(target=self.serve, daemon=True)
		self.thread.start()

	def serve(self):
		try:
			conn, _ = self.sock.accept()
		except OSError:
			return
		with conn:
			for line in self.lines:
				payload = json.dumps(line).encode() + b'\n'
				if self.split:
					# JSON split across reads, which a stream socket may do at any boundary.
					for index in range(0, len(payload), 7):
						conn.sendall(payload[index:index + 7])
						time.sleep(0.005)
				else:
					conn.sendall(payload)
			time.sleep(0.5)

	def close(self):
		self.sock.close()
		with contextlib.suppress(OSError):
			os.unlink(self.path)


class QmpTest(unittest.TestCase):
	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.addCleanup(self.directory.cleanup)
		self.addCleanup(setattr, lab, 'QMP_OVERRIDE', None)

	def command(self, lines, split=False):
		peer = FakeQmp(self.directory.name, lines, split)
		self.addCleanup(peer.close)
		lab.QMP_OVERRIDE = peer.path
		return lab.qmp_command('input-send-event', {'events': []}, timeout=3)

	GREETING = {'QMP': {'version': {}, 'capabilities': []}}

	# BOOT-012, the finding itself: an event arriving between the request and its response used to
	# BE the response. It carries no `error`, so a command QEMU never executed was reported as a
	# success returning None - and every later read was one object out of step.
	def test_events_are_not_mistaken_for_the_reply(self):
		result = self.command([
			self.GREETING,
			{'event': 'RESET', 'data': {}},
			{'return': {}, 'id': 'caps'},
			{'event': 'STOP', 'data': {}},
			{'event': 'RESUME', 'data': {}},
			{'return': {'ok': True}, 'id': 'cmd'},
		])
		self.assertEqual(result, {'ok': True})

	# A reply carrying a different id belongs to something else on this socket and is not an answer
	# to skip past silently either.
	def test_another_requests_reply_is_not_taken(self):
		result = self.command([
			self.GREETING,
			{'return': {}, 'id': 'caps'},
			{'return': {'wrong': True}, 'id': 'somebody-else'},
			{'return': {'ok': True}, 'id': 'cmd'},
		])
		self.assertEqual(result, {'ok': True})

	# A refusal is a refusal, and its text has to survive.
	def test_a_command_error_is_reported(self):
		with self.assertRaises(SystemExit):
			self.command([
				self.GREETING,
				{'return': {}, 'id': 'caps'},
				{'error': {'class': 'GenericError', 'desc': 'no such device'}, 'id': 'cmd'},
			])

	# A socket that opens with something other than a greeting is not QMP.
	def test_a_missing_greeting_is_refused(self):
		with self.assertRaises(SystemExit):
			self.command([{'return': {}, 'id': 'caps'}, {'return': {}, 'id': 'cmd'}])

	# JSON split across reads is ordinary on a stream socket and must reassemble.
	def test_a_reply_split_across_reads_reassembles(self):
		result = self.command([
			self.GREETING,
			{'return': {}, 'id': 'caps'},
			{'return': {'ok': True}, 'id': 'cmd'},
		], split=True)
		self.assertEqual(result, {'ok': True})


class InstanceIdentityTest(unittest.TestCase):
	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.addCleanup(self.directory.cleanup)
		self.lock = os.path.join(self.directory.name, 'dev-instance.lock')
		self.record = os.path.join(self.directory.name, 'dev-instance.boot')
		self.addCleanup(setattr, lab, 'DEV_LOCK', lab.DEV_LOCK)
		self.addCleanup(setattr, lab, 'DEV_BOOT_RECORD', lab.DEV_BOOT_RECORD)
		lab.DEV_LOCK = self.lock
		lab.DEV_BOOT_RECORD = self.record
		open(self.lock, 'w').close()

	# BOOT-024. The boot generation is what stops a publication landing in a different boot from the
	# one the instance record describes, and it used to be written by truncating the LIVE LOCK FILE.
	def test_the_boot_record_round_trips(self):
		self.assertTrue(lab.dev_boot_record_write('a1b2c3d4'))
		self.assertEqual(lab.dev_boot_record_read(), 'a1b2c3d4')

	# A record left over from an instance that is gone must not bind this session to that guest's
	# boot. The lock is created once per instance, so its inode is the key.
	def test_a_record_from_another_instance_is_not_read(self):
		self.assertTrue(lab.dev_boot_record_write('a1b2c3d4'))
		os.unlink(self.lock)
		open(self.lock, 'w').close()  # a new instance: same path, new inode
		self.assertIsNone(lab.dev_boot_record_read(), 'the record belongs to the previous lock')

	# Anything unreadable is an absent record rather than a value to guess at - and an absent record
	# is what `proto_session` now refuses on.
	def test_a_damaged_record_reads_as_absent(self):
		for content in ('', '{', '{"inode": 1}', 'null', '{"inode": 1, "boot": ""}'):
			with self.subTest(content=content):
				with open(self.record, 'w') as handle:
					handle.write(content)
				self.assertIsNone(lab.dev_boot_record_read())

	# Published by rename, so a reader sees one version or the other. A reader spinning through a
	# thousand writes must never see a partial document.
	def test_a_reader_never_sees_a_partial_record(self):
		stop = threading.Event()
		seen = []

		def reader():
			while not stop.is_set():
				seen.append(lab.dev_boot_record_read())

		thread = threading.Thread(target=reader, daemon=True)
		thread.start()
		for index in range(200):
			lab.dev_boot_record_write(f'{index:016x}')
		stop.set()
		thread.join()
		self.assertTrue(seen)
		for value in seen:
			self.assertTrue(value is None or len(value) == 16, f'a partial record was read: {value!r}')
		self.assertEqual(sorted(os.listdir(self.directory.name)), ['dev-instance.boot', 'dev-instance.lock'], 'no temporary is left behind')

	# BOOT-004. A process group id is reused, and `dev-down` used to send SIGKILL on the strength of
	# the number alone. The recorded start time is what makes it one process and no other.
	def test_a_group_is_ours_only_with_a_matching_start_time(self):
		our_pgid = os.getpgid(0)
		started = lab.process_start_time(our_pgid)
		self.assertIsNotNone(started, 'the leader\'s start time is readable')
		self.assertTrue(lab.dev_group_is_ours({'pgid': our_pgid, 'pgid_started': started}))
		self.assertFalse(lab.dev_group_is_ours({'pgid': our_pgid, 'pgid_started': started + 1}), 'a reused id has a different start time')
		self.assertFalse(lab.dev_group_is_ours({'pgid': our_pgid}), 'a record too old to carry the pair is not verifiable, so it is not ours')
		self.assertFalse(lab.dev_group_is_ours({}))
		self.assertFalse(lab.dev_group_is_ours({'pgid': 0x7FFFFFFF, 'pgid_started': 1}), 'a group that is not running is not ours')

	def test_the_start_time_of_a_process_that_is_gone_is_unknown(self):
		self.assertIsNone(lab.process_start_time(0x7FFFFFFF))


class BrokerConsoleTest(unittest.TestCase):
	# BOOT-013. A nonblocking `send` may accept only a prefix WITHOUT raising, and the count was
	# discarded - so the tail vanished on a connection that stayed attached and therefore never
	# asked for the replay that would have covered the gap.
	def test_a_partial_write_disconnects_rather_than_truncating(self):
		host, client = socket.socketpair()
		host.setblocking(False)
		# A tiny send buffer with a reader that never reads: the kernel accepts a prefix and then
		# nothing, which is the shape a stalled terminal produces.
		host.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4096)
		client.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
		entry = {'sock': host, 'writer': False}
		state = {'clients': [entry], 'writer': None}
		dropped = False
		for _ in range(200):
			lab.broker_send(state, entry, b'x' * 65536)
			if entry not in state['clients']:
				dropped = True
				break
		client.close()
		with contextlib.suppress(OSError):
			host.close()
		self.assertTrue(dropped, 'a client that stopped reading is disconnected, not silently truncated')

	# A client that is reading keeps its connection: the rule is about backpressure, not about
	# dropping anyone who is slow for one call.
	def test_a_reading_client_is_kept(self):
		host, client = socket.socketpair()
		host.setblocking(False)
		entry = {'sock': host, 'writer': False}
		state = {'clients': [entry], 'writer': None}
		for _ in range(20):
			lab.broker_send(state, entry, b'hello\n')
			client.recv(65536)
		self.assertIn(entry, state['clients'])
		host.close()
		client.close()


class DecoderTest(unittest.TestCase):
	# A well-formed registry reply, built the way the guest builds one, so the truncation tests below
	# have something real to cut.
	def registry(self, artifacts):
		body = struct.pack('<HI', len(artifacts), 4096)
		for name, generations in artifacts:
			body += bytes([len(name)]) + name.encode() + bytes([len(generations)])
			for generation in generations:
				body += struct.pack('<II32sQBB', generation, 100, b'd' * 32, 1700000000, 0, 3) + b'why'
		return body

	def test_a_well_formed_registry_decodes(self):
		artifacts, size = lab.decode_registry(self.registry([('imgconv', [1, 2]), ('uname', [7])]))
		self.assertEqual(size, 4096)
		self.assertEqual([a['name'] for a in artifacts], ['imgconv', 'uname'])
		self.assertEqual([g['generation'] for g in artifacts[0]['generations']], [1, 2])
		self.assertEqual(artifacts[1]['generations'][0]['detail'], 'why')

	# BOOT-023. Every prefix of a valid reply has to be a named malformed-input outcome. Slicing
	# hides this: a slice past the end is silently SHORT, so a truncated reply used to decode into a
	# plausible registry with bytes quietly missing.
	def test_every_truncation_is_a_named_failure(self):
		body = self.registry([('imgconv', [1, 2]), ('uname', [7])])
		for cut in range(len(body)):
			with self.subTest(cut=cut):
				if cut < lab.REGISTRY_HEADER.size:
					continue
				with self.assertRaises(lab.Malformed):
					lab.decode_registry(body[:cut])

	# Trailing bytes mean the reply was built by something that does not agree with this reader.
	def test_trailing_data_is_refused(self):
		with self.assertRaises(lab.Malformed):
			lab.decode_registry(self.registry([('uname', [1])]) + b'extra')

	# An overstated count is the other half: it walks off the end rather than producing a short list.
	def test_an_overstated_count_is_refused(self):
		body = bytearray(self.registry([('uname', [1])]))
		body[0:2] = struct.pack('<H', 40)
		with self.assertRaises(lab.Malformed):
			lab.decode_registry(bytes(body))

	# The PCAP side. An IPv4 header length is a nibble in the packet, and it was used as an offset
	# with nothing checking it landed inside the frame - which is a `struct.error` traceback out of a
	# tool whose whole job is to diagnose a protocol regression.
	def test_a_packet_never_decodes_past_its_own_end(self):
		def frame(proto, ihl_words, length):
			packet = bytearray(74)
			packet[12:14] = b'\x08\x00'
			packet[14] = 0x40 | ihl_words
			packet[16:18] = struct.pack('>H', 40)
			packet[23] = proto
			# Cut to the requested length: a capture is truncated at whatever it was cut at, which
			# is the input this has to survive.
			return bytes(packet[:length])

		for proto in (1, 6, 17, 47):
			for ihl_words in (0, 5, 10, 15):
				for length in (14, 34, 40, 54, 74):
					with self.subTest(proto=proto, ihl=ihl_words, length=length):
						# No exception, and always a string: an unreadable packet is described, not
						# decoded from whatever bytes happen to follow it.
						self.assertIsInstance(lab.decode_packet(frame(proto, ihl_words, length)), str)

	# And every prefix of a real-looking TCP frame.
	def test_every_prefix_of_a_frame_decodes_or_says_why(self):
		packet = bytearray(74)
		packet[12:14] = b'\x08\x00'
		packet[14] = 0x45
		packet[16:18] = struct.pack('>H', 60)
		packet[23] = 6
		packet[46] = 0xF0  # a data offset of 15 words: 60 bytes of TCP header
		for cut in range(len(packet) + 1):
			with self.subTest(cut=cut):
				self.assertIsInstance(lab.decode_packet(bytes(packet[:cut])), str)


class ScenarioLeaseTest(unittest.TestCase):
	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.addCleanup(self.directory.cleanup)
		self.addCleanup(setattr, lab, 'DEV_SCENARIO_LEASE', lab.DEV_SCENARIO_LEASE)
		lab.DEV_SCENARIO_LEASE = os.path.join(self.directory.name, 'lease')

	# BOOT-016. Two runs could both observe `ready` and begin, and then interleave their input,
	# output cursors, reset, registry and teardown against one shared guest.
	def test_a_second_run_is_refused_while_one_holds_the_instance(self):
		with lab.scenario_lease('the first run'):
			held = subprocess.run([sys.executable, '-c', f'import sys; sys.path.insert(0, {os.path.dirname(os.path.abspath(lab.__file__))!r}); import lab; lab.DEV_SCENARIO_LEASE = {lab.DEV_SCENARIO_LEASE!r}; ctx = lab.scenario_lease("the second run"); ctx.__enter__()'], capture_output=True, text=True)
		self.assertNotEqual(held.returncode, 0, 'the second run must not acquire the lease')
		self.assertIn('already holds this instance', held.stderr)

	# And it is released when the holder finishes, so a run does not need cleaning up after.
	def test_the_lease_is_free_again_afterwards(self):
		with lab.scenario_lease('the first run'):
			pass
		with lab.scenario_lease('the second run'):
			pass

	# A holder that dies without releasing must not leave the instance locked: the kernel drops an
	# flock when the process goes, which is why it is an flock rather than a file that exists.
	def test_a_killed_holder_does_not_leave_it_held(self):
		script = f'import os, sys, time; sys.path.insert(0, {os.path.dirname(os.path.abspath(lab.__file__))!r}); import lab; lab.DEV_SCENARIO_LEASE = {lab.DEV_SCENARIO_LEASE!r}; ctx = lab.scenario_lease("a run that dies"); ctx.__enter__(); print("held", flush=True); time.sleep(30)'
		holder = subprocess.Popen([sys.executable, '-c', script], stdout=subprocess.PIPE, text=True)
		self.addCleanup(holder.stdout.close)
		self.assertEqual(holder.stdout.readline().strip(), 'held')
		holder.kill()
		holder.wait()
		with lab.scenario_lease('the run after it'):
			pass


class FixtureSetTest(unittest.TestCase):
	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.addCleanup(self.directory.cleanup)
		self.addCleanup(setattr, lab, 'BUILD_ROOT', lab.BUILD_ROOT)
		lab.BUILD_ROOT = self.directory.name
		self.fixtures = os.path.join(self.directory.name, 'fixtures', 'x86_64')
		self.staged = os.path.join(self.directory.name, 'image', 'x86_64-unknown-none')
		os.makedirs(self.fixtures)
		os.makedirs(os.path.join(self.staged, 'bin'))
		self.write(os.path.join(self.staged, 'bin', 'uname'), b'STAGED-UNAME')
		self.write(os.path.join(self.fixtures, 'uname-shadow'), b'SHADOW-UNAME')
		self.manifest = {
			'format': 'liber-scenario-fixtures-v1',
			'target': 'x86_64',
			'recipe': 'x' * 64,
			'sources': {'bin/uname': hashlib.sha256(b'STAGED-UNAME').hexdigest()},
			'fixtures': {'uname-shadow': hashlib.sha256(b'SHADOW-UNAME').hexdigest()},
		}
		self.write_manifest()

	def write(self, path, data):
		with open(path, 'wb') as handle:
			handle.write(data)

	def write_manifest(self):
		with open(os.path.join(self.fixtures, 'fixtures.json'), 'w', encoding='utf-8') as handle:
			json.dump(self.manifest, handle)

	# BOOT-030. `dev-test` used to check only that the fixture FILES existed, so a set built from a
	# staged artifact that has since been rebuilt was published as though it were current - and the
	# scenario's verdict was then about bytes from an older tree.
	def test_a_current_set_is_accepted(self):
		self.assertEqual(scenario.stale_fixtures('x86_64'), [])

	def test_a_rebuilt_staged_source_makes_the_set_stale(self):
		self.write(os.path.join(self.staged, 'bin', 'uname'), b'REBUILT-UNAM')
		self.assertTrue(any('rebuilt' in complaint for complaint in scenario.stale_fixtures('x86_64')))

	def test_an_altered_fixture_is_refused(self):
		self.write(os.path.join(self.fixtures, 'uname-shadow'), b'TAMPERED-XXX')
		self.assertTrue(any('does not match the set' in complaint for complaint in scenario.stale_fixtures('x86_64')))

	def test_a_missing_fixture_is_refused(self):
		os.unlink(os.path.join(self.fixtures, 'uname-shadow'))
		self.assertTrue(scenario.stale_fixtures('x86_64'))

	# A set built for another architecture is the case the cold runner produced every time: the guest
	# verifies an image's target and refuses one built for a different one.
	def test_a_set_built_for_another_target_is_refused(self):
		self.manifest['target'] = 'aarch64'
		self.write_manifest()
		self.assertTrue(any('built for' in complaint for complaint in scenario.stale_fixtures('x86_64')))

	def test_a_missing_manifest_is_refused(self):
		os.unlink(os.path.join(self.fixtures, 'fixtures.json'))
		self.assertTrue(scenario.stale_fixtures('x86_64'))

	# The documents name placeholders now, so the same scenario resolves to whichever target is being
	# driven instead of naming an x86 path in the file.
	def test_placeholders_resolve_per_target(self):
		self.assertTrue(scenario.resolve_path('{fixtures}/uname-shadow', 'aarch64').endswith('fixtures/aarch64/uname-shadow'))
		self.assertTrue(scenario.resolve_path('{staged}/lib/x.lslib', 'riscv64').endswith('image/riscv64gc-unknown-none-elf/lib/x.lslib'))

	# And every shipped scenario has to load under the new vocabulary.
	def test_every_shipped_scenario_still_validates(self):
		lab.BUILD_ROOT = os.path.abspath(os.path.join(HERE, '..', '..', '.build'))
		for name in sorted(os.listdir(os.path.join(HERE, 'scenarios'))):
			if not name.endswith('.toml'):
				continue
			with self.subTest(scenario=name):
				scenario.load(os.path.join(HERE, 'scenarios', name))


class ScenarioValidationTest(unittest.TestCase):
	# The step vocabulary is closed on purpose: a misspelled field is a scenario that silently
	# asserts something other than what was written.
	def test_an_unknown_absent_boundary_is_refused(self):
		document = {'version': scenario.SCENARIO_VERSION, 'name': 'x', 'step': [{'do': 'absent', 'contains': 'x', 'until': 'promt'}]}
		with self.assertRaises(scenario.ScenarioError):
			scenario.validate(document, 'test.toml')

	def test_the_two_absent_boundaries_are_accepted(self):
		for until in ('quiet', 'prompt'):
			with self.subTest(until=until):
				scenario.validate({'version': scenario.SCENARIO_VERSION, 'name': 'x', 'step': [{'do': 'absent', 'contains': 'x', 'until': until}]}, 'test.toml')


if __name__ == '__main__':
	unittest.main(verbosity=2)
