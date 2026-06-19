// driver.virtio-blk - the userspace virtio block-device driver.

#![no_std]
#![no_main]

mod common;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	common::run(bootstrap, b"driver.virtio-blk: online")
}
