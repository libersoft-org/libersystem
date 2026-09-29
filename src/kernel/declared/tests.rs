// THE DECLARED-REGISTER SUITE: a claim's registers reached one at a time at exactly their width, a write changing only
// its mask's bits, and the capability gone with the claim - on the test machine's `i6300esb` on every port, and on
// x86_64 q35's ICH9 LPC bridge, whose row mints exactly the TCO block and declares GCS.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::{device, sched, syscall};

// The first row of a PCI vendor and device.
fn row_of(vendor: u16, product: u16) -> Option<usize> {
	(0..device::count()).find(|&index| device::with(index, |row| (row.vendor, row.product) == (vendor, product)).unwrap_or(false))
}

// RUN `body` ON A THREAD OF ITS OWN, on this core, to its end: the syscall paths need a caller with a handle table.
fn in_thread(body: extern "C" fn(u64), done: &'static AtomicBool) {
	done.store(false, Ordering::SeqCst);
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(done.load(Ordering::SeqCst), "the test's thread ran to its end");
}

fn invoke(number: u64, a0: u64, a1: u64, a2: u64) -> i64 {
	unsafe { crate::arch::syscall::invoke(number, a0, a1, a2, 0) as i64 }
}

fn read(handle: i64, which: u64) -> i64 {
	invoke(abi::SYS_DEVICE_REGISTER_READ, handle as u64, which, 0)
}

fn write(handle: i64, which: u64, value: u64) -> i64 {
	invoke(abi::SYS_DEVICE_REGISTER_WRITE, handle as u64, which, value)
}

crate::tagged_test!(an_i6300esb_s_arming_registers_are_reached_at_their_widths_and_gone_after_release, [Object, Kernel, Pci, Syscall], id = "kernel.declared.an_i6300esb_s_arming_registers_are_reached_at_their_widths_and_gone_after_release", covers = ["kernel"]);
fn an_i6300esb_s_arming_registers_are_reached_at_their_widths_and_gone_after_release() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_: u64) {
		through_the_i6300esb();
		DONE.store(true, Ordering::SeqCst);
	}
	in_thread(body, &DONE);
}

// 0x60 ANSWERS ONLY AS A WORD AND 0x68 ONLY AS A BYTE - a dword access to either falls through and does nothing - so
// what is proved here is that each is reached at its own width: a value written reads back.
fn through_the_i6300esb() {
	let row = row_of(0x8086, 0x25ab).expect("the test machine carries an i6300esb");
	assert_eq!(super::count(row), 2, "the scan declared 0x60 and 0x68");
	let (bus, dev, func) = device::with(row, |row| (row.bus, row.dev, row.func)).expect("the row");
	// THE WIDTHS THE MECHANISM REFUSES: one that is not a byte, a word or a dword, and one not aligned to itself.
	assert_eq!(crate::arch::pci::config_read_exact(bus, dev, func, 0x60, 3), None, "a width of three is refused");
	assert_eq!(crate::arch::pci::config_read_exact(bus, dev, func, 0x61, 2), None, "a word at an odd offset is refused");
	assert_eq!(crate::arch::pci::config_write_exact(bus, dev, func, 0x69, 4, 0), false, "a dword not aligned to four is refused");
	let grant = crate::tests::claim_device(row as u64).expect("the i6300esb is claimed under the entry that declares it");
	let handle = invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_REGISTERS, 0);
	assert!(handle > 0, "the claim mints its declared registers ({handle})");
	assert_eq!(invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_REGISTERS, 1), syscall::ERR_INVALID, "one set of registers per claim");
	// 0x60 AS A WORD: reboot-disabled (bit 5) set and read back, then the value it held written back. (The
	// interrupt-type bits are not the probe: QEMU keeps them under a mask of its own, 0x11.)
	let config = read(handle, 0);
	assert!(config >= 0, "0x60 reads ({config})");
	assert_eq!(Some(config as u32), crate::arch::pci::config_read_exact(bus, dev, func, 0x60, 2), "at 16 bits, as the configuration space holds it");
	assert_eq!(write(handle, 0, 0x0020), 0);
	assert_eq!(read(handle, 0), 0x0020, "a word written to 0x60 reads back");
	assert_eq!(write(handle, 0, config as u64), 0);
	assert_eq!(read(handle, 0), config, "and the value it held is back");
	// 0x68 AS A BYTE: free-run (bit 2) set and cleared - which neither starts nor locks the timer.
	assert_eq!(read(handle, 1), 0, "the timer is neither enabled nor locked at boot");
	assert_eq!(write(handle, 1, 0x04), 0);
	assert_eq!(read(handle, 1), 0x04, "a byte written to 0x68 reads back");
	assert_eq!(write(handle, 1, 0), 0);
	assert_eq!(read(handle, 1), 0);
	// AN UNDECLARED INDEX, and a value wider than any register: refused.
	assert_eq!(read(handle, 2), syscall::ERR_INVALID, "an index past the declaration");
	assert_eq!(write(handle, 2, 0), syscall::ERR_INVALID);
	assert_eq!(write(handle, 0, 1 << 32), syscall::ERR_INVALID, "a value past 32 bits");
	// THE RELEASE REVOKES IT, and writes nothing: the handle reaches nothing, and 0x68 holds what it held.
	crate::tests::release_device(&grant);
	assert!(read(handle, 0) < 0, "the registers went with the claim");
	assert!(write(handle, 1, 0x02) < 0, "and nothing is written through them");
	assert_eq!(crate::arch::pci::config_read_exact(bus, dev, func, 0x68, 1), Some(0), "the timer was never enabled");
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(the_ich9_lpc_row_mints_exactly_the_tco_block_and_declares_gcs, [Object, Kernel, Pci, Syscall, ArchX86_64], id = "kernel.declared.the_ich9_lpc_row_mints_exactly_the_tco_block_and_declares_gcs", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn the_ich9_lpc_row_mints_exactly_the_tco_block_and_declares_gcs() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_: u64) {
		through_the_lpc_bridge();
		DONE.store(true, Ordering::SeqCst);
	}
	in_thread(body, &DONE);
}

#[cfg(target_arch = "x86_64")]
fn through_the_lpc_bridge() {
	use crate::object::ObjectType;
	use crate::object::handle::Handle;
	use crate::object::port_range::PortRange;
	use crate::object::rights::Rights;
	let row = row_of(0x8086, 0x2918).expect("q35 carries an ICH9 LPC bridge");
	let (bus, dev, func) = device::with(row, |row| (row.bus, row.dev, row.func)).expect("the row");
	let pm_base = (crate::arch::pci::config_read32(bus, dev, func, 0x40) & 0xFF80) as u16;
	assert!(pm_base != 0, "the PM block has a base");
	let command = crate::arch::pci::config_read32(bus, dev, func, 0x04) & 0xFFFF;
	let grant = crate::tests::claim_device(row as u64).expect("the LPC bridge is claimed under the TCO driver's entry");
	// ITS PORTS: EXACTLY THE TCO BLOCK - one range, PM base + 0x60, 32 ports - and nothing else of the PM block.
	let tco = invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_PORT_RANGE, 0);
	assert!(tco > 0, "the claim mints the TCO block ({tco})");
	let range = {
		let thread = sched::current_thread().expect("a current thread");
		let object = thread.handles().lock().lookup_typed(Handle::from_raw(tco as u64), ObjectType::PortRange, Rights::MAP).expect("a port-range handle");
		object.into_any_arc().downcast::<PortRange>().ok().expect("a PortRange")
	};
	assert_eq!(range.span(), (pm_base + 0x60, 32), "the TCO block, and only it");
	drop(range);
	assert_eq!(invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_PORT_RANGE, 1), syscall::ERR_INVALID, "and no second range");
	// PM1's event and control blocks, the PM timer and GPE0 stay the kernel's: no mint reaches them.
	for (offset, len, what) in [(0x00u16, 4u16, "PM1's event block"), (0x04, 2, "PM1's control"), (0x08, 4, "the PM timer"), (0x20, 16, "GPE0")] {
		assert!(PortRange::mint(pm_base + offset, len, None).is_err(), "{what} at {:#x} is refused to every mint", pm_base + offset);
	}
	// GCS, DECLARED WITH A MASK OF NO-REBOOT ALONE: a write of every bit changes that one and no other.
	let registers = invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_REGISTERS, 0);
	assert!(registers > 0, "the claim mints GCS ({registers})");
	let gcs = read(registers, 0);
	assert!(gcs >= 0, "GCS reads ({gcs})");
	let gcs = gcs as u32;
	assert_eq!(write(registers, 0, (!gcs) as u64), 0);
	assert_eq!(read(registers, 0) as u32, gcs ^ (1 << 5), "only No-Reboot changed");
	assert_eq!(write(registers, 0, gcs as u64), 0);
	assert_eq!(read(registers, 0) as u32, gcs, "and it is back");
	assert_eq!(read(registers, 1), syscall::ERR_INVALID, "GCS is the one register");
	// THE CLAIM CHANGED NO DECODE BIT of the function, whose decode the kernel's own PM1 and reset paths use.
	assert_eq!(crate::arch::pci::config_read32(bus, dev, func, 0x04) & 0x3, command & 0x3, "the I/O and memory decode are as they were");
	crate::tests::release_device(&grant);
	assert!(read(registers, 0) < 0, "GCS went with the claim");
	assert_eq!(crate::arch::pci::config_read32(bus, dev, func, 0x04) & 0x3, command & 0x3, "and the release changed no decode bit either");
}

// THE ICH9 SMBUS FUNCTION'S HOSTC, WRITTEN BY THE CLAIM AND RESTORED BY THE RELEASE: seeded here with HST_EN clear and
// I2C_EN set, so both bits have to move - and come back - for this to pass. With the host enabled the SMBus base, BAR
// 4, decodes: its host status register reads as a register rather than as nothing.
crate::tagged_test!(the_ich9_smbus_claim_enables_its_host_and_the_release_restores_hostc, [Object, Kernel, Pci, Syscall, ArchX86_64], id = "kernel.declared.the_ich9_smbus_claim_enables_its_host_and_the_release_restores_hostc", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn the_ich9_smbus_claim_enables_its_host_and_the_release_restores_hostc() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_: u64) {
		through_the_smbus_host();
		DONE.store(true, Ordering::SeqCst);
	}
	in_thread(body, &DONE);
}

#[cfg(target_arch = "x86_64")]
fn through_the_smbus_host() {
	use super::{HOSTC, HOSTC_HST_EN, HOSTC_I2C_EN};
	use crate::object::ObjectType;
	use crate::object::handle::Handle;
	use crate::object::port_range::PortRange;
	use crate::object::rights::Rights;
	let row = row_of(0x8086, 0x2930).expect("q35 carries an ICH9 SMBus function");
	let (bus, dev, func) = device::with(row, |row| (row.bus, row.dev, row.func)).expect("the row");
	let found = crate::arch::pci::config_read_exact(bus, dev, func, HOSTC, 1).expect("HOSTC reads");
	let seeded = (found & !HOSTC_HST_EN) | HOSTC_I2C_EN;
	assert!(crate::arch::pci::config_write_exact(bus, dev, func, HOSTC, 1, seeded), "HOSTC is seeded");
	let grant = crate::tests::claim_device(row as u64).expect("the SMBus function is claimed under the smbus_ich9 entry");
	let claimed = crate::arch::pci::config_read_exact(bus, dev, func, HOSTC, 1).expect("HOSTC reads");
	assert_eq!(claimed & (HOSTC_HST_EN | HOSTC_I2C_EN), HOSTC_HST_EN, "the claim set HST_EN and cleared I2C_EN");
	// THE BASE, minted from the claim, and decoding.
	let base = invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_PORT_RANGE, 0);
	assert!(base > 0, "the claim mints the SMBus base ({base})");
	let (first, len) = {
		let thread = sched::current_thread().expect("a current thread");
		let object = thread.handles().lock().lookup_typed(Handle::from_raw(base as u64), ObjectType::PortRange, Rights::MAP).expect("a port-range handle");
		object.into_any_arc().downcast::<PortRange>().ok().expect("a PortRange").span()
	};
	assert!(len >= 16, "BAR 4's span ({first:#x}, {len})");
	// SAFETY: the claimed function's own SMBus base, which the kernel may read like any port.
	let aux_ctl = unsafe { crate::arch::port::inb(first + 0x0D) };
	assert_ne!(aux_ctl, 0xFF, "the host's auxiliary control reads as a register - the base decodes");
	crate::tests::release_device(&grant);
	assert_eq!(crate::arch::pci::config_read_exact(bus, dev, func, HOSTC, 1), Some(seeded), "the release wrote back what the claim found");
	assert!(crate::arch::pci::config_write_exact(bus, dev, func, HOSTC, 1, found), "and HOSTC is left as the boot had it");
}

// THE CHIPSET BASE GCS IS FOUND AT, against fixture values of the LPC bridge's RCBA register (0xF0): enabled, it is
// the base; disabled - bit 0 clear - or zero, GCS is not declared at all.
crate::tagged_test!(a_chipset_register_is_declared_only_at_an_enabled_base, [Kernel, Pci], id = "kernel.declared.a_chipset_register_is_declared_only_at_an_enabled_base", covers = ["kernel"]);
fn a_chipset_register_is_declared_only_at_an_enabled_base() {
	assert_eq!(super::chipset_base(0xFED1_C001, 0xFFFF_C000, 1), Some(0xFED1_C000), "q35's root complex, enabled");
	assert_eq!(super::chipset_base(0xFED1_C000, 0xFFFF_C000, 1), None, "the root-complex base disabled");
	assert_eq!(super::chipset_base(0x0000_0001, 0xFFFF_C000, 1), None, "enabled at zero");
	// THE LPC ROW NAMES THE TABLE THAT SUPPRESSES IT: with a WDAT present its claim is refused and nothing is declared.
	let lpc = super::row_for(0x8086, 0x2918).expect("the ICH9 LPC row");
	assert_eq!(lpc.suppressed_by, Some(*b"WDAT"));
	assert!(super::row_for(0x8086, 0x25ab).is_some_and(|row| row.suppressed_by.is_none() && row.bar == Some(0)), "the i6300esb's row resolves BAR 0 and nothing suppresses it");
}
