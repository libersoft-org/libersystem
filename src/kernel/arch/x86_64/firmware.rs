// WHAT THE ACPI SERVICE REACHES ON x86_64, through the kernel: the tables - the DSDT and the FACS through the FADT's
// pointers, every other by signature and instance - the SMI command port, the CMOS NVRAM, the PM timer and the global
// lock's release bit. Every one of these is the kernel's to perform: the ports are in the reserved set, and a table is
// read out of the direct map, checked, and copied.

use super::port;
use abi::{ERR_ACCESS_DENIED, ERR_INVALID, ERR_UNSUPPORTED};

pub fn fadt() -> Option<acpi::Fadt<'static>> {
	let bytes = crate::smp::acpi_table(crate::boot_info().rsdp, b"FACP")?;
	acpi::Fadt::new(bytes).ok()
}

// Whether this machine describes itself with ACPI at all.
pub fn available() -> bool {
	fadt().is_some()
}

// A table out of the direct map at `phys`: its header's length read, the whole checked against the direct map, and
// a checksum required where the table has one (every table but the FACS).
fn table_at(phys: u64, checksummed: bool) -> Option<&'static [u8]> {
	if phys == 0 || !crate::mem::within_direct_map(phys, 36) {
		return None;
	}
	let base = crate::mem::hhdm_offset() + phys;
	// SAFETY: `phys..phys+36` is in the direct map, checked above.
	let length = unsafe { core::ptr::read_unaligned((base + 4) as *const u32) } as u64;
	if !(8..=acpi::MAX_TABLE_LEN as u64).contains(&length) || !crate::mem::within_direct_map(phys, length) {
		return None;
	}
	// SAFETY: the whole table is in the direct map, checked above; firmware tables are never written after the boot.
	let bytes = unsafe { core::slice::from_raw_parts(base as *const u8, length as usize) };
	if checksummed && bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) != 0 {
		return None;
	}
	Some(bytes)
}

// THE `instance`-TH TABLE OF `signature`: the DSDT and the FACS through the FADT, every other through the root table.
pub fn table(signature: &[u8; 4], instance: usize) -> Option<&'static [u8]> {
	match signature {
		b"DSDT" if instance == 0 => table_at(fadt()?.dsdt()?, true),
		b"FACS" if instance == 0 => table_at(fadt()?.facs()?, false),
		b"DSDT" | b"FACS" => None,
		_ => crate::smp::acpi_table_instance(crate::boot_info().rsdp, signature, instance),
	}
}

// THE SMI COMMAND PORT, written - any value but the FADT's ACPI-disable value, which would take the machine out of the
// mode every event path depends on.
pub fn smi_command(value: u8) -> i64 {
	let Some(fadt) = fadt() else { return ERR_UNSUPPORTED };
	let (Some(port), Some((_, disable))) = (fadt.smi_command(), fadt.acpi_mode_values()) else { return ERR_UNSUPPORTED };
	if value == disable {
		return ERR_ACCESS_DENIED;
	}
	let Ok(port) = u16::try_from(port) else { return ERR_INVALID };
	// SAFETY: the FADT's SMI command port, one byte written, as the firmware defines it.
	unsafe { port::outb(port, value) };
	0
}

// The PM timer's count, or `None` on a machine with none.
pub fn pm_timer() -> Option<u32> {
	let timer = fadt()?.timer()?;
	let port = timer.block.io_port()?;
	// SAFETY: the FADT's PM timer register, read.
	let value = unsafe { port::inl(port) };
	Some(if timer.counting_bits == 24 { value & 0x00FF_FFFF } else { value })
}

// PM1 control's GBL_RLS (bit 2) set - the firmware is told the lock it asked for is free - with SLP_EN kept clear
// and every other bit as it was.
pub fn global_lock_release() -> i64 {
	const GBL_RLS: u16 = 1 << 2;
	const SLP_EN: u16 = 1 << 13;
	let Some(fadt) = fadt() else { return ERR_UNSUPPORTED };
	for block in [fadt.pm1a_control(), fadt.pm1b_control()].into_iter().flatten() {
		let Some(control) = block.io_port() else { continue };
		// SAFETY: the FADT's PM1 control register, read and written back with GBL_RLS set and SLP_EN clear.
		unsafe {
			let current = port::inw(control);
			port::outw(control, (current | GBL_RLS) & !SLP_EN);
		}
	}
	0
}

// CMOS NVRAM: every byte from 0x0E up but the FADT's century index - the clock's registers stay the kernel's.
pub fn nvram_allowed(index: u64) -> Option<u8> {
	let index = u8::try_from(index).ok().filter(|index| (0x0E..0x80).contains(index))?;
	if fadt().and_then(|fadt| fadt.century_index()) == Some(index) {
		return None;
	}
	Some(index)
}

pub fn nvram_read(index: u64) -> i64 {
	match nvram_allowed(index) {
		Some(index) => super::rtc::nvram_read(index) as i64,
		None => ERR_ACCESS_DENIED,
	}
}

pub fn nvram_write(index: u64, value: u64) -> i64 {
	let (Some(index), Ok(value)) = (nvram_allowed(index), u8::try_from(value)) else { return ERR_ACCESS_DENIED };
	super::rtc::nvram_write(index, value);
	0
}

// THE GENERAL-PURPOSE EVENTS, which the SCI handler keeps - see `sci`. A test build has no SCI (it would reprogram an
// interrupt controller under the suite), so there they are unsupported, as on a device-tree machine; the state machine
// is host-tested in `acpi::gpe`.
#[cfg(not(test))]
pub fn gpe_request(operation: u64, gpe: u64) -> i64 {
	super::sci::gpe_request(operation, gpe)
}

#[cfg(test)]
pub fn gpe_request(_operation: u64, _gpe: u64) -> i64 {
	ERR_UNSUPPORTED
}

pub fn gpe_instance_ended() {
	#[cfg(not(test))]
	super::sci::gpe_instance_ended();
}

#[cfg(not(test))]
pub fn take_events(out: &mut dyn FnMut(u8, u16)) {
	super::sci::take_events(out);
}

// THE WIRED LINES THE KERNEL USES, which no namespace device's `_CRS` may name: the timer's (IRQ 0 and the GSI 2 an
// override routes it to) and the SCI's. A kernel-held console's line is the kernel-held row's own.
pub fn kernel_lines() -> ([u32; 3], usize) {
	#[allow(unused_mut)]
	let (mut lines, mut count) = ([0u32, 2, 0], 2);
	#[cfg(not(test))]
	if let Some(sci) = super::sci::sci_line() {
		lines[2] = sci;
		count = 3;
	}
	(lines, count)
}

// THE CONFIGURATION REGISTERS THE KERNEL'S CHIPSET ROWS OWN, by (vendor, device): q35's MCH PCIEXBAR - the ECAM base -
// and the ICH9 LPC bridge's PMBASE and ACPI_CNTL - the PM block, which holds the GPE0 block, and its decode enable.
// Firmware code may read them and never write them.
const CHIPSET_REGISTERS: &[(u16, u16, &[(u16, u16)])] = &[(0x8086, 0x29c0, &[(0x60, 8)]), (0x8086, 0x2918, &[(0x40, 8)])];

pub fn chipset_registers(vendor: u16, device: u16) -> &'static [(u16, u16)] {
	CHIPSET_REGISTERS.iter().find(|row| row.0 == vendor && row.1 == device).map_or(&[], |row| row.2)
}
