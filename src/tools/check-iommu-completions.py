#!/usr/bin/env python3
"""Exercise production IOMMU queue ownership, teardown and DMA-buffer cleanup on a host."""
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / 'src/kernel/iommu/mod.rs'
QUEUE = ROOT / 'src/kernel/iommu/virtqueue.rs'
BUFFER = ROOT / 'src/kernel/object/dma_buffer/mod.rs'


def function(source, name):
    match = re.search(r'^(?:pub )?fn ' + name + r'\b.*?^}', source, re.M | re.S)
    if match is None:
        raise ValueError(f'missing production function {name}')
    return match.group()


def main():
    source = SOURCE.read_text()
    wire = source[source.index('pub struct Wire {'):source.index('\npub struct Controller {')]
    production = '\n'.join(function(source, name) for name in (
        'attach_for', 'domain_of', 'retained_domain_of', 'faults_for', 'grants_for',
        'quarantined_grants_for', 'requester_of', 'detach_for_inner', 'poll_faults',
        'poll_faults_attributed', 'poll_faults_attributed_with', 'drain_faults',
        'attribution_trustworthy', 'with', 'map_for_device', 'map_device_buffer',
        'domain_for_generation', 'unmap_for_device'))
    prelude = r'''
extern crate alloc;
use dma::{Fault, Generation};
use dma::virtio_iommu::Transport;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
#[macro_export]
macro_rules! serial_println { ($($arg:tt)*) => {{ let _ = format_args!($($arg)*); }} }
mod mem {
    pub fn hhdm_offset() -> u64 { 0 }
    pub mod frame {
        use std::sync::atomic::{AtomicUsize, Ordering};
        pub const PAGE_SIZE: u64 = 4096;
        pub static RETIRED: AtomicUsize = AtomicUsize::new(0);
        pub fn pages_for(size: usize) -> usize { size.max(1).div_ceil(PAGE_SIZE as usize) }
        pub fn allocate_contiguous(_: usize) -> Option<u64> { Some(0x800000) }
        pub fn allocate() -> Option<u64> { panic!("host fixture never creates hardware queues") }
        pub unsafe fn deallocate(_: u64) { panic!("the fixture's successful allocation is never rolled back") }
        pub unsafe fn retire(frames: &[u64]) { RETIRED.fetch_add(frames.len(), Ordering::SeqCst); }
        pub fn note_lost_pages(_: u64) {}
        pub fn lost_pages() -> u64 { 0 }
    }
    pub mod heap { pub fn try_arc<T>(value: T) -> Option<alloc::sync::Arc<T>> { Some(alloc::sync::Arc::new(value)) } }
}
mod arch { pub mod apic { pub fn ticks() -> u64 { 1 } } }
struct SpinLock<T>(Mutex<T>);
impl<T> SpinLock<T> {
    const fn new(value: T) -> Self { Self(Mutex::new(value)) }
    fn lock(&self) -> std::sync::MutexGuard<'_, T> { self.0.lock().unwrap() }
}
trait FixtureConfig { fn config(&self) -> &dma::virtio_iommu::Config; }
impl FixtureConfig for dma::fake::Fake {
    fn config(&self) -> &dma::virtio_iommu::Config {
        &dma::virtio_iommu::Config { page_size_mask: 4096, input_start: 0x1000,
            input_end: 0x100fff, domain_start: 1, domain_end: 100, probe_size: 0, bypass: 0 }
    }
}
struct ObjectHeader;
impl ObjectHeader { fn new() -> Self { Self } }
#[derive(Debug)]
enum MemoryError { OutOfMemory, QuotaExceeded }
struct Domain { charged: AtomicUsize }
impl Domain {
    fn try_charge_dma(&self, bytes: u64) -> bool { self.charged.fetch_add(bytes as usize, Ordering::SeqCst); true }
    fn uncharge_dma(&self, bytes: u64) { self.charged.fetch_sub(bytes as usize, Ordering::SeqCst); }
}
mod iommu {
    pub use super::{map_device_buffer, unmap_for_device};
    pub fn translating() -> bool { true }
}
mod device {
    pub fn binding_of_faulting_endpoint(_: u8, _: u8, _: u8, _: u64) -> Option<(usize, bool)> { Some((0, false)) }
    pub fn contain_faulting_endpoint(_: u8, _: u8, _: u8) -> Option<usize> { None }
    pub fn contain_faulting_endpoint_of_a_live_binding(_: u8, _: u8, _: u8, _: u64) -> Option<usize> { None }
}
'''
    queue_fixture = r'''
    pub fn simulated() -> (VirtQueue, u64, u64) {
        fn page() -> u64 { Box::into_raw(Box::new([0u64; 512])) as u64 }
        let used = page();
        let avail = page();
        (VirtQueue { desc: page(), avail, used, scratch: page(), size: 16,
            avail_index: 0, next_descriptor: 0, used_seen: 0, notify: page(), request_failed: false }, used, avail)
    }
}
use virtqueue::VirtQueue;
'''
    lifecycle = r'''
static CONTROLLER: SpinLock<Option<Controller>> = SpinLock::new(None);
static FAIL_ASSOCIATION_RESERVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
struct DomainRows { rows: Vec<(u32, dma::DomainId)> }
impl std::ops::Deref for DomainRows {
    type Target = Vec<(u32, dma::DomainId)>;
    fn deref(&self) -> &Self::Target { &self.rows }
}
impl std::ops::DerefMut for DomainRows {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.rows }
}
impl DomainRows {
    fn try_reserve(&mut self, additional: usize) -> Result<(), ()> {
        if FAIL_ASSOCIATION_RESERVE.swap(false, Ordering::SeqCst) { return Err(()); }
        self.rows.try_reserve(additional).map_err(|_| ())
    }
}
static DOMAINS: SpinLock<DomainRows> = SpinLock::new(DomainRows { rows: Vec::new() });
static RETAINED: SpinLock<Vec<Option<dma::DomainId>>> = SpinLock::new(Vec::new());
static RETAINED_FAULTS: SpinLock<Vec<u64>> = SpinLock::new(Vec::new());
static ENDED_FAULTS: SpinLock<Vec<(u64, u64)>> = SpinLock::new(Vec::new());
struct Controller { ledger: dma::Iommu<dma::fake::Fake> }
impl Controller { fn iommu(&mut self) -> &mut dma::Iommu<dma::fake::Fake> { &mut self.ledger } }
#[derive(Clone, Copy)]
pub enum Containment { WhateverFaulted, OnlyLiveBindings }
fn attach_endpoint(bus: u8, dev: u8, func: u8, generation: u64) -> Result<dma::DomainId, Fault> {
    with(|controller| {
        let domain = controller.iommu().create_domain(0x1000, 0x100000, Vec::new(), Generation(generation))?;
        controller.iommu().attach(domain, requester_of(bus, dev, func))?;
        Ok(domain)
    }).unwrap()
}
// A real detach queues one report before the final production drain. The hardware side is
// represented by this fixture; the ledger, drain, association, copy and exposed reader are real.
fn revoke_endpoint(domain: dma::DomainId, bus: u8, dev: u8, func: u8) -> Result<dma::Release, Fault> {
    with(|controller| {
        let result = controller.iommu().revoke_endpoint(domain, requester_of(bus, dev, func));
        controller.iommu().backend_mut_for_test().queue_fault(dma::fake::event(7, 0, 0x2000, dma::Access::Write, Fault::NotMapped));
        result
    }).unwrap()
}
fn setup(map_failure: bool) -> dma::DomainId {
    let mut ledger = dma::Iommu::new(dma::fake::Fake::new(), 8);
    let domain = ledger.create_domain(0x1000, 0x100000, Vec::new(), Generation(1)).unwrap();
    ledger.attach(domain, dma::EndpointId(7)).unwrap();
    if map_failure {
        ledger.backend_mut_for_test().inject(dma::fake::Injection::Map, Fault::Unconfirmed);
        let requirements = dma::Requirements::new(64, 4096, 1, true).unwrap();
        assert_eq!(ledger.map(domain, 0x800000, 4096, dma::Direction::Bidirectional, &requirements), Err(Fault::Unconfirmed));
    }
    *CONTROLLER.lock() = Some(Controller { ledger });
    DOMAINS.lock().rows = vec![(0, domain)];
    *RETAINED.lock() = vec![None];
    *RETAINED_FAULTS.lock() = vec![0];
    *ENDED_FAULTS.lock() = vec![(0, 0)];
    domain
}
#[test]
fn association_allocation_failure_precedes_every_hardware_attach() {
    setup(false);
    DOMAINS.lock().clear();
    *CONTROLLER.lock() = Some(Controller { ledger: dma::Iommu::new(dma::fake::Fake::new(), 8) });
    with(|controller| controller.iommu().backend_mut_for_test().inject(dma::fake::Injection::Detach, Fault::Unconfirmed));
    FAIL_ASSOCIATION_RESERVE.store(true, Ordering::SeqCst);
    assert!(!attach_for(0, 0, 0, 7, 1));
    assert_eq!(domain_of(0), None);
    assert_eq!(retained_domain_of(0), None);
    with(|controller| {
        assert_eq!(controller.iommu().backend().attachments(), 0, "allocation refusal never needs an uncertain hardware rollback");
        assert!(controller.iommu().backend().calls().is_empty(), "even domain creation follows the bookkeeping reservation");
    });
    assert!(attach_for(0, 0, 0, 7, 1));
    let domain = domain_of(0).expect("a successful attach publishes its reserved association");
    assert_eq!(with(|controller| controller.iommu().generation_of(domain)), Some(Some(Generation(1))));
    FAIL_ASSOCIATION_RESERVE.store(true, Ordering::SeqCst);
    assert!(attach_for(0, 0, 0, 7, 1), "an existing association needs no allocation or second attach");
    assert!(FAIL_ASSOCIATION_RESERVE.swap(false, Ordering::SeqCst));
    assert_eq!(with(|controller| controller.iommu().backend().attachments()), Some(1));
}
#[test]
fn a_successful_teardowns_last_fault_is_counted_once() {
    setup(false);
    assert!(detach_for_inner(0, 0, 0, 7));
    assert_eq!(RETAINED_FAULTS.lock()[0], 1);
    assert_eq!(ENDED_FAULTS.lock()[0].1, 0);
    assert_eq!(faults_for(0, 1), 1);
    assert_eq!(retained_domain_of(0), None);
    assert_eq!(grants_for(0), 0);
}
#[test]
fn a_failed_maps_quarantine_survives_the_production_teardown() {
    let domain = setup(true);
    assert_eq!(quarantined_grants_for(0), 1);
    assert!(!detach_for_inner(0, 0, 0, 7));
    assert_eq!(retained_domain_of(0), Some(domain));
    assert_eq!(grants_for(0), 1);
    assert_eq!(quarantined_grants_for(0), 1);
    assert_eq!(faults_for(0, 1), 1);
}
#[test]
fn a_late_completion_never_confirms_a_reused_request_or_overwrites_its_scratch() {
    let (requests, used, avail) = virtqueue::simulated();
    let scratch = requests.scratch_virtual();
    let (events, _, _) = virtqueue::simulated();
    let mut wire = Wire { requests, events, event_buffer: 0, event_physical: 0, event_descriptor: None };
    virtqueue::set_spin_budget(0);
    let mut tail = [0u8; 4];
    assert_eq!(wire.request(&[1, 2, 3, 4], &mut tail, 0), Err(Fault::Unconfirmed));
    for _ in 0..7 { assert_eq!(wire.request(&[9; 4], &mut tail, 0), Err(Fault::Unconfirmed)); }
    unsafe {
        (used as *mut u16).add(1).write_volatile(1);
        ((used + 4) as *mut u32).write_volatile(0);
        ((used + 8) as *mut u32).write_volatile(4);
    }
    virtqueue::set_spin_budget(2);
    assert_eq!(wire.request(&[9; 4], &mut tail, 0), Err(Fault::Unconfirmed));
    assert_eq!(unsafe { ((avail + 2) as *const u16).read_volatile() }, 1);
    assert_eq!(unsafe { std::slice::from_raw_parts(scratch as *const u8, 4) }, &[1, 2, 3, 4]);
}
#[test]
fn a_surviving_buffer_reclaims_its_frames_after_confirmed_domain_retirement() {
    for drop_before_detach in [false, true] {
        let isolation = setup(false);
        let domain = alloc::sync::Arc::new(Domain { charged: AtomicUsize::new(0) });
        mem::frame::RETIRED.store(0, Ordering::SeqCst);
        let key = abi::ClaimKey { device_index: 0, _pad: 0, generation: 1 };
        let buffer = buffers::DmaBuffer::create_for(&domain, 4096, Some(key)).unwrap();
        let survivor = buffer.clone();
        let id = buffers::mapping_id(&buffer);
        buffer.mark_orphaned();
        drop(buffer);
        assert_eq!(mem::frame::RETIRED.load(Ordering::SeqCst), 0);
        assert_eq!(domain.charged.load(Ordering::SeqCst), 4096);
        if drop_before_detach {
            drop(survivor);
            assert!(detach_for_inner(0, 0, 0, 7));
        } else {
            assert!(detach_for_inner(0, 0, 0, 7));
            assert!(with(|controller| controller.iommu().generation_of(isolation).is_none()).unwrap(), "the domain really retired before the last reference dropped");
            assert_eq!(buffers::release_for(0), 0);
            assert_eq!(grants_for(0), 0);
            assert_eq!(domain.charged.load(Ordering::SeqCst), 4096);
            drop(survivor);
        }
        assert_eq!(mem::frame::RETIRED.load(Ordering::SeqCst), 1, "confirmed translated frames are retired exactly once in either close order");
        assert_eq!(domain.charged.load(Ordering::SeqCst), 0, "the surviving buffer refunds its DMA quota");
        assert_eq!(buffers::held_frames_for_test(0), 0, "confirmed translations need no later device reset");
        assert!(with(|controller| controller.iommu().mapping(id).is_none()).unwrap(), "the completion row is consumed with the last buffer reference");
        assert_eq!(unmap_for_device(id), Err(Fault::NotMapped), "an unknown id never implies confirmed release");
    }
}
#[test]
fn a_surviving_buffer_keeps_unconfirmed_frames_and_quota_quarantined() {
    setup(false);
    let domain = alloc::sync::Arc::new(Domain { charged: AtomicUsize::new(0) });
    mem::frame::RETIRED.store(0, Ordering::SeqCst);
    let key = abi::ClaimKey { device_index: 0, _pad: 0, generation: 1 };
    let buffer = buffers::DmaBuffer::create_for(&domain, 4096, Some(key)).unwrap();
    buffer.mark_orphaned();
    with(|controller| controller.iommu().backend_mut_for_test().inject(dma::fake::Injection::Unmap, Fault::Unconfirmed));
    assert!(!detach_for_inner(0, 0, 0, 7));
    drop(buffer);
    assert_eq!(mem::frame::RETIRED.load(Ordering::SeqCst), 0);
    assert_eq!(domain.charged.load(Ordering::SeqCst), 4096);
    assert_eq!(quarantined_grants_for(0), 1);
}
'''
    buffer_source = BUFFER.read_text()
    buffer_parts = buffer_source[buffer_source.index('pub struct DmaBuffer {'):buffer_source.index('\nimpl DmaBuffer {')]
    buffer_parts += '\nimpl DmaBuffer {\n' + '\n'.join(
        re.search(r'^\tpub fn ' + name + r'\b.*?^\t}', buffer_source, re.M | re.S).group()
        for name in ('create_for', 'mark_orphaned')) + '\n}\n'
    buffer_parts += buffer_source[buffer_source.index('impl Drop for DmaBuffer {'):buffer_source.index('\n#[cfg(test)]\nmod tests;')]
    buffer_fixture = '''
mod buffers {
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use super::{Domain, MemoryError, ObjectHeader, SpinLock};
    use crate::mem::frame::{self, PAGE_SIZE};
''' + buffer_parts + '''
    pub fn mapping_id(buffer: &DmaBuffer) -> dma::MappingId { buffer.translation.lock().unwrap().0 }
}
'''
    with tempfile.TemporaryDirectory(prefix='liber-iommu-completions-') as directory:
        path = Path(directory)
        # Compile the exact DMA source as a local dependency with its test accessor enabled.
        # The accessor only exposes the fake backend for injection; production logic is unchanged.
        dma_source = (ROOT / 'src/dma/src/lib.rs').read_text().replace('\t#[cfg(test)]\n\tpub fn backend_mut_for_test', '\tpub fn backend_mut_for_test')
        (path / 'dma').mkdir()
        for file in (ROOT / 'src/dma/src').glob('*.rs'):
            (path / 'dma' / file.name).write_text(dma_source if file.name == 'lib.rs' else file.read_text())
        (path / 'dma/Cargo.toml').write_text('[package]\nname="dma"\nedition="2024"\n[lib]\npath="lib.rs"\n')
        (path / 'Cargo.toml').write_text('[package]\nname="iommu-completion-regressions"\nedition="2024"\n[dependencies]\ndma={path="dma"}\nabi={path="' + str(ROOT / 'src/abi') + '"}\n[lib]\npath="tests.rs"\n')
        program = prelude + 'mod virtqueue {\n' + QUEUE.read_text() + queue_fixture + wire + lifecycle + production + buffer_fixture
        (path / 'tests.rs').write_text(program)
        result = subprocess.run(['cargo', 'test', '--offline', '--quiet', '--manifest-path', str(path / 'Cargo.toml'), '--lib', '--', '--test-threads=1'], cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if result.returncode or '6 passed' not in result.stdout:
            raise SystemExit(result.stdout)
        print('iommu-completions: 6 production attach, queue, teardown and buffer regressions passed')
        reservation = re.search(r'\tif domains.try_reserve\(1\).is_err\(\) \{.*?\n\t\}\n', program, re.S).group()
        mutations = {
            'bookkeeping reserved after hardware attach': program.replace(reservation, '').replace('\t\t\tdomains.push((index as u32, domain));', reservation + '\t\t\tdomains.push((index as u32, domain));'),
            'unresolved queue reuse': program.replace('self.size < 2 || self.request_failed', 'self.size < 2').replace('if !self.requests.request_available()', 'if false'),
            'scratch reuse after timeout': program.replace('if !self.requests.request_available()', 'if false'),
            'double-counted terminal fault': program.replace('\tif let Some(slot) = RETAINED.lock().get_mut(index) {\n\t\t*slot = Some(domain);\n\t}\n', '', 1),
            'forgotten buffer completion': program.replace('\t\tiommu.retain_mapping(id)?;\n', ''),
            'confirmed translated frames held after reset': program.replace('(Some(device), true, false) =>', '(Some(device), true, _) =>'),
        }
        for name, mutant in mutations.items():
            if mutant == program:
                raise ValueError(f'{name}: mutation did not change production code')
            (path / 'tests.rs').write_text(mutant)
            result = subprocess.run(['cargo', 'test', '--offline', '--quiet', '--manifest-path', str(path / 'Cargo.toml'), '--lib', '--', '--test-threads=1'], cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            if result.returncode != 101 or 'test result: FAILED' not in result.stdout:
                raise SystemExit(f'{name}: expected an assertion failure:\n{result.stdout}')
            print(f'iommu-completions: rejected {name}')


if __name__ == '__main__':
    main()
