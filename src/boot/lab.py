#!/usr/bin/env python3
# lab - drive a live LiberSystem instance for debugging, from the host.
#
# The manual debug loop (boot QEMU with a serial log, type into it through
# monitor sendkey one 0.3-second keystroke at a time, sleep-and-grep the log)
# is slow and fragile. This harness owns the serial console instead: `lab boot`
# starts the system with the UART on a unix socket and forks a broker that tees
# everything to a log file and serves a small control socket; `lab sh` then runs
# a shell command in the guest and returns its exact output by waiting for the
# prompt to come back - no sendkey pacing, no guessed sleeps. The QEMU monitor,
# a packet capture with a decoder, the keyboard path and the test suite ride
# along as subcommands.
#
# Usage:
#   boot/lab.py boot [--fresh] [--vnc] [--spice] [--timeout N]
#   boot/lab.py sh <command...>      run a shell command, print its output
#   boot/lab.py int                  interrupt the foreground job (Ctrl+C)
#   boot/lab.py wait [--timeout N]   wait for the shell prompt
#   boot/lab.py log [-f | <pattern>] show / follow / grep the serial log
#   boot/lab.py key <text>           type through the emulated keyboard (HID path)
#   boot/lab.py monitor <command...> one QEMU monitor command, print the reply
#   boot/lab.py usb-attach           hot-plug the USB mass-storage stick at runtime
#   boot/lab.py usb-detach           hot-unplug the USB stick at runtime
#   boot/lab.py pcap <on|off|dump>   capture guest network traffic and decode it
#   boot/lab.py test                 run the kernel test suite, summarize
#   boot/lab.py shot <path>          screenshot the framebuffer (screenshot.sh)
#   boot/lab.py quit                 shut the instance down and clean up
#
# A second family keeps one guest alive across commands instead of booting per command:
#   boot/lab.py dev-up [--fresh...]  boot once and keep the instance (takes the lock)
#   boot/lab.py dev-status           report the instance state deterministically
#   boot/lab.py dev-log [-f|<pat>]   show / follow / grep its serial log
#   boot/lab.py dev-down             stop it gracefully and release the lock
#
# `sh` joins its arguments, so quoting is optional: `just lab sh time ls`.

import fcntl
import json
import os
import re
import select
import signal
import socket
import struct
import subprocess
import sys
import time

USAGE = """lab - drive a live LiberSystem instance for debugging.

Usage (via `just lab ...` from src/, or boot/lab.py directly):
  boot [--fresh] [--vnc] [--spice] [--timeout N]
  sh <command...>       run a shell command in the guest, print its output
  int                   interrupt the foreground job (Ctrl+C over serial)
  wait [--timeout N]    wait for the shell prompt
  log [-f | <pattern>]  show / follow / grep the serial log
  key <text>            type through the emulated keyboard (the HID path)
  monitor <command...>  one QEMU monitor command, print the reply
  usb-attach            hot-plug the USB mass-storage stick at runtime
  usb-detach            hot-unplug the USB stick at runtime
  pcap <on|off|dump>    capture guest network traffic and decode it
  test                  run the kernel test suite, summarize
  shot <path>           screenshot the framebuffer
  quit                  shut the instance down and clean up

Persistent development instance (one owner at a time):
  dev-up [--vnc] [--spice] [--timeout N]   boot once and keep it running
  dev-status            report the instance state (exit 0 only when ready)
  dev-log [-f | <pat>]  show / follow / grep its serial log
  dev-down              stop it gracefully and release the lock"""

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.dirname(HERE)
REPO = os.path.dirname(SRC)
BUILD = os.path.join(REPO, '.build', 'boot')
SERIAL_SOCK = os.path.join(BUILD, 'lab-serial.sock')
CTL_SOCK = os.path.join(BUILD, 'lab-ctl.sock')
SERIAL_LOG = os.path.join(BUILD, 'lab-serial.log')
QEMU_LOG = os.path.join(BUILD, 'lab-qemu.log')
MON_SOCK = os.path.join(BUILD, 'qemu-monitor.sock')
PCAP = os.path.join(BUILD, 'lab.pcap')
VOLUME_IMG = os.path.join(BUILD, 'virtio-blk.img')
USB_IMG = os.path.join(BUILD, 'usb-media.img')

# The persistent development instance keeps its own serial socket, control socket and log
# so an ad-hoc `lab boot` debugging session never steals its console. The monitor and QMP
# sockets above stay shared, keeping their existing owners.
DEV_LOCK = os.path.join(BUILD, 'dev-instance.lock')
DEV_SERIAL_SOCK = os.path.join(BUILD, 'dev-serial.sock')
DEV_CTL_SOCK = os.path.join(BUILD, 'dev-ctl.sock')
DEV_SERIAL_LOG = os.path.join(BUILD, 'dev-serial.log')
DEV_QEMU_LOG = os.path.join(BUILD, 'dev-qemu.log')

ANSI = re.compile(rb'\x1b\[[0-9;?]*[ -/]*[@-~]')
PROMPT = re.compile(rb'vol://[^\r\n]*> ?$')


def strip_ansi(data):
	return ANSI.sub(b'', data)


def has_prompt(tail):
	return PROMPT.search(strip_ansi(tail)) is not None


def die(message):
	print(f'lab: {message}', file=sys.stderr)
	sys.exit(1)


# ---- broker ----------------------------------------------------------------
# The broker is forked by `boot` once the serial socket is up. It is the single
# owner of the serial connection: it tees every byte to the log file and serves
# one control request at a time - "RUN <timeout>\n<command>" sends the command
# and collects output until the prompt returns, "WAIT <timeout>" just waits for
# the prompt. It exits when the serial socket closes (QEMU is gone).

def broker(serial, ctl_path=CTL_SOCK, log_path=SERIAL_LOG):
	if os.path.exists(ctl_path):
		os.unlink(ctl_path)
	ctl = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
	ctl.bind(ctl_path)
	ctl.listen(1)
	log = open(log_path, 'ab', buffering=0)
	serial.setblocking(False)
	while True:
		ready, _, _ = select.select([serial, ctl], [], [], 0.5)
		if serial in ready:
			data = serial.recv(65536)
			if not data:
				break
			log.write(data)
		if ctl in ready:
			conn, _ = ctl.accept()
			try:
				if not serve_request(serial, conn, log, log_path):
					break
			finally:
				conn.close()
	log.close()
	os.unlink(ctl_path)


def serial_tail(log_path, size=256):
	try:
		with open(log_path, 'rb') as handle:
			handle.seek(0, os.SEEK_END)
			handle.seek(max(0, handle.tell() - size))
			return handle.read()
	except OSError:
		return b''


def serve_request(serial, conn, log, log_path=SERIAL_LOG):
	conn.settimeout(5)
	try:
		request = conn.makefile('rb').readline().decode(errors='replace').rstrip('\n')
	except OSError:
		return True
	parts = request.split(' ', 2)
	if not parts:
		return True
	collected = b''
	if parts[0] == 'RUN' and len(parts) == 3:
		timeout, command = float(parts[1]), parts[2]
		serial.sendall(command.encode() + b'\n')
	elif parts[0] == 'INT' and len(parts) >= 2:
		timeout = float(parts[1])
		serial.sendall(b'\x03')
	elif parts[0] == 'WAIT' and len(parts) >= 2:
		timeout = float(parts[1])
		# A prompt that arrived before this request is still a prompt. Seeding from the
		# log makes waiting on an already-idle guest return at once; without it the wait
		# can only see bytes yet to come, and an idle guest sends none.
		collected = serial_tail(log_path)
	else:
		return True
	# Collect serial output (teeing to the log all the while) until the prompt
	# returns or the timeout passes; the collected bytes are the reply.
	deadline = time.time() + timeout
	while time.time() < deadline:
		ready, _, _ = select.select([serial], [], [], 0.2)
		if serial in ready:
			data = serial.recv(65536)
			if not data:
				conn.sendall(collected)
				return False
			log.write(data)
			collected += data
		if has_prompt(collected[-256:]):
			break
	try:
		conn.sendall(collected)
	except OSError:
		pass
	return True


# Which instance the ad-hoc commands talk to. An explicit `lab boot` session wins, so
# debugging one never gets redirected; otherwise they reuse the persistent development
# instance rather than demanding a boot of their own.
def active_ctl_sock():
	if os.path.exists(CTL_SOCK):
		return CTL_SOCK
	if os.path.exists(DEV_CTL_SOCK):
		return DEV_CTL_SOCK
	return CTL_SOCK


def ctl_request(request, timeout, ctl_path=CTL_SOCK):
	if not os.path.exists(ctl_path):
		die('no live instance (run `just lab boot` first)')
	conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
	conn.connect(ctl_path)
	conn.settimeout(timeout + 10)
	conn.sendall(request.encode() + b'\n')
	reply = b''
	while True:
		try:
			data = conn.recv(65536)
		except socket.timeout:
			break
		if not data:
			break
		reply += data
	conn.close()
	return reply


# ---- persistent development instance ---------------------------------------
# `dev-up` boots one guest and leaves it running so later commands reuse it instead of
# paying for a boot each time. Exactly one instance may own the profile, because they
# would otherwise race over one system volume, one monitor socket and one set of build
# outputs. Ownership is an flock held by the broker for as long as the instance lives,
# so it is released by the kernel when the broker dies: there is no stale lock to break
# by hand, and "is it running" is answered by asking the lock rather than by guessing
# from a socket file that may have outlived its process.

def dev_identity_read():
	try:
		with open(DEV_LOCK) as handle:
			return json.load(handle)
	except (OSError, ValueError):
		return {}


def dev_identity_write(lock_fd, identity):
	os.ftruncate(lock_fd, 0)
	os.lseek(lock_fd, 0, os.SEEK_SET)
	os.write(lock_fd, json.dumps(identity).encode() + b'\n')
	os.fsync(lock_fd)


# True when no broker holds the instance. The probe takes and immediately drops the lock,
# which is safe precisely because an owner would have made the attempt fail.
def dev_lock_free():
	try:
		fd = os.open(DEV_LOCK, os.O_RDWR | os.O_CREAT, 0o644)
	except OSError:
		return True
	try:
		fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
		fcntl.flock(fd, fcntl.LOCK_UN)
		return True
	except OSError:
		return False
	finally:
		os.close(fd)


def dev_ready(timeout=3):
	if not os.path.exists(DEV_CTL_SOCK):
		return False
	try:
		reply = ctl_request(f'WAIT {timeout}', timeout, DEV_CTL_SOCK)
	except (SystemExit, OSError):
		return False
	return has_prompt(reply[-256:])


# One of: down, starting, ready, stale, foreign. Every dev command branches on this, so
# the same inputs always produce the same verdict and the same exit status.
def dev_state():
	identity = dev_identity_read()
	if dev_lock_free():
		leftovers = [p for p in (DEV_CTL_SOCK, DEV_SERIAL_SOCK) if os.path.exists(p)]
		return ('stale' if leftovers else 'down'), identity
	if identity.get('repo') != REPO:
		return 'foreign', identity
	return ('ready' if dev_ready() else 'starting'), identity


def dev_describe(identity):
	repo = identity.get('repo', 'unknown worktree')
	host = identity.get('host', 'unknown host')
	pid = identity.get('pgid', '?')
	return f'{repo} on {host} (process group {pid})'


def dev_uptime(identity):
	started = identity.get('started')
	return f'{time.time() - started:.1f} s' if started else 'unknown'


# Refuse rather than race, and name both the owner and the exact command that frees it.
def dev_owner_conflict(identity):
	print(f'lab: development profile is owned by {dev_describe(identity)}', file=sys.stderr)
	print(f'lab: release it with `just dev-down` in {identity.get("repo", "that worktree")}', file=sys.stderr)
	sys.exit(1)


def cmd_dev_up(args):
	timeout = arg_value(args, '--timeout', 240)
	displays = [d for d in ('vnc', 'spice') if f'--{d}' in args]
	state, identity = dev_state()
	if state == 'ready':
		# Idempotent for the owner: booting once and reusing the instance is the point,
		# so a repeated `dev-up` reports the running guest instead of disturbing it.
		print(f'lab: development instance already up, ready (up {dev_uptime(identity)})')
		return
	if state in ('starting', 'foreign'):
		dev_owner_conflict(identity)
	if state == 'stale':
		die('stale development instance state; run `just dev-down` to clear it')

	lock_fd = os.open(DEV_LOCK, os.O_RDWR | os.O_CREAT, 0o644)
	try:
		fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
	except OSError:
		os.close(lock_fd)
		dev_owner_conflict(dev_identity_read())

	for path in (DEV_SERIAL_SOCK, DEV_CTL_SOCK):
		if os.path.exists(path):
			os.unlink(path)
	open(DEV_SERIAL_LOG, 'wb').close()
	started = time.time()
	# `server` without `nowait`: QEMU blocks until the broker connects, so no boot output
	# is lost. `start_new_session` gives the instance its own process group, which is what
	# `dev-down` stops - never a QEMU that this instance does not own.
	env = dict(os.environ, SERIAL=f'unix:{DEV_SERIAL_SOCK},server', DEV_PROFILE='1')
	qemu_log = open(DEV_QEMU_LOG, 'wb')
	guest = subprocess.Popen(['just', 'run'] + displays, cwd=SRC, env=env, stdout=qemu_log, stderr=qemu_log, start_new_session=True)
	while True:
		if time.time() - started > timeout:
			os.close(lock_fd)
			die(f'serial socket did not appear within {timeout} s (see {DEV_QEMU_LOG})')
		# A fresh socket per attempt: a stream socket whose connect failed cannot be
		# reliably reconnected, so retrying on the same one can fail for good.
		serial = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
		try:
			serial.connect(DEV_SERIAL_SOCK)
			break
		except OSError:
			serial.close()
			if guest.poll() is not None:
				os.close(lock_fd)
				die(f'guest exited before the serial socket appeared (see {DEV_QEMU_LOG})')
			time.sleep(0.5)
	# The broker inherits the locked descriptor across the fork, so ownership outlives
	# this command and ends exactly when the broker does.
	broker_pid = os.fork()
	if broker_pid == 0:
		os.setsid()
		# Drop the inherited standard streams. The broker outlives this command, so
		# holding its caller's stdout open would keep any pipe or command substitution
		# around `dev-up` waiting for an end-of-file that only arrives when the whole
		# instance is stopped.
		null = os.open(os.devnull, os.O_RDWR)
		for stream in (0, 1, 2):
			os.dup2(null, stream)
		os.close(null)
		try:
			broker(serial, DEV_CTL_SOCK, DEV_SERIAL_LOG)
		finally:
			os._exit(0)
	serial.close()
	dev_identity_write(lock_fd, {
		'profile': 'development',
		'repo': REPO,
		'host': socket.gethostname(),
		'broker': broker_pid,
		'pgid': guest.pid,
		'started': started,
	})
	os.close(lock_fd)
	time.sleep(0.2)
	reply = ctl_request(f'WAIT {timeout}', timeout, DEV_CTL_SOCK)
	if not has_prompt(reply[-256:]):
		die(f'no shell prompt within {timeout} s (see {DEV_SERIAL_LOG})')
	profile = 'development' if b'boot profile: development' in strip_ansi(reply) or dev_profile_logged() else 'not reported'
	print(f'lab: development instance ready in {time.time() - started:.1f} s (guest profile: {profile})')
	print(f'lab: serial log {os.path.relpath(DEV_SERIAL_LOG, SRC)}; `just dev-status`, `just dev-down`')


def dev_profile_logged():
	try:
		with open(DEV_SERIAL_LOG, 'rb') as handle:
			return b'boot profile: development' in handle.read()
	except OSError:
		return False


def cmd_dev_status(args):
	state, identity = dev_state()
	print(f'lab: development instance {state}')
	if state == 'down':
		print('     start it with `just dev-up`')
		sys.exit(1)
	if state == 'stale':
		print('     the broker is gone but its sockets remain')
		print('     clear it with `just dev-down`')
		sys.exit(1)
	print(f'     owner    {dev_describe(identity)}')
	print(f'     broker   pid {identity.get("broker", "?")}')
	print(f'     uptime   {dev_uptime(identity)}')
	print(f'     profile  {"development" if dev_profile_logged() else "not reported by the guest"}')
	print(f'     serial   {os.path.relpath(DEV_SERIAL_LOG, SRC)}')
	if state == 'foreign':
		print('     another worktree owns it; this one cannot use or stop it')
		sys.exit(1)
	if state == 'starting':
		print('     no shell prompt yet; rerun `just dev-status` or watch `just dev-log -f`')
		sys.exit(1)
	sys.exit(0)


def cmd_dev_log(args):
	if not os.path.exists(DEV_SERIAL_LOG):
		die('no development serial log yet (run `just dev-up` first)')
	if '-f' in args:
		os.execvp('tail', ['tail', '-f', DEV_SERIAL_LOG])
	if args:
		os.execvp('grep', ['grep', '-a', '--color=auto', ' '.join(args), DEV_SERIAL_LOG])
	os.execvp('tail', ['tail', '-40', DEV_SERIAL_LOG])


def dev_group_alive(pgid):
	if not pgid:
		return False
	try:
		os.killpg(pgid, 0)
		return True
	except OSError:
		return False


def dev_stopped(pgid):
	return dev_lock_free() and not dev_group_alive(pgid)


def cmd_dev_down(args):
	timeout = arg_value(args, '--timeout', 30)
	state, identity = dev_state()
	if state == 'foreign':
		dev_owner_conflict(identity)
	if state == 'down':
		print('lab: no development instance')
		return
	# Ask the guest to stop through the monitor first; the recorded process group is the
	# fallback, so a wedged QEMU still goes away without touching an instance this one
	# does not own. A stale instance takes the same path on purpose: its broker is gone,
	# which says nothing about whether its guest is still running.
	pgid = identity.get('pgid')
	if os.path.exists(MON_SOCK):
		try:
			monitor_command('quit')
		except (SystemExit, OSError):
			pass
	deadline = time.time() + timeout
	while time.time() < deadline and not dev_stopped(pgid):
		time.sleep(0.2)
	if not dev_stopped(pgid) and pgid:
		try:
			os.killpg(pgid, signal.SIGKILL)
		except OSError:
			pass
		deadline = time.time() + 5
		while time.time() < deadline and not dev_stopped(pgid):
			time.sleep(0.2)
	# Settle the verdict before unlinking: probing the lock recreates its file, which
	# would leave an empty one behind and make a stopped instance look half-cleaned.
	stopped = dev_stopped(pgid)
	for path in (DEV_SERIAL_SOCK, DEV_CTL_SOCK, DEV_LOCK):
		if os.path.exists(path):
			os.unlink(path)
	if stopped:
		print('lab: development instance down')
	else:
		die(f'development instance did not stop; inspect it with `ps -g {pgid}`')


# ---- subcommands -----------------------------------------------------------

def cmd_boot(args):
	fresh = '--fresh' in args
	timeout = arg_value(args, '--timeout', 240)
	displays = [d for d in ('vnc', 'spice') if f'--{d}' in args]
	subprocess.run(['pkill', '-9', '-f', 'qemu-system-x86'], check=False)
	time.sleep(1)
	for path in (SERIAL_SOCK, CTL_SOCK):
		if os.path.exists(path):
			os.unlink(path)
	if fresh and os.path.exists(VOLUME_IMG):
		os.unlink(VOLUME_IMG)
	open(SERIAL_LOG, 'wb').close()
	# `server` without `nowait`: QEMU blocks until the broker connects, so no
	# boot output is ever lost between startup and the connect below.
	env = dict(os.environ, SERIAL=f'unix:{SERIAL_SOCK},server')
	qemu_log = open(QEMU_LOG, 'wb')
	subprocess.Popen(['just', 'run'] + displays, cwd=SRC, env=env, stdout=qemu_log, stderr=qemu_log, start_new_session=True)
	started = time.time()
	serial = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
	while True:
		if time.time() - started > timeout:
			die(f'serial socket did not appear within {timeout} s (see {QEMU_LOG})')
		try:
			serial.connect(SERIAL_SOCK)
			break
		except OSError:
			time.sleep(0.5)
	# Hand the connection to a detached broker, then wait for the first prompt.
	if os.fork() == 0:
		os.setsid()
		try:
			broker(serial)
		finally:
			os._exit(0)
	serial.close()
	time.sleep(0.2)
	reply = ctl_request(f'WAIT {timeout}', timeout)
	if not has_prompt(reply[-256:]):
		die(f'no shell prompt within {timeout} s (see {SERIAL_LOG})')
	print(f'lab: booted in {time.time() - started:.1f} s' + (' (fresh volume)' if fresh else ''))
	print(f'lab: serial log {os.path.relpath(SERIAL_LOG, SRC)}; try `just lab sh uname`')


def cmd_sh(args):
	timeout, rest = take_arg(args, '--timeout', 30)
	command = ' '.join(rest)
	if not command:
		die('usage: lab sh <command...>')
	reply = ctl_request(f'RUN {timeout} {command}', timeout, active_ctl_sock())
	text = strip_ansi(reply).decode(errors='replace').replace('\r\n', '\n')
	lines = text.split('\n')
	# Drop the echoed command line and the trailing prompt; the rest is the output.
	if lines and command in lines[0]:
		lines = lines[1:]
	while lines and (lines[-1] == '' or PROMPT.search(lines[-1].encode())):
		lines.pop()
	print('\n'.join(lines))


def cmd_wait(args):
	timeout = arg_value(args, '--timeout', 60)
	reply = ctl_request(f'WAIT {timeout}', timeout, active_ctl_sock())
	sys.exit(0 if has_prompt(reply[-256:]) else 1)


# Interrupt the guest's foreground job: one 0x03 byte on the serial console (the
# console's line discipline turns it into SIG_INT), then wait for the prompt.
def cmd_int(args):
	timeout = arg_value(args, '--timeout', 15)
	reply = ctl_request(f'INT {timeout}', timeout, active_ctl_sock())
	sys.exit(0 if has_prompt(reply[-256:]) else 1)


def cmd_log(args):
	if not os.path.exists(SERIAL_LOG):
		die('no serial log yet')
	if '-f' in args:
		os.execvp('tail', ['tail', '-f', SERIAL_LOG])
	if args:
		os.execvp('grep', ['grep', '-a', '--color=auto', ' '.join(args), SERIAL_LOG])
	os.execvp('tail', ['tail', '-40', SERIAL_LOG])


# The monitor sendkey names for the characters the shell needs; letters pass
# through (uppercase via shift-), so only the specials are listed.
KEYMAP = {' ': 'spc', '.': 'dot', ',': 'comma', '-': 'minus', '/': 'slash', ':': 'shift-semicolon', ';': 'semicolon', '_': 'shift-minus', '=': 'equal', '\n': 'ret'}


def cmd_key(args):
	text = ' '.join(args)
	for ch in text + '\n':
		if ch.isalpha():
			key = f'shift-{ch.lower()}' if ch.isupper() else ch
		elif ch.isdigit():
			key = ch
		elif ch in KEYMAP:
			key = KEYMAP[ch]
		else:
			die(f'no sendkey mapping for {ch!r}')
		monitor_command(f'sendkey {key}')
		time.sleep(0.05)


def monitor_command(command):
	if not os.path.exists(MON_SOCK):
		die('no QEMU monitor socket (is the instance up?)')
	conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
	conn.connect(MON_SOCK)
	conn.settimeout(5)
	conn.sendall(command.encode() + b'\n')
	reply = b''
	try:
		while True:
			data = conn.recv(65536)
			if not data:
				break
			reply += data
			if reply.count(b'(qemu)') >= 2:
				break
	except socket.timeout:
		pass
	conn.close()
	lines = strip_ansi(reply).decode(errors='replace').replace('\r', '').split('\n')
	return '\n'.join(l for l in lines if l and not l.startswith('QEMU ') and '(qemu)' not in l)


def cmd_monitor(args):
	if not args:
		die('usage: lab monitor <command...>')
	output = monitor_command(' '.join(args))
	if output:
		print(output)


def cmd_usb_attach(args):
	# Hot-plug the USB mass-storage stick onto the xHCI bus at runtime. The xhci driver
	# watches port-status-change events and enumerates the new device, DeviceManager binds
	# the storage role, and a StorageService instance mounts it as vol://usb - the runtime
	# counterpart of the boot-time enumeration. Re-add the block backend first (a detach
	# removes it); its output is ignored so an already-present drive is harmless.
	monitor_command(f'drive_add 0 file={USB_IMG},if=none,id=vusb,format=raw')
	output = monitor_command('device_add usb-storage,bus=usb.0,drive=vusb,id=usbstick')
	print(output or 'lab: usb attached')


def cmd_usb_detach(args):
	# Hot-unplug the USB stick: the xhci driver sees the port disconnect, disables the
	# port's slots, drops the storage state (vol://usb unmounts) and prints "port
	# detached" - without wedging the standing StorageService instance.
	output = monitor_command('device_del usbstick')
	print(output or 'lab: usb detached')


def cmd_pcap(args):
	action = args[0] if args else 'dump'
	if action == 'on':
		if os.path.exists(PCAP):
			os.unlink(PCAP)
		monitor_command(f'object_add filter-dump,id=lab0,netdev=vnet0,file={PCAP}')
		print(f'lab: capturing to {os.path.relpath(PCAP, SRC)}')
	elif action == 'off':
		monitor_command('object_del lab0')
		print('lab: capture stopped')
	elif action == 'dump':
		pcap_dump()
	else:
		die('usage: lab pcap <on|off|dump>')


def pcap_dump():
	if not os.path.exists(PCAP):
		die('no capture file (run `lab pcap on` first)')
	data = open(PCAP, 'rb').read()
	offset, index = 24, 0
	while offset + 16 <= len(data):
		_, _, incl, _ = struct.unpack('<IIII', data[offset:offset + 16])
		packet = data[offset + 16:offset + 16 + incl]
		offset += 16 + incl
		index += 1
		print(f'{index}: {decode_packet(packet)}')


def decode_packet(p):
	if len(p) < 14:
		return f'short frame ({len(p)} B)'
	ethertype = struct.unpack('>H', p[12:14])[0]
	if ethertype == 0x0806:
		op = 'request' if len(p) >= 22 and p[21] == 1 else 'reply'
		return f'ARP {op} {ip_str(p[28:32])} -> {ip_str(p[38:42])}' if len(p) >= 42 else 'ARP'
	if ethertype != 0x0800 or len(p) < 34:
		return f'ethertype {ethertype:#06x} ({len(p)} B)'
	ihl = (p[14] & 0x0f) * 4
	total = struct.unpack('>H', p[16:18])[0]
	proto, src, dst = p[23], ip_str(p[26:30]), ip_str(p[30:34])
	t = 14 + ihl
	if proto == 1:
		return f'ICMP {src} -> {dst} type {p[t]} ({total} B)'
	if proto == 17:
		sp, dp = struct.unpack('>HH', p[t:t + 4])
		return f'UDP {src}:{sp} -> {dst}:{dp} len {total - ihl - 8}'
	if proto == 6:
		sp, dp = struct.unpack('>HH', p[t:t + 4])
		seq, ack = struct.unpack('>II', p[t + 4:t + 12])
		doff = (p[t + 12] >> 4) * 4
		flags = ''.join(name for name, bit in (('F', 1), ('S', 2), ('R', 4), ('P', 8), ('A', 16)) if p[t + 13] & bit)
		win = struct.unpack('>H', p[t + 14:t + 16])[0]
		opts = p[t + 20:t + doff].hex()
		payload = total - ihl - doff
		return f'TCP {src}:{sp} -> {dst}:{dp} [{flags}] seq={seq} ack={ack} win={win} len={payload}' + (f' opts={opts}' if opts else '')
	return f'IP proto {proto} {src} -> {dst} ({total} B)'


def ip_str(b):
	return '.'.join(str(x) for x in b)


def cmd_test(args):
	timeout = arg_value(args, '--timeout', 900)
	subprocess.run(['pkill', '-9', '-f', 'qemu-system-x86'], check=False)
	time.sleep(1)
	if os.path.exists(VOLUME_IMG):
		os.unlink(VOLUME_IMG)
	log_path = os.path.join(BUILD, 'lab-test.log')
	with open(log_path, 'wb') as log:
		result = subprocess.run(['cargo', 'test'], cwd=os.path.join(SRC, 'kernel'), env=dict(os.environ, TEST='1'), stdout=log, stderr=log, timeout=timeout)
	output = open(log_path, 'rb').read().decode(errors='replace')
	ok = output.count('[ok]')
	print(f'lab: suite RC={result.returncode}, {ok} [ok] (log {os.path.relpath(log_path, SRC)})')
	if result.returncode != 0:
		for line in output.splitlines():
			if 'panic' in line.lower() or 'FAILED' in line:
				print(f'   {line.strip()}')
	sys.exit(result.returncode)


def cmd_shot(args):
	if not args:
		die('usage: lab shot <path>')
	sys.exit(subprocess.run([os.path.join(HERE, 'screenshot.sh'), args[0]], cwd=SRC).returncode)


def cmd_quit(_args):
	# The monitor socket may be a stale file from an instance that is already gone
	# (e.g. after `lab test` replaced it) - a clean quit falls through to the kill.
	if os.path.exists(MON_SOCK):
		try:
			monitor_command('quit')
		except (SystemExit, OSError):
			pass
	time.sleep(1)
	subprocess.run(['pkill', '-9', '-f', 'qemu-system-x86'], check=False)
	for path in (SERIAL_SOCK, CTL_SOCK):
		if os.path.exists(path):
			os.unlink(path)
	print('lab: instance down')


def arg_value(args, name, default):
	for i, arg in enumerate(args):
		if arg == name and i + 1 < len(args):
			return int(args[i + 1])
		if arg.startswith(name + '='):
			return int(arg.split('=', 1)[1])
	return default


# Like arg_value, additionally returning the arguments with the option removed -
# for subcommands whose remaining arguments are free text (`sh`).
def take_arg(args, name, default):
	value, rest, skip = default, [], False
	for i, arg in enumerate(args):
		if skip:
			skip = False
			continue
		if arg == name and i + 1 < len(args):
			value, skip = int(args[i + 1]), True
		elif arg.startswith(name + '='):
			value = int(arg.split('=', 1)[1])
		else:
			rest.append(arg)
	return value, rest


COMMANDS = {'boot': cmd_boot, 'sh': cmd_sh, 'int': cmd_int, 'wait': cmd_wait, 'log': cmd_log, 'key': cmd_key, 'monitor': cmd_monitor, 'usb-attach': cmd_usb_attach, 'usb-detach': cmd_usb_detach, 'pcap': cmd_pcap, 'test': cmd_test, 'shot': cmd_shot, 'quit': cmd_quit, 'dev-up': cmd_dev_up, 'dev-status': cmd_dev_status, 'dev-log': cmd_dev_log, 'dev-down': cmd_dev_down}


def main():
	signal.signal(signal.SIGPIPE, signal.SIG_DFL)
	if len(sys.argv) < 2 or sys.argv[1] in ('-h', '--help', 'help'):
		print(USAGE)
		sys.exit(0)
	command = sys.argv[1]
	if command not in COMMANDS:
		die(f'unknown command {command!r} (see `lab help`)')
	COMMANDS[command](sys.argv[2:])


if __name__ == '__main__':
	main()
