"""Exact-source radio voice accepted-prefix continuity proof.

Production tone helpers, radio_voice write/cursor, Stream buffer acceptance and
PCM Format execute against explicitly modeled memory capabilities and voice IPC.
This does not test a guest, kernel, controller, SCO transport or live scheduling.
"""
import argparse
import ast
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import subprocess
import struct
import tempfile

item = replace_once = None


def production(root, source_tree=None):
    pins = {}

    def read(relative):
        candidate = source_tree / relative if source_tree is not None else root / relative
        path = candidate if candidate.is_file() else root / relative
        content = path.read_text()
        pins[relative] = hashlib.sha256(path.read_bytes()).hexdigest()
        return content

    def constant(source, name):
        prefix = f'const {name}:'
        matches = [line for line in source.splitlines() if line.startswith(prefix)]
        if len(matches) != 1:
            raise ValueError(f'voice partial write missing/ambiguous constant {name}')
        return matches[0]

    probe = read('src/user/services/core/src/audioprobe.rs')
    engine = read('src/user/services/core/src/audio_engine.rs')
    pcm = read('src/user/libs/audio/pcm/src/lib.rs')
    radio = item(probe, 'fn radio_voice(')
    write_start = 'match client().write('
    if radio.count(write_start) != 1:
        raise ValueError('radio_voice must contain one identifiable write operation')
    # The baseline has no cursor. This fixture-only initial value makes its
    # unchanged write block compile; it does not change the baseline waveform.
    cursor = 'let mut written_frames = 0usize;'
    if 'written_frames' in radio and radio.count(cursor) != 1:
        raise ValueError('radio_voice cursor declaration does not match the reviewed source')
    if 'written_frames' in radio:
        begin = radio.index(cursor)
        cursor = radio[begin:radio.index(';', begin) + 1]
    tone = [item(probe, anchor) for anchor in ('fn tone(', 'fn tone_channels(')]
    if 'fn tone_channels_from(' in probe:
        tone.append(item(probe, 'fn tone_channels_from('))
    stream = [constant(engine, 'MAX_QUEUED_FRAMES'), item(engine,
        '#[derive(Clone, Copy, PartialEq, Eq, Debug)]\nenum Front {'), item(engine, 'struct Stream {')]
    implementation = item(engine, 'impl Stream {')
    stream.append('impl Stream {\n' + '\n'.join(item(implementation, anchor) for anchor in (
        'fn new(', 'fn queued_frames(', 'fn capacity(', 'fn next_frame(',
        'fn compact(', 'fn append(', 'fn write_buffer(',
    )) + '\n}')
    format_constants = []
    for name in ('OUTPUT_RATE', 'MIN_RATE'):
        prefix = f'pub const {name}:'
        lines = [line for line in pcm.splitlines() if line.startswith(prefix)]
        if len(lines) != 1:
            raise ValueError(f'PCM Format constant {name} missing/ambiguous')
        format_constants.append(lines[0])
    return {
        'FORMAT': '\n'.join(format_constants + [item(pcm,
            '#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub struct Format {'), item(pcm, 'impl Format {')]),
        'STREAM': '\n'.join(stream),
        'TONE': '\n'.join(tone),
        'CURSOR': cursor,
        'WRITE': item(radio, write_start),
    }, pins


def cases_for(actual, expect_failure):
    if expect_failure:
        return [('original', actual, 'VOICE_PARTIAL_CONTINUITY', 'continuity')]
    fixed = 'written_frames += accepted as usize;'
    if actual['WRITE'].count(fixed) != 1:
        raise ValueError('candidate must advance its cursor by the accepted source-frame count')
    phase_reset = dict(actual)
    phase_reset['TONE'] = replace_once(actual['TONE'],
        '(first_frame + at / channels) / half_period', '(at / channels) / half_period')
    requested_count = dict(actual)
    requested_count['WRITE'] = replace_once(actual['WRITE'], fixed, 'written_frames += 480;')
    return [
        ('production', actual, None, 'all'),
        ('reset-phase', phase_reset, 'VOICE_PARTIAL_CONTINUITY', 'continuity'),
        ('advance-requested-not-accepted', requested_count, 'VOICE_PARTIAL_CONTINUITY', 'continuity'),
    ]


def run(root, keep=None, *, source_tree=None, expect_failure=False):
    global item, replace_once
    root = Path(root).resolve()
    helper_path = root / 'src/tools/bluetooth_audio_lifecycle.py'
    spec = importlib.util.spec_from_file_location('voice_partial_write_extraction', helper_path)
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    item, replace_once = helper.item, helper.replace_once
    actual, pins = production(root, Path(source_tree).resolve() if source_tree else None)
    peer_relative = 'src/harness/bluetooth_classic_peers.py'
    peer_path = Path(source_tree).resolve() / peer_relative if source_tree else root / peer_relative
    if not peer_path.is_file():
        peer_path = root / peer_relative
    peer_source = peer_path.read_bytes()
    pins[peer_relative] = hashlib.sha256(peer_source).hexdigest()
    peer_ast = ast.parse(peer_source, filename=str(peer_path))
    functions = [node for node in peer_ast.body if isinstance(node, ast.FunctionDef) and node.name == 'audio_metrics']
    if len(functions) != 1:
        raise ValueError('independent peer audio_metrics is missing/ambiguous')
    metrics_ast = ast.Module(body=functions, type_ignores=[])
    metrics_globals = {'math': math, 'struct': struct}
    # Only the exact function is loaded: never import Bumble or the peer module.
    exec(compile(metrics_ast, str(peer_path), 'exec'), metrics_globals)
    metrics = metrics_globals['audio_metrics']
    fixture = Path(__file__).with_name('bluetooth-voice-partial-write.rs').read_text()
    target = Path(keep) if keep is not None else None
    if target is not None:
        target.mkdir(parents=True, exist_ok=True)
        (target / 'source-inputs.json').write_text(json.dumps(pins, indent=2) + '\n')
    rustc = os.environ.get('RUSTC', 'rustc')
    with tempfile.TemporaryDirectory(prefix='bluetooth-voice-partial-write-') as directory:
        work = Path(directory)
        for label, pieces, marker, selection in cases_for(actual, expect_failure):
            code = fixture
            for key, value in pieces.items():
                code = replace_once(code, f'/* EXACT_{key} */', value)
            rust, binary = work / f'{label}.rs', work / label
            rust.write_text(code)
            command = [rustc, '--edition=2024', '--crate-name', 'bluetooth_voice_partial_write', str(rust), '-o', str(binary)]
            built = subprocess.run(command, capture_output=True, text=True, timeout=120)
            log = f'compile: {command!r}\nexit: {built.returncode}\n{built.stdout}{built.stderr}'
            if target is not None:
                (target / f'{label}.rs').write_text(code)
                (target / f'{label}.log').write_text(log)
            if built.returncode:
                raise ValueError(f'voice partial write {label} did not compile (not an accepted negative):\n{built.stderr}')
            pcm_path = work / f'{label}.pcm'
            arguments = [str(binary), selection, str(pcm_path)]
            tested = subprocess.run(arguments, capture_output=True, text=True, timeout=15)
            output = tested.stdout + tested.stderr
            log += f'\nrun: {arguments!r}\nexit: {tested.returncode}\n{output}'
            if target is not None:
                (target / f'{label}.log').write_text(log)
            if selection in ('all', 'continuity'):
                pcm_bytes = pcm_path.read_bytes()
                result = metrics(pcm_bytes, 16_000, 1)
                expected_frequency = 333 if marker is None else 1000
                log += f'\nunchanged independent audio_metrics: {result!r}\n'
                if target is not None:
                    (target / f'{label}.pcm').write_bytes(pcm_bytes)
                    (target / f'{label}-metrics.json').write_text(json.dumps(result, indent=2) + '\n')
                    (target / f'{label}.log').write_text(log)
                if result['samples'] < 16_000 or result['rms'] <= 500 or result['peak_frequency'] != expected_frequency:
                    raise ValueError(f'voice partial write {label} did not reproduce expected {expected_frequency} Hz using unchanged peer metrics: {result}')
            if target is not None:
                (target / f'{label}.log').write_text(log)
            if marker is None:
                accepted = tested.returncode == 0 and 'VOICE_PARTIAL_COMPLETE' in output
            else:
                accepted = tested.returncode != 0 and marker in output
            if not accepted:
                raise ValueError(f'voice partial write {label} wrong verdict (required {marker or "success"}):\n{output}')
            print(f'bluetooth-voice-partial-write: {label}: ' + ('PASS' if marker is None else f'causal negative accepted: {marker}'), flush=True)
    print(f'bluetooth-voice-partial-write: exact source SHA256 identities {pins}; production 4096-frame capacity, 160-frame drains, accepted-prefix continuity, unchanged independent spectrum metrics (old 1000 Hz, corrected 333 Hz), legacy tones and refusal ownership; memory capabilities and IPC modeled, not live Bluetooth.', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--source-tree', type=Path)
    parser.add_argument('--keep', type=Path)
    parser.add_argument('--expect-failure', action='store_true')
    args = parser.parse_args()
    run(args.root, args.keep, source_tree=args.source_tree, expect_failure=args.expect_failure)
