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

use rt::*;

// The state DeviceManager tracks per discovered device.
const STATE_UNKNOWN: u8 = 0;
const STATE_ONLINE: u8 = 1;
const STATE_FAILED: u8 = 2;

// The most devices DeviceManager tracks (QEMU exposes a handful).
const MAX_DEVICES: usize = 8;

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

		// 2. launch the matching driver for each discovered device. The virtio-blk
		//    driver hands back a block-read service channel, which we route up to
		//    ServiceManager (it forwards it to StorageService).
		let mut block_client: u64 = 0;
		let mut net_client: u64 = 0;
		let mut gpu_client: u64 = 0;
		launch_drivers(&package, &mut buf, &mut block_client, &mut net_client, &mut gpu_client);

		// 3. report in once the devices are bound to drivers, transferring the block
		//    service channel up the boot chain, then the net driver's control channel and
		//    the gpu driver's display channel in follow-up messages (the report itself
		//    carries one handle; each `GPU`/`NET` handle is 0 when that device is absent).
		send_blocking(bootstrap, b"DeviceManager: online", block_client);
		send_blocking(bootstrap, b"NET", net_client);
		send_blocking(bootstrap, b"GPU", gpu_client);

		// 4. stand until ServiceManager asks us to stop (which also drops the driver
		//    channels, so the drivers shut down with us), then acknowledge and exit.
		if let Received::Message { .. } = recv_blocking(bootstrap, &mut buf) {
			send_blocking(bootstrap, b"DeviceManager: stopped", 0);
		}
	}
	exit();
}

// Enumerate the kernel device table and, for each device, spawn the matching driver
// from the package, handing it only that device's MMIO capability and info. Tracks
// each device's state (online once its driver reports in, failed otherwise) and
// prints a summary. The driver's "online" report is printed; it does not flow up
// the boot-chain report channel (which carries only the service lifecycle).
unsafe fn launch_drivers(package: &Package, buf: &mut [u8], block_client: &mut u64, net_client: &mut u64, gpu_client: &mut u64) {
	unsafe {
		let count: u64 = device_count();
		let mut state: [u8; MAX_DEVICES] = [STATE_UNKNOWN; MAX_DEVICES];
		let mut i: u64 = 0;
		while i < count && i < MAX_DEVICES as u64 {
			let idx: usize = i as usize;
			state[idx] = STATE_FAILED;
			let mut info: DeviceInfo = DeviceInfo::default();
			if !device_info(i, &mut info) {
				i += 1;
				continue;
			}
			let driver_name: &[u8] = driver_for(info.virtio_type);
			let elf: &[u8] = match package.lookup(driver_name) {
				Some(e) => e,
				None => {
					i += 1;
					continue;
				}
			};
			// launch the driver, restarting it if it crashes during bring-up. The
			// block driver's report carries a service channel we route upward.
			let mut handle: u64 = 0;
			if launch_one(i, &info, elf, driver_name, buf, &mut handle) {
				state[idx] = STATE_ONLINE;
				if driver_name == b"virtio_blk" {
					*block_client = handle;
				}
				if driver_name == b"virtio_net" {
					*net_client = handle;
				}
				if driver_name == b"virtio_gpu" {
					*gpu_client = handle;
				}
			}
			i += 1;
		}
		report_state(&state, count);
	}
}

// Launch (and, on a crash during bring-up, restart) the driver for device `i`,
// handing it only that device's MMIO capability and info. Returns true once the
// driver reports in. If a started driver crashes before reporting, the kernel tears
// it down and its bootstrap channel peer-closes (recv returns Closed); DeviceManager
// then re-acquires a fresh capability and respawns it, up to a few times - the
// driver crash/restart cycle. Drivers do not crash in normal operation, so the
// restart path is dormant on a healthy boot.
unsafe fn launch_one(i: u64, info: &DeviceInfo, elf: &[u8], driver_name: &[u8], buf: &mut [u8], service_handle: &mut u64) -> bool {
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
			// the interrupt-driven drivers (input, net) also need their device's Interrupt
			// capability: acquire it (which routes the IOAPIC) and transfer it as a second
			// "IRQ" message. The polling drivers (blk/console/gpu) get none, so their
			// device IRQs stay masked and never storm. The gpu shares a PCI INTx line with
			// virtio-input here, so it deliberately polls the display size for resizes
			// rather than acquiring that shared interrupt (which would hijack input's
			// IOAPIC routing and storm); see driver.virtio-gpu.
			if driver_name == b"virtio_input" || driver_name == b"virtio_net" {
				let irq: i64 = device_interrupt_acquire(i);
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
// reported in) out of those discovered - the device-state DeviceManager tracks.
unsafe fn report_state(state: &[u8; MAX_DEVICES], count: u64) {
	unsafe {
		let mut online: u32 = 0;
		for &s in state {
			if s == STATE_ONLINE {
				online += 1;
			}
		}
		print(b"DeviceManager: ");
		print_count(online);
		print(b" of ");
		print_count(count as u32);
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

// The init-package binary name of the driver for a virtio device type.
fn driver_for(virtio_type: u32) -> &'static [u8] {
	match virtio_type {
		VIRTIO_TYPE_NET => b"virtio_net",
		VIRTIO_TYPE_BLOCK => b"virtio_blk",
		VIRTIO_TYPE_CONSOLE => b"virtio_console",
		VIRTIO_TYPE_INPUT => b"virtio_input",
		VIRTIO_TYPE_GPU => b"virtio_gpu",
		_ => b"",
	}
}
