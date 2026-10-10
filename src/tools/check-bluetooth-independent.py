#!/usr/bin/env python3
"""Production USB Bluetooth interoperability with pinned RootCanal/Bumble peers.

The host controls peer actions, never guest packets or guest verdicts. Independent
codec readers check the actual over-the-radio payloads. BNEP/OPP adapters are
explicitly same-team peers; DHCP and IP answers come from dnsmasq/Linux.
Existing in-guest fixtures retain malformed-packet and resource-exhaustion cases.
An explicit BOOT_IMAGE is authoritative. Otherwise prepare one current private
development image before starting peers, then reuse it and its paired disk for both boots.
"""
import argparse
import ast
import contextlib
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
HARNESS = ROOT / 'src/harness'
sys.path.insert(0, str(HARNESS))
spec = importlib.util.spec_from_file_location('tickless', Path(__file__).with_name('check-tickless-idle.py'))
tickless = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tickless)
GateError = tickless.GateError


def require(condition, message):
    if not condition:
        raise GateError(message)


def note(message):
    print(f'bluetooth-independent: {message}', flush=True)


def matches(expected, text):
    if callable(expected):
        return bool(expected(text))
    if isinstance(expected, str):
        expected = expected.encode()
    return expected in text


def require_hid_refusal(reply, *, forgotten=False):
    # Classic L2CAP result 3 is Security Block. After forget, connection/auth
    # itself may fail: accept only actual HCI authentication/key/security codes,
    # including 0x0F, the shipping host's inbound unknown-peer policy refusal.
    code = reply.get('error_code')
    namespace = reply.get('error_namespace')
    refused = namespace == 'L2CAP' and code == 3
    if forgotten:
        refused |= namespace == 'hci' and code in (0x05, 0x06, 0x0E, 0x0F, 0x18, 0x2F)
    require(reply.get('ok') is False and type(code) is int and refused,
            f'{"forgotten" if forgotten else "untrusted"} keyboard lacked an explicit security refusal: {reply}')


def audio_device(text, *, voice=False, route=False):
    rows = []
    for line in text.splitlines():
        found = re.match(r'device (\d+): .*\(Bluetooth([^)]*)\)', line)
        if found and (', voice' in found[2]) == voice and (', a phone playing' in found[2]) == route:
            rows.append((int(found[1]), line))
    require(len(rows) == 1, f'expected one Bluetooth audio device (voice={voice}, route={route}), got {rows}')
    return rows[0]


def controller_line(text):
    rows = [line for line in text.splitlines() if line.startswith('controller ')]
    require(len(rows) == 1, f'expected one real controller, got {rows}')
    return rows[0]


def wait_audio(g, **kind):
    end = time.monotonic() + 30
    while True:
        output = g.command('audioctl devices')
        try:
            return audio_device(output, **kind)
        except GateError:
            require(time.monotonic() < end, f'Bluetooth audio inventory did not settle: {output}')
            g.pause(.2)


def capture_baseline(status):
    return {row['lc3']: row['frames'] for row in status['captures']}


def verify_captures(status, baseline, frequencies, rate):
    """Require fresh decoded audio even when a PCM endpoint keeps its CIS open."""
    require(not status['errors'], f'independent LE peer errors: {status["errors"]}')
    selected = []
    for name, frequency in frequencies.items():
        found = []
        for row in status['captures']:
            if row['peer'] != name or row['rate'] != rate:
                continue
            previous = baseline.get(row['lc3'])
            # Reused captures report a rolling one-second PCM window. Replace
            # that whole window before accepting its pitch as this phase's audio.
            fresh = row['frames'] >= 25 if previous is None else (row['frames'] - previous) * row['duration_us'] >= 1_000_000
            if fresh:
                found.append(row)
        if not found:
            return None
        row = found[-1]
        require(row['lost'] == 0 and row['sequence_gaps'] == 0, f'{name}: lost ISO frames: {row}')
        if row['rms'] < 200 or abs(row['frequency_hz'] - frequency) > 100:
            return None
        if rate == 48000:
            require(row['octets'] == 120 and row['location'] == (1 if name == 'le-left' else 2), f'{name}: wrong channel configuration: {row}')
        selected.append(row)
    return selected


class Gate:
    require = staticmethod(require)

    def __init__(self, work, sockets):
        self.work, self.sockets = work, sockets
        self.serial = None
        self.guest = None
        self.guest_address = None
        self.ready = None
        self.processes = []
        self.logs = []
        self.control_log = (work / 'control.jsonl').open('w')
        self.results = []
        self.fixture_requirements = []
        self.boot_image = None

    def spawn(self, command, log, env=None):
        stream = (self.work / log).open('wb')
        self.logs.append(stream)
        process = subprocess.Popen(command, cwd=ROOT, env=env, stdout=stream, stderr=subprocess.STDOUT, start_new_session=True)
        self.processes.append(process)
        return process

    def await_file(self, path, process, timeout=60):
        end = time.monotonic() + timeout
        while not path.exists():
            require(process.poll() is None, f'host/guest process exited before {path} appeared')
            require(time.monotonic() < end, f'timeout waiting for {path}')
            time.sleep(.05)

    def prepare_image(self):
        # Like guest-gate.sh, own one fresh medium unless the caller supplied one.
        # The runner otherwise packages whichever ordinary/development artifacts
        # a preceding gate left behind. An environment flag alone cannot rebuild them.
        supplied = os.environ.get('BOOT_IMAGE')
        image = (ROOT / supplied).resolve() if supplied else self.work / 'development.iso'
        if not supplied:
            env = dict(os.environ, LIBER_DEVELOPMENT='1', LIBER_IMAGE_OUTPUT=str(image))
            process = self.spawn(['bash', str(ROOT / 'image.sh'), '--format', 'iso', '--dma-mode', 'harness'],
                                 'image-build.log', env)
            require(process.wait() == 0, 'the current development image did not build; see image-build.log')
        require(image.is_file(), f'the selected development image does not exist: {image}')
        disk = self.work / 'system.img'
        require(not disk.exists(), 'refusing to replace a persistent Bluetooth test disk')
        # Extract THIS medium's paired volume, never a mutable canonical volume.
        # Subsequent cold boots reuse the now-written disk without reseeding it.
        with tempfile.TemporaryDirectory(prefix='paired-', dir=self.work) as scratch:
            esp = Path(scratch) / 'esp.img'
            extract = self.spawn(['xorriso', '-osirrox', 'on', '-indev', str(image), '-extract',
                                  '/boot/efiboot.img', str(esp)], 'image-extract.log')
            require(extract.wait() == 0, 'the selected image could not provide its ESP; see image-extract.log')
            copy = self.spawn(['mcopy', '-i', str(esp), '::/system-volume.img', str(disk)], 'image-volume.log')
            require(copy.wait() == 0 and disk.is_file(), 'the selected image could not seed its paired volume; see image-volume.log')
        def identity(path):
            with path.open('rb') as stream:
                return dict(path=str(path), bytes=path.stat().st_size,
                            sha256=hashlib.file_digest(stream, 'sha256').hexdigest())
        self.boot_image = image
        self.record('boot-artifacts', supplied=bool(supplied), image=identity(image), initial_disk=identity(disk))

    def start_peers(self):
        python = ROOT / '.build/bluetooth-oracles/venv/bin/python'
        require(python.exists(), 'prepare pinned oracles first: python3 src/harness/prepare-bluetooth-oracles.py')
        peers = self.spawn([str(python), str(HARNESS / 'bluetooth-radio-peers.py'), '--work', str(self.work / 'radio'), '--socket', str(self.sockets / 'peers')], 'peers.log')
        self.await_file(self.work / 'radio/ready.json', peers)
        self.ready = json.loads((self.work / 'radio/ready.json').read_text())
        self.bridge = self.spawn([sys.executable, str(HARNESS / 'usbredir_device.py'), '--emulate', 'bt-bridge', '--hci', self.ready['endpoint'], '--connections', '2', '--socket', str(self.sockets / 'usb'), '--ready', str(self.sockets / 'usb-ready')], 'usb.log')
        self.await_file(self.sockets / 'usb-ready', self.bridge)

    def control(self, action, peer='phone', group='classic', allow_error=False, **values):
        request = dict(action=action, peer=peer, group=group, **values)
        if action == 'events':
            request.pop('group')
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(45)
            sock.connect(str(self.sockets / 'peers'))
            sock.sendall((json.dumps(request) + '\n').encode())
            with sock.makefile('rb') as reader:
                line = reader.readline()
        reply = json.loads(line)
        self.control_log.write(json.dumps(dict(seconds=time.monotonic(), request=request, reply=reply)) + '\n')
        self.control_log.flush()
        if allow_error:
            return reply
        require(reply['ok'], f'independent peer refused {request}: {reply}')
        return reply['result']

    def eventually(self, action, predicate, peer='phone', timeout=120, **values):
        end = time.monotonic() + timeout
        last = None
        while time.monotonic() < end:
            last = self.control(action, peer=peer, **values)
            if predicate(last):
                return last
            self.pause(.2)
        raise GateError(f'peer condition for {action} did not occur: {last}')

    def address(self, peer):
        groups = self.ready['groups']
        value = groups['le-generic'][peer] if peer in ('tag', 'display', 'remote', 'dual') else groups['le']['peers'][peer] if peer.startswith('le-') else groups['classic'][peer]
        return value['address'].split('/')[0].lower()

    def events(self, since=0):
        return self.control('events', since=since)

    def event(self, name, peer, since=0, timeout=60):
        result = self.eventually('events', lambda data: any(e['event'] == name and e.get('peer') == peer for e in data['events']), since=since, timeout=timeout)
        return next(e for e in result['events'] if e['event'] == name and e.get('peer') == peer)

    def pause(self, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if self.serial:
                self.serial.pump(min(.1, max(0, end - time.monotonic())))
            else:
                time.sleep(min(.1, max(0, end - time.monotonic())))

    def start(self, command):
        note(f'guest: {command}')
        mark = len(self.serial.data)
        self.serial.type(command.encode() + b'\n')
        return mark

    def wait(self, mark, expected, timeout=120):
        self.serial.wait_for(mark, lambda text: matches(expected, text), timeout, f'guest evidence {expected!r}')
        return self.serial.text_since(mark).decode(errors='replace')

    def finish(self, mark, timeout=120):
        self.serial.wait_prompt(mark, timeout, 'command completion')
        return self.serial.text_since(mark).decode(errors='replace')

    def wait_hid_input(self, label, *, peer='keyboard', group='classic'):
        # Encryption/CCCD readiness is at the peer. InputService opens its input
        # stream independently, on a two-second retry. Observe that whole path
        # with an empty HID-typed line before sending the acceptance text once.
        # Serial.wait_prompt may nudge via UART, so it cannot prove HID adoption.
        end = time.monotonic() + 10
        attempts = 0
        with (self.work / 'hid-readiness.jsonl').open('a') as log:
            while time.monotonic() < end:
                require(self.serial.settle(tickless.lab.PROMPT_SETTLE, min(1, end - time.monotonic())),
                        f'{group}/{peer} input readiness could not drain prior serial output')
                require(time.monotonic() < end, f'{group}/{peer} input readiness exceeded its 10 s bound')
                mark = len(self.serial.data)
                attempts += 1
                row = dict(label=label, peer=peer, group=group, attempt=attempts, mark=mark,
                           seconds=time.monotonic(), status='requested', text='\n')
                log.write(json.dumps(row) + '\n')
                log.flush()
                ready = False
                try:
                    self.control('type', peer=peer, group=group, text='\n')
                    attempt_end = min(end, time.monotonic() + 1)
                    while time.monotonic() < attempt_end:
                        # Require the Enter's line ending and a complete fresh
                        # prompt, not a prompt prefix or anything before mark.
                        text = self.serial.text_since(mark)
                        if re.search(rb'\nvol://[^\r\n>]*> $', text):
                            settle = tickless.lab.PROMPT_SETTLE
                            if time.monotonic() + settle <= attempt_end and not self.serial.pump(settle):
                                ready = True
                                break
                        self.serial.pump(min(.05, max(0, attempt_end - time.monotonic())))
                finally:
                    log.write(json.dumps(dict(row, status='ready' if ready else 'not-ready',
                                              completed=time.monotonic(), end=len(self.serial.data),
                                              observed=self.serial.text_since(mark).decode(errors='replace'))) + '\n')
                    log.flush()
                if ready:
                    self.record('hid-input-ready', label=label, peer=peer, group=group, attempts=attempts)
                    return
        raise GateError(f'{group}/{peer} did not reach InputService and a fresh console prompt within 10 s')

    def command(self, command, expect=None, timeout=120):
        mark = self.start(command)
        output = self.finish(mark, timeout)
        if expect is not None:
            require(matches(expect, output.encode()), f'`{command}` lacks {expect!r}: {output}')
        require(not any(f'{probe}: FAIL' in output for probe in ('audioprobe', 'btserial', 'btgatt')), f'guest probe failed: {output}')
        return output

    def service_status(self, name, state, desired):
        # lssvc's compact JSON is one complete line. Command echo and shell
        # prompt are not status evidence; prefix matches must not select a peer.
        output = self.command(f'lssvc json-min {name}')
        arrays = []
        for line in output.splitlines():
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(value, list):
                arrays.append(value)
        require(len(arrays) == 1, f'{name}: missing or ambiguous supervisor JSON: {output}')
        rows = [row for row in arrays[0] if isinstance(row, dict) and row.get('name') == name]
        require(len(rows) == 1, f'{name}: missing or duplicate supervisor row: {output}')
        row = rows[0]
        require(row.get('state') == state and row.get('desired') == desired,
                f'{name}: supervisor did not reach {state}/{desired}: {row}')
        require(type(row.get('epoch')) is int and row['epoch'] > 0,
                f'{name}: supervisor lacks a real process identity: {row}')
        return row

    def stop_service(self, name):
        before = self.service_status(name, 'running', 'running')
        output = self.command(f'stop {name}')
        require(f'supervisor: {name} did not end within its bound after SIG_KILL - it is left behind' not in output,
                f'{name}: supervisor acknowledged stop without observing process exit: {output}')
        # stop prints an ordered, newline-delimited teardown list on success.
        # A command echo, log line, failure or another stopped name cannot pass.
        reply = re.search(r'(?m)^stopped:\r?\n((?:[a-z][a-z0-9_.-]*\r?\n)+)', output)
        require(reply is not None and name in reply[1].splitlines(),
                f'{name}: supervisor stop was not acknowledged: {output}')
        stopped = self.service_status(name, 'stopped', 'stopped')
        require(stopped['epoch'] == before['epoch'], f'{name}: stop changed the instance being observed: {stopped}')
        self.record('service-stopped', service=name, before=before, stopped=stopped)
        return before['epoch']

    def start_service(self, name, previous_epoch):
        output = self.command(f'start {name}')
        require(re.search(r'(?m)^started: ' + re.escape(name) + r'\r?\n', output) is not None,
                f'{name}: supervisor start was not acknowledged: {output}')
        started = self.service_status(name, 'running', 'running')
        require(started['epoch'] != previous_epoch,
                f'{name}: restart did not create a new process: {started}')
        self.record('service-started', service=name, previous_epoch=previous_epoch, started=started)
        return started['epoch']

    def command_eventually(self, command, predicate, timeout=60):
        end = time.monotonic() + timeout
        while True:
            output = self.command(command)
            if predicate(output):
                return output
            require(time.monotonic() < end, f'`{command}` did not reach its required state: {output}')
            self.pause(.2)

    def record(self, name, **values):
        row = dict(scenario=name, **values)
        self.results.append(row)
        (self.work / 'results.json').write_text(json.dumps(self.results, indent=2) + '\n')
        note(f'PASS {name}')

    def fixture_required(self, row):
        self.fixture_requirements.append(row)
        self.results.append(dict(scenario='fixture-required', status='NOT RUN independently', **row))
        (self.work / 'results.json').write_text(json.dumps(self.results, indent=2) + '\n')
        note(f"FIXTURE REQUIRED {row['feature']}: {row['reason']} ({row['gate']})")

    def boot(self, label):
        serial_path = self.sockets / 'serial'
        serial_path.unlink(missing_ok=True)
        env = dict(os.environ, LIBER_DEVELOPMENT='1', LIBER_RUN_MODE='development', DEV_PROFILE='1', COLD='0', UEFI='1', SMP='2', LIBER_DEV_STATE=str(self.sockets), SERIAL=f'unix:{serial_path},server=on,wait=off', USB_REDIR_SOCKET=str(self.sockets / 'usb'), RUN_DISK=str(self.work / 'system.img'))
        for name in ('TEST', 'QEMU_EXTRA', 'NET_NONE', 'BT_FIXTURE', 'IDLE_FIXTURE', 'SYSTEM_SUSPEND_FIXTURE'):
            env.pop(name, None)
        require(self.boot_image is not None, 'the development image was not prepared')
        env['BOOT_IMAGE'] = str(self.boot_image)
        # qemu-run's explicit-medium path boots its embedded kernel, without a
        # canonical host ELF prerequisite or another image/loader build.
        kernel = ROOT / '.build/cargo/kernel/x86_64-unknown-none/debug/kernel'
        self.guest = self.spawn(['bash', str(HARNESS / 'qemu-run.sh'), 'x86_64', str(kernel)], f'{label}-runner.log', env)
        self.await_file(serial_path, self.guest)
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.connect(str(serial_path))
        log = (self.work / f'{label}-serial.log').open('wb')
        self.logs.append(log)
        self.serial = tickless.Serial(sock, log, 1)
        self.serial.wait_prompt(0, 240, 'boot')
        boot = self.serial.text_since(0)
        require(b'boot: the system volume is a paired block volume' in boot, 'guest did not boot the persistent paired volume')
        require(b'vol://system is a live copy in memory' not in boot, 'cold reboot would discard the bonds')
        end = time.monotonic() + 60
        while True:
            output = self.command('btctl list')
            found = re.search(r'controller \d+: ([0-9a-f:]{17}) \S+ - on', output)
            if found:
                break
            require(time.monotonic() < end, 'production USB Bluetooth controller never became ready')
            self.pause(.5)
        if self.guest_address:
            require(found[1] == self.guest_address, 'controller address changed across the cold reboot')
        self.guest_address = found[1]
        require('bt-fixture' not in output, 'a synthetic guest Bluetooth fixture was accidentally selected')
        self.record(label + '-boot', controller=self.guest_address)

    def poweroff(self):
        self.start('poweroff')
        end = time.monotonic() + 60
        while self.guest.poll() is None and time.monotonic() < end:
            try:
                self.serial.pump(.1)
            except GateError:
                break
        self.guest.wait(timeout=max(1, end - time.monotonic()))
        require(self.guest.returncode == 0, f'guest poweroff exited {self.guest.returncode}')
        self.serial.sock.close()
        self.serial = None
        self.guest = None

    def close(self):
        if self.serial:
            self.serial.sock.close()
            self.serial = None
        for process in reversed(self.processes):
            if process.poll() is None:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
        for stream in self.logs:
            stream.close()
        self.control_log.close()


def pair(g, peer, level, *, kind='bredr', io=None, sc=True, mitm=True, legacy=False, group='classic', address=None, required_prompt=None):
    address = address or g.address(peer)
    if io:
        g.control('pairing-config', peer=peer, group=group, io=io, sc=sc, mitm=mitm, ssp=not legacy)
    scan = g.command('btctl scan 5', timeout=90)
    require(address.lower() in scan.lower(), f'{peer} was not heard by the real controller')
    before = g.events()['next']
    command = f'btctl {"pair-legacy" if legacy else "pair"} {address} {kind}'
    mark = g.start(command)
    end = time.monotonic() + 120
    answered = set()
    while time.monotonic() < end:
        text = g.serial.text_since(mark)
        if b'bonded:' in text or b'btctl: the pairing failed' in text or b'was refused' in text:
            break
        if match := re.search(rb'Does it match (\d{6})\? \[y/n\]', text):
            if 'compare' not in answered:
                remote = g.event('pairing-number', peer, since=before)
                require(remote['number'] == int(match[1]), 'Numeric Comparison numbers differ between independent peers')
                g.serial.type(b'y\n')
                answered.add('compare')
        elif match := re.search(rb'Type (\d{6}) on .*Enter here to wait', text):
            if 'show' not in answered:
                g.control('pairing-number', peer=peer, group=group, number=int(match[1]))
                g.serial.type(b'\n')
                answered.add('show')
        elif b'Type the six digits ' in text and b' shows: ' in text:
            if 'enter' not in answered:
                remote = g.event('pairing-display', peer, since=before)
                g.serial.type(f'{remote["number"]:06}\n'.encode())
                answered.add('enter')
        elif b'PIN for ' in text and 'pin' not in answered:
            g.serial.type(b'1234\n')
            answered.add('pin')
        elif b'Allow it? [y/n]' in text and 'consent' not in answered:
            g.serial.type(b'y\n')
            answered.add('consent')
        g.pause(.05)
    output = g.finish(mark)
    require('bonded: ' + level in output, f'{peer} did not earn {level}: {output}')
    require(required_prompt is None or required_prompt in answered, f'{peer} did not complete required {required_prompt} prompt')
    g.record('pair-' + peer, level=level, prompts=sorted(answered))


def trust(g, peer, *profiles):
    for profile in profiles:
        g.command(f'btctl trust {g.address(peer)} {profile}', expect=f'is trusted for {profile}')


def keyboard(g, label):
    state = g.control('status')['keyboard']
    if state['connections']:
        g.control('disconnect', peer='keyboard')
        g.pause(.3)
    g.control('hid-reconnect', peer='keyboard', address=g.guest_address)
    g.eventually('status', lambda result: result['keyboard']['encrypted'] > state['encrypted'])
    g.wait_hid_input(label)
    mark = len(g.serial.data)
    g.control('type', peer='keyboard', text=f'echo independent-keyboard-{label}\n')
    g.finish(mark)
    text = g.serial.text_since(mark).decode()
    require(re.search(rf'(?m)^independent-keyboard-{label}\r?$', text), 'Bluetooth keyboard did not execute a console command through InputService')
    require(g.control('status')['keyboard']['pairings'] == state['pairings'], 'keyboard paired again instead of using its stored bond')
    g.record('keyboard-' + label)


def classic_input(g):
    # A bonded device is not input-trusted just because its key is known.
    reply = g.control('hid-reconnect', peer='keyboard', address=g.guest_address, allow_error=True)
    require_hid_refusal(reply)
    g.control('disconnect', peer='keyboard')
    trust(g, 'keyboard', 'input')
    keyboard(g, 'initial')
    # The keyboard descriptor exposes these keys. Neither may enter the trusted
    # power path; the next actual HID-typed command must still execute.
    g.control('report', peer='keyboard', data='05004c0000000000')
    g.control('report', peer='keyboard', data='0000000000000000')
    g.control('report', peer='keyboard', data='0000660000000000')
    g.control('report', peer='keyboard', data='0000000000000000')
    mark = len(g.serial.data)
    g.control('type', peer='keyboard', text='echo independent-keyboard-alive\n')
    g.finish(mark)
    require(re.search(rb'(?m)^independent-keyboard-alive\r?$', g.serial.text_since(mark)), 'radio power/chord prevented the following command')
    require(g.serial.text_since(0).count(b'BluetoothService: online') == 1, 'radio key caused an unexpected service/machine restart')
    trust(g, 'gamepad', 'input')
    mark = g.start('gamepad')
    g.wait(mark, 'gamepad - q or Ctrl+C leaves')
    g.control('hid-reconnect', peer='gamepad', address=g.guest_address)
    g.control('report', peer='gamepad', data='01400002')
    g.wait(mark, lambda text: b'buttons: 1' in text and b'X 64 (-127..127)' in text and b'hat 1 E' in text)
    departed = len(g.serial.data)
    g.control('disconnect', peer='gamepad')
    g.wait(departed, 'No gamepad is connected.')
    g.serial.type(b'q')
    g.finish(mark)
    g.record('classic-input', trusted_sink_keys='ignored', gamepad='button, axis, hat, departure')


def domain_snapshot(g):
    graph = g.command('graph bluetooth_service')
    require('graph: ' not in graph and any('bluetooth_service' in line and not line.strip().endswith('graph bluetooth_service') for line in graph.splitlines()),
            f'BluetoothService graph observation failed: {graph}')
    usage = g.command('usage')
    rows = [line for line in usage.splitlines() if 'bluetooth_service' in line and '=' in line]
    require(len(rows) == 1, f'BluetoothService domain accounting missing or ambiguous: {usage}')
    resources = {name: dict(used=used, limit=limit) for name, used, limit in re.findall(r'(\S+)=(\d+|unlimited)/(\d+|unlimited)', rows[0])}
    require(len(resources) == 6, f'incomplete BluetoothService resource accounting: {rows[0]}')
    for name, resource in resources.items():
        require(resource['used'] != 'unlimited' and resource['limit'] != 'unlimited' and int(resource['used']) <= int(resource['limit']),
                f'BluetoothService {name} exceeded or lacked its stated domain limit: {resource}')
    return dict(graph=graph, usage=usage, bluetooth_resources=resources,
                scope='live domain occupancy; retained heap pages include heap high-water; IPC queue peak is not measured')


def le_audio(g):
    g.control('start-advertisements', group='le')
    pair(g, 'le-left', 'Secure Connections, Just Works', kind='random')
    trust(g, 'le-left', 'audio', 'voice')
    g.command(f'btctl connect {g.address("le-left")} audio', expect='connecting audio')
    g.eventually('status', lambda state: bool(state['peers']['le-right']['connections']), group='le')
    g.command_eventually('btctl list', lambda output: any(g.address('le-right') in line.lower() and 'Secure Connections' in line and 'audio' in line for line in output.splitlines()))
    device, row = wait_audio(g)
    require('48000 Hz stereo' in row and 'default output' in row, f'coordinated set is not one stereo default: {row}')
    voice, voice_row = wait_audio(g, voice=True)
    require('16000 Hz mono' in voice_row, 'set has no 16 kHz voice endpoint')
    first = capture_baseline(g.control('status', group='le'))
    mark = g.start('audioprobe radio-stereo')
    g.wait(mark, 'radio-stereo started')
    status = g.eventually('status', lambda state: verify_captures(state, first, {'le-left': 1000, 'le-right': 2000}, 48000), group='le')
    g.wait(mark, 'audioprobe: PASS radio-stereo')
    g.finish(mark)
    g.record('le-stereo', captures=verify_captures(status, first, {'le-left': 1000, 'le-right': 2000}, 48000))
    g.command(f'audioctl volume {device} 80', expect='done')
    g.eventually('status', lambda state: all(state['peers'][name]['volume'] == 204 for name in ('le-left', 'le-right')), group='le')
    g.control('set-volume', group='le', peer='le-left', level=255)
    end = time.monotonic() + 20
    while 'level 100 on the device' not in audio_device(g.command('audioctl devices'))[1]:
        require(time.monotonic() < end, 'earbud-originated volume did not reach AudioService')
        g.pause(.2)
    g.record('le-volume', operator_level=80, peer_level=255)
    g.command(f'audioctl default {voice} voice', expect='done')
    g.control('gtbs-watch', group='le', peer='le-left')
    g.control('source-voice', group='le', peer='le-left', frequency=1500, enabled=True)
    first = capture_baseline(g.control('status', group='le'))
    mark = g.start('audioprobe radio-voice')
    g.wait(mark, 'radio-voice incoming')
    state = g.eventually('status', lambda state: any(call['state'] == 0 for call in state['peers']['le-left']['calls']), group='le')
    call = next(call for call in state['peers']['le-left']['calls'] if call['state'] == 0)
    g.control('gtbs-control', group='le', peer='le-left', control='accept', index=call['index'])
    g.wait(mark, 'radio-voice answered')
    status = g.eventually('status', lambda state: verify_captures(state, first, {'le-left': 333}, 16000), group='le')
    g.pause(1)
    g.control('gtbs-control', group='le', peer='le-left', control='terminate', index=call['index'])
    g.wait(mark, 'audioprobe: PASS radio-voice')
    g.finish(mark)
    g.control('source-voice', group='le', peer='le-left', enabled=False)
    g.record('le-voice', captures=verify_captures(status, first, {'le-left': 333}, 16000))
    # Host source -> real BIG/BIS USB ISO -> guest decoder/routing -> real CIS
    # -> independent liblc3 decoder. A join acknowledgement alone cannot pass.
    for encrypted in (False, True):
        request = {'code': b'IndependentCode!'.hex()} if encrypted else {}
        g.control('broadcast-start', group='le', broadcast_id=0x123456, **request)
        g.command('btctl broadcast scan 5', expect='0x123456', timeout=90)
        first = capture_baseline(g.control('status', group='le'))
        g.command('btctl broadcast play 0x123456' + (' IndependentCode!' if encrypted else ''), expect='joining broadcast')
        status = g.eventually('status', lambda state: verify_captures(state, first, {'le-left': 1500, 'le-right': 3000}, 48000), group='le')
        active = capture_baseline(status)
        observed = domain_snapshot(g)
        continuing = g.eventually('status', lambda state: verify_captures(state, active, {'le-left': 1500, 'le-right': 3000}, 48000), group='le')
        g.record('domain-live-broadcast-' + ('encrypted' if encrypted else 'clear'), **observed,
                 frames_before=active,
                 captures_after=verify_captures(continuing, active, {'le-left': 1500, 'le-right': 3000}, 48000))
        g.command('btctl broadcast stop', expect='the broadcast was stopped')
        g.control('broadcast-stop', group='le')
        g.record('le-broadcast-' + ('encrypted' if encrypted else 'clear'), captures=verify_captures(status, first, {'le-left': 1500, 'le-right': 3000}, 48000), wrong_code='excluded: RootCanal 1.13.0 ignores encryption/code in BIG create sync')
    g.control('depart', group='le', peer='le-right')
    g.eventually('status', lambda state: not state['peers']['le-right']['connections'] and not state['peers']['le-right']['advertising'], group='le')
    require(wait_audio(g)[0] == device, 'one member leaving removed the surviving set')
    g.control('depart', group='le', peer='le-left')
    g.eventually('status', lambda state: not state['peers']['le-left']['connections'] and not state['peers']['le-left']['advertising'], group='le')
    g.command_eventually('audioctl devices', lambda output: f'device {device}:' not in output and f'device {voice}:' not in output)
    g.record('le-departure')
    # Departure was verified while both bonds remained trusted. Release their
    # pending accept-list attempts before the separate own-RPA timer scenario:
    # the controller cannot replace its initiating address during an attempt.
    for peer in ('le-left', 'le-right'):
        g.command(f'btctl untrust {g.address(peer)} audio', expect='is no longer trusted for audio')


def verify_stereo_broadcast(value):
    if value['samples'] < 24000:
        return False
    require(value.get('rate') == 48000 and len(value.get('channels', [])) == 2,
            f'broadcast A2DP decode lost stereo/48k format: {value}')
    return all(row['samples'] >= 24000 and row['rms'] > 100 and row['peak_frequency'] == frequency
               for row, frequency in zip(value['channels'], (1500, 3000)))


def broadcast_to_classic(g):
    # BIS reception does not require pairing. Preserve independent USB ISO +
    # LC3 decoding/routing evidence even when SC earbuds cannot be bonded.
    g.control('disconnect', peer='phone')
    g.command(f'btctl connect {g.address("headset")} audio', expect='connecting audio')
    device, row = wait_audio(g)
    require('48000 Hz stereo' in row, f'BIS route has no stereo A2DP output: {row}')
    g.command(f'audioctl default {device} output', expect='done')
    g.command(f'audioctl volume {device} 100', expect='done')
    for encrypted in (False, True):
        request = {'code': b'IndependentCode!'.hex()} if encrypted else {}
        g.control('broadcast-start', group='le', broadcast_id=0x123456, **request)
        g.command('btctl broadcast scan 5', expect='0x123456', timeout=90)
        g.control('music-reset', peer='headset')
        g.command('btctl broadcast play 0x123456' + (' IndependentCode!' if encrypted else ''), expect='joining broadcast')
        first = g.eventually('music-verdict', verify_stereo_broadcast, peer='headset')
        observed = domain_snapshot(g)
        # Discard captured RTP only, not any transmitted data, then require a
        # fresh decoded half-second on both channels while the route stays live.
        g.control('music-reset', peer='headset')
        continuing = g.eventually('music-verdict', verify_stereo_broadcast, peer='headset')
        label = 'encrypted' if encrypted else 'clear'
        g.record('domain-live-broadcast-to-a2dp-' + label, **observed,
                 capture_before=first, fresh_capture_after=continuing,
                 load='independent BIG -> LC3 decode -> AudioService mix -> SBC/A2DP; not the unavailable simultaneous CIS load')
        g.command('btctl broadcast stop', expect='the broadcast was stopped')
        g.control('broadcast-stop', group='le')
        g.record('le-broadcast-to-a2dp-' + label, independent_decode=continuing,
                 code_scope='correct-code command/transport; RF wrong-code rejection remains in-guest fixture')


def run(g):
    g.prepare_image()
    g.start_peers()
    g.boot('first')
    rest = g.command('btctl list')
    from bluetooth_controller_capabilities import read_capabilities, fixture_fallbacks
    capabilities = read_capabilities((g.work / 'usb.log').read_text())
    le_sc = capabilities['le_secure_connections']
    require(('cannot pair on LE (no LE Secure Connections)' not in controller_line(rest)) == le_sc,
            'guest inventory disagrees with the actual controller command bitmap')
    g.record('controller-capabilities', **capabilities)
    for row in fixture_fallbacks(capabilities):
        g.fixture_required(row)
    require(not any(word in controller_line(rest) for word in ('connectable', 'discoverable', 'pairable')), 'unwatched, untrusted radio is open at rest')
    g.command('btctl discoverable 2', expect='discoverable for 2 s')
    g.pause(3)
    output = g.command('btctl list')
    require('discoverable' not in controller_line(output), 'discoverable lease did not expire')
    from bluetooth_guest_le import ordinary_le, ordinary_le_without_dhkey, privacy_rotation, reuse_tag, forget_tag
    if le_sc:
        ordinary_le(g, pair)
    else:
        ordinary_le_without_dhkey(g, pair)
    from bluetooth_guest_classic import classic_pairing
    classic_pairing(g, pair)
    classic_input(g)
    trust(g, 'serial', 'spp')
    g.command(f'btctl alias {g.address("serial")} serial-1', expect='is "serial-1"')
    g.command('btserial', expect='btserial: PASS')
    require(g.control('status')['serial']['bytes_received'] >= 64000, 'independent RFCOMM peer did not receive the burst')
    g.record('classic-serial')
    trust(g, 'headset', 'audio', 'voice')
    trust(g, 'phone', 'audio', 'pan')
    from bluetooth_guest_classic import music_voice, transfer
    music_voice(g)
    transfer(g)
    if not le_sc:
        broadcast_to_classic(g)
    # Classic outputs must leave before the one-device LE set assertions.
    for peer in ('headset', 'phone'):
        g.control('disconnect', peer=peer)
    g.command_eventually('audioctl devices', lambda output: '(Bluetooth' not in output)
    if le_sc:
        le_audio(g)
    privacy_rotation(g)
    g.record('domain-after-load', **domain_snapshot(g))
    before = g.control('status')['keyboard']['pairings']
    key_path = g.work / 'radio/keyboard-keys.json'
    fingerprint = hashlib.sha256(key_path.read_bytes()).hexdigest()
    bluetooth_epoch = g.stop_service('bluetooth_service')
    g.start_service('bluetooth_service', bluetooth_epoch)
    g.command_eventually('btctl list', lambda output: re.search(r'controller \d+: [0-9a-f:]{17} \S+ - on', output))
    reuse_tag(g, 'restart')
    keyboard(g, 'restart')
    require(g.control('status')['keyboard']['pairings'] == before, 'restart paired keyboard again')
    require(hashlib.sha256(key_path.read_bytes()).hexdigest() == fingerprint, 'keyboard key changed on restart')
    g.poweroff()
    g.boot('second')
    reuse_tag(g, 'cold')
    keyboard(g, 'cold')
    require(g.control('status')['keyboard']['pairings'] == before, 'cold boot paired keyboard again')
    require(hashlib.sha256(key_path.read_bytes()).hexdigest() == fingerprint, 'keyboard key changed across cold boot')
    g.command(f'btctl forget {g.address("keyboard")}', expect='is forgotten')
    g.control('disconnect', peer='keyboard')
    reply = g.control('hid-reconnect', peer='keyboard', address=g.guest_address, allow_error=True)
    require_hid_refusal(reply, forgotten=True)
    forget_tag(g)
    g.command('echo independent-forget-alive', expect='independent-forget-alive')
    g.record('bond-persistence-and-forget', key_file_sha256=fingerprint)
    g.poweroff()
    g.bridge.wait(timeout=10)
    require(g.bridge.returncode == 0, 'USB bridge failed during the two-boot run')
    bridge = (g.work / 'usb.log').read_text()
    counters = re.findall(r'H4 packets (\{[^\n]+\})', bridge)
    require(counters, 'USB bridge did not preserve H4 transport counters')
    counters = ast.literal_eval(counters[-1])
    required_transport = [('out', 1), ('in', 4), ('in', 2), ('out', 2), ('in', 3), ('out', 3), ('in', 5)]
    if le_sc:
        required_transport.append(('out', 5))  # CIS transmit depends on bonding the SC earbuds.
    for direction, kind in required_transport:
        require(counters.get(f'{direction}-{kind}', 0) > 0, f'USB transport never carried {direction} H4 type {kind}')
    g.record('production-usb-hci', packets=counters, required_directions=required_transport,
             outbound_iso='CIS exercised' if le_sc else 'fixture required: SC earbud bonding unavailable')


def image_preparation_self_test():
    from types import SimpleNamespace
    from unittest.mock import patch

    class BeforeGuest(Exception):
        pass

    # Real Gate preparation/boot methods; only external builders/media tools are
    # substituted. These are orchestration tests, not a new image-format oracle.
    for supplied, fault in ((False, None), (True, None), ('relative', None), (False, 'build'),
                            (False, 'missing-built'), (True, 'missing-supplied'),
                            (True, 'extract'), (True, 'copy'), (True, 'missing-disk'),
                            (True, 'existing-disk')):
        with tempfile.TemporaryDirectory(prefix='liber-bt-image-test-') as scratch:
            work = Path(scratch)
            external = work / 'supplied medium.iso'
            if supplied and fault != 'missing-supplied':
                external.write_bytes(b'selected-medium')
            if fault == 'existing-disk':
                (work / 'system.img').write_bytes(b'prior-bonds')
            gate = Gate(work, work)
            commands = []
            def spawn(command, log, env=None):
                commands.append((command, env))
                code = 0
                if command[0] == 'bash' and command[1] == str(ROOT / 'image.sh'):
                    require(env['LIBER_DEVELOPMENT'] == '1', 'image preparation kept an ordinary build profile')
                    code = 1 if fault == 'build' else 0
                    if not code and fault != 'missing-built':
                        Path(env['LIBER_IMAGE_OUTPUT']).write_bytes(b'fresh-development-medium')
                elif command[0] == 'xorriso':
                    require(command[command.index('-indev') + 1] == str(external if supplied else work / 'development.iso'),
                            'volume extraction used an unrelated canonical medium')
                    code = 1 if fault == 'extract' else 0
                    if not code:
                        Path(command[-1]).write_bytes(b'paired-esp')
                elif command[0] == 'mcopy':
                    code = 1 if fault == 'copy' else 0
                    if not code and fault != 'missing-disk':
                        Path(command[-1]).write_bytes(b'volume-from-selected-medium')
                else:
                    require(command[:2] == ['bash', str(HARNESS / 'qemu-run.sh')], 'unexpected preparation tool')
                return SimpleNamespace(wait=lambda: code)
            gate.spawn = spawn
            gate.await_file = lambda *args, **kwargs: (_ for _ in ()).throw(BeforeGuest())
            try:
                supplied_path = os.path.relpath(external, ROOT) if supplied == 'relative' else str(external)
                with patch.dict(os.environ, {'BOOT_IMAGE': supplied_path if supplied else '', 'LIBER_DEVELOPMENT': '0'}):
                    try:
                        gate.prepare_image()
                    except GateError:
                        require(fault is not None and gate.boot_image is None, 'valid medium refused or failed medium committed')
                    else:
                        require(fault is None, f'image preparation accepted failure: {fault}')
                        built = [c for c, _ in commands if c[:2] == ['bash', str(ROOT / 'image.sh')]]
                        require(len(built) == (0 if supplied else 1), 'explicit image rebuilt, or fresh image not built once')
                        initial = gate.results[-1]
                        require(initial['initial_disk']['sha256'] == hashlib.sha256(b'volume-from-selected-medium').hexdigest(),
                                'initial paired-volume identity was not recorded')
                        (work / 'system.img').write_bytes(b'remembered-bonds')
                        prepared_commands = len(commands)
                        for label in ('first', 'cold'):
                            try:
                                gate.boot(label)
                            except BeforeGuest:
                                pass
                            else:
                                raise GateError('mock boot reached the guest')
                            require(commands[-1][1]['BOOT_IMAGE'] == str(gate.boot_image), 'cold boot selected a different medium')
                            require((work / 'system.img').read_bytes() == b'remembered-bonds', 'cold boot erased persistent bonds')
                        require(len(commands) == prepared_commands + 2, 'boot rebuilt the selected image')
                    if fault == 'existing-disk':
                        require((work / 'system.img').read_bytes() == b'prior-bonds', 'preparation overwrote an existing disk')
            finally:
                gate.close()
    note('image preparation self-test PASS: fresh development build, explicit-image authority, exact paired extraction, cold-disk retention and failure refusals')


def hid_readiness_self_test():
    from unittest.mock import patch
    import bluetooth_guest_le

    class Clock:
        now = 0.0
        def monotonic(self):
            return self.now

    class Serial(tickless.Serial):
        def __init__(self, clock):
            self.clock = clock
            self.scale = 1
            self.data = bytearray(b'old output\nvol://system> ')
            self.pending = []
        def pump(self, wait):
            self.clock.now += wait
            if self.pending:
                self.data.extend(self.pending.pop(0))
                return True
            return False
        def type(self, _data):
            raise GateError('HID readiness attempted a UART write')

    # Actual Gate readiness, actual HOGP/Classic acceptance functions, fake
    # peer/serial only. The clock advances without sleeping or starting a guest.
    for mode in ('absent', 'stale', 'partial', 'fresh', 'delayed', 'partial-acceptance'):
        for profile in ('le-generic', 'classic'):
            with tempfile.TemporaryDirectory(prefix='bluetooth-hid-ready-') as scratch:
                clock = Clock()
                gate = Gate.__new__(Gate)
                gate.work = Path(scratch)
                gate.serial = Serial(clock)
                if mode == 'absent':
                    gate.serial.data.clear()
                gate.results = []
                gate.require = require
                sent = []
                attempts = 0
                row = dict(connections=[dict(encrypted=True, hid_notify=True)],
                           hid_reads=14, pairings=2, encrypted=1)
                def control(action, *, peer='keyboard', group='classic', **values):
                    nonlocal attempts
                    if action == 'status':
                        return {'keyboard': row, 'tag': row}
                    if action in ('disconnect', 'hid-reconnect'):
                        return {}
                    require(action == 'type', 'unexpected HID readiness mock action')
                    text = values['text']
                    sent.append((group, peer, text))
                    if text == '\n':
                        attempts += 1
                        if mode == 'stale':
                            gate.serial.pending.append(b'vol://system> ')
                        elif mode == 'partial':
                            gate.serial.pending.append(b'\nvol://system>')
                        elif mode in ('fresh', 'partial-acceptance') or (mode == 'delayed' and attempts >= 3):
                            gate.serial.pending.extend([b'\r\n\x1b[1;32mvol://sys', b'tem> \x1b[0m'])
                    else:
                        require(text.startswith('echo independent-') and text.endswith('\n'), 'acceptance text changed')
                        output = text[5:].encode() if mode != 'partial-acceptance' else b'unknown command\n'
                        gate.serial.pending.append(text.encode() + output + b'vol://system> ')
                    return row
                gate.control = control
                gate.guest_address = 'host'
                gate.eventually = lambda action, predicate, **values: {'keyboard': row, 'tag': row}
                with patch.object(time, 'monotonic', clock.monotonic):
                    try:
                        if profile == 'le-generic':
                            bluetooth_guest_le.type_tag(gate, 'self-test')
                        else:
                            keyboard(gate, 'self-test')
                    except GateError:
                        require(mode in ('absent', 'stale', 'partial', 'partial-acceptance'), 'ready HID path was refused')
                    else:
                        require(mode in ('fresh', 'delayed'), 'invalid HID readiness/acceptance was accepted')
                accepted = [text for _, _, text in sent if text != '\n']
                require(len(accepted) == (1 if mode in ('fresh', 'delayed', 'partial-acceptance') else 0),
                        'acceptance text ran before readiness or was retried')
                expected_peer = 'tag' if profile == 'le-generic' else 'keyboard'
                require(all(group == profile and peer == expected_peer for group, peer, _ in sent),
                        'readiness and acceptance used different HID peers')
                rows = [json.loads(line) for line in (gate.work / 'hid-readiness.jsonl').read_text().splitlines()]
                require(len(rows) == attempts * 2 and all(row['text'] == '\n' for row in rows),
                        'not every readiness attempt was retained')
                if mode in ('absent', 'stale', 'partial'):
                    require(not any(row['status'] == 'ready' for row in rows) and clock.now <= 11,
                            'missing consumer/stale prompt escaped the readiness bound')
                if mode == 'delayed':
                    require(attempts == 3, 'delayed adoption was not actually observed')
    note('HID readiness self-test PASS: stale/partial/absent refused, fresh/delayed adopted, original acceptance once on LE and Classic')


def classic_p192_self_test():
    from unittest.mock import patch

    # Run the exact scenario call and actual pair() helper. Only the serial and
    # peer-control boundaries are mocked; this does not execute the radio model.
    source = ast.parse((HARNESS / 'bluetooth_guest_classic.py').read_text())
    scenario = next(node for node in source.body if isinstance(node, ast.FunctionDef) and node.name == 'classic_pairing')
    calls = [node for node in ast.walk(scenario) if isinstance(node, ast.Call)
             and isinstance(node.func, ast.Name) and node.func.id == 'pair'
             and len(node.args) > 1 and isinstance(node.args[1], ast.Constant) and node.args[1].value == 'gamepad']
    require(len(calls) == 1, 'gamepad P192 scenario is absent or ambiguous')
    command = compile(ast.Expression(calls[0]), '<actual-gamepad-pair-call>', 'eval')

    class PeerSerial:
        def __init__(self, owner):
            self.owner = owner
            self.writes = []
            self.reads = 0
        def text_since(self, mark):
            require(mark == 17, 'P192 prompt cursor changed')
            self.reads += 1
            if self.owner.mode == 'no-comparison':
                return b'bonded: P-192 Simple Pairing, authenticated\n'
            if self.owner.mode == 'refused':
                return b'btctl: the pairing failed\n'
            # Matches the observed DisplayOnly run and the controller's documented
            # DisplayYesNo/DisplayOnly association; no cryptography is simulated.
            if self.owner.mode == 'just-works' or self.owner.io == 'DISPLAY_OUTPUT_ONLY':
                return b'bonded: P-192 Simple Pairing, Just Works\n'
            if self.reads <= 2:
                return b'Does it match 000027? [y/n]'
            return b'bonded: P-192 Simple Pairing, authenticated\n'
        def type(self, data):
            self.writes.append(data)

    class Peer:
        require = staticmethod(require)
        def __init__(self, mode):
            self.mode, self.io, self.now = mode, None, 0.0
            self.serial = PeerSerial(self)
            self.rows, self.event_calls = [], []
        def address(self, peer):
            require(peer == 'gamepad', 'unexpected P192 peer')
            return 'da:4c:10:de:00:01'
        def control(self, action, **values):
            require(action == 'pairing-config' and values['peer'] == 'gamepad'
                    and values['group'] == 'classic' and values['sc'] is False
                    and values['mitm'] is True and values['ssp'] is True,
                    'P192 authentication configuration weakened')
            self.io = values['io']
        def command(self, value, **_kwargs):
            require(value == 'btctl scan 5', 'unexpected P192 discovery command')
            return self.address('gamepad')
        def events(self):
            return {'next': 43}
        def start(self, value):
            require(value == 'btctl pair da:4c:10:de:00:01 bredr', 'P192 used legacy or a different address')
            return 17
        def event(self, event, peer, **values):
            require(event == 'pairing-number' and peer == 'gamepad' and values == {'since': 43},
                    'P192 comparison did not use a fresh peer event')
            self.event_calls.append((event, peer, values))
            if self.mode == 'missing-peer-number':
                raise GateError('independent peer comparison number absent')
            return {'number': 28 if self.mode == 'mismatch' else 27}
        def pause(self, seconds):
            self.now += seconds
            require(self.now < 1, 'mock P192 transcript did not terminate')
        def finish(self, mark):
            return self.serial.text_since(mark).decode()
        def record(self, scenario, **values):
            self.rows.append(dict(scenario=scenario, **values))

    reasons = {
        'mismatch': 'Numeric Comparison numbers differ',
        'missing-peer-number': 'independent peer comparison number absent',
        'no-comparison': 'gamepad did not complete required compare prompt',
        'just-works': 'gamepad did not earn P-192 Simple Pairing, authenticated',
        'refused': 'gamepad did not earn P-192 Simple Pairing, authenticated',
    }
    for mode in ('matching', *reasons):
        guest = Peer(mode)
        with patch.object(time, 'monotonic', lambda: guest.now):
            try:
                eval(command, {'pair': pair, 'g': guest})
            except GateError as error:
                require(mode in reasons and reasons[mode] in str(error),
                        f'P192 {mode} failed for an unexpected cause: {error}')
                require(not guest.serial.writes and not guest.rows, 'refused P192 transcript was confirmed or recorded as passing')
            else:
                require(mode == 'matching', f'P192 {mode} transcript was falsely accepted')
                require(guest.io == 'DISPLAY_OUTPUT_AND_YES_NO_INPUT', 'P192 peer cannot confirm the displayed number')
                require(guest.serial.writes == [b'y\n'] and len(guest.event_calls) == 1,
                        'P192 Numeric Comparison was missing, duplicated or not independently matched')
                require(guest.rows == [dict(scenario='pair-gamepad', level='P-192 Simple Pairing, authenticated', prompts=['compare'])],
                        'P192 comparison proof was not retained')
    note('P192 pairing self-test PASS: six actual-call transcripts; matching fresh numbers confirmed once, mismatch/missing number/missing prompt/Just Works/refusal rejected')


def self_test():
    classic_p192_self_test()
    hid_readiness_self_test()
    from bluetooth_le_reconnect import run as le_reconnect_self_test
    le_reconnect_self_test(ROOT)
    from bluetooth_le_audio_identity import run as le_audio_identity_self_test
    le_audio_identity_self_test(ROOT)
    image_preparation_self_test()
    from bluetooth_audio_lifecycle import run as audio_lifecycle_self_test
    audio_lifecycle_self_test(ROOT)
    from bluetooth_controller_capabilities import self_test as capability_self_test
    capability_self_test()
    from types import SimpleNamespace
    accounting = 'bluetooth_service: memory=1212416/67108864 handles=21/256 threads=1/4 ipc-queue=1200/4194304 dma=0/0 stack=131072/2097152\n'
    def observer(usage=accounting, graph='graph bluetooth_service\nname=bluetooth_service state=running\n'):
        return SimpleNamespace(command=lambda command: graph if command.startswith('graph ') else usage)
    require(domain_snapshot(observer())['bluetooth_resources']['memory']['used'] == '1212416', 'domain sample parser lost the real resource value')
    for observed in (observer('usage: service unavailable\n'), observer(accounting.replace('1212416/67108864', '67108865/67108864')),
                     observer(accounting.replace('stack=131072/2097152', '')), observer(graph='graph bluetooth_service\ngraph: no such component\n')):
        try:
            domain_snapshot(observed)
        except GateError:
            continue
        raise GateError('missing/failed/incomplete/over-limit live accounting accepted')
    refused = dict(ok=False, error_namespace='L2CAP', error_code=3)
    require_hid_refusal(refused)
    require_hid_refusal(refused, forgotten=True)
    for code in (0x05, 0x06, 0x0E, 0x0F, 0x18, 0x2F):
        require_hid_refusal(dict(ok=False, error_namespace='hci', error_code=code), forgotten=True)
    negatives = [dict(ok=False, error='TimeoutError()'), dict(ok=False, error='KeyError()'),
                 dict(refused, ok=True), dict(refused, error_code='3'),
                 dict(refused, error_code=0), dict(refused, error_code=2),
                 dict(refused, error_code=4), dict(refused, error_namespace='rfcomm'),
                 dict(ok=False, error_namespace='hci', error_code=0x04),  # Page timeout.
                 dict(ok=False, error_namespace='hci', error_code=0x08),  # Link timeout.
                 dict(ok=False, error_namespace='hci', error_code=0x0D),  # Resources.
                 dict(ok=False, error_namespace='hci', error_code=0x13)]  # Normal disconnect.
    for forgotten in (False, True):
        for reply in negatives:
            try:
                require_hid_refusal(reply, forgotten=forgotten)
            except GateError:
                continue
            raise GateError(f'non-security HID failure accepted: {reply}')
    try:
        require_hid_refusal(dict(ok=False, error_namespace='hci', error_code=0x05))
    except GateError:
        pass
    else:
        raise GateError('untrusted HID oracle accepted link failure instead of L2CAP security block')
    row = dict(peer='le-left', rate=48000, frames=25, duration_us=10000, lc3='capture-a.lc3', lost=0, sequence_gaps=0, frequency_hz=1000, rms=500, octets=120, location=1)
    status = dict(errors=[], captures=[row])
    require(verify_captures(status, {}, {'le-left': 1000}, 48000), 'valid independent capture refused')
    baseline = capture_baseline(status)
    require(verify_captures(status, baseline, {'le-left': 1000}, 48000) is None, 'stale capture accepted')
    reused = dict(errors=[], captures=[dict(row, frames=125)])
    require(verify_captures(reused, baseline, {'le-left': 1000}, 48000), 'fresh audio on a persistent CIS refused')
    reused['captures'][0]['frames'] = 124
    require(verify_captures(reused, baseline, {'le-left': 1000}, 48000) is None, 'partly stale PCM window accepted')
    for field, value in [('frames', 24), ('frequency_hz', 333), ('rms', 0), ('rate', 16000)]:
        require(verify_captures(dict(errors=[], captures=[dict(row, **{field: value})]), {}, {'le-left': 1000}, 48000) is None, f'bad {field} accepted')
    for field, value in [('lost', 1), ('sequence_gaps', 1), ('location', 2), ('octets', 40)]:
        try:
            verify_captures(dict(errors=[], captures=[dict(row, **{field: value})]), {}, {'le-left': 1000}, 48000)
        except GateError:
            continue
        raise GateError(f'bad {field} accepted')
    require(len(b'IndependentCode!') == 16, 'broadcast code length')
    require(audio_device('device 7: set (Bluetooth) - out 48000 Hz stereo')[0] == 7, 'audio inventory oracle')
    note('oracle self-test PASS: exact HID security refusals; new decoded capture accepted; stale, silent, wrong tone/format/channel, short/lost/gapped captures refused')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--self-test', action='store_true')
    parser.add_argument('--work', type=Path, help='new retained artifact directory')
    args = parser.parse_args()
    def interrupted(number, _frame):
        raise GateError(f'interrupted by {signal.Signals(number).name}')
    for number in (signal.SIGINT, signal.SIGTERM):
        signal.signal(number, interrupted)
    try:
        self_test()
        if args.self_test:
            return 0
        work = (args.work or ROOT / '.build/logs/bluetooth-independent' / time.strftime('%Y%m%d-%H%M%S', time.gmtime())).resolve()
        work.mkdir(parents=True, exist_ok=False)
        note(f'artifacts: {work}')
        with tempfile.TemporaryDirectory(prefix='liber-radio-') as sockets:
            gate = Gate(work, Path(sockets))
            try:
                run(gate)
            finally:
                gate.close()
        note(f'PASS supported independent scenarios, production USB transport and bond persistence; {len(gate.fixture_requirements)} explicit per-feature fixture requirements remain in results.json (no all-profile independent PASS)')
        return 0
    except (GateError, OSError, ValueError, subprocess.TimeoutExpired) as error:
        print(f'bluetooth-independent: FAIL - {error}', file=sys.stderr, flush=True)
        return 1


if __name__ == '__main__':
    sys.exit(main())
