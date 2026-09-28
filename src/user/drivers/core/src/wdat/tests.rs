use super::*;
use alloc::collections::BTreeMap;

// THE TCO's REGISTERS at q35's PM base 0x600 + 0x60, and GCS at the root-complex base + 0x3410 - the registers the
// harness's WDAT fixture describes.
const TCO_RLD: u64 = 0x660;
const TCO2_STS: u64 = 0x666;
const TCO1_CNT: u64 = 0x668;
const TCO_TMR: u64 = 0x672;
const PM1A_CNT: u64 = 0x604;
const GCS: u64 = 0xFED1_F410;

// One entry: (action, instruction, space, bit width, bit offset, access, address, value, mask).
type Entry = (u8, u8, u8, u8, u8, u8, u64, u32, u32);

// A WDAT with a valid checksum: `period` ms per count, the count range, `flags`, the entries, and a DECLARED
// entry count that may exceed them.
fn table(flags: u8, entries: &[Entry], declared: u32) -> Vec<u8> {
	let len = 68 + 24 * entries.len();
	let mut bytes = alloc::vec![0u8; len];
	bytes[0..4].copy_from_slice(b"WDAT");
	bytes[4..8].copy_from_slice(&(len as u32).to_le_bytes());
	bytes[8] = 1;
	bytes[48..52].copy_from_slice(&1200u32.to_le_bytes());
	bytes[52..56].copy_from_slice(&1023u32.to_le_bytes());
	bytes[56..60].copy_from_slice(&2u32.to_le_bytes());
	bytes[60] = flags;
	bytes[64..68].copy_from_slice(&declared.to_le_bytes());
	for (at, &(action, instruction, space, bit_width, bit_offset, access, address, value, mask)) in entries.iter().enumerate() {
		let o = 68 + 24 * at;
		bytes[o] = action;
		bytes[o + 1] = instruction;
		bytes[o + 4] = space;
		bytes[o + 5] = bit_width;
		bytes[o + 6] = bit_offset;
		bytes[o + 7] = access;
		bytes[o + 8..o + 16].copy_from_slice(&address.to_le_bytes());
		bytes[o + 16..o + 20].copy_from_slice(&value.to_le_bytes());
		bytes[o + 20..o + 24].copy_from_slice(&mask.to_le_bytes());
	}
	let sum = bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
	bytes[9] = 0u8.wrapping_sub(sum);
	bytes
}

const IO: u8 = 1;
const MEM: u8 = 0;

// The TCO's watchdog as a WDAT describes it: the reload, the halt bit as the running state, the count, the
// second-timeout status, and GCS's No-Reboot as the reboot setting.
fn tco_entries() -> Vec<Entry> {
	alloc::vec![
		(RESET, 2, IO, 16, 0, 2, TCO_RLD, 1, 0x3FF),
		(QUERY_RUNNING, 0, IO, 16, 0, 2, TCO1_CNT, 0, 0x800),
		(SET_RUNNING, 2 | 0x80, IO, 16, 0, 2, TCO1_CNT, 0, 0x800),
		(SET_STOPPED, 2 | 0x80, IO, 16, 0, 2, TCO1_CNT, 0x800, 0x800),
		(SET_COUNTDOWN, 3 | 0x80, IO, 16, 0, 2, TCO_TMR, 0, 0x3FF),
		(QUERY_COUNTDOWN, 1, IO, 16, 0, 2, TCO_TMR, 0, 0x3FF),
		(QUERY_STATUS, 0, IO, 16, 0, 2, TCO2_STS, 2, 2),
		(SET_STATUS, 2, IO, 16, 0, 2, TCO2_STS, 2, 2),
		(SET_REBOOT, 2 | 0x80, MEM, 32, 0, 3, GCS, 0, 0x20),
		(QUERY_REBOOT, 0, MEM, 32, 0, 3, GCS, 0, 0x20),
	]
}

fn minted() -> Minted {
	Minted { ports: alloc::vec![(0x660, 32)], memory: alloc::vec![(GCS, 4)] }
}

// A bus that is a map of registers, recording every write.
#[derive(Default)]
struct Fake {
	ports: BTreeMap<u16, u32>,
	declared: BTreeMap<usize, u32>,
	writes: Vec<(Reach, u32)>,
}

impl Bus for Fake {
	fn read(&mut self, reach: Reach) -> Option<u32> {
		Some(match reach {
			Reach::Port { port, .. } => *self.ports.get(&port).unwrap_or(&0),
			Reach::Declared { index, .. } => *self.declared.get(&index).unwrap_or(&0),
		})
	}

	fn write(&mut self, reach: Reach, value: u32) -> bool {
		self.writes.push((reach, value));
		match reach {
			Reach::Port { port, .. } => self.ports.insert(port, value),
			Reach::Declared { index, .. } => self.declared.insert(index, value),
		};
		true
	}
}

#[test]
fn a_table_over_the_tco_runs_its_actions_as_the_register_accesses_it_lists() {
	let bytes = table(FLAG_ENABLED | FLAG_STOPPED_IN_SLEEP, &tco_entries(), 10);
	assert_eq!(memory_registers(&bytes), [(GCS, 4)], "the one memory register, as the kernel declares it");
	let wdat = Table::load(&bytes, &minted()).expect("a watchdog");
	assert_eq!((wdat.period_ms, wdat.min_count, wdat.max_count), (1200, 2, 1023));
	assert!(wdat.flags & FLAG_STOPPED_IN_SLEEP != 0);
	let mut bus = Fake::default();
	// The halt bit set: not running.
	bus.ports.insert(0x668, 0x0800 | 0x0008);
	assert_eq!(wdat.run(QUERY_RUNNING, 0, &mut bus), Some(0));
	// SET_COUNTDOWN keeps TCO_TMR's other bits, SET_RUNNING clears the halt bit and keeps the rest of TCO1_CNT.
	bus.ports.insert(0x672, 0xFC00);
	assert_eq!(wdat.run(SET_COUNTDOWN, 50, &mut bus), Some(0));
	assert_eq!(bus.ports[&0x672], 0xFC00 | 50);
	assert_eq!(wdat.run(SET_RUNNING, 0, &mut bus), Some(0));
	assert_eq!(bus.ports[&0x668], 0x0008, "the halt bit alone cleared");
	assert_eq!(wdat.run(QUERY_RUNNING, 0, &mut bus), Some(1));
	assert_eq!(wdat.run(QUERY_COUNTDOWN, 0, &mut bus), Some(50));
	// SET_REBOOT clears No-Reboot in GCS through the declared register, keeping its other bits.
	bus.declared.insert(0, 0x21);
	assert_eq!(wdat.run(SET_REBOOT, 0, &mut bus), Some(0));
	assert_eq!(bus.declared[&0], 0x01);
	assert_eq!(wdat.run(QUERY_REBOOT, 0, &mut bus), Some(1));
	// The status: the second timeout, cleared by writing it back.
	bus.ports.insert(0x666, 2);
	assert_eq!(wdat.run(QUERY_STATUS, 0, &mut bus), Some(1));
	assert_eq!(wdat.run(RESET, 0, &mut bus), Some(0));
	assert_eq!(bus.ports[&0x660], 1, "a reload is a write of the reload register");
	assert_eq!(wdat.run(QUERY_SHUTDOWN, 0, &mut bus), None, "an action the table does not list is not run");
}

#[test]
fn a_register_outside_what_was_minted_is_never_executed() {
	// PM1a CONTROL - whose write powers the machine off - named by the pet: the table is not a watchdog.
	let mut entries = tco_entries();
	entries[0] = (RESET, 2, IO, 16, 0, 2, PM1A_CNT, 0x2000, 0xFFFF);
	assert_eq!(Table::load(&table(FLAG_ENABLED, &entries, 10), &minted()).err(), Some(Refusal::Unrunnable(RESET)));
	// THE PIT, named by an optional action: that action is dropped and never run; the table loads.
	let mut entries = tco_entries();
	entries.push((QUERY_SHUTDOWN, 0, IO, 8, 0, 1, 0x43, 0, 0xFF));
	let wdat = Table::load(&table(FLAG_ENABLED, &entries, 11), &minted()).expect("the rest is a watchdog");
	assert!(!wdat.has(QUERY_SHUTDOWN));
	assert!(wdat.dropped.contains(&QUERY_SHUTDOWN));
	// A PORT IN ANOTHER CLAIM'S RANGE is outside this row's as well.
	let mut entries = tco_entries();
	entries[4] = (SET_COUNTDOWN, 3, IO, 16, 0, 2, 0x2F8, 0, 0x3FF);
	assert_eq!(Table::load(&table(FLAG_ENABLED, &entries, 10), &minted()).err(), Some(Refusal::Unrunnable(SET_COUNTDOWN)));
	// A memory register the kernel did not declare, in the SET_REBOOT the table lists: not a watchdog.
	let entries = tco_entries();
	let undeclared = Minted { ports: alloc::vec![(0x660, 32)], memory: Vec::new() };
	assert_eq!(Table::load(&table(FLAG_ENABLED, &entries, 10), &undeclared).err(), Some(Refusal::Unrunnable(SET_REBOOT)));
	// One half of an action outside: the whole action is dropped, never half run.
	let mut entries = tco_entries();
	entries.push((SET_STATUS, 2, IO, 16, 0, 2, 0x70, 0, 0xFF));
	let wdat = Table::load(&table(FLAG_ENABLED, &entries, 11), &minted()).expect("SET_STATUS is optional");
	assert!(!wdat.has(SET_STATUS), "the action whose other half is outside is not run");
}

#[test]
fn a_hostile_table_is_refused_by_name_and_an_unknown_action_is_ignored() {
	assert_eq!(Table::load(&table(0, &tco_entries(), 10), &minted()).err(), Some(Refusal::NotEnabled), "ENABLED clear");
	let without_reset: Vec<Entry> = tco_entries().into_iter().filter(|entry| entry.0 != RESET).collect();
	assert_eq!(Table::load(&table(FLAG_ENABLED, &without_reset, 9), &minted()).err(), Some(Refusal::Missing(RESET)));
	let without_query: Vec<Entry> = tco_entries().into_iter().filter(|entry| entry.0 != QUERY_RUNNING).collect();
	assert_eq!(Table::load(&table(FLAG_ENABLED, &without_query, 9), &minted()).err(), Some(Refusal::Missing(QUERY_RUNNING)));
	// AN UNKNOWN ACTION CODE is ignored, never run.
	let mut entries = tco_entries();
	entries.push((0x7E, 2, IO, 16, 0, 2, TCO_RLD, 0xFFFF, 0xFFFF));
	let wdat = Table::load(&table(FLAG_ENABLED, &entries, 11), &minted()).expect("the known actions are a watchdog");
	assert!(wdat.dropped.contains(&0x7E));
	// AN UNKNOWN INSTRUCTION in a required action: not runnable.
	let mut entries = tco_entries();
	entries[2].1 = 0x05;
	assert_eq!(Table::load(&table(FLAG_ENABLED, &entries, 10), &minted()).err(), Some(Refusal::Unrunnable(SET_RUNNING)));
	// AN INSTRUCTION COUNT PAST THE TABLE: only the entries that fit are read - here the table declares forty.
	let wdat = Table::load(&table(FLAG_ENABLED, &tco_entries(), 40), &minted()).expect("what fits is read");
	assert!(wdat.has(QUERY_REBOOT));
	// Not a WDAT at all.
	let mut bytes = table(FLAG_ENABLED, &tco_entries(), 10);
	bytes[0] = b'X';
	assert_eq!(Table::load(&bytes, &minted()).err(), Some(Refusal::Malformed));
}
