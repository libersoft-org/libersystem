// driver.virtio-rng - the userspace entropy-source driver.
//
// The smallest driver in this tree, and the one with the narrowest job: ask the device for bytes,
// hand them to the kernel, and never look at them. It publishes NO provider, because what it
// produces is not something a consumer opens a connection to - it is a seed for the machine's own
// pool, and the only thing that may hold it is the kernel.
//
// IT DOES NOT SAY WHAT ITS BYTES ARE WORTH. `SYS_ENTROPY_ADD` takes bytes and a device capability,
// and the KERNEL decides the credit, from the kind of source, at a rate below one bit per bit. A
// driver that could name its own credit could seed a machine to "fully seeded" with a constant and
// nothing downstream of `SYS_RANDOM_GET` could tell - so the number this driver gets back is a
// report, not a request.
//
// AND IT DOES NOT EXPOSE THE HOST'S BYTES. They go from the DMA buffer into the pool and are mixed
// beyond recovery; nothing in userspace ever reads them. A virtual machine's entropy device is a
// real seed source and not a proof of health - the host may be replaying a recording, the device may
// be a file, and a guest resumed from a snapshot gets a device about to repeat itself. None of that
// is visible from in here, which is exactly why the conclusion is not this driver's to draw.
//
// The queue is interrupt-driven. An entropy device answers when it feels like it, and a driver that
// polled would spin under a cooperative scheduler at the expense of every thread with boot work
// left.

#![no_std]
#![no_main]

use rt::*;

use crate::virtio::{Queue, Virtio};
use drivers::{common, virtio};

// How much is asked for at a time. Sized so ONE answered request crosses the pool's seed threshold
// at the quarter rate a paravirtual source is credited at: 64 bytes is 512 bits submitted and 128
// credited, so four full answers seed a machine. Larger requests buy nothing - the credit for a
// single submission is capped well below what a bigger buffer would offer - and a smaller one just
// takes more round trips.
const REQUEST_BYTES: u64 = 64;

// How long one request may take before the device is treated as not answering. A device that has
// nothing to give says so by saying nothing, and an entropy source is never on a critical path, so
// this is generous rather than tight: ten seconds at the 100 Hz scheduler tick.
const REQUEST_TICKS: u64 = 1000;

// How often the pool is topped up once it is seeded. Reseeding is not what makes the pool safe - the
// pool is safe once, from its threshold - but a long-lived machine that folded in nothing after its
// first minute would be running for months on the state of one moment.
const RESEED_TICKS: u64 = 6000;

// How many consecutive silent or empty requests it takes before the device is reported as not
// answering. Said ONCE rather than every round: a device that has stopped is not news sixty times a
// minute, and a log that repeats is a log nobody reads.
const SILENCE_LIMIT: u32 = 4;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// Bring the device up and take its MSI-X Interrupt, then route this device's interrupts to
		// table entry 0 before the queue is set up, so the queue is told the vector.
		let (bind, resources) = common::handshake(bootstrap);
		let mut device: Virtio = common::bringup_bound(bootstrap, &bind, &resources, 0);
		let irq: u64 = resources.irq;
		device.set_msix_vector(0);
		// One queue: the request queue, which is entirely device-writable. There is no header and no
		// command - the buffer IS the request.
		let mut requests: Queue = match device.setup_queue(0) {
			Some(queue) => queue,
			None => exit(),
		};
		requests.enable_interrupts();
		let (_buffer, virt, phys): (u64, u64, u64) = match dma_buffer_for(device.capability, REQUEST_BYTES) {
			Some(triple) => triple,
			None => exit(),
		};
		device.driver_ok();
		let mut line = [0u8; 64];
		let n = common::describe(&mut line, b"virtio-rng", &device, b"entropy");
		// NO PROVIDER, and that is not an omission. What this driver produces goes to the kernel's
		// pool through a syscall whose authority is this device's own capability; there is nothing
		// for a consumer to connect to, and publishing a channel that handed out raw host bytes
		// would be the exact thing this driver exists not to do.
		if !common::online(bootstrap, &bind, &line[..n], &[]) {
			exit();
		}
		serve(&device, &bind, irq, bootstrap, &mut requests, virt, phys)
	}
}

// Ask, submit, sleep, repeat - forever.
//
// Two cadences, and the difference is the whole of the startup story. Until the pool is seeded the
// driver asks again as soon as the last answer is in, because everything wanting key material on a
// machine with no hardware instruction is waiting on exactly this. Afterwards it asks on a slow
// timer, because the pool is already safe and a device answering as fast as it can would be a
// hot loop for no gain.
fn serve(device: &Virtio, bind: &common::Bind, irq: u64, bootstrap: u64, requests: &mut Queue, virt: u64, phys: u64) -> ! {
	let mut silent: u32 = 0;
	let mut reported: bool = false;
	let mut announced: bool = false;
	loop {
		let taken: usize = request_once(device, bind, irq, bootstrap, requests, phys);
		if taken == 0 {
			silent = silent.saturating_add(1);
			if silent == SILENCE_LIMIT && !reported {
				print(b"driver.virtio-rng: the device is not answering requests; the pool is not being topped up\n");
				reported = true;
			}
		} else {
			silent = 0;
			reported = false;
			// SAFETY: the DMA buffer is this driver's mapping, `taken` is bounded by the length the
			// device reported and by the buffer's own size, and the device has given the buffer
			// back - which is what a used-ring entry means.
			let bytes: &[u8] = unsafe { core::slice::from_raw_parts(virt as *const u8, taken) };
			// The answer is how many BITS the kernel credited, and it is a report rather than a
			// request. A refusal - a capability that is not this device's, or a claim that is over -
			// is negative, and it is not something a retry fixes.
			if entropy_add(device.capability, bytes) < 0 {
				print(b"driver.virtio-rng: the kernel refused this device's submission; nothing further will be accepted\n");
				exit();
			}
		}
		let mut health: EntropyHealth = EntropyHealth::default();
		let seeded: bool = entropy_health(&mut health) >= 0 && health.seeded != 0;
		if seeded && !announced {
			print(b"driver.virtio-rng: the entropy pool is seeded\n");
			announced = true;
		}
		if !seeded {
			// Straight back round: nothing else on this machine can produce key material until the
			// threshold is crossed, so there is nothing to wait for.
			if !common::answer_ping(bootstrap, bind) {
				stop(bootstrap, bind, device.capability);
			}
			continue;
		}
		// Seeded: wait out the reseed interval, answering the manager while waiting. PERIODIC,
		// because a plain timed wait counts as pending progress and would keep the scheduler from
		// ever calling the system idle - an entropy top-up is housekeeping and nothing is waiting
		// for it.
		let due: u64 = clock().saturating_add(RESEED_TICKS);
		while clock() < due {
			if !common::answer_ping(bootstrap, bind) {
				stop(bootstrap, bind, device.capability);
			}
			let ready: i64 = wait_any_periodic(&[bootstrap], due);
			if ready < 0 && ready != ERR_TIMED_OUT {
				stop(bootstrap, bind, device.capability);
			}
		}
	}
}

// One request, answered or not. Returns how many bytes the device wrote, which is zero for a device
// that did not answer within the deadline and zero for one that answered with nothing.
//
// A SHORT ANSWER IS AN ANSWER. The device may write fewer bytes than the buffer holds, and what came
// back is what is submitted - padding it out, or discarding it for being short, would both be the
// driver deciding something about entropy that is not the driver's to decide.
fn request_once(device: &Virtio, bind: &common::Bind, irq: u64, bootstrap: u64, requests: &mut Queue, phys: u64) -> usize {
	requests.post_recv(0, phys, REQUEST_BYTES as u32);
	requests.notify();
	let limit: u64 = clock().saturating_add(REQUEST_TICKS);
	loop {
		if !common::answer_ping(bootstrap, bind) {
			stop(bootstrap, bind, device.capability);
		}
		if let Some((id, len)) = requests.take_used() {
			if id != 0 {
				return 0;
			}
			return (len as u64).min(REQUEST_BYTES) as usize;
		}
		if clock() >= limit {
			// THE BUFFER IS STILL THE DEVICE'S. It was posted and not returned, so the descriptor
			// must not be reused - reposting it would leave the device holding a descriptor that
			// the next request overwrites, which corrupts the ring for the life of the guest. The
			// caller comes back round and waits again rather than asking a second time.
			return 0;
		}
		if wait_any(&[irq, bootstrap], limit) == 0 {
			// Read the ISR to deassert a level-triggered INTx line before acking (a harmless zero
			// read on MSI-X, which is edge-triggered).
			let _ = device.read_isr();
			interrupt_ack(irq);
		}
	}
}

// The manager asked this driver to stop, or dropped its channel. A stop that was ASKED FOR is
// certified after the device is quiet, which is what lets the kernel reclaim the frames and the
// masked vector this binding held; a channel that simply closed has nothing to certify.
fn stop(bootstrap: u64, bind: &common::Bind, capability: u64) -> ! {
	if common::stop_requested() {
		common::finish_stop(bootstrap, bind, capability, common::quiesce_virtio());
	}
	exit()
}
