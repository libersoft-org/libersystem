#!/usr/bin/env python3
"""Bounded real RootCanal/Bumble equipment check; no Liber guest is tested."""
import argparse
import asyncio
import copy
import json
from pathlib import Path
import socket
import sys

from bumble import att, core, device, gatt, hci, keys, pairing
from bumble.transport import open_transport
from bluetooth_generic_le_peers import create_generic_le_peers


async def until(predicate, seconds=5):
    async with asyncio.timeout(seconds):
        while not predicate():
            await asyncio.sleep(.01)


class Delegate(pairing.PairingDelegate):
    def __init__(self, events, target):
        both = self.DEFAULT_KEY_DISTRIBUTION | self.KeyDistribution.DISTRIBUTE_LINK_KEY
        super().__init__(io_capability=self.DISPLAY_OUTPUT_AND_KEYBOARD_INPUT,
                         local_initiator_key_distribution=both,
                         local_responder_key_distribution=both)
        self.events, self.target = events, target

    async def compare_numbers(self, number, digits):
        await until(lambda: any(row['event'] == 'pairing-number' and row['peer'] == self.target.name for row in self.events))
        assert [row for row in self.events if row['event'] == 'pairing-number' and row['peer'] == self.target.name][-1]['number'] == number
        return True

    async def display_number(self, number, digits):
        self.target.pairing_delegate.number = number

    async def get_number(self):
        await until(lambda: any(row['event'] == 'pairing-display' and row['peer'] == self.target.name for row in self.events))
        return [row for row in self.events if row['event'] == 'pairing-display' and row['peer'] == self.target.name][-1]['number']

    async def confirm(self, auto=False):
        return True


async def exercise(endpoint, work, emit, events):
    peers = await create_generic_le_peers(endpoint, work, emit)
    transport = None
    try:
        transport = await open_transport(f'tcp-client:{endpoint}')
        central = device.Device.from_config_with_hci(device.DeviceConfiguration.from_dict(dict(
            name='Generic LE checker', address='F0:F1:F2:F3:F4:C0',
            classic_enabled=True, classic_sc_enabled=True, le_enabled=True,
            le_privacy_enabled=True, le_rpa_timeout=0)), transport.source, transport.sink)
        central.keystore = keys.JsonKeyStore(namespace='central', filename=str(work / 'central-keys.json'))
        await central.power_on()
        await central.set_connectable(True)
        summaries = []

        async def configure(name, io, sc=True, mitm=True, public=False):
            target = peers.peers[name]
            await peers.command(dict(action='pairing-config', peer=name, io=io, sc=sc, mitm=mitm))
            delegate = Delegate(events, target)
            def configuration(connection):
                distribution = delegate.DEFAULT_KEY_DISTRIBUTION
                if connection.transport == core.PhysicalTransport.LE:
                    distribution |= delegate.KeyDistribution.DISTRIBUTE_LINK_KEY
                delegate.local_initiator_key_distribution = distribution
                delegate.local_responder_key_distribution = distribution
                return pairing.PairingConfig(sc=sc, mitm=mitm, bonding=True, delegate=delegate,
                    identity_address_type=pairing.PairingConfig.AddressType.PUBLIC if public else pairing.PairingConfig.AddressType.RANDOM)
            central.pairing_config_factory = configuration
            return target

        tag = await configure('tag', 'DISPLAY_OUTPUT_AND_YES_NO_INPUT')
        await tag.advertise()
        connection = await central.connect(tag.device.random_address)
        await connection.pair()
        assert connection.encryption
        proof = await tag.privacy()
        assert proof['encrypted'] and proof['is_private'] and proof['identity']
        remote = device.Peer(connection)
        await remote.request_mtu(247)
        await remote.discover_services()
        chars = await remote.discover_characteristics()
        by_uuid = {str(char.uuid): char for char in chars}
        def characteristic(uuid):
            return by_uuid[str(core.UUID.from_16_bits(uuid))]
        assert await characteristic(0x2a19).read_value() == b'\x57'
        for uuid in (0xfff1, 0xfff2):
            value = characteristic(uuid)
            assert await value.read_value() == b'fixture'
            received = []
            await value.subscribe(received.append, prefer_notify=uuid == 0xfff1)
            await value.write_value(b'hello', with_response=True)
            await until(lambda: received == [b'hello'])
            await value.unsubscribe(received.append)
        await until(lambda: tag.indications == 1)
        async def held_request():
            await peers.command(dict(action='att-hold', peer='tag'))
            target = tag.custom[0]
            pending = asyncio.create_task(remote.gatt_client.send_request(att.ATT_Find_Information_Request(
                starting_handle=target.handle + 1, ending_handle=target.end_group_handle)))
            await until(lambda: len(tag.att_held) == 1)
            assert not pending.done(), 'withheld request unexpectedly completed'
            return pending
        pending = await held_request()
        assert await peers.command(dict(action='att-release', peer='tag')) == dict(released=1, retired=0)
        response = await pending
        assert response.op_code == att.Opcode.ATT_FIND_INFORMATION_RESPONSE
        assert bytes(response)[-2:] == b'\x02\x29', 'Bumble did not return the real CCCD descriptor'
        report_map = await characteristic(0x2a4b).read_value()
        assert len(report_map) > 247 and tag.hid_reads >= 2
        reports = []
        await characteristic(0x2a4d).subscribe(reports.append)
        await tag.type_text('a')
        assert reports == [bytes([0, 0, 4, 0, 0, 0, 0, 0]), b'\0' * 8]
        before = await tag.key_status()
        pairings = tag.smp_pairings
        old_address = tag.device.random_address
        pending = await held_request()
        await tag.rotate()
        assert await peers.command(dict(action='att-release', peer='tag')) == dict(released=0, retired=1)
        pending.cancel()
        await asyncio.gather(pending, return_exceptions=True)
        assert old_address != tag.device.random_address
        connection = await central.connect(tag.device.random_address)
        await connection.encrypt()
        await until(lambda: len(tag.encryptions) == 2)
        assert tag.smp_pairings == pairings and await tag.key_status() == before
        assert (await tag.privacy())['identity'] == proof['identity']
        summaries.append(dict(scenario='gatt-indication-hogp-private-reconnect', status=copy.deepcopy(await tag.status()), privacy=proof))
        await peers.command(dict(action='stop-advertising', peer='tag'))
        await connection.disconnect()

        for name, io, sc, mitm in [('display', 'DISPLAY_OUTPUT_ONLY', True, True),
                                   ('remote', 'KEYBOARD_INPUT_ONLY', True, True),
                                   ('remote', 'KEYBOARD_INPUT_ONLY', False, True),
                                   ('remote', 'NO_OUTPUT_NO_INPUT', False, False)]:
            target = await configure(name, io, sc, mitm)
            for address, _ in await central.keystore.get_all():
                await central.keystore.delete(address)
            await peers.command(dict(action='forget-keys', peer=name))
            await target.advertise()
            connection = await central.connect(target.device.random_address)
            await connection.pair()
            assert connection.encryption
            before = target.smp_pairings
            await connection.disconnect()
            connection = await central.connect(target.device.random_address)
            await connection.encrypt()
            assert connection.encryption and target.smp_pairings == before
            summaries.append(dict(scenario=f'pair-{name}-{sc}-{mitm}', status=copy.deepcopy(await target.status())))
            await peers.command(dict(action='stop-advertising', peer=name))
            await connection.disconnect()

        # Both key-derivation directions cross actual controller transports.
        dual = await configure('dual', 'DISPLAY_OUTPUT_AND_YES_NO_INPUT', public=True)
        connection = await central.connect(dual.device.public_address, transport=core.PhysicalTransport.BR_EDR)
        await connection.authenticate()
        await connection.encrypt()
        # SMP on the encrypted BR/EDR bearer negotiates cross-transport keys.
        await connection.pair()
        before = await dual.key_status()
        assert any('ltk' in row['fingerprints'] and 'link_key' in row['fingerprints'] for row in before), before
        await dual.advertise()
        le = await central.connect(dual.device.public_address)
        await le.encrypt()
        assert le.encryption and await dual.key_status() == before
        summaries.append(dict(scenario='ctkd-bredr-to-le', status=copy.deepcopy(await dual.status())))
        await peers.command(dict(action='stop-advertising', peer='dual'))
        await le.disconnect()
        await connection.disconnect()
        for address, _ in await central.keystore.get_all():
            await central.keystore.delete(address)
        await peers.command(dict(action='forget-keys', peer='dual'))
        await central.refresh_resolving_list()
        await dual.advertise()
        le = await central.connect(dual.device.public_address)
        await le.pair()
        before = await dual.key_status()
        assert any('ltk' in row['fingerprints'] and 'link_key' in row['fingerprints'] for row in before), before
        connection = await dual.connection(str(central.public_address))
        assert connection.transport == core.PhysicalTransport.BR_EDR and connection.encryption and await dual.key_status() == before
        summaries.append(dict(scenario='ctkd-le-to-bredr', status=copy.deepcopy(await dual.status())))
        failures = [row for row in events if row['event'] == 'failure']
        assert not failures, failures
        summary = dict(status='passed', scope='host test equipment only; no Liber guest', scenarios=summaries,
                       same_team_fault_controls='real held ATT request: live release answered by Bumble; retired bearer discarded')
        (work / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
        print(json.dumps(dict(status='passed', scenarios=[row['scenario'] for row in summaries])), flush=True)
    finally:
        await peers.close()
        if transport:
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
    observations = []
    with (work / 'events.jsonl').open('w') as events, (work / 'rootcanal.log').open('w') as log:
        def emit(event, **values):
            row = dict(event=event, **values)
            observations.append(row)
            events.write(json.dumps(row) + '\n')
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
            await asyncio.wait_for(exercise(f'127.0.0.1:{ports[1]}', work, emit, observations), 55)
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
