use super::*;

// A table built from structures, each written as the specification lays it out.
struct Table {
	bytes: [u8; 4096],
	len: usize,
	count: u16,
}

impl Table {
	fn new() -> Self {
		Table { bytes: [0; 4096], len: 0, count: 0 }
	}

	fn push(&mut self, kind: u8, handle: u16, body: &[u8], strings: &[&[u8]]) -> &mut Self {
		let length = 4 + body.len();
		self.bytes[self.len] = kind;
		self.bytes[self.len + 1] = length as u8;
		self.bytes[self.len + 2..self.len + 4].copy_from_slice(&handle.to_le_bytes());
		self.bytes[self.len + 4..self.len + length].copy_from_slice(body);
		self.len += length;
		if strings.is_empty() {
			self.len += 2;
		} else {
			for text in strings {
				self.bytes[self.len..self.len + text.len()].copy_from_slice(text);
				self.len += text.len() + 1;
			}
			self.len += 1;
		}
		self.count += 1;
		self
	}

	fn end(&mut self) -> &mut Self {
		self.push(TYPE_END, 0xFFFF, &[], &[])
	}

	fn bytes(&self) -> &[u8] {
		&self.bytes[..self.len]
	}
}

fn fix_checksum(bytes: &mut [u8], at: usize, over: core::ops::Range<usize>) {
	bytes[at] = 0;
	let sum = bytes[over].iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
	bytes[at] = 0u8.wrapping_sub(sum);
}

fn entry_v3(address: u64, max: u32) -> [u8; 31] {
	let mut bytes = [0u8; 31];
	bytes[..5].copy_from_slice(b"_SM3_");
	bytes[6] = 0x18;
	bytes[7] = 3;
	bytes[8] = 2;
	bytes[0x0A] = 1;
	bytes[0x0C..0x10].copy_from_slice(&max.to_le_bytes());
	bytes[0x10..0x18].copy_from_slice(&address.to_le_bytes());
	fix_checksum(&mut bytes, 5, 0..0x18);
	bytes
}

fn entry_v2(address: u32, length: u16, count: u16) -> [u8; 31] {
	let mut bytes = [0u8; 31];
	bytes[..4].copy_from_slice(b"_SM_");
	bytes[5] = 0x1F;
	bytes[6] = 2;
	bytes[7] = 8;
	bytes[0x10..0x15].copy_from_slice(b"_DMI_");
	bytes[0x16..0x18].copy_from_slice(&length.to_le_bytes());
	bytes[0x18..0x1C].copy_from_slice(&address.to_le_bytes());
	bytes[0x1C..0x1E].copy_from_slice(&count.to_le_bytes());
	bytes[0x1E] = 0x28;
	fix_checksum(&mut bytes, 0x15, 0x10..0x1F);
	fix_checksum(&mut bytes, 4, 0..0x1F);
	bytes
}

// A type-1 body: string indices for manufacturer, product, version and serial, the UUID, the wake type,
// then SKU and family.
fn system_body(uuid: [u8; 16]) -> [u8; 23] {
	let mut body = [0u8; 23];
	body[..4].copy_from_slice(&[1, 2, 3, 4]);
	body[4..20].copy_from_slice(&uuid);
	body[20] = 6;
	body[21] = 5;
	body[22] = 6;
	body
}

const UUID: [u8; 16] = [0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];

#[test]
fn a_3x_entry_point_names_its_table_and_bounds_it_by_the_maximum_size() {
	let entry = entry_point(&entry_v3(0x7f5f_c000, 0x200)).expect("a valid 3.x entry point");
	assert_eq!(entry, EntryPoint { major: 3, minor: 2, table_address: 0x7f5f_c000, table_length: 0x200, structure_count: None });
}

#[test]
fn a_2x_entry_point_checks_both_of_its_checksums() {
	let entry = entry_point(&entry_v2(0x000f_0000, 0x180, 9)).expect("a valid 2.x entry point");
	assert_eq!(entry, EntryPoint { major: 2, minor: 8, table_address: 0x000f_0000, table_length: 0x180, structure_count: Some(9) });
	let mut outer = entry_v2(0x000f_0000, 0x180, 9);
	outer[0x1E] ^= 1;
	assert_eq!(entry_point(&outer), Err(Refusal::BadChecksum), "a changed byte breaks both sums");
	// ONLY THE INTERMEDIATE SUM BROKEN: the outer one repaired over the change, so the check that fails is the
	// one a legacy reader makes over `_DMI_` alone.
	let mut inner = entry_v2(0x000f_0000, 0x180, 9);
	inner[0x1C] ^= 1;
	fix_checksum(&mut inner, 4, 0..0x1F);
	assert_eq!(entry_point(&inner), Err(Refusal::BadChecksum));
}

#[test]
fn an_entry_point_that_is_not_one_is_refused_by_what_is_wrong() {
	assert_eq!(entry_point(b"not an anchor at all, nothing here"), Err(Refusal::NoAnchor));
	let mut bad = entry_v3(0x1000, 0x100);
	bad[0x10] ^= 0xFF;
	assert_eq!(entry_point(&bad), Err(Refusal::BadChecksum));
	let mut short = entry_v3(0x1000, 0x100);
	short[6] = 0x10;
	assert_eq!(entry_point(&short), Err(Refusal::BadEntryLength));
	assert_eq!(entry_point(&entry_v3(0x1000, 0x100)[..20]), Err(Refusal::BadEntryLength), "fewer bytes than the form");
	assert_eq!(entry_point(&entry_v3(0x1000, MAX_TABLE_LENGTH + 1)), Err(Refusal::BadTableLength), "a table past the bound");
	assert_eq!(entry_point(&entry_v3(0x1000, 2)), Err(Refusal::BadTableLength), "a table shorter than one header");
}

#[test]
fn the_system_is_named_by_its_type_one_structure() {
	let mut table = Table::new();
	table.push(0, 0, &[1, 2, 0, 0xF0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], &[b"EFI Development Kit II / OVMF", b"0.0.0"]);
	table.push(TYPE_SYSTEM, 0x100, &system_body(UUID), &[b"QEMU", b"Standard PC (Q35 + ICH9, 2009)", b"pc-q35-10.0", b"SN-1", b"SKU-9", b"Family"]);
	table.end();
	let entry = EntryPoint { major: 3, minor: 0, table_address: 0, table_length: table.len as u32, structure_count: None };
	let system = system_identity(table.bytes(), &entry).expect("a well-formed table").expect("a type-1 structure");
	assert_eq!(system.manufacturer, Some(&b"QEMU"[..]));
	assert_eq!(system.product, Some(&b"Standard PC (Q35 + ICH9, 2009)"[..]));
	assert_eq!(system.version, Some(&b"pc-q35-10.0"[..]));
	assert_eq!(system.serial, Some(&b"SN-1"[..]));
	assert_eq!(system.sku, Some(&b"SKU-9"[..]));
	assert_eq!(system.family, Some(&b"Family"[..]));
	assert_eq!(system.uuid, Some(UUID));
	let mut text = [0u8; 36];
	uuid_text(&UUID, &mut text);
	assert_eq!(&text, b"00112233-4455-6677-8899-aabbccddeeff", "the first three fields are little-endian");
}

#[test]
fn a_uuid_of_all_zeros_or_all_ones_is_no_uuid() {
	for none in [[0u8; 16], [0xFF; 16]] {
		let mut table = Table::new();
		table.push(TYPE_SYSTEM, 1, &system_body(none), &[b"M", b"P", b"V", b"S"]).end();
		let entry = EntryPoint { major: 3, minor: 0, table_address: 0, table_length: table.len as u32, structure_count: None };
		assert_eq!(system_identity(table.bytes(), &entry).unwrap().unwrap().uuid, None);
	}
}

#[test]
fn a_string_index_of_zero_or_past_the_set_is_no_string() {
	let mut table = Table::new();
	table.push(TYPE_SYSTEM, 1, &[0, 9, 1, 1], &[b"only"]).end();
	let entry = EntryPoint { major: 2, minor: 0, table_address: 0, table_length: table.len as u32, structure_count: None };
	let system = system_identity(table.bytes(), &entry).unwrap().unwrap();
	assert_eq!(system.manufacturer, None, "index zero is the specification's none");
	assert_eq!(system.product, None, "index nine of a one-string set");
	assert_eq!(system.version, Some(&b"only"[..]));
	assert_eq!(system.uuid, None, "a 2.0 structure is too short to carry one");
	assert_eq!(system.family, None);
}

#[test]
fn a_table_with_no_system_structure_answers_none() {
	let mut table = Table::new();
	table.push(3, 1, &[1], &[b"chassis"]).end();
	let entry = EntryPoint { major: 3, minor: 0, table_address: 0, table_length: table.len as u32, structure_count: None };
	assert_eq!(system_identity(table.bytes(), &entry), Ok(None));
}

#[test]
fn a_structure_shorter_than_its_header_or_longer_than_the_table_ends_the_walk() {
	let mut table = Table::new();
	table.push(0, 0, &[], &[]);
	let mut bytes = [0u8; 64];
	bytes[..table.len].copy_from_slice(table.bytes());
	// The second structure claims a length of 2, below its own four-byte header.
	bytes[table.len..table.len + 4].copy_from_slice(&[TYPE_SYSTEM, 2, 0, 0]);
	let entry = EntryPoint { major: 3, minor: 0, table_address: 0, table_length: 64, structure_count: None };
	let walked: [Option<Result<u8, Refusal>>; 3] = {
		let mut walk = structures(&bytes, &entry).map(|structure| structure.map(|s| s.kind));
		[walk.next(), walk.next(), walk.next()]
	};
	assert_eq!(walked, [Some(Ok(0)), Some(Err(Refusal::BadStructure)), None]);
	// One whose length runs past the end of the table.
	let mut long = [0u8; 16];
	long[..4].copy_from_slice(&[TYPE_SYSTEM, 40, 0, 0]);
	let entry = EntryPoint { major: 3, minor: 0, table_address: 0, table_length: 16, structure_count: None };
	assert_eq!(structures(&long, &entry).next(), Some(Err(Refusal::BadStructure)));
	assert_eq!(system_identity(&long, &entry), Err(Refusal::BadStructure));
}

#[test]
fn a_string_set_with_no_terminator_inside_the_table_is_refused_not_read_past() {
	let mut bytes = [b'x'; 32];
	bytes[..4].copy_from_slice(&[TYPE_SYSTEM, 4, 0, 0]);
	let entry = EntryPoint { major: 3, minor: 0, table_address: 0, table_length: 32, structure_count: None };
	assert_eq!(structures(&bytes, &entry).next(), Some(Err(Refusal::UnterminatedStrings)));
	// The terminator exists in memory past the table's stated length and must not be found there.
	let mut beyond = [0u8; 40];
	beyond[..4].copy_from_slice(&[TYPE_SYSTEM, 4, 0, 0]);
	beyond[4..20].copy_from_slice(&[b'y'; 16]);
	let entry = EntryPoint { major: 3, minor: 0, table_address: 0, table_length: 20, structure_count: None };
	assert_eq!(structures(&beyond, &entry).next(), Some(Err(Refusal::UnterminatedStrings)));
}

#[test]
fn the_walk_stops_at_the_end_of_table_and_at_a_2x_count() {
	let mut table = Table::new();
	table.push(0, 0, &[], &[]).push(TYPE_SYSTEM, 1, &[0, 0, 0, 0], &[]).end().push(TYPE_SYSTEM, 2, &[1, 0, 0, 0], &[b"after the end"]);
	let entry = EntryPoint { major: 3, minor: 0, table_address: 0, table_length: table.len as u32, structure_count: None };
	assert_eq!(structures(table.bytes(), &entry).count(), 3, "type 127 ends a 3.x table");
	let counted = EntryPoint { structure_count: Some(1), ..entry };
	assert_eq!(structures(table.bytes(), &counted).count(), 1, "and a 2.x count ends a 2.x one");
}

#[test]
fn an_ipmi_record_decodes_its_address_the_way_its_interface_needs() {
	let mut table = Table::new();
	// KCS at I/O 0xCA3: the base carries 0xCA2 | 1 (I/O), and the modifier's bit 4 supplies address bit 0.
	let mut kcs = [0u8; 14];
	kcs[0] = IPMI_KCS;
	kcs[1] = 0x20;
	kcs[2] = 0x20;
	kcs[4..12].copy_from_slice(&0x0CA3u64.to_le_bytes());
	kcs[12] = 0x10;
	table.push(TYPE_IPMI, 0x2600, &kcs, &[]);
	// SSIF at seven-bit address 0x10, stored shifted as 0x20.
	let mut ssif = [0u8; 14];
	ssif[0] = IPMI_SSIF;
	ssif[1] = 0x20;
	ssif[4..12].copy_from_slice(&0x20u64.to_le_bytes());
	ssif[13] = 11;
	table.push(TYPE_IPMI, 0x2601, &ssif, &[]);
	// A record too short to name an address: skipped, still counted.
	table.push(TYPE_IPMI, 0x2602, &[IPMI_BT, 0x20], &[]);
	table.end();
	let entry = EntryPoint { major: 3, minor: 0, table_address: 0, table_length: table.len as u32, structure_count: None };
	let mut seen: [Option<IpmiRecord>; 4] = [None; 4];
	let mut at = 0;
	ipmi_records(table.bytes(), &entry, |record| {
		seen[at] = Some(record);
		at += 1;
	})
	.expect("a well-formed table");
	assert_eq!(at, 2, "the short record names nothing");
	let kcs = seen[0].unwrap();
	assert!(kcs.io_space());
	assert_eq!((kcs.instance, kcs.interface, kcs.address(), kcs.interrupt), (0, IPMI_KCS, 0x0CA3, None));
	let ssif = seen[1].unwrap();
	assert_eq!((ssif.instance, ssif.interface, ssif.ssif_address(), ssif.interrupt), (1, IPMI_SSIF, 0x10, Some(11)));
}
