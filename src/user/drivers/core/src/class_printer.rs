// The USB printer class side of driver.xhci: a printer on the bus, published as the `printer` provider
// SpoolService drives over `liber:printer-device`'s backend contract.
//
// WHAT IT DOES AND DOES NOT. It moves an already-rendered document down the bulk OUT pipe, reads the port
// status byte and the IEEE 1284 device ID with the class's own requests, and resets the port with SOFT_RESET.
// Admission, ordering, deadlines and recovery are SpoolService's; there is no raw control request, and
// nothing here reads what a document says.
//
// A WRITE IS ANSWERED WHEN THE PRINTER TOOK IT. The transfer is posted and the loop goes back to serving the
// bus; the answer is sent from the completion, with exactly the count the controller reports moved - so a
// printer that is slow to read makes the write slow, not the keyboard. A printer that never takes it is
// SpoolService's to give up on: it resets the port after thirty seconds without progress, and the reset
// abandons the transfer here before it resets anything.
//
// ONE CONSUMER, BY ATTACHMENT. `attach` names the connection that holds the printer and a generation for it;
// every later request must name that generation, and a reset ends it - a new one on success, none on failure.

use alloc::vec::Vec;
use proto::system::{Error, PortReading, PrinterAttach, printer_backend};
use rt::*;

use crate::classes::{self, Module, Pipe};
use crate::usb_hid::Hids;
use crate::{KIND_PRINTER, UsbDevice, Xhci, control_in_req, control_nodata, r8};
use drivers::common;
use drivers::printer::{self, Binding};
use drivers::usb_class::ClassKind;

const VERSION: u32 = 1;
// The largest request the contract sends: opcode, correlation, attachment, and a 4096-byte write.
const REQUEST_BYTES: usize = 2 + 4 + 8 + 2 + 4096;
// The longest device ID read, which is also the data page.
const DEVICE_ID_BYTES: u16 = 4096;

pub struct Printer {
	dev: UsbDevice,
	binding: Binding,
	out: Pipe,
	// The attachment generation, and the connection holding it (zero when nobody does).
	attachment: u64,
	consumer: u64,
	// The write the printer has not finished taking: who asked, and under which correlation.
	pending: Option<(u64, u32)>,
	buf: Vec<u8>,
}

/// Bring a bound printer's OUT pipe up and select its setting. The device comes back on failure, still
/// addressed, for the inventory to hold.
///
/// # Safety
/// `dev` is an addressed device this controller owns.
pub unsafe fn configure(hc: &mut Xhci, mut dev: UsbDevice, binding: Binding) -> Result<Printer, UsbDevice> {
	unsafe {
		let Some(mut out) = Pipe::new(dev.slot, dev.speed, &binding.bulk_out) else {
			print(b"driver.xhci: a printer is on the bus and no pages were left for its pipe\n");
			return Err(dev);
		};
		if !classes::configure_pipes(hc, &mut dev, &[&out], 0) {
			out.release(hc);
			print(b"driver.xhci: a printer's bulk OUT endpoint was refused by the controller\n");
			return Err(dev);
		}
		if !classes::select(hc, &mut dev, binding.config_value, binding.interface, binding.alternate) {
			out.release(hc);
			print(b"driver.xhci: a printer refused its configuration or its printing setting\n");
			return Err(dev);
		}
		let mut line: common::Bounded<96> = common::Bounded::new();
		line.push(b"driver.xhci: printer bound - ");
		line.push(if binding.protocol == printer::PROTOCOL_BIDIRECTIONAL { b"bidirectional".as_slice() } else { b"unidirectional" });
		line.push(b", ");
		line.decimal(out.mps as u64);
		line.push(b"-byte packets\n");
		print(line.as_bytes());
		Ok(Printer { dev, binding, out, attachment: 0, consumer: 0, pending: None, buf: alloc::vec![0u8; REQUEST_BYTES] })
	}
}

impl Printer {
	// The device ID exactly as the printer gave it, up to its own declared length when that length is one.
	fn device_id(&mut self, hc: &mut Xhci, hids: &mut Hids) -> Result<Vec<u8>, Error> {
		let received = control_in_req(hc, hids, &mut self.dev, printer::RT_CLASS_INTERFACE_IN, printer::REQ_GET_DEVICE_ID, 0, self.binding.device_id_index(), DEVICE_ID_BYTES).ok_or(Error::Io)?;
		let bytes: Vec<u8> = (0..received.min(DEVICE_ID_BYTES as u32) as u64).map(|at| unsafe { r8(self.dev.data_virt + at) }).collect();
		let length = printer::device_id_length(&bytes).unwrap_or(bytes.len());
		Ok(bytes[..length].to_vec())
	}

	fn port(&mut self, hc: &mut Xhci, hids: &mut Hids) -> Result<u8, Error> {
		let received = control_in_req(hc, hids, &mut self.dev, printer::RT_CLASS_INTERFACE_IN, printer::REQ_GET_PORT_STATUS, 0, self.binding.interface as u16, 1).ok_or(Error::Io)?;
		if received < 1 {
			return Err(Error::Io);
		}
		Ok(unsafe { r8(self.dev.data_virt) })
	}

	// A request that names an attachment must be from the connection holding it, and name the live one.
	fn holds(&self, chan: u64, attachment: u64) -> Result<(), Error> {
		if self.consumer == 0 || self.consumer != chan {
			return Err(Error::Invalid);
		}
		if attachment != self.attachment {
			return Err(Error::Stale);
		}
		Ok(())
	}

	// A write, taken apart by hand because it is answered later: the attachment, the length and the bytes,
	// with the length agreeing with the frame exactly.
	fn write(&mut self, hc: &mut Xhci, chan: u64, corr: u32, len: usize) {
		let result = (|| {
			let request = &self.buf[..len];
			if request.len() < 16 {
				return Err(Error::Invalid);
			}
			let attachment = u64::from_le_bytes(request[6..14].try_into().map_err(|_| Error::Invalid)?);
			let count = u16::from_le_bytes([request[14], request[15]]) as usize;
			if count == 0 || count > 4096 || request.len() != 16 + count {
				return Err(Error::Invalid);
			}
			self.holds(chan, attachment)?;
			// ONE WRITE OUTSTANDING, which the contract says and which the page enforces anyway.
			if self.pending.is_some() {
				return Err(Error::Invalid);
			}
			if !self.out.fill(&request[16..]) || !self.out.post(hc, count as u32) {
				return Err(Error::Io);
			}
			self.pending = Some((chan, corr));
			Ok(())
		})();
		if let Err(error) = result {
			classes::answer_u32(chan, corr, Err(error));
		}
	}

	// Abandon an unfinished write, answering it: nothing waits for it any more, but a consumer that is still
	// there is owed an answer to every request.
	fn abandon(&mut self, hc: &mut Xhci, hids: &mut Hids) {
		if let Some((chan, corr)) = self.pending.take() {
			let moved = classes::abandon(hc, hids, &mut self.out);
			classes::answer_u32(
				chan,
				corr,
				match moved {
					Some(moved) if moved > 0 => Ok(moved),
					_ => Err(Error::Io),
				},
			);
		}
	}
}

struct View<'a> {
	printer: &'a mut Printer,
	hc: &'a mut Xhci,
	hids: &'a mut Hids,
	chan: u64,
}

impl printer_backend::Service for View<'_> {
	fn attach(&mut self, version: u32) -> Result<PrinterAttach, Error> {
		if version != VERSION {
			return Err(Error::Unsupported);
		}
		// ONE CONSUMER: a second attach, from this connection or another, is refused while one holds it.
		if self.printer.consumer != 0 {
			return Err(Error::Invalid);
		}
		let device_id = self.printer.device_id(self.hc, self.hids)?;
		self.printer.attachment += 1;
		self.printer.consumer = self.chan;
		Ok(PrinterAttach { version: VERSION, attachment: self.printer.attachment, device_id })
	}

	fn port_status(&mut self, attachment: u64) -> Result<PortReading, Error> {
		self.printer.holds(self.chan, attachment)?;
		let bits = self.printer.port(self.hc, self.hids)?;
		// THE CLASS HAS NO COVER OR JAM BIT, so there is no evidence of either - which is `None`, never
		// `false`.
		Ok(PortReading { bits, cover_open: None, jam: None })
	}

	fn write(&mut self, _attachment: u64, _bytes: Vec<u8>) -> Result<u32, Error> {
		// Taken before dispatch, because it is answered when the printer finishes.
		Err(Error::Invalid)
	}

	fn reset(&mut self, attachment: u64) -> Result<PrinterAttach, Error> {
		self.printer.holds(self.chan, attachment)?;
		// THE ATTACHMENT IS OVER WHATEVER HAPPENS NEXT, and the unfinished write with it.
		self.printer.consumer = 0;
		self.printer.abandon(self.hc, self.hids);
		let interface = self.printer.binding.interface as u16;
		if control_nodata(self.hc, self.hids, &mut self.printer.dev, printer::RT_CLASS_INTERFACE_OUT, printer::REQ_SOFT_RESET, 0, interface).is_none() {
			return Err(Error::Io);
		}
		// AND THE PIPE, because SOFT_RESET resets the printer's end of it: a controller still holding the
		// old toggle would send the next packet as a retransmission the printer drops without a word.
		if !classes::reset_pipe(self.hc, self.hids, &mut self.printer.dev, &mut self.printer.out, 0) {
			return Err(Error::Io);
		}
		let device_id = self.printer.device_id(self.hc, self.hids)?;
		self.printer.attachment += 1;
		self.printer.consumer = self.chan;
		Ok(PrinterAttach { version: VERSION, attachment: self.printer.attachment, device_id })
	}
}

impl Module for Printer {
	fn kind(&self) -> ClassKind {
		ClassKind::Printer
	}

	fn inventory(&self) -> u8 {
		KIND_PRINTER
	}

	fn provider(&self) -> u16 {
		driver_protocol::provider::PRINTER
	}

	fn name(&self) -> &'static [u8] {
		driver_protocol::provider::USB_PRINTER_NAME
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
		let (len, mut handles) = match try_recv_caps(chan, &mut buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => {
				self.buf = buf;
				return true;
			}
			PolledCaps::Closed => {
				self.buf = buf;
				return false;
			}
		};
		self.buf = buf;
		match classes::correlation(&self.buf[..len]) {
			Some((printer_backend::OP_WRITE, corr)) => {
				// THIS CONTRACT TRANSFERS NOTHING on a request, so a handle that came with one is closed.
				for &handle in handles.as_slice() {
					close(handle);
				}
				self.write(hc, chan, corr, len);
			}
			_ => {
				let request = self.buf[..len].to_vec();
				let mut reply = [0u8; 4200];
				let mut reply_handles = wire::Handles::new();
				let mut view = View { printer: self, hc, hids, chan };
				if let Some(written) = printer_backend::dispatch(&mut view, &request, &mut handles, &mut reply, &mut reply_handles) {
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
		if self.pending.is_some_and(|(asked, _)| asked == chan) {
			self.pending = None;
			let _ = classes::abandon(hc, hids, &mut self.out);
		}
		if self.consumer == chan {
			self.consumer = 0;
		}
	}

	fn absorb(&mut self, hc: &mut Xhci, hids: &mut Hids, _pointer: u64, status: u32, control: u32) -> bool {
		if !self.out.owns(control) {
			return false;
		}
		let (code, moved) = self.out.complete(status);
		// EXACTLY WHAT MOVED, and `again` for nothing: the contract's answer is a prefix, so a printer that
		// took part of a write before stalling has taken that part.
		let answer = if classes::succeeded(code) {
			if moved == 0 { Err(Error::Again) } else { Ok(moved) }
		} else {
			if classes::stalled(code) {
				print(b"driver.xhci: the printer stalled a write - its pipe is cleared\n");
				let _ = classes::clear_halt(hc, hids, &mut self.dev, &mut self.out);
			}
			if moved > 0 { Ok(moved) } else { Err(Error::Io) }
		};
		if let Some((chan, corr)) = self.pending.take() {
			classes::answer_u32(chan, corr, answer);
		}
		true
	}

	fn release(&mut self, hc: &mut Xhci) {
		self.pending = None;
		self.out.release(hc);
	}
}
