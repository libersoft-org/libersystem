//! LE AUDIO'S DATA: the length-type-value structures the Basic Audio Profile speaks - a PAC record's LC3
//! capabilities, an ASE's codec configuration, a broadcast's BASE - the choice of one LC3 configuration both sides
//! support, the announcements a broadcast is found by, a coordinated set's identifiers (CSIS) and a device's volume
//! (VCS).
//!
//! WHAT IS CHOSEN. LC3 only - the one codec the profiles mandate. For music the best of 48 kHz, 32 kHz, 24 kHz and
//! 16 kHz the sink supports, 10 ms frames, at the octets the BAP's configurations name (48_4: 120, 32_2: 80, 24_2: 60,
//! 16_2: 40); for voice 16 kHz or 8 kHz. Each frame one SDU.
//!
//! THE SET'S KEYS. A set member's SIRK arrives encrypted with the link's LTK (`sdf`) or in plain text; an RSI another
//! member advertises is resolved with it (`sih`, the same function as a resolvable address's `ah`). Both functions are
//! held to CSIS's own sample data.

use crate::{aes, cmac};
use alloc::string::String;
use alloc::vec::Vec;

/// LC3's coding format.
pub const CODEC_LC3: u8 = 0x06;
/// LC3's codec identifier as a PAC record and an ASE carry it: the coding format, then no company or vendor codec.
pub const LC3_ID: [u8; 5] = [CODEC_LC3, 0, 0, 0, 0];

/// The audio contexts the stack declares.
pub mod context {
	pub const UNSPECIFIED: u16 = 0x0001;
	pub const CONVERSATIONAL: u16 = 0x0002;
	pub const MEDIA: u16 = 0x0004;
}

/// The audio locations: a set member's channel.
pub mod location {
	pub const FRONT_LEFT: u32 = 0x0000_0001;
	pub const FRONT_RIGHT: u32 = 0x0000_0002;
}

/// The service and characteristic classes.
pub mod uuid {
	pub const ASCS: u16 = 0x184e;
	pub const BASS: u16 = 0x184f;
	pub const PACS: u16 = 0x1850;
	pub const BASIC_AUDIO_ANNOUNCEMENT: u16 = 0x1851;
	pub const BROADCAST_AUDIO_ANNOUNCEMENT: u16 = 0x1852;
	pub const CSIS: u16 = 0x1846;
	pub const VCS: u16 = 0x1844;
	pub const GTBS: u16 = 0x184c;
	pub const SINK_ASE: u16 = 0x2bc4;
	pub const SOURCE_ASE: u16 = 0x2bc5;
	pub const ASE_CONTROL_POINT: u16 = 0x2bc6;
	pub const SINK_PAC: u16 = 0x2bc9;
	pub const SINK_AUDIO_LOCATIONS: u16 = 0x2bca;
	pub const SOURCE_PAC: u16 = 0x2bcb;
	pub const SOURCE_AUDIO_LOCATIONS: u16 = 0x2bcc;
	pub const AVAILABLE_AUDIO_CONTEXTS: u16 = 0x2bcd;
	pub const SUPPORTED_AUDIO_CONTEXTS: u16 = 0x2bce;
	pub const SET_IDENTITY_RESOLVING_KEY: u16 = 0x2b84;
	pub const COORDINATED_SET_SIZE: u16 = 0x2b85;
	pub const SET_MEMBER_RANK: u16 = 0x2b87;
	pub const VOLUME_STATE: u16 = 0x2b7d;
	pub const VOLUME_CONTROL_POINT: u16 = 0x2b7e;
	pub const BEARER_PROVIDER_NAME: u16 = 0x2bb3;
	pub const BEARER_UCI: u16 = 0x2bb4;
	pub const BEARER_TECHNOLOGY: u16 = 0x2bb5;
	pub const BEARER_URI_SCHEMES: u16 = 0x2bb6;
	pub const BEARER_LIST_CURRENT_CALLS: u16 = 0x2bb9;
	pub const CONTENT_CONTROL_ID: u16 = 0x2bba;
	pub const STATUS_FLAGS: u16 = 0x2bbb;
	pub const CALL_STATE: u16 = 0x2bbd;
	pub const CALL_CONTROL_POINT: u16 = 0x2bbe;
	pub const CALL_CONTROL_POINT_OPTIONAL_OPCODES: u16 = 0x2bbf;
	pub const TERMINATION_REASON: u16 = 0x2bc0;
	pub const INCOMING_CALL: u16 = 0x2bc1;
}

/// The advertising data types LE Audio adds.
pub mod ad {
	pub const SERVICE_DATA_16: u8 = 0x16;
	pub const RSI: u8 = 0x2e;
	pub const BROADCAST_NAME: u8 = 0x30;
}

// ------------------------------------------------------------------ LTV

/// THE LTVS OF A FIELD: each a length (of type and value), a type and a value. `None` for one that runs past the field
/// or has no type - the whole field is then not one this host reads.
pub fn ltvs(mut bytes: &[u8]) -> Option<Vec<(u8, &[u8])>> {
	let mut out = Vec::new();
	while let Some((&len, rest)) = bytes.split_first() {
		let len = usize::from(len);
		if len == 0 || rest.len() < len {
			return None;
		}
		out.push((rest[0], &rest[1..len]));
		bytes = &rest[len..];
	}
	Some(out)
}

fn push_ltv(out: &mut Vec<u8>, kind: u8, value: &[u8]) {
	out.push(1 + value.len() as u8);
	out.push(kind);
	out.extend_from_slice(value);
}

// ------------------------------------------------------------------ LC3 capabilities and configuration

/// The sampling frequencies' codes, and the rate each names.
const FREQUENCIES: [(u8, u32); 8] = [(0x01, 8_000), (0x02, 11_025), (0x03, 16_000), (0x04, 22_050), (0x05, 24_000), (0x06, 32_000), (0x07, 44_100), (0x08, 48_000)];

/// The supported-frequencies bit a rate has in a capability.
fn frequency_bit(rate: u32) -> Option<u16> {
	FREQUENCIES.iter().position(|(_, held)| *held == rate).map(|index| 1 << index)
}

/// WHAT A PAC RECORD'S LC3 CAPABILITIES SAY: the rates, the frame durations, the channel counts, the octets per frame
/// and the frames an SDU may carry. Absent fields take the specification's defaults.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capabilities {
	pub frequencies: u16,
	pub durations: u8,
	pub channel_counts: u8,
	pub min_octets: u16,
	pub max_octets: u16,
	pub frames_per_sdu: u8,
}

impl Capabilities {
	pub fn parse(field: &[u8]) -> Option<Capabilities> {
		let mut out = Capabilities { frequencies: 0, durations: 0, channel_counts: 0b1, min_octets: 0, max_octets: 0, frames_per_sdu: 1 };
		for (kind, value) in ltvs(field)? {
			match (kind, value.len()) {
				(0x01, 2) => out.frequencies = u16::from_le_bytes([value[0], value[1]]),
				(0x02, 1) => out.durations = value[0],
				(0x03, 1) => out.channel_counts = value[0],
				(0x04, 4) => {
					out.min_octets = u16::from_le_bytes([value[0], value[1]]);
					out.max_octets = u16::from_le_bytes([value[2], value[3]]);
				}
				(0x05, 1) => out.frames_per_sdu = value[0],
				_ => {}
			}
		}
		Some(out)
	}

	/// The capability as LTVs, for a PAC record this host serves.
	pub fn encode(&self) -> Vec<u8> {
		let mut out = Vec::new();
		push_ltv(&mut out, 0x01, &self.frequencies.to_le_bytes());
		push_ltv(&mut out, 0x02, &[self.durations]);
		push_ltv(&mut out, 0x03, &[self.channel_counts]);
		let mut octets = self.min_octets.to_le_bytes().to_vec();
		octets.extend_from_slice(&self.max_octets.to_le_bytes());
		push_ltv(&mut out, 0x04, &octets);
		push_ltv(&mut out, 0x05, &[self.frames_per_sdu]);
		out
	}

	fn supports(&self, config: &Config) -> bool {
		let duration_bit = if config.frame_us == 7_500 { 0b01 } else { 0b10 };
		frequency_bit(config.sample_rate).is_some_and(|bit| self.frequencies & bit != 0) && self.durations & duration_bit != 0 && (self.min_octets..=self.max_octets).contains(&config.octets)
	}
}

/// ONE LC3 CONFIGURATION, as an ASE's codec configuration and a BIS's carry it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
	pub sample_rate: u32,
	pub frame_us: u32,
	pub octets: u16,
	/// The audio locations the stream carries; zero for mono with none.
	pub allocation: u32,
	pub frames_per_sdu: u8,
}

impl Config {
	pub const fn new(sample_rate: u32, frame_us: u32, octets: u16) -> Config {
		Config { sample_rate, frame_us, octets, allocation: 0, frames_per_sdu: 1 }
	}

	/// The channels the allocation names: one for none or one location.
	pub fn channels(&self) -> usize {
		(self.allocation.count_ones() as usize).max(1)
	}

	/// An SDU's interval and its largest size.
	pub fn sdu_interval_us(&self) -> u32 {
		self.frame_us * u32::from(self.frames_per_sdu.max(1))
	}

	pub fn max_sdu(&self) -> u16 {
		self.octets * self.channels() as u16 * u16::from(self.frames_per_sdu.max(1))
	}

	pub fn encode(&self) -> Vec<u8> {
		let mut out = Vec::new();
		let frequency = FREQUENCIES.iter().find(|(_, rate)| *rate == self.sample_rate).map_or(0, |(code, _)| *code);
		push_ltv(&mut out, 0x01, &[frequency]);
		push_ltv(&mut out, 0x02, &[if self.frame_us == 7_500 { 0x00 } else { 0x01 }]);
		if self.allocation != 0 {
			push_ltv(&mut out, 0x03, &self.allocation.to_le_bytes());
		}
		push_ltv(&mut out, 0x04, &self.octets.to_le_bytes());
		push_ltv(&mut out, 0x05, &[self.frames_per_sdu]);
		out
	}

	/// A configuration's LTVs over `base`'s - a BIS's over its subgroup's - each field the later one names replacing the
	/// earlier. `None` for a field no level sets that LC3 needs, or one LC3 does not have.
	pub fn parse_over(base: Option<&Config>, field: &[u8]) -> Option<Config> {
		let mut sample_rate = base.map(|config| config.sample_rate);
		let mut frame_us = base.map(|config| config.frame_us);
		let mut octets = base.map(|config| config.octets);
		let mut allocation = base.map_or(0, |config| config.allocation);
		let mut frames_per_sdu = base.map_or(1, |config| config.frames_per_sdu);
		for (kind, value) in ltvs(field)? {
			match (kind, value.len()) {
				(0x01, 1) => sample_rate = Some(FREQUENCIES.iter().find(|(code, _)| *code == value[0])?.1),
				(0x02, 1) => {
					frame_us = Some(match value[0] {
						0x00 => 7_500,
						0x01 => 10_000,
						_ => return None,
					})
				}
				(0x03, 4) => allocation = u32::from_le_bytes([value[0], value[1], value[2], value[3]]),
				(0x04, 2) => octets = Some(u16::from_le_bytes([value[0], value[1]])),
				(0x05, 1) => frames_per_sdu = value[0].max(1),
				_ => {}
			}
		}
		Some(Config { sample_rate: sample_rate?, frame_us: frame_us?, octets: octets?, allocation, frames_per_sdu })
	}

	pub fn parse(field: &[u8]) -> Option<Config> {
		Config::parse_over(None, field)
	}
}

/// What a stream is for, which decides the configurations tried.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Purpose {
	Media,
	Voice,
}

/// The BAP configurations this host asks for, best first.
pub const MEDIA_CONFIGURATIONS: [Config; 4] = [Config::new(48_000, 10_000, 120), Config::new(32_000, 10_000, 80), Config::new(24_000, 10_000, 60), Config::new(16_000, 10_000, 40)];
pub const VOICE_CONFIGURATIONS: [Config; 2] = [Config::new(16_000, 10_000, 40), Config::new(8_000, 10_000, 30)];

/// THE BEST CONFIGURATION a sink's - or a source's - LC3 capabilities support, for what the stream is for.
pub fn choose(capabilities: &Capabilities, purpose: Purpose) -> Option<Config> {
	let candidates: &[Config] = match purpose {
		Purpose::Media => &MEDIA_CONFIGURATIONS,
		Purpose::Voice => &VOICE_CONFIGURATIONS,
	};
	candidates.iter().find(|config| capabilities.supports(config)).copied()
}

/// One PAC record: its codec, its capabilities and its metadata.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Pac {
	pub codec: [u8; 5],
	pub capabilities: Vec<u8>,
	pub metadata: Vec<u8>,
}

/// A Sink or Source PAC characteristic's records. `None` for a value that runs past itself.
pub fn parse_pacs(value: &[u8]) -> Option<Vec<Pac>> {
	let (&count, mut rest) = value.split_first()?;
	let mut out = Vec::new();
	for _ in 0..count {
		let codec: [u8; 5] = rest.get(..5)?.try_into().ok()?;
		let capabilities_len = usize::from(*rest.get(5)?);
		let capabilities = rest.get(6..6 + capabilities_len)?.to_vec();
		let at = 6 + capabilities_len;
		let metadata_len = usize::from(*rest.get(at)?);
		let metadata = rest.get(at + 1..at + 1 + metadata_len)?.to_vec();
		rest = &rest[at + 1 + metadata_len..];
		out.push(Pac { codec, capabilities, metadata });
	}
	Some(out)
}

/// One PAC record's bytes, for a PAC characteristic this host serves.
pub fn encode_pac(codec: &[u8; 5], capabilities: &[u8], metadata: &[u8]) -> Vec<u8> {
	let mut out = alloc::vec![1];
	out.extend_from_slice(codec);
	out.push(capabilities.len() as u8);
	out.extend_from_slice(capabilities);
	out.push(metadata.len() as u8);
	out.extend_from_slice(metadata);
	out
}

/// The LC3 capabilities in a set of PAC records - the first LC3 record's.
pub fn lc3_capabilities(pacs: &[Pac]) -> Option<Capabilities> {
	pacs.iter().find(|pac| pac.codec == LC3_ID).and_then(|pac| Capabilities::parse(&pac.capabilities))
}

/// Metadata naming the streaming audio contexts - what an Enable carries.
pub fn streaming_contexts(contexts: u16) -> Vec<u8> {
	let mut out = Vec::new();
	push_ltv(&mut out, 0x02, &contexts.to_le_bytes());
	out
}

/// THE PACS THIS HOST SERVES as a broadcast sink: a Sink PAC of LC3 at the rates and frame durations this host decodes,
/// one or two channels, 26 to 155 octets; the front left and right locations; media and conversational contexts
/// available and supported - and no source, this sink records nothing over LE broadcast. Kind, properties, value,
/// writable, as the GATT server takes them.
pub fn sink_pacs() -> Vec<(u16, u8, Vec<u8>, bool)> {
	let capabilities = Capabilities { frequencies: 0b1011_0101, durations: 0b11, channel_counts: 0b11, min_octets: 26, max_octets: 155, frames_per_sdu: 1 };
	let contexts = [(context::MEDIA | context::CONVERSATIONAL | context::UNSPECIFIED).to_le_bytes(), [0, 0]].concat();
	alloc::vec![
		(uuid::SINK_PAC, 0x02, encode_pac(&LC3_ID, &capabilities.encode(), &[]), false),
		(uuid::SINK_AUDIO_LOCATIONS, 0x02, (location::FRONT_LEFT | location::FRONT_RIGHT).to_le_bytes().to_vec(), false),
		(uuid::AVAILABLE_AUDIO_CONTEXTS, 0x12, contexts.clone(), false),
		(uuid::SUPPORTED_AUDIO_CONTEXTS, 0x02, contexts, false),
	]
}

// ------------------------------------------------------------------ broadcast

/// One BIS of a subgroup: its index, and its configuration over the subgroup's.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Bis {
	pub index: u8,
	pub config: Config,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Subgroup {
	pub codec: [u8; 5],
	pub metadata: Vec<u8>,
	pub bises: Vec<Bis>,
}

/// A BROADCAST'S BASE: its presentation delay, and its subgroups with their streams.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Base {
	pub presentation_delay_us: u32,
	pub subgroups: Vec<Subgroup>,
}

/// The BASE a Basic Audio Announcement's service data carries - the bytes after its UUID. A subgroup of another codec
/// keeps its BISes out: this host decodes LC3 alone.
pub fn parse_base(bytes: &[u8]) -> Option<Base> {
	let delay = u32::from_le_bytes([*bytes.first()?, *bytes.get(1)?, *bytes.get(2)?, 0]);
	let count = usize::from(*bytes.get(3)?);
	let mut at = 4;
	let mut subgroups = Vec::new();
	for _ in 0..count {
		let bises = usize::from(*bytes.get(at)?);
		let codec: [u8; 5] = bytes.get(at + 1..at + 6)?.try_into().ok()?;
		let config_len = usize::from(*bytes.get(at + 6)?);
		let config = bytes.get(at + 7..at + 7 + config_len)?;
		at += 7 + config_len;
		let metadata_len = usize::from(*bytes.get(at)?);
		let metadata = bytes.get(at + 1..at + 1 + metadata_len)?.to_vec();
		at += 1 + metadata_len;
		let base = if codec == LC3_ID { Config::parse(config) } else { None };
		let mut parsed = Vec::new();
		for _ in 0..bises {
			let index = *bytes.get(at)?;
			let len = usize::from(*bytes.get(at + 1)?);
			let field = bytes.get(at + 2..at + 2 + len)?;
			at += 2 + len;
			if codec == LC3_ID
				&& let Some(config) = Config::parse_over(base.as_ref(), field)
			{
				parsed.push(Bis { index, config });
			}
		}
		subgroups.push(Subgroup { codec, metadata, bises: parsed });
	}
	Some(Base { presentation_delay_us: delay, subgroups })
}

/// A BASE's bytes, for a broadcast the fixture announces: one LC3 subgroup, each BIS with its location.
pub fn encode_base(presentation_delay_us: u32, config: &Config, locations: &[u32]) -> Vec<u8> {
	let mut out = presentation_delay_us.to_le_bytes()[..3].to_vec();
	out.push(1);
	out.push(locations.len() as u8);
	out.extend_from_slice(&LC3_ID);
	let common = Config { allocation: 0, ..*config }.encode();
	out.push(common.len() as u8);
	out.extend_from_slice(&common);
	out.push(0);
	for (index, location) in locations.iter().enumerate() {
		out.push(index as u8 + 1);
		let mut field = Vec::new();
		push_ltv(&mut field, 0x03, &location.to_le_bytes());
		out.push(field.len() as u8);
		out.extend_from_slice(&field);
	}
	out
}

/// The service data an advertisement carries for one 16-bit class: the bytes after the UUID.
pub fn service_data(data: &[u8], class: u16) -> Option<&[u8]> {
	let mut rest = data;
	while let Some((&len, tail)) = rest.split_first() {
		let len = usize::from(len);
		if len == 0 || tail.len() < len {
			return None;
		}
		let (field, after) = tail.split_at(len);
		if field[0] == ad::SERVICE_DATA_16 && field.len() >= 3 && u16::from_le_bytes([field[1], field[2]]) == class {
			return Some(&field[3..]);
		}
		rest = after;
	}
	None
}

fn ad_field(data: &[u8], kind: u8) -> Option<&[u8]> {
	let mut rest = data;
	while let Some((&len, tail)) = rest.split_first() {
		let len = usize::from(len);
		if len == 0 || tail.len() < len {
			return None;
		}
		let (field, after) = tail.split_at(len);
		if field[0] == kind {
			return Some(&field[1..]);
		}
		rest = after;
	}
	None
}

/// A BROADCAST SOURCE'S ANNOUNCEMENT in its extended advertising: its Broadcast ID, and its name where it gives one -
/// the source's words, cut to what is printable.
pub fn broadcast_announcement(data: &[u8]) -> Option<(u32, String)> {
	let id = service_data(data, uuid::BROADCAST_AUDIO_ANNOUNCEMENT)?;
	let id = u32::from_le_bytes([*id.first()?, *id.get(1)?, *id.get(2)?, 0]);
	let name = ad_field(data, ad::BROADCAST_NAME).map(|bytes| String::from_utf8_lossy(bytes).chars().filter(|character| !character.is_control()).take(32).collect()).unwrap_or_default();
	Some((id, name))
}

/// The RSI a set member advertises.
pub fn rsi(data: &[u8]) -> Option<[u8; 6]> {
	ad_field(data, ad::RSI)?.try_into().ok()
}

// ------------------------------------------------------------------ the coordinated set

fn reversed(bytes: &[u8; 16]) -> [u8; 16] {
	let mut out = *bytes;
	out.reverse();
	out
}

/// `s1(M) = AES-CMAC_ZERO(M)`.
fn s1(message: &[u8]) -> [u8; 16] {
	cmac::mac(&aes::Key::new(&[0; 16]), message)
}

/// `k1(N, SALT, P) = AES-CMAC_T(P)`, `T = AES-CMAC_SALT(N)`.
fn k1(n: &[u8; 16], salt: &[u8; 16], p: &[u8]) -> [u8; 16] {
	let t = cmac::mac(&aes::Key::new(salt), n);
	cmac::mac(&aes::Key::new(&t), p)
}

/// THE SIRK, DECRYPTED (`sdf`): `k1(K, s1("SIRKenc"), "csis") ^ EncSIRK`, K the link's LTK. Every value most
/// significant octet first, as the specification's sample data writes them.
pub fn sdf(key: &[u8; 16], encrypted: &[u8; 16]) -> [u8; 16] {
	let mask = k1(key, &s1(b"SIRKenc"), b"csis");
	let mut out = [0u8; 16];
	for (index, byte) in out.iter_mut().enumerate() {
		*byte = mask[index] ^ encrypted[index];
	}
	out
}

/// `sef` is the same function the other way.
pub fn sef(key: &[u8; 16], sirk: &[u8; 16]) -> [u8; 16] {
	sdf(key, sirk)
}

/// `sih(k, r) = e(k, r') mod 2^24`, `r'` the 24-bit `r` padded with zeros - the hash an RSI carries.
pub fn sih(sirk: &[u8; 16], prand: u32) -> u32 {
	let mut block = [0u8; 16];
	block[13..16].copy_from_slice(&prand.to_be_bytes()[1..]);
	let out = aes::Key::new(sirk).block(&block);
	u32::from_be_bytes([0, out[13], out[14], out[15]])
}

/// The Set Identity Resolving Key characteristic's value - a type and the key least significant octet first - as the
/// SIRK, most significant first: decrypted with the link's LTK (least significant first, as the bond keeps it) where it
/// is encrypted.
pub fn sirk_of(value: &[u8], ltk_wire: &[u8; 16]) -> Option<[u8; 16]> {
	let (&kind, key) = value.split_first()?;
	let key: [u8; 16] = key.try_into().ok()?;
	let key = reversed(&key);
	match kind {
		0x00 => Some(sdf(&reversed(ltk_wire), &key)),
		0x01 => Some(key),
		_ => None,
	}
}

/// WHETHER AN RSI - as the advertisement carries it, least significant octet first - is a member of the set `sirk`
/// names: its random half's top bits `01`, and its hash the SIRK's.
pub fn rsi_resolves(sirk: &[u8; 16], rsi: &[u8; 6]) -> bool {
	let hash = u32::from_le_bytes([rsi[0], rsi[1], rsi[2], 0]);
	let prand = u32::from_le_bytes([rsi[3], rsi[4], rsi[5], 0]);
	prand >> 22 == 0b01 && sih(sirk, prand) == hash
}

/// An RSI for `sirk` from 22 random bits, least significant octet first - for a fixture's set member.
pub fn make_rsi(sirk: &[u8; 16], random: u32) -> [u8; 6] {
	let prand = (random & 0x003f_ffff) | 0x0040_0000;
	let hash = sih(sirk, prand);
	let mut out = [0u8; 6];
	out[..3].copy_from_slice(&hash.to_le_bytes()[..3]);
	out[3..].copy_from_slice(&prand.to_le_bytes()[..3]);
	out
}

// ------------------------------------------------------------------ volume

/// A Volume Control Service's state: the setting, 0..=255, whether muted, and the change counter a write must name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VolumeState {
	pub setting: u8,
	pub muted: bool,
	pub counter: u8,
}

impl VolumeState {
	pub fn parse(value: &[u8]) -> Option<VolumeState> {
		match value {
			[setting, muted, counter] => Some(VolumeState { setting: *setting, muted: *muted != 0, counter: *counter }),
			_ => None,
		}
	}
}

/// `Set Absolute Volume`, naming the counter the state last said.
pub fn set_absolute_volume(counter: u8, setting: u8) -> [u8; 3] {
	[0x04, counter, setting]
}

/// AudioService's level, 0..=100, as a volume setting, and back.
pub fn setting_of(level: u8) -> u8 {
	((u32::from(level.min(100)) * 255 + 50) / 100) as u8
}

pub fn level_of(setting: u8) -> u8 {
	((u32::from(setting) * 100 + 127) / 255) as u8
}

#[cfg(test)]
mod tests;
