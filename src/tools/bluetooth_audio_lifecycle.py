"""Host proof of production Bluetooth audio retirement with mocked IPC/profile boundaries.

Invoked by the registered bluetooth-independent gate's unconditional self_test, and by
its --self-test mode. This does not claim a live radio or real kernel IPC overflow.
"""
from pathlib import Path
import argparse
import hashlib
import os
import subprocess
import tempfile


def item(source, anchor):
    """Take the complete, uniquely named production item, never a copied test implementation."""
    if source.count(anchor) != 1:
        raise ValueError(f'Bluetooth lifecycle source anchor is missing or ambiguous: {anchor}')
    begin = source.index(anchor)
    opening = source.index('{', begin)
    depth, end = 1, opening + 1
    while depth and end < len(source):
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    if depth:
        raise ValueError(f'Bluetooth lifecycle item is incomplete: {anchor}')
    return source[begin:end]


def replace_once(source, before, after):
    if source.count(before) != 1:
        raise ValueError(f'Bluetooth lifecycle mutation is missing or ambiguous: {before}')
    return source.replace(before, after, 1)


def production(root):
    path = root / 'src/user/services/core/src/bluetooth_service/audio.rs'
    source = path.read_text()
    constant = 'const MAX_ENDPOINTS: usize = '
    begin = source.index(constant)
    declarations = [source[begin:source.index(';', begin) + 1]]
    declarations += [item(source, anchor) for anchor in (
        'pub(crate) struct Pcm {', 'impl Pcm {',
        'pub(crate) struct AudioRoot {', 'impl AudioRoot {',
        "pub(crate) struct AudioView<'a> {", "impl bluetooth_audio::Service for AudioView<'_> {",
        'pub(crate) fn serve_endpoints(',
        '#[derive(Clone, Copy, PartialEq, Eq, Debug)]\npub(crate) enum Source {',
    )]
    declarations.append('impl Stack {\n' + '\n'.join(item(source, anchor) for anchor in (
        'pub(crate) fn serve_audio_subscriber(', 'fn close_pcm(', 'pub(crate) fn source_of(',
    )) + '\n}')
    return '\n\n'.join(declarations), hashlib.sha256(source.encode()).hexdigest()


def run(root, keep=None):
    root = Path(root).resolve()
    actual, digest = production(root)
    fixture = Path(__file__).with_name('bluetooth-audio-lifecycle.rs').read_text()
    if fixture.count('/* EXACT_PRODUCTION */') != 1:
        raise ValueError('Bluetooth lifecycle fixture has no unique production insertion')
    cases = [
        ('production', actual, True),
        ('ignore-full-stream', replace_once(actual, 'self.subscriber_failed = true;', 'self.subscriber_failed = false;'), False),
        ('close-before-bulk-owner-removal', replace_once(actual,
            'for pcm in core::mem::take(&mut self.audio.pcm) {',
            'while !self.audio.pcm.is_empty() { let pcm = self.audio.pcm.remove(0);'), False),
        ('reopen-without-subscriber', replace_once(actual, 'if self.stack.audio.subscriber == 0 {', 'if false {'), False),
        ('reset-microphone-in-snapshot', replace_once(actual,
            'if let Ok(volume) = self.microphone_volume(id) {',
            'if let Ok(volume) = Ok::<u8, Error>(100) {'), False),
        ('replacement-stays-failed', replace_once(actual,
            'stack.audio.subscriber_failed = false;', 'stack.audio.subscriber_failed = true;'), False),
    ]
    rustc = os.environ.get('RUSTC', 'rustc')
    with tempfile.TemporaryDirectory(prefix='bluetooth-audio-lifecycle-') as folder:
        work = Path(folder)
        for label, body, should_pass in cases:
            code = fixture.replace('/* EXACT_PRODUCTION */', body)
            rust, binary = work / f'{label}.rs', work / label
            rust.write_text(code)
            command = [rustc, '--edition=2024', '--crate-name', 'bluetooth_audio_lifecycle', str(rust), '-o', str(binary)]
            built = subprocess.run(command, capture_output=True, text=True, timeout=120)
            if built.returncode:
                raise ValueError(f'Bluetooth audio lifecycle {label} did not compile:\n{built.stderr}')
            tested = subprocess.run([str(binary)], capture_output=True, text=True, timeout=15)
            if keep is not None:
                target = Path(keep)
                target.mkdir(parents=True, exist_ok=True)
                (target / f'{label}.rs').write_text(code)
                (target / f'{label}.log').write_text(f'compile: {command!r}\nexit: {built.returncode}\n{built.stderr}\nrun exit: {tested.returncode}\n{tested.stdout}{tested.stderr}')
            if (tested.returncode == 0) != should_pass:
                raise ValueError(f'Bluetooth audio lifecycle {label} gave the wrong verdict:\n{tested.stdout}{tested.stderr}')
            print(f'bluetooth-audio-lifecycle: {label}: ' + ('PASS' if should_pass else 'mutation correctly refused'), flush=True)
    print(f'bluetooth-audio-lifecycle: exact production audio.rs sha256={digest}; bounded depth64/65th Stalled, complete PCM retirement before profile hooks, call reset, retained offers/gains, stale-open refusal and replacement snapshot/open PASS (mocked IPC/profile/wire boundaries; not live Bluetooth overflow)', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--keep', type=Path)
    arguments = parser.parse_args()
    run(arguments.root, arguments.keep)
