"""Explicitly same-team OPP/BNEP scenario peers over independent Bumble transports.

Bumble has neither OBEX nor BNEP. The plan allows this glue; the external Linux
network namespace and dnsmasq independently supply Ethernet/IP/ARP/ICMP/DHCP.
"""
import asyncio
import hashlib
import os
from pathlib import Path
import struct
import signal
import sys

from bumble import core, l2cap, rfcomm, sdp


def record(handle, service, protocols, extras=()):
    d = sdp.DataElement
    return [sdp.ServiceAttribute(number, value) for number, value in [
        (0x0000, d.unsigned_integer_32(handle)),
        (0x0001, d.sequence([d.uuid(core.UUID.from_16_bits(service))])),
        (0x0004, d.sequence(protocols)),
        (0x0009, d.sequence([d.sequence([d.uuid(core.UUID.from_16_bits(service)), d.unsigned_integer_16(0x0102)])])),
        *extras,
    ]]


def obex_packet(opcode, payload=b''):
    return bytes([opcode]) + struct.pack('>H', len(payload) + 3) + payload


def obex_header(identifier, payload):
    return bytes([identifier]) + struct.pack('>H', len(payload) + 3) + payload


def obex_headers(data):
    while data:
        kind = data[0]
        if kind >> 6 < 2:
            size, = struct.unpack_from('>H', data, 1)
            if size < 3 or size > len(data):
                raise ValueError('invalid OBEX variable header')
            value = data[3:size]
        else:
            size = 2 if kind >> 6 == 2 else 5
            if len(data) < size:
                raise ValueError('short OBEX fixed header')
            value = data[1:size]
        yield kind, value
        data = data[size:]


class TransferPeer:
    def __init__(self, peer):
        self.peer = peer
        self.received = bytearray()
        self.received_name = None
        self.opp_transport = None
        self.opp_channel = None
        self.opp_complete = False
        self.pan_channel = None
        self.network = None
        self.frames_in = self.frames_out = 0
        self.server = rfcomm.Server(peer.device)
        self.server.listen(self.opp_accept, channel=5)
        self.opp_l2cap_server = peer.device.create_l2cap_server(
            l2cap.ClassicChannelSpec(psm=0x1001, mtu=4096, mps=128,
                                   mode=l2cap.TransmissionMode.ENHANCED_RETRANSMISSION),
            lambda channel: self.opp_accept(channel, transport='l2cap-ertm'))
        self.offer_l2cap(False)
        d = sdp.DataElement
        peer.device.sdp_service_records[0x10006] = record(0x10006, 0x1116, [
            d.sequence([d.uuid(core.BT_L2CAP_PROTOCOL_ID), d.unsigned_integer_16(0xf)]),
            d.sequence([d.uuid(core.BT_BNEP_PROTOCOL_ID), d.unsigned_integer_16(0x0100), d.sequence([d.unsigned_integer_16(0x0800), d.unsigned_integer_16(0x0806)])]),
        ], [(0x030a, d.unsigned_integer_16(0xfffe)), (0x030b, d.unsigned_integer_32(10000000))])
        self.pan_server = peer.device.create_l2cap_server(l2cap.ClassicChannelSpec(psm=0xf, mtu=1691), self.pan_accept)

    def offer_l2cap(self, enabled):
        d = sdp.DataElement
        extras = [(0x0303, d.sequence([d.unsigned_integer_8(0xff)]))]
        if enabled:
            extras.append((0x0200, d.unsigned_integer_16(0x1001)))
        self.peer.device.sdp_service_records[0x10005] = record(0x10005, 0x1105, [
            d.sequence([d.uuid(core.BT_L2CAP_PROTOCOL_ID)]),
            d.sequence([d.uuid(core.BT_RFCOMM_PROTOCOL_ID), d.unsigned_integer_8(5)]),
            d.sequence([d.uuid(core.BT_OBEX_PROTOCOL_ID)]),
        ], extras)
        return dict(goep_psm=0x1001 if enabled else None)

    def opp_accept(self, dlc, transport='rfcomm'):
        pending = bytearray()
        self.received.clear()
        self.received_name = None
        self.expected_size = None
        self.opp_transport = transport
        self.opp_channel = dlc
        self.opp_complete = False

        def data_received(data):
            if transport == 'l2cap-ertm' and not isinstance(dlc.processor, l2cap.EnhancedRetransmissionProcessor):
                raise ValueError('GOEP did not negotiate enhanced retransmission mode')
            pending.extend(data)
            while len(pending) >= 3:
                size, = struct.unpack_from('>H', pending, 1)
                if size < 3 or size > 4096:
                    raise ValueError('invalid OBEX packet length')
                if len(pending) < size:
                    return
                packet = bytes(pending[:size])
                del pending[:size]
                opcode = packet[0]
                if opcode == 0x80:
                    if len(packet) < 7:
                        raise ValueError('short OBEX CONNECT')
                    dlc.write(obex_packet(0xa0, b'\x10\x00\x10\x00'))
                elif opcode in (0x02, 0x82):
                    for identifier, value in obex_headers(packet[3:]):
                        if identifier == 0x01:
                            self.received_name = value.decode('utf-16-be').rstrip('\0')
                        elif identifier == 0xc3:
                            self.expected_size, = struct.unpack('>I', value)
                        elif identifier in (0x48, 0x49):
                            self.received.extend(value)
                    if len(self.received) > 1 << 20:
                        raise ValueError('scenario OBEX object exceeded 1 MiB')
                    if opcode == 0x82:
                        if self.expected_size is not None and self.expected_size != len(self.received):
                            raise ValueError('OBEX length differs from received object')
                        self.opp_complete = True
                        (self.peer.work / 'opp-received.bin').write_bytes(self.received)
                        self.peer.observe('opp-received', **self.opp_verdict())
                    dlc.write(obex_packet(0xa0 if opcode == 0x82 else 0x90))
                elif opcode in (0x81, 0xff):
                    dlc.write(obex_packet(0xa0))
                else:
                    dlc.write(obex_packet(0xc0))

        dlc.sink = data_received
        self.peer.observe('opp-open', transport=transport)

    def opp_verdict(self):
        return dict(name=self.received_name, bytes=len(self.received), sha256=hashlib.sha256(self.received).hexdigest(),
                    transport=self.opp_transport, complete=self.opp_complete,
                    ertm=isinstance(getattr(self.opp_channel, 'processor', None), l2cap.EnhancedRetransmissionProcessor))

    async def opp_send(self, address, name, data):
        connection = await self.peer.connection(address)
        client = rfcomm.Client(connection)
        multiplexer = await client.start()
        try:
            try:
                dlc = await multiplexer.open_dlc(9)
            except core.ConnectionError as error:
                # Preserve the real RFCOMM DM refusal in the control response;
                # Bumble's default repr omits its namespace and error code.
                if error.error_namespace == 'rfcomm' and error.error_code == core.ConnectionError.CONNECTION_REFUSED:
                    raise ValueError('OPP RFCOMM channel 9 refused by peer') from error
                raise
            incoming = asyncio.StreamReader()
            dlc.sink = incoming.feed_data

            async def exchange(opcode, payload, expected):
                dlc.write(obex_packet(opcode, payload))
                head = await asyncio.wait_for(incoming.readexactly(3), 10)
                size, = struct.unpack_from('>H', head, 1)
                if size < 3 or size > 4096:
                    raise ValueError(f'invalid OBEX response length {size}')
                if 0xc0 <= head[0] <= 0xe1:
                    raise ValueError(f'OBEX refusal code 0x{head[0]:02x}')
                if head[0] != expected:
                    raise ValueError(f'OBEX response {head.hex()}, expected {expected:02x}')
                return await incoming.readexactly(size - 3)

            connected = await exchange(0x80, b'\x10\x00\x10\x00', 0xa0)
            if len(connected) < 4:
                raise ValueError('short OBEX CONNECT response')
            mtu, = struct.unpack_from('>H', connected, 2)
            headers = obex_header(1, (name + '\0').encode('utf-16-be')) + b'\xc3' + struct.pack('>I', len(data))
            await exchange(0x02, headers, 0x90)
            chunk = min(512, mtu - 6)
            if chunk <= 0:
                raise ValueError('invalid OBEX peer MTU')
            for offset in range(0, len(data), chunk):
                fragment = data[offset:offset + chunk]
                final = offset + len(fragment) == len(data)
                await exchange(0x82 if final else 0x02, obex_header(0x49 if final else 0x48, fragment), 0xa0 if final else 0x90)
            await exchange(0x81, b'', 0xa0)
            result = dict(bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
            self.peer.observe('opp-sent', **result)
            return result
        finally:
            await client.shutdown()

    async def pan_start(self):
        if self.network:
            return
        path = self.peer.work / 'pan-network.sock'
        self.mac = bytes.fromhex(str(self.peer.device.public_address).split('/')[0].replace(':', ''))
        script = Path(__file__).with_name('bluetooth-pan-network.py')
        self.network_log = (self.peer.work / 'pan-network.log').open('w')
        self.network = await asyncio.create_subprocess_exec('unshare', '--net', sys.executable, str(script), '--work', str(self.peer.work), '--socket', str(path), '--mac', self.mac.hex(':'), stdout=self.network_log, stderr=asyncio.subprocess.STDOUT, start_new_session=True)
        for _ in range(100):
            try:
                self.network_reader, self.network_writer = await asyncio.open_unix_connection(path)
                break
            except OSError:
                if self.network.returncode is not None:
                    raise RuntimeError('PAN network process exited; inspect pan-network.log')
                await asyncio.sleep(.05)
        else:
            raise TimeoutError('PAN network socket did not open')
        self.peer.spawn(self.pan_receive())
        self.peer.observe('pan-network-ready', gateway='10.94.0.1', dhcp='dnsmasq', udp_echo=194)

    def pan_accept(self, channel):
        if not self.network:
            raise RuntimeError('PAN network must be started before connecting')
        self.pan_channel = channel
        self.remote_mac = bytes.fromhex(str(channel.connection.peer_address).split('/')[0].replace(':', ''))
        channel.sink = self.pan_data
        self.peer.observe('pan-l2cap-open')

    def pan_data(self, data):
        if not data or data[0] & 0x80:
            raise ValueError('BNEP extension is outside this scenario')
        kind = data[0]
        if kind == 1:
            command = data[1]
            if command == 1:
                if data[2:] != b'\x02\x11\x16\x11\x15':
                    raise ValueError('BNEP setup did not request NAP from PANU')
                self.pan_channel.write(b'\x01\x02\x00\x00')
                self.peer.observe('pan-setup')
            elif command in (3, 5):
                self.pan_channel.write(bytes([1, command + 1, 0, 0]))
            else:
                self.pan_channel.write(bytes([1, 0, command]))
            return
        if kind == 0:
            frame = data[1:]
        elif kind == 2:
            frame = self.mac + self.remote_mac + data[1:]
        elif kind == 3:
            frame = self.mac + data[1:]
        elif kind == 4:
            frame = data[1:7] + self.remote_mac + data[7:]
        else:
            raise ValueError('unknown BNEP packet type')
        if not 14 <= len(frame) <= 1518:
            raise ValueError('BNEP Ethernet frame length is invalid')
        self.frames_in += 1
        self.network_writer.write(struct.pack('<H', len(frame)) + frame)

    async def pan_receive(self):
        while True:
            size, = struct.unpack('<H', await self.network_reader.readexactly(2))
            frame = await self.network_reader.readexactly(size)
            if self.pan_channel and self.pan_channel.state == l2cap.ClassicChannel.State.OPEN:
                self.pan_channel.write(b'\0' + frame)
                self.frames_out += 1

    def pan_verdict(self):
        leases = self.peer.work / 'dnsmasq.leases'
        return dict(frames_in=self.frames_in, frames_out=self.frames_out, leases=leases.read_text() if leases.exists() else '')

    async def close(self):
        if self.network:
            if hasattr(self, 'network_writer'):
                self.network_writer.close()
                await self.network_writer.wait_closed()
            if self.network.returncode is None:
                self.network.terminate()
                try:
                    await asyncio.wait_for(self.network.wait(), 5)
                except TimeoutError:
                    os.killpg(self.network.pid, signal.SIGKILL)
                    await self.network.wait()
            self.network_log.close()
