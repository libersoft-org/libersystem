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

import contextlib
import fcntl
import glob
import hashlib
import json
import os
import re
import select
import shutil
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
  dev-reboot            reboot the guest to a clean state; the volume and the instance survive
  dev-restart           replace the development agent, keeping the guest running
  dev-key <key|chord>... | --text <line>  key events through the emulated keyboard
  dev-pointer [--x F] [--y F] [--press|--release|--click] [button]  pointer events
  dev-test [--verbose] <file.toml>...  run declarative application scenarios
  dev-launch <program> [args...]  launch a program through PermissionManager, print output
  dev-stop              end the program launched through the control channel
  dev-loop <artifact> <scenario.toml>...  build, publish and run; stops at the failing phase
  dev-clean [--dry-run]  prune host-side leftovers against documented limits
  dev-down              stop it gracefully and release the lock"""

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
SRC = os.path.dirname(HERE)


# The same machine-readable phase events the shared-image builder, the kernel test driver and the
# QEMU runner emit: one host-nanosecond timestamp, a phase and an event, appended to whatever
# file `LIBER_TIMING_LOG` names. Written here too so that a loop iteration can be attributed
# across the host and the guest from one file, rather than from a build log and a stopwatch.
def timing_event(phase, event):
	path = os.environ.get('LIBER_TIMING_LOG')
	if not path:
		return
	try:
		with open(path, 'a') as handle:
			handle.write(f'{time.time_ns()}\t{phase}\t{event}\n')
	except OSError:
		pass
REPO = os.path.dirname(SRC)
BUILD_ROOT = os.path.join(REPO, '.build')

# The two entry points this file drives, BY PATH.
#
# It used to name Justfile recipes - `just run`, `just user kernel-build`, `just user-aarch64` -
# and none of the three exists any more: building moved to `build.sh` and booting to `run.sh` at the
# repository root, and the recipes went with the move. Nothing noticed, because every caller here
# reports the failure as something else: `dev-up` waits for a QEMU that was never started and
# blames the build timeout, and `scenario-cold` dies with "the userspace did not build" whatever
# went wrong. Both were, in fact, `error: justfile does not contain recipe`.
#
# Paths rather than recipe names, so a missing entry point is a missing FILE - which is a thing this
# file can check for and say plainly.
RUN_SH = os.path.join(REPO, 'run.sh')
BUILD_SH = os.path.join(REPO, 'build.sh')


# The QEMU command a lab guest is started with. `run.sh` builds first and then execs the runner, so
# the process this returns is the one that becomes QEMU - which is what `dev_guest_qemu` waits for
# and what `record_lab_guest` takes the process group from.
# EVERY CHILD THIS FILE STARTS HAS A DEADLINE, and a child that ignores TERM is killed.
#
# `dev-loop`, the scenario wrappers, the cold build and the artifact build all ran children with no
# host timeout at all. A command documented as bounded by construction could therefore run
# indefinitely: the deadline was a value passed to some GUEST requests, not a boundary on the host
# processes those requests were made from. A child hanging in build logic, socket setup or QEMU
# before reaching its own internal timeout was outside every bound in the system.
#
# `subprocess.run` with a timeout kills the child on expiry but not its descendants, so the child
# gets a session of its own and the whole group is signalled - TERM first, KILL after a grace
# period, which is what stops a cold QEMU that ignores TERM from surviving the command that started
# it. Returns a `CompletedProcess`; on expiry the returncode is negative and `timed_out` is set.
# How much longer than its own declared timeout a child is given before the host stops it. Enough
# for a child that is going to report its own failure to do so - that message is more useful than
# "the host killed it" - and short enough that the step's bound still means something.
CHILD_GRACE = 15


class ChildTimeout(Exception):
	def __init__(self, command, seconds):
		super().__init__(f'{os.path.basename(command[0])} did not finish within {seconds:.0f} s')
		self.command = command
		self.seconds = seconds


def run_child(command, timeout, **kwargs):
	kwargs.setdefault('start_new_session', True)
	child = subprocess.Popen(command, **kwargs)
	try:
		out, err = child.communicate(timeout=timeout)
		return subprocess.CompletedProcess(command, child.returncode, out, err)
	except subprocess.TimeoutExpired:
		stop_child_group(child)
		raise ChildTimeout(command, timeout) from None


# TERM the child's whole group, then KILL what is left. The group is what matters: a build script
# that starts a compiler, or a runner that starts QEMU, leaves those behind when only the leader is
# signalled.
def stop_child_group(child, grace=5):
	try:
		group = os.getpgid(child.pid)
	except OSError:
		return
	for signal_number in (signal.SIGTERM, signal.SIGKILL):
		try:
			os.killpg(group, signal_number)
		except OSError:
			return
		try:
			child.wait(timeout=grace)
			return
		except subprocess.TimeoutExpired:
			continue


def run_command(displays):
	if not os.path.exists(RUN_SH):
		die(f'{RUN_SH} is missing - the lab boots the system through it')
	command = [RUN_SH]
	if displays:
		command += ['--display', ','.join(displays)]
	return command


# What a target has to be built with before a cold guest can boot it: the userspace, the kernel and
# the boot packages the loader hands over. `build.sh` with no `--part` builds every part, which is
# what this wants - the old recipe pair (`user` + `kernel-build`) predates the packages being a
# build part at all.
def build_command(target):
	if not os.path.exists(BUILD_SH):
		die(f'{BUILD_SH} is missing - the lab builds the system through it')
	return [BUILD_SH, '--arch', target]
BUILD = os.path.join(BUILD_ROOT, 'boot')
SERIAL_SOCK = os.path.join(BUILD, 'lab-serial.sock')
CTL_SOCK = os.path.join(BUILD, 'lab-ctl.sock')
SERIAL_LOG = os.path.join(BUILD, 'lab-serial.log')
QEMU_LOG = os.path.join(BUILD, 'lab-qemu.log')
MON_SOCK = os.path.join(BUILD, 'qemu-monitor.sock')
QMP_SOCK = os.path.join(BUILD, 'qemu-qmp.sock')

# The monitor and QMP endpoints in use. The persistent instance's by default; a cold run points
# them at the guest it started, alongside the control channel and the console log. All four move
# together or a scenario drives one guest and reads another.
MON_OVERRIDE = None
QMP_OVERRIDE = None


def mon_sock():
	return MON_OVERRIDE or MON_SOCK


def qmp_sock():
	return QMP_OVERRIDE or QMP_SOCK


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

# How long a prompt match has to stand with no further guest output before the broker calls a
# request finished. See the note in `serve_request`: this is what stops a foreground program that
# prints a prompt-shaped line from ending someone else's command.
PROMPT_SETTLE = 0.25

# The broker's reply frame. `<magic> <outcome> <length>\n` then exactly `<length>` bytes.
#
# There was no frame at all: the reply was the collected bytes, so the caller could not tell a
# prompt from a timeout from a closed serial line. The outcomes are named rather than inferred.
REPLY_MAGIC = b'LIBERLAB1'
REPLY_OUTCOMES = ('prompt', 'timeout', 'closed', 'status', 'unsupported')


class Reply:
	__slots__ = ('outcome', 'data')

	def __init__(self, outcome, data):
		self.outcome = outcome
		self.data = data

	# The one question almost every caller asks. Named so no caller has to re-derive it by running
	# the prompt regex over the bytes again - which is what they all did, and which cannot tell a
	# timeout whose tail happens to hold a prompt from a request the prompt actually ended.
	@property
	def prompted(self):
		return self.outcome == 'prompt'


def broker_reply(conn, outcome, data):
	try:
		conn.sendall(REPLY_MAGIC + f' {outcome} {len(data)}\n'.encode() + data)
	except OSError:
		pass


# The instance's console output, as a file the broker appends to. Reading it is how anything
# here watches the guest say something without taking the console away from whoever has it.
# Where the guest's console output is read from. The persistent instance's log by default; a
# cold run points it at that guest's own log, the same way it points the control channel at that
# guest's own socket. Both have to move together, or a scenario types into one guest and reads
# another's screen.
SERIAL_OVERRIDE = None


def serial_log_path():
	return SERIAL_OVERRIDE or DEV_SERIAL_LOG


def serial_size():
	try:
		return os.path.getsize(serial_log_path())
	except OSError:
		return 0


def serial_since(at):
	return strip_ansi(serial_raw_since(at)).decode(errors='replace')


# The same bytes with nothing removed. Terminal restoration is asserted here and nowhere else:
# putting a terminal back is escape sequences and only escape sequences, so the reader that
# strips them cannot see whether it happened.
def serial_raw_since(at):
	return serial_read(at)[0]


# THE BYTES AND WHERE THEY END, from ONE open descriptor.
#
# The cursor used to be advanced with a separate `serial_size()` after the read - and the broker
# appends to this file the whole time. Bytes landing between the read reaching EOF and the size
# being taken were skipped: the cursor moved past them although no oracle ever saw them. That is a
# false failure when the skipped bytes hold a marker a step was waiting for, and a false PASS when
# they hold one a step forbade - `absent` is exactly that shape.
#
# Nothing about the file system makes this rare. Two adjacent lines of guest output are enough.
def serial_read(at):
	try:
		with open(serial_log_path(), 'rb') as handle:
			handle.seek(at)
			data = handle.read()
			return data, at + len(data)
	except OSError:
		return b'', at


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


# Never block on a client. A terminal that stopped reading must not hold up the guest.
#
# WHAT IT MUST NOT DO IS LOSE BYTES SILENTLY. A nonblocking `send` may accept only a PREFIX without
# raising at all, and the return value was discarded - so the unsent tail simply vanished, on a
# connection that stayed attached and therefore never asked for the replay that would have covered
# the gap. A live console then showed truncated escape sequences, half prompts and missing log
# lines while looking like a continuous stream, and anything using that stream as an oracle was
# reading a different guest from the one that was running.
#
# Backpressure disconnects instead. Replay is bounded and starts from a known point, so a client
# that reconnects gets a window it can reason about; a client that keeps its connection and quietly
# misses the middle cannot tell that it did.
def broker_send(state, client, data):
	try:
		sent = client['sock'].send(data)
	except BlockingIOError:
		broker_drop(state, client)
		return
	except OSError:
		broker_drop(state, client)
		return
	if sent < len(data):
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
		broker_reply(conn, 'unsupported', b'')
		return True
	if parts[0] == 'STAT':
		# Console occupancy, asked over the control channel rather than over the console
		# socket, so querying it never touches the human byte stream.
		readers = sum(1 for c in state['clients'] if not c['writer'])
		broker_reply(conn, 'status', f'writer={1 if state["writer"] else 0} readers={readers}\n'.encode())
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
		# A request this broker does not implement is answered, not dropped. Silence reaches the
		# caller as an empty read that looks like every other empty read.
		broker_reply(conn, 'unsupported', b'')
		return True
	# Collect serial output until the prompt returns or the timeout passes. Everything still goes
	# through the shared drain, so an attached console keeps seeing the guest while a scripted
	# command runs.
	#
	# WHAT HAPPENED IS PART OF THE ANSWER. This used to send the collected bytes and nothing else in
	# both cases, so a request that timed out and a request whose command finished were WIRE
	# IDENTICAL: a hung command that had printed something looked exactly like a successful one, and
	# `lab sh` exited 0 on it. That is a false green in the one tool a person is most likely to wrap
	# in a script.
	#
	# AND A PROMPT HAS TO SETTLE. `has_prompt` is a regex over terminal bytes and any foreground
	# program can print that shape - `program output\nvol://attacker> ` matches, measured. Nothing
	# on this side can prove a prompt came from the shell; what it can do is refuse to accept one
	# that is still being written past. A real prompt is followed by silence until the next input, so
	# a match must survive PROMPT_SETTLE seconds with no further bytes. A program that prints a
	# prompt shape and keeps running no longer ends the request. A program that prints one and then
	# blocks in silence still does, and closing that needs an unforgeable per-request sentinel from
	# the guest shell, which is guest-side work this cannot do alone.
	deadline = time.time() + timeout
	outcome = 'timeout'
	settled_at = None
	while time.time() < deadline:
		ready, _, _ = select.select([serial], [], [], 0.2)
		if serial in ready:
			data = serial.recv(65536)
			if not data:
				broker_reply(conn, 'closed', collected)
				return False
			broker_absorb(state, data)
			collected += data
			settled_at = None
		if has_prompt(collected[-256:]):
			if settled_at is None:
				settled_at = time.time()
			elif time.time() - settled_at >= PROMPT_SETTLE:
				outcome = 'prompt'
				break
		else:
			settled_at = None
	broker_reply(conn, outcome, collected)
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
	return parse_reply(reply)


# Take the frame apart, and refuse anything that is not one.
#
# A broker that answers without a frame is one from before this protocol existed - a persistent
# instance that was already running when the tree changed under it. That is reported rather than
# guessed at: reading its bytes as a successful reply is exactly the false green the frame was added
# to remove, and the fix (`just lab dev-down && just lab dev-up`) is one line to state.
def parse_reply(reply):
	header, newline, body = reply.partition(b'\n')
	parts = header.split(b' ')
	if not newline or len(parts) != 3 or parts[0] != REPLY_MAGIC:
		die('the instance answered without a reply frame - it predates this protocol; restart it with `just lab dev-down && just lab dev-up`')
	outcome = parts[1].decode(errors='replace')
	if outcome not in REPLY_OUTCOMES:
		die(f'the instance answered with an unknown outcome {outcome!r}')
	try:
		length = int(parts[2])
	except ValueError:
		die('the instance answered with a malformed reply frame')
	# Short is a truncated answer and must not be read as a complete one; long would mean the frame
	# and the payload disagree, which is the same defect from the other side.
	if length != len(body):
		die(f'the instance answered with {len(body)} B of payload, its frame declares {length} B')
	return Reply(outcome, body)


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
	('packages', [], ['.build/boot/init-x86_64.pkg', '.build/boot/volume-x86_64.pkg']),
	('image', [], ['.build/boot/libersystem.iso', 'src/boot/mkimage.sh']),
	('topology', [], ['src/boot/qemu-run.sh']),
	# THE CLASSES THAT WERE MISSING, and each of them can change what a guest is running while
	# every fingerprint above stays equal.
	#
	# `payload`: the factory volume directory and the product configuration. Editing a payload
	# source without rebuilding leaves the built package bytes alone, so `packages` reported the
	# instance current against a tree whose content had moved.
	# `build`: the scripts that decide what gets built and how it is assembled. They are not inside
	# any source root above, so a changed build recipe was invisible to every class.
	# `manifest`: the single statement of what the system contains and where each part goes.
	('payload', ['src/volume'], ['product.conf']),
	('build', ['src/tools'], ['build.sh', 'image.sh', 'run.sh', 'lib.sh', 'src/Justfile']),
	('manifest', [], ['src/user/services/manifest.toml']),
)

INPUT_ACTIONS = {
	'payload': 'factory volume content or product configuration: reassembles the system volume and the image',
	'build': 'build or packaging recipe: what every artifact is made of changed, so nothing below can be trusted as current',
	'manifest': 'the system manifest: which components ship and where they are placed on the medium',
	'protocol': 'shared boot contract: rebuilds every binary that consumes it, kernel and loader alike',
	'kernel': 'recompiles the kernel and reassembles the image; an unchanged loader is not recompiled',
	'loader': 'recompiles the loader and reassembles the image; an unchanged kernel is not recompiled',
	'packages': 'reassembles the boot image from unchanged binaries',
	'image': 'reassembles the boot image from unchanged binaries',
	'topology': 'restarts the VM; the boot image is not rebuilt',
}



# EVERY FILE UNDER THE ROOT, not four suffixes.
#
# It hashed `.rs`, `.toml`, `.lock` and `.ld` and nothing else, so a linker script with another
# extension, an assembly file, a generated header, a build script, a JSON target specification or a
# payload the build reads was invisible: the instance reported itself current against a tree it no
# longer matched. The exclusions are build OUTPUT directories, which is a different thing from a
# suffix allowlist - a directory of products can be named, and the set of source kinds cannot.
#
# `unreadable` is recorded as a distinct marker per path rather than folded into the digest silently,
# so a file that cannot be read changes the fingerprint instead of vanishing from it.
IGNORED_TREE_DIRS = ('target', '.build', '__pycache__', '.git')


def digest_tree(root, digest):
	for base, dirs, names in os.walk(root):
		dirs[:] = sorted(d for d in dirs if d not in IGNORED_TREE_DIRS)
		for name in sorted(names):
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


# Whether this process has already compared the instance against the tree it was built from.
# The comparison walks and hashes several source trees, which is cheap once and wasteful per
# request - and a scenario opens a session per step.
_inputs_compared = False


# Say so, once, when the running instance is older than the tree the caller is working in.
#
# Every command that speaks to the guest passes through here, so this is where a cold
# invalidation gets explained rather than discovered later as a confusing failure: publishing
# an artifact built against a changed boot contract into a guest that predates it is the case
# worth naming out loud. It warns and does not refuse, because which of the two the caller
# meant is not knowable from here - a kernel edit does not make typing at the terminal wrong -
# and because `dev-status` is the command that exists to answer it definitively.
def warn_stale_inputs(identity):
	global _inputs_compared
	# A command that composes others answers this once and says so, so its children do not each
	# pay for the same walk. The comparison is around a quarter of a second, which is nothing
	# to a person and a noticeable share of a warm loop.
	if _inputs_compared or os.environ.get('LIBER_DEV_INPUTS_CHECKED') == '1':
		return
	_inputs_compared = True
	stale = instance_stale_inputs(identity)
	if not stale:
		return
	print(f'lab: WARNING: this instance predates the tree: {", ".join(stale)} changed since it booted', file=sys.stderr)
	for name in stale:
		print(f'     {name:9}{INPUT_ACTIONS[name]}', file=sys.stderr)
	print('     a cold restart (`just lab dev-down && just lab dev-up`) is what picks it up; hot publication cannot', file=sys.stderr)


def dev_identity_read():
	try:
		with open(DEV_LOCK) as handle:
			return json.load(handle)
	except (OSError, ValueError):
		return {}


# WHICH BOOT THIS INSTANCE OWNS, in a file of its own.
#
# The guest draws a value once per boot and reports it in every handshake, and comparing against it
# is what stops a publication, a rollback or a registry read from landing in a different boot from
# the one the instance record describes. That value used to be written by reopening the LIVE LOCK
# FILE and truncating it: a crash or a concurrent read during the rewrite left an empty or partial
# JSON document, which `dev_identity_read` returns as `{}` - so status misread a live locked
# instance as merely starting, and the boot generation was simply gone.
#
# A sidecar, published by rename, so a reader sees the old record or the new one and never half of
# either. KEYED TO THE LOCK'S INODE: the lock is created once per instance and unlinked by
# `dev-down`, so a sidecar naming a different inode belongs to an instance that is gone, and reading
# it would bind this session to a boot generation from a previous guest.
DEV_BOOT_RECORD = os.path.join(BUILD, 'dev-instance.boot')

# ONE SCENARIO RUN AT A TIME, PER INSTANCE.
#
# Scenarios share everything the instance has: the terminal, the control channel, the artifact
# registry, the single launch slot, the fixture area, the reset and restart operations, and the
# teardown that gives all of it back. Two host processes could both see `ready` and begin - and then
# one run's reset erased the other's registry, one run's teardown stopped the other's program, and
# their output cursors read each other's bytes. The failure lands on whichever run notices the
# other's state, which is not the one that caused it.
#
# An flock, so it is released by the kernel when the holder dies: a killed run does not leave a
# lease to break by hand, and "is somebody running scenarios" is answered by asking the lock rather
# than by trusting a file to have been cleaned up.
DEV_SCENARIO_LEASE = os.path.join(BUILD, 'dev-scenario.lease')


@contextlib.contextmanager
def scenario_lease(what):
	fd = os.open(DEV_SCENARIO_LEASE, os.O_RDWR | os.O_CREAT, 0o600)
	try:
		fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
	except OSError:
		os.close(fd)
		die(f'another scenario run already holds this instance; {what} would interleave with it - wait for it to finish')
	try:
		os.ftruncate(fd, 0)
		os.write(fd, f'{os.getpid()} {what}\n'.encode())
		yield
	finally:
		os.close(fd)


def dev_boot_record_write(boot):
	try:
		inode = os.stat(DEV_LOCK).st_ino
	except OSError:
		return False
	temporary = f'{DEV_BOOT_RECORD}.{os.getpid()}'
	try:
		with open(temporary, 'w') as handle:
			handle.write(json.dumps({'inode': inode, 'boot': boot}) + '\n')
			handle.flush()
			os.fsync(handle.fileno())
		os.replace(temporary, DEV_BOOT_RECORD)
	except OSError:
		with contextlib.suppress(OSError):
			os.unlink(temporary)
		return False
	return True


def dev_boot_record_read():
	try:
		with open(DEV_BOOT_RECORD) as handle:
			record = json.load(handle)
		inode = os.stat(DEV_LOCK).st_ino
	except (OSError, ValueError):
		return None
	if not isinstance(record, dict) or record.get('inode') != inode:
		return None
	boot = record.get('boot')
	return boot if isinstance(boot, str) and boot else None


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
		return ctl_request(f'WAIT {timeout}', timeout, DEV_CTL_SOCK).prompted
	except (SystemExit, OSError):
		return False


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
		# VERIFIED, not merely alive. A lock-free record whose pgid happens to name a living group
		# was called `detached` on the strength of the number alone - and `detached` is the state
		# `dev-down` escalates SIGKILL against and `dev-up` reattaches to. A reused id would have
		# taken either of those to an unrelated process group.
		if dev_group_is_ours(identity):
			return 'detached', identity
		leftovers = [p for p in (DEV_CTL_SOCK, DEV_SERIAL_SOCK, DEV_CONSOLE_SOCK) if os.path.exists(p)]
		return ('stale' if leftovers else 'down'), identity
	# The lock is taken before the identity is written, so a bring-up in progress has the one
	# without the other. Comparing an absent repository against this one makes that read as
	# `foreign`, which tells the reader to go release the profile from a worktree that does not
	# exist. Nothing recorded means nothing has claimed it yet.
	if not identity:
		return 'starting', identity
	if identity.get('repo') != REPO:
		return 'foreign', identity
	return ('ready' if dev_ready() else 'starting'), identity


# Console occupancy, read over the control channel so it never disturbs the byte stream.
def dev_console_stat():
	if not os.path.exists(DEV_CTL_SOCK):
		return ''
	try:
		return ctl_request('STAT', 3, DEV_CTL_SOCK).data.decode(errors='replace').strip()
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
	# A bring-up that has taken the lock but recorded nothing yet is not an ownership dispute,
	# and saying so would send the reader looking for a worktree to release it from.
	if not identity:
		print('lab: a development instance is starting; wait for it, or `just lab dev-down` to abandon it', file=sys.stderr)
		sys.exit(1)
	print(f'lab: development profile is owned by {dev_describe(identity)}', file=sys.stderr)
	print(f'lab: release it with `just lab dev-down` in {identity.get("repo", "that worktree")}', file=sys.stderr)
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
	if not reply.prompted:
		die(f'reattached, but no shell prompt within {timeout} s (see {DEV_SERIAL_LOG})')
	print(f'lab: reattached to the running instance without rebooting (up {dev_uptime(identity)})')


# End a bring-up that cannot finish, taking its guest with it.
#
# The lock is this process's, so it goes when this process does - which means a guest left
# running here is invisible to `dev-status` and unreapable by `dev-down`, and the next `dev-up`
# then fails with a host forwarding rule already taken, naming a port rather than a cause.
# Nothing else reaps it, so this has to.
def dev_bring_up_failed(lock_fd, guest, message):
	for attempt in (signal.SIGTERM, signal.SIGKILL):
		try:
			os.killpg(os.getpgid(guest.pid), attempt)
		except OSError:
			break
		try:
			guest.wait(timeout=10)
			break
		except subprocess.TimeoutExpired:
			continue
	os.close(lock_fd)
	die(message)


# Whether QEMU is up yet in the guest's own process group.
def dev_guest_qemu(pgid):
	return subprocess.run(['pgrep', '-g', str(pgid), '-f', 'qemu-system'], capture_output=True).returncode == 0


def cmd_dev_up(args):
	timeout = arg_value(args, '--timeout', 240)
	build_timeout = arg_value(args, '--build-timeout', 1800)
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
		die('stale development instance state; run `just lab dev-down` to clear it')

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
	# THE BUILD IS A STEP OF ITS OWN, because the runner no longer performs one.
	#
	# The build and the boot are different waits and want different budgets: a tree needing a full
	# rebuild - anything that touched the build tooling invalidates every artifact's cache key - can
	# spend longer building than any sensible boot deadline, and charged to one number a slow build
	# is reported as a serial socket that never appeared, which sends the reader to QEMU for a
	# problem that is in Cargo. That reasoning is unchanged; what changed is who builds. `run.sh`
	# says of itself that it builds nothing, so the wait that used to watch for QEMU appearing
	# behind a building `just run` was watching a process that starts QEMU immediately - and a stale
	# or absent artifact was reported by the runner as a missing kernel rather than built.
	timing_event('qemu', 'build-start')
	# x86_64 by name: the persistent instance is that target and nothing else - `dev-status`
	# fingerprints `init-x86_64.pkg`, and `scenario-cold` exists precisely because the other two
	# have no persistent instance to be.
	try:
		build = subprocess.run(build_command('x86_64'), cwd=SRC, env=env, stdout=qemu_log, stderr=qemu_log, timeout=build_timeout)
	except subprocess.TimeoutExpired:
		os.close(lock_fd)
		die(f'the development build did not finish within {build_timeout} s (see {DEV_QEMU_LOG})')
	if build.returncode != 0:
		os.close(lock_fd)
		die(f'the development build failed (see {DEV_QEMU_LOG})')
	guest = subprocess.Popen(run_command(displays), cwd=SRC, env=env, stdout=qemu_log, stderr=qemu_log, start_new_session=True)
	record_lab_guest(guest)
	qemu_deadline = time.time() + 60
	while not dev_guest_qemu(guest.pid):
		if guest.poll() is not None:
			dev_bring_up_failed(lock_fd, guest, f'the runner exited before QEMU started (see {DEV_QEMU_LOG})')
		if time.time() > qemu_deadline:
			dev_bring_up_failed(lock_fd, guest, f'the runner did not reach QEMU within 60 s (see {DEV_QEMU_LOG})')
		time.sleep(0.5)
	timing_event('qemu', 'start')
	booted = time.time()
	while True:
		if time.time() - booted > timeout:
			dev_bring_up_failed(lock_fd, guest, f'serial socket did not appear within {timeout} s of QEMU starting (see {DEV_QEMU_LOG})')
		# A fresh socket per attempt: a stream socket whose connect failed cannot be
		# reliably reconnected, so retrying on the same one can fail for good.
		serial = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
		try:
			serial.connect(DEV_SERIAL_SOCK)
			break
		except OSError:
			serial.close()
			if guest.poll() is not None:
				# The runner is gone, but QEMU is a child in the same group and may not be, so
				# this goes through the same reaping the deadlines use.
				dev_bring_up_failed(lock_fd, guest, f'guest exited before the serial socket appeared (see {DEV_QEMU_LOG})')
			time.sleep(0.5)
	broker_pid = dev_fork_broker(serial)
	dev_restrict_sockets()
	dev_identity_write(lock_fd, {
		'profile': 'development',
		'repo': REPO,
		'host': socket.gethostname(),
		'broker': broker_pid,
		'pgid': guest.pid,
		# The pair, not the number. See `dev_group_is_ours`: a pgid on its own is reusable, and
		# `dev-down` signals what this names.
		'pgid_started': process_start_time(guest.pid),
		'started': started,
		# Taken after the guest booted, so it describes what this instance is actually
		# running. A reattach deliberately keeps it: the broker changed, the guest did not.
		'inputs': instance_inputs(),
	})
	os.close(lock_fd)
	time.sleep(0.2)
	reply = ctl_request(f'WAIT {timeout}', timeout, DEV_CTL_SOCK)
	if not reply.prompted:
		die(f'no shell prompt within {timeout} s (see {DEV_SERIAL_LOG})')
	# Record which boot this is, now that the guest is up and its agent can answer. Every
	# later session compares against it, so a tool can never publish into, or read a registry
	# from, a guest that restarted since this instance was recorded.
	timing_event('guest', 'ready')
	# READINESS INCLUDES THIS. The result was computed and thrown away, and `dev_record_boot`
	# returns False when the agent does not answer inside its retry interval or when the record
	# cannot be written - so `dev-up` could print "ready" having recorded no boot generation at all,
	# with every later session then skipping the restart comparison because there was nothing to
	# compare against. The shell comes up before the agent does, which is exactly when this happens.
	if not dev_record_boot():
		dev_bring_up_failed(lock_fd, guest, 'the guest reached a shell but its development agent never answered, so this instance has no boot generation - nothing could tell a later restart from this boot')
	# A fresh guest has touched nothing, so the record of which scenarios have run starts empty.
	# The scenario runner reads it to tell a first run's first-touch residency, which is memory
	# in use, from a repeat run's loss, which is not.
	try:
		os.remove(os.path.join(BUILD, 'dev-scenarios-seen'))
	except OSError:
		pass
	profile = 'development' if b'boot profile: development' in strip_ansi(reply.data) or dev_profile_logged() else 'not reported'
	print(f'lab: development instance ready in {time.time() - started:.1f} s (guest profile: {profile})')
	print(f'lab: serial log {os.path.relpath(DEV_SERIAL_LOG, SRC)}; `just lab dev-status`, `just lab dev-down`')


def dev_profile_logged():
	try:
		with open(serial_log_path(), 'rb') as handle:
			return b'boot profile: development' in handle.read()
	except OSError:
		return False


def cmd_dev_status(args):
	state, identity = dev_state()
	print(f'lab: development instance {state}')
	if state == 'down':
		print('     start it with `just lab dev-up`')
		sys.exit(1)
	if state == 'stale':
		print('     the broker is gone but its sockets remain')
		print('     clear it with `just lab dev-down`')
		sys.exit(1)
	if state == 'detached':
		print(f'     owner    {dev_describe(identity)}')
		print('     the guest is running but no broker is draining its console')
		print('     reattach without rebooting: `just lab dev-up`')
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
		print('     no shell prompt yet; rerun `just lab dev-status` or watch `just lab dev-log -f`')
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
		print('              these override the built image until `just lab dev-rollback` or a restart')
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
	print('     action   cold restart: `just lab dev-down && just lab dev-up`')
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
		die(f'{os.path.relpath(path, SRC)} is mode {mode:03o}, reachable beyond its owner; stop the instance with `just lab dev-down`')


# Ask the guest which boot it is and record it on the instance lock.
#
# Retried until the agent answers, because the moment a guest reaches a shell prompt is not the
# moment its development agent is serving: asking too early lands in the transport driver's
# discard window and the handshake is swallowed. Giving up quietly there is worse than it looks -
# the recorded value stays the previous boot's, so every later session refuses the guest for
# having restarted and sends the caller to `dev-up`, which rebuilds and reboots the thing that
# was already running.
def dev_record_boot(deadline=30):
	give_up = time.monotonic() + deadline
	bounds = None
	while bounds is None:
		# An attempt that finds the agent not yet serving is expected here and says nothing the
		# caller can act on, so its diagnostics are dropped. Only running out of time is news,
		# and that is the caller's to report.
		quiet, sys.stderr = sys.stderr, open(os.devnull, 'w')
		try:
			try:
				sock = proto_connect(2)
			except SystemExit:
				sock = None
			if sock is not None:
				buffer = bytearray()
				try:
					bounds = proto_hello(sock, buffer, 2)
				except (SystemExit, ProtoTimeout, OSError):
					bounds = None
				finally:
					sock.close()
		finally:
			sys.stderr.close()
			sys.stderr = quiet
		if bounds is None:
			if time.monotonic() >= give_up:
				return False
			time.sleep(0.5)
	return dev_boot_record_write(bounds['boot'])


# THE BOOT THIS GUEST IS CURRENTLY ON, straight from a handshake.
#
# The guest draws the value once per boot, so it is the only thing that distinguishes one boot from
# the next. Tools that wanted to prove "the guest did not restart under me" compared QEMU's process
# GROUP before and after instead - and `system_reset` keeps that process group while creating an
# entirely new boot, so the check could not see the event it was written for. None when the guest
# cannot be asked, which is a third answer and not a restart.
def guest_boot(timeout=5):
	try:
		sock = proto_connect(timeout)
	except SystemExit:
		return None
	buffer = bytearray()
	try:
		return proto_hello(sock, buffer, timeout)['boot']
	except (SystemExit, ProtoTimeout, OSError, struct.error):
		return None
	finally:
		sock.close()


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
		# `[-1]` on an artifact the guest listed with no generations is an `IndexError` out of a
		# status command, which reads as a broken tool rather than as a guest that answered oddly.
		return [(a['name'], a['generations'][-1]['generation'], published_ago(a['generations'][-1]['published_at'])) for a in artifacts if a['generations']]
	# `Malformed` and `IndexError` are in this list because a truncated or inconsistent reply used to
	# leave here as a traceback: only `struct.error` was caught, and the decoder's own failures are
	# an index past the end or a short slice. A status command reporting "no answer" is the right
	# outcome for a guest whose agent has gone wrong; a stack trace is not.
	except (SystemExit, ProtoTimeout, OSError, struct.error, Malformed, IndexError, UnicodeError):
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
		die('no development instance (run `just lab dev-up` first)')
	if not os.path.exists(DEV_CONSOLE_SOCK):
		die('this instance has no console socket; restart it with `just lab dev-down && just lab dev-up`')
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
		die('no development serial log yet (run `just lab dev-up` first)')
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


# WHEN THE LEADER OF `pgid` IS THE PROCESS THIS INSTANCE STARTED, and not merely something with
# that number.
#
# A process group id is reused. `dev_group_alive` answers "some group with this number exists", and
# that was the whole basis on which `dev-down` sent SIGKILL to it and on which `dev_state` called a
# lock-free instance `detached`. An id belonging to a guest that exited long ago can be held by
# anything at all by the time somebody runs `dev-down`.
#
# The leader's start time is what makes the record immutable: it is fixed when the process is
# created and cannot be reused with the number. `/proc/<pid>/stat` field 22 is that value in clock
# ticks since boot, so a recorded pair (pgid, starttime) identifies one process and no other on this
# machine's uptime.
def process_start_time(pid):
	try:
		with open(f'/proc/{pid}/stat', 'rb') as handle:
			fields = handle.read()
	except OSError:
		return None
	# The command name is field 2 and may contain spaces and brackets, so everything is counted from
	# after its closing parenthesis rather than by splitting the whole line.
	close = fields.rfind(b')')
	if close < 0:
		return None
	rest = fields[close + 2:].split()
	if len(rest) < 20:
		return None
	try:
		return int(rest[19])
	except ValueError:
		return None


def dev_group_is_ours(identity):
	pgid = identity.get('pgid')
	if not pgid or not dev_group_alive(pgid):
		return False
	recorded = identity.get('pgid_started')
	if recorded is None:
		# An instance recorded before this field existed. Its group cannot be verified, so it is not
		# signalled - reporting an unverifiable record is recoverable, and killing the wrong process
		# group is not.
		return False
	return process_start_time(pgid) == recorded


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
	verified = dev_group_is_ours(identity)
	if pgid and dev_group_alive(pgid) and not verified:
		# The recorded group is alive but is NOT the process this instance started - the number has
		# been reused, or the record predates the identity that would prove it. Signalling it is how
		# a cleanup kills something it has nothing to do with, so it does not.
		die(f'the recorded process group {pgid} is alive but is not this instance\'s guest (the id has been reused, or the record is too old to verify); inspect it with `ps -g {pgid}` and stop it yourself')
	if os.path.exists(mon_sock()):
		try:
			monitor_command('quit')
		except (SystemExit, OSError):
			pass
	deadline = time.time() + timeout
	while time.time() < deadline and not dev_stopped(pgid):
		time.sleep(0.2)
	if not dev_stopped(pgid) and verified:
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
	# AND NOTHING IS UNLINKED UNLESS IT STOPPED.
	#
	# This removed the lock and every socket whatever the verdict. If the broker or QEMU survived,
	# unlinking `DEV_LOCK` took away the PATHNAME while the broker still held its flock on the old
	# inode - so the next `dev-up` created a new file at the same path, locked it successfully, and
	# two instances both believed they owned the singleton profile. That is the exclusivity
	# guarantee that protects the system volume and the shared ports, destroyed by the command whose
	# job is to release it. Removing the live socket names on top of that is what makes the repair
	# hard: there is then nothing left to reattach to.
	if not stopped:
		print(f'lab: the guest did not stop; its lock and sockets are LEFT IN PLACE so the instance stays reachable', file=sys.stderr)
		die(f'development instance did not stop; inspect it with `ps -g {pgid}`')
	for path in (DEV_SERIAL_SOCK, DEV_CTL_SOCK, DEV_CONSOLE_SOCK, DEV_CHANNEL_SOCK, DEV_BOOT_RECORD, DEV_LOCK):
		if os.path.exists(path):
			os.unlink(path)
	print('lab: development instance down')


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
OP_LAUNCH = 0x30
OP_LAUNCH_ACK = 0x31
OP_LAUNCH_OUTPUT = 0x32
OP_LAUNCH_BYTES = 0x33
OP_LAUNCH_STOP = 0x34
OP_LAUNCH_STOP_ACK = 0x35
OP_TERM_INPUT = 0x20
OP_TERM_ACK = 0x21
OP_RESET = 0x22
OP_RESET_ACK = 0x23
OP_RESTART = 0x24
OP_RESTART_ACK = 0x25
OP_MEM_STATS = 0x26
OP_MEM_STATS_REPLY = 0x27
OP_FIXTURE_PUT = 0x28
OP_FIXTURE_ACK = 0x29
OP_FIXTURE_CLEAR = 0x2a
OP_FIXTURE_CLEAR_ACK = 0x2b
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
	23: 'no launcher is wired to the development agent',
	24: 'the launcher refused the component',
	25: 'no launch to read output from',
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
# The control channel a session talks over. Normally the persistent instance's; a cold run on
# another target points this at that target's own socket for the length of the run, because the
# protocol and everything above it are identical either way - only the endpoint differs.
CHANNEL_OVERRIDE = None


def channel_path():
	return CHANNEL_OVERRIDE or DEV_CHANNEL_SOCK


def proto_connect(timeout):
	if not os.path.exists(channel_path()):
		die('this instance has no development channel; restart it with `just lab dev-down && just lab dev-up`')
	if CHANNEL_OVERRIDE is None:
		dev_check_socket(DEV_CHANNEL_SOCK)
	sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
	sock.settimeout(timeout)
	try:
		sock.connect(channel_path())
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
	if CHANNEL_OVERRIDE is not None:
		# A cold run owns the guest it started; there is no instance record to consult and no
		# other owner to conflict with.
		sock = proto_connect(timeout)
		buffer = bytearray()
		bounds = proto_hello(sock, buffer, timeout)
		return sock, buffer, bounds
	state, identity = dev_state()
	if state == 'foreign':
		dev_owner_conflict(identity)
	if state in ('down', 'stale'):
		die('no development instance (run `just lab dev-up` first)')
	if state == 'detached':
		die('the guest is running but no broker owns it; reattach with `just lab dev-up`')
	sock = proto_connect(timeout)
	buffer = bytearray()
	bounds = proto_hello(sock, buffer, timeout)
	# The guest draws a value once per boot and reports it in every handshake. An instance is
	# meant to outlive the tools that drive it, so a tool can be talking to a guest that
	# restarted under it; comparing against what `dev-up` recorded turns that into a refusal
	# instead of a publication into the wrong boot.
	#
	# AN ABSENT RECORD IS A REFUSAL, not a comparison to skip. The check was `if recorded and ...`,
	# so an instance that never recorded one - or whose record was lost to the in-place rewrite this
	# replaced - handshook happily with whichever boot answered, and the safety invariant was off
	# with nothing saying so.
	recorded = dev_boot_record_read()
	if recorded is None:
		die('this instance has no recorded boot generation, so nothing can tell this guest from one that restarted under it; rerun `just lab dev-up`')
	if recorded != bounds['boot']:
		die(f'the guest has restarted since this instance was recorded (boot {recorded[:16]} -> {bounds["boot"][:16]}); rerun `just lab dev-up`')
	warn_stale_inputs(identity)
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
	timing_event('publish', 'start')
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
		# ONE ABSOLUTE DEADLINE FOR THE WHOLE PUBLICATION, not `--timeout` again per exchange.
		#
		# It was applied independently to the handshake, the begin, EVERY CHUNK and the commit. A
		# maximum-size artifact is thousands of chunks, so `--timeout 15` bounded nothing anyone
		# would recognise as fifteen seconds: the advertised bound was multiplied by the number of
		# frames the file happened to need.
		deadline = started + timeout

		def left(what):
			remaining = deadline - time.monotonic()
			if remaining <= 0:
				die(f'publication of {name} ran past its {timeout} s deadline before {what}')
			return remaining

		begin = struct.pack('<I', len(blob)) + digest + bytes([len(encoded)]) + encoded
		_, generation, body = proto_request(sock, buffer, 2, OP_PUB_BEGIN, begin, timeout=left('the begin'), what='publication begin')
		print(f'lab: publishing {name} as generation {generation} ({len(blob)} B, sha256 {digest.hex()[:16]}...)')
		request = 3
		sent = 0
		try:
			while sent < len(blob):
				chunk = blob[sent:sent + bounds['max_payload']]
				_, _, body = proto_request(sock, buffer, request, OP_PUB_CHUNK, chunk, generation, left(f'the chunk at offset {sent}'), f'chunk at offset {sent}')
				sent += len(chunk)
				acked = struct.unpack('<I', body[:4])[0] if len(body) >= 4 else -1
				if acked != sent:
					die(f'guest acknowledged {acked} B after {sent} B were sent')
				request += 1
			_, _, body = proto_request(sock, buffer, request, OP_PUB_COMMIT, b'', generation, left('the commit'), 'commit')
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
		timing_event('publish', 'end')
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
# Put a wedged guest back to a just-booted state without rebuilding anything.
#
# This is the recovery for corrupted guest state, and it is a real reboot rather than a restored
# snapshot. A snapshot would have been the faster answer and is not available: QEMU's `savevm`
# needs a qcow2 device to write the state into, and every drive here is raw, so offering it
# would mean changing the disk format that every run and every test uses - a topology change,
# paid on every boot, to serve a recovery path. The reboot is fast enough to be the equivalent:
# the guest reaches a prompt in about the time a fresh one does, and nothing is compiled.
#
# It also does not hide what a snapshot would have hidden. The system volume is persistent and
# survives this, so a bug that corrupts stored state is still there afterwards - which is
# exactly the property a restored snapshot would have quietly papered over.
#
# The instance itself survives: same QEMU, same broker, same sockets, same lock, so every tool
# keeps talking to the thing it was talking to. What does change is the guest's per-boot value,
# and that has to be recorded again or every later session refuses the guest for having
# restarted - which it did.
def cmd_dev_reboot(args):
	timeout = arg_value(args, '--timeout', 120)
	state, identity = dev_state()
	# `starting` is accepted alongside `ready`, and refusing it was the first thing using this
	# command found wrong with it: a guest that never reached a prompt is exactly the corrupted
	# state this exists to clear, so demanding a healthy instance refused the one case that
	# needed it. What is refused is an instance this worktree does not own or does not have.
	if state not in ('ready', 'starting'):
		die(f'the development instance is {state}; a reboot needs one this worktree owns and is running')
	started = time.time()
	qmp_command('system_reset')
	reply = ctl_request(f'WAIT {timeout}', timeout, DEV_CTL_SOCK)
	if not reply.prompted:
		die(f'no shell prompt within {timeout} s of the reset (see {DEV_SERIAL_LOG})')
	# The registry went with the reboot, because it was the agent's memory. Recording the new
	# boot value is not optional bookkeeping: without it every session refuses this guest for
	# having restarted, and the recovery it names is the rebuild this command exists to avoid.
	if not dev_record_boot():
		die('the guest rebooted but its development agent never answered, so the instance is now unusable; `just lab dev-up` will rebuild it')
	print(f'lab: guest rebooted to a prompt in {time.time() - started:.1f} s; the registry is empty and the volume is unchanged')


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


# Replace the guest's development agent with a fresh one. Everything the old one held goes
# with it - the registry included - and the guest keeps running: the port, its driver and the
# instance are untouched, and the supervisor that started the agent starts the next one.
#
# The wait afterwards is the operation, not politeness about it. The acknowledgement means the
# old agent accepted the request, not that a new one is serving; a caller that returned there
# would hand the next command to a port with nobody behind it yet. So this asks for a
# handshake until one answers, which is exactly the condition the caller cares about.
def cmd_dev_restart(args):
	timeout = arg_value(args, '--timeout', 30)
	seen_at = serial_size()
	sock, buffer, _ = proto_session(timeout)
	try:
		opcode, _, body = proto_request(sock, buffer, 2, OP_RESTART, timeout=timeout, what='restart')
		if opcode != OP_RESTART_ACK or len(body) < 2:
			die(f'restart answered with opcode {opcode:#04x} and {len(body)} B')
		print(f'lab: the agent is ending, dropping {struct.unpack("<H", body[:2])[0]} generation(s)')
	finally:
		sock.close()
	started = time.monotonic()
	deadline = started + timeout
	# Wait for the replacement to say so on the console before writing a byte to the port.
	# Asking instead would cost several seconds rather than save any: the driver discards what
	# arrives while it has no agent, so a handshake written into that window is split across it,
	# the tail reaches the fresh agent as the beginning of a frame, and the stream only clears
	# when the fragment deadline expires - which a caller retrying faster than that deadline
	# keeps restarting. The supervisor prints the agent's own report when it starts, so the
	# readiness signal already exists and costs nothing to watch.
	at = seen_at
	while time.monotonic() < deadline:
		if 'agent.dev: online' in serial_since(at):
			break
		time.sleep(0.1)
	else:
		die(f'no replacement agent reported in within {timeout} s of the restart')
	while time.monotonic() < deadline:
		try:
			ready, _, _ = proto_session(min(3, max(1, int(deadline - time.monotonic()))), announce=False)
			ready.close()
			print(f'lab: a fresh agent is serving after {(time.monotonic() - started) * 1000:.0f} ms, with an empty registry')
			return
		except SystemExit:
			# Reported in but not yet reading the port, which is a moment, not a state. Backing
			# off further than the guest's own fragment deadline keeps a failed attempt from
			# being what makes the next one fail.
			time.sleep(2.5)
	die(f'no agent answered within {timeout} s of the restart')


# Read the registry: every artifact, and the generations retained for it. The newest
# generation of each is what currently shadows the system volume, which is the thing worth
# seeing at a glance - a forgotten override is a fix that appears not to have worked.
# A malformed reply, named as one.
#
# Raised rather than `die`d so a caller that is diagnosing a protocol regression - which is what
# these tools are for - can say WHICH field at WHICH offset did not fit.
class Malformed(Exception):
	pass


# A cursor that checks every read against the end of the buffer.
#
# The registry decoder indexed `body[at]`, sliced names, generation records and detail strings, and
# advanced past them without a single bounds check, and Python's slicing hides the failure: a slice
# past the end is silently SHORT, so a truncated reply produced a plausible-looking registry with
# quietly missing bytes, while an index past the end raised `IndexError` out of a function whose
# caller catches `struct.error`. Neither is a named malformed-input outcome, and both come from a
# guest that has gone wrong, which is exactly when a diagnostic tool has to be exact.
class Cursor:
	def __init__(self, data, what):
		self.data = data
		self.what = what
		self.at = 0

	def take(self, count):
		if count < 0 or self.at + count > len(self.data):
			raise Malformed(f'{self.what}: {count} B at offset {self.at}, past the end of a {len(self.data)} B payload')
		chunk = self.data[self.at:self.at + count]
		self.at += count
		return chunk

	def unpack(self, layout):
		return layout.unpack(self.take(layout.size))

	def byte(self):
		return self.take(1)[0]

	# Some formats end exactly where they say they do, and trailing bytes mean the reply was built
	# by something that does not agree with this reader.
	def require_exhausted(self):
		if self.at != len(self.data):
			raise Malformed(f'{self.what}: {len(self.data) - self.at} B of trailing data after offset {self.at}')


REGISTRY_HEADER = struct.Struct('<HI')
# generation u32 | length u32 | digest [32] | published_at u64 | verdict u8 | detail length u8
REGISTRY_GENERATION = struct.Struct('<II32sQBB')


def decode_registry(body):
	cursor = Cursor(body, 'registry reply')
	count, registry_bytes = cursor.unpack(REGISTRY_HEADER)
	artifacts = []
	for _ in range(count):
		name = cursor.take(cursor.byte()).decode(errors='replace')
		held = cursor.byte()
		generations = []
		for _ in range(held):
			generation, length, digest, published_at, verdict, detail_len = cursor.unpack(REGISTRY_GENERATION)
			detail = cursor.take(detail_len).decode(errors='replace')
			generations.append({'generation': generation, 'length': length, 'digest': digest, 'published_at': published_at, 'verdict': verdict, 'detail': detail})
		artifacts.append({'name': name, 'generations': generations})
	# The reply describes exactly `count` artifacts and ends. Anything after them was written by
	# something that does not agree with this reader about the format.
	cursor.require_exhausted()
	return artifacts, registry_bytes


def read_registry(sock, buffer, timeout):
	opcode, _, body = proto_request(sock, buffer, 2, OP_GEN_LIST, timeout=timeout, what='registry query')
	if opcode != OP_GEN_LIST_REPLY or len(body) < REGISTRY_HEADER.size:
		die(f'registry query answered with opcode {opcode:#04x} and {len(body)} B')
	try:
		return decode_registry(body)
	except Malformed as error:
		die(str(error))


def published_ago(published_at):
	if not published_at:
		return 'unknown time'
	seconds = max(int(time.time()) - published_at, 0)
	if seconds < 90:
		return f'{seconds} s ago'
	if seconds < 5400:
		return f'{seconds // 60} min ago'
	return f'{seconds // 3600} h ago'


# Launch a canonical program through PermissionManager and read what it prints. The name,
# the arguments and the working directory are separate typed fields the whole way down; at no
# point is a string handed to an interpreter.
def launch_payload(name, args, cwd):
	name, args, cwd = name.encode(), args.encode(), cwd.encode()
	return bytes([len(name)]) + name + struct.pack('<H', len(args)) + args + struct.pack('<H', len(cwd)) + cwd


# Where an artifact is staged on the host, from the manifest that decides it. Programs lose
# the `.lsexe` the manifest names them by when they are staged; libraries keep their suffix.
# Read from the manifest rather than guessed, because the manifest is the one place that
# decides where anything goes.
def staged_artifact(name, target='x86_64-unknown-none'):
	try:
		manifest = json.loads(subprocess.run([os.path.join(SRC, 'tools', 'system-manifest.sh'), 'export-json'], cwd=SRC, capture_output=True, text=True, check=True).stdout)
	except (OSError, ValueError, subprocess.CalledProcessError) as error:
		die(f'cannot read the system manifest: {error}')
	entry = manifest.get('programs', {}).get(name) or manifest.get('libraries', {}).get(name)
	if not entry:
		die(f'{name} is not a manifest-declared artifact; only a declared name can iterate hot')
	destination = entry['destination']
	if destination.endswith('.lsexe'):
		destination = destination[: -len('.lsexe')]
	path = os.path.join(BUILD_ROOT, 'image', target, destination)
	if not os.path.isfile(path):
		die(f'{name} is declared but not staged at {path}; build the tree first')
	return path


# The development loop itself: build the artifact, publish it into the running guest, and run
# the scenarios that say whether it works. One command because it is one thought, and it stops
# at the phase that failed rather than reporting three results the reader has to combine.
#
# Each phase is exactly the command a person would run by hand, invoked as such. That is what
# keeps the loop honest: there is no faster path here that the individual commands do not have,
# so a phase that passes here passes there.
#
# What makes a warm iteration cheap is the build phase's own content-addressed cache, which
# reports what it reused; publish and run always happen, because a publication of unchanged
# bytes costs milliseconds and a test that is skipped has not passed.
def cmd_dev_loop(args):
	timeout, rest = take_arg(args, '--timeout', 300)
	target, rest = take_string_arg(rest, '--target', 'x86_64')
	if len(rest) < 2:
		die('usage: dev-loop [--target T] <artifact> <scenario.toml>...')
	artifact, scenarios = rest[0], rest[1:]
	# Answered once, here, before anything is built: an instance older than the tree is the one
	# thing a loop cannot fix by running, and each phase asking again would only repeat it.
	_, identity = dev_state()
	warn_stale_inputs(identity)
	child_env = dict(os.environ, LIBER_DEV_INPUTS_CHECKED='1')
	phases = []
	started = time.monotonic()
	# ONE ABSOLUTE DEADLINE ACROSS THE PHASES. `--timeout` was parsed and then placed on nothing: the
	# build and the scenario run were unbounded children, so a loop documented as bounded could hang
	# in either forever. The publish child gets what is left rather than the whole number again.
	deadline = started + timeout

	def remaining(phase):
		left = deadline - time.monotonic()
		if left <= 0:
			report_loop(phases, started, '')
			die(f'dev-loop: out of time before the {phase} phase ({timeout} s total)')
		return left

	def phase_child(name, command):
		at = time.monotonic()
		try:
			done = run_child(command, remaining(name), cwd=SRC, env=child_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
		except ChildTimeout as expired:
			phases.append((name, time.monotonic() - at, False))
			report_loop(phases, started, str(expired))
			die(f'dev-loop: the {name} phase did not finish within its share of {timeout} s')
		phases.append((name, time.monotonic() - at, done.returncode == 0))
		return done

	build = phase_child('build', [os.path.join(SRC, 'tools', 'dev-build.sh'), artifact, target])
	summary = next((line for line in reversed(build.stdout.splitlines()) if 'summary target=' in line), '')
	if build.returncode != 0:
		report_loop(phases, started, build.stdout + build.stderr)
		die(f'dev-loop: the build phase failed for {artifact}')
	if summary:
		print(f'lab: {summary.split("summary ", 1)[-1]}')

	path = staged_artifact(artifact, {'x86_64': 'x86_64-unknown-none', 'aarch64': 'aarch64-unknown-none', 'riscv64': 'riscv64gc-unknown-none-elf'}.get(target, target))
	publish = phase_child('publish', [os.path.join(HERE, 'lab.py'), 'dev-publish', artifact, path, '--timeout', str(int(remaining('publish')))])
	if publish.returncode != 0:
		report_loop(phases, started, publish.stdout + publish.stderr)
		die(f'dev-loop: publishing {artifact} failed')
	for line in publish.stdout.splitlines():
		if 'committed' in line or 'compatibility' in line:
			print(line)

	run = phase_child('run', [os.path.join(HERE, 'lab.py'), 'dev-test'] + scenarios)
	print(run.stdout.strip())
	report_loop(phases, started, run.stderr if run.returncode != 0 else '')
	if run.returncode != 0:
		die('dev-loop: the scenarios failed')


def report_loop(phases, started, detail):
	if detail.strip():
		print(detail.strip())
	shape = '  '.join(f'{name} {"ok" if ok else "FAILED"} {elapsed:.1f}s' for name, elapsed, ok in phases)
	print(f'lab: {shape}  total {time.monotonic() - started:.1f}s')


# How much of each kind of host-side leftover a clean keeps. Written as numbers rather than
# left to judgement, so what a clean removes is knowable before running it and arguable
# afterwards. Nothing here is a source or a build input: every one of these is either output
# that can be produced again or scratch from a run that has ended.
KEEP_TEST_RUNS = 20
KEEP_BASELINE_SAMPLES = 20


# Prune what the host accumulates and nothing else.
#
# The rule the list below follows: a thing may be removed when it is reproducible from sources
# or belongs to a run that is over, and may not be removed when anything still refers to it.
# The running instance is the case that matters - its log and sockets are live state, so a
# clean leaves them alone while it is up and says so.
def cmd_dev_clean(args):
	dry = '--dry-run' in args
	removed = []

	# Test logs, newest kept. They come in pairs per run, so runs are counted rather than files.
	logs = sorted(glob.glob(os.path.join(BUILD_ROOT, 'logs', 'test', '*-run.log')), key=os.path.getmtime, reverse=True)
	for stale in logs[KEEP_TEST_RUNS:]:
		removed.append(stale)
		removed.append(stale.replace('-run.log', '-guest.log'))

	# Baseline samples, newest kept.
	samples = sorted(glob.glob(os.path.join(BUILD_ROOT, 'measure', 'dev-baseline', '*')), key=os.path.getmtime, reverse=True)
	removed.extend(samples[KEEP_BASELINE_SAMPLES:])

	# Scratch directories a build left behind, named after the process that made them. One
	# whose process is gone can never be claimed again; one whose process is alive is in use.
	for scratch in glob.glob(os.path.join(BUILD_ROOT, 'image-source-metadata.*')):
		owner = scratch.rsplit('.', 1)[-1]
		if not owner.isdigit():
			continue
		try:
			os.kill(int(owner), 0)
		except ProcessLookupError:
			removed.append(scratch)
		except OSError:
			pass

	# Superseded object generations. The build keeps one per artifact and drops the rest as it
	# goes, so these are the backlog from before it did that, plus anything left by a tool that
	# has not been rebuilt since. They are not inputs to the next build: each artifact's
	# `.object` reference names the one generation that is, and the others are reproducible from
	# sources like everything else this prunes.
	for reference in glob.glob(os.path.join(BUILD_ROOT, 'cache', '*', 'executable-*.object')):
		name = os.path.basename(reference)[len('executable-') : -len('.object')]
		try:
			with open(reference) as handle:
				current = next((line.split('=', 1)[1].strip() for line in handle if line.startswith('key=')), None)
		except OSError:
			continue
		if not current:
			continue
		for path in glob.glob(os.path.join(os.path.dirname(reference), f'object-{name}-*')):
			key = os.path.basename(path)[len(f'object-{name}-') :].split('.', 1)[0]
			if len(key) == 64 and key != current:
				removed.append(path)

	# Scratch a build could not clean up after itself. Its exit path removes what it made, so
	# anything surviving here belongs to a build that was killed outright rather than one that
	# failed. Age is the only usable test - the names carry no owner - and a build in flight is
	# minutes, so a day is not a close call.
	for scratch in glob.glob(os.path.join(BUILD_ROOT, 'tmp', '*')):
		try:
			if time.time() - os.path.getmtime(scratch) > 24 * 3600:
				removed.append(scratch)
		except OSError:
			pass

	# The instance's own files are live state while it is up, and stale sockets when it is not.
	state, _ = dev_state()
	if state in ('down', 'stale'):
		removed.extend(path for path in (DEV_SERIAL_SOCK, DEV_CTL_SOCK, DEV_CONSOLE_SOCK, DEV_CHANNEL_SOCK) if os.path.exists(path))
	else:
		print(f'lab: the development instance is {state}; leaving its log and sockets alone')

	total = 0
	for path in removed:
		if not os.path.exists(path):
			continue
		total += directory_bytes(path) if os.path.isdir(path) else os.path.getsize(path)
		if not dry:
			shutil.rmtree(path, ignore_errors=True) if os.path.isdir(path) else os.unlink(path)
	verb = 'would remove' if dry else 'removed'
	print(f'lab: {verb} {len(removed)} item(s), {total // 1024} kB (keeping the {KEEP_TEST_RUNS} newest test runs and {KEEP_BASELINE_SAMPLES} newest baseline samples)')
	print('lab: current artifacts and staged images are left alone; they are inputs to the next build, and `just clean` is what discards them')


def directory_bytes(path):
	total = 0
	for root, _, files in os.walk(path):
		for name in files:
			try:
				total += os.path.getsize(os.path.join(root, name))
			except OSError:
				pass
	return total


def cmd_dev_launch(args):
	timeout, rest = take_arg(args, '--timeout', 30)
	if not rest:
		die('usage: dev-launch [--timeout N] <program> [args...]')
	# Everything after the program name is the program's, dashes included. Filtering options
	# out here would silently eat the flags of the thing being launched, which is how
	# `dev-launch imgconv --help` came to ask imgconv to convert nothing.
	name, program_args = rest[0], ' '.join(rest[1:])
	sock, buffer, _ = proto_session(timeout)
	try:
		opcode, _, body = proto_request(sock, buffer, 2, OP_LAUNCH, launch_payload(name, program_args, 'vol://system'), timeout=timeout, what=f'launching {name}')
		if opcode != OP_LAUNCH_ACK or len(body) < 8:
			die(f'launch answered with opcode {opcode:#04x} and {len(body)} B')
		print(f'lab: launched {name} as koid {struct.unpack("<Q", body[:8])[0]}')
		# Each poll gets what is LEFT of the deadline this loop computed. It gave every poll the
		# original full timeout, so the last one could overrun that deadline by another complete
		# interval - the bound was the loop's, and the operation it bounded had never been told.
		request, deadline = 3, time.monotonic() + timeout
		while time.monotonic() < deadline:
			opcode, _, body = proto_request(sock, buffer, request, OP_LAUNCH_OUTPUT, timeout=max(deadline - time.monotonic(), 0.1), what='reading output')
			request += 1
			if len(body) >= 2:
				if body[1]:
					print('lab: (output was truncated; the program printed faster than it was read)', file=sys.stderr)
				sys.stdout.write(body[2:].decode(errors='replace'))
				sys.stdout.flush()
				if body[0]:
					print(f'lab: {name} finished')
					return
			time.sleep(0.1)
		die(f'{name} had not finished within {timeout} s')
	finally:
		sock.close()


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
		return serial_size()

	def serial_since(self, at):
		return serial_since(at)

	def serial_raw_since(self, at):
		return serial_raw_since(at)

	def serial_read(self, at):
		return serial_read(at)

	# Key and pointer events go through the emulated devices rather than the control protocol,
	# so a scenario that sends them exercises the input stack the way a person does: the device,
	# its driver, InputService, the session and the foreground program. Typed input over the
	# protocol reaches the console directly and proves none of that.
	def send_keys(self, keys, timeout=None):
		try:
			return send_keys(keys, None if timeout is None else time.monotonic() + timeout)
		except SystemExit:
			return False

	def send_pointer(self, x, y, button, action, timeout=None):
		try:
			return send_pointer(x, y, button, action, timeout)
		except SystemExit:
			return False

	def wait_prompt(self, timeout):
		# A cold run has no broker to ask, so the guest's own console log is what answers. The
		# broker path stays for the persistent instance, where it is cheaper than polling a file
		# and is what every existing scenario already runs through.
		if SERIAL_OVERRIDE is not None:
			deadline = time.time() + timeout
			while time.time() < deadline:
				try:
					with open(SERIAL_OVERRIDE, 'rb') as handle:
						tail = handle.read()[-256:]
				except OSError:
					tail = b''
				if has_prompt(tail):
					return True
				time.sleep(0.5)
			return False
		try:
			return ctl_request(f'WAIT {timeout}', timeout, DEV_CTL_SOCK).prompted
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

	# Launch through PermissionManager and return the koid, or None when it was refused.
	def launch(self, name, args, cwd, timeout):
		sock, buffer, _ = proto_session(timeout, announce=False)
		try:
			opcode, _, body = proto_request(sock, buffer, 2, OP_LAUNCH, launch_payload(name, args, cwd), timeout=timeout, what=f'launching {name}', tolerate=(23, 24))
			if opcode != OP_LAUNCH_ACK or len(body) < 8:
				return None
			return struct.unpack('<Q', body[:8])[0]
		finally:
			sock.close()

	# Everything the launched program has printed since the last read, and whether it ended.
	def launch_output(self, timeout):
		sock, buffer, _ = proto_session(timeout, announce=False)
		try:
			opcode, _, body = proto_request(sock, buffer, 2, OP_LAUNCH_OUTPUT, timeout=timeout, what='reading output', tolerate=(25,))
			if opcode != OP_LAUNCH_BYTES or len(body) < 2:
				return None, False
			return body[2:].decode(errors='replace'), bool(body[0])
		finally:
			sock.close()

	# End the launched program. Answers whether anything was signalled, which is not the same
	# as whether this succeeded: a program that had already finished is nothing to stop.
	def stop_launch(self, timeout):
		sock, buffer, _ = proto_session(timeout, announce=False)
		try:
			opcode, _, body = proto_request(sock, buffer, 2, OP_LAUNCH_STOP, timeout=timeout, what='stopping the launched program', tolerate=(25,))
			return opcode == OP_LAUNCH_STOP_ACK and bool(body[:1] and body[0])
		finally:
			sock.close()

	# What the guest still holds on a scenario's behalf, as a list of plain descriptions, empty
	# when it holds nothing. None when it could not be asked at all, which is a different and
	# worse answer than "nothing".
	#
	# Write one fixture file into the guest's scenario fixture area. `name` is a bare name the
	# guest joins to its own reserved prefix, so nothing here can name a path. Returns True when
	# the guest wrote it.
	def fixture_put(self, name, path, timeout):
		with open(path, 'rb') as handle:
			body = handle.read()
		encoded = name.encode()
		payload = bytes([len(encoded)]) + encoded + body
		try:
			sock, buffer, _ = proto_session(timeout, announce=False)
		except SystemExit:
			return False
		try:
			opcode, _, _ = proto_request(sock, buffer, 2, OP_FIXTURE_PUT, payload=payload, timeout=timeout, what=f'writing the fixture {name}')
			return opcode == OP_FIXTURE_ACK
		except SystemExit:
			return False
		finally:
			sock.close()

	# Remove every fixture the guest wrote for this run. Returns the number it could not remove,
	# or None when the instance could not answer - both of which a teardown reports rather than
	# swallows, because a fixture left behind is inherited by whatever runs next.
	def fixture_clear(self, timeout):
		try:
			sock, buffer, _ = proto_session(timeout, announce=False)
		except SystemExit:
			return None
		try:
			opcode, _, body = proto_request(sock, buffer, 2, OP_FIXTURE_CLEAR, timeout=timeout, what='clearing the fixtures', tolerate=(2,))
			if opcode != OP_FIXTURE_CLEAR_ACK or len(body) < 4:
				return None
			return struct.unpack('<H', body[2:4])[0]
		except SystemExit:
			return None
		finally:
			sock.close()

	# The guest's system memory account, as (free_frames, total_frames, heap_free, heap_total),
	# or None when the instance cannot answer. None also covers an instance that predates the
	# opcode: it replies ST_BAD_OPCODE (2), which is tolerated here rather than treated as a
	# failure, because a running instance older than this check is a reason to skip the check and
	# not a reason to fail somebody's scenario.
	def memory_stats(self, timeout):
		try:
			sock, buffer, _ = proto_session(timeout, announce=False)
		except SystemExit:
			return None
		try:
			opcode, _, body = proto_request(sock, buffer, 2, OP_MEM_STATS, timeout=timeout, what='reading the memory account', tolerate=(2,))
			if opcode != OP_MEM_STATS_REPLY or len(body) < 32:
				return None
			return struct.unpack('<QQQQ', body[:32])
		except SystemExit:
			return None
		finally:
			sock.close()

	# Asked of the guest rather than inferred from what the runner did: the point of the check
	# is to catch the case where the runner believes it cleaned up and the guest disagrees.
	# Everything a teardown needs to know, over one session: clear the fixtures, then ask what is
	# still held and what memory is free. Four questions down one connection rather than one each,
	# because a session costs a handshake and a guest round trip - measured at about 0.2 s, which
	# is not much until a teardown pays it four times and the scenario phase grows by 0.6 s.
	#
	# Returns (held, fixtures_stuck, free_frames); any element is None when the guest could not
	# answer it, which the caller reports rather than treats as success.
	def teardown_state(self, timeout):
		try:
			sock, buffer, _ = proto_session(timeout, announce=False)
		except SystemExit:
			return None, None, None
		held = []
		stuck = None
		free = None
		try:
			opcode, _, body = proto_request(sock, buffer, 2, OP_FIXTURE_CLEAR, timeout=timeout, what='clearing the fixtures', tolerate=(2,))
			if opcode == OP_FIXTURE_CLEAR_ACK and len(body) >= 4:
				stuck = struct.unpack('<H', body[2:4])[0]
			opcode, _, body = proto_request(sock, buffer, 3, OP_GEN_LIST, timeout=timeout, what='reading the registry', tolerate=(21,))
			if opcode == OP_GEN_LIST_REPLY and len(body) >= 6:
				artifacts, spent = struct.unpack('<HI', body[:6])
				if artifacts or spent:
					held.append(f'{artifacts} artifact(s) and {spent} B still in the registry')
			opcode, _, body = proto_request(sock, buffer, 4, OP_LAUNCH_OUTPUT, timeout=timeout, what='reading the launch state', tolerate=(25,))
			# A launch that has ended is nothing to hold; one that has not is this run's, still
			# running, on an instance the next run is about to use.
			if opcode == OP_LAUNCH_BYTES and len(body) >= 1 and not body[0]:
				held.append('a launched program is still running')
			opcode, _, body = proto_request(sock, buffer, 5, OP_MEM_STATS, timeout=timeout, what='reading the memory account', tolerate=(2,))
			if opcode == OP_MEM_STATS_REPLY and len(body) >= 32:
				free = struct.unpack('<QQQQ', body[:32])[0]
		except SystemExit:
			return None, stuck, free
		finally:
			sock.close()
		return held, stuck, free

	# These three run a child, and the child was given the step's timeout as an ARGUMENT with no host
	# bound on the process itself. A child that hangs before reaching its own deadline - in socket
	# setup, in an import, anywhere - ran forever inside a step that had declared a limit. The host
	# bound is the same number plus a small allowance, so the child's own timeout still reports the
	# failure when it can and the host stops it when it cannot.
	def scenario_child(self, arguments, timeout):
		try:
			return run_child([os.path.join(HERE, 'lab.py'), *arguments], timeout + CHILD_GRACE, cwd=SRC, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0
		except ChildTimeout:
			return False

	def publish(self, artifact, path, timeout):
		return self.scenario_child(['dev-publish', artifact, path, '--timeout', str(timeout)], timeout)

	def reset(self, timeout):
		return self.scenario_child(['dev-reset', '--timeout', str(timeout)], timeout)

	def restart(self, timeout):
		return self.scenario_child(['dev-restart', '--timeout', str(timeout)], timeout)


def cmd_dev_test(args):
	import scenario

	verbose = '--verbose' in args
	rest = [a for a in args if not a.startswith('--')]

	if not rest:
		die('usage: dev-test [--verbose] <file.toml>...')
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
		die(f'development instance is {state}; scenarios need a ready one (`just lab dev-up`)')
	# The fixtures are checked against the tree they were made from BEFORE anything is published.
	# Validation used to check only that the files existed, so a fixture whose staged source had
	# been rebuilt was published as though it were current and the run's verdict was about bytes
	# from an older tree.
	stale = scenario.stale_fixtures()
	if stale:
		for complaint in stale:
			print(f'lab: {complaint}', file=sys.stderr)
		die('the scenario fixtures do not match this tree; rebuild them with boot/scenarios/make-fixtures.py')
	failures = 0
	# HELD ACROSS EVERY SCENARIO, not per scenario: teardown restores the instance for the run that
	# comes next, and a second run starting between two of them inherits a half-restored guest.
	with scenario_lease(f'{len(documents)} scenario(s)'):
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


# Run scenarios against a cold boot of any target: build it with the development profile, start
# one guest, drive it over the same protocol the persistent instance uses, and take it down again.
#
# This is what keeps a migrated application test from narrowing the set of architectures it is
# exercised on. The persistent instance is x86_64-only and always will be - it exists to be fast,
# and it earns that by staying up - but a scenario is data interpreted on the host, so the only
# thing that ever tied it to one target was that no other target had a guest that would answer.
# Now they do, and this is the command that starts one.
def cmd_scenario_cold(args):
	import scenario

	global CHANNEL_OVERRIDE, SERIAL_OVERRIDE, MON_OVERRIDE, QMP_OVERRIDE
	verbose = '--verbose' in args
	rest = [a for a in args if not a.startswith('--')]
	if len(rest) < 2 or rest[0] not in ('x86_64', 'aarch64', 'riscv64'):
		die('usage: scenario-cold [--verbose] <x86_64|aarch64|riscv64> <file.toml>...')
	target, paths = rest[0], rest[1:]
	documents = []
	for path in paths:
		try:
			documents.append((path, scenario.load(path)))
		except scenario.ScenarioError as error:
			die(str(error))

	env = dict(os.environ, LIBER_DEVELOPMENT='1')
	build_timeout = arg_value(args, '--build-timeout', 3600)
	print(f'lab: building {target} with the development profile')
	# Bounded, and its whole group is stopped on expiry. A cross build for an emulated target is the
	# longest-running child this file starts, and it had no deadline at all - so a wedged compiler
	# held a command that is documented as bounded for as long as it liked.
	try:
		if run_child(build_command(target), build_timeout, cwd=SRC, env=env).returncode != 0:
			die(f'the {target} system did not build')
	except ChildTimeout as expired:
		die(f'the {target} build did not finish within {build_timeout} s ({expired})')
	triple = {'x86_64': 'x86_64-unknown-none', 'aarch64': 'aarch64-unknown-none', 'riscv64': 'riscv64gc-unknown-none-elf'}[target]

	# A NAMESPACE OF ITS OWN, on every target. This took the persistent instance's unsuffixed x86
	# path and unlinked it below without checking whether that instance was up - so a cold run and
	# the persistent instance destroyed each other. `COLD=1` in the guest environment makes the
	# runner bind the matching names.
	socket_path = os.path.join(BUILD, f'dev-channel-cold-{target}.sock')
	kernel = os.path.join(BUILD_ROOT, 'cargo', 'kernel', triple, 'debug', 'kernel')
	log = os.path.join(BUILD, f'cold-{target}.log')
	with contextlib.suppress(OSError):
		os.remove(socket_path)
	# The previous run's serial log has to go with it. The runner reads readiness out of this
	# file, and a leftover one still holds the last run's shell prompt - which is matched
	# immediately, so the scenario starts typing into a guest that is still booting.
	with contextlib.suppress(OSError):
		os.remove(log)
	# A cold guest has run nothing, so the record of which scenarios have already run starts
	# empty here for the same reason `dev-up` empties it: first-touch residency is relative to a
	# boot, and this is a new one.
	#
	# RUN-LOCAL, per target. Every cold run on every architecture shared the persistent instance's
	# one `dev-scenarios-seen`, so a cold aarch64 run deleted the x86 instance's record and its own
	# first/repeat verdicts were decided by whatever had run last anywhere.
	scenario.SEEN_PATH = os.path.join(BUILD, f'cold-{target}-scenarios-seen')
	with contextlib.suppress(OSError):
		os.remove(scenario.SEEN_PATH)
	# THE FIXTURES FOR THIS TARGET, built here and checked here.
	#
	# The documents name `{staged}` and `{fixtures}`; this is what decides which target those resolve
	# to. Without it a cold aarch64 or riscv64 run published x86 images into a guest that refuses
	# them by design, so a substantial part of the advertised cross-architecture suite could not
	# exercise its stated behaviour at all.
	scenario.TARGET = target
	fixture_env = dict(env, FIXTURE_TARGET=target)
	try:
		if run_child([os.path.join(HERE, 'scenarios', 'make-fixtures.py')], 600, cwd=SRC, env=fixture_env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE).returncode != 0:
			die(f'the {target} scenario fixtures could not be built')
	except ChildTimeout as expired:
		die(f'building the {target} scenario fixtures did not finish ({expired})')
	stale = scenario.stale_fixtures(target)
	if stale:
		for complaint in stale:
			print(f'lab: {complaint}', file=sys.stderr)
		die(f'the {target} scenario fixtures do not match this tree')
	guest_env = dict(env, DEV_PROFILE='1', COLD='1', SERIAL=f'file:{log}')
	# UEFI ON THE DEVICE-TREE TARGETS, because a direct boot cannot carry what a driven guest needs.
	#
	# `-kernel` on `virt` has no module hand-off: the machine takes one blob, so a direct boot gets
	# the init package and NOT the system volume package beside it. That starts SystemManager and
	# stops - no shell, no development agent - and this runner then waited on a handshake that could
	# never arrive from a guest that looked perfectly alive. The runner refuses the combination now;
	# this is the side that asks for the boot that works. `run.sh` takes the same position for the
	# same reason.
	if target != 'x86_64':
		guest_env['UEFI'] = '1'
	print(f'lab: booting {target}; serial log {os.path.relpath(log, SRC)}')
	# The runner's own diagnosis goes into the serial log's neighbour rather than into /dev/null: it
	# is the process that says "loader EFI not found" or "init package not found", and discarding
	# that left every startup failure looking like a guest that never answered.
	runner_log = open(os.path.join(BUILD, f'cold-{target}-runner.log'), 'wb')
	guest = subprocess.Popen(['bash', 'boot/qemu-run.sh', target, kernel], cwd=SRC, env=guest_env, stdout=runner_log, stderr=runner_log, start_new_session=True)
	record_lab_guest(guest)
	failures = 0
	try:
		CHANNEL_OVERRIDE = socket_path
		SERIAL_OVERRIDE = log
		# An emulated guest is slower than the native one every scenario deadline was written
		# against, by roughly an order of magnitude on the interactive steps.
		scenario.TIME_SCALE = 1.0 if target == 'x86_64' else 10.0
		MON_OVERRIDE = os.path.join(BUILD, f'qemu-monitor-cold-{target}.sock')
		QMP_OVERRIDE = os.path.join(BUILD, f'qemu-qmp-cold-{target}.sock')
		# The guest is answering when it answers, not when a timer says so: poll the handshake
		# rather than sleeping for a number that would be wrong on both a fast and a slow target.
		# Generous on purpose. An emulated aarch64 or riscv64 guest takes minutes to bring its
		# whole chain up, and a deadline shorter than that does not report a slow boot - it
		# reports a hang, which is a different thing and sends whoever reads it somewhere else
		# entirely. This one was 300 s and cost a long hunt for a stall that was never there.
		deadline = time.time() + 1800
		while time.time() < deadline:
			if guest.poll() is not None:
				die(f'the {target} guest exited before it served the control channel (see {log})')
			try:
				sock, _, _ = proto_session(5, announce=False)
				sock.close()
				break
			except (OSError, SystemExit):
				pass
			time.sleep(1)
		else:
			die(f'the {target} guest never served the control channel (see {log})')
		# The agent answers as soon as DeviceManager starts it, which is well before the boot
		# chain has finished and a shell is at a prompt. A scenario's first step would otherwise
		# race the rest of the boot, and on an emulated target that race is not close.
		# Readiness is read from the guest's own serial log, not asked of a broker: a cold run has
		# no broker, so `wait_prompt` - which goes through one - can only ever fail here, and a
		# readiness check that cannot succeed is worse than none. Progress is printed while
		# waiting, because an emulated guest takes minutes and silence for minutes is
		# indistinguishable from a hang to whoever is watching.
		said = 0.0
		while time.time() < deadline:
			try:
				with open(log, 'rb') as handle:
					tail = handle.read()[-4096:]
			except OSError:
				tail = b''
			if PROMPT.search(strip_ansi(tail)):
				break
			if guest.poll() is not None:
				die(f'the {target} guest exited while booting (see {log})')
			if time.time() - said >= 30:
				said = time.time()
				print(f'lab: waiting for the {target} guest to reach a shell ({len(tail)} B of console output so far)')
			time.sleep(2)
		else:
			die(f'the {target} guest served its channel but never reached a shell (see {log})')
		for path, document in documents:
			name = document['name']
			try:
				elapsed = scenario.run(document, LabGuest(60), verbose)
				print(f'lab: {name} passed in {elapsed:.1f} s on {target} ({os.path.relpath(path, SRC)})')
			except scenario.ScenarioError as error:
				print(f'lab: {name} FAILED on {target}: {error}', file=sys.stderr)
				failures += 1
	finally:
		CHANNEL_OVERRIDE = None
		SERIAL_OVERRIDE = None
		MON_OVERRIDE = None
		QMP_OVERRIDE = None
		# TERM, then KILL. This sent TERM, waited fifteen seconds and SUPPRESSED the failure - so a
		# QEMU that ignores TERM outlived the command that started it, holding the sockets and disk
		# images the next run needs and appearing as a mysteriously busy resource somewhere else.
		stop_child_group(guest, grace=15)
		if guest.poll() is None:
			print(f'lab: the {target} guest (pgid {guest.pid}) did not stop even on SIGKILL', file=sys.stderr)
	if failures:
		die(f'{failures} of {len(documents)} scenario(s) failed on {target}')
	print(f'lab: {len(documents)} scenario(s) passed on {target}')


# ---- subcommands -----------------------------------------------------------

# The lab's own guest, and nothing else.
#
# This was `pkill -9 -f qemu-system-x86`, which matches by PATTERN across every process the user
# owns: a developer with an unrelated QEMU open - another project, a VM they were mid-way through -
# lost it to `lab boot`, `lab test` or `lab down`. The pattern cannot tell whose guest it is.
#
# The lab starts its guest with `start_new_session=True`, so it has a process group of its own.
# Recording that group at launch and killing it - after checking the group still holds a QEMU - is
# precise, needs no QEMU flag, and cannot reach a process the lab did not start.
LAB_PGIDFILE = os.path.join(BUILD, 'lab-guest.pgid')


def record_lab_guest(process):
	"""Remember the process group the lab just started, so it can kill that and nothing else."""
	try:
		os.makedirs(BUILD, exist_ok=True)
		with open(LAB_PGIDFILE, 'w') as handle:
			handle.write(str(os.getpgid(process.pid)))
	except OSError:
		pass


def kill_lab_guest():
	try:
		with open(LAB_PGIDFILE) as handle:
			pgid = int(handle.read().strip())
	except (OSError, ValueError):
		return
	# Only if that group still holds a QEMU. A recorded group whose number has been reused by
	# something unrelated is the same mistake one step along, and `pgrep -g` answers it directly.
	held = subprocess.run(['pgrep', '-g', str(pgid), '-f', 'qemu-system'], capture_output=True)
	if held.returncode == 0:
		try:
			os.killpg(pgid, signal.SIGKILL)
		except OSError:
			pass
	try:
		os.unlink(LAB_PGIDFILE)
	except OSError:
		pass


def cmd_boot(args):
	fresh = '--fresh' in args
	timeout = arg_value(args, '--timeout', 240)
	displays = [d for d in ('vnc', 'spice') if f'--{d}' in args]
	kill_lab_guest()
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
	# BUILD, then boot. The runner builds nothing, so without this `lab boot` boots whatever
	# artifacts were last left in `.build` - or refuses with "no kernel for x86_64" on a clean tree,
	# which reads as a broken lab rather than as a step nobody performed.
	if subprocess.run(build_command('x86_64'), cwd=SRC, env=env, stdout=qemu_log, stderr=qemu_log).returncode != 0:
		die(f'the build failed (see {QEMU_LOG})')
	record_lab_guest(subprocess.Popen(run_command(displays), cwd=SRC, env=env, stdout=qemu_log, stderr=qemu_log, start_new_session=True))
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
	if not reply.prompted:
		die(f'no shell prompt within {timeout} s (see {SERIAL_LOG})')
	print(f'lab: booted in {time.time() - started:.1f} s' + (' (fresh volume)' if fresh else ''))
	print(f'lab: serial log {os.path.relpath(SERIAL_LOG, SRC)}; try `just lab sh uname`')


def cmd_sh(args):
	timeout, rest = take_arg(args, '--timeout', 30)
	command = ' '.join(rest)
	if not command:
		die('usage: lab sh <command...>')
	reply = ctl_request(f'RUN {timeout} {command}', timeout, active_ctl_sock())
	text = strip_ansi(reply.data).decode(errors='replace').replace('\r\n', '\n')
	lines = text.split('\n')
	# Drop the echoed command line and the trailing prompt; the rest is the output.
	if lines and command in lines[0]:
		lines = lines[1:]
	while lines and (lines[-1] == '' or PROMPT.search(lines[-1].encode())):
		lines.pop()
	print('\n'.join(lines))
	# AND THE OUTCOME DECIDES THE EXIT STATUS. This printed whatever bytes arrived and exited 0
	# either way, so a command that hung until its deadline - having printed part of its output -
	# was indistinguishable from one that finished. Anything wrapping `lab sh` in a script read that
	# as success. There is still no guest command status here: this says whether the REQUEST
	# completed, which is the honest claim and is the one the broker can actually make.
	if not reply.prompted:
		print(f'lab: the request ended as {reply.outcome}, not at a prompt - the output above may be partial', file=sys.stderr)
		sys.exit(1)


def cmd_wait(args):
	timeout = arg_value(args, '--timeout', 60)
	reply = ctl_request(f'WAIT {timeout}', timeout, active_ctl_sock())
	sys.exit(0 if reply.prompted else 1)


# Interrupt the guest's foreground job: one 0x03 byte on the serial console (the
# console's line discipline turns it into SIG_INT), then wait for the prompt.
def cmd_int(args):
	timeout = arg_value(args, '--timeout', 15)
	reply = ctl_request(f'INT {timeout}', timeout, active_ctl_sock())
	sys.exit(0 if reply.prompted else 1)


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

# The key names a scenario or a command may name directly, beyond letters and digits. A fixed
# vocabulary rather than a string handed through to QEMU: this is the one place where what a
# scenario says becomes what the emulator does, and an open channel there would be a way to
# run monitor commands from scenario data.
KEY_NAMES = frozenset(list(KEYMAP.values()) + ['ret', 'esc', 'tab', 'spc', 'backspace', 'delete', 'insert', 'home', 'end', 'pgup', 'pgdn', 'up', 'down', 'left', 'right'] + [f'f{index}' for index in range(1, 13)])
MODIFIERS = frozenset(['ctrl', 'alt', 'shift'])
# The pointer buttons QEMU knows, and the only ones a scenario may name.
POINTER_BUTTONS = frozenset(['left', 'middle', 'right', 'wheel-up', 'wheel-down'])
# The absolute axis range QEMU's input layer uses, whatever the guest's resolution is. A
# scenario names a fraction of the screen, which is the only thing it can know without being
# told the mode the guest happens to be in.
ABSOLUTE_RANGE = 32767


# One key name, validated. A chord is modifiers and one key joined by dashes, which is the
# form the monitor already takes.
def key_sequence(name):
	parts = name.split('-')
	key = parts[-1]
	if not parts or any(part not in MODIFIERS for part in parts[:-1]):
		return None
	if len(key) == 1 and (key.isalpha() or key.isdigit()):
		return name.lower()
	return name if key in KEY_NAMES else None


# The keys one line of text is typed as, or None when a character has no mapping. Typed as key
# events rather than written into the console: this is the path a person's keyboard takes -
# through the emulated device, the driver, InputService and the session - and the only way a
# scenario exercises any of it.
def text_keys(text, enter):
	keys = []
	for character in text + ('\n' if enter else ''):
		if character.isalpha():
			keys.append(f'shift-{character.lower()}' if character.isupper() else character)
		elif character.isdigit():
			keys.append(character)
		elif character in KEYMAP:
			keys.append(KEYMAP[character])
		else:
			return None
	return keys


# QMP, for the input events the human monitor has no command for. The greeting has to be read
# and capabilities negotiated before anything is accepted, so a connection is made per batch
# rather than kept: these are rare, and a socket held open across a guest restart would be a
# thing to invalidate.
# The most bytes a QMP peer may queue before this gives up. QEMU's replies are small; a stream that
# never produces a newline is a peer that is not QMP, and reading it forever is how a helper with a
# timeout still hangs.
QMP_MAX_BUFFER = 4 * 1024 * 1024


def qmp_command(execute, arguments=None, timeout=5):
	if not os.path.exists(qmp_sock()):
		die('no QEMU QMP socket (is the instance up?)')
	conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
	conn.settimeout(timeout)
	buffer = bytearray()

	# One JSON object, whatever it is.
	def read_object():
		while b'\n' not in buffer:
			more = conn.recv(65536)
			if not more:
				die('the QEMU QMP socket closed')
			buffer.extend(more)
			if len(buffer) > QMP_MAX_BUFFER:
				die('the QMP peer sent more than one reply frame without a newline')
		line, _, rest = bytes(buffer).partition(b'\n')
		buffer[:] = rest
		try:
			return json.loads(line)
		except ValueError:
			die('the QMP peer sent something that is not JSON')

	# THE OBJECT THAT ANSWERS THIS REQUEST, and not whatever arrives next.
	#
	# QMP is asynchronous: a device event, a reset, a stop or a job update can arrive between a
	# request and its response. This used to read exactly the next line and return it - so an event
	# became the answer, and because an event carries no `error` key, a refused or never-executed
	# command was reported as a success with `None`. Every later read was then one object out of
	# step. Requests carry an `id` now and this skips everything that is not the matching response,
	# which is what the `id` is for.
	def response_to(request_id):
		deadline = time.monotonic() + timeout
		while True:
			answer = read_object()
			if 'event' in answer:
				continue
			if answer.get('id') == request_id:
				return answer
			# A reply with someone else's id, or none, is not this request's answer. It can only
			# come from a socket shared with something else, which is worth saying rather than
			# silently skipping past.
			if time.monotonic() > deadline:
				die(f'no QMP reply for request {request_id} within {timeout} s')

	try:
		conn.connect(qmp_sock())
		greeting = read_object()
		if 'QMP' not in greeting:
			die('the QMP socket did not open with a greeting')
		conn.sendall(json.dumps({'execute': 'qmp_capabilities', 'id': 'caps'}).encode() + b'\n')
		negotiated = response_to('caps')
		if 'error' in negotiated:
			die(f'QMP capability negotiation refused: {negotiated["error"].get("desc", negotiated["error"])}')
		request = {'execute': execute, 'id': 'cmd'}
		if arguments is not None:
			request['arguments'] = arguments
		conn.sendall(json.dumps(request).encode() + b'\n')
		answer = response_to('cmd')
		if 'error' in answer:
			die(f'QMP {execute} refused: {answer["error"].get("desc", answer["error"])}')
		return answer.get('return')
	finally:
		conn.close()


# Send key events into the guest through the emulated keyboard.
# `deadline` is a monotonic instant, not a per-key allowance.
#
# A batch is up to 64 keys and each key was its own five-second monitor operation plus a pause, so a
# step declaring five seconds could spend several minutes inside this and the scenario's own total
# deadline meant nothing while it did. The budget is the caller's and it is spent, not multiplied.
def send_keys(keys, deadline=None):
	for name in keys:
		sequence = key_sequence(name)
		if sequence is None:
			die(f'no key named {name!r}')
		if deadline is not None and time.monotonic() >= deadline:
			die(f'the key batch ran out of time with {len(keys)} key(s) requested')
		remaining = 5 if deadline is None else max(0.5, deadline - time.monotonic())
		monitor_command(f'sendkey {sequence}', timeout=remaining)
		# The guest's line discipline is a real one: keys arriving faster than it drains lose
		# nothing, but pacing them keeps a burst from being one indistinguishable event.
		time.sleep(0.05)
	return True


# Send pointer events into the guest through the emulated tablet. `x` and `y` are fractions of
# the screen, absolute because the device is a tablet; a button is pressed and released as one
# event batch, which is what a click is.
def send_pointer(x=None, y=None, button=None, action='click', timeout=None):
	events = []
	if x is not None:
		events.append({'type': 'abs', 'data': {'axis': 'x', 'value': int(max(0.0, min(1.0, x)) * ABSOLUTE_RANGE)}})
	if y is not None:
		events.append({'type': 'abs', 'data': {'axis': 'y', 'value': int(max(0.0, min(1.0, y)) * ABSOLUTE_RANGE)}})
	if button is not None:
		if button not in POINTER_BUTTONS:
			die(f'no pointer button named {button!r}')
		if action in ('press', 'click'):
			events.append({'type': 'btn', 'data': {'down': True, 'button': button}})
		if action in ('release', 'click'):
			events.append({'type': 'btn', 'data': {'down': False, 'button': button}})
	if not events:
		die('a pointer event needs a position, a button, or both')
	qmp_command('input-send-event', {'events': events}, timeout=5 if timeout is None else max(1, timeout))
	return True


# Named keys and chords, or a line of text with `--text`. The two are separate spellings
# because they are separate things: `ctrl-c` is one event and `ls -l` is nine, and guessing
# which was meant from what a word looks like would eventually guess wrong.
def cmd_dev_stop(args):
	timeout = arg_value(args, '--timeout', 10)
	sock, buffer, _ = proto_session(timeout)
	try:
		opcode, _, body = proto_request(sock, buffer, 2, OP_LAUNCH_STOP, timeout=timeout, what='stopping the launched program', tolerate=(25,))
		if opcode != OP_LAUNCH_STOP_ACK:
			die('nothing has been launched through the control channel')
		print('lab: stopped the launched program' if body[:1] and body[0] else 'lab: the launched program had already finished')
	finally:
		sock.close()


def cmd_dev_key(args):
	if '--text' in args:
		text = ' '.join(args[args.index('--text') + 1:])
		keys = text_keys(text, '--no-enter' not in args)
		if keys is None:
			die(f'no key mapping for one of the characters in {text!r}')
	else:
		keys = [argument for argument in args if not argument.startswith('--')]
		if not keys:
			die('usage: dev-key <key|chord>...   or   dev-key --text <line>   e.g. `dev-key ctrl-c up ret`')
	send_keys(keys)
	print(f'lab: sent {len(keys)} key event(s) through the emulated keyboard')


def cmd_dev_pointer(args):
	x, rest = take_fraction(args, '--x')
	y, rest = take_fraction(rest, '--y')
	action = 'click'
	for name in ('press', 'release', 'click'):
		if f'--{name}' in rest:
			action = name
	button = next((argument for argument in rest if not argument.startswith('--')), None)
	if x is None and y is None and button is None:
		die('usage: dev-pointer [--x FRACTION] [--y FRACTION] [--press|--release|--click] [button]')
	send_pointer(x, y, button, action)
	print('lab: sent a pointer event through the emulated tablet')


# A screen fraction option, kept apart from `take_arg` because that one parses integers and a
# position here is 0.0 to 1.0.
# The string form of `take_arg`, for options whose value is a name rather than a number.
def take_string_arg(args, name, default):
	value, rest, skip = default, [], False
	for index, argument in enumerate(args):
		if skip:
			skip = False
			continue
		if argument == name and index + 1 < len(args):
			value, skip = args[index + 1], True
		elif argument.startswith(name + '='):
			value = argument.split('=', 1)[1]
		else:
			rest.append(argument)
	return value, rest


def take_fraction(args, name):
	value, rest, skip = None, [], False
	for index, argument in enumerate(args):
		if skip:
			skip = False
			continue
		if argument == name and index + 1 < len(args):
			value, skip = argument_fraction(args[index + 1], name), True
		elif argument.startswith(name + '='):
			value = argument_fraction(argument.split('=', 1)[1], name)
		else:
			rest.append(argument)
	return value, rest


def argument_fraction(text, name):
	try:
		value = float(text)
	except ValueError:
		die(f'{name} takes a fraction of the screen, 0.0 to 1.0, not {text!r}')
	if not 0.0 <= value <= 1.0:
		die(f'{name} is {value}; a fraction of the screen is 0.0 to 1.0')
	return value


def cmd_key(args):
	text = ' '.join(args)
	keys = text_keys(text, True)
	if keys is None:
		die(f'no sendkey mapping for one of the characters in {text!r}')
	send_keys(keys)


def monitor_command(command, timeout=5):
	if not os.path.exists(mon_sock()):
		die('no QEMU monitor socket (is the instance up?)')
	conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
	conn.connect(mon_sock())
	conn.settimeout(timeout)
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
	with open(PCAP, 'rb') as handle:
		data = handle.read()
	offset, index = 24, 0
	while offset + 16 <= len(data):
		_, _, incl, _ = struct.unpack('<IIII', data[offset:offset + 16])
		# A RECORD LENGTH IS A CLAIM, and this file may be truncated - a capture is stopped by
		# whatever stopped the guest. A slice past the end is silently short in Python, so an
		# incomplete final record used to be decoded as a whole packet, with every offset inside it
		# reading whatever was there.
		if incl > len(data) - offset - 16:
			print(f'{index + 1}: truncated record, {incl} B claimed and {len(data) - offset - 16} B present')
			return
		packet = data[offset + 16:offset + 16 + incl]
		offset += 16 + incl
		index += 1
		print(f'{index}: {decode_packet(packet)}')


# EVERY OFFSET CHECKED AGAINST THE PACKET, because a capture is untrusted input in the ordinary
# sense: it is bytes off a wire, truncated at whatever length the capture was cut at, and the
# header fields that say where the transport starts are themselves in the packet. `ihl` was read
# and used as an offset with no check that it landed inside the frame - a nibble of 15 puts the
# transport header 60 bytes in - and the transport fields were then unpacked from there. That is a
# `struct.error` traceback out of a diagnostic tool, or a decode of adjacent bytes.
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
	# The minimum legal IHL is 20 bytes, and the header has to be inside the frame.
	if ihl < 20 or 14 + ihl > len(p):
		return f'IP {src} -> {dst}: header length {ihl} B does not fit a {len(p)} B frame'
	t = 14 + ihl

	# How many transport bytes are actually present from `t` onward.
	def fits(count):
		return t + count <= len(p)

	if proto == 1:
		return f'ICMP {src} -> {dst} type {p[t]} ({total} B)' if fits(1) else f'ICMP {src} -> {dst}: no type byte in a {len(p)} B frame'
	if proto == 17:
		if not fits(8):
			return f'UDP {src} -> {dst}: header truncated in a {len(p)} B frame'
		sp, dp = struct.unpack('>HH', p[t:t + 4])
		return f'UDP {src}:{sp} -> {dst}:{dp} len {total - ihl - 8}'
	if proto == 6:
		if not fits(20):
			return f'TCP {src} -> {dst}: header truncated in a {len(p)} B frame'
		sp, dp = struct.unpack('>HH', p[t:t + 4])
		seq, ack = struct.unpack('>II', p[t + 4:t + 12])
		doff = (p[t + 12] >> 4) * 4
		flags = ''.join(name for name, bit in (('F', 1), ('S', 2), ('R', 4), ('P', 8), ('A', 16)) if p[t + 13] & bit)
		win = struct.unpack('>H', p[t + 14:t + 16])[0]
		# The data offset is also a claim. Options that run past the frame are reported as absent
		# rather than as a short hex string that looks like the whole of them.
		opts = p[t + 20:t + doff].hex() if 20 <= doff and fits(doff) else ''
		payload = total - ihl - doff
		return f'TCP {src}:{sp} -> {dst}:{dp} [{flags}] seq={seq} ack={ack} win={win} len={payload}' + (f' opts={opts}' if opts else '')
	return f'IP proto {proto} {src} -> {dst} ({total} B)'


def ip_str(b):
	return '.'.join(str(x) for x in b)


def cmd_test(args):
	timeout = arg_value(args, '--timeout', 900)
	kill_lab_guest()
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
	if os.path.exists(mon_sock()):
		try:
			monitor_command('quit')
		except (SystemExit, OSError):
			pass
	time.sleep(1)
	kill_lab_guest()
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


COMMANDS = {'boot': cmd_boot, 'sh': cmd_sh, 'int': cmd_int, 'wait': cmd_wait, 'log': cmd_log, 'key': cmd_key, 'monitor': cmd_monitor, 'usb-attach': cmd_usb_attach, 'usb-detach': cmd_usb_detach, 'pcap': cmd_pcap, 'test': cmd_test, 'shot': cmd_shot, 'quit': cmd_quit, 'dev-up': cmd_dev_up, 'dev-status': cmd_dev_status, 'dev-console': cmd_dev_console, 'dev-log': cmd_dev_log, 'dev-ping': cmd_dev_ping, 'dev-publish': cmd_dev_publish, 'dev-generations': cmd_dev_generations, 'dev-rollback': cmd_dev_rollback, 'dev-type': cmd_dev_type, 'dev-reset': cmd_dev_reset, 'dev-reboot': cmd_dev_reboot, 'dev-restart': cmd_dev_restart, 'dev-stop': cmd_dev_stop, 'dev-key': cmd_dev_key, 'dev-pointer': cmd_dev_pointer, 'dev-test': cmd_dev_test, 'dev-launch': cmd_dev_launch, 'dev-loop': cmd_dev_loop, 'dev-clean': cmd_dev_clean, 'dev-down': cmd_dev_down, 'scenario-cold': cmd_scenario_cold}


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
