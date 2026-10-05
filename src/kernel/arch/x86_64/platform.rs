// THE DEVICES THIS MACHINE'S FIRMWARE DESCRIBES AND NOTHING ANNOUNCES, as the descriptions `device::init`
// publishes, in the order it publishes them: what the kernel declares because it drives it - the legacy
// chipset, the interrupt controllers, the PCI host, COM1 - then what the static ACPI tables name.
//
// THE KERNEL-HELD SET IS PUBLISHED SO THE MACHINE IS ACCOUNTED FOR, and is never claimable. COM1 is a kernel-declared
// row a claim may take: that claim is the console's handoff to a driver. So are the fixed-hardware buttons, which hold
// nothing: their claim is where DeviceManager hands their presses. The ISA DMA controller's channel registers are held
// and its page registers (0x80..0x8F) are not: they master nothing and firmware methods use them.

use alloc::vec::Vec;

use crate::device::Described;

// One kernel-held or kernel-declared device: its identity, ports, MMIO and match id.
struct Held {
	identity: &'static [u8],
	ports: &'static [(u16, u16)],
	mmio: &'static [(u64, u64)],
}

// The legacy devices every PC has at fixed addresses.
const LEGACY: [Held; 5] = [
	Held { identity: b"kernel:pic", ports: &[(0x20, 2), (0xa0, 2), (0x4d0, 2)], mmio: &[] },
	Held { identity: b"kernel:pit", ports: &[(0x40, 4)], mmio: &[] },
	Held { identity: b"kernel:speaker", ports: &[(0x61, 1)], mmio: &[] },
	Held { identity: b"kernel:cmos", ports: &[(0x70, 2)], mmio: &[] },
	Held { identity: b"kernel:isa-dma", ports: &[(0x00, 16), (0xc0, 32)], mmio: &[] },
];

fn held(identity: &[u8]) -> Option<platform::Description> {
	platform::Description::new(abi::PLATFORM_SOURCE_KERNEL, abi::PLATFORM_STATE_KERNEL_HELD, identity)
}

fn push(out: &mut Vec<Described>, description: platform::Description) {
	// ALLOC-OK: boot, once per device the firmware describes.
	out.push(Described { description, properties: Vec::new(), targets: Vec::new(), registers: Vec::new() });
}

// Every description this machine's firmware gives, in publication order.
pub fn describe() -> Vec<Described> {
	let mut out = Vec::new();
	let rsdp = crate::boot_info().rsdp;
	for device in LEGACY.iter() {
		let Some(mut description) = held(device.identity) else { continue };
		for &(base, len) in device.ports {
			description.add_port(base, len);
		}
		for &(base, len) in device.mmio {
			description.add_mmio(base, len);
		}
		push(&mut out, description);
	}
	// fw_cfg: its selector and data ports, and the DMA address register QEMU puts beside them.
	if let Some(mut fw_cfg) = held(b"kernel:fw-cfg") {
		fw_cfg.add_port(0x510, 2);
		fw_cfg.add_port(0x514, 8);
		push(&mut out, fw_cfg);
	}
	// THE LOCAL APIC WINDOW, every core's register page at one physical address.
	if let Some(mut lapic) = held(b"kernel:lapic") {
		lapic.add_mmio(0xfee0_0000, 0x1000);
		push(&mut out, lapic);
	}
	let madt = crate::smp::acpi_table(rsdp, b"APIC").and_then(|bytes| acpi::Madt::new(bytes).ok());
	// EVERY I/O APIC THE MADT NAMES, each a row of the kernel's.
	if let Some(madt) = madt.as_ref() {
		for (at, io_apic) in madt.io_apics().enumerate() {
			let mut name = [0u8; abi::PLATFORM_NAME_LEN];
			// ALLOC-OK: boot, one name per I/O APIC the MADT lists, before userspace exists.
			let body = alloc::format!("ioapic#{at}");
			let Some(len) = platform::identity(b"kernel:", body.as_bytes(), &mut name) else { continue };
			let Some(mut description) = held(&name[..len]) else { continue };
			description.add_mmio(u64::from(io_apic.address), 0x1000);
			push(&mut out, description);
		}
	}
	// THE PCI HOST: the legacy configuration ports, and the ECAM windows the MCFG table places.
	if let Some(mut host) = held(b"kernel:pci-host") {
		host.add_port(0xcf8, 8);
		if let Some(mcfg) = crate::smp::acpi_table(rsdp, b"MCFG") {
			let mut at = 44;
			while at + 16 <= mcfg.len() {
				let entry = &mcfg[at..at + 16];
				let base = u64::from_le_bytes(entry[0..8].try_into().unwrap_or([0; 8]));
				let (first, last) = (u64::from(entry[10]), u64::from(entry[11]));
				if base != 0 && last >= first {
					host.add_mmio(base + (first << 20), (last - first + 1) << 20);
				}
				at += 16;
			}
		}
		push(&mut out, host);
	}
	// THE HPET THE TABLE DESCRIBES, which this kernel holds and does not drive: its window is accounted for.
	if let Some(hpet) = crate::smp::acpi_table(rsdp, b"HPET").and_then(|bytes| acpi::Hpet::new(bytes).ok()) {
		let mut name = [0u8; abi::PLATFORM_NAME_LEN];
		let len = platform::table_identity(b"HPET", 0, &mut name);
		if let Some(mut description) = platform::Description::new(abi::PLATFORM_SOURCE_TABLE, abi::PLATFORM_STATE_KERNEL_HELD, &name[..len]) {
			description.add_match(abi::MATCH_ID_TABLE, b"HPET");
			description.add_mmio(hpet.base.address, 0x400);
			push(&mut out, description);
		}
	}
	// EVERY IOMMU UNIT THE FIRMWARE NAMES - Intel's DMAR hardware units and AMD's IVRS hardware definitions -
	// each a device that remaps DMA and that no driver may take, accounted for by its register window.
	for signature in [b"DMAR", b"IVRS"] {
		let Some(bytes) = crate::smp::acpi_table(rsdp, signature) else { continue };
		let mut units: Vec<acpi::IommuUnit> = Vec::new();
		// ALLOC-OK: boot, once per unit the firmware names.
		let walked = if signature == b"DMAR" { acpi::dmar_units(bytes, |unit| units.push(unit)) } else { acpi::ivrs_units(bytes, |unit| units.push(unit)) };
		let table = core::str::from_utf8(signature).unwrap_or("?");
		if walked.is_err() {
			crate::serial_println!("device: the {table} table ends in a structure that cannot be read - the units before it are published");
		}
		// AND THE NAMESPACE DEVICES A DMAR PUTS BEHIND ITS UNITS: their requester ids, attached to their rows when the
		// ACPI service publishes them.
		if signature == b"DMAR" && acpi::dmar_namespace_streams(bytes, |stream| crate::firmware::note_namespace_stream(stream.name(), stream.source_id)).is_err() {
			crate::serial_println!("device: the DMAR's namespace-device scopes end in a structure that cannot be read");
		}
		for (index, unit) in units.iter().enumerate() {
			let mut name = [0u8; abi::PLATFORM_NAME_LEN];
			// ALLOC-OK: boot, one name per unit the table lists, before userspace exists.
			let body = alloc::format!("{table}#0.{index}");
			if let Some(len) = platform::identity(b"table:", body.as_bytes(), &mut name)
				&& let Some(mut description) = platform::Description::new(abi::PLATFORM_SOURCE_TABLE, abi::PLATFORM_STATE_KERNEL_HELD, &name[..len])
			{
				description.add_match(abi::MATCH_ID_TABLE, signature);
				description.add_mmio(unit.base, unit.len);
				push(&mut out, description);
			}
		}
	}
	// COM1, the kernel's console: its ports and ISA IRQ 4 at the Global System Interrupt the MADT moves it to.
	// THE ONE KERNEL-DECLARED ROW THAT IS CLAIMABLE, and its claim is the console's handoff to a driver: the
	// kernel drives it until then and takes it back at the release. The test build keeps it the kernel's - the
	// suite is judged by what COM1 carries, and it exercises the handoff on its second UART instead.
	#[cfg(not(test))]
	let com1_state = abi::PLATFORM_STATE_CLAIMABLE;
	#[cfg(test)]
	let com1_state = abi::PLATFORM_STATE_KERNEL_HELD;
	if let Some(mut com1) = platform::Description::new(abi::PLATFORM_SOURCE_KERNEL, com1_state, b"kernel:com1") {
		com1.part.flags |= abi::PLATFORM_FLAG_CONSOLE;
		com1.add_match(abi::MATCH_ID_HID, b"PNP0501");
		com1.add_port(0x3f8, 8);
		com1.add_line(isa_line(madt.as_ref(), 4));
		push(&mut out, com1);
	}
	// THE FIXED-HARDWARE BUTTONS THE FADT DESCRIBES, each a claimable row with no resource of its own: a press is a PM1
	// status bit the SCI's handler decodes and reports, and DeviceManager hands it to the binding that holds the row.
	let fixed = crate::smp::acpi_table(rsdp, b"FACP").and_then(|bytes| acpi::Fadt::new(bytes).ok()).map_or(0, |fadt| fixed_of(&fadt));
	for (bit, identity, hid) in [
		(abi::SLEEP_FIXED_POWER_BUTTON, abi::PLATFORM_ROW_POWER_BUTTON, abi::PLATFORM_HID_POWER_BUTTON),
		(abi::SLEEP_FIXED_SLEEP_BUTTON, abi::PLATFORM_ROW_SLEEP_BUTTON, abi::PLATFORM_HID_SLEEP_BUTTON),
	] {
		if fixed & bit == 0 {
			continue;
		}
		if let Some(mut button) = platform::Description::new(abi::PLATFORM_SOURCE_KERNEL, abi::PLATFORM_STATE_CLAIMABLE, identity) {
			button.add_match(abi::MATCH_ID_HID, hid);
			push(&mut out, button);
		}
	}
	tables(&mut out, rsdp, madt.as_ref());
	crate::arch::common::platform::report_smbios(crate::boot_info().smbios, |phys| crate::mem::hhdm_offset() + phys);
	out
}

/// THE FIXED BUTTONS A FADT DESCRIBES, as `SLEEP_FIXED_*` bits: none on a hardware-reduced machine or one whose PM1a
/// event block is not in port I/O, since no press of theirs could be read. The kernel's platform rows and the SCI's
/// arming read the same answer.
pub fn fixed_of(fadt: &acpi::Fadt<'_>) -> u64 {
	if fadt.hardware_reduced() || fadt.pm1a_event().and_then(|event| event.io_port()).is_none() {
		return 0;
	}
	let flags: u32 = fadt.flags().unwrap_or(0);
	(if flags & (1 << 4) == 0 { abi::SLEEP_FIXED_POWER_BUTTON } else { 0 }) | (if flags & (1 << 5) == 0 { abi::SLEEP_FIXED_SLEEP_BUTTON } else { 0 })
}

// An ISA IRQ as the wired line it arrives on: its Global System Interrupt and configuration from the MADT's
// override, else the ISA bus's own - edge, active high, at the GSI of its own number.
fn isa_line(madt: Option<&acpi::Madt<'_>>, irq: u8) -> abi::WiredLine {
	let over = madt.and_then(|madt| madt.isa_override(irq));
	let gsi = over.map_or(u32::from(irq), |over| over.gsi);
	let level = over.is_some_and(|over| over.trigger == acpi::Trigger::Level);
	let low = over.is_some_and(|over| over.polarity == acpi::Polarity::ActiveLow);
	abi::WiredLine { number: gsi, trigger: if level { abi::LINE_TRIGGER_LEVEL } else { abi::LINE_TRIGGER_EDGE }, polarity: if low { abi::LINE_POLARITY_LOW } else { abi::LINE_POLARITY_HIGH }, controller: abi::LINE_CONTROLLER_IOAPIC, _pad: 0 }
}

// An ISA IRQ's wired line on this machine, and a PCI function's INTx line by the ISA IRQ its firmware wrote in
// its interrupt-line register - LEVEL, active low unless the MADT's override for that IRQ says high - for the
// suite, which claims both as platform rows' lines.
#[cfg(test)]
fn test_madt() -> Option<acpi::Madt<'static>> {
	crate::smp::acpi_table(crate::boot_info().rsdp, b"APIC").and_then(|bytes| acpi::Madt::new(bytes).ok())
}

#[cfg(test)]
pub fn isa_irq_line(irq: u8) -> abi::WiredLine {
	isa_line(test_madt().as_ref(), irq)
}

#[cfg(test)]
pub fn pci_intx_line(irq: u8) -> abi::WiredLine {
	let over = test_madt().and_then(|madt| madt.isa_override(irq));
	let high = over.is_some_and(|over| over.polarity == acpi::Polarity::ActiveHigh);
	abi::WiredLine { number: over.map_or(u32::from(irq), |over| over.gsi), trigger: abi::LINE_TRIGGER_LEVEL, polarity: if high { abi::LINE_POLARITY_HIGH } else { abi::LINE_POLARITY_LOW }, controller: abi::LINE_CONTROLLER_IOAPIC, _pad: 0 }
}

// A register window a table describes, added to `description` in its own address space.
fn add_window(description: &mut platform::Description, gas: &acpi::Gas, io_len: u16) -> bool {
	match gas.space {
		acpi::AddressSpace::SystemIo if gas.address <= 0xffff => description.add_port(gas.address as u16, io_len),
		acpi::AddressSpace::SystemMemory if gas.address != 0 => description.add_mmio(gas.address & !0xfff, 0x1000),
		_ => false,
	}
}

// THE STATIC TABLES, each into what it is.
fn tables(out: &mut Vec<Described>, rsdp: u64, madt: Option<&acpi::Madt<'_>>) {
	let mut name = [0u8; abi::PLATFORM_NAME_LEN];
	// THE TPM: one MMIO range, locality 0's page; no interrupt, since the transport polls; no DMA.
	if let Some(bytes) = crate::smp::acpi_table(rsdp, b"TPM2") {
		match tpm::table::discover(bytes) {
			Ok(interface) => {
				let base = match interface {
					tpm::table::Interface::Fifo { base } | tpm::table::Interface::Crb { base } => base,
				};
				let len = platform::table_identity(b"TPM2", 0, &mut name);
				if let Some(mut description) = platform::Description::new(abi::PLATFORM_SOURCE_TABLE, abi::PLATFORM_STATE_CLAIMABLE, &name[..len]) {
					description.add_match(abi::MATCH_ID_TABLE, b"TPM2");
					description.add_mmio(base, 0x1000);
					push(out, description);
				}
			}
			Err(tpm::table::Refusal::Unsupported(method)) => crate::serial_println!("device: the TPM2 table names start method {method}, which this kernel does not describe - no device"),
			Err(refusal) => crate::serial_println!("device: the TPM2 table is refused - {refusal:?}"),
		}
	}
	// THE CONSOLE THE FIRMWARE REDIRECTED TO: the UART description it is, merged into that UART's row.
	if let Some(spcr) = crate::smp::acpi_table(rsdp, b"SPCR").and_then(|bytes| acpi::Spcr::new(bytes).ok()) {
		let len = platform::table_identity(b"SPCR", 0, &mut name);
		if let Some(mut description) = platform::Description::new(abi::PLATFORM_SOURCE_TABLE, abi::PLATFORM_STATE_CLAIMABLE, &name[..len]) {
			if add_window(&mut description, &spcr.base, 8) {
				if spcr.interrupt_type & 0b10 != 0 {
					description.add_line(abi::WiredLine { number: spcr.gsi, trigger: abi::LINE_TRIGGER_EDGE, polarity: abi::LINE_POLARITY_HIGH, controller: abi::LINE_CONTROLLER_IOAPIC, _pad: 0 });
				} else if spcr.interrupt_type & 0b1 != 0 {
					description.add_line(isa_line(madt, spcr.irq));
				}
				push(out, description);
			}
		}
	}
	// THE DEBUG PORTS, each serial one a UART description.
	for instance in 0..4 {
		let Some(bytes) = crate::smp::acpi_table_instance(rsdp, b"DBG2", instance) else { break };
		let mut index = 0u32;
		let _ = acpi::dbg2_devices(bytes, |device| {
			if device.port_type == acpi::DBG2_TYPE_SERIAL {
				// ALLOC-OK: boot, one name per serial port the DBG2 tables list, before userspace exists.
				let body = alloc::format!("DBG2#{instance}.{index}");
				if let Some(len) = platform::identity(b"table:", body.as_bytes(), &mut name)
					&& let Some(mut description) = platform::Description::new(abi::PLATFORM_SOURCE_TABLE, abi::PLATFORM_STATE_CLAIMABLE, &name[..len])
					&& add_window(&mut description, &device.base, 8)
				{
					push(out, description);
				}
			}
			index += 1;
		});
	}
	// THE WATCHDOG ACTION TABLE: a device whose resources are the registers its instructions name. Its I/O
	// registers are port ranges, merged where they touch. A memory register is NEVER a mapping - the page
	// that holds it can hold the chipset's interrupt routing too - so it is a DECLARED register of the row,
	// reached one access at a time at its width.
	if let Some(bytes) = crate::smp::acpi_table(rsdp, b"WDAT") {
		let len = platform::table_identity(b"WDAT", 0, &mut name);
		if let Some(mut description) = platform::Description::new(abi::PLATFORM_SOURCE_TABLE, abi::PLATFORM_STATE_CLAIMABLE, &name[..len]) {
			description.add_match(abi::MATCH_ID_TABLE, b"WDAT");
			let mut ports: Vec<(u16, u16)> = Vec::new();
			let mut memory: Vec<(u64, u8)> = Vec::new();
			let _ = acpi::wdat_instructions(bytes, |entry| {
				let Ok(entry) = entry else { return };
				match entry.register.space {
					acpi::AddressSpace::SystemIo if entry.register.address <= 0xffff => {
						let base = entry.register.address as u16;
						let width = u16::from(acpi::wdat_register_width(&entry.register));
						// ALLOC-OK: boot, bounded by the instructions the table declares.
						ports.push((base, width));
					}
					acpi::AddressSpace::SystemMemory => {
						let width = acpi::wdat_register_width(&entry.register);
						if !memory.contains(&(entry.register.address, width)) {
							// ALLOC-OK: boot, bounded by the instructions the table declares.
							memory.push((entry.register.address, width));
						}
					}
					_ => {}
				}
			});
			ports.sort();
			let mut merged: Vec<(u16, u16)> = Vec::new();
			for (base, width) in ports {
				match merged.last_mut() {
					Some(last) if u32::from(base) <= u32::from(last.0) + u32::from(last.1) => last.1 = last.1.max((base + width).saturating_sub(last.0)),
					// ALLOC-OK: as above.
					_ => merged.push((base, width)),
				}
			}
			let whole = merged.iter().all(|&(base, width)| description.add_port(base, width));
			// THE TABLE ITSELF IS THE ROW'S PROPERTY BLOCK, one VALUE record named `WDAT`: its driver runs the
			// instructions, and nothing else carries them to userspace.
			let padded = (bytes.len() + 3) & !3;
			let mut block: Vec<u8> = Vec::new();
			// Boot, once, bounded by the block's limit - and the room asked for first, so a short heap leaves it unpublished.
			if 12 + padded <= abi::MAX_DEVICE_PROPERTIES && block.try_reserve_exact(12 + padded).is_ok() {
				block.push(abi::DEVICE_PROPERTY_VALUE);
				block.push(0);
				block.extend_from_slice(&4u16.to_le_bytes());
				block.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
				block.extend_from_slice(b"WDAT");
				block.extend_from_slice(bytes);
				block.resize(12 + padded, 0);
			}
			if block.is_empty() {
				crate::serial_println!("device: the WDAT table is longer than a row's property block, or there is no memory for it - not published");
			} else if whole {
				// ALLOC-OK: boot, once per device the firmware describes.
				out.push(Described { description, properties: block, targets: Vec::new(), registers: memory });
			} else {
				crate::serial_println!("device: the WDAT table names more port ranges than a row carries - not published");
			}
		}
	}
	// THE BOOT LOGO, a fact about the boot and never a device.
	if let Some(bgrt) = crate::smp::acpi_table(rsdp, b"BGRT").and_then(|bytes| acpi::Bgrt::new(bytes).ok()) {
		crate::serial_println!("firmware: the boot logo is an image of type {} at {:#x}, drawn at ({}, {}){}", bgrt.image_type, bgrt.image_address, bgrt.x, bgrt.y, if bgrt.status & 1 != 0 { "" } else { " and not displayed" });
	}
}
