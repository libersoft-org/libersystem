// The hostile endpoint, which is the only kind that proves anything.
//
// QEMU's `edu` device is a PCI function with a DMA engine a driver programs directly: a source
// address, a destination, a length and a start bit. That makes it the one device in reach that can
// be told to read or write an ARBITRARY physical address on demand - which is exactly what a
// malicious driver does, and exactly what an IOMMU is supposed to stop.
//
// WHY IT IS IN THE KERNEL AND NOT A USERSPACE DRIVER. There is nothing to drive: this is a fixture,
// its only user is the conformance suite, and giving a userspace process the ability to program
// arbitrary DMA would be handing out the capability the milestone exists to remove. It claims one
// BAR, does one transfer, and is bounded in both.
//
// A SENTINEL IS THE EVIDENCE. Every case here writes a known pattern into a frame the device is not
// entitled to touch, asks the device to touch it, and checks the pattern afterwards. "The transfer
// was refused" is what the kernel is told; "the memory did not change" is what actually matters, and
// only the second is checked here.

use crate::mem::frame::PAGE_SIZE;

// The `edu` device's PCI identity.
const VENDOR: u16 = 0x1234;
const DEVICE: u16 = 0x11e8;

// Its register window, as QEMU documents it.
const REG_IDENT: u64 = 0x00;
const REG_LIVENESS: u64 = 0x04;
const REG_DMA_SOURCE: u64 = 0x80;
const REG_DMA_DESTINATION: u64 = 0x88;
const REG_DMA_COUNT: u64 = 0x90;
const REG_DMA_COMMAND: u64 = 0x98;

// The command register's bits.
const DMA_START: u64 = 1 << 0;
// Direction: clear means RAM -> the device's own buffer, set means the device's buffer -> RAM.
const DMA_FROM_DEVICE: u64 = 1 << 1;

// The device's internal buffer lives at this address in its own DMA address space, and is 4096
// bytes long. A transfer must name it as one of its two ends.
const DEVICE_BUFFER: u64 = 0x4_0000;
const DEVICE_BUFFER_LEN: u64 = 4096;

// The identification register's value tells a real `edu` from whatever else answered. QEMU's device
// publishes `0x010000ed`: a major and minor version in the high half, and `0x00ed` in the low half
// as the identity proper. The version is deliberately NOT part of the check - a later revision of the
// same device is still the device.
const IDENT_MASK: u32 = 0x0000_FFFF;
const IDENT_VALUE: u32 = 0x0000_00ED;

pub struct Edu {
	// The direct-map address of the register window.
	registers: u64,
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
}

unsafe fn read32(at: u64) -> u32 {
	unsafe { core::ptr::read_volatile(at as *const u32) }
}

unsafe fn write32(at: u64, value: u32) {
	unsafe { core::ptr::write_volatile(at as *mut u32, value) }
}

unsafe fn write64(at: u64, value: u64) {
	unsafe { core::ptr::write_volatile(at as *mut u64, value) }
}

// Find the device, if this machine has one. Every boot except the fixture's does not.
pub fn find() -> Option<Edu> {
	find_nth(0)
}

// The `n`th `edu` function on the bus. The fixture's domain-locality case needs TWO endpoints -
// "the same numeric address means different memory to different devices" is not a claim one device
// can be asked about.
pub fn find_nth(wanted: usize) -> Option<Edu> {
	let mut seen = 0;
	for index in 0..crate::device::pci_count() {
		let function = crate::device::pci_get(index)?;
		if function.vendor != VENDOR || function.device != DEVICE {
			continue;
		}
		if seen != wanted {
			seen += 1;
			continue;
		}
		let (base, size) = crate::arch::pci::function_bar(function.bus, function.dev, function.func, 0)?;
		// A WINDOW THIS SMALL IS THE WHOLE POINT. The fixture claims one BAR of a documented size;
		// anything else answering at this identity is refused rather than poked at.
		if size < 0x100 {
			return None;
		}
		let registers = crate::mem::hhdm_offset() + base;
		// SAFETY: the BAR the device published, reached through the direct map like every other
		// MMIO window in this kernel.
		let identity = unsafe { read32(registers + REG_IDENT) };
		if identity & IDENT_MASK != IDENT_VALUE {
			return None;
		}
		return Some(Edu { registers, bus: function.bus, dev: function.dev, func: function.func });
	}
	None
}

impl Edu {
	// The device answers, and answers correctly. `edu` inverts whatever is written to its liveness
	// register, which is a cheap way to know the window is really this device's.
	pub fn alive(&self) -> bool {
		// SAFETY: this fixture's own register window, resolved in `find`.
		unsafe {
			write32(self.registers + REG_LIVENESS, 0xDEAD_BEEF);
			read32(self.registers + REG_LIVENESS) == !0xDEAD_BEEFu32
		}
	}

	// Let the device master the bus, or stop it. The fixture does this itself rather than through
	// the driver-binding path: `edu` has no driver and no device-table entry, which is the point of
	// it being a fixture.
	pub fn set_bus_master(&self, on: bool) {
		crate::arch::pci::set_bus_master(self.bus, self.dev, self.func, on);
	}

	// Ask the device to copy `len` bytes between RAM at `address` and its own buffer.
	//
	// BOUNDED IN EVERY DIRECTION: the length is clamped to the device's buffer, the poll for
	// completion has an end, and a device that never finishes returns `false` rather than stopping
	// the kernel. A hostile emulated device is inside this milestone's threat model.
	pub fn transfer(&self, address: u64, len: u64, from_device: bool) -> bool {
		let len = len.min(DEVICE_BUFFER_LEN);
		let (source, destination) = if from_device { (DEVICE_BUFFER, address) } else { (address, DEVICE_BUFFER) };
		// SAFETY: this fixture's own register window.
		unsafe {
			write64(self.registers + REG_DMA_SOURCE, source);
			write64(self.registers + REG_DMA_DESTINATION, destination);
			write64(self.registers + REG_DMA_COUNT, len);
			write64(self.registers + REG_DMA_COMMAND, DMA_START | if from_device { DMA_FROM_DEVICE } else { 0 });
		}
		// The command register clears its start bit when the transfer ends.
		for _ in 0..2_000_000u64 {
			// SAFETY: as above.
			let command = unsafe { read32(self.registers + REG_DMA_COMMAND) } as u64;
			if command & DMA_START == 0 {
				return true;
			}
			core::hint::spin_loop();
		}
		false
	}
}

// A frame with a known pattern in it, and the means to ask whether it changed.
//
// THE FRAME IS NOT MAPPED FOR THE DEVICE. That is what makes it a sentinel: the whole question is
// whether a device that was never given this address can reach it anyway.
pub struct Sentinel {
	pub physical: u64,
	pattern: u8,
}

impl Sentinel {
	pub fn new(pattern: u8) -> Option<Sentinel> {
		let physical = crate::mem::frame::allocate()?;
		// SAFETY: a freshly allocated frame, reached through the direct map, owned by nobody else.
		unsafe { core::ptr::write_bytes((crate::mem::hhdm_offset() + physical) as *mut u8, pattern, PAGE_SIZE as usize) };
		Some(Sentinel { physical, pattern })
	}

	// Whether every byte is still what was written. A device that reached this frame changed it.
	pub fn intact(&self) -> bool {
		let base = crate::mem::hhdm_offset() + self.physical;
		for offset in 0..PAGE_SIZE {
			// SAFETY: this sentinel's own frame, through the direct map.
			if unsafe { core::ptr::read_volatile((base + offset) as *const u8) } != self.pattern {
				return false;
			}
		}
		true
	}
}

impl Drop for Sentinel {
	fn drop(&mut self) {
		// SAFETY: this sentinel owns the frame and it was never mapped into any address space.
		// NEVER-MAPPED: allocated here, written through the direct map, and never handed to a
		// device that was allowed to reach it.
		unsafe { crate::mem::frame::deallocate(self.physical) };
	}
}
