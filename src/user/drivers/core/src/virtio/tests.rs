// The ring's device half, driven by a fake controller: a buffer this test owns, laid out the way
// `setup_queue` lays a ring out, with the device's fields written by hand.
use super::{Queue, UsedFault, check_used_advance, check_used_element, ring_layout};
use rt::{VIRTIO_DESC_F_NEXT as DESC_NEXT, VIRTIO_DESC_F_WRITE as DESC_WRITE};

struct Ring {
	// u64-backed for alignment: the ring's fields are read and written as u16/u32/u64 in place.
	memory: Vec<u64>,
	size: u16,
	avail_off: u64,
	used_off: u64,
}

impl Ring {
	fn new(size: u16) -> Ring {
		let (avail_off, used_off, bytes) = ring_layout(size);
		// Two bytes past the ring for the notify slot the queue writes on every submission.
		Ring { memory: vec![0u64; (bytes as usize + 8).div_ceil(8) + 1], size, avail_off, used_off }
	}
	fn base(&self) -> u64 {
		self.memory.as_ptr() as u64
	}
	fn notify(&self) -> u64 {
		self.base() + ring_layout(self.size).2 + 2
	}
	fn queue(&self) -> Queue {
		Queue::over(self.base(), self.size, self.notify())
	}
	fn set_used_index(&self, index: u16) {
		unsafe { ((self.base() + self.used_off + 2) as *mut u16).write_volatile(index) }
	}
	fn set_used_element(&self, slot: u16, id: u32, len: u32) {
		let element = self.base() + self.used_off + 4 + (slot % self.size) as u64 * 8;
		unsafe {
			(element as *mut u32).write_volatile(id);
			((element + 4) as *mut u32).write_volatile(len);
		}
	}
	fn descriptor(&self, id: u16) -> (u64, u32, u16, u16) {
		let d = self.base() + id as u64 * 16;
		unsafe { ((d as *const u64).read_volatile(), ((d + 8) as *const u32).read_volatile(), ((d + 12) as *const u16).read_volatile(), ((d + 14) as *const u16).read_volatile()) }
	}
	fn available_index(&self) -> u16 {
		unsafe { ((self.base() + self.avail_off + 2) as *const u16).read_volatile() }
	}
	// THE FAKE DEVICE, on a thread: the synchronous path samples the used index BEFORE it publishes
	// and then spins until the index moves, so the completion has to land while it spins. The
	// element is written first and the index bumped after, the way a device does it.
	fn complete_later(&self, slot: u16, id: u32, len: u32) -> std::thread::JoinHandle<()> {
		let base = self.base();
		let used_off = self.used_off;
		let size = self.size;
		std::thread::spawn(move || {
			std::thread::sleep(std::time::Duration::from_millis(2));
			let element = base + used_off + 4 + (slot % size) as u64 * 8;
			unsafe {
				(element as *mut u32).write_volatile(id);
				((element + 4) as *mut u32).write_volatile(len);
				core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
				((base + used_off + 2) as *mut u16).write_volatile(slot.wrapping_add(1));
			}
		})
	}
}

#[test]
fn the_layout_is_the_one_setup_queue_used_to_compute_by_hand() {
	// 16 bytes per descriptor, the available ring at 2 + 2 + 2n + 2 bytes, the used ring 4-aligned.
	assert_eq!(ring_layout(4), (64, 80, 118));
	assert_eq!(ring_layout(8), (128, 152, 222));
	assert_eq!(ring_layout(256), (4096, 4616, 6670));
}

#[test]
fn the_pure_checks_refuse_a_foreign_id_an_oversized_length_and_an_index_past_what_was_posted() {
	assert_eq!(check_used_element(0, 512, 4, Some(0), 512), Ok((0, 512)));
	assert_eq!(check_used_element(1, 0, 4, Some(0), 512), Err(UsedFault::Id), "the synchronous path posted head 0");
	assert_eq!(check_used_element(4, 0, 4, None, 512), Err(UsedFault::Id), "an id the ring does not have");
	assert_eq!(check_used_element(3, 513, 4, None, 512), Err(UsedFault::Length));
	assert_eq!(check_used_element(3, 512, 4, None, 512), Ok((3, 512)));
	assert_eq!(check_used_advance(0, 0, 2), Ok(0));
	assert_eq!(check_used_advance(0, 2, 2), Ok(2));
	assert_eq!(check_used_advance(0, 3, 2), Err(UsedFault::Index), "three completions for two buffers");
	assert_eq!(check_used_advance(65535, 1, 2), Ok(2), "the index is free-running and wraps like the ring");
	assert_eq!(check_used_advance(5, 4, 8), Err(UsedFault::Index), "backwards is 65535 completions, not minus one");
}

// The synchronous path: a request with one device-writable buffer, completed by the fake device
// before the queue looks (the poll sees the moved index at once).
#[test]
fn a_synchronous_completion_is_accepted_only_for_the_posted_head_within_its_writable_bytes() {
	let ring = Ring::new(4);
	let queue = ring.queue();
	let chain = [(0x1000u64, 16u32, false), (0x2000u64, 512u32, true), (0x3000u64, 1u32, true)];
	// A well-behaved device: one completion, of descriptor 0, 200 of the 513 writable bytes. Each
	// submission below is one more slot of the used ring, completed by the fake device's thread.
	let device = ring.complete_later(0, 0, 200);
	assert_eq!(unsafe { queue.submit_checked(&chain) }, Ok(200));
	device.join().unwrap();
	assert_eq!(ring.available_index(), 1, "the head was published once");
	let (phys, len, flags, next) = ring.descriptor(1);
	assert_eq!((phys, len, flags & DESC_WRITE != 0, flags & DESC_NEXT != 0, next), (0x2000, 512, true, true, 2));
	// Every device-writable byte, exactly.
	let device = ring.complete_later(1, 0, 513);
	assert_eq!(unsafe { queue.submit_checked(&chain) }, Ok(513));
	device.join().unwrap();
	// One byte more than the chain could take: the device is lying about what it wrote.
	let device = ring.complete_later(2, 0, 514);
	assert_eq!(unsafe { queue.submit_checked(&chain) }, Err(UsedFault::Length));
	device.join().unwrap();
	// The completion names a descriptor that is not the head this path posted.
	let device = ring.complete_later(3, 1, 16);
	assert_eq!(unsafe { queue.submit_checked(&chain) }, Err(UsedFault::Id));
	device.join().unwrap();
	// Two completions for the one chain posted: the device bumps the index twice.
	ring.set_used_element(4, 0, 16);
	let device = ring.complete_later(5, 0, 16);
	assert_eq!(unsafe { queue.submit_checked(&chain) }, Err(UsedFault::Index));
	device.join().unwrap();
	// A chain the ring cannot hold is refused before anything is written.
	assert_eq!(unsafe { queue.submit_checked(&[]) }, Err(UsedFault::Chain));
	let five = [(0u64, 1u32, false); 5];
	assert_eq!(unsafe { queue.submit_checked(&five) }, Err(UsedFault::Chain));
	// And the wrapper the drivers call keeps its shape: a refusal is `None`.
	let device = ring.complete_later(6, 3, 16);
	assert_eq!(unsafe { queue.submit(&chain) }, None);
	device.join().unwrap();
}

// The asynchronous path: a pool of device-writable buffers, reaped one completion at a time.
#[test]
fn the_receive_pool_refuses_what_it_did_not_post_and_what_would_not_fit() {
	let ring = Ring::new(4);
	let mut queue = ring.queue();
	unsafe {
		queue.post_recv(1, 0x1000, 1500);
		queue.post_recv(2, 0x2000, 1500);
	}
	assert_eq!(unsafe { queue.take_used() }, None, "nothing completed yet");
	assert_eq!(queue.fault(), None);
	// The device completes buffer 2 with a frame that fits.
	ring.set_used_element(0, 2, 60);
	ring.set_used_index(1);
	assert_eq!(unsafe { queue.take_used() }, Some((2, 60)));
	assert_eq!(unsafe { queue.take_used() }, None);
	// More completions than buffers outstanding: one buffer is left, the index claims three.
	ring.set_used_index(4);
	assert_eq!(unsafe { queue.take_used() }, None);
	assert_eq!(queue.fault(), Some(UsedFault::Index));
	// Back to a plausible index: an id the ring does not have.
	let mut queue = ring.queue();
	unsafe { queue.post_recv(3, 0x3000, 1500) };
	ring.set_used_element(0, 7, 60);
	ring.set_used_index(1);
	assert_eq!(unsafe { queue.take_used() }, None);
	assert_eq!(queue.fault(), Some(UsedFault::Id));
	// A length beyond the buffer that was posted for that id.
	let mut queue = ring.queue();
	unsafe { queue.post_recv(3, 0x3000, 1500) };
	ring.set_used_element(0, 3, 1501);
	ring.set_used_index(1);
	assert_eq!(unsafe { queue.take_used() }, None);
	assert_eq!(queue.fault(), Some(UsedFault::Length));
	// Exactly the buffer: accepted, and the pool count comes down with it.
	let mut queue = ring.queue();
	unsafe { queue.post_recv(3, 0x3000, 1500) };
	ring.set_used_element(0, 3, 1500);
	ring.set_used_index(1);
	assert_eq!(unsafe { queue.take_used() }, Some((3, 1500)));
	assert_eq!(unsafe { queue.take_used() }, None, "and nothing is outstanding to reap");
	assert_eq!(queue.fault(), None);
}

// The interrupt-driven single request: `submit_async` counts what it posted, so its completion is
// within the bound `take_used` checks, and a chain's writable bytes are summed across its links.
#[test]
fn an_asynchronous_submission_is_reaped_within_the_bytes_its_chain_offered() {
	let ring = Ring::new(4);
	let mut queue = ring.queue();
	let chain = [(0x1000u64, 16u32, false), (0x2000u64, 4096u32, true), (0x3000u64, 64u32, true)];
	assert!(unsafe { queue.submit_async(&chain) });
	ring.set_used_element(0, 0, 4160);
	ring.set_used_index(1);
	assert_eq!(unsafe { queue.take_used() }, Some((0, 4160)), "the sum of the two writable links");
	let mut queue = ring.queue();
	assert!(unsafe { queue.submit_async(&chain) });
	ring.set_used_element(0, 0, 4161);
	ring.set_used_index(1);
	assert_eq!(unsafe { queue.take_used() }, None);
	assert_eq!(queue.fault(), Some(UsedFault::Length));
	// A chain the ring cannot hold is refused before the count moves.
	let mut queue = ring.queue();
	assert!(!unsafe { queue.submit_async(&[]) });
	ring.set_used_index(1);
	assert_eq!(unsafe { queue.take_used() }, None);
	assert_eq!(queue.fault(), Some(UsedFault::Index), "nothing was posted, so a completion is a completion of nothing");
}
