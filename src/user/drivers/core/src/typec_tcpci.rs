//! A PORT CONTROLLER'S CONNECTOR, as the `tcpci` driver holds it: the board's description of the connector read from
//! the controller's property block, and the `liber:typec@1` record and the `usb-c` source built from what the sink
//! engine reports.
//!
//! THE DESCRIPTION is the `usb-c-connector` child - a device tree's `connector` node, or `_DSD`'s hierarchical data node
//! of that name - with `power-role`, `sink-pdos` and `op-sink-microwatt`, the properties Linux's binding names. A tree
//! carries each value as its raw cells and strings; a namespace device's block carries each in the node channel's value
//! encoding. A connector described as a SOURCE or DUAL-ROLE is refused (nothing here can source); a sink whose
//! description is missing or unreadable runs no Power Delivery and sinks at the Type-C current only. A sink PDO of the
//! augmented kind is not one this sink ever requests, so it is left out of what the engine is given.
//!
//! WHAT THE PARTNER IS, without Power Delivery, is a supply: nothing on CC says whether it offers USB data. With Power
//! Delivery, its first offer's USB Communications Capable bit says.

use alloc::vec::Vec;
use power_model::usbc::{Reading, UsbC};
use proto::generated::liber::typec::v1 as typec;
use typec::{Capabilities, Connector, Contract, DataRole, Offer, OperationMode, Orientation, PartnerKind, PowerRole, RefusedRequest, Transport, TypecCurrent};
use usb_pd::engine::{Report, Rp};
use usb_pd::pdo::{Sink, SinkPdo};

/// The most sink PDOs a Sink_Capabilities message carries.
pub const MAX_SINK_PDOS: usize = 7;
/// The only connector a port controller has.
pub const CONNECTOR: u8 = 1;

/// What the board says of the connector.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Described {
	/// A sink the engine runs Power Delivery for.
	Sink(Sink),
	/// No description, or one that could not be read: the Type-C current only, and why.
	TypeCOnly(&'static str),
	/// Described as something that sources: refused at bind, naming the role.
	Refused(&'static str),
}

// ONE RECORD of a property block: its kind, its depth, its name and its value.
struct Record<'a> {
	kind: u8,
	depth: u8,
	name: &'a [u8],
	value: &'a [u8],
}

fn records(block: &[u8]) -> Vec<Record<'_>> {
	let mut out = Vec::new();
	let mut at = 0usize;
	while at + 8 <= block.len() {
		let name_len = usize::from(u16::from_le_bytes([block[at + 2], block[at + 3]]));
		let value_len = u32::from_le_bytes([block[at + 4], block[at + 5], block[at + 6], block[at + 7]]) as usize;
		let name_at = at + 8;
		let value_at = name_at + name_len;
		let Some(end) = value_at.checked_add(value_len).filter(|end| *end <= block.len()) else { break };
		out.push(Record { kind: block[at], depth: block[at + 1], name: &block[name_at..value_at], value: &block[value_at..end] });
		at = value_at + ((value_len + 3) & !3);
	}
	out
}

// A value as the description reads it: text, or a list of numbers.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Value {
	Text(Vec<Vec<u8>>),
	Numbers(Vec<u64>),
}

// A TREE'S VALUE: a string list when every byte before each NUL is printable, otherwise big-endian cells. A property
// the description reads is one or the other by its name, so the ambiguity of an empty value never arises.
fn tree_value(raw: &[u8], text: bool) -> Option<Value> {
	if text {
		if raw.last() != Some(&0) {
			return None;
		}
		return Some(Value::Text(raw[..raw.len() - 1].split(|byte| *byte == 0).map(Vec::from).collect()));
	}
	if raw.is_empty() || raw.len() % 4 != 0 {
		return None;
	}
	Some(Value::Numbers(raw.chunks(4).map(|cell| u64::from(u32::from_be_bytes([cell[0], cell[1], cell[2], cell[3]]))).collect()))
}

// A NAMESPACE DEVICE'S VALUE: a string, an integer, or a package of either.
fn acpi_value(raw: &[u8]) -> Option<Value> {
	match aml::wire::decode(raw).ok()? {
		aml::wire::Value::String(text) => Some(Value::Text(alloc::vec![text.into_bytes()])),
		aml::wire::Value::Integer(value) => Some(Value::Numbers(alloc::vec![value])),
		aml::wire::Value::Package(elements) => {
			if elements.iter().all(|element| matches!(element, aml::wire::Value::Integer(_))) {
				Some(Value::Numbers(elements.into_iter().filter_map(|element| if let aml::wire::Value::Integer(value) = element { Some(value) } else { None }).collect()))
			} else if elements.iter().all(|element| matches!(element, aml::wire::Value::String(_))) {
				Some(Value::Text(elements.into_iter().filter_map(|element| if let aml::wire::Value::String(text) = element { Some(text.into_bytes()) } else { None }).collect()))
			} else {
				None
			}
		}
		_ => None,
	}
}

// THE CONNECTOR NODE'S PROPERTIES: the first child at depth 1 named `connector` (a tree may give it a unit address) or
// whose `compatible` is `usb-c-connector`. Each as (name, raw value).
fn connector_properties<'a>(block: &'a [u8], acpi: bool) -> Option<Vec<(&'a [u8], &'a [u8])>> {
	let all = records(block);
	let mut at = 0;
	while at < all.len() {
		let record = &all[at];
		at += 1;
		if record.kind != rt::DEVICE_PROPERTY_NODE || record.depth != 1 {
			continue;
		}
		let named = record.name == b"connector" || record.name.starts_with(b"connector@");
		let mut properties = Vec::new();
		// Its own values: the records at depth 1 until the next node at depth 1 or above.
		while at < all.len() && !(all[at].kind == rt::DEVICE_PROPERTY_NODE && all[at].depth <= 1) {
			if all[at].kind == rt::DEVICE_PROPERTY_VALUE && all[at].depth == 1 {
				properties.push((all[at].name, all[at].value));
			}
			at += 1;
		}
		let compatible = properties.iter().find(|(name, _)| *name == b"compatible").and_then(|(_, raw)| if acpi { acpi_value(raw) } else { tree_value(raw, true) });
		let usb_c = matches!(&compatible, Some(Value::Text(texts)) if texts.iter().any(|text| text == b"usb-c-connector"));
		if named || usb_c {
			return Some(properties);
		}
	}
	None
}

/// A sink PDO as the board encodes it - the same object Sink_Capabilities carries. `None` for an augmented one.
pub fn sink_pdo(object: u32) -> Option<SinkPdo> {
	let upper = ((object >> 20) & 0x3FF) * 50;
	let lower = ((object >> 10) & 0x3FF) * 50;
	match object >> 30 {
		0 => Some(SinkPdo::Fixed { millivolts: lower, milliamps: (object & 0x3FF) * 10 }),
		1 => Some(SinkPdo::Battery { min_millivolts: lower, max_millivolts: upper, milliwatts: (object & 0x3FF) * 250 }),
		2 => Some(SinkPdo::Variable { min_millivolts: lower, max_millivolts: upper, milliamps: (object & 0x3FF) * 10 }),
		_ => None,
	}
}

/// THE BOARD'S DESCRIPTION of the connector, from the controller's property block.
pub fn describe(block: &[u8], acpi: bool) -> Described {
	let Some(properties) = connector_properties(block, acpi) else { return Described::TypeCOnly("the board describes no usb-c-connector") };
	let value = |key: &[u8], text: bool| properties.iter().find(|(name, _)| *name == key).and_then(|(_, raw)| if acpi { acpi_value(raw) } else { tree_value(raw, text) });
	let role = match value(b"power-role", true) {
		Some(Value::Text(texts)) if texts.len() == 1 => texts[0].clone(),
		_ => return Described::TypeCOnly("its connector names no power-role"),
	};
	match role.as_slice() {
		b"sink" => {}
		b"source" => return Described::Refused("source"),
		b"dual" => return Described::Refused("dual"),
		_ => return Described::TypeCOnly("its connector names a power-role that is none of sink, source and dual"),
	}
	let objects = match value(b"sink-pdos", false) {
		Some(Value::Numbers(objects)) if !objects.is_empty() && objects.len() <= MAX_SINK_PDOS && objects.iter().all(|object| *object <= u64::from(u32::MAX)) => objects,
		_ => return Described::TypeCOnly("its connector's sink-pdos are missing or unreadable"),
	};
	let operational_microwatts = match value(b"op-sink-microwatt", false) {
		Some(Value::Numbers(values)) if values.len() == 1 => values[0],
		_ => return Described::TypeCOnly("its connector's op-sink-microwatt is missing or unreadable"),
	};
	// THE FIRST IS vSafe5V, FIXED - as Power Delivery requires of every sink's capabilities.
	if !matches!(sink_pdo(objects[0] as u32), Some(SinkPdo::Fixed { millivolts: 5_000, .. })) {
		return Described::TypeCOnly("its connector's first sink PDO is not a fixed 5 V one");
	}
	Described::Sink(Sink { pdos: objects.iter().filter_map(|object| sink_pdo(*object as u32)).collect(), operational_microwatts })
}

fn operation_mode(rp: Rp) -> OperationMode {
	match rp {
		Rp::Default => OperationMode::DefaultUsb,
		Rp::Medium => OperationMode::Typec15a,
		Rp::High => OperationMode::Typec3a,
	}
}

fn typec_current(rp: Option<Rp>) -> TypecCurrent {
	match rp {
		None => TypecCurrent::None,
		Some(Rp::Default) => TypecCurrent::Default,
		Some(Rp::Medium) => TypecCurrent::Medium,
		Some(Rp::High) => TypecCurrent::High,
	}
}

/// What the partner is: with Power Delivery, its first offer's USB Communications Capable bit (26) says whether it
/// offers data; without, it is a supply.
pub fn partner(report: &Report) -> PartnerKind {
	if !report.attached {
		return PartnerKind::None;
	}
	match report.offers.first() {
		Some(first) if report.pd && first >> 30 == 0 && first & (1 << 26) != 0 => PartnerKind::Host,
		_ => PartnerKind::Charger,
	}
}

/// THE CONNECTOR'S RECORD for TypeCService.
pub fn record(report: &Report, orientation: Option<bool>, runs_pd: bool, last_refusal: Option<RefusedRequest>) -> Connector {
	let attached = report.attached;
	let contract = report.contract.map(|(selection, object)| Contract { offer: Offer { position: selection.position, object }, operating: selection.milliamps, maximum: selection.milliamps, capability_mismatch: selection.mismatch, in_transition: report.in_transition });
	Connector {
		number: CONNECTOR,
		capabilities: Capabilities { transport: Transport::Tcpci, sink: true, source: false, data_device: true, data_host: false, power_delivery: runs_pd, ucsi_version: 0, ucsi_features: 0, alternate_mode_override: false },
		partner: partner(report),
		orientation: match (attached, orientation) {
			(true, Some(true)) => Orientation::Flipped,
			(true, Some(false)) => Orientation::Normal,
			_ => Orientation::Unknown,
		},
		power_role: attached.then_some(PowerRole::Sink),
		data_role: attached.then_some(DataRole::Device),
		operation_mode: match (attached, report.pd, report.typec_current) {
			(false, _, _) => OperationMode::Unknown,
			(true, true, _) => OperationMode::PowerDelivery,
			(true, false, Some(rp)) => operation_mode(rp),
			(true, false, None) => OperationMode::Unknown,
		},
		contract: if attached { contract } else { None },
		typec_current: typec_current(report.typec_current),
		offers: if attached { report.offers.iter().take(crate::typec_ucsi::MAX_OFFERS).enumerate().map(|(at, object)| Offer { position: at as u8 + 1, object: *object }).collect() } else { Vec::new() },
		cable: None,
		modes: Vec::new(),
		displayport: None,
		last_refusal,
		answering: true,
	}
}

/// THE PARTNER AS A SUPPLY, for PowerService: the controller's VBUS measurement where it has one, its alarm as the
/// fault, and no current - a port controller measures none.
pub fn supply(report: &Report, vbus_millivolts: Option<u32>, measures: bool) -> UsbC {
	let voltage = match (measures, vbus_millivolts) {
		(false, _) => Reading::Unsupported,
		(true, Some(millivolts)) => Reading::Value(u64::from(millivolts) * 1000),
		(true, None) => Reading::Unknown,
	};
	UsbC { attached: Some(report.attached), sinking: report.attached.then_some(report.sinking), voltage, current: Reading::Unsupported, fault: Some(report.fault) }
}

#[cfg(test)]
mod tests;
