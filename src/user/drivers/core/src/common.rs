// Shared logic for the userspace virtio drivers.
//
// DeviceManager launches one driver process per device and hands it, over its
// bootstrap channel, a "DEVICE" message carrying the device's DeviceInfo (its MMIO
// struct offsets) and a transferred DeviceMemory capability to its MMIO BAR. The
// driver maps the BAR, brings the device up through the shared virtio transport
// (negotiation + a ready virtqueue), does its device-specific I/O over the queue,
// reports in, and then stands holding its device. This is the isolated,
// capability-scoped shell each driver runs inside.

use rt::*;

use crate::virtio::{self, Virtio};

// Receive the device from DeviceManager, map its MMIO BAR, and negotiate it up to
// FEATURES_OK through the virtio transport. Returns the negotiated device; the
// caller sets up its queues and calls `driver_ok`. Exits the process on any failure
// (a driver with no working device has nothing to do).
pub unsafe fn bringup(bootstrap: u64) -> Virtio {
	unsafe { bringup_features(bootstrap, 0) }
}

// `bringup`, additionally asking the negotiation for the word-0 (device-specific)
// feature bits `want_word0` names; the accepted set is readable off the returned
// device (`features_word0`).
pub unsafe fn bringup_features(bootstrap: u64, want_word0: u32) -> Virtio {
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
		// reset -> negotiate -> features-ok, and the reset is also what says the frames a previous
		// driver of this device left behind are safe to recycle (see `virtio::negotiate_for`).
		match virtio::negotiate_for(device_handle, base, &info, want_word0) {
			Some(device) => device,
			None => exit(),
		}
	}
}

// Report in over the bootstrap channel, then stand holding the device until
// DeviceManager drops the channel.
// A driver's report, with the device it is about.
//
// FOUR IDENTICAL LINES ARE ONE LINE THE READER CANNOT USE. A machine with four `virtio-blk`
// functions printed `driver.virtio-blk: online` four times, and the information that would have told
// them apart was six lines further down in the kernel's DMA audit, which lists the same four devices
// by address. So the address comes with the report: `driver.virtio-blk: online (00:01.0)`.
//
// `detail` is whatever else the driver has to say about itself - a role, a self-test result - and is
// empty for most. The whole line is built in a fixed buffer because a driver has no formatter.
pub fn describe(out: &mut [u8; 64], name: &[u8], device: &Virtio, detail: &[u8]) -> usize {
	let (bus, dev, func) = device.address();
	let mut n = 0usize;
	push(out, &mut n, b"driver.");
	push(out, &mut n, name);
	push(out, &mut n, b": online (");
	push(out, &mut n, &hex2(bus));
	push(out, &mut n, b":");
	push(out, &mut n, &hex2(dev));
	push(out, &mut n, b".");
	push(out, &mut n, &[b'0' + (func % 10)]);
	if !detail.is_empty() {
		push(out, &mut n, b", ");
		push(out, &mut n, detail);
	}
	push(out, &mut n, b")");
	n
}

// Append what fits and drop what does not: a report that runs off the end of its buffer is a report,
// and a driver that panicked while writing one is a device that never came up.
fn push(out: &mut [u8; 64], at: &mut usize, bytes: &[u8]) {
	for byte in bytes {
		if *at < out.len() {
			out[*at] = *byte;
			*at += 1;
		}
	}
}

fn hex2(byte: u8) -> [u8; 2] {
	const HEX: &[u8; 16] = b"0123456789abcdef";
	[HEX[(byte >> 4) as usize], HEX[(byte & 0xf) as usize]]
}

pub unsafe fn online_and_stand(bootstrap: u64, report: &[u8]) -> ! {
	unsafe {
		let mut buf: [u8; 16] = [0u8; 16];
		send_blocking(bootstrap, report, 0);
		let _ = recv_blocking(bootstrap, &mut buf);
	}
	exit();
}
