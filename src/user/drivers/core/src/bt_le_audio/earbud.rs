// THE EARBUDS ABOVE THEIR PAIRING: the attribute server each one is to a host, and the call bearer client each one runs
// against the host's.
//
// WRITTEN FROM THE SERVICE SPECIFICATIONS, apart from the host's: PACS's capability records and contexts, ASCS's stream
// endpoints with their state machine, their control point and its response and reason codes, CSIS's set key encrypted
// under the bond's key, VCS's volume server and its control point - and, the other way round, a TBS client that finds the
// host's generic call bearer, turns its notifications on and follows its calls.
//
// WHAT AN EARBUD REPORTS is what the far side of a real link would see: each endpoint's transitions with the
// configuration the host chose, its stream established and dropped, what it heard on its sink - judged with the
// fixture's own LC3 reader - its volume as the host set it, and the host's calls as the bearer said them.

use super::{EarLink, SIRK, heard_line, le24, ltvs, sef, u16_at, u24};
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

// ------------------------------------------------------------------ the two earbuds

pub struct Side {
	pub letter: char,
	pub name: &'static str,
	// The Audio Location of its sink, and of its microphone where it has one.
	pub location: u32,
	pub source: bool,
	pub rank: u8,
	pub source_contexts: u16,
}

pub const SIDES: [Side; 2] = [
	Side { letter: 'L', name: "fixture earbud L", location: 0x0000_0001, source: true, rank: 1, source_contexts: 0x0003 },
	Side { letter: 'R', name: "fixture earbud R", location: 0x0000_0002, source: false, rank: 2, source_contexts: 0x0000 },
];

// The contexts both sinks support and are available for: unspecified, conversational and media.
const SINK_CONTEXTS: u16 = 0x0007;
// THE ATT_MTU an earbud's server takes: BAP's floor of 64 is far below it, and an L2CAP frame of it fills one LE ACL
// buffer.
const MTU: usize = 247;
const DEFAULT_MTU: usize = 23;
// What the long-write queue holds.
const PREPARE_QUEUE: usize = 512;
// VCS's step for the relative procedures.
const STEP: u8 = 16;
// The presentation delays an endpoint supports, and the ones it prefers, in microseconds.
const DELAY_MIN: u32 = 10_000;
const DELAY_MAX: u32 = 40_000;
const PREFERRED_DELAY: (u32, u32) = (20_000, 40_000);
// The codec every PAC record names: LC3.
const LC3: [u8; 5] = [0x06, 0, 0, 0, 0];
// The frames an earbud judges before it says what it heard.
const FRAMES_HEARD: usize = 50;
// The microphone's spectral line: 20, held frame after frame a tone at 100 * ceil(20 / 2) = 1000 Hz.
const MICROPHONE_LINE: u16 = 20;

// Characteristic properties.
const READ: u8 = 0x02;
const WRITE_COMMAND: u8 = 0x04;
const WRITE: u8 = 0x08;
const NOTIFY: u8 = 0x10;

// ------------------------------------------------------------------ the capability records (PACS)

struct Pac {
	// Supported_Sampling_Frequencies: bit n is Sampling_Frequency n + 1.
	frequencies: u16,
	// Supported_Frame_Durations: bit 0 7.5 ms, bit 1 10 ms.
	durations: u8,
	// Supported_Audio_Channel_Counts: bit n is n + 1 channels.
	channels: u8,
	octets: (u16, u16),
	frames: u8,
	preferred: u16,
}

// The sink: 16, 24, 32 and 48 kHz, 10 ms, one channel, 26 to 155 octets, one frame an SDU, preferring media and
// conversation.
const SINK_PAC: Pac = Pac { frequencies: 0x00b4, durations: 0x02, channels: 0x01, octets: (26, 155), frames: 1, preferred: 0x0006 };
// The microphone: 16 and 24 kHz, 10 ms, one channel, 30 to 60 octets, one frame an SDU, preferring conversation.
const SOURCE_PAC: Pac = Pac { frequencies: 0x0014, durations: 0x02, channels: 0x01, octets: (30, 60), frames: 1, preferred: 0x0002 };

// A PAC CHARACTERISTIC'S VALUE: one record - the codec, its capabilities as LTVs, its metadata as LTVs.
fn pac_value(pac: &Pac) -> Vec<u8> {
	let f = pac.frequencies.to_le_bytes();
	let (min, max) = (pac.octets.0.to_le_bytes(), pac.octets.1.to_le_bytes());
	let capabilities = [0x03, 0x01, f[0], f[1], 0x02, 0x02, pac.durations, 0x02, 0x03, pac.channels, 0x05, 0x04, min[0], min[1], max[0], max[1], 0x02, 0x05, pac.frames];
	let p = pac.preferred.to_le_bytes();
	let metadata = [0x03, 0x01, p[0], p[1]];
	let mut value = alloc::vec![1];
	value.extend_from_slice(&LC3);
	value.push(capabilities.len() as u8);
	value.extend_from_slice(&capabilities);
	value.push(metadata.len() as u8);
	value.extend_from_slice(&metadata);
	value
}

// ------------------------------------------------------------------ a codec configuration (BAP 4.3.2)

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Codec {
	// The Sampling_Frequency value, and the rate it names.
	pub frequency: u8,
	pub rate: u32,
	pub duration_us: u32,
	pub octets: u16,
	// Audio_Channel_Allocation where present.
	pub allocation: Option<u32>,
	pub blocks: u8,
}

impl Codec {
	// The channels an SDU carries: one per allocated location, or one with no location.
	pub fn channels(&self) -> u32 {
		self.allocation.map_or(1, |allocation| allocation.count_ones().max(1))
	}

	// AN SDU'S LENGTH: a frame for every channel in every block.
	pub fn sdu(&self) -> usize {
		usize::from(self.octets) * self.channels() as usize * usize::from(self.blocks)
	}
}

const RATES: [u32; 13] = [8000, 11025, 16000, 22050, 24000, 32000, 44100, 48000, 88200, 96000, 176400, 192000, 384000];

// A CODEC CONFIGURATION READ FROM ITS LTVs: `Err` where it is malformed - a structure that overruns, one of a known type
// with the wrong length, a reserved value, or a mandatory one missing (the rate, the frame duration, the octets).
pub fn codec_configuration(bytes: &[u8]) -> Result<Codec, ()> {
	let mut codec = Codec { blocks: 1, ..Codec::default() };
	let (mut rate, mut duration) = (false, false);
	for (kind, value) in ltvs(bytes).map_err(|_| ())? {
		let length = match kind {
			0x01 | 0x02 | 0x05 => 1,
			0x03 => 4,
			0x04 => 2,
			_ => continue,
		};
		if value.len() != length {
			return Err(());
		}
		match kind {
			0x01 => {
				codec.frequency = value[0];
				codec.rate = *RATES.get(usize::from(value[0]).wrapping_sub(1)).ok_or(())?;
				rate = true;
			}
			0x02 => {
				codec.duration_us = match value[0] {
					0x00 => 7500,
					0x01 => 10_000,
					_ => return Err(()),
				};
				duration = true;
			}
			0x03 => codec.allocation = Some(u32::from_le_bytes([value[0], value[1], value[2], value[3]])),
			0x04 => codec.octets = u16::from_le_bytes([value[0], value[1]]),
			_ => codec.blocks = value[0],
		}
	}
	if !rate || !duration || codec.octets == 0 || codec.blocks == 0 {
		return Err(());
	}
	Ok(codec)
}

// WHETHER A PAC RECORD COVERS A CONFIGURATION: its rate, its frame duration, its channel count, its octets, its blocks -
// and its locations among the ones the earbud has.
fn covers(pac: &Pac, codec: &Codec, locations: u32) -> bool {
	let channels = codec.channels();
	let duration = if codec.duration_us == 10_000 { 0x02 } else { 0x01 };
	pac.frequencies & (1 << (codec.frequency - 1)) != 0 && pac.durations & duration != 0 && channels <= 8 && pac.channels & (1 << (channels - 1)) != 0 && (pac.octets.0..=pac.octets.1).contains(&codec.octets) && codec.blocks <= pac.frames && codec.allocation.is_none_or(|allocation| allocation & !locations == 0)
}

// The location a configuration allocates, as the lines say it.
fn location(allocation: Option<u32>) -> String {
	String::from(match allocation.unwrap_or(0) {
		0 => "none",
		1 => "front left",
		2 => "front right",
		3 => "front left and right",
		other => return format!("{other:#010x}"),
	})
}

// ------------------------------------------------------------------ the stream endpoints (ASCS)

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
	Idle,
	CodecConfigured,
	QosConfigured,
	Enabling,
	Streaming,
	Disabling,
	Releasing,
}

impl State {
	fn code(self) -> u8 {
		match self {
			State::Idle => 0,
			State::CodecConfigured => 1,
			State::QosConfigured => 2,
			State::Enabling => 3,
			State::Streaming => 4,
			State::Disabling => 5,
			State::Releasing => 6,
		}
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Qos {
	cig: u8,
	cis: u8,
	sdu_interval: u32,
	framing: u8,
	phy: u8,
	max_sdu: u16,
	rtn: u8,
	latency: u16,
	delay: u32,
}

struct Ase {
	id: u8,
	sink: bool,
	state: State,
	config: Vec<u8>,
	codec: Codec,
	qos: Option<Qos>,
	metadata: Vec<u8>,
}

impl Ase {
	fn on(&self, cig: u8, cis: u8) -> bool {
		self.qos.is_some_and(|qos| qos.cig == cig && qos.cis == cis)
	}

	fn word(&self) -> &'static str {
		if self.sink { "sink" } else { "source" }
	}
}

// ------------------------------------------------------------------ the attribute table

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Char {
	Name,
	SinkPac,
	SinkLocations,
	SourcePac,
	SourceLocations,
	Available,
	Supported,
	Ase(usize),
	Control,
	Sirk,
	Size,
	Rank,
	VolumeState,
	VolumeControl,
	VolumeFlags,
}

impl Char {
	fn properties(self) -> u8 {
		match self {
			Char::Available | Char::Ase(_) | Char::VolumeState | Char::VolumeFlags => READ | NOTIFY,
			Char::Control => WRITE | WRITE_COMMAND | NOTIFY,
			Char::VolumeControl => WRITE,
			_ => READ,
		}
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Attr {
	Service(u16),
	// Its properties, its value's handle and its UUID.
	Declaration(u8, u16, u16),
	Value(u16, Char),
	Configuration(Char),
}

impl Attr {
	fn kind(&self) -> u16 {
		match self {
			Attr::Service(_) => 0x2800,
			Attr::Declaration(..) => 0x2803,
			Attr::Value(uuid, _) => *uuid,
			Attr::Configuration(_) => 0x2902,
		}
	}

	// WHAT NEEDS AN ENCRYPTED LINK: every value and configuration of the LE Audio services - their tables all say
	// "encryption required" - and nothing of the layout or the name.
	fn guarded(&self) -> bool {
		!matches!(self, Attr::Service(_) | Attr::Declaration(..) | Attr::Value(_, Char::Name))
	}
}

struct Table(Vec<(u16, Attr)>);

impl Table {
	fn next(&self) -> u16 {
		self.0.len() as u16 + 1
	}

	fn service(&mut self, uuid: u16) {
		let handle = self.next();
		self.0.push((handle, Attr::Service(uuid)));
	}

	fn characteristic(&mut self, uuid: u16, ch: Char) {
		let handle = self.next();
		let properties = ch.properties();
		self.0.push((handle, Attr::Declaration(properties, handle + 1, uuid)));
		self.0.push((handle + 1, Attr::Value(uuid, ch)));
		if properties & NOTIFY != 0 {
			self.0.push((handle + 2, Attr::Configuration(ch)));
		}
	}
}

// THE EARBUD'S TABLE: GAP's name; PACS with the sink's record, locations and - on the left - the microphone's; the
// contexts; ASCS with its endpoints and control point; CSIS's key, size and rank; VCS's state, control point and flags.
fn table(side: &Side) -> Vec<(u16, Attr)> {
	let mut table = Table(Vec::new());
	table.service(0x1800);
	table.characteristic(0x2a00, Char::Name);
	table.service(0x1850);
	table.characteristic(0x2bc9, Char::SinkPac);
	table.characteristic(0x2bca, Char::SinkLocations);
	if side.source {
		table.characteristic(0x2bcb, Char::SourcePac);
		table.characteristic(0x2bcc, Char::SourceLocations);
	}
	table.characteristic(0x2bcd, Char::Available);
	table.characteristic(0x2bce, Char::Supported);
	table.service(0x184e);
	table.characteristic(0x2bc4, Char::Ase(0));
	if side.source {
		table.characteristic(0x2bc5, Char::Ase(1));
	}
	table.characteristic(0x2bc6, Char::Control);
	table.service(0x1846);
	table.characteristic(0x2b84, Char::Sirk);
	table.characteristic(0x2b85, Char::Size);
	table.characteristic(0x2b87, Char::Rank);
	table.service(0x1844);
	table.characteristic(0x2b7d, Char::VolumeState);
	table.characteristic(0x2b7e, Char::VolumeControl);
	table.characteristic(0x2b7f, Char::VolumeFlags);
	table.0
}

fn error(request: u8, handle: u16, code: u8) -> Vec<u8> {
	let h = handle.to_le_bytes();
	alloc::vec![0x01, request, h[0], h[1], code]
}

// ------------------------------------------------------------------ the call bearer client (TBS, against the host)

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Step {
	#[default]
	Idle,
	Services,
	Characteristics,
	Descriptors,
	EnableState,
	EnableControl,
	ReadState,
	Ready,
	Failed,
}

#[derive(Default)]
struct Client {
	step: Step,
	// The request awaiting the host's answer, by opcode, and the ones waiting their turn.
	waiting: Option<u8>,
	queued: VecDeque<Vec<u8>>,
	// The generic bearer's handles, what discovery found in them, and the two values and their configurations.
	range: (u16, u16),
	found: Vec<(u16, u16, u16)>,
	descriptors: Vec<(u16, u16)>,
	state: u16,
	control: u16,
	configurations: (u16, u16),
	// The first call's state as last seen, so a repeat is not said twice.
	seen: Option<Option<u8>>,
}

fn call_state(value: &[u8]) -> (Option<u8>, &'static str) {
	let Some(&state) = value.get(1).filter(|_| value.len() >= 3) else { return (None, "none") };
	let name = match state {
		0x00 => "incoming",
		0x01 => "dialing",
		0x02 => "alerting",
		0x03 => "active",
		0x04 => "locally held",
		0x05 => "remotely held",
		0x06 => "locally and remotely held",
		_ => "of an unknown kind",
	};
	(Some(state), name)
}

fn call_result(code: u8) -> String {
	String::from(match code {
		0x00 => "success",
		0x01 => "opcode not supported",
		0x02 => "operation not possible",
		0x03 => "invalid call index",
		0x04 => "state mismatch",
		0x05 => "lack of resources",
		0x06 => "invalid outgoing URI",
		other => return format!("code {other}"),
	})
}

// ------------------------------------------------------------------ the earbud

// What it heard on its sink since its stream was established.
#[derive(Default)]
struct Heard {
	refused: u32,
	lines: Vec<u16>,
	reported: bool,
}

pub struct Earbud {
	pub side: &'static Side,
	table: Vec<(u16, Attr)>,
	ases: Vec<Ase>,
	// THE CHARACTERISTICS WHOSE NOTIFICATIONS ARE ON: kept across the connections of one bond, as a bonded client's
	// configuration is.
	notify: Vec<Char>,
	volume: u8,
	muted: bool,
	counter: u8,
	persisted: bool,
	// The link's: its MTU, the long writes queued on it, and the client running over it.
	mtu: usize,
	prepared: Vec<(u16, u16, Vec<u8>)>,
	client: Client,
	heard: Heard,
	// The microphone's frame, by rate and octets - `None` where the codec would not write one.
	microphone: Option<(u32, u16, Option<Vec<u8>>)>,
	pub rsi: [u8; 6],
}

impl Earbud {
	pub fn new(side: &'static Side) -> Earbud {
		let mut ases = alloc::vec![Ase { id: 1, sink: true, state: State::Idle, config: Vec::new(), codec: Codec::default(), qos: None, metadata: Vec::new() }];
		if side.source {
			ases.push(Ase { id: 2, sink: false, state: State::Idle, config: Vec::new(), codec: Codec::default(), qos: None, metadata: Vec::new() });
		}
		Earbud { side, table: table(side), ases, notify: Vec::new(), volume: 128, muted: false, counter: 0, persisted: false, mtu: DEFAULT_MTU, prepared: Vec::new(), client: Client::default(), heard: Heard::default(), microphone: None, rsi: [0; 6] }
	}

	fn say(&self, log: &mut Vec<String>, line: String) {
		log.push(format!("earbud {} {line}", self.side.letter));
	}

	// THE ADVERTISING DATA: flags, the name, the services, ASCS's general announcement with the available contexts, and
	// the set identifier.
	pub fn advertising_data(&self) -> Vec<u8> {
		let mut data = alloc::vec![0x02, 0x01, 0x06];
		data.push(self.side.name.len() as u8 + 1);
		data.push(0x09);
		data.extend_from_slice(self.side.name.as_bytes());
		data.extend_from_slice(&[0x09, 0x03, 0x4e, 0x18, 0x50, 0x18, 0x46, 0x18, 0x44, 0x18]);
		let (sink, source) = (SINK_CONTEXTS.to_le_bytes(), self.side.source_contexts.to_le_bytes());
		data.extend_from_slice(&[0x09, 0x16, 0x4e, 0x18, 0x00, sink[0], sink[1], source[0], source[1], 0x00]);
		data.extend_from_slice(&[0x07, 0x2e]);
		data.extend_from_slice(&self.rsi);
		data
	}

	// A NEW LINK: the default MTU, nothing queued, the client to run again.
	pub fn connected(&mut self) {
		self.mtu = DEFAULT_MTU;
		self.prepared.clear();
		self.client = Client::default();
		self.heard = Heard::default();
	}

	// THE LINK IS GONE: every endpoint released with it, to idle - ASCS's Released operation, with no client to tell.
	pub fn link_lost(&mut self, log: &mut Vec<String>) {
		for at in 0..self.ases.len() {
			if self.ases[at].state != State::Idle {
				let word = self.ases[at].word();
				self.reset_ase(at);
				self.say(log, format!("{word} ASE went idle with the link"));
			}
		}
		self.connected();
	}

	// A RESET TO FACTORY SETTINGS, or a new bond: no client configuration survives it.
	pub fn forget(&mut self) {
		self.notify.clear();
	}

	fn reset_ase(&mut self, at: usize) {
		let ase = &mut self.ases[at];
		ase.state = State::Idle;
		ase.config.clear();
		ase.codec = Codec::default();
		ase.qos = None;
		ase.metadata.clear();
	}

	fn attr(&self, handle: u16) -> Option<Attr> {
		self.table.iter().find(|entry| entry.0 == handle).map(|entry| entry.1)
	}

	fn value_handle(&self, ch: Char) -> u16 {
		self.table.iter().find(|entry| matches!(entry.1, Attr::Value(_, held) if held == ch)).map_or(0, |entry| entry.0)
	}

	// ------------------------------------------------------------------ values

	fn ase_value(&self, at: usize) -> Vec<u8> {
		let ase = &self.ases[at];
		let mut value = alloc::vec![ase.id, ase.state.code()];
		match ase.state {
			// Unframed supported, LE 2M preferred, two retransmissions, 20 ms, the delays, the codec and its configuration.
			State::CodecConfigured => {
				value.extend_from_slice(&[0x00, 0x02, 2, 20, 0]);
				for delay in [DELAY_MIN, DELAY_MAX, PREFERRED_DELAY.0, PREFERRED_DELAY.1] {
					value.extend_from_slice(&le24(delay));
				}
				value.extend_from_slice(&LC3);
				value.push(ase.config.len() as u8);
				value.extend_from_slice(&ase.config);
			}
			State::QosConfigured => {
				let qos = ase.qos.expect("a QoS configured endpoint's QoS");
				value.extend_from_slice(&[qos.cig, qos.cis]);
				value.extend_from_slice(&le24(qos.sdu_interval));
				value.extend_from_slice(&[qos.framing, qos.phy]);
				value.extend_from_slice(&qos.max_sdu.to_le_bytes());
				value.push(qos.rtn);
				value.extend_from_slice(&qos.latency.to_le_bytes());
				value.extend_from_slice(&le24(qos.delay));
			}
			State::Enabling | State::Streaming | State::Disabling => {
				let qos = ase.qos.expect("an enabled endpoint's QoS");
				value.extend_from_slice(&[qos.cig, qos.cis, ase.metadata.len() as u8]);
				value.extend_from_slice(&ase.metadata);
			}
			State::Idle | State::Releasing => {}
		}
		value
	}

	fn contexts_value(&self) -> Vec<u8> {
		let (sink, source) = (SINK_CONTEXTS.to_le_bytes(), self.side.source_contexts.to_le_bytes());
		alloc::vec![sink[0], sink[1], source[0], source[1]]
	}

	// A CHARACTERISTIC'S VALUE as this client reads it; `None` where it cannot be read.
	fn value(&self, ch: Char, link: &EarLink) -> Option<Vec<u8>> {
		Some(match ch {
			Char::Name => self.side.name.as_bytes().to_vec(),
			Char::SinkPac => pac_value(&SINK_PAC),
			Char::SourcePac => pac_value(&SOURCE_PAC),
			Char::SinkLocations | Char::SourceLocations => self.side.location.to_le_bytes().to_vec(),
			Char::Available | Char::Supported => self.contexts_value(),
			Char::Ase(at) => self.ase_value(at),
			// THE SET KEY, ENCRYPTED UNDER THE BOND'S LTK and least significant first: never in plain text.
			Char::Sirk => {
				let mut encrypted = sef(&link.ltk?, &SIRK);
				encrypted.reverse();
				let mut value = alloc::vec![0x00];
				value.extend_from_slice(&encrypted);
				value
			}
			Char::Size => alloc::vec![2],
			Char::Rank => alloc::vec![self.side.rank],
			Char::VolumeState => alloc::vec![self.volume, u8::from(self.muted), self.counter],
			Char::VolumeFlags => alloc::vec![u8::from(self.persisted)],
			Char::Control | Char::VolumeControl => return None,
		})
	}

	fn attribute_value(&self, attr: Attr, link: &EarLink) -> Option<Vec<u8>> {
		match attr {
			Attr::Service(uuid) => Some(uuid.to_le_bytes().to_vec()),
			Attr::Declaration(properties, handle, uuid) => {
				let (h, u) = (handle.to_le_bytes(), uuid.to_le_bytes());
				Some(alloc::vec![properties, h[0], h[1], u[0], u[1]])
			}
			Attr::Value(_, ch) => self.value(ch, link),
			Attr::Configuration(ch) => Some(alloc::vec![u8::from(self.notify.contains(&ch)), 0]),
		}
	}

	// AN ENCRYPTION THE LINK LACKS: Insufficient Encryption where this earbud holds a key for the host, Insufficient
	// Authentication where it holds none and the two must pair first.
	fn secure(&self, attr: Attr, link: &EarLink) -> Result<(), u8> {
		if attr.guarded() && !link.encrypted {
			return Err(if link.ltk.is_some() { 0x0f } else { 0x05 });
		}
		Ok(())
	}

	fn readable(&self, attr: Attr, link: &EarLink) -> Result<(), u8> {
		if let Attr::Value(_, ch) = attr
			&& ch.properties() & READ == 0
		{
			return Err(0x02);
		}
		self.secure(attr, link)
	}

	// A NOTIFICATION of a characteristic's value, where the host turned them on and the link is encrypted - cut to what
	// the MTU carries, as ATT cuts one.
	fn notification(&self, ch: Char, value: &[u8], link: &EarLink) -> Option<Vec<u8>> {
		if !self.notify.contains(&ch) || !link.encrypted {
			return None;
		}
		let handle = self.value_handle(ch).to_le_bytes();
		let mut pdu = alloc::vec![0x1b, handle[0], handle[1]];
		pdu.extend_from_slice(&value[..value.len().min(self.mtu - 3)]);
		Some(pdu)
	}

	// ------------------------------------------------------------------ the attribute server

	// ONE ATT PDU FROM THE HOST: a request this earbud's server answers, or the host's answer to this earbud's client.
	pub fn att(&mut self, pdu: &[u8], link: &EarLink, up: &[(u8, u8)], log: &mut Vec<String>) -> Vec<Vec<u8>> {
		let Some((&opcode, body)) = pdu.split_first() else { return Vec::new() };
		match opcode {
			0x01 | 0x05 | 0x09 | 0x0b | 0x11 | 0x13 | 0x1b | 0x1d => self.client_pdu(opcode, body, log),
			0x03 | 0x1e => Vec::new(),
			_ => self.serve(opcode, body, link, up, log),
		}
	}

	fn range(body: &[u8]) -> (u16, u16) {
		(u16_at(body, 0), u16_at(body, 2))
	}

	fn serve(&mut self, opcode: u8, body: &[u8], link: &EarLink, up: &[(u8, u8)], log: &mut Vec<String>) -> Vec<Vec<u8>> {
		let length_ok = match opcode {
			0x02 | 0x0a => body.len() == 2,
			0x04 | 0x0c => body.len() == 4,
			0x06 => body.len() >= 6,
			0x08 | 0x10 => body.len() == 6 || body.len() == 20,
			0x12 | 0x52 => body.len() >= 2,
			0x16 => body.len() >= 4,
			0x18 => body.len() == 1,
			// A command this server does not speak has no answer; a request is told so.
			_ if opcode & 0x40 != 0 => return Vec::new(),
			_ => return alloc::vec![error(opcode, 0, 0x06)],
		};
		if !length_ok {
			return if opcode & 0x40 != 0 { Vec::new() } else { alloc::vec![error(opcode, 0, 0x04)] };
		}
		alloc::vec![match opcode {
			0x02 => {
				self.mtu = usize::from(u16_at(body, 0)).clamp(DEFAULT_MTU, MTU);
				let mtu = (MTU as u16).to_le_bytes();
				alloc::vec![0x03, mtu[0], mtu[1]]
			}
			0x04 => self.find_information(body),
			0x06 => self.find_by_type_value(body),
			0x08 => self.read_by_type(body, link),
			0x0a | 0x0c => self.read(opcode, body, link),
			0x10 => self.read_by_group_type(body),
			0x16 => self.prepare(body, link),
			0x18 => return self.execute(body[0], link, up, log),
			_ => return self.write(opcode, u16_at(body, 0), &body[2..], link, up, log),
		}]
	}

	fn find_information(&self, body: &[u8]) -> Vec<u8> {
		let (from, to) = Self::range(body);
		if from == 0 || from > to {
			return error(0x04, from, 0x01);
		}
		let mut out = alloc::vec![0x05, 0x01];
		for (handle, attr) in self.table.iter().filter(|entry| entry.0 >= from && entry.0 <= to) {
			if out.len() + 4 > self.mtu {
				break;
			}
			out.extend_from_slice(&handle.to_le_bytes());
			out.extend_from_slice(&attr.kind().to_le_bytes());
		}
		if out.len() == 2 { error(0x04, from, 0x0a) } else { out }
	}

	// A SERVICE'S LAST HANDLE: the one before the next service, or the table's last.
	fn group_end(&self, start: u16) -> u16 {
		self.table.iter().find(|entry| entry.0 > start && matches!(entry.1, Attr::Service(_))).map_or_else(|| self.table.last().map_or(start, |last| last.0), |next| next.0 - 1)
	}

	// PRIMARY SERVICE DISCOVERY BY UUID.
	fn find_by_type_value(&self, body: &[u8]) -> Vec<u8> {
		let (from, to) = Self::range(body);
		if from == 0 || from > to {
			return error(0x06, from, 0x01);
		}
		let (kind, wanted) = (u16_at(body, 4), &body[6..]);
		let mut out = alloc::vec![0x07];
		for (handle, attr) in self.table.iter().filter(|entry| entry.0 >= from && entry.0 <= to) {
			if let (0x2800, Attr::Service(uuid)) = (kind, attr)
				&& uuid.to_le_bytes() == wanted
			{
				if out.len() + 4 > self.mtu {
					break;
				}
				out.extend_from_slice(&handle.to_le_bytes());
				out.extend_from_slice(&self.group_end(*handle).to_le_bytes());
			}
		}
		if out.len() == 1 { error(0x06, from, 0x0a) } else { out }
	}

	fn read_by_type(&self, body: &[u8], link: &EarLink) -> Vec<u8> {
		let (from, to) = Self::range(body);
		if from == 0 || from > to {
			return error(0x08, from, 0x01);
		}
		if body.len() != 6 {
			return error(0x08, from, 0x0a);
		}
		let kind = u16_at(body, 4);
		let found: Vec<(u16, Attr)> = self.table.iter().filter(|entry| entry.0 >= from && entry.0 <= to && entry.1.kind() == kind).copied().collect();
		let Some(&(first, attr)) = found.first() else { return error(0x08, from, 0x0a) };
		if let Err(code) = self.readable(attr, link) {
			return error(0x08, first, code);
		}
		let Some(first_value) = self.attribute_value(attr, link) else { return error(0x08, first, 0x02) };
		// ONE LENGTH A RESPONSE: the entries whose value is as long as the first's.
		let size = first_value.len().min(self.mtu - 4).min(253) + 2;
		let mut out = alloc::vec![0x09, size as u8];
		for (handle, attr) in found {
			if out.len() + size > self.mtu || self.readable(attr, link).is_err() {
				break;
			}
			let Some(value) = self.attribute_value(attr, link) else { break };
			if handle != first && value.len() != first_value.len() {
				break;
			}
			out.extend_from_slice(&handle.to_le_bytes());
			out.extend_from_slice(&value[..size - 2]);
		}
		out
	}

	fn read(&self, opcode: u8, body: &[u8], link: &EarLink) -> Vec<u8> {
		let handle = u16_at(body, 0);
		let Some(attr) = self.attr(handle) else { return error(opcode, handle, 0x01) };
		if let Err(code) = self.readable(attr, link) {
			return error(opcode, handle, code);
		}
		let Some(value) = self.attribute_value(attr, link) else { return error(opcode, handle, 0x02) };
		let offset = if opcode == 0x0c { usize::from(u16_at(body, 2)) } else { 0 };
		if offset > value.len() {
			return error(opcode, handle, 0x07);
		}
		let mut out = alloc::vec![opcode + 1];
		out.extend_from_slice(&value[offset..value.len().min(offset + self.mtu - 1)]);
		out
	}

	fn read_by_group_type(&self, body: &[u8]) -> Vec<u8> {
		let (from, to) = Self::range(body);
		if from == 0 || from > to {
			return error(0x10, from, 0x01);
		}
		match (body.len(), u16_at(body, 4)) {
			(6, 0x2800) => {}
			(6, 0x2801) => return error(0x10, from, 0x0a),
			_ => return error(0x10, from, 0x10),
		}
		let mut out = alloc::vec![0x11, 6];
		for (handle, attr) in self.table.iter().filter(|entry| entry.0 >= from && entry.0 <= to) {
			if let Attr::Service(uuid) = attr {
				if out.len() + 6 > self.mtu {
					break;
				}
				out.extend_from_slice(&handle.to_le_bytes());
				out.extend_from_slice(&self.group_end(*handle).to_le_bytes());
				out.extend_from_slice(&uuid.to_le_bytes());
			}
		}
		if out.len() == 2 { error(0x10, from, 0x0a) } else { out }
	}

	// A WRITE, as a request (answered) or a command (not): a configuration, the stream control point, the volume control
	// point.
	fn write(&mut self, opcode: u8, handle: u16, value: &[u8], link: &EarLink, up: &[(u8, u8)], log: &mut Vec<String>) -> Vec<Vec<u8>> {
		let request = opcode == 0x12;
		let fail = |code: u8| if request { alloc::vec![error(0x12, handle, code)] } else { Vec::new() };
		let acknowledged = || if request { alloc::vec![alloc::vec![0x13]] } else { Vec::new() };
		let Some(attr) = self.attr(handle) else { return fail(0x01) };
		let permitted = match attr {
			Attr::Configuration(_) => request,
			Attr::Value(_, ch) => ch.properties() & (if request { WRITE } else { WRITE_COMMAND }) != 0,
			_ => false,
		};
		if !permitted {
			return fail(0x03);
		}
		if let Err(code) = self.secure(attr, link) {
			return fail(code);
		}
		match attr {
			Attr::Configuration(ch) => {
				if value.len() != 2 {
					return fail(0x0d);
				}
				// Indications are not what these characteristics have.
				if value[0] & 0x02 != 0 {
					return fail(0xfd);
				}
				self.notify.retain(|held| *held != ch);
				if value[0] & 0x01 != 0 {
					self.notify.push(ch);
				}
				acknowledged()
			}
			Attr::Value(_, Char::Control) => {
				let mut out = acknowledged();
				out.extend(self.control_point(value, link, up, log));
				out
			}
			Attr::Value(_, Char::VolumeControl) => match self.volume_control(value, link, log) {
				Ok(notifications) => {
					let mut out = acknowledged();
					out.extend(notifications);
					out
				}
				Err(code) => fail(code),
			},
			_ => fail(0x03),
		}
	}

	// A LONG WRITE'S PIECE, queued until it is executed.
	fn prepare(&mut self, body: &[u8], link: &EarLink) -> Vec<u8> {
		let (handle, offset) = (u16_at(body, 0), u16_at(body, 2));
		let Some(attr) = self.attr(handle) else { return error(0x16, handle, 0x01) };
		if !matches!(attr, Attr::Value(_, ch) if ch.properties() & WRITE != 0) {
			return error(0x16, handle, 0x03);
		}
		if let Err(code) = self.secure(attr, link) {
			return error(0x16, handle, code);
		}
		if self.prepared.iter().map(|part| part.2.len()).sum::<usize>() + body.len() - 4 > PREPARE_QUEUE {
			return error(0x16, handle, 0x09);
		}
		self.prepared.push((handle, offset, body[4..].to_vec()));
		let mut out = alloc::vec![0x17];
		out.extend_from_slice(body);
		out
	}

	// THE QUEUE EXECUTED - each value whole, its pieces in order from offset zero - or cancelled.
	fn execute(&mut self, flags: u8, link: &EarLink, up: &[(u8, u8)], log: &mut Vec<String>) -> Vec<Vec<u8>> {
		let queue = core::mem::take(&mut self.prepared);
		match flags {
			0x00 => return alloc::vec![alloc::vec![0x19]],
			0x01 => {}
			_ => return alloc::vec![error(0x18, 0, 0x04)],
		}
		let mut values: Vec<(u16, Vec<u8>)> = Vec::new();
		for (handle, offset, part) in queue {
			let at = match values.iter().position(|value| value.0 == handle) {
				Some(at) => at,
				None => {
					values.push((handle, Vec::new()));
					values.len() - 1
				}
			};
			if usize::from(offset) != values[at].1.len() {
				return alloc::vec![error(0x18, handle, 0x07)];
			}
			values[at].1.extend_from_slice(&part);
		}
		let mut out = alloc::vec![alloc::vec![0x19]];
		for (handle, value) in values {
			match self.attr(handle) {
				Some(Attr::Value(_, Char::Control)) => out.extend(self.control_point(&value, link, up, log)),
				Some(Attr::Value(_, Char::VolumeControl)) => match self.volume_control(&value, link, log) {
					Ok(notifications) => out.extend(notifications),
					Err(code) => return alloc::vec![error(0x18, handle, code)],
				},
				_ => return alloc::vec![error(0x18, handle, 0x03)],
			}
		}
		out
	}

	// ------------------------------------------------------------------ the ASE control point

	// ONE CONTROL POINT WRITE: the control point's answer notified first, then each endpoint it moved, in the order they
	// moved.
	fn control_point(&mut self, value: &[u8], link: &EarLink, up: &[(u8, u8)], log: &mut Vec<String>) -> Vec<Vec<u8>> {
		let mut moved = Vec::new();
		let response = self.operate(value, up, log, &mut moved);
		let mut out: Vec<Vec<u8>> = self.notification(Char::Control, &response, link).into_iter().collect();
		for (at, snapshot) in moved {
			out.extend(self.notification(Char::Ase(at), &snapshot, link));
		}
		out
	}

	// THE OPERATION RUN, endpoint by endpoint: its answer - the opcode, the count, and for each endpoint its id, response
	// code and reason - or, for an opcode it does not have or a length that does not add up, one entry for none.
	pub(super) fn operate(&mut self, value: &[u8], up: &[(u8, u8)], log: &mut Vec<String>, moved: &mut Vec<(usize, Vec<u8>)>) -> Vec<u8> {
		let Some((&opcode, params)) = value.split_first() else { return alloc::vec![0x00, 0xff, 0x00, 0x02, 0x00] };
		if !(0x01..=0x08).contains(&opcode) {
			return alloc::vec![opcode, 0xff, 0x00, 0x01, 0x00];
		}
		let Some(entries) = Self::entries(opcode, params) else { return alloc::vec![opcode, 0xff, 0x00, 0x02, 0x00] };
		let mut response = alloc::vec![opcode, entries.len() as u8];
		for entry in entries {
			let id = entry[0];
			let (code, reason) = match self.ases.iter().position(|ase| ase.id == id) {
				None => (0x03, 0x00),
				Some(at) => match self.apply(opcode, at, entry, up, log, moved) {
					Ok(()) => (0x00, 0x00),
					Err(refusal) => refusal,
				},
			};
			if code != 0 {
				self.say(log, format!("refused operation {opcode:#04x} on ASE {id}: response {code:#04x} reason {reason:#04x}"));
			}
			response.extend_from_slice(&[id, code, reason]);
		}
		response
	}

	// AN OPERATION'S PARAMETER ARRAYS, one slice an endpoint: `None` unless the count is at least one and the arrays
	// consume the parameters exactly.
	fn entries(opcode: u8, params: &[u8]) -> Option<Vec<&[u8]>> {
		let (&count, mut rest) = params.split_first()?;
		if count == 0 {
			return None;
		}
		let mut entries = Vec::new();
		for _ in 0..count {
			let length = match opcode {
				0x01 => 9 + usize::from(*rest.get(8)?),
				0x02 => 16,
				0x03 | 0x07 => 2 + usize::from(*rest.get(1)?),
				_ => 1,
			};
			if rest.len() < length {
				return None;
			}
			entries.push(&rest[..length]);
			rest = &rest[length..];
		}
		rest.is_empty().then_some(entries)
	}

	fn mark(&self, at: usize, moved: &mut Vec<(usize, Vec<u8>)>) {
		moved.push((at, self.ase_value(at)));
	}

	fn apply(&mut self, opcode: u8, at: usize, entry: &[u8], up: &[(u8, u8)], log: &mut Vec<String>, moved: &mut Vec<(usize, Vec<u8>)>) -> Result<(), (u8, u8)> {
		let state = self.ases[at].state;
		let sink = self.ases[at].sink;
		let word = self.ases[at].word();
		let invalid = Err((0x04, 0x00));
		match opcode {
			// CONFIG CODEC: from Idle, Codec Configured or QoS Configured; LC3, well formed, and covered by the record for
			// the endpoint's direction.
			0x01 => {
				if !matches!(state, State::Idle | State::CodecConfigured | State::QosConfigured) {
					return invalid;
				}
				if entry[3..8] != LC3 {
					return Err((0x07, 0x01));
				}
				let config = &entry[9..];
				let codec = codec_configuration(config).map_err(|()| (0x09, 0x02))?;
				if !covers(if sink { &SINK_PAC } else { &SOURCE_PAC }, &codec, self.side.location) {
					return Err((0x06, 0x00));
				}
				let ase = &mut self.ases[at];
				ase.state = State::CodecConfigured;
				ase.config = config.to_vec();
				ase.codec = codec;
				ase.qos = None;
				ase.metadata.clear();
				self.mark(at, moved);
				self.say(log, format!("{word} ASE is codec configured: {} Hz, {} us, {} octets, location {}", codec.rate, codec.duration_us, codec.octets, location(codec.allocation)));
			}
			// CONFIG QOS: from Codec Configured or QoS Configured, a CIS no other endpoint of this direction holds, values
			// within their ranges, an SDU interval the frames fill, an SDU that holds them, a delay this earbud supports.
			0x02 => {
				if !matches!(state, State::CodecConfigured | State::QosConfigured) {
					return invalid;
				}
				let qos = Qos { cig: entry[1], cis: entry[2], sdu_interval: u24(&entry[3..6]), framing: entry[6], phy: entry[7], max_sdu: u16_at(entry, 8), rtn: entry[10], latency: u16_at(entry, 11), delay: u24(&entry[13..16]) };
				let taken = self.ases.iter().enumerate().any(|(other, ase)| other != at && ase.sink == sink && !matches!(ase.state, State::Idle | State::CodecConfigured | State::Releasing) && ase.on(qos.cig, qos.cis));
				if qos.cig > 0xef || qos.cis > 0xef || taken {
					return Err((0x09, 0x0a));
				}
				if !(0xff..=0xf_ffff).contains(&qos.sdu_interval) {
					return Err((0x09, 0x03));
				}
				if qos.framing > 1 {
					return Err((0x09, 0x04));
				}
				if qos.phy == 0 || qos.phy & !0x07 != 0 {
					return Err((0x09, 0x05));
				}
				if qos.max_sdu > 0x0fff {
					return Err((0x09, 0x06));
				}
				if !(0x0005..=0x0fa0).contains(&qos.latency) {
					return Err((0x09, 0x08));
				}
				let codec = self.ases[at].codec;
				if qos.sdu_interval != codec.duration_us * u32::from(codec.blocks) {
					return Err((0x07, 0x03));
				}
				if usize::from(qos.max_sdu) < codec.sdu() {
					return Err((0x07, 0x06));
				}
				if !(DELAY_MIN..=DELAY_MAX).contains(&qos.delay) {
					return Err((0x07, 0x09));
				}
				let ase = &mut self.ases[at];
				ase.state = State::QosConfigured;
				ase.qos = Some(qos);
				self.mark(at, moved);
				self.say(log, format!("{word} ASE is QoS configured: CIG {} CIS {}, SDU interval {} us, max SDU {}", qos.cig, qos.cis, qos.sdu_interval, qos.max_sdu));
			}
			// ENABLE: from QoS Configured, with metadata whose streaming contexts the earbud is available for. A sink whose
			// CIS is already up starts receiving at once - the Receiver Start Ready a sink initiates itself.
			0x03 => {
				if state != State::QosConfigured {
					return invalid;
				}
				let metadata = &entry[2..];
				let contexts = self.contexts(metadata, sink)?;
				self.ases[at].state = State::Enabling;
				self.ases[at].metadata = metadata.to_vec();
				self.mark(at, moved);
				self.say(log, format!("{word} ASE is enabling, contexts {contexts:#06x}"));
				let qos = self.ases[at].qos.expect("an enabled endpoint's QoS");
				if sink && up.contains(&(qos.cig, qos.cis)) {
					self.start(at, log, moved);
				}
			}
			// RECEIVER START READY: the client is the receiver, so a source endpoint only - Enabling, its CIS up.
			0x04 => {
				if sink {
					return Err((0x05, 0x00));
				}
				let coupled = self.ases[at].qos.is_some_and(|qos| up.contains(&(qos.cig, qos.cis)));
				if state != State::Enabling || !coupled {
					return invalid;
				}
				self.start(at, log, moved);
			}
			// DISABLE: a sink goes back to QoS Configured; a source keeps sending until the client stops receiving.
			0x05 => {
				if !matches!(state, State::Enabling | State::Streaming) {
					return invalid;
				}
				if sink {
					self.ases[at].state = State::QosConfigured;
					self.ases[at].metadata.clear();
					self.say(log, format!("{word} ASE was disabled"));
				} else {
					self.ases[at].state = State::Disabling;
					self.say(log, format!("{word} ASE is disabling"));
				}
				self.mark(at, moved);
			}
			// RECEIVER STOP READY: a source endpoint that is disabling.
			0x06 => {
				if sink {
					return Err((0x05, 0x00));
				}
				if state != State::Disabling {
					return invalid;
				}
				self.ases[at].state = State::QosConfigured;
				self.ases[at].metadata.clear();
				self.mark(at, moved);
				self.say(log, format!("{word} ASE stopped sending"));
			}
			// UPDATE METADATA, while Enabling or Streaming.
			0x07 => {
				if !matches!(state, State::Enabling | State::Streaming) {
					return invalid;
				}
				let metadata = &entry[2..];
				let contexts = self.contexts(metadata, sink)?;
				self.ases[at].metadata = metadata.to_vec();
				self.mark(at, moved);
				self.say(log, format!("{word} ASE's metadata was updated, contexts {contexts:#06x}"));
			}
			// RELEASE, from any configured state: Releasing, and idle at once where no CIS is coupled - otherwise when the
			// client takes it down.
			_ => {
				if matches!(state, State::Idle | State::Releasing) {
					return invalid;
				}
				let coupled = self.ases[at].qos.is_some_and(|qos| up.contains(&(qos.cig, qos.cis)));
				self.ases[at].state = State::Releasing;
				self.mark(at, moved);
				self.say(log, format!("{word} ASE is releasing"));
				if !coupled {
					self.released(at, log, moved);
				}
			}
		}
		Ok(())
	}

	// THE STREAMING CONTEXTS AN ENABLE OR AN UPDATE CARRIES - unspecified where it carries none, as BAP reads its absence -
	// refused where the metadata is malformed or names no context the earbud is available for in that direction.
	fn contexts(&self, metadata: &[u8], sink: bool) -> Result<u16, (u8, u8)> {
		let mut contexts = 0x0001;
		for (kind, value) in ltvs(metadata).map_err(|kind| (0x0c, kind))? {
			if kind == 0x02 {
				if value.len() != 2 {
					return Err((0x0c, 0x02));
				}
				contexts = u16::from_le_bytes([value[0], value[1]]);
			}
		}
		let available = if sink { SINK_CONTEXTS } else { self.side.source_contexts };
		if contexts & available == 0 {
			return Err((0x0b, 0x02));
		}
		Ok(contexts)
	}

	fn start(&mut self, at: usize, log: &mut Vec<String>, moved: &mut Vec<(usize, Vec<u8>)>) {
		self.ases[at].state = State::Streaming;
		self.mark(at, moved);
		let word = self.ases[at].word();
		self.say(log, format!("{word} ASE is streaming"));
		if !self.ases[at].sink {
			self.say(log, String::from("sends its microphone"));
		}
	}

	fn released(&mut self, at: usize, log: &mut Vec<String>, moved: &mut Vec<(usize, Vec<u8>)>) {
		let word = self.ases[at].word();
		self.reset_ase(at);
		self.mark(at, moved);
		self.say(log, format!("{word} ASE is released"));
	}

	// ------------------------------------------------------------------ the streams

	// WHETHER AN ENDPOINT WAITS FOR THIS CIS: Enabling, configured for it.
	pub fn enabling(&self, cig: u8, cis: u8) -> bool {
		self.ases.iter().any(|ase| ase.state == State::Enabling && ase.on(cig, cis))
	}

	// THE CIS IS UP: a sink waiting for it is ready to receive, and starts streaming on its own.
	pub fn cis_up(&mut self, cig: u8, cis: u8, link: &EarLink, log: &mut Vec<String>) -> Vec<Vec<u8>> {
		self.say(log, String::from("CIS is established"));
		self.heard = Heard::default();
		let mut moved = Vec::new();
		for at in 0..self.ases.len() {
			if self.ases[at].sink && self.ases[at].state == State::Enabling && self.ases[at].on(cig, cis) {
				self.start(at, log, &mut moved);
			}
		}
		moved.into_iter().filter_map(|(at, snapshot)| self.notification(Char::Ase(at), &snapshot, link)).collect()
	}

	// THE CIS IS DOWN: a streaming or disabling endpoint falls back to QoS Configured, a releasing one is released.
	pub fn cis_down(&mut self, cig: u8, cis: u8, link: &EarLink, log: &mut Vec<String>) -> Vec<Vec<u8>> {
		self.say(log, String::from("CIS is disconnected"));
		let mut moved = Vec::new();
		for at in 0..self.ases.len() {
			if !self.ases[at].on(cig, cis) {
				continue;
			}
			match self.ases[at].state {
				State::Streaming | State::Disabling => {
					self.ases[at].state = State::QosConfigured;
					self.ases[at].metadata.clear();
					self.mark(at, &mut moved);
					let word = self.ases[at].word();
					self.say(log, format!("{word} ASE lost its CIS and is QoS configured again"));
				}
				State::Releasing => self.released(at, log, &mut moved),
				_ => {}
			}
		}
		moved.into_iter().filter_map(|(at, snapshot)| self.notification(Char::Ase(at), &snapshot, link)).collect()
	}

	// THE CIS WENT WITH THE LINK: said, and the link's loss does the rest.
	pub fn cis_lost(&mut self, log: &mut Vec<String>) {
		self.say(log, String::from("CIS is disconnected"));
	}

	// ONE SDU ON THE SINK'S STREAM, judged: a frame for every channel, each read whole by the fixture's LC3 reader. After
	// the fiftieth that is not silent it says what it heard - once a stream.
	pub fn hear(&mut self, cig: u8, cis: u8, sdu: &[u8], log: &mut Vec<String>) {
		let Some(at) = self.ases.iter().position(|ase| ase.sink && ase.state == State::Streaming && ase.on(cig, cis)) else { return };
		// A ZERO-LENGTH SDU is what a sink must tolerate, and says nothing.
		if sdu.is_empty() {
			return;
		}
		let codec = self.ases[at].codec;
		let frame = usize::from(codec.octets);
		if frame == 0 || sdu.len() != codec.sdu() {
			self.heard.refused += 1;
		} else {
			let mut loudest = None;
			for piece in sdu.chunks(frame) {
				match crate::bt_lc3::read(piece, codec.rate, codec.duration_us) {
					Ok(info) => loudest = loudest.or(info.loudest),
					Err(_) => self.heard.refused += 1,
				}
			}
			if let Some(line) = loudest
				&& self.heard.lines.len() < FRAMES_HEARD
			{
				self.heard.lines.push(line);
			}
		}
		if !self.heard.reported && self.heard.lines.len() == FRAMES_HEARD {
			self.heard.reported = true;
			log.push(heard_line(self.side.letter, codec.rate, self.heard.refused, most_frequent(&self.heard.lines)));
		}
	}

	// AN SDU THIS CONTROLLER COULD NOT TAKE WHOLE, on a stream the sink is receiving.
	pub fn refuse(&mut self, cig: u8, cis: u8) {
		if self.ases.iter().any(|ase| ase.sink && ase.state == State::Streaming && ase.on(cig, cis)) {
			self.heard.refused += 1;
		}
	}

	// THE MICROPHONE'S NEXT SDU on this CIS, while its endpoint streams or is disabling: one spectral line, written once
	// by the fixture's LC3 writer and sent every 10 ms. `None` where nothing is sent.
	pub fn microphone(&mut self, cig: u8, cis: u8, log: &mut Vec<String>) -> Option<Vec<u8>> {
		let at = self.ases.iter().position(|ase| !ase.sink && matches!(ase.state, State::Streaming | State::Disabling) && ase.on(cig, cis))?;
		let codec = self.ases[at].codec;
		let fresh = !matches!(&self.microphone, Some((rate, octets, _)) if *rate == codec.rate && *octets == codec.octets);
		if fresh {
			let mut frame = alloc::vec![0u8; usize::from(codec.octets)];
			let written = match crate::bt_lc3::write_tone(MICROPHONE_LINE, codec.rate, codec.duration_us, &mut frame) {
				Ok(()) => Some(frame),
				Err(why) => {
					self.say(log, format!("could not write its microphone: {why}"));
					None
				}
			};
			self.microphone = Some((codec.rate, codec.octets, written));
		}
		let frame = self.microphone.as_ref()?.2.as_ref()?;
		Some(frame.repeat(codec.channels() as usize * usize::from(codec.blocks)))
	}

	// ------------------------------------------------------------------ the volume (VCS)

	// ONE VOLUME CONTROL POINT WRITE: an opcode it has, of its length, with the change counter it holds - or the ATT error
	// VCS names. The notifications of what changed.
	fn volume_control(&mut self, value: &[u8], link: &EarLink, log: &mut Vec<String>) -> Result<Vec<Vec<u8>>, u8> {
		let Some(&opcode) = value.first().filter(|opcode| **opcode <= 0x06) else { return Err(0x81) };
		if value.len() != if opcode == 0x04 { 3 } else { 2 } {
			return Err(0x0d);
		}
		if value[1] != self.counter {
			return Err(0x80);
		}
		let (volume, muted) = (self.volume, self.muted);
		match opcode {
			0x00 | 0x02 => self.volume = self.volume.saturating_sub(STEP),
			0x01 | 0x03 => self.volume = self.volume.saturating_add(STEP),
			0x04 => self.volume = value[2],
			_ => {}
		}
		match opcode {
			0x02 | 0x03 | 0x05 => self.muted = false,
			0x06 => self.muted = true,
			_ => {}
		}
		match opcode {
			0x04 => self.say(log, format!("volume was set to {}", self.volume)),
			0x00..=0x03 => self.say(log, format!("volume was stepped to {}", self.volume)),
			_ => {}
		}
		if opcode == 0x05 || (matches!(opcode, 0x02 | 0x03) && muted) {
			self.say(log, String::from("was unmuted"));
		}
		if opcode == 0x06 {
			self.say(log, String::from("was muted"));
		}
		Ok(self.volume_changed(volume, muted, Some(link)))
	}

	// WHAT A CHANGE OWES: the counter moves only where the state did, and the flags say a user set the volume.
	fn volume_changed(&mut self, volume: u8, muted: bool, link: Option<&EarLink>) -> Vec<Vec<u8>> {
		if self.volume == volume && self.muted == muted {
			return Vec::new();
		}
		self.counter = self.counter.wrapping_add(1);
		let mut out = Vec::new();
		let state = [self.volume, u8::from(self.muted), self.counter];
		out.extend(link.and_then(|link| self.notification(Char::VolumeState, &state, link)));
		if self.volume != volume && !self.persisted {
			self.persisted = true;
			out.extend(link.and_then(|link| self.notification(Char::VolumeFlags, &[1], link)));
		}
		out
	}

	pub fn own_volume(&mut self, volume: u8, link: Option<&EarLink>, log: &mut Vec<String>) -> Vec<Vec<u8>> {
		let (before, muted) = (self.volume, self.muted);
		self.volume = volume;
		self.say(log, format!("changed its own volume to {volume}"));
		self.volume_changed(before, muted, link)
	}

	// ------------------------------------------------------------------ the call bearer client

	fn request(&mut self, pdu: Vec<u8>) -> Vec<Vec<u8>> {
		self.client.waiting = Some(pdu[0]);
		alloc::vec![pdu]
	}

	fn ranged(opcode: u8, from: u16, to: u16, kind: Option<u16>) -> Vec<u8> {
		let mut pdu = alloc::vec![opcode];
		pdu.extend_from_slice(&from.to_le_bytes());
		pdu.extend_from_slice(&to.to_le_bytes());
		if let Some(kind) = kind {
			pdu.extend_from_slice(&kind.to_le_bytes());
		}
		pdu
	}

	fn handled(opcode: u8, handle: u16, value: &[u8]) -> Vec<u8> {
		let mut pdu = alloc::vec![opcode];
		pdu.extend_from_slice(&handle.to_le_bytes());
		pdu.extend_from_slice(value);
		pdu
	}

	// THE LINK IS ENCRYPTED AND THE KEYS ARE KEPT: the earbud looks for the host's generic call bearer, from the first
	// primary service on.
	pub fn secured(&mut self) -> Vec<Vec<u8>> {
		if self.client.step != Step::Idle {
			return Vec::new();
		}
		self.client.step = Step::Services;
		self.request(Self::ranged(0x10, 0x0001, 0xffff, Some(0x2800)))
	}

	fn failed(&mut self, log: &mut Vec<String>, why: String) -> Vec<Vec<u8>> {
		self.client.step = Step::Failed;
		self.say(log, why);
		Vec::new()
	}

	// THE HOST'S ANSWER TO THIS EARBUD'S REQUEST, or what the host notified.
	fn client_pdu(&mut self, opcode: u8, body: &[u8], log: &mut Vec<String>) -> Vec<Vec<u8>> {
		if matches!(opcode, 0x1b | 0x1d) {
			if body.len() >= 2 {
				self.bearer_value(u16_at(body, 0), &body[2..], log);
			}
			return if opcode == 0x1d { alloc::vec![alloc::vec![0x1e]] } else { Vec::new() };
		}
		let Some(asked) = self.client.waiting else { return Vec::new() };
		let refused = opcode == 0x01 && body.first() == Some(&asked);
		if opcode != asked + 1 && !refused {
			return Vec::new();
		}
		self.client.waiting = None;
		let code = if refused { body.get(3).copied().unwrap_or(0) } else { 0 };
		let mut out = match self.client.step {
			Step::Services => self.services(refused, code, body, log),
			Step::Characteristics => self.characteristics(refused, code, body, log),
			Step::Descriptors => self.descriptors(refused, code, body, log),
			Step::EnableState | Step::EnableControl | Step::ReadState if refused => self.failed(log, format!("found the host's call bearer, which refused its request with error {code:#04x}")),
			Step::EnableState => {
				self.client.step = Step::EnableControl;
				let configuration = self.client.configurations.1;
				self.request(Self::handled(0x12, configuration, &[0x01, 0x00]))
			}
			Step::EnableControl => {
				self.client.step = Step::ReadState;
				self.say(log, String::from("found the host's call bearer"));
				let state = self.client.state;
				self.request(Self::handled(0x0a, state, &[]))
			}
			Step::ReadState => {
				self.client.step = Step::Ready;
				let state = self.client.state;
				self.bearer_value(state, body, log);
				Vec::new()
			}
			Step::Ready if refused => {
				log.push(format!("earbud {}'s call control write was refused with error {code:#04x}", self.side.letter));
				Vec::new()
			}
			_ => Vec::new(),
		};
		if self.client.waiting.is_none()
			&& self.client.step == Step::Ready
			&& let Some(next) = self.client.queued.pop_front()
		{
			out.extend(self.request(next));
		}
		out
	}

	// PRIMARY SERVICE DISCOVERY, every service in turn until the generic bearer.
	fn services(&mut self, refused: bool, code: u8, body: &[u8], log: &mut Vec<String>) -> Vec<Vec<u8>> {
		if refused {
			return self.failed(log, if code == 0x0a { String::from("found no call bearer on the host") } else { format!("could not discover the host's services: error {code:#04x}") });
		}
		let mut last = 0u16;
		let size = usize::from(body.first().copied().unwrap_or(0));
		if size >= 4 {
			for entry in body[1..].chunks_exact(size) {
				let (start, end) = (u16_at(entry, 0), u16_at(entry, 2));
				last = last.max(end);
				if size == 6 && u16_at(entry, 4) == 0x184c {
					self.client.step = Step::Characteristics;
					self.client.range = (start, end);
					return self.request(Self::ranged(0x08, start, end, Some(0x2803)));
				}
			}
		}
		if last == 0 || last == 0xffff {
			return self.failed(log, String::from("found no call bearer on the host"));
		}
		self.request(Self::ranged(0x10, last + 1, 0xffff, Some(0x2800)))
	}

	// THE BEARER'S CHARACTERISTICS, every declaration in its range.
	fn characteristics(&mut self, refused: bool, code: u8, body: &[u8], log: &mut Vec<String>) -> Vec<Vec<u8>> {
		let end = self.client.range.1;
		if refused && code != 0x0a {
			return self.failed(log, format!("could not discover the host's call bearer: error {code:#04x}"));
		}
		if !refused {
			let size = usize::from(body.first().copied().unwrap_or(0));
			let mut last = end;
			if size >= 5 {
				for entry in body[1..].chunks_exact(size) {
					last = u16_at(entry, 0);
					if size == 7 {
						self.client.found.push((last, u16_at(entry, 3), u16_at(entry, 5)));
					}
				}
			}
			if last < end {
				return self.request(Self::ranged(0x08, last + 1, end, Some(0x2803)));
			}
		}
		let value = |uuid: u16| self.client.found.iter().find(|entry| entry.2 == uuid).map_or(0, |entry| entry.1);
		let (state, control) = (value(0x2bbd), value(0x2bbe));
		if state == 0 || control == 0 {
			return self.failed(log, String::from("found the host's call bearer without its call state or control point"));
		}
		self.client.state = state;
		self.client.control = control;
		self.client.step = Step::Descriptors;
		let from = self.client.found.first().map_or(state, |first| first.1 + 1);
		self.request(Self::ranged(0x04, from, end, None))
	}

	// THE BEARER'S DESCRIPTORS, and the two configurations the client turns on.
	fn descriptors(&mut self, refused: bool, code: u8, body: &[u8], log: &mut Vec<String>) -> Vec<Vec<u8>> {
		let end = self.client.range.1;
		if refused && code != 0x0a {
			return self.failed(log, format!("could not discover the host's call bearer: error {code:#04x}"));
		}
		if !refused {
			let size = match body.first() {
				Some(1) => 4,
				Some(2) => 18,
				_ => return self.failed(log, String::from("could not read the host's call bearer descriptors")),
			};
			let mut last = end;
			for entry in body[1..].chunks_exact(size) {
				last = u16_at(entry, 0);
				self.client.descriptors.push((last, if size == 4 { u16_at(entry, 2) } else { 0 }));
			}
			if last < end {
				return self.request(Self::ranged(0x04, last + 1, end, None));
			}
		}
		// A VALUE'S CONFIGURATION: the first one after it and before the next declaration.
		let configuration = |value: u16| {
			let limit = self.client.found.iter().map(|entry| entry.0).filter(|declaration| *declaration > value).min().unwrap_or(end + 1);
			self.client.descriptors.iter().find(|descriptor| descriptor.1 == 0x2902 && descriptor.0 > value && descriptor.0 < limit).map_or(0, |descriptor| descriptor.0)
		};
		let configurations = (configuration(self.client.state), configuration(self.client.control));
		if configurations.0 == 0 || configurations.1 == 0 {
			return self.failed(log, String::from("found the host's call bearer without its notifications"));
		}
		self.client.configurations = configurations;
		self.client.step = Step::EnableState;
		self.request(Self::handled(0x12, configurations.0, &[0x01, 0x00]))
	}

	// WHAT THE BEARER SAID: the first call's state, or the answer to this earbud's call control write.
	fn bearer_value(&mut self, handle: u16, value: &[u8], log: &mut Vec<String>) {
		if handle == 0 {
			return;
		}
		if handle == self.client.state {
			let (state, name) = call_state(value);
			if self.client.seen != Some(state) {
				self.client.seen = Some(state);
				self.say(log, format!("saw call state {name}"));
			}
		} else if handle == self.client.control && value.len() >= 3 {
			let result = call_result(value[2]);
			log.push(format!("earbud {}'s call control was answered {result}", self.side.letter));
		}
	}

	// THE EARBUD PRESSES A CALL BUTTON: opcode `opcode` on call index 1, written to the host's Call Control Point with a
	// Write Request - after whatever it is still waiting for. `None` where it found no bearer.
	pub fn call(&mut self, opcode: u8, log: &mut Vec<String>) -> Option<Vec<Vec<u8>>> {
		if self.client.step != Step::Ready || self.client.control == 0 {
			self.say(log, String::from("has no call bearer to control"));
			return None;
		}
		let write = Self::handled(0x12, self.client.control, &[opcode, 1]);
		self.say(log, format!("wrote the host's call control point: opcode {opcode} on call 1"));
		if self.client.waiting.is_some() {
			self.client.queued.push_back(write);
			return Some(Vec::new());
		}
		Some(self.request(write))
	}
}

// The most frequent line - the lowest of those tied.
fn most_frequent(lines: &[u16]) -> u16 {
	let mut best = (0usize, 0u16);
	for &line in lines {
		let count = lines.iter().filter(|other| **other == line).count();
		if count > best.0 || (count == best.0 && line < best.1) {
			best = (count, line);
		}
	}
	best.1
}

#[cfg(test)]
pub(super) mod probe {
	// What the tests read of an earbud without a client: the handle of a characteristic by its UUID, and an endpoint's
	// state code.
	use super::{Attr, Earbud};

	pub fn value_handle(earbud: &Earbud, uuid: u16) -> u16 {
		earbud.table.iter().find(|entry| matches!(entry.1, Attr::Value(held, _) if held == uuid)).map_or(0, |entry| entry.0)
	}

	pub fn configuration_handle(earbud: &Earbud, uuid: u16) -> u16 {
		value_handle(earbud, uuid) + 1
	}

	pub fn state(earbud: &Earbud, id: u8) -> u8 {
		earbud.ases.iter().find(|ase| ase.id == id).map_or(0xff, |ase| ase.state.code())
	}
}
