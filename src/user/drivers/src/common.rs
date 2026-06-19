// Shared logic for the userspace virtio drivers.
//
// DeviceManager launches one driver process per device and hands it, over its
// bootstrap channel, a "DEVICE" message carrying the device's DeviceInfo (its MMIO
// struct offsets) and a transferred DeviceMemory capability to its MMIO BAR. The
// driver maps the BAR, touches the device to prove the MMIO is reachable, reports
// in, and then stands holding its device. The full virtqueue transport and the
// per-device data path arrive in M24; this is the isolated, capability-scoped
// shell each driver runs inside.

#![allow(dead_code)]

use rt::*;

// Offset of the one-byte device_status register within the virtio common-config
// structure. Writing 0 resets the device (a driver's first step).
const DEVICE_STATUS: u64 = 0x14;

// Run the driver: receive the device, map its MMIO, reset it, report `report`
// (e.g. "driver.virtio-blk: online"), then stand until DeviceManager drops us.
pub fn run(bootstrap: u64, report: &[u8]) -> ! {
	let mut buf: [u8; 96] = [0u8; 96];
	let info_size: usize = core::mem::size_of::<DeviceInfo>();
	unsafe {
		// 1. receive "DEVICE" + DeviceInfo + the DeviceMemory capability.
		let (device_handle, info): (u64, DeviceInfo) = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if handle != 0 && len >= 6 + info_size && &buf[..6] == b"DEVICE" => (handle, (buf.as_ptr().add(6) as *const DeviceInfo).read_unaligned()),
			_ => exit(),
		};
		// 2. map the device's MMIO BAR into our address space.
		let base: u64 = syscall(SYS_DEVICE_MEMORY_MAP, device_handle, 0, 0, 0);
		if sys_is_err(base) {
			exit();
		}
		// 3. touch the device: reset it via the common-config device_status register
		//    and read it back (0 after reset), proving the MMIO is genuinely reachable.
		let status: *mut u8 = (base + info.common_offset as u64 + DEVICE_STATUS) as *mut u8;
		status.write_volatile(0);
		if status.read_volatile() == 0 {
			send_blocking(bootstrap, report, 0);
		}
		// 4. stand, holding the device, until DeviceManager drops the channel.
		let _ = recv_blocking(bootstrap, &mut buf);
	}
	exit();
}
