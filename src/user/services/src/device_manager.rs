// DeviceManager - the userspace device supervisor (stub).
//
// ServiceManager starts this program from the init package and hands it a
// bootstrap channel. For this step DeviceManager simply reports in over that
// channel and exits; M23 grows it into device detection, driver mapping, and
// driver-crash handling.

#![no_std]
#![no_main]

use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		send_blocking(bootstrap, b"DeviceManager: online", 0);
	}
	exit();
}
