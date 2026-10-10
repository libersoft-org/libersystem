"""Independent Bumble classic peers for P02M0194's production USB guest gate.

Only scenario data and observations live here. Bumble owns SDP, L2CAP, HID,
RFCOMM, AVDTP, AVRCP, HFP and pairing; RootCanal owns the radio/controller.
"""
import asyncio
import hashlib
import importlib.util
import math
import struct
from pathlib import Path

from bumble import a2dp, avc, avdtp, avrcp, core, device, hci, hfp, hid, keys, pairing, rfcomm, sdp
from bumble.transport import open_transport
from bluetooth_transfer_peers import TransferPeer


def codec_oracle():
    # The host codec gate owns these thin ABIs to pinned, separately written C codecs.
    # Importing it never invokes its main() or the shipping Rust adapter.
    path = Path(__file__).resolve().parents[1] / 'tools/check-bluetooth-codecs.py'
    spec = importlib.util.spec_from_file_location('independent_codecs', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def pcm_tone(rate, count, channels, frequency=1000, start=0):
    return struct.pack(f'<{count * channels}h', *(
        round(5000 * math.sin(2 * math.pi * frequency * (start + i) / rate))
        for i in range(count) for _ in range(channels)))


def audio_metrics(pcm, rate, channels, channel=0):
    samples = struct.unpack(f'<{len(pcm) // 2}h', pcm)[channel::channels]
    # Use a complete half-second window after decoder/filter start-up.
    samples = samples[-rate // 2:]
    if not samples:
        return dict(samples=0, rms=0, peak_frequency=0)
    energy = sum(x * x for x in samples) / len(samples)
    powers = {frequency: abs(sum(x * complex(math.cos(2 * math.pi * frequency * i / rate), math.sin(2 * math.pi * frequency * i / rate)) for i, x in enumerate(samples))) ** 2
              for frequency in (333, 500, 667, 1000, 1500, 2000, 2500, 3000)}
    return dict(samples=len(pcm) // 2 // channels, rms=math.sqrt(energy), peak_frequency=max(powers, key=powers.get))


KEYBOARD_MAP = bytes.fromhex('05010906a101050719e029e71500250175019508810295017508810195067508150025660507190029668100c0')
GAMEPAD_MAP = bytes.fromhex('05010905a101050919012908150025017501950881020501093009311581257f7508950281020939150025073500463b016514750495018142750495018101c0')


def hid_record(handle, report_map, keyboard):
    """Scenario descriptor values; Bumble serializes and serves the SDP record."""
    d = sdp.DataElement
    attributes = {
        0x0000: d.unsigned_integer_32(handle),
        0x0001: d.sequence([d.uuid(core.BT_HUMAN_INTERFACE_DEVICE_SERVICE)]),
        0x0004: d.sequence([d.sequence([d.uuid(core.BT_L2CAP_PROTOCOL_ID), d.unsigned_integer_16(hid.HID_CONTROL_PSM)]), d.sequence([d.uuid(core.BT_HIDP_PROTOCOL_ID)])]),
        0x0009: d.sequence([d.sequence([d.uuid(core.BT_HUMAN_INTERFACE_DEVICE_SERVICE), d.unsigned_integer_16(0x0111)])]),
        0x000d: d.sequence([d.sequence([d.sequence([d.uuid(core.BT_L2CAP_PROTOCOL_ID), d.unsigned_integer_16(hid.HID_INTERRUPT_PSM)]), d.sequence([d.uuid(core.BT_HIDP_PROTOCOL_ID)])])]),
        0x0100: d(sdp.DataElement.TEXT_STRING, b'Independent Bumble HID'),
        0x0201: d.unsigned_integer_16(0x0111),
        0x0202: d.unsigned_integer_8(0x40 if keyboard else 0x08),
        0x0204: d.boolean(True),
        0x0205: d.boolean(True),
        0x0206: d.sequence([d.sequence([d.unsigned_integer_8(0x22), d(sdp.DataElement.TEXT_STRING, report_map)])]),
        0x020b: d.unsigned_integer_16(0x0111),
        0x020d: d.boolean(True),
        0x020e: d.boolean(keyboard),
    }
    return [sdp.ServiceAttribute(number, value) for number, value in sorted(attributes.items())]


def codec_capabilities():
    info = a2dp.SbcMediaCodecInformation
    return avdtp.MediaCodecCapabilities(
        media_type=avdtp.AVDTP_AUDIO_MEDIA_TYPE,
        media_codec_type=a2dp.A2DP_SBC_CODEC_TYPE,
        media_codec_information=info(
            sampling_frequency=info.SamplingFrequency.SF_48000,
            channel_mode=info.ChannelMode.JOINT_STEREO,
            block_length=info.BlockLength.BL_16,
            subbands=info.Subbands.S_8,
            allocation_method=info.AllocationMethod.LOUDNESS,
            minimum_bitpool_value=2, maximum_bitpool_value=53,
        ),
    )


class Peer:
    def __init__(self, name, transport, instance, work, emit):
        self.name, self.transport, self.device = name, transport, instance
        self.work, self.emit = work, emit
        self.connections = {}
        self.pairings = 0
        self.encrypted = 0
        self.received = bytearray()
        self.tasks = set()
        self.pairing_result = None
        self.pairing_delegate = ScenarioPairing(self)
        instance.on('connection', self.connected)

    def observe(self, event, **values):
        self.emit(event, peer=self.name, **values)

    def spawn(self, operation):
        task = asyncio.create_task(operation)
        self.tasks.add(task)
        def finished(task):
            self.tasks.discard(task)
            if not task.cancelled() and task.exception() is not None:
                self.observe('failure', error=repr(task.exception()))
        task.add_done_callback(finished)
        return task

    def connected(self, connection):
        self.connections[connection.handle] = connection
        self.observe('connection', address=str(connection.peer_address), handle=connection.handle)
        connection.on('disconnection', lambda reason: self.disconnected(connection, reason))
        connection.on('classic_pairing', self.paired)
        connection.on('pairing_failure', lambda reason: self.observe('pairing-failure', reason=int(reason)))
        connection.on('classic_pairing_failure', lambda reason: self.observe('pairing-failure', reason=int(reason)))
        connection.on('connection_encryption_change', lambda: self.encryption(connection))

    def paired(self):
        self.pairings += 1
        self.observe('paired', count=self.pairings)

    def encryption(self, connection):
        self.encrypted += bool(connection.encryption)
        self.observe('encryption', enabled=bool(connection.encryption), count=self.encrypted)

    def disconnected(self, connection, reason):
        self.connections.pop(connection.handle, None)
        self.observe('disconnection', reason=int(reason))

    def status(self):
        return dict(name=self.name, address=str(self.device.public_address), connections=len(self.connections), pairings=self.pairings, encrypted=self.encrypted, bytes_received=len(self.received))

    async def connection(self, address):
        address = hci.Address.from_string_for_transport(address, core.PhysicalTransport.BR_EDR)
        for connection in self.connections.values():
            if connection.transport == core.PhysicalTransport.BR_EDR and connection.peer_address == address:
                return connection
        connection = await self.device.connect(address, transport=core.PhysicalTransport.BR_EDR)
        await connection.authenticate()
        await connection.encrypt()
        return connection

    async def initiate_pairing(self, address):
        try:
            connection = await self.connection(address)
            self.pairing_result = dict(ok=True, encrypted=bool(connection.encryption))
        except core.BaseError as error:
            self.pairing_result = dict(ok=False, error_namespace=error.error_namespace,
                                       error_code=error.error_code, error_name=error.error_name)
        except Exception as error:
            self.pairing_result = dict(ok=False, error=repr(error))
        self.observe('pairing-result', **self.pairing_result)

    async def close(self):
        for task in list(self.tasks):
            task.cancel()
        await asyncio.gather(*self.tasks, return_exceptions=True)
        if hasattr(self, 'transfer'):
            await self.transfer.close()
        await self.transport.close()


class ScenarioPairing(pairing.PairingDelegate):
    def __init__(self, peer):
        super().__init__()
        self.peer = peer
        self.number = None
        self.pin = '1234'
        self.accepted = True

    async def compare_numbers(self, number, digits):
        self.peer.observe('pairing-number', number=number, digits=digits)
        return self.accepted

    async def confirm(self, auto=False):
        self.peer.observe('pairing-confirm', automatic=auto)
        return self.accepted

    async def display_number(self, number, digits):
        self.peer.observe('pairing-display', number=number, digits=digits)

    async def get_number(self):
        self.peer.observe('pairing-input-required')
        for _ in range(600):
            if self.number is not None:
                return self.number
            await asyncio.sleep(.05)
        return None

    async def get_string(self, max_length):
        self.peer.observe('pairing-pin-required')
        return self.pin[:max_length]


class HidPeer(Peer):
    def setup(self, keyboard):
        self.keyboard = keyboard
        self.hid = hid.Device(self.device)
        self.device.sdp_service_records[0x10001] = hid_record(0x10001, KEYBOARD_MAP if keyboard else GAMEPAD_MAP, keyboard)
        self.protocol = 1
        self.hid.register_get_protocol_cb(lambda: hid.Device.GetSetStatus(status=hid.Device.GetSetReturn.SUCCESS, data=bytes([self.protocol])))
        self.hid.register_set_protocol_cb(self.set_protocol)
        self.hid.register_get_report_cb(lambda *_: hid.Device.GetSetStatus(status=hid.Device.GetSetReturn.SUCCESS, data=b'\0' * (8 if keyboard else 4)))

    def set_protocol(self, protocol):
        self.protocol = protocol
        self.observe('hid-protocol', protocol=protocol)
        return hid.Device.GetSetStatus(status=hid.Device.GetSetReturn.SUCCESS)

    async def reconnect(self, address):
        await self.connection(address)
        await self.hid.connect_control_channel()
        await self.hid.connect_interrupt_channel()

    async def report(self, data):
        if self.hid.l2cap_intr_channel is None:
            raise RuntimeError('HID interrupt channel is not open')
        self.hid.send_data(data)
        self.observe('hid-report', data=data.hex())
        await asyncio.sleep(.04)

    async def type_text(self, text):
        for char in text:
            if 'a' <= char <= 'z':
                usage, modifier = ord(char) - ord('a') + 4, 0
            elif '0' <= char <= '9':
                usage, modifier = (ord(char) - ord('1') + 30) if char != '0' else 39, 0
            elif char in ' \n-':
                usage, modifier = {' ': 44, '\n': 40, '-': 45}[char], 0
            else:
                raise ValueError(f'unsupported scenario character {char!r}')
            await self.report(bytes([modifier, 0, usage, 0, 0, 0, 0, 0]))
            await self.report(b'\0' * 8)


class SerialPeer(Peer):
    def setup(self):
        self.server = rfcomm.Server(self.device)
        channel = self.server.listen(self.accept, channel=3)
        self.device.sdp_service_records[0x10001] = rfcomm.make_service_sdp_records(0x10001, channel, core.UUID.from_16_bits(0x1101))

    def accept(self, dlc):
        self.dlc = dlc
        self.observe('spp-open')

        def data_received(data):
            self.received.extend(data)
            dlc.write(data)
            self.observe('spp-data', length=len(data), total=len(self.received), digest=hashlib.sha256(self.received).hexdigest())

        dlc.sink = data_received


class VolumeDelegate(avrcp.Delegate):
    def __init__(self, peer):
        super().__init__(supported_events=[avrcp.EventId.VOLUME_CHANGED, avrcp.EventId.PLAYBACK_STATUS_CHANGED, avrcp.EventId.TRACK_CHANGED])
        self.peer, self.volume = peer, 64
        self.playback_status = avrcp.PlayStatus.PLAYING
        self.current_track_uid = 1

    async def set_absolute_volume(self, volume):
        await super().set_absolute_volume(volume)
        self.peer.observe('volume', value=volume)

    async def on_key_event(self, key, pressed, data):
        self.peer.observe('media-key', key=int(key), pressed=pressed)


class ScenarioAvrcp(avrcp.Protocol):
    """Supply track scenario data through Bumble's command parser/response codec.

    Bumble provides GetElementAttributes types but its target dispatcher has no
    metadata delegate. Only that dispatch is supplied here; framing, transactions,
    continuation, notification registration and parsing remain Bumble's.
    """
    def _on_command_pdu(self, pdu_id, pdu):
        if pdu_id != avrcp.PduId.GET_ELEMENT_ATTRIBUTES:
            return super()._on_command_pdu(pdu_id, pdu)
        command = avrcp.Command.from_bytes(pdu_id, pdu)
        metadata = {avrcp.MediaAttributeId.TITLE: 'Independent radio tone', avrcp.MediaAttributeId.ARTIST_NAME: 'Bumble oracle', avrcp.MediaAttributeId.ALBUM_NAME: 'P02M0194'}
        requested = command.attribute_ids or list(metadata)
        attributes = [avrcp.MediaAttribute(identifier, 106, metadata.get(identifier, '')) for identifier in requested]
        self.send_avrcp_response(self.receive_command_state.transaction_label, avc.ResponseFrame.ResponseCode.IMPLEMENTED_OR_STABLE, avrcp.GetElementAttributesResponse(attributes))
        self.receive_command_state = None


class DelayReportingSink(avdtp.LocalSink):
    """Bumble's sink, with a non-default 180 ms delay before stream opening."""
    def __init__(self, protocol, peer):
        super().__init__(protocol, len(protocol.local_endpoints) + 1, codec_capabilities())
        self.peer = peer
        self.capabilities.append(avdtp.ServiceCapabilities(avdtp.AVDTP_DELAY_REPORTING_SERVICE_CATEGORY))

    async def on_open_command(self):
        result = await super().on_open_command()
        if result is None:
            response = await self.protocol.send_command(avdtp.DelayReport_Command(self.stream.remote_endpoint.seid, 1800))
            if not isinstance(response, avdtp.DelayReport_Response):
                raise AssertionError(f'guest refused the negotiated AVDTP delay report: {response}')
            self.peer.observe('avdtp-delay', tenths_ms=1800)
        return result


class AudioPeer(Peer):
    def setup(self, phone=False):
        self.phone = phone
        records = self.device.sdp_service_records
        factory = a2dp.make_audio_source_service_sdp_records if phone else a2dp.make_audio_sink_service_sdp_records
        records[0x10001] = factory(0x10001)
        records[0x10002] = avrcp.ControllerServiceSdpRecord(0x10002).to_service_attributes()
        records[0x10003] = avrcp.TargetServiceSdpRecord(0x10003).to_service_attributes()
        self.volume = VolumeDelegate(self)
        self.avrcp = ScenarioAvrcp(delegate=self.volume)
        self.avrcp.listen(self.device)
        self.avrcp.on('start', lambda: self.observe('avrcp-start'))
        self.avdtp = None
        self.media_packets = []
        self.listener = avdtp.Listener.for_device(self.device)
        self.listener.on('connection', self.avdtp_connection)
        self.oracle = codec_oracle()
        self.sco_frames = []
        self.sco_buffer = bytearray()
        self.sco_link = None
        self.hold_sco_accept = False
        self.held_sco_request = None
        self.microphone_reference = None
        if not phone:
            self.hf_configuration = hfp.HfConfiguration(
                supported_hf_features=[hfp.HfFeature.REMOTE_VOLUME_CONTROL, hfp.HfFeature.CODEC_NEGOTIATION, hfp.HfFeature.HF_INDICATORS, hfp.HfFeature.ESCO_S4_SETTINGS_SUPPORTED],
                supported_hf_indicators=[hfp.HfIndicator.BATTERY_LEVEL],
                supported_audio_codecs=[hfp.AudioCodec.CVSD, hfp.AudioCodec.MSBC])
            self.rfcomm_server = rfcomm.Server(self.device)
            channel = self.rfcomm_server.listen(self.hfp_connected, channel=4)
            records[0x10004] = hfp.make_hf_sdp_records(0x10004, channel, self.hf_configuration)
        else:
            self.transfer = TransferPeer(self)

    def avdtp_connection(self, protocol):
        self.avdtp = protocol
        if not self.phone:
            self.sink = DelayReportingSink(protocol, self)
            protocol.local_endpoints.append(self.sink)
            self.sink.on('rtp_packet', self.media)
        else:
            self.add_source(protocol)
        self.observe('avdtp-connected')

    def media(self, packet):
        self.media_packets.append(packet.payload)
        self.received.extend(packet.payload)
        with (self.work / f'{self.name}-rtp.bin').open('ab') as output:
            output.write(struct.pack('<H', len(packet.payload)) + packet.payload)
        if len(self.media_packets) % 25 == 1:
            self.observe('a2dp-media', packets=len(self.media_packets), length=len(packet.payload))

    def add_source(self, protocol):
        frames = [pcm_tone(48000, 128, 2, start=128 * i) for i in range(7500)]
        encoded = self.oracle.Sbc().encode((48000, 16, 3, 0, 8, 53), frames, False)
        self.source_data = b''.join(encoded)
        offset = 0

        async def read(count):
            nonlocal offset
            data = self.source_data[offset:offset + count]
            offset += len(data)
            return data

        packets = a2dp.SbcPacketSource(read, protocol.l2cap_channel.peer_mtu)
        self.source = protocol.add_source(codec_capabilities(), avdtp.MediaPacketPump(packets.packets))

    async def source_start(self, address):
        if self.avdtp is None:
            connection = await self.connection(address)
            self.avdtp = await avdtp.Protocol.connect(connection)
            self.add_source(self.avdtp)
        await self.avdtp.discover_remote_endpoints()
        sink = self.avdtp.find_remote_sink_by_codec(avdtp.AVDTP_AUDIO_MEDIA_TYPE, a2dp.A2DP_SBC_CODEC_TYPE)
        if sink is None:
            raise RuntimeError('guest did not advertise an SBC sink')
        self.source_stream = await self.avdtp.create_stream(self.source, sink)
        await self.source_stream.start()
        self.observe('a2dp-source-started', codec='AOSP SBC', bytes=len(self.source_data))

    async def hfp_connect(self, address, codec):
        self.hf_configuration.supported_audio_codecs = [hfp.AudioCodec.CVSD] if codec == 'cvsd' else [hfp.AudioCodec.CVSD, hfp.AudioCodec.MSBC]
        connection = await self.connection(address)
        self.rfcomm_client = rfcomm.Client(connection)
        await self.rfcomm_client.start()
        dlc = await self.rfcomm_client.multiplexer.open_dlc(1)
        self.hfp_connected(dlc)
        await self.hf.initiate_slc()
        self.observe('hfp-slc', codec=str(self.hf.active_codec))
        self.spawn(self.hf.run())

    async def hsp_connect(self, address):
        connection = await self.connection(address)
        channel = await rfcomm.find_rfcomm_channel_with_uuid(connection, core.UUID.from_16_bits(0x1112))
        if channel != 2:
            raise AssertionError(f'guest HSP Audio Gateway SDP record has channel {channel}, expected 2')
        self.rfcomm_client = rfcomm.Client(connection)
        await self.rfcomm_client.start()
        dlc = await self.rfcomm_client.multiplexer.open_dlc(channel)
        # HSP shares the AT framing/unsolicited RING and gain parser, but does
        # not execute HFP's SLC, indicators or codec negotiation commands.
        self.hfp_connected(dlc)
        self.observe('hsp-connected', channel=channel, codec=int(self.hf.active_codec))
        self.spawn(self.hf.run())

    def status(self):
        return dict(super().status(), sco=bool(self.sco_link and self.sco_link.handle in self.device.sco_links), sco_request_held=self.held_sco_request is not None)

    def disconnected(self, connection, reason):
        if self.held_sco_request is not None and self.held_sco_request[0] is connection:
            self.held_sco_request = None
            self.observe('sco-request-discarded', handle=connection.handle)
        super().disconnected(connection, reason)

    def hfp_connected(self, dlc):
        self.hf = hfp.HfProtocol(dlc, self.hf_configuration)
        connection = dlc.multiplexer.l2cap_channel.connection
        connection.on('sco_request', lambda kind: self.sco_request(connection, kind))
        connection.on('sco_connection', self.sco_connected)
        self.hf.on('codec_negotiation', lambda codec: self.observe('hfp-codec', codec=int(codec)))
        self.hf.on('ring', lambda: self.observe('hfp-ring'))
        self.hf.on('ag_indicator', lambda indicator: self.observe('hfp-indicator', name=indicator.indicator.value, value=indicator.current_status))
        self.hf.on('speaker_volume', lambda volume: self.observe('hfp-volume', value=volume))
        self.hf.on('microphone_volume', lambda volume: self.observe('hfp-microphone-volume', value=volume))

    def sco_request(self, connection, kind):
        # Same-team scheduling control: Bumble still constructs the real accept command and
        # RootCanal supplies the actual connection completion. No guest event is fabricated.
        if self.hold_sco_accept:
            if self.held_sco_request is not None:
                raise AssertionError('a second SCO request arrived while one acceptance was held')
            self.held_sco_request = (connection, kind)
            self.observe('sco-request-held', handle=connection.handle)
            return
        self.accept_sco(connection, kind)

    def release_sco_request(self):
        if self.held_sco_request is None:
            raise AssertionError('no SCO acceptance is held')
        connection, kind = self.held_sco_request
        self.held_sco_request = None
        self.hold_sco_accept = False
        if self.connections.get(connection.handle) is not connection:
            raise AssertionError('the held SCO request belongs to a departed ACL connection')
        self.accept_sco(connection, kind)

    def accept_sco(self, connection, kind):
        codec = self.hf.active_codec
        selection = hfp.DefaultCodecParameters.ESCO_MSBC_T2 if codec == hfp.AudioCodec.MSBC else hfp.DefaultCodecParameters.ESCO_CVSD_S4
        if kind == hci.HCI_Connection_Complete_Event.LinkType.SCO:
            selection = hfp.DefaultCodecParameters.SCO_CVSD_D1
        parameters = hfp.ESCO_PARAMETERS[selection]
        self.spawn(self.device.send_async_command(hci.HCI_Enhanced_Accept_Synchronous_Connection_Request_Command(bd_addr=connection.peer_address, **parameters.asdict())))

    def sco_connected(self, link):
        self.sco_link = link
        self.sco_buffer.clear()
        self.sco_frames.clear()
        self.observe('sco-connected', handle=link.handle, air_mode=int(link.air_mode), codec=int(self.hf.active_codec))
        link.sink = self.sco_data
        link.on('disconnection', lambda reason: self.observe('sco-disconnected', reason=int(reason)))
        self.spawn(self.microphone(link))

    def sco_data(self, packet):
        if int(packet.packet_status) != 0:
            self.observe('sco-loss', status=int(packet.packet_status))
            return
        self.sco_buffer.extend(packet.data)
        if self.hf.active_codec == hfp.AudioCodec.MSBC:
            while len(self.sco_buffer) >= 60:
                frame = bytes(self.sco_buffer[:60])
                del self.sco_buffer[:60]
                if frame[0] != 1 or frame[1] not in (8, 0x38, 0xc8, 0xf8) or frame[2] != 0xad:
                    raise ValueError('invalid HFP mSBC H2 framing from guest')
                self.sco_frames.append(frame[2:59])
        else:
            self.sco_frames.append(bytes(self.sco_buffer))
            self.sco_buffer.clear()

    async def microphone(self, link):
        msbc = self.hf.active_codec == hfp.AudioCodec.MSBC
        rate = 16000 if msbc else 8000
        samples = 120 if msbc else 60
        pcm = [pcm_tone(rate, samples, 1, frequency=1500, start=i * samples) for i in range(134)]
        frames = self.oracle.Sbc().encode((16000, 15, 0, 0, 8, 26), pcm, True) if msbc else pcm
        # Calibrated PCM at the simulated microphone's output, after its hardware
        # gain. Compare independently decoded transmitted samples, never the guest.
        reference = self.oracle.Sbc().decode((16000, 15, 0, 0, 8, 26), frames, True) if msbc else frames
        samples_on_wire = struct.unpack(f'<{sum(map(len, reference)) // 2}h', b''.join(reference))
        steady = samples_on_wire[-rate:]
        self.microphone_reference = dict(mean_square=sum(value * value for value in steady) / len(steady),
                                         samples=len(steady), rate=rate, source='post-gain calibrated PCM; independent AOSP decode' if msbc else 'post-gain calibrated CVSD PCM')
        for index in range(4000):
            if self.sco_link is not link or link.handle not in self.device.sco_links:
                break
            data = frames[index % len(frames)]
            if msbc:
                data = bytes([1, (8, 0x38, 0xc8, 0xf8)[index % 4]]) + data + b'\0'
            self.device.host.send_sco_sdu(connection_handle=link.handle, sdu=data)
            await asyncio.sleep(.0075)

    def voice_verdict(self):
        msbc = self.hf.active_codec == hfp.AudioCodec.MSBC
        pcm = b''.join(self.oracle.Sbc().decode((16000, 15, 0, 0, 8, 26), self.sco_frames, True)) if msbc else b''.join(self.sco_frames)
        (self.work / f'{self.name}-voice.pcm').write_bytes(pcm)
        return dict(audio_metrics(pcm, 16000 if msbc else 8000, 1), microphone_reference=self.microphone_reference)

    def music_verdict(self):
        frames = []
        for payload in self.media_packets:
            if not payload or payload[0] & 0xf0:
                raise ValueError('fragmented SBC RTP payload was not expected')
            data = payload[1:]
            for _ in range(payload[0] & 15):
                if len(data) < 4 or data[0] != 0x9c:
                    raise ValueError('invalid SBC RTP frame header')
                blocks = ((data[1] >> 4 & 3) + 1) * 4
                mode, bands, pool = data[1] >> 2 & 3, (8 if data[1] & 1 else 4), data[2]
                channels = 1 if mode == 0 else 2
                bits = blocks * pool * (channels if mode < 2 else 1) + (bands if mode == 3 else 0)
                length = 4 + bands * channels // 2 + (bits + 7) // 8
                frames.append(data[:length])
                data = data[length:]
            if data:
                raise ValueError('SBC RTP packet has trailing bytes')
        if not frames:
            return dict(samples=0, rms=0, peak_frequency=0)
        header = frames[0][1]
        config = ([16000, 32000, 44100, 48000][header >> 6], ((header >> 4 & 3) + 1) * 4, header >> 2 & 3, header >> 1 & 1, 8 if header & 1 else 4, frames[0][2])
        pcm = b''.join(self.oracle.Sbc().decode(config, frames, False))
        (self.work / f'{self.name}-music.pcm').write_bytes(pcm)
        channels = 1 if config[2] == 0 else 2
        metrics = [audio_metrics(pcm, config[0], channels, channel) for channel in range(channels)]
        return dict(metrics[0], rate=config[0], channels=metrics)


class ClassicPeers:
    def __init__(self, peers):
        self.peers = {peer.name: peer for peer in peers}
        self.devices = [peer.device for peer in peers]

    async def command(self, request):
        action = request['action']
        if action == 'status':
            return {name: peer.status() for name, peer in self.peers.items()}
        peer = self.peers[request['peer']]
        if action == 'hid-reconnect':
            await peer.reconnect(request['address'])
        elif action == 'type':
            await peer.type_text(request['text'])
        elif action == 'report':
            await peer.report(bytes.fromhex(request['data']))
        elif action == 'disconnect':
            for connection in list(peer.connections.values()):
                await connection.disconnect()
        elif action == 'pairing-config':
            delegate = peer.pairing_delegate
            delegate.io_capability = pairing.PairingDelegate.IoCapability[request['io']]
            delegate.accepted = request.get('accept', True)
            delegate.number = request.get('number')
            delegate.pin = request.get('pin', '1234')
            if 'ssp' in request:
                await peer.device.send_sync_command(hci.HCI_Write_Simple_Pairing_Mode_Command(simple_pairing_mode=int(request['ssp'])))
            if 'sc' in request:
                await peer.device.send_sync_command(hci.HCI_Write_Secure_Connections_Host_Support_Command(secure_connections_host_support=int(request['sc'])))
            peer.device.pairing_config_factory = lambda _: pairing.PairingConfig(sc=request.get('sc', True), mitm=request.get('mitm', True), bonding=True, delegate=delegate)
        elif action == 'pairing-number':
            peer.pairing_delegate.number = request['number']
        elif action == 'pairing-start':
            peer.pairing_result = None
            peer.spawn(peer.initiate_pairing(request['address']))
        elif action == 'pairing-result':
            return peer.pairing_result
        elif action == 'authenticate':
            connection = await peer.connection(request['address'])
            return dict(encrypted=bool(connection.encryption), pairings=peer.pairings)
        elif action == 'save-key':
            address = str(hci.Address(request['address'], hci.Address.PUBLIC_DEVICE_ADDRESS))
            peer.saved_key = await peer.device.keystore.get(address)
            if peer.saved_key is None or peer.saved_key.link_key is None:
                raise AssertionError('no independent classic key to save')
            return dict(sha256=hashlib.sha256(peer.saved_key.link_key.value).hexdigest())
        elif action == 'restore-key':
            address = str(hci.Address(request['address'], hci.Address.PUBLIC_DEVICE_ADDRESS))
            await peer.device.keystore.update(address, peer.saved_key)
            return dict(sha256=hashlib.sha256(peer.saved_key.link_key.value).hexdigest())
        elif action == 'forget-key':
            await peer.device.keystore.delete(str(hci.Address(request['address'], hci.Address.PUBLIC_DEVICE_ADDRESS)))
        elif action == 'volume':
            peer.volume.volume = request['value']
            peer.avrcp.notify_volume_changed(request['value'])
        elif action == 'key':
            response = await peer.avrcp.send_key_event(avc.PassThroughFrame.OperationId[request['key']], True)
            released = await peer.avrcp.send_key_event(avc.PassThroughFrame.OperationId[request['key']], False)
            return {'response': str(response), 'pressed_code': int(response.response), 'released_code': int(released.response)}
        elif action == 'media-state':
            peer.volume.playback_status = avrcp.PlayStatus[request['state']]
            peer.avrcp.notify_playback_status_changed(peer.volume.playback_status)
            if 'track' in request:
                peer.volume.current_track_uid = request['track']
                peer.avrcp.notify_track_changed(request['track'])
        elif action == 'media-query':
            status = await peer.avrcp.get_play_status()
            attributes = await peer.avrcp.get_element_attributes(0, [avrcp.MediaAttributeId.TITLE])
            return {'status': str(status), 'attributes': [str(attribute) for attribute in attributes]}
        elif action == 'source-start':
            await peer.source_start(request['address'])
        elif action == 'hfp-connect':
            await peer.hfp_connect(request['address'], request.get('codec', 'msbc'))
        elif action == 'hsp-connect':
            await peer.hsp_connect(request['address'])
        elif action == 'sco-hold':
            if peer.held_sco_request is not None:
                raise AssertionError('a SCO acceptance is already held')
            peer.hold_sco_accept = True
        elif action == 'sco-release':
            peer.release_sco_request()
        elif action == 'hfp-at':
            return {'response': str(await peer.hf.execute_command(request['command'], response_type=hfp.AtResponseType.MULTIPLE))}
        elif action == 'hfp-expect-error':
            try:
                await peer.hf.execute_command(request['command'])
            except hfp.HfpProtocolError as error:
                if error.error_name != 'ERROR':
                    raise AssertionError(f"HFP refused with unrelated status {error.error_name!r}, expected ERROR") from error
                return {'rejected': True, 'response': str(error), 'status': error.error_name}
            raise AssertionError(f"guest accepted forbidden HFP command {request['command']}")
        elif action == 'answer':
            await peer.hf.answer_incoming_call()
        elif action == 'hangup':
            await peer.hf.terminate_call()
        elif action == 'voice-verdict':
            return peer.voice_verdict()
        elif action == 'music-verdict':
            return peer.music_verdict()
        elif action == 'music-reset':
            peer.media_packets.clear()
            (peer.work / f'{peer.name}-rtp.bin').unlink(missing_ok=True)
        elif action == 'opp-verdict':
            return peer.transfer.opp_verdict()
        elif action == 'opp-offer-l2cap':
            return peer.transfer.offer_l2cap(request['enabled'])
        elif action == 'opp-send':
            data = bytes.fromhex(request['data'])
            return await peer.transfer.opp_send(request['address'], request['name'], data)
        elif action == 'pan-start':
            await peer.transfer.pan_start()
        elif action == 'pan-verdict':
            return peer.transfer.pan_verdict()
        else:
            raise ValueError(f'unknown classic peer action {action}')
        return peer.status()

    async def close(self):
        await asyncio.gather(*(peer.close() for peer in self.peers.values()))


async def create_classic_peers(endpoint, work: Path, emit):
    peers = []
    try:
        for name, cls in [('keyboard', HidPeer), ('gamepad', HidPeer), ('serial', SerialPeer), ('headset', AudioPeer), ('phone', AudioPeer)]:
            transport = await open_transport(f'tcp-client:{endpoint}')
            config = device.DeviceConfiguration.from_dict(dict(name=f'Bumble {name}', classic_enabled=True, le_enabled=False, classic_sc_enabled=True))
            instance = device.Device.from_config_with_hci(config, transport.source, transport.sink)
            instance.keystore = keys.JsonKeyStore(namespace=name, filename=str(work / f'{name}-keys.json'))
            peer = cls(name, transport, instance, work, emit)
            instance.pairing_config_factory = lambda _, delegate=peer.pairing_delegate: pairing.PairingConfig(sc=True, mitm=False, bonding=True, delegate=delegate)
            peers.append(peer)
            if isinstance(peer, HidPeer):
                peer.setup(name == 'keyboard')
            elif isinstance(peer, AudioPeer):
                peer.setup(name == 'phone')
            else:
                peer.setup()
            await instance.power_on()
            await instance.set_discoverable(True)
            await instance.set_connectable(True)
            emit('ready', **peer.status())
        return ClassicPeers(peers)
    except BaseException:
        await asyncio.gather(*(peer.close() for peer in peers), return_exceptions=True)
        raise
