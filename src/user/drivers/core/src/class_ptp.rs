// The USB Still Image class side of driver.xhci: a PTP camera (or an MTP device, which is PTP with more
// operations) on the bus, published as the `ptp-transport` provider MediaImportService drives.
//
// BYTES OUT, BYTES IN, AND NOTHING UNDERSTOOD. A command container goes down the bulk OUT pipe whole and is
// answered when the device took it; bulk-IN bytes are kept as they arrive and handed out by `pull`, `again`
// while none have; event containers from the interrupt pipe go onto the consumer's stream, eight at most
// waiting. What the containers SAY is MediaImportService's to validate - this side reads one field of them,
// the transaction ID of the last command, because the class's Cancel Request has to name it.
//
// STANDING TRANSFERS, ONE PER PIPE. The bulk IN receive is posted again only once what it delivered has been
// pulled, so a consumer that stops pulling stops the device - the page is the bound, not a queue that grows.
// The event pipe is posted again as soon as its container is on the stream, or counted as lost if the stream
// is full.

use alloc::vec::Vec;
use proto::system::{Error, PtpAttach, PtpEvent, PtpEventContainer, ptp_transport};
use rt::*;

use crate::classes::{self, Module, Pipe};
use crate::usb_hid::Hids;
use crate::{KIND_STILL_IMAGE, UsbDevice, Xhci, control_in_req, control_nodata, control_out_req};
use drivers::common;
use drivers::ptp::{self, Binding};
use drivers::usb_class::ClassKind;

const VERSION: u32 = 1;
// The largest request: opcode, correlation, attachment, and a 32-byte command container.
const REQUEST_BYTES: usize = 2 + 4 + 8 + 2 + ptp::MAX_COMMAND;
// A receive takes a page; a pull hands out at most that.
const RECEIVE_BYTES: u32 = 4096;
// Events waiting on the stream before later ones are dropped.
const EVENT_DEPTH: u64 = 8;
// How many times a cancel reads the device status while the device says it is busy.
const STATUS_TRIES: u32 = 64;

pub struct Ptp {
	dev: UsbDevice,
	binding: Binding,
	out: Pipe,
	input: Pipe,
	events: Pipe,
	attachment: u64,
	consumer: u64,
	// The command the device has not finished taking: who asked, under which correlation, and its length.
	pending: Option<(u64, u32, u32)>,
	// Bulk-IN bytes that arrived, and how many of them have been pulled.
	arrived: Vec<u8>,
	pulled: usize,
	// The transaction of the last command, which a cancel names.
	transaction: u32,
	// The consumer's event stream, its sequence, and whether events were dropped since it last had room.
	stream: u64,
	stream_seq: u32,
	lost: bool,
	buf: Vec<u8>,
}

/// Bring a bound camera's three pipes up and select its setting. The device comes back on failure.
///
/// # Safety
/// `dev` is an addressed device this controller owns.
pub unsafe fn configure(hc: &mut Xhci, mut dev: UsbDevice, binding: Binding) -> Result<Ptp, UsbDevice> {
	unsafe {
		let pipes = (Pipe::new(dev.slot, dev.speed, &binding.bulk_out), Pipe::new(dev.slot, dev.speed, &binding.bulk_in), Pipe::new(dev.slot, dev.speed, &binding.interrupt_in));
		let (Some(mut out), Some(mut input), Some(mut events)) = pipes else {
			let (a, b, c) = pipes;
			for mut pipe in [a, b, c].into_iter().flatten() {
				pipe.release(hc);
			}
			print(b"driver.xhci: a PTP camera is on the bus and no pages were left for its pipes\n");
			return Err(dev);
		};
		if !classes::configure_pipes(hc, &mut dev, &[&out, &input, &events], 0) || !classes::select(hc, &mut dev, binding.config_value, binding.interface, binding.alternate) {
			out.release(hc);
			input.release(hc);
			events.release(hc);
			print(b"driver.xhci: a PTP camera's pipes or its setting were refused\n");
			return Err(dev);
		}
		let mut line: common::Bounded<96> = common::Bounded::new();
		line.push(b"driver.xhci: still image camera bound - a PTP transport, ");
		line.decimal(input.mps as u64);
		line.push(b"-byte packets\n");
		print(line.as_bytes());
		Ok(Ptp { dev, binding, out, input, events, attachment: 0, consumer: 0, pending: None, arrived: Vec::new(), pulled: 0, transaction: 0, stream: 0, stream_seq: 0, lost: false, buf: alloc::vec![0u8; REQUEST_BYTES] })
	}
}

impl Ptp {
	fn holds(&self, chan: u64, attachment: u64) -> Result<(), Error> {
		if self.consumer == 0 || self.consumer != chan {
			return Err(Error::Invalid);
		}
		if attachment != self.attachment {
			return Err(Error::Stale);
		}
		Ok(())
	}

	// The two standing receives, each posted when it is not already.
	fn post_receives(&mut self, hc: &mut Xhci) {
		if self.consumer == 0 {
			return;
		}
		if !self.input.busy && self.pulled >= self.arrived.len() {
			self.arrived.clear();
			self.pulled = 0;
			self.input.post(hc, RECEIVE_BYTES);
		}
		if !self.events.busy {
			let length = self.events.mps.min(ptp::MAX_EVENT as u32 * 2);
			self.events.post(hc, length);
		}
	}

	// Everything this attachment had in flight is abandoned and forgotten: a command still going answers `io`,
	// and bytes of a transaction nobody will pull are dropped.
	fn quiesce(&mut self, hc: &mut Xhci, hids: &mut Hids) {
		if let Some((chan, corr, _)) = self.pending.take() {
			let _ = classes::abandon(hc, hids, &mut self.out);
			classes::answer_unit(chan, corr, Err(Error::Io));
		}
		let _ = classes::abandon(hc, hids, &mut self.input);
		let _ = classes::abandon(hc, hids, &mut self.events);
		self.arrived.clear();
		self.pulled = 0;
	}

	fn close_stream(&mut self) {
		if self.stream != 0 {
			close(self.stream);
			self.stream = 0;
		}
		self.stream_seq = 0;
		self.lost = false;
	}

	// One event on the stream, without waiting. A stream with no room loses this one - and says so first
	// thing when it has room again.
	fn deliver(&mut self, event: PtpEvent) {
		if self.stream == 0 {
			self.lost = true;
			return;
		}
		let mut frame = [0u8; 128];
		let mut handles = wire::Handles::new();
		if self.lost {
			let Some(len) = ptp_transport::events_frame(self.stream_seq, &PtpEvent::Overflow(self.attachment), &mut frame, &mut handles) else { return };
			if !try_send(self.stream, &frame[..len], 0) {
				return;
			}
			self.stream_seq += 1;
			self.lost = false;
		}
		let Some(len) = ptp_transport::events_frame(self.stream_seq, &event, &mut frame, &mut handles) else { return };
		if try_send(self.stream, &frame[..len], 0) {
			self.stream_seq += 1;
		} else {
			self.lost = true;
		}
	}

	// A command container, taken apart by hand because it is answered when the device took it.
	fn command(&mut self, hc: &mut Xhci, chan: u64, corr: u32, len: usize) {
		let result = (|| {
			let request = &self.buf[..len];
			if request.len() < 16 {
				return Err(Error::Invalid);
			}
			let attachment = u64::from_le_bytes(request[6..14].try_into().map_err(|_| Error::Invalid)?);
			let count = u16::from_le_bytes([request[14], request[15]]) as usize;
			if count == 0 || count > ptp::MAX_COMMAND || request.len() != 16 + count {
				return Err(Error::Invalid);
			}
			self.holds(chan, attachment)?;
			// ONE TRANSACTION AT A TIME, and one command container of it in flight.
			if self.pending.is_some() {
				return Err(Error::Invalid);
			}
			let container = &request[16..];
			self.transaction = ptp::transaction(container);
			if !self.out.fill(container) || !self.out.post(hc, count as u32) {
				return Err(Error::Io);
			}
			self.pending = Some((chan, corr, count as u32));
			Ok(())
		})();
		if let Err(error) = result {
			classes::answer_unit(chan, corr, Err(error));
		}
	}

	// The device's status after a cancel: read until it is not busy, clearing every pipe it says it stalled.
	fn settle_status(&mut self, hc: &mut Xhci, hids: &mut Hids) -> Result<(), Error> {
		let interface = self.binding.interface as u16;
		for _ in 0..STATUS_TRIES {
			let received = control_in_req(hc, hids, &mut self.dev, ptp::RT_CLASS_INTERFACE_IN, ptp::REQ_GET_DEVICE_STATUS, 0, interface, ptp::MAX_STATUS as u16).ok_or(Error::Io)?;
			// A DEVICE THAT ANSWERS THE STATUS REQUEST WITH NOTHING has no status to give - QEMU's `usb-mtp` is
			// one - and the cancel it acknowledged stands. What is refused is a status that is malformed.
			if received == 0 {
				return Ok(());
			}
			let bytes: Vec<u8> = (0..received.min(ptp::MAX_STATUS as u32) as u64).map(|at| unsafe { crate::r8(self.dev.data_virt + at) }).collect();
			let status = ptp::device_status(&bytes).ok_or(Error::Io)?;
			for address in status.stalled {
				for pipe in [&mut self.out, &mut self.input, &mut self.events] {
					let pipe_address = (pipe.dci >> 1) as u8 | if pipe.is_in() { 0x80 } else { 0 };
					if pipe_address == address {
						let _ = classes::clear_halt(hc, hids, &mut self.dev, pipe);
					}
				}
			}
			if status.code != ptp::STATUS_DEVICE_BUSY {
				return Ok(());
			}
			yield_now();
		}
		Err(Error::TimedOut)
	}
}

struct View<'a> {
	ptp: &'a mut Ptp,
	hc: &'a mut Xhci,
	hids: &'a mut Hids,
	chan: u64,
}

impl ptp_transport::Service for View<'_> {
	fn attach(&mut self, version: u32) -> Result<PtpAttach, Error> {
		if version != VERSION {
			return Err(Error::Unsupported);
		}
		if self.ptp.consumer != 0 {
			return Err(Error::Invalid);
		}
		self.ptp.attachment += 1;
		self.ptp.consumer = self.chan;
		self.ptp.post_receives(self.hc);
		Ok(PtpAttach { version: VERSION, attachment: self.ptp.attachment })
	}

	fn command(&mut self, _attachment: u64, _container: Vec<u8>) -> Result<(), Error> {
		// Taken before dispatch, because it is answered when the device took it.
		Err(Error::Invalid)
	}

	fn pull(&mut self, attachment: u64, max: u16) -> Result<Vec<u8>, Error> {
		self.ptp.holds(self.chan, attachment)?;
		if max == 0 {
			return Err(Error::Invalid);
		}
		let waiting = self.ptp.arrived.len() - self.ptp.pulled;
		if waiting == 0 {
			self.ptp.post_receives(self.hc);
			// A consumer asking is a reason to look again - see `Pipe::kick`.
			self.ptp.input.kick(self.hc);
			return Err(Error::Again);
		}
		let take = waiting.min(max as usize).min(RECEIVE_BYTES as usize);
		let bytes = self.ptp.arrived[self.ptp.pulled..self.ptp.pulled + take].to_vec();
		self.ptp.pulled += take;
		// THE PAGE IS FREE AGAIN once everything it delivered is out, and only then is the receive posted.
		self.ptp.post_receives(self.hc);
		Ok(bytes)
	}

	fn events(&mut self) -> Vec<PtpEvent> {
		Vec::new()
	}

	fn cancel(&mut self, attachment: u64) -> Result<(), Error> {
		self.ptp.holds(self.chan, attachment)?;
		let interface = self.ptp.binding.interface as u16;
		let request = ptp::cancel_request(self.ptp.transaction);
		unsafe { core::ptr::copy_nonoverlapping(request.as_ptr(), self.ptp.dev.data_virt as *mut u8, request.len()) };
		let sent = control_out_req(self.hc, self.hids, &mut self.ptp.dev, ptp::RT_CLASS_INTERFACE_OUT, ptp::REQ_CANCEL, 0, interface, request.len() as u16);
		// WHAT THE CANCELLED TRANSACTION LEFT IS DROPPED, sent or not: nobody pulls it any more.
		if let Some((chan, corr, _)) = self.ptp.pending.take() {
			let _ = classes::abandon(self.hc, self.hids, &mut self.ptp.out);
			classes::answer_unit(chan, corr, Err(Error::Cancelled));
		}
		let _ = classes::abandon(self.hc, self.hids, &mut self.ptp.input);
		self.ptp.arrived.clear();
		self.ptp.pulled = 0;
		sent.ok_or(Error::Io)?;
		let settled = self.ptp.settle_status(self.hc, self.hids);
		self.ptp.post_receives(self.hc);
		settled
	}

	fn reset(&mut self, attachment: u64) -> Result<PtpAttach, Error> {
		self.ptp.holds(self.chan, attachment)?;
		// THE ATTACHMENT IS OVER WHATEVER HAPPENS NEXT.
		self.ptp.consumer = 0;
		self.ptp.quiesce(self.hc, self.hids);
		let interface = self.ptp.binding.interface as u16;
		control_nodata(self.hc, self.hids, &mut self.ptp.dev, ptp::RT_CLASS_INTERFACE_OUT, ptp::REQ_DEVICE_RESET, 0, interface).ok_or(Error::Io)?;
		// Every pipe back to where it began: the device reset its ends of them.
		let entries = self.ptp.out.dci.max(self.ptp.input.dci).max(self.ptp.events.dci);
		for pipe in [&mut self.ptp.out, &mut self.ptp.input, &mut self.ptp.events] {
			if !classes::reset_pipe(self.hc, self.hids, &mut self.ptp.dev, pipe, entries) {
				return Err(Error::Io);
			}
		}
		self.ptp.attachment += 1;
		self.ptp.consumer = self.chan;
		self.ptp.post_receives(self.hc);
		Ok(PtpAttach { version: VERSION, attachment: self.ptp.attachment })
	}
}

impl Module for Ptp {
	fn kind(&self) -> ClassKind {
		ClassKind::StillImage
	}

	fn inventory(&self) -> u8 {
		KIND_STILL_IMAGE
	}

	fn provider(&self) -> u16 {
		driver_protocol::provider::PTP_TRANSPORT
	}

	fn name(&self) -> &'static [u8] {
		driver_protocol::provider::USB_STILL_IMAGE_NAME
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
		match classes::correlation(&self.buf[..len]) {
			Some((ptp_transport::OP_COMMAND, corr)) => {
				for &handle in handles.as_slice() {
					close(handle);
				}
				self.command(hc, chan, corr, len);
			}
			// THE EVENT STREAM, opened by hand: its answer carries the consumer's end, and this side keeps the
			// producer.
			Some((ptp_transport::OP_EVENTS, _)) => {
				let request = self.buf[..len].to_vec();
				let mut view = View { ptp: self, hc, hids, chan };
				let Some((corr, _)) = ptp_transport::events_open(&mut view, &request, &mut handles) else { return true };
				if self.consumer != chan {
					classes::answer_unit(chan, corr, Err(Error::Invalid));
					return true;
				}
				let Some((producer, consumer)) = channel_with_depth(EVENT_DEPTH) else { return true };
				let was_lost = self.lost;
				self.close_stream();
				self.stream = producer;
				self.lost = was_lost;
				send_caps_blocking(chan, &corr.to_le_bytes(), &[consumer]);
			}
			_ => {
				let request = self.buf[..len].to_vec();
				let mut reply = [0u8; 4200];
				let mut reply_handles = wire::Handles::new();
				let mut view = View { ptp: self, hc, hids, chan };
				if let Some(written) = ptp_transport::dispatch(&mut view, &request, &mut handles, &mut reply, &mut reply_handles) {
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
		self.quiesce(hc, hids);
		self.close_stream();
	}

	fn absorb(&mut self, hc: &mut Xhci, hids: &mut Hids, _pointer: u64, status: u32, control: u32) -> bool {
		if self.out.owns(control) {
			let (code, moved) = self.out.complete(status);
			if classes::stalled(code) {
				let _ = classes::clear_halt(hc, hids, &mut self.dev, &mut self.out);
			}
			// SENT WHOLE OR NOT AT ALL, and a part is an error: the service ends the session rather than guess.
			if let Some((chan, corr, length)) = self.pending.take() {
				classes::answer_unit(chan, corr, if classes::succeeded(code) && moved == length { Ok(()) } else { Err(Error::Io) });
			}
			// THE DEVICE HAS A REASON TO ANSWER NOW, on the other pipe - see `Pipe::kick`.
			self.input.kick(hc);
			return true;
		}
		if self.input.owns(control) {
			let (code, moved) = self.input.complete(status);
			if classes::succeeded(code) && moved > 0 {
				self.arrived = self.input.read(moved as usize);
				self.pulled = 0;
			} else if classes::stalled(code) {
				let _ = classes::clear_halt(hc, hids, &mut self.dev, &mut self.input);
			}
			// A ZERO-LENGTH PACKET ENDS A DATA PHASE and carries nothing; the receive is posted again.
			self.post_receives(hc);
			return true;
		}
		if self.events.owns(control) {
			let (code, moved) = self.events.complete(status);
			if classes::succeeded(code) && moved > 0 {
				let bytes = self.events.read((moved as usize).min(ptp::MAX_EVENT));
				self.deliver(PtpEvent::Container(PtpEventContainer { attachment: self.attachment, bytes }));
			} else if classes::stalled(code) {
				let _ = classes::clear_halt(hc, hids, &mut self.dev, &mut self.events);
			}
			self.post_receives(hc);
			return true;
		}
		false
	}

	fn release(&mut self, hc: &mut Xhci) {
		self.pending = None;
		self.close_stream();
		self.out.release(hc);
		self.input.release(hc);
		self.events.release(hc);
	}
}
