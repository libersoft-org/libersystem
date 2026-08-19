// The loader's diagnostic output: the firmware's console while it exists, and the machine's own
// afterwards.
//
// Every architecture backend used to carry a UART driver at a FIXED PHYSICAL ADDRESS - PL011 at
// 0x0900_0000 on aarch64, a 16550 at 0x1000_0000 on riscv64 - and `efi_main` started printing almost
// immediately. Those are QEMU's addresses. UEFI promises an identity map for RAM; it promises
// nothing about a device region at an address the loader made up, so on a machine that is not
// `virt` the first diagnostic line was a store to whatever happens to live there. That is the wrong
// way round twice over: while boot services exist, the firmware's own console is the console, and it
// works on every machine because the firmware wrote the driver for the machine it is running on -
// and once they are gone, the machine still says where its console is, in the device tree or in
// ACPI's SPCR table.
//
// So there are two halves here and NEITHER of them is a guess. Output goes to `ConOut` until
// `ExitBootServices`; after that it goes to the address the machine named, and if the machine named
// none, it goes nowhere. The fixed addresses are gone rather than demoted to a fallback: a fallback
// is the same store into somebody else's device region, kept on the grounds that it usually is not.

use core::ptr;

use uefi::{self, SimpleTextOutput};

// The firmware's console, or null before it is known and after boot services end.
static mut CON_OUT: *mut SimpleTextOutput = ptr::null_mut();

// Remember the firmware console. Called from `efi_main` before anything is printed.
pub(crate) fn adopt(system_table: *mut uefi::SystemTable) {
	unsafe { CON_OUT = (*system_table).con_out };
}

// Give it up. After `ExitBootServices` the firmware's console is a pointer to memory the loader no
// longer owns, so calling it is worse than printing nothing.
pub(crate) fn release() {
	unsafe { CON_OUT = ptr::null_mut() };
}

// Write a string through the firmware console. False when there is none, which is the signal to try
// the post-ExitBootServices console below - which may itself answer false, and then nothing is
// printed.
//
// `OutputString` takes NUL-terminated UTF-16, so this fills a small buffer and flushes it - no
// allocation, because this runs before the heap exists and after it is gone. Newlines are expanded
// to CRLF for the same reason the UART path does it: a terminal that does not do it itself.
pub(crate) fn write_str(s: &str) -> bool {
	let con_out = unsafe { CON_OUT };
	if con_out.is_null() {
		return false;
	}
	let mut buf = [0u16; 64];
	let mut n = 0;
	for unit in s.encode_utf16() {
		if unit == u16::from(b'\n') {
			buf[n] = u16::from(b'\r');
			n += 1;
		}
		buf[n] = unit;
		n += 1;
		// Two units can be added per pass, and one slot is the terminator.
		if n + 2 >= buf.len() {
			buf[n] = 0;
			unsafe { ((*con_out).output_string)(con_out, buf.as_ptr()) };
			n = 0;
		}
	}
	buf[n] = 0;
	unsafe { ((*con_out).output_string)(con_out, buf.as_ptr()) };
	true
}

// One byte, for the callers that build their output a character at a time.
pub(crate) fn write_byte(byte: u8) -> bool {
	let mut one = [0u8; 1];
	one[0] = byte;
	match core::str::from_utf8(&one) {
		Ok(s) => write_str(s),
		// Not a character on its own; the raw byte path takes it.
		Err(_) => false,
	}
}

// ---------------------------------------------------------------------------------------------
// After ExitBootServices: the console the MACHINE named, or nothing at all.
// ---------------------------------------------------------------------------------------------

// Once boot services end the firmware console is gone and the loader still has a handful of lines
// to print - the staging copy, the handover. Those lines used to go to a fixed address per target,
// `0x0900_0000` on aarch64 and `0x1000_0000` on riscv64, which are QEMU's `virt` addresses. On a
// machine that is not `virt` that is a store into whatever lives there.
//
// So the address is DISCOVERED, before boot services end, from the machine's own description of
// itself: `/chosen/stdout-path` in the device tree, or the ACPI SPCR table on a machine that has no
// device tree (which is every server-class aarch64 board, and QEMU's aarch64 virt under AAVMF -
// where the loader's own log says "no device-tree table"). Both are read by crates a host can test
// against real tables.
//
// AND WHEN NEITHER ANSWERS, NOTHING IS PRINTED. That is the whole change: a machine this loader
// cannot identify gets silence for six diagnostic lines instead of a store to an address somebody
// wrote down while looking at an emulator.
#[derive(Clone, Copy)]
pub(crate) struct PostEbs {
	pl011: bool,
	base: u64,
	reg_shift: u32,
	// Access width in bytes for the 16550 family: a UART whose table declares dword access often
	// does not answer a byte access. The device-tree path has no equivalent field and uses 1.
	access_width: u32,
}

static mut POST_EBS: Option<PostEbs> = None;

// The two firmware descriptions of the machine, as the configuration table published them.
//
// Kept because the console is not the only question they answer: `build_boot_info` asks them which
// instruction reaches PSCI, which it used to infer from its own exception level. Read once here,
// while the configuration table is still there.
static mut FIRMWARE_TABLES: (u64, u64) = (0, 0);

// Physical addresses are reachable as themselves here: UEFI identity-maps memory for the loader,
// and this runs before anything changes that.
fn identity(address: u64) -> u64 {
	address
}

// Ask the machine where its console is. Called from `efi_main`, while the configuration table is
// still there to be read.
pub(crate) fn discover(system_table: *mut uefi::SystemTable) {
	let (mut dtb, mut rsdp) = (0u64, 0u64);
	unsafe {
		// A NULL POINTER IS NOT AN EMPTY SLICE. `from_raw_parts` requires a non-null, aligned pointer
		// EVEN FOR LENGTH ZERO, and firmware with no configuration tables is entitled to publish
		// null with a count of zero - so the ordinary case was undefined behaviour that happened to
		// work. There is nothing to search either way; say so instead of forming the slice.
		let table = (*system_table).configuration_table;
		let count = (*system_table).number_of_table_entries;
		let entries: &[uefi::ConfigurationTable] = if table.is_null() || count == 0 { &[] } else { core::slice::from_raw_parts(table, count) };
		for entry in entries {
			if entry.vendor_guid == uefi::DTB_TABLE_GUID {
				dtb = entry.vendor_table as u64;
			}
			// ACPI 2.0 is preferred and 1.0 is the fallback, which is the same order `find_rsdp`
			// uses on x86_64 - a machine publishing both publishes the same RSDP twice.
			if entry.vendor_guid == uefi::ACPI_20_TABLE_GUID {
				rsdp = entry.vendor_table as u64;
			} else if rsdp == 0 && entry.vendor_guid == uefi::ACPI_10_TABLE_GUID {
				rsdp = entry.vendor_table as u64;
			}
		}
	}
	let from_tree = (dtb != 0).then(|| unsafe { fdt::Fdt::new(dtb, identity) }.console()).flatten().map(|console| PostEbs { pl011: console.uart == fdt::Uart::Pl011, base: console.base, reg_shift: console.reg_shift, access_width: 1 });
	// The device tree first: a machine that has one is describing the hardware it actually has,
	// while SPCR describes what the firmware was using - which is the same thing on every machine
	// this has met, and the tree is the more specific of the two.
	let found = from_tree.or_else(|| (rsdp != 0).then(|| unsafe { uefi::acpi::Acpi::new(rsdp, identity) }.console()).flatten().map(|console| PostEbs { pl011: console.uart == uefi::acpi::Uart::Pl011, base: console.base, reg_shift: console.reg_shift, access_width: console.access_width }));
	unsafe {
		POST_EBS = found;
		FIRMWARE_TABLES = (dtb, rsdp);
	}
}

// The device tree and RSDP physical addresses the firmware published, or 0 for one it did not.
pub(crate) fn firmware_tables() -> (u64, u64) {
	unsafe { FIRMWARE_TABLES }
}

// Physical addresses are reachable as themselves in the loader; exposed so the backends can read
// the same tables through the same translation this module uses.
pub(crate) fn identity_map(address: u64) -> u64 {
	identity(address)
}

// What was discovered, for the backends' post-ExitBootServices output.
pub(crate) fn post_ebs() -> Option<PostEbs> {
	unsafe { POST_EBS }
}

// One line saying what the machine said, printed while the firmware console still works - so a
// boot on unfamiliar hardware says which console its later lines will go to, or that there will not
// be any.
pub(crate) fn report() {
	match post_ebs() {
		Some(console) => {
			write_str("loader: console at ");
			let mut digits = [0u8; 18];
			let mut at = digits.len();
			let mut value = console.base;
			loop {
				at -= 1;
				digits[at] = b"0123456789abcdef"[(value & 0xf) as usize];
				value >>= 4;
				if value == 0 {
					break;
				}
			}
			write_str("0x");
			write_str(core::str::from_utf8(&digits[at..]).unwrap_or("?"));
			write_str(if console.pl011 { " (pl011)\n" } else { " (16550)\n" });
		}
		None => {
			write_str("loader: this machine names no console it can drive; nothing will be printed after ExitBootServices\n");
		}
	}
}

// Transmit one byte to the discovered console. False when there is none, which is the signal to
// print nothing rather than to reach for a fallback address.
pub(crate) fn write_byte_post_ebs(byte: u8) -> bool {
	let Some(console) = post_ebs() else { return false };
	// THE WAIT IS BOUNDED, and that is not tidiness. These addresses now come from a table the
	// machine wrote, so an unfamiliar or wrong one is reachable in a way a hard-coded address was
	// not - and an unbounded "wait for the transmitter" loop against something that is not a UART
	// reads a status bit that never changes and hangs the boot at its very last step. A diagnostic
	// that can stop the machine is worse than no diagnostic. Sixteen million spins is far longer
	// than any real UART takes to drain and far shorter than a person waits.
	const PATIENCE: u32 = 1 << 24;
	unsafe {
		if console.pl011 {
			// PL011: data register at +0x00, flag register at +0x18, TXFF is bit 5.
			let mut left = PATIENCE;
			while core::ptr::read_volatile((console.base + 0x18) as *const u32) & (1 << 5) != 0 {
				left -= 1;
				if left == 0 {
					return true;
				}
				core::hint::spin_loop();
			}
			core::ptr::write_volatile(console.base as *mut u32, byte as u32);
		} else {
			// 16550: transmit holding register at +0, line status at +5, THRE is bit 5 - both
			// scaled by the register spacing the machine declared.
			let lsr = console.base + (5u64 << console.reg_shift);
			let mut left = PATIENCE;
			// AT THE DECLARED WIDTH. The spacing was honoured and the access size was not, so a UART
			// described as dword-access got byte reads at correctly-spaced addresses - and many such
			// parts do not answer those at all.
			while read_reg(lsr, console.access_width) & (1 << 5) == 0 {
				left -= 1;
				if left == 0 {
					return true;
				}
				core::hint::spin_loop();
			}
			write_reg(console.base, byte, console.access_width);
		}
	}
	true
}

// Read and write a 16550 register at the width its table declares. Anything unrecognised is a byte,
// which is what every 16550 answers.
#[inline]
unsafe fn read_reg(address: u64, width: u32) -> u8 {
	unsafe {
		match width {
			8 => core::ptr::read_volatile(address as *const u64) as u8,
			4 => core::ptr::read_volatile(address as *const u32) as u8,
			2 => core::ptr::read_volatile(address as *const u16) as u8,
			_ => core::ptr::read_volatile(address as *const u8),
		}
	}
}

#[inline]
unsafe fn write_reg(address: u64, byte: u8, width: u32) {
	unsafe {
		match width {
			8 => core::ptr::write_volatile(address as *mut u64, byte as u64),
			4 => core::ptr::write_volatile(address as *mut u32, byte as u32),
			2 => core::ptr::write_volatile(address as *mut u16, byte as u16),
			_ => core::ptr::write_volatile(address as *mut u8, byte),
		}
	}
}
