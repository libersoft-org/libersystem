//! THE BASIC AUDIO PROFILE'S UNICAST CLIENT, FOR ONE EARBUD: its attribute table walked, what it can play and record
//! read, and its audio stream endpoints moved through their states - and its volume.
//!
//! ONE REQUEST AT A TIME, AS ATT IS. Every step is an ATT request this module queues and the service sends; its answer
//! comes back to `on_response`, and the next one goes. The MTU is exchanged first, so an ASE's configuration and a PAC
//! record fit; a value longer than one response is read on with Read Blob.
//!
//! THE WALK: the primary services; the characteristics of the four this client uses - PACS, ASCS, CSIS and VCS - and
//! each notifying characteristic's configuration descriptor; then the PACs, the audio locations, every ASE, the set's
//! key, size and rank, and the volume, read; then notifications on for the ASEs, the control point and the volume.
//! `Event::Ready` says what was found.
//!
//! THE STREAM: `configure` writes Config Codec for a sink ASE and, for a call, a source ASE; each ASE's notification
//! moves it, and `Event::Ase` says where it is. `qos`, `enable` and `receiver_ready` follow as the group's CIS comes up
//! (`cap`); `release` ends it. Every control point answer that is not success is `Event::Refused`.

use crate::ascs::{self, Ase, Qos, State};
use crate::att::op;
use crate::le_audio::{self, Capabilities, Config, VolumeState, uuid};
use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// The MTU this client asks for.
pub const MTU: u16 = 251;
/// The most characteristics the walk keeps.
const MAX_CHARACTERISTICS: usize = 32;

/// One characteristic: its class, its value handle, its properties, and its configuration descriptor's handle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Characteristic {
	pub kind: u16,
	pub value: u16,
	pub properties: u8,
	pub configuration: Option<u16>,
	// The last handle of its declaration's group: where its descriptors end.
	end: u16,
}

/// A stream's direction, from this host's side.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
	/// This host plays: the earbud's sink ASE.
	Sink,
	/// This host records: the earbud's source ASE.
	Source,
}

/// WHAT THE WALK FOUND.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Found {
	pub sink: Option<Capabilities>,
	pub source: Option<Capabilities>,
	pub sink_locations: u32,
	/// The coordinated set: its key - decrypted - its size and this member's rank.
	pub set: Option<([u8; 16], u8, u8)>,
	pub volume: Option<VolumeState>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
	Ready(Found),
	/// An ASE's new state.
	Ase {
		direction: Direction,
		id: u8,
		state: State,
	},
	/// The control point refused an operation for an ASE: the opcode, the response code and the reason.
	Refused {
		opcode: u8,
		id: u8,
		code: u8,
		reason: u8,
	},
	Volume(VolumeState),
	/// The walk could not finish: the earbud lacks what a unicast client needs.
	Failed(&'static str),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Out {
	/// An ATT request to send; its answer goes to `on_response`.
	Request(Vec<u8>),
	Event(Event),
}

// What an outstanding request was for.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Pending {
	Mtu,
	Services { from: u16 },
	Characteristics { service: usize, from: u16 },
	Descriptors { characteristic: usize, from: u16 },
	Read { handle: u16, value: Vec<u8> },
	Configure,
	Write,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
	Walking,
	Ready,
}

pub struct Member {
	mtu: usize,
	ltk_wire: [u8; 16],
	services: Vec<(u16, u16, u16)>,
	characteristics: Vec<Characteristic>,
	queue: VecDeque<(Vec<u8>, Pending)>,
	pending: Option<Pending>,
	stage: Stage,
	found: Found,
	sink_ases: Vec<(u16, Ase)>,
	source_ases: Vec<(u16, Ase)>,
	set_key: Option<Vec<u8>>,
	set_size: u8,
	set_rank: u8,
	// The walk's discovery is over and its reads are queued: the queue running dry ends it.
	walked: bool,
}

const WANTED: [u16; 4] = [uuid::PACS, uuid::ASCS, uuid::CSIS, uuid::VCS];

fn request(code: u8, fields: &[u16]) -> Vec<u8> {
	let mut out = alloc::vec![code];
	for field in fields {
		out.extend_from_slice(&field.to_le_bytes());
	}
	out
}

fn write_request(handle: u16, value: &[u8]) -> Vec<u8> {
	let mut out = request(op::WRITE_REQUEST, &[handle]);
	out.extend_from_slice(value);
	out
}

impl Member {
	/// A walk of the earbud's table, on a link encrypted with `ltk_wire` - the key a set's SIRK is decrypted with.
	pub fn new(ltk_wire: [u8; 16]) -> (Member, Vec<Out>) {
		let mut member = Member { mtu: crate::att::DEFAULT_MTU, ltk_wire, services: Vec::new(), characteristics: Vec::new(), queue: VecDeque::new(), pending: None, stage: Stage::Walking, found: Found { sink: None, source: None, sink_locations: 0, set: None, volume: None }, sink_ases: Vec::new(), source_ases: Vec::new(), set_key: None, set_size: 0, set_rank: 0, walked: false };
		member.queue.push_back((request(op::EXCHANGE_MTU_REQUEST, &[MTU]), Pending::Mtu));
		let out = member.next();
		(member, out)
	}

	pub fn ready(&self) -> bool {
		self.stage == Stage::Ready
	}

	pub fn characteristic(&self, kind: u16) -> Option<Characteristic> {
		self.characteristics.iter().find(|characteristic| characteristic.kind == kind).copied()
	}

	/// The ASEs of a direction, as last reported.
	pub fn ases(&self, direction: Direction) -> impl Iterator<Item = &Ase> {
		let list = match direction {
			Direction::Sink => &self.sink_ases,
			Direction::Source => &self.source_ases,
		};
		list.iter().map(|(_, ase)| ase)
	}

	fn first_ase(&self, direction: Direction) -> Option<u8> {
		self.ases(direction).next().map(|ase| ase.id)
	}

	// The next queued request, where nothing is outstanding.
	fn next(&mut self) -> Vec<Out> {
		if self.pending.is_some() {
			return Vec::new();
		}
		let Some((pdu, pending)) = self.queue.pop_front() else { return Vec::new() };
		self.pending = Some(pending);
		alloc::vec![Out::Request(pdu)]
	}

	fn enqueue(&mut self, pdu: Vec<u8>, pending: Pending) -> Vec<Out> {
		self.queue.push_back((pdu, pending));
		self.next()
	}

	/// THE ANSWER TO THE OUTSTANDING REQUEST.
	pub fn on_response(&mut self, pdu: &[u8]) -> Vec<Out> {
		let Some(pending) = self.pending.take() else { return Vec::new() };
		let error = pdu.first() == Some(&op::ERROR_RESPONSE);
		let mut out = Vec::new();
		match pending {
			Pending::Mtu => {
				if !error && pdu.len() == 3 {
					self.mtu = usize::from(u16::from_le_bytes([pdu[1], pdu[2]]).min(MTU)).max(crate::att::DEFAULT_MTU);
				}
				self.queue.push_back((request(op::READ_BY_GROUP_TYPE_REQUEST, &[1, 0xffff, 0x2800]), Pending::Services { from: 1 }));
			}
			Pending::Services { .. } => {
				let mut next = None;
				if !error && pdu.len() > 2 && pdu[1] == 6 {
					for entry in pdu[2..].chunks_exact(6) {
						let (start, end, kind) = (u16::from_le_bytes([entry[0], entry[1]]), u16::from_le_bytes([entry[2], entry[3]]), u16::from_le_bytes([entry[4], entry[5]]));
						if end < start || self.services.len() >= 32 {
							continue;
						}
						self.services.push((kind, start, end));
						next = (end < 0xffff).then_some(end + 1);
					}
				}
				match next {
					Some(from) => self.queue.push_back((request(op::READ_BY_GROUP_TYPE_REQUEST, &[from, 0xffff, 0x2800]), Pending::Services { from })),
					None => self.walk_characteristics(0),
				}
			}
			Pending::Characteristics { service, from } => {
				let (_, _, end) = self.services[service];
				let mut next = None;
				if !error && pdu.len() > 2 && pdu[1] == 7 {
					for entry in pdu[2..].chunks_exact(7) {
						let declaration = u16::from_le_bytes([entry[0], entry[1]]);
						let value = u16::from_le_bytes([entry[3], entry[4]]);
						if declaration < from || value <= declaration || value > end || self.characteristics.len() >= MAX_CHARACTERISTICS {
							continue;
						}
						// THE PREVIOUS CHARACTERISTIC'S GROUP ends before this declaration.
						if let Some(last) = self.characteristics.last_mut().filter(|last| last.end >= declaration) {
							last.end = declaration - 1;
						}
						self.characteristics.push(Characteristic { kind: u16::from_le_bytes([entry[5], entry[6]]), value, properties: entry[2], configuration: None, end });
						next = (value < end).then_some(value + 1);
					}
				}
				match next {
					Some(from) => self.queue.push_back((request(op::READ_BY_TYPE_REQUEST, &[from, end, 0x2803]), Pending::Characteristics { service, from })),
					None => self.walk_characteristics(service + 1),
				}
			}
			Pending::Descriptors { characteristic, .. } => {
				if !error && pdu.len() > 2 && pdu[1] == 1 {
					let found = pdu[2..].chunks_exact(4).find(|entry| u16::from_le_bytes([entry[2], entry[3]]) == 0x2902).map(|entry| u16::from_le_bytes([entry[0], entry[1]]));
					self.characteristics[characteristic].configuration = found;
				}
				self.walk_descriptors(characteristic + 1);
			}
			Pending::Read { handle, mut value } => {
				if !error && pdu.len() > 1 {
					value.extend_from_slice(&pdu[1..]);
					// A FULL RESPONSE may have more behind it.
					if pdu.len() == self.mtu && value.len() < 512 {
						let mut blob = request(op::READ_BLOB_REQUEST, &[handle]);
						blob.extend_from_slice(&(value.len() as u16).to_le_bytes());
						self.queue.push_front((blob, Pending::Read { handle, value }));
						out.extend(self.next());
						return out;
					}
				}
				out.extend(self.on_value(handle, &value));
			}
			Pending::Configure | Pending::Write => {}
		}
		out.extend(self.next());
		// THE WALK ENDS when its last read or configuration is answered.
		if self.stage == Stage::Walking && self.walked && self.pending.is_none() {
			out.extend(self.finish_walk());
		}
		out
	}

	// The characteristics of the wanted services, one service after another; then their descriptors.
	fn walk_characteristics(&mut self, from: usize) {
		let next = (from..self.services.len()).find(|&index| WANTED.contains(&self.services[index].0));
		match next {
			Some(service) => {
				let (_, start, end) = self.services[service];
				self.queue.push_back((request(op::READ_BY_TYPE_REQUEST, &[start, end, 0x2803]), Pending::Characteristics { service, from: start }));
			}
			None => self.walk_descriptors(0),
		}
	}

	fn walk_descriptors(&mut self, from: usize) {
		let next = (from..self.characteristics.len()).find(|&index| self.characteristics[index].properties & 0x30 != 0 && self.characteristics[index].value < self.characteristics[index].end);
		match next {
			Some(characteristic) => {
				let found = self.characteristics[characteristic];
				self.queue.push_back((request(op::FIND_INFORMATION_REQUEST, &[found.value + 1, found.end]), Pending::Descriptors { characteristic, from: found.value + 1 }));
			}
			None => self.read_values(),
		}
	}

	// WHAT THE WALK READS, and the notifications it turns on.
	fn read_values(&mut self) {
		self.walked = true;
		// A DEVICE WITH NO STREAM CONTROL is not one this client drives: nothing more is read.
		if self.characteristic(uuid::ASE_CONTROL_POINT).is_none() || self.characteristic(uuid::SINK_PAC).is_none() && self.characteristic(uuid::SOURCE_PAC).is_none() {
			return;
		}
		let reads = [
			uuid::SINK_PAC,
			uuid::SOURCE_PAC,
			uuid::SINK_AUDIO_LOCATIONS,
			uuid::SET_IDENTITY_RESOLVING_KEY,
			uuid::COORDINATED_SET_SIZE,
			uuid::SET_MEMBER_RANK,
			uuid::VOLUME_STATE,
		];
		for characteristic in self.characteristics.clone() {
			if reads.contains(&characteristic.kind) || matches!(characteristic.kind, uuid::SINK_ASE | uuid::SOURCE_ASE) {
				self.queue.push_back((request(op::READ_REQUEST, &[characteristic.value]), Pending::Read { handle: characteristic.value, value: Vec::new() }));
			}
		}
		for characteristic in self.characteristics.clone() {
			if matches!(characteristic.kind, uuid::SINK_ASE | uuid::SOURCE_ASE | uuid::ASE_CONTROL_POINT | uuid::VOLUME_STATE)
				&& let Some(configuration) = characteristic.configuration
			{
				self.queue.push_back((write_request(configuration, &[0x01, 0x00]), Pending::Configure));
			}
		}
	}

	fn finish_walk(&mut self) -> Vec<Out> {
		if self.stage == Stage::Ready {
			return Vec::new();
		}
		self.stage = Stage::Ready;
		if self.characteristic(uuid::ASE_CONTROL_POINT).is_none() || self.found.sink.is_none() && self.found.source.is_none() {
			return alloc::vec![Out::Event(Event::Failed("the device has no LC3 stream endpoint this host can use"))];
		}
		if let Some(key) = self.set_key.as_ref() {
			self.found.set = le_audio::sirk_of(key, &self.ltk_wire).map(|sirk| (sirk, self.set_size, self.set_rank));
		}
		alloc::vec![Out::Event(Event::Ready(self.found.clone()))]
	}

	// A VALUE READ, or notified.
	fn on_value(&mut self, handle: u16, value: &[u8]) -> Vec<Out> {
		let Some(characteristic) = self.characteristics.iter().find(|characteristic| characteristic.value == handle).copied() else { return Vec::new() };
		match characteristic.kind {
			uuid::SINK_PAC => self.found.sink = le_audio::parse_pacs(value).as_deref().and_then(le_audio::lc3_capabilities),
			uuid::SOURCE_PAC => self.found.source = le_audio::parse_pacs(value).as_deref().and_then(le_audio::lc3_capabilities),
			uuid::SINK_AUDIO_LOCATIONS if value.len() == 4 => self.found.sink_locations = u32::from_le_bytes([value[0], value[1], value[2], value[3]]),
			uuid::SET_IDENTITY_RESOLVING_KEY => self.set_key = Some(value.to_vec()),
			uuid::COORDINATED_SET_SIZE if value.len() == 1 => self.set_size = value[0],
			uuid::SET_MEMBER_RANK if value.len() == 1 => self.set_rank = value[0],
			uuid::VOLUME_STATE => {
				let state = VolumeState::parse(value);
				self.found.volume = state;
				if self.stage == Stage::Ready
					&& let Some(state) = state
				{
					return alloc::vec![Out::Event(Event::Volume(state))];
				}
			}
			uuid::SINK_ASE | uuid::SOURCE_ASE => {
				let Some(ase) = ascs::parse_ase(value) else { return Vec::new() };
				let direction = if characteristic.kind == uuid::SINK_ASE { Direction::Sink } else { Direction::Source };
				let list = if direction == Direction::Sink { &mut self.sink_ases } else { &mut self.source_ases };
				let state = ase.state.clone();
				let id = ase.id;
				match list.iter_mut().find(|(held, _)| *held == handle) {
					Some((_, held)) => *held = ase,
					None => list.push((handle, ase)),
				}
				if self.stage == Stage::Ready {
					return alloc::vec![Out::Event(Event::Ase { direction, id, state })];
				}
			}
			uuid::ASE_CONTROL_POINT => {
				let Some(response) = ascs::parse_response(value) else { return Vec::new() };
				return response.ases.iter().filter(|(_, code, _)| *code != 0).map(|&(id, code, reason)| Out::Event(Event::Refused { opcode: response.opcode, id, code, reason })).collect();
			}
			_ => {}
		}
		Vec::new()
	}

	/// A NOTIFICATION from the earbud.
	pub fn on_notification(&mut self, handle: u16, value: &[u8]) -> Vec<Out> {
		self.on_value(handle, value)
	}

	fn control_point(&mut self, operation: Vec<u8>) -> Vec<Out> {
		let Some(point) = self.characteristic(uuid::ASE_CONTROL_POINT) else { return Vec::new() };
		self.enqueue(write_request(point.value, &operation), Pending::Write)
	}

	/// CONFIG CODEC: the sink ASE with `sink`, and the source ASE with `source` where a call records too.
	pub fn configure(&mut self, sink: Option<Config>, source: Option<Config>) -> Vec<Out> {
		let mut entries = Vec::new();
		if let (Some(config), Some(id)) = (sink, self.first_ase(Direction::Sink)) {
			entries.push((id, ascs::LOW_LATENCY, config));
		}
		if let (Some(config), Some(id)) = (source, self.first_ase(Direction::Source)) {
			entries.push((id, ascs::LOW_LATENCY, config));
		}
		if entries.is_empty() {
			return Vec::new();
		}
		self.control_point(ascs::config_codec(&entries))
	}

	/// CONFIG QOS for each configured ASE: on CIG `cig`, CIS `cis`, with its configuration's SDU and the earbud's own
	/// preferred retransmissions and latency, and the smallest presentation delay it can do.
	pub fn qos(&mut self, cig: u8, cis: u8, sink: Option<Config>, source: Option<Config>) -> Vec<Out> {
		let mut entries = Vec::new();
		for (direction, config) in [(Direction::Sink, sink), (Direction::Source, source)] {
			let Some(config) = config else { continue };
			let Some(ase) = self.ases(direction).find(|ase| matches!(ase.state, State::CodecConfigured { .. } | State::QosConfigured { .. })) else { continue };
			let preferences = match &ase.state {
				State::CodecConfigured { preferences, .. } => *preferences,
				_ => ascs::Preferences { retransmissions: 2, max_latency_ms: 20, delay_min_us: 40_000, delay_max_us: 40_000 },
			};
			entries.push(Qos { ase: ase.id, cig, cis, sdu_interval_us: config.sdu_interval_us(), max_sdu: config.max_sdu(), retransmissions: preferences.retransmissions, max_latency_ms: preferences.max_latency_ms, presentation_delay_us: preferences.delay_min_us });
		}
		if entries.is_empty() {
			return Vec::new();
		}
		self.control_point(ascs::config_qos(&entries))
	}

	/// ENABLE every QoS-configured ASE, for `contexts`.
	pub fn enable(&mut self, contexts: u16) -> Vec<Out> {
		let entries: Vec<(u8, u16)> = self.sink_ases.iter().chain(self.source_ases.iter()).filter(|(_, ase)| matches!(ase.state, State::QosConfigured { .. })).map(|(_, ase)| (ase.id, contexts)).collect();
		if entries.is_empty() {
			return Vec::new();
		}
		self.control_point(ascs::enable(&entries))
	}

	/// RECEIVER START READY for the source ASE once its CIS is up: this host is ready to take what the earbud records.
	pub fn receiver_ready(&mut self) -> Vec<Out> {
		let ids: Vec<u8> = self.source_ases.iter().filter(|(_, ase)| matches!(ase.state, State::Enabling { .. })).map(|(_, ase)| ase.id).collect();
		if ids.is_empty() {
			return Vec::new();
		}
		self.control_point(ascs::ases_only(ascs::opcode::RECEIVER_START_READY, &ids))
	}

	/// RELEASE every ASE not idle.
	pub fn release(&mut self) -> Vec<Out> {
		let ids: Vec<u8> = self.sink_ases.iter().chain(self.source_ases.iter()).filter(|(_, ase)| !matches!(ase.state, State::Idle | State::Releasing)).map(|(_, ase)| ase.id).collect();
		if ids.is_empty() {
			return Vec::new();
		}
		self.control_point(ascs::ases_only(ascs::opcode::RELEASE, &ids))
	}

	/// THE LEVEL, written as an absolute volume with the change counter the state last said.
	pub fn set_volume(&mut self, level: u8) -> Vec<Out> {
		let (Some(point), Some(state)) = (self.characteristic(uuid::VOLUME_CONTROL_POINT), self.found.volume) else { return Vec::new() };
		self.enqueue(write_request(point.value, &le_audio::set_absolute_volume(state.counter, le_audio::setting_of(level))), Pending::Write)
	}
}

#[cfg(test)]
mod tests;
