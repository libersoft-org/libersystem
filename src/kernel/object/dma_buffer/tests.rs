use super::DmaBuffer;
use crate::mem::frame::PAGE_SIZE;
use crate::object::domain::Domain;

crate::tagged_test!(dma_buffer_uses_a_contiguous_span_and_refunds_its_charge, [Dma, Drivers, Memory]);
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
