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
use bluetooth_proto::generated::liber::bluetooth::v1::{BondedPeer, BroadcastSource, ControllerInfo, DeviceStatus, MediaCommand, PairingProgress, PeerAddress, Profile, PromptReply, ScanHandle, ScanResult};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_controllers"]
	fn read_controllers(chan: u64) -> Option<Result<Vec<ControllerInfo>, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_scan"]
	fn read_scan(chan: u64, controller: &u32, deadline_ms: &u32) -> Option<Result<ScanHandle, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_results"]
	fn read_results(chan: u64, scan: &ScanHandle) -> Option<Result<Vec<ScanResult>, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_scanning"]
	fn read_scanning(chan: u64, scan: &ScanHandle) -> Option<Result<bool, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_cancel"]
	fn read_cancel(chan: u64, scan: &ScanHandle) -> Option<Result<(), Error>>;
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
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_cancel"]
	fn operator_cancel(chan: u64, controller: &u32) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_prompts"]
	fn operator_prompts(chan: u64, controller: &u32) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_answer"]
	fn operator_answer(chan: u64, controller: &u32, prompt: &u32, reply: &PromptReply) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_discoverable"]
	fn operator_discoverable(chan: u64, controller: &u32, seconds: &u32) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_trust"]
	fn operator_trust(chan: u64, controller: &u32, peer: &PeerAddress, profile: &Profile, on: &bool) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_alias"]
	fn operator_alias(chan: u64, controller: &u32, peer: &PeerAddress, alias: &str) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_devices"]
	fn operator_devices(chan: u64, controller: &u32) -> Option<Result<Vec<DeviceStatus>, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_connect"]
	fn operator_connect(chan: u64, controller: &u32, peer: &PeerAddress, profile: &Profile) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_media"]
	fn operator_media(chan: u64, controller: &u32, peer: &PeerAddress, command: &MediaCommand) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_disconnect"]
	fn operator_disconnect(chan: u64, controller: &u32, peer: &PeerAddress, profile: &Profile) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_send"]
	fn operator_send(chan: u64, controller: &u32, peer: &PeerAddress, name: &str, length: &Option<u64>) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_receive"]
	fn operator_receive(chan: u64, controller: &u32, peer: &PeerAddress, max_bytes: &u64) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_connect_pan"]
	fn operator_connect_pan(chan: u64, controller: &u32, peer: &PeerAddress, replace_uplink: &bool) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_broadcast_scan"]
	fn operator_broadcast_scan(chan: u64, controller: &u32, seconds: &u32) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_broadcasts"]
	fn operator_broadcasts(chan: u64, controller: &u32) -> Option<Result<Vec<BroadcastSource>, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_broadcast_play"]
	fn operator_broadcast_play(chan: u64, controller: &u32, broadcast_id: &u32, code: &[u8]) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_broadcast_stop"]
	fn operator_broadcast_stop(chan: u64, controller: &u32) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_object_push_write"]
	fn object_push_write(chan: u64, data: &[u8]) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_object_push_finish"]
	fn object_push_finish(chan: u64) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_bluetooth_object_push_abort"]
	fn object_push_abort(chan: u64) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_bluetooth_bluetooth_operator_pair_legacy"]
	fn operator_pair_legacy(chan: u64, controller: &u32, peer: &PeerAddress) -> Option<Result<(), Error>>;
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

	#[inline(always)]
	pub fn cancel(&mut self, scan: &ScanHandle) -> Option<Result<(), Error>> {
		unsafe { read_cancel(self.chan, scan) }
	}
}

/// The OPERATOR authority: power, pairing and its prompts, the bonds, trust, and profile connections.
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

	#[inline(always)]
	pub fn cancel(&mut self, controller: u32) -> Option<Result<(), Error>> {
		unsafe { operator_cancel(self.chan, &controller) }
	}

	/// The prompt watcher: the stream's consumer end, read with the protocol's `prompts_read`.
	#[inline(always)]
	pub fn prompts(&mut self, controller: u32) -> Option<Result<u64, Error>> {
		unsafe { operator_prompts(self.chan, &controller) }
	}

	#[inline(always)]
	pub fn answer(&mut self, controller: u32, prompt: u32, reply: &PromptReply) -> Option<Result<(), Error>> {
		unsafe { operator_answer(self.chan, &controller, &prompt, reply) }
	}

	#[inline(always)]
	pub fn discoverable(&mut self, controller: u32, seconds: u32) -> Option<Result<(), Error>> {
		unsafe { operator_discoverable(self.chan, &controller, &seconds) }
	}

	#[inline(always)]
	pub fn trust(&mut self, controller: u32, peer: &PeerAddress, profile: Profile, on: bool) -> Option<Result<(), Error>> {
		unsafe { operator_trust(self.chan, &controller, peer, &profile, &on) }
	}

	#[inline(always)]
	pub fn alias(&mut self, controller: u32, peer: &PeerAddress, alias: &str) -> Option<Result<(), Error>> {
		unsafe { operator_alias(self.chan, &controller, peer, alias) }
	}

	#[inline(always)]
	pub fn devices(&mut self, controller: u32) -> Option<Result<Vec<DeviceStatus>, Error>> {
		unsafe { operator_devices(self.chan, &controller) }
	}

	#[inline(always)]
	pub fn connect(&mut self, controller: u32, peer: &PeerAddress, profile: Profile) -> Option<Result<(), Error>> {
		unsafe { operator_connect(self.chan, &controller, peer, &profile) }
	}

	#[inline(always)]
	pub fn media(&mut self, controller: u32, peer: &PeerAddress, command: MediaCommand) -> Option<Result<(), Error>> {
		unsafe { operator_media(self.chan, &controller, peer, &command) }
	}

	#[inline(always)]
	pub fn disconnect(&mut self, controller: u32, peer: &PeerAddress, profile: Profile) -> Option<Result<(), Error>> {
		unsafe { operator_disconnect(self.chan, &controller, peer, &profile) }
	}

	#[inline(always)]
	pub fn pair_legacy(&mut self, controller: u32, peer: &PeerAddress) -> Option<Result<(), Error>> {
		unsafe { operator_pair_legacy(self.chan, &controller, peer) }
	}

	/// Tethering to a peer's network access point, with whether the link may replace a selected uplink.
	#[inline(always)]
	pub fn connect_pan(&mut self, controller: u32, peer: &PeerAddress, replace_uplink: bool) -> Option<Result<(), Error>> {
		unsafe { operator_connect_pan(self.chan, &controller, peer, &replace_uplink) }
	}

	/// LE Audio broadcasts: a scan for their announcements, what it heard, one played, and the one playing stopped.
	#[inline(always)]
	pub fn broadcast_scan(&mut self, controller: u32, seconds: u32) -> Option<Result<(), Error>> {
		unsafe { operator_broadcast_scan(self.chan, &controller, &seconds) }
	}

	#[inline(always)]
	pub fn broadcasts(&mut self, controller: u32) -> Option<Result<Vec<BroadcastSource>, Error>> {
		unsafe { operator_broadcasts(self.chan, &controller) }
	}

	#[inline(always)]
	pub fn broadcast_play(&mut self, controller: u32, broadcast_id: u32, code: &[u8]) -> Option<Result<(), Error>> {
		unsafe { operator_broadcast_play(self.chan, &controller, &broadcast_id, code) }
	}

	#[inline(always)]
	pub fn broadcast_stop(&mut self, controller: u32) -> Option<Result<(), Error>> {
		unsafe { operator_broadcast_stop(self.chan, &controller) }
	}

	/// An object push: its `object-push` channel, written with `ObjectPushClient`.
	#[inline(always)]
	pub fn send(&mut self, controller: u32, peer: &PeerAddress, name: &str, length: Option<u64>) -> Option<Result<u64, Error>> {
		unsafe { operator_send(self.chan, &controller, peer, name, &length) }
	}

	/// A receiver: the stream's consumer end, read with the protocol's `receive_read`.
	#[inline(always)]
	pub fn receive(&mut self, controller: u32, peer: &PeerAddress, max_bytes: u64) -> Option<Result<u64, Error>> {
		unsafe { operator_receive(self.chan, &controller, peer, &max_bytes) }
	}
}

/// AN OBJECT BEING PUSHED, from `BluetoothOperatorClient::send`.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct ObjectPushClient {
	chan: u64,
}

impl ObjectPushClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn write(&mut self, data: &[u8]) -> Option<Result<u64, Error>> {
		unsafe { object_push_write(self.chan, data) }
	}

	#[inline(always)]
	pub fn finish(&mut self) -> Option<Result<u64, Error>> {
		unsafe { object_push_finish(self.chan) }
	}

	#[inline(always)]
	pub fn abort(&mut self) -> Option<Result<(), Error>> {
		unsafe { object_push_abort(self.chan) }
	}
}
