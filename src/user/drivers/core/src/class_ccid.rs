// The USB CCID class side of driver.xhci: a smart-card reader on the bus, published as the `smartcard-reader`
// provider SmartcardService drives over `liber:smartcard`'s provider contract.
//
// THE PROVIDER OWNS THE TRANSPORT AND DECIDES NOTHING. Descriptor and slot parsing, the ATR, parameter
// negotiation, sequence numbers, message framing, abort and the slot-change pipe are here; who may talk to which
// card, and every PIN, are SmartcardService's. This transport does not drive a reader's keypad - it advertises no
// secure verification, and a request for one is a fault - so no PIN passes through it in any form.
//
// A REQUEST AND ITS ANSWER, BOUNDED. Each contract request is one reader message and the reply with its sequence
// number, waited for until the reader's deadline; a reader that asks for more time gets it, up to a bound, and one
// that goes silent past it has its transfer abandoned and the request answered as a fault. Replies naming another
// sequence - what an aborted command leaves behind - are read and dropped.
//
// A CARD IS A GENERATION. Every insertion the slot-change pipe reports is a new generation of that slot's card, and
// a request prepared against an older one is answered `card-removed` without touching the reader.

use alloc::vec::Vec;
use proto::system::{CardProtocol, Error, ExchangeLevel, PinpadCapabilities, ProviderOutcome, ProviderReply, ReaderDescription, ReaderEvent, ReaderEventKind, RequestHeader, SecureVerifyRequest, SlotReport, smartcard_reader};
use rt::*;

use crate::classes::{self, Module, Pipe};
use crate::usb_hid::Hids;
use crate::{KIND_SMARTCARD, UsbDevice, Xhci, control_nodata};
use drivers::ccid::{self, Binding, Exchange};
use drivers::common;
use drivers::usb_class::ClassKind;

const REQUEST_BYTES: usize = 512;
const STREAM_DEPTH: u64 = 16;
// A reply arrives within two seconds, and a reader that keeps asking for more time gets at most thirty.
const ANSWER_TICKS: u64 = 200;
const MOST_TICKS: u64 = 3000;

#[derive(Clone)]
struct Slot {
	present: bool,
	generation: u64,
	powered: bool,
}

pub struct Ccid {
	dev: UsbDevice,
	binding: Binding,
	out: Pipe,
	input: Pipe,
	notify: Option<Pipe>,
	slots: Vec<Slot>,
	generations: u64,
	seq: u8,
	consumer: u64,
	stream: u64,
	stream_seq: u32,
	buf: Vec<u8>,
}

/// Bring a bound reader's pipes up, select its setting and learn which slots hold a card.
///
/// # Safety
/// `dev` is an addressed device this controller owns.
pub unsafe fn configure(hc: &mut Xhci, mut dev: UsbDevice, binding: Binding) -> Result<Ccid, UsbDevice> {
	unsafe {
		let out = Pipe::new(dev.slot, dev.speed, &binding.bulk_out);
		let input = Pipe::new(dev.slot, dev.speed, &binding.bulk_in);
		let notify = binding.interrupt_in.and_then(|endpoint| Pipe::new(dev.slot, dev.speed, &endpoint));
		let (Some(mut out), Some(mut input)) = (out, input) else {
			print(b"driver.xhci: a smart-card reader is on the bus and no pages were left for its pipes\n");
			return Err(dev);
		};
		let mut pipes: Vec<&Pipe> = alloc::vec![&out, &input];
		if let Some(notify) = notify.as_ref() {
			pipes.push(notify);
		}
		if !classes::configure_pipes(hc, &mut dev, &pipes, 0) || !classes::select(hc, &mut dev, binding.config_value, binding.interface, binding.alternate) {
			out.release(hc);
			input.release(hc);
			if let Some(mut notify) = notify {
				notify.release(hc);
			}
			print(b"driver.xhci: a smart-card reader's pipes or its setting were refused\n");
			return Err(dev);
		}
		let slots = alloc::vec![Slot { present: false, generation: 0, powered: false }; binding.slots as usize];
		let mut reader = Ccid { dev, binding, out, input, notify, slots, generations: 0, seq: 0, consumer: 0, stream: 0, stream_seq: 0, buf: alloc::vec![0u8; REQUEST_BYTES] };
		let mut none = Hids::new();
		for slot in 0..reader.binding.slots {
			if let Ok(reply) = reader.transact(hc, &mut none, ccid::PC_TO_RDR_GET_SLOT_STATUS, slot, [0, 0, 0], &[]) {
				reader.presence(slot, reply.icc != 2);
			}
		}
		let mut line: common::Bounded<128> = common::Bounded::new();
		line.push(b"driver.xhci: smart-card reader bound - ");
		line.decimal(reader.binding.slots as u64);
		line.push(b" slot(s), ");
		line.push(match reader.binding.exchange {
			Exchange::ShortApdu => b"short-APDU".as_slice(),
			Exchange::ExtendedApdu => b"extended-APDU",
			Exchange::Tpdu => b"TPDU",
			Exchange::Character => b"character",
		});
		line.push(b" exchange, ");
		line.decimal(reader.slots.iter().filter(|slot| slot.present).count() as u64);
		line.push(b" card(s) in\n");
		print(line.as_bytes());
		Ok(reader)
	}
}

impl Ccid {
	fn next_seq(&mut self) -> u8 {
		self.seq = ccid::next_seq(self.seq);
		self.seq
	}

	// A slot's presence as the reader reports it: a card that arrives is a new generation.
	fn presence(&mut self, slot: u8, present: bool) -> bool {
		let Some(state) = self.slots.get_mut(slot as usize) else { return false };
		let changed = state.present != present;
		if present && !state.present {
			self.generations += 1;
			state.generation = self.generations;
		}
		if !present {
			state.powered = false;
		}
		state.present = present;
		changed
	}

	fn report(&self, slot: u8) -> SlotReport {
		let state = &self.slots[slot as usize];
		SlotReport { slot, present: state.present, card_generation: state.generation, atr: Vec::new(), powered: state.powered }
	}

	// One message and its reply: the message down the bulk OUT pipe, then replies until one names its sequence
	// and is not a request for more time - or until the deadline.
	fn transact(&mut self, hc: &mut Xhci, hids: &mut Hids, kind: u8, slot: u8, specific: [u8; 3], data: &[u8]) -> Result<ccid::Reply, ProviderOutcome> {
		let seq = self.next_seq();
		let message = ccid::encode(kind, slot, seq, specific, data);
		let started = clock();
		if !self.out.fill(&message) {
			return Err(ProviderOutcome::Fault);
		}
		match classes::transfer(hc, hids, &mut self.out, message.len() as u32, started + ANSWER_TICKS) {
			Some((code, moved)) if classes::succeeded(code) && moved as usize == message.len() => {}
			Some((code, _)) => {
				if classes::stalled(code) {
					let _ = classes::clear_halt(hc, hids, &mut self.dev, &mut self.out);
				}
				return Err(ProviderOutcome::Fault);
			}
			None => return Err(ProviderOutcome::Fault),
		}
		let mut deadline = started + ANSWER_TICKS;
		loop {
			let length = (self.binding.max_message.min(4096) as u32).max(self.input.mps);
			let Some((code, moved)) = classes::transfer(hc, hids, &mut self.input, length, deadline) else { return Err(ProviderOutcome::Fault) };
			if classes::stalled(code) {
				let _ = classes::clear_halt(hc, hids, &mut self.dev, &mut self.input);
				return Err(ProviderOutcome::Fault);
			}
			if !classes::succeeded(code) {
				return Err(ProviderOutcome::Fault);
			}
			let bytes = self.input.read(moved as usize);
			let Ok(reply) = ccid::decode(&bytes, self.binding.max_message) else { return Err(ProviderOutcome::Fault) };
			match reply.verdict(kind, seq, slot) {
				// ANOTHER SEQUENCE IS AN ANSWER TO SOMETHING ELSE - an aborted command's, arriving late.
				ccid::Verdict::Stray => continue,
				// MORE TIME, ASKED FOR: granted, up to the bound.
				ccid::Verdict::MoreTime => {
					deadline = (clock() + ANSWER_TICKS).min(started + MOST_TICKS);
					continue;
				}
				ccid::Verdict::CardAbsent => {
					self.presence(slot, false);
					return Err(ProviderOutcome::CardRemoved);
				}
				ccid::Verdict::WrongKind | ccid::Verdict::Failed => return Err(ProviderOutcome::Fault),
				ccid::Verdict::Answer => return Ok(reply),
			}
		}
	}

	// A request's header, held against the slot's card: a slot the reader does not have is an error, and an older
	// or absent card is `card-removed` without a word to the reader.
	fn current(&self, header: &RequestHeader) -> Result<Option<ProviderOutcome>, Error> {
		let slot = self.slots.get(header.slot as usize).ok_or(Error::Invalid)?;
		if !slot.present || slot.generation != header.card_generation {
			return Ok(Some(ProviderOutcome::CardRemoved));
		}
		Ok(None)
	}

	fn event(&mut self, slot: u8) {
		if self.stream == 0 {
			return;
		}
		let event = ReaderEvent { kind: ReaderEventKind::Presence, slot, report: Some(self.report(slot)) };
		let mut frame = [0u8; 256];
		let mut handles = wire::Handles::new();
		let Some(len) = smartcard_reader::events_frame(self.stream_seq, &event, &mut frame, &mut handles) else { return };
		if try_send(self.stream, &frame[..len], 0) {
			self.stream_seq += 1;
		}
	}

	fn post_notify(&mut self, hc: &mut Xhci) {
		if let Some(notify) = self.notify.as_mut()
			&& !notify.busy
		{
			let length = notify.mps.min(64);
			notify.post(hc, length);
		}
	}
}

fn reply(header: &RequestHeader, outcome: ProviderOutcome, response: Vec<u8>, atr: Vec<u8>) -> ProviderReply {
	ProviderReply { header: header.clone(), outcome, response, atr }
}

struct View<'a> {
	reader: &'a mut Ccid,
	hc: &'a mut Xhci,
	hids: &'a mut Hids,
}

impl smartcard_reader::Service for View<'_> {
	fn describe(&mut self) -> Result<ReaderDescription, Error> {
		let binding = &self.reader.binding;
		// NO KEYPAD IS DRIVEN, whatever the reader has: a PIN never passes through this transport.
		let pinpad = PinpadCapabilities { secure_verify: false, ascii: false, max_block: 0, min_digits: 0, max_digits: 0 };
		let exchange = match binding.exchange {
			Exchange::Character => ExchangeLevel::Character,
			Exchange::Tpdu => ExchangeLevel::Tpdu,
			Exchange::ShortApdu => ExchangeLevel::ShortApdu,
			Exchange::ExtendedApdu => ExchangeLevel::ExtendedApdu,
		};
		Ok(ReaderDescription { name: alloc::string::String::from("USB CCID reader"), slots: binding.slots, exchange, protocols: binding.protocols, pinpad })
	}

	// A NEW SESSION: every slot off, and each one's card as the reader says it is now.
	fn open_session(&mut self) -> Result<Vec<SlotReport>, Error> {
		let mut reports = Vec::new();
		for slot in 0..self.reader.binding.slots {
			let _ = self.reader.transact(self.hc, self.hids, ccid::PC_TO_RDR_ICC_POWER_OFF, slot, [0, 0, 0], &[]);
			self.reader.slots[slot as usize].powered = false;
			match self.reader.transact(self.hc, self.hids, ccid::PC_TO_RDR_GET_SLOT_STATUS, slot, [0, 0, 0], &[]) {
				Ok(status) => {
					self.reader.presence(slot, status.icc != 2);
				}
				Err(ProviderOutcome::CardRemoved) => {}
				Err(_) => return Err(Error::Io),
			}
			reports.push(self.reader.report(slot));
		}
		Ok(reports)
	}

	fn events(&mut self) -> Vec<ReaderEvent> {
		Vec::new()
	}

	fn power(&mut self, header: RequestHeader, on: bool) -> Result<ProviderReply, Error> {
		if let Some(outcome) = self.reader.current(&header)? {
			return Ok(reply(&header, outcome, Vec::new(), Vec::new()));
		}
		let kind = if on { ccid::PC_TO_RDR_ICC_POWER_ON } else { ccid::PC_TO_RDR_ICC_POWER_OFF };
		match self.reader.transact(self.hc, self.hids, kind, header.slot, [0, 0, 0], &[]) {
			Ok(answered) => {
				if on && (answered.data.is_empty() || answered.data.len() > ccid::MAX_ATR) {
					return Ok(reply(&header, ProviderOutcome::Fault, Vec::new(), Vec::new()));
				}
				self.reader.slots[header.slot as usize].powered = on;
				Ok(reply(&header, ProviderOutcome::Done, Vec::new(), if on { answered.data } else { Vec::new() }))
			}
			Err(outcome) => Ok(reply(&header, outcome, Vec::new(), Vec::new())),
		}
	}

	fn select_protocol(&mut self, header: RequestHeader, protocol: CardProtocol) -> Result<ProviderReply, Error> {
		if let Some(outcome) = self.reader.current(&header)? {
			return Ok(reply(&header, outcome, Vec::new(), Vec::new()));
		}
		let t1 = protocol == CardProtocol::T1;
		if self.reader.binding.protocols & if t1 { 0b10 } else { 0b01 } == 0 {
			return Err(Error::Unsupported);
		}
		let (specific, data) = ccid::default_parameters(t1);
		let outcome = match self.reader.transact(self.hc, self.hids, ccid::PC_TO_RDR_SET_PARAMETERS, header.slot, specific, &data) {
			Ok(_) => ProviderOutcome::Done,
			Err(outcome) => outcome,
		};
		Ok(reply(&header, outcome, Vec::new(), Vec::new()))
	}

	fn exchange(&mut self, header: RequestHeader, apdu: Vec<u8>) -> Result<ProviderReply, Error> {
		if apdu.len() < 4 || apdu.len() > ccid::MAX_APDU {
			return Err(Error::Invalid);
		}
		if let Some(outcome) = self.reader.current(&header)? {
			return Ok(reply(&header, outcome, Vec::new(), Vec::new()));
		}
		if !self.reader.slots[header.slot as usize].powered {
			return Ok(reply(&header, ProviderOutcome::Fault, Vec::new(), Vec::new()));
		}
		match self.reader.transact(self.hc, self.hids, ccid::PC_TO_RDR_XFR_BLOCK, header.slot, [0, 0, 0], &apdu) {
			// A RESPONSE IS AT LEAST ITS STATUS WORD and at most a short APDU's 256 bytes and SW.
			Ok(answered) if (2..=ccid::MAX_RESPONSE).contains(&answered.data.len()) && answered.chain == 0 => Ok(reply(&header, ProviderOutcome::Done, answered.data, Vec::new())),
			Ok(_) => Ok(reply(&header, ProviderOutcome::Fault, Vec::new(), Vec::new())),
			Err(outcome) => Ok(reply(&header, outcome, Vec::new(), Vec::new())),
		}
	}

	fn secure_verify(&mut self, request: SecureVerifyRequest) -> Result<ProviderReply, Error> {
		// NOT ADVERTISED, AND SO A FAULT: see `describe`.
		Ok(reply(&request.header, ProviderOutcome::Fault, Vec::new(), Vec::new()))
	}

	// THE CLASS'S ABORT: the control request names the slot and a sequence, and the bulk message with the same
	// sequence follows; the reader answers once the slot is quiet.
	fn abort(&mut self, header: RequestHeader) -> Result<ProviderReply, Error> {
		if header.slot >= self.reader.binding.slots {
			return Err(Error::Invalid);
		}
		// The sequence `transact` will take next: the control request and the bulk message must name the same one.
		let seq = ccid::next_seq(self.reader.seq);
		let interface = self.reader.binding.interface as u16;
		let _ = control_nodata(self.hc, self.hids, &mut self.reader.dev, ccid::RT_CLASS_INTERFACE_OUT, ccid::REQ_ABORT, (seq as u16) << 8 | header.slot as u16, interface);
		let outcome = match self.reader.transact(self.hc, self.hids, ccid::PC_TO_RDR_ABORT, header.slot, [0, 0, 0], &[]) {
			Ok(_) => ProviderOutcome::Done,
			Err(ProviderOutcome::CardRemoved) => ProviderOutcome::Done,
			Err(outcome) => outcome,
		};
		Ok(reply(&header, outcome, Vec::new(), Vec::new()))
	}
}

impl Module for Ccid {
	fn kind(&self) -> ClassKind {
		ClassKind::SmartCard
	}

	fn inventory(&self) -> u8 {
		KIND_SMARTCARD
	}

	fn provider(&self) -> u16 {
		driver_protocol::provider::SMARTCARD_READER
	}

	fn name(&self) -> &'static [u8] {
		driver_protocol::provider::USB_CCID_NAME
	}

	fn device(&self) -> &UsbDevice {
		&self.dev
	}

	fn device_mut(&mut self) -> &mut UsbDevice {
		&mut self.dev
	}

	// THE SLOT-CHANGE PIPE STANDS FROM THE PUBLICATION ON: a card pulled out while nobody was asking is still a
	// card that left.
	fn start(&mut self, hc: &mut Xhci) {
		self.post_notify(hc);
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
		if classes::correlation(&request).map(|(op, _)| op) == Some(smartcard_reader::OP_EVENTS) {
			let mut view = View { reader: self, hc, hids };
			let Some((corr, _)) = smartcard_reader::events_open(&mut view, &request, &mut handles) else { return true };
			let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
			if self.stream != 0 {
				close(self.stream);
			}
			self.stream = producer;
			self.stream_seq = 0;
			send_caps_blocking(chan, &corr.to_le_bytes(), &[consumer]);
			return true;
		}
		let mut reply = [0u8; 1024];
		let mut reply_handles = wire::Handles::new();
		let mut view = View { reader: self, hc, hids };
		if let Some(written) = smartcard_reader::dispatch(&mut view, &request, &mut handles, &mut reply, &mut reply_handles) {
			send_caps_blocking(chan, &reply[..written], reply_handles.as_slice());
		}
		for &handle in handles.as_slice() {
			close(handle);
		}
		true
	}

	fn departed(&mut self, _hc: &mut Xhci, _hids: &mut Hids, chan: u64) {
		if self.consumer == chan {
			self.consumer = 0;
			if self.stream != 0 {
				close(self.stream);
				self.stream = 0;
			}
		}
	}

	fn absorb(&mut self, hc: &mut Xhci, hids: &mut Hids, _pointer: u64, status: u32, control: u32) -> bool {
		let Some(notify) = self.notify.as_mut() else { return false };
		if !notify.owns(control) {
			return false;
		}
		let (code, moved) = notify.complete(status);
		if classes::succeeded(code) && moved > 0 {
			let bytes = notify.read(moved as usize);
			if let Some(changes) = ccid::slot_changes(&bytes, self.binding.slots) {
				for (slot, present, changed) in changes {
					// A CHANGE WITH THE CARD PRESENT MAY BE A CARD THAT LEFT AND CAME BACK between two notifications:
					// that is a new card, and a new generation.
					if changed && present && self.slots[slot as usize].present {
						self.presence(slot, false);
					}
					if self.presence(slot, present) || changed {
						self.event(slot);
					}
				}
			}
		} else if classes::stalled(code)
			&& let Some(notify) = self.notify.as_mut()
		{
			let _ = classes::clear_halt(hc, hids, &mut self.dev, notify);
		}
		self.post_notify(hc);
		true
	}

	fn release(&mut self, hc: &mut Xhci) {
		if self.stream != 0 {
			close(self.stream);
			self.stream = 0;
		}
		self.out.release(hc);
		self.input.release(hc);
		if let Some(notify) = self.notify.as_mut() {
			notify.release(hc);
		}
	}
}
