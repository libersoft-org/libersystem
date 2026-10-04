// UNIVERSAL MIDI PACKETS (M2-104-UM, Universal MIDI Packet and MIDI 2.0 Protocol, 1.1) AND THEIR USB SIDE (USB Device
// Class Definition for MIDI Devices 2.0), AS PURE DECISIONS: how long a message is from its first word, which group it
// belongs to, a word stream cut into messages, SysEx7 and SysEx8 kept within the bounds MIDI 1.0's SysEx is kept in,
// the translation between USB-MIDI 1.0 event packets and MIDI 1.0 in UMP both ways - and MIDI 2.0 channel voice down
// to MIDI 1.0 by the specification's default translation - and the Group Terminal Block descriptors a device lists
// its groups in.
//
// A MESSAGE IS ONE TO FOUR WORDS, AND ITS FIRST WORD SAYS HOW MANY. The message type is the word's top four bits, and
// the size follows from it alone - reserved types included, which is what lets a receiver skip a message it does not
// know without losing its place. A stream that ends inside a message is a fault, never a short message.
//
// A GROUP IS NEVER FABRICATED. Utility (type 0) and UMP stream (type 15) messages belong to no group; every other
// type carries one in bits 27-24. A MIDI 1.0 cable is carried AS a group by the translation, which is the
// specification's mapping, and nothing else invents one.

use alloc::vec::Vec;

/// The most words one message has.
pub const MAX_WORDS: usize = 4;
/// The message types this layer names; the rest are reserved, and sized by the specification all the same.
pub const MT_UTILITY: u8 = 0x0;
pub const MT_SYSTEM: u8 = 0x1;
pub const MT_MIDI1_VOICE: u8 = 0x2;
pub const MT_DATA64: u8 = 0x3;
pub const MT_MIDI2_VOICE: u8 = 0x4;
pub const MT_DATA128: u8 = 0x5;
pub const MT_FLEX: u8 = 0xD;
pub const MT_STREAM: u8 = 0xF;
/// A SysEx's bytes, counted as MIDI 1.0's are - so a UMP sender cannot do what a MIDI 1.0 one may not.
pub const SYSEX_CAP: u32 = 64 * 1024;

/// How many words a message of type `mt` is.
pub fn words_of(mt: u8) -> usize {
	match mt & 0xF {
		0x0..=0x2 | 0x6 | 0x7 => 1,
		0x3 | 0x4 | 0x8..=0xA => 2,
		0xB | 0xC => 3,
		_ => 4,
	}
}

/// One message: its words, as many as its type says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ump {
	words: [u32; MAX_WORDS],
	len: u8,
}

impl Ump {
	/// The message whose words these are - `None` when they are not as many as the first one's type says.
	pub fn new(words: &[u32]) -> Option<Ump> {
		let first = *words.first()?;
		if words.len() != words_of((first >> 28) as u8) {
			return None;
		}
		let mut held = [0u32; MAX_WORDS];
		held[..words.len()].copy_from_slice(words);
		Some(Ump { words: held, len: words.len() as u8 })
	}

	pub fn words(&self) -> &[u32] {
		&self.words[..self.len as usize]
	}

	pub fn message_type(&self) -> u8 {
		(self.words[0] >> 28) as u8
	}

	/// Its group, for every type but utility and stream messages, which have none.
	pub fn group(&self) -> Option<u8> {
		match self.message_type() {
			MT_UTILITY | MT_STREAM => None,
			_ => Some((self.words[0] >> 24 & 0xF) as u8),
		}
	}

	/// The same message on another group - for a type that has one.
	pub fn on_group(mut self, group: u8) -> Ump {
		if self.group().is_some() {
			self.words[0] = self.words[0] & !(0xF << 24) | u32::from(group & 0xF) << 24;
		}
		self
	}

	/// Its words as the wire carries them: each little-endian, as USB carries a UMP.
	pub fn bytes(&self) -> Vec<u8> {
		self.words().iter().flat_map(|word| word.to_le_bytes()).collect()
	}
}

/// Why a stream of words is not messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
	/// Bytes that are not a whole number of words.
	Alignment,
	/// A message the stream ends inside of.
	Truncated,
}

/// A batch's bytes as words, each little-endian.
pub fn words_of_bytes(bytes: &[u8]) -> Result<Vec<u32>, Fault> {
	if bytes.len() % 4 != 0 {
		return Err(Fault::Alignment);
	}
	Ok(bytes.chunks_exact(4).map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]])).collect())
}

/// A batch of words cut into messages. Everything before a message the batch ends inside of is kept; that message is
/// the fault.
pub fn split(words: &[u32]) -> (Vec<Ump>, Option<Fault>) {
	let mut out = Vec::new();
	let mut at = 0;
	while at < words.len() {
		let length = words_of((words[at] >> 28) as u8);
		let Some(message) = words.get(at..at + length).and_then(Ump::new) else {
			return (out, Some(Fault::Truncated));
		};
		out.push(message);
		at += length;
	}
	(out, None)
}

// ------------------------------------------------------------------ SysEx7 and SysEx8, counted

/// Where a data message stands in its SysEx: the status in bits 23-20.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
	Complete,
	Start,
	Continue,
	End,
}

fn part_of(word: u32) -> Option<Part> {
	match word >> 20 & 0xF {
		0 => Some(Part::Complete),
		1 => Some(Part::Start),
		2 => Some(Part::Continue),
		3 => Some(Part::End),
		_ => None,
	}
}

/// Why a SysEx was abandoned, in the words MIDI 1.0's decoder uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Abort {
	/// It crossed `SYSEX_CAP`.
	Cap,
	/// A continuation or an end that does not fit the message, or a byte count past what the type carries.
	Malformed,
	/// A new start before its end.
	Restarted,
}

/// What the tracker made of one message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tracked {
	/// Deliver it; for a SysEx part, the message number it belongs to.
	Message(Ump, Option<u32>),
	/// Deliver nothing of it, and discard what was delivered of message `number` on `group`.
	Aborted { group: u8, number: u32, reason: Abort },
	/// A part with no message to belong to: dropped.
	Stray(Ump),
}

#[derive(Clone, Copy, Default)]
struct Open {
	number: u32,
	bytes: u32,
}

/// SYSEX7 AND SYSEX8 PER GROUP, COUNTED: whether one is open, its number, and the bytes it has had. A start opens a
/// message (restarting one that was open), a continuation or an end needs one open, and the bytes are counted against
/// `SYSEX_CAP`; a part past the cap, or a byte count past what the type carries, aborts the message by number, and what
/// follows of it is a stray until a fresh start. Nothing is assembled. SysEx7 and SysEx8 are tracked apart - a group may
/// carry one of each at once.
#[derive(Clone)]
pub struct Tracker {
	open: [[Option<Open>; 16]; 2],
	next: [u32; 16],
}

impl Default for Tracker {
	fn default() -> Tracker {
		Tracker { open: [[None; 16]; 2], next: [0; 16] }
	}
}

impl Tracker {
	pub fn new() -> Tracker {
		Tracker::default()
	}

	/// One message, in stream order.
	pub fn take(&mut self, message: Ump) -> Vec<Tracked> {
		let (kind, most) = match message.message_type() {
			MT_DATA64 => (0usize, 6u32),
			// SysEx8's statuses 8 and 9 are mixed data sets, which are not SysEx and pass as they are.
			MT_DATA128 if message.words[0] >> 20 & 0xF <= 3 => (1, 14),
			_ => return alloc::vec![Tracked::Message(message, None)],
		};
		let group = message.group().unwrap_or(0);
		let slot = group as usize;
		let count = message.words[0] >> 16 & 0xF;
		let Some(part) = part_of(message.words[0]) else { return alloc::vec![Tracked::Stray(message)] };
		let mut out = Vec::new();
		// A START OR A WHOLE MESSAGE ends whatever was open on the group - as a restart.
		if matches!(part, Part::Start | Part::Complete)
			&& let Some(open) = self.open[kind][slot].take()
		{
			out.push(Tracked::Aborted { group, number: open.number, reason: Abort::Restarted });
		}
		let open = match part {
			Part::Start | Part::Complete => {
				let number = self.next[slot];
				self.next[slot] = number.wrapping_add(1);
				Open { number, bytes: 0 }
			}
			Part::Continue | Part::End => match self.open[kind][slot].take() {
				Some(open) => open,
				None => {
					out.push(Tracked::Stray(message));
					return out;
				}
			},
		};
		// MORE BYTES THAN THE TYPE CARRIES: the message it continued is over, and one it would have opened never is.
		if count > most {
			out.push(if matches!(part, Part::Continue | Part::End) { Tracked::Aborted { group, number: open.number, reason: Abort::Malformed } } else { Tracked::Stray(message) });
			return out;
		}
		let bytes = open.bytes + count;
		if bytes > SYSEX_CAP {
			out.push(Tracked::Aborted { group, number: open.number, reason: Abort::Cap });
			return out;
		}
		if matches!(part, Part::Start | Part::Continue) {
			self.open[kind][slot] = Some(Open { number: open.number, bytes });
		}
		out.push(Tracked::Message(message, Some(open.number)));
		out
	}
}

// ------------------------------------------------------------------ MIDI 1.0 event packets <-> MIDI 1.0 in UMP

/// A USB-MIDI 1.0 event packet: the cable and code index, then three bytes.
pub type Packet = [u8; 4];

/// ONE USB-MIDI 1.0 EVENT PACKET AS MIDI 1.0 IN UMP, its cable as the group: a channel voice message as type 2, a
/// system common or real-time one as type 1, and a SysEx packet as a SysEx7 message carrying its bytes without the
/// F0 and F7 - a start where the packet holds the F0, an end where its code index ends the message, both where it does
/// both. `None` for a code index that carries no message (miscellaneous, cable events and the reserved ones).
pub fn ump_of_packet(packet: Packet) -> Option<Ump> {
	let group = u32::from(packet[0] >> 4);
	let cin = packet[0] & 0xF;
	let word = |mt: u32, length: usize| -> Ump {
		let mut bytes = [0u8; 3];
		bytes[..length].copy_from_slice(&packet[1..1 + length]);
		let w = mt << 28 | group << 24 | u32::from(bytes[0]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[2]);
		Ump::new(&[w]).expect("one word")
	};
	match cin {
		0x8..=0xE => Some(word(2, if cin == 0xC || cin == 0xD { 2 } else { 3 })),
		// System common: two bytes, three bytes, and the single-byte F6 - or a lone byte of any kind.
		0x2 => Some(word(1, 2)),
		0x3 => Some(word(1, 3)),
		0x5 if packet[1] == 0xF6 => Some(word(1, 1)),
		0xF if packet[1] >= 0xF8 || packet[1] == 0xF6 => Some(word(1, 1)),
		// SysEx: start or continuation with three bytes, and the three ends.
		0x4..=0x7 => {
			let length = match cin {
				0x4 | 0x7 => 3,
				0x5 => 1,
				_ => 2,
			};
			let body = &packet[1..1 + length];
			let start = body.first() == Some(&0xF0);
			let end = cin != 0x4;
			let data: Vec<u8> = body.iter().copied().filter(|&byte| byte != 0xF0 && byte != 0xF7).collect();
			let status: u32 = match (start, end) {
				(true, true) => 0,
				(true, false) => 1,
				(false, false) => 2,
				(false, true) => 3,
			};
			let mut bytes = [0u8; 6];
			bytes[..data.len()].copy_from_slice(&data);
			let first = 3 << 28 | group << 24 | status << 20 | (data.len() as u32) << 16 | u32::from(bytes[0]) << 8 | u32::from(bytes[1]);
			let second = u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
			Ump::new(&[first, second])
		}
		_ => None,
	}
}

/// MIDI 1.0 IN UMP BACK TO USB-MIDI 1.0 EVENT PACKETS, its group as the cable - a stateful job for SysEx only, whose
/// packets carry three bytes until the last and must be cut at threes whatever the UMP messages carried. Per group:
/// the SysEx bytes not yet in a packet. MIDI 2.0 channel voice is first translated down (`midi1_of_midi2`); utility,
/// stream, flex and SysEx8 messages have no MIDI 1.0 form and give nothing.
#[derive(Clone, Default)]
pub struct ToPackets {
	pending: [Vec<u8>; 16],
}

fn status_length(status: u8) -> usize {
	match status {
		0x80..=0xBF | 0xE0..=0xEF | 0xF2 => 3,
		0xC0..=0xDF | 0xF1 | 0xF3 => 2,
		_ => 1,
	}
}

/// One complete MIDI 1.0 message - a channel voice, system common or real-time one - as the event packet that carries it
/// on `cable`.
pub fn packet_of_message(cable: u8, bytes: &[u8]) -> Packet {
	let cin = match (bytes[0], bytes.len()) {
		(0x80..=0xEF, _) => bytes[0] >> 4,
		(_, 1) if bytes[0] >= 0xF8 => 0xF,
		(0xF6, 1) => 0x5,
		(_, 2) => 0x2,
		_ => 0x3,
	};
	let mut packet = [cable << 4 | cin, 0, 0, 0];
	packet[1..1 + bytes.len()].copy_from_slice(bytes);
	packet
}

impl ToPackets {
	pub fn new() -> ToPackets {
		ToPackets::default()
	}

	pub fn take(&mut self, message: &Ump) -> Vec<Packet> {
		let Some(group) = message.group() else { return Vec::new() };
		let word = message.words()[0];
		match message.message_type() {
			MT_MIDI1_VOICE | MT_SYSTEM => {
				let status = (word >> 16) as u8;
				let bytes = [status, (word >> 8) as u8 & 0x7F, word as u8 & 0x7F];
				alloc::vec![packet_of_message(group, &bytes[..status_length(status)])]
			}
			MT_MIDI2_VOICE => midi1_of_midi2(message).iter().map(|bytes| packet_of_message(group, &bytes[..status_length(bytes[0])])).collect(),
			MT_DATA64 => {
				let Some(part) = part_of(word) else { return Vec::new() };
				let count = (word >> 16 & 0xF).min(6) as usize;
				let all = [(word >> 8) as u8, word as u8];
				let rest = message.words()[1].to_be_bytes();
				let data = all.iter().chain(rest.iter()).take(count).copied();
				let pending = &mut self.pending[group as usize];
				if matches!(part, Part::Start | Part::Complete) {
					pending.clear();
					pending.push(0xF0);
				}
				pending.extend(data);
				let ends = matches!(part, Part::End | Part::Complete);
				if ends {
					pending.push(0xF7);
				}
				let mut out = Vec::new();
				// THREE AT A TIME, and the remainder only when the message ends.
				while pending.len() > 3 || (ends && !pending.is_empty()) {
					let take = pending.len().min(3);
					let bytes: Vec<u8> = pending.drain(..take).collect();
					let last = ends && pending.is_empty();
					let cin = if last { [0x5, 0x6, 0x7][take - 1] } else { 0x4 };
					let mut packet = [group << 4 | cin, 0, 0, 0];
					packet[1..1 + take].copy_from_slice(&bytes);
					out.push(packet);
				}
				out
			}
			_ => Vec::new(),
		}
	}
}

/// THE SPECIFICATION'S DEFAULT TRANSLATION OF A MIDI 2.0 CHANNEL VOICE MESSAGE DOWN TO MIDI 1.0 (M2-104-UM, appendix
/// D): note off and on with their velocity's top seven bits - a note on whose velocity becomes zero sent with one, so it
/// stays a note on - poly and channel pressure, a control change and pitch bend scaled the same way, a program change
/// with its bank select first when the message says the bank is valid, and a registered or assignable controller as
/// the controller-number sequence MIDI 1.0 sets one with. Per-note messages have no MIDI 1.0 form and give nothing.
pub fn midi1_of_midi2(message: &Ump) -> Vec<[u8; 3]> {
	if message.message_type() != MT_MIDI2_VOICE {
		return Vec::new();
	}
	let (first, data) = (message.words()[0], message.words()[1]);
	let channel = (first >> 16 & 0xF) as u8;
	let index = (first >> 8) as u8 & 0x7F;
	let low = first as u8 & 0x7F;
	let seven = |value: u32| (value >> 25) as u8;
	let control = |number: u8, value: u8| [0xB0 | channel, number, value & 0x7F];
	match first >> 20 & 0xF {
		0x8 => alloc::vec![[0x80 | channel, index, (data >> 25) as u8]],
		0x9 => alloc::vec![[0x90 | channel, index, ((data >> 25) as u8).max(1)]],
		0xA => alloc::vec![[0xA0 | channel, index, seven(data)]],
		0xB => alloc::vec![control(index, seven(data))],
		0xC => {
			let mut out = Vec::new();
			if first & 1 != 0 {
				out.push(control(0, (data >> 8) as u8));
				out.push(control(32, data as u8));
			}
			out.push([0xC0 | channel, (data >> 24) as u8 & 0x7F, 0]);
			out
		}
		0xD => alloc::vec![[0xD0 | channel, seven(data), 0]],
		0xE => {
			let bend = data >> 18;
			alloc::vec![[0xE0 | channel, bend as u8 & 0x7F, (bend >> 7) as u8 & 0x7F]]
		}
		// Registered (2) and assignable (3) controllers: the parameter number, then the value's top fourteen bits.
		status @ (0x2 | 0x3) => {
			let (msb, lsb) = if status == 0x2 { (101, 100) } else { (99, 98) };
			let value = data >> 18;
			alloc::vec![control(msb, index), control(lsb, low), control(6, (value >> 7) as u8), control(38, value as u8)]
		}
		_ => Vec::new(),
	}
}

// ------------------------------------------------------------------ Group Terminal Blocks

/// The class-specific descriptor type a Group Terminal Block's descriptors carry, and their two subtypes.
pub const CS_GR_TRM_BLOCK: u8 = 0x26;
pub const GR_TRM_BLOCK_HEADER: u8 = 0x01;
pub const GR_TRM_BLOCK: u8 = 0x02;
/// The most blocks one endpoint lists.
pub const MAX_BLOCKS: usize = 8;

/// Which way a block's groups carry messages, from the DEVICE's side: it receives them, sends them, or both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
	Receives,
	Sends,
	Both,
}

/// One Group Terminal Block: its id, which way its groups go, the first of them and how many, its name's string
/// index, and the protocol it says it speaks (`bMIDIProtocol`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
	pub id: u8,
	pub direction: Direction,
	pub first_group: u8,
	pub groups: u8,
	pub name: u8,
	pub protocol: u8,
}

/// Why a Group Terminal Block answer was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockRefusal {
	/// No header, or a header whose total length is not what arrived.
	Header,
	/// A block descriptor shorter than its fields, of another type, or naming groups past fifteen.
	Malformed,
	/// More than `MAX_BLOCKS`.
	TooMany,
}

/// THE ANSWER TO GET_DESCRIPTOR(GR_TRM_BLOCK): a header whose total length must be exactly what arrived, then its
/// blocks. Every number in it is the device's, so each is checked: a block's groups must lie inside the sixteen, and
/// its type must be one of the three.
pub fn blocks(answer: &[u8]) -> Result<Vec<Block>, BlockRefusal> {
	if answer.len() < 5 || answer[0] != 5 || answer[1] != CS_GR_TRM_BLOCK || answer[2] != GR_TRM_BLOCK_HEADER || u16::from_le_bytes([answer[3], answer[4]]) as usize != answer.len() {
		return Err(BlockRefusal::Header);
	}
	let mut out = Vec::new();
	let mut at = 5;
	while at < answer.len() {
		let length = answer[at] as usize;
		let record = answer.get(at..at + length).ok_or(BlockRefusal::Malformed)?;
		if length < 13 || record[1] != CS_GR_TRM_BLOCK || record[2] != GR_TRM_BLOCK {
			return Err(BlockRefusal::Malformed);
		}
		let direction = match record[4] {
			0 => Direction::Both,
			1 => Direction::Receives,
			2 => Direction::Sends,
			_ => return Err(BlockRefusal::Malformed),
		};
		let (first_group, groups) = (record[5], record[6]);
		if groups == 0 || u16::from(first_group) + u16::from(groups) > 16 {
			return Err(BlockRefusal::Malformed);
		}
		if out.len() == MAX_BLOCKS {
			return Err(BlockRefusal::TooMany);
		}
		out.push(Block { id: record[3], direction, first_group, groups, name: record[7], protocol: record[8] });
		at += length;
	}
	Ok(out)
}

#[cfg(test)]
mod tests;
