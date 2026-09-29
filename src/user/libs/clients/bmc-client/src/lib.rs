//! The concrete BMC client a dynamic consumer links: the BMC service's `bmc` interface, which the `bmc` tool reads.
//!
//! WHY A CRATE AND NOT `Client::new(ChannelTransport { .. })`, for the reason `font-client` gives: the generated client
//! is generic over its transport, so instantiating it in a consumer copies the codec into that consumer - the "generic
//! transport residual" the shared-image inventory refuses. The copy is made once, inside `bmc-proto`, and reaches a
//! consumer through the trampolines `bmc-client-provider` declares.
//!
//! EVERY SCALAR CROSSES BY REFERENCE, as the generated implementations take them: the two sides meet only at the linker,
//! where a by-value declaration against a by-reference definition is a mismatch nothing checks.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use bmc_proto::generated::liber::bmc::v1::{BmcSummary, Chassis, Fru, Lan, SelInfo, SelPage, Sensors, Status};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_bmc_bmc_list"]
	fn read_list(chan: u64) -> Option<Result<Vec<BmcSummary>, Error>>;
	#[link_name = "liber_channel_liber_bmc_bmc_sensors"]
	fn read_sensors(chan: u64, name: &str) -> Option<Result<Sensors, Error>>;
	#[link_name = "liber_channel_liber_bmc_bmc_sel_info"]
	fn read_sel_info(chan: u64, name: &str) -> Option<Result<SelInfo, Error>>;
	#[link_name = "liber_channel_liber_bmc_bmc_sel_page"]
	fn read_sel_page(chan: u64, name: &str, first: &u16) -> Option<Result<SelPage, Error>>;
	#[link_name = "liber_channel_liber_bmc_bmc_fru"]
	fn read_fru(chan: u64, name: &str) -> Option<Result<Fru, Error>>;
	#[link_name = "liber_channel_liber_bmc_bmc_chassis"]
	fn read_chassis(chan: u64, name: &str) -> Option<Result<Chassis, Error>>;
	#[link_name = "liber_channel_liber_bmc_bmc_identify"]
	fn read_identify(chan: u64, name: &str, seconds: &u8) -> Option<Result<Status, Error>>;
	#[link_name = "liber_channel_liber_bmc_bmc_lan"]
	fn read_lan(chan: u64, name: &str) -> Option<Result<Lan, Error>>;
}

/// THE BMC SERVICE'S READ INTERFACE: every BMC it holds, and each one's sensors, log, inventory, chassis and LAN.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct BmcClient {
	chan: u64,
}

impl BmcClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn list(&mut self) -> Option<Result<Vec<BmcSummary>, Error>> {
		unsafe { read_list(self.chan) }
	}

	#[inline(always)]
	pub fn sensors(&mut self, name: &str) -> Option<Result<Sensors, Error>> {
		unsafe { read_sensors(self.chan, name) }
	}

	#[inline(always)]
	pub fn sel_info(&mut self, name: &str) -> Option<Result<SelInfo, Error>> {
		unsafe { read_sel_info(self.chan, name) }
	}

	#[inline(always)]
	pub fn sel_page(&mut self, name: &str, first: u16) -> Option<Result<SelPage, Error>> {
		unsafe { read_sel_page(self.chan, name, &first) }
	}

	#[inline(always)]
	pub fn fru(&mut self, name: &str) -> Option<Result<Fru, Error>> {
		unsafe { read_fru(self.chan, name) }
	}

	#[inline(always)]
	pub fn chassis(&mut self, name: &str) -> Option<Result<Chassis, Error>> {
		unsafe { read_chassis(self.chan, name) }
	}

	#[inline(always)]
	pub fn identify(&mut self, name: &str, seconds: u8) -> Option<Result<Status, Error>> {
		unsafe { read_identify(self.chan, name, &seconds) }
	}

	#[inline(always)]
	pub fn lan(&mut self, name: &str) -> Option<Result<Lan, Error>> {
		unsafe { read_lan(self.chan, name) }
	}
}
