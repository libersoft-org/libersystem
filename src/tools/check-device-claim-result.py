#!/usr/bin/env python3
"""Exercise production claim-error observation, ownership, settlement and retry admission."""
from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path
import os
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
spec = spec_from_file_location("progress", Path(__file__).with_name("check-device-manager-progress.py"))
progress = module_from_spec(spec)
spec.loader.exec_module(progress)
item = progress.item

FIXTURE = r'''
#![allow(dead_code, unused_variables, unused_unsafe)]
extern crate alloc;
use abi::*;
use driver_binding::{BindingId, BindingRecord as ActualBindingRecord, BindingState, BindingQueue, FailureCause, Heartbeat};
use std::{cell::RefCell, collections::VecDeque};
const MAX_AUTOMATIC_ATTEMPTS:u32=3;
const BACKOFF_TICKS:[u64;2]=[10,20];
const STATE_DRIVER_MISSING:u8=1;
#[derive(Default)] struct Runtime {
    snapshots:VecDeque<Option<DeviceClaimSnapshot>>, errno:i64, claims:usize,
    effects:Vec<(&'static str,u64)>, logs:Vec<u8>, granted:Option<driver_binding::Holdings>,
    transitions:Vec<(BindingState,BindingState,bool)>,
}
thread_local! { static RT:RefCell<Runtime> = RefCell::new(Runtime::default()); }
// Observe attempted transitions while retaining the real state machine and record methods.
struct BindingRecord(ActualBindingRecord);
impl BindingRecord {
    fn new()->Self {Self(ActualBindingRecord::new())}
    fn move_to(&mut self,next:BindingState,failure:Option<FailureCause>)->bool {
        let from=self.0.state;
        let accepted=self.0.move_to(next,failure);
        RT.with_borrow_mut(|rt|rt.transitions.push((from,next,accepted)));
        accepted
    }
}
impl std::ops::Deref for BindingRecord {
    type Target=ActualBindingRecord;
    fn deref(&self)->&Self::Target {&self.0}
}
impl std::ops::DerefMut for BindingRecord {
    fn deref_mut(&mut self)->&mut Self::Target {&mut self.0}
}
fn clock()->u64 { 10 }
fn print(bytes:&[u8]) { RT.with_borrow_mut(|rt|rt.logs.extend_from_slice(bytes)); }
fn print_driver_name(bytes:&[u8]) { print(bytes); }
fn signal(handle:u64,_:u64) { RT.with_borrow_mut(|rt|rt.effects.push(("kill",handle))); }
fn close(handle:u64) { RT.with_borrow_mut(|rt|rt.effects.push(("close",handle))); }
fn device_release(handle:u64)->i64 { RT.with_borrow_mut(|rt|rt.effects.push(("release",handle))); CLAIM_STATE_FREE as i64 }
fn domain_kill(handle:u64) { RT.with_borrow_mut(|rt|rt.effects.push(("domain",handle))); }
fn device_claim_snapshot(_:u64,_:u64)->Option<DeviceClaimSnapshot> {
    RT.with_borrow_mut(|rt|rt.snapshots.pop_front().expect("unexpected extra snapshot"))
}
fn device_claim(_:u64,_:u64)->Result<ClaimGrant,i64> { RT.with_borrow_mut(|rt| {
    rt.claims+=1;
    if rt.errno<0 { Err(rt.errno) } else { Ok(ClaimGrant { key:ClaimKey { device_index:0,_pad:0,generation:9 },memory:20,claim:21 }) }
}) }
struct Offers;
impl Offers { fn new()->Self {Self} fn close_all(&mut self) {} }
struct Entry { name:&'static [u8], artifact:&'static [u8], boot_critical:bool }
static ENTRY:Entry=Entry { name:b"fixture", artifact:b"fixture", boot_critical:false };
struct Incident { opened:bool,deadline:u64,teardown_reserve:u64 }
impl Incident { fn open()->Self { Self {opened:true,deadline:1000,teardown_reserve:100} }
    fn allows_backoff(&self,delay:u64)->bool {clock()+delay<self.deadline.saturating_sub(self.teardown_reserve)} }
#[derive(Clone,Copy)] struct Diagnostic;
fn capture(_:&Node,_:FailureCause)->Diagnostic { Diagnostic }
fn report_incident(_:&[u8],_:&Diagnostic) {}
struct Catalogue;
impl Catalogue { fn retire_binding(&mut self,_:BindingId) {} }
fn gate_on_requirements(_:&mut Node,_:&'static Entry,_:&Catalogue)->bool {true}
fn back_off_until(_:&Incident,_:u32)->u64 {panic!("quarantine cannot back off")}
static IMAGE:[u8;1]=[0];
fn read_driver(_:u64,_:&[u8])->Option<(u64,u64,usize)> { Some((77,IMAGE.as_ptr() as u64,1)) }
fn unmap_object(handle:u64) { RT.with_borrow_mut(|rt|rt.effects.push(("unmap",handle))); }
mod proto { pub mod system {
    #[derive(PartialEq,Eq,Debug)] pub enum PolicyOutcome {Accepted,Quarantined,Busy,Refused,NotACandidate}
    pub enum PolicyVerb {Retry,Enable,Disable,Select}
} }
fn candidate_position(_:&Node,_:&[u8])->Option<usize> {None}
PRODUCTION_TYPES
impl Node { NODE_METHODS }
impl Attempt { fn new()->Self {Self {held:driver_binding::Holdings::new(),key:ClaimKey::default()}} BEGIN_TEARDOWN }
struct Syscalls;
PRODUCTION_FUNCTIONS
// Only the claim stage is under test: keep its actual pre-observation, admission, match and
// successful ownership statements. ELF parsing and later channel/spawn effects are outside it.
unsafe fn begin_bind(node:&mut Node,info:&DeviceInfo,elf:&[u8],driver_name:&[u8],key_producer:u64,power:u64,console_input:u64,device_privilege:u64)->BindStart {
    unsafe {
        let teardown_deadline=1000;
        let attempts_left=!node.retry_once && may_try_again(&node.incident,node.attempt);
        CLAIM_STAGE
        RT.with_borrow_mut(|rt|rt.granted=Some(txn.held));
        BindStart::Opened
    }
}
// This is the actual post-queue settlement branch from advance, including its Step result.
unsafe fn settle_node(node:&mut Node,catalogue:&mut Catalogue)->Step { unsafe {
    let driver_name=b"fixture";
    SETTLEMENT
    Step::Waiting
} }
fn snapshot(state:u32)->DeviceClaimSnapshot {
    let charged=u32::from(state==CLAIM_STATE_QUARANTINED);
    DeviceClaimSnapshot {
        state,_pad0:0,generation:if state==CLAIM_STATE_FREE {8} else {9},release_deadline:500,
        mmio_windows:charged,irq_vectors:2*charged,iommu_grants:3*charged,iommu_quarantined:charged,iommu_faults:0,
    }
}
fn setup(pre:Option<u32>,post:Option<u32>,errno:i64,prior:u32,manual:bool,count:usize)->Node {
    RT.with_borrow_mut(|rt|*rt=Runtime { snapshots:VecDeque::from([pre.map(snapshot),post.map(snapshot)]),errno,..Runtime::default() });
    let mut node=Node::new(0,&DeviceInfo::default(),vec![&ENTRY;count]);
    node.incident=Incident::open(); node.attempt=prior;node.retry_once=manual;node.retry_pending=manual;
    node
}
fn start(node:&mut Node,standing:bool) {
    unsafe { if standing {start_candidate(node,1,0,0,0,1,&Catalogue,&mut [0]);}
    else {let info=node.info;begin_bind(node,&info,&IMAGE,b"fixture",0,0,0,1);} }
}
#[test]
fn quarantine_after_refusal_is_terminal_across_callers_and_budgets() {
    for standing in [false,true] { for count in [1,3] { for (prior,manual) in [(0,false),(2,false),(0,true),(3,true)] {
        let mut node=setup(Some(CLAIM_STATE_FREE),Some(CLAIM_STATE_QUARANTINED),ERR_UNSUPPORTED,prior,manual,count);
        start(&mut node,standing);
        assert!(node.record.state==BindingState::Quarantined,"claim result must adopt kernel quarantine");
        assert!(node.record.failure==Some(FailureCause::TeardownUnconfirmed));
        assert_eq!((node.id.generation,node.record.generation),(9,9),"adopt actual attempted generation");
        assert_eq!(node.granted_resources,6);
        assert_eq!((node.attempt,node.record.attempts),(prior,0),"refund only this unclaimed reservation");
        assert_eq!(node.retry_pending,manual);
        assert!(node.teardown.is_some(),"caller must defer candidate progression until settlement");
        assert!(matches!(unsafe {settle_node(&mut node,&mut Catalogue)},Step::Done),"quarantine must stop candidate progression");
        assert!(node.teardown.is_none());
        assert!(unsafe {resolve_teardown(&mut node,b"fixture",10)}.is_none());
        assert!(matches!(unsafe {settle_node(&mut node,&mut Catalogue)},Step::Waiting));
        assert_eq!(node.candidate,0);
        assert_eq!(decide_policy(&node,proto::system::PolicyVerb::Retry,"").outcome,proto::system::PolicyOutcome::Quarantined,"operator cannot retry quarantine");
        RT.with_borrow(|rt| {
            assert_eq!(rt.claims,1);
            assert!(rt.transitions.iter().all(|(from,_,_)|*from!=BindingState::Quarantined),"quarantine must not attempt another transition");
            assert!(rt.granted.is_none());
            assert!(rt.effects.iter().all(|(kind,handle)|standing && *handle==77 && (*kind=="unmap" || *kind=="close")),"no phantom ownership effects");
        });
    } } }
}
#[test]
fn preexisting_quarantine_and_non_unsupported_error_adopt_the_same_snapshot() {
    for (pre,errno,claims) in [(CLAIM_STATE_QUARANTINED,ERR_UNSUPPORTED,0),(CLAIM_STATE_FREE,ERR_RESOURCE_EXHAUSTED,1),(CLAIM_STATE_FREE,ERR_ACCESS_DENIED,1)] {
        let mut node=setup(Some(pre),Some(CLAIM_STATE_QUARANTINED),errno,2,false,1);
        start(&mut node,false);
        assert!(matches!(unsafe {settle_node(&mut node,&mut Catalogue)},Step::Done));
        assert!(node.record.state==BindingState::Quarantined);
        assert_eq!((node.attempt,node.id.generation,node.record.generation),(2,9,9));
        RT.with_borrow(|rt|assert_eq!(rt.claims,claims));
    }
}
#[test]
fn ordinary_refusals_stay_distinct_and_do_not_invent_quarantine() {
    for post in [None,Some(CLAIM_STATE_FREE),Some(CLAIM_STATE_CLAIMED)] {
        for (errno,cause) in [(ERR_UNSUPPORTED,FailureCause::ClaimRefused),(ERR_ALREADY_CLAIMED,FailureCause::ClaimRefused),(ERR_ACCESS_DENIED,FailureCause::IommuRequired)] {
            let mut node=setup(Some(CLAIM_STATE_FREE),post,errno,1,false,1);start(&mut node,false);
            assert!(matches!(unsafe {settle_node(&mut node,&mut Catalogue)},Step::NextCandidate));
            assert!(node.record.state==BindingState::Failed && node.record.failure==Some(cause));
            assert_eq!(node.attempt,1);
        }
    }
}
#[test]
fn releasing_claim_is_parked_and_a_successful_grant_is_owned() {
    let mut node=setup(Some(CLAIM_STATE_RELEASING),None,ERR_UNSUPPORTED,1,false,2);start(&mut node,true);
    assert!(node.record.state==BindingState::Backoff && node.waiting_for_claim);
    assert!(node.teardown.is_none());assert_eq!(node.candidate,0);assert_eq!(node.attempt,1);
    RT.with_borrow(|rt|assert_eq!(rt.claims,0));
    let mut node=setup(None,None,0,1,false,1);start(&mut node,false);
    assert!(node.record.state==BindingState::Binding && node.teardown.is_none());
    assert_eq!((node.attempt,node.record.attempts,node.record.generation),(2,1,9));
    RT.with_borrow(|rt| {let held=rt.granted.as_ref().unwrap();assert_eq!(held.claim,21);assert_eq!(held.resources(),&[(driver_protocol::ResourceKind::Device as u16,20)]);assert_eq!(rt.snapshots.len(),1);});
}
'''


def fixture(source):
    types = ['struct Node {', 'struct Binding {', 'struct Attempt {', 'struct Teardown {', 'enum ClaimReadiness {', 'enum BindStart {', 'enum Step {', 'struct PolicyDecision {', 'enum PolicySlot {']
    functions = ['impl driver_binding::Closes for Syscalls', 'unsafe fn give_up_retryable(', 'unsafe fn give_up_with_budget(', 'unsafe fn resolve_teardown(', 'unsafe fn observe_claim(', 'unsafe fn observe_claim_snapshot(', 'fn bind_start_of(', 'unsafe fn may_try_again(', 'unsafe fn start_candidate(', 'fn decide_policy(']
    methods = ['fn new(index: u64, info:', 'fn has_bind_allowance(', 'fn admit_bind_attempt(', 'fn refund_unclaimed_attempt(', 'fn claim_admitted(']
    begin = source.index('let mut txn = Attempt::new();', source.index('unsafe fn begin_bind('))
    end = source.index('let (dm_side, driver_side):', begin)
    advance = item(source, 'unsafe fn advance(')
    settlement = item(advance, 'if node.teardown.is_some() {')
    return (FIXTURE.replace('PRODUCTION_TYPES', '\n'.join(item(source, name) for name in types))
            .replace('NODE_METHODS', '\n'.join(item(source, name) for name in methods))
            .replace('BEGIN_TEARDOWN', item(source, 'unsafe fn begin_teardown(&mut self'))
            .replace('PRODUCTION_FUNCTIONS', '\n'.join(item(source, name) for name in functions))
            .replace('CLAIM_STAGE', source[begin:end])
            .replace('SETTLEMENT', settlement))


def main():
    source = (ROOT / 'src/user/services/core/src/device_manager.rs').read_text()
    variants = [('production', source, None)]
    old = 'Some(snapshot) if snapshot.state == CLAIM_STATE_QUARANTINED => {'
    assert source.count(old) == 1
    variants.append(('unobserved claim refusal', source.replace(old, 'Some(snapshot) if false => {', 1), 'claim result must adopt kernel quarantine'))
    guard = '\t\tif node.record.state == BindingState::Quarantined {\n\t\t\treturn Some(BindingState::Quarantined);\n\t\t}'
    assert source.count(guard) == 1
    # Remove the actual production fix: the real state machine refuses the demotion,
    # but the attempted invalid transition still violates terminal settlement.
    variants.append(('empty ledger attempts quarantine demotion', source.replace(guard, '', 1), 'quarantine must not attempt another transition'))
    generation = '\t\t\t\tnode.record.generation = snapshot.generation;\n\t\t\t\tnode.id = node.id.rebound(snapshot.generation);'
    assert source.count(generation) == 1
    variants.append(('lost attempted generation', source.replace(generation, '', 1), 'adopt actual attempted generation'))
    with tempfile.TemporaryDirectory(prefix='liber-claim-result-') as directory:
        path = Path(directory)
        (path / 'src').mkdir()
        manifest = '[package]\nname="claim-result-check"\nversion="0.0.0"\nedition="2024"\n[dependencies]\n'
        for name, location in [('driver-binding','src/user/libs/driver/binding'),('driver-protocol','src/user/libs/driver/protocol'),('abi','src/abi')]:
            manifest += f'{name}={{path="{ROOT / location}"}}\n'
        (path / 'Cargo.toml').write_text(manifest)
        env = {**os.environ, 'CARGO_TARGET_DIR': str(path / 'target')}
        for label, variant, assertion in variants:
            (path / 'src/lib.rs').write_text(fixture(variant))
            result = subprocess.run(['cargo','test','--offline','--quiet','--manifest-path',str(path/'Cargo.toml')],env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
            if assertion is None:
                if result.returncode != 0: raise SystemExit(result.stdout)
            elif result.returncode == 0 or assertion not in result.stdout or 'panicked at' not in result.stdout:
                raise SystemExit(f'{label}: expected named assertion, not build failure\n{result.stdout}')
            print(f'device-claim-result: {label} {"passed" if assertion is None else "rejected"}')


if __name__ == '__main__':
    main()
