#!/usr/bin/env python3
# The real SBI SUSP entry on OpenSBI's documented test backend, and its absent-firmware fallback.
# The backend retains devices and waits before its WFI; this proves the firmware handshake, saved-hart
# return and RTC wake, not physical power removal. No host command wakes either guest.

import importlib.util
import os
import re
import signal
import socket
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location('tickless', os.path.join(HERE, 'check-tickless-idle.py'))
tickless = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tickless)
lab = tickless.lab
GateError = tickless.GateError

TARGET = 'riscv64'
SMP = 4
WAKE_S = 8  # Longer than OpenSBI's test backend's five-second preparatory wait.
MARGIN_MS = 300
ENTERED = 'sleep: entered (firmware system suspend,'
STOPPED = 'sleep: system suspend - every other core is stopped'
BACK = f'sleep: system suspend - all {SMP} cores are running again'
REFUSAL = 'sleep: SBI system suspend refused (-3)'
UNWOUND = 'sleep: system suspend refused - every stopped core and the wake route restored'
RESUMED = re.compile(r'sleep: resumed \(the RTC alarm, after (\d+) ms, at tick (\d+)\)')
TIMER = re.compile(r'timer (armed for 1000 ms|fired) mono-ms (\d+) boot-ms (\d+)')
CLOCKS = re.compile(r'sleepcheck: clocks mono-ms \d+ boot-ms \d+ rtc (\d+)')
LAST_SLEEP = re.compile(r'slept (\d+) ms and was woken by the RTC alarm')


def note(message):
	print(f'check-system-suspend: {message}', flush=True)


def require(condition, message):
	if not condition:
		raise GateError(message)


# Pure oracle, also exercised by --self-test. Check clocks across the same adjacent counter pair
# that straddles the entry, not an unrelated maximum gap elsewhere in the transcript.
def check_sleep(lines, record):
	# Serial.lines_since removes LF but retains the CR from the wire's CRLF terminator.
	# Normalize that terminator only; the ordered core acknowledgments still match whole lines.
	lines = [(stamp, line.removesuffix('\r')) for stamp, line in lines]
	entries = [(at, stamp, int(found.group(1))) for at, (stamp, line) in enumerate(lines) if ENTERED in line and (found := re.search(r'at tick (\d+)\)', line))]
	resumes = [(at, stamp, int(found.group(2)), int(found.group(1))) for at, (stamp, line) in enumerate(lines) if (found := RESUMED.search(line))]
	require(len(entries) == len(resumes) == 1, 'one real firmware entry and one RTC resume are required')
	(entry_at, entered, _), (resume_at, resumed, _, reported) = entries[0], resumes[0]
	require(entry_at < resume_at, 'resume preceded the firmware entry')
	window = [line for _, line in lines[entry_at:resume_at + 1]]
	require(window.count(STOPPED) == window.count(BACK) == 1, 'the firmware entry or the saved-core return was not acknowledged exactly once')
	require(window.index(STOPPED) < window.index(BACK), 'the saved cores returned before they stopped')
	require(not any(REFUSAL in line or UNWOUND in line for line in window), 'a refused transition was reported as a sleep')
	elapsed = (resumed - entered) * 1000
	require(elapsed >= WAKE_S * 1000 - MARGIN_MS, f'the host measured only {elapsed:.0f} ms for the {WAKE_S} s alarm')
	require(reported >= WAKE_S * 1000 - MARGIN_MS, f'the RTC measured only {reported} ms asleep')
	# The host interval includes controller/core restoration after the RTC sample. It can be longer,
	# but an RTC duration longer than the whole host interval would be fabricated sleep time.
	require(reported <= elapsed + MARGIN_MS, f'the host measured {elapsed:.0f} ms but the RTC reported {reported} ms')
	rows = []
	for at, (stamp, line) in enumerate(lines):
		if found := tickless.COUNTER.search(tickless.PROMPT_TEXT.sub('', line)):
			rows.append((at, stamp, *map(int, found.groups())))
	require(len(rows) >= 2 and [row[2] for row in rows] == list(range(1, len(rows) + 1)), 'the counter skipped, repeated, restarted, or did not begin at one')
	require(not any(entry_at < row[0] < resume_at for row in rows), 'the counter ran while the machine slept')
	_, _, mono, boot = tickless.sleep_clock_gap(rows, entries[0], resumes[0][:3], reported, MARGIN_MS)
	last = LAST_SLEEP.findall(record)
	require(len(last) == 1 and int(last[0]) == reported, 'the last record does not match this successful RTC wake')
	return elapsed, mono, boot


def check_refusal(text):
	markers = [ENTERED, STOPPED, 'sleep: development fixture asks SBI system suspend for a reserved sleep type', REFUSAL, BACK, UNWOUND]
	position = -1
	for marker in markers:
		position = text.find(marker, position + 1)
		require(position >= 0, f'firmware refusal lacks ordered evidence: {marker}')
	require('sleep: resumed (' not in text, 'the failed firmware call was credited as a successful sleep')
	require('unwound at the entry' in text, 'ServiceManager did not unwind the refused firmware entry')


def check_core_progress(before, after):
	require(set(before) == set(after) == set(range(SMP)), 'the running machine does not report all four cores')
	for cpu in range(SMP):
		require(after[cpu]['halts'] > before[cpu]['halts'], f'core {cpu} never returned to its idle loop after the transition')


def check_frame(before, after):
	from PIL import ImageChops
	require(before.size == after.size, 'the display geometry changed during the keyboard check')
	bounds = ImageChops.difference(before.convert('RGB'), after.convert('RGB')).getbbox()
	# The console's cell is eight pixels wide (term::CELL_W). Four typed letters must change more
	# than one cell; a caret blink alone must not count as a frame displaying the keyboard input.
	require(bounds is not None and bounds[2] - bounds[0] > 8, 'the emulated keyboard changed no text beyond the caret after wake')


def self_test():
	tickless.clock_self_test()
	lines = [(0.0, 'count 1 mono-ms 100 boot-ms 100'), (0.1, ENTERED + ' at tick 1)'), (0.2, STOPPED), (8.09, BACK), (8.1, 'sleep: resumed (the RTC alarm, after 8000 ms, at tick 1)'), (8.2, 'count 2 mono-ms 200 boot-ms 8200')]
	record = 'slept 8000 ms and was woken by the RTC alarm'
	check_sleep(lines, record)
	check_sleep([(stamp, line + '\r') for stamp, line in lines], record + '\r')
	refusal = '\n'.join([ENTERED, STOPPED, 'sleep: development fixture asks SBI system suspend for a reserved sleep type', REFUSAL, BACK, UNWOUND, 'unwound at the entry'])
	check_refusal(refusal)
	cores_before = {cpu: {'halts': 10} for cpu in range(SMP)}
	cores_after = {cpu: {'halts': 11} for cpu in range(SMP)}
	check_core_progress(cores_before, cores_after)
	from PIL import Image
	frame = Image.new('RGB', (32, 16))
	typed = frame.copy()
	typed.paste('white', (0, 0, 24, 16))
	caret = frame.copy()
	caret.paste('white', (0, 0, 8, 16))
	check_frame(frame, typed)
	cases = [
		('missing stop', lambda: check_sleep([row for row in lines if row[1] != STOPPED], record)),
		('missing core return', lambda: check_sleep([row for row in lines if row[1] != BACK], record)),
		('core return before stop', lambda: check_sleep([(stamp, BACK if line == STOPPED else STOPPED if line == BACK else line) for stamp, line in lines], record)),
		('early wake', lambda: check_sleep([(stamp / 2, line) for stamp, line in lines], record)),
		('invented RTC duration', lambda: check_sleep([(stamp, line.replace('after 8000 ms', 'after 16000 ms')) for stamp, line in lines], record)),
		('counter inside sleep', lambda: check_sleep(lines[:3] + [(4.0, 'count 2 mono-ms 150 boot-ms 4150')] + lines[3:], record)),
		('clock includes sleep', lambda: check_sleep(lines[:-1] + [(8.2, 'count 2 mono-ms 8200 boot-ms 8200')], record)),
		('wrong last wake', lambda: check_sleep(lines, 'slept and woke, woken by the platform')),
		('stale last duration', lambda: check_sleep(lines, record.replace('8000', '1'))),
		('static core inventory', lambda: check_core_progress(cores_before, {**cores_after, 3: {'halts': 10}})),
		('unchanged display', lambda: check_frame(frame, frame)),
		('caret-only frame', lambda: check_frame(frame, caret)),
		('fake refusal', lambda: check_refusal(refusal.replace(REFUSAL, 'firmware feature absent'))),
		('refusal without recovery', lambda: check_refusal(refusal.replace(BACK, ''))),
		('refusal in wrong step', lambda: check_refusal(refusal.replace('unwound at the entry', 'unwound at the drivers'))),
	]
	for name, case in cases:
		try:
			case()
		except GateError:
			continue
		raise GateError(f'oracle accepted {name}')
	note(f'oracle self-test PASS (LF/CRLF sleep, refusal, core progress and display positives; {len(cases)} rejected false outcomes)')


def all_cores(serial):
	rows = tickless.cores(serial, 600)
	require(set(rows) == set(range(SMP)), 'the running machine does not report all four cores')
	return rows


def after_wake(serial, qmp_path, work, label, before_cores):
	output = serial.run('ping -c 2 10.0.2.2', 300)
	require('2 packets transmitted, 2 received' in output, 'the network did not answer after the wake')
	output = serial.run(f'cat suspend-{label}.txt', 300)
	require(f'system-suspend-marker-{label}' in output, 'the file written before sleep did not read back')
	output = serial.run('uname', 300)
	require(re.search(r'LiberSystem\s+\S+\s+riscv64', output), 'the serial shell did not answer uname after wake')
	mark = len(serial.data)
	serial.run('sleepcheck timer 1000', 300)
	rows = [(stamp, found.group(1), int(found.group(2))) for stamp, line in serial.lines_since(mark) if (found := TIMER.search(line))]
	require(len(rows) == 2 and rows[0][1].startswith('armed') and rows[1][1] == 'fired', 'the post-wake timer did not arm and fire')
	waited = (rows[1][0] - rows[0][0]) * 1000
	require(800 <= waited <= 1600 and 800 <= rows[1][2] - rows[0][2] <= 1600, f'the post-wake one-second timer took {waited:.0f} ms')
	mark = len(serial.data)
	serial.run('sleepcheck clocks', 300)
	clocks = [(stamp, int(found.group(1))) for stamp, line in serial.lines_since(mark) if (found := CLOCKS.search(line))]
	require(len(clocks) == 1 and abs(clocks[0][0] - clocks[0][1]) <= 2, 'the post-wake RTC differs from the host by over two seconds')
	# QMP types on the emulated keyboard, exactly as the ordinary sleep gate does. Serial input
	# alone cannot prove that a frame reaches the display. Clear the unsubmitted text afterwards.
	before, after = [os.path.join(work, f'{label}-{part}.ppm') for part in ('before', 'after')]
	# The RISC-V runner also has ramfb for the boot log, which is QEMU's primary display. Select
	# the named virtio GPU DisplayService actually adopted; the default captures a stale boot log.
	tickless.qmp(qmp_path, 'screendump', {'filename': before, 'device': 'system-suspend-display', 'head': 0}, 30)
	for key in 'wake':
		tickless.qmp(qmp_path, 'send-key', {'keys': [{'type': 'qcode', 'data': key}], 'hold-time': 100}, 30)
		until = time.monotonic() + 0.2
		while time.monotonic() < until:
			serial.pump(0.05)
	until = time.monotonic() + 5
	while time.monotonic() < until:
		serial.pump(0.1)
	tickless.qmp(qmp_path, 'screendump', {'filename': after, 'device': 'system-suspend-display', 'head': 0}, 30)
	from PIL import Image
	with Image.open(before) as first, Image.open(after) as second:
		check_frame(first, second)
	for _ in 'wake':
		tickless.qmp(qmp_path, 'send-key', {'keys': [{'type': 'qcode', 'data': 'backspace'}], 'hold-time': 100}, 30)
		until = time.monotonic() + 0.2
		while time.monotonic() < until:
			serial.pump(0.05)
	until = time.monotonic() + 1
	while time.monotonic() < until:
		serial.pump(0.1)
	check_core_progress(before_cores, all_cores(serial))
	note(f'{label}: ping, file, serial, display, all cores and a {waited:.0f} ms one-second timer passed')


def ram_sleep(serial, qmp_path, work, label):
	output = serial.run(f'write suspend-{label}.txt system-suspend-marker-{label}', 300)
	require(re.search('wrote|written', output, re.I), 'the before-sleep file was not written')
	before_cores = all_cores(serial)
	mark = len(serial.data)
	serial.type(b'sleepcheck count 30 &\n')
	serial.wait_for(mark, lambda seen: re.search(rb'sleepcheck: count 1 mono-ms \d+ boot-ms \d+\r?\n', seen), 600, 'the background counter')
	serial.type(f'sleepctl suspend ram {WAKE_S}\n'.encode())
	serial.wait_for(mark, lambda seen: b'ServiceManager: sleep: the transaction ended' in seen, 3000, 'the system suspend transaction')
	serial.wait_for(mark, lambda seen: b'sleepcheck: count done' in seen, 1200, 'the counter finishing after thaw')
	lines = serial.lines_since(mark)
	with open(os.path.join(work, f'{label}-stamped.log'), 'w') as log:
		for stamp, line in lines:
			log.write(f'{stamp:.6f} {line}\n')
	require(sum(tickless.COUNTER.search(tickless.PROMPT_TEXT.sub('', line)) is not None for _, line in lines) == 300, 'the thirty-second counter did not publish all 300 values')
	record = serial.run('sleepctl last', 600)
	elapsed, mono, boot = check_sleep(lines, record)
	note(f'{label}: RTC wake, host {elapsed:.0f} ms, counter silent; monotonic +{mono} ms, boot-time +{boot} ms')
	after_wake(serial, qmp_path, work, label, before_cores)


def check(serial, qmp_path, work, fixture):
	serial.wait_prompt(0, 1800, 'the RISC-V boot')
	boot = serial.text_since(0).decode(errors='replace')
	offer = 'offered' if fixture else 'not offered'
	require(f'the SBI System Suspend extension is {offer} by this firmware' in boot, f'the firmware did not report SUSP {offer}')
	status = serial.run('sleepctl status', 600)
	states = re.search(r'^states: ([^\r\n]*)', status, re.M)
	require(states is not None and ('suspend to RAM' in states.group(1)) == fixture, f'RAM admission disagrees with this boot: {status}')
	before_cores = all_cores(serial)
	mark = len(serial.data)
	serial.run(f'sleepctl suspend ram {WAKE_S}', 3000)
	text = serial.text_since(mark).decode(errors='replace')
	with open(os.path.join(work, f'{"fixture" if fixture else "unsupported"}-refusal-stamped.log'), 'w') as log:
		for stamp, line in serial.lines_since(mark):
			log.write(f'{stamp:.6f} {line}\n')
	if fixture:
		check_refusal(text)
		check_core_progress(before_cores, all_cores(serial))
		for label in ('first', 'repeat'):
			ram_sleep(serial, qmp_path, work, label)
	else:
		require('refused' in text and ENTERED not in text, 'unsupported firmware did not refuse RAM before entering')
	# The ordinary sleep path must remain usable after rejection and after repeated saved-hart resumes.
	tickless.suspend_to_idle(TARGET, serial, 10.0)
	all_cores(serial)


def drive(work, fixture):
	label = 'fixture' if fixture else 'unsupported'
	serial_path = os.path.join(lab.BUILD, 'system-suspend-riscv64.sock')
	qmp_path = os.path.join(lab.BUILD, 'qemu-qmp-cold-riscv64.sock')
	if os.path.exists(serial_path):
		os.unlink(serial_path)
	kernel = os.path.join(lab.BUILD_ROOT, 'cargo', 'kernel', tickless.TRIPLES[TARGET], 'debug', 'kernel')
	env = dict(os.environ, LIBER_DEVELOPMENT='1', DEV_PROFILE='1', COLD='1', SERIAL=f'unix:{serial_path},server', SMP=str(SMP), LIBER_RUN_MODE='development', UEFI='1')
	for name in ('SYSTEM_SUSPEND_FIXTURE', 'IDLE_FIXTURE', 'I2C_FIXTURE', 'DMA_DTB_NODE', 'QEMU_EXTRA'):
		env.pop(name, None)
	if fixture:
		env.update(SYSTEM_SUSPEND_FIXTURE='1', QEMU_EXTRA='-fw_cfg name=opt/org.libersystem/absent,string=system-suspend-entry')
	note(f'booting {label}; logs in {work}')
	with open(os.path.join(work, f'{label}-runner.log'), 'wb') as runner, open(os.path.join(work, f'{label}-serial.log'), 'wb') as log:
		guest = subprocess.Popen(['bash', 'harness/qemu-run.sh', TARGET, kernel], cwd=SRC, env=env, stdout=runner, stderr=runner, start_new_session=True)
		sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
		serial = None
		try:
			deadline = time.monotonic() + 300
			while True:
				require(guest.poll() is None, f'the {label} guest exited before opening serial (see {runner.name})')
				try:
					sock.connect(serial_path)
					break
				except OSError:
					require(time.monotonic() < deadline, 'the guest never opened serial')
					time.sleep(0.5)
			serial = tickless.Serial(sock, log, 10.0)
			check(serial, qmp_path, work, fixture)
		finally:
			try:
				if serial is not None:
					with open(os.path.join(work, f'{label}-stamped.log'), 'w') as stamped:
						for stamp, line in serial.lines_since(0):
							stamped.write(f'{stamp:.6f} {line}\n')
			finally:
				sock.close()
				try:
					os.killpg(guest.pid, signal.SIGTERM)
					guest.wait(timeout=20)
				except (OSError, subprocess.TimeoutExpired):
					if guest.poll() is None:
						os.killpg(guest.pid, signal.SIGKILL)
					guest.wait()
				if os.path.exists(serial_path):
					os.unlink(serial_path)


def main():
	args = sys.argv[1:]
	if any(arg not in ('--no-build', '--self-test') for arg in args):
		print('usage: check-system-suspend.py [--no-build] [--self-test]', file=sys.stderr)
		return 2
	# The direct Python entry point is used with --no-build too. SIGTERM must unwind drive's finally
	# rather than leave its separate QEMU process group alive until another gate finds locked disks.
	def interrupted(number, _frame):
		raise GateError(f'interrupted by {signal.Signals(number).name}')
	for number in (signal.SIGINT, signal.SIGTERM):
		signal.signal(number, interrupted)
	try:
		self_test()
		if '--self-test' in args:
			return 0
		if '--no-build' not in args:
			tickless.build(TARGET)
		work = os.path.join(lab.BUILD_ROOT, 'logs', 'system-suspend-riscv64')
		os.makedirs(work, exist_ok=True)
		for fixture in (False, True):
			drive(work, fixture)
	except (GateError, OSError) as error:
		print(f'check-system-suspend: FAIL - {error}', file=sys.stderr, flush=True)
		return 1
	note('PASS: unsupported firmware fallback, real SBI refusal recovery and two RTC system resumes')
	return 0


if __name__ == '__main__':
	sys.exit(main())
