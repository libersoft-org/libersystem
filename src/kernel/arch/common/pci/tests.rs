// KERN-ARCH-015: BAR assignment does not hand out memory a device already decodes, and a device
// whose BARs could not all be placed is not switched on.
//
// Driven through a synthetic `ConfigAccess`: the trait IS the bus, so a device with any BAR layout
// can be stood up here - which is the only way to reach the layouts QEMU does not produce.

use super::{ConfigAccess, PciDevice, assign_bars_ecam};
use crate::sync::SpinLock;

const WINDOW_BASE: u64 = 0x1000_0000;
// Deliberately small - 256 kB - so a window that runs out is one line of setup away.
const WINDOW_END: u64 = 0x1004_0000;

struct Space {
	bar: [u32; 6],
	// What an all-ones write reads back: the size mask, or zero for a slot the device does not
	// implement. This is the whole of what a BAR probe can learn.
	mask: [u32; 6],
	command: u16,
	next: u64,
}

static SPACE: SpinLock<Space> = SpinLock::new(Space { bar: [0; 6], mask: [0; 6], command: 0, next: WINDOW_BASE });

// Put a device on the fake bus: `bar` is what the firmware left in each slot, `mask` what each
// slot answers a probe with.
fn stand_up(bar: [u32; 6], mask: [u32; 6]) {
	let mut space = SPACE.lock();
	space.bar = bar;
	space.mask = mask;
	space.command = 0;
	space.next = WINDOW_BASE;
}

struct Fake;

impl ConfigAccess for Fake {
	const BUS_COUNT: u16 = 1;
	const MMIO_WINDOW_END: u64 = WINDOW_END;

	fn read32(_bus: u8, _dev: u8, _func: u8, off: u16) -> u32 {
		let space = SPACE.lock();
		match off {
			0x04 => space.command as u32,
			0x10..=0x24 => space.bar[((off - 0x10) / 4) as usize],
			_ => 0,
		}
	}

	fn write32(_bus: u8, _dev: u8, _func: u8, off: u16, val: u32) {
		let mut space = SPACE.lock();
		match off {
			0x04 => space.command = val as u16,
			0x10..=0x24 => {
				let index = ((off - 0x10) / 4) as usize;
				// A device answers an all-ones write with its size mask and stores anything else.
				// That is what makes a probe a probe, and what the kernel's restore relies on.
				let mask = space.mask[index];
				space.bar[index] = if val == 0xFFFF_FFFF { mask } else { val };
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

const DEVICE: PciDevice = PciDevice { bus: 0, dev: 0, func: 0, vendor: 0x1af4, device_id: 0x1000, class: 0x02, subclass: 0x00, prog_if: 0x00, header_type: 0, bars: [0; 6] };

// A 32-bit memory BAR of `size` bytes: the mask a probe reads back.
const fn mask32(size: u32) -> u32 {
	!(size - 1)
}

fn command() -> u16 {
	SPACE.lock().command
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
	assert!(command() & 0x02 != 0 && command() & 0x04 != 0, "both BARs are placed, so the device is enabled");
}

crate::tagged_test!(a_device_whose_bar_will_not_fit_is_left_switched_off, [Kernel, Pci], id = "kernel.arch.common.pci.a_device_whose_bar_will_not_fit_is_left_switched_off", covers = ["kernel"]);
fn a_device_whose_bar_will_not_fit_is_left_switched_off() {
	// The other half of -015. When the window had nothing left, the allocation simply did not
	// happen - and the command register was written anyway, three lines later, unconditionally.
	// The device was told to decode memory and to master the bus with a BAR still reading zero:
	// responding at address zero, and a bus master with no aperture of its own.
	//
	// One BAR larger than the whole 256 kB window.
	stand_up([0, 0, 0, 0, 0, 0], [mask32(0x8_0000), 0, 0, 0, 0, 0]);
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
