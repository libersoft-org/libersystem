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


async def legacy_peer_pin_self_test():
    """Exercise pinned Bumble's actual PIN dispatch; host HCI transport is captured."""
    from types import SimpleNamespace
    from bumble import device, hci, pairing, utils
    from bluetooth_classic_peers import ScenarioPairing

    address = hci.Address('DA:4C:10:DE:00:0C', hci.Address.PUBLIC_DEVICE_ADDRESS)
    for io in ('NO_OUTPUT_NO_INPUT', 'KEYBOARD_INPUT_ONLY'):
        observed, sent = [], []
        delegate = ScenarioPairing(SimpleNamespace(observe=lambda event, **fields: observed.append((event, fields))))
        delegate.io_capability = pairing.PairingDelegate.IoCapability[io]
        async def send(command):
            sent.append(command)
        async def cancel_on_disconnection(operation):
            return await operation
        connection = SimpleNamespace(peer_address=address, cancel_on_disconnection=cancel_on_disconnection)
        instance = SimpleNamespace(
            host=SimpleNamespace(send_sync_command=send), pending_connections={}, connections={7: connection},
            pairing_config_factory=lambda _: pairing.PairingConfig(sc=False, mitm=False, bonding=True, delegate=delegate))
        before = set(utils.AsyncRunner.running_tasks)
        # The real address-to-connection decorator and actual Device handler run.
        device.Device.on_pin_code_request(instance, address)
        tasks = set(utils.AsyncRunner.running_tasks) - before
        assert len(tasks) == 1
        await asyncio.gather(*tasks)
        assert len(sent) == 1 and sent[0].bd_addr == address
        if io == 'NO_OUTPUT_NO_INPUT':
            assert isinstance(sent[0], hci.HCI_PIN_Code_Request_Negative_Reply_Command)
            assert observed == []
        else:
            assert isinstance(sent[0], hci.HCI_PIN_Code_Request_Reply_Command)
            assert sent[0].pin_code_length == 4 and sent[0].pin_code[:4] == b'1234'
            assert observed == [('pairing-pin-required', {})]
    print('bluetooth-radio-peers: pinned PIN dispatch PASS: no-input negative reply; keyboard delegate event and positive 1234 reply (captured HCI, no radio/PIN cryptographic validation)')


async def hsp_peer_self_test():
    from types import SimpleNamespace
    from unittest.mock import patch
    from bumble import hci, hfp, rfcomm

    audio = object.__new__(AudioPeer)
    audio.tasks = set()
    audio.hf_configuration = hfp.HfConfiguration(
        [hfp.HfFeature.CODEC_NEGOTIATION], [], [hfp.AudioCodec.CVSD, hfp.AudioCodec.MSBC])
    observed, writes, callbacks = [], [], {}
    audio.observe = lambda event, **fields: observed.append(dict(event=event, **fields))
    connection = SimpleNamespace(handle=7, peer_address=hci.Address('DA:4C:10:DE:00:00', hci.Address.PUBLIC_DEVICE_ADDRESS),
                                 transport=core.PhysicalTransport.BR_EDR, on=lambda *args: None)
    audio.connections = {connection.handle: connection}
    channel = SimpleNamespace(connection=connection, EVENT_CLOSE='close', on=lambda name, callback: callbacks.update({name: callback}))
    dlc = SimpleNamespace(multiplexer=SimpleNamespace(l2cap_channel=channel))
    def write(command):
        writes.append(command)
        assert command == 'AT+CKPD=200\r', 'HSP parser initiated an HFP command'
        dlc.sink(b'\r\nERROR\r\n')
    dlc.write = write
    async def start():
        return None
    async def open_dlc(channel):
        assert channel == 2
        return dlc
    async def find_channel(actual_connection, uuid):
        assert actual_connection is connection and uuid == core.UUID.from_16_bits(0x1112)
        return 2
    client = SimpleNamespace(start=start, multiplexer=SimpleNamespace(open_dlc=open_dlc))
    with patch.object(rfcomm, 'Client', lambda actual_connection: client if actual_connection is connection else None), \
         patch.object(rfcomm, 'find_rfcomm_channel_with_uuid', find_channel):
        await audio.hsp_connect('DA:4C:10:DE:00:00')
    tasks = list(audio.tasks)
    try:
        await asyncio.sleep(0)
        assert writes == [], 'HSP parser initiated an HFP command'
        assert audio.hf.active_codec == hfp.AudioCodec.CVSD
        dlc.sink(b'\r\nRING\r\n\r\n+VGM: 6\r\n\r\n+VGS: 9\r\n')
        await asyncio.sleep(0)
        assert observed == [dict(event='hsp-connected', channel=2, codec=int(hfp.AudioCodec.CVSD)),
                            dict(event='hfp-ring'), dict(event='hfp-microphone-volume', value=6),
                            dict(event='hfp-volume', value=9)]
        try:
            await audio.hf.execute_command('AT+CKPD=200')
        except hfp.HfpProtocolError as error:
            assert error.error_name == 'ERROR'
        else:
            raise AssertionError('HSP parser lost the actual ERROR response')
        assert writes == ['AT+CKPD=200\r']
    finally:
        callbacks['close']()
        await asyncio.wait_for(asyncio.gather(*tasks), 2)
    assert all(task.done() for task in tasks)


def profile_status_self_test():
    from types import SimpleNamespace
    from bumble import l2cap
    from bluetooth_classic_peers import HidPeer

    for kind in (HidPeer, AudioPeer):
        peer = object.__new__(kind)
        peer.name, peer.device = 'observed', SimpleNamespace(public_address='DA:4C:10:DE:00:01', sco_links={})
        peer.connections, peer.pairings, peer.encrypted, peer.received = {7: object()}, 2, 3, b''
        channel = SimpleNamespace(state=l2cap.ClassicChannel.State.OPEN)
        if kind is HidPeer:
            peer.hid = SimpleNamespace(l2cap_ctrl_channel=channel, l2cap_intr_channel=channel)
            fields = ('hid_control', 'hid_interrupt')
        else:
            peer.phone, peer.sco_link, peer.held_sco_request = True, None, None
            peer.avdtp = SimpleNamespace(l2cap_channel=channel)
            peer.source_stream = SimpleNamespace(rtp_channel=channel)
            peer.avrcp = SimpleNamespace(avctp_protocol=SimpleNamespace(l2cap_channel=channel))
            fields = ('audio_signalling', 'audio_media', 'audio_remote_control')
        for state in l2cap.ClassicChannel.State:
            channel.state = state
            observed = peer.status()
            assert observed['connections'] == 1 and observed['pairings'] == 2
            assert all(observed[field] == state.name for field in fields)
            assert all((observed[field] in (None, 'CLOSED')) == (state == l2cap.ClassicChannel.State.CLOSED) for field in fields)
    print('bluetooth-radio-peers: profile status PASS: retained objects report actual pinned L2CAP states; pending disconnect is not closed')


async def self_test():
    from types import SimpleNamespace
    from bumble import hci, hfp, l2cap

    profile_status_self_test()
    await legacy_peer_pin_self_test()
    await hsp_peer_self_test()

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
    audio.sco_only = False
    audio.sco_request(connection, hci.HCI_Connection_Complete_Event.LinkType.SCO)
    await asyncio.sleep(0)
    assert not sent and audio.held_sco_request[0] is connection
    audio.release_sco_request()
    await asyncio.sleep(0)
    assert len(sent) == 1 and isinstance(sent[0], hci.HCI_Enhanced_Accept_Synchronous_Connection_Request_Command)
    assert sent[0].bd_addr == connection.peer_address
    # HSP restricts the actual acceptance to HV3, including all inverted EDR exclusions.
    audio.sco_only = True
    audio.accept_sco(connection, hci.HCI_Connection_Complete_Event.LinkType.ESCO)
    await asyncio.sleep(0)
    assert len(sent) == 2 and int(sent[-1].packet_type) == 0x03c4
    assert int(sent[-1].retransmission_effort) == 0
    assert int(hfp.ESCO_PARAMETERS[hfp.DefaultCodecParameters.SCO_CVSD_D1].packet_type) == 4
    audio.sco_only = False
    sent.pop()
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
    print('bluetooth-radio-peers: self-test PASS: address reuse, exact protocol errors, HSP without HFP SLC, pinned HFP gains, bounded SCO hold/release and stale-connection refusal')


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
