use super::*;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;

// ------------------------------------------------------------------ a BMC behind any interface

// THE BMC EVERY MODEL BELOW HANDS ITS REQUESTS TO: Get Device ID answered, everything else echoed back with its data.
fn bmc(request: &[u8]) -> Vec<u8> {
	let (netfn_lun, cmd) = (request[0], request[1]);
	let mut out = vec![netfn_lun | 0x04, cmd, cc::OK];
	if netfn_lun >> 2 == netfn::APP && cmd == identity::GET_DEVICE_ID {
		out.extend_from_slice(&[0x20, 0x81, 0x02, 0x05, 0x02, 0xBF, 0x57, 0x01, 0x00, 0x34, 0x12]);
	} else {
		out.extend_from_slice(&request[2..]);
	}
	out
}

fn device_id_request() -> Request {
	Request::new(netfn::APP, identity::GET_DEVICE_ID, &[])
}

// ------------------------------------------------------------------ KCS

// A KCS INTERFACE AS QEMU MODELS IT (`hw/ipmi/ipmi_kcs.c`): the same states, the same OBF and IBF rules, the same abort -
// with the BMC answering at once, or after `delay` status reads, or never; and the hostile shapes the specification
// allows a real interface.
struct Kcs {
	status: u8,
	data_out: u8,
	data_in: Option<u8>,
	cmd: Option<u8>,
	inmsg: Vec<u8>,
	write_end: bool,
	outmsg: Vec<u8>,
	outpos: usize,
	pending: Option<Vec<u8>>,
	delay: u32,
	clock: u64,
	answer: Box<dyn FnMut(&[u8]) -> Option<Vec<u8>>>,
	// Hostile: WRITE_START puts the interface in its read state.
	ignore_start: bool,
	// Hostile: the Nth data byte of a request puts the interface in its error state, with this status code.
	error_on: Option<(usize, u8)>,
	// Hostile: IBF never clears after the Nth write.
	freeze_after: Option<u32>,
	writes: u32,
	requests: u32,
}

const IDLE: u8 = 0;
const READ_STATE: u8 = 1;
const WRITE_STATE: u8 = 2;
const ERROR_STATE: u8 = 3;

impl Kcs {
	fn new(answer: impl FnMut(&[u8]) -> Option<Vec<u8>> + 'static) -> Kcs {
		Kcs { status: 0, data_out: 0, data_in: None, cmd: None, inmsg: Vec::new(), write_end: false, outmsg: Vec::new(), outpos: 0, pending: None, delay: 0, clock: 0, answer: Box::new(answer), ignore_start: false, error_on: None, freeze_after: None, writes: 0, requests: 0 }
	}

	fn state(&self) -> u8 {
		self.status >> 6
	}

	fn set_state(&mut self, state: u8) {
		self.status = (self.status & 0x3F) | (state << 6);
	}

	fn event(&mut self) {
		if self.cmd == Some(kcs::GET_STATUS_ABORT) {
			if self.state() != ERROR_STATE {
				self.pending = None;
				self.outmsg = vec![0x01];
				self.outpos = 0;
				self.set_state(ERROR_STATE);
				self.status |= kcs::OBF;
			}
			self.finish();
			return;
		}
		match self.state() {
			IDLE => {
				if self.cmd == Some(kcs::WRITE_START) && self.ignore_start {
					self.cmd = None;
					self.set_state(READ_STATE);
					self.status |= kcs::OBF;
				} else if self.cmd == Some(kcs::WRITE_START) {
					self.set_state(WRITE_STATE);
					self.cmd = None;
					self.write_end = false;
					self.inmsg.clear();
					self.status |= kcs::OBF;
				}
			}
			READ_STATE => {
				if self.outpos >= self.outmsg.len() {
					self.set_state(IDLE);
					self.status |= kcs::OBF;
				} else if self.data_in == Some(kcs::READ) {
					self.data_out = self.outmsg[self.outpos];
					self.outpos += 1;
					self.status |= kcs::OBF;
				} else {
					self.outmsg = vec![0x02];
					self.outpos = 0;
					self.set_state(ERROR_STATE);
					self.status |= kcs::OBF;
					self.finish();
					return;
				}
			}
			WRITE_STATE => {
				if let Some(byte) = self.data_in {
					self.inmsg.push(byte);
					if let Some((at, code)) = self.error_on
						&& self.inmsg.len() == at
					{
						self.outmsg = vec![code];
						self.outpos = 0;
						self.set_state(ERROR_STATE);
						self.status |= kcs::OBF;
						self.finish();
						return;
					}
				}
				if self.write_end {
					self.write_end = false;
					self.requests += 1;
					let request = self.inmsg.clone();
					self.pending = (self.answer)(&request);
					// IBF STAYS SET while the BMC works.
					self.data_in = None;
					self.cmd = None;
					return;
				} else if self.cmd == Some(kcs::WRITE_END) {
					self.cmd = None;
					self.write_end = true;
				}
				self.status |= kcs::OBF;
			}
			_ => {
				if self.data_in.is_some() {
					self.set_state(READ_STATE);
					self.data_in = Some(kcs::READ);
					self.outpos = 0;
					if self.outpos < self.outmsg.len() {
						self.data_out = self.outmsg[self.outpos];
						self.outpos += 1;
						self.status |= kcs::OBF;
					}
				}
			}
		}
		if self.cmd.is_some() {
			self.outmsg = vec![0x02];
			self.outpos = 0;
			self.set_state(ERROR_STATE);
		}
		self.finish();
	}

	fn finish(&mut self) {
		self.cmd = None;
		self.data_in = None;
		self.status &= !kcs::IBF;
	}

	// The BMC's answer arriving: the read state, the first byte.
	fn deliver(&mut self) {
		if let Some(answer) = self.pending.take() {
			self.outmsg = answer;
			self.outpos = 0;
			self.set_state(READ_STATE);
			self.data_in = Some(kcs::READ);
			self.event();
		}
	}
}

impl Registers for Kcs {
	fn read(&mut self, index: u8) -> u8 {
		self.clock += 1;
		if index == kcs::DATA {
			self.status &= !kcs::OBF;
			return self.data_out;
		}
		if self.pending.is_some() {
			if self.delay == 0 {
				self.deliver();
			} else {
				self.delay -= 1;
			}
		}
		self.status
	}

	fn write(&mut self, index: u8, value: u8) {
		if self.status & kcs::IBF != 0 {
			return;
		}
		self.writes += 1;
		if index == kcs::DATA {
			self.data_in = Some(value);
		} else {
			self.cmd = Some(value);
		}
		self.status |= kcs::IBF;
		if self.freeze_after.is_some_and(|after| self.writes >= after) {
			return;
		}
		self.event();
	}

	fn now_ms(&mut self) -> u64 {
		self.clock
	}

	fn pause(&mut self) {
		self.clock += 10;
	}
}

#[test]
fn a_kcs_transaction_writes_the_request_and_reads_the_bmc_s_answer() {
	let mut model = Kcs::new(|request| Some(bmc(request)));
	model.delay = 200;
	let deadline = model.now_ms() + TRANSACTION_MS;
	let answered = kcs::transact(&mut model, &device_id_request(), deadline).expect("Get Device ID is answered");
	assert!(answered.ok());
	let found = identity::device_id(&answered.data).expect("eleven bytes");
	assert_eq!((found.device_id, found.manufacturer, found.product, found.ipmi), (0x20, 0x157, 0x1234, (2, 0)));
	assert!(kcs::idle(&mut model), "the interface is left idle for the next");
	// AND A SECOND ONE AFTER IT.
	let deadline = model.now_ms() + TRANSACTION_MS;
	let echoed = kcs::transact(&mut model, &Request::new(netfn::STORAGE, 0x11, &[1, 2, 3]), deadline).expect("answered");
	assert_eq!(echoed.data, vec![1, 2, 3]);
	assert_eq!(model.requests, 2);
}

#[test]
fn a_kcs_answer_past_the_bound_is_refused_and_the_interface_aborted_to_idle() {
	// 273 BYTES: inside QEMU's 300-byte buffer, past this layer's bound.
	let mut model = Kcs::new(|request| {
		let mut out = bmc(request);
		out.resize(MAX_RESPONSE + 1, 0xAB);
		Some(out)
	});
	assert_eq!(kcs::transact(&mut model, &device_id_request(), TRANSACTION_MS), Err(Failure::TooLong));
	assert!(kcs::idle(&mut model), "the abort left it idle");
	// EXACTLY THE BOUND is taken.
	let mut model = Kcs::new(|request| {
		let mut out = bmc(request);
		out.resize(MAX_RESPONSE, 0xAB);
		Some(out)
	});
	assert_eq!(kcs::transact(&mut model, &device_id_request(), TRANSACTION_MS).map(|found| found.data.len()), Ok(MAX_RESPONSE - 3));
}

#[test]
fn a_kcs_interface_that_never_answers_fails_at_the_deadline_and_is_aborted() {
	let mut model = Kcs::new(|_| None);
	let started = model.now_ms();
	assert_eq!(kcs::transact(&mut model, &device_id_request(), started + TRANSACTION_MS), Err(Failure::Deadline));
	assert!(model.now_ms() >= started + TRANSACTION_MS, "not before its deadline");
	assert!(model.now_ms() <= started + TRANSACTION_MS + RECOVERY_MS + 100, "and the abort within its own bound after it");
	// IBF STAYS SET while a BMC works, so an abort cannot reach an interface whose BMC never answers - what QEMU's own
	// timeout (0xC3 after four seconds, inside this deadline) exists to end.
	assert!(!kcs::idle(&mut model));
}

#[test]
fn a_kcs_interface_stuck_in_each_state_is_aborted_and_the_failure_named() {
	// THE READ STATE AFTER WRITE_START: not the write state.
	let mut model = Kcs::new(|request| Some(bmc(request)));
	model.ignore_start = true;
	assert!(matches!(kcs::transact(&mut model, &device_id_request(), TRANSACTION_MS), Err(Failure::Protocol(_))));
	// IBF NEVER CLEARING, mid-write: the deadline, and an abort that cannot reach it either.
	let mut model = Kcs::new(|request| Some(bmc(request)));
	model.freeze_after = Some(2);
	assert_eq!(kcs::transact(&mut model, &device_id_request(), 1_000), Err(Failure::Deadline));
	assert_eq!(kcs::abort(&mut model), Err(Failure::Deadline), "an interface whose IBF never clears cannot be aborted, and says so in its bound");
}

#[test]
fn the_kcs_error_state_is_reported_with_the_code_the_abort_read() {
	// AN INTERFACE ALREADY IN ITS ERROR STATE: the abort reads that error's own code.
	let mut model = Kcs::new(|request| Some(bmc(request)));
	model.set_state(ERROR_STATE);
	model.outmsg = vec![0x06];
	assert_eq!(kcs::abort(&mut model), Ok(0x06));
	assert!(kcs::idle(&mut model));
	// ONE ENTERING IT MID-WRITE: the transaction fails with the code, and the interface is idle after.
	let mut model = Kcs::new(|request| Some(bmc(request)));
	model.error_on = Some((2, 0x06));
	assert_eq!(kcs::transact(&mut model, &Request::new(netfn::APP, 0x01, &[1, 2, 3]), TRANSACTION_MS), Err(Failure::Interface(0x06)));
	assert!(kcs::idle(&mut model));
	// A plain abort of an idle interface reads ABORTED.
	assert_eq!(kcs::abort(&mut model), Ok(0x01));
}

#[test]
fn a_kcs_answer_to_another_request_is_refused() {
	let mut model = Kcs::new(|request| {
		let mut out = bmc(request);
		out[1] ^= 0x10;
		Some(out)
	});
	assert_eq!(kcs::transact(&mut model, &device_id_request(), TRANSACTION_MS), Err(Failure::Mismatch));
}

// ------------------------------------------------------------------ BT

// A BT INTERFACE AS QEMU MODELS IT (`hw/ipmi/ipmi_bt.c`): the control register's toggles, the buffer's two pointers,
// Get BT Interface Capabilities answered by the interface itself - and hostile shapes.
struct Bt {
	control: u8,
	inmsg: Vec<u8>,
	outmsg: Vec<u8>,
	outpos: usize,
	clock: u64,
	answer: Box<dyn FnMut(&[u8]) -> Option<Vec<u8>>>,
	// Hostile: the sequence number answered.
	sequence: Option<u8>,
	// Hostile: a length byte of its own.
	length: Option<u8>,
	output_buffer: u8,
}

impl Bt {
	fn new(answer: impl FnMut(&[u8]) -> Option<Vec<u8>> + 'static) -> Bt {
		Bt { control: 0, inmsg: Vec::new(), outmsg: Vec::new(), outpos: 0, clock: 0, answer: Box::new(answer), sequence: None, length: None, output_buffer: 0xFF }
	}

	fn event(&mut self) {
		if self.inmsg.len() < 4 || self.inmsg[0] as usize != self.inmsg.len() - 1 {
			return;
		}
		let (netfn_lun, sequence, cmd) = (self.inmsg[1], self.inmsg[2], self.inmsg[3]);
		let answer = if netfn_lun == netfn::APP << 2 && cmd == bt::GET_BT_CAPABILITIES {
			Some(vec![netfn_lun | 4, cmd, 0, 1, 0xFF, self.output_buffer, 10, 0])
		} else {
			let mut request = vec![netfn_lun, cmd];
			request.extend_from_slice(&self.inmsg[4..]);
			(self.answer)(&request)
		};
		let Some(answer) = answer else { return };
		let mut out = vec![self.length.unwrap_or((answer.len() + 1) as u8), answer[0], self.sequence.unwrap_or(sequence)];
		out.extend_from_slice(&answer[1..]);
		self.outmsg = out;
		self.outpos = 0;
		self.control &= !bt::B_BUSY;
		self.control |= bt::B2H_ATN;
	}
}

impl Registers for Bt {
	fn read(&mut self, index: u8) -> u8 {
		self.clock += 1;
		match index {
			bt::CONTROL => self.control,
			bt::BUFFER => {
				let byte = self.outmsg.get(self.outpos).copied().unwrap_or(0xFF);
				self.outpos += 1;
				byte
			}
			_ => 0,
		}
	}

	fn write(&mut self, index: u8, value: u8) {
		match index {
			bt::CONTROL => {
				if value & bt::CLR_WR_PTR != 0 {
					self.inmsg.clear();
				}
				if value & bt::CLR_RD_PTR != 0 {
					self.outpos = 0;
				}
				if value & bt::B2H_ATN != 0 {
					self.control &= !bt::B2H_ATN;
				}
				if value & bt::H_BUSY != 0 {
					self.control ^= bt::H_BUSY;
				}
				if value & bt::H2B_ATN != 0 {
					self.control |= bt::B_BUSY;
					self.event();
				}
			}
			bt::BUFFER => self.inmsg.push(value),
			_ => {}
		}
	}

	fn now_ms(&mut self) -> u64 {
		self.clock
	}

	fn pause(&mut self) {
		self.clock += 10;
	}
}

#[test]
fn a_bt_transaction_reads_the_capabilities_first_and_then_carries_requests() {
	let mut model = Bt::new(|request| Some(bmc(request)));
	let mut interface = bt::Bt::default();
	let capabilities = interface.read_capabilities(&mut model, TRANSACTION_MS).expect("the interface answers its capabilities");
	assert_eq!((capabilities.outstanding, capabilities.input, capabilities.output), (1, 0xFF, 0xFF));
	let deadline = model.now_ms() + TRANSACTION_MS;
	let answered = interface.transact(&mut model, &device_id_request(), deadline).expect("answered");
	assert_eq!(identity::device_id(&answered.data).map(|found| found.product), Some(0x1234));
	assert_eq!(model.control & (bt::H_BUSY | bt::B2H_ATN), 0, "H_BUSY toggled back and the attention taken");
	let deadline = model.now_ms() + TRANSACTION_MS;
	let echoed = interface.transact(&mut model, &Request::new(netfn::STORAGE, 0x40, &[9]), deadline).expect("answered");
	assert_eq!(echoed.data, vec![9]);
}

#[test]
fn a_bt_answer_with_a_sequence_it_was_never_sent_is_refused() {
	let mut model = Bt::new(|request| Some(bmc(request)));
	model.sequence = Some(0x77);
	let mut interface = bt::Bt::default();
	assert_eq!(interface.transact(&mut model, &device_id_request(), TRANSACTION_MS), Err(Failure::Mismatch));
}

#[test]
fn a_bt_length_byte_past_the_reported_buffer_is_refused() {
	let mut model = Bt::new(|request| Some(bmc(request)));
	model.output_buffer = 0x40;
	let mut interface = bt::Bt::default();
	interface.read_capabilities(&mut model, TRANSACTION_MS).expect("capabilities");
	model.length = Some(0x50);
	let deadline = model.now_ms() + TRANSACTION_MS;
	assert_eq!(interface.transact(&mut model, &device_id_request(), deadline), Err(Failure::TooLong));
	// A buffer too small to hold a header is not a BMC that can be spoken to.
	assert_eq!(bt::capabilities(&[1, 4, 5, 10, 0]), None);
}

#[test]
fn a_bt_bmc_that_never_raises_its_attention_fails_at_the_deadline() {
	let mut model = Bt::new(|_| None);
	let mut interface = bt::Bt::default();
	assert_eq!(interface.transact(&mut model, &device_id_request(), 2_000), Err(Failure::Deadline));
	assert!(model.now_ms() >= 2_000);
}

// ------------------------------------------------------------------ SSIF

// AN SSIF BMC AS QEMU MODELS IT (`hw/ipmi/smbus_ipmi.c`): single and multi-part writes, the response NACKed until it is
// ready and then read in 32-byte blocks - with hostile shapes.
struct Ssif {
	inmsg: Vec<u8>,
	outmsg: Vec<u8>,
	block: u32,
	nacks: u32,
	clock: u64,
	answer: Box<dyn FnMut(&[u8]) -> Vec<u8>>,
	writes: Vec<(u8, usize)>,
	// Hostile: the block number answered for the second middle block.
	skip: bool,
	// Hostile: a block longer than 32 bytes.
	oversize: bool,
	// Hostile: NACK for ever.
	silent: bool,
}

impl Ssif {
	fn new(answer: impl FnMut(&[u8]) -> Vec<u8> + 'static) -> Ssif {
		Ssif { inmsg: Vec::new(), outmsg: Vec::new(), block: 0, nacks: 0, clock: 0, answer: Box::new(answer), writes: Vec::new(), skip: false, oversize: false, silent: false }
	}

	fn load(&self, block: u32) -> Option<Vec<u8>> {
		let length = self.outmsg.len();
		if length == 0 {
			return None;
		}
		if length <= 32 {
			return (block == 0).then(|| self.outmsg.clone());
		}
		if block == 0 {
			let mut out = vec![0x00, 0x01];
			out.extend_from_slice(&self.outmsg[..30]);
			return Some(out);
		}
		let at = 30 + (block as usize - 1) * 31;
		if at >= length {
			return None;
		}
		let rest = length - at;
		let mut out = Vec::new();
		if rest > 31 {
			out.push(if self.skip && block == 2 { 5 } else { (block - 1) as u8 });
			out.extend_from_slice(&self.outmsg[at..at + 31]);
		} else {
			out.push(0xFF);
			out.extend_from_slice(&self.outmsg[at..]);
		}
		Some(out)
	}
}

impl ssif::Smbus for Ssif {
	fn block_write(&mut self, command: u8, data: &[u8]) -> Result<(), ssif::BusError> {
		self.writes.push((command, data.len()));
		match command {
			ssif::WRITE_SINGLE | ssif::WRITE_START => self.inmsg = data.to_vec(),
			ssif::WRITE_MIDDLE | ssif::WRITE_END => self.inmsg.extend_from_slice(data),
			_ => return Err(ssif::BusError::Failed),
		}
		if command == ssif::WRITE_SINGLE || command == ssif::WRITE_END {
			let request = self.inmsg.clone();
			self.outmsg = if request[0] == netfn::APP << 2 && request[1] == ssif::GET_SYSTEM_INTERFACE_CAPABILITIES { vec![(netfn::APP + 1) << 2, request[1], 0, 0, 2 << 6, 0xFF, 0xFF] } else { (self.answer)(&request) };
			self.nacks = 3;
		}
		Ok(())
	}

	fn block_read(&mut self, command: u8) -> Result<Vec<u8>, ssif::BusError> {
		if self.silent || self.nacks > 0 {
			self.nacks = self.nacks.saturating_sub(1);
			return Err(ssif::BusError::Nack);
		}
		self.block = match command {
			ssif::READ_SINGLE => 0,
			ssif::READ_MIDDLE => self.block + 1,
			_ => return Err(ssif::BusError::Failed),
		};
		let mut block = self.load(self.block).ok_or(ssif::BusError::Nack)?;
		if self.oversize {
			block.resize(33, 0);
		}
		Ok(block)
	}

	fn now_ms(&mut self) -> u64 {
		self.clock += 1;
		self.clock
	}

	fn pause(&mut self) {
		self.clock += 10;
	}
}

#[test]
fn an_ssif_request_and_response_each_take_as_many_parts_as_they_need() {
	let mut model = Ssif::new(|request| {
		let mut out = vec![request[0] | 4, request[1], 0];
		out.extend_from_slice(&request[2..]);
		out
	});
	let mut interface = ssif::Ssif::default();
	let capabilities = interface.read_capabilities(&mut model, TRANSACTION_MS).expect("capabilities");
	assert_eq!((capabilities.parts, capabilities.input, capabilities.output), (ssif::Parts::Middle, 0xFF, 0xFF));
	// ONE PART EACH WAY.
	let short = interface.transact(&mut model, &Request::new(netfn::STORAGE, 0x11, &[1, 2, 3]), TRANSACTION_MS).expect("answered");
	assert_eq!(short.data, vec![1, 2, 3]);
	// A HUNDRED BYTES: a start, two middles and an end going in; a start, two middles and an end coming out.
	model.writes.clear();
	let data: Vec<u8> = (0..100).collect();
	let long = interface.transact(&mut model, &Request::new(netfn::STORAGE, 0x11, &data), TRANSACTION_MS).expect("answered");
	assert_eq!(long.data, data);
	assert_eq!(model.writes, vec![(ssif::WRITE_START, 32), (ssif::WRITE_MIDDLE, 32), (ssif::WRITE_MIDDLE, 32), (ssif::WRITE_END, 6)]);
}

#[test]
fn an_ssif_response_out_of_order_past_32_or_nacked_past_the_bound_is_refused() {
	let long = |request: &[u8]| {
		let mut out = vec![request[0] | 4, request[1], 0];
		out.extend_from_slice(&[7; 120]);
		out
	};
	let interface = ssif::Ssif { capabilities: ssif::Capabilities { parts: ssif::Parts::Middle, pec: false, version: 0, input: 0xFF, output: 0xFF } };
	let mut model = Ssif::new(long);
	model.skip = true;
	assert!(matches!(interface.transact(&mut model, &device_id_request(), TRANSACTION_MS), Err(Failure::Protocol(_))), "a block out of order");
	let mut model = Ssif::new(long);
	model.oversize = true;
	assert!(matches!(interface.transact(&mut model, &device_id_request(), TRANSACTION_MS), Err(Failure::Protocol(_))), "a count past 32");
	let mut model = Ssif::new(long);
	model.silent = true;
	assert!(matches!(interface.transact(&mut model, &device_id_request(), u64::MAX), Err(Failure::Protocol(_))), "NACKed past the retry bound");
	let mut model = Ssif::new(long);
	model.silent = true;
	assert_eq!(interface.transact(&mut model, &device_id_request(), 200), Err(Failure::Deadline), "or past the deadline first");
	// A BMC THAT TAKES ONE PART is never sent two.
	let single = ssif::Ssif::default();
	assert_eq!(single.transact(&mut Ssif::new(long), &Request::new(netfn::APP, 1, &[0; 40]), TRANSACTION_MS), Err(Failure::RequestTooLong));
}

// ------------------------------------------------------------------ the message layer

#[test]
fn three_silent_transactions_mark_the_bmc_unavailable_and_an_answer_brings_it_back() {
	let mut availability = Availability::default();
	let timeout = Ok(Response { cc: cc::TIMEOUT, data: Vec::new() });
	assert_eq!(availability.observe(&timeout), Change::None);
	assert_eq!(availability.observe(&Err(Failure::Deadline)), Change::None);
	assert_eq!(availability.observe(&Ok(Response { cc: cc::BMC_INIT, data: Vec::new() })), Change::Lost);
	assert!(!availability.available());
	assert_eq!(availability.observe(&timeout), Change::None, "still gone, and said once");
	assert_eq!(availability.observe(&Ok(Response { cc: cc::INVALID_COMMAND, data: Vec::new() })), Change::Back, "any answer is a BMC");
	assert!(availability.available());
	// A REFUSAL IS NOT SILENCE, and resets the count.
	availability.observe(&timeout);
	availability.observe(&timeout);
	availability.observe(&Err(Failure::Mismatch));
	assert_eq!(availability.observe(&timeout), Change::None);
}

#[test]
fn a_pet_goes_to_the_head_of_the_queue_and_is_never_refused() {
	let mut queue = queue::Queue::new(2);
	let watchdog = |entry: &&str| entry.starts_with("pet");
	assert!(queue.push("read sdr 1", queue::Priority::Ordinary, watchdog));
	assert!(queue.push("read sdr 2", queue::Priority::Ordinary, watchdog));
	assert!(!queue.push("read sel", queue::Priority::Ordinary, watchdog), "an ordinary entry is refused at the bound");
	assert!(queue.push("pet 1", queue::Priority::Watchdog, watchdog));
	assert!(queue.push("pet 2", queue::Priority::Watchdog, watchdog), "a watchdog entry past the bound is still taken");
	let order: Vec<&str> = core::iter::from_fn(|| queue.pop()).collect();
	assert_eq!(order, ["pet 1", "pet 2", "read sdr 1", "read sdr 2"]);
}

#[test]
fn a_response_is_held_against_its_request() {
	let request = Request::new(netfn::APP, 0x01, &[]);
	assert_eq!(response(&request, &[0x1C, 0x01, 0x00, 5]), Ok(Response { cc: 0, data: vec![5] }));
	assert_eq!(response(&request, &[0x1C, 0x02, 0x00]), Err(Failure::Mismatch));
	assert_eq!(response(&request, &[0x28, 0x01, 0x00]), Err(Failure::Mismatch));
	assert!(matches!(response(&request, &[0x1C, 0x01]), Err(Failure::Protocol(_))));
	assert_eq!(response(&request, &vec![0x1C; MAX_RESPONSE + 1]), Err(Failure::TooLong));
}

// ------------------------------------------------------------------ identity

#[test]
fn a_bmc_is_named_by_its_guid_or_by_its_device_and_its_binding() {
	let device = identity::device_id(&[0x20, 0x81, 0x02, 0x05, 0x02, 0xBF, 0x57, 0x01, 0x00, 0x34, 0x12]).expect("decoded");
	assert!(device.provides_sdrs && device.available);
	assert_eq!(identity::guid(&[0; 16]), None, "sixteen zeros is no GUID");
	let guid: [u8; 16] = core::array::from_fn(|at| at as u8);
	let named = identity::Identity::of(&device, identity::guid(&guid), "pci:0000:00:05.0");
	assert_eq!(named.name(), "bmc:000102030405060708090a0b0c0d0e0f");
	assert_eq!(named.selector().map(|bytes| bytes.len()), Some(36));
	let fallback = identity::Identity::of(&device, None, "pci:0000:00:05.0");
	assert_eq!(fallback.name(), "bmc:00157-1234-20@pci:0000:00:05.0");
	assert!(fallback.selector().is_some());
	// PAST THE SELECTOR'S 64 BYTES a fallback is no administrable name, rather than a truncated one.
	let long = identity::Identity::of(&device, None, "acpi:\\_SB_.PCI0.SF8_.SMB0.MI01.XXXX.YYYY.ZZZZ.WWWW");
	assert!(long.name().len() > identity::SELECTOR_MAX);
	assert_eq!(long.selector(), None);
	// ONLY A GUID PAIRS.
	assert!(named.same_bmc(&identity::Identity::of(&device, identity::guid(&guid), "acpi:\\_SB_.MI00")));
	assert!(!fallback.same_bmc(&fallback.clone()));
}

// ------------------------------------------------------------------ SDR

// A FULL SENSOR RECORD at the specification's offsets: a temperature, y = x degrees C, with its upper thresholds
// readable, named "CPU Temp".
fn full_record(id: u16, number: u8, sensor_type: u8, name: &str) -> Vec<u8> {
	let mut body = vec![0u8; 43];
	body[0] = sdr::BMC_OWNER;
	body[2] = number;
	body[3] = 0x03; // entity: processor
	body[7] = sensor_type;
	body[8] = sdr::THRESHOLD;
	body[13] = 0b0011_1000; // UNR, UC and UNC readable
	body[15] = 0x00; // units 1: unsigned
	body[16] = 0x01; // degrees C
	body[19] = 1; // M = 1
	body[31] = 95; // UNR
	body[32] = 85; // UC
	body[33] = 70; // UNC
	body[42] = 0xC0 | name.len() as u8;
	body.extend_from_slice(name.as_bytes());
	let [i0, i1] = id.to_le_bytes();
	let mut out = vec![i0, i1, 0x51, sdr::FULL, body.len() as u8];
	out.extend_from_slice(&body);
	out
}

#[test]
fn a_full_record_is_read_at_the_specification_s_offsets() {
	let bytes = full_record(7, 0x30, sdr::TEMPERATURE, "CPU Temp");
	let sdr::Record::Sensor(sensor) = sdr::record(&bytes).expect("decoded") else { panic!("a sensor") };
	assert_eq!((sensor.record_id, sensor.number, sensor.name.as_str(), sensor.kind), (7, 0x30, "CPU Temp", sdr::Kind::Full));
	assert!(sensor.temperature());
	assert_eq!((sensor.thresholds.unr(), sensor.thresholds.uc(), sensor.thresholds.unc(), sensor.thresholds.lc()), (Some(95), Some(85), Some(70), None));
	let reading = power_model::ipmi::reading(&[40, 0xC0, 0xC0]).expect("a reading");
	let zone = power_model::ipmi::thermal(&sensor.temperature_sensor(Some(reading)).expect("a full record converts"));
	assert_eq!(zone.temperature.value, 40_000);
	assert_eq!(sensor.value(&reading), power_model::convert::Tagged::Known(40_000));
}

#[test]
fn a_record_whose_length_or_name_lies_is_refused_and_the_repository_still_read() {
	let mut short = full_record(1, 1, sdr::TEMPERATURE, "A");
	short[4] += 1;
	assert_eq!(sdr::record(&short), Err(sdr::Refusal::Length));
	let mut long_name = full_record(2, 2, sdr::TEMPERATURE, "AB");
	long_name[5 + 42] = 0xC0 | 20;
	assert_eq!(sdr::record(&long_name), Err(sdr::Refusal::Name));
	assert_eq!(sdr::record(&[1, 0, 0x51, sdr::FULL, 3, 0, 0, 0]), Err(sdr::Refusal::Short));
	let mut repository = sdr::Repository::default();
	assert!(repository.add(&short));
	assert!(repository.add(&full_record(3, 3, sdr::TEMPERATURE, "C")));
	assert!(repository.add(&[4, 0, 0x51, 0xC0, 0]), "an OEM record is kept as its type");
	assert_eq!((repository.refused, repository.sensors().count(), repository.records.len()), (1, 1, 2));
}

#[test]
fn the_repository_is_bounded_and_says_it_was_truncated() {
	let mut repository = sdr::Repository::default();
	let mut added = 0;
	for id in 0..=sdr::MAX_SENSORS as u16 {
		if !repository.add(&full_record(id, id as u8, 0x02, "V")) {
			break;
		}
		added += 1;
	}
	assert_eq!(added, sdr::MAX_SENSORS);
	assert!(repository.truncated);
}

#[test]
fn compact_event_only_and_locator_records_decode() {
	let mut compact = vec![5, 0, 0x51, sdr::COMPACT, 0];
	let mut body = vec![0u8; 27];
	body[0] = sdr::BMC_OWNER;
	body[2] = 0x40;
	body[7] = 0x08; // power supply
	body[8] = 0x6F;
	body[26] = 0xC3;
	body.extend_from_slice(b"PS1");
	compact[4] = body.len() as u8;
	compact.extend_from_slice(&body);
	let sdr::Record::Sensor(found) = sdr::record(&compact).expect("compact") else { panic!("a sensor") };
	assert_eq!((found.kind, found.sensor_type, found.name.as_str(), found.linear), (sdr::Kind::Compact, 0x08, "PS1", None));
	let mut fru = vec![6, 0, 0x51, sdr::FRU_LOCATOR, 0];
	let mut body = vec![0u8; 11];
	body[0] = 0x20;
	body[1] = 3; // FRU device 3
	body[2] = 0x80; // logical
	body[10] = 0xC4;
	body.extend_from_slice(b"Base");
	fru[4] = body.len() as u8;
	fru.extend_from_slice(&body);
	assert_eq!(sdr::record(&fru), Ok(sdr::Record::Fru(sdr::FruLocator { record_id: 6, device: 3, logical: true, entity: (0, 0), name: String::from("Base") })));
}

#[test]
fn a_record_is_read_in_partial_reads_header_first() {
	let bytes = full_record(9, 1, sdr::TEMPERATURE, "Inlet");
	let mut read = sdr::RecordRead::new(9);
	let mut asked = Vec::new();
	loop {
		match read.step() {
			sdr::Step::Ask { offset, count } => {
				asked.push((offset, count));
				assert!(read.answered(10, &bytes[offset as usize..offset as usize + count as usize]));
			}
			sdr::Step::Done { bytes: whole, next } => {
				assert_eq!((whole, next), (bytes.clone(), 10));
				break;
			}
		}
	}
	assert_eq!(asked[0], (0, 5), "the header first");
	assert!(asked.iter().all(|(_, count)| *count <= sdr::CHUNK));
	assert!(!sdr::RecordRead::new(1).answered(2, &[0; 6]), "more than was asked for is refused");
}

// ------------------------------------------------------------------ FRU

fn area(mut bytes: Vec<u8>) -> Vec<u8> {
	bytes.resize(bytes.len().div_ceil(8) * 8 - 1, 0);
	let sum = bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
	bytes.push(0u8.wrapping_sub(sum));
	bytes[1] = (bytes.len() / 8) as u8;
	let sum = bytes[..bytes.len() - 1].iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
	*bytes.last_mut().unwrap() = 0u8.wrapping_sub(sum);
	bytes
}

fn inventory_bytes(board_checksum_broken: bool) -> Vec<u8> {
	let chassis = area(vec![1, 0, 0x17, 0xC4, b'P', b'A', b'R', b'T', 0xC2, b'S', b'N', fru::END]);
	// A BOARD with a BCD-plus serial and a six-bit ASCII part number ("ABCD" in three bytes).
	let mut board = area(vec![1, 0, 25, 0x10, 0x20, 0x30, 0xC5, b'L', b'i', b'b', b'e', b'r', 0xC4, b'B', b'o', b'r', b'd', 0x42, 0x12, 0x34, 0x83, 0xA1, 0x38, 0x92, 0xC0, 0xC0, fru::END]);
	if board_checksum_broken {
		board[3] ^= 1;
	}
	let product = area(vec![1, 0, 25, 0xC3, b'A', b'C', b'M', 0xC4, b'N', b'o', b'd', b'e', fru::END]);
	let chassis_at = 1u8;
	let board_at = chassis_at + (chassis.len() / 8) as u8;
	let product_at = board_at + (board.len() / 8) as u8;
	let mut header = vec![1, 0, chassis_at, board_at, product_at, 0, 0];
	let sum = header.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
	header.push(0u8.wrapping_sub(sum));
	[header, chassis, board, product].concat()
}

#[test]
fn a_fru_inventory_decodes_every_field_kind_with_every_checksum_checked() {
	let found = fru::inventory(&inventory_bytes(false)).expect("an inventory");
	let chassis = found.chassis.expect("a chassis area");
	assert_eq!((chassis.chassis_type, chassis.part.map(|field| field.text()), chassis.serial.map(|field| field.text())), (0x17, Some(String::from("PART")), Some(String::from("SN"))));
	let board = found.board.expect("a board area");
	assert_eq!(board.manufactured, 0x302010);
	assert_eq!(board.manufacturer.map(|field| field.text()).as_deref(), Some("Liber"));
	assert_eq!(board.serial.map(|field| field.text()).as_deref(), Some("1234"), "BCD plus");
	assert_eq!(board.part.map(|field| field.text()).as_deref(), Some("ABCD"), "six-bit ASCII");
	assert_eq!(found.product.expect("a product area").name.map(|field| field.text()).as_deref(), Some("Node"));
	assert!(found.refused.is_empty());
}

#[test]
fn a_fru_area_that_lies_is_refused_and_the_others_still_read() {
	let found = fru::inventory(&inventory_bytes(true)).expect("the header holds");
	assert!(found.board.is_none());
	assert_eq!(found.refused, vec![fru::Refusal::Area("an area's checksum")]);
	assert!(found.chassis.is_some() && found.product.is_some());
	let mut broken = inventory_bytes(false);
	broken[7] ^= 1;
	assert_eq!(fru::inventory(&broken), Err(fru::Refusal::Header("the common header's checksum")));
	// A FIELD RUNNING PAST ITS AREA.
	let overlong = area(vec![1, 0, 0x17, 0xC9, b'P', fru::END]);
	let mut header = vec![1, 0, 1, 0, 0, 0, 0];
	let sum = header.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
	header.push(0u8.wrapping_sub(sum));
	let found = fru::inventory(&[header, overlong].concat()).expect("the header holds");
	assert_eq!(found.refused, vec![fru::Refusal::Field("a field's length runs past its area")]);
	assert_eq!(fru::read_response(&[3, 1, 2, 3]), Some(&[1u8, 2, 3][..]));
	assert_eq!(fru::read_response(&[4, 1, 2, 3]), None, "a count that is not the bytes");
}

// ------------------------------------------------------------------ SEL, events, the watchdog, the chassis, LAN

#[test]
fn the_sel_is_counted_read_and_cleared_under_a_reservation() {
	let info = sel::info(&[0x51, 3, 0, 0x80, 0x0C, 1, 0, 0, 0, 2, 0, 0, 0, 0x02]).expect("fourteen bytes");
	assert_eq!((info.entries, info.reserve, info.overflow), (3, true, false));
	assert_eq!(sel::clear_request(0x1234, true).data, vec![0x34, 0x12, b'C', b'L', b'R', 0xAA]);
	assert_eq!(sel::clear_complete(&[1]), Some(true));
	let boot = event::boot_completed();
	let mut record = [0u8; 16];
	record[2] = 0x02;
	record[7] = event::GENERATOR;
	record[9] = boot.data[1];
	record[10] = boot.data[2];
	record[11] = boot.data[3];
	record[12] = boot.data[4];
	record[13..16].copy_from_slice(&boot.data[5..8]);
	let decoded = sel::record(&record).expect("a system event");
	assert_eq!(event::ours(&decoded), Some(event::Ours::Boot));
	record[2] = 0x10;
	assert_eq!(sel::record(&record), Err(sel::Refused { id: 0, record_type: 0x10 }), "a type the specification leaves undefined");
	assert_eq!(event::graceful_shutdown().data, vec![0x41, 0x04, 0x20, 0x00, 0x6F, 0x03, 0xFF, 0xFF]);
	assert_eq!(sel::entry_response(&[2, 0]), None, "a record shorter than sixteen bytes");
}

#[test]
fn the_watchdog_is_armed_to_reset_disarmed_by_its_don_t_stop_bit_and_its_expiry_read() {
	let arm = watchdog::arm(30_050, watchdog::EXPIRED_SMS_OS).expect("above the minimum");
	assert_eq!(arm.data, vec![0x44, 0x01, 0x00, 0x10, 0x2C, 0x01], "SMS/OS, don't stop, hard reset, 300 steps of 100 ms, rounded down");
	assert_eq!(watchdog::arm(14_999, 0), None, "below the minimum is refused, never raised");
	assert_eq!(watchdog::effective_ms(15_000), Some(15_000));
	assert_eq!(watchdog::arm(u64::MAX, 0).map(|found| found.data[4..].to_vec()), Some(vec![0xFF, 0xFF]), "above the ceiling: 6553.5 s");
	assert_eq!(watchdog::disarm(0).data[0] & watchdog::DONT_STOP, 0);
	let state = watchdog::state(&[0x44, 0x01, 0, 0x10, 0x2C, 0x01, 0x20, 0x01]).expect("eight bytes");
	assert!(state.expired() && state.running);
	assert_eq!((state.initial_ms, state.present_ms), (30_000, 28_800));
	// QEMU's simulator after an expiry: "started" as the last Set left it, the present countdown zero - stopped.
	let expired = watchdog::state(&[0x44, 0x01, 0, 0x10, 0x96, 0x00, 0x00, 0x00]).expect("eight bytes");
	assert!(expired.expired() && !expired.running);
	assert_eq!(expired.initial_ms, 15_000);
}

#[test]
fn chassis_control_takes_the_four_operations_and_refuses_power_up_and_the_interrupt() {
	assert_eq!(chassis::Control::from_parameter(3), Some(chassis::Control::HardReset));
	assert_eq!(chassis::Control::from_parameter(1), None);
	assert_eq!(chassis::Control::from_parameter(4), None);
	assert!(chassis::Control::PowerCycle.hard() && !chassis::Control::SoftShutdown.hard());
	let status = chassis::status(&[0x21, 0x00, 0x50]).expect("three bytes");
	assert_eq!((status.power_on, status.restore_policy, status.identify), (true, 1, Some(1)));
	assert_eq!(chassis::status(&[0x01, 0, 0]).and_then(|found| found.identify), None, "identify not reported");
}

#[test]
fn lan_configuration_and_users_decode_read_only() {
	assert_eq!(lan::vlan(&[0x11, 0x0A, 0x80]), Some(Some(10)));
	assert_eq!(lan::vlan(&[0x11, 0x0A, 0x00]), Some(None));
	assert_eq!(lan::medium(&[1, 0x04, 1]), Some(lan::MEDIUM_LAN));
	let access = lan::access(&[0x0A, 0x02, 0x01, 0x14]).expect("four bytes");
	assert_eq!((access.max_users, access.enabled_users, access.privilege, access.messaging), (10, 2, 4, true));
	let mut name = [0u8; 16];
	name[..5].copy_from_slice(b"admin");
	assert_eq!(lan::user_name(&name).as_deref(), Some("admin"));
}
