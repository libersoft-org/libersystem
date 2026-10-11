#!/usr/bin/env python3
"""Short real-RootCanal USB-redirection framing/reconnect check; no guest claims."""
import argparse
import hashlib
import importlib.util
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
from unittest import mock

import usbredir_device as usb


def self_test(root, bridge_module=usb, keep=None, expect_missing_wideband=False):
    """Actual USB descriptors and guest parser; socketpair replaces only HCI connection.

    This is a local bridge/driver contract proof, without RootCanal, a USB host
    controller, guest scheduling or a radio. Existing BtSco remains narrowband.
    """
    root = Path(root).resolve()
    host, peer = socket.socketpair()
    peer.settimeout(1)
    try:
        with mock.patch.object(bridge_module.socket, 'create_connection', return_value=host):
            bridge = bridge_module.BtBridge('local-test:1')
        narrowband = bridge_module.BtSco().configuration_descriptor()
        descriptor = bridge.configuration_descriptor()
        modules = ('descriptor', 'usb_function', 'bt_usb')
        paths = {name: root / f'src/user/drivers/core/src/{name}.rs' for name in modules}
        imports = '\n'.join(f'#[path = {json.dumps(str(path))}] mod {name};' for name, path in paths.items())
        rust = r'''#![allow(dead_code)]
extern crate alloc;
/* MODULES */

fn packet(length: u8, seed: u8) -> Vec<u8> {
    [0x21, 0, length].into_iter().chain((0..length).map(|n| n.wrapping_mul(7) ^ seed)).collect()
}

fn main() {
    let paths: Vec<_> = std::env::args().skip(1).collect();
    let narrow = bt_usb::bind(&std::fs::read(&paths[1]).unwrap()).expect("actual BtSco descriptor binds");
    assert_eq!(narrow.voice_at(6), None, "existing BtSco still has no wideband alternate");
    let bound = bt_usb::bind(&std::fs::read(&paths[0]).unwrap()).expect("actual BtBridge descriptor binds");
    assert_eq!((bound.events.address, bound.acl_in.address, bound.acl_out.address), (0x81, 0x82, 0x02));
    assert_eq!(bound.voice_interface, Some(1));
    assert_eq!(bound.voice_at(0), None, "zero bandwidth has no active voice pair");
    for alternate in 1..=5 {
        assert_eq!(bound.voice_at(alternate), narrow.voice_at(alternate), "legacy alternates unchanged");
    }
    let alternate = bt_usb::alternate_for(1, 16, true).expect("wideband table row");
    assert_eq!(alternate, 6);
    let voice = bound.voice_at(alternate).expect("BT_BRIDGE_MISSING_WIDEBAND_ALTERNATE_6");
    assert_eq!((voice.iso_in.address, voice.iso_out.address), (0x83, 0x03));
    assert_eq!((voice.iso_in.max_packet(), voice.iso_out.max_packet()), (63, 63));
    assert_eq!((voice.iso_in.transfer(), voice.iso_out.transfer()), (1, 1));
    assert_eq!((voice.iso_in.interval, voice.iso_out.interval), (1, 1));
    let capacity = voice.iso_in.max_packet() as usize;
    let mut pieces = bt_usb::ScoPieces::new(capacity);
    for length in [60, 120, 180, 255] {
        let frame = packet(length, length);
        let mut got = Vec::new();
        for part in frame.chunks(capacity) {
            got.extend(pieces.piece(part));
            assert!(pieces.piece(&[]).is_empty());
        }
        assert_eq!(got, vec![frame], "63-byte service intervals retain complete SCO packets");
    }
    let torn = packet(180, 2);
    let tail = packet(60, 3);
    let mut chunks = torn.chunks(capacity);
    assert!(pieces.piece(chunks.next().unwrap()).is_empty());
    chunks.next().unwrap();
    pieces.lost();
    assert!(pieces.piece(chunks.next().unwrap()).is_empty(), "lost middle discards trailing piece");
    assert_eq!(pieces.piece(&tail), vec![tail.clone()]);
    pieces.lost();
    for part in torn.chunks(capacity).skip(1) {
        assert!(pieces.piece(part).is_empty(), "lost header does not leak a torn packet");
    }
    assert_eq!(pieces.piece(&tail), vec![tail.clone()]);
    assert!(pieces.piece(&torn[..capacity]).is_empty());
    pieces.lost();
    pieces.clear();
    assert_eq!(pieces.piece(&tail), vec![tail.clone()], "stop clears pending loss skip before reopen");
    assert!(pieces.piece(&torn[..capacity]).is_empty());
    pieces.clear();
    assert_eq!(pieces.piece(&tail), vec![tail], "stop clears partial packet before reopen");
    println!("PASS production bind/alternate6/63-byte SCO, torn pieces and clear/reopen");
}
'''.replace('/* MODULES */', imports)
        with tempfile.TemporaryDirectory(prefix='bluetooth-bridge-self-test-') as folder:
            work = Path(folder)
            rust_path, binary = work / 'bridge.rs', work / 'bridge'
            bridge_path, narrow_path = work / 'bridge.bin', work / 'bt-sco.bin'
            rust_path.write_text(rust)
            bridge_path.write_bytes(descriptor)
            narrow_path.write_bytes(narrowband)
            command = [os.environ.get('RUSTC', 'rustc'), '--edition=2024', '--crate-name', 'bluetooth_bridge_contract', str(rust_path), '-o', str(binary)]
            built = subprocess.run(command, capture_output=True, text=True, timeout=120)
            tested = subprocess.run([str(binary), str(bridge_path), str(narrow_path)], capture_output=True, text=True, timeout=15) if built.returncode == 0 else None
            if keep is not None:
                target = Path(keep)
                target.mkdir(parents=True, exist_ok=True)
                (target / 'bridge.rs').write_text(rust)
                (target / 'bridge-descriptor.bin').write_bytes(descriptor)
                (target / 'bt-sco-descriptor.bin').write_bytes(narrowband)
                (target / 'production-inputs.json').write_text(json.dumps({str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in paths.values()}, indent=2) + '\n')
                (target / 'rust.log').write_text(f'compile: {command!r}\nexit: {built.returncode}\n{built.stdout}{built.stderr}\n' + (f'run exit: {tested.returncode}\n{tested.stdout}{tested.stderr}' if tested is not None else 'NOT RUN: compilation failed\n'))
            assert built.returncode == 0, f'bridge production contract did not compile:\n{built.stderr}'
            if expect_missing_wideband:
                assert tested.returncode != 0 and 'BT_BRIDGE_MISSING_WIDEBAND_ALTERNATE_6' in tested.stderr, tested
                print('PASS causal old-descriptor rejection: production bind has no wideband alternate 6', flush=True)
                return
            assert tested.returncode == 0, f'bridge production contract failed:\n{tested.stdout}{tested.stderr}'
        assert bridge_module.BtSco.SIZES == (0, 9, 17, 25, 33, 49)
        assert bridge.has_setting(1, 6) and bridge.has_setting(1, 0)
        bridge.configuration = 1
        bridge.alternates[1] = 6
        bridge.on_alternate(1, 6)
        packet = struct.pack('<HB', 0x21, 180) + bytes(range(180))
        peer.sendall(b'\x03' + packet[:1])
        bridge.poll()
        assert not bridge.sco_back, 'partial H4 header must wait'
        peer.sendall(packet[1:])
        bridge.poll()
        incoming = [bridge.iso_in(0x83) for _ in range(3)]
        assert list(map(len, incoming)) == [63, 63, 57] and b''.join(incoming) == packet
        assert bridge.iso_in(0x83) == b'', 'no duplicate HCI SCO data'
        for piece in incoming:
            bridge.iso_out(0x03, piece)

        def exact(size):
            result = b''
            while len(result) < size:
                part = peer.recv(size - len(result))
                assert part, 'bridge HCI connection closed'
                result += part
            return result

        assert exact(1 + len(packet)) == b'\x03' + packet
        bridge.iso_out(0x03, packet[:30])
        peer.sendall(b'\x03' + packet)
        bridge.poll()
        assert bridge.sco_held and bridge.sco_back
        bridge.alternates[1] = 0
        bridge.on_alternate(1, 0)
        assert not bridge.sco_held and not bridge.sco_back, 'alternate zero clears partial outbound and queued inbound SCO'
        peer.sendall(b'\x03' + packet)
        bridge.poll()
        assert bridge.iso_in(0x83) == b'', 'alternate zero carries no inbound SCO'
        bridge.alternates[1] = 6
        bridge.on_alternate(1, 6)
        fresh = struct.pack('<HB', 0x21, 60) + bytes(range(60))
        peer.sendall(b'\x03' + fresh)
        bridge.poll()
        assert bridge.iso_in(0x83) == fresh and bridge.iso_in(0x83) == b''
        bridge.iso_out(0x03, fresh)
        assert exact(1 + len(fresh)) == b'\x03' + fresh, 'reopen forwards only new complete SCO data'
        bridge.iso_out(0x03, packet[:30])
        peer.sendall(b'\x03' + fresh)
        bridge.poll()
        bridge.on_reset()
        assert not bridge.sco_held and not bridge.sco_back, 'reset retires voice fragments and queued SCO'
        peer.sendall(b'\x03' + fresh)
        bridge.poll()
        assert bridge.iso_in(0x83) == fresh, 'complete HCI SCO resumes after reset'
        peer.sendall(b'\xff')
        try:
            bridge.poll()
        except OSError as error:
            assert 'invalid incoming H4 type 255' in str(error), error
        else:
            raise AssertionError('invalid H4 type was silently accepted')
        peer.close()
        try:
            bridge.poll()
        except OSError as error:
            assert 'RootCanal closed HCI' in str(error), error
        else:
            raise AssertionError('HCI EOF was silently accepted')
        print('PASS bridge wideband descriptor/production parser/SCO reassembly and actual H4 socketpair transport, alternate zero/reopen; BtSco unchanged; no live radio claim', flush=True)
    finally:
        host.close()
        peer.close()


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
    parser.add_argument('--work', type=Path)
    parser.add_argument('--self-test', action='store_true')
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--keep', type=Path)
    parser.add_argument('--expect-missing-wideband', action='store_true')
    parser.add_argument('--bridge-source', type=Path)
    args = parser.parse_args()
    if args.self_test:
        module = usb
        if args.bridge_source is not None:
            spec = importlib.util.spec_from_file_location('bluetooth_bridge_contract_source', args.bridge_source.resolve())
            module = importlib.util.module_from_spec(spec)
            sys.modules[spec.name] = module
            spec.loader.exec_module(module)
        self_test(args.root, bridge_module=module, keep=args.keep, expect_missing_wideband=args.expect_missing_wideband)
    elif args.work is not None:
        if args.expect_missing_wideband or args.bridge_source is not None:
            parser.error('--expect-missing-wideband and --bridge-source require --self-test')
        run(args.work.resolve())
    else:
        parser.error('one of --self-test or --work is required')
