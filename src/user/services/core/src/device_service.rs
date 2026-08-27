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
use ipc_client::ChannelTransport;
use proto::system::device::{self, Service};
use proto::system::{BindingRecord, DeviceEntry, DeviceType, Error, provider_catalogue};
use rt::*;

include!(concat!(env!("OUT_DIR"), "/roles_device_service.rs"));

// The kernel device table, behind the generated Device contract - plus the binding snapshot, which
// this service does not hold and does not derive: it FORWARDS it, verbatim, from the one process
// that does. A second rendering is how one surface comes to report a constant where a state belongs.
struct Devices {
	bindings: u64,
}

impl Service for Devices {
	fn bindings(&mut self) -> Result<Vec<BindingRecord>, Error> {
		if self.bindings == 0 {
			// A boot that granted no catalogue connection has nothing to forward, and saying so is
			// better than an empty list a caller would read as "no devices are bound".
			return Err(Error::Closed);
		}
		provider_catalogue::Client::new(ChannelTransport { chan: self.bindings }).bindings().ok_or(Error::Closed)
	}

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
	// 1. report in to the supervisor that started us.
	unsafe {
		send_blocking(bootstrap, b"DeviceService: online", 0);
	}

	// 2. take the roles the plan says this service is handed - here one, the channel clients
	//    reach us on. Checked against the GENERATED list rather than read by hand: the tag,
	//    the kernel object type and the rights are all things a receiver can check, and a
	//    bootstrap that is wrong is better refused by name than served with the wrong
	//    handle. A supervisor that dropped the channel instead (no clients this boot)
	//    reports as a missing role, and there is nothing left to serve either way.
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = unsafe { receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) } {
		unsafe { fail_bootstrap(bootstrap, error.tag(), error.reason()) };
	}
	let service: u64 = roles[0];
	// The catalogue connection, if the plan handed one over. Its position in `BOOTSTRAP_ROLES` is
	// generated from the manifest, so this reads by name rather than by a number written twice.
	let bindings: u64 = if roles.len() > 1 { roles[1] } else { 0 };

	// 3. serve generated list/get requests until the client side closes.
	let mut devices: Devices = Devices { bindings };
	let mut request: [u8; 256] = [0u8; 256];
	let mut reply: [u8; 4096] = [0u8; 4096];
	unsafe {
		serve_multi(service, &mut request, &mut reply, |_chan, req, handle, out, reply_handle| -> Option<usize> { device::dispatch(&mut devices, req, handle, out, reply_handle) });
	}
	exit();
}
