#!/usr/bin/env python3
"""Check the microphone oracle's one playback period at QEMU's USB speaker.

QEMU 10.0.13 hw/usb/pcap.c writes native-endian classic PCAP with 64-byte
usbmon headers; len_cap includes that header. OUT completions omit actual
length and payload. dev-audio.c can return status0 after accepting fewer bytes,
so the owned runner must enable debug2 and preserve its overrun diagnostics.
This proves USB-device receipt, not audible output from the audio backend.
Source: https://raw.githubusercontent.com/qemu/qemu/v10.0.13/hw/usb/pcap.c
        https://raw.githubusercontent.com/qemu/qemu/v10.0.13/hw/usb/dev-audio.c
"""
import argparse
from pathlib import Path
import struct
import sys

ENABLED = 'qemu-run: microphone duplex sink capture enabled (usb-audio debug=2)'
INTERFACE = 'usb-audio: set interface 1'
OVERRUN = 'usb-audio: output overrun'
PERIOD = 2048
PACKET = 192
PACKETS = 11


def need(condition, reason):
    if not condition:
        raise ValueError(reason)


def expected_audio():
    # The existing hardware.rs pcm_period(99), followed by the final USB packet's padding.
    return bytes(((at + 99 * 7) % 251) | 1 for at in range(PERIOD)) + bytes(64)


def check(data, log):
    need(log.count(ENABLED) == 1, 'missing or repeated debug2 capture admission')
    need(INTERFACE in log.splitlines(), 'missing actual stereo interface activation')
    need(OVERRUN not in log, 'USB speaker reported output overrun')
    need(len(data) >= 24, 'missing or truncated PCAP header')
    endian = {b'\xd4\xc3\xb2\xa1': '<', b'\xa1\xb2\xc3\xd4': '>'}.get(data[:4])
    need(endian is not None, 'unsupported PCAP magic')
    magic, major, minor, zone, precision, snaplen, linktype = struct.unpack_from(endian + 'IHHIIII', data)
    need((magic, major, minor, zone, precision, snaplen, linktype) ==
         (0xa1b2c3d4, 2, 4, 0, 0, 4160, 220), 'unexpected QEMU usbmon PCAP format')
    offset, records = 24, 0
    submitted, completed, pending = [], set(), {}
    device_identity = None
    while offset < len(data):
        need(len(data) - offset >= 16, 'truncated PCAP record header')
        seconds, micros, captured, original = struct.unpack_from(endian + 'IIII', data, offset)
        offset += 16
        need(64 <= captured <= snaplen and captured <= original, 'invalid PCAP record lengths')
        need(captured <= len(data) - offset, 'truncated PCAP record data')
        body = data[offset:offset + captured]
        offset += captured
        records += 1
        urb, event, transfer, endpoint, device, bus, setup, flag = struct.unpack_from(endian + 'QBBBBHBB', body)
        usb_seconds, usb_micros, status, length, cap = struct.unpack_from(endian + 'qiiII', body, 16)
        need((seconds, micros) == (usb_seconds, usb_micros) and micros < 1_000_000,
             'PCAP and usbmon timestamps differ')
        need(cap == captured and original == length + 64, 'usbmon lengths differ from PCAP record')
        need(event in (ord('S'), ord('C')), 'unexpected usbmon event type')
        if transfer == 2:
            # Enumeration control URBs all use id0; they are not playback receipt evidence.
            need(endpoint in (0, 0x80), 'control record has a data endpoint')
            continue
        need(transfer == 0 and endpoint == 1, 'unexpected data endpoint or transfer type')
        need(setup == ord('-') and body[40:64] == bytes(24), 'unexpected QEMU isochronous metadata')
        need(device != 0, 'playback device has no assigned address')
        identity = (bus, device, endpoint)
        if device_identity is None:
            device_identity = identity
        need(identity == device_identity, 'playback records name different devices')
        need(status == 0, 'failed USB data transaction')
        if event == ord('S'):
            need(urb not in pending and urb not in completed, 'duplicate playback submission')
            need(flag == ord('=') and length == PACKET and captured == 64 + PACKET,
                 'playback submission is short or truncated')
            payload = body[64:]
            pending[urb] = payload
            submitted.append((urb, payload))
        else:
            need(urb in pending, 'unmatched or duplicate playback completion')
            need(flag == ord('>') and length == 0 and captured == 64,
                 'unexpected OUT completion payload or length')
            pending.pop(urb)
            completed.add(urb)
    need(not pending, 'missing playback completion')
    need(len(submitted) == len(completed) == PACKETS, 'expected exactly eleven completed playback packets')
    need(b''.join(payload for _, payload in submitted) == expected_audio(),
         'playback PCM bytes or final zero padding differ')
    return dict(records=records, completed_packets=PACKETS, pcm_bytes=PERIOD,
                padding_bytes=64, device=list(device_identity),
                scope='USB-device receipt; audio backend output is not measured')


def self_test():
    """Synthetic format/oracle tests, not a substitute for the actual guest capture."""
    def header(endian='<', linktype=220):
        return struct.pack(endian + 'IHHIIII', 0xa1b2c3d4, 2, 4, 0, 0, 4160, linktype)

    def record(urb, event, payload=b'', *, endian='<', status=0, endpoint=1,
               transfer=0, device=7, flag=None, length=None):
        if flag is None:
            flag = '=' if event == 'S' else '>'
        if length is None:
            length = len(payload)
        captured = 64 + len(payload)
        body = struct.pack(endian + 'QBBBBHBBqiiII8siiII', urb, ord(event), transfer,
                           endpoint, device, 0, ord('-'), ord(flag), 100, 1234,
                           status, length, captured, bytes(8), 0, 0, 0, 0) + payload
        return struct.pack(endian + 'IIII', 100, 1234, captured, 64 + length) + body

    def fixture(endian='<', audio=None):
        audio = expected_audio() if audio is None else audio
        rows = []
        # Valid unrelated endpoint-zero controls must not enter the playback count.
        rows += [record(0, 'S', endian=endian, transfer=2, endpoint=0),
                 record(0, 'C', endian=endian, transfer=2, endpoint=0)]
        for at in range(PACKETS):
            rows += [record(at + 1, 'S', audio[at * PACKET:(at + 1) * PACKET], endian=endian),
                     record(at + 1, 'C', endian=endian)]
        return rows

    log = ENABLED + '\nusb-audio: reset\n' + INTERFACE + '\n'
    cases = 0
    for endian in ('<', '>'):
        result = check(header(endian) + b''.join(fixture(endian)), log)
        need(result['completed_packets'] == 11 and result['pcm_bytes'] == 2048,
             'positive fixture produced the wrong receipt')
        cases += 1

    def rejected(name, data, diagnostic, output=log):
        nonlocal cases
        try:
            check(data, output)
        except ValueError as error:
            need(str(error) == diagnostic, name + ': rejected for the wrong reason: ' + str(error))
        else:
            raise ValueError(name + ': false sink success')
        cases += 1

    rows = fixture()
    good = header() + b''.join(rows)
    rejected('missing completion', header() + b''.join(rows[:-1]), 'missing playback completion')
    rejected('failed completion', header() + b''.join(rows[:-1]) + record(11, 'C', status=-121),
             'failed USB data transaction')
    rejected('failed submission', header() + b''.join(rows[:2]) + record(1, 'S', expected_audio()[:192], status=-19)
             + b''.join(rows[3:]), 'failed USB data transaction')
    corrupted = bytearray(expected_audio())
    corrupted[109] ^= 0x40
    rejected('corrupt PCM', header() + b''.join(fixture(audio=bytes(corrupted))),
             'playback PCM bytes or final zero padding differ')
    corrupted = bytearray(expected_audio())
    corrupted[-1] = 1
    rejected('nonzero padding', header() + b''.join(fixture(audio=bytes(corrupted))),
             'playback PCM bytes or final zero padding differ')
    rejected('short final USB packet', header() + b''.join(rows[:-2])
             + record(11, 'S', expected_audio()[-192:-64]) + record(11, 'C'),
             'playback submission is short or truncated')
    rejected('truncated record', good[:-1], 'truncated PCAP record data')
    rejected('trailing partial header', good + b'x', 'truncated PCAP record header')
    rejected('duplicate submission', header() + b''.join(rows[:3]) + rows[2] + b''.join(rows[3:]),
             'duplicate playback submission')
    rejected('reused completed id', good + rows[2], 'duplicate playback submission')
    rejected('duplicate completion', good + rows[-1], 'unmatched or duplicate playback completion')
    rejected('unmatched completion', header() + record(999, 'C'), 'unmatched or duplicate playback completion')
    rejected('empty playback', header() + b''.join(rows[:2]), 'expected exactly eleven completed playback packets')
    rejected('extra playback', good + record(12, 'S', bytes(192)) + record(12, 'C'),
             'expected exactly eleven completed playback packets')
    rejected('wrong endpoint', header() + record(1, 'S', expected_audio()[:192], endpoint=2),
             'unexpected data endpoint or transfer type')
    rejected('changed device', header() + rows[2] + record(1, 'C', device=8),
             'playback records name different devices')
    rejected('pcap-hidden short acceptance', good, 'USB speaker reported output overrun',
             log + 'usb-audio: output overrun (192 bytes)\n')
    rejected('debug2 absent', good, 'missing or repeated debug2 capture admission', INTERFACE + '\n')
    rejected('debug2 repeated', good, 'missing or repeated debug2 capture admission', log + ENABLED + '\n')
    rejected('inactive speaker', good, 'missing actual stereo interface activation', ENABLED + '\n')
    rejected('absent capture', b'', 'missing or truncated PCAP header')
    rejected('wrong linktype', header(linktype=1) + b''.join(rows), 'unexpected QEMU usbmon PCAP format')
    print(f'usb-audio-pcap: self-test PASS ({cases} synthetic cases; actual USB receipt still requires the guest capture)')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pcap', type=Path)
    parser.add_argument('--qemu-log', type=Path)
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    if args.self_test:
        if args.pcap or args.qemu_log:
            parser.error('--self-test is separate from an actual capture check')
    elif not args.pcap or not args.qemu_log:
        parser.error('--pcap and --qemu-log are both required')
    try:
        if args.self_test:
            self_test()
        else:
            check(args.pcap.read_bytes(), args.qemu_log.read_text(errors='replace'))
            print('usb-audio-pcap: PASS: 11 completed isochronous packets, 2048 PCM bytes and 64 final padding bytes; USB sink receipt only')
    except (OSError, ValueError) as error:
        print('usb-audio-pcap: FAIL: ' + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
