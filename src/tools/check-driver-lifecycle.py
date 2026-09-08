#!/usr/bin/env python3
"""Execute production READY admission and operator dependency-stop ordering on the host."""
from pathlib import Path
import subprocess
import tempfile

from importlib.util import module_from_spec, spec_from_file_location

ROOT = Path(__file__).resolve().parents[2]
spec = spec_from_file_location("manager_progress", Path(__file__).with_name("check-device-manager-progress.py"))
progress = module_from_spec(spec)
spec.loader.exec_module(progress)
item = progress.item

FIXTURE = r'''
#![allow(dead_code, unused_unsafe)]
extern crate alloc;
use driver_binding::{BindingEvent, BindingQueue, BindingRecord, BindingState, BindingId, StopIntent, Heartbeat, EventDecision};
use std::{cell::RefCell, collections::VecDeque};
thread_local! {
    static CLOCKS: RefCell<VecDeque<u64>> = RefCell::new(VecDeque::new());
    static FRAMES: RefCell<VecDeque<Vec<u8>>> = RefCell::new(VecDeque::new());
    static EFFECTS: RefCell<Vec<(bool, u64)>> = RefCell::new(Vec::new());
}
fn clock() -> u64 { CLOCKS.with_borrow_mut(|clocks| clocks.pop_front().unwrap_or(101)) }
fn print(_: &[u8]) {}
fn print_driver_name(_: &[u8]) {}
fn close(_: u64) { panic!("control fixture has no transferred handles"); }
mod wire { pub struct Handles; impl Handles { pub fn as_slice(&self) -> &[u64] { &[] } } }
enum PolledCaps { Message { len: usize, handles: wire::Handles }, Empty, Closed }
fn try_recv_caps(_: u64, buf: &mut [u8]) -> PolledCaps {
    FRAMES.with_borrow_mut(|frames| match frames.pop_front() {
        Some(frame) => { buf[..frame.len()].copy_from_slice(&frame); PolledCaps::Message { len: frame.len(), handles: wire::Handles } }
        None => PolledCaps::Empty,
    })
}
struct Offers;
impl Offers { fn push(&mut self, _: u16, _: u16, _: u64) -> bool { panic!("control fixture is not an offer"); } }
struct Binding { channel: u64 }
struct Incident { teardown_reserve: u64 }
impl Incident { fn open() -> Self { Self { teardown_reserve: 20 } } }
struct Teardown { intent: StopIntent, landed: Option<BindingState>, retrying: bool }
struct Entry { requires: &'static [u16], provides: &'static [(u16, u16, u16)] }
static PROVIDER: Entry = Entry { requires: &[], provides: &[(1, 0, 1)] };
static MIDDLE: Entry = Entry { requires: &[1], provides: &[(2, 0, 1)] };
static LEAF: Entry = Entry { requires: &[2], provides: &[] };
static OTHER: Entry = Entry { requires: &[], provides: &[] };
struct Node {
    record: BindingRecord, id: BindingId, binding: Option<Binding>, teardown: Option<Teardown>,
    stop_intent: StopIntent, stop_deadline: u64, ready_deadline: u64, queue: BindingQueue,
    last_opcode: u16, last_frame_at: u64, beat: Heartbeat, offers: Offers, incident: Incident,
    candidates: Vec<&'static Entry>, candidate: usize, running: Option<usize>, preferred: Option<usize>,
    disabled_by_policy: bool, retry_pending: bool, restart_requested: bool, retry_once: bool,
    retry_at: u64, selection_pending: bool,
}
impl Node {
    fn new(channel: u64, entry: &'static Entry) -> Self {
        let mut record = BindingRecord::new(); assert!(record.move_to(BindingState::Binding, None));
        Self { record, id: BindingId::new(0, channel as u8, 0, 1), binding: Some(Binding { channel }), teardown: None,
            stop_intent: StopIntent::Fault, stop_deadline: 0, ready_deadline: 100, queue: BindingQueue::new(),
            last_opcode: 0, last_frame_at: 0, beat: Heartbeat::default(), offers: Offers, incident: Incident::open(),
            candidates: vec![entry], candidate: 0, running: Some(0), preferred: None,
            disabled_by_policy: false, retry_pending: false, restart_requested: false, retry_once: false,
            retry_at: 0, selection_pending: false }
    }
    fn push(&mut self, event: BindingEvent) -> bool { self.queue.push(event) }
    fn driver_name(&self) -> &'static [u8] { b"fixture" }
    fn finish_operator_attempt(&mut self) { self.retry_pending = false; self.retry_once = false; }
    NODE_ENTRY
}
mod proto { pub mod system { #[derive(PartialEq, Eq)] pub enum PolicyVerb { Disable, Enable, Select, Retry } } }
const MAX_AUTOMATIC_ATTEMPTS: u32 = 3;
fn candidate_position(_: &Node, _: &[u8]) -> Option<usize> { None }
struct Catalogue { entries: Vec<(BindingId, u16)> }
impl Catalogue {
    fn count_of(&self, kind: u16) -> usize { self.entries.iter().filter(|entry| entry.1 == kind).count() }
    fn count_for(&self, id: BindingId, kind: u16) -> usize { self.entries.iter().filter(|entry| entry.0 == id && entry.1 == kind).count() }
    fn withdraw_binding(&mut self, id: BindingId) {
        self.entries.retain(|entry| entry.0 != id);
        EFFECTS.with_borrow_mut(|effects| effects.push((false, id.dev as u64)));
    }
}
fn send_frame(channel: u64, opcode: driver_protocol::Opcode, _: u64, _: &[u8], _: u64, _: u64) -> bool {
    assert_eq!(opcode, driver_protocol::Opcode::Stop);
    EFFECTS.with_borrow_mut(|effects| effects.push((true, channel))); true
}
#[cfg(test)] mod tests;
'''

TESTS = r'''
use super::*;
fn frame(generation: u64) -> Vec<u8> {
    driver_protocol::Header { version: driver_protocol::VERSION, opcode: driver_protocol::Opcode::Ready, generation, payload_len: 0 }.encode().to_vec()
}
#[test]
fn ready_receipt_deadline() {
    for (clocks, timely) in [([99, 99, 99], true), ([99, 99, 100], false), ([99, 99, 101], false), ([101, 101, 101], false)] {
        let mut node = Node::new(1, &OTHER);
        CLOCKS.with_borrow_mut(|values| *values = clocks.into());
        FRAMES.with_borrow_mut(|frames| *frames = [frame(1)].into());
        unsafe { drain_channel(&mut node, &mut [0; 128]); }
        // Supervision may run after receipt but before the queued lifecycle event is reduced.
        if driver_binding::handshake_expired(node.record.state, node.ready_deadline, 101) {
            node.push(BindingEvent::TimedOut { generation: 1 });
        }
        let event = node.queue.pop(1).expect("deadline or READY must be queued");
        let EventDecision::Admitted { next_state, .. } = driver_binding::reduce_event(node.record.state, event) else { panic!("admission refused"); };
        assert!(next_state == Some(if timely { BindingState::Online } else { BindingState::Stopping }), "receipt clocks {clocks:?}");
    }
}
#[test]
fn stale_and_full_queue_keep_deadline() {
    let mut node = Node::new(1, &OTHER);
    CLOCKS.with_borrow_mut(|values| *values = [99, 99].into());
    FRAMES.with_borrow_mut(|frames| *frames = [frame(2)].into());
    unsafe { drain_channel(&mut node, &mut [0; 128]); }
    assert!(node.queue.is_empty());
    for _ in 0..driver_binding::MAX_NODE_EVENTS { assert!(node.push(BindingEvent::Offered { generation: 1 })); }
    CLOCKS.with_borrow_mut(|values| *values = [99, 99, 101].into());
    FRAMES.with_borrow_mut(|frames| *frames = [frame(1)].into());
    unsafe { drain_channel(&mut node, &mut [0; 128]); }
    assert!(driver_binding::handshake_expired(node.record.state, node.ready_deadline, 101));
    while node.queue.pop(1).is_some() {}
    unsafe { drain_channel(&mut node, &mut [0; 128]); }
    assert!(matches!(node.queue.pop(1), Some(BindingEvent::TimedOut { .. })));
}
#[test]
fn disable_orders_dependency_closure() {
    for binding in [false, true] {
        for alternate in [false, true] {
            EFFECTS.with_borrow_mut(Vec::clear);
            let mut nodes = vec![Node::new(1, &PROVIDER), Node::new(2, &MIDDLE), Node::new(3, &LEAF), Node::new(4, &OTHER)];
            for node in &mut nodes { if !binding { assert!(node.record.move_to(BindingState::Online, None)); } }
            // A next-bind selection must not change the dependencies of the running driver.
            nodes[1].candidates.push(&OTHER); nodes[1].candidate = 1;
            let mut catalogue = Catalogue { entries: vec![(nodes[0].id, 1), (nodes[1].id, 2)] };
            if alternate { catalogue.entries.push((nodes[3].id, 1)); }
            unsafe { apply_policy(&mut nodes, 0, proto::system::PolicyVerb::Disable, "", &mut catalogue); }
            let effects = EFFECTS.with_borrow(Clone::clone);
            let stops: Vec<u64> = effects.iter().filter_map(|&(stop, id)| stop.then_some(id)).collect();
            assert_eq!(stops, if alternate { vec![1] } else { vec![3, 2, 1] });
            for id in &stops {
                assert!(effects.iter().position(|effect| effect == &(false, *id)).unwrap() < effects.iter().position(|effect| effect == &(true, *id)).unwrap());
            }
            assert!(nodes[0].disabled_by_policy && nodes[0].stop_intent == StopIntent::OperatorDisable);
            if !alternate {
                for node in &nodes[1..3] { assert!(node.record.state == BindingState::Stopping && node.stop_intent == StopIntent::DependencyLost && node.stop_deadline > 101 && !node.disabled_by_policy); }
            } else { assert!(nodes[1].record.state == if binding { BindingState::Binding } else { BindingState::Online }); }
            assert!(nodes[3].record.state != BindingState::Stopping);
        }
    }
}
'''


def fixture(source: str) -> str:
    functions = ["unsafe fn drain_channel(", "unsafe fn expire_planned_stop(", "unsafe fn planned_stop_deadline(", "unsafe fn apply_policy(", "unsafe fn begin_operator_stop(", "unsafe fn begin_dependency_stop(", "unsafe fn stop_nodes_that_lost_a_dependency(", "fn stoppable_on_a_lost_dependency(", "fn requirements_met(", "fn dependency_depths("]
    return FIXTURE.replace("NODE_ENTRY", item(source, "fn entry(&self)")) + "\n".join(item(source, start) for start in functions)


def main() -> None:
    source = (ROOT / "src/user/services/core/src/device_manager.rs").read_text()
    receipt = "driver_binding::handshake_expired(node.record.state, node.ready_deadline, node.last_frame_at)"
    order = "\t\t\tstop_nodes_that_lost_a_dependency(nodes, catalogue);"
    assert source.count(receipt) == 1 and source.count(order) == 1
    variants = [("production", source, None), ("late READY admitted", source.replace(receipt, "false", 1), "ready_receipt_deadline"), ("provider stopped first", source.replace(order, "", 1), "disable_orders_dependency_closure")]
    with tempfile.TemporaryDirectory(prefix="driver-lifecycle-") as directory:
        root = Path(directory)
        (root / "src").mkdir()
        (root / "Cargo.toml").write_text('[package]\nname="driver-lifecycle"\nversion="0.1.0"\nedition="2024"\n[dependencies]\ndriver-binding={path="' + str(ROOT / 'src/user/libs/driver/binding') + '"}\ndriver-protocol={path="' + str(ROOT / 'src/user/libs/driver/protocol') + '"}\n')
        (root / "src/tests.rs").write_text(TESTS)
        for name, variant, test in variants:
            (root / "src/lib.rs").write_text(fixture(variant))
            command = ["cargo", "test", "--offline", "--manifest-path", str(root / "Cargo.toml"), "--target-dir", str(root / "target"), "--lib"]
            if test:
                command.append(test)
            result = subprocess.run(command, capture_output=True, text=True, timeout=60)
            if test:
                if result.returncode != 101 or f"tests::{test} ... FAILED" not in result.stdout:
                    raise SystemExit(f"{name}: expected the named assertion failure:\n{result.stdout}\n{result.stderr}")
            elif result.returncode:
                raise SystemExit(f"{name}: {result.stdout}\n{result.stderr}")
            print(f"driver-lifecycle: {name} {'rejected' if test else 'passed (3 tests)'}")


if __name__ == "__main__":
    main()
