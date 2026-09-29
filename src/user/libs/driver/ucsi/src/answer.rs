//! THE ANSWERS, decoded from MESSAGE_IN - each checked for the length its structure needs before a field is read, and
//! reserved values refused.

use alloc::vec::Vec;

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
	Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
	Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

// Bits `at..at + width` of a little-endian bit string.
fn bits(data: &[u8], at: usize, width: usize) -> Option<u32> {
	if at + width > data.len() * 8 || width > 32 {
		return None;
	}
	let mut value = 0u64;
	for bit in 0..width {
		let index = at + bit;
		value |= u64::from((data[index / 8] >> (index % 8)) & 1) << bit;
	}
	Some(value as u32)
}

/// GET_CAPABILITY.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capability {
	pub attributes: u32,
	pub connectors: u8,
	/// bmOptionalFeatures, 24 bits.
	pub features: u32,
	pub alternate_modes: u8,
	pub pd_version: u16,
	pub typec_version: u16,
}

impl Capability {
	/// bmOptionalFeatures: SET_CCOM (SET_UOM) is supported.
	pub const SET_CCOM: u32 = 1 << 0;
	pub const ALT_MODE_DETAILS: u32 = 1 << 2;
	pub const ALT_MODE_OVERRIDE: u32 = 1 << 3;
	pub const PDO_DETAILS: u32 = 1 << 4;
	pub const CABLE_DETAILS: u32 = 1 << 5;
	/// bmAttributes: USB Power Delivery.
	pub const ATTRIBUTE_PD: u32 = 1 << 2;

	pub fn decode(data: &[u8]) -> Option<Capability> {
		let attributes = u32_at(data, 0)?;
		let connectors = *data.get(4)?;
		let features = u32::from(*data.get(5)?) | u32::from(*data.get(6)?) << 8 | u32::from(*data.get(7)?) << 16;
		let alternate_modes = *data.get(8)?;
		let pd_version = u16_at(data, 12).unwrap_or(0);
		let typec_version = u16_at(data, 14).unwrap_or(0);
		// A PPM WITH NO CONNECTOR, OR MORE THAN A CONNECTOR NUMBER CAN NAME, describes nothing this OPM can serve.
		if connectors == 0 || connectors > 127 {
			return None;
		}
		Some(Capability { attributes, connectors, features, alternate_modes, pd_version, typec_version })
	}

	pub fn has(&self, feature: u32) -> bool {
		self.features & feature != 0
	}
}

/// GET_CONNECTOR_CAPABILITY.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ConnectorCapability {
	/// bmOperationMode: bit 0 DFP only, 1 UFP only, 2 DRP, 3 audio accessory, 4 debug accessory, 5 USB 2, 6 USB 3,
	/// 7 alternate modes.
	pub operation_mode: u8,
	pub provider: bool,
	pub consumer: bool,
	pub swap_to_host: bool,
	pub swap_to_device: bool,
	pub swap_to_source: bool,
	pub swap_to_sink: bool,
}

impl ConnectorCapability {
	pub fn decode(data: &[u8]) -> Option<ConnectorCapability> {
		let operation_mode = *data.first()?;
		let flags = *data.get(1)?;
		Some(ConnectorCapability { operation_mode, provider: flags & 1 != 0, consumer: flags & 2 != 0, swap_to_host: flags & 4 != 0, swap_to_device: flags & 8 != 0, swap_to_source: flags & 16 != 0, swap_to_sink: flags & 32 != 0 })
	}

	pub fn host(&self) -> bool {
		self.operation_mode & (1 | 4) != 0
	}

	pub fn device(&self) -> bool {
		self.operation_mode & (2 | 4) != 0
	}
}

/// What a connector's partner is, as CONNECTOR STATUS says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PartnerType {
	None,
	/// A downstream-facing port: this machine is its device.
	Dfp,
	/// An upstream-facing port: this machine is its host.
	Ufp,
	PoweredCable,
	PoweredCableWithUfp,
	DebugAccessory,
	AudioAccessory,
}

/// GET_CONNECTOR_STATUS.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ConnectorStatus {
	pub change: u16,
	/// 1 default USB, 2 Battery Charging, 3 Power Delivery, 4 Type-C 1.5 A, 5 Type-C 3 A.
	pub operation_mode: u8,
	pub connected: bool,
	/// This machine is the source.
	pub source: bool,
	pub partner_flags: u8,
	pub partner: PartnerType,
	/// The request data object of the contract, when there is one.
	pub rdo: u32,
	/// From 2.0: the plug is flipped.
	pub orientation: Option<bool>,
	/// From 2.1: VBUS in microvolts and the average current in microamps, when the reading is ready.
	pub voltage: Option<u64>,
	pub current: Option<u64>,
}

impl ConnectorStatus {
	pub const CHANGE_CONNECT: u16 = 1 << 14;

	/// Decoded as the layout of `version` has it: 1.x's nine bytes, and from 2.0 the orientation, from 2.1 the power
	/// readings.
	pub fn decode(data: &[u8], version: u16) -> Option<ConnectorStatus> {
		let change = u16_at(data, 0)?;
		let flags = u16_at(data, 2)?;
		let operation_mode = (flags & 0x7) as u8;
		if operation_mode > 5 {
			return None;
		}
		let connected = flags & (1 << 3) != 0;
		let partner = match (flags >> 13) & 0x7 {
			0 => PartnerType::None,
			1 => PartnerType::Dfp,
			2 => PartnerType::Ufp,
			3 => PartnerType::PoweredCable,
			4 => PartnerType::PoweredCableWithUfp,
			5 => PartnerType::DebugAccessory,
			6 => PartnerType::AudioAccessory,
			_ => return None,
		};
		let rdo = u32_at(data, 4)?;
		let orientation = if version >= 0x0200 { Some(bits(data, 86, 1)? != 0) } else { None };
		let (voltage, current) = if version >= 0x0210 && bits(data, 89, 1)? != 0 {
			let current_scale = u64::from(bits(data, 90, 3)?);
			let average = u64::from(bits(data, 109, 16)?);
			let voltage_scale = u64::from(bits(data, 125, 4)?);
			let reading = u64::from(bits(data, 129, 16)?);
			// Five millivolts and five milliamps a unit of scale.
			(Some(reading * voltage_scale * 5_000), Some(average * current_scale * 5_000))
		} else {
			(None, None)
		};
		Some(ConnectorStatus { change, operation_mode, connected, source: flags & (1 << 4) != 0, partner_flags: ((flags >> 5) & 0xFF) as u8, partner, rdo, orientation, voltage, current })
	}

	/// The contract's offer position, from the request data object: bits 31-28, 0 for none.
	pub fn position(&self) -> u8 {
		(self.rdo >> 28) as u8 & 0xF
	}

	/// The partner offers USB data (partner flag bit 0).
	pub fn partner_usb(&self) -> bool {
		self.partner_flags & 1 != 0
	}

	/// The partner is in an alternate mode (partner flag bit 1).
	pub fn partner_alt_mode(&self) -> bool {
		self.partner_flags & 2 != 0
	}
}

/// GET_CABLE_PROPERTY.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CableProperty {
	pub speed: u16,
	/// Milliamps.
	pub current: u16,
	pub vbus: bool,
	pub active: bool,
	pub directional: bool,
	pub plug_end: u8,
	pub alternate_modes: bool,
	pub pd_revision: u8,
}

impl CableProperty {
	pub fn decode(data: &[u8]) -> Option<CableProperty> {
		let speed = u16_at(data, 0)?;
		let current = u16::from(*data.get(2)?) * 50;
		let flags = *data.get(3)?;
		// The fifth byte is the latency, which nothing here reports; the answer still has to carry it.
		data.get(4)?;
		Some(CableProperty { speed, current, vbus: flags & 1 != 0, active: flags & 2 != 0, directional: flags & 4 != 0, plug_end: (flags >> 3) & 0x3, alternate_modes: flags & 0x20 != 0, pd_revision: (flags >> 6) & 0x3 })
	}
}

/// One alternate mode: its SVID and its mode object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AlternateMode {
	pub svid: u16,
	pub vdo: u32,
}

/// GET_ALTERNATE_MODES: one or two modes, six bytes each; an SVID of zero ends the list.
pub fn alternate_modes(data: &[u8]) -> Option<Vec<AlternateMode>> {
	if data.len() % 6 != 0 {
		return None;
	}
	let mut modes = Vec::new();
	for at in (0..data.len()).step_by(6) {
		let svid = u16_at(data, at)?;
		if svid == 0 {
			break;
		}
		modes.push(AlternateMode { svid, vdo: u32_at(data, at + 2)? });
	}
	Some(modes)
}

/// GET_PDOS: whole power data objects, four bytes each.
pub fn pdos(data: &[u8]) -> Option<Vec<u32>> {
	if data.len() % 4 != 0 {
		return None;
	}
	Some(data.chunks_exact(4).map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])).take_while(|object| *object != 0).collect())
}

/// GET_ERROR_STATUS: the error information bits.
pub fn error_status(data: &[u8]) -> Option<u32> {
	match data.len() {
		0 => None,
		1 => Some(u32::from(data[0])),
		_ => Some(u32::from(u16_at(data, 0)?)),
	}
}

/// GET_ATTENTION_VDO: the DisplayPort status the partner last sent.
pub fn attention_vdo(data: &[u8]) -> Option<u32> {
	u32_at(data, 0)
}

/// DisplayPort's status VDO: hot-plug detect is bit 7.
pub fn dp_hot_plug(vdo: u32) -> bool {
	vdo & (1 << 7) != 0
}
