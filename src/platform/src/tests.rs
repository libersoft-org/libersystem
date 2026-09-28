extern crate std;
use std::{format, vec, vec::Vec};

use super::*;

fn line(number: u32, trigger: u8) -> WiredLine {
	WiredLine { number, trigger, polarity: abi::LINE_POLARITY_HIGH, controller: abi::LINE_CONTROLLER_IOAPIC, _pad: 0 }
}

// The rows as `place` reads them: a platform part and its ports each, PCI rows as None.
fn views<'a>(rows: &'a [Option<Description>]) -> Vec<RowView<'a>> {
	rows.iter().map(|row| row.as_ref().map(|row| (&row.part, row.ports()))).collect()
}

fn tpm_table() -> Description {
	let mut tpm = Description::new(abi::PLATFORM_SOURCE_TABLE, abi::PLATFORM_STATE_CLAIMABLE, b"table:TPM2#0").unwrap();
	assert!(tpm.add_match(abi::MATCH_ID_TABLE, b"TPM2"));
	assert!(tpm.add_mmio(0xfed4_0000, 0x1000));
	tpm
}

fn com1() -> Description {
	let mut com1 = Description::new(abi::PLATFORM_SOURCE_KERNEL, abi::PLATFORM_STATE_KERNEL_HELD, b"kernel:com1").unwrap();
	assert!(com1.add_match(abi::MATCH_ID_HID, b"PNP0501"));
	assert!(com1.add_port(0x3f8, 8));
	assert!(com1.add_line(line(4, abi::LINE_TRIGGER_EDGE)));
	com1
}

#[test]
fn a_description_under_an_identity_a_row_carries_is_that_row() {
	let rows = [None, Some(tpm_table()), Some(com1())];
	let again = Description::new(abi::PLATFORM_SOURCE_KERNEL, abi::PLATFORM_STATE_KERNEL_HELD, b"kernel:com1").unwrap();
	assert_eq!(place(&views(&rows), &again), Placement::Same(2));
	// And an identity a MERGED description brought is the row's too.
	let mut merged = tpm_table();
	let mut node = Description::new(abi::PLATFORM_SOURCE_ACPI, abi::PLATFORM_STATE_CLAIMABLE, b"acpi:\\_SB_.TPM_").unwrap();
	assert!(node.add_mmio(0xfed4_0000, 0x5000));
	assert!(merge(&mut merged.part, &node));
	let rows = [Some(merged)];
	assert_eq!(place(&views(&rows), &node), Placement::Same(0));
}

#[test]
fn the_tpm_table_and_its_namespace_node_are_one_device_that_keeps_the_tables_page() {
	// QEMU's `_CRS` names 0x5000 bytes (every locality) where the table's claim is locality 0's page: both
	// start at 0xFED40000 and one contains the other, so they merge - and the row keeps the one page.
	let rows = [None, Some(tpm_table())];
	let mut node = Description::new(abi::PLATFORM_SOURCE_ACPI, abi::PLATFORM_STATE_CLAIMABLE, b"acpi:\\_SB_.TPM_").unwrap();
	assert!(node.add_match(abi::MATCH_ID_HID, b"MSFT0101"));
	assert!(node.add_mmio(0xfed4_0000, 0x5000));
	assert_eq!(place(&views(&rows), &node), Placement::Merge(1));
	let mut row = rows[1].unwrap();
	assert!(merge(&mut row.part, &node));
	assert_eq!(row.part.mmio(), &[MmioResource { base: 0xfed4_0000, len: 0x1000 }], "the first description's resources");
	let ids: Vec<(u8, &[u8])> = row.part.match_ids().iter().map(|id| (id.kind, id.text())).collect();
	assert_eq!(ids, vec![(abi::MATCH_ID_TABLE, &b"TPM2"[..]), (abi::MATCH_ID_IDENTITY, &b"acpi:\\_SB_.TPM_"[..]), (abi::MATCH_ID_HID, &b"MSFT0101"[..])]);
}

#[test]
fn com1_and_its_namespace_node_are_one_device_through_their_ports() {
	let rows = [Some(com1())];
	let mut node = Description::new(abi::PLATFORM_SOURCE_ACPI, abi::PLATFORM_STATE_CLAIMABLE, b"acpi:\\_SB_.PCI0.SF8_.COM1").unwrap();
	assert!(node.add_match(abi::MATCH_ID_HID, b"PNP0501"));
	assert!(node.add_port(0x3f8, 8));
	assert_eq!(place(&views(&rows), &node), Placement::Merge(0));
	let mut row = rows[0].unwrap();
	assert!(merge(&mut row.part, &node));
	assert_eq!(row.part.state, abi::PLATFORM_STATE_KERNEL_HELD, "the row stays the kernel's");
	assert_eq!(row.part.match_ids().iter().filter(|id| id.kind == abi::MATCH_ID_HID).count(), 1, "a match id the row already carries is not repeated");
}

#[test]
fn an_overlap_that_does_not_start_at_the_same_base_is_refused() {
	let rows = [Some(tpm_table())];
	let mut shifted = Description::new(abi::PLATFORM_SOURCE_TREE, abi::PLATFORM_STATE_CLAIMABLE, b"dt:/tpm@fed40800").unwrap();
	assert!(shifted.add_mmio(0xfed4_0800, 0x1000));
	assert_eq!(place(&views(&rows), &shifted), Placement::Refuse { row: 0, what: Overlap::Mmio });
	let mut ports = Description::new(abi::PLATFORM_SOURCE_TABLE, abi::PLATFORM_STATE_CLAIMABLE, b"table:SPCR#0").unwrap();
	assert!(ports.add_port(0x3fc, 4));
	assert_eq!(place(&views(&[Some(com1())]), &ports), Placement::Refuse { row: 0, what: Overlap::Ports });
}

#[test]
fn a_description_overlapping_two_rows_is_refused_rather_than_merged_into_either() {
	let mut second = Description::new(abi::PLATFORM_SOURCE_TABLE, abi::PLATFORM_STATE_CLAIMABLE, b"table:SSDT#0").unwrap();
	assert!(second.add_mmio(0xfed5_0000, 0x1000));
	let rows = [Some(tpm_table()), Some(second)];
	let mut both = Description::new(abi::PLATFORM_SOURCE_TREE, abi::PLATFORM_STATE_CLAIMABLE, b"dt:/both").unwrap();
	assert!(both.add_mmio(0xfed4_0000, 0x1000));
	assert!(both.add_mmio(0xfed5_0000, 0x1000));
	assert_eq!(place(&views(&rows), &both), Placement::Refuse { row: 1, what: Overlap::Mmio });
}

#[test]
fn a_description_touching_nothing_is_a_row_of_its_own() {
	let rows = [None, Some(tpm_table()), Some(com1())];
	let mut rtc = Description::new(abi::PLATFORM_SOURCE_TREE, abi::PLATFORM_STATE_KERNEL_HELD, b"dt:/pl031@9010000").unwrap();
	assert!(rtc.add_mmio(0x0901_0000, 0x1000));
	assert_eq!(place(&views(&rows), &rtc), Placement::New);
	// Adjacent is not overlapping.
	let mut next = Description::new(abi::PLATFORM_SOURCE_TREE, abi::PLATFORM_STATE_CLAIMABLE, b"dt:/next").unwrap();
	assert!(next.add_mmio(0xfed4_1000, 0x1000));
	assert_eq!(place(&views(&rows), &next), Placement::New);
}

#[test]
fn a_merge_that_does_not_fit_adds_nothing() {
	let mut full = Description::new(abi::PLATFORM_SOURCE_TREE, abi::PLATFORM_STATE_CLAIMABLE, b"dt:/full").unwrap();
	for n in 0..abi::MAX_MATCH_IDS {
		assert!(full.add_match(abi::MATCH_ID_COMPATIBLE, format!("vendor,part{n}").as_bytes()));
	}
	assert!(!full.add_match(abi::MATCH_ID_COMPATIBLE, b"one,more"), "a description past its bound says so");
	let before = full.part;
	let other = Description::new(abi::PLATFORM_SOURCE_ACPI, abi::PLATFORM_STATE_CLAIMABLE, b"acpi:\\_SB_.FULL").unwrap();
	assert!(!merge(&mut full.part, &other));
	assert_eq!(full.part, before, "nothing was added");
}

#[test]
fn a_range_over_ram_a_bar_or_a_kernel_held_range_is_named() {
	let forbidden = [(0x4000_0000u64, 0x2000_0000u64), (0x1000_0000, 0x1000), (0x0800_0000, 0x1_0000)];
	assert_eq!(over(0x5000_0000, 0x1000, &forbidden), Some(0), "RAM");
	assert_eq!(over(0x1000_0800, 0x100, &forbidden), Some(1), "a BAR");
	assert_eq!(over(0x0800_f000, 0x2000, &forbidden), Some(2), "a kernel-held controller");
	assert_eq!(over(0x0900_0000, 0x1000, &forbidden), None);
}

#[test]
fn identities_have_their_forms_and_their_bound() {
	let mut out = [0u8; abi::PLATFORM_NAME_LEN];
	let len = table_identity(b"TPM2", 0, &mut out);
	assert_eq!(&out[..len], b"table:TPM2#0");
	let len = table_identity(b"WDAT", 12, &mut out);
	assert_eq!(&out[..len], b"table:WDAT#12");
	let len = identity(b"dt:", b"/soc/serial@10000000", &mut out).unwrap();
	assert_eq!(&out[..len], b"dt:/soc/serial@10000000");
	assert_eq!(identity(b"dt:", &[b'x'; 62], &mut out), None, "past the bound");
	assert!(Description::new(abi::PLATFORM_SOURCE_TREE, abi::PLATFORM_STATE_CLAIMABLE, &[b'x'; 65]).is_none());
	assert!(Description::new(abi::PLATFORM_SOURCE_TREE, abi::PLATFORM_STATE_CLAIMABLE, b"").is_none());
}
