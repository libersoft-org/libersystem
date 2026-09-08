#!/usr/bin/env python3
"""Exercise AudioService's production subscription and driver waits on host channels."""
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def method(source: str, name: str) -> str:
    match = re.search(r"^\tfn " + name + r"\b.*?^\t}", source, re.M | re.S)
    if match is None:
        raise ValueError(f"missing production Audio::{name}")
    return match.group()


FIXTURE = r'''
#![allow(dead_code, unused_variables, unused_unsafe)]
extern crate alloc;
use std::{cell::RefCell, collections::{HashMap, HashSet, VecDeque}};
use device_proto::generated::liber::device::v1::{provider_catalogue, ProviderInfo, ProviderKind};
const PERIOD_BYTES: usize = 2048;
#[derive(Default)]
struct Runtime {
    messages: HashMap<u64, VecDeque<Vec<u8>>>, closed: HashSet<u64>,
    released: Vec<u64>, opened: Vec<u64>, sent: Vec<u64>,
}
thread_local! { static RT: RefCell<Runtime> = RefCell::new(Runtime::default()); }
fn print(_: &[u8]) {}
fn close(handle: u64) { RT.with_borrow_mut(|rt| rt.released.push(handle)); }
fn send_blocking(handle: u64, _: &[u8], _: u64) -> bool { RT.with_borrow_mut(|rt| {
    if rt.closed.contains(&handle) { return false; }
    rt.sent.push(handle); true
}) }
enum Received { Message { len: usize, handle: u64 }, Closed }
fn recv_blocking(handle: u64, out: &mut [u8]) -> Received { RT.with_borrow_mut(|rt| {
    if let Some(bytes) = rt.messages.entry(handle).or_default().pop_front() {
        out[..bytes.len()].copy_from_slice(&bytes);
        Received::Message { len: bytes.len(), handle: 0 }
    } else {
        assert!(rt.closed.contains(&handle), "receive must follow readiness");
        Received::Closed
    }
}) }
fn wait_any(handles: &[u64], _: u64) -> i64 { RT.with_borrow(|rt| {
    handles.iter().position(|handle| rt.closed.contains(handle) || rt.messages.get(handle).is_some_and(|q| !q.is_empty())).map_or(-1, |at| at as i64)
}) }
fn open_provider(_: u64, info: &ProviderInfo) -> u64 { RT.with_borrow_mut(|rt| rt.opened.push(info.binding_generation)); 99 }
#[derive(PartialEq)]
enum DriverPending { None, Period, Stop, Capture(usize) }
struct Pending { caps: wire::Handles }
struct Stream { chan: u64, pending: Option<Pending> }
struct Capture { chan: u64, pending: Option<()>, unavailable: bool, ready: Option<()> }
struct Audio {
    snd: u64, driver_pending: DriverPending, driver_running: bool, capture_running: bool,
    streams: Vec<Stream>, captures: Vec<Capture>, tones: Vec<()>, period: Vec<u8>,
}
impl Audio {
    fn has_audio(&self) -> bool { !self.tones.is_empty() }
    fn fill_period(&mut self) {}
    fn capture_ready(&mut self, _: usize, _: &[u8]) { panic!("capture conversion is outside this fixture"); }
    @@METHODS@@
}
struct Client { chan: u64 }
fn step(state: &mut Audio, providers: &mut u64) {
    let catalogue = 70; let admin = 90; let clients = vec![Client { chan: 100 }];
    let mut request = [0; 128]; let mut providers = *providers;
    // A production continue completes this one turn; an empty wait must never receive.
    for _ in 0..1 {
        @@WAIT_AND_EVENTS@@
    }
}
fn audio() -> Audio {
    RT.with_borrow_mut(|rt| *rt = Runtime::default());
    Audio { snd: 10, driver_pending: DriverPending::None, driver_running: false, capture_running: false,
        streams: vec![], captures: vec![], tones: vec![], period: vec![0; PERIOD_BYTES] }
}
fn announce(live: bool, generation: u64) {
    let info = ProviderInfo { kind: ProviderKind::Audio, bus: 0, dev: 1, func: 0,
        binding_generation: generation, slot: 0, provider_generation: generation as u32, live };
    let mut frame = [0; 128]; let mut handles = wire::Handles::new();
    let len = provider_catalogue::subscribe_frame(0, &info, &mut frame, &mut handles).unwrap();
    RT.with_borrow_mut(|rt| rt.messages.entry(80).or_default().push_back(frame[..len].to_vec()));
}
#[test]
fn an_idle_driver_rebind_is_adopted_before_the_next_playback() {
    for withdrawal_before_exit in [false, true] {
        let mut state = audio(); let mut providers = 80;
        announce(false, 1);
        if withdrawal_before_exit { step(&mut state, &mut providers); }
        RT.with_borrow_mut(|rt| { rt.closed.insert(10); });
        announce(true, 2);
        for _ in 0..3 { step(&mut state, &mut providers); }
        assert_eq!(state.snd, 99, "the closed idle driver cannot hide its replacement publication");
        RT.with_borrow(|rt| { assert_eq!(rt.opened, [2]); assert_eq!(rt.released, [10]); });
        state.tones.push(()); state.pump();
        RT.with_borrow(|rt| assert_eq!(rt.sent, [99], "playback must use the adopted connection"));
    }
}
#[test]
fn another_providers_withdrawal_keeps_the_healthy_connection() {
    let mut state = audio(); let mut providers = 80;
    announce(false, 2); step(&mut state, &mut providers);
    assert_eq!(state.snd, 10);
    state.tones.push(()); state.pump();
    RT.with_borrow(|rt| { assert!(rt.released.is_empty()); assert!(rt.opened.is_empty()); assert_eq!(rt.sent, [10]); });
}
#[test]
fn a_pending_period_reply_still_completes_on_the_same_connection() {
    let mut state = audio(); let mut providers = 80;
    state.driver_pending = DriverPending::Period;
    RT.with_borrow_mut(|rt| rt.messages.entry(10).or_default().push_back(vec![]));
    step(&mut state, &mut providers);
    assert!(state.driver_pending == DriverPending::None);
    assert_eq!(state.snd, 10);
    step(&mut state, &mut providers);
    RT.with_borrow(|rt| assert!(rt.released.is_empty()));
}
'''


def main() -> None:
    source = (ROOT / "src/user/services/core/src/audio_engine.rs").read_text()
    serving = source[source.index("unsafe fn serve(root:"):]
    events = serving[serving.index("let driver_first:"):serving.index("if ready_chan == admin {")]
    methods = "\n".join(method(source, name) for name in ("pump", "driver_ready", "driver_failed"))
    program = FIXTURE.replace("@@METHODS@@", methods).replace("@@WAIT_AND_EVENTS@@", events)
    mutant = program.replace("let driver_first: bool = state.snd != 0;", "let driver_first: bool = state.snd != 0 && state.driver_pending != DriverPending::None;")
    if mutant == program:
        raise ValueError("idle-close mutation did not change production wait")
    with tempfile.TemporaryDirectory(prefix="liber-audio-provider-") as directory:
        path = Path(directory)
        dependencies = {"device-proto": "src/user/libs/protocol/device-proto", "wire": "src/wire"}
        manifest = '[package]\nname="audio-provider-regressions"\nedition="2024"\n[lib]\npath="tests.rs"\n[dependencies]\n'
        manifest += "".join(f'{name}={{path="{ROOT / relative}"}}\n' for name, relative in dependencies.items())
        (path / "Cargo.toml").write_text(manifest)
        command = ["cargo", "test", "--offline", "--quiet", "--manifest-path", str(path / "Cargo.toml"), "--lib"]
        (path / "tests.rs").write_text(program)
        result = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if result.returncode or "3 passed" not in result.stdout:
            raise SystemExit(result.stdout)
        print("audio-provider-recovery: 3 production subscription and driver-wait regressions passed")
        (path / "tests.rs").write_text(mutant)
        result = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if result.returncode != 101 or "test result: FAILED" not in result.stdout:
            raise SystemExit(f"idle-close mutation: expected assertion failure:\n{result.stdout}")
        print("audio-provider-recovery: rejected idle driver omitted from wait")


if __name__ == "__main__":
    main()
