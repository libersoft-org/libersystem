// THE ACPI SERVICE'S KERNEL SIDE, driven through its syscalls from a thread with a handle table, as the service
// drives it: the privilege every call needs; the tables and the mediated accesses on x86_64; configuration writes
// held to the policy; SystemMemory regions mapped as the memory they are, a companion's BAR making its function
// firmware-held; a claim and another node's region refusing each other whichever comes first; a namespace row's ports
// minted from its claim and refused over an I/O BAR or a bridge's I/O window; a walk reconciled by
// identity across two instances, with "namespace loaded" withdrawing what the second did not report again; and an
// ended instance leaving no general-purpose event enabled.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use platform::report::{self, DeviceReport, Function, List};

use crate::object::KernelObject;
use crate::object::channel::Channel;
use crate::object::handle::Handle;
use crate::object::privilege::{Privilege, PrivilegeKind};
use crate::object::rights::Rights;
use crate::{device, sched, syscall};

fn invoke(number: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> i64 {
	unsafe { crate::arch::syscall::invoke(number, a0, a1, a2, a3) as i64 }
}

fn in_thread(body: extern "C" fn(u64), done: &'static AtomicBool) {
	done.store(false, Ordering::SeqCst);
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(done.load(Ordering::SeqCst), "the test's thread ran to its end");
}

fn privilege(kind: PrivilegeKind) -> u64 {
	let thread = sched::current_thread().expect("a current thread");
	let privilege = Privilege::create(kind).expect("a test privilege");
	thread.handles().lock().try_insert_object(privilege, Rights::ALL).expect("the privilege installs").raw()
}

fn install(object: Arc<dyn crate::object::KernelObject>) -> u64 {
	let thread = sched::current_thread().expect("a current thread");
	thread.handles().lock().try_insert_object(object, Rights::ALL).expect("the object installs").raw()
}

fn close(handle: i64) {
	let thread = sched::current_thread().expect("a current thread");
	thread.handles().lock().close(Handle::from_raw(handle as u64)).expect("the handle closes");
}

fn device_memory_of(handle: u64) -> Arc<crate::object::device_memory::DeviceMemory> {
	let thread = sched::current_thread().expect("a current thread");
	let object = thread.handles().lock().lookup_typed(Handle::from_raw(handle), crate::object::ObjectType::DeviceMemory, Rights::MAP).expect("a device-memory handle");
	object.into_any_arc().downcast::<crate::object::device_memory::DeviceMemory>().ok().expect("a DeviceMemory")
}

fn koid() -> u64 {
	sched::current_thread().expect("a current thread").process().header().koid()
}

// A free MMIO window of the test machine, outside RAM, every BAR and every kernel-held range - the platform-row
// suite's, one page past the ranges it uses.
fn free_window(offset: u64) -> u64 {
	#[cfg(target_arch = "x86_64")]
	let base = 0xfed7_0000;
	#[cfg(target_arch = "aarch64")]
	let base = 0x0d10_0000;
	#[cfg(target_arch = "riscv64")]
	let base = 0x0510_0000;
	base + offset
}

// THE TEST MACHINE'S i6300esb: a PCI function with a memory BAR that a suite entry declares, so a test can claim it -
// its row and its `SYS_FIRMWARE_PCI` address.
fn watchdog_function() -> (usize, u64) {
	let row = (0..device::count()).find(|&index| device::with(index, |entry| entry.platform.is_none() && (entry.vendor, entry.product) == (0x8086, 0x25ab)).unwrap_or(false)).expect("the test machine carries an i6300esb");
	let (bus, dev, func) = device::with(row, |entry| (entry.bus, entry.dev, entry.func)).expect("its row");
	(row, (bus as u64) << 16 | (dev as u64) << 8 | func as u64)
}

fn map_request(base: u64, len: u64, node: &[u8], companion: Option<Function>) -> abi::FirmwareMapRequest {
	let mut request = abi::FirmwareMapRequest { base, len, ..abi::FirmwareMapRequest::default() };
	request.node[..node.len()].copy_from_slice(node);
	request.node_len = node.len() as u32;
	if let Some(function) = companion {
		request.companion = (function.segment as u64) << 32 | (function.bus as u64) << 16 | (function.device as u64) << 8 | function.function as u64;
	}
	request
}

fn map(privilege: u64, request: &abi::FirmwareMapRequest) -> i64 {
	invoke(abi::SYS_FIRMWARE_MAP, privilege, request as *const _ as u64, 0, 0)
}

fn send_report(privilege: u64, bytes: &[u8]) -> i64 {
	invoke(abi::SYS_FIRMWARE_REPORT, privilege, bytes.as_ptr() as u64, bytes.len() as u64, 0)
}

fn device_report(privilege: u64, description: platform::Description) -> i64 {
	let mut out = [0u8; report::MAX_REPORT];
	let report = DeviceReport { description, targets: [(0, &[]); abi::MAX_PLATFORM_CONNECTIONS], target_count: 0, parent: None, properties: &[] };
	let len = report::encode_device(&report, &mut out).expect("the report encodes");
	send_report(privilege, &out[..len])
}

fn withdraw_report(privilege: u64, identity: &[u8]) -> i64 {
	let mut out = [0u8; 128];
	let len = report::encode_withdraw(identity, &mut out).expect("the withdrawal encodes");
	send_report(privilege, &out[..len])
}

fn loaded_report(privilege: u64, instance: u64) -> i64 {
	let mut out = [0u8; 16];
	let len = report::encode_loaded(instance, &mut out).expect("the report encodes");
	send_report(privilege, &out[..len])
}

fn namespace_description(identity: &[u8], state: u8, hid: &[u8]) -> platform::Description {
	let mut description = platform::Description::new(abi::PLATFORM_SOURCE_ACPI, state, identity).expect("a test identity fits");
	assert!(description.add_match(abi::MATCH_ID_HID, hid), "the id fits");
	description
}

// Attach the calling thread's process as a new instance, its events on a fresh channel; the instance's number.
fn attach(privilege: u64) -> (u64, Arc<Channel>) {
	let (ours, theirs) = Channel::create();
	let handle = install(theirs);
	let instance = invoke(abi::SYS_FIRMWARE_EVENTS, privilege, handle, 0, 0);
	assert!(instance > 0, "the instance attaches ({instance})");
	(instance as u64, ours)
}

// Every device event queued so far, as (kind, the eight bytes after it).
fn device_events(events: &Channel) -> Vec<(u8, u64)> {
	let mut out = Vec::new();
	while let Ok(message) = events.recv() {
		if message.bytes.len() == 9 {
			out.push((message.bytes[0], u64::from_le_bytes(message.bytes[1..9].try_into().expect("eight bytes"))));
		}
	}
	out
}

fn device_node(index: usize) -> abi::FirmwareNode {
	let mut node = abi::FirmwareNode::default();
	assert_eq!(invoke(abi::SYS_DEVICE_NODE, index as u64, &mut node as *mut _ as u64, core::mem::size_of::<abi::FirmwareNode>() as u64, 0), 0, "row {index} answers its node");
	node
}

crate::tagged_test!(every_firmware_call_needs_the_interpreter_privilege, [Kernel, Syscall], id = "kernel.firmware.every_firmware_call_needs_the_interpreter_privilege", covers = ["kernel"]);
fn every_firmware_call_needs_the_interpreter_privilege() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_: u64) {
		let other = privilege(PrivilegeKind::DeviceManager);
		let request = map_request(free_window(0), 0x1000, b"acpi:\\TST_", None);
		let calls: [(u64, u64, u64, u64); 7] = [
			(abi::SYS_FIRMWARE_TABLE, u32::from_le_bytes(*b"DSDT") as u64, 0, 0),
			(abi::SYS_FIRMWARE_MAP, &request as *const _ as u64, 0, 0),
			(abi::SYS_FIRMWARE_MEDIATED, abi::FIRMWARE_PM_TIMER, 0, 0),
			(abi::SYS_FIRMWARE_PCI, 0, 4 << 16, 0),
			(abi::SYS_FIRMWARE_REPORT, &[0u8; 9] as *const _ as u64, 9, 0),
			(abi::SYS_FIRMWARE_EVENTS, 0, 0, 0),
			(abi::SYS_FIRMWARE_GPE, abi::GPE_COUNT, 0, 0),
		];
		for (number, a1, a2, a3) in calls {
			assert_eq!(invoke(number, other, a1, a2, a3), syscall::ERR_ACCESS_DENIED, "call {number} with another kind of privilege");
			assert_eq!(invoke(number, 0, a1, a2, a3), syscall::ERR_BAD_HANDLE, "call {number} with no handle");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	in_thread(body, &DONE);
}

crate::tagged_test!(the_tables_and_the_mediated_accesses_are_performed_by_the_kernel, [Kernel, Syscall], id = "kernel.firmware.the_tables_and_the_mediated_accesses_are_performed_by_the_kernel", covers = ["kernel"]);
fn the_tables_and_the_mediated_accesses_are_performed_by_the_kernel() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_: u64) {
		let firmware = privilege(PrivilegeKind::FirmwareInterpreter);
		let selector = |signature: &[u8; 4], instance: u64| u32::from_le_bytes(*signature) as u64 | instance << 32;
		#[cfg(not(target_arch = "x86_64"))]
		{
			// A DEVICE-TREE MACHINE: no tables, no mediated accesses.
			assert!(!crate::arch::firmware::available());
			assert_eq!(invoke(abi::SYS_FIRMWARE_TABLE, firmware, selector(b"DSDT", 0), 0, 0), syscall::ERR_UNSUPPORTED);
			assert_eq!(invoke(abi::SYS_FIRMWARE_MEDIATED, firmware, abi::FIRMWARE_PM_TIMER, 0, 0), syscall::ERR_UNSUPPORTED);
		}
		#[cfg(target_arch = "x86_64")]
		the_x86_64_tables_and_accesses(firmware, selector);
		DONE.store(true, Ordering::SeqCst);
	}
	in_thread(body, &DONE);
}

#[cfg(target_arch = "x86_64")]
fn the_x86_64_tables_and_accesses(firmware: u64, selector: impl Fn(&[u8; 4], u64) -> u64) {
	{
		// THE DSDT THROUGH THE FADT: its size asked with no buffer, then the whole table, checksummed.
		let size = invoke(abi::SYS_FIRMWARE_TABLE, firmware, selector(b"DSDT", 0), 0, 0);
		assert!(size > 36, "the DSDT's size is answered ({size})");
		let mut table = alloc::vec![0u8; size as usize];
		assert_eq!(invoke(abi::SYS_FIRMWARE_TABLE, firmware, selector(b"DSDT", 0), table.as_mut_ptr() as u64, table.len() as u64), size);
		assert_eq!(&table[..4], b"DSDT");
		assert_eq!(table.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)), 0, "and it checksums");
		// THE FACS through the FADT too, and every other table through the root table by instance.
		let facs = invoke(abi::SYS_FIRMWARE_TABLE, firmware, selector(b"FACS", 0), 0, 0);
		assert!(facs >= 64, "the FACS is answered ({facs})");
		assert!(invoke(abi::SYS_FIRMWARE_TABLE, firmware, selector(b"APIC", 0), 0, 0) > 0, "the MADT");
		assert_eq!(invoke(abi::SYS_FIRMWARE_TABLE, firmware, selector(b"APIC", 1), 0, 0), syscall::ERR_INVALID, "there is one MADT");
		assert_eq!(invoke(abi::SYS_FIRMWARE_TABLE, firmware, selector(b"XXXX", 0), 0, 0), syscall::ERR_INVALID, "and no such table");
		// A BUFFER TOO SMALL IS NOT WRITTEN, and the size is answered.
		let mut short = [0xAAu8; 8];
		assert_eq!(invoke(abi::SYS_FIRMWARE_TABLE, firmware, selector(b"DSDT", 0), short.as_mut_ptr() as u64, 8), size);
		assert_eq!(short, [0xAA; 8]);
		// THE CMOS: the clock's registers and the century byte stay the kernel's; a byte from 0x0E up is written and
		// read back, then restored.
		assert_eq!(invoke(abi::SYS_FIRMWARE_MEDIATED, firmware, abi::FIRMWARE_CMOS_READ, 0x00, 0), syscall::ERR_ACCESS_DENIED, "the seconds register");
		assert_eq!(invoke(abi::SYS_FIRMWARE_MEDIATED, firmware, abi::FIRMWARE_CMOS_WRITE, 0x0B, 0), syscall::ERR_ACCESS_DENIED, "status B");
		if let Some(century) = crate::arch::firmware::fadt().and_then(|fadt| fadt.century_index()) {
			assert_eq!(invoke(abi::SYS_FIRMWARE_MEDIATED, firmware, abi::FIRMWARE_CMOS_READ, century as u64, 0), syscall::ERR_ACCESS_DENIED, "the century byte");
		}
		assert_eq!(invoke(abi::SYS_FIRMWARE_MEDIATED, firmware, abi::FIRMWARE_CMOS_READ, 0x80, 0), syscall::ERR_ACCESS_DENIED, "past the bank");
		let held = invoke(abi::SYS_FIRMWARE_MEDIATED, firmware, abi::FIRMWARE_CMOS_READ, 0x5E, 0);
		assert!(held >= 0, "an NVRAM byte reads ({held})");
		assert_eq!(invoke(abi::SYS_FIRMWARE_MEDIATED, firmware, abi::FIRMWARE_CMOS_WRITE, 0x5E, 0xA5), 0);
		assert_eq!(invoke(abi::SYS_FIRMWARE_MEDIATED, firmware, abi::FIRMWARE_CMOS_READ, 0x5E, 0), 0xA5);
		assert_eq!(invoke(abi::SYS_FIRMWARE_MEDIATED, firmware, abi::FIRMWARE_CMOS_WRITE, 0x5E, held as u64), 0);
		// THE SMI COMMAND PORT refuses the ACPI-disable value, which would take the machine out of ACPI mode.
		if let Some((_, disable)) = crate::arch::firmware::fadt().and_then(|fadt| fadt.acpi_mode_values()) {
			assert_eq!(invoke(abi::SYS_FIRMWARE_MEDIATED, firmware, abi::FIRMWARE_SMI_COMMAND, disable as u64, 0), syscall::ERR_ACCESS_DENIED);
		}
		assert_eq!(invoke(abi::SYS_FIRMWARE_MEDIATED, firmware, abi::FIRMWARE_SMI_COMMAND, 0x100, 0), syscall::ERR_INVALID, "a value is a byte");
		// THE PM TIMER counts.
		let first = invoke(abi::SYS_FIRMWARE_MEDIATED, firmware, abi::FIRMWARE_PM_TIMER, 0, 0);
		assert!(first >= 0, "the PM timer reads ({first})");
		let mut moved = false;
		for _ in 0..100_000 {
			if invoke(abi::SYS_FIRMWARE_MEDIATED, firmware, abi::FIRMWARE_PM_TIMER, 0, 0) != first {
				moved = true;
				break;
			}
		}
		assert!(moved, "and counts");
		assert_eq!(invoke(abi::SYS_FIRMWARE_MEDIATED, firmware, abi::FIRMWARE_GLOBAL_LOCK_RELEASE, 0, 0), 0);
		assert_eq!(invoke(abi::SYS_FIRMWARE_MEDIATED, firmware, 99, 0, 0), syscall::ERR_INVALID);
	}
}

crate::tagged_test!(a_configuration_write_is_held_to_the_policy, [Kernel, Pci, Syscall], id = "kernel.firmware.a_configuration_write_is_held_to_the_policy", covers = ["kernel"]);
fn a_configuration_write_is_held_to_the_policy() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_: u64) {
		let firmware = privilege(PrivilegeKind::FirmwareInterpreter);
		let edu = crate::iommu::edu::find().expect("the test machine carries an edu function");
		let address = (edu.bus as u64) << 16 | (edu.dev as u64) << 8 | edu.func as u64;
		let access = |offset: u64, width: u64, write: bool| offset | width << 16 | if write { abi::FIRMWARE_PCI_WRITE } else { 0 };
		// ANY READ: the edu function's identity.
		assert_eq!(invoke(abi::SYS_FIRMWARE_PCI, firmware, address, access(0, 4, false), 0), 0x11e8_1234, "the edu function's vendor and device");
		assert_eq!(invoke(abi::SYS_FIRMWARE_PCI, firmware, address, access(1, 2, false), 0), syscall::ERR_INVALID, "a misaligned word");
		assert_eq!(invoke(abi::SYS_FIRMWARE_PCI, firmware, address | 1 << 32, access(0, 4, false), 0), syscall::ERR_UNSUPPORTED, "a second segment");
		// THE STANDARD HEADER is never written: the command register would give firmware the bus.
		assert_eq!(invoke(abi::SYS_FIRMWARE_PCI, firmware, address, access(0x04, 2, true), 0x0006), syscall::ERR_ACCESS_DENIED);
		// INSIDE THE MSI CAPABILITY the edu function carries.
		let mut at = crate::arch::pci::config_read_exact(edu.bus, edu.dev, edu.func, 0x34, 1).unwrap_or(0) as u16 & 0xFC;
		let mut msi = None;
		for _ in 0..48 {
			if at < 0x40 {
				break;
			}
			let header = crate::arch::pci::config_read_exact(edu.bus, edu.dev, edu.func, at, 2).unwrap_or(0);
			if header & 0xFF == 0x05 {
				msi = Some(at);
				break;
			}
			at = (header >> 8) as u16 & 0xFC;
		}
		let msi = msi.expect("the edu function has an MSI capability");
		assert_eq!(invoke(abi::SYS_FIRMWARE_PCI, firmware, address, access(msi as u64 + 4, 4, true), 0), syscall::ERR_ACCESS_DENIED, "the MSI address register");
		// A FUNCTION A DRIVER HOLDS is not written at all: the test machine's i6300esb, claimed under the entry that
		// declares it, then released.
		let (row, esb) = watchdog_function();
		let grant = crate::tests::claim_device(row as u64).expect("the i6300esb is claimed");
		assert_eq!(invoke(abi::SYS_FIRMWARE_PCI, firmware, esb, access(0x6C, 1, true), 0), syscall::ERR_ACCESS_DENIED, "a driver holds it");
		crate::tests::release_device(&grant);
		// THE KERNEL'S CHIPSET REGISTERS: q35's ECAM base.
		#[cfg(target_arch = "x86_64")]
		{
			assert_eq!(invoke(abi::SYS_FIRMWARE_PCI, firmware, 0, access(0, 4, false), 0), 0x29c0_8086, "q35's MCH at 00:00.0");
			assert_eq!(invoke(abi::SYS_FIRMWARE_PCI, firmware, 0, access(0x60, 4, true), 0), syscall::ERR_ACCESS_DENIED, "PCIEXBAR");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	in_thread(body, &DONE);
}

crate::tagged_test!(a_region_is_mapped_as_the_memory_it_is_and_a_companion_bar_is_held_by_the_firmware, [Kernel, Pci, Syscall], id = "kernel.firmware.a_region_is_mapped_as_the_memory_it_is_and_a_companion_bar_is_held_by_the_firmware", covers = ["kernel"]);
fn a_region_is_mapped_as_the_memory_it_is_and_a_companion_bar_is_held_by_the_firmware() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_: u64) {
		let firmware = privilege(PrivilegeKind::FirmwareInterpreter);
		// RAM IS REFUSED.
		let ram = (0..crate::mem::memmap_len()).filter_map(crate::mem::memmap_get).find(|region| region.kind == abi::MEMMAP_USABLE && region.length >= 0x1000).expect("the machine has RAM");
		assert_eq!(map(firmware, &map_request(ram.base, 0x1000, b"acpi:\\TST_", None)), syscall::ERR_ACCESS_DENIED, "RAM");
		// A HOLE IN THE MAP IS MMIO, mapped uncached.
		let hole = map(firmware, &map_request(free_window(0x3000), 0x100, b"acpi:\\TST_", None));
		assert!(hole > 0, "a free MMIO window maps ({hole})");
		let object = device_memory_of(hole as u64);
		assert!(!object.write_back(), "uncached");
		close(hole);
		// FIRMWARE MEMORY - ACPI NVS or firmware-reserved - is mapped write-back, where the machine has some outside
		// every kernel-held range.
		for kind in [abi::MEMMAP_ACPI_NVS, abi::MEMMAP_ACPI_RECLAIMABLE] {
			if let Some(region) = (0..crate::mem::memmap_len()).filter_map(crate::mem::memmap_get).find(|region| region.kind == kind && region.length >= 0x1000) {
				let handle = map(firmware, &map_request(region.base, 0x1000, b"acpi:\\TST_", None));
				assert!(handle > 0, "firmware memory of kind {kind} maps ({handle})");
				assert!(device_memory_of(handle as u64).write_back(), "write-back");
				close(handle);
			}
		}
		// A BAR: refused to any node but the function's companion; admitted to the companion, whose function becomes
		// FIRMWARE-HELD - shown in its row, answered by its node, and never claimed.
		let (row, address) = watchdog_function();
		let function = Function { segment: 0, bus: (address >> 16) as u8, device: (address >> 8) as u8, function: address as u8 };
		let (bar, _) = crate::arch::pci::function_bar(function.bus, function.device, function.function, 0).expect("the i6300esb's BAR");
		assert_eq!(map(firmware, &map_request(bar, 0x10, b"acpi:\\TST_", None)), syscall::ERR_ACCESS_DENIED, "another node's BAR");
		// A DRIVER HOLDS IT FIRST: refused even to the companion.
		let grant = crate::tests::claim_device(row as u64).expect("the i6300esb is claimed");
		assert_eq!(map(firmware, &map_request(bar, 0x10, b"acpi:\\_SB_.PCI0.WDT_", Some(function))), syscall::ERR_ACCESS_DENIED, "a driver holds the function");
		crate::tests::release_device(&grant);
		let held = map(firmware, &map_request(bar, 0x10, b"acpi:\\_SB_.PCI0.WDT_", Some(function)));
		assert!(held > 0, "the companion maps its own function's BAR ({held})");
		assert_eq!(device::info(row).map(|info| info.platform.state), Some(abi::PLATFORM_STATE_FIRMWARE_HELD), "the row says firmware-held");
		assert_ne!(device_node(row).flags & abi::FIRMWARE_NODE_FIRMWARE_HELD, 0, "and so does its node");
		assert_eq!(crate::tests::claim_device(row as u64).err(), Some(syscall::ERR_UNSUPPORTED), "no driver may take it");
		// HELD PAST THE MAPPING: the function is the service's, the next instance's too.
		close(held);
		assert_eq!(crate::tests::claim_device(row as u64).err(), Some(syscall::ERR_UNSUPPORTED), "still held with the region gone");
		super::forget_for_test();
		let again = crate::tests::claim_device(row as u64).expect("the suite's reset gives the function back");
		crate::tests::release_device(&again);
		DONE.store(true, Ordering::SeqCst);
	}
	in_thread(body, &DONE);
}

crate::tagged_test!(a_claim_and_another_node_s_region_refuse_each_other_whichever_came_first, [Drivers, Kernel, Syscall], id = "kernel.firmware.a_claim_and_another_node_s_region_refuse_each_other_whichever_came_first", covers = ["kernel"]);
fn a_claim_and_another_node_s_region_refuse_each_other_whichever_came_first() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_: u64) {
		let firmware = privilege(PrivilegeKind::FirmwareInterpreter);
		let (_, _events) = attach(firmware);
		// A NAMESPACE DEVICE with one MMIO range, claimable under the suite's test id.
		let window = free_window(0x4000);
		let mut description = namespace_description(b"acpi:\\_SB_.TST_.SHR_", abi::PLATFORM_STATE_CLAIMABLE, crate::dma_policy::SYNTHETIC_PLATFORM_HID);
		assert!(description.add_mmio(window, 0x1000));
		let row = device_report(firmware, description);
		assert!(row > 0, "the device is published ({row})");
		// THE REGION FIRST: another node maps part of the range, and the claim is refused while it does.
		let other = map(firmware, &map_request(window + 0x100, 0x10, b"acpi:\\_SB_.TST_.OTH_", None));
		assert!(other > 0, "no claim holds the range yet ({other})");
		assert_eq!(crate::tests::claim_device(row as u64).err(), Some(syscall::ERR_UNSUPPORTED), "the claim is refused under another node's region");
		close(other);
		// THE CLAIM FIRST: another node's region over it is refused; the device's own node's, inside its range, is not.
		let grant = crate::tests::claim_device(row as u64).expect("with the region gone the claim is taken");
		assert_eq!(map(firmware, &map_request(window + 0x100, 0x10, b"acpi:\\_SB_.TST_.OTH_", None)), syscall::ERR_ACCESS_DENIED, "another node's region over a claimed range");
		let own = map(firmware, &map_request(window + 0x100, 0x10, b"acpi:\\_SB_.TST_.SHR_", None));
		assert!(own > 0, "the device's own node shares its memory with its driver ({own})");
		close(own);
		crate::tests::release_device(&grant);
		super::forget_for_test();
		DONE.store(true, Ordering::SeqCst);
	}
	in_thread(body, &DONE);
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(a_namespace_row_s_ports_are_minted_from_its_claim_and_never_over_a_bar_or_a_bridge_window, [Drivers, Kernel, Pci, Syscall, ArchX86_64], id = "kernel.firmware.a_namespace_row_s_ports_are_minted_from_its_claim_and_never_over_a_bar_or_a_bridge_window", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn a_namespace_row_s_ports_are_minted_from_its_claim_and_never_over_a_bar_or_a_bridge_window() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_: u64) {
		let firmware = privilege(PrivilegeKind::FirmwareInterpreter);
		let (_, _events) = attach(firmware);
		// A `_CRS` WITH PORTS NOTHING HOLDS - COM3's, which the test machine does not carry - claimable under the suite's
		// test id: the claim mints them by index, as it mints a static row's, and the release takes them back.
		let mut description = namespace_description(b"acpi:\\_SB_.TST_.COM3", abi::PLATFORM_STATE_CLAIMABLE, crate::dma_policy::SYNTHETIC_PLATFORM_HID);
		assert!(description.add_port(0x3e8, 8), "a port range fits");
		let row = device_report(firmware, description);
		assert!(row > 0, "the device is published ({row})");
		let grant = crate::tests::claim_device(row as u64).expect("the row is claimed under the entry that declares it");
		let ports = invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_PORT_RANGE, 0, 0);
		assert!(ports > 0, "the claim mints the row's ports ({ports})");
		assert_eq!(invoke(abi::SYS_DEVICE_RESOURCE_ACQUIRE, grant.claim, abi::RESOURCE_KIND_PORT_RANGE, 1, 0), syscall::ERR_INVALID, "an index past the row's port ranges");
		crate::tests::release_device(&grant);
		{
			let thread = sched::current_thread().expect("a current thread");
			assert!(thread.handles().lock().lookup_typed(Handle::from_raw(ports as u64), crate::object::ObjectType::PortRange, Rights::NONE).is_err(), "the release revoked the range");
		}
		// A FUNCTION'S I/O BAR AND A BRIDGE'S I/O WINDOW are the bus's, and never a namespace device's.
		let bar = (0..device::count()).find_map(|index| device::with(index, |entry| if entry.platform.is_none() { entry.ports[..entry.port_count as usize].iter().find(|port| port.source == abi::PORT_SOURCE_IO_BAR).map(|port| port.base) } else { None }).flatten()).expect("a function of the test machine decodes an I/O BAR");
		let mut over_bar = namespace_description(b"acpi:\\_SB_.TST_.BAR_", abi::PLATFORM_STATE_CLAIMABLE, crate::dma_policy::SYNTHETIC_PLATFORM_HID);
		assert!(over_bar.add_port(bar, 4));
		assert_eq!(device_report(firmware, over_bar), syscall::ERR_ACCESS_DENIED, "ports over the I/O BAR at {bar:#x} are refused");
		let window = super::STATE.lock().io_windows.first().copied().expect("the test machine's hot-plug root port forwards an I/O window");
		let mut over_window = namespace_description(b"acpi:\\_SB_.TST_.WIN_", abi::PLATFORM_STATE_CLAIMABLE, crate::dma_policy::SYNTHETIC_PLATFORM_HID);
		assert!(over_window.add_port(window.0 + 0x10, 4));
		assert_eq!(device_report(firmware, over_window), syscall::ERR_ACCESS_DENIED, "ports inside the bridge window at {:#x} are refused", window.0);
		super::process_ended(koid());
		super::forget_for_test();
		DONE.store(true, Ordering::SeqCst);
	}
	in_thread(body, &DONE);
}

crate::tagged_test!(a_walk_is_reconciled_by_identity_and_loaded_withdraws_what_it_did_not_report, [Drivers, Kernel, Syscall], id = "kernel.firmware.a_walk_is_reconciled_by_identity_and_loaded_withdraws_what_it_did_not_report", covers = ["kernel"]);
fn a_walk_is_reconciled_by_identity_and_loaded_withdraws_what_it_did_not_report() {
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_: u64) {
		let firmware = privilege(PrivilegeKind::FirmwareInterpreter);
		let (events, theirs) = Channel::create();
		device::attach_events(events);
		// NO INSTANCE ATTACHED, NO REPORT.
		assert_eq!(loaded_report(firmware, 1), syscall::ERR_UNSUPPORTED, "a report needs an attached instance");
		let (first, _gpes) = attach(firmware);
		// A METHOD-ONLY DEVICE - no range, no line - published once and the same row when reported again.
		let kept = device_report(firmware, namespace_description(b"acpi:\\_SB_.LSF0", abi::PLATFORM_STATE_CLAIMABLE, b"LSFX0002"));
		assert!(kept > 0, "published ({kept})");
		assert_eq!(device_report(firmware, namespace_description(b"acpi:\\_SB_.LSF0", abi::PLATFORM_STATE_CLAIMABLE, b"LSFX0002")), kept, "reported again it is the same row");
		let mut differing = namespace_description(b"acpi:\\_SB_.LSF0", abi::PLATFORM_STATE_CLAIMABLE, b"LSFX0002");
		assert!(differing.add_mmio(free_window(0x5000), 0x1000));
		assert_eq!(device_report(firmware, differing), kept, "reported differing it is still that row, which keeps what it had");
		assert_eq!(device::with(kept as usize, |entry| entry.platform.as_ref().map(|row| row.part.mmio_count)), Some(Some(0)));
		// A SECOND ONE, which the next instance will not report.
		let gone = device_report(firmware, namespace_description(b"acpi:\\_SB_.LSF1", abi::PLATFORM_STATE_CLAIMABLE, b"LSFX0002"));
		assert!(gone > kept);
		// A RESERVATION: a row of its own, never claimable, keeping every new row out of its ranges.
		let mut reservation = namespace_description(b"acpi:\\_SB_.RES0", abi::PLATFORM_STATE_RESERVATION, b"PNP0C02");
		assert!(reservation.add_mmio(free_window(0x6000), 0x2000));
		let reserved = device_report(firmware, reservation);
		assert!(reserved > 0, "the reservation is recorded ({reserved})");
		assert_eq!(crate::tests::claim_device(reserved as u64).err(), Some(syscall::ERR_ACCESS_DENIED), "never claimable");
		let mut inside = namespace_description(b"acpi:\\_SB_.INS0", abi::PLATFORM_STATE_CLAIMABLE, b"LSFX0001");
		assert!(inside.add_mmio(free_window(0x7000), 0x100));
		assert_eq!(device_report(firmware, inside), syscall::ERR_ACCESS_DENIED, "a new row inside a reservation is refused");
		// A DESCRIPTION MERGED INTO A ROW THE KERNEL PUBLISHED: its ids join the row and nothing is minted.
		let mut stat = crate::tests::synthetic_platform_description(b"kernel:test-merge-target");
		assert!(stat.add_mmio(free_window(0x9000), 0x1000));
		let target = crate::tests::publish_synthetic_platform(stat).expect("the static row");
		let ids_before = device::with(target, |entry| entry.platform.as_ref().map(|row| row.part.match_count)).flatten();
		let mut merged = namespace_description(b"acpi:\\_SB_.MRG0", abi::PLATFORM_STATE_CLAIMABLE, b"LSFX0003");
		assert!(merged.add_mmio(free_window(0x9000), 0x1000));
		assert_eq!(device_report(firmware, merged), target as i64, "merged into the static row");
		assert!(device::with(target, |entry| entry.platform.as_ref().map(|row| row.part.match_count)).flatten() > ids_before, "its ids joined");
		// A COMPANION: the edu function's node, with a line list, answered by the function's node.
		let edu = crate::iommu::edu::find().expect("the edu function");
		let mut aei = List::default();
		assert!(aei.push(7));
		let companion = report::CompanionReport { path: b"acpi:\\_SB_.PCI0.EDU_", function: Function { segment: 0, bus: edu.bus, device: edu.dev, function: edu.func }, aei_lines: aei, field_lines: List::default(), field_addresses: List::default() };
		let mut out = [0u8; 512];
		let len = report::encode_companion(&companion, &mut out).expect("the companion encodes");
		let function_row = send_report(firmware, &out[..len]);
		assert!(function_row >= 0, "the companion is joined to the function's row ({function_row})");
		let node = device_node(function_row as usize);
		assert_eq!((node.flags & abi::FIRMWARE_NODE_COMPANION != 0, node.path(), node.aei()), (true, &b"acpi:\\_SB_.PCI0.EDU_"[..], &[7u32][..]));
		assert_eq!(send_report(firmware, &out[..len]), function_row, "joined again it is the same companion");
		// A NAMESPACE ROW THAT IS ITSELF A CONTROLLER carries its lists under its own identity.
		let mut lines = List::default();
		assert!(lines.push(5));
		let lists = report::ListsReport { identity: b"acpi:\\_SB_.LSF0", aei_lines: List::default(), field_lines: lines, field_addresses: List::default() };
		let len = report::encode_lists(&lists, &mut out).expect("the lists encode");
		assert_eq!(send_report(firmware, &out[..len]), 0);
		let node = device_node(kept as usize);
		assert_eq!((node.flags & abi::FIRMWARE_NODE_LISTS != 0, node.path(), node.field_lines()), (true, &b"acpi:\\_SB_.LSF0"[..], &[5u32][..]));
		// THE FIRST INSTANCE'S NAMESPACE IS LOADED: an arrival for each new row, then the report - and the report
		// named by a number that is not the instance's is refused.
		assert_eq!(loaded_report(firmware, first + 7), syscall::ERR_INVALID);
		assert_eq!(loaded_report(firmware, first), 0);
		let seen = device_events(&theirs);
		for row in [kept, gone, reserved] {
			assert_eq!(seen.iter().filter(|event| **event == (abi::DEVICE_EVENT_ARRIVED, row as u64)).count(), 1, "one arrival for row {row}: {seen:?}");
		}
		assert_eq!(seen.last(), Some(&(abi::DEVICE_EVENT_NAMESPACE_LOADED, first)), "the report last: {seen:?}");
		// A DEVICE TREE'S PCI CHILD, joined by the kernel at the boot scan: no walk reports it, and no "loaded" takes it.
		let tree_function = device::with(0, |entry| (entry.bus, entry.dev, entry.func)).expect("row 0 is a PCI function");
		assert_ne!(tree_function, (edu.bus, edu.dev, edu.func), "a function the namespace's companion does not describe");
		super::tree_companion(b"dt:/test-pcie/child@0", tree_function.0, tree_function.1, tree_function.2);
		// THE SERVICE RESTARTS: the second instance reports the first device and nothing else.
		super::process_ended(koid());
		let (second, _gpes_again) = attach(firmware);
		assert_eq!(second, first + 1);
		assert_eq!(device_report(firmware, namespace_description(b"acpi:\\_SB_.LSF0", abi::PLATFORM_STATE_CLAIMABLE, b"LSFX0002")), kept, "the same row, not a second");
		assert_eq!(loaded_report(firmware, second), 0);
		let seen = device_events(&theirs);
		assert!(!seen.iter().any(|event| *event == (abi::DEVICE_EVENT_ARRIVED, kept as u64)), "no event for a row reported again: {seen:?}");
		for row in [gone, reserved] {
			assert!(seen.iter().any(|event| *event == (abi::DEVICE_EVENT_DEPARTED, row as u64)), "row {row} was not reported again and is withdrawn: {seen:?}");
			assert_eq!(device::with(row as usize, |entry| entry.on_bus), Some(false), "its row stays, with its index");
		}
		assert_eq!(seen.last(), Some(&(abi::DEVICE_EVENT_NAMESPACE_LOADED, second)));
		assert_eq!(device::with(target, |entry| entry.platform.as_ref().map(|row| row.part.match_count)).flatten(), ids_before, "the static row keeps its place and loses only what was merged");
		assert_eq!(device::with(target, |entry| entry.on_bus), Some(true));
		assert_eq!(device_node(function_row as usize).flags & abi::FIRMWARE_NODE_COMPANION, 0, "the companion no walk reported is detached");
		let tree_node = device_node(0);
		assert_eq!((tree_node.flags & abi::FIRMWARE_NODE_COMPANION != 0, tree_node.path()), (true, &b"dt:/test-pcie/child@0"[..]), "the tree's companion is the kernel's, and stays");
		assert_eq!(crate::tests::claim_device(gone as u64).err(), Some(syscall::ERR_ACCESS_DENIED), "a withdrawn row is not claimable");
		// REPORTED AGAIN, THE WITHDRAWN ROW IS REFILLED: the same index, a new generation, an arrival.
		let generation = device::claim_generation(gone as usize);
		assert_eq!(device_report(firmware, namespace_description(b"acpi:\\_SB_.LSF1", abi::PLATFORM_STATE_CLAIMABLE, b"LSFX0002")), gone, "refilled in its own row");
		assert_ne!(device::claim_generation(gone as usize), generation, "with a generation that moved");
		assert!(device_events(&theirs).contains(&(abi::DEVICE_EVENT_ARRIVED, gone as u64)));
		// AND A WALK'S OWN WITHDRAWAL: a departure.
		assert_eq!(withdraw_report(firmware, b"acpi:\\_SB_.LSF1"), 0);
		assert!(device_events(&theirs).contains(&(abi::DEVICE_EVENT_DEPARTED, gone as u64)));
		assert_eq!(withdraw_report(firmware, b"acpi:\\_SB_.NONE"), syscall::ERR_INVALID, "nothing carries that identity");
		super::process_ended(koid());
		super::forget_for_test();
		DONE.store(true, Ordering::SeqCst);
	}
	in_thread(body, &DONE);
}

crate::tagged_test!(only_the_running_instance_asks_for_general_purpose_events, [Kernel, Syscall], id = "kernel.firmware.only_the_running_instance_asks_for_general_purpose_events", covers = ["kernel"]);
fn only_the_running_instance_asks_for_general_purpose_events() {
	// THE REGISTERS ARE THE SCI HANDLER'S, which a test build does not carry - it would reprogram an interrupt
	// controller under the suite - so here the events are unsupported, and what is proved is the gate in front of
	// them: no request but the count from anything but the running instance, and none after it ended. The state
	// machine is `acpi::gpe`'s host suite; the delivery, the QEMU gate's CPU hot-plug event.
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_: u64) {
		let firmware = privilege(PrivilegeKind::FirmwareInterpreter);
		super::forget_for_test();
		assert_eq!(invoke(abi::SYS_FIRMWARE_GPE, firmware, abi::GPE_COUNT, 0, 0), syscall::ERR_UNSUPPORTED, "the count is anybody's, and a test build has no blocks");
		assert_eq!(invoke(abi::SYS_FIRMWARE_GPE, firmware, abi::GPE_ENABLE, 0x0F, 0), syscall::ERR_ACCESS_DENIED, "no instance is attached");
		let (_, _events) = attach(firmware);
		assert_eq!(invoke(abi::SYS_FIRMWARE_GPE, firmware, abi::GPE_ENABLE, 0x0F, 0), syscall::ERR_UNSUPPORTED, "the running instance reaches the blocks");
		super::process_ended(koid());
		assert_eq!(invoke(abi::SYS_FIRMWARE_GPE, firmware, abi::GPE_REARM, 0x0F, 0), syscall::ERR_ACCESS_DENIED, "and nothing once it ended");
		super::forget_for_test();
		DONE.store(true, Ordering::SeqCst);
	}
	in_thread(body, &DONE);
}

crate::tagged_test!(the_boot_framebuffers_decoder_is_the_function_whose_memory_bar_holds_it, [Kernel, Pci], id = "kernel.firmware.the_boot_framebuffers_decoder_is_the_function_whose_memory_bar_holds_it", covers = ["kernel"]);
fn the_boot_framebuffers_decoder_is_the_function_whose_memory_bar_holds_it() {
	let vga = Function { segment: 0, bus: 0, device: 1, function: 0 };
	let bridge = Function { segment: 0, bus: 0, device: 0x1c, function: 0 };
	let gpu = Function { segment: 0, bus: 1, device: 0, function: 0 };
	let ranges = [(0xE000_0000u64, 0x1000_0000u64, bridge, true), (0xE000_0000, 0x0100_0000, gpu, false), (0xFD00_0000, 0x0100_0000, vga, false)];
	assert_eq!(super::decoder_in(ranges.iter().copied(), 0xFD00_0000), Some(vga), "the base itself");
	assert_eq!(super::decoder_in(ranges.iter().copied(), 0xFDFF_F000), Some(vga), "inside the BAR");
	assert_eq!(super::decoder_in(ranges.iter().copied(), 0xFE00_0000), None, "one past its end decodes nothing");
	assert_eq!(super::decoder_in(ranges.iter().copied(), 0xE000_8000), Some(gpu), "the function behind a bridge, never the bridge's window");
	assert_eq!(super::decoder_in(ranges.iter().copied(), 0xEF00_0000), None, "a window alone is no decoder");
	assert_eq!(super::decoder_in([(0x1000u64, 0u64, vga, false)].into_iter(), 0x1000), None, "an empty BAR holds nothing");
	// AND THE BOOT ITSELF: on the test profile the boot framebuffer lies in a function's BAR the scan recorded.
	if let Some((address, _)) = crate::framebuffer_geometry()
		&& let Some(physical) = crate::arch::paging::translate(address)
	{
		let decoder = super::decoder_of(physical);
		crate::serial_println!("firmware: the boot framebuffer at {physical:#x} is decoded by {:?}", decoder.map(|function| (function.bus, function.device, function.function)));
	}
}

crate::tagged_test!(a_dmar_namespace_name_is_the_identity_the_service_publishes, [Kernel], id = "kernel.firmware.a_dmar_namespace_name_is_the_identity_the_service_publishes", covers = ["kernel"]);
fn a_dmar_namespace_name_is_the_identity_the_service_publishes() {
	assert_eq!(super::namespace_identity(b"\\_SB.PCI0.I2C1").as_deref(), Some(&b"acpi:\\_SB_.PCI0.I2C1"[..]), "every segment four characters");
	assert_eq!(super::namespace_identity(b"_SB.UAR").as_deref(), Some(&b"acpi:\\_SB_.UAR_"[..]), "a name without the root's backslash is from the root");
	assert_eq!(super::namespace_identity(b"\\_SB.TOOLONG"), None, "a segment past four characters is no namespace name");
	assert_eq!(super::namespace_identity(b"\\_SB..X"), None, "nor is an empty one");
}
