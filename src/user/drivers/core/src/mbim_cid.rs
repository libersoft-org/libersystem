// MBIM ABOVE THE FRAMING (USB MBIM 1.0, sections 9 and 10): which interfaces a mobile-broadband function is, the
// control messages a host sends and the ones it reads back, and the BASIC_CONNECT information buffers the modem
// provider needs - each one read by its own offsets and sizes, every one of which is held against the buffer it
// points into before a byte is taken.
//
// WHAT A HOST ASKS. Device capabilities, the SIM's readiness and its PIN state, registration, signal, the data
// context's activation and its IPv4 configuration. Nothing here decides whether to ask: ModemService does, and the
// class module turns its commands into these.
//
// STRINGS ARE UTF-16LE, by an offset and a size inside the information buffer. A string past the buffer, an odd
// size, or one longer than the provider contract carries is refused, not truncated.

use crate::usb_function::{Configuration, DT_CS_INTERFACE, Endpoint, Refused as ConfigRefused};
use alloc::string::String;
use alloc::vec::Vec;

pub const CLASS_COMMUNICATIONS: u8 = 0x02;
pub const SUBCLASS_MBIM: u8 = 0x0e;
pub const CLASS_CDC_DATA: u8 = 0x0a;
pub const PROTOCOL_NTB: u8 = 0x02;
pub const FN_UNION: u8 = 0x06;
pub const FN_MBIM: u8 = 0x1b;

/// The encapsulated-command class requests, and the NCM ones a host sizes transfer blocks with.
pub const REQ_SEND_ENCAPSULATED_COMMAND: u8 = 0x00;
pub const REQ_GET_ENCAPSULATED_RESPONSE: u8 = 0x01;
pub const REQ_GET_NTB_PARAMETERS: u8 = 0x80;
pub const REQ_SET_NTB_INPUT_SIZE: u8 = 0x86;
pub const RT_CLASS_INTERFACE_OUT: u8 = 0x21;
pub const RT_CLASS_INTERFACE_IN: u8 = 0xa1;
/// The notification that says a response is waiting.
pub const NOTIFY_RESPONSE_AVAILABLE: u8 = 0x01;

pub const OPEN_MSG: u32 = 0x0000_0001;
pub const CLOSE_MSG: u32 = 0x0000_0002;
pub const COMMAND_MSG: u32 = 0x0000_0003;
pub const OPEN_DONE: u32 = 0x8000_0001;
pub const CLOSE_DONE: u32 = 0x8000_0002;
pub const COMMAND_DONE: u32 = 0x8000_0003;
pub const FUNCTION_ERROR: u32 = 0x8000_0004;
pub const INDICATE_STATUS: u32 = 0x8000_0007;

/// The BASIC_CONNECT device service, as its UUID travels.
pub const BASIC_CONNECT: [u8; 16] = [0xa2, 0x89, 0xcc, 0x33, 0xbc, 0xbb, 0x8b, 0x4f, 0xb6, 0xb0, 0x13, 0x3e, 0xc2, 0xaa, 0xe6, 0xdf];
/// The Internet context type.
pub const CONTEXT_INTERNET: [u8; 16] = [0x7e, 0x5e, 0x2a, 0x7e, 0x4e, 0x6f, 0x72, 0x72, 0x73, 0x6b, 0x65, 0x6e, 0x7e, 0x5e, 0x2a, 0x7e];

pub const CID_DEVICE_CAPS: u32 = 1;
pub const CID_SUBSCRIBER_READY_STATUS: u32 = 2;
pub const CID_PIN: u32 = 4;
pub const CID_REGISTER_STATE: u32 = 9;
pub const CID_SIGNAL_STATE: u32 = 11;
pub const CID_CONNECT: u32 = 12;
pub const CID_IP_CONFIGURATION: u32 = 15;

pub const STATUS_SUCCESS: u32 = 0;

/// Bounds the provider contract carries.
pub const MAX_NAME: usize = 32;
pub const MAX_APN: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotBindable {
	NoMbimInterface,
	/// No union naming a data interface, no data setting with the bulk pair, or no notification pipe.
	Incomplete,
	Malformed(ConfigRefused),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
	pub config_value: u8,
	pub control_interface: u8,
	pub data_interface: u8,
	pub data_alternate: u8,
	pub notify: Endpoint,
	pub bulk_in: Endpoint,
	pub bulk_out: Endpoint,
	pub max_control: u16,
	pub max_segment: u16,
}

/// The mobile-broadband function of one configuration: its control interface, the data interface its union
/// names, and the data setting that carries the bulk pair.
pub fn bind(config: &[u8]) -> Result<Binding, NotBindable> {
	let parsed = Configuration::parse(config).map_err(NotBindable::Malformed)?;
	let control = parsed.settings.iter().find(|setting| setting.is(CLASS_COMMUNICATIONS, SUBCLASS_MBIM)).ok_or(NotBindable::NoMbimInterface)?;
	let mut data_interface = None;
	let mut functional = None;
	for record in parsed.functional(control) {
		if record.kind != DT_CS_INTERFACE {
			continue;
		}
		match record.field(2) {
			Ok(FN_UNION) => data_interface = record.field(4).ok(),
			Ok(FN_MBIM) => functional = Some((record.field16(5).ok(), record.field16(9).ok())),
			_ => {}
		}
	}
	let data_interface = data_interface.ok_or(NotBindable::Incomplete)?;
	let (Some(max_control), Some(max_segment)) = functional.ok_or(NotBindable::Incomplete)? else { return Err(NotBindable::Incomplete) };
	let notify = control.first(Endpoint::is_interrupt_in).ok_or(NotBindable::Incomplete)?;
	let data = parsed.settings.iter().find(|setting| setting.interface == data_interface && setting.class == CLASS_CDC_DATA && setting.protocol == PROTOCOL_NTB && setting.first(Endpoint::is_bulk_in).is_some() && setting.first(Endpoint::is_bulk_out).is_some()).ok_or(NotBindable::Incomplete)?;
	Ok(Binding { config_value: parsed.value, control_interface: control.interface, data_interface, data_alternate: data.alternate, notify, bulk_in: data.first(Endpoint::is_bulk_in).ok_or(NotBindable::Incomplete)?, bulk_out: data.first(Endpoint::is_bulk_out).ok_or(NotBindable::Incomplete)?, max_control: max_control.max(64), max_segment })
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
	Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// MBIM_OPEN_MSG: one transfer, the header and the largest control transfer the host takes.
pub fn open(transaction: u32, max_control: u32) -> Vec<u8> {
	let mut out = Vec::with_capacity(16);
	out.extend_from_slice(&OPEN_MSG.to_le_bytes());
	out.extend_from_slice(&16u32.to_le_bytes());
	out.extend_from_slice(&transaction.to_le_bytes());
	out.extend_from_slice(&max_control.to_le_bytes());
	out
}

/// A command's body, after the fragment header: the service, the CID, query or set, and the information buffer.
pub fn command(service: &[u8; 16], cid: u32, set: bool, info: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(28 + info.len());
	out.extend_from_slice(service);
	out.extend_from_slice(&cid.to_le_bytes());
	out.extend_from_slice(&u32::from(set).to_le_bytes());
	out.extend_from_slice(&(info.len() as u32).to_le_bytes());
	out.extend_from_slice(info);
	out
}

/// A COMMAND_DONE's body, read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Done {
	pub service: [u8; 16],
	pub cid: u32,
	pub status: u32,
	pub info: Vec<u8>,
}

/// An INDICATE_STATUS's body, read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Indicated {
	pub service: [u8; 16],
	pub cid: u32,
	pub info: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfoRefused {
	/// Shorter than its fixed part, or a length past what arrived.
	Short,
	/// An offset and size pointing outside the buffer, an odd string size, or a string past its bound.
	Field,
}

pub fn done(body: &[u8]) -> Result<Done, InfoRefused> {
	if body.len() < 28 {
		return Err(InfoRefused::Short);
	}
	let length = le32(body, 24).ok_or(InfoRefused::Short)? as usize;
	if 28 + length > body.len() {
		return Err(InfoRefused::Short);
	}
	Ok(Done { service: body[..16].try_into().map_err(|_| InfoRefused::Short)?, cid: le32(body, 16).ok_or(InfoRefused::Short)?, status: le32(body, 20).ok_or(InfoRefused::Short)?, info: body[28..28 + length].to_vec() })
}

pub fn indicated(body: &[u8]) -> Result<Indicated, InfoRefused> {
	if body.len() < 24 {
		return Err(InfoRefused::Short);
	}
	let length = le32(body, 20).ok_or(InfoRefused::Short)? as usize;
	if 24 + length > body.len() {
		return Err(InfoRefused::Short);
	}
	Ok(Indicated { service: body[..16].try_into().map_err(|_| InfoRefused::Short)?, cid: le32(body, 16).ok_or(InfoRefused::Short)?, info: body[24..24 + length].to_vec() })
}

/// The status an OPEN_DONE or CLOSE_DONE carries after its header.
pub fn open_status(body: &[u8]) -> Option<u32> {
	le32(body, 0)
}

// A string by its offset and size inside `info`, at most `bound` characters.
fn string(info: &[u8], at: usize, bound: usize) -> Result<Option<String>, InfoRefused> {
	let offset = le32(info, at).ok_or(InfoRefused::Short)? as usize;
	let size = le32(info, at + 4).ok_or(InfoRefused::Short)? as usize;
	if size == 0 {
		return Ok(None);
	}
	if size % 2 != 0 || size / 2 > bound || offset.checked_add(size).is_none_or(|end| end > info.len()) {
		return Err(InfoRefused::Field);
	}
	let units: Vec<u16> = info[offset..offset + size].chunks_exact(2).map(|unit| u16::from_le_bytes([unit[0], unit[1]])).collect();
	Ok(Some(String::from_utf16_lossy(&units)))
}

/// Builds an information buffer whose strings follow its fixed part, each referenced by an offset and a size.
pub struct InfoWriter {
	fixed: Vec<u8>,
	data: Vec<u8>,
	fixed_len: usize,
}

impl InfoWriter {
	pub fn new(fixed_len: usize) -> InfoWriter {
		InfoWriter { fixed: Vec::with_capacity(fixed_len), data: Vec::new(), fixed_len }
	}

	pub fn u32(&mut self, value: u32) -> &mut Self {
		self.fixed.extend_from_slice(&value.to_le_bytes());
		self
	}

	pub fn bytes(&mut self, value: &[u8]) -> &mut Self {
		self.fixed.extend_from_slice(value);
		self
	}

	/// An offset/size pair for `text`, whose UTF-16LE goes after the fixed part, four-byte aligned.
	pub fn string(&mut self, text: &str) -> &mut Self {
		if text.is_empty() {
			return self.u32(0).u32(0);
		}
		while self.data.len() % 4 != 0 {
			self.data.push(0);
		}
		let offset = self.fixed_len + self.data.len();
		let encoded: Vec<u8> = text.encode_utf16().flat_map(|unit| unit.to_le_bytes()).collect();
		let size = encoded.len();
		self.data.extend_from_slice(&encoded);
		self.u32(offset as u32).u32(size as u32)
	}

	pub fn finish(&mut self) -> Vec<u8> {
		let mut out = core::mem::take(&mut self.fixed);
		out.extend_from_slice(&self.data);
		out
	}
}

/// MBIM_CONNECT set: activate or deactivate the Internet context of session 0 with an APN.
pub fn connect_set(session: u32, activate: bool, apn: &str) -> Vec<u8> {
	// Session, command, the APN, no user name or password, no compression or authentication, IPv4, Internet.
	InfoWriter::new(60).u32(session).u32(u32::from(activate)).string(apn).u32(0).u32(0).u32(0).u32(0).u32(0).u32(0).u32(IP_TYPE_IPV4).bytes(&CONTEXT_INTERNET).finish()
}

/// The query buffers that name only a session.
pub fn session_query(session: u32) -> Vec<u8> {
	session.to_le_bytes().to_vec()
}

pub const PIN_TYPE_PIN1: u32 = 2;
pub const PIN_TYPE_PUK1: u32 = 11;
pub const PIN_OPERATION_ENTER: u32 = 0;

/// MBIM_PIN set: enter a PIN, or a PUK with the new PIN it sets.
pub fn pin_set(pin_type: u32, pin: &str, new_pin: &str) -> Vec<u8> {
	InfoWriter::new(24).u32(pin_type).u32(PIN_OPERATION_ENTER).string(pin).string(new_pin).finish()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCaps {
	pub device_id: Option<String>,
	pub firmware: Option<String>,
	pub hardware: Option<String>,
}

pub fn device_caps(info: &[u8]) -> Result<DeviceCaps, InfoRefused> {
	if info.len() < 64 {
		return Err(InfoRefused::Short);
	}
	Ok(DeviceCaps { device_id: string(info, 40, MAX_NAME)?, firmware: string(info, 48, MAX_NAME)?, hardware: string(info, 56, MAX_NAME)? })
}

pub const READY_INITIALIZED: u32 = 1;
pub const READY_SIM_NOT_INSERTED: u32 = 2;
pub const READY_BAD_SIM: u32 = 3;
pub const READY_DEVICE_LOCKED: u32 = 6;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscriberReady {
	pub state: u32,
	pub imsi: Option<String>,
	pub iccid: Option<String>,
	pub number: Option<String>,
}

pub fn subscriber_ready(info: &[u8]) -> Result<SubscriberReady, InfoRefused> {
	if info.len() < 28 {
		return Err(InfoRefused::Short);
	}
	let count = le32(info, 24).ok_or(InfoRefused::Short)? as usize;
	let number = if count > 0 { string(info, 28, 16)? } else { None };
	Ok(SubscriberReady { state: le32(info, 0).ok_or(InfoRefused::Short)?, imsi: string(info, 4, 16)?, iccid: string(info, 12, 22)?, number })
}

pub const PIN_STATE_UNLOCKED: u32 = 0;
pub const PIN_STATE_LOCKED: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinInfo {
	pub pin_type: u32,
	pub state: u32,
	pub remaining: u32,
}

pub fn pin_info(info: &[u8]) -> Result<PinInfo, InfoRefused> {
	Ok(PinInfo { pin_type: le32(info, 0).ok_or(InfoRefused::Short)?, state: le32(info, 4).ok_or(InfoRefused::Short)?, remaining: le32(info, 8).ok_or(InfoRefused::Short)? })
}

pub const REGISTER_UNKNOWN: u32 = 0;
pub const REGISTER_DEREGISTERED: u32 = 1;
pub const REGISTER_SEARCHING: u32 = 2;
pub const REGISTER_HOME: u32 = 3;
pub const REGISTER_ROAMING: u32 = 4;
pub const REGISTER_PARTNER: u32 = 5;
pub const REGISTER_DENIED: u32 = 6;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Register {
	pub state: u32,
	pub provider: Option<String>,
}

pub fn register_state(info: &[u8]) -> Result<Register, InfoRefused> {
	if info.len() < 48 {
		return Err(InfoRefused::Short);
	}
	Ok(Register { state: le32(info, 4).ok_or(InfoRefused::Short)?, provider: string(info, 28, MAX_NAME)? })
}

/// Signal: the RSSI code (0 to 31, 99 unknown) as dBm, when known.
pub fn signal_dbm(info: &[u8]) -> Result<Option<i32>, InfoRefused> {
	let rssi = le32(info, 0).ok_or(InfoRefused::Short)?;
	Ok((rssi <= 31).then(|| -113 + 2 * rssi as i32))
}

pub const IP_TYPE_IPV4: u32 = 1;
pub const ACTIVATION_ACTIVATED: u32 = 1;
pub const ACTIVATION_DEACTIVATED: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Connect {
	pub session: u32,
	pub activation: u32,
	pub ip_type: u32,
}

pub fn connect_info(info: &[u8]) -> Result<Connect, InfoRefused> {
	if info.len() < 36 {
		return Err(InfoRefused::Short);
	}
	Ok(Connect { session: le32(info, 0).ok_or(InfoRefused::Short)?, activation: le32(info, 4).ok_or(InfoRefused::Short)?, ip_type: le32(info, 12).ok_or(InfoRefused::Short)? })
}

/// What an IP_CONFIGURATION answer says of IPv4, the only family this system installs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ipv4 {
	pub address: u32,
	pub prefix: u8,
	pub gateway: Option<u32>,
	pub dns: Vec<u32>,
	pub mtu: u16,
}

/// IPv4 of an IP_CONFIGURATION answer, or `None` when it offers no IPv4 address - an IPv6-only context.
pub fn ipv4_configuration(info: &[u8]) -> Result<Option<Ipv4>, InfoRefused> {
	if info.len() < 60 {
		return Err(InfoRefused::Short);
	}
	let field = |at| le32(info, at).ok_or(InfoRefused::Short);
	let available = field(4)?;
	let count = field(12)? as usize;
	if available & 1 == 0 || count == 0 {
		return Ok(None);
	}
	let offset = field(16)? as usize;
	if offset.checked_add(8).is_none_or(|end| end > info.len()) {
		return Err(InfoRefused::Field);
	}
	let prefix = le32(info, offset).ok_or(InfoRefused::Field)?;
	if prefix > 32 {
		return Err(InfoRefused::Field);
	}
	let address = u32::from_be_bytes(info[offset + 4..offset + 8].try_into().map_err(|_| InfoRefused::Field)?);
	let gateway = if available & 2 != 0 {
		let at = field(28)? as usize;
		Some(u32::from_be_bytes(info.get(at..at + 4).ok_or(InfoRefused::Field)?.try_into().map_err(|_| InfoRefused::Field)?))
	} else {
		None
	};
	let mut dns = Vec::new();
	if available & 4 != 0 {
		let dns_count = (field(36)? as usize).min(2);
		let at = field(40)? as usize;
		for n in 0..dns_count {
			let start = at + n * 4;
			dns.push(u32::from_be_bytes(info.get(start..start + 4).ok_or(InfoRefused::Field)?.try_into().map_err(|_| InfoRefused::Field)?));
		}
	}
	let mtu = if available & 8 != 0 { field(52)?.min(u16::MAX as u32) as u16 } else { 1500 };
	Ok(Some(Ipv4 { address, prefix: prefix as u8, gateway, dns, mtu }))
}

#[cfg(test)]
mod tests;
