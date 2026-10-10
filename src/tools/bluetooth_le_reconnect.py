"""Exact production LE reconnect/identity decisions with mocked IPC/controller boundaries.

The registered independent Bluetooth self-test calls this proof; actual radio
privacy and stored-key reuse remain mandatory guest scenarios afterward.
"""
import argparse
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile

from bluetooth_audio_lifecycle import item, replace_once


FORGET_IRK = '''\t\tcontroller.le.irks.retain_mut(|(held, irk)| {
\t\t\tif *held != peer {
\t\t\t\treturn true;
\t\t\t}
\t\t\tscrub(irk);
\t\t\tfalse
\t\t});'''
DELETE = 'self.stack.bonds().delete(&local_wire(controller), &peer_to_wire(&peer)).ok_or(Error::Closed)??;'


RESOLVE = '''\t\tif peer_kind == KIND_RANDOM
\t\t\t&& let Some(identity) = controller.le.resolve(&address)
\t\t{
\t\t\tpeer = identity;
\t\t}'''


PAIR_IDENTITY = '\t\tlet identity = if peer[0] == KIND_RANDOM {\n\t\t\tlet mut address = [0u8; 6];\n\t\t\taddress.copy_from_slice(&peer[1..]);\n\t\t\tself.controllers[at].le.resolve(&address).unwrap_or(peer)\n\t\t} else {\n\t\t\tpeer\n\t\t};'
LINK_BOUND = '\t\t// THE LINK BOUND: eight links across both radios.'


def production(root, source_tree):
    main_path = source_tree / 'src/user/services/core/src/bluetooth_service.rs'
    le_path = source_tree / 'src/user/services/core/src/bluetooth_service/le.rs'
    main, le = main_path.read_text(), le_path.read_text()
    actual = item(le, 'pub(crate) struct LeState {')
    actual += '\nimpl LeState {\n' + '\n'.join(item(le, anchor) for anchor in (
        'pub fn new() -> LeState {', 'pub fn local(', 'pub fn resolve(', 'pub fn own_type(')) + '\n}\n'
    actual += '\n'.join(item(main, anchor) for anchor in (
        'fn advertised_kind(', 'fn peer_to_wire(', 'fn peer_from_wire(')) + '\n'
    actual += 'impl Stack {\n' + '\n'.join(item(main, anchor) for anchor in (
        'fn on_connected(', 'fn on_advertisements(', 'fn pair(&mut self, at: usize,')) + '\n'
    actual += '\n'.join(item(le, anchor) for anchor in (
        'pub(crate) fn start_le_pairing(', 'pub(crate) fn pair_le(', 'fn set_aside(')) + '\n}\n'
    actual += "impl OperatorView<'_> {\n" + item(main, 'fn forget(') + '\n}\n'
    actual += 'impl Controller {\n' + item(main, 'fn fail_attempt(') + '\n}\n'
    constants = [line for line in main.splitlines() if line.startswith('const KIND_BREDR:')]
    if len(constants) != 1:
        raise ValueError('ambiguous production KIND_BREDR constant')
    actual += constants[0] + '\n'
    modules = []
    sources = [main_path, le_path]
    for name in ('aes', 'cmac', 'bt_keys', 'bt_bounds', 'hci', 'hci_codec'):
        path = root / 'src/user/services/logic/src' / (name + '.rs')
        modules.append(f'#[path = "{path}"] pub mod {name};')
        sources.append(path)
    pins = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in sources}
    return actual, '\n'.join(modules), pins


def run(root, keep=None, *, source_tree=None, expect_failure=False, forget_only=False, expect_pair_failure=False):
    if expect_pair_failure and (expect_failure or forget_only):
        raise ValueError("the pair failure diagnostic cannot be combined with old negative modes")
    if forget_only and not expect_failure:
        raise ValueError("--forget-only is limited to the original --expect-failure diagnostic")
    root = Path(root).resolve()
    actual, modules, pins = production(root, Path(source_tree).resolve() if source_tree else root)
    fixture = Path(__file__).with_name('bluetooth-le-reconnect.rs').read_text()
    if fixture.count('/* EXACT_PRODUCTION */') != 1 or fixture.count('/* EXACT_MODULES */') != 1:
        raise ValueError('LE reconnect fixture insertion is missing or ambiguous')
    negative = expect_failure or expect_pair_failure
    expected = ('raw-RPA scan choice must create a canonical pairing attempt' if expect_pair_failure else
                'successful forget must retire only the matching cached IRK' if forget_only else
                'only stored identity may select its bond')
    cases = [('production', actual, not negative, expected if negative else None)]
    if not negative:
        cases += [
            ('no-rpa-resolution', replace_once(actual, RESOLVE, ''), False,
             'only stored identity may select its bond'),
            ('identity-used-for-smp', replace_once(actual,
                'Initiator::start(link.local, connection_peer,',
                'Initiator::start(link.local, link.peer,'), False, 'SMP must receive the raw connection address'),
            ('keep-forgotten-irk', replace_once(actual, FORGET_IRK, ''), False,
             'successful forget must retire only the matching cached IRK'),
            ('drop-without-scrubbing', replace_once(actual, '\t\t\tscrub(irk);', ''), False,
             'forgotten IRK must be scrubbed before removal'),
            ('ignore-delete-failure', replace_once(actual, DELETE,
                'let _ = self.stack.bonds().delete(&local_wire(controller), &peer_to_wire(&peer));'), False,
             'failed durable deletion must fail before changing live state'),
            ('raw-pairing-bookkeeping', replace_once(actual, PAIR_IDENTITY, '\t\tlet identity = peer;'), False,
             'raw-RPA scan choice must create a canonical pairing attempt'),
            ('raw-security-lookup', replace_once(actual, 'self.record(at, &identity).map(|record| level_from_wire(&record.level))', 'self.record(at, &peer).map(|record| level_from_wire(&record.level))'), False,
             'raw scan must not bypass legacy-over-SC bond policy'),
            ('resolve-after-failure', replace_once(replace_once(actual, RESOLVE, ''), LINK_BOUND, RESOLVE + '\n' + LINK_BOUND), False,
             'failed raw-RPA completion must end the canonical attempt'),
        ]
    rustc = os.environ.get('RUSTC', 'rustc')
    with tempfile.TemporaryDirectory(prefix='bluetooth-le-reconnect-') as folder:
        work = Path(folder)
        for label, body, should_pass, expected_error in cases:
            code = fixture.replace('/* EXACT_PRODUCTION */', body).replace('/* EXACT_MODULES */', modules)
            rust, binary = work / (label + '.rs'), work / label
            rust.write_text(code)
            command = [rustc, '--edition=2024', '--crate-name', 'bluetooth_le_reconnect', str(rust), '-o', str(binary)]
            built = subprocess.run(command, capture_output=True, text=True, timeout=120)
            if keep is not None:
                target = Path(keep)
                target.mkdir(parents=True, exist_ok=True)
                (target / (label + '.rs')).write_text(code)
                (target / (label + '-compile.log')).write_text(f'{command!r}\nexit: {built.returncode}\n{built.stdout}{built.stderr}')
            if built.returncode:
                raise ValueError(f'LE reconnect {label} did not compile:\n{built.stderr}')
            tested = subprocess.run([str(binary)] + (['--forget-only'] if forget_only else []), capture_output=True, text=True, timeout=15)
            if keep is not None:
                (target / (label + '.log')).write_text(f'run exit: {tested.returncode}\n{tested.stdout}{tested.stderr}')
            if (tested.returncode == 0) != should_pass or (expected_error and expected_error not in tested.stderr):
                raise ValueError(f'LE reconnect {label} gave the wrong verdict:\n{tested.stdout}{tested.stderr}')
            print(f'bluetooth-le-reconnect: {label}: ' + ('PASS31 cases' if should_pass else 'expected causal failure proved'), flush=True)
    print(f'bluetooth-le-reconnect: exact source SHA256 identities {pins}; mocked bond/controller/SMP-start boundaries; not radio/encryption completion', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--keep', type=Path)
    parser.add_argument('--source-tree', type=Path, help='reviewed staged source overlay for pre-apply proof only')
    parser.add_argument('--forget-only', action='store_true', help='isolate the original forget-cache causal negative')
    parser.add_argument('--expect-pair-failure', action='store_true', help='prove the frozen raw-RPA pairing regression')
    parser.add_argument('--expect-failure', action='store_true', help='prove the original missing-identity assertion fails')
    args = parser.parse_args()
    run(args.root, args.keep, source_tree=args.source_tree, expect_failure=args.expect_failure, forget_only=args.forget_only, expect_pair_failure=args.expect_pair_failure)
