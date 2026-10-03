#!/usr/bin/env python3
# check-hibernate-ports.py - hibernation on the device-tree ports, end to end: aarch64 and riscv64, each booting its
# development build from a system disk SET UP FOR IT - a GPT whose first partition is the system volume and whose second
# is a hibernation partition a little larger than the guest's memory - first with `swtpm` behind QEMU's
# `tpm-tis-device`, which the device tree describes as a `tcg,tpm-tis-mmio` node, and then with no TPM: the owner's
# "hibernation without a TPM", allowed with a warning, the image's key written beside it in the clear.
#
# THE ORACLES ARE THE HOST'S. Neither port has an S4, so a hibernation ends in the firmware's power-off and the guest's
# QEMU exits; the image's header is read from the disk file between the boots - its magic, its state, and how its key is
# kept. On each target:
#
#   1. SET UP, SEALED: the kernel says hibernation is offered (every core's context through the firmware), and with the
#      TPM bound `sleepctl status` says it is set up, with no warning.
#   2. HIBERNATE AND RESTORE. A counter runs in the background at the serial shell; `sleepctl hibernate`; the image is
#      written with its key sealed through the TPM - the header says so - and QEMU exits. A new boot on the same disk and
#      the same TPM unseals the key, puts the image in the kernel, replaces memory on the core the image's boot core was,
#      and the image's kernel resumes: every other core turned on again at its record, the transaction ending "slept and
#      woke" by the restore, the header invalidated - and the counter goes on at its next value, one unbroken sequence,
#      its boot-time clock moved by at least half the time off and by exactly the sleep the kernel reported more than its
#      monotonic clock: the same program, the same counter, on a machine all of whose cores came back.
#   3. THE BOOT AFTER A RESUME, WITH NO TPM, sees no image, and says hibernation is set up with the warning that no TPM
#      seals its key. A counter again, and an image written with its key in the clear.
#   4. RESTORED IN THE CLEAR: the next boot, still with no TPM, takes the key from the header and the counter goes on as
#      in 2. The restored machine hibernates again.
#   5. A MODIFIED IMAGE: that image with one byte of a chunk flipped on the host - refused as modified, the header
#      invalidated, and the machine boots fresh.
#
# ONE GUEST AT A TIME: each boot is torn down before the next one, and each target before the next.
#
# usage: check-hibernate-ports.py [--no-build] [aarch64|riscv64 ...]   (default: both)

import importlib.util
import os
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(SRC, 'harness'))
import lab  # noqa: E402

# THE TICKLESS GATE'S SERIAL LINE AND BUILD, reused rather than copied: the same guest, the same shell, driven the same way.
_spec = importlib.util.spec_from_file_location('tickless', os.path.join(HERE, 'check-tickless-idle.py'))
tickless = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tickless)
Serial = tickless.Serial
GateError = tickless.GateError

TARGETS = ('aarch64', 'riscv64')
TRIPLES = tickless.TRIPLES
# THE GUEST: memory the hibernation partition exceeds, and the four cores every boot of a target has.
MEM_MIB = 1024
HIBERNATION_MIB = MEM_MIB + 64
SMP = 4
COUNTER_S = 60
LIBERFS_TYPE = '4C424653-0001-4000-8000-4C6962657246'
HIBERNATION_TYPE = '4C424653-0002-4000-8000-4C6962657246'
WARNING = "WARNING: no TPM is bound to seal the image's key, so the image's key is written beside it in the clear"


def note(message):
	print(f'check-hibernate-ports: {message}', flush=True)


# ------------------------------------------------------------------ the disk

class Disk:
	def __init__(self, target, work):
		self.path = os.path.join(work, f'hibernate-disk-{target}.img')
		volume = os.path.join(lab.BUILD, f'system-volume-{target}.img')
		if not os.path.exists(volume):
			raise GateError(f'no system volume at {volume}')
		volume_mib = (os.path.getsize(volume) + 1048575) // 1048576 + 64
		if os.path.exists(self.path):
			os.unlink(self.path)
		with open(self.path, 'wb') as disk:
			disk.truncate((volume_mib + HIBERNATION_MIB + 4) * 1048576)
		for args in (['-n', f'1:2048:+{volume_mib}M', '-t', f'1:{LIBERFS_TYPE}', '-c', '1:system'], ['-n', f'2:0:+{HIBERNATION_MIB}M', '-t', f'2:{HIBERNATION_TYPE}', '-c', '2:hibernation']):
			if subprocess.run(['sgdisk', self.path, *args], stdout=subprocess.DEVNULL).returncode != 0:
				raise GateError(f'sgdisk could not write the partition table ({args})')
		with open(volume, 'rb') as source, open(self.path, 'r+b') as disk:
			disk.seek(1048576)
			while chunk := source.read(1 << 20):
				disk.write(chunk)
		info = subprocess.run(['sgdisk', '-i', '2', self.path], capture_output=True, text=True).stdout
		first = re.search(r'First sector: (\d+)', info)
		if not first:
			raise GateError(f'sgdisk did not say where the hibernation partition starts: {info}')
		self.area = int(first.group(1)) * 512

	def header(self):
		with open(self.path, 'rb') as disk:
			disk.seek(self.area)
			block = disk.read(24)
		if block[:8] != b'LSHIBRN1':
			return 'none'
		return {1: 'image', 0: 'empty'}.get(struct.unpack_from('<I', block, 12)[0], 'unknown')

	def protection(self):
		with open(self.path, 'rb') as disk:
			disk.seek(self.area)
			block = disk.read(24)
		return {1: 'tpm', 2: 'clear'}.get(struct.unpack_from('<I', block, 20)[0], 'unknown')

	# ONE BYTE OF CHUNK 2's DATA flipped: the image's authentication must fail.
	def modify(self):
		at = self.area + 4096 + 2 * (4096 + 256 * 4096) + 4096 + 1234
		with open(self.path, 'r+b') as disk:
			disk.seek(at)
			byte = disk.read(1)
			disk.seek(at)
			disk.write(bytes([byte[0] ^ 0x40]))


# ------------------------------------------------------------------ the TPM

# `swtpm` FOR ONE BOOT, on a state directory kept across the boots that share it: started before QEMU, stopped after it,
# so every boot's PCRs start from a reset as a machine's do.
class Tpm:
	def __init__(self, state):
		self.state = state
		self.socket = os.path.join(state, 'swtpm.sock')
		self.process = None
		os.makedirs(state, exist_ok=True)

	def start(self):
		if os.path.exists(self.socket):
			os.unlink(self.socket)
		log = os.path.join(self.state, 'swtpm.log')
		self.process = subprocess.Popen(['swtpm', 'socket', '--tpm2', '--tpmstate', f'dir={self.state}', '--ctrl', f'type=unixio,path={self.socket}', '--log', f'file={log}'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
		deadline = time.monotonic() + 10
		while not os.path.exists(self.socket):
			if self.process.poll() is not None or time.monotonic() > deadline:
				raise GateError(f'swtpm did not open its control socket (see {log})')
			time.sleep(0.1)

	def stop(self):
		if self.process is not None:
			self.process.terminate()
			try:
				self.process.wait(timeout=10)
			except subprocess.TimeoutExpired:
				self.process.kill()
				self.process.wait()
			self.process = None


# ------------------------------------------------------------------ one boot

class Guest:
	def __init__(self, target, disk, label, work, tpm=None):
		self.target = target
		self.tpm = tpm
		self.scale = 10.0
		self.serial_path = os.path.join(lab.BUILD, f'hibernate-serial-{target}.sock')
		self.log_path = os.path.join(work, f'hibernate-{target}-{label}.log')
		self.runner_log = open(os.path.join(work, f'hibernate-{target}-{label}-runner.log'), 'wb')
		kernel = os.path.join(lab.BUILD_ROOT, 'cargo', 'kernel', TRIPLES[target], 'debug', 'kernel')
		if os.path.exists(self.serial_path):
			os.unlink(self.serial_path)
		# AN ENTROPY DEVICE: neither port has a random instruction, and the image's keys are drawn from the kernel's pool.
		env = dict(os.environ, LIBER_DEVELOPMENT='1', DEV_PROFILE='1', COLD='1', SERIAL=f'unix:{self.serial_path},server', SMP=str(SMP), MEM=f'{MEM_MIB}M', RUN_DISK=disk.path, LIBER_RUN_MODE='development', UEFI='1', ENTROPY='1')
		env.pop('TPM_SOCKET', None)
		env.pop('TPM_FRONTEND', None)
		if tpm is not None:
			tpm.start()
			env.update(TPM_SOCKET=tpm.socket, TPM_FRONTEND='tis')
		note(f'{target}: boot {label}{" with the TPM" if tpm else " with no TPM"}; serial log {self.log_path}')
		self.process = subprocess.Popen(['bash', 'harness/qemu-run.sh', target, kernel], cwd=SRC, env=env, stdout=self.runner_log, stderr=self.runner_log, start_new_session=True)
		self.log = open(self.log_path, 'wb')
		sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
		deadline = time.monotonic() + 300
		while True:
			try:
				if self.process.poll() is not None:
					raise GateError(f'{label}: the guest exited before it opened its serial line (see {self.runner_log.name})')
				if time.monotonic() > deadline:
					raise GateError(f'{label}: the guest never opened its serial line')
			except GateError:
				self.close()
				raise
			try:
				sock.connect(self.serial_path)
				break
			except OSError:
				time.sleep(0.5)
		self.serial = Serial(sock, self.log, self.scale)

	def boot_to_shell(self, what):
		self.serial.wait_prompt(0, 1800, what)

	def seen(self, needle):
		return needle.encode() in lab.strip_ansi(bytes(self.serial.data))

	def wait_line(self, needle, timeout, what, since=0):
		self.serial.wait_for(since, lambda text: needle.encode() in text, timeout, what)

	# THE GUEST GONE: QEMU exits at the firmware's power-off, and so does the runner that exec'd it.
	def wait_exit(self, timeout, what):
		end = time.monotonic() + timeout
		while self.process.poll() is None:
			if time.monotonic() > end:
				raise GateError(f'{what}: QEMU was still running {timeout:.0f} s on')
			try:
				self.serial.pump(0.5)
			except GateError:
				time.sleep(0.5)

	def close(self):
		try:
			os.killpg(self.process.pid, signal.SIGTERM)
			self.process.wait(timeout=20)
		except (OSError, subprocess.TimeoutExpired):
			try:
				os.killpg(self.process.pid, signal.SIGKILL)
			except OSError:
				pass
			self.process.wait()
		if self.tpm is not None:
			self.tpm.stop()
		self.log.close()
		self.runner_log.close()


# THE COUNTER ACROSS THE TIME OFF, read from its file once it has ended: one unbroken run of values from 1, whose widest
# step of the boot-time clock is the time off, at least `off_ms`, and across which the boot-time clock moved by EXACTLY
# the sleep the kernel reported more than the monotonic clock did - to the RTC's second. Not a bound on the monotonic
# step itself: under emulation the transaction around the time off takes it tens of seconds, and the counter's own
# output, waiting on the volume behind its redirection, can hold the counter longer than that before the freeze.
COUNT = re.compile(r'sleepcheck: count (\d+) mono-ms (\d+) boot-ms (\d+)')
# THE FILE READ THROUGH `grep`, which writes a line at a time, so a line the serial console mixes in cuts one line of it
# and not a window; a read whose lines were cut is read again.
def counter_rows(guest, name):
	for _ in range(3):
		text = guest.serial.run(f'grep count {name}', 300)
		rows = [tuple(int(value) for value in found) for found in COUNT.findall(text)]
		if 'sleepcheck: count done' in text and [row[0] for row in rows] == list(range(1, len(rows) + 1)):
			break
	return rows


def counter_goes_on(guest, name, off_ms, slept_ms):
	end = time.monotonic() + (COUNTER_S * 2 + 120) * guest.scale
	while 'sleepcheck: count done' not in guest.serial.run(f'tail -n 2 {name}', 120):
		if time.monotonic() > end:
			raise GateError('the counter never finished, so its file was never published')
		time.sleep(5)
	rows = counter_rows(guest, name)
	if not rows or [row[0] for row in rows] != list(range(1, len(rows) + 1)):
		raise GateError('the counter did not run one unbroken sequence from 1 - it skipped, repeated or started again')
	gap = max(range(1, len(rows)), key=lambda at: rows[at][2] - rows[at - 1][2])
	mono, boot = rows[gap][1] - rows[gap - 1][1], rows[gap][2] - rows[gap - 1][2]
	if boot < off_ms:
		raise GateError(f'the boot-time clock moved {boot} ms at its widest step, under the {off_ms} ms off')
	if abs((boot - mono) - slept_ms) > 1000:
		raise GateError(f'across the time off the boot-time clock moved {boot} ms and the monotonic clock {mono} ms - not {slept_ms} ms apart, the sleep the kernel reported')
	return rows[gap - 1][0], rows[gap][0], mono, boot


# A HIBERNATION ASKED: the image written - its key sealed through the TPM, or in the clear with the warning - and the
# guest gone.
def hibernate(guest, disk, what, sealed):
	mark = len(guest.serial.data)
	guest.serial.type(b'sleepctl hibernate\n')
	guest.wait_line('HibernationService: the image is written', 900, f'{what}: the image', mark)
	# THE WHOLE LINE, which arrives a piece at a time: how its key is kept, and the warning where no TPM seals it.
	if sealed:
		guest.wait_line('chunks, its key sealed to PCR 4', 60, f'{what}: the image written with its key sealed', mark)
	else:
		guest.wait_line(f'its key NOT sealed - {WARNING}', 60, f'{what}: the image written with its key in the clear and the warning', mark)
	guest.wait_line('hibernate: the image is written - no S4 on this machine, so it is powered off', 300, f'{what}: the power-off', mark)
	guest.wait_exit(300, what)
	guest.close()
	kept = 'tpm' if sealed else 'clear'
	if disk.header() != 'image' or disk.protection() != kept:
		raise GateError(f'{what}: the partition does not hold an image whose key is kept {kept} ({disk.header()}, {disk.protection()})')


# `sleepctl status`, once the TPM's driver has come online where there is one - the status asks without waiting - until
# it says `want` (a line of the serial console's can land inside the tool's output, so it is asked up to three times).
def status(guest, tpm, want, what):
	if tpm is not None:
		guest.wait_line('driver.tpm: online', 1800, f'{what}: the TPM driver')
	for _ in range(3):
		lines = [line.strip() for line in guest.serial.run('sleepctl status', 300).splitlines()]
		if want(lines):
			return
	raise GateError(f'{what}: {lines}')


# A RESTORE: the image in the kernel, memory replaced and the image's kernel resumed, the transaction ended, every core
# back and the header invalidated - and the counter going on across the time off.
def restored(guest, disk, what, off_started, counter):
	guest.wait_line('HibernationService: the image is authenticated and in the kernel', 1800, f'{what}: the image')
	guest.wait_line('hibernate: replacing memory with the image', 600, f'{what}: the replacement')
	resumed = re.compile(rb'sleep: resumed \(the restore of a hibernation image, after (\d+) ms')
	guest.serial.wait_for(0, lambda text: resumed.search(text) is not None, 600, f'{what}: the resume')
	slept_ms = int(resumed.search(guest.serial.text_since(0)).group(1))
	off_ms = int((time.monotonic() - off_started) * 1000)
	guest.wait_line('ServiceManager: sleep: the transaction ended - slept and woke', 900, f'{what}: the transaction')
	if guest.seen('did not come back from the image') or guest.seen('was not turned on again'):
		raise GateError(f'{what}: a core did not come back from the image')
	guest.serial.type(b'\n')
	guest.boot_to_shell(f'{what}: the restored machine')
	if disk.header() != 'empty':
		raise GateError(f'{what}: the header was not invalidated before the jump ({disk.header()})')
	before, after, mono, boot = counter_goes_on(guest, counter, off_ms // 2, slept_ms)
	note(f'{guest.target}: {what} - count {before} then {after}: monotonic +{mono} ms, boot-time +{boot} ms, every core back')


def run(target, work):
	disk = Disk(target, work)
	state = os.path.join(work, f'tpm-{target}')
	shutil.rmtree(state, ignore_errors=True)
	tpm = Tpm(state)
	# 1 and 2, WITH THE TPM: set up with no warning; an image sealed, and restored.
	guest = Guest(target, disk, 'sealed', work, tpm)
	try:
		guest.boot_to_shell('the first boot')
		if not guest.seen('sleep: hibernation is offered'):
			raise GateError('the kernel did not say hibernation is offered')
		status(guest, tpm, lambda lines: 'hibernation: set up' in lines, 'with the TPM, the status does not say hibernation is set up, with no warning')
		guest.serial.type(f'sleepcheck count {COUNTER_S} > counter-sealed.txt &\n'.encode())
		time.sleep(5 * guest.scale)
		hibernate(guest, disk, 'sealed: hibernate', True)
	finally:
		guest.close()
	off_started = time.monotonic()
	note(f'{target}: hibernated - the image written with its key sealed through the TPM, and the machine off')
	guest = Guest(target, disk, 'sealed-restore', work, tpm)
	try:
		restored(guest, disk, 'sealed: restored', off_started, 'counter-sealed.txt')
	finally:
		guest.close()
	# 3, WITH NO TPM: no image after the resume; set up with the warning; an image in the clear.
	guest = Guest(target, disk, 'clear', work)
	try:
		guest.boot_to_shell('the boot after the resume')
		status(guest, None, lambda lines: 'the image found at this boot: none' in lines and any(line.startswith(f'hibernation: set up - {WARNING}') for line in lines), 'with no TPM after the resume, the status does not say no image was found and hibernation is set up with the warning')
		guest.serial.type(f'sleepcheck count {COUNTER_S} > counter-clear.txt &\n'.encode())
		time.sleep(5 * guest.scale)
		hibernate(guest, disk, 'clear: hibernate', False)
	finally:
		guest.close()
	off_started = time.monotonic()
	note(f'{target}: the boot after the resume found no image, was set up with the warning, and hibernated in the clear')
	# 4. Restored in the clear, and hibernated again from the restored machine.
	guest = Guest(target, disk, 'clear-restore', work)
	try:
		guest.wait_line('HibernationService: its key was written in the clear - no TPM sealed it', 1800, 'clear: the key')
		restored(guest, disk, 'clear: restored', off_started, 'counter-clear.txt')
		hibernate(guest, disk, 'clear: the restored machine hibernates again', False)
	finally:
		guest.close()
	note(f'{target}: the restored machine hibernated again')
	# 5. A modified image.
	disk.modify()
	guest = Guest(target, disk, 'modified', work)
	try:
		# THE WHOLE LINE, which arrives a piece at a time: its reason is its end.
		refusal = re.compile(rb'HibernationService: the image is refused, and the machine boots fresh - ([^\n]*)\n')
		guest.serial.wait_for(0, lambda text: refusal.search(text) is not None, 1800, 'modified: the refusal')
		why = refusal.search(guest.serial.text_since(0)).group(1).decode(errors='replace')
		if 'modified' not in why:
			raise GateError(f'modified: the image was refused, but not as modified: {why}')
		guest.boot_to_shell('the fresh boot after the refusal')
		if disk.header() != 'empty':
			raise GateError(f'modified: the refused image\'s header was not invalidated ({disk.header()})')
	finally:
		guest.close()
	note(f'{target}: a modified image was refused, the header invalidated and the machine booted fresh')


def main():
	args = sys.argv[1:]
	skip_build = '--no-build' in args
	targets = [a for a in args if not a.startswith('--')] or list(TARGETS)
	for target in targets:
		if target not in TARGETS:
			print(f'check-hibernate-ports: unknown target {target!r}', file=sys.stderr)
			return 2
	if shutil.which('swtpm') is None:
		print('check-hibernate-ports: swtpm is not installed - setup.sh installs it, and this gate fails rather than skips without it', file=sys.stderr)
		return 1
	work = os.path.join(lab.BUILD_ROOT, 'logs', 'hibernate-ports')
	os.makedirs(work, exist_ok=True)
	failed = []
	for target in targets:
		try:
			if not skip_build:
				tickless.build(target)
			run(target, work)
		except GateError as error:
			print(f'check-hibernate-ports: {target}: FAIL - {error}', file=sys.stderr, flush=True)
			failed.append(target)
	if failed:
		print(f'check-hibernate-ports: FAIL on {", ".join(failed)}', file=sys.stderr)
		return 1
	note(f'PASS on {", ".join(targets)}')
	return 0


if __name__ == '__main__':
	sys.exit(main())
