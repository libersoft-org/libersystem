"""Exact production LE Audio set membership with mocked storage/pairing boundaries.

RSI validation, privacy resolution and member bookkeeping are the production
methods. The companion LE reconnect proof and real radio gates verify pairing.
"""
import argparse
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile

from bluetooth_audio_lifecycle import item, replace_once


KNOWN_BOND_FAILURE = 'known canonical bond must prevent redundant set pairing'


def production(root, source_tree):
    main_path = source_tree / 'src/user/services/core/src/bluetooth_service.rs'
    le_path = source_tree / 'src/user/services/core/src/bluetooth_service/le.rs'
    audio_path = source_tree / 'src/user/services/core/src/bluetooth_service/le_audio.rs'
    logic_path = root / 'src/user/services/logic/src/le_audio.rs'
    main, le, audio, logic = (path.read_text() for path in (main_path, le_path, audio_path, logic_path))
    actual = item(le, 'pub(crate) struct LeState {')
    actual += '\nimpl LeState {\n' + '\n'.join(item(le, anchor) for anchor in (
        'pub fn new() -> LeState {', 'pub fn resolve(')) + '\n}\n'
    actual += '\n'.join(item(main, anchor) for anchor in ('fn peer_to_wire(', 'fn peer_from_wire(')) + '\n'
    actual += 'impl LeDevice {\n' + item(audio, 'fn holds(') + '\n}\n'
    actual += 'impl Stack {\n' + '\n'.join(item(audio, anchor) for anchor in (
        'pub(crate) fn le_audio_advertised(', 'pub(crate) fn le_audio_bonded(')) + '\n}\n'
    actual += 'mod lea {\nuse crate::aes;\n' + '\n'.join(item(logic, anchor) for anchor in (
        'pub mod ad {', 'fn ad_field(', 'pub fn rsi(', 'pub fn sih(',
        'pub fn rsi_resolves(', 'pub fn make_rsi(')) + '\n}\n'
    modules = []
    sources = [main_path, le_path, audio_path, logic_path]
    for name in ('aes', 'cmac', 'bt_keys'):
        path = root / 'src/user/services/logic/src' / (name + '.rs')
        modules.append(f'#[path = "{path}"] pub mod {name};')
        sources.append(path)
    pins = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in sources}
    return actual, '\n'.join(modules), pins


def run(root, keep=None, *, source_tree=None, expect_failure=False):
    root = Path(root).resolve()
    actual, modules, pins = production(root, Path(source_tree).resolve() if source_tree else root)
    fixture_path = Path(__file__).with_name('bluetooth-le-audio-identity.rs')
    fixture = fixture_path.read_text()
    if fixture.count('/* EXACT_PRODUCTION */') != 1 or fixture.count('/* EXACT_MODULES */') != 1:
        raise ValueError('LE Audio identity fixture insertion is missing or ambiguous')
    pins[str(fixture_path)] = hashlib.sha256(fixture_path.read_bytes()).hexdigest()
    cases = [('production', actual, not expect_failure, KNOWN_BOND_FAILURE if expect_failure else None)]
    if not expect_failure:
        cases += [
            ('identity-used-for-connection', replace_once(actual,
                'self.pair_le(at, connection_peer, false)', 'self.pair_le(at, peer, false)'),
             False, 'set pairing must retain the raw connection address'),
            ('raw-member-kept', replace_once(actual,
                'self.le_devices[index].members.push(peer);',
                'self.le_devices[index].members.push(connection_peer);'),
             False, 'pending set member must match canonical pairing bookkeeping'),
        ]
    rustc = os.environ.get('RUSTC', 'rustc')
    with tempfile.TemporaryDirectory(prefix='bluetooth-le-audio-identity-') as folder:
        work = Path(folder)
        for label, body, should_pass, expected_error in cases:
            code = fixture.replace('/* EXACT_PRODUCTION */', body).replace('/* EXACT_MODULES */', modules)
            rust, binary = work / (label + '.rs'), work / label
            rust.write_text(code)
            command = [rustc, '--edition=2024', '--crate-name', 'bluetooth_le_audio_identity', str(rust), '-o', str(binary)]
            built = subprocess.run(command, capture_output=True, text=True, timeout=120)
            if keep is not None:
                target = Path(keep)
                target.mkdir(parents=True, exist_ok=True)
                (target / (label + '.rs')).write_text(code)
                (target / (label + '-compile.log')).write_text(f'{command!r}\nexit: {built.returncode}\n{built.stdout}{built.stderr}')
            if built.returncode:
                raise ValueError(f'LE Audio identity {label} did not compile:\n{built.stderr}')
            tested = subprocess.run([str(binary)], capture_output=True, text=True, timeout=15)
            if keep is not None:
                (target / (label + '.log')).write_text(f'run exit: {tested.returncode}\n{tested.stdout}{tested.stderr}')
            if (tested.returncode == 0) != should_pass or (expected_error and expected_error not in tested.stderr):
                raise ValueError(f'LE Audio identity {label} gave the wrong verdict:\n{tested.stdout}{tested.stderr}')
            print(f'bluetooth-le-audio-identity: {label}: ' + ('PASS10 cases' if should_pass else 'expected causal failure proved'), flush=True)
    if any(hashlib.sha256(Path(path).read_bytes()).hexdigest() != digest for path, digest in pins.items()):
        raise ValueError('LE Audio identity source changed during verification')
    print(f'bluetooth-le-audio-identity: exact source SHA256 identities {pins}; mocked storage/pairing boundaries; not radio/encryption completion', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--keep', type=Path)
    parser.add_argument('--source-tree', type=Path, help='reviewed staged runtime source overlay for pre-apply proof only')
    parser.add_argument('--expect-failure', action='store_true', help='prove the original known-bond membership assertion fails after successful compilation')
    args = parser.parse_args()
    run(args.root, args.keep, source_tree=args.source_tree, expect_failure=args.expect_failure)
