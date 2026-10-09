#!/usr/bin/env python3
"""Production USB Bluetooth interoperability with pinned RootCanal/Bumble peers.

The host controls peer actions, never guest packets or guest verdicts. Independent
codec readers check the actual over-the-radio payloads. BNEP/OPP adapters are
explicitly same-team peers; DHCP and IP answers come from dnsmasq/Linux.
Existing in-guest fixtures retain malformed-packet and resource-exhaustion cases.
Run after the shared development build/image; this gate never builds the guest.
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
        before = self.service_status(name, 'ready', 'running')
        output = self.command(f'stop {name}')
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
        started = self.service_status(name, 'ready', 'running')
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

    def boot(self, label):
        serial_path = self.sockets / 'serial'
        serial_path.unlink(missing_ok=True)
        env = dict(os.environ, LIBER_DEVELOPMENT='1', LIBER_RUN_MODE='development', DEV_PROFILE='1', COLD='0', UEFI='1', SMP='2', LIBER_DEV_STATE=str(self.sockets), SERIAL=f'unix:{serial_path},server=on,wait=off', USB_REDIR_SOCKET=str(self.sockets / 'usb'), RUN_DISK=str(self.work / 'system.img'))
        for name in ('TEST', 'QEMU_EXTRA', 'NET_NONE', 'BT_FIXTURE', 'IDLE_FIXTURE', 'SYSTEM_SUSPEND_FIXTURE'):
            env.pop(name, None)
        kernel = ROOT / '.build/cargo/kernel/x86_64-unknown-none/debug/kernel'
        require(kernel.exists(), 'missing development kernel: run the shared development build/image first')
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


def pair(g, peer, level, *, kind='bredr', io=None, sc=True, mitm=True, legacy=False, group='classic', address=None):
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


def run(g):
    g.start_peers()
    g.boot('first')
    rest = g.command('btctl list')
    require(not any(word in controller_line(rest) for word in ('connectable', 'discoverable', 'pairable')), 'unwatched, untrusted radio is open at rest')
    g.command('btctl discoverable 2', expect='discoverable for 2 s')
    g.pause(3)
    output = g.command('btctl list')
    require('discoverable' not in controller_line(output), 'discoverable lease did not expire')
    from bluetooth_guest_le import ordinary_le, privacy_rotation, reuse_tag, forget_tag
    ordinary_le(g, pair)
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
    # Classic outputs must leave before the one-device LE set assertions.
    for peer in ('headset', 'phone'):
        g.control('disconnect', peer=peer)
    g.command_eventually('audioctl devices', lambda output: '(Bluetooth' not in output)
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
    for direction, kind in [('out', 1), ('in', 4), ('in', 2), ('out', 2), ('in', 3), ('out', 3), ('in', 5), ('out', 5)]:
        require(counters.get(f'{direction}-{kind}', 0) > 0, f'USB transport never carried {direction} H4 type {kind}')
    g.record('production-usb-hci', packets=counters)


def self_test():
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
        note('PASS independent classic/LE profiles, production USB transport, stored bonds across service restart and cold reboot')
        return 0
    except (GateError, OSError, ValueError, subprocess.TimeoutExpired) as error:
        print(f'bluetooth-independent: FAIL - {error}', file=sys.stderr, flush=True)
        return 1


if __name__ == '__main__':
    sys.exit(main())
