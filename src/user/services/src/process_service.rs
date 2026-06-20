// ProcessService - the userspace typed process-lifecycle service.
//
// ServiceManager starts this program from the init package and hands it a
// bootstrap channel, over which it receives a read-only view of the init package
// (so it can launch programs from it) and a "SERVE" channel its clients reach it
// on. Over that channel clients speak the generated `liber:system` Process
// bindings: they START a named program (the kernel create/load/thread syscalls,
// wrapped by rt::spawn) and LIST the processes started so far, receiving typed
// `process-info` records (koid + name) that render as CLI / JSON on the client.
//
// Phase 1: started programs get no bootstrap capability, so they run unattended;
// the deliverable is the typed create/info lifecycle, not a full job launcher.
//
// When the supervisor that started it drops the bootstrap channel, the service
// exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::process::{self, Service};
use proto::system::{Error, ProcessInfo};
use rt::*;

// The init package to launch from, plus the processes started so far (in order).
struct Processes<'a> {
	package: Package<'a>,
	started: Vec<ProcessInfo>,
}

impl<'a> Service for Processes<'a> {
	fn start(&mut self, name: String) -> Result<ProcessInfo, Error> {
		let elf: &[u8] = self.package.lookup(name.as_bytes()).ok_or(Error::NotFound)?;
		// spawn with no bootstrap capability (phase 1: started processes run
		// unattended), then read back the new process's koid and record it.
		let handle: i64 = unsafe { spawn(elf, 0) };
		if handle < 0 {
			return Err(Error::Again);
		}
		let koid: u64 = unsafe { object_info(handle as u64) }.map(|i| i.koid).ok_or(Error::Again)?;
		unsafe { syscall(SYS_HANDLE_CLOSE, handle as u64, 0, 0, 0) };
		let info: ProcessInfo = ProcessInfo { koid, name };
		self.started.push(info.clone());
		Ok(info)
	}

	fn list(&mut self) -> Result<Vec<ProcessInfo>, Error> {
		Ok(self.started.clone())
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. receive the init package shared buffer (to launch programs from) and map it.
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
	let archive: &[u8] = unsafe { core::slice::from_raw_parts(pkg_base as *const u8, pkg_len) };
	let package: Package = match Package::parse(archive) {
		Some(p) => p,
		None => exit(),
	};

	// 2. wait for the serve channel clients reach us on.
	let service: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 5 && &buf[..5] == b"SERVE" => handle,
		_ => exit(),
	};

	// 3. report in to the supervisor that started us.
	unsafe {
		send_blocking(bootstrap, b"ProcessService: online", 0);
	}

	// 4. serve generated start/list requests until the client side closes. A
	//    zero-length message is the explicit quit sentinel.
	let mut procs: Processes = Processes { package, started: Vec::new() };
	let mut request: [u8; 256] = [0u8; 256];
	let mut reply: [u8; 1024] = [0u8; 1024];
	loop {
		match unsafe { recv_blocking(service, &mut request) } {
			Received::Message { len, .. } if len == 0 => break,
			Received::Message { len, handle } => {
				let mut reply_handle: u64 = 0;
				if let Some(n) = process::dispatch(&mut procs, &request[..len], handle, &mut reply, &mut reply_handle) {
					unsafe { send_blocking(service, &reply[..n], reply_handle) };
				}
			}
			Received::Closed => break,
		}
	}
	exit();
}
