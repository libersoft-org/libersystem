#!/usr/bin/env python3
"""Exercise the development driver's actual busy-work, TX-wait and adoption control paths."""
from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
spec = spec_from_file_location("manager_progress", Path(__file__).with_name("check-device-manager-progress.py"))
progress = module_from_spec(spec)
spec.loader.exec_module(progress)
item = progress.item

FIXTURE = r'''
#![allow(dead_code, unused_unsafe)]
extern crate alloc;
use alloc::vec::Vec;
use std::{cell::{Cell, RefCell}, collections::VecDeque};
thread_local! {
    static NOW: Cell<u64> = const { Cell::new(10) };
    static SENDS: Cell<usize> = const { Cell::new(0) };
    static DELIVERIES: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
    static ATTEMPTS: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
    static REPOSTS: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
    static MODE: Cell<u8> = const { Cell::new(0) };
    static RX_READS: Cell<usize> = const { Cell::new(0) };
    static TX_READS: Cell<usize> = const { Cell::new(0) };
    static CONTROL_READS: Cell<usize> = const { Cell::new(0) };
    static WAITS: Cell<usize> = const { Cell::new(0) };
    static TX_DONE: Cell<bool> = const { Cell::new(false) };
    static QUIET: Cell<bool> = const { Cell::new(true) };
    static FRAMES: RefCell<VecDeque<(Vec<u8>, u64)>> = RefCell::new(VecDeque::new());
    static EFFECTS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    static CLOSED: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}
#[derive(Debug)] struct DriverExit;
fn exit() -> ! { std::panic::panic_any(DriverExit) }
fn print(_: &[u8]) {}
fn clock() -> u64 { NOW.get() }
fn close(handle: u64) { CLOSED.with_borrow_mut(|closed| closed.push(handle)); EFFECTS.with_borrow_mut(|effects| effects.push("close")); }
fn interrupt_ack(_: u64) {}
fn device_quiesced(device: u64) { assert_eq!(device, 7); EFFECTS.with_borrow_mut(|effects| effects.push("quiesced")); }
RUNTIME_FUNCTIONS
const SYS_CHANNEL_SEND: u64 = abi::SYS_CHANNEL_SEND;
const SYS_WAIT_ANY: u64 = abi::SYS_WAIT_ANY;
const SYS_WAIT: u64 = abi::SYS_WAIT;
const WAIT_PERIODIC: u64 = abi::WAIT_PERIODIC;
const WAIT_WRITABLE: u64 = abi::WAIT_WRITABLE;
const ERR_WOULD_BLOCK: i64 = abi::ERR_WOULD_BLOCK;
const ERR_TIMED_OUT: i64 = abi::ERR_TIMED_OUT;
fn yield_now() { panic!("blocking send did not service control"); }
unsafe fn syscall(op: u64, a: u64, b: u64, c: u64, d: u64) -> u64 {
    if op == SYS_CHANNEL_SEND {
        let payload = unsafe { core::slice::from_raw_parts(b as *const u8, c as usize) }.to_vec();
        SENDS.set(SENDS.get() + 1);
        assert!(SENDS.get() < 32, "retry loop failed to service control");
        ATTEMPTS.with_borrow_mut(|attempts| attempts.push(payload.clone()));
        assert_eq!(d, 0);
        if MODE.get() >= 10 {
            assert_eq!(a, 10, "pending old-session payload cannot be replayed to replacement");
            if MODE.get() == 16 {
                FRAMES.with_borrow_mut(|frames| frames.push_back((b"BYTES".to_vec(), 55)));
                return abi::ERR_INVALID as u64;
            }
            if MODE.get() != 12 || SENDS.get() <= 3 { return ERR_WOULD_BLOCK as u64; }
        }
        DELIVERIES.with_borrow_mut(|deliveries| deliveries.push(payload));
        return 0;
    }
    if op == SYS_WAIT {
        assert_eq!(a, 10); assert_eq!(b, 0); assert_eq!(c, WAIT_WRITABLE);
        FRAMES.with_borrow_mut(|frames| frames.push_back((frame(driver_protocol::Opcode::Stop, 1), 0)));
        assert!(EFFECTS.with_borrow(|effects| effects.contains(&"stopped")), "unlimited data writable wait hides pending STOP");
        return 0;
    }
    assert_eq!(op, SYS_WAIT_ANY);
    assert_eq!(unsafe { core::slice::from_raw_parts(a as *const u64, b as usize) }, &[9]);
    assert_eq!(c, NOW.get() + 1, "idle backpressure must use a finite retry wake");
    assert_eq!(d, WAIT_PERIODIC, "retry wake must permit scheduler settling");
    WAITS.set(WAITS.get() + 1);
    assert!(WAITS.get() < 8, "control was not serviced during repeated backpressure");
    NOW.set(c);
    if MODE.get() == 15 { return abi::ERR_INVALID as u64; }
    FRAMES.with_borrow_mut(|frames| {
        if WAITS.get() == 1 {
            frames.push_back((frame(driver_protocol::Opcode::Ping, 0), 0));
            frames.push_back((frame(driver_protocol::Opcode::Stop, 0), 0));
            frames.push_back((frame(driver_protocol::Opcode::Ping, 1), 0));
        }
        if WAITS.get() == 3 {
            match MODE.get() {
                10 | 11 => frames.push_back((frame(driver_protocol::Opcode::Stop, 1), 0)),
                14 => frames.push_back((b"BYTES".to_vec(), 55)),
                _ => {},
            }
        }
    });
    if WAITS.get() == 2 { abi::ERR_TIMED_OUT as u64 } else { 0 }
}
fn wait_any(handles: &[u64], deadline: u64) -> i64 {
    WAITS.set(WAITS.get() + 1);
    assert!(WAITS.get() < 8, "control was never serviced after its wake");
    if MODE.get() == 7 {
        assert!(handles.contains(&9));
        2
    } else if matches!(MODE.get(), 3 | 6) {
        assert_eq!(handles, &[8, 9], "TX ownership wait must include bootstrap");
        assert_eq!(deadline, 3010, "the existing transport timeout stays intact");
        FRAMES.with_borrow_mut(|frames| {
            frames.push_back((frame(driver_protocol::Opcode::Ping, 1), 0));
            if MODE.get() == 3 {
                frames.push_back((frame(driver_protocol::Opcode::Stop, 1), 0));
            } else {
                frames.push_back((b"BYTES".to_vec(), 55));
                TX_DONE.set(true);
            }
        });
        1
    } else if MODE.get() == 5 {
        assert!(handles.contains(&55), "the replacement channel must become the data wait target");
        1
    } else {
        panic!("the control fixture should finish before waiting");
    }
}
enum Polled { Message { len: usize, handle: u64 }, Empty, Closed }
fn try_recv(channel: u64, bytes: &mut [u8]) -> Polled {
    if channel == 9 {
        if MODE.get() == 13 && WAITS.get() >= 3 { return Polled::Closed; }
        CONTROL_READS.set(CONTROL_READS.get() + 1);
        assert!(CONTROL_READS.get() < 1024, "control loop did not return");
        let ready = matches!(MODE.get(), 1 | 7) && RX_READS.get() >= RX_SLOTS as usize
            || MODE.get() == 2 && RX_READS.get() >= 2 && TX_READS.get() >= RX_SLOTS as usize;
        let message = FRAMES.with_borrow_mut(VecDeque::pop_front)
            .or_else(|| ready.then(|| (frame(driver_protocol::Opcode::Stop, 1), 0)));
        if let Some((frame, handle)) = message {
            bytes[..frame.len()].copy_from_slice(&frame);
            return Polled::Message { len: frame.len(), handle };
        }
    } else if matches!(MODE.get(), 5 | 14 | 16) {
        if channel == 10 { return Polled::Closed; }
        assert_eq!(channel, 55);
        EFFECTS.with_borrow_mut(|effects| effects.push("replacement"));
        FRAMES.with_borrow_mut(|frames| frames.push_back((frame(driver_protocol::Opcode::Stop, 1), 0)));
        bytes[0] = 42;
        return Polled::Message { len: 1, handle: 0 };
    } else if matches!(MODE.get(), 2 | 11) {
        TX_READS.set(TX_READS.get() + 1);
        assert!(TX_READS.get() < 1024, "agent intake never yielded to the next pass");
        bytes[0] = 42;
        return Polled::Message { len: 1, handle: 0 };
    }
    Polled::Empty
}
struct Virtio { capability: u64 }
impl Virtio { fn read_isr(&self) -> u8 { 0 } }
struct Queue { capability: u64, rx: bool }
impl Queue {
    fn take_used(&mut self) -> Option<(u16, u32)> {
        if self.rx {
            RX_READS.set(RX_READS.get() + 1);
            assert!(RX_READS.get() < 1024, "receive ring never yielded to control");
            return match MODE.get() {
                1 => Some((0, 1)),
                4 | 7 => Some((0, 0)),
                10 | 14 | 16 if RX_READS.get() == 1 => Some((0, 1)),
                14 | 16 if RX_READS.get() == 2 => Some((1, 1)),
                _ => None
            };
        }
        TX_DONE.replace(false).then_some((0, 1))
    }
    fn post_recv(&mut self, id: u16, _: u64, _: u32) {
        if matches!(MODE.get(), 10 | 14) { assert!(WAITS.get() >= 3, "held RX descriptor reposted while delivery remains pending"); }
        REPOSTS.with_borrow_mut(|reposts| reposts.push(id));
    }
    fn notify(&self) {}
    fn submit_async(&mut self, _: &[(u64, u32, bool)]) -> bool {
        EFFECTS.with_borrow_mut(|effects| effects.push("submit"));
        TX_DONE.set(true);
        MODE.get() != 11
    }
}
fn frame(opcode: driver_protocol::Opcode, generation: u64) -> Vec<u8> {
    let payload = if opcode == driver_protocol::Opcode::Ping { 17u32.to_le_bytes().to_vec() } else { Vec::new() };
    let mut frame = driver_protocol::Header { version: driver_protocol::VERSION, opcode, generation, payload_len: payload.len() as u32 }.encode().to_vec();
    frame.extend_from_slice(&payload);
    frame
}
mod common {
    use super::*;
    use driver_protocol as proto;
    static STOP_PENDING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    pub struct Bind { pub generation: u64 }
    pub fn pong(_: u64, _: &Bind, sequence: u32) -> bool {
        assert_eq!(sequence, 17);
        EFFECTS.with_borrow_mut(|effects| effects.push("pong"));
        true
    }
    pub fn quiesce_virtio() -> bool {
        EFFECTS.with_borrow_mut(|effects| effects.push("reset"));
        QUIET.get()
    }
    fn send_frame(_: u64, opcode: proto::Opcode, generation: u64, _: &[u8]) -> bool {
        assert_eq!(opcode, proto::Opcode::Stopped);
        assert_eq!(generation, 1);
        EFFECTS.with_borrow_mut(|effects| effects.push("stopped"));
        true
    }
    COMMON_FUNCTIONS
}
'''

TESTS = r'''
#[cfg(test)] mod tests {
    use super::*;
    fn reset(mode: u8) {
        MODE.set(mode); NOW.set(10); SENDS.set(0); DELIVERIES.with_borrow_mut(Vec::clear); ATTEMPTS.with_borrow_mut(Vec::clear); REPOSTS.with_borrow_mut(Vec::clear); RX_READS.set(0); TX_READS.set(0); CONTROL_READS.set(0); WAITS.set(0);
        TX_DONE.set(false); QUIET.set(true);
        FRAMES.with_borrow_mut(VecDeque::clear); EFFECTS.with_borrow_mut(Vec::clear); CLOSED.with_borrow_mut(Vec::clear);
    }
    fn expect_exit(action: impl FnOnce()) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action));
        assert!(result.is_err_and(|error| error.is::<DriverExit>()), "the driver must observe control and exit, not hit the starvation guard");
    }
    fn stopped() {
        let effects = EFFECTS.with_borrow(Clone::clone);
        let reset = effects.iter().position(|&effect| effect == "reset").expect("transport reset");
        assert_eq!(&effects[reset..], &["reset", "quiesced", "stopped"]);
    }
    fn run_pump(mode: u8) {
        reset(mode);
        if mode == 5 { FRAMES.with_borrow_mut(|frames| frames.push_back((b"BYTES".to_vec(), 55))); }
        let device = Virtio { capability: 7 };
        let mut tx = Queue { capability: 7, rx: false };
        let mut rx = Queue { capability: 7, rx: true };
        let mut memory = vec![0u8; MAX_FRAME];
        let receive = vec![0u8; RX_SLOTS as usize * RX_SLOT as usize];
        let mut port = Port { device: &device, irq: 8, tx: &mut tx, virt: memory.as_mut_ptr() as u64, phys: 1, busy: false, pending_bytes: 0 };
        expect_exit(|| unsafe { pump(&device, &common::Bind { generation: 1 }, 8, 9, 10, &mut rx, &mut port, receive.as_ptr() as u64, &[1; RX_SLOTS as usize]) });
        stopped();
    }
    #[test] fn receive_backpressure_services_ping_and_stop() {
        run_pump(10);
        assert_eq!(WAITS.get(), 3);
        assert_eq!(SENDS.get(), 3);
        assert_eq!(EFFECTS.with_borrow(Clone::clone), ["pong", "reset", "quiesced", "stopped"]);
        assert!(DELIVERIES.with_borrow(Vec::is_empty));
        assert!(REPOSTS.with_borrow(Vec::is_empty), "STOP cancels delivery without reposting held RX");
    }
    #[test] fn failure_notice_backpressure_services_ping_and_stop() {
        run_pump(11);
        assert_eq!(WAITS.get(), 3);
        assert!(ATTEMPTS.with_borrow(|attempts| attempts.len() == 3 && attempts.iter().all(Vec::is_empty)));
        assert!(EFFECTS.with_borrow(|effects| effects.contains(&"pong")));
        assert!(DELIVERIES.with_borrow(Vec::is_empty));
    }
    #[test] fn capacity_recovery_delivers_once_in_payload_order() {
        reset(12);
        let mut pending = 0;
        for payload in [b"first".as_slice(), b"second", b"third"] {
            assert!(unsafe { send_to_agent(&common::Bind { generation: 1 }, 9, 7, &mut pending, 10, payload) });
        }
        assert_eq!(WAITS.get(), 3);
        assert_eq!(DELIVERIES.with_borrow(Clone::clone), [b"first".to_vec(), b"second".to_vec(), b"third".to_vec()]);
        assert_eq!(ATTEMPTS.with_borrow(|attempts| attempts[..4].to_vec()), vec![b"first".to_vec(); 4]);
        assert_eq!(EFFECTS.with_borrow(Clone::clone), ["pong"]);
    }
    #[test] fn bootstrap_closure_and_invalid_wait_terminate_without_adoption() {
        for mode in [13, 15] {
            reset(mode);
            expect_exit(|| { unsafe { send_to_agent(&common::Bind { generation: 1 }, 9, 7, &mut 0, 10, b"held"); } });
            assert!(CLOSED.with_borrow(Vec::is_empty));
            assert!(DELIVERIES.with_borrow(Vec::is_empty));
        }
    }
    #[test] fn stalled_handoff_discards_pending_payload_and_old_receive_pool() {
        run_pump(14);
        assert_eq!(CLOSED.with_borrow(Clone::clone), [10]);
        assert_eq!(SENDS.get(), 3, "old payload is not retried after replacement handoff");
        assert!(DELIVERIES.with_borrow(Vec::is_empty));
        assert_eq!(REPOSTS.with_borrow(Clone::clone), [1, 0], "queued old pool discarded before held descriptor returns");
    }
    #[test] fn terminal_send_failure_adopts_without_replay() {
        run_pump(16);
        assert_eq!(CLOSED.with_borrow(Clone::clone), [10]);
        assert_eq!(SENDS.get(), 1);
        assert!(DELIVERIES.with_borrow(Vec::is_empty));
        assert_eq!(REPOSTS.with_borrow(Clone::clone), [1, 0]);
    }
    #[test] fn stalled_stop_refuses_failed_quiescence() {
        reset(10); QUIET.set(false);
        expect_exit(|| { unsafe { send_to_agent(&common::Bind { generation: 1 }, 9, 7, &mut 0, 10, b"held"); } });
        assert_eq!(EFFECTS.with_borrow(Clone::clone), ["pong", "reset"]);
    }
    #[test] fn empty_receive_completions_still_yield_to_control() { run_pump(7); assert_eq!(RX_READS.get(), RX_SLOTS as usize); }
    #[test] fn replenished_receive_still_services_control() { run_pump(1); assert_eq!(RX_READS.get(), RX_SLOTS as usize); }
    #[test] fn replenished_agent_returns_to_receive_and_control() { run_pump(2); assert!(RX_READS.get() >= 2); assert!(TX_READS.get() <= 2 * RX_SLOTS as usize); }
    #[test] fn stalled_transmit_services_bootstrap_before_buffer_reuse() {
        reset(3);
        let device = Virtio { capability: 7 };
        let mut tx = Queue { capability: 7, rx: false };
        let mut memory = vec![0u8; MAX_FRAME];
        let mut port = Port { device: &device, irq: 8, tx: &mut tx, virt: memory.as_mut_ptr() as u64, phys: 1, busy: true, pending_bytes: 0 };
        expect_exit(|| { unsafe { port.write(b"pending", &common::Bind { generation: 1 }, 9); } });
        assert_eq!(WAITS.get(), 1);
        assert_eq!(EFFECTS.with_borrow(Clone::clone), ["pong", "reset", "quiesced", "stopped"]);
        assert!(port.busy, "an uncompleted descriptor was not reused");
    }
    #[test] fn adoption_stop_uses_the_latched_quiescence_path() {
        for quiet in [true, false] {
            reset(4); QUIET.set(quiet);
            FRAMES.with_borrow_mut(|frames| frames.push_back((frame(driver_protocol::Opcode::Stop, 1), 0)));
            let device = Virtio { capability: 7 };
            let mut rx = Queue { capability: 7, rx: true };
            expect_exit(|| { unsafe { adopt(&device, &common::Bind { generation: 1 }, 8, 9, 10, &mut 0, &mut rx, &[1; RX_SLOTS as usize]); } });
            if quiet { stopped(); } else { assert_eq!(EFFECTS.with_borrow(Clone::clone), ["close", "reset"]); }
        }
    }
    #[test] fn stale_stop_preserves_the_replacement_bytes_handoff() {
        reset(0);
        FRAMES.with_borrow_mut(|frames| {
            frames.push_back((frame(driver_protocol::Opcode::Stop, 0), 0));
            frames.push_back((b"BYTES".to_vec(), 55));
        });
        let device = Virtio { capability: 7 };
        let mut rx = Queue { capability: 7, rx: true };
        assert_eq!(unsafe { adopt(&device, &common::Bind { generation: 1 }, 8, 9, 10, &mut 0, &mut rx, &[1; RX_SLOTS as usize]) }, 55);
        assert_eq!(EFFECTS.with_borrow(Clone::clone), ["close"]);
    }
    #[test] fn ready_handoff_survives_control_before_old_channel_closure() {
        run_pump(5);
        assert!(EFFECTS.with_borrow(|effects| effects.contains(&"replacement")));
        assert_eq!(CLOSED.with_borrow(Clone::clone), [10]);
    }
    #[test] fn handoff_during_transmit_wait_is_owned_until_adoption() {
        reset(6);
        let device = Virtio { capability: 7 };
        let mut tx = Queue { capability: 7, rx: false };
        let mut rx = Queue { capability: 7, rx: true };
        let mut memory = vec![0u8; MAX_FRAME];
        let mut port = Port { device: &device, irq: 8, tx: &mut tx, virt: memory.as_mut_ptr() as u64, phys: 1, busy: true, pending_bytes: 0 };
        assert!(unsafe { port.write(b"pending", &common::Bind { generation: 1 }, 9) });
        assert_eq!(port.pending_bytes, 55);
        assert!(CLOSED.with_borrow(Vec::is_empty));
        assert_eq!(unsafe { adopt(&device, &common::Bind { generation: 1 }, 8, 9, 10, &mut port.pending_bytes, &mut rx, &[1; RX_SLOTS as usize]) }, 55);
        assert_eq!(port.pending_bytes, 0);
        assert_eq!(CLOSED.with_borrow(Clone::clone), [10]);
    }
    #[test] fn pending_handoff_discards_the_previous_receive_pool() {
        reset(4);
        let device = Virtio { capability: 7 };
        let mut rx = Queue { capability: 7, rx: true };
        let mut pending = 55;
        assert_eq!(unsafe { adopt(&device, &common::Bind { generation: 1 }, 8, 9, 10, &mut pending, &mut rx, &[1; RX_SLOTS as usize]) }, 55);
        assert_eq!(RX_READS.get(), RX_SLOTS as usize, "old-session bytes are discarded before the retained handoff");
        assert_eq!(pending, 0);
        assert_eq!(CLOSED.with_borrow(Clone::clone), [10]);
    }
}
'''


def fixture(source: str, common: str) -> str:
    functions = ["struct Port<'a>", "impl Port<'_>", "unsafe fn heartbeat(", "unsafe fn send_to_agent(", "unsafe fn pump(", "unsafe fn adopt("]
    constants = '\n'.join(line for line in source.splitlines() if line.startswith(('const RX_', 'const MAX_FRAME:', 'const TX_DRAIN_TICKS:')))
    common_functions = '\n'.join(item(common, start) for start in ['pub fn latch_stop(', 'pub unsafe fn finish_stop(', 'pub unsafe fn stopped('])
    runtime = (ROOT / 'src/user/runtime/rt/src/lib.rs').read_text()
    wrappers = '\n'.join(item(runtime, start) for start in ['pub enum SendOutcome {', 'pub unsafe fn try_send_outcome(', 'pub unsafe fn wait_any_periodic(', 'pub unsafe fn send_blocking(', 'pub unsafe fn wait_writable('])
    return FIXTURE.replace('RUNTIME_FUNCTIONS', wrappers).replace('COMMON_FUNCTIONS', common_functions) + constants + '\n' + '\n'.join(item(source, start) for start in functions) + TESTS


def main() -> None:
    source = (ROOT / 'src/user/drivers/core/src/dev_channel.rs').read_text()
    common = (ROOT / 'src/user/drivers/core/src/common.rs').read_text()
    control = '\t\t\tif !heartbeat(bind, bootstrap, rx.capability, &mut port.pending_bytes) {\n\t\t\t\texit();\n\t\t\t}'
    rx = 'for _ in 0..RX_SLOTS {\n\t\t\t\tlet Some((id, len)) = rx.take_used() else { break };'
    tx = 'for _ in 0..RX_SLOTS {\n\t\t\t\tmatch try_recv(bytes, &mut outbound) {'
    wait = 'wait_any(&[self.irq, bootstrap], limit)'
    tx_control = 'if !heartbeat(bind, bootstrap, self.device.capability, &mut self.pending_bytes) {\n\t\t\t\t\texit();\n\t\t\t\t}'
    adopt_rx = 'for _ in 0..RX_SLOTS {\n\t\t\t\tlet Some((id, _)) = rx.take_used() else { break };'
    handoff = '*pending_bytes = handle;'
    adopt_stop = 'driver_protocol::Opcode::Stop => {\n\t\t\t\t\t\t\t\t\tcommon::latch_stop();\n\t\t\t\t\t\t\t\t\tcommon::finish_stop(bootstrap, bind, rx.capability, common::quiesce_virtio());\n\t\t\t\t\t\t\t\t\texit();\n\t\t\t\t\t\t\t\t}'
    for needle in [control, rx, tx, wait, tx_control, adopt_stop, adopt_rx, handoff]:
        assert source.count(needle) == 1, needle
    send_control = 'if !heartbeat(bind, bootstrap, capability, pending_bytes) {\n\t\t\t\texit();\n\t\t\t}'
    send = 'send_to_agent(bind, bootstrap, rx.capability, &mut port.pending_bytes, bytes, chunk)'
    notice = 'send_to_agent(bind, bootstrap, rx.capability, &mut port.pending_bytes, bytes, &[])'
    periodic = 'wait_any_periodic(&[bootstrap], clock().saturating_add(1))'
    assert source.count(send_control) == 1 and source.count(periodic) == 1
    variants = [
        ('receive uses blocking send', source.replace(send, 'send_blocking(bytes, chunk, 0)', 1), 'receive_backpressure_services_ping_and_stop'),
        ('failure notice uses blocking send', source.replace(notice, 'send_blocking(bytes, &[], 0)', 1), 'failure_notice_backpressure_services_ping_and_stop'),
        ('send retries omit control', source.replace(send_control, '', 1), 'receive_backpressure_services_ping_and_stop'),
        ('send retry spins without wait', source.replace(periodic, '0', 1), 'receive_backpressure_services_ping_and_stop'),
        ('periodic timeout ends the session', source.replace('ready < 0 && ready != ERR_TIMED_OUT', 'ready < 0', 1), 'capacity_recovery_delivers_once_in_payload_order'),
        ('send retry waits forever', source.replace(periodic, 'wait_any_periodic(&[bootstrap], 0)', 1), 'capacity_recovery_delivers_once_in_payload_order'),
        ('production', source, None),
        ('control only after idle work', source.replace(control, '', 1), 'empty_receive_completions_still_yield_to_control'),
        ('unbounded receive pool', source.replace(rx, 'while let Some((id, len)) = rx.take_used() {', 1), 'empty_receive_completions_still_yield_to_control'),
        ('unbounded agent intake', source.replace(tx, 'loop {\n\t\t\t\tmatch try_recv(bytes, &mut outbound) {', 1), 'replenished_agent_returns_to_receive_and_control'),
        ('TX wait omits bootstrap', source.replace(wait, 'wait_any(&[self.irq], limit)', 1), 'stalled_transmit_services_bootstrap_before_buffer_reuse'),
        ('TX wait ignores control', source.replace(tx_control, '', 1), 'stalled_transmit_services_bootstrap_before_buffer_reuse'),
        ('adoption ignores STOP', source.replace(adopt_stop, 'driver_protocol::Opcode::Stop => {}', 1), 'adoption_stop_uses_the_latched_quiescence_path'),
        ('adoption STOP is not latched', source.replace(adopt_stop, adopt_stop.replace('common::latch_stop();', ''), 1), 'adoption_stop_uses_the_latched_quiescence_path'),
        ('unbounded adoption receive pool', source.replace(adopt_rx, 'while let Some((id, _)) = rx.take_used() {', 1), 'adoption_stop_uses_the_latched_quiescence_path'),
        ('early control discards handoff', source.replace(handoff, 'close(handle);', 1), 'ready_handoff_survives_control_before_old_channel_closure'),
        ('TX control discards handoff', source.replace(handoff, 'close(handle);', 1), 'handoff_during_transmit_wait_is_owned_until_adoption'),
        ('retained handoff bypasses old receive discard', source.replace('close(dead);', 'close(dead); if *pending_bytes != 0 { return core::mem::take(pending_bytes); }', 1), 'pending_handoff_discards_the_previous_receive_pool'),
    ]
    with tempfile.TemporaryDirectory(prefix='dev-channel-control-') as directory:
        root = Path(directory)
        (root / 'src').mkdir()
        (root / 'Cargo.toml').write_text('[package]\nname="dev-channel-control"\nversion="0.1.0"\nedition="2024"\n[dependencies]\ndriver-protocol={path="' + str(ROOT / 'src/user/libs/driver/protocol') + '"}\nabi={path="' + str(ROOT / 'src/abi') + '"}\n')
        for name, variant, test in variants:
            (root / 'src/lib.rs').write_text(fixture(variant, common))
            command = ['cargo', 'test', '--offline', '--manifest-path', str(root / 'Cargo.toml'), '--target-dir', str(root / 'target'), '--lib']
            if test:
                command.append(test)
            command.extend(['--', '--test-threads=1'])
            result = subprocess.run(command, capture_output=True, text=True, timeout=60)
            if test:
                if result.returncode != 101 or f'tests::{test} ... FAILED' not in result.stdout:
                    raise SystemExit(f'{name}: expected the named assertion failure:\n{result.stdout}\n{result.stderr}')
            elif result.returncode:
                raise SystemExit(f'{name}: {result.stdout}\n{result.stderr}')
            print(f'dev-channel-control: {name} {"rejected" if test else "passed (16 tests)"}')


if __name__ == '__main__':
    main()
