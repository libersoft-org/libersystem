//! The concrete administrative-request clients a dynamic consumer links: the request connection
//! PermissionManager mints for one launch, and the grant AdminService hands back once a person confirmed.
//!
//! WHY A CRATE AND NOT `Client::new(ChannelTransport { .. })`, for the reason `font-client` gives: the
//! generated client is generic over its transport, so instantiating it in a consumer copies the codec into
//! that consumer - the "generic transport residual" the shared-image inventory refuses. The copy is made
//! once, inside `admin-proto`, and reaches a consumer through the trampolines this crate declares.
//!
//! EVERY SCALAR CROSSES BY REFERENCE, as the generated implementations take them: the two sides meet only at
//! the linker, where a by-value declaration against a by-reference definition is a mismatch nothing checks.

#![no_std]

use admin_proto::generated::liber::admin::v1::{AdminAnswer, AdminRequestArgs, AdminResult};
use base_proto::generated::liber::base::v1::Error;

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_admin_admin_request_request"]
	fn request_request(chan: u64, args: &AdminRequestArgs, payload: &u64) -> Option<Result<AdminAnswer, Error>>;
	#[link_name = "liber_channel_liber_admin_admin_authority_execute"]
	fn authority_execute(chan: u64) -> Option<Result<AdminResult, Error>>;
}

/// A REQUEST CONNECTION: one question at a time to the trusted administrative path, answered once a person
/// has seen it on the protected screen - granted, or declined.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct AdminRequestClient {
	chan: u64,
}

impl AdminRequestClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn request(&mut self, args: &AdminRequestArgs, payload: u64) -> Option<Result<AdminAnswer, Error>> {
		unsafe { request_request(self.chan, args, &payload) }
	}
}

/// A GRANT: the one attempt at the operation a person confirmed.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct AdminAuthorityClient {
	chan: u64,
}

impl AdminAuthorityClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn execute(&mut self) -> Option<Result<AdminResult, Error>> {
		unsafe { authority_execute(self.chan) }
	}
}
