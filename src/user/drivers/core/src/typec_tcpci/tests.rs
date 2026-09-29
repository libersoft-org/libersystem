use super::*;
use alloc::string::String;
use alloc::vec;
use usb_pd::pdo::Selection;

// A property block written record by record, as the kernel and the ACPI service write theirs.
fn put(block: &mut Vec<u8>, kind: u8, depth: u8, name: &str, value: &[u8]) {
	block.push(kind);
	block.push(depth);
	block.extend_from_slice(&(name.len() as u16).to_le_bytes());
	block.extend_from_slice(&(value.len() as u32).to_le_bytes());
	block.extend_from_slice(name.as_bytes());
	block.extend_from_slice(value);
	block.resize(block.len() + ((value.len() + 3) & !3) - value.len(), 0);
}

fn cells(values: &[u32]) -> Vec<u8> {
	values.iter().flat_map(|value| value.to_be_bytes()).collect()
}

fn text(value: &str) -> Vec<u8> {
	let mut out = Vec::from(value.as_bytes());
	out.push(0);
	out
}

fn fixed(millivolts: u32, milliamps: u32) -> u32 {
	(millivolts / 50) << 10 | milliamps / 10
}

// THE GATE'S BOARD in a tree: the controller's own properties, then its `connector` child.
fn tree(role: &str, pdos: &[u32]) -> Vec<u8> {
	let mut block = Vec::new();
	put(&mut block, rt::DEVICE_PROPERTY_VALUE, 0, "compatible", &text("tcpci"));
	put(&mut block, rt::DEVICE_PROPERTY_VALUE, 0, "reg", &cells(&[0x52]));
	put(&mut block, rt::DEVICE_PROPERTY_NODE, 1, "connector", &[]);
	put(&mut block, rt::DEVICE_PROPERTY_VALUE, 1, "compatible", &text("usb-c-connector"));
	put(&mut block, rt::DEVICE_PROPERTY_VALUE, 1, "power-role", &text(role));
	put(&mut block, rt::DEVICE_PROPERTY_VALUE, 1, "sink-pdos", &cells(pdos));
	put(&mut block, rt::DEVICE_PROPERTY_VALUE, 1, "op-sink-microwatt", &cells(&[15_000_000]));
	block
}

fn wire(value: aml::wire::Value) -> Vec<u8> {
	aml::wire::encode(&value).expect("encodes")
}

// AND UNDER ACPI: `_DSD`'s hierarchical data node `connector`, each value in the node channel's encoding.
fn acpi(role: &str, pdos: &[u32]) -> Vec<u8> {
	let mut block = Vec::new();
	put(&mut block, rt::DEVICE_PROPERTY_VALUE, 0, "compatible", &wire(aml::wire::Value::String(String::from("tcpci"))));
	put(&mut block, rt::DEVICE_PROPERTY_NODE, 1, "connector", &[]);
	put(&mut block, rt::DEVICE_PROPERTY_VALUE, 1, "power-role", &wire(aml::wire::Value::String(String::from(role))));
	put(&mut block, rt::DEVICE_PROPERTY_VALUE, 1, "sink-pdos", &wire(aml::wire::Value::Package(pdos.iter().map(|pdo| aml::wire::Value::Integer(u64::from(*pdo))).collect())));
	put(&mut block, rt::DEVICE_PROPERTY_VALUE, 1, "op-sink-microwatt", &wire(aml::wire::Value::Integer(15_000_000)));
	block
}

fn board() -> Sink {
	Sink { pdos: vec![SinkPdo::Fixed { millivolts: 5_000, milliamps: 3_000 }, SinkPdo::Fixed { millivolts: 15_000, milliamps: 2_000 }], operational_microwatts: 15_000_000 }
}

#[test]
fn the_connector_is_read_alike_from_a_tree_and_from_dsd() {
	let pdos = [fixed(5_000, 3_000), fixed(15_000, 2_000)];
	assert_eq!(describe(&tree("sink", &pdos), false), Described::Sink(board()));
	assert_eq!(describe(&acpi("sink", &pdos), true), Described::Sink(board()));
	// A variable and a battery sink PDO, and an augmented one left out.
	let wide = [fixed(5_000, 3_000), 2 << 30 | (12_000 / 50) << 20 | (9_000 / 50) << 10 | 100, 1 << 30 | (20_000 / 50) << 20 | (9_000 / 50) << 10 | 108, 3 << 30 | 1];
	let Described::Sink(sink) = describe(&tree("sink", &wide), false) else { panic!("a sink") };
	assert_eq!(
		sink.pdos[1..],
		[
			SinkPdo::Variable { min_millivolts: 9_000, max_millivolts: 12_000, milliamps: 1_000 },
			SinkPdo::Battery { min_millivolts: 9_000, max_millivolts: 20_000, milliwatts: 27_000 }
		]
	);
}

#[test]
fn a_source_or_dual_role_connector_is_refused_and_a_missing_description_is_the_type_c_current() {
	let pdos = [fixed(5_000, 3_000)];
	assert_eq!(describe(&tree("source", &pdos), false), Described::Refused("source"));
	assert_eq!(describe(&acpi("dual", &pdos), true), Described::Refused("dual"));
	assert!(matches!(describe(&[], false), Described::TypeCOnly(_)), "no block");
	// THE CONTROLLER'S OWN PROPERTIES WITHOUT A CONNECTOR CHILD.
	let mut bare = Vec::new();
	put(&mut bare, rt::DEVICE_PROPERTY_VALUE, 0, "compatible", &text("tcpci"));
	assert!(matches!(describe(&bare, false), Described::TypeCOnly(_)));
	// Unreadable: the first sink PDO not vSafe5V, cells that are no whole number of words, no sink PDO at all, eight.
	assert!(matches!(describe(&tree("sink", &[fixed(9_000, 3_000)]), false), Described::TypeCOnly(_)));
	let mut cut = tree("sink", &pdos);
	let at = cut.windows(9).position(|window| window == b"sink-pdos").expect("the property");
	cut[at - 4] = 3;
	assert!(matches!(describe(&cut, false), Described::TypeCOnly(_)), "a value of three bytes");
	assert!(matches!(describe(&tree("sink", &[]), false), Described::TypeCOnly(_)));
	assert!(matches!(describe(&tree("sink", &[fixed(5_000, 3_000); 8]), false), Described::TypeCOnly(_)));
	assert!(matches!(describe(&tree("sideways", &pdos), false), Described::TypeCOnly(_)));
	// A block cut short in a record reads what came before it and no further.
	let whole = tree("sink", &pdos);
	assert!(matches!(describe(&whole[..whole.len() - 2], false), Described::TypeCOnly(_)));
}

fn report(attached: bool) -> Report {
	Report { attached, typec_current: attached.then_some(Rp::High), pd: false, revision: usb_pd::message::Revision::R3, offers: Vec::new(), contract: None, in_transition: false, hard_resets: 0, fault: false, sinking: attached, resetting: false }
}

#[test]
fn the_record_carries_the_contract_the_offers_and_the_type_c_current() {
	let offers = vec![fixed(5_000, 3_000) | 1 << 26, fixed(9_000, 3_000), fixed(15_000, 3_000)];
	let mut made = report(true);
	made.pd = true;
	made.offers = offers.clone();
	made.contract = Some((Selection { position: 3, millivolts: 15_000, milliamps: 2_000, mismatch: false }, offers[2]));
	let connector = record(&made, Some(true), true, None);
	let contract = connector.contract.expect("the contract");
	assert_eq!((contract.offer.position, contract.offer.object, contract.operating, contract.maximum, contract.in_transition), (3, offers[2], 2_000, 2_000, false));
	assert_eq!((connector.partner, connector.orientation, connector.operation_mode, connector.typec_current), (PartnerKind::Host, Orientation::Flipped, OperationMode::PowerDelivery, TypecCurrent::High));
	assert_eq!(connector.offers.len(), 3);
	assert_eq!((connector.capabilities.transport, connector.capabilities.sink, connector.capabilities.source, connector.capabilities.power_delivery), (Transport::Tcpci, true, false, true));
	// IN TRANSITION, as the engine reports it.
	made.in_transition = true;
	assert!(record(&made, Some(false), true, None).contract.expect("in transition").in_transition);
	// WITHOUT POWER DELIVERY: a supply at the Type-C current.
	let plain = report(true);
	let connector = record(&plain, Some(false), false, None);
	assert_eq!((connector.partner, connector.operation_mode, connector.typec_current, connector.contract), (PartnerKind::Charger, OperationMode::Typec3a, TypecCurrent::High, None));
	// DETACHED: nothing of a partner.
	let connector = record(&report(false), None, true, None);
	assert_eq!((connector.partner, connector.power_role, connector.orientation, connector.offers.len()), (PartnerKind::None, None, Orientation::Unknown, 0));
}

#[test]
fn the_supply_is_what_the_controller_measures_and_its_alarm() {
	let mut attached = report(true);
	attached.fault = true;
	let supply = supply(&attached, Some(15_000), true);
	assert_eq!((supply.attached, supply.sinking, supply.voltage, supply.current, supply.fault), (Some(true), Some(true), Reading::Value(15_000_000), Reading::Unsupported, Some(true)));
	assert_eq!(super::supply(&attached, None, false).voltage, Reading::Unsupported, "a controller that measures nothing");
	assert_eq!(super::supply(&report(false), Some(0), true).sinking, None);
}
