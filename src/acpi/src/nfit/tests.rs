//! Host tests over NFIT fixtures: the shape QEMU writes for its `nvdimm` devices, and the ways a table can lie about
//! the memory it describes.

use super::*;

/// A table under construction: structures appended, the length and checksum written by `finish`.
struct Nfit {
	bytes: alloc::vec::Vec<u8>,
}

impl Nfit {
	fn new() -> Nfit {
		let mut bytes = alloc::vec![0u8; FIRST_STRUCTURE];
		bytes[0..4].copy_from_slice(b"NFIT");
		bytes[8] = 1;
		Nfit { bytes }
	}

	fn raw(mut self, structure: &[u8]) -> Nfit {
		self.bytes.extend_from_slice(structure);
		self
	}

	fn spa(self, index: u16, kind: Guid, base: u64, len: u64) -> Nfit {
		let mut s = [0u8; 56];
		s[0..2].copy_from_slice(&TYPE_SPA_RANGE.to_le_bytes());
		s[2..4].copy_from_slice(&56u16.to_le_bytes());
		s[4..6].copy_from_slice(&index.to_le_bytes());
		s[6..8].copy_from_slice(&3u16.to_le_bytes());
		s[16..32].copy_from_slice(&kind);
		s[32..40].copy_from_slice(&base.to_le_bytes());
		s[40..48].copy_from_slice(&len.to_le_bytes());
		s[48..56].copy_from_slice(&0x8008u64.to_le_bytes());
		self.raw(&s)
	}

	fn mapping(self, handle: u32, spa_index: u16, size: u64, state: u16) -> Nfit {
		let mut s = [0u8; 48];
		s[0..2].copy_from_slice(&TYPE_REGION_MAPPING.to_le_bytes());
		s[2..4].copy_from_slice(&48u16.to_le_bytes());
		s[4..8].copy_from_slice(&handle.to_le_bytes());
		s[12..14].copy_from_slice(&spa_index.to_le_bytes());
		s[14..16].copy_from_slice(&(spa_index + 1).to_le_bytes());
		s[16..24].copy_from_slice(&size.to_le_bytes());
		s[42..44].copy_from_slice(&1u16.to_le_bytes());
		s[44..46].copy_from_slice(&state.to_le_bytes());
		self.raw(&s)
	}

	fn control(self, index: u16) -> Nfit {
		let mut s = [0u8; 80];
		s[0..2].copy_from_slice(&TYPE_CONTROL_REGION.to_le_bytes());
		s[2..4].copy_from_slice(&80u16.to_le_bytes());
		s[4..6].copy_from_slice(&index.to_le_bytes());
		s[6..8].copy_from_slice(&0x8086u16.to_le_bytes());
		s[8..10].copy_from_slice(&1u16.to_le_bytes());
		s[10..12].copy_from_slice(&1u16.to_le_bytes());
		s[28..30].copy_from_slice(&0x0301u16.to_le_bytes());
		self.raw(&s)
	}

	fn flush_hint(self, handle: u32, declared: u16, addresses: &[u64]) -> Nfit {
		let mut s = alloc::vec![0u8; 16 + 8 * addresses.len()];
		s[0..2].copy_from_slice(&TYPE_FLUSH_HINT.to_le_bytes());
		let len = s.len() as u16;
		s[2..4].copy_from_slice(&len.to_le_bytes());
		s[4..8].copy_from_slice(&handle.to_le_bytes());
		s[8..10].copy_from_slice(&declared.to_le_bytes());
		for (at, address) in addresses.iter().enumerate() {
			s[16 + at * 8..24 + at * 8].copy_from_slice(&address.to_le_bytes());
		}
		self.raw(&s)
	}

	fn capabilities(self, highest: u8, bits: u32) -> Nfit {
		let mut s = [0u8; 16];
		s[0..2].copy_from_slice(&TYPE_CAPABILITIES.to_le_bytes());
		s[2..4].copy_from_slice(&16u16.to_le_bytes());
		s[4] = highest;
		s[8..12].copy_from_slice(&bits.to_le_bytes());
		self.raw(&s)
	}

	fn finish(mut self) -> alloc::vec::Vec<u8> {
		let len = self.bytes.len() as u32;
		self.bytes[4..8].copy_from_slice(&len.to_le_bytes());
		self.bytes[9] = 0;
		let sum = self.bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
		self.bytes[9] = 0u8.wrapping_sub(sum);
		self.bytes
	}
}

const MIB: u64 = 1 << 20;
const BASE: u64 = 0x1_8000_0000;

fn all(bytes: &[u8]) -> alloc::vec::Vec<Result<Namespace, (u16, Refusal)>> {
	let mut out = alloc::vec::Vec::new();
	namespaces(bytes, |result| out.push(result)).expect("a valid table");
	out
}

fn refusal(bytes: &[u8]) -> Refusal {
	match all(bytes).as_slice() {
		[Err((_, refusal))] => *refusal,
		other => panic!("one refused range, not {other:?}"),
	}
}

#[test]
// THE SHAPE QEMU WRITES: per module a persistent-memory range, its mapping, its control region; and the platform's
// capabilities when the machine names a persistence domain.
fn a_module_s_range_is_one_namespace_with_its_state_and_the_platform_s_capabilities() {
	let table = Nfit::new().spa(2, PERSISTENT_MEMORY, BASE, 16 * MIB).mapping(1, 2, 16 * MIB, 0).control(3).spa(4, PERSISTENT_MEMORY, BASE + 16 * MIB, 16 * MIB).mapping(2, 4, 16 * MIB, STATE_NOT_ARMED).control(5).capabilities(1, CAPABILITY_MEMORY_CONTROLLER_FLUSH).finish();
	let found = all(&table);
	assert_eq!(found.len(), 2);
	let first = found[0].expect("the armed module's range");
	assert_eq!(first, Namespace { index: 2, base: BASE, len: 16 * MIB, attributes: 0x8008, state: 0, modules: 1, flush_hints: 0, capabilities: Some(CAPABILITY_MEMORY_CONTROLLER_FLUSH) });
	let second = found[1].expect("the unarmed module's range");
	assert_eq!((second.index, second.base, second.state), (4, BASE + 16 * MIB, STATE_NOT_ARMED), "a module's state is its range's");
	let mut kinds = alloc::vec::Vec::new();
	structures(&table, |structure| kinds.push(core::mem::discriminant(&structure))).expect("valid");
	assert_eq!(kinds.len(), 7, "every structure is visited");
}

#[test]
fn a_control_region_a_block_window_and_volatile_memory_are_never_namespaces() {
	let table = Nfit::new().spa(1, CONTROL_REGION, BASE, MIB).spa(2, BLOCK_DATA_WINDOW, BASE + MIB, MIB).spa(3, VOLATILE_MEMORY, BASE + 2 * MIB, MIB).mapping(1, 1, MIB, 0).mapping(1, 2, MIB, 0).mapping(1, 3, MIB, 0).finish();
	assert!(all(&table).is_empty(), "a module's registers and its firmware interface are not memory anybody is handed");
}

#[test]
fn a_range_the_specification_does_not_allow_is_refused_for_its_own_reason() {
	let backed = |index: u16, base: u64, len: u64| Nfit::new().spa(index, PERSISTENT_MEMORY, base, len).mapping(1, index, len, 0).finish();
	assert_eq!(refusal(&backed(0, BASE, MIB)), Refusal::Index, "index zero is reserved");
	assert_eq!(refusal(&backed(1, BASE, 0)), Refusal::Empty);
	assert_eq!(refusal(&backed(1, BASE + 512, MIB)), Refusal::Unaligned, "a base inside a page");
	assert_eq!(refusal(&backed(1, BASE, MIB + 512)), Refusal::Unaligned, "a length that is not whole pages");
	assert_eq!(refusal(&backed(1, u64::MAX - 0xfff, 0x2000)), Refusal::Wraps, "an end past the address space");
}

#[test]
// THE PHYSICAL MAP HAS ONE OWNER PER BYTE: a range over another range - a control region's most of all, since that is a
// module's registers - is no namespace, and neither range may be served by a guess about which is right.
fn a_range_over_another_or_under_a_second_index_is_refused() {
	let over_control = Nfit::new().spa(1, CONTROL_REGION, BASE + 4 * MIB, MIB).spa(2, PERSISTENT_MEMORY, BASE, 16 * MIB).mapping(1, 2, 16 * MIB, 0).finish();
	assert_eq!(refusal(&over_control), Refusal::Overlap, "a persistent range laid over a module's control region");
	let two = Nfit::new().spa(1, PERSISTENT_MEMORY, BASE, 16 * MIB).mapping(1, 1, 16 * MIB, 0).spa(2, PERSISTENT_MEMORY, BASE + 8 * MIB, 16 * MIB).mapping(2, 2, 16 * MIB, 0).finish();
	assert_eq!(all(&two), alloc::vec![Err((1, Refusal::Overlap)), Err((2, Refusal::Overlap))], "both of two overlapping ranges");
	let duplicate = Nfit::new().spa(1, PERSISTENT_MEMORY, BASE, 16 * MIB).spa(1, PERSISTENT_MEMORY, BASE + 64 * MIB, 16 * MIB).mapping(1, 1, 16 * MIB, 0).finish();
	assert_eq!(all(&duplicate), alloc::vec![Err((1, Refusal::Duplicate)), Err((1, Refusal::Duplicate))], "one index naming two ranges is neither");
	let adjacent = Nfit::new().spa(1, PERSISTENT_MEMORY, BASE, 16 * MIB).mapping(1, 1, 16 * MIB, 0).spa(2, PERSISTENT_MEMORY, BASE + 16 * MIB, 16 * MIB).mapping(2, 2, 16 * MIB, 0).finish();
	assert!(all(&adjacent).iter().all(Result::is_ok), "two ranges that only touch are both namespaces");
}

#[test]
fn a_range_its_modules_do_not_account_for_is_refused() {
	let unbacked = Nfit::new().spa(1, PERSISTENT_MEMORY, BASE, 16 * MIB).finish();
	assert_eq!(refusal(&unbacked), Refusal::Unbacked);
	let short = Nfit::new().spa(1, PERSISTENT_MEMORY, BASE, 16 * MIB).mapping(1, 1, 8 * MIB, 0).finish();
	assert_eq!(refusal(&short), Refusal::Uncovered, "a module backing half the range");
	let unmapped = Nfit::new().spa(1, PERSISTENT_MEMORY, BASE, 16 * MIB).mapping(1, 1, 16 * MIB, STATE_NOT_MAPPED).finish();
	assert_eq!(refusal(&unmapped), Refusal::Unbacked, "a module the table says is not mapped backs nothing");
	let wraps = Nfit::new().spa(1, PERSISTENT_MEMORY, BASE, 16 * MIB).mapping(1, 1, u64::MAX, 0).mapping(2, 1, 16 * MIB + 1, 0).finish();
	assert_eq!(refusal(&wraps), Refusal::Uncovered, "sizes whose sum wraps account for nothing");
}

#[test]
// AN INTERLEAVE SET IS ONE RANGE BACKED BY SEVERAL MODULES, and its namespace carries every module's state: one module
// that is not armed is a range whose writes are not all saved.
fn an_interleaved_range_is_one_namespace_carrying_every_module_s_state() {
	let table = Nfit::new().spa(1, PERSISTENT_MEMORY, BASE, 32 * MIB).mapping(1, 1, 16 * MIB, STATE_SMART_EVENTS).mapping(2, 1, 16 * MIB, STATE_NOT_ARMED).finish();
	let found = all(&table);
	let namespace = found[0].expect("one namespace");
	assert_eq!((namespace.modules, namespace.state), (2, STATE_SMART_EVENTS | STATE_NOT_ARMED));
	assert_eq!(namespace.capabilities, None, "a table without the capabilities structure declares none");
}

#[test]
fn flush_hints_are_counted_for_the_modules_that_back_the_range_and_held_to_their_structure() {
	let table = Nfit::new().spa(1, PERSISTENT_MEMORY, BASE, 16 * MIB).mapping(7, 1, 16 * MIB, 0).flush_hint(7, 2, &[0xfed0_0000, 0xfed0_1000]).flush_hint(9, 1, &[0xfed1_0000]).finish();
	assert_eq!(all(&table)[0].expect("a namespace").flush_hints, 2, "the other module's hint is not this range's");
	let mut hints = alloc::vec::Vec::new();
	let lying = Nfit::new().flush_hint(7, 500, &[0xfed0_0000, 0xfed0_1000, 0xfed0_2000]).finish();
	structures(&lying, |structure| {
		if let Structure::FlushHint(hint) = structure {
			hints.push(hint.addresses().to_vec());
		}
	})
	.expect("valid");
	assert_eq!(hints, alloc::vec![alloc::vec![0xfed0_0000, 0xfed0_1000, 0xfed0_2000]], "a declared count past the structure's length is held to it");
	let many: alloc::vec::Vec<u64> = (0..40).map(|at| 0xfed0_0000 + at * 0x1000).collect();
	let mut read = 0;
	structures(&Nfit::new().flush_hint(1, 40, &many).finish(), |structure| {
		if let Structure::FlushHint(hint) = structure {
			read = hint.addresses().len();
		}
	})
	.expect("valid");
	assert_eq!(read, MAX_FLUSH_HINTS, "and to the bound");
}

#[test]
fn only_the_capabilities_the_platform_declares_valid_are_read() {
	let mut found = alloc::vec::Vec::new();
	let table = Nfit::new().capabilities(0, CAPABILITY_CPU_CACHE_FLUSH | CAPABILITY_MEMORY_CONTROLLER_FLUSH | (1 << 2)).capabilities(31, 0xffff_ffff).finish();
	structures(&table, |structure| {
		if let Structure::Capabilities(bits) = structure {
			found.push(bits);
		}
	})
	.expect("valid");
	assert_eq!(found, alloc::vec![CAPABILITY_CPU_CACHE_FLUSH, u32::MAX], "bits past the highest valid one are not the platform's word");
}

#[test]
fn a_structure_that_does_not_fit_ends_the_walk_and_a_short_one_is_not_decoded() {
	let mut runs_past = [0u8; 8];
	runs_past[0..2].copy_from_slice(&TYPE_SPA_RANGE.to_le_bytes());
	runs_past[2..4].copy_from_slice(&56u16.to_le_bytes());
	let table = Nfit::new().spa(1, PERSISTENT_MEMORY, BASE, 16 * MIB).mapping(1, 1, 16 * MIB, 0).raw(&runs_past).finish();
	let mut seen = 0;
	structures(&table, |_| seen += 1).expect("valid");
	assert_eq!(seen, 2, "a structure longer than what is left of the table is not read");
	let zero = Nfit::new().raw(&[0, 0, 0, 0]).spa(1, PERSISTENT_MEMORY, BASE, 16 * MIB).finish();
	let mut seen = 0;
	structures(&zero, |_| seen += 1).expect("valid");
	assert_eq!(seen, 0, "a length below a structure's own header ends the walk rather than looping on it");
	let mut short = [0u8; 40];
	short[0..2].copy_from_slice(&TYPE_SPA_RANGE.to_le_bytes());
	short[2..4].copy_from_slice(&40u16.to_le_bytes());
	let mut decoded = alloc::vec::Vec::new();
	structures(&Nfit::new().raw(&short).finish(), |structure| decoded.push(structure)).expect("valid");
	assert_eq!(decoded, alloc::vec![Structure::Other(TYPE_SPA_RANGE)], "a range structure too short for its fields is no range");
}

#[test]
fn a_table_that_is_not_a_whole_nfit_is_refused() {
	let mut table = Nfit::new().spa(1, PERSISTENT_MEMORY, BASE, 16 * MIB).mapping(1, 1, 16 * MIB, 0).finish();
	table[50] ^= 1;
	assert_eq!(namespaces(&table, |_| {}), Err(Error::Checksum));
	let mut other = Nfit::new();
	other.bytes[0..4].copy_from_slice(b"NFIX");
	let other = other.finish();
	assert_eq!(structures(&other, |_| {}), Err(Error::Signature));
}

#[test]
fn a_namespace_crosses_to_its_driver_as_one_fixed_record() {
	let namespace = Namespace { index: 2, base: BASE, len: 16 * MIB, attributes: 0x8008, state: STATE_NOT_ARMED, modules: 1, flush_hints: 3, capabilities: Some(CAPABILITY_MEMORY_CONTROLLER_FLUSH) };
	let record = namespace.encode();
	assert_eq!(Namespace::decode(&record), Some(namespace));
	let without = Namespace { capabilities: None, ..namespace };
	assert_eq!(Namespace::decode(&without.encode()), Some(without), "no capabilities is not capabilities of zero");
	assert_eq!(Namespace::decode(&record[..RECORD_LEN - 1]), None, "a short record is none");
	let mut garbled = record;
	garbled[12] = 2;
	assert_eq!(Namespace::decode(&garbled), None, "a presence flag that is neither is refused");
}

// THE TABLE QEMU 10.0 WRITES for two 16 MiB `nvdimm` modules, the second `unarmed=on`, on a q35 machine with
// `nvdimm-persistence=mem-ctrl` - dumped from the guest the kernel suite boots, hex, 424 bytes.
const QEMU_TWO_MODULES: [&str; 9] = [
	"4e464954a801000001cb424f434853204258504320202020010000004258504301000000000000000000380002000300",
	"000000000000000079d3f066f3b47440ac430d3318b78cdb000000800100000000000001000000000880000000000000",
	"010030000100000000000000020003000000000100000000000000000000000000000000000000000000010000000000",
	"040050000300868001000100000000000000000000000000563412000103000000000000000000000000000000000000",
	"000000000000000000000000000000000000000000000000000000000000000000003800040003000000000000000000",
	"79d3f066f3b47440ac430d3318b78cdb0000008101000000000000010000000008800000000000000100300002000000",
	"000000000400050000000001000000000000000000000000000000000000000000000100080000000400500005008680",
	"010001000000000000000000000000005734120001030000000000000000000000000000000000000000000000000000",
	"00000000000000000000000000000000000000000000000007001000010000000200000000000000",
];

#[test]
// THE EMULATOR'S OWN BYTES, which no builder here wrote: per module a range, its mapping and its control region, then
// the capabilities - and no flush hints, which QEMU does not write.
fn the_table_qemu_writes_for_two_modules_is_two_namespaces_the_second_not_armed() {
	let hex: alloc::string::String = QEMU_TWO_MODULES.concat();
	let table: alloc::vec::Vec<u8> = (0..hex.len()).step_by(2).map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex")).collect();
	assert_eq!(table.len(), 424);
	let found = all(&table);
	let first = Namespace { index: 2, base: 0x1_8000_0000, len: 16 * MIB, attributes: 0x8008, state: 0, modules: 1, flush_hints: 0, capabilities: Some(CAPABILITY_MEMORY_CONTROLLER_FLUSH) };
	assert_eq!(found, alloc::vec![Ok(first), Ok(Namespace { index: 4, base: 0x1_8100_0000, state: STATE_NOT_ARMED, ..first })]);
	let mut controls = alloc::vec::Vec::new();
	structures(&table, |structure| {
		if let Structure::Control(control) = structure {
			controls.push((control.index, control.vendor, control.device, control.format, control.windows));
		}
	})
	.expect("valid");
	assert_eq!(controls, alloc::vec![(3, 0x8086, 1, 0x0301, 0), (5, 0x8086, 1, 0x0301, 0)], "a byte-addressable, energy-backed module with no block windows, each");
}
