// THE FIRMWARE INTERPRETER'S POLICY AND REPORTS, as pure functions: both admissions and the carve-out's two, a claim
// refused under another node's region, the SystemIO rules around the DMA controller and the POST port, a
// reservation over kernel-held MMIO and reserved ports recorded rather than refused, the merge and reservation
// rules, a walk's reconciliation by identity, the SMBIOS type-38 attach - and every report round-tripped, a
// hostile one refused.

extern crate std;
use std::vec::Vec;

use super::policy::*;
use super::report::{self, CompanionReport, DeviceReport, Function, List, Report};
use super::*;

const ECAM: (u64, u64) = (0xB000_0000, 0x1000_0000);

fn ivshmem() -> Function {
	Function { segment: 0, bus: 0, device: 0x10, function: 0 }
}

fn nic() -> Function {
	Function { segment: 0, bus: 0, device: 3, function: 0 }
}

fn bar_set(driver_held_nic: bool, firmware_held_fixture: bool) -> Vec<Bar> {
	std::vec![
		Bar { base: 0xFE00_0000, len: 0x1_0000, function: nic(), window: false, driver_held: driver_held_nic, firmware_held: false, fixture: false },
		Bar { base: 0xFD00_0000, len: 0x10_0000, function: ivshmem(), window: false, driver_held: false, firmware_held: firmware_held_fixture, fixture: true },
	]
}

fn regions() -> Vec<(u64, u64, MemoryKind)> {
	std::vec![
		(0, 0x9F000, MemoryKind::Ram),
		(0x9F000, 0x1000, MemoryKind::FirmwareReserved),
		(0x10_0000, 0x7EF0_0000, MemoryKind::Ram),
		(0x7FF0_0000, 0x8000, MemoryKind::AcpiReclaimable),
		(0x7FF0_8000, 0x8000, MemoryKind::AcpiNvs),
		(0xFFC0_0000, 0x40_0000, MemoryKind::Mmio)
	]
}

#[test]
fn system_memory_maps_firmware_memory_write_back_and_mmio_uncached_and_nothing_else() {
	let regions = regions();
	let bars = bar_set(false, false);
	let view = MemoryView { regions: &regions, kernel_held: &[ECAM, (0xFEE0_0000, 0x1000)], bars: &bars, claimed: &[], carve_out: false };
	assert_eq!(system_memory(&view, 0x7FF0_8000, 0x100, b"acpi:\\_SB_.EC0_", None), Ok(Admission::WriteBack), "ACPI NVS");
	assert_eq!(system_memory(&view, 0x9F000, 0x10, b"acpi:\\_SB_", None), Ok(Admission::WriteBack), "firmware-reserved");
	assert_eq!(system_memory(&view, 0xFED0_0000, 0x400, b"acpi:\\_SB_.HPET", None), Ok(Admission::Uncached), "a hole in the map is MMIO");
	assert_eq!(system_memory(&view, 0xFFC0_0000, 0x1000, b"acpi:\\_SB_", None), Ok(Admission::Uncached), "reported MMIO");
	assert_eq!(system_memory(&view, 0x20_0000, 0x1000, b"acpi:\\_SB_", None), Err(Refusal::Ram));
	assert_eq!(system_memory(&view, 0x7FF0_F000, 0x2000, b"acpi:\\_SB_", None), Err(Refusal::Mixed), "NVS running into a hole");
	assert_eq!(system_memory(&view, 0xB000_0000, 0x1000, b"acpi:\\_SB_", None), Err(Refusal::KernelHeld), "the ECAM");
	assert_eq!(system_memory(&view, 0xFEE0_0000, 4, b"acpi:\\_SB_", None), Err(Refusal::KernelHeld), "the LAPIC");
	assert_eq!(system_memory(&view, 0xFE00_0100, 4, b"acpi:\\_SB_", None), Err(Refusal::Bar(nic())));
	assert_eq!(system_memory(&view, 0, 0, b"x", None), Err(Refusal::Empty));
}

#[test]
fn the_two_admissions_and_a_claim_under_another_nodes_region() {
	let regions = regions();
	let claimed = [Claimed { base: 0xFED4_0000, len: 0x1000, node: b"acpi:\\_SB_.UCSI" }];
	let bars = bar_set(false, false);
	let view = MemoryView { regions: &regions, kernel_held: &[], bars: &bars, claimed: &claimed, carve_out: false };
	// ADMISSION ONE: the claimed device's own node, inside its own range.
	assert_eq!(system_memory(&view, 0xFED4_0000, 0x100, b"acpi:\\_SB_.UCSI", None), Ok(Admission::Uncached));
	// Any other node over the claimed range is refused, naming the claim.
	assert_eq!(system_memory(&view, 0xFED4_0080, 0x10, b"acpi:\\_SB_.PCI0.LPCB", None), Err(Refusal::Claimed(0)));
	// ADMISSION TWO: the companion's BAR of a function no driver holds - the function becomes firmware-held.
	assert_eq!(system_memory(&view, 0xFE00_0000, 0x100, b"acpi:\\_SB_.PCI0.S18_", Some(nic())), Ok(Admission::FirmwareHold(nic())));
	let held = bar_set(true, false);
	let view = MemoryView { bars: &held, ..view };
	assert_eq!(system_memory(&view, 0xFE00_0000, 0x100, b"acpi:\\_SB_.PCI0.S18_", Some(nic())), Err(Refusal::DriverHeld(nic())), "not while a driver holds it");
	assert_eq!(system_memory(&view, 0xFE00_0000, 0x100, b"acpi:\\_SB_.PCI0.S20_", Some(ivshmem())), Err(Refusal::Bar(nic())), "and never another function's BAR");
	// THE REVERSE: a claim refused while another node's region maps any part of its ranges.
	let mappings: [(u64, u64, &[u8]); 2] = [(0xFED4_0000, 0x10, b"acpi:\\_SB_.UCSI"), (0xFED4_0800, 0x10, b"acpi:\\_SB_.OTHR")];
	assert_eq!(claim_blocked(&[(0xFED4_0000, 0x1000)], b"acpi:\\_SB_.UCSI", &mappings), Some(1));
	assert_eq!(claim_blocked(&[(0xFED4_0000, 0x100)], b"acpi:\\_SB_.UCSI", &mappings), None, "its own node's region does not block it");
}

#[test]
fn the_fixture_carve_out_admits_its_two_in_a_development_build_only() {
	let regions = regions();
	let claimed = [Claimed { base: 0xFD00_8000, len: 0x1000, node: b"acpi:\\_SB_.LSF1" }];
	let bars = bar_set(false, true);
	let shipping = MemoryView { regions: &regions, kernel_held: &[], bars: &bars, claimed: &claimed, carve_out: false };
	let development = MemoryView { carve_out: true, ..shipping };
	// A fixture device's own node, inside its claimed range inside the firmware-held fixture BAR.
	assert_eq!(system_memory(&development, 0xFD00_8000, 0x100, b"acpi:\\_SB_.LSF1", None), Ok(Admission::Uncached));
	assert_eq!(system_memory(&shipping, 0xFD00_8000, 0x100, b"acpi:\\_SB_.LSF1", None), Err(Refusal::Bar(ivshmem())), "every shipping build refuses it");
	// Another node over that range is refused in both.
	assert_eq!(system_memory(&development, 0xFD00_8000, 0x10, b"acpi:\\_SB_.LSF2", None), Err(Refusal::Claimed(0)));
	// A `_CRS` range inside the fixture BAR: a row in development, refused in shipping.
	let mut row = Description::new(abi::PLATFORM_SOURCE_ACPI, abi::PLATFORM_STATE_CLAIMABLE, b"acpi:\\_SB_.LSF1").unwrap();
	assert!(row.add_mmio(0xFD00_8000, 0x1000));
	assert_eq!(crs(&row, &development, |_, _| true, &[], &[]), Ok(()));
	assert_eq!(crs(&row, &shipping, |_, _| true, &[], &[]), Err(CrsRefusal::MmioOverBar(ivshmem())));
}

#[test]
fn system_io_and_configuration_writes_keep_to_the_rules() {
	// THE ISA DMA CONTROLLERS' registers are in the fixed reserved set; the page registers and the POST port are not.
	let recordable = |base: u16, len: u16| {
		let end = base as u32 + len as u32;
		!((base as u32) < 0x20 || (base as u32) < 0xE0 && end > 0xC0)
	};
	assert!(!system_io(0x00, 0x10, recordable, &[]), "a DMA channel register");
	assert!(!system_io(0xC0, 0x20, recordable, &[]), "the second controller");
	assert!(system_io(0x80, 0x10, recordable, &[]), "the POST port and the page registers");
	assert!(!system_io(0x3F8, 8, |_, _| true, &[(0x3F8, 8)]), "inside a live claim's ports");
	assert!(!system_io(0xFFF8, 0x10, |_, _| true, &[]), "past the port space");
	// Configuration writes.
	assert_eq!(config_write(0x04, 2, &[], false, &[]), Err(ConfigRefusal::Header));
	assert_eq!(config_write(0x50, 2, &[(0x50, 0x18)], false, &[]), Err(ConfigRefusal::Msi));
	assert_eq!(config_write(0x80, 4, &[], true, &[]), Err(ConfigRefusal::DriverHeld));
	assert_eq!(config_write(0x40, 4, &[], false, &[(0x40, 4)]), Err(ConfigRefusal::Chipset), "the PM base");
	assert_eq!(config_write(0x41, 2, &[], false, &[]), Err(ConfigRefusal::Width));
	assert_eq!(config_write(0xA0, 1, &[(0x50, 0x18)], false, &[(0x40, 4)]), Ok(()));
}

#[test]
fn a_reservation_mints_nothing_so_its_resources_are_recorded_and_it_keeps_later_rows_out() {
	// q35's DRAC over the MCFG window - the ECAM, which is kernel-held: a reservation is recorded; the same range as a
	// device's resource is refused.
	let regions = regions();
	let view = MemoryView { regions: &regions, kernel_held: &[ECAM], bars: &[], claimed: &[], carve_out: false };
	let mut drac = Description::new(abi::PLATFORM_SOURCE_ACPI, abi::PLATFORM_STATE_RESERVATION, b"acpi:\\_SB_.DRAC").unwrap();
	assert!(drac.add_mmio(ECAM.0, ECAM.1));
	assert!(drac.add_port(0x61, 1));
	assert_eq!(crs(&drac, &view, |_, _| false, &[], &[]), Err(CrsRefusal::MmioKernelHeld), "the check would refuse it as a device ...");
	// ... and the kernel does not run it for a reservation: what it publishes is the row in the reservation state.
	assert_eq!(drac.part.state, abi::PLATFORM_STATE_RESERVATION);
	// A row published AFTER the reservation, inside its ranges: kept out.
	let mut later = Description::new(abi::PLATFORM_SOURCE_ACPI, abi::PLATFORM_STATE_CLAIMABLE, b"acpi:\\_SB_.LATE").unwrap();
	assert!(later.add_mmio(ECAM.0 + 0x1000, 0x1000));
	assert!(reserved_against(&drac.part, drac.ports(), &later));
	let mut elsewhere = Description::new(abi::PLATFORM_SOURCE_ACPI, abi::PLATFORM_STATE_CLAIMABLE, b"acpi:\\_SB_.ELSE").unwrap();
	assert!(elsewhere.add_mmio(0xFED0_0000, 0x400));
	assert!(!reserved_against(&drac.part, drac.ports(), &elsewhere));
	// Kernel lines and reserved ports refuse a device.
	let mut uart = Description::new(abi::PLATFORM_SOURCE_ACPI, abi::PLATFORM_STATE_CLAIMABLE, b"acpi:\\_SB_.COM2").unwrap();
	assert!(uart.add_port(0x2F8, 8));
	assert!(uart.add_line(abi::WiredLine { number: 9, trigger: abi::LINE_TRIGGER_LEVEL, polarity: abi::LINE_POLARITY_LOW, controller: abi::LINE_CONTROLLER_IOAPIC, _pad: 0 }));
	assert_eq!(crs(&uart, &view, |_, _| true, &[], &[0, 9]), Err(CrsRefusal::KernelLine(9)), "the SCI's line");
	assert_eq!(crs(&uart, &view, |_, _| false, &[], &[]), Err(CrsRefusal::PortsReserved));
	assert_eq!(crs(&uart, &view, |_, _| true, &[(0x2F0, 0x10)], &[]), Err(CrsRefusal::PortsOverBar));
}

#[test]
fn a_walk_is_reconciled_by_identity() {
	let mut tpm = Description::new(abi::PLATFORM_SOURCE_TABLE, abi::PLATFORM_STATE_CLAIMABLE, b"table:TPM2#0").unwrap();
	assert!(tpm.add_mmio(0xFED4_0000, 0x1000));
	let node = {
		let mut node = Description::new(abi::PLATFORM_SOURCE_ACPI, abi::PLATFORM_STATE_CLAIMABLE, b"acpi:\\_SB_.TPM_").unwrap();
		assert!(node.add_match(abi::MATCH_ID_HID, b"MSFT0101"));
		assert!(node.add_mmio(0xFED4_0000, 0x1000));
		node
	};
	assert!(merge(&mut tpm.part, &node));
	let mut lsfx = Description::new(abi::PLATFORM_SOURCE_ACPI, abi::PLATFORM_STATE_CLAIMABLE, b"acpi:\\_SB_.LSF2").unwrap();
	assert!(lsfx.add_match(abi::MATCH_ID_HID, b"LSFX0002"));
	let rows = [None, Some(RowState { part: &tpm.part, withdrawn: false }), Some(RowState { part: &lsfx.part, withdrawn: false })];
	// The same path reported again - a restart's walk - is the same row, no event; a static row found by the
	// identity merged into it.
	assert_eq!(reconcile(&rows, &lsfx), Reconcile::Same { row: 2, differs: false });
	assert_eq!(reconcile(&rows, &node), Reconcile::Same { row: 1, differs: false });
	// A report differing from the first: the same row, logged, the first kept.
	let mut moved = lsfx;
	assert!(moved.add_mmio(0xFED5_0000, 0x100));
	assert_eq!(reconcile(&rows, &moved), Reconcile::Same { row: 2, differs: true });
	// A withdrawn row is refilled; an unknown path is placed.
	let withdrawn = [None, Some(RowState { part: &tpm.part, withdrawn: false }), Some(RowState { part: &lsfx.part, withdrawn: true })];
	assert_eq!(reconcile(&withdrawn, &lsfx), Reconcile::Refill(2));
	let fresh = Description::new(abi::PLATFORM_SOURCE_ACPI, abi::PLATFORM_STATE_CLAIMABLE, b"acpi:\\_SB_.NEW_").unwrap();
	assert_eq!(reconcile(&rows, &fresh), Reconcile::Place);
}

#[test]
fn the_smbios_type_38_record_an_ipmi_node_agrees_with() {
	let records = [(1u8, 0xCA2u64), (2, 0xE4)];
	assert_eq!(smbios_ipmi(&records, 2, 0xE4), Some(1));
	assert_eq!(smbios_ipmi(&records, 1, 0xCA3), Some(0), "the I/O-space bit of the base address is not part of it");
	assert_eq!(smbios_ipmi(&records, 3, 0xCA2), None);
}

#[test]
fn every_report_round_trips_and_a_hostile_one_is_refused() {
	let mut description = Description::new(abi::PLATFORM_SOURCE_ACPI, abi::PLATFORM_STATE_CLAIMABLE, b"acpi:\\_SB_.I2C1.TPD0").unwrap();
	assert!(description.add_match(abi::MATCH_ID_HID, b"LSFX0003"));
	assert!(description.add_match(abi::MATCH_ID_CID, b"PNP0C50"));
	assert!(description.add_mmio(0xFED4_0000, 0x1000));
	assert!(description.add_port(0x2F8, 8));
	assert!(description.add_line(abi::WiredLine { number: 17, trigger: abi::LINE_TRIGGER_LEVEL, polarity: abi::LINE_POLARITY_LOW, controller: abi::LINE_CONTROLLER_IOAPIC, _pad: 0 }));
	assert!(description.add_connection(abi::Connection { kind: abi::CONNECTION_I2C, trigger: 0, polarity: 0, _pad: 0, controller: u32::MAX, value: 0x2C, extra: 400_000 }));
	assert!(description.add_connection(abi::Connection { kind: abi::CONNECTION_GPIO_LINE, trigger: abi::LINE_TRIGGER_LEVEL, polarity: abi::LINE_POLARITY_LOW, _pad: 0, controller: u32::MAX, value: 3, extra: 0 }));
	let properties = [1u8, 0, 0, 0, 0, 0, 0, 0];
	let device = DeviceReport { description, targets: [(0, b"acpi:\\_SB_.I2C1"), (1, b"acpi:\\_SB_.GPI0"), (0, &[]), (0, &[])], target_count: 2, parent: Some(Function { segment: 0, bus: 0, device: 0x15, function: 0 }), properties: &properties };
	let mut buffer = [0u8; report::MAX_REPORT];
	let length = report::encode_device(&device, &mut buffer).unwrap();
	match report::decode(&buffer[..length]) {
		Some(Report::Device(decoded)) => {
			assert_eq!(decoded.description, device.description);
			assert_eq!(decoded.target_count, 2);
			assert_eq!(decoded.targets[1], (1, &b"acpi:\\_SB_.GPI0"[..]));
			assert_eq!(decoded.parent, device.parent);
			assert_eq!(decoded.properties, &properties);
		}
		other => panic!("{other:?}"),
	}
	// Every other kind.
	let mut aei = List::default();
	assert!(aei.push(2));
	let companion = CompanionReport { path: b"acpi:\\_SB_.PCI0.GPIO", function: Function { segment: 0, bus: 0, device: 0x15, function: 0 }, aei_lines: aei, field_lines: List::default(), field_addresses: List::default() };
	let length = report::encode_companion(&companion, &mut buffer).unwrap();
	assert!(matches!(report::decode(&buffer[..length]), Some(Report::Companion(decoded)) if decoded.path == companion.path && decoded.aei_lines.as_slice() == [2]));
	let length = report::encode_withdraw(b"acpi:\\_SB_.LSF2", &mut buffer).unwrap();
	assert!(matches!(report::decode(&buffer[..length]), Some(Report::Withdraw(path)) if path == b"acpi:\\_SB_.LSF2"));
	let length = report::encode_osc(0, 0, 0xFF, report::osc::HOT_PLUG | report::osc::AER, &mut buffer).unwrap();
	assert!(matches!(report::decode(&buffer[..length]), Some(Report::Osc { granted, bus_end: 0xFF, .. }) if granted == 9));
	let length = report::encode_loaded(7, &mut buffer).unwrap();
	assert!(matches!(report::decode(&buffer[..length]), Some(Report::Loaded { instance: 7 })));
	// HOSTILE: a count past a bound, a truncated report, trailing bytes, an unknown kind.
	let length = report::encode_device(&device, &mut buffer).unwrap();
	assert!(report::decode(&buffer[..length - 1]).is_none(), "truncated");
	let mut trailing = buffer[..length].to_vec();
	trailing.push(0);
	assert!(report::decode(&trailing).is_none(), "trailing bytes");
	let identity_length = 1 + 1 + 1 + 1 + device.description.identity().len();
	let mut too_many = buffer[..length].to_vec();
	too_many[identity_length] = 9;
	assert!(report::decode(&too_many).is_none(), "nine match ids");
	assert!(report::decode(&[9, 0, 0]).is_none());
	assert!(report::decode(&[report::COMPANION, 1, b'x', 0, 0, 0, 0, 0, 33]).is_none(), "a list past its bound");
}
