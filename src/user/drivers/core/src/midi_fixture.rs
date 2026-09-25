// midi_fixture - an in-guest MIDI device with two receive endpoints, published as a `midi` provider over the
// production wire, and a control endpoint for the probe that drives the MIDI gate.
//
// DEVELOPMENT-ONLY. It binds to a QEMU test function at a pinned address only the MIDI gate adds, and its
// control endpoint is a kind no scope minted for real hardware admits. It is not a claim about USB MIDI
// transport: it delivers the packets the probe scripts, as a transfer completion would deliver them, and
// decodes nothing - MidiService does that, with the driver library's decoder.
//
// WHAT IT PLAYS. Endpoint 0 has two cables and endpoint 1 has one. A batch is delivered only on an endpoint a
// receiver started, under that receiver's generation, with the host time the fixture sampled as it delivered
// it. The probe can deliver any packets (malformed ones included), flood an endpoint, report input lost, and
// withdraw and republish the device.
//
// AND IT HAS A TRANSMIT ENDPOINT, 2, with two cables, WIRED BACK TO ENDPOINT 0: what a sender puts on it comes
// back as one batch on the receive endpoint, so the MIDI gate sends through its grant and reads what it sent
// through another - a whole round trip with no device. Packets sent while nobody receives on endpoint 0 are
// gone, as on a cable nobody listens to.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use drivers::common;
use proto::system::{Error, MidiBounds, MidiDeviceDirection, MidiDeviceEndpoint, MidiDeviceEvent, MidiFixtureStats, MidiInputLost, MidiOpen, MidiPacketBatch, midi_device, midi_fixture};
use rt::*;

const NAME: &[u8] = b"org.libersystem.midi-fixture";
const CONTROL_NAME: &[u8] = b"org.libersystem.midi-fixture.control";
const MIDI_TOKEN: u16 = 0;
const CONTROL_TOKEN: u16 = 1;
const VERSION: u32 = 1;
const STREAM_DEPTH: u64 = 64;
// Endpoint index and cable count, of the two receive endpoints.
const ENDPOINTS: [(u32, u8); 2] = [(0, 2), (1, 1)];
// The transmit endpoint, its cables, and the receive endpoint its packets come back on.
const TRANSMIT: (u32, u8) = (2, 2);
const LOOPBACK: u32 = 0;

#[derive(Default)]
struct Session {
	chan: u64,
	events: u64,
	event_seq: u32,
}

struct Fixture {
	live: bool,
	token: u16,
	next_token: u16,
	connection: u64,
	session: Session,
	// The receiver generation each endpoint was started under, if any.
	receiving: [Option<u64>; 2],
	stats: MidiFixtureStats,
}

impl Fixture {
	// One event on the stream, without waiting; false when the stream has no room.
	fn event(&mut self, event: &MidiDeviceEvent) -> bool {
		if self.session.events == 0 {
			return false;
		}
		let mut frame = [0u8; 512];
		let mut handles = wire::Handles::new();
		let Some(len) = midi_device::events_frame(self.session.event_seq, event, &mut frame, &mut handles) else { return false };
		if try_send(self.session.events, &frame[..len], 0) {
			self.session.event_seq += 1;
			return true;
		}
		false
	}

	fn deliver(&mut self, endpoint: u32, packets: Vec<u8>) -> Result<(), Error> {
		let generation = self.receiving.get(endpoint as usize).copied().flatten().ok_or(Error::NotFound)?;
		let count = (packets.len() / 4) as u32;
		// SAMPLED AS THE BATCH BECOMES AVAILABLE, before anything queues it.
		let batch = MidiPacketBatch { endpoint, receiver_generation: generation, received_ns: clock_ns(), packets };
		if !self.event(&MidiDeviceEvent::Batch(batch)) {
			return Err(Error::Again);
		}
		self.stats.batches += 1;
		self.stats.packets += count;
		Ok(())
	}

	fn departed(&mut self) {
		if self.session.events != 0 {
			close(self.session.events);
		}
		self.session = Session::default();
		self.receiving = [None; 2];
		self.stats.receiving = 0;
	}
}

struct DeviceView<'a> {
	fixture: &'a mut Fixture,
}

impl midi_device::Service for DeviceView<'_> {
	fn open(&mut self, version: u32) -> Result<MidiOpen, Error> {
		if version != VERSION {
			return Err(Error::Unsupported);
		}
		self.fixture.connection += 1;
		Ok(MidiOpen { connection_generation: self.fixture.connection, bounds: MidiBounds { version: VERSION, max_packets: 64 } })
	}

	fn endpoints(&mut self) -> Result<Vec<MidiDeviceEndpoint>, Error> {
		let mut endpoints: Vec<MidiDeviceEndpoint> = ENDPOINTS.iter().map(|&(index, cables)| MidiDeviceEndpoint { index, name: alloc::format!("fixture port {index}"), cables, direction: MidiDeviceDirection::Receive }).collect();
		endpoints.push(MidiDeviceEndpoint { index: TRANSMIT.0, name: alloc::format!("fixture port {} out", TRANSMIT.0), cables: TRANSMIT.1, direction: MidiDeviceDirection::Transmit });
		Ok(endpoints)
	}

	fn start(&mut self, endpoint: u32, receiver_generation: u64) -> Result<(), Error> {
		let slot = self.fixture.receiving.get_mut(endpoint as usize).ok_or(Error::NotFound)?;
		*slot = Some(receiver_generation);
		self.fixture.stats.receiving = self.fixture.receiving.iter().filter(|slot| slot.is_some()).count() as u8;
		Ok(())
	}

	fn stop(&mut self, endpoint: u32, receiver_generation: u64) -> Result<(), Error> {
		let slot = self.fixture.receiving.get_mut(endpoint as usize).ok_or(Error::NotFound)?;
		// A stop for a generation that is not the current one stops nothing.
		if *slot == Some(receiver_generation) {
			*slot = None;
		}
		self.fixture.stats.receiving = self.fixture.receiving.iter().filter(|slot| slot.is_some()).count() as u8;
		Ok(())
	}

	fn events(&mut self) -> Vec<MidiDeviceEvent> {
		Vec::new()
	}

	// TAKEN WHOLE, AND PLAYED BACK on the receive endpoint as one batch - or lost, if nobody is receiving there.
	fn send(&mut self, endpoint: u32, packets: Vec<u8>) -> Result<(), Error> {
		if ENDPOINTS.iter().any(|&(index, _)| index == endpoint) {
			return Err(Error::Invalid);
		}
		if endpoint != TRANSMIT.0 {
			return Err(Error::NotFound);
		}
		if packets.is_empty() || packets.len() % 4 != 0 || packets.len() > 256 || packets.chunks_exact(4).any(|packet| packet[0] >> 4 >= TRANSMIT.1) {
			return Err(Error::Invalid);
		}
		self.fixture.stats.sent += (packets.len() / 4) as u32;
		if self.fixture.receiving[LOOPBACK as usize].is_some() {
			self.fixture.deliver(LOOPBACK, packets)?;
		}
		Ok(())
	}
}

struct ControlView<'a> {
	fixture: &'a mut Fixture,
	serving: &'a mut common::Serving,
	bootstrap: u64,
	bind: &'a common::Bind,
}

impl midi_fixture::Service for ControlView<'_> {
	fn inject(&mut self, endpoint: u32, packets: Vec<u8>) -> Result<(), Error> {
		self.fixture.deliver(endpoint, packets)
	}

	// AS FAST AS THE SERVICE TAKES THEM: every batch a full 64 note-ons, until the count or the stream's room
	// runs out.
	fn flood(&mut self, endpoint: u32, batches: u32) -> Result<(), Error> {
		let batch: Vec<u8> = (0..64u8).flat_map(|note| [0x09, 0x90, note, 0x40]).collect();
		for _ in 0..batches {
			self.fixture.deliver(endpoint, batch.clone())?;
		}
		Ok(())
	}

	fn lose(&mut self, endpoint: u32) -> Result<(), Error> {
		let generation = self.fixture.receiving.get(endpoint as usize).copied().flatten().ok_or(Error::NotFound)?;
		if !self.fixture.event(&MidiDeviceEvent::Lost(MidiInputLost { endpoint, receiver_generation: generation })) {
			return Err(Error::Again);
		}
		Ok(())
	}

	fn withdraw(&mut self) -> Result<(), Error> {
		let fixture = &mut *self.fixture;
		if !fixture.live {
			return Err(Error::NotFound);
		}
		fixture.live = false;
		fixture.departed();
		if !common::withdraw(self.bootstrap, self.bind, fixture.token) {
			return Err(Error::Io);
		}
		Ok(())
	}

	fn republish(&mut self) -> Result<(), Error> {
		let fixture = &mut *self.fixture;
		if fixture.live {
			return Err(Error::Invalid);
		}
		let token = fixture.next_token;
		let Some((near, far)) = channel() else { return Err(Error::Exhausted) };
		if !self.serving.publish(token, near) {
			close(near);
			close(far);
			return Err(Error::Exhausted);
		}
		if !common::offer_named(self.bootstrap, self.bind, driver_protocol::provider::MIDI, token, NAME, far) {
			return Err(Error::Io);
		}
		fixture.next_token = token + 1;
		fixture.token = token;
		fixture.live = true;
		Ok(())
	}

	fn stats(&mut self) -> Result<MidiFixtureStats, Error> {
		Ok(self.fixture.stats.clone())
	}
}

fn serve(fixture: &mut Fixture, serving: &mut common::Serving, bootstrap: u64, bind: &common::Bind, token: u16, channel: u64, buf: &mut [u8]) -> bool {
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	if len < 2 {
		return true;
	}
	let mut reply_buf = [0u8; 1024];
	let mut reply_handles = wire::Handles::new();
	if token == CONTROL_TOKEN {
		let mut view = ControlView { fixture: &mut *fixture, serving: &mut *serving, bootstrap, bind };
		if let Some(written) = midi_fixture::dispatch(&mut view, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles) {
			send_caps_blocking(channel, &reply_buf[..written], reply_handles.as_slice());
		}
		return true;
	}
	if token != fixture.token || !fixture.live {
		return true;
	}
	if fixture.session.chan != channel {
		fixture.departed();
		fixture.session.chan = channel;
	}
	if u16::from_le_bytes([buf[0], buf[1]]) == midi_device::OP_EVENTS {
		let mut view = DeviceView { fixture: &mut *fixture };
		let Some((corr, _)) = midi_device::events_open(&mut view, &buf[..len], &mut handles) else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		if fixture.session.events != 0 {
			close(fixture.session.events);
		}
		fixture.session.events = producer;
		fixture.session.event_seq = 0;
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		return true;
	}
	let mut view = DeviceView { fixture: &mut *fixture };
	if let Some(written) = midi_device::dispatch(&mut view, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles) {
		send_caps_blocking(channel, &reply_buf[..written], reply_handles.as_slice());
	}
	true
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, _resources) = common::handshake(bootstrap);
	let (Some((midi, midi_far)), Some((control, control_far))) = (channel(), channel()) else { exit() };
	common::online_named(bootstrap, &bind, b"driver.midi-fixture: online (a MIDI device with two receive endpoints and a transmit one wired back, for the MIDI gate)", &[(driver_protocol::provider::MIDI, midi_far, NAME), (driver_protocol::provider::FIXTURE_CONTROL, control_far, CONTROL_NAME)]);
	let mut serving = common::Serving::from_offers(&[(MIDI_TOKEN, midi), (CONTROL_TOKEN, control)]);
	let mut fixture = Fixture { live: true, token: MIDI_TOKEN, next_token: CONTROL_TOKEN + 1, connection: 0, session: Session::default(), receiving: [None; 2], stats: MidiFixtureStats { batches: 0, packets: 0, receiving: 0, sent: 0 } };
	let mut buf = alloc::vec![0u8; 2048];
	loop {
		match common::wait_providers_or_answer(bootstrap, &bind, &mut serving, &[]) {
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
				if !serve(&mut fixture, &mut serving, bootstrap, &bind, token, chan, &mut buf) {
					let token = serving.close_at(index);
					if token == fixture.token && chan == fixture.session.chan {
						fixture.departed();
					}
					if !common::disconnected(bootstrap, &bind, token) {
						exit();
					}
				}
			}
		}
	}
}
