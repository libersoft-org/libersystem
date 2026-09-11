//! The fixtures that hold the framing to the bounds.
//!
//! A BOUND THE SERVICE CANNOT RECEIVE IS A LIE ON THE WIRE. These build the largest legal value of
//! each shape the contract permits, encode it, and check it fits the buffer one client call is
//! framed in. They are the reason the reply buffer is 65536 rather than a number that looked
//! generous: 4096 was generous until `net-info` grew five tables.

use super::*;
use crate::generated::liber::network::v1::*;
use alloc::vec::Vec;

fn scope() -> InterfaceId {
	InterfaceId { index: 0, generation: 1 }
}

fn v6(low: u16) -> IpAddress {
	let mut octets = [0u8; 16];
	octets[0] = 0x20;
	octets[1] = 0x01;
	octets[2] = 0x0d;
	octets[3] = 0xb8;
	octets[14..].copy_from_slice(&low.to_be_bytes());
	IpAddress::V6(Ipv6Addr::from_octets(octets))
}

/// A scoped IPv6 address, which is the widest one of these records can hold.
fn scoped(low: u16) -> ScopedAddress {
	ScopedAddress { addr: v6(low), scope: Some(scope()) }
}

/// The largest `net-info` this contract permits: every list full, every address the wide family.
fn widest_info() -> NetInfo {
	NetInfo {
		scope: scope(),
		name: alloc::string::String::from("0123456789abcdef"),
		mac: MacAddr::from_octets([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]),
		mtu: 1500,
		addresses: (0..MAX_INTERFACE_ADDRESSES as u16).map(|index| InterfaceAddress { addr: v6(index), prefix_len: 64, state: AddressState::Preferred, preferred_seconds: u32::MAX, valid_seconds: u32::MAX }).collect(),
		// EVERY ROUTE `VIA` AN ADDRESS, which is the larger of the two forms: a direct route carries
		// no address at all, so a table of direct routes would measure the smaller case.
		routes: (0..MAX_ROUTES as u16).map(|index| RouteEntry { destination: v6(index), prefix_len: 64, scope: scope(), preference: RoutePreference::Medium, lifetime_seconds: u32::MAX, hop: NextHop::Via(v6(index)) }).collect(),
		routers: (0..MAX_ROUTERS as u16).map(|index| RouterEntry { addr: v6(index), scope: scope(), preference: RoutePreference::High, state: Reachability::Reachable, lifetime_seconds: 1800 }).collect(),
		dns: (0..MAX_DNS_SERVERS as u16).map(|index| DnsServer { addr: v6(index), scope: scope() }).collect(),
		neighbors: (0..MAX_NEIGHBORS as u16).map(|index| Neighbor { addr: v6(index), mac: MacAddr::from_octets([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]), scope: scope() }).collect(),
	}
}

#[test]
fn the_widest_interface_snapshot_fits_the_reply_buffer() {
	let info = widest_info();
	let encoded: Vec<u8> = info.encode_vec().expect("it encodes");
	assert!(encoded.len() <= REPLY_BYTES, "a full net-info is {} bytes and the reply buffer is {REPLY_BYTES}", encoded.len());
	// AND IT IS NOT COMFORTABLY SMALL EITHER, which is the half of this that matters: it must not
	// fit the buffer this contract replaced, or the buffer would not have needed to grow and the
	// fixture would be measuring nothing.
	assert!(encoded.len() > 4096, "a full net-info is {} bytes, which the old 4096-byte buffer would have held", encoded.len());
	assert_eq!(NetInfo::decode(&encoded).expect("it decodes"), info, "and it round trips whole");
}

#[test]
fn the_widest_socket_list_fits_the_reply_buffer() {
	let sockets: Vec<SockInfo> = (0..MAX_SOCKET_ROWS as u16).map(|index| SockInfo { local: ScopedEndpoint { addr: scoped(index), port: 443 }, remote: ScopedEndpoint { addr: scoped(index), port: 1024 + index }, state: SockState::Established }).collect();
	// The list rides inside a `result<list<sock-info>, error>`, so the measurement includes the
	// length prefix and the ok byte the reply actually carries.
	let mut encoded: Vec<u8> = alloc::vec![1u8];
	encoded.extend_from_slice(&(sockets.len() as u16).to_le_bytes());
	for socket in &sockets {
		encoded.extend_from_slice(&socket.encode_vec().expect("it encodes"));
	}
	assert!(encoded.len() <= REPLY_BYTES, "a full socket list is {} bytes and the reply buffer is {REPLY_BYTES}", encoded.len());
	assert!(encoded.len() > 4096, "a full socket list is {} bytes, which the old 4096-byte buffer would have held", encoded.len());
}

#[test]
fn the_widest_open_request_fits_the_request_buffer() {
	// Eight scoped IPv6 destinations, a caller-chosen source, and the full 1024 request bytes.
	let request = TcpRequest { target: OpenTarget { destinations: (0..MAX_OPEN_DESTINATIONS as u16).map(scoped).collect(), port: 443, source: Some(scoped(99)) }, request: alloc::vec![b'x'; MAX_REQUEST_BYTES] };
	let encoded: Vec<u8> = request.encode_vec().expect("it encodes");
	assert!(encoded.len() <= REQUEST_BYTES, "the widest fetch is {} bytes and the request buffer is {REQUEST_BYTES}", encoded.len());
	assert_eq!(TcpRequest::decode(&encoded).expect("it decodes"), request);
}

#[test]
fn the_longest_name_fits_the_request_buffer() {
	// A DNS name may be 253 bytes, and refusing a legal name because the framing could not hold it
	// would be this contract breaking the protocol it carries.
	let name: alloc::string::String = core::iter::repeat('a').take(MAX_HOST_NAME).collect();
	assert_eq!(name.len(), 253);
	assert!(name.len() + 16 <= REQUEST_BYTES);
}

#[test]
fn every_list_bound_admits_both_families_at_once() {
	// THE POINT OF EACH NUMBER, stated as arithmetic rather than left to a reader. Each is the IPv6
	// maximum plus what IPv4 already holds on the same interface; a bound that copied only the IPv6
	// half would overflow exactly when both families are full.
	assert_eq!(MAX_INTERFACE_ADDRESSES, 16 + 1, "16 IPv6 addresses and the current IPv4 address");
	assert_eq!(MAX_ROUTES, 32 + 2, "32 IPv6 routes, and IPv4's on-link and default routes");
	assert_eq!(MAX_ROUTERS, 8 + 1, "8 IPv6 default routers and the IPv4 gateway");
	assert_eq!(MAX_DNS_SERVERS, 4 + 1, "4 RDNSS records and the current IPv4 server");
	assert_eq!(MAX_NEIGHBORS, 1024 + 64, "the default ARP cache and all 64 IPv6 neighbours");
}

#[test]
fn a_list_past_its_bound_is_refused_by_the_decoder_rather_than_accepted() {
	// THE BOUND IS ON THE WIRE, NOT ONLY IN A COMMENT. A peer that sends one more neighbour than the
	// contract permits must be refused by the generated decoder, or the bound is advice.
	let mut info = widest_info();
	info.neighbors.push(Neighbor { addr: v6(9999), mac: MacAddr::from_octets([0; 6]), scope: scope() });
	let encoded: Vec<u8> = info.encode_vec().expect("an over-bound value still encodes");
	assert_eq!(NetInfo::decode(&encoded), None, "and the decoder refuses it");

	let mut target = OpenTarget { destinations: (0..MAX_OPEN_DESTINATIONS as u16).map(scoped).collect(), port: 80, source: None };
	assert!(OpenTarget::decode(&target.encode_vec().expect("encodes")).is_some(), "exactly the bound is admitted");
	target.destinations.push(scoped(99));
	assert_eq!(OpenTarget::decode(&target.encode_vec().expect("encodes")), None, "one past it is not");
}

/// The snapshot an ordinary boot produces: one IPv4 address, its on-link route, a default route via
/// the gateway, that gateway, the resolver, and one neighbour.
fn ordinary_info() -> NetInfo {
	// THE SHAPE THE SERVICE ACTUALLY SENDS ON AN ORDINARY BOOT, which the widest-case fixture above
	// does not cover: a DIRECT route carries no address at all, and a variant case with no payload
	// encodes differently from one with a payload.
	let v4 = |a: u8, b: u8, c: u8, d: u8| IpAddress::V4(Ipv4Addr { a, b, c, d });
	let info = NetInfo {
		scope: scope(),
		name: alloc::string::String::from("net0"),
		mac: MacAddr::from_octets([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]),
		mtu: 1500,
		addresses: alloc::vec![InterfaceAddress { addr: v4(10, 0, 2, 15), prefix_len: 24, state: AddressState::Preferred, preferred_seconds: u32::MAX, valid_seconds: u32::MAX }],
		routes: alloc::vec![
			RouteEntry { destination: v4(10, 0, 2, 0), prefix_len: 24, scope: scope(), preference: RoutePreference::Medium, lifetime_seconds: u32::MAX, hop: NextHop::Direct },
			RouteEntry { destination: v4(0, 0, 0, 0), prefix_len: 0, scope: scope(), preference: RoutePreference::Medium, lifetime_seconds: u32::MAX, hop: NextHop::Via(v4(10, 0, 2, 2)) },
		],
		routers: alloc::vec![RouterEntry { addr: v4(10, 0, 2, 2), scope: scope(), preference: RoutePreference::Medium, state: Reachability::Reachable, lifetime_seconds: u32::MAX }],
		dns: alloc::vec![DnsServer { addr: v4(10, 0, 2, 3), scope: scope() }],
		neighbors: alloc::vec![Neighbor { addr: v4(10, 0, 2, 2), mac: MacAddr::from_octets([0x52, 0x55, 0x0a, 0x00, 0x02, 0x02]), scope: scope() }],
	};
	info
}

#[test]
fn the_snapshot_a_link_with_no_ipv6_produces_round_trips() {
	// THE SHAPE THE SERVICE ACTUALLY SENDS ON AN ORDINARY BOOT, which the widest-case fixture above
	// does not cover: a DIRECT route carries no address at all, and a variant case with no payload
	// encodes differently from one with a payload.
	let info = ordinary_info();
	let encoded: Vec<u8> = info.encode_vec().expect("it encodes");
	assert_eq!(NetInfo::decode(&encoded), Some(info), "the ordinary snapshot survives the wire");
}

// A service that answers `info` with the ordinary snapshot and refuses everything else, so the
// reply path can be exercised end to end on a host.
struct Stub(NetInfo);

impl network::Service for Stub {
	fn info(&mut self) -> Result<NetInfo, crate::generated::liber::base::v1::Error> {
		Ok(self.0.clone())
	}
	fn resolve(&mut self, _name: alloc::string::String) -> Result<Vec<IpAddress>, crate::generated::liber::base::v1::Error> {
		Err(crate::generated::liber::base::v1::Error::Unsupported)
	}
	fn ping(&mut self, _addr: ScopedAddress) -> Result<PingReply, crate::generated::liber::base::v1::Error> {
		Err(crate::generated::liber::base::v1::Error::Unsupported)
	}
	fn fetch(&mut self, _req: TcpRequest) -> Result<Vec<FetchChunk>, crate::generated::liber::base::v1::Error> {
		Err(crate::generated::liber::base::v1::Error::Unsupported)
	}
	fn connect(&mut self, _target: OpenTarget) -> Result<u64, crate::generated::liber::base::v1::Error> {
		Err(crate::generated::liber::base::v1::Error::Unsupported)
	}
	fn open(&mut self) -> Result<u64, crate::generated::liber::base::v1::Error> {
		Err(crate::generated::liber::base::v1::Error::Unsupported)
	}
	fn listen(&mut self, _req: ListenRequest) -> Result<ListenResult, crate::generated::liber::base::v1::Error> {
		Err(crate::generated::liber::base::v1::Error::Unsupported)
	}
	fn sockets(&mut self) -> Result<Vec<SockInfo>, crate::generated::liber::base::v1::Error> {
		Ok(Vec::new())
	}
	fn sntp(&mut self, _server: ScopedAddress) -> Result<u64, crate::generated::liber::base::v1::Error> {
		Err(crate::generated::liber::base::v1::Error::Unsupported)
	}
	fn capacity(&mut self) -> Result<NetCapacity, crate::generated::liber::base::v1::Error> {
		Ok(NetCapacity { clients: 0, sockets: 0, listeners: 0, connections: 0 })
	}
	fn probe(&mut self, _addr: ScopedAddress, _ttl: u8) -> Result<TraceHop, crate::generated::liber::base::v1::Error> {
		Err(crate::generated::liber::base::v1::Error::Unsupported)
	}
}

#[test]
fn a_snapshot_survives_the_dispatch_and_decode_a_client_actually_performs() {
	// THE ROUND TRIP THE SERVICE PERFORMS, not only the record's own encoding. A reply that encodes
	// and decodes in isolation can still be refused by the client - a correlation id, a trailing
	// byte, a reply buffer one byte short - and the guest is an expensive place to find that out.
	let info = ordinary_info();
	let mut service = Stub(info.clone());
	let corr: u32 = 0x1234_5678;
	let mut request: Vec<u8> = Vec::new();
	request.extend_from_slice(&network::OP_INFO.to_le_bytes());
	request.extend_from_slice(&corr.to_le_bytes());

	let mut out: Vec<u8> = alloc::vec![0u8; REPLY_BYTES];
	let mut request_handles = crate::codec::Handles::new();
	let mut reply_handles = crate::codec::Handles::new();
	let written: usize = network::dispatch(&mut service, &request, &mut request_handles, &mut out, &mut reply_handles).expect("the service answered");

	let mut reader = crate::codec::Reader::with_handle_list(&out[..written], &reply_handles);
	let r = &mut reader;
	assert_eq!(r.u32().expect("a correlation id"), corr);
	assert!(r.tag().expect("an outcome tag"), "the reply is the ok arm");
	let decoded = NetInfo::read(r).expect("the client decodes what the service sent");
	r.finish().expect("and nothing is left over");
	assert_eq!(decoded, info);
}
