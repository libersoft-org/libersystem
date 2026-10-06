// The USB Bluetooth side of driver.xhci: a controller's HCI transport on the bus, published as the `bluetooth-hci`
// provider BluetoothService drives over `liber:device`'s `hci-transport`.
//
// A BOUNDED PACKET TRANSPORT AND NOTHING ABOVE IT. Commands go down the control pipe, ACL data down the bulk OUT
// pipe; events come up the interrupt pipe and ACL data up the bulk IN pipe, each cut out of the transfers by its
// own header. The command state machine, L2CAP, ATT and pairing are BluetoothService's, in a process with no device
// claim; nothing here reads what a packet means beyond where it ends.
//
// A SEND IS ANSWERED WHEN THE PIPE TOOK IT. A command's control transfer is waited for; an ACL packet's bulk
// transfer is answered when it completes, so a controller slow to take data makes the send slow and nothing else.
//
// A RESET IS A NEW SESSION. The controller is sent HCI_Reset and its completion is taken here, not delivered - it
// belongs to the session that ended - and the epoch advances, which every packet after it carries and the control
// stream announces.
//
// VOICE IS A SETTING THE CONSUMER ASKS FOR. Once BluetoothService has set a synchronous link up with the controller
// it asks `voice` for the channels and sample size it negotiated, and the voice interface's alternate the
// specification's table names is selected with its isochronous pair: transfers standing on the IN pipe, each a piece
// of the SCO stream cut back into packets (`bt_usb::ScoPieces` - a packet that lost a piece is refused, never
// delivered with a hole), and a SCO send cut into pieces of the alternate's size on the OUT pipe, answered when its
// last piece is taken. Zero channels - and a reset, and the consumer leaving - put the interface back at zero
// bandwidth.
//
// ISO SHARES THE ACL PAIR, told from ACL only by a connection handle, and the handles are the host's. So this transport
// carries an outbound `iso` packet on the bulk OUT pipe like ACL, and delivers every packet the IN pipe brings as `acl`,
// cut by the four-byte header both kinds share at the larger of the two ceilings: BluetoothService classifies.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use proto::system::{Error, HciAttachment, HciControlEvent, HciControlKind, HciPacket, HciPacketKind, hci_transport};
use rt::*;

use crate::classes::{self, Module, Pipe};
use crate::iso::{self, Completed, IsoPipe};
use crate::usb_hid::Hids;
use crate::{KIND_BLUETOOTH, UsbDevice, Xhci, control_nodata, control_out_req};
use drivers::bt_usb::{self, Binding, Kind, Reassembly, ScoPieces, Voice};
use drivers::common;
use drivers::usb_class::ClassKind;

const VERSION: u32 = 1;
// A send's request: opcode, correlation, kind, a length and the largest packet.
const REQUEST_BYTES: usize = 2 + 4 + 1 + 2 + bt_usb::MAX_ACL + 16;
const STREAM_DEPTH: u64 = 256;
// How long a reset waits for its completion.
const RESET_TICKS: u64 = 300;
const HCI_RESET: [u8; 3] = [0x03, 0x0c, 0x00];
// The request that selects an interface's alternate setting.
const REQ_SET_INTERFACE: u8 = 0x0b;
const RT_INTERFACE_OUT: u8 = 0x01;
// SCO packets sent and not yet taken: `again` past this many.
const SCO_QUEUE: usize = 8;

// THE VOICE SETTING OPEN: its alternate, its isochronous pair, the packets cut out of what comes in, and the packets
// going out not yet taken - by the TRB of each one's last piece, with the request it answers.
struct VoiceOpen {
	alternate: u8,
	input: IsoPipe,
	output: IsoPipe,
	pieces: ScoPieces,
	sending: VecDeque<(u64, u64, u32, u32)>,
}

pub struct Bluetooth {
	dev: UsbDevice,
	binding: Binding,
	events: Pipe,
	acl_in: Pipe,
	acl_out: Pipe,
	event_packets: Reassembly,
	acl_packets: Reassembly,
	epoch: u32,
	consumer: u64,
	packets: u64,
	packets_seq: u32,
	control: u64,
	control_seq: u32,
	// The ACL packet the controller has not finished taking.
	pending: Option<(u64, u32, u32)>,
	voice: Option<VoiceOpen>,
	buf: Vec<u8>,
}

/// Bring a bound controller's three pipes up and select its configuration. The device comes back on failure.
///
/// # Safety
/// `dev` is an addressed device this controller owns.
pub unsafe fn configure(hc: &mut Xhci, mut dev: UsbDevice, binding: Binding) -> Result<Bluetooth, UsbDevice> {
	unsafe {
		let pipes = (Pipe::new(dev.slot, dev.speed, &binding.events), Pipe::new(dev.slot, dev.speed, &binding.acl_in), Pipe::new(dev.slot, dev.speed, &binding.acl_out));
		let (Some(mut events), Some(mut acl_in), Some(mut acl_out)) = pipes else {
			let (a, b, c) = pipes;
			for mut pipe in [a, b, c].into_iter().flatten() {
				pipe.release(hc);
			}
			print(b"driver.xhci: a Bluetooth controller is on the bus and no pages were left for its pipes\n");
			return Err(dev);
		};
		if !classes::configure_pipes(hc, &mut dev, &[&events, &acl_in, &acl_out], 0) || !classes::select(hc, &mut dev, binding.config_value, binding.interface, 0) {
			events.release(hc);
			acl_in.release(hc);
			acl_out.release(hc);
			print(b"driver.xhci: a Bluetooth controller's pipes or its configuration were refused\n");
			return Err(dev);
		}
		print(if binding.has_voice() { b"driver.xhci: Bluetooth controller bound - an HCI transport over its event pipe and its ACL pair, and voice on its isochronous settings\n".as_slice() } else { b"driver.xhci: Bluetooth controller bound - an HCI transport over its event pipe and its ACL pair\n".as_slice() });
		Ok(Bluetooth { dev, binding, events, acl_in, acl_out, event_packets: Reassembly::new(Kind::Event), acl_packets: Reassembly::new(Kind::Acl), epoch: 1, consumer: 0, packets: 0, packets_seq: 0, control: 0, control_seq: 0, pending: None, voice: None, buf: alloc::vec![0u8; REQUEST_BYTES] })
	}
}

impl Bluetooth {
	fn post_receives(&mut self, hc: &mut Xhci) {
		if !self.events.busy {
			let length = self.events.mps.min(bt_usb::MAX_EVENT as u32);
			self.events.post(hc, length);
		}
		// ONE PACKET'S WORTH A RECEIVE, and not a frame's: a bulk pipe sends no zero-length packet after data that
		// ends on a packet boundary, so a receive longer than a packet would wait on a 512-byte ACL frame for bytes
		// that are the NEXT frame's. The reassembly joins what the packets carry.
		if !self.acl_in.busy {
			let length = self.acl_in.mps;
			self.acl_in.post(hc, length);
		}
	}

	fn deliver(&mut self, kind: HciPacketKind, bytes: Vec<u8>) {
		if self.packets == 0 {
			return;
		}
		let packet = HciPacket { kind, epoch: self.epoch, bytes };
		let mut frame = alloc::vec![0u8; bt_usb::MAX_ACL + 64];
		let mut handles = wire::Handles::new();
		let Some(len) = hci_transport::receive_frame(self.packets_seq, &packet, &mut frame, &mut handles) else { return };
		if try_send(self.packets, &frame[..len], 0) {
			self.packets_seq += 1;
		} else {
			// A CONSUMER THAT STOPPED DRAINING has its session end, and the control stream - which it must still be
			// reading - says so.
			self.announce(HciControlKind::Fault);
		}
	}

	fn announce(&mut self, kind: HciControlKind) {
		if self.control == 0 {
			return;
		}
		let event = HciControlEvent { kind, epoch: self.epoch };
		let mut frame = [0u8; 64];
		let mut handles = wire::Handles::new();
		if let Some(len) = hci_transport::control_frame(self.control_seq, &event, &mut frame, &mut handles)
			&& try_send(self.control, &frame[..len], 0)
		{
			self.control_seq += 1;
		}
	}

	// One ACL send - or an ISO one, which shares the bulk OUT pipe - answered when the controller took it.
	fn send_acl(&mut self, hc: &mut Xhci, chan: u64, corr: u32, packet: &[u8], iso: bool) {
		if !(if iso { bt_usb::iso_is_whole(packet) } else { bt_usb::acl_is_whole(packet) }) {
			classes::answer_u32(chan, corr, Err(Error::Invalid));
			return;
		}
		// THE TRANSPORT'S OWN QUEUE IS ONE PACKET: `again` says it is full, which is not the controller's buffers.
		if self.pending.is_some() {
			classes::answer_u32(chan, corr, Err(Error::Again));
			return;
		}
		if !self.acl_out.fill(packet) || !self.acl_out.post(hc, packet.len() as u32) {
			classes::answer_u32(chan, corr, Err(Error::Io));
			return;
		}
		self.pending = Some((chan, corr, packet.len() as u32));
	}

	// THE VOICE ALTERNATE SELECTED with its isochronous pair: both endpoints brought up in one Configure Endpoint, the
	// setting selected, and the IN pipe's transfers standing. A setting already open is closed first.
	fn open_voice(&mut self, hc: &mut Xhci, hids: &mut Hids, voice: Voice) -> Result<u8, Error> {
		self.close_voice(hc, hids);
		let Some(interface) = self.binding.voice_interface else { return Err(Error::Unsupported) };
		let floor = [self.binding.events, self.binding.acl_in, self.binding.acl_out].iter().map(|endpoint| endpoint.dci()).max().unwrap_or(0);
		let endpoints = [(voice.iso_in, u32::from(voice.iso_in.max_packet())), (voice.iso_out, u32::from(voice.iso_out.max_packet()))];
		let Some(mut pipes) = (unsafe { iso::open(hc, &mut self.dev, &endpoints, floor) }) else {
			print(b"driver.xhci: the Bluetooth controller's voice pipes were refused by the controller\n");
			return Err(Error::Io);
		};
		let (Some(mut output), Some(mut input)) = (pipes.pop(), pipes.pop()) else { return Err(Error::Io) };
		if control_nodata(hc, hids, &mut self.dev, RT_INTERFACE_OUT, REQ_SET_INTERFACE, u16::from(voice.alternate), u16::from(interface)).is_none() {
			input.release(hc);
			output.release(hc);
			print(b"driver.xhci: the Bluetooth controller refused its voice setting\n");
			return Err(Error::Io);
		}
		input.post_in(hc);
		let mut line: common::Bounded<128> = common::Bounded::new();
		line.push(b"driver.xhci: Bluetooth voice open on alternate ");
		line.decimal(u64::from(voice.alternate));
		line.push(b" - pieces of ");
		line.decimal(u64::from(output.capacity));
		line.push(b" bytes each way\n");
		print(line.as_bytes());
		self.voice = Some(VoiceOpen { alternate: voice.alternate, pieces: ScoPieces::new(input.capacity as usize), input, output, sending: VecDeque::new() });
		Ok(voice.alternate)
	}

	// THE VOICE SETTING CLOSED: the standing transfers abandoned, the sends not yet taken answered as failed, and the
	// voice interface back at zero bandwidth - unless the device is gone, which `release` says by passing no hids.
	fn close_voice(&mut self, hc: &mut Xhci, hids: &mut Hids) {
		let Some(mut voice) = self.voice.take() else { return };
		voice.input.stop(hc, hids);
		voice.output.stop(hc, hids);
		voice.input.release(hc);
		voice.output.release(hc);
		for (_, chan, corr, _) in voice.sending.drain(..) {
			classes::answer_u32(chan, corr, Err(Error::Io));
		}
		if let Some(interface) = self.binding.voice_interface {
			let _ = control_nodata(hc, hids, &mut self.dev, RT_INTERFACE_OUT, REQ_SET_INTERFACE, 0, u16::from(interface));
		}
		print(b"driver.xhci: Bluetooth voice closed - its interface is back at zero bandwidth\n");
	}

	// One SCO send, cut into the alternate's pieces and answered when the last is taken.
	fn send_sco(&mut self, hc: &mut Xhci, chan: u64, corr: u32, packet: &[u8]) {
		if !self.binding.has_voice() {
			classes::answer_u32(chan, corr, Err(Error::Unsupported));
			return;
		}
		// NO SETTING OPEN IS NO LINK: a voice packet before `voice` is a consumer that skipped a step.
		let (true, Some(voice)) = (bt_usb::sco_is_whole(packet), self.voice.as_mut()) else {
			classes::answer_u32(chan, corr, Err(Error::Invalid));
			return;
		};
		if voice.sending.len() >= SCO_QUEUE {
			classes::answer_u32(chan, corr, Err(Error::Again));
			return;
		}
		let pieces: Vec<&[u8]> = packet.chunks(voice.output.capacity as usize).collect();
		match voice.output.send_out(hc, &pieces) {
			Some(last) => voice.sending.push_back((last, chan, corr, packet.len() as u32)),
			None => {
				classes::answer_u32(chan, corr, Err(Error::Again));
			}
		}
	}

	fn close_streams(&mut self) {
		for stream in [&mut self.packets, &mut self.control] {
			if *stream != 0 {
				close(*stream);
				*stream = 0;
			}
		}
		self.packets_seq = 0;
		self.control_seq = 0;
	}
}

struct View<'a> {
	bt: &'a mut Bluetooth,
	hc: &'a mut Xhci,
	hids: &'a mut Hids,
}

impl hci_transport::Service for View<'_> {
	fn attach(&mut self, version: u32) -> Result<HciAttachment, Error> {
		if version != VERSION {
			return Err(Error::Unsupported);
		}
		let sco = self.bt.binding.has_voice();
		// ISO ON THE BULK PAIR: carried out as its own kind, delivered in as ACL for the host to classify by its handle.
		Ok(HciAttachment { version, iso: true, max_command: bt_usb::MAX_COMMAND as u32, max_event: bt_usb::MAX_EVENT as u32, max_acl: bt_usb::MAX_ACL as u32, max_iso: bt_usb::MAX_ISO as u32, command_credits: 1, acl_credits: 1, acl_queue: 1, epoch: self.bt.epoch, sco, max_sco: if sco { bt_usb::MAX_SCO as u32 } else { 0 } })
	}

	fn send(&mut self, kind: HciPacketKind, bytes: Vec<u8>) -> Result<u32, Error> {
		match kind {
			HciPacketKind::Command => {
				if !bt_usb::command_is_whole(&bytes) {
					return Err(Error::Invalid);
				}
				unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.bt.dev.data_virt as *mut u8, bytes.len()) };
				let interface = self.bt.binding.interface as u16;
				control_out_req(self.hc, self.hids, &mut self.bt.dev, bt_usb::RT_CLASS_DEVICE_OUT, 0, 0, interface, bytes.len() as u16).ok_or(Error::Io)?;
				Ok(bytes.len() as u32)
			}
			// ACL, ISO and SCO are taken before dispatch, because they are answered when the controller took them.
			HciPacketKind::Acl | HciPacketKind::Iso | HciPacketKind::Sco => Err(Error::Invalid),
			HciPacketKind::Event => Err(Error::Invalid),
		}
	}

	fn receive(&mut self) -> Vec<HciPacket> {
		Vec::new()
	}

	fn control(&mut self) -> Vec<HciControlEvent> {
		Vec::new()
	}

	fn voice(&mut self, channels: u8, bits: u8, wideband: bool) -> Result<u8, Error> {
		if channels == 0 {
			self.bt.close_voice(self.hc, self.hids);
			return Ok(0);
		}
		let alternate = bt_usb::alternate_for(channels, bits, wideband).ok_or(Error::Invalid)?;
		let voice = self.bt.binding.voice_at(alternate).ok_or(Error::Unsupported)?;
		if self.bt.voice.as_ref().is_some_and(|open| open.alternate == alternate) {
			return Ok(alternate);
		}
		self.bt.open_voice(self.hc, self.hids, voice)
	}

	fn reset(&mut self) -> Result<u32, Error> {
		// THE SESSION'S SYNCHRONOUS LINKS END WITH IT, and so does the voice setting they were carried on.
		self.bt.close_voice(self.hc, self.hids);
		let bt = &mut *self.bt;
		// WHAT THE ENDING SESSION HAD IN FLIGHT IS ANSWERED AND DROPPED.
		if let Some((chan, corr, _)) = bt.pending.take() {
			let _ = classes::abandon(self.hc, self.hids, &mut bt.acl_out);
			classes::answer_u32(chan, corr, Err(Error::Io));
		}
		unsafe { core::ptr::copy_nonoverlapping(HCI_RESET.as_ptr(), bt.dev.data_virt as *mut u8, HCI_RESET.len()) };
		let interface = bt.binding.interface as u16;
		control_out_req(self.hc, self.hids, &mut bt.dev, bt_usb::RT_CLASS_DEVICE_OUT, 0, 0, interface, HCI_RESET.len() as u16).ok_or(Error::Io)?;
		// ITS COMPLETION IS TAKEN HERE, and everything before it with it: all of it belongs to the ended session.
		bt.event_packets.clear();
		let deadline = clock() + RESET_TICKS;
		let mut done = false;
		while !done {
			let length = bt.events.mps.min(bt_usb::MAX_EVENT as u32);
			let Some((code, moved)) = classes::transfer(self.hc, self.hids, &mut bt.events, length, deadline) else { break };
			if !classes::succeeded(code) {
				break;
			}
			let bytes = bt.events.read(moved as usize);
			for event in bt.event_packets.push(&bytes) {
				// Command Complete for HCI_Reset: event 0x0e, then credits, then the opcode.
				if event.len() >= 5 && event[0] == 0x0e && event[3..5] == HCI_RESET[..2] {
					done = true;
				}
			}
		}
		if !done {
			return Err(Error::Io);
		}
		bt.acl_packets.clear();
		bt.announce(HciControlKind::Reset);
		bt.epoch = bt.epoch.wrapping_add(1);
		bt.post_receives(self.hc);
		Ok(bt.epoch)
	}
}

impl Module for Bluetooth {
	fn kind(&self) -> ClassKind {
		ClassKind::Bluetooth
	}

	fn inventory(&self) -> u8 {
		KIND_BLUETOOTH
	}

	fn provider(&self) -> u16 {
		driver_protocol::provider::BLUETOOTH_HCI
	}

	fn name(&self) -> &'static [u8] {
		driver_protocol::provider::USB_BLUETOOTH_NAME
	}

	fn device(&self) -> &UsbDevice {
		&self.dev
	}

	fn device_mut(&mut self) -> &mut UsbDevice {
		&mut self.dev
	}

	fn start(&mut self, hc: &mut Xhci) {
		self.post_receives(hc);
	}

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
		if self.consumer == 0 {
			self.consumer = chan;
		}
		match classes::correlation(&request) {
			// An ACL, ISO or SCO send: the kind byte, then the packet as a length-prefixed list.
			Some((hci_transport::OP_SEND, corr)) if request.len() >= 9 && (request[6] == HciPacketKind::Acl as u8 || request[6] == HciPacketKind::Iso as u8 || request[6] == HciPacketKind::Sco as u8) => {
				for &handle in handles.as_slice() {
					close(handle);
				}
				let count = u16::from_le_bytes([request[7], request[8]]) as usize;
				if request.len() != 9 + count {
					classes::answer_u32(chan, corr, Err(Error::Invalid));
					return true;
				}
				let packet = request[9..].to_vec();
				if request[6] == HciPacketKind::Sco as u8 {
					self.send_sco(hc, chan, corr, &packet);
				} else {
					self.send_acl(hc, chan, corr, &packet, request[6] == HciPacketKind::Iso as u8);
				}
			}
			// THE TWO STREAMS, opened by hand: their answers carry the consumer's ends.
			Some((op @ (hci_transport::OP_RECEIVE | hci_transport::OP_CONTROL), _)) => {
				let mut view = View { bt: self, hc, hids };
				let corr = if op == hci_transport::OP_RECEIVE { hci_transport::receive_open(&mut view, &request, &mut handles).map(|(corr, _)| corr) } else { hci_transport::control_open(&mut view, &request, &mut handles).map(|(corr, _)| corr) };
				let Some(corr) = corr else { return true };
				let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
				let slot = if op == hci_transport::OP_RECEIVE { &mut self.packets } else { &mut self.control };
				if *slot != 0 {
					close(*slot);
				}
				*slot = producer;
				send_caps_blocking(chan, &corr.to_le_bytes(), &[consumer]);
			}
			_ => {
				let mut reply = [0u8; 512];
				let mut reply_handles = wire::Handles::new();
				let mut view = View { bt: self, hc, hids };
				if let Some(written) = hci_transport::dispatch(&mut view, &request, &mut handles, &mut reply, &mut reply_handles) {
					send_caps_blocking(chan, &reply[..written], reply_handles.as_slice());
				}
				for &handle in handles.as_slice() {
					close(handle);
				}
			}
		}
		true
	}

	fn departed(&mut self, hc: &mut Xhci, hids: &mut Hids, chan: u64) {
		if self.consumer != chan {
			return;
		}
		self.consumer = 0;
		if self.pending.take().is_some() {
			let _ = classes::abandon(hc, hids, &mut self.acl_out);
		}
		self.close_voice(hc, hids);
		self.close_streams();
	}

	fn absorb(&mut self, hc: &mut Xhci, hids: &mut Hids, pointer: u64, status: u32, control: u32) -> bool {
		// VOICE: a send's last piece taken answers it and every send before it; a piece in is cut into packets, and a
		// piece lost loses the packet it belonged to.
		if let Some(voice) = self.voice.as_mut() {
			if voice.output.owns(control) {
				voice.output.complete_out(pointer, status);
				if let Some(at) = voice.sending.iter().position(|&(last, ..)| last == pointer) {
					let taken = classes::succeeded(status >> 24);
					for (_, chan, corr, length) in voice.sending.drain(..=at) {
						classes::answer_u32(chan, corr, if taken { Ok(length) } else { Err(Error::Io) });
					}
				}
				return true;
			}
			if voice.input.owns(control) {
				let packets = match voice.input.complete_in(status) {
					Completed::Payload(piece) => voice.pieces.piece(&piece),
					Completed::Lost => {
						voice.pieces.lost();
						Vec::new()
					}
					Completed::Nothing => Vec::new(),
				};
				voice.input.post_in(hc);
				for packet in packets {
					self.deliver(HciPacketKind::Sco, packet);
				}
				return true;
			}
		}
		if self.acl_out.owns(control) {
			let (code, moved) = self.acl_out.complete(status);
			if classes::stalled(code) {
				let cleared = classes::clear_halt(hc, hids, &mut self.dev, &mut self.acl_out);
				print(if cleared { b"driver.xhci: the Bluetooth controller stalled its ACL OUT pipe - cleared\n" } else { b"driver.xhci: the Bluetooth controller stalled its ACL OUT pipe - NOT cleared\n" });
			}
			if let Some((chan, corr, length)) = self.pending.take() {
				classes::answer_u32(chan, corr, if classes::succeeded(code) && moved == length { Ok(moved) } else { Err(Error::Io) });
			}
			return true;
		}
		for kind in [Kind::Event, Kind::Acl] {
			let pipe = if kind == Kind::Event { &mut self.events } else { &mut self.acl_in };
			if !pipe.owns(control) {
				continue;
			}
			let (code, moved) = pipe.complete(status);
			if classes::succeeded(code) && moved > 0 {
				let bytes = pipe.read(moved as usize);
				let packets = if kind == Kind::Event { self.event_packets.push(&bytes) } else { self.acl_packets.push(&bytes) };
				for packet in packets {
					self.deliver(if kind == Kind::Event { HciPacketKind::Event } else { HciPacketKind::Acl }, packet);
				}
			} else if classes::stalled(code) {
				let pipe = if kind == Kind::Event { &mut self.events } else { &mut self.acl_in };
				let cleared = classes::clear_halt(hc, hids, &mut self.dev, pipe);
				print(match (kind == Kind::Event, cleared) {
					(true, true) => b"driver.xhci: the Bluetooth controller stalled its event pipe - cleared\n",
					(true, false) => b"driver.xhci: the Bluetooth controller stalled its event pipe - NOT cleared\n",
					(false, true) => b"driver.xhci: the Bluetooth controller stalled its ACL IN pipe - cleared\n",
					(false, false) => b"driver.xhci: the Bluetooth controller stalled its ACL IN pipe - NOT cleared\n",
				});
			}
			self.post_receives(hc);
			return true;
		}
		false
	}

	fn release(&mut self, hc: &mut Xhci) {
		// THE DEVICE IS GONE, and a consumer still reading the control stream is told before it closes.
		self.announce(HciControlKind::Removed);
		self.close_streams();
		// THE DEVICE IS GONE: its voice pipes are let go of, and nothing is asked of it.
		if let Some(mut voice) = self.voice.take() {
			voice.input.release(hc);
			voice.output.release(hc);
			for (_, chan, corr, _) in voice.sending.drain(..) {
				classes::answer_u32(chan, corr, Err(Error::Io));
			}
		}
		self.events.release(hc);
		self.acl_in.release(hc);
		self.acl_out.release(hc);
	}
}
