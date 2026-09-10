// One site of every class the inventory must be able to see, so that a class going missing is
// mechanically detectable: a cfg-only site, a macro-produced site, a generated site, an in-tree
// dependency site (in `inventory-fixture-dep`) and a test-only site.
#![no_std]

// A GENERATED SITE: the build script wrote `generated_boundary` into OUT_DIR.
include!(concat!(env!("OUT_DIR"), "/generated_boundary.rs"));

// A CFG-ONLY SITE: present in the source, compiled only on one architecture.
#[cfg(target_arch = "riscv64")]
pub unsafe fn only_on_riscv(pointer: *mut u64) {
	unsafe { *pointer = 0 };
}

// A MACRO-PRODUCED SITE: the template carries the `unsafe`, every expansion emits one.
macro_rules! read_volatile_at {
	($address:expr) => {
		unsafe { core::ptr::read_volatile($address as *const u32) }
	};
}

pub fn expands_the_template() -> u32 {
	read_volatile_at!(0x1000usize) + read_volatile_at!(0x2000usize)
}

// A TEST-ONLY SITE: not in any shipping row.
#[cfg(test)]
pub unsafe fn only_in_tests() {}

// An ordinary source site, for the baseline.
pub static mut COUNTER: u32 = 0;

pub fn uses_the_dependency(pointer: *const u8) -> u8 {
	unsafe { inventory_fixture_dep::dependency_boundary(pointer) }
}
