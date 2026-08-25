// ProcessService - the userspace typed process-lifecycle service.
//
// ServiceManager starts this program from the init package and hands it a
// bootstrap channel, over which it receives a StorageService client (the system
// volume, from which it loads on-disk programs through their manifest paths), a
// read-only view of the init package (the bring-up fallback when no storage client is
// wired) and a "SERVE" channel its clients reach it on. Over that channel clients speak
// the generated `liber:system` Process bindings: they START a named program unattended,
// LAUNCH one with a caller-provided bootstrap channel (so a policy front end like
// PermissionManager can grant the new process its capabilities over that channel), LAUNCH
// BOUNDED with the same bootstrap under a memory-limited child Domain of its own,
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
use proto::system::{Budget, Error, OpenOpts, ProcessInfo, ResourceType, ResourceUsage, StartResult};
use rt::*;
use services::REGISTRY_ANNOUNCEMENT;
use services::executable;
use services::graph_limits;

const LIBRARY_BASE: u64 = 0x2000_0000;
const LIBRARY_SLOT_SIZE: u64 = 0x0100_0000;
const IDENTITY_FORMAT: &[u8] = b"format=liber-image-identity-v1";
#[cfg(target_arch = "x86_64")]
const IMAGE_TARGET: &str = "x86_64-unknown-none";
#[cfg(target_arch = "aarch64")]
const IMAGE_TARGET: &str = "aarch64-unknown-none";
#[cfg(target_arch = "riscv64")]
const IMAGE_TARGET: &str = "riscv64gc-unknown-none-elf";

include!(concat!(env!("OUT_DIR"), "/library_paths.rs"));
include!(concat!(env!("OUT_DIR"), "/program_path.rs"));

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
	if bytes.is_empty() || bytes.len() > bootproto::elf::MAX_LIBER_IDENTITY_RECORD_BYTES || !bytes.ends_with(b"\n") || !valid_identity_name(artifact) {
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
	let mut providers: Vec<(String, [u8; 32])> = Vec::new();
	for line in lines {
		let value = identity_value(line, b"provider=")?;
		let separator = value.iter().position(|byte| *byte == b':')?;
		let provider = core::str::from_utf8(&value[..separator]).ok()?;
		if !valid_identity_name(provider) || providers.len() >= graph_limits::MAX_MODULES || providers.last().is_some_and(|(previous, _)| provider <= previous.as_str()) {
			return None;
		}
		providers.push((String::from(provider), parse_digest(&value[separator + 1..])?));
	}
	Some(Identity { digest: bootproto::sha256::digest(bytes), providers })
}

fn verify_identity(elf: &bootproto::elf::Elf<'_>, kind: &str, artifact: &str) -> Option<Identity> {
	parse_identity(elf.liber_identity_note()?, kind, artifact)
}

// Where a loaded provider's bytes come from. The installed image is mapped from the volume
// and read in place; a registry generation is held as bytes, because there is no file behind
// it. Nothing below this type cares which it is: a generation only reaches here after it has
// been proven to stand in for the installed image.
enum Image {
	Installed(MappedFile),
	Registry(Vec<u8>),
}

impl Image {
	unsafe fn bytes(&self) -> &[u8] {
		match self {
			Image::Installed(file) => unsafe { file.bytes() },
			Image::Registry(bytes) => bytes,
		}
	}
}

struct Module {
	name: String,
	image: Image,
	dependencies: Vec<String>,
	// The identity digest of the *installed* image at this name, which is what a consumer's
	// record names whether or not a registry generation is standing in for it. For an installed
	// module it is that module's own digest; for a shadowing generation it is the digest of the
	// image the generation was proven interchangeable with. Keeping the installed digest is what
	// lets the dependency check below stay a plain comparison: the compatibility argument was
	// made once, where both images were in hand, rather than repeated per consumer.
	baseline: [u8; 32],
}

fn identity_matches_dependencies(identity: &Identity, dependencies: &[String], modules: &[Module]) -> bool {
	if identity.providers.len() != dependencies.len() {
		return false;
	}
	for dependency in dependencies {
		let Some(name) = dependency.strip_suffix(".lslib") else { return false };
		let Some(module) = modules.iter().find(|module| module.name.as_str() == dependency.as_str()) else { return false };
		let Some((_, digest)) = identity.providers.iter().find(|(provider, _)| provider.as_str() == name) else { return false };
		if *digest != module.baseline {
			return false;
		}
	}
	true
}

struct Resolver {
	storage: u64,
	// The development registry, or zero. Zeroed for the rest of this resolution as soon as a
	// query goes unanswered: one launch must not pay the answer timeout once per provider
	// because the agent is busy or gone.
	registry: u64,
	modules: Vec<Module>,
	visiting: Vec<String>,
}

impl Resolver {
	unsafe fn collect(&mut self, name: &str, depth: usize) -> bool {
		unsafe {
			if self.modules.iter().any(|module| module.name == name) {
				return true;
			}
			if !graph_limits::can_visit(depth, self.modules.len(), self.visiting.iter().any(|visiting| visiting == name)) || !valid_library_name(name) {
				return false;
			}
			self.visiting.push(String::from(name));
			let module = (|| {
				let stem = name.strip_suffix(".lslib")?;
				// Name the provider that is missing. Everything below unwinds through `?` into a
				// bare "could not start", which tells a caller nothing about WHICH library the
				// image lacks - and a missing provider is the most likely reason a correctly
				// built program will not start on an incorrectly built image.
				let Some(path) = library_path(name) else {
					print(b"loader: ");
					print(name.as_bytes());
					print(b": no install path is declared for this provider\n");
					return None;
				};
				let Some(installed) = MappedFile::open(self.storage, String::from(path)) else {
					print(b"loader: ");
					print(name.as_bytes());
					print(b": provider not found at ");
					print(path.as_bytes());
					print(b"\n");
					return None;
				};
				// The installed image is read first even when a generation will replace it, and
				// that order is the whole of the rule: the digest a consumer's record names is
				// this one, and compatibility is a statement about these two images.
				let baseline = verify_identity(&bootproto::elf::Elf::parse(installed.bytes())?, "library", stem)?.digest;
				let image = match registry_generation(&mut self.registry, stem) {
					// A generation may stand in for the installed provider only when the written
					// rule says a process that has already resolved against the installed one
					// could not tell the difference. The publication reported the same verdict
					// when it arrived, but the loader decides for itself: the registry holds
					// incompatible generations too, and this is the point where they are refused.
					// Refused, not skipped - loading the installed image instead would run
					// something other than what was published while looking like success.
					Some(shadow) => {
						let compatible = bootproto::compat::decide(installed.bytes(), &shadow).is_compatible();
						drop(installed);
						if !compatible {
							return None;
						}
						Image::Registry(shadow)
					}
					None => Image::Installed(installed),
				};
				let bytes = image.bytes();
				let elf = bootproto::elf::Elf::parse(bytes)?;
				if elf.image_type != bootproto::elf::ET_DYN {
					return None;
				}
				let identity = verify_identity(&elf, "library", stem)?;
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
				Some(Module { name: String::from(name), image, dependencies, baseline })
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
				if process_load_module(process, module.image.bytes(), bias) < 0 {
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
	// The development registry, when there is one. A launch asks it whether it holds a
	// generation of the artifact about to be loaded, and uses those bytes instead.
	//
	// The handle is present on every boot, because it is handed over before anything could
	// know whether an agent will exist. What decides is `registry_armed`: the agent announces
	// itself on this channel when it is given the other end, and until that announcement
	// arrives nothing is asked and every launch reads the volume. Querying an unarmed channel
	// would block this service - the one every launch goes through - against a peer that may
	// never exist, which is a boot that never finishes.
	registry: u64,
	registry_armed: bool,
	started: Vec<Launched>,
	// Launches that exist but have not run, by koid and start token. A pipeline prepares every
	// stage, installs the endpoints between them and then releases them, so that no stage can
	// write into a consumer that has not been given its reader.
	//
	// A token left here is a process that never runs, which is what an abandoned transaction
	// should leave behind: harmless, and reaped with everything else.
	// (owner channel, koid, token). The OWNER is what makes this safe.
	//
	// A prepared launch was keyed by koid alone, and a koid is not a secret - `process.list` returns
	// them. Any client of this shared service could therefore release, and so START, another
	// client's prepared launch: the one step a pipeline builder performs precisely because it wants
	// to decide when the program runs. The channel a request arrived on is the client's identity and
	// `serve_multi` already provides it; it was simply being discarded.
	prepared: Vec<(u64, u64, Spawned)>,
	// The channel the request being dispatched arrived on, set by the serve loop before dispatch.
	// Zero means "no client" - a self-call or a test - which owns nothing and can release nothing.
	client: u64,
}

// One launched process, and the handle that lets this service find out whether it is still
// running. The record used to be the `ProcessInfo` alone, which is why `ps` listed every
// process the system had ever started: nothing removed an entry, because nothing could tell
// that a process had ended. `launch` transfers its handle to the caller for job control, so
// the handle kept here is a duplicate with READ only - enough for `process_stats` and not
// enough to signal or otherwise act on somebody else's job.
//
// A duplicate keeps the Process object alive after the program exits, which is a zombie of
// exactly one small kernel object and no memory: a process's mappings go with its threads.
// `reap` closes it at the first opportunity anyone asks anything, so the zombie lasts until
// the next launch or the next `ps` rather than until reboot.
struct Launched {
	info: ProcessInfo,
	handle: u64,
	// The Domain this launch runs in, when it was given one of its own. Kept so `accounting`
	// can report what it is holding: a per-launch Domain is invisible to ResourceManager,
	// which reports only the Domains it was handed, so isolation was enforced and could not
	// be observed. It is closed by the same `reap` that closes the process handle, so it
	// still lives exactly as long as what it accounts - handing this handle to an observer
	// instead would keep the Domain alive until that observer learned the process had ended,
	// which is the lifetime problem a Domain-per-launch was created to remove.
	//
	// 0 for a launch that runs in the caller's Domain, which is every launch without a stated
	// memory limit.
	domain: u64,
}

impl<'a> Processes<'a> {
	// Record a launch, having first dropped whatever has ended since the last one. Reaping
	// here as well as in `list` is what bounds the record by the number of live processes
	// rather than by how long the system has been up: a guest nobody runs `ps` on would
	// otherwise accumulate an entry per command exactly as before, just without lying about
	// them when finally asked.
	fn record(&mut self, info: ProcessInfo, handle: u64, domain: u64) {
		self.reap();
		self.started.push(Launched { info, handle, domain });
	}

	// Drop every process that is no longer running, closing the handle that was holding its
	// kernel object. A handle this service could not duplicate (0) is kept rather than
	// guessed about - reporting a live process as gone is the failure this replaced, in the
	// other direction - and `process_stats` returning None means the object is already gone,
	// which is as terminated as it gets.
	fn reap(&mut self) {
		let mut live: Vec<Launched> = Vec::new();
		for entry in core::mem::take(&mut self.started) {
			if entry.handle == 0 {
				live.push(entry);
				continue;
			}
			match unsafe { process_stats(entry.handle) } {
				Some(stats) if stats.state == PROC_STATE_RUNNING => live.push(entry),
				// STOPPED and never finished: it has not STARTED yet.
				//
				// `PROC_STATE_STOPPED` is `live_threads().is_empty()`, which is true of a process
				// whose threads have all exited AND of one whose entry thread has been started but
				// not yet picked up by a scheduler. Reaping on the state alone cannot tell those
				// apart, so a launch recorded microseconds before the next one was dropped on the
				// next one's way in - and `ps` showed one process where two had started.
				//
				// Seen twice: riscv64 2026-08-10 in a run of 208, x86_64 2026-08-09 once in 211 and
				// not on the immediate re-run. The window is general; x86_64 merely hits it less.
				//
				// SEEN AGAIN, x86_64 2026-08-13, and this branch cannot be the reason: it keeps a
				// STOPPED entry, so the one that disappeared reported either an exit status or no
				// stats at all - it had really finished. The immediate re-run of the same tree
				// passed. What that points at is the HARNESS rather than the reap: the child's only
				// reason to live is its bootstrap channel, and whether it has reached the end of it
				// by the time the list request is drained is not something the test controls. The
				// discriminator this branch adds is still right; it is not the whole race, and
				// The sighting is recorded with the allocator's own evidence.
				//
				// `completion_valid` is the discriminator the state does not carry: the kernel sets
				// it from `exit_status()`, which exists only once the process has actually finished.
				// So "not running and no exit status" means not started yet, and the entry stays.
				//
				// This does not leak, and the leak is the reason the obvious fix - remember whether
				// an entry was ever seen RUNNING - was rejected. A launch that FAILS is never
				// recorded at all (`record` runs after `spawn_program` succeeded), a killed process
				// reads `PROC_STATE_FAILED` and is reaped, and a vanished one gives `None`. The only
				// entry kept indefinitely would be a process that never runs and never exits, which
				// is a kernel fault rather than a bookkeeping one - and keeping a live process is
				// the direction this whole function is supposed to err in.
				Some(stats) if stats.state == PROC_STATE_STOPPED && stats.completion_valid == 0 => live.push(entry),
				_ => unsafe {
					close(entry.handle);
					// The Domain goes with the process it accounted. The kernel frees it once
					// nothing holds it, and this record was the last holder.
					if entry.domain != 0 {
						close(entry.domain);
					}
				},
			}
		}
		self.started = live;
	}

	// A resource Domain for one launch, and only that one.
	//
	// It used to be one Domain per distinct limit, shared by every launch that asked for the
	// same number, which made the limit an aggregate budget: two concurrent runs of the same
	// tool divided it. The one limit in the system is `imgconv`'s, and it was measured as the
	// whole-Domain peak of a SINGLE run - so the aggregate reading was the one the number was
	// not sized for, and a second concurrent run would have failed inside a budget that looked
	// generous. Per launch, each run gets what was measured.
	//
	// It also makes the Domain the scope of the thing it accounts. The shared ones were created
	// on first use and never released, because there was nothing to release them at: a cache
	// keyed by a number has no idea when the last user is gone. One per launch is handed to the
	// process and forgotten, and the kernel frees it when the process does.
	unsafe fn bounded_domain(&mut self, memory_limit: u64) -> Result<u64, Error> {
		let domain = unsafe { domain_create(memory_limit, u64::MAX, u64::MAX) };
		if domain < 0 {
			return Err(Error::Again);
		}
		Ok(domain as u64)
	}

	// Load program `name` and create a process from it, handing the child `bootstrap` as
	// its bootstrap capability. With a storage client wired, the binary is read from the
	// system volume's manifest-declared path; with none, it comes from the built-in package. Returns the
	// new process handle plus its canonical physical basename, or None if the command
	// is malformed, absent or cannot be spawned.
	// Whether the development agent has announced itself on the registry channel. Checked
	// without blocking, and only until it has: after that the channel is known live.
	unsafe fn registry_ready(&mut self) -> bool {
		unsafe {
			if self.registry == 0 {
				return false;
			}
			if !self.registry_armed && channel_peek(self.registry) >= 0 {
				let mut buf: [u8; 16] = [0u8; 16];
				if let Received::Message { .. } = recv_blocking(self.registry, &mut buf) {
					self.registry_armed = true;
				}
			}
			self.registry_armed
		}
	}

	unsafe fn spawn_program(&mut self, name: &str, bootstrap: u64, domain: u64) -> Option<(Spawned, String)> {
		unsafe {
			if let Some((path, basename)) = executable::explicit_path(name) {
				if self.storage == 0 {
					return None;
				}
				let registry: u64 = if self.registry_ready() { self.registry } else { 0 };
				let spawned = spawn_from_path(self.storage, registry, path, basename, bootstrap, domain)?;
				name_process(spawned.process, basename);
				return Some((spawned, String::from(basename)));
			}
			let registry: u64 = if self.registry_ready() { self.registry } else { 0 };
			for artifact in executable::launch_candidates(name)? {
				let spawned = if self.storage != 0 {
					let logical_name = executable::logical_name(&artifact)?;
					let path = program_path(logical_name)?;
					match spawn_from_path(self.storage, registry, path, &artifact, bootstrap, domain) {
						Some(spawned) => spawned,
						None => continue,
					}
				} else {
					match self.package.lookup(artifact.as_bytes()) {
						Some(elf) => match spawn_program_bytes(self.storage, 0, elf, None, bootstrap, domain) {
							Some(spawned) => spawned,
							None => continue,
						},
						None => continue,
					}
				};
				name_process(spawned.process, &artifact);
				return Some((spawned, artifact));
			}
			None
		}
	}
}

// Label the new process with the artifact it was launched as, so a fault message can name it.
// The kernel reads a name out of a staged image's identity note, which covers everything on
// the volume; the static programs in the init package carry no note, and this is the only
// place their name is known. Best effort by design - a process that could not be labelled is
// still a process, and refusing the launch over a label would trade a working system for a
// better log message.
fn name_process(handle: u64, artifact: &str) {
	unsafe { set_object_name(handle, artifact) };
}

// Read one exact `.lsexe` path through the storage client, map its shared buffer,
// create a process from the mapped ELF image, then release the mapping. Returns the new
// process handle. None means the named artifact was absent; a present but invalid
// artifact returns a negative handle so resolution never falls through to another name.
unsafe fn spawn_from_path(storage: u64, mut registry: u64, path: &str, artifact: &str, bootstrap: u64, domain: u64) -> Option<Spawned> {
	unsafe {
		let logical_name = executable::logical_name(artifact)?;
		// Ask the development registry first. It answers with a generation of exactly this
		// artifact or with nothing, and the name it is asked for is the manifest-resolved one -
		// so a registry generation can shadow a declared artifact and can shadow nothing else.
		// Everything after this point is identical either way: the image is verified against
		// its own identity record and its providers are checked against what that record names,
		// so a shadowing generation earns its launch by the same rules the installed one does.
		//
		// No compatibility rule applies to an executable, and none is missing: that rule exists
		// because a running process has already resolved against a provider, and nothing has
		// resolved against a program that has not started yet.
		if let Some(shadow) = registry_generation(&mut registry, logical_name) {
			return spawn_program_bytes(storage, registry, &shadow, Some(logical_name), bootstrap, domain);
		}
		let main = MappedFile::open(storage, String::from(path))?;
		spawn_program_bytes(storage, registry, main.bytes(), Some(logical_name), bootstrap, domain)
	}
}

// Ask the development registry for a generation of `artifact`. None when there is no
// registry, when it holds nothing for that name, or when it does not answer - all of which
// mean the installed artifact is what gets loaded.
//
// The handle is taken by reference and zeroed when the registry does not answer, which ends
// the questioning for the rest of this resolution. One launch reaches this once for the
// program and once per provider in its closure, so an agent that has gone quiet would
// otherwise cost the answer timeout on every one of them. Only this launch's copy is
// zeroed - the silence may be nothing more than an agent busy inside another call.
// How long a launch waits for the registry to answer, in scheduler ticks (100 Hz).
const REGISTRY_ANSWER_TICKS: u64 = 100;

unsafe fn registry_generation(registry: &mut u64, artifact: &str) -> Option<Vec<u8>> {
	unsafe {
		let handle: u64 = *registry;
		if handle == 0 || artifact.len() > 64 {
			return None;
		}
		// Drop anything already queued before asking. A query whose deadline expired leaves its
		// reply to arrive later, and reading that as the answer to the NEXT query puts this
		// channel permanently one answer behind - every launch then loads what the registry
		// held at the previous launch, which is worse than not resolving at all because it
		// looks like it worked.
		while channel_peek(handle) >= 0 {
			match recv_vec_blocking(handle) {
				ReceivedVec::Closed | ReceivedVec::Failed => {
					*registry = 0;
					return None;
				}
				ReceivedVec::Message { .. } => {}
			}
		}
		// Never block on this send. The registry is a development convenience whose whole
		// contract is that an unanswered end costs a launch nothing, and `send_blocking` breaks
		// that contract the moment the queue fills: an agent that takes queries and answers none
		// stops being ignorable and starts stopping the boot, one launch at a time, until
		// ProcessService is wedged and every service after it never starts. Measured before this
		// was written: on an aarch64 development boot the 42nd query blocked forever and the
		// chain ended at DisplayService, with nothing anywhere saying why.
		//
		// A refused send means the agent is not keeping up, which is the same answer as no
		// generation - so the launch reads the volume, which is what it would have done anyway.
		if !try_send(handle, artifact.as_bytes(), 0) {
			return None;
		}
		// Look before waiting. The agent can answer before this ever reaches the wait, and a
		// wait that is asked to sleep until something arrives has nothing left to wake it when
		// it already has - so a fast answer would time out while sitting in the queue. Bounded
		// even so: an agent that died between announcing itself and this query must not take
		// every later launch down with it.
		let limit: u64 = clock() + REGISTRY_ANSWER_TICKS;
		loop {
			if channel_peek(handle) >= 0 {
				return match recv_vec_blocking(handle) {
					// A replacement agent announcing itself, which can land in the middle of a
					// query because an agent can be restarted at any moment. It is not an answer
					// and must not be read as one: a five-byte image would fail to parse and take
					// the launch down with it. Keep waiting for the real reply instead.
					ReceivedVec::Message { bytes, .. } if bytes == REGISTRY_ANNOUNCEMENT => continue,
					ReceivedVec::Message { bytes, .. } if !bytes.is_empty() => Some(bytes),
					ReceivedVec::Message { .. } => None,
					// Either way there is no answer coming on this channel.
					ReceivedVec::Closed | ReceivedVec::Failed => {
						*registry = 0;
						None
					}
				};
			}
			if clock() >= limit || wait(handle, limit) < 0 {
				*registry = 0;
				return None;
			}
		}
	}
}

unsafe fn spawn_program_bytes(storage: u64, registry: u64, bytes: &[u8], expected_identity: Option<&str>, bootstrap: u64, domain: u64) -> Option<Spawned> {
	unsafe {
		let Some(elf) = bootproto::elf::Elf::parse(bytes) else { return None };
		let Some(dynamic) = elf.dynamic_info() else { return None };
		let Some(dynamic) = dynamic else {
			if expected_identity.is_none() {
				let (process, thread) = spawn_prepared_in(bytes, bootstrap, domain)?;
				return Some(Spawned { process, thread });
			}
			return None;
		};
		let Some(artifact) = expected_identity else { return None };
		let Some(identity) = verify_identity(&elf, "executable", artifact) else { return None };
		let Some(dependencies) = dependencies(&elf, &dynamic) else { return None };
		if dependencies.is_empty() {
			if !identity_matches_dependencies(&identity, &dependencies, &[]) {
				return None;
			}
			let (process, thread) = spawn_prepared_in(bytes, bootstrap, 0)?;
			return Some(Spawned { process, thread });
		}
		if storage == 0 {
			return None;
		}
		let mut resolver = Resolver { storage, registry, modules: Vec::new(), visiting: Vec::new() };
		for dependency in &dependencies {
			if !resolver.collect(dependency, 0) {
				return None;
			}
		}
		if !identity_matches_dependencies(&identity, &dependencies, &resolver.modules) {
			return None;
		}
		let Some(order) = resolver.order() else { return None };
		let process = process_create(domain);
		if process < 0 {
			return None;
		}
		let process = process as u64;
		if !resolver.load(process, &order) {
			close(process);
			return None;
		}
		let entry = process_load_main(process, bytes);
		if entry < 0 {
			close(process);
			return None;
		}
		// Prepare rather than start. The decision to run belongs to the caller, because a
		// pipeline must have every stage loaded and wired before ANY of them executes - an
		// early stage that ran now would write into a consumer with no reader yet. Every
		// ordinary launch releases immediately and is unchanged; only a prepared launch holds
		// the token back.
		let thread = process_prepare(process, entry as u64, bootstrap);
		if thread < 0 {
			close(process);
			return None;
		}
		Some(Spawned { process, thread: thread as u64 })
	}
}

// A process that has been loaded but has not run: the process itself, and the token that
// starts it. The spawn path returns both because both are results of the same operation -
// carrying the token out of band would leave the two able to disagree about which launch they
// describe.
struct Spawned {
	process: u64,
	// The prepared first thread. Releasing it runs the program; dropping it leaves a process
	// that never does, which is what an abandoned transaction should leave behind.
	thread: u64,
}

impl Spawned {
	// Run it now. Every ordinary launch does this immediately; only a prepared launch holds
	// the token back.
	unsafe fn release(self) -> bool {
		unsafe { process_release(self.thread) >= 0 }
	}

	// Abandon it: close both handles, leaving nothing running and nothing leaked.
	unsafe fn abandon(self) {
		unsafe {
			close(self.thread);
			close(self.process);
		}
	}
}

impl Processes<'_> {
	// Abandon every prepared launch belonging to `client`. Called when its channel closes.
	fn abandon_prepared_of(&mut self, client: u64) {
		let mut index = 0usize;
		while index < self.prepared.len() {
			if self.prepared[index].0 == client {
				let (_, koid, spawned) = self.prepared.remove(index);
				unsafe { spawned.abandon() };
				self.forget(koid);
			} else {
				index += 1;
			}
		}
	}

	// Drop this service's OWN record of a launch that never ran.
	//
	// `record` keeps a read duplicate of the process so `list` can report it and `reap` can tell
	// when it ends. For a cancelled launch that duplicate is the last thing holding the kernel
	// object alive - closing the caller's handle and the start token frees neither the address
	// space nor the frames while this one lives. A launch that never started has nothing to report
	// and nothing to reap, so the entry goes with it (IDL-001).
	fn forget(&mut self, koid: u64) {
		let mut index = 0usize;
		while index < self.started.len() {
			if self.started[index].info.koid == koid {
				let entry = self.started.remove(index);
				unsafe {
					if entry.handle != 0 {
						close(entry.handle);
					}
					if entry.domain != 0 {
						close(entry.domain);
					}
				}
			} else {
				index += 1;
			}
		}
	}
}

impl<'a> Service for Processes<'a> {
	fn start(&mut self, name: String) -> Result<ProcessInfo, Error> {
		// spawn with no bootstrap capability (phase 1: started processes run
		// unattended), then read back the new process's koid and record it. Unlike `launch`
		// nothing else wants this handle, so it is recorded directly rather than duplicated.
		let (spawned, artifact) = unsafe { self.spawn_program(&name, 0, 0) }.ok_or(Error::NotFound)?;
		let process = spawned.process;
		unsafe { spawned.release() };
		let koid: u64 = unsafe { object_info(process) }.map(|i| i.koid).ok_or(Error::Again)?;
		let info: ProcessInfo = ProcessInfo { koid, name: artifact };
		self.record(info.clone(), process, 0);
		Ok(info)
	}

	fn list(&mut self) -> Result<Vec<ProcessInfo>, Error> {
		self.reap();
		Ok(self.started.iter().map(|p| p.info.clone()).collect())
	}

	// What this service is currently accounting: one budget per live launch that runs in a
	// Domain of its own, named after the program so a reader can tell which run it is. A
	// launch without a stated memory limit runs in the caller's Domain and has nothing of its
	// own to report, so it is absent rather than listed with somebody else's numbers.
	//
	// Values, never handles. Sending the Domain itself would make the receiver its last
	// holder and keep it alive past the process it accounts, which is exactly the lifetime
	// the Domain-per-launch change removed.
	fn accounting(&mut self) -> Result<Vec<Budget>, Error> {
		self.reap();
		let mut budgets: Vec<Budget> = Vec::new();
		for entry in &self.started {
			if entry.domain == 0 {
				continue;
			}
			let Some(stats) = (unsafe { domain_stats(entry.domain) }) else { continue };
			budgets.push(Budget {
				name: entry.info.name.clone(),
				usage: alloc::vec![
					ResourceUsage { r#type: ResourceType::Memory, used: stats.memory_used, limit: stats.memory_limit },
					ResourceUsage { r#type: ResourceType::Handles, used: stats.handles_used, limit: stats.handles_limit },
					ResourceUsage { r#type: ResourceType::Threads, used: stats.threads_used, limit: stats.threads_limit },
					ResourceUsage { r#type: ResourceType::IpcQueue, used: stats.ipc_used, limit: stats.ipc_limit },
					ResourceUsage { r#type: ResourceType::Dma, used: stats.dma_used, limit: stats.dma_limit },
					ResourceUsage { r#type: ResourceType::Stack, used: stats.stack_used, limit: stats.stack_limit },
				],
			});
		}
		Ok(budgets)
	}

	fn launch(&mut self, name: String, bootstrap: u64) -> Result<StartResult, Error> {
		// spawn with the caller-provided bootstrap channel (the policy front end's end of
		// the new process's bootstrap), then read back the new process's koid. The live
		// process handle is handed back to the caller for job control - so unlike `start`
		// we do not close it here; it is transferred out as the reply's handle.
		let (spawned, artifact) = unsafe { self.spawn_program(&name, bootstrap, 0) }.ok_or(Error::NotFound)?;
		let process = spawned.process;
		unsafe { spawned.release() };
		let koid: u64 = unsafe { object_info(process) }.map(|i| i.koid).ok_or(Error::Again)?;
		let info: ProcessInfo = ProcessInfo { koid, name: artifact };
		let observer: i64 = unsafe { duplicate(process, RIGHT_READ) };
		self.record(info.clone(), if observer > 0 { observer as u64 } else { 0 }, 0);
		Ok(StartResult { task: process, info })
	}

	// Load a program and leave it stopped. Identical to `launch` except that the process has
	// not begun: the start token stays here and `release` queues it. This is what lets a
	// pipeline exist whole before it runs - `a | b` needs b's reader installed before a writes.
	fn launch_prepared(&mut self, name: String, bootstrap: u64) -> Result<StartResult, Error> {
		let (spawned, artifact) = unsafe { self.spawn_program(&name, bootstrap, 0) }.ok_or(Error::NotFound)?;
		let Some(koid) = (unsafe { object_info(spawned.process) }).map(|i| i.koid) else {
			// A prepared launch nobody can identify could never be released - a process
			// stopped forever. Abandoning it closes both handles, so nothing runs and nothing
			// leaks.
			unsafe { spawned.abandon() };
			return Err(Error::Again);
		};
		let info = ProcessInfo { koid, name: artifact };
		let observer: i64 = unsafe { duplicate(spawned.process, RIGHT_READ) };
		self.record(info.clone(), if observer > 0 { observer as u64 } else { 0 }, 0);
		let task = spawned.process;
		self.prepared.push((self.client, koid, spawned));
		Ok(StartResult { task, info })
	}

	// Start a launch prepared earlier. Returns false when that koid has no pending launch -
	// already released, never prepared, or reaped - which a caller building a pipeline needs
	// to distinguish from an error, because releasing twice is a bug in the caller and not a
	// condition the system should hide.
	fn release(&mut self, koid: u64) -> Result<bool, Error> {
		// ITS OWN CLIENT'S, or nobody's. A koid names a prepared launch and proves nothing about who
		// prepared it; matching the owner too is what stops one client starting another's program.
		// A client that names a koid it does not own gets the same answer as one naming a koid that
		// does not exist - the truth from where it stands, and no evidence that the koid is real.
		let Some(index) = self.prepared.iter().position(|(owner, pending, _)| *pending == koid && *owner == self.client) else {
			return Ok(false);
		};
		let (_, _, spawned) = self.prepared.remove(index);
		Ok(unsafe { spawned.release() })
	}

	// The other end of `release`: drop a prepared launch and tear the loaded process down.
	//
	// Without this the only ways out of a prepared launch were to start it or to leave it, and
	// leaving it means a process that is loaded, stopped, holding its Domain and its bootstrap
	// channel, forever - which is what PermissionManager's early returns produced whenever a
	// pipeline failed to assemble (IDL-001). `false` is "no such prepared launch of yours", the
	// same answer `release` gives and for the same reason.
	fn cancel(&mut self, koid: u64) -> Result<bool, Error> {
		let Some(index) = self.prepared.iter().position(|(owner, pending, _)| *pending == koid && *owner == self.client) else {
			return Ok(false);
		};
		let (_, _, spawned) = self.prepared.remove(index);
		unsafe { spawned.abandon() };
		self.forget(koid);
		Ok(true)
	}

	// Start a whole prepared pipeline, or none of it.
	//
	// EVERY TOKEN IS CHECKED BEFORE ANY THREAD IS QUEUED, and that ordering is the entire primitive.
	// `security.run-pipeline` promises that a failure at any stage starts none of them, and a caller
	// releasing one koid at a time cannot keep it: past the first release a stage may already have
	// written, sent or printed, so telling it to stop is not the same as it never having run. The
	// broker had no way to ask for the group as a unit, which is why the promise was false exactly
	// when it mattered.
	//
	// A koid this client does not own, one that was never prepared, one already released, and the
	// same koid named twice all answer `false` with nothing started - the same answer `release`
	// gives for a koid it cannot use, and for the same reason: a caller learns that its request was
	// refused and nothing about whose koid it named.
	fn release_group(&mut self, koids: Vec<u64>) -> Result<bool, Error> {
		if koids.is_empty() {
			return Ok(false);
		}
		let mut indices: Vec<usize> = Vec::new();
		for koid in &koids {
			let Some(index) = self.prepared.iter().position(|(owner, pending, _)| pending == koid && *owner == self.client) else {
				return Ok(false);
			};
			// The same koid twice would resolve to one prepared launch and release it once while the
			// caller believes two stages started.
			if indices.contains(&index) {
				return Ok(false);
			}
			indices.push(index);
		}
		// Taken high index first so the ones not yet removed keep their positions.
		indices.sort_unstable();
		let mut ready: Vec<Spawned> = Vec::new();
		for index in indices.iter().rev() {
			let (_, _, spawned) = self.prepared.remove(*index);
			ready.push(spawned);
		}
		// PAST HERE IT IS NOT A POLICY DECISION ANY MORE. Queueing a thread that already exists is a
		// local operation; if one refuses, the group is half-started and no answer this returns can
		// make that untrue, so it is reported as a fault rather than as a refusal a caller could
		// mistake for "nothing happened".
		let mut queued = true;
		for spawned in ready {
			queued &= unsafe { spawned.release() };
		}
		if queued { Ok(true) } else { Err(Error::Invalid) }
	}

	fn launch_bounded(&mut self, name: String, memory_limit: u64, bootstrap: u64) -> Result<StartResult, Error> {
		let domain = unsafe { self.bounded_domain(memory_limit)? };
		let started = unsafe { self.spawn_program(&name, bootstrap, domain) };
		// The Domain was made for this one process, and the handle is kept only so
		// `accounting` can read its counters. A process holds its own Domain, so the kernel
		// frees it when the process ends whatever this service does; what the handle changes
		// is that the freeing waits for the next reap, which is the same bargain the process
		// handle already makes. A launch that never started keeps nothing.
		let Some((spawned, artifact)) = started else {
			unsafe { close(domain) };
			return Err(Error::NotFound);
		};
		let process = spawned.process;
		unsafe { spawned.release() };
		let koid = unsafe { object_info(process) }.map(|info| info.koid).ok_or(Error::Again)?;
		let info = ProcessInfo { koid, name: artifact };
		let observer: i64 = unsafe { duplicate(process, RIGHT_READ) };
		self.record(info.clone(), if observer > 0 { observer as u64 } else { 0 }, domain);
		Ok(StartResult { task: process, info })
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

	// 2b. receive the development registry channel. Handed over even when nothing will ever
	//     answer on it, so this service never has to learn about a capability arriving after
	//     it started serving; an unanswered end simply means every launch reads the volume.
	let registry: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"REGISTRY") }.unwrap_or(0);

	// 3. wait for the serve channel clients reach us on.
	let service: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"SERVE") }.unwrap_or_else(|| unsafe { fail_bootstrap(bootstrap, b"serve", b"missing serve channel") });

	// 4. report in to the supervisor that started us.
	unsafe {
		send_blocking(bootstrap, b"ProcessService: online", 0);
	}

	// 5. serve generated start/list requests until the client side closes.
	let mut procs: Processes = Processes { package, storage, registry, registry_armed: false, started: Vec::new(), prepared: Vec::new(), client: 0 };
	let mut request: [u8; 256] = [0u8; 256];
	let mut reply: [u8; 4096] = [0u8; 4096];
	unsafe {
		serve_multi(service, &mut request, &mut reply, |chan, req, handle, out, reply_handle| -> Option<usize> {
			// The client's identity, carried into the dispatch that cannot take an extra argument:
			// the generated trait's shape is fixed, and the serve loop is the only place that knows
			// which channel a request arrived on.
			procs.client = chan;
			// A CLIENT THAT SIMPLY GOES (IDL-001). `serve_multi` synthesises this when a client's
			// channel closes; everything that client prepared and never released is abandoned here,
			// which is the difference between a transaction that was dropped and a process loaded,
			// stopped and holding its Domain for the life of the system.
			if req.len() == 2 && u16::from_le_bytes([req[0], req[1]]) == abi::DISCONNECT_OP {
				procs.abandon_prepared_of(chan);
				return None;
			}
			process::dispatch(&mut procs, req, handle, out, reply_handle)
		});
	}
	exit();
}
