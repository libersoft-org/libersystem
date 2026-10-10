"""Independent Bumble ordinary LE, HOGP, privacy and cross-transport peers.

Scenario values are ours; SMP, ATT/GATT, resolving addresses and link encryption
are Bumble/RootCanal. No guest result is inferred from a manufactured packet.
"""
import asyncio
import hashlib
from pathlib import Path

from bumble import core, device, gatt, hci, keys, pairing, rfcomm, sdp, smp
from bumble.transport import open_transport
from bluetooth_classic_peers import Peer, KEYBOARD_MAP


ADDRESSES = {'tag': 'F0:F1:F2:F3:F4:C1', 'display': 'F0:F1:F2:F3:F4:C2',
             'remote': 'F0:F1:F2:F3:F4:C3', 'dual': 'F0:F1:F2:F3:F4:C4'}


class GenericPeer(Peer):
    def __init__(self, *args):
        self.smp_pairings = 0
        self.classic_pairings = 0
        self.encryptions = []
        self.writes = []
        self.indications = 0
        self.hid_subscriptions = 0
        self.hid_reads = 0
        self.att_hold = False
        self.att_held = []
        self.att_requests = []
        self.advertising = None
        self.want_advertising = False
        self.advertising_restart = None
        self.sc, self.mitm = True, True
        super().__init__(*args)
        self.device.host.long_term_key_provider = self.legacy_responder_key
        delegate = self.pairing_delegate
        delegate.io_capability = pairing.PairingDelegate.DISPLAY_OUTPUT_AND_YES_NO_INPUT
        def configuration(connection):
            distribution = delegate.DEFAULT_KEY_DISTRIBUTION
            if self.name == 'dual' and connection.transport == core.PhysicalTransport.LE:
                distribution |= delegate.KeyDistribution.DISTRIBUTE_LINK_KEY
            delegate.local_initiator_key_distribution = distribution
            delegate.local_responder_key_distribution = distribution
            return pairing.PairingConfig(sc=self.sc, mitm=self.mitm, bonding=True, delegate=delegate,
                identity_address_type=pairing.PairingConfig.AddressType.PUBLIC if self.name == 'dual' else pairing.PairingConfig.AddressType.RANDOM)
        self.device.pairing_config_factory = configuration
        self.setup_gatt()
        self.att_dispatch = self.device.gatt_server.on_gatt_pdu
        self.device.gatt_server.on_gatt_pdu = self.controlled_att
        if self.name == 'dual':
            self.rfcomm_server = rfcomm.Server(self.device)
            self.rfcomm_server.listen(lambda dlc: setattr(dlc, 'sink', dlc.write), channel=3)
            self.device.sdp_service_records[0x10001] = rfcomm.make_service_sdp_records(
                0x10001, 3, core.UUID.from_16_bits(0x1101))

    def controlled_att(self, bearer, pdu):
        # Explicit same-team fault injection: retain one real request instead
        # of answering it. All released requests still use Bumble's ATT server.
        target = self.custom[0]
        if pdu.op_code == 0x12 and target.handle <= pdu.attribute_handle <= target.end_group_handle:
            self.att_requests.append(dict(handle=pdu.attribute_handle, value=pdu.attribute_value.hex(), encrypted=bool(bearer.encryption)))
            self.observe('generic-att-write', **self.att_requests[-1])
        if (self.att_hold and pdu.op_code == 0x04 and
                pdu.starting_handle == target.handle + 1 and pdu.ending_handle == target.end_group_handle):
            self.att_hold = False
            self.att_held.append((bearer, pdu))
            self.observe('generic-att-held', connection=bearer.handle, request=bytes(pdu).hex())
            return
        self.att_dispatch(bearer, pdu)

    def release_att(self):
        released = dropped = 0
        held, self.att_held = self.att_held, []
        self.att_hold = False
        for bearer, pdu in held:
            if self.connections.get(bearer.handle) is bearer:
                self.att_dispatch(bearer, pdu)
                released += 1
            else:
                # A retired connection is never replaced by one that happened
                # to reuse its numeric HCI handle.
                dropped += 1
        result = dict(released=released, retired=dropped)
        self.observe('generic-att-released', **result)
        return result

    async def legacy_responder_key(self, handle, rand, ediv):
        # Bumble 0.0.235 stores the responder's distributed key in ltk_central
        # on BOTH sides (smp.Session.on_pairing_complete), but Device's lookup
        # selects ltk_peripheral on the responder. Use the actual distributed
        # key and identifiers, without changing SMP, derivation or HCI verdicts.
        connection = self.device.lookup_connection(handle)
        if connection and connection.role == hci.Role.PERIPHERAL:
            active = self.device.smp_manager.get_long_term_key(connection, rand, ediv)
            if active is not None:
                return active
            stored = await self.device.keystore.get(str(connection.peer_address))
            if stored and stored.ltk is None and (key := stored.ltk_central) is not None:
                return key.value if key.rand == rand and key.ediv == ediv else None
        return await self.device.get_long_term_key(handle, rand, ediv)

    def connected(self, connection):
        self.connections[connection.handle] = connection
        self.observe('generic-connected', handle=connection.handle, transport=connection.transport.name,
                     remote=str(connection.peer_address), rpa=str(connection.peer_resolvable_address) if connection.peer_resolvable_address else None)
        connection.on('disconnection', lambda reason: self.disconnected(connection, reason))
        connection.on(connection.EVENT_PAIRING, lambda stored: self.paired_le(connection, stored))
        connection.on(connection.EVENT_CLASSIC_PAIRING, self.paired_classic)
        connection.on(connection.EVENT_PAIRING_FAILURE, lambda reason: self.observe('generic-pairing-failure', reason=int(reason)))
        connection.on(connection.EVENT_CONNECTION_ENCRYPTION_CHANGE, lambda: self.encryption(connection))

    def paired_classic(self):
        self.classic_pairings += 1
        self.observe('generic-classic-paired', count=self.classic_pairings)

    def disconnected(self, connection, reason):
        super().disconnected(connection, reason)
        if self.want_advertising and connection.transport == core.PhysicalTransport.LE:
            self.advertising_restart = self.spawn(self.advertise())

    def paired_le(self, connection, stored):
        self.smp_pairings += 1
        self.observe('generic-smp-paired', count=self.smp_pairings, transport=connection.transport.name,
                     has_ltk=stored.ltk is not None or stored.ltk_peripheral is not None,
                     has_link_key=stored.link_key is not None, has_irk=stored.irk is not None)

    def encryption(self, connection):
        if connection.encryption:
            self.encrypted += 1
            observation = dict(transport=connection.transport.name, handle=connection.handle,
                               remote=str(connection.peer_address),
                               rpa=str(connection.peer_resolvable_address) if connection.peer_resolvable_address else None)
            self.encryptions.append(observation)
            self.observe('generic-encrypted', **observation)

    def setup_gatt(self):
        c = gatt.Characteristic
        encrypted = 'READABLE,READ_REQUIRES_ENCRYPTION'
        battery = c(core.UUID.from_16_bits(0x2a19), c.READ, encrypted, bytes([87]))
        self.device.add_service(gatt.Service(core.UUID.from_16_bits(0x180f), [battery]))
        self.custom = []
        for uuid, mode in ((0xfff1, c.NOTIFY), (0xfff2, c.INDICATE)):
            def written(connection, value, index=len(self.custom)):
                self.writes.append(dict(handle=self.custom[index].handle, value=value.hex(), encrypted=bool(connection.encryption)))
                self.observe('generic-write', **self.writes[-1])
                self.spawn(self.echo_value(connection, index, value))
            characteristic = c(core.UUID.from_16_bits(uuid), c.READ | c.WRITE | mode,
                               encrypted + ',WRITEABLE,WRITE_REQUIRES_ENCRYPTION',
                               gatt.CharacteristicValue(read=lambda _: b'fixture', write=written))
            self.custom.append(characteristic)
        self.device.add_service(gatt.Service(core.UUID.from_16_bits(0xfff0), self.custom))
        # Balanced global Push/Pop makes the valid map exceed one 247-byte ATT
        # response. The keyboard data itself uses the shared HID descriptor.
        report_map = KEYBOARD_MAP[:-1] + b'\xa4\xb4' * 128 + KEYBOARD_MAP[-1:]
        def read_map(_):
            self.hid_reads += 1
            return report_map
        self.hid = c(core.UUID.from_16_bits(0x2a4d), c.READ | c.NOTIFY, encrypted, b'\0' * 8,
                     descriptors=[gatt.Descriptor(core.UUID.from_16_bits(0x2908), 'READABLE', b'\0\x01')])
        self.hid.on(self.hid.EVENT_SUBSCRIPTION, self.hid_subscription)
        self.device.add_service(gatt.Service(core.UUID.from_16_bits(0x1812), [
            c(core.UUID.from_16_bits(0x2a4a), c.READ, encrypted, b'\x11\x01\0\x02'),
            c(core.UUID.from_16_bits(0x2a4b), c.READ, encrypted, gatt.CharacteristicValue(read=read_map)),
            c(core.UUID.from_16_bits(0x2a4e), c.READ | c.WRITE_WITHOUT_RESPONSE,
              encrypted + ',WRITEABLE,WRITE_REQUIRES_ENCRYPTION', b'\x01'),
            c(core.UUID.from_16_bits(0x2a4c), c.WRITE_WITHOUT_RESPONSE,
              'WRITEABLE,WRITE_REQUIRES_ENCRYPTION', b'\0'), self.hid]))

    def hid_subscription(self, connection, notify, indicate):
        self.hid_subscriptions += bool(notify)
        self.observe('generic-hid-subscription', encrypted=bool(connection.encryption), notify=bool(notify))

    async def echo_value(self, connection, index, value):
        # Let Bumble send the write response before a notification/indication.
        await asyncio.sleep(.02)
        if index == 0:
            await self.device.notify_subscriber(connection, self.custom[index], value)
        else:
            await self.device.indicate_subscriber(connection, self.custom[index], value)
            self.indications += 1
            self.observe('generic-indication-confirmed', handle=self.custom[index].handle)

    async def advertise(self):
        self.want_advertising = True
        if self.advertising is None:
            public = self.name == 'dual'
            data = bytes(core.AdvertisingData([
                (core.AdvertisingData.FLAGS, bytes([core.AdvertisingData.LE_GENERAL_DISCOVERABLE_MODE_FLAG])),
                (core.AdvertisingData.COMPLETE_LOCAL_NAME, ('Bumble ' + self.name).encode()),
                (core.AdvertisingData.COMPLETE_LIST_OF_16_BIT_SERVICE_CLASS_UUIDS, b'\x12\x18\xf0\xff')]))
            self.advertising = await self.device.create_advertising_set(
                random_address=None if public else self.device.random_address,
                advertising_parameters=device.AdvertisingParameters(
                    own_address_type=hci.OwnAddressType.PUBLIC if public else hci.OwnAddressType.RANDOM,
                    primary_advertising_interval_min=100, primary_advertising_interval_max=100),
                advertising_data=data, auto_restart=False)
        elif not self.advertising.enabled:
            await self.advertising.start()

    async def stop_advertising(self):
        # A disconnect may already have queued an automatic restart. Let that
        # actual HCI operation settle before disabling/changing the address;
        # cancellation would leave an uncertain controller-side enable behind.
        self.want_advertising = False
        if self.advertising_restart is not None:
            restart, self.advertising_restart = self.advertising_restart, None
            await restart
            self.want_advertising = False
        if self.advertising:
            self.advertising.auto_restart = False
            if self.advertising.enabled:
                await self.advertising.stop()

    async def rotate(self):
        if self.name not in ('tag', 'remote'):
            raise ValueError('privacy rotation is offered by the tag and the explicit legacy remote')
        await self.stop_advertising()
        for connection in list(self.connections.values()):
            await connection.disconnect()
        address = hci.Address.generate_private_address(self.device.irk)
        # Program the actual new address explicitly. Pinned Bumble.update_rpa()
        # sends its previous address before updating the Python field.
        self.device.random_address = address
        await self.device.send_sync_command(hci.HCI_LE_Set_Random_Address_Command(random_address=address))
        if self.advertising:
            await self.advertising.set_random_address(address)
            self.advertising.random_address = address
        await self.advertise()
        return str(address)

    async def type_text(self, text):
        for char in text:
            if 'a' <= char <= 'z':
                usage = ord(char) - ord('a') + 4
            elif '0' <= char <= '9':
                usage = ord(char) - ord('1') + 30 if char != '0' else 39
            elif char in ' \n-':
                usage = {' ': 44, '\n': 40, '-': 45}[char]
            else:
                raise ValueError(f'unsupported keyboard character {char!r}')
            await self.device.notify_subscribers(self.hid, bytes([0, 0, usage, 0, 0, 0, 0, 0]))
            await asyncio.sleep(.04)
            await self.device.notify_subscribers(self.hid, b'\0' * 8)
            await asyncio.sleep(.04)

    async def privacy(self):
        if len(self.connections) != 1:
            raise ValueError('privacy proof needs one actual LE connection')
        connection = next(iter(self.connections.values()))
        address = connection.peer_resolvable_address or connection.peer_address
        resolver = smp.AddressResolver(await self.device.keystore.get_resolving_keys())
        identity = resolver.resolve(address)
        return dict(remote=str(address), is_private=address.is_resolvable,
                    identity=str(identity) if identity else None, encrypted=bool(connection.encryption))

    async def server_values(self):
        connection = next(connection for connection in self.connections.values() if connection.transport == core.PhysicalTransport.LE)
        peer = device.Peer(connection)
        services = await peer.discover_services()
        names = await peer.read_characteristics_by_uuid(gatt.GATT_DEVICE_NAME_CHARACTERISTIC)
        appearances = await peer.read_characteristics_by_uuid(gatt.GATT_APPEARANCE_CHARACTERISTIC)
        return dict(services=[str(service.uuid) for service in services], names=[value.decode() for value in names], appearances=[value.hex() for value in appearances])

    async def key_status(self):
        result = []
        for address, stored in await self.device.keystore.get_all():
            fingerprints = {name: hashlib.sha256(key.value).hexdigest() for name in ('ltk', 'ltk_central', 'ltk_peripheral', 'link_key', 'irk') if (key := getattr(stored, name)) is not None}
            result.append(dict(address=address, fingerprints=fingerprints))
        return result

    async def status(self):
        return dict(address=str(self.device.public_address if self.name == 'dual' else self.device.static_address),
                    advertising_address=str(self.device.public_address if self.name == 'dual' else self.device.random_address),
                    connections=[dict(handle=c.handle, transport=c.transport.name, encrypted=bool(c.encryption),
                                      hid_notify=bool(self.device.gatt_server.read_cccd(c, self.hid)[0] & 1)) for c in self.connections.values()],
                    smp_pairings=self.smp_pairings, classic_pairings=self.classic_pairings,
                    encryptions=self.encryptions, keys=await self.key_status(), writes=self.writes,
                    indications=self.indications, hid_subscriptions=self.hid_subscriptions, hid_reads=self.hid_reads,
                    att_requests=self.att_requests, att_hold=self.att_hold,
                    att_held=[dict(connection=bearer.handle, request=bytes(pdu).hex(),
                                   connected=self.connections.get(bearer.handle) is bearer) for bearer, pdu in self.att_held])


class GenericPeers:
    def __init__(self, peers):
        self.peers = {peer.name: peer for peer in peers}

    async def command(self, request):
        action = request['action']
        if action == 'status':
            return {name: await peer.status() for name, peer in self.peers.items()}
        peer = self.peers[request['peer']]
        if action == 'advertise':
            await peer.advertise()
        elif action == 'stop-advertising':
            await peer.stop_advertising()
        elif action == 'pairing-config':
            peer.sc, peer.mitm = request.get('sc', True), request.get('mitm', True)
            peer.pairing_delegate.number = None
            peer.pairing_delegate.io_capability = pairing.PairingDelegate.IoCapability[request['io']]
            peer.pairing_delegate.accepted = request.get('accept', True)
        elif action == 'pairing-number':
            peer.pairing_delegate.number = request['number']
        elif action == 'type':
            await peer.type_text(request['text'])
        elif action == 'report':
            await peer.device.notify_subscribers(peer.hid, bytes.fromhex(request['data']))
        elif action == 'rotate':
            address = await peer.rotate()
            return dict(address=address, is_resolvable=peer.device.random_address.is_resolvable,
                        identity=str(peer.device.static_address))
        elif action == 'privacy':
            return await peer.privacy()
        elif action == 'server-values':
            return await peer.server_values()
        elif action == 'att-hold':
            if peer.att_held or peer.att_hold:
                raise ValueError('an ATT hold is already pending')
            peer.att_hold = True
        elif action == 'att-release':
            return peer.release_att()
        elif action == 'disconnect':
            for connection in list(peer.connections.values()):
                await connection.disconnect()
        elif action == 'forget-keys':
            for address, _ in await peer.device.keystore.get_all():
                await peer.device.keystore.delete(address)
            await peer.device.refresh_resolving_list()
        elif action == 'save-keys':
            peer.saved_keys = await peer.device.keystore.get_all()
        elif action == 'restore-keys':
            for address, _ in await peer.device.keystore.get_all():
                await peer.device.keystore.delete(address)
            for address, stored in peer.saved_keys:
                await peer.device.keystore.update(address, stored)
            await peer.device.refresh_resolving_list()
        elif action == 'connect-classic':
            await peer.connection(request['address'])
        else:
            raise ValueError(f'unknown ordinary LE action: {action}')
        return await peer.status()

    async def close(self):
        for peer in self.peers.values():
            peer.want_advertising = False
        await asyncio.gather(*(peer.close() for peer in self.peers.values()))


async def create_generic_le_peers(endpoint, work: Path, emit):
    peers = []
    try:
        for name, address in ADDRESSES.items():
            transport = await open_transport(f'tcp-client:{endpoint}')
            config = device.DeviceConfiguration.from_dict(dict(name='Bumble ' + name, address=address,
                classic_enabled=name == 'dual', classic_sc_enabled=True, le_enabled=True,
                le_privacy_enabled=name == 'tag', le_rpa_timeout=0))
            instance = device.Device.from_config_with_hci(config, transport.source, transport.sink)
            instance.keystore = keys.JsonKeyStore(namespace=name, filename=str(work / f'{name}-keys.json'))
            peer = GenericPeer(name, transport, instance, work, emit)
            peers.append(peer)
            await instance.power_on()
            if name == 'dual':
                await instance.set_discoverable(True)
                await instance.set_connectable(True)
            emit('generic-ready', peer=name, **await peer.status())
        return GenericPeers(peers)
    except BaseException:
        await asyncio.gather(*(peer.close() for peer in peers), return_exceptions=True)
        raise
