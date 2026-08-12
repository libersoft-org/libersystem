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
use ipc_client::ChannelTransport;
use proto::system::{OpenOpts, volume};
use rt::*;

include!(concat!(env!("OUT_DIR"), "/program_paths.rs"));

// The state DeviceManager tracks per discovered device.
// PRESENCE IS NOT ACTIVATION, and the states say which is which.
//
// It was three values and two of them answered different questions with the same silence: a device
// no driver binds and a device whose driver could not be loaded were both simply absent from the
// summary, so "this system does not support that card" and "the image is missing a file" looked
// identical to anyone reading it. The milestone's second item is that distinction, and these are
// the names it asks for.
const STATE_UNKNOWN: u8 = 0;
const STATE_ONLINE: u8 = 1;
const STATE_FAILED: u8 = 2;
// Present, and no registry entry matches it. The system does not support this device; nothing is
// missing and nothing is wrong. It costs no process, no mapped ELF and no capability.
const STATE_UNBOUND: u8 = 3;
// Present, matched, and the artifact the registry names could not be read off the volume. The image
// is inconsistent with its own registry - a different fault from an unsupported device, and the one
// an operator can act on.
const STATE_DRIVER_MISSING: u8 = 4;

// How many times DeviceManager restarts a driver that crashes during bring-up
// before giving up on its device.
const MAX_DRIVER_RESTARTS: u32 = 3;

// Everything this program holds on behalf of the development agent, so supervising it is one
// value rather than five more out-parameters threaded through driver launching.
//
// DeviceManager supervises the agent because DeviceManager started it: it exists exactly when
// the development channel device does, and nothing else in the system knows that. The agent
// dying is survivable, and this is what makes it so - without a supervisor its death would
// take the port with it, since the driver has nobody to hand bytes to and no way to acquire
// another.
//
// In a shipping build none of this is reachable: the device is not bound, no agent is
// started, and the capabilities below are never delivered.
#[cfg_attr(not(feature = "development"), allow(dead_code))]
#[derive(Default)]
struct DevAgent {
	// The agent's bootstrap. It is both how capabilities reach the agent and how this program
	// learns the agent is gone: it closes when the process ends, however it ended.
	bootstrap: u64,
	// The transport driver's bootstrap, over which a replacement agent's wire is handed down.
	// The driver keeps its device and its port across the gap and waits for that message.
	driver: u64,
	// A volume client of this program's own, kept past driver bring-up. The connection the
	// drivers were read through belongs to ServiceManager's message and is closed with it,
	// and a replacement has to be read off the volume long after that.
	storage: u64,
	// The capabilities delivered once, after the first agent was already running. They are
	// retained rather than forwarded and forgotten, because a replacement that could neither
	// launch a program nor answer a resolution query would be an agent in name only - and
	// nothing would deliver them a second time.
	launcher: u64,
	registry: u64,
	// The value every handshake reports so a tool can tell which boot answered it. It is drawn
	// here, once, and handed to each agent: it identifies the boot, and this program is what
	// lives as long as the boot does. An agent drawing its own would announce a reboot that did
	// not happen every time it was replaced.
	nonce: [u8; 8],
}

impl DevAgent {
	// Retain a capability and pass the live agent a copy of it. A copy rather than the handle
	// itself: this program has to be able to give the same capability to the next agent, and
	// only one agent is ever alive to use it.
	unsafe fn deliver(&self, tag: &[u8], held: u64) {
		unsafe {
			if self.bootstrap == 0 || held == 0 {
				return;
			}
			let Some(info) = object_info(held) else { return };
			let copy: i64 = duplicate(held, info.rights);
			if copy < 0 || !send_blocking(self.bootstrap, tag, copy as u64) {
				if copy >= 0 {
					close(copy as u64);
				}
				print(b"DeviceManager: the development agent did not take ");
				print(tag);
				print(b"\n");
			}
		}
	}

	unsafe fn hold_launcher(&mut self, handle: u64) {
		unsafe {
			if handle == 0 {
				return;
			}
			if self.bootstrap == 0 {
				close(handle);
				return;
			}
			self.launcher = handle;
			self.deliver(b"PERM", handle);
		}
	}

	unsafe fn hold_registry(&mut self, handle: u64) {
		unsafe {
			if handle == 0 {
				return;
			}
			if self.bootstrap == 0 {
				close(handle);
				return;
			}
			self.registry = handle;
			self.deliver(b"REG", handle);
		}
	}

	// The agent's bootstrap became readable. Anything it says is passed through to the console;
	// its closing means the process ended, and a fresh one takes its place.
	#[cfg(feature = "development")]
	unsafe fn supervise(&mut self, buf: &mut [u8]) {
		unsafe {
			match recv_blocking(self.bootstrap, buf) {
				Received::Message { len, .. } => {
					print(&buf[..len]);
					print(b"\n");
				}
				Received::Closed => self.restart(),
			}
		}
	}

	// Start a replacement and give it everything the one before it had, except what it held:
	// the registry was in the dead process's memory and is gone, which is the whole meaning of
	// a restart. A failure at any step leaves the port transport-only rather than half-wired,
	// and says so.
	#[cfg(feature = "development")]
	unsafe fn restart(&mut self) {
		unsafe {
			close(self.bootstrap);
			self.bootstrap = 0;
			if self.driver == 0 || self.storage == 0 {
				return;
			}
			let Some((driver_side, agent_side)) = channel() else { return };
			// The agent is started before the driver is told, so a start that fails leaves the
			// driver waiting for a channel that will be offered again rather than holding one
			// whose other end never appears.
			self.bootstrap = start_dev_agent(self.storage, agent_side, &self.nonce);
			if self.bootstrap == 0 || !send_blocking(self.driver, b"BYTES", driver_side) {
				close(driver_side);
				print(b"DeviceManager: the development agent did not restart; the control channel is transport-only\n");
				return;
			}
			self.deliver(b"PERM", self.launcher);
			self.deliver(b"REG", self.registry);
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. receive the init package shared buffer (to spawn drivers from) and map it.
		let (_pkg_handle, archive): (u64, &[u8]) = recv_package(bootstrap, &mut buf).unwrap_or_else(|| fail_bootstrap(bootstrap, b"package", b"init package not delivered"));
		let package: Package = Package::parse(archive).unwrap_or_else(|| fail_bootstrap(bootstrap, b"package", b"init package malformed"));
		// 1b. receive the power capability - a root-Domain handle carrying MANAGE - which this
		//     service holds only to delegate to the keyboard drivers. `SYS_SYSTEM_POWER`
		//     checks it, and the Power key is the one path to stopping the machine that must
		//     survive a wedged supervisor, so it cannot be routed through ServiceManager.
		let power: u64 = recv_tagged(bootstrap, &mut buf, b"POWER").unwrap_or_else(|| fail_bootstrap(bootstrap, b"power", b"missing power capability"));
		// 1b2. and the ConsoleInputSource capability, held only to delegate to the same two
		//      keyboard drivers. `SYS_CONSOLE_FEED` requires it: a keyboard without one types
		//      nothing rather than typing on an authority it does not hold. Optional, like the
		//      drivers' own handling of it, so a boot that granted none still starts.
		let console_input: u64 = recv_tagged(bootstrap, &mut buf, b"CONSOLE").unwrap_or(0);
		// 1b3. and the DeviceManager privilege, which is what `device_acquire`,
		//      `device_msix_acquire` and `interrupt_bind` now require. Without it this program takes
		//      no device out of the kernel and no driver is launched - which is the right failure:
		//      ungated, those syscalls handed any process the BAR of any PCI device, and on a
		//      machine with no IOMMU a DMA-capable one reaches memory the page tables were meant to
		//      isolate.
		let device_privilege: u64 = recv_tagged(bootstrap, &mut buf, b"DEVPRIV").unwrap_or(0);

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
		let mut usb_pointer: u64 = 0;
		let mut raw_keys: u64 = 0;
		// What this program holds on behalf of the development agent: its bootstrap, so the
		// launcher can be handed to it once PermissionManager exists - which is after this
		// program has finished starting drivers - and so its death is noticed and answered.
		let mut dev: DevAgent = DevAgent::default();
		launch_boot_drivers(&package, power, console_input, device_privilege, &mut buf, &mut block_client, &mut block2_client, &mut block3_client, &mut block4_client);

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
			// The development agent's bootstrap is watched alongside ServiceManager's. Its
			// closing is how this program learns the agent died, and answering that is this
			// program's job because this program is what started it.
			#[cfg(feature = "development")]
			if dev.bootstrap != 0 && wait_any(&[bootstrap, dev.bootstrap], 0) == 1 {
				dev.supervise(&mut buf);
				continue;
			}
			match recv_blocking(bootstrap, &mut buf) {
				Received::Message { len, handle } if len >= 7 && &buf[..7] == b"DRIVERS" => {
					launch_volume_drivers(handle, power, console_input, device_privilege, &mut buf, &mut net_client, &mut gpu_client, &mut snd_client, &mut input_client, &mut usb_client, &mut usbq_client, &mut usb_pointer, &mut raw_keys, &mut dev);
					if handle != 0 {
						close(handle);
					}
					send_blocking(bootstrap, b"NET", net_client);
					send_blocking(bootstrap, b"GPU", gpu_client);
					send_blocking(bootstrap, b"SND", snd_client);
					send_blocking(bootstrap, b"INPUT", input_client);
					send_blocking(bootstrap, b"USB", usb_client);
					send_blocking(bootstrap, b"USBBUS", usbq_client);
					// the xhci driver's pointer-event channel (a USB pointing device;
					// InputService folds it alongside the virtio pointer's).
					send_blocking(bootstrap, b"INPUT2", usb_pointer);
					send_blocking(bootstrap, b"KEYS", raw_keys);
				}
				// The development agent's launcher, delivered once PermissionManager is up.
				// Forwarded rather than held: this program has no use for it, and the agent
				// could not have been given one when it started.
				Received::Message { len, handle } if len >= 7 && &buf[..7] == b"DEVPERM" => dev.hold_launcher(handle),
				// The other end of the channel ProcessService already holds, so a launch can
				// ask the registry whether it has a generation of the artifact it is about to
				// read off the volume.
				Received::Message { len, handle } if len >= 6 && &buf[..6] == b"DEVREG" => dev.hold_registry(handle),
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

// Phase 1: enumerate the kernel device table and spawn the bootstrap block
// driver (virtio_blk) for each disk it backs, from the init package, handing it only that
// device's MMIO capability and info. Each disk's block-read service channel is routed up
// (system / media / iso / udf, in discovery order). The non-bootstrap drivers are skipped
// here - they load from the volume in phase 2, once it is mounted.
unsafe fn launch_boot_drivers(package: &Package, power: u64, console_input: u64, device_privilege: u64, buf: &mut [u8], block_client: &mut u64, block2_client: &mut u64, block3_client: &mut u64, block4_client: &mut u64) {
	unsafe {
		let count: u64 = device_count();
		let mut i: u64 = 0;
		while i < count {
			let mut info: DeviceInfo = DeviceInfo::default();
			if !device_info(i, &mut info) {
				i += 1;
				continue;
			}
			// PHASE ONE IS THE BOOT-CRITICAL LIFECYCLE, declared in the manifest, rather than the
			// name `virtio_blk` written here. That is the milestone's bootstrap exception stated as
			// a rule: only a driver needed to mount the system volume lives in `init.pkg`, and
			// `system-manifest` refuses a boot-critical driver staged anywhere else.
			let Some(entry) = registry_entry(&info) else {
				i += 1;
				continue;
			};
			if entry.lifecycle != Lifecycle::BootCritical {
				i += 1;
				continue;
			}
			let driver_name: &[u8] = entry.name;
			let elf: &[u8] = match package.lookup(entry.artifact) {
				Some(e) => e,
				None => {
					i += 1;
					continue;
				}
			};
			let mut handle: u64 = 0;
			let mut dm_chan: u64 = 0;
			if launch_one(i, &info, elf, driver_name, 0, power, console_input, device_privilege, buf, &mut handle, &mut dm_chan) {
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

// Phase 2: now that the system volume is mounted, load each non-bootstrap
// driver from vol://system/drivers/ through the StorageService client `storage` and spawn
// it with its device's MMIO capability. Their control / event channels are handed back for
// NetworkService, ConsoleService, AudioService, InputService and the USB StorageService
// instance, plus the xHCI driver's USB bus query channel (for the `lsusb` inventory) and
// its pointer-event channel (a USB pointing device, folded by InputService), and the
// merged raw-key consumer fed by every keyboard driver.
// Tracks each device's state and prints a summary.
#[allow(clippy::too_many_arguments)]
unsafe fn launch_volume_drivers(storage: u64, power: u64, console_input: u64, device_privilege: u64, buf: &mut [u8], net_client: &mut u64, gpu_client: &mut u64, snd_client: &mut u64, input_client: &mut u64, usb_client: &mut u64, usbq_client: &mut u64, usb_pointer: &mut u64, raw_keys: &mut u64, #[cfg_attr(not(feature = "development"), allow(unused_variables))] dev: &mut DevAgent) {
	unsafe {
		let (key_producer, key_consumer): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return,
		};
		*raw_keys = key_consumer;
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
			let candidates = registry_candidates(&info);
			if candidates.is_empty() {
				// Present and unsupported. Recorded rather than skipped: an unbound device is a
				// fact about this system, and leaving it out of the summary is what made it
				// indistinguishable from a driver that failed to load.
				state[idx] = STATE_UNBOUND;
				i += 1;
				continue;
			}
			if candidates[0].lifecycle == Lifecycle::BootCritical {
				// bound in phase 1, before there was a volume to load from; count them as online.
				state[idx] = STATE_ONLINE;
				i += 1;
				continue;
			}
			// EACH CANDIDATE IN TURN, most specific first, and the next one only after the last is
			// gone. The ELF is unmapped and its handle closed at the bottom of every attempt
			// whatever the outcome, so a fallback never runs beside the process it replaces.
			//
			// Every rejection is said: which driver, and why. A fallback that quietly succeeds hides
			// the fact that the preferred driver did not, which is how a machine comes to run on its
			// second choice for months with nobody aware of it.
			for entry in candidates {
				let driver_name: &[u8] = entry.name;
				state[idx] = STATE_FAILED;
				// load the driver's ELF off the volume, keep it mapped while we spawn from it.
				let loaded: Option<(u64, u64, usize)> = read_driver(storage, driver_name);
				let (file, mapped, size): (u64, u64, usize) = match loaded {
					Some(t) => t,
					None => {
						// The registry names an artifact the volume does not have. That is the image
						// disagreeing with itself, not a driver that ran and failed, and the two are
						// worth telling apart: one is a packaging fault and the other is a bug.
						state[idx] = STATE_DRIVER_MISSING;
						print(b"devmgr: ");
						print(driver_name);
						print(b" is named by the registry and not on the volume; trying the next candidate\n");
						continue;
					}
				};
				let elf: &[u8] = core::slice::from_raw_parts(mapped as *const u8, size);
				let mut handle: u64 = 0;
				let mut dm_chan: u64 = 0;
				if launch_one(i, &info, elf, driver_name, key_producer, power, console_input, device_privilege, buf, &mut handle, &mut dm_chan) {
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
					// The development channel driver hands up a raw byte channel, and the agent
					// that speaks the protocol over it is started here rather than by
					// ServiceManager. It exists exactly when the device does, it has no other
					// client, and its whole reason to be a separate process is to keep the
					// artifact registry out of the address space that holds a device capability -
					// so it is started where that device is bound, and nowhere else.
					#[cfg(feature = "development")]
					if driver_name == b"dev_channel" && handle != 0 {
						// The driver's bootstrap is kept rather than left to leak, because a
						// replacement agent's wire is handed down over it; and a volume connection of
						// this program's own is opened, because the one these drivers were read
						// through is closed as soon as this function returns.
						dev.driver = dm_chan;
						dev.storage = service_connect(storage).unwrap_or(0);
						// INSECURE by name, because that is what this is: an identifier that tells one
						// boot from another, not a secret. Asking for the secure one would refuse on
						// every machine with no hardware random source - which is two of the three
						// architectures - for a number that never needed to be unguessable.
						random_insecure(&mut dev.nonce);
						dev.bootstrap = start_dev_agent(dev.storage, handle, &dev.nonce);
						if dev.bootstrap == 0 {
							print(b"DeviceManager: development agent did not start; the control channel is transport-only\n");
						}
					}
					// The pointer flavour of virtio_input hands up an event channel (non-zero
					// handle); the keyboard flavour hands up nothing (handle 0), so a non-zero
					// virtio_input handle is the pointer's INPUT channel for InputService.
					if driver_name == b"virtio_input" && handle != 0 {
						*input_client = handle;
					}
					// The xhci driver hands up the USB stick's block-service channel (handle 0
					// when no mass-storage device is attached), routed to the usb StorageService,
					// then its USB bus query channel under "USBBUS" (the `lsusb` inventory) and
					// its pointer-event channel under "POINTER" (a USB pointing device).
					if driver_name == b"xhci" {
						if handle != 0 {
							*usb_client = handle;
						}
						if let Received::Message { len, handle: usbq } = recv_blocking(dm_chan, buf)
							&& len >= 6 && &buf[..6] == b"USBBUS"
						{
							*usbq_client = usbq;
						}
						if let Received::Message { len, handle: ptr } = recv_blocking(dm_chan, buf)
							&& len >= 7 && &buf[..7] == b"POINTER"
						{
							*usb_pointer = ptr;
						}
					}
				}
				unmap_object(file);
				close(file);
				// Bound. Nothing below this candidate is tried.
				if state[idx] == STATE_ONLINE {
					break;
				}
				print(b"devmgr: ");
				print(driver_name);
				print(b" did not bind; its resources are released and the next candidate follows\n");
			}
			i += 1;
		}
		close(key_producer);
		report_state(&state);
	}
}

// Start the development agent on the byte channel the development channel driver handed up.
// The channel becomes the agent's bootstrap: raw port bytes arrive on it and whole protocol
// frames go back, so the driver never learns what a frame is. The agent reports in once, and
// a failure to start is reported rather than retried - a development instance without its
// agent is still a usable guest, just one whose control channel carries nothing.
#[cfg(feature = "development")]
unsafe fn start_dev_agent(storage: u64, bytes: u64, nonce: &[u8; 8]) -> u64 {
	unsafe {
		let loaded: Option<(u64, u64, usize)> = read_driver(storage, b"dev_agent");
		let (file, mapped, size): (u64, u64, usize) = match loaded {
			Some(t) => t,
			None => return 0,
		};
		let elf: &[u8] = core::slice::from_raw_parts(mapped as *const u8, size);
		// The agent gets a bootstrap of its own and the byte channel transferred over it. The
		// byte channel is the wire: anything sent on it goes out of the port, so it is not a
		// place to report in, and DeviceManager must never read from it either - every byte on
		// it belongs to the agent.
		let (dm_side, agent_side): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => {
				unmap_object(file);
				close(file);
				return 0;
			}
		};
		let started: bool = spawn(elf, agent_side) >= 0;
		unmap_object(file);
		close(file);
		// The wire and the boot's identity in one message: the identity is not the agent's to
		// draw, since it outlives any one agent, and one message leaves no order to get wrong.
		let mut opening: [u8; 13] = [0u8; 13];
		opening[..5].copy_from_slice(b"BYTES");
		opening[5..].copy_from_slice(nonce);
		if !started || !send_blocking(dm_side, &opening, bytes) {
			return 0;
		}
		// A volume connection of its own, so the agent can read the installed artifact a
		// publication would shadow. A fresh connection rather than a duplicate of
		// DeviceManager's handle: a volume client is a request and reply channel, and two
		// readers on one endpoint would take each other's replies.
		let connection: u64 = match service_connect(storage) {
			Some(connection) => connection,
			None => return 0,
		};
		if !send_blocking(dm_side, b"STORAGE", connection) {
			return 0;
		}
		// The agent reports in before it serves, so a start that loaded but never ran is not
		// mistaken for a working one. The bootstrap stays open afterwards: dropping it is how
		// the agent would learn to shut down.
		let mut buf: [u8; 64] = [0u8; 64];
		match recv_blocking(dm_side, &mut buf) {
			Received::Message { len, .. } if len >= 5 && &buf[..5] == b"agent" => {
				print(&buf[..len]);
				print(b"\n");
				// Kept, not dropped: this is how the launcher reaches the agent later, and how
				// the agent would learn to shut down.
				dm_side
			}
			_ => 0,
		}
	}
}

// Open a manifest-declared driver path through the StorageService client and map its bytes,
// returning (file handle, mapped address, size) so the caller can spawn from the image and
// then release the mapping. None if the driver cannot be read.
unsafe fn read_driver(storage: u64, name: &[u8]) -> Option<(u64, u64, usize)> {
	unsafe {
		let name = core::str::from_utf8(name).ok()?;
		let opts: OpenOpts = OpenOpts { path: alloc::string::String::from(program_path(name)?), write: false, create: false };
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
unsafe fn launch_one(i: u64, info: &DeviceInfo, elf: &[u8], driver_name: &[u8], key_producer: u64, power: u64, console_input: u64, device_privilege: u64, buf: &mut [u8], service_handle: &mut u64, control_out: &mut u64) -> bool {
	unsafe {
		let info_size: usize = core::mem::size_of::<DeviceInfo>();
		let mut attempt: u32 = 0;
		loop {
			let cap: i64 = device_acquire(i, device_privilege);
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
			// the interrupt-driven drivers (virtio-input, virtio-net, virtio-snd, xhci,
			// virtio-gpu, dev-channel) also need their device's Interrupt capability,
			// transferred as a second "IRQ" message. Each takes its own per-device MSI-X
			// vector (edge-triggered, with no INTx sharing). The gpu routes only its CONFIG
			// vector to it (display changes); its control queue stays polled. The dev channel
			// is idle almost always, so it must block on its interrupt rather than poll: a
			// spinning driver starves the cooperative scheduler for the guest's whole life.
			// The remaining polling drivers (blk/console) get none, so their device IRQs stay
			// silent.
			let use_msix: bool = driver_name == b"virtio_input" || driver_name == b"virtio_net" || driver_name == b"virtio_snd" || driver_name == b"xhci" || driver_name == b"virtio_gpu" || driver_name == b"dev_channel";
			if use_msix {
				let irq: i64 = device_msix_acquire(i, device_privilege);
				if irq < 0 {
					return false;
				}
				buf[..3].copy_from_slice(b"IRQ");
				if !send_blocking(dm_side, &buf[..3], irq as u64) {
					return false;
				}
			}
			if driver_name == b"virtio_input" || driver_name == b"xhci" {
				let sink: i64 = duplicate(key_producer, RIGHT_SEND | RIGHT_TRANSFER);
				if sink < 0 || !send_blocking(dm_side, b"KEYS", sink as u64) {
					return false;
				}
				// The same two drivers own the Power key, so they get the capability that
				// makes it work. A duplicate per driver: two keyboards each hold their own,
				// and this service keeps the one it was handed.
				let grant: i64 = duplicate(power, RIGHT_MANAGE | RIGHT_TRANSFER);
				if grant < 0 || !send_blocking(dm_side, b"POWER", grant as u64) {
					return false;
				}
				// The capability that lets those keystrokes reach the console at all. A
				// duplicate per driver, for the same reason as POWER.
				if console_input != 0 {
					let feed: i64 = duplicate(console_input, RIGHT_TRANSFER);
					if feed < 0 || !send_blocking(dm_side, b"CONSOLE", feed as u64) {
						return false;
					}
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
		let mut unbound: u32 = 0;
		let mut missing: u32 = 0;
		for &s in state {
			match s {
				STATE_UNKNOWN => {}
				STATE_UNBOUND => unbound += 1,
				STATE_DRIVER_MISSING => {
					missing += 1;
					tracked += 1;
				}
				STATE_ONLINE => {
					online += 1;
					tracked += 1;
				}
				_ => tracked += 1,
			}
		}
		print(b"DeviceManager: ");
		print_count(online);
		print(b" of ");
		print_count(tracked);
		print(b" device(s) online");
		// SAID, not counted into the same number. An unsupported device is not a failure and a
		// missing artifact is not an unsupported device; folding either into "online out of
		// tracked" is what made both invisible.
		if unbound > 0 {
			print(b", ");
			print_count(unbound);
			print(b" unbound");
		}
		if missing > 0 {
			print(b", ");
			print_count(missing);
			print(b" driver-missing");
		}
		print(b"\n");
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

// THE DRIVER REGISTRY, generated from the manifest. See `build.rs`.
//
// It replaces `driver_for(&DeviceInfo)`, which was seven `match` arms of virtio type numbers plus
// one hardcoded PCI address for the development console - driver selection written in Rust rather
// than declared, so adding a driver meant editing this file, and nothing could check that the
// image's drivers and the code's list agreed. `system-manifest` refuses a duplicate identity, an
// ambiguous equal-priority match, an empty rule set, a rule naming something discovery cannot
// answer, and a boot-critical driver staged on the volume it exists to mount, all before this table
// is emitted.

// What kind of thing an entry binds to. Carried through selection because supervision and start
// order depend on it, which a `&'static [u8]` name could not express.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
	BootCritical,
	Controller,
	Function,
	Interface,
}

// How specific a match is. Ordered: a quirk outranks an exact match, which outranks the generic
// path. The registry refuses two entries that tie, so ordering is total where it is consulted.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Priority {
	Generic,
	Exact,
	Quirk,
}

// ONE RULE IS A CONJUNCTION: every predicate that is present must hold, and `None` means "do not
// ask" rather than "must be absent". A driver's rule list is the disjunction.
//
// Both halves are needed and the tree proved it: the development console is selected by its virtio
// type AND its pinned address, which a set of single-question rules could only OR together - and
// OR-ing them would bind any device at that address to the console driver.
#[derive(Clone, Copy)]
struct Address {
	bus: u8,
	dev: u8,
	func: u8,
}

#[derive(Clone, Copy)]
struct Rule {
	// One field, because `DeviceInfo` has one: the virtio kinds and xHCI share a number space.
	device_type: Option<u32>,
	// The standards identity, which discovery now carries. It was declarable and never satisfied
	// while `DeviceInfo` had no class byte; the kernel had resolved all three for every function
	// since the first PCI scan and kept them for `lspci` alone.
	pci_class: Option<u8>,
	pci_subclass: Option<u8>,
	pci_interface: Option<u8>,
	pci_address: Option<Address>,
}

impl Rule {
	fn matches(self, info: &DeviceInfo) -> bool {
		if self.pci_class.is_some_and(|class| info.class != class) {
			return false;
		}
		if self.pci_subclass.is_some_and(|subclass| info.subclass != subclass) {
			return false;
		}
		if self.pci_interface.is_some_and(|interface| info.prog_if != interface) {
			return false;
		}
		if self.device_type.is_some_and(|kind| info.device_type != kind) {
			return false;
		}
		if let Some(address) = self.pci_address
			&& (info.bus != address.bus || info.dev != address.dev || info.func != address.func)
		{
			return false;
		}
		true
	}
}

struct Entry {
	name: &'static [u8],
	// The staged file, which is what the loader asks for - not derived from the name, because the
	// two differ for a pinned driver and deriving it is how they drift.
	artifact: &'static [u8],
	lifecycle: Lifecycle,
	priority: Priority,
	rules: &'static [Rule],
}

include!(concat!(env!("OUT_DIR"), "/driver_registry.rs"));

// The registry entry that binds `info`, or None when nothing in the image does.
//
// EVERY candidate is collected and the most specific wins, rather than the first that matches -
// "never choose by enumeration order" is the milestone's rule and a first-match loop is exactly
// that. A tie cannot happen: `system-manifest` refuses two entries whose rules overlap at one
// priority, which is decidable because the rule set is closed. The assertion here is what keeps
// that proof honest if the check is ever weakened.
fn registry_entry(info: &DeviceInfo) -> Option<&'static Entry> {
	registry_candidates(info).first().copied()
}

// Every entry that matches `info`, most specific first.
//
// The ORDER is the arbitration and the LIST is the fallback. A bind that fails may try the next
// compatible candidate - but only after the first attempt's resources are gone, which the caller
// enforces by unmapping and closing before it comes back here. Trying them in parallel, or trying
// the next while the first still holds the device, is how two drivers come to own one controller.
//
// Nothing is launched to find out whether it fits: the choice is made from metadata alone, which is
// the item's "probe metadata before process creation rather than launching every possible driver to
// see which one succeeds".
//
// A tie is refused rather than broken. `system-manifest` proves at registry-build time that two
// entries cannot overlap at one priority, so this cannot fire - and it stays here because a proof
// that nothing checks is a comment.
fn registry_candidates(info: &DeviceInfo) -> Vec<&'static Entry> {
	let mut candidates: Vec<&'static Entry> = DRIVER_REGISTRY.iter().filter(|entry| entry.rules.iter().any(|rule| rule.matches(info))).collect();
	candidates.sort_by(|left, right| right.priority.cmp(&left.priority));
	for pair in candidates.windows(2) {
		if pair[0].priority == pair[1].priority {
			unsafe { print(b"devmgr: two registry entries match one device at the same priority; leaving it unbound\n") };
			return Vec::new();
		}
	}
	candidates
}
