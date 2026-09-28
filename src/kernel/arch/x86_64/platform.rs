// THE DEVICES THIS MACHINE'S FIRMWARE DESCRIBES AND NOTHING ANNOUNCES, as the descriptions `device::init`
// publishes, in the order it publishes them: what the kernel declares because it drives it - the legacy
// chipset, the interrupt controllers, the PCI host, COM1 - then what the static ACPI tables name.
//
// THE KERNEL-HELD SET IS PUBLISHED SO THE MACHINE IS ACCOUNTED FOR, and is never claimable. COM1 is held
// until its handoff releases it. The ISA DMA controller's channel registers are held and its page
// registers (0x80..0x8F) are not: they master nothing and firmware methods use them.

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
	out.push(Described { description, properties: Vec::new(), targets: Vec::new() });
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
	// COM1, the kernel's console: its ports and ISA IRQ 4 at the Global System Interrupt the MADT moves it to.
	if let Some(mut com1) = held(b"kernel:com1") {
		com1.add_match(abi::MATCH_ID_HID, b"PNP0501");
		com1.add_port(0x3f8, 8);
		com1.add_line(isa_line(madt.as_ref(), 4));
		push(&mut out, com1);
	}
	tables(&mut out, rsdp, madt.as_ref());
	crate::arch::common::platform::report_smbios(crate::boot_info().smbios, |phys| crate::mem::hhdm_offset() + phys);
	out
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
	// that holds it can hold the chipset's interrupt routing too - so it is left out of the row and said.
	if let Some(bytes) = crate::smp::acpi_table(rsdp, b"WDAT") {
		let len = platform::table_identity(b"WDAT", 0, &mut name);
		if let Some(mut description) = platform::Description::new(abi::PLATFORM_SOURCE_TABLE, abi::PLATFORM_STATE_CLAIMABLE, &name[..len]) {
			description.add_match(abi::MATCH_ID_TABLE, b"WDAT");
			let mut ports: Vec<(u16, u16)> = Vec::new();
			let mut memory = 0usize;
			let _ = acpi::wdat_instructions(bytes, |entry| {
				let Ok(entry) = entry else { return };
				match entry.register.space {
					acpi::AddressSpace::SystemIo if entry.register.address <= 0xffff => {
						let base = entry.register.address as u16;
						let width = entry.register.access.bytes().unwrap_or(u64::from(entry.register.bit_width.div_ceil(8)).max(1)) as u16;
						// ALLOC-OK: boot, bounded by the instructions the table declares.
						ports.push((base, width));
					}
					acpi::AddressSpace::SystemMemory => memory += 1,
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
			if memory != 0 {
				crate::serial_println!("device: the WDAT table names {memory} memory register(s), which are never mapped for a claimant - a page such as the chipset's also holds its interrupt routing - so they are not in its row");
			}
			if whole {
				push(out, description);
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
