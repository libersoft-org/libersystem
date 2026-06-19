// driver.virtio-net - the userspace virtio network-device driver (the driver only;
// the network stack is a later phase).

#![no_std]
#![no_main]

mod common;
mod virtio;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	common::run(bootstrap, b"driver.virtio-net: online")
}
