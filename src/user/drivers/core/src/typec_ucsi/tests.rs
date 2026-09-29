use super::*;
use alloc::vec;
use power_model::canon::validate;
use power_model::schema::{Tristate, ValueState};

fn drp() -> ConnectorCapability {
	ConnectorCapability { operation_mode: 0x04 | 0x80, provider: true, consumer: true, swap_to_host: true, swap_to_device: true, swap_to_source: true, swap_to_sink: true }
}

fn capability(features: u32) -> Capability {
	Capability { attributes: Capability::ATTRIBUTE_PD, connectors: 2, features, alternate_modes: 1, pd_version: 0x0300, typec_version: 0x0200 }
}

fn fixed(millivolts: u32, milliamps: u32) -> u32 {
	(millivolts / 50) << 10 | milliamps / 10
}

// A charger on connector 1: PD, sinking from it, a contract at position 2 (9 V at 2 A of 3 A).
fn charger() -> Port {
	let mut port = Port::new(1, drp());
	port.status = Some(ConnectorStatus { change: 0, operation_mode: 3, connected: true, source: false, partner_flags: 0, partner: PartnerType::Dfp, rdo: 2 << 28 | 200 << 10 | 300, orientation: Some(false), voltage: Some(9_020_000), current: Some(1_500_000) });
	port.offers = vec![fixed(5_000, 3_000), fixed(9_000, 3_000), fixed(15_000, 3_000)];
	port
}

#[test]
fn a_charger_is_a_charger_with_its_contract_and_offers_and_an_online_source_measuring_only_what_2_1_reads() {
	let port = charger();
	let connector = record(&port, &capability(Capability::PDO_DETAILS), 0x0210);
	assert_eq!((connector.partner, connector.power_role, connector.data_role, connector.operation_mode), (PartnerKind::Charger, Some(PowerRole::Sink), Some(DataRole::Device), OperationMode::PowerDelivery));
	let contract = connector.contract.expect("PD with a position");
	assert_eq!((contract.offer.position, contract.offer.object, contract.operating, contract.maximum, contract.capability_mismatch), (2, fixed(9_000, 3_000), 2_000, 3_000, false));
	assert_eq!(connector.offers.len(), 3);
	assert_eq!(connector.orientation, Orientation::Normal);
	let source = power_model::usbc::usb_c(&supply(&port, 0x0210));
	assert_eq!((source.present, source.online, source.voltage.state, source.voltage.value, source.current.value), (Tristate::Yes, Tristate::Yes, ValueState::Known, 9_020_000, -1_500_000));
	assert_eq!(validate(&source), Ok(()));
	let old = power_model::usbc::usb_c(&supply(&port, 0x0120));
	assert_eq!((old.voltage.state, old.current.state), (ValueState::Unsupported, ValueState::Unsupported), "before 2.1 nothing is measured");
}

#[test]
fn a_host_offering_data_and_a_device_turn_the_data_role_round() {
	let mut port = charger();
	port.status.as_mut().expect("status").partner_flags = 1;
	assert_eq!(port.partner(), PartnerKind::Host);
	port.status.as_mut().expect("status").partner = PartnerType::Ufp;
	port.status.as_mut().expect("status").source = true;
	assert_eq!((port.partner(), port.data_role(), port.power_role()), (PartnerKind::Device, Some(DataRole::Host), Some(PowerRole::Source)));
}

#[test]
fn a_detached_connector_carries_nothing_of_its_last_partner_and_its_source_knows_nothing() {
	let mut port = charger();
	port.cable = Some(CableProperty { speed: 1, current: 3000, vbus: true, active: false, directional: false, plug_end: 2, alternate_modes: false, pd_revision: 2 });
	port.status = Some(ConnectorStatus { connected: false, partner: PartnerType::None, rdo: 0, voltage: Some(40_000), ..port.status.expect("status") });
	let connector = record(&port, &capability(Capability::PDO_DETAILS | Capability::CABLE_DETAILS), 0x0210);
	assert_eq!((connector.partner, connector.contract.is_none(), connector.offers.len(), connector.cable.is_none(), connector.power_role), (PartnerKind::None, true, 0, true, None));
	let source = power_model::usbc::usb_c(&supply(&port, 0x0210));
	assert_eq!((source.present, source.online, source.voltage.state), (Tristate::No, Tristate::No, ValueState::Unknown));
	assert_eq!(validate(&source), Ok(()));
}

#[test]
fn a_contract_naming_an_offer_nobody_made_is_no_contract() {
	let mut port = charger();
	port.status.as_mut().expect("status").rdo = 7 << 28;
	assert_eq!(record(&port, &capability(0), 0x0210).contract, None);
	port.status.as_mut().expect("status").operation_mode = 5;
	let connector = record(&port, &capability(0), 0x0210);
	assert_eq!((connector.contract, connector.typec_current), (None, TypecCurrent::High), "Type-C current, no contract");
}

#[test]
fn a_silent_ppm_leaves_the_record_as_last_read_and_the_supply_unknown() {
	let mut port = charger();
	port.answering = false;
	assert!(!record(&port, &capability(0), 0x0210).answering);
	let source = power_model::usbc::usb_c(&supply(&port, 0x0210));
	assert_eq!((source.present, source.online), (Tristate::Unknown, Tristate::Unknown));
	assert_eq!(validate(&source), Ok(()));
}

#[test]
fn displayport_is_entered_by_its_connector_offset_with_pin_d_first_and_refused_without_the_override() {
	let mut port = charger();
	port.modes = vec![AlternateMode { svid: DISPLAYPORT_SVID, vdo: 0x0C_0045 }];
	port.own_modes = vec![AlternateMode { svid: 0x8087, vdo: 1 }, AlternateMode { svid: DISPLAYPORT_SVID, vdo: 0x0405 }];
	assert_eq!(mode_offset(&port, &capability(0), DISPLAYPORT_SVID), Err(Refusal::PlatformEntersModes));
	assert_eq!(mode_offset(&port, &capability(Capability::ALT_MODE_OVERRIDE), DISPLAYPORT_SVID), Ok(1));
	assert_eq!(mode_offset(&port, &capability(Capability::ALT_MODE_OVERRIDE), 0x1234), Err(Refusal::ConnectorCannot));
	// A receptacle offering C and D: D is chosen.
	assert_eq!(pin_assignment(0x0C_0045), Some(0x08));
	assert_eq!(pin_assignment(0x04_0045), Some(0x04));
	assert_eq!(pin_assignment(0x00_1005), Some(0x10), "a plug's pins are in bits 15-8");
	assert_eq!(pin_assignment(0x0000_0045), None);
	assert_eq!(dp_configuration(0x08), 0x0805);
	port.current = Some(1);
	port.pin = Some(0x08);
	port.attention = Some(0x80);
	let connector = record(&port, &capability(Capability::ALT_MODE_OVERRIDE), 0x0210);
	assert!(connector.modes[0].entered);
	assert_eq!(connector.displayport, Some(Displayport { pin_assignment: 0x08, hot_plug: Some(true) }));
}

#[test]
fn a_swap_the_connector_cannot_take_is_refused_before_any_command() {
	let mut port = charger();
	assert_eq!(data_swap_refusal(&port, DataRole::Host), None);
	port.capability.swap_to_host = false;
	assert_eq!(data_swap_refusal(&port, DataRole::Host), Some(Refusal::ConnectorCannot));
	assert_eq!(power_swap_refusal(&port, PowerRole::Source), None);
	port.status = None;
	assert_eq!(power_swap_refusal(&port, PowerRole::Source), Some(Refusal::ConnectorCannot), "nobody to swap with");
}
