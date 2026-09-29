//! The concrete Type-C client a dynamic consumer links: TypeCService's `typec` read interface, which the `typec` tool
//! reads.
//!
//! WHY A CRATE AND NOT `Client::new(ChannelTransport { .. })`, for the reason `font-client` gives: the generated client
//! is generic over its transport, so instantiating it in a consumer copies the codec into that consumer. The copy is
//! made once, inside `typec-proto`, and reaches a consumer through the trampolines `typec-client-provider` declares.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use typec_proto::generated::liber::typec::v1::ConnectorSnapshot;

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_typec_typec_connectors"]
	fn read_connectors(chan: u64) -> Option<Result<Vec<ConnectorSnapshot>, Error>>;
}

/// TYPECSERVICE'S READ INTERFACE: every connector it holds.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct TypecClient {
	chan: u64,
}

impl TypecClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn connectors(&mut self) -> Option<Result<Vec<ConnectorSnapshot>, Error>> {
		unsafe { read_connectors(self.chan) }
	}
}
