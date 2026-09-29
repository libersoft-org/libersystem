// typec - the USB Type-C connectors the system's transports report: each connector's partner and roles, the Power
// Delivery contract it made and the partner's offers, its cable, its alternate modes, and the last request refused on
// it.
//
//   typec                every connector TypeCService holds
//
// IT HOLDS THE READ GRANT ONLY. A role swap or an alternate mode is the operator's authority, which no shipping program
// holds. A connector whose transport is not answering is said to be, with what was last read.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use proto::generated::liber::typec::v1::{ConnectorSnapshot, DataRole, OperationMode, PartnerKind, PowerRole, Refusal, RequestKind, Transport, TypecCurrent};
use proto::system::LaunchContext;
use rt::*;
use typec_client::TypecClient;

fn out(line: &str) {
	print(line.as_bytes());
	print(b"\n");
}

fn fail(line: &str) -> ! {
	eprint(b"typec: ");
	eprint(line.as_bytes());
	eprint(b"\n");
	exit();
}

// Millivolts or milliamps as a decimal of the whole unit.
fn milli(value: u32) -> String {
	format!("{}.{:03}", value / 1000, value % 1000)
}

// ONE POWER DATA OBJECT, as Power Delivery lays it out: its supply type in bits 31-30, and each type's fields.
fn object(object: u32) -> String {
	match object >> 30 {
		0 => format!("fixed {} V {} A", milli(((object >> 10) & 0x3FF) * 50), milli((object & 0x3FF) * 10)),
		1 => format!("battery {}-{} V {} W", milli(((object >> 10) & 0x3FF) * 50), milli(((object >> 20) & 0x3FF) * 50), milli((object & 0x3FF) * 250)),
		2 => format!("variable {}-{} V {} A", milli(((object >> 10) & 0x3FF) * 50), milli(((object >> 20) & 0x3FF) * 50), milli((object & 0x3FF) * 10)),
		_ => format!("augmented {object:#010x}"),
	}
}

fn partner(kind: PartnerKind) -> &'static str {
	match kind {
		PartnerKind::None => "none",
		PartnerKind::Charger => "a charger",
		PartnerKind::Device => "a device",
		PartnerKind::Host => "a host",
		PartnerKind::PoweredCable => "a powered cable",
		PartnerKind::AudioAccessory => "an audio accessory",
		PartnerKind::DebugAccessory => "a debug accessory",
	}
}

fn mode(mode: OperationMode, current: TypecCurrent) -> &'static str {
	match (mode, current) {
		(OperationMode::PowerDelivery, _) => "Power Delivery",
		(OperationMode::Typec3a, _) | (_, TypecCurrent::High) => "Type-C current, 3 A",
		(OperationMode::Typec15a, _) | (_, TypecCurrent::Medium) => "Type-C current, 1.5 A",
		(OperationMode::Bc12, _) => "Battery Charging 1.2",
		(OperationMode::DefaultUsb, _) | (_, TypecCurrent::Default) => "default USB power",
		_ => "not known",
	}
}

fn request(kind: RequestKind) -> &'static str {
	match kind {
		RequestKind::DataRoleSwap => "a data-role swap",
		RequestKind::PowerRoleSwap => "a power-role swap",
		RequestKind::EnterMode => "entering a mode",
		RequestKind::ExitMode => "leaving a mode",
	}
}

fn refusal(reason: Refusal) -> &'static str {
	match reason {
		Refusal::ConnectorCannot => "the connector cannot",
		Refusal::NotOffered => "the platform does not offer it",
		Refusal::PlatformEntersModes => "the platform enters modes itself",
		Refusal::RefusedByPlatform => "the platform refused it",
		Refusal::TransportDoesNot => "the transport does not do it",
		Refusal::Busy => "another request was outstanding",
	}
}

fn print_connector(snapshot: &ConnectorSnapshot) {
	let c = &snapshot.connector;
	let caps = &c.capabilities;
	let mut can: Vec<&str> = Vec::new();
	for (has, word) in [(caps.sink, "sink"), (caps.source, "source"), (caps.data_device, "device"), (caps.data_host, "host"), (caps.power_delivery, "Power Delivery")] {
		if has {
			can.push(word);
		}
	}
	let transport = match caps.transport {
		Transport::Ucsi => format!("UCSI {}.{}", caps.ucsi_version >> 8, (caps.ucsi_version >> 4) & 0xF),
		Transport::Tcpci => String::from("a port controller"),
	};
	out(&format!("connector {} of publication {}.{} - {transport}; it can: {}{}", c.number, snapshot.id.slot, snapshot.id.generation, can.join(", "), if caps.alternate_mode_override { "; the system may choose its modes" } else { "" }));
	if !c.answering {
		out("  NOT ANSWERING - what follows is what was last read");
	}
	let orientation = match c.orientation {
		proto::generated::liber::typec::v1::Orientation::Normal => ", plugged normally",
		proto::generated::liber::typec::v1::Orientation::Flipped => ", plugged flipped",
		_ => "",
	};
	out(&format!("  partner: {}{orientation}", partner(c.partner)));
	let power = match c.power_role {
		Some(PowerRole::Sink) => "sink",
		Some(PowerRole::Source) => "source",
		None => "-",
	};
	let data = match c.data_role {
		Some(DataRole::Device) => "device",
		Some(DataRole::Host) => "host",
		None => "-",
	};
	out(&format!("  power role {power}, data role {data}, power by {}", mode(c.operation_mode, c.typec_current)));
	if let Some(contract) = &c.contract {
		out(&format!("  contract: offer {} ({}), operating {} mA, maximum {} mA{}{}", contract.offer.position, object(contract.offer.object), contract.operating, contract.maximum, if contract.capability_mismatch { ", capability mismatch" } else { "" }, if contract.in_transition { " - in transition" } else { "" }));
	}
	for offer in c.offers.iter() {
		out(&format!("  offer {}: {}", offer.position, object(offer.object)));
	}
	if let Some(cable) = &c.cable {
		out(&format!("  cable: {} mA, {}{}{}, speeds {:#06x}", cable.current, if cable.active { "active" } else { "passive" }, if cable.vbus { ", carries VBUS" } else { "" }, if cable.directional { ", directional" } else { "" }, cable.speed));
	}
	for m in c.modes.iter() {
		out(&format!("  mode {:#06x} vdo {:#010x}{}", m.svid, m.vdo, if m.entered { " - entered" } else { "" }));
	}
	if let Some(dp) = &c.displayport {
		let pins = match dp.pin_assignment {
			0x04 => "C",
			0x08 => "D",
			0x10 => "E",
			0 => "not reported",
			_ => "other",
		};
		let hpd = match dp.hot_plug {
			Some(true) => "high",
			Some(false) => "low",
			None => "not reported",
		};
		out(&format!("  DisplayPort: pin assignment {pins}, hot-plug {hpd}"));
	}
	if let Some(last) = &c.last_refusal {
		out(&format!("  last refused: {} - {}{}", request(last.kind), refusal(last.reason), if last.error != 0 { format!(" (error {:#x})", last.error) } else { String::new() }));
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let service = recv_tagged(bootstrap, &mut buf, b"TYPEC").unwrap_or(0);
	if service == 0 {
		fail("this launch holds no connection to TypeCService");
	}
	if !context.arguments.trim().is_empty() {
		fail("usage: typec");
	}
	let connectors = match TypecClient::new(service).connectors() {
		Some(Ok(connectors)) => connectors,
		other => fail(&format!("TypeCService did not answer - {other:?}")),
	};
	if connectors.is_empty() {
		out("typec: this machine reports no Type-C connector");
	}
	for connector in &connectors {
		print_connector(connector);
	}
	exit();
}
