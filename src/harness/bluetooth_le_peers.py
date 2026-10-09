#!/usr/bin/env python3
"""Independent Bumble LE Audio peers over real RootCanal HCI connections.

PACS/ASCS/CSIP/VCS, SMP, ATT and ISO setup are Bumble's implementations. PCM is
encoded/decoded by pinned Google liblc3. The adapter only controls peers, paces
source audio and records packets that actually crossed their controller links.
"""

import array
import asyncio
import contextlib
import ctypes as c
import functools
import math
from pathlib import Path
import struct

from bumble import core, data_types, device, gatt, hci, pairing
from bumble.profiles import ascs, bap, cap, csip, le_audio, pacs, tmap, vcs
from bumble.transport import open_transport

ROOT = Path(__file__).resolve().parents[2]
SIRK = bytes.fromhex("00112233445566778899aabbccddeeff")
ADDRESSES = {"le-left": "F0:F1:F2:F3:F4:A1", "le-right": "F0:F1:F2:F3:F4:A2", "le-broadcast": "F0:F1:F2:F3:F4:B0"}


class Lc3:
    """A single independent encoder or decoder, retaining its own overlap state."""

    def __init__(self, rate, duration, encoding):
        self.rate, self.duration, self.encoding = rate, duration, encoding
        self.samples = rate * duration // 1_000_000
        self.lib = c.CDLL(str(ROOT / ".build/bluetooth-oracles/liblc3/bin/liblc3.so"))
        kind = "encoder" if encoding else "decoder"
        size = getattr(self.lib, f"lc3_{kind}_size")
        size.argtypes, size.restype = [c.c_int, c.c_int], c.c_uint
        setup = getattr(self.lib, f"lc3_setup_{kind}")
        setup.argtypes, setup.restype = [c.c_int, c.c_int, c.c_int, c.c_void_p], c.c_void_p
        self.memory = c.create_string_buffer(size(duration, rate))
        self.context = setup(duration, rate, rate, self.memory)
        if not self.context:
            raise ValueError("liblc3 refused the negotiated configuration")
        self.encode_fn = self.lib.lc3_encode
        self.encode_fn.argtypes = [c.c_void_p, c.c_int, c.c_void_p, c.c_int, c.c_int, c.c_void_p]
        self.encode_fn.restype = c.c_int
        self.decode_fn = self.lib.lc3_decode
        self.decode_fn.argtypes = [c.c_void_p, c.c_void_p, c.c_int, c.c_int, c.c_void_p, c.c_int]
        self.decode_fn.restype = c.c_int
        self.position = 0

    def tone(self, frequency, octets):
        samples = [round(8192 * math.sin(2 * math.pi * frequency * (self.position + i) / self.rate)) for i in range(self.samples)]
        self.position += self.samples
        pcm = (c.c_int16 * self.samples)(*samples)
        encoded = c.create_string_buffer(octets)
        if self.encode_fn(self.context, 0, pcm, 1, octets, encoded) != 0:
            raise ValueError("independent LC3 encoder refused a frame")
        return encoded.raw

    def decode(self, encoded):
        source = c.create_string_buffer(encoded)
        pcm = c.create_string_buffer(self.samples * 2)
        if self.decode_fn(self.context, source, len(encoded), 0, pcm, 1) != 0:
            raise ValueError("independent LC3 decoder refused an observed frame")
        return pcm.raw


class Capture:
    def __init__(self, group, peer, ase):
        self.group, self.peer, self.ase_id = group, peer, ase.ase_id
        config = ase.codec_specific_configuration
        if not isinstance(config, bap.CodecSpecificConfiguration) or not config.sampling_frequency or config.frame_duration is None or not config.octets_per_codec_frame:
            raise ValueError("streaming ASE lacks a complete codec configuration")
        if (config.codec_frames_per_sdu or 1) != 1:
            raise ValueError("peer advertised one LC3 frame per SDU")
        if config.audio_channel_allocation and int(config.audio_channel_allocation).bit_count() != 1:
            raise ValueError("one earbud was configured with more than one channel")
        self.rate, self.duration = config.sampling_frequency.hz, config.frame_duration.us
        self.octets = config.octets_per_codec_frame
        self.location = int(config.audio_channel_allocation or 0)
        self.decoder = Lc3(self.rate, self.duration, False)
        self.frames = self.lost = self.sequence_gaps = 0
        self.sequence = None
        self.partial = bytearray()
        self.expected = None
        self.samples = array.array("h")
        stem = group.work / f"{peer}-ase{ase.ase_id}-{len(group.captures)}"
        self.lc3_path, self.pcm_path = stem.with_suffix(".lc3"), stem.with_suffix(".pcm")
        self.lc3 = self.lc3_path.open("wb")
        self.pcm = self.pcm_path.open("wb")
        self.closed = False

    def packet(self, packet):
        if self.closed:
            return
        try:
            if packet.pb_flag in (0, 2):
                if self.partial:
                    raise ValueError("new ISO SDU overtook an unfinished SDU")
                if packet.packet_status_flag:
                    self.lost += 1
                    return
                if self.sequence is not None and packet.packet_sequence_number != ((self.sequence + 1) & 0xFFFF):
                    self.sequence_gaps += 1
                self.sequence = packet.packet_sequence_number
                self.expected = packet.iso_sdu_length
                if self.expected != self.octets:
                    raise ValueError(f"ISO SDU length {self.expected}, negotiated {self.octets}")
            elif self.expected is None:
                raise ValueError("ISO continuation without first fragment")
            self.partial.extend(packet.iso_sdu_fragment)
            if len(self.partial) > self.octets:
                raise ValueError("ISO SDU exceeded its advertised length")
            if packet.pb_flag not in (2, 3):
                return
            if len(self.partial) != self.expected:
                raise ValueError("ISO SDU ended short")
            encoded = bytes(self.partial)
            self.partial.clear()
            self.expected = None
            pcm = self.decoder.decode(encoded)
            self.frames += 1
            # Bound artifacts to 30 seconds per stream; counters keep measuring beyond that.
            if self.frames * self.duration <= 30_000_000:
                self.lc3.write(struct.pack("<H", len(encoded)) + encoded)
                self.pcm.write(pcm)
                self.lc3.flush()
                self.pcm.flush()
            self.samples.frombytes(pcm)
            if len(self.samples) > self.rate:
                del self.samples[:-self.rate]
            if self.frames in (1, 25, 100):
                self.group.observe("le-iso-capture", **self.status())
        except Exception as error:
            self.group.error(self.peer, str(error))

    def status(self):
        samples = self.samples[len(self.samples) // 10:]
        crossings = sum(a <= 0 < b for a, b in zip(samples, samples[1:]))
        return {"peer": self.peer, "ase": self.ase_id, "rate": self.rate, "duration_us": self.duration, "octets": self.octets, "location": self.location, "frames": self.frames, "lost": self.lost, "sequence_gaps": self.sequence_gaps, "frequency_hz": round(crossings * self.rate / max(1, len(samples)), 2), "rms": round(math.sqrt(sum(sample * sample for sample in samples) / max(1, len(samples))), 2), "lc3": str(self.lc3_path), "pcm": str(self.pcm_path)}

    def close(self):
        if not self.closed:
            self.closed = True
            self.lc3.close()
            self.pcm.close()


class LePeers:
    def __init__(self, endpoint, work, emit):
        self.endpoint, self.work, self.emit = endpoint, Path(work), emit
        self.work.mkdir(parents=True, exist_ok=True)
        self.devices = []
        self.transports = []
        self.peers = {}
        self.tasks = set()
        self.captures = []
        self.errors = []
        self.broadcast = None
        self.closed = False

    def observe(self, event, **fields):
        self.emit(event, **fields)

    def error(self, peer, reason):
        self.errors.append({"peer": peer, "reason": reason})
        self.observe("le-error", peer=peer, reason=reason)

    def task(self, awaitable, peer):
        task = asyncio.create_task(awaitable)
        self.tasks.add(task)

        def done(completed):
            self.tasks.discard(completed)
            if not completed.cancelled() and (error := completed.exception()):
                self.error(peer, str(error))

        task.add_done_callback(done)
        return task

    async def create(self, name):
        transport = await open_transport(f"tcp-client:{self.endpoint}")
        self.transports.append(transport)
        config = device.DeviceConfiguration.from_dict(dict(name="Bumble " + name, address=ADDRESSES[name], cis_enabled=True, keystore=f"JsonKeyStore:{self.work / (name + '-keys.json')}", advertising_interval_min=100, advertising_interval_max=100))
        peer = device.Device.from_config_with_hci(config, transport.source, transport.sink)
        peer.pairing_config_factory = lambda _: pairing.PairingConfig(sc=True, mitm=False, bonding=True, identity_address_type=pairing.PairingConfig.AddressType.RANDOM)
        self.devices.append(peer)
        state = {"device": peer, "connections": {}, "advertising": None, "want_advertising": False, "voice": False, "frequency": 1000, "source_frames": 0, "gtbs": {}, "calls": []}
        self.peers[name] = state
        peer.on(peer.EVENT_CONNECTION, functools.partial(self.connection, name))
        await asyncio.wait_for(peer.power_on(), 10)
        self.observe("le-peer-ready", peer=name, address=str(peer.random_address), public_address=str(peer.public_address), le_features=f"{peer.host.local_le_features:016x}")
        return state

    def connection(self, name, connection):
        state = self.peers[name]
        state["connections"][connection.handle] = connection
        self.observe("le-connected", peer=name, remote=str(connection.peer_address), handle=connection.handle)
        connection.on(connection.EVENT_PAIRING, lambda _keys: self.observe("le-paired", peer=name, remote=str(connection.peer_address)))
        connection.on(connection.EVENT_CONNECTION_ENCRYPTION_CHANGE, lambda: self.observe("le-encrypted", peer=name, encrypted=bool(connection.encryption)))

        def disconnected(reason):
            state["connections"].pop(connection.handle, None)
            state["gtbs"].pop(connection.handle, None)
            # Bumble's ASE objects outlive the ACL bearer. Reset this peer's
            # connection-local stream state so a fresh connection can configure
            # the same advertised endpoints after an interrupted stream.
            if not state["connections"] and "ascs" in state:
                for ase in state["ascs"].ase_state_machines.values():
                    ase.cis_link = None
                    ase.state = ase.State.IDLE
            self.observe("le-disconnected", peer=name, reason=reason)
            if state["want_advertising"] and not self.closed:
                self.task(self.advertise_one(name), name)

        connection.on(connection.EVENT_DISCONNECTION, disconnected)

    async def earbud(self, name, rank, location):
        state = await self.create(name)
        peer = state["device"]
        csis = csip.CoordinatedSetIdentificationService(SIRK, csip.SirkType.ENCRYPTED, coordinated_set_size=2, set_member_rank=rank)
        peer.add_service(cap.CommonAudioServiceService(csis))

        def pac(frequencies, maximum):
            return pacs.PacRecord(coding_format=hci.CodingFormat(hci.CodecID.LC3), codec_specific_capabilities=bap.CodecSpecificCapabilities(supported_sampling_frequencies=frequencies, supported_frame_durations=bap.SupportedFrameDuration.DURATION_10000_US_SUPPORTED | bap.SupportedFrameDuration.DURATION_7500_US_SUPPORTED, supported_audio_channel_count=[1], min_octets_per_codec_frame=20, max_octets_per_codec_frame=maximum, supported_max_codec_frames_per_sdu=1))

        context = bap.ContextType.MEDIA | bap.ContextType.CONVERSATIONAL
        peer.add_service(pacs.PublishedAudioCapabilitiesService(supported_source_context=context, supported_sink_context=context, available_source_context=context, available_sink_context=context, sink_pac=[pac(bap.SupportedSamplingFrequency.FREQ_16000 | bap.SupportedSamplingFrequency.FREQ_48000, 120)], sink_audio_locations=location, source_pac=[pac(bap.SupportedSamplingFrequency.FREQ_16000, 40)] if rank == 1 else [], source_audio_locations=bap.AudioLocation.FRONT_LEFT if rank == 1 else None))
        service = ascs.AudioStreamControlService(peer, sink_ase_id=[1], source_ase_id=[2] if rank == 1 else [])
        peer.add_service(service)
        volume = vcs.VolumeControlService(volume_setting=128)
        peer.add_service(volume)
        peer.add_service(tmap.TelephonyAndMediaAudioService(tmap.Role.CALL_TERMINAL | tmap.Role.UNICAST_MEDIA_RECEIVER))
        state.update(ascs=service, vcs=volume, csis=csis)
        volume.on(volume.EVENT_VOLUME_STATE_CHANGE, lambda: self.observe("le-volume", peer=name, level=volume.volume_setting, muted=volume.muted, counter=volume.change_counter, origin="remote"))
        for ase in service.ase_state_machines.values():
            ase.on(ase.EVENT_STATE_CHANGE, functools.partial(self.ase_state, name, ase))
        state["advertising_data"] = bytes(core.AdvertisingData([data_types.CompleteLocalName("Bumble " + name), data_types.Flags(core.AdvertisingData.LE_GENERAL_DISCOVERABLE_MODE_FLAG | core.AdvertisingData.BR_EDR_NOT_SUPPORTED_FLAG), data_types.IncompleteListOf16BitServiceUUIDs([pacs.PublishedAudioCapabilitiesService.UUID])])) + csis.get_advertising_data() + bytes(bap.UnicastServerAdvertisingData())

    def ase_state(self, name, ase):
        self.observe("le-ase-state", peer=name, ase=ase.ase_id, state=ase.state.name, role=ase.role.name)
        if source := getattr(ase, "liber_source", None):
            source.cancel()
            ase.liber_source = None
        old_capture = getattr(ase, "liber_capture", None)
        if old_capture:
            old_capture.close()
            if ase.cis_link and ase.cis_link.sink == old_capture.packet:
                ase.cis_link.sink = None
            ase.liber_capture = None
        if ase.state != ase.State.STREAMING:
            return
        try:
            config = ase.codec_specific_configuration
            self.observe("le-codec", peer=name, ase=ase.ase_id, configuration=bytes(config).hex(), cis_handle=ase.cis_link.handle)
            if ase.role == ascs.AudioRole.SINK:
                capture = Capture(self, name, ase)
                self.captures.append(capture)
                ase.liber_capture = capture
                ase.cis_link.sink = capture.packet
            elif self.peers[name]["voice"]:
                self.start_voice(name, ase)
        except Exception as error:
            self.error(name, str(error))

    def start_voice(self, name, ase):
        existing = getattr(ase, "liber_source", None)
        if existing and not existing.done():
            return
        ase.liber_source = self.task(self.voice(name, ase), name)

    async def voice(self, name, ase):
        state = self.peers[name]
        config = ase.codec_specific_configuration
        codec = Lc3(config.sampling_frequency.hz, config.frame_duration.us, True)
        stream = device.IsoPacketStream(ase.cis_link, 4)
        deadline = asyncio.get_running_loop().time()
        while state["voice"] and ase.state == ase.State.STREAMING and ase.cis_link:
            await stream.write(codec.tone(state["frequency"], config.octets_per_codec_frame))
            state["source_frames"] += 1
            deadline += codec.duration / 1_000_000
            await asyncio.sleep(max(0, deadline - asyncio.get_running_loop().time()))

    async def advertise_one(self, name):
        state = self.peers[name]
        if not state["want_advertising"] or self.closed:
            return
        # Bumble installs auto_restart at connection time; changing that flag
        # later does not cancel the installed callback. Own the restart so a
        # deliberate departure really disables advertising before disconnect.
        if state["advertising"] is None:
            state["advertising"] = await state["device"].create_advertising_set(advertising_parameters=device.AdvertisingParameters(primary_advertising_interval_min=100, primary_advertising_interval_max=100), advertising_data=state["advertising_data"], auto_restart=False)
        elif not state["advertising"].enabled:
            await state["advertising"].start()

    async def advertise(self):
        for name in ("le-left", "le-right"):
            self.peers[name]["want_advertising"] = True
            await self.advertise_one(name)
        self.observe("le-advertising", peers=["le-left", "le-right"])

    async def gtbs_watch(self, name):
        state = self.peers[name]
        if not state["connections"]:
            raise ValueError("earbud has no connection to a call gateway")
        connection = next(iter(state["connections"].values()))
        peer = device.Peer(connection)
        services = await peer.discover_services([gatt.GATT_GENERIC_TELEPHONE_BEARER_SERVICE])
        if len(services) != 1:
            raise ValueError("connected gateway does not expose one GTBS")
        service = services[0]
        await service.discover_characteristics()
        call_state = service.get_required_characteristic_by_uuid(gatt.GATT_CALL_STATE_CHARACTERISTIC)
        control = service.get_required_characteristic_by_uuid(gatt.GATT_CALL_CONTROL_POINT_CHARACTERISTIC)

        def observed(value):
            if len(value) % 3:
                self.error(name, "GTBS call state is not a list of triples")
                return
            state["calls"] = [{"index": value[i], "state": value[i + 1], "flags": value[i + 2]} for i in range(0, len(value), 3)]
            self.observe("le-gtbs-calls", peer=name, calls=state["calls"])

        await call_state.subscribe(observed)
        observed(await call_state.read_value())
        await control.subscribe(lambda value: self.observe("le-gtbs-result", peer=name, value=bytes(value).hex()))
        state["gtbs"][connection.handle] = control

    async def start_broadcast(self, request):
        if self.broadcast:
            raise ValueError("a broadcast is already running")
        state = self.peers["le-broadcast"]
        peer = state["device"]
        broadcast_id = int(request.get("broadcast_id", 0x123456))
        if not 0 <= broadcast_id <= 0xFFFFFF:
            raise ValueError("broadcast_id must fit 24 bits")
        code = bytes.fromhex(request["code"]) if request.get("code") else None
        if code is not None and len(code) != 16:
            raise ValueError("broadcast code must be sixteen bytes encoded as hex")
        config = bap.CodecSpecificConfiguration(sampling_frequency=bap.SamplingFrequency.FREQ_48000, frame_duration=bap.FrameDuration.DURATION_10000_US, octets_per_codec_frame=120)
        base = bap.BasicAudioAnnouncement(presentation_delay=40_000, subgroups=[bap.BasicAudioAnnouncement.Subgroup(codec_id=hci.CodingFormat(hci.CodecID.LC3), codec_specific_configuration=config, metadata=le_audio.Metadata(), bis=[bap.BasicAudioAnnouncement.BIS(index=index, codec_specific_configuration=bap.CodecSpecificConfiguration(audio_channel_allocation=location)) for index, location in [(1, bap.AudioLocation.FRONT_LEFT), (2, bap.AudioLocation.FRONT_RIGHT)]])])
        advertising_data = bytes(core.AdvertisingData([data_types.CompleteLocalName("Bumble independent broadcast"), data_types.BroadcastName("Independent LC3")])) + bap.BroadcastAudioAnnouncement(broadcast_id).get_advertising_data()
        advertising = await peer.create_advertising_set(advertising_parameters=device.AdvertisingParameters(advertising_event_properties=device.AdvertisingEventProperties(is_connectable=False), primary_advertising_interval_min=100, primary_advertising_interval_max=100, advertising_sid=1), advertising_data=advertising_data, periodic_advertising_parameters=device.PeriodicAdvertisingParameters(periodic_advertising_interval_min=100, periodic_advertising_interval_max=100), periodic_advertising_data=base.get_advertising_data(), auto_start=True)
        self.broadcast = {"advertising": advertising, "big": None, "frames": 0, "broadcast_id": broadcast_id, "encrypted": code is not None}
        try:
            await advertising.start_periodic()
            big = await peer.create_big(advertising, device.BigParameters(num_bis=2, sdu_interval=10_000, max_sdu=120, max_transport_latency=65, rtn=2, broadcast_code=code))
            self.broadcast["big"] = big
            streams = []
            for link in big.bis_links:
                await link.setup_data_path(direction=link.Direction.HOST_TO_CONTROLLER)
                streams.append(device.IsoPacketStream(link, 4))
            self.broadcast["task"] = self.task(self.broadcast_audio(streams), "le-broadcast")
            self.observe("le-broadcast-started", broadcast_id=broadcast_id, encrypted=code is not None, bis_handles=[link.handle for link in big.bis_links], base=bytes(base).hex())
        except BaseException:
            await self.stop_broadcast()
            raise

    async def broadcast_audio(self, streams):
        codecs = [Lc3(48_000, 10_000, True), Lc3(48_000, 10_000, True)]
        deadline = asyncio.get_running_loop().time()
        while self.broadcast:
            for stream, codec, frequency in zip(streams, codecs, (1500, 3000)):
                await stream.write(codec.tone(frequency, 120))
            self.broadcast["frames"] += 1
            deadline += 0.010
            await asyncio.sleep(max(0, deadline - asyncio.get_running_loop().time()))

    async def stop_broadcast(self):
        broadcast, self.broadcast = self.broadcast, None
        if not broadcast:
            return
        if task := broadcast.get("task"):
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await task
        if broadcast["big"]:
            await asyncio.wait_for(broadcast["big"].terminate(), 5)
        advertising = broadcast["advertising"]
        if advertising.periodic_enabled:
            await advertising.stop_periodic()
        await advertising.stop()
        await advertising.remove()
        self.observe("le-broadcast-stopped", frames=broadcast["frames"])

    def status(self):
        return {"peers": {name: {"address": str(state["device"].random_address), "connections": list(state["connections"]), "advertising": bool(state["advertising"] and state["advertising"].enabled), "source_frames": state["source_frames"], "calls": state["calls"], "volume": state["vcs"].volume_setting if "vcs" in state else None} for name, state in self.peers.items()}, "captures": [capture.status() for capture in self.captures], "errors": self.errors, "broadcast": {key: self.broadcast[key] for key in ("frames", "broadcast_id", "encrypted")} if self.broadcast else None}

    async def command(self, request):
        op = request["action"]
        name = request.get("peer", "le-left")
        if op == "status":
            return self.status()
        if op == "start-advertisements":
            await self.advertise()
        elif op == "set-volume":
            state = self.peers[name]
            volume = state["vcs"]
            level = int(request["level"])
            if not 0 <= level <= 255:
                raise ValueError("volume must be 0..255")
            volume.volume_setting = level
            volume.change_counter = (volume.change_counter + 1) & 255
            await state["device"].notify_subscribers(volume.volume_state)
            self.observe("le-volume", peer=name, level=level, origin="local")
        elif op == "source-voice":
            state = self.peers[name]
            state["voice"] = bool(request.get("enabled", True))
            state["frequency"] = int(request.get("frequency", 1000))
            for ase in state["ascs"].ase_state_machines.values():
                if ase.role == ascs.AudioRole.SOURCE and ase.state == ase.State.STREAMING and state["voice"]:
                    self.start_voice(name, ase)
                elif not state["voice"] and (source := getattr(ase, "liber_source", None)):
                    source.cancel()
                    ase.liber_source = None
        elif op == "gtbs-watch":
            await self.gtbs_watch(name)
        elif op == "gtbs-control":
            state = self.peers[name]
            opcode = {"accept": 0, "terminate": 1}[request["control"]]
            controls = list(state["gtbs"].values())
            if len(controls) != 1:
                raise ValueError("gtbs-watch must select the one connected gateway first")
            await controls[0].write_value(bytes([opcode, int(request["index"])]), with_response=True)
        elif op == "broadcast-start":
            await self.start_broadcast(request)
        elif op in ("broadcast-stop", "stop"):
            await self.stop_broadcast()
        elif op in ("disconnect", "depart"):
            if op == "depart":
                state = self.peers[name]
                state["want_advertising"] = False
                if state["advertising"] and state["advertising"].enabled:
                    await state["advertising"].stop()
            for connection in list(self.peers[name]["connections"].values()):
                await connection.disconnect()
        else:
            raise ValueError(f"unknown LE peer operation: {op}")
        return self.status()

    async def close(self):
        if self.closed:
            return
        self.closed = True
        try:
            await self.stop_broadcast()
        finally:
            for task in list(self.tasks):
                task.cancel()
            await asyncio.gather(*self.tasks, return_exceptions=True)
            for capture in self.captures:
                capture.close()
            for transport in self.transports:
                await transport.close()


async def create_le_peers(endpoint: str, work: Path, emit):
    peers = LePeers(endpoint, work, emit)
    try:
        await peers.earbud("le-left", 1, bap.AudioLocation.FRONT_LEFT)
        await peers.earbud("le-right", 2, bap.AudioLocation.FRONT_RIGHT)
        await peers.create("le-broadcast")
        return peers
    except BaseException:
        await peers.close()
        raise
