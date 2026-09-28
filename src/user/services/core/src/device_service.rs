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
use proto::system::{BindingRecord, DeviceEntry, DeviceType, Error, PlatformId, PlatformIdKind, PlatformResource, PlatformResourceKind, PlatformSource, PlatformState, RowKind, provider_catalogue};
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
		let count: u64 = device_count();
		let mut i: u64 = 0;
		while i < count {
			if let Some(entry) = device_entry(i) {
				out.push(entry);
			}
			i += 1;
		}
		Ok(out)
	}

	fn get(&mut self, index: u32) -> Result<DeviceEntry, Error> {
		device_entry(index as u64).ok_or(Error::NotFound)
	}
}

// Read device `i` from the kernel table and map it to a typed entry, or None if the
// index is out of range.
fn device_entry(i: u64) -> Option<DeviceEntry> {
	let mut info: DeviceInfo = DeviceInfo::default();
	if !device_info(i, &mut info) {
		return None;
	}
	// The address comes straight from the kernel table, which is what makes a row number
	// resolvable to a device without asking any service. See `device-entry`.
	let mut entry = DeviceEntry { index: i as u32, r#type: type_of(info.device_type), mmio_len: info.bar_len, present: info.on_bus != 0, bus: info.bus as u32, dev: info.dev as u32, func: info.func as u32, kind: RowKind::Pci, source: PlatformSource::None, state: PlatformState::None, identity: alloc::string::String::new(), ids: Vec::new(), resources: Vec::new(), unresolved: false, companion: alloc::string::String::new(), parent: alloc::string::String::new() };
	if info.platform.kind == ROW_KIND_PLATFORM {
		describe_platform(&info, &mut entry);
	} else if info.platform.state == PLATFORM_STATE_FIRMWARE_HELD {
		// A FUNCTION THE ACPI SERVICE'S REGIONS HOLD: no driver may take it.
		entry.state = PlatformState::FirmwareHeld;
	}
	attach_firmware_node(i, &mut entry);
	Some(entry)
}

// WHAT THE NAMESPACE ATTACHED TO THE ROW: a companion's path, a parent function.
fn attach_firmware_node(index: u64, entry: &mut DeviceEntry) {
	let mut node = FirmwareNode::default();
	let answer = unsafe { syscall(SYS_DEVICE_NODE, index, &mut node as *mut FirmwareNode as u64, core::mem::size_of::<FirmwareNode>() as u64, 0) } as i64;
	if answer != 0 {
		return;
	}
	if node.flags & FIRMWARE_NODE_COMPANION != 0 {
		entry.companion = alloc::string::String::from_utf8_lossy(node.path()).into_owned();
	}
	if node.flags & FIRMWARE_NODE_PARENT != 0 {
		entry.parent = alloc::format!("{:02x}:{:02x}.{}", node.parent_bus, node.parent_dev, node.parent_func);
	}
}

// A PLATFORM ROW, AS THE FIRMWARE DESCRIBED IT: where the description came from, who may take the device, its
// identity and ids, and what a claim of it mints - its MMIO ranges, port ranges and wired lines, then its
// connections - in the row's order, which is the order a driver is handed them in.
fn describe_platform(info: &DeviceInfo, entry: &mut DeviceEntry) {
	let part = &info.platform;
	entry.kind = RowKind::Platform;
	entry.source = match part.source {
		PLATFORM_SOURCE_KERNEL => PlatformSource::Kernel,
		PLATFORM_SOURCE_TABLE => PlatformSource::Table,
		PLATFORM_SOURCE_TREE => PlatformSource::Tree,
		PLATFORM_SOURCE_ACPI => PlatformSource::Acpi,
		_ => PlatformSource::None,
	};
	entry.state = match part.state {
		PLATFORM_STATE_CLAIMABLE => PlatformState::Claimable,
		PLATFORM_STATE_KERNEL_HELD => PlatformState::KernelHeld,
		PLATFORM_STATE_FIRMWARE_HELD => PlatformState::FirmwareHeld,
		PLATFORM_STATE_RESERVATION => PlatformState::Reservation,
		_ => PlatformState::None,
	};
	entry.identity = alloc::string::String::from_utf8_lossy(part.identity()).into_owned();
	for id in part.match_ids() {
		let kind = match id.kind {
			MATCH_ID_HID => PlatformIdKind::Hid,
			MATCH_ID_CID => PlatformIdKind::Cid,
			MATCH_ID_COMPATIBLE => PlatformIdKind::Compatible,
			MATCH_ID_TABLE => PlatformIdKind::Table,
			MATCH_ID_CLASS => PlatformIdKind::Class,
			MATCH_ID_IDENTITY => PlatformIdKind::Identity,
			MATCH_ID_SMBIOS => PlatformIdKind::Smbios,
			_ => PlatformIdKind::None,
		};
		entry.ids.push(PlatformId { kind, text: alloc::string::String::from_utf8_lossy(id.text()).into_owned() });
	}
	let resource = |kind, base: u64, length: u64, level: bool, active_low: bool, controller: u32| PlatformResource { kind, base, length, level, active_low, controller };
	for range in part.mmio() {
		entry.resources.push(resource(PlatformResourceKind::Mmio, range.base, range.len, false, false, u32::MAX));
	}
	for port in &info.ports[..(info.port_count as usize).min(info.ports.len())] {
		entry.resources.push(resource(PlatformResourceKind::Ports, u64::from(port.base), u64::from(port.len), false, false, u32::MAX));
	}
	for line in part.lines() {
		entry.resources.push(resource(PlatformResourceKind::Line, u64::from(line.number), 0, line.trigger == LINE_TRIGGER_LEVEL, line.polarity == LINE_POLARITY_LOW, u32::MAX));
	}
	for connection in part.connections() {
		let kind = match connection.kind {
			CONNECTION_I2C => PlatformResourceKind::I2c,
			CONNECTION_SPI => PlatformResourceKind::Spi,
			_ => PlatformResourceKind::GpioLine,
		};
		// A GPIO line's trigger is the device tree's own flags: 4 and 8 are levels, 2 and 8 active low.
		let gpio = connection.kind == CONNECTION_GPIO_LINE;
		entry.resources.push(resource(kind, u64::from(connection.value), 0, gpio && connection.trigger & 0xc != 0, gpio && connection.trigger & 0xa != 0, connection.controller));
	}
	entry.unresolved = part.flags & PLATFORM_FLAG_UNRESOLVED != 0;
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
	{
		send_blocking(bootstrap, b"DeviceService: online", 0);
	}

	// 2. take the roles the plan says this service is handed - here one, the channel clients
	//    reach us on. Checked against the GENERATED list rather than read by hand: the tag,
	//    the kernel object type and the rights are all things a receiver can check, and a
	//    bootstrap that is wrong is better refused by name than served with the wrong
	//    handle. A supervisor that dropped the channel instead (no clients this boot)
	//    reports as a missing role, and there is nothing left to serve either way.
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let service: u64 = roles[0];
	// The catalogue connection, if the plan handed one over. Its position in `BOOTSTRAP_ROLES` is
	// generated from the manifest, so this reads by name rather than by a number written twice.
	let bindings: u64 = if roles.len() > 1 { roles[1] } else { 0 };

	// 3. serve generated list/get requests until the client side closes.
	let mut devices: Devices = Devices { bindings };
	let mut request: [u8; 256] = [0u8; 256];
	let mut reply: [u8; 4096] = [0u8; 4096];
	serve_multi(service, &mut request, &mut reply, |_chan, req, handle, out, reply_handle| -> Option<usize> { device::dispatch(&mut devices, req, handle, out, reply_handle) });
	exit();
}
