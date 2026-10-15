"""Exact-source OPP receiver lifecycle with real OBEX/RFCOMM state machines.

IPC delivery, controller/link lookup, policy refresh and the L2CAP frame carrier
are explicit in-memory boundaries. This is not a guest, Bumble or RF test.
"""
from pathlib import Path
import argparse
import hashlib
import importlib.util
import json
import os
import subprocess
import tempfile


def production(root, source_tree, item):
    hashes = {}

    def read(relative):
        path = source_tree / relative if source_tree is not None else root / relative
        if not path.is_file():
            path = root / relative
        data = path.read_text()
        hashes[relative] = hashlib.sha256(path.read_bytes()).hexdigest()
        return data, path.resolve()

    def constant(text, name):
        anchor = 'const ' + name + ':'
        if text.count(anchor) != 1:
            raise ValueError('OPP constant missing or ambiguous: ' + name)
        begin = text.index(anchor)
        return text[begin:text.index(';', begin) + 1]

    opp, _ = read('src/user/services/core/src/bluetooth_service/opp.rs')
    classic, _ = read('src/user/services/core/src/bluetooth_service/classic.rs')
    policy, _ = read('src/user/services/logic/src/bt_policy.rs')
    dependencies = []
    for name in ('bt_bounds', 'obex', 'rfcomm'):
        _, path = read(f'src/user/services/logic/src/{name}.rs')
        dependencies.append(f'#[path = {json.dumps(str(path))}] pub mod {name};')
    policy_items = [item(policy, anchor) for anchor in (
        '#[derive(Clone, Copy, PartialEq, Eq, Debug)]\npub enum Profile {',
        'impl Profile {', 'impl Trust {',
        '#[derive(Clone, Copy, PartialEq, Eq, Debug)]\npub enum Inbound {',
        '#[derive(Clone, Copy, PartialEq, Eq, Debug)]\npub enum Decision {',
        '#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]\npub struct Peer {',
        'pub const fn inbound_by_channel(', 'pub fn decide(',
    )]
    trust = '#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]\npub struct Trust(pub u8);'
    if policy.count(trust) != 1:
        raise ValueError('OPP policy Trust declaration changed')
    policy_items.append(trust)
    policy_items.extend('pub ' + constant(policy, name) for name in ('HFP_CHANNEL', 'HSP_CHANNEL', 'OPP_CHANNEL'))
    dependencies.append('mod bt_policy {\n' + '\n'.join(policy_items) + '\n}')
    dependencies.append('mod classic {\n\tpub ' + constant(classic, 'OPP_RECORD') + '\n}')
    declarations = [constant(opp, name) for name in ('RECEIVE_TICKS', 'PIECE')]
    declarations.extend(item(opp, anchor) for anchor in (
        '#[derive(Clone, Copy, PartialEq, Eq, Debug)]\npub(crate) enum Transport {',
        'pub(crate) struct Pusher {', 'pub(crate) struct Receiver {',
        'pub(crate) enum RfEvent {', 'fn event(', 'fn peer_is_classic_peer(',
    ))
    methods = [item(opp, anchor) for anchor in (
        'pub(crate) fn receive_object(', 'pub(crate) fn receiver_started(',
        'fn receive_opened(', 'fn receive_data(', 'fn end_receiver(',
        'pub(crate) fn receiver_gone(', 'pub(crate) fn receiver_streams(',
        'pub(crate) fn opp_rfcomm_channels(', 'pub(crate) fn opp_rfcomm(',
        'pub(crate) fn opp_link_gone(', 'pub(crate) fn opp_deadline(',
        'pub(crate) fn opp_timers(',
    )]
    methods.append(item(classic, 'pub(crate) fn rfcomm_send('))
    flush_hook = ''
    if 'pub(crate) fn opp_rfcomm_flushed(' in opp:
        methods.append(item(opp, 'pub(crate) fn opp_rfcomm_flushed('))
        dispatch = item(classic, 'pub(crate) fn run_rfcomm(')
        anchor = 'self.opp_rfcomm_flushed(at, handle);'
        tail = 'for (channel, happened) in opp_events {\n\t\t\tself.opp_rfcomm(at, handle, channel, happened);\n\t\t}\n\t\t' + anchor + '\n\t}'
        if dispatch.count(anchor) != 1 or not dispatch.endswith(tail):
            raise ValueError('OPP flush hook is not after actual RFCOMM Send/event dispatch')
        flush_hook = anchor
    return dict(DEPENDENCIES='\n'.join(dependencies), DECLARATIONS='\n'.join(declarations),
                METHODS='\n'.join(methods), FLUSH_HOOK=flush_hook), hashes


def run(root, keep=None, source_tree=None, expect_failure=False):
    root = Path(root).resolve()
    source_tree = Path(source_tree).resolve() if source_tree is not None else None
    helper_path = root / 'src/tools/bluetooth_audio_lifecycle.py'
    spec = importlib.util.spec_from_file_location('opp_disconnect_extraction', helper_path)
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    actual, hashes = production(root, source_tree, helper.item)
    fixture = Path(__file__).with_name('bluetooth-opp-disconnect.rs').read_text()
    cases = [('original', actual, 'OPP_FINAL_PUT_RETAINED')] if expect_failure else [('production', actual, None)]
    if not expect_failure:
        early = dict(actual)
        early['METHODS'] = helper.replace_once(actual['METHODS'],
            'self.receivers[index].completed = true;\n\t\t\t\t\t\ttrue',
            'self.end_receiver(index, Some(event(ReceivedKind::Complete, self.receivers[index].received)));\n\t\t\t\t\t\treturn;')
        cases.append(('premature-final-put-close', early, 'OPP_FINAL_PUT_RETAINED'))
        eager = dict(actual)
        eager['METHODS'] = helper.replace_once(actual['METHODS'],
            ' && dlc.queue.is_empty()', '')
        cases.append(('close-with-disconnect-reply-queued', eager, 'OPP_DISCONNECT_REPLY_QUEUED'))
    target = Path(keep) if keep is not None else None
    if target is not None:
        target.mkdir(parents=True, exist_ok=True)
        (target / 'source-inputs.json').write_text(json.dumps(hashes, indent=2) + '\n')
    rustc = os.environ.get('RUSTC', 'rustc')
    with tempfile.TemporaryDirectory(prefix='bluetooth-opp-disconnect-') as folder:
        work = Path(folder)
        for label, pieces, marker in cases:
            code = fixture
            for name, value in pieces.items():
                code = helper.replace_once(code, '/* EXACT_' + name + ' */', value)
            rust, binary = work / (label + '.rs'), work / label
            rust.write_text(code)
            command = [rustc, '--edition=2024', '--crate-name', 'bluetooth_opp_disconnect', str(rust), '-o', str(binary)]
            compiled = subprocess.run(command, capture_output=True, text=True, timeout=120)
            log = f'compile: {command!r}\nexit: {compiled.returncode}\n{compiled.stdout}{compiled.stderr}'
            if target is not None:
                (target / (label + '.rs')).write_text(code)
                (target / (label + '.log')).write_text(log)
            if compiled.returncode:
                raise ValueError(f'OPP disconnect {label} did not compile (not an accepted negative):\n{compiled.stderr}')
            invocation = [str(binary)] + (['credits'] if label == 'close-with-disconnect-reply-queued' else [])
            tested = subprocess.run(invocation, capture_output=True, text=True, timeout=15)
            output = tested.stdout + tested.stderr
            log += f'\nrun: {invocation!r}\nexit: {tested.returncode}\n{output}'
            if target is not None:
                (target / (label + '.log')).write_text(log)
            accepted = (tested.returncode == 0 and 'OPP_DISCONNECT_COMPLETE' in output) if marker is None else (tested.returncode != 0 and marker in output)
            if not accepted:
                raise ValueError(f'OPP disconnect {label} gave the wrong verdict:\n{output}')
            print(f'bluetooth-opp-disconnect: {label}: ' + ('PASS' if marker is None else 'causal failure confirmed'), flush=True)
    print('bluetooth-opp-disconnect: exact receiver methods plus real OBEX/RFCOMM; modeled IPC/controller/L2CAP boundaries; no guest or RF claim', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--source-tree', type=Path)
    parser.add_argument('--keep', type=Path)
    parser.add_argument('--expect-failure', action='store_true')
    args = parser.parse_args()
    run(args.root, args.keep, args.source_tree, args.expect_failure)
