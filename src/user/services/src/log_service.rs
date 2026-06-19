// LogService - the userspace structured-logging service (stub).
//
// ServiceManager starts this program from the init package and hands it a
// bootstrap channel. For this step LogService simply reports in over that channel
// and exits; M22 grows it into a real structured-log ingest/query service.

#![no_std]
#![no_main]

use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		send_blocking(bootstrap, b"LogService: online", 0);
	}
	exit();
}
