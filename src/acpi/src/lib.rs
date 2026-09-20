//! The ACPI tables this system READS, bounded: the Generic Address Structure, and the Fixed ACPI
//! Description Table the firmware describes its fixed hardware with.
//!
//! IT PUBLISHES NO DEVICE AND CLAIMS NOTHING. There is no driver here, no binding, no provider and
//! no process: a caller that has the bytes of a table gets typed answers about them, and everything
//! that would act on those answers - enabling ACPI mode, arming a general-purpose event, resetting
//! the machine - is somebody else's item. That is what lets this one close on host tests over
//! fixtures rather than on hardware nobody has wired up yet.
//!
//! WHY IT EXISTS SEPARATELY. The kernel has a generic x86 table lookup and parses `APIC`, `SRAT` and
//! `SLIT`; the loader has a small RSDP-rooted reader for the ONE field it needs before it has a
//! memory map (which instruction reaches PSCI). Neither is a FADT parser, and the absence of an AML
//! interpreter does not supply one: every fixed-hardware item that follows - power button, sleep,
//! general-purpose events, the PM timer - needs the FADT's fields, and each would otherwise read them
//! out of the table with its own offsets and its own idea of which ones its revision may be believed
//! about.
//!
//! `&[u8]`-SHAPED, AND THAT IS THE DIFFERENCE FROM `fdt`. The device-tree reader takes a
//! `phys_to_virt` because one of its callers runs before the memory map is settled. Everything that
//! reads a FADT reads it through a mapping that already exists - the kernel's direct map - so the
//! bytes are a slice, which is also what makes a hostile fixture a byte array in a test rather than a
//! mapping a test has to fake.
//!
//! THREE RULES RUN THROUGH THE WHOLE FILE, because they are how a table lies:
//!
//!   1. THE DECLARED LENGTH IS CHECKED AGAINST THE SLICE AND NOTHING IS READ PAST IT. A header that
//!      says 276 bytes inside 200 is refused whole rather than read for the fields that happen to
//!      fit. Everything below indexes through `Table`, which cannot answer past that length.
//!   2. A FIELD THE REVISION OR THE LENGTH DOES NOT COVER IS `None` AND NEVER ZERO. Zero is a legal
//!      value of almost every field here - a zero `SMI_CMD` means "no SMI", a zero century index
//!      means "no century byte" - so a reader that returns zero for absent has destroyed the
//!      difference between a firmware that said nothing and one that said none.
//!   3. ARITHMETIC ON A FIELD IS CHECKED BEFORE THE FIELD IS BELIEVED. A register whose address plus
//!      its own span wraps a `u64` is refused here, once, rather than in each consumer that would
//!      otherwise compute an end address from numbers a firmware chose.

#![cfg_attr(not(test), no_std)]

/// Why a table, or a structure inside one, was refused.
///
/// ONE ERROR TYPE FOR THE CRATE, and each variant names a DIFFERENT way a real table is wrong -
/// which is what makes a fixture suite able to say which refusal it is testing. A single `Invalid`
/// would let a test pass because the table was rejected for the wrong reason.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
	/// Fewer bytes than a system description table header.
	TooShort,
	/// The header's own length field is smaller than the header it is in.
	LengthBelowHeader,
	/// The header's length field is larger than the bytes handed over.
	LengthPastEnd,
	/// The header's length field is larger than any real table.
	LengthUnreasonable,
	/// The bytes do not sum to zero over the declared length.
	Checksum,
	/// The table is not the one that was asked for.
	Signature,
	/// Fewer than twelve bytes were offered for a Generic Address Structure.
	GasTruncated,
	/// A Generic Address Structure whose access size is not one of the five the standard defines.
	GasAccessSize,
	/// A Generic Address Structure with an address and no width, or a width and no address.
	GasHalfPresent,
	/// A Generic Address Structure whose address plus its own span does not fit in a `u64`.
	GasAddressWraps,
}

/// Every system description table begins with the same 36-byte header.
pub const HEADER_LEN: usize = 36;

/// A Generic Address Structure is twelve bytes, in every table that carries one.
pub const GAS_LEN: usize = 12;

/// The largest table this reader will consider.
///
/// A BOUND ON THE DECLARED LENGTH IS NOT A BOUND ON REALITY, and it does not have to be: the point is
/// that a length field is a number the firmware chose, and a reader that accepts four billion has
/// agreed to walk four billion bytes looking for a checksum. A megabyte is larger than every fixed
/// table this system reads by three orders of magnitude.
pub const MAX_TABLE_LEN: usize = 0x10_0000;

/// A system description table whose header has been CHECKED: its length lies inside the bytes it was
/// given, and its bytes sum to zero over that length.
///
/// EVERY READ BELOW GOES THROUGH THIS. The accessors answer `None` past the declared length, so a
/// field extractor cannot read one byte further than the firmware said the table goes - which is the
/// whole of the "truncated table" half of this item, stated once rather than at each field.
#[derive(Clone, Copy)]
pub struct Table<'a> {
	// EXACTLY the declared length, never the slice that was handed over. A caller with a 4 KiB page
	// holding a 276-byte table gets a reader that stops at 276.
	bytes: &'a [u8],
}

impl<'a> Table<'a> {
	/// Validate a table: its header, its declared length against the bytes offered, and its checksum.
	pub fn new(bytes: &'a [u8]) -> Result<Self, Error> {
		if bytes.len() < HEADER_LEN {
			return Err(Error::TooShort);
		}
		let declared = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
		if declared < HEADER_LEN {
			return Err(Error::LengthBelowHeader);
		}
		if declared > MAX_TABLE_LEN {
			return Err(Error::LengthUnreasonable);
		}
		if declared > bytes.len() {
			return Err(Error::LengthPastEnd);
		}
		let bytes = &bytes[..declared];
		// THE CHECKSUM IS A RULE EVERY ACPI STRUCTURE SHARES: the bytes sum to zero in eight bits.
		// A table that fails it is a table the firmware did not finish writing, or one this reader
		// was pointed at by mistake, and either way its fields are not answers.
		let mut sum: u8 = 0;
		for byte in bytes {
			sum = sum.wrapping_add(*byte);
		}
		if sum != 0 {
			return Err(Error::Checksum);
		}
		Ok(Self { bytes })
	}

	/// The same, requiring a signature. `Table::new` plus a comparison, in the order that refuses the
	/// cheapest thing first.
	pub fn with_signature(bytes: &'a [u8], signature: &[u8; 4]) -> Result<Self, Error> {
		let table = Self::new(bytes)?;
		if &table.signature() != signature {
			return Err(Error::Signature);
		}
		Ok(table)
	}

	pub fn signature(&self) -> [u8; 4] {
		[self.bytes[0], self.bytes[1], self.bytes[2], self.bytes[3]]
	}

	/// The table's own revision, which decides which fields it HAS. Not the ACPI version: a FADT of
	/// revision 1 is ACPI 1.0's 116-byte table and one of revision 6 is 276 bytes.
	pub fn revision(&self) -> u8 {
		self.bytes[8]
	}

	/// The declared length, which is also the length of `bytes`.
	pub fn len(&self) -> usize {
		self.bytes.len()
	}

	/// Never true: a validated table is at least a header. Present because `len` without it is a
	/// lint, and because saying so is cheaper than the reader wondering.
	pub fn is_empty(&self) -> bool {
		false
	}

	/// The validated bytes, for a caller that needs the table itself rather than a field of it.
	pub fn bytes(&self) -> &'a [u8] {
		self.bytes
	}

	pub fn u8_at(&self, offset: usize) -> Option<u8> {
		self.bytes.get(offset).copied()
	}

	pub fn u16_at(&self, offset: usize) -> Option<u16> {
		let bytes = self.slice_at(offset, 2)?;
		Some(u16::from_le_bytes([bytes[0], bytes[1]]))
	}

	pub fn u32_at(&self, offset: usize) -> Option<u32> {
		let bytes = self.slice_at(offset, 4)?;
		Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
	}

	pub fn u64_at(&self, offset: usize) -> Option<u64> {
		let bytes = self.slice_at(offset, 8)?;
		Some(u64::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]]))
	}

	/// A Generic Address Structure at an offset, or `None` when the table is too short for one - and
	/// an `Err` when there are twelve bytes there and they are not a structure.
	///
	/// THE TWO CASES ARE DIFFERENT ANSWERS. "This revision does not carry that register" is an absent
	/// field; "this register is described with an access size that does not exist" is a broken one.
	pub fn gas_at(&self, offset: usize) -> Option<Result<Gas, Error>> {
		let bytes = self.slice_at(offset, GAS_LEN)?;
		Some(Gas::decode(bytes))
	}

	fn slice_at(&self, offset: usize, len: usize) -> Option<&'a [u8]> {
		let end = offset.checked_add(len)?;
		self.bytes.get(offset..end)
	}
}

/// Which address space a Generic Address Structure's address is in.
///
/// TOTAL, WITH THE UNNAMED VALUES KEPT RATHER THAN COLLAPSED. A consumer that reaches an
/// `EmbeddedController` address with a memory read is reading whatever is at that physical address,
/// so "an address space this reader does not name" has to be distinguishable from system memory -
/// and OEM-defined spaces have to be distinguishable from reserved ones, because one is a firmware
/// using its own allocation and the other is a firmware writing a value that means nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AddressSpace {
	SystemMemory,
	SystemIo,
	PciConfiguration,
	EmbeddedController,
	SmBus,
	SystemCmos,
	PciBarTarget,
	Ipmi,
	GeneralPurposeIo,
	GenericSerialBus,
	PlatformCommunications,
	/// Space 0x7F: the register is reached by an architecture-specific mechanism rather than by an
	/// address, which is how ARM describes a PSCI-driven register and x86 an MSR.
	FunctionalFixedHardware,
	/// 0x80 and above: the firmware's own, and not this reader's to interpret.
	Oem(u8),
	/// Everything else the standard has not defined.
	Reserved(u8),
}

impl AddressSpace {
	pub const fn decode(value: u8) -> Self {
		match value {
			0x00 => AddressSpace::SystemMemory,
			0x01 => AddressSpace::SystemIo,
			0x02 => AddressSpace::PciConfiguration,
			0x03 => AddressSpace::EmbeddedController,
			0x04 => AddressSpace::SmBus,
			0x05 => AddressSpace::SystemCmos,
			0x06 => AddressSpace::PciBarTarget,
			0x07 => AddressSpace::Ipmi,
			0x08 => AddressSpace::GeneralPurposeIo,
			0x09 => AddressSpace::GenericSerialBus,
			0x0a => AddressSpace::PlatformCommunications,
			0x7f => AddressSpace::FunctionalFixedHardware,
			0x80..=0xff => AddressSpace::Oem(value),
			other => AddressSpace::Reserved(other),
		}
	}

	/// Whether an ordinary load or store reaches this space. A caller that wants to READ a register
	/// asks this rather than matching, because the answer is the same for every consumer and the
	/// match is where one of them forgets a case.
	pub const fn is_directly_addressable(&self) -> bool {
		matches!(self, AddressSpace::SystemMemory | AddressSpace::SystemIo)
	}
}

/// How wide one access to a register is.
///
/// `Undefined` IS AN ANSWER AND NOT A DEFAULT. A firmware that writes zero has declined to say, and
/// the consumer's fallback - usually the register's own width - is the consumer's decision to state,
/// not something to hide by pretending the table said "byte".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccessSize {
	Undefined,
	Byte,
	Word,
	Dword,
	Qword,
}

impl AccessSize {
	pub const fn decode(value: u8) -> Option<Self> {
		match value {
			0 => Some(AccessSize::Undefined),
			1 => Some(AccessSize::Byte),
			2 => Some(AccessSize::Word),
			3 => Some(AccessSize::Dword),
			4 => Some(AccessSize::Qword),
			// FIVE AND ABOVE ARE NOT AN ACCESS SIZE. Reading such a table as "byte" is how a register
			// declared with a wild value gets touched at all.
			_ => None,
		}
	}

	/// The access width in bytes, or `None` when the table did not say.
	pub const fn bytes(&self) -> Option<u64> {
		match self {
			AccessSize::Undefined => None,
			AccessSize::Byte => Some(1),
			AccessSize::Word => Some(2),
			AccessSize::Dword => Some(4),
			AccessSize::Qword => Some(8),
		}
	}
}

/// A Generic Address Structure: where a register is, how wide it is, and how it is reached.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gas {
	pub space: AddressSpace,
	pub bit_width: u8,
	pub bit_offset: u8,
	pub access: AccessSize,
	pub address: u64,
}

impl Gas {
	/// Decode twelve bytes, refusing the three things a structure can say that cannot be true.
	pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
		if bytes.len() < GAS_LEN {
			return Err(Error::GasTruncated);
		}
		let access = AccessSize::decode(bytes[3]).ok_or(Error::GasAccessSize)?;
		let address = u64::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11]]);
		let gas = Gas { space: AddressSpace::decode(bytes[0]), bit_width: bytes[1], bit_offset: bytes[2], access, address };
		// ALL ZEROES IS THE STANDARD'S OWN WAY OF SAYING "NOT IMPLEMENTED", and it is the common case
		// in every FADT: a machine with no PM1b block writes twelve zero bytes.
		if gas.is_absent() {
			return Ok(gas);
		}
		// AN ADDRESS WITH NO WIDTH, OR A WIDTH WITH NO ADDRESS, IS HALF A REGISTER. Consumers reading
		// one of the two fields would disagree about whether the register exists, which is the shape
		// of a bug that appears only on the one machine whose firmware does this.
		if (gas.address == 0) != (gas.bit_width == 0) {
			return Err(Error::GasHalfPresent);
		}
		// AND THE END ADDRESS IS COMPUTED ONCE, HERE, so no consumer computes one from numbers that
		// wrap. A register at the top of the address space whose width runs past it is not a
		// register, and the consumer that would have mapped it is the one that would have wrapped.
		if gas.address.checked_add(gas.span_bytes()).is_none() {
			return Err(Error::GasAddressWraps);
		}
		Ok(gas)
	}

	/// Whether the structure describes no register at all.
	pub const fn is_absent(&self) -> bool {
		self.address == 0 && self.bit_width == 0 && self.bit_offset == 0
	}

	/// Whether it describes one.
	pub const fn is_present(&self) -> bool {
		self.address != 0 && self.bit_width != 0
	}

	/// How many bytes the register occupies from its address: the bits it declares, plus the offset
	/// they start at, rounded up.
	///
	/// IT CANNOT OVERFLOW AND THE TYPES SAY SO. Both fields are bytes, so the widest possible span is
	/// sixty-four; the arithmetic that CAN overflow is the address plus this, and `decode` is where
	/// that is checked, once, for every consumer.
	pub const fn span_bytes(&self) -> u64 {
		((self.bit_offset as u64) + (self.bit_width as u64)).div_ceil(8)
	}
}

/// What a machine is FOR, as the firmware declares it. Read, never acted on here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PowerProfile {
	Unspecified,
	Desktop,
	Mobile,
	Workstation,
	EnterpriseServer,
	SohoServer,
	AppliancePc,
	PerformanceServer,
	Tablet,
	/// A profile this reader does not name. Kept rather than folded into `Unspecified`, which is a
	/// firmware SAYING nothing rather than saying something new.
	Other(u8),
}

impl PowerProfile {
	const fn decode(value: u8) -> Self {
		match value {
			0 => PowerProfile::Unspecified,
			1 => PowerProfile::Desktop,
			2 => PowerProfile::Mobile,
			3 => PowerProfile::Workstation,
			4 => PowerProfile::EnterpriseServer,
			5 => PowerProfile::SohoServer,
			6 => PowerProfile::AppliancePc,
			7 => PowerProfile::PerformanceServer,
			8 => PowerProfile::Tablet,
			other => PowerProfile::Other(other),
		}
	}
}

/// Which form of the FADT a register block's answer came from.
///
/// IT TRAVELS WITH THE ANSWER because a consumer debugging a machine needs to know which of the two
/// descriptions it is looking at, and because `agrees` is only meaningful when both exist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockSource {
	/// The 64-bit `X_` Generic Address Structure, which the standard requires an OS to prefer.
	Extended,
	/// The 32-bit legacy field plus its `*_LEN` byte, synthesised into a system-I/O register.
	Legacy,
}

/// A fixed-hardware register block, resolved from the two forms a FADT carries for it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Block {
	pub gas: Gas,
	/// The byte length the legacy `*_LEN` field declares for this block.
	pub declared_bytes: u8,
	pub source: BlockSource,
}

impl Block {
	/// THE ENABLE REGISTER OF AN EVENT BLOCK, which is HALF A BLOCK PAST THE STATUS REGISTER.
	///
	/// A PM1 event block is TWO REGISTERS AND THE FADT DECLARES THEIR TOTAL: status is the lower half
	/// and enable the upper half. That one piece of arithmetic is the whole of this function, and it
	/// is here rather than in the consumer because of what getting it wrong does - writing the enable
	/// bits into the STATUS register ACKNOWLEDGES events instead of arming them, which changes
	/// nothing a reader can see and leaves a machine whose power button does nothing.
	///
	/// A DECLARED LENGTH THAT IS NOT TWO REGISTERS IS REFUSED RATHER THAN HALVED. One byte has no
	/// second half at all, and an odd length cannot be halved into two registers of the same width -
	/// taking the floor would put the enable register one byte INSIDE the status register. Zero is
	/// the absent case and is refused with them.
	///
	/// IT ANSWERS FOR SYSTEM-I/O BLOCKS ONLY, because a port is sixteen bits and the address of an
	/// event block in any other space is not one. A caller with a memory-mapped block computes its
	/// own offset from the same rule.
	pub fn enable_port(&self) -> Option<u16> {
		if self.gas.space != AddressSpace::SystemIo {
			return None;
		}
		if self.declared_bytes < 2 || self.declared_bytes % 2 != 0 {
			return None;
		}
		let base = u16::try_from(self.gas.address).ok()?;
		base.checked_add(self.declared_bytes as u16 / 2)
	}

	/// The system-I/O port a block sits at, or `None` for a block in another address space.
	///
	/// THE STANDARD ALLOWS THESE BLOCKS IN SYSTEM MEMORY and every machine this reader runs on puts
	/// them in port I/O. A consumer that treated an address as a port because it expected one would
	/// be writing to a port numbered by the low sixteen bits of a physical address, which is a real
	/// port belonging to something else.
	pub fn io_port(&self) -> Option<u16> {
		if self.gas.space != AddressSpace::SystemIo {
			return None;
		}
		u16::try_from(self.gas.address).ok()
	}

	/// Whether the extended form's width and the legacy length say the same thing.
	///
	/// A FIRMWARE WHOSE TWO DESCRIPTIONS DISAGREE IS A REAL DEFECT and the consumer is the one that
	/// must decide what to do about it - use the one the standard prefers, or refuse the machine. It
	/// is reported rather than resolved here, because resolving it silently is how the disagreement
	/// stops being visible to anybody.
	pub const fn agrees(&self) -> bool {
		self.gas.bit_width as u64 == self.declared_bytes as u64 * 8
	}
}

/// The PM timer, which is a block plus the one bit that says how much of it counts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Timer {
	pub block: Block,
	/// 24 or 32. The register is always 32 bits wide; `TMR_VAL_EXT` says whether the top eight of
	/// them count - and a consumer that assumes 32 on a 24-bit timer sees time jump backwards once
	/// every four seconds.
	pub counting_bits: u8,
}

/// A general-purpose event block and where its events start in the GPE namespace.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GpeBlock {
	pub block: Block,
	/// GPE0 starts at zero by definition; GPE1 starts at `GPE1_BASE`.
	pub base: u16,
}

/// The reset mechanism, which exists only when the flags say it does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reset {
	pub register: Gas,
	pub value: u8,
}

/// The x86 boot architecture flags: what legacy hardware this machine still has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IapcBoot {
	pub legacy_devices: bool,
	pub has_8042: bool,
	pub vga_not_present: bool,
	pub msi_not_supported: bool,
	pub pcie_aspm_controls: bool,
	pub cmos_rtc_not_present: bool,
}

/// The ARM boot architecture flags: whether PSCI exists, and which instruction reaches it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ArmBoot {
	pub psci_compliant: bool,
	/// `HVC` when true, `SMC` when false. Meaningless unless `psci_compliant`.
	pub use_hvc: bool,
}

// The FADT's field offsets, written out once. Every one of them is from the standard's own table and
// none is computed: a layout described by arithmetic over previous fields is a layout that moves when
// somebody miscounts one of them.
const FADT_FIRMWARE_CTRL: usize = 36;
const FADT_DSDT: usize = 40;
const FADT_POWER_PROFILE: usize = 45;
const FADT_SCI_INT: usize = 46;
const FADT_SMI_CMD: usize = 48;
const FADT_ACPI_ENABLE: usize = 52;
const FADT_ACPI_DISABLE: usize = 53;
const FADT_S4BIOS_REQ: usize = 54;
const FADT_PSTATE_CNT: usize = 55;
const FADT_PM1A_EVT: usize = 56;
const FADT_PM1B_EVT: usize = 60;
const FADT_PM1A_CNT: usize = 64;
const FADT_PM1B_CNT: usize = 68;
const FADT_PM2_CNT: usize = 72;
const FADT_PM_TMR: usize = 76;
const FADT_GPE0: usize = 80;
const FADT_GPE1: usize = 84;
const FADT_PM1_EVT_LEN: usize = 88;
const FADT_PM1_CNT_LEN: usize = 89;
const FADT_PM2_CNT_LEN: usize = 90;
const FADT_PM_TMR_LEN: usize = 91;
const FADT_GPE0_LEN: usize = 92;
const FADT_GPE1_LEN: usize = 93;
const FADT_GPE1_BASE: usize = 94;
const FADT_CENTURY: usize = 108;
const FADT_IAPC_BOOT: usize = 109;
const FADT_FLAGS: usize = 112;
const FADT_RESET_REG: usize = 116;
const FADT_RESET_VALUE: usize = 128;
const FADT_ARM_BOOT: usize = 129;
const FADT_MINOR_VERSION: usize = 131;
const FADT_X_FIRMWARE_CTRL: usize = 132;
const FADT_X_DSDT: usize = 140;
const FADT_X_PM1A_EVT: usize = 148;
const FADT_X_PM1B_EVT: usize = 160;
const FADT_X_PM1A_CNT: usize = 172;
const FADT_X_PM1B_CNT: usize = 184;
const FADT_X_PM2_CNT: usize = 196;
const FADT_X_PM_TMR: usize = 208;
const FADT_X_GPE0: usize = 220;
const FADT_X_GPE1: usize = 232;
const FADT_SLEEP_CONTROL: usize = 244;
const FADT_SLEEP_STATUS: usize = 256;

/// Fixed feature flags, at offset 112. Only the ones a decision in this tree turns on are named.
const FLAG_TMR_VAL_EXT: u32 = 1 << 8;
const FLAG_RESET_REG_SUP: u32 = 1 << 10;
const FLAG_HW_REDUCED_ACPI: u32 = 1 << 20;

/// The Fixed ACPI Description Table, read field by field and never further than it goes.
pub struct Fadt<'a> {
	table: Table<'a>,
}

impl<'a> Fadt<'a> {
	/// Validate and adopt a FADT. The signature is `FACP`, which is the one place in ACPI where the
	/// table's name and its signature differ.
	pub fn new(bytes: &'a [u8]) -> Result<Self, Error> {
		Ok(Self { table: Table::with_signature(bytes, b"FACP")? })
	}

	/// Adopt a table somebody else already validated.
	pub fn from_table(table: Table<'a>) -> Result<Self, Error> {
		if &table.signature() != b"FACP" {
			return Err(Error::Signature);
		}
		Ok(Self { table })
	}

	pub fn table(&self) -> &Table<'a> {
		&self.table
	}

	pub fn revision(&self) -> u8 {
		self.table.revision()
	}

	/// The minor version, which arrived in revision 5 and distinguishes ACPI 5.0 from 5.1 - the
	/// revision that added the ARM boot flags.
	pub fn minor_version(&self) -> Option<u8> {
		self.table.u8_at(FADT_MINOR_VERSION)
	}

	pub fn flags(&self) -> Option<u32> {
		self.table.u32_at(FADT_FLAGS)
	}

	/// HARDWARE-REDUCED ACPI: there are no PM1, PM2, PM timer or GPE blocks on this machine, and the
	/// fields that would describe them mean nothing.
	///
	/// FALSE WHEN THE FLAGS ARE ABSENT, which is the honest reading: a table too short to carry the
	/// flags is an ACPI 1.0 table, and ACPI 1.0 has no hardware-reduced mode.
	pub fn hardware_reduced(&self) -> bool {
		self.flags().map(|flags| flags & FLAG_HW_REDUCED_ACPI != 0).unwrap_or(false)
	}

	pub fn power_profile(&self) -> Option<PowerProfile> {
		self.table.u8_at(FADT_POWER_PROFILE).map(PowerProfile::decode)
	}

	/// The interrupt the machine signals ACPI events on. Meaningless in hardware-reduced mode, where
	/// events arrive as ordinary interrupts described elsewhere.
	pub fn sci_interrupt(&self) -> Option<u16> {
		if self.hardware_reduced() {
			return None;
		}
		self.table.u16_at(FADT_SCI_INT)
	}

	/// The port that switches the machine into ACPI mode, or `None` when there is none.
	///
	/// ZERO IS "NO SMI COMMAND PORT" AND IS REPORTED AS ABSENCE, which is the standard's own rule and
	/// the case on every machine whose firmware boots already in ACPI mode. A consumer that wrote to
	/// port zero because the field was zero would be writing to a port that belongs to the DMA
	/// controller.
	pub fn smi_command(&self) -> Option<u32> {
		if self.hardware_reduced() {
			return None;
		}
		self.table.u32_at(FADT_SMI_CMD).filter(|port| *port != 0)
	}

	/// The value to write to the SMI command port to enter ACPI mode, paired with the one that leaves
	/// it. Both or neither: a consumer that can enter and not leave has half a mechanism.
	pub fn acpi_mode_values(&self) -> Option<(u8, u8)> {
		self.smi_command()?;
		Some((self.table.u8_at(FADT_ACPI_ENABLE)?, self.table.u8_at(FADT_ACPI_DISABLE)?))
	}

	/// The value that requests an S4 entry through the SMI command port, or `None` when the firmware
	/// declines to offer one.
	pub fn s4bios_request(&self) -> Option<u8> {
		self.smi_command()?;
		self.table.u8_at(FADT_S4BIOS_REQ).filter(|value| *value != 0)
	}

	/// The value that hands processor performance control to the OS, on the same terms.
	pub fn pstate_control(&self) -> Option<u8> {
		self.smi_command()?;
		self.table.u8_at(FADT_PSTATE_CNT).filter(|value| *value != 0)
	}

	/// Where the FACS is, preferring the 64-bit field.
	pub fn facs(&self) -> Option<u64> {
		self.preferred_pointer(FADT_X_FIRMWARE_CTRL, FADT_FIRMWARE_CTRL)
	}

	/// Where the DSDT is, on the same rule.
	pub fn dsdt(&self) -> Option<u64> {
		self.preferred_pointer(FADT_X_DSDT, FADT_DSDT)
	}

	pub fn pm1a_event(&self) -> Option<Block> {
		self.block(FADT_X_PM1A_EVT, FADT_PM1A_EVT, FADT_PM1_EVT_LEN)
	}

	pub fn pm1b_event(&self) -> Option<Block> {
		self.block(FADT_X_PM1B_EVT, FADT_PM1B_EVT, FADT_PM1_EVT_LEN)
	}

	pub fn pm1a_control(&self) -> Option<Block> {
		self.block(FADT_X_PM1A_CNT, FADT_PM1A_CNT, FADT_PM1_CNT_LEN)
	}

	pub fn pm1b_control(&self) -> Option<Block> {
		self.block(FADT_X_PM1B_CNT, FADT_PM1B_CNT, FADT_PM1_CNT_LEN)
	}

	pub fn pm2_control(&self) -> Option<Block> {
		self.block(FADT_X_PM2_CNT, FADT_PM2_CNT, FADT_PM2_CNT_LEN)
	}

	/// The power management timer, with the width its own flag decides.
	pub fn timer(&self) -> Option<Timer> {
		let block = self.block(FADT_X_PM_TMR, FADT_PM_TMR, FADT_PM_TMR_LEN)?;
		let extended = self.flags().map(|flags| flags & FLAG_TMR_VAL_EXT != 0).unwrap_or(false);
		Some(Timer { block, counting_bits: if extended { 32 } else { 24 } })
	}

	pub fn gpe0(&self) -> Option<GpeBlock> {
		Some(GpeBlock { block: self.block(FADT_X_GPE0, FADT_GPE0, FADT_GPE0_LEN)?, base: 0 })
	}

	/// The second GPE block, whose events are numbered from `GPE1_BASE` rather than from the end of
	/// the first block.
	pub fn gpe1(&self) -> Option<GpeBlock> {
		let block = self.block(FADT_X_GPE1, FADT_GPE1, FADT_GPE1_LEN)?;
		Some(GpeBlock { block, base: self.table.u8_at(FADT_GPE1_BASE)? as u16 })
	}

	/// The sleep control register, which is how a hardware-reduced machine is put to sleep.
	///
	/// NOT GATED ON HARDWARE-REDUCED MODE, deliberately: the registers arrived with revision 5 and a
	/// machine may describe them either way. What IS gated is the fixed-hardware block set above,
	/// because those the standard says are meaningless when the flag is set.
	pub fn sleep_control(&self) -> Option<Gas> {
		self.present_gas(FADT_SLEEP_CONTROL)
	}

	pub fn sleep_status(&self) -> Option<Gas> {
		self.present_gas(FADT_SLEEP_STATUS)
	}

	/// The reset register and the value to write to it, or `None` when this machine has no ACPI reset.
	///
	/// THREE CONDITIONS AND ALL THREE ARE REQUIRED: the flags must say the mechanism is supported, the
	/// table must be long enough to carry the register, and the register must describe something. A
	/// consumer that wrote the reset value to a register the firmware never filled in would be
	/// writing an arbitrary byte to address zero of whatever space the zeroes decoded to.
	pub fn reset(&self) -> Option<Reset> {
		let flags = self.flags()?;
		if flags & FLAG_RESET_REG_SUP == 0 {
			return None;
		}
		let register = self.present_gas(FADT_RESET_REG)?;
		Some(Reset { register, value: self.table.u8_at(FADT_RESET_VALUE)? })
	}

	/// The CMOS index of the century byte, or `None`.
	///
	/// TWO WAYS TO BE ABSENT AND BOTH MATTER: zero means the firmware declines to name one, and a
	/// machine whose boot flags say there is no CMOS RTC has nowhere for the index to point. A reader
	/// that answered the raw byte would have consumers indexing a CMOS that is not there.
	pub fn century_index(&self) -> Option<u8> {
		if self.iapc_boot().map(|boot| boot.cmos_rtc_not_present).unwrap_or(false) {
			return None;
		}
		self.table.u8_at(FADT_CENTURY).filter(|index| *index != 0)
	}

	/// The x86 boot architecture flags, which arrived in revision 2.
	pub fn iapc_boot(&self) -> Option<IapcBoot> {
		if self.revision() < 2 {
			return None;
		}
		let flags = self.table.u16_at(FADT_IAPC_BOOT)?;
		Some(IapcBoot { legacy_devices: flags & 0x0001 != 0, has_8042: flags & 0x0002 != 0, vga_not_present: flags & 0x0004 != 0, msi_not_supported: flags & 0x0008 != 0, pcie_aspm_controls: flags & 0x0010 != 0, cmos_rtc_not_present: flags & 0x0020 != 0 })
	}

	/// The ARM boot architecture flags, which arrived in revision 5.
	///
	/// `None` RATHER THAN "NO PSCI" WHEN THE FIELD IS ABSENT. The two are different answers: a table
	/// that does not carry the field has said nothing about PSCI, and a caller deciding how to bring
	/// up a secondary processor has to know which it is looking at.
	pub fn arm_boot(&self) -> Option<ArmBoot> {
		if self.revision() < 5 {
			return None;
		}
		let flags = self.table.u16_at(FADT_ARM_BOOT)?;
		Some(ArmBoot { psci_compliant: flags & 0x0001 != 0, use_hvc: flags & 0x0002 != 0 })
	}

	// The 64-bit pointer when the table carries one and it is non-zero, else the 32-bit one. The
	// standard's own rule, and the reason it exists: a machine with a table above four gigabytes
	// writes zero in the 32-bit field.
	fn preferred_pointer(&self, extended: usize, legacy: usize) -> Option<u64> {
		if let Some(address) = self.table.u64_at(extended)
			&& address != 0
		{
			return Some(address);
		}
		self.table.u32_at(legacy).map(u64::from).filter(|address| *address != 0)
	}

	// A Generic Address Structure that is both well-formed AND describes a register. A structure of
	// twelve zeroes is well-formed and describes nothing, which is absence rather than an error.
	fn present_gas(&self, offset: usize) -> Option<Gas> {
		self.table.gas_at(offset)?.ok().filter(Gas::is_present)
	}

	// One fixed-hardware register block, from whichever of the two forms the table carries.
	//
	// THE EXTENDED FORM WINS, which is what the standard requires of an OS that understands both -
	// and the legacy fields are kept beside it rather than discarded, because the `*_LEN` byte is the
	// only place the block's LENGTH is stated in the legacy form and the only thing `agrees` can
	// compare the extended width against.
	fn block(&self, extended: usize, legacy: usize, length: usize) -> Option<Block> {
		// HARDWARE-REDUCED MACHINES HAVE NO FIXED HARDWARE, and the standard says these fields are to
		// be ignored there. Answering with them anyway is how a consumer comes to write to an I/O
		// port on a machine that has no I/O ports.
		if self.hardware_reduced() {
			return None;
		}
		let declared_bytes = self.table.u8_at(length)?;
		if let Some(Ok(gas)) = self.table.gas_at(extended)
			&& gas.is_present()
		{
			return Some(Block { gas, declared_bytes, source: BlockSource::Extended });
		}
		let address = self.table.u32_at(legacy)?;
		if address == 0 || declared_bytes == 0 {
			return None;
		}
		// THE LEGACY FORM IS A SYSTEM-I/O REGISTER whose width is its declared length in bits. That
		// is what the legacy fields mean - they predate the address-space vocabulary - and
		// synthesising the structure here is what lets every consumer take one shape.
		let bit_width = (declared_bytes as u32 * 8).min(u8::MAX as u32) as u8;
		let gas = Gas { space: AddressSpace::SystemIo, bit_width, bit_offset: 0, access: AccessSize::Undefined, address: address as u64 };
		Some(Block { gas, declared_bytes, source: BlockSource::Legacy })
	}
}

/// How an interrupt line is asserted, as the MADT's flags word states it.
///
/// THE "CONFORMS TO THE BUS" VALUE IS KEPT AND NOT RESOLVED HERE. Zero means "whatever this source's
/// bus specifies", and what that is depends on the bus: an ISA line is edge-triggered and
/// active-high, a PCI line is level-triggered and active-low. A reader that collapsed zero into one
/// of those would be answering a question about a BUS while reading a table about an INTERRUPT, and
/// the caller is the one that knows which bus its source is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Polarity {
	/// Conforms to the specification of the bus the source is on.
	Bus,
	ActiveHigh,
	ActiveLow,
	/// The reserved encoding, kept rather than mapped onto one of the two real ones.
	Reserved,
}

/// How an interrupt line is delivered, as the MADT's flags word states it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
	/// Conforms to the specification of the bus the source is on.
	Bus,
	Edge,
	Level,
	/// The reserved encoding, kept rather than mapped onto one of the two real ones.
	Reserved,
}

/// One MADT Interrupt Source Override: an ISA source that does NOT arrive at the Global System
/// Interrupt of the same number, or that does not arrive the way its bus would say.
///
/// BOTH HALVES MATTER AND FIRMWARE USES BOTH. The classic PC remaps the timer - ISA IRQ 0 arrives
/// at GSI 2 - which is the redirection half; and QEMU's q35 leaves the SCI at its own number and
/// overrides only its POLARITY AND TRIGGER, because an ISA line defaults to edge-triggered
/// active-high and the SCI is neither. A reader that took only the GSI would route the SCI to the
/// right pin and configure it as the wrong kind of line, which on a level source means it is
/// delivered once and then never again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InterruptOverride {
	/// The bus the source is on. Zero is ISA and is the only value the standard defines.
	pub bus: u8,
	/// The source's number on that bus - an ISA IRQ, for bus zero.
	pub source: u8,
	/// The Global System Interrupt it actually arrives at.
	pub gsi: u32,
	pub polarity: Polarity,
	pub trigger: Trigger,
}

/// The Multiple APIC Description Table, read for the entries this system acts on.
///
/// WHAT IT IS FOR HERE IS THE OVERRIDES AND NOT THE PROCESSORS. The kernel already walks this table
/// for local APIC ids in its own boot path, where it runs before there is anything to test with; the
/// entries below are DECISIONS about how a line is configured, and a decision belongs where a fixture
/// can plant the mistake. A caller wanting the processors keeps its own walk.
#[derive(Clone, Copy)]
pub struct Madt<'a> {
	table: Table<'a>,
}

/// The MADT's entries begin after the 36-byte header, a 4-byte local controller address and a
/// 4-byte flags word.
const MADT_ENTRIES: usize = 44;

/// An Interrupt Source Override entry is type 2 and ten bytes.
const MADT_OVERRIDE: u8 = 2;
const MADT_OVERRIDE_LEN: usize = 10;

impl<'a> Madt<'a> {
	/// Validate the bytes as an `APIC` table.
	pub fn new(bytes: &'a [u8]) -> Result<Self, Error> {
		Ok(Self { table: Table::with_signature(bytes, b"APIC")? })
	}

	pub fn table(&self) -> &Table<'a> {
		&self.table
	}

	/// The override for `source` on the ISA bus, or `None` when the firmware states none.
	///
	/// `None` IS AN ANSWER AND NOT A FAILURE. A source with no override arrives at the Global System
	/// Interrupt of its own number, configured the way its bus specifies - which is what the standard
	/// says in as many words, and is why the absence has to be distinguishable from a table this
	/// reader could not walk.
	///
	/// THE FIRST MATCH WINS. A table listing the same source twice is firmware disagreeing with
	/// itself, and taking the last would mean the answer depended on how far the walk got.
	pub fn isa_override(&self, source: u8) -> Option<InterruptOverride> {
		self.overrides().find(|entry| entry.bus == 0 && entry.source == source)
	}

	/// Every Interrupt Source Override the table carries, in the order the firmware wrote them.
	///
	/// AN ENTRY THAT DOES NOT FIT INSIDE THE TABLE ENDS THE WALK. A length of zero would not advance
	/// and a length running past the declared end is a structure this table does not contain; both
	/// are firmware that is structurally wrong under a checksum that passed, which is the case a
	/// boot path has nobody to complain to about.
	pub fn overrides(&self) -> impl Iterator<Item = InterruptOverride> + '_ {
		let mut offset = MADT_ENTRIES;
		core::iter::from_fn(move || {
			loop {
				let kind = self.table.u8_at(offset)?;
				let len = self.table.u8_at(offset + 1)? as usize;
				if len < 2 || offset.checked_add(len)? > self.table.len() {
					return None;
				}
				let here = offset;
				offset += len;
				// THE DECLARED LENGTH HAS TO COVER THE ENTRY, not merely fit in the table. An
				// override declaring eight bytes has no flags word, and reading one out of the next
				// entry is how a walk answers a question about the wrong structure.
				if kind == MADT_OVERRIDE && len >= MADT_OVERRIDE_LEN {
					let bus = self.table.u8_at(here + 2)?;
					let source = self.table.u8_at(here + 3)?;
					let gsi = self.table.u32_at(here + 4)?;
					let flags = self.table.u16_at(here + 8)?;
					return Some(InterruptOverride { bus, source, gsi, polarity: polarity_of(flags), trigger: trigger_of(flags) });
				}
			}
		})
	}
}

/// Bits 1:0 of an MPS INTI flags word.
fn polarity_of(flags: u16) -> Polarity {
	match flags & 0b11 {
		0 => Polarity::Bus,
		1 => Polarity::ActiveHigh,
		3 => Polarity::ActiveLow,
		_ => Polarity::Reserved,
	}
}

/// Bits 3:2 of an MPS INTI flags word.
///
/// THE SHIFT IS TWO AND THE MASK IS TWO BITS, which is the mistake this function exists to make once.
/// A reader masking the whole low nibble reads polarity and trigger together as one number, and
/// every override that states both comes back as a value neither enumeration has.
fn trigger_of(flags: u16) -> Trigger {
	match (flags >> 2) & 0b11 {
		0 => Trigger::Bus,
		1 => Trigger::Edge,
		3 => Trigger::Level,
		_ => Trigger::Reserved,
	}
}

#[cfg(test)]
mod tests;
