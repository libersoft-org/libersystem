#!/usr/bin/env python3
"""Exercise production catalogue publication and failed capability delivery on host channels."""
from pathlib import Path
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
extern crate alloc;
use std::{cell::RefCell, collections::{HashMap, HashSet}};
use driver_binding::{BindingId, ProviderId};
use proto::system::provider_catalogue::Service;
mod proto { pub mod system {
    pub use device_proto::generated::liber::device::v1::*;
    pub use device_proto::generated::liber::base::v1::Error;
} }
const MAX_CATALOGUE_CLIENTS: usize = 8;
const MAX_SUBSCRIBERS: usize = MAX_CATALOGUE_CLIENTS;
const CONNECT_OP: u16 = 0xff00;
const HEARTBEAT_OP: u16 = 0xff01;
#[derive(Default)]
struct Runtime {
    next: u64, live: HashSet<u64>, peers: HashMap<u64, u64>, closed: Vec<u64>,
    request: Vec<u8>, delivered: Vec<u64>, fail_reply: bool, connected: Vec<(u16, u64)>,
}
thread_local! { static RT: RefCell<Runtime> = RefCell::new(Runtime::default()); }
fn print(_: &[u8]) {}
fn channel() -> Option<(u64, u64)> { RT.with_borrow_mut(|rt| {
    let mine = rt.next + 100; let theirs = mine + 1; rt.next += 2;
    rt.live.extend([mine, theirs]); rt.peers.insert(mine, theirs); rt.peers.insert(theirs, mine);
    Some((mine, theirs))
}) }
fn close(handle: u64) { RT.with_borrow_mut(|rt| {
    assert!(rt.live.remove(&handle), "close must own its live endpoint: {handle}");
    rt.closed.push(handle);
}) }
fn send_blocking(handle: u64, bytes: &[u8], transfer: u64) -> bool {
    send_caps_blocking(handle, bytes, if transfer == 0 { &[] } else { std::slice::from_ref(&transfer) })
}
fn send_caps_blocking(_: u64, _: &[u8], handles: &[u64]) -> bool { RT.with_borrow_mut(|rt| {
    if rt.fail_reply { return false; }
    for handle in handles { assert!(rt.live.remove(handle)); rt.delivered.push(*handle); }
    true
}) }
fn send_frame(_: u64, opcode: driver_protocol::Opcode, _: u64, payload: &[u8], server: u64, _: u32) -> bool {
    assert_eq!(opcode, driver_protocol::Opcode::Connect);
    RT.with_borrow_mut(|rt| {
        assert!(rt.live.remove(&server));
        rt.connected.push((u16::from_le_bytes(payload.try_into().unwrap()), server));
    }); true
}
fn try_send_caps(_: u64, _: &[u8], _: &[u64]) -> bool { true }
enum PolledCaps { Empty, Closed }
fn try_recv_caps(_: u64, _: &mut [u8]) -> PolledCaps { PolledCaps::Empty }
enum ReceivedCaps { Message { len: usize, handles: wire::Handles }, Closed }
fn recv_caps_blocking(_: u64, out: &mut [u8]) -> ReceivedCaps { RT.with_borrow(|rt| {
    out[..rt.request.len()].copy_from_slice(&rt.request);
    ReceivedCaps::Message { len: rt.request.len(), handles: wire::Handles::new() }
}) }
fn open_subscription(_: u64, _: &mut Catalogue, _: &[Node], _: &[u8], _: &mut wire::Handles) {
    panic!("subscription is covered by the manager progress gate");
}
struct Entry { name: &'static [u8], provides: &'static [(u16, u16, u16)] }
static SINGLE: Entry = Entry { name: b"single", provides: &[(driver_protocol::provider::BLOCK, 3, 1), (driver_protocol::provider::NET, 3, 1)] };
static DOUBLE: Entry = Entry { name: b"double", provides: &[(driver_protocol::provider::BLOCK, 3, 2)] };
struct Binding { channel: u64 }
struct Node { id: BindingId, binding: Option<Binding>, declared: &'static Entry }
impl Node { fn entry(&self) -> Option<&'static Entry> { Some(self.declared) } }
struct CatalogueView<'a> { catalogue: &'a mut Catalogue, nodes: &'a [Node] }
impl Service for CatalogueView<'_> {
    fn bindings(&mut self) -> Vec<proto::system::BindingRecord> { vec![] }
    fn subscribe(&mut self, _: proto::system::ProviderKind) -> Vec<proto::system::ProviderInfo> { vec![] }
    OPEN_METHOD
}
fn binding(device: u8) -> BindingId { BindingId::new(0, device, 0, 1) }
fn publish(catalogue: &mut Catalogue, id: BindingId, entry: &'static Entry, token: u16, kind: u16) -> (usize, u64) {
    let (_, offered) = channel().unwrap(); let mut offers = Offers::new();
    assert!(offers.push(kind, token, offered));
    (unsafe { catalogue.publish_all(id, entry, &mut offers) }, offered)
}
fn request_open(info: &proto::system::ProviderInfo, fail: bool) {
    let mut request = proto::system::provider_catalogue::OP_OPEN.to_le_bytes().to_vec();
    request.extend_from_slice(&7u32.to_le_bytes()); request.extend(info.encode_vec().unwrap());
    RT.with_borrow_mut(|rt| { rt.request = request; rt.fail_reply = fail; });
}
fn serve(catalogue: &mut Catalogue, nodes: &[Node], clients: &mut CatalogueClients, root: bool) {
    assert!(unsafe { serve_catalogue_once(1, root, clients, catalogue, nodes, &mut [0; 512]) });
}
fn refund_departure(catalogue: &mut Catalogue, id: BindingId, token: u16, client: u64) {
    assert!(RT.with_borrow(|rt| rt.closed.contains(&client)), "the driver can observe departure only after the manager closes the undelivered client");
    catalogue.disconnected(id, token);
}
#[test]
fn failed_offered_and_minted_replies_return_the_concurrent_allowance() {
    for entry in [&SINGLE, &DOUBLE] {
        RT.with_borrow_mut(|rt| *rt = Runtime::default());
        let mut catalogue = Catalogue::new(); let mut clients = CatalogueClients::new();
        let nodes = [Node { id: binding(1), binding: Some(Binding { channel: 9 }), declared: entry }];
        let (_, offered) = publish(&mut catalogue, nodes[0].id, entry, 7, driver_protocol::provider::BLOCK);
        let info = provider_info_wire(catalogue.entries[0].as_ref().unwrap(), true);
        if std::ptr::eq(entry, &DOUBLE) {
            assert!(CatalogueView { catalogue: &mut catalogue, nodes: &nodes }.open(info.clone()).is_ok());
        }
        request_open(&info, true); serve(&mut catalogue, &nodes, &mut clients, false);
        let closed = RT.with_borrow(|rt| *rt.closed.last().expect("failed reply endpoint must be closed"));
        if std::ptr::eq(entry, &SINGLE) { assert_eq!(closed, offered); }
        refund_departure(&mut catalogue, nodes[0].id, 7, closed);
        assert!(CatalogueView { catalogue: &mut catalogue, nodes: &nodes }.open(info).is_ok(), "the next client must reuse the allowance");
    }
}
#[test]
fn successful_reply_transfers_ownership_without_closing_the_provider() {
    let mut catalogue = Catalogue::new(); let mut clients = CatalogueClients::new();
    let nodes = [Node { id: binding(1), binding: Some(Binding { channel: 9 }), declared: &SINGLE }];
    let (_, offered) = publish(&mut catalogue, nodes[0].id, &SINGLE, 7, driver_protocol::provider::BLOCK);
    let info = provider_info_wire(catalogue.entries[0].as_ref().unwrap(), true);
    request_open(&info, false); serve(&mut catalogue, &nodes, &mut clients, false);
    RT.with_borrow(|rt| { assert_eq!(rt.delivered, [offered]); assert!(rt.closed.is_empty()); });
    assert_eq!(catalogue.entries[0].as_ref().unwrap().consumers, 1);
}
#[test]
fn failed_root_connections_do_not_spend_client_slots() {
    let mut catalogue = Catalogue::new(); let mut clients = CatalogueClients::new();
    let (existing, _) = channel().unwrap(); clients.channels[0] = existing; clients.count = 1;
    RT.with_borrow_mut(|rt| { rt.request = CONNECT_OP.to_le_bytes().to_vec(); rt.fail_reply = true; });
    for _ in 0..MAX_CATALOGUE_CLIENTS + 2 {
        let before = RT.with_borrow(|rt| rt.live.len());
        serve(&mut catalogue, &[], &mut clients, true);
        assert_eq!(clients.live(), [existing], "only the failed pair should retire");
        assert_eq!(RT.with_borrow(|rt| rt.live.len()), before, "neither end of the undelivered pair may leak");
    }
    RT.with_borrow_mut(|rt| rt.fail_reply = false);
    serve(&mut catalogue, &[], &mut clients, true); assert_eq!(clients.count, 2);
}
#[test]
fn handshake_tokens_are_unique_across_kinds() {
    let mut offers = Offers::new();
    assert!(offers.push(driver_protocol::provider::BLOCK, 7, 10));
    assert!(!offers.push(driver_protocol::provider::BLOCK, 7, 11));
    assert!(!offers.push(driver_protocol::provider::NET, 7, 12));
    assert!(offers.push(driver_protocol::provider::NET, 8, 13));
    assert_eq!(offers.count, 2);
}
#[test]
fn tokens_are_unique_for_the_complete_binding_generation() {
    let mut catalogue = Catalogue::new(); let id = binding(1);
    assert_eq!(publish(&mut catalogue, id, &SINGLE, 7, driver_protocol::provider::BLOCK).0, 1);
    for kind in [driver_protocol::provider::BLOCK, driver_protocol::provider::NET] {
        let (count, duplicate) = publish(&mut catalogue, id, &SINGLE, 7, kind);
        assert_eq!(count, 0, "a later OFFER cannot alias a live token even under another kind");
        assert!(RT.with_borrow(|rt| rt.closed.contains(&duplicate)));
    }
    assert_eq!(publish(&mut catalogue, binding(2), &SINGLE, 7, driver_protocol::provider::BLOCK).0, 1);
    assert_eq!(publish(&mut catalogue, id, &SINGLE, 8, driver_protocol::provider::BLOCK).0, 1);
    catalogue.entries[0].as_mut().unwrap().consumers = 1;
    catalogue.entries[1].as_mut().unwrap().consumers = 1;
    catalogue.entries[2].as_mut().unwrap().consumers = 1;
    catalogue.disconnected(id, 8);
    assert_eq!(catalogue.entries.iter().flatten().map(|p| p.consumers).collect::<Vec<_>>(), [1,1,0]);
    assert!(unsafe { catalogue.withdraw(id, 7) }.is_some());
    assert_eq!(catalogue.count_for_binding(binding(2)), 1);
    assert_eq!(publish(&mut catalogue, id, &SINGLE, 7, driver_protocol::provider::NET).0, 0);
    unsafe { catalogue.withdraw_binding(id); }
    assert_eq!(publish(&mut catalogue, id, &SINGLE, 7, driver_protocol::provider::NET).0, 0);
    catalogue.retire_binding(id);
    assert!(!catalogue.used_tokens.iter().any(|(owner, _)| *owner == id));
    assert!(catalogue.used_tokens.contains(&(binding(2), 7)));
    let next = BindingId::new(0, 1, 0, 2);
    assert_eq!(publish(&mut catalogue, next, &SINGLE, 7, driver_protocol::provider::NET).0, 1);
}
#[test]
fn retired_departures_cannot_refund_replacement_or_new_generation() {
    let id = binding(1);
    let mut catalogue = Catalogue::new();
    let mut nodes = [Node { id, binding: Some(Binding { channel: 9 }), declared: &SINGLE }];
    for unopened in [false, true] {
        let token = if unopened { 9 } else { 7 };
        let (_, offered) = publish(&mut catalogue, id, &SINGLE, token, driver_protocol::provider::BLOCK);
        let info = provider_info_wire(catalogue.entries[0].as_ref().unwrap(), true);
        if !unopened { assert!(CatalogueView { catalogue: &mut catalogue, nodes: &nodes }.open(info).is_ok()); }
        unsafe { catalogue.withdraw(id, token); }
        if unopened { assert!(RT.with_borrow(|rt| rt.closed.contains(&offered))); }
        assert_eq!(publish(&mut catalogue, id, &SINGLE, token, driver_protocol::provider::BLOCK).0, 0);
        assert_eq!(publish(&mut catalogue, id, &SINGLE, token + 1, driver_protocol::provider::BLOCK).0, 1);
        let replacement = provider_info_wire(catalogue.entries[0].as_ref().unwrap(), true);
        assert!(CatalogueView { catalogue: &mut catalogue, nodes: &nodes }.open(replacement.clone()).is_ok());
        if !unopened { close(offered); }
        catalogue.disconnected(id, token);
        assert_eq!(catalogue.entries[0].as_ref().unwrap().consumers, 1);
        assert!(CatalogueView { catalogue: &mut catalogue, nodes: &nodes }.open(replacement.clone()).is_err());
        catalogue.disconnected(id, token + 1);
        assert!(CatalogueView { catalogue: &mut catalogue, nodes: &nodes }.open(replacement).is_ok());
        unsafe { catalogue.withdraw_binding(id); }
        assert_eq!(publish(&mut catalogue, id, &SINGLE, token, driver_protocol::provider::BLOCK).0, 0);
    }
    catalogue.retire_binding(id);
    nodes[0].id = BindingId::new(0, 1, 0, 2);
    assert_eq!(publish(&mut catalogue, nodes[0].id, &SINGLE, 7, driver_protocol::provider::BLOCK).0, 1);
    let replacement = provider_info_wire(catalogue.entries[0].as_ref().unwrap(), true);
    assert!(CatalogueView { catalogue: &mut catalogue, nodes: &nodes }.open(replacement.clone()).is_ok());
    catalogue.disconnected(id, 7);
    assert!(unsafe { catalogue.withdraw(id, 7) }.is_none());
    assert!(CatalogueView { catalogue: &mut catalogue, nodes: &nodes }.open(replacement).is_err());
}
#[test]
fn token_history_allocation_failure_closes_offer_without_spending_token_or_slot() {
    let mut catalogue = Catalogue::new(); let id = binding(1);
    FAIL_TOKENS.with(|flag| flag.set(true));
    let (count, refused) = publish(&mut catalogue, id, &SINGLE, 7, driver_protocol::provider::BLOCK);
    assert_eq!(count, 0);
    assert!(RT.with_borrow(|rt| rt.closed.contains(&refused)));
    assert!(catalogue.entries.is_empty());
    assert!(catalogue.used_tokens.is_empty());
    FAIL_TOKENS.with(|flag| flag.set(false));
    assert_eq!(publish(&mut catalogue, id, &SINGLE, 7, driver_protocol::provider::BLOCK).0, 1);
}
// Replace only the backing allocation in the extracted catalogue. All admission and commit logic
// remains production code; this double makes its fallible reserve deterministically fail.
thread_local! { static FAIL_TOKENS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
struct TokenHistory(Vec<(BindingId, u16)>);
impl TokenHistory {
    const fn new() -> Self { Self(Vec::new()) }
    fn try_reserve(&mut self, extra: usize) -> Result<(), ()> {
        if FAIL_TOKENS.with(|flag| flag.get()) { Err(()) } else { self.0.try_reserve(extra).map_err(|_| ()) }
    }
}
impl std::ops::Deref for TokenHistory { type Target = Vec<(BindingId, u16)>; fn deref(&self) -> &Self::Target { &self.0 } }
impl std::ops::DerefMut for TokenHistory { fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 } }

'''


def main() -> None:
    source = (ROOT / "src/user/services/core/src/device_manager.rs").read_text()
    definitions = ["struct Offers {", "impl Offers {", "struct Provider {", "impl Provider {", "struct Subscriber {", "struct Catalogue {", "impl Catalogue {", "impl driver_binding::Withdrawn<Provider> for Catalogue {", "struct CatalogueClients {", "impl CatalogueClients {"]
    functions = ["fn outstanding(", "fn provider_kind_from_wire(", "fn provider_kind_wire(", "fn provider_info_wire(", "unsafe fn send_provider_frame(", "unsafe fn serve_catalogue_once(", "unsafe fn channel_pair_for_catalogue("]
    opening = item(source[source.index("impl proto::system::provider_catalogue::Service for CatalogueView"):], "fn open(")
    program = FIXTURE.replace("OPEN_METHOD", opening) + "\n".join(item(source, start) for start in definitions + functions)
    program = program.replace("used_tokens: Vec<(BindingId, u16)>", "used_tokens: TokenHistory").replace("used_tokens: Vec::new()", "used_tokens: TokenHistory::new()")
    mutations = {
        "unclosed factory reply": program.replace("for &handle in reply_handles.as_slice() {\n\t\t\tclose(handle);", "for &handle in reply_handles.as_slice() {"),
        "unretired root connection": program.replace("clients.retire(clients.count - 1);", ""),
        "duplicate handshake token": program.replace(" || self.tokens[..self.count].contains(&token)", ""),
        "live-only token validation": program.replace("if self.used_tokens.contains(&(binding, offers.tokens[index])) {", "if self.entries.iter().flatten().any(|provider| provider.binding_is(binding) && provider.token == offers.tokens[index]) {"),
        "unchecked token history reserve": program.replace("if self.used_tokens.try_reserve(1).is_err() {", "if false {"),
        "early token retirement": program.replace("unsafe fn withdraw_binding(&mut self, binding: BindingId) -> usize {", "unsafe fn withdraw_binding(&mut self, binding: BindingId) -> usize { self.retire_binding(binding);"),
    }
    with tempfile.TemporaryDirectory(prefix="liber-provider-catalogue-") as directory:
        path = Path(directory)
        dependencies = {"driver-binding": "src/user/libs/driver/binding", "driver-protocol": "src/user/libs/driver/protocol", "device-proto": "src/user/libs/protocol/device-proto", "wire": "src/wire"}
        manifest = '[package]\nname="provider-catalogue-regressions"\nedition="2024"\n[lib]\npath="tests.rs"\n[features]\ndevelopment=[]\n[dependencies]\n'
        manifest += "".join(f'{name}={{path="{ROOT / relative}"}}\n' for name, relative in dependencies.items())
        (path / "Cargo.toml").write_text(manifest)
        command = ["cargo", "test", "--offline", "--quiet", "--manifest-path", str(path / "Cargo.toml"), "--lib"]
        (path / "tests.rs").write_text(program)
        result = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if result.returncode or "7 passed" not in result.stdout:
            raise SystemExit(result.stdout)
        print("provider-catalogue: 7 production delivery and publication regressions passed")
        for name, mutant in mutations.items():
            if mutant == program:
                raise ValueError(f"{name}: mutation did not change code")
            (path / "tests.rs").write_text(mutant)
            result = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            if result.returncode != 101 or "test result: FAILED" not in result.stdout:
                raise SystemExit(f"{name}: expected assertion failure:\n{result.stdout}")
            print(f"provider-catalogue: rejected {name}")


if __name__ == "__main__":
    main()
