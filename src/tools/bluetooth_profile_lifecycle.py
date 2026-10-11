"""Exact production classic-profile lifecycle proof with explicit host boundaries.

The real L2CAP channel table and extracted service decisions run on the host.
Controller/IPC/decoder/codec peers are traceable test boundaries, not a live radio.
"""
import argparse
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile

from bluetooth_audio_lifecycle import item, replace_once


QUEUE_FLAG = """\t\t\t\t\tif profile == Profile::Input
\t\t\t\t\t\t&& let Some(classic) = self.controllers[at].link_mut(handle).and_then(|link| link.classic.as_mut())
\t\t\t\t\t{
\t\t\t\t\t\tclassic.hid.outgoing = true;
\t\t\t\t\t}"""
SDP_CANCEL = 'classic.sdp_client.as_ref().is_some_and(|client| client.purpose == Purpose::Profile(Profile::Input))'


def production(root, source_tree):
    paths = []

    def read(name):
        relative = Path('src/user/services/core') / name
        candidate = source_tree / relative
        path = candidate if candidate.is_file() else root / relative
        paths.append(path)
        return path.read_text()

    classic = read('src/bluetooth_service/classic.rs')
    audio = read('src/bluetooth_service/a2dp.rs')
    main = read('src/bluetooth_service.rs')
    endpoints = read('src/bluetooth_service/audio.rs')
    endpoint_limits = [line for line in endpoints.splitlines() if line.startswith('const MAX_ENDPOINTS:')]
    if len(endpoint_limits) != 1:
        raise ValueError('ambiguous production audio endpoint bound')
    declarations = endpoint_limits + [item(classic, anchor) for anchor in (
        '#[derive(Clone, Copy, PartialEq, Eq)]\npub(crate) enum Want {',
        '#[derive(Clone, Copy, PartialEq, Eq)]\npub(crate) enum Purpose {',
        '#[derive(Default)]\npub(crate) struct Hid {',
        'pub(crate) fn connected_profiles(',
    )]
    declarations += [item(audio, anchor) for anchor in (
        '#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]\npub(crate) enum State {',
        '#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]\npub(crate) enum Volume {',
        '#[derive(Default)]\npub(crate) struct A2dp {',
    )]
    declarations += [item(main, anchor) for anchor in ('fn release_streams(', 'fn deliver(')]
    methods = [item(classic, anchor) for anchor in (
        'pub(crate) fn close_channel(', 'fn channel_opened(', 'fn channel_closed(',
        'fn sdp_answer(', 'fn found_service(', 'pub(crate) fn connect_profile(',
        'pub(crate) fn disconnect_profile(', 'fn hid_ready(', 'fn start_profile(',
        'pub(crate) fn start_search(',
    )]
    # The baseline predates these helpers. Its public operator method must still
    # compile and fail causally, rather than a missing-helper compilation error.
    for source, anchor in ((classic, 'fn hid_channel_closed('), (audio, 'pub(crate) fn disconnect_a2dp(')):
        if anchor in source:
            methods.append(item(source, anchor))
    methods += [item(audio, anchor) for anchor in (
        'fn a2dp(&mut self,', 'fn a2dp_offer(', 'pub(crate) fn a2dp_channel_closed(',
        'pub(crate) fn a2dp_connect(',
    )]
    queued = item(classic, 'for want in wanted {')
    methods.append('fn resume_wanted(&mut self, at: usize, handle: u16, wanted: Vec<Want>) {\n' + queued + '\n}')
    declarations.append('impl Stack {\n' + '\n'.join(methods) + '\n}')
    declarations.append('impl AudioRoot {\n' + '\n'.join(item(endpoints, anchor) for anchor in (
        'pub fn offer(', 'pub fn withdraw(',
    )) + '\n}')
    modules = []
    for name in ('bt_bounds', 'l2cap_bredr'):
        path = root / 'src/user/services/logic/src' / (name + '.rs')
        paths.append(path)
        modules.append(f'#[path = "{path}"] mod {name};')
    return '\n\n'.join(declarations), '\n'.join(modules), {
        str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in paths
    }


def run(root, keep=None, *, source_tree=None, expect_failure=False):
    root = Path(root).resolve()
    actual, modules, pins = production(root, Path(source_tree).resolve() if source_tree else root)
    fixture = Path(__file__).with_name('bluetooth-profile-lifecycle.rs').read_text()
    if any(fixture.count(marker) != 1 for marker in ('/* EXACT_PRODUCTION */', '/* EXACT_MODULES */')):
        raise ValueError('classic profile fixture insertion is missing or ambiguous')
    cases = [('production', actual, not expect_failure,
              'operator Audio disconnect must succeed' if expect_failure else None,
              'operator-audio' if expect_failure else 'all')]
    if not expect_failure:
        cases += [
            ('audio-still-rfcomm-only', replace_once(actual,
                'if profile == Profile::Audio {', 'if false && profile == Profile::Audio {'),
             False, 'operator Audio disconnect must succeed', 'operator-audio'),
            ('keep-avctp', replace_once(actual,
                'avctp.filter(|held| *held != cid)', 'avctp.filter(|_| false)'),
             False, 'Audio disconnect must close signalling, media and AVRCP', 'operator-audio'),
            ('missing-audio-arrival-bit', replace_once(actual,
                'classic.profiles.with(bt_policy::Profile::Audio, true)',
                'classic.profiles.with(bt_policy::Profile::Audio, false)'),
             False, 'offered Audio endpoint must appear in connected inventory', 'audio-inventory'),
            ('stale-audio-departure-bit', replace_once(actual,
                'classic.profiles.with(bt_policy::Profile::Audio, false)',
                'classic.profiles.with(bt_policy::Profile::Audio, true)'),
             False, 'withdrawn Audio endpoint must leave connected inventory', 'audio-inventory'),
            ('skip-held-stream-release', replace_once(actual, '\t\trelease_streams(link);', ''),
             False, 'held input reports must precede closing each stream', 'operator-input'),
            ('keep-decoder', replace_once(actual, '\t\tlink.decoder = None;', ''),
             False, 'retired HID decoder must not be visible to a new input consumer', 'operator-input'),
            ('keep-hid-flags', replace_once(actual, '\t\tclassic.hid = Hid::default();',
                '\t\tclassic.hid.control = None; classic.hid.interrupt = None;'),
             False, 'fresh inbound HID must request its descriptor on the same ACL', 'inbound-reconnect'),
            ('keep-late-input-sdp', replace_once(actual, SDP_CANCEL,
                'classic.sdp_client.as_ref().is_some_and(|client| false && client.purpose == Purpose::Profile(Profile::Input))'),
             False, 'late SDP must not resurrect retired input', 'late-sdp'),
            ('keep-late-audio-sdp', replace_once(actual,
                'classic.sdp_client.as_ref().is_some_and(|client| client.purpose == super::classic::Purpose::Profile(Profile::Audio))',
                'classic.sdp_client.as_ref().is_some_and(|client| false && client.purpose == super::classic::Purpose::Profile(Profile::Audio))'),
             False, 'late SDP must not resurrect retired Audio', 'audio-late-sdp'),
            ('drop-cold-input-intent', replace_once(actual, QUEUE_FLAG, ''),
             False, 'queued cold Input connect must open HID control after SDP', 'cold-input'),
        ]
    rustc = os.environ.get('RUSTC', 'rustc')
    with tempfile.TemporaryDirectory(prefix='bluetooth-profile-lifecycle-') as directory:
        work = Path(directory)
        for label, body, should_pass, expected_error, selected in cases:
            code = fixture.replace('/* EXACT_PRODUCTION */', body).replace('/* EXACT_MODULES */', modules)
            rust, binary = work / (label + '.rs'), work / label
            rust.write_text(code)
            command = [rustc, '--edition=2024', '--crate-name', 'bluetooth_profile_lifecycle', str(rust), '-o', str(binary)]
            built = subprocess.run(command, capture_output=True, text=True, timeout=120)
            if keep is not None:
                target = Path(keep)
                target.mkdir(parents=True, exist_ok=True)
                (target / (label + '.rs')).write_text(code)
                (target / (label + '-compile.log')).write_text(f'{command!r}\nexit: {built.returncode}\n{built.stdout}{built.stderr}')
            if built.returncode:
                raise ValueError(f'classic profile lifecycle {label} did not compile:\n{built.stderr}')
            tested = subprocess.run([str(binary), selected], capture_output=True, text=True, timeout=15)
            if keep is not None:
                (target / (label + '.log')).write_text(f'run {selected!r} exit: {tested.returncode}\n{tested.stdout}{tested.stderr}')
            if (tested.returncode == 0) != should_pass or (expected_error and expected_error not in tested.stderr):
                raise ValueError(f'classic profile lifecycle {label} gave the wrong verdict:\n{tested.stdout}{tested.stderr}')
            if expect_failure and 'Err(NotFound)' not in tested.stderr:
                raise ValueError(f'original Audio disconnect did not fail for NotFound:\n{tested.stderr}')
            print(f'bluetooth-profile-lifecycle: {label}: ' + ('PASS' if should_pass else 'expected causal failure proved'), flush=True)
    print(f'bluetooth-profile-lifecycle: exact source SHA256 identities {pins}; real L2CAP table, mocked IPC/decoder/codec/controller boundaries; not live Bluetooth', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--source-tree', type=Path, help='reviewed staged service overlay for pre-apply proof')
    parser.add_argument('--keep', type=Path)
    parser.add_argument('--expect-failure', action='store_true', help='prove the original public Audio disconnect failure')
    args = parser.parse_args()
    run(args.root, args.keep, source_tree=args.source_tree, expect_failure=args.expect_failure)
