// Shared logic for the userspace virtio drivers.
//
// DeviceManager launches one driver process per device and hands it, over its
// bootstrap channel, a "DEVICE" message carrying the device's DeviceInfo (its MMIO
// struct offsets) and a transferred DeviceMemory capability to its MMIO BAR. The
// driver maps the BAR, brings the device up through the shared virtio transport
// (negotiation + a ready virtqueue), does its device-specific I/O over the queue,
// reports in, and then stands holding its device. This is the isolated,
// capability-scoped shell each driver runs inside.

#![allow(dead_code)]

use rt::*;

use crate::virtio::{self, Virtio};

// Receive the device from DeviceManager, map its MMIO BAR, and negotiate it up to
// FEATURES_OK through the virtio transport. Returns the negotiated device; the
// caller sets up its queues and calls `driver_ok`. Exits the process on any failure
// (a driver with no working device has nothing to do).
pub unsafe fn bringup(bootstrap: u64) -> Virtio {
	unsafe {
		let mut buf: [u8; 96] = [0u8; 96];
		let info_size: usize = core::mem::size_of::<DeviceInfo>();
		// receive "DEVICE" + DeviceInfo + the DeviceMemory capability.
		let (device_handle, info): (u64, DeviceInfo) = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if handle != 0 && len >= 6 + info_size && &buf[..6] == b"DEVICE" => (handle, (buf.as_ptr().add(6) as *const DeviceInfo).read_unaligned()),
			_ => exit(),
		};
		// map the device's MMIO BAR into our address space.
		let base: u64 = syscall(SYS_DEVICE_MEMORY_MAP, device_handle, 0, 0, 0);
		if sys_is_err(base) {
			exit();
		}
		// reset -> negotiate -> features-ok.
		match virtio::negotiate(base, &info) {
			Some(device) => device,
			None => exit(),
		}
	}
}

// Report in over the bootstrap channel, then stand holding the device until
// DeviceManager drops the channel.
pub unsafe fn online_and_stand(bootstrap: u64, report: &[u8]) -> ! {
	unsafe {
		let mut buf: [u8; 16] = [0u8; 16];
		send_blocking(bootstrap, report, 0);
		let _ = recv_blocking(bootstrap, &mut buf);
	}
	exit();
}
