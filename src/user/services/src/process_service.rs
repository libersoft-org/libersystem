// ProcessService - the userspace typed process-lifecycle service.
//
// ServiceManager starts this program from the init package and hands it a
// bootstrap channel, over which it receives a StorageService client (the system
// volume, from which it loads the on-disk program binaries under `system/bin`), a
// read-only view of the init package (the bring-up fallback when no storage client is
// wired) and a "SERVE" channel its clients reach it on. Over that channel clients speak
// the generated `liber:system` Process bindings: they START a named program unattended,
// LAUNCH one with a caller-provided bootstrap channel (so a policy front end like
// PermissionManager can grant the new process its capabilities over that channel), LAUNCH
// BOUNDED with the same bootstrap under a reusable aggregate memory-limited child Domain,
// receive back the live process handle for job control, and LIST the processes started so
// far as typed `process-info` records (koid + name) that render as CLI / JSON on the client.
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
use ipc_client::ChannelTransport;
use proto::system::process::{self, Service};
use proto::system::volume;
use proto::system::{Error, OpenOpts, ProcessInfo, StartResult};
use rt::*;
use services::executable;

// Where the on-disk program binaries live on the system volume (staged there by the
// factory-seed pipeline). A named program is loaded from `<PROGRAM_DIR><name>`.
const PROGRAM_DIR: &str = "vol://system/bin/";
const LIBRARY_DIR: &str = "vol://system/lib/";
const LIBRARY_IDENTITY_DIR: &str = "vol://system/id/lib/";
const EXECUTABLE_IDENTITY_DIR: &str = "vol://system/id/bin/";
const ORDER_DIR: &str = "vol://system/order/";
const LIBRARY_BASE: u64 = 0x2000_0000;
const LIBRARY_SLOT_SIZE: u64 = 0x0100_0000;
const MAX_IDENTITY_BYTES: usize = 8 * 1024;
const IDENTITY_FORMAT: &[u8] = b"format=liber-image-identity-v1";
#[cfg(target_arch = "x86_64")]
const IMAGE_TARGET: &str = "x86_64-unknown-none";
#[cfg(target_arch = "aarch64")]
const IMAGE_TARGET: &str = "aarch64-unknown-none";
#[cfg(target_arch = "riscv64")]
const IMAGE_TARGET: &str = "riscv64gc-unknown-none-elf";
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

struct Identity {
	digest: [u8; 32],
	providers: Vec<(String, [u8; 32])>,
}

fn valid_identity_name(name: &str) -> bool {
	!name.is_empty() && !name.starts_with("lib") && name.len() <= 58 && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn identity_value<'a>(line: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
	line.starts_with(key).then(|| &line[key.len()..])
}

fn identity_field_matches(line: &[u8], key: &[u8], value: &[u8]) -> bool {
	identity_value(line, key).is_some_and(|actual| actual == value)
}

fn hex_value(byte: u8) -> Option<u8> {
	match byte {
		b'0'..=b'9' => Some(byte - b'0'),
		b'a'..=b'f' => Some(byte - b'a' + 10),
		b'A'..=b'F' => Some(byte - b'A' + 10),
		_ => None,
	}
}

fn valid_hex(bytes: &[u8], len: usize) -> bool {
	bytes.len() == len && bytes.iter().all(|byte| hex_value(*byte).is_some())
}

fn parse_digest(bytes: &[u8]) -> Option<[u8; 32]> {
	if !valid_hex(bytes, 64) {
		return None;
	}
	let mut digest = [0u8; 32];
	for (index, pair) in bytes.chunks_exact(2).enumerate() {
		digest[index] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
	}
	Some(digest)
}

fn parse_identity(bytes: &[u8], kind: &str, artifact: &str) -> Option<Identity> {
	if bytes.is_empty() || bytes.len() > MAX_IDENTITY_BYTES || !bytes.ends_with(b"\n") || !valid_identity_name(artifact) {
		return None;
	}
	let mut lines = bytes[..bytes.len() - 1].split(|byte| *byte == b'\n');
	if lines.next()? != IDENTITY_FORMAT || !identity_field_matches(lines.next()?, b"kind=", kind.as_bytes()) || !identity_field_matches(lines.next()?, b"artifact=", artifact.as_bytes()) {
		return None;
	}
	let package = identity_value(lines.next()?, b"package=")?;
	let source = identity_value(lines.next()?, b"source-sha256=")?;
	let rustc = identity_value(lines.next()?, b"rustc-commit=")?;
	if package.is_empty() || !valid_hex(source, 64) || !valid_hex(rustc, 40) || !identity_field_matches(lines.next()?, b"target=", IMAGE_TARGET.as_bytes()) || !identity_field_matches(lines.next()?, b"profile=", b"release") {
		return None;
	}
	let rustflags = identity_value(lines.next()?, b"rustflags=")?;
	let features = identity_value(lines.next()?, b"features=")?;
	if !rustflags.starts_with(b"-C relocation-model=pic") || features.is_empty() {
		return None;
	}
	let mut providers = Vec::new();
	for line in lines {
		let value = identity_value(line, b"provider=")?;
		let separator = value.iter().position(|byte| *byte == b':')?;
		let provider = core::str::from_utf8(&value[..separator]).ok()?;
		if !valid_identity_name(provider) || providers.len() >= MAX_MODULES || providers.iter().any(|(name, _)| name == provider) {
			return None;
		}
		providers.push((String::from(provider), parse_digest(&value[separator + 1..])?));
	}
	Some(Identity { digest: bootproto::sha256::digest(bytes), providers })
}

fn verify_identity(elf: &bootproto::elf::Elf<'_>, bytes: &[u8], kind: &str, artifact: &str) -> Option<Identity> {
	let identity = parse_identity(bytes, kind, artifact)?;
	(elf.liber_identity_note_digest()? == identity.digest).then_some(identity)
}

unsafe fn load_identity(storage: u64, directory: &str, kind: &str, artifact: &str, elf: &bootproto::elf::Elf<'_>) -> Option<Identity> {
	unsafe {
		let record = MappedFile::open(storage, alloc::format!("{directory}{artifact}"))?;
		verify_identity(elf, record.bytes(), kind, artifact)
	}
}

struct Module {
	name: String,
	file: MappedFile,
	dependencies: Vec<String>,
	identity: Identity,
}

fn identity_matches_dependencies(identity: &Identity, dependencies: &[String], modules: &[Module]) -> bool {
	if identity.providers.len() != dependencies.len() {
		return false;
	}
	for dependency in dependencies {
		let Some(name) = dependency.strip_suffix(".lslib") else { return false };
		let Some(module) = modules.iter().find(|module| module.name.as_str() == dependency.as_str()) else { return false };
		let Some((_, digest)) = identity.providers.iter().find(|(provider, _)| provider.as_str() == name) else { return false };
		if *digest != module.identity.digest {
			return false;
		}
	}
	true
}

struct Resolver {
	storage: u64,
	modules: Vec<Module>,
	visiting: Vec<String>,
}

impl Resolver {
	unsafe fn collect(&mut self, name: &str, depth: usize) -> bool {
		unsafe {
			if self.modules.iter().any(|module| module.name == name) {
				return true;
			}
			if depth >= MAX_DEPENDENCY_DEPTH || self.modules.len() >= MAX_MODULES || self.visiting.iter().any(|visiting| visiting == name) || !valid_library_name(name) {
				return false;
			}
			self.visiting.push(String::from(name));
			let module = (|| {
				let file = MappedFile::open(self.storage, alloc::format!("{LIBRARY_DIR}{name}"))?;
				let bytes = file.bytes();
				let elf = bootproto::elf::Elf::parse(bytes)?;
				if elf.image_type != bootproto::elf::ET_DYN {
					return None;
				}
				let stem = name.strip_suffix(".lslib")?;
				let identity = load_identity(self.storage, LIBRARY_IDENTITY_DIR, "library", stem, &elf)?;
				let dynamic = elf.dynamic_info()??;
				let dependencies = dependencies(&elf, &dynamic)?;
				for dependency in &dependencies {
					if !self.collect(dependency, depth + 1) {
						return None;
					}
				}
				if !identity_matches_dependencies(&identity, &dependencies, &self.modules) {
					return None;
				}
				Some(Module { name: String::from(name), file, dependencies, identity })
			})();
			self.visiting.pop();
			if let Some(module) = module {
				self.modules.push(module);
				true
			} else {
				false
			}
		}
	}

	fn order(&self) -> Option<Vec<String>> {
		let mut order = Vec::with_capacity(self.modules.len());
		while order.len() < self.modules.len() {
			let module = self.modules.iter().filter(|module| !order.iter().any(|name: &String| name == &module.name) && module.dependencies.iter().all(|dependency| order.iter().any(|name| name == dependency))).min_by(|left, right| left.name.cmp(&right.name))?;
			order.push(module.name.clone());
		}
		Some(order)
	}

	unsafe fn load(self, process: u64, order: &[String]) -> bool {
		unsafe {
			for (index, name) in order.iter().enumerate() {
				let Some(module) = self.modules.iter().find(|module| &module.name == name) else { return false };
				let Some(bias) = LIBRARY_BASE.checked_add((index as u64).checked_mul(LIBRARY_SLOT_SIZE).unwrap_or(u64::MAX)) else { return false };
				if process_load_module(process, module.file.bytes(), bias) < 0 {
					return false;
				}
			}
			true
		}
	}
}

fn valid_library_name(name: &str) -> bool {
	name.strip_suffix(".lslib").is_some_and(valid_identity_name)
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

fn parse_order(bytes: &[u8]) -> Option<Vec<String>> {
	if bytes.is_empty() || bytes.len() > MAX_MODULES * 65 || bytes.last() != Some(&b'\n') {
		return None;
	}
	let text = core::str::from_utf8(bytes).ok()?;
	let mut order = Vec::new();
	for name in text.lines() {
		if order.len() >= MAX_MODULES || !valid_library_name(name) || order.iter().any(|loaded: &String| loaded == name) {
			return None;
		}
		order.push(String::from(name));
	}
	(!order.is_empty()).then_some(order)
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
	bounded_domains: Vec<(u64, u64)>,
}

impl<'a> Processes<'a> {
	unsafe fn bounded_domain(&mut self, memory_limit: u64) -> Result<u64, Error> {
		if let Some((_, domain)) = self.bounded_domains.iter().find(|(limit, _)| *limit == memory_limit) {
			return Ok(*domain);
		}
		let domain = unsafe { domain_create(memory_limit, u64::MAX, u64::MAX) };
		if domain < 0 {
			return Err(Error::Again);
		}
		let domain = domain as u64;
		self.bounded_domains.push((memory_limit, domain));
		Ok(domain)
	}

	// Load program `name` and create a process from it, handing the child `bootstrap` as
	// its bootstrap capability. With a storage client wired, the binary is read from the
	// system volume's `bin/`; with none, it comes from the built-in package. Returns the
	// new process handle plus its canonical physical basename, or None if the command
	// is malformed, absent or cannot be spawned.
	unsafe fn spawn_program(&self, name: &str, bootstrap: u64, domain: u64) -> Option<(i64, String)> {
		unsafe {
			if let Some((path, basename)) = executable::explicit_path(name) {
				if self.storage == 0 {
					return None;
				}
				let handle = spawn_from_path(self.storage, path, basename, bootstrap, domain)?;
				return (handle >= 0).then(|| (handle, String::from(basename)));
			}
			for artifact in executable::launch_candidates(name)? {
				let handle = if self.storage != 0 {
					match spawn_from_path(self.storage, &alloc::format!("{PROGRAM_DIR}{artifact}"), &artifact, bootstrap, domain) {
						Some(handle) => handle,
						None => continue,
					}
				} else {
					match self.package.lookup(artifact.as_bytes()) {
						Some(elf) => spawn_program_bytes(self.storage, elf, None, None, bootstrap, domain),
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
unsafe fn spawn_from_path(storage: u64, path: &str, artifact: &str, bootstrap: u64, domain: u64) -> Option<i64> {
	unsafe {
		let main = MappedFile::open(storage, String::from(path))?;
		let logical_name = executable::logical_name(artifact)?;
		let identity = MappedFile::open(storage, alloc::format!("{EXECUTABLE_IDENTITY_DIR}{logical_name}"))?;
		let order = MappedFile::open(storage, alloc::format!("{ORDER_DIR}{logical_name}"));
		Some(spawn_program_bytes(storage, main.bytes(), Some((identity.bytes(), logical_name)), order.as_ref().map(|file| file.bytes()), bootstrap, domain))
	}
}

unsafe fn spawn_program_bytes(storage: u64, bytes: &[u8], expected_identity: Option<(&[u8], &str)>, expected_order: Option<&[u8]>, bootstrap: u64, domain: u64) -> i64 {
	unsafe {
		let Some(elf) = bootproto::elf::Elf::parse(bytes) else { return -1 };
		let Some(dynamic) = elf.dynamic_info() else { return -1 };
		let Some(dynamic) = dynamic else {
			if expected_identity.is_none() {
				return spawn_in(bytes, bootstrap, domain);
			}
			return -1;
		};
		let Some((identity_bytes, artifact)) = expected_identity else { return -1 };
		let Some(identity) = verify_identity(&elf, identity_bytes, "executable", artifact) else { return -1 };
		let Some(dependencies) = dependencies(&elf, &dynamic) else { return -1 };
		if dependencies.is_empty() {
			if !identity_matches_dependencies(&identity, &dependencies, &[]) {
				return -1;
			}
			return spawn(bytes, bootstrap);
		}
		if storage == 0 {
			return -1;
		}
		let mut resolver = Resolver { storage, modules: Vec::new(), visiting: Vec::new() };
		for dependency in &dependencies {
			if !resolver.collect(dependency, 0) {
				return -1;
			}
		}
		if !identity_matches_dependencies(&identity, &dependencies, &resolver.modules) {
			return -1;
		}
		let Some(order) = resolver.order() else { return -1 };
		let Some(expected_order) = expected_order.and_then(parse_order) else { return -1 };
		if order != expected_order {
			return -1;
		}
		let process = process_create(domain);
		if process < 0 {
			return process;
		}
		let process = process as u64;
		if !resolver.load(process, &order) {
			close(process);
			return -1;
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
		let (handle, artifact) = unsafe { self.spawn_program(&name, 0, 0) }.ok_or(Error::NotFound)?;
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
		let (handle, artifact) = unsafe { self.spawn_program(&name, bootstrap, 0) }.ok_or(Error::NotFound)?;
		let koid: u64 = unsafe { object_info(handle as u64) }.map(|i| i.koid).ok_or(Error::Again)?;
		let info: ProcessInfo = ProcessInfo { koid, name: artifact };
		self.started.push(info.clone());
		Ok(StartResult { task: handle as u64, info })
	}

	fn launch_bounded(&mut self, name: String, memory_limit: u64, bootstrap: u64) -> Result<StartResult, Error> {
		let domain = unsafe { self.bounded_domain(memory_limit)? };
		let (handle, artifact) = unsafe { self.spawn_program(&name, bootstrap, domain) }.ok_or(Error::NotFound)?;
		let koid = unsafe { object_info(handle as u64) }.map(|info| info.koid).ok_or(Error::Again)?;
		let info = ProcessInfo { koid, name: artifact };
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
	let mut procs: Processes = Processes { package, storage, started: Vec::new(), bounded_domains: Vec::new() };
	let mut request: [u8; 256] = [0u8; 256];
	let mut reply: [u8; 4096] = [0u8; 4096];
	unsafe {
		serve_multi(service, &mut request, &mut reply, |_chan: u64, req: &[u8], handle: &mut u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> { process::dispatch(&mut procs, req, handle, out, reply_handle) });
	}
	exit();
}
