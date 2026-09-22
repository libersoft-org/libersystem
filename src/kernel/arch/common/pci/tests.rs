// KERN-ARCH-015: BAR assignment does not hand out memory a device already decodes, and a device
// whose BARs could not all be placed is not switched on.
//
// Driven through a synthetic `ConfigAccess`: the trait IS the bus, so a device with any BAR layout
// can be stood up here - which is the only way to reach the layouts QEMU does not produce.

use super::{ConfigAccess, PciDevice, SLOT_CAP_HOT_PLUG, SLOT_CAP_POWER_CONTROLLER, SLOT_CONTROL_ATTENTION_BUTTON_ENABLE, SLOT_CONTROL_HOT_PLUG_INTERRUPT_ENABLE, SLOT_CONTROL_POWER_INDICATOR, SLOT_CONTROL_POWER_INDICATOR_OFF, SLOT_CONTROL_POWER_INDICATOR_ON, SLOT_CONTROL_POWER_OFF, SLOT_CONTROL_PRESENCE_CHANGED_ENABLE, SLOT_STATUS_ATTENTION_BUTTON, SLOT_STATUS_PRESENCE_CHANGED, SLOT_STATUS_PRESENT, SlotEvent, assign_bars_ecam, resolve_slot, slot_acknowledge, slot_acknowledge_button, slot_arm, slot_event, slot_power_off, slot_power_on};
use crate::sync::SpinLock;

const WINDOW_BASE: u64 = 0x1000_0000;
// Deliberately small - 4 MB - so a window that runs out is one line of setup away. It is no
// smaller because a BRIDGE window cannot be: the base and limit registers name whole megabytes, so
// a platform window under one could never hold a bridge, and the fixture would be unable to
// express the machine these tests are about.
const WINDOW_END: u64 = 0x1040_0000;

struct Space {
	bar: [u32; 6],
	// What an all-ones write reads back: the size mask, or zero for a slot the device does not
	// implement. This is the whole of what a BAR probe can learn.
	mask: [u32; 6],
	// The COMMAND dword: command in the low half, status in the high half, because that is one
	// dword in config space and the kernel reads the status through it.
	command: u32,
	// The rest of config space, byte-addressable, so a capability chain can be laid out here the
	// way a device lays one out. Offsets 0x04 and 0x10..0x27 are answered from the fields above.
	cfg: [u8; 256],
	// EXTENDED config space: offsets 0x100..0x200, where every PCIe extended capability lives.
	//
	// A SEPARATE ARRAY AND NOT A LONGER ONE, because the two halves are reached by different
	// MECHANISMS and a fixture that made them one would test a machine nobody has: the legacy port
	// pair cannot address past 0xFF at all, so a backend that answered 0x104 out of a 4 kB array
	// would be modelling an ECAM machine while claiming to be the other one.
	ext: [u8; 256],
	// Whether this fixture's mechanism can reach that half. The one thing a walk has to ask first.
	ext_reach: bool,
	// Which EXTENDED dwords are write-one-to-clear. The AER status registers are, and the whole
	// point of reading one is that reading and clearing it are one operation - a fixture that stored
	// the write would make the correct acknowledgement look like setting every bit.
	ext_rw1c: [u16; 4],
	// WHICH HALF-WORD IS WRITE-ONE-TO-CLEAR, or zero for none.
	//
	// A FIXTURE THAT STORES WHAT IT IS GIVEN CANNOT TEST AN RW1C REGISTER, and Slot Status is one:
	// writing a ONE clears the bit. A fake that simply stored the write would make the correct
	// acknowledgement look like SETTING the bit and the incorrect one - masking it out and writing
	// back - look like clearing it, which is the defect exactly upside down. So the one behaviour
	// these tests are about is modelled here rather than assumed away.
	rw1c: u16,
	next: u64,
	// A PCI-TO-PCI BRIDGE ON THE SAME FAKE BUS, when a test stands one up.
	//
	// A SECOND ADDRESS AND NOT A SECOND FIXTURE, because what these tests are about is one walk
	// reading BOTH: the kernel is not told where the bridge is, it SCANS for the one that forwards
	// the bus its device is on. So the fixture has to answer for two addresses and to answer
	// `absent` - all ones - for every other one, which is the only thing that makes a scan a scan.
	bridge: Option<[u8; 64]>,
}

// The device every test that needs one bus stands up, the bridge, and the address behind it.
const DEVICE_AT: (u8, u8, u8) = (0, 0, 0);
const BRIDGE_AT: (u8, u8, u8) = (0, 2, 0);
const BEHIND_AT: (u8, u8, u8) = (1, 0, 0);

static SPACE: SpinLock<Space> = SpinLock::new(Space { bar: [0; 6], mask: [0; 6], command: 0, cfg: [0; 256], ext: [0; 256], ext_reach: false, ext_rw1c: [0; 4], rw1c: 0, next: WINDOW_BASE, bridge: None });

// Put a device on the fake bus: `bar` is what the firmware left in each slot, `mask` what each
// slot answers a probe with.
fn stand_up(bar: [u32; 6], mask: [u32; 6]) {
	let mut space = SPACE.lock();
	space.bar = bar;
	space.mask = mask;
	space.command = 0;
	space.cfg = [0; 256];
	space.ext = [0; 256];
	space.ext_reach = false;
	space.ext_rw1c = [0; 4];
	space.rw1c = 0;
	space.next = WINDOW_BASE;
	space.bridge = None;
	drop(space);
	super::forget_apertures();
}

// Put a bridge on the fake bus, forwarding one bus, with `window` in its memory base/limit dword.
// `window` is what firmware left there: `BRIDGE_CLOSED` for a bridge nobody opened.
fn stand_up_bridge(secondary: u8, subordinate: u8, window: u32) {
	let mut cfg = [0u8; 64];
	cfg[0x00..0x04].copy_from_slice(&0x000c_1b36u32.to_le_bytes()); // a QEMU PCIe root port
	cfg[0x0e] = 0x01; // header type 1: a PCI-to-PCI bridge
	cfg[0x18] = 0; // primary bus
	cfg[0x19] = secondary;
	cfg[0x1a] = subordinate;
	cfg[0x20..0x24].copy_from_slice(&window.to_le_bytes());
	SPACE.lock().bridge = Some(cfg);
}

// What a bridge holds out of reset: base 0xfff0_0000 above limit 0x0000_0000, which is a window
// that forwards nothing. It is also what firmware leaves behind a slot that was empty when it
// looked, which is the case the hot-plug path runs into.
const BRIDGE_CLOSED: u32 = 0x0000_FFF0;

// A bridge's window as programmed: `base` and `last` are addresses, and the register keeps the top
// twelve bits of each.
const fn bridge_window(base: u64, last: u64) -> u32 {
	((base >> 16) as u32 & 0xFFF0) | (((last >> 16) as u32 & 0xFFF0) << 16)
}

// What the bridge's memory window reads back, and its command register.
fn bridge_cfg(off: usize) -> u32 {
	let space = SPACE.lock();
	let cfg = space.bridge.expect("a bridge is standing");
	u32::from_le_bytes([cfg[off], cfg[off + 1], cfg[off + 2], cfg[off + 3]])
}

// The slot is emptied and filled again: the same device, arriving with its BARs unprogrammed the
// way anything plugged into a live machine does. The bridge and its aperture stay, because the
// machine's topology is what outlives a device.
fn replug() {
	let mut space = SPACE.lock();
	space.bar = [0; 6];
	space.command = 0;
}

// One virtio vendor capability, as a device lays it out: id, next pointer, length, type, BAR
// index, then the offset and length of the structure inside that BAR.
struct Cap {
	cfg_type: u8,
	bar: u8,
	offset: u32,
	length: u32,
}

// Lay a capability chain out in config space and turn the capability-list status bit on, so the
// resolver walks it exactly as it walks a real device's.
fn stand_up_caps(caps: &[Cap]) {
	let mut space = SPACE.lock();
	space.command = (STATUS_CAP_LIST as u32) << 16;
	space.cfg[0x34] = 0x40; // capability pointer
	let mut at = 0x40usize;
	for (index, cap) in caps.iter().enumerate() {
		let next = if index + 1 == caps.len() { 0 } else { at + 0x20 };
		space.cfg[at] = 0x09; // PCI_CAP_ID_VNDR
		space.cfg[at + 1] = next as u8;
		space.cfg[at + 2] = 0x14; // capability length
		space.cfg[at + 3] = cap.cfg_type;
		space.cfg[at + 4] = cap.bar;
		space.cfg[at + 8..at + 12].copy_from_slice(&cap.offset.to_le_bytes());
		space.cfg[at + 12..at + 16].copy_from_slice(&cap.length.to_le_bytes());
		at += 0x20;
	}
}

const STATUS_CAP_LIST: u16 = 1 << 4;
const VIRTIO_CAP_COMMON: u8 = 1;
const VIRTIO_CAP_NOTIFY: u8 = 2;
const VIRTIO_CAP_ISR: u8 = 3;
const VIRTIO_CAP_DEVICE: u8 = 4;

struct Fake;

impl ConfigAccess for Fake {
	const BUS_COUNT: u16 = 1;
	const MMIO_WINDOW_END: u64 = WINDOW_END;

	fn extended_reach() -> bool {
		SPACE.lock().ext_reach
	}

	fn read32(bus: u8, dev: u8, func: u8, off: u16) -> u32 {
		let space = SPACE.lock();
		// ONE FUNCTION IS NOT A BUS. Every address other than the two this fixture stands devices
		// up at answers ALL ONES, which is what an absent function answers and what stops a scan.
		if (bus, dev, func) == BRIDGE_AT {
			let at = off as usize;
			return match space.bridge {
				Some(cfg) if at + 4 <= cfg.len() => u32::from_le_bytes([cfg[at], cfg[at + 1], cfg[at + 2], cfg[at + 3]]),
				Some(_) => 0,
				None => u32::MAX,
			};
		}
		if (bus, dev, func) != DEVICE_AT && (bus, dev, func) != BEHIND_AT {
			return u32::MAX;
		}
		// A MECHANISM THAT CANNOT REACH THIS HALF DOES NOT ANSWER NOTHING - it answers the register
		// 0x100 bytes BELOW, because the address wraps in eight bits. That is the defect the reach
		// question exists to prevent, and a fixture that answered zeros here would hide it.
		if off >= 0x100 {
			if !space.ext_reach {
				let at = (off & 0xFC) as usize;
				return u32::from_le_bytes([space.cfg[at], space.cfg[at + 1], space.cfg[at + 2], space.cfg[at + 3]]);
			}
			let at = (off as usize) - 0x100;
			if at + 4 > space.ext.len() {
				return u32::MAX;
			}
			return u32::from_le_bytes([space.ext[at], space.ext[at + 1], space.ext[at + 2], space.ext[at + 3]]);
		}
		match off {
			0x04 => space.command,
			0x10..=0x24 => space.bar[((off - 0x10) / 4) as usize],
			off if (off as usize) + 4 <= space.cfg.len() => {
				let at = off as usize;
				u32::from_le_bytes([space.cfg[at], space.cfg[at + 1], space.cfg[at + 2], space.cfg[at + 3]])
			}
			_ => 0,
		}
	}

	fn write32(bus: u8, dev: u8, func: u8, off: u16, val: u32) {
		let mut space = SPACE.lock();
		if (bus, dev, func) == BRIDGE_AT {
			let at = off as usize;
			if let Some(cfg) = space.bridge.as_mut()
				&& at + 4 <= cfg.len()
			{
				cfg[at..at + 4].copy_from_slice(&val.to_le_bytes());
			}
			return;
		}
		if (bus, dev, func) != DEVICE_AT && (bus, dev, func) != BEHIND_AT {
			return;
		}
		if off >= 0x100 {
			if !space.ext_reach {
				return;
			}
			let at = (off as usize) - 0x100;
			if at + 4 > space.ext.len() {
				return;
			}
			let rw1c = space.ext_rw1c.contains(&(off & !3));
			let mut bytes = val.to_le_bytes();
			if rw1c {
				// WRITE ONE TO CLEAR, over the whole dword: a one clears, a zero leaves it alone.
				for i in 0..4 {
					bytes[i] = space.ext[at + i] & !bytes[i];
				}
			}
			space.ext[at..at + 4].copy_from_slice(&bytes);
			return;
		}
		match off {
			// THE STATUS HALF IS NOT STORED. Its bits are write-one-to-clear, so a zero written
			// there changes nothing at all - which is exactly what every writer of this register
			// puts there. A fixture that stored the write made an ordinary command-register write
			// look as though it had wiped the device's status, and the capability-list bit with it.
			0x04 => space.command = (val & 0xFFFF) | (space.command & 0xFFFF_0000),
			0x10..=0x24 => {
				let index = ((off - 0x10) / 4) as usize;
				// A device answers an all-ones write with its size mask and stores anything else.
				// That is what makes a probe a probe, and what the kernel's restore relies on.
				let mask = space.mask[index];
				space.bar[index] = if val == 0xFFFF_FFFF { mask } else { val };
			}
			// THE REST OF CONFIG SPACE IS WRITABLE, which it was not - every write to a capability
			// was dropped, so a test could read a chain a device laid out and never see what the
			// kernel wrote back into one.
			off if (off as usize) + 4 <= space.cfg.len() => {
				let at = off as usize;
				let rw1c = space.rw1c;
				let mut bytes = val.to_le_bytes();
				if rw1c != 0 && (rw1c & !3) == off {
					// The addressed dword holds the write-one-to-clear half: a one CLEARS, a zero
					// leaves the bit alone, and the other half of the dword is stored as written.
					let half = (rw1c & 2) as usize;
					for i in 0..2 {
						bytes[half + i] = space.cfg[at + half + i] & !bytes[half + i];
					}
				}
				space.cfg[at..at + 4].copy_from_slice(&bytes);
			}
			_ => {}
		}
	}

	// The same bump the two ECAM backends use, over the small window above.
	fn alloc_mmio(size: u64) -> Option<u64> {
		let size = size.max(0x1000);
		let mut space = SPACE.lock();
		let base = (space.next + size - 1) & !(size - 1);
		if base.checked_add(size)? > WINDOW_END {
			return None;
		}
		space.next = base + size;
		Some(base)
	}

	// THE FIXTURE PLACES ITS OWN BARS, as the two ports with no firmware do. It did not, so every
	// device resolved through it had a BAR reading zero - and a resolver that took zero for an
	// address was a resolver these tests could never catch out.
	fn assign_bars(d: &PciDevice) {
		assign_bars_ecam::<Self>(d);
	}

	fn reserve_mmio(base: u64, size: u64) {
		let end = base.saturating_add(size);
		if end <= WINDOW_BASE || base >= WINDOW_END {
			return;
		}
		let mut space = SPACE.lock();
		if space.next < end {
			space.next = end;
		}
	}
}

const DEVICE: PciDevice = PciDevice { bus: 0, dev: 0, func: 0, vendor: 0x1af4, device_id: 0x1000, class: 0x02, subclass: 0x00, prog_if: 0x00, header_type: 0 };

// A 32-bit memory BAR of `size` bytes: the mask a probe reads back.
const fn mask32(size: u32) -> u32 {
	!(size - 1)
}

fn command() -> u16 {
	SPACE.lock().command as u16
}

fn bar(index: usize) -> u32 {
	SPACE.lock().bar[index]
}

crate::tagged_test!(a_bar_the_firmware_placed_is_not_handed_out_to_another, [Kernel, Pci], id = "kernel.arch.common.pci.a_bar_the_firmware_placed_is_not_handed_out_to_another", covers = ["kernel"]);
fn a_bar_the_firmware_placed_is_not_handed_out_to_another() {
	// KERN-ARCH-015. A BAR the firmware had already placed inside the window was kept - correctly -
	// and never told to the allocator, whose cursor still stood at the bottom of the window. The
	// next unprogrammed BAR was then handed the same addresses. Two apertures, one span, and
	// nothing either device can report: whichever decodes first answers, and the other's driver
	// reads someone else's registers.
	//
	// BAR0 is retained at the very bottom of the window, which is exactly where the bump would
	// otherwise start; BAR1 is unprogrammed and the same size.
	stand_up([WINDOW_BASE as u32, 0, 0, 0, 0, 0], [mask32(0x1000), mask32(0x1000), 0, 0, 0, 0]);
	assign_bars_ecam::<Fake>(&DEVICE);

	assert_eq!(bar(0) & 0xFFFF_FFF0, WINDOW_BASE as u32, "the retained BAR was left where the firmware put it");
	let placed = (bar(1) & 0xFFFF_FFF0) as u64;
	assert_ne!(placed, WINDOW_BASE, "and the unprogrammed BAR did not land on top of it");
	assert!(placed >= WINDOW_BASE + 0x1000 && placed < WINDOW_END, "it landed inside the window, past the retained span: {placed:#x}");
	assert!(command() & 0x02 != 0, "both BARs are placed, so the device decodes memory");
	assert_eq!(command() & 0x04, 0, "and enumeration is not ownership, so it is not mastering the bus");
}

crate::tagged_test!(enumeration_takes_bus_mastering_away_from_a_device_that_arrived_with_it_on, [Kernel, Pci], id = "kernel.arch.common.pci.enumeration_takes_bus_mastering_away_from_a_device_that_arrived_with_it_on", covers = ["kernel"]);
fn enumeration_takes_bus_mastering_away_from_a_device_that_arrived_with_it_on() {
	// Enumeration used to OR the bit in and never clear it, so a device the firmware had already
	// set mastering on stayed a bus master from boot until the machine stopped - with no driver, no
	// IOMMU, and therefore write access to every physical address in the machine. Leaving the bit
	// alone is not neutral: the kernel is the only thing that will ever look.
	stand_up([0, 0, 0, 0, 0, 0], [mask32(0x1000), 0, 0, 0, 0, 0]);
	SPACE.lock().command = 0x06; // memory space + bus master, as the firmware left it
	assign_bars_ecam::<Fake>(&DEVICE);
	assert!(command() & 0x02 != 0, "the device still decodes memory - its BAR is placed");
	assert_eq!(command() & 0x04, 0, "but it does not master the bus, whatever it arrived with");
}

crate::tagged_test!(granting_bus_mastering_disturbs_no_other_command_bit, [Kernel, Pci], id = "kernel.arch.common.pci.granting_bus_mastering_disturbs_no_other_command_bit", covers = ["kernel"]);
fn granting_bus_mastering_disturbs_no_other_command_bit() {
	// The grant runs against a device that is already set up - BARs placed, memory decoding on,
	// INTx pin disabled - so writing the command register wholesale would undo that setup at the
	// moment a driver takes the device. It is one bit, read-modify-write, and nothing else.
	stand_up([0, 0, 0, 0, 0, 0], [mask32(0x1000), 0, 0, 0, 0, 0]);
	assign_bars_ecam::<Fake>(&DEVICE);
	super::set_intx_disabled::<Fake>(DEVICE.bus, DEVICE.dev, DEVICE.func, true);
	let before = command();
	super::set_bus_master::<Fake>(DEVICE.bus, DEVICE.dev, DEVICE.func, true);
	assert_eq!(command(), before | 0x04, "the grant sets exactly the one bit");
	super::set_bus_master::<Fake>(DEVICE.bus, DEVICE.dev, DEVICE.func, false);
	assert_eq!(command(), before, "and revoking it puts the register back where it was");
}

crate::tagged_test!(a_device_whose_bar_will_not_fit_is_left_switched_off, [Kernel, Pci], id = "kernel.arch.common.pci.a_device_whose_bar_will_not_fit_is_left_switched_off", covers = ["kernel"]);
fn a_device_whose_bar_will_not_fit_is_left_switched_off() {
	// The other half of -015. When the window had nothing left, the allocation simply did not
	// happen - and the command register was written anyway, three lines later, unconditionally.
	// The device was told to decode memory and to master the bus with a BAR still reading zero:
	// responding at address zero, and a bus master with no aperture of its own.
	//
	// One BAR larger than the whole 4 MB window.
	stand_up([0, 0, 0, 0, 0, 0], [mask32(0x80_0000), 0, 0, 0, 0, 0]);
	assign_bars_ecam::<Fake>(&DEVICE);
	assert_eq!(bar(0) & 0xFFFF_FFF0, 0, "there was nowhere to put it");
	assert_eq!(command() & 0x02, 0, "so memory decoding stays off");
	assert_eq!(command() & 0x04, 0, "and so does bus mastering");
}

crate::tagged_test!(an_unimplemented_bar_is_not_a_four_gigabyte_request, [Kernel, Pci], id = "kernel.arch.common.pci.an_unimplemented_bar_is_not_a_four_gigabyte_request", covers = ["kernel"]);
fn an_unimplemented_bar_is_not_a_four_gigabyte_request() {
	// A slot the device does not implement answers a probe with zero, and there is no other signal.
	// Read as a size that works out to 4 GiB - which is what the arithmetic gives for a 32-bit slot
	// - every virtio device on this bus has three or four of them, and once a failed allocation
	// stops a device being enabled, mistaking one for a real BAR would switch the whole bus off.
	stand_up([0, 0, 0, 0, 0, 0], [mask32(0x1000), 0, 0, 0, 0, 0]);
	assign_bars_ecam::<Fake>(&DEVICE);
	assert_eq!((bar(0) & 0xFFFF_FFF0) as u64, WINDOW_BASE, "the one real BAR is placed");
	assert_eq!(bar(1), 0, "and the empty slots are left alone");
	assert!(command() & 0x02 != 0, "a device with unused BAR slots is still a working device");
}

crate::tagged_test!(a_sixty_four_bit_bar_takes_two_slots_and_both_halves_are_written, [Kernel, Pci], id = "kernel.arch.common.pci.a_sixty_four_bit_bar_takes_two_slots_and_both_halves_are_written", covers = ["kernel"]);
fn a_sixty_four_bit_bar_takes_two_slots_and_both_halves_are_written() {
	// A 64-bit BAR is one aperture across two slots (type bits 0b10 in the low dword). Walking it
	// as two would probe the high half as an address of its own, and reserving it as two would
	// take the span out of the window twice.
	//
	// BAR0/1 are the 64-bit pair, BAR2 a 32-bit BAR that must land after it.
	// Bits [2:1] = 0b10 is the 64-bit type code, so the low dword reads 0b100.
	stand_up([0b100, 0, 0, 0, 0, 0], [mask32(0x2000) | 0b100, 0xFFFF_FFFF, mask32(0x1000), 0, 0, 0]);
	assign_bars_ecam::<Fake>(&DEVICE);
	let low = (bar(0) & 0xFFFF_FFF0) as u64;
	assert_eq!(low, WINDOW_BASE, "the 64-bit BAR takes the bottom of the window");
	assert_eq!(bar(1), 0, "its high half is written, and the window is below 4 GB");
	assert_eq!(bar(0) & 0xF, 0b100, "the type bits survive the write");
	let third = (bar(2) & 0xFFFF_FFF0) as u64;
	assert!(third >= low + 0x2000, "the next BAR starts past the whole 8 kB aperture, not past its first half: {third:#x}");
	assert!(command() & 0x02 != 0, "everything was placed");
}

// The function a bridge forwards to, which is where a hot-plug slot puts what is plugged into it:
// function zero of device zero on the bridge's secondary bus.
const BEHIND: PciDevice = PciDevice { bus: 1, dev: 0, func: 0, vendor: 0x1af4, device_id: 0x1000, class: 0x02, subclass: 0x00, prog_if: 0x00, header_type: 0 };

crate::tagged_test!(a_bar_that_could_not_be_placed_is_not_physical_zero, [Kernel, Pci], id = "kernel.arch.common.pci.a_bar_that_could_not_be_placed_is_not_physical_zero", covers = ["kernel"]);
fn a_bar_that_could_not_be_placed_is_not_physical_zero() {
	// A BAR nobody placed reads back zero, and zero was read as an address.
	//
	// THE FAILURE IS NOT THAT THE DEVICE DOES NOT WORK. The driver is handed a writable window over
	// the bottom of physical memory, and a virtio handshake performed against RAM reads back every
	// value it writes: the reset is acknowledged because the zero written is the zero read, and
	// FEATURES_OK is accepted because the byte is still there. The device reports itself ONLINE
	// with nothing having answered it - which is how a device plugged into a live machine looked
	// bound on one port while the same device on another said honestly that it was not responding.
	//
	// One BAR larger than the whole window, so it cannot be placed and stays at zero.
	stand_up([0, 0, 0, 0, 0, 0], [mask32(0x80_0000), 0, 0, 0, 0, 0]);
	stand_up_caps(&[
		Cap { cfg_type: VIRTIO_CAP_COMMON, bar: 0, offset: 0, length: 0x38 },
		Cap { cfg_type: VIRTIO_CAP_NOTIFY, bar: 0, offset: 0x1000, length: 0x100 },
		Cap { cfg_type: VIRTIO_CAP_ISR, bar: 0, offset: 0x2000, length: 4 },
	]);
	assert!(super::resolve_virtio::<Fake>(&DEVICE).is_none(), "a device whose window could not be placed is left unclaimed");
	assert_eq!(bar(0) & 0xFFFF_FFF0, 0, "its BAR is still unplaced");
	assert!(super::bar_address::<Fake>(&DEVICE, 0).is_none(), "and the address of an unplaced BAR is no address, not zero");
}

crate::tagged_test!(a_device_behind_a_bridge_is_placed_where_the_bridge_forwards, [Kernel, Pci], id = "kernel.arch.common.pci.a_device_behind_a_bridge_is_placed_where_the_bridge_forwards", covers = ["kernel"]);
fn a_device_behind_a_bridge_is_placed_where_the_bridge_forwards() {
	// A BRIDGE FORWARDS AN ADDRESS RANGE, AND AN ADDRESS OUTSIDE IT REACHES NOTHING.
	//
	// BARs were placed out of the platform window whatever bus the device was on, which is correct
	// for a root bus and unreachable behind a bridge: the address is perfectly valid, the device
	// decodes it, and every read of it returns all ones because the bridge in front never forwards
	// the transaction. What the driver reports is a device that is not responding, which is true
	// and says nothing about why.
	//
	// The bridge starts as one out of reset: a base above its limit, forwarding nothing.
	stand_up([0, 0, 0, 0, 0, 0], [mask32(0x1000), 0, 0, 0, 0, 0]);
	stand_up_bridge(1, 1, BRIDGE_CLOSED);
	super::place_bars::<Fake>(&BEHIND);

	let window = bridge_cfg(0x20);
	assert_ne!(window, BRIDGE_CLOSED, "a bridge that forwarded nothing was given a window");
	let base = ((window & 0xFFF0) as u64) << 16;
	let last = ((((window >> 16) & 0xFFF0) as u64) << 16) | 0xF_FFFF;
	assert!(base >= WINDOW_BASE && last < WINDOW_END, "and the window came out of the platform's own addresses: {base:#x}..{last:#x}");

	let placed = (bar(0) & 0xFFFF_FFF0) as u64;
	assert!(placed >= base && placed <= last, "the BAR is inside the range its bridge forwards: {placed:#x} in {base:#x}..{last:#x}");
	assert!(bridge_cfg(0x04) & 0x02 != 0, "and the bridge decodes memory, or it forwards nothing whatever its window says");
	assert!(command() & 0x02 != 0, "the device is switched on");
}

crate::tagged_test!(a_window_the_firmware_left_open_is_the_window_a_bar_goes_into, [Kernel, Pci], id = "kernel.arch.common.pci.a_window_the_firmware_left_open_is_the_window_a_bar_goes_into", covers = ["kernel"]);
fn a_window_the_firmware_left_open_is_the_window_a_bar_goes_into() {
	// FIRMWARE THAT PADS A HOT-PLUG BRIDGE LEAVES THE ROOM FOR A LATER ARRIVAL IN THE WINDOW, and
	// on a port whose firmware places the BARs this kernel hands out no addresses of its own at
	// all - so the padded window is the only source of an address a device added to a live machine
	// can be reached at. Opening a second window over the platform's addresses would be this kernel
	// telling the bridge to forward a range firmware had not reserved for it.
	const BASE: u64 = WINDOW_BASE + 0x20_0000;
	const LAST: u64 = BASE + 0x20_0000 - 1;
	stand_up([0, 0, 0, 0, 0, 0], [mask32(0x1000), 0, 0, 0, 0, 0]);
	stand_up_bridge(1, 1, bridge_window(BASE, LAST));
	let before = bridge_cfg(0x20);
	super::place_bars::<Fake>(&BEHIND);

	assert_eq!(bridge_cfg(0x20), before, "the window firmware programmed is the window, unchanged");
	let placed = (bar(0) & 0xFFFF_FFF0) as u64;
	assert!(placed >= BASE && placed <= LAST, "and the BAR went into it: {placed:#x}");
	assert_ne!(placed, WINDOW_BASE, "rather than out of the platform window, where the bump was standing");
}

crate::tagged_test!(a_bar_inside_a_bridge_window_is_not_handed_out_to_another, [Kernel, Pci], id = "kernel.arch.common.pci.a_bar_inside_a_bridge_window_is_not_handed_out_to_another", covers = ["kernel"]);
fn a_bar_inside_a_bridge_window_is_not_handed_out_to_another() {
	// KERN-ARCH-015, one level down. A bridge's window is a second allocator and it inherits the
	// same rule: what is already placed inside it was never told to it, so a cursor starting at the
	// base would hand those addresses out again. Two apertures, one span, and nothing either device
	// can report - whichever decodes first answers, and the other's driver reads someone else's
	// registers.
	//
	// BAR0 sits at the bottom of the bridge's window already; BAR1 is unprogrammed and the same size.
	const BASE: u64 = WINDOW_BASE + 0x20_0000;
	const LAST: u64 = BASE + 0x20_0000 - 1;
	stand_up([BASE as u32, 0, 0, 0, 0, 0], [mask32(0x1000), mask32(0x1000), 0, 0, 0, 0]);
	stand_up_bridge(1, 1, bridge_window(BASE, LAST));
	super::place_bars::<Fake>(&BEHIND);

	assert_eq!((bar(0) & 0xFFFF_FFF0) as u64, BASE, "the placed BAR was left where it was");
	let placed = (bar(1) & 0xFFFF_FFF0) as u64;
	assert!(placed >= BASE + 0x1000 && placed <= LAST, "and the unprogrammed one did not land on top of it: {placed:#x}");
}

crate::tagged_test!(a_slot_emptied_and_filled_again_gets_its_addresses_back, [Kernel, Pci], id = "kernel.arch.common.pci.a_slot_emptied_and_filled_again_gets_its_addresses_back", covers = ["kernel"]);
fn a_slot_emptied_and_filled_again_gets_its_addresses_back() {
	// A HOT-PLUG SLOT IS THE THING A PERSON USES REPEATEDLY, and the cursor in a bridge's window
	// only ever moved forward: a device that left took its addresses with it and gave nothing back,
	// so the same one device in the same one slot would eventually arrive to a window with nothing
	// left in it and be left disabled. Nothing about the machine would have changed.
	stand_up([0, 0, 0, 0, 0, 0], [mask32(0x1000), 0, 0, 0, 0, 0]);
	stand_up_bridge(1, 1, BRIDGE_CLOSED);
	super::place_bars::<Fake>(&BEHIND);
	let first = bar(0) & 0xFFFF_FFF0;
	let window = bridge_cfg(0x20);
	assert_ne!(first, 0, "the first arrival was placed");

	replug();
	super::place_bars::<Fake>(&BEHIND);
	assert_eq!(bar(0) & 0xFFFF_FFF0, first, "and the second arrival is given the addresses the first one left");
	assert_eq!(bridge_cfg(0x20), window, "the window itself is the machine's and does not move");
}

crate::tagged_test!(a_virtio_device_that_spreads_itself_over_bars_is_not_claimed, [Kernel, Pci], id = "kernel.arch.common.pci.a_virtio_device_that_spreads_itself_over_bars_is_not_claimed", covers = ["kernel"]);
fn a_virtio_device_that_spreads_itself_over_bars_is_not_claimed() {
	// KERN-ARCH-014. Each virtio capability names its OWN BAR, and the specification allows them to
	// differ. What the kernel hands a driver is one physical window plus four offsets into it, and
	// the driver adds each offset to that one base - so a structure in another BAR was read at the
	// right offset of the wrong aperture, with every access succeeding.
	//
	// First the case that must keep working: everything in BAR 0.
	stand_up([0, 0, 0, 0, 0, 0], [mask32(0x4000), 0, mask32(0x1000), 0, 0, 0]);
	stand_up_caps(&[
		Cap { cfg_type: VIRTIO_CAP_COMMON, bar: 0, offset: 0, length: 0x38 },
		Cap { cfg_type: VIRTIO_CAP_NOTIFY, bar: 0, offset: 0x1000, length: 0x100 },
		Cap { cfg_type: VIRTIO_CAP_ISR, bar: 0, offset: 0x2000, length: 4 },
		Cap { cfg_type: VIRTIO_CAP_DEVICE, bar: 0, offset: 0x3000, length: 0x40 },
	]);
	let together = super::resolve_virtio::<Fake>(&DEVICE).expect("a device whose structures share a BAR is claimed");
	assert_eq!(together.bar, 0);
	assert_eq!(together.region_len, 0x4000, "the window covers the furthest structure, rounded to a page");
	assert!(together.device.is_some(), "and its device configuration is reported");

	// Now the same device with its notify structure in BAR 2. It is left unclaimed rather than
	// claimed and driven through BAR 0's registers.
	stand_up([0, 0, 0, 0, 0, 0], [mask32(0x4000), 0, mask32(0x1000), 0, 0, 0]);
	stand_up_caps(&[
		Cap { cfg_type: VIRTIO_CAP_COMMON, bar: 0, offset: 0, length: 0x38 },
		Cap { cfg_type: VIRTIO_CAP_NOTIFY, bar: 2, offset: 0, length: 0x100 },
		Cap { cfg_type: VIRTIO_CAP_ISR, bar: 0, offset: 0x2000, length: 4 },
	]);
	assert!(super::resolve_virtio::<Fake>(&DEVICE).is_none(), "a device the kernel cannot describe in one window is not claimed");
}

crate::tagged_test!(an_absent_device_configuration_is_absent_rather_than_offset_zero, [Kernel, Pci], id = "kernel.arch.common.pci.an_absent_device_configuration_is_absent_rather_than_offset_zero", covers = ["kernel"]);
fn an_absent_device_configuration_is_absent_rather_than_offset_zero() {
	// The device-specific structure is optional, and a missing one used to be reported as
	// `VirtioCap::default()` - offset zero, length zero - which is exactly what a device with its
	// device config at the start of the window reports. The two were indistinguishable.
	stand_up([0, 0, 0, 0, 0, 0], [mask32(0x3000), 0, 0, 0, 0, 0]);
	stand_up_caps(&[
		Cap { cfg_type: VIRTIO_CAP_COMMON, bar: 0, offset: 0x1000, length: 0x38 },
		Cap { cfg_type: VIRTIO_CAP_NOTIFY, bar: 0, offset: 0x2000, length: 0x100 },
		Cap { cfg_type: VIRTIO_CAP_ISR, bar: 0, offset: 0x2100, length: 4 },
	]);
	let absent = super::resolve_virtio::<Fake>(&DEVICE).expect("a device without a device config is still a device");
	assert!(absent.device.is_none(), "there is no device configuration, and that is what is reported");

	// And a device config that really does start at offset zero is reported as one.
	stand_up([0, 0, 0, 0, 0, 0], [mask32(0x3000), 0, 0, 0, 0, 0]);
	stand_up_caps(&[
		Cap { cfg_type: VIRTIO_CAP_COMMON, bar: 0, offset: 0x1000, length: 0x38 },
		Cap { cfg_type: VIRTIO_CAP_NOTIFY, bar: 0, offset: 0x2000, length: 0x100 },
		Cap { cfg_type: VIRTIO_CAP_ISR, bar: 0, offset: 0x2100, length: 4 },
		Cap { cfg_type: VIRTIO_CAP_DEVICE, bar: 0, offset: 0, length: 0x40 },
	]);
	let at_zero = super::resolve_virtio::<Fake>(&DEVICE).expect("a device with its config at zero is claimed");
	assert_eq!(at_zero.device.map(|cap| (cap.offset, cap.length)), Some((0, 0x40)), "a structure at offset zero is a structure");
}

crate::tagged_test!(a_structure_that_runs_past_its_bar_is_not_claimed, [Kernel, Pci], id = "kernel.arch.common.pci.a_structure_that_runs_past_its_bar_is_not_claimed", covers = ["kernel"]);
fn a_structure_that_runs_past_its_bar_is_not_claimed() {
	// `offset` and `length` come from the device's own config space and were never checked against
	// the aperture they index. A structure ending past the end of the BAR is a device describing
	// memory the BAR does not decode - and the driver would map and read exactly that.
	stand_up([0, 0, 0, 0, 0, 0], [mask32(0x2000), 0, 0, 0, 0, 0]);
	stand_up_caps(&[
		Cap { cfg_type: VIRTIO_CAP_COMMON, bar: 0, offset: 0, length: 0x38 },
		Cap { cfg_type: VIRTIO_CAP_NOTIFY, bar: 0, offset: 0x1000, length: 0x100 },
		Cap { cfg_type: VIRTIO_CAP_ISR, bar: 0, offset: 0x1FFF, length: 4 },
	]);
	assert!(super::resolve_virtio::<Fake>(&DEVICE).is_none(), "the ISR structure ends one byte past an 8 kB BAR");
}

// A PCI Express capability laid out the way a port lays one out: the capabilities word, then the
// slot registers at their fixed offsets inside it.
fn stand_up_pcie(port_type: u16, slot_implemented: bool, slot_caps: u32, slot_status: u16) {
	let mut space = SPACE.lock();
	space.command = (STATUS_CAP_LIST as u32) << 16;
	space.cfg = [0; 256];
	space.cfg[0x34] = 0x40;
	let at = 0x40usize;
	space.cfg[at] = 0x10; // PCI Express capability
	space.cfg[at + 1] = 0; // end of the list
	let caps: u16 = (port_type << 4) | if slot_implemented { 1 << 8 } else { 0 };
	space.cfg[at + 2] = caps as u8;
	space.cfg[at + 3] = (caps >> 8) as u8;
	for (i, byte) in slot_caps.to_le_bytes().iter().enumerate() {
		space.cfg[at + 0x14 + i] = *byte;
	}
	for (i, byte) in slot_status.to_le_bytes().iter().enumerate() {
		space.cfg[at + 0x1A + i] = *byte;
	}
	space.rw1c = (at + 0x1A) as u16;
}

// Put a value into the slot's STATUS half without going through the write path.
//
// A TEST THAT WROTE IT THROUGH `write32` WOULD CLEAR IT, which is the whole point of the register:
// the status half is RW1C, and the fake models that faithfully - so a write of the bits a device
// arriving would set is a write that ACKNOWLEDGES them. What a fixture needs is to be the port for a
// moment, and a port sets these bits itself.
fn slot_status_is(slot_cap: u16, status: u16) {
	let mut space = SPACE.lock();
	for (i, byte) in status.to_le_bytes().iter().enumerate() {
		space.cfg[slot_cap as usize + 0x1A + i] = *byte;
	}
}

fn a_function() -> PciDevice {
	// A ROOT PORT as the bus reports one: a bridge header, and the class triple a PCI-to-PCI bridge
	// carries. What `resolve_slot` reads is the capability chain, not these - but standing up a
	// plausible function is what stops a later reader taking the fixture for a shortcut.
	PciDevice { bus: 0, dev: 0, func: 0, vendor: 0x8086, device_id: 0x3420, class: 0x06, subclass: 0x04, prog_if: 0x00, header_type: 0x01 }
}

crate::tagged_test!(slot_registers_are_only_slot_registers_on_a_port_that_has_a_slot, [Kernel, Pci], id = "kernel.arch.common.pci.slot_registers_are_only_slot_registers_on_a_port_that_has_a_slot", covers = ["kernel"]);
fn slot_registers_are_only_slot_registers_on_a_port_that_has_a_slot() {
	// THE CAPABILITY IS ON EVERY PCIe FUNCTION AND THE SLOT IS NOT. An endpoint carries the same
	// capability at the same offsets, so a reader that goes straight to Slot Status reads an
	// endpoint's reserved bytes and calls them a presence bit - a device that is always there, in a
	// slot that does not exist.
	stand_up_pcie(4, true, SLOT_CAP_HOT_PLUG | (7 << 19), 0);
	assert_eq!(resolve_slot::<Fake>(&a_function()).map(|s| s.number), Some(7), "a root port with a slot, and the platform's own slot number");

	// An endpoint (type 0) with the same bytes in the same places.
	stand_up_pcie(0, true, SLOT_CAP_HOT_PLUG | (7 << 19), 0);
	assert!(resolve_slot::<Fake>(&a_function()).is_none(), "an endpoint has no slot whatever those bytes say");

	// A root port that does not implement one.
	stand_up_pcie(4, false, SLOT_CAP_HOT_PLUG | (7 << 19), 0);
	assert!(resolve_slot::<Fake>(&a_function()).is_none(), "`Slot Implemented` is the bit that makes the rest mean anything");

	// A slot that cannot be hot-plugged will never report a change, so watching it is a loop that
	// never ends and never fires.
	stand_up_pcie(4, true, 7 << 19, 0);
	assert!(resolve_slot::<Fake>(&a_function()).is_none(), "a slot with no hot-plug capability is not one to watch");

	// A switch's downstream port carries them too.
	stand_up_pcie(6, true, SLOT_CAP_HOT_PLUG | (3 << 19), 0);
	assert_eq!(resolve_slot::<Fake>(&a_function()).map(|s| s.number), Some(3));
}

crate::tagged_test!(presence_and_the_change_are_two_questions_and_a_reader_needs_both, [Kernel, Pci], id = "kernel.arch.common.pci.presence_and_the_change_are_two_questions_and_a_reader_needs_both", covers = ["kernel"]);
fn presence_and_the_change_are_two_questions_and_a_reader_needs_both() {
	// The state says what is in the slot NOW; the change bit is STICKY and says it changed since
	// somebody cleared it. A reader watching only the state never learns a device was swapped
	// between two looks - the state is the same before and after - and one acting on the change bit
	// alone knows something happened and not which of the two things it was.
	stand_up_pcie(4, true, SLOT_CAP_HOT_PLUG, SLOT_STATUS_PRESENT);
	let slot = resolve_slot::<Fake>(&a_function()).expect("a slot");
	assert_eq!(slot_event::<Fake>(&a_function(), slot), SlotEvent::Quiet, "a device sitting there is not an event");

	stand_up_pcie(4, true, SLOT_CAP_HOT_PLUG, SLOT_STATUS_PRESENT | SLOT_STATUS_PRESENCE_CHANGED);
	assert_eq!(slot_event::<Fake>(&a_function(), slot), SlotEvent::Arrived);

	stand_up_pcie(4, true, SLOT_CAP_HOT_PLUG, SLOT_STATUS_PRESENCE_CHANGED);
	assert_eq!(slot_event::<Fake>(&a_function(), slot), SlotEvent::Departed, "changed, and nothing is there now");
}

crate::tagged_test!(the_change_bit_is_cleared_by_writing_a_one_and_the_control_half_survives_it, [Kernel, Pci], id = "kernel.arch.common.pci.the_change_bit_is_cleared_by_writing_a_one_and_the_control_half_survives_it", covers = ["kernel"]);
fn the_change_bit_is_cleared_by_writing_a_one_and_the_control_half_survives_it() {
	// RW1C IS THE OPPOSITE OF WHAT CLEARING A BIT USUALLY LOOKS LIKE. A reader that masks the bit out
	// and writes the result back leaves it exactly as it was, and every later look reports the same
	// change for ever - one plug read as thousands of arrivals, which looks like a device that keeps
	// re-appearing rather than like a bug in the acknowledgement.
	stand_up_pcie(4, true, SLOT_CAP_HOT_PLUG, SLOT_STATUS_PRESENT | SLOT_STATUS_PRESENCE_CHANGED);
	let slot = resolve_slot::<Fake>(&a_function()).expect("a slot");
	// Slot Control and Slot Status share one dword, so arming must survive the acknowledgement.
	slot_arm::<Fake>(&a_function(), slot);
	let armed = Fake::read16(0, 0, 0, slot.cap + 0x18);
	assert!(armed & SLOT_CONTROL_PRESENCE_CHANGED_ENABLE != 0 && armed & SLOT_CONTROL_HOT_PLUG_INTERRUPT_ENABLE != 0, "the slot was asked to report changes");
	slot_acknowledge::<Fake>(&a_function(), slot);
	assert_eq!(slot_event::<Fake>(&a_function(), slot), SlotEvent::Quiet, "the change is acknowledged and the next one is a new one");
	assert!(Fake::read16(0, 0, 0, slot.cap + 0x1A) & SLOT_STATUS_PRESENT != 0, "and what is IN the slot is not something an acknowledgement changes");
	assert_eq!(Fake::read16(0, 0, 0, slot.cap + 0x18), armed, "the control half of the shared dword survives the write");
}

crate::tagged_test!(an_armed_slot_is_a_powered_slot, [Kernel, Pci], id = "kernel.arch.common.pci.an_armed_slot_is_a_powered_slot", covers = ["kernel"]);
fn an_armed_slot_is_a_powered_slot() {
	// THE POWER IS THE HALF THAT IS EASY TO LEAVE OUT AND IMPOSSIBLE TO NOTICE.
	//
	// A slot with a power controller comes out of reset with the power OFF, and a port holding an
	// unpowered slot presents nothing behind it: the presence bit stays clear, the change bit never
	// sets, and the interrupt the arming just enabled never fires. Everything looks correct and
	// nothing ever happens - which from an operator's side is a machine that ignores the disk they
	// plugged into it. This is that case, and the first machine it was tried on behaved exactly so.
	stand_up_pcie(4, true, SLOT_CAP_HOT_PLUG | SLOT_CAP_POWER_CONTROLLER, 0);
	let slot = resolve_slot::<Fake>(&a_function()).expect("a slot");
	// The reset state this asserts against: power off, which is a ONE.
	Fake::write32(0, 0, 0, slot.cap + 0x18, SLOT_CONTROL_POWER_OFF as u32);
	slot_arm::<Fake>(&a_function(), slot);
	let control = Fake::read16(0, 0, 0, slot.cap + 0x18);
	assert!(control & SLOT_CONTROL_POWER_OFF == 0, "arming a slot with a power controller turns it ON - a zero is on");
	assert_eq!(control & SLOT_CONTROL_POWER_INDICATOR, SLOT_CONTROL_POWER_INDICATOR_ON, "and says so on the indicator");
	assert!(control & SLOT_CONTROL_ATTENTION_BUTTON_ENABLE != 0, "and asks to be told when somebody presses the button");
}

crate::tagged_test!(a_slot_with_no_power_controller_is_not_told_to_power_up, [Kernel, Pci], id = "kernel.arch.common.pci.a_slot_with_no_power_controller_is_not_told_to_power_up", covers = ["kernel"]);
fn a_slot_with_no_power_controller_is_not_told_to_power_up() {
	// A CAPABILITY THE SLOT DOES NOT HAVE IS NOT A BIT TO WRITE. `Power Controller Control` is
	// defined only where `Power Controller Present` says so; writing it on a slot without one is
	// writing a reserved bit, and what a port does with that is the port's business rather than
	// something a kernel may assume.
	stand_up_pcie(4, true, SLOT_CAP_HOT_PLUG, 0);
	let slot = resolve_slot::<Fake>(&a_function()).expect("a slot");
	Fake::write32(0, 0, 0, slot.cap + 0x18, SLOT_CONTROL_POWER_OFF as u32);
	slot_arm::<Fake>(&a_function(), slot);
	let control = Fake::read16(0, 0, 0, slot.cap + 0x18);
	assert!(control & SLOT_CONTROL_POWER_OFF != 0, "the bit is left exactly as it was found");
	assert!(control & SLOT_CONTROL_PRESENCE_CHANGED_ENABLE != 0, "and the arming that IS this slot's still happened");
}

crate::tagged_test!(the_attention_button_is_a_request_and_not_a_removal, [Kernel, Pci], id = "kernel.arch.common.pci.the_attention_button_is_a_request_and_not_a_removal", covers = ["kernel"]);
fn the_attention_button_is_a_request_and_not_a_removal() {
	// A MANAGED REMOVAL BEGINS WITH SOMEBODY ASKING, and the device is still there when they do.
	//
	// What the system owes in return is to stop the driver and let go of the resources BEFORE the
	// slot goes down. A reader that took the button for a departure would report a device gone while
	// its driver still held a mapping of it; one that ignored the button would only ever see the
	// surprise removal it was supposed to prepare for.
	stand_up_pcie(4, true, SLOT_CAP_HOT_PLUG | SLOT_CAP_POWER_CONTROLLER, SLOT_STATUS_PRESENT | SLOT_STATUS_ATTENTION_BUTTON);
	let slot = resolve_slot::<Fake>(&a_function()).expect("a slot");
	assert_eq!(slot_event::<Fake>(&a_function(), slot), SlotEvent::RemovalRequested, "the button is the request");
	assert!(Fake::read16(0, 0, 0, slot.cap + 0x1A) & SLOT_STATUS_PRESENT != 0, "and the device is still in the slot while it is being asked for");

	// AND IT IS ITS OWN STICKY BIT. Acknowledging the button leaves the presence change alone, and
	// acknowledging a presence change leaves the button alone - a request read on every poll is a
	// driver asked to stop a hundred times a second.
	slot_acknowledge_button::<Fake>(&a_function(), slot);
	assert_eq!(slot_event::<Fake>(&a_function(), slot), SlotEvent::Quiet, "asked once");
}

crate::tagged_test!(powering_a_slot_down_says_off_on_the_indicator_and_not_the_reserved_value, [Kernel, Pci], id = "kernel.arch.common.pci.powering_a_slot_down_says_off_on_the_indicator_and_not_the_reserved_value", covers = ["kernel"]);
fn powering_a_slot_down_says_off_on_the_indicator_and_not_the_reserved_value() {
	// OFF IS THREE AND NOT ZERO, which is the specification's spelling and not an obvious one: the
	// two indicator bits encode on, blink and off as 01, 10 and 11, and ZERO IS RESERVED. A port
	// reads the indicator as part of deciding that the guest has finished with the slot, so writing
	// the reserved value is a power-down request the port does not recognise - the slot goes dark and
	// the device stays in it. That is exactly what the first live run did.
	stand_up_pcie(4, true, SLOT_CAP_HOT_PLUG | SLOT_CAP_POWER_CONTROLLER, SLOT_STATUS_PRESENT);
	let slot = resolve_slot::<Fake>(&a_function()).expect("a slot");
	slot_arm::<Fake>(&a_function(), slot);
	slot_power_off::<Fake>(&a_function(), slot);
	let control = Fake::read16(0, 0, 0, slot.cap + 0x18);
	assert!(control & SLOT_CONTROL_POWER_OFF != 0, "the power controller is told to switch off - a one is off");
	assert_eq!(control & SLOT_CONTROL_POWER_INDICATOR, SLOT_CONTROL_POWER_INDICATOR_OFF, "and the indicator says off, which is three");
	assert!(Fake::read16(0, 0, 0, slot.cap + 0x1A) & SLOT_STATUS_PRESENT != 0, "and no sticky status bit was acknowledged on the way past");

	// AND IT COMES BACK UP, because a slot left powered down after a removal is a slot that works
	// exactly once.
	slot_power_on::<Fake>(&a_function(), slot);
	let control = Fake::read16(0, 0, 0, slot.cap + 0x18);
	assert!(control & SLOT_CONTROL_POWER_OFF == 0);
	assert_eq!(control & SLOT_CONTROL_POWER_INDICATOR, SLOT_CONTROL_POWER_INDICATOR_ON);
}

crate::tagged_test!(a_poll_answers_a_change_once_and_the_state_it_left_behind, [Kernel, Pci], id = "kernel.arch.common.pci.a_poll_answers_a_change_once_and_the_state_it_left_behind", covers = ["kernel"]);
fn a_poll_answers_a_change_once_and_the_state_it_left_behind() {
	// WHAT A POLL OWES ITS CALLER: each change answered ONCE, with the state at the moment of the
	// change, and nothing at all when nothing moved. Every one of those is a way this is written
	// wrong - a change re-reported is a device that binds a hundred times a second, and a change
	// reported with the state read at some later moment is a device reported arriving after it left.
	stand_up_pcie(4, true, SLOT_CAP_HOT_PLUG | SLOT_CAP_POWER_CONTROLLER, 0);
	// THE KERNEL'S OWN PORT TABLE IS PUT BACK WHEN THIS DROPS - see `HeldPorts`.
	let _ports = super::HeldPorts::take();
	super::arm_hot_plug_slots::<Fake>(&[a_function()]);
	let slot = resolve_slot::<Fake>(&a_function()).expect("a slot");
	let mut out = [super::SlotChange { bus: 0, dev: 0, func: 0, secondary: 0, what: SlotEvent::Quiet }; super::MAX_HOT_PLUG_PORTS];

	assert_eq!(super::poll_slots::<Fake>(&mut out), 0, "an armed slot with nothing in it reports nothing");

	// A DEVICE ARRIVES: the change bit sets and the presence bit with it.
	slot_status_is(slot.cap, SLOT_STATUS_PRESENT | SLOT_STATUS_PRESENCE_CHANGED);
	assert_eq!(super::poll_slots::<Fake>(&mut out), 1, "the arrival is reported");
	assert_eq!(out[0].what, SlotEvent::Arrived);
	assert_eq!((out[0].bus, out[0].dev, out[0].func), (0, 0, 0), "and it names the port it happened at");
	assert_eq!(super::poll_slots::<Fake>(&mut out), 0, "and it is reported once - the poll acknowledged it");

	// THE BUTTON IS A SECOND KIND OF NEWS about a slot whose state has not moved.
	slot_status_is(slot.cap, SLOT_STATUS_PRESENT | SLOT_STATUS_ATTENTION_BUTTON);
	assert_eq!(super::poll_slots::<Fake>(&mut out), 1, "somebody asked for the device to be removed");
	assert_eq!(out[0].what, SlotEvent::RemovalRequested);
	assert_eq!(super::poll_slots::<Fake>(&mut out), 0, "asked once");

	// AND A CHANGE THAT LEFT THE STATE WHERE IT WAS IS NOT NEWS. A device that came and went between
	// two reads is a device that is not there now, and there is nothing for a caller to do about it.
	slot_status_is(slot.cap, SLOT_STATUS_PRESENT | SLOT_STATUS_PRESENCE_CHANGED);
	assert_eq!(super::poll_slots::<Fake>(&mut out), 0, "the slot holds what it held at the last answer");
}

crate::tagged_test!(a_port_with_no_interrupt_pin_is_not_bound_to_a_vector, [Kernel, Pci], id = "kernel.arch.common.pci.a_port_with_no_interrupt_pin_is_not_bound_to_a_vector", covers = ["kernel"]);
fn a_port_with_no_interrupt_pin_is_not_bound_to_a_vector() {
	// THE LINE AND THE PIN ARE TWO REGISTERS AND BOTH MATTER. A function with no interrupt PIN
	// asserts nothing whatever its line says, and firmware leaves the line at `0xff` for a function
	// it routed nowhere. Both are "this port will not tell you", and a handler registered on either
	// is a handler on a vector nothing raises - which reads, from the outside, as a slot that works
	// and a device that never arrives.
	stand_up_pcie(4, true, SLOT_CAP_HOT_PLUG, 0);
	// THE KERNEL'S OWN PORT TABLE IS PUT BACK WHEN THIS DROPS - see `HeldPorts`.
	let _ports = super::HeldPorts::take();
	super::arm_hot_plug_slots::<Fake>(&[a_function()]);
	let mut ports: [Option<super::HotPlugPort>; super::MAX_HOT_PLUG_PORTS] = [None; super::MAX_HOT_PLUG_PORTS];
	assert_eq!(super::hot_plug_ports(&mut ports), 1, "the scan remembered the port");
	let port = ports[0].expect("a port");

	// No pin and no line: the reset state of the fixture.
	assert_eq!(super::slot_interrupt_line::<Fake>(&port), None, "a port that names no pin raises nothing");

	// A line with no pin is still nothing.
	Fake::write32(0, 0, 0, 0x3c, 0x0b);
	assert_eq!(super::slot_interrupt_line::<Fake>(&port), None, "a line without a pin is a line nothing asserts on");

	// `0xff` is firmware's way of saying "routed nowhere", and it is not IRQ 255.
	Fake::write32(0, 0, 0, 0x3c, 0x01ff);
	assert_eq!(super::slot_interrupt_line::<Fake>(&port), None);

	// And a real routing is answered.
	Fake::write32(0, 0, 0, 0x3c, 0x010b);
	assert_eq!(super::slot_interrupt_line::<Fake>(&port), Some(11));
}

crate::tagged_test!(powering_a_named_slot_finds_it_by_address_and_leaves_the_others_alone, [Kernel, Pci], id = "kernel.arch.common.pci.powering_a_named_slot_finds_it_by_address_and_leaves_the_others_alone", covers = ["kernel"]);
fn powering_a_named_slot_finds_it_by_address_and_leaves_the_others_alone() {
	// THE CALLER HAS AN ADDRESS AND NOT A SLOT. What completes a coordinated removal is a write to
	// one port's Slot Control, and what the caller holds by then is the bus address it was told the
	// request came from - so the lookup is by address, and an address that is not a hot-plug port is
	// a write that must not happen.
	stand_up_pcie(4, true, SLOT_CAP_HOT_PLUG | SLOT_CAP_POWER_CONTROLLER, SLOT_STATUS_PRESENT);
	// THE KERNEL'S OWN PORT TABLE IS PUT BACK WHEN THIS DROPS - see `HeldPorts`.
	let _ports = super::HeldPorts::take();
	super::arm_hot_plug_slots::<Fake>(&[a_function()]);
	let slot = resolve_slot::<Fake>(&a_function()).expect("a slot");
	let armed = Fake::read16(0, 0, 0, slot.cap + 0x18);

	super::set_slot_power::<Fake>(9, 9, 9, false);
	assert_eq!(Fake::read16(0, 0, 0, slot.cap + 0x18), armed, "an address that is not a port of this machine writes nothing");

	super::set_slot_power::<Fake>(0, 0, 0, false);
	assert!(Fake::read16(0, 0, 0, slot.cap + 0x18) & SLOT_CONTROL_POWER_OFF != 0, "and the port that IS named is powered down");
}

// ---------------------------------------------------------------------------------------------
// Advanced Error Reporting and power-management events.
// ---------------------------------------------------------------------------------------------

// Lay an AER extended capability out at 0x100, with a stated status and severity.
//
// AT 0x100 BECAUSE THAT IS WHERE THE FIRST EXTENDED CAPABILITY IS - the offset is fixed by the
// specification, and a fixture that put it elsewhere would be testing a walk that starts at the
// wrong place.
fn stand_up_aer(uncorrectable: u32, correctable: u32, severity: u32) {
	let mut space = SPACE.lock();
	space.ext = [0; 256];
	space.ext_reach = true;
	// Header: capability id in 15:0, version in 19:16, next offset in 31:20. Next is zero: the end.
	let header: u32 = 0x0001 | (2 << 16);
	space.ext[0..4].copy_from_slice(&header.to_le_bytes());
	space.ext[0x04..0x08].copy_from_slice(&uncorrectable.to_le_bytes());
	space.ext[0x0C..0x10].copy_from_slice(&severity.to_le_bytes());
	space.ext[0x10..0x14].copy_from_slice(&correctable.to_le_bytes());
	// The two STATUS registers are write-one-to-clear and the severity register is not: a reader
	// that acknowledged the severity would rewrite the function's own policy.
	space.ext_rw1c = [0x104, 0x110, 0, 0];
}

crate::tagged_test!(an_extended_capability_is_found_only_where_the_mechanism_can_reach_one, [Kernel, Pci], id = "kernel.arch.common.pci.an_extended_capability_is_found_only_where_the_mechanism_can_reach_one", covers = ["kernel"]);
fn an_extended_capability_is_found_only_where_the_mechanism_can_reach_one() {
	// THE LEGACY PORT PAIR CANNOT ADDRESS PAST 0xFF, AND THE FAILURE IS SILENT: the register number
	// is eight bits, so a read at 0x104 answers the register at 0x04 - the command and status
	// registers. A walk that started without asking would find a capability whose id is whatever
	// those bytes happen to be, on every function of every machine.
	stand_up([0; 6], [0; 6]);
	stand_up_aer(0, 0, 0);
	assert_eq!(super::find_extended_capability::<Fake>(0, 0, 0, 0x0001), Some(0x100), "a reachable capability is found where it is");

	{
		// A MARKER IN THE REGISTER 0x100 BYTES BELOW, so what the unreachable read answers is visible.
		let mut space = SPACE.lock();
		space.ext_reach = false;
		space.cfg[0x00..0x04].copy_from_slice(&0xdead_beef_u32.to_le_bytes());
	}
	assert_eq!(super::find_extended_capability::<Fake>(0, 0, 0, 0x0001), None, "and a mechanism that cannot reach that half finds nothing rather than the vendor id");
	// AND THIS IS WHY THE QUESTION HAS TO BE ASKED. The unreachable read does not answer nothing: the
	// register number WRAPS in eight bits, so a read at 0x100 answers the register at 0x00 - which on
	// a real function is the vendor and device id, and a walk that took it for a capability header
	// would find one on every function of every machine.
	assert_eq!(Fake::read32(0, 0, 0, 0x100), 0xdead_beef, "the unreachable read answers the register 0x100 bytes below rather than nothing");
}

crate::tagged_test!(a_malformed_extended_capability_list_stops_rather_than_spinning, [Kernel, Pci], id = "kernel.arch.common.pci.a_malformed_extended_capability_list_stops_rather_than_spinning", covers = ["kernel"]);
fn a_malformed_extended_capability_list_stops_rather_than_spinning() {
	// A LIST IS A DEVICE'S OWN DATA AND A DEVICE MAY BE BROKEN. Three shapes end a walk and none of
	// them may end it by running forever: a header of all ones, which is what an absent function
	// answers; a next pointer that goes BACKWARDS out of extended space; and one that points at
	// itself.
	stand_up([0; 6], [0; 6]);
	stand_up_aer(0, 0, 0);
	{
		let mut space = SPACE.lock();
		space.ext[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
	}
	assert_eq!(super::find_extended_capability::<Fake>(0, 0, 0, 0x0001), None, "a header of all ones is the end of the list");

	stand_up_aer(0, 0, 0);
	{
		// A next pointer at 0x40, which is INSIDE the legacy half: following it would read a
		// capability of the other list and report it as an extended one.
		let mut space = SPACE.lock();
		let header: u32 = 0x00FF | (2 << 16) | (0x040 << 20);
		space.ext[0..4].copy_from_slice(&header.to_le_bytes());
	}
	assert_eq!(super::find_extended_capability::<Fake>(0, 0, 0, 0x0001), None, "a next pointer below the extended half is refused rather than followed");

	stand_up_aer(0, 0, 0);
	{
		// A header that points at itself. Bounded by the hop count, so this returns at all.
		let mut space = SPACE.lock();
		let header: u32 = 0x00FF | (2 << 16) | (0x100 << 20);
		space.ext[0..4].copy_from_slice(&header.to_le_bytes());
	}
	assert_eq!(super::find_extended_capability::<Fake>(0, 0, 0, 0x0001), None, "and a list that points at itself stops instead of spinning");
}

crate::tagged_test!(an_error_record_is_read_and_cleared_in_one_pass, [Kernel, Pci], id = "kernel.arch.common.pci.an_error_record_is_read_and_cleared_in_one_pass", covers = ["kernel"]);
fn an_error_record_is_read_and_cleared_in_one_pass() {
	// READING AND CLEARING ARE ONE OPERATION, because the alternative is reporting the same error for
	// ever: the status bits are sticky until somebody writes a one to them, so a reader that only
	// read would turn one marginal link into an endless stream of identical records - and a machine
	// whose log says the same thing a thousand times says nothing.
	stand_up([0; 6], [0; 6]);
	// A bad TLP (correctable, bit 6) and a completion timeout (uncorrectable, bit 14), with the
	// timeout declared NON-fatal by this function.
	stand_up_aer(1 << 14, 1 << 6, 0);
	let record = super::take_error_record::<Fake>(0, 0, 0).expect("a function with an AER capability has a record");
	assert_eq!(record.correctable, 1 << 6, "the correctable half is what the device set");
	assert_eq!(record.uncorrectable, 1 << 14, "and so is the uncorrectable half");
	assert!(!record.fatal, "and a bit the function declares non-fatal is not fatal");
	assert_eq!(super::correctable_name(record.correctable), "a bad TLP", "the record says what happened");
	assert_eq!(super::uncorrectable_name(record.uncorrectable), "a completion timeout");

	// AND THE SECOND PASS IS QUIET, which is the half a reader without the write does not get.
	let again = super::take_error_record::<Fake>(0, 0, 0).expect("the capability is still there");
	assert!(again.is_quiet(), "the record was cleared as it was read: {again:?}");

	// AND THE SEVERITY REGISTER IS NOT A STATUS REGISTER. Acknowledging it would rewrite the
	// function's own policy about which errors are fatal, and the next real error would be reported
	// under a severity nobody chose.
	stand_up_aer(1 << 18, 0, 1 << 18);
	let _ = super::take_error_record::<Fake>(0, 0, 0);
	assert_eq!(Fake::read32(0, 0, 0, 0x100 + 0x0C), 1 << 18, "the severity register survives the acknowledgement");
}

crate::tagged_test!(severity_is_the_functions_own_and_decides_what_is_fatal, [Kernel, Pci], id = "kernel.arch.common.pci.severity_is_the_functions_own_and_decides_what_is_fatal", covers = ["kernel"]);
fn severity_is_the_functions_own_and_decides_what_is_fatal() {
	// THE SAME BIT IS FATAL ON ONE FUNCTION AND NOT ON ANOTHER, because severity is a register the
	// FUNCTION carries rather than a property of the error. A kernel with a hard-coded list of fatal
	// bits would quarantine a device its own hardware said was fine, and leave one alone that was
	// not - and quarantining is the one action here that cannot be taken back.
	stand_up([0; 6], [0; 6]);
	stand_up_aer(1 << 18, 0, 0);
	let lenient = super::take_error_record::<Fake>(0, 0, 0).expect("a record");
	assert!(!lenient.fatal, "a malformed TLP the function declares non-fatal is not fatal");

	stand_up_aer(1 << 18, 0, 1 << 18);
	let strict = super::take_error_record::<Fake>(0, 0, 0).expect("a record");
	assert!(strict.fatal, "and the same bit with the severity set IS");

	// AND A SEVERITY BIT WITH NO STATUS BEHIND IT IS NOT AN ERROR AT ALL. A function declares every
	// error it could have as fatal or not, all the time; what happened is the STATUS.
	stand_up_aer(0, 1 << 6, u32::MAX);
	let corrected = super::take_error_record::<Fake>(0, 0, 0).expect("a record");
	assert!(!corrected.fatal, "a correctable error is not fatal however the severity register is set");
	assert!(!corrected.is_quiet(), "and it is still a record");
}

crate::tagged_test!(a_function_that_left_the_bus_reports_no_record_rather_than_every_error_there_is, [Kernel, Pci], id = "kernel.arch.common.pci.a_function_that_left_the_bus_reports_no_record_rather_than_every_error_there_is", covers = ["kernel"]);
fn a_function_that_left_the_bus_reports_no_record_rather_than_every_error_there_is() {
	// AN ABSENT FUNCTION ANSWERS ALL ONES, and all ones in both status registers is EVERY error at
	// once - which a reader that took it at face value would report as a catastrophic failure and
	// then quarantine a device that is not there. What it is is a device that left the bus, and that
	// is the departure path's question rather than this one's.
	stand_up([0; 6], [0; 6]);
	stand_up_aer(u32::MAX, u32::MAX, 0);
	assert!(super::take_error_record::<Fake>(0, 0, 0).is_none(), "a function answering all ones has no record to report");

	// AND ONE REGISTER OF ALL ONES IS STILL A RECORD, because a function CAN set many bits at once -
	// what says it is gone is that the whole of config space answers the same thing.
	stand_up_aer(u32::MAX, 0, u32::MAX);
	let record = super::take_error_record::<Fake>(0, 0, 0).expect("a record");
	assert!(record.fatal, "every uncorrectable error with every severity set is fatal");
}

crate::tagged_test!(a_power_event_names_its_requester_and_the_acknowledgement_does_not_write_it_back, [Kernel, Pci], id = "kernel.arch.common.pci.a_power_event_names_its_requester_and_the_acknowledgement_does_not_write_it_back", covers = ["kernel"]);
fn a_power_event_names_its_requester_and_the_acknowledgement_does_not_write_it_back() {
	// A PME SAYS WHICH FUNCTION ASKED TO BE WOKEN, and that is the whole of what a port can say: what
	// a wake MEANS belongs to a power policy, and there is not one here. The requester id is in the
	// same register as the status bit and is NOT writable, so the acknowledgement writes the status
	// bit alone - writing the value that was read would put an id back into a field that does not
	// take one, and on hardware that is a write of reserved bits.
	stand_up([0; 6], [0; 6]);
	stand_up_pcie(4, true, SLOT_CAP_HOT_PLUG, SLOT_STATUS_PRESENT);
	let slot = resolve_slot::<Fake>(&a_function()).expect("a slot");
	let port = super::HotPlugPort { bus: 0, dev: 0, func: 0, slot, occupied: true };
	assert!(super::take_power_event::<Fake>(&port).is_none(), "a port nothing woke reports nothing");

	// The port records a PME from 01:03.2, which is requester id 0x011a. THE STATUS BIT IS IN THE
	// UPPER HALF-WORD and the requester in the lower, which is why the fixture's write-one-to-clear
	// half is set to the upper one: on hardware the id is READ-ONLY and the bit is RW1C, in one
	// register.
	{
		let mut space = SPACE.lock();
		let at = (slot.cap + 0x20) as usize;
		let status: u32 = (1 << 16) | 0x011a;
		space.cfg[at..at + 4].copy_from_slice(&status.to_le_bytes());
		space.rw1c = slot.cap + 0x20 + 2;
	}
	let event = super::take_power_event::<Fake>(&port).expect("the port reports the event");
	assert_eq!(event.requester, 0x011a, "the event names the function that sent it");
	assert_eq!((event.requester >> 8) as u8, 1, "which is bus 1");
	assert_eq!(((event.requester >> 3) & 0x1F) as u8, 3, "device 3");
	assert_eq!((event.requester & 0x7) as u8, 2, "function 2");

	// THE ACKNOWLEDGEMENT WRITES THE STATUS BIT ALONE, which is the thing this test is about. A
	// reader that wrote back the value it had just read would put the requester id into the lower
	// half - a write of a field that does not take one, and on hardware a write of reserved bits - so
	// what is checked is that the lower half of what was written is EMPTY.
	let after = Fake::read32(0, 0, 0, slot.cap + 0x20);
	assert_eq!(after & (1 << 16), 0, "the status bit was acknowledged");
	assert_eq!(after & 0xFFFF, 0, "and the write carried no requester id, so it was the bit alone and not the value that was read");
	assert!(super::take_power_event::<Fake>(&port).is_none(), "and a second look reports nothing");
}

crate::tagged_test!(a_sweep_reports_only_the_functions_that_said_something, [Kernel, Pci], id = "kernel.arch.common.pci.a_sweep_reports_only_the_functions_that_said_something", covers = ["kernel"]);
fn a_sweep_reports_only_the_functions_that_said_something() {
	// A SWEEP OVER A WORKING MACHINE FILLS NOTHING. A record whose every field is zero is a function
	// that is fine, and a sweep that returned one per watched function would make finding the one
	// that is not the caller's problem - which on a machine with sixteen reporters is most of them.
	stand_up([0; 6], [0; 6]);
	super::forget_error_reporters();
	stand_up_aer(0, 0, 0);
	assert!(super::note_error_reporter::<Fake>(0, 0, 0), "a function with an AER capability is watched");
	// IDEMPOTENT, because a hot-plug slot's address is reused: a card plugged into one twice is the
	// same bus, device and function, and a list that grew each time would fill up with one slot.
	assert!(super::note_error_reporter::<Fake>(0, 0, 0), "and noting it twice is still true");

	let quiet = super::ErrorRecord { bus: 0, dev: 0, func: 0, correctable: 0, uncorrectable: 0, fatal: false };
	let mut out = [quiet; super::MAX_ERROR_REPORTERS];
	assert_eq!(super::poll_errors::<Fake>(&mut out), 0, "a machine with nothing to report fills no records");

	// AND A FUNCTION THAT SAID SOMETHING IS FILLED ONCE. The sweep clears as it reads, so the second
	// pass over the same machine is quiet again - which is what stops one fault becoming a stream.
	stand_up_aer(0, 1 << 8, 0);
	assert_eq!(super::poll_errors::<Fake>(&mut out), 1, "the function that reported is filled");
	assert_eq!(out[0].correctable, 1 << 8, "with what it reported");
	assert_eq!(super::correctable_name(out[0].correctable), "a replay-number rollover");
	assert_eq!(super::poll_errors::<Fake>(&mut out), 0, "and the next sweep is quiet, because the first cleared it");

	// AND A FUNCTION WITH NO CAPABILITY IS NOT WATCHED AT ALL, which is what makes the sweep cheap on
	// an ordinary machine: most functions carry none.
	super::forget_error_reporters();
	stand_up([0; 6], [0; 6]);
	assert!(!super::note_error_reporter::<Fake>(0, 0, 0), "a function with no AER capability is not watched");
	assert_eq!(super::poll_errors::<Fake>(&mut out), 0, "and a sweep over an empty list reports nothing");
}

crate::tagged_test!(a_power_event_sweep_reads_every_port_and_clears_what_it_read, [Kernel, Pci], id = "kernel.arch.common.pci.a_power_event_sweep_reads_every_port_and_clears_what_it_read", covers = ["kernel"]);
fn a_power_event_sweep_reads_every_port_and_clears_what_it_read() {
	// THE SWEEP IS OVER THE PORTS AND NOT OVER THE DEVICES, because a PME is a MESSAGE a function
	// sends UPSTREAM: what records it is the root port above it, and the requester id is how the port
	// says which function that was. A sweep over endpoints would find nothing and conclude nothing
	// woke the machine.
	stand_up([0; 6], [0; 6]);
	stand_up_pcie(4, true, SLOT_CAP_HOT_PLUG, SLOT_STATUS_PRESENT);
	// THE KERNEL'S OWN PORT TABLE IS PUT BACK WHEN THIS DROPS - see `HeldPorts`.
	let _ports = super::HeldPorts::take();
	super::arm_hot_plug_slots::<Fake>(&[a_function()]);
	let slot = resolve_slot::<Fake>(&a_function()).expect("a slot");

	let mut out = [super::PowerEvent { bus: 0, dev: 0, func: 0, requester: 0 }; super::MAX_HOT_PLUG_PORTS];
	assert_eq!(super::poll_power_events::<Fake>(&mut out), 0, "a machine nothing woke reports nothing");

	{
		let mut space = SPACE.lock();
		let at = (slot.cap + 0x20) as usize;
		let status: u32 = (1 << 16) | 0x0208;
		space.cfg[at..at + 4].copy_from_slice(&status.to_le_bytes());
		space.rw1c = slot.cap + 0x20 + 2;
	}
	assert_eq!(super::poll_power_events::<Fake>(&mut out), 1, "the port that recorded one reports it");
	assert_eq!(out[0].requester, 0x0208, "and names the function that sent it");
	assert_eq!(super::poll_power_events::<Fake>(&mut out), 0, "and the next sweep is quiet, because the first acknowledged it");
}
