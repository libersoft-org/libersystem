// ServiceManager - the userspace service supervisor.
//
// SystemManager spawns this program from the init package and hands it a
// bootstrap channel. For this first step ServiceManager simply reports in over
// that channel and exits; later steps grow it into a standing supervisor that
// starts the core services in dependency order and tracks their state.

#![no_std]
#![no_main]

use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let message = b"ServiceManager: online";
	unsafe {
		send_blocking(bootstrap, message, 0);
	}
	exit();
}
