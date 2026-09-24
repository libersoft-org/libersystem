// USB-MIDI 1.0 EVENT PACKETS, DECODED INTO BOUNDED CHUNKS (USB Device Class Definition for MIDI Devices 1.0,
// section 4): cable and Code Index Number checked, status and data validated, SysEx tracked by counting and
// never assembled.
//
// WHAT THIS IS FOR. MidiService decodes every batch a MIDI provider delivers with this code, and the USB MIDI
// class module will hand it the packets its transfers complete. The in-guest fixture delivers raw packets and
// nothing else, so what a client sees is what this decoder made of them.
//
// ONE PACKET, ONE CABLE. Every packet names its cable, so a malformed one resets that cable's partial SysEx
// only; a batch that is not a whole number of packets, or a cable the endpoint does not have, cannot be
// attributed, and resets every cable. A reserved code index, a status that does not match its code index, a
// data byte with the top bit set and non-zero padding are typed faults - never another message.
//
// SYSEX IS COUNTED. Per cable: whether a message is open, its number, how many bytes it has had with its
// delimiters, and when it last moved. 64 kB crosses the cap; two seconds without a fragment is inactivity; a
// non-realtime status inside it interrupts it; a new start restarts it. Each of those ABORTS it by number,
// and what follows is discarded until an end or a fresh start. Realtime bytes pass through in their place
// and never touch a SysEx.

use alloc::vec::Vec;

pub const PACKET: usize = 4;
pub const MAX_PACKETS: usize = 64;
pub const MAX_CABLES: u8 = 16;
/// A SysEx's whole size, delimiters included.
pub const SYSEX_CAP: u32 = 64 * 1024;
/// Two seconds at the system's 100 Hz.
pub const SYSEX_IDLE_TICKS: u64 = 200;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	Short,
	SysexStart,
	SysexContinue,
	SysexEnd,
	Raw,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chunk {
	pub cable: u8,
	pub kind: Kind,
	pub bytes: [u8; 3],
	pub len: u8,
	pub message: Option<u32>,
	pub start: bool,
	pub end: bool,
}

impl Chunk {
	pub fn bytes(&self) -> &[u8] {
		&self.bytes[..usize::from(self.len)]
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AbortReason {
	Cap,
	Malformed,
	Interrupted,
	Inactivity,
	Restarted,
	Reset,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FaultCode {
	Alignment,
	Cable,
	Reserved,
	Status,
	Padding,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Output {
	Chunk(Chunk),
	Abort {
		cable: u8,
		message: u32,
		reason: AbortReason,
	},
	/// `cable` is `None` when the fault could not be attributed and every cable was reset.
	Fault {
		cable: Option<u8>,
		code: FaultCode,
	},
}

#[derive(Clone, Copy, Default)]
struct Cable {
	// The open message: its number, bytes so far, and the tick it last moved.
	open: Option<(u32, u32, u64)>,
	// After an abort, the rest of that message is dropped until an end or a new start.
	discarding: bool,
	next_message: u32,
}

pub struct Decoder {
	cables: u8,
	states: [Cable; MAX_CABLES as usize],
}

fn data(byte: u8) -> bool {
	byte < 0x80
}

fn realtime(byte: u8) -> bool {
	byte >= 0xf8
}

impl Decoder {
	/// A decoder for an endpoint with `cables` cables (1..=16).
	pub fn new(cables: u8) -> Self {
		Self { cables: cables.clamp(1, MAX_CABLES), states: [Cable::default(); MAX_CABLES as usize] }
	}

	fn abort(&mut self, cable: u8, reason: AbortReason, out: &mut Vec<Output>) {
		let state = &mut self.states[usize::from(cable)];
		if let Some((message, _, _)) = state.open.take() {
			out.push(Output::Abort { cable, message, reason });
			state.discarding = true;
		}
	}

	/// Every cable's partial state ends: what could not be attributed resets them all.
	pub fn reset(&mut self, reason: AbortReason, out: &mut Vec<Output>) {
		for cable in 0..self.cables {
			self.abort(cable, reason, out);
		}
	}

	pub fn next_deadline(&self) -> Option<u64> {
		self.states.iter().filter_map(|state| state.open.map(|(_, _, last)| last + SYSEX_IDLE_TICKS)).min()
	}

	/// A SysEx with nothing for two seconds is aborted.
	pub fn tick(&mut self, now: u64, out: &mut Vec<Output>) {
		for cable in 0..self.cables {
			if self.states[usize::from(cable)].open.is_some_and(|(_, _, last)| now >= last + SYSEX_IDLE_TICKS) {
				self.abort(cable, AbortReason::Inactivity, out);
			}
		}
	}

	// One SysEx fragment: `bytes` with its delimiters, starting and/or ending the message.
	fn sysex(&mut self, cable: u8, bytes: &[u8], start: bool, end: bool, now: u64, out: &mut Vec<Output>) {
		if start {
			self.abort(cable, AbortReason::Restarted, out);
			let state = &mut self.states[usize::from(cable)];
			let message = state.next_message;
			state.next_message = state.next_message.wrapping_add(1);
			state.discarding = false;
			state.open = Some((message, 0, now));
		}
		let state = &mut self.states[usize::from(cable)];
		let Some((message, count, _)) = state.open else {
			// A continuation or an end with no message open: the rest of an aborted one is dropped quietly;
			// anything else is a fault.
			if state.discarding {
				if end {
					state.discarding = false;
				}
				return;
			}
			out.push(Output::Fault { cable: Some(cable), code: FaultCode::Status });
			return;
		};
		let total = match count.checked_add(bytes.len() as u32) {
			Some(total) if total <= SYSEX_CAP => total,
			_ => {
				self.abort(cable, AbortReason::Cap, out);
				if end {
					self.states[usize::from(cable)].discarding = false;
				}
				return;
			}
		};
		let mut chunk = Chunk {
			cable,
			kind: if end {
				Kind::SysexEnd
			} else if start {
				Kind::SysexStart
			} else {
				Kind::SysexContinue
			},
			bytes: [0; 3],
			len: bytes.len() as u8,
			message: Some(message),
			start,
			end,
		};
		chunk.bytes[..bytes.len()].copy_from_slice(bytes);
		out.push(Output::Chunk(chunk));
		let state = &mut self.states[usize::from(cable)];
		state.open = if end { None } else { Some((message, total, now)) };
	}

	fn short(&mut self, cable: u8, bytes: &[u8], out: &mut Vec<Output>) {
		// A NON-REALTIME STATUS INSIDE A SYSEX INTERRUPTS IT; a realtime byte never does.
		if !realtime(bytes[0]) {
			self.abort(cable, AbortReason::Interrupted, out);
		}
		let mut chunk = Chunk { cable, kind: Kind::Short, bytes: [0; 3], len: bytes.len() as u8, message: None, start: false, end: false };
		chunk.bytes[..bytes.len()].copy_from_slice(bytes);
		out.push(Output::Chunk(chunk));
	}

	// A fault on a known cable resets that cable alone.
	fn fault(&mut self, cable: u8, code: FaultCode, out: &mut Vec<Output>) {
		self.abort(cable, AbortReason::Malformed, out);
		out.push(Output::Fault { cable: Some(cable), code });
	}

	// The single-byte form: a byte on its own, which may be anything.
	fn single(&mut self, cable: u8, byte: u8, now: u64, out: &mut Vec<Output>) {
		let open = self.states[usize::from(cable)].open.is_some();
		let discarding = self.states[usize::from(cable)].discarding;
		match byte {
			0xf0 => self.sysex(cable, &[byte], true, false, now, out),
			0xf7 if open || discarding => self.sysex(cable, &[byte], false, true, now, out),
			// A stray end: nothing was open, and nothing is ended by it.
			0xf7 => out.push(Output::Fault { cable: Some(cable), code: FaultCode::Status }),
			_ if realtime(byte) => self.short(cable, &[byte], out),
			_ if data(byte) && (open || discarding) => self.sysex(cable, &[byte], false, false, now, out),
			_ => {
				if !data(byte) {
					self.abort(cable, AbortReason::Interrupted, out);
				}
				out.push(Output::Chunk(Chunk { cable, kind: Kind::Raw, bytes: [byte, 0, 0], len: 1, message: None, start: false, end: false }));
			}
		}
	}

	/// Decode one batch, received at tick `now`.
	pub fn decode(&mut self, batch: &[u8], now: u64, out: &mut Vec<Output>) {
		// NOT A WHOLE NUMBER OF PACKETS: nothing in it can be attributed, so nothing of it is emitted.
		if batch.len() % PACKET != 0 || batch.len() > PACKET * MAX_PACKETS {
			self.reset(AbortReason::Reset, out);
			out.push(Output::Fault { cable: None, code: FaultCode::Alignment });
			return;
		}
		for packet in batch.chunks_exact(PACKET) {
			let cable = packet[0] >> 4;
			let cin = packet[0] & 0x0f;
			let (b1, b2, b3) = (packet[1], packet[2], packet[3]);
			if cable >= self.cables {
				self.reset(AbortReason::Reset, out);
				out.push(Output::Fault { cable: None, code: FaultCode::Cable });
				continue;
			}
			let padded = |used: usize| packet[1 + used..].iter().all(|byte| *byte == 0);
			match cin {
				0x0 | 0x1 => self.fault(cable, FaultCode::Reserved, out),
				0x2 => match (b1, data(b2), padded(2)) {
					(0xf1 | 0xf3, true, true) => self.short(cable, &[b1, b2], out),
					(_, _, false) => self.fault(cable, FaultCode::Padding, out),
					_ => self.fault(cable, FaultCode::Status, out),
				},
				0x3 => match (b1, data(b2) && data(b3)) {
					(0xf2, true) => self.short(cable, &[b1, b2, b3], out),
					_ => self.fault(cable, FaultCode::Status, out),
				},
				0x4 => {
					if b1 == 0xf0 && data(b2) && data(b3) {
						self.sysex(cable, &[b1, b2, b3], true, false, now, out);
					} else if data(b1) && data(b2) && data(b3) {
						self.sysex(cable, &[b1, b2, b3], false, false, now, out);
					} else {
						self.fault(cable, FaultCode::Status, out);
					}
				}
				0x5 => match (b1, padded(1)) {
					(_, false) => self.fault(cable, FaultCode::Padding, out),
					(0xf7, true) => self.single_end(cable, now, out),
					(0xf6, true) => self.short(cable, &[b1], out),
					_ => self.fault(cable, FaultCode::Status, out),
				},
				0x6 => match (b1, b2, padded(2)) {
					(_, _, false) => self.fault(cable, FaultCode::Padding, out),
					(0xf0, 0xf7, true) => self.sysex(cable, &[b1, b2], true, true, now, out),
					(first, 0xf7, true) if data(first) => self.sysex(cable, &[b1, b2], false, true, now, out),
					_ => self.fault(cable, FaultCode::Status, out),
				},
				0x7 => match (b1, b2, b3) {
					(0xf0, second, 0xf7) if data(second) => self.sysex(cable, &[b1, b2, b3], true, true, now, out),
					(first, second, 0xf7) if data(first) && data(second) => self.sysex(cable, &[b1, b2, b3], false, true, now, out),
					_ => self.fault(cable, FaultCode::Status, out),
				},
				0x8..=0xe => {
					let length = if matches!(cin, 0xc | 0xd) { 2 } else { 3 };
					if b1 >> 4 != cin || !packet[2..1 + length].iter().all(|byte| data(*byte)) {
						self.fault(cable, FaultCode::Status, out);
					} else if !padded(length) {
						self.fault(cable, FaultCode::Padding, out);
					} else {
						self.short(cable, &packet[1..1 + length], out);
					}
				}
				_ => {
					if !padded(1) {
						self.fault(cable, FaultCode::Padding, out);
					} else {
						self.single(cable, b1, now, out);
					}
				}
			}
		}
	}

	// CIN 5 with an end: a one-byte end of the open message, or of nothing.
	fn single_end(&mut self, cable: u8, now: u64, out: &mut Vec<Output>) {
		let state = self.states[usize::from(cable)];
		if state.open.is_some() || state.discarding {
			self.sysex(cable, &[0xf7], false, true, now, out);
		} else {
			out.push(Output::Fault { cable: Some(cable), code: FaultCode::Status });
		}
	}
}

#[cfg(test)]
mod tests;
