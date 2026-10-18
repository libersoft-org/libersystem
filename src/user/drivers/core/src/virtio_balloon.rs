// driver.virtio-balloon - memory a virtual machine gives back to its host (virtio device 5), within an allowance its
// policy grants, served as the `balloon-device` contract to ResourceManager.
//
// THE HOST ASKS, THE POLICY ALLOWS, THE KERNEL FLOORS. The host's target is the device's `num_pages`; ResourceManager -
// the one consumer - grants an allowance; the balloon holds the smaller of the two, and the kernel's balloon object
// takes only FREE frames and none below its floor, whatever both say. With no allowance granted it holds nothing: a
// balloon nobody governs gives the host nothing.
//
// BOUNDED BATCHES, AND THE TARGET READ AGAIN AFTER EACH: at most `drivers::balloon::BATCH` pages a request, so a host
// that changes its mind half way - or a policy that lowers the allowance under pressure - is followed within one batch.
// A deflate tells the host first and gives the frames back to the kernel after, as the device's protocol orders it.
//
// THE HOST IS TOLD WHAT THE MACHINE HAS: the statistics queue answers its requests with the free, the total and the
// available memory - pressure reported, never a Domain named.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use drivers::balloon::{self, Plan};
use drivers::common;
use drivers::virtio::{Queue, Virtio};
use proto::generated::liber::balloon_device::v1::{BalloonStatus, balloon_device};
use proto::system::Error;
use rt::*;

const INFLATE_QUEUE: u16 = 0;
const DEFLATE_QUEUE: u16 = 1;
const STATS_QUEUE: u16 = 2;

// The configuration: the host's target and what the driver reports it holds, both in 4 KiB pages.
const CONFIG_NUM_PAGES: u64 = 0;
const CONFIG_ACTUAL: u64 = 4;

const PAGE: u64 = 4096;
// How often the target, the allowance and the statistics queue are looked at when nothing else wakes the driver.
const TICK: u64 = 10;

#[derive(Clone, Copy)]
struct Buffer {
	virt: u64,
	phys: u64,
}

impl Buffer {
	fn write(&self, at: usize, bytes: &[u8]) {
		// SAFETY: this driver's own DMA page; every caller writes inside it.
		unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), (self.virt + at as u64) as *mut u8, bytes.len()) };
	}
}

struct Balloon {
	device: Virtio,
	inflate: Queue,
	deflate: Queue,
	stats: Option<Queue>,
	// The kernel's balloon: the frames this driver holds for the host.
	frames: u64,
	// Their physical addresses, in the order taken: a deflate gives the most recent back first.
	held: Vec<u64>,
	allowed: u64,
	pfns: Buffer,
	stats_page: Buffer,
	stats_posted: bool,
}

impl Balloon {
	fn target(&self) -> u64 {
		let bytes: [u8; 4] = core::array::from_fn(|at| self.device.config_read(CONFIG_NUM_PAGES + at as u64));
		u64::from(u32::from_le_bytes(bytes))
	}

	fn report_actual(&self) {
		let actual = u32::try_from(self.held.len()).unwrap_or(u32::MAX).to_le_bytes();
		for (at, byte) in actual.iter().enumerate() {
			self.device.config_write(CONFIG_ACTUAL + at as u64, *byte);
		}
	}

	fn status(&self) -> BalloonStatus {
		let target = self.target();
		let held = self.held.len() as u64;
		BalloonStatus { target, held, allowed: self.allowed, held_back: target.saturating_sub(held) }
	}

	// ONE BATCH TOWARD THE PLAN: frames taken and the host told, or the host told and frames given back. False when
	// there was nothing to do, or the kernel's floor stopped a take.
	fn step(&mut self) -> bool {
		match balloon::plan(self.target(), self.allowed, self.held.len() as u64) {
			Plan::Hold => false,
			Plan::Inflate(count) => self.inflate(count),
			Plan::Deflate(count) => self.deflate(count),
		}
	}

	fn inflate(&mut self, count: usize) -> bool {
		if self.held.try_reserve(count).is_err() {
			return false;
		}
		let mut frames = [0u64; balloon::BATCH];
		let taken = unsafe { syscall(SYS_BALLOON_TAKE, self.frames, frames.as_mut_ptr() as u64, count as u64, 0) } as i64;
		if taken <= 0 {
			return false;
		}
		let taken = taken as usize;
		let mut pfns = [0u8; balloon::BATCH * 4];
		let Some(len) = balloon::pfns(&frames[..taken], &mut pfns) else {
			let _ = unsafe { syscall(SYS_BALLOON_GIVE, self.frames, frames.as_ptr() as u64, taken as u64, 0) };
			return false;
		};
		self.pfns.write(0, &pfns[..len]);
		let chain: [(u64, u32, bool); 1] = [(self.pfns.phys, len as u32, false)];
		let mut used = [0u32; 1];
		if self.inflate.submit_chains(&[&chain], None, &mut used).is_err() {
			// THE HOST WAS NOT TOLD, so the frames are still the machine's: given straight back.
			let _ = unsafe { syscall(SYS_BALLOON_GIVE, self.frames, frames.as_ptr() as u64, taken as u64, 0) };
			return false;
		}
		self.held.extend_from_slice(&frames[..taken]);
		self.report_actual();
		true
	}

	fn deflate(&mut self, count: usize) -> bool {
		let from = self.held.len() - count;
		let mut pfns = [0u8; balloon::BATCH * 4];
		let Some(len) = balloon::pfns(&self.held[from..], &mut pfns) else { return false };
		self.pfns.write(0, &pfns[..len]);
		let chain: [(u64, u32, bool); 1] = [(self.pfns.phys, len as u32, false)];
		let mut used = [0u32; 1];
		// THE HOST IS TOLD FIRST, and only then are the frames the machine's again.
		if self.deflate.submit_chains(&[&chain], None, &mut used).is_err() {
			return false;
		}
		let given = unsafe { syscall(SYS_BALLOON_GIVE, self.frames, self.held[from..].as_ptr() as u64, count as u64, 0) } as i64;
		if given != count as i64 {
			print(b"driver.virtio-balloon: the kernel refused frames this driver held - nothing given\n");
			return false;
		}
		self.held.truncate(from);
		self.report_actual();
		true
	}

	// THE STATISTICS THE HOST ASKED FOR: when it hands the buffer back, the free, total and available memory written into
	// it and the buffer posted again.
	fn statistics(&mut self) {
		let Some(queue) = self.stats.as_mut() else { return };
		if self.stats_posted && queue.take_used().is_none() {
			return;
		}
		let mut stats = MemoryStats::default();
		let _ = memory_stats(&mut stats);
		let mut record = [0u8; balloon::STATS_LEN];
		let len = balloon::stats(&stats, &mut record);
		self.stats_page.write(0, &record[..len]);
		self.stats_posted = queue.submit_async(&[(self.stats_page.phys, len as u32, false)]);
	}
}

impl balloon_device::Service for Balloon {
	fn status(&mut self) -> Result<BalloonStatus, Error> {
		Ok(Balloon::status(self))
	}

	fn allow(&mut self, pages: u64) -> Result<BalloonStatus, Error> {
		self.allowed = pages;
		// THE FIRST BATCH AT ONCE, so a lowered allowance starts giving back before the answer.
		self.step();
		Ok(Balloon::status(self))
	}
}

fn serve(balloon: &mut Balloon, serving: &common::Serving, index: usize, buf: &mut [u8], reply: &mut [u8]) -> bool {
	let channel = serving.at(index);
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	let mut reply_handles = wire::Handles::new();
	if let Some(written) = balloon_device::dispatch(balloon, &buf[..len], &mut handles, reply, &mut reply_handles) {
		send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
	}
	true
}

struct Sleep<'a> {
	balloon: &'a mut Balloon,
	serving: &'a mut common::Serving,
	stopped: bool,
}

impl common::SleepStep for Sleep<'_> {
	fn suspend(&mut self, request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		match self.balloon.device.sleep(request) {
			Some(stopped) => {
				self.stopped = stopped;
				driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
			}
			None => driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Refused(driver_protocol::DriverFailureCode::DeviceNotResponding), awake_by_ms: 0 },
		}
	}

	// A DEVICE THAT LOST ITS POWER FORGOT WHAT IT WAS GIVEN, and the host holds those pages no longer: they are given back
	// to the kernel, and the balloon starts from nothing.
	fn resume(&mut self, lost_power: bool) -> bool {
		let balloon = &mut *self.balloon;
		let mut queues: Vec<&mut Queue> = Vec::new();
		queues.push(&mut balloon.inflate);
		queues.push(&mut balloon.deflate);
		if let Some(stats) = balloon.stats.as_mut() {
			queues.push(stats);
		}
		let woke = balloon.device.wake(&mut queues, lost_power, core::mem::take(&mut self.stopped));
		if woke && lost_power {
			for chunk in balloon.held.chunks(balloon::BATCH) {
				let _ = unsafe { syscall(SYS_BALLOON_GIVE, balloon.frames, chunk.as_ptr() as u64, chunk.len() as u64, 0) };
			}
			balloon.held.clear();
			balloon.stats_posted = false;
			balloon.report_actual();
		}
		woke
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(self.serving)
	}
}

fn page(device: &Virtio) -> Buffer {
	// SAFETY: the bound device's capability; a page the device may read.
	match unsafe { dma_buffer_for(device.capability, PAGE) } {
		Some((_, virt, phys)) => Buffer { virt, phys },
		None => exit(),
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let (bind, resources) = common::handshake(bootstrap);
		let device: Virtio = common::bringup_bound(bootstrap, &bind, &resources, balloon::FEATURE_STATS_VQ);
		let frames = syscall(SYS_BALLOON_CREATE, resources.device, 0, 0, 0);
		if sys_is_err(frames) {
			print(b"driver.virtio-balloon: the kernel gave this driver no balloon - it is not driven\n");
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
		}
		let Some(inflate) = device.setup_queue(INFLATE_QUEUE) else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable) };
		let Some(deflate) = device.setup_queue(DEFLATE_QUEUE) else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable) };
		let stats = if device.features_word0() & balloon::FEATURE_STATS_VQ != 0 { device.setup_queue(STATS_QUEUE) } else { None };
		let (pfns, stats_page) = (page(&device), page(&device));
		let mut balloon = Balloon { device, inflate, deflate, stats, frames, held: Vec::new(), allowed: 0, pfns, stats_page, stats_posted: false };
		balloon.device.driver_ok();
		balloon.report_actual();
		balloon.statistics();
		let Some((mine, far)) = channel() else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::OutOfMemory) };
		let mut line = [0u8; 64];
		let n = common::describe(&mut line, b"virtio-balloon", &balloon.device, if balloon.stats.is_some() { b"statistics" } else { b"" });
		if !common::online(bootstrap, &bind, &line[..n], &[(driver_protocol::provider::MEMORY_BALLOON, far)]) {
			exit();
		}
		let mut serving = common::Serving::from_offers(&[(0, mine)]);
		let mut buf = alloc::vec![0u8; 512];
		let mut reply = alloc::vec![0u8; 512];
		common::takes_sleep();
		loop {
			// AS LONG AS THERE IS WORK, A BATCH AT A TIME, the consumer served in between; otherwise a tick.
			let busy = balloon.step();
			balloon.statistics();
			let deadline = clock() + if busy { 1 } else { TICK };
			match common::wait_providers_until(bootstrap, &bind, &mut serving, &[], deadline) {
				None => {
					if common::stop_requested() {
						common::finish_stop(bootstrap, &bind, balloon.device.capability, common::quiesce_virtio());
					}
					exit();
				}
				Some(None) => {
					if !common::take_sleep_step(bootstrap, &bind, &mut Sleep { balloon: &mut balloon, serving: &mut serving, stopped: false }) {
						if common::stop_requested() {
							common::finish_stop(bootstrap, &bind, balloon.device.capability, common::quiesce_virtio());
						}
						exit();
					}
				}
				Some(Some(common::ProviderReady::Connected(_))) | Some(Some(common::ProviderReady::Device(_))) => {}
				Some(Some(common::ProviderReady::Consumer(index))) => {
					if !serve(&mut balloon, &serving, index, &mut buf, &mut reply) {
						// THE POLICY WENT: the allowance it granted goes with it, and the balloon gives everything back.
						balloon.allowed = 0;
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
