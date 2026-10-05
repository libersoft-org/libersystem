// THE DEVICES A MACHINE'S FIRMWARE DESCRIBES, the parts every port shares: naming the system from its
// SMBIOS tables, and turning the device tree's nodes into the descriptions the device table publishes.
//
// Each port's own `platform::describe` decides WHAT to describe - x86_64 its kernel-held set, COM1 and its
// static tables; the device-tree ports their tree - and hands the descriptions to `device::init`, which
// places each against the rows already published.

#[cfg(all(not(test), any(target_arch = "aarch64", target_arch = "riscv64")))]
use alloc::vec::Vec;

#[cfg(all(not(test), any(target_arch = "aarch64", target_arch = "riscv64")))]
use crate::device::Described;

// THE SYSTEM, AS ITS SMBIOS TABLES NAME IT, on the boot log - and every IPMI record type 38 carries, which
// the IPMI node the ACPI namespace describes is later checked against. `entry` is the entry point the loader
// found, or zero; `phys_to_virt` reaches physical memory on this port. Nothing here is a device: it is what
// the machine says it is.
// A test kernel on the device-tree ports reads no SMBIOS - its rows are the suite's own.
#[cfg(any(not(test), target_arch = "x86_64"))]
pub fn report_smbios(entry: u64, phys_to_virt: fn(u64) -> u64) {
	if entry == 0 {
		crate::serial_println!("smbios: none - no firmware handed over an entry point");
		return;
	}
	if !crate::mem::within_direct_map(entry, smbios::ENTRY_POINT_BYTES as u64) {
		crate::serial_println!("smbios: the entry point at {entry:#x} is outside the direct map - not read");
		return;
	}
	// SAFETY: inside the direct map, checked above; the entry point is at most this long.
	let bytes = unsafe { core::slice::from_raw_parts(phys_to_virt(entry) as *const u8, smbios::ENTRY_POINT_BYTES) };
	let point = match smbios::entry_point(bytes) {
		Ok(point) => point,
		Err(refusal) => {
			crate::serial_println!("smbios: the entry point at {entry:#x} is refused - {refusal:?}");
			return;
		}
	};
	if !crate::mem::within_direct_map(point.table_address, u64::from(point.table_length)) {
		crate::serial_println!("smbios: {}.{} names a structure table at {:#x} outside the direct map - not read", point.major, point.minor, point.table_address);
		return;
	}
	// SAFETY: as above, for the table's own stated length, which the entry point's parser bounded.
	let table = unsafe { core::slice::from_raw_parts(phys_to_virt(point.table_address) as *const u8, point.table_length as usize) };
	// ALLOC-OK: boot, the SMBIOS identity said once, before userspace exists.
	let text = |bytes: Option<&[u8]>| -> alloc::string::String { bytes.map(|bytes| alloc::string::String::from_utf8_lossy(bytes).into_owned()).unwrap_or_else(|| alloc::string::String::from("-")) };
	match smbios::system_identity(table, &point) {
		Ok(Some(system)) => {
			let mut uuid = [0u8; 36];
			let uuid = system.uuid.map(|raw| {
				smbios::uuid_text(&raw, &mut uuid);
				alloc::string::String::from_utf8_lossy(&uuid).into_owned()
			});
			crate::serial_println!("smbios: {}.{} - the system is {} {} (version {}, serial {}, uuid {})", point.major, point.minor, text(system.manufacturer), text(system.product), text(system.version), text(system.serial), uuid.as_deref().unwrap_or("-"));
		}
		Ok(None) => crate::serial_println!("smbios: {}.{} - the table names no system", point.major, point.minor),
		Err(refusal) => crate::serial_println!("smbios: {}.{} - the structure table is refused - {refusal:?}", point.major, point.minor),
	}
	let _ = smbios::ipmi_records(table, &point, |record| {
		// AN SSIF RECORD IS KEPT BY ITS SEVEN-BIT ADDRESS, the form an `IPI0001` node's `I2cSerialBusV2` names.
		let address = if record.interface == smbios::IPMI_SSIF { u64::from(record.ssif_address()) } else { record.address() };
		crate::firmware::note_ipmi_record(record.instance as usize, record.interface, address);
		let interface = match record.interface {
			smbios::IPMI_KCS => "KCS",
			smbios::IPMI_SMIC => "SMIC",
			smbios::IPMI_BT => "BT",
			smbios::IPMI_SSIF => "SSIF",
			_ => "an unknown interface",
		};
		if record.interface == smbios::IPMI_SSIF {
			crate::serial_println!("smbios: smbios:38#{} - an IPMI controller over {interface} at bus address {:#04x}", record.instance, record.ssif_address());
		} else {
			crate::serial_println!("smbios: smbios:38#{} - an IPMI controller over {interface} at {} {:#x}", record.instance, if record.io_space() { "port" } else { "address" }, record.address());
		}
	});
}

// THE NODES OF A DEVICE TREE, AS DESCRIPTIONS: each node naming a `compatible` and resources, identified by
// its path (`dt:/soc/serial@10000000`), matched by each of its `compatible` strings, with its `reg`
// translated to MMIO, its interrupts made WIRED LINES where their parent is the interrupt controller this
// kernel drives (`line` says what a specifier becomes on this port) and LINE CONNECTIONS where their parent
// is a GPIO controller that is also an interrupt controller, an address on the I2C or SPI bus it sits on made
// a connection, and its property block attached. KERNEL-HELD are the nodes whose `compatible` names a device
// the kernel drives itself, and the console UART at `console_base`.
//
// THE TPM NODE IS CUT TO ITS PAGE: QEMU's `tcg,tpm-tis-mmio` node names all five localities (0x5000 bytes),
// and the device is ONE MMIO range - locality 0's 4 KiB page at the node's base - with no interrupt, exactly
// as the `TPM2` table's row is on x86_64.
//
// PUBLISHED IN THE TREE'S OWN ORDER - a parent before its children. Each connection names its controller by
// that controller's identity - an I2C or SPI device's bus is the node containing it, a GPIO line's controller
// is the node carrying the phandle the line names - and `device::init` joins it to that row once every
// description is published.
#[cfg(all(not(test), any(target_arch = "aarch64", target_arch = "riscv64")))]
pub fn from_tree(tree: &fdt::Fdt, kernel_held: &[&[u8]], console_base: u64, line: impl Fn(&fdt::IntxRoute) -> Option<abi::WiredLine>) -> Vec<Described> {
	let mut nodes: Vec<fdt::DeviceNode> = Vec::new();
	let walked = tree.devices(|node| {
		// ALLOC-OK: boot, once, one entry per device node the tree describes.
		nodes.push(*node);
	});
	if !walked {
		crate::serial_println!("device: the device tree could not be walked to its end - its devices are not published");
		return Vec::new();
	}
	// THE TREE'S ORDER: a node's path is a prefix of its children's, so sorting by path puts a parent first.
	nodes.sort_by(|a, b| a.path().cmp(b.path()));
	// Who carries which phandle, so a GPIO line names its controller's row by that row's identity.
	let identity_of = |node: &fdt::DeviceNode| -> Option<Vec<u8>> {
		let mut identity = [0u8; abi::PLATFORM_NAME_LEN];
		// ALLOC-OK: boot, one identity per controller the device tree names, before userspace exists.
		platform::identity(b"dt:", node.path(), &mut identity).map(|len| identity[..len].to_vec())
	};
	// ALLOC-OK: as above.
	let controllers: Vec<(u32, Vec<u8>)> = nodes.iter().filter(|node| node.phandle != 0).filter_map(|node| identity_of(node).map(|identity| (node.phandle, identity))).collect();
	let mut out: Vec<Described> = Vec::new();
	let mut properties = [0u8; abi::MAX_DEVICE_PROPERTIES];
	for node in nodes.iter() {
		let mut identity = [0u8; abi::PLATFORM_NAME_LEN];
		let Some(len) = platform::identity(b"dt:", node.path(), &mut identity) else {
			crate::serial_println!("device: the node {} has a path too long to be an identity - not published", alloc::string::String::from_utf8_lossy(node.path()));
			continue;
		};
		let name = alloc::string::String::from_utf8_lossy(&identity[..len]).into_owned();
		// A PCI FUNCTION'S NODE is its companion - joined to the function's row the bus scan made, never a row of its
		// own - and what its children describe is the function's.
		if node.bus == fdt::NodeBus::Pci {
			match node.pci_function() {
				Some((bus, dev, func)) => crate::firmware::tree_companion(&identity[..len], bus, dev, func),
				None => crate::serial_println!("device: {name} is a PCI child whose reg names no function - not joined"),
			}
			continue;
		}
		if node.reg_refused {
			crate::serial_println!("device: {name} is not published - its reg could not be read or translated to a physical address");
			continue;
		}
		let held = node.compatible().any(|compatible| kernel_held.contains(&compatible)) || node.regs().iter().any(|&(base, _)| base == console_base && node.bus == fdt::NodeBus::Memory);
		let state = if held { abi::PLATFORM_STATE_KERNEL_HELD } else { abi::PLATFORM_STATE_CLAIMABLE };
		let Some(mut description) = platform::Description::new(abi::PLATFORM_SOURCE_TREE, state, &identity[..len]) else { continue };
		let mut whole = true;
		for compatible in node.compatible() {
			whole &= description.add_match(abi::MATCH_ID_COMPATIBLE, compatible);
		}
		let tpm = node.is_compatible(b"tcg,tpm-tis-mmio");
		match node.bus {
			fdt::NodeBus::Memory => {
				for &(base, size) in node.regs() {
					whole &= description.add_mmio(base, if tpm { 0x1000 } else { size });
					if tpm {
						break;
					}
				}
			}
			fdt::NodeBus::I2c | fdt::NodeBus::Spi => {
				let kind = if node.bus == fdt::NodeBus::I2c { abi::CONNECTION_I2C } else { abi::CONNECTION_SPI };
				for &(address, _) in node.regs() {
					whole &= description.add_connection(abi::Connection { kind, trigger: 0, polarity: 0, _pad: 0, controller: u32::MAX, value: address as u32, extra: 0 });
				}
			}
			// A PCI child was joined to its function above and never reaches here.
			fdt::NodeBus::Pci | fdt::NodeBus::Other => {}
		}
		// THE CONTROLLER OF EACH CONNECTION, by identity: an I2C or SPI device's bus is the node containing it.
		let mut targets: Vec<(u8, Vec<u8>)> = Vec::new();
		if matches!(node.bus, fdt::NodeBus::I2c | fdt::NodeBus::Spi)
			&& let Some(cut) = identity[..len].iter().rposition(|byte| *byte == b'/')
		{
			for at in 0..description.part.connection_count {
				// ALLOC-OK: boot, one per connection a node names.
				targets.push((at, identity[..cut].to_vec()));
			}
		}
		let mut unresolved = false;
		if !tpm {
			for interrupt in node.interrupts() {
				match interrupt {
					fdt::NodeInterrupt::Controller(route) => match line(route) {
						Some(wired) => whole &= description.add_line(wired),
						None => unresolved = true,
					},
					fdt::NodeInterrupt::GpioLine { controller, line, flags } => {
						let at = description.part.connection_count;
						whole &= description.add_connection(abi::Connection { kind: abi::CONNECTION_GPIO_LINE, trigger: (*flags & 0xff) as u8, polarity: 0, _pad: 0, controller: u32::MAX, value: *line, extra: *controller });
						if let Some((_, identity)) = controllers.iter().find(|(phandle, _)| phandle == controller) {
							// ALLOC-OK: as above.
							targets.push((at, identity.clone()));
						}
					}
					fdt::NodeInterrupt::Unresolved => unresolved = true,
				}
			}
		}
		if !whole {
			crate::serial_println!("device: {name} is not published - it names more ids or resources than a row carries, and part of a device is not published");
			continue;
		}
		// A NODE WITH NOTHING TO OWN IS NOT A DEVICE ROW: no range, no line, no connection. The CPUs, the
		// PSCI node and the clock nodes are described by the tree and owned by nobody.
		if description.part.mmio_count == 0 && description.part.line_count == 0 && description.part.connection_count == 0 {
			continue;
		}
		let block = tree.property_block(node, &mut properties);
		if block.unresolved || unresolved {
			description.part.flags |= abi::PLATFORM_FLAG_UNRESOLVED;
		}
		// A NODE WHOSE `iommus` NAMES A STREAM masters the bus behind an IOMMU: the stream is the row's, and its claim is
		// attached like a PCI endpoint where this kernel's IOMMU driver serves that controller - refused by name elsewhere.
		if let Some(stream) = node.dma_stream {
			description.part.flags |= abi::PLATFORM_FLAG_DMA_STREAM;
			description.part.dma_stream = stream;
		}
		// ALLOC-OK: boot, once per published node; the block is bounded by `MAX_DEVICE_PROPERTIES`.
		out.push(Described { description, properties: properties[..block.len].to_vec(), targets, registers: Vec::new() });
	}
	out
}
