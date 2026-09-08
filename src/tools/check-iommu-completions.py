#!/usr/bin/env python3
"""Exercise production IOMMU queue ownership and teardown accounting on host memory."""
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / 'src/kernel/iommu/mod.rs'
QUEUE = ROOT / 'src/kernel/iommu/virtqueue.rs'


def function(source, name):
    match = re.search(r'^(?:pub )?fn ' + name + r'\b.*?^}', source, re.M | re.S)
    if match is None:
        raise ValueError(f'missing production function {name}')
    return match.group()


def main():
    source = SOURCE.read_text()
    wire = source[source.index('pub struct Wire {'):source.index('\npub struct Controller {')]
    production = '\n'.join(function(source, name) for name in (
        'domain_of', 'retained_domain_of', 'faults_for', 'grants_for',
        'quarantined_grants_for', 'requester_of', 'detach_for_inner', 'poll_faults',
        'poll_faults_attributed', 'poll_faults_attributed_with', 'drain_faults',
        'attribution_trustworthy', 'with'))
    prelude = r'''
extern crate alloc;
use dma::{Fault, Generation};
use dma::virtio_iommu::Transport;
use std::sync::Mutex;
#[macro_export]
macro_rules! serial_println { ($($arg:tt)*) => {{ let _ = format_args!($($arg)*); }} }
mod mem {
    pub fn hhdm_offset() -> u64 { 0 }
    pub mod frame {
        pub const PAGE_SIZE: u64 = 4096;
        pub fn allocate() -> Option<u64> { panic!("host fixture never creates hardware queues") }
    }
}
mod arch { pub mod apic { pub fn ticks() -> u64 { 1 } } }
struct SpinLock<T>(Mutex<T>);
impl<T> SpinLock<T> {
    const fn new(value: T) -> Self { Self(Mutex::new(value)) }
    fn lock(&self) -> std::sync::MutexGuard<'_, T> { self.0.lock().unwrap() }
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
static DOMAINS: SpinLock<Vec<(u32, dma::DomainId)>> = SpinLock::new(Vec::new());
static RETAINED: SpinLock<Vec<Option<dma::DomainId>>> = SpinLock::new(Vec::new());
static RETAINED_FAULTS: SpinLock<Vec<u64>> = SpinLock::new(Vec::new());
static ENDED_FAULTS: SpinLock<Vec<(u64, u64)>> = SpinLock::new(Vec::new());
struct Controller { ledger: dma::Iommu<dma::fake::Fake> }
impl Controller { fn iommu(&mut self) -> &mut dma::Iommu<dma::fake::Fake> { &mut self.ledger } }
#[derive(Clone, Copy)]
pub enum Containment { WhateverFaulted, OnlyLiveBindings }
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
    *DOMAINS.lock() = vec![(0, domain)];
    *RETAINED.lock() = vec![None];
    *RETAINED_FAULTS.lock() = vec![0];
    *ENDED_FAULTS.lock() = vec![(0, 0)];
    domain
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
        program = prelude + 'mod virtqueue {\n' + QUEUE.read_text() + queue_fixture + wire + lifecycle + production
        (path / 'tests.rs').write_text(program)
        result = subprocess.run(['cargo', 'test', '--offline', '--quiet', '--manifest-path', str(path / 'Cargo.toml'), '--lib', '--', '--test-threads=1'], cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if result.returncode or '3 passed' not in result.stdout:
            raise SystemExit(result.stdout)
        print('iommu-completions: 3 production queue and teardown regressions passed')
        mutations = {
            'unresolved queue reuse': program.replace('self.size < 2 || self.request_failed', 'self.size < 2').replace('if !self.requests.request_available()', 'if false'),
            'scratch reuse after timeout': program.replace('if !self.requests.request_available()', 'if false'),
            'double-counted terminal fault': program.replace('\tif let Some(slot) = RETAINED.lock().get_mut(index) {\n\t\t*slot = Some(domain);\n\t}\n', '', 1),
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
