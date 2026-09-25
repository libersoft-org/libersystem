// The USB CDC-MBIM side of driver.xhci: a mobile-broadband modem on the bus, published as the `modem` provider
// ModemService drives over `liber:modem-device`.
//
// A PROVIDER TRANSLATES FRAMING; THE SERVICE DECIDES. Commands become MBIM control messages on the control pipe -
// cut into fragments no longer than the device's own maximum, and its answers reassembled by `drivers::mbim`'s
// bounded assembler - and datagrams become NCM transfer blocks on the bulk pair, taken apart by the same session
// validator. Whether a PIN is entered, which context may come up and who asked are ModemService's; nothing here
// keeps a secret past the request that carried it, and the pages it crossed are zeroed after it.
//
// THE STATE IS THE MODEM'S, READ WHEN ASKED. A state is five BASIC_CONNECT queries - the SIM's readiness and its
// PIN, registration, signal and the context - and an indication from the modem is answered with a fresh one on
// the indication stream. A SIM whose ICC ID changes, or that arrives, is a new SIM generation; a context is the
// generation the service activated it under, and a datagram is carried only for the active one.

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::{Command, CommandKind, CommandReply, CommandStatus, Datagram, DeviceIdentity, DeviceLimits, DeviceOpen, DeviceRegistration, DeviceSim, DeviceState, Error, Indication, IpConfig, IpFamily, modem_device};
use rt::*;

use crate::classes::{self, Module, Pipe};
use crate::usb_hid::Hids;
use crate::{KIND_MODEM, UsbDevice, Xhci, control_in_req, control_out_req, r8};
use drivers::common;
use drivers::mbim;
use drivers::mbim_cid::{self as cid, Binding};
use drivers::usb_class::ClassKind;

const VERSION: u32 = 1;
const REQUEST_BYTES: usize = 512;
const STREAM_DEPTH: u64 = 64;
// A command is answered within five seconds or not at all.
const ANSWER_TICKS: u64 = 500;
// A transfer block, either way, is at most a page.
const BLOCK_BYTES: u32 = 4096;
const MTU: u16 = 1500;
const SESSION: u8 = 0;

pub struct Mbim {
	dev: UsbDevice,
	binding: Binding,
	notify: Pipe,
	data_in: Pipe,
	data_out: Pipe,
	assembler: mbim::Assembler,
	transaction: u32,
	connection: u64,
	consumer: u64,
	sim_generation: u64,
	iccid: Option<String>,
	context: Option<u64>,
	revision: u64,
	indicated: bool,
	manufacturer: String,
	model: String,
	indications: u64,
	indications_seq: u32,
	receive: u64,
	receive_seq: u32,
	transmit: u64,
	sequence: u16,
	buf: Vec<u8>,
}

fn limits() -> mbim::Limits {
	mbim::Limits::negotiate(16, 16 * 1024, 64 * 1024, 4)
}

// A USB string descriptor, as text.
fn usb_string(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, index: u8) -> String {
	if index == 0 {
		return String::new();
	}
	let Some(received) = control_in_req(hc, hids, dev, 0x80, crate::REQ_GET_DESCRIPTOR, 0x0300 | index as u16, 0x0409, 255) else { return String::new() };
	let bytes: Vec<u8> = (0..received.min(255) as u64).map(|at| unsafe { r8(dev.data_virt + at) }).collect();
	if bytes.len() < 2 || bytes[1] != 3 {
		return String::new();
	}
	let units: Vec<u16> = bytes[2..(bytes[0] as usize).min(bytes.len())].chunks_exact(2).map(|unit| u16::from_le_bytes([unit[0], unit[1]])).take(cid::MAX_NAME).collect();
	String::from_utf16_lossy(&units)
}

/// Bring a bound modem's pipes up, select its data setting and size its transfer blocks.
///
/// # Safety
/// `dev` is an addressed device this controller owns.
pub unsafe fn configure(hc: &mut Xhci, mut dev: UsbDevice, binding: Binding) -> Result<Mbim, UsbDevice> {
	unsafe {
		let pipes = (Pipe::new(dev.slot, dev.speed, &binding.notify), Pipe::new(dev.slot, dev.speed, &binding.bulk_in), Pipe::new(dev.slot, dev.speed, &binding.bulk_out));
		let (Some(mut notify), Some(mut data_in), Some(mut data_out)) = pipes else {
			let (a, b, c) = pipes;
			for mut pipe in [a, b, c].into_iter().flatten() {
				pipe.release(hc);
			}
			print(b"driver.xhci: a modem is on the bus and no pages were left for its pipes\n");
			return Err(dev);
		};
		let release = |hc: &mut Xhci, pipes: [&mut Pipe; 3]| {
			for pipe in pipes {
				pipe.release(hc);
			}
		};
		if !classes::configure_pipes(hc, &mut dev, &[&notify, &data_in, &data_out], 0) || !classes::select(hc, &mut dev, binding.config_value, binding.data_interface, binding.data_alternate) {
			release(hc, [&mut notify, &mut data_in, &mut data_out]);
			print(b"driver.xhci: a modem's pipes or its data setting were refused\n");
			return Err(dev);
		}
		let mut none = Hids::new();
		// ITS BLOCKS NO LARGER THAN A PAGE, asked for rather than assumed: a device that took the request sends none
		// larger, and one that did not is refused on its first block past the page by the validator.
		let size = BLOCK_BYTES.to_le_bytes();
		core::ptr::copy_nonoverlapping(size.as_ptr(), dev.data_virt as *mut u8, 4);
		let _ = control_out_req(hc, &mut none, &mut dev, cid::RT_CLASS_INTERFACE_OUT, cid::REQ_SET_NTB_INPUT_SIZE, 0, binding.control_interface as u16, 4);
		// WHO MADE IT AND WHAT IT IS, which MBIM does not say and the device's own strings do.
		let identity = control_in_req(hc, &mut none, &mut dev, 0x80, crate::REQ_GET_DESCRIPTOR, 0x0100, 0, 18).filter(|&received| received >= 16).map(|_| (r8(dev.data_virt + 14), r8(dev.data_virt + 15)));
		let (manufacturer, model) = match identity {
			Some((m, p)) => (usb_string(hc, &mut none, &mut dev, m), usb_string(hc, &mut none, &mut dev, p)),
			None => (String::new(), String::new()),
		};
		let mut line: common::Bounded<96> = common::Bounded::new();
		line.push(b"driver.xhci: MBIM modem bound - ");
		line.decimal(binding.max_control as u64);
		line.push(b"-byte control messages, ");
		line.decimal(data_in.mps as u64);
		line.push(b"-byte packets\n");
		print(line.as_bytes());
		Ok(Mbim { dev, binding, notify, data_in, data_out, assembler: mbim::Assembler::new(limits()), transaction: 0, connection: 0, consumer: 0, sim_generation: 0, iccid: None, context: None, revision: 0, indicated: false, manufacturer, model, indications: 0, indications_seq: 0, receive: 0, receive_seq: 0, transmit: 0, sequence: 0, buf: alloc::vec![0u8; REQUEST_BYTES] })
	}
}

impl Mbim {
	fn next_transaction(&mut self) -> u32 {
		self.transaction = self.transaction.wrapping_add(1).max(1);
		self.transaction
	}

	// One message, in fragments no longer than the device takes - and the page each crossed zeroed after it,
	// because a PIN rides in these.
	fn send(&mut self, hc: &mut Xhci, hids: &mut Hids, transfers: Vec<Vec<u8>>) -> bool {
		let interface = self.binding.control_interface as u16;
		let mut sent = true;
		for mut transfer in transfers {
			if transfer.len() > 4096 {
				sent = false;
				break;
			}
			unsafe { core::ptr::copy_nonoverlapping(transfer.as_ptr(), self.dev.data_virt as *mut u8, transfer.len()) };
			sent = control_out_req(hc, hids, &mut self.dev, cid::RT_CLASS_INTERFACE_OUT, cid::REQ_SEND_ENCAPSULATED_COMMAND, 0, interface, transfer.len() as u16).is_some();
			unsafe { core::ptr::write_bytes(self.dev.data_virt as *mut u8, 0, transfer.len()) };
			for byte in transfer.iter_mut() {
				unsafe { core::ptr::write_volatile(byte, 0) };
			}
			if !sent {
				break;
			}
		}
		sent
	}

	// Every response the device has waiting, reassembled: the one a caller waits for is returned, and an
	// indication is noted for the state that follows it.
	fn collect(&mut self, hc: &mut Xhci, hids: &mut Hids, wanted: Option<(u32, u32)>) -> Option<mbim::Message> {
		let interface = self.binding.control_interface as u16;
		let length = self.binding.max_control.min(4096);
		loop {
			let received = control_in_req(hc, hids, &mut self.dev, cid::RT_CLASS_INTERFACE_IN, cid::REQ_GET_ENCAPSULATED_RESPONSE, 0, interface, length)?;
			if received == 0 {
				return None;
			}
			let bytes: Vec<u8> = (0..received as u64).map(|at| unsafe { r8(self.dev.data_virt + at) }).collect();
			let Ok(fragment) = mbim::parse_fragment(&bytes) else { continue };
			let Ok(Some(message)) = self.assembler.feed(&fragment, clock()) else { continue };
			if message.message_type == cid::INDICATE_STATUS {
				if let Ok(indicated) = cid::indicated(&message.body)
					&& indicated.cid == cid::CID_CONNECT
					&& cid::connect_info(&indicated.info).is_ok_and(|connect| connect.activation != cid::ACTIVATION_ACTIVATED)
				{
					// THE NETWORK ENDED THE CONTEXT: it is over whatever the service thinks.
					self.context = None;
				}
				self.indicated = true;
				continue;
			}
			if wanted.is_some_and(|(kind, transaction)| message.message_type == kind && message.transaction == transaction) {
				return Some(message);
			}
		}
	}

	// Send one message and wait for its answer, reading responses as the device signals them.
	fn exchange(&mut self, hc: &mut Xhci, hids: &mut Hids, transfers: Vec<Vec<u8>>, answer: u32, transaction: u32) -> Option<mbim::Message> {
		if !self.send(hc, hids, transfers) {
			return None;
		}
		let deadline = clock() + ANSWER_TICKS;
		loop {
			if let Some(message) = self.collect(hc, hids, Some((answer, transaction))) {
				return Some(message);
			}
			// NOTHING YET: the device says when there is, on its notification pipe.
			let length = self.notify.mps.min(64);
			classes::transfer(hc, hids, &mut self.notify, length, deadline)?;
		}
	}

	fn query(&mut self, hc: &mut Xhci, hids: &mut Hids, which: u32, info: &[u8], set: bool) -> Result<cid::Done, Error> {
		let transaction = self.next_transaction();
		let body = cid::command(&cid::BASIC_CONNECT, which, set, info);
		let transfers = mbim::fragments(cid::COMMAND_MSG, transaction, &body, self.binding.max_control as usize);
		let message = self.exchange(hc, hids, transfers, cid::COMMAND_DONE, transaction).ok_or(Error::TimedOut)?;
		let done = cid::done(&message.body).map_err(|_| Error::Corrupt)?;
		if done.service != cid::BASIC_CONNECT || done.cid != which {
			return Err(Error::Corrupt);
		}
		Ok(done)
	}

	// The modem's whole state, from the five queries.
	fn state(&mut self, hc: &mut Xhci, hids: &mut Hids) -> Result<DeviceState, Error> {
		let ready = cid::subscriber_ready(&self.query(hc, hids, cid::CID_SUBSCRIBER_READY_STATUS, &[], false)?.info).map_err(|_| Error::Corrupt)?;
		// A SIM THAT ARRIVED, OR ONE WHOSE ICC ID CHANGED, is a new SIM generation.
		let present = ready.state != cid::READY_SIM_NOT_INSERTED;
		let iccid = if present { ready.iccid.clone() } else { None };
		if self.sim_generation == 0 || iccid != self.iccid {
			self.sim_generation += 1;
			self.iccid = iccid;
		}
		let (mut pin_attempts, mut puk_attempts) = (None, None);
		let sim = match ready.state {
			cid::READY_INITIALIZED => DeviceSim::Ready,
			cid::READY_SIM_NOT_INSERTED => DeviceSim::Absent,
			cid::READY_BAD_SIM => DeviceSim::Blocked,
			cid::READY_DEVICE_LOCKED => {
				let pin = cid::pin_info(&self.query(hc, hids, cid::CID_PIN, &[], false)?.info).map_err(|_| Error::Corrupt)?;
				match (pin.pin_type, pin.state) {
					(cid::PIN_TYPE_PIN1, cid::PIN_STATE_LOCKED) => {
						pin_attempts = Some(pin.remaining.min(255) as u8);
						DeviceSim::LockedPin
					}
					(cid::PIN_TYPE_PUK1, cid::PIN_STATE_LOCKED) => {
						puk_attempts = Some(pin.remaining.min(255) as u8);
						DeviceSim::LockedPuk
					}
					_ => DeviceSim::Unknown,
				}
			}
			_ => DeviceSim::Unknown,
		};
		let register = cid::register_state(&self.query(hc, hids, cid::CID_REGISTER_STATE, &[], false)?.info).map_err(|_| Error::Corrupt)?;
		let registration = match register.state {
			cid::REGISTER_HOME => DeviceRegistration::Home,
			cid::REGISTER_ROAMING | cid::REGISTER_PARTNER => DeviceRegistration::Roaming,
			cid::REGISTER_SEARCHING => DeviceRegistration::Searching,
			cid::REGISTER_DEREGISTERED => DeviceRegistration::NotRegistered,
			cid::REGISTER_DENIED => DeviceRegistration::Denied,
			_ => DeviceRegistration::Unknown,
		};
		let signal = cid::signal_dbm(&self.query(hc, hids, cid::CID_SIGNAL_STATE, &[], false)?.info).map_err(|_| Error::Corrupt)?;
		let connect = cid::connect_info(&self.query(hc, hids, cid::CID_CONNECT, &cid::session_query(SESSION as u32), false)?.info).map_err(|_| Error::Corrupt)?;
		let context_active = connect.activation == cid::ACTIVATION_ACTIVATED;
		if !context_active {
			self.context = None;
		}
		Ok(DeviceState {
			manufacturer: self.manufacturer.clone(),
			model: self.model.clone(),
			sim,
			sim_generation: self.sim_generation,
			registration,
			operator: register.provider,
			signal_valid: signal.is_some(),
			rssi_dbm: signal.unwrap_or(0),
			// The RSSI code's own scale, 0 to 31, as a percentage.
			quality: signal.map_or(0, |dbm| ((dbm + 113) * 100 / 62).clamp(0, 100) as u8),
			pin_attempts,
			puk_attempts,
			context_active,
		})
	}

	// An indication noted since the last one was published, published now with a fresh state.
	fn publish_indication(&mut self, hc: &mut Xhci, hids: &mut Hids) {
		if !self.indicated || self.indications == 0 {
			return;
		}
		self.indicated = false;
		let Ok(state) = self.state(hc, hids) else { return };
		self.revision += 1;
		let indication = Indication { revision: self.revision, state };
		let mut frame = [0u8; 512];
		let mut handles = wire::Handles::new();
		if let Some(len) = modem_device::indications_frame(self.indications_seq, &indication, &mut frame, &mut handles)
			&& try_send(self.indications, &frame[..len], 0)
		{
			self.indications_seq += 1;
		}
	}

	fn command(&mut self, hc: &mut Xhci, hids: &mut Hids, mut command: Command) -> Result<CommandReply, Error> {
		let mut reply = CommandReply { connection_generation: command.connection_generation, transaction: command.transaction, kind: command.kind, status: CommandStatus::Done, sim_generation: self.sim_generation, context_generation: command.context_generation, state: None, config: None, identity: None };
		let outcome = (|| -> Result<(), Error> {
			// A COMMAND PREPARED AGAINST ANOTHER SIM OR ANOTHER CONNECTION is stale, and does nothing.
			if command.connection_generation != self.connection || command.sim_generation != self.sim_generation {
				reply.status = CommandStatus::Stale;
				return Ok(());
			}
			match command.kind {
				CommandKind::Query => {}
				CommandKind::EnterPin | CommandKind::EnterPuk => {
					let secret = core::str::from_utf8(&command.secret).map_err(|_| Error::Invalid)?;
					let new_secret = core::str::from_utf8(&command.new_secret).map_err(|_| Error::Invalid)?;
					let pin_type = if command.kind == CommandKind::EnterPin { cid::PIN_TYPE_PIN1 } else { cid::PIN_TYPE_PUK1 };
					let mut info = cid::pin_set(pin_type, secret, if command.kind == CommandKind::EnterPuk { new_secret } else { "" });
					let done = self.query(hc, hids, cid::CID_PIN, &info, true);
					for byte in info.iter_mut() {
						unsafe { core::ptr::write_volatile(byte, 0) };
					}
					reply.status = if done?.status == cid::STATUS_SUCCESS { CommandStatus::Done } else { CommandStatus::Rejected };
				}
				CommandKind::Activate => {
					if command.apn.len() > cid::MAX_APN {
						return Err(Error::Invalid);
					}
					let state = self.state(hc, hids)?;
					if state.sim != DeviceSim::Ready {
						reply.status = CommandStatus::Rejected;
						return Ok(());
					}
					if self.context.is_some() {
						reply.status = CommandStatus::Failed;
						return Ok(());
					}
					let done = self.query(hc, hids, cid::CID_CONNECT, &cid::connect_set(SESSION as u32, true, &command.apn), true)?;
					let connect = cid::connect_info(&done.info).map_err(|_| Error::Corrupt)?;
					if done.status != cid::STATUS_SUCCESS || connect.activation != cid::ACTIVATION_ACTIVATED {
						reply.status = CommandStatus::Failed;
						return Ok(());
					}
					self.context = Some(command.context_generation);
					let configuration = self.query(hc, hids, cid::CID_IP_CONFIGURATION, &cid::session_query(SESSION as u32), false)?;
					reply.config = Some(match cid::ipv4_configuration(&configuration.info).map_err(|_| Error::Corrupt)? {
						Some(ipv4) => IpConfig { family: IpFamily::Ipv4, address: ipv4.address, prefix: ipv4.prefix, gateway: ipv4.gateway, dns: ipv4.dns, mtu: ipv4.mtu },
						// AN IPV6-ONLY CONTEXT says so, and its IPv4 fields mean nothing.
						None => IpConfig { family: IpFamily::Ipv6, address: 0, prefix: 0, gateway: None, dns: Vec::new(), mtu: 0 },
					});
				}
				CommandKind::Deactivate => {
					if self.context != Some(command.context_generation) {
						reply.status = CommandStatus::Stale;
						return Ok(());
					}
					let done = self.query(hc, hids, cid::CID_CONNECT, &cid::connect_set(SESSION as u32, false, ""), true)?;
					if done.status != cid::STATUS_SUCCESS {
						reply.status = CommandStatus::Failed;
						return Ok(());
					}
					self.context = None;
				}
				CommandKind::Identity => {
					let ready = cid::subscriber_ready(&self.query(hc, hids, cid::CID_SUBSCRIBER_READY_STATUS, &[], false)?.info).map_err(|_| Error::Corrupt)?;
					if ready.state == cid::READY_SIM_NOT_INSERTED {
						reply.status = CommandStatus::Failed;
					} else {
						reply.identity = Some(DeviceIdentity { imsi: ready.imsi, iccid: ready.iccid, msisdn: ready.number });
					}
				}
			}
			Ok(())
		})();
		// ZEROED, whatever the command was: nothing it carried outlives its handling.
		for byte in command.secret.iter_mut().chain(command.new_secret.iter_mut()) {
			unsafe { core::ptr::write_volatile(byte, 0) };
		}
		outcome?;
		reply.state = Some(self.state(hc, hids)?);
		reply.sim_generation = self.sim_generation;
		Ok(reply)
	}

	// Datagrams from the service, as far as one transfer block carries them.
	fn transmit_ready(&mut self, hc: &mut Xhci) {
		let mut datagrams: Vec<Vec<u8>> = Vec::new();
		let mut size = 64usize;
		let mut buf = alloc::vec![0u8; 4200];
		while !self.data_out.busy && size < BLOCK_BYTES as usize - 1600 {
			match try_recv_caps(self.transmit, &mut buf) {
				PolledCaps::Message { len, handles } => {
					for &handle in handles.as_slice() {
						close(handle);
					}
					let Some(datagram) = Datagram::decode(&buf[..len]) else { continue };
					// ONLY THE ACTIVE CONTEXT'S, and none too big for the link.
					if Some(datagram.context_generation) != self.context || datagram.bytes.is_empty() || datagram.bytes.len() > MTU as usize {
						continue;
					}
					size += datagram.bytes.len() + 8;
					datagrams.push(datagram.bytes);
				}
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					close(self.transmit);
					self.transmit = 0;
					break;
				}
			}
		}
		if datagrams.is_empty() {
			return;
		}
		self.sequence = self.sequence.wrapping_add(1);
		let slices: Vec<&[u8]> = datagrams.iter().map(Vec::as_slice).collect();
		let block = mbim::block(SESSION, &slices, self.sequence);
		if self.data_out.fill(&block) {
			self.data_out.post(hc, block.len() as u32);
		}
	}

	fn post_receives(&mut self, hc: &mut Xhci) {
		if !self.notify.busy {
			let length = self.notify.mps.min(64);
			self.notify.post(hc, length);
		}
		if !self.data_in.busy {
			self.data_in.post(hc, BLOCK_BYTES);
		}
	}

	fn close_session(&mut self) {
		for handle in [&mut self.indications, &mut self.receive, &mut self.transmit] {
			if *handle != 0 {
				close(*handle);
				*handle = 0;
			}
		}
		self.indications_seq = 0;
		self.receive_seq = 0;
	}
}

struct View<'a> {
	mbim: &'a mut Mbim,
	hc: &'a mut Xhci,
	hids: &'a mut Hids,
}

impl modem_device::Service for View<'_> {
	// A NEW SESSION IS A NEW CONNECTION GENERATION, and the modem is opened again under it.
	fn open(&mut self, version: u32) -> Result<DeviceOpen, Error> {
		if version != VERSION {
			return Err(Error::Unsupported);
		}
		let transaction = self.mbim.next_transaction();
		let message = self.mbim.exchange(self.hc, self.hids, alloc::vec![cid::open(transaction, self.mbim.binding.max_control as u32)], cid::OPEN_DONE, transaction).ok_or(Error::TimedOut)?;
		if cid::open_status(&message.body) != Some(cid::STATUS_SUCCESS) {
			return Err(Error::Io);
		}
		self.mbim.connection += 1;
		self.mbim.context = None;
		// THE SIM GENERATION IS THE MODEM'S FROM THE START: read now, so the first command is held against it and
		// not against a zero this side had not yet replaced.
		let _ = self.mbim.state(self.hc, self.hids);
		let limits = DeviceLimits { version: VERSION, pending: 1, fragments: 16, message_bytes: 16 * 1024, assembly_bytes: 64 * 1024, assemblies: 4, mtu: MTU };
		Ok(DeviceOpen { limits, connection_generation: self.mbim.connection })
	}

	fn command(&mut self, command: Command) -> Result<CommandReply, Error> {
		let reply = self.mbim.command(self.hc, self.hids, command);
		self.mbim.publish_indication(self.hc, self.hids);
		reply
	}

	fn indications(&mut self) -> Vec<Indication> {
		Vec::new()
	}

	fn receive(&mut self) -> Vec<Datagram> {
		Vec::new()
	}

	fn transmit(&mut self) -> Result<u64, Error> {
		let (mine, theirs) = channel_with_depth(STREAM_DEPTH).ok_or(Error::Exhausted)?;
		if self.mbim.transmit != 0 {
			close(self.mbim.transmit);
		}
		self.mbim.transmit = mine;
		Ok(theirs)
	}
}

impl Module for Mbim {
	fn kind(&self) -> ClassKind {
		ClassKind::Mbim
	}

	fn inventory(&self) -> u8 {
		KIND_MODEM
	}

	fn provider(&self) -> u16 {
		driver_protocol::provider::MODEM
	}

	fn name(&self) -> &'static [u8] {
		driver_protocol::provider::USB_MBIM_NAME
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
		match classes::correlation(&request).map(|(op, _)| op) {
			Some(op @ (modem_device::OP_INDICATIONS | modem_device::OP_RECEIVE)) => {
				let mut view = View { mbim: self, hc, hids };
				let corr = if op == modem_device::OP_INDICATIONS { modem_device::indications_open(&mut view, &request, &mut handles).map(|(corr, _)| corr) } else { modem_device::receive_open(&mut view, &request, &mut handles).map(|(corr, _)| corr) };
				let Some(corr) = corr else { return true };
				let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
				let slot = if op == modem_device::OP_INDICATIONS { &mut self.indications } else { &mut self.receive };
				if *slot != 0 {
					close(*slot);
				}
				*slot = producer;
				send_caps_blocking(chan, &corr.to_le_bytes(), &[consumer]);
			}
			_ => {
				let mut reply = alloc::vec![0u8; 2048];
				let mut reply_handles = wire::Handles::new();
				let mut view = View { mbim: self, hc, hids };
				if let Some(written) = modem_device::dispatch(&mut view, &request, &mut handles, &mut reply, &mut reply_handles) {
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
		self.close_session();
		let _ = classes::abandon(hc, hids, &mut self.data_out);
	}

	fn absorb(&mut self, hc: &mut Xhci, hids: &mut Hids, _pointer: u64, status: u32, control: u32) -> bool {
		if self.notify.owns(control) {
			let (code, moved) = self.notify.complete(status);
			if classes::succeeded(code) && moved >= 2 && self.notify.read(2)[1] == cid::NOTIFY_RESPONSE_AVAILABLE {
				let _ = self.collect(hc, hids, None);
				self.publish_indication(hc, hids);
			} else if classes::stalled(code) {
				let _ = classes::clear_halt(hc, hids, &mut self.dev, &mut self.notify);
			}
			self.post_receives(hc);
			return true;
		}
		if self.data_in.owns(control) {
			let (code, moved) = self.data_in.complete(status);
			if classes::succeeded(code) && moved > 0 {
				let block = self.data_in.read(moved as usize);
				if let (Some(context), Ok(ranges)) = (self.context, mbim::datagrams(&block, SESSION, MTU)) {
					for range in ranges {
						let datagram = Datagram { context_generation: context, bytes: block[range].to_vec() };
						let mut frame = alloc::vec![0u8; datagram.bytes.len() + 64];
						let mut handles = wire::Handles::new();
						if self.receive != 0
							&& let Some(len) = modem_device::receive_frame(self.receive_seq, &datagram, &mut frame, &mut handles)
							&& try_send(self.receive, &frame[..len], 0)
						{
							self.receive_seq += 1;
						}
					}
				}
			} else if classes::stalled(code) {
				let _ = classes::clear_halt(hc, hids, &mut self.dev, &mut self.data_in);
			}
			self.post_receives(hc);
			return true;
		}
		if self.data_out.owns(control) {
			let (code, _) = self.data_out.complete(status);
			if classes::stalled(code) {
				let _ = classes::clear_halt(hc, hids, &mut self.dev, &mut self.data_out);
			}
			// THE PIPE IS FREE: whatever the service queued meanwhile goes now.
			if self.transmit != 0 {
				self.transmit_ready(hc);
			}
			return true;
		}
		false
	}

	fn waits(&self) -> Vec<u64> {
		if self.transmit != 0 && !self.data_out.busy { alloc::vec![self.transmit] } else { Vec::new() }
	}

	fn ready(&mut self, hc: &mut Xhci, _hids: &mut Hids, handle: u64) {
		if handle == self.transmit {
			self.transmit_ready(hc);
		}
	}

	fn release(&mut self, hc: &mut Xhci) {
		self.close_session();
		self.notify.release(hc);
		self.data_in.release(hc);
		self.data_out.release(hc);
	}
}
