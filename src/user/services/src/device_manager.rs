// DeviceManager - the userspace device supervisor (stub).
//
// ServiceManager starts this program from the init package and hands it a
// bootstrap channel. For this step DeviceManager reports in over that channel and
// then stands, waiting for a control message: ServiceManager uses this to exercise
// the supervisor's stop path, sending "STOP", to which DeviceManager replies
// "DeviceManager: stopped" and exits. M23 grows it into device detection, driver
// mapping, and driver-crash handling.

#![no_std]
#![no_main]

use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 32] = [0u8; 32];
	unsafe {
		send_blocking(bootstrap, b"DeviceManager: online", 0);
		// Stand until ServiceManager asks us to stop (or drops our control channel),
		// then acknowledge the shutdown and exit.
		if let Received::Message { .. } = recv_blocking(bootstrap, &mut buf) {
			send_blocking(bootstrap, b"DeviceManager: stopped", 0);
		}
	}
	exit();
}
