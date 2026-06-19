// driver.virtio-console - the userspace virtio serial/console driver.

#![no_std]
#![no_main]

mod common;
mod virtio;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let _device = common::bringup(bootstrap);
		common::online_and_stand(bootstrap, b"driver.virtio-console: online")
	}
}
