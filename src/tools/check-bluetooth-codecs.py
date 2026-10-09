#!/usr/bin/env python3
"""Independent, host-only codec differential gate. See bluetooth-oracles.md for provenance.

The adapter calls the shipping Rust leaves. Each encoder's frames are decoded by BOTH
implementations, with fresh state for each stream. Neither expected PCM nor expected
frames are computed by the code under test. Dependencies are prepared explicitly.
"""
import argparse
import ctypes as c
import itertools
import hashlib
import json
import math
from pathlib import Path
import random
import struct
import subprocess

ROOT = Path(__file__).resolve().parents[2]
WORK = ROOT / '.build/bluetooth-oracles'
I16 = c.POINTER(c.c_int16)
U8 = c.POINTER(c.c_uint8)


def declare(lib, name, result, *args):
    function = getattr(lib, name)
    function.restype, function.argtypes = result, args
    return function


def packets(data):
    result = []
    while data:
        size, = struct.unpack_from('<H', data)
        assert len(data) >= size + 2, 'truncated adapter reply'
        result.append(data[2:2 + size])
        data = data[2 + size:]
    return result


def rust(codec, operation, config, frames):
    wire = b''.join(struct.pack('<H', len(frame)) + frame for frame in frames)
    result = subprocess.run([str(WORK / 'codec-host'), codec, operation, *map(str, config)], input=wire, capture_output=True, check=True)
    answer = packets(result.stdout)
    assert len(answer) == len(frames), 'adapter omitted a frame'
    return answer


def pcm_bytes(samples):
    return struct.pack(f'<{len(samples)}h', *samples)


def pcm_samples(data):
    return struct.unpack(f'<{len(data) // 2}h', data)


def signals(rate, samples, channels, frames=32):
    """Silence, impulses, swept/mixed tones, full-scale and deterministic noise.

    State is continuous across changes; channel two differs from channel one.
    This catches channel swaps and stale filter history as well as syntax errors.
    """
    rng = random.Random(0x194)
    result = []
    for frame in range(frames):
        output = []
        for i in range(samples):
            t = (frame * samples + i) / rate
            for channel in range(channels):
                if frame < 3:
                    value = 0
                elif frame < 6:
                    value = 24000 if i == channel * 3 else 0
                elif frame < 20:
                    value = round(11000 * math.sin(2 * math.pi * (397 + channel * 631) * t) + 6000 * math.sin(2 * math.pi * (rate * .17) * t))
                elif frame < 23:
                    value = 32767 if (frame + i + channel) % 2 else -32768
                else:
                    value = rng.randrange(-18000, 18001)
                output.append(value)
        result.append(pcm_bytes(output))
    return result


def difference(left, right):
    a, b = pcm_samples(b''.join(left)), pcm_samples(b''.join(right))
    assert len(a) == len(b) and a
    errors = [x - y for x, y in zip(a, b)]
    rms = math.sqrt(sum(e * e for e in errors) / len(errors)) / 32768
    return max(abs(e) for e in errors) / 32768, 20 * math.log10(rms) if rms else -999


class Sbc:
    def __init__(self):
        self.encoder = c.CDLL(str(WORK / 'sbc-encoder.so'))
        self.decoder = c.CDLL(str(WORK / 'sbc-decoder.so'))
        self.new_encoder = declare(self.encoder, 'oracle_encoder_new', c.c_void_p, *([c.c_int] * 7))
        self.encode_frame = declare(self.encoder, 'oracle_encode', c.c_int, c.c_void_p, I16, U8)
        self.free_encoder = declare(self.encoder, 'oracle_encoder_free', None, c.c_void_p)
        self.new_decoder = declare(self.decoder, 'oracle_decoder_new', c.c_void_p, c.c_int, c.c_int)
        self.decode_frame = declare(self.decoder, 'oracle_decode', c.c_int, c.c_void_p, U8, c.c_uint32, I16, c.c_uint32)
        self.free_decoder = declare(self.decoder, 'oracle_decoder_free', None, c.c_void_p)

    def encode(self, config, frames, msbc):
        rate, blocks, mode, allocation, bands, pool = config
        context = self.new_encoder([16000, 32000, 44100, 48000].index(rate), blocks, mode, allocation, bands, pool, msbc)
        assert context, 'AOSP encoder setup failed'
        try:
            result = []
            for data in frames:
                pcm = (c.c_int16 * (len(data) // 2)).from_buffer_copy(data)
                out = (c.c_uint8 * 1024)()
                n = self.encode_frame(context, pcm, out)
                assert 0 < n <= len(out), f'AOSP encoded length {n}'
                result.append(bytes(out[:n]))
            return result
        finally:
            self.free_encoder(context)

    def decode(self, config, frames, msbc):
        _, blocks, mode, _, bands, _ = config
        count = blocks * bands * (1 if mode == 0 else 2)
        context = self.new_decoder(1 if mode == 0 else 2, msbc)
        assert context, 'AOSP decoder setup failed'
        try:
            result = []
            for data in frames:
                # AOSP's HFP decoder consumes the H2 transport's trailing padding
                # octet as well as the 57-byte mSBC frame; its encoder emits only
                # the codec frame. Do not add that transport byte to Liber's codec.
                if msbc:
                    data += b'\0'
                frame = (c.c_uint8 * len(data)).from_buffer_copy(data)
                pcm = (c.c_int16 * count)()
                n = self.decode_frame(context, frame, len(data), pcm, c.sizeof(pcm))
                assert n == c.sizeof(pcm), f'AOSP refused frame: {n}, expected {c.sizeof(pcm)}'
                result.append(bytes(pcm))
            return result
        finally:
            self.free_decoder(context)


class Lc3:
    def __init__(self):
        self.lib = c.CDLL(str(WORK / 'liblc3/bin/liblc3.so'))
        for kind in ('encoder', 'decoder'):
            declare(self.lib, f'lc3_{kind}_size', c.c_uint, c.c_int, c.c_int)
            declare(self.lib, f'lc3_setup_{kind}', c.c_void_p, c.c_int, c.c_int, c.c_int, c.c_void_p)
        self.encode_frame = declare(self.lib, 'lc3_encode', c.c_int, c.c_void_p, c.c_int, c.c_void_p, c.c_int, c.c_int, c.c_void_p)
        self.decode_frame = declare(self.lib, 'lc3_decode', c.c_int, c.c_void_p, c.c_void_p, c.c_int, c.c_int, c.c_void_p, c.c_int)

    def process(self, config, frames, encoding=False):
        rate, duration = config
        # 44.1 kHz uses the 48 kHz LC3 configuration with a scaled frame duration.
        codec_rate = 48000 if rate == 44100 else rate
        kind = 'encoder' if encoding else 'decoder'
        memory = c.create_string_buffer(getattr(self.lib, f'lc3_{kind}_size')(duration, codec_rate))
        context = getattr(self.lib, f'lc3_setup_{kind}')(duration, codec_rate, codec_rate, memory)
        assert context, 'liblc3 setup failed'
        result = []
        for data in frames:
            if encoding:
                size, = struct.unpack_from('<H', data)
                pcm = c.create_string_buffer(data[2:])
                out = c.create_string_buffer(size)
                assert self.encode_frame(context, 0, pcm, 1, size, out) == 0, 'liblc3 encoder refused frame'
                result.append(out.raw)
            else:
                out = c.create_string_buffer(2 * codec_rate * duration // 1000000)
                frame = c.create_string_buffer(data)
                assert self.decode_frame(context, frame, len(data), 0, out, 1) == 0, 'liblc3 decoder refused frame'
                result.append(out.raw)
        return result


class GoogleSbc:
    def __init__(self):
        self.lib = c.CDLL(str(WORK / 'google-sbc.so'))
        self.new = declare(self.lib, 'google_sbc_new', c.c_void_p)
        self.frame = declare(self.lib, 'google_sbc_decode', c.c_int, c.c_void_p, c.c_void_p, c.c_uint, I16)
        self.free = declare(self.lib, 'google_sbc_free', None, c.c_void_p)

    def decode(self, frames):
        state = self.new()
        assert state, 'Google SBC decoder setup failed'
        try:
            result = []
            for data in frames:
                pcm = (c.c_int16 * 256)()
                frame = c.create_string_buffer(data)
                size = self.frame(state, frame, len(data), pcm)
                assert 0 < size <= c.sizeof(pcm), f'Google SBC refused frame: {size}: {data.hex()}'
                result.append(bytes(pcm)[:size])
            return result
        finally:
            self.free(state)


def check_sbc(quick):
    oracle = Sbc()
    google = GoogleSbc()
    configs = list(itertools.product((16000, 32000, 44100, 48000), (4, 8, 12, 16), range(4), range(2), (4, 8)))
    if quick:
        configs = [(48000, 16, 3, 0, 8), (16000, 4, 0, 1, 4)]
    cases = [(False, (*config, pool)) for config in configs for pool in (2, 26, 53)]
    cases.append((True, (16000, 15, 0, 0, 8, 26)))
    worst = [0.0, -999.0]
    frames_checked = 0
    for msbc, config in cases:
        codec = 'msbc' if msbc else 'sbc'
        rate, blocks, mode, _, bands, _ = config
        # AOSP's 4-band synthesis overflows its signed 32-bit accumulator on
        # full-scale low-bitpool overload (retained sanitizer proof in the audit).
        # Keep this precision comparison 12 dB below overload; never change the
        # shipping saturating decoder to reproduce the reference's overflow.
        source = [pcm_bytes([sample // 4 for sample in pcm_samples(frame)]) for frame in signals(rate, blocks * bands, 1 if mode == 0 else 2)]
        for producer, encoded in [('aosp', oracle.encode(config, source, msbc)), ('liber', rust(codec, 'encode', config, source))]:
            actual = rust(codec, 'decode', config, encoded)
            # Google's public API restricts compressed frames to the source PCM
            # size. AOSP also covers legal SBC pools outside that API's range.
            references = [('google', google.decode(encoded))] if max(map(len, encoded)) <= len(source[0]) else []
            # Pinned AOSP ReadScalefactors initializes bitPtr to 32 then calls
            # ReadUINT4Aligned, whose precondition is bitPtr < 16, for joint/4.
            # Its resulting wrong scale factor is not an acceptable expected value.
            if not (mode == 3 and bands == 4):
                references.append(('aosp', oracle.decode(config, encoded, msbc)))
            assert references, f'no independent decoder covers {config}'
            for reference, expected in references:
                mad, rms_db = difference(actual, expected)
                worst = [max(worst[0], mad), max(worst[1], rms_db)]
                # Fixed point syntheses are independently quantized. Each must
                # reconstruct the SAME stream within 2 PCM bits RMS and 16 peak samples.
                assert mad <= 16 / 32768 and rms_db <= 20 * math.log10(4 / 32768), (codec, config, producer, reference, mad, rms_db)
            frames_checked += len(encoded)
    print(f'bluetooth-codecs: SBC/mSBC PASS {len(cases)} configurations, {frames_checked} cross-decoded frames; maximum normalized difference {worst[0]:.8f}, worst RMS {worst[1]:.2f} dB', flush=True)
    return dict(configurations=len(cases), frames=frames_checked, max_difference=worst[0], worst_rms_db=worst[1])


def check_lc3(quick):
    oracle = Lc3()
    configs = list(itertools.product((8000, 16000, 24000, 32000, 44100, 48000), (7500, 10000)))
    if quick:
        configs = [(16000, 10000), (48000, 7500)]
    worst = [0.0, -999.0]
    frames_checked = 0
    for config in configs:
        rate, duration = config
        count = (48000 if rate == 44100 else rate) * duration // 1000000
        source = signals(rate, count, 1, frames=96)
        # Exercise every legal frame size, including external rate adaptation.
        sizes = (20, 21, 30, 40, 60, 80, 100, 155, 200, 300, 399, 400)
        if not quick:
            # First cross high/low-rate LTPF boundaries repeatedly, then visit
            # every byte count. The first segment retains the transition regression.
            source = signals(rate, count, 1, frames=96 + 381)
            sizes = sizes * 8 + tuple(range(20, 401))
        framed = [struct.pack('<H', sizes[i % len(sizes)]) + data for i, data in enumerate(source)]
        for producer, encoded in [('google', oracle.process(config, framed, True)), ('liber', rust('lc3', 'encode', config, framed))]:
            expected = oracle.process(config, encoded)
            actual = rust('lc3', 'decode', config, encoded)
            mad, rms_db = difference(actual, expected)
            worst = [max(worst[0], mad), max(worst[1], rms_db)]
            # LC3 16-bit decoder precision thresholds, also reported in liblc3's
            # upstream speech_decode_10m/7m5 conformance reports. Passing synthetic
            # differential vectors is not the SIG's member-only qualification suite.
            assert mad <= .00148 and rms_db <= -89.06, (config, producer, mad, rms_db)
            frames_checked += len(encoded)
    print(f'bluetooth-codecs: LC3 PASS {len(configs)} configurations, {frames_checked} cross-decoded frames; maximum normalized difference {worst[0]:.8f}, worst RMS {worst[1]:.2f} dB', flush=True)
    return dict(configurations=len(configs), frames=frames_checked, max_difference=worst[0], worst_rms_db=worst[1])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--quick', action='store_true', help='bounded development check; does not establish the full gate')
    parser.add_argument('--codec', choices=('sbc', 'lc3', 'all'), default='all')
    parser.add_argument('--no-build', action='store_true', help='development only: reuse the last host adapters')
    args = parser.parse_args()
    if not args.no_build:
        subprocess.run(['python3', str(ROOT / 'src/harness/prepare-bluetooth-oracles.py'), '--only', 'codecs', '--offline'], check=True)
    results = {}
    if args.codec in ('sbc', 'all'):
        results['sbc'] = check_sbc(args.quick)
    if args.codec in ('lc3', 'all'):
        results['lc3'] = check_lc3(args.quick)
    results['sources'] = json.loads((ROOT / 'src/harness/bluetooth-oracles-sources.json').read_text())
    leaves = [ROOT / 'src/user/services/logic/src/sbc.rs', ROOT / 'src/user/services/logic/src/lc3.rs', *sorted((ROOT / 'src/user/services/logic/src/lc3').glob('*.rs'))]
    results['production_sha256'] = {str(path.relative_to(ROOT)): hashlib.sha256(path.read_bytes()).hexdigest() for path in leaves}
    results['no_build'] = args.no_build
    (WORK / ('quick-result.json' if args.quick else 'result.json')).write_text(json.dumps(results, indent=2) + '\n')


if __name__ == '__main__':
    main()
