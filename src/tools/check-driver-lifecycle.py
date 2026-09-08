#!/usr/bin/env python3
"""Execute production driver deadlines, bounded supervision and dependency-stop ordering."""
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
    static REFILL: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static REFILL_FRAME: RefCell<Vec<u8>> = RefCell::new(Vec::new());
    static READS: RefCell<Vec<u64>> = RefCell::new(Vec::new());
}
fn clock() -> u64 { CLOCKS.with_borrow_mut(|clocks| clocks.pop_front().unwrap_or(101)) }
fn print(_: &[u8]) {}
fn print_driver_name(_: &[u8]) {}
fn close(_: u64) { panic!("control fixture has no transferred handles"); }
mod wire { pub struct Handles; impl Handles { pub fn as_slice(&self) -> &[u64] { &[] } } }
enum PolledCaps { Message { len: usize, handles: wire::Handles }, Empty, Closed }
fn try_recv_caps(channel: u64, buf: &mut [u8]) -> PolledCaps {
    READS.with_borrow_mut(|reads| {
        reads.push(channel);
        assert!(reads.len() <= 4096, "one refilled channel never returned to supervision");
    });
    if REFILL.get() == channel {
        // The peer refills one consumed slot, without ever exceeding channel capacity.
        return REFILL_FRAME.with_borrow(|frame| {
            let len = if frame.is_empty() {
                buf[..driver_protocol::HEADER_LEN].fill(0); driver_protocol::HEADER_LEN
            } else {
                buf[..frame.len()].copy_from_slice(frame); frame.len()
            };
            PolledCaps::Message { len, handles: wire::Handles }
        });
    }
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
fn supervised(channel: u64) -> Node {
    let mut node = Node::new(channel, &OTHER);
    assert!(node.record.move_to(BindingState::Online, None));
    node.ready_deadline = 0;
    node.beat.arm(Some(10), 0, 5);
    assert_eq!(node.beat.tick(5), driver_binding::Beat::Ask(1));
    node.beat.asked(5);
    node
}
fn pong(sequence: u32) -> Vec<u8> {
    let mut frame = driver_protocol::Header { version: driver_protocol::VERSION, opcode: driver_protocol::Opcode::Pong, generation: 1, payload_len: 4 }.encode().to_vec();
    frame.extend_from_slice(&sequence.to_le_bytes()); frame
}
fn consume(node: &mut Node) -> usize {
    let mut expiries = 0;
    while let Some(event) = node.queue.pop(1) {
        expiries += usize::from(matches!(event, BindingEvent::Wedged { .. }));
        if let EventDecision::Admitted { next_state: Some(next), cause, .. } = driver_binding::reduce_event(node.record.state, event) {
            assert!(node.record.move_to(next, cause));
        }
    }
    expiries
}
#[test]
fn refilled_channel_returns_to_other_nodes() {
    READS.with_borrow_mut(Vec::clear);
    REFILL.set(1);
    let mut nodes = [supervised(1), supervised(2)];
    unsafe { tick_heartbeats(&mut nodes, &mut [0; 128]); }
    let reads = READS.with_borrow(Clone::clone);
    assert!(reads.contains(&2), "another node must be serviced despite continuing malformed traffic");
    assert_eq!(reads.iter().filter(|&&channel| channel == 1).count(), MAX_DRIVER_FRAMES_PER_PASS);
    for node in &mut nodes {
        assert_eq!(consume(node), 1, "both expired watchdogs must reach their lifecycle queues");
        assert!(node.record.state == BindingState::Stopping);
    }
}
#[test]
fn full_queue_preserves_watchdog_expiry() {
    for count in [driver_binding::MAX_NODE_EVENTS - 1, driver_binding::MAX_NODE_EVENTS] {
        let mut node = supervised(1);
        FRAMES.with_borrow_mut(|frames| *frames = (0..count).map(|_| pong(2)).collect());
        let wake = unsafe { tick_heartbeats(core::slice::from_mut(&mut node), &mut [0; 128]) };
        let delivered = consume(&mut node);
        if count == driver_binding::MAX_NODE_EVENTS {
            assert_eq!(delivered, 0);
            assert_eq!(wake, 15, "a full queue must leave expiry runnable");
            // A late matching reply cannot cancel the expiry while delivery is pending.
            FRAMES.with_borrow_mut(|frames| frames.push_back(pong(1)));
            unsafe { tick_heartbeats(core::slice::from_mut(&mut node), &mut [0; 128]); }
            assert_eq!(consume(&mut node), 1);
        } else {
            assert_eq!(delivered, 1);
        }
        assert!(node.record.state == BindingState::Stopping);
        assert_eq!(node.beat.wake_at(), 0);
        assert_eq!(node.beat.tick(100000), driver_binding::Beat::Idle);
    }
}
#[test]
fn continuing_traffic_cannot_starve_pending_expiry() {
    let mut node = supervised(1);
    REFILL.set(1);
    REFILL_FRAME.with_borrow_mut(|frame| *frame = pong(2));
    unsafe { tick_heartbeats(core::slice::from_mut(&mut node), &mut [0; 128]); }
    assert_eq!(consume(&mut node), 0);
    assert!(node.beat.expiry_pending());
    // The standing loop can receive a readable channel after consuming its queue, before
    // the next heartbeat tick. That intake must also prioritize the pending verdict.
    unsafe { drain_channel(&mut node, &mut [0; 128]); }
    unsafe { tick_heartbeats(core::slice::from_mut(&mut node), &mut [0; 128]); }
    assert_eq!(consume(&mut node), 1, "fresh traffic cannot overtake an already pending expiry");
    assert!(node.record.state == BindingState::Stopping);
    assert_eq!(node.beat.wake_at(), 0);
}
#[test]
fn an_expiry_losing_to_exit_cannot_fault_the_replacement_handshake() {
    let mut node = supervised(1);
    node.push(BindingEvent::Exited { generation: 1 });
    FRAMES.with_borrow_mut(|frames| *frames = (1..driver_binding::MAX_NODE_EVENTS).map(|_| pong(2)).collect());
    unsafe { tick_heartbeats(core::slice::from_mut(&mut node), &mut [0; 128]); }
    assert_eq!(consume(&mut node), 0);
    assert!(node.record.state == BindingState::Stopping && node.beat.expiry_pending());
    // Confirmed teardown and retry install a new binding before READY rearms its heartbeat.
    assert!(node.record.move_to(BindingState::Backoff, None));
    assert!(node.record.move_to(BindingState::Binding, None));
    node.id.generation = 2;
    node.ready_deadline = 200;
    unsafe { drain_channel(&mut node, &mut [0; 128]); }
    assert!(node.queue.is_empty(), "the previous generation's heartbeat cannot fault a replacement handshake");
    assert!(node.record.state == BindingState::Binding);
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
    functions = ["unsafe fn drain_channel(", "unsafe fn tick_heartbeats(", "unsafe fn expire_planned_stop(", "unsafe fn planned_stop_deadline(", "unsafe fn apply_policy(", "unsafe fn begin_operator_stop(", "unsafe fn begin_dependency_stop(", "unsafe fn stop_nodes_that_lost_a_dependency(", "fn stoppable_on_a_lost_dependency(", "fn requirements_met(", "fn dependency_depths("]
    bound = next(line for line in source.splitlines() if line.startswith("const MAX_DRIVER_FRAMES_PER_PASS:"))
    return FIXTURE.replace("NODE_ENTRY", item(source, "fn entry(&self)")) + bound + "\n" + "\n".join(item(source, start) for start in functions)


def main() -> None:
    source = (ROOT / "src/user/services/core/src/device_manager.rs").read_text()
    receipt = "driver_binding::handshake_expired(node.record.state, node.ready_deadline, node.last_frame_at)"
    order = "\t\t\tstop_nodes_that_lost_a_dependency(nodes, catalogue);"
    assert source.count(receipt) == 1 and source.count(order) == 1
    drain = "for _ in 0..MAX_DRIVER_FRAMES_PER_PASS {"
    expiry = "if node.push(BindingEvent::Wedged { generation }) {"
    pending = "if node.record.state == BindingState::Online && node.beat.expiry_pending() && node.push(BindingEvent::Wedged { generation: node.id.generation }) {\n\t\t\tnode.beat.expiry_queued();\n\t\t}"
    assert source.count(drain) == 1 and source.count(expiry) == 1 and source.count(pending) == 1
    variants = [
        ("production", source, None),
        ("late READY admitted", source.replace(receipt, "false", 1), "ready_receipt_deadline"),
        ("provider stopped first", source.replace(order, "", 1), "disable_orders_dependency_closure"),
        ("unbounded driver drain", source.replace(drain, "loop {", 1), "refilled_channel_returns_to_other_nodes"),
        ("refused expiry discarded", source.replace(expiry, "if { node.push(BindingEvent::Wedged { generation }); true } {", 1), "full_queue_preserves_watchdog_expiry"),
        ("pending expiry behind fresh traffic", source.replace(pending, "", 1), "continuing_traffic_cannot_starve_pending_expiry"),
        ("old expiry faults replacement", source.replace(pending, pending.replace("node.record.state == BindingState::Online && ", ""), 1), "an_expiry_losing_to_exit_cannot_fault_the_replacement_handshake"),
    ]
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
            print(f"driver-lifecycle: {name} {'rejected' if test else 'passed (7 tests)'}")


if __name__ == "__main__":
    main()
