#!/usr/bin/env python3
"""Exercise production bring-up scheduling, retry gates and subscription snapshots on the host."""
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def item(source: str, start: str) -> str:
    begin = source.index(start)
    brace = source.index("{", begin)
    depth = 1
    end = brace + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[begin:end]


FIXTURE = r'''
#![allow(dead_code, unused_unsafe, unused_variables)]
use std::{cell::RefCell, collections::HashMap};
use driver_binding::{BindingEvent, BindingId, BindingRecord, BindingState, FailureCause, ProviderId};
mod abi { pub const MAX_WAIT_HANDLES: usize = 256; pub const ERR_TIMED_OUT: i64 = -1; }
const WAIT_CHANNEL: u8 = 0;
const WAIT_EXIT: u8 = 1;
const WAIT_CLAIM: u8 = 2;
const MAX_SUBSCRIBERS: usize = 8;
struct Runtime { now: u64, waits: Vec<Vec<u64>>, due: Vec<u64>, ready: Vec<u64>, closed: Vec<u64>, killed: Vec<u64>, next: u64, queues: HashMap<u64, (usize, usize)>, delivered: u64, binds: usize }
impl Default for Runtime { fn default() -> Self { Self { now: 10, waits: Vec::new(), due: Vec::new(), ready: Vec::new(), closed: Vec::new(), killed: Vec::new(), next: 1000, queues: HashMap::new(), delivered: 0, binds: 0 } } }
thread_local! { static RT: RefCell<Runtime> = RefCell::new(Runtime::default()); }
fn clock() -> u64 { RT.with_borrow(|rt| rt.now) }
fn print(_: &[u8]) {}
fn close(handle: u64) { RT.with_borrow_mut(|rt| rt.closed.push(handle)); }
fn sleep_until(due: u64) { RT.with_borrow_mut(|rt| { rt.now = due; rt.due.push(due); }); }
fn wait_any(handles: &[u64], due: u64) -> i64 { RT.with_borrow_mut(|rt| {
    rt.waits.push(handles.to_vec()); rt.due.push(due);
    if let Some(at) = handles.iter().position(|handle| rt.ready.contains(handle)) { at as i64 }
    else { rt.now = due; abi::ERR_TIMED_OUT }
}) }
struct ClaimInfo { settled: u32, state: u32 }
fn device_claim_info(_: u64) -> Result<ClaimInfo, ()> { Ok(ClaimInfo { settled: 1, state: driver_binding::CLAIM_FREE }) }
fn channel() -> Option<(u64,u64)> { channel_with_depth(0) }
fn channel_with_depth(depth: u64) -> Option<(u64,u64)> { RT.with_borrow_mut(|rt| {
    let producer = rt.next; rt.next += 2;
    let depth = if depth == 0 { DEFAULT_DEPTH } else { depth.min(MAX_DEPTH) as usize };
    rt.queues.insert(producer, (depth, 0)); Some((producer, producer + 1))
}) }
fn try_send_caps(producer: u64, _: &[u8], _: &[u64]) -> bool { RT.with_borrow_mut(|rt| {
    let Some((depth, count)) = rt.queues.get_mut(&producer) else { return false };
    if *count == *depth { return false; } *count += 1; true
}) }
fn send_blocking(_: u64, _: &[u8], consumer: u64) -> bool { RT.with_borrow_mut(|rt| rt.delivered = consumer); true }
enum PolledCaps { Empty, Closed }
fn try_recv_caps(_: u64, _: &mut [u8]) -> PolledCaps { PolledCaps::Empty }
mod wire { pub struct Handles; impl Handles { pub fn new() -> Self { Self } pub fn as_slice(&self) -> &[u64] { &[] } } }
mod proto { pub mod system {
    pub struct ProviderInfo { pub kind: u16, pub bus: u32, pub dev: u32, pub func: u32, pub binding_generation: u64, pub slot: u32, pub provider_generation: u32, pub live: bool }
    pub mod provider_catalogue {
        pub fn subscribe_frame(_: u32, _: &super::ProviderInfo, _: &mut [u8], _: &mut crate::wire::Handles) -> Option<usize> { Some(1) }
        pub fn subscribe_open(_: &mut crate::CatalogueView, _: &[u8], _: &mut crate::wire::Handles) -> Option<(u32, ())> { Some((7, ())) }
    }
} }
struct Provider { id: ProviderId, kind: u16 }
struct Subscriber { producer: u64, kind: u16, seq: u32 }
struct Catalogue { entries: Vec<Option<Provider>>, subscribers: [Option<Subscriber>; MAX_SUBSCRIBERS] }
impl Catalogue { fn new() -> Self { Self { entries: Vec::new(), subscribers: [const { None }; MAX_SUBSCRIBERS] } } }
struct CatalogueView<'a> { catalogue: &'a mut Catalogue, nodes: &'a [Node] }
fn provider_kind_from_wire(kind: u16) -> u16 { kind }
fn subscribed_kind(request: &[u8]) -> Option<u16> { request.first().map(|&kind| kind as u16) }
struct Entry { name: &'static [u8], artifact: &'static [u8], requires: &'static [u16] }
static NEEDS_BLOCK: Entry = Entry { name: b"consumer", artifact: b"consumer", requires: &[1] };
struct Binding { channel: u64, process: u64 }
struct Teardown { pending: driver_binding::Pending, deadline: u64 }
struct Offers;
impl Offers { fn close_all(&mut self) {} }
struct Node { record: BindingRecord, id: BindingId, binding: Option<Binding>, teardown: Option<Teardown>, ready_deadline: u64, stop_deadline: u64, retry_at: u64, restart_requested: bool, waiting_for_claim: bool, candidates: Vec<&'static Entry>, candidate: usize, info: u64, offers: Offers, queue: Vec<BindingEvent> }
impl Node {
    fn new() -> Self { Self { record: BindingRecord::new(), id: BindingId::new(0,0,0,1), binding: None, teardown: None, ready_deadline: 1000, stop_deadline: 50, retry_at: 0, restart_requested: false, waiting_for_claim: false, candidates: vec![&NEEDS_BLOCK], candidate: 0, info: 0, offers: Offers, queue: Vec::new() } }
    fn driver_name(&self) -> &'static [u8] { b"fixture" }
    fn push(&mut self, event: BindingEvent) { self.queue.push(event); }
    IN_FLIGHT
}
struct Closes;
impl driver_binding::Closes for Closes {
    fn kill(&mut self, _: u64) {}
    fn close(&mut self, handle: u64) { close(handle); }
    fn release(&mut self, _: u64) -> Option<u32> { Some(driver_binding::CLAIM_FREE) }
    fn kill_domain(&mut self, domain: u64) { RT.with_borrow_mut(|rt| rt.killed.push(domain)); }
}
enum Step { Waiting, Online, Again, NextCandidate, Done, Resting }
// The scheduler owns dispatch; the effect boundary uses the production Pending ledger.
fn advance(node: &mut Node, _: &[u8], _: &mut Catalogue) -> Step {
    for event in node.queue.drain(..) { if let Some(teardown) = node.teardown.as_mut() { teardown.pending.note(event); } }
    if let Some(teardown) = node.teardown.as_mut() {
        match teardown.pending.settle(&mut Closes, clock(), teardown.deadline) {
            Some(driver_binding::Settled::Free) => { node.teardown = None; node.record.state = BindingState::Backoff; node.retry_at = clock() + 10; return Step::Again; }
            Some(driver_binding::Settled::Unconfirmed) => { node.teardown = None; node.record.state = BindingState::Quarantined; return Step::Done; }
            None => return Step::Waiting,
        }
    }
    if node.record.state == BindingState::Backoff { Step::Again } else { Step::Waiting }
}
fn tick_heartbeats(_: &mut [Node], _: &mut [u8]) -> u64 { 0 }
fn tick_handshakes(_: &mut [Node], soonest: u64) -> u64 { soonest }
fn drain_channel(_: &mut Node, _: &mut [u8]) {}
fn settle_dependencies(_: &mut [Node], _: &mut Catalogue) -> usize { 0 }
struct Package;
impl Package { fn lookup(&self, _: &[u8]) -> Option<&[u8]> { Some(&[]) } }
fn begin_bind(node: &mut Node, _: &u64, _: &[u8], _: &[u8], _: u64, _: u64, _: u64, _: u64) { node.record.state = BindingState::Binding; RT.with_borrow_mut(|rt| rt.binds += 1); }
unsafe fn boot_retry_pass(nodes: &mut [Node], catalogue: &mut Catalogue) {
    let first_node = 0; let package = Package; let power = 0; let console_input = 0; let device_privilege = 0;
    RETRY_LOOPS
}
fn provider(slot: u16, kind: u16) -> Option<Provider> { Some(Provider { id: ProviderId::new(BindingId::new(0,0,0,1), slot, 1), kind }) }
fn pending_node(process: u64, claim: u64, domain: u64, state: Option<u32>) -> Node {
    let mut node = Node::new(); node.record.state = BindingState::Stopping;
    node.teardown = Some(Teardown { pending: driver_binding::Pending { process, claim, domain, exited: process == 0, state }, deadline: 50 }); node
}
fn main() { unsafe {
    let case = std::env::args().nth(1).unwrap();
    let mut catalogue = Catalogue::new(); let mut buf = [0; 128];
    match case.as_str() {
        "ready-teardown" => {
            let mut nodes = vec![pending_node(0, 0, 99, Some(driver_binding::CLAIM_FREE))];
            assert!(pump(&mut nodes, 0, &mut catalogue, &mut buf), "settlement must run before phase exit");
            assert!(RT.with_borrow(|rt| rt.waits.is_empty() && rt.due.is_empty()), "a completed rollback must not wait");
            assert!(matches!(advance(&mut nodes[0], b"fixture", &mut catalogue), Step::Again));
            assert!(RT.with_borrow(|rt| rt.killed == [99] && rt.closed == [99]), "the production Pending must release the child Domain exactly once");
            assert!(pump(&mut nodes, 0, &mut catalogue, &mut buf), "backoff must remain scheduled");
            catalogue.entries.push(provider(0, 1)); boot_retry_pass(&mut nodes, &mut catalogue);
            assert!(nodes[0].record.state == BindingState::Binding);
        }
        "ready-beside-handshake" => {
            let mut binding = Node::new(); binding.binding = Some(Binding { channel: 11, process: 12 }); binding.record.state = BindingState::Binding;
            let mut nodes = vec![pending_node(0, 0, 99, Some(driver_binding::CLAIM_FREE)), binding];
            assert!(pump(&mut nodes, 0, &mut catalogue, &mut buf));
            assert!(RT.with_borrow(|rt| rt.waits.is_empty()), "another handshake must not delay completed cleanup");
        }
        "earlier-teardown" => {
            let mut binding = Node::new(); binding.binding = Some(Binding { channel: 11, process: 12 }); binding.record.state = BindingState::Binding;
            let mut nodes = vec![pending_node(21, 22, 99, None), binding];
            RT.with_borrow_mut(|rt| rt.ready = vec![21,22]);
            assert!(pump(&mut nodes, 1, &mut catalogue, &mut buf));
            assert!(RT.with_borrow(|rt| rt.waits[0].contains(&21) && rt.waits[0].contains(&22) && rt.due[0] == 50), "earlier teardown owns waits and the first deadline");
            assert!(pump(&mut nodes, 1, &mut catalogue, &mut buf));
            let _ = pump(&mut nodes, 1, &mut catalogue, &mut buf);
            assert!(nodes[0].teardown.is_none() && nodes[0].record.state == BindingState::Backoff, "earlier confirmations must settle before an unrelated handshake expires");
            assert!(RT.with_borrow(|rt| rt.killed == [99]));
        }
        "queued-event" => {
            let mut node = Node::new(); node.record.state = BindingState::Stopping;
            node.binding = Some(Binding { channel: 11, process: 12 }); node.stop_deadline = 0;
            node.push(BindingEvent::Stopped { generation: node.id.generation });
            assert!(pump(&mut [node], 0, &mut catalogue, &mut buf));
            assert!(RT.with_borrow(|rt| rt.waits.is_empty()), "a queued STOPPED must run without waiting for another event");
        }
        "unwaitable-teardown" => {
            let mut nodes = vec![pending_node(0, 0, 99, None)];
            assert!(pump(&mut nodes, 0, &mut catalogue, &mut buf), "a teardown without a waitable confirmation still owns its deadline");
            assert_eq!(clock(), 50);
            assert!(matches!(advance(&mut nodes[0], b"fixture", &mut catalogue), Step::Done));
            assert!(nodes[0].record.state == BindingState::Quarantined);
        }
        "retry-dependencies" => {
            // Both the deadline/restart pass and the immediate Step::Again branch must gate.
            for (restart, due) in [(false, 10), (true, 0), (false, 0)] {
                let mut node = Node::new(); node.record.state = BindingState::Backoff; node.restart_requested = restart; node.retry_at = due;
                let mut nodes = vec![node]; boot_retry_pass(&mut nodes, &mut catalogue);
                assert!(nodes[0].record.state == BindingState::DependencyPending, "a boot retry must not claim without its requirement");
            }
            assert_eq!(RT.with_borrow(|rt| rt.binds), 0);
            catalogue.entries.push(provider(0, 1));
            let mut node = Node::new(); node.record.state = BindingState::Backoff; node.retry_at = 10;
            boot_retry_pass(&mut [node], &mut catalogue);
            assert_eq!(RT.with_borrow(|rt| rt.binds), 1, "a present dependency permits exactly one bind");
        }
        "snapshot" => {
            for count in [0, 1, 64, 65, 128] {
                RT.with_borrow_mut(|rt| *rt = Runtime::default());
                let mut catalogue = Catalogue::new(); catalogue.entries = (0..count).map(|at| provider(at, 1)).collect();
                catalogue.entries.push(provider(count, 2));
                open_subscription(1, &mut catalogue, &[], &[1], &mut wire::Handles::new());
                let subscriber = catalogue.subscribers.iter().flatten().next().expect("complete snapshot must retain the live stream");
                assert_eq!(subscriber.seq, count as u32);
                RT.with_borrow_mut(|rt| {
                    assert_eq!(rt.delivered, subscriber.producer + 1);
                    assert_eq!(rt.queues[&subscriber.producer].1, count as usize);
                    assert!(rt.closed.is_empty());
                    rt.queues.get_mut(&subscriber.producer).unwrap().1 = 0;
                });
                catalogue.announce(&provider(count, 1).unwrap(), true);
                catalogue.announce(&provider(0, 1).unwrap(), false);
                assert_eq!(catalogue.subscribers.iter().flatten().next().unwrap().seq, count as u32 + 2, "adds and withdrawals follow the full snapshot");
            }
        }
        _ => panic!("unknown fixture"),
    }
} }
'''


def source_fixture(source: str) -> str:
    boot = item(source, "unsafe fn launch_boot_drivers(")
    loop_start = boot.index("while pump(")
    first = boot.index("for at in first_node..nodes.len()", loop_start)
    retry = item(boot[first:], "for at in first_node..nodes.len()")
    second = first + len(retry)
    retry += item(boot[second:], "for at in first_node..nodes.len()")
    kernel = (ROOT / "src/kernel/object/channel/mod.rs").read_text()
    default = re.search(r"const CHANNEL_QUEUE_DEFAULT: usize = (\d+)", kernel)[1]
    maximum = re.search(r"const CHANNEL_QUEUE_MAX: usize = (\d+)", kernel)[1]
    fixture = FIXTURE.replace("IN_FLIGHT", item(source, "fn in_flight(&self)")).replace("RETRY_LOOPS", retry)
    fixture = fixture.replace("DEFAULT_DEPTH", default).replace("MAX_DEPTH", maximum)
    functions = ["unsafe fn pump(", "fn requirements_met(", "unsafe fn gate_on_requirements(", "unsafe fn open_subscription(", "unsafe fn send_provider_frame(", "fn provider_info_wire("]
    methods = ["unsafe fn subscribe_stream(", "unsafe fn reap_dead_subscribers(", "unsafe fn announce(", "fn count_of("]
    return fixture + "\n".join(item(source, name) for name in functions) + "\nimpl Catalogue {\n" + "\n".join(item(source, name) for name in methods) + "\n}\n"


def main() -> None:
    source = (ROOT / "src/user/services/core/src/device_manager.rs").read_text()
    ready = item(source, "if nodes.iter().any(|node| node.teardown.as_ref().is_some_and(")
    queued = item(source, "if nodes.iter().skip(in_flight_from).any(|node| !node.queue.is_empty())")
    requirement = "if !gate_on_requirements(&mut nodes[at], entry, catalogue)"
    mutations = [
        ("completed teardown omitted", source.replace(ready, "", 1), "ready-teardown"),
        ("completed teardown waits behind a handshake", source.replace(ready, "", 1), "ready-beside-handshake"),
        ("earlier phase omitted from waits", source.replace("if !node.in_flight() || set + 2", "if at < in_flight_from || !node.in_flight() || set + 2", 1), "earlier-teardown"),
        ("earlier phase omitted from advance", source.replace("if nodes[at].record.state != BindingState::Online && !nodes[at].in_flight()", "if nodes[at].record.state != BindingState::Online", 1), "earlier-teardown"),
        ("confirmed exit remains in wait set", source.replace("if teardown.pending.process != 0 && !teardown.pending.exited", "if teardown.pending.process != 0", 1), "earlier-teardown"),
        ("queued event waits for another wake", source.replace(queued, "", 1), "queued-event"),
        ("unwaitable teardown loses deadline", source.replace("parked.into_iter().chain(nodes.iter().filter_map(|node| node.teardown.as_ref().map(|teardown| teardown.deadline))).min()", "parked", 1), "unwaitable-teardown"),
        ("expired boot retry requirement omitted", source.replace(requirement, "if false", 1), "retry-dependencies"),
        ("immediate boot retry requirement omitted", source[:source.index(requirement) + len(requirement)] + source[source.index(requirement) + len(requirement):].replace(requirement, "if false", 1), "retry-dependencies"),
        ("default snapshot depth", source.replace("channel_with_depth(depth)", "channel()", 1), "snapshot"),
    ]
    with tempfile.TemporaryDirectory(prefix="device-manager-progress-") as folder:
        root = Path(folder)
        (root / "src").mkdir()
        (root / "Cargo.toml").write_text('[package]\nname="device-manager-progress"\nedition="2024"\n[dependencies]\ndriver-binding={path="' + str(ROOT / 'src/user/libs/driver/binding') + '"}\n')
        for label, variant, cases in [("production", source, ["ready-teardown", "ready-beside-handshake", "earlier-teardown", "queued-event", "unwaitable-teardown", "retry-dependencies", "snapshot"])] + [(label, variant, [case]) for label, variant, case in mutations]:
            (root / "src/main.rs").write_text(source_fixture(variant))
            built = subprocess.run(["cargo", "build", "--offline", "--manifest-path", str(root / "Cargo.toml"), "--target-dir", str(root / "target")], capture_output=True, text=True)
            if built.returncode:
                raise SystemExit(f"device-manager-progress: {label} failed to compile:\n{built.stderr}")
            for case in cases:
                ran = subprocess.run([str(root / "target/debug/device-manager-progress"), case], capture_output=True, text=True, timeout=10)
                if (ran.returncode == 0) != (label == "production") or (ran.returncode != 0 and "panicked at" not in ran.stderr):
                    raise SystemExit(f"device-manager-progress: {label}/{case} gave the wrong verdict:\n{ran.stderr}")
            print(f"device-manager-progress: {label} {'passed' if label == 'production' else 'rejected'}")


if __name__ == "__main__":
    main()
