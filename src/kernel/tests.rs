// The kernel test suite and its scenario helpers (custom_test_frameworks, runs
// under `cargo test` in QEMU). Everything here is test-only: the ring-3 probe
// programs and their thread bodies, the packaged-scenario drivers the service
// tests build on, the Testable harness, and the test cases themselves. The boot
// path and the helpers it shares with the suite (the module locators, the
// SystemManager spawn and the supervise ladder) stay in main.rs.

use super::*;
use alloc::vec::Vec;

#[path = "test_suites/applications.rs"]
mod applications;
#[path = "test_suites/boot.rs"]
mod boot;
#[path = "test_suites/dynamic.rs"]
mod dynamic;
#[path = "test_suites/hardware.rs"]
mod hardware;
#[path = "test_suites/kernel.rs"]
mod kernel;
#[path = "test_suites/services.rs"]
mod services;
#[path = "test_suites/volume_layout.rs"]
mod volume_layout;

// Userspace (ring 3) page layout for the test: one USER page for the program,
// one for its stack, mapped into the low half of the shared address space
// (per-process page tables / CR3 isolation are a later refinement).
use crate::memlayout::{USER_CODE_VA, USER_STACK_VA};

include!(concat!(env!("OUT_DIR"), "/library_paths.rs"));

// Kernel-thread body that runs a ring-3 program. It maps a USER code and stack
// page, copies the embedded position-independent program in, and drops to ring 3
// with its bootstrap Channel handle. The program makes a capability-gated channel
// send and a debug-write, then exits back here, where we tear the mapping down.
extern "C" fn user_thread_body(handle: u64) {
	use mem::frame::{self, PAGE_SIZE};
	let code = frame::allocate().expect("user code frame");
	let stack = frame::allocate().expect("user stack frame");
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
	arch::paging::map_page(USER_CODE_VA, code, flags);
	arch::paging::map_page(USER_STACK_VA, stack, flags | arch::paging::NO_EXECUTE);
	let program = arch::usermode::program_bytes();
	unsafe {
		arch::paging::copy_to_user_page(USER_CODE_VA, program);
		arch::usermode::enter(USER_CODE_VA, USER_STACK_VA + PAGE_SIZE, handle);
	}
	arch::paging::unmap_page(USER_CODE_VA);
	arch::paging::unmap_page(USER_STACK_VA);
	unsafe { frame::deallocate(code) };
	unsafe { frame::deallocate(stack) };
}

// Load the volume archive bytes and the parsed init package - the 'static modules
// every userspace scenario starts from.
fn scenario_packages() -> Result<(&'static [u8], pkg::Package<'static>), &'static str> {
	let volume = volume_package_bytes().ok_or("volume package module not found")?;
	let init = init_package_bytes().ok_or("init package module not found")?;
	let package = pkg::Package::parse(init).ok_or("init package is malformed")?;
	Ok((volume, package))
}

// Look up `name` in the volume archive and return a copy of its bytes - the file a
// scenario expects the component/client to read back.
fn volume_file(volume: &[u8], name: &[u8]) -> Result<alloc::vec::Vec<u8>, &'static str> {
	pkg::Package::parse(volume).and_then(|p| p.lookup(name).map(|b| b.to_vec())).ok_or("file missing from the volume package")
}

// Resolve a program's ELF for a test. The pinned bootstrap programs live in the init
// package; every other program resolves through the manifest-declared system-volume path.
// The returned slice borrows
// the 'static module data, so it outlives the temporary volume Package.
// Time the harness can move.
//
// Added to every architecture's tick counter in a test build. Nothing in the system distinguishes
// it from time passing - the scheduler's timed waiters are compared against the same reading - so a
// test can reach a deadline deliberately instead of waiting it out.
static CLOCK_SKEW: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub(crate) fn clock_skew() -> u64 {
	CLOCK_SKEW.load(core::sync::atomic::Ordering::Relaxed)
}

// Push the guest's notion of time forward. Waiters due after the jump are woken by the scheduler's
// deadline check, which runs on every pass of `run_until_idle` - so a pump after this is what
// actually delivers the wake.
pub(crate) fn advance_clock(ticks: u64) {
	CLOCK_SKEW.fetch_add(ticks, core::sync::atomic::Ordering::Relaxed);
}

fn program_elf(package: &pkg::Package<'static>, volume: &'static [u8], name: &[u8]) -> Option<&'static [u8]> {
	let mut artifact: alloc::vec::Vec<u8> = name.to_vec();
	artifact.extend_from_slice(b".lsexe");
	if let Some(elf) = package.lookup(&artifact) {
		return Some(elf);
	}
	let name = core::str::from_utf8(name).ok()?;
	let path = test_program_path(name)?;
	pkg::Package::parse(volume).and_then(|p| p.lookup(path.as_bytes()))
}

// Send a tagged capability over a bootstrap channel: wrap `object` in a Capability
// carrying `rights` and send it with `payload` as the message bytes. The shared
// "hand a process one of its initial capabilities" step the scenarios repeat.
// Encode a launch context the way `base_proto`'s generated codec does: the argument string, the
// working directory, and an environment count, each little-endian length-prefixed.
//
// Hand-built because the kernel links no userspace protocol crate, which makes this the one place
// in the tree that restates the record's shape. It is written once, here, rather than at each of
// the thirteen stagings that need it - and if it ever disagrees with the schema, every governed
// tool test fails at once rather than one of them subtly.
pub(crate) fn launch_context(arguments: &[u8], cwd: &[u8]) -> alloc::vec::Vec<u8> {
	let mut out: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	for field in [arguments, cwd] {
		out.extend_from_slice(&(field.len() as u16).to_le_bytes());
		out.extend_from_slice(field);
	}
	out.extend_from_slice(&0u16.to_le_bytes()); // no environment variables
	out
}

fn send_cap(channel: &object::channel::Channel, payload: &[u8], object: alloc::sync::Arc<dyn object::KernelObject>, rights: object::rights::Rights) -> Result<(), &'static str> {
	let cap = object::handle::Capability::new(object, rights, 0);
	channel.send(object::channel::Message::new(payload.to_vec(), alloc::vec![cap], 0)).map_err(|_| "bootstrap capability send failed")
}

// Create a ramdisk MemoryObject from `volume`, fill it, and hand it to a service's
// bootstrap channel as "RAMDISK" + the volume's byte length, with a read+map cap.
fn send_ramdisk(channel: &object::channel::Channel, volume: &[u8]) -> Result<(), &'static str> {
	use object::rights::Rights;
	let ramdisk = object::memory_object::MemoryObject::create(volume.len()).ok_or("no memory for the ramdisk")?;
	copy_into_object(&ramdisk, volume);
	let mut msg = alloc::vec::Vec::with_capacity(7 + 8);
	msg.extend_from_slice(b"RAMDISK");
	msg.extend_from_slice(&(volume.len() as u64).to_le_bytes());
	send_cap(channel, &msg, ramdisk, Rights::READ | Rights::MAP)
}

// Create a MemoryObject from `archive`, fill it, and hand it to a process's bootstrap
// channel as "PACKAGE" + the archive's byte length, with a read+map+transfer cap - the
// rt recv_package handshake. A launcher (e.g. PermissionManager) maps it and spawns the
// programs it governs from it.
fn send_package(channel: &object::channel::Channel, archive: &[u8]) -> Result<(), &'static str> {
	use object::rights::Rights;
	let object = object::memory_object::MemoryObject::create(archive.len()).ok_or("no memory for the package")?;
	copy_into_object(&object, archive);
	let mut msg = alloc::vec::Vec::with_capacity(7 + 8);
	msg.extend_from_slice(b"PACKAGE");
	msg.extend_from_slice(&(archive.len() as u64).to_le_bytes());
	send_cap(channel, &msg, object, Rights::READ | Rights::MAP | Rights::TRANSFER)
}

// Build the storage topology and run it to completion. A MemoryObject holds
// the ramdisk volume; the StorageService process maps it and serves files over a
// service channel; a client process opens vol://system/hello.txt through the
// service, receives a shared-buffer capability to the file's bytes, maps it, and
// reports the contents back over its bootstrap channel. The kernel only brokers
// the initial capabilities - the open, the resolve, and the zero-copy read all
// happen in userspace. Returns (expected, actual): the file straight from the
// volume archive, and the bytes the client read through the service.
fn run_storage_scenario() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>), &'static str> {
	use object::channel::Channel;
	use object::rights::Rights;

	// the volume archive backing the ramdisk, the file we expect served, and the
	// userspace programs from the init package
	let (volume, package) = scenario_packages()?;
	let expected = volume_file(volume, b"hello.txt")?;
	let service_elf = package.lookup(b"storage_service.lsexe").ok_or("storage_service.lsexe missing from the init package")?;
	let client_elf = program_elf(&package, volume, b"storage_client").ok_or("storage_client missing from the package or volume")?;

	// channels: a bootstrap per process, plus the service<->client request channel
	let (service_boot_kernel, service_boot_user) = Channel::create();
	let (client_boot_kernel, client_boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();

	// spawn the two processes with their bootstrap endpoints
	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), service_elf, service_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	let _client = spawn_dynamic_test_process(domain, client_elf, client_boot_user);

	// hand the service its ramdisk (with the volume length) and its service
	// endpoint, then hand the client the other end of that service channel.
	send_ramdisk(&service_boot_kernel, volume)?;
	send_cap(&service_boot_kernel, b"SERVE", service_server, Rights::ALL)?;
	send_cap(&client_boot_kernel, b"CONNECT", service_client, Rights::ALL)?;

	// run the cooperative schedule until everyone is done, then read the result
	sched::run_until_idle();
	let result = client_boot_kernel.recv().map_err(|_| "the client reported no result")?;
	Ok((expected, result.bytes))
}

// Build the WASI topology and run it to completion. A StorageService serves the
// ramdisk volume; the wasi_host process loads the embedded Wasm component and runs
// it, and the component's only import (`liber.read`) is wired by the host to read
// the granted file vol://system/hello.txt through StorageService into the
// component's linear memory. The component has no other capability - no ambient
// authority. The host reports the bytes the component read back over its bootstrap
// channel. The kernel only brokers the initial capabilities.
//
// Returns (expected, STATUS, actual): the file straight from the volume, the status word the host
// sends ahead of the payload, and the bytes the component read. The comment here said `(expected,
// actual)` for as long as the third element has existed - which is the exact confusion the status
// was added to remove, restated in the comment above the function that removes it.
fn run_wasi_scenario() -> Result<(alloc::vec::Vec<u8>, i32, alloc::vec::Vec<u8>), &'static str> {
	let (volume, _) = scenario_packages()?;
	let expected = volume_file(volume, b"hello.txt")?;
	let run = run_wasi_scenario_on(None)?;
	Ok((expected, run.status, run.payload))
}

// What the host's report says: the status word and, when there is one, the payload after it.
//
// ONE PARSER, because the protocol has three shapes and only one of them had a test. The status is
// four little-endian bytes carrying what the component's `run` answered - a byte count when
// non-negative, one of the world's negative statuses when not - and the payload is exactly that
// many bytes of the component's memory. A refusal and a successful read of an empty file both
// produce an empty payload and are told apart ONLY by the status, which is the whole reason the
// status exists; parsing it in each test separately is how one of them comes to check the count
// against the payload length and the others do not.
pub(crate) struct WasiRun {
	pub(crate) status: i32,
	pub(crate) payload: alloc::vec::Vec<u8>,
}

fn parse_wasi_report(bytes: &[u8]) -> Result<WasiRun, &'static str> {
	if bytes.len() < 4 {
		return Err("the host's report carries a status");
	}
	let status = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
	let payload = bytes[4..].to_vec();
	// The two halves have to AGREE, and nothing checked that they did. A negative status carries no
	// payload because there is nothing the component read; a non-negative one is the count of the
	// bytes that follow. A report whose two halves disagree is a host contradicting itself, and
	// reading the payload past that point is reading whatever the mismatch left behind.
	if status < 0 && !payload.is_empty() {
		return Err("a refusal carries no payload, and this one does");
	}
	if status >= 0 && payload.len() != status as usize {
		return Err("the report's status is the length of its payload, and these disagree");
	}
	Ok(WasiRun { status, payload })
}

// The scenario, with the volume StorageService serves chosen by the caller.
//
// `None` is the real system volume - what the ordinary test runs against. `Some(archive)` hands
// StorageService a volume built for the occasion, which is how the report protocol's other two
// shapes are reachable at all: an empty `hello.txt` for the "status 0 with an empty payload" case,
// and a volume without it for the refusal. The PROGRAMS still come out of the real volume, because
// what is being varied is the data the host is granted and not the system it runs on.
fn run_wasi_scenario_on(serve: Option<&[u8]>) -> Result<WasiRun, &'static str> {
	use object::channel::Channel;
	use object::rights::Rights;

	let (volume, package) = scenario_packages()?;
	let serve = serve.unwrap_or(volume);
	let storage_elf = package.lookup(b"storage_service.lsexe").ok_or("storage_service.lsexe missing from the init package")?;
	let host_elf = program_elf(&package, volume, b"wasi_host").ok_or("wasi_host missing from the package or volume")?;

	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (host_boot_kernel, host_boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();

	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	let _host = spawn_dynamic_test_process(domain, host_elf, host_boot_user);

	// storage bootstrap: the ramdisk volume and its service channel; the host gets
	// only the StorageService client - the one capability it is granted.
	send_ramdisk(&storage_boot_kernel, serve)?;
	send_cap(&storage_boot_kernel, b"SERVE", service_server, Rights::ALL)?;
	send_cap(&host_boot_kernel, b"STORAGE", service_client, Rights::ALL)?;

	sched::run_until_idle();
	let result = host_boot_kernel.recv().map_err(|_| "the host reported no result")?;
	parse_wasi_report(&result.bytes)
}

// The same scenario against a volume built for the occasion, so the report protocol's other two
// shapes are reachable.
//
// `run_wasi_scenario` has always run against the real system volume, where `hello.txt` exists and
// is non-empty - which is exactly one of the three answers the protocol can give. The two it could
// not reach are the two the status was ADDED for: a legitimate zero (an empty file) and a negative
// status (a refusal), which produce identical payloads and are told apart only by the status word.
//
// `abi::bootstrap::build_package` is the archive writer this crate already carries, so a volume can
// be assembled here without a build change. The PROGRAMS still come from the real volume; what is
// varied is only the data StorageService serves.
pub(crate) fn run_wasi_scenario_over(files: &[(&[u8], &[u8])]) -> Result<WasiRun, &'static str> {
	let archive = abi::bootstrap::build_package(files).ok_or("the fixture archive could not be built")?;
	run_wasi_scenario_on(Some(&archive))
}

// Build the powerbox topology and run it to completion. A StorageService serves
// the ramdisk volume; a file_picker holds the trusted storage client and serves the
// Picker contract; the wasi_host is given ONLY a picker client - no filesystem
// access of its own - and runs the same Wasm component. The component's read import
// now goes through the picker: `pick` (standing in for the user's choice) opens the
// chosen file (motd.txt) over StorageService and hands back that one file as a
// handle<file> capability, which the host reads into the component's memory. So a
// component with no filesystem capability reaches exactly the user-picked file and
// nothing else. The kernel only brokers the initial capabilities. Returns
// (expected, actual): the picked file straight from the volume, and what the
// component read.
fn run_powerbox_scenario() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>), &'static str> {
	use object::channel::Channel;
	use object::rights::Rights;

	let (volume, package) = scenario_packages()?;
	let expected = volume_file(volume, b"motd.txt")?;
	let storage_elf = package.lookup(b"storage_service.lsexe").ok_or("storage_service.lsexe missing from the init package")?;
	let picker_elf = program_elf(&package, volume, b"file_picker").ok_or("file_picker missing from the package or volume")?;
	let host_elf = program_elf(&package, volume, b"wasi_host").ok_or("wasi_host missing from the package or volume")?;

	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (picker_boot_kernel, picker_boot_user) = Channel::create();
	let (host_boot_kernel, host_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	let (picker_server, picker_client) = Channel::create();

	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	let _picker = spawn_dynamic_test_process(domain.clone(), picker_elf, picker_boot_user);
	let _host = spawn_dynamic_test_process(domain, host_elf, host_boot_user);

	// StorageService: the ramdisk volume and its service channel. file_picker: the
	// trusted StorageService client and its own service channel. wasi_host: only the
	// picker client - no filesystem access of its own.
	send_ramdisk(&storage_boot_kernel, volume)?;
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL)?;
	send_cap(&picker_boot_kernel, b"STORAGE", storage_client, Rights::ALL)?;
	send_cap(&picker_boot_kernel, b"SERVE", picker_server, Rights::ALL)?;
	send_cap(&host_boot_kernel, b"PICKER", picker_client, Rights::ALL)?;

	sched::run_until_idle();
	let result = host_boot_kernel.recv().map_err(|_| "the host reported no result")?;
	// The same report shape as the scenario above: status first, payload after.
	if result.bytes.len() < 4 {
		return Err("the host's report carries a status");
	}
	Ok((expected, result.bytes[4..].to_vec()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PermissionScenario {
	Probes,
	GovernedTools,
	ScopedGrants,
}

struct PermissionScenarioResult {
	// What the last stage of a two-stage governed pipeline printed, and whether the broker
	// started it at all - the transaction's observable result.
	pipeline_read: alloc::vec::Vec<u8>,
	pipeline_started: bool,
	// What the terminal saw from a pipeline whose FIRST stage fails: its diagnostic must arrive
	// as itself, not relayed by the consumer, which is the difference between an error stream on
	// the terminal and one that empties into the pipe.
	diagnostic_read: alloc::vec::Vec<u8>,
	// What `redirect_in hello.txt | readln` printed. A redirection IS a pipeline stage in this
	// shell, so proving it is proving that the governed program opened the file with its own
	// `volumes` grant and pushed the bytes down an ordinary edge - and that the consumer never
	// touched storage at all.
	redirect_read: alloc::vec::Vec<u8>,
	// What `cat ::not-a-path 2>&1 | readln` printed - the diagnostic scenario with `merge-errors`
	// on the producer. The contrast with `diagnostic_read` is the whole assertion: same stages,
	// same failure, one flag, and the sentence changes which side of the pipe it comes out on.
	merged_read: alloc::vec::Vec<u8>,
	// What the three migrated-tool pipelines printed, in the order the scenario runs them:
	// `... | wc`, `... | head -n 1`, `... | tee <path> | wc`.
	stream_reads: alloc::vec::Vec<alloc::vec::Vec<u8>>,
	// What the REAL SHELL printed after one typed line - the layer every other field here skips.
	shell_read: alloc::vec::Vec<u8>,
	expected: alloc::vec::Vec<u8>,
	probe_read: alloc::vec::Vec<u8>,
	probe_summary: alloc::vec::Vec<u8>,
	date_read: alloc::vec::Vec<u8>,
	date_summary: alloc::vec::Vec<u8>,
	// What the P02M0101 command tools printed through the governed launch path: `pwd` from its
	// launch context alone, then `wc` and `head` over a volume file through the bounded read.
	command_read: alloc::vec::Vec<u8>,
	request_read: alloc::vec::Vec<u8>,
	request_summary: alloc::vec::Vec<u8>,
	cat_read: alloc::vec::Vec<u8>,
	ip_read: alloc::vec::Vec<u8>,
	ip_summary: alloc::vec::Vec<u8>,
	graphics_read: alloc::vec::Vec<u8>,
	graphics_start_ns: u64,
}

// Build the permission topology and run it to completion. A StorageService serves
// the ramdisk volume; a ProcessService is the loading mechanism; a TimeService serves the
// wall clock; the permission_manager (PermissionManager) is given the clients it may grant
// onward - a duplicable StorageService client, a duplicable (but dead-peer) LogService
// client, and a TimeService client - plus a NetworkService client it holds but is NOT to
// grant, a ProcessService client it drives to load components, and the channel its clients
// reach it on. PermissionManager governs components through ProcessService, each under a
// typed permission manifest. Two are report-back probes: sandbox_probe (granted storage and
// log but not network - it transfers exactly those two clients and withholds the network one)
// and request_probe (granted only log, which then asks for an undeclared capability - storage
// - at runtime), recording every decision. Three tools launch on demand through its `run`
// op, each printing to a captured stdout: `date` (granted
// only time) renders the wall clock, `cat` (granted only volumes) prints a file, and `ip`
// (granted only network) renders typed interface state over a fresh client. Each
// sandboxed component reaches only its granted capabilities: sandbox_probe reads its one
// granted file vol://system/hello.txt through the storage grant and reports the bytes back;
// `date` reads the wall clock through the time grant and prints the rendered instant to its
// captured stdout; request_probe's runtime request is refused by the headless policy default
// (least privilege - an undeclared capability is never granted) and recorded as a dynamic
// denial; and `cat` prints that file through its storage grant to the forwarded stdout. The
// scoped-grant scenario also launches `imgview` over a staged image and display/input
// stand-ins, proving its acquire -> present -> focus -> key-quit -> release sequence, then
// launches `play` through a playback-only audio grant. The kernel only brokers the initial
// capabilities.
fn run_permission_scenario(scenario: PermissionScenario) -> Result<PermissionScenarioResult, &'static str> {
	use object::channel::{Channel, Message};
	use object::memory_object::MemoryObject;
	use object::process::Process;
	use object::rights::Rights;

	let (volume, package) = scenario_packages()?;
	let init = init_package_bytes().ok_or("init package module not found")?;
	let expected = volume_file(volume, b"hello.txt")?;
	let storage_elf = package.lookup(b"storage_service.lsexe").ok_or("storage_service.lsexe missing from the init package")?;
	let process_elf = package.lookup(b"process_service.lsexe").ok_or("process_service.lsexe missing from the init package")?;
	let time_elf = program_elf(&package, volume, b"time_service").ok_or("time_service missing from the package or volume")?;
	let pm_elf = program_elf(&package, volume, b"permission_manager").ok_or("permission_manager missing from the package or volume")?;

	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (process_boot_kernel, process_boot_user) = Channel::create();
	let (time_boot_kernel, time_boot_user) = Channel::create();
	let (pm_boot_kernel, pm_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	let (process_server, process_client) = Channel::create();
	let (time_server, time_client) = Channel::create();
	let (perm_server, perm_client) = Channel::create();
	// The shell's own client to the manager. A handle to the same endpoint, which is only safe
	// because the shell runs at the very end of this scenario, after the test has finished asking
	// its own questions - two holders of one reply queue is the hazard `volume.connect` exists for.
	let perm_client_for_shell = perm_client.clone();
	// The manager's log grant: a real, duplicable client whose service peer is dropped, so
	// the sandboxed probe's best-effort log emit fails fast instead of blocking (no
	// LogService runs in this scenario). The capability is still granted and audited.
	let (log_server, log_client) = Channel::create();
	core::mem::drop(log_server);
	// The manager's network capability: held, but never granted to the probe.
	let (net_server, net_client) = Channel::create();
	// TimeService's own network client: a real, dead-peer client whose service peer is
	// dropped, so its best-effort SNTP discipline fails fast (PeerClosed) instead of
	// blocking on a reply that never comes (no NetworkService runs in this scenario). It
	// still serves the RTC-seeded wall clock to the governed `date` command.
	let (time_net_server, time_net_client) = Channel::create();
	core::mem::drop(time_net_server);
	// The manager's config / device / audio / resource capabilities: real, dead-peer clients (no
	// such services run in this scenario), held but never granted to the governed components here.
	let (config_server, config_client) = Channel::create();
	core::mem::drop(config_server);
	let (device_server, device_client) = Channel::create();
	core::mem::drop(device_server);
	let (audio_server, audio_client) = Channel::create();
	core::mem::drop(audio_server);
	let (resource_server, resource_client) = Channel::create();
	core::mem::drop(resource_server);
	// The manager's grantable process capability: a real, dead-peer ProcessService connection
	// (distinct from the live ProcessService it drives as the launch mechanism below), held but
	// never granted to the governed components here.
	let (process_grant_server, process_grant_client) = Channel::create();
	core::mem::drop(process_grant_server);
	// The manager's grantable supervisor capability: a real, dead-peer ServiceManager admin
	// channel, held but never granted to the governed components here (the `stop` command,
	// which would receive a narrowed copy, is not among them).
	let (supervisor_server, supervisor_client) = Channel::create();
	core::mem::drop(supervisor_server);
	// The manager's grantable volume capabilities: the four non-system volume StorageService
	// clients (media / iso / udf / usb) it bundles with the system storage client under the
	// `volumes` capability. Real, dead-peer clients here (no such services run in this scenario),
	// held but never granted to the governed components (the `lsvol` command is not among them).
	let (storage_media_server, storage_media_client) = Channel::create();
	core::mem::drop(storage_media_server);
	let (storage_iso_server, storage_iso_client) = Channel::create();
	core::mem::drop(storage_iso_server);
	let (storage_udf_server, storage_udf_client) = Channel::create();
	core::mem::drop(storage_udf_server);
	let (storage_usb_server, storage_usb_client) = Channel::create();
	core::mem::drop(storage_usb_server);
	// The manager's grantable services capability: a real, dead-peer ServiceManager status
	// channel, held but never granted to the governed components here (the `lssvc` command,
	// which would receive a narrowed copy, is not among them).
	let (services_server, services_client) = Channel::create();
	core::mem::drop(services_server);
	// The manager's grantable usb capability: a real, dead-peer xHCI bus query channel,
	// held but never granted to the governed components here (the `lsusb` command, which
	// would receive a narrowed copy, is not among them).
	let (usb_server, usb_client) = Channel::create();
	core::mem::drop(usb_server);
	let (display_admin_server, display_admin_client) = Channel::create();
	let (input_admin_server, input_admin_client) = Channel::create();
	let (audio_admin_server, audio_admin_client) = Channel::create();
	let (_display_scope_server, display_scope_client) = Channel::create();
	let (_input_scope_server, input_scope_client) = Channel::create();
	let (_audio_scope_server, audio_scope_client) = Channel::create();

	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	loader::spawn_elf_process(domain.clone(), process_elf, process_boot_user, Rights::ALL, 0).map_err(|_| "failed to load ProcessService")?;
	let _time = spawn_dynamic_test_process(domain.clone(), time_elf, time_boot_user);
	let _permission_manager = spawn_dynamic_test_process(domain, pm_elf, pm_boot_user);

	// StorageService: the ramdisk volume and its service channel.
	send_ramdisk(&storage_boot_kernel, volume)?;
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL)?;

	// ProcessService: the init package (the bring-up fallback) and its service channel,
	// plus a StorageService client so it loads the components PermissionManager governs
	// from the system volume's bin/ - the loading mechanism, kept separate
	// from the granting policy. The client is a duplicate of the manager's storage
	// connection; the cooperative schedule serializes the reads, so sharing it is safe.
	send_package(&process_boot_kernel, init)?;
	// The development registry channel. ProcessService receives it unconditionally, so it must be
	// sent even here where nothing will ever answer on it: an absent one blocks that service in
	// its bootstrap, and every launch after it waits on a reply that cannot come.
	let (registry_server, registry_client) = Channel::create();
	core::mem::drop(registry_server);
	send_cap(&process_boot_kernel, b"STORAGE", storage_client.clone(), Rights::ALL)?;
	send_cap(&process_boot_kernel, b"REGISTRY", registry_client, Rights::ALL)?;
	// A SECOND CLIENT FOR THE SHELL, taken here where the pair is made. The shell drives every
	// governed launch through PermissionManager and never through this, but its bootstrap requires
	// one: a shell without a process client fails before it prints a prompt.
	let process_client_for_shell = process_client.clone();
	send_cap(&process_boot_kernel, b"SERVE", process_server, Rights::ALL)?;

	// TimeService: its (dead-peer) network client and its service channel. It seeds its
	// wall clock from the RTC and serves it; the governed `date` command reads it through
	// the grant PermissionManager hands on.
	send_cap(&time_boot_kernel, b"NET", time_net_client, Rights::ALL)?;
	send_cap(&time_boot_kernel, b"SERVE", time_server, Rights::ALL)?;

	// PermissionManager: the grantable clients (storage + log, both duplicable, and time, plus
	// dead-peer config / device / audio / resource / process-grant / supervisor / media-iso-udf
	// storage it holds but does not grant here), a network client it withholds, the ProcessService
	// client it drives to load the components, and the channel its clients reach it on. The order
	// matches PermissionManager's receive order. (The grantable permission capability is not sent:
	// the manager mints that self-connection itself.)
	let shell_storage_client = storage_client.clone();
	send_cap(&pm_boot_kernel, b"STORAGE", storage_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"LOG", log_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"NETWORK", net_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"TIME", time_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"CONFIG", config_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"DEVICE", device_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"AUDIO", audio_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"DISPLAY_ADMIN", display_admin_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"INPUT_ADMIN", input_admin_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"AUDIO_ADMIN", audio_admin_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"RESOURCE", resource_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"PROCESS_GRANT", process_grant_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"SUPERVISOR", supervisor_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"STORAGE_MEDIA", storage_media_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"STORAGE_ISO", storage_iso_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"STORAGE_UDF", storage_udf_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"STORAGE_USB", storage_usb_client, Rights::ALL)?;
	// The two memory volumes. Sent even though this scenario mounts neither, though no longer
	// because it must be: PermissionManager now takes its capabilities by NAME, so one it never
	// receives simply reads as absent instead of swallowing the next message and shifting
	// everything after it - which is exactly how this failed when the pair was added to the
	// manager and not here. Kept because the scenario is more faithful for sending them.
	for tag in [b"STORAGE_RAM".as_slice(), b"STORAGE_TMP".as_slice()] {
		pm_boot_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).map_err(|_| "could not hand PermissionManager a memory volume slot")?;
	}
	send_cap(&pm_boot_kernel, b"SERVICES", services_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"USBBUS", usb_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"PROCESS", process_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"SERVE", perm_server, Rights::ALL)?;
	// END the run, the way the supervisor does. PermissionManager takes its capabilities by name
	// out of a set read up to this terminator, so without it the manager waits for a message that
	// never comes and does nothing at all - which is how this harness first failed after that
	// migration. Being a hand-written third sender of a handshake is exactly why the ordered
	// version of it kept drifting.
	pm_boot_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).map_err(|_| "could not end PermissionManager's bootstrap")?;

	sched::run_until_idle();
	let open_request = net_server.recv().map_err(|_| "PermissionManager did not request a fresh NetworkService client")?;
	if open_request.bytes.len() != 6 || le_u16(&open_request.bytes, 0) != 6 {
		return Err("PermissionManager sent an invalid NetworkService open request");
	}
	let (tool_net_server, tool_net_client) = Channel::create();
	let mut open_reply = alloc::vec::Vec::new();
	open_reply.extend_from_slice(&open_request.bytes[2..6]);
	open_reply.push(1);
	open_reply.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&net_server, &open_reply, tool_net_client, Rights::ALL)?;
	sched::run_until_idle();
	let info_request = tool_net_server.recv().map_err(|_| "governed ip did not query its fresh NetworkService client")?;
	if info_request.bytes.len() != 6 || le_u16(&info_request.bytes, 0) != 1 {
		return Err("governed ip sent an invalid NetworkService info request");
	}
	let mut info_reply = alloc::vec::Vec::new();
	info_reply.extend_from_slice(&info_request.bytes[2..6]);
	info_reply.push(1);
	info_reply.extend_from_slice(&[10, 0, 2, 15]);
	info_reply.extend_from_slice(&6u16.to_le_bytes());
	info_reply.extend_from_slice(&[0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
	info_reply.extend_from_slice(&1500u16.to_le_bytes());
	info_reply.extend_from_slice(&[10, 0, 2, 2]);
	info_reply.extend_from_slice(&0u16.to_le_bytes());
	tool_net_server.send(Message::new(info_reply, alloc::vec::Vec::new(), 0)).map_err(|_| "could not answer governed ip NetworkService request")?;
	sched::run_until_idle();

	// A two-stage pipeline through the SAME broker: `echo` writes into the edge and `readln`
	// reads it back out, so this proves the transaction end to end - both stages authorized,
	// the edge allocated by the broker, the stages released together, and data actually
	// crossing from one to the other. `readln` prefixes each line it reads with `in> `, so
	// the reply distinguishes "the consumer read the producer's bytes" from "the producer's
	// bytes reached the terminal directly".
	let (pipeline_read_end, pipeline_write_end) = Channel::create();
	let mut pipeline_request: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	pipeline_request.extend_from_slice(&4u16.to_le_bytes());
	pipeline_request.extend_from_slice(&0u32.to_le_bytes());
	pipeline_request.extend_from_slice(&2u16.to_le_bytes());
	for (name, args) in [(&b"echo"[..], &b"hello"[..]), (&b"readln"[..], &b""[..])] {
		for value in [name, args] {
			pipeline_request.extend_from_slice(&(value.len() as u16).to_le_bytes());
			pipeline_request.extend_from_slice(value);
		}
		// `merge-errors`: this stage's diagnostics go to the terminal, the ordinary case.
		pipeline_request.push(0);
	}
	pipeline_request.extend_from_slice(&(b"vol://system".len() as u16).to_le_bytes());
	pipeline_request.extend_from_slice(b"vol://system");
	pipeline_request.extend_from_slice(&0u16.to_le_bytes()); // an empty environment
	pipeline_request.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&perm_client, &pipeline_request, pipeline_write_end, Rights::ALL)?;
	sched::run_until_idle();
	let pipeline_reply = perm_client.recv().map_err(|_| "PermissionManager did not answer the pipeline request")?;
	let pipeline_started: bool = pipeline_reply.bytes.len() >= 5 && pipeline_reply.bytes[4] != 0;
	// Drained rather than read once: `readln` prints its prefix and the line it read as two
	// separate writes, so the consumer's output arrives as two messages. Reading one and
	// comparing would fail on an otherwise working pipeline.
	let mut pipeline_read: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	if pipeline_started {
		for _ in 0..8 {
			sched::run_until_idle();
			match pipeline_read_end.recv() {
				Ok(message) => pipeline_read.extend_from_slice(&message.bytes),
				Err(_) => break,
			}
		}
	}
	// REDIRECTION AS A PIPELINE STAGE, end to end through the broker.
	//
	// `cat < hello.txt` is `redirect_in hello.txt | cat` after the shell expands it, and this drives
	// the expansion's product directly: `redirect_in hello.txt | readln`. What it proves is the
	// whole design - `redirect_in` was authorized against its own manifest, opened the file with the
	// `volumes` grant that manifest gives it, and pushed the bytes down an edge the broker
	// allocated, while the consumer holds no storage capability of any kind and reads a stream.
	//
	// `readln` prefixes each line with `in> `, which is what tells "the consumer read the file's
	// bytes" from "the bytes reached the terminal some other way".
	let (redirect_read_end, redirect_write_end) = Channel::create();
	let mut redirect_request: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	redirect_request.extend_from_slice(&4u16.to_le_bytes());
	redirect_request.extend_from_slice(&2u32.to_le_bytes());
	redirect_request.extend_from_slice(&2u16.to_le_bytes());
	for (name, args) in [(&b"redirect_in"[..], &b"hello.txt"[..]), (&b"readln"[..], &b""[..])] {
		for value in [name, args] {
			redirect_request.extend_from_slice(&(value.len() as u16).to_le_bytes());
			redirect_request.extend_from_slice(value);
		}
		// `merge-errors`: this stage's diagnostics go to the terminal, the ordinary case.
		redirect_request.push(0);
	}
	redirect_request.extend_from_slice(&(b"vol://system".len() as u16).to_le_bytes());
	redirect_request.extend_from_slice(b"vol://system");
	redirect_request.extend_from_slice(&0u16.to_le_bytes());
	redirect_request.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&perm_client, &redirect_request, redirect_write_end, Rights::ALL)?;
	sched::run_until_idle();
	let _ = perm_client.recv();
	let mut redirect_read: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	for _ in 0..8 {
		sched::run_until_idle();
		match redirect_read_end.recv() {
			Ok(message) => redirect_read.extend_from_slice(&message.bytes),
			Err(_) => break,
		}
	}

	// A pipeline whose PRODUCER fails: its diagnostic belongs on the terminal, not in the pipe.
	//
	// `cat` refuses a path no volume can name and says so. Every stage but the last writes into an
	// edge, so without an error endpoint that sentence goes to stdout - the edge - and `readln`
	// reads it as input and echoes it behind its own `in> ` prefix. With one, the terminal sees the
	// message itself and the pipe carries nothing.
	let (diagnostic_read_end, diagnostic_write_end) = Channel::create();
	let mut diagnostic_request: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	diagnostic_request.extend_from_slice(&4u16.to_le_bytes());
	diagnostic_request.extend_from_slice(&1u32.to_le_bytes());
	diagnostic_request.extend_from_slice(&2u16.to_le_bytes());
	for (name, args) in [(&b"cat"[..], &b"::not-a-path"[..]), (&b"readln"[..], &b""[..])] {
		for value in [name, args] {
			diagnostic_request.extend_from_slice(&(value.len() as u16).to_le_bytes());
			diagnostic_request.extend_from_slice(value);
		}
		// `merge-errors`: this stage's diagnostics go to the terminal, the ordinary case.
		diagnostic_request.push(0);
	}
	diagnostic_request.extend_from_slice(&(b"vol://system".len() as u16).to_le_bytes());
	diagnostic_request.extend_from_slice(b"vol://system");
	diagnostic_request.extend_from_slice(&0u16.to_le_bytes());
	diagnostic_request.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&perm_client, &diagnostic_request, diagnostic_write_end, Rights::ALL)?;
	sched::run_until_idle();
	let _ = perm_client.recv();
	let mut diagnostic_read: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	for _ in 0..16 {
		sched::run_until_idle();
		match diagnostic_read_end.recv() {
			Ok(message) => diagnostic_read.extend_from_slice(&message.bytes),
			Err(_) => break,
		}
	}

	// THE SAME LINE WITH `2>&1`, which is the previous scenario with one byte of the request flipped.
	//
	// That is the point of it. `cat ::not-a-path | readln` above proves the diagnostic reaches the
	// terminal and NOT the pipe; this is the identical request, the identical stages, the identical
	// failure - and `merge-errors` set on the producer. The sentence now arrives at the consumer,
	// behind `readln`'s `in> ` prefix, which is the prefix that distinguishes "read as input" from
	// "printed to the terminal".
	//
	// AND IT COULD NOT HAVE BEEN A STAGE. Every other redirection expands into a program because it
	// names a file, and a file is authority somebody has to be granted. This one names the stage's
	// own output edge - a handle the broker allocates inside this very transaction, which the shell
	// never sees and cannot pass. So it travels as a flag on the record and the broker duplicates
	// the handle it just made, which is what this exercises.
	let (merged_read_end, merged_write_end) = Channel::create();
	let mut merged_request: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	merged_request.extend_from_slice(&4u16.to_le_bytes());
	merged_request.extend_from_slice(&3u32.to_le_bytes());
	merged_request.extend_from_slice(&2u16.to_le_bytes());
	for (name, args, merge) in [(&b"cat"[..], &b"::not-a-path"[..], 1u8), (&b"readln"[..], &b""[..], 0u8)] {
		for value in [name, args] {
			merged_request.extend_from_slice(&(value.len() as u16).to_le_bytes());
			merged_request.extend_from_slice(value);
		}
		merged_request.push(merge);
	}
	merged_request.extend_from_slice(&(b"vol://system".len() as u16).to_le_bytes());
	merged_request.extend_from_slice(b"vol://system");
	merged_request.extend_from_slice(&0u16.to_le_bytes());
	merged_request.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&perm_client, &merged_request, merged_write_end, Rights::ALL)?;
	sched::run_until_idle();
	let _ = perm_client.recv();
	let mut merged_read: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	for _ in 0..16 {
		sched::run_until_idle();
		match merged_read_end.recv() {
			Ok(message) => merged_read.extend_from_slice(&message.bytes),
			Err(_) => break,
		}
	}

	// THE MIGRATED STREAM TOOLS, reading a pipeline instead of a path.
	//
	// This is what the migration is FOR, and each of the three asks a different question.
	//
	// `redirect_in motd.txt | wc` - a tool that was given no path at all counts the bytes that
	// arrived on its input. `motd.txt` has two lines, so a leading "2" says it counted the whole
	// stream: a `wc` that read nothing would print zeros and one that lost a window would print one.
	//
	// `redirect_in motd.txt | head -n 1` - the early-closing consumer this milestone asks for.
	// `head` stops after one line and drops its source, which closes the read end; the producer
	// learns at its next write. Nothing in `head` does that explicitly - it falls out of owning the
	// endpoint - so what this really pins is that the pipeline ENDS rather than hanging with a
	// producer blocked on a consumer that is gone.
	//
	// `redirect_in motd.txt | tee <path> | wc` - the fan-out, on a scenario whose volume is a
	// read-only archive. So the destination CANNOT be opened, which makes this the test for the
	// decision `tee` documents: a destination that fails is reported and abandoned, and the stream
	// the rest of the line is built on carries on. `wc` still counts two lines.
	let mut stream_reads: alloc::vec::Vec<alloc::vec::Vec<u8>> = alloc::vec::Vec::new();
	for (correlation, stages) in [
		(4u32, &[(&b"redirect_in"[..], &b"motd.txt"[..]), (&b"tee"[..], &b"teed.txt"[..]), (&b"wc"[..], &b""[..])][..]),
		(5u32, &[(&b"redirect_in"[..], &b"motd.txt"[..]), (&b"head"[..], &b"-n 1"[..])][..]),
		(6u32, &[(&b"redirect_in"[..], &b"motd.txt"[..]), (&b"wc"[..], &b""[..])][..]),
	] {
		let (read_end, write_end) = Channel::create();
		let mut request: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
		request.extend_from_slice(&4u16.to_le_bytes());
		request.extend_from_slice(&correlation.to_le_bytes());
		request.extend_from_slice(&(stages.len() as u16).to_le_bytes());
		for (name, args) in stages {
			for value in [name, args] {
				request.extend_from_slice(&(value.len() as u16).to_le_bytes());
				request.extend_from_slice(value);
			}
			request.push(0);
		}
		request.extend_from_slice(&(b"vol://system".len() as u16).to_le_bytes());
		request.extend_from_slice(b"vol://system");
		request.extend_from_slice(&0u16.to_le_bytes());
		request.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&perm_client, &request, write_end, Rights::ALL)?;
		sched::run_until_idle();
		let _ = perm_client.recv();
		let mut read: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
		for _ in 0..16 {
			sched::run_until_idle();
			match read_end.recv() {
				Ok(message) => read.extend_from_slice(&message.bytes),
				Err(_) => break,
			}
		}
		stream_reads.push(read);
	}

	// The P02M0101 command tools, through the same governed path as everything else.
	//
	// `pwd` holds NO capability: its answer comes from the launch context, so this is the one
	// launch here that proves a tool can be useful with nothing granted at all. `wc` and `head`
	// hold the volume bundle and read `hello.txt` through the bounded `volume.read` - the window
	// path, not `open`, which is the difference between a tool that streams and one that maps.
	// EACH ONE IS STAGED WHERE THE LAUNCHER WILL LOOK. Asserted before any of them is launched,
	// because a tool that is not in the volume is refused by PermissionManager with the same reply
	// as one that is refused by policy - and because the coverage model reads these lookups to know
	// what this test reaches.
	for staged in [
		program_elf(&package, volume, b"pwd"),
		program_elf(&package, volume, b"wc"),
		program_elf(&package, volume, b"head"),
		program_elf(&package, volume, b"tail"),
		program_elf(&package, volume, b"hexdump"),
		program_elf(&package, volume, b"which"),
		program_elf(&package, volume, b"grep"),
		program_elf(&package, volume, b"sort"),
		program_elf(&package, volume, b"cut"),
	] {
		if staged.is_none() {
			return Err("a P02M0101 command tool is not staged in the volume");
		}
	}
	let mut command_read: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	for (index, (name, args)) in [
		(&b"pwd"[..], &b""[..]),
		(&b"wc"[..], &b"hello.txt"[..]),
		(&b"head"[..], &b"-n 1 hello.txt"[..]),
		(&b"tail"[..], &b"-n 1 hello.txt"[..]),
		(&b"hexdump"[..], &b"-n 4 hello.txt"[..]),
		(&b"which"[..], &b"cat"[..]),
		(&b"find"[..], &b"vol://system --name hello.txt"[..]),
		(&b"grep"[..], &b"-c e hello.txt"[..]),
		(&b"sort"[..], &b"hello.txt"[..]),
		(&b"cut"[..], &b"-c 1-3 hello.txt"[..]),
		// NO DIRECTORY-WALKING TOOL IS IN THIS LIST, and the reason is the harness rather than the
		// tools: a listing is a sub-channel the storage service pumps between passes, and
		// `run_until_idle` returns while that is still in flight - so a governed `ls` prints
		// nothing here either, which is how this was told apart from a defect in the walker.
		// `tree` and `find` are covered by the walker's own bounds instead.
	]
	.iter()
	.enumerate()
	{
		let (output, stdout) = Channel::create();
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&3u16.to_le_bytes());
		request.extend_from_slice(&(0x101 + index as u32).to_le_bytes());
		for value in [*name, *args, &b"vol://system"[..]] {
			request.extend_from_slice(&(value.len() as u16).to_le_bytes());
			request.extend_from_slice(value);
		}
		// ONE VARIABLE: `PATH`, which is what `which` resolves through. It is the whole environment
		// because this is the only field any of these tools reads, and an empty one is what every
		// other launch in this scenario sends.
		request.extend_from_slice(&1u16.to_le_bytes());
		for value in [&b"PATH"[..], &b"vol://system/bin"[..]] {
			request.extend_from_slice(&(value.len() as u16).to_le_bytes());
			request.extend_from_slice(value);
		}
		request.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&perm_client, &request, stdout, Rights::ALL)?;
		sched::run_until_idle();
		let reply = perm_client.recv().map_err(|_| "PermissionManager did not answer a command tool run")?;
		if reply.bytes.len() < 5 || reply.bytes[4] == 0 {
			return Err("PermissionManager refused a P02M0101 command tool");
		}
		// A tool writes its answer in as many messages as it likes, AND a tool that walks a
		// directory does not produce its first message on the first pass: a listing is a
		// sub-channel the storage service pumps between passes, and `run_until_idle` returns while
		// that is still in flight. So this pumps a fixed number of times and takes what arrives
		// rather than stopping at the first empty receive - which is what made `find` and `tree`
		// look like tools that printed nothing.
		for _ in 0..256 {
			sched::run_until_idle();
			while let Ok(message) = output.recv() {
				command_read.extend_from_slice(&message.bytes);
			}
		}
	}

	// PermissionManager reports its "online" line, then each governed component's proof and
	// decisions summary: the bytes sandbox_probe read through its one granted storage
	// capability and its summary, the instant `date` printed through its one granted time
	// capability and its summary, then request_probe's verdict on its runtime request for an
	// undeclared capability and its summary (which marks that refused request as dynamic) -
	// exactly which capabilities each component was and was not given - and finally the bytes
	// the on-demand `cat` tool printed through its storage grant to the forwarded stdout.
	let _online = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported nothing")?;
	let probe_read = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no sandbox read-back")?;
	let probe_summary = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no sandbox decisions summary")?;
	let date_read = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no date read-back")?;
	let date_summary = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no date decisions summary")?;
	let request_read = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no dynamic-request verdict")?;
	let request_summary = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no dynamic-request decisions summary")?;
	let cat_read = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no cat output")?;
	let ip_read = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no ip output")?;
	let ip_summary = pm_boot_kernel.recv().map_err(|_| "PermissionManager reported no ip decisions summary")?;
	// THE SHELL ITSELF, driven by a typed line - the one layer the rest of this scenario skips.
	//
	// Every pipeline above is a hand-written request to PermissionManager, which is the transaction
	// and not the shell: the parse, the redirection expansion, the launch and the status report are
	// the half above it, and the defects found in this milestone lived there. So this spawns the
	// real `shell` binary against the services already running here, types one line at its console
	// and reads what comes back.
	//
	// LAST IN THE SCENARIO on purpose. The shell needs a client to PermissionManager, and this test
	// holds the only one; a second handle to the same endpoint is a second holder of one reply
	// queue, which is the exact hazard `volume.connect` was added to end. Running the shell after
	// the test has finished asking its own questions means the two never overlap.
	let shell_read: alloc::vec::Vec<u8> = {
		let shell_elf = program_elf(&package, volume, b"shell").ok_or("shell in the package or volume")?;
		let (shell_boot_kernel, shell_boot_user) = Channel::create();
		// One full-duplex channel, because that is what a console IS to a shell: it prints to the
		// same endpoint it reads keystrokes from. The test holds the other end and plays the
		// terminal.
		let (terminal, shell_console) = Channel::create();
		let (_terminal_control, shell_control) = Channel::create();
		// Required at bootstrap and unused by anything typed here. A dead peer is the honest stand-in:
		// the net builtins would report the service unavailable, which is true.
		let (_dead_net, shell_net) = Channel::create();
		let _shell = spawn_dynamic_test_process(sched::root_domain(), shell_elf, shell_boot_user);
		send_cap(&shell_boot_kernel, b"STORAGE", shell_storage_client, Rights::ALL)?;
		send_cap(&shell_boot_kernel, b"PROCESS", process_client_for_shell, Rights::ALL)?;
		send_cap(&shell_boot_kernel, b"NET", shell_net, Rights::ALL)?;
		send_cap(&shell_boot_kernel, b"PERM", perm_client_for_shell, Rights::ALL)?;
		send_cap(&shell_boot_kernel, b"CONSOLE", shell_console, Rights::ALL)?;
		send_cap(&shell_boot_kernel, b"CONTROL", shell_control, Rights::ALL)?;
		shell_boot_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).map_err(|_| "could not end the shell's bootstrap")?;
		sched::run_until_idle();
		// WHAT THE SHELL SAID ON ITS BOOTSTRAP, kept for the failure message. A shell that could not
		// start writes the reason there and exits, and without this a missing capability looks
		// exactly like a shell that ran and printed nothing.
		let mut announced: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
		while let Ok(message) = shell_boot_kernel.recv() {
			announced.extend_from_slice(&message.bytes);
			sched::run_until_idle();
		}
		// The banner and the first prompt, discarded: what this test is about is what comes back
		// AFTER the line, and leaving the greeting in the buffer would let a test pass on it.
		while terminal.recv().is_ok() {
			sched::run_until_idle();
		}
		// THE LINES, EACH AS ONE MESSAGE, which is what a line discipline delivers: the shell's read
		// loop takes a whole submitted line including its newline. Everything they print is
		// concatenated, because what each one proves is a distinct string in the output and the
		// order they arrive in is the order they were typed.
		//
		// Together they are the forms this milestone added, at the layer a person reaches them:
		//   - a two-stage pipeline of a redirection and a migrated tool, which is the whole chain;
		//   - `<` and `>` in one line, whose destination is a read-only archive here, so what it
		//     proves is that the refusal reaches the terminal rather than the pipe;
		//   - `2>&1`, folding a failing stage's diagnostic into the stream so `wc` counts it;
		//   - three lines the grammar refuses, each of which used to be RUN as an ordinary command:
		//     an unsupported descriptor, a state-mutating builtin in a pipeline, and a redirection
		//     target that expanded to nothing.
		let mut read: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
		for line in [
			&b"redirect_in motd.txt | wc\n"[..],
			&b"cat < motd.txt > copied.txt\n"[..],
			&b"cat ::not-a-path 2>&1 | wc\n"[..],
			&b"cat 3>&1\n"[..],
			&b"cd x | grep y\n"[..],
			&b"cat < $NOTHING\n"[..],
			// THE TOOLS ADDED FOR P02M0101, at the layer a person reaches them. `watch` and `less`
			// own the terminal while they run, so they are not typed here - a test that took the
			// alternate screen would be reading its own redraws. `tee` is the one of the three that
			// composes, and this is it in the shape it is for: a producer, a copy to a file, and a
			// consumer that still sees the stream.
			&b"redirect_in motd.txt | tee teed.txt | wc\n"[..],
		] {
			terminal.send(Message::new(line.to_vec(), alloc::vec::Vec::new(), 0)).map_err(|_| "could not type at the shell")?;
			for _ in 0..24 {
				sched::run_until_idle();
				match terminal.recv() {
					Ok(message) => read.extend_from_slice(&message.bytes),
					Err(_) => continue,
				}
			}
		}
		if read.is_empty() {
			read.extend_from_slice(b"<the shell printed nothing; its bootstrap said: ");
			read.extend_from_slice(&announced);
			read.extend_from_slice(b">");
		}
		read
	};

	if scenario != PermissionScenario::ScopedGrants {
		return Ok(PermissionScenarioResult { pipeline_read: pipeline_read.clone(), pipeline_started, diagnostic_read: diagnostic_read.clone(), redirect_read: redirect_read.clone(), merged_read: merged_read.clone(), stream_reads: stream_reads.clone(), shell_read: shell_read.clone(), expected, probe_read: probe_read.bytes, probe_summary: probe_summary.bytes, date_read: date_read.bytes, date_summary: date_summary.bytes, command_read: command_read.clone(), request_read: request_read.bytes, request_summary: request_summary.bytes, cat_read: cat_read.bytes, ip_read: ip_read.bytes, ip_summary: ip_summary.bytes, graphics_read: alloc::vec::Vec::new(), graphics_start_ns: 0 });
	}

	// Prequeue one successful admin mint on each private connection. PermissionManager's
	// generated clients all start at correlation id 0; DisplayService additionally receives
	// the exact Process handle in the bind request queued at `display_admin_server`.
	let admin_reply = |channel: &Channel, capability: alloc::sync::Arc<dyn object::KernelObject>, corr: u32| -> Result<(), &'static str> {
		let mut bytes = alloc::vec::Vec::new();
		bytes.extend_from_slice(&corr.to_le_bytes());
		bytes.push(1);
		bytes.extend_from_slice(&0u32.to_le_bytes());
		send_cap(channel, &bytes, capability, Rights::ALL)
	};
	admin_reply(&display_admin_server, display_scope_client, 0)?;
	admin_reply(&input_admin_server, input_scope_client, 0)?;
	admin_reply(&audio_admin_server, audio_scope_client, 0)?;

	let (graphics_output, graphics_stdout) = Channel::create();
	let mut run = alloc::vec::Vec::new();
	run.extend_from_slice(&3u16.to_le_bytes());
	run.extend_from_slice(&0u32.to_le_bytes());
	for value in [&b"graphics_probe"[..], &b""[..], &b"vol://system"[..]] {
		run.extend_from_slice(&(value.len() as u16).to_le_bytes());
		run.extend_from_slice(value);
	}
	run.extend_from_slice(&0u16.to_le_bytes()); // an empty environment
	run.extend_from_slice(&0u32.to_le_bytes());
	let graphics_start = arch::tsc::now();
	send_cap(&perm_client, &run, graphics_stdout, Rights::ALL)?;
	sched::run_until_idle();
	let run_reply = perm_client.recv().map_err(|_| "PermissionManager did not answer graphics_probe run")?;
	if run_reply.bytes.len() < 5 || run_reply.bytes[4] == 0 {
		return Err("PermissionManager refused graphics_probe");
	}
	let graphics_read = graphics_output.recv().map_err(|_| "graphics_probe received incomplete grants")?;
	let graphics_start_ns = arch::tsc::cycles_to_ns(arch::tsc::now().wrapping_sub(graphics_start));
	crate::serial_println!("app-start-perf: graphics_probe={}ns", graphics_start_ns);

	// Launch the real image viewer through the same governed path. Each scoped grant uses a
	// fresh generated admin client, so its correlation id starts at zero; their server ends
	// are the focused stand-ins below, so the test observes the exact app-side protocol.
	let (view_display_server, view_display_client) = Channel::create();
	let (view_input_server, view_input_client) = Channel::create();
	admin_reply(&display_admin_server, view_display_client, 0)?;
	admin_reply(&input_admin_server, view_input_client, 0)?;
	let (view_output, view_stdout) = Channel::create();
	let mut view_run = alloc::vec::Vec::new();
	view_run.extend_from_slice(&3u16.to_le_bytes());
	view_run.extend_from_slice(&1u32.to_le_bytes());
	for value in [&b"imgview"[..], &b"vol://system/wallpapers/logo.webp"[..], &b"vol://system"[..]] {
		view_run.extend_from_slice(&(value.len() as u16).to_le_bytes());
		view_run.extend_from_slice(value);
	}
	view_run.extend_from_slice(&0u16.to_le_bytes()); // an empty environment
	view_run.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&perm_client, &view_run, view_stdout, Rights::ALL)?;
	sched::run_until_idle();
	let view_reply = perm_client.recv().map_err(|_| "PermissionManager did not answer imgview run")?;
	if view_reply.bytes.len() < 5 || view_reply.bytes[4] == 0 {
		return Err("PermissionManager refused imgview");
	}
	let view_process = view_reply.caps.first().ok_or("imgview run returned no Process handle")?.object().into_any_arc().downcast::<Process>().map_err(|_| "imgview run handle was not a Process")?;

	let acquire = view_display_server.recv().map_err(|_| "imgview did not acquire a surface")?;
	if acquire.bytes.len() < 14 || le_u16(&acquire.bytes, 0) != 1 || le_u32(&acquire.bytes, 6) != 0 || le_u32(&acquire.bytes, 10) != 0 {
		return Err("imgview sent an invalid acquire request");
	}
	let surface = MemoryObject::create(4).ok_or("imgview surface allocation failed")?;
	let acquire_corr = le_u32(&acquire.bytes, 2);
	let mut acquire_reply = alloc::vec::Vec::new();
	acquire_reply.extend_from_slice(&acquire_corr.to_le_bytes());
	acquire_reply.push(1);
	acquire_reply.extend_from_slice(&4u64.to_le_bytes());
	acquire_reply.extend_from_slice(&1u32.to_le_bytes());
	acquire_reply.extend_from_slice(&1u32.to_le_bytes());
	acquire_reply.extend_from_slice(&4u32.to_le_bytes());
	acquire_reply.push(0);
	send_cap(&view_display_server, &acquire_reply, surface.clone(), Rights::ALL)?;
	sched::run_until_idle();

	let present = view_display_server.recv().map_err(|_| "imgview did not present its decoded image")?;
	if present.bytes.len() < 22 || le_u16(&present.bytes, 0) != 2 || le_u32(&present.bytes, 14) != 1 || le_u32(&present.bytes, 18) != 1 {
		return Err("imgview sent an invalid first present");
	}
	if !read_from_object(&surface, 4).iter().any(|byte| *byte != 0) {
		return Err("imgview presented a blank decoded image");
	}
	let present_corr = le_u32(&present.bytes, 2);
	view_display_server.send(Message::new([present_corr.to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).map_err(|_| "imgview present reply failed")?;
	sched::run_until_idle();

	let focus_request = view_display_server.recv().map_err(|_| "imgview did not request input focus")?;
	if focus_request.bytes.len() < 6 || le_u16(&focus_request.bytes, 0) != 5 {
		return Err("imgview sent an invalid input-focus request");
	}
	let focus_corr = le_u32(&focus_request.bytes, 2);
	let (_focus_server, focus_client) = Channel::create();
	let mut focus_reply = alloc::vec::Vec::new();
	focus_reply.extend_from_slice(&focus_corr.to_le_bytes());
	focus_reply.push(1);
	focus_reply.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&view_display_server, &focus_reply, focus_client.clone(), Rights::ALL)?;
	sched::run_until_idle();

	let subscribe = view_input_server.recv().map_err(|_| "imgview did not subscribe to focused keys")?;
	if subscribe.bytes.len() < 10 || le_u16(&subscribe.bytes, 0) != 2 || subscribe.caps.is_empty() {
		return Err("imgview sent an invalid key subscription");
	}
	let transferred_focus = subscribe.caps[0].object().into_any_arc().downcast::<Channel>().map_err(|_| "imgview key subscription did not transfer focus proof")?;
	if !alloc::sync::Arc::ptr_eq(&transferred_focus, &focus_client) {
		return Err("imgview transferred the wrong focus proof");
	}
	let subscribe_corr = le_u32(&subscribe.bytes, 2);
	let (key_producer, key_consumer) = Channel::create();
	send_cap(&view_input_server, &subscribe_corr.to_le_bytes(), key_consumer, Rights::ALL)?;
	sched::run_until_idle();
	// Zoom in before panning, because `imgview` fits the image to the framebuffer when it opens:
	// at the initial zoom the image is never larger than the viewport, `can_pan` is false, and an
	// arrow key correctly redraws nothing. Panning only becomes possible once the image exceeds
	// the viewport, so asserting a pan present without zooming first asked for the impossible -
	// which is what this scenario did, and why it had never passed.
	let zoom_frame = [0, 0, 0, 0, 0x2e, 0, 1];
	key_producer.send(Message::new(zoom_frame.to_vec(), alloc::vec::Vec::new(), 0)).map_err(|_| "failed to send imgview zoom key")?;
	sched::run_until_idle();
	let zoom_present = view_display_server.recv().map_err(|_| "imgview did not present after zoom-in")?;
	let zoom_corr = le_u32(&zoom_present.bytes, 2);
	view_display_server.send(Message::new([zoom_corr.to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).map_err(|_| "imgview zoom-present reply failed")?;
	sched::run_until_idle();

	let pan_frame = [0, 0, 0, 0, 0x4f, 0, 1];
	key_producer.send(Message::new(pan_frame.to_vec(), alloc::vec::Vec::new(), 0)).map_err(|_| "failed to send imgview pan key")?;
	sched::run_until_idle();
	let pan_present = view_display_server.recv().map_err(|_| "imgview did not present after arrow-key pan")?;
	if pan_present.bytes.len() < 22 || le_u16(&pan_present.bytes, 0) != 2 || le_u32(&pan_present.bytes, 14) != 1 || le_u32(&pan_present.bytes, 18) != 1 {
		return Err("imgview sent an invalid pan present");
	}
	let pan_corr = le_u32(&pan_present.bytes, 2);
	view_display_server.send(Message::new([pan_corr.to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).map_err(|_| "imgview pan-present reply failed")?;
	sched::run_until_idle();
	// Release the arrow, because panning is continuous while a key is held: a press that is
	// never released keeps producing presents, and how many arrive before the next step is a
	// matter of how fast the target runs. Emulated riscv64 is roughly twenty-five times slower
	// than x86_64 here, which is enough for the difference to change what the next receive sees.
	let pan_release = [0, 0, 0, 0, 0x4f, 0, 0];
	key_producer.send(Message::new(pan_release.to_vec(), alloc::vec::Vec::new(), 0)).map_err(|_| "failed to release imgview pan key")?;
	sched::run_until_idle();
	let quit_frame = [1, 0, 0, 0, 0x14, 0, 1];
	key_producer.send(Message::new(quit_frame.to_vec(), alloc::vec::Vec::new(), 0)).map_err(|_| "failed to send imgview quit key")?;
	sched::run_until_idle();

	// Answer any presents still in flight before the release. Requiring the release to be the
	// very next message would make this scenario depend on the target's speed rather than on
	// `imgview` giving the surface back, which is what it is here to prove.
	let release = loop {
		let message = view_display_server.recv().map_err(|_| "imgview did not release its surface after q")?;
		if message.bytes.len() >= 6 && le_u16(&message.bytes, 0) == 3 {
			break message;
		}
		if message.bytes.len() < 6 || le_u16(&message.bytes, 0) != 2 {
			return Err("imgview sent an invalid release request");
		}
		let corr = le_u32(&message.bytes, 2);
		view_display_server.send(Message::new([corr.to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).map_err(|_| "imgview trailing-present reply failed")?;
		sched::run_until_idle();
	};
	let release_corr = le_u32(&release.bytes, 2);
	view_display_server.send(Message::new([release_corr.to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).map_err(|_| "imgview release reply failed")?;
	core::mem::drop(view_output);
	sched::run_until_idle();
	if !view_process.is_terminated() {
		return Err("imgview did not exit after releasing the surface");
	}

	let (mp3_audio_server, mp3_audio_client) = Channel::create();
	admin_reply(&audio_admin_server, mp3_audio_client, 0)?;
	let (mp3_output, mp3_stdout) = Channel::create();
	let mut mp3_run = alloc::vec::Vec::new();
	mp3_run.extend_from_slice(&3u16.to_le_bytes());
	mp3_run.extend_from_slice(&11u32.to_le_bytes());
	for value in [&b"play"[..], &b"vol://system/audio/test.mp3"[..], &b"vol://system"[..]] {
		mp3_run.extend_from_slice(&(value.len() as u16).to_le_bytes());
		mp3_run.extend_from_slice(value);
	}
	mp3_run.extend_from_slice(&0u16.to_le_bytes()); // an empty environment
	mp3_run.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&perm_client, &mp3_run, mp3_stdout, Rights::ALL)?;
	sched::run_until_idle();
	let mp3_reply = perm_client.recv().map_err(|_| "PermissionManager did not answer MP3 play run")?;
	if mp3_reply.bytes.len() < 5 || mp3_reply.bytes[4] == 0 {
		return Err("PermissionManager refused MP3 play");
	}
	let mp3_process = mp3_reply.caps.first().ok_or("MP3 play returned no Process handle")?.object().into_any_arc().downcast::<Process>().map_err(|_| "MP3 play handle was not a Process")?;
	let mp3_open = mp3_audio_server.recv().map_err(|_| "MP3 play did not open an audio stream")?;
	if mp3_open.bytes.len() < 11 || le_u16(&mp3_open.bytes, 0) != 2 || le_u32(&mp3_open.bytes, 6) != 44_100 || mp3_open.bytes[10] != 1 {
		return Err("MP3 play opened the wrong format");
	}
	let (mp3_stream_server, mp3_stream_client) = Channel::create();
	let mp3_open_corr = le_u32(&mp3_open.bytes, 2);
	let mut mp3_open_reply = alloc::vec::Vec::new();
	mp3_open_reply.extend_from_slice(&mp3_open_corr.to_le_bytes());
	mp3_open_reply.push(1);
	mp3_open_reply.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&mp3_audio_server, &mp3_open_reply, mp3_stream_client, Rights::ALL)?;
	sched::run_until_idle();
	let mut heard_audio = false;
	for _ in 0..8 {
		let write = mp3_stream_server.recv().map_err(|_| "MP3 play sent too few PCM writes")?;
		if write.bytes.len() < 14 || le_u16(&write.bytes, 0) != 1 || le_u64(&write.bytes, 6) != 2_048 || write.caps.len() != 1 {
			return Err("MP3 play sent invalid decoded PCM");
		}
		let buffer = write.caps[0].object().into_any_arc().downcast::<MemoryObject>().map_err(|_| "MP3 play did not transfer PCM memory")?;
		heard_audio |= read_from_object(&buffer, 2_048).iter().any(|byte| *byte != 0);
		if heard_audio {
			mp3_process.set_int_pending();
			for thread in mp3_process.live_threads() {
				sched::wake_thread(&thread);
			}
		}
		let correlation = le_u32(&write.bytes, 2);
		mp3_stream_server.send(Message::new([correlation.to_le_bytes().as_slice(), &[1], &1_024u32.to_le_bytes()].concat(), alloc::vec::Vec::new(), 0)).map_err(|_| "MP3 PCM reply failed")?;
		sched::run_until_idle();
		if heard_audio {
			break;
		}
	}
	if !heard_audio {
		return Err("MP3 play decoded only silence after its bounded delay");
	}
	let mp3_close = mp3_stream_server.recv().map_err(|_| "MP3 play did not close")?;
	let mp3_close_corr = le_u32(&mp3_close.bytes, 2);
	mp3_stream_server.send(Message::new([mp3_close_corr.to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).map_err(|_| "MP3 close reply failed")?;
	core::mem::drop(mp3_output);
	sched::run_until_idle();
	if !mp3_process.is_terminated() {
		return Err("MP3 play did not exit");
	}
	Ok(PermissionScenarioResult { pipeline_read, pipeline_started, diagnostic_read, redirect_read, merged_read, stream_reads, shell_read, expected, probe_read: probe_read.bytes, probe_summary: probe_summary.bytes, date_read: date_read.bytes, date_summary: date_summary.bytes, command_read: command_read.clone(), request_read: request_read.bytes, request_summary: request_summary.bytes, cat_read: cat_read.bytes, ip_read: ip_read.bytes, ip_summary: ip_summary.bytes, graphics_read: graphics_read.bytes, graphics_start_ns })
}

// Build the component topology and run it to completion. A StorageService serves
// the ramdisk volume and a LogService holds the journal; the component_host is given
// exactly two capabilities - a StorageService client and a LogService client - and
// nothing else. It loads a real Wasm component (built by the Rust SDK, served from
// storage as vol://system/components/liber_component/app.wasm rather than embedded in the kernel image) and runs
// it: the component's three imports are wired by name to the two services - `read` /
// `write` to StorageService, `log` to LogService - with no ambient authority. The
// component reads its one granted file, upper-cases it, logs the result through
// LogService, writes it back, and returns the count; the host also calls the
// component's float `score` export. The kernel only brokers the initial capabilities.
// Returns (expected, content, logged, score): the upper-cased granted file, the bytes
// the component produced, whether the log grant was reached, and the float result.
fn run_component_scenario() -> Result<ComponentRun, &'static str> {
	use object::channel::Channel;
	use object::rights::Rights;

	let (volume, package) = scenario_packages()?;
	let raw = volume_file(volume, b"hello.txt")?;
	let expected: alloc::vec::Vec<u8> = raw.iter().map(|b: &u8| b.to_ascii_uppercase()).collect();
	let storage_elf = package.lookup(b"storage_service.lsexe").ok_or("storage_service.lsexe missing from the init package")?;
	let log_elf = package.lookup(b"log_service.lsexe").ok_or("log_service.lsexe missing from the init package")?;
	let host_elf = program_elf(&package, volume, b"component_host").ok_or("component_host missing from the package or volume")?;

	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (log_boot_kernel, log_boot_user) = Channel::create();
	let (host_boot_kernel, host_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	let (log_server, log_client) = Channel::create();

	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	loader::spawn_elf_process(domain.clone(), log_elf, log_boot_user, Rights::ALL, 0).map_err(|_| "failed to load LogService")?;
	let _host = spawn_dynamic_test_process(domain, host_elf, host_boot_user);

	// StorageService: the ramdisk volume and its service channel. LogService: its
	// service channel. component_host: the StorageService client, then the LogService
	// client - exactly the two capabilities its world is wired to, and nothing else.
	send_ramdisk(&storage_boot_kernel, volume)?;
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL)?;
	send_cap(&log_boot_kernel, b"SERVE", log_server, Rights::ALL)?;
	send_cap(&host_boot_kernel, b"STORAGE", storage_client, Rights::ALL)?;
	send_cap(&host_boot_kernel, b"LOG", log_client, Rights::ALL)?;

	sched::run_until_idle();
	let result = host_boot_kernel.recv().map_err(|_| "the host reported no result")?;
	let report = parse_component_report(&result.bytes)?;
	Ok(ComponentRun { expected, report })
}

// What component_host sends back: a log-grant flag, the two scores, `run`'s return value, the
// output file read back off the volume AFTER the run, and the copy the component handed to its
// write import.
//
// ONE PARSER, because there are two readers - this scenario and the volume-layout suite - and the
// report has grown three times. The second reader was left on a stale offset by the last growth and
// compared four bytes of score against the file's contents; a positional tuple did the same thing
// inside this function.
pub struct ComponentReport {
	pub logged: bool,
	pub score: i32,
	pub score_negative: i32,
	pub count: i32,
	pub readback: alloc::vec::Vec<u8>,
	pub output: alloc::vec::Vec<u8>,
}

pub fn parse_component_report(bytes: &[u8]) -> Result<ComponentReport, &'static str> {
	if bytes.len() < 17 {
		return Err("the host report was too short");
	}
	let word = |at: usize| i32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
	let written: usize = word(13) as u32 as usize;
	if bytes.len() < 17 + written {
		return Err("the host report claimed more written bytes than it carried");
	}
	Ok(ComponentReport {
		logged: bytes[0] != 0,
		score: word(1),
		// The negative case, where truncation and flooring differ - see the host, and `score`'s
		// comment.
		score_negative: word(5),
		// What `run` returned: the byte count it processed, or one of the world's negative statuses.
		// This was read into `_count` in the host and dropped, so nothing checked the export's
		// return ABI on either side.
		count: word(9),
		// The FILE, read back through StorageService after the run - the only thing that can prove
		// the write happened.
		readback: bytes[17..17 + written].to_vec(),
		// And the copy the component handed to its write import, which says the bytes reached the
		// granted path at all.
		output: bytes[17 + written..].to_vec(),
	})
}

// What one run of the SDK component produced: the bytes the granted file should have become, and
// everything the host reported.
pub struct ComponentRun {
	pub expected: alloc::vec::Vec<u8>,
	pub report: ComponentReport,
}

// Build the resource topology and run it to completion. The resource_manager
// (ResourceManager) is given the init package (to launch the component it governs from) and
// the channel its clients reach it on - nothing more, since it governs through the kernel's
// resource syscalls, not by brokering service connections. ResourceManager creates a
// bounded sub-Domain, launches its one governed component (resource_probe) into that Domain,
// caps the Domain's memory, drives the probe to fill the budget and be refused once (the
// over-budget allocation is contained to that Domain with RESOURCE_EXHAUSTED rather than
// crashing the probe or the system), then raises the cap at runtime and drives the probe
// into the new headroom. The kernel only charges and enforces the per-Domain budget.
// Returns the manager's budget summary: the pages granted under the cap, the contained
// refusal, and the pages regranted after the runtime raise.
fn run_resource_scenario() -> Result<alloc::vec::Vec<u8>, &'static str> {
	use object::channel::Channel;
	use object::rights::Rights;

	let (volume, package) = scenario_packages()?;
	let init = init_package_bytes().ok_or("init package module not found")?;
	let rm_elf = program_elf(&package, volume, b"resource_manager").ok_or("resource_manager missing from the package or volume")?;

	let (rm_boot_kernel, rm_boot_user) = Channel::create();
	let (resource_server, _resource_client) = Channel::create();

	let domain = sched::root_domain();
	let _resource_manager = spawn_dynamic_test_process(domain, rm_elf, rm_boot_user);

	// ResourceManager: the init package (to launch the probe from), the channel its clients
	// reach it on, and a ProcessService client. The order matches ResourceManager's receive
	// order: PACKAGE, SERVE, PROCESS.
	//
	// The process client's peer is dropped, the idiom this harness already uses for the
	// capabilities no scenario here answers on: a bootstrap reads its handoffs in order and
	// each read consumes whatever arrived, so a skipped one is not skipped - it swallows the
	// next message and blocks forever. That is how adding a capability to a service breaks
	// every caller that starts it, this harness included.
	let (process_peer, process_client) = Channel::create();
	core::mem::drop(process_peer);
	send_package(&rm_boot_kernel, init)?;
	send_cap(&rm_boot_kernel, b"SERVE", resource_server, Rights::ALL)?;
	send_cap(&rm_boot_kernel, b"PROCESS", process_client, Rights::ALL)?;

	sched::run_until_idle();

	// ResourceManager reports its "online" line, then the budget proof: the pages it granted
	// under the cap, the contained over-budget refusal, and the pages it regranted after
	// raising the budget at runtime.
	let _online = rm_boot_kernel.recv().map_err(|_| "ResourceManager reported nothing")?;
	let summary = rm_boot_kernel.recv().map_err(|_| "ResourceManager reported no budget summary")?;
	Ok(summary.bytes)
}

// Read a file from a vol:// volume by driving the StorageService as the kernel's
// own client (the kernel storage self-test). Spawns the service, hands
// it the ramdisk and a service channel, sends one open request plus an empty quit
// sentinel (so the service exits and the cooperative schedule drains), runs the
// schedule to completion, then receives the reply and reads the returned shared
// buffer through the HHDM. Returns the file's bytes, or an error string.
fn storage_read(uri: &[u8]) -> Result<alloc::vec::Vec<u8>, &'static str> {
	use alloc::sync::Arc;
	use object::KernelObject;
	use object::channel::{Channel, Message};
	use object::handle::Capability;
	use object::memory_object::MemoryObject;
	use object::rights::Rights;

	let volume = volume_package_bytes().ok_or("volume package module not found")?;
	let init = init_package_bytes().ok_or("init package module not found")?;
	let package = pkg::Package::parse(init).ok_or("init package is malformed")?;
	let service_elf = package.lookup(b"storage_service.lsexe").ok_or("storage_service.lsexe missing from the init package")?;

	// the ramdisk: a MemoryObject filled with the volume archive via the HHDM
	let ramdisk = MemoryObject::create(volume.len()).ok_or("no memory for the ramdisk")?;
	copy_into_object(&ramdisk, volume);

	let (service_boot_kernel, service_boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();

	loader::spawn_elf_process(sched::root_domain(), service_elf, service_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;

	// bootstrap the service: the ramdisk (with its length) and the service endpoint
	let mut ramdisk_msg = alloc::vec::Vec::with_capacity(7 + 8);
	ramdisk_msg.extend_from_slice(b"RAMDISK");
	ramdisk_msg.extend_from_slice(&(volume.len() as u64).to_le_bytes());
	let ramdisk_cap = Capability::new(ramdisk as Arc<dyn KernelObject>, Rights::READ | Rights::MAP, 0);
	service_boot_kernel.send(Message::new(ramdisk_msg, alloc::vec![ramdisk_cap], 0)).map_err(|_| "service ramdisk bootstrap failed")?;
	let service_server_cap = Capability::new(service_server as Arc<dyn KernelObject>, Rights::ALL, 0);
	service_boot_kernel.send(Message::new(b"SERVE".to_vec(), alloc::vec![service_server_cap], 0)).map_err(|_| "service serve bootstrap failed")?;

	// the generated volume.open request - [op u16][corr u32][open-opts] where
	// open-opts = [path: [len u16][utf8]][write u8][create u8] - then an empty quit
	// sentinel, which the service treats as end-of-session and exits on.
	let corr: u32 = 1;
	let mut request = alloc::vec::Vec::new();
	request.extend_from_slice(&1u16.to_le_bytes()); // OP_OPEN
	request.extend_from_slice(&corr.to_le_bytes());
	request.extend_from_slice(&(uri.len() as u16).to_le_bytes());
	request.extend_from_slice(uri);
	request.push(0); // write = false
	request.push(0); // create = false
	service_client.send(Message::new(request, alloc::vec::Vec::new(), 0)).map_err(|_| "open request failed")?;
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).map_err(|_| "quit sentinel failed")?;

	sched::run_until_idle();

	let reply = service_client.recv().map_err(|_| "the service sent no reply")?;
	// the generated reply - [corr u32][is_ok u8] then, on ok, the open-result record
	// [file placeholder u32][size u64] with the file capability transferred
	// out-of-band; the handle itself rides reply.caps, not the byte stream.
	if reply.bytes.len() < 5 {
		return Err("malformed reply");
	}
	if reply.bytes[4] != 1 {
		return Err("the service denied or could not find the file");
	}
	if reply.bytes.len() < 17 {
		return Err("malformed reply");
	}
	let size = u64::from_le_bytes([reply.bytes[9], reply.bytes[10], reply.bytes[11], reply.bytes[12], reply.bytes[13], reply.bytes[14], reply.bytes[15], reply.bytes[16]]) as usize;
	let cap = reply.caps.first().ok_or("the service granted no buffer")?;
	let object = cap.object();
	let memory = object.as_any().downcast_ref::<MemoryObject>().ok_or("the granted capability was not a buffer")?;
	Ok(read_from_object(memory, size))
}

// Read `len` bytes out of a MemoryObject's frames through the HHDM (the reverse of
// copy_into_object). The object need not be mapped: its physical frames are read
// directly.
fn read_from_object(object: &object::memory_object::MemoryObject, len: usize) -> alloc::vec::Vec<u8> {
	let hhdm = mem::hhdm_offset();
	let page = mem::frame::PAGE_SIZE as usize;
	let mut out = alloc::vec::Vec::with_capacity(len);
	for (i, &phys) in object.frames().iter().enumerate() {
		let start = i * page;
		if start >= len {
			break;
		}
		let end = core::cmp::min(start + page, len);
		let chunk = unsafe { core::slice::from_raw_parts((hhdm + phys) as *const u8, end - start) };
		out.extend_from_slice(chunk);
	}
	out
}

// Statics the fault-probe body records into; read back by the fault-isolation test.
static FAULT_GOT: core::sync::atomic::AtomicI64 = core::sync::atomic::AtomicI64::new(0);
static FAULT_KIND: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static FAULT_ADDR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// Kernel-thread body that drops to ring 3 running the fault-probe program. Before
// entering it opens a MemoryObject - charging its Domain's memory and a handle -
// and deliberately leaves it open, so that tearing the process down (when this
// thread is reaped) is what refunds it. The ring-3 program writes to an unmapped
// address and faults; the kernel records the fault, terminates the process, and
// longjmps back here, where we read the recorded fault and free the user mapping.
extern "C" fn user_fault_thread_body(_arg: u64) {
	use core::sync::atomic::Ordering;
	use mem::frame::{self, PAGE_SIZE};
	let _mo = unsafe { arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, PAGE_SIZE, 0, 0, 0) };
	let code = frame::allocate().expect("user code frame");
	let stack = frame::allocate().expect("user stack frame");
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
	arch::paging::map_page(USER_CODE_VA, code, flags);
	arch::paging::map_page(USER_STACK_VA, stack, flags | arch::paging::NO_EXECUTE);
	let program = arch::usermode::program_fault_bytes();
	unsafe {
		arch::paging::copy_to_user_page(USER_CODE_VA, program);
		// Drops to ring 3; the program faults and the kernel returns control here.
		arch::usermode::enter(USER_CODE_VA, USER_STACK_VA + PAGE_SIZE, 0);
	}
	// Back from the ring-3 fault: read the fault the kernel recorded for us.
	let mut info = fault::FaultInfo { kind: 0, error_code: 0, address: 0, instruction_pointer: 0 };
	let got = unsafe { arch::syscall::invoke(syscall::SYS_FAULT_INFO_GET, &mut info as *mut fault::FaultInfo as u64, core::mem::size_of::<fault::FaultInfo>() as u64, 0, 0) };
	FAULT_GOT.store(got as i64, Ordering::SeqCst);
	FAULT_KIND.store(info.kind, Ordering::SeqCst);
	FAULT_ADDR.store(info.address, Ordering::SeqCst);
	// Tear the user mapping down. The MemoryObject handle stays open on purpose, so
	// process teardown is what frees it.
	arch::paging::unmap_page(USER_CODE_VA);
	arch::paging::unmap_page(USER_STACK_VA);
	unsafe { frame::deallocate(code) };
	unsafe { frame::deallocate(stack) };
}

// A bindable test IRQ vector (33..47, distinct from the interrupt_bind test's) a
// crashing "driver" holds before it faults. x86-only (legacy INTx; aarch64 is MSI-only).
#[cfg(target_arch = "x86_64")]
const DRIVER_IRQ_VECTOR: u64 = 0x2d;

// Where the no-execute probe's recorded fault lands (mirrors the FAULT_* statics).
static NX_GOT: core::sync::atomic::AtomicI64 = core::sync::atomic::AtomicI64::new(0);
static NX_KIND: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static NX_ADDR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static NX_CODE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// Where the stack-growth probe's outcome lands: whether a fault was recorded (0 =
// clean exit), its kind/address/code, and the Domain's mapped stack bytes observed
// while the process was still alive.
static STACK_GOT: core::sync::atomic::AtomicI64 = core::sync::atomic::AtomicI64::new(0);
static STACK_KIND: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static STACK_ADDR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static STACK_CODE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static STACK_USED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// Kernel-thread body that drops to ring 3 running the stack-growth probe with
// `pages` touches. Only the code page is mapped up front - the stack region below
// USER_STACK_TOP starts entirely unmapped, so even the probe's first store
// demand-pages. After the excursion the touched span is unmapped from the shared
// test address space (the frames themselves belong to the process, which frees
// them when it is dropped).
extern "C" fn user_stack_probe_thread_body(pages: u64) {
	use core::sync::atomic::Ordering;
	use mem::frame::{self, PAGE_SIZE};
	let code = frame::allocate().expect("user code frame");
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
	arch::paging::map_page(USER_CODE_VA, code, flags);
	let program = arch::usermode::program_stack_probe_bytes();
	unsafe {
		arch::paging::copy_to_user_page(USER_CODE_VA, program);
		arch::usermode::enter(USER_CODE_VA, memlayout::USER_STACK_TOP, pages);
	}
	let mut info = fault::FaultInfo { kind: 0, error_code: 0, address: 0, instruction_pointer: 0 };
	let got = unsafe { arch::syscall::invoke(syscall::SYS_FAULT_INFO_GET, &mut info as *mut fault::FaultInfo as u64, core::mem::size_of::<fault::FaultInfo>() as u64, 0, 0) };
	STACK_GOT.store(got as i64, Ordering::SeqCst);
	STACK_KIND.store(info.kind, Ordering::SeqCst);
	STACK_ADDR.store(info.address, Ordering::SeqCst);
	STACK_CODE.store(info.error_code, Ordering::SeqCst);
	if let Some(thread) = sched::current_thread() {
		STACK_USED.store(thread.process().domain().account().stack().used(), Ordering::SeqCst);
	}
	arch::paging::unmap_page(USER_CODE_VA);
	unsafe { frame::deallocate(code) };
	// Unmap whatever the probe grew (up to the whole requested span; pages past
	// the kill point were never mapped and unmap is a no-op there).
	for i in 1..=pages {
		arch::paging::unmap_page(memlayout::USER_STACK_TOP - i * PAGE_SIZE);
	}
}

// Kernel-thread body that drops to ring 3 running the no-execute probe: the
// program jumps into its writable, no-execute stack page, so the instruction
// fetch itself must page-fault (W^X). Mirrors user_fault_thread_body.
extern "C" fn user_nx_thread_body(_arg: u64) {
	use core::sync::atomic::Ordering;
	use mem::frame::{self, PAGE_SIZE};
	let code = frame::allocate().expect("user code frame");
	let stack = frame::allocate().expect("user stack frame");
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
	arch::paging::map_page(USER_CODE_VA, code, flags);
	arch::paging::map_page(USER_STACK_VA, stack, flags | arch::paging::NO_EXECUTE);
	let program = arch::usermode::program_nx_bytes();
	unsafe {
		arch::paging::copy_to_user_page(USER_CODE_VA, program);
		arch::usermode::enter(USER_CODE_VA, USER_STACK_VA + PAGE_SIZE, 0);
	}
	let mut info = fault::FaultInfo { kind: 0, error_code: 0, address: 0, instruction_pointer: 0 };
	let got = unsafe { arch::syscall::invoke(syscall::SYS_FAULT_INFO_GET, &mut info as *mut fault::FaultInfo as u64, core::mem::size_of::<fault::FaultInfo>() as u64, 0, 0) };
	NX_GOT.store(got as i64, Ordering::SeqCst);
	NX_KIND.store(info.kind, Ordering::SeqCst);
	NX_ADDR.store(info.address, Ordering::SeqCst);
	NX_CODE.store(info.error_code, Ordering::SeqCst);
	arch::paging::unmap_page(USER_CODE_VA);
	arch::paging::unmap_page(USER_STACK_VA);
	unsafe { frame::deallocate(code) };
	unsafe { frame::deallocate(stack) };
}

// A DeviceManager privilege in the calling thread's handle table.
//
// `SYS_DEVICE_ACQUIRE`, `SYS_DEVICE_MSIX_ACQUIRE` and `SYS_INTERRUPT_BIND` require one: ungated,
// they minted a capability to any device's BAR for any caller that named an index. The suite drives
// syscalls from kernel threads and there is no ring-0 exemption - a gate every test walks around is
// a gate nobody has walked through - so a test that means to acquire a device holds the authority
// like the one program that legitimately does.
fn device_privilege() -> u64 {
	use object::privilege::{Privilege, PrivilegeKind};
	let thread = sched::current_thread().expect("a current thread");
	let privilege = Privilege::create(PrivilegeKind::DeviceManager).expect("a test privilege");
	thread.handles().lock().try_insert_object(privilege, object::rights::Rights::ALL, 0).expect("the privilege installs").raw()
}

// Kernel-thread body for the driver-crash test: it acquires real driver resources
// - a bound IRQ and a DMA buffer - then drops to ring 3 and faults, leaving both
// open so the kernel's crash cleanup is what detaches the IRQ and refunds the DMA.
// Mirrors user_fault_thread_body's ring-3 fault, plus the held driver resources.
// x86-only: it binds a legacy INTx vector, which aarch64 (MSI-only) does not offer.
#[cfg(target_arch = "x86_64")]
extern "C" fn driver_crash_thread_body(_arg: u64) {
	use mem::frame::{self, PAGE_SIZE};
	unsafe {
		let irq = arch::syscall::invoke(syscall::SYS_INTERRUPT_BIND, DRIVER_IRQ_VECTOR, device_privilege(), 0, 0);
		assert!((irq as i64) > 0, "driver should bind its IRQ");
		let dma = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_CREATE, PAGE_SIZE, 0, 0, 0);
		assert!((dma as i64) > 0, "driver should create its DMA buffer");
	}
	let code = frame::allocate().expect("user code frame");
	let stack = frame::allocate().expect("user stack frame");
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
	arch::paging::map_page(USER_CODE_VA, code, flags);
	arch::paging::map_page(USER_STACK_VA, stack, flags | arch::paging::NO_EXECUTE);
	let program = arch::usermode::program_fault_bytes();
	unsafe {
		arch::paging::copy_to_user_page(USER_CODE_VA, program);
		arch::usermode::enter(USER_CODE_VA, USER_STACK_VA + PAGE_SIZE, 0);
	}
	// Back from the crash: drop the raw code/stack mappings. The IRQ and DMA handles
	// stay open, so the kernel's process teardown is what releases them.
	arch::paging::unmap_page(USER_CODE_VA);
	arch::paging::unmap_page(USER_STACK_VA);
	unsafe { frame::deallocate(code) };
	unsafe { frame::deallocate(stack) };
}

// A kernel thread that holds a resource and parks until its Domain is killed. It
// opens a MemoryObject (charged to its Domain) and then yields forever; once its
// Domain is killed, it observes the kill at the next yield and exits, releasing
// the object. Used by the domain-kill test.
extern "C" fn domain_parker(_arg: u64) {
	let _mo = unsafe { arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, mem::frame::PAGE_SIZE, 0, 0, 0) };
	loop {
		sched::yield_now();
	}
}

// test harness (custom_test_frameworks, runs under `cargo test` in QEMU)
pub(crate) trait Testable {
	fn run(&self);
	fn tags(&self) -> &'static [TestTag];
	// The stable identity an exact selection names. Defaults to the function name; `id = "..."`
	// overrides it so a rename does not zero the test's history.
	fn id(&self) -> &'static str;
}

macro_rules! define_test_tags {
	($($variant:ident => $name:literal),+ $(,)?) => {
		#[derive(Clone, Copy, PartialEq, Eq)]
		pub(crate) enum TestTag {
			$($variant),+
		}

		impl TestTag {
			const ALL: &'static [Self] = &[$(Self::$variant),+];

			const fn as_str(self) -> &'static str {
				match self {
					$(Self::$variant => $name),+
				}
			}

			fn parse(value: &str) -> Option<Self> {
				Self::ALL.iter().copied().find(|tag| tag.as_str() == value)
			}
		}
	};
}

define_test_tags! {
	Apic => "apic",
	ArchAarch64 => "arch-aarch64",
	ArchRiscv64 => "arch-riscv64",
	ArchX86_64 => "arch-x86_64",
	Audio => "audio",
	AudioService => "audio-service",
	Boot => "boot",
	Channel => "channel",
	Component => "component",
	Console => "console",
	Config => "config",
	Dma => "dma",
	Display => "display",
	Domain => "domain",
	Drivers => "drivers",
	Dynamic => "dynamic",
	DynamicReject => "dynamic-reject",
	Filesystem => "filesystem",
	Frame => "frame",
	Handle => "handle",
	Image => "image",
	Idt => "idt",
	Imgview => "imgview",
	Input => "input",
	Interrupt => "interrupt",
	Ipc => "ipc",
	Kernel => "kernel",
	Lico => "lico",
	LicoLoad => "lico-load",
	Memory => "memory",
	Mouse => "mouse",
	Network => "network",
	Object => "object",
	Paging => "paging",
	Pci => "pci",
	Process => "process",
	ProcessService => "process-service",
	PermissionService => "permission-service",
	Scheduler => "scheduler",
	Service => "service",
	Shell => "shell",
	Smp => "smp",
	Smoke => "smoke",
	Slow => "slow",
	Storage => "storage",
	Stress => "stress",
	Syscall => "syscall",
	Usb => "usb",
	VolumeLayout => "volume-layout",
	VolumeScope => "volume-scope",
}

pub(crate) struct TaggedTest {
	pub(crate) name: &'static str,
	// The identity everything OUTSIDE the guest keys on: age, cost, shadow and regression history.
	//
	// It defaults to the function name and can be overridden with `id = "..."`, and the difference
	// matters the day a test is renamed - a name-derived identity zeroes all four records, silently,
	// and the graph of what has been verified starts again from nothing. An explicit id survives the
	// rename; changing one is then a deliberate act rather than a side effect of tidying.
	pub(crate) id: &'static str,
	pub(crate) tags: &'static [TestTag],
	pub(crate) run: fn(),
}

impl Testable for TaggedTest {
	fn id(&self) -> &'static str {
		self.id
	}

	fn run(&self) {
		// The ID, not the function name. Everything else that names a test - an exact selection, a
		// plan key, a history record, a shadow comparison - uses the id, and a log that used the
		// other name meant the shadow comparison had to STRIP a prefix to line the two up. That
		// worked only while the two strings differed by exactly that prefix; the moment ids became
		// namespaced it would have silently matched nothing, which is a comparison that reports
		// agreement because it compared nothing.
		serial_print!("{}...\t", self.id);
		// The clock, read either side, so "slow" and "stuck" stop being a judgement call somebody
		// makes by watching a log.
		//
		// They were exactly that, and it cost two hours: a riscv64 run that stopped producing output
		// for ten minutes was read as a livelock and chased through four subsystems, when the region
		// it was in genuinely takes minutes per test. The evidence that would have settled it in one
		// glance - how long the LAST test took - was never on the line.
		//
		// Printed only when it is worth reading. A suite where every line carries `(0.2 s)` is a
		// suite nobody reads the timings in, and the emulated targets are where the question arises.
		let started = crate::arch::tsc::now();
		(self.run)();
		let elapsed_ns = crate::arch::tsc::cycles_to_ns(crate::arch::tsc::now().wrapping_sub(started));
		// The frame allocator's two views of the pool, compared after every test.
		//
		// A bitmap allocator can hand one page to two owners, and the run table it replaced could
		// not - so this is a class of defect the suite had never had to look for. It does not show
		// up where it happens: it showed up once as an ELF header of sixteen zero bytes, in a
		// different test, four hundred allocations later. Comparing here costs a few milliseconds
		// and names the test that did it.
		if let Some((pages, first)) = crate::mem::frame::audit() {
			serial_println!("[failed]");
			panic!("the frame allocator's bitmap and its ownership record disagree about {pages} page(s) after this test, first at {first:#x}");
		}
		// One second is the threshold: below it the number is noise, above it the number is the
		// reason the suite is taking as long as it is.
		if elapsed_ns >= 1_000_000_000 {
			serial_println!("[ok] ({} s)", elapsed_ns / 1_000_000_000);
		} else {
			serial_println!("[ok]");
		}
	}

	fn tags(&self) -> &'static [TestTag] {
		self.tags
	}
}

// `covers` says what a test would CATCH, and it is read on the host rather than in the guest.
//
// `covers: X` means the test contains an assertion able to detect a regression in X's contract. It
// does NOT mean the execution path goes through X: every integration test here runs the scheduler,
// the allocator, IPC and the loader, so if touching counted, every test would cover everything and
// a selection would be the full suite wearing new metadata.
//
// The names are string literals because components are not Rust identifiers - `bin.audioconv`,
// `base-proto`. Nothing in the guest reads them; `verify-model` parses them out of this source and
// checks each one against the component graph, so a typo fails the model's gate rather than
// silently narrowing a selection. They cost no image bytes.
//
// The clause is OPTIONAL, and that is the migration rather than an oversight: a test with no
// `covers` is treated as covering everything and is therefore ALWAYS selected. Annotating a test
// can only ever make the suite cheaper, never less safe, so this can be done a file at a time
// instead of as one change to 209 call sites.
#[macro_export]
macro_rules! tagged_test {
	// One internal rule builds the descriptor and the four public shapes feed it, so there is
	// exactly ONE `#[test_case]` in this file however many optional clauses exist. The alternative -
	// an arm per combination - grows a descriptor per arm, and `check-test-tags.sh` exists precisely
	// to notice a `#[test_case]` that the macro did not generate.
	(@build $(#[$attr:meta])* $name:ident, [$first_tag:ident $(, $tag:ident)* $(,)?], $id:expr, $covers:expr) => {
		$(#[$attr])*
		mod $name {
			// Named and unused: it makes the covers literals part of the compile, so a malformed
			// list is a build error here rather than a parse failure on the host.
			#[allow(dead_code)]
			const COVERS: &[&str] = $covers;
			#[test_case]
			static CASE: $crate::tests::TaggedTest = $crate::tests::TaggedTest {
				name: stringify!($name),
				id: $id,
				tags: &[$crate::tests::TestTag::$first_tag $(, $crate::tests::TestTag::$tag)*],
				run: super::$name,
			};
		}
	};
	// There is deliberately NO arm without `id`. It used to default to `stringify!($name)`, and all
	// 209 tests took the default - so a test's identity WAS its function name, and renaming one
	// retired its history and started a fresh key with no evidence behind it. Not a false green: the
	// loss is of trust rather than of coverage. But the design called for an identity that a rename
	// cannot destroy, and a default nobody overrides is not one.
	//
	// A missing `id` is now a compile error naming the test, which is the only enforcement that
	// cannot be forgotten.
	($(#[$attr:meta])* $name:ident, [$first_tag:ident $(, $tag:ident)* $(,)?], id = $id:literal) => {
		$crate::tagged_test!(@build $(#[$attr])* $name, [$first_tag $(, $tag)*], $id, &[]);
	};
	($(#[$attr:meta])* $name:ident, [$first_tag:ident $(, $tag:ident)* $(,)?], id = $id:literal, covers = [$($covers:literal),* $(,)?]) => {
		$crate::tagged_test!(@build $(#[$attr])* $name, [$first_tag $(, $tag)*], $id, &[$($covers),*]);
	};
}

pub(crate) fn test_runner(tests: &[&dyn Testable]) {
	// Exact selection first: a list of stable IDs, which is the machine's execution interface.
	//
	// Tags are the human one and they answer a different question - "everything to do with audio" -
	// which is why they cannot express a selector's answer. `verify.sh` computes an exact set of
	// PlanItemKeys and had no way to hand it over, so a scoped kernel selection ran the whole suite:
	// safe, and the reason the selection dimension paid nothing.
	//
	// An unknown ID is a HARD FAILURE. Silently running fewer tests than were asked for is the same
	// false green this whole mechanism exists against, and a renamed test is exactly how a selection
	// comes to name something that no longer exists.
	if let Some(list) = option_env!("TEST_SELECTION").filter(|value| !value.trim().is_empty()) {
		let wanted: Vec<&str> = list.split(',').map(str::trim).filter(|value| !value.is_empty()).collect();
		for name in &wanted {
			if !tests.iter().any(|test| test.id() == *name) {
				serial_println!("test selection error: no test with id '{name}'");
				arch::exit_qemu(false);
			}
		}
		let mut ran = 0usize;
		serial_println!("running {} selected tests ({} total)", wanted.len(), tests.len());
		for test in tests {
			if wanted.contains(&test.id()) {
				test.run();
				ran += 1;
			}
		}
		if ran != wanted.len() {
			serial_println!("test selection error: asked for {} tests and ran {ran}", wanted.len());
			arch::exit_qemu(false);
		}
		serial_println!("test suite complete: {ran} passed");
		arch::exit_qemu(true);
	}

	let Some(filter) = option_env!("TEST_TAGS").filter(|value| !value.trim().is_empty()) else {
		serial_println!("running {} tests (all tags)", tests.len());
		for test in tests {
			test.run();
		}
		serial_println!("test suite complete: {} passed", tests.len());
		arch::exit_qemu(true);
	};

	let mut requested: Vec<TestTag> = Vec::new();
	for value in filter.split(',').map(str::trim) {
		let Some(tag) = TestTag::parse(value) else {
			serial_println!("test filter error: unknown tag '{value}'");
			arch::exit_qemu(false);
		};
		if !requested.contains(&tag) {
			requested.push(tag);
		}
	}
	serial_print!("test tags: requested={filter}, effective={filter}");
	if !requested.contains(&TestTag::Smoke) {
		serial_print!(",smoke");
	}
	serial_println!();

	let allow_slow = requested.contains(&TestTag::Slow);
	let allow_stress = requested.contains(&TestTag::Stress);
	let mut selected = 0usize;
	let mut selected_non_smoke = 0usize;
	for test in tests {
		let tags = test.tags();
		let gated = (tags.contains(&TestTag::Slow) && !allow_slow) || (tags.contains(&TestTag::Stress) && !allow_stress);
		let requested_match = tags.iter().any(|tag| requested.contains(tag));
		let smoke_match = tags.contains(&TestTag::Smoke);
		if !gated && (requested_match || smoke_match) {
			selected += 1;
			if requested_match && tags.iter().any(|tag| *tag != TestTag::Smoke) {
				selected_non_smoke += 1;
			}
		}
	}
	if selected_non_smoke == 0 {
		serial_println!("test filter error: requested tags selected no non-smoke tests");
		arch::exit_qemu(false);
	}
	serial_println!("running {selected} tests ({} skipped, {} total)", tests.len() - selected, tests.len());
	for test in tests {
		let tags = test.tags();
		let gated = (tags.contains(&TestTag::Slow) && !allow_slow) || (tags.contains(&TestTag::Stress) && !allow_stress);
		if !gated && (tags.contains(&TestTag::Smoke) || tags.iter().any(|tag| requested.contains(tag))) {
			test.run();
		}
	}
	serial_println!("test suite complete: {selected} passed");
	arch::exit_qemu(true);
}

#[cfg(target_arch = "x86_64")]
const TEST_ELF_MACHINE: u16 = 62;
#[cfg(target_arch = "aarch64")]
const TEST_ELF_MACHINE: u16 = 183;
#[cfg(target_arch = "riscv64")]
const TEST_ELF_MACHINE: u16 = 243;

#[cfg(target_arch = "x86_64")]
const TEST_RELATIVE_RELOCATION: u32 = 8;
#[cfg(target_arch = "aarch64")]
const TEST_RELATIVE_RELOCATION: u32 = 1027;
#[cfg(target_arch = "riscv64")]
const TEST_RELATIVE_RELOCATION: u32 = 3;

#[cfg(target_arch = "x86_64")]
const TEST_IMPORT_RELOCATION: u32 = 6;
#[cfg(target_arch = "aarch64")]
const TEST_IMPORT_RELOCATION: u32 = 1026;
#[cfg(target_arch = "riscv64")]
const TEST_IMPORT_RELOCATION: u32 = 5;

const fn elf_machine() -> u16 {
	TEST_ELF_MACHINE
}

const fn relative_relocation_type() -> u32 {
	TEST_RELATIVE_RELOCATION
}

const fn import_relocation_type() -> u32 {
	TEST_IMPORT_RELOCATION
}

// Spawn a userspace service from the init package and hand it the channel its
// clients reach it on ("SERVE"). Returns (boot_kernel, service_client): the report
// channel the kernel reads the service's "online" report on, and the client end the
// kernel-as-client drives the generated bindings over. The shared setup of the
// service integration tests.
fn spawn_service(name: &[u8]) -> (alloc::sync::Arc<object::channel::Channel>, alloc::sync::Arc<object::channel::Channel>) {
	use object::channel::Channel;
	use object::rights::Rights;
	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let service_elf = program_elf(&package, volume, name).expect("service in the init package or volume");
	let (boot_kernel, boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();
	let _service = if bootproto::elf::Elf::parse(service_elf).is_some_and(|elf| elf.image_type == bootproto::elf::ET_DYN) {
		Some(spawn_dynamic_test_process(sched::root_domain(), service_elf, boot_user))
	} else {
		loader::spawn_elf_process(sched::root_domain(), service_elf, boot_user, Rights::ALL, 0).expect("spawn service");
		None
	};
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");
	(boot_kernel, service_client)
}

// Like `spawn_service`, but also hands the service a read-only copy of the init
// package ("PACKAGE" + length) and a placeholder "STORAGE" message with no client
// (so it falls back to loading from that package) before the serve channel - the
// bootstrap a service that launches programs (ProcessService) needs.
fn spawn_service_with_package(name: &[u8]) -> (alloc::sync::Arc<object::channel::Channel>, alloc::sync::Arc<object::channel::Channel>) {
	use object::channel::{Channel, Message};
	use object::rights::Rights;
	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let service_elf = program_elf(&package, volume, name).expect("service in the init package or volume");
	let (boot_kernel, boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), service_elf, boot_user, Rights::ALL, 0).expect("spawn service");
	let pkg_obj = object::memory_object::MemoryObject::create(init.len()).expect("memory for the package");
	copy_into_object(&pkg_obj, init);
	let mut pkg_msg = alloc::vec::Vec::new();
	pkg_msg.extend_from_slice(b"PACKAGE");
	pkg_msg.extend_from_slice(&(init.len() as u64).to_le_bytes());
	send_cap(&boot_kernel, &pkg_msg, pkg_obj, Rights::READ | Rights::MAP | Rights::TRANSFER).expect("package bootstrap");
	// A "STORAGE" message carrying no client (handle 0): ProcessService reads it, finds
	// no storage client, and loads programs from the package instead.
	boot_kernel.send(Message::new(b"STORAGE".to_vec(), alloc::vec::Vec::new(), 0)).expect("storage bootstrap");
	// The same for "REGISTRY": no development registry answers here, so every launch
	// reads the volume. The message still has to arrive - the bootstrap reads its
	// handoffs in order and each read consumes whatever arrived, so a skipped one is
	// not skipped at all, it swallows the next message and then blocks forever.
	boot_kernel.send(Message::new(b"REGISTRY".to_vec(), alloc::vec::Vec::new(), 0)).expect("registry bootstrap");
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");
	(boot_kernel, service_client)
}

// Little-endian field readers for decoding the proto reply bytes in the tests.
fn le_u16(b: &[u8], off: usize) -> u16 {
	u16::from_le_bytes(b[off..off + 2].try_into().unwrap())
}
fn le_u32(b: &[u8], off: usize) -> u32 {
	u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}
fn le_u64(b: &[u8], off: usize) -> u64 {
	u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

#[derive(Clone, Copy)]
enum AudioServiceScenario {
	ScopeAndMixing,
	Backpressure,
	Mp3Continuity,
	DriverFailure,
}

fn run_audio_service_scenario(scenario: AudioServiceScenario) {
	use object::channel::{Channel, Message};
	use object::memory_object::MemoryObject;
	use object::rights::Rights;

	fn open(root: &Channel, corr: u32, rate: u32, channels: u8) -> Result<alloc::sync::Arc<Channel>, u8> {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&2u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&rate.to_le_bytes());
		request.push(channels);
		root.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("open-stream request");
		sched::run_until_idle();
		let reply = root.recv().expect("open-stream reply");
		assert_eq!(le_u32(&reply.bytes, 0), corr);
		if reply.bytes[4] == 0 {
			return Err(reply.bytes[5]);
		}
		let cap = reply.caps.first().expect("PCM stream channel");
		Ok(cap.object().into_any_arc().downcast::<Channel>().expect("PCM stream is a channel"))
	}

	fn open_scope(admin: &Channel, corr: u32) -> alloc::sync::Arc<Channel> {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&1u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		admin.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("open playback-only connection");
		sched::run_until_idle();
		let reply = admin.recv().expect("playback-only connection reply");
		assert_eq!(le_u32(&reply.bytes, 0), corr);
		assert_eq!(reply.bytes[4], 1, "playback-only connection succeeds");
		reply.caps.first().expect("playback-only connection").object().into_any_arc().downcast::<Channel>().expect("audio-stream grant is a channel")
	}

	fn launch_play(process_service: &Channel, storage: alloc::sync::Arc<Channel>, audio: alloc::sync::Arc<Channel>, argument: &[u8]) -> (alloc::sync::Arc<Channel>, alloc::sync::Arc<object::process::Process>) {
		let (bootstrap, child) = Channel::create();
		let (stdout, child_stdout) = Channel::create();
		let mut launch = alloc::vec::Vec::new();
		launch.extend_from_slice(&4u16.to_le_bytes());
		launch.extend_from_slice(&1u32.to_le_bytes());
		launch.extend_from_slice(&4u16.to_le_bytes());
		launch.extend_from_slice(b"play");
		launch.extend_from_slice(&(128u64 * 1024 * 1024).to_le_bytes());
		launch.extend_from_slice(&0u32.to_le_bytes());
		send_cap(process_service, &launch, child, Rights::ALL).expect("bounded play launch request");
		sched::run_until_idle();
		let reply = process_service.recv().expect("bounded play launch reply");
		assert_eq!(le_u32(&reply.bytes, 0), 1);
		assert_eq!(reply.bytes[4], 1, "dynamic play loaded with its providers");
		let process = reply.caps[0].object().into_any_arc().downcast::<object::process::Process>().expect("play launch returns a Process");
		send_cap(&bootstrap, b"STDOUT", child_stdout, Rights::ALL).expect("play stdout bootstrap");
		bootstrap.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
		bootstrap.send(Message::new(launch_context(argument, b"vol://system"), alloc::vec::Vec::new(), 0)).expect("play argument bootstrap");
		send_cap(&bootstrap, b"SYSTEM", storage, Rights::ALL).expect("play system volume bootstrap");
		for tag in [b"MEDIA".as_slice(), b"ISO".as_slice(), b"UDF".as_slice(), b"USB".as_slice(), b"RAM".as_slice(), b"TMP".as_slice()] {
			bootstrap.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("play absent volume bootstrap");
		}
		bootstrap.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("volume bundle terminator");
		send_cap(&bootstrap, b"AUDIO_STREAM", audio, Rights::ALL).expect("play audio-stream bootstrap");
		bootstrap.send(Message::new(b"vol://system".to_vec(), alloc::vec::Vec::new(), 0)).expect("play cwd bootstrap");
		(stdout, process)
	}

	fn pcm(frames: usize, channels: usize, sample: i16) -> alloc::vec::Vec<u8> {
		let mut bytes = alloc::vec::Vec::with_capacity(frames * channels * 2);
		for _ in 0..frames * channels {
			bytes.extend_from_slice(&sample.to_le_bytes());
		}
		bytes
	}

	fn send_write(stream: &Channel, corr: u32, bytes: &[u8]) {
		let object = MemoryObject::create(bytes.len()).expect("PCM MemoryObject");
		copy_into_object(&object, bytes);
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&1u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
		send_cap(stream, &request, object, Rights::READ | Rights::MAP | Rights::TRANSFER).expect("PCM write request");
	}

	fn write_reply(stream: &Channel, corr: u32, frames: u32) {
		let reply = stream.recv().expect("PCM write reply");
		assert_eq!(le_u32(&reply.bytes, 0), corr);
		assert_eq!(reply.bytes[4], 1, "PCM write succeeds");
		assert_eq!(le_u32(&reply.bytes, 5), frames, "accepted source frames");
	}

	fn close_stream(stream: &Channel, corr: u32) {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&2u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		stream.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("PCM close request");
	}

	fn sample(message: &Message) -> i16 {
		i16::from_le_bytes([message.bytes[0], message.bytes[1]])
	}

	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let service_elf = program_elf(&package, volume, b"audio_service").expect("audio_service in the package or volume");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe in init package");
	let process_elf = package.lookup(b"process_service.lsexe").expect("process_service.lsexe in init package");
	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	let (process_boot_kernel, process_boot_user) = Channel::create();
	let (process_server, process_client) = Channel::create();
	let (boot_kernel, boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();
	let (snd_host, snd_service) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), storage_elf, storage_boot_user, Rights::ALL, 0).expect("spawn StorageService");
	loader::spawn_elf_process(sched::root_domain(), process_elf, process_boot_user, Rights::ALL, 0).expect("spawn ProcessService");
	let _audio_service = spawn_dynamic_test_process(sched::root_domain(), service_elf, boot_user);
	send_ramdisk(&storage_boot_kernel, volume).expect("storage ramdisk bootstrap");
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL).expect("storage serve bootstrap");
	send_package(&process_boot_kernel, init).expect("process package bootstrap");
	// The development registry channel. ProcessService receives it unconditionally, so it must be
	// sent even here where nothing will ever answer on it: an absent one blocks that service in
	// its bootstrap, and every launch after it waits on a reply that cannot come.
	let (registry_server, registry_client) = Channel::create();
	core::mem::drop(registry_server);
	send_cap(&process_boot_kernel, b"STORAGE", storage_client.clone(), Rights::ALL).expect("process storage bootstrap");
	send_cap(&process_boot_kernel, b"REGISTRY", registry_client, Rights::ALL).expect("process registry bootstrap");
	send_cap(&process_boot_kernel, b"SERVE", process_server, Rights::ALL).expect("process serve bootstrap");
	send_cap(&boot_kernel, b"SND", snd_service, Rights::ALL).expect("snd bootstrap");
	let (audio_admin, admin) = Channel::create();
	send_cap(&boot_kernel, b"ADMIN", admin, Rights::ALL).expect("audio admin bootstrap");
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");
	sched::run_until_idle();
	let storage_online = storage_boot_kernel.recv().expect("StorageService online report");
	assert_eq!(&storage_online.bytes[..], b"StorageService: online");
	let online = boot_kernel.recv().expect("AudioService online report");
	assert_eq!(&online.bytes[..], b"AudioService: online");
	match scenario {
		AudioServiceScenario::ScopeAndMixing => {
			assert!(open(&service_client, 1, 4_000, 1).is_err(), "unsupported sample rate is refused");
			let scoped = open_scope(&audio_admin, 30);
			let mut denied_beep = alloc::vec::Vec::new();
			denied_beep.extend_from_slice(&1u16.to_le_bytes());
			denied_beep.extend_from_slice(&31u32.to_le_bytes());
			denied_beep.extend_from_slice(&440u16.to_le_bytes());
			denied_beep.extend_from_slice(&10u32.to_le_bytes());
			scoped.send(Message::new(denied_beep, alloc::vec::Vec::new(), 0)).expect("scoped beep request");
			sched::run_until_idle();
			let denied = scoped.recv().expect("scoped beep denial");
			assert_eq!(denied.bytes[4], 0, "audio-stream scope denies beep");
			let scoped_stream = open(&scoped, 32, 48_000, 2).expect("audio-stream scope permits playback");
			drop(scoped_stream);

			let stereo = open(&service_client, 2, 48_000, 2).expect("48 kHz stereo stream");
			let mono = open(&service_client, 3, 24_000, 1).expect("24 kHz mono stream");
			send_write(&stereo, 4, &pcm(1_536, 2, 30_000));
			sched::run_until_idle();
			write_reply(&stereo, 4, 1_536);
			let first = snd_host.recv().expect("first hardware period");
			assert_eq!(first.bytes.len(), 2_048);
			assert_eq!(sample(&first), 30_000, "first stream plays alone");

			send_write(&mono, 5, &pcm(512, 1, 3_000));
			let mut beep = alloc::vec::Vec::new();
			beep.extend_from_slice(&1u16.to_le_bytes());
			beep.extend_from_slice(&6u32.to_le_bytes());
			beep.extend_from_slice(&1_000u16.to_le_bytes());
			beep.extend_from_slice(&30u32.to_le_bytes());
			service_client.send(Message::new(beep, alloc::vec::Vec::new(), 0)).expect("beep request");
			sched::run_until_idle();
			write_reply(&mono, 5, 512);
			let beep_reply = service_client.recv().expect("beep reply");
			assert_eq!(beep_reply.bytes[4], 1, "beep queues into the mixer");

			snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("first period ACK");
			sched::run_until_idle();
			let second = snd_host.recv().expect("mixed second period");
			assert_eq!(sample(&second), i16::MAX, "two streams plus beep saturate instead of wrapping");
			snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("second period ACK");
			sched::run_until_idle();
			let third = snd_host.recv().expect("resampled third period");
			assert_eq!(sample(&third), 27_000, "24 kHz mono is duplicated and survives for two output periods");

			close_stream(&stereo, 7);
			close_stream(&mono, 8);
			sched::run_until_idle();
			assert_eq!(stereo.recv().expect("stereo close reply").bytes[4], 1);
			assert_eq!(mono.recv().expect("mono close reply").bytes[4], 1);
			snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("third period ACK");
			sched::run_until_idle();
			let fourth = snd_host.recv().expect("beep tail period");
			assert_eq!(sample(&fourth), 6_000, "beep continues through the shared mixer after streams drain");
			snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("fourth period ACK");
			sched::run_until_idle();
			let stop = snd_host.recv().expect("hardware stop sentinel");
			assert!(stop.bytes.is_empty(), "idle mixer releases the hardware stream");
			snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("stop ACK");
			sched::run_until_idle();
		}
		AudioServiceScenario::Backpressure => {
			let bounded = open(&service_client, 9, 48_000, 2).expect("bounded stream");
			send_write(&bounded, 10, &pcm(4_096, 2, 100));
			sched::run_until_idle();
			write_reply(&bounded, 10, 4_096);
			let period = snd_host.recv().expect("bounded period one");
			assert_eq!(sample(&period), 100);
			send_write(&bounded, 11, &pcm(512, 2, 100));
			sched::run_until_idle();
			write_reply(&bounded, 11, 512);
			send_write(&bounded, 12, &pcm(512, 2, 100));
			sched::run_until_idle();
			assert!(bounded.recv().is_err(), "full queue defers the write reply");
			snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("bounded period one ACK");
			sched::run_until_idle();
			let period = snd_host.recv().expect("bounded period two");
			assert_eq!(sample(&period), 100);
			assert!(bounded.recv().is_err(), "one ACK has not yet made bounded capacity visible");
			snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("bounded period two ACK");
			sched::run_until_idle();
			write_reply(&bounded, 12, 512);
			let period = snd_host.recv().expect("bounded period three");
			assert_eq!(sample(&period), 100);
			drop(bounded);
			snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("bounded period three ACK");
			sched::run_until_idle();
			let stop = snd_host.recv().expect("peer-close stop sentinel");
			assert!(stop.bytes.is_empty(), "peer-close drops queued source frames before another period");
			snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("peer-close stop ACK");
			sched::run_until_idle();
		}
		AudioServiceScenario::Mp3Continuity => {
			let mp3_scope = open_scope(&audio_admin, 43);
			let (_mp3_stdout, mp3_process) = launch_play(&process_client, storage_client, mp3_scope, b"vol://system/audio/test.mp3");
			sched::run_until_idle();
			let mut mp3_period = snd_host.recv().expect("MP3 first hardware period");
			assert!(!mp3_period.bytes.is_empty(), "MP3 starts with an audio period");
			let mut mp3_periods = 1u32;
			while mp3_periods < 12 {
				snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("MP3 period ACK");
				sched::run_until_idle();
				mp3_period = snd_host.recv().expect("next MP3 period");
				assert!(!mp3_period.bytes.is_empty(), "MP3 queue underrun stopped the hardware stream");
				mp3_periods += 1;
			}
			mp3_process.set_int_pending();
			for thread in mp3_process.live_threads() {
				sched::wake_thread(&thread);
			}
			let mut mp3_tail = 0u32;
			loop {
				snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("MP3 tail ACK");
				sched::run_until_idle();
				mp3_period = snd_host.recv().expect("MP3 tail period or stop");
				if mp3_period.bytes.is_empty() {
					break;
				}
				mp3_tail += 1;
				assert!(mp3_tail <= 64, "interrupted MP3 leaves at most the bounded accepted queue tail");
			}
			snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("MP3 stop ACK");
			sched::run_until_idle();
			assert!(mp3_process.is_terminated(), "interrupted MP3 player closes and exits");
		}
		AudioServiceScenario::DriverFailure => {
			let doomed = open(&service_client, 13, 48_000, 2).expect("stream before driver crash");
			send_write(&doomed, 14, &pcm(512, 2, 200));
			sched::run_until_idle();
			write_reply(&doomed, 14, 512);
			let period = snd_host.recv().expect("period pending at driver crash");
			assert_eq!(sample(&period), 200);
			drop(snd_host);
			sched::run_until_idle();
			assert!(doomed.is_peer_closed(), "driver crash closes live PCM streams");
			assert!(open(&service_client, 15, 48_000, 2).is_err(), "driver crash makes future opens fail");
		}
	}
}

fn run_process_service_requests(starts: &[(u32, &[u8])], list_correlation: Option<u32>) -> (alloc::vec::Vec<alloc::vec::Vec<u8>>, Option<alloc::vec::Vec<u8>>) {
	use object::channel::Message;

	let (boot_kernel, service_client) = spawn_service_with_package(b"process_service");
	for &(correlation, name) in starts {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&1u16.to_le_bytes());
		request.extend_from_slice(&correlation.to_le_bytes());
		request.extend_from_slice(&(name.len() as u16).to_le_bytes());
		request.extend_from_slice(name);
		service_client.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("start request");
	}
	if let Some(correlation) = list_correlation {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&2u16.to_le_bytes());
		request.extend_from_slice(&correlation.to_le_bytes());
		service_client.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("list request");
	}
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");
	sched::run_until_idle();
	let online = boot_kernel.recv().expect("ProcessService online report");
	assert_eq!(&online.bytes[..], b"ProcessService: online", "ProcessService reports in");
	let replies = starts.iter().map(|_| service_client.recv().expect("start reply").bytes).collect();
	let list = list_correlation.map(|_| service_client.recv().expect("list reply").bytes);
	(replies, list)
}

// How many processes ProcessService reports as live right now. Separate from
// `run_process_service_requests` because that one drives a whole session and reads its
// replies at the end; asking the same question twice around a termination needs the answer
// in hand before the next step runs.
fn process_service_list_len(service_client: &alloc::sync::Arc<object::channel::Channel>, correlation: u32) -> u16 {
	use object::channel::Message;

	let mut request = alloc::vec::Vec::new();
	request.extend_from_slice(&2u16.to_le_bytes());
	request.extend_from_slice(&correlation.to_le_bytes());
	service_client.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("list request");
	sched::run_until_idle();
	let reply = service_client.recv().expect("list reply").bytes;
	assert_eq!(le_u32(&reply, 0), correlation, "list reply echoes the correlation id");
	assert_eq!(reply[4], 1, "list succeeded");
	le_u16(&reply, 5)
}

// The budgets ProcessService reports for the Domains it is accounting, as (name, memory limit)
// per budget. Decoded by hand against the generated wire form rather than through a client,
// because the kernel harness speaks to services over raw channels: a reply is a correlation,
// a success byte, a u16 count, then per budget a length-prefixed name and a u16-counted list
// of {type: u8, used: u64, limit: u64}. Only the memory line is returned, which is the one a
// launch limit is stated in.
fn process_service_accounting(service_client: &alloc::sync::Arc<object::channel::Channel>, correlation: u32) -> alloc::vec::Vec<(alloc::vec::Vec<u8>, u64)> {
	use object::channel::Message;

	let mut request = alloc::vec::Vec::new();
	request.extend_from_slice(&5u16.to_le_bytes());
	request.extend_from_slice(&correlation.to_le_bytes());
	service_client.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("accounting request");
	sched::run_until_idle();
	let reply = service_client.recv().expect("accounting reply").bytes;
	assert_eq!(le_u32(&reply, 0), correlation, "accounting reply echoes the correlation id");
	assert_eq!(reply[4], 1, "accounting succeeded");
	let mut out = alloc::vec::Vec::new();
	let mut at = 7usize;
	for _ in 0..le_u16(&reply, 5) {
		let name_len = le_u16(&reply, at) as usize;
		at += 2;
		let name = reply[at..at + name_len].to_vec();
		at += name_len;
		let lines = le_u16(&reply, at) as usize;
		at += 2;
		let mut memory_limit = 0u64;
		for _ in 0..lines {
			let kind = reply[at];
			let limit = le_u64(&reply, at + 9);
			if kind == 0 {
				memory_limit = limit;
			}
			at += 17;
		}
		out.push((name, memory_limit));
	}
	out
}

fn assert_process_start_reply(reply: &[u8], correlation: u32, artifact: &[u8]) {
	assert_eq!(le_u32(reply, 0), correlation, "start reply echoes the correlation id");
	assert_eq!(reply[4], 1, "start succeeded");
	assert!(le_u64(reply, 5) >= 1, "the started process has a koid");
	let name_len = le_u16(reply, 13) as usize;
	assert_eq!(&reply[15..15 + name_len], artifact, "the launch reports the canonical artifact name");
}

fn start_process_service_from_volume(volume: &[u8]) -> (alloc::sync::Arc<object::channel::Channel>, alloc::sync::Arc<object::channel::Channel>, alloc::sync::Arc<object::channel::Channel>) {
	use object::channel::Channel;
	use object::rights::Rights;

	let (_, package) = scenario_packages().expect("scenario packages");
	let init = init_package_bytes().expect("init package module not found");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe in the init package");
	let process_elf = package.lookup(b"process_service.lsexe").expect("process_service.lsexe in the init package");

	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (process_boot_kernel, process_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	let (process_server, process_client) = Channel::create();

	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).expect("spawn StorageService");
	loader::spawn_elf_process(domain, process_elf, process_boot_user, Rights::ALL, 0).expect("spawn ProcessService");
	send_ramdisk(&storage_boot_kernel, volume).expect("storage ramdisk bootstrap");
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL).expect("storage serve bootstrap");
	send_package(&process_boot_kernel, init).expect("process package bootstrap");
	// The development registry channel. ProcessService receives it unconditionally, so it must be
	// sent even here where nothing will ever answer on it: an absent one blocks that service in
	// its bootstrap, and every launch after it waits on a reply that cannot come.
	let (registry_server, registry_client) = Channel::create();
	core::mem::drop(registry_server);
	send_cap(&process_boot_kernel, b"STORAGE", storage_client.clone(), Rights::ALL).expect("process storage bootstrap");
	send_cap(&process_boot_kernel, b"REGISTRY", registry_client, Rights::ALL).expect("process registry bootstrap");
	send_cap(&process_boot_kernel, b"SERVE", process_server, Rights::ALL).expect("process serve bootstrap");
	(process_boot_kernel, storage_boot_kernel, process_client)
}

fn launch_dynamic_for_measurement(process_client: &alloc::sync::Arc<object::channel::Channel>, name: &[u8], correlation: u32) -> (alloc::sync::Arc<object::process::Process>, alloc::sync::Arc<object::channel::Channel>, u64) {
	use object::channel::Channel;
	use object::process::Process;
	use object::rights::Rights;

	let (bootstrap_kernel, bootstrap_user) = Channel::create();
	let mut request = alloc::vec::Vec::new();
	request.extend_from_slice(&3u16.to_le_bytes());
	request.extend_from_slice(&correlation.to_le_bytes());
	request.extend_from_slice(&(name.len() as u16).to_le_bytes());
	request.extend_from_slice(name);
	request.extend_from_slice(&0u32.to_le_bytes());
	let started = arch::tsc::now();
	send_cap(process_client, &request, bootstrap_user, Rights::ALL).expect("measured launch request");
	sched::run_until_idle();
	let reply = process_client.recv().expect("measured launch reply");
	assert_eq!(le_u32(&reply.bytes, 0), correlation);
	assert_eq!(reply.bytes[4], 1, "measured dynamic executable loaded");
	let process = reply.caps[0].object().into_any_arc().downcast::<Process>().expect("measured launch capability is a Process");
	let elapsed = arch::tsc::cycles_to_ns(arch::tsc::now().wrapping_sub(started));
	(process, bootstrap_kernel, elapsed)
}

// The footprint is asserted as a relation between two launches, never as an absolute page
// count. Counts are 4 kB quanta: adding a line of code grows a section by a few dozen bytes,
// which crosses a page boundary often enough that a pinned number fails on ordinary edits, and
// each target compiles to different instruction sizes so every number needs one value per
// architecture. Worse, an absolute count cannot tell a boundary landing one page over from a
// regression that stopped sharing altogether - it fails identically for both. What the numbers
// were there to protect is proven directly below and in
// `dynamic_process_service_loads_programs_from_system_bin`, by comparing the physical frame two
// concurrent processes map: if sharing breaks, that comparison fails and says so.
fn measure_dynamic_wave_launch(process_client: &alloc::sync::Arc<object::channel::Channel>, wave: u8, name: &[u8], correlation: u32) {
	let (first, first_bootstrap, first_ns) = launch_dynamic_for_measurement(process_client, name, correlation);
	let (second, second_bootstrap, warm_ns) = launch_dynamic_for_measurement(process_client, name, correlation + 1);
	let private_pages = first.private_image_pages();
	let shared_pages = first.shared_image_pages();
	assert!(first_ns != 0 && warm_ns != 0, "wave launch timings are nonzero");
	assert!(shared_pages != 0, "a dynamic launch maps immutable image pages to share");
	assert_eq!(second.private_image_pages(), private_pages, "repeated wave launch has the same private footprint");
	assert_eq!(second.shared_image_pages(), shared_pages, "repeated wave launch has the same shared footprint");
	drop(first_bootstrap);
	drop(second_bootstrap);
	sched::run_until_idle();
	assert!(first.is_terminated() && second.is_terminated(), "wave representatives exit after bootstrap closure");
	let first_provider_frame = first.address_space().unmap(0x2000_0000).expect("first wave representative lsrt text page");
	let second_provider_frame = second.address_space().unmap(0x2000_0000).expect("second wave representative lsrt text page");
	assert_eq!(first_provider_frame, second_provider_frame, "repeated wave launches share one physical lsrt text page");
	crate::serial_println!("dynamic-wave-perf: wave={} tool={} first={}ns warm={}ns private-pages={} shared-pages={}", wave, core::str::from_utf8(name).unwrap_or("invalid"), first_ns, warm_ns, private_pages, shared_pages);
}

fn assert_unrelated_dynamic_consumers_share(process_client: &alloc::sync::Arc<object::channel::Channel>, first_name: &[u8], second_name: &[u8], correlation: u32, provider_address: u64, provider: &str) {
	let (first, first_bootstrap, _) = launch_dynamic_for_measurement(process_client, first_name, correlation);
	let (second, second_bootstrap, _) = launch_dynamic_for_measurement(process_client, second_name, correlation + 1);
	drop(first_bootstrap);
	drop(second_bootstrap);
	sched::run_until_idle();
	assert!(first.is_terminated() && second.is_terminated(), "unrelated dynamic consumers exit after bootstrap closure");
	let first_frame = first.address_space().unmap(provider_address).expect("first unrelated consumer provider text page");
	let second_frame = second.address_space().unmap(provider_address).expect("second unrelated consumer provider text page");
	assert_eq!(first_frame, second_frame, "unrelated dynamic consumers share one physical {provider} text page");
}

fn replace_dynamic_needed(volume: &mut [u8], artifact: &[u8], expected: &str, replacement: &str) {
	assert_eq!(expected.len(), replacement.len(), "dynamic dependency replacement changes ELF string layout");
	let volume_base = volume.as_ptr() as usize;
	let offset = {
		let archive = pkg::Package::parse(&*volume).expect("volume package parses");
		let bytes = archive.lookup(artifact).expect("dynamic test executable is staged");
		let elf = bootproto::elf::Elf::parse(bytes).expect("dynamic test executable is ELF");
		let dynamic = elf.dynamic_info().expect("dynamic test executable metadata parses").expect("dynamic test executable has PT_DYNAMIC");
		let dependency = elf.needed_names(&dynamic).expect("dynamic test executable dependencies parse").find(|name| *name == expected).expect("dynamic test executable names expected provider");
		dependency.as_ptr() as usize - volume_base
	};
	volume[offset..offset + replacement.len()].copy_from_slice(replacement.as_bytes());
}

fn duplicate_dynamic_needed(volume: &mut [u8], artifact: &[u8]) {
	let volume_base = volume.as_ptr() as usize;
	let (offset, replacement) = {
		let archive = pkg::Package::parse(&*volume).expect("volume package parses");
		let bytes = archive.lookup(artifact).expect("duplicate edge test executable is staged");
		let elf = bootproto::elf::Elf::parse(bytes).expect("duplicate edge test executable is ELF");
		let segment = (0..elf.segment_count())
			.find_map(|index| {
				let segment = elf.segment(index)?;
				(segment.p_type == bootproto::elf::PT_DYNAMIC).then_some(segment)
			})
			.expect("duplicate edge test executable has PT_DYNAMIC");
		let needed: alloc::vec::Vec<(usize, u64)> = elf.dynamic_entries().expect("duplicate edge test dynamic entries parse").expect("duplicate edge test executable has one dynamic table").enumerate().filter_map(|(index, entry)| (entry.tag == bootproto::elf::DT_NEEDED).then_some((index, entry.value))).collect();
		assert!(needed.len() >= 2, "duplicate edge test executable has two providers");
		let (_, first_value) = needed[0];
		let (second_index, second_value) = needed[1];
		let entry_len = core::mem::size_of::<bootproto::elf::DynamicEntry>();
		let tag_len = core::mem::size_of::<i64>();
		let value_offset = usize::try_from(segment.p_offset).expect("duplicate edge dynamic offset fits") + second_index * entry_len + tag_len;
		assert_eq!(i64::from_le_bytes(bytes[value_offset - tag_len..value_offset].try_into().expect("duplicate edge tag bytes")), bootproto::elf::DT_NEEDED);
		assert_eq!(u64::from_le_bytes(bytes[value_offset..value_offset + core::mem::size_of::<u64>()].try_into().expect("duplicate edge value bytes")), second_value);
		(bytes.as_ptr() as usize - volume_base + value_offset, first_value)
	};
	volume[offset..offset + core::mem::size_of::<u64>()].copy_from_slice(&replacement.to_le_bytes());
}

fn swap_dynamic_needed_order(volume: &mut [u8], artifact: &[u8]) {
	let volume_base = volume.as_ptr() as usize;
	let (first_offset, first_value, second_offset, second_value) = {
		let archive = pkg::Package::parse(&*volume).expect("volume package parses");
		let bytes = archive.lookup(artifact).expect("dynamic order test executable is staged");
		let elf = bootproto::elf::Elf::parse(bytes).expect("dynamic order test executable is ELF");
		let needed: alloc::vec::Vec<(usize, u64)> = elf.dynamic_entries().expect("dynamic order test metadata parses").expect("dynamic order test executable has one dynamic table").enumerate().filter_map(|(index, entry)| (entry.tag == bootproto::elf::DT_NEEDED).then_some((index, entry.value))).collect();
		assert!(needed.len() >= 2, "dynamic order test executable has two providers");
		let (first_index, first_value) = needed[0];
		let (second_index, second_value) = needed[1];
		let value_offset = core::mem::size_of::<i64>();
		(bytes.as_ptr() as usize - volume_base + dynamic_entry_file_offset(&elf, first_index) + value_offset, first_value, bytes.as_ptr() as usize - volume_base + dynamic_entry_file_offset(&elf, second_index) + value_offset, second_value)
	};
	volume[first_offset..first_offset + core::mem::size_of::<u64>()].copy_from_slice(&second_value.to_le_bytes());
	volume[second_offset..second_offset + core::mem::size_of::<u64>()].copy_from_slice(&first_value.to_le_bytes());
}

fn program_header_file_offset(bytes: &[u8], index: usize) -> usize {
	let table_offset = usize::try_from(u64::from_le_bytes(bytes[32..40].try_into().expect("program-header table offset bytes"))).expect("program-header table offset fits");
	let entry_len = usize::from(u16::from_le_bytes(bytes[54..56].try_into().expect("program-header entry length bytes")));
	let count = usize::from(u16::from_le_bytes(bytes[56..58].try_into().expect("program-header count bytes")));
	assert!(index < count, "program-header index is in range");
	table_offset.checked_add(index.checked_mul(entry_len).expect("program-header entry offset fits")).expect("program-header file offset fits")
}

fn dynamic_segment_file_offset(elf: &bootproto::elf::Elf<'_>) -> (usize, bootproto::elf::ProgramHeader) {
	let (_, segment) = (0..elf.segment_count())
		.find_map(|index| {
			let segment = elf.segment(index)?;
			(segment.p_type == bootproto::elf::PT_DYNAMIC).then_some((index, segment))
		})
		.expect("dynamic metadata test executable has PT_DYNAMIC");
	(usize::try_from(segment.p_offset).expect("dynamic segment file offset fits"), segment)
}

fn dynamic_entry_file_offset(elf: &bootproto::elf::Elf<'_>, index: usize) -> usize {
	let (offset, segment) = dynamic_segment_file_offset(elf);
	let entry_len = core::mem::size_of::<bootproto::elf::DynamicEntry>();
	assert!(index.checked_add(1).and_then(|count| count.checked_mul(entry_len)).is_some_and(|bytes| bytes <= segment.p_filesz as usize), "dynamic entry index is in range");
	offset + index * entry_len
}

fn duplicate_dynamic_segment(volume: &mut [u8], artifact: &[u8]) {
	let volume_base = volume.as_ptr() as usize;
	let offset = {
		let archive = pkg::Package::parse(&*volume).expect("volume package parses");
		let bytes = archive.lookup(artifact).expect("duplicate segment test executable is staged");
		let elf = bootproto::elf::Elf::parse(bytes).expect("duplicate segment test executable is ELF");
		let index = (0..elf.segment_count()).find(|index| elf.segment(*index).is_some_and(|segment| segment.p_type != bootproto::elf::PT_DYNAMIC && segment.p_filesz != 0)).expect("duplicate segment test finds a nonempty non-dynamic segment");
		let header_offset = program_header_file_offset(bytes, index);
		assert_ne!(u32::from_le_bytes(bytes[header_offset..header_offset + core::mem::size_of::<u32>()].try_into().expect("duplicate segment type bytes")), bootproto::elf::PT_DYNAMIC);
		bytes.as_ptr() as usize - volume_base + header_offset
	};
	volume[offset..offset + core::mem::size_of::<u32>()].copy_from_slice(&bootproto::elf::PT_DYNAMIC.to_le_bytes());
}

fn remove_dynamic_terminator(volume: &mut [u8], artifact: &[u8]) {
	let volume_base = volume.as_ptr() as usize;
	let offsets = {
		let archive = pkg::Package::parse(&*volume).expect("volume package parses");
		let bytes = archive.lookup(artifact).expect("missing terminator test executable is staged");
		let elf = bootproto::elf::Elf::parse(bytes).expect("missing terminator test executable is ELF");
		elf.dynamic_entries().expect("missing terminator test dynamic entries parse").expect("missing terminator test executable has one dynamic table").enumerate().filter_map(|(index, entry)| (entry.tag == bootproto::elf::DT_NULL).then_some(bytes.as_ptr() as usize - volume_base + dynamic_entry_file_offset(&elf, index))).collect::<alloc::vec::Vec<usize>>()
	};
	assert!(!offsets.is_empty(), "missing terminator test finds a terminator");
	for offset in offsets {
		volume[offset..offset + core::mem::size_of::<i64>()].copy_from_slice(&0x6fff_ffffi64.to_le_bytes());
	}
}

fn duplicate_dynamic_singleton(volume: &mut [u8], artifact: &[u8]) {
	let volume_base = volume.as_ptr() as usize;
	let offset = {
		let archive = pkg::Package::parse(&*volume).expect("volume package parses");
		let bytes = archive.lookup(artifact).expect("duplicate singleton test executable is staged");
		let elf = bootproto::elf::Elf::parse(bytes).expect("duplicate singleton test executable is ELF");
		let entries: alloc::vec::Vec<(usize, bootproto::elf::DynamicEntry)> = elf.dynamic_entries().expect("duplicate singleton test dynamic entries parse").expect("duplicate singleton test executable has one dynamic table").enumerate().collect();
		assert!(entries.iter().any(|(_, entry)| entry.tag == bootproto::elf::DT_STRTAB), "duplicate singleton test has DT_STRTAB");
		let index = entries.iter().find_map(|(index, entry)| (entry.tag != bootproto::elf::DT_STRTAB && entry.tag != bootproto::elf::DT_NULL).then_some(*index)).expect("duplicate singleton test finds a non-singleton entry");
		bytes.as_ptr() as usize - volume_base + dynamic_entry_file_offset(&elf, index)
	};
	volume[offset..offset + core::mem::size_of::<i64>()].copy_from_slice(&bootproto::elf::DT_STRTAB.to_le_bytes());
}

fn replace_dynamic_value(volume: &mut [u8], artifact: &[u8], tag: i64, replacement: u64) {
	let volume_base = volume.as_ptr() as usize;
	let offset = {
		let archive = pkg::Package::parse(&*volume).expect("volume package parses");
		let bytes = archive.lookup(artifact).expect("dynamic metadata test executable is staged");
		let elf = bootproto::elf::Elf::parse(bytes).expect("dynamic metadata test executable is ELF");
		let index = elf.dynamic_entries().expect("dynamic metadata test entries parse").expect("dynamic metadata test executable has one dynamic table").enumerate().find_map(|(index, entry)| (entry.tag == tag).then_some(index)).expect("dynamic metadata test finds the requested tag");
		bytes.as_ptr() as usize - volume_base + dynamic_entry_file_offset(&elf, index) + core::mem::size_of::<i64>()
	};
	volume[offset..offset + core::mem::size_of::<u64>()].copy_from_slice(&replacement.to_le_bytes());
}

fn invalidate_dynamic_symbol_entry_size(volume: &mut [u8], artifact: &[u8]) {
	replace_dynamic_value(volume, artifact, bootproto::elf::DT_SYMENT, 23);
}

fn overflow_dynamic_symbol_count(volume: &mut [u8], artifact: &[u8]) {
	let volume_base = volume.as_ptr() as usize;
	let offset = {
		let archive = pkg::Package::parse(&*volume).expect("volume package parses");
		let bytes = archive.lookup(artifact).expect("dynamic symbol test executable is staged");
		let elf = bootproto::elf::Elf::parse(bytes).expect("dynamic symbol test executable is ELF");
		let dynamic = elf.dynamic_info().expect("dynamic symbol test metadata parses").expect("dynamic symbol test executable has PT_DYNAMIC");
		let hash = elf.virtual_data(dynamic.hash.expect("dynamic symbol test executable has DT_HASH"), 8).expect("dynamic symbol test hash header is file-backed");
		hash.as_ptr() as usize - volume_base + core::mem::size_of::<u32>()
	};
	volume[offset..offset + core::mem::size_of::<u32>()].copy_from_slice(&u32::MAX.to_le_bytes());
}

fn invalidate_plt_relocation_size(volume: &mut [u8], artifact: &[u8]) {
	replace_dynamic_value(volume, artifact, bootproto::elf::DT_PLTRELSZ, 47);
}

fn replace_volume_entry(volume: &mut [u8], destination: &[u8], source: &[u8]) {
	let volume_base = volume.as_ptr() as usize;
	let (destination_offset, destination_len, source_bytes) = {
		let archive = pkg::Package::parse(&*volume).expect("volume package parses");
		let destination_bytes = archive.lookup(destination).expect("identity test destination is staged");
		let source_bytes = archive.lookup(source).expect("identity test source is staged");
		assert!(source_bytes.len() <= destination_bytes.len(), "identity test replacement does not fit its package entry");
		(destination_bytes.as_ptr() as usize - volume_base, destination_bytes.len(), source_bytes.to_vec())
	};
	volume[destination_offset..destination_offset + destination_len].fill(0);
	volume[destination_offset..destination_offset + source_bytes.len()].copy_from_slice(&source_bytes);
}

fn corrupt_identity_note(volume: &mut [u8], artifact: &[u8], field: &[u8]) {
	let volume_base = volume.as_ptr() as usize;
	let offset = {
		let archive = pkg::Package::parse(&*volume).expect("volume package parses");
		let bytes = archive.lookup(artifact).expect("identity test artifact is staged");
		let elf = bootproto::elf::Elf::parse(bytes).expect("identity test artifact is ELF");
		let record = elf.liber_identity_note().expect("identity test artifact carries a record");
		let field_offset = record.windows(field.len()).position(|window| window == field).expect("identity test field is present");
		record.as_ptr() as usize - volume_base + field_offset + field.len()
	};
	volume[offset] = if volume[offset] == b'0' { b'1' } else { b'0' };
}

fn loader_visible_dynamic_export(symbol: bootproto::elf::Symbol, name: &str) -> bool {
	symbol.is_defined() && matches!(symbol.binding(), 1 | 2) && matches!(symbol.symbol_type(), 0..=2) && matches!(symbol.visibility(), 0 | 3) && !name.is_empty()
}

fn replace_provider_export(volume: &mut [u8], provider_entry: &[u8], runtime_entry: &[u8]) {
	let volume_base = volume.as_ptr() as usize;
	let (offset, replacement) = {
		let archive = pkg::Package::parse(&*volume).expect("volume package parses");
		let provider_bytes = archive.lookup(provider_entry).expect("provider export test provider is staged");
		let provider = bootproto::elf::Elf::parse(provider_bytes).expect("provider export test provider is ELF");
		let provider_dynamic = provider.dynamic_info().expect("provider export test provider metadata parses").expect("provider export test provider has PT_DYNAMIC");
		let runtime_bytes = archive.lookup(runtime_entry).expect("provider export test runtime is staged");
		let runtime = bootproto::elf::Elf::parse(runtime_bytes).expect("provider export test runtime is ELF");
		let runtime_dynamic = runtime.dynamic_info().expect("provider export test runtime metadata parses").expect("provider export test runtime has PT_DYNAMIC");
		let runtime_exports: alloc::vec::Vec<&str> = runtime.symbols(&runtime_dynamic).expect("provider export test runtime symbols parse").filter_map(|(symbol, name)| loader_visible_dynamic_export(symbol, name).then_some(name)).collect();
		let (source, replacement) = provider
			.symbols(&provider_dynamic)
			.expect("provider export test provider symbols parse")
			.find_map(|(symbol, name)| {
				if !loader_visible_dynamic_export(symbol, name) {
					return None;
				}
				runtime_exports.iter().copied().find(|candidate| candidate.len() == name.len() && *candidate != name).map(|candidate| (name, candidate))
			})
			.expect("provider export test finds equal-length provider and runtime exports");
		assert_eq!(source.len(), replacement.len(), "provider export replacement preserves the ELF string layout");
		(source.as_ptr() as usize - volume_base, replacement.as_bytes().to_vec())
	};
	volume[offset..offset + replacement.len()].copy_from_slice(&replacement);
}

fn launch_from_volume(volume: &[u8], name: &[u8], correlation: u32) -> object::channel::Message {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	let (_, package) = scenario_packages().expect("scenario packages");
	let init = init_package_bytes().expect("init package module not found");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe in the init package");
	let process_elf = package.lookup(b"process_service.lsexe").expect("process_service.lsexe in the init package");
	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (process_boot_kernel, process_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	let (process_server, process_client) = Channel::create();
	let (_, bootstrap) = Channel::create();
	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).expect("spawn StorageService");
	loader::spawn_elf_process(domain, process_elf, process_boot_user, Rights::ALL, 0).expect("spawn ProcessService");
	send_ramdisk(&storage_boot_kernel, volume).expect("test storage ramdisk bootstrap");
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL).expect("storage serve bootstrap");
	send_package(&process_boot_kernel, init).expect("process package bootstrap");
	// The development registry channel. ProcessService receives it unconditionally, so it must be
	// sent even here where nothing will ever answer on it: an absent one blocks that service in
	// its bootstrap, and every launch after it waits on a reply that cannot come.
	let (registry_server, registry_client) = Channel::create();
	core::mem::drop(registry_server);
	send_cap(&process_boot_kernel, b"STORAGE", storage_client, Rights::ALL).expect("process storage bootstrap");
	send_cap(&process_boot_kernel, b"REGISTRY", registry_client, Rights::ALL).expect("process registry bootstrap");
	send_cap(&process_boot_kernel, b"SERVE", process_server, Rights::ALL).expect("process serve bootstrap");
	let mut launch = alloc::vec::Vec::new();
	launch.extend_from_slice(&3u16.to_le_bytes());
	launch.extend_from_slice(&correlation.to_le_bytes());
	launch.extend_from_slice(&(name.len() as u16).to_le_bytes());
	launch.extend_from_slice(name);
	launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &launch, bootstrap, Rights::ALL).expect("dynamic test launch request");
	sched::run_until_idle();
	assert_eq!(&process_boot_kernel.recv().expect("ProcessService online report").bytes, b"ProcessService: online");
	let reply = process_client.recv().expect("dynamic test launch reply");
	process_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");
	sched::run_until_idle();
	reply
}

// The sector where StorageService lays the LiberFS volume when a disk carries no GPT partition
// for it - it must mirror the storage service's own FS_START_SECTOR
// (src/user/services/storage/src/service.rs).
//
// It was 65536 (32 MiB in), clearing a factory archive at LBA 0 that the service seeded a fresh
// volume from. The archive is retired (P02M0108): the volume is built as a filesystem, so there is
// nothing in front of it to skip and it starts at the beginning of its container.
const FALLBACK_START_SECTOR: u64 = 0;

// Lay a complete, correctly checksummed GPT - protective MBR, primary header, entry array,
// and the backup copy at the far end - into a sparse sector map, naming `entries` as
// (type GUID, first LBA, last LBA).
//
// Real checksums, because the probe checks them now. A test that hand-assembles a header
// without them is testing the refusal path whatever else it claims to be about, and the two
// GPT tests here did exactly that until the probe grew teeth.
fn lay_gpt(disk: &mut alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>>, capacity_sectors: u64, entries: &[([u8; 16], u64, u64)]) {
	const SECTOR: usize = 512;
	const NUM_ENTRIES: u64 = 128;
	const ENTRY_SIZE: usize = 128;
	let array_sectors: u64 = NUM_ENTRIES * ENTRY_SIZE as u64 / SECTOR as u64;
	let first_usable: u64 = 2 + array_sectors;
	let last_usable: u64 = capacity_sectors - array_sectors - 2;

	let mut mbr = alloc::vec![0u8; SECTOR];
	mbr[446 + 4] = 0xEE;
	mbr[446 + 12..446 + 16].copy_from_slice(&u32::MAX.to_le_bytes());
	mbr[510] = 0x55;
	mbr[511] = 0xAA;
	disk.insert(0, mbr);

	let mut array = alloc::vec![0u8; (NUM_ENTRIES * ENTRY_SIZE as u64) as usize];
	for (i, (guid, first, last)) in entries.iter().enumerate() {
		let off = i * ENTRY_SIZE;
		array[off..off + 16].copy_from_slice(guid);
		array[off + 16..off + 24].copy_from_slice(&(i as u64 + 1).to_le_bytes());
		array[off + 32..off + 40].copy_from_slice(&first.to_le_bytes());
		array[off + 40..off + 48].copy_from_slice(&last.to_le_bytes());
	}
	let array_crc = partition::crc32(&array);

	let mut header = |header_lba: u64, backup_lba: u64, entries_lba: u64| {
		for (i, chunk) in array.chunks(SECTOR).enumerate() {
			disk.insert(entries_lba + i as u64, chunk.to_vec());
		}
		let mut hdr = alloc::vec![0u8; SECTOR];
		hdr[0..8].copy_from_slice(b"EFI PART");
		hdr[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
		hdr[12..16].copy_from_slice(&92u32.to_le_bytes());
		hdr[24..32].copy_from_slice(&header_lba.to_le_bytes());
		hdr[32..40].copy_from_slice(&backup_lba.to_le_bytes());
		hdr[40..48].copy_from_slice(&first_usable.to_le_bytes());
		hdr[48..56].copy_from_slice(&last_usable.to_le_bytes());
		hdr[72..80].copy_from_slice(&entries_lba.to_le_bytes());
		hdr[80..84].copy_from_slice(&(NUM_ENTRIES as u32).to_le_bytes());
		hdr[84..88].copy_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
		hdr[88..92].copy_from_slice(&array_crc.to_le_bytes());
		let crc = partition::crc32(&hdr[..92]);
		hdr[16..20].copy_from_slice(&crc.to_le_bytes());
		disk.insert(header_lba, hdr);
	};
	header(1, capacity_sectors - 1, 2);
	header(capacity_sectors - 1, 1, capacity_sectors - 1 - array_sectors);
}

// Serve pending raw-block-protocol requests (read/write/capacity/flush) over a sparse
// in-memory sector map: the stand-in block driver behind the StorageService layout
// tests.
fn pump_block_stand_in(blk_host: &object::channel::Channel, disk: &mut alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>>, capacity: u64) {
	use object::channel::Message;
	use object::memory_object::MemoryObject;
	use object::rights::Rights;
	const SECTOR: usize = 512;
	while let Ok(req) = blk_host.recv() {
		assert!(req.bytes.len() >= 16, "a block request is [op][lba][count]");
		let op = u32::from_le_bytes([req.bytes[0], req.bytes[1], req.bytes[2], req.bytes[3]]);
		let lba = u64::from_le_bytes(req.bytes[4..12].try_into().unwrap());
		let count = u32::from_le_bytes(req.bytes[12..16].try_into().unwrap()).max(1);
		match op {
			0 => {
				// read: hand back a fresh buffer of the requested sectors.
				let mut data = alloc::vec![0u8; count as usize * SECTOR];
				for s in 0..count as u64 {
					if let Some(sec) = disk.get(&(lba + s)) {
						data[s as usize * SECTOR..(s as usize + 1) * SECTOR].copy_from_slice(sec);
					}
				}
				let obj = MemoryObject::create(data.len()).expect("the sector buffer should allocate");
				copy_into_object(&obj, &data);
				send_cap(blk_host, &0u32.to_le_bytes(), obj, Rights::ALL).expect("the read reply should send");
			}
			1 => {
				// write: store the transferred sectors into the sparse disk.
				let cap = req.caps.first().expect("a write carries its buffer");
				let object = cap.object();
				let memory = object.as_any().downcast_ref::<MemoryObject>().expect("the buffer is a MemoryObject");
				let data = read_from_object(memory, count as usize * SECTOR);
				for s in 0..count as u64 {
					disk.insert(lba + s, data[s as usize * SECTOR..(s as usize + 1) * SECTOR].to_vec());
				}
				blk_host.send(Message::new(0u32.to_le_bytes().to_vec(), alloc::vec::Vec::new(), 0)).expect("the write reply should send");
			}
			2 => {
				// capacity: the sparse disk's size in bytes.
				let mut reply = alloc::vec::Vec::with_capacity(12);
				reply.extend_from_slice(&0u32.to_le_bytes());
				reply.extend_from_slice(&capacity.to_le_bytes());
				blk_host.send(Message::new(reply, alloc::vec::Vec::new(), 0)).expect("the capacity reply should send");
			}
			3 => {
				// flush: the in-memory disk is trivially durable; acknowledge the barrier.
				blk_host.send(Message::new(0u32.to_le_bytes().to_vec(), alloc::vec::Vec::new(), 0)).expect("the flush reply should send");
			}
			other => panic!("unexpected block op {}", other),
		}
	}
}

// What a harness was started over, so `restart` can start it the same way.
#[derive(Clone)]
enum Backing {
	// A block device the harness itself serves out of its sector map. `tag` is the bootstrap word
	// - BLOCK, FATBLOCK, ISOBLOCK, UDFBLOCK, USBBLOCK - because they are not interchangeable.
	Disk { tag: alloc::vec::Vec<u8> },
	// A volume that lives on the service's heap. There is no disk, which is the whole point.
	Memory { tag: alloc::vec::Vec<u8>, bytes: usize },
}

struct StorageHarness {
	boot: alloc::sync::Arc<object::channel::Channel>,
	block: alloc::sync::Arc<object::channel::Channel>,
	client: alloc::sync::Arc<object::channel::Channel>,
	admin: alloc::sync::Arc<object::channel::Channel>,
	disk: alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>>,
	capacity: u64,
	// How this harness was started, so `restart` can start it the same way.
	//
	// It could not, and restarted EVERYTHING as `BLOCK` over whatever was in `disk`. For a
	// disk-backed harness that is right. For a memory-backed one `disk` is empty, so the service
	// was handed a blank disk - and the only reason the test passed is that the service used to
	// FORMAT one, producing an empty volume, which is what a restarted memory volume looks like.
	// The assertion was true for a reason that had nothing to do with what it was testing, and the
	// moment the service stopped formatting disks it failed.
	backing: Backing,
	// The service's own process, so a test can read its handle table. Handle leaks are invisible
	// from the outside - the service keeps answering, one handle poorer each time - until the table
	// is full and every later request fails for a reason unrelated to what caused it.
	process: Option<alloc::sync::Arc<object::process::Process>>,
}

// The harness disk itself, as a block device, so a fixture volume can be formatted straight into
// the sector map the harness will serve.
//
// The first version built the whole image as one contiguous `Vec` and then chopped it into
// sectors. That is two copies of a multi-megabyte volume in the kernel heap, and the contiguous
// one is the expensive half: the kernel allocator grows in 2 MiB regions and first-fits within
// them, so a single 8 MiB request has to find eight coalesced regions. It held on x86_64 and did
// not on aarch64, whose binaries - and therefore whose volume - are close to twice the size.
// Writing sectors as they are produced removes the contiguous allocation entirely.
struct FixtureDisk {
	sectors: alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>>,
}

impl FixtureDisk {
	const SECTOR: usize = 512;
}

impl fscore::BlockDevice for FixtureDisk {
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool {
		// A BUFFER THAT IS NOT A WHOLE NUMBER OF SECTORS IS REFUSED, not silently left alone.
		//
		// `per` is `buf.len() / SECTOR`, so a buffer shorter than one sector made the loop below run
		// zero times - and this returned `true`. A caller then held a buffer it believed had been
		// filled from the medium and had in fact never been touched, with the device reporting
		// success: the "looks like it worked" failure this tree keeps finding, in the fixture that
		// stands in for a disk.
		//
		// No caller does this today, which is exactly why the harness should say so if one starts.
		if buf.is_empty() || buf.len() % Self::SECTOR != 0 {
			return false;
		}
		let per = buf.len() / Self::SECTOR;
		for s in 0..per {
			let lba = index * per as u64 + s as u64;
			let out = &mut buf[s * Self::SECTOR..(s + 1) * Self::SECTOR];
			match self.sectors.get(&lba) {
				Some(sector) => out.copy_from_slice(sector),
				// An unwritten sector reads as zeros, like a fresh disk.
				None => out.fill(0),
			}
		}
		true
	}

	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		// The same rule writing: a partial sector written as nothing and reported as written is how
		// a fixture comes to disagree with what the code under test believes it stored.
		if buf.is_empty() || buf.len() % Self::SECTOR != 0 {
			return false;
		}
		let per = buf.len() / Self::SECTOR;
		for s in 0..per {
			let lba = index * per as u64 + s as u64;
			let mut sector = alloc::vec![0u8; Self::SECTOR];
			sector.copy_from_slice(&buf[s * Self::SECTOR..(s + 1) * Self::SECTOR]);
			self.sectors.insert(lba, sector);
		}
		true
	}
}

// A LiberFS volume laid straight onto a whole device, as a sector map: the fixed whole-device
// layout, superblock at LBA 0, no partition table.
//
// This is what `mkpackages` produces and what gets written to a medium. Tests build it here rather
// than asking StorageService for one, because the service reads disks and does not make them - it
// used to format a blank disk at boot, and several tests got their volume that way without ever
// saying so.
fn whole_device_volume(bytes: usize) -> alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>> {
	prepared_volume(0, (bytes / liberfs::BLOCK_SIZE) as u64)
}

// The same, starting at `first_lba` - a volume inside a GPT partition rather than on the whole
// device. The sector map is shifted, which is what a partition IS to the block layer.
//
// FORMATTED ONCE PER SIZE and cloned, for the reason `start_system` gives about its own fixture:
// laying out a LiberFS volume is a B-tree walk and a transaction log, and under TCG that is minutes
// rather than seconds when several tests each pay it. Copying the finished sector map is a memcpy.
//
// The first version had no cache and three tests asked for the same 64 MB volume. It cost riscv64
// roughly half its throughput - 36 s per test against 22 - which is the exact trap the comment on
// `start_system` was written to warn about, walked into three functions further down the same file.
fn prepared_volume(first_lba: u64, blocks: u64) -> alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>> {
	static FORMATTED: crate::sync::SpinLock<Option<(u64, alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>>)>> = crate::sync::SpinLock::new(None);
	// Keyed by size alone: the shift is applied to the copy, so one cached map serves every offset.
	let cached = FORMATTED.lock().as_ref().and_then(|(size, sectors)| (*size == blocks).then(|| sectors.clone()));
	let sectors = match cached {
		Some(sectors) => sectors,
		None => {
			let opts = liberfs::FormatOpts { uuid: *b"libersystem-who\0", label: b"system".to_vec(), compress: false };
			let disk = FixtureDisk { sectors: alloc::collections::BTreeMap::new() };
			let fs = liberfs::LiberFs::format_opts(disk, blocks, opts).expect("format the volume fixture");
			let sectors = fs.into_device().sectors;
			*FORMATTED.lock() = Some((blocks, sectors.clone()));
			sectors
		}
	};
	if first_lba == 0 {
		return sectors;
	}
	sectors.into_iter().map(|(lba, sector)| (lba + first_lba, sector)).collect()
}

// Spawn the storage harness, and if the image will not load, SAY WHAT THE BYTES WERE.
//
// `BadImage` from this call means the ELF failed to PARSE, and for a slice into the boot package
// that means the bytes changed underneath it - but the panic alone says nothing about how. The
// header is the evidence: zeros are a frame handed to somebody else and cleared, a different magic
// is a second image written over this one, and an intact magic moves the fault out of memory and
// into the loader. It costs nothing on the path that works, which is every path but one.
fn spawn_harness(storage_elf: &[u8], boot_user: alloc::sync::Arc<dyn object::KernelObject>) -> alloc::sync::Arc<object::process::Process> {
	match loader::spawn_elf_process(sched::root_domain(), storage_elf, boot_user, object::rights::Rights::ALL, 0) {
		Ok(process) => process,
		Err(error) => {
			let head = &storage_elf[..16.min(storage_elf.len())];
			crate::serial_println!("harness: StorageService image will not load ({error:?}) - {} bytes, header {head:02x?}", storage_elf.len());
			panic!("spawn StorageService harness");
		}
	}
}

impl StorageHarness {
	// Start a StorageService over a disk carrying `image` verbatim: a FAT, ISO or UDF medium, or
	// any other fixture that is already a filesystem image.
	fn start(storage_elf: &[u8], tag: &[u8], image: &[u8], capacity: u64) -> Self {
		const SECTOR: usize = 512;
		let mut disk = alloc::collections::BTreeMap::new();
		for (lba, chunk) in image.chunks(SECTOR).enumerate() {
			let mut sector = alloc::vec![0u8; SECTOR];
			sector[..chunk.len()].copy_from_slice(chunk);
			disk.insert(lba as u64, sector);
		}
		Self::start_disk(storage_elf, tag, disk, capacity)
	}

	// Start a StorageService over a disk carrying a LiberFS SYSTEM VOLUME built from the scenario
	// archive.
	//
	// The archive used to be laid on the disk raw and the service formatted a volume and seeded
	// itself from it. That seeding is gone (P02M0108) - the system volume is built as a filesystem
	// now - so the fixture has to be a filesystem too. Formatting it here with the same crate the
	// service mounts means the fixture cannot drift from the format under test.
	//
	// Separate from `start` because that one lays an image verbatim, and the media fixtures it
	// serves (FAT, ISO, UDF) are images already - reinterpreting them as archives to rebuild would
	// be nonsense, which is exactly what happened when the two shared one function.
	// A disk-backed harness over an empty volume of `bytes`, for the cases that need free space
	// rather than contents.
	fn start_empty(storage_elf: &[u8], bytes: usize) -> Self {
		Self::start_disk(storage_elf, b"BLOCK", Self::build_empty_fixture(bytes), bytes as u64)
	}

	fn start_system(storage_elf: &[u8], tag: &[u8], archive: &[u8], capacity: u64) -> Self {
		// Formatted ONCE and cloned per harness. Building it per test made the aarch64 suite
		// exceed its watchdog: laying out a LiberFS volume is a B-tree walk and a transaction log
		// per file, and under TCG that is minutes rather than seconds when a dozen storage tests
		// each pay it. Copying the finished sector map is a memcpy of the same bytes.
		static FIXTURE: crate::sync::SpinLock<Option<alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>>>> = crate::sync::SpinLock::new(None);
		if let Some(sectors) = FIXTURE.lock().as_ref() {
			return Self::start_disk(storage_elf, tag, sectors.clone(), capacity);
		}
		let sectors = Self::build_system_fixture(archive);
		*FIXTURE.lock() = Some(sectors.clone());
		Self::start_disk(storage_elf, tag, sectors, capacity)
	}

	// Format a LiberFS volume carrying the scenario archive's files, as a sector map.
	// An EMPTY volume of a given size, formatted once per size and cached.
	//
	// The note on `start_system` says formatting is expensive, and it is - but per FILE: a B-tree
	// walk and a transaction log each. An empty volume is a superblock and allocator metadata
	// whatever its size, which is what makes a large one affordable and lets a write bigger than
	// the old invented 16 MiB ceiling be tested at all.
	fn build_empty_fixture(bytes: usize) -> alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>> {
		const BLOCK: usize = liberfs::BLOCK_SIZE;
		static EMPTY: crate::sync::SpinLock<Option<(usize, alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>>)>> = crate::sync::SpinLock::new(None);
		if let Some((size, sectors)) = EMPTY.lock().as_ref() {
			if *size == bytes {
				return sectors.clone();
			}
		}
		let opts = liberfs::FormatOpts { uuid: *b"libersystem-emp\0", label: b"system".to_vec(), compress: false };
		let disk = FixtureDisk { sectors: alloc::collections::BTreeMap::new() };
		let fs = liberfs::LiberFs::format_opts(disk, (bytes / BLOCK) as u64, opts).expect("format the empty fixture");
		let sectors = fs.into_device().sectors;
		*EMPTY.lock() = Some((bytes, sectors.clone()));
		sectors
	}

	// A minimal LiberFS volume: two small files, formatted once and cached like the big one.
	fn build_tiny_fixture() -> alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>> {
		const BLOCK: usize = liberfs::BLOCK_SIZE;
		static TINY: crate::sync::SpinLock<Option<alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>>>> = crate::sync::SpinLock::new(None);
		if let Some(sectors) = TINY.lock().as_ref() {
			return sectors.clone();
		}
		let size = 512 * 1024;
		let opts = liberfs::FormatOpts { uuid: *b"libersystem-tin\0", label: b"system".to_vec(), compress: false };
		let disk = FixtureDisk { sectors: alloc::collections::BTreeMap::new() };
		let mut fs = liberfs::LiberFs::format_opts(disk, (size / BLOCK) as u64, opts).expect("format the tiny fixture");
		fs.write_file(b"hello.txt", b"hello").expect("write hello");
		fs.write_file(b"motd.txt", b"motd").expect("write motd");
		let sectors = fs.into_device().sectors;
		*TINY.lock() = Some(sectors.clone());
		sectors
	}

	fn build_system_fixture(archive: &[u8]) -> alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>> {
		const BLOCK: usize = liberfs::BLOCK_SIZE;
		let entries = pkg::Package::parse(archive).expect("scenario archive parses");
		let payload: usize = (0..entries.len()).filter_map(|i| entries.name(i).and_then(|n| entries.lookup(n)).map(|b| b.len())).sum();
		// Room for the files, their metadata, and what the scenarios write afterwards.
		let size = ((payload + payload / 4 + 1024 * 1024) + BLOCK - 1) / BLOCK * BLOCK;

		let opts = liberfs::FormatOpts { uuid: *b"libersystem-fix\0", label: b"system".to_vec(), compress: false };
		let disk = FixtureDisk { sectors: alloc::collections::BTreeMap::new() };
		let mut fs = liberfs::LiberFs::format_opts(disk, (size / BLOCK) as u64, opts).expect("format the fixture volume");
		let mut made: alloc::collections::BTreeSet<alloc::vec::Vec<u8>> = alloc::collections::BTreeSet::new();
		for index in 0..entries.len() {
			let Some(name) = entries.name(index) else { continue };
			let Some(bytes) = entries.lookup(name) else { continue };
			let mut prefix: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
			let segments: alloc::vec::Vec<&[u8]> = name.split(|&b| b == b'/').collect();
			for segment in &segments[..segments.len().saturating_sub(1)] {
				if !prefix.is_empty() {
					prefix.push(b'/');
				}
				prefix.extend_from_slice(segment);
				if made.insert(prefix.clone()) {
					fs.mkdir(&prefix).expect("fixture directory");
				}
			}
			fs.write_file(name, bytes).expect("fixture file");
		}

		// Prove the fixture mounts WRITABLE before handing it over.
		//
		// This asserted `mount(..).is_ok()`, and `is_ok()` is not the property the fixture needs:
		// `LiberFs::mount` answers `Ok` for a volume it has degraded to READ-ONLY because
		// `derive_free` found damage. So a format that produced damaged metadata handed back a
		// perfectly good-looking fixture, which is then CACHED in a static and cloned into every
		// later storage harness - one bad format poisoning every storage test in the suite.
		//
		// What that cost: on aarch64 the suite ran forty-eight tests past this point and then took
		// an unhandled data abort inside `imgconv`, reading an `&[u8; 8]` at address five. The
		// fault was four levels of consequence away from the cause, and the only clue was a line
		// StorageService printed about a read-only mount. A fixture that cannot be trusted has to
		// say so at the moment it is built, not leave the next forty tests to discover it.
		//
		// And it says WHICH KIND, because the categories drive completely different investigations:
		// a checksum failure is the medium, an I/O failure is the device, and a structural failure
		// is this code writing metadata that cannot be true - which is the only one of the three a
		// deterministic in-memory disk can produce.
		let mut disk = fs.into_device();
		let mut check = liberfs::LiberFs::mount(FixtureDisk { sectors: disk.sectors.clone() }).expect("the fixture volume does not mount");
		if check.is_read_only() {
			let report = check.fsck();
			match report {
				Ok(report) => {
					crate::serial_println!("fixture: LiberFS refuses to mount its own format writable - checksum {} io {} structural {} stream {}", report.checksum_failures, report.io_failures, report.structural_failures, report.stream_failures);
					for fault in report.faults.iter().take(8) {
						crate::serial_println!("fixture:   {}", core::str::from_utf8(fault).unwrap_or("<not utf-8>"));
					}
					for path in report.damaged.iter().take(8) {
						crate::serial_println!("fixture:   damaged: {}", core::str::from_utf8(path).unwrap_or("<not utf-8>"));
					}
				}
				Err(e) => crate::serial_println!("fixture: LiberFS refuses to mount its own format writable, and fsck could not run either: {e:?}"),
			}
			panic!("the fixture volume mounts READ-ONLY, so every storage test built on it is running against damaged metadata");
		}
		core::mem::take(&mut disk.sectors)
	}

	fn start_disk(storage_elf: &[u8], tag: &[u8], disk: alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>>, capacity: u64) -> Self {
		use object::channel::Channel;
		use object::rights::Rights;
		let (boot, boot_user) = Channel::create();
		let (block, block_child) = Channel::create();
		let (server, client) = Channel::create();
		let (admin, admin_child) = Channel::create();
		spawn_harness(storage_elf, boot_user);
		send_cap(&boot, tag, block_child, Rights::ALL).expect("storage block bootstrap");
		send_cap(&boot, b"ADMIN", admin_child, Rights::ALL).expect("storage admin bootstrap");
		send_cap(&boot, b"SERVE", server, Rights::ALL).expect("storage serve bootstrap");
		let mut harness = Self { boot, block, client, admin, disk, capacity, process: None, backing: Backing::Disk { tag: tag.to_vec() } };
		for _ in 0..100_000 {
			harness.pump();
			if let Ok(report) = harness.boot.recv() {
				assert_eq!(&report.bytes[..], b"StorageService: online");
				return harness;
			}
		}
		panic!("StorageService harness did not report online");
	}

	// Start a StorageService over one of the memory volumes. No block device is created and
	// none is handed over: the filesystem holds its files on the heap, so the tag carries the
	// capacity in bytes instead of a handle. That absence is the point - every other harness
	// below builds a disk first.
	fn start_memory(storage_elf: &[u8], tag: &[u8], bytes: usize) -> Self {
		use object::channel::{Channel, Message};
		use object::rights::Rights;
		let (boot, boot_user) = Channel::create();
		let (block, _unused) = Channel::create();
		let (server, client) = Channel::create();
		let (admin, admin_child) = Channel::create();
		let process = spawn_harness(storage_elf, boot_user);
		let mut request: alloc::vec::Vec<u8> = tag.to_vec();
		request.extend_from_slice(alloc::format!("{bytes}").as_bytes());
		boot.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("memory volume bootstrap");
		send_cap(&boot, b"ADMIN", admin_child, Rights::ALL).expect("storage admin bootstrap");
		send_cap(&boot, b"SERVE", server, Rights::ALL).expect("storage serve bootstrap");
		let mut harness = Self { boot, block, client, admin, disk: alloc::collections::BTreeMap::new(), capacity: bytes as u64, process: Some(process), backing: Backing::Memory { tag: tag.to_vec(), bytes } };
		for _ in 0..100_000 {
			harness.pump();
			if let Ok(report) = harness.boot.recv() {
				assert_eq!(&report.bytes[..], b"StorageService: online");
				return harness;
			}
		}
		panic!("memory StorageService harness did not report online");
	}

	// A READ-ONLY volume: the boot archive, handed over exactly as the kernel hands it to the
	// real service. There was no read-only volume in this harness at all, which is why "a stream
	// to a medium that cannot be written is refused before the first byte" had nothing to test
	// against.
	fn start_archive(storage_elf: &[u8]) -> Self {
		use object::channel::{Channel, Message};
		use object::memory_object::MemoryObject;
		use object::rights::Rights;
		let volume = volume_package_bytes().expect("volume package module not found");
		let (boot, boot_user) = Channel::create();
		let (block, _unused) = Channel::create();
		let (server, client) = Channel::create();
		let (admin, admin_child) = Channel::create();
		spawn_harness(storage_elf, boot_user);
		let ramdisk = MemoryObject::create(volume.len()).expect("no memory for the archive");
		copy_into_object(&ramdisk, volume);
		let mut request = alloc::vec::Vec::with_capacity(7 + 8);
		request.extend_from_slice(b"RAMDISK");
		request.extend_from_slice(&(volume.len() as u64).to_le_bytes());
		let cap = object::handle::Capability::new(ramdisk as alloc::sync::Arc<dyn object::KernelObject>, Rights::READ | Rights::MAP, 0);
		boot.send(Message::new(request, alloc::vec![cap], 0)).expect("archive volume bootstrap");
		send_cap(&boot, b"ADMIN", admin_child, Rights::ALL).expect("storage admin bootstrap");
		send_cap(&boot, b"SERVE", server, Rights::ALL).expect("storage serve bootstrap");
		// Restarting this one is not expressible: the volume is a MemoryObject handed over at
		// bootstrap, not a backing this harness can rebuild. No test asks, and `restart` would need
		// the archive bytes to try.
		let mut harness = Self { boot, block, client, admin, disk: alloc::collections::BTreeMap::new(), capacity: volume.len() as u64, process: None, backing: Backing::Memory { tag: alloc::vec::Vec::new(), bytes: 0 } };
		for _ in 0..100_000 {
			harness.pump();
			if let Ok(report) = harness.boot.recv() {
				assert_eq!(&report.bytes[..], b"StorageService: online");
				return harness;
			}
		}
		panic!("archive StorageService harness did not report online");
	}

	// How many handles the service holds right now.
	fn handle_count(&self) -> u64 {
		self.process.as_ref().expect("this harness did not keep the service process").handle_count()
	}

	// Restart the service over the SAME backing, which is the only restart that means anything: a
	// disk-backed volume must read its files back, and a memory-backed one must not.
	fn restart(mut self, storage_elf: &[u8]) -> Self {
		use object::channel::Message;
		self.client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("storage shutdown request");
		for _ in 0..100_000 {
			self.pump();
			if self.client.is_peer_closed() {
				let disk = core::mem::take(&mut self.disk);
				let capacity = self.capacity;
				let backing = self.backing.clone();
				drop(self);
				return match backing {
					Backing::Disk { tag } => Self::start_disk(storage_elf, &tag, disk, capacity),
					Backing::Memory { tag, bytes } => Self::start_memory(storage_elf, &tag, bytes),
				};
			}
		}
		panic!("StorageService harness did not shut down");
	}

	fn pump(&mut self) {
		sched::run_until_idle();
		pump_block_stand_in(&self.block, &mut self.disk, self.capacity);
	}

	fn connect(&mut self) -> alloc::sync::Arc<object::channel::Channel> {
		let client = self.client.clone();
		self.connect_from(&client)
	}

	fn connect_from(&mut self, client: &alloc::sync::Arc<object::channel::Channel>) -> alloc::sync::Arc<object::channel::Channel> {
		use object::channel::{Channel, Message};
		client.send(Message::new(abi::CONNECT_OP.to_le_bytes().to_vec(), alloc::vec::Vec::new(), 0)).expect("storage connect request");
		for _ in 0..100_000 {
			self.pump();
			if let Ok(reply) = client.recv() {
				let cap = reply.caps.first().expect("storage connection capability");
				return cap.object().into_any_arc().downcast::<Channel>().expect("storage connection is a channel");
			}
		}
		panic!("StorageService did not mint a connection");
	}

	fn open_directory(&mut self, path: &[u8]) -> alloc::sync::Arc<object::channel::Channel> {
		use object::channel::{Channel, Message};
		let corr: u32 = 0xd1ec_7000;
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&1u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		self.admin.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("storage directory request");
		for _ in 0..100_000 {
			self.pump();
			if let Ok(reply) = self.admin.recv() {
				assert_eq!(le_u32(&reply.bytes, 0), corr, "storage directory reply echoes the correlation id");
				assert_eq!(reply.bytes.get(4), Some(&1), "storage directory scope succeeds");
				let cap = reply.caps.first().expect("storage directory capability");
				return cap.object().into_any_arc().downcast::<Channel>().expect("storage directory scope is a channel");
			}
		}
		panic!("StorageService did not mint a directory scope");
	}

	// Mint a client scoped to EXACTLY ONE PATH - the selected-file grant, as `volume-admin.open-file`
	// answers it. `writable` decides whether it may open a transactional writer over that path.
	fn open_file(&mut self, path: &[u8], writable: bool) -> alloc::sync::Arc<object::channel::Channel> {
		use object::channel::{Channel, Message};
		let corr: u32 = 0xd1ec_8000;
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&2u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.push(u8::from(writable));
		self.admin.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("storage file request");
		for _ in 0..100_000 {
			self.pump();
			if let Ok(reply) = self.admin.recv() {
				assert_eq!(le_u32(&reply.bytes, 0), corr, "storage file reply echoes the correlation id");
				assert_eq!(reply.bytes.get(4), Some(&1), "storage file scope succeeds");
				let cap = reply.caps.first().expect("storage file capability");
				return cap.object().into_any_arc().downcast::<Channel>().expect("storage file scope is a channel");
			}
		}
		panic!("StorageService did not mint a file scope");
	}

	// Whether a client may open a transactional writer over `path`. The one op that separates a
	// read-only selected-file grant from a writable one.
	fn can_open_writer(&mut self, client: &alloc::sync::Arc<object::channel::Channel>, path: &[u8], corr: u32) -> bool {
		use object::channel::Message;
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&23u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.push(0);
		client.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("storage writer request");
		for _ in 0..100_000 {
			self.pump();
			if let Ok(reply) = client.recv() {
				assert_eq!(le_u32(&reply.bytes, 0), corr, "writer reply echoes the correlation id");
				return reply.bytes.get(4) == Some(&1);
			}
		}
		panic!("StorageService did not answer an open-writer request");
	}

	fn open(&mut self, path: &[u8], corr: u32) -> Option<alloc::vec::Vec<u8>> {
		let client = self.client.clone();
		self.open_from(&client, path, corr)
	}

	fn open_from(&mut self, client: &alloc::sync::Arc<object::channel::Channel>, path: &[u8], corr: u32) -> Option<alloc::vec::Vec<u8>> {
		use object::channel::Message;
		use object::memory_object::MemoryObject;
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&1u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.extend_from_slice(&[0, 0]);
		client.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("storage open request");
		for _ in 0..100_000 {
			self.pump();
			if let Ok(reply) = client.recv() {
				// A reply for ANOTHER correlation is not this call's answer. Skipping it matters
				// now that two operations can be outstanding at once: with a stream pending, its
				// reply may land here first, and treating that as this call's failure made the
				// concurrency test fail on the emulated architectures only - where the stream had
				// time to expire before the read was issued.
				if le_u32(&reply.bytes, 0) != corr {
					continue;
				}
				if reply.bytes.get(4) != Some(&1) {
					return None;
				}
				let size = le_u64(&reply.bytes, 9) as usize;
				let object = reply.caps.first()?.object().into_any_arc().downcast::<MemoryObject>().ok()?;
				return Some(read_from_object(&object, size));
			}
		}
		None
	}

	// Write through the STREAMING path: the service is handed a channel and reads chunks off it
	// until the sender closes.
	fn write_stream(&mut self, path: &[u8], chunks: &[&[u8]], corr: u32, oversized: Option<usize>) -> bool {
		use object::channel::{Channel, Message};
		use object::rights::Rights;
		let (service_side, our_side) = Channel::create();
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&16u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&self.client, &request, service_side, Rights::ALL).expect("storage write-stream request");
		for _ in 0..64 {
			self.pump();
		}
		for chunk in chunks {
			let _ = our_side.send(Message::new(chunk.to_vec(), alloc::vec::Vec::new(), 0));
			self.pump();
		}
		if let Some(len) = oversized {
			let _ = our_side.send(Message::new(alloc::vec![b'!'; len], alloc::vec::Vec::new(), 0));
			self.pump();
		}
		drop(our_side);
		for _ in 0..100_000 {
			self.pump();
			if let Ok(reply) = self.client.recv() {
				return le_u32(&reply.bytes, 0) == corr && reply.bytes.get(4) == Some(&1);
			}
		}
		false
	}

	// Open a write stream and see whether the service refuses it WITHOUT being sent anything.
	//
	// The distinction this draws is the whole point of the write plan: a refusal that arrives on
	// the strength of the destination alone, before the sender has offered a byte. Checking with
	// an empty stream does not draw it - dropping the sender ends the stream cleanly and the
	// refusal then comes from the write at the end, which is what happened before the plan existed
	// and would let a weaker implementation pass.
	//
	// Returns true when a failure reply arrives while the sender is still holding its end open.
	fn stream_refused_before_sending(&mut self, path: &[u8], corr: u32) -> bool {
		use object::channel::Channel;
		use object::rights::Rights;
		let (service_side, _our_side) = Channel::create();
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&16u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&self.client, &request, service_side, Rights::ALL).expect("storage write-stream request");
		// `_our_side` stays alive for the whole loop, so the stream is never closed and nothing is
		// ever sent: any reply here is a decision made about the destination.
		for _ in 0..20_000 {
			self.pump();
			if let Ok(reply) = self.client.recv() {
				return le_u32(&reply.bytes, 0) == corr && reply.bytes.get(4) == Some(&0);
			}
		}
		false
	}

	// A sender that never goes idle but never finishes: one byte, then pumping until the idle
	// window would have expired, over and over. Returns true if the service gave up on it.
	//
	// This is the case the idle deadline alone did NOT cover. That deadline is rebuilt after every
	// chunk, so a sender drip-feeding just under it renews its window forever while never being
	// idle - and the service, receiving synchronously, is unavailable to everyone else for as long
	// as the sender cares to continue.
	fn stream_slowloris(&mut self, path: &[u8], corr: u32, drips: usize, pumps_per_drip: usize) -> bool {
		use object::channel::{Channel, Message};
		use object::rights::Rights;
		let (service_side, our_side) = Channel::create();
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&16u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&self.client, &request, service_side, Rights::ALL).expect("storage write-stream request");
		for _ in 0..drips {
			let _ = our_side.send(Message::new(alloc::vec![b'.'], alloc::vec::Vec::new(), 0));
			for _ in 0..pumps_per_drip {
				self.pump();
				if let Ok(reply) = self.client.recv() {
					// The service ended it. A refusal is the pass: the sender is still holding the
					// channel open and still willing to send more.
					return le_u32(&reply.bytes, 0) == corr && reply.bytes.get(4) == Some(&0);
				}
			}
		}
		false
	}

	// Hand the service a TRUNCATED LiberFS image as a live volume.
	//
	// A live medium's system volume is copied into memory at boot, and a copy that fails half way
	// must be refused rather than served: a system missing executables that reports itself healthy
	// is worse than one that does not come up. The service exits on a failed import, so the test is
	// that it never reports itself online.
	//
	// Returns true if it came up anyway.
	fn live_volume_comes_up(storage_elf: &[u8], image: &[u8]) -> bool {
		use object::channel::{Channel, Message};
		use object::memory_object::MemoryObject;
		use object::rights::Rights;
		let (boot, boot_user) = Channel::create();
		let (block, _unused) = Channel::create();
		let (server, client) = Channel::create();
		let (admin, admin_child) = Channel::create();
		spawn_harness(storage_elf, boot_user);
		let buffer = MemoryObject::create(image.len().max(1)).expect("no memory for the live image");
		copy_into_object(&buffer, image);
		let mut request = alloc::vec::Vec::with_capacity(7 + 8);
		request.extend_from_slice(b"LIVEVOL");
		request.extend_from_slice(&(image.len() as u64).to_le_bytes());
		let cap = object::handle::Capability::new(buffer as alloc::sync::Arc<dyn object::KernelObject>, Rights::READ | Rights::MAP, 0);
		boot.send(Message::new(request, alloc::vec![cap], 0)).expect("live volume bootstrap");
		send_cap(&boot, b"ADMIN", admin_child, Rights::ALL).expect("storage admin bootstrap");
		send_cap(&boot, b"SERVE", server, Rights::ALL).expect("storage serve bootstrap");
		// As above: the live image arrives as a MemoryObject, so there is no backing to restart from.
		let mut harness = Self { boot, block, client, admin, disk: alloc::collections::BTreeMap::new(), capacity: image.len() as u64, process: None, backing: Backing::Memory { tag: alloc::vec::Vec::new(), bytes: 0 } };
		for _ in 0..100_000 {
			harness.pump();
			if let Ok(report) = harness.boot.recv() {
				return &report.bytes[..] == b"StorageService: online";
			}
		}
		false
	}

	// A LiberFS image as contiguous bytes, for the cases that need one in memory rather than as a
	// sector map: a live volume arrives as a buffer, not as a disk.
	fn fixture_image(archive: &[u8]) -> alloc::vec::Vec<u8> {
		let _ = archive;
		// A SMALL volume of its own rather than the scenario archive's.
		//
		// The scenario fixture is megabytes, and this is copied into a memory object and mounted
		// twice - whole and truncated. Under emulation that alone is minutes. Nothing here needs
		// the scenario's contents: the test is about an import that cannot be completed, and two
		// files prove it as well as two hundred.
		let sectors = Self::build_tiny_fixture();
		let mut bytes = alloc::vec::Vec::new();
		for (lba, sector) in &sectors {
			let offset = (*lba as usize) * 512;
			if bytes.len() < offset + sector.len() {
				bytes.resize(offset + sector.len(), 0);
			}
			bytes[offset..offset + sector.len()].copy_from_slice(sector);
		}
		bytes
	}

	// Ask for a listing on a channel and hang up before the reply can be delivered.
	//
	// The service mints the consumer, tries to hand it over, and finds nobody there. That send used
	// to be unchecked: the consumer leaked, and because it stayed open inside the service the
	// producer kept a live peer nobody would ever read - so the next send on it blocked forever.
	fn list_then_hang_up(&mut self, path: &[u8], corr: u32) {
		use object::channel::Message;
		let second = self.connect();
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&2u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		second.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("storage list request");
		// Gone before the service can answer.
		drop(second);
		for _ in 0..512 {
			self.pump();
		}
	}

	// Open a write stream with a handle the service cannot wait on.
	//
	// The interface says `handle<channel>` and no more, so a client may transfer a channel stripped
	// of WAIT - or something that is not a channel at all. Putting that into the shared wait set
	// makes every wait fail immediately, and a loop that retries on error then spins at full speed
	// serving nobody. Returns true if the service refused it.
	fn stream_with_rights(&mut self, path: &[u8], corr: u32, rights: object::rights::Rights) -> bool {
		use object::channel::Channel;
		let (service_side, _our_side) = Channel::create();
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&16u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&self.client, &request, service_side, rights).expect("storage write-stream request");
		// This CANNOT tell a prompt refusal from a thirty-second spin, and the attempt is recorded
		// rather than hidden. A service spinning on a wait it cannot perform keeps the run queue
		// non-empty, so `run_until_idle` never settles and each of these pumps takes as long as the
		// spin does - two thousand of them span the deadline, and the reply arrives either way.
		// It is the same structural limit that stopped `yield` from bounding a stalled send.
		//
		// What it does assert: the service answers rather than dying or going silent.
		for _ in 0..2_000 {
			self.pump();
			if let Ok(reply) = self.client.recv() {
				return le_u32(&reply.bytes, 0) == corr && reply.bytes.get(4) == Some(&0);
			}
		}
		false
	}

	// Register a write stream and RETURN, keeping the sender.
	//
	// The stream stays pending: nothing is sent, nothing is closed, and the service has answered
	// nothing yet. What the caller does next is the point - anything the service still does is
	// something it could not have done while receiving a stream synchronously.
	fn stream_pending(&mut self, path: &[u8], corr: u32) -> alloc::sync::Arc<object::channel::Channel> {
		use object::channel::Channel;
		use object::rights::Rights;
		let (service_side, our_side) = Channel::create();
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&16u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&self.client, &request, service_side, Rights::ALL).expect("storage write-stream request");
		// Enough for the service to take the request and register it, not enough for anything to
		// time out.
		for _ in 0..64 {
			self.pump();
		}
		our_side
	}

	// Send the chunks a pending stream is waiting for, then close it and collect the reply.
	fn stream_finish(&mut self, sender: alloc::sync::Arc<object::channel::Channel>, chunks: &[&[u8]], corr: u32) -> bool {
		use object::channel::Message;
		for chunk in chunks {
			let _ = sender.send(Message::new(chunk.to_vec(), alloc::vec::Vec::new(), 0));
			self.pump();
		}
		drop(sender);
		for _ in 0..100_000 {
			self.pump();
			if let Ok(reply) = self.client.recv() {
				return le_u32(&reply.bytes, 0) == corr && reply.bytes.get(4) == Some(&1);
			}
		}
		false
	}

	// Open a write stream, send NOTHING, and move the clock past the service's idle bound.
	//
	// The bound is a deadline, so without a way to move time this could only be waited out - about
	// a million scheduler passes. Returns true if the service gave the stream up.
	// Open a write stream from a SUBCLIENT, send one chunk, then drop that client entirely -
	// request channel and all - while the stream is still pending.
	//
	// Returns whether the service went on serving. What is being asked is not only that it
	// survives: the pending write held the one pending slot and the volume's memory, and its reply
	// was addressed to a handle that no longer named anything.
	fn stream_orphaned_by_client(&mut self, path: &[u8], corr: u32) -> bool {
		use object::channel::{Channel, Message};
		use object::rights::Rights;
		let sub = self.connect();
		let (service_side, our_side) = Channel::create();
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&16u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&sub, &request, service_side, Rights::ALL).expect("orphan write-stream request");
		for _ in 0..64 {
			self.pump();
		}
		let _ = our_side.send(Message::new(b"half a file".to_vec(), alloc::vec::Vec::new(), 0));
		self.pump();
		// ONLY the request channel goes. The stream stays open on purpose: closing it too would
		// end the stream cleanly, the file would be committed for good reason, and the test would
		// be measuring an ordinary completion instead of an orphan.
		drop(sub);
		for _ in 0..128 {
			self.pump();
		}
		drop(our_side);
		for _ in 0..32 {
			self.pump();
		}
		// Two separate facts, reported separately so a failure says which one broke: the service
		// still answers at all, and the pending SLOT is free for the next stream - which it would
		// not be if the orphan were still holding it.
		if !self.write(b"vol://tmp/after-orphan", b"x", corr.wrapping_add(1)) {
			return false;
		}
		let sender = self.stream_pending(b"vol://tmp/next", corr.wrapping_add(2));
		self.stream_finish(sender, &[b"kept"], corr.wrapping_add(2))
	}

	// Flood a SUBCLIENT's reply queue with heartbeats it never reads, then ask the service for
	// something on the root client.
	//
	// The heartbeat answered through an unbounded send. A client that asks and never listens fills
	// its reply queue, and the next `PONG` then held the whole service - every other client, every
	// volume, the admin endpoint - with no deadline to end it. The typed dispatch had been given a
	// bound and called the last such place, which was not true of the heartbeat, either `CONNECT`
	// form, or the two refusals.
	//
	// Returns whether the service is still serving. The clock is moved past the reply deadline so
	// the stalled subclient is given up on rather than waited out.
	fn heartbeat_flood_from_silent_client(&mut self, corr: u32, skip: u64) -> bool {
		use object::channel::Message;
		let sub = self.connect();
		// Send and let the service answer, over and over, WITHOUT ever reading a reply.
		//
		// Sending a burst first does not work and looked like it did: the request queue fills at the
		// same depth as the reply queue, so the extra sends are dropped and the service answers
		// exactly as many as the reply queue can hold - never one more, which is the one that
		// blocks. Interleaving keeps the requests flowing so the replies pile up past the depth.
		// Eighty, not two hundred: the reply queue is 64 deep, so the queue is full and the next
		// answer is blocking well before this ends. Every iteration is a full scheduler pass, which
		// under emulation is the most expensive thing this test does.
		for _ in 0..80 {
			let _ = sub.send(Message::new(abi::HEARTBEAT_OP.to_le_bytes().to_vec(), alloc::vec::Vec::new(), 0));
			self.pump();
		}
		advance_clock(skip);
		for _ in 0..128 {
			self.pump();
		}
		// The stalled client stays ALIVE across the probe. Dropping it first closes its end, which
		// releases a blocked send all by itself - so the service recovered for a reason that had
		// nothing to do with the deadline, and the test passed with the bound removed.
		let served = self.write(b"vol://tmp/after-flood", b"x", corr);
		drop(sub);
		served
	}

	// Fill the ROOT client's reply queue, let a reply time out, and then have the root ask for a
	// LISTING. Returns whether a client that connected beforehand is still served.
	//
	// The root, deliberately, because it is the one client the service never drops - closing it
	// would end the service. So a stalled root stays in the table with its queue full, and its next
	// request is exactly the case the bound has to cover.
	//
	// An earlier version flooded a SUBCLIENT and was green with the bound reverted: the service
	// drops a subclient on its first stalled reply, so the listing went to a channel that was
	// already closed and never reached the code under test. Only the revert showed it.
	fn root_lists_after_filling_its_reply_queue(&mut self, corr: u32, skip: u64) -> bool {
		use object::channel::Message;
		// Minted BEFORE the flood: the prober is a request/reply on the root, and once the root's
		// queue is full there is no way to ask for one.
		let prober = self.connect();
		for _ in 0..80 {
			let _ = self.client.send(Message::new(abi::HEARTBEAT_OP.to_le_bytes().to_vec(), alloc::vec::Vec::new(), 0));
			self.pump();
		}
		advance_clock(skip);
		for _ in 0..128 {
			self.pump();
		}
		// The listing, on a root whose queue is full and which has already stalled once.
		let path: &[u8] = b"vol://tmp";
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&2u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		let _ = self.client.send(Message::new(request, alloc::vec::Vec::new(), 0));
		for _ in 0..64 {
			self.pump();
		}
		// Past the listing's own deadline as well: a listing whose consumer nobody reads POLLS every
		// tick, so leaving one alive means `run_until_idle` never idles and the harness never gets
		// its turn back.
		advance_clock(skip);
		for _ in 0..128 {
			self.pump();
		}
		// Is anybody else still being served? Asked on the prober, because the root is the client
		// that stopped reading.
		let mut heartbeat = alloc::vec::Vec::new();
		heartbeat.extend_from_slice(&abi::HEARTBEAT_OP.to_le_bytes());
		if prober.send(Message::new(heartbeat, alloc::vec::Vec::new(), 0)).is_err() {
			return false;
		}
		for _ in 0..4_000 {
			self.pump();
			if let Ok(reply) = prober.recv() {
				return reply.bytes == b"PONG";
			}
		}
		false
	}

	// Time `rounds` heartbeat round-trips on one client, with `crowd` other clients connected and
	// silent. Returns the nanoseconds per round trip.
	//
	// The measurement P02M0117 exists for. What is being fixed there is a SLOPE - the cost of a pass
	// against the number of clients the service listens to - and there was no harness that reported
	// it, so "it got faster" was a feeling and "it hung" was the whole signal a failure gave. Three
	// numbers at three crowd sizes say in one run what a night of bisecting does not.
	fn round_trip_ns_with_crowd(&mut self, crowd: usize, rounds: usize) -> u64 {
		use object::channel::{Channel, Message};
		let mut held: Vec<alloc::sync::Arc<Channel>> = alloc::vec::Vec::new();
		// Connected in batches: a round trip per connection would cost more than the measurement.
		const BATCH: usize = 16;
		let mut asked = 0usize;
		while asked < crowd {
			let batch = BATCH.min(crowd - asked);
			for _ in 0..batch {
				self.client.send(Message::new(abi::CONNECT_OP.to_le_bytes().to_vec(), alloc::vec::Vec::new(), 0)).expect("connect");
			}
			asked += batch;
			let mut answered = 0usize;
			for _ in 0..2_000 {
				self.pump();
				while let Ok(reply) = self.client.recv() {
					answered += 1;
					if let Some(cap) = reply.caps.first() {
						held.push(cap.object().into_any_arc().downcast::<Channel>().expect("a connection is a channel"));
					}
				}
				if answered == batch {
					break;
				}
			}
			assert_eq!(answered, batch, "the service answered every connect while building a crowd of {crowd}");
		}
		// The prober is one of the crowd, so what is timed is a client like any other.
		let prober = held.last().cloned().unwrap_or_else(|| self.client.clone());
		let started = arch::tsc::now();
		for _ in 0..rounds {
			prober.send(Message::new(abi::HEARTBEAT_OP.to_le_bytes().to_vec(), alloc::vec::Vec::new(), 0)).expect("heartbeat");
			let mut answered = false;
			for _ in 0..10_000 {
				self.pump();
				if prober.recv().is_ok() {
					answered = true;
					break;
				}
			}
			assert!(answered, "the service answered every heartbeat with a crowd of {crowd}");
		}
		let elapsed = arch::tsc::cycles_to_ns(arch::tsc::now().wrapping_sub(started));
		elapsed / rounds as u64
	}

	// Connect over and over until the service refuses, and report how many it granted.
	//
	// A refusal is an empty reply carrying no capability, which is what this call answers with when
	// the table is full or a channel cannot be minted. `None` means it never refused.
	fn connect_until_refused(&mut self, limit: usize) -> Option<usize> {
		use object::channel::{Channel, Message};
		let mut granted: Vec<alloc::sync::Arc<Channel>> = alloc::vec::Vec::new();
		for _ in 0..limit {
			self.client.send(Message::new(abi::CONNECT_OP.to_le_bytes().to_vec(), alloc::vec::Vec::new(), 0)).expect("storage connect request");
			let mut answered = false;
			for _ in 0..2_000 {
				self.pump();
				if let Ok(reply) = self.client.recv() {
					match reply.caps.first() {
						Some(cap) => granted.push(cap.object().into_any_arc().downcast::<Channel>().expect("a connection is a channel")),
						// the refusal form: an empty reply with nothing in it.
						None => return Some(granted.len()),
					}
					answered = true;
					break;
				}
			}
			assert!(answered, "the service answered every connect it was given");
		}
		None
	}

	fn stream_idle_until_deadline(&mut self, path: &[u8], corr: u32, skip: u64) -> bool {
		use object::channel::Channel;
		use object::rights::Rights;
		let (service_side, _our_side) = Channel::create();
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&16u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&self.client, &request, service_side, Rights::ALL).expect("storage write-stream request");
		// Let the service reach its receive before time moves, so the deadline it computes is the
		// one being tested rather than one already in the past.
		for _ in 0..256 {
			self.pump();
		}
		advance_clock(skip);
		// `_our_side` is held for the whole loop: the sender is present and silent, which is the
		// case the bound exists for. Dropping it would end the stream cleanly and prove nothing.
		for _ in 0..100_000 {
			self.pump();
			if let Ok(reply) = self.client.recv() {
				return le_u32(&reply.bytes, 0) == corr && reply.bytes.get(4) == Some(&0);
			}
		}
		false
	}

	// Ask for a listing, take the consumer, and never read from it.
	//
	// The service produces entries into a channel nobody drains; past the channel's queue depth the
	// send blocks, and an unbounded one held the whole service there. Returns the consumer so the
	// caller can keep it open - dropping it would let the send fail for a different reason.
	fn list_without_reading(&mut self, path: &[u8], corr: u32) -> Option<alloc::sync::Arc<object::channel::Channel>> {
		use object::channel::{Channel, Message};
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&2u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		self.client.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("storage list request");
		for _ in 0..100_000 {
			self.pump();
			if let Ok(reply) = self.client.recv() {
				if le_u32(&reply.bytes, 0) != corr {
					continue;
				}
				return reply.caps.first().and_then(|cap| cap.object().into_any_arc().downcast::<Channel>().ok());
			}
		}
		None
	}

	// The four path verbs, hand-encoded like every other request in this harness: an op, a
	// correlation id, a length-prefixed path and whatever the op adds after it. Each returns
	// whether the service answered `ok`, which is the whole of what these calls promise.
	fn stat(&mut self, path: &[u8], corr: u32) -> Option<(u64, bool)> {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&17u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		let reply = self.request(request, corr)?;
		if reply.bytes.get(4) != Some(&1) {
			return None;
		}
		// [corr 4][ok 1][name lp][size u64][type u8]...
		let name_len = le_u16(&reply.bytes, 5) as usize;
		let at = 7 + name_len;
		Some((le_u64(&reply.bytes, at), reply.bytes.get(at + 8) == Some(&1)))
	}

	fn rename(&mut self, from: &[u8], to: &[u8], corr: u32) -> bool {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&18u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(from.len() as u16).to_le_bytes());
		request.extend_from_slice(from);
		request.extend_from_slice(&(to.len() as u16).to_le_bytes());
		request.extend_from_slice(to);
		self.request(request, corr).is_some_and(|reply| reply.bytes.get(4) == Some(&1))
	}

	fn truncate(&mut self, path: &[u8], length: u64, corr: u32) -> bool {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&19u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.extend_from_slice(&length.to_le_bytes());
		self.request(request, corr).is_some_and(|reply| reply.bytes.get(4) == Some(&1))
	}

	fn touch(&mut self, path: &[u8], create: bool, corr: u32) -> bool {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&20u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.push(u8::from(create));
		// Zero: this harness has no wall clock to offer, so the service stamps its own.
		request.extend_from_slice(&0u64.to_le_bytes());
		self.request(request, corr).is_some_and(|reply| reply.bytes.get(4) == Some(&1))
	}

	// One window of a file. `None` is a refusal; an empty vector is the end of the file, and the
	// difference between the two is the point of the call.
	fn read_window(&mut self, path: &[u8], offset: u64, length: u32, corr: u32) -> Option<alloc::vec::Vec<u8>> {
		use object::memory_object::MemoryObject;
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&21u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.extend_from_slice(&offset.to_le_bytes());
		request.extend_from_slice(&length.to_le_bytes());
		let reply = self.request(request, corr)?;
		if reply.bytes.get(4) != Some(&1) {
			return None;
		}
		// A `buffer` is a handle and a length; unlike `handle<T>` in a record it writes no
		// placeholder, so the length starts right after the tag.
		let len = le_u64(&reply.bytes, 5) as usize;
		let object = reply.caps.first()?.object().into_any_arc().downcast::<MemoryObject>().ok()?;
		Some(read_from_object(&object, len))
	}

	// Subscribe to a path's changes; the channel is the consumer end of the event stream.
	fn watch(&mut self, path: &[u8], corr: u32) -> Option<alloc::sync::Arc<object::channel::Channel>> {
		use object::channel::Channel;
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&22u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		let reply = self.request(request, corr)?;
		if reply.bytes.get(4) != Some(&1) {
			return None;
		}
		reply.caps.first().and_then(|cap| cap.object().into_any_arc().downcast::<Channel>().ok())
	}

	// Open a transactional writer; the channel speaks the `writer` interface.
	fn open_writer(&mut self, path: &[u8], append: bool, corr: u32) -> Option<alloc::sync::Arc<object::channel::Channel>> {
		use object::channel::Channel;
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&23u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.push(u8::from(append));
		let reply = self.request(request, corr)?;
		if reply.bytes.get(4) != Some(&1) {
			return None;
		}
		reply.caps.first().and_then(|cap| cap.object().into_any_arc().downcast::<Channel>().ok())
	}

	// Drive one op on an open writer session and report whether it succeeded.
	fn writer_op(&mut self, session: &alloc::sync::Arc<object::channel::Channel>, request: alloc::vec::Vec<u8>, corr: u32) -> Option<alloc::vec::Vec<u8>> {
		use object::channel::Message;
		session.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("writer request");
		for _ in 0..100_000 {
			self.pump();
			if let Ok(reply) = session.recv() {
				if le_u32(&reply.bytes, 0) != corr {
					continue;
				}
				return if reply.bytes.get(4) == Some(&1) { Some(reply.bytes) } else { None };
			}
		}
		None
	}

	// Send a request on the root client and collect the reply that carries `corr`.
	//
	// Replies for OTHER correlations are skipped rather than treated as this call's answer: more
	// than one operation can be outstanding at a time, and a watcher's event or a stream's reply
	// landing first is not this call's failure.
	fn request(&mut self, request: alloc::vec::Vec<u8>, corr: u32) -> Option<object::channel::Message> {
		use object::channel::Message;
		self.client.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("storage request");
		for _ in 0..100_000 {
			self.pump();
			if let Ok(reply) = self.client.recv() {
				if le_u32(&reply.bytes, 0) != corr {
					continue;
				}
				return Some(reply);
			}
		}
		None
	}

	fn write(&mut self, path: &[u8], data: &[u8], corr: u32) -> bool {
		use object::memory_object::MemoryObject;
		use object::rights::Rights;
		let buffer = MemoryObject::create(data.len().max(1)).expect("storage write buffer");
		copy_into_object(&buffer, data);
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&3u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.extend_from_slice(&(data.len() as u64).to_le_bytes());
		send_cap(&self.client, &request, buffer, Rights::READ | Rights::MAP | Rights::TRANSFER).expect("storage write request");
		for _ in 0..100_000 {
			self.pump();
			if let Ok(reply) = self.client.recv() {
				return le_u32(&reply.bytes, 0) == corr && reply.bytes.get(4) == Some(&1);
			}
		}
		false
	}
}

fn fat16_image(files: &[([u8; 11], &[u8])], fill_free: bool) -> alloc::vec::Vec<u8> {
	fat16_image_with_clusters(files, fill_free, 5_000)
}

// The same image with a chosen data capacity. The default 5,000 sectors is 2.5 MB, which is
// ample for the small fixtures every other case writes and far too small for a 4K PNG: a
// conversion that runs to completion and then cannot store its result reports `cannot write
// output`, which reads exactly like a memory failure while being a property of the test's
// media instead. Sizing the medium is part of measuring the conversion.
fn fat16_image_with_clusters(files: &[([u8; 11], &[u8])], fill_free: bool, clusters: usize) -> alloc::vec::Vec<u8> {
	const SECTOR: usize = 512;
	let cluster_count: usize = clusters;
	const RESERVED: usize = 1;
	const ROOT_ENTRIES: usize = 512;
	let fat_sectors = ((cluster_count + 2) * 2).div_ceil(SECTOR);
	let root_sectors = (ROOT_ENTRIES * 32).div_ceil(SECTOR);
	let first_data = RESERVED + fat_sectors + root_sectors;
	let total = first_data + cluster_count;
	let mut image = alloc::vec![0u8; total * SECTOR];
	let fat_offset = RESERVED * SECTOR;
	image[fat_offset..fat_offset + 2].copy_from_slice(&0xfff8u16.to_le_bytes());
	image[fat_offset + 2..fat_offset + 4].copy_from_slice(&0xffffu16.to_le_bytes());
	let root_offset = (RESERVED + fat_sectors) * SECTOR;
	// Files span as many clusters as they need, chained through the FAT. This used to be one cluster
	// per file with an assertion that nothing exceeded 512 bytes, which was true of every image
	// fixture and false of the first audio one: a quarter of a second of mono is eight clusters.
	let mut next_free = 2usize;
	for (index, (name, data)) in files.iter().enumerate() {
		assert!(index < ROOT_ENTRIES, "the fixture has more files than the root directory holds");
		let needed = data.len().div_ceil(SECTOR).max(1);
		assert!(next_free - 2 + needed <= cluster_count, "the fixture image is too small for its files");
		let first = next_free;
		for step in 0..needed {
			let cluster = first + step;
			let fat = fat_offset + cluster * 2;
			let link = if step + 1 == needed { 0xffffu16 } else { (cluster + 1) as u16 };
			image[fat..fat + 2].copy_from_slice(&link.to_le_bytes());
			let start = step * SECTOR;
			let end = (start + SECTOR).min(data.len());
			if start < end {
				let data_offset = (first_data + cluster - 2) * SECTOR;
				image[data_offset..data_offset + (end - start)].copy_from_slice(&data[start..end]);
			}
		}
		next_free = first + needed;
		let entry = root_offset + index * 32;
		image[entry..entry + 11].copy_from_slice(name);
		image[entry + 11] = 0x20;
		image[entry + 26..entry + 28].copy_from_slice(&(first as u16).to_le_bytes());
		image[entry + 28..entry + 32].copy_from_slice(&(data.len() as u32).to_le_bytes());
	}
	if fill_free {
		for cluster in next_free..cluster_count + 2 {
			let fat = fat_offset + cluster * 2;
			image[fat..fat + 2].copy_from_slice(&0xffffu16.to_le_bytes());
		}
	}
	image[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
	image[13] = 1;
	image[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
	image[16] = 1;
	image[17..19].copy_from_slice(&(ROOT_ENTRIES as u16).to_le_bytes());
	image[19..21].copy_from_slice(&(total as u16).to_le_bytes());
	image[22..24].copy_from_slice(&(fat_sectors as u16).to_le_bytes());
	image[510] = 0x55;
	image[511] = 0xaa;
	image
}

fn spawn_dynamic_test_process(domain: alloc::sync::Arc<object::domain::Domain>, main: &[u8], bootstrap: alloc::sync::Arc<object::channel::Channel>) -> alloc::sync::Arc<object::process::Process> {
	fn load(package: &pkg::Package<'_>, process: &object::process::Process, name: &str, loaded: &mut alloc::vec::Vec<alloc::string::String>, visiting: &mut alloc::vec::Vec<alloc::string::String>) {
		if loaded.iter().any(|item| item == name) {
			return;
		}
		assert!(!visiting.iter().any(|item| item == name), "dynamic test provider cycle");
		visiting.push(alloc::string::String::from(name));
		let path = test_library_path(name).expect("dynamic test provider has a manifest destination");
		let bytes = package.lookup(path.as_bytes()).expect("dynamic test provider is staged");
		let elf = bootproto::elf::Elf::parse(bytes).expect("dynamic test provider is ELF");
		let dynamic = elf.dynamic_info().expect("provider dynamic metadata parses").expect("provider has PT_DYNAMIC");
		for dependency in elf.needed_names(&dynamic).expect("provider dependencies parse") {
			load(package, process, dependency, loaded, visiting);
		}
		let bias = 0x2000_0000 + loaded.len() as u64 * 0x0100_0000;
		loader::load_module_into(process, bytes, bias).expect("load dynamic test provider");
		visiting.pop();
		loaded.push(alloc::string::String::from(name));
	}

	let volume = volume_package_bytes().expect("volume package present");
	let package = pkg::Package::parse(volume).expect("volume package parses");
	let process = object::process::Process::new(object::address_space::AddressSpace::create().expect("dynamic test address space"), domain).expect("dynamic test process");
	let elf = bootproto::elf::Elf::parse(main).expect("dynamic test main is ELF");
	let dynamic = elf.dynamic_info().expect("main dynamic metadata parses").expect("main has PT_DYNAMIC");
	let mut loaded = alloc::vec::Vec::new();
	let mut visiting = alloc::vec::Vec::new();
	for dependency in elf.needed_names(&dynamic).expect("main dependencies parse") {
		load(&package, &process, dependency, &mut loaded, &mut visiting);
	}
	let entry = loader::load_image_into(&process, main).expect("load dynamic test main");
	let bootstrap = process.install(bootstrap, object::rights::Rights::ALL, 0).expect("a bootstrap handle");
	let thread = loader::create_user_thread(&process, entry, memlayout::USER_STACK_TOP, bootstrap).expect("create dynamic test thread");
	assert!(sched::thread_start(thread), "start dynamic test thread");
	process
}

// Run a tool that holds volumes and nothing else, and capture the one line it prints.
//
// Named for what it does rather than for its first caller: `imgconv` and `audioconv` are the same
// shape of program - a bundle of volume capabilities, an argument string, a line of output, an exit
// - and the harness was already generic when it was called `run_imgconv_harness`.
fn run_volume_tool_result(domain: alloc::sync::Arc<object::domain::Domain>, tool_elf: &[u8], args: &[u8], system: &mut StorageHarness, media: &mut StorageHarness) -> (Option<alloc::vec::Vec<u8>>, u64) {
	use object::channel::{Channel, Message};
	use object::rights::Rights;
	let (bootstrap, child) = Channel::create();
	let (stdout, child_stdout) = Channel::create();
	let process = spawn_dynamic_test_process(domain.clone(), tool_elf, child);
	send_cap(&bootstrap, b"STDOUT", child_stdout, Rights::ALL).expect("the tool's stdout");
	bootstrap.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
	bootstrap.send(Message::new(launch_context(args, b"vol://system"), alloc::vec::Vec::new(), 0)).expect("the tool's arguments");
	send_cap(&bootstrap, b"SYSTEM", system.client.clone(), Rights::ALL).expect("the tool's system volume");
	send_cap(&bootstrap, b"MEDIA", media.client.clone(), Rights::ALL).expect("the tool's media volume");
	for tag in [b"ISO".as_slice(), b"UDF".as_slice(), b"USB".as_slice(), b"RAM".as_slice(), b"TMP".as_slice()] {
		bootstrap.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("an absent volume");
	}
	bootstrap.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("volume bundle terminator");
	let mut line = None;
	for _ in 0..100_000 {
		system.pump();
		media.pump();
		if line.is_none()
			&& let Ok(message) = stdout.recv()
		{
			line = Some(message.bytes);
		}
		if line.is_some() && process.is_terminated() {
			break;
		}
	}
	assert!(process.is_terminated(), "the tool exits");
	(line, domain.account().memory().peak())
}

fn run_volume_tool_in(domain: alloc::sync::Arc<object::domain::Domain>, tool_elf: &[u8], args: &[u8], system: &mut StorageHarness, media: &mut StorageHarness) -> (alloc::vec::Vec<u8>, u64) {
	let (line, peak) = run_volume_tool_result(domain, tool_elf, args, system, media);
	(line.expect("the tool prints a result"), peak)
}

fn run_volume_tool(tool_elf: &[u8], args: &[u8], system: &mut StorageHarness, media: &mut StorageHarness) -> alloc::vec::Vec<u8> {
	run_volume_tool_in(sched::root_domain(), tool_elf, args, system, media).0
}

fn viewer_surface(image: &pix::RgbaImage) -> alloc::vec::Vec<u8> {
	let source = image.to_bgrx().expect("viewer source converts to BGRX");
	let mut output = alloc::vec![0u8; 16];
	let result = pix::blit(pix::Image { data: &source, width: image.width, height: image.height, pitch: image.pitch }, pix::Target { data: &mut output, width: 2, height: 2, pitch: 8, bytes_per_pixel: 4, red_shift: 16, red_size: 8, green_shift: 8, green_size: 8, blue_shift: 0, blue_size: 8 }, pix::Rect { x: 0, y: 0, width: image.width, height: image.height }, true);
	assert!(result.is_some(), "expected viewer pixels render");
	output
}

fn run_imgview_help_harness(imgview_elf: &[u8], system: &mut StorageHarness, media: &mut StorageHarness) {
	use object::channel::{Channel, Message};
	use object::rights::Rights;
	let (bootstrap, child) = Channel::create();
	let (stdout, child_stdout) = Channel::create();
	let process = spawn_dynamic_test_process(sched::root_domain(), imgview_elf, child);
	send_cap(&bootstrap, b"STDOUT", child_stdout, Rights::ALL).expect("imgview help stdout");
	bootstrap.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
	bootstrap.send(Message::new(crate::tests::launch_context(b"--help", b"vol://system"), alloc::vec::Vec::new(), 0)).expect("imgview help args");
	send_cap(&bootstrap, b"SYSTEM", system.client.clone(), Rights::ALL).expect("imgview help system volume");
	send_cap(&bootstrap, b"MEDIA", media.client.clone(), Rights::ALL).expect("imgview help media volume");
	for tag in [b"ISO".as_slice(), b"UDF".as_slice(), b"USB".as_slice(), b"RAM".as_slice(), b"TMP".as_slice()] {
		bootstrap.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("imgview help absent capability");
	}
	// The bundle ends here; DISPLAY and INPUT_KEYS are separate grants that follow it.
	bootstrap.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("volume bundle terminator");
	for tag in [b"DISPLAY".as_slice(), b"INPUT_KEYS".as_slice()] {
		bootstrap.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("imgview help absent capability");
	}
	let output = loop {
		system.pump();
		media.pump();
		if let Ok(message) = stdout.recv() {
			break message.bytes;
		}
	};
	assert_eq!(output, b"Usage: imgview <image>\nDisplays a still image or composited animation frame 0; animation playback is not supported.\nControls: +/= zoom in, - zoom out, hold arrows to pan, Esc/q quit.\n");
	for _ in 0..100_000 {
		system.pump();
		media.pump();
		if process.is_terminated() {
			return;
		}
	}
	panic!("imgview help harness did not exit");
}

#[derive(Clone, Copy)]
enum ImgviewExit {
	KeyQ,
	KeyEscape,
	RawEscape,
	ZoomAndHold,
}

fn run_imgview_harness(imgview_elf: &[u8], path: &[u8], expected: &[u8], system: &mut StorageHarness, media: &mut StorageHarness) {
	run_imgview_harness_with_exit(imgview_elf, path, expected, system, media, ImgviewExit::KeyQ);
}

fn run_imgview_harness_with_exit(imgview_elf: &[u8], path: &[u8], expected: &[u8], system: &mut StorageHarness, media: &mut StorageHarness, exit: ImgviewExit) {
	use object::channel::{Channel, Message};
	use object::memory_object::MemoryObject;
	use object::rights::Rights;
	let (bootstrap, child) = Channel::create();
	let (stdout, child_stdout) = Channel::create();
	let (display, display_client) = Channel::create();
	let (input, input_client) = Channel::create();
	let process = spawn_dynamic_test_process(sched::root_domain(), imgview_elf, child);
	send_cap(&bootstrap, b"STDOUT", child_stdout, Rights::ALL).expect("imgview stdout");
	bootstrap.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
	bootstrap.send(Message::new(launch_context(path, b"vol://system"), alloc::vec::Vec::new(), 0)).expect("imgview args");
	send_cap(&bootstrap, b"SYSTEM", system.client.clone(), Rights::ALL).expect("imgview system volume");
	send_cap(&bootstrap, b"MEDIA", media.client.clone(), Rights::ALL).expect("imgview media volume");
	for tag in [b"ISO".as_slice(), b"UDF".as_slice(), b"USB".as_slice(), b"RAM".as_slice(), b"TMP".as_slice()] {
		bootstrap.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("imgview absent volume");
	}
	bootstrap.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("volume bundle terminator");
	send_cap(&bootstrap, b"DISPLAY", display_client, Rights::ALL).expect("imgview display");
	send_cap(&bootstrap, b"INPUT_KEYS", input_client, Rights::ALL).expect("imgview input");

	let acquire = loop {
		system.pump();
		media.pump();
		if let Ok(request) = display.recv() {
			break request;
		}
	};
	assert_eq!(le_u16(&acquire.bytes, 0), 1, "imgview acquires a surface");
	let surface = MemoryObject::create(16).expect("imgview surface");
	let mut reply = alloc::vec::Vec::new();
	reply.extend_from_slice(&le_u32(&acquire.bytes, 2).to_le_bytes());
	reply.push(1);
	reply.extend_from_slice(&16u64.to_le_bytes());
	reply.extend_from_slice(&2u32.to_le_bytes());
	reply.extend_from_slice(&2u32.to_le_bytes());
	reply.extend_from_slice(&8u32.to_le_bytes());
	reply.push(0);
	send_cap(&display, &reply, surface.clone(), Rights::ALL).expect("imgview acquire reply");

	let present = loop {
		system.pump();
		media.pump();
		if let Ok(request) = display.recv() {
			break request;
		}
	};
	assert_eq!(le_u16(&present.bytes, 0), 2, "imgview presents converted image");
	assert_eq!(read_from_object(&surface, 16), expected, "imgview presents the expected alpha-converted composited frame");
	display.send(Message::new([le_u32(&present.bytes, 2).to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).expect("imgview present reply");

	let focus = loop {
		system.pump();
		media.pump();
		if let Ok(request) = display.recv() {
			break request;
		}
	};
	assert_eq!(le_u16(&focus.bytes, 0), 5, "imgview requests focus");
	let (_focus_server, focus_client) = Channel::create();
	let mut focus_reply = alloc::vec::Vec::new();
	focus_reply.extend_from_slice(&le_u32(&focus.bytes, 2).to_le_bytes());
	focus_reply.push(1);
	focus_reply.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&display, &focus_reply, focus_client, Rights::ALL).expect("imgview focus reply");

	let subscribe = loop {
		system.pump();
		media.pump();
		if let Ok(request) = input.recv() {
			break request;
		}
	};
	assert_eq!(le_u16(&subscribe.bytes, 0), 2, "imgview subscribes to keys");
	let (keys, key_consumer) = Channel::create();
	send_cap(&input, &le_u32(&subscribe.bytes, 2).to_le_bytes(), key_consumer, Rights::ALL).expect("imgview key stream");
	match exit {
		ImgviewExit::KeyQ => {
			keys.send(Message::new(alloc::vec![0, 0, 0, 0, 0x14, 0, 1], alloc::vec::Vec::new(), 0)).expect("imgview q key");
		}
		ImgviewExit::KeyEscape => {
			keys.send(Message::new(alloc::vec![0, 0, 0, 0, 0x29, 0, 1], alloc::vec::Vec::new(), 0)).expect("imgview escape key");
		}
		ImgviewExit::RawEscape => {
			stdout.send(Message::new(alloc::vec![0x1b], alloc::vec::Vec::new(), 0)).expect("imgview raw escape");
		}
		ImgviewExit::ZoomAndHold => {
			let send_key = |code: u16, pressed: bool| {
				keys.send(Message::new(alloc::vec![0, 0, 0, 0, code as u8, (code >> 8) as u8, pressed as u8], alloc::vec::Vec::new(), 0)).expect("imgview interaction key");
			};
			send_key(0x4f, true);
			system.pump();
			media.pump();
			assert!(display.recv().is_err(), "fit-to-screen arrow must not redraw or auto-zoom");
			send_key(0x4f, false);
			for iteration in 0..8 {
				let zoom_in_code = if iteration % 2 == 0 { 0x2e } else { 0x57 };
				send_key(zoom_in_code, true);
				let request = loop {
					system.pump();
					media.pump();
					if let Ok(request) = display.recv() {
						break request;
					}
				};
				assert_eq!(le_u16(&request.bytes, 0), 2, "imgview interaction redraws with a present request");
				display.send(Message::new([le_u32(&request.bytes, 2).to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).expect("imgview interaction present reply");
				send_key(zoom_in_code, false);
			}
			for zoom_out_code in [0x56, 0x2d] {
				send_key(zoom_out_code, true);
				let request = loop {
					system.pump();
					media.pump();
					if let Ok(request) = display.recv() {
						break request;
					}
				};
				assert_eq!(le_u16(&request.bytes, 0), 2, "imgview interaction zooms out with a present request");
				display.send(Message::new([le_u32(&request.bytes, 2).to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).expect("imgview zoom-out present reply");
				send_key(zoom_out_code, false);
			}
			for _ in 0..20 {
				send_key(0x57, true);
				let request = loop {
					system.pump();
					media.pump();
					if let Ok(request) = display.recv() {
						break request;
					}
				};
				assert_eq!(le_u16(&request.bytes, 0), 2, "imgview interaction zooms with keypad plus");
				display.send(Message::new([le_u32(&request.bytes, 2).to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).expect("imgview keypad zoom present reply");
				send_key(0x57, false);
			}
			stdout.send(Message::new(alloc::vec![0x1b, b'[', b'C'], alloc::vec::Vec::new(), 0)).expect("imgview serial right arrow");
			let request = loop {
				system.pump();
				media.pump();
				if let Ok(request) = display.recv() {
					break request;
				}
			};
			assert_eq!(le_u16(&request.bytes, 0), 2, "imgview serial arrow redraws with a present request");
			display.send(Message::new([le_u32(&request.bytes, 2).to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).expect("imgview serial-arrow present reply");
			send_key(0x4f, true);
			let mut repeat_presents = 0usize;
			for _ in 0..1_000 {
				system.pump();
				media.pump();
				while let Ok(request) = display.recv() {
					assert_eq!(le_u16(&request.bytes, 0), 2, "imgview held arrow redraws with a present request");
					display.send(Message::new([le_u32(&request.bytes, 2).to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).expect("imgview held-arrow present reply");
					repeat_presents += 1;
				}
				if repeat_presents >= 2 {
					break;
				}
				arch::idle_halt();
			}
			assert!(repeat_presents >= 2, "held arrow must produce repeated pan redraws");
			send_key(0x4f, false);
			for _ in 0..10 {
				system.pump();
				media.pump();
				assert!(display.recv().is_err(), "released arrow must stop repeated pan redraws");
			}
			stdout.send(Message::new(alloc::vec![0x1b], alloc::vec::Vec::new(), 0)).expect("imgview serial escape");
		}
	}

	let release = loop {
		system.pump();
		media.pump();
		if let Ok(request) = display.recv() {
			break request;
		}
	};
	assert_eq!(le_u16(&release.bytes, 0), 3, "imgview releases its surface");
	display.send(Message::new([le_u32(&release.bytes, 2).to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).expect("imgview release reply");
	for _ in 0..100_000 {
		system.pump();
		media.pump();
		if process.is_terminated() {
			return;
		}
	}
	panic!("imgview harness did not exit");
}

fn run_lico_harness(lico_elf: &[u8], system: &mut StorageHarness) {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	let (bootstrap, child) = Channel::create();
	let (terminal, terminal_child) = Channel::create();
	let process = spawn_dynamic_test_process(sched::root_domain(), lico_elf, child);
	send_cap(&bootstrap, b"STDOUT", terminal_child, Rights::ALL).expect("lico terminal bootstrap");
	bootstrap.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("endpoint run terminator");
	bootstrap.send(Message::new(launch_context(b"", b"vol://system"), alloc::vec::Vec::new(), 0)).expect("lico empty arguments");
	send_cap(&bootstrap, b"SYSTEM", system.client.clone(), Rights::ALL).expect("lico system volume");
	for tag in [b"MEDIA".as_slice(), b"ISO".as_slice(), b"UDF".as_slice(), b"USB".as_slice(), b"RAM".as_slice(), b"TMP".as_slice()] {
		bootstrap.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("lico absent volume");
	}
	bootstrap.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("volume bundle terminator");
	// THE TWO GRANTS THIS HARNESS WITHHOLDS, sent as bare messages so the tag arrives and the
	// capability does not. `lico` takes a launch broker and its own asset directory from
	// PermissionManager; here it gets neither, which is a state it has to run in - a manager that
	// blocked forever waiting for a grant nobody sent would be one that could not start on a boot
	// that granted less than the full set.
	bootstrap.send(Message::new(b"PERMISSION".to_vec(), alloc::vec::Vec::new(), 0)).expect("lico absent launch broker");
	bootstrap.send(Message::new(b"APP_ASSETS".to_vec(), alloc::vec::Vec::new(), 0)).expect("lico absent asset directory");

	let mut output = alloc::vec::Vec::new();
	let mut rendered = false;
	for _ in 0..100_000 {
		system.pump();
		while let Ok(message) = terminal.recv() {
			rendered |= message.bytes.starts_with(b"\x1b[H\x1b[2J\x1b[1mlico\x1b[0m");
			output.push(message.bytes);
		}
		if rendered {
			break;
		}
	}
	assert!(rendered, "lico renders both panels before waiting for input");
	assert_eq!(output.get(0).map(Vec::as_slice), Some(b"\x1b[?1049h".as_slice()), "lico enters the alternate screen first");
	// By CONTENT, not by position. This read message four, which was the mouse request only while
	// two tty-mode escapes sat in front of it - and those are gone: raw and echo are a request on
	// the terminal's control channel now, not bytes a file could also contain. A positional
	// assertion on a byte stream breaks whenever the stream is one message shorter, which says
	// nothing about what the program asked for.
	assert!(output.iter().any(|line| line.as_slice() == b"\x1b[?1000h"), "lico requests pointer press reports");
	// AND THE MODE ESCAPES ARE NOT THERE AT ALL. This is the property the change bought: no output
	// a program can produce - and therefore no file it can print - sets the tty's modes.
	assert!(!output.iter().any(|line| line.windows(4).any(|w| w == b"9001" || w == b"9002")), "the tty's modes are not settable from the output stream");
	let initial: alloc::vec::Vec<u8> = output.iter().flat_map(|line| line.iter().copied()).collect();
	assert!(initial.windows(b">vol://system".len()).any(|window| window == b">vol://system"), "left panel begins active");

	terminal.send(Message::new(b"\t".to_vec(), alloc::vec::Vec::new(), 0)).expect("lico tab focus input");
	let mut switched = false;
	for _ in 0..100_000 {
		system.pump();
		while let Ok(message) = terminal.recv() {
			switched |= message.bytes.windows(b" | >vol://system".len()).any(|window| window == b" | >vol://system");
			output.push(message.bytes);
		}
		if switched {
			break;
		}
	}
	assert!(switched, "Tab moves the active panel to the right side");

	// F7 MAKES A DIRECTORY, and this is the first thing in this test that changes the volume rather
	// than the screen. It is worth having at exactly this layer: the mkdir path runs through the
	// prompt mode, the volume client the panel's own URI names, and the refresh afterwards - and a
	// manager that drew a directory it had not created, or created one and did not show it, would
	// look identical from any one of those three alone.
	terminal.send(Message::new(b"\x1b[18~".to_vec(), alloc::vec::Vec::new(), 0)).expect("lico F7 input");
	for byte in b"licodir" {
		terminal.send(Message::new(alloc::vec![*byte], alloc::vec::Vec::new(), 0)).expect("lico directory name");
	}
	terminal.send(Message::new(b"\r".to_vec(), alloc::vec::Vec::new(), 0)).expect("lico confirms the name");
	let mut created = false;
	for _ in 0..200_000 {
		system.pump();
		while let Ok(message) = terminal.recv() {
			created |= message.bytes.windows(b"licodir/".len()).any(|window| window == b"licodir/");
			output.push(message.bytes);
		}
		if created {
			break;
		}
	}
	assert!(created, "F7 creates the directory and the panel shows it, with the trailing separator that marks one");

	terminal.send(Message::new(b"\x1b[21~".to_vec(), alloc::vec::Vec::new(), 0)).expect("lico F10 input");
	for _ in 0..100_000 {
		system.pump();
		while let Ok(message) = terminal.recv() {
			output.push(message.bytes);
		}
		if process.is_terminated() {
			break;
		}
	}
	assert!(process.is_terminated(), "lico exits after F10");
	// The tty's raw and echo modes are NOT in this list any more: they are requests on the
	// terminal's control channel, not escapes in the output stream. What is left is the terminal's
	// own state - mouse reporting, the cursor, the alternate screen - which a program may set for
	// its own screen and which no file's contents can misuse.
	let restore = [b"\x1b[?1006l".as_slice(), b"\x1b[?1000l".as_slice(), b"\x1b[?25h".as_slice(), b"\x1b[?1049l".as_slice()];
	let mut cursor = 0;
	for expected in restore {
		let position = output[cursor..].iter().position(|line| line == expected).expect("lico restores every terminal mode in order");
		cursor += position + 1;
	}
}

// Kernel-thread body that drops to ring 3 running the embedded cooperative-yield
// program. Each instance takes a distinct slot so two can be alive on the same
// core at once (their user pages share the kernel address space at non-overlapping
// virtual addresses). The program yields several times before reporting in, so two
// instances interleave through the scheduler.
extern "C" fn user_yield_thread_body(handle: u64) {
	use core::sync::atomic::{AtomicU64, Ordering};
	use mem::frame::{self, PAGE_SIZE};
	static SLOT: AtomicU64 = AtomicU64::new(0);
	let slot = SLOT.fetch_add(1, Ordering::Relaxed);
	let code_va = 0x0000_0000_5000_0000 + slot * 0x0010_0000;
	let stack_va = code_va + 0x0001_0000;
	let code = frame::allocate().expect("user code frame");
	let stack = frame::allocate().expect("user stack frame");
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
	arch::paging::map_page(code_va, code, flags);
	arch::paging::map_page(stack_va, stack, flags | arch::paging::NO_EXECUTE);
	let program = arch::usermode::program_yield_bytes();
	unsafe {
		arch::paging::copy_to_user_page(code_va, program);
		arch::usermode::enter(code_va, stack_va + PAGE_SIZE, handle);
	}
	arch::paging::unmap_page(code_va);
	arch::paging::unmap_page(stack_va);
	unsafe { frame::deallocate(code) };
	unsafe { frame::deallocate(stack) };
}
