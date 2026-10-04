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
// TWO FACES, ONE AT A TIME. A USB MIDI 2.0 device keeps its MIDI 1.0 face on alternate zero and carries Universal MIDI
// Packets on alternate 1 (`usb_midi::Binding::ump`), whose function blocks - its Group Terminal Blocks - are read from
// it once, with their names, as it binds. The MIDI 1.0 endpoints are 0 and 1, the UMP ones 2 and 3, and starting or
// sending on one selects its face: the other face's pipes are let go of and this one's brought up - refused as `again`
// while the other face is receiving or sending, because a setting is selected whole.
//
// SENDING IS ONE TRANSFER AT A TIME, ANSWERED WHEN IT COMPLETES. The service's packets go out on the same
// setting's OUT endpoint as one bulk transfer, and the answer to `send` waits for its completion - so a device
// that is slow to take them slows the sender, and nothing queues in between. A second send while one is out is
// `again`; a stall is cleared and the send it cut is a failure; a device that leaves fails the one in flight.

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::{Error, MidiBounds, MidiDeviceBlock, MidiDeviceBlockDirection, MidiDeviceDirection, MidiDeviceEndpoint, MidiDeviceEvent, MidiDeviceProtocol, MidiInputLost, MidiOpen, MidiPacketBatch, midi_device};
use rt::*;

use crate::classes::{self, Module, Pipe};
use crate::usb_hid::Hids;
use crate::{KIND_MIDI, UsbDevice, Xhci};
use drivers::common;
use drivers::ump;
use drivers::usb_class::ClassKind;
use drivers::usb_function::Endpoint;
use drivers::usb_midi::{self, Binding};

const VERSION: u32 = 1;
// A batch is at most sixty-four packets, and one receive is one batch.
const BATCH_BYTES: u32 = (usb_midi::MAX_PACKETS * usb_midi::PACKET) as u32;
const STREAM_DEPTH: u64 = 64;
// THE LARGEST REQUEST IS A FULL `send`: the header, the endpoint, the length and sixty-four packets. A buffer
// sized for the control calls alone left that request unread in the channel, answered by nothing.
const REQUEST_BYTES: usize = 512;
// The endpoints of the two faces.
const MIDI1_IN: u32 = 0;
const MIDI1_OUT: u32 = 1;
const UMP_IN: u32 = 2;
const UMP_OUT: u32 = 3;
// A Group Terminal Block answer is read whole, up to this.
const BLOCK_BYTES: u16 = 5 + 13 * ump::MAX_BLOCKS as u16;

// Which face is selected now.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Face {
	Midi1,
	Ump,
}

// ONE FUNCTION BLOCK, as read from the device, with its name.
struct Block {
	block: ump::Block,
	name: String,
}

pub struct Midi {
	dev: UsbDevice,
	binding: Binding,
	// The face selected, and its pipes: the receive pipe, and the transmit one with the cables - or, for UMP, the groups
	// - it carries.
	face: Face,
	input: Pipe,
	connection: u64,
	consumer: u64,
	// The endpoint receiving and the receiver generation it was started under, if one is.
	receiving: Option<(u32, u64)>,
	stream: u64,
	stream_seq: u32,
	// Input was dropped since the stream last had room, for this receiver generation.
	lost: Option<u64>,
	buf: Vec<u8>,
	// The transmit pipe and the cables it carries, when the device has one.
	output: Option<(Pipe, u8)>,
	// The send in flight: the connection and correlation its answer goes to.
	sending: Option<(u64, u32)>,
	// The UMP face's function blocks, read as the device bound - none for a MIDI 1.0 device, or one whose blocks did
	// not read whole.
	blocks: Vec<Block>,
}

// THE FUNCTION BLOCKS OF A MIDI 2.0 SETTING: GET_DESCRIPTOR of the Group Terminal Blocks - a standard request to the
// interface, the type and the alternate in its value - read whole and parsed by `ump::blocks`, each with its name.
unsafe fn read_blocks(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, interface: u8, alternate: u8) -> Option<Vec<Block>> {
	unsafe {
		let value = u16::from(ump::CS_GR_TRM_BLOCK) << 8 | u16::from(alternate);
		let received = crate::control_in_req(hc, hids, dev, 0x81, crate::REQ_GET_DESCRIPTOR, value, u16::from(interface), BLOCK_BYTES)?;
		let answer: Vec<u8> = (0..u64::from(received.min(u32::from(BLOCK_BYTES)))).map(|at| crate::r8(dev.data_virt + at)).collect();
		let blocks = ump::blocks(&answer).ok()?;
		Some(
			blocks
				.into_iter()
				.map(|block| {
					let mut name = crate::class_mbim::usb_string(hc, hids, dev, block.name);
					name.truncate(32);
					Block { block, name }
				})
				.collect(),
		)
	}
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
		let mut hids = Hids::new();
		let blocks = match binding.ump {
			Some(setting) => match read_blocks(hc, &mut hids, &mut dev, binding.interface, setting.alternate) {
				Some(blocks) => {
					let mut line: common::Bounded<96> = common::Bounded::new();
					line.push(b"driver.xhci: the MIDI device has a MIDI 2.0 face - ");
					line.decimal(blocks.len() as u64);
					line.push(b" function block(s)\n");
					print(line.as_bytes());
					blocks
				}
				None => {
					print(b"driver.xhci: the MIDI device's MIDI 2.0 face did not list its function blocks whole - MIDI 1.0 only\n");
					Vec::new()
				}
			},
			None => Vec::new(),
		};
		Ok(Midi { dev, binding, face: Face::Midi1, input, connection: 0, consumer: 0, receiving: None, stream: 0, stream_seq: 0, lost: None, buf: alloc::vec![0u8; REQUEST_BYTES], output, sending: None, blocks })
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
	fn deliver(&mut self, endpoint: u32, generation: u64, packets: Vec<u8>, received_ns: u64) {
		if let Some(lost) = self.lost {
			if !self.send(&MidiDeviceEvent::Lost(MidiInputLost { endpoint, receiver_generation: lost })) {
				return;
			}
			self.lost = None;
		}
		if !self.send(&MidiDeviceEvent::Batch(MidiPacketBatch { endpoint, receiver_generation: generation, received_ns, packets })) {
			self.lost = Some(generation);
		}
	}

	// Whether the UMP face is there to select: a MIDI 2.0 setting whose blocks read whole.
	fn has_ump(&self) -> bool {
		self.binding.ump.is_some() && !self.blocks.is_empty()
	}

	// The blocks one UMP endpoint names, and the groups they cover - at least one, at most sixteen.
	fn blocks_of(&self, endpoint: u32) -> (Vec<&Block>, u8) {
		let Some(setting) = self.binding.ump else { return (Vec::new(), 0) };
		let ids = match endpoint {
			UMP_IN => Some(setting.input.1),
			UMP_OUT => setting.output.map(|(_, ids)| ids),
			_ => None,
		};
		let Some(ids) = ids else { return (Vec::new(), 0) };
		let blocks: Vec<&Block> = self.blocks.iter().filter(|block| ids.as_slice().contains(&block.block.id)).collect();
		let groups = blocks.iter().map(|block| block.block.first_group + block.block.groups).max().unwrap_or(0);
		(blocks, groups)
	}

	// THE FACE `endpoint` BELONGS TO SELECTED: nothing to do when it is the one selected; `again` while the other one is
	// receiving or sending; otherwise its pipes let go of and the other face's brought up with its setting.
	fn select_face(&mut self, hc: &mut Xhci, endpoint: u32) -> Result<(), Error> {
		let face = if endpoint >= UMP_IN { Face::Ump } else { Face::Midi1 };
		if face == self.face {
			return Ok(());
		}
		if self.receiving.is_some() || self.sending.is_some() {
			return Err(Error::Again);
		}
		let (alternate, input, output): (u8, Endpoint, Option<(Endpoint, u8)>) = match face {
			Face::Midi1 => (self.binding.alternate, self.binding.input, self.binding.output),
			Face::Ump => {
				let setting = self.binding.ump.ok_or(Error::NotFound)?;
				let groups = self.blocks_of(UMP_OUT).1;
				(setting.alternate, setting.input.0, setting.output.map(|(endpoint, _)| (endpoint, groups)))
			}
		};
		// SAFETY: the device is one this controller addressed, and these are endpoints of its setting about to be selected.
		let (Some(mut new_input), new_output) = (unsafe { Pipe::new(self.dev.slot, self.dev.speed, &input) }, output.map(|(endpoint, cables)| unsafe { Pipe::new(self.dev.slot, self.dev.speed, &endpoint) }.map(|pipe| (pipe, cables)))) else {
			return Err(Error::Exhausted);
		};
		let mut new_output = match new_output {
			Some(Some(pipe)) => Some(pipe),
			Some(None) => {
				new_input.release(hc);
				return Err(Error::Exhausted);
			}
			None => None,
		};
		self.input.release(hc);
		if let Some((pipe, _)) = self.output.as_mut() {
			pipe.release(hc);
		}
		let configured = unsafe {
			match new_output.as_ref() {
				Some((pipe, _)) => classes::configure_pipes(hc, &mut self.dev, &[&new_input, pipe], 0),
				None => classes::configure_pipes(hc, &mut self.dev, &[&new_input], 0),
			}
		};
		let selected = configured && classes::select(hc, &mut self.dev, self.binding.config_value, self.binding.interface, alternate);
		self.input = new_input;
		self.output = new_output.take();
		self.face = face;
		if !selected {
			print(b"driver.xhci: the MIDI device refused the setting of the face asked for\n");
			return Err(Error::Io);
		}
		print(if face == Face::Ump { b"driver.xhci: the MIDI device is on its MIDI 2.0 face - UMP\n".as_slice() } else { b"driver.xhci: the MIDI device is on its MIDI 1.0 face\n".as_slice() });
		Ok(())
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

impl View<'_> {
	fn block_records(&self, endpoint: u32) -> Vec<MidiDeviceBlock> {
		self.midi
			.blocks_of(endpoint)
			.0
			.into_iter()
			.map(|block| MidiDeviceBlock {
				id: block.block.id,
				name: block.name.clone(),
				first_group: block.block.first_group,
				groups: block.block.groups,
				direction: match block.block.direction {
					ump::Direction::Receives => MidiDeviceBlockDirection::Receives,
					ump::Direction::Sends => MidiDeviceBlockDirection::Sends,
					ump::Direction::Both => MidiDeviceBlockDirection::Both,
				},
				protocol: block.block.protocol,
			})
			.collect()
	}
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
		let mut endpoints = alloc::vec![MidiDeviceEndpoint { index: MIDI1_IN, name: String::from("USB MIDI in"), cables: self.midi.binding.cables, direction: MidiDeviceDirection::Receive, protocol: MidiDeviceProtocol::Midi1 }];
		if let Some((_, cables)) = self.midi.binding.output {
			endpoints.push(MidiDeviceEndpoint { index: MIDI1_OUT, name: String::from("USB MIDI out"), cables, direction: MidiDeviceDirection::Transmit, protocol: MidiDeviceProtocol::Midi1 });
		}
		if self.midi.has_ump() {
			let groups = self.midi.blocks_of(UMP_IN).1;
			if groups > 0 {
				endpoints.push(MidiDeviceEndpoint { index: UMP_IN, name: String::from("USB MIDI 2.0 in"), cables: groups, direction: MidiDeviceDirection::Receive, protocol: MidiDeviceProtocol::Ump });
			}
			let groups = self.midi.blocks_of(UMP_OUT).1;
			if groups > 0 {
				endpoints.push(MidiDeviceEndpoint { index: UMP_OUT, name: String::from("USB MIDI 2.0 out"), cables: groups, direction: MidiDeviceDirection::Transmit, protocol: MidiDeviceProtocol::Ump });
			}
		}
		Ok(endpoints)
	}

	fn start(&mut self, endpoint: u32, receiver_generation: u64) -> Result<(), Error> {
		if self.midi.consumer != self.chan {
			return Err(Error::Invalid);
		}
		if endpoint != MIDI1_IN && !(endpoint == UMP_IN && self.midi.has_ump()) {
			return Err(Error::NotFound);
		}
		// ONE RECEIVER: another endpoint receiving is a face, or a stream, already taken.
		if self.midi.receiving.is_some_and(|(receiving, _)| receiving != endpoint) {
			return Err(Error::Again);
		}
		self.midi.select_face(self.hc, endpoint)?;
		self.midi.receiving = Some((endpoint, receiver_generation));
		if !self.midi.input.busy && !self.midi.input.post(self.hc, BATCH_BYTES) {
			return Err(Error::Io);
		}
		Ok(())
	}

	fn stop(&mut self, endpoint: u32, receiver_generation: u64) -> Result<(), Error> {
		if self.midi.consumer != self.chan {
			return Err(Error::Invalid);
		}
		if endpoint != MIDI1_IN && endpoint != UMP_IN {
			return Err(Error::NotFound);
		}
		// A STOP FOR A GENERATION THAT IS NOT THE CURRENT ONE stops nothing.
		if self.midi.receiving == Some((endpoint, receiver_generation)) {
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
		match endpoint {
			// A receive endpoint is not one packets go OUT on.
			MIDI1_IN | UMP_IN => return Err(Error::Invalid),
			MIDI1_OUT if self.midi.binding.output.is_some() => {}
			UMP_OUT if self.midi.has_ump() && self.midi.binding.ump.is_some_and(|setting| setting.output.is_some()) => {}
			_ => return Err(Error::NotFound),
		}
		// A MIDI 1.0 batch is checked to its cables; a UMP batch is words, which the service checked.
		if packets.is_empty() || packets.len() % usb_midi::PACKET != 0 || packets.len() > BATCH_BYTES as usize {
			return Err(Error::Invalid);
		}
		if self.midi.sending.is_some() {
			return Err(Error::Again);
		}
		self.midi.select_face(self.hc, endpoint)?;
		let Some((pipe, cables)) = self.midi.output.as_mut() else { return Err(Error::NotFound) };
		let cables = *cables;
		if endpoint == MIDI1_OUT && packets.chunks_exact(usb_midi::PACKET).any(|packet| packet[0] >> 4 >= cables) {
			return Err(Error::Invalid);
		}
		if pipe.busy {
			return Err(Error::Again);
		}
		if !pipe.fill(&packets) || !pipe.post(self.hc, packets.len() as u32) {
			return Err(Error::Io);
		}
		self.posted = true;
		Ok(())
	}

	fn blocks(&mut self, endpoint: u32) -> Result<Vec<MidiDeviceBlock>, Error> {
		if self.midi.consumer != self.chan {
			return Err(Error::Invalid);
		}
		match endpoint {
			MIDI1_IN | MIDI1_OUT => Ok(Vec::new()),
			UMP_IN | UMP_OUT if self.midi.has_ump() => Ok(self.block_records(endpoint)),
			_ => Err(Error::NotFound),
		}
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
		let Some((endpoint, generation)) = self.receiving else { return true };
		if classes::succeeded(code) && moved > 0 {
			let packets = self.input.read(moved as usize);
			self.deliver(endpoint, generation, packets, received_ns);
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
