#!/usr/bin/env python3
"""Check independent LE peer capabilities with RootCanal, without a Liber guest.

This checks the test equipment, not production Bluetooth interoperability. Every
GATT/SMP/CIS exchange crosses distinct RootCanal controllers. Artifacts and the
summary are retained in --work for subsequent guest-gate diagnostics.
"""

import argparse
import asyncio
import dataclasses
import json
from pathlib import Path
import socket
import sys
from types import SimpleNamespace

from bumble import core, device, gatt, hci, pairing
from bumble.profiles import ascs, bap, csip, pacs, vcs
from bumble.transport import open_transport

from bluetooth_le_peers import Capture, Lc3, SIRK, create_le_peers


def ports():
    sockets = [socket.socket() for _ in range(4)]
    try:
        for sock in sockets:
            sock.bind(("127.0.0.1", 0))
        return [sock.getsockname()[1] for sock in sockets]
    finally:
        for sock in sockets:
            sock.close()


async def until(predicate, seconds=5):
    async with asyncio.timeout(seconds):
        while not predicate():
            await asyncio.sleep(.01)


async def exercise(endpoint, work, emit):
    peers = await create_le_peers(endpoint, work, emit)
    transport = None
    try:
        await peers.command({"action": "start-advertisements"})
        transport = await open_transport(f"tcp-client:{endpoint}")
        central = device.Device.from_config_with_hci(
            device.DeviceConfiguration.from_dict(dict(
                name="Independent LE fixture checker", address="F0:F1:F2:F3:F4:C0",
                cis_enabled=True, keystore=f"JsonKeyStore:{work / 'central-keys.json'}")),
            transport.source, transport.sink)
        central.pairing_config_factory = lambda _: pairing.PairingConfig(sc=True, mitm=False, bonding=True, identity_address_type=pairing.PairingConfig.AddressType.RANDOM)
        controls = []
        call_state = gatt.Characteristic(gatt.GATT_CALL_STATE_CHARACTERISTIC,
            gatt.Characteristic.Properties.READ | gatt.Characteristic.Properties.NOTIFY,
            gatt.Characteristic.Permissions.READABLE, b"\x01\x00\x00")
        control_point = gatt.Characteristic(gatt.GATT_CALL_CONTROL_POINT_CHARACTERISTIC,
            gatt.Characteristic.Properties.WRITE | gatt.Characteristic.Properties.NOTIFY,
            gatt.Characteristic.Permissions.WRITEABLE,
            gatt.CharacteristicValue(write=lambda _connection, value: controls.append(bytes(value))))
        central.add_service(gatt.Service(gatt.GATT_GENERIC_TELEPHONE_BEARER_SERVICE,
            [call_state, control_point]))
        await central.power_on()
        connection = await central.connect(hci.Address("F0:F1:F2:F3:F4:A1"))
        await connection.pair()
        assert connection.encryption, "SMP did not encrypt the real ACL connection"
        remote = device.Peer(connection)
        csis = await remote.discover_service_and_create_proxy(csip.CoordinatedSetIdentificationProxy)
        sirk_type, sirk = await csis.read_set_identity_resolving_key()
        assert sirk_type == csip.SirkType.ENCRYPTED and sirk == SIRK
        assert await csis.coordinated_set_size.read_value() == b"\x02"
        assert await csis.set_member_rank.read_value() == b"\x01"
        assert await remote.discover_service_and_create_proxy(pacs.PublishedAudioCapabilitiesServiceProxy)

        volume = await remote.discover_service_and_create_proxy(vcs.VolumeControlServiceProxy)
        observed_volume = []
        await volume.volume_state.subscribe(observed_volume.append)
        initial = await volume.volume_state.read_value()
        await volume.volume_control_point.write_value(bytes([4, initial.change_counter, 71]), with_response=True)
        await until(lambda: observed_volume and observed_volume[-1].volume_setting == 71)
        await peers.command({"action": "set-volume", "level": 129})
        await until(lambda: observed_volume[-1].volume_setting == 129)

        await peers.command({"action": "gtbs-watch"})
        assert peers.peers["le-left"]["calls"] == [{"index": 1, "state": 0, "flags": 0}]
        await peers.command({"action": "gtbs-control", "control": "accept", "index": 1})
        await until(lambda: controls == [b"\x00\x01"])
        call_state.value = b"\x01\x03\x00"
        await central.notify_subscribers(call_state)
        await until(lambda: peers.peers["le-left"]["calls"][0]["state"] == 3)

        audio = await remote.discover_service_and_create_proxy(ascs.AudioStreamControlServiceProxy)
        config = bap.CodecSpecificConfiguration(
            sampling_frequency=bap.SamplingFrequency.FREQ_16000,
            frame_duration=bap.FrameDuration.DURATION_10000_US,
            audio_channel_allocation=bap.AudioLocation.FRONT_LEFT,
            octets_per_codec_frame=40, codec_frames_per_sdu=1)
        await audio.ase_control_point.write_value(bytes(ascs.ASE_Config_Codec(
            ase_id=[1, 2], target_latency=[1, 1], target_phy=[2, 2],
            codec_id=[hci.CodingFormat(hci.CodecID.LC3)] * 2,
            codec_specific_configuration=[config, config])), with_response=True)
        await audio.ase_control_point.write_value(bytes(ascs.ASE_Config_QOS(
            ase_id=[1, 2], cig_id=[1, 1], cis_id=[1, 1], sdu_interval=[10000, 10000],
            framing=[0, 0], phy=[2, 2], max_sdu=[40, 40], retransmission_number=[2, 2],
            max_transport_latency=[40, 40], presentation_delay=[40000, 40000])), with_response=True)
        await audio.ase_control_point.write_value(bytes(ascs.ASE_Enable(
            ase_id=[1, 2], metadata=[b"", b""])), with_response=True)
        handles = await central.setup_cig(device.CigParameters(cig_id=1,
            cis_parameters=[device.CigParameters.CisParameters(cis_id=1, max_sdu_c_to_p=40, max_sdu_p_to_c=40)],
            sdu_interval_c_to_p=10000, sdu_interval_p_to_c=10000,
            max_transport_latency_c_to_p=40, max_transport_latency_p_to_c=40))
        link, = await central.create_cis([(handles[0], connection)])
        await link.setup_data_path(direction=link.Direction.HOST_TO_CONTROLLER)
        await link.setup_data_path(direction=link.Direction.CONTROLLER_TO_HOST)
        received = Capture(peers, "fixture-central", SimpleNamespace(ase_id=2, codec_specific_configuration=config))
        peers.captures.append(received)
        link.sink = received.packet
        await audio.ase_control_point.write_value(bytes(ascs.ASE_Receiver_Start_Ready(ase_id=[2])), with_response=True)
        await peers.command({"action": "source-voice", "frequency": 1000})
        stream = device.IsoPacketStream(link, 4)
        codec = Lc3(16000, 10000, True)
        for _ in range(30):
            await stream.write(codec.tone(2000, 40))
            await asyncio.sleep(.01)
        await until(lambda: len(peers.captures) == 2 and all(capture.frames >= 25 for capture in peers.captures))
        captures = [capture.status() for capture in peers.captures]
        for capture in captures:
            expected = 1000 if capture["peer"] == "fixture-central" else 2000
            assert abs(capture["frequency_hz"] - expected) < 80, capture
            assert capture["rms"] > 3000 and capture["lost"] == 0 and capture["sequence_gaps"] == 0, capture
        await peers.command({"action": "source-voice", "enabled": False})
        await connection.disconnect()
        await until(lambda: peers.peers["le-left"]["advertising"].enabled)
        connection = await central.connect(hci.Address("F0:F1:F2:F3:F4:A1"))
        await connection.encrypt()
        assert connection.encryption
        await peers.command({"action": "depart", "peer": "le-left"})
        await until(lambda: not peers.peers["le-left"]["connections"])
        await asyncio.sleep(.1)
        assert not peers.peers["le-left"]["advertising"].enabled

        await peers.command({"action": "broadcast-start", "code": "00112233445566778899aabbccddeeff"})
        await central.start_scanning(active=False, filter_duplicates=False)
        sync = await central.create_periodic_advertising_sync(hci.Address("F0:F1:F2:F3:F4:B0"), sid=1)
        announcements = []
        biginfos = []
        sync.on(sync.EVENT_BIGINFO_ADVERTISEMENT, biginfos.append)

        def periodic(advertisement):
            for uuid, value in advertisement.data.get_all(core.AdvertisingData.Type.SERVICE_DATA_16_BIT_UUID):
                if uuid == gatt.GATT_BASIC_AUDIO_ANNOUNCEMENT_SERVICE:
                    announcements.append(bap.BasicAudioAnnouncement.from_bytes(value))

        sync.on(sync.EVENT_PERIODIC_ADVERTISEMENT, periodic)
        await until(lambda: announcements and biginfos and sync.state == sync.State.ESTABLISHED)
        await central.stop_scanning()
        # RootCanal 1.13.0 ignores the broadcast encryption/code parameters. An
        # explicitly wrong key below records that limitation with real traffic;
        # it MUST NOT be mistaken for a passed encrypted-broadcast security test.
        big_sync = await central.create_big_sync(sync, device.BigSyncParameters(
            big_sync_timeout=100, bis=[1, 2], broadcast_code=bytes(16)))
        subgroup = announcements[-1].subgroups[0]
        bis_captures = []
        for bis, link in zip(subgroup.bis, big_sync.bis_links, strict=True):
            bis_config = dataclasses.replace(subgroup.codec_specific_configuration,
                audio_channel_allocation=bis.codec_specific_configuration.audio_channel_allocation)
            capture = Capture(peers, "fixture-bis", SimpleNamespace(ase_id=bis.index, codec_specific_configuration=bis_config))
            peers.captures.append(capture)
            bis_captures.append(capture)
            link.sink = capture.packet
            await link.setup_data_path(direction=link.Direction.CONTROLLER_TO_HOST)
        await until(lambda: all(capture.frames >= 25 for capture in bis_captures))
        bis_results = [capture.status() for capture in bis_captures]
        for capture, expected in zip(bis_results, (1500, 3000), strict=True):
            assert abs(capture["frequency_hz"] - expected) < 80, capture
            assert capture["rms"] > 3000 and capture["lost"] == 0 and capture["sequence_gaps"] == 0, capture
        broadcast_frames = peers.broadcast["frames"]
        await big_sync.terminate()
        await sync.terminate()
        await peers.command({"action": "broadcast-stop"})
        assert not peers.errors, peers.errors
        summary = {"status": "passed", "scope": "host fixture only; no Liber guest",
            "smp_encrypted": True, "encrypted_csis_sirk": True, "pacs_ascs": True,
            "vcs_bidirectional": True, "gtbs_client": True, "cis_captures": captures,
            "disconnect_reconnect_and_departure": True,
            "bis_transmitted_frames_each": broadcast_frames, "bis_count": 2,
            "bis_captures": bis_results,
            "broadcast_key_authentication": "unsupported by RootCanal: wrong code accepted"}
        (work / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps(summary, sort_keys=True), flush=True)
    finally:
        await peers.close()
        if transport:
            await transport.close()


async def main(args):
    args.work.mkdir(parents=True, exist_ok=True)
    with (args.work / "events.jsonl").open("w") as events, (args.work / "rootcanal.log").open("w") as controller_log:
        def emit(event, **fields):
            events.write(json.dumps(dict(event=event, **fields), sort_keys=True) + "\n")
            events.flush()

        chosen = ports()
        process = await asyncio.create_subprocess_exec(sys.executable, "-m", "rootcanal",
            *(f"--{name}_port={port}" for name, port in zip(("test", "hci", "link", "link_ble"), chosen)),
            "--enable_log_color=false", stdout=controller_log, stderr=asyncio.subprocess.STDOUT)
        try:
            async with asyncio.timeout(5):
                while True:
                    try:
                        _, writer = await asyncio.open_connection("127.0.0.1", chosen[0])
                        writer.close()
                        await writer.wait_closed()
                        break
                    except OSError:
                        if process.returncode is not None:
                            raise RuntimeError("RootCanal failed to start")
                        await asyncio.sleep(.05)
            await asyncio.wait_for(exercise(f"127.0.0.1:{chosen[1]}", args.work, emit), 30)
        finally:
            if process.returncode is None:
                process.terminate()
                try:
                    await asyncio.wait_for(process.wait(), 5)
                except TimeoutError:
                    process.kill()
                    await process.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, required=True)
    asyncio.run(main(parser.parse_args()))
