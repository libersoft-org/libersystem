// The in-tree dependency's site: reached through the probe's closure, never a root itself.
#![no_std]

pub unsafe fn dependency_boundary(pointer: *const u8) -> u8 {
	unsafe { *pointer }
}
