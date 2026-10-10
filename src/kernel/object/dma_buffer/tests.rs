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
	assert_eq!(dma.device_address(), frames[0], "an untranslated buffer answers with the physical address it always did");
	assert!(!dma.is_translated(), "and says that is what it is");
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
	// The buffers below belong to a BINDING of that device, which is what the create path takes now:
	// the generation is what stops a mapping from landing in a later binding's domain. These test
	// devices are not translated, so the key is here to name the device rather than to be checked.
	const BINDING: abi::ClaimKey = abi::ClaimKey { device_index: DEVICE, _pad: 0, generation: 1 };
	let parent = Domain::new(u64::MAX, u64::MAX, u64::MAX);
	let domain = Domain::new_child(&parent, u64::MAX, u64::MAX, u64::MAX).expect("a child DMA account");
	domain.account().dma().set_limit(3 * PAGE_SIZE);
	parent.account().dma().set_limit(3 * PAGE_SIZE);
	assert_eq!(super::held_frames_for_test(DEVICE), 0, "nothing is held for this device to begin with");

	// 1. A buffer whose owner CLOSED it: the frames go back at once, exactly as before.
	let Ok(deliberate) = DmaBuffer::create_for(&domain, 2 * PAGE_SIZE as usize, Some(BINDING)) else {
		panic!("a 2-page DMA buffer should allocate");
	};
	drop(deliberate);
	assert_eq!(super::held_frames_for_test(DEVICE), 0, "a buffer its owner released is not held - that would leak on the ordinary path");
	assert_eq!(domain.account().dma().used(), 0, "clean close returns the child's charge");
	assert_eq!(parent.account().dma().used(), 0, "and its ancestor's charge");

	// 2. A buffer whose owner was TERMINATED holding it. The process teardown marks it, so the drop
	//    that follows keeps the frames out of circulation.
	let process = Process::new(AddressSpace::create().expect("an address space"), domain.clone()).expect("a test process");
	let Ok(orphan) = DmaBuffer::create_for(&domain, 3 * PAGE_SIZE as usize, Some(BINDING)) else {
		panic!("a 3-page DMA buffer should allocate");
	};
	let frames: alloc::vec::Vec<u64> = orphan.frames().to_vec();
	assert!(process.install(orphan, Rights::ALL).is_some(), "the buffer is installed in the driver process");
	process.terminate();
	crate::sched::run_until_idle();
	assert_eq!(super::held_frames_for_test(DEVICE), frames.len(), "the frames of a driver that died holding a buffer are held for its device, not handed to whoever allocates next");
	// AND THE DEVICE IS KNOWN TO BE ONE ITS LAST DRIVER LEFT RUNNING, which is what holds a re-claim's bus
	// mastering off until the next driver has reset it.
	assert!(super::holds_for(DEVICE), "frames held for the device say its last driver ended with its DMA unconfirmed");
	assert_eq!(domain.account().dma().used(), 3 * PAGE_SIZE, "killing the owner retains the held backing's charge");
	assert_eq!(parent.account().dma().used(), 3 * PAGE_SIZE, "the same retained charge reaches its ancestor");
	assert!(matches!(DmaBuffer::create_in(&domain, PAGE_SIZE as usize), Err(super::MemoryError::QuotaExceeded)), "retained frames cannot be hidden from the DMA limit by killing their process");
	assert!(!super::holds_for(DEVICE + 1), "and say nothing about any other device");

	// 3. And they come back when - and only when - somebody proves the device has been stopped.
	//    That claim is a capability: the holder of the device's own DeviceMemory.
	// THROUGH A CLAIM, because that is the only way a device-indexed window exists in production -
	// `DeviceMemory::for_claim` is what `SYS_DEVICE_CLAIM` hands out, and the index it is keyed on is
	// the claim's. The claimless constructor this used to call, and the separate index field behind
	// it, went with the quiesce syscall's last production reader of them (2026-09-02).
	let other_key = abi::ClaimKey { device_index: DEVICE + 1, _pad: 0, generation: 1 };
	let other = DeviceMemory::for_claim(other_key, 0x1000_0000, PAGE_SIZE as usize).expect("a test device memory");
	assert_eq!(super::release_for(other.claim().expect("it names a binding").device_index), 0, "resetting a different device releases nothing");
	assert_eq!(super::held_frames_for_test(DEVICE), frames.len(), "still held");
	assert_eq!(domain.account().dma().used(), 3 * PAGE_SIZE, "another device's reset refunds nothing here");
	assert_eq!(parent.account().dma().used(), 3 * PAGE_SIZE);

	let released = super::release_for(DEVICE);
	assert_eq!(released, frames.len(), "resetting the device releases exactly its held frames");
	assert_eq!(super::held_frames_for_test(DEVICE), 0, "and nothing is held for it any more");
	assert!(!super::holds_for(DEVICE), "a device that was reset holds nothing back for its next binding");
	assert_eq!(domain.account().dma().used(), 0, "a confirmed release refunds the child exactly once");
	assert_eq!(parent.account().dma().used(), 0, "and its ancestor exactly once");
	assert_eq!(super::release_for(DEVICE), 0, "repeated reset releases nothing twice");
	assert_eq!(domain.account().dma().used(), 0);
	assert_eq!(parent.account().dma().used(), 0);
	let Ok(replacement) = DmaBuffer::create_in(&domain, 3 * PAGE_SIZE as usize) else { panic!("a reset makes the quota available again") };
	drop(replacement);
	drop(process);
	crate::sched::run_until_idle();

	// A standalone account has no parent's child list keeping it alive. The held backing itself
	// must retain its owner after the creating buffer and all caller references disappear.
	let owner = Domain::new(u64::MAX, u64::MAX, u64::MAX);
	let weak = alloc::sync::Arc::downgrade(&owner);
	let Ok(pending) = DmaBuffer::create_for(&owner, PAGE_SIZE as usize, Some(BINDING)) else { panic!("one held page") };
	pending.mark_orphaned();
	drop(pending);
	drop(owner);
	assert_eq!(weak.upgrade().expect("held frames retain their account").account().dma().used(), PAGE_SIZE);
	assert_eq!(super::release_for(DEVICE), 1);
	assert!(weak.upgrade().is_none(), "confirmed release also relinquishes the held account owner");
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

	// One past it, through the actual buffer destructor. These three frame numbers are invented,
	// just like the filled table; the full-table branch must NEVER retire them. The charge is real
	// test accounting, so this catches a destructor that refunds after hold reports overflow.
	let parent = Domain::new(u64::MAX, u64::MAX, u64::MAX);
	let domain = Domain::new_child(&parent, u64::MAX, u64::MAX, u64::MAX).expect("an overflow account");
	domain.account().dma().set_limit(3 * PAGE_SIZE);
	assert!(domain.try_charge_dma(3 * PAGE_SIZE));
	let owner = alloc::sync::Arc::downgrade(&domain);
	let orphan = DmaBuffer { header: super::ObjectHeader::new(), frames: alloc::vec![FAKE_BASE + 0x1_0000, FAKE_BASE + 0x2_0000, FAKE_BASE + 0x3_0000], size: 3 * PAGE_SIZE as usize, mappings: super::SpinLock::new(alloc::vec::Vec::new()), domain: domain.clone(), device: Some(DEVICE), translation: super::SpinLock::new(None), orphaned: super::AtomicBool::new(true) };
	drop(orphan);
	assert_eq!(super::held_frames_for_test(DEVICE), super::MAX_HELD, "the table did not grow");
	assert_eq!(super::leaked_frames_for_test(), leaked_before + 3, "the three frames it could not hold are counted as leaked");
	assert_eq!(crate::mem::frame::lost_pages(), lost_before + 3, "and counted in the machine-wide lost total, which is where a leak is diagnosed from");

	super::forget_for_test(DEVICE);
	assert_eq!(super::held_frames_for_test(DEVICE), 0, "the test leaves the table as it found it");
	assert_eq!(super::release_for(DEVICE), 0, "a later reset cannot recover overflow pages whose records were lost");
	assert_eq!(domain.account().dma().used(), 3 * PAGE_SIZE, "permanently lost pages stay charged after all retained entries are gone");
	assert_eq!(parent.account().dma().used(), 3 * PAGE_SIZE, "permanent loss remains charged to the ancestor too");
	assert!(matches!(DmaBuffer::create_in(&domain, PAGE_SIZE as usize), Err(super::MemoryError::QuotaExceeded)), "overflow cannot erase its quota cost");
	drop(domain);
	drop(parent);
	assert_eq!(owner.upgrade().expect("the lost backing permanently retains its charged owner").account().dma().used(), 3 * PAGE_SIZE);
}
