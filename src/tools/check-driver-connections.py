#!/usr/bin/env python3
"""Run the production provider connection set and control wait against host channels."""
from pathlib import Path
import re
import subprocess
import sys
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
    functions = '\n'.join(function(source, name) for name in ('wait_providers_or_answer', 'wait_providers', 'wait_providers_inner', 'wait_or_answer_until', 'drain_control_into', 'disconnected', 'pong'))
    # THE STUBS ARE SAFE, LIKE THE `rt` CALLS THEY STAND IN FOR. They were `unsafe fn`, from a time
    # when `close`, `poll_ready`, `wait_any` and `try_recv` were - so the extracted production code,
    # which calls them from safe functions, stopped compiling the moment the runtime's did not. A
    # gate that will not build is a gate that checks nothing, and it says so as a compile error about
    # somebody else's function rather than as a failed claim.
    fixture = r'''
use driver_protocol as proto;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
const MAX_PROVIDER_CLIENTS: usize = 8;
const ERR_TIMED_OUT: i64 = -11;
static STOP_PENDING: AtomicBool = AtomicBool::new(false);
// THE NODE AND THE SLEEP, as the production wait reads them: no node channel, and a driver that does not take the
// sleep itself - so a `SUSPEND` would go to the common step, which these regressions never send.
static NODE: AtomicU64 = AtomicU64::new(0);
static OWN_SLEEP: AtomicBool = AtomicBool::new(false);
static SUSPEND_ASKED: AtomicU64 = AtomicU64::new(0);
// THE FIXED BUTTON'S PRESSES, as the production wait counts a manager's `PRESSED` - which these regressions never send.
static PRESSES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
fn request_node(_: u64, _: &Bind) -> bool { true }
fn take_node(_: &Bind, _: proto::Opcode, handle: u64) { if handle != 0 { close(handle) } }
fn encode_request(_: &proto::SuspendRequest) -> u64 { 0 }
fn common_sleep_step(_: u64, _: &Bind, _: &proto::SuspendRequest, _: Option<&mut Serving>) -> Control { Control::Continue }
fn resumed(_: u64, _: &Bind, _: bool) -> bool { true }
fn clock() -> u64 { 0 }
static QUEUED: Mutex<VecDeque<(Vec<u8>, u64)>> = Mutex::new(VecDeque::new());
static SCHEDULED: Mutex<VecDeque<(Vec<u8>, u64)>> = Mutex::new(VecDeque::new());
static CLOSED: Mutex<Vec<u64>> = Mutex::new(Vec::new());
static SENT: Mutex<Vec<(proto::Opcode, u64, Vec<u8>)>> = Mutex::new(Vec::new());
static READY: Mutex<Vec<u64>> = Mutex::new(Vec::new());
struct Bind { generation: u64 }
enum Polled { Message { len: usize, handle: u64 }, Empty, Closed }
fn close(end: u64) { CLOSED.lock().unwrap().push(end); }
fn poll_ready(end: u64) -> bool { READY.lock().unwrap().contains(&end) }
fn wait_any(set: &[u64], _: u64) -> i64 {
    assert!(set.contains(&100), "an idle provider keeps its control channel in the wait");
    let mut scheduled = SCHEDULED.lock().unwrap();
    if scheduled.is_empty() { return -1 }
    QUEUED.lock().unwrap().append(&mut scheduled);
    0
}
fn wait_any_periodic(set: &[u64], deadline: u64) -> i64 { wait_any(set, deadline) }
fn try_recv(channel: u64, out: &mut [u8]) -> Polled {
    assert_eq!(channel, 100);
    match QUEUED.lock().unwrap().pop_front() {
        Some((bytes, handle)) => { out[..bytes.len()].copy_from_slice(&bytes); Polled::Message { len: bytes.len(), handle } }
        None => Polled::Empty,
    }
}
fn send_frame(_: u64, opcode: proto::Opcode, generation: u64, payload: &[u8]) -> bool {
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
        assert!(matches!(wait_providers_or_answer(100, &bind, &mut serving, &[200]), Some(ProviderReady::Connected(0))), "every connection gets its own initial metadata opportunity");
        let token = serving.close_at(0);
        assert!(disconnected(100, &bind, token));
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
    assert!(matches!(drain_control_into(100, &bind, Some(&mut serving), false), Control::Continue));
    assert_eq!(serving.at(2), 21); assert_eq!(serving.token_at(2), 1);
    while !serving.as_slice().is_empty() { serving.close_at(0); }
    QUEUED.lock().unwrap().push_back(connect(2, 31, 4));
    assert!(matches!(wait_providers_or_answer(100, &bind, &mut serving, &[200]), Some(ProviderReady::Connected(0))));
    assert_eq!(serving.first_for(2), 31);
}
#[test]
fn refused_connections_return_allowance_and_stale_handles_are_closed() {
    reset(); let bind = Bind { generation: 4 }; let mut serving = Serving::new(10, 0);
    for end in 11..18 { assert!(serving.accept(end, 0, proto::Scope::Whole)); }
    QUEUED.lock().unwrap().push_back(connect(0, 99, 4));
    QUEUED.lock().unwrap().push_back(connect(0, 98, 3));
    assert!(matches!(drain_control_into(100, &bind, Some(&mut serving), false), Control::Continue));
    assert_eq!(*CLOSED.lock().unwrap(), [99, 98]);
    assert_eq!(SENT.lock().unwrap().len(), 1, "a stale generation cannot refund a current binding's allowance");
    assert_eq!(SENT.lock().unwrap()[0].0, proto::Opcode::Disconnect);
    assert!(!serving.accept(97, 9, proto::Scope::Whole), "an unoffered token cannot change a provider's kind");
}
#[test]
fn a_scoped_connection_is_served_under_the_scope_its_connect_named() {
    // A BUS PROVIDER'S SHAPE: a publication with no endpoint of its own, and connections scoped by their CONNECT.
    reset(); let bind = Bind { generation: 4 }; let mut serving = Serving::from_offers(&[(0, 0)]);
    let mut payload = [0u8; proto::CONNECT_PAYLOAD_MAX];
    let len = proto::encode_connect(0, proto::Scope::I2cAddress(0x50), &mut payload);
    let mut bytes = proto::Header { version: proto::VERSION, opcode: proto::Opcode::Connect, generation: 4, payload_len: len as u32 }.encode().to_vec();
    bytes.extend_from_slice(&payload[..len]);
    QUEUED.lock().unwrap().push_back((bytes, 41));
    assert!(matches!(wait_providers_or_answer(100, &bind, &mut serving, &[200]), Some(ProviderReady::Connected(0))));
    assert_eq!((serving.at(0), serving.scope_at(0)), (41, proto::Scope::I2cAddress(0x50)), "the endpoint is served for the one address its CONNECT named");
    QUEUED.lock().unwrap().push_back(connect(0, 42, 4));
    assert!(matches!(wait_providers_or_answer(100, &bind, &mut serving, &[200]), Some(ProviderReady::Connected(1))));
    assert_eq!(serving.scope_at(1), proto::Scope::Whole, "an unscoped CONNECT is the two-byte payload it always was");
    serving.close_at(0);
    assert_eq!((serving.at(0), serving.scope_at(0)), (42, proto::Scope::Whole), "removal keeps each remaining endpoint's own scope");
}
#[test]
fn an_idle_provider_still_services_device_work_and_stop() {
    reset(); let bind = Bind { generation: 4 }; let mut serving = Serving::new(10, 0);
    serving.close_at(0); READY.lock().unwrap().push(200);
    assert!(matches!(wait_providers_or_answer(100, &bind, &mut serving, &[200]), Some(ProviderReady::Device(0))));
    let bytes = proto::Header { version: proto::VERSION, opcode: proto::Opcode::Stop, generation: 4, payload_len: 0 }.encode().to_vec();
    QUEUED.lock().unwrap().push_back((bytes, 0));
    assert!(wait_providers_or_answer(100, &bind, &mut serving, &[200]).is_none());
    assert!(STOP_PENDING.load(Ordering::Relaxed));
}
#[test]
fn a_consumer_accepted_during_a_wait_on_the_callers_set_is_handed_back_so_the_set_is_built_again() {
    // THE RELAUNCHED CONSUMER: the first consumer's endpoint is what the caller waits on, and a second connects meanwhile.
    // Kept in the wait it was accepted in, it would never be read - so it comes back as "nothing ready".
    reset(); let bind = Bind { generation: 4 }; let mut serving = Serving::new(10, 0);
    let handles = serving.as_slice().to_vec();
    QUEUED.lock().unwrap().push_back(connect(0, 11, 4));
    assert!(matches!(wait_or_answer_until(100, &bind, &handles, u64::MAX, Some(&mut serving)), Some(None)), "the accepted consumer is handed back for the caller to wait on");
    assert_eq!(serving.as_slice(), &[10, 11]);
}
'''
    with tempfile.TemporaryDirectory(prefix='liber-driver-connections-') as directory:
        path = Path(directory)
        (path / 'Cargo.toml').write_text('[package]\nname="driver-connection-regressions"\nedition="2024"\n[dependencies]\ndriver-protocol={path="' + str(ROOT / 'src/user/libs/driver/protocol') + '"}\n[lib]\npath="tests.rs"\n')
        program = fixture + definitions + functions
        command = ['cargo', 'test', '--offline', '--quiet', '--manifest-path', str(path / 'Cargo.toml'), '--lib', '--', '--test-threads=1']
        (path / 'tests.rs').write_text(program)
        result = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if result.returncode or '6 passed' not in result.stdout:
            raise SystemExit(result.stdout)
        print('driver-connections: 6 production connection and control-wait regressions passed')
        mutations = {
            'discarded CONNECT while idle': program.replace('drain_control_into(bootstrap, bind, Some(serving), surfaces)', 'drain_control_into(bootstrap, bind, None, surfaces)'),
            'an accepted consumer kept out of the wait': program.replace('if serving.as_deref().is_some_and(|serving| serving.as_slice().len() > serving_before) {', 'if false {'),
            'lost DISCONNECT refund': program.replace('send_frame(bootstrap, proto::Opcode::Disconnect, bind.generation, &payload)', 'true'),
            'lost publication token during removal': program.replace('self.tokens[index] = self.tokens[self.count];', ''),
            'lost connection scope': program.replace('self.scopes[self.count] = scope;', 'self.scopes[self.count] = proto::Scope::Whole;'),
        }
        for name, mutant in mutations.items():
            if mutant == program:
                raise ValueError(f'{name}: mutation did not change code')
            (path / 'tests.rs').write_text(mutant)
            result = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            if result.returncode != 101 or 'test result: FAILED' not in result.stdout:
                raise SystemExit(f'{name}: expected assertion failure:\n{result.stdout}')
            print(f'driver-connections: rejected {name}')
    subprocess.run([sys.executable, str(ROOT / 'src/tools/check-provider-catalogue.py')], check=True)
    # AUDIOSERVICE'S PROVIDER RECOVERY is no longer a fragment extracted from one slot: every publication is a device
    # of its own, withdrawn by its identity and failed by its channel, and the kernel suite drives it with two
    # providers (`kernel.services.audio_service_routes_streams_by_the_device_inventory`) and with its only driver
    # gone (`kernel.services.audio_service_keeps_streams_through_driver_loss`).


if __name__ == '__main__':
    main()
