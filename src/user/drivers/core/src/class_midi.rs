// The USB MIDI class side of driver.xhci: a MIDI 1.0 device on the bus, published as the `midi` provider
// MidiService drives over `liber:midi-device`.
//
// A PROVIDER MOVES PACKETS; THE SERVICE DECODES. What arrives on the receive endpoint is handed on as the
// transfer completed it - at most sixty-four four-byte event packets to a batch, with the host time the
// completion was taken off the ring - and MidiService decodes it with the driver library's staged decoder. A
// malformed packet is the decoder's to name; nothing here reads a status byte.
//
// RECEIVING IS STARTED, NOT STANDING. The receive is posted when the service starts the endpoint under a
// receiver generation, and abandoned when it stops it, so what a device sent before anybody listened is not
// delivered to whoever starts listening later under a generation it was never sent for.
//
// A STREAM THAT IS FULL LOSES INPUT AND SAYS SO. The loop that drives the whole bus cannot wait on one
// consumer, so a batch that does not fit is dropped and the next thing the stream carries is `lost` for that
// receiver - which is the contract's word for input the provider could not deliver.
//
// SENDING IS ONE TRANSFER AT A TIME, ANSWERED WHEN IT COMPLETES. The service's packets go out on the same
// setting's OUT endpoint as one bulk transfer, and the answer to `send` waits for its completion - so a device
// that is slow to take them slows the sender, and nothing queues in between. A second send while one is out is
// `again`; a stall is cleared and the send it cut is a failure; a device that leaves fails the one in flight.

use alloc::vec::Vec;
use proto::system::{Error, MidiBounds, MidiDeviceDirection, MidiDeviceEndpoint, MidiDeviceEvent, MidiInputLost, MidiOpen, MidiPacketBatch, midi_device};
use rt::*;

use crate::classes::{self, Module, Pipe};
use crate::usb_hid::Hids;
use crate::{KIND_MIDI, UsbDevice, Xhci};
use drivers::common;
use drivers::usb_class::ClassKind;
use drivers::usb_midi::{self, Binding};

const VERSION: u32 = 1;
// A batch is at most sixty-four packets, and one receive is one batch.
const BATCH_BYTES: u32 = (usb_midi::MAX_PACKETS * usb_midi::PACKET) as u32;
const STREAM_DEPTH: u64 = 64;
// THE LARGEST REQUEST IS A FULL `send`: the header, the endpoint, the length and sixty-four packets. A buffer
// sized for the control calls alone left that request unread in the channel, answered by nothing.
const REQUEST_BYTES: usize = 512;

pub struct Midi {
	dev: UsbDevice,
	binding: Binding,
	input: Pipe,
	connection: u64,
	consumer: u64,
	// The receiver generation endpoint 0 was started under, if it was.
	receiving: Option<u64>,
	stream: u64,
	stream_seq: u32,
	// Input was dropped since the stream last had room, for this receiver generation.
	lost: Option<u64>,
	buf: Vec<u8>,
	// The transmit pipe and the cables it carries, when the device has one.
	output: Option<(Pipe, u8)>,
	// The send in flight: the connection and correlation its answer goes to.
	sending: Option<(u64, u32)>,
}

/// Bring a bound MIDI device's receive pipe up and select its setting. The device comes back on failure.
///
/// # Safety
/// `dev` is an addressed device this controller owns.
pub unsafe fn configure(hc: &mut Xhci, mut dev: UsbDevice, binding: Binding) -> Result<Midi, UsbDevice> {
	unsafe {
		let Some(mut input) = Pipe::new(dev.slot, dev.speed, &binding.input) else {
			print(b"driver.xhci: a MIDI device is on the bus and no pages were left for its pipe\n");
			return Err(dev);
		};
		let mut output = match binding.output {
			Some((endpoint, cables)) => match Pipe::new(dev.slot, dev.speed, &endpoint) {
				Some(pipe) => Some((pipe, cables)),
				None => {
					input.release(hc);
					print(b"driver.xhci: a MIDI device is on the bus and no pages were left for its pipes\n");
					return Err(dev);
				}
			},
			None => None,
		};
		let configured = match output.as_ref() {
			Some((pipe, _)) => classes::configure_pipes(hc, &mut dev, &[&input, pipe], 0),
			None => classes::configure_pipes(hc, &mut dev, &[&input], 0),
		};
		if !configured || !classes::select(hc, &mut dev, binding.config_value, binding.interface, binding.alternate) {
			input.release(hc);
			if let Some((pipe, _)) = output.as_mut() {
				pipe.release(hc);
			}
			print(b"driver.xhci: a MIDI device's endpoints or its setting were refused\n");
			return Err(dev);
		}
		let mut line: common::Bounded<96> = common::Bounded::new();
		line.push(b"driver.xhci: MIDI device bound - MIDI 1.0, ");
		line.decimal(binding.cables as u64);
		line.push(b" cable(s) in, ");
		line.decimal(output.as_ref().map_or(0, |(_, cables)| *cables) as u64);
		line.push(b" out\n");
		print(line.as_bytes());
		Ok(Midi { dev, binding, input, connection: 0, consumer: 0, receiving: None, stream: 0, stream_seq: 0, lost: None, buf: alloc::vec![0u8; REQUEST_BYTES], output, sending: None })
	}
}

impl Midi {
	fn send(&mut self, event: &MidiDeviceEvent) -> bool {
		if self.stream == 0 {
			return false;
		}
		let mut frame = [0u8; 512];
		let mut handles = wire::Handles::new();
		let Some(len) = midi_device::events_frame(self.stream_seq, event, &mut frame, &mut handles) else { return false };
		if try_send(self.stream, &frame[..len], 0) {
			self.stream_seq += 1;
			return true;
		}
		false
	}

	// One batch, or the loss of it - and a loss not yet reported goes first.
	fn deliver(&mut self, generation: u64, packets: Vec<u8>, received_ns: u64) {
		if let Some(lost) = self.lost {
			if !self.send(&MidiDeviceEvent::Lost(MidiInputLost { endpoint: 0, receiver_generation: lost })) {
				return;
			}
			self.lost = None;
		}
		if !self.send(&MidiDeviceEvent::Batch(MidiPacketBatch { endpoint: 0, receiver_generation: generation, received_ns, packets })) {
			self.lost = Some(generation);
		}
	}

	fn stop_receiving(&mut self, hc: &mut Xhci, hids: &mut Hids) {
		self.receiving = None;
		let _ = classes::abandon(hc, hids, &mut self.input);
	}

	// The send in flight ends: answered with `result`, if anybody is waiting for it.
	fn answer_send(&mut self, result: Result<(), Error>) {
		if let Some((chan, corr)) = self.sending.take() {
			let _ = classes::answer_unit(chan, corr, result);
		}
	}

	fn close_stream(&mut self) {
		if self.stream != 0 {
			close(self.stream);
			self.stream = 0;
		}
		self.stream_seq = 0;
		self.lost = None;
	}
}

struct View<'a> {
	midi: &'a mut Midi,
	hc: &'a mut Xhci,
	hids: &'a mut Hids,
	chan: u64,
	// A send was put on the wire: its answer waits for the transfer, not for this dispatch.
	posted: bool,
}

impl midi_device::Service for View<'_> {
	fn open(&mut self, version: u32) -> Result<MidiOpen, Error> {
		if version != VERSION {
			return Err(Error::Unsupported);
		}
		if self.midi.consumer != 0 && self.midi.consumer != self.chan {
			return Err(Error::Invalid);
		}
		self.midi.consumer = self.chan;
		self.midi.connection += 1;
		Ok(MidiOpen { connection_generation: self.midi.connection, bounds: MidiBounds { version: VERSION, max_packets: usb_midi::MAX_PACKETS as u16 } })
	}

	fn endpoints(&mut self) -> Result<Vec<MidiDeviceEndpoint>, Error> {
		if self.midi.consumer != self.chan {
			return Err(Error::Invalid);
		}
		let mut endpoints = alloc::vec![MidiDeviceEndpoint { index: 0, name: alloc::string::String::from("USB MIDI in"), cables: self.midi.binding.cables, direction: MidiDeviceDirection::Receive }];
		if let Some((_, cables)) = self.midi.output {
			endpoints.push(MidiDeviceEndpoint { index: 1, name: alloc::string::String::from("USB MIDI out"), cables, direction: MidiDeviceDirection::Transmit });
		}
		Ok(endpoints)
	}

	fn start(&mut self, endpoint: u32, receiver_generation: u64) -> Result<(), Error> {
		if self.midi.consumer != self.chan {
			return Err(Error::Invalid);
		}
		if endpoint != 0 {
			return Err(Error::NotFound);
		}
		self.midi.receiving = Some(receiver_generation);
		if !self.midi.input.busy && !self.midi.input.post(self.hc, BATCH_BYTES) {
			return Err(Error::Io);
		}
		Ok(())
	}

	fn stop(&mut self, endpoint: u32, receiver_generation: u64) -> Result<(), Error> {
		if self.midi.consumer != self.chan {
			return Err(Error::Invalid);
		}
		if endpoint != 0 {
			return Err(Error::NotFound);
		}
		// A STOP FOR A GENERATION THAT IS NOT THE CURRENT ONE stops nothing.
		if self.midi.receiving == Some(receiver_generation) {
			self.midi.stop_receiving(self.hc, self.hids);
		}
		Ok(())
	}

	fn events(&mut self) -> Vec<MidiDeviceEvent> {
		Vec::new()
	}

	fn send(&mut self, endpoint: u32, packets: Vec<u8>) -> Result<(), Error> {
		if self.midi.consumer != self.chan {
			return Err(Error::Invalid);
		}
		let (pipe, cables) = match (endpoint, self.midi.output.as_mut()) {
			// The receive endpoint is not one packets go OUT on.
			(0, _) => return Err(Error::Invalid),
			(1, Some((pipe, cables))) => (pipe, *cables),
			_ => return Err(Error::NotFound),
		};
		if packets.is_empty() || packets.len() % usb_midi::PACKET != 0 || packets.len() > BATCH_BYTES as usize || packets.chunks_exact(usb_midi::PACKET).any(|packet| packet[0] >> 4 >= cables) {
			return Err(Error::Invalid);
		}
		if self.midi.sending.is_some() || pipe.busy {
			return Err(Error::Again);
		}
		if !pipe.fill(&packets) || !pipe.post(self.hc, packets.len() as u32) {
			return Err(Error::Io);
		}
		self.posted = true;
		Ok(())
	}
}

impl Module for Midi {
	fn kind(&self) -> ClassKind {
		ClassKind::Midi
	}

	fn inventory(&self) -> u8 {
		KIND_MIDI
	}

	fn provider(&self) -> u16 {
		driver_protocol::provider::MIDI
	}

	fn name(&self) -> &'static [u8] {
		driver_protocol::provider::USB_MIDI_NAME
	}

	fn device(&self) -> &UsbDevice {
		&self.dev
	}

	fn device_mut(&mut self) -> &mut UsbDevice {
		&mut self.dev
	}

	fn start(&mut self, _hc: &mut Xhci) {}

	fn serve(&mut self, hc: &mut Xhci, hids: &mut Hids, chan: u64, _bootstrap: u64, _bind: &common::Bind) -> bool {
		let mut buf = core::mem::take(&mut self.buf);
		let polled = try_recv_caps(chan, &mut buf);
		self.buf = buf;
		let (len, mut handles) = match polled {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return true,
			PolledCaps::Closed => return false,
		};
		let request = self.buf[..len].to_vec();
		if classes::correlation(&request).map(|(op, _)| op) == Some(midi_device::OP_EVENTS) {
			let mut view = View { midi: self, hc, hids, chan, posted: false };
			let Some((corr, _)) = midi_device::events_open(&mut view, &request, &mut handles) else { return true };
			let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
			self.close_stream();
			self.stream = producer;
			send_caps_blocking(chan, &corr.to_le_bytes(), &[consumer]);
			return true;
		}
		let mut reply = [0u8; 512];
		let mut reply_handles = wire::Handles::new();
		let mut view = View { midi: self, hc, hids, chan, posted: false };
		let written = midi_device::dispatch(&mut view, &request, &mut handles, &mut reply, &mut reply_handles);
		let posted = view.posted;
		if posted {
			// THE ANSWER IS THE TRANSFER'S: it goes out when the completion does, from `absorb`.
			if let Some((_, corr)) = classes::correlation(&request) {
				self.sending = Some((chan, corr));
			}
		} else if let Some(written) = written {
			send_caps_blocking(chan, &reply[..written], reply_handles.as_slice());
		}
		for &handle in handles.as_slice() {
			close(handle);
		}
		true
	}

	fn departed(&mut self, hc: &mut Xhci, hids: &mut Hids, chan: u64) {
		if self.consumer != chan {
			return;
		}
		self.consumer = 0;
		self.stop_receiving(hc, hids);
		self.close_stream();
		// A SEND THE CONSUMER WILL NEVER HEAR ABOUT IS TAKEN BACK, so the pipe is free for the next one.
		self.sending = None;
		if let Some((pipe, _)) = self.output.as_mut()
			&& pipe.busy
		{
			let _ = classes::abandon(hc, hids, pipe);
		}
	}

	fn absorb(&mut self, hc: &mut Xhci, hids: &mut Hids, _pointer: u64, status: u32, control: u32) -> bool {
		if let Some((pipe, _)) = self.output.as_mut()
			&& pipe.owns(control)
		{
			let posted = pipe.posted;
			let (code, moved) = pipe.complete(status);
			if classes::stalled(code) {
				let _ = classes::clear_halt(hc, hids, &mut self.dev, pipe);
			}
			// TAKEN WHOLE OR NOT AT ALL: a bulk OUT that moved less than it was given ended early.
			let whole = classes::succeeded(code) && moved == posted;
			self.answer_send(if whole { Ok(()) } else { Err(Error::Io) });
			return true;
		}
		if !self.input.owns(control) {
			return false;
		}
		// SAMPLED AS THE COMPLETION IS TAKEN OFF THE RING, before anything queues it.
		let received_ns = clock_ns();
		let (code, moved) = self.input.complete(status);
		if classes::stalled(code) {
			let _ = classes::clear_halt(hc, hids, &mut self.dev, &mut self.input);
		}
		let Some(generation) = self.receiving else { return true };
		if classes::succeeded(code) && moved > 0 {
			let packets = self.input.read(moved as usize);
			self.deliver(generation, packets, received_ns);
		}
		self.input.post(hc, BATCH_BYTES);
		true
	}

	fn release(&mut self, hc: &mut Xhci) {
		self.receiving = None;
		self.close_stream();
		self.input.release(hc);
		// THE DEVICE IS GONE: a send still out never completes, and says so.
		self.answer_send(Err(Error::Io));
		if let Some((pipe, _)) = self.output.as_mut() {
			pipe.release(hc);
		}
	}
}
