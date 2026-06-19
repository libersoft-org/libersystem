// driver.virtio-net - the userspace virtio network-device driver (the driver only;
// the network stack is a later phase).

#![no_std]
#![no_main]

mod common;
mod virtio;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let _device = common::bringup(bootstrap);
		common::online_and_stand(bootstrap, b"driver.virtio-net: online")
	}
}
