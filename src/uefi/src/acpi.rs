//! ACPI table discovery, and the one table that says where the console is.
//!
//! A machine that boots this loader through UEFI describes itself with a device tree or with ACPI,
//! and the device-tree half is the `fdt` crate. This is the other half: the RSDP the firmware puts
//! in its configuration table, the XSDT it points at, and SPCR - the Serial Port Console
//! Redirection table, which exists for exactly the question P02M0129 has been carrying since it
//! opened. "Where does this machine's serial console live" had one answer in this tree and it was a
//! literal copied from QEMU.
//!
//! Server-class aarch64 is where this matters: those machines are ACPI, they have no device tree,
//! and their console is not at `0x0900_0000`.
//!
//! Every read goes through a `phys_to_virt` the caller supplies, the way `fdt` does and for the
//! same reason: the loader reads these tables identity-mapped, and a host test reads a byte array
//! at its own address. Nothing here allocates and nothing dereferences an address it has not first
//! bounds-checked against the table length it came from.

use core::ptr::read_volatile;

// The UART families this system has a driver for. Deliberately the same two as `fdt::Uart` and
// deliberately a SEPARATE type: this one is what an SPCR interface byte decodes to, that one is
// what a `compatible` string decodes to, and a single shared enum would invite one decoder's
// notion of "unknown" to be read as the other's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Uart {
	// SPCR interface types 3 (ARM PL011), 0x0d (ARM SBSA, 32-bit only) and 0x0e (ARM SBSA generic).
	// The SBSA UART is register-compatible with the PL011 for the two registers used here.
	Pl011,
	// SPCR interface types 0 (full 16550), 1 (16550 subset) and 0x12 (16550 with DBG2 parameters).
	Ns16550,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Console {
	pub uart: Uart,
	pub base: u64,
	// Register spacing for the 16550 family, derived from the Generic Address Structure's access
	// size: a table describing 32-bit accesses is describing registers four bytes apart.
	pub reg_shift: u32,
}

// An ACPI view rooted at the RSDP the firmware published.
pub struct Acpi {
	rsdp: u64,
	p2v: fn(u64) -> u64,
}

impl Acpi {
	pub fn new(rsdp: u64, phys_to_virt: fn(u64) -> u64) -> Self {
		Self { rsdp, p2v: phys_to_virt }
	}

	fn u8_at(&self, pa: u64) -> u8 {
		unsafe { read_volatile((self.p2v)(pa) as *const u8) }
	}

	fn u32_at(&self, pa: u64) -> u32 {
		u32::from_le_bytes([self.u8_at(pa), self.u8_at(pa + 1), self.u8_at(pa + 2), self.u8_at(pa + 3)])
	}

	fn u64_at(&self, pa: u64) -> u64 {
		(self.u32_at(pa + 4) as u64) << 32 | self.u32_at(pa) as u64
	}

	fn signature_is(&self, pa: u64, want: &[u8]) -> bool {
		want.iter().enumerate().all(|(index, byte)| self.u8_at(pa + index as u64) == *byte)
	}

	// Is this a plausible RSDP? Signature and the first checksum, which is what tells a real RSDP
	// from a configuration-table entry pointing at something else entirely.
	pub fn is_valid(&self) -> bool {
		if self.rsdp == 0 || !self.signature_is(self.rsdp, b"RSD PTR ") {
			return false;
		}
		// The ACPI 1.0 part is the first twenty bytes and sums to zero. A table that fails this is
		// not an RSDP, and walking the pointers inside it would follow arbitrary numbers.
		let sum = (0..20u64).fold(0u8, |sum, index| sum.wrapping_add(self.u8_at(self.rsdp + index)));
		sum == 0
	}

	// The physical address of the table with this signature, or None.
	//
	// XSDT first (64-bit entries, ACPI 2.0 and later), RSDT as the fallback - which is the order
	// the specification requires of an OS that understands both, because a firmware providing both
	// may describe MORE tables in the XSDT.
	pub fn table(&self, signature: &[u8; 4]) -> Option<u64> {
		if !self.is_valid() {
			return None;
		}
		let revision = self.u8_at(self.rsdp + 15);
		let (root, width) = if revision >= 2 {
			let xsdt = self.u64_at(self.rsdp + 24);
			if xsdt != 0 { (xsdt, 8u64) } else { (self.u32_at(self.rsdp + 16) as u64, 4) }
		} else {
			(self.u32_at(self.rsdp + 16) as u64, 4)
		};
		if root == 0 {
			return None;
		}
		let length = self.u32_at(root + 4) as u64;
		// A header is 36 bytes; a length below that, or one large enough to be a wild number, means
		// the pointer is not a table and its entries are not entries.
		if !(36..0x10_0000).contains(&length) {
			return None;
		}
		let mut at = root + 36;
		while at + width <= root + length {
			let entry = if width == 8 { self.u64_at(at) } else { self.u32_at(at) as u64 };
			at += width;
			if entry != 0 && self.signature_is(entry, signature) {
				return Some(entry);
			}
		}
		None
	}

	// The console SPCR describes, or None when there is no SPCR, it names a device this system
	// cannot drive, or it puts it somewhere that is not memory.
	//
	// None is a real answer: the loader prints nothing after `ExitBootServices` rather than storing
	// to an address nobody chose, which is the whole point of asking the machine.
	pub fn console(&self) -> Option<Console> {
		let spcr = self.table(b"SPCR")?;
		let length = self.u32_at(spcr + 4) as u64;
		// Interface type at 36, then three reserved bytes, then a twelve-byte Generic Address
		// Structure at 40. Revision 1 tables are 80 bytes; anything shorter than the GAS is not a
		// table this can read.
		if length < 52 {
			return None;
		}
		let uart = match self.u8_at(spcr + 36) {
			0x00 | 0x01 | 0x12 => Uart::Ns16550,
			0x03 | 0x0d | 0x0e => Uart::Pl011,
			// Every other interface type is a real UART this system has no driver for - 0x02 is an
			// Intel EHCI debug port, 0x05 is a NEC, 0x07 is a Renesas. Guessing at one of the two
			// drivers would write into registers that mean something else.
			_ => return None,
		};
		// The Generic Address Structure: space id, bit width, bit offset, access size, address.
		let space = self.u8_at(spcr + 40);
		let access = self.u8_at(spcr + 43);
		let base = self.u64_at(spcr + 44);
		// Space 0 is system memory. Space 1 is I/O ports, which exist on x86 and which this loader
		// has no post-ExitBootServices path for; anything else is a PCI or embedded-controller
		// address that is not a store away.
		if space != 0 || base == 0 {
			return None;
		}
		// Access size 1 = byte, 2 = word, 3 = dword, 4 = qword. A table describing dword accesses
		// is describing registers four bytes apart, which is `reg-shift = 2` in the device tree's
		// vocabulary. PL011 registers are at fixed offsets and take no shift.
		let reg_shift = match (uart, access) {
			(Uart::Pl011, _) => 0,
			(Uart::Ns16550, 3) => 2,
			(Uart::Ns16550, 2) => 1,
			(Uart::Ns16550, _) => 0,
		};
		Some(Console { uart, base, reg_shift })
	}
}
