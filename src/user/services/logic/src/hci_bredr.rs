//! THE CLASSIC (BR/EDR) HCI COMMANDS THIS HOST SENDS AND THE EVENTS IT READS: inquiry and its extended results,
//! paging and accepting, Secure Simple Pairing's host half, encryption, role switch, sniff, scan modes and synchronous
//! connections. Every number on this wire is little-endian, the device address included, and every event's declared
//! length is held against the bytes that arrived - `hci_codec`'s rules, for the other radio.
//!
//! WHAT THE HOST DOES IN SECURE SIMPLE PAIRING. The controller runs the key agreement over LMP; the host answers its
//! questions - the IO capability, the user's confirmation of a six-digit value, a passkey, a link key from the bond
//! store - and stores the link key the controller hands back. So this module carries the questions and answers, and
//! `bt_pairing` decides them.

use alloc::vec::Vec;

#[cfg(test)]
mod tests;

/// Opcodes, as `(OGF << 10) | OCF`.
pub mod opcode {
	pub const INQUIRY: u16 = 0x0401;
	pub const INQUIRY_CANCEL: u16 = 0x0402;
	pub const CREATE_CONNECTION: u16 = 0x0405;
	pub const DISCONNECT: u16 = 0x0406;
	pub const CREATE_CONNECTION_CANCEL: u16 = 0x0408;
	pub const ACCEPT_CONNECTION_REQUEST: u16 = 0x0409;
	pub const REJECT_CONNECTION_REQUEST: u16 = 0x040A;
	pub const LINK_KEY_REQUEST_REPLY: u16 = 0x040B;
	pub const LINK_KEY_REQUEST_NEGATIVE_REPLY: u16 = 0x040C;
	pub const PIN_CODE_REQUEST_REPLY: u16 = 0x040D;
	pub const PIN_CODE_REQUEST_NEGATIVE_REPLY: u16 = 0x040E;
	pub const AUTHENTICATION_REQUESTED: u16 = 0x0411;
	pub const SET_CONNECTION_ENCRYPTION: u16 = 0x0413;
	pub const REMOTE_NAME_REQUEST: u16 = 0x0419;
	pub const READ_REMOTE_SUPPORTED_FEATURES: u16 = 0x041B;
	pub const READ_REMOTE_EXTENDED_FEATURES: u16 = 0x041C;
	pub const SETUP_SYNCHRONOUS_CONNECTION: u16 = 0x0428;
	pub const ACCEPT_SYNCHRONOUS_CONNECTION_REQUEST: u16 = 0x0429;
	pub const REJECT_SYNCHRONOUS_CONNECTION_REQUEST: u16 = 0x042A;
	pub const IO_CAPABILITY_REQUEST_REPLY: u16 = 0x042B;
	pub const USER_CONFIRMATION_REQUEST_REPLY: u16 = 0x042C;
	pub const USER_CONFIRMATION_REQUEST_NEGATIVE_REPLY: u16 = 0x042D;
	pub const USER_PASSKEY_REQUEST_REPLY: u16 = 0x042E;
	pub const USER_PASSKEY_REQUEST_NEGATIVE_REPLY: u16 = 0x042F;
	pub const IO_CAPABILITY_REQUEST_NEGATIVE_REPLY: u16 = 0x0434;
	pub const SNIFF_MODE: u16 = 0x0803;
	pub const EXIT_SNIFF_MODE: u16 = 0x0804;
	pub const SWITCH_ROLE: u16 = 0x080B;
	pub const WRITE_LINK_POLICY_SETTINGS: u16 = 0x080D;
	pub const WRITE_DEFAULT_LINK_POLICY_SETTINGS: u16 = 0x080F;
	pub const WRITE_LOCAL_NAME: u16 = 0x0C13;
	pub const WRITE_PAGE_TIMEOUT: u16 = 0x0C18;
	pub const WRITE_SCAN_ENABLE: u16 = 0x0C1A;
	pub const WRITE_CLASS_OF_DEVICE: u16 = 0x0C24;
	pub const WRITE_INQUIRY_MODE: u16 = 0x0C45;
	pub const WRITE_EXTENDED_INQUIRY_RESPONSE: u16 = 0x0C52;
	pub const WRITE_SIMPLE_PAIRING_MODE: u16 = 0x0C56;
	pub const WRITE_LE_HOST_SUPPORT: u16 = 0x0C6D;
	pub const WRITE_SECURE_CONNECTIONS_HOST_SUPPORT: u16 = 0x0C7A;
	pub const READ_LOCAL_SUPPORTED_FEATURES: u16 = 0x1003;
	pub const READ_BUFFER_SIZE: u16 = 0x1005;
}

/// Classic event codes.
pub mod event {
	pub const INQUIRY_COMPLETE: u8 = 0x01;
	pub const INQUIRY_RESULT: u8 = 0x02;
	pub const CONNECTION_COMPLETE: u8 = 0x03;
	pub const CONNECTION_REQUEST: u8 = 0x04;
	pub const DISCONNECTION_COMPLETE: u8 = 0x05;
	pub const AUTHENTICATION_COMPLETE: u8 = 0x06;
	pub const REMOTE_NAME_REQUEST_COMPLETE: u8 = 0x07;
	pub const ENCRYPTION_CHANGE: u8 = 0x08;
	pub const READ_REMOTE_SUPPORTED_FEATURES_COMPLETE: u8 = 0x0B;
	pub const ROLE_CHANGE: u8 = 0x12;
	pub const MODE_CHANGE: u8 = 0x14;
	pub const PIN_CODE_REQUEST: u8 = 0x16;
	pub const LINK_KEY_REQUEST: u8 = 0x17;
	pub const LINK_KEY_NOTIFICATION: u8 = 0x18;
	pub const INQUIRY_RESULT_WITH_RSSI: u8 = 0x22;
	pub const READ_REMOTE_EXTENDED_FEATURES_COMPLETE: u8 = 0x23;
	pub const SYNCHRONOUS_CONNECTION_COMPLETE: u8 = 0x2C;
	pub const EXTENDED_INQUIRY_RESULT: u8 = 0x2F;
	pub const ENCRYPTION_KEY_REFRESH_COMPLETE: u8 = 0x30;
	pub const IO_CAPABILITY_REQUEST: u8 = 0x31;
	pub const IO_CAPABILITY_RESPONSE: u8 = 0x32;
	pub const USER_CONFIRMATION_REQUEST: u8 = 0x33;
	pub const USER_PASSKEY_REQUEST: u8 = 0x34;
	pub const SIMPLE_PAIRING_COMPLETE: u8 = 0x36;
	pub const USER_PASSKEY_NOTIFICATION: u8 = 0x3B;
	pub const KEYPRESS_NOTIFICATION: u8 = 0x3C;
}

/// The General Inquiry Access Code, little-endian.
pub const GIAC: [u8; 3] = [0x33, 0x8B, 0x9E];

/// The packet types this host offers when it pages: DM1, DH1, DM3, DH3, DM5 and DH5, the EDR types left allowed.
pub const ACL_PACKET_TYPES: u16 = 0xCC18;

/// The scan modes `Write Scan Enable` takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scan {
	None,
	/// Inquiry scan only: discoverable and not connectable, which this host never asks for.
	Inquiry,
	/// Page scan: connectable.
	Page,
	/// Both: discoverable and connectable.
	Both,
}

impl Scan {
	pub const fn value(self) -> u8 {
		match self {
			Scan::None => 0,
			Scan::Inquiry => 1,
			Scan::Page => 2,
			Scan::Both => 3,
		}
	}
}

/// An IO capability, as SSP's IO Capability exchange names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IoCapability {
	DisplayOnly,
	DisplayYesNo,
	KeyboardOnly,
	NoInputNoOutput,
}

impl IoCapability {
	pub const fn value(self) -> u8 {
		match self {
			IoCapability::DisplayOnly => 0,
			IoCapability::DisplayYesNo => 1,
			IoCapability::KeyboardOnly => 2,
			IoCapability::NoInputNoOutput => 3,
		}
	}

	pub const fn from_value(value: u8) -> Option<IoCapability> {
		match value {
			0 => Some(IoCapability::DisplayOnly),
			1 => Some(IoCapability::DisplayYesNo),
			2 => Some(IoCapability::KeyboardOnly),
			3 => Some(IoCapability::NoInputNoOutput),
			_ => None,
		}
	}
}

/// The authentication requirements this host states: MITM required, general bonding - `0x05` - or, with nobody to
/// answer a prompt, general bonding without MITM - `0x04`.
pub const AUTH_MITM_BONDING: u8 = 0x05;
pub const AUTH_BONDING: u8 = 0x04;

/// A link key's type, as Link Key Notification gives it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyType {
	Combination,
	DebugCombination,
	/// P-192, unauthenticated or authenticated.
	UnauthenticatedP192,
	AuthenticatedP192,
	ChangedCombination,
	UnauthenticatedP256,
	AuthenticatedP256,
	Other(u8),
}

impl KeyType {
	pub const fn from_value(value: u8) -> KeyType {
		match value {
			0x00 => KeyType::Combination,
			0x03 => KeyType::DebugCombination,
			0x04 => KeyType::UnauthenticatedP192,
			0x05 => KeyType::AuthenticatedP192,
			0x06 => KeyType::ChangedCombination,
			0x07 => KeyType::UnauthenticatedP256,
			0x08 => KeyType::AuthenticatedP256,
			other => KeyType::Other(other),
		}
	}

	pub const fn value(self) -> u8 {
		match self {
			KeyType::Combination => 0x00,
			KeyType::DebugCombination => 0x03,
			KeyType::UnauthenticatedP192 => 0x04,
			KeyType::AuthenticatedP192 => 0x05,
			KeyType::ChangedCombination => 0x06,
			KeyType::UnauthenticatedP256 => 0x07,
			KeyType::AuthenticatedP256 => 0x08,
			KeyType::Other(other) => other,
		}
	}
}

fn with_address(address: &[u8; 6], rest: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(6 + rest.len());
	out.extend_from_slice(address);
	out.extend_from_slice(rest);
	out
}

/// `Inquiry`: the general access code, `length` in units of 1.28 s, at most `responses` (0 for unlimited).
pub fn inquiry(length: u8, responses: u8) -> Vec<u8> {
	let mut out = GIAC.to_vec();
	out.extend_from_slice(&[length.clamp(1, 0x30), responses]);
	out
}

/// `Create Connection` to a wire-order address: the packet types, the page scan repetition mode and clock offset from
/// inquiry where known, and a role switch allowed.
pub fn create_connection(address: &[u8; 6], repetition: u8, clock_offset: u16) -> Vec<u8> {
	let mut rest = Vec::with_capacity(7);
	rest.extend_from_slice(&ACL_PACKET_TYPES.to_le_bytes());
	rest.push(repetition);
	rest.push(0);
	rest.extend_from_slice(&(clock_offset | if clock_offset != 0 { 0x8000 } else { 0 }).to_le_bytes());
	rest.push(1);
	with_address(address, &rest)
}

/// `Accept Connection Request`, this host staying where it is (`0x01`: remain peripheral) or taking central (`0x00`).
pub fn accept_connection(address: &[u8; 6], become_central: bool) -> Vec<u8> {
	with_address(address, &[u8::from(!become_central)])
}

/// `Reject Connection Request` with a reason - limited resources `0x0D`, security `0x0E`, unacceptable address `0x0F`.
pub fn reject_connection(address: &[u8; 6], reason: u8) -> Vec<u8> {
	with_address(address, &[reason])
}

pub fn link_key_reply(address: &[u8; 6], key: &[u8; 16]) -> Vec<u8> {
	with_address(address, key)
}

pub fn address_only(address: &[u8; 6]) -> Vec<u8> {
	address.to_vec()
}

/// `PIN Code Request Reply`: legacy pairing, for a device that cannot do better, when the operator asked for it.
pub fn pin_code_reply(address: &[u8; 6], pin: &[u8]) -> Option<Vec<u8>> {
	if pin.is_empty() || pin.len() > 16 {
		return None;
	}
	let mut code = [0u8; 16];
	code[..pin.len()].copy_from_slice(pin);
	let mut rest = Vec::with_capacity(17);
	rest.push(pin.len() as u8);
	rest.extend_from_slice(&code);
	Some(with_address(address, &rest))
}

pub fn io_capability_reply(address: &[u8; 6], capability: IoCapability, authentication: u8) -> Vec<u8> {
	// No out-of-band data: this machine has no out-of-band channel.
	with_address(address, &[capability.value(), 0x00, authentication])
}

/// `IO Capability Request Negative Reply` with the reason: pairing not allowed `0x18`.
pub fn io_capability_negative(address: &[u8; 6]) -> Vec<u8> {
	with_address(address, &[0x18])
}

pub fn passkey_reply(address: &[u8; 6], passkey: u32) -> Vec<u8> {
	with_address(address, &passkey.min(999_999).to_le_bytes())
}

pub fn handle_only(handle: u16) -> Vec<u8> {
	handle.to_le_bytes().to_vec()
}

pub fn set_encryption(handle: u16, on: bool) -> Vec<u8> {
	let mut out = handle.to_le_bytes().to_vec();
	out.push(u8::from(on));
	out
}

pub fn remote_name_request(address: &[u8; 6], repetition: u8, clock_offset: u16) -> Vec<u8> {
	let mut rest = alloc::vec![repetition, 0];
	rest.extend_from_slice(&(clock_offset | if clock_offset != 0 { 0x8000 } else { 0 }).to_le_bytes());
	with_address(address, &rest)
}

/// SNIFF'S BOUNDS: the interval between 100 ms and 1.28 s in 0.625 ms slots, two attempts, a timeout of one. An idle
/// keyboard is put there so it does not hold the radio awake.
pub const SNIFF_MAX_SLOTS: u16 = 2048;
pub const SNIFF_MIN_SLOTS: u16 = 160;

pub fn sniff_mode(handle: u16, max_slots: u16, min_slots: u16) -> Vec<u8> {
	let max = max_slots.clamp(SNIFF_MIN_SLOTS, SNIFF_MAX_SLOTS) & !1;
	let min = min_slots.clamp(SNIFF_MIN_SLOTS, max) & !1;
	let mut out = handle.to_le_bytes().to_vec();
	for value in [max, min, 2, 1] {
		out.extend_from_slice(&value.to_le_bytes());
	}
	out
}

pub fn switch_role(address: &[u8; 6], central: bool) -> Vec<u8> {
	with_address(address, &[u8::from(!central)])
}

/// The link policy this host allows on every link: role switch and sniff mode.
pub const LINK_POLICY: u16 = 0x0001 | 0x0004;

pub fn write_link_policy(handle: u16, policy: u16) -> Vec<u8> {
	let mut out = handle.to_le_bytes().to_vec();
	out.extend_from_slice(&policy.to_le_bytes());
	out
}

/// The class of device this host states: Computer (major 0x01), Desktop (minor 0x01), with the Audio, Rendering,
/// Object Transfer and Networking service classes.
pub const CLASS_OF_DEVICE: u32 = 0x00_1C_01_04 | (1 << 21) | (1 << 18) | (1 << 20) | (1 << 17);

pub fn class_of_device(class: u32) -> Vec<u8> {
	class.to_le_bytes()[..3].to_vec()
}

/// `Write Local Name`: the name padded to its 248 bytes.
pub fn local_name(name: &[u8]) -> Vec<u8> {
	let mut out = alloc::vec![0u8; 248];
	let len = name.len().min(247);
	out[..len].copy_from_slice(&name[..len]);
	out
}

/// `Setup Synchronous Connection` for a voice link: 8 kHz each way (8000 bytes a second), the latency and
/// retransmission effort the codec asks for, and the voice setting - CVSD's `0x0060` or transparent `0x0063` for mSBC.
pub fn setup_synchronous(handle: u16, max_latency_ms: u16, voice: u16, retransmission: u8, packet_types: u16) -> Vec<u8> {
	let mut out = handle.to_le_bytes().to_vec();
	out.extend_from_slice(&8000u32.to_le_bytes());
	out.extend_from_slice(&8000u32.to_le_bytes());
	out.extend_from_slice(&max_latency_ms.to_le_bytes());
	out.extend_from_slice(&voice.to_le_bytes());
	out.push(retransmission);
	out.extend_from_slice(&packet_types.to_le_bytes());
	out
}

pub fn accept_synchronous(address: &[u8; 6], max_latency_ms: u16, voice: u16, retransmission: u8, packet_types: u16) -> Vec<u8> {
	let mut rest = Vec::with_capacity(17);
	rest.extend_from_slice(&8000u32.to_le_bytes());
	rest.extend_from_slice(&8000u32.to_le_bytes());
	rest.extend_from_slice(&max_latency_ms.to_le_bytes());
	rest.extend_from_slice(&voice.to_le_bytes());
	rest.push(retransmission);
	rest.extend_from_slice(&packet_types.to_le_bytes());
	with_address(address, &rest)
}

/// The fields of an extended inquiry response this host reads.
pub mod eir {
	pub const UUID16_INCOMPLETE: u8 = 0x02;
	pub const UUID16_COMPLETE: u8 = 0x03;
	pub const NAME_SHORT: u8 = 0x08;
	pub const NAME_COMPLETE: u8 = 0x09;
}

/// What an extended inquiry response says: the name and the 16-bit service class UUIDs.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Eir {
	pub name: Vec<u8>,
	pub uuids: Vec<u16>,
}

/// THE FIELDS OF AN EXTENDED INQUIRY RESPONSE, read to their zero-length terminator or the 240 bytes' end; a field that
/// runs past them ends the read with what was read.
pub fn parse_eir(mut bytes: &[u8]) -> Eir {
	let mut out = Eir::default();
	while let Some((&len, rest)) = bytes.split_first() {
		let len = usize::from(len);
		if len == 0 || len > rest.len() {
			break;
		}
		let (kind, data) = (rest[0], &rest[1..len]);
		match kind {
			eir::NAME_SHORT | eir::NAME_COMPLETE if out.name.is_empty() || kind == eir::NAME_COMPLETE => out.name = data.iter().take(248).copied().collect(),
			eir::UUID16_INCOMPLETE | eir::UUID16_COMPLETE => out.uuids.extend(data.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]]))),
			_ => {}
		}
		bytes = &rest[len..];
	}
	out
}

/// One device inquiry found: its wire-order address, page scan repetition mode, class of device, clock offset, signal
/// strength where reported, and its extended response where it gave one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Found {
	pub address: [u8; 6],
	pub repetition: u8,
	pub class: u32,
	pub clock_offset: u16,
	pub rssi: Option<i8>,
	pub eir: Eir,
}

/// One classic event.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
	InquiryComplete { status: u8 },
	Inquiry(Vec<Found>),
	ConnectionComplete { status: u8, handle: u16, address: [u8; 6], acl: bool, encrypted: bool },
	ConnectionRequest { address: [u8; 6], class: u32, link: u8 },
	AuthenticationComplete { status: u8, handle: u16 },
	RemoteName { status: u8, address: [u8; 6], name: Vec<u8> },
	EncryptionChange { status: u8, handle: u16, enabled: u8 },
	RemoteFeatures { status: u8, handle: u16, features: [u8; 8] },
	RemoteExtendedFeatures { status: u8, handle: u16, page: u8, features: [u8; 8] },
	RoleChange { status: u8, address: [u8; 6], central: bool },
	ModeChange { status: u8, handle: u16, mode: u8, interval: u16 },
	PinCodeRequest { address: [u8; 6] },
	LinkKeyRequest { address: [u8; 6] },
	LinkKeyNotification { address: [u8; 6], key: [u8; 16], kind: KeyType },
	SynchronousComplete { status: u8, handle: u16, address: [u8; 6], esco: bool, air_mode: u8 },
	EncryptionKeyRefresh { status: u8, handle: u16 },
	IoCapabilityRequest { address: [u8; 6] },
	IoCapabilityResponse { address: [u8; 6], capability: u8, authentication: u8 },
	UserConfirmationRequest { address: [u8; 6], value: u32 },
	UserPasskeyRequest { address: [u8; 6] },
	SimplePairingComplete { status: u8, address: [u8; 6] },
	UserPasskeyNotification { address: [u8; 6], passkey: u32 },
	KeypressNotification { address: [u8; 6], kind: u8 },
}

fn address(bytes: &[u8], at: usize) -> Option<[u8; 6]> {
	bytes.get(at..at + 6)?.try_into().ok()
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
	Some(u16::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
	Some(u32::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?, *bytes.get(at + 2)?, *bytes.get(at + 3)?]))
}

fn class_at(bytes: &[u8], at: usize) -> Option<u32> {
	Some(u32::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?, *bytes.get(at + 2)?, 0]))
}

/// ONE CLASSIC EVENT from an event packet's bytes - code, length, parameters - or `None` for an event this module does
/// not read, or one whose length disagrees with what arrived or whose fields run past it.
pub fn decode(packet: &[u8]) -> Option<Event> {
	let (&code, rest) = packet.split_first()?;
	let (&len, p) = rest.split_first()?;
	if p.len() != usize::from(len) {
		return None;
	}
	let status = || p.first().copied();
	match code {
		event::INQUIRY_COMPLETE => Some(Event::InquiryComplete { status: status()? }),
		event::INQUIRY_RESULT | event::INQUIRY_RESULT_WITH_RSSI => {
			// FOURTEEN BYTES PER DEVICE, read device by device as controllers send them: the address, the page scan
			// repetition mode, then reserved bytes, the class and the clock offset - and for the RSSI form one reserved
			// byte fewer and the signal strength last. With one response, which is what inquiry sends, the
			// specification's field-by-field layout is the same bytes.
			let count = usize::from(*p.first()?);
			let fields = &p[1..];
			if fields.len() != count * 14 {
				return None;
			}
			let mut found = Vec::new();
			for device in fields.chunks_exact(14) {
				let (class, clock, rssi) = if code == event::INQUIRY_RESULT { (class_at(device, 9)?, u16_at(device, 12)?, None) } else { (class_at(device, 8)?, u16_at(device, 11)?, Some(*device.get(13)? as i8)) };
				found.push(Found { address: address(device, 0)?, repetition: device[6], class, clock_offset: clock, rssi, eir: Eir::default() });
			}
			Some(Event::Inquiry(found))
		}
		event::EXTENDED_INQUIRY_RESULT => {
			if *p.first()? != 1 || p.len() < 255 {
				return None;
			}
			let found = Found { address: address(p, 1)?, repetition: *p.get(7)?, class: class_at(p, 9)?, clock_offset: u16_at(p, 12)?, rssi: Some(*p.get(14)? as i8), eir: parse_eir(&p[15..255]) };
			Some(Event::Inquiry(alloc::vec![found]))
		}
		event::CONNECTION_COMPLETE => Some(Event::ConnectionComplete { status: status()?, handle: u16_at(p, 1)? & 0x0FFF, address: address(p, 3)?, acl: *p.get(9)? == 1, encrypted: *p.get(10)? != 0 }),
		event::CONNECTION_REQUEST => Some(Event::ConnectionRequest { address: address(p, 0)?, class: class_at(p, 6)?, link: *p.get(9)? }),
		event::AUTHENTICATION_COMPLETE => Some(Event::AuthenticationComplete { status: status()?, handle: u16_at(p, 1)? & 0x0FFF }),
		event::REMOTE_NAME_REQUEST_COMPLETE => {
			let name = p.get(7..)?;
			let end = name.iter().position(|byte| *byte == 0).unwrap_or(name.len());
			Some(Event::RemoteName { status: status()?, address: address(p, 1)?, name: name[..end].to_vec() })
		}
		event::ENCRYPTION_CHANGE => Some(Event::EncryptionChange { status: status()?, handle: u16_at(p, 1)? & 0x0FFF, enabled: *p.get(3)? }),
		event::READ_REMOTE_SUPPORTED_FEATURES_COMPLETE => Some(Event::RemoteFeatures { status: status()?, handle: u16_at(p, 1)? & 0x0FFF, features: p.get(3..11)?.try_into().ok()? }),
		event::READ_REMOTE_EXTENDED_FEATURES_COMPLETE => Some(Event::RemoteExtendedFeatures { status: status()?, handle: u16_at(p, 1)? & 0x0FFF, page: *p.get(3)?, features: p.get(5..13)?.try_into().ok()? }),
		event::ROLE_CHANGE => Some(Event::RoleChange { status: status()?, address: address(p, 1)?, central: *p.get(7)? == 0 }),
		event::MODE_CHANGE => Some(Event::ModeChange { status: status()?, handle: u16_at(p, 1)? & 0x0FFF, mode: *p.get(3)?, interval: u16_at(p, 4)? }),
		event::PIN_CODE_REQUEST => Some(Event::PinCodeRequest { address: address(p, 0)? }),
		event::LINK_KEY_REQUEST => Some(Event::LinkKeyRequest { address: address(p, 0)? }),
		event::LINK_KEY_NOTIFICATION => Some(Event::LinkKeyNotification { address: address(p, 0)?, key: p.get(6..22)?.try_into().ok()?, kind: KeyType::from_value(*p.get(22)?) }),
		event::SYNCHRONOUS_CONNECTION_COMPLETE => Some(Event::SynchronousComplete { status: status()?, handle: u16_at(p, 1)? & 0x0FFF, address: address(p, 3)?, esco: *p.get(9)? == 2, air_mode: *p.get(16)? }),
		event::ENCRYPTION_KEY_REFRESH_COMPLETE => Some(Event::EncryptionKeyRefresh { status: status()?, handle: u16_at(p, 1)? & 0x0FFF }),
		event::IO_CAPABILITY_REQUEST => Some(Event::IoCapabilityRequest { address: address(p, 0)? }),
		event::IO_CAPABILITY_RESPONSE => Some(Event::IoCapabilityResponse { address: address(p, 0)?, capability: *p.get(6)?, authentication: *p.get(8)? }),
		event::USER_CONFIRMATION_REQUEST => Some(Event::UserConfirmationRequest { address: address(p, 0)?, value: u32_at(p, 6)? }),
		event::USER_PASSKEY_REQUEST => Some(Event::UserPasskeyRequest { address: address(p, 0)? }),
		event::SIMPLE_PAIRING_COMPLETE => Some(Event::SimplePairingComplete { status: status()?, address: address(p, 1)? }),
		event::USER_PASSKEY_NOTIFICATION => Some(Event::UserPasskeyNotification { address: address(p, 0)?, passkey: u32_at(p, 6)? }),
		event::KEYPRESS_NOTIFICATION => Some(Event::KeypressNotification { address: address(p, 0)?, kind: *p.get(6)? }),
		_ => None,
	}
}

/// The LMP feature bits this host reads from a peer's page 0 and page 1: Secure Simple Pairing on the controller, the
/// host's Secure Simple Pairing support and Secure Connections, sniff mode, role switch and eSCO.
pub mod feature {
	/// Page 0 byte 0 bit 5: role switch. Byte 0 bit 7: sniff mode. Byte 3 bit 7: extended SCO link (EV3). Byte 6 bit 3:
	/// Secure Simple Pairing (controller).
	pub const fn role_switch(page0: &[u8; 8]) -> bool {
		page0[0] & (1 << 5) != 0
	}

	pub const fn sniff(page0: &[u8; 8]) -> bool {
		page0[0] & (1 << 7) != 0
	}

	pub const fn esco(page0: &[u8; 8]) -> bool {
		page0[3] & (1 << 7) != 0
	}

	pub const fn simple_pairing(page0: &[u8; 8]) -> bool {
		page0[6] & (1 << 3) != 0
	}

	/// Page 1 byte 0 bit 0: the host's Secure Simple Pairing; bit 3: the host's Secure Connections.
	pub const fn host_simple_pairing(page1: &[u8; 8]) -> bool {
		page1[0] & 1 != 0
	}

	pub const fn host_secure_connections(page1: &[u8; 8]) -> bool {
		page1[0] & (1 << 3) != 0
	}
}
