// ProcessService - the userspace typed process-lifecycle service.
//
// ServiceManager starts this program from the init package and hands it a
// bootstrap channel, over which it receives a StorageService client (the system
// volume, from which it loads the on-disk program binaries under `system/bin`), a
// read-only view of the init package (the bring-up fallback when no storage client is
// wired) and a "SERVE" channel its clients reach it on. Over that channel clients speak
// the generated `liber:system` Process bindings: they START a named program unattended,
// LAUNCH one with a caller-provided bootstrap channel (so a policy front end like
// PermissionManager can grant the new process its capabilities over that channel) and
// receive back the live process handle for job control, and LIST the processes started
// so far as typed `process-info` records (koid + name) that render as CLI / JSON on the
// client.
//
// The storage client is the loading mechanism only - reading its own binaries off the
// system volume; the service holds no grantable service clients and decides no grants,
// so the policy of what a launched program may reach lives in the front end that drives
// `launch`.
//
// When the supervisor that started it drops the bootstrap channel, the service
// exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::process::{self, Service};
use proto::system::volume;
use proto::system::{Error, OpenOpts, ProcessInfo, StartResult};
use rt::*;

// Where the on-disk program binaries live on the system volume (staged there by the
// factory-seed pipeline). A named program is loaded from `<PROGRAM_DIR><name>`.
const PROGRAM_DIR: &str = "vol://system/bin/";

// The processes started so far (in order), the StorageService client the on-disk
// binaries are loaded through, and the init package they fall back to.
//
// The storage client is the loading mechanism - it is not a grantable capability and
// nothing about a launched program's authority passes through it; the policy of what a
// program may reach lives in the front end that drives `launch`. When no storage client
// is wired (early or isolated bring-up), programs are loaded from the built-in package
// instead.
struct Processes<'a> {
	package: Package<'a>,
	storage: u64,
	started: Vec<ProcessInfo>,
}

impl<'a> Processes<'a> {
	// Load program `name` and create a process from it, handing the child `bootstrap` as
	// its bootstrap capability. With a storage client wired, the binary is read from the
	// system volume's `bin/`; with none, it comes from the built-in package. Returns the
	// new process handle, or a negative value if the binary cannot be read or spawned.
	unsafe fn spawn_program(&self, name: &str, bootstrap: u64) -> i64 {
		unsafe {
			if self.storage != 0 {
				return spawn_from_storage(self.storage, name, bootstrap);
			}
			match self.package.lookup(name.as_bytes()) {
				Some(elf) => spawn(elf, bootstrap),
				None => -1,
			}
		}
	}
}

// Read `vol://system/bin/<name>` through the storage client, map its shared buffer,
// create a process from the mapped ELF image, then release the mapping. Returns the new
// process handle, or a negative value if the binary cannot be read or spawned.
unsafe fn spawn_from_storage(storage: u64, name: &str, bootstrap: u64) -> i64 {
	unsafe {
		let opts: OpenOpts = OpenOpts { path: alloc::format!("{PROGRAM_DIR}{name}"), write: false, create: false };
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let result = match client.open(&opts) {
			Some(Ok(r)) => r,
			_ => return -1,
		};
		if result.file == 0 || result.size == 0 {
			if result.file != 0 {
				close(result.file);
			}
			return -1;
		}
		let mapped: u64 = match map_object(result.file) {
			Some(base) => base,
			None => {
				close(result.file);
				return -1;
			}
		};
		let elf: &[u8] = core::slice::from_raw_parts(mapped as *const u8, result.size as usize);
		let handle: i64 = spawn(elf, bootstrap);
		unmap_object(result.file);
		close(result.file);
		handle
	}
}

impl<'a> Service for Processes<'a> {
	fn start(&mut self, name: String) -> Result<ProcessInfo, Error> {
		// spawn with no bootstrap capability (phase 1: started processes run
		// unattended), then read back the new process's koid and record it.
		let handle: i64 = unsafe { self.spawn_program(&name, 0) };
		if handle < 0 {
			return Err(Error::NotFound);
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
		// spawn with the caller-provided bootstrap channel (the policy front end's end of
		// the new process's bootstrap), then read back the new process's koid. The live
		// process handle is handed back to the caller for job control - so unlike `start`
		// we do not close it here; it is transferred out as the reply's handle.
		let handle: i64 = unsafe { self.spawn_program(&name, bootstrap) };
		if handle < 0 {
			return Err(Error::NotFound);
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

	// 1. receive the init package shared buffer (the bring-up fallback source) and map it.
	let (_pkg_handle, archive): (u64, &[u8]) = unsafe { recv_package(bootstrap, &mut buf) }.unwrap_or_else(|| unsafe { fail_bootstrap(bootstrap, b"package", b"init package not delivered") });
	let package: Package = Package::parse(archive).unwrap_or_else(|| unsafe { fail_bootstrap(bootstrap, b"package", b"init package malformed") });

	// 2. receive the StorageService client the on-disk binaries are loaded through. A 0
	//    handle (no client wired, e.g. an isolated bring-up) leaves us loading from the
	//    package instead.
	let storage: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"STORAGE") }.unwrap_or(0);

	// 3. wait for the serve channel clients reach us on.
	let service: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"SERVE") }.unwrap_or_else(|| unsafe { fail_bootstrap(bootstrap, b"serve", b"missing serve channel") });

	// 4. report in to the supervisor that started us.
	unsafe {
		send_blocking(bootstrap, b"ProcessService: online", 0);
	}

	// 5. serve generated start/list requests until the client side closes.
	let mut procs: Processes = Processes { package, storage, started: Vec::new() };
	let mut request: [u8; 256] = [0u8; 256];
	let mut reply: [u8; 4096] = [0u8; 4096];
	unsafe {
		serve_multi(service, &mut request, &mut reply, |_chan: u64, req: &[u8], handle: &mut u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> { process::dispatch(&mut procs, req, handle, out, reply_handle) });
	}
	exit();
}
