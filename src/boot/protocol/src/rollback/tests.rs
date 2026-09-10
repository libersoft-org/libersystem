use std::vec;
use std::vec::Vec;

use super::*;

const PRODUCT: &[u8] = b"LiberSystem";

fn product() -> [u8; 32] {
	product_identity(PRODUCT)
}

fn valid(floor: u64, slot: Slot) -> Read {
	Read::present(ATTRIBUTES, &encode(floor, slot, &product()))
}

fn marker() -> Read {
	Read::present(ATTRIBUTES, &[MARKER_VALUE])
}

// A firmware in memory: what each variable holds, how a write behaves, and what was written.
#[derive(Default)]
struct Mock {
	a: Option<Read>,
	b: Option<Read>,
	marker: Option<Read>,
	// A write that is refused, that tears (keeps only the first `tear` bytes), or that lands with
	// the wrong attributes.
	refuse_writes: bool,
	tear: Option<usize>,
	write_attributes: Option<u32>,
	writes: Vec<(Slot, [u8; RECORD_LEN])>,
}

impl Mock {
	fn provisioned(a: Read, b: Read) -> Mock {
		Mock { a: Some(a), b: Some(b), marker: Some(marker()), ..Mock::default() }
	}

	fn slot(&self, slot: Slot) -> Option<Read> {
		match slot {
			Slot::A => self.a,
			Slot::B => self.b,
		}
	}
}

impl Firmware for Mock {
	fn read(&mut self, which: Variable) -> Read {
		match which {
			Variable::SlotA => self.a.unwrap_or(Read::Absent),
			Variable::SlotB => self.b.unwrap_or(Read::Absent),
			Variable::Marker => self.marker.unwrap_or(Read::Absent),
		}
	}

	fn write(&mut self, slot: Slot, record: &[u8; RECORD_LEN]) -> Result<(), WriteFault> {
		if self.refuse_writes {
			return Err(WriteFault::Refused(0x8000_0000_0000_0008));
		}
		self.writes.push((slot, *record));
		let kept = &record[..self.tear.unwrap_or(RECORD_LEN)];
		let stored = Read::present(self.write_attributes.unwrap_or(ATTRIBUTES), kept);
		match slot {
			Slot::A => self.a = Some(stored),
			Slot::B => self.b = Some(stored),
		}
		Ok(())
	}
}

// THE LAYOUT, at the offsets the plan fixes.
#[test]
fn the_record_is_sixty_four_bytes_at_the_frozen_offsets() {
	let record = encode(0x1122_3344_5566_7788, Slot::A, &product());
	assert_eq!(record.len(), 64);
	assert_eq!(&record[0..8], b"LSROLLB1");
	assert_eq!(&record[8..12], &1u32.to_le_bytes());
	assert_eq!(&record[12..16], &[0, 0, 0, 0]);
	assert_eq!(&record[16..48], &product());
	assert_eq!(&record[48..56], &[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11], "little-endian floor");
	// The tag is the first eight bytes of SHA-256 over the 56 bytes before it plus the slot's one
	// index byte - 57 bytes hashed, exactly.
	let mut input = [0u8; 57];
	input[..56].copy_from_slice(&record[..56]);
	input[56] = 0x00;
	assert_eq!(&record[56..], &sha256::digest(&input)[..8]);
}

#[test]
fn both_slots_round_trip_and_neither_slot_reads_as_the_other() {
	let a = encode(9, Slot::A, &product());
	let b = encode(9, Slot::B, &product());
	assert_eq!(decode(&a, Slot::A, &product()), Ok(9));
	assert_eq!(decode(&b, Slot::B, &product()), Ok(9));
	assert_ne!(a, b, "the index byte is in the tag, so the two records differ");
	// A VALID SLOT A RECORD COPIED INTO SLOT B IS REFUSED, and the reverse - which is what stops a
	// cross-slot copy forcing the floor down with two individually well-formed records.
	assert_eq!(decode(&a, Slot::B, &product()), Err(Invalid::Tag));
	assert_eq!(decode(&b, Slot::A, &product()), Err(Invalid::Tag));
	assert_eq!(Slot::A.index_byte(), 0x00);
	assert_eq!(Slot::B.index_byte(), 0x01);
}

#[test]
fn every_way_a_record_is_not_believed_is_named() {
	let good = encode(5, Slot::A, &product());
	assert_eq!(decode(&good[..63], Slot::A, &product()), Err(Invalid::Length(63)), "short");
	let mut long = [0u8; 65];
	long[..64].copy_from_slice(&good);
	assert_eq!(decode(&long, Slot::A, &product()), Err(Invalid::Length(65)), "oversized");
	let mut magic = good;
	magic[0] = b'X';
	assert_eq!(decode(&magic, Slot::A, &product()), Err(Invalid::Magic));
	let mut version = good;
	version[8] = 2;
	assert_eq!(decode(&version, Slot::A, &product()), Err(Invalid::Version(2)));
	let mut reserved = good;
	reserved[13] = 1;
	assert_eq!(decode(&reserved, Slot::A, &product()), Err(Invalid::Reserved(0x100)));
	let other = encode(5, Slot::A, &product_identity(b"SomebodyElse"));
	assert_eq!(decode(&other, Slot::A, &product()), Err(Invalid::Product), "wrong product");
	// A TORN WRITE: the floor landed and the tag did not. It must not parse as a plausible lower
	// value - it must not parse at all.
	let mut torn = good;
	torn[56..].fill(0);
	assert_eq!(decode(&torn, Slot::A, &product()), Err(Invalid::Tag));
	let mut flipped = good;
	flipped[48] ^= 1;
	assert_eq!(decode(&flipped, Slot::A, &product()), Err(Invalid::Tag), "a changed floor is a tag that no longer matches");
}

#[test]
fn a_slot_read_is_judged_by_outcome_then_attributes_then_bytes() {
	let p = product();
	assert_eq!(slot_state(Read::Absent, Slot::A, &p), SlotState::Absent);
	assert_eq!(slot_state(Read::AccessDenied, Slot::A, &p), SlotState::AccessDenied);
	assert_eq!(slot_state(Read::DeviceError, Slot::A, &p), SlotState::DeviceError);
	assert_eq!(slot_state(Read::Failed(7), Slot::A, &p), SlotState::Failed(7));
	assert_eq!(slot_state(Read::Oversized(80), Slot::A, &p), SlotState::Invalid(Invalid::Length(80)));
	let record = encode(3, Slot::A, &p);
	assert_eq!(slot_state(Read::present(ATTRIBUTES | 0x4, &record), Slot::A, &p), SlotState::WrongAttributes(0x7), "runtime access is not this record's mask");
	assert_eq!(slot_state(Read::present(ATTRIBUTES, &record), Slot::A, &p), SlotState::Valid(3));
	assert_eq!(slot_state(Read::present(ATTRIBUTES, &record[..10]), Slot::A, &p), SlotState::Invalid(Invalid::Length(10)));
}

#[test]
fn the_marker_is_one_byte_of_one_value_under_one_mask_and_everything_else_refuses() {
	assert_eq!(marker_state(Read::Absent), MarkerState::Absent, "a SUCCESSFUL read reporting not-present is the only absence");
	assert_eq!(marker_state(marker()), MarkerState::Present);
	assert_eq!(marker_state(Read::present(ATTRIBUTES, &[MARKER_VALUE, 0])), MarkerState::Refused(MarkerRefusal::Length(2)));
	assert_eq!(marker_state(Read::present(ATTRIBUTES, &[])), MarkerState::Refused(MarkerRefusal::Length(0)));
	assert_eq!(marker_state(Read::present(ATTRIBUTES, &[0x00])), MarkerState::Refused(MarkerRefusal::Value(0)));
	assert_eq!(marker_state(Read::present(ATTRIBUTES, &[0x02])), MarkerState::Refused(MarkerRefusal::Value(2)));
	assert_eq!(marker_state(Read::present(0x7, &[MARKER_VALUE])), MarkerState::Refused(MarkerRefusal::Attributes(7)));
	assert_eq!(marker_state(Read::AccessDenied), MarkerState::Refused(MarkerRefusal::AccessDenied));
	assert_eq!(marker_state(Read::DeviceError), MarkerState::Refused(MarkerRefusal::DeviceError), "a device error is not absence");
	assert_eq!(marker_state(Read::Failed(9)), MarkerState::Refused(MarkerRefusal::Failed(9)));
	assert_eq!(marker_state(Read::Oversized(70)), MarkerState::Refused(MarkerRefusal::Length(70)));
}

#[test]
fn the_partial_states_classify_the_way_the_table_says() {
	let p = product();
	let v = |n: u64, s: Slot| slot_state(valid(n, s), s, &p);
	// marker absent: unprovisioned whatever the slots hold, including valid ones from an interrupted ceremony
	assert_eq!(classify(MarkerState::Absent, SlotState::Absent, SlotState::Absent), Provisioning::Unprovisioned);
	assert_eq!(classify(MarkerState::Absent, v(4, Slot::A), v(4, Slot::B)), Provisioning::Unprovisioned);
	assert_eq!(classify(MarkerState::Absent, v(4, Slot::A), SlotState::Invalid(Invalid::Tag)), Provisioning::Unprovisioned);
	// marker present, both valid and equal
	assert_eq!(classify(MarkerState::Present, v(4, Slot::A), v(4, Slot::B)), Provisioning::Provisioned { floor: 4, lagging: None });
	// the interrupted advance: {N, N+1} is floor N+1 with A lagging
	assert_eq!(classify(MarkerState::Present, v(4, Slot::A), v(5, Slot::B)), Provisioning::Provisioned { floor: 5, lagging: Some(Slot::A) });
	assert_eq!(classify(MarkerState::Present, v(5, Slot::A), v(4, Slot::B)), Provisioning::Provisioned { floor: 5, lagging: Some(Slot::B) });
	// one valid: authoritative, the other lagging whatever is wrong with it
	assert_eq!(classify(MarkerState::Present, v(4, Slot::A), SlotState::Absent), Provisioning::Provisioned { floor: 4, lagging: Some(Slot::B) });
	assert_eq!(classify(MarkerState::Present, SlotState::DeviceError, v(4, Slot::B)), Provisioning::Provisioned { floor: 4, lagging: Some(Slot::A) });
	// neither valid on a provisioned machine: refused, and not reset
	assert_eq!(classify(MarkerState::Present, SlotState::Absent, SlotState::Invalid(Invalid::Tag)), Provisioning::Refused(Refusal::NoValidSlot { a: SlotState::Absent, b: SlotState::Invalid(Invalid::Tag) }));
	// a marker that cannot be believed refuses before the slots are even considered
	assert_eq!(classify(MarkerState::Refused(MarkerRefusal::DeviceError), v(4, Slot::A), v(4, Slot::B)), Provisioning::Refused(Refusal::Marker(MarkerRefusal::DeviceError)));
}

#[test]
fn the_decision_compares_then_converges_and_never_writes_the_only_valid_highest_first() {
	let both = Provisioning::Provisioned { floor: 4, lagging: None };
	assert_eq!(decide(both, 3), Ok(Verdict::Below { floor: 4, generation: 3 }));
	assert_eq!(decide(both, 4), Ok(Verdict::Accepted { previous: 4, floor: 4, writes: Writes::none() }), "equal boots and writes nothing when both slots agree");
	assert_eq!(decide(both, 5), Ok(Verdict::Accepted { previous: 4, floor: 5, writes: Writes { first: Some(Slot::A), second: Some(Slot::B) } }), "an advance is two writes");
	let lagging = Provisioning::Provisioned { floor: 5, lagging: Some(Slot::A) };
	assert_eq!(decide(lagging, 5), Ok(Verdict::Accepted { previous: 5, floor: 5, writes: Writes { first: Some(Slot::A), second: None } }), "an equal boot completes the interrupted advance");
	assert_eq!(decide(lagging, 6), Ok(Verdict::Accepted { previous: 5, floor: 6, writes: Writes { first: Some(Slot::A), second: Some(Slot::B) } }), "the lagging slot goes first, the authoritative one last");
	assert_eq!(decide(lagging, 4), Ok(Verdict::Below { floor: 5, generation: 4 }), "the floor is the highest valid slot, not the lagging one");
	assert_eq!(decide(Provisioning::Unprovisioned, 9), Ok(Verdict::Unprovisioned));
	assert_eq!(decide(Provisioning::Refused(Refusal::Marker(MarkerRefusal::Value(0))), 9), Err(Refusal::Marker(MarkerRefusal::Value(0))));
}

// THE SEQUENCES, driven through the firmware trait.
#[test]
fn an_advance_writes_both_slots_in_order_and_reads_each_back() {
	let p = product();
	let mut firmware = Mock::provisioned(valid(4, Slot::A), valid(4, Slot::B));
	assert_eq!(enforce(&mut firmware, &p, 5), Ok(Outcome::Accepted { previous: 4, floor: 5, converged: Writes { first: Some(Slot::A), second: Some(Slot::B) } }));
	assert_eq!(firmware.writes.iter().map(|(slot, _)| *slot).collect::<Vec<_>>(), vec![Slot::A, Slot::B]);
	assert_eq!(slot_state(firmware.slot(Slot::A).unwrap(), Slot::A, &p), SlotState::Valid(5));
	assert_eq!(slot_state(firmware.slot(Slot::B).unwrap(), Slot::B, &p), SlotState::Valid(5));
	// and the same generation again boots with nothing to write
	assert_eq!(enforce(&mut firmware, &p, 5), Ok(Outcome::Accepted { previous: 5, floor: 5, converged: Writes::none() }));
	assert_eq!(firmware.writes.len(), 2);
	// and the old one is refused, from either surviving slot
	assert_eq!(enforce(&mut firmware, &p, 4), Err(Fault::Below { floor: 5, generation: 4 }));
	firmware.a = None;
	assert_eq!(enforce(&mut firmware, &p, 4), Err(Fault::Below { floor: 5, generation: 4 }), "deleting slot A after the advance does not lower the floor");
}

#[test]
fn an_interrupted_advance_is_completed_by_the_equal_boot_before_control_is_transferred() {
	let p = product();
	// Power failed between the two writes: {A=4, B=5}.
	let mut firmware = Mock::provisioned(valid(4, Slot::A), valid(5, Slot::B));
	assert_eq!(enforce(&mut firmware, &p, 5), Ok(Outcome::Accepted { previous: 5, floor: 5, converged: Writes { first: Some(Slot::A), second: None } }));
	assert_eq!(firmware.writes.iter().map(|(slot, _)| *slot).collect::<Vec<_>>(), vec![Slot::A]);
	// Now delete B - the slot that carried the advance - and the old generation is still refused.
	firmware.b = None;
	assert_eq!(enforce(&mut firmware, &p, 4), Err(Fault::Below { floor: 5, generation: 4 }));
	// And that boot of 5 repairs B from A, so the machine converges again.
	assert_eq!(enforce(&mut firmware, &p, 5), Ok(Outcome::Accepted { previous: 5, floor: 5, converged: Writes { first: Some(Slot::B), second: None } }));
	assert_eq!(slot_state(firmware.slot(Slot::B).unwrap(), Slot::B, &p), SlotState::Valid(5));
}

#[test]
fn a_damaged_slot_is_repaired_from_the_survivor_and_two_damaged_slots_refuse() {
	let p = product();
	let mut torn = encode(4, Slot::A, &p);
	torn[60] ^= 0xff;
	let mut firmware = Mock::provisioned(Read::present(ATTRIBUTES, &torn), valid(4, Slot::B));
	assert_eq!(enforce(&mut firmware, &p, 4), Ok(Outcome::Accepted { previous: 4, floor: 4, converged: Writes { first: Some(Slot::A), second: None } }));
	assert_eq!(slot_state(firmware.slot(Slot::A).unwrap(), Slot::A, &p), SlotState::Valid(4), "the damaged slot was rewritten from the survivor on THIS boot");
	// the cross-slot copy: slot A's bytes in slot B, both present, only A valid
	let mut copied = Mock::provisioned(valid(4, Slot::A), Read::present(ATTRIBUTES, &encode(2, Slot::A, &p)));
	assert_eq!(enforce(&mut copied, &p, 4), Ok(Outcome::Accepted { previous: 4, floor: 4, converged: Writes { first: Some(Slot::B), second: None } }), "a record copied from the other slot is not a lower floor, it is an invalid slot to repair");
	let mut both = Mock::provisioned(Read::present(ATTRIBUTES, &torn), Read::present(ATTRIBUTES, &encode(2, Slot::A, &p)));
	assert_eq!(enforce(&mut both, &p, 4), Err(Fault::Refused(Refusal::NoValidSlot { a: SlotState::Invalid(Invalid::Tag), b: SlotState::Invalid(Invalid::Tag) })));
	assert!(both.writes.is_empty(), "a provisioned machine whose state cannot be believed is not reset by a boot");
}

#[test]
fn an_unprovisioned_machine_boots_and_writes_nothing_and_a_provisioned_one_with_no_state_refuses() {
	let p = product();
	let mut fresh = Mock::default();
	assert_eq!(enforce(&mut fresh, &p, 9), Ok(Outcome::Unprovisioned { generation: 9 }));
	assert!(fresh.writes.is_empty());
	// the ceremony interrupted after both slots, before the marker: still unprovisioned
	let mut interrupted = Mock { a: Some(valid(3, Slot::A)), b: Some(valid(3, Slot::B)), ..Mock::default() };
	assert_eq!(enforce(&mut interrupted, &p, 2), Ok(Outcome::Unprovisioned { generation: 2 }), "no floor is enforced until the marker commits the ceremony");
	assert!(interrupted.writes.is_empty());
	// the marker alone, both slots gone
	let mut gone = Mock { marker: Some(marker()), ..Mock::default() };
	assert_eq!(enforce(&mut gone, &p, 9), Err(Fault::Refused(Refusal::NoValidSlot { a: SlotState::Absent, b: SlotState::Absent })));
}

#[test]
fn every_firmware_failure_refuses_rather_than_reading_as_absence() {
	let p = product();
	for (name, read, expected) in [
		("access denied", Read::AccessDenied, MarkerRefusal::AccessDenied),
		("device error", Read::DeviceError, MarkerRefusal::DeviceError),
		("another status", Read::Failed(0x8000_0000_0000_0003), MarkerRefusal::Failed(0x8000_0000_0000_0003)),
		("a wrong value", Read::present(ATTRIBUTES, &[0x00]), MarkerRefusal::Value(0)),
	] {
		let mut firmware = Mock { a: Some(valid(4, Slot::A)), b: Some(valid(4, Slot::B)), marker: Some(read), ..Mock::default() };
		assert_eq!(enforce(&mut firmware, &p, 4), Err(Fault::Refused(Refusal::Marker(expected))), "marker: {name}");
	}
	// a slot whose read fails is a lagging slot, never a lower floor; both failing refuses
	let mut denied = Mock::provisioned(Read::AccessDenied, valid(4, Slot::B));
	assert_eq!(enforce(&mut denied, &p, 4), Ok(Outcome::Accepted { previous: 4, floor: 4, converged: Writes { first: Some(Slot::A), second: None } }));
	let mut both = Mock::provisioned(Read::DeviceError, Read::AccessDenied);
	assert_eq!(enforce(&mut both, &p, 4), Err(Fault::Refused(Refusal::NoValidSlot { a: SlotState::DeviceError, b: SlotState::AccessDenied })));
}

#[test]
fn a_write_that_fails_or_does_not_read_back_refuses_the_boot() {
	let p = product();
	let mut refused = Mock::provisioned(valid(4, Slot::A), valid(4, Slot::B));
	refused.refuse_writes = true;
	assert_eq!(enforce(&mut refused, &p, 5), Err(Fault::Write { slot: Slot::A, fault: WriteFault::Refused(0x8000_0000_0000_0008) }));
	let mut torn = Mock::provisioned(valid(4, Slot::A), valid(4, Slot::B));
	torn.tear = Some(40);
	assert_eq!(enforce(&mut torn, &p, 5), Err(Fault::Readback { slot: Slot::A, found: SlotState::Invalid(Invalid::Length(40)) }), "a torn write is caught by the readback of the first slot, before the second is touched");
	assert_eq!(torn.writes.len(), 1);
	assert_eq!(slot_state(torn.slot(Slot::B).unwrap(), Slot::B, &p), SlotState::Valid(4), "the untouched slot still carries the old floor");
	let mut mask = Mock::provisioned(valid(4, Slot::A), valid(4, Slot::B));
	mask.write_attributes = Some(0x7);
	assert_eq!(enforce(&mut mask, &p, 5), Err(Fault::Readback { slot: Slot::A, found: SlotState::WrongAttributes(0x7) }), "a record that came back under another mask is not the record that was written");
}

#[test]
fn the_uefi_names_and_namespace_are_the_frozen_ones() {
	assert_eq!(VENDOR_GUID_TEXT, "4c696265-7253-7973-2d52-6f6c6c626b31");
	assert_eq!(VENDOR_GUID_DATA1, 0x4c69_6265);
	assert_eq!(VENDOR_GUID_DATA2, 0x7253);
	assert_eq!(VENDOR_GUID_DATA3, 0x7973);
	assert_eq!(VENDOR_GUID_DATA4, [0x2d, 0x52, 0x6f, 0x6c, 0x6c, 0x62, 0x6b, 0x31]);
	assert_eq!(ATTRIBUTES, 0x3, "non-volatile and boot-service access, and nothing else");
	for (name, utf16) in [(SLOT_A_NAME, &SLOT_A_NAME_UTF16[..]), (SLOT_B_NAME, &SLOT_B_NAME_UTF16[..]), (MARKER_NAME, &MARKER_NAME_UTF16[..])] {
		let expected: Vec<u16> = name.bytes().map(u16::from).chain(core::iter::once(0)).collect();
		assert_eq!(utf16, &expected[..], "{name}");
	}
	assert_eq!(Variable::Marker.name(), "LiberSystemRollbackProvisioned");
	assert_eq!(Slot::A.name(), "LiberSystemRollbackA");
	assert_eq!(Slot::B.name(), "LiberSystemRollbackB");
}
