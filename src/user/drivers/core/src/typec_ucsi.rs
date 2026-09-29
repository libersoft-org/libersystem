//! A UCSI CONNECTOR, as `ucsi-acpi` holds it: what the policy manager answered about it, the `liber:typec@1` record
//! and the `usb-c` source built from that, and the rules a request is refused by before any command is sent.
//!
//! A FACT WHOSE COMMAND THE PLATFORM DOES NOT OFFER IS REPORTED AS NOT OFFERED, NEVER GUESSED: the offers, the cable,
//! the modes and DisplayPort's hot-plug are read only where `GET_CAPABILITY` advertises their commands, and a
//! connector's record carries none of them otherwise. The data role is the partner's type turned round - a
//! downstream-facing partner makes this machine its device - and the power role the connector's power direction.

use alloc::vec::Vec;
use power_model::usbc::{Reading, UsbC};
use proto::generated::liber::typec::v1 as typec;
use typec::{Cable, Capabilities, Connector, Contract, DataRole, Displayport, Offer, OperationMode, Orientation, PartnerKind, PowerRole, Refusal, RefusedRequest, Transport, TypecCurrent};
use ucsi::answer::{AlternateMode, CableProperty, Capability, ConnectorCapability, ConnectorStatus, PartnerType};

/// DisplayPort's standard ID.
pub const DISPLAYPORT_SVID: u16 = 0xFF01;
/// The most offers a connector's record carries: Power Delivery's seven standard positions and four extended.
pub const MAX_OFFERS: usize = 11;
/// The most alternate modes read of a partner.
pub const MAX_MODES: usize = 16;

/// Everything read of one connector.
#[derive(Clone, PartialEq, Debug)]
pub struct Port {
	pub number: u8,
	pub capability: ConnectorCapability,
	pub status: Option<ConnectorStatus>,
	/// The partner's source offers, in position order.
	pub offers: Vec<u32>,
	pub cable: Option<CableProperty>,
	/// The partner's modes, and the connector's own - which `SET_NEW_CAM`'s offset indexes.
	pub modes: Vec<AlternateMode>,
	pub own_modes: Vec<AlternateMode>,
	/// The connector's current alternate mode, as an offset into `own_modes`.
	pub current: Option<u8>,
	/// DisplayPort's status VDO, from the last attention.
	pub attention: Option<u32>,
	/// The DisplayPort pin assignment this OPM configured when it entered the mode; `None` when the platform entered it,
	/// which says no pin assignment.
	pub pin: Option<u8>,
	pub last_refusal: Option<RefusedRequest>,
	pub answering: bool,
}

impl Port {
	pub fn new(number: u8, capability: ConnectorCapability) -> Port {
		Port { number, capability, status: None, offers: Vec::new(), cable: None, modes: Vec::new(), own_modes: Vec::new(), current: None, attention: None, pin: None, last_refusal: None, answering: true }
	}

	pub fn connected(&self) -> bool {
		self.status.is_some_and(|status| status.connected)
	}

	/// Whether this connector can take power at all: only such a connector is a `usb-c` source.
	pub fn sinks(&self) -> bool {
		self.capability.consumer
	}

	/// What the partner is, the machine's own power role being `source`.
	pub fn partner(&self) -> PartnerKind {
		let Some(status) = self.status.filter(|status| status.connected) else { return PartnerKind::None };
		match status.partner {
			PartnerType::None => PartnerKind::None,
			// A downstream-facing partner that supplies this machine and offers no USB data is a charger.
			PartnerType::Dfp if !status.source && !status.partner_usb() => PartnerKind::Charger,
			PartnerType::Dfp => PartnerKind::Host,
			PartnerType::Ufp => PartnerKind::Device,
			PartnerType::PoweredCable | PartnerType::PoweredCableWithUfp => PartnerKind::PoweredCable,
			PartnerType::DebugAccessory => PartnerKind::DebugAccessory,
			PartnerType::AudioAccessory => PartnerKind::AudioAccessory,
		}
	}

	pub fn data_role(&self) -> Option<DataRole> {
		let status = self.status.filter(|status| status.connected)?;
		match status.partner {
			PartnerType::Dfp => Some(DataRole::Device),
			PartnerType::Ufp | PartnerType::PoweredCableWithUfp => Some(DataRole::Host),
			_ => None,
		}
	}

	pub fn power_role(&self) -> Option<PowerRole> {
		let status = self.status.filter(|status| status.connected)?;
		Some(if status.source { PowerRole::Source } else { PowerRole::Sink })
	}

	/// The mode entered, as the partner's: the connector's current mode's SVID, if the partner offers it.
	pub fn entered_svid(&self) -> Option<u16> {
		let current = self.own_modes.get(usize::from(self.current?))?;
		self.modes.iter().any(|mode| mode.svid == current.svid).then_some(current.svid)
	}
}

fn operation_mode(mode: u8) -> OperationMode {
	match mode {
		1 => OperationMode::DefaultUsb,
		2 => OperationMode::Bc12,
		3 => OperationMode::PowerDelivery,
		4 => OperationMode::Typec15a,
		5 => OperationMode::Typec3a,
		_ => OperationMode::Unknown,
	}
}

fn typec_current(mode: u8) -> TypecCurrent {
	match mode {
		1 => TypecCurrent::Default,
		4 => TypecCurrent::Medium,
		5 => TypecCurrent::High,
		_ => TypecCurrent::None,
	}
}

/// THE CONTRACT, from the request data object and the offer it names: a fixed or variable request's operating and
/// maximum current in 10 mA units (bits 19-10 and 9-0), a battery request's power in 250 mW units, and its mismatch
/// flag (bit 26). None without Power Delivery, without a position, or naming an offer the partner did not make.
pub fn contract(status: &ConnectorStatus, offers: &[u32]) -> Option<Contract> {
	if status.operation_mode != 3 || !status.connected {
		return None;
	}
	let position = status.position();
	let object = *offers.get(usize::from(position).checked_sub(1)?)?;
	let rdo = status.rdo;
	let (operating, maximum) = if object >> 30 == 1 { (((rdo >> 10) & 0x3FF) * 250, (rdo & 0x3FF) * 250) } else { (((rdo >> 10) & 0x3FF) * 10, (rdo & 0x3FF) * 10) };
	Some(Contract { offer: Offer { position, object }, operating, maximum, capability_mismatch: rdo & (1 << 26) != 0, in_transition: false })
}

/// THE CONNECTOR'S RECORD for TypeCService.
pub fn record(port: &Port, capability: &Capability, version: u16) -> Connector {
	let status = port.status.filter(|status| status.connected);
	let capabilities = Capabilities { transport: Transport::Ucsi, sink: port.capability.consumer, source: port.capability.provider, data_device: port.capability.device(), data_host: port.capability.host(), power_delivery: capability.attributes & Capability::ATTRIBUTE_PD != 0, ucsi_version: version, ucsi_features: capability.features, alternate_mode_override: capability.has(Capability::ALT_MODE_OVERRIDE) };
	let entered = port.entered_svid();
	let modes = port.modes.iter().take(MAX_MODES).map(|mode| typec::AlternateMode { svid: mode.svid, vdo: mode.vdo, entered: entered == Some(mode.svid) }).collect();
	let displayport = (entered == Some(DISPLAYPORT_SVID)).then(|| Displayport { pin_assignment: port.pin.unwrap_or(0), hot_plug: port.attention.map(ucsi::answer::dp_hot_plug) });
	Connector {
		number: port.number,
		capabilities,
		partner: port.partner(),
		orientation: match status.and_then(|status| status.orientation) {
			Some(true) => Orientation::Flipped,
			Some(false) => Orientation::Normal,
			None => Orientation::Unknown,
		},
		power_role: port.power_role(),
		data_role: port.data_role(),
		operation_mode: status.map_or(OperationMode::Unknown, |status| operation_mode(status.operation_mode)),
		contract: status.and_then(|status| contract(&status, &port.offers)),
		typec_current: status.map_or(TypecCurrent::None, |status| typec_current(status.operation_mode)),
		offers: if status.is_some() { port.offers.iter().take(MAX_OFFERS).enumerate().map(|(at, object)| Offer { position: at as u8 + 1, object: *object }).collect() } else { Vec::new() },
		cable: if status.is_some() { port.cable.map(|cable| Cable { speed: cable.speed, current: cable.current, vbus: cable.vbus, active: cable.active, directional: cable.directional, plug_end: cable.plug_end, alternate_modes: cable.alternate_modes, pd_revision: cable.pd_revision }) } else { None },
		modes: if status.is_some() { modes } else { Vec::new() },
		displayport: if status.is_some() { displayport } else { None },
		last_refusal: port.last_refusal.clone(),
		answering: port.answering,
	}
}

/// THE PARTNER AS A SUPPLY, for PowerService: 2.1's readings when the PPM has them, nothing measured before 2.1.
pub fn supply(port: &Port, version: u16) -> UsbC {
	let status = port.status;
	let measures = version >= 0x0210;
	let attached = if port.answering { status.map(|status| status.connected) } else { None };
	let (voltage, current) = match status {
		Some(status) if measures => (status.voltage.map_or(Reading::Unknown, Reading::Value), status.current.map_or(Reading::Unknown, |microamps| Reading::Value(if status.source { microamps as i64 } else { -(microamps as i64) }))),
		_ if measures => (Reading::Unknown, Reading::Unknown),
		_ => (Reading::Unsupported, Reading::Unsupported),
	};
	UsbC { attached, sinking: status.filter(|status| status.connected && port.answering).map(|status| !status.source), voltage, current, fault: None }
}

/// DisplayPort's pin assignments a partner's mode object allows - a receptacle's in bits 23-16, a plug's in 15-8 - and
/// the one chosen: D first, because it keeps USB 3, then C, then E.
pub fn pin_assignment(vdo: u32) -> Option<u8> {
	let pins = if vdo & (1 << 6) != 0 { (vdo >> 16) & 0xFF } else { (vdo >> 8) & 0xFF };
	[0x08u8, 0x04, 0x10].into_iter().find(|pin| pins & u32::from(*pin) != 0)
}

/// DisplayPort's configuration for `SET_NEW_CAM`: the pin assignment chosen, DisplayPort signalling, and this machine
/// as the source of the display (UFP_U as DFP_D on the partner's side).
pub fn dp_configuration(pin: u8) -> u32 {
	u32::from(pin) << 8 | 1 << 2 | 1
}

/// WHY A ROLE SWAP IS REFUSED BEFORE ANY COMMAND: the connector cannot take the role asked for.
pub fn data_swap_refusal(port: &Port, role: DataRole) -> Option<Refusal> {
	let can = match role {
		DataRole::Host => port.capability.swap_to_host,
		DataRole::Device => port.capability.swap_to_device,
	};
	(!can || !port.connected()).then_some(Refusal::ConnectorCannot)
}

pub fn power_swap_refusal(port: &Port, role: PowerRole) -> Option<Refusal> {
	let can = match role {
		PowerRole::Source => port.capability.swap_to_source,
		PowerRole::Sink => port.capability.swap_to_sink,
	};
	(!can || !port.connected()).then_some(Refusal::ConnectorCannot)
}

/// A MODE ENTERED OR LEFT, refused before any command: without the platform's override the platform enters modes
/// itself; a mode the connector or the partner does not have cannot be entered. Otherwise the connector's offset for it.
pub fn mode_offset(port: &Port, capability: &Capability, svid: u16) -> Result<u8, Refusal> {
	if !capability.has(Capability::ALT_MODE_OVERRIDE) {
		return Err(Refusal::PlatformEntersModes);
	}
	if !port.connected() || !port.modes.iter().any(|mode| mode.svid == svid) {
		return Err(Refusal::ConnectorCannot);
	}
	port.own_modes.iter().position(|mode| mode.svid == svid).map(|at| at as u8).ok_or(Refusal::ConnectorCannot)
}

#[cfg(test)]
mod tests;
