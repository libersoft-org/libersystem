use super::*;
use alloc::string::String;

#[test]
fn ipv4_addr_wire_is_stable() {
	let sample = Ipv4Addr { a: 7, b: 7, c: 7, d: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 7, 7];
	assert_eq!(bytes, golden);
	assert_eq!(Ipv4Addr::decode(&bytes).unwrap(), sample);
}
#[test]
fn ipv6_addr_wire_is_stable() {
	let sample = Ipv6Addr { o0: 7, o1: 7, o2: 7, o3: 7, o4: 7, o5: 7, o6: 7, o7: 7, o8: 7, o9: 7, o10: 7, o11: 7, o12: 7, o13: 7, o14: 7, o15: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7];
	assert_eq!(bytes, golden);
	assert_eq!(Ipv6Addr::decode(&bytes).unwrap(), sample);
}
#[test]
fn mac_addr_wire_is_stable() {
	let sample = MacAddr { a: 7, b: 7, c: 7, d: 7, e: 7, f: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 7, 7, 7, 7];
	assert_eq!(bytes, golden);
	assert_eq!(MacAddr::decode(&bytes).unwrap(), sample);
}
#[test]
fn ip_address_wire_is_stable() {
	let sample = IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 });
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 7, 7, 7];
	assert_eq!(bytes, golden);
	assert_eq!(IpAddress::decode(&bytes).unwrap(), sample);
}
#[test]
fn interface_id_wire_is_stable() {
	let sample = InterfaceId { index: 7, generation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(InterfaceId::decode(&bytes).unwrap(), sample);
}
#[test]
fn scoped_address_wire_is_stable() {
	let sample = ScopedAddress { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), scope: Some(InterfaceId { index: 7, generation: 7 }) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 7, 7, 7, 1, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ScopedAddress::decode(&bytes).unwrap(), sample);
}
#[test]
fn scoped_endpoint_wire_is_stable() {
	let sample = ScopedEndpoint { addr: ScopedAddress { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), scope: Some(InterfaceId { index: 7, generation: 7 }) }, port: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 7, 7, 7, 1, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ScopedEndpoint::decode(&bytes).unwrap(), sample);
}
#[test]
fn address_state_wire_is_stable() {
	let sample = AddressState::Tentative;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(AddressState::decode(&bytes).unwrap(), sample);
}
#[test]
fn interface_address_wire_is_stable() {
	let sample = InterfaceAddress { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), prefix_len: 7, state: AddressState::Tentative, preferred_seconds: 7, valid_seconds: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 7, 7, 7, 7, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(InterfaceAddress::decode(&bytes).unwrap(), sample);
}
#[test]
fn next_hop_wire_is_stable() {
	let sample = NextHop::Direct;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(NextHop::decode(&bytes).unwrap(), sample);
}
#[test]
fn route_preference_wire_is_stable() {
	let sample = RoutePreference::Low;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(RoutePreference::decode(&bytes).unwrap(), sample);
}
#[test]
fn reachability_wire_is_stable() {
	let sample = Reachability::Incomplete;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(Reachability::decode(&bytes).unwrap(), sample);
}
#[test]
fn route_entry_wire_is_stable() {
	let sample = RouteEntry { destination: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), prefix_len: 7, scope: InterfaceId { index: 7, generation: 7 }, preference: RoutePreference::Low, lifetime_seconds: 7, hop: NextHop::Direct };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 7, 7, 7, 7, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(RouteEntry::decode(&bytes).unwrap(), sample);
}
#[test]
fn router_entry_wire_is_stable() {
	let sample = RouterEntry { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), scope: InterfaceId { index: 7, generation: 7 }, preference: RoutePreference::Low, state: Reachability::Incomplete, lifetime_seconds: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 7, 7, 7, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(RouterEntry::decode(&bytes).unwrap(), sample);
}
#[test]
fn dns_server_wire_is_stable() {
	let sample = DnsServer { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), scope: InterfaceId { index: 7, generation: 7 } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 7, 7, 7, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(DnsServer::decode(&bytes).unwrap(), sample);
}
#[test]
fn neighbor_wire_is_stable() {
	let sample = Neighbor { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), mac: MacAddr { a: 7, b: 7, c: 7, d: 7, e: 7, f: 7 }, scope: InterfaceId { index: 7, generation: 7 } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Neighbor::decode(&bytes).unwrap(), sample);
}
#[test]
fn net_info_wire_is_stable() {
	let sample = NetInfo { scope: InterfaceId { index: 7, generation: 7 }, name: String::from("x"), mac: MacAddr { a: 7, b: 7, c: 7, d: 7, e: 7, f: 7 }, mtu: 7, addresses: alloc::vec![InterfaceAddress { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), prefix_len: 7, state: AddressState::Tentative, preferred_seconds: 7, valid_seconds: 7 }], routes: alloc::vec![RouteEntry { destination: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), prefix_len: 7, scope: InterfaceId { index: 7, generation: 7 }, preference: RoutePreference::Low, lifetime_seconds: 7, hop: NextHop::Direct }], routers: alloc::vec![RouterEntry { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), scope: InterfaceId { index: 7, generation: 7 }, preference: RoutePreference::Low, state: Reachability::Incomplete, lifetime_seconds: 7 }], dns: alloc::vec![DnsServer { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), scope: InterfaceId { index: 7, generation: 7 } }], neighbors: alloc::vec![Neighbor { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), mac: MacAddr { a: 7, b: 7, c: 7, d: 7, e: 7, f: 7 }, scope: InterfaceId { index: 7, generation: 7 } }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		0,
		120,
		7,
		7,
		7,
		7,
		7,
		7,
		7,
		0,
		1,
		0,
		0,
		7,
		7,
		7,
		7,
		7,
		0,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		1,
		0,
		0,
		7,
		7,
		7,
		7,
		7,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		1,
		0,
		0,
		7,
		7,
		7,
		7,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		1,
		0,
		0,
		7,
		7,
		7,
		7,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		0,
		0,
		7,
		7,
		7,
		7,
		7,
		7,
		7,
		7,
		7,
		7,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
	];
	assert_eq!(bytes, golden);
	assert_eq!(NetInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn family_readiness_wire_is_stable() {
	let sample = FamilyReadiness::Disabled;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(FamilyReadiness::decode(&bytes).unwrap(), sample);
}
#[test]
fn net_capacity_wire_is_stable() {
	let sample = NetCapacity { clients: 7, sockets: 7, listeners: 7, connections: 7, ipv4: FamilyReadiness::Disabled, ipv6: FamilyReadiness::Disabled, diagnostic_used: 7, diagnostic_limit: 7, diagnostic_refusals: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(NetCapacity::decode(&bytes).unwrap(), sample);
}
#[test]
fn ping_status_wire_is_stable() {
	let sample = PingStatus::Reply;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(PingStatus::decode(&bytes).unwrap(), sample);
}
#[test]
fn ping_reply_wire_is_stable() {
	let sample = PingReply { status: PingStatus::Reply, ttl: 7, rtt_us: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(PingReply::decode(&bytes).unwrap(), sample);
}
#[test]
fn hop_status_wire_is_stable() {
	let sample = HopStatus::TimeExceeded;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(HopStatus::decode(&bytes).unwrap(), sample);
}
#[test]
fn trace_hop_wire_is_stable() {
	let sample = TraceHop { status: HopStatus::TimeExceeded, addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), rtt_us: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 0, 7, 7, 7, 7, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(TraceHop::decode(&bytes).unwrap(), sample);
}
#[test]
fn open_target_wire_is_stable() {
	let sample = OpenTarget { destinations: alloc::vec![ScopedAddress { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), scope: Some(InterfaceId { index: 7, generation: 7 }) }], port: 7, source: Some(ScopedAddress { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), scope: Some(InterfaceId { index: 7, generation: 7 }) }) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 0, 7, 7, 7, 7, 1, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 1, 0, 7, 7, 7, 7, 1, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(OpenTarget::decode(&bytes).unwrap(), sample);
}
#[test]
fn tcp_request_wire_is_stable() {
	let sample = TcpRequest { target: OpenTarget { destinations: alloc::vec![ScopedAddress { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), scope: Some(InterfaceId { index: 7, generation: 7 }) }], port: 7, source: Some(ScopedAddress { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), scope: Some(InterfaceId { index: 7, generation: 7 }) }) }, request: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 0, 7, 7, 7, 7, 1, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 1, 0, 7, 7, 7, 7, 1, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(TcpRequest::decode(&bytes).unwrap(), sample);
}
#[test]
fn fetch_outcome_wire_is_stable() {
	let sample = FetchOutcome::Complete;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(FetchOutcome::decode(&bytes).unwrap(), sample);
}
#[test]
fn fetch_chunk_wire_is_stable() {
	let sample = FetchChunk { data: alloc::vec![7], outcome: Some(FetchOutcome::Complete) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 7, 1, 0];
	assert_eq!(bytes, golden);
	assert_eq!(FetchChunk::decode(&bytes).unwrap(), sample);
}
#[test]
fn bind_mode_wire_is_stable() {
	let sample = BindMode::Ipv4Only;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(BindMode::decode(&bytes).unwrap(), sample);
}
#[test]
fn listen_request_wire_is_stable() {
	let sample = ListenRequest { mode: BindMode::Ipv4Only, local: ScopedEndpoint { addr: ScopedAddress { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), scope: Some(InterfaceId { index: 7, generation: 7 }) }, port: 7 }, backlog: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 0, 7, 7, 7, 7, 1, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 7, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ListenRequest::decode(&bytes).unwrap(), sample);
}
#[test]
fn sock_state_wire_is_stable() {
	let sample = SockState::Closed;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(SockState::decode(&bytes).unwrap(), sample);
}
#[test]
fn sock_info_wire_is_stable() {
	let sample = SockInfo { local: ScopedEndpoint { addr: ScopedAddress { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), scope: Some(InterfaceId { index: 7, generation: 7 }) }, port: 7 }, remote: ScopedEndpoint { addr: ScopedAddress { addr: IpAddress::V4(Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }), scope: Some(InterfaceId { index: 7, generation: 7 }) }, port: 7 }, state: SockState::Closed };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 7, 7, 7, 1, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 7, 7, 7, 7, 1, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(SockInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn chunk_wire_is_stable() {
	let sample = Chunk { data: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(Chunk::decode(&bytes).unwrap(), sample);
}
