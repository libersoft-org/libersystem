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
import sys

from bumble import core, device, l2cap, pairing, rfcomm, sdp
from bumble.transport import open_transport
from bluetooth_classic_peers import Peer
from bluetooth_transfer_peers import TransferPeer


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


async def run(work):
    work.mkdir(parents=True, exist_ok=False)
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
        controller = await asyncio.create_subprocess_exec(sys.executable, '-m', 'rootcanal',
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
    asyncio.run(run(parser.parse_args().work))
