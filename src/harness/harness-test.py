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
# Run: harness/harness-test.py            (or ./check.sh --gate boot-harness from the repository root)

import contextlib
import fcntl
import hashlib
import io
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
from unittest import mock

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


class ImageCommandTest(unittest.TestCase):
	# AND THE MODE THE MEDIUM IS SIGNED FOR, which both of these left out. A lab image is signed
	# `harness` - no DMA mode of its own - so the same image boots the translated machine and the
	# `--no-iommu` one, taking the mode from the harness carrier each time. The flag was added to
	# `image_command` and these two expectations were not, so this suite has been failing on it.
	def test_ordinary_images_use_the_production_strip_default(self):
		self.assertEqual(lab.image_command(), [lab.IMAGE_SH, '--format', 'iso', '--dma-mode', 'harness'])

	def test_development_images_request_the_complete_kernel(self):
		self.assertEqual(
			lab.image_command('none'),
			[lab.IMAGE_SH, '--format', 'iso', '--dma-mode', 'harness', '--strip', 'none'],
		)


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


class ColdPromptTest(unittest.TestCase):
	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.log = SerialLog(self.directory.name)
		self.addCleanup(self.directory.cleanup)
		self.addCleanup(setattr, lab, 'SERIAL_OVERRIDE', None)
		self.addCleanup(setattr, lab, 'LAST_INPUT_AT', 0)
		self.addCleanup(setattr, lab, 'BOOT_NUDGE_QUIET', lab.BOOT_NUDGE_QUIET)
		lab.BOOT_NUDGE_QUIET = 0.3
		self.typed = []
		original = lab.LabGuest.type_text

		def fake(guest, text, enter, timeout):
			lab.note_input()
			self.typed.append((text, enter))
			self.log.write('\r\n\x1b[1;32mvol://system> \x1b[0m')
			return True

		lab.LabGuest.type_text = fake
		self.addCleanup(setattr, lab.LabGuest, 'type_text', original)

	# A service's line after the shell's prompt - the watchdog service's first answered `alive` - left the prompt
	# not the last thing printed, and a cold run's first `prompt` step waited out its whole timeout on a shell that
	# was waiting. Broken version: the wait had no nudge, and this returned False.
	def test_a_prompt_the_guests_own_lines_buried_is_asked_for_again(self):
		self.log.write('shell attached\r\n\x1b[1;32mvol://system> \x1b[0mWatchdogService: ServiceManager answered\r\n')
		self.assertTrue(lab.LabGuest(5).wait_prompt(5), 'the prompt printed again answers the wait')
		self.assertEqual(self.typed, [('', True)], 'one empty line, and nothing else, was typed')

	# And never while a command the runner typed may still be running: output after the last input with no prompt
	# past it is the command's, and an Enter would reach the command.
	def test_output_past_the_last_input_is_never_nudged(self):
		self.log.write('\x1b[1;32mvol://system> \x1b[0m')
		lab.note_input()
		self.log.write('bmc sel\r\n1 entries, 2032 bytes free\r\n')
		self.assertFalse(lab.LabGuest(1).wait_prompt(1))
		self.assertEqual(self.typed, [], 'nothing was typed into a running command')


# A COLD RUN IS ITS OWN, AND BUILDS NOTHING WHEN WHAT IT BOOTS IS CURRENT. Two runs of one target get two state
# directories, and every name the guest is handed - the control channel, the serial log, the monitor and QMP, the host
# port - is in its own; a current staging starts no build; and under the runner a staging that is not current is
# refused, with a refusal's status, rather than built under the runs beside it. Watched failing against the runner as
# it stood: every name was `cold-x86_64` in `.build/boot`, and the staging was built on every run.
class ColdRunPrivacyTest(unittest.TestCase):
	SCENARIO = os.path.join(HERE, 'scenarios', 'launch-program.toml')

	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.addCleanup(self.directory.cleanup)
		for name in ('SEEN_PATH', 'TARGET', 'TIME_SCALE'):
			self.addCleanup(setattr, scenario, name, getattr(scenario, name))

	def run_cold(self, environ, stale=()):
		started, built = [], []

		class Exited:
			pid = 2 ** 22 + 7
			returncode = 1

			def poll(self):
				return 1

			def wait(self, timeout=None):
				return 1

		def popen(command, **kwargs):
			started.append(dict(kwargs.get('env') or {}))
			return Exited()

		def run_child(command, timeout, **kwargs):
			built.append(command)
			return subprocess.CompletedProcess(command, 0, b'', b'')

		environment = {name: value for name, value in os.environ.items() if name not in ('LIBER_DEV_STATE', 'LIBER_STAGING_RUNNER', 'HOSTFWD_PORT')}
		environment.update(environ)
		with mock.patch.dict(os.environ, environment, clear=True), mock.patch.object(lab, 'BUILD_ROOT', self.directory.name), mock.patch.object(lab, 'staging_not_current', lambda target, env: list(stale)), mock.patch.object(lab, 'run_child', run_child), mock.patch.object(lab.subprocess, 'Popen', popen), mock.patch.object(scenario, 'stale_fixtures', lambda target=None: []), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
			with self.assertRaises(SystemExit) as raised:
				lab.cmd_scenario_cold(['x86_64', self.SCENARIO])
		return raised.exception.code, started, built

	def test_two_runs_of_one_target_name_two_guests_and_build_nothing_on_a_current_staging(self):
		first, second = (os.path.join(self.directory.name, name) for name in ('first', 'second'))
		runs = [self.run_cold({'LIBER_DEV_STATE': state}) for state in (first, second)]
		for state, (_, started, built) in zip((first, second), runs):
			self.assertEqual(built, [], 'a current staging starts no build')
			self.assertEqual(len(started), 1, 'the guest was started')
			env = started[0]
			self.assertEqual(env['LIBER_DEV_STATE'], state, "the runner binds the guest's sockets in this run's directory")
			self.assertEqual(env['SERIAL'], f'file:{state}/cold-x86_64.log', "and the serial log is this run's")
			self.assertEqual(env['COLD'], '1')
			self.assertRegex(env['HOSTFWD_PORT'], r'^[0-9]+$')
			self.assertNotEqual(env['HOSTFWD_PORT'], '5556', 'the host port is not the shared one')
			self.assertTrue(os.path.exists(os.path.join(state, 'cold-x86_64-runner.log')), "the runner's own log is in this run's directory")

	def test_a_run_by_hand_makes_a_directory_of_its_own(self):
		_, started, _ = self.run_cold({})
		state = started[0]['LIBER_DEV_STATE']
		self.assertEqual(os.path.dirname(state), os.path.join(self.directory.name, 'tmp'))
		self.assertTrue(os.path.basename(state).startswith('cold-x86_64.'))

	def test_under_the_runner_a_stale_staging_is_refused_and_not_built(self):
		code, started, built = self.run_cold({'LIBER_DEV_STATE': os.path.join(self.directory.name, 'refused'), 'LIBER_STAGING_RUNNER': '1'}, stale=["the x86_64 staging's 'libs' is not built from these sources"])
		self.assertEqual(code, lab.STATE_REFUSED)
		self.assertEqual(built, [], 'nothing is built under the runs beside it')
		self.assertEqual(started, [], 'and no guest is started')

	def test_by_hand_a_stale_staging_is_built_as_it_always_was(self):
		_, started, built = self.run_cold({'LIBER_DEV_STATE': os.path.join(self.directory.name, 'hand')}, stale=["the x86_64 staging's 'libs' is not built from these sources"])
		self.assertEqual([os.path.basename(command[0]) for command in built], ['build.sh', 'build.sh'], 'the staging and the bootable volume')
		self.assertEqual(len(started), 1)


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


class UnorderedExpectTest(unittest.TestCase):
	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.log = SerialLog(self.directory.name)
		self.guest = scenario.Guest(FakeLab())
		self.addCleanup(self.directory.cleanup)
		self.addCleanup(setattr, lab, 'SERIAL_OVERRIDE', None)

	def expect(self, contains, unordered=False, limit=1):
		step = {'do': 'expect', 'contains': contains}
		if unordered:
			step['unordered'] = True
		scenario.run_step(step, self.guest, FakeLab(), limit, 0)

	# Two lines about one action, in the order the log happened to get them: an ordered pair fails on the
	# second, and an unordered one finds both.
	def test_lines_in_either_order_after_the_action(self):
		self.guest.acted = self.guest.at
		self.log.write('service: declined\nprobe: ends\n')
		self.expect('probe: ends')
		with self.assertRaises(scenario.ScenarioError):
			self.expect('service: declined')
		self.expect('service: declined', unordered=True)

	# It never reaches back past the action it belongs to.
	def test_nothing_before_the_action_is_found(self):
		self.log.write('service: declined\n')
		self.guest.at = lab.serial_size()
		self.guest.acted = self.guest.at
		self.log.write('probe: ends\n')
		with self.assertRaises(scenario.ScenarioError):
			self.expect('service: declined', unordered=True)

	# And it never moves the cursor back: the next ordered step starts after the furthest match.
	def test_the_cursor_only_moves_forward(self):
		self.guest.acted = self.guest.at
		self.log.write('a\nb\nc\n')
		self.expect('c')
		furthest = self.guest.at
		self.expect('a', unordered=True)
		self.assertEqual(self.guest.at, furthest, 'an earlier unordered match left the cursor where it was')

	# A scenario can only say true or false.
	def test_the_option_is_a_boolean(self):
		with self.assertRaises(scenario.ScenarioError):
			scenario.validate_step({'do': 'expect', 'contains': 'x', 'unordered': 'yes'}, 0, 'test')


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


class QemuAudioArgumentsTest(unittest.TestCase):
	# Exercise the production argument builder without booting a guest or creating
	# audio devices. Its default remains the historical single backend definition.
	def audio(self, voices=None, wav=None, spice=False):
		with open(os.path.join(HERE, 'qemu-run.sh')) as handle:
			source = handle.read()
		function = 'qemu_append_audio() {' + source.split('qemu_append_audio() {', 1)[1].split('\n}\n', 1)[0] + '\n}\n'
		environment = dict(os.environ, want_spice='1' if spice else '0')
		for name in ('AUDIO_OUT_VOICES', 'AUDIO_WAV'):
			environment.pop(name, None)
		if voices is not None:
			environment['AUDIO_OUT_VOICES'] = voices
		if wav is not None:
			environment['AUDIO_WAV'] = wav
		return subprocess.run(['bash'], input=('set -eu\n' + function + 'args=()\nqemu_append_audio args\nprintf "%s\\0" "${args[@]}"\n').encode(), env=environment, capture_output=True)

	def test_existing_backend_arguments_are_unchanged_without_an_override(self):
		for options, expected in (({}, b'none,id=snd0'), ({'wav': '/tmp/capture file.wav'}, b'wav,id=snd0,path=/tmp/capture file.wav'), ({'spice': True}, b'spice,id=snd0')):
			with self.subTest(options=options):
				result = self.audio(**options)
				self.assertEqual(result.returncode, 0, result.stderr)
				self.assertEqual(result.stdout, b'-audiodev\0' + expected + b'\0')

	def test_explicit_none_output_voices_keep_the_same_backend(self):
		for voices in ('1', '2', '2147483647'):
			with self.subTest(voices=voices):
				result = self.audio(voices=voices)
				self.assertEqual(result.returncode, 0, result.stderr)
				self.assertEqual(result.stdout, b'-audiodev\0none,id=snd0,out.voices=' + voices.encode() + b'\0')

	def test_invalid_voice_counts_are_refused_before_generating_arguments(self):
		for voices in ('', '0', '-1', '+2', '01', '2.0', ' 2', '2 ', '2\n', '2,out.mixing-engine=off', '2147483648', '18446744073709551616'):
			with self.subTest(voices=voices):
				result = self.audio(voices=voices)
				self.assertEqual(result.returncode, 2, result.stderr)
				self.assertEqual(result.stdout, b'')
				self.assertIn(b'AUDIO_OUT_VOICES', result.stderr)

	def test_explicit_voice_counts_do_not_change_wav_or_spice_semantics(self):
		for options in ({'wav': '/tmp/capture.wav'}, {'spice': True}, {'wav': '/tmp/capture.wav', 'spice': True}):
			with self.subTest(options=options):
				result = self.audio(voices='2', **options)
				self.assertEqual(result.returncode, 2, result.stderr)
				self.assertEqual(result.stdout, b'')
				self.assertIn(b'none backend', result.stderr)


class BrokerPromptTest(unittest.TestCase):
	# The broker half, driven over a socket pair: one end is `serve_request`'s "serial", the other is
	# the test playing the guest.
	def serve(self, request, guest_writes, log_path, nudge_replies=()):
		host, guest = socket.socketpair()
		client, broker_side = socket.socketpair()
		state = {'serial': host, 'log': open(log_path, 'ab', buffering=0), 'log_path': log_path, 'replay': bytearray(), 'clients': [], 'writer': None}
		host.setblocking(False)
		finished = threading.Event()
		self.inputs = []
		failures = []

		def write():
			try:
				for delay, text in guest_writes:
					time.sleep(delay)
					guest.sendall(text)
				pending, replies = b'', list(nudge_replies)
				guest.settimeout(0.05)
				while True:
					try:
						data = guest.recv(4096)
						if not data:
							break
						pending += data
						while b'\n' in pending:
							line, _, pending = pending.partition(b'\n')
							self.inputs.append(line + b'\n')
							if not line and replies:
								guest.sendall(replies.pop(0))
					except socket.timeout:
						if finished.is_set():
							break
			except BaseException as error:
				failures.append(error)

		thread = threading.Thread(target=write, daemon=True)
		thread.start()
		client.sendall(request.encode() + b'\n')
		lab.serve_request(state, broker_side)
		finished.set()
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
		if failures:
			raise failures[0]
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
		self.assertEqual(self.inputs, [b'hang\n'])

	# P0099: the recording completed, but a watchdog line buried its real prompt.
	# The old RUN path never asked for another prompt and timed out on this exact shape.
	def test_completed_recording_with_a_buried_prompt_is_asked_for_a_new_one(self):
		completed = (b'audiorec -s 12 -f p0099-recovery.wav\r\n'
		             b'audiorec: 48000Hz/2ch/16-bit 576000fr duration=12000ms bytes=2304044 dropped=512 peak=2048\r\n'
		             b'\x1b[1;32mvol://system> \x1b[0mWatchdogService: ServiceManager answered - the watchdog policy applies from now\r\n')
		prompt = b'\r\n\x1b[1;32mvol://system> \x1b[0m'
		with mock.patch.object(lab, 'BOOT_NUDGE_QUIET', 0.3):
			reply = self.serve('RUN 2 audiorec -s 12 -f p0099-recovery.wav', [(0.01, completed)], self.log, [prompt])
		self.assertEqual(reply.outcome, 'prompt')
		self.assertTrue(reply.data.startswith(completed), 'completed output and the asynchronous line remain evidence')
		self.assertTrue(reply.data.endswith(prompt), 'success requires the new prompt, not the buried old one')
		self.assertEqual(self.inputs, [b'audiorec -s 12 -f p0099-recovery.wav\n', b'\n'])

	def test_a_buried_prompt_without_a_new_answer_still_times_out_and_is_nudged_once(self):
		with mock.patch.object(lab, 'BOOT_NUDGE_QUIET', 0.3):
			reply = self.serve('RUN 1.4 completed', [(0.01, b'completed\r\nvol://system> async line\r\n')], self.log)
		self.assertEqual(reply.outcome, 'timeout')
		self.assertEqual(self.inputs, [b'completed\n', b'\n'], 'one observed prompt authorizes only one bounded nudge')

	def test_run_does_not_nudge_from_a_historical_prompt_or_restore_marker(self):
		with open(self.log, 'wb') as log:
			log.write(b'vol://system> old asynchronous line\r\n')
		for output in (b'busy\r\nworking\r\n', b'busy\r\nsleep: resumed (the restore of a hibernation image)\r\n'):
			with self.subTest(output=output), mock.patch.object(lab, 'BOOT_NUDGE_QUIET', 0.1):
				reply = self.serve('RUN 0.6 busy', [(0.01, output)], self.log)
			self.assertEqual(reply.outcome, 'timeout')
			self.assertEqual(self.inputs, [b'busy\n'], 'neither the old log nor a restore message proves this command has prompted')

	def test_continued_output_after_a_prompt_never_receives_a_nudge(self):
		writes = [(0.01, b'evil\r\nvol://attacker> ')] + [(0.1, b'still running\r\n') for _ in range(10)]
		with mock.patch.object(lab, 'BOOT_NUDGE_QUIET', 0.3):
			reply = self.serve('RUN 0.9 evil', writes, self.log)
		self.assertEqual(reply.outcome, 'timeout')
		self.assertEqual(self.inputs, [b'evil\n'])

	def test_another_async_line_requires_another_new_prompt_and_settlement(self):
		with mock.patch.object(lab, 'BOOT_NUDGE_QUIET', 0.3):
			reply = self.serve('RUN 3 done', [(0.01, b'done\r\nvol://system> first async line\r\n')], self.log,
			                   [b'\r\nvol://system> second async line\r\n', b'\r\nvol://system> '])
		self.assertEqual(reply.outcome, 'prompt')
		self.assertEqual(self.inputs, [b'done\n', b'\n', b'\n'])
		self.assertIn(b'second async line', reply.data)

	def test_a_prompt_prefix_on_the_command_echo_never_nudges_foreground_input(self):
		for command in ('interactive', 'echo vol://system> text'):
			output = b'\x1b[1;32mvol://system> \x1b[0m' + command.encode() + b'\r\nwaiting for input\r\n'
			with self.subTest(command=command), mock.patch.object(lab, 'BOOT_NUDGE_QUIET', 0.1):
				reply = self.serve('RUN 0.6 ' + command, [(0.01, output)], self.log)
			self.assertEqual(reply.outcome, 'timeout')
			self.assertEqual(self.inputs, [command.encode() + b'\n'])

	def test_an_ordinary_wait_does_not_inject_input_after_a_buried_prompt(self):
		with mock.patch.object(lab, 'BOOT_NUDGE_QUIET', 0.1):
			reply = self.serve('WAIT 0.6', [(0.01, b'vol://system> interactive output\r\n')], self.log)
		self.assertEqual(reply.outcome, 'timeout')
		self.assertEqual(self.inputs, [])


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
			'recipe': scenario.file_digest(os.path.join(HERE, 'scenarios', 'make-fixtures.py')),
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

	# AND A SET MADE BY ANOTHER RECIPE: a cold run builds the set only when this check names something, so a changed
	# recipe has to be named here or its fixtures are never made again.
	def test_a_set_made_by_another_recipe_is_stale(self):
		self.manifest['recipe'] = 'x' * 64
		self.write_manifest()
		self.assertTrue(any('another version of make-fixtures.py' in complaint for complaint in scenario.stale_fixtures('x86_64')))

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

	def document(self, name, step):
		path = os.path.join(self.directory.name, name + '.toml')
		self.write(path, f'version = 1\nname = "{name}"\n[[step]]\n{step}\n'.encode())
		return path

	@contextlib.contextmanager
	def dev_runner(self):
		# Only the live guest boundary is replaced. Document loading and fixture validation run
		# through cmd_dev_test against the real temporary files and their recorded digests.
		with mock.patch.object(lab, 'dev_state', return_value=('ready', {})), \
			mock.patch.object(lab, 'scenario_lease', return_value=contextlib.nullcontext()), \
			mock.patch.object(lab, 'LabGuest'), \
			mock.patch.object(scenario, 'run', return_value=0) as run, \
			contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
			yield run

	# The performance loop deliberately rebuilds uname before running a terminal-only scenario.
	# The old unconditional fixture check rejected that scenario although it publishes no fixture.
	def test_terminal_only_scenarios_do_not_require_generated_fixtures(self):
		path = self.document('terminal', 'do = "prompt"')
		self.write(os.path.join(self.staged, 'bin', 'uname'), b'REBUILT-UNAM')
		with self.dev_runner() as run:
			lab.cmd_dev_test([path])
			run.assert_called_once()
		os.unlink(os.path.join(self.fixtures, 'fixtures.json'))
		with self.dev_runner() as run:
			lab.cmd_dev_test([path])
			run.assert_called_once()

	def test_any_fixture_consuming_document_still_requires_a_current_set(self):
		terminal = self.document('terminal', 'do = "prompt"')
		for kind, field in (('publish', 'artifact = "uname"'), ('fixture', 'name = "sample"')):
			with self.subTest(kind=kind):
				path = self.document(kind, f'do = "{kind}"\n{field}\nfile = "{{fixtures}}/uname-shadow"')
				self.write(os.path.join(self.staged, 'bin', 'uname'), b'STAGED-UNAME')
				with self.dev_runner() as run:
					lab.cmd_dev_test([terminal, path])
					self.assertEqual(run.call_count, 2)
				self.write(os.path.join(self.staged, 'bin', 'uname'), b'REBUILT-UNAM')
				with self.dev_runner() as run:
					with self.assertRaises(SystemExit):
						lab.cmd_dev_test([terminal, path])
					run.assert_not_called()

	# And every shipped scenario has to load under the new vocabulary.
	def test_every_shipped_scenario_still_validates(self):
		lab.BUILD_ROOT = os.path.abspath(os.path.join(HERE, '..', '..', '.build'))
		for name in sorted(os.listdir(os.path.join(HERE, 'scenarios'))):
			if not name.endswith('.toml'):
				continue
			with self.subTest(scenario=name):
				scenario.load(os.path.join(HERE, 'scenarios', name))


class RefusedStepTest(unittest.TestCase):
	# BOOT-020. `launch-program` ended with a paragraph saying an unmanifested component is refused
	# and no step after it, so the behaviour it claimed to cover was asserted nowhere.
	def guest(self, status):
		class Guest(ScriptedGuest):
			def launch_status(self, *args, **kwargs):
				self.calls.append('launch_status')
				return status

		return Guest()

	def step(self, guest, expected=None):
		step = {'do': 'refused', 'program': 'no-such-component'}
		if expected is not None:
			step['status'] = expected
		scenario.run_step(step, scenario.Guest(guest), guest, 5, 0)

	def test_a_refusal_passes(self):
		self.step(self.guest(24), 24)

	# The failure this step exists to catch: the component was launched after all.
	def test_a_successful_launch_fails_the_step(self):
		with self.assertRaises(scenario.ScenarioError) as caught:
			self.step(self.guest(0), 24)
		self.assertIn('was LAUNCHED', str(caught.exception))

	# A refusal for a different reason is not the refusal that was asserted - "the guest is gone"
	# would otherwise satisfy "this component is refused".
	def test_the_wrong_refusal_fails_the_step(self):
		with self.assertRaises(scenario.ScenarioError):
			self.step(self.guest(23), 24)

	def test_any_refusal_passes_when_none_is_named(self):
		self.step(self.guest(23))

	def test_the_status_field_is_validated(self):
		document = {'version': scenario.SCENARIO_VERSION, 'name': 'x', 'step': [{'do': 'refused', 'program': 'p', 'status': 'twenty-four'}]}
		with self.assertRaises(scenario.ScenarioError):
			scenario.validate(document, 'test.toml')


# THE WATCHDOG GATE'S ORACLE: a run state reached within a bound, or held over a stretch - and a hold fails at the
# first sample that is anything else, since `running` held is the claim that no expiry happened in it.
class RunStateStepTest(unittest.TestCase):
	def guest(self, states):
		class Guest(ScriptedGuest):
			def run_state(self, timeout=5):
				self.calls.append('run_state')
				return states.pop(0) if len(states) > 1 else states[0]

		return Guest()

	def drive(self, step, guest, limit=5):
		scenario.run_step(step, scenario.Guest(guest), guest, limit, 0)

	def test_a_state_reached_within_the_bound_passes(self):
		self.drive({'do': 'run-state', 'state': 'watchdog'}, self.guest(['running', 'running', 'watchdog']))

	def test_a_state_never_reached_fails(self):
		with self.assertRaises(scenario.ScenarioError) as caught:
			self.drive({'do': 'run-state', 'state': 'watchdog'}, self.guest(['running']), 1)
		self.assertIn('not watchdog', str(caught.exception))

	def test_a_hold_fails_at_the_first_other_state(self):
		with self.assertRaises(scenario.ScenarioError) as caught:
			self.drive({'do': 'run-state', 'state': 'running', 'hold': 5}, self.guest(['running', 'watchdog']))
		self.assertIn('was watchdog, not running', str(caught.exception))

	def test_an_unanswered_query_fails_a_hold(self):
		with self.assertRaises(scenario.ScenarioError):
			self.drive({'do': 'run-state', 'state': 'running', 'hold': 2}, self.guest([None]))

	def test_the_fields_are_validated(self):
		for step in ({'do': 'run-state', 'state': 'asleep'}, {'do': 'run-state', 'state': 'running', 'hold': 0}, {'do': 'run-state', 'state': 'running', 'hold': True}, {'do': 'kill-driver', 'function': '8086-25ab'}):
			document = {'version': scenario.SCENARIO_VERSION, 'name': 'x', 'step': [step]}
			with self.assertRaises(scenario.ScenarioError, msg=repr(step)):
				scenario.validate(document, 'test.toml')
		document = {'version': scenario.SCENARIO_VERSION, 'name': 'x', 'step': [{'do': 'run-state', 'state': 'running', 'hold': 45}, {'do': 'kill-driver', 'function': '8086:25ab'}]}
		scenario.validate(document, 'test.toml')


# THE SERIAL-HANDOFF GATE'S PORT STEPS: a command typed into the UART whose answer must come back through it, the
# development kernel's console requests, and a scenario that ends with the machine halted on purpose.
class SerialStepTest(unittest.TestCase):
	def guest(self, answer):
		class Guest(ScriptedGuest):
			def serial_sh(self, command, timeout):
				self.calls.append(('serial_sh', command))
				return answer

		return Guest()

	def drive(self, step, guest, limit=5):
		scenario.run_step(step, scenario.Guest(guest), guest, limit, 0)

	def test_an_answer_carrying_the_text_passes(self):
		guest = self.guest('echo handoff\r\nhandoff\r\n')
		self.drive({'do': 'serial-sh', 'command': 'echo handoff', 'contains': 'handoff'}, guest)
		self.assertIn(('serial_sh', 'echo handoff'), guest.calls, 'the command went to the UART')

	def test_an_answer_without_the_text_fails(self):
		with self.assertRaises(scenario.ScenarioError) as caught:
			self.drive({'do': 'serial-sh', 'command': 'echo handoff', 'contains': 'elsewhere'}, self.guest('echo handoff\r\nhandoff\r\n'))
		self.assertIn('without', str(caught.exception))

	# NO BROKER, OR NO PROMPT: the step fails rather than reading an empty answer as one that happened not to match.
	def test_no_answer_fails_by_name(self):
		with self.assertRaises(scenario.ScenarioError) as caught:
			self.drive({'do': 'serial-sh', 'command': 'echo handoff', 'contains': 'handoff'}, self.guest(None))
		self.assertIn('serial broker', str(caught.exception))

	def test_a_refused_console_request_fails(self):
		guest = ScriptedGuest(fail_on=None)
		guest.answer = lambda name: name != 'kernel_console'
		with self.assertRaises(scenario.ScenarioError):
			self.drive({'do': 'kernel-console', 'request': 'kill-holder'}, guest)

	def test_the_fields_and_the_end_are_validated(self):
		for step in ({'do': 'serial-sh', 'command': 'echo a\necho b', 'contains': 'a'}, {'do': 'serial-sh', 'command': ' ', 'contains': 'a'}, {'do': 'serial-sh', 'command': 'ls'}, {'do': 'kernel-console', 'request': 'reboot'}):
			document = {'version': scenario.SCENARIO_VERSION, 'name': 'x', 'step': [step]}
			with self.assertRaises(scenario.ScenarioError, msg=repr(step)):
				scenario.validate(document, 'test.toml')
		with self.assertRaises(scenario.ScenarioError):
			scenario.validate({'version': scenario.SCENARIO_VERSION, 'name': 'x', 'ends': 'crashed', 'step': [{'do': 'prompt'}]}, 'test.toml')
		document = {'version': scenario.SCENARIO_VERSION, 'name': 'x', 'ends': 'halted', 'step': [{'do': 'serial-sh', 'command': 'echo a', 'contains': 'a'}, {'do': 'kernel-console', 'request': 'panic'}]}
		scenario.validate(document, 'test.toml')

	# A HALTED MACHINE ANSWERS NOTHING, and the teardown asks exactly that: a prompt after the panic fails it.
	def test_a_halted_end_is_torn_down_by_asking_that_nothing_answers(self):
		quiet = ScriptedGuest()
		quiet.answer = lambda name: False if name == 'wait_prompt' else True
		self.assertEqual(scenario.teardown(quiet, ends='halted'), [])
		self.assertEqual(scenario.teardown(ScriptedGuest(), ends='halted'), ['the machine answered a prompt after it was asked to power off'])


# BOOT-007's other half: the claim that a child which ignores TERM is killed, and that its whole
# GROUP goes with it. Asserted in a comment when it was written; exercised here, because a cleanup
# that quietly fails to clean is worse than none - the next run inherits whatever survived.
class ChildProcessTest(unittest.TestCase):
	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.addCleanup(self.directory.cleanup)

	def script(self, body):
		path = os.path.join(self.directory.name, 'child-%d.py' % len(os.listdir(self.directory.name)))
		with open(path, 'w') as handle:
			handle.write(body)
		return [sys.executable, path]

	def test_a_child_that_finishes_returns_its_output(self):
		done = lab.run_child(self.script('print("answered")'), 30, stdout=subprocess.PIPE, text=True)
		self.assertEqual(done.returncode, 0)
		self.assertEqual(done.stdout.strip(), 'answered')

	def test_a_failing_child_reports_its_status(self):
		self.assertEqual(lab.run_child(self.script('raise SystemExit(3)'), 30).returncode, 3)

	# A child that never finishes must not hold the caller past its deadline.
	def test_a_hanging_child_raises_within_its_deadline(self):
		started = time.monotonic()
		with self.assertRaises(lab.ChildTimeout):
			lab.run_child(self.script('import time; time.sleep(120)'), 1)
		self.assertLess(time.monotonic() - started, 30, 'the deadline bounds the wait, not the child')

	# The case the escalation exists for: a child that catches TERM and keeps going.
	def test_a_child_that_ignores_term_is_killed(self):
		body = 'import signal, time\nsignal.signal(signal.SIGTERM, lambda *a: None)\nprint("up", flush=True)\ntime.sleep(120)\n'
		with self.assertRaises(lab.ChildTimeout):
			lab.run_child(self.script(body), 1, stdout=subprocess.PIPE, text=True)
		self.assertEqual(self.leftovers(), [], 'a process that ignores TERM survived the escalation')

	# And the child's own children, which is the whole reason the GROUP is signalled: a build script
	# that started a compiler leaves it behind when only the leader is stopped.
	def test_the_childs_own_children_go_with_it(self):
		marker = os.path.join(self.directory.name, 'grandchild.pid')
		body = (
			'import subprocess, sys, time\n'
			'child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"])\n'
			'open(%r, "w").write(str(child.pid))\n'
			'time.sleep(120)\n'
		) % marker
		with self.assertRaises(lab.ChildTimeout):
			lab.run_child(self.script(body), 2)
		with open(marker) as handle:
			grandchild = int(handle.read())
		for _ in range(50):
			if not self.alive(grandchild):
				break
			time.sleep(0.1)
		self.assertFalse(self.alive(grandchild), 'the grandchild outlived the group it was signalled with')

	def alive(self, pid):
		try:
			os.kill(pid, 0)
		except OSError:
			return False
		return True

	def leftovers(self):
		found = subprocess.run(['pgrep', '-f', self.directory.name], capture_output=True, text=True)
		return [line for line in found.stdout.split() if line]


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


# Which guest a scenario's CHILD steps drive. `publish`, `reset` and `restart` re-invoke `lab.py` as
# a subprocess, so the endpoint every other step reaches through a module global has to be handed
# across the exec - and a module global does not cross one.
class ChildEndpointTest(unittest.TestCase):
	def setUp(self):
		self.calls = []
		previous = lab.run_child

		def record(command, timeout, **kwargs):
			self.calls.append((command, kwargs.get('env')))
			return subprocess.CompletedProcess(command, 0)

		lab.run_child = record
		self.addCleanup(setattr, lab, 'run_child', previous)
		for name in ('CHANNEL_OVERRIDE', 'SERIAL_OVERRIDE', 'MON_OVERRIDE', 'QMP_OVERRIDE'):
			self.addCleanup(setattr, lab, name, getattr(lab, name))

	# The break: `scenario_child` started its child with this process's environment and nothing else.
	# On a cold run the parent drove the cold guest and the three child steps drove the persistent
	# instance's sockets - which on a machine with no persistent instance do not exist, so a scenario
	# passed and its teardown could not drop the state it had just made. Watched to fail with the
	# `env=` argument removed: the child then carries no endpoint at all.
	def test_a_child_step_is_told_which_guest_to_drive(self):
		lab.CHANNEL_OVERRIDE = '/tmp/cold-channel.sock'
		lab.SERIAL_OVERRIDE = '/tmp/cold-serial.log'
		lab.MON_OVERRIDE = '/tmp/cold-monitor.sock'
		lab.QMP_OVERRIDE = '/tmp/cold-qmp.sock'
		self.assertTrue(lab.LabGuest(10).reset(10))
		command, env = self.calls[0]
		self.assertIn('dev-reset', command)
		self.assertEqual(env.get('LAB_CHANNEL_OVERRIDE'), '/tmp/cold-channel.sock')
		self.assertEqual(env.get('LAB_SERIAL_OVERRIDE'), '/tmp/cold-serial.log')
		self.assertEqual(env.get('LAB_MON_OVERRIDE'), '/tmp/cold-monitor.sock')
		self.assertEqual(env.get('LAB_QMP_OVERRIDE'), '/tmp/cold-qmp.sock')

	# And the persistent instance keeps its defaults: with no override in force the child is told
	# nothing, rather than being pointed at an empty string it would then treat as a path.
	def test_a_child_step_with_no_override_inherits_the_default_endpoint(self):
		lab.CHANNEL_OVERRIDE = None
		lab.SERIAL_OVERRIDE = None
		lab.MON_OVERRIDE = None
		lab.QMP_OVERRIDE = None
		self.assertTrue(lab.LabGuest(10).publish('artifact', '/tmp/artifact', 10))
		_, env = self.calls[0]
		self.assertIsNotNone(env, 'the child is given an environment to inherit')
		for name in ('LAB_CHANNEL_OVERRIDE', 'LAB_SERIAL_OVERRIDE', 'LAB_MON_OVERRIDE', 'LAB_QMP_OVERRIDE'):
			self.assertNotIn(name, env)

	# The other half of the same contract: what `scenario_child` writes into the environment is what
	# a fresh `lab.py` reads out of it. Read through a child process, because that is the only place
	# the import-time read actually happens.
	def test_a_fresh_process_reads_the_endpoint_out_of_its_environment(self):
		script = 'import lab; print(lab.channel_path()); print(lab.serial_log_path()); print(lab.mon_sock()); print(lab.qmp_sock())'
		env = dict(os.environ, LAB_CHANNEL_OVERRIDE='/tmp/cold-channel.sock', LAB_SERIAL_OVERRIDE='/tmp/cold-serial.log', LAB_MON_OVERRIDE='/tmp/cold-monitor.sock', LAB_QMP_OVERRIDE='/tmp/cold-qmp.sock')
		out = subprocess.run([sys.executable, '-c', script], cwd=HERE, env=env, capture_output=True, text=True, check=True).stdout.split()
		self.assertEqual(out, ['/tmp/cold-channel.sock', '/tmp/cold-serial.log', '/tmp/cold-monitor.sock', '/tmp/cold-qmp.sock'])


# `guest-console.py` against a console that closes, as QEMU's does when its guest powers off. The
# break, watched: an empty read from a closed socket was taken for a timeout's, so the driver kept
# recording nothing until `--seconds` ran out.
class GuestConsoleTest(unittest.TestCase):
	def test_the_driver_ends_when_the_console_closes(self):
		with tempfile.TemporaryDirectory() as directory:
			path = os.path.join(directory, 'console')
			server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
			self.addCleanup(server.close)
			server.bind(path)
			server.listen(1)
			script = os.path.join(directory, 'script')
			with open(script, 'w') as handle:
				handle.write('poweroff\n')
			log = os.path.join(directory, 'guest')

			def guest():
				connection, _ = server.accept()
				connection.settimeout(30)
				# The banner and the prompt follow the attach line, as the shell prints them.
				connection.sendall(b'shell attached\r\n')
				time.sleep(0.3)
				connection.sendall(b'LiberSystem\r\nvol://system> ')
				typed = b''
				while b'\n' not in typed:
					typed += connection.recv(64)
				connection.sendall(b'power: the system is going down\r\n')
				connection.close()

			threading.Thread(target=guest, daemon=True).start()
			started = time.monotonic()
			done = subprocess.run([sys.executable, os.path.join(HERE, 'guest-console.py'), '--socket', path, '--log', log, '--script', script, '--seconds', '60'], capture_output=True, text=True, timeout=120)
			elapsed = time.monotonic() - started
			self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
			self.assertLess(elapsed, 20, 'the driver spun on a closed console to its deadline')
			with open(log, 'rb') as handle:
				recorded = handle.read()
			self.assertIn(b'# guest-console sent: poweroff', recorded)
			self.assertIn(b'power: the system is going down', recorded)


# A SCREENSHOT OF A LAB GUEST WITH A STATE DIRECTORY OF ITS OWN snaps that guest: `qemu-run.sh` puts its control monitor
# in `LIBER_DEV_STATE`, and `screenshot.sh` looked only in `.build/boot`, so it booted a throwaway instance of its own
# and the 2D demo gate's frames never showed the demo. Watched failing against the old path: the capture fell back to
# that boot.
class ScreenshotTest(unittest.TestCase):
	def test_a_lab_guest_in_its_own_state_directory_is_snapped_live(self):
		directory = tempfile.mkdtemp()
		self.addCleanup(shutil.rmtree, directory, True)
		os.makedirs(os.path.join(directory, 'src', 'harness'))
		shutil.copy(os.path.join(HERE, 'screenshot.sh'), os.path.join(directory, 'src', 'harness'))
		state = os.path.join(directory, 'state')
		os.makedirs(state)
		monitor = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
		self.addCleanup(monitor.close)
		monitor.bind(os.path.join(state, 'qemu-monitor.sock'))
		monitor.listen(4)

		# An HMP monitor that answers `screendump PATH` with a one-pixel frame; the liveness probe connects and leaves.
		def serve():
			while True:
				try:
					connection, _ = monitor.accept()
				except OSError:
					return
				with connection:
					received = b''
					while chunk := connection.recv(4096):
						received += chunk
						if b'\n' in received:
							break
					if received.startswith(b'screendump '):
						with open(received.split(b'\n')[0][len(b'screendump '):].decode(), 'wb') as handle:
							handle.write(b'P6\n1 1\n255\n\x10\x20\x30')

		threading.Thread(target=serve, daemon=True).start()
		out = os.path.join(directory, 'shot.ppm')
		done = subprocess.run([os.path.join(directory, 'src', 'harness', 'screenshot.sh'), out], capture_output=True, text=True, timeout=60, env={**os.environ, 'LIBER_DEV_STATE': state})
		self.assertEqual(done.returncode, 0, done.stderr)
		self.assertIn('attaching to the running QEMU', done.stderr)
		self.assertGreater(os.path.getsize(out), 0)



# A GATE'S COMPLETION LINE ends the recording, the settle window after it, and nothing else. The gates that name one,
# each with a late line its oracle judges: the driver ends within seconds of the window rather than at its deadline,
# and a line arriving late inside the window is in the log the gate's oracle reads. Watched failing with the window
# taken away: the same late line is then missing, so a gate that refused it would pass, and one that needed it would
# fail. A gate whose checks are all of lines that must appear before its completion line keeps no window, and has no
# late line to judge.
COMPLETION_LINES = (
	# (gate script, the completion line it names, its settle window, a late line its oracle refuses or needs)
	('check-firmware-tree.sh', 'tree-fixture: event 2: GPIO line 2 high, acknowledged', 20, 'tree-fixture: refused: injected late'),
	('check-usb-release.sh', 'usb-release-done', 10, "thread 'main' panicked at injected late"),
	('check-usb-legacy.sh', 'usb-legacy-done', 10, "thread 'main' panicked at injected late"),
	('check-i2c-hid.sh', 'hidcheck: PASS malformed', 10, 'hidcheck: FAIL injected late'),
	('check-bluetooth-le.sh', 'btclassic: PASS ctkd', 5, 'btclassic: BEGIN SC authentication downgrade'),
	('check-brightness-usb.sh', 'brightness-usb-done', 10, 'brightcheck: FAIL injected late'),
	('check-power-ups.sh', 'power-ups-done', 10, 'upscheck: FAIL injected late'),
	('check-bluetooth-classic.sh', 'bluetooth-classic-done', 10, 'bt-fixture: paired keyboard type 0x08 key 0badc0de'),
	('check-bluetooth-service.sh', 'bluetooth-service-done', 10, 'bt-fixture: paired; key 0badc0de'),
	('check-power-service.sh', 'power-service-done', 10, 'powercheck: FAIL injected late'),
	('check-smartcard-service.sh', 'smartcard-service-done', 10, 'cardhold: FAIL injected late'),
	('check-bluetooth-transfer.sh', 'bluetooth-transfer-done', 20, 'network: configured via DHCP - 10.0.2.15'),
	('check-foreign-loader-guest.sh', 'foreign-loader-guest-done', 5, 'vkprobe: foreign: icd interface: injected late - refused'),
	('check-lifecycle.sh', 'lifecycle-done', 5, 'lifecheck: crashing'),
	('check-bluetooth-le-audio.sh', 'bluetooth-le-audio-done', 0, None),
	('check-bluetooth-audio.sh', 'bluetooth-audio-done', 0, None),
	('check-foreign-facilities-guest.sh', 'foreign-facilities-guest-done', 0, None),
	('check-icd-selection.sh', 'icd-selection-done', 0, None),
)


class CompletionLineTest(unittest.TestCase):
	def drive(self, done, settle, late, delay, during_boot=False):
		directory = tempfile.mkdtemp()
		self.addCleanup(shutil.rmtree, directory, True)
		path = os.path.join(directory, 'console')
		server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
		self.addCleanup(server.close)
		server.bind(path)
		server.listen(1)
		script = os.path.join(directory, 'script')
		with open(script, 'w') as handle:
			handle.write('probe\n')
		log = os.path.join(directory, 'guest')
		stop = threading.Event()
		self.addCleanup(stop.set)

		def guest():
			connection, _ = server.accept()
			connection.settimeout(30)
			if during_boot:
				connection.sendall(done.encode() + b'\r\n')
				time.sleep(0.5)
			connection.sendall(b'shell attached\r\n')
			time.sleep(0.3)
			connection.sendall(b'vol://system> ')
			typed = b''
			while b'\n' not in typed:
				typed += connection.recv(64)
			# The shell echoes what was typed, which carries no completion line of its own.
			connection.sendall(b'probe\r\n' + (b'' if during_boot else b'\x1b[32m' + done.encode() + b'\x1b[0m\r\n'))
			time.sleep(delay)
			# A driver with no window has ended by now, and the late line meets a closed console.
			with contextlib.suppress(OSError):
				connection.sendall(late.encode() + b'\r\n')
			stop.wait(120)
			connection.close()

		threading.Thread(target=guest, daemon=True).start()
		started = time.monotonic()
		done_run = subprocess.run([sys.executable, os.path.join(HERE, 'guest-console.py'), '--socket', path, '--log', log, '--script', script, '--seconds', '90', '--done', done, '--settle', str(settle)], capture_output=True, text=True, timeout=150)
		elapsed = time.monotonic() - started
		with open(log, 'rb') as handle:
			return done_run, elapsed, handle.read()

	def test_each_gate_names_its_completion_line_and_window(self):
		for name, done, settle, _ in COMPLETION_LINES:
			with self.subTest(gate=name):
				with open(os.path.join(os.path.dirname(HERE), 'tools', name)) as handle:
					text = handle.read()
				self.assertIn(done, text, 'the gate names its completion line')
				self.assertRegex(text, r'GUEST_GATE_SETTLE=%d\b' % settle, 'and states its settle window')

	def test_a_late_line_inside_the_window_is_recorded_and_the_driver_ends_after_it(self):
		results = {}

		def one(case):
			_, done, settle, late = case
			# The late line half-way through the gate's window, with the window and without it.
			results[case] = (self.drive(done, settle, late, settle / 2), self.drive(done, 0, late, settle / 2))

		windowed = [case for case in COMPLETION_LINES if case[3] is not None]
		threads = [threading.Thread(target=one, args=(case,)) for case in windowed]
		for thread in threads:
			thread.start()
		for thread in threads:
			thread.join()
		for case in windowed:
			name, done, settle, late = case
			with self.subTest(gate=name):
				(run, elapsed, recorded), (_, unwindowed, missed) = results[case]
				self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
				self.assertLess(elapsed, settle + 20, 'the driver ran past its completion line and window toward its deadline')
				self.assertIn(late.encode(), recorded, 'a line inside the window reaches the log the oracle reads')
				self.assertLess(unwindowed, settle / 2 + 15)
				self.assertNotIn(late.encode(), missed, 'with no window the late line is missed - which is why a gate with negative checks keeps one')


	# A LINE PRINTED WHILE THE GUEST BOOTS COUNTS: `firmware-tree`'s second acknowledged edge came before the shell
	# took `lsdev` on both ports, and a driver that watched only what followed the last command ran to its budget.
	def test_a_completion_line_printed_during_boot_ends_the_recording_once_the_commands_are_typed(self):
		_, done, _, late = COMPLETION_LINES[0]
		run, elapsed, recorded = self.drive(done, 3, late, 1, during_boot=True)
		self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
		self.assertIn(b'# guest-console sent: probe', recorded, 'every command is still typed')
		self.assertIn(b'the completion line came', recorded)
		self.assertLess(elapsed, 20, 'the driver ran past its completion line toward its deadline')


# A suite run's two logs, named as `test-kernel.sh` names them, passed or failed by its own pass line.
def write_suite_run(logs, arch, stamp, pid, passed=True, trace=False, whole=False):
	stem = os.path.join(logs, 'test', f'{arch}-{stamp}-{pid}')
	os.makedirs(os.path.dirname(stem), exist_ok=True)
	with open(f'{stem}-guest.log', 'w') as handle:
		handle.write(('running 12 tests (all tags)\n' if whole else '') + ('captrace: begin\n' if trace else '') + ('test suite complete: 12 passed\n' if passed else 'test FAILED: storage::mount\n'))
	with open(f'{stem}-run.log', 'w') as handle:
		handle.write('running 12 tests\n')
	return [f'{stem}-guest.log', f'{stem}-run.log']


# WHAT `.build/logs` KEEPS - `lab.py`'s logs rules over a fixture tree. The break each test was
# watched failing against is the rule it replaced: the newest twenty test runs by time, whatever
# they were, and nothing for any other folder.
class LogsRulesTest(unittest.TestCase):
	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.addCleanup(self.directory.cleanup)
		self.root = os.path.join(self.directory.name, 'logs')
		os.makedirs(os.path.join(self.root, 'test'))
		self.writers = {'acpi', 'boot-volume-controllers', 'qemu-2d-account', 'qemu-2d-demo', 'test'}

	def write(self, relative, age=0):
		path = os.path.join(self.root, relative)
		os.makedirs(os.path.dirname(path), exist_ok=True)
		with open(path, 'w') as handle:
			handle.write('log\n')
		if age:
			stamp = time.time() - age
			os.utime(path, (stamp, stamp))
			os.utime(os.path.dirname(path), (stamp, stamp))
		return path

	def suite(self, arch, stamp, pid, **kwargs):
		return {os.path.relpath(path, self.root) for path in write_suite_run(self.root, arch, stamp, pid, **kwargs)}

	def removed(self, pins=(), alive=lambda pid: False):
		files, _ = lab.logs_removals(self.root, self.writers, set(pins), alive)
		return {os.path.relpath(path, self.root) for path in files}

	def test_the_newest_run_of_each_target_is_kept_and_older_ones_go(self):
		old = self.suite('x86_64', '20261001T000000Z', 101)
		self.suite('x86_64', '20261002T000000Z', 102)
		self.suite('aarch64', '20261001T000000Z', 103)
		self.assertEqual(self.removed(), old)

	# `check-capability-trace.sh` takes the newest x86_64 guest log that CARRIES a trace, from all of
	# them: a later `./test.sh --tags numa` writes a newer log without one, and the trace must stay.
	def test_an_older_x86_64_log_with_a_trace_is_kept_beside_a_newer_one_without(self):
		traced = self.suite('x86_64', '20261001T000000Z', 101, trace=True)
		self.suite('x86_64', '20261002T000000Z', 102)
		self.assertEqual(self.removed(), {path for path in traced if path.endswith('-run.log')})

	def test_a_failed_run_is_kept_until_a_later_run_of_its_target_passes(self):
		passed = self.suite('aarch64', '20261001T000000Z', 101)
		failed = self.suite('aarch64', '20261002T000000Z', 102, passed=False) | self.suite('aarch64', '20261003T000000Z', 103, passed=False)
		self.assertEqual(self.removed(), passed)
		self.suite('aarch64', '20261004T000000Z', 104)
		self.assertEqual(self.removed(), passed | failed)

	# A RUN THAT HAD TO RESUME FAILED, though its last resumed boot printed the pass line: it is kept like any failed
	# run, with its resumed boots' logs, until a later run of its target passes.
	def test_a_resumed_run_is_a_failed_run(self):
		resumed = self.suite('x86_64', '20261002T000000Z', 102, passed=False)
		for half in ('run', 'guest'):
			name = f'test/x86_64-20261002T000000Z-102-restart1-{half}.log'
			with open(os.path.join(self.root, name), 'w') as handle:
				handle.write('test suite complete: 5 passed\n')
			resumed.add(name)
		self.suite('x86_64', '20261003T000000Z', 103, passed=False)
		self.assertEqual(self.removed(), set(), 'a resumed run is kept as a failed one, its resumed boots with it')
		self.suite('x86_64', '20261004T000000Z', 104)
		self.assertTrue(resumed <= self.removed(), 'and goes, whole, once a later run of its target passes')

	# THE PLANNER'S DURATIONS: a port's suite is split across guests by each test's measured duration in the newest
	# whole run, which a later selection does not replace - so that run stays beside the newer one, and goes once a newer
	# whole run of its target has ended.
	def test_the_newest_whole_run_is_kept_beside_a_newer_selection(self):
		whole = self.suite('aarch64', '20261001T000000Z', 101, whole=True)
		selection = self.suite('aarch64', '20261002T000000Z', 102)
		self.assertEqual(self.removed(), set())
		self.suite('aarch64', '20261003T000000Z', 103, whole=True)
		self.assertEqual(self.removed(), whole | selection)

	def test_a_live_runs_logs_are_left_alone(self):
		live = self.suite('riscv64', '20261001T000000Z', 101)
		self.suite('riscv64', '20261002T000000Z', 102)
		self.assertEqual(self.removed(alive=lambda pid: pid == 101), set())
		self.assertEqual(self.removed(), live)

	def test_a_pinned_file_is_kept_and_an_unpinned_one_beside_it_removed(self):
		document = os.path.join(self.directory.name, 'NOTES.md')
		with open(document, 'w') as handle:
			handle.write(
				'The runs: `.build/logs/measurement/runs/{a,c}.json`, and the old suite .build/logs/test/x86_64-20261001T000000Z-101-guest.log.\n'
				'A run folder below the top is pinned whole: .build/logs/qemu-2d-account/ordinary-2/.\n'
				'Whole top-level folders and patterns are pointers: `.build/logs/scratch/`, `.build/logs/test/x86_64-*-guest.log`, `.build/logs/<gate>/run.log`.\n'
			)
		for name in ('measurement/runs/a.json', 'measurement/runs/b.json', 'measurement/runs/c.json', 'measurement/summary.txt', 'scratch/x.log', 'qemu-2d-account/ordinary-2/run.out', 'qemu-2d-account/ordinary-1/run.out'):
			self.write(name)
		oldest = self.suite('x86_64', '20260930T000000Z', 100)
		old = self.suite('x86_64', '20261001T000000Z', 101)
		self.suite('x86_64', '20261002T000000Z', 102)
		pins = lab.logs_pins([document])
		self.assertEqual(self.removed(pins), {'measurement/runs/b.json', 'measurement/summary.txt', 'scratch/x.log', 'qemu-2d-account/ordinary-1/run.out'} | oldest | {path for path in old if path.endswith('-run.log')})

	def test_each_gate_keeps_its_newest_set_and_what_no_writer_names_goes(self):
		self.write('acpi/serial.log', age=7200)
		self.write('acpi/down.log')
		self.write('boot-volume-controllers-x86_64/guest.log', age=7200)
		self.write('qemu-2d-demo.AAAAAA/demo.log', age=7200)
		self.write('qemu-2d-demo.BBBBBB/demo.log', age=60)
		for run in ('20261001T000000Z', '20261002T000000Z-opt1', 'ordinary-1'):
			self.write(f'qemu-2d-account/{run}/run.out')
		for orphan in ('bt-pan-389ba/result.json', 'dma-aarch64-build.log', 'verify-shadow.Ab12Cd34/host-full.txt'):
			self.write(orphan)
		self.assertEqual(self.removed(), {'qemu-2d-demo.AAAAAA/demo.log', 'qemu-2d-account/20261001T000000Z/run.out', 'qemu-2d-account/ordinary-1/run.out', 'bt-pan-389ba/result.json', 'dma-aarch64-build.log', 'verify-shadow.Ab12Cd34/host-full.txt'})


# THE LOGS LOCK, through the real entry points over a fixture repository. `lab.py` finds its tree
# from its own path, so copies of it, `lib.sh` and `dev.sh` in a temporary directory sweep that
# directory's `.build/logs` and nothing of this tree's.
class LogsLockTest(unittest.TestCase):
	def setUp(self):
		self.directory = tempfile.TemporaryDirectory()
		self.addCleanup(self.directory.cleanup)
		self.repo = self.directory.name
		repo = os.path.dirname(os.path.dirname(HERE))
		os.makedirs(os.path.join(self.repo, 'src', 'harness'))
		for name in ('lib.sh', 'dev.sh'):
			shutil.copy2(os.path.join(repo, name), os.path.join(self.repo, name))
		os.makedirs(os.path.join(self.repo, 'src', 'tools'))
		shutil.copy2(os.path.join(repo, 'src', 'tools', 'staging-state.sh'), os.path.join(self.repo, 'src', 'tools', 'staging-state.sh'))
		self.lab = os.path.join(self.repo, 'src', 'harness', 'lab.py')
		shutil.copy2(os.path.join(HERE, 'lab.py'), self.lab)
		with open(os.path.join(self.repo, 'check.sh'), 'w') as handle:
			handle.write('declare -A GATES=(\n\t["qemu-2d-demo"]="tools/check-qemu-2d-demo.sh"\n)\n')
		subprocess.run(['git', 'init', '-q', self.repo], check=True)
		self.logs = os.path.join(self.repo, '.build', 'logs')
		self.lock = os.path.join(self.repo, '.build', 'state', 'logs.lock')
		# A run this one is part of must not make the fixture's runs nested ones.
		self.env = {name: value for name, value in os.environ.items() if name not in ('LIBER_LOGS_RUN', 'RUN_STATUS_FILE', 'LIBER_DEV_STATE')}

	def dead_pid(self):
		ended = subprocess.Popen(['true'])
		ended.wait()
		return ended.pid

	def hold(self):
		os.makedirs(os.path.dirname(self.lock), exist_ok=True)
		handle = os.open(self.lock, os.O_RDWR | os.O_CREAT)
		fcntl.flock(handle, fcntl.LOCK_SH)
		return handle

	# The holders a run's `dev-clean --run` left behind: forks of it, so named by its command line.
	def holders(self):
		found = []
		for pid in filter(str.isdigit, os.listdir('/proc')):
			try:
				with open(f'/proc/{pid}/cmdline', 'rb') as handle:
					if self.lab.encode() in handle.read():
						found.append(int(pid))
			except OSError:
				pass
		return found

	def settled(self):
		for _ in range(300):
			if not self.holders():
				return
			time.sleep(0.1)
		self.fail('the holder outlived its run')

	def begin(self, owner):
		return subprocess.run([sys.executable, self.lab, 'dev-clean', '--run', str(owner)], capture_output=True, text=True, timeout=120, env=self.env)

	# The break: no lock at all - every run swept, and so would a run beside a live one, removing an
	# ended run's logs before that run's gate had read them.
	def test_a_run_sweeps_nothing_while_another_run_holds_the_lock(self):
		old = write_suite_run(self.logs, 'x86_64', '20261001T000000Z', self.dead_pid())
		write_suite_run(self.logs, 'x86_64', '20261002T000000Z', self.dead_pid())
		handle = self.hold()
		try:
			owner = subprocess.Popen(['sleep', '60'])
			begun = self.begin(owner.pid)
			self.assertEqual(begun.returncode, 0, begun.stderr)
			self.assertIn('another run is alive', begun.stderr)
			owner.kill()
			owner.wait()
			self.settled()
			self.assertTrue(all(os.path.exists(path) for path in old), 'a run swept beside another run that was alive')
		finally:
			os.close(handle)
		begun = self.begin(self.dead_pid())
		self.assertEqual(begun.returncode, 0, begun.stderr)
		self.settled()
		self.assertFalse(any(os.path.exists(path) for path in old), 'a run that is alone did not sweep')

	def test_a_hand_clean_refuses_while_a_run_holds_the_lock(self):
		old = write_suite_run(self.logs, 'aarch64', '20261001T000000Z', self.dead_pid())
		write_suite_run(self.logs, 'aarch64', '20261002T000000Z', self.dead_pid())
		handle = self.hold()
		try:
			clean = subprocess.run([os.path.join(self.repo, 'dev.sh'), 'clean'], capture_output=True, text=True, timeout=120, env=self.env)
		finally:
			os.close(handle)
		self.assertNotEqual(clean.returncode, 0)
		self.assertIn('holds the logs lock', clean.stderr)
		self.assertTrue(all(os.path.exists(path) for path in old))
		clean = subprocess.run([os.path.join(self.repo, 'dev.sh'), 'clean'], capture_output=True, text=True, timeout=120, env=self.env)
		self.assertEqual(clean.returncode, 0, clean.stderr)
		self.assertFalse(any(os.path.exists(path) for path in old))

	# The automatic sweep is the logs rules alone. A build started by hand holds no lock, and it
	# moves a new object generation into place before it records the reference to it - so the
	# object rule, run on its own, could remove the object a build is about to name.
	def test_the_automatic_sweep_touches_nothing_outside_the_logs(self):
		cache = os.path.join(self.repo, '.build', 'cache', 'x86_64')
		os.makedirs(cache)
		current, superseded = 'a' * 64, 'b' * 64
		with open(os.path.join(cache, 'executable-tool.object'), 'w') as handle:
			handle.write(f'key={current}\n')
		for key in (current, superseded):
			open(os.path.join(cache, f'object-tool-{key}.o'), 'w').close()
		scratch = os.path.join(self.repo, '.build', 'tmp', 'killed-build')
		os.makedirs(scratch)
		os.utime(scratch, (time.time() - 3 * 86400, time.time() - 3 * 86400))
		old = write_suite_run(self.logs, 'riscv64', '20261001T000000Z', self.dead_pid())
		write_suite_run(self.logs, 'riscv64', '20261002T000000Z', self.dead_pid())
		begun = self.begin(self.dead_pid())
		self.assertEqual(begun.returncode, 0, begun.stderr)
		self.settled()
		self.assertFalse(any(os.path.exists(path) for path in old), 'the automatic sweep did not run')
		self.assertTrue(os.path.exists(os.path.join(cache, f'object-tool-{superseded}.o')), 'the automatic sweep removed an object generation')
		self.assertTrue(os.path.exists(scratch), 'the automatic sweep removed build scratch')
		clean = subprocess.run([os.path.join(self.repo, 'dev.sh'), 'clean'], capture_output=True, text=True, timeout=120, env=self.env)
		self.assertEqual(clean.returncode, 0, clean.stderr)
		self.assertFalse(os.path.exists(os.path.join(cache, f'object-tool-{superseded}.o')), 'the hand clean lost its object rule')
		self.assertTrue(os.path.exists(os.path.join(cache, f'object-tool-{current}.o')))

	# Ten top-level runs of one gate that makes a folder per run, each through `lib.sh` as `check.sh`
	# starts one: the sweep at each start keeps the newest set, and the one when each run ends keeps
	# that run's own - one set, where there were ten.
	def test_ten_runs_of_one_gate_leave_one_set(self):
		gate = os.path.join(self.repo, 'gate.sh')
		with open(gate, 'w') as handle:
			handle.write(
				'#!/usr/bin/env bash\n'
				'SCRIPT_NAME=gate.sh\n'
				'source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"\n'
				'arm_run_verdict\n'
				'logs_run_begin\n'
				'mkdir -p "$BUILD_DIR/logs"\n'
				'proof="$(mktemp -d "$BUILD_DIR/logs/qemu-2d-demo.XXXXXX")"\n'
				'echo frame >"$proof/demo.log"\n'
			)
		os.chmod(gate, 0o755)
		for _ in range(10):
			run = subprocess.run([gate], capture_output=True, text=True, timeout=120, env=self.env)
			self.assertEqual(run.returncode, 0, run.stderr)
			self.settled()
		self.assertEqual(len([name for name in os.listdir(self.logs) if name.startswith('qemu-2d-demo.')]), 1)


if __name__ == '__main__':
	unittest.main(verbosity=2)
