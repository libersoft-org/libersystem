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

// How a PSCI call reaches its implementation on this platform - the same two answers `fdt` gives,
// from the other kind of firmware description. A separate type for the same reason `Uart` is: this
// one decodes a FADT flag and that one a device-tree string, and one shared enum would invite one
// decoder's notion of "unknown" to be read as the other's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PsciConduit {
	Smc,
	Hvc,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Console {
	pub uart: Uart,
	pub base: u64,
	// Register spacing for the 16550 family, derived from the Generic Address Structure's access
	// size: a table describing 32-bit accesses is describing registers four bytes apart.
	pub reg_shift: u32,
	// ACCESS WIDTH IN BYTES, kept as well as the spacing it implies.
	//
	// The access-size byte was converted to `reg_shift` and then thrown away, so the console scaled
	// its ADDRESSES by the declared width and still performed byte reads and writes. A UART whose
	// SPCR says dword access frequently does not answer a byte access at all - the spacing and the
	// access size are two facts and only one of them was kept.
	pub access_width: u32,
}

// An ACPI view rooted at the RSDP the firmware published.
pub struct Acpi {
	rsdp: u64,
	p2v: fn(u64) -> u64,
}

impl Acpi {
	// # Safety
	// `rsdp` must be the physical address the firmware published for a real RSDP, and `phys_to_virt`
	// must map every physical address reachable from it - the RSDT/XSDT and every table they name -
	// to a readable virtual address. EVERY OTHER METHOD ON THIS TYPE DEREFERENCES what this
	// constructor was handed: `u8_at` is a `read_volatile` through `p2v`, so a wrong number here is
	// not a wrong answer later, it is a read of arbitrary memory. That contract was carried by the
	// callers knowing what they were doing and by nothing in the signature (UEFI-005).
	pub unsafe fn new(rsdp: u64, phys_to_virt: fn(u64) -> u64) -> Self {
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

	// Is this a plausible RSDP? Signature and BOTH checksums.
	//
	// The ACPI 1.0 part is the first twenty bytes and sums to zero, and that is what tells a real
	// RSDP from a configuration-table entry pointing at something else entirely. It is not enough:
	// from revision 2 the structure carries `Length` and a second checksum over the WHOLE of it, and
	// the XSDT pointer lives in the part the first checksum does not cover. So the pointer this
	// parser walks first was taken from bytes nothing had summed - and the console address it walks
	// to is the only thing printed after `ExitBootServices`, which this code then writes to. A table
	// that passes a signature check and fails its checksum is precisely the "firmware that is not
	// QEMU" case this milestone exists for.
	pub fn is_valid(&self) -> bool {
		if self.rsdp == 0 || !self.signature_is(self.rsdp, b"RSD PTR ") {
			return false;
		}
		if !self.sums_to_zero(self.rsdp, 20) {
			return false;
		}
		let revision = self.u8_at(self.rsdp + 15);
		if revision < 2 {
			return true;
		}
		let length = self.u32_at(self.rsdp + 20) as u64;
		// The extended structure is 36 bytes; anything shorter than the fields it is about to be
		// read for, or long enough to be a wild number, is not one.
		if !(36..0x1000).contains(&length) {
			return false;
		}
		self.sums_to_zero(self.rsdp, length)
	}

	// Do `length` bytes at `pa` sum to zero? Every ACPI structure carries a checksum with exactly
	// this rule, so there is one implementation of it.
	fn sums_to_zero(&self, pa: u64, length: u64) -> bool {
		let mut sum = 0u8;
		let mut index = 0u64;
		while index < length {
			sum = sum.wrapping_add(self.u8_at(pa + index));
			index += 1;
		}
		sum == 0
	}

	// Is the system description table at `pa` one this parser may read? Its signature, a sane
	// declared length, and its checksum over that whole length.
	//
	// One helper because the rule is one rule: the root table, SPCR and the FADT all carry a
	// 36-byte header whose bytes 0..4 are the signature, 4..8 the length and 9 the checksum. None of
	// the three was checked.
	fn table_is_valid(&self, pa: u64, signature: Option<&[u8; 4]>) -> bool {
		if pa == 0 {
			return false;
		}
		if let Some(want) = signature
			&& !self.signature_is(pa, want)
		{
			return false;
		}
		let length = self.u32_at(pa + 4) as u64;
		if !(36..0x10_0000).contains(&length) {
			return false;
		}
		self.sums_to_zero(pa, length)
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
		// THE ROOT TABLE'S OWN SIGNATURE AND CHECKSUM. Neither was checked: a length below 36 or
		// absurdly large was refused and everything else was walked, so entries were read out of a
		// structure that had only been assumed to be a table.
		let want: &[u8; 4] = if width == 8 { b"XSDT" } else { b"RSDT" };
		if !self.table_is_valid(root, Some(want)) {
			return None;
		}
		let length = self.u32_at(root + 4) as u64;
		let mut at = root + 36;
		while at + width <= root + length {
			let entry = if width == 8 { self.u64_at(at) } else { self.u32_at(at) as u64 };
			at += width;
			// And the table the entry names, before its contents are believed.
			if entry != 0 && self.signature_is(entry, signature) && self.table_is_valid(entry, Some(signature)) {
				return Some(entry);
			}
		}
		None
	}

	// Which instruction reaches PSCI on this machine, as the FADT states it - the ACPI half of the
	// same question `fdt::psci_conduit` answers from a device tree.
	//
	// The FADT's ARM Boot Architecture Flags are a u16 at offset 129: bit 0 `PSCI_COMPLIANT` says
	// whether PSCI exists at all, bit 1 `PSCI_USE_HVC` says which of the two instructions reaches it.
	// A firmware that is not PSCI-compliant gets `None`, which the caller must treat as "no PSCI"
	// rather than as a reason to guess.
	//
	// The flags field arrived in FADT revision 5, so a shorter table has no answer either.
	pub fn psci_conduit(&self) -> Option<PsciConduit> {
		let fadt = self.table(b"FACP")?;
		let length = self.u32_at(fadt + 4) as u64;
		if length < 131 {
			return None;
		}
		let flags = self.u8_at(fadt + 129) as u16 | (self.u8_at(fadt + 130) as u16) << 8;
		if flags & 0x0001 == 0 {
			return None;
		}
		Some(if flags & 0x0002 != 0 { PsciConduit::Hvc } else { PsciConduit::Smc })
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
		// Access size 4 is QWORD and fell into the default zero-shift branch, so a table declaring
		// eight-byte spacing got registers one byte apart - the one value in the enumeration that
		// was silently mistranslated rather than merely unused.
		let reg_shift = match (uart, access) {
			(Uart::Pl011, _) => 0,
			(Uart::Ns16550, 4) => 3,
			(Uart::Ns16550, 3) => 2,
			(Uart::Ns16550, 2) => 1,
			(Uart::Ns16550, _) => 0,
		};
		let access_width = match (uart, access) {
			(Uart::Pl011, _) => 4,
			(Uart::Ns16550, 4) => 8,
			(Uart::Ns16550, 3) => 4,
			(Uart::Ns16550, 2) => 2,
			// Access size 1 is byte, and 0 means the table did not say - byte is the safe reading of
			// both, and the one every 16550 answers.
			(Uart::Ns16550, _) => 1,
		};
		Some(Console { uart, base, reg_shift, access_width })
	}
}
