// ProcessService - the userspace typed process-lifecycle service.
//
// ServiceManager starts this program from the init package and hands it a
// bootstrap channel, over which it receives a read-only view of the init package
// (so it can launch programs from it) and a "SERVE" channel its clients reach it
// on. Over that channel clients speak the generated `liber:system` Process
// bindings: they START a named program unattended, LAUNCH one with a caller-provided
// bootstrap channel (so a policy front end like PermissionManager can grant the new
// process its capabilities over that channel) and receive back the live process handle
// for job control, and LIST the processes started so far as typed `process-info` records
// (koid + name) that render as CLI / JSON on the client.
//
// The service holds no service clients and decides no grants - it is the loading
// mechanism only; the policy of what a launched program may reach lives in the front
// end that drives `launch`.
//
// When the supervisor that started it drops the bootstrap channel, the service
// exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::process::{self, Service};
use proto::system::{Error, ProcessInfo, StartResult};
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
		unsafe { close(handle as u64) };
		let info: ProcessInfo = ProcessInfo { koid, name };
		self.started.push(info.clone());
		Ok(info)
	}

	fn list(&mut self) -> Result<Vec<ProcessInfo>, Error> {
		Ok(self.started.clone())
	}

	fn launch(&mut self, name: String, bootstrap: u64) -> Result<StartResult, Error> {
		let elf: &[u8] = self.package.lookup(name.as_bytes()).ok_or(Error::NotFound)?;
		// spawn with the caller-provided bootstrap channel (the policy front end's end of
		// the new process's bootstrap), then read back the new process's koid. The live
		// process handle is handed back to the caller for job control - so unlike `start`
		// we do not close it here; it is transferred out as the reply's handle.
		let handle: i64 = unsafe { spawn(elf, bootstrap) };
		if handle < 0 {
			return Err(Error::Again);
		}
		let koid: u64 = unsafe { object_info(handle as u64) }.map(|i| i.koid).ok_or(Error::Again)?;
		let info: ProcessInfo = ProcessInfo { koid, name };
		self.started.push(info.clone());
		Ok(StartResult { task: handle as u64, info })
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. receive the init package shared buffer (to launch programs from) and map it.
	let (_pkg_handle, archive): (u64, &[u8]) = unsafe { recv_package(bootstrap, &mut buf) }.unwrap_or_else(|| exit());
	let package: Package = Package::parse(archive).unwrap_or_else(|| exit());

	// 2. wait for the serve channel clients reach us on.
	let service: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"SERVE") }.unwrap_or_else(|| exit());

	// 3. report in to the supervisor that started us.
	unsafe {
		send_blocking(bootstrap, b"ProcessService: online", 0);
	}

	// 4. serve generated start/list requests until the client side closes.
	let mut procs: Processes = Processes { package, started: Vec::new() };
	let mut request: [u8; 256] = [0u8; 256];
	let mut reply: [u8; 1024] = [0u8; 1024];
	unsafe {
		serve_multi(service, &mut request, &mut reply, |_chan: u64, req: &[u8], handle: u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> { process::dispatch(&mut procs, req, handle, out, reply_handle) });
	}
	exit();
}
