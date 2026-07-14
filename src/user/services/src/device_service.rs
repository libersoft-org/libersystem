// DeviceService - the userspace typed device-enumeration service.
//
// ServiceManager starts this program from the init package and hands it a
// bootstrap channel. DeviceService reports in, then waits for a "SERVE" message
// carrying the channel its clients reach it on. Over that channel clients speak the
// generated `liber:system` Device bindings: they LIST the devices the kernel
// discovered on the bus (read from the kernel device table over the device
// syscalls - the same table DeviceManager binds drivers to) or GET one by index,
// receiving typed `device-entry` records that render as CLI / JSON on the client.
//
// When the supervisor that started it drops the bootstrap channel, the service
// exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::device::{self, Service};
use proto::system::{DeviceEntry, DeviceType, Error};
use rt::*;

// The kernel device table, behind the generated Device contract.
struct Devices;

impl Service for Devices {
	fn list(&mut self) -> Result<Vec<DeviceEntry>, Error> {
		let mut out: Vec<DeviceEntry> = Vec::new();
		let count: u64 = unsafe { device_count() };
		let mut i: u64 = 0;
		while i < count {
			if let Some(entry) = unsafe { device_entry(i) } {
				out.push(entry);
			}
			i += 1;
		}
		Ok(out)
	}

	fn get(&mut self, index: u32) -> Result<DeviceEntry, Error> {
		unsafe { device_entry(index as u64) }.ok_or(Error::NotFound)
	}
}

// Read device `i` from the kernel table and map it to a typed entry, or None if the
// index is out of range.
unsafe fn device_entry(i: u64) -> Option<DeviceEntry> {
	unsafe {
		let mut info: DeviceInfo = DeviceInfo::default();
		if !device_info(i, &mut info) {
			return None;
		}
		Some(DeviceEntry { index: i as u32, r#type: type_of(info.device_type), mmio_len: info.bar_len })
	}
}

// Map a kernel device-type code to the typed device type.
fn type_of(device_type: u32) -> DeviceType {
	match device_type {
		VIRTIO_TYPE_NET => DeviceType::Net,
		VIRTIO_TYPE_BLOCK => DeviceType::Block,
		VIRTIO_TYPE_CONSOLE => DeviceType::Console,
		DEVICE_TYPE_XHCI => DeviceType::Usb,
		_ => DeviceType::Unknown,
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. report in to the supervisor that started us.
	unsafe {
		send_blocking(bootstrap, b"DeviceService: online", 0);
	}

	// 2. wait for the serve channel clients reach us on. If the supervisor drops the
	//    bootstrap channel first (no clients this boot), we are done.
	let service: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"SERVE") }.unwrap_or_else(|| exit());

	// 3. serve generated list/get requests until the client side closes.
	let mut devices: Devices = Devices;
	let mut request: [u8; 256] = [0u8; 256];
	let mut reply: [u8; 4096] = [0u8; 4096];
	unsafe {
		serve_multi(service, &mut request, &mut reply, |_chan: u64, req: &[u8], handle: &mut u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> { device::dispatch(&mut devices, req, handle, out, reply_handle) });
	}
	exit();
}
