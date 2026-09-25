// WHERE THE TPM IS, FROM THE `TPM2` STATIC TABLE (TCG ACPI Specification) - which needs no AML: its start method
// says which interface, and its control area address says where a CRB's registers are. The FIFO interface has
// no address in the table; on a PC it is at the platform profile's fixed base.
//
// ONLY THE TWO METHODS THIS CRATE DRIVES ARE ANSWERED. A start method that needs an ACPI method run (the ACPI
// start, CRB with ACPI start) needs the AML interpreter nobody owns; one that rings a secure monitor is an Arm
// firmware call; one over I2C is a bus this tree has no controller for. Each is refused by name rather than
// guessed at, and a CRB whose control area is not at the offset the platform profile puts it is refused too:
// its register block would be wherever a guess put it.

use acpi::Table;

pub const START_METHOD_ACPI: u32 = 2;
pub const START_METHOD_FIFO: u32 = 6;
pub const START_METHOD_CRB: u32 = 7;
pub const START_METHOD_CRB_ACPI: u32 = 8;
pub const START_METHOD_CRB_SMC: u32 = 11;
pub const START_METHOD_FIFO_I2C: u32 = 12;

/// The FIFO interface's base on a PC, and the size of its five localities' registers.
pub const FIFO_BASE: u64 = 0xFED4_0000;
pub const REGION_LEN: usize = 0x5000;
/// Where a CRB's control area sits in its locality's register block.
pub const CRB_CONTROL_OFFSET: u64 = 0x40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interface {
	/// The FIFO interface, with its registers at `base`.
	Fifo { base: u64 },
	/// A CRB, with its locality-0 register block at `base`.
	Crb { base: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
	/// The bytes are not a valid `TPM2` table.
	Table(acpi::Error),
	/// Shorter than the fields this reads.
	Short,
	/// A start method this crate does not drive, by its number.
	Unsupported(u32),
	/// A CRB control area at an address the platform profile does not put one.
	Layout,
}

/// The interface a `TPM2` table describes.
pub fn discover(bytes: &[u8]) -> Result<Interface, Refusal> {
	let table = Table::with_signature(bytes, b"TPM2").map_err(Refusal::Table)?;
	let control = table.u64_at(40).ok_or(Refusal::Short)?;
	let method = table.u32_at(48).ok_or(Refusal::Short)?;
	match method {
		START_METHOD_FIFO => Ok(Interface::Fifo { base: FIFO_BASE }),
		START_METHOD_CRB => {
			if control % 0x1000 != CRB_CONTROL_OFFSET {
				return Err(Refusal::Layout);
			}
			Ok(Interface::Crb { base: control - CRB_CONTROL_OFFSET })
		}
		other => Err(Refusal::Unsupported(other)),
	}
}
