#![no_std]

extern crate alloc;
use alloc::vec::Vec;

use proto::codec::Buffer;
use proto::system::{Endpoint, Error, Ipv4Addr, NetCapacity, NetInfo, PingReply, SockInfo, TcpRequest};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_network_network_info"]
	fn network_info(chan: u64) -> Option<Result<NetInfo, Error>>;
	#[link_name = "liber_channel_liber_network_network_resolve"]
	fn network_resolve(chan: u64, name: &str) -> Option<Result<Ipv4Addr, Error>>;
	#[link_name = "liber_channel_liber_network_network_ping"]
	fn network_ping(chan: u64, addr: &Ipv4Addr) -> Option<Result<PingReply, Error>>;
	fn network_fetch(chan: u64, request: &TcpRequest) -> Option<Result<Vec<u8>, Error>>;
	#[link_name = "liber_channel_liber_network_network_connect"]
	fn network_connect(chan: u64, endpoint: &Endpoint) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_network_network_open"]
	fn network_open(chan: u64) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_network_network_listen"]
	fn network_listen(chan: u64, port: &u16) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_network_network_sockets"]
	fn network_sockets(chan: u64) -> Option<Result<Vec<SockInfo>, Error>>;
	#[link_name = "liber_channel_liber_network_network_sntp"]
	fn network_sntp(chan: u64, server: &Ipv4Addr) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_network_network_capacity"]
	fn network_capacity(chan: u64) -> Option<Result<NetCapacity, Error>>;
	#[link_name = "liber_channel_liber_network_socket_send"]
	fn socket_send(chan: u64, data: &Buffer) -> Option<Result<u32, Error>>;
	#[link_name = "liber_channel_liber_network_socket_recv"]
	fn socket_recv(chan: u64) -> Option<u64>;
	#[link_name = "liber_channel_liber_network_socket_close"]
	fn socket_close(chan: u64) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_network_listener_accept"]
	fn listener_accept(chan: u64) -> Option<Result<u64, Error>>;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct NetworkClient {
	chan: u64,
}

impl NetworkClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}
	#[inline(always)]
	pub fn info(&mut self) -> Option<Result<NetInfo, Error>> {
		unsafe { network_info(self.chan) }
	}
	#[inline(always)]
	pub fn resolve(&mut self, name: &str) -> Option<Result<Ipv4Addr, Error>> {
		unsafe { network_resolve(self.chan, name) }
	}
	#[inline(always)]
	pub fn ping(&mut self, addr: &Ipv4Addr) -> Option<Result<PingReply, Error>> {
		unsafe { network_ping(self.chan, addr) }
	}
	#[inline(always)]
	pub fn fetch(&mut self, request: &TcpRequest) -> Option<Result<Vec<u8>, Error>> {
		unsafe { network_fetch(self.chan, request) }
	}
	#[inline(always)]
	pub fn connect(&mut self, endpoint: &Endpoint) -> Option<Result<u64, Error>> {
		unsafe { network_connect(self.chan, endpoint) }
	}
	#[inline(always)]
	pub fn open(&mut self) -> Option<Result<u64, Error>> {
		unsafe { network_open(self.chan) }
	}
	#[inline(always)]
	pub fn listen(&mut self, port: &u16) -> Option<Result<u64, Error>> {
		unsafe { network_listen(self.chan, port) }
	}
	#[inline(always)]
	pub fn sockets(&mut self) -> Option<Result<Vec<SockInfo>, Error>> {
		unsafe { network_sockets(self.chan) }
	}
	#[inline(always)]
	pub fn sntp(&mut self, server: &Ipv4Addr) -> Option<Result<u64, Error>> {
		unsafe { network_sntp(self.chan, server) }
	}
	#[inline(always)]
	pub fn capacity(&mut self) -> Option<Result<NetCapacity, Error>> {
		unsafe { network_capacity(self.chan) }
	}
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct SocketClient {
	chan: u64,
}

impl SocketClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn send(&mut self, data: &Buffer) -> Option<Result<u32, Error>> {
		unsafe { socket_send(self.chan, data) }
	}

	#[inline(always)]
	pub fn recv(&mut self) -> Option<u64> {
		unsafe { socket_recv(self.chan) }
	}

	#[inline(always)]
	pub fn close(&mut self) -> Option<Result<(), Error>> {
		unsafe { socket_close(self.chan) }
	}
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct ListenerClient {
	chan: u64,
}

impl ListenerClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn accept(&mut self) -> Option<Result<u64, Error>> {
		unsafe { listener_accept(self.chan) }
	}
}
