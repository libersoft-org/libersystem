// gamepad_fixture - in-guest gamepads, published as a `gamepad` provider over the production wire, and a
// control endpoint for the probe that drives the gamepad tool's gate.
//
// DEVELOPMENT-ONLY. It binds to QEMU's `edu` test function at a pinned address only the gamepad tool's gate
// adds, and its control endpoint is a kind no scope minted for real hardware admits. It is NOT A CLAIM ABOUT
// USB: it delivers the frames a driver would, through the same publisher table the xHCI driver publishes
// through - which is the half the USB proof does not reach.
//
// WHAT IT PLAYS. Every gamepad it attaches has the harness's shape - X, Y, Z and Rz over 0..255, sixteen
// buttons and one hat - under the label the probe gives it, and reports the buttons, hat and axes the probe
// states. Handles are the publisher's, never reused, so a gamepad detached and attached again is a new one.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use driver_protocol::gamepad;
use drivers::common;
use proto::system::{Error, gamepad_fixture};
use rt::*;

const NAME: &[u8] = b"org.libersystem.gamepad-fixture";
const CONTROL_NAME: &[u8] = b"org.libersystem.gamepad-fixture.control";
const PAD_TOKEN: u16 = 0;
const CONTROL_TOKEN: u16 = 1;
const MAX_PADS: usize = 8;

// The publication: the publisher table and the connection it writes to.
struct Fixture {
	publisher: gamepad::Publisher<MAX_PADS>,
	sink: u64,
}

struct Sink(u64);

impl gamepad::Sender for Sink {
	fn send(&mut self, frame: &[u8]) -> bool {
		matches!(try_send_outcome(self.0, frame, 0), SendOutcome::Delivered)
	}
}

impl Fixture {
	// The connection the publication is served on: a new one is owed every gamepad, none is owed nothing.
	fn follow(&mut self, sink: u64) {
		if sink == self.sink {
			return;
		}
		self.sink = sink;
		if sink != 0 {
			self.publisher.connected();
		} else {
			self.publisher.disconnected();
		}
		self.flush();
	}

	fn flush(&mut self) {
		if self.sink != 0 {
			self.publisher.flush(&mut Sink(self.sink));
		}
	}
}

// The harness's gamepad shape, under a label.
fn shape(label: &str) -> Option<gamepad::Shape> {
	let axis = |usage: u32| gamepad::Axis { usage: 0x0001_0000 | usage, minimum: 0, maximum: 255 };
	gamepad::Shape::new(label.as_bytes(), 16, 1, &[axis(0x30), axis(0x31), axis(0x32), axis(0x35)])
}

struct ControlView<'a> {
	fixture: &'a mut Fixture,
}

impl gamepad_fixture::Service for ControlView<'_> {
	fn attach(&mut self, label: String) -> Result<u32, Error> {
		let shape = shape(&label).ok_or(Error::Invalid)?;
		let handle = self.fixture.publisher.attach(shape).ok_or(Error::Exhausted)?;
		self.fixture.flush();
		Ok(handle)
	}

	fn report(&mut self, handle: u32, buttons: u32, hat: u8, axes: Vec<i32>) -> Result<(), Error> {
		if axes.len() != 4 {
			return Err(Error::Invalid);
		}
		let mut values = [0i32; gamepad::MAX_AXES];
		values[..4].copy_from_slice(&axes);
		let state = gamepad::State { buttons, hats: [hat, gamepad::CENTRED], axes: values };
		// A STATE THE SHAPE DOES NOT ADMIT - a button past sixteen, a hat past centred, an axis outside 0..255 is
		// the device's own value and admitted - or a handle this fixture does not hold is refused.
		if !self.fixture.publisher.report(handle, state) {
			return Err(Error::Invalid);
		}
		self.fixture.flush();
		Ok(())
	}

	fn detach(&mut self, handle: u32) -> Result<(), Error> {
		if !self.fixture.publisher.detach(handle) {
			return Err(Error::NotFound);
		}
		self.fixture.flush();
		Ok(())
	}
}

// Serve one control request; false when the probe's connection closed.
fn serve_control(fixture: &mut Fixture, channel: u64, buf: &mut [u8]) -> bool {
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	let mut reply_buf = [0u8; 256];
	let mut reply_handles = wire::Handles::new();
	let mut view = ControlView { fixture };
	if let Some(written) = gamepad_fixture::dispatch(&mut view, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles) {
		send_caps_blocking(channel, &reply_buf[..written], reply_handles.as_slice());
	}
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	true
}

// The gamepad publication's consumer sends nothing; anything it does send is drained. False when it closed.
fn drain_consumer(channel: u64, buf: &mut [u8]) -> bool {
	match try_recv_caps(channel, buf) {
		PolledCaps::Message { handles, .. } => {
			for &handle in handles.as_slice() {
				close(handle);
			}
			true
		}
		PolledCaps::Empty => true,
		PolledCaps::Closed => false,
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, _resources) = common::handshake(bootstrap);
	let (Some((pads, pads_far)), Some((control, control_far))) = (channel(), channel()) else { exit() };
	let retry: u64 = match timer_create() {
		timer if timer > 0 => timer as u64,
		_ => exit(),
	};
	common::online_named(bootstrap, &bind, b"driver.gamepad-fixture: online (gamepads of the harness's shape, for the gamepad tool's gate)", &[(driver_protocol::provider::GAMEPAD, pads_far, NAME), (driver_protocol::provider::FIXTURE_CONTROL, control_far, CONTROL_NAME)]);
	let mut serving = common::Serving::from_offers(&[(PAD_TOKEN, pads), (CONTROL_TOKEN, control)]);
	let mut fixture = Fixture { publisher: gamepad::Publisher::new(), sink: 0 };
	fixture.follow(serving.first_for(PAD_TOKEN));
	let mut buf = alloc::vec![0u8; 512];
	loop {
		// WHAT IS OWED GOES AT EVERY WAKE, and while anything is owed the retry wakes the loop a tick later on a
		// housekeeping wait. ARMED EVERY PASS, because a timer stays expired until it is armed again.
		fixture.flush();
		let owed = fixture.publisher.owes();
		timer_set(retry, if owed { clock() + 1 } else { u64::MAX });
		let ready = common::wait_providers(bootstrap, &bind, &mut serving, &[retry], owed);
		fixture.follow(serving.first_for(PAD_TOKEN));
		match ready {
			None => {
				if common::stop_requested() {
					common::finish_stop(bootstrap, &bind, 0, true);
				}
				exit();
			}
			Some(common::ProviderReady::Connected(_)) | Some(common::ProviderReady::Device(_)) => {}
			Some(common::ProviderReady::Consumer(index)) => {
				let token = serving.token_at(index);
				let chan = serving.at(index);
				let open = if token == CONTROL_TOKEN { serve_control(&mut fixture, chan, &mut buf) } else { drain_consumer(chan, &mut buf) };
				if !open {
					let token = serving.close_at(index);
					fixture.follow(serving.first_for(PAD_TOKEN));
					if !common::disconnected(bootstrap, &bind, token) {
						exit();
					}
				}
			}
		}
	}
}
