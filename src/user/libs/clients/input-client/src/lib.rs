//! The concrete input client a dynamic consumer links: InputService's `input` interface as the `gamepad` tool reads it.
//!
//! WHY A CRATE AND NOT `Client::new(ChannelTransport { .. })`, for the reason `font-client` gives: the generated client
//! is generic over its transport, so instantiating it in a consumer copies the codec into that consumer. The copy is
//! made once, inside `input-proto`, and reaches a consumer through the trampoline `input-client-provider` declares.

#![no_std]

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_input_input_observe_gamepads"]
	fn observe_gamepads(chan: u64) -> Option<u64>;
}

/// INPUTSERVICE'S `input` INTERFACE, as a gamepad console reads it: the stream of every gamepad's arrivals, states and
/// departures.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct InputClient {
	chan: u64,
}

impl InputClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn observe_gamepads(&mut self) -> Option<u64> {
		unsafe { observe_gamepads(self.chan) }
	}
}
