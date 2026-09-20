//! Host tests over FIXTURES THAT ARE WRONG ON PURPOSE.
//!
//! There is no ACPI machine here and there does not need to be: every answer this crate gives is a
//! function of bytes, and the bytes a real firmware writes are the easy half. What a parser has to
//! survive is the other half - a length field that lies in both directions, a revision that does not
//! carry the field being read, an access size that is not one of five, an address at the top of the
//! space with a width that runs past it - and each of those is a fixture below.

use super::*;

/// The four lengths this file builds, which are the four the standard defines: ACPI 1.0's table, the
/// one that ends at the reset register, revision 5's, and revision 6's.
const REV1_LEN: usize = 116;
const REV3_LEN: usize = 244;
const REV5_LEN: usize = 268;
const REV6_LEN: usize = 276;

/// A table under construction. The header is written by `new` and the checksum by `finish`, so a
/// fixture states only the fields it is about.
struct Builder {
	bytes: Vec<u8>,
}

impl Builder {
	fn new(signature: &[u8; 4], revision: u8, length: usize) -> Self {
		let mut bytes = vec![0u8; length];
		bytes[0..4].copy_from_slice(signature);
		bytes[4..8].copy_from_slice(&(length as u32).to_le_bytes());
		bytes[8] = revision;
		Self { bytes }
	}

	fn fadt(revision: u8, length: usize) -> Self {
		Self::new(b"FACP", revision, length)
	}

	fn u8(mut self, offset: usize, value: u8) -> Self {
		self.bytes[offset] = value;
		self
	}

	fn u16(mut self, offset: usize, value: u16) -> Self {
		self.bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
		self
	}

	fn u32(mut self, offset: usize, value: u32) -> Self {
		self.bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
		self
	}

	fn u64(mut self, offset: usize, value: u64) -> Self {
		self.bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
		self
	}

	fn gas(mut self, offset: usize, space: u8, bit_width: u8, bit_offset: u8, access: u8, address: u64) -> Self {
		self.bytes[offset] = space;
		self.bytes[offset + 1] = bit_width;
		self.bytes[offset + 2] = bit_offset;
		self.bytes[offset + 3] = access;
		self.bytes[offset + 4..offset + 12].copy_from_slice(&address.to_le_bytes());
		self
	}

	/// Declare a length OTHER than the bytes actually present, which is how a truncated or an
	/// overlong table is built.
	fn declare_length(mut self, length: u32) -> Self {
		self.bytes[4..8].copy_from_slice(&length.to_le_bytes());
		self
	}

	/// Pad the buffer past the declared length: bytes that exist and that nothing may read.
	fn pad_to(mut self, length: usize) -> Self {
		self.bytes.resize(length, 0xee);
		self
	}

	fn finish(mut self) -> Vec<u8> {
		self.bytes[9] = 0;
		let declared = u32::from_le_bytes([self.bytes[4], self.bytes[5], self.bytes[6], self.bytes[7]]) as usize;
		let covered = declared.min(self.bytes.len());
		let sum = self.bytes[..covered].iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
		self.bytes[9] = sum.wrapping_neg();
		self.bytes
	}

	/// The same bytes with the checksum left wrong, which is the fixture for a table nobody finished.
	fn finish_broken(self) -> Vec<u8> {
		let mut bytes = self.finish();
		bytes[9] = bytes[9].wrapping_add(1);
		bytes
	}
}

/// An ordinary x86 machine's FADT: legacy blocks in I/O space, their extended forms beside them, a
/// reset register, a century byte and the flags that make them mean something.
fn ordinary_pc() -> Vec<u8> {
	Builder::fadt(6, REV6_LEN).u8(FADT_MINOR_VERSION, 5).u8(FADT_POWER_PROFILE, 4).u16(FADT_SCI_INT, 9).u32(FADT_SMI_CMD, 0xb2).u8(FADT_ACPI_ENABLE, 0xa0).u8(FADT_ACPI_DISABLE, 0xa1).u8(FADT_S4BIOS_REQ, 0x77).u8(FADT_PSTATE_CNT, 0x80).u32(FADT_FIRMWARE_CTRL, 0x7fff_0000).u32(FADT_DSDT, 0x7ffe_0000).u32(FADT_PM1A_EVT, 0x0600).u32(FADT_PM1A_CNT, 0x0604).u32(FADT_PM2_CNT, 0x0620).u32(FADT_PM_TMR, 0x0608).u32(FADT_GPE0, 0x0650).u32(FADT_GPE1, 0x0660).u8(FADT_PM1_EVT_LEN, 4).u8(FADT_PM1_CNT_LEN, 2).u8(FADT_PM2_CNT_LEN, 1).u8(FADT_PM_TMR_LEN, 4).u8(FADT_GPE0_LEN, 4).u8(FADT_GPE1_LEN, 2).u8(FADT_GPE1_BASE, 16).u8(FADT_CENTURY, 0x32).u16(FADT_IAPC_BOOT, 0x0002).u32(FADT_FLAGS, FLAG_RESET_REG_SUP).gas(FADT_RESET_REG, 1, 8, 0, 1, 0x0cf9).u8(FADT_RESET_VALUE, 0x06).gas(FADT_X_PM1A_EVT, 1, 32, 0, 2, 0x0600).gas(FADT_X_PM1A_CNT, 1, 16, 0, 2, 0x0604).gas(FADT_X_PM_TMR, 1, 32, 0, 3, 0x0608).gas(FADT_X_GPE0, 1, 32, 0, 1, 0x0650).gas(FADT_X_GPE1, 1, 16, 0, 1, 0x0660).finish()
}

#[test]
// The easy half, and it still has to be right: every field the fixture states comes back, in the
// form the standard says it is in rather than as the raw bytes.
fn a_complete_table_answers_the_fields_it_was_written_with() {
	let bytes = ordinary_pc();
	let fadt = Fadt::new(&bytes).expect("an ordinary FADT");
	assert_eq!(fadt.revision(), 6);
	assert_eq!(fadt.minor_version(), Some(5));
	assert_eq!(fadt.power_profile(), Some(PowerProfile::EnterpriseServer));
	assert_eq!(fadt.sci_interrupt(), Some(9));
	assert_eq!(fadt.smi_command(), Some(0xb2));
	assert_eq!(fadt.acpi_mode_values(), Some((0xa0, 0xa1)));
	assert_eq!(fadt.s4bios_request(), Some(0x77));
	assert_eq!(fadt.pstate_control(), Some(0x80));
	assert_eq!(fadt.facs(), Some(0x7fff_0000));
	assert_eq!(fadt.dsdt(), Some(0x7ffe_0000));
	assert_eq!(fadt.century_index(), Some(0x32));
	assert_eq!(fadt.iapc_boot().map(|boot| boot.has_8042), Some(true));
	assert_eq!(fadt.iapc_boot().map(|boot| boot.cmos_rtc_not_present), Some(false));
	assert!(!fadt.hardware_reduced());

	let event = fadt.pm1a_event().expect("a PM1a event block");
	assert_eq!(event.source, BlockSource::Extended, "the standard requires the extended form to be preferred");
	assert_eq!(event.gas.address, 0x0600);
	assert_eq!(event.gas.space, AddressSpace::SystemIo);
	assert_eq!(event.gas.access, AccessSize::Word);
	assert_eq!(event.declared_bytes, 4);
	assert!(event.agrees(), "four bytes and thirty-two bits are the same block");

	// A MACHINE WITH NO PM1b SAYS SO WITH ZEROES IN BOTH FORMS, and that is absence rather than a
	// register at address zero.
	assert_eq!(fadt.pm1b_event(), None);
	assert_eq!(fadt.pm1b_control(), None);

	// The PM2 control block exists only in the legacy form here, which is the common case: it was
	// rarely given an extended one.
	let pm2 = fadt.pm2_control().expect("a PM2 control block");
	assert_eq!(pm2.source, BlockSource::Legacy);
	assert_eq!(pm2.gas.address, 0x0620);
	assert_eq!(pm2.gas.bit_width, 8);
	assert_eq!(pm2.gas.access, AccessSize::Undefined, "the legacy form never says how wide an access is");

	assert_eq!(fadt.gpe0().map(|gpe| (gpe.block.gas.address, gpe.base)), Some((0x0650, 0)));
	assert_eq!(fadt.gpe1().map(|gpe| (gpe.block.gas.address, gpe.base)), Some((0x0660, 16)), "GPE1's events start at its own base");

	let reset = fadt.reset().expect("a reset register");
	assert_eq!((reset.register.address, reset.value), (0x0cf9, 0x06));
}

#[test]
// THE FIRST RULE: nothing is read past the declared length, even when the bytes are there. A table
// that says it is an ACPI 1.0 table inside a page of memory is one, and the fields beyond it belong
// to whatever comes next.
fn nothing_is_read_past_the_declared_length() {
	// A revision-1 table's bytes, padded with 0xee to the length of a revision-6 one - which is what
	// a reader handed "the page the table is on" would see.
	let bytes = Builder::fadt(1, REV6_LEN).declare_length(REV1_LEN as u32).u32(FADT_PM1A_EVT, 0x0600).u8(FADT_PM1_EVT_LEN, 4).finish();
	let mut bytes = bytes;
	for byte in bytes.iter_mut().skip(REV1_LEN) {
		*byte = 0xee;
	}
	let fadt = Fadt::new(&bytes).expect("a revision-1 table in a longer buffer");
	assert_eq!(fadt.table().len(), REV1_LEN, "the reader stops where the table says it does");
	assert_eq!(fadt.pm1a_event().map(|block| block.gas.address), Some(0x0600));
	// The flags are the LAST field of the ACPI 1.0 table - 112 to 116 - so they are present and
	// zero, and the reset register that begins at 116 is the first byte past the end.
	assert_eq!(fadt.flags(), Some(0));
	// Every field past 116 is absent, and each of these would have read 0xee bytes.
	assert_eq!(fadt.reset(), None);
	assert_eq!(fadt.arm_boot(), None);
	assert_eq!(fadt.minor_version(), None);
	assert_eq!(fadt.sleep_control(), None);
	assert_eq!(fadt.table().u64_at(FADT_X_DSDT), None);
}

#[test]
// A length field is a number the firmware chose, and it can be wrong in four different ways.
fn a_length_that_lies_is_refused_and_says_how() {
	assert_eq!(Table::new(&[0u8; 8]).err(), Some(Error::TooShort));

	let short = Builder::fadt(6, REV6_LEN).declare_length(20).finish();
	assert_eq!(Fadt::new(&short).err(), Some(Error::LengthBelowHeader), "a table cannot be shorter than its own header");

	let past = Builder::fadt(6, REV6_LEN).declare_length(REV6_LEN as u32 + 1).finish();
	assert_eq!(Fadt::new(&past).err(), Some(Error::LengthPastEnd), "one byte past what was handed over is past it");

	let absurd = Builder::fadt(6, REV6_LEN).declare_length(MAX_TABLE_LEN as u32 + 1).finish();
	assert_eq!(Fadt::new(&absurd).err(), Some(Error::LengthUnreasonable), "a length no real table has is refused before the checksum walks it");

	// AND THE ORDER MATTERS: an absurd length is refused BEFORE the bytes are summed, because summing
	// it is the work the bound exists to prevent.
	let absurd_and_broken = Builder::fadt(6, REV6_LEN).declare_length(0xffff_ffff).finish_broken();
	assert_eq!(Fadt::new(&absurd_and_broken).err(), Some(Error::LengthUnreasonable));
}

#[test]
// The checksum is the only thing that distinguishes a table from a region that happens to begin with
// four plausible letters.
fn a_table_that_does_not_sum_to_zero_is_not_a_table() {
	let bytes = Builder::fadt(6, REV6_LEN).u16(FADT_SCI_INT, 9).finish_broken();
	assert_eq!(Fadt::new(&bytes).err(), Some(Error::Checksum));
}

#[test]
fn another_table_is_not_this_one() {
	let bytes = Builder::new(b"APIC", 3, 64).finish();
	assert_eq!(Fadt::new(&bytes).err(), Some(Error::Signature));
	// And the generic reader accepts it, which is what makes the refusal above the FADT's own.
	assert_eq!(Table::new(&bytes).map(|table| table.signature()).ok(), Some(*b"APIC"));
	assert_eq!(Table::with_signature(&bytes, b"FACP").err(), Some(Error::Signature));
}

#[test]
// THE SECOND RULE: a field the revision does not carry is absent, not zero. An ACPI 1.0 table is not
// a machine without PSCI; it is a machine that was never asked.
fn an_acpi_one_table_answers_nothing_about_fields_it_predates() {
	let bytes = Builder::fadt(1, REV1_LEN).u32(FADT_PM1A_EVT, 0x0400).u32(FADT_PM1A_CNT, 0x0404).u8(FADT_PM1_EVT_LEN, 4).u8(FADT_PM1_CNT_LEN, 2).u8(FADT_CENTURY, 0x32).u32(FADT_FLAGS, 0).finish();
	let fadt = Fadt::new(&bytes).expect("an ACPI 1.0 FADT");
	assert_eq!(fadt.revision(), 1);
	// The legacy blocks still answer: they are all this table has.
	assert_eq!(fadt.pm1a_event().map(|block| (block.gas.address, block.source)), Some((0x0400, BlockSource::Legacy)));
	assert_eq!(fadt.pm1a_control().map(|block| block.gas.bit_width), Some(16));
	// The x86 boot flags arrived in revision 2 and the ARM ones in revision 5; the reset register's
	// flag bit exists here but the register itself does not.
	assert_eq!(fadt.iapc_boot(), None);
	assert_eq!(fadt.arm_boot(), None);
	assert_eq!(fadt.reset(), None);
	// The century byte is a revision-1 field and is present.
	assert_eq!(fadt.century_index(), Some(0x32));
}

#[test]
// A hardware-reduced machine has no PM1, no PM2, no PM timer and no GPE blocks, and the standard says
// those fields are to be ignored. Answering with them would have a consumer writing to I/O ports on a
// machine that has none.
fn a_hardware_reduced_machine_has_no_fixed_hardware() {
	let bytes = Builder::fadt(6, REV6_LEN).u16(FADT_SCI_INT, 9).u32(FADT_SMI_CMD, 0xb2).u32(FADT_PM1A_EVT, 0x0600).u32(FADT_PM_TMR, 0x0608).u32(FADT_GPE0, 0x0650).u8(FADT_PM1_EVT_LEN, 4).u8(FADT_PM_TMR_LEN, 4).u8(FADT_GPE0_LEN, 4).u32(FADT_FLAGS, FLAG_HW_REDUCED_ACPI).gas(FADT_SLEEP_CONTROL, 0, 8, 0, 1, 0x4000_0000).gas(FADT_SLEEP_STATUS, 0, 8, 0, 1, 0x4000_0004).finish();
	let fadt = Fadt::new(&bytes).expect("a hardware-reduced FADT");
	assert!(fadt.hardware_reduced());
	assert_eq!(fadt.pm1a_event(), None);
	assert_eq!(fadt.pm1a_control(), None);
	assert_eq!(fadt.pm2_control(), None);
	assert_eq!(fadt.timer(), None);
	assert_eq!(fadt.gpe0(), None);
	assert_eq!(fadt.gpe1(), None);
	// And the two that ARE the mechanism on such a machine answer.
	assert_eq!(fadt.sleep_control().map(|gas| gas.address), Some(0x4000_0000));
	assert_eq!(fadt.sleep_status().map(|gas| gas.address), Some(0x4000_0004));
	// The SCI and the SMI command port belong to the mechanism that is not there.
	assert_eq!(fadt.sci_interrupt(), None);
	assert_eq!(fadt.smi_command(), None);
}

#[test]
// THE THIRD RULE, three ways. Each of these decodes to twelve bytes that cannot describe a register.
fn a_generic_address_structure_that_cannot_be_true_is_refused() {
	// An access size of five is not one of the five the standard defines.
	let wild_access = Builder::fadt(6, REV6_LEN).gas(FADT_X_PM1A_EVT, 1, 32, 0, 5, 0x0600).finish();
	let fadt = Fadt::new(&wild_access).expect("the table itself is well formed");
	assert_eq!(fadt.table().gas_at(FADT_X_PM1A_EVT), Some(Err(Error::GasAccessSize)));

	// An address with no width, and a width with no address, are each half a register.
	assert_eq!(Gas::decode(&[1, 0, 0, 1, 0x00, 0x06, 0, 0, 0, 0, 0, 0]).err(), Some(Error::GasHalfPresent));
	assert_eq!(Gas::decode(&[1, 32, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]).err(), Some(Error::GasHalfPresent));

	// An address at the very top of the space with a width that runs past it.
	let mut wrapping = [0u8; GAS_LEN];
	wrapping[0] = 0;
	wrapping[1] = 32;
	wrapping[3] = 3;
	wrapping[4..].copy_from_slice(&u64::MAX.to_le_bytes());
	assert_eq!(Gas::decode(&wrapping).err(), Some(Error::GasAddressWraps));

	// Fewer than twelve bytes is not a structure at all.
	assert_eq!(Gas::decode(&[0u8; 11]).err(), Some(Error::GasTruncated));

	// AND TWELVE ZEROES ARE LEGAL: that is how a machine says a register does not exist.
	let absent = Gas::decode(&[0u8; GAS_LEN]).expect("twelve zeroes are a well-formed absence");
	assert!(absent.is_absent() && !absent.is_present());
}

#[test]
// A truncated table and a missing register are different answers, and `gas_at` is where they part.
fn a_register_the_table_is_too_short_for_is_absent_rather_than_broken() {
	// Long enough for the reset register, not for the sleep registers.
	let bytes = Builder::fadt(3, REV3_LEN).u32(FADT_FLAGS, FLAG_RESET_REG_SUP).gas(FADT_RESET_REG, 1, 8, 0, 1, 0x0cf9).finish();
	let fadt = Fadt::new(&bytes).expect("a revision-3 FADT");
	assert!(fadt.reset().is_some());
	assert_eq!(fadt.table().gas_at(FADT_SLEEP_CONTROL), None, "absent, and not an error about bytes that are not there");
	assert_eq!(fadt.sleep_control(), None);
	// The X_ blocks are inside a revision-3 table, so a machine that fills them in is answered from
	// them even at this length.
	assert!(fadt.table().gas_at(FADT_X_GPE1).is_some());
}

#[test]
// The extended form wins where it exists, the legacy one is the fallback, and a table whose two forms
// disagree is REPORTED rather than resolved behind the caller's back.
fn the_two_forms_of_a_block_are_resolved_by_the_standard_s_rule() {
	let bytes = Builder::fadt(6, REV6_LEN).u32(FADT_PM1A_EVT, 0x0600).u8(FADT_PM1_EVT_LEN, 4).gas(FADT_X_PM1A_EVT, 0, 32, 0, 3, 0xfed0_0000).u32(FADT_PM1A_CNT, 0x0604).u8(FADT_PM1_CNT_LEN, 2).gas(FADT_X_PM1A_CNT, 1, 8, 0, 1, 0x0604).finish();
	let fadt = Fadt::new(&bytes).expect("a FADT with both forms");

	// The extended form is a memory-mapped register at a different address from the legacy one, and
	// it is the one that answers.
	let event = fadt.pm1a_event().expect("the event block");
	assert_eq!((event.gas.space, event.gas.address), (AddressSpace::SystemMemory, 0xfed0_0000));
	assert!(event.agrees());

	// The control block's two descriptions disagree about its width: eight bits against two bytes.
	let control = fadt.pm1a_control().expect("the control block");
	assert_eq!(control.gas.bit_width, 8);
	assert_eq!(control.declared_bytes, 2);
	assert!(!control.agrees(), "the disagreement is the caller's to decide about, and it can only decide if it is told");
}

#[test]
// The timer register is always thirty-two bits wide and does not always COUNT in all of them. A
// consumer that assumes it does sees time jump backwards every four seconds on a machine with a
// twenty-four bit counter.
fn the_timer_counts_as_many_bits_as_its_own_flag_says() {
	let narrow = Builder::fadt(6, REV6_LEN).u32(FADT_PM_TMR, 0x0608).u8(FADT_PM_TMR_LEN, 4).finish();
	assert_eq!(Fadt::new(&narrow).unwrap().timer().map(|timer| timer.counting_bits), Some(24));

	let wide = Builder::fadt(6, REV6_LEN).u32(FADT_PM_TMR, 0x0608).u8(FADT_PM_TMR_LEN, 4).u32(FADT_FLAGS, FLAG_TMR_VAL_EXT).finish();
	assert_eq!(Fadt::new(&wide).unwrap().timer().map(|timer| timer.counting_bits), Some(32));
}

#[test]
// Three conditions, all required. Two of them were the whole reason this accessor is not a field read.
fn a_reset_the_machine_did_not_offer_is_not_performed() {
	// The register is described and the flag is clear: the firmware filled in a field it does not
	// support, which is common.
	let unsupported = Builder::fadt(6, REV6_LEN).gas(FADT_RESET_REG, 1, 8, 0, 1, 0x0cf9).u8(FADT_RESET_VALUE, 6).finish();
	assert_eq!(Fadt::new(&unsupported).unwrap().reset(), None);

	// The flag is set and the register is twelve zeroes: there is nothing to write to.
	let empty = Builder::fadt(6, REV6_LEN).u32(FADT_FLAGS, FLAG_RESET_REG_SUP).u8(FADT_RESET_VALUE, 6).finish();
	assert_eq!(Fadt::new(&empty).unwrap().reset(), None);

	// Both, and it answers.
	let supported = Builder::fadt(6, REV6_LEN).u32(FADT_FLAGS, FLAG_RESET_REG_SUP).gas(FADT_RESET_REG, 1, 8, 0, 1, 0x0cf9).u8(FADT_RESET_VALUE, 6).finish();
	assert_eq!(Fadt::new(&supported).unwrap().reset().map(|reset| reset.value), Some(6));
}

#[test]
// The century byte indexes a CMOS that a machine may not have, and the boot flags are where it says
// so. A raw field read would have the consumer indexing hardware that is not there.
fn there_is_no_century_byte_on_a_machine_with_no_cmos_clock() {
	let no_cmos = Builder::fadt(6, REV6_LEN).u8(FADT_CENTURY, 0x32).u16(FADT_IAPC_BOOT, 0x0020).finish();
	assert_eq!(Fadt::new(&no_cmos).unwrap().century_index(), None);

	// And zero is the firmware declining to name one, which is not index zero.
	let unnamed = Builder::fadt(6, REV6_LEN).u8(FADT_CENTURY, 0).finish();
	assert_eq!(Fadt::new(&unnamed).unwrap().century_index(), None);
}

#[test]
// Zero is a legal value of `SMI_CMD` and it means "there is no port", which is the case on every
// machine whose firmware hands over already in ACPI mode. Everything that depends on the port goes
// with it.
fn an_smi_command_port_of_zero_is_no_port_at_all() {
	let bytes = Builder::fadt(6, REV6_LEN).u8(FADT_ACPI_ENABLE, 0xa0).u8(FADT_ACPI_DISABLE, 0xa1).u8(FADT_S4BIOS_REQ, 0x77).u8(FADT_PSTATE_CNT, 0x80).finish();
	let fadt = Fadt::new(&bytes).expect("a FADT with no SMI port");
	assert_eq!(fadt.smi_command(), None);
	assert_eq!(fadt.acpi_mode_values(), None, "there is nowhere to write the enable value");
	assert_eq!(fadt.s4bios_request(), None);
	assert_eq!(fadt.pstate_control(), None);
}

#[test]
// The 64-bit pointers exist because a table can live above four gigabytes, where the 32-bit field can
// only be zero. The rule is the standard's: prefer the extended one WHEN IT IS NON-ZERO.
fn the_wider_pointer_is_the_one_the_standard_prefers() {
	let both = Builder::fadt(6, REV6_LEN).u32(FADT_DSDT, 0x7ffe_0000).u64(FADT_X_DSDT, 0x1_0000_0000).finish();
	assert_eq!(Fadt::new(&both).unwrap().dsdt(), Some(0x1_0000_0000));

	let legacy_only = Builder::fadt(6, REV6_LEN).u32(FADT_DSDT, 0x7ffe_0000).finish();
	assert_eq!(Fadt::new(&legacy_only).unwrap().dsdt(), Some(0x7ffe_0000), "a zero extended field is not an answer");

	let neither = Builder::fadt(6, REV6_LEN).finish();
	assert_eq!(Fadt::new(&neither).unwrap().dsdt(), None);
}

#[test]
// An address space this reader does not name must stay distinguishable from system memory, and an
// OEM's own must stay distinguishable from a value that means nothing.
fn an_address_space_this_reader_does_not_name_is_kept_apart_from_one_it_does() {
	assert_eq!(AddressSpace::decode(0x00), AddressSpace::SystemMemory);
	assert_eq!(AddressSpace::decode(0x01), AddressSpace::SystemIo);
	assert_eq!(AddressSpace::decode(0x03), AddressSpace::EmbeddedController);
	assert_eq!(AddressSpace::decode(0x7f), AddressSpace::FunctionalFixedHardware);
	assert_eq!(AddressSpace::decode(0x0b), AddressSpace::Reserved(0x0b));
	assert_eq!(AddressSpace::decode(0x90), AddressSpace::Oem(0x90));

	// AND ONLY TWO OF THEM ARE A LOAD OR A STORE AWAY, which is the question every consumer of a
	// register actually has.
	assert!(AddressSpace::SystemMemory.is_directly_addressable());
	assert!(AddressSpace::SystemIo.is_directly_addressable());
	for space in [
		AddressSpace::EmbeddedController,
		AddressSpace::SmBus,
		AddressSpace::PciConfiguration,
		AddressSpace::FunctionalFixedHardware,
		AddressSpace::Oem(0x90),
		AddressSpace::Reserved(0x0b),
	] {
		assert!(!space.is_directly_addressable(), "{space:?} is not reached by a load");
	}
}

#[test]
// The access size decides how a register is TOUCHED, and a table that declines to say is not a table
// that said "byte".
fn an_access_size_that_was_not_stated_is_not_a_byte() {
	assert_eq!(AccessSize::decode(0), Some(AccessSize::Undefined));
	assert_eq!(AccessSize::decode(4), Some(AccessSize::Qword));
	assert_eq!(AccessSize::decode(5), None);
	assert_eq!(AccessSize::decode(255), None);
	assert_eq!(AccessSize::Undefined.bytes(), None);
	assert_eq!(AccessSize::Byte.bytes(), Some(1));
	assert_eq!(AccessSize::Qword.bytes(), Some(8));
}

#[test]
// The span is what every consumer would use to bound a mapping, so it is computed once and it is
// computed from both fields - a register that starts part way into a word occupies the bytes its
// offset pushes it into.
fn a_register_spans_the_bytes_its_offset_pushes_it_into() {
	let byte = Gas::decode(&[1, 8, 0, 1, 0x60, 0x06, 0, 0, 0, 0, 0, 0]).expect("a byte register");
	assert_eq!(byte.span_bytes(), 1);

	let straddling = Gas::decode(&[0, 16, 4, 2, 0, 0, 0, 0xfe, 0, 0, 0, 0]).expect("a register at a bit offset");
	assert_eq!(straddling.span_bytes(), 3, "four bits in, sixteen bits wide, is three bytes");

	// The widest a structure can describe, which is what the two byte-sized fields allow.
	let widest = Gas::decode(&[0, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]).expect("the widest describable register");
	assert_eq!(widest.span_bytes(), 64);
}

#[test]
// ARM machines answer the one question the loader asks a FADT, and they answer it only from revision
// 5 onward. `None` is not "no PSCI": it is "this table cannot say".
fn the_arm_boot_flags_distinguish_silence_from_a_denial() {
	let none = Builder::fadt(4, REV3_LEN).finish();
	assert_eq!(Fadt::new(&none).unwrap().arm_boot(), None, "revision 4 has no such field");

	let not_compliant = Builder::fadt(5, REV5_LEN).u16(FADT_ARM_BOOT, 0).finish();
	assert_eq!(Fadt::new(&not_compliant).unwrap().arm_boot(), Some(ArmBoot { psci_compliant: false, use_hvc: false }));

	let smc = Builder::fadt(5, REV5_LEN).u16(FADT_ARM_BOOT, 0x0001).finish();
	assert_eq!(Fadt::new(&smc).unwrap().arm_boot(), Some(ArmBoot { psci_compliant: true, use_hvc: false }));

	let hvc = Builder::fadt(6, REV6_LEN).u16(FADT_ARM_BOOT, 0x0003).finish();
	assert_eq!(Fadt::new(&hvc).unwrap().arm_boot(), Some(ArmBoot { psci_compliant: true, use_hvc: true }));
}

#[test]
// A generic table is read for its header and nothing else, and every accessor on it is bounded by the
// same declared length the FADT's are.
fn the_generic_reader_is_bounded_by_the_same_length() {
	let bytes = Builder::new(b"HPET", 1, 56).u32(40, 0xdead_beef).finish().to_vec();
	let table = Table::new(&bytes).expect("an HPET table");
	assert_eq!(table.signature(), *b"HPET");
	assert_eq!(table.revision(), 1);
	assert_eq!(table.len(), 56);
	assert!(!table.is_empty());
	assert_eq!(table.u32_at(40), Some(0xdead_beef));
	assert_eq!(table.u32_at(53), None, "four bytes from 53 is past 56");
	assert_eq!(table.u8_at(55), Some(0));
	assert_eq!(table.u8_at(56), None);
	assert_eq!(table.u64_at(usize::MAX - 2), None, "an offset whose end address overflows is past the end");
	assert_eq!(table.bytes().len(), 56);
}

#[test]
// The padded-buffer case from the other direction: a table that declares LESS than the buffer holds
// is checksummed over what it declares, so trailing bytes cannot break a valid table or rescue an
// invalid one.
fn the_checksum_covers_the_declared_length_and_not_the_buffer() {
	let bytes = Builder::fadt(6, REV6_LEN).u16(FADT_SCI_INT, 9).finish();
	let mut padded = bytes.clone();
	padded.extend_from_slice(&[0x11; 32]);
	assert!(Fadt::new(&padded).is_ok(), "bytes after the table are not part of it");

	let mut broken = Builder::fadt(6, REV6_LEN).u16(FADT_SCI_INT, 9).pad_to(REV6_LEN + 8).finish();
	// Corrupt one byte INSIDE the declared length; the padding stays as it was.
	broken[FADT_SCI_INT] = 10;
	assert_eq!(Fadt::new(&broken).err(), Some(Error::Checksum));
}

#[test]
// A table adopted through `from_table` is the same table, which is what lets a caller that already
// walked the XSDT hand one over without a second validation pass.
fn a_table_somebody_else_validated_is_adopted_as_it_is() {
	let bytes = ordinary_pc();
	let table = Table::new(&bytes).expect("a table");
	let fadt = Fadt::from_table(table).expect("a FADT");
	assert_eq!(fadt.sci_interrupt(), Some(9));

	let other = Builder::new(b"SSDT", 2, 64).finish();
	let table = Table::new(&other).expect("a table");
	assert_eq!(Fadt::from_table(table).err(), Some(Error::Signature));
}

/// A MADT under construction: the header, the local controller address and the flags word, then one
/// entry per call.
struct MadtBuilder {
	entries: Vec<u8>,
}

impl MadtBuilder {
	fn new() -> Self {
		Self { entries: Vec::new() }
	}

	/// One Interrupt Source Override, written the way firmware writes it.
	fn isa_override(mut self, source: u8, gsi: u32, flags: u16) -> Self {
		self.entries.push(2);
		self.entries.push(10);
		self.entries.push(0);
		self.entries.push(source);
		self.entries.extend_from_slice(&gsi.to_le_bytes());
		self.entries.extend_from_slice(&flags.to_le_bytes());
		self
	}

	/// An entry of some other type, so a walk has to step over something before it finds what it is
	/// looking for. A Local APIC entry is type 0 and eight bytes.
	fn local_apic(mut self, apic_id: u8) -> Self {
		self.entries.extend_from_slice(&[0, 8, apic_id, apic_id, 1, 0, 0, 0]);
		self
	}

	/// An entry whose declared length is a number the caller chose, for the truncation fixtures.
	fn raw(mut self, kind: u8, declared: u8, payload: &[u8]) -> Self {
		self.entries.push(kind);
		self.entries.push(declared);
		self.entries.extend_from_slice(payload);
		self
	}

	fn finish(self) -> Vec<u8> {
		let mut builder = Builder::new(b"APIC", 5, 44 + self.entries.len());
		builder.bytes[44..].copy_from_slice(&self.entries);
		builder.finish()
	}
}

/// THE OVERRIDE'S FLAGS ARE TWO FIELDS AND NOT ONE, which is the mistake a reader makes when it
/// masks the low nibble: polarity is bits 1:0 and trigger is bits 3:2, so the SCI's usual
/// active-low level flags word is 0b1111 and reads as a single value of fifteen.
#[test]
fn an_override_states_its_polarity_and_its_trigger_separately() {
	// 0b1111: active low (3) in bits 1:0, level (3) in bits 3:2 - what q35's firmware writes for the
	// SCI, and what an ISA line does NOT default to.
	let bytes = MadtBuilder::new().local_apic(0).isa_override(9, 9, 0b1111).finish();
	let madt = Madt::new(&bytes).expect("a MADT");
	let entry = madt.isa_override(9).expect("the SCI override");
	assert_eq!(entry.gsi, 9);
	assert_eq!(entry.polarity, Polarity::ActiveLow);
	assert_eq!(entry.trigger, Trigger::Level);
}

/// THE REDIRECTION HALF, which is the one every reader remembers: the PC timer is ISA IRQ 0 and
/// arrives at Global System Interrupt 2.
#[test]
fn an_override_can_move_a_source_to_a_different_global_interrupt() {
	let bytes = MadtBuilder::new().isa_override(0, 2, 0).finish();
	let madt = Madt::new(&bytes).expect("a MADT");
	let entry = madt.isa_override(0).expect("the timer override");
	assert_eq!(entry.gsi, 2);
	// ZERO IS "CONFORMS TO THE BUS" AND IS KEPT AS THAT. Collapsing it into edge/active-high here
	// would answer a question about the ISA bus while reading a table about an interrupt.
	assert_eq!(entry.polarity, Polarity::Bus);
	assert_eq!(entry.trigger, Trigger::Bus);
}

/// A SOURCE WITH NO OVERRIDE IS `None` AND NOT A DEFAULT. The caller has to be able to tell "the
/// firmware said nothing about this line" from "the firmware said it is ordinary", because the
/// second is a statement and the first is an absence - and only the caller knows which bus the
/// source is on.
#[test]
fn a_source_the_table_does_not_mention_has_no_override() {
	let bytes = MadtBuilder::new().local_apic(0).isa_override(0, 2, 0).finish();
	let madt = Madt::new(&bytes).expect("a MADT");
	assert_eq!(madt.isa_override(9), None);
}

/// AN OVERRIDE ON ANOTHER BUS IS NOT AN ISA OVERRIDE. Bus zero is the only value the standard
/// defines, so a table naming another one is firmware this reader does not understand - and
/// answering with its entry would configure an ISA line from a statement about something else.
#[test]
fn an_override_on_another_bus_does_not_answer_for_isa() {
	let mut builder = MadtBuilder::new();
	builder.entries.extend_from_slice(&[2, 10, 1, 9, 9, 0, 0, 0, 0b1111, 0]);
	let bytes = builder.finish();
	let madt = Madt::new(&bytes).expect("a MADT");
	assert_eq!(madt.isa_override(9), None);
	assert_eq!(madt.overrides().count(), 1);
}

/// AN ENTRY TOO SHORT FOR THE FIELDS IT WOULD BE READ FOR IS NOT ONE. An override declaring eight
/// bytes has no flags word; a walk that read one anyway would take the first two bytes of the NEXT
/// entry as the polarity and the trigger of this one.
#[test]
fn an_override_shorter_than_its_own_fields_is_skipped_rather_than_read_into_the_next_entry() {
	let bytes = MadtBuilder::new().raw(2, 8, &[0, 9, 9, 0, 0, 0]).isa_override(5, 5, 0b1111).finish();
	let madt = Madt::new(&bytes).expect("a MADT");
	assert_eq!(madt.isa_override(9), None);
	assert_eq!(madt.isa_override(5).map(|entry| entry.trigger), Some(Trigger::Level));
}

/// A LENGTH THAT DOES NOT ADVANCE ENDS THE WALK. Zero would make the iterator return the same entry
/// forever, which on a boot path is a machine that stops rather than a table that is refused.
#[test]
fn an_entry_declaring_no_length_ends_the_walk_instead_of_repeating_it() {
	let bytes = MadtBuilder::new().raw(2, 0, &[0, 9, 9, 0, 0, 0, 0b1111, 0]).finish();
	let madt = Madt::new(&bytes).expect("a MADT");
	assert_eq!(madt.overrides().count(), 0);
}

/// A LENGTH RUNNING PAST THE DECLARED END ENDS IT TOO, and for the reason every walk in this crate
/// stops at the declared length: a checksum-valid table can still be structurally wrong.
#[test]
fn an_entry_claiming_more_bytes_than_the_table_holds_ends_the_walk() {
	let bytes = MadtBuilder::new().raw(2, 40, &[0, 9, 9, 0, 0, 0, 0b1111, 0]).finish();
	let madt = Madt::new(&bytes).expect("a MADT");
	assert_eq!(madt.overrides().count(), 0);
}

/// THE TABLE HAS TO BE THE ONE THAT WAS ASKED FOR. A FADT handed to the MADT reader is refused by
/// signature rather than walked from offset 44, which in a FADT is the middle of its fixed fields.
#[test]
fn another_table_is_refused_by_its_signature() {
	let bytes = Builder::fadt(6, REV6_LEN).u16(FADT_SCI_INT, 9).finish();
	assert_eq!(Madt::new(&bytes).err(), Some(Error::Signature));
}

/// THE FIRST MATCH WINS, so firmware that lists a source twice gets a deterministic answer rather
/// than one that depends on how far the walk went.
#[test]
fn a_source_listed_twice_answers_with_the_first_entry() {
	let bytes = MadtBuilder::new().isa_override(9, 9, 0b1111).isa_override(9, 20, 0).finish();
	let madt = Madt::new(&bytes).expect("a MADT");
	assert_eq!(madt.isa_override(9).map(|entry| entry.gsi), Some(9));
}

/// THE ENABLE REGISTER IS HALF A BLOCK PAST THE STATUS REGISTER, which is the one piece of
/// arithmetic a PM1 consumer gets wrong - and getting it wrong ACKNOWLEDGES events where it meant to
/// arm them, which is silent. q35's block is four bytes at 0x0600, so its enable register is 0x0602.
#[test]
fn an_event_blocks_enable_register_is_half_a_block_past_its_status_register() {
	let bytes = Builder::fadt(6, REV6_LEN).u32(FADT_PM1A_EVT, 0x0600).u8(FADT_PM1_EVT_LEN, 4).gas(FADT_X_PM1A_EVT, 1, 32, 0, 2, 0x0600).finish();
	let fadt = Fadt::new(&bytes).expect("a FADT");
	let event = fadt.pm1a_event().expect("the event block");
	assert_eq!(event.io_port(), Some(0x0600));
	assert_eq!(event.enable_port(), Some(0x0602));
}

/// A LENGTH THAT IS NOT TWO REGISTERS IS REFUSED RATHER THAN HALVED. One byte has no second half;
/// an odd length halved by flooring puts the enable register one byte INSIDE the status register,
/// where the write lands on the wrong bits of the wrong thing.
#[test]
fn an_event_block_that_is_not_two_registers_has_no_enable_register() {
	for declared in [0u8, 1, 3, 5] {
		let bytes = Builder::fadt(6, REV6_LEN).u32(FADT_PM1A_EVT, 0x0600).u8(FADT_PM1_EVT_LEN, declared).gas(FADT_X_PM1A_EVT, 1, (declared as u32 * 8).min(255) as u8, 0, 2, 0x0600).finish();
		let fadt = Fadt::new(&bytes).expect("a FADT");
		let Some(event) = fadt.pm1a_event() else { continue };
		assert_eq!(event.enable_port(), None, "a block of {declared} byte(s) is not two registers");
	}
}

/// A BLOCK IN ANOTHER ADDRESS SPACE IS NOT A PORT. The standard allows these blocks in system
/// memory; a consumer that took the low sixteen bits of a physical address as a port number would be
/// writing to a real port belonging to something else.
#[test]
fn a_block_that_is_not_a_system_io_register_answers_with_no_port() {
	let bytes = Builder::fadt(6, REV6_LEN).u32(FADT_PM1A_EVT, 0x0600).u8(FADT_PM1_EVT_LEN, 4).gas(FADT_X_PM1A_EVT, 0, 32, 0, 3, 0xfed0_0000).finish();
	let fadt = Fadt::new(&bytes).expect("a FADT");
	let event = fadt.pm1a_event().expect("the event block");
	assert_eq!(event.gas.space, AddressSpace::SystemMemory);
	assert_eq!(event.io_port(), None);
	assert_eq!(event.enable_port(), None);
}

/// AND A PORT ADDRESS THAT DOES NOT FIT SIXTEEN BITS IS NOT A PORT EITHER, whatever the address
/// space says. A firmware declaring a system-I/O register above 0xffff has declared something this
/// architecture cannot address, and truncating it names a different register.
#[test]
fn a_system_io_address_that_does_not_fit_a_port_is_refused() {
	let bytes = Builder::fadt(6, REV6_LEN).u32(FADT_PM1A_EVT, 0x0600).u8(FADT_PM1_EVT_LEN, 4).gas(FADT_X_PM1A_EVT, 1, 32, 0, 2, 0x1_0000).finish();
	let fadt = Fadt::new(&bytes).expect("a FADT");
	let event = fadt.pm1a_event().expect("the event block");
	assert_eq!(event.io_port(), None);
	assert_eq!(event.enable_port(), None);
}
