#!/usr/bin/env python3
"""Exercise the serial port's transmit wait, its receive pool and its stream backpressure.

WHAT THIS MODELS AND WHY IT MOVED. It used to model `dev_channel.rs`, which on 2026-09-15 stopped
being a program with control paths of its own: the driver is a thirty-line transport now, and every
decision this gate exists to protect - when a stalled transmit gives up, whether the manager's
channel is answered while it waits, how many receive buffers one pass may take, what a stalled
stream send does - moved into `drivers::serial_port`, which BOTH console drivers share. A checker
still pointed at the old file was asserting against a program that no longer exists.

WHAT IT PROTECTS. Each of these is a line whose absence is invisible in every ordinary run and fatal
in the one that matters: a driver that stops answering its supervisor while a consumer is slow is a
driver a watchdog kills for somebody else's slowness; a transmit wait that omits the supervisor's
handle sleeps through the stop it was sent; an unbounded receive loop is a driver a host can hold in
one pass for as long as it keeps writing; a buffer taken and not re-posted is a ring that shrinks by
one every interrupt until the port is deaf.

AND THE ORACLE IS THE PRODUCTION SOURCE, not a copy of it. The functions below are extracted from
`serial_port.rs` at run time and compiled against fakes, so a change to the real file is a change to
what is tested - and each mutation plants one real regression and requires a NAMED test to fail,
which is what keeps the battery from passing because the fixture stopped compiling.
"""
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
#![allow(dead_code, unused_unsafe, unused_variables)]
extern crate alloc;
use alloc::vec::Vec;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

thread_local! {
    static NOW: Cell<u64> = const { Cell::new(0) };
    // How many times the manager's channel was answered, and what it answered.
    static PINGS: Cell<usize> = const { Cell::new(0) };
    static PING_ALIVE: Cell<bool> = const { Cell::new(true) };
    // Every wait this pass made: the handles it named and the deadline it named.
    static WAITS: RefCell<Vec<(Vec<u64>, u64)>> = const { RefCell::new(Vec::new()) };
    static PERIODIC: RefCell<Vec<(Vec<u64>, u64)>> = const { RefCell::new(Vec::new()) };
    // What the stream took, and what it was told each time.
    static DELIVERED: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
    static SEND_SCRIPT: RefCell<VecDeque<u8>> = const { RefCell::new(VecDeque::new()) };
    static CLOSED: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    // The receive ring: what the device hands back, what goes back on it, and how often it is told.
    static USED: RefCell<VecDeque<(u16, u32)>> = const { RefCell::new(VecDeque::new()) };
    static ENDLESS: Cell<bool> = const { Cell::new(false) };
    static POSTED: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
    static NOTIFIES: Cell<usize> = const { Cell::new(0) };
    // The transmit side.
    static SUBMITS: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
    static TX_RETURNS_AT: Cell<usize> = const { Cell::new(usize::MAX) };
    static ISR_READS: Cell<usize> = const { Cell::new(0) };
    static ACKS: Cell<usize> = const { Cell::new(0) };
    static RX_BACKING: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static TX_BACKING: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

const BOOTSTRAP: u64 = 9;
const IRQ: u64 = 8;
const STREAM: u64 = 10;

const ERR_TIMED_OUT: i64 = abi::ERR_TIMED_OUT;
const ERR_PEER_CLOSED: i64 = abi::ERR_PEER_CLOSED;

// THE DEADLINE A STALLED TRANSMIT IS ALLOWED TO REACH, and the count that turns an unbounded one
// into a failing test rather than a hung runner. A mutant that removes the production bound would
// otherwise spin in `Port::write` for ever and the battery would report a timeout instead of the
// assertion it planted.
const WAIT_CEILING: usize = 4096;

fn clock() -> u64 { NOW.get() }

fn close(handle: u64) { CLOSED.with_borrow_mut(|closed| closed.push(handle)); }

fn interrupt_ack(_: u64) { ACKS.set(ACKS.get() + 1); }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Error { Invalid, Closed, Again, Io, Exhausted, Unsupported }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SendOutcome { Delivered, Stalled, Failed }

// The consumer's end, scripted: 0 delivers, 1 stalls, 2 fails. An exhausted script delivers, so a
// test that only cares about the first few answers does not have to spell out the rest.
fn try_send_outcome(handle: u64, payload: &[u8], _: u64) -> SendOutcome {
    assert_eq!(handle, STREAM, "a chunk goes down the stream this session granted and nowhere else");
    match SEND_SCRIPT.with_borrow_mut(|script| script.pop_front()).unwrap_or(0) {
        1 => SendOutcome::Stalled,
        2 => SendOutcome::Failed,
        _ => {
            DELIVERED.with_borrow_mut(|delivered| delivered.push(payload.to_vec()));
            SendOutcome::Delivered
        }
    }
}

fn send_blocking(handle: u64, payload: &[u8], _: u64) -> bool {
    // PRESENT SO A MUTANT CAN REACH IT, and it fails the moment it is: a send with no end is the
    // regression this file's whole first half exists to refuse.
    let _ = (handle, payload);
    panic!("a blocking send on a consumer path parks the driver");
}

fn wait_any(handles: &[u64], deadline: u64) -> i64 {
    WAITS.with_borrow_mut(|waits| waits.push((handles.to_vec(), deadline)));
    assert!(WAITS.with_borrow(Vec::len) < WAIT_CEILING, "the transmit wait never gave up");
    // Time passes, which is what lets a bounded wait reach its deadline and an unbounded one run
    // into the ceiling above.
    NOW.set(NOW.get() + 1);
    0
}

fn wait_any_periodic(handles: &[u64], deadline: u64) -> i64 {
    PERIODIC.with_borrow_mut(|waits| waits.push((handles.to_vec(), deadline)));
    assert!(PERIODIC.with_borrow(Vec::len) < WAIT_CEILING, "the stream retry never gave up");
    NOW.set(deadline);
    ERR_TIMED_OUT
}

// The supervisor's channel, answered from inside both waits. `false` is a binding that has ended.
mod common {
    pub struct Bind { pub generation: u64 }
    pub fn answer_ping(bootstrap: u64, _: &Bind) -> bool {
        assert_eq!(bootstrap, super::BOOTSTRAP);
        super::PINGS.set(super::PINGS.get() + 1);
        super::PING_ALIVE.get()
    }
}

// The device's two queues, as much of them as these paths touch.
struct Virtio;

impl Virtio {
    fn read_isr(&self) -> u32 { ISR_READS.set(ISR_READS.get() + 1); 0 }
}

struct Queue { rx: bool }

impl Queue {
    fn take_used(&mut self) -> Option<(u16, u32)> {
        if self.rx && ENDLESS.get() {
            // A HOST THAT KEEPS WRITING. The production loop is bounded by the pool size and stops;
            // an unbounded one runs until the ceiling below turns it into a failed assertion.
            let taken = POSTED.with_borrow(Vec::len);
            assert!(taken < WAIT_CEILING, "the receive loop is unbounded");
            return Some(((taken % RX_SLOTS as usize) as u16, 4));
        }
        if self.rx {
            return USED.with_borrow_mut(|used| used.pop_front());
        }
        let done = SUBMITS.with_borrow(Vec::len) > 0 && WAITS.with_borrow(Vec::len) >= TX_RETURNS_AT.get();
        if done && TX_RETURNS_AT.get() != usize::MAX { TX_RETURNS_AT.set(usize::MAX); Some((0, 0)) } else { None }
    }

    fn post_recv(&mut self, id: u16, _: u64, _: u32) { POSTED.with_borrow_mut(|posted| posted.push(id)); }

    fn notify(&mut self) { NOTIFIES.set(NOTIFIES.get() + 1); }

    fn submit_async(&mut self, parts: &[(u64, u32, bool)]) -> bool {
        let (_, len, _) = parts[0];
        let bytes = TX_BACKING.with_borrow(|backing| backing[..len as usize].to_vec());
        SUBMITS.with_borrow_mut(|submits| submits.push(bytes));
        true
    }
}

// The one frame encoder these paths use, stubbed to the shape they depend on: a header carrying the
// sequence number, then the bytes.
mod console_stream {
    pub fn receive_frame(seq: u32, item: &super::ConsoleChunk, out: &mut [u8], _: &mut super::Handles) -> Option<usize> {
        let total = 4 + item.bytes.len();
        if total > out.len() { return None }
        out[..4].copy_from_slice(&seq.to_le_bytes());
        out[4..total].copy_from_slice(&item.bytes);
        Some(total)
    }
}

struct ConsoleChunk { bytes: Vec<u8> }

struct Handles;

impl Handles {
    fn new() -> Self { Handles }
}

EXTRACTED

fn reset() {
    NOW.set(0);
    PINGS.set(0);
    PING_ALIVE.set(true);
    WAITS.with_borrow_mut(Vec::clear);
    PERIODIC.with_borrow_mut(Vec::clear);
    DELIVERED.with_borrow_mut(Vec::clear);
    SEND_SCRIPT.with_borrow_mut(VecDeque::clear);
    CLOSED.with_borrow_mut(Vec::clear);
    USED.with_borrow_mut(VecDeque::clear);
    ENDLESS.set(false);
    POSTED.with_borrow_mut(Vec::clear);
    NOTIFIES.set(0);
    SUBMITS.with_borrow_mut(Vec::clear);
    TX_RETURNS_AT.set(usize::MAX);
    ISR_READS.set(0);
    ACKS.set(0);
    RX_BACKING.with_borrow_mut(|backing| { backing.clear(); backing.resize(RX_SLOTS as usize * RX_SLOT as usize, b'x'); });
    TX_BACKING.with_borrow_mut(|backing| { backing.clear(); backing.resize(MAX_WRITE, 0); });
}

fn rx_virt() -> u64 { RX_BACKING.with_borrow(|backing| backing.as_ptr() as u64) }

fn tx_virt() -> u64 { TX_BACKING.with_borrow(|backing| backing.as_ptr() as u64) }

fn attached() -> Attachment {
    let mut state = Attachment::default();
    state.attached = true;
    state.stream = STREAM;
    state
}
'''

TESTS = r'''
#[cfg(test)]
mod tests {
    use super::*;

    const BIND: common::Bind = common::Bind { generation: 1 };

    fn port(device: &Virtio) -> Port<'_> {
        Port { device, irq: IRQ, tx: Queue { rx: false }, virt: tx_virt(), phys: 0x1000, busy: false }
    }

    // A HOST THAT STOPPED READING HOLDS THE BUFFER, AND THE DRIVER GOES ON ANSWERING ITS SUPERVISOR.
    //
    // Three facts in one place because they are the same failure seen from three sides: the wait
    // must name the supervisor's handle, the supervisor must be answered on every pass, and the
    // whole thing must end at a deadline rather than when the host feels like reading.
    #[test]
    fn a_stalled_transmit_answers_control_waits_on_bootstrap_and_gives_up() {
        reset();
        let device = Virtio;
        let mut port = port(&device);
        assert!(unsafe { port.send_now(b"first") }, "the first write takes the buffer");
        let outcome = unsafe { port.write(b"second", &BIND, BOOTSTRAP) };
        assert_eq!(outcome, Err(Error::Again), "a buffer the device never gave back is `again`, not a lie");
        let waits = WAITS.with_borrow(Clone::clone);
        assert!(!waits.is_empty(), "it waited rather than spinning");
        for (handles, deadline) in &waits {
            assert!(handles.contains(&BOOTSTRAP), "every transmit wait names the supervisor's channel");
            assert!(handles.contains(&IRQ), "and the device's own interrupt");
            assert_eq!(*deadline, TX_DRAIN_TICKS, "bounded by the transport deadline and not by the host");
        }
        assert!(PINGS.get() >= waits.len(), "the supervisor is answered on every pass of the wait");
    }

    // AND A BINDING THAT ENDED ENDS THE WRITE. The supervisor answering `false` is a stop or a
    // closed channel, and a driver that kept waiting for a buffer after that is one nothing can
    // reclaim.
    #[test]
    fn a_transmit_wait_ends_when_the_binding_does() {
        reset();
        PING_ALIVE.set(false);
        let device = Virtio;
        let mut port = port(&device);
        assert!(unsafe { port.send_now(b"first") });
        assert_eq!(unsafe { port.write(b"second", &BIND, BOOTSTRAP) }, Err(Error::Closed));
        assert!(WAITS.with_borrow(Vec::is_empty), "it never reached the wait");
    }

    // AND A BUFFER THE DEVICE GIVES BACK IS THE END OF THE WAIT, which is what says the loop is
    // waiting for the completion rather than counting to a number.
    #[test]
    fn a_returned_buffer_completes_the_write() {
        reset();
        TX_RETURNS_AT.set(2);
        let device = Virtio;
        let mut port = port(&device);
        assert!(unsafe { port.send_now(b"first") });
        assert_eq!(unsafe { port.write(b"second", &BIND, BOOTSTRAP) }, Ok(6));
        assert_eq!(SUBMITS.with_borrow(|s| s.len()), 2, "both writes reached the ring");
        assert_eq!(SUBMITS.with_borrow(|s| s[1].clone()), b"second".to_vec());
    }

    // A CONSUMER THAT IS MERELY SLOW IS WAITED FOR, AND THE SUPERVISOR IS ANSWERED WHILE WAITING.
    #[test]
    fn stream_backpressure_answers_control_and_retries_until_capacity() {
        reset();
        let mut state = attached();
        SEND_SCRIPT.with_borrow_mut(|script| { script.push_back(1); script.push_back(1); script.push_back(0); });
        let mut frame = alloc::vec![0u8; 64];
        send_chunk(&BIND, BOOTSTRAP, &mut state, b"hello", &mut frame);
        assert_eq!(DELIVERED.with_borrow(Vec::len), 1, "the chunk went once, after the stall cleared");
        assert_eq!(PERIODIC.with_borrow(Vec::len), 2, "one bounded retry wake per stall");
        for (handles, deadline) in PERIODIC.with_borrow(Clone::clone) {
            assert_eq!(handles, alloc::vec![BOOTSTRAP], "the retry wake is on the supervisor's channel");
            assert!(deadline > 0, "and it is a deadline, not a wait with no end");
        }
        assert!(PINGS.get() >= 3, "the supervisor is answered before every attempt");
    }

    // A TIMED-OUT RETRY IS NOT AN ERROR. It is the deadline this loop asked for arriving, and a
    // driver that treated it as one would drop a chunk every time a consumer was a tick slow.
    #[test]
    fn a_retry_that_times_out_keeps_the_session() {
        reset();
        let mut state = attached();
        SEND_SCRIPT.with_borrow_mut(|script| { for _ in 0..3 { script.push_back(1) } script.push_back(0); });
        let mut frame = alloc::vec![0u8; 64];
        send_chunk(&BIND, BOOTSTRAP, &mut state, b"hello", &mut frame);
        assert_eq!(DELIVERED.with_borrow(Vec::len), 1, "every one of those wakes timed out and the chunk still went");
        assert_eq!(state.stream, STREAM, "and the session is intact");
        assert!(CLOSED.with_borrow(Vec::is_empty));
    }

    // A CONSUMER THAT HAS GONE ENDS THE ATTACHMENT, because there is nobody left to read what the
    // port produces - and the endpoint is given back rather than held for the life of the driver.
    #[test]
    fn a_failed_send_ends_the_attachment() {
        reset();
        let mut state = attached();
        SEND_SCRIPT.with_borrow_mut(|script| script.push_back(2));
        let mut frame = alloc::vec![0u8; 64];
        send_chunk(&BIND, BOOTSTRAP, &mut state, b"hello", &mut frame);
        assert_eq!(state.stream, 0, "the stream is let go");
        assert_eq!(CLOSED.with_borrow(Clone::clone), alloc::vec![STREAM]);
        assert!(DELIVERED.with_borrow(Vec::is_empty));
    }

    // ONE PASS TAKES AT MOST THE POOL, which is what stops a host that keeps writing from holding
    // this driver in one call for as long as it likes.
    #[test]
    fn the_receive_pass_is_bounded_by_the_pool() {
        reset();
        ENDLESS.set(true);
        let mut state = attached();
        let mut rx = Queue { rx: true };
        let phys = [0u64; RX_SLOTS as usize];
        let mut frame = alloc::vec![0u8; RX_SLOT as usize + 32];
        assert!(drain_receive(&BIND, BOOTSTRAP, &mut rx, rx_virt(), &phys, &mut state, &mut frame));
        assert_eq!(POSTED.with_borrow(Vec::len), RX_SLOTS as usize, "one pass takes the pool and stops");
    }

    // AND EVERY BUFFER IT TAKES GOES BACK ON THE RING, once, with the device told once at the end.
    // A ring that loses a buffer per interrupt is a port that goes deaf after eight of them.
    #[test]
    fn every_taken_buffer_is_re_posted_and_the_device_told_once() {
        reset();
        let mut state = attached();
        let mut rx = Queue { rx: true };
        USED.with_borrow_mut(|used| { for id in 0..3u16 { used.push_back((id, 4)) } });
        let phys = [0u64; RX_SLOTS as usize];
        let mut frame = alloc::vec![0u8; RX_SLOT as usize + 32];
        assert!(drain_receive(&BIND, BOOTSTRAP, &mut rx, rx_virt(), &phys, &mut state, &mut frame));
        assert_eq!(POSTED.with_borrow(Clone::clone), alloc::vec![0u16, 1, 2]);
        assert_eq!(NOTIFIES.get(), 1, "the device is told once for the pass and not once per buffer");
        assert_eq!(DELIVERED.with_borrow(Vec::len), 3, "and each one's bytes reached the consumer");
    }

    // A PASS THAT TOOK NOTHING TELLS THE DEVICE NOTHING AND SAYS SO, which is what lets the pump
    // park instead of looping.
    #[test]
    fn an_empty_receive_pass_notifies_nothing() {
        reset();
        let mut state = attached();
        let mut rx = Queue { rx: true };
        let phys = [0u64; RX_SLOTS as usize];
        let mut frame = alloc::vec![0u8; RX_SLOT as usize + 32];
        assert!(!drain_receive(&BIND, BOOTSTRAP, &mut rx, rx_virt(), &phys, &mut state, &mut frame));
        assert_eq!(NOTIFIES.get(), 0);
        assert!(POSTED.with_borrow(Vec::is_empty));
    }

    // BYTES WITH NOBODY TO TAKE THEM ARE DISCARDED AND THE BUFFER IS RECYCLED. Both halves: the
    // bytes belong to a session nobody holds, and eight buffers is all the device has.
    #[test]
    fn bytes_with_no_consumer_are_discarded_and_the_buffer_recycled() {
        reset();
        let mut state = Attachment::default();
        let mut rx = Queue { rx: true };
        USED.with_borrow_mut(|used| { for id in 0..2u16 { used.push_back((id, 4)) } });
        let phys = [0u64; RX_SLOTS as usize];
        let mut frame = alloc::vec![0u8; RX_SLOT as usize + 32];
        assert!(drain_receive(&BIND, BOOTSTRAP, &mut rx, rx_virt(), &phys, &mut state, &mut frame));
        assert!(DELIVERED.with_borrow(Vec::is_empty), "there is nobody to hand them to");
        assert_eq!(POSTED.with_borrow(Clone::clone), alloc::vec![0u16, 1], "and the ring keeps its buffers");
    }
}
'''


def fixture(source: str) -> str:
    constants = '\n'.join(line for line in source.splitlines() if line.startswith(('pub const RX_', 'pub const MAX_WRITE:', 'pub const TX_DRAIN_TICKS:')))
    # `Attachment` IS TAKEN WITH ITS DERIVE, because `reset` calls `Attachment::default()` and a
    # struct extracted from the line below the attribute is one that no longer has it.
    items = ['pub struct Port<'"'"'a>', 'impl Port<'"'"'_>', '#[derive(Default)]\npub struct Attachment', 'impl Attachment', 'fn drain_receive(', 'fn send_chunk(']
    body = '\n'.join(item(source, start) for start in items)
    return FIXTURE.replace('EXTRACTED', constants + '\n' + body) + TESTS


def main() -> None:
    source = (ROOT / 'src/user/drivers/core/src/serial_port.rs').read_text()

    # THE ANCHORS, CHECKED BEFORE ANYTHING IS MUTATED. A mutation that silently matched nothing is a
    # battery reporting that production passed its own tests, which it always does.
    tx_control = 'if !common::answer_ping(bootstrap, bind) {\n\t\t\t\t\treturn Err(Error::Closed);\n\t\t\t\t}'
    tx_bound = 'if clock() >= limit {\n\t\t\t\t\treturn Err(Error::Again);\n\t\t\t\t}'
    tx_wait = 'wait_any(&[self.irq, bootstrap], limit)'
    send_control = 'if !common::answer_ping(bootstrap, bind) {\n\t\t\treturn;\n\t\t}'
    send_wait = 'wait_any_periodic(&[bootstrap], clock().saturating_add(1))'
    send_timeout = 'if ready < 0 && ready != ERR_TIMED_OUT {'
    send_failed = 'SendOutcome::Failed => {\n\t\t\t\tclose(state.stream);\n\t\t\t\tstate.stream = 0;\n\t\t\t\treturn;\n\t\t\t}'
    rx_bound = 'for _ in 0..RX_SLOTS {\n\t\tlet Some((id, len)) = rx.take_used() else { break };'
    # THE ONE INSIDE `drain_receive` AND NOT THE ONE THAT FILLS THE POOL AT OPEN. Both lines are
    # identical; what tells them apart is what follows, and a mutation that hit the wrong one would
    # be testing a path no interrupt ever takes.
    rx_repost = 'rx.post_recv(id, rx_phys[id as usize], RX_SLOT as u32);\n\t\tworked = true;'
    rx_notify = 'if worked {\n\t\trx.notify();\n\t}'
    for needle in [tx_control, tx_bound, tx_wait, send_control, send_wait, send_timeout, send_failed, rx_bound, rx_repost, rx_notify]:
        if source.count(needle) != 1:
            raise SystemExit(f'dev-channel-control: the anchor below is not in `serial_port.rs` exactly once - it was found {source.count(needle)} times:\n{needle}')

    variants = [
        ('production', source, None),
        ('transmit wait ignores control', source.replace(tx_control, '', 1), 'a_stalled_transmit_answers_control_waits_on_bootstrap_and_gives_up'),
        ('transmit wait omits bootstrap', source.replace(tx_wait, 'wait_any(&[self.irq], limit)', 1), 'a_stalled_transmit_answers_control_waits_on_bootstrap_and_gives_up'),
        ('transmit wait is unbounded', source.replace(tx_bound, '', 1), 'a_stalled_transmit_answers_control_waits_on_bootstrap_and_gives_up'),
        ('a stalled write claims success', source.replace('return Err(Error::Again);', 'return Ok(0);', 1), 'a_stalled_transmit_answers_control_waits_on_bootstrap_and_gives_up'),
        ('stream retries omit control', source.replace(send_control, '', 1), 'stream_backpressure_answers_control_and_retries_until_capacity'),
        ('stream retry spins without waiting', source.replace(send_wait, '0', 1), 'stream_backpressure_answers_control_and_retries_until_capacity'),
        ('a retry timeout ends the session', source.replace(send_timeout, 'if ready < 0 {', 1), 'a_retry_that_times_out_keeps_the_session'),
        ('a failed send leaks the stream', source.replace(send_failed, 'SendOutcome::Failed => {\n\t\t\t\tstate.stream = 0;\n\t\t\t\treturn;\n\t\t\t}', 1), 'a_failed_send_ends_the_attachment'),
        ('the receive pass is unbounded', source.replace(rx_bound, 'while let Some((id, len)) = rx.take_used() {', 1), 'the_receive_pass_is_bounded_by_the_pool'),
        ('a taken buffer is not re-posted', source.replace(rx_repost, '', 1), 'every_taken_buffer_is_re_posted_and_the_device_told_once'),
        ('the device is told once per buffer', source.replace(rx_notify, '', 1).replace(rx_repost, rx_repost + '\n\t\trx.notify();', 1), 'every_taken_buffer_is_re_posted_and_the_device_told_once'),
    ]

    with tempfile.TemporaryDirectory(prefix='dev-channel-control-') as directory:
        root = Path(directory)
        (root / 'src').mkdir()
        (root / 'Cargo.toml').write_text('[package]\nname="dev-channel-control"\nversion="0.1.0"\nedition="2024"\n[dependencies]\nabi={path="' + str(ROOT / 'src/abi') + '"}\ndriver-protocol={path="' + str(ROOT / 'src/user/libs/driver/protocol') + '"}\n')
        for name, variant, test in variants:
            (root / 'src/lib.rs').write_text(fixture(variant))
            command = ['cargo', 'test', '--offline', '--manifest-path', str(root / 'Cargo.toml'), '--target-dir', str(root / 'target'), '--lib']
            if test:
                command.append(test)
            command.extend(['--', '--test-threads=1'])
            result = subprocess.run(command, capture_output=True, text=True, timeout=180)
            if test:
                if result.returncode != 101 or f'tests::{test} ... FAILED' not in result.stdout:
                    raise SystemExit(f'dev-channel-control: {name}: expected the named assertion to fail, not a build failure:\n{result.stdout}\n{result.stderr}')
            elif result.returncode:
                raise SystemExit(f'dev-channel-control: {name}:\n{result.stdout}\n{result.stderr}')
            print(f'dev-channel-control: {name} {"rejected" if test else "passed"}')


if __name__ == '__main__':
    main()
