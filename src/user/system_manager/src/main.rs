// SystemManager - the first userspace process.
//
// The kernel loads this program from the init package into a fresh Process and
// drops it into ring 3 at `_start` (provided by the shared `rt` runtime) with a
// bootstrap channel handle in rdi. For this milestone SystemManager simply
// reports in over that channel - the first real userspace-to-kernel IPC from a
// process loaded off the init package - and exits. Later milestones grow it into
// a standing service.

#![no_std]
#![no_main]

// The ring-3 entry stub, syscall wrapper, panic handler, and ABI constants all
// come from the shared userspace runtime crate.
use rt::*;

// Report in to the kernel over the bootstrap channel, then exit. `rt`'s `_start`
// enters here with the bootstrap channel handle in rdi.
#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let message = b"SystemManager: online";
	unsafe {
		syscall(SYS_CHANNEL_SEND, bootstrap, message.as_ptr() as u64, message.len() as u64, 0);
		syscall(SYS_USER_EXIT, 0, 0, 0, 0);
	}
	loop {}
}
