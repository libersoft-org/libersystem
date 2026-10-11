"""Actual voice_up/HCI command regression; controller negotiation stays in the live HSP gate.

Only the controller command sink and the already-negotiated codec/voice lookup are
modeled. Production voice_up and setup_synchronous construct the command bytes.
This standard wire-contract check neither mirrors RootCanal's link selector nor
claims an independent controller, SCO PCM, USB, or RF run.
"""
from pathlib import Path
import argparse
import hashlib
import importlib.util
import json
import os
import subprocess
import tempfile

VOICE = 'src/user/services/core/src/bluetooth_service/voice.rs'
HCI = 'src/user/services/logic/src/hci_bredr.rs'

FIXTURE = r'''
#![allow(dead_code)]
extern crate alloc;
/* EXACT_HCI */
/* EXACT_CONSTANTS */
struct Gateway { allowed: bool }
impl Gateway { fn set_audio_allowed(&mut self, allowed: bool) { self.allowed = allowed; } }
struct Voice { gateway: Gateway, wideband: bool, sco: Option<u16>, setting_up: bool }
impl Voice { fn msbc(&self) -> bool { self.wideband } }
#[derive(Default)]
struct Controller { commands: Vec<(u16, Vec<u8>)> }
impl Controller { fn command(&mut self, opcode: u16, bytes: &[u8]) { self.commands.push((opcode, bytes.to_vec())); } }
struct Stack { voice: Option<Voice>, controllers: Vec<Controller> }
impl Stack {
    fn voice(&mut self, at: usize, handle: u16) -> Option<&mut Voice> {
        assert_eq!((at, handle), (0, 0x123)); self.voice.as_mut()
    }
    /* EXACT_VOICE_UP */
}
fn setup(wideband: bool) -> Vec<u8> {
    let voice = Voice { gateway: Gateway { allowed: false }, wideband, sco: None, setting_up: false };
    let mut stack = Stack { voice: Some(voice), controllers: vec![Controller::default()] };
    stack.voice_up(0, 0x123);
    assert!(stack.voice.as_ref().unwrap().gateway.allowed);
    assert!(stack.voice.as_ref().unwrap().setting_up);
    assert_eq!(stack.controllers[0].commands.len(), 1);
    stack.voice_up(0, 0x123);
    assert_eq!(stack.controllers[0].commands.len(), 1, "pending setup must not be duplicated");
    stack.voice.as_mut().unwrap().setting_up = false;
    stack.voice.as_mut().unwrap().sco = Some(0x456);
    stack.voice_up(0, 0x123);
    assert_eq!(stack.controllers[0].commands.len(), 1, "live synchronous link must not be set up again");
    stack.voice = None;
    stack.voice_up(0, 0x123);
    assert_eq!(stack.controllers[0].commands.len(), 1);
    let (opcode, packet) = stack.controllers[0].commands.pop().unwrap();
    assert_eq!(opcode, 0x0428);
    assert_eq!(packet.len(), 17);
    assert_eq!(&packet[..10], &[0x23, 0x01, 0x40, 0x1f, 0, 0, 0x40, 0x1f, 0, 0]);
    packet
}
fn main() {
    let cvsd = setup(false);
    let msbc = setup(true);
    let packets = u16::from_le_bytes([cvsd[15], cvsd[16]]);
    // Core HCI packet types: positive HV1/HV2/HV3/EV3 bits; inverted EDR exclusion bits.
    assert_eq!(packets & 7, 7, "VOICE_SCO_CVSD_ALLOWED_HV");
    assert_eq!(packets, 0x03cf, "VOICE_SCO_CVSD_ESCO_RETAINED");
    assert_eq!(cvsd[14], 0xff, "VOICE_SCO_CVSD_RETRANSMISSION");
    assert_eq!(&cvsd[10..14], &[12, 0, 0x60, 0]);
    assert_eq!(&msbc[10..], &[13, 0, 0x63, 0, 2, 0xc8, 3], "VOICE_SCO_MSBC_UNCHANGED");
    println!("VOICE_SCO_COMMANDS_COMPLETE");
}
'''


def run(root, keep=None, source_tree=None, expect_failure=False):
    root = Path(root).resolve()
    tree = Path(source_tree).resolve() if source_tree is not None else root
    helper_spec = importlib.util.spec_from_file_location('voice_sco_extraction', root / 'src/tools/bluetooth_audio_lifecycle.py')
    helper = importlib.util.module_from_spec(helper_spec)
    helper_spec.loader.exec_module(helper)
    item, replace_once = helper.item, helper.replace_once
    voice_path = tree / VOICE
    hci_path = tree / HCI
    if not hci_path.is_file():
        hci_path = root / HCI
    voice = voice_path.read_text()
    constants = []
    for name in ('VOICE_CVSD', 'VOICE_TRANSPARENT', 'PACKET_TYPES'):
        prefix = f'const {name}: u16 = '
        if voice.count(prefix) != 1:
            raise ValueError(f'voice SCO constant is absent or ambiguous: {name}')
        begin = voice.index(prefix)
        constants.append(voice[begin:voice.index(';', begin) + 1])
    method = item(voice, 'pub(crate) fn voice_up(')
    actual = replace_once(FIXTURE, '/* EXACT_VOICE_UP */', method)
    actual = replace_once(actual, '/* EXACT_CONSTANTS */', '\n'.join(constants))
    actual = replace_once(actual, '/* EXACT_HCI */', f'#[path = {json.dumps(str(hci_path))}] mod hci_bredr;')
    if expect_failure:
        cases = [('original', actual, 'VOICE_SCO_CVSD_ALLOWED_HV')]
    else:
        cases = [
            ('production', actual, None),
            ('without-hv', replace_once(actual, 'PACKET_TYPES | 0x0007', 'PACKET_TYPES'), 'VOICE_SCO_CVSD_ALLOWED_HV'),
            ('without-sco-effort', replace_once(actual, 'VOICE_CVSD, 0xff,', 'VOICE_CVSD, 2,'), 'VOICE_SCO_CVSD_RETRANSMISSION'),
            ('wideband-allows-sco', replace_once(actual, 'VOICE_TRANSPARENT, 2, PACKET_TYPES)',
                                               'VOICE_TRANSPARENT, 2, PACKET_TYPES | 0x0007)'), 'VOICE_SCO_MSBC_UNCHANGED'),
        ]
    target = Path(keep) if keep is not None else None
    if target is not None:
        target.mkdir(parents=True, exist_ok=True)
        inputs = {str(path): hashlib.sha256(path.read_bytes()).hexdigest()
                  for path in (voice_path, hci_path, Path(__file__), root / 'src/tools/bluetooth_audio_lifecycle.py')}
        (target / 'source-inputs.json').write_text(json.dumps(inputs, indent=2) + '\n')
    with tempfile.TemporaryDirectory(prefix='bluetooth-voice-sco-') as folder:
        for label, code, marker in cases:
            rust, binary = Path(folder) / f'{label}.rs', Path(folder) / label
            rust.write_text(code)
            command = [os.environ.get('RUSTC', 'rustc'), '--edition=2024', '--crate-name', 'bluetooth_voice_sco', str(rust), '-o', str(binary)]
            built = subprocess.run(command, capture_output=True, text=True, timeout=120)
            log = f'compile: {command!r}\nexit: {built.returncode}\n{built.stdout}{built.stderr}'
            if target is not None:
                (target / f'{label}.rs').write_text(code)
                (target / f'{label}.log').write_text(log)
            if built.returncode:
                raise ValueError(f'voice SCO {label} did not compile (not an accepted negative):\n{built.stderr}')
            tested = subprocess.run([str(binary)], capture_output=True, text=True, timeout=15)
            output = tested.stdout + tested.stderr
            log += f'\nrun: {[str(binary)]!r}\nexit: {tested.returncode}\n{output}'
            if target is not None:
                (target / f'{label}.log').write_text(log)
            accepted = (tested.returncode == 0 and output.count('VOICE_SCO_COMMANDS_COMPLETE') == 1) if marker is None else (tested.returncode != 0 and marker in output)
            if not accepted:
                raise ValueError(f'voice SCO {label} wrong verdict (required {marker or "success"}):\n{output}')
            print(f'bluetooth-voice-sco: {label}: ' + ('PASS' if marker is None else f'causal negative accepted: {marker}'), flush=True)
    print('bluetooth-voice-sco: actual voice_up/HCI command bytes checked; codec lookup/controller sink modeled; live SCO-only HSP negotiation remains the independent guest gate', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument('--source-tree', type=Path)
    parser.add_argument('--keep', type=Path)
    parser.add_argument('--expect-failure', action='store_true')
    arguments = parser.parse_args()
    run(arguments.root, arguments.keep, arguments.source_tree, arguments.expect_failure)
