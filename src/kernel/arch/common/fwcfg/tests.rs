// KERN-ARCH-022: a firmware span this module allocated comes back whole.

crate::tagged_test!(a_framebuffer_span_is_released_whole_rather_than_one_page_of_it, [Kernel, Memory], id = "kernel.arch.common.fwcfg.a_framebuffer_span_is_released_whole_rather_than_one_page_of_it", covers = ["kernel"]);
fn a_framebuffer_span_is_released_whole_rather_than_one_page_of_it() {
	// The ramfb failure path called `deallocate` once on the base of a multi-page contiguous
	// framebuffer. That API releases exactly ONE frame, so every other page of a 1920x1080
	// framebuffer - 2024 of the 2025 - was gone for the life of the boot, on a path that only
	// runs when something has already gone wrong and nobody is watching the free count.
	//
	// Measured on the allocator rather than through fw-cfg: forcing the final DMA to fail needs a
	// device that is not there, and the property being fixed is how much memory comes back.
	use crate::mem::frame;
	const PAGES: usize = 8;
	let before = frame::free_count();
	let base = frame::allocate_contiguous(PAGES).expect("a contiguous span");
	assert_eq!(frame::free_count(), before - PAGES, "the span left the pool whole");
	// SAFETY: the span allocated two lines above, never mapped, freed once.
	unsafe { super::release_span(base, PAGES) };
	assert_eq!(frame::free_count(), before, "and came back whole");
}
