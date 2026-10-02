#!/usr/bin/env python3
# check-hibernate-ports.py - hibernation on the device-tree ports, end to end: aarch64 and riscv64, each booting its
# development build from a system disk SET UP FOR IT - a GPT whose first partition is the system volume and whose second
# is a hibernation partition a little larger than the guest's memory. No TPM is attached: these boots are the owner's
# "hibernation without a TPM", allowed with a warning, the image's key written beside it in the clear.
#
# THE ORACLES ARE THE HOST'S. Neither port has an S4, so a hibernation ends in the firmware's power-off and the guest's
# QEMU exits; the image's header is read from the disk file between the boots - its magic, its state, and how its key is
# kept. On each target:
#
#   1. SET UP AND WARNED: the kernel says hibernation is offered (every core's context through the firmware), and
#      `sleepctl status` says it is set up, with the warning that no TPM seals its key.
#   2. HIBERNATE AND RESTORE. A counter runs in the background at the serial shell; `sleepctl hibernate`; the image is
#      written with its key in the clear - the header says so - and QEMU exits. A new boot on the same disk takes the key
#      from the header, puts the image in the kernel, replaces memory on the core the image's boot core was, and the
#      image's kernel resumes: every other core turned on again at its record, the transaction ending "slept and woke"
#      by the restore, the header invalidated - and the counter goes on at its next value, one unbroken sequence, with
#      its monotonic clock moved by less than five seconds across the time off and its boot-time clock by at least half
#      of it: the same program, the same counter, on a machine all of whose cores came back.
#   3. THE BOOT AFTER A RESUME sees no image.
#   4. A MODIFIED IMAGE: hibernated again, one byte of a chunk flipped on the host - refused as modified, the header
#      invalidated, and the machine boots fresh.
#
# ONE GUEST AT A TIME: each boot is torn down before the next one, and each target before the next.
#
# usage: check-hibernate-ports.py [--no-build] [aarch64|riscv64 ...]   (default: both)

import importlib.util
import os
import re
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


# ------------------------------------------------------------------ one boot

class Guest:
	def __init__(self, target, disk, label, work):
		self.target = target
		self.scale = 10.0
		self.serial_path = os.path.join(lab.BUILD, f'hibernate-serial-{target}.sock')
		self.log_path = os.path.join(work, f'hibernate-{target}-{label}.log')
		self.runner_log = open(os.path.join(work, f'hibernate-{target}-{label}-runner.log'), 'wb')
		kernel = os.path.join(lab.BUILD_ROOT, 'cargo', 'kernel', TRIPLES[target], 'debug', 'kernel')
		if os.path.exists(self.serial_path):
			os.unlink(self.serial_path)
		# AN ENTROPY DEVICE: neither port has a random instruction, and the image's keys are drawn from the kernel's pool.
		env = dict(os.environ, LIBER_DEVELOPMENT='1', DEV_PROFILE='1', COLD='1', SERIAL=f'unix:{self.serial_path},server', SMP=str(SMP), MEM=f'{MEM_MIB}M', RUN_DISK=disk.path, LIBER_RUN_MODE='development', UEFI='1', ENTROPY='1')
		note(f'{target}: boot {label}; serial log {self.log_path}')
		self.process = subprocess.Popen(['bash', 'harness/qemu-run.sh', target, kernel], cwd=SRC, env=env, stdout=self.runner_log, stderr=self.runner_log, start_new_session=True)
		sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
		deadline = time.monotonic() + 300
		while True:
			if self.process.poll() is not None:
				raise GateError(f'{label}: the guest exited before it opened its serial line (see {self.runner_log.name})')
			try:
				sock.connect(self.serial_path)
				break
			except OSError:
				if time.monotonic() > deadline:
					raise GateError(f'{label}: the guest never opened its serial line') from None
				time.sleep(0.5)
		self.log = open(self.log_path, 'wb')
		self.serial = Serial(sock, self.log, self.scale)

	def boot_to_shell(self, what):
		self.serial.wait_prompt(0, 1800, what)

	def seen(self, needle):
		return needle.encode() in lab.strip_ansi(bytes(self.serial.data))

	def wait_line(self, needle, timeout, what):
		self.serial.wait_for(0, lambda text: needle.encode() in text, timeout, what)

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
		self.log.close()
		self.runner_log.close()


# THE COUNTER ACROSS THE TIME OFF, read from its file once it has ended: one unbroken run of values from 1, whose widest
# step of the boot-time clock is the time off, at least `off_ms`, while the monotonic clock moved by less than five
# seconds across it.
COUNT = re.compile(r'sleepcheck: count (\d+) mono-ms (\d+) boot-ms (\d+)')


def counter_goes_on(guest, name, off_ms):
	end = time.monotonic() + (COUNTER_S * 2 + 120) * guest.scale
	while True:
		text = guest.serial.run(f'cat {name}', 120)
		if 'sleepcheck: count done' in text:
			break
		if time.monotonic() > end:
			raise GateError('the counter never finished, so its file was never published')
		time.sleep(5)
	rows = [tuple(int(value) for value in found) for found in COUNT.findall(text)]
	if not rows or [row[0] for row in rows] != list(range(1, len(rows) + 1)):
		raise GateError('the counter did not run one unbroken sequence from 1 - it skipped, repeated or started again')
	gap = max(range(1, len(rows)), key=lambda at: rows[at][2] - rows[at - 1][2])
	mono, boot = rows[gap][1] - rows[gap - 1][1], rows[gap][2] - rows[gap - 1][2]
	if boot < off_ms:
		raise GateError(f'the boot-time clock moved {boot} ms at its widest step, under the {off_ms} ms off')
	if mono > 5000:
		raise GateError(f'the monotonic clock moved {mono} ms across the time off')
	return rows[gap - 1][0], rows[gap][0], mono, boot


# A HIBERNATION ASKED: the image written with its key in the clear, and the guest gone.
def hibernate(guest, disk, what):
	guest.serial.type(b'sleepctl hibernate\n')
	guest.wait_line('HibernationService: the image is written', 900, f'{what}: the image')
	# THE WHOLE LINE, which arrives a piece at a time: its key in the clear, and the warning.
	guest.wait_line(f'its key NOT sealed - {WARNING}', 60, f'{what}: the image written with its key in the clear and the warning')
	guest.wait_line('hibernate: the image is written - no S4 on this machine, so it is powered off', 300, f'{what}: the power-off')
	guest.wait_exit(300, what)
	guest.close()
	if disk.header() != 'image' or disk.protection() != 'clear':
		raise GateError(f'{what}: the partition does not hold an image whose key is in the clear ({disk.header()}, {disk.protection()})')


def run(target, work):
	disk = Disk(target, work)
	# 1 and 2: set up and warned; hibernate.
	guest = Guest(target, disk, 'fresh', work)
	try:
		guest.boot_to_shell('the first boot')
		if not guest.seen('sleep: hibernation is offered'):
			raise GateError('the kernel did not say hibernation is offered')
		status = guest.serial.run('sleepctl status', 300)
		if f'hibernation: set up - {WARNING}' not in status:
			raise GateError(f'the status does not say hibernation is set up with the warning: {status}')
		guest.serial.type(f'sleepcheck count {COUNTER_S} > counter.txt &\n'.encode())
		time.sleep(5 * guest.scale)
		hibernate(guest, disk, 'hibernate')
	finally:
		guest.close()
	off_started = time.monotonic()
	note(f'{target}: hibernated - the image written with its key in the clear, and the machine off')
	guest = Guest(target, disk, 'restore', work)
	try:
		guest.wait_line('HibernationService: its key was written in the clear - no TPM sealed it', 1800, 'restore: the key')
		guest.wait_line('HibernationService: the image is authenticated and in the kernel', 1800, 'restore: the image')
		guest.wait_line('hibernate: replacing memory with the image', 600, 'restore: the replacement')
		guest.wait_line('sleep: resumed (the restore of a hibernation image', 600, 'restore: the resume')
		off_ms = int((time.monotonic() - off_started) * 1000)
		guest.wait_line('ServiceManager: sleep: the transaction ended - slept and woke', 900, 'restore: the transaction')
		if guest.seen('did not come back from the image') or guest.seen('was not turned on again'):
			raise GateError('restore: a core did not come back from the image')
		guest.serial.type(b'\n')
		guest.boot_to_shell('the restored machine')
		if disk.header() != 'empty':
			raise GateError(f'restore: the header was not invalidated before the jump ({disk.header()})')
		before, after, mono, boot = counter_goes_on(guest, 'counter.txt', off_ms // 2)
		note(f'{target}: restored - count {before} then {after}: monotonic +{mono} ms, boot-time +{boot} ms, every core back')
	finally:
		guest.close()
	# 3. The boot after a resume.
	guest = Guest(target, disk, 'after-restore', work)
	try:
		guest.boot_to_shell('the boot after the resume')
		status = guest.serial.run('sleepctl status', 300)
		if 'the image found at this boot: none' not in status:
			raise GateError(f'the boot after a resume found an image: {status}')
		hibernate(guest, disk, 'hibernate again')
	finally:
		guest.close()
	note(f'{target}: the boot after the resume found no image; hibernated again')
	# 4. A modified image.
	disk.modify()
	guest = Guest(target, disk, 'modified', work)
	try:
		guest.wait_line('HibernationService: the image is refused, and the machine boots fresh - ', 1800, 'modified: the refusal')
		refused = [line for _, line in guest.serial.lines_since(0) if 'the image is refused' in line]
		if not any('modified' in line for line in refused):
			raise GateError(f'modified: the image was refused, but not as modified: {refused}')
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
