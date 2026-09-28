//! THE ACPI SERVICE'S DECISIONS AGAINST SCRIPTED NAMESPACES: a DSDT encoded with the interpreter's own encoder, walked
//! with `_STA` and `_INI`, every node given its role and its row; the handshakes' answers; the node channel's
//! admission; the event methods.

use std::string::String;
use std::vec;
use std::vec::Vec;

use aml::devices::{Identity, Kind};
use aml::testing::build::*;
use aml::testing::{Model, load};
use platform::report::{Function, osc};

use crate::admission::{self, Refusal, Target};
use crate::events::{self, Answer, Trigger};
use crate::handshake;
use crate::node::{self, Objects, Role, Scope};
use crate::properties;

fn id(hid: Option<&str>, cids: &[&str], adr: Option<u64>) -> Identity {
	Identity { hid: hid.map(String::from), cids: cids.iter().map(|cid| String::from(*cid)).collect(), uid: None, adr }
}

#[test]
fn each_kind_of_node_is_said_not_guessed() {
	let none = Objects::default();
	assert_eq!(node::role(Kind::Device, &id(Some("PNP0A08"), &["PNP0A03"], None), None, &none), Role::HostBridge);
	assert_eq!(node::role(Kind::Device, &id(Some("PNP0C02"), &[], None), None, &none), Role::Reservation);
	assert_eq!(node::role(Kind::Device, &id(Some("PNP0C01"), &[], None), None, &none), Role::Reservation);
	assert_eq!(node::role(Kind::Device, &id(Some("PNP0C09"), &[], None), None, &none), Role::EmbeddedController);
	assert_eq!(node::role(Kind::Device, &id(Some("ACPI0007"), &[], None), None, &none), Role::Processor);
	assert_eq!(node::role(Kind::Processor, &id(None, &[], None), None, &none), Role::Processor);
	assert_eq!(node::role(Kind::ThermalZone, &id(None, &[], None), None, &none), Role::ThermalZone);
	assert!(matches!(node::role(Kind::PowerResource, &id(None, &[], None), None, &none), Role::NotADevice(_)));
	assert!(matches!(node::role(Kind::Device, &id(None, &[], None), None, &none), Role::NotADevice(_)), "neither _HID nor _ADR");
	assert!(matches!(node::role(Kind::Device, &id(None, &[], Some(0x10000)), None, &none), Role::NotADevice(_)), "an _ADR with no host bridge above");
	assert_eq!(node::role(Kind::Device, &id(Some("MSFT0101"), &[], None), None, &none), Role::Device { parent: None, class: None });
	// UNDER A HOST BRIDGE, `_ADR` IS A FUNCTION; under a bridge's companion, a function on its secondary bus.
	let root = Scope::HostBridge { segment: 0, bus: 0 };
	assert_eq!(node::role(Kind::Device, &id(None, &[], Some(0x0003_0001)), Some(root), &none), Role::Companion { segment: 0, bus: 0, device: 3, function: 1 });
	let bridge = Scope::Companion { function: Function { segment: 0, bus: 0, device: 0x1c, function: 0 }, secondary: Some(2) };
	assert_eq!(node::role(Kind::Device, &id(None, &[], Some(0)), Some(bridge), &none), Role::Companion { segment: 0, bus: 2, device: 0, function: 0 });
	// BELOW AN ENDPOINT'S COMPANION a node is never a PCI function: with `_HID` a device carrying the function; with
	// none, a class from the rule table or not a device.
	let gpu = Function { segment: 0, bus: 0, device: 2, function: 0 };
	let endpoint = Scope::Companion { function: gpu, secondary: None };
	let output = Objects { bcl: true, bcm: true, parent_dod: true, parent_dos: false };
	assert_eq!(node::role(Kind::Device, &id(None, &[], Some(0x0400)), Some(endpoint), &output), Role::Device { parent: Some(gpu), class: Some(node::VIDEO_OUTPUT) });
	assert!(matches!(node::role(Kind::Device, &id(None, &[], Some(0x0400)), Some(endpoint), &Objects { bcm: false, ..output }), Role::NotADevice(_)), "_BCL without _BCM is no video output");
	assert!(matches!(node::role(Kind::Device, &id(None, &[], Some(0x0400)), Some(endpoint), &Objects { parent_dod: false, ..output }), Role::NotADevice(_)), "nor below an adapter with neither _DOD nor _DOS");
	assert_eq!(node::role(Kind::Device, &id(Some("PNP0C50"), &[], None), Some(endpoint), &none), Role::Device { parent: Some(gpu), class: None }, "a HID-over-I2C device below a controller's companion");
	assert_eq!(node::role(Kind::Device, &id(Some("PNP0C50"), &[], None), Some(Scope::Child { parent: gpu }), &none), Role::Device { parent: Some(gpu), class: None }, "and below that");
	// THE SCOPES A ROLE MAKES.
	assert_eq!(node::scope_for(&Role::HostBridge, 0, 0x20, None, None), Some(Scope::HostBridge { segment: 0, bus: 0x20 }));
	assert_eq!(node::scope_for(&Role::Companion { segment: 0, bus: 0, device: 0x1c, function: 0 }, 0, 0, Some(2), None), Some(bridge));
	assert_eq!(node::scope_for(&Role::NotADevice("x"), 0, 0, None, Some(root)), Some(root), "a scope-like node passes its scope through");
	assert_eq!(node::scope_for(&Role::Reservation, 0, 0, None, Some(root)), None);
}

// A DSDT with a host bridge, a reservation over MCFG-like memory, a companion with a child video output, a platform
// device with every kind of resource, a controller for its connections, an EC, a processor and a thermal zone.
fn machine() -> Vec<u8> {
	let crs = template(&[
		memory32_fixed(true, 0xFED7_0000, 0x1000),
		io(0x2E8, 8),
		interrupt(true, true, true, &[20]),
		gpio(true, 0x0001, &[3], "\\_SB.GPI0"),
		gpio(false, 0x0002, &[9], "\\_SB.GPI0"),
		i2c(0x50, 400_000, "\\_SB.I2C0"),
	]);
	let dsd = package(&[
		uuid(aml::dsd::DEVICE_PROPERTIES),
		package(&[package(&[string("fixture-mode"), int(3)])]),
		uuid(aml::dsd::HIERARCHICAL_DATA),
		package(&[package(&[string("port@0"), string("PRT0")])]),
	]);
	let body = cat(&[
		scope(
			"\\_SB_",
			cat(&[
				device(
					"PCI0",
					cat(&[
						name_obj("_HID", eisaid("PNP0A08")),
						name_obj("_CID", eisaid("PNP0A03")),
						name_obj("_BBN", int(0)),
						device(
							"GFX0",
							cat(&[
								name_obj("_ADR", int(0x0002_0000)),
								method("_DOD", 0, ret(package(&[int(0x400)]))),
								device("LCD0", cat(&[name_obj("_ADR", int(0x400)), method("_BCL", 0, ret(package(&[int(100), int(50)]))), method("_BCM", 1, vec![])])),
							]),
						),
						device("RP01", cat(&[name_obj("_ADR", int(0x001C_0000)), device("SLT0", name_obj("_ADR", int(0)))])),
					]),
				),
				device("DRAC", cat(&[name_obj("_HID", eisaid("PNP0C01")), name_obj("_CRS", buffer(&template(&[memory32_fixed(false, 0xB000_0000, 0x1000_0000)])))])),
				device("GPI0", cat(&[name_obj("_HID", string("LSFX0003")), name_obj("_UID", int(0))])),
				device(
					"I2C0",
					cat(&[
						name_obj("_HID", string("LSFX0004")),
						device(
							"DEV0",
							cat(&[
								name_obj("_HID", string("LSFX0001")),
								name_obj("_UID", string("fixture")),
								name_obj("_CRS", buffer(&crs)),
								name_obj("PRT0", package(&[uuid(aml::dsd::DEVICE_PROPERTIES), package(&[package(&[string("reg"), int(0)])])])),
								name_obj("_DSD", dsd),
							]),
						),
					]),
				),
				device("EC0_", cat(&[name_obj("_HID", eisaid("PNP0C09")), name_obj("_CRS", buffer(&template(&[io(0x62, 1), io(0x66, 1)])))])),
				device("CPU0", name_obj("_HID", string("ACPI0007"))),
				device("GONE", cat(&[name_obj("_HID", string("LSFX0002")), method("_STA", 0, ret(int(0)))])),
			]),
		),
		thermal_zone("\\_TZ_.TZ00", method("_TMP", 0, ret(int(3000)))),
	]);
	body
}

fn roles(aml: &mut aml::Aml, model: &mut Model) -> Vec<(String, Role, Option<Scope>)> {
	let walk = aml.initialize(model);
	assert!(walk.failures.is_empty(), "the walk ran clean: {:?}", walk.failures);
	let mut scopes: Vec<(aml::NodeId, Option<Scope>)> = Vec::new();
	let mut out = Vec::new();
	for found in walk.found {
		let identity = aml.identity(found.node, model).expect("an identity");
		let inherited = {
			let mut at = aml.ns.parent(found.node);
			let mut scope = None;
			while let Some(parent) = at {
				if let Some((_, known)) = scopes.iter().find(|(node, _)| *node == parent) {
					scope = *known;
					break;
				}
				at = aml.ns.parent(parent);
			}
			scope
		};
		let has = |aml: &aml::Aml, node: aml::NodeId, name: &[u8; 4]| aml.ns.child(node, aml::Seg(*name)).is_some();
		let parent = aml.ns.parent(found.node).unwrap_or(aml::ROOT);
		let objects = Objects { bcl: has(aml, found.node, b"_BCL"), bcm: has(aml, found.node, b"_BCM"), parent_dod: has(aml, parent, b"_DOD"), parent_dos: has(aml, parent, b"_DOS") };
		let role = node::role(found.kind, &identity, inherited, &objects);
		// The bridge's secondary bus is read from the bridge in the service; here RP01 forwards bus 2.
		let secondary = matches!(role, Role::Companion { device: 0x1c, .. }).then_some(2);
		let scope = node::scope_for(&role, 0, 0, secondary, inherited);
		scopes.push((found.node, scope));
		out.push((found.path.text(), role, scope));
	}
	out
}

#[test]
fn a_walked_namespace_is_every_node_accounted_for() {
	let (mut aml, mut model) = load(&machine());
	let found = roles(&mut aml, &mut model);
	let role_of = |path: &str| found.iter().find(|(at, ..)| at == path).map(|(_, role, _)| role.clone());
	assert_eq!(role_of("\\_SB_.PCI0"), Some(Role::HostBridge));
	assert_eq!(role_of("\\_SB_.PCI0.GFX0"), Some(Role::Companion { segment: 0, bus: 0, device: 2, function: 0 }));
	assert_eq!(role_of("\\_SB_.PCI0.GFX0.LCD0"), Some(Role::Device { parent: Some(Function { segment: 0, bus: 0, device: 2, function: 0 }), class: Some(node::VIDEO_OUTPUT) }));
	assert_eq!(role_of("\\_SB_.PCI0.RP01"), Some(Role::Companion { segment: 0, bus: 0, device: 0x1c, function: 0 }));
	assert_eq!(role_of("\\_SB_.PCI0.RP01.SLT0"), Some(Role::Companion { segment: 0, bus: 2, device: 0, function: 0 }), "behind the bridge, on its secondary bus");
	assert_eq!(role_of("\\_SB_.DRAC"), Some(Role::Reservation));
	assert_eq!(role_of("\\_SB_.EC0_"), Some(Role::EmbeddedController));
	assert_eq!(role_of("\\_SB_.CPU0"), Some(Role::Processor));
	assert_eq!(role_of("\\_TZ_.TZ00"), Some(Role::ThermalZone));
	assert_eq!(role_of("\\_SB_.I2C0.DEV0"), Some(Role::Device { parent: None, class: None }));
	assert_eq!(role_of("\\_SB_.GONE"), None, "a device whose _STA says absent is not walked");
}

#[test]
fn a_device_is_described_with_its_resources_and_its_connections() {
	let (mut aml, mut model) = load(&machine());
	let _ = aml.initialize(&mut model);
	let node = aml.lookup("\\_SB_.I2C0.DEV0").expect("the device");
	let identity = aml.identity(node, &mut model).expect("its identity");
	let resources = aml.resources(node, &mut model).expect("its _CRS");
	let dsd = aml.properties(node, &mut model).expect("its _DSD");
	let block = properties::block(identity.uid.as_deref(), &dsd);
	assert!(!block.cut);
	let path = aml.ns.path(node);
	let mut resolve = |text: &str| aml.lookup_from(node, text).map(|target| node::identity(&aml.ns.path(target)));
	let described = node::describe(&path, &Role::Device { parent: None, class: None }, &identity, &resources, block.bytes.clone(), &mut resolve).expect("described");
	let part = &described.description.part;
	assert_eq!(described.description.identity(), b"acpi:\\_SB_.I2C0.DEV0");
	assert_eq!(part.state, abi::PLATFORM_STATE_CLAIMABLE);
	assert_eq!(part.match_ids().iter().map(|id| (id.kind, id.text().to_vec())).collect::<Vec<_>>(), vec![(abi::MATCH_ID_HID, b"LSFX0001".to_vec())]);
	assert_eq!(part.mmio().iter().map(|range| (range.base, range.len)).collect::<Vec<_>>(), vec![(0xFED7_0000, 0x1000)]);
	assert_eq!(described.description.ports().iter().map(|port| (port.base, port.len)).collect::<Vec<_>>(), vec![(0x2E8, 8)]);
	assert_eq!(part.lines().iter().map(|line| (line.number, line.trigger, line.polarity)).collect::<Vec<_>>(), vec![(20, abi::LINE_TRIGGER_LEVEL, abi::LINE_POLARITY_LOW)]);
	// THE GpioInt ON GPI0 AND THE I2C ADDRESS ON THE PARENT BUS; THE OUTPUT GpioIo REFUSED BY NAME.
	assert_eq!(part.connections().iter().map(|connection| (connection.kind, connection.value)).collect::<Vec<_>>(), vec![(abi::CONNECTION_GPIO_LINE, 3), (abi::CONNECTION_I2C, 0x50)]);
	assert_eq!(described.targets, vec![(0, String::from("acpi:\\_SB_.GPI0")), (1, String::from("acpi:\\_SB_.I2C0"))]);
	assert_eq!(described.refused.len(), 1);
	assert!(described.refused[0].contains("output"), "{:?}", described.refused);
	assert!(!described.method_only());
	// THE BLOCK: `_UID`, the property, the data node and its own property, each value in the channel's encoding.
	let records = records(&block.bytes);
	assert_eq!(records[0], (abi::DEVICE_PROPERTY_VALUE, 0, String::from("_UID"), aml::wire::encode(&aml::wire::Value::String(String::from("fixture"))).unwrap()));
	assert_eq!(records[1], (abi::DEVICE_PROPERTY_VALUE, 0, String::from("fixture-mode"), aml::wire::encode(&aml::wire::Value::Integer(3)).unwrap()));
	assert_eq!((records[2].0, records[2].1, records[2].2.as_str()), (abi::DEVICE_PROPERTY_NODE, 1, "port@0"));
	assert_eq!((records[3].0, records[3].1, records[3].2.as_str()), (abi::DEVICE_PROPERTY_VALUE, 1, "reg"));
	// A RESERVATION IS A ROW IN THAT STATE; the embedded controller is FIRMWARE-HELD; a processor is no row.
	let drac = aml.lookup("\\_SB_.DRAC").expect("DRAC");
	let identity = aml.identity(drac, &mut model).unwrap();
	let resources = aml.resources(drac, &mut model).unwrap();
	let reservation = node::describe(&aml.ns.path(drac), &Role::Reservation, &identity, &resources, Vec::new(), &mut |_| None).unwrap();
	assert_eq!(reservation.description.part.state, abi::PLATFORM_STATE_RESERVATION);
	assert_eq!(reservation.description.part.mmio()[0].base, 0xB000_0000);
	let ec = aml.lookup("\\_SB_.EC0_").expect("the EC");
	let identity = aml.identity(ec, &mut model).unwrap();
	let resources = aml.resources(ec, &mut model).unwrap();
	assert_eq!(node::describe(&aml.ns.path(ec), &Role::EmbeddedController, &identity, &resources, Vec::new(), &mut |_| None).unwrap().description.part.state, abi::PLATFORM_STATE_FIRMWARE_HELD);
	assert!(node::describe(&aml.ns.path(ec), &Role::Processor, &identity, &resources, Vec::new(), &mut |_| None).is_err());
	// A METHOD-ONLY DEVICE: a video output, published under its class, carrying its adapter's function.
	let lcd = aml.lookup("\\_SB_.PCI0.GFX0.LCD0").expect("the output");
	let identity = aml.identity(lcd, &mut model).unwrap();
	let gfx = Function { segment: 0, bus: 0, device: 2, function: 0 };
	let output = node::describe(&aml.ns.path(lcd), &Role::Device { parent: Some(gfx), class: Some(node::VIDEO_OUTPUT) }, &identity, &[], Vec::new(), &mut |_| None).unwrap();
	assert!(output.method_only());
	assert_eq!(output.parent, Some(gfx));
	assert_eq!(output.description.part.match_ids()[0].text(), node::VIDEO_OUTPUT.as_bytes());
	// A THERMAL ZONE, under THERMALZONE.
	let tz = aml.lookup("\\_TZ_.TZ00").expect("the zone");
	let zone = node::describe(&aml.ns.path(tz), &Role::ThermalZone, &Identity::default(), &[], Vec::new(), &mut |_| None).unwrap();
	assert_eq!(zone.description.part.match_ids()[0].text(), b"THERMALZONE");
}

#[test]
fn a_row_past_its_bounds_is_refused_whole() {
	let cids: Vec<String> = (0..abi::MAX_MATCH_IDS).map(|at| std::format!("LSFX{at:04}")).collect();
	let identity = Identity { hid: Some(String::from("LSFX9999")), cids, uid: None, adr: None };
	let path = aml::Path(vec![aml::Seg(*b"_SB_"), aml::Seg(*b"MANY")]);
	assert!(node::describe(&path, &Role::Device { parent: None, class: None }, &identity, &[], Vec::new(), &mut |_| None).is_err());
}

fn records(block: &[u8]) -> Vec<(u8, u8, String, Vec<u8>)> {
	let mut out = Vec::new();
	let mut at = 0;
	while at + 8 <= block.len() {
		let kind = block[at];
		let depth = block[at + 1];
		let name_len = u16::from_le_bytes([block[at + 2], block[at + 3]]) as usize;
		let value_len = u32::from_le_bytes(block[at + 4..at + 8].try_into().unwrap()) as usize;
		let name = String::from_utf8(block[at + 8..at + 8 + name_len].to_vec()).unwrap();
		let value = block[at + 8 + name_len..at + 8 + name_len + value_len].to_vec();
		out.push((kind, depth, name, value));
		at += 8 + name_len + ((value_len + 3) & !3);
	}
	out
}

#[test]
fn the_handshakes_grant_only_what_was_asked_and_answered() {
	assert_eq!(handshake::pci_granted(None), 0, "no _OSC grants nothing");
	assert_eq!(handshake::pci_granted(Some(&[0, handshake::PCI_SUPPORT, osc::HOT_PLUG | osc::AER | 1 << 30])), osc::HOT_PLUG | osc::AER, "masked to what was asked");
	assert_eq!(handshake::pci_granted(Some(&[handshake::status::UNRECOGNIZED_UUID, 0, handshake::PCI_CONTROL])), 0);
	assert_eq!(handshake::pci_granted(Some(&[handshake::status::FAILURE, 0, handshake::PCI_CONTROL])), 0);
	assert_eq!(handshake::pci_granted(Some(&[0, 0])), 0, "a short answer grants nothing");
	assert_eq!(handshake::pci_granted(Some(&[handshake::status::CAPABILITIES_MASKED, 0, osc::PME])), osc::PME, "a masked answer grants what it kept");
	assert_eq!(handshake::pci_request(), [0, handshake::PCI_SUPPORT, osc::HOT_PLUG | osc::PME | osc::AER | osc::CAPABILITY | osc::LTR]);
	assert_eq!(handshake::platform_granted(Some(&[0, u32::MAX])), handshake::PLATFORM_SUPPORT);
	// THE PROCESSOR FORMS NAME NO MODEL-SPECIFIC REGISTER: not P-state (bit 0) nor T-state (bit 2) fixed hardware.
	assert_eq!(handshake::PROCESSOR_SUPPORT & (0x0001 | 0x0004), 0);
	assert_ne!(handshake::PROCESSOR_SUPPORT & 0x0300, 0, "the MWAIT hints are declared");
	assert_eq!(&handshake::pdc_buffer()[8..12], &handshake::PROCESSOR_SUPPORT.to_le_bytes());
	// A SCRIPTED HOST BRIDGE: its `_OSC` keeps hot-plug and PME and answers the rest masked.
	let osc_body = cat(&[
		create_dword_field(arg(3), int(0), "CDW1"),
		create_dword_field(arg(3), int(8), "CDW3"),
		store(and(name("CDW3"), int((osc::HOT_PLUG | osc::PME) as u64), vec![0x00]), name("CDW3")),
		store(or(name("CDW1"), int(handshake::status::CAPABILITIES_MASKED as u64), vec![0x00]), name("CDW1")),
		ret(arg(3)),
	]);
	let (mut aml, mut model) = load(&scope("\\_SB_", device("PCI0", cat(&[name_obj("_HID", eisaid("PNP0A08")), method("_OSC", 4, osc_body)]))));
	let bridge = aml.lookup("\\_SB_.PCI0").unwrap();
	let answer = aml.osc(bridge, handshake::PCI_HOST_BRIDGE, 1, &handshake::pci_request(), &mut model).expect("_OSC runs");
	assert_eq!(handshake::pci_granted(answer.as_deref()), osc::HOT_PLUG | osc::PME);
	assert_eq!(handshake::pci_control_text(osc::HOT_PLUG | osc::PME), "hot-plug, PME");
	assert_eq!(handshake::pci_control_text(0), "nothing");
}

#[test]
fn a_driver_evaluates_its_own_node_and_nothing_else() {
	assert_eq!(admission::admit("", None), Ok(Target::Node));
	assert_eq!(admission::admit("_STA", None), Ok(Target::Own(*b"_STA")));
	assert_eq!(admission::admit("RDRG", None), Ok(Target::Own(*b"RDRG")));
	assert_eq!(admission::admit("_BC", None), Ok(Target::Own(*b"_BC_")), "a short name is padded");
	for platform in ["_INI", "_REG", "_OSC", "_PTS", "_WAK", "_S3", "_SST"] {
		assert_eq!(admission::admit(platform, None), Err(Refusal::Platform), "{platform}");
	}
	assert_eq!(admission::admit("\\_SB.PCI0._INI", None), Err(Refusal::OtherNode));
	assert_eq!(admission::admit("CHLD.RDRG", None), Err(Refusal::OtherNode));
	assert_eq!(admission::admit("^_DOS", None), Err(Refusal::OtherNode), "no class, no parent method");
	assert_eq!(admission::admit("^_DOS", Some(node::VIDEO_OUTPUT)), Ok(Target::Parent(*b"_DOS")), "the video output's adapter's _DOS");
	assert_eq!(admission::admit("^_DOD", Some(node::VIDEO_OUTPUT)), Err(Refusal::OtherNode));
	assert_eq!(admission::admit("ab c", None), Err(Refusal::Malformed));
	assert_eq!(admission::admit("TOOLONG", None), Err(Refusal::Malformed));
}

#[test]
fn an_event_is_answered_by_its_method() {
	assert_eq!(events::parse(b"_L02"), Some((2, Trigger::Level)));
	assert_eq!(events::parse(b"_E1F"), Some((0x1F, Trigger::Edge)));
	assert_eq!(events::parse(b"_EVT"), None);
	assert_eq!(events::parse(b"_Q20"), None);
	assert_eq!(events::name(0x2A, Trigger::Edge), Some(*b"_E2A"));
	assert_eq!(events::name(0x100, Trigger::Edge), None);
	assert_eq!(events::handled(&[*b"_E02", *b"_L01", *b"_INI", *b"_L02"]), vec![(1, Trigger::Level), (2, Trigger::Edge)]);
	assert_eq!(events::gpio_answer(3, &[*b"_E03"], true), Some(Answer::Named(*b"_E03", Trigger::Edge)));
	assert_eq!(events::gpio_answer(3, &[*b"_L03"], false), Some(Answer::Named(*b"_L03", Trigger::Level)));
	assert_eq!(events::gpio_answer(0x1FF, &[], true), Some(Answer::Evt), "a pin above 255 is _EVT's");
	assert_eq!(events::gpio_answer(4, &[*b"_E03"], false), None);
}
