// x86_64 PCI config-space access: the legacy configuration mechanism #1 (I/O ports
// 0xCF8/0xCFC). This is the ONLY architecture-specific part of PCI enumeration - the
// device tables, capability walk, BAR decoding and MSI-X resolution all live in
// `arch::common::pci`, generic over the `ConfigAccess` primitives implemented here.
// QEMU's q35 places the virtio endpoints and any xHCI controller on bus 0, and its
// firmware assigns the BARs, so this backend probes bus 0 only and needs no BAR
// allocator (`assign_bars` stays the common no-op).

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use super::port::{inl, outl};
use crate::arch::common::pci as common;
use crate::sync::SpinLock;

// The PCI surface every backend re-exports (the HAL contract); not every type is
// named directly in this backend's code.
// `SlotChange` and `SlotEvent` are what the boot's hot-plug handler reads, and that handler is
// `not(test)` - so a test build re-exports two names nothing in it uses. SAID IN A CFG RATHER
// THAN SUPPRESSED: the two names travel with the handler that reads them.
pub use common::{PciDevice, ResourcedDevice, VirtioDevice};
// AND THE HOT-PLUG HALF, WHICH A TEST BUILD NAMES NOWHERE: every shim that reads an error
// record or a slot event is `not(test)`, because what a kernel test drives is a fake config
// space and not this machine's.
#[cfg(not(test))]
pub use common::{ErrorRecord, HotPlugPort, MAX_ERROR_REPORTERS, MAX_HOT_PLUG_PORTS, PowerEvent, SlotChange, SlotEvent};

// The PCI configuration mechanism #1 ports.
const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

// Mechanism #1 is a TRANSACTION, and it was being issued as two independent accesses.
//
// `CONFIG_ADDRESS` selects the register and `CONFIG_DATA` transfers it, so the pair is only one
// operation while nothing runs between them. Two cores interleaving means one reads or writes the
// other's device:
//
//     CPU0: out CF8 = device A
//     CPU1: out CF8 = device B
//     CPU0: in  CFC        -> device B's register
//
// The boot scan never showed it because the scan is serial on one core. Runtime does: `msix_enable`
// is reached from `SYS_INTERRUPT_CREATE_MSI` and `set_intx_disabled` from device acquisition, so
// ring 3 on any core issues this pair - and a write landing on the wrong function is a command
// register written into somebody else's device.
//
// x86 only, and the reason belongs here rather than in a commit message: ECAM puts the register in
// the ADDRESS, so the same read on aarch64 and riscv64 is a single `read_volatile` with nothing to
// interleave. Mechanism #1 puts it in a side register, and that is what makes it a transaction
// rather than an access.
static CONFIG_PORTS: SpinLock<()> = SpinLock::new(());

// Build the CONFIG_ADDRESS value selecting a device's config dword. `offset` is
// rounded down to a 4-byte boundary (the dword the field lives in).
fn address(bus: u8, dev: u8, func: u8, offset: u16) -> u32 {
	0x8000_0000 | (bus as u32) << 16 | (dev as u32) << 11 | (func as u32) << 8 | (offset as u32 & 0xFC)
}

// EXTENDED CONFIG SPACE IS NOT REACHABLE THROUGH MECHANISM #1, AND THE FAILURE IS SILENT.
//
// `CONFIG_ADDRESS` carries EIGHT BITS of register number, so a read at 0x104 is a read at 0x04 -
// which answers the command and status registers and would be reported as an error status. Every
// PCIe EXTENDED capability lives at 0x100 or past it: Advanced Error Reporting is the first of them.
// So a kernel that wants to surface an AER record on this architecture has to reach config space the
// other way, through the memory-mapped window ACPI's MCFG table describes.
//
// MAPPED ONCE AND NEVER REMAPPED, WHICH IS THE ONLY SHAPE THAT WORKS HERE.
//
// The first version of this borrowed ONE page and remapped it per access, under the config lock. It
// panicked on the second access: `map_page` REFUSES to replace a live mapping, deliberately - a
// mapper that overwrote one would silently drop whatever frame was there - so "remap a scratch page"
// is not an operation this kernel has. Mapping the window once is also simply better: no lock
// interaction, no TLB question, and a read that is one volatile load.
//
// AND NOT THE WHOLE WINDOW. The MCFG on this machine describes 256 buses, which is 256 MB of address
// space and five hundred page tables for a register almost nothing reads. This kernel follows
// bridges from bus 0 and reaches a handful; the window is mapped for the first `ECAM_MAPPED_BUSES`
// of them and a function past that answers as though it had no extended capability - which is the
// same rule `MAX_BUS` already states for the bus walk: the walk stops where the mechanism does.
const ECAM_VIRT_BASE: u64 = 0xffff_f400_0000_0000;
#[cfg(not(test))]
const ECAM_MAPPED_BUSES: u64 = 16;
#[cfg(not(test))]
const ECAM_MAPPED_BYTES: u64 = ECAM_MAPPED_BUSES << 20;

// The MCFG window's base for segment 0, and how many buses of it are mapped. Zero means no MCFG was
// found, which is a machine where extended config space does not exist as far as this kernel is
// concerned - and that is SAID rather than guessed at, because a guessed base is a read of whatever
// the map holds there.
static ECAM_BASE: AtomicU64 = AtomicU64::new(0);
static ECAM_BUSES: AtomicU64 = AtomicU64::new(0);

// Record the MCFG window and map what this kernel will read of it.
//
// CALLED ONCE AT BOOT, BEFORE ANY PROCESS ADDRESS SPACE EXISTS, because `new_address_space` COPIES
// the kernel half: a mapping that needs a new top-level entry after one has been made reaches only
// the address space that made it, and switching to any other loads a CR3 that does not map the
// kernel executing in it. The top-level slot is reserved for the same reason.
#[cfg(not(test))]
pub fn set_ecam_window(base: u64, first_bus: u8, last_bus: u8) {
	if last_bus < first_bus || first_bus != 0 {
		// A WINDOW THAT DOES NOT START AT BUS ZERO IS NOT ONE THIS KERNEL USES. The addressing below
		// is `base + (bus << 20)`, which assumes the window's first bus is zero; a segment starting
		// elsewhere would need its own base, and this machine has never presented one.
		crate::serial_println!("pci: the MCFG window covers buses {first_bus}..={last_bus}, which does not start at zero; extended config space is unavailable");
		return;
	}
	let buses = ECAM_MAPPED_BUSES.min(last_bus as u64 + 1);
	super::paging::reserve_kernel_top_level(ECAM_VIRT_BASE, ECAM_MAPPED_BYTES);
	let flags = super::paging::WRITABLE | super::paging::NO_CACHE | super::paging::NO_EXECUTE;
	for page in 0..(buses << 20) / 0x1000 {
		super::paging::map_page(ECAM_VIRT_BASE + page * 0x1000, base + page * 0x1000, flags);
	}
	ECAM_BASE.store(base, Ordering::Release);
	ECAM_BUSES.store(buses, Ordering::Release);
	crate::serial_println!("pci: extended config space at {base:#x} for buses 0..={} of the window's 0..={last_bus}", buses - 1);
}

// Where a function's config page is in the mapped window, or `None` when there is no window or the
// bus is outside what was mapped. A read past it is a read of whatever the map holds next, which is
// why this answers `None` rather than clamping.
fn ecam_virt(bus: u8, dev: u8, func: u8) -> Option<u64> {
	if ECAM_BASE.load(Ordering::Acquire) == 0 || (bus as u64) >= ECAM_BUSES.load(Ordering::Acquire) {
		return None;
	}
	Some(ECAM_VIRT_BASE + ((bus as u64) << 20) + ((dev as u64) << 15) + ((func as u64) << 12))
}

// Read or write one dword of extended config space. ONE VOLATILE ACCESS AND NOTHING ELSE: the window
// is mapped uncached at boot, so there is no address register to set and nothing to serialise - which
// is what an ECAM access is on every other architecture here, and the reason the lock this backend
// holds around mechanism #1 buys nothing for this half.
fn ecam_access(bus: u8, dev: u8, func: u8, off: u16, value: Option<u32>) -> Option<u32> {
	let at = (ecam_virt(bus, dev, func)? + (off as u64 & 0xFFC)) as *mut u32;
	match value {
		// SAFETY: the address is inside a window mapped uncached at boot, and the offset is masked
		// into the function's own four-kilobyte page.
		None => Some(unsafe { core::ptr::read_volatile(at) }),
		Some(value) => {
			unsafe { core::ptr::write_volatile(at, value) };
			Some(value)
		}
	}
}

// The config-space access mechanism: dword reads/writes through the CF8/CFC ports.
// The byte/word reads and every enumeration routine come from `common` unchanged.
struct Access;

impl common::ConfigAccess for Access {
	// The ROOT bus, not the only bus. Legacy CF8/CFC has no way to know how many buses exist, so
	// enumeration starts at 0 and follows every bridge it finds from there - which is how a device
	// behind a PCIe root port or a `pcie-pci-bridge` is reached. It used to stop here, so anything
	// not placed directly on bus 0 was never read.
	const BUS_COUNT: u16 = 1;

	// WHETHER EXTENDED CONFIG SPACE IS REACHABLE IS A PROPERTY OF THE MACHINE HERE and not of the
	// mechanism: it depends on whether firmware described an MCFG window. Every other backend in this
	// tree reaches config space through ECAM already and answers `true` without asking.
	fn extended_reach() -> bool {
		ECAM_BASE.load(Ordering::Acquire) != 0
	}

	fn read32(bus: u8, dev: u8, func: u8, off: u16) -> u32 {
		let _serialised = CONFIG_PORTS.lock();
		Self::read32_raw(bus, dev, func, off)
	}

	fn write32(bus: u8, dev: u8, func: u8, off: u16, val: u32) {
		let _serialised = CONFIG_PORTS.lock();
		Self::write32_raw(bus, dev, func, off, val);
	}

	// The lock held across a whole read-modify-write rather than across each half of it. It is not
	// reentrant, which is why the `_raw` forms exist and why they are the only accesses allowed
	// inside.
	fn with_config<R>(f: impl FnOnce() -> R) -> R {
		let _serialised = CONFIG_PORTS.lock();
		f()
	}

	// AN OFFSET PAST THE FIRST 256 BYTES GOES THE OTHER WAY, and a machine with no window answers
	// ALL ONES - which is what an absent capability reads as, so a walk over it stops rather than
	// finding a capability at the command register.
	fn read32_raw(bus: u8, dev: u8, func: u8, off: u16) -> u32 {
		if off >= 0x100 {
			return ecam_access(bus, dev, func, off, None).unwrap_or(u32::MAX);
		}
		unsafe {
			outl(CONFIG_ADDRESS, address(bus, dev, func, off));
			inl(CONFIG_DATA)
		}
	}

	fn write32_raw(bus: u8, dev: u8, func: u8, off: u16, val: u32) {
		if off >= 0x100 {
			ecam_access(bus, dev, func, off, Some(val));
			return;
		}
		unsafe {
			outl(CONFIG_ADDRESS, address(bus, dev, func, off));
			outl(CONFIG_DATA, val);
		}
	}
}

// Enumerate every present function reachable from bus 0, following bridges.
pub fn scan() -> Vec<PciDevice> {
	common::scan::<Access>()
}

// Scan the bus and resolve every modern virtio device's MMIO layout.
pub fn scan_virtio() -> Vec<VirtioDevice> {
	common::scan_virtio::<Access>()
}

// Scan the bus and resolve every xHCI USB host controller's MMIO window.
pub fn scan_resourced() -> Vec<ResourcedDevice> {
	common::scan_resourced::<Access>()
}

// The identity of one function, or `None` where nothing is there - see
// `arch::common::pci::probe_function`.
pub fn probe_function(bus: u8, dev: u8, func: u8) -> Option<PciDevice> {
	common::probe_function::<Access>(bus, dev, func)
}

// One function's virtio layout and one function's resourced window, for a device that arrived after
// the boot scan - see `arch::common::pci::resolve_virtio_function`.
pub fn resolve_virtio_function(function: &PciDevice) -> Option<VirtioDevice> {
	common::resolve_virtio_function::<Access>(function)
}

pub fn resolve_endpoint_function(function: &PciDevice) -> Option<ResourcedDevice> {
	common::resolve_endpoint_function::<Access>(function)
}

// Power one hot-plug slot off or on - see `arch::common::pci::set_slot_power`.
// THE BOOT'S HOT-PLUG PATH ONLY, and so not compiled into a test build: the slot protocol's own
// tests drive the shared module directly, over a fake config space, and the handler that reaches for
// these wrappers is the boot's - which a test kernel does not have.
#[cfg(not(test))]
pub fn set_slot_power(bus: u8, dev: u8, func: u8, on: bool) {
	common::set_slot_power::<Access>(bus, dev, func, on);
}

// Every hot-plug port this machine has - see `arch::common::pci::hot_plug_ports`.
#[cfg(not(test))]
pub fn hot_plug_ports(out: &mut [Option<HotPlugPort>; MAX_HOT_PLUG_PORTS]) -> usize {
	common::hot_plug_ports(out)
}

// Read every hot-plug slot and answer what changed - see `arch::common::pci::poll_slots`.
#[cfg(not(test))]
pub fn poll_slots(out: &mut [common::SlotChange; common::MAX_HOT_PLUG_PORTS]) -> usize {
	common::poll_slots::<Access>(out)
}

// Bind this hot-plug port's slot interrupt to `handler`, and answer the interrupt number it was
// bound to. `None` for a port that raises none, whose slot is polled on the idle pass instead.
//
// x86 FIRMWARE ROUTED THE LINE AND WROTE THE NUMBER DOWN, which is the whole of what makes this
// port's arming different from the two that boot from a device tree. A BIOS or a UEFI picked an I/O
// APIC input for this function's pin and put it in config space, so there is nothing to resolve:
// the number is read back, the entry is pointed at the boot core, and the handler is registered on
// the vector that entry raises.
#[cfg(not(test))]
pub fn arm_slot_interrupt(port: &common::HotPlugPort, handler: crate::arch::interrupts::HandlerFn) -> Option<u32> {
	let line: u8 = common::slot_interrupt_line::<Access>(port)?;
	common::set_intx_disabled::<Access>(port.bus, port.dev, port.func, false);
	crate::arch::interrupts::register(crate::arch::interrupts::IRQ_BASE as u32 + line as u32, handler);
	// LEVEL-TRIGGERED AND ACTIVE-LOW, WHICH IS WHAT AN INTx PIN IS. This routed the ISA defaults,
	// and the comment on `settle_hot_plug` already named the consequence without connecting it to
	// the cause: "a level-triggered line one handler already cleared" is a line whose EDGE nobody
	// saw. Every event this path reports is acknowledged inside the handler - `poll_slots` writes
	// the slot's sticky bits back before it returns - so the source is clear before the EOI, which
	// is the condition a level entry needs.
	crate::arch::ioapic::route(line as u32, crate::arch::interrupts::IRQ_BASE + line, crate::smp::lapic_id(0), crate::arch::ioapic::Kind::LevelLow);
	Some(line as u32)
}

// Set or clear a function's PCI command-register Interrupt Disable bit (bit 10).
pub fn set_intx_disabled(bus: u8, dev: u8, func: u8, disabled: bool) {
	common::set_intx_disabled::<Access>(bus, dev, func, disabled);
}

// Turn bus mastering on or off for one function. The only caller is `device`, which knows whether a
// driver owns the device - see `arch::common::pci::set_bus_master`.
pub fn set_bus_master(bus: u8, dev: u8, func: u8, on: bool) {
	common::set_bus_master::<Access>(bus, dev, func, on);
}

// WHERE THIS PORT'S INTERRUPTS ARE WRITTEN, for a translated endpoint that named no doorbell of its
// own.
//
// A device's MSI is a memory write, so behind a translating IOMMU it needs a mapping like any other
// write. An endpoint that reports an MSI reserved region says where; one that offers no PROBE at
// all, or lists no such region, says nothing - and used to end up with no doorbell mapping and no
// interrupts, silently. This is the address that endpoint would have named.
pub fn msi_doorbell() -> Option<(u64, u64)> {
	// The local-APIC message window: `0xFEE00000 | dest << 12`, so the whole megabyte belongs to it
	// and every destination in this machine is inside the one range.
	Some((0xFEE0_0000, 0x10_0000))
}

// One function's memory BAR, resolved live from configuration space: its assigned base and its
// probed size.
//
// FOR A FUNCTION THIS KERNEL BINDS NO DRIVER TO. The device table admits resolved virtio and xHCI
// functions only, and the IOMMU fixture needs the PCI `edu` device - which is neither. Retaining a
// window for an arbitrary function is what that fixture needs and what this provides; it does not
// map anything or grant anything, it reads two registers.
//
// NOT TEST-ONLY ANY MORE (P02M0173): the bypass transition quiesces every firmware-touched
// function by its class before it clears bus mastering, and an NVMe controller - the riscv64 UEFI
// boot's ESP - is admitted to the table without a BAR, so its registers are resolved here.
pub fn function_bar(bus: u8, dev: u8, func: u8, index: usize) -> Option<(u64, u64)> {
	let device = common::probe_function::<Access>(bus, dev, func)?;
	let base = common::bar_address::<Access>(&device, index)?;
	let size = common::bar_size::<Access>(&device, index)?;
	if base == 0 || size == 0 { None } else { Some((base, size)) }
}

// One function's COMMAND register, read back - test-only, see `arch::common::pci::command`.
#[cfg(test)]
pub fn command(bus: u8, dev: u8, func: u8) -> u16 {
	common::command::<Access>(bus, dev, func)
}

// Enable MSI-X on a device and ensure its memory space is decoded. `cap` is the
// MSI-X capability's config-space offset (from VirtioDevice::msix_cap).
pub fn msix_enable(bus: u8, dev: u8, func: u8, cap: u16) {
	common::msix_enable::<Access>(bus, dev, func, cap);
}

// The other half - see `common::msix_disable`. A released claim leaves the function unable to send.
pub fn msix_disable(bus: u8, dev: u8, func: u8, cap: u16) {
	common::msix_disable::<Access>(bus, dev, func, cap);
}

#[cfg(test)]
mod tests;

// What the bus reported about itself: every watched function's error record, read and CLEARED.
#[cfg(not(test))]
pub fn poll_errors(out: &mut [common::ErrorRecord; common::MAX_ERROR_REPORTERS]) -> usize {
	common::poll_errors::<Access>(out)
}

// Every hot-plug port's power-management event, read and cleared.
#[cfg(not(test))]
pub fn poll_power_events(out: &mut [common::PowerEvent; common::MAX_HOT_PLUG_PORTS]) -> usize {
	common::poll_power_events::<Access>(out)
}

// The names the two error halves go by, so a report says what happened rather than a bit number.
#[cfg(not(test))]
pub fn uncorrectable_name(bits: u32) -> &'static str {
	common::uncorrectable_name(bits)
}

#[cfg(not(test))]
pub fn correctable_name(bits: u32) -> &'static str {
	common::correctable_name(bits)
}

// Watch a function for errors if it can report any, answering whether it can.
//
// CALLED WHERE A DEVICE ARRIVES as well as at the scan: a card plugged into a live machine reports
// errors like any other, and a list built once at boot would watch every function except the ones
// somebody put in afterwards.
pub fn note_error_reporter(bus: u8, dev: u8, func: u8) -> bool {
	common::note_error_reporter::<Access>(bus, dev, func)
}
