#!/usr/bin/env python3
"""Check the same-team OBEX peer over real Bumble RFCOMM and ERTM transports.

This validates the test equipment, not the Liber Bluetooth implementation.
RootCanal, protocol events, exact payload receipts and scope are retained in --work.
"""
import argparse
import asyncio
import hashlib
import json
from pathlib import Path
import socket
import struct
from types import SimpleNamespace
from unittest.mock import patch

from bumble import core, device, l2cap, pairing, rfcomm, sdp
from bumble.transport import open_transport
from bluetooth_classic_peers import Peer
from bluetooth_transfer_peers import TransferPeer


async def exercise_send_cleanup(work):
    """Real peer method and Bumble state machines; only the L2CAP wire is modeled.

    Both channels begin already configured/open, without HCI, controller or RF.
    Real Client.start/shutdown, Multiplexer, DLC and ClassicChannel disconnect
    process the exchanged frames. Faults are injected only at the wire boundary.
    """
    payload = bytes((index * 37 + 11) % 251 for index in range(963))
    expected = dict(bytes=len(payload), sha256=hashlib.sha256(payload).hexdigest())
    completed = []
    for mode in ('closed-during-mux', 'open', 'exchange-error', 'obex-refusal',
                 'mux-error', 'waiting-channel', 'other-cleanup-error', 'open-invalid-state'):
        case = work / ('send-cleanup-' + mode)
        case.mkdir()
        events, requests, clients = [], [], []
        sentinel = (core.InvalidStateError(mode) if mode in ('mux-error', 'open-invalid-state')
                    else RuntimeError(mode))
        loop = asyncio.get_running_loop()
        channels = []

        class Wire:
            def next_identifier(self, connection):
                return 1

            def on_channel_closed(self, channel):
                events.append(('closed', channels.index(channel)))

            def send_control_frame(self, connection, cid, frame):
                assert cid == 1 and isinstance(frame, (l2cap.L2CAP_Disconnection_Request, l2cap.L2CAP_Disconnection_Response))
                requests.append((connection.side, type(frame).__name__))
                if connection.side == 0 and isinstance(frame, l2cap.L2CAP_Disconnection_Request):
                    if mode == 'other-cleanup-error':
                        raise sentinel
                    if mode == 'open-invalid-state':
                        channels[0]._change_state(l2cap.ClassicChannel.State.OPEN)
                        raise sentinel
                target = channels[1 - connection.side]
                handler = (target.on_disconnection_request if isinstance(frame, l2cap.L2CAP_Disconnection_Request)
                           else target.on_disconnection_response)
                loop.call_soon(handler, frame)

            def send_pdu(self, connection, cid, data, fcs):
                assert not fcs
                frame = rfcomm.RFCOMM_Frame.from_bytes(bytes(data))
                information = frame.information[1:] if frame.p_f else frame.information
                if connection.side == 0 and frame.dlci and information[:1] == b'\x80' and mode == 'exchange-error':
                    raise sentinel
                if connection.side == 0 and frame.dlci == 0 and frame.type == rfcomm.FrameType.DISC and mode == 'mux-error':
                    channels[0].abort()
                    raise sentinel
                loop.call_soon(channels[1 - connection.side].on_pdu, bytes(data))
                if connection.side == 1 and frame.dlci == 0 and frame.type == rfcomm.FrameType.UA:
                    if channels[1].sink.__self__.state == rfcomm.Multiplexer.State.DISCONNECTED:
                        events.append(('mux-reply', channels[0].state.name))
                        if mode in ('closed-during-mux', 'exchange-error', 'obex-refusal'):
                            # UA and the peer's real close request are delivered
                            # before Client.shutdown resumes its awaited mux close.
                            channels[1]._disconnect_sync()
                        elif mode == 'waiting-channel':
                            loop.call_soon(channels[0]._change_state, l2cap.ClassicChannel.State.WAIT_DISCONNECT)

        wire = Wire()
        connections = [SimpleNamespace(side=side, peer_address='modeled-peer') for side in (0, 1)]
        for side, connection in enumerate(connections):
            channel = l2cap.ClassicChannel(wire, connection, 1, rfcomm.RFCOMM_PSM, 64 + side,
                l2cap.ClassicChannelSpec(psm=rfcomm.RFCOMM_PSM, mtu=1024, fcs_enabled=False))
            channel.destination_cid = 65 - side
            channel.peer_mtu = 1024
            channel._change_state(l2cap.ClassicChannel.State.OPEN)
            channels.append(channel)

        async def create_channel(*, spec):
            assert spec.psm == rfcomm.RFCOMM_PSM
            return channels[0]

        async def connect(address):
            assert address == 'modeled-peer'
            return connections[0]

        connections[0].create_l2cap_channel = create_channel
        receiver = object.__new__(TransferPeer)
        receiver.received = bytearray()
        receiver.peer = SimpleNamespace(work=case, observe=lambda event, **values: events.append((event, values)))
        remote = rfcomm.Multiplexer(channels[1], rfcomm.Multiplexer.Role.RESPONDER)
        remote.acceptor = lambda number: ((rfcomm.RFCOMM_DEFAULT_MAX_FRAME_SIZE, rfcomm.RFCOMM_DEFAULT_INITIAL_CREDITS)
                                          if number == 9 else None)

        def accept(dlc):
            receiver.opp_accept(dlc)
            original_sink = dlc.sink

            def receive(data):
                if mode == 'obex-refusal' and data[:1] == b'\x02':
                    dlc.write(b'\xc3\x00\x03')
                else:
                    original_sink(data)

            dlc.sink = receive

        remote.on(remote.EVENT_DLC, accept)
        sender = object.__new__(TransferPeer)
        sender.peer = SimpleNamespace(connection=connect, observe=lambda event, **values: events.append((event, values)))
        original_client = rfcomm.Client

        def capture_client(connection):
            client = original_client(connection)
            clients.append(client)
            return client

        result, error = None, None
        with patch.object(rfcomm, 'Client', side_effect=capture_client):
            try:
                result = await asyncio.wait_for(sender.opp_send('modeled-peer', 'cleanup.bin', payload), 5)
            except Exception as caught:
                error = caught
        await asyncio.sleep(0)  # Complete the modeled peer's queued close response.
        assert len(clients) == 1
        client = clients[0]
        sent = [value for event, value in events if event == 'opp-sent']
        (case / 'observation.json').write_text(json.dumps(dict(case=mode, result=result,
            error=repr(error) if error else None, events=events, control_frames=requests,
            channel_states=[channel.state.name for channel in channels],
            multiplexer_cleared=client.multiplexer is None, channel_cleared=client.l2cap_channel is None), indent=2) + '\n')
        if mode in ('closed-during-mux', 'open'):
            assert error is None and result == expected, (mode, result, repr(error))
            assert sent == [expected] and receiver.received == payload and receiver.opp_complete and receiver.received_name == 'cleanup.bin'
            assert client.multiplexer is None and client.l2cap_channel is None
            assert channels[0].state == channels[1].state == l2cap.ClassicChannel.State.CLOSED
            assert ((0, 'L2CAP_Disconnection_Request') in requests) == (mode == 'open')
        elif mode == 'obex-refusal':
            assert type(error) is ValueError and str(error) == 'OBEX refusal code 0xc3', repr(error)
            assert not sent and client.multiplexer is None and client.l2cap_channel is None
        elif mode == 'waiting-channel':
            assert type(error) is core.InvalidStateError and str(error) == 'invalid state', repr(error)
            assert client.multiplexer is None and client.l2cap_channel.state == l2cap.ClassicChannel.State.WAIT_DISCONNECT
        else:
            assert error is sentinel, (mode, repr(error))
            if mode == 'exchange-error':
                assert not sent and client.multiplexer is None and client.l2cap_channel is None
            elif mode == 'mux-error':
                assert client.multiplexer is not None and channels[0].state == l2cap.ClassicChannel.State.CLOSED
            elif mode == 'open-invalid-state':
                assert client.multiplexer is None and channels[0].state == l2cap.ClassicChannel.State.OPEN
        row = dict(case=mode, status='passed', result=result, error=repr(error) if error else None,
                   events=events, control_frames=requests)
        (case / 'result.json').write_text(json.dumps(row, indent=2) + '\n')
        completed.append(row)
        print('bluetooth-opp-send-cleanup: PASS ' + mode, flush=True)
    summary = dict(status='passed', scope='actual peer and Bumble state machines; modeled L2CAP wire, no guest or RF', cases=completed)
    (work / 'send-cleanup-summary.json').write_text(json.dumps(summary, indent=2) + '\n')


async def exercise(endpoint, work, emit):
    transports, peers = [], []
    try:
        for name in ('receiver', 'sender'):
            transport = await open_transport(f'tcp-client:{endpoint}')
            transports.append(transport)
            config = device.DeviceConfiguration.from_dict(dict(
                name=f'OPP fixture {name}', classic_enabled=True, le_enabled=False,
                classic_sc_enabled=True))
            instance = device.Device.from_config_with_hci(config, transport.source, transport.sink)
            peer = Peer(name, transport, instance, work, emit)
            instance.pairing_config_factory = lambda _, delegate=peer.pairing_delegate: pairing.PairingConfig(
                sc=True, mitm=False, bonding=True, delegate=delegate)
            peers.append(peer)
            if name == 'receiver':
                peer.transfer = TransferPeer(peer)
            await instance.power_on()
            await instance.set_connectable(True)
        receiver, sender = peers
        connection = await sender.connection(str(receiver.device.public_address))
        payload = bytes((index * 37 + 11) % 251 for index in range(4915))
        receipts = []
        for goep in (False, True):
            receiver.transfer.offer_l2cap(goep)
            async with sdp.Client(connection) as discovery:
                records = await discovery.search_attributes([core.UUID.from_16_bits(0x1105)], [(0, 0xffff)])
            assert len(records) == 1, records
            offered = [attribute.value.value for attribute in records[0] if attribute.id == 0x0200]
            assert offered == ([0x1001] if goep else []), offered
            client = None
            if goep:
                channel = await connection.create_l2cap_channel(spec=l2cap.ClassicChannelSpec(
                    psm=offered[0], mtu=4096, mps=128,
                    mode=l2cap.TransmissionMode.ENHANCED_RETRANSMISSION))
                assert isinstance(channel.processor, l2cap.EnhancedRetransmissionProcessor)
                assert channel.processor.peer_mps == 128
            else:
                client = rfcomm.Client(connection)
                multiplexer = await client.start()
                channel = await multiplexer.open_dlc(5)
            incoming = asyncio.StreamReader()
            channel.sink = incoming.feed_data

            async def exchange(opcode, body, expected):
                channel.write(bytes([opcode]) + struct.pack('>H', len(body) + 3) + body)
                head = await asyncio.wait_for(incoming.readexactly(3), 5)
                size = int.from_bytes(head[1:], 'big')
                assert head[0] == expected and 3 <= size <= 4096, head.hex()
                return await asyncio.wait_for(incoming.readexactly(size - 3), 5)

            try:
                assert await exchange(0x80, b'\x10\x00\x10\x00', 0xa0) == b'\x10\x00\x10\x00'
                name = 'transport-proof.bin\0'.encode('utf-16-be')
                headers = b'\x01' + struct.pack('>H', len(name) + 3) + name
                headers += b'\xc3' + struct.pack('>I', len(payload))
                await exchange(0x02, headers, 0x90)
                # Each 700-byte OBEX payload crosses several independent 128-byte ERTM PDUs.
                for offset in range(0, len(payload), 700):
                    chunk = payload[offset:offset + 700]
                    final = offset + len(chunk) == len(payload)
                    body = bytes([0x49 if final else 0x48]) + struct.pack('>H', len(chunk) + 3) + chunk
                    await exchange(0x82 if final else 0x02, body, 0xa0 if final else 0x90)
                await exchange(0x81, b'', 0xa0)
                receipt = receiver.transfer.opp_verdict()
                assert receipt == dict(name='transport-proof.bin', bytes=len(payload),
                    sha256=hashlib.sha256(payload).hexdigest(), complete=True,
                    transport='l2cap-ertm' if goep else 'rfcomm', ertm=goep), receipt
                receipts.append(receipt)
            finally:
                if client:
                    await client.shutdown()
                else:
                    await channel.disconnect()
        summary = dict(status='passed', scope='host fixture only; no Liber guest', receipts=receipts)
        (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
        print(json.dumps(summary), flush=True)
    finally:
        for peer in reversed(peers):
            await peer.close()
        for transport in transports[len(peers):]:
            await transport.close()


async def run(work, send_cleanup_only=False):
    work.mkdir(parents=True, exist_ok=False)
    await asyncio.wait_for(exercise_send_cleanup(work), 15)
    if send_cleanup_only:
        return
    from rootcanal.binaries import get_package_binary_resource_path

    sockets = [socket.socket() for _ in range(4)]
    try:
        for sock in sockets:
            sock.bind(('127.0.0.1', 0))
        ports = [sock.getsockname()[1] for sock in sockets]
    finally:
        for sock in sockets:
            sock.close()
    with (work / 'events.jsonl').open('w') as events, (work / 'rootcanal.log').open('w') as log:
        def emit(event, **values):
            events.write(json.dumps(dict(event=event, **values)) + '\n')
            events.flush()
        controller = await asyncio.create_subprocess_exec(get_package_binary_resource_path('rootcanal'),
            *(f'--{name}_port={port}' for name, port in zip(('test', 'hci', 'link', 'link_ble'), ports)),
            '--enable_log_color=false', stdout=log, stderr=asyncio.subprocess.STDOUT)
        try:
            async with asyncio.timeout(5):
                while f'Listening on: {ports[1]} ' not in (work / 'rootcanal.log').read_text():
                    if controller.returncode is not None:
                        raise RuntimeError('RootCanal exited before HCI was ready')
                    await asyncio.sleep(.05)
            await asyncio.wait_for(exercise(f'127.0.0.1:{ports[1]}', work, emit), 40)
        finally:
            if controller.returncode is None:
                controller.terminate()
                try:
                    await asyncio.wait_for(controller.wait(), 5)
                except TimeoutError:
                    controller.kill()
                    await controller.wait()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--work', type=Path, required=True)
    parser.add_argument('--send-cleanup-only', action='store_true', help='run only the bounded sender cleanup regression, without RootCanal')
    args = parser.parse_args()
    asyncio.run(run(args.work, args.send_cleanup_only))
