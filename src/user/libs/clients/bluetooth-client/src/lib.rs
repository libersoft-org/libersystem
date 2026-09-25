//! The concrete Bluetooth clients a dynamic consumer links: the read interface and the operator one.
//!
//! WHY A CRATE AND NOT `Client::new(ChannelTransport { .. })`, for the reason `font-client` gives: the
//! generated client is generic over its transport, so instantiating it in a consumer copies the codec into
//! that consumer - the "generic transport residual" the shared-image inventory refuses. The copy is made
//! once, inside `bluetooth-proto`, and reaches a consumer through the trampolines this crate declares.
//!
//! EVERY SCALAR CROSSES BY REFERENCE, as the generated implementations take them: the two sides meet only at
//! the linker, where a by-value declaration against a by-reference definition is a mismatch nothing checks.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use bluetooth_proto::generated::liber::bluetooth::v1::{BondedPeer, ControllerInfo, PairingProgress, PeerAddress, ScanHandle, ScanResult};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_controllers"]
	fn read_controllers(chan: u64) -> Option<Result<Vec<ControllerInfo>, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_scan"]
	fn read_scan(chan: u64, controller: &u32, deadline_ms: &u32) -> Option<Result<ScanHandle, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_results"]
	fn read_results(chan: u64, scan: &ScanHandle) -> Option<Result<Vec<ScanResult>, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_scanning"]
	fn read_scanning(chan: u64, scan: &ScanHandle) -> Option<Result<bool, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_power"]
	fn operator_power(chan: u64, controller: &u32, on: &bool) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_pair"]
	fn operator_pair(chan: u64, controller: &u32, peer: &PeerAddress) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_progress"]
	fn operator_progress(chan: u64, controller: &u32) -> Option<Result<PairingProgress, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_bonded"]
	fn operator_bonded(chan: u64, controller: &u32) -> Option<Result<Vec<BondedPeer>, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_forget"]
	fn operator_forget(chan: u64, controller: &u32, peer: &PeerAddress) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_enable"]
	fn operator_enable(chan: u64, controller: &u32, peer: &PeerAddress, on: &bool) -> Option<Result<(), Error>>;
}

/// The READ authority: the controllers, and a scan with its results.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct BluetoothClient {
	chan: u64,
}

impl BluetoothClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn controllers(&mut self) -> Option<Result<Vec<ControllerInfo>, Error>> {
		unsafe { read_controllers(self.chan) }
	}

	#[inline(always)]
	pub fn scan(&mut self, controller: u32, deadline_ms: u32) -> Option<Result<ScanHandle, Error>> {
		unsafe { read_scan(self.chan, &controller, &deadline_ms) }
	}

	#[inline(always)]
	pub fn results(&mut self, scan: &ScanHandle) -> Option<Result<Vec<ScanResult>, Error>> {
		unsafe { read_results(self.chan, scan) }
	}

	#[inline(always)]
	pub fn scanning(&mut self, scan: &ScanHandle) -> Option<Result<bool, Error>> {
		unsafe { read_scanning(self.chan, scan) }
	}
}

/// The OPERATOR authority: power, pairing and the bonds.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct BluetoothOperatorClient {
	chan: u64,
}

impl BluetoothOperatorClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn power(&mut self, controller: u32, on: bool) -> Option<Result<(), Error>> {
		unsafe { operator_power(self.chan, &controller, &on) }
	}

	#[inline(always)]
	pub fn pair(&mut self, controller: u32, peer: &PeerAddress) -> Option<Result<(), Error>> {
		unsafe { operator_pair(self.chan, &controller, peer) }
	}

	#[inline(always)]
	pub fn progress(&mut self, controller: u32) -> Option<Result<PairingProgress, Error>> {
		unsafe { operator_progress(self.chan, &controller) }
	}

	#[inline(always)]
	pub fn bonded(&mut self, controller: u32) -> Option<Result<Vec<BondedPeer>, Error>> {
		unsafe { operator_bonded(self.chan, &controller) }
	}

	#[inline(always)]
	pub fn forget(&mut self, controller: u32, peer: &PeerAddress) -> Option<Result<(), Error>> {
		unsafe { operator_forget(self.chan, &controller, peer) }
	}

	#[inline(always)]
	pub fn enable(&mut self, controller: u32, peer: &PeerAddress, on: bool) -> Option<Result<(), Error>> {
		unsafe { operator_enable(self.chan, &controller, peer, &on) }
	}
}
