//! The concrete font-catalogue clients a dynamic consumer links.
//!
//! WHY A CRATE AND NOT `Client::new(ChannelTransport { .. })`. The generated client is generic over
//! its transport, so instantiating it in a consumer monomorphises the whole codec INTO that
//! consumer - which is the "generic transport residual" the shared-image inventory refuses, and it
//! is refused because a shared image whose consumers each carry their own copy of the transport is
//! a shared image in name only. The monomorphisation happens once, inside `font-proto`, and reaches
//! a consumer through the trampolines this crate declares.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use font_proto::generated::liber::font::v1::{FaceBytes, FaceIdentity, FaceRecord, RescanOutcome, ResolveOutcome};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_font_font_catalogue_list"]
	fn font_list(chan: u64) -> Option<Result<Vec<FaceRecord>, Error>>;
	#[link_name = "liber_channel_liber_font_font_catalogue_resolve_info"]
	fn font_resolve_info(chan: u64, identity: &FaceIdentity) -> Option<Result<FaceBytes, Error>>;
	#[link_name = "liber_channel_liber_font_font_catalogue_resolve_into"]
	fn font_resolve_into(chan: u64, identity: &FaceIdentity, generation: &u64, target: u64) -> Option<Result<ResolveOutcome, Error>>;
	#[link_name = "liber_channel_liber_font_font_catalogue_subscribe"]
	fn font_subscribe(chan: u64) -> Option<u64>;
	#[link_name = "liber_channel_liber_font_font_catalogue_admin_rescan"]
	fn font_rescan(chan: u64) -> Option<Result<RescanOutcome, Error>>;
}

/// The installed faces, and one face's bytes into a buffer the CALLER created and owns.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct FontClient {
	chan: u64,
}

impl FontClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn list(&mut self) -> Option<Result<Vec<FaceRecord>, Error>> {
		unsafe { font_list(self.chan) }
	}

	/// How long a face is, and which publication that answer is about. Allocates nothing.
	#[inline(always)]
	pub fn resolve_info(&mut self, identity: &FaceIdentity) -> Option<Result<FaceBytes, Error>> {
		unsafe { font_resolve_info(self.chan, identity) }
	}

	/// Fill an object the caller created. The handle is CONSUMED by the send, which is why the
	/// caller passes an attenuated DUPLICATE and keeps its own.
	#[inline(always)]
	pub fn resolve_into(&mut self, identity: &FaceIdentity, generation: u64, target: u64) -> Option<Result<ResolveOutcome, Error>> {
		unsafe { font_resolve_into(self.chan, identity, &generation, target) }
	}

	/// The consumer end of a live generation stream.
	#[inline(always)]
	pub fn subscribe(&mut self) -> Option<u64> {
		unsafe { font_subscribe(self.chan) }
	}
}

/// The operator's endpoint: recovery after a dropped watch hint, and nothing else.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct FontAdminClient {
	chan: u64,
}

impl FontAdminClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn rescan(&mut self) -> Option<Result<RescanOutcome, Error>> {
		unsafe { font_rescan(self.chan) }
	}
}
