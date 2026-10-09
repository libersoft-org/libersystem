#!/usr/bin/env python3
"""Private network namespace/TAP for the same-team PAN peer; dnsmasq owns DHCP."""
import argparse
import asyncio
import fcntl
import os
from pathlib import Path
import signal
import struct
import subprocess


async def run(args):
    tap = os.open('/dev/net/tun', os.O_RDWR | os.O_NONBLOCK)
    fcntl.ioctl(tap, 0x400454ca, struct.pack('16sH', b'btpan0', 0x0002 | 0x1000))
    for command in [
        ['ip', 'link', 'set', 'lo', 'up'],
        ['ip', 'link', 'set', 'btpan0', 'address', args.mac],
        ['ip', 'addr', 'add', '10.94.0.1/24', 'dev', 'btpan0'],
        ['ip', 'link', 'set', 'btpan0', 'up'],
    ]:
        subprocess.run(command, check=True)
    log = (args.work / 'dnsmasq.log').open('w')
    dnsmasq = await asyncio.create_subprocess_exec(
        'dnsmasq', '--no-daemon', '--conf-file=/dev/null', '--port=0', '--bind-interfaces',
        '--interface=btpan0', '--dhcp-range=10.94.0.10,10.94.0.20,255.255.255.0,1h',
        '--dhcp-option=3,10.94.0.1', '--dhcp-option=6,10.94.0.1',
        f'--dhcp-leasefile={args.work / "dnsmasq.leases"}', '--log-dhcp',
        '--dhcp-authoritative', '--user=root', stdout=log, stderr=asyncio.subprocess.STDOUT)
    loop = asyncio.get_running_loop()
    stopping = asyncio.Event()
    for signum in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(signum, stopping.set)
    pending = asyncio.Queue(maxsize=128)

    def readable():
        try:
            frame = os.read(tap, 2048)
            pending.put_nowait(frame)
        except BlockingIOError:
            pass
        except asyncio.QueueFull:
            stopping.set()

    async def handle(reader, writer):
        async def send():
            while True:
                frame = await pending.get()
                writer.write(struct.pack('<H', len(frame)) + frame)
                await writer.drain()
        task = asyncio.create_task(send())
        try:
            while True:
                size, = struct.unpack('<H', await reader.readexactly(2))
                if not 14 <= size <= 1518:
                    raise ValueError('invalid Ethernet frame size')
                frame = await reader.readexactly(size)
                os.write(tap, frame)
        except asyncio.IncompleteReadError:
            stopping.set()
        finally:
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)
            writer.close()
            await writer.wait_closed()

    class Echo(asyncio.DatagramProtocol):
        def connection_made(self, transport):
            self.transport = transport

        def datagram_received(self, data, address):
            self.transport.sendto(data, address)

    echo, _ = await loop.create_datagram_endpoint(Echo, local_addr=('10.94.0.1', 194))
    async def tcp_echo(reader, writer):
        data = await reader.read(4096)
        writer.write(data)
        await writer.drain()
        writer.close()
        await writer.wait_closed()

    tcp = await asyncio.start_server(tcp_echo, '10.94.0.1', 194)
    server = await asyncio.start_unix_server(handle, path=args.socket)
    loop.add_reader(tap, readable)
    try:
        await asyncio.sleep(.15)
        if dnsmasq.returncode is not None:
            raise RuntimeError('dnsmasq exited during startup; inspect dnsmasq.log')
        (args.work / 'pan-network.ready').write_text('dnsmasq 10.94.0.1, DHCP 10.94.0.10..20, UDP echo 194\n')
        await stopping.wait()
    finally:
        server.close()
        await server.wait_closed()
        echo.close()
        tcp.close()
        await tcp.wait_closed()
        loop.remove_reader(tap)
        os.close(tap)
        if dnsmasq.returncode is None:
            dnsmasq.terminate()
            await dnsmasq.wait()
        log.close()
        args.socket.unlink(missing_ok=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--work', type=Path, required=True)
    parser.add_argument('--socket', type=Path, required=True)
    parser.add_argument('--mac', required=True)
    asyncio.run(run(parser.parse_args()))
