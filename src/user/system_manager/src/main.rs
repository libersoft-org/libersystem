// SystemManager - the first userspace process.
//
// The kernel loads this program from the init package into a fresh Process and
// drops it into ring 3 at `_start` (provided by the shared `rt` runtime) with a
// bootstrap channel handle in rdi. Over that channel the kernel hands it the init
// package as a read-only shared buffer. SystemManager maps the package, spawns
// ServiceManager from it (the next link of the boot chain), relays ServiceManager's
// report up to the kernel, reports in itself, and exits. Later milestones grow it
// into a standing process that supervises ServiceManager and performs recovery.

#![no_std]
#![no_main]

// The ring-3 entry stub, syscall wrapper, panic handler, spawn/IPC helpers, and
// ABI constants all come from the shared userspace runtime crate.
use rt::*;

// `rt`'s `_start` enters here with the bootstrap channel handle in rdi.
#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. receive the init package shared buffer and map it.
	let (pkg_base, pkg_len): (u64, usize) = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 + 8 && &buf[..7] == b"PACKAGE" => {
			let length: usize = u64::from_le_bytes([buf[7], buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14]]) as usize;
			let base: u64 = unsafe { syscall(SYS_MEMORY_MAP, handle, 0, 0, 0) };
			if sys_is_err(base) {
				exit();
			}
			(base, length)
		}
		_ => exit(),
	};

	// 2. find ServiceManager in the package and spawn it, handing it one end of a
	//    fresh report channel as its bootstrap.
	let archive: &[u8] = unsafe { core::slice::from_raw_parts(pkg_base as *const u8, pkg_len) };
	let svc_elf: &[u8] = match Package::parse(archive).and_then(|p| p.lookup(b"service_manager")) {
		Some(elf) => elf,
		None => exit(),
	};
	let (sm_side, svc_side): (u64, u64) = match unsafe { channel() } {
		Some(pair) => pair,
		None => exit(),
	};
	if unsafe { spawn(svc_elf, svc_side) } < 0 {
		exit();
	}

	// 3. relay ServiceManager's report to the kernel, then report in ourselves.
	if let Received::Message { len, .. } = unsafe { recv_blocking(sm_side, &mut buf) } {
		unsafe {
			send_blocking(bootstrap, &buf[..len], 0);
		}
	}
	unsafe {
		send_blocking(bootstrap, b"SystemManager: online", 0);
	}
	exit();
}
