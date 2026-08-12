use super::DmaBuffer;
use crate::mem::frame::PAGE_SIZE;
use crate::object::domain::Domain;

crate::tagged_test!(dma_buffer_uses_a_contiguous_span_and_refunds_its_charge, [Dma, Drivers, Memory], id = "kernel.object.dma_buffer.dma_buffer_uses_a_contiguous_span_and_refunds_its_charge", covers = ["kernel"]);
fn dma_buffer_uses_a_contiguous_span_and_refunds_its_charge() {
	let domain = Domain::new(1 << 24, 8, 4);
	let dma = match DmaBuffer::create_in(&domain, 6 * PAGE_SIZE as usize) {
		Ok(dma) => dma,
		Err(_) => panic!("a 6-page DMA buffer should allocate"),
	};
	let frames = dma.frames();
	assert_eq!(frames.len(), 6);
	for pair in frames.windows(2) {
		assert_eq!(pair[1], pair[0] + PAGE_SIZE, "DMA frames are physically contiguous");
	}
	assert_eq!(dma.phys_base(), frames[0]);
	drop(dma);
	assert_eq!(domain.account().dma().used(), 0, "the DMA charge is refunded");
}

crate::tagged_test!(dma_buffer_quota_enforced_cleanly, [Dma, Drivers, Kernel, Syscall], id = "kernel.object.dma_buffer.dma_buffer_quota_enforced_cleanly", covers = ["kernel"]);
fn dma_buffer_quota_enforced_cleanly() {
	use crate::object::domain::UNLIMITED;
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// A thread accounted to a Domain capped at two pages of pinned DMA. The
	// dma_buffer_create syscall charges the DMA quota at the create boundary, so a
	// third buffer must be refused cleanly (ERR_RESOURCE_EXHAUSTED, nothing
	// allocated) and closing the buffers must refund the quota.
	extern "C" fn body(_arg: u64) {
		unsafe {
			let first = crate::arch::syscall::invoke(crate::syscall::SYS_DMA_BUFFER_CREATE, 4096, 0, 0, 0);
			assert!(!crate::syscall::sys_is_err(first));
			let second = crate::arch::syscall::invoke(crate::syscall::SYS_DMA_BUFFER_CREATE, 4096, 0, 0, 0);
			assert!(!crate::syscall::sys_is_err(second));
			let third = crate::arch::syscall::invoke(crate::syscall::SYS_DMA_BUFFER_CREATE, 4096, 0, 0, 0);
			assert_eq!(third as i64, crate::syscall::ERR_RESOURCE_EXHAUSTED);
			// Closing the buffers refunds both their DMA quota and their handles.
			assert_eq!(crate::arch::syscall::invoke(crate::syscall::SYS_HANDLE_CLOSE, first, 0, 0, 0) as i64, 0);
			assert_eq!(crate::arch::syscall::invoke(crate::syscall::SYS_HANDLE_CLOSE, second, 0, 0, 0) as i64, 0);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	let domain = Domain::new(UNLIMITED, UNLIMITED, UNLIMITED);
	domain.account().dma().set_limit(2 * 4096);
	assert!(crate::sched::spawn_in(domain.clone(), body, 0).is_some());
	crate::sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "DMA quota test thread did not finish");
	// Every buffer was closed, so the pinned-DMA quota is back to zero.
	assert_eq!(domain.account().dma().used(), 0);
}

crate::tagged_test!(a_dead_drivers_dma_frames_wait_for_its_device_to_be_reset, [Dma, Drivers, Memory, Kernel], id = "kernel.object.dma_buffer.a_dead_drivers_dma_frames_wait_for_its_device_to_be_reset", covers = ["kernel"]);
fn a_dead_drivers_dma_frames_wait_for_its_device_to_be_reset() {
	// A driver hands its device a REAL PHYSICAL ADDRESS and there is no IOMMU, so the moment the
	// frames stop being the device's business is the moment the DEVICE stops - not the moment the
	// driver's last handle closes. A driver that faults with a descriptor live never gets to say
	// anything, and the kernel used to recycle its DMA frames immediately: the next allocation got
	// memory a running device was still writing into.
	//
	// The two cases have to be told apart, and the difference is who closed the buffer. Both are
	// exercised here, because holding the frames of a driver that shut down cleanly would be a leak
	// on the ordinary path.
	use crate::object::address_space::AddressSpace;
	use crate::object::device_memory::DeviceMemory;
	use crate::object::process::Process;
	use crate::object::rights::Rights;
	const DEVICE: u32 = 7;
	let domain = crate::sched::root_domain();
	assert_eq!(super::held_frames_for_test(DEVICE), 0, "nothing is held for this device to begin with");

	// 1. A buffer whose owner CLOSED it: the frames go back at once, exactly as before.
	let Ok(deliberate) = DmaBuffer::create_for(&domain, 2 * PAGE_SIZE as usize, Some(DEVICE)) else {
		panic!("a 2-page DMA buffer should allocate");
	};
	drop(deliberate);
	assert_eq!(super::held_frames_for_test(DEVICE), 0, "a buffer its owner released is not held - that would leak on the ordinary path");

	// 2. A buffer whose owner was TERMINATED holding it. The process teardown marks it, so the drop
	//    that follows keeps the frames out of circulation.
	let process = Process::new(AddressSpace::create().expect("an address space"), domain.clone());
	let Ok(orphan) = DmaBuffer::create_for(&domain, 3 * PAGE_SIZE as usize, Some(DEVICE)) else {
		panic!("a 3-page DMA buffer should allocate");
	};
	let frames: alloc::vec::Vec<u64> = orphan.frames().to_vec();
	assert!(process.install(orphan, Rights::ALL, 0) != 0, "the buffer is installed in the driver process");
	process.terminate();
	crate::sched::run_until_idle();
	assert_eq!(super::held_frames_for_test(DEVICE), frames.len(), "the frames of a driver that died holding a buffer are held for its device, not handed to whoever allocates next");

	// 3. And they come back when - and only when - somebody proves the device has been stopped.
	//    That claim is a capability: the holder of the device's own DeviceMemory.
	let other = DeviceMemory::for_device(DEVICE + 1, 0x1000_0000, PAGE_SIZE as usize);
	assert_eq!(super::release_for(other.device_index().expect("a device-table entry")), 0, "resetting a different device releases nothing");
	assert_eq!(super::held_frames_for_test(DEVICE), frames.len(), "still held");

	let released = super::release_for(DEVICE);
	assert_eq!(released, frames.len(), "resetting the device releases exactly its held frames");
	assert_eq!(super::held_frames_for_test(DEVICE), 0, "and nothing is held for it any more");
	drop(process);
	crate::sched::run_until_idle();
}

crate::tagged_test!(a_full_hold_table_leaks_a_dead_drivers_frames_rather_than_recycling_them, [Dma, Drivers, Memory, Kernel], id = "kernel.object.dma_buffer.a_full_hold_table_leaks_a_dead_drivers_frames_rather_than_recycling_them", covers = ["kernel"]);
fn a_full_hold_table_leaks_a_dead_drivers_frames_rather_than_recycling_them() {
	// Past 64 held entries the overflow used to hand the frames back to its caller, which RETIRED
	// them - the kernel announcing, out loud, that it was returning memory a device may still be
	// writing into to whoever allocated next. The rule this table exists for has no exception, so
	// the overflow leaks instead: the pages leave circulation permanently and are counted.
	//
	// FRAME NUMBERS THAT WERE NEVER ALLOCATED. The table only records them, and the assertions are
	// about what it does with the record - so a real allocation of 65 buffers would be 65 slower
	// ways to test the same branch. It does mean the test may not `release_for`, which retires;
	// `forget_for_test` drops the records the way this test made them.
	const DEVICE: u32 = 0xD1;
	const FAKE_BASE: u64 = 0xDEAD_0000_0000;
	super::forget_for_test(DEVICE);
	assert_eq!(super::held_frames_for_test(DEVICE), 0, "nothing is held for this device to begin with");
	let leaked_before = super::leaked_frames_for_test();
	let lost_before = crate::mem::frame::lost_pages();

	// Fill it exactly. Every one of these is held, none is lost.
	for index in 0..super::MAX_HELD {
		super::hold_for_test(DEVICE, alloc::vec![FAKE_BASE + index as u64 * PAGE_SIZE]);
	}
	assert_eq!(super::held_frames_for_test(DEVICE), super::MAX_HELD, "a table with room holds every entry");
	assert_eq!(super::leaked_frames_for_test(), leaked_before, "and loses nothing while it has room");

	// One past it. The frames do not come back and they are not retired - they are counted lost.
	super::hold_for_test(DEVICE, alloc::vec![FAKE_BASE + 0x1_0000, FAKE_BASE + 0x2_0000, FAKE_BASE + 0x3_0000]);
	assert_eq!(super::held_frames_for_test(DEVICE), super::MAX_HELD, "the table did not grow");
	assert_eq!(super::leaked_frames_for_test(), leaked_before + 3, "the three frames it could not hold are counted as leaked");
	assert_eq!(crate::mem::frame::lost_pages(), lost_before + 3, "and counted in the machine-wide lost total, which is where a leak is diagnosed from");

	super::forget_for_test(DEVICE);
	assert_eq!(super::held_frames_for_test(DEVICE), 0, "the test leaves the table as it found it");
}
