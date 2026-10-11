"""Exact-source Bluetooth receive-turn/PCM fairness proof, with bounded IPC/transport models.

The modeled 8.6094 ms synchronous workload is a representative cadence consistent
with retained delivered-packet intervals, NOT a measured IPC blocking duration.
123 HCI bytes / 17 USB bytes require eight full-speed 1 ms service intervals.
The proof demonstrates starvation under a replenished receive queue; it does not
claim that the retained wire log uniquely identifies the live scheduling cause.
"""
from pathlib import Path
import argparse
import hashlib
import importlib.util
import json
import os
import subprocess
import tempfile

# The existing extraction helper is loaded from the explicit root at execution,
# so a partial private source tree need not duplicate unrelated helper files.
item = replace_once = None

SERVICE = 'src/user/services/core/src/bluetooth_service.rs'
AUDIO = 'src/user/services/core/src/bluetooth_service/audio.rs'
KERNEL = 'src/kernel/syscall/mod.rs'


def between(text, first, after):
    if text.count(first) != 1 or text.count(after) != 1:
        raise ValueError(f'voice fairness ambiguous range: {first!r}, {after!r}')
    return text[text.index(first):text.index(after)]


def production(root, source_tree=None):
    hashes = {}

    def read(relative):
        path = (source_tree / relative) if source_tree is not None else root / relative
        if not path.is_file():
            path = root / relative
        content = path.read_text()
        hashes[relative] = hashlib.sha256(content.encode()).hexdigest()
        return content, path.resolve()

    service, _ = read(SERVICE)
    audio, _ = read(AUDIO)
    kernel, _ = read(KERNEL)
    kernel_wait = item(kernel, 'fn sys_wait_any(')
    first_ready = item(kernel_wait, 'for (i, slot) in objects.iter().enumerate().take(n) {')
    main = item(service, 'pub extern "C" fn __user_main(') if 'pub extern "C" fn __user_main(' in service else item(service, 'fn __user_main(')
    wait = between(main, 'let mut waitset: Vec<u64>', 'if ready < 0 {')
    dispatch = between(main, 'if ready < 0 {', 'if let Some(index) = stack.grants.iter().position(|grant| grant.owner == handle) {')
    client = between(main, 'let root_interface = if handle == read_root {', 'if len >= 2 {')
    reply = main[main.index('let mut reply_handles = wire::Handles::new();'):]
    # Remove only the enclosing original infinite loop and entrypoint braces.
    reply = reply.rsplit('}', 2)[0]
    declarations = '\n'.join(item(audio, anchor) for anchor in (
        'pub(crate) fn period_bytes(', 'pub(crate) struct Pcm {', 'impl Pcm {',
        '#[derive(Clone, Copy, PartialEq, Eq, Debug)]\npub(crate) enum Source {',
    ))
    methods = '\n'.join(item(service, anchor) for anchor in ('fn drain_packets(', 'fn on_packet(', 'fn withdraw('))
    methods += '\n' + '\n'.join(item(audio, anchor) for anchor in (
        'pub(crate) fn serve_pcm(', 'pub(crate) fn pcm_timers(', 'pub(crate) fn pcm_deadline(',
        'pub(crate) fn serve_audio_subscriber(', 'fn close_pcm(', 'pub(crate) fn source_of(',
    ))
    dependencies = []
    for name in ('hci', 'le_iso'):
        _, path = read(f'src/user/services/logic/src/{name}.rs')
        dependencies.append(f'#[path = {json.dumps(str(path))}] pub mod {name};')
    pieces = {
        'DEPENDENCIES': 'mod service_logic {\n' + '\n'.join(dependencies) + '\n}',
        'DECLARATIONS': declarations,
        'METHODS': methods,
        'SCRUB': item(service, 'fn scrub('),
        'FIRST_READY': first_ready,
        'WAITSET': wait,
        'DISPATCH': dispatch,
        'CLIENT': client,
        'REPLY': reply,
    }
    return pieces, hashes


def cases_for(actual, expect_failure):
    if expect_failure:
        return [('original', actual, 'VOICE_FAIRNESS_DRAIN_STARVATION', ['fairness'])]
    yield_block = 'if matches!(packet.kind, HciPacketKind::Sco) {\n\t\t\t\treturn true;\n\t\t\t}'
    if actual['METHODS'].count(yield_block) != 1:
        raise ValueError('candidate must contain the reviewed post-SCO yield')
    order_only = dict(actual)
    order_only['METHODS'] = replace_once(actual['METHODS'], yield_block, '')
    yield_only = dict(actual)
    block = item(actual['WAITSET'], 'for controller in &stack.controllers {')
    early = actual['WAITSET'].index('waitset.extend(clients.iter()')
    if actual['WAITSET'].index(block) < early:
        raise ValueError('candidate controller waitset still precedes clients/PCM')
    waiting = replace_once(actual['WAITSET'], block, '')
    yield_only['WAITSET'] = replace_once(waiting,
        'waitset.extend(clients.iter().map(|client| client.chan));',
        block + '\nwaitset.extend(clients.iter().map(|client| client.chan));')
    return [
        ('production', actual, None, []),
        ('order-only', order_only, 'VOICE_FAIRNESS_DRAIN_STARVATION', ['fairness']),
        ('yield-only', yield_only, 'VOICE_FAIRNESS_WAITSET_STARVATION', ['fairness']),
    ]


def run(root, keep=None, source_tree=None, expect_failure=False):
    global item, replace_once
    root = Path(root).resolve()
    helper_path = root / 'src/tools/bluetooth_audio_lifecycle.py'
    helper_spec = importlib.util.spec_from_file_location('voice_fairness_extraction', helper_path)
    helper = importlib.util.module_from_spec(helper_spec)
    helper_spec.loader.exec_module(helper)
    item, replace_once = helper.item, helper.replace_once
    source_tree = Path(source_tree).resolve() if source_tree is not None else None
    actual, hashes = production(root, source_tree)
    fixture = Path(__file__).with_name('bluetooth-voice-fairness.rs').read_text()
    target = Path(keep) if keep is not None else None
    if target is not None:
        target.mkdir(parents=True, exist_ok=True)
        (target / 'source-inputs.json').write_text(json.dumps(hashes, indent=2) + '\n')
    rustc = os.environ.get('RUSTC', 'rustc')
    with tempfile.TemporaryDirectory(prefix='bluetooth-voice-fairness-') as folder:
        work = Path(folder)
        for label, pieces, marker, arguments in cases_for(actual, expect_failure):
            code = fixture
            for key, value in pieces.items():
                code = replace_once(code, f'/* EXACT_{key} */', value)
            rust, binary = work / f'{label}.rs', work / label
            rust.write_text(code)
            command = [rustc, '--edition=2024', '--crate-name', 'bluetooth_voice_fairness', str(rust), '-o', str(binary)]
            built = subprocess.run(command, capture_output=True, text=True, timeout=120)
            log = f'compile: {command!r}\nexit: {built.returncode}\n{built.stdout}{built.stderr}'
            if target is not None:
                (target / f'{label}.rs').write_text(code)
                (target / f'{label}.log').write_text(log)
            if built.returncode:
                raise ValueError(f'voice fairness {label} did not compile (not an accepted negative):\n{built.stderr}')
            tested = subprocess.run([str(binary), *arguments], capture_output=True, text=True, timeout=15)
            log += f'\nrun: {[str(binary), *arguments]!r}\nexit: {tested.returncode}\n{tested.stdout}{tested.stderr}'
            if target is not None:
                (target / f'{label}.log').write_text(log)
            output = tested.stdout + tested.stderr
            if marker is None:
                accepted = tested.returncode == 0 and 'VOICE_FAIRNESS_COMPLETE' in output
            else:
                accepted = tested.returncode != 0 and marker in output
            if not accepted:
                raise ValueError(f'voice fairness {label} wrong verdict (required {marker or "success"}):\n{output}')
            print(f'bluetooth-voice-fairness: {label}: ' + ('PASS' if marker is None else f'causal negative accepted: {marker}'), flush=True)
    print('bluetooth-voice-fairness: exact drain/on_packet/waitset/first-ready/PCM methods; IPC wire and profile transport boundaries modeled. 123/17 -> 8 FS intervals; 8.6094 ms workload vs 8.5634 ms arrivals is representative retained wire cadence, not measured blocking latency or a live scheduling trace.', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--source-tree', type=Path)
    parser.add_argument('--keep', type=Path)
    parser.add_argument('--expect-failure', action='store_true')
    args = parser.parse_args()
    run(args.root, args.keep, args.source_tree, args.expect_failure)
