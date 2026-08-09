use super::DmaBuffer;
use crate::mem::frame::PAGE_SIZE;
use crate::object::domain::Domain;

crate::tagged_test!(dma_buffer_uses_a_contiguous_span_and_refunds_its_charge, [Dma, Drivers, Memory], covers = ["kernel"]);
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

crate::tagged_test!(dma_buffer_quota_enforced_cleanly, [Dma, Drivers, Kernel, Syscall], covers = ["kernel"]);
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
