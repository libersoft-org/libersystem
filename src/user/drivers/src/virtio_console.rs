// driver.virtio-console - the userspace virtio serial/console driver.

#![no_std]
#![no_main]

mod common;
mod virtio;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let device = common::bringup(bootstrap);
		// set up the first queue and go live; transmitting console output over the
		// queue is the next step.
		let _q = device.setup_queue(0);
		device.driver_ok();
		common::online_and_stand(bootstrap, b"driver.virtio-console: online")
	}
}
