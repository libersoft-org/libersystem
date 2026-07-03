// DeviceManager - the userspace device supervisor.
//
// ServiceManager starts this program from the init package, hands it a bootstrap
// channel, and over it a view of the init package (so it can spawn drivers from
// it). DeviceManager enumerates the devices the kernel discovered on the PCI bus
// (over the device syscalls) and launches the matching userspace driver for each,
// handing that driver only its own device's MMIO capability. It then reports in
// and stands; ServiceManager exercises the stop path on it (sending "STOP", to
// which it replies "DeviceManager: stopped" and exits). Device-state tracking and
// reacting to a driver crash grow here in later steps.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::{OpenOpts, volume};
use rt::*;

// Where the non-bootstrap driver binaries live on the system volume (M61 box 8): a
// named driver is loaded from `<DRIVER_DIR><name>`.
const DRIVER_DIR: &str = "vol://system/drivers/";

// The state DeviceManager tracks per discovered device.
const STATE_UNKNOWN: u8 = 0;
const STATE_ONLINE: u8 = 1;
const STATE_FAILED: u8 = 2;

// How many times DeviceManager restarts a driver that crashes during bring-up
// before giving up on its device.
const MAX_DRIVER_RESTARTS: u32 = 3;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. receive the init package shared buffer (to spawn drivers from) and map it.
		let (_pkg_handle, archive): (u64, &[u8]) = recv_package(bootstrap, &mut buf).unwrap_or_else(|| exit());
		let package: Package = Package::parse(archive).unwrap_or_else(|| exit());

		// 2. phase 1: launch the bootstrap block driver (virtio_blk) for each disk it backs.
		//    It hands back a block-read service channel, which we route up to ServiceManager
		//    (it forwards it to StorageService). The non-bootstrap drivers cannot load yet -
		//    they live on the system volume, which is only mountable once virtio_blk and
		//    StorageService are up - so they wait for phase 2 below.
		let mut block_client: u64 = 0;
		let mut block2_client: u64 = 0;
		let mut block3_client: u64 = 0;
		let mut block4_client: u64 = 0;
		let mut net_client: u64 = 0;
		let mut gpu_client: u64 = 0;
		let mut snd_client: u64 = 0;
		let mut input_client: u64 = 0;
		let mut usb_client: u64 = 0;
		let mut usbq_client: u64 = 0;
		launch_boot_drivers(&package, &mut buf, &mut block_client, &mut block2_client, &mut block3_client, &mut block4_client);

		// 3. report in once the disks are bound, transferring the block service channel up
		//    the boot chain, then the second/third/fourth block disks' service channels (the
		//    report itself carries one handle; each `BLOCK2`/`BLOCK3`/`BLOCK4` handle is 0
		//    when that disk is absent). The net / gpu / snd / input driver channels follow in
		//    phase 2, once the volume they load from is mounted.
		send_blocking(bootstrap, b"DeviceManager: online", block_client);
		send_blocking(bootstrap, b"BLOCK2", block2_client);
		send_blocking(bootstrap, b"BLOCK3", block3_client);
		send_blocking(bootstrap, b"BLOCK4", block4_client);

		// 4. stand until ServiceManager drives phase 2 (a "DRIVERS" message carrying a
		//    StorageService client, once the volume is up: we load the non-bootstrap drivers
		//    from vol://system/drivers/ and hand their channels up) or asks us to stop (which
		//    also drops the driver channels, so the drivers shut down with us).
		loop {
			match recv_blocking(bootstrap, &mut buf) {
				Received::Message { len, handle } if len >= 7 && &buf[..7] == b"DRIVERS" => {
					launch_volume_drivers(handle, &mut buf, &mut net_client, &mut gpu_client, &mut snd_client, &mut input_client, &mut usb_client, &mut usbq_client);
					if handle != 0 {
						close(handle);
					}
					send_blocking(bootstrap, b"NET", net_client);
					send_blocking(bootstrap, b"GPU", gpu_client);
					send_blocking(bootstrap, b"SND", snd_client);
					send_blocking(bootstrap, b"INPUT", input_client);
					send_blocking(bootstrap, b"USB", usb_client);
					send_blocking(bootstrap, b"USBBUS", usbq_client);
				}
				Received::Message { .. } => {
					send_blocking(bootstrap, b"DeviceManager: stopped", 0);
					break;
				}
				Received::Closed => break,
			}
		}
	}
	exit();
}

// Phase 1 (M61 box 8): enumerate the kernel device table and spawn the bootstrap block
// driver (virtio_blk) for each disk it backs, from the init package, handing it only that
// device's MMIO capability and info. Each disk's block-read service channel is routed up
// (system / media / iso / udf, in discovery order). The non-bootstrap drivers are skipped
// here - they load from the volume in phase 2, once it is mounted.
unsafe fn launch_boot_drivers(package: &Package, buf: &mut [u8], block_client: &mut u64, block2_client: &mut u64, block3_client: &mut u64, block4_client: &mut u64) {
	unsafe {
		let count: u64 = device_count();
		let mut i: u64 = 0;
		while i < count {
			let mut info: DeviceInfo = DeviceInfo::default();
			if !device_info(i, &mut info) {
				i += 1;
				continue;
			}
			let driver_name: &[u8] = driver_for(info.device_type);
			if driver_name != b"virtio_blk" {
				i += 1;
				continue;
			}
			let elf: &[u8] = match package.lookup(driver_name) {
				Some(e) => e,
				None => {
					i += 1;
					continue;
				}
			};
			let mut handle: u64 = 0;
			let mut dm_chan: u64 = 0;
			if launch_one(i, &info, elf, driver_name, buf, &mut handle, &mut dm_chan) {
				// the first virtio-blk disk is the writable system volume; a second is routed
				// up separately as the read-only FAT media volume, a third as the read-only
				// ISO9660 volume, a fourth as the read-only UDF volume.
				if *block_client == 0 {
					*block_client = handle;
				} else if *block2_client == 0 {
					*block2_client = handle;
				} else if *block3_client == 0 {
					*block3_client = handle;
				} else if *block4_client == 0 {
					*block4_client = handle;
				}
			}
			i += 1;
		}
	}
}

// Phase 2 (M61 box 8): now that the system volume is mounted, load each non-bootstrap
// driver from vol://system/drivers/ through the StorageService client `storage` and spawn
// it with its device's MMIO capability. Their control / event channels are handed back for
// NetworkService, ConsoleService, AudioService, InputService and the USB StorageService
// instance, plus the xHCI driver's USB bus query channel (for the `lsusb` inventory).
// Tracks each device's state and prints a summary.
unsafe fn launch_volume_drivers(storage: u64, buf: &mut [u8], net_client: &mut u64, gpu_client: &mut u64, snd_client: &mut u64, input_client: &mut u64, usb_client: &mut u64, usbq_client: &mut u64) {
	unsafe {
		let count: u64 = device_count();
		// per-device state, sized by what the kernel actually discovered - the bus is
		// the only bound, never an artificial cap that would silently skip devices.
		let mut state: Vec<u8> = alloc::vec![STATE_UNKNOWN; count as usize];
		let mut i: u64 = 0;
		while i < count {
			let idx: usize = i as usize;
			let mut info: DeviceInfo = DeviceInfo::default();
			if !device_info(i, &mut info) {
				i += 1;
				continue;
			}
			let driver_name: &[u8] = driver_for(info.device_type);
			if driver_name.is_empty() {
				// a device with no userspace driver yet (e.g. the xHCI controller until
				// its driver lands): skip it, leaving it out of the online summary.
				i += 1;
				continue;
			}
			if driver_name == b"virtio_blk" {
				// the disks are bound in phase 1; count them as online in the summary.
				state[idx] = STATE_ONLINE;
				i += 1;
				continue;
			}
			state[idx] = STATE_FAILED;
			// load the driver's ELF off the volume, keep it mapped while we spawn from it.
			let loaded: Option<(u64, u64, usize)> = read_driver(storage, driver_name);
			let (file, mapped, size): (u64, u64, usize) = match loaded {
				Some(t) => t,
				None => {
					i += 1;
					continue;
				}
			};
			let elf: &[u8] = core::slice::from_raw_parts(mapped as *const u8, size);
			let mut handle: u64 = 0;
			let mut dm_chan: u64 = 0;
			if launch_one(i, &info, elf, driver_name, buf, &mut handle, &mut dm_chan) {
				state[idx] = STATE_ONLINE;
				if driver_name == b"virtio_net" {
					*net_client = handle;
				}
				if driver_name == b"virtio_gpu" {
					*gpu_client = handle;
				}
				if driver_name == b"virtio_snd" {
					*snd_client = handle;
				}
				// The pointer flavour of virtio_input hands up an event channel (non-zero
				// handle); the keyboard flavour hands up nothing (handle 0), so a non-zero
				// virtio_input handle is the pointer's INPUT channel for InputService.
				if driver_name == b"virtio_input" && handle != 0 {
					*input_client = handle;
				}
				// The xhci driver hands up the USB stick's block-service channel (handle 0
				// when no mass-storage device is attached), routed to the usb StorageService,
				// then its USB bus query channel under "USBBUS" (the `lsusb` inventory).
				if driver_name == b"xhci" {
					if handle != 0 {
						*usb_client = handle;
					}
					if let Received::Message { len, handle: usbq } = recv_blocking(dm_chan, buf)
						&& len >= 6 && &buf[..6] == b"USBBUS"
					{
						*usbq_client = usbq;
					}
				}
			}
			unmap_object(file);
			close(file);
			i += 1;
		}
		report_state(&state);
	}
}

// Open vol://system/drivers/<name> through the StorageService client and map its bytes,
// returning (file handle, mapped address, size) so the caller can spawn from the image and
// then release the mapping. None if the driver cannot be read.
unsafe fn read_driver(storage: u64, name: &[u8]) -> Option<(u64, u64, usize)> {
	unsafe {
		let mut path: alloc::string::String = alloc::string::String::from(DRIVER_DIR);
		path.push_str(&alloc::string::String::from_utf8_lossy(name));
		let opts: OpenOpts = OpenOpts { path, write: false, create: false };
		let result = match volume::Client::new(ChannelTransport { chan: storage }).open(&opts) {
			Some(Ok(r)) => r,
			_ => return None,
		};
		if result.file == 0 || result.size == 0 {
			if result.file != 0 {
				close(result.file);
			}
			return None;
		}
		let mapped: u64 = match map_object(result.file) {
			Some(base) => base,
			None => {
				close(result.file);
				return None;
			}
		};
		Some((result.file, mapped, result.size as usize))
	}
}

// Launch (and, on a crash during bring-up, restart) the driver for device `i`,
// handing it only that device's MMIO capability and info. Returns true once the
// driver reports in. If a started driver crashes before reporting, the kernel tears
// it down and its bootstrap channel peer-closes (recv returns Closed); DeviceManager
// then re-acquires a fresh capability and respawns it, up to a few times - the
// driver crash/restart cycle. Drivers do not crash in normal operation, so the
// restart path is dormant on a healthy boot.
unsafe fn launch_one(i: u64, info: &DeviceInfo, elf: &[u8], driver_name: &[u8], buf: &mut [u8], service_handle: &mut u64, control_out: &mut u64) -> bool {
	unsafe {
		let info_size: usize = core::mem::size_of::<DeviceInfo>();
		let mut attempt: u32 = 0;
		loop {
			let cap: i64 = device_acquire(i);
			let (dm_side, driver_side): (u64, u64) = match channel() {
				Some(pair) => pair,
				None => return false,
			};
			if cap < 0 || spawn(elf, driver_side) < 0 {
				return false;
			}
			// hand the driver "DEVICE" + its DeviceInfo + the transferred MMIO cap.
			buf[..6].copy_from_slice(b"DEVICE");
			let info_bytes: &[u8] = core::slice::from_raw_parts(info as *const DeviceInfo as *const u8, info_size);
			buf[6..6 + info_size].copy_from_slice(info_bytes);
			if !send_blocking(dm_side, &buf[..6 + info_size], cap as u64) {
				return false;
			}
			// the interrupt-driven drivers (virtio-input, virtio-net, virtio-snd, xhci)
			// also need their device's Interrupt capability, transferred as a second
			// "IRQ" message. Each takes its own per-device MSI-X vector (edge-triggered,
			// with no INTx sharing). The polling drivers (blk/console/gpu) get none, so
			// their device IRQs stay silent. The gpu takes no interrupt at all - it polls
			// the display size for resizes rather than acquiring one; see driver.virtio-gpu.
			let use_msix: bool = driver_name == b"virtio_input" || driver_name == b"virtio_net" || driver_name == b"virtio_snd" || driver_name == b"xhci";
			if use_msix {
				let irq: i64 = device_msix_acquire(i);
				if irq < 0 {
					return false;
				}
				buf[..3].copy_from_slice(b"IRQ");
				if !send_blocking(dm_side, &buf[..3], irq as u64) {
					return false;
				}
			}
			match recv_blocking(dm_side, buf) {
				Received::Message { len, handle } => {
					*service_handle = handle;
					*control_out = dm_side;
					print(&buf[..len]);
					print(b"\n");
					return true;
				}
				Received::Closed => {
					// the driver crashed before reporting in: restart it a few times.
					if attempt >= MAX_DRIVER_RESTARTS {
						return false;
					}
					attempt += 1;
					print(b"DeviceManager: restarting ");
					print(driver_name);
					print(b"\n");
				}
			}
		}
	}
}

// Print a one-line summary of how many devices are online (their driver bound and
// reported in) out of those with a driver to bind - the device-state DeviceManager
// tracks. Devices with no userspace driver yet stay unknown and are not counted.
unsafe fn report_state(state: &[u8]) {
	unsafe {
		let mut online: u32 = 0;
		let mut tracked: u32 = 0;
		for &s in state {
			if s != STATE_UNKNOWN {
				tracked += 1;
			}
			if s == STATE_ONLINE {
				online += 1;
			}
		}
		print(b"DeviceManager: ");
		print_count(online);
		print(b" of ");
		print_count(tracked);
		print(b" device(s) online\n");
	}
}

// Print a small non-negative count in decimal (one or two digits suffice for the
// handful of devices QEMU exposes).
unsafe fn print_count(n: u32) {
	unsafe {
		if n >= 10 {
			print(&[b'0' + (n / 10) as u8]);
		}
		print(&[b'0' + (n % 10) as u8]);
	}
}

// The binary name of the driver for a device type; empty when no userspace driver
// exists for it yet.
fn driver_for(device_type: u32) -> &'static [u8] {
	match device_type {
		VIRTIO_TYPE_NET => b"virtio_net",
		VIRTIO_TYPE_BLOCK => b"virtio_blk",
		VIRTIO_TYPE_CONSOLE => b"virtio_console",
		VIRTIO_TYPE_INPUT => b"virtio_input",
		VIRTIO_TYPE_GPU => b"virtio_gpu",
		VIRTIO_TYPE_SOUND => b"virtio_snd",
		DEVICE_TYPE_XHCI => b"xhci",
		_ => b"",
	}
}
