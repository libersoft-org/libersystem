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
fn endpoint_wire_is_stable() {
	let sample = Endpoint { addr: Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }, port: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 7, 7, 7, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Endpoint::decode(&bytes).unwrap(), sample);
}
#[test]
fn neighbor_wire_is_stable() {
	let sample = Neighbor { addr: Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }, mac: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 7, 7, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(Neighbor::decode(&bytes).unwrap(), sample);
}
#[test]
fn net_info_wire_is_stable() {
	let sample = NetInfo { addr: Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }, mac: alloc::vec![7], mtu: 7, gateway: Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }, neighbors: alloc::vec![Neighbor { addr: Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }, mac: alloc::vec![7] }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 7, 7, 1, 0, 7, 7, 0, 7, 7, 7, 7, 1, 0, 7, 7, 7, 7, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(NetInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn net_capacity_wire_is_stable() {
	let sample = NetCapacity { clients: 7, sockets: 7, listeners: 7, connections: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
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
fn tcp_request_wire_is_stable() {
	let sample = TcpRequest { ep: Endpoint { addr: Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }, port: 7 }, request: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 7, 7, 7, 0, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(TcpRequest::decode(&bytes).unwrap(), sample);
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
	let sample = SockInfo { local_port: 7, remote: Endpoint { addr: Ipv4Addr { a: 7, b: 7, c: 7, d: 7 }, port: 7 }, state: SockState::Closed };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 7, 7, 7, 7, 7, 0, 0];
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
