//! WHAT A NAMESPACE NODE IS, and the row it is reported as.
//!
//! EACH KIND IS SAID, NOT GUESSED. `PNP0C01` and `PNP0C02` are RESERVATIONS; a PCI host bridge is the root companions
//! are found under and the kernel's own bus; a node with `_ADR` under a host bridge - through PCI bridges only - is the
//! COMPANION of the function it describes; a node below an endpoint's companion that is not itself a PCI function is
//! a platform device of its own, carrying that function as its parent; `ACPI0007` and `Processor` objects and the
//! embedded controller are the service's own; a `ThermalZone` is published under the id `THERMALZONE`; any other node
//! with `_HID` is a platform device; a node with neither `_HID` nor `_ADR`, and a power resource, are not devices.
//! A child with no `_HID` MATCHES BY A CLASS from a rule table - the first row is `video-output`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use aml::devices::{Identity, Kind};
use aml::resource::Resource;
use platform::report::Function;

/// The prefix of every namespace identity.
pub const IDENTITY_PREFIX: &str = "acpi:";

/// A node's row identity: `acpi:` and its absolute path.
pub fn identity(path: &aml::Path) -> String {
	format!("{IDENTITY_PREFIX}{}", path.text())
}

/// What a node's nearest PCI ancestor made of it: a host bridge's bus, or a companion's function and - for a bridge -
/// the bus behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
	HostBridge {
		segment: u16,
		bus: u8,
	},
	Companion {
		function: Function,
		secondary: Option<u8>,
	},
	/// A platform device below an endpoint's companion: its descendants carry the same parent.
	Child {
		parent: Function,
	},
}

/// What a node is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
	Reservation,
	/// A PCI host bridge: the kernel's bus; `_OSC` is asked here.
	HostBridge,
	/// The companion of the PCI function its `_ADR` names on `bus`.
	Companion {
		segment: u16,
		bus: u8,
		device: u8,
		function: u8,
	},
	/// A platform device: claimable, with its `_CRS` resources - below a companion, carrying that function.
	Device {
		parent: Option<Function>,
		class: Option<&'static str>,
	},
	/// The embedded controller: FIRMWARE-HELD, the service's own transport.
	EmbeddedController,
	/// A processor: the service's own (the power half's).
	Processor,
	/// A thermal zone, published under `THERMALZONE`.
	ThermalZone,
	/// Not a device, and why.
	NotADevice(&'static str),
}

/// The id a thermal zone is published under.
pub const THERMALZONE: &str = "THERMALZONE";
/// The class a child with `_BCL` and `_BCM` below a companion with `_DOD` or `_DOS` is published under.
pub const VIDEO_OUTPUT: &str = "video-output";

fn ids(identity: &Identity) -> impl Iterator<Item = &str> {
	identity.hid.iter().map(|hid| hid.as_str()).chain(identity.cids.iter().map(|cid| cid.as_str()))
}

fn named(identity: &Identity, wanted: &[&str]) -> bool {
	ids(identity).any(|id| wanted.contains(&id))
}

/// The objects of a node and its parent the class rules read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Objects {
	pub bcl: bool,
	pub bcm: bool,
	pub parent_dod: bool,
	pub parent_dos: bool,
}

/// THE CLASS RULE TABLE, one row per class, for a child with no `_HID`: the first row is `video-output`, a child of a
/// companion that has `_DOD` or `_DOS`, itself having `_BCL` and `_BCM`.
pub fn class(objects: &Objects) -> Option<&'static str> {
	(objects.bcl && objects.bcm && (objects.parent_dod || objects.parent_dos)).then_some(VIDEO_OUTPUT)
}

/// WHAT A NODE IS, from its kind, its identity, the scope its nearest PCI ancestor made, and the class objects.
pub fn role(kind: Kind, identity: &Identity, scope: Option<Scope>, objects: &Objects) -> Role {
	match kind {
		Kind::Processor => return Role::Processor,
		Kind::ThermalZone => return Role::ThermalZone,
		Kind::PowerResource => return Role::NotADevice("a power resource"),
		Kind::Other => return Role::NotADevice("not a device object"),
		Kind::Device => {}
	}
	if named(identity, &["PNP0A03", "PNP0A08"]) {
		return Role::HostBridge;
	}
	if named(identity, &["PNP0C01", "PNP0C02"]) {
		return Role::Reservation;
	}
	// A PROCESSOR, and a processor container holding them: the power half's, never a driver's.
	if named(identity, &["ACPI0007", "ACPI0010"]) {
		return Role::Processor;
	}
	// A PCI INTERRUPT LINK: unused, since INTx is disabled and every function interrupts by MSI.
	if named(identity, &["PNP0C0F"]) {
		return Role::NotADevice("a PCI interrupt link - unused, INTx is disabled");
	}
	if named(identity, &["PNP0C09"]) {
		return Role::EmbeddedController;
	}
	match (identity.hid.is_some(), identity.adr, scope) {
		// A PCI FUNCTION: `_ADR` on the bus its scope roots.
		(false, Some(adr), Some(Scope::HostBridge { segment, bus })) => Role::Companion { segment, bus, device: (adr >> 16) as u8, function: adr as u8 },
		(false, Some(adr), Some(Scope::Companion { function, secondary: Some(bus) })) => Role::Companion { segment: function.segment, bus, device: (adr >> 16) as u8, function: adr as u8 },
		// BELOW AN ENDPOINT'S COMPANION, a node is never a PCI function: a platform device of its own, carrying the
		// function - a video output, a device on the controller's bus.
		(_, _, Some(Scope::Companion { function, secondary: None })) | (_, _, Some(Scope::Child { parent: function })) => {
			let class = if identity.hid.is_none() { class(objects) } else { None };
			if identity.hid.is_none() && class.is_none() {
				return Role::NotADevice("a node below a companion with neither _HID nor a class");
			}
			Role::Device { parent: Some(function), class }
		}
		(true, _, _) => Role::Device { parent: None, class: None },
		(false, Some(_), _) => Role::NotADevice("an _ADR with no PCI host bridge above it"),
		(false, None, _) => Role::NotADevice("neither _HID nor _ADR"),
	}
}

/// The scope a node's role makes for its children: a host bridge's bus from its `_SEG` and `_BBN`, a companion's
/// function and, for a bridge, its secondary bus (read by the service from the bridge itself), a child's parent.
pub fn scope_for(role: &Role, segment: u16, bbn: u8, bridge_secondary: Option<u8>, inherited: Option<Scope>) -> Option<Scope> {
	match role {
		Role::HostBridge => Some(Scope::HostBridge { segment, bus: bbn }),
		Role::Companion { segment, bus, device, function } => Some(Scope::Companion { function: Function { segment: *segment, bus: *bus, device: *device, function: *function }, secondary: bridge_secondary }),
		Role::Device { parent: Some(parent), .. } => Some(Scope::Child { parent: *parent }),
		_ => match inherited {
			// A scope passes through a node that is not a device - a scope-like object - to what is below it.
			Some(scope) if matches!(role, Role::NotADevice(_)) => Some(scope),
			_ => None,
		},
	}
}

/// A row as reported: the description, each connection's controller by identity, the parent function, and the
/// property block - plus what could not be described, in words.
#[derive(Clone, Debug)]
pub struct Described {
	pub description: platform::Description,
	pub targets: Vec<(u8, String)>,
	pub parent: Option<Function>,
	pub properties: Vec<u8>,
	pub refused: Vec<String>,
}

impl Described {
	/// No memory range, port or wired line: a method-only device, whose claim carries its node channel and its
	/// connections.
	pub fn method_only(&self) -> bool {
		self.description.part.mmio_count == 0 && self.description.port_count == 0 && self.description.part.line_count == 0
	}
}

/// The id a node takes to be matched by its `_DSD`'s `compatible`, as a device-tree node is.
pub const PRP0001: &str = "PRP0001";

/// THE `compatible` STRINGS of a property block's own node (depth 0): one string, or a package of them in order. A
/// value of any other shape names nothing.
pub fn compatibles(block: &[u8]) -> Vec<String> {
	let mut at = 0usize;
	while at + 8 <= block.len() {
		let (kind, depth) = (block[at], block[at + 1]);
		let name_len = u16::from_le_bytes([block[at + 2], block[at + 3]]) as usize;
		let value_len = u32::from_le_bytes([block[at + 4], block[at + 5], block[at + 6], block[at + 7]]) as usize;
		let value_at = at + 8 + name_len;
		let Some(end) = value_at.checked_add(value_len).filter(|end| *end <= block.len()) else { break };
		if kind == abi::DEVICE_PROPERTY_VALUE && depth == 0 && &block[at + 8..value_at] == b"compatible" {
			return match aml::wire::decode(&block[value_at..end]) {
				Ok(aml::wire::Value::String(text)) => alloc::vec![text],
				Ok(aml::wire::Value::Package(elements)) => elements
					.into_iter()
					.map_while(|element| match element {
						aml::wire::Value::String(text) => Some(text),
						_ => None,
					})
					.collect(),
				_ => Vec::new(),
			};
		}
		at = value_at + ((value_len + 3) & !3);
	}
	Vec::new()
}

/// THE ROW A NODE IS REPORTED AS: its identity, the state its role puts it in, `_HID` and each `_CID` (or its class),
/// and its `_CRS` resources - memory, ports, wired lines, and the GPIO and I2C connections by their controllers'
/// identities (`resolve` turns a resource source into one). A `GpioIo` restricted to output is refused by name, as a
/// part of the device this system does not support; anything past a row's bounds refuses the whole report.
pub fn describe(path: &aml::Path, role: &Role, identity: &Identity, resources: &[Resource], properties: Vec<u8>, resolve: &mut dyn FnMut(&str) -> Option<String>) -> Result<Described, String> {
	let name = self::identity(path);
	let state = match role {
		Role::Reservation => abi::PLATFORM_STATE_RESERVATION,
		Role::EmbeddedController => abi::PLATFORM_STATE_FIRMWARE_HELD,
		Role::Device { .. } | Role::ThermalZone => abi::PLATFORM_STATE_CLAIMABLE,
		other => return Err(format!("{name} is {other:?}, which is not reported as a row")),
	};
	let mut description = platform::Description::new(abi::PLATFORM_SOURCE_ACPI, state, name.as_bytes()).ok_or_else(|| format!("{name} is longer than a row's identity"))?;
	let mut whole = true;
	if *role == Role::ThermalZone {
		whole &= description.add_match(abi::MATCH_ID_HID, THERMALZONE.as_bytes());
	}
	if let Some(hid) = &identity.hid {
		whole &= description.add_match(abi::MATCH_ID_HID, hid.as_bytes());
	}
	for cid in &identity.cids {
		whole &= description.add_match(abi::MATCH_ID_CID, cid.as_bytes());
	}
	// A `PRP0001` NODE IS MATCHED AS THE TREE MATCHES ITS NODE: by its `_DSD`'s `compatible` strings.
	if identity.hid.as_deref() == Some(PRP0001) || identity.cids.iter().any(|cid| cid == PRP0001) {
		for compatible in compatibles(&properties) {
			whole &= description.add_match(abi::MATCH_ID_COMPATIBLE, compatible.as_bytes());
		}
	}
	let parent = match role {
		Role::Device { parent, class } => {
			if let Some(class) = class {
				whole &= description.add_match(abi::MATCH_ID_CLASS, class.as_bytes());
			}
			*parent
		}
		_ => None,
	};
	let mut targets: Vec<(u8, String)> = Vec::new();
	let mut refused: Vec<String> = Vec::new();
	for resource in resources {
		match resource {
			Resource::Memory { base, length, .. } => whole &= description.add_mmio(*base, *length),
			Resource::Io { base, length } => match (u16::try_from(*base), u16::try_from(*length)) {
				(Ok(base), Ok(length)) => whole &= description.add_port(base, length),
				_ => refused.push(format!("ports {base:#x}+{length:#x} are not I/O port numbers")),
			},
			Resource::Interrupt { line, level, active_low, .. } => whole &= description.add_line(abi::WiredLine { number: *line, trigger: if *level { abi::LINE_TRIGGER_LEVEL } else { abi::LINE_TRIGGER_EDGE }, polarity: if *active_low { abi::LINE_POLARITY_LOW } else { abi::LINE_POLARITY_HIGH }, controller: abi::LINE_CONTROLLER_IOAPIC, _pad: 0 }),
			Resource::Gpio { interrupt, pins, controller, level, active_low, restriction, .. } => {
				// OUTPUT LINES ARE NOT SUPPORTED: a `GpioIo` restricted to output is refused by name.
				if !interrupt && *restriction == 2 {
					refused.push(format!("a GpioIo restricted to output on {controller} - output lines are not supported"));
					continue;
				}
				let Some(target) = resolve(controller) else {
					refused.push(format!("a GPIO connection names {controller}, which is no node"));
					continue;
				};
				for pin in pins {
					let at = description.part.connection_count;
					let trigger = match (interrupt, level) {
						(false, _) => 0,
						(true, true) => abi::LINE_TRIGGER_LEVEL,
						(true, false) => abi::LINE_TRIGGER_EDGE,
					};
					whole &= description.add_connection(abi::Connection { kind: abi::CONNECTION_GPIO_LINE, trigger, polarity: if *active_low { abi::LINE_POLARITY_LOW } else { abi::LINE_POLARITY_HIGH }, _pad: 0, controller: u32::MAX, value: *pin as u32, extra: 0 });
					targets.push((at, target.clone()));
				}
			}
			Resource::I2c { address, speed, ten_bit, controller, .. } => {
				let Some(target) = resolve(controller) else {
					refused.push(format!("an I2C connection names {controller}, which is no node"));
					continue;
				};
				let at = description.part.connection_count;
				// A TEN-BIT ADDRESS IS CARRIED AS ONE, so the binding refuses it by name rather than reach a seven-bit neighbour.
				let flags = if *ten_bit { abi::CONNECTION_I2C_TEN_BIT } else { 0 };
				whole &= description.add_connection(abi::Connection { kind: abi::CONNECTION_I2C, trigger: flags, polarity: 0, _pad: 0, controller: u32::MAX, value: *address as u32, extra: *speed });
				targets.push((at, target));
			}
			Resource::SerialBus { kind, controller, .. } => refused.push(format!("a serial bus of type {kind} on {controller} is not supported")),
			// A producer's bus numbers are a bridge's, and a DMA channel is the ISA controller's the kernel holds.
			Resource::BusNumbers { .. } | Resource::Dma { .. } | Resource::Register { .. } | Resource::Other { .. } => {}
		}
	}
	if !whole {
		return Err(format!("{name} names more ids or resources than a row carries, and part of a device is not published"));
	}
	Ok(Described { description, targets, parent, properties, refused })
}
