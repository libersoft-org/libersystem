// driver.virtio-mem - memory a virtual machine's host plugs in at run time (virtio device 24), brought online block by
// block within an allowance its policy grants, served as the `hotplug-device` contract to ResourceManager.
//
// A BLOCK COMES ONLINE ONLY ONCE THE HOST HAS PLUGGED IT, AND GOES BACK ONLY ONCE NOTHING HOLDS IT. Plugging is a request
// the host acknowledges, then the kernel's region brings the block online; giving back is the kernel taking the block
// offline - which it refuses while one frame of it is in use - then the request the host acknowledges. So whatever the
// host asks back, memory a process, a buffer or a device holds stays where it is until it is freed; the driver offers
// the highest blocks first and moves on past a block in use.
//
// THE TARGET IS THE SMALLEST of what the host requests, what the policy allows and what the region can use now, and it
// is read again after every step: a host that changes its mind is followed one step later. With no policy connected
// nothing is plugged.
//
// A RESTARTED DRIVER FINDS WHAT ITS PREDECESSOR LEFT: the kernel hands it the same region with what is online still
// online, and a block the device holds plugged that the kernel does not have online - a predecessor that died between
// the two - is unplugged. A first start with nothing online asks the device to unplug everything.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use drivers::common;
use drivers::memplug::{self, Config, Step};
use drivers::virtio::{Queue, Virtio};
use proto::generated::liber::hotplug_device::v1::{HotplugStatus, hotplug_device};
use proto::system::Error;
use rt::*;

const REQUEST_QUEUE: u16 = 0;
const PAGE: u64 = 4096;
const TICK: u64 = 10;
// Where the request and the answer lie in the driver's page.
const REQUEST_AT: u64 = 0;
const RESPONSE_AT: u64 = 64;

#[derive(Clone, Copy)]
struct Buffer {
	virt: u64,
	phys: u64,
}

struct Mem {
	device: Virtio,
	queue: Queue,
	page: Buffer,
	// The kernel's region: the authority to bring blocks online and take them offline.
	region: u64,
	block_size: u64,
	addr: u64,
	region_size: u64,
	online: Vec<bool>,
	// Blocks offered back this tick whose frames were in use.
	held: Vec<bool>,
	allowed: u64,
	in_use: u64,
	said_error: bool,
	// Whether the last steps plugged or unplugged anything: a block skipped for being in use is no progress.
	moved: bool,
}

impl Mem {
	fn config(&self) -> Option<Config> {
		let mut bytes = [0u8; memplug::CONFIG_LEN];
		for (at, byte) in bytes.iter_mut().enumerate() {
			*byte = self.device.config_read(at as u64);
		}
		Config::parse(&bytes)
	}

	// ONE REQUEST AND ITS ANSWER.
	fn ask(&mut self, kind: u16, block: usize, count: usize) -> Option<(u16, u16)> {
		let request = memplug::request(kind, self.addr + block as u64 * self.block_size, count as u16);
		// SAFETY: this driver's own DMA page; both offsets lie inside it.
		unsafe {
			core::ptr::copy_nonoverlapping(request.as_ptr(), (self.page.virt + REQUEST_AT) as *mut u8, request.len());
			core::ptr::write_bytes((self.page.virt + RESPONSE_AT) as *mut u8, 0xFF, memplug::RESPONSE_LEN);
		}
		let chain: [(u64, u32, bool); 2] = [(self.page.phys + REQUEST_AT, memplug::REQUEST_LEN as u32, false), (self.page.phys + RESPONSE_AT, memplug::RESPONSE_LEN as u32, true)];
		let mut used = [0u32; 1];
		self.queue.submit_chains(&[&chain], None, &mut used).ok()?;
		// SAFETY: as above; the device has given the chain back.
		let answer = unsafe { core::slice::from_raw_parts((self.page.virt + RESPONSE_AT) as *const u8, memplug::RESPONSE_LEN) };
		memplug::response(answer)
	}

	fn plugged(&self) -> u64 {
		self.online.iter().filter(|online| **online).count() as u64 * self.block_size
	}

	fn status(&self) -> HotplugStatus {
		let requested = self.config().map_or(0, |config| config.requested_size);
		HotplugStatus { block_size: self.block_size, region_size: self.region_size, requested, plugged: self.plugged(), allowed: self.allowed, in_use: self.in_use }
	}

	// ONE STEP TOWARD THE TARGET: true when something moved, so the next follows at once.
	fn step(&mut self) -> bool {
		let Some(config) = self.config() else { return false };
		let blocks = |bytes: u64| (bytes / self.block_size) as usize;
		let requested = blocks(config.requested_size);
		let allowed = blocks(self.allowed);
		match memplug::step(&self.online, requested, allowed, config.usable_blocks(), &self.held) {
			Step::Hold => {
				let plugged = self.online.iter().filter(|online| **online).count();
				self.in_use = plugged.saturating_sub(requested.min(allowed)) as u64 * self.block_size;
				false
			}
			Step::Plug(first, count) => self.plug(first, count),
			Step::Unplug(block) => self.unplug(block),
		}
	}

	fn plug(&mut self, first: usize, count: usize) -> bool {
		match self.ask(memplug::REQ_PLUG, first, count) {
			Some((memplug::RESP_ACK, _)) => {}
			// THE HOST SAID NOT NOW: asked again at the next tick.
			Some((memplug::RESP_NACK | memplug::RESP_BUSY, _)) => return false,
			_ => {
				if !core::mem::replace(&mut self.said_error, true) {
					print(b"driver.virtio-mem: the device refused a plug this driver asked for - nothing is brought online\n");
				}
				return false;
			}
		}
		let onlined = unsafe { syscall(SYS_MEMORY_PLUG_ONLINE, self.region, first as u64, count as u64, 0) } as i64;
		if onlined != 0 {
			// THE KERNEL COULD NOT TAKE IT: the host is given it straight back.
			let _ = self.ask(memplug::REQ_UNPLUG, first, count);
			print(b"driver.virtio-mem: the kernel could not bring plugged blocks online - given back\n");
			return false;
		}
		for block in &mut self.online[first..first + count] {
			*block = true;
		}
		self.moved = true;
		true
	}

	fn unplug(&mut self, block: usize) -> bool {
		let offlined = unsafe { syscall(SYS_MEMORY_PLUG_OFFLINE, self.region, block as u64, 1, 0) } as i64;
		if offlined == ERR_WOULD_BLOCK {
			// SOMETHING HOLDS A FRAME OF IT: it stays, and the next block down is offered instead.
			self.held[block] = true;
			return true;
		}
		if offlined != 0 {
			self.held[block] = true;
			return false;
		}
		match self.ask(memplug::REQ_UNPLUG, block, 1) {
			Some((memplug::RESP_ACK, _)) => {
				self.online[block] = false;
				self.moved = true;
				true
			}
			_ => {
				// THE HOST KEPT IT PLUGGED: online again, since it is still memory this machine has.
				let _ = unsafe { syscall(SYS_MEMORY_PLUG_ONLINE, self.region, block as u64, 1, 0) };
				self.held[block] = true;
				false
			}
		}
	}

	// AT THE START: what the kernel has online is the truth; whatever else the device holds plugged is unplugged.
	fn reconcile(&mut self, plugged_size: u64) {
		if !self.online.iter().any(|online| *online) {
			if plugged_size != 0 {
				let _ = self.ask(memplug::REQ_UNPLUG_ALL, 0, 0);
			}
			return;
		}
		for block in 0..self.online.len() {
			if self.online[block] {
				continue;
			}
			if let Some((memplug::RESP_ACK, state)) = self.ask(memplug::REQ_STATE, block, 1)
				&& state != memplug::STATE_UNPLUGGED
			{
				let _ = self.ask(memplug::REQ_UNPLUG, block, 1);
			}
		}
	}
}

impl hotplug_device::Service for Mem {
	fn status(&mut self) -> Result<HotplugStatus, Error> {
		Ok(Mem::status(self))
	}

	fn allow(&mut self, bytes: u64) -> Result<HotplugStatus, Error> {
		self.allowed = bytes;
		Ok(Mem::status(self))
	}
}

fn serve(mem: &mut Mem, serving: &common::Serving, index: usize, buf: &mut [u8], reply: &mut [u8]) -> bool {
	let channel = serving.at(index);
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	let mut reply_handles = wire::Handles::new();
	if let Some(written) = hotplug_device::dispatch(mem, &buf[..len], &mut handles, reply, &mut reply_handles) {
		send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
	}
	true
}

struct Sleep<'a> {
	mem: &'a mut Mem,
	serving: &'a mut common::Serving,
	stopped: bool,
}

impl common::SleepStep for Sleep<'_> {
	fn suspend(&mut self, request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		match self.mem.device.sleep(request) {
			Some(stopped) => {
				self.stopped = stopped;
				driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
			}
			None => driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Refused(driver_protocol::DriverFailureCode::DeviceNotResponding), awake_by_ms: 0 },
		}
	}

	// A SLEEP KEEPS THE MEMORY: the plugged blocks are the machine's RAM through it, and the device holds them plugged.
	fn resume(&mut self, lost_power: bool) -> bool {
		self.mem.device.wake(&mut [&mut self.mem.queue], lost_power, core::mem::take(&mut self.stopped))
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(self.serving)
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let (bind, resources) = common::handshake(bootstrap);
		let device: Virtio = common::bringup_bound(bootstrap, &bind, &resources, memplug::FEATURE_ACPI_PXM | memplug::FEATURE_UNPLUGGED_INACCESSIBLE);
		let mut bytes = [0u8; memplug::CONFIG_LEN];
		for (at, byte) in bytes.iter_mut().enumerate() {
			*byte = device.config_read(at as u64);
		}
		let Some(config) = Config::parse(&bytes).filter(Config::usable) else {
			print(b"driver.virtio-mem: the device's region is not whole blocks this driver can bring online - it is not driven\n");
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::UnsupportedDevice);
		};
		let region = syscall(SYS_MEMORY_PLUG_REGION, resources.device, config.addr, config.region_size, config.block_size);
		if sys_is_err(region) {
			print(b"driver.virtio-mem: the kernel would not take the device's region - it is not driven\n");
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
		}
		let Some(queue) = device.setup_queue(REQUEST_QUEUE) else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable) };
		let Some((_, virt, phys)) = dma_buffer_for(device.capability, PAGE) else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::OutOfMemory) };
		let blocks = config.blocks();
		let mut states = alloc::vec![0u8; blocks];
		let counted = syscall(SYS_MEMORY_PLUG_STATE, region, states.as_mut_ptr() as u64, blocks as u64, 0) as i64;
		if counted != blocks as i64 {
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
		}
		let online: Vec<bool> = states.iter().map(|state| *state != 0).collect();
		let mut mem = Mem { device, queue, page: Buffer { virt, phys }, region, block_size: config.block_size, addr: config.addr, region_size: config.region_size, online, held: alloc::vec![false; blocks], allowed: 0, in_use: 0, said_error: false, moved: false };
		mem.device.driver_ok();
		mem.reconcile(config.plugged_size);
		let Some((mine, far)) = channel() else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::OutOfMemory) };
		let mut detail: common::Bounded<40> = common::Bounded::new();
		detail.decimal(config.region_size >> 20);
		detail.push(b" MiB in ");
		detail.decimal(config.block_size >> 20);
		detail.push(b" MiB blocks");
		let mut line = [0u8; 64];
		let n = common::describe(&mut line, b"virtio-mem", &mem.device, detail.as_bytes());
		if !common::online(bootstrap, &bind, &line[..n], &[(driver_protocol::provider::MEMORY_HOTPLUG, far)]) {
			exit();
		}
		let mut serving = common::Serving::from_offers(&[(0, mine)]);
		let mut buf = alloc::vec![0u8; 512];
		let mut reply = alloc::vec![0u8; 512];
		common::takes_sleep();
		loop {
			// A FRESH ROUND EACH TICK: a block in use is offered again once the tick has passed.
			mem.held.iter_mut().for_each(|held| *held = false);
			mem.moved = false;
			for _ in 0..mem.online.len().max(1) {
				if !mem.step() {
					break;
				}
			}
			let deadline = clock() + if mem.moved { 1 } else { TICK };
			match common::wait_providers_until(bootstrap, &bind, &mut serving, &[], deadline) {
				None => {
					if common::stop_requested() {
						common::finish_stop(bootstrap, &bind, mem.device.capability, common::quiesce_virtio());
					}
					exit();
				}
				Some(None) => {
					if !common::take_sleep_step(bootstrap, &bind, &mut Sleep { mem: &mut mem, serving: &mut serving, stopped: false }) {
						if common::stop_requested() {
							common::finish_stop(bootstrap, &bind, mem.device.capability, common::quiesce_virtio());
						}
						exit();
					}
				}
				Some(Some(common::ProviderReady::Connected(_))) | Some(Some(common::ProviderReady::Device(_))) => {}
				Some(Some(common::ProviderReady::Consumer(index))) => {
					if !serve(&mut mem, &serving, index, &mut buf, &mut reply) {
						// THE POLICY WENT: nothing more is plugged; what is plugged stays, being memory the machine uses.
						mem.allowed = mem.plugged();
						let token = serving.close_at(index);
						if !common::disconnected(bootstrap, &bind, token) {
							exit();
						}
					}
				}
			}
		}
	}
}
