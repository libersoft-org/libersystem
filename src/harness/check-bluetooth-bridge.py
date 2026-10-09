#!/usr/bin/env python3
"""Short real-RootCanal USB-redirection framing/reconnect check; no guest claims."""
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time

import usbredir_device as usb


def receive(sock):
    def exact(size):
        result = b''
        while len(result) < size:
            piece = sock.recv(size - len(result))
            if not piece:
                raise EOFError('USB redirection peer closed')
            result += piece
        return result
    kind, size, identifier = struct.unpack('<III', exact(12))
    return kind, identifier, exact(size)


def send(sock, kind, body=b'', identifier=0):
    sock.sendall(struct.pack('<III', kind, len(body), identifier) + body)


def controller_address(path):
    with socket.socket(socket.AF_UNIX) as sock:
        sock.settimeout(5)
        sock.connect(str(path))
        assert receive(sock)[0] == usb.HELLO
        send(sock, usb.HELLO, b'independent bridge check'.ljust(64, b'\0') + struct.pack('<I', 0))
        while receive(sock)[0] != usb.DEVICE_CONNECT:
            pass
        send(sock, usb.SET_CONFIGURATION, b'\1')
        send(sock, usb.START_INTERRUPT_RECEIVING, b'\x81')
        event_bytes = bytearray()
        address = None
        for opcode in (0x0c03, 0x1009):
            command = struct.pack('<HB', opcode, 0)
            send(sock, usb.CONTROL_PACKET, struct.pack('<BBBBHHH', 0, 0, 0x20, 0, 0, 0, len(command)) + command, opcode)
            while True:
                kind, identifier, body = receive(sock)
                if kind == usb.CONTROL_PACKET:
                    assert body[3] == usb.SUCCESS, 'USB HCI command stalled'
                elif kind == usb.INTERRUPT_PACKET:
                    endpoint, status, size = struct.unpack_from('<BBH', body)
                    assert endpoint == 0x81 and status == 0 and size == len(body) - 4
                    event_bytes.extend(body[4:])
                    if len(event_bytes) < 2 or len(event_bytes) < event_bytes[1] + 2:
                        continue
                    event = bytes(event_bytes[:event_bytes[1] + 2])
                    del event_bytes[:len(event)]
                    assert event[0] == 0x0e and struct.unpack_from('<H', event, 3)[0] == opcode and event[5] == 0, event.hex()
                    if opcode == 0x1009:
                        address = event[6:12][::-1].hex(':')
                    break
        return address


def run(work):
    work.mkdir(parents=True, exist_ok=True)
    ports = []
    for _ in range(4):
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            ports.append(sock.getsockname()[1])
    controller_log = (work / 'controller.log').open('w')
    bridge_log = (work / 'bridge.log').open('w')
    controller = subprocess.Popen([sys.executable, '-m', 'rootcanal', *(f'--{name}_port={port}' for name, port in zip(('test', 'hci', 'link', 'link_ble'), ports))], stdout=controller_log, stderr=subprocess.STDOUT, start_new_session=True)
    bridge = None
    sockets = tempfile.TemporaryDirectory(prefix='liber-bt-usb-')
    try:
        for _ in range(100):
            if f'Listening on: {ports[1]} ' in (work / 'controller.log').read_text(errors='replace'):
                break
            assert controller.poll() is None, 'RootCanal exited during startup'
            time.sleep(.02)
        else:
            raise TimeoutError('RootCanal did not start')
        path, ready = Path(sockets.name) / 'usb.sock', work / 'usb.ready'
        path.unlink(missing_ok=True)
        ready.unlink(missing_ok=True)
        bridge = subprocess.Popen([sys.executable, str(Path(__file__).with_name('usbredir_device.py')), '--emulate', 'bt-bridge', '--hci', f'127.0.0.1:{ports[1]}', '--connections', '2', '--socket', str(path), '--ready', str(ready)], stdout=bridge_log, stderr=subprocess.STDOUT, start_new_session=True)
        for _ in range(100):
            if ready.exists():
                break
            assert bridge.poll() is None, 'bridge exited during startup'
            time.sleep(.02)
        else:
            raise TimeoutError('bridge did not start')
        addresses = [controller_address(path), controller_address(path)]
        assert addresses[0] and addresses[0] == addresses[1], addresses
        assert bridge.wait(timeout=5) == 0
        result = dict(usb_connections=2, commands_per_connection=['Reset', 'Read_BD_ADDR'], addresses=addresses, guest=False)
        (work / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print('PASS real RootCanal through USB redirection; stable address across reconnect:', addresses[0])
    finally:
        for process in (bridge, controller):
            if process and process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
        controller_log.close()
        bridge_log.close()
        sockets.cleanup()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--work', type=Path, required=True)
    run(parser.parse_args().work.resolve())
