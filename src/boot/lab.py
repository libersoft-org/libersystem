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
#   boot/lab.py dev-console [--read-only]  attach a detachable terminal (Ctrl-] leaves)
#   boot/lab.py dev-log [-f|<pat>]   show / follow / grep its serial log
#   boot/lab.py dev-down             stop it gracefully and release the lock
#
# `sh` joins its arguments, so quoting is optional: `just lab sh time ls`.

import fcntl
import hashlib
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
  dev-console [--read-only]  attach an interactive terminal; Ctrl-] detaches
  dev-log [-f | <pat>]  show / follow / grep its serial log
  dev-ping [--count N] [--size N] [--timeout N]  exercise the control protocol
  dev-publish <name> <file>  stream a file into the guest artifact registry
  dev-generations       list what the guest registry shadows, and since when
  dev-rollback <name>   return an artifact to the generation before its newest
  dev-type [--no-enter] <text...>  type into the guest terminal, with a byte count back
  dev-reset             drop the guest development state (not a reboot)
  dev-scenario [--verbose] <file.toml>...  run declarative application scenarios
  dev-down              stop it gracefully and release the lock"""

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
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
DEV_CONSOLE_SOCK = os.path.join(BUILD, 'dev-console.sock')
# The control channel is the second virtio-serial device, not the console. QEMU listens
# here and the guest's dev-channel driver holds the other end; nothing tees or logs it,
# because it carries framed requests rather than terminal output.
DEV_CHANNEL_SOCK = os.path.join(BUILD, 'dev-channel.sock')

# The development instance's sockets are its whole boundary: the control channel publishes
# executable code into a running guest, and the console is a terminal on it. They are created
# owner-only, and every connection checks before trusting one - a socket anyone on the machine
# can reach is not a development channel, it is a way into this guest.
DEV_SOCKET_MODE = 0o600

# An artifact name identifies a registry slot in the guest, not a path. The guest checks the
# same rule character by character; this one exists so a typo fails before a megabyte moves.
NAME_OK = re.compile(r'[A-Za-z0-9_-][A-Za-z0-9_.-]{0,47}')

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
#
# It is also the only reader of the guest's UART, which is what makes a human console
# detachable: the broker drains QEMU whether or not anyone is watching, so no absent or
# slow terminal can back the socket up and stall the guest. Attached terminals are fed
# from that drain, and a bounded replay of recent output lets a reconnecting one pick up
# where it left off. The console socket carries raw bytes and nothing else - it shares no
# framing, socket or state with the control channel above, the QEMU monitor or QMP.

# How much recent output a reattaching console replays. Bounded on purpose: the broker
# must never grow with the guest's lifetime, and the searchable history is the log file.
REPLAY_BYTES = 262144


# Read one handshake line without reading ahead. A buffered reader would happily pull the
# bytes that follow into its own buffer, and on a console connection those bytes are the
# replay or the operator's first keystrokes - lost before anyone could use them.
def recv_line(sock, limit=256):
	line = b''
	while len(line) < limit:
		byte = sock.recv(1)
		if not byte or byte == b'\n':
			break
		line += byte
	return line.decode(errors='replace').strip()


def broker_absorb(state, data):
	state['log'].write(data)
	replay = state['replay']
	replay += data
	if len(replay) > REPLAY_BYTES:
		del replay[:len(replay) - REPLAY_BYTES]
	for client in list(state['clients']):
		broker_send(state, client, data)


# Never block on a client. A terminal that stopped reading loses bytes rather than
# holding up the guest; its replay covers the gap when it reattaches.
def broker_send(state, client, data):
	try:
		client['sock'].send(data)
	except BlockingIOError:
		pass
	except OSError:
		broker_drop(state, client)


def broker_drop(state, client):
	if client in state['clients']:
		state['clients'].remove(client)
	if state['writer'] is client:
		state['writer'] = None
	try:
		client['sock'].close()
	except OSError:
		pass


# One line of handshake, then the connection is a raw byte stream in both directions.
# Exactly one client may hold the writer slot; everyone else observes.
def broker_attach(state, sock):
	sock.settimeout(5)
	try:
		request = recv_line(sock)
	except OSError:
		sock.close()
		return
	if not request.startswith('ATTACH'):
		sock.close()
		return
	wants_write = request.split()[1:2] == ['rw']
	client = {'sock': sock, 'writer': False}
	if wants_write and state['writer'] is None:
		client['writer'] = True
		state['writer'] = client
		reply = b'OK rw\n'
	elif wants_write:
		reply = b'OK ro busy\n'
	else:
		reply = b'OK ro\n'
	try:
		sock.sendall(reply)
		sock.sendall(bytes(state['replay']))
	except OSError:
		broker_drop(state, client)
		return
	sock.settimeout(None)
	sock.setblocking(False)
	state['clients'].append(client)


def broker_client_input(state, client):
	try:
		data = client['sock'].recv(65536)
	except BlockingIOError:
		return
	except OSError:
		broker_drop(state, client)
		return
	if not data:
		broker_drop(state, client)
		return
	# Only the writer reaches the guest. A reader's keystrokes are discarded rather than
	# silently interleaved into someone else's session.
	if client['writer']:
		try:
			state['serial'].sendall(data)
		except OSError:
			pass


def broker(serial, ctl_path=CTL_SOCK, log_path=SERIAL_LOG, console_path=None):
	if os.path.exists(ctl_path):
		os.unlink(ctl_path)
	ctl = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
	ctl.bind(ctl_path)
	# Narrow it where it is created rather than afterwards: the broker is forked, so anything
	# the parent tried to chmod would race the bind that has not happened yet.
	os.chmod(ctl_path, DEV_SOCKET_MODE)
	ctl.listen(1)
	console = None
	if console_path:
		if os.path.exists(console_path):
			os.unlink(console_path)
		console = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
		console.bind(console_path)
		os.chmod(console_path, DEV_SOCKET_MODE)
		console.listen(4)
	log = open(log_path, 'ab', buffering=0)
	serial.setblocking(False)
	state = {'serial': serial, 'log': log, 'log_path': log_path, 'replay': bytearray(), 'clients': [], 'writer': None}
	while True:
		watch = [serial, ctl] + ([console] if console else []) + [c['sock'] for c in state['clients']]
		ready, _, _ = select.select(watch, [], [], 0.5)
		if serial in ready:
			data = serial.recv(65536)
			if not data:
				break
			broker_absorb(state, data)
		if console is not None and console in ready:
			conn, _ = console.accept()
			broker_attach(state, conn)
		for client in list(state['clients']):
			if client['sock'] in ready:
				broker_client_input(state, client)
		if ctl in ready:
			conn, _ = ctl.accept()
			try:
				if not serve_request(state, conn):
					break
			finally:
				conn.close()
	for client in list(state['clients']):
		broker_drop(state, client)
	log.close()
	os.unlink(ctl_path)
	if console_path and os.path.exists(console_path):
		os.unlink(console_path)


def serial_tail(log_path, size=256):
	try:
		with open(log_path, 'rb') as handle:
			handle.seek(0, os.SEEK_END)
			handle.seek(max(0, handle.tell() - size))
			return handle.read()
	except OSError:
		return b''


def serve_request(state, conn):
	serial = state['serial']
	conn.settimeout(5)
	try:
		request = conn.makefile('rb').readline().decode(errors='replace').rstrip('\n')
	except OSError:
		return True
	parts = request.split(' ', 2)
	if not parts:
		return True
	if parts[0] == 'STAT':
		# Console occupancy, asked over the control channel rather than over the console
		# socket, so querying it never touches the human byte stream.
		readers = sum(1 for c in state['clients'] if not c['writer'])
		try:
			conn.sendall(f'writer={1 if state["writer"] else 0} readers={readers}\n'.encode())
		except OSError:
			pass
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
		collected = serial_tail(state['log_path'])
	else:
		return True
	# Collect serial output until the prompt returns or the timeout passes; the collected
	# bytes are the reply. Everything still goes through the shared drain, so an attached
	# console keeps seeing the guest while a scripted command runs.
	deadline = time.time() + timeout
	while time.time() < deadline:
		ready, _, _ = select.select([serial], [], [], 0.2)
		if serial in ready:
			data = serial.recv(65536)
			if not data:
				conn.sendall(collected)
				return False
			broker_absorb(state, data)
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

# What the running guest was booted from, split into the classes whose invalidations differ.
# Each entry is (name, source roots, files). Sources catch an edit that has not been built
# yet; files catch a build that has not been booted yet. Both are cold invalidations, and
# naming the class is what keeps one from being mistaken for a hot-publishable change.
INSTANCE_INPUTS = (
	('protocol', ['src/bootproto'], []),
	('kernel', ['src/kernel'], ['.build/cargo/kernel/x86_64-unknown-none/debug/kernel']),
	('loader', ['src/loader'], ['.build/cargo/loader/x86_64-unknown-uefi/debug/libersystem-loader.efi']),
	('packages', [], ['.build/boot/init.pkg', '.build/boot/volume.pkg']),
	('image', [], ['.build/boot/libersystem.iso', 'src/boot/mkimage.sh']),
	('topology', [], ['src/boot/qemu-run.sh']),
)

INPUT_ACTIONS = {
	'protocol': 'shared boot contract: rebuilds every binary that consumes it, kernel and loader alike',
	'kernel': 'recompiles the kernel and reassembles the image; an unchanged loader is not recompiled',
	'loader': 'recompiles the loader and reassembles the image; an unchanged kernel is not recompiled',
	'packages': 'reassembles the boot image from unchanged binaries',
	'image': 'reassembles the boot image from unchanged binaries',
	'topology': 'restarts the VM; the boot image is not rebuilt',
}

SOURCE_SUFFIXES = ('.rs', '.toml', '.lock', '.ld')


def digest_tree(root, digest):
	for base, dirs, names in os.walk(root):
		dirs[:] = sorted(d for d in dirs if d != 'target')
		for name in sorted(names):
			if not name.endswith(SOURCE_SUFFIXES):
				continue
			path = os.path.join(base, name)
			digest.update(os.path.relpath(path, REPO).encode() + b'\0')
			try:
				with open(path, 'rb') as handle:
					for chunk in iter(lambda: handle.read(1 << 20), b''):
						digest.update(chunk)
			except OSError:
				digest.update(b'unreadable')


# Content rather than timestamps: a rebuild that produces identical bytes must not read as
# a change, or every status would claim the instance is stale.
def instance_inputs():
	fingerprint = {}
	for name, roots, files in INSTANCE_INPUTS:
		digest = hashlib.sha256()
		for root in roots:
			digest_tree(os.path.join(REPO, root), digest)
		for relative in files:
			path = os.path.join(REPO, relative)
			digest.update(relative.encode() + b'\0')
			try:
				with open(path, 'rb') as handle:
					for chunk in iter(lambda: handle.read(1 << 20), b''):
						digest.update(chunk)
			except OSError:
				digest.update(b'missing')
		fingerprint[name] = digest.hexdigest()
	return fingerprint


def instance_stale_inputs(identity):
	recorded = identity.get('inputs') or {}
	if not recorded:
		return None
	current = instance_inputs()
	return [name for name, _, _ in INSTANCE_INPUTS if recorded.get(name) != current.get(name)]


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


# One of: down, detached, stale, foreign, starting, ready. Every dev command branches on
# this, so the same inputs always produce the same verdict and the same exit status.
#
# `detached` is the recoverable one: the broker is gone but its guest is not. That is the
# case worth separating, because nothing is draining the UART any more and the guest will
# eventually block on a full socket buffer - so it is reported as a state with a named
# repair rather than left to look like a healthy instance or a dead one.
def dev_state():
	identity = dev_identity_read()
	if dev_lock_free():
		if dev_group_alive(identity.get('pgid')):
			return 'detached', identity
		leftovers = [p for p in (DEV_CTL_SOCK, DEV_SERIAL_SOCK, DEV_CONSOLE_SOCK) if os.path.exists(p)]
		return ('stale' if leftovers else 'down'), identity
	if identity.get('repo') != REPO:
		return 'foreign', identity
	return ('ready' if dev_ready() else 'starting'), identity


# Console occupancy, read over the control channel so it never disturbs the byte stream.
def dev_console_stat():
	if not os.path.exists(DEV_CTL_SOCK):
		return ''
	try:
		return ctl_request('STAT', 3, DEV_CTL_SOCK).decode(errors='replace').strip()
	except (SystemExit, OSError):
		return ''


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


# Fork the broker onto an already-connected serial socket. It inherits the locked
# descriptor, so instance ownership outlives this command and ends exactly when the
# broker does, and it drops the inherited standard streams so a pipe around `dev-up`
# is not held open for the instance's whole life.
def dev_fork_broker(serial):
	broker_pid = os.fork()
	if broker_pid == 0:
		os.setsid()
		null = os.open(os.devnull, os.O_RDWR)
		for stream in (0, 1, 2):
			os.dup2(null, stream)
		os.close(null)
		try:
			broker(serial, DEV_CTL_SOCK, DEV_SERIAL_LOG, DEV_CONSOLE_SOCK)
		finally:
			os._exit(0)
	serial.close()
	return broker_pid


def dev_reattach(lock_fd, identity, timeout):
	for path in (DEV_CTL_SOCK, DEV_CONSOLE_SOCK):
		if os.path.exists(path):
			os.unlink(path)
	serial = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
	try:
		serial.connect(DEV_SERIAL_SOCK)
	except OSError as error:
		os.close(lock_fd)
		die(f'guest is running but its serial socket refused a new broker: {error}')
	broker_pid = dev_fork_broker(serial)
	identity = dict(identity, broker=broker_pid, repo=REPO, host=socket.gethostname())
	dev_identity_write(lock_fd, identity)
	os.close(lock_fd)
	time.sleep(0.2)
	reply = ctl_request(f'WAIT {timeout}', timeout, DEV_CTL_SOCK)
	if not has_prompt(reply[-256:]):
		die(f'reattached, but no shell prompt within {timeout} s (see {DEV_SERIAL_LOG})')
	print(f'lab: reattached to the running instance without rebooting (up {dev_uptime(identity)})')


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

	if state == 'detached':
		# The guest outlived its broker. Put a new one back on the same UART instead of
		# rebooting: QEMU's socket server accepts a fresh connection once the old one is
		# gone, and everything the instance owns - guest, system volume, uptime - survives.
		dev_reattach(lock_fd, identity, timeout)
		return

	for path in (DEV_SERIAL_SOCK, DEV_CTL_SOCK, DEV_CONSOLE_SOCK):
		if os.path.exists(path):
			os.unlink(path)
	open(DEV_SERIAL_LOG, 'wb').close()
	started = time.time()
	# `server` without `nowait`: QEMU blocks until the broker connects, so no boot output
	# is lost. `start_new_session` gives the instance its own process group, which is what
	# `dev-down` stops - never a QEMU that this instance does not own.
	# The development agent, its registry and the control port's transport are behind a
	# compile-time feature that is off everywhere else, so the persistent instance is the one
	# configuration that asks for them. An ordinary build, and every test build, contains no
	# trace of them.
	env = dict(os.environ, SERIAL=f'unix:{DEV_SERIAL_SOCK},server', DEV_PROFILE='1', LIBER_DEVELOPMENT='1')
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
	broker_pid = dev_fork_broker(serial)
	dev_restrict_sockets()
	dev_identity_write(lock_fd, {
		'profile': 'development',
		'repo': REPO,
		'host': socket.gethostname(),
		'broker': broker_pid,
		'pgid': guest.pid,
		'started': started,
		# Taken after the guest booted, so it describes what this instance is actually
		# running. A reattach deliberately keeps it: the broker changed, the guest did not.
		'inputs': instance_inputs(),
	})
	os.close(lock_fd)
	time.sleep(0.2)
	reply = ctl_request(f'WAIT {timeout}', timeout, DEV_CTL_SOCK)
	if not has_prompt(reply[-256:]):
		die(f'no shell prompt within {timeout} s (see {DEV_SERIAL_LOG})')
	# Record which boot this is, now that the guest is up and its agent can answer. Every
	# later session compares against it, so a tool can never publish into, or read a registry
	# from, a guest that restarted since this instance was recorded.
	dev_record_boot()
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
	if state == 'detached':
		print(f'     owner    {dev_describe(identity)}')
		print('     the guest is running but no broker is draining its console')
		print('     reattach without rebooting: `just dev-up`')
		sys.exit(1)
	console = dev_console_stat()
	print(f'     owner    {dev_describe(identity)}')
	print(f'     broker   pid {identity.get("broker", "?")}')
	print(f'     uptime   {dev_uptime(identity)}')
	print(f'     profile  {"development" if dev_profile_logged() else "not reported by the guest"}')
	print(f'     console  {console or "unknown"}')
	print(f'     serial   {os.path.relpath(DEV_SERIAL_LOG, SRC)}')
	if state == 'foreign':
		print('     another worktree owns it; this one cannot use or stop it')
		sys.exit(1)
	if state == 'starting':
		print('     no shell prompt yet; rerun `just dev-status` or watch `just dev-log -f`')
		sys.exit(1)
	shadowed = dev_shadowed()
	if shadowed is None:
		print('     registry not reachable (the control channel is busy or absent)')
	elif not shadowed:
		print('     registry empty; nothing shadows the system volume')
	else:
		print(f'     registry {len(shadowed)} artifact(s) shadowing the system volume:')
		for name, generation, when in shadowed:
			print(f'              {name} generation {generation}, published {when}')
		print('              these override the built image until `just dev-rollback` or a restart')
	stale = instance_stale_inputs(identity)
	if stale is None:
		print('     inputs   not recorded by this instance; restart it to compare')
		sys.exit(1)
	if not stale:
		print(f'     inputs   current ({", ".join(name for name, _, _ in INSTANCE_INPUTS)})')
		sys.exit(0)
	# Name the class, not the symptom. Each of these is a cold invalidation with its own
	# scope, and saying which one changed is what stops it being read as an application
	# edit that could have been published into the running guest.
	print(f'     inputs   stale: {", ".join(stale)}')
	for name in stale:
		print(f'              {name:9}{INPUT_ACTIONS[name]}')
	print('     action   cold restart: `just dev-down && just dev-up`')
	print('              this is a cold invalidation, not a hot-publishable application change')
	sys.exit(1)


def dev_restrict_sockets():
	for path in (DEV_SERIAL_SOCK, DEV_CTL_SOCK, DEV_CONSOLE_SOCK, DEV_CHANNEL_SOCK):
		try:
			os.chmod(path, DEV_SOCKET_MODE)
		except OSError:
			pass


# Refuse a socket anyone but its owner can reach, rather than narrowing it silently: by the
# time a tool is connecting, whoever else could reach it has already had the chance.
def dev_check_socket(path):
	try:
		mode = os.stat(path).st_mode & 0o777
	except OSError as error:
		die(f'cannot check {os.path.relpath(path, SRC)}: {error}')
	if mode & 0o077:
		die(f'{os.path.relpath(path, SRC)} is mode {mode:03o}, reachable beyond its owner; stop the instance with `just dev-down`')


# Ask the guest which boot it is and record it on the instance lock.
def dev_record_boot():
	try:
		sock = proto_connect(5)
	except SystemExit:
		return
	buffer = bytearray()
	try:
		bounds = proto_hello(sock, buffer, 5)
	except (SystemExit, ProtoTimeout, OSError):
		return
	finally:
		sock.close()
	identity = dev_identity_read()
	identity['boot'] = bounds['boot']
	try:
		fd = os.open(DEV_LOCK, os.O_RDWR)
	except OSError:
		return
	try:
		os.ftruncate(fd, 0)
		os.lseek(fd, 0, os.SEEK_SET)
		os.write(fd, json.dumps(identity).encode() + b'\n')
	finally:
		os.close(fd)


# What the guest registry currently shadows, as (name, generation, age). None when the
# control channel cannot be reached at all - a status command reports that rather than
# failing, because a busy channel is not a broken instance.
def dev_shadowed():
	if not os.path.exists(DEV_CHANNEL_SOCK):
		return None
	try:
		sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
		sock.settimeout(2)
		sock.connect(DEV_CHANNEL_SOCK)
	except OSError:
		return None
	buffer = bytearray()
	try:
		proto_hello(sock, buffer, 2)
		artifacts, _ = read_registry(sock, buffer, 2)
		return [(a['name'], a['generations'][-1]['generation'], published_ago(a['generations'][-1]['published_at'])) for a in artifacts]
	except (SystemExit, ProtoTimeout, OSError, struct.error):
		return None
	finally:
		sock.close()


# Detach key: Ctrl-] , the telnet convention. It leaves the guest and the shell running,
# which is the whole point of a detachable console.
DETACH_KEY = b'\x1d'


def cmd_dev_console(args):
	read_only = '--read-only' in args or '--ro' in args
	state, identity = dev_state()
	if state == 'foreign':
		dev_owner_conflict(identity)
	if state in ('down', 'stale'):
		die('no development instance (run `just dev-up` first)')
	if not os.path.exists(DEV_CONSOLE_SOCK):
		die('this instance has no console socket; restart it with `just dev-down && just dev-up`')
	sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
	try:
		sock.connect(DEV_CONSOLE_SOCK)
	except OSError as error:
		die(f'cannot attach to the console: {error}')
	sock.sendall(b'ATTACH ro\n' if read_only else b'ATTACH rw\n')
	granted = recv_line(sock)
	if granted == 'OK ro busy':
		print('lab: another client owns serial input; attached read-only', file=sys.stderr)
	elif granted == 'OK ro':
		print('lab: attached read-only', file=sys.stderr)
	elif granted == 'OK rw':
		print('lab: attached; detach with Ctrl-]', file=sys.stderr)
	else:
		die(f'console refused the attach: {granted!r}')
	console_pump(sock, interactive=not read_only)


def console_pump(sock, interactive):
	# Raw mode so the guest sees keystrokes as typed: no line buffering, no local echo,
	# no host-side interpretation of Ctrl-C, which belongs to the guest's shell.
	restore = None
	if interactive and sys.stdin.isatty():
		import termios
		import tty
		fd = sys.stdin.fileno()
		restore = (fd, termios.tcgetattr(fd))
		tty.setraw(fd)
	out = sys.stdout.buffer
	try:
		sock.setblocking(False)
		watch = [sock] + ([sys.stdin] if interactive else [])
		while True:
			ready, _, _ = select.select(watch, [], [], 0.2)
			if sock in ready:
				try:
					data = sock.recv(65536)
				except BlockingIOError:
					data = None
				except OSError:
					break
				if data == b'':
					break
				if data:
					out.write(data)
					out.flush()
			if interactive and sys.stdin in ready:
				keys = os.read(sys.stdin.fileno(), 4096)
				if not keys or DETACH_KEY in keys:
					sock.sendall(keys.split(DETACH_KEY)[0])
					break
				sock.sendall(keys)
	except KeyboardInterrupt:
		# Only reachable for an observer: an interactive session runs raw, so Ctrl-C is
		# the guest's to handle. Either way, detaching is the right answer, not a trace.
		pass
	finally:
		if restore:
			import termios
			termios.tcsetattr(restore[0], termios.TCSADRAIN, restore[1])
		sock.close()
		print('\r\nlab: detached (the instance keeps running)', file=sys.stderr)


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
	for path in (DEV_SERIAL_SOCK, DEV_CTL_SOCK, DEV_CONSOLE_SOCK, DEV_CHANNEL_SOCK, DEV_LOCK):
		if os.path.exists(path):
			os.unlink(path)
	if stopped:
		print('lab: development instance down')
	else:
		die(f'development instance did not stop; inspect it with `ps -g {pgid}`')


# ---- development-control protocol ------------------------------------------
# The host half of the framing the guest's dev-channel driver speaks, over the second
# virtio-serial device. One 16-byte little-endian header per frame:
#
#   magic u16 | version u8 | opcode u8 | request u32 | generation u32 | length u16 | status u16
#
# Every bound below is a constant on both sides and is reported by the guest in the
# handshake, so a mismatch is a named handshake failure rather than a payload that silently
# does not fit. Every exchange carries a deadline: the socket is a live guest that can stop
# answering at any point, and a control tool that blocks forever on that is unusable.
#
# The protocol carries bytes and typed fields only. Nothing here encodes a host path, a
# guest path, a shell command or a capability request, and no opcode is a passthrough.

PROTO_MAGIC = 0x444c
PROTO_MAGIC_BYTES = struct.pack('<H', PROTO_MAGIC)
PROTO_VERSION = 1
PROTO_HEADER = struct.Struct('<HBBIIHH')
PROTO_MAX_FRAME = 65536
PROTO_MAX_PAYLOAD = PROTO_MAX_FRAME - PROTO_HEADER.size

# How long one handshake attempt waits before being sent again. Comfortably past the guest's
# own deadline for discarding an abandoned fragment, so a retry lands on a resynchronised
# stream rather than into the same swallowed payload.
PROTO_HANDSHAKE_RETRY = 3

OP_HELLO = 0x01
OP_HELLO_ACK = 0x02
OP_PING = 0x03
OP_PONG = 0x04
OP_PUB_BEGIN = 0x10
OP_PUB_CHUNK = 0x11
OP_PUB_COMMIT = 0x12
OP_PUB_ABORT = 0x13
OP_PUB_ACK = 0x14
OP_GEN_LIST = 0x15
OP_GEN_LIST_REPLY = 0x16
OP_ROLLBACK = 0x17
OP_ROLLBACK_ACK = 0x18
OP_TERM_INPUT = 0x20
OP_TERM_ACK = 0x21
OP_RESET = 0x22
OP_RESET_ACK = 0x23
OP_ERROR = 0xff

# Each rejection the guest can name. They exist so a failure is explained by the frame that
# caused it, instead of surfacing as a host-side timeout with no cause.
PROTO_STATUS = {
	0: 'ok',
	1: 'unsupported protocol version',
	2: 'unknown opcode',
	3: 'frame over the size bound',
	4: 'malformed frame',
	5: 'handshake required first',
	6: 'duplicate or out-of-order request id',
	7: 'frame still incomplete at its deadline',
	8: 'a publication is already open',
	9: 'no candidate with that generation',
	10: 'candidate shorter than it declared',
	11: 'candidate does not match its declared digest',
	12: 'no room in the registry',
	13: 'the guest console refused the input',
	14: 'not a readable image',
	15: 'built for a different target than the guest',
	16: 'no canonical image identity record',
	17: 'the image is not the artifact it was published as',
	18: 'a dependency the identity record does not declare',
	19: 'unreadable dynamic metadata',
	20: 'no earlier generation to return to',
	21: 'this boot has no artifact registry (not the development profile)',
	22: 'the installed manifest declares no such artifact',
}

# How a committed generation compares with the one it succeeded, under the written provider
# compatibility rule. "first" is not a weaker "compatible": nothing was replaced.
PROTO_VERDICT = {1: 'hot-publishable', 2: 'needs the cold path', 3: 'unknown (installed artifact unreadable)'}


# Raised when a read runs out of time or the channel closes. A distinct exception rather than
# `die` because the handshake retries: printing the reason on every attempt would report a
# failure the next attempt is about to fix.
class ProtoTimeout(Exception):
	pass


def proto_status(status):
	return PROTO_STATUS.get(status, f'unknown status {status}')


def proto_frame(opcode, request, payload=b'', generation=0, status=0):
	if len(payload) > PROTO_MAX_PAYLOAD:
		die(f'payload of {len(payload)} B exceeds the {PROTO_MAX_PAYLOAD} B frame bound')
	return PROTO_HEADER.pack(PROTO_MAGIC, PROTO_VERSION, opcode, request, generation, len(payload), status) + payload


# Read one frame, hunting for the magic first. On x86_64 the channel carries a UEFI console
# preamble before the guest owns the port - firmware writes its output to every
# console-class device it enumerates - so the reader has to find a frame start rather than
# assume the first byte it is handed is one. `buffer` is a bytearray the caller keeps across
# calls, since one read can deliver several frames or half of one.
def proto_read(sock, buffer, deadline):
	while True:
		at = buffer.find(PROTO_MAGIC_BYTES)
		if at < 0:
			# A magic can straddle two reads, so keep the last byte and discard the rest.
			del buffer[:max(len(buffer) - 1, 0)]
		else:
			del buffer[:at]
			if len(buffer) >= PROTO_HEADER.size:
				_, version, opcode, request, generation, length, status = PROTO_HEADER.unpack(bytes(buffer[:PROTO_HEADER.size]))
				if version != PROTO_VERSION:
					die(f'guest speaks protocol version {version}, this host speaks {PROTO_VERSION}')
				if len(buffer) >= PROTO_HEADER.size + length:
					payload = bytes(buffer[PROTO_HEADER.size:PROTO_HEADER.size + length])
					del buffer[:PROTO_HEADER.size + length]
					return opcode, request, generation, status, payload
		remaining = deadline - time.monotonic()
		if remaining <= 0:
			raise ProtoTimeout('development channel gave no reply before its deadline')
		sock.settimeout(remaining)
		try:
			chunk = sock.recv(4096)
		except socket.timeout:
			raise ProtoTimeout('development channel gave no reply before its deadline') from None
		if not chunk:
			raise ProtoTimeout('development channel closed while waiting for a reply')
		buffer += chunk


# Read frames until the one answering `request` arrives, discarding anything else. The
# discards are real: the guest keeps whatever it was mid-way through writing when a previous
# host walked away, and that reply is flushed the moment a new one connects and drains the
# port. Matching on the request ID is what stops the new session reading the old one's mail.
def proto_await(sock, buffer, request, deadline):
	while True:
		opcode, replied, generation, status, payload = proto_read(sock, buffer, deadline)
		if replied == request:
			return opcode, generation, status, payload


# QEMU listens on the channel socket and serves one client at a time, so a second client is
# a connection that sits in the backlog unanswered rather than one that is refused. The
# deadline on the handshake is what turns that into a diagnosable failure.
def proto_connect(timeout):
	if not os.path.exists(DEV_CHANNEL_SOCK):
		die('this instance has no development channel; restart it with `just dev-down && just dev-up`')
	dev_check_socket(DEV_CHANNEL_SOCK)
	sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
	sock.settimeout(timeout)
	try:
		sock.connect(DEV_CHANNEL_SOCK)
	except OSError as error:
		die(f'development channel refused a connection: {error}')
	return sock


# Handshake, and check the guest's bounds against this host's. It is also the session reset
# point: a virtio-serial port without MULTIPORT reports no open or close, so the guest is
# never told that a host went away, and a HELLO is the only thing that tells it a new one
# has taken over. Request IDs restart from here, which is what makes a reconnect
# deterministic rather than a continuation of the previous host's numbering.
def proto_hello(sock, buffer, timeout):
	deadline = time.monotonic() + timeout
	sock.sendall(proto_frame(OP_HELLO, 1))
	# A previous host that died mid-frame leaves the guest waiting for a payload it will never
	# get, and this handshake is read as part of that payload rather than as a frame. The guest
	# discards the fragment on its own deadline, so a second handshake after that is what
	# recovers - and the alternative is a tool that fails once for what the guest already knows
	# how to fix. Retry until the deadline the caller gave, rather than at a fixed count.
	while True:
		try:
			opcode, _, status, payload = proto_await(sock, buffer, 1, min(time.monotonic() + PROTO_HANDSHAKE_RETRY, deadline))
			break
		except ProtoTimeout as error:
			if time.monotonic() >= deadline:
				die(f'{error}; the guest never completed a handshake')
			sock.sendall(proto_frame(OP_HELLO, 1))
	if opcode == OP_ERROR:
		die(f'handshake refused: {proto_status(status)}')
	if opcode != OP_HELLO_ACK or len(payload) < 36:
		die(f'handshake reply was opcode {opcode:#04x} with {len(payload)} B of payload')
	if len(payload) < 36:
		die(f'handshake reply is {len(payload)} B; this host expects at least 36')
	fields = struct.unpack('<IIHHIHHIB', payload[:25])
	bounds = {'max_frame': fields[0], 'max_payload': fields[1], 'max_outstanding': fields[2], 'max_name': fields[3], 'max_artifact': fields[4], 'max_generations': fields[5], 'max_term_input': fields[6], 'max_registry': fields[7], 'registry': bool(fields[8]), 'boot': payload[28:36].hex()}
	if bounds['max_frame'] != PROTO_MAX_FRAME or bounds['max_payload'] != PROTO_MAX_PAYLOAD:
		die(f'guest reports a {bounds["max_frame"]} B frame bound ({bounds["max_payload"]} B payload); this host is built for {PROTO_MAX_FRAME} B ({PROTO_MAX_PAYLOAD} B)')
	return bounds


# Send one request and return its answer, turning a refusal into a named failure. Every
# caller passes a deadline, because the peer is a live guest that can stop answering.
def proto_request(sock, buffer, request, opcode, payload=b'', generation=0, timeout=5, what='request', tolerate=()):
	sock.sendall(proto_frame(opcode, request, payload, generation))
	try:
		reply, replied_generation, status, body = proto_await(sock, buffer, request, time.monotonic() + timeout)
	except ProtoTimeout as error:
		die(f'{what}: {error}')
	if reply == OP_ERROR and status not in tolerate:
		die(f'{what} refused: {proto_status(status)}')
	return reply, replied_generation, body


# Open a session and describe its bounds in one line, so every command that talks to the
# guest starts from the same reported facts rather than from this host's assumptions.
def proto_session(timeout, announce=True):
	state, identity = dev_state()
	if state == 'foreign':
		dev_owner_conflict(identity)
	if state in ('down', 'stale'):
		die('no development instance (run `just dev-up` first)')
	if state == 'detached':
		die('the guest is running but no broker owns it; reattach with `just dev-up`')
	sock = proto_connect(timeout)
	buffer = bytearray()
	bounds = proto_hello(sock, buffer, timeout)
	# The guest draws a value once per boot and reports it in every handshake. An instance is
	# meant to outlive the tools that drive it, so a tool can be talking to a guest that
	# restarted under it; comparing against what `dev-up` recorded turns that into a refusal
	# instead of a publication into the wrong boot.
	recorded = identity.get('boot')
	if recorded and recorded != bounds['boot']:
		die(f'the guest has restarted since this instance was recorded (boot {recorded[:16]} -> {bounds["boot"][:16]}); rerun `just dev-up`')
	if announce:
		registry = f'registry {bounds["max_registry"] // (1024 * 1024)} MB, {bounds["max_generations"]} generations per artifact' if bounds['registry'] else 'no registry on this boot'
		print(f'lab: handshake ok (protocol v{PROTO_VERSION}, max frame {bounds["max_frame"]} B, max artifact {bounds["max_artifact"] // (1024 * 1024)} MB, {registry}, max terminal input {bounds["max_term_input"]} B)')
	return sock, buffer, bounds


def cmd_dev_ping(args):
	count = arg_value(args, '--count', 1)
	size = arg_value(args, '--size', 0)
	timeout = arg_value(args, '--timeout', 5)
	if size > PROTO_MAX_PAYLOAD:
		die(f'--size {size} exceeds the {PROTO_MAX_PAYLOAD} B payload bound')
	sock, buffer, _ = proto_session(timeout)
	try:
		# Request IDs must be non-zero and strictly increasing; the handshake took 1. A ping
		# echoes its payload, so this measures the round trip of real bytes and proves the
		# frame arrived intact rather than merely arrived.
		payload = bytes(i & 0xff for i in range(size))
		for i in range(count):
			request = 2 + i
			started = time.monotonic()
			opcode, _, echo = proto_request(sock, buffer, request, OP_PING, payload, timeout=timeout, what=f'ping {request}')
			elapsed = (time.monotonic() - started) * 1000
			if opcode != OP_PONG or echo != payload:
				die(f'ping {request} answered with opcode {opcode:#04x}, {len(echo)} B echoed of {len(payload)} B sent')
			print(f'lab: pong in {elapsed:.1f} ms (request {request}, {len(echo)} B echoed)')
	finally:
		sock.close()


# Publish one artifact: declare it, stream it, commit it. The host reads a local file and
# sends its bytes; the path never crosses the wire, only the bytes and the typed metadata
# the guest checks them against. An abort on any host-side failure is what stops a killed
# publish leaving a candidate parked in the guest until its deadline.
def cmd_dev_publish(args):
	timeout, rest = take_arg(args, '--timeout', 15)
	rest = [a for a in rest if not a.startswith('--')]
	if len(rest) != 2:
		die('usage: dev-publish <name> <file>')
	name, path = rest[0], rest[1]
	if not NAME_OK.fullmatch(name):
		die(f'artifact name {name!r} must be 1-48 characters of letters, digits, dot, dash or underscore, and may not start with a dot')
	try:
		with open(path, 'rb') as handle:
			blob = handle.read()
	except OSError as error:
		die(f'cannot read {path}: {error}')
	if not blob:
		die(f'{path} is empty')
	digest = hashlib.sha256(blob).digest()
	sock, buffer, bounds = proto_session(timeout)
	try:
		if not bounds['registry']:
			die('this boot has no artifact registry; publication needs the development profile')
		if len(blob) > bounds['max_artifact']:
			die(f'{path} is {len(blob)} B; the guest accepts at most {bounds["max_artifact"]} B')
		encoded = name.encode()
		if len(encoded) > bounds['max_name']:
			die(f'name is {len(encoded)} B; the guest accepts at most {bounds["max_name"]} B')
		started = time.monotonic()
		begin = struct.pack('<I', len(blob)) + digest + bytes([len(encoded)]) + encoded
		_, generation, body = proto_request(sock, buffer, 2, OP_PUB_BEGIN, begin, timeout=timeout, what='publication begin')
		print(f'lab: publishing {name} as generation {generation} ({len(blob)} B, sha256 {digest.hex()[:16]}...)')
		request = 3
		sent = 0
		try:
			while sent < len(blob):
				chunk = blob[sent:sent + bounds['max_payload']]
				_, _, body = proto_request(sock, buffer, request, OP_PUB_CHUNK, chunk, generation, timeout, f'chunk at offset {sent}')
				sent += len(chunk)
				acked = struct.unpack('<I', body[:4])[0] if len(body) >= 4 else -1
				if acked != sent:
					die(f'guest acknowledged {acked} B after {sent} B were sent')
				request += 1
			_, _, body = proto_request(sock, buffer, request, OP_PUB_COMMIT, b'', generation, timeout, 'commit')
		except SystemExit:
			# The guest would drop the candidate on its own deadline; aborting now returns the
			# megabyte it is holding immediately instead.
			try:
				sock.sendall(proto_frame(OP_PUB_ABORT, request + 1, b'', generation))
			except OSError:
				pass
			raise
		elapsed = (time.monotonic() - started) * 1000
		print(f'lab: generation {generation} committed in {elapsed:.1f} ms ({sent} B, digest verified by the guest)')
		# The verdict is recorded, never a gate: the artifact is in the registry either way,
		# and what this decides is whether installing it could be a hot swap.
		if len(body) >= 4:
			verdict, detail_len = body[2], body[3]
			detail = body[4:4 + detail_len].decode(errors='replace')
			print(f'lab: compatibility {PROTO_VERDICT.get(verdict, verdict)}' + (f' - {detail}' if detail else ''))
	finally:
		sock.close()


# Type into the guest's terminal over the control channel rather than into its UART. The
# difference that matters is accounting: the guest reports how many bytes its console
# actually took, so a refusal is a number to resume from instead of output that never
# appeared for reasons nobody can see.
#
# It has to resume, too. The console input queue is short - measured at a few dozen bytes -
# so anything past a keystroke or two is refused partway and the rest has to follow once the
# shell has drained. Looping here is what makes the operation carry a line rather than a
# character, and the guest's count is what keeps the loop from replaying bytes that landed.
def cmd_dev_type(args):
	timeout, rest = take_arg(args, '--timeout', 15)
	enter = '--no-enter' not in rest
	text = ' '.join(a for a in rest if not a.startswith('--'))
	if not text:
		die('usage: dev-type [--no-enter] <text...>')
	payload = text.encode() + (b'\r' if enter else b'')
	sock, buffer, bounds = proto_session(timeout)
	try:
		if len(payload) > bounds['max_term_input']:
			die(f'{len(payload)} B of input; the guest accepts at most {bounds["max_term_input"]} B per frame')
		deadline = time.monotonic() + timeout
		request, sent = 2, 0
		while sent < len(payload):
			opcode, _, body = proto_request(sock, buffer, request, OP_TERM_INPUT, payload[sent:], timeout=max(deadline - time.monotonic(), 0.1), what='terminal input', tolerate=(13,))
			accepted = struct.unpack('<H', body[:2])[0] if len(body) >= 2 else 0
			sent += accepted
			request += 1
			if opcode == OP_TERM_ACK:
				break
			if time.monotonic() >= deadline:
				die(f'the guest console took {sent} of {len(payload)} B before the deadline')
			# Nothing was taken this round, so the queue is still full: let the shell drain
			# before asking again, rather than spinning a refusal into a busy loop.
			if accepted == 0:
				time.sleep(0.05)
		print(f'lab: typed {sent} B into the guest console')
	finally:
		sock.close()


# Drop the development state the guest holds. It is not a reboot: it clears the artifact
# registry and any half-streamed candidate, and the guest reports exactly what it dropped.
def cmd_dev_reset(args):
	timeout = arg_value(args, '--timeout', 5)
	sock, buffer, _ = proto_session(timeout)
	try:
		opcode, _, body = proto_request(sock, buffer, 2, OP_RESET, timeout=timeout, what='reset')
		if opcode != OP_RESET_ACK or len(body) < 3:
			die(f'reset answered with opcode {opcode:#04x} and {len(body)} B')
		dropped = struct.unpack('<H', body[:2])[0]
		candidate = 'and one candidate being streamed' if body[2] else 'with no candidate open'
		print(f'lab: dropped {dropped} generation(s) {candidate}')
	finally:
		sock.close()


# Read the registry: every artifact, and the generations retained for it. The newest
# generation of each is what currently shadows the system volume, which is the thing worth
# seeing at a glance - a forgotten override is a fix that appears not to have worked.
def read_registry(sock, buffer, timeout):
	opcode, _, body = proto_request(sock, buffer, 2, OP_GEN_LIST, timeout=timeout, what='registry query')
	if opcode != OP_GEN_LIST_REPLY or len(body) < 6:
		die(f'registry query answered with opcode {opcode:#04x} and {len(body)} B')
	count, registry_bytes = struct.unpack('<HI', body[:6])
	at = 6
	artifacts = []
	for _ in range(count):
		name_len = body[at]
		name = body[at + 1:at + 1 + name_len].decode(errors='replace')
		at += 1 + name_len
		held = body[at]
		at += 1
		generations = []
		for _ in range(held):
			generation, length = struct.unpack('<II', body[at:at + 8])
			digest = body[at + 8:at + 40]
			published_at = struct.unpack('<Q', body[at + 40:at + 48])[0]
			verdict, detail_len = body[at + 48], body[at + 49]
			detail = body[at + 50:at + 50 + detail_len].decode(errors='replace')
			at += 50 + detail_len
			generations.append({'generation': generation, 'length': length, 'digest': digest, 'published_at': published_at, 'verdict': verdict, 'detail': detail})
		artifacts.append({'name': name, 'generations': generations})
	return artifacts, registry_bytes


def published_ago(published_at):
	if not published_at:
		return 'unknown time'
	seconds = max(int(time.time()) - published_at, 0)
	if seconds < 90:
		return f'{seconds} s ago'
	if seconds < 5400:
		return f'{seconds // 60} min ago'
	return f'{seconds // 3600} h ago'


def cmd_dev_generations(args):
	timeout = arg_value(args, '--timeout', 5)
	sock, buffer, bounds = proto_session(timeout)
	try:
		artifacts, registry_bytes = read_registry(sock, buffer, timeout)
		if not artifacts:
			print('lab: the guest registry is empty; nothing shadows the system volume')
			return
		print(f'lab: {len(artifacts)} artifact(s), {registry_bytes} B of {bounds["max_registry"]} B registry')
		for artifact in artifacts:
			shadowing = artifact['generations'][-1]
			print(f'     {artifact["name"]} - shadowed by generation {shadowing["generation"]}, published {published_ago(shadowing["published_at"])}')
			for entry in artifact['generations']:
				marker = '*' if entry is shadowing else ' '
				print(f'     {marker}  {entry["generation"]:<4} {entry["length"]:>9} B  sha256 {entry["digest"].hex()[:16]}...  {PROTO_VERDICT.get(entry["verdict"], entry["verdict"])}')
				if entry['detail']:
					print(f'            {entry["detail"]}')
	finally:
		sock.close()


# Return an artifact to the generation before its newest. Named on purpose: overshooting is
# as common as failing in a development loop, and undoing it should not mean republishing an
# image the host may no longer have.
def cmd_dev_rollback(args):
	timeout, rest = take_arg(args, '--timeout', 5)
	rest = [a for a in rest if not a.startswith('--')]
	if len(rest) != 1:
		die('usage: dev-rollback <name>')
	name = rest[0].encode()
	sock, buffer, _ = proto_session(timeout)
	try:
		opcode, _, body = proto_request(sock, buffer, 2, OP_ROLLBACK, bytes([len(name)]) + name, timeout=timeout, what=f'rollback of {rest[0]}')
		if opcode != OP_ROLLBACK_ACK or len(body) < 8:
			die(f'rollback answered with opcode {opcode:#04x} and {len(body)} B')
		now, dropped = struct.unpack('<II', body[:8])
		print(f'lab: {rest[0]} rolled back to generation {now}; generation {dropped} discarded')
	finally:
		sock.close()


# ---- scenarios -------------------------------------------------------------
# The bridge between the scenario runner and this instance. The runner is deliberately given
# an object rather than this module: it decides what a scenario means, and everything about
# how the guest is actually reached stays here.


class LabGuest:
	def __init__(self, timeout):
		self.timeout = timeout

	def serial_size(self):
		try:
			return os.path.getsize(DEV_SERIAL_LOG)
		except OSError:
			return 0

	def serial_since(self, at):
		try:
			with open(DEV_SERIAL_LOG, 'rb') as handle:
				handle.seek(at)
				return strip_ansi(handle.read()).decode(errors='replace')
		except OSError:
			return ''

	def wait_prompt(self, timeout):
		try:
			return has_prompt(ctl_request(f'WAIT {timeout}', timeout, DEV_CTL_SOCK)[-256:])
		except (SystemExit, OSError):
			return False

	# Terminal input, resuming from the count the guest reports: the console queue is short,
	# so anything past a keystroke or two is accepted in pieces.
	def type_text(self, text, enter, timeout):
		payload = text.encode() + (b'\r' if enter else b'')
		sock, buffer, bounds = proto_session(timeout, announce=False)
		try:
			if len(payload) > bounds['max_term_input']:
				return False
			end = time.monotonic() + timeout
			request, sent = 2, 0
			while sent < len(payload):
				opcode, _, body = proto_request(sock, buffer, request, OP_TERM_INPUT, payload[sent:], timeout=max(end - time.monotonic(), 0.1), what='terminal input', tolerate=(13,))
				sent += struct.unpack('<H', body[:2])[0] if len(body) >= 2 else 0
				request += 1
				if opcode == OP_TERM_ACK:
					return True
				if time.monotonic() >= end:
					return False
				time.sleep(0.05)
			return True
		finally:
			sock.close()

	def publish(self, artifact, path, timeout):
		return subprocess.run([os.path.join(HERE, 'lab.py'), 'dev-publish', artifact, path, '--timeout', str(timeout)], cwd=SRC, capture_output=True).returncode == 0

	def reset(self, timeout):
		return subprocess.run([os.path.join(HERE, 'lab.py'), 'dev-reset', '--timeout', str(timeout)], cwd=SRC, capture_output=True).returncode == 0


def cmd_dev_scenario(args):
	import scenario

	verbose = '--verbose' in args
	rest = [a for a in args if not a.startswith('--')]

	if not rest:
		die('usage: dev-scenario [--verbose] <file.toml>...')
	documents = []
	for path in rest:
		try:
			documents.append((path, scenario.load(path)))
		except scenario.ScenarioError as error:
			die(str(error))
	# Every scenario is validated before any of them runs, so a typo in the last file does not
	# surface after the first three have already changed the guest.
	state, identity = dev_state()
	if state == 'foreign':
		dev_owner_conflict(identity)
	if state != 'ready':
		die(f'development instance is {state}; scenarios need a ready one (`just dev-up`)')
	failures = 0
	for path, document in documents:
		name = document['name']
		try:
			elapsed = scenario.run(document, LabGuest(30), verbose)
			print(f'lab: {name} passed in {elapsed:.1f} s ({os.path.relpath(path, SRC)})')
		except scenario.ScenarioError as error:
			print(f'lab: {name} FAILED: {error}', file=sys.stderr)
			failures += 1
	if failures:
		die(f'{failures} of {len(documents)} scenario(s) failed')
	print(f'lab: {len(documents)} scenario(s) passed')


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


COMMANDS = {'boot': cmd_boot, 'sh': cmd_sh, 'int': cmd_int, 'wait': cmd_wait, 'log': cmd_log, 'key': cmd_key, 'monitor': cmd_monitor, 'usb-attach': cmd_usb_attach, 'usb-detach': cmd_usb_detach, 'pcap': cmd_pcap, 'test': cmd_test, 'shot': cmd_shot, 'quit': cmd_quit, 'dev-up': cmd_dev_up, 'dev-status': cmd_dev_status, 'dev-console': cmd_dev_console, 'dev-log': cmd_dev_log, 'dev-ping': cmd_dev_ping, 'dev-publish': cmd_dev_publish, 'dev-generations': cmd_dev_generations, 'dev-rollback': cmd_dev_rollback, 'dev-type': cmd_dev_type, 'dev-reset': cmd_dev_reset, 'dev-scenario': cmd_dev_scenario, 'dev-down': cmd_dev_down}


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
