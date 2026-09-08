#!/usr/bin/env python3
"""Run the production provider connection set and control wait against host channels."""
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / 'src/user/drivers/core/src/common.rs'


def function(source, name):
    match = re.search(r'^(?:pub )?(?:unsafe )?fn ' + name + r'\b.*?^}', source, re.M | re.S)
    if match is None:
        raise ValueError(f'missing production function {name}')
    return match.group()


def main():
    source = SOURCE.read_text()
    definitions = '\n'.join(re.search(r'^' + pattern + r'.*?^}', source, re.M | re.S).group() for pattern in (
        'pub enum ProviderReady ', 'pub struct Serving ', 'impl Serving ', 'enum Control '))
    functions = '\n'.join(function(source, name) for name in ('wait_providers_or_answer', 'drain_control_into', 'disconnected', 'pong'))
    fixture = r'''
use driver_protocol as proto;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
const MAX_PROVIDER_CLIENTS: usize = 8;
static STOP_PENDING: AtomicBool = AtomicBool::new(false);
static QUEUED: Mutex<VecDeque<(Vec<u8>, u64)>> = Mutex::new(VecDeque::new());
static SCHEDULED: Mutex<VecDeque<(Vec<u8>, u64)>> = Mutex::new(VecDeque::new());
static CLOSED: Mutex<Vec<u64>> = Mutex::new(Vec::new());
static SENT: Mutex<Vec<(proto::Opcode, u64, Vec<u8>)>> = Mutex::new(Vec::new());
static READY: Mutex<Vec<u64>> = Mutex::new(Vec::new());
struct Bind { generation: u64 }
enum Polled { Message { len: usize, handle: u64 }, Empty, Closed }
unsafe fn close(end: u64) { CLOSED.lock().unwrap().push(end); }
unsafe fn poll_ready(end: u64) -> bool { READY.lock().unwrap().contains(&end) }
unsafe fn wait_any(set: &[u64], _: u64) -> i64 {
    assert!(set.contains(&100), "an idle provider keeps its control channel in the wait");
    let mut scheduled = SCHEDULED.lock().unwrap();
    if scheduled.is_empty() { return -1 }
    QUEUED.lock().unwrap().append(&mut scheduled);
    0
}
unsafe fn try_recv(channel: u64, out: &mut [u8]) -> Polled {
    assert_eq!(channel, 100);
    match QUEUED.lock().unwrap().pop_front() {
        Some((bytes, handle)) => { out[..bytes.len()].copy_from_slice(&bytes); Polled::Message { len: bytes.len(), handle } }
        None => Polled::Empty,
    }
}
unsafe fn send_frame(_: u64, opcode: proto::Opcode, generation: u64, payload: &[u8]) -> bool {
    SENT.lock().unwrap().push((opcode, generation, payload.to_vec())); true
}
fn connect(token: u16, end: u64, generation: u64) -> (Vec<u8>, u64) {
    let mut bytes = proto::Header { version: proto::VERSION, opcode: proto::Opcode::Connect, generation, payload_len: 2 }.encode().to_vec();
    bytes.extend_from_slice(&token.to_le_bytes()); (bytes, end)
}
fn reset() {
    QUEUED.lock().unwrap().clear(); SCHEDULED.lock().unwrap().clear();
    CLOSED.lock().unwrap().clear(); SENT.lock().unwrap().clear(); READY.lock().unwrap().clear();
    STOP_PENDING.store(false, Ordering::Relaxed);
}
#[test]
fn a_single_consumer_allowance_is_reusable_after_every_exit() {
    reset(); let bind = Bind { generation: 4 }; let mut serving = Serving::new(10, 0);
    for round in 0..12 {
        assert!(matches!(unsafe { wait_providers_or_answer(100, &bind, &mut serving, &[200]) }, Some(ProviderReady::Connected(0))), "every connection gets its own initial metadata opportunity");
        let token = serving.close_at(0);
        assert!(unsafe { disconnected(100, &bind, token) });
        assert!(serving.as_slice().is_empty());
        SCHEDULED.lock().unwrap().push_back(connect(0, 11 + round, 4));
    }
    assert_eq!(SENT.lock().unwrap().iter().filter(|(opcode, _, payload)| *opcode == proto::Opcode::Disconnect && payload == &[0, 0]).count(), 12);
    assert_eq!(CLOSED.lock().unwrap().len(), 12);
}
#[test]
fn usb_connections_keep_their_publication_identity_across_removal_and_reopen() {
    reset(); let bind = Bind { generation: 4 };
    let mut serving = Serving::from_offers(&[(0, 10), (1, 20), (2, 30)]);
    assert_eq!(serving.close_at(1), 1);
    assert_eq!(serving.at(1), 30); assert_eq!(serving.token_at(1), 2);
    assert_eq!(serving.first_for(2), 30);
    QUEUED.lock().unwrap().push_back(connect(1, 21, 4));
    assert!(matches!(unsafe { drain_control_into(100, &bind, Some(&mut serving)) }, Control::Continue));
    assert_eq!(serving.at(2), 21); assert_eq!(serving.token_at(2), 1);
    while !serving.as_slice().is_empty() { serving.close_at(0); }
    QUEUED.lock().unwrap().push_back(connect(2, 31, 4));
    assert!(matches!(unsafe { wait_providers_or_answer(100, &bind, &mut serving, &[200]) }, Some(ProviderReady::Connected(0))));
    assert_eq!(serving.first_for(2), 31);
}
#[test]
fn refused_connections_return_allowance_and_stale_handles_are_closed() {
    reset(); let bind = Bind { generation: 4 }; let mut serving = Serving::new(10, 0);
    for end in 11..18 { assert!(serving.accept(end, 0)); }
    QUEUED.lock().unwrap().push_back(connect(0, 99, 4));
    QUEUED.lock().unwrap().push_back(connect(0, 98, 3));
    assert!(matches!(unsafe { drain_control_into(100, &bind, Some(&mut serving)) }, Control::Continue));
    assert_eq!(*CLOSED.lock().unwrap(), [99, 98]);
    assert_eq!(SENT.lock().unwrap().len(), 1, "a stale generation cannot refund a current binding's allowance");
    assert_eq!(SENT.lock().unwrap()[0].0, proto::Opcode::Disconnect);
    assert!(!serving.accept(97, 9), "an unoffered token cannot change a provider's kind");
}
#[test]
fn an_idle_provider_still_services_device_work_and_stop() {
    reset(); let bind = Bind { generation: 4 }; let mut serving = Serving::new(10, 0);
    serving.close_at(0); READY.lock().unwrap().push(200);
    assert!(matches!(unsafe { wait_providers_or_answer(100, &bind, &mut serving, &[200]) }, Some(ProviderReady::Device(0))));
    let bytes = proto::Header { version: proto::VERSION, opcode: proto::Opcode::Stop, generation: 4, payload_len: 0 }.encode().to_vec();
    QUEUED.lock().unwrap().push_back((bytes, 0));
    assert!(unsafe { wait_providers_or_answer(100, &bind, &mut serving, &[200]) }.is_none());
    assert!(STOP_PENDING.load(Ordering::Relaxed));
}
'''
    with tempfile.TemporaryDirectory(prefix='liber-driver-connections-') as directory:
        path = Path(directory)
        (path / 'Cargo.toml').write_text('[package]\nname="driver-connection-regressions"\nedition="2024"\n[dependencies]\ndriver-protocol={path="' + str(ROOT / 'src/user/libs/driver/protocol') + '"}\n[lib]\npath="tests.rs"\n')
        program = fixture + definitions + functions
        command = ['cargo', 'test', '--offline', '--quiet', '--manifest-path', str(path / 'Cargo.toml'), '--lib', '--', '--test-threads=1']
        (path / 'tests.rs').write_text(program)
        result = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if result.returncode or '4 passed' not in result.stdout:
            raise SystemExit(result.stdout)
        print('driver-connections: 4 production connection and control-wait regressions passed')
        mutations = {
            'discarded CONNECT while idle': program.replace('drain_control_into(bootstrap, bind, Some(serving))', 'drain_control_into(bootstrap, bind, None)'),
            'lost DISCONNECT refund': program.replace('send_frame(bootstrap, proto::Opcode::Disconnect, bind.generation, &payload)', 'true'),
            'lost publication token during removal': program.replace('self.tokens[index] = self.tokens[self.count];', ''),
        }
        for name, mutant in mutations.items():
            if mutant == program:
                raise ValueError(f'{name}: mutation did not change code')
            (path / 'tests.rs').write_text(mutant)
            result = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            if result.returncode != 101 or 'test result: FAILED' not in result.stdout:
                raise SystemExit(f'{name}: expected assertion failure:\n{result.stdout}')
            print(f'driver-connections: rejected {name}')


if __name__ == '__main__':
    main()
