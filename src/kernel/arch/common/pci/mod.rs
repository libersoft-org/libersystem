// Portable PCI / PCIe enumeration - shared by every architecture backend.
//
// PCI configuration space is a bus standard: the device identity, the BAR layout,
// the capability list, the virtio MMIO structure descriptors and the MSI-X capability
// are all architecture-independent. The ONLY arch-specific part is how a config-space
// dword is reached - x86 issues the I/O-port mechanism #1 (0xCF8/0xCFC), while ECAM
// platforms (aarch64, riscv64 on QEMU `virt`) map the config space as MMIO. Each
// backend therefore implements the tiny `ConfigAccess` trait (a `read32` / `write32`
// primitive, the bus count, and - where no firmware assigns BARs - a BAR allocator),
// and gets all of the enumeration below for free.
//
// Discovery has to live in the kernel: reading config space needs either the I/O-port
// instructions ring 3 cannot issue or the ECAM MMIO the kernel keeps mapped. The
// resolved device tables are handed to DeviceManager, which maps each BAR to a
// userspace driver via a DeviceMemory capability.

use crate::sync::SpinLock;
use alloc::vec::Vec;

// virtio's PCI vendor id (Red Hat / virtio).
pub const VIRTIO_VENDOR: u16 = 0x1AF4;

// Modern virtio-pci device ids are 0x1040 + the virtio device type.
const VIRTIO_MODERN_BASE: u16 = 0x1040;

// THE CLASS TRIPLES THIS KERNEL RESOLVES A REGISTER WINDOW FOR, and what it calls each.
//
// It was one function per family - `is_xhci`, `resolve_xhci`, `scan_xhci`, a shim in each of the
// three architectures and a loop of its own in `device.rs` - which is six edits to add a second
// family and is why there was only ever one. AHCI, SDHCI, HDA, EHCI, OHCI/UHCI and TPM are all
// standard PCI functions whose whole identity is a class triple, and each of them needs exactly
// what this table gives: BAR 0 and an MSI-X vector.
//
// IT IS A TABLE AND NOT "EVERY FUNCTION ON THE BUS", which is a deliberate line. A function this
// kernel resolves nothing for still gets an inventory row carrying its standards identity and NO
// resources, so a rule can match it and nothing can claim what it does not have. Resourcing the
// whole bus would reverse that decision silently; adding a row states which family was decided on.
// THE LAST FIELD IS WHICH BAR HOLDS THE REGISTER FILE, and it is here because the second family to
// need this table disagreed with the first two. xHCI and NVMe both put their whole register file in
// BAR 0, which made "resolve BAR 0" and "resolve the register file" the same sentence; AHCI's ABAR
// is BAR 5. So the row carries the index and the resolver stays one function. SDHCI, HDA and the
// OHCI/UHCI pair each name their own BAR too, and inherit this rather than discovering it again.
const RESOURCED: &[(u8, u8, u8, u32, usize)] = &[
	(abi::PCI_CLASS_SERIAL_BUS, abi::PCI_SUBCLASS_USB, abi::PCI_PROG_IF_XHCI, abi::DEVICE_TYPE_XHCI, 0),
	(abi::PCI_CLASS_MASS_STORAGE, abi::PCI_SUBCLASS_NVM, abi::PCI_PROG_IF_NVME, abi::DEVICE_TYPE_NVME, 0),
	(abi::PCI_CLASS_MASS_STORAGE, abi::PCI_SUBCLASS_SATA, abi::PCI_PROG_IF_AHCI, abi::DEVICE_TYPE_AHCI, 5),
	(abi::PCI_CLASS_BASE_PERIPHERAL, abi::PCI_SUBCLASS_SD_HOST, abi::PCI_PROG_IF_SD_HOST, abi::DEVICE_TYPE_SDHCI, 0),
	(abi::PCI_CLASS_MULTIMEDIA, abi::PCI_SUBCLASS_AUDIO_DEVICE, abi::PCI_PROG_IF_HDA, abi::DEVICE_TYPE_HDA, 0),
	// AN IPMI SYSTEM INTERFACE, KCS or BT: BAR 0 is its register file. Resolved when it is a memory BAR; an I/O BAR 0
	// resolves no window here, and its function reaches the driver through its recorded port range instead.
	(abi::PCI_CLASS_SERIAL_BUS, abi::PCI_SUBCLASS_IPMI, abi::PCI_PROG_IF_IPMI_KCS, abi::DEVICE_TYPE_IPMI_KCS, 0),
	(abi::PCI_CLASS_SERIAL_BUS, abi::PCI_SUBCLASS_IPMI, abi::PCI_PROG_IF_IPMI_BT, abi::DEVICE_TYPE_IPMI_BT, 0),
];

// THE FUNCTIONS RESOLVED BY IDENTITY, whose class names nothing a register file could be found by: QEMU's `ivshmem-plain`,
// whose window is BAR 2, the memory a host file backs.
const RESOURCED_BY_ID: &[(u16, u16, u32, usize)] = &[(abi::PCI_VENDOR_REDHAT, abi::PCI_DEVICE_IVSHMEM, abi::DEVICE_TYPE_SHARED_MEMORY, 2)];

// PCI status register bit 4: a capability list is present (pointer at offset 0x34).
const STATUS_CAP_LIST: u16 = 1 << 4;
// Vendor-specific capability id; virtio describes its MMIO structures with these.
const CAP_ID_VENDOR: u8 = 0x09;
// MSI-X capability id. Message Control at +2 (bit 15 = MSI-X Enable, bit 14 =
// Function Mask, bits 10:0 = table size - 1); Table Offset/BIR at +4 (bits 2:0 =
// which BAR, bits 31:3 = byte offset into it).
const MSIX_CAP_ID: u8 = 0x11;
// The PCI Express capability, which is where a port's slot lives.
const PCIE_CAP_ID: u8 = 0x10;
// Bits 4..7 of the PCI Express Capabilities register: the port type. A slot belongs to a ROOT PORT
// or to a switch's DOWNSTREAM port and to nothing else - an endpoint has the same capability and the
// same offsets, and its bytes there are not a slot.
const PCIE_TYPE_ROOT_PORT: u16 = 4;
const PCIE_TYPE_DOWNSTREAM_PORT: u16 = 6;
// Bit 8: this port implements a slot. WITHOUT IT THE SLOT REGISTERS ARE NOT SLOT REGISTERS, which is
// the check a reader skips and then sees a device present in a slot that does not exist.
const PCIE_SLOT_IMPLEMENTED: u16 = 1 << 8;
// Slot Capabilities bit 5: the slot can be hot-plugged at all. One that cannot will never report a
// change, so watching it is a loop that never ends and never fires.
const SLOT_CAP_HOT_PLUG: u32 = 1 << 5;
// Slot Status bit 6: what is in the slot NOW. Slot Status bit 3: that it CHANGED since this bit was
// last cleared. Two different questions, and a reader needs both - see `SlotEvent`.
const SLOT_STATUS_PRESENT: u16 = 1 << 6;
const SLOT_STATUS_PRESENCE_CHANGED: u16 = 1 << 3;
// THE ATTENTION BUTTON IS HOW A REMOVAL IS ASKED FOR, and it is the half that makes this
// coordinated rather than a surprise. A managed removal does not begin with the device leaving: it
// begins with somebody ASKING - a person pressing the button on the chassis, or an operator typing
// the equivalent - and what the system owes in return is to stop the driver, let go of the
// resources, and only then power the slot down. A port that tore the device out first would leave a
// driver holding a mapping of something that is gone.
const SLOT_STATUS_ATTENTION_BUTTON: u16 = 1 << 0;
const SLOT_CONTROL_ATTENTION_BUTTON_ENABLE: u16 = 1 << 0;
// Slot Control bit 3: report a presence change. Bit 5: report it as an interrupt rather than only in
// the status register.
const SLOT_CONTROL_PRESENCE_CHANGED_ENABLE: u16 = 1 << 3;
const SLOT_CONTROL_HOT_PLUG_INTERRUPT_ENABLE: u16 = 1 << 5;
// Whether the slot HAS a power controller, and the control bit that turns it on.
//
// A SLOT WHOSE POWER IS OFF REPORTS NOTHING, which is the whole reason this is here. `Power
// Controller Control` is a one for OFF and a zero for ON - the inverted spelling is the
// specification's - and a slot with a power controller comes out of reset with it OFF. A guest that
// armed the presence-change interrupt and left the power off armed a slot that will never see a
// device: the port holds it unpowered, so nothing behind it is presented and no change is reported.
// A person plugging a disk into that machine watches nothing happen.
const SLOT_CAP_POWER_CONTROLLER: u32 = 1 << 1;
const SLOT_CONTROL_POWER_OFF: u16 = 1 << 10;
// The Power Indicator's two bits, and the value that means ON. An indicator left at its reset value
// says "the slot's state is unknown", which is what an operator's light would show.
const SLOT_CONTROL_POWER_INDICATOR: u16 = 3 << 8;
const SLOT_CONTROL_POWER_INDICATOR_ON: u16 = 1 << 8;
// AND OFF IS THREE AND NOT ZERO, which is the specification's spelling and not an obvious one. The
// two bits encode on, blink and off as 01, 10 and 11; ZERO IS RESERVED. A port reads the indicator
// as part of deciding that the guest has finished with the slot, so writing the reserved value is a
// power-down request a port does not recognise - the slot goes dark and the device stays in it.
const SLOT_CONTROL_POWER_INDICATOR_OFF: u16 = 3 << 8;

/// A hot-plug slot on a port: where its registers are, and which physical slot it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Slot {
	/// The config-space offset of the PCI Express capability the slot registers hang off.
	pub cap: u16,
	/// The physical slot number the platform assigned, out of Slot Capabilities bits 19..31. It is
	/// what a person reads off a chassis, and it is not the bus/device/function of anything.
	pub number: u32,
}

/// What a slot's status says has happened to it.
///
/// TWO BITS AND TWO QUESTIONS, WHICH IS THE WHOLE OF WHY THIS IS A TYPE. `Presence Detect State`
/// says what is in the slot NOW; `Presence Detect Changed` is a STICKY bit saying it changed since
/// somebody last cleared it. A reader that watches only the state never learns that a device was
/// swapped between two looks - the state is the same before and after - and one that acts on the
/// changed bit alone knows something happened and not WHICH of the two things it was. The pair
/// answers it; either alone does not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotEvent {
	/// Nothing has changed since the last acknowledgement.
	Quiet,
	/// A device is in the slot and the change bit says it arrived.
	Arrived,
	/// The slot is empty and the change bit says it left.
	Departed,
	/// Somebody asked for the device in this slot to be removed, and it is still there.
	///
	/// THE REQUEST IS NOT THE REMOVAL. What this reports is the beginning of a coordinated removal:
	/// the driver has to be stopped and its resources released, and only then may the slot be powered
	/// down - which is what lets the port take the device out.
	RemovalRequested,
}

/// Resolve a port's hot-plug slot, or `None` when it has none.
///
/// A CAPABILITY EVERY PCIe FUNCTION HAS, AND SLOT REGISTERS ONLY SOME OF THEM DO. The PCI Express
/// capability sits on endpoints too, at the same offsets, so reading Slot Status off whatever
/// carries the capability reads an endpoint's reserved bytes and calls them a presence bit. Two
/// things have to be true first: the port type is a root port or a switch's downstream port, and
/// `Slot Implemented` is set. Then the slot has to be HOT-PLUG CAPABLE, because one that is not will
/// never report a change however long anything watches it.
pub fn resolve_slot<A: ConfigAccess>(d: &PciDevice) -> Option<Slot> {
	if A::read16(d.bus, d.dev, d.func, 0x06) & STATUS_CAP_LIST == 0 {
		return None;
	}
	let mut ptr: u16 = (A::read8(d.bus, d.dev, d.func, 0x34) & 0xFC) as u16;
	// Bounded like every other walk here: a malformed list must not spin.
	for _ in 0..48 {
		if ptr == 0 {
			break;
		}
		let cap_id = A::read8(d.bus, d.dev, d.func, ptr);
		let next = (A::read8(d.bus, d.dev, d.func, ptr + 1) & 0xFC) as u16;
		if cap_id == PCIE_CAP_ID {
			let caps = A::read16(d.bus, d.dev, d.func, ptr + 2);
			let port_type = (caps >> 4) & 0x0F;
			if (port_type != PCIE_TYPE_ROOT_PORT && port_type != PCIE_TYPE_DOWNSTREAM_PORT) || caps & PCIE_SLOT_IMPLEMENTED == 0 {
				return None;
			}
			let slot_caps = A::read32(d.bus, d.dev, d.func, ptr + 0x14);
			if slot_caps & SLOT_CAP_HOT_PLUG == 0 {
				return None;
			}
			return Some(Slot { cap: ptr, number: slot_caps >> 19 });
		}
		ptr = next;
	}
	None
}

/// What this slot says has happened, WITHOUT changing anything.
pub fn slot_event<A: ConfigAccess>(d: &PciDevice, slot: Slot) -> SlotEvent {
	let status = A::read16(d.bus, d.dev, d.func, slot.cap + 0x1A);
	// THE BUTTON IS READ BEFORE THE PRESENCE, because it is the earlier half of the same story: a
	// removal is REQUESTED and then, once the system has let go, it happens. A reader that looked at
	// presence first would answer `Quiet` for the request and only ever see the removal it was
	// supposed to prepare for.
	if status & SLOT_STATUS_ATTENTION_BUTTON != 0 {
		return SlotEvent::RemovalRequested;
	}
	if status & SLOT_STATUS_PRESENCE_CHANGED == 0 {
		return SlotEvent::Quiet;
	}
	if status & SLOT_STATUS_PRESENT != 0 { SlotEvent::Arrived } else { SlotEvent::Departed }
}

/// Power the slot down, which is how a guest says the removal may proceed.
///
/// THIS IS THE ACKNOWLEDGEMENT THAT MATTERS. The button said "may I", the driver has stopped and its
/// claim is free, and this is the answer: the port may now take the device out. Writing it before the
/// driver let go is the surprise removal this whole path exists to avoid.
pub fn slot_power_off<A: ConfigAccess>(d: &PciDevice, slot: Slot) {
	let mut control = A::read16(d.bus, d.dev, d.func, slot.cap + 0x18);
	control |= SLOT_CONTROL_POWER_OFF;
	control = (control & !SLOT_CONTROL_POWER_INDICATOR) | SLOT_CONTROL_POWER_INDICATOR_OFF;
	A::write32(d.bus, d.dev, d.func, slot.cap + 0x18, control as u32);
}

/// Power the slot back up, so the next device plugged into it is seen.
pub fn slot_power_on<A: ConfigAccess>(d: &PciDevice, slot: Slot) {
	let mut control = A::read16(d.bus, d.dev, d.func, slot.cap + 0x18);
	control &= !SLOT_CONTROL_POWER_OFF;
	control = (control & !SLOT_CONTROL_POWER_INDICATOR) | SLOT_CONTROL_POWER_INDICATOR_ON;
	A::write32(d.bus, d.dev, d.func, slot.cap + 0x18, control as u32);
}

/// Acknowledge the change this slot reported, so the next one is a new one.
///
/// WRITTEN AS A ONE AND NOT A ZERO. `Presence Detect Changed` is RW1C - write-one-to-clear - which
/// is the opposite of what clearing a bit usually looks like: a reader that masks it out and writes
/// the result back leaves the bit exactly as it was, and every later look reports the same change
/// for ever. That is an event storm from one plug, and it reads as a device arriving thousands of
/// times rather than as a bug in the acknowledgement.
///
/// AND THE OTHER STICKY BITS ARE NOT TOUCHED. Writing a one to a bit CLEARS it, so a write-back of
/// the whole status register would acknowledge every event the slot had recorded, including the ones
/// this caller never read.
pub fn slot_acknowledge<A: ConfigAccess>(d: &PciDevice, slot: Slot) {
	// ONE DWORD, WRITTEN DELIBERATELY, because Slot Control and Slot Status share it and the two
	// halves do not obey the same rule. The control half is ordinary and is written back as it was;
	// the status half is RW1C and carries EXACTLY the bit being acknowledged, so every other sticky
	// event the slot has recorded survives. A read-modify-write of the whole dword would write the
	// status bits back as ones and acknowledge all of them, including ones nobody read.
	let control = A::read16(d.bus, d.dev, d.func, slot.cap + 0x18) as u32;
	A::write32(d.bus, d.dev, d.func, slot.cap + 0x18, control | ((SLOT_STATUS_PRESENCE_CHANGED as u32) << 16));
}

/// Acknowledge the attention button, which is a separate sticky bit from the presence change.
pub fn slot_acknowledge_button<A: ConfigAccess>(d: &PciDevice, slot: Slot) {
	let control = A::read16(d.bus, d.dev, d.func, slot.cap + 0x18) as u32;
	A::write32(d.bus, d.dev, d.func, slot.cap + 0x18, control | ((SLOT_STATUS_ATTENTION_BUTTON as u32) << 16));
}

/// Ask the slot to report presence changes, as an interrupt as well as in its status, and TURN IT ON.
///
/// THE POWER IS THE HALF THAT IS EASY TO LEAVE OUT AND IMPOSSIBLE TO NOTICE. A slot with a power
/// controller comes out of reset with the power OFF, and a port holding an unpowered slot presents
/// nothing behind it: the presence bit stays clear, the change bit never sets, and the interrupt
/// this function just armed never fires. Everything looks correct and nothing ever happens - which
/// from an operator's side is a machine that ignores the disk they plugged into it.
pub fn slot_arm<A: ConfigAccess>(d: &PciDevice, slot: Slot) {
	let has_power_controller = A::read32(d.bus, d.dev, d.func, slot.cap + 0x14) & SLOT_CAP_POWER_CONTROLLER != 0;
	// AND THE STATUS HALF IS WRITTEN AS ZERO, which is how "change nothing" is spelled in an RW1C
	// register. Writing back what was read there would clear every event the slot had recorded -
	// this function's business is the control half, and the other half of the dword it has to write
	// is not its to acknowledge.
	let mut control = A::read16(d.bus, d.dev, d.func, slot.cap + 0x18);
	control |= SLOT_CONTROL_PRESENCE_CHANGED_ENABLE | SLOT_CONTROL_HOT_PLUG_INTERRUPT_ENABLE | SLOT_CONTROL_ATTENTION_BUTTON_ENABLE;
	if has_power_controller {
		// A ZERO IS ON. See `SLOT_CONTROL_POWER_OFF`.
		control &= !SLOT_CONTROL_POWER_OFF;
		control = (control & !SLOT_CONTROL_POWER_INDICATOR) | SLOT_CONTROL_POWER_INDICATOR_ON;
	}
	A::write32(d.bus, d.dev, d.func, slot.cap + 0x18, control as u32);
}

// virtio capability cfg_type values (which structure the capability points at).
const VIRTIO_CAP_COMMON: u8 = 1;
const VIRTIO_CAP_NOTIFY: u8 = 2;
const VIRTIO_CAP_ISR: u8 = 3;
const VIRTIO_CAP_DEVICE: u8 = 4;

// The command-register bits (config offset 0x04, low 16).
#[cfg(target_arch = "x86_64")]
const CMD_IO_SPACE: u16 = 1 << 0;
const CMD_MEMORY_SPACE: u16 = 1 << 1;
const CMD_BUS_MASTER: u16 = 1 << 2;
const CMD_INTX_DISABLE: u16 = 1 << 10;
// MSI-X Message Control bits (config offset cap+2, upper 16 of the dword at `cap`).
const MSIX_ENABLE: u16 = 1 << 15;
const MSIX_FUNCTION_MASK: u16 = 1 << 14;

// The arch-specific config-space access mechanism. A backend implements the two
// primitives (`read32` / `write32`) and, where relevant, the BAR allocator; the
// derived byte / word reads and every enumeration routine below are portable.
pub trait ConfigAccess {
	// How many ROOT buses to sweep: legacy x86 CAM starts at bus 0 alone; an ECAM window
	// exposes several (16 on QEMU `virt`). Buses behind a bridge are reached by the walk in `scan`
	// rather than by this count.
	const BUS_COUNT: u16;

	// The highest bus number this access mechanism can address, which is NOT the same question.
	// CF8/CFC carries eight bits of bus and reaches all 256 of them from one root; an ECAM window
	// is a fixed span of memory and reaches exactly as many buses as it is wide - a read past it is
	// a read of whatever the map holds next. So a bridge that claims to forward buses beyond this
	// is not followed: the walk stops where the mechanism does.
	const MAX_BUS: u8 = 255;

	// The end of the low 32-bit MMIO window that `assign_bars` reassigns BARs into,
	// used to decide whether a firmware-placed BAR sits outside the mapped window.
	// Only consulted by ECAM platforms that override `assign_bars`; 0 otherwise.
	const MMIO_WINDOW_END: u64 = 0;

	// Whether this mechanism can address EXTENDED config space - offsets at or past 0x100.
	//
	// AN ECAM WINDOW PUTS THE REGISTER IN THE ADDRESS and has twelve bits of it, so it reaches the
	// whole four-kilobyte config space of a function; the legacy port pair carries eight bits of
	// register number and cannot reach past 0xFF at all. Every PCIe extended capability - Advanced
	// Error Reporting first among them - lives past that line, so a walk has to ask before it starts:
	// on a mechanism that cannot reach them, a read at 0x100 answers the register at 0x00, and a walk
	// would report the vendor id as a capability header.
	fn extended_reach() -> bool {
		true
	}

	// Read / write a 32-bit config-space dword for one bus/device/function.
	fn read32(bus: u8, dev: u8, func: u8, off: u16) -> u32;
	fn write32(bus: u8, dev: u8, func: u8, off: u16, val: u32);

	// Run `f` with the backend's config access serialised, if it serialises at all.
	//
	// The x86 CF8/CFC pair is a two-register protocol - an address write then a data transfer - so
	// its backend takes a lock per ACCESS. That closes the interleaving of two accesses and does
	// nothing for the operations built on them: a read-modify-write of the command register is two
	// separately-locked accesses, and a concurrent writer between them loses one of the two updates.
	// No such writer exists today (the INTx disable happens during the serial boot scan, and MSI-X
	// enable is DeviceManager's, one device at a time), so this is hardening - recorded because the
	// lock is there and looks as though it covers this.
	//
	// The backend's own lock is not reentrant, so everything inside `f` must use the `_raw` forms.
	fn with_config<R>(f: impl FnOnce() -> R) -> R {
		f()
	}

	// The raw accesses, WITHOUT the backend's serialisation: only for use inside `with_config`,
	// which is holding it. A backend that does not serialise leaves these as they are.
	fn read32_raw(bus: u8, dev: u8, func: u8, off: u16) -> u32 {
		Self::read32(bus, dev, func, off)
	}

	fn write32_raw(bus: u8, dev: u8, func: u8, off: u16, val: u32) {
		Self::write32(bus, dev, func, off, val);
	}

	// Read-modify-write a config dword as ONE operation, so a future concurrent writer cannot land
	// between the halves. Every read-modify-write of config space goes through here.
	fn update32(bus: u8, dev: u8, func: u8, off: u16, f: impl FnOnce(u32) -> u32) {
		Self::with_config(|| {
			let value = Self::read32_raw(bus, dev, func, off);
			Self::write32_raw(bus, dev, func, off, f(value));
		});
	}

	// A 16-bit config field, extracted from its enclosing dword. Works for both an
	// ECAM MMIO window and legacy port CAM (which only reads whole dwords), so no
	// backend needs its own sub-dword read.
	fn read16(bus: u8, dev: u8, func: u8, off: u16) -> u16 {
		(Self::read32(bus, dev, func, off & !3) >> ((off as u32 & 2) * 8)) as u16
	}

	// An 8-bit config field, extracted from its enclosing dword.
	fn read8(bus: u8, dev: u8, func: u8, off: u16) -> u8 {
		(Self::read32(bus, dev, func, off & !3) >> ((off as u32 & 3) * 8)) as u8
	}

	// Whether the config space is reachable yet (an ECAM backend gates this on the
	// ECAM base being discovered in the device tree). Default: always ready.
	fn ready() -> bool {
		true
	}

	// Assign a device's memory BARs. A no-op where firmware (or QEMU on x86) already
	// programmed them; ECAM platforms with no firmware override this to call
	// `assign_bars_ecam::<Self>` so the BARs land in the kernel-mapped low window.
	fn assign_bars(_d: &PciDevice) {}

	// Allocate a size-aligned span from the platform's low MMIO window, or None when
	// exhausted. Provided by ECAM platforms whose `assign_bars` reprograms BARs.
	fn alloc_mmio(_size: u64) -> Option<u64> {
		None
	}

	// Take a span the FIRMWARE already placed out of the window, before anything is allocated
	// from it (KERN-ARCH-015). A retained BAR was left where it was and never told to the
	// allocator, so the next unprogrammed BAR could be handed the same addresses - two devices
	// decoding one aperture, which is not a fault either of them can report.
	//
	// Provided by the same ECAM platforms as `alloc_mmio`; a backend that allocates nothing has
	// nothing to reserve.
	fn reserve_mmio(_base: u64, _size: u64) {}
}

// One discovered PCI function.
#[derive(Clone, Copy)]
pub struct PciDevice {
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
	pub vendor: u16,
	pub device_id: u16,
	pub class: u8,
	pub subclass: u8,
	pub prog_if: u8,
	pub header_type: u8,
	// The six 32-bit base address registers (raw; only meaningful for header type 0).
}

impl PciDevice {
	// Whether this is a virtio device.
	pub fn is_virtio(&self) -> bool {
		self.vendor == VIRTIO_VENDOR
	}

	// The device type this kernel resolves a register window under, or `None` for a function it
	// classifies but resources nothing for. ONE QUESTION RATHER THAN ONE PER FAMILY: an `is_xhci`
	// beside an `is_nvme` beside an `is_ahci` is a list every caller has to keep up with, and the
	// table above is the list.
	pub fn resourced_type(&self) -> Option<(u32, usize)> {
		if let Some(&(_, _, device_type, bar)) = RESOURCED_BY_ID.iter().find(|(vendor, device, _, _)| self.vendor == *vendor && self.device_id == *device) {
			return Some((device_type, bar));
		}
		RESOURCED.iter().find(|(class, subclass, prog_if, _, _)| self.class == *class && self.subclass == *subclass && self.prog_if == *prog_if).map(|(_, _, _, device_type, bar)| (*device_type, *bar))
	}

	// The virtio device type. Modern ids encode it as device_id - 0x1040; the
	// transitional ids QEMU can expose (0x1000 net, 0x1001 block, ...) map to the
	// same type numbers so the modern capability path still applies. None if this is
	// not a virtio device.
	pub fn virtio_type(&self) -> Option<u16> {
		if !self.is_virtio() {
			return None;
		}
		if (VIRTIO_MODERN_BASE..VIRTIO_MODERN_BASE + 0x40).contains(&self.device_id) {
			return Some(self.device_id - VIRTIO_MODERN_BASE);
		}
		match self.device_id {
			0x1000 => Some(abi::VIRTIO_TYPE_NET as u16),
			0x1001 => Some(abi::VIRTIO_TYPE_BLOCK as u16),
			0x1003 => Some(abi::VIRTIO_TYPE_CONSOLE as u16),
			// QEMU presents `virtio-scsi-pci` as TRANSITIONAL by default, so the id a machine
			// actually shows is this one rather than the modern 0x1048 - which is the whole reason
			// this table exists beside the modern range above.
			0x1004 => Some(abi::VIRTIO_TYPE_SCSI as u16),
			0x1005 => Some(abi::VIRTIO_TYPE_RNG as u16),
			_ => None,
		}
	}
}

// One located virtio configuration structure (resolved from a virtio PCI cap):
// which BAR it lives in, its byte offset within that BAR, and its length.
#[derive(Clone, Copy, Default)]
pub struct VirtioCap {
	pub bar: u8,
	pub offset: u32,
	pub length: u32,
	// For the notify capability only: the queue_notify_off multiplier.
	pub notify_multiplier: u32,
}

// A modern virtio-pci device with its MMIO layout resolved from its capabilities.
// `bar_phys`/`region_len` describe the physical MMIO window a driver maps (the BAR
// the common-config structure lives in); the per-structure offsets index into it.
#[derive(Clone, Copy)]
pub struct VirtioDevice {
	pub pci: PciDevice,
	pub virtio_type: u16,
	// The BAR all three configuration structures share. Checked from a local while the device is
	// resolved; kept on the device only for the boot print that names it.
	#[cfg(any(test, target_arch = "aarch64"))]
	pub bar: u8,
	pub bar_phys: u64,
	pub region_len: u64,
	pub common: VirtioCap,
	pub notify: VirtioCap,
	pub isr: VirtioCap,
	// OPTIONAL, AND SAID SO (KERN-ARCH-014). It used to be a `VirtioCap::default()` when the device
	// had none - offset zero, length zero - which is indistinguishable from a real structure at the
	// start of the window.
	pub device: Option<VirtioCap>,
	// MSI-X (when present): the config-space offset of the MSI-X capability (0 = none),
	// the number of table entries, and the physical address of the MSI-X table. The
	// kernel programs table entry 0 and enables MSI-X for an interrupt-driven driver.
	pub msix_cap: u16,
	pub msix_table_phys: u64,
}

// An xHCI USB host controller with its MMIO window resolved: the physical base and
// probed size of BAR 0 (the capability registers start at its base; the operational,
// runtime, and doorbell registers follow at offsets the driver reads from them), plus
// its MSI-X capability for a per-device interrupt vector.
#[derive(Clone, Copy)]
pub struct ResourcedDevice {
	pub pci: PciDevice,
	// Which family the class triple resolved to. It used to be implied by the struct's name, which
	// worked while exactly one family had a resolver.
	pub device_type: u32,
	pub bar_phys: u64,
	pub bar_len: u64,
	pub msix_cap: u16,
	pub msix_table_phys: u64,
}

// The identity of one function, or `None` where nothing is there.
//
// The public form of `read_function`, for a caller that has a bus address and needs the device
// behind it - the IOMMU fixture, which works with a function this kernel binds no driver to, and
// the bypass transition, which resolves an NVMe controller's registers to quiesce it (P02M0173).
pub fn probe_function<A: ConfigAccess>(bus: u8, dev: u8, func: u8) -> Option<PciDevice> {
	let vendor = A::read16(bus, dev, func, 0x00);
	if vendor == 0xFFFF || vendor == 0 {
		return None;
	}
	Some(read_function::<A>(bus, dev, func))
}

// Read the full identity of one present function.
fn read_function<A: ConfigAccess>(bus: u8, dev: u8, func: u8) -> PciDevice {
	PciDevice { bus, dev, func, vendor: A::read16(bus, dev, func, 0x00), device_id: A::read16(bus, dev, func, 0x02), class: A::read8(bus, dev, func, 0x0b), subclass: A::read8(bus, dev, func, 0x0a), prog_if: A::read8(bus, dev, func, 0x09), header_type: A::read8(bus, dev, func, 0x0e) }
}

// A PCI-to-PCI bridge's header type (bits 0-6 of the header-type byte), and the config offset of
// its secondary bus number - the bus its downstream side is numbered as.
const HEADER_TYPE_BRIDGE: u8 = 0x01;
const BRIDGE_SECONDARY_BUS: u16 = 0x19;
const BRIDGE_SUBORDINATE_BUS: u16 = 0x1a;

// Enumerate every present function reachable from the backend's root buses, FOLLOWING BRIDGES.
// Multi-function devices (header-type bit 7) have all eight functions probed; absent slots (vendor
// 0xFFFF) are skipped.
//
// A flat loop over `BUS_COUNT` finds only what firmware happened to put on a root bus, so on x86
// (`BUS_COUNT = 1`) anything behind a PCIe root port or a bridge did not exist - not "was not
// driven", did not exist: it was never read. Every backend gets the walk, because a bus number is
// only meaningful relative to the bridge that forwards it; a linear sweep of 256 buses happens to
// find the same devices on ECAM, and happens to is not a rule.
//
// BOUNDED THREE WAYS, because bridge numbering comes from firmware and firmware is an input here.
// A bus is visited at most once (`seen`), a bridge never descends to a bus at or below its own
// (which is what a cycle would have to do), and the recursion carries an explicit depth limit -
// deeper than any real topology and shallower than the kernel stack.
pub fn scan<A: ConfigAccess>() -> Vec<PciDevice> {
	let mut out: Vec<PciDevice> = Vec::new();
	if !A::ready() {
		return out;
	}
	let mut seen = [false; 256];
	for bus in 0..A::BUS_COUNT.min(A::MAX_BUS as u16 + 1) {
		scan_bus::<A>(bus as u8, &mut seen, &mut out, 0);
	}
	arm_hot_plug_slots::<A>(&out);
	note_error_reporters::<A>(&out);
	out
}

// Which functions of this machine can report an error, remembered once so the poll costs nothing on
// the ones that cannot.
//
// SAID ONCE, LIKE THE SLOTS, and for the same reason: a machine that reports errors at all is worth
// knowing about before one happens, and a machine that reports none is worth knowing about too - it
// means the silence later is the absence of a reporter and not the absence of a fault.
fn note_error_reporters<A: ConfigAccess>(devices: &[PciDevice]) {
	let first: bool = !REPORTERS_SAID.swap(true, core::sync::atomic::Ordering::AcqRel);
	let mut found = 0;
	for function in devices.iter().filter(|function| controls(function.bus, OSC_AER)) {
		if note_error_reporter::<A>(function.bus, function.dev, function.func) {
			found += 1;
			if first {
				crate::serial_println!("pci: {:02x}:{:02x}.{} reports errors", function.bus, function.dev, function.func);
			}
		}
	}
	if first && found == 0 {
		crate::serial_println!("pci: no function on this machine reports errors{}", if A::extended_reach() { "" } else { " - extended config space is unreachable here" });
	}
}

// Whether the error reporters have been said. The bus is scanned several times in a boot.
static REPORTERS_SAID: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// ON AN ACPI MACHINE, NATIVE CONTROL IS WHAT `_OSC` GRANTED. The firmware owns a host bridge's hot-plug slots and its
// error reporting until its `_OSC` hands them over - a slot armed natively while the firmware also drives it through a
// GPE is two owners of one register - so on such a machine the scan arms a slot and watches an error reporter only on
// a bus a grant covers, and a bridge without `_OSC` grants nothing. A device-tree machine, and the test kernel, keep
// native control as always.
static OSC_GATED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// The grants: (first bus, last bus, the control bits granted). One per host bridge.
const MAX_OSC_GRANTS: usize = 8;
static OSC_GRANTS: SpinLock<([(u8, u8, u32); MAX_OSC_GRANTS], usize)> = SpinLock::new(([(0, 0, 0); MAX_OSC_GRANTS], 0));

// `_OSC`'s control bits this file acts on.
pub const OSC_HOT_PLUG: u32 = 1 << 0;
pub const OSC_AER: u32 = 1 << 3;

// Wait for the firmware's grant before arming anything native. Called before the first scan on an ACPI machine.
pub fn gate_on_osc() {
	OSC_GATED.store(true, core::sync::atomic::Ordering::Release);
	crate::serial_println!("pci: native hot-plug and error reporting wait for the firmware's _OSC grant");
}

// RECORD ONE HOST BRIDGE'S GRANT, answering whether it changed what this kernel controls - the caller scans again
// and arms what it now may. The slot and reporter lines are said again for what the grant brought.
pub fn apply_grant(bus_start: u8, bus_end: u8, granted: u32) -> bool {
	let mut grants = OSC_GRANTS.lock();
	let count = grants.1;
	if grants.0[..count].contains(&(bus_start, bus_end, granted)) {
		return false;
	}
	if let Some(slot) = grants.0[..count].iter_mut().find(|grant| grant.0 == bus_start && grant.1 == bus_end) {
		*slot = (bus_start, bus_end, granted);
	} else if count < MAX_OSC_GRANTS {
		grants.0[count] = (bus_start, bus_end, granted);
		grants.1 += 1;
	} else {
		crate::serial_println!("pci: more than {MAX_OSC_GRANTS} host bridges answered _OSC - buses {bus_start:#04x}..{bus_end:#04x} keep firmware control");
		return false;
	}
	drop(grants);
	for (bit, what) in [(OSC_HOT_PLUG, "hot-plug"), (OSC_AER, "error reporting")] {
		if granted & bit == 0 {
			crate::serial_println!("pci: the firmware keeps {what} control of buses {bus_start:#04x}..{bus_end:#04x} (_OSC) - left unarmed");
		} else {
			crate::serial_println!("pci: _OSC grants native {what} on buses {bus_start:#04x}..{bus_end:#04x}");
		}
	}
	SLOTS_REPORTED.store(false, core::sync::atomic::Ordering::Release);
	REPORTERS_SAID.store(false, core::sync::atomic::Ordering::Release);
	true
}

// Whether this kernel controls `bit` for a function on `bus`.
fn controls(bus: u8, bit: u32) -> bool {
	if !OSC_GATED.load(core::sync::atomic::Ordering::Acquire) {
		return true;
	}
	let grants = OSC_GRANTS.lock();
	grants.0[..grants.1].iter().any(|&(start, end, granted)| (start..=end).contains(&bus) && granted & bit != 0)
}

// Whether the slots have been reported. The bus is scanned SEVERAL TIMES in a boot - the device
// table, the virtio pass, and whatever asks later - and a machine does not have three hot-plug slots
// because three passes found the same one.
static SLOTS_REPORTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// Arm every hot-plug slot this scan found, and say what is in each - once per boot.
//
// ARMED BECAUSE A SLOT NOBODY ASKED TO BE TOLD ABOUT REPORTS NOTHING, however long anything watches
// it. SAID because an empty slot is invisible everywhere else: an operator who plugs a disk into one
// needs to know the system was ever going to notice.
//
// AND ANY CHANGE ALREADY STANDING IS ACKNOWLEDGED, because it is about whatever happened before this
// system booted. Left set, the first look after boot reports an arrival that is only the machine's
// starting shape.
/// The hot-plug ports this machine has, remembered at the scan that found them.
///
/// REMEMBERED AND NOT RESCANNED. A slot has to be READ to know what is in it, and the read has to
/// happen whenever something might have changed - from an interrupt handler, among other places. A
/// full bus walk there would allocate, would follow every bridge, and would do it while a device is
/// half plugged in; what is needed is the handful of ports that carry a slot at all, which is what
/// the scan already found.
///
/// A FIXED ARRAY BECAUSE THIS IS READ FROM AN INTERRUPT. Eight is more root ports than any machine
/// this kernel boots on has, and a ninth is reported rather than silently ignored.
pub const MAX_HOT_PLUG_PORTS: usize = 8;

#[derive(Clone, Copy)]
pub struct HotPlugPort {
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
	pub slot: Slot,
	/// What was in the slot the last time this was read, so a poll can tell a CHANGE from a state.
	pub occupied: bool,
}

static PORTS: SpinLock<([Option<HotPlugPort>; MAX_HOT_PLUG_PORTS], usize)> = SpinLock::new(([None; MAX_HOT_PLUG_PORTS], 0));

/// Take the port table away and give it back, for a test that drives the slot machinery over a
/// synthetic bus.
///
/// THIS IS REAL STATE AND THE TESTS WERE WRITING IT. `arm_hot_plug_slots` and `poll_slots` work
/// through this static, so an in-guest test standing up a fake port left the RUNNING kernel holding
/// it: the idle pass then polled a port at `00:00.0` with slot number zero that no machine had, and
/// anything asking what hot-plug ports this machine has was answered with the fixture. It was found
/// by an oracle that asks exactly that and failed on aarch64 and not on x86_64 - the same tests, a
/// different order.
///
/// A GUARD RATHER THAN A RULE, because a restore somebody has to remember is the defect again. The
/// value comes back when the guard drops, on every path out of the test including a panic.
#[cfg(test)]
pub(crate) struct HeldPorts(([Option<HotPlugPort>; MAX_HOT_PLUG_PORTS], usize), bool);

#[cfg(test)]
impl HeldPorts {
	pub(crate) fn take() -> HeldPorts {
		HeldPorts(*PORTS.lock(), SLOTS_REPORTED.load(core::sync::atomic::Ordering::Acquire))
	}
}

#[cfg(test)]
impl Drop for HeldPorts {
	fn drop(&mut self) {
		*PORTS.lock() = self.0;
		// AND THE "HAVE THE SLOTS BEEN REPORTED" FLAG WITH IT, which `arm_hot_plug_slots` swaps: a
		// test that left it set would silence the boot line on a later real scan, and one that left
		// it clear would print the machine's slots a second time in the middle of a suite.
		SLOTS_REPORTED.store(self.1, core::sync::atomic::Ordering::Release);
	}
}

/// What a poll of the slots found, for a caller that acts on it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SlotChange {
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
	/// The bus behind the port, which is where an arrival appears.
	pub secondary: u8,
	pub what: SlotEvent,
}

/// Read every hot-plug slot, acknowledge what it reported, and answer what CHANGED.
///
/// THE CHANGE BIT AND THE STATE ARE BOTH READ, because neither is enough. The sticky change bit says
/// something happened and not which of the two it was; the state says what is there NOW and cannot
/// tell a swap from a silence. What this answers is the state at the moment of a change, against the
/// state at the last one - so a device removed and replaced between two polls is one arrival, which
/// is what the machine looks like from here.
pub fn poll_slots<A: ConfigAccess>(out: &mut [SlotChange; MAX_HOT_PLUG_PORTS]) -> usize {
	let mut found = 0;
	let mut ports = PORTS.lock();
	let count = ports.1;
	for index in 0..count {
		let Some(port) = ports.0[index] else { continue };
		let function = PciDevice { bus: port.bus, dev: port.dev, func: port.func, vendor: 0, device_id: 0, class: 0, subclass: 0, prog_if: 0, header_type: 0 };
		let event = slot_event::<A>(&function, port.slot);
		match event {
			SlotEvent::Quiet => continue,
			SlotEvent::RemovalRequested => {
				// ACKNOWLEDGED SO IT IS ASKED ONCE. The button is sticky like every other slot event,
				// and a request read on every poll is a driver asked to stop a hundred times a
				// second.
				slot_acknowledge_button::<A>(&function, port.slot);
				if !port.occupied {
					// A BUTTON ON AN EMPTY SLOT is somebody asking for a device that is not there.
					continue;
				}
			}
			SlotEvent::Arrived | SlotEvent::Departed => {
				slot_acknowledge::<A>(&function, port.slot);
				let arrived = event == SlotEvent::Arrived;
				if arrived == port.occupied {
					// THE STATE DID NOT MOVE. A change bit with the same state on both sides of it is
					// a device that came and went between two reads, and there is nothing for a
					// caller to do about a device that is not there now.
					continue;
				}
				ports.0[index] = Some(HotPlugPort { occupied: arrived, ..port });
			}
		}
		out[found] = SlotChange { bus: port.bus, dev: port.dev, func: port.func, secondary: A::read8(port.bus, port.dev, port.func, BRIDGE_SECONDARY_BUS), what: event };
		found += 1;
	}
	found
}

/// The legacy interrupt PIN a hot-plug port asserts on: 1..=4 for INTA..INTD, `None` for a function
/// that asserts no legacy line at all.
///
/// THE HALF EVERY BACKEND NEEDS, and the reason it is its own function. The LINE beside it in the
/// same register is a number x86 firmware wrote there when it routed the function; a device-tree
/// machine's firmware writes nothing into it, and those boards put their routing in the host
/// bridge's `interrupt-map` instead - which the PIN is what indexes. So all three ports read this
/// one, and only x86_64 goes on to read the line below.
pub fn slot_interrupt_pin<A: ConfigAccess>(port: &HotPlugPort) -> Option<u8> {
	let pin = (A::read32(port.bus, port.dev, port.func, 0x3c) >> 8) as u8;
	(1..=4).contains(&pin).then_some(pin)
}

/// The legacy interrupt line a hot-plug port asserts on, or `None` where it has none.
///
/// THE LINE AND THE PIN ARE TWO REGISTERS AND BOTH MATTER. A function with no interrupt PIN asserts
/// nothing whatever its line says, and firmware leaves the line at `0xff` for a function it routed
/// nowhere - both are "this port will not tell you", and a handler registered on either would be a
/// handler on a vector nothing raises.
// COMPILED WHERE IT IS READ FROM, WHICH IS THE PORT WHOSE FIRMWARE WRITES IT. A BIOS or a UEFI puts
// the routed line in this byte; the two ports that boot from a device tree are handed a byte nobody
// wrote, and they route through `interrupt-map` and the pin above instead. Standing on all three
// backends as a wrapper nobody called is what made this surface look portable while one port could
// use it, and it cost a dead-code suppression on each of the other two.
#[cfg(any(target_arch = "x86_64", test))]
pub fn slot_interrupt_line<A: ConfigAccess>(port: &HotPlugPort) -> Option<u8> {
	let line = A::read32(port.bus, port.dev, port.func, 0x3c) as u8;
	(slot_interrupt_pin::<A>(port).is_some() && line != 0xff).then_some(line)
}

/// Power the slot behind one named port off or on, for a caller that has coordinated a removal.
pub fn set_slot_power<A: ConfigAccess>(bus: u8, dev: u8, func: u8, on: bool) {
	let ports = PORTS.lock();
	for port in ports.0.iter().take(ports.1).flatten() {
		if port.bus != bus || port.dev != dev || port.func != func {
			continue;
		}
		let function = PciDevice { bus, dev, func, vendor: 0, device_id: 0, class: 0, subclass: 0, prog_if: 0, header_type: 0 };
		if on {
			slot_power_on::<A>(&function, port.slot);
		} else {
			slot_power_off::<A>(&function, port.slot);
		}
		return;
	}
}

/// Every hot-plug port this machine has, for a caller that binds their interrupts.
///
/// Filled by the scan on every backend, and read by every backend's arming pass: x86_64 routes each
/// port's line through an I/O APIC, and the two device-tree ports resolve the same port's pin
/// through the host bridge's `interrupt-map` and arm their own controller with it.
pub fn hot_plug_ports(out: &mut [Option<HotPlugPort>; MAX_HOT_PLUG_PORTS]) -> usize {
	let ports = PORTS.lock();
	*out = ports.0;
	ports.1
}

fn arm_hot_plug_slots<A: ConfigAccess>(devices: &[PciDevice]) {
	let first: bool = !SLOTS_REPORTED.swap(true, core::sync::atomic::Ordering::AcqRel);
	let mut ports = PORTS.lock();
	ports.1 = 0;
	for function in devices.iter().filter(|function| controls(function.bus, OSC_HOT_PLUG)) {
		let Some(slot) = resolve_slot::<A>(function) else { continue };
		slot_arm::<A>(function, slot);
		slot_acknowledge::<A>(function, slot);
		let occupied = slot_event::<A>(function, slot) == SlotEvent::Arrived;
		// REMEMBERED HERE AND NOWHERE ELSE, so a later read costs no bus walk. A ninth port is
		// reported rather than dropped: a machine with more hot-plug ports than this array holds is
		// a machine this kernel would watch part of, which is worse than one it says it cannot.
		if ports.1 < MAX_HOT_PLUG_PORTS {
			let at = ports.1;
			ports.0[at] = Some(HotPlugPort { bus: function.bus, dev: function.dev, func: function.func, slot, occupied });
			ports.1 += 1;
		} else if first {
			crate::serial_println!("pci: more than {MAX_HOT_PLUG_PORTS} hot-plug ports - {:02x}:{:02x}.{} is not watched", function.bus, function.dev, function.func);
		}
		if first {
			crate::serial_println!("pci: {:02x}:{:02x}.{} carries hot-plug slot {} - {}", function.bus, function.dev, function.func, slot.number, if occupied { "occupied" } else { "empty" });
		}
	}
}

// The deepest chain of bridges this will follow. The PCI specification allows 256 buses in total,
// so a chain that long is a numbering fault rather than a topology; the limit is here so a
// malformed one costs a bounded number of frames instead of the kernel stack.
const MAX_BRIDGE_DEPTH: u8 = 8;

fn scan_bus<A: ConfigAccess>(bus: u8, seen: &mut [bool; 256], out: &mut Vec<PciDevice>, depth: u8) {
	if seen[bus as usize] {
		return;
	}
	seen[bus as usize] = true;
	for dev in 0..32u8 {
		if A::read16(bus, dev, 0, 0x00) == 0xFFFF {
			continue;
		}
		let multifunction = A::read8(bus, dev, 0, 0x0e) & 0x80 != 0;
		let func_count: u8 = if multifunction { 8 } else { 1 };
		for func in 0..func_count {
			if A::read16(bus, dev, func, 0x00) == 0xFFFF {
				continue;
			}
			let function = read_function::<A>(bus, dev, func);
			let is_bridge = function.header_type & 0x7F == HEADER_TYPE_BRIDGE;
			// ALLOC-OK: PCI enumeration at boot; the count is the bus's, not ring 3's.
			out.push(function);
			if !is_bridge || depth >= MAX_BRIDGE_DEPTH {
				continue;
			}
			// Down the bridge, over the range it says it forwards. `secondary` is the bus
			// immediately behind it and `subordinate` the highest bus below that, so a bridge whose
			// downstream side is unconfigured (both zero, or subordinate below secondary) forwards
			// nothing and is not descended into - rather than being read as "bus 0", which is the
			// bus the walk started on.
			let secondary = A::read8(bus, dev, func, BRIDGE_SECONDARY_BUS);
			let subordinate = A::read8(bus, dev, func, BRIDGE_SUBORDINATE_BUS).min(A::MAX_BUS);
			if secondary <= bus || subordinate < secondary || secondary > A::MAX_BUS {
				continue;
			}
			for behind in secondary..=subordinate {
				scan_bus::<A>(behind, seen, out, depth + 1);
			}
		}
	}
}

// The human name of a virtio device type, for the boot log.
#[cfg(target_arch = "aarch64")]
pub fn virtio_type_name(virtio_type: u16) -> &'static str {
	match virtio_type as u32 {
		abi::VIRTIO_TYPE_NET => "net",
		abi::VIRTIO_TYPE_BLOCK => "blk",
		abi::VIRTIO_TYPE_CONSOLE => "console",
		abi::VIRTIO_TYPE_RNG => "rng",
		abi::VIRTIO_TYPE_GPU => "gpu",
		abi::VIRTIO_TYPE_SOUND => "snd",
		abi::VIRTIO_TYPE_VSOCK => "vsock",
		_ => "other",
	}
}

// Decode a memory BAR's assigned physical base (read live from config space, so it
// is correct after `assign_bars` reprograms it), handling 64-bit BARs (which occupy
// two adjacent slots). Returns None for an I/O BAR or an out-of-range index.
// ONE MEMORY RANGE THE BUS DECODES: a function's BAR, or a bridge's memory or prefetchable window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedRange {
	pub base: u64,
	pub len: u64,
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
	/// A bridge's window rather than a function's own BAR.
	pub window: bool,
}

// EVERY BRIDGE'S I/O WINDOW, as (first port, length): what a namespace device's `_CRS` ports may not overlap, beside the
// functions' own I/O BARs. A bridge with its I/O decode off forwards no port, whatever its registers hold - an
// unprogrammed base and limit of 0 would otherwise read as the whole legacy range - and a window above the 16-bit port
// space is out of every port's reach.
pub fn io_windows<A: ConfigAccess>(devices: &[PciDevice]) -> Vec<(u16, u16)> {
	let mut out = Vec::new();
	for d in devices.iter().filter(|d| d.header_type & 0x7F == 1) {
		if A::read32(d.bus, d.dev, d.func, 0x04) & 0x1 == 0 {
			continue;
		}
		let window = A::read32(d.bus, d.dev, d.func, 0x1C);
		let (base_low, limit_low) = (window & 0xFF, (window >> 8) & 0xFF);
		let upper = if base_low & 0x0F == 1 { A::read32(d.bus, d.dev, d.func, 0x30) } else { 0 };
		let base = (upper & 0xFFFF) << 16 | (base_low & 0xF0) << 8;
		let limit = (upper >> 16) << 16 | (limit_low & 0xF0) << 8 | 0xFFF;
		if limit > base && base <= 0xFFFF {
			let end = limit.min(0xFFFF);
			// ALLOC-OK: boot, one per bridge.
			out.push((base as u16, (end - base + 1).min(0xFFFF) as u16));
		}
	}
	out
}

// EVERY MEMORY RANGE THE BUS DECODES - each function's BARs, sized, and each bridge's memory and prefetchable windows -
// what no firmware region and no platform row may take. Read once, at the boot scan: sizing a BAR writes to it, which
// is safe only before a driver holds the function.
pub fn decoded_ranges<A: ConfigAccess>(devices: &[PciDevice]) -> Vec<DecodedRange> {
	let mut out = Vec::new();
	for d in devices {
		let bridge = d.header_type & 0x7F == 1;
		let bars = if bridge { 2 } else { 6 };
		// MEMORY DECODE OFF WHILE THE BARS ARE SIZED: an all-ones BAR with decode on answers at an address nobody placed
		// it, and the boot framebuffer lives in one. Restored as it was.
		let command = A::read32(d.bus, d.dev, d.func, 0x04);
		if command & 0x2 != 0 {
			A::write32(d.bus, d.dev, d.func, 0x04, command & 0xFFFF & !0x2);
		}
		let mut index = 0usize;
		while index < bars {
			let raw = A::read32(d.bus, d.dev, d.func, 0x10 + (index as u16) * 4);
			let wide = raw & 1 == 0 && (raw >> 1) & 3 == 2;
			if let (Some(base), Some(len)) = (bar_address::<A>(d, index), bar_size::<A>(d, index)) {
				// ALLOC-OK: boot, bounded by the functions on the bus.
				out.push(DecodedRange { base, len, bus: d.bus, dev: d.dev, func: d.func, window: false });
			}
			index += if wide { 2 } else { 1 };
		}
		if command & 0x2 != 0 {
			A::write32(d.bus, d.dev, d.func, 0x04, command & 0xFFFF);
		}
		if !bridge {
			continue;
		}
		// THE MEMORY WINDOW: base and limit in 1 MiB units, bits 31:20 of each 16-bit half.
		let memory = A::read32(d.bus, d.dev, d.func, 0x20);
		let (base, limit) = (((memory & 0xFFF0) as u64) << 16, (((memory >> 16) & 0xFFF0) as u64) << 16 | 0xF_FFFF);
		if limit > base {
			// ALLOC-OK: as above.
			out.push(DecodedRange { base, len: limit - base + 1, bus: d.bus, dev: d.dev, func: d.func, window: true });
		}
		// THE PREFETCHABLE WINDOW, 64-bit where bit 0 of the base says so.
		let prefetch = A::read32(d.bus, d.dev, d.func, 0x24);
		let wide = prefetch & 0xF == 1;
		let upper_base = if wide { A::read32(d.bus, d.dev, d.func, 0x28) as u64 } else { 0 };
		let upper_limit = if wide { A::read32(d.bus, d.dev, d.func, 0x2C) as u64 } else { 0 };
		let base = upper_base << 32 | ((prefetch & 0xFFF0) as u64) << 16;
		let limit = upper_limit << 32 | (((prefetch >> 16) & 0xFFF0) as u64) << 16 | 0xF_FFFF;
		if limit > base {
			// ALLOC-OK: as above.
			out.push(DecodedRange { base, len: limit - base + 1, bus: d.bus, dev: d.dev, func: d.func, window: true });
		}
	}
	out
}

pub fn bar_address<A: ConfigAccess>(d: &PciDevice, bar_idx: usize) -> Option<u64> {
	if bar_idx >= 6 {
		return None;
	}
	let bar = A::read32(d.bus, d.dev, d.func, 0x10 + (bar_idx as u16) * 4);
	if bar & 1 != 0 {
		return None; // an I/O-space BAR, not memory
	}
	let base_lo = (bar & 0xFFFF_FFF0) as u64;
	let base = if (bar >> 1) & 3 == 2 {
		// 64-bit memory BAR: the high half lives in the next slot.
		let hi = A::read32(d.bus, d.dev, d.func, 0x10 + (bar_idx as u16 + 1) * 4) as u64;
		hi << 32 | base_lo
	} else {
		base_lo
	};
	// A BAR NOBODY PLACED READS BACK ZERO, AND ZERO IS NOT AN ADDRESS.
	//
	// A function added to a LIVE machine arrives with its BARs unprogrammed - firmware placed the
	// ones it found when it looked and has not run since - so this read answers zero unless
	// something in this kernel placed them first. Taken as an address it is physical zero, and the
	// consequences are not a failure to work: the driver is handed a writable window over the
	// bottom of memory, and a virtio handshake performed against RAM reads back every value it
	// writes. The reset is acknowledged because the zero written is the zero read; FEATURES_OK is
	// accepted because the byte is still there. The device then REPORTS ITSELF ONLINE without
	// anything having answered it, which is how a hot-plug arrival passed on one port while the
	// same device on another said honestly that it was not responding.
	//
	// The caller's `None` is the honest answer: a function whose register window cannot be located
	// gets an inventory row with no resources, the same place a family with no resolver lands.
	if base == 0 {
		return None;
	}
	Some(base)
}

// Measure a memory BAR's window size with the standard probe: write all-ones to the
// register, read back the address mask, and restore the original value (the low half
// suffices - no device here has a window over 4 GB). Needed for devices like xHCI
// whose window size is not described anywhere else; virtio derives its window from
// the capability list instead. Returns None for an I/O BAR or an out-of-range index.
pub fn bar_size<A: ConfigAccess>(d: &PciDevice, bar_idx: usize) -> Option<u64> {
	if bar_idx >= 6 {
		return None;
	}
	let off: u16 = 0x10 + (bar_idx as u16) * 4;
	let bar = A::read32(d.bus, d.dev, d.func, off);
	if bar & 1 != 0 {
		return None; // an I/O-space BAR, not memory
	}
	A::write32(d.bus, d.dev, d.func, off, 0xFFFF_FFFF);
	let mask = A::read32(d.bus, d.dev, d.func, off);
	A::write32(d.bus, d.dev, d.func, off, bar);
	let size = (!(mask & 0xFFFF_FFF0) as u64).wrapping_add(1);
	if size == 0 { None } else { Some(size) }
}

// Assign a device's memory BARs out of the low 32-bit MMIO window, then enable
// memory-space decoding and bus-master in the command register. For ECAM platforms
// with no firmware to program the BARs: a BAR is (re)assigned if it is unprogrammed
// (QEMU `virt` with `-kernel`) OR the firmware placed it outside the low window the
// kernel's boot stub maps (a UEFI boot may assign the 64-bit window at 512 GB, which
// the direct map does not cover) - so devices land in the mapped low window regardless
// of how the kernel was booted. Memory decode is turned off while the BARs move.
// What one BAR slot is: how many of the six slots it consumes, and - when it is an implemented
// memory BAR - whether it is 64-bit, the address the firmware left in it, and its size.
//
// Probing means writing all-ones and reading the mask back, so it is only safe while memory
// decoding is off; both passes below run inside that window and the original value is always
// restored.
fn probe_bar<A: ConfigAccess>(d: &PciDevice, i: usize) -> (usize, Option<(bool, u64, u64)>) {
	let off = 0x10 + (i as u16) * 4;
	let bar = A::read32(d.bus, d.dev, d.func, off);
	if bar & 1 != 0 {
		return (1, None); // an I/O BAR - not used here
	}
	let is64 = (bar >> 1) & 3 == 2;
	let step = if is64 { 2 } else { 1 };
	A::write32(d.bus, d.dev, d.func, off, 0xFFFF_FFFF);
	let mask_lo = A::read32(d.bus, d.dev, d.func, off);
	A::write32(d.bus, d.dev, d.func, off, bar);
	let (mask, cur, probed) = if is64 {
		let bar_hi = A::read32(d.bus, d.dev, d.func, off + 4);
		A::write32(d.bus, d.dev, d.func, off + 4, 0xFFFF_FFFF);
		let mask_hi = A::read32(d.bus, d.dev, d.func, off + 4);
		A::write32(d.bus, d.dev, d.func, off + 4, bar_hi);
		(((mask_hi as u64) << 32) | (mask_lo & 0xFFFF_FFF0) as u64, ((bar_hi as u64) << 32) | (bar & 0xFFFF_FFF0) as u64, ((mask_hi as u64) << 32) | (mask_lo & 0xFFFF_FFF0) as u64)
	} else {
		((mask_lo & 0xFFFF_FFF0) as u64 | 0xFFFF_FFFF_0000_0000, (bar & 0xFFFF_FFF0) as u64, (mask_lo & 0xFFFF_FFF0) as u64)
	};
	// AN UNIMPLEMENTED BAR READS BACK ZERO from the all-ones write, and there is no other way to
	// tell: `size` for a 32-bit slot then works out to a nominal 4 GiB, which the old loop tried to
	// allocate, failed, and skipped in silence. That silence is gone now - a BAR that cannot be
	// placed stops the device being enabled - so the difference between "this device has three
	// BARs" and "this device cannot be placed" has to be stated here.
	if probed == 0 {
		return (step, None);
	}
	let size = (!mask).wrapping_add(1);
	if size == 0 || mask == !0u64 {
		return (step, None); // nothing this kernel can place
	}
	(step, Some((is64, cur, size)))
}

// Whether a BAR the firmware left at `cur` is one this kernel keeps: inside the low window the
// boot stub maps, and actually programmed.
fn is_retained<A: ConfigAccess>(cur: u64) -> bool {
	cur != 0 && cur < A::MMIO_WINDOW_END
}

// A PCI-TO-PCI BRIDGE FORWARDS AN ADDRESS RANGE AND NOT A DEVICE, and this is the range.
//
// The window is one dword: the base in the low half and the limit in the high half, each naming
// the top twelve bits of an address, so a window covers whole megabytes and a base ABOVE the limit
// is a window that is CLOSED. Closed is what a bridge holds out of reset and what firmware leaves
// behind a hot-plug slot that was empty when it looked - which is a bridge that forwards nothing,
// however correctly the BAR behind it is programmed.
const BRIDGE_MEMORY_WINDOW: u16 = 0x20;
const BRIDGE_WINDOW_GRAIN: u64 = 0x10_0000;

// What a bridge whose window is closed is given, when this kernel has addresses to open one from.
// Two megabytes is what the PCI Hot Plug specification names as a bridge's default memory padding,
// and it holds a hundred virtio register files.
const BRIDGE_APERTURE: u64 = 2 * BRIDGE_WINDOW_GRAIN;

// One bridge's window, and where the next BAR placed behind it goes.
#[derive(Clone, Copy)]
struct Aperture {
	// The buses this bridge forwards: how a device finds the window its own address has to be in.
	bus_lo: u8,
	bus_hi: u8,
	base: u64,
	next: u64,
	end: u64,
}

// EIGHT, one per bridge that has anything placed behind it. A row is made the first time a BAR is
// placed behind a bridge and lives as long as the machine does: the window is programmed once, and
// what moves afterwards is only the cursor inside it.
const MAX_BRIDGE_APERTURES: usize = 8;
static APERTURES: SpinLock<[Option<Aperture>; MAX_BRIDGE_APERTURES]> = SpinLock::new([None; MAX_BRIDGE_APERTURES]);

// Where a device's addresses come from.
#[derive(Clone, Copy)]
enum Pool {
	// The platform's own low MMIO window - a root bus, and the only case there has ever been.
	Platform,
	// The window a bridge forwards to the bus this device is on.
	Aperture(usize),
	// Behind a bridge this kernel could not give a window to, so there is no address that reaches
	// this device at all. Named rather than folded into an exhausted pool, because the operator's
	// two questions - "is the window full" and "is there a window" - have different answers.
	Unreachable,
}

impl Pool {
	fn describe(self) -> &'static str {
		match self {
			Pool::Platform => "the MMIO window",
			Pool::Aperture(_) => "the window its bridge forwards",
			Pool::Unreachable => "a bridge with no window",
		}
	}
}

// Whether a BAR already at `cur` is one this pool keeps where it is.
fn pool_retains<A: ConfigAccess>(pool: Pool, cur: u64) -> bool {
	match pool {
		Pool::Platform => is_retained::<A>(cur),
		// INSIDE THE WINDOW ITS BRIDGE FORWARDS, which is a different question from the platform's
		// and has to be: a BAR firmware placed in the low window is a perfectly good address that
		// nothing behind this bridge can be reached at.
		Pool::Aperture(index) => matches!(APERTURES.lock()[index], Some(row) if cur >= row.base && cur < row.end),
		Pool::Unreachable => false,
	}
}

fn pool_reserve<A: ConfigAccess>(pool: Pool, base: u64, size: u64) {
	match pool {
		Pool::Platform => A::reserve_mmio(base, size),
		// A retained BAR inside an aperture was already pushed past when the aperture was made
		// (`occupied_behind`), which is the only moment every function behind the bridge is read.
		Pool::Aperture(_) | Pool::Unreachable => {}
	}
}

fn pool_alloc<A: ConfigAccess>(pool: Pool, size: u64) -> Option<u64> {
	match pool {
		Pool::Platform => A::alloc_mmio(size),
		Pool::Aperture(index) => {
			let mut table = APERTURES.lock();
			let row = table[index].as_mut()?;
			let size = size.max(0x1000);
			let base = (row.next + size - 1) & !(size - 1);
			if base.checked_add(size)? > row.end {
				return None;
			}
			row.next = base + size;
			Some(base)
		}
		Pool::Unreachable => None,
	}
}

// The bridge that forwards `target`, or None when `target` is a root bus.
//
// THE CLOSEST ONE. A bus behind two bridges is forwarded by both, and the window a BAR has to fit
// inside is the inner bridge's - so the walk goes down before it answers.
fn forwarding_bridge<A: ConfigAccess>(target: u8) -> Option<(u8, u8, u8)> {
	fn walk<A: ConfigAccess>(from: u8, target: u8, depth: u8) -> Option<(u8, u8, u8)> {
		if from == target || depth > MAX_BRIDGE_DEPTH {
			return None;
		}
		for dev in 0..32u8 {
			if A::read16(from, dev, 0, 0x00) == 0xFFFF {
				continue;
			}
			let func_count: u8 = if A::read8(from, dev, 0, 0x0e) & 0x80 != 0 { 8 } else { 1 };
			for func in 0..func_count {
				if A::read16(from, dev, func, 0x00) == 0xFFFF || A::read8(from, dev, func, 0x0e) & 0x7F != HEADER_TYPE_BRIDGE {
					continue;
				}
				let secondary = A::read8(from, dev, func, BRIDGE_SECONDARY_BUS);
				let subordinate = A::read8(from, dev, func, BRIDGE_SUBORDINATE_BUS).min(A::MAX_BUS);
				if secondary <= from || subordinate < secondary || target < secondary || target > subordinate {
					continue;
				}
				return walk::<A>(secondary, target, depth + 1).or(Some((from, dev, func)));
			}
		}
		None
	}
	walk::<A>(0, target, 0)
}

// Probe one function's BAR with its memory decoding off, which is the only state a probe is valid
// in: probing writes all-ones and reads the size mask back, so a function that is decoding would
// answer at whatever the all-ones address decodes to for as long as the write stood.
fn probe_bar_quietly<A: ConfigAccess>(d: &PciDevice, i: usize) -> (usize, Option<(bool, u64, u64)>) {
	let command = A::read32(d.bus, d.dev, d.func, 0x04) as u16;
	// THE STATUS HALF IS WRITTEN AS ZERO, both times. Its bits are write-one-to-clear, so putting
	// back what was read would clear whatever the device had reported between the two writes -
	// which is the same reason every other writer of this register in this file writes zeros.
	A::write32(d.bus, d.dev, d.func, 0x04, (command & !CMD_MEMORY_SPACE) as u32);
	let probed = probe_bar::<A>(d, i);
	A::write32(d.bus, d.dev, d.func, 0x04, command as u32);
	probed
}

// The highest address already spoken for inside a bridge's window, so a cursor starting there
// cannot hand out what something behind the bridge already decodes.
//
// THE SAME RULE AS THE PLATFORM WINDOW'S (KERN-ARCH-015), for the same reason and one level down:
// firmware placed the BARs of whatever was behind this bridge when it looked, inside the window it
// gave the bridge, and a bump starting at the base would hand those addresses to the next device
// to arrive. Two apertures, one span, and nothing either device can report.
//
// NOTHING BEHIND THE BRIDGE IS BEING DRIVEN WHILE THIS RUNS, which is what makes probing here
// safe: a row is made the FIRST time a BAR is placed behind this bridge, so either this is the
// boot scan - before any driver exists - or the bridge was empty until the function now arriving,
// which has no driver yet either.
fn occupied_behind<A: ConfigAccess>(secondary: u8, subordinate: u8, base: u64, end: u64) -> u64 {
	let mut next = base;
	for bus in secondary..=subordinate {
		for dev in 0..32u8 {
			if A::read16(bus, dev, 0, 0x00) == 0xFFFF {
				continue;
			}
			let func_count: u8 = if A::read8(bus, dev, 0, 0x0e) & 0x80 != 0 { 8 } else { 1 };
			for func in 0..func_count {
				if A::read16(bus, dev, func, 0x00) == 0xFFFF {
					continue;
				}
				let header_type = A::read8(bus, dev, func, 0x0e);
				let function = PciDevice { bus, dev, func, vendor: 0, device_id: 0, class: 0, subclass: 0, prog_if: 0, header_type };
				// A BRIDGE HAS TWO BAR SLOTS AND NOT SIX: the four dwords a header-type-0 function
				// keeps its remaining BARs in are where a bridge keeps its bus numbers and its
				// windows, and probing those would write all-ones over the numbering this walk is
				// standing on.
				let slots = if header_type & 0x7F == HEADER_TYPE_BRIDGE { 2 } else { 6 };
				let mut i = 0usize;
				while i < slots {
					let (step, bar) = probe_bar_quietly::<A>(&function, i);
					if let Some((_, cur, size)) = bar
						&& cur >= base && cur < end
					{
						next = next.max(cur.saturating_add(size).min(end));
					}
					i += step;
				}
			}
		}
	}
	next
}

// How many functions are present behind a bridge. Config reads only - nothing is probed and
// nothing is written - because this is the question that decides whether probing is allowed.
fn functions_behind<A: ConfigAccess>(secondary: u8, subordinate: u8) -> usize {
	let mut found = 0usize;
	for bus in secondary..=subordinate {
		for dev in 0..32u8 {
			if A::read16(bus, dev, 0, 0x00) == 0xFFFF {
				continue;
			}
			let func_count: u8 = if A::read8(bus, dev, 0, 0x0e) & 0x80 != 0 { 8 } else { 1 };
			for func in 0..func_count {
				if A::read16(bus, dev, func, 0x00) != 0xFFFF {
					found += 1;
				}
			}
		}
	}
	found
}

// Give a bridge a window and turn its memory decoding on, so the span it now forwards is one that
// transactions actually reach.
//
// THE PREFETCHABLE WINDOW IS LEFT EXACTLY AS FOUND. A bridge decodes by ADDRESS and not by what a
// device's BAR says about itself, so a prefetchable BAR placed in the ordinary window is forwarded
// by the ordinary window; and this only runs when the ordinary window was CLOSED, which on a
// machine whose firmware programmed neither leaves nothing to conflict with. Rewriting it would be
// this kernel closing a window firmware opened for a device it cannot see.
fn program_bridge_window<A: ConfigAccess>(bus: u8, dev: u8, func: u8, base: u64, size: u64) {
	let last = base + size - 1;
	let window = ((base >> 16) as u32 & 0xFFF0) | (((last >> 16) as u32 & 0xFFF0) << 16);
	A::write32(bus, dev, func, BRIDGE_MEMORY_WINDOW, window);
	A::update32(bus, dev, func, 0x04, |dword| ((dword as u16) | CMD_MEMORY_SPACE) as u32);
	crate::serial_println!("pci: {:02x}:{:02x}.{} now forwards {:#x}..{:#x} to the bus behind it", bus, dev, func, base, last + 1);
}

// The aperture for one bridge, made on first use. `parent` is where its window comes from if it
// has to be opened, which for a bridge behind another bridge is that one's aperture - so an inner
// window is inside its outer one by construction rather than by luck.
fn ensure_aperture<A: ConfigAccess>(bus: u8, dev: u8, func: u8, parent: Pool) -> Option<usize> {
	let secondary = A::read8(bus, dev, func, BRIDGE_SECONDARY_BUS);
	let subordinate = A::read8(bus, dev, func, BRIDGE_SUBORDINATE_BUS).min(A::MAX_BUS);
	if secondary == 0 || subordinate < secondary {
		return None;
	}
	let find = |table: &[Option<Aperture>; MAX_BRIDGE_APERTURES]| table.iter().position(|row| matches!(row, Some(a) if a.bus_lo == secondary && a.bus_hi == subordinate));
	// BOUND BEFORE THE `if let` AND NOT INSIDE ITS SCRUTINEE. A temporary in an `if let` condition
	// lives until the end of its body, so the guard this lock returns would still be held while the
	// body below locks again - which on a spin lock is not a race, it is a stop.
	let existing = find(&APERTURES.lock());
	if let Some(index) = existing {
		// A SLOT EMPTIED AND FILLED AGAIN GETS ITS ADDRESSES BACK.
		//
		// The cursor only ever moves forward, and a device that leaves takes its BARs with it and
		// gives nothing back - so a slot plugged and unplugged enough times would run its bridge's
		// window out and the arrival after that would be left disabled, on a machine with the same
		// one device in the same one slot. A hot-plug slot is the thing a person uses REPEATEDLY.
		//
		// RE-READING IS SAFE EXACTLY WHEN THERE IS ONE FUNCTION BEHIND THE BRIDGE, and the guard is
		// that count: the one function is then the one now arriving, which has no driver yet, so
		// the probe `occupied_behind` does cannot disturb anything. With a second function present
		// - a multi-function card, a device already bound - the cursor is left where it is and the
		// window is spent rather than risked.
		let (base, end) = match APERTURES.lock()[index] {
			Some(row) => (row.base, row.end),
			None => return None,
		};
		if functions_behind::<A>(secondary, subordinate) == 1 {
			let next = occupied_behind::<A>(secondary, subordinate, base, end);
			if let Some(row) = APERTURES.lock()[index].as_mut() {
				row.next = next;
			}
		}
		return Some(index);
	}
	// THE WINDOW FIRMWARE ALREADY FORWARDS, WHERE THERE IS ONE. Firmware that padded a hot-plug
	// bridge left the room for a device added later HERE, and using it is what makes such a device
	// reachable on a port where this kernel hands out no addresses of its own at all.
	let programmed = A::read32(bus, dev, func, BRIDGE_MEMORY_WINDOW);
	let open_base = ((programmed & 0xFFF0) as u64) << 16;
	let open_last = (((programmed >> 16) & 0xFFF0) as u64) << 16;
	// AND A WINDOW AT ADDRESS ZERO IS NOT A WINDOW, the same rule as a BAR's and for the same
	// reason: a base of zero with a limit of zero is what a register nobody wrote reads back, and
	// read as a window it says the bridge forwards the first megabyte of physical memory.
	let (base, end) = if open_last >= open_base && open_base != 0 {
		(open_base, open_last + BRIDGE_WINDOW_GRAIN)
	} else {
		let base = pool_alloc::<A>(parent, BRIDGE_APERTURE)?;
		program_bridge_window::<A>(bus, dev, func, base, BRIDGE_APERTURE);
		(base, base + BRIDGE_APERTURE)
	};
	let next = occupied_behind::<A>(secondary, subordinate, base, end);
	let mut table = APERTURES.lock();
	if let Some(index) = find(&table) {
		return Some(index);
	}
	let index = table.iter().position(|row| row.is_none())?;
	table[index] = Some(Aperture { bus_lo: secondary, bus_hi: subordinate, base, next, end });
	Some(index)
}

// Forget every aperture, so a test that stands up a bridge does not inherit the one before it.
// The table is a machine's topology, which outlives everything else in a running kernel.
#[cfg(test)]
pub fn forget_apertures() {
	*APERTURES.lock() = [None; MAX_BRIDGE_APERTURES];
}

// Which pool a device on `bus` is placed out of, opening every window on the way down that is not
// open yet.
fn pool_for<A: ConfigAccess>(bus: u8) -> Pool {
	let Some(bridge) = forwarding_bridge::<A>(bus) else {
		return Pool::Platform;
	};
	// The chain from the device's bridge up to the root, walked OUTWARDS and then used INWARDS: a
	// bridge's window can only be carved out of its parent's, so the outer one has to exist first.
	let mut chain = [(0u8, 0u8, 0u8); MAX_BRIDGE_DEPTH as usize + 1];
	let mut depth = 0usize;
	let mut at = bridge;
	while depth < chain.len() {
		chain[depth] = at;
		depth += 1;
		match forwarding_bridge::<A>(at.0) {
			Some(parent) => at = parent,
			None => break,
		}
	}
	let mut pool = Pool::Platform;
	for &(bus, dev, func) in chain[..depth].iter().rev() {
		pool = match ensure_aperture::<A>(bus, dev, func, pool) {
			Some(index) => Pool::Aperture(index),
			None => return Pool::Unreachable,
		};
	}
	pool
}

// EVERY BAR PLACEMENT GOES THROUGH HERE, and this is the one place that knows a device behind a
// bridge is not placed the way a device on a root bus is.
//
// A root bus keeps exactly the behaviour each port already had - the ports with firmware place
// nothing, the ports without place out of their own window. What is new is the other case, which
// used to be the same as the first and is why a device plugged into a live machine did not work on
// any port: on the ports with no firmware its BAR went into the platform window, which the bridge
// in front of it does not forward, and the driver found nothing at the address it was given; on
// the port with firmware the BAR was never placed at all, and the driver was given physical zero.
pub fn place_bars<A: ConfigAccess>(d: &PciDevice) {
	match pool_for::<A>(d.bus) {
		Pool::Platform => A::assign_bars(d),
		Pool::Unreachable => crate::serial_println!("pci: {:02x}:{:02x}.{} is behind a bridge this kernel has no addresses to open a window for; it is left as it was found", d.bus, d.dev, d.func),
		pool => place_bars_from::<A>(d, pool),
	}
}

#[cfg(any(test, target_arch = "aarch64", target_arch = "riscv64"))]
pub fn assign_bars_ecam<A: ConfigAccess>(d: &PciDevice) {
	place_bars_from::<A>(d, Pool::Platform);
}

// THE SAME PLACEMENT, OUT OF WHICHEVER POOL THE DEVICE'S ADDRESSES HAVE TO COME FROM.
//
// A device on a root bus is placed out of the platform's low MMIO window, which is what this has
// always done. A device BEHIND A BRIDGE cannot be: a bridge forwards an address RANGE, and an
// address outside the range it forwards reaches nothing - so its BARs come out of that bridge's
// own window and nowhere else. The two cases differ only in where an address comes from and which
// already-placed BARs have to be kept clear of, so they are one function and a pool.
fn place_bars_from<A: ConfigAccess>(d: &PciDevice, pool: Pool) {
	// Disable memory-space decoding while the BARs move (the firmware may have enabled it).
	A::update32(d.bus, d.dev, d.func, 0x04, |dword| ((dword as u16) & !CMD_MEMORY_SPACE) as u32);

	// PASS ONE: EVERY SPAN THE FIRMWARE ALREADY PLACED, RESERVED BEFORE ANYTHING IS ALLOCATED
	// (KERN-ARCH-015). A retained BAR was left alone and never handed to the allocator, so a later
	// unprogrammed BAR - on this device or the next one enumerated - could be given the same
	// addresses. Two functions then decode one aperture, and neither of them can tell.
	let mut i = 0usize;
	while i < 6 {
		let (step, bar) = probe_bar::<A>(d, i);
		if let Some((_, cur, size)) = bar
			&& pool_retains::<A>(pool, cur)
		{
			pool_reserve::<A>(pool, cur, size);
		}
		i += step;
	}

	// PASS TWO: place what is missing, and remember whether all of it was placed.
	let mut all_placed = true;
	i = 0;
	while i < 6 {
		let (step, bar) = probe_bar::<A>(d, i);
		if let Some((is64, cur, size)) = bar
			&& !pool_retains::<A>(pool, cur)
		{
			match pool_alloc::<A>(pool, size) {
				Some(base) => {
					let off = 0x10 + (i as u16) * 4;
					let raw = A::read32(d.bus, d.dev, d.func, off);
					A::write32(d.bus, d.dev, d.func, off, (base as u32 & 0xFFFF_FFF0) | (raw & 0xF));
					if is64 {
						A::write32(d.bus, d.dev, d.func, off + 4, (base >> 32) as u32);
					}
				}
				None => {
					all_placed = false;
					crate::serial_println!("pci: {:02x}:{:02x}.{} BAR{i} needs {size} bytes and {} has none left", d.bus, d.dev, d.func, pool.describe());
				}
			}
		}
		i += step;
	}

	// A DEVICE WITH A BAR THAT WAS NOT PLACED IS NOT ENABLED (KERN-ARCH-015).
	//
	// Decode and bus-master used to be turned on regardless: an exhausted window left the BAR
	// reading zero, and the device was then told to respond at address zero and to master the bus.
	// A device that cannot be addressed is a device that stays off, and the line above says which
	// one and why.
	if !all_placed {
		crate::serial_println!("pci: {:02x}:{:02x}.{} left disabled - a BAR could not be placed", d.bus, d.dev, d.func);
		return;
	}
	// Memory space on, BUS MASTERING OFF - and off explicitly, whatever the firmware left set.
	//
	// A device may write to memory exactly while a driver owns it. Enumeration is not ownership: it
	// is the kernel looking at the bus, and a device left mastering from here would be able to DMA
	// into any physical address from boot until the machine stopped, on a machine with no IOMMU,
	// with nobody driving it. `device::acquire_bus_master` turns it on when a driver takes the
	// device and `device::release_bus_master` turns it off when the last one lets go.
	A::update32(d.bus, d.dev, d.func, 0x04, |dword| (((dword as u16) | CMD_MEMORY_SPACE) & !CMD_BUS_MASTER) as u32);
}

// Walk a device's PCI capability list and resolve its MSI-X capability: the
// capability's config-space offset (0 = none), the table entry count, and the
// physical address of the MSI-X table. Shared by the virtio and xHCI paths.
fn resolve_msix<A: ConfigAccess>(d: &PciDevice) -> (u16, u64) {
	if A::read16(d.bus, d.dev, d.func, 0x06) & STATUS_CAP_LIST == 0 {
		return (0, 0);
	}
	let mut ptr: u16 = (A::read8(d.bus, d.dev, d.func, 0x34) & 0xFC) as u16;
	// Bound the walk so a malformed (cyclic) list cannot spin forever.
	for _ in 0..48 {
		if ptr == 0 {
			break;
		}
		let cap_id = A::read8(d.bus, d.dev, d.func, ptr);
		let next = (A::read8(d.bus, d.dev, d.func, ptr + 1) & 0xFC) as u16;
		if cap_id == MSIX_CAP_ID {
			let table_off_bir = A::read32(d.bus, d.dev, d.func, ptr + 4);
			let bir = (table_off_bir & 7) as usize;
			let table_offset = (table_off_bir & !7) as u64;
			if let Some(base) = bar_address::<A>(d, bir) {
				return (ptr, base + table_offset);
			}
		}
		ptr = next;
	}
	(0, 0)
}

// Walk a device's PCI capability list and resolve its virtio configuration
// structures. Returns None if it is not a virtio device, has no capability list, or
// is missing the required common/notify/ISR structures. `assign_bars` runs first so
// ECAM platforms program the BARs the resolved offsets are relative to.
fn resolve_virtio<A: ConfigAccess>(d: &PciDevice) -> Option<VirtioDevice> {
	let virtio_type = d.virtio_type()?;
	if A::read16(d.bus, d.dev, d.func, 0x06) & STATUS_CAP_LIST == 0 {
		return None;
	}
	place_bars::<A>(d);
	let (mut common, mut notify, mut isr, mut device) = (None, None, None, None);
	let mut ptr: u16 = (A::read8(d.bus, d.dev, d.func, 0x34) & 0xFC) as u16;
	// Bound the walk so a malformed (cyclic) list cannot spin forever.
	for _ in 0..48 {
		if ptr == 0 {
			break;
		}
		let cap_id = A::read8(d.bus, d.dev, d.func, ptr);
		let next = (A::read8(d.bus, d.dev, d.func, ptr + 1) & 0xFC) as u16;
		if cap_id == CAP_ID_VENDOR {
			let cfg_type = A::read8(d.bus, d.dev, d.func, ptr + 3);
			let mut cap = VirtioCap { bar: A::read8(d.bus, d.dev, d.func, ptr + 4), offset: A::read32(d.bus, d.dev, d.func, ptr + 8), length: A::read32(d.bus, d.dev, d.func, ptr + 12), notify_multiplier: 0 };
			match cfg_type {
				VIRTIO_CAP_COMMON => common = Some(cap),
				VIRTIO_CAP_NOTIFY => {
					cap.notify_multiplier = A::read32(d.bus, d.dev, d.func, ptr + 16);
					notify = Some(cap);
				}
				VIRTIO_CAP_ISR => isr = Some(cap),
				VIRTIO_CAP_DEVICE => device = Some(cap),
				_ => {}
			}
		}
		ptr = next;
	}
	let (msix_cap, msix_table_phys) = resolve_msix::<A>(d);
	let common = common?;
	let notify = notify?;
	let isr = isr?;
	let bar = common.bar;

	// ONE BAR, OR THIS IS NOT A DEVICE THIS KERNEL CAN DESCRIBE (KERN-ARCH-014).
	//
	// Each virtio capability names its own BAR, and a spec-valid device may spread them. What is
	// handed to a driver is ONE physical window plus four offsets into it, and the driver adds each
	// offset to that one base - so a structure living in another BAR was read at the right offset
	// of the WRONG aperture. Silently: the addresses are mapped and the reads succeed.
	//
	// Refusing is the honest answer while the hand-off is one window. Claiming the device and
	// driving it through the wrong registers is not, and neither is inventing a second window the
	// driver ABI has nowhere to put.
	if notify.bar != bar || isr.bar != bar {
		crate::serial_println!("pci: virtio {:02x}:{:02x}.{} spreads its configuration over BARs {} (common), {} (notify) and {} (isr); a driver is handed one window, so this device is left unclaimed", d.bus, d.dev, d.func, bar, notify.bar, isr.bar);
		return None;
	}
	// The optional structure is dropped rather than the device refused when it is the odd one out:
	// it is the only one a driver can work without.
	let device = match device {
		Some(cap) if cap.bar != bar => {
			crate::serial_println!("pci: virtio {:02x}:{:02x}.{} keeps its device configuration in BAR {} rather than BAR {bar}; it is not reported", d.bus, d.dev, d.func, cap.bar);
			None
		}
		other => other,
	};

	let bar_phys = bar_address::<A>(d, bar as usize)?;
	// AND INSIDE THE BAR IT NAMES. `offset` and `length` come from the device's own config space,
	// so a structure running past the end of the aperture is a device describing memory the BAR
	// does not decode - which the driver would then map and read.
	let bar_len = bar_size::<A>(d, bar as usize)?;
	for cap in [Some(common), Some(notify), Some(isr), device].into_iter().flatten() {
		let end = cap.offset as u64 + cap.length as u64;
		if end > bar_len {
			crate::serial_println!("pci: virtio {:02x}:{:02x}.{} declares a structure ending at {end:#x} in a BAR {bar_len:#x} bytes long; the device is left unclaimed", d.bus, d.dev, d.func);
			return None;
		}
	}

	// The window the driver maps is the BAR holding the common-config structure;
	// its length is the furthest end of any virtio structure in it, rounded up to a page.
	let mut end: u64 = 0;
	for cap in [Some(common), Some(notify), Some(isr), device].into_iter().flatten() {
		end = end.max(cap.offset as u64 + cap.length as u64);
	}
	let region_len = end.div_ceil(0x1000) * 0x1000;
	Some(VirtioDevice {
		pci: *d,
		virtio_type,
		#[cfg(any(test, target_arch = "aarch64"))]
		bar,
		bar_phys,
		region_len,
		common,
		notify,
		isr,
		device,
		msix_cap,
		msix_table_phys,
	})
}

// Scan the bus and resolve every modern virtio device's MMIO layout.
// One function's virtio layout, for a caller that has an address rather than a scan - which is what
// a device plugged into a live machine is.
pub fn resolve_virtio_function<A: ConfigAccess>(function: &PciDevice) -> Option<VirtioDevice> {
	resolve_virtio::<A>(function)
}

// The same for a resourced function: one window, resolved from one address.
pub fn resolve_endpoint_function<A: ConfigAccess>(function: &PciDevice) -> Option<ResourcedDevice> {
	resolve_endpoint::<A>(function)
}

pub fn scan_virtio<A: ConfigAccess>() -> Vec<VirtioDevice> {
	// ALLOC-OK: bus enumeration, at boot, before any userspace exists to reach it.
	scan::<A>().iter().filter_map(resolve_virtio::<A>).collect()
}

// Resolve one resourced function's MMIO window: BAR 0 holds the whole register file for every
// family in the table above - the xHCI capability registers, the NVMe controller registers - so its
// (assigned) base plus probed size is the window a driver maps. `assign_bars` runs first for ECAM
// platforms with no firmware. Returns None if the class triple is not one this kernel resolves, or
// if BAR 0 is not a memory BAR.
//
// THE MISSING BAR IS A REFUSAL AND NOT A ZERO. A function whose BAR 0 does not resolve is left to
// the inventory pass, which gives it an identity row with no resources - the same place a family
// with no resolver lands. Filling `bar_phys: 0, bar_len: 0` into a resourced row instead would
// produce an entry that claims a profile and hands out a window of nothing.
fn resolve_endpoint<A: ConfigAccess>(d: &PciDevice) -> Option<ResourcedDevice> {
	let (device_type, bar) = d.resourced_type()?;
	place_bars::<A>(d);
	let bar_phys = bar_address::<A>(d, bar)?;
	let bar_len = bar_size::<A>(d, bar)?;
	let (msix_cap, msix_table_phys) = resolve_msix::<A>(d);
	Some(ResourcedDevice { pci: *d, device_type, bar_phys, bar_len, msix_cap, msix_table_phys })
}

// Scan the bus and resolve the MMIO window of every function whose class triple this kernel knows.
pub fn scan_resourced<A: ConfigAccess>() -> Vec<ResourcedDevice> {
	// ALLOC-OK: bus enumeration, at boot, as above.
	scan::<A>().iter().filter_map(resolve_endpoint::<A>).collect()
}

// Set or clear a function's PCI command-register Interrupt Disable bit (bit 10), which
// gates whether the device may assert its legacy INTx pin. Disabling it silences a
// device whose driver does not service its interrupt, so it cannot storm a shared INTx
// line (the kernel takes every device interrupt via per-device MSI-X). The status half
// is written as 0, so no write-1-to-clear status bit is touched.
pub fn set_intx_disabled<A: ConfigAccess>(bus: u8, dev: u8, func: u8, disabled: bool) {
	A::update32(bus, dev, func, 0x04, |dword| {
		let command = dword as u16;
		let new_command = if disabled { command | CMD_INTX_DISABLE } else { command & !CMD_INTX_DISABLE };
		// The status half is written as 0, so no write-1-to-clear status bit is touched - which is
		// what `write_command` did and is preserved here rather than carried over from the read.
		new_command as u32
	});
}

// Enable MSI-X on a device (set the MSI-X Enable bit, clear the Function Mask) and make sure its
// memory space is decoded, so the MSI-X table BAR responds. Called once the kernel has programmed
// the device's table entry. `cap` is the MSI-X capability's config-space offset.
//
// IT DOES NOT TOUCH BUS MASTERING ANY MORE. It used to set the bit as a side effect, with the
// reasoning that MSI delivery is itself a DMA write - true, and still not this function's to decide:
// the message is only sent when a driver has the device running, and a driver has the device only
// after `sys_device_acquire`, which is where the bit is turned on now. Two places setting one bit is
// how it came to be set for devices nobody owned.
pub fn msix_enable<A: ConfigAccess>(bus: u8, dev: u8, func: u8, cap: u16) {
	A::update32(bus, dev, func, 0x04, |dword| ((dword as u16) | CMD_MEMORY_SPACE) as u32);
	// Message Control is the upper 16 bits of the dword at `cap` (cap_id/next are the
	// low 16): enable MSI-X, clear the function mask.
	A::update32(bus, dev, func, cap, |dword| {
		let mc = (((dword >> 16) as u16) | MSIX_ENABLE) & !MSIX_FUNCTION_MASK;
		(dword & 0x0000_ffff) | ((mc as u32) << 16)
	});
}

// TURN MSI-X OFF AGAIN: clear MSI-X Enable and set the Function Mask.
//
// THE OTHER HALF OF `msix_enable`, AND IT DID NOT EXIST. A claim release masked the device's table
// ENTRY and unmapped the table page, and left the function itself enabled - so the next binding of
// that device started with MSI-X Enable already set, inherited from a binding that had ended. That
// is what made the window in `sys_device_msix_acquire` reachable: a stale-generation caller whose
// claim check passed and whose claim was released while the call ran would program entry 0 of the
// REPLACEMENT's table with vector control zero - unmasked - on a function that was already enabled,
// and the late generation check in `register_derived` rolls the bookkeeping back after the device
// has been made able to deliver. With the function disabled by the release, the same write reaches a
// device that cannot send anything, and the replacement's own acquire is what enables it again.
//
// Both bits, because they answer different questions: Enable is whether the function uses MSI-X at
// all, and the Function Mask is whether any of its vectors may be sent. A device left with Enable
// clear and the mask clear is one bit away from delivering.
//
// Memory space is NOT touched. The teardown still has to reach the table page to mask the entry, and
// a function whose BARs stopped responding mid-teardown is a function whose mask write goes nowhere.
pub fn msix_disable<A: ConfigAccess>(bus: u8, dev: u8, func: u8, cap: u16) {
	A::update32(bus, dev, func, cap, |dword| {
		let mc = (((dword >> 16) as u16) & !MSIX_ENABLE) | MSIX_FUNCTION_MASK;
		(dword & 0x0000_ffff) | ((mc as u32) << 16)
	});
}

// Ensure a function decodes memory space without touching its interrupt-delivery mode. The riscv
// INTx-over-PLIC path needs the BARs to respond but must NOT enable MSI-X: on QEMU virt the PLIC
// receives only wired INTx, so a device switched to MSI-X would send a message nothing receives.
// This keeps the device on its INTx pin.
//
// BUS MASTERING IS NOT ITS BUSINESS EITHER - see `msix_enable`. The name lost its second half with
// the behaviour.
#[cfg(target_arch = "riscv64")]
pub fn enable_memory_space<A: ConfigAccess>(bus: u8, dev: u8, func: u8) {
	A::update32(bus, dev, func, 0x04, |dword| ((dword as u16) | CMD_MEMORY_SPACE) as u32);
}

// ONE FUNCTION'S I/O BARS: (index, base, length) for each, into `out`, answering how many.
//
// SIZED WITH THE FUNCTION'S I/O DECODE OFF for as long as the all-ones pattern is in a register, so the
// probe never makes the function answer at ports nobody gave it; the command register and every BAR are
// restored before the serialised access ends. A function with no I/O BAR is never written at all - its
// decode is left exactly as it was, which matters for the bridges and the chipset functions whose decode
// carries ports the kernel itself uses.
//
// A BAR nobody placed reads back zero and is not recorded; neither is one whose base or end lies past
// 0xFFFF, which is no port.
#[cfg(target_arch = "x86_64")]
pub fn io_bars<A: ConfigAccess>(bus: u8, dev: u8, func: u8, out: &mut [(u8, u16, u16); 6]) -> usize {
	// Endpoints only: a bridge's I/O window forwards for what is behind it and is nobody's to grant.
	if (A::read32(bus, dev, func, 0x0C) >> 16) & 0x7F != 0 {
		return 0;
	}
	let mut any = false;
	let mut slot = 0usize;
	while slot < 6 {
		let bar = A::read32(bus, dev, func, 0x10 + slot as u16 * 4);
		if bar & 1 != 0 {
			any = true;
			slot += 1;
		} else {
			// A 64-bit memory BAR takes two slots.
			slot += if (bar >> 1) & 3 == 2 { 2 } else { 1 };
		}
	}
	if !any {
		return 0;
	}
	let mut count = 0usize;
	A::with_config(|| {
		let command = A::read32_raw(bus, dev, func, 0x04) as u16;
		// Only the command half is written back: writing zero to the status half's write-one-to-clear bits
		// leaves them as they are.
		A::write32_raw(bus, dev, func, 0x04, (command & !CMD_IO_SPACE) as u32);
		let mut slot = 0usize;
		while slot < 6 {
			let offset = 0x10 + slot as u16 * 4;
			let bar = A::read32_raw(bus, dev, func, offset);
			if bar & 1 == 0 {
				slot += if (bar >> 1) & 3 == 2 { 2 } else { 1 };
				continue;
			}
			A::write32_raw(bus, dev, func, offset, 0xFFFF_FFFF);
			let mask = A::read32_raw(bus, dev, func, offset);
			A::write32_raw(bus, dev, func, offset, bar);
			// A decoder of sixteen bits reads the upper half back as zero; the size is in the low half
			// either way.
			let decoded = mask & 0xFFFC;
			let base = bar & 0xFFFF_FFFC;
			if decoded != 0 && base != 0 {
				let len = ((!decoded) & 0xFFFF) + 1;
				if base + len <= 0x1_0000 {
					out[count] = (slot as u8, base as u16, len as u16);
					count += 1;
				} else {
					crate::serial_println!("pci: {bus:02x}:{dev:02x}.{func} BAR {slot} is an I/O BAR past the port space ({base:#x}+{len:#x}) - not recorded");
				}
			}
			slot += 1;
		}
		A::write32_raw(bus, dev, func, 0x04, command as u32);
	});
	count
}

// Turn a function's I/O decode on or off - a claim of a row with an I/O BAR sets it, its release clears
// it, exactly as bus mastering is handled.
#[cfg(target_arch = "x86_64")]
pub fn set_io_decode<A: ConfigAccess>(bus: u8, dev: u8, func: u8, on: bool) {
	A::update32(bus, dev, func, 0x04, |dword| {
		let command = dword as u16;
		(if on { command | CMD_IO_SPACE } else { command & !CMD_IO_SPACE }) as u32
	});
}

// Turn bus mastering on or off for one function.
//
// THE ONE PLACE THE BIT IS WRITTEN, and the caller is `device`, which knows whether anybody owns the
// device. Every other writer of this bit was removed: enumeration clears it, MSI-X setup leaves it
// alone, and the riscv INTx path leaves it alone.
pub fn set_bus_master<A: ConfigAccess>(bus: u8, dev: u8, func: u8, on: bool) {
	A::update32(bus, dev, func, 0x04, |dword| {
		let command = dword as u16;
		(if on { command | CMD_BUS_MASTER } else { command & !CMD_BUS_MASTER }) as u32
	});
}

// ---------------------------------------------------------------------------------------------
// Advanced Error Reporting and power-management events.
//
// WHAT THIS IS FOR IS SAYING SO, AND NOT RECOVERING. A PCIe function records what went wrong on its
// link in a standard register, and a machine with nobody reading it is a machine where a marginal
// cable, a failing slot or a device that has started corrupting transactions is INVISIBLE: the
// symptom is data that is occasionally wrong, and the evidence was there the whole time. So this
// reads the record and says it.
//
// A FATAL ERROR QUARANTINES THE FUNCTION AND RESETS NOTHING. Recovery would mean a link retrain or a
// secondary-bus reset, which reaches every function behind the port - so a disk that failed would
// take a working network card with it, and a kernel doing that on its own is worse than the fault.
// Refusing to use the function again is a decision this layer can make correctly; repairing it is
// not.
//
// AND PME IS NOTICED AND NOT ACTED ON. What a wake MEANS belongs to a power policy that does not
// exist here; what a port can say is that a function behind it asked to be woken, and which one.
// ---------------------------------------------------------------------------------------------

/// The PCI Express EXTENDED capability id of Advanced Error Reporting.
const AER_EXT_CAP_ID: u16 = 0x0001;

/// Where a function's extended capability list begins. Everything before this is the 256 bytes the
/// legacy mechanism can reach.
const EXT_CAP_FIRST: u16 = 0x100;

/// A function's config space is four kilobytes, so a walk cannot pass this and a header at or past
/// it is a malformed list rather than a capability.
const EXT_CAP_LAST: u16 = 0x1000;

/// AER register offsets from the capability header.
const AER_UNCORRECTABLE_STATUS: u16 = 0x04;
const AER_UNCORRECTABLE_SEVERITY: u16 = 0x0C;
const AER_CORRECTABLE_STATUS: u16 = 0x10;

/// The PCI Express capability's Root Control and Root Status, which exist on a ROOT PORT only - the
/// same rule the slot registers follow, and for the same reason: an endpoint has the capability at
/// the same offsets and its bytes there are not these.
const PCIE_ROOT_STATUS: u16 = 0x20;
/// Root Status bit 16: a PME message arrived. RW1C, like every other event bit here.
const PCIE_ROOT_PME_STATUS: u32 = 1 << 16;
/// Root Status bits 15:0: the requester id of the function that sent it - bus, device and function.
const PCIE_ROOT_PME_REQUESTER: u32 = 0xFFFF;

/// How many functions this kernel watches for errors.
///
/// A SMALL FIXED LIST, AND THE REASON IS THE POLL. Reading one function's AER status on x86_64 is a
/// page remap and two reads; doing it for every function of every bus on every idle pass would be a
/// machine spending its idle time on config space. A machine has a handful of functions that report
/// errors at all - the root ports, and the endpoints whose firmware enabled it - and this is the list
/// of them, filled where every function is already being looked at.
pub const MAX_ERROR_REPORTERS: usize = 16;

/// The functions with an AER capability, and where it is on each. Filled by `scan` and added to when
/// a device arrives, so a hot-plugged card that reports errors is watched like any other.
static ERROR_REPORTERS: SpinLock<([Option<(u8, u8, u8)>; MAX_ERROR_REPORTERS], usize)> = SpinLock::new(([None; MAX_ERROR_REPORTERS], 0));

/// Remember a function if it carries an AER capability. Answers whether it does.
///
/// IDEMPOTENT, because a hot-plug slot's address is reused: a card plugged into the same slot twice
/// is the same bus/device/function, and a list that grew each time would fill up with one slot.
pub fn note_error_reporter<A: ConfigAccess>(bus: u8, dev: u8, func: u8) -> bool {
	if find_extended_capability::<A>(bus, dev, func, AER_EXT_CAP_ID).is_none() {
		return false;
	}
	let (mut table, mut count) = {
		let held = ERROR_REPORTERS.lock();
		(held.0, held.1)
	};
	if table.iter().take(count).flatten().any(|entry| *entry == (bus, dev, func)) {
		return true;
	}
	if count >= MAX_ERROR_REPORTERS {
		return false;
	}
	table[count] = Some((bus, dev, func));
	count += 1;
	*ERROR_REPORTERS.lock() = (table, count);
	// AN ERROR REPORTER IS POLLED, so the idle boot processor wakes for it at the housekeeping bound.
	crate::idle::add_housekeeping(crate::idle::HOUSEKEEPING_PCI_ERRORS);
	true
}

/// Forget every watched function, so a fixture can stand up its own machine.
///
/// TEST-ONLY. A running system's list grows at the scan and when a device arrives, and never shrinks:
/// a function that reported errors once is one worth watching for the life of the boot, and a slot
/// whose card was replaced is the same address. A fixture stands up a different machine per test.
#[cfg(test)]
pub fn forget_error_reporters() {
	*ERROR_REPORTERS.lock() = ([None; MAX_ERROR_REPORTERS], 0);
}

/// Read and clear every watched function's error status, answering how many records were filled.
///
/// ONLY THE FUNCTIONS THAT SAID SOMETHING. A record whose every field is zero is a function that is
/// working, and filling the caller's array with those would make finding the one that is not the
/// caller's problem.
pub fn poll_errors<A: ConfigAccess>(out: &mut [ErrorRecord; MAX_ERROR_REPORTERS]) -> usize {
	let (table, count) = {
		let held = ERROR_REPORTERS.lock();
		(held.0, held.1)
	};
	let mut filled = 0;
	for entry in table.iter().take(count).flatten() {
		let (bus, dev, func) = *entry;
		let Some(record) = take_error_record::<A>(bus, dev, func) else { continue };
		if record.is_quiet() {
			continue;
		}
		out[filled] = record;
		filled += 1;
		if filled == out.len() {
			break;
		}
	}
	filled
}

/// Read and clear every hot-plug port's PME status, answering how many events were filled.
pub fn poll_power_events<A: ConfigAccess>(out: &mut [PowerEvent; MAX_HOT_PLUG_PORTS]) -> usize {
	let (ports, count) = {
		let held = PORTS.lock();
		(held.0, held.1)
	};
	let mut filled = 0;
	for port in ports.iter().take(count).flatten() {
		let Some(event) = take_power_event::<A>(port) else { continue };
		out[filled] = event;
		filled += 1;
		if filled == out.len() {
			break;
		}
	}
	filled
}

/// One function's error record, as it was read and cleared.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ErrorRecord {
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
	/// The correctable error status bits that were set. A CORRECTABLE ERROR IS NOT A FAILURE - the
	/// hardware fixed it - but a rising count of them is a link about to stop working, which is the
	/// only warning anybody gets.
	pub correctable: u32,
	/// The uncorrectable error status bits that were set.
	pub uncorrectable: u32,
	/// Whether any uncorrectable bit that was set is FATAL by the function's own severity register.
	/// Severity is a property of the function and not of the bit, because a device may declare an
	/// error non-fatal that another treats as fatal.
	pub fatal: bool,
}

impl ErrorRecord {
	/// Whether this record says anything at all. A function with no bits set is one that is working.
	pub fn is_quiet(&self) -> bool {
		self.correctable == 0 && self.uncorrectable == 0
	}
}

/// A PME a root port received, and who sent it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PowerEvent {
	/// The port that received it.
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
	/// The requester id the port recorded: bus in 15:8, device in 7:3, function in 2:0.
	pub requester: u16,
}

/// Find an EXTENDED capability on a function, or `None`.
///
/// BOUNDED, LIKE EVERY OTHER WALK HERE, and bounded twice: the offset may not leave the function's
/// four kilobytes and the number of hops is capped, so a list that points at itself stops rather
/// than spinning. A header of all ones or all zeros is the end - an absent capability reads as ones
/// on a machine that answers reads of nothing, and as zeros on one that answers zeros.
pub fn find_extended_capability<A: ConfigAccess>(bus: u8, dev: u8, func: u8, want: u16) -> Option<u16> {
	if !A::extended_reach() {
		return None;
	}
	let mut at = EXT_CAP_FIRST;
	for _ in 0..64 {
		if at < EXT_CAP_FIRST || at >= EXT_CAP_LAST || at & 3 != 0 {
			return None;
		}
		let header = A::read32(bus, dev, func, at);
		if header == 0 || header == u32::MAX {
			return None;
		}
		if (header & 0xFFFF) as u16 == want {
			return Some(at);
		}
		let next = ((header >> 20) & 0xFFF) as u16;
		if next == 0 {
			return None;
		}
		at = next;
	}
	None
}

/// Read and CLEAR one function's AER status, or `None` when it carries no AER capability.
///
/// READ AND CLEARED IN ONE OPERATION, because the alternative is reporting the same error for ever:
/// the status bits are RW1C, so acknowledging them is writing back exactly the bits that were set.
/// Writing anything else either acknowledges an error that arrived between the read and the write -
/// losing it - or leaves a set bit alone and reports it again on the next pass, which turns one
/// marginal link into an endless stream of identical records.
pub fn take_error_record<A: ConfigAccess>(bus: u8, dev: u8, func: u8) -> Option<ErrorRecord> {
	let cap = find_extended_capability::<A>(bus, dev, func, AER_EXT_CAP_ID)?;
	let uncorrectable = A::read32(bus, dev, func, cap + AER_UNCORRECTABLE_STATUS);
	let correctable = A::read32(bus, dev, func, cap + AER_CORRECTABLE_STATUS);
	// AN ABSENT FUNCTION ANSWERS ALL ONES, and every bit set in both registers at once is a device
	// that has left the bus rather than one reporting every error there is.
	if uncorrectable == u32::MAX && correctable == u32::MAX {
		return None;
	}
	let severity = A::read32(bus, dev, func, cap + AER_UNCORRECTABLE_SEVERITY);
	if uncorrectable != 0 {
		A::write32(bus, dev, func, cap + AER_UNCORRECTABLE_STATUS, uncorrectable);
	}
	if correctable != 0 {
		A::write32(bus, dev, func, cap + AER_CORRECTABLE_STATUS, correctable);
	}
	Some(ErrorRecord { bus, dev, func, correctable, uncorrectable, fatal: uncorrectable & severity != 0 })
}

/// Read and clear a root port's PME status, or `None` when nothing woke it.
pub fn take_power_event<A: ConfigAccess>(port: &HotPlugPort) -> Option<PowerEvent> {
	let status = A::read32(port.bus, port.dev, port.func, port.slot.cap + PCIE_ROOT_STATUS);
	if status == u32::MAX || status & PCIE_ROOT_PME_STATUS == 0 {
		return None;
	}
	let requester = (status & PCIE_ROOT_PME_REQUESTER) as u16;
	// RW1C, and the REQUESTER FIELD IS NOT WRITABLE - so the acknowledgement writes the status bit
	// alone rather than the value that was read, which would put the requester id back into a
	// register that does not take one.
	A::write32(port.bus, port.dev, port.func, port.slot.cap + PCIE_ROOT_STATUS, PCIE_ROOT_PME_STATUS);
	Some(PowerEvent { bus: port.bus, dev: port.dev, func: port.func, requester })
}

/// The name of the lowest set uncorrectable error bit, for a record that has one.
///
/// THE LOWEST AND NOT ALL OF THEM, because one fault sets several: a completion timeout sets its own
/// bit and the "first error pointer" names it, and printing six names for one event reads as six
/// faults. What a reader needs is the one that happened and the raw bits beside it.
pub fn uncorrectable_name(bits: u32) -> &'static str {
	match bits.trailing_zeros() {
		4 => "a data-link protocol error",
		5 => "a surprise link-down",
		12 => "a poisoned TLP",
		13 => "a flow-control protocol error",
		14 => "a completion timeout",
		15 => "a completer abort",
		16 => "an unexpected completion",
		17 => "a receiver overflow",
		18 => "a malformed TLP",
		19 => "an ECRC failure",
		20 => "an unsupported request",
		_ => "an error this kernel has no name for",
	}
}

/// The same, for the correctable half.
pub fn correctable_name(bits: u32) -> &'static str {
	match bits.trailing_zeros() {
		0 => "a receiver error",
		6 => "a bad TLP",
		7 => "a bad DLLP",
		8 => "a replay-number rollover",
		12 => "a replay timer timeout",
		13 => "an advisory non-fatal error",
		14 => "a corrected internal error",
		15 => "a header-log overflow",
		_ => "an error this kernel has no name for",
	}
}

// One function's COMMAND register, read back.
//
// TEST-ONLY. The kernel never reads this register for its own sake - every write above is a
// read-modify-write that reads it inline - but a test that asserts what a device is allowed to do
// has to be able to see it, and asserting on the value the kernel intended to write would assert
// nothing about the bus.
#[cfg(test)]
pub fn command<A: ConfigAccess>(bus: u8, dev: u8, func: u8) -> u16 {
	A::read16(bus, dev, func, 0x04)
}

// ------------------------------------------------------------------ across a reset of every function

// WHAT AN S3 AND A HIBERNATION'S RESTORE BOTH NEED: every function's configuration as the kernel and its drivers left
// it, saved before and written back after a sleep that resets every function with the machine - on every port, through
// the port's exact-width configuration accessors.

// ONE FUNCTION'S CONFIGURATION, its first 256 bytes, as the kernel and its drivers left it before an S3 or a snapshot -
// a sleep that resets every function with the machine (QEMU's wakeup reset included), root ports and bridges among them.
struct SavedFunction {
	bus: u8,
	dev: u8,
	func: u8,
	dwords: [u32; 64],
}

static SAVED: SpinLock<Vec<SavedFunction>> = SpinLock::new(Vec::new());

// Every function the boot scan found, saved - in bus order, which is the order the restore needs: a bridge's bus
// numbers and windows go back before anything behind it is reachable.
pub fn save_config_all(config_read_exact: fn(u8, u8, u8, u16, u8) -> Option<u32>) -> bool {
	let mut addresses = crate::device::pci_addresses();
	addresses.sort_unstable();
	let mut saved = SAVED.lock();
	saved.clear();
	if saved.try_reserve(addresses.len()).is_err() {
		return false;
	}
	for (bus, dev, func) in addresses {
		let mut dwords = [0u32; 64];
		for (at, dword) in dwords.iter_mut().enumerate() {
			*dword = config_read_exact(bus, dev, func, (at * 4) as u16, 4).unwrap_or(0);
		}
		saved.push(SavedFunction { bus, dev, func, dwords });
	}
	true
}

// A saved word or byte of one function.
fn saved_word(saved: &SavedFunction, offset: u16) -> u16 {
	(saved.dwords[(offset / 4) as usize] >> ((offset % 4) * 8)) as u16
}

// THE CONFIGURATION WRITTEN BACK, the command register last so no BAR decodes before it holds its address:
//   a bridge's bus numbers, I/O, memory and prefetchable windows, ROM and bridge control; an endpoint's six BARs and
//   ROM; the interrupt line;
//   the capabilities the kernel and its drivers set - MSI's message, MSI-X's control (the table entries are the
//   kernel's `restore_msix_entries`, after this), and PCI Express's device, link, SLOT and root controls, which is
//   where a hot-plug port's presence and attention enables, its power and its indicators live.
pub fn restore_config_all(config_write_exact: fn(u8, u8, u8, u16, u8, u32) -> bool) {
	let saved = SAVED.lock();
	for function in saved.iter() {
		let (bus, dev, func) = (function.bus, function.dev, function.func);
		if function.dwords[0] == 0xFFFF_FFFF || function.dwords[0] == 0 {
			continue;
		}
		// The command register off while the addresses go back.
		config_write_exact(bus, dev, func, 0x04, 2, 0);
		let header = (function.dwords[3] >> 16) & 0x7F;
		let range: &[u16] = if header == 1 { &[0x10, 0x14, 0x18, 0x20, 0x24, 0x28, 0x2C, 0x30, 0x38] } else { &[0x10, 0x14, 0x18, 0x1C, 0x20, 0x24, 0x30] };
		for &offset in range {
			config_write_exact(bus, dev, func, offset, 4, function.dwords[(offset / 4) as usize]);
		}
		if header == 1 {
			// I/O base and limit, not the secondary status beside them, whose bits clear on a write of one.
			config_write_exact(bus, dev, func, 0x1C, 2, saved_word(function, 0x1C) as u32);
			config_write_exact(bus, dev, func, 0x3E, 2, saved_word(function, 0x3E) as u32);
		}
		config_write_exact(bus, dev, func, 0x3C, 1, function.dwords[0x3C / 4] & 0xFF);
		// THE CAPABILITIES, walked in the saved image.
		if (function.dwords[1] >> 16) & 0x10 != 0 {
			let mut at = (function.dwords[0x34 / 4] & 0xFC) as u16;
			let mut hops = 0;
			while at >= 0x40 && at < 0x100 && hops < 48 {
				hops += 1;
				let id = (function.dwords[(at / 4) as usize] >> ((at % 4) * 8)) as u8;
				match id {
					// MSI: the message address, data and control, in that order.
					0x05 => {
						let control = saved_word(function, at + 2);
						let wide = control & (1 << 7) != 0;
						config_write_exact(bus, dev, func, at + 4, 4, function.dwords[((at + 4) / 4) as usize]);
						if wide {
							config_write_exact(bus, dev, func, at + 8, 4, function.dwords[((at + 8) / 4) as usize]);
							config_write_exact(bus, dev, func, at + 12, 2, saved_word(function, at + 12) as u32);
						} else {
							config_write_exact(bus, dev, func, at + 8, 2, saved_word(function, at + 8) as u32);
						}
						config_write_exact(bus, dev, func, at + 2, 2, control as u32);
					}
					// MSI-X: its control, the function mask kept until the kernel has written the entries again.
					0x11 => {
						config_write_exact(bus, dev, func, at + 2, 2, saved_word(function, at + 2) as u32);
					}
					// PCI EXPRESS: device, link, slot and root control, and the second device and link controls.
					0x10 => {
						let flags = saved_word(function, at + 2);
						config_write_exact(bus, dev, func, at + 0x08, 2, saved_word(function, at + 0x08) as u32);
						config_write_exact(bus, dev, func, at + 0x10, 2, saved_word(function, at + 0x10) as u32 & !(1 << 5));
						if flags & (1 << 8) != 0 {
							config_write_exact(bus, dev, func, at + 0x18, 2, saved_word(function, at + 0x18) as u32);
						}
						let port = (flags >> 4) & 0xF;
						if port == 4 || port == 0xA {
							config_write_exact(bus, dev, func, at + 0x1C, 2, saved_word(function, at + 0x1C) as u32);
						}
						if flags & 0xF >= 2 {
							config_write_exact(bus, dev, func, at + 0x28, 2, saved_word(function, at + 0x28) as u32);
							config_write_exact(bus, dev, func, at + 0x30, 2, saved_word(function, at + 0x30) as u32);
						}
					}
					_ => {}
				}
				at = ((function.dwords[(at / 4) as usize] >> ((at % 4) * 8 + 8)) & 0xFC) as u16;
			}
		}
		config_write_exact(bus, dev, func, 0x04, 2, saved_word(function, 0x04) as u32);
	}
}

#[cfg(test)]
mod tests;
