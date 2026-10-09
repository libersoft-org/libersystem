#!/usr/bin/env python3
"""Pinned independent Bluetooth peers; JSON line control for the QEMU gate.

Run with .build/bluetooth-oracles/venv/bin/python after the explicit preparer.
RootCanal and Bumble run only on the host. Commands and observations go to a
private Unix socket; the guest sees only the production USB HCI transport.
"""
import argparse
import asyncio
import contextlib
import json
import os
from pathlib import Path
import signal
import socket
import sys
import time

from bumble import core
from bluetooth_classic_peers import AudioPeer, Peer, create_classic_peers


def control_error(error):
    reply = dict(ok=False, error=repr(error), error_type=type(error).__name__)
    # BaseError.__repr__ loses the protocol fields. Keep their actual values so
    # a transport timeout cannot masquerade as the guest's security refusal.
    if isinstance(error, core.BaseError):
        reply.update(error_namespace=error.error_namespace,
                     error_code=error.error_code, error_name=error.error_name)
    return reply


async def self_test():
    from types import SimpleNamespace
    from bumble import hci, hfp, l2cap

    refusal = l2cap.L2CAP_Connection_Response.Result.CONNECTION_REFUSED_SECURITY_BLOCK
    reply = control_error(l2cap.L2capError(refusal, refusal.name))
    assert reply['error_namespace'] == 'L2CAP' and reply['error_code'] == 3
    assert reply['error_name'] == 'CONNECTION_REFUSED_SECURITY_BLOCK'
    reply = control_error(hci.HCI_Error(hci.HCI_PIN_OR_KEY_MISSING_ERROR))
    assert reply['error_namespace'] == 'hci' and reply['error_code'] == 6
    assert reply['error_name'] == 'PIN_OR_KEY_MISSING_ERROR'
    for error in (TimeoutError(), core.TimeoutError(), KeyError('peer')):
        assert 'error_code' not in control_error(error)
    connection = SimpleNamespace(peer_address=hci.Address('DA:4C:10:DE:00:00', hci.Address.PUBLIC_DEVICE_ADDRESS), transport=core.PhysicalTransport.BR_EDR)
    peer = object.__new__(Peer)
    peer.connections = {1: connection}
    # No device exists on this test object: a mistaken second connect must fail.
    assert await peer.connection('da:4c:10:de:00:00') is connection
    assert await peer.connection('DA:4C:10:DE:00:00') is connection
    # Pinned Bumble parses the actual AG bytes and the production fixture
    # adapters must preserve the distinct event names and numeric values.
    audio = object.__new__(AudioPeer)
    audio.hf_configuration = hfp.HfConfiguration([], [], [hfp.AudioCodec.CVSD])
    observed = []
    audio.observe = lambda event, **fields: observed.append(dict(event=event, **fields))
    link = SimpleNamespace(on=lambda *args: None)
    channel = SimpleNamespace(on=lambda *args: None, EVENT_CLOSE='close', connection=link)
    dlc = SimpleNamespace(multiplexer=SimpleNamespace(l2cap_channel=channel))
    audio.hfp_connected(dlc)
    dlc.sink(b'\r\n+VGM: 6\r\n\r\n+VGS: 9\r\n')
    await audio.hf.handle_unsolicited()
    await audio.hf.handle_unsolicited()
    assert observed == [dict(event='hfp-microphone-volume', value=6), dict(event='hfp-volume', value=9)]
    # A held acceptance retains one actual connection object, not merely a reusable HCI handle.
    # The release constructs Bumble's actual command; holding must transmit no command at all.
    sent = []
    async def send(command):
        sent.append(command)
    audio.device = SimpleNamespace(send_async_command=send)
    audio.tasks = set()
    connection = SimpleNamespace(handle=7, peer_address=hci.Address('DA:4C:10:DE:00:00', hci.Address.PUBLIC_DEVICE_ADDRESS))
    audio.connections = {connection.handle: connection}
    audio.hold_sco_accept = True
    audio.held_sco_request = None
    audio.sco_request(connection, hci.HCI_Connection_Complete_Event.LinkType.SCO)
    await asyncio.sleep(0)
    assert not sent and audio.held_sco_request[0] is connection
    audio.release_sco_request()
    await asyncio.sleep(0)
    assert len(sent) == 1 and isinstance(sent[0], hci.HCI_Enhanced_Accept_Synchronous_Connection_Request_Command)
    assert sent[0].bd_addr == connection.peer_address
    assert audio.held_sco_request is None and not audio.hold_sco_accept
    audio.hold_sco_accept = True
    audio.sco_request(connection, hci.HCI_Connection_Complete_Event.LinkType.SCO)
    audio.connections[connection.handle] = SimpleNamespace(handle=7, peer_address=connection.peer_address)
    try:
        audio.release_sco_request()
    except AssertionError:
        pass
    else:
        raise AssertionError('held acceptance was applied to a replacement connection')
    await asyncio.sleep(0)
    assert len(sent) == 1
    audio.connections[connection.handle] = connection
    audio.hold_sco_accept = True
    audio.sco_request(connection, hci.HCI_Connection_Complete_Event.LinkType.SCO)
    audio.disconnected(connection, 0x13)
    assert audio.held_sco_request is None
    print('bluetooth-radio-peers: self-test PASS: address reuse, exact protocol errors, pinned HFP gains, bounded SCO hold/release and stale-connection refusal')


def free_ports(count):
    sockets = []
    try:
        for _ in range(count):
            sock = socket.socket()
            sock.bind(('127.0.0.1', 0))
            sockets.append(sock)
        return [sock.getsockname()[1] for sock in sockets]
    finally:
        for sock in sockets:
            sock.close()


async def run(args):
    args.work.mkdir(parents=True, exist_ok=True)
    events = []
    groups = {}
    clients = set()
    stopping = asyncio.Event()
    loop = asyncio.get_running_loop()
    for signum in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(signum, stopping.set)
    event_log = (args.work / 'events.jsonl').open('w')

    def emit(event, **fields):
        item = dict(sequence=len(events), seconds=time.monotonic(), event=event, **fields)
        events.append(item)
        event_log.write(json.dumps(item, sort_keys=True) + '\n')
        event_log.flush()

    controller_log = (args.work / 'rootcanal.log').open('w')
    ports = free_ports(4)
    controller = await asyncio.create_subprocess_exec(
        sys.executable, '-m', 'rootcanal',
        *(f'--{name}_port={port}' for name, port in zip(('test', 'hci', 'link', 'link_ble'), ports)),
        stdout=controller_log, stderr=asyncio.subprocess.STDOUT, start_new_session=True)
    server = None
    try:
        # Do not open a dummy HCI controller (it consumes an address), or briefly
        # connect/disconnect the test socket (RootCanal may abort on its greeting's
        # broken pipe). Its startup log records the actual listening HCI socket.
        for _ in range(100):
            if f'Listening on: {ports[1]} ' in (args.work / 'rootcanal.log').read_text(errors='replace'):
                break
            if controller.returncode is not None:
                raise RuntimeError('RootCanal exited during startup')
            await asyncio.sleep(.05)
        else:
            raise TimeoutError('RootCanal HCI port did not open')
        endpoint = f'127.0.0.1:{ports[1]}'
        groups['classic'] = await create_classic_peers(endpoint, args.work, emit)
        if not args.classic_only:
            from bluetooth_le_peers import create_le_peers
            from bluetooth_generic_le_peers import create_generic_le_peers
            groups['le'] = await create_le_peers(endpoint, args.work, emit)
            groups['le-generic'] = await create_generic_le_peers(endpoint, args.work, emit)

        async def handle(reader, writer):
            clients.add(writer)
            try:
                while line := await reader.readline():
                    try:
                        request = json.loads(line)
                        action = request['action']
                        if action == 'events':
                            result = {'events': events[request.get('since', 0):], 'next': len(events)}
                        elif action == 'shutdown':
                            stopping.set()
                            result = {'stopping': True}
                        elif action == 'status' and 'group' not in request:
                            result = {name: await peers.command({'action': 'status'}) for name, peers in groups.items()}
                        else:
                            result = await asyncio.wait_for(groups[request.get('group', 'classic')].command(request), 40)
                        reply = {'ok': True, 'result': result}
                    except Exception as error:
                        reply = control_error(error)
                        emit('control-failure', **reply)
                    writer.write((json.dumps(reply) + '\n').encode())
                    await writer.drain()
            finally:
                clients.discard(writer)
                writer.close()
                with contextlib.suppress(ConnectionError):
                    await writer.wait_closed()

        server = await asyncio.start_unix_server(handle, path=args.socket)
        ready = {'endpoint': endpoint, 'socket': str(args.socket), 'groups': {name: await peers.command({'action': 'status'}) for name, peers in groups.items()}}
        (args.work / 'ready.json').write_text(json.dumps(ready, indent=2) + '\n')
        print(json.dumps(ready), flush=True)
        await stopping.wait()
    finally:
        if server:
            server.close()
            for client in list(clients):
                client.close()
            await server.wait_closed()
        for peers in reversed(list(groups.values())):
            await peers.close()
        if controller.returncode is None:
            os.killpg(controller.pid, signal.SIGTERM)
            try:
                await asyncio.wait_for(controller.wait(), 5)
            except TimeoutError:
                os.killpg(controller.pid, signal.SIGKILL)
                await controller.wait()
        controller_log.close()
        event_log.close()
        args.socket.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--work', type=Path)
    parser.add_argument('--socket', type=Path)
    parser.add_argument('--self-test', action='store_true')
    parser.add_argument('--classic-only', action='store_true', help='bounded host development probe; does not satisfy the full guest gate')
    args = parser.parse_args()
    if args.self_test:
        asyncio.run(self_test())
    else:
        if args.work is None or args.socket is None:
            parser.error('--work and --socket are required for radio peers')
        asyncio.run(run(args))


if __name__ == '__main__':
    main()
