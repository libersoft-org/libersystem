#!/usr/bin/env python3
"""Exercise the production root matcher and role selector without starting a guest."""
from pathlib import Path
import os
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


def main() -> None:
    storage = (ROOT / "src/user/services/storage/src/service.rs").read_text()
    bootstrap = (ROOT / "src/user/services/core/src/service_manager/bootstrap.rs").read_text()
    matcher = item(storage, "unsafe fn mount_by_uuid(")
    selector = item(bootstrap, "let mut take_format =") + ";"
    capture_begin = bootstrap.index("let expected_probes =")
    capture_end = bootstrap.index("// The pinned bootstrap set", capture_begin)
    capture = bootstrap[capture_begin:capture_end]
    receive_begin = bootstrap.index("let report_buf: &mut [u8]")
    receive_end = bootstrap.index("match recv_blocking", receive_begin)
    receive = bootstrap[receive_begin:receive_end]
    validate_begin = bootstrap.index("let text = if name == b\"storage_service\"")
    validate_end = bootstrap.index("// Relay the service", validate_begin)
    validation = bootstrap[validate_begin:validate_end]
    declaration = item(bootstrap, 'if name == b"storage_service" && role.tag == b"BLOCK"')
    block_count_begin = storage.index("let expected = if len >= 33")
    block_count_end = storage.index("let probes =", block_count_begin)
    block_count = storage[block_count_begin:block_count_end]
    live_count_begin = storage.index("let expected = if len == 11")
    live_count_end = storage.index("let probes =", live_count_begin)
    live_count = storage[live_count_begin:live_count_end]
    failure_cleanup = item(bootstrap, 'if name == b"storage_service" && started.0 == State::Failed')
    forwarding = item(bootstrap, 'if let Some((bytes, handle)) = external(role)')
    report_helpers = "\n".join([item(storage, "fn storage_bootstrap_report("), item(bootstrap, "fn classification_report_buffer("), item(bootstrap, "fn parse_classification_report("), item(bootstrap, "unsafe fn serve_root(")])
    fixture = r'''
extern crate alloc;
use std::{marker::PhantomData, sync::atomic::{AtomicBool, AtomicU64, Ordering}, cell::RefCell};
static FAIL_ALLOCATION: AtomicBool = AtomicBool::new(false);
struct Allocator;
unsafe impl std::alloc::GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        if FAIL_ALLOCATION.swap(false, Ordering::Relaxed) { std::ptr::null_mut() }
        else { unsafe { std::alloc::System.alloc(layout) } }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        if FAIL_ALLOCATION.swap(false, Ordering::Relaxed) { std::ptr::null_mut() }
        else { unsafe { std::alloc::System.realloc(ptr, layout, size) } }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) { unsafe { std::alloc::System.dealloc(ptr, layout) } }
}
#[global_allocator] static ALLOCATOR: Allocator = Allocator;
mod abi { pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024; }
mod rt { pub use crate::abi::MAX_MESSAGE_BYTES; }
const RIGHT_SEND: u32 = 1; const RIGHT_RECEIVE: u32 = 2; const RIGHT_WAIT: u32 = 4;
const RIGHT_TRANSFER: u32 = 8; const RIGHT_DUPLICATE: u32 = 16;
fn channel() -> Option<(u64, u64)> { Some((501, 502)) }
fn duplicate(handle: u64, _: u32) -> i64 { assert_eq!(handle, 501); 503 }
const SIG_KILL: u64 = 9;
fn signal(handle: u64, value: u64) { assert_eq!(value, SIG_KILL); assert_eq!(handle, 99); }
fn object_info(handle: u64) -> Option<()> { (handle == 91).then_some(()) }
struct Kept { ends: [[u64; 2]; 1] }
struct Entry { name: &'static [u8] }
static MANIFEST: [Entry; 1] = [Entry { name: b"storage_service" }];
fn index_of(name: &[u8]) -> Option<usize> { (name == b"storage_service").then_some(0) }
thread_local! { static SENT: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) }; }
static REFUSE_TRANSFER: AtomicU64 = AtomicU64::new(0);
fn send_blocking(_: u64, _: &[u8], handle: u64) -> bool {
    if handle == REFUSE_TRANSFER.load(Ordering::Relaxed) { false }
    else { if handle != 0 { SENT.with_borrow_mut(|sent| sent.push(handle)); } true }
}
fn forward(role: &Role, handle: u64, probes: Vec<u64>) -> bool {
    let manager_side = 90; let index = 0;
    let mut external = |_: &Role| Some((role.tag.to_vec(), handle));
    let mut pending = Some(probes);
    let mut follow = |_: &Role| pending.take().unwrap();
    for role in [role] { FORWARDING }
    true
}
fn cleanup_failure(spawned: bool) {
    let name = b"storage_service"; let started = (State::Failed, Reason::BootstrapRefused);
    let proc_out = &mut if spawned { 99 } else { 0 }; let service_side = 91; let manager_side = 90;
    let control = &mut 90; let block_formats = &mut vec![4]; let probe_blocks = &mut vec![21, 0, 22];
    let role_blocks = &mut vec![31, 32, 0]; let block_client = &mut 0;
    let kept = &mut Kept { ends: [[41, 42]] }; let storage_client = &mut 41; let storage_admin = &mut 42;
    FAILURE_CLEANUP
    assert_eq!((*proc_out, *control, *storage_client, *storage_admin), (0, 0, 0, 0));
    assert!(block_formats.is_empty());
    assert_eq!(kept.ends, [[0, 0]]);
    CLOSED.with_borrow(|closed| {
        let mut got = closed.clone(); got.sort();
        let mut expected = vec![21, 22, 31, 32, 41, 42, 90, if spawned { 99 } else { 91 }]; expected.sort();
        assert_eq!(got, expected);
    });
}
static ROOT_HEAD: AtomicU64 = AtomicU64::new(1);
static ROOT_UUID_LOW: AtomicU64 = AtomicU64::new(0);
static ROOT_UUID_HIGH: AtomicU64 = AtomicU64::new(0);
struct Role { tag: &'static [u8] }
mod wire_bootstrap {
    use super::*;
    pub fn declaration(probe_count: u32, live_volume: u64) -> Option<(Vec<u8>, u64)> {
        let name = b"storage_service"; let role = Role { tag: b"BLOCK" }; let block = 0;
        DECLARATION
        None
    }
}
fn announced(buf: &[u8], embedded: bool) -> usize {
    let len = buf.len();
    if embedded { LIVE_COUNT expected } else { BLOCK_COUNT expected }
}
#[derive(Debug, PartialEq)] enum State { Ready, Failed }
#[derive(Debug, PartialEq)] enum Reason { ReportedReady, BootstrapRefused }
thread_local! { static CLOSED: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) }; }
fn close(handle: u64) { CLOSED.with_borrow_mut(|closed| {
    assert!(!closed.contains(&handle), "candidate closed twice: {handle}"); closed.push(handle);
}); }
fn consume_report(report: &[u8], name: &[u8], probe_blocks: &mut Vec<u64>, role_blocks: &mut Vec<u64>, block_formats: &mut Vec<u8>, failure_out: &mut String) -> (State, Reason) {
    let mut scratch = [0u8; 256]; let buf = &mut scratch[..];
    CAPTURE
    let mut probe_handles = if name == b"storage_service" { core::mem::take(probe_blocks) } else { Vec::new() };
    let probe_count = expected_probes as u32;
    // Real count declaration and decoding on both backing paths, after the vector moves.
    for embedded in [false, true] {
        let (message, _) = wire_bootstrap::declaration(probe_count, u64::from(embedded)).unwrap();
        assert_eq!(announced(&message, embedded), probe_handles.len());
    }
    probe_handles.clear();
    RECEIVE
    // SYS_CHANNEL_RECEIVE copies min(message length, receive capacity).
    let len = report.len().min(report_buf.len()); report_buf[..len].copy_from_slice(&report[..len]);
    let handle = 0;
    VALIDATION
    assert_eq!(&report_buf[..text], b"StorageService: online (vol://system)");
    (State::Ready, Reason::ReportedReady)
}
struct ChannelBlockDevice;
struct LiberFs<T> { uuid: [u8; 16], device: PhantomData<T> }
impl<T> LiberFs<T> { fn uuid(&self) -> [u8; 16] { self.uuid } }
#[derive(Clone, Copy)]
enum RootMountError { Missing, Ambiguous }
unsafe fn mount_system_volume(channel: u64) -> Option<LiberFs<ChannelBlockDevice>> {
    let id = match channel { 11 | 13 => 1, 12 | 22 => 2, _ => return None };
    Some(LiberFs { uuid: [id; 16], device: PhantomData })
}
'''.replace("DECLARATION", declaration).replace("LIVE_COUNT", live_count).replace("BLOCK_COUNT", block_count).replace("CAPTURE", capture).replace("    RECEIVE\n", receive + "\n").replace("VALIDATION", validation).replace("FAILURE_CLEANUP", failure_cleanup).replace("FORWARDING", forwarding)
    checks = r'''
fn roles(mut role_blocks: Vec<u64>, block_formats: Vec<u8>) -> [u64; 3] {
    SELECTOR
    [take_format(4), take_format(2), take_format(3)]
}
fn check_reports() {
    for failure in [true, false] {
        CLOSED.with_borrow_mut(Vec::clear); SENT.with_borrow_mut(Vec::clear);
        REFUSE_TRANSFER.store(if failure { 503 } else { 0 }, Ordering::Relaxed);
        let mut client = 0;
        assert_eq!(unsafe { serve_root(90, b"SERVE", false, &mut client) }, !failure);
        if failure { assert_eq!(client, 0); CLOSED.with_borrow(|closed| assert_eq!(closed, &[501, 503, 502])); }
        else { assert_eq!(client, 502); CLOSED.with_borrow(|closed| assert_eq!(closed, &[501])); }
    }
    for spawned in [false, true] { CLOSED.with_borrow_mut(Vec::clear); cleanup_failure(spawned); }
    for failed in [100, 201, 202, 203] {
        CLOSED.with_borrow_mut(Vec::clear); SENT.with_borrow_mut(Vec::clear);
        REFUSE_TRANSFER.store(failed, Ordering::Relaxed);
        assert!(!forward(&Role { tag: b"BLOCK" }, 100, vec![201, 202, 203]));
        CLOSED.with_borrow(|closed| SENT.with_borrow(|sent| {
            assert!(closed.iter().all(|handle| !sent.contains(handle)), "transferred handles cannot be closed by sender");
            if failed == 100 { assert_eq!(closed, &[100]); assert!(sent.is_empty()); }
            else { assert_eq!(closed, &(failed..=203).collect::<Vec<_>>()); assert_eq!(sent[0], 100); }
        }));
    }
    REFUSE_TRANSFER.store(0, Ordering::Relaxed);
    for embedded in [false, true] {
        for count in [0, 1, 218, 219, 220, 256] {
            CLOSED.with_borrow_mut(Vec::clear);
            let mut formats = vec![0; count];
            if count > 0 { formats[count - 1] = 4; }
            if count > 1 { formats[count - 2] = 2; formats[count - 3] = 3; }
            let (declaration, _) = wire_bootstrap::declaration(count as u32, u64::from(embedded)).unwrap();
            assert_eq!(announced(&declaration, embedded), count);
            let report = storage_bootstrap_report(b"system", false, &formats).unwrap();
            let mut probes = (1..=count as u64).collect::<Vec<_>>();
            let mut candidates = probes.clone(); let mut table = vec![255]; let mut reason = String::new();
            assert_eq!(consume_report(&report, b"storage_service", &mut probes, &mut candidates, &mut table, &mut reason).0, State::Ready, "{reason}");
            assert_eq!(table, formats); assert!(probes.is_empty());
            if count > 1 {
                let picked = roles(candidates, table);
                assert_eq!(picked, [count as u64, count as u64 - 1, count as u64 - 2]);
                CLOSED.with_borrow(|closed| assert!(picked.iter().all(|h| !closed.contains(h))));
            } else if count == 1 { assert_eq!(roles(candidates, table), [1, 0, 0]); }
        }
    }
    let good = storage_bootstrap_report(b"system", false, &[4, 0, 3]).unwrap();
    let text = good.iter().position(|b| *b == 0).unwrap();
    let mut surplus = good.clone(); surplus.push(2);
    let mut invalid = good.clone(); *invalid.last_mut().unwrap() = 5;
    for report in [&good[..text], &good[..text + 1], &good[..good.len() - 1], &surplus[..], &invalid[..]] {
        CLOSED.with_borrow_mut(Vec::clear);
        let mut probes = vec![11, 12, 13]; let mut roles = vec![11, 12, 13]; let mut table = vec![4, 2, 3]; let mut reason = String::new();
        assert_eq!(consume_report(report, b"storage_service", &mut probes, &mut roles, &mut table, &mut reason).0, State::Failed);
        assert!(table.is_empty(), "previous attempt cannot survive malformed data");
        assert_eq!(roles, [11, 12, 13], "invalid tables cannot select/close candidates");
        CLOSED.with_borrow(|closed| assert!(closed.is_empty()));
        assert!(reason.contains("classification report"));
    }
    assert!(classification_report_buffer(usize::MAX).is_err());
    assert!(classification_report_buffer(abi::MAX_MESSAGE_BYTES).is_err());
    assert!(parse_classification_report(b"StorageService: online (vol://system)", 0).is_ok());
    assert!(parse_classification_report(b"StorageService: online (vol://system)\0", 0).is_ok());
    assert!(parse_classification_report(&good, 0).is_err());
    FAIL_ALLOCATION.store(true, Ordering::Relaxed);
    assert!(classification_report_buffer(219).is_err());
    FAIL_ALLOCATION.store(true, Ordering::Relaxed);
    assert!(parse_classification_report(&good, 3).is_err());
    FAIL_ALLOCATION.store(true, Ordering::Relaxed);
    assert!(storage_bootstrap_report(b"system", false, &[4]).is_err());
}
fn main() {
    check_reports();
    // Provider zero can be FAT while the loader-selected root is a later provider.
    assert_eq!(roles(vec![100, 200, 300, 400], vec![4, 1, 2, 3]), [100, 300, 400]);
    // Unknown media must not hide a fifth provider or be assigned by position.
    assert_eq!(roles(vec![100, 200, 300, 400, 500], vec![1, 0, 4, 2, 3]), [300, 400, 500]);
    // Embedded and block roots use the same table: the adverse media order still resolves.
    assert_eq!(roles(vec![100, 200, 300, 400], vec![1, 3, 2, 4]), [400, 300, 200]);
    assert_eq!(roles(vec![100, 200], vec![1, 0]), [0, 0, 0]);
    unsafe {
        assert_eq!(mount_by_uuid(11, &[11, 12], Some([2; 16])).ok().unwrap().1, 12);
        assert_eq!(mount_by_uuid(11, &[0, 0, 0, 0, 12], Some([2; 16])).ok().unwrap().1, 12);
        // The primary is already represented in probes; it must not count twice.
        assert_eq!(mount_by_uuid(11, &[11, 12], Some([1; 16])).ok().unwrap().1, 11);
        assert!(matches!(mount_by_uuid(11, &[11, 13], Some([1; 16])), Err(RootMountError::Ambiguous)));
        assert!(matches!(mount_by_uuid(11, &[11, 12], Some([3; 16])), Err(RootMountError::Missing)));
        assert_eq!(mount_by_uuid(11, &[], Some([1; 16])).ok().unwrap().1, 11);
    }
}
'''.replace("SELECTOR", selector)
    source = fixture + matcher + report_helpers + checks
    rustc = os.environ.get("RUSTC", "rustc")
    with tempfile.TemporaryDirectory(prefix="provider-routing-") as folder:
        rust = Path(folder) / "main.rs"
        binary = Path(folder) / "routing"
        for label, body, should_pass in [
            ("production", source, True),
            ("duplicate-identity mutation", source.replace("if selected.is_some()", "if false"), False),
            ("provider-zero mutation", source.replace("role_blocks.get_mut(at)", "role_blocks.get_mut(at.max(1))"), False),
            ("fixed-report-buffer mutation", source.replace('if name == b"storage_service" { &mut system_report } else { buf }', 'buf'), False),
            ("lost-serve-root mutation", source.replace("close(narrowed as u64);", ""), False),
            ("lost-probe-tail mutation", re.sub(r"if unsent != 0 \{\s*close\(unsent\);\s*\}", "", source), False),
            ("incomplete-format-validation mutation", source.replace('if formats.len() != expected || formats.iter().any(|format| *format > 4)', 'if false'), False),
        ]:
            rust.write_text(body)
            built = subprocess.run([rustc, "--edition=2024", str(rust), "-o", str(binary)], capture_output=True, text=True)
            if built.returncode:
                raise SystemExit(f"provider-routing: {label} did not compile:\n{built.stderr}")
            ran = subprocess.run([str(binary)], capture_output=True, text=True)
            if (ran.returncode == 0) != should_pass:
                raise SystemExit(f"provider-routing: {label} gave the wrong verdict:\n{ran.stderr}")
    print("provider-routing: non-first and fifth roots, all media roles, missing/ambiguous identities passed; complete BLOCK/LIVEVOL reports at 0/1/218/219/220/256 probes, malformed tables and allocation failures passed; six regression mutations failed")


if __name__ == "__main__":
    main()
