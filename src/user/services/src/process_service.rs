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
use services::executable;

// Where the on-disk program binaries live on the system volume (staged there by the
// factory-seed pipeline). A named program is loaded from `<PROGRAM_DIR><name>`.
const PROGRAM_DIR: &str = "vol://system/bin/";
const LIBRARY_DIR: &str = "vol://system/lib/";
const LIBRARY_BASE: u64 = 0x2000_0000;
const LIBRARY_SLOT_SIZE: u64 = 0x0100_0000;
// Per-process dependency-graph limits. MAX_MODULES counts unique loaded libraries
// (not every library installed in the image); MAX_DEPENDENCY_DEPTH bounds one DFS
// branch. Together with `visiting` cycle detection they make hostile DT_NEEDED
// graphs consume bounded storage reads, allocations, recursion and address slots.
const MAX_MODULES: usize = 64;
const MAX_DEPENDENCY_DEPTH: usize = 16;

struct MappedFile {
	handle: u64,
	address: u64,
	len: usize,
}

impl MappedFile {
	unsafe fn open(storage: u64, path: String) -> Option<MappedFile> {
		unsafe {
			let mut client = volume::Client::new(ChannelTransport { chan: storage });
			let result = match client.open(&OpenOpts { path, write: false, create: false })? {
				Ok(result) if result.file != 0 && result.size != 0 => result,
				_ => return None,
			};
			let len = match usize::try_from(result.size) {
				Ok(len) => len,
				Err(_) => {
					close(result.file);
					return None;
				}
			};
			let address = match map_object(result.file) {
				Some(address) => address,
				None => {
					close(result.file);
					return None;
				}
			};
			Some(MappedFile { handle: result.file, address, len })
		}
	}

	unsafe fn bytes(&self) -> &[u8] {
		unsafe { core::slice::from_raw_parts(self.address as *const u8, self.len) }
	}
}

impl Drop for MappedFile {
	fn drop(&mut self) {
		unsafe {
			unmap_object(self.handle);
			close(self.handle);
		}
	}
}

struct Resolver {
	storage: u64,
	process: u64,
	loaded: Vec<String>,
	visiting: Vec<String>,
}

impl Resolver {
	unsafe fn load(&mut self, name: &str, depth: usize) -> bool {
		unsafe {
			if self.loaded.iter().any(|loaded| loaded == name) {
				return true;
			}
			if depth >= MAX_DEPENDENCY_DEPTH || self.loaded.len() >= MAX_MODULES || self.visiting.iter().any(|visiting| visiting == name) || !valid_library_name(name) {
				return false;
			}
			self.visiting.push(String::from(name));
			let loaded = (|| {
				let file = MappedFile::open(self.storage, alloc::format!("{LIBRARY_DIR}{name}"))?;
				let bytes = file.bytes();
				let elf = bootproto::elf::Elf::parse(bytes)?;
				if elf.image_type != bootproto::elf::ET_DYN {
					return None;
				}
				let dynamic = elf.dynamic_info()??;
				let dependencies = dependencies(&elf, &dynamic)?;
				for dependency in dependencies {
					if !self.load(&dependency, depth + 1) {
						return None;
					}
				}
				let bias = LIBRARY_BASE.checked_add((self.loaded.len() as u64).checked_mul(LIBRARY_SLOT_SIZE)?)?;
				if process_load_module(self.process, bytes, bias) < 0 {
					return None;
				}
				Some(())
			})()
			.is_some();
			self.visiting.pop();
			if loaded {
				self.loaded.push(String::from(name));
			}
			loaded
		}
	}
}

fn valid_library_name(name: &str) -> bool {
	let Some(stem) = name.strip_suffix(".lslib") else { return false };
	!stem.is_empty() && !stem.starts_with("lib") && name.len() <= 64 && stem.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn dependencies(elf: &bootproto::elf::Elf<'_>, dynamic: &bootproto::elf::DynamicInfo) -> Option<Vec<String>> {
	let mut dependencies = Vec::new();
	for name in elf.needed_names(dynamic)? {
		if !valid_library_name(name) || dependencies.iter().any(|dependency: &String| dependency == name) {
			return None;
		}
		dependencies.push(String::from(name));
	}
	Some(dependencies)
}

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
	// new process handle plus its canonical physical basename, or None if the command
	// is malformed, absent or cannot be spawned.
	unsafe fn spawn_program(&self, name: &str, bootstrap: u64) -> Option<(i64, String)> {
		unsafe {
			if let Some((path, basename)) = executable::explicit_path(name) {
				if self.storage == 0 {
					return None;
				}
				let handle = spawn_from_path(self.storage, path, bootstrap)?;
				return (handle >= 0).then(|| (handle, String::from(basename)));
			}
			for artifact in executable::launch_candidates(name)? {
				let handle = if self.storage != 0 {
					match spawn_from_path(self.storage, &alloc::format!("{PROGRAM_DIR}{artifact}"), bootstrap) {
						Some(handle) => handle,
						None => continue,
					}
				} else {
					match self.package.lookup(artifact.as_bytes()) {
						Some(elf) => spawn_program_bytes(self.storage, elf, bootstrap),
						None => continue,
					}
				};
				return (handle >= 0).then_some((handle, artifact));
			}
			None
		}
	}
}

// Read one exact `.lsexe` path through the storage client, map its shared buffer,
// create a process from the mapped ELF image, then release the mapping. Returns the new
// process handle. None means the named artifact was absent; a present but invalid
// artifact returns a negative handle so resolution never falls through to another name.
unsafe fn spawn_from_path(storage: u64, path: &str, bootstrap: u64) -> Option<i64> {
	unsafe {
		let main = MappedFile::open(storage, String::from(path))?;
		Some(spawn_program_bytes(storage, main.bytes(), bootstrap))
	}
}

unsafe fn spawn_program_bytes(storage: u64, bytes: &[u8], bootstrap: u64) -> i64 {
	unsafe {
		let Some(elf) = bootproto::elf::Elf::parse(bytes) else { return -1 };
		let Some(dynamic) = elf.dynamic_info() else { return -1 };
		let Some(dynamic) = dynamic else { return spawn(bytes, bootstrap) };
		let Some(dependencies) = dependencies(&elf, &dynamic) else { return -1 };
		if dependencies.is_empty() {
			return spawn(bytes, bootstrap);
		}
		if storage == 0 {
			return -1;
		}
		let process = process_create(0);
		if process < 0 {
			return process;
		}
		let process = process as u64;
		let mut resolver = Resolver { storage, process, loaded: Vec::new(), visiting: Vec::new() };
		for dependency in dependencies {
			if !resolver.load(&dependency, 0) {
				close(process);
				return -1;
			}
		}
		let entry = process_load_main(process, bytes);
		if entry < 0 {
			close(process);
			return entry;
		}
		let started = process_start(process, entry as u64, bootstrap);
		if started < 0 {
			close(process);
		}
		started
	}
}

impl<'a> Service for Processes<'a> {
	fn start(&mut self, name: String) -> Result<ProcessInfo, Error> {
		// spawn with no bootstrap capability (phase 1: started processes run
		// unattended), then read back the new process's koid and record it.
		let (handle, artifact) = unsafe { self.spawn_program(&name, 0) }.ok_or(Error::NotFound)?;
		let koid: u64 = unsafe { object_info(handle as u64) }.map(|i| i.koid).ok_or(Error::Again)?;
		unsafe { close(handle as u64) };
		let info: ProcessInfo = ProcessInfo { koid, name: artifact };
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
		let (handle, artifact) = unsafe { self.spawn_program(&name, bootstrap) }.ok_or(Error::NotFound)?;
		let koid: u64 = unsafe { object_info(handle as u64) }.map(|i| i.koid).ok_or(Error::Again)?;
		let info: ProcessInfo = ProcessInfo { koid, name: artifact };
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
