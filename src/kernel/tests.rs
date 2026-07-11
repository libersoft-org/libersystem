// The kernel test suite and its scenario helpers (custom_test_frameworks, runs
// under `cargo test` in QEMU). Everything here is test-only: the ring-3 probe
// programs and their thread bodies, the packaged-scenario drivers the service
// tests build on, the Testable harness, and the test cases themselves. The boot
// path and the helpers it shares with the suite (the module locators, the
// SystemManager spawn and the supervise ladder) stay in main.rs.

use super::*;

// Userspace (ring 3) page layout for the test: one USER page for the program,
// one for its stack, mapped into the low half of the shared address space
// (per-process page tables / CR3 isolation are a later milestone).
use crate::memlayout::{USER_CODE_VA, USER_STACK_VA};

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
	frame::deallocate(code);
	frame::deallocate(stack);
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
// package; every other service, manager and demo component is staged on the system volume
// under `bin/` (M61 box 8), so fall back to the volume there. The returned slice borrows
// the 'static module data, so it outlives the temporary volume Package.
fn program_elf(package: &pkg::Package<'static>, volume: &'static [u8], name: &[u8]) -> Option<&'static [u8]> {
	if let Some(elf) = package.lookup(name) {
		return Some(elf);
	}
	let mut path: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	path.extend_from_slice(b"bin/");
	path.extend_from_slice(name);
	pkg::Package::parse(volume).and_then(|p| p.lookup(&path))
}

// Send a tagged capability over a bootstrap channel: wrap `object` in a Capability
// carrying `rights` and send it with `payload` as the message bytes. The shared
// "hand a process one of its initial capabilities" step the scenarios repeat.
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

// Build the M16 storage topology and run it to completion. A MemoryObject holds
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
	let service_elf = package.lookup(b"storage_service").ok_or("storage_service missing from the init package")?;
	let client_elf = program_elf(&package, volume, b"storage_client").ok_or("storage_client missing from the package or volume")?;

	// channels: a bootstrap per process, plus the service<->client request channel
	let (service_boot_kernel, service_boot_user) = Channel::create();
	let (client_boot_kernel, client_boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();

	// spawn the two processes with their bootstrap endpoints
	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), service_elf, service_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	loader::spawn_elf_process(domain, client_elf, client_boot_user, Rights::ALL, 0).map_err(|_| "failed to load the storage client")?;

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

// Build the M28 WASI topology and run it to completion. A StorageService serves the
// ramdisk volume; the wasi_host process loads the embedded Wasm component and runs
// it, and the component's only import (`liber.read`) is wired by the host to read
// the granted file vol://system/hello.txt through StorageService into the
// component's linear memory. The component has no other capability - no ambient
// authority. The host reports the bytes the component read back over its bootstrap
// channel. The kernel only brokers the initial capabilities. Returns (expected,
// actual): the file straight from the volume, and the bytes the component read.
fn run_wasi_scenario() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>), &'static str> {
	use object::channel::Channel;
	use object::rights::Rights;

	let (volume, package) = scenario_packages()?;
	let expected = volume_file(volume, b"hello.txt")?;
	let storage_elf = package.lookup(b"storage_service").ok_or("storage_service missing from the init package")?;
	let host_elf = program_elf(&package, volume, b"wasi_host").ok_or("wasi_host missing from the package or volume")?;

	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (host_boot_kernel, host_boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();

	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	loader::spawn_elf_process(domain, host_elf, host_boot_user, Rights::ALL, 0).map_err(|_| "failed to load wasi_host")?;

	// storage bootstrap: the ramdisk volume and its service channel; the host gets
	// only the StorageService client - the one capability it is granted.
	send_ramdisk(&storage_boot_kernel, volume)?;
	send_cap(&storage_boot_kernel, b"SERVE", service_server, Rights::ALL)?;
	send_cap(&host_boot_kernel, b"STORAGE", service_client, Rights::ALL)?;

	sched::run_until_idle();
	let result = host_boot_kernel.recv().map_err(|_| "the host reported no result")?;
	Ok((expected, result.bytes))
}

// Build the M29 powerbox topology and run it to completion. A StorageService serves
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
	let storage_elf = package.lookup(b"storage_service").ok_or("storage_service missing from the init package")?;
	let picker_elf = program_elf(&package, volume, b"file_picker").ok_or("file_picker missing from the package or volume")?;
	let host_elf = program_elf(&package, volume, b"wasi_host").ok_or("wasi_host missing from the package or volume")?;

	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (picker_boot_kernel, picker_boot_user) = Channel::create();
	let (host_boot_kernel, host_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	let (picker_server, picker_client) = Channel::create();

	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	loader::spawn_elf_process(domain.clone(), picker_elf, picker_boot_user, Rights::ALL, 0).map_err(|_| "failed to load file_picker")?;
	loader::spawn_elf_process(domain, host_elf, host_boot_user, Rights::ALL, 0).map_err(|_| "failed to load wasi_host")?;

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
	Ok((expected, result.bytes))
}

// Build the M38 permission topology and run it to completion. A StorageService serves
// the ramdisk volume; a ProcessService is the loading mechanism; a TimeService serves the
// wall clock; the permission_manager (PermissionManager) is given the clients it may grant
// onward - a duplicable StorageService client, a duplicable (but dead-peer) LogService
// client, and a TimeService client - plus a NetworkService client it holds but is NOT to
// grant, a ProcessService client it drives to load components, and the channel its clients
// reach it on. PermissionManager governs four components through ProcessService, each under
// a typed permission manifest. Two are report-back probes: sandbox_probe (granted storage and
// log but not network - it transfers exactly those two clients and withholds the network one)
// and request_probe (granted only log, which then asks for an undeclared capability - storage
// - at runtime), recording every decision. The other two it launches on demand through its
// `run` op (the launcher / granter path), each printing to a captured stdout: `date` (granted
// only time) renders the wall clock, and `cat` (granted only storage) prints a file. Each
// sandboxed component reaches only its granted capabilities: sandbox_probe reads its one
// granted file vol://system/hello.txt through the storage grant and reports the bytes back;
// `date` reads the wall clock through the time grant and prints the rendered instant to its
// captured stdout; request_probe's runtime request is refused by the headless policy default
// (least privilege - an undeclared capability is never granted) and recorded as a dynamic
// denial; and `cat` prints that file through its storage grant to the forwarded stdout. The
// kernel only brokers the initial capabilities. Returns (expected,
// probe_read, probe_summary, date_read, date_summary, request_read, request_summary,
// cat_read): the file straight from the volume, then each component's proof and decisions
// summary, then the bytes `cat` printed through the run launcher.
fn run_permission_scenario() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>), &'static str> {
	use object::channel::Channel;
	use object::rights::Rights;

	let (volume, package) = scenario_packages()?;
	let init = init_package_bytes().ok_or("init package module not found")?;
	let expected = volume_file(volume, b"hello.txt")?;
	let storage_elf = package.lookup(b"storage_service").ok_or("storage_service missing from the init package")?;
	let process_elf = package.lookup(b"process_service").ok_or("process_service missing from the init package")?;
	let time_elf = program_elf(&package, volume, b"time_service").ok_or("time_service missing from the package or volume")?;
	let pm_elf = program_elf(&package, volume, b"permission_manager").ok_or("permission_manager missing from the package or volume")?;

	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (process_boot_kernel, process_boot_user) = Channel::create();
	let (time_boot_kernel, time_boot_user) = Channel::create();
	let (pm_boot_kernel, pm_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	let (process_server, process_client) = Channel::create();
	let (time_server, time_client) = Channel::create();
	let (perm_server, _perm_client) = Channel::create();
	// The manager's log grant: a real, duplicable client whose service peer is dropped, so
	// the sandboxed probe's best-effort log emit fails fast instead of blocking (no
	// LogService runs in this scenario). The capability is still granted and audited.
	let (log_server, log_client) = Channel::create();
	core::mem::drop(log_server);
	// The manager's network capability: held, but never granted to the probe.
	let (_net_server, net_client) = Channel::create();
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

	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	loader::spawn_elf_process(domain.clone(), process_elf, process_boot_user, Rights::ALL, 0).map_err(|_| "failed to load ProcessService")?;
	loader::spawn_elf_process(domain.clone(), time_elf, time_boot_user, Rights::ALL, 0).map_err(|_| "failed to load TimeService")?;
	loader::spawn_elf_process(domain, pm_elf, pm_boot_user, Rights::ALL, 0).map_err(|_| "failed to load PermissionManager")?;

	// StorageService: the ramdisk volume and its service channel.
	send_ramdisk(&storage_boot_kernel, volume)?;
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL)?;

	// ProcessService: the init package (the bring-up fallback) and its service channel,
	// plus a StorageService client so it loads the components PermissionManager governs
	// from the system volume's bin/ (M61 box 8) - the loading mechanism, kept separate
	// from the granting policy. The client is a duplicate of the manager's storage
	// connection; the cooperative schedule serializes the reads, so sharing it is safe.
	send_package(&process_boot_kernel, init)?;
	send_cap(&process_boot_kernel, b"STORAGE", storage_client.clone(), Rights::ALL)?;
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
	send_cap(&pm_boot_kernel, b"STORAGE", storage_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"LOG", log_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"NETWORK", net_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"TIME", time_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"CONFIG", config_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"DEVICE", device_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"AUDIO", audio_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"RESOURCE", resource_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"PROCESS_GRANT", process_grant_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"SUPERVISOR", supervisor_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"STORAGE_MEDIA", storage_media_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"STORAGE_ISO", storage_iso_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"STORAGE_UDF", storage_udf_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"STORAGE_USB", storage_usb_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"SERVICES", services_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"USBBUS", usb_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"PROCESS", process_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"SERVE", perm_server, Rights::ALL)?;

	sched::run_until_idle();

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
	Ok((expected, probe_read.bytes, probe_summary.bytes, date_read.bytes, date_summary.bytes, request_read.bytes, request_summary.bytes, cat_read.bytes))
}

// Build the M41 component topology and run it to completion. A StorageService serves
// the ramdisk volume and a LogService holds the journal; the component_host is given
// exactly two capabilities - a StorageService client and a LogService client - and
// nothing else. It loads a real Wasm component (built by the Rust SDK, served from
// storage as vol://system/app.wasm rather than embedded in the kernel image) and runs
// it: the component's three imports are wired by name to the two services - `read` /
// `write` to StorageService, `log` to LogService - with no ambient authority. The
// component reads its one granted file, upper-cases it, logs the result through
// LogService, writes it back, and returns the count; the host also calls the
// component's float `score` export. The kernel only brokers the initial capabilities.
// Returns (expected, content, logged, score): the upper-cased granted file, the bytes
// the component produced, whether the log grant was reached, and the float result.
fn run_component_scenario() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, bool, i32), &'static str> {
	use object::channel::Channel;
	use object::rights::Rights;

	let (volume, package) = scenario_packages()?;
	let raw = volume_file(volume, b"hello.txt")?;
	let expected: alloc::vec::Vec<u8> = raw.iter().map(|b: &u8| b.to_ascii_uppercase()).collect();
	let storage_elf = package.lookup(b"storage_service").ok_or("storage_service missing from the init package")?;
	let log_elf = package.lookup(b"log_service").ok_or("log_service missing from the init package")?;
	let host_elf = program_elf(&package, volume, b"component_host").ok_or("component_host missing from the package or volume")?;

	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (log_boot_kernel, log_boot_user) = Channel::create();
	let (host_boot_kernel, host_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	let (log_server, log_client) = Channel::create();

	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).map_err(|_| "failed to load StorageService")?;
	loader::spawn_elf_process(domain.clone(), log_elf, log_boot_user, Rights::ALL, 0).map_err(|_| "failed to load LogService")?;
	loader::spawn_elf_process(domain, host_elf, host_boot_user, Rights::ALL, 0).map_err(|_| "failed to load component_host")?;

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
	let bytes: alloc::vec::Vec<u8> = result.bytes;
	if bytes.len() < 5 {
		return Err("the host report was too short");
	}
	let logged: bool = bytes[0] != 0;
	let score: i32 = i32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
	let content: alloc::vec::Vec<u8> = bytes[5..].to_vec();
	Ok((expected, content, logged, score))
}

// Build the M39 resource topology and run it to completion. The resource_manager
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
	loader::spawn_elf_process(domain, rm_elf, rm_boot_user, Rights::ALL, 0).map_err(|_| "failed to load ResourceManager")?;

	// ResourceManager: the init package (to launch the probe from) and the channel its
	// clients reach it on. The order matches ResourceManager's receive order: PACKAGE, SERVE.
	send_package(&rm_boot_kernel, init)?;
	send_cap(&rm_boot_kernel, b"SERVE", resource_server, Rights::ALL)?;

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
	let service_elf = package.lookup(b"storage_service").ok_or("storage_service missing from the init package")?;

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
	frame::deallocate(code);
	frame::deallocate(stack);
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
	frame::deallocate(code);
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
	frame::deallocate(code);
	frame::deallocate(stack);
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
		let irq = arch::syscall::invoke(syscall::SYS_INTERRUPT_BIND, DRIVER_IRQ_VECTOR, 0, 0, 0);
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
	frame::deallocate(code);
	frame::deallocate(stack);
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
}

impl<T: Fn()> Testable for T {
	fn run(&self) {
		serial_print!("{}...\t", core::any::type_name::<T>());
		self();
		serial_println!("[ok]");
	}
}

pub(crate) fn test_runner(tests: &[&dyn Testable]) {
	serial_println!("running {} tests", tests.len());
	for test in tests {
		test.run();
	}
	arch::exit_qemu(true);
}

#[test_case]
fn trivial_assertion() {
	assert_eq!(1 + 1, 2);
}

#[cfg(target_arch = "x86_64")]
#[test_case]
fn breakpoint_exception_returns() {
	// reaching the next line proves the IDT breakpoint handler returned cleanly
	unsafe { core::arch::asm!("int3") };
}

#[cfg(target_arch = "riscv64")]
#[test_case]
fn breakpoint_exception_returns() {
	// reaching the next line proves the trap handler resumed past the ebreak: it decodes
	// the trapped instruction width (2 bytes for a compressed c.ebreak, else 4) and
	// advances sepc, the riscv analogue of x86's int3 breakpoint round-trip.
	unsafe { core::arch::asm!("ebreak") };
}

#[test_case]
fn frame_alloc_distinct() {
	let a = mem::frame::allocate().expect("frame a");
	let b = mem::frame::allocate().expect("frame b");
	assert_ne!(a, b);
	mem::frame::deallocate(a);
	mem::frame::deallocate(b);
}

#[test_case]
fn contiguous_frames_and_dma_spans() {
	use mem::frame::{self, PAGE_SIZE};
	use object::domain::Domain;
	// The contiguous-run allocator: a multi-page allocation is one physical span,
	// freeing re-coalesces it, and a DmaBuffer built on it reports strictly
	// consecutive frames - the property virtqueue rings, whole-request block data
	// stages and jumbo frames stand on.
	let base = frame::allocate_contiguous(64).expect("a 256 kB span");
	// the span is really ours page by page: freeing it and re-fitting a LARGER
	// span still succeeds (coalescing reassembled the run rather than splitting it)
	for i in 0..64u64 {
		frame::deallocate(base + i * PAGE_SIZE);
	}
	let again = frame::allocate_contiguous(128).expect("a 512 kB span after coalescing");
	for i in 0..128u64 {
		frame::deallocate(again + i * PAGE_SIZE);
	}
	// a DmaBuffer's frames are consecutive, so a device sees one run
	let domain = Domain::new(1 << 24, 8, 4);
	let dma = match object::dma_buffer::DmaBuffer::create_in(&domain, 6 * PAGE_SIZE as usize) {
		Ok(d) => d,
		Err(_) => panic!("a 6-page DMA buffer should allocate"),
	};
	let frames = dma.frames();
	assert_eq!(frames.len(), 6);
	for pair in frames.windows(2) {
		assert_eq!(pair[1], pair[0] + PAGE_SIZE, "DMA frames are physically contiguous");
	}
	assert_eq!(dma.phys_base(), frames[0]);
	drop(dma);
	assert_eq!(domain.account().dma().used(), 0, "the DMA charge is refunded");
}

#[test_case]
fn the_frame_pool_grows_past_the_boot_table_and_refuses_a_double_free() {
	use mem::frame::{self, PAGE_SIZE};
	// The run table is heap-backed after boot, so pathological fragmentation grows
	// it instead of leaking frames past a fixed size: freeing every other page of a
	// 4096-page span leaves 2048 disjoint single-page runs - far past the old fixed
	// table - and none may be lost. Freeing the other half re-coalesces the span
	// whole (the free count round-trips exactly and a big contiguous fit works).
	let before = frame::free_count();
	let base = frame::allocate_contiguous(4096).expect("a 16 MB span");
	for i in (0..4096u64).step_by(2) {
		frame::deallocate(base + i * PAGE_SIZE);
	}
	for i in (1..4096u64).step_by(2) {
		frame::deallocate(base + i * PAGE_SIZE);
	}
	assert_eq!(frame::free_count(), before, "every fragmented page returned to the pool");
	let again = frame::allocate_contiguous(4096).expect("the span re-coalesced whole");
	// A double free is refused: freeing a page inside the pool's free runs must
	// not be honored (it would let the same frame be handed out twice).
	frame::deallocate(again);
	let after_free = frame::free_count();
	frame::deallocate(again);
	assert_eq!(frame::free_count(), after_free, "a double free adds nothing to the pool");
	// Hand the rest of the span back (page 0 is already free).
	for i in 1..4096u64 {
		frame::deallocate(again + i * PAGE_SIZE);
	}
	assert_eq!(frame::free_count(), before, "the pool round-trips exactly");
}

#[test_case]
fn map_degrades_to_error_when_out_of_frames() {
	use mem::frame;
	use object::address_space::AddressSpace;
	// A userspace-triggered map must degrade, not panic, when the frame pool is
	// empty: the walk cannot allocate an intermediate page table and returns an
	// error the map syscalls turn into ERR_NO_MEMORY. A fresh address space has an
	// empty user half, so mapping a low (user) VA is guaranteed to need a new
	// intermediate table.
	let space = AddressSpace::create().expect("a fresh address space");
	let leaf = frame::allocate().expect("one frame to point the leaf at");
	// Drain the rest of the pool. Reserve the holding vector first so it never
	// grows (mapping a heap page) inside the drained window.
	let mut held: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
	held.reserve(frame::free_count() + 8);
	while let Some(f) = frame::allocate() {
		held.push(f);
	}
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER | arch::paging::NO_EXECUTE;
	let result = space.try_map(0x1_0000, leaf, flags);
	// Refill the pool BEFORE asserting, so a failed assertion never leaves it
	// drained. `leaf` stays ours until the end.
	for f in held {
		frame::deallocate(f);
	}
	assert!(result.is_err(), "an out-of-frames map must fail cleanly, not panic");
	// The failed map left nothing behind: the same VA maps fine now the pool is back.
	space.try_map(0x1_0000, leaf, flags).expect("the map succeeds once frames are available");
	space.unmap(0x1_0000);
	frame::deallocate(leaf);
}

#[test_case]
fn paging_map_unmap() {
	let phys = mem::frame::allocate().expect("scratch frame");
	// Sv39 (riscv64) only has a 39-bit canonical VA range, so the 48-bit x86/aarch64
	// scratch address below is non-canonical there and faults; use a free canonical
	// kernel-half VA just past the riscv mmap window (KERNEL_MMAP_BASE + 64 GiB).
	#[cfg(not(target_arch = "riscv64"))]
	let virt: u64 = 0xffff_f000_0000_0000;
	#[cfg(target_arch = "riscv64")]
	let virt: u64 = 0xffff_fff0_0000_0000;
	arch::paging::map_page(virt, phys, arch::paging::WRITABLE);
	let ptr = virt as *mut u64;
	unsafe {
		ptr.write_volatile(0xdead_beef);
		assert_eq!(ptr.read_volatile(), 0xdead_beef);
	}
	let unmapped = arch::paging::unmap_page(virt).expect("was mapped");
	assert_eq!(unmapped, phys);
	mem::frame::deallocate(phys);
}

#[test_case]
fn heap_box_vec() {
	let boxed = alloc::boxed::Box::new(42u64);
	assert_eq!(*boxed, 42);
	let mut v = alloc::vec::Vec::new();
	for i in 0u64..1000 {
		v.push(i);
	}
	let sum: u64 = v.iter().sum();
	assert_eq!(sum, 1000 * 999 / 2);
}

#[test_case]
fn timer_ticks_advance() {
	// Interrupts are enabled by kmain before the tests run, so the periodic
	// LAPIC timer must keep incrementing the tick counter.
	let start = arch::apic::ticks();
	while arch::apic::ticks() == start {
		core::hint::spin_loop();
	}
	assert!(arch::apic::ticks() > start);
}

#[cfg(target_arch = "x86_64")]
#[test_case]
fn handler_registration_dispatch() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static FIRED: AtomicBool = AtomicBool::new(false);
	fn handler(_vector: u8) {
		FIRED.store(true, Ordering::SeqCst);
	}
	// Register on an unused device vector and trigger it with a software
	// interrupt: proves registration and dispatch wiring without a device.
	arch::interrupts::register(47, handler);
	unsafe { core::arch::asm!("int 0x2f", options(nomem, nostack)) };
	assert!(FIRED.load(Ordering::SeqCst));
}

#[test_case]
fn smp_all_cores_online() {
	// init_smp ran before the tests and waited for every core to report in, so
	// the online count must equal the managed core count (and exceed one when
	// QEMU is given more than a single CPU).
	assert_eq!(smp::online_count(), smp::cpu_count());
}

// A minimal kernel object used only to exercise the object/capability core.
struct TestObject {
	header: object::ObjectHeader,
	value: u64,
}

impl TestObject {
	fn new(value: u64) -> alloc::sync::Arc<Self> {
		alloc::sync::Arc::new(Self { header: object::ObjectHeader::new(), value })
	}

	fn value(&self) -> u64 {
		self.value
	}
}

impl object::KernelObject for TestObject {
	fn header(&self) -> &object::ObjectHeader {
		&self.header
	}

	fn object_type(&self) -> object::ObjectType {
		object::ObjectType::Event
	}

	fn as_any(&self) -> &dyn core::any::Any {
		self
	}

	fn into_any_arc(self: alloc::sync::Arc<Self>) -> alloc::sync::Arc<dyn core::any::Any + Send + Sync> {
		self
	}
}

#[test_case]
fn handle_create_lookup_close() {
	use object::handle::{HandleError, HandleTable};
	use object::rights::Rights;
	let mut table = HandleTable::new();
	let obj = TestObject::new(42);
	let h = table.insert_object(obj, Rights::READ | Rights::WRITE, 0);
	assert_eq!(table.len(), 1);
	let looked = table.lookup(h, Rights::READ).expect("lookup");
	assert_eq!(looked.as_any().downcast_ref::<TestObject>().unwrap().value(), 42);
	table.close(h).expect("close");
	assert_eq!(table.len(), 0);
	// A closed handle must no longer resolve.
	assert!(matches!(table.lookup(h, Rights::READ), Err(HandleError::BadHandle)));
}

#[test_case]
fn handle_rights_enforced() {
	use object::handle::{HandleError, HandleTable};
	use object::rights::Rights;
	let mut table = HandleTable::new();
	let h = table.insert_object(TestObject::new(7), Rights::READ, 0);
	assert!(table.lookup(h, Rights::READ).is_ok());
	// A right the handle does not carry is denied.
	assert!(matches!(table.lookup(h, Rights::WRITE), Err(HandleError::AccessDenied)));
}

#[test_case]
fn handle_duplicate_attenuates() {
	use object::handle::{HandleError, HandleTable};
	use object::rights::Rights;
	let mut table = HandleTable::new();
	let h = table.insert_object(TestObject::new(1), Rights::READ | Rights::WRITE | Rights::DUPLICATE, 0);
	// A duplicate may drop rights...
	let weak = table.duplicate(h, Rights::READ).expect("duplicate");
	assert!(table.lookup(weak, Rights::READ).is_ok());
	assert!(matches!(table.lookup(weak, Rights::WRITE), Err(HandleError::AccessDenied)));
	// ...but never gain a right the original lacked.
	assert!(matches!(table.duplicate(h, Rights::EXECUTE), Err(HandleError::AccessDenied)));
	// Without the DUPLICATE right, duplication itself is denied.
	let plain = table.insert_object(TestObject::new(2), Rights::READ, 0);
	assert!(matches!(table.duplicate(plain, Rights::READ), Err(HandleError::AccessDenied)));
}

#[test_case]
fn handle_revocation_invalidates() {
	use object::handle::{HandleError, HandleTable};
	use object::rights::Rights;
	let mut table = HandleTable::new();
	let obj = TestObject::new(99);
	let h = table.insert_object(obj.clone(), Rights::READ, 0);
	assert!(table.lookup(h, Rights::READ).is_ok());
	// Revoking the object invalidates every existing handle to it at once.
	obj.header.revoke();
	assert!(matches!(table.lookup(h, Rights::READ), Err(HandleError::Revoked)));
}

#[test_case]
fn handle_type_sealing() {
	use object::ObjectType;
	use object::handle::{HandleError, HandleTable};
	use object::rights::Rights;
	let mut table = HandleTable::new();
	let h = table.insert_object(TestObject::new(5), Rights::READ, 0);
	assert!(table.lookup_typed(h, ObjectType::Event, Rights::READ).is_ok());
	// The same handle cannot be used where a different object type is expected.
	assert!(matches!(table.lookup_typed(h, ObjectType::Channel, Rights::READ), Err(HandleError::WrongType)));
}

#[test_case]
fn handle_refcount_lifetime() {
	use alloc::sync::Arc;
	use object::handle::HandleTable;
	use object::rights::Rights;
	let mut table = HandleTable::new();
	let obj = TestObject::new(3);
	assert_eq!(Arc::strong_count(&obj), 1);
	let h = table.insert_object(obj.clone(), Rights::READ, 0);
	assert_eq!(Arc::strong_count(&obj), 2);
	let looked = table.lookup(h, Rights::READ).expect("lookup");
	assert_eq!(Arc::strong_count(&obj), 3);
	drop(looked);
	assert_eq!(Arc::strong_count(&obj), 2);
	// Closing the handle drops the kernel's last reference held via the table.
	table.close(h).expect("close");
	assert_eq!(Arc::strong_count(&obj), 1);
}

#[test_case]
fn thread_object_basics() {
	use object::address_space::AddressSpace;
	use object::process::Process;
	use object::thread::{Thread, ThreadState};
	use object::{KernelObject, ObjectType};
	extern "C" fn noop(_arg: u64) {}
	let process = Process::new(AddressSpace::kernel(), sched::root_domain());
	let thread = Thread::new(noop, 0, process);
	assert_eq!(thread.object_type(), ObjectType::Thread);
	assert_eq!(thread.state(), ThreadState::Ready);
	assert!(thread.tid() >= 1);
}

#[test_case]
fn scheduler_multiplexes_threads() {
	use core::sync::atomic::{AtomicU32, Ordering};
	static COUNTER: AtomicU32 = AtomicU32::new(0);
	static DONE: AtomicU32 = AtomicU32::new(0);
	extern "C" fn worker(iters: u64) {
		// Yield between increments so the threads genuinely interleave rather
		// than each running to completion in one go.
		for _ in 0..iters {
			COUNTER.fetch_add(1, Ordering::SeqCst);
			sched::yield_now();
		}
		DONE.fetch_add(1, Ordering::SeqCst);
	}
	let threads = 4u32;
	let iters = 10u32;
	for _ in 0..threads {
		sched::spawn(worker, iters as u64);
	}
	sched::run_until_idle();
	assert_eq!(DONE.load(Ordering::SeqCst), threads);
	assert_eq!(COUNTER.load(Ordering::SeqCst), threads * iters);
}

#[test_case]
fn preemption_preempts_a_cpu_bound_thread() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static STOP: AtomicBool = AtomicBool::new(false);
	static MATE_RAN: AtomicBool = AtomicBool::new(false);
	// A CPU-bound thread that NEVER yields: it spins until another thread sets STOP.
	// Only timer-driven preemption can let that other thread run, so without
	// preemption this spins forever and hangs the test.
	extern "C" fn hog(_arg: u64) {
		while !STOP.load(Ordering::SeqCst) {
			core::hint::spin_loop();
		}
	}
	// The cohabiting thread: records that it ran, then releases the hog so the run
	// queue can drain.
	extern "C" fn mate(_arg: u64) {
		MATE_RAN.store(true, Ordering::SeqCst);
		STOP.store(true, Ordering::SeqCst);
	}
	STOP.store(false, Ordering::SeqCst);
	MATE_RAN.store(false, Ordering::SeqCst);
	// Both land on this core's run queue; the hog runs first and never yields.
	sched::spawn(hog, 0);
	sched::spawn(mate, 0);
	sched::run_until_idle();
	assert!(MATE_RAN.load(Ordering::SeqCst), "the cohabiting thread never ran: the never-yielding thread was not preempted");
}

#[test_case]
fn a_cpu_bound_ring3_thread_is_preempted() {
	use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
	use mem::frame::{self, PAGE_SIZE};
	// The spinner's shared data page sits at a fixed USER address clear of the test
	// code and stack pages: [0] = stop flag, [8] = liveness counter.
	const SPIN_FLAG_VA: u64 = 0x0000_0000_4000_2000;
	static SPIN_FLAG_PHYS: AtomicU64 = AtomicU64::new(0);
	static SPIN_DONE: AtomicBool = AtomicBool::new(false);
	// Host thread for the ring-3 spinner: maps code + stack + the shared data page,
	// publishes the data frame for the releaser, and drops to ring 3. The spinner
	// makes NO syscall until released, so without ring-3 preemption it would own
	// this core forever and the test would hang.
	extern "C" fn spinner_body(_arg: u64) {
		let code = frame::allocate().expect("user code frame");
		let stack = frame::allocate().expect("user stack frame");
		let data = frame::allocate().expect("user data frame");
		// Zero the data page so the stop flag starts clear (a recycled frame is not).
		unsafe { core::ptr::write_bytes((mem::hhdm_offset() + data) as *mut u8, 0, PAGE_SIZE as usize) };
		let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
		arch::paging::map_page(USER_CODE_VA, code, flags);
		arch::paging::map_page(USER_STACK_VA, stack, flags | arch::paging::NO_EXECUTE);
		arch::paging::map_page(SPIN_FLAG_VA, data, flags | arch::paging::NO_EXECUTE);
		let program = arch::usermode::program_spin_bytes();
		unsafe {
			arch::paging::copy_to_user_page(USER_CODE_VA, program);
		}
		SPIN_FLAG_PHYS.store(data, Ordering::SeqCst);
		unsafe {
			arch::usermode::enter(USER_CODE_VA, USER_STACK_VA + PAGE_SIZE, SPIN_FLAG_VA);
		}
		arch::paging::unmap_page(USER_CODE_VA);
		arch::paging::unmap_page(USER_STACK_VA);
		arch::paging::unmap_page(SPIN_FLAG_VA);
		frame::deallocate(code);
		frame::deallocate(stack);
		frame::deallocate(data);
		SPIN_DONE.store(true, Ordering::SeqCst);
	}
	// The releaser waits until the spinner's counter demonstrably grows - proof the
	// ring-3 loop is running AND being preempted (this kernel thread shares the same
	// core) - then raises the stop flag through the frame's kernel mapping.
	extern "C" fn releaser(_arg: u64) {
		let data = loop {
			let phys = SPIN_FLAG_PHYS.load(Ordering::SeqCst);
			if phys != 0 {
				break phys;
			}
			core::hint::spin_loop();
		};
		let flag = (mem::hhdm_offset() + data) as *mut u64;
		let counter = unsafe { flag.add(1) };
		let start = unsafe { counter.read_volatile() };
		while unsafe { counter.read_volatile() } < start.wrapping_add(1000) {
			core::hint::spin_loop();
		}
		unsafe { flag.write_volatile(1) };
	}
	SPIN_FLAG_PHYS.store(0, Ordering::SeqCst);
	SPIN_DONE.store(false, Ordering::SeqCst);
	// Both threads land on this core: the spinner never yields in ring 3, the
	// releaser never yields in ring 0 - only the timer can interleave them, and the
	// spinner's half of that needs ring-3 preemption.
	sched::spawn(spinner_body, 0);
	sched::spawn(releaser, 0);
	sched::run_until_idle();
	assert!(SPIN_DONE.load(Ordering::SeqCst), "the ring-3 spinner never finished: ring 3 was not preempted");
}

#[test_case]
fn a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick() {
	use core::sync::atomic::{AtomicU64, Ordering};
	static RAN_AT: AtomicU64 = AtomicU64::new(0);
	extern "C" fn stamp(_arg: u64) {
		RAN_AT.store(arch::tsc::now().max(1), Ordering::SeqCst);
	}
	if smp::cpu_count() < 2 {
		return;
	}
	// Without the wake IPI a halted AP only notices queued work on its next 100 Hz
	// timer tick, so each spawn-to-run trip averages ~5 ms and the odds of five
	// trips all finishing under the bound are below a percent. With the IPI the
	// trip is microseconds, leaving generous headroom for host jitter.
	const TRIP_BOUND_NS: u64 = 4_000_000;
	// A warmup trip whose latency is not asserted: the first cross-hart spawn pays
	// one-time costs (allocating the new Process's address space, the first traversal
	// of the wake-IPI path, the emulator JIT-compiling it) that on a slow emulator can
	// exceed the per-trip bound. The measured trips below then observe steady-state wake
	// latency rather than cold start.
	RAN_AT.store(0, Ordering::SeqCst);
	sched::spawn_on(1, stamp, 0);
	while RAN_AT.load(Ordering::SeqCst) == 0 {
		core::hint::spin_loop();
	}
	for _ in 0..5 {
		RAN_AT.store(0, Ordering::SeqCst);
		let start = arch::tsc::now();
		sched::spawn_on(1, stamp, 0);
		while RAN_AT.load(Ordering::SeqCst) == 0 {
			core::hint::spin_loop();
		}
		let elapsed = arch::tsc::cycles_to_ns(RAN_AT.load(Ordering::SeqCst).wrapping_sub(start));
		assert!(elapsed < TRIP_BOUND_NS, "a remote spawn waited out the tick: the wake IPI did not reach the halted core");
	}
}

#[test_case]
fn scheduler_runs_across_cores() {
	use core::sync::atomic::{AtomicU32, Ordering};
	static CROSS: AtomicU32 = AtomicU32::new(0);
	extern "C" fn ap_worker(_arg: u64) {
		CROSS.fetch_add(1, Ordering::SeqCst);
	}
	// Spawn one thread onto every application processor; each runs the worker in
	// its idle loop. With a single core this is a no-op and the test trivially
	// holds.
	let others = smp::cpu_count() - 1;
	for cpu in 1..smp::cpu_count() {
		sched::spawn_on(cpu, ap_worker, 0);
	}
	// Wait (bounded) for every AP to run its thread on its own core.
	let mut spins = 0u64;
	while (CROSS.load(Ordering::SeqCst) as usize) < others {
		core::hint::spin_loop();
		spins += 1;
		assert!(spins < 2_000_000_000, "AP threads did not run");
	}
	assert_eq!(CROSS.load(Ordering::SeqCst) as usize, others);
}

#[test_case]
fn process_isolation_and_per_process_tables() {
	use core::sync::atomic::{AtomicU64, Ordering};
	use mem::frame;
	use object::address_space::AddressSpace;
	use object::process::Process;
	use object::rights::Rights;

	// A single user virtual address that both processes map - to different frames.
	const VA: u64 = 0x0000_0000_3000_0000;
	// Each reader records the CR3 it ran on and the value it saw at VA, indexed by
	// the discriminator it is spawned with.
	static CR3: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
	static SEEN: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
	extern "C" fn reader(which: u64) {
		let cr3 = arch::context::read_cr3();
		// The page is USER-mapped, so this ring-0 read goes through the sanctioned
		// SMAP window (the test reads it to prove CR3 isolation, not to dodge SMAP).
		let value = arch::paging::user_access(|| unsafe { (VA as *const u64).read_volatile() });
		CR3[which as usize].store(cr3, Ordering::SeqCst);
		SEEN[which as usize].store(value, Ordering::SeqCst);
	}

	// Two processes, each with its own page tables, in the root Domain.
	let p1 = Process::new(AddressSpace::create().expect("address space 1"), sched::root_domain());
	let p2 = Process::new(AddressSpace::create().expect("address space 2"), sched::root_domain());

	// Back the same VA with a distinct physical frame in each process, and stamp a
	// distinct value into each frame through the HHDM before mapping it.
	let f1 = frame::allocate().expect("frame 1");
	let f2 = frame::allocate().expect("frame 2");
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
	unsafe {
		((f1 + mem::hhdm_offset()) as *mut u64).write_volatile(0x1111_1111);
		((f2 + mem::hhdm_offset()) as *mut u64).write_volatile(0x2222_2222);
	}
	p1.address_space().map(VA, f1, flags);
	p2.address_space().map(VA, f2, flags);

	// Run a reader in each process and let them both finish.
	sched::thread_create(p1.clone(), reader, 0);
	sched::thread_create(p2.clone(), reader, 1);
	sched::run_until_idle();

	// Same VA, different physical frames: each reader saw only its own process's
	// memory - the address spaces are isolated.
	assert_eq!(SEEN[0].load(Ordering::SeqCst), 0x1111_1111);
	assert_eq!(SEEN[1].load(Ordering::SeqCst), 0x2222_2222);

	// The readers ran on different page-table roots, each its own process's CR3 -
	// proof the context switch reloaded CR3.
	let cr3_1 = CR3[0].load(Ordering::SeqCst);
	let cr3_2 = CR3[1].load(Ordering::SeqCst);
	assert_ne!(cr3_1, cr3_2);
	assert_eq!(cr3_1, p1.address_space().cr3());
	assert_eq!(cr3_2, p2.address_space().cr3());

	// Handle tables are per-process: a capability installed in one process is
	// invisible to the other.
	let (endpoint, _peer) = object::channel::Channel::create();
	p1.install(endpoint, Rights::ALL, 0);
	assert_eq!(p1.handles().lock().len(), 1);
	assert_eq!(p2.handles().lock().len(), 0);

	// Reclaim the data frames. Dropping the address spaces frees their page
	// tables, but these leaf frames are ours to release.
	assert_eq!(p1.address_space().unmap(VA), Some(f1));
	assert_eq!(p2.address_space().unmap(VA), Some(f2));
	frame::deallocate(f1);
	frame::deallocate(f2);
}

#[test_case]
fn syscall_roundtrip_stateless() {
	// Stateless syscalls round-trip from the test (idle) context: there is no
	// current thread, but these calls do not need one.
	unsafe {
		// A call returns there and back, carrying a value across the boundary.
		assert_eq!(arch::syscall::invoke(syscall::SYS_DEBUG_NOOP, 0x1234, 0, 0, 0), 0x1234);
		// An unknown syscall number is rejected with the error sentinel.
		let bad = arch::syscall::invoke(9999, 0, 0, 0, 0);
		assert_eq!(bad as i64, syscall::ERR_BAD_SYSCALL);
		assert!(syscall::sys_is_err(bad));
		// The kernel clock is monotonic across two reads.
		let first = arch::syscall::invoke(syscall::SYS_CLOCK_GET, 0, 0, 0, 0);
		let second = arch::syscall::invoke(syscall::SYS_CLOCK_GET, 0, 0, 0, 0);
		assert!(second >= first);
		assert!(!syscall::sys_is_err(first));
	}
}

#[test_case]
fn abi_check_accepts_the_matching_revision_and_refuses_a_mismatch() {
	// SYS_ABI_CHECK is the runtime's first syscall: a starting binary reports the ABI
	// revision it was built against, and the kernel refuses a mismatch so it never runs
	// against a renumbered call or a grown struct. Stateless, so it round-trips from the
	// idle context.
	unsafe {
		let ok = arch::syscall::invoke(syscall::SYS_ABI_CHECK, syscall::ABI_VERSION as u64, 0, 0, 0);
		assert_eq!(ok, 0, "the kernel's own ABI revision is accepted");
		assert!(!syscall::sys_is_err(ok));
		let mismatch = arch::syscall::invoke(syscall::SYS_ABI_CHECK, syscall::ABI_VERSION as u64 + 1, 0, 0, 0);
		assert_eq!(mismatch as i64, syscall::ERR_ABI_MISMATCH, "a different ABI revision is refused");
		assert!(syscall::sys_is_err(mismatch));
	}
}

#[test_case]
fn syscall_object_and_handle_ops() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// The object/handle/mapping syscalls operate on the current thread's handle
	// table, so the sequence runs inside a spawned kernel thread. A failed
	// assertion here panics the thread, which fails the test run.
	extern "C" fn body(_arg: u64) {
		use object::rights::Rights;
		unsafe {
			// object create -> a handle into the caller's table
			let handle = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			assert!(!syscall::sys_is_err(handle));
			// address-space op: map it, then write and read back through the mapping
			let virt = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, handle, 0, 0, 0);
			assert!(!syscall::sys_is_err(virt));
			let ptr = virt as *mut u64;
			ptr.write_volatile(0xfeed_face);
			assert_eq!(ptr.read_volatile(), 0xfeed_face);
			// mapping the same object twice is rejected (one mapping in M6)
			let again = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, handle, 0, 0, 0);
			assert_eq!(again as i64, syscall::ERR_INVALID);
			// handle op: duplicate with attenuated rights (READ only)
			let dup = arch::syscall::invoke(syscall::SYS_HANDLE_DUPLICATE, handle, Rights::READ.bits() as u64, 0, 0);
			assert!(!syscall::sys_is_err(dup));
			// the READ-only duplicate lacks MAP, so mapping through it is denied
			let dup_map = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, dup, 0, 0, 0);
			assert!(syscall::sys_is_err(dup_map));
			// unmap and close both handles
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, handle, 0, 0, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, handle, 0, 0, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, dup, 0, 0, 0) as i64, 0);
			// a closed handle no longer resolves
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, handle, 0, 0, 0) as i64, syscall::ERR_BAD_HANDLE);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

#[test_case]
fn an_unmapped_va_range_is_reused_not_leaked() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// The mmap window reclaims released ranges: an unmap returns its range to the
	// window's pool and the next map of the same size gets it back (first-fit),
	// so a map/unmap loop no longer walks off the window. Freeing two adjacent
	// ranges coalesces them, so a larger mapping fits the merged hole - churn
	// cannot shatter the window into unusable slivers.
	extern "C" fn body(_arg: u64) {
		unsafe {
			let page: u64 = mem::frame::PAGE_SIZE;
			// reuse: map, unmap, map again - the same range comes back.
			let a = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, page, 0, 0, 0);
			assert!(!syscall::sys_is_err(a));
			let first = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, a, 0, 0, 0);
			assert!(!syscall::sys_is_err(first));
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, a, 0, 0, 0) as i64, 0);
			let second = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, a, 0, 0, 0);
			assert_eq!(second, first, "the released range should be handed out again");
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, a, 0, 0, 0) as i64, 0);
			// coalescing: two adjacent single-page ranges released in either order
			// merge, so a two-page mapping fits where they were.
			let b = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, page, 0, 0, 0);
			let c = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, page, 0, 0, 0);
			let base_b = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, b, 0, 0, 0);
			let base_c = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, c, 0, 0, 0);
			assert_eq!(base_b, first, "the first-fit hole is the one just released");
			assert_eq!(base_c, base_b + page, "adjacent allocations pack the window");
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, b, 0, 0, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, c, 0, 0, 0) as i64, 0);
			let d = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 2 * page, 0, 0, 0);
			let base_d = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, d, 0, 0, 0);
			assert_eq!(base_d, base_b, "the merged hole should fit the larger mapping");
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, d, 0, 0, 0) as i64, 0);
			for handle in [a, b, c, d] {
				assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, handle, 0, 0, 0) as i64, 0);
			}
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

#[test_case]
fn device_memory_maps_mmio_region() {
	use core::sync::atomic::{AtomicBool, Ordering};
	use object::device_memory::DeviceMemory;
	use object::rights::Rights;
	const MARK: u64 = 0xfeed_face_dead_beef;
	static DONE: AtomicBool = AtomicBool::new(false);
	// A driver maps a DeviceMemory capability (a physical MMIO region) into its
	// address space and reads/writes through the mapping. A freshly allocated RAM
	// frame is a controllable stand-in for device registers; only the uncacheable
	// mapping is exercised (no concurrent cached access to the same frame).
	extern "C" fn body(device_handle: u64) {
		unsafe {
			let va = arch::syscall::invoke(syscall::SYS_DEVICE_MEMORY_MAP, device_handle, 0, 0, 0);
			assert!(!syscall::sys_is_err(va), "device memory did not map");
			let ptr = va as *mut u64;
			ptr.write_volatile(MARK);
			assert_eq!(ptr.read_volatile(), MARK, "the mapped MMIO region is not read/write");
			// A second map of the same region is rejected (one mapping per object).
			let again = arch::syscall::invoke(syscall::SYS_DEVICE_MEMORY_MAP, device_handle, 0, 0, 0);
			assert_eq!(again as i64, syscall::ERR_INVALID);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	let phys = mem::frame::allocate().expect("a frame for the stand-in MMIO region");
	let device = DeviceMemory::new(phys, mem::frame::PAGE_SIZE as usize);
	// Hand the capability to the driver thread as its bootstrap handle.
	sched::spawn_with_object(body, device, Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "device-memory mapping thread did not finish");
	// The thread (and its handle table) is reaped by run_until_idle, dropping the
	// DeviceMemory and tearing its mapping down, so the frame is free to reclaim.
	mem::frame::deallocate(phys);
}

#[test_case]
fn random_get_fills_distinct_bytes() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let mut a = [0u8; 32];
			let mut b = [0u8; 32];
			let na = arch::syscall::invoke(syscall::SYS_RANDOM_GET, a.as_mut_ptr() as u64, a.len() as u64, 0, 0);
			let nb = arch::syscall::invoke(syscall::SYS_RANDOM_GET, b.as_mut_ptr() as u64, b.len() as u64, 0, 0);
			assert_eq!(na as usize, a.len(), "random_get did not fill the whole buffer");
			assert_eq!(nb as usize, b.len());
			// The buffer was actually written, and two draws differ (a false failure
			// is a 1-in-2^256 event).
			assert_ne!(a, [0u8; 32], "random_get left the buffer zeroed");
			assert_ne!(a, b, "two random draws were identical");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

#[cfg(target_arch = "x86_64")]
#[test_case]
fn interrupt_bind_delivers_to_driver() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// Vector 0x2c (IRQ 12) is a bindable device-IRQ vector (not the timer at 0x20).
	const VECTOR: u64 = 0x2c;
	extern "C" fn body(_arg: u64) {
		unsafe {
			let h = arch::syscall::invoke(syscall::SYS_INTERRUPT_BIND, VECTOR, 0, 0, 0);
			assert!(!syscall::sys_is_err(h), "interrupt_bind failed");
			// Simulate the device IRQ firing with a software interrupt; the dispatch
			// path marks the bound Interrupt pending and wakes any waiter.
			core::arch::asm!("int 0x2c");
			// The interrupt is now pending, so a wait observes it and returns.
			let r = arch::syscall::invoke(syscall::SYS_WAIT, h, 0, 0, 0);
			assert_eq!(r as i64, 0, "wait did not observe the delivered interrupt");
			// Binding the same vector again while ours lives is refused.
			let again = arch::syscall::invoke(syscall::SYS_INTERRUPT_BIND, VECTOR, 0, 0, 0);
			assert_eq!(again as i64, syscall::ERR_RESOURCE_EXHAUSTED);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

// The aarch64 counterpart of the x86 interrupt tests above: x86 delivers device IRQs
// through the IDT + legacy INTx vectors (bindable, `int 0x2c`), which aarch64 has no
// equivalent for - every aarch64 device interrupt is MSI-X delivered through the
// GICv2m frame (a device writes its SPI to MSI_SETSPI_NS, the GIC pends that SPI, and
// gic::handle_irq dispatches it). These test that GICv2m MSI path directly.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn gicv2m_msi_binds_and_dispatch_signals_the_driver() {
	use mem::frame;
	use object::interrupt::Interrupt;
	// A frame stands in for a device's MSI-X table: acquire_msi programs entry 0 into it
	// (message address = the GICv2m frame's MSI_SETSPI_NS, message data = the SPI).
	let table = frame::allocate().expect("a frame for the fake MSI-X table");
	// Acquire a per-device MSI vector (a GICv2m SPI). `dest` (the x86 LAPIC target) is
	// unused on aarch64; `owner` is a fake discovered-device index.
	let vector = arch::interrupts::acquire_msi(table, 0, 3).expect("acquire_msi hands out a free SPI");
	// Bind a driver Interrupt to the vector; a second live bind is refused.
	let intr = Interrupt::new(vector);
	assert!(arch::interrupts::bind_msi(vector, &intr), "the first bind succeeds");
	assert!(arch::interrupts::is_bound(vector), "the vector reads as bound");
	let intr2 = Interrupt::new(vector);
	assert!(!arch::interrupts::bind_msi(vector, &intr2), "a second live bind is refused");
	// Dispatching the SPI INTID - what gic::handle_irq does when the SPI fires - marks
	// the bound Interrupt pending (its wait readiness).
	assert!(!intr.is_pending(), "not pending before the SPI fires");
	assert!(arch::interrupts::dispatch_msi(vector as u32), "dispatch_msi claims its own SPI");
	assert!(intr.is_pending(), "dispatch signalled the bound Interrupt");
	// An INTID below the frame's SPI range (the SGI / PPI space) is not an MSI vector.
	assert!(!arch::interrupts::dispatch_msi(0), "INTID 0 is not one of the frame's MSI SPIs");
	// Unbinding frees the slot for re-use.
	arch::interrupts::unbind(vector);
	assert!(!arch::interrupts::is_bound(vector), "unbind drops the binding");
	frame::deallocate(table);
}

#[cfg(target_arch = "aarch64")]
#[test_case]
fn gicv2m_msi_inventory_reports_the_timer_and_msi_vectors() {
	use mem::frame;
	// Index 0 of the aarch64 IRQ inventory (what `lsirq` reads) is the kernel's own EL1
	// physical-timer PPI (INTID 30), always in use and reported as a fixed vector - the
	// aarch64 analogue of x86's fixed timer entry.
	let timer = arch::interrupts::irq_info(0).expect("the inventory has a timer entry");
	assert_eq!(timer.kind, abi::IRQ_KIND_FIXED, "index 0 is the fixed timer PPI");
	assert_eq!(timer.vector, 30, "the aarch64 timer is the EL1 physical-timer PPI (INTID 30)");
	assert_eq!(timer.bound, 1, "the timer is always the kernel's own");
	// After the timer, each entry is a GICv2m MSI SPI. Acquiring one for a fake device
	// makes it appear in the inventory as an MSI vector owned by that device index.
	let table = frame::allocate().expect("a frame for the fake MSI-X table");
	let vector = arch::interrupts::acquire_msi(table, 0, 9).expect("acquire an MSI SPI");
	let mut seen = false;
	for i in 1..arch::interrupts::irq_info_len() {
		if let Some(info) = arch::interrupts::irq_info(i)
			&& info.vector == vector as u32
		{
			assert_eq!(info.kind, abi::IRQ_KIND_MSI, "an acquired vector reports as MSI");
			assert_eq!(info.device, 9, "the inventory records the owning device index");
			seen = true;
		}
	}
	assert!(seen, "the acquired MSI vector appears in the inventory");
	arch::interrupts::unbind(vector);
	frame::deallocate(table);
}

// The riscv64 counterpart of the x86 INTx and aarch64 GICv2m interrupt tests: on riscv
// (QEMU `virt,aia=aplic-imsic`) every device interrupt is an MSI-X-delivered EID pended in
// a hart's IMSIC S-file (imsic.rs) - there is no bindable wired vector. These exercise the
// AIA/IMSIC MSI path directly: acquire an EID, bind a driver Interrupt, dispatch the EID.
#[cfg(target_arch = "riscv64")]
#[test_case]
fn imsic_msi_binds_and_dispatch_signals_the_driver() {
	use mem::frame;
	use object::interrupt::Interrupt;
	// A frame stands in for a device's MSI-X table: acquire_msi programs entry 0 into it
	// (message address = the acquiring hart's IMSIC S-file, message data = the EID).
	let table = frame::allocate().expect("a frame for the fake MSI-X table");
	// Acquire a per-device MSI vector (an IMSIC EID). `dest` (the x86 LAPIC target) is
	// unused on riscv; `owner` is a fake discovered-device index.
	let vector = arch::interrupts::acquire_msi(table, 0, 3).expect("acquire_msi hands out a free EID");
	// Bind a driver Interrupt to the vector; a second live bind is refused.
	let intr = Interrupt::new(vector);
	assert!(arch::interrupts::bind_msi(vector, &intr), "the first bind succeeds");
	assert!(arch::interrupts::is_bound(vector), "the vector reads as bound");
	let intr2 = Interrupt::new(vector);
	assert!(!arch::interrupts::bind_msi(vector, &intr2), "a second live bind is refused");
	// Dispatching the EID - what imsic::handle_external does when the EID fires - marks the
	// bound Interrupt pending (its wait readiness).
	assert!(!intr.is_pending(), "not pending before the EID fires");
	assert!(arch::interrupts::dispatch_msi(vector as u32), "dispatch_msi claims its own EID");
	assert!(intr.is_pending(), "dispatch signalled the bound Interrupt");
	// EID 0 is "no interrupt" - outside the MSI window - so it dispatches to no one.
	assert!(!arch::interrupts::dispatch_msi(0), "EID 0 is not one of the device MSI EIDs");
	// Unbinding frees the slot for re-use.
	arch::interrupts::unbind(vector);
	assert!(!arch::interrupts::is_bound(vector), "unbind drops the binding");
	frame::deallocate(table);
}

#[cfg(target_arch = "riscv64")]
#[test_case]
fn imsic_msi_inventory_reports_the_timer_and_msi_vectors() {
	use mem::frame;
	// Index 0 of the riscv IRQ inventory (what `lsirq` reads) is the kernel's own S-mode
	// timer interrupt (SCAUSE code 5), always in use and reported as a fixed vector - the
	// riscv analogue of x86's fixed LAPIC-timer entry and aarch64's timer PPI.
	let timer = arch::interrupts::irq_info(0).expect("the inventory has a timer entry");
	assert_eq!(timer.kind, abi::IRQ_KIND_FIXED, "index 0 is the fixed S-mode timer");
	assert_eq!(timer.vector, 5, "the riscv timer is the S-mode timer interrupt (scause code 5)");
	assert_eq!(timer.bound, 1, "the timer is always the kernel's own");
	// After the timer, each entry is an IMSIC MSI EID. Acquiring one for a fake device
	// makes it appear in the inventory as an MSI vector owned by that device index.
	let table = frame::allocate().expect("a frame for the fake MSI-X table");
	let vector = arch::interrupts::acquire_msi(table, 0, 9).expect("acquire an MSI EID");
	let mut seen = false;
	for i in 1..arch::interrupts::irq_info_len() {
		if let Some(info) = arch::interrupts::irq_info(i)
			&& info.vector == vector as u32
		{
			assert_eq!(info.kind, abi::IRQ_KIND_MSI, "an acquired vector reports as MSI");
			assert_eq!(info.device, 9, "the inventory records the owning device index");
			seen = true;
		}
	}
	assert!(seen, "the acquired MSI vector appears in the inventory");
	arch::interrupts::unbind(vector);
	frame::deallocate(table);
}

#[test_case]
fn object_property_set_names_an_object() {
	use core::sync::atomic::{AtomicBool, Ordering};
	use object::KernelObject;
	use object::event::Event;
	use object::rights::Rights;
	static DONE: AtomicBool = AtomicBool::new(false);
	const NAME: &[u8] = b"irq-driver";
	extern "C" fn body(handle: u64) {
		unsafe {
			let r = arch::syscall::invoke(syscall::SYS_OBJECT_PROPERTY_SET, handle, syscall::PROP_NAME, NAME.as_ptr() as u64, NAME.len() as u64);
			assert_eq!(r as i64, 0, "set name failed");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	let event = Event::create();
	// The driver thread holds a handle to this same Event; the test keeps an Arc to
	// read the label back after the thread names it.
	sched::spawn_with_object(body, event.clone(), Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	assert_eq!(event.header().name().as_deref(), Some("irq-driver"));
}

#[test_case]
fn object_property_set_bounds_a_domain() {
	use core::sync::atomic::{AtomicBool, Ordering};
	use object::domain::{Domain, UNLIMITED};
	use object::rights::Rights;
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(handle: u64) {
		unsafe {
			// Set the Domain's memory limit to 8192 bytes via the property syscall.
			let r = arch::syscall::invoke(syscall::SYS_OBJECT_PROPERTY_SET, handle, syscall::PROP_MEMORY_LIMIT, 8192, 0);
			assert_eq!(r as i64, 0, "set memory limit failed");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	let domain = Domain::new(UNLIMITED, UNLIMITED, UNLIMITED);
	sched::spawn_with_object(body, domain.clone(), Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	assert_eq!(domain.account().memory().limit(), 8192);
}

#[test_case]
fn channel_message_and_capability_transfer() {
	use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
	static OK: AtomicBool = AtomicBool::new(false);
	static MARKER: AtomicU64 = AtomicU64::new(0);
	// One thread holds each end of a channel. The sender writes a marker into a
	// memory object and transfers it alongside a byte payload; the receiver reads
	// the bytes and the marker back through the handle it is granted. A failed
	// assertion inside a thread panics it, which fails the test run.
	extern "C" fn sender(ch: u64) {
		unsafe {
			let mo = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			let virt = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, mo, 0, 0, 0);
			(virt as *mut u64).write_volatile(0x5151_5151);
			arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, mo, 0, 0, 0);
			let payload = *b"hi";
			let sent = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, ch, payload.as_ptr() as u64, payload.len() as u64, mo);
			assert!(!syscall::sys_is_err(sent));
			// the transferred handle was consumed on success
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, mo, 0, 0, 0) as i64, syscall::ERR_BAD_HANDLE);
		}
	}
	extern "C" fn receiver(ch: u64) {
		unsafe {
			let mut buf = [0u8; 8];
			let mut xfer: u64 = 0;
			let mut n;
			loop {
				n = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, ch, buf.as_mut_ptr() as u64, buf.len() as u64, &mut xfer as *mut u64 as u64);
				if !syscall::sys_is_err(n) {
					break;
				}
				assert_eq!(n as i64, syscall::ERR_WOULD_BLOCK);
				sched::yield_now();
			}
			assert_eq!(&buf[..n as usize], b"hi");
			assert_ne!(xfer, 0);
			let virt = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, xfer, 0, 0, 0);
			MARKER.store((virt as *const u64).read_volatile(), Ordering::SeqCst);
			arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, xfer, 0, 0, 0);
			arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, xfer, 0, 0, 0);
			OK.store(true, Ordering::SeqCst);
		}
	}
	let (ep0, ep1) = object::channel::Channel::create();
	sched::spawn_with_object(sender, ep0, object::rights::Rights::ALL, 0);
	sched::spawn_with_object(receiver, ep1, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(OK.load(Ordering::SeqCst));
	assert_eq!(MARKER.load(Ordering::SeqCst), 0x5151_5151);
}

#[test_case]
fn blocking_wait_wakes_on_message() {
	use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};
	static OK: AtomicBool = AtomicBool::new(false);
	static WAIT_RET: AtomicI64 = AtomicI64::new(-999);
	// The server blocks in SYS_WAIT on its (empty) channel - descheduled, not
	// spinning. The client then sends, which wakes the server; it returns from
	// wait with success and recv's the message. Exercises block_on + wake_object +
	// the reschedule Block path end to end.
	extern "C" fn server(ch: u64) {
		unsafe {
			let ret = arch::syscall::invoke(syscall::SYS_WAIT, ch, 0, 0, 0);
			WAIT_RET.store(ret as i64, Ordering::SeqCst);
			let mut buf = [0u8; 8];
			let mut xfer: u64 = 0;
			let n = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, ch, buf.as_mut_ptr() as u64, buf.len() as u64, &mut xfer as *mut u64 as u64);
			assert!(!syscall::sys_is_err(n));
			assert_eq!(&buf[..n as usize], b"ping");
			OK.store(true, Ordering::SeqCst);
		}
	}
	extern "C" fn client(ch: u64) {
		unsafe {
			let payload = *b"ping";
			let sent = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, ch, payload.as_ptr() as u64, payload.len() as u64, 0);
			assert!(!syscall::sys_is_err(sent));
		}
	}
	let (ep0, ep1) = object::channel::Channel::create();
	// Spawn the server first so it runs and blocks before the client sends.
	sched::spawn_with_object(server, ep0, object::rights::Rights::ALL, 0);
	sched::spawn_with_object(client, ep1, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(OK.load(Ordering::SeqCst));
	assert_eq!(WAIT_RET.load(Ordering::SeqCst), 0);
}

#[test_case]
fn a_sender_on_a_full_channel_blocks_and_wakes_on_drain() {
	use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
	static SENDER_REFUSED: AtomicBool = AtomicBool::new(false);
	static SENDER_DONE: AtomicBool = AtomicBool::new(false);
	static RECEIVED: AtomicU64 = AtomicU64::new(0);
	// Backpressure without spinning: a channel created with a queue depth of 2
	// refuses the third send with WOULD_BLOCK (the depth is a creation parameter,
	// not a hardwired constant), the sender then BLOCKS in SYS_WAIT with
	// WAIT_WRITABLE, and the receiver's first recv - the queue leaving its full
	// state - wakes it to deliver the rest. If the drain never woke the sender, it
	// would stay blocked forever and SENDER_DONE would read false.
	extern "C" fn sender(ch: u64) {
		unsafe {
			for msg in [b"m1", b"m2", b"m3"] {
				loop {
					let sent = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, ch, msg.as_ptr() as u64, msg.len() as u64, 0);
					if sent as i64 == syscall::ERR_WOULD_BLOCK {
						SENDER_REFUSED.store(true, Ordering::SeqCst);
						let ret = arch::syscall::invoke(syscall::SYS_WAIT, ch, 0, abi::WAIT_WRITABLE, 0);
						assert_eq!(ret as i64, 0, "the writable wait returns ready");
						continue;
					}
					assert!(!syscall::sys_is_err(sent));
					break;
				}
			}
			SENDER_DONE.store(true, Ordering::SeqCst);
		}
	}
	extern "C" fn receiver(ch: u64) {
		unsafe {
			let mut buf = [0u8; 8];
			let mut xfer: u64 = 0;
			while RECEIVED.load(Ordering::SeqCst) < 3 {
				let n = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, ch, buf.as_mut_ptr() as u64, buf.len() as u64, &mut xfer as *mut u64 as u64);
				if n as i64 == syscall::ERR_WOULD_BLOCK {
					arch::syscall::invoke(syscall::SYS_WAIT, ch, 0, 0, 0);
					continue;
				}
				assert!(!syscall::sys_is_err(n), "recv failed");
				RECEIVED.fetch_add(1, Ordering::SeqCst);
			}
		}
	}
	let (ep0, ep1) = object::channel::Channel::create_with_depth(2);
	// Run the sender ALONE first so it deterministically fills the depth-2 queue, is
	// refused on the third send and blocks - without the receiver interleaving to drain a
	// slot before that third send (a scheduling race that made this flaky on riscv-TCG,
	// where a timer tick could switch to the receiver mid-fill).
	sched::spawn_with_object(sender, ep0, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(SENDER_REFUSED.load(Ordering::SeqCst), "the depth-2 queue refused the third send");
	// Now the receiver drains, which must wake the blocked sender to deliver the rest.
	sched::spawn_with_object(receiver, ep1, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(SENDER_DONE.load(Ordering::SeqCst), "the drain woke the blocked sender");
	assert_eq!(RECEIVED.load(Ordering::SeqCst), 3, "every message was delivered");
}

#[test_case]
fn blocking_wait_times_out_on_deadline() {
	use core::sync::atomic::{AtomicI64, Ordering};
	static WAIT_RET: AtomicI64 = AtomicI64::new(-999);
	// A thread waits on an event that is never signaled, with a short absolute
	// deadline. The wait must wake itself when the deadline passes and report
	// ERR_TIMED_OUT - the timed-wait path (the scheduler's deadline check).
	extern "C" fn waiter(_arg: u64) {
		unsafe {
			let ev = arch::syscall::invoke(syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
			let now = arch::syscall::invoke(syscall::SYS_CLOCK_GET, 0, 0, 0, 0);
			let deadline = now + 3; // ~30 ms at the 100 Hz tick
			let ret = arch::syscall::invoke(syscall::SYS_WAIT, ev, deadline, 0, 0);
			WAIT_RET.store(ret as i64, Ordering::SeqCst);
		}
	}
	sched::spawn(waiter, 0);
	sched::run_until_idle();
	assert_eq!(WAIT_RET.load(Ordering::SeqCst), syscall::ERR_TIMED_OUT);
}

#[test_case]
fn a_periodic_wait_ticks_but_never_holds_the_scheduler() {
	use core::sync::atomic::{AtomicU64, Ordering};
	static TICKS: AtomicU64 = AtomicU64::new(0);
	// A service thread waits with WAIT_PERIODIC on an event nothing signals,
	// re-arming a short deadline forever - the virtio-gpu poll pattern. Without the
	// flag this loop would keep run_until_idle from ever returning (the M35c blink
	// hang); with it, the scheduler settles while the wait is parked, and each later
	// run_until_idle entry wakes the tick that came due - the wait still TICKS.
	extern "C" fn service(_arg: u64) {
		unsafe {
			let ev = arch::syscall::invoke(syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
			loop {
				let now = arch::syscall::invoke(syscall::SYS_CLOCK_GET, 0, 0, 0, 0);
				let ret = arch::syscall::invoke(syscall::SYS_WAIT, ev, now + 2, abi::WAIT_PERIODIC, 0);
				assert_eq!(ret as i64, syscall::ERR_TIMED_OUT, "the periodic wake fires as a timeout");
				TICKS.fetch_add(1, Ordering::SeqCst);
			}
		}
	}
	sched::spawn(service, 0);
	// The first run must RETURN despite the perpetually re-armed deadline - this is
	// the settling property the flag exists for (an ordinary wait here would hang).
	sched::run_until_idle();
	let settled = TICKS.load(Ordering::SeqCst);
	// Later entries (the standing loop's re-entry, here explicit) wake the due tick.
	let target = settled + 2;
	let give_up = arch::apic::ticks() + 100;
	while TICKS.load(Ordering::SeqCst) < target && arch::apic::ticks() < give_up {
		sched::run_until_idle();
		arch::idle_halt();
	}
	assert!(TICKS.load(Ordering::SeqCst) >= target, "the periodic wait keeps ticking across settles");
}

#[test_case]
fn wait_any_wakes_on_the_ready_handle() {
	use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
	static HB: AtomicU64 = AtomicU64::new(0);
	static WAIT_RET: AtomicI64 = AtomicI64::new(-999);
	static OK: AtomicBool = AtomicBool::new(false);
	// The server blocks in SYS_WAIT_ANY on TWO channels at once. Only the second
	// (hb) ever receives a message, so wait_any must wake, return index 1, and the
	// server then recv's that message - exercising block_on_any and the multi-koid
	// waiter cleanup (the thread is parked under both koids, woken via one, and must
	// leave no stale entry under the other).
	extern "C" fn server(ha: u64) {
		unsafe {
			let hb = HB.load(Ordering::SeqCst);
			let handles = [ha, hb];
			let ret = arch::syscall::invoke(syscall::SYS_WAIT_ANY, handles.as_ptr() as u64, handles.len() as u64, 0, 0);
			WAIT_RET.store(ret as i64, Ordering::SeqCst);
			let mut buf = [0u8; 8];
			let mut xfer: u64 = 0;
			let n = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, hb, buf.as_mut_ptr() as u64, buf.len() as u64, &mut xfer as *mut u64 as u64);
			OK.store(!syscall::sys_is_err(n) && &buf[..n as usize] == b"pong", Ordering::SeqCst);
		}
	}
	extern "C" fn client(ch: u64) {
		unsafe {
			let payload = *b"pong";
			let _ = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, ch, payload.as_ptr() as u64, payload.len() as u64, 0);
		}
	}
	let (a0, a1) = object::channel::Channel::create();
	let (b0, b1) = object::channel::Channel::create();
	// Spawn the server with the first channel (its arg is that handle), then install
	// the second channel as a second handle and record its value for the server.
	let server = sched::spawn_with_object(server, a0, object::rights::Rights::ALL, 0);
	let hb = server.handles().lock().insert(object::handle::Capability::new(b0, object::rights::Rights::ALL, 0)).raw();
	HB.store(hb, Ordering::SeqCst);
	sched::spawn_with_object(client, b1, object::rights::Rights::ALL, 0);
	// Hold the first channel's peer open so that handle stays silent (not closed),
	// otherwise its peer-close would make it ready and wait_any could return 0.
	let _keep_a1 = a1;
	sched::run_until_idle();
	assert_eq!(WAIT_RET.load(Ordering::SeqCst), 1);
	assert!(OK.load(Ordering::SeqCst));
}

#[test_case]
fn waiting_on_a_process_handle_wakes_when_it_exits() {
	use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};
	static WAIT_RET: AtomicI64 = AtomicI64::new(-999);
	static DONE: AtomicBool = AtomicBool::new(false);
	// A subject process blocks until released, then returns - its last thread exiting
	// terminates the process. A waiter blocks in SYS_WAIT on a handle to that process.
	// The Process handle must stay unready while the subject runs, then become ready -
	// waking the waiter, which returns 0 - once the subject exits. This is the
	// process-terminated signal that lets a parent wait for a child to finish instead
	// of polling, the primitive shell job control reaps background jobs on.
	extern "C" fn subject(release: u64) {
		unsafe {
			// Block until the test sends on the release channel's peer, then fall off
			// the end -> thread_bootstrap -> sched::exit(), terminating the process.
			arch::syscall::invoke(syscall::SYS_WAIT, release, 0, 0, 0);
		}
	}
	extern "C" fn waiter(proc_handle: u64) {
		unsafe {
			let ret = arch::syscall::invoke(syscall::SYS_WAIT, proc_handle, 0, 0, 0);
			WAIT_RET.store(ret as i64, Ordering::SeqCst);
			DONE.store(true, Ordering::SeqCst);
		}
	}
	let (rel0, rel1) = object::channel::Channel::create();
	let subject_thread = sched::spawn_with_object(subject, rel0, object::rights::Rights::ALL, 0);
	let subject_process = subject_thread.process().clone();
	// The waiter gets a handle to the subject's process as its argument (installed by
	// spawn_with_object), carrying the WAIT right.
	let _waiter = sched::spawn_with_object(waiter, subject_process.clone(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	// Both are blocked now: the subject on the release channel, the waiter on the
	// not-yet-terminated process handle.
	assert!(!DONE.load(Ordering::SeqCst), "the waiter blocks while the subject still runs");
	// Release the subject so it returns and exits.
	rel1.send(object::channel::Message::new(alloc::vec![1], alloc::vec::Vec::new(), 0)).unwrap();
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the waiter wakes once the subject exits");
	assert_eq!(WAIT_RET.load(Ordering::SeqCst), 0, "the process handle became ready on exit");
}

#[test_case]
fn signal_terminate_wakes_a_blocked_thread() {
	use core::sync::atomic::{AtomicBool, Ordering};
	use object::thread::ThreadState;
	static RAN: AtomicBool = AtomicBool::new(false);
	static PAST_WAIT: AtomicBool = AtomicBool::new(false);
	// The victim blocks forever in SYS_WAIT on a channel whose peer is held open, so
	// nothing wakes it on its own. Delivering the terminate disposition (mark the
	// process killed + wake its threads, exactly as sys_process_signal(SIG_INT)) must
	// wake the blocked thread, have it observe the kill at the wait's checkpoint, and
	// retire it - never running the code past the wait. This proves a signal reaches a
	// thread blocked on something that would otherwise never become ready.
	extern "C" fn victim(handle: u64) {
		unsafe {
			RAN.store(true, Ordering::SeqCst);
			arch::syscall::invoke(syscall::SYS_WAIT, handle, 0, 0, 0);
			PAST_WAIT.store(true, Ordering::SeqCst);
		}
	}
	let (a, b) = object::channel::Channel::create();
	let _keep = b; // hold the peer so the channel never becomes ready by itself
	let victim_thread = sched::spawn_with_object(victim, a, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(RAN.load(Ordering::SeqCst), "the victim ran and blocked");
	assert!(!PAST_WAIT.load(Ordering::SeqCst), "the victim is blocked in the wait");
	// The terminate disposition, exactly as sys_process_signal(SIG_INT) applies it.
	let process = victim_thread.process().clone();
	process.terminate();
	for thread in process.live_threads() {
		sched::wake_thread(&thread);
	}
	sched::run_until_idle();
	assert!(!PAST_WAIT.load(Ordering::SeqCst), "the killed thread must retire at the wait, not resume past it");
	assert_eq!(victim_thread.state(), ThreadState::Exited, "the victim thread has exited");
}

#[test_case]
fn channel_endpoint_semantics() {
	use object::channel::{Channel, ChannelError, Message};
	let (a, b) = Channel::create();
	// an empty inbox reports would-block while the peer is open
	assert!(matches!(b.recv(), Err(ChannelError::Empty)));
	// a message carries its byte payload and the sender badge to the peer
	a.send(Message::new(alloc::vec![1, 2, 3], alloc::vec::Vec::new(), 0x99)).unwrap();
	let message = b.recv().unwrap();
	assert_eq!(message.bytes, alloc::vec![1, 2, 3]);
	assert_eq!(message.badge, 0x99);
	// once the peer is dropped and the inbox drained, recv reports peer-closed
	drop(a);
	assert!(b.is_peer_closed());
	assert!(matches!(b.recv(), Err(ChannelError::PeerClosed)));
}

#[test_case]
fn channel_peek_reports_the_pending_length() {
	use object::channel::{Channel, ChannelError, Message};
	// The peek that retires the wire ceiling: a receiver learns the next pending
	// message's exact byte length without dequeuing it, sizes its buffer, and the
	// recv that follows loses nothing - demonstrated well past the old 4096 B
	// reply convention.
	let (a, b) = Channel::create();
	assert!(matches!(b.peek_len(), Err(ChannelError::Empty)));
	let big: alloc::vec::Vec<u8> = (0..20_000u32).map(|i| i as u8).collect();
	a.send(Message::new(big.clone(), alloc::vec::Vec::new(), 0)).unwrap();
	a.send(Message::new(alloc::vec![7u8; 3], alloc::vec::Vec::new(), 0)).unwrap();
	// the peek names the FRONT message and does not consume it
	assert_eq!(b.peek_len().unwrap(), 20_000);
	assert_eq!(b.peek_len().unwrap(), 20_000, "peek does not dequeue");
	let first = b.recv().unwrap();
	assert_eq!(first.bytes, big, "the exactly-sized recv loses nothing");
	assert_eq!(b.peek_len().unwrap(), 3, "the next message's length follows");
	let _ = b.recv().unwrap();
	// empty again while the peer is open, peer-closed once it is gone
	assert!(matches!(b.peek_len(), Err(ChannelError::Empty)));
	drop(a);
	assert!(matches!(b.peek_len(), Err(ChannelError::PeerClosed)));
}

#[test_case]
fn pci_scan_finds_virtio_devices() {
	// QEMU is launched (see qemu-run.sh) with virtio-blk, virtio-net, and a virtio
	// serial device on the PCI bus. The kernel's PCI scan must find them: at least
	// one device carrying virtio's PCI vendor id, and each such modern virtio device
	// must report a recognizable device type.
	let devices = arch::pci::scan();
	let virtio: alloc::vec::Vec<_> = devices.iter().filter(|d| d.is_virtio()).collect();
	assert!(!virtio.is_empty(), "the PCI scan should find the QEMU virtio devices");
	for d in &virtio {
		assert!(d.virtio_type().is_some(), "a modern virtio device should report a device type (id {:#06x})", d.device_id);
	}
}

#[test_case]
fn device_table_exposes_virtio_mmio() {
	use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};
	// device::init() populated the table at boot from the PCI scan. A driver-like
	// thread queries it the way DeviceManager will: count the devices, read the
	// first one's DeviceInfo, acquire its DeviceMemory capability, and map the MMIO.
	static COUNT: AtomicI64 = AtomicI64::new(-1);
	static VTYPE: AtomicU64 = AtomicU64::new(0);
	static BAR_LEN: AtomicU64 = AtomicU64::new(0);
	static MAPPED: AtomicU64 = AtomicU64::new(0);
	extern "C" fn body(_arg: u64) {
		let mut info = abi::DeviceInfo::default();
		let size = core::mem::size_of::<abi::DeviceInfo>() as u64;
		unsafe {
			COUNT.store(arch::syscall::invoke(syscall::SYS_DEVICE_COUNT, 0, 0, 0, 0) as i64, Ordering::SeqCst);
			if arch::syscall::invoke(syscall::SYS_DEVICE_INFO, 0, &mut info as *mut _ as u64, size, 0) as i64 == 0 {
				VTYPE.store(info.device_type as u64, Ordering::SeqCst);
				BAR_LEN.store(info.bar_len, Ordering::SeqCst);
			}
			let handle = arch::syscall::invoke(syscall::SYS_DEVICE_ACQUIRE, 0, 0, 0, 0);
			if !syscall::sys_is_err(handle) {
				MAPPED.store(arch::syscall::invoke(syscall::SYS_DEVICE_MEMORY_MAP, handle, 0, 0, 0), Ordering::SeqCst);
			}
		}
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(COUNT.load(Ordering::SeqCst) >= 3, "expected at least the 3 QEMU virtio devices");
	assert!((1..=4).contains(&VTYPE.load(Ordering::SeqCst)), "device 0 should report a virtio type");
	assert!(BAR_LEN.load(Ordering::SeqCst) > 0, "the MMIO BAR should have a non-zero length");
	let mapped = MAPPED.load(Ordering::SeqCst);
	assert!(mapped != 0 && !syscall::sys_is_err(mapped), "the device MMIO should map to a valid address");
}

#[test_case]
fn pci_scan_finds_the_xhci_controller() {
	// QEMU is launched (see qemu-run.sh) with a qemu-xhci USB host controller. The
	// kernel's PCI scan must find it by its class triple (0x0C/0x03/0x30) and resolve
	// its MMIO window: a non-zero BAR 0 base and a probed BAR size (the sizing write-
	// all-ones round-trip), plus an MSI-X capability for its interrupt vector.
	let controllers = arch::pci::scan_xhci();
	assert!(!controllers.is_empty(), "the PCI scan should find the QEMU xHCI controller");
	for x in &controllers {
		assert!(x.bar_phys != 0, "the xHCI BAR 0 should have a physical base");
		assert!(x.bar_len >= 0x1000, "the xHCI BAR 0 should be at least a page (probed {:#x})", x.bar_len);
		assert!(x.msix_cap != 0, "the xHCI controller should expose MSI-X");
	}
}

#[test_case]
fn device_table_exposes_the_xhci_controller() {
	use core::sync::atomic::{AtomicU64, Ordering};
	// The xHCI controller joins the same device table the virtio devices live in. A
	// driver-like thread walks the table over the device syscalls the way DeviceManager
	// will: find the entry reporting DEVICE_TYPE_XHCI, acquire its DeviceMemory
	// capability, and map the controller's register file.
	static BAR_LEN: AtomicU64 = AtomicU64::new(0);
	static MAPPED: AtomicU64 = AtomicU64::new(0);
	extern "C" fn body(_arg: u64) {
		let mut info = abi::DeviceInfo::default();
		let size = core::mem::size_of::<abi::DeviceInfo>() as u64;
		unsafe {
			let count = arch::syscall::invoke(syscall::SYS_DEVICE_COUNT, 0, 0, 0, 0);
			for i in 0..count {
				if arch::syscall::invoke(syscall::SYS_DEVICE_INFO, i, &mut info as *mut _ as u64, size, 0) as i64 != 0 {
					continue;
				}
				if info.device_type != abi::DEVICE_TYPE_XHCI {
					continue;
				}
				BAR_LEN.store(info.bar_len, Ordering::SeqCst);
				let handle = arch::syscall::invoke(syscall::SYS_DEVICE_ACQUIRE, i, 0, 0, 0);
				if !syscall::sys_is_err(handle) {
					MAPPED.store(arch::syscall::invoke(syscall::SYS_DEVICE_MEMORY_MAP, handle, 0, 0, 0), Ordering::SeqCst);
				}
				break;
			}
		}
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(BAR_LEN.load(Ordering::SeqCst) > 0, "the device table should hold the xHCI controller");
	let mapped = MAPPED.load(Ordering::SeqCst);
	assert!(mapped != 0 && !syscall::sys_is_err(mapped), "the xHCI register file should map to a valid address");
}

#[test_case]
fn xhci_driver_enumerates_the_usb_bus() {
	use object::channel::{Channel, Message};
	use object::device_memory::DeviceMemory;
	use object::rights::Rights;

	// The userspace xhci driver, driven the way DeviceManager drives it: spawn its
	// staged ELF (it lives on the system volume under drivers/, not in the init
	// package) with a bootstrap channel, hand it "DEVICE" + the controller's
	// DeviceInfo + a DeviceMemory capability to its register file and "IRQ" + its
	// MSI-X Interrupt capability, and wait for its report. The driver resets the
	// controller, builds the command and event rings, enumerates the root-hub
	// ports, addresses each connected device and reads its device descriptor - QEMU
	// hangs a hub with a USB keyboard and a USB tablet behind it and a mass-storage
	// stick off the controller (see qemu-run.sh), so four devices must come back
	// addressed: the hub (expanded through its class requests and route strings),
	// the keyboard and the tablet behind it (their HID interfaces configured and
	// their report descriptors parsed, which the report's keyboard and pointer
	// markers prove), and the stick (its Bulk-Only transport brought up).
	let (volume, _package) = scenario_packages().expect("boot modules should be present");
	let elf = pkg::Package::parse(volume).and_then(|p| p.lookup(b"drivers/xhci")).expect("the xhci driver should be staged on the volume under drivers/");

	// find the controller in the device table and mint its MMIO capability.
	let mut found: Option<(abi::DeviceInfo, u64, u64, usize)> = None;
	for i in 0..device::count() {
		let entry = device::with(i, |d| (d.device_type, d.bar_phys, d.bar_len)).unwrap();
		if entry.0 as u32 == abi::DEVICE_TYPE_XHCI {
			let info = device::with(i, |d| abi::DeviceInfo { device_type: d.device_type as u32, bar_len: d.bar_len, common_offset: d.common_offset, notify_offset: d.notify_offset, notify_multiplier: d.notify_multiplier, isr_offset: d.isr_offset, device_offset: d.device_offset }).unwrap();
			found = Some((info, entry.1, entry.2, i));
			break;
		}
	}
	let (info, bar_phys, bar_len, index) = found.expect("the device table should hold the xHCI controller");

	// mint the controller's MSI-X Interrupt the way sys_device_msix_acquire does:
	// reserve a vector, program table entry 0, bind the Interrupt object to the
	// vector, and enable MSI-X on the function.
	let (msix_cap, table_phys, bus, dev, func) = device::with(index, |d| (d.msix_cap, d.msix_table_phys, d.bus, d.dev, d.func)).unwrap();
	assert!(msix_cap != 0, "the xHCI controller should expose MSI-X");
	let dest = arch::percpu::this_cpu().lapic_id() as u8;
	let vector = arch::interrupts::acquire_msi(table_phys, dest, index as u32).expect("an MSI vector should be free");
	let interrupt = object::interrupt::Interrupt::new(vector);
	assert!(arch::interrupts::bind_msi(vector, &interrupt), "the MSI vector should bind");
	arch::pci::msix_enable(bus, dev, func, msix_cap);

	let (kernel_ep, user_ep) = object::channel::Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, user_ep, Rights::ALL, 0).expect("the xhci driver should load");
	let mut msg = alloc::vec::Vec::with_capacity(6 + core::mem::size_of::<abi::DeviceInfo>());
	msg.extend_from_slice(b"DEVICE");
	msg.extend_from_slice(unsafe { core::slice::from_raw_parts(&info as *const abi::DeviceInfo as *const u8, core::mem::size_of::<abi::DeviceInfo>()) });
	send_cap(&kernel_ep, &msg, DeviceMemory::new(bar_phys, bar_len as usize), Rights::ALL).expect("the DEVICE handoff should send");
	send_cap(&kernel_ep, b"IRQ", interrupt, Rights::ALL).expect("the IRQ handoff should send");
	sched::run_until_idle();

	let report = kernel_ep.recv().expect("the xhci driver should report in");
	assert_eq!(&report.bytes[..], b"driver.xhci: online (4 device(s)) (keyboard) (pointer) (storage)", "the driver should expand the hub, address the QEMU USB keyboard and tablet behind it and the stick, and configure all three classes");

	// the report is followed by the bus query channel ("USBBUS"): drive one raw
	// `usb.list` request over it ([op u16][correlation u32], the generated wire
	// header) and expect a successful reply naming all four devices' roles - the
	// live inventory `lsusb` reads.
	let usbq_msg = kernel_ep.recv().expect("the USBBUS message should follow the report");
	assert_eq!(&usbq_msg.bytes[..], b"USBBUS", "the second message carries the bus query channel");
	let usbq_cap = usbq_msg.caps.first().expect("the query channel is transferred with it");
	let usbq = usbq_cap.object().into_any_arc().downcast::<Channel>().expect("the query channel is a channel");
	// the pointer-event channel follows ("POINTER"): the raw stream a USB pointing
	// device's reports feed, routed to InputService live.
	let ptr_msg = kernel_ep.recv().expect("the POINTER message should follow USBBUS");
	assert_eq!(&ptr_msg.bytes[..], b"POINTER", "the third message carries the pointer-event channel");
	assert!(ptr_msg.caps.first().is_some(), "the pointer channel is transferred with it");
	let mut list = alloc::vec::Vec::new();
	list.extend_from_slice(&1u16.to_le_bytes()); // OP_LIST
	list.extend_from_slice(&1u32.to_le_bytes()); // correlation id
	usbq.send(Message::new(list, alloc::vec::Vec::new(), 0)).expect("the usb.list request should send");
	sched::run_until_idle();
	let inventory = usbq.recv().expect("the usb.list reply should arrive");
	assert!(inventory.bytes.len() >= 5 && inventory.bytes[4] == 1, "the inventory query should succeed");
	let has = |needle: &[u8]| inventory.bytes.windows(needle.len()).any(|w| w == needle);
	assert!(has(b"hub") && has(b"keyboard") && has(b"pointer") && has(b"storage"), "the inventory should name the hub, the keyboard, the tablet and the stick by role");

	// the report carries the disk's block channel: read sector 0 over it, the same
	// [op u32][lba u64][count u32] contract driver.virtio-blk serves, and expect a
	// success status plus a 512-byte shared buffer.
	let cap = report.caps.first().expect("the block channel is transferred with the report");
	let blk = cap.object().into_any_arc().downcast::<Channel>().expect("the block channel is a channel");
	// first the capacity query (op 2): the reply is [status u32][capacity bytes u64]
	// and must report the seeded 16 MB stick image.
	let mut capacity = alloc::vec::Vec::with_capacity(16);
	capacity.extend_from_slice(&2u32.to_le_bytes()); // op = capacity
	capacity.extend_from_slice(&0u64.to_le_bytes());
	capacity.extend_from_slice(&0u32.to_le_bytes());
	blk.send(Message::new(capacity, alloc::vec::Vec::new(), 0)).expect("the capacity request should send");
	sched::run_until_idle();
	let cap_reply = blk.recv().expect("the capacity reply should arrive");
	assert_eq!(&cap_reply.bytes[..4], &0u32.to_le_bytes(), "the capacity query should succeed");
	let bytes = u64::from_le_bytes([cap_reply.bytes[4], cap_reply.bytes[5], cap_reply.bytes[6], cap_reply.bytes[7], cap_reply.bytes[8], cap_reply.bytes[9], cap_reply.bytes[10], cap_reply.bytes[11]]);
	assert_eq!(bytes, 16 * 1024 * 1024, "the stick should report its seeded 16 MB capacity");
	let mut request = alloc::vec::Vec::with_capacity(16);
	request.extend_from_slice(&0u32.to_le_bytes()); // op = read
	request.extend_from_slice(&0u64.to_le_bytes()); // lba 0
	request.extend_from_slice(&1u32.to_le_bytes()); // one sector
	blk.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("the block request should send");
	sched::run_until_idle();
	let reply = blk.recv().expect("the block reply should arrive");
	assert_eq!(&reply.bytes[..4], &0u32.to_le_bytes(), "the USB read should succeed");
	let buf_cap = reply.caps.first().expect("the read should grant a buffer");
	let object = buf_cap.object();
	let memory = object.as_any().downcast_ref::<object::memory_object::MemoryObject>().expect("the granted capability should be a buffer");
	assert_eq!(read_from_object(memory, 512).len(), 512, "the buffer should hold the sector");

	// the vol://usb volume end to end: a StorageService instance is handed the same
	// block channel ("USBBLOCK" - the removable FAT backing that mounts lazily, on
	// first use) and a serve channel, and must resolve a file off the stick's FAT
	// image - the same bytes the seed laid down from volume/. The kernel's block
	// endpoint moves to the service whole: the service is its consumer now.
	let (volume2, package) = scenario_packages().expect("boot modules should be present");
	let service_elf = package.lookup(b"storage_service").expect("storage_service should be in the init package");
	let (service_boot_kernel, service_boot_user) = object::channel::Channel::create();
	let (service_server, service_client) = object::channel::Channel::create();
	loader::spawn_elf_process(sched::root_domain(), service_elf, service_boot_user, Rights::ALL, 0).expect("the StorageService should load");
	send_cap(&service_boot_kernel, b"USBBLOCK", blk, Rights::ALL).expect("the USBBLOCK handoff should send");
	send_cap(&service_boot_kernel, b"SERVE", service_server, Rights::ALL).expect("the SERVE handoff should send");
	sched::run_until_idle();
	let online = service_boot_kernel.recv().expect("the usb StorageService should report in");
	assert_eq!(&online.bytes[..], b"StorageService: online", "the instance should come up without touching the media (the mount is lazy)");

	// one generated volume.open request for a seeded file, plus the quit sentinel.
	let uri: &[u8] = b"vol://usb/hello.txt";
	let mut open = alloc::vec::Vec::new();
	open.extend_from_slice(&1u16.to_le_bytes()); // OP_OPEN
	open.extend_from_slice(&1u32.to_le_bytes()); // correlation id
	open.extend_from_slice(&(uri.len() as u16).to_le_bytes());
	open.extend_from_slice(uri);
	open.push(0); // write = false
	open.push(0); // create = false
	service_client.send(Message::new(open, alloc::vec::Vec::new(), 0)).expect("the open request should send");
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("the quit sentinel should send");
	sched::run_until_idle();
	let reply = service_client.recv().expect("the open reply should arrive");
	assert!(reply.bytes.len() >= 17 && reply.bytes[4] == 1, "the usb volume should resolve the seeded file");
	let size = u64::from_le_bytes([reply.bytes[9], reply.bytes[10], reply.bytes[11], reply.bytes[12], reply.bytes[13], reply.bytes[14], reply.bytes[15], reply.bytes[16]]) as usize;
	let file_cap = reply.caps.first().expect("the open should grant the file buffer");
	let file_object = file_cap.object();
	let file = file_object.as_any().downcast_ref::<object::memory_object::MemoryObject>().expect("the granted capability should be a buffer");
	let expected = volume_file(volume2, b"hello.txt").expect("hello.txt should be in the volume package");
	assert_eq!(read_from_object(file, size), expected, "vol://usb should serve the seeded file's bytes");
}

#[test_case]
fn dma_buffer_maps_and_reports_phys() {
	use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
	// A driver allocates a DMA buffer for its virtqueue, maps it, and programs its
	// physical base into the device. Here a thread writes a marker through the
	// mapping and reads it back at the reported physical address (via the HHDM),
	// proving the mapping and the phys base name the same memory - what makes device
	// DMA work. The check runs inside the thread, while the buffer is still alive
	// (it is freed when the thread's process is reaped).
	const MARK: u64 = 0xc0ffee_d00d_u64;
	static PHYS: AtomicU64 = AtomicU64::new(0);
	static READBACK: AtomicU64 = AtomicU64::new(0);
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let handle = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_CREATE, 4096, 0, 0, 0);
			if syscall::sys_is_err(handle) {
				return;
			}
			let virt = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_MAP, handle, 0, 0, 0);
			let phys = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_PHYS, handle, 0, 0, 0);
			if syscall::sys_is_err(virt) {
				return;
			}
			(virt as *mut u64).write_volatile(MARK);
			let via_hhdm = ((mem::hhdm_offset() + phys) as *const u64).read_volatile();
			PHYS.store(phys, Ordering::SeqCst);
			READBACK.store(via_hhdm, Ordering::SeqCst);
			DONE.store(true, Ordering::SeqCst);
		}
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the DMA buffer thread did not complete");
	assert!(PHYS.load(Ordering::SeqCst) != 0, "the DMA buffer should report a non-zero physical base");
	assert_eq!(READBACK.load(Ordering::SeqCst), MARK, "the bytes written through the mapping must be visible at the physical base");
}

#[test_case]
fn log_record_roundtrip_and_renders() {
	use abi::log::{LogRecord, Severity, encode, render_cbor, render_json, render_text};
	// A LogRecord is the canonical structured object; text/JSON/CBOR are derived
	// representations. Encode one, parse it back (the fields survive), then render
	// the SAME record three ways and check each representation byte-for-byte.
	let fields: [(&[u8], &[u8]); 2] = [(b"event", b"online"), (b"files", b"2")];
	let mut wire: [u8; 128] = [0u8; 128];
	let n: usize = encode(42, Severity::Info, b"storage_service", &fields, &mut wire).expect("encode fits");
	let rec: LogRecord<'_> = LogRecord::parse(&wire[..n]).expect("parse round-trips");
	assert_eq!(rec.ts(), 42);
	assert_eq!(rec.severity(), Severity::Info);
	assert_eq!(rec.source(), b"storage_service");
	assert_eq!(rec.field_count(), 2);
	let mut it = rec.fields();
	assert_eq!(it.next(), Some((&b"event"[..], &b"online"[..])));
	assert_eq!(it.next(), Some((&b"files"[..], &b"2"[..])));
	assert_eq!(it.next(), None);
	// human text
	let mut tbuf: [u8; 128] = [0u8; 128];
	let tn: usize = render_text(&rec, &mut tbuf).expect("text fits");
	assert_eq!(&tbuf[..tn], b"[42] INFO storage_service: event=online files=2");
	// JSON
	let mut jbuf: [u8; 256] = [0u8; 256];
	let jn: usize = render_json(&rec, &mut jbuf).expect("json fits");
	assert_eq!(&jbuf[..jn], br#"{"ts":42,"severity":"INFO","source":"storage_service","fields":{"event":"online","files":"2"}}"#);
	// CBOR: map(4); spot-check the head and that the source text is embedded
	let mut cbuf: [u8; 128] = [0u8; 128];
	let cn: usize = render_cbor(&rec, &mut cbuf).expect("cbor fits");
	assert_eq!(cbuf[0], 0xA4, "CBOR record is a 4-entry map");
	assert!(cbuf[..cn].windows(b"storage_service".len()).any(|w: &[u8]| w == b"storage_service"), "source string present in CBOR");
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
	loader::spawn_elf_process(sched::root_domain(), service_elf, boot_user, Rights::ALL, 0).expect("spawn service");
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
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");
	(boot_kernel, service_client)
}

// A managed service that cannot complete a required bootstrap step reports the failing
// step and the reason over its bootstrap channel before exiting, so the supervisor
// records why it went down instead of seeing an unexplained peer-close. DeviceManager
// needs the init package before it reports in; hand it a plain message where the package
// should be and it reports the failure honestly rather than dying silently.
#[test_case]
fn a_service_reports_a_bootstrap_failure() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	let init = init_package_bytes().expect("init package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let device_elf = package.lookup(b"device_manager").expect("device_manager in the init package");
	let (boot_kernel, boot_user) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), device_elf, boot_user, Rights::ALL, 0).expect("spawn device_manager");
	// Where the "PACKAGE" grant should be, hand it a plain message with no transferred
	// object: recv_package rejects it and the service reports the failing step.
	boot_kernel.send(Message::new(b"NOTAPACKAGE".to_vec(), alloc::vec::Vec::new(), 0)).expect("bogus bootstrap");

	sched::run_until_idle();

	let report = boot_kernel.recv().expect("a bootstrap failure report");
	assert!(report.bytes.starts_with(b"BOOTFAIL"), "reports the failing step, not a silent exit");
	assert!(report.bytes.windows(7).any(|w| w == b"package"), "the report names the failing step");
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

#[test_case]
fn log_service_speaks_generated_bindings() {
	use abi::log::{self, Severity};
	use object::channel::Message;

	// Drive the real userspace LogService as a client over its generated Log
	// bindings: spawn it from the init package, hand it a serve channel, EMIT two
	// records and QUERY them back. The wire is the proto framing - request
	// [op u16][corr u32][args], reply [corr u32][result] - and the proto Entry
	// encoding is byte-for-byte the abi::log record, so we build entries with
	// log::encode and frame them by hand. Everything is pre-queued so the
	// cooperative service drains it in one pass and exits, after which we read its
	// replies (the kernel-as-client pattern).
	let (_boot_kernel, service_client) = spawn_service(b"log_service");

	// EMIT one record: [op = 1 (emit) u16][corr u32][entry bytes].
	let emit = |corr: u32, ts: u64, severity: Severity, source: &[u8], fields: &[(&[u8], &[u8])]| {
		let mut wire = [0u8; 128];
		let n = log::encode(ts, severity, source, fields, &mut wire).expect("encode entry");
		let mut msg = alloc::vec::Vec::new();
		msg.extend_from_slice(&1u16.to_le_bytes());
		msg.extend_from_slice(&corr.to_le_bytes());
		msg.extend_from_slice(&wire[..n]);
		service_client.send(Message::new(msg, alloc::vec::Vec::new(), 0)).expect("emit");
	};
	emit(1, 10, Severity::Info, b"storage_service", &[(b"event" as &[u8], b"online" as &[u8])]);
	emit(2, 11, Severity::Error, b"device_manager", &[(b"code" as &[u8], b"5" as &[u8])]);

	// QUERY all severities: [op = 2 (query) u16][corr u32][query bytes]. The query
	// record is since:option<u64> min-severity:option<severity> source:option<string>
	// boot:option<u32> limit:u32; all-absent with limit 0 is eight zero bytes.
	let mut q = alloc::vec::Vec::new();
	q.extend_from_slice(&2u16.to_le_bytes());
	q.extend_from_slice(&7u32.to_le_bytes());
	q.extend_from_slice(&[0u8; 8]);
	service_client.send(Message::new(q, alloc::vec::Vec::new(), 0)).expect("query");
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");

	sched::run_until_idle();

	// Each emit is a round-trip replying result<unit, error> = [corr u32][ok u8 = 1].
	for corr in [1u32, 2] {
		let reply = service_client.recv().expect("emit reply");
		assert_eq!(reply.bytes.len(), 5, "emit reply is corr + ok");
		assert_eq!(le_u32(&reply.bytes, 0), corr, "emit reply echoes the correlation id");
		assert_eq!(reply.bytes[4], 1, "emit succeeded");
	}

	// The query reply is [corr u32 = 7][ok u8 = 1][count u16 = 2][entry][entry].
	let reply = service_client.recv().expect("query reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), 7, "query reply echoes the correlation id");
	assert_eq!(b[4], 1, "query succeeded");
	assert_eq!(le_u16(b, 5), 2, "both records came back");
	// spot-check both entries are present in the structured reply
	assert!(b.windows(b"storage_service".len()).any(|w: &[u8]| w == b"storage_service"), "first entry present");
	assert!(b.windows(b"device_manager".len()).any(|w: &[u8]| w == b"device_manager"), "second entry present");
}

#[test_case]
fn device_service_lists_devices() {
	use object::channel::Message;

	// Drive the real userspace DeviceService as a client over its generated Device
	// bindings: spawn it, hand it a serve channel, and LIST the devices the kernel
	// discovered on the bus. The wire is the proto framing - request [op u16][corr
	// u32][args], reply [corr u32][result]; `list` takes no args and replies
	// result<list<device-entry>, error>. Everything is pre-queued so the cooperative
	// service drains it in one pass and exits (the kernel-as-client pattern).
	let (boot_kernel, service_client) = spawn_service(b"device_service");

	// LIST: [op = 1 (list) u16][corr u32], no args. Then an empty quit sentinel.
	let corr: u32 = 9;
	let mut req = alloc::vec::Vec::new();
	req.extend_from_slice(&1u16.to_le_bytes());
	req.extend_from_slice(&corr.to_le_bytes());
	service_client.send(Message::new(req, alloc::vec::Vec::new(), 0)).expect("list request");
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");

	sched::run_until_idle();

	// the service reports in on its bootstrap channel before it serves
	let online = boot_kernel.recv().expect("DeviceService online report");
	assert_eq!(&online.bytes[..], b"DeviceService: online", "DeviceService reports in");

	// The list reply is [corr u32][ok u8 = 1][count u16][device-entry...], each entry
	// [index u32][type u8][mmio-len u64]. QEMU exposes the virtio devices the kernel
	// found on the bus, so the count is non-zero and the first entry is index 0.
	let reply = service_client.recv().expect("list reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), corr, "list reply echoes the correlation id");
	assert_eq!(b[4], 1, "list succeeded");
	let count = le_u16(b, 5);
	assert!(count >= 1, "at least one device was enumerated");
	assert_eq!(le_u32(b, 7), 0, "the first device is index 0");
}

#[test_case]
fn input_service_streams_pointer_events() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// Drive the real userspace InputService end to end over its generated Input
	// bindings: spawn it from the init package, hand it a SERVE channel, an INPUT
	// raw channel (the one the virtio_input pointer driver would feed), and a FORWARD
	// channel (ConsoleService's pointer sink, which it mirrors raw events to), inject a
	// couple of normalized [x u16][y u16][buttons u8] pointer events the way the
	// driver does, then SUBSCRIBE and read the mapped text-cell events back off the
	// stream. The pointer device is interactive-only, so here the test plays the
	// driver itself by sending raw events on the producer end it keeps.
	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let service_elf = program_elf(&package, volume, b"input_service").expect("input_service in the package or volume");
	let (boot_kernel, boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();
	let (raw_producer, raw_consumer) = Channel::create();
	// ConsoleService's pointer sink: the test keeps the consumer end alive so the forward
	// channel stays open (InputService mirrors each raw event to it), but does not assert
	// on it here - the forwarding path is exercised by the live console.
	let (_forward_drain, forward_input) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), service_elf, boot_user, Rights::ALL, 0).expect("spawn InputService");
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");
	send_cap(&boot_kernel, b"INPUT", raw_consumer, Rights::ALL).expect("input raw bootstrap");
	// no USB pointer in this scenario: the second raw channel is absent (handle 0).
	boot_kernel.send(Message::new(b"INPUT2".to_vec(), alloc::vec::Vec::new(), 0)).expect("input2 raw bootstrap");
	send_cap(&boot_kernel, b"FORWARD", forward_input, Rights::ALL).expect("forward raw bootstrap");

	// Inject two normalized pointer events as the driver would. The grid is COLS = 80
	// x ROWS = 50 over the 0..0x10000 normalized span, so col = (x * 80) / 0x10000 and
	// row = (y * 50) / 0x10000. x = y = 0x8000 (half span) lands on col 40 / row 25
	// with the left button held; the second event is the top-left corner, no buttons.
	let raw_event = |x: u16, y: u16, buttons: u8| -> Message {
		let mut bytes = alloc::vec::Vec::new();
		bytes.extend_from_slice(&x.to_le_bytes());
		bytes.extend_from_slice(&y.to_le_bytes());
		bytes.push(buttons);
		Message::new(bytes, alloc::vec::Vec::new(), 0)
	};
	raw_producer.send(raw_event(0x8000, 0x8000, 1)).expect("first pointer event");
	raw_producer.send(raw_event(0, 0, 0)).expect("second pointer event");

	// SUBSCRIBE: [op = 1 (subscribe) u16][corr u32], no args.
	let corr: u32 = 7;
	let mut req = alloc::vec::Vec::new();
	req.extend_from_slice(&1u16.to_le_bytes());
	req.extend_from_slice(&corr.to_le_bytes());
	service_client.send(Message::new(req, alloc::vec::Vec::new(), 0)).expect("subscribe request");

	sched::run_until_idle();

	// the service reports in on its bootstrap channel before it serves
	let online = boot_kernel.recv().expect("InputService online report");
	assert_eq!(&online.bytes[..], b"InputService: online", "InputService reports in");

	// the subscribe reply is [corr u32] with the stream consumer transferred out of band
	let reply = service_client.recv().expect("subscribe reply");
	assert_eq!(le_u32(&reply.bytes, 0), corr, "subscribe reply echoes the correlation id");
	let cap = reply.caps.first().expect("the stream consumer is transferred");
	let consumer = cap.object().into_any_arc().downcast::<Channel>().expect("the consumer is a channel");

	// each event rides its own framed message [seq u32][col u16][row u16][buttons u8];
	// closing the producer ends the stream, so recv drains to a clean close.
	let mut events = alloc::vec::Vec::new();
	while let Ok(frame) = consumer.recv() {
		let f = &frame.bytes;
		events.push((le_u16(f, 4), le_u16(f, 6), f[8]));
	}
	assert_eq!(events.len(), 2, "both injected pointer events stream back");
	assert_eq!(events[0], (40, 25, 1), "the half-span event maps to the middle cell with the left button");
	assert_eq!(events[1], (0, 0, 0), "the corner event maps to column 0, row 0, no buttons");
}

#[test_case]
fn dhcp_lease_renews_at_t1_and_restarts_its_clock() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// Drive the real userspace NetworkService end to end as its DHCP server AND its
	// frame-mover driver: spawn it with FRAMES + SERVE channels, lead with its MAC,
	// answer the DISCOVER -> REQUEST handshake with a lease whose clock is short
	// (T1 = 1 s, T2 = 2 s, lease 3 s), answer the gratuitous ARP so the service
	// learns the server's MAC, and then let the scheduler tick: at T1 the service
	// must send the lease-extension REQUEST on its own - the RFC 2131 RENEWING form
	// (ciaddr filled, unicast to the server, no server-id option) - and an ACK must
	// restart its clock, proven by the NEXT renewal arriving a full T1 later rather
	// than at the unanswered-retransmit pace.
	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let service_elf = program_elf(&package, volume, b"network_service").expect("network_service in the package or volume");
	let our_mac: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
	let srv_mac: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
	let leased: [u8; 4] = [10, 0, 2, 99];
	let server: [u8; 4] = [10, 0, 2, 2];

	// Build a DHCP server reply frame (Ethernet + IPv4 + UDP 67 -> 68 + BOOTP reply
	// with the lease-clock options; the stack verifies no checksums).
	let reply = |msg_type: u8, dst_ip: [u8; 4], dst_mac: [u8; 6]| -> Message {
		let mut bootp = alloc::vec![0u8; 236];
		bootp[0] = 2; // BOOTREPLY
		bootp[16..20].copy_from_slice(&leased); // yiaddr
		bootp.extend_from_slice(&0x6382_5363u32.to_be_bytes());
		bootp.extend_from_slice(&[53, 1, msg_type]);
		bootp.extend_from_slice(&[54, 4, server[0], server[1], server[2], server[3]]);
		bootp.extend_from_slice(&[1, 4, 255, 255, 255, 0]);
		bootp.extend_from_slice(&[3, 4, server[0], server[1], server[2], server[3]]);
		bootp.extend_from_slice(&[6, 4, 10, 0, 2, 3]);
		bootp.extend_from_slice(&[51, 4, 0, 0, 0, 3]); // lease 3 s
		bootp.extend_from_slice(&[58, 4, 0, 0, 0, 1]); // T1 1 s
		bootp.extend_from_slice(&[59, 4, 0, 0, 0, 2]); // T2 2 s
		bootp.push(255);
		let mut f = alloc::vec::Vec::new();
		f.extend_from_slice(&dst_mac);
		f.extend_from_slice(&srv_mac);
		f.extend_from_slice(&0x0800u16.to_be_bytes());
		let total: u16 = (20 + 8 + bootp.len()) as u16;
		let mut ip = [0u8; 20];
		ip[0] = 0x45;
		ip[2..4].copy_from_slice(&total.to_be_bytes());
		ip[8] = 64;
		ip[9] = 17; // UDP
		ip[12..16].copy_from_slice(&server);
		ip[16..20].copy_from_slice(&dst_ip);
		f.extend_from_slice(&ip);
		f.extend_from_slice(&67u16.to_be_bytes());
		f.extend_from_slice(&68u16.to_be_bytes());
		f.extend_from_slice(&((8 + bootp.len()) as u16).to_be_bytes());
		f.extend_from_slice(&[0, 0]); // checksum: unverified
		f.extend_from_slice(&bootp);
		Message::new(f, alloc::vec::Vec::new(), 0)
	};
	// Decode a frame from the service: a DHCP client message's (type, ciaddr,
	// unicast Ethernet destination, server-id option present), or None.
	let decode = |f: &[u8]| -> Option<(u8, [u8; 4], bool, bool)> {
		if f.len() < 14 + 20 + 8 + 240 || f[12..14] != [0x08, 0x00] || f[14 + 9] != 17 {
			return None;
		}
		if f[14 + 20..14 + 22] != [0, 68] || f[14 + 22..14 + 24] != [0, 67] {
			return None;
		}
		let bootp = &f[14 + 20 + 8..];
		let ciaddr: [u8; 4] = [bootp[12], bootp[13], bootp[14], bootp[15]];
		let mut msg_type: u8 = 0;
		let mut server_id: bool = false;
		let mut p: usize = 240;
		while p + 2 <= bootp.len() && bootp[p] != 255 {
			match bootp[p] {
				0 => p += 1,
				53 => {
					msg_type = bootp[p + 2];
					p += 2 + bootp[p + 1] as usize;
				}
				54 => {
					server_id = true;
					p += 2 + bootp[p + 1] as usize;
				}
				_ => p += 2 + bootp[p + 1] as usize,
			}
		}
		Some((msg_type, ciaddr, f[0..6] == srv_mac, server_id))
	};

	let (boot_kernel, boot_user) = Channel::create();
	let (frames_kernel, frames_user) = Channel::create();
	let (_serve_kernel, serve_user) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), service_elf, boot_user, Rights::ALL, 0).expect("spawn NetworkService");
	send_cap(&boot_kernel, b"FRAMES", frames_user, Rights::ALL).expect("frames bootstrap");
	// no config tree serves this scenario: CONFIG with no handle tells the service
	// to fall back to its compiled-in defaults (the neighbor-cache size).
	boot_kernel.send(Message::new(b"CONFIG".to_vec(), alloc::vec::Vec::new(), 0)).expect("config bootstrap");
	send_cap(&boot_kernel, b"SERVE", serve_user, Rights::ALL).expect("serve bootstrap");
	// Pre-queue the whole bind conversation (the kernel test thread cannot answer
	// mid-wait): the MAC lead-in, the OFFER and the clock-carrying ACK the handshake
	// will consume in order, and the ARP reply that teaches the service the server's
	// MAC (its own gratuitous ARP pumps it in), so the T1 renewal can go unicast.
	let mut mac_msg = alloc::vec::Vec::new();
	mac_msg.extend_from_slice(b"MAC");
	mac_msg.extend_from_slice(&our_mac);
	frames_kernel.send(Message::new(mac_msg, alloc::vec::Vec::new(), 0)).expect("MAC handoff");
	frames_kernel.send(reply(2, [255; 4], [0xff; 6])).expect("the OFFER should queue");
	frames_kernel.send(reply(5, [255; 4], [0xff; 6])).expect("the ACK should queue");
	let mut arp_reply = alloc::vec::Vec::new();
	arp_reply.extend_from_slice(&our_mac);
	arp_reply.extend_from_slice(&srv_mac);
	arp_reply.extend_from_slice(&[0x08, 0x06]);
	arp_reply.extend_from_slice(&[0, 1, 0x08, 0, 6, 4, 0, 2]);
	arp_reply.extend_from_slice(&srv_mac);
	arp_reply.extend_from_slice(&server);
	arp_reply.extend_from_slice(&our_mac);
	arp_reply.extend_from_slice(&leased);
	frames_kernel.send(Message::new(arp_reply, alloc::vec::Vec::new(), 0)).expect("the ARP reply should queue");
	sched::run_until_idle();

	// The service binds and reports in; its side of the conversation arrives in
	// order: the DISCOVER, the selecting REQUEST (ciaddr empty, server-id present),
	// and the gratuitous ARP announcement.
	let online = boot_kernel.recv().expect("NetworkService online report");
	assert_eq!(&online.bytes[..], b"NetworkService: online", "the service binds and reports in");
	let discover = frames_kernel.recv().expect("the DISCOVER should broadcast");
	assert_eq!(decode(&discover.bytes).map(|(t, _, _, _)| t), Some(1), "the first frame is the DISCOVER");
	let request = frames_kernel.recv().expect("the REQUEST should follow the OFFER");
	let (rtype, rciaddr, _, rsid) = decode(&request.bytes).expect("the second frame decodes");
	assert!(rtype == 3 && rciaddr == [0; 4] && rsid, "the selecting REQUEST names the server, ciaddr empty");
	let arp = frames_kernel.recv().expect("the gratuitous ARP should send");
	assert_eq!(&arp.bytes[12..14], &[0x08, 0x06], "the announcement is an ARP request");

	// Let the clock tick to T1: the service must wake itself (the lease deadline is
	// a periodic housekeeping wake) and send the RENEWING-form REQUEST.
	let mut renewal: Option<Message> = None;
	let give_up = arch::apic::ticks() + 500;
	while renewal.is_none() && arch::apic::ticks() < give_up {
		sched::run_until_idle();
		arch::idle_halt();
		renewal = frames_kernel.recv().ok();
	}
	let renewal = renewal.expect("the T1 renewal REQUEST should arrive unprompted");
	let (t, ciaddr, unicast, sid) = decode(&renewal.bytes).expect("the renewal decodes");
	assert_eq!(t, 3, "the renewal is a REQUEST");
	assert_eq!(ciaddr, leased, "the renewal carries the bound address in ciaddr");
	assert!(unicast, "the renewal goes unicast to the server it learned by ARP");
	assert!(!sid, "the RENEWING form omits the server-id option");

	// ACK the renewal (unicast to the bound address) and prove the clock RESTARTED:
	// the next renewal must arrive a full T1 (~100 ticks) later - an unanswered
	// REQUEST would have retransmitted at half the time to T2 (~50 ticks) instead.
	let acked_at = arch::apic::ticks();
	frames_kernel.send(reply(5, leased, our_mac)).expect("the renewal ACK should send");
	let mut second: Option<Message> = None;
	let give_up = acked_at + 500;
	while second.is_none() && arch::apic::ticks() < give_up {
		sched::run_until_idle();
		arch::idle_halt();
		second = frames_kernel.recv().ok();
	}
	let second = second.expect("the next T1 renewal should arrive");
	let (t2, ciaddr2, _, _) = decode(&second.bytes).expect("the second renewal decodes");
	assert!(t2 == 3 && ciaddr2 == leased, "the clock re-arms another renewal");
	assert!(arch::apic::ticks() - acked_at >= 75, "the renewal came at the restarted T1, not the retransmit pace");
}

#[test_case]
fn process_service_starts_a_program() {
	use object::channel::Message;

	// Drive the real userspace ProcessService over its generated Process bindings:
	// spawn it, hand it the init package (to launch from) and a serve channel, then
	// START a program and LIST it back. The wire is the proto framing - request [op
	// u16][corr u32][args], reply [corr u32][result]; `start` takes a string name and
	// replies result<process-info, error> = [koid u64][name string]. Everything is
	// pre-queued so the cooperative service drains it in one pass and exits.
	let (boot_kernel, service_client) = spawn_service_with_package(b"process_service");

	// START a pinned program by name (log_service is in the init package, which this
	// ProcessService falls back to since it has no storage client): [op = 1 u16][corr
	// u32][name: [len u16][utf8]].
	let name: &[u8] = b"log_service";
	let mut start = alloc::vec::Vec::new();
	start.extend_from_slice(&1u16.to_le_bytes());
	start.extend_from_slice(&1u32.to_le_bytes());
	start.extend_from_slice(&(name.len() as u16).to_le_bytes());
	start.extend_from_slice(name);
	service_client.send(Message::new(start, alloc::vec::Vec::new(), 0)).expect("start request");

	// LIST: [op = 2 u16][corr u32]. Then an empty quit sentinel.
	let mut list = alloc::vec::Vec::new();
	list.extend_from_slice(&2u16.to_le_bytes());
	list.extend_from_slice(&2u32.to_le_bytes());
	service_client.send(Message::new(list, alloc::vec::Vec::new(), 0)).expect("list request");
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");

	sched::run_until_idle();

	// the service reports in on its bootstrap channel before it serves
	let online = boot_kernel.recv().expect("ProcessService online report");
	assert_eq!(&online.bytes[..], b"ProcessService: online", "ProcessService reports in");

	// The start reply is [corr u32 = 1][ok u8 = 1][koid u64][name: [len u16][utf8]].
	let reply = service_client.recv().expect("start reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), 1, "start reply echoes the correlation id");
	assert_eq!(b[4], 1, "start succeeded");
	let koid = le_u64(b, 5);
	assert!(koid >= 1, "the started process has a koid");
	let name_len = le_u16(b, 13) as usize;
	assert_eq!(&b[15..15 + name_len], name, "the reply echoes the launched program name");

	// The list reply is [corr u32 = 2][ok u8 = 1][count u16 = 1][process-info].
	let reply = service_client.recv().expect("list reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), 2, "list reply echoes the correlation id");
	assert_eq!(b[4], 1, "list succeeded");
	assert_eq!(le_u16(b, 5), 1, "the started process is listed");
}

#[test_case]
fn process_service_loads_a_program_from_system_bin() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// M61 box 2: ProcessService loads a named program's ELF from the system volume's
	// `bin/` through a StorageService client, not the init package. Stand up a
	// StorageService over the factory volume archive (which stages the tools under
	// `bin/`) and a ProcessService wired to its client, then START a staged tool by name:
	// ProcessService resolves it to `vol://system/bin/<name>` and loads it off the volume,
	// proving the on-disk load path the shell's `run` and ConsoleService's shell spawn now
	// take.
	let (volume, package) = scenario_packages().expect("scenario packages");
	let init = init_package_bytes().expect("init package module not found");
	let storage_elf = package.lookup(b"storage_service").expect("storage_service in the init package");
	let process_elf = package.lookup(b"process_service").expect("process_service in the init package");

	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (process_boot_kernel, process_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	let (process_server, process_client) = Channel::create();

	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).expect("spawn StorageService");
	loader::spawn_elf_process(domain, process_elf, process_boot_user, Rights::ALL, 0).expect("spawn ProcessService");

	// StorageService: the ramdisk volume archive (staging the tools under bin/) and its
	// service channel.
	send_ramdisk(&storage_boot_kernel, volume).expect("storage ramdisk bootstrap");
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL).expect("storage serve bootstrap");

	// ProcessService: the init package (the bring-up fallback, unused here), the live
	// StorageService client it loads binaries from, then its own service channel. The
	// receive order matches ProcessService's: package, storage, serve.
	send_package(&process_boot_kernel, init).expect("process package bootstrap");
	send_cap(&process_boot_kernel, b"STORAGE", storage_client, Rights::ALL).expect("process storage bootstrap");
	send_cap(&process_boot_kernel, b"SERVE", process_server, Rights::ALL).expect("process serve bootstrap");

	// START a staged tool: [op = 1 u16][corr u32][name: [len u16][utf8]], then quit.
	let name: &[u8] = b"ptyecho";
	let mut start = alloc::vec::Vec::new();
	start.extend_from_slice(&1u16.to_le_bytes());
	start.extend_from_slice(&1u32.to_le_bytes());
	start.extend_from_slice(&(name.len() as u16).to_le_bytes());
	start.extend_from_slice(name);
	process_client.send(Message::new(start, alloc::vec::Vec::new(), 0)).expect("start request");
	process_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");

	sched::run_until_idle();

	// the service reports in on its bootstrap channel before it serves.
	let online = process_boot_kernel.recv().expect("ProcessService online report");
	assert_eq!(&online.bytes[..], b"ProcessService: online", "ProcessService reports in");

	// the start reply is [corr u32 = 1][ok u8 = 1][koid u64][name]: success proves the
	// binary was found and loaded from the system volume's bin/ (a missing binary would
	// reply with an error, since a wired storage client does not fall back to the package).
	let reply = process_client.recv().expect("start reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), 1, "start reply echoes the correlation id");
	assert_eq!(b[4], 1, "the staged tool loaded from vol://system/bin");
	let koid = le_u64(b, 5);
	assert!(koid >= 1, "the started process has a koid");
	let name_len = le_u16(b, 13) as usize;
	assert_eq!(&b[15..15 + name_len], name, "the reply echoes the launched program name");
}

#[test_case]
fn config_service_serves_the_tree() {
	use object::channel::Message;

	// Drive the real userspace ConfigService over its generated Config bindings:
	// spawn it, hand it a serve channel, GET a seeded node, LIST the tree, SET a new
	// node, and GET it back. The wire is the proto framing - request [op u16][corr
	// u32][args], reply [corr u32][result]; strings are [len u16][utf8].
	let (boot_kernel, service_client) = spawn_service(b"config_service");

	// frame a GET: [op = 1 u16][corr u32][key: [len u16][utf8]].
	let get = |corr: u32, key: &[u8]| -> alloc::vec::Vec<u8> {
		let mut m = alloc::vec::Vec::new();
		m.extend_from_slice(&1u16.to_le_bytes());
		m.extend_from_slice(&corr.to_le_bytes());
		m.extend_from_slice(&(key.len() as u16).to_le_bytes());
		m.extend_from_slice(key);
		m
	};
	service_client.send(Message::new(get(1, b"system.name"), alloc::vec::Vec::new(), 0)).expect("get");

	// LIST: [op = 2 u16][corr u32].
	let mut list = alloc::vec::Vec::new();
	list.extend_from_slice(&2u16.to_le_bytes());
	list.extend_from_slice(&2u32.to_le_bytes());
	service_client.send(Message::new(list, alloc::vec::Vec::new(), 0)).expect("list");

	// SET demo.key = hi: [op = 3 u16][corr u32][config-entry: key string + value string].
	let (k, v): (&[u8], &[u8]) = (b"demo.key", b"hi");
	let mut set = alloc::vec::Vec::new();
	set.extend_from_slice(&3u16.to_le_bytes());
	set.extend_from_slice(&3u32.to_le_bytes());
	set.extend_from_slice(&(k.len() as u16).to_le_bytes());
	set.extend_from_slice(k);
	set.extend_from_slice(&(v.len() as u16).to_le_bytes());
	set.extend_from_slice(v);
	service_client.send(Message::new(set, alloc::vec::Vec::new(), 0)).expect("set");
	service_client.send(Message::new(get(4, b"demo.key"), alloc::vec::Vec::new(), 0)).expect("get-back");
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");

	sched::run_until_idle();

	let online = boot_kernel.recv().expect("ConfigService online report");
	assert_eq!(&online.bytes[..], b"ConfigService: online", "ConfigService reports in");

	// GET reply: [corr u32 = 1][ok u8 = 1][value: [len u16][utf8]].
	let r = service_client.recv().expect("get reply");
	let b = &r.bytes;
	assert_eq!(le_u32(b, 0), 1, "get echoes the correlation id");
	assert_eq!(b[4], 1, "get succeeded");
	let vlen = le_u16(b, 5) as usize;
	assert_eq!(&b[7..7 + vlen], b"LiberSystem", "system.name is the seeded value");

	// LIST reply: [corr u32 = 2][ok u8 = 1][count u16][entries...].
	let r = service_client.recv().expect("list reply");
	let b = &r.bytes;
	assert_eq!(le_u32(b, 0), 2, "list echoes the correlation id");
	assert_eq!(b[4], 1, "list succeeded");
	assert!(le_u16(b, 5) >= 4, "the seeded tree has nodes");

	// SET reply: [corr u32 = 3][ok u8 = 1].
	let r = service_client.recv().expect("set reply");
	let b = &r.bytes;
	assert_eq!(le_u32(b, 0), 3, "set echoes the correlation id");
	assert_eq!(b[4], 1, "set succeeded");

	// GET demo.key reply: the value we just set reads back.
	let r = service_client.recv().expect("get-back reply");
	let b = &r.bytes;
	assert_eq!(le_u32(b, 0), 4, "get-back echoes the correlation id");
	assert_eq!(b[4], 1, "get-back succeeded");
	let vlen = le_u16(b, 5) as usize;
	assert_eq!(&b[7..7 + vlen], b"hi", "the value just set reads back");
}

// This end-to-end test asserts the EXACT boot-chain report order, which requires the
// interrupt-driven services (NetworkService over virtio-net, and its transitive
// dependents TimeService/PermissionManager/ConsoleService/SystemGraphService/Shell) to
// all settle inside the harness's single `run_until_idle()`. It was previously gated off
// riscv64 (`#[cfg(not(target_arch = "riscv64"))]`) because those services intermittently
// failed to report in there - which turned out to be the riscv trap-frame register clobber
// (a trap could corrupt the interrupted thread's t0/x5), not an interrupt-timing issue;
// with that fixed the chain settles deterministically on riscv64 too.
#[test_case]
fn init_package_starts_system_manager() {
	// The boot chain, end to end: SystemManager starts from the init package, spawns
	// ServiceManager and delegates the package and the ramdisk to it, and
	// ServiceManager brings up the core services in dependency order - LogService
	// first, then DeviceService and ConfigService (they depend only on LogService, so
	// they come up right after), then ResourceManager (which also depends only on
	// LogService, so it comes up among them, and in turn launches the component it
	// governs and caps its Domain before reporting in), then DeviceManager,
	// StorageService (handed the disk block channel DeviceManager routes up), the
	// media StorageService (handed the second disk's block channel, mounting it as the
	// writable FAT / exFAT vol://media), the iso StorageService (handed the third disk's
	// block channel, mounting it as the read-only ISO9660 vol://iso), and the udf
	// StorageService (handed the fourth disk's block channel, mounting it as the read-only
	// UDF vol://udf - so four StorageService reports arrive),
	// NetworkService (handed the net driver's frame channel the same way), then
	// ProcessService (which depends on StorageService, since it loads the on-disk program
	// binaries from the system volume's `bin/`, so it comes up once storage is running),
	// PermissionManager (which needs storage and network to grant onward, so it comes up
	// once they are running, and in turn launches its sandboxed component before reporting
	// in), and finally - after every component it observes - SystemGraphService, then the
	// shell. Every report is relayed up, so the kernel observes the
	// services come up in dependency order, then the watchdog canary brought up,
	// restarted after a commanded crash and recovered after a missed heartbeat
	// (ServiceManager exercises the restart policy and watchdog), then the
	// transparent-restart drill: ConfigService - a REAL service other components hold
	// channels to - is killed and restarted per policy, and the canary (a standing
	// client with a CONFIG grant) re-resolves it through the broker and round-trips a
	// typed request against the restarted instance, proving a client survives a
	// service restart (and that an un-granted resolve is denied). Then DeviceManager
	// stopped (ServiceManager exercises the stop path on that service - after the
	// restart drill, whose replacement is launched from the system volume that
	// DeviceManager's virtio-blk backs), followed by the two managers.
	let (kernel_ep, _koid) = spawn_system_manager().expect("SystemManager should start from the init package");
	sched::run_until_idle();
	let reports: [&[u8]; 28] = [
		b"LogService: online",
		b"DeviceManager: online",
		b"StorageService: online",
		b"StorageService: online",
		b"StorageService: online",
		b"StorageService: online",
		b"StorageService: online",
		b"ProcessService: online",
		b"ConfigService: online",
		b"AudioService: online",
		b"InputService: online",
		b"ResourceManager: online",
		b"SessionService: online",
		b"NetworkService: online",
		b"DeviceService: online",
		b"TimeService: online",
		b"PermissionManager: online",
		b"ConsoleService: online",
		b"SystemGraphService: online",
		b"Shell: online",
		b"WatchdogProbe: online",
		b"WatchdogProbe: restarted",
		b"WatchdogProbe: recovered",
		b"ConfigService: restarted",
		b"WatchdogProbe: config client survived",
		b"DeviceManager: stopped",
		b"ServiceManager: online",
		b"SystemManager: online",
	];
	for expected in reports {
		let message = kernel_ep.recv().expect("a boot-chain report should arrive");
		assert_eq!(&message.bytes[..], expected, "boot-chain reports must arrive in dependency order");
	}
}

#[test_case]
fn pty_hosts_a_program() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// The M35i PTY abstraction: a program hosts a terminal it is not the hardware console
	// for. ConsoleService opens a pseudo-terminal on request, spawns a slave program on it,
	// and hands back the master channel; the host drives the slave through the line
	// discipline over that master, exactly as the `script` tool (and a future ssh) does.
	// Here we stand in for the host (and for VT 1's idle shell) and drive a `ptyecho` slave:
	// a line written to the master is cooked by the line discipline, delivered to the slave,
	// echoed back prefixed with "pty:", and forwarded out the master to us.
	let (volume, package) = scenario_packages().expect("scenario packages");
	let init = init_package_bytes().expect("init package module not found");
	let console_elf = program_elf(&package, volume, b"console_service").expect("console_service in the package or volume");
	let storage_elf = package.lookup(b"storage_service").expect("storage_service in the init package");
	let process_elf = package.lookup(b"process_service").expect("process_service in the init package");

	// ConsoleService's bootstrap channel and the channels its __user_main expects: VT 1's
	// data (CLIENT) + control (CONTROL), a factory per service (FSTORAGE..FNET; only FPROCESS
	// is a live ProcessService here, which loads the ptyecho slave - the rest are unused, as
	// the slave needs no services), then GPU (none) and POINTER (none).
	let (boot_kernel, boot_user) = Channel::create();
	let (vt1_console_a, _vt1_console_b) = Channel::create();
	let (ctl_console, ctl_shell) = Channel::create();
	let (dummy_a, _dummy_b) = Channel::create();

	loader::spawn_elf_process(sched::root_domain(), console_elf, boot_user, Rights::ALL, 0).expect("spawn ConsoleService");

	// A StorageService over the factory volume (which stages ptyecho under bin/), so the
	// ProcessService below can load the ptyecho slave from vol://system/bin/ptyecho.
	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), storage_elf, storage_boot_user, Rights::ALL, 0).expect("spawn StorageService");
	send_ramdisk(&storage_boot_kernel, volume).expect("storage ramdisk bootstrap");
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL).expect("storage serve bootstrap");

	// A live ProcessService the console loads and launches the ptyecho slave through (the
	// sole process-creation mechanism), reading it from the system volume through the
	// StorageService client.
	let (proc_boot_kernel, proc_boot_user) = Channel::create();
	let (proc_server, proc_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), process_elf, proc_boot_user, Rights::ALL, 0).expect("spawn ProcessService");
	send_package(&proc_boot_kernel, init).expect("process package bootstrap");
	send_cap(&proc_boot_kernel, b"STORAGE", storage_client, Rights::ALL).expect("process storage bootstrap");
	send_cap(&proc_boot_kernel, b"SERVE", proc_server, Rights::ALL).expect("process serve bootstrap");

	send_cap(&boot_kernel, b"CLIENT", vt1_console_a, Rights::ALL).expect("CLIENT bootstrap");
	send_cap(&boot_kernel, b"CONTROL", ctl_console, Rights::ALL).expect("CONTROL bootstrap");
	for tag in [&b"FSTORAGE"[..], &b"FLOG"[..], &b"FDEVICE"[..], &b"FPROCESS"[..], &b"FCONFIG"[..], &b"FTIME"[..], &b"FAUDIO"[..], &b"FSESSION"[..], &b"FPERM"[..], &b"FNET"[..]] {
		let factory: alloc::sync::Arc<dyn object::KernelObject> = if tag == b"FPROCESS" { proc_client.clone() } else { dummy_a.clone() };
		send_cap(&boot_kernel, tag, factory, Rights::ALL).expect("factory bootstrap");
	}
	boot_kernel.send(Message::new(b"GPU".to_vec(), alloc::vec::Vec::new(), 0)).expect("GPU bootstrap");
	boot_kernel.send(Message::new(b"POINTER".to_vec(), alloc::vec::Vec::new(), 0)).expect("POINTER bootstrap");
	boot_kernel.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new(), 0)).expect("READY bootstrap");

	// stand in for the shell's PTY_OPEN request: ask the console to host a `ptyecho` slave
	// on a new pty.
	ctl_shell.send(Message::new(b"PTY_OPENptyecho".to_vec(), alloc::vec::Vec::new(), 0)).expect("PTY_OPEN request");

	sched::run_until_idle();

	// the console replies "PTY" + the master channel (the host side of the pty).
	let reply = ctl_shell.recv().expect("a PTY reply should arrive");
	assert_eq!(&reply.bytes[..3], b"PTY", "the console opens the pty");
	let cap = reply.caps.first().expect("the master channel is transferred");
	let master = cap.object().into_any_arc().downcast::<Channel>().expect("the master is a channel");

	// drive the slave: a line through the master is cooked and delivered, the slave echoes
	// it back prefixed, and the prefixed line is forwarded out the master back to us.
	master.send(Message::new(b"hello\n".to_vec(), alloc::vec::Vec::new(), 0)).expect("write to the pty master");
	sched::run_until_idle();

	let mut captured = alloc::vec::Vec::new();
	while let Ok(msg) = master.recv() {
		captured.extend_from_slice(&msg.bytes);
	}
	assert!(captured.windows(b"pty:hello".len()).any(|w| w == b"pty:hello"), "the slave's reply is forwarded back out the master");
}

#[test_case]
fn interactive_tool_reads_stdin() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// A foreground tool reads its standard input, not just prints: the shell hands a
	// foreground child a full-duplex dup of its console (the controlling terminal), so the
	// child reads cooked input lines back from the same channel it prints to. Here we stand
	// in for the shell and drive a `readln` child - spawn it, hand it a console channel as its
	// STDOUT (which `rt::inherit_stdout` adopts as stdin too), deliver a cooked line the way
	// ConsoleService's line discipline would, and observe readln echo it back prefixed.
	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let readln_elf = program_elf(&package, volume, b"readln").expect("readln in the package or volume");

	// readln's bootstrap channel, and the console channel that is its stdout + stdin: the
	// child holds one end (transferred as STDOUT), we keep the other and act as the terminal.
	let (boot_kernel, boot_user) = Channel::create();
	let (console_host, console_child) = Channel::create();

	loader::spawn_elf_process(sched::root_domain(), readln_elf, boot_user, Rights::ALL, 0).expect("spawn readln");

	// Hand the child the console as STDOUT (its `inherit_stdout` adopts it as stdin too),
	// then an empty argv message - readln takes no arguments.
	send_cap(&boot_kernel, b"STDOUT", console_child, Rights::ALL).expect("STDOUT bootstrap");
	boot_kernel.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("argv bootstrap");
	sched::run_until_idle();

	// deliver one cooked input line (with its trailing newline, as the line discipline does),
	// then end input (a zero-byte read is the tty's EOF) so readln echoes it and exits.
	console_host.send(Message::new(b"hello\n".to_vec(), alloc::vec::Vec::new(), 0)).expect("write a line to the child's stdin");
	sched::run_until_idle();
	console_host.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("send EOF to the child");
	sched::run_until_idle();

	// readln echoes the line back on the same console, prefixed with "in> ".
	let mut captured = alloc::vec::Vec::new();
	while let Ok(msg) = console_host.recv() {
		captured.extend_from_slice(&msg.bytes);
	}
	assert!(captured.windows(b"in> hello".len()).any(|w| w == b"in> hello"), "the foreground tool reads its stdin and echoes it back");
}

// Run one no-argument, no-capability tool from the volume the way the launcher does
// - spawn its staged ELF, hand it a console channel as STDOUT and an empty argv -
// and return everything it printed. The zero-capability inventory commands (uname,
// uptime, dmesg, lscpu, free, lsmem, lsirq) are driven through this.
fn run_inventory_tool(name: &[u8]) -> alloc::vec::Vec<u8> {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let elf = program_elf(&package, volume, name).expect("the tool should be staged");

	let (boot_kernel, boot_user) = Channel::create();
	let (console_host, console_child) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL, 0).expect("the tool should spawn");
	send_cap(&boot_kernel, b"STDOUT", console_child, Rights::ALL).expect("STDOUT bootstrap");
	boot_kernel.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("argv bootstrap");
	sched::run_until_idle();

	let mut captured = alloc::vec::Vec::new();
	while let Ok(msg) = console_host.recv() {
		captured.extend_from_slice(&msg.bytes);
	}
	captured
}

#[test_case]
fn inventory_tools_print_the_system_identity() {
	// The zero-capability inventory commands (M63): each runs as its own sandboxed
	// ELF and prints compile-time / free-syscall data - no service client, the
	// emptiest manifests in the permission store. uname prints the product identity
	// and architecture, uptime the time since boot, and dmesg the kernel boot log
	// (the same text SYS_CONSOLE_READLOG hands ConsoleService for the boot screen).
	let uname = run_inventory_tool(b"uname");
	let arch = if cfg!(target_arch = "aarch64") {
		"aarch64"
	} else if cfg!(target_arch = "riscv64") {
		"riscv64"
	} else {
		"x86_64"
	};
	let expected = alloc::format!("{} {} {}\n", env!("PRODUCT_NAME"), env!("PRODUCT_VERSION"), arch);
	assert_eq!(uname, expected.as_bytes(), "uname should print the product name, version and architecture");

	let uptime = run_inventory_tool(b"uptime");
	assert!(uptime.starts_with(b"up ") && uptime.ends_with(b"\n"), "uptime should render the time since boot");

	let dmesg = run_inventory_tool(b"dmesg");
	assert!(!dmesg.is_empty(), "dmesg should print the kernel boot log (or report there is none)");
}

#[test_case]
fn inventory_tools_report_the_hardware() {
	// The hardware-inventory commands (M63): each runs as its own sandboxed ELF over
	// a free syscall reading state the kernel now retains past boot - the CPU set
	// (lscpu), the frame-pool and heap totals (free), the boot memory map (lsmem),
	// and the device-interrupt vector table (lsirq).
	let contains = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);

	let lscpu = run_inventory_tool(b"lscpu");
	let arch_line: &[u8] = if cfg!(target_arch = "aarch64") {
		b"arch: aarch64"
	} else if cfg!(target_arch = "riscv64") {
		b"arch: riscv64"
	} else {
		b"arch: x86_64"
	};
	assert!(contains(&lscpu, arch_line) && contains(&lscpu, b"cpu0: lapic "), "lscpu should print the architecture and each core's LAPIC id");

	let free = run_inventory_tool(b"free");
	assert!(free.starts_with(b"Mem:  total ") && contains(&free, b"Heap: total "), "free should print the frame-pool and heap totals");

	let lsmem = run_inventory_tool(b"lsmem");
	assert!(contains(&lsmem, b" usable\n"), "lsmem should print the retained boot memory map with a usable region");

	let lsirq = run_inventory_tool(b"lsirq");
	// The kernel's own fixed vector: the LAPIC timer (vector 32) on x86, the EL1
	// physical-timer PPI (INTID 30) on aarch64, the S-mode timer (scause code 5) on riscv.
	let timer_line: &[u8] = if cfg!(target_arch = "aarch64") {
		b"vector 30: fixed"
	} else if cfg!(target_arch = "riscv64") {
		b"vector 5: fixed"
	} else {
		b"vector 32: fixed"
	};
	assert!(contains(&lsirq, timer_line), "lsirq should report the kernel timer's fixed vector as in use");

	let lspci = run_inventory_tool(b"lspci");
	assert!(contains(&lspci, b"1af4:") && contains(&lspci, b"(network controller)"), "lspci should report the retained bus scan with the virtio functions");
}

// The sector where StorageService lays the fixed factory LiberFS layout when a disk
// carries no GPT partition for it - it must mirror the storage service's own
// FS_START_SECTOR (src/user/storage/src/service.rs), which sits past the largest
// architecture's factory archive so the seed always fits ahead of the filesystem.
const FACTORY_START_SECTOR: u64 = 65536;

#[test_case]
fn system_volume_formats_to_the_disks_capacity() {
	use alloc::collections::BTreeMap;
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// M65: a fresh system volume spans the whole disk - StorageService asks the block
	// device for its capacity (the block protocol's op 2) and derives the pool from
	// it, instead of formatting a fixed 32 MB. Here we stand in for the block driver
	// with a sparse in-memory disk (a sector map; unwritten sectors read back as
	// zeros) reporting a 64 MB capacity: the mount probe finds no superblock and the
	// seed probe no archive, so the service formats fresh - and the superblock it
	// lays down must record a pool spanning everything past the factory-archive
	// region, not the old fixed constant.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	const SECTOR: usize = 512;
	let expected_pool: u64 = (CAPACITY - FACTORY_START_SECTOR * SECTOR as u64) / 4096;

	let (_volume, package) = scenario_packages().expect("boot modules should be present");
	let elf = package.lookup(b"storage_service").expect("storage_service should be in the init package");

	let (boot_kernel, boot_user) = Channel::create();
	let (blk_host, blk_child) = Channel::create();
	let (serve_server, _serve_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL, 0).expect("the StorageService should load");
	send_cap(&boot_kernel, b"BLOCK", blk_child, Rights::ALL).expect("the BLOCK handoff should send");
	send_cap(&boot_kernel, b"SERVE", serve_server, Rights::ALL).expect("the SERVE handoff should send");

	// serve the raw block protocol over the sparse disk until the service reports in.
	let mut disk: alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	let mut online = false;
	'serve: for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(report) = boot_kernel.recv() {
			assert_eq!(&report.bytes[..], b"StorageService: online", "the service should come up on the fresh disk");
			online = true;
			break 'serve;
		}
	}
	assert!(online, "the service should format the disk and report in");
	// the freshly laid superblock (filesystem block 0 = the first sector past the
	// factory-archive region) must record the capacity-derived pool. num_blocks sits
	// at bytes 16..24 of the superblock - its stable on-disk ABI.
	let sb = disk.get(&FACTORY_START_SECTOR).expect("the format should write superblock slot 0");
	let num_blocks = u64::from_le_bytes(sb[16..24].try_into().unwrap());
	assert_eq!(num_blocks, expected_pool, "the pool should span everything past the archive region, derived from the reported capacity");

	// M75: the typed volume health/policy ops over the serve channel. Send a generated
	// request ([op u16][corr u32][args]) and pump block traffic until the reply lands.
	let mut request = |body: &[u8]| -> alloc::vec::Vec<u8> {
		_serve_client.send(Message::new(body.to_vec(), alloc::vec::Vec::new(), 0)).expect("the typed request should send");
		for _ in 0..100_000 {
			sched::run_until_idle();
			pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
			if let Ok(reply) = _serve_client.recv() {
				return reply.bytes;
			}
		}
		panic!("no typed reply arrived");
	};

	// status (op 12): the label is "system", the pool matches the derived size,
	// compression starts OFF, and the mount is read-write.
	let mut st = alloc::vec::Vec::new();
	st.extend_from_slice(&12u16.to_le_bytes());
	st.extend_from_slice(&1u32.to_le_bytes());
	let reply = request(&st);
	assert_eq!(reply[4], 1, "status should succeed");
	let label_len = u16::from_le_bytes([reply[5], reply[6]]) as usize;
	assert_eq!(&reply[7..7 + label_len], b"system", "the volume should carry its label");
	let total = u64::from_le_bytes(reply[7 + label_len..15 + label_len].try_into().unwrap());
	assert_eq!(total, expected_pool * 4096, "status reports the pool in bytes");
	let compression = reply[23 + label_len];
	let read_only = reply[24 + label_len];
	assert_eq!(compression, 0, "compression starts off by default");
	assert_eq!(read_only, 0, "the fresh volume mounts read-write");

	// set-compression on (op 13) flips the live volume; status reflects it.
	let mut sc = alloc::vec::Vec::new();
	sc.extend_from_slice(&13u16.to_le_bytes());
	sc.extend_from_slice(&2u32.to_le_bytes());
	sc.push(1);
	let reply = request(&sc);
	assert_eq!(reply[4], 1, "set-compression should succeed");
	let reply = request(&st);
	assert_eq!(reply[23 + label_len], 1, "compression should now be on");

	// fsck (op 14): a fresh volume verifies clean, with no damaged files named.
	let mut fs = alloc::vec::Vec::new();
	fs.extend_from_slice(&14u16.to_le_bytes());
	fs.extend_from_slice(&3u32.to_le_bytes());
	let reply = request(&fs);
	assert_eq!(reply[4], 1, "fsck should succeed");
	let failures = u32::from_le_bytes(reply[5..9].try_into().unwrap());
	let damaged = u16::from_le_bytes([reply[9], reply[10]]);
	assert_eq!((failures, damaged), (0, 0), "a fresh volume is clean");
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

#[test_case]
fn system_volume_lands_in_a_gpt_partition() {
	use alloc::collections::BTreeMap;
	use object::channel::Channel;
	use object::rights::Rights;

	// A disk partitioned by another system: a GPT whose entry array names a LiberFS
	// partition (the type GUID 4C424653-0001-4000-8000-4C6962657246) starting at LBA
	// 8192 - NOT the fixed factory layout's FACTORY_START_SECTOR. StorageService must
	// find the partition, format the volume INSIDE it, and size the pool to it.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	const PART_FIRST: u64 = 8192;
	const PART_BLOCKS: u64 = 4096; // 16 MB
	const PART_LAST: u64 = PART_FIRST + PART_BLOCKS * 8 - 1;

	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	// the GPT header at LBA 1: signature, entry-array LBA, entry count and size.
	let mut header = alloc::vec![0u8; 512];
	header[0..8].copy_from_slice(b"EFI PART");
	header[72..80].copy_from_slice(&2u64.to_le_bytes());
	header[80..84].copy_from_slice(&128u32.to_le_bytes());
	header[84..88].copy_from_slice(&128u32.to_le_bytes());
	disk.insert(1, header);
	// entry 0 at LBA 2: the LiberFS type GUID (on-disk byte order) and the span.
	let mut entries = alloc::vec![0u8; 512];
	entries[0..16].copy_from_slice(&[0x53, 0x46, 0x42, 0x4C, 0x01, 0x00, 0x00, 0x40, 0x80, 0x00, 0x4C, 0x69, 0x62, 0x65, 0x72, 0x46]);
	entries[32..40].copy_from_slice(&PART_FIRST.to_le_bytes());
	entries[40..48].copy_from_slice(&PART_LAST.to_le_bytes());
	disk.insert(2, entries);

	let (_volume, package) = scenario_packages().expect("boot modules should be present");
	let elf = package.lookup(b"storage_service").expect("storage_service should be in the init package");
	let (boot_kernel, boot_user) = Channel::create();
	let (blk_host, blk_child) = Channel::create();
	let (serve_server, _serve_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL, 0).expect("the StorageService should load");
	send_cap(&boot_kernel, b"BLOCK", blk_child, Rights::ALL).expect("the BLOCK handoff should send");
	send_cap(&boot_kernel, b"SERVE", serve_server, Rights::ALL).expect("the SERVE handoff should send");

	let mut online = false;
	'serve: for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(report) = boot_kernel.recv() {
			assert_eq!(&report.bytes[..], b"StorageService: online", "the service should come up on the GPT disk");
			online = true;
			break 'serve;
		}
	}
	assert!(online, "the service should format inside the partition and report in");

	// the superblock lands at the partition's first LBA, sized to the partition -
	// and nothing was written at the fixed factory-layout offset.
	let sb = disk.get(&PART_FIRST).expect("superblock slot 0 should sit at the partition start");
	assert_eq!(&sb[0..8], b"LIBERFS1", "the partition should carry a LiberFS superblock");
	let num_blocks = u64::from_le_bytes(sb[16..24].try_into().unwrap());
	assert_eq!(num_blocks, PART_BLOCKS, "the pool should span exactly the partition");
	assert!(disk.get(&FACTORY_START_SECTOR).is_none(), "the fixed factory offset must stay untouched on a GPT disk");
}

#[test_case]
fn a_degenerate_gpt_entry_cannot_kill_the_storage_service() {
	use alloc::collections::BTreeMap;
	use object::channel::Channel;
	use object::rights::Rights;

	// M79: the disk's content must never deny storage. A GPT names a LiberFS
	// partition too small to format (8 sectors - below even the superblock slots):
	// the probe must SKIP it and fall back to the fixed factory layout instead of
	// failing the format and exiting.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	let expected_pool: u64 = (CAPACITY - FACTORY_START_SECTOR * 512) / 4096;

	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	let mut header = alloc::vec![0u8; 512];
	header[0..8].copy_from_slice(b"EFI PART");
	header[72..80].copy_from_slice(&2u64.to_le_bytes());
	header[80..84].copy_from_slice(&128u32.to_le_bytes());
	header[84..88].copy_from_slice(&128u32.to_le_bytes());
	disk.insert(1, header);
	// a LiberFS-typed entry spanning 8 sectors: syntactically valid, unusably small.
	let mut entries = alloc::vec![0u8; 512];
	entries[0..16].copy_from_slice(&[0x53, 0x46, 0x42, 0x4C, 0x01, 0x00, 0x00, 0x40, 0x80, 0x00, 0x4C, 0x69, 0x62, 0x65, 0x72, 0x46]);
	entries[32..40].copy_from_slice(&100u64.to_le_bytes());
	entries[40..48].copy_from_slice(&107u64.to_le_bytes());
	disk.insert(2, entries);

	let (_volume, package) = scenario_packages().expect("boot modules should be present");
	let elf = package.lookup(b"storage_service").expect("storage_service should be in the init package");
	let (boot_kernel, boot_user) = Channel::create();
	let (blk_host, blk_child) = Channel::create();
	let (serve_server, _serve_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL, 0).expect("the StorageService should load");
	send_cap(&boot_kernel, b"BLOCK", blk_child, Rights::ALL).expect("the BLOCK handoff should send");
	send_cap(&boot_kernel, b"SERVE", serve_server, Rights::ALL).expect("the SERVE handoff should send");

	let mut online = false;
	'serve: for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(report) = boot_kernel.recv() {
			assert_eq!(&report.bytes[..], b"StorageService: online", "the service must survive the degenerate entry");
			online = true;
			break 'serve;
		}
	}
	assert!(online, "the service must fall back to the factory layout and report in");

	// the fallback formatted at the factory offset, sized by the disk's capacity.
	let sb = disk.get(&FACTORY_START_SECTOR).expect("the fallback should write superblock slot 0 at the factory offset");
	assert_eq!(&sb[0..8], b"LIBERFS1", "the factory layout should carry the volume");
	let num_blocks = u64::from_le_bytes(sb[16..24].try_into().unwrap());
	assert_eq!(num_blocks, expected_pool, "the pool should span the capacity-derived factory region");
}

#[test_case]
fn a_lying_seed_archive_cannot_kill_the_storage_service() {
	use alloc::collections::BTreeMap;
	use object::channel::Channel;
	use object::rights::Rights;

	// M83: the boot-time seeding path runs exactly on a disk WITHOUT a valid
	// filesystem - the least trustworthy disk there is. A PKGARCH1 header whose
	// entry count claims a ~137 GB table used to size the read buffer straight off
	// the disk's word; the claim must be bounded by the seed region and treated as
	// "no archive", so the service formats an empty volume and reports in.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	let expected_pool: u64 = (CAPACITY - FACTORY_START_SECTOR * 512) / 4096;

	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	let mut header = alloc::vec![0u8; 512];
	header[0..8].copy_from_slice(b"PKGARCH1");
	header[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
	disk.insert(0, header);

	let (_volume, package) = scenario_packages().expect("boot modules should be present");
	let elf = package.lookup(b"storage_service").expect("storage_service should be in the init package");
	let (boot_kernel, boot_user) = Channel::create();
	let (blk_host, blk_child) = Channel::create();
	let (serve_server, _serve_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), elf, boot_user, Rights::ALL, 0).expect("the StorageService should load");
	send_cap(&boot_kernel, b"BLOCK", blk_child, Rights::ALL).expect("the BLOCK handoff should send");
	send_cap(&boot_kernel, b"SERVE", serve_server, Rights::ALL).expect("the SERVE handoff should send");

	let mut online = false;
	'serve: for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(report) = boot_kernel.recv() {
			assert_eq!(&report.bytes[..], b"StorageService: online", "the service must survive the lying archive");
			online = true;
			break 'serve;
		}
	}
	assert!(online, "the service must treat the hostile claim as no archive and report in");

	// the volume formatted normally (empty - nothing was seeded from the "archive").
	let sb = disk.get(&FACTORY_START_SECTOR).expect("superblock slot 0 should sit at the factory offset");
	assert_eq!(&sb[0..8], b"LIBERFS1", "the factory layout should carry the volume");
	let num_blocks = u64::from_le_bytes(sb[16..24].try_into().unwrap());
	assert_eq!(num_blocks, expected_pool, "the pool should span the capacity-derived factory region");
}

#[test_case]
fn ps_live_view_drives_the_terminal_contract() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// `ps -i` (M63): the live process/resource view runs full-screen on its controlling
	// terminal - it must enter the alternate screen, hide the cursor and flip the tty
	// raw (the ESC[?1049h / ?25l / ?9001h private modes ConsoleService's terminal
	// honours), redraw a snapshot in place, quit on a raw `q` keystroke, and restore
	// every mode on the way out. Here we stand in for the terminal and both granted
	// services: the service channels answer garbage (so each query degrades to its
	// "unavailable" row - the terminal contract is what is under test), and a raw `q`
	// is queued so the first frame's key check quits the loop.
	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let ps_elf = program_elf(&package, volume, b"ps").expect("ps should be staged");

	let (boot_kernel, boot_user) = Channel::create();
	let (console_host, console_child) = Channel::create();
	let (res_host, res_child) = Channel::create();
	let (proc_host, proc_child) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), ps_elf, boot_user, Rights::ALL, 0).expect("ps should spawn");
	send_cap(&boot_kernel, b"STDOUT", console_child, Rights::ALL).expect("STDOUT bootstrap");
	boot_kernel.send(Message::new(b"-i".to_vec(), alloc::vec::Vec::new(), 0)).expect("argv bootstrap");
	send_cap(&boot_kernel, b"RESOURCE", res_child, Rights::ALL).expect("RESOURCE bootstrap");
	send_cap(&boot_kernel, b"PROCESS", proc_child, Rights::ALL).expect("PROCESS bootstrap");
	sched::run_until_idle();

	// the first frame queries the process list; answer garbage so it renders the
	// unavailable row, queue the quitting keystroke, then answer the budgets query.
	let _list_req = proc_host.recv().expect("the live view should query the process list");
	proc_host.send(Message::new(b"?".to_vec(), alloc::vec::Vec::new(), 0)).expect("the garbage list reply should send");
	console_host.send(Message::new(b"q".to_vec(), alloc::vec::Vec::new(), 0)).expect("the raw q keystroke should send");
	sched::run_until_idle();
	let _usage_req = res_host.recv().expect("the live view should query the budgets");
	res_host.send(Message::new(b"?".to_vec(), alloc::vec::Vec::new(), 0)).expect("the garbage usage reply should send");
	sched::run_until_idle();

	let mut captured = alloc::vec::Vec::new();
	while let Ok(msg) = console_host.recv() {
		captured.extend_from_slice(&msg.bytes);
	}
	let contains = |needle: &[u8]| captured.windows(needle.len()).any(|w| w == needle);
	assert!(contains(b"\x1b[?1049h\x1b[?25l\x1b[?9001h"), "the live view should enter the alternate screen, hide the cursor and flip the tty raw");
	assert!(contains(b"live process / resource view"), "the live view should render its header");
	assert!(contains(b"unavailable"), "the degraded queries should render their unavailable rows");
	assert!(contains(b"\x1b[?9001l") && contains(b"\x1b[?1049l"), "quitting on q should restore the tty and leave the alternate screen");
}

#[test_case]
fn system_manager_recovery_escalates_after_repeated_crashes() {
	use object::KernelObject;
	// The kernel supervises SystemManager: if it faults, the kernel starts a
	// recovery SystemManager, up to a limit, then escalates (it reboots in
	// production). Here the "SystemManager" faults on every attempt (a ring-3 page
	// fault), so supervision detects each crash via the crash-notify channel,
	// exhausts its restarts, and reports failure - the trigger for escalation.
	let (crash_tx, crash_rx) = object::channel::Channel::create();
	fault::set_crash_notify(crash_tx);
	let up = supervise(&crash_rx, 3, || {
		let thread = sched::spawn(user_fault_thread_body, 0);
		thread.process().header().koid()
	});
	fault::clear_crash_notify();
	assert!(!up, "a SystemManager that faults on every attempt must exhaust recovery and escalate");
}

#[test_case]
fn system_manager_recovery_survives_a_clean_start() {
	use object::KernelObject;
	// A SystemManager that does not fault must survive on the first attempt, so
	// supervision returns "up" without starting a recovery SystemManager.
	extern "C" fn clean_body(_arg: u64) {}
	let (crash_tx, crash_rx) = object::channel::Channel::create();
	fault::set_crash_notify(crash_tx);
	let up = supervise(&crash_rx, 3, || {
		let thread = sched::spawn(clean_body, 0);
		thread.process().header().koid()
	});
	fault::clear_crash_notify();
	assert!(up, "a SystemManager that does not fault should survive without recovery");
}

#[test_case]
fn a_clean_exit_releases_the_process_channel_endpoints() {
	use object::channel::Channel;
	use object::process::Process;
	use object::rights::Rights;

	// The shell's tool relay waits for the tool's stdout channel to CLOSE - and a
	// supervisor (the shell's job table, ps) legitimately holds the Process handle
	// long after the exit. A clean exit must therefore close the process's handle
	// table itself, exactly like the kill path does: the channel endpoints a dead
	// process held must not stay open until the LAST Process reference drops, or
	// every relay on a cleanly exiting child waits forever.
	let domain = sched::root_domain();
	let process = sched::process_create(domain).expect("the process should create");
	let (ours, theirs) = Channel::create();
	// park the peer endpoint in the child's handle table, standing in for a tool's
	// inherited stdout.
	process.install(theirs, Rights::ALL, 0);
	// the child's single thread exits cleanly at once.
	extern "C" fn clean_body(_arg: u64) {}
	let thread = sched::thread_create(process.clone(), clean_body, 0);
	sched::run_until_idle();
	drop(thread);
	// the process terminated cleanly...
	assert!(process.is_terminated(), "the process should have exited");
	// ...and even though we STILL HOLD a Process reference (the supervisor's view),
	// its endpoint is gone: the peer reads as closed, not merely quiet.
	assert!(ours.is_peer_closed(), "a clean exit must release the process's channel endpoints while a Process reference is still held");
	let _: &Process = &process;
}

#[test_case]
fn userspace_spawn_syscalls_start_a_second_process() {
	use core::sync::atomic::{AtomicU64, Ordering};
	// A kernel thread drives the userspace spawn syscalls exactly as a ring-3
	// spawner would: process_create -> process_load -> thread_create -> thread_start.
	// The image is the embedded LogService ELF, a leaf service that reports in over
	// its bootstrap channel and exits. The spawner hands the child the channel
	// endpoint it received as its own bootstrap (transferred through thread_create).
	static ELF_PTR: AtomicU64 = AtomicU64::new(0);
	static ELF_LEN: AtomicU64 = AtomicU64::new(0);
	extern "C" fn spawner(bootstrap: u64) {
		unsafe {
			let child = arch::syscall::invoke(syscall::SYS_PROCESS_CREATE, 0, 0, 0, 0);
			assert!((child as i64) > 0, "process_create");
			let entry = arch::syscall::invoke(syscall::SYS_PROCESS_LOAD, child, ELF_PTR.load(Ordering::SeqCst), ELF_LEN.load(Ordering::SeqCst), 0);
			assert!((entry as i64) > 0, "process_load");
			let thread = arch::syscall::invoke(syscall::SYS_THREAD_CREATE, child, entry, memlayout::USER_STACK_TOP, bootstrap);
			assert!((thread as i64) > 0, "thread_create");
			let started = arch::syscall::invoke(syscall::SYS_THREAD_START, thread, 0, 0, 0);
			assert_eq!(started as i64, 0, "thread_start");
		}
	}
	let bytes = init_package_bytes().expect("init package present");
	let package = pkg::Package::parse(bytes).expect("init package parses");
	let elf = package.lookup(b"log_service").expect("log_service image");
	ELF_PTR.store(elf.as_ptr() as u64, Ordering::SeqCst);
	ELF_LEN.store(elf.len() as u64, Ordering::SeqCst);
	let (kernel_ep, user_ep) = object::channel::Channel::create();
	sched::spawn_with_object(spawner, user_ep, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	let message = kernel_ep.recv().expect("the spawned process should report in over IPC");
	assert_eq!(&message.bytes[..], b"LogService: online");
}

#[test_case]
fn storage_serves_volume_file_to_client() {
	// The StorageService (a ring-3 process) maps a ramdisk volume, and a client
	// process opens vol://system/hello.txt through it, receives a shared-buffer
	// capability to the file's bytes, maps it, and reports the contents back. The
	// bytes the client read must equal the file straight from the volume archive -
	// an end-to-end, capability-brokered, zero-copy read across two userspace
	// processes coordinated only by IPC.
	let (expected, actual) = run_storage_scenario().expect("the storage scenario should run");
	assert!(!expected.is_empty(), "the volume file should not be empty");
	assert_eq!(actual, expected);
}

#[test_case]
fn wasi_host_runs_a_component() {
	// The wasi_host (a ring-3 process) loads an embedded Wasm component and runs it
	// on the `wasm` runtime. The component's only import, `liber.read`, is wired by
	// the host to read the one granted file (vol://system/hello.txt) through
	// StorageService into the component's linear memory - a WASI-style world: the
	// component has no other capability and can reach nothing it was not given. The
	// bytes the component read must equal the file straight from the volume, proving
	// a Wasm component performed a capability-gated operation via a host import
	// mapped to a native service.
	let (expected, actual) = run_wasi_scenario().expect("the wasi scenario should run");
	assert!(!expected.is_empty(), "the granted file should not be empty");
	assert_eq!(actual, expected, "the component read the granted file's bytes through the host import");
}

#[test_case]
fn powerbox_grants_a_picked_file_to_a_component() {
	// A Wasm component with NO filesystem access of its own runs under wasi_host,
	// which holds only a FilePicker client. The component's read import goes through
	// the picker, which (standing in for the user's choice) opens the chosen file
	// over StorageService and hands back exactly that file as a handle<file>
	// capability; the host reads it into the component's memory. The bytes must equal
	// the picked file straight from the volume - the component gained access to
	// exactly one user-picked file, and to nothing else (the powerbox pattern).
	let (expected, actual) = run_powerbox_scenario().expect("the powerbox scenario should run");
	assert!(!expected.is_empty(), "the picked file should not be empty");
	assert_eq!(actual, expected, "the component read the user-picked file through the picker");
}

#[test_case]
fn permission_manager_sandboxes_a_component() {
	// The PermissionManager governs four components under typed permission manifests. Two are
	// report-back probes. sandbox_probe is granted storage and log but not network: it starts
	// with only its manifest's capabilities - the manager transfers exactly the storage and
	// log clients to it and withholds the network one it holds, recording every decision - and
	// reads its one granted file through the storage capability, reporting the bytes back.
	// request_probe is granted only log and then asks for an undeclared capability (storage)
	// at runtime: the headless policy default refuses it (least privilege) and the manager
	// records that refusal as a dynamic decision. The other two are real system tools the
	// manager launches on demand through its `run` op, each printing to a captured stdout:
	// `date` (granted only time) reaches the wall clock through that one capability and prints
	// the rendered instant, and `cat` (granted only storage) prints its one file argument. The
	// probe's bytes must equal the file straight from the volume (the storage grant is live
	// and reaches exactly that file) and its summary must show storage and log granted and
	// every other capability denied; `date`'s output must be a well-formed ISO-8601 UTC instant
	// (the time grant is live) and its summary must show only time granted and every other
	// capability denied; request_probe's runtime request must be denied and its summary must
	// mark that refusal as dynamic - each component was given exactly its manifest and nothing
	// more. Finally `cat`'s output must equal that file (the storage grant reaches it through
	// the on-demand launcher).
	let (expected, probe_read, probe_summary, date_read, date_summary, request_read, request_summary, cat_read) = run_permission_scenario().expect("the permission scenario should run");
	assert!(!expected.is_empty(), "the granted file should not be empty");
	assert_eq!(probe_read, expected, "the sandboxed component read its one granted file through the storage grant");
	assert_eq!(probe_summary.as_slice(), b"storage=grant log=grant network=deny device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny", "sandbox_probe was granted exactly its manifest - storage and log - and denied every other capability in the vocabulary");
	// `date` reached its one granted capability: its output is a well-formed ISO-8601 UTC
	// instant "YYYY-MM-DDTHH:MM:SSZ" (the exact moment varies, so check the shape, not the
	// value - its presence proves the time grant is live).
	assert_eq!(date_read.len(), 20, "the date command rendered a 20-byte ISO-8601 UTC instant through its time grant");
	assert_eq!(date_read[4], b'-', "the date instant has a date separator after the year");
	assert_eq!(date_read[7], b'-', "the date instant has a date separator after the month");
	assert_eq!(date_read[10], b'T', "the date instant separates date and time with 'T'");
	assert_eq!(date_read[13], b':', "the date instant has a time separator after the hour");
	assert_eq!(date_read[16], b':', "the date instant has a time separator after the minute");
	assert_eq!(date_read[19], b'Z', "the date instant is UTC, terminated by 'Z'");
	assert_eq!(date_summary.as_slice(), b"storage=deny log=deny network=deny device=deny config=deny time=grant audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny", "date was granted exactly its manifest - time - and denied every other capability in the vocabulary");
	// request_probe asked for storage at runtime - a capability outside its manifest. The
	// headless policy default refused it, so the request comes back denied and its summary
	// carries the static grants followed by the refused runtime request marked `(dynamic)`.
	assert_eq!(request_read.as_slice(), b"storage denied", "request_probe's runtime request for an undeclared capability was refused by the headless policy default");
	assert_eq!(request_summary.as_slice(), b"storage=deny log=grant network=deny device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny storage=deny(dynamic)", "request_probe was granted exactly its manifest - log - and its runtime storage request was refused and recorded as a dynamic denial");
	// The on-demand `cat` tool, launched through PermissionManager's `run` op under a manifest
	// granting only storage, printed the file it was given through that grant to the stdout the
	// manager forwarded it: the bytes it rendered must equal the file straight from the volume.
	assert_eq!(cat_read, expected, "the cat tool printed its file argument through the storage grant the run launcher gave it, forwarded to the captured stdout");
}

#[test_case]
fn component_host_runs_an_sdk_component() {
	// component_host (a ring-3 process) loads a real Wasm component - built by the Rust
	// SDK and served from storage as vol://system/app.wasm, not embedded in the kernel
	// image - and runs it. Its three imports are resolved by name and wired to two
	// typed services with no ambient authority: `read` / `write` to StorageService,
	// `log` to LogService. The component reads its one granted file, upper-cases it,
	// logs the result through LogService, writes it back, and returns the count; the
	// host also calls the component's float `score` export. The bytes the component
	// produced must equal the upper-cased granted file (a real SDK component performed a
	// capability-gated filesystem read and transformed it on the interpreter), the log
	// grant must have been reached (the second typed service was wired - no ambient
	// authority), and score(10) must be 17 (the float path on genuine toolchain output).
	let (expected, content, logged, score) = run_component_scenario().expect("the component scenario should run");
	assert!(!expected.is_empty(), "the granted file should not be empty");
	assert_eq!(content, expected, "the component read, transformed, and returned its granted file's bytes through the host imports");
	assert!(logged, "the component reached its LogService grant - the second typed service was wired with no ambient authority");
	assert_eq!(score, 17, "the component's float `score` export computed floor(10 * 1.5 + 2.0) on real toolchain output");
}

#[test_case]
fn resource_manager_contains_a_domain() {
	// The ResourceManager creates a bounded sub-Domain, launches resource_probe into it, and
	// caps the Domain's memory at four one-page objects above the probe's baseline. It drives
	// the probe to fill the budget (four objects fit) and be refused the fifth - that
	// over-budget allocation fails with RESOURCE_EXHAUSTED, contained to the offending Domain
	// rather than crashing the probe (which survives and answers) or the system. The manager
	// then raises the cap by another four pages at runtime and drives the probe into the new
	// headroom (four more fit). The budget summary must show exactly that: four pages granted
	// under the cap, one contained refusal survived, and four pages regranted after the
	// runtime raise - the kernel enforced the per-Domain budget and the policy adjusted it
	// live.
	let summary = run_resource_scenario().expect("the resource scenario should run");
	assert_eq!(summary.as_slice(), b"granted=4 denied=1 regranted=4", "the kernel enforced the Domain's memory budget, contained the over-budget refusal, and honored the runtime raise");
}

#[test_case]
fn capability_grants_no_operation_beyond_rights() {
	// Property: a handle grants no operation beyond the rights it carries. Across many random
	// granted-rights sets and random probe rights, a rights-checked lookup succeeds exactly
	// when the probe is a subset of the granted set - never a superset. (Fixed-seed xorshift,
	// so the run is deterministic.)
	use object::handle::HandleTable;
	use object::rights::Rights;
	let mut seed: u64 = 0x5eed_1238_d38a_77c1;
	let mut next = || -> u64 {
		seed ^= seed << 13;
		seed ^= seed >> 7;
		seed ^= seed << 17;
		seed
	};
	let mut table = HandleTable::new();
	for _ in 0..512 {
		let granted = Rights::from_bits(next() as u32);
		let probe = Rights::from_bits(next() as u32);
		let h = table.insert_object(TestObject::new(1), granted, 0);
		assert_eq!(table.lookup(h, probe).is_ok(), granted.contains(probe), "a lookup must succeed iff the probe rights are a subset of the granted rights");
		table.close(h).expect("close");
	}
}

#[test_case]
fn capability_attenuation_only_narrows() {
	// Property: duplicating a capability can only narrow it, never widen it. Across many
	// random grants (carrying the DUPLICATE right) and random requests, duplication succeeds
	// exactly when the request is a subset of the grant, and the derived handle carries
	// exactly the requested rights - no right the original lacked, and none outside the
	// request. There is no path by which a derived capability gains authority.
	use object::handle::HandleTable;
	use object::rights::Rights;
	let mut seed: u64 = 0xabcd_0042_1357_9bdf;
	let mut next = || -> u64 {
		seed ^= seed << 13;
		seed ^= seed >> 7;
		seed ^= seed << 17;
		seed
	};
	let mut table = HandleTable::new();
	for _ in 0..512 {
		let granted = Rights::from_bits(next() as u32) | Rights::DUPLICATE;
		let requested = Rights::from_bits(next() as u32);
		let h = table.insert_object(TestObject::new(2), granted, 0);
		match table.duplicate(h, requested) {
			Ok(dup) => {
				// Duplication is allowed only when the request is within the grant...
				assert!(granted.contains(requested), "duplication widened the rights beyond the original");
				// ...and the derived handle carries exactly the requested rights, never more.
				let probe = Rights::from_bits(next() as u32);
				assert_eq!(table.lookup(dup, probe).is_ok(), requested.contains(probe), "the derived capability carries exactly the requested rights");
				table.close(dup).expect("close dup");
			}
			Err(_) => {
				// The grant carries DUPLICATE, so the only reason to refuse is that the request
				// asked for a right outside the grant - widening, which is forbidden.
				assert!(!granted.contains(requested), "duplication refused a request that was within the grant");
			}
		}
		table.close(h).expect("close");
	}
}

#[test_case]
fn no_ambient_authority_fresh_table_empty() {
	// A newly created handle table holds nothing: a process begins with no ambient authority
	// and can reach only capabilities explicitly handed to it. The table is empty, and every
	// lookup into it - across a wide range of handle values - is rejected as a bad handle.
	use object::handle::{Handle, HandleError, HandleTable};
	use object::rights::Rights;
	let table = HandleTable::new();
	assert_eq!(table.len(), 0, "a fresh handle table must be empty");
	let mut seed: u64 = 0x0f0f_1234_dead_c0de;
	let mut next = || -> u64 {
		seed ^= seed << 13;
		seed ^= seed >> 7;
		seed ^= seed << 17;
		seed
	};
	for _ in 0..256 {
		let handle = Handle::from_raw(next());
		assert!(matches!(table.lookup(handle, Rights::NONE), Err(HandleError::BadHandle)), "an empty table must resolve no handle");
	}
}

#[test_case]
fn syscall_fuzz_rejects_invalid_calls() {
	// Syscall fuzzing: from a ring-0 thread (with its own, empty handle table), drive the
	// syscall boundary with random unknown syscall numbers and random arguments, then known
	// handle syscalls with random (bogus) handle arguments. Every call must be rejected with
	// an error rather than crash the kernel - the boundary validates its inputs, and a caller
	// cannot reach authority it was never handed. The thread completing at all is itself the
	// survival check. (Fixed-seed xorshift, so the run is deterministic.)
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		let mut seed: u64 = 0x1357_2468_face_b00c;
		let mut next = || -> u64 {
			seed ^= seed << 13;
			seed ^= seed >> 7;
			seed ^= seed << 17;
			seed
		};
		unsafe {
			// Unknown syscall numbers (well above the defined range) must be rejected.
			for _ in 0..1024 {
				let num = 10_000 + (next() % 0x00ff_ffff);
				let r = arch::syscall::invoke(num, next(), next(), next(), next());
				assert!(syscall::sys_is_err(r), "an unknown syscall number must return an error");
			}
			// Known handle syscalls with random handle arguments. The fuzz thread's handle
			// table is empty, so every random handle resolves to nothing and is rejected
			// before any user buffer is touched - a bogus capability grants no authority.
			let ops = [syscall::SYS_HANDLE_CLOSE, syscall::SYS_HANDLE_DUPLICATE, syscall::SYS_MEMORY_MAP, syscall::SYS_MEMORY_UNMAP];
			for _ in 0..1024 {
				let op = ops[(next() as usize) % ops.len()];
				let r = arch::syscall::invoke(op, next(), next(), next(), next());
				assert!(syscall::sys_is_err(r), "a syscall on a bogus handle must return an error");
			}
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the syscall fuzz thread did not finish - the kernel did not survive");
}

#[test_case]
fn object_info_get_reports_object() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// object_info_get introspects a handle in the caller's table, so it runs inside
	// a spawned kernel thread (which has one). It reports the object's identity,
	// type, the rights the handle confers, and the object's byte size, and rejects
	// an unknown handle.
	extern "C" fn body(_arg: u64) {
		use object::ObjectType;
		use object::rights::Rights;
		unsafe {
			let handle = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			assert!(!syscall::sys_is_err(handle));
			let mut info = syscall::ObjectInfo { koid: 0, object_type: 0, rights: 0, generation: 0, size: 0 };
			let info_ptr = &mut info as *mut syscall::ObjectInfo as u64;
			let size = core::mem::size_of::<syscall::ObjectInfo>() as u64;
			let got = arch::syscall::invoke(syscall::SYS_OBJECT_INFO_GET, handle, info_ptr, size, 0);
			assert_eq!(got, 1);
			assert!(info.koid >= 1);
			assert_eq!(info.object_type, ObjectType::MemoryObject.code());
			assert_eq!(info.rights, Rights::ALL.bits());
			assert!(info.generation >= 1);
			assert_eq!(info.size, 4096, "a MemoryObject reports its real byte size");
			// an unknown handle is rejected with the bad-handle error
			let bad = arch::syscall::invoke(syscall::SYS_OBJECT_INFO_GET, 0xdead_beef, info_ptr, size, 0);
			assert_eq!(bad as i64, syscall::ERR_BAD_HANDLE);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

#[test_case]
fn system_graph_reflects_live_state() {
	use object::address_space::AddressSpace;
	use object::channel::Channel;
	use object::domain::Domain;
	use object::process::Process;
	use object::rights::Rights;
	use object::{KernelObject, ObjectType};
	// A standalone Domain with one process holding two handles. Collecting the
	// graph from that Domain must reflect the live structure exactly: one process
	// with two handles, one of them the channel we installed - with its koid, type,
	// rights, and badge intact. Dropping the process removes it from the graph.
	let domain = Domain::new(1 << 20, 16, 8);
	let process = Process::new(AddressSpace::kernel(), domain.clone());
	let (endpoint, _peer) = Channel::create();
	let channel_koid = endpoint.header().koid();
	process.install(endpoint, Rights::READ | Rights::WRITE, 42);
	process.install(object::event::Event::create(), Rights::ALL, 0);

	let node = graph::collect_from(&domain);
	assert_eq!(node.koid, domain.header().koid());
	assert_eq!(node.processes.len(), 1, "the Domain has one live process");
	let proc_node = &node.processes[0];
	assert_eq!(proc_node.koid, process.header().koid());
	assert_eq!(proc_node.handles.len(), 2, "the process holds two handles");
	let channel_handle = proc_node.handles.iter().find(|h| h.koid == channel_koid).expect("the channel handle should appear in the graph");
	assert_eq!(channel_handle.object_type, ObjectType::Channel);
	assert_eq!(channel_handle.rights, Rights::READ | Rights::WRITE);
	assert_eq!(channel_handle.badge, 42);

	// Dropping the process removes it from the live graph.
	drop(process);
	let after = graph::collect_from(&domain);
	assert_eq!(after.processes.len(), 0, "the process is gone after it drops");
}

#[test_case]
fn process_counters_track_ipc_and_resources() {
	use object::address_space::AddressSpace;
	use object::domain::Domain;
	use object::process::Process;
	use object::rights::Rights;
	// The per-process observability counters SYS_PROCESS_STATS_GET reads back: a fresh
	// process has done no IPC and holds nothing, recording sends and receives bumps the
	// IPC volume independently, installing handles grows the handle count, and a kill is
	// observable as the FAILED liveness the stats syscall derives.
	let domain = Domain::new(1 << 20, 16, 8);
	let process = Process::new(AddressSpace::kernel(), domain.clone());
	assert_eq!(process.messages_sent(), 0);
	assert_eq!(process.messages_received(), 0);
	assert_eq!(process.handle_count(), 0);
	assert_eq!(process.memory_bytes(), 0, "a kernel process owns no user frames");

	process.record_send();
	process.record_send();
	process.record_recv();
	assert_eq!(process.messages_sent(), 2, "two sends counted");
	assert_eq!(process.messages_received(), 1, "one recv counted");

	process.install(object::event::Event::create(), Rights::ALL, 0);
	process.install(object::event::Event::create(), Rights::ALL, 0);
	assert_eq!(process.handle_count(), 2, "two installed handles");

	// Liveness the stats syscall reports: not killed here, killed after terminate().
	assert!(!process.is_killed(), "a live process is not failed");
	process.terminate();
	assert!(process.is_killed(), "a terminated process reports as failed");
}

#[test_case]
fn kernel_reads_file_through_storage_service() {
	// The kernel drives the StorageService as its own client, sending one open request
	// and a quit sentinel, then reads the returned shared buffer. The bytes must equal
	// the file straight from the volume archive - a round-trip to a real userspace
	// service.
	let expected = pkg::Package::parse(volume_package_bytes().expect("the volume package should be present")).and_then(|p| p.lookup(b"hello.txt").map(|b| b.to_vec())).expect("hello.txt should be in the volume");
	let actual = storage_read(b"vol://system/hello.txt").expect("the storage read should succeed");
	assert!(!expected.is_empty(), "the volume file should not be empty");
	assert_eq!(actual, expected);
}

#[test_case]
fn storage_serves_staged_tool_binary() {
	// M61 box 7: the tool ELFs are staged onto the system volume under bin/ by the
	// factory-seed pipeline (build.rs strips them into the volume archive, the boot runner
	// lays that archive at LBA 0, and StorageService seeds it into the freshly-formatted
	// LiberFS). Reading one back through StorageService must return a valid ELF image -
	// proof the whole staging path works end to end.
	let actual = storage_read(b"vol://system/bin/cat").expect("the staged tool read should succeed");
	assert!(actual.len() > 4, "the staged tool should not be empty");
	assert_eq!(&actual[..4], b"\x7fELF", "the staged tool should be an ELF image");
}

#[test_case]
fn event_timer_objects() {
	use object::event::Event;
	use object::timer::Timer;
	let event = Event::create();
	assert!(!event.is_signaled());
	event.signal();
	assert!(event.is_signaled());
	event.clear();
	assert!(!event.is_signaled());

	let timer = Timer::create();
	// not armed -> never expired
	assert!(!timer.is_expired());
	let deadline = arch::apic::ticks() + 2;
	timer.set(deadline);
	// bounded wait for the tick counter to reach the deadline
	let mut spins = 0u64;
	while !timer.is_expired() {
		core::hint::spin_loop();
		spins += 1;
		assert!(spins < 2_000_000_000, "timer never expired");
	}
	assert!(timer.is_expired());
	timer.cancel();
	assert!(!timer.is_expired());
}

#[test_case]
fn event_timer_syscalls() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// Event and Timer driven through the syscall path need a current thread's
	// handle table, so they run inside a spawned kernel thread.
	extern "C" fn body(_arg: u64) {
		unsafe {
			let event = arch::syscall::invoke(syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
			assert!(!syscall::sys_is_err(event));
			assert_eq!(arch::syscall::invoke(syscall::SYS_EVENT_POLL, event, 0, 0, 0), 0);
			arch::syscall::invoke(syscall::SYS_EVENT_SIGNAL, event, 0, 0, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_EVENT_POLL, event, 0, 0, 0), 1);

			let timer = arch::syscall::invoke(syscall::SYS_TIMER_CREATE, 0, 0, 0, 0);
			assert!(!syscall::sys_is_err(timer));
			// not armed -> not expired
			assert_eq!(arch::syscall::invoke(syscall::SYS_TIMER_POLL, timer, 0, 0, 0), 0);
			// a deadline already reached reports expired immediately
			let now = arch::syscall::invoke(syscall::SYS_CLOCK_GET, 0, 0, 0, 0);
			arch::syscall::invoke(syscall::SYS_TIMER_SET, timer, now, 0, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_TIMER_POLL, timer, 0, 0, 0), 1);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

#[test_case]
fn userspace_runs_and_ipcs() {
	use object::channel::Channel;
	// Hand a fresh kernel thread one end of a channel and let it drop to ring 3
	// running the embedded user program. The program makes a capability-gated
	// channel send (a syscall from userspace) and exits; the kernel reads the
	// message back through the peer endpoint it kept.
	let (ep0, ep1) = Channel::create();
	sched::spawn_with_object(user_thread_body, ep0, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	let message = ep1.recv().expect("ring-3 program sent a message");
	assert_eq!(&message.bytes[..], b"OK");
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
	frame::deallocate(code);
	frame::deallocate(stack);
}

#[test_case]
fn userspace_yields_cooperatively() {
	use object::channel::Channel;
	// Two ring-3 threads share one core and each call SYS_YIELD several times
	// before sending "OK". The yields interleave them through the scheduler, which
	// only works if every syscall saves its user return state (rip/rsp/rflags) and
	// its kernel syscall stack per thread - a single per-CPU slot would be clobbered
	// by the sibling and one thread would return to the wrong context. Both messages
	// arriving proves the save path is per-thread.
	let (k0, u0) = Channel::create();
	let (k1, u1) = Channel::create();
	sched::spawn_with_object(user_yield_thread_body, u0, object::rights::Rights::ALL, 0);
	sched::spawn_with_object(user_yield_thread_body, u1, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert_eq!(&k0.recv().expect("first ring-3 thread sent a message").bytes[..], b"OK");
	assert_eq!(&k1.recv().expect("second ring-3 thread sent a message").bytes[..], b"OK");
}

#[test_case]
fn fault_isolation_kills_only_process() {
	use core::sync::atomic::Ordering;
	use object::domain::Domain;
	// A ring-3 thread dereferences a bad pointer. The kernel must terminate only
	// that process - not panic - and teardown must refund every resource it held to
	// its Domain. The thread runs in a bounded Domain and is not retained here, so
	// reaping it drops the Process and runs the refunds.
	let domain = Domain::new(1 << 20, 8, 4);
	sched::spawn_in(domain.clone(), user_fault_thread_body, 0).expect("spawn faulting thread");
	sched::run_until_idle();
	// Reaching here means the kernel survived the ring-3 fault and resumed
	// scheduling. The fault was recorded with the expected cause and address.
	assert_eq!(FAULT_GOT.load(Ordering::SeqCst), 1, "fault info should be recorded");
	assert_eq!(FAULT_KIND.load(Ordering::SeqCst), fault::FAULT_PAGE);
	assert_eq!(FAULT_ADDR.load(Ordering::SeqCst), arch::usermode::FAULT_PROBE_ADDR);
	// Teardown refunded the open MemoryObject (memory + handle) and the thread slot.
	assert_eq!(domain.account().memory().used(), 0, "memory refunded");
	assert_eq!(domain.account().handles().used(), 0, "handles refunded");
	assert_eq!(domain.account().threads().used(), 0, "thread slot refunded");
}

#[cfg(target_arch = "x86_64")]
#[test_case]
fn writable_pages_are_not_executable() {
	use core::sync::atomic::Ordering;
	use object::domain::Domain;
	// W^X: a ring-3 thread jumps into its own writable stack page. With EFER.NXE on
	// and the stack mapped NO_EXECUTE, the instruction FETCH page-faults (error code
	// bit 4) before a single stack byte executes, the kernel kills only that
	// process, and the recorded fault names the stack address it tried to run.
	assert!(arch::paging::nx_enabled(), "the test hardware supports NX");
	let domain = Domain::new(1 << 20, 8, 4);
	sched::spawn_in(domain.clone(), user_nx_thread_body, 0).expect("spawn nx probe thread");
	sched::run_until_idle();
	assert_eq!(NX_GOT.load(Ordering::SeqCst), 1, "fault info should be recorded");
	assert_eq!(NX_KIND.load(Ordering::SeqCst), fault::FAULT_PAGE);
	let addr = NX_ADDR.load(Ordering::SeqCst);
	assert!((USER_STACK_VA..USER_STACK_VA + mem::frame::PAGE_SIZE).contains(&addr), "the fault is inside the stack page");
	assert!(NX_CODE.load(Ordering::SeqCst) & 0x10 != 0, "the fault is an instruction fetch");
	assert_eq!(domain.account().threads().used(), 0, "thread slot refunded");
}

// The aarch64 counterpart: aarch64 has no x86 page-fault error code (the NX bit + the
// `& 0x10` fetch bit above are x86-specific). It encodes W^X with the UXN descriptor
// bit and reports the fault in ESR_EL1, so the same NX probe is checked through the
// aarch64 exception class instead.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn writable_pages_are_not_executable() {
	use core::sync::atomic::Ordering;
	use object::domain::Domain;
	// W^X on aarch64 (UXN): `map_page` sets UXN on a WRITABLE page, so a ring-3 thread
	// jumping into its own writable stack page takes an EL0 instruction abort on the
	// FETCH (before a stack byte runs); the kernel kills only that process and records
	// the stack address it tried to run and the faulting ESR.
	let domain = Domain::new(1 << 20, 8, 4);
	sched::spawn_in(domain.clone(), user_nx_thread_body, 0).expect("spawn nx probe thread");
	sched::run_until_idle();
	assert_eq!(NX_GOT.load(Ordering::SeqCst), 1, "fault info should be recorded");
	assert_eq!(NX_KIND.load(Ordering::SeqCst), fault::FAULT_PAGE);
	let addr = NX_ADDR.load(Ordering::SeqCst);
	assert!((USER_STACK_VA..USER_STACK_VA + mem::frame::PAGE_SIZE).contains(&addr), "the fault is inside the stack page");
	// The aarch64-specific angle: the recorded error_code is ESR_EL1, whose exception
	// class (bits 31:26) is 0x20 - an Instruction Abort from a lower EL (EL0), i.e. the
	// fault was an instruction fetch blocked by UXN, not a data access (which is 0x24).
	let ec = (NX_CODE.load(Ordering::SeqCst) >> 26) & 0x3f;
	assert_eq!(ec, 0x20, "the W^X fault is an EL0 instruction abort (UXN), not a data abort");
	assert_eq!(domain.account().threads().used(), 0, "thread slot refunded");
}

// The riscv64 counterpart: riscv has no x86 page-fault error code nor aarch64 ESR; a W^X
// fetch fault is just the scause exception code. On Sv39 a WRITABLE leaf leaves the X bit
// clear (map_page only sets X for an executable mapping), so the same NX probe is checked
// through scause instead.
#[cfg(target_arch = "riscv64")]
#[test_case]
fn writable_pages_are_not_executable() {
	use core::sync::atomic::Ordering;
	use object::domain::Domain;
	// W^X on riscv (Sv39): a WRITABLE leaf PTE has its X bit clear, so a U-mode thread
	// jumping into its own writable stack page takes an instruction page fault on the
	// FETCH (before a stack byte runs); the kernel kills only that process and records the
	// stack address it tried to run.
	let domain = Domain::new(1 << 20, 8, 4);
	sched::spawn_in(domain.clone(), user_nx_thread_body, 0).expect("spawn nx probe thread");
	sched::run_until_idle();
	assert_eq!(NX_GOT.load(Ordering::SeqCst), 1, "fault info should be recorded");
	assert_eq!(NX_KIND.load(Ordering::SeqCst), fault::FAULT_PAGE);
	let addr = NX_ADDR.load(Ordering::SeqCst);
	assert!((USER_STACK_VA..USER_STACK_VA + mem::frame::PAGE_SIZE).contains(&addr), "the fault is inside the stack page");
	// The riscv-specific angle: the recorded error_code is scause, which is 12 - an
	// instruction page fault (the fetch blocked by the clear X bit), not a load (13) or
	// store (15) page fault.
	assert_eq!(NX_CODE.load(Ordering::SeqCst), 12, "the W^X fault is an instruction page fault (scause 12)");
	assert_eq!(domain.account().threads().used(), 0, "thread slot refunded");
}

#[cfg(target_arch = "x86_64")]
#[test_case]
fn kernel_access_to_user_memory_is_refused_outside_the_window() {
	use mem::frame;
	// SMAP/SMEP: a kernel dereference of a USER-mapped page outside the sanctioned
	// user_access window must page-fault (SMAP), and a ring-0 jump into a
	// USER-mapped page must page-fault as an instruction fetch (SMEP) - a kernel
	// bug can neither silently read user memory nor execute it. Each probe runs in
	// its own kernel thread; the armed page-fault handler recognizes the expected
	// fault and retires the thread instead of halting the machine. The probe VA is
	// clear of every other test's user pages.
	const SMAP_PROBE_VA: u64 = 0x0000_0000_4100_0000;
	assert!(arch::paging::smap_enabled(), "the test hardware supports SMAP");
	assert!(arch::paging::smep_enabled(), "the test hardware supports SMEP");
	let frame = frame::allocate().expect("probe frame");
	// Stamp a marker through the HHDM so a silent (unrefused) read would be visible.
	unsafe { ((mem::hhdm_offset() + frame) as *mut u64).write_volatile(0x5341_4645) };
	// Map it USER (no NX: the SMEP probe below fetches from it; SMAP alone must
	// refuse the data read regardless of NX).
	arch::paging::map_page(SMAP_PROBE_VA, frame, arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER);
	// Probe 1 (SMAP): a plain kernel read of the user page. Only Copy values live
	// across the faulting access - the handler retires this thread mid-statement.
	extern "C" fn smap_probe(_arg: u64) {
		fault::arm_smap_probe(SMAP_PROBE_VA);
		let value = unsafe { (SMAP_PROBE_VA as *const u64).read_volatile() };
		// Reached only if SMAP failed to refuse the access.
		panic!("SMAP did not refuse a kernel read of user memory (read {:#x})", value);
	}
	sched::spawn(smap_probe, 0);
	sched::run_until_idle();
	let code = fault::smap_probe_hit().expect("the kernel read of user memory faulted");
	assert!(code & 0x1 != 0, "the SMAP refusal is a protection fault on a present page");
	assert!(code & 0x10 == 0, "the SMAP refusal is a data access, not a fetch");
	// The sanctioned window still reads it fine - the copy paths keep working.
	let through_window = arch::paging::user_access(|| unsafe { (SMAP_PROBE_VA as *const u64).read_volatile() });
	assert_eq!(through_window, 0x5341_4645, "the sanctioned user_access window reads the page");
	// Probe 2 (SMEP): a ring-0 jump into the user page. The fetch faults before a
	// single byte executes, so the page's content never matters.
	extern "C" fn smep_probe(_arg: u64) {
		fault::arm_smap_probe(SMAP_PROBE_VA);
		let target: extern "C" fn() = unsafe { core::mem::transmute::<u64, extern "C" fn()>(SMAP_PROBE_VA) };
		target();
		panic!("SMEP did not refuse a kernel jump into user memory");
	}
	sched::spawn(smep_probe, 0);
	sched::run_until_idle();
	let code = fault::smap_probe_hit().expect("the kernel jump into user memory faulted");
	assert!(code & 0x10 != 0, "the SMEP refusal is an instruction fetch");
	// The probe threads died mid-body: clean their mapping up here.
	arch::paging::unmap_page(SMAP_PROBE_VA);
	frame::deallocate(frame);
}

#[test_case]
fn a_user_stack_grows_on_demand_past_its_initial_pages() {
	use core::sync::atomic::Ordering;
	use object::domain::Domain;
	// Demand-paged stacks: nothing below USER_STACK_TOP is mapped up front for this
	// probe, and the Domain's default ceiling is megabytes. The probe touches 100
	// pages (400 kB - past the old eagerly-mapped 256 kB, let alone the 8-page
	// initial mapping) walking down; every touch page-faults, the handler maps the
	// missing page and the instruction resumes, and the probe reaches its clean
	// exit. The Domain's stack account holds exactly the grown bytes while the
	// process lives and refunds them when it is reaped.
	let domain = Domain::new(1 << 22, 8, 4);
	sched::spawn_in(domain.clone(), user_stack_probe_thread_body, 100).expect("spawn stack probe");
	sched::run_until_idle();
	assert_eq!(STACK_GOT.load(Ordering::SeqCst), 0, "a grown stack records no fault");
	assert_eq!(STACK_USED.load(Ordering::SeqCst), 100 * mem::frame::PAGE_SIZE, "the stack account holds the grown pages");
	assert_eq!(domain.account().stack().used(), 0, "the stack bytes are refunded at teardown");
	assert_eq!(domain.account().threads().used(), 0, "thread slot refunded");
}

#[test_case]
fn recursion_past_the_stack_floor_is_killed() {
	use core::sync::atomic::Ordering;
	use mem::frame::PAGE_SIZE;
	use object::domain::Domain;
	// The hard floor: the Domain's stack ceiling is squeezed to 16 pages, so the
	// probe's 15 touches above the one-page guard grow, and the 16th - the guard
	// page itself - is a genuine fault that kills the process (runaway recursion
	// dies instead of eating the machine).
	let domain = Domain::new(1 << 22, 8, 4);
	domain.account().stack().set_limit(16 * PAGE_SIZE);
	sched::spawn_in(domain.clone(), user_stack_probe_thread_body, 32).expect("spawn stack probe");
	sched::run_until_idle();
	assert_eq!(STACK_GOT.load(Ordering::SeqCst), 1, "overrunning the floor records a fault");
	assert_eq!(STACK_KIND.load(Ordering::SeqCst), fault::FAULT_PAGE);
	assert_eq!(STACK_ADDR.load(Ordering::SeqCst), memlayout::USER_STACK_TOP - 16 * PAGE_SIZE, "the kill lands on the guard page at the floor");
	assert_eq!(STACK_USED.load(Ordering::SeqCst), 15 * PAGE_SIZE, "only the pages above the guard grew");
	assert_eq!(domain.account().stack().used(), 0, "the stack bytes are refunded at teardown");
	assert_eq!(domain.account().threads().used(), 0, "thread slot refunded");
}

#[cfg(target_arch = "x86_64")]
#[test_case]
fn driver_crash_is_cleaned_up_and_notified() {
	use object::KernelObject;
	use object::domain::Domain;
	// A "driver" process binds an IRQ and creates a DMA buffer, then faults. The
	// kernel must detach the IRQ, refund the DMA, remove the caps, and deliver a
	// crash record naming the process - all without cooperation from the driver.
	let (notify_tx, notify_rx) = object::channel::Channel::create();
	fault::set_crash_notify(notify_tx);
	let domain = Domain::new(1 << 20, 8, 4);
	let koid = {
		let driver = sched::spawn_in(domain.clone(), driver_crash_thread_body, 0).expect("spawn driver");
		// Capture the process identity, then drop the Arc so reaping the thread can
		// tear the process down and run the crash cleanup.
		driver.process().header().koid()
	};
	sched::run_until_idle();
	// The IRQ binding is gone, and the DMA and handle quotas are back to zero: the
	// crashed driver's resources were reclaimed by the kernel.
	assert!(!arch::interrupts::is_bound(DRIVER_IRQ_VECTOR as u8), "the driver's IRQ should be detached");
	assert_eq!(domain.account().dma().used(), 0, "the driver's DMA should be refunded");
	assert_eq!(domain.account().handles().used(), 0, "the driver's handles should be removed");
	// A crash record naming the driver process was delivered to the supervisor.
	let record = notify_rx.recv().expect("a crash notification should be delivered");
	assert_eq!(record.bytes.len(), 16, "crash record is koid + kind");
	let got_koid = u64::from_le_bytes(record.bytes[0..8].try_into().unwrap());
	let got_kind = u64::from_le_bytes(record.bytes[8..16].try_into().unwrap());
	assert_eq!(got_koid, koid, "crash record names the crashed process");
	assert_eq!(got_kind, fault::FAULT_PAGE, "crash record carries the fault kind");
	fault::clear_crash_notify();
}

#[cfg(target_arch = "x86_64")]
#[test_case]
fn device_manager_reacts_to_a_driver_crash() {
	use object::KernelObject;
	use object::domain::Domain;
	// DeviceManager's reaction to a driver crash: the kernel reports the crash on the
	// crash-notify channel (M20h), and the supervisor finds the device that driver
	// was bound to and marks it offline. Here device 0 is driven by a process that
	// then crashes; consuming the crash event, the supervisor marks it offline.
	#[derive(PartialEq, Debug)]
	enum DeviceState {
		Online,
		Offline,
	}
	let (notify_tx, notify_rx) = object::channel::Channel::create();
	fault::set_crash_notify(notify_tx);
	let mut device0 = DeviceState::Online;
	let domain = Domain::new(1 << 20, 8, 4);
	let driver_koid = {
		let driver = sched::spawn_in(domain.clone(), driver_crash_thread_body, 0).expect("spawn driver");
		driver.process().header().koid()
	};
	sched::run_until_idle();
	// react: the crash event names the crashed process; if it is our device's driver,
	// mark the device offline.
	let record = notify_rx.recv().expect("a crash event should be delivered");
	let crashed_koid = u64::from_le_bytes(record.bytes[0..8].try_into().unwrap());
	if crashed_koid == driver_koid {
		device0 = DeviceState::Offline;
	}
	fault::clear_crash_notify();
	assert_eq!(device0, DeviceState::Offline, "DeviceManager should mark a crashed driver's device offline");
}

#[test_case]
fn driver_survives_crash_and_restart() {
	use object::KernelObject;
	// The driver crash/restart cycle: a driver that faults is respawned by its
	// supervisor, and the restarted driver runs cleanly. The supervisor spawns the
	// driver, detects the fault on the crash-notify channel, and respawns it until an
	// attempt survives - the loop DeviceManager runs over a driver's bootstrap channel
	// (a crash there peer-closes it) and the kernel runs to recover SystemManager.
	extern "C" fn clean_driver(_arg: u64) {}
	let (crash_tx, crash_rx) = object::channel::Channel::create();
	fault::set_crash_notify(crash_tx);
	let mut restarts: u32 = 0;
	let mut survived = false;
	for attempt in 0..4u32 {
		// the first start faults; each restart runs the clean driver.
		let body: extern "C" fn(u64) = if attempt == 0 { user_fault_thread_body } else { clean_driver };
		let koid = {
			let driver = sched::spawn(body, 0);
			driver.process().header().koid()
		};
		sched::run_until_idle();
		if crash_seen(&crash_rx, koid) {
			restarts += 1;
			continue;
		}
		survived = true;
		break;
	}
	fault::clear_crash_notify();
	assert!(survived, "the restarted driver should run without faulting");
	assert!(restarts >= 1, "the supervisor should have restarted the crashed driver");
}

#[test_case]
fn domain_quota_enforced_cleanly() {
	use core::sync::atomic::{AtomicBool, Ordering};
	use object::domain::Domain;
	static DONE: AtomicBool = AtomicBool::new(false);
	// A thread accounted to a bounded Domain exercises the create-boundary
	// quotas. Reaching a cap must return ERR_RESOURCE_EXHAUSTED, not crash. The
	// create syscalls charge the current thread's Domain, so the sequence runs
	// inside a spawned thread; a failed assertion panics it and fails the run.
	extern "C" fn body(_arg: u64) {
		unsafe {
			// memory: the cap is 8192 bytes = two pages. Two objects fit exactly,
			// the third is refused cleanly without allocating anything.
			let m0 = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			assert!(!syscall::sys_is_err(m0));
			let m1 = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			assert!(!syscall::sys_is_err(m1));
			let m2 = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			assert_eq!(m2 as i64, syscall::ERR_RESOURCE_EXHAUSTED);
			// closing the two objects refunds their memory and their handles
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, m0, 0, 0, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, m1, 0, 0, 0) as i64, 0);
			// handles: the cap is 4. Four events fit, the fifth is refused cleanly.
			for _ in 0..4 {
				let e = arch::syscall::invoke(syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
				assert!(!syscall::sys_is_err(e));
			}
			let over = arch::syscall::invoke(syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
			assert_eq!(over as i64, syscall::ERR_RESOURCE_EXHAUSTED);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	// 8192 bytes of memory (two pages), 4 handles, 4 threads.
	let domain = Domain::new(8192, 4, 4);
	// Do not keep the returned Arc, so the thread is free to be reaped (and its
	// charges refunded) once it exits.
	assert!(sched::spawn_in(domain.clone(), body, 0).is_some());
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	// Tearing the thread down returned every resource: the four still-open events
	// are refunded by the handle table's drop and the thread slot by the thread's
	// drop, so the bounded Domain is back to zero - clean refusal, no leak.
	assert_eq!(domain.account().memory().used(), 0);
	assert_eq!(domain.account().handles().used(), 0);
	assert_eq!(domain.account().threads().used(), 0);
}

#[test_case]
fn dma_buffer_quota_enforced_cleanly() {
	use core::sync::atomic::{AtomicBool, Ordering};
	use object::domain::{Domain, UNLIMITED};
	static DONE: AtomicBool = AtomicBool::new(false);
	// A thread accounted to a Domain capped at two pages of pinned DMA. The
	// dma_buffer_create syscall charges the DMA quota at the create boundary, so a
	// third buffer must be refused cleanly (ERR_RESOURCE_EXHAUSTED, nothing
	// allocated) and closing the buffers must refund the quota.
	extern "C" fn body(_arg: u64) {
		unsafe {
			let d0 = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_CREATE, 4096, 0, 0, 0);
			assert!(!syscall::sys_is_err(d0));
			let d1 = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_CREATE, 4096, 0, 0, 0);
			assert!(!syscall::sys_is_err(d1));
			let d2 = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_CREATE, 4096, 0, 0, 0);
			assert_eq!(d2 as i64, syscall::ERR_RESOURCE_EXHAUSTED);
			// Closing the buffers refunds both their DMA quota and their handles.
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, d0, 0, 0, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, d1, 0, 0, 0) as i64, 0);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	let domain = Domain::new(UNLIMITED, UNLIMITED, UNLIMITED);
	domain.account().dma().set_limit(2 * 4096);
	assert!(sched::spawn_in(domain.clone(), body, 0).is_some());
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "DMA quota test thread did not finish");
	// Every buffer was closed, so the pinned-DMA quota is back to zero.
	assert_eq!(domain.account().dma().used(), 0);
}

#[test_case]
fn ipc_queue_bytes_accounting_enforced() {
	use core::sync::atomic::{AtomicBool, Ordering};
	use object::domain::{Domain, UNLIMITED};
	static DONE: AtomicBool = AtomicBool::new(false);
	// A thread in a Domain capped at 250 bytes of in-transit IPC. Each 100-byte
	// send charges the sender's queue, so two fit and the third is refused
	// (WOULD_BLOCK); receiving one refunds the quota, so a send fits again.
	extern "C" fn body(_arg: u64) {
		unsafe {
			let mut handles = [0u64; 2];
			let created = arch::syscall::invoke(syscall::SYS_CHANNEL_CREATE, handles.as_mut_ptr() as u64, handles.as_mut_ptr().add(1) as u64, 0, 0);
			assert_eq!(created as i64, 0, "channel create failed");
			let (h0, h1) = (handles[0], handles[1]);
			let payload = [0u8; 100];
			let p = payload.as_ptr() as u64;
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, h0, p, 100, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, h0, p, 100, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, h0, p, 100, 0) as i64, syscall::ERR_WOULD_BLOCK, "the third send should hit the queue cap");
			// Receiving one message refunds the sender's queue, so a send fits again.
			let mut buf = [0u8; 128];
			let n = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, h1, buf.as_mut_ptr() as u64, 128, 0);
			assert_eq!(n as i64, 100);
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, h0, p, 100, 0) as i64, 0, "a send fits after a recv refund");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	let domain = Domain::new(UNLIMITED, UNLIMITED, UNLIMITED);
	domain.account().ipc_queue().set_limit(250);
	assert!(sched::spawn_in(domain.clone(), body, 0).is_some());
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "ipc queue test thread did not finish");
	// The thread and its channels are reaped, refunding every undelivered message.
	assert_eq!(domain.account().ipc_queue().used(), 0);
}

#[test_case]
fn domain_hierarchy_limits_aggregate() {
	use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};
	use object::domain::{Domain, UNLIMITED};
	static DONE: AtomicBool = AtomicBool::new(false);
	static THIRD: AtomicI64 = AtomicI64::new(0);
	// A child Domain's charges also count against its parent, and the parent's
	// aggregate limit binds even when the child itself is unbounded. The parent
	// caps memory at two pages; the unbounded child may charge two pages but not a
	// third. Part one checks the Domain API directly (deterministic, no thread).
	let parent = Domain::new(8192, UNLIMITED, UNLIMITED);
	let child = Domain::new_child(&parent, UNLIMITED, UNLIMITED, UNLIMITED);
	assert!(child.try_charge_memory(4096));
	assert_eq!(parent.account().memory().used(), 4096, "charge propagates to the parent");
	assert!(child.try_charge_memory(4096)); // parent now full at two pages
	assert!(!child.try_charge_memory(4096), "parent aggregate binds though the child is unbounded");
	assert_eq!(child.account().memory().used(), 8192, "the refused charge was rolled back at the child");
	assert_eq!(parent.account().memory().used(), 8192, "and left the parent unchanged");
	child.uncharge_memory(8192);
	assert_eq!(parent.account().memory().used(), 0, "uncharge propagates to the parent");
	// Part two checks the same limit through the create syscall: a process in the
	// unbounded child is refused the third page because the parent caps memory at
	// two. It records the third result and exits; teardown refunds the rest.
	extern "C" fn body(_arg: u64) {
		unsafe {
			assert!(!syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0)));
			assert!(!syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0)));
			THIRD.store(arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0) as i64, Ordering::SeqCst);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	assert!(sched::spawn_in(child.clone(), body, 0).is_some());
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	assert_eq!(THIRD.load(Ordering::SeqCst), syscall::ERR_RESOURCE_EXHAUSTED, "parent limit binds through the syscall path");
	// The body exited without closing its objects; teardown refunds them.
	assert_eq!(child.account().memory().used(), 0, "child memory refunded");
	assert_eq!(parent.account().memory().used(), 0, "parent aggregate memory refunded");
}

#[test_case]
fn domain_kill_frees_subtree() {
	use core::sync::atomic::{AtomicI64, Ordering};
	use object::domain::Domain;
	use object::rights::Rights;
	static KILL_RET: AtomicI64 = AtomicI64::new(-100);
	// Build a Domain subtree parent -> child, run two parked processes under the
	// child that each hold a MemoryObject, then kill the PARENT through the real
	// domain_kill syscall. The whole subtree must be torn down: the parkers'
	// resources refunded and their threads reaped, leaving both Domains' accounts
	// at zero. Killing a parent thus terminates every descendant process.
	let parent = Domain::new(1 << 20, 16, 8);
	let child = Domain::new_child(&parent, 1 << 20, 16, 8);
	// The killer runs in the root Domain (so it is not itself killed); it is
	// seeded with a handle to the parent Domain and kills it.
	extern "C" fn killer(domain_handle: u64) {
		let ret = unsafe { arch::syscall::invoke(syscall::SYS_DOMAIN_KILL, domain_handle, 0, 0, 0) };
		KILL_RET.store(ret as i64, Ordering::SeqCst);
	}
	// Spawn the parkers before the killer so they run first: they create their
	// objects and park, and only then does the killer tear the subtree down.
	sched::spawn_in(child.clone(), domain_parker, 0).expect("spawn parker 0");
	sched::spawn_in(child.clone(), domain_parker, 0).expect("spawn parker 1");
	sched::spawn_with_object(killer, parent.clone(), Rights::MANAGE, 0);
	sched::run_until_idle();
	// The kill syscall succeeded and the subtree was fully reclaimed: the killed
	// processes' handles (and the memory those objects pinned) were freed eagerly,
	// and the parked threads self-terminated and were reaped.
	assert_eq!(KILL_RET.load(Ordering::SeqCst), 0, "domain_kill returned ok");
	assert_eq!(child.account().memory().used(), 0, "child memory refunded");
	assert_eq!(child.account().handles().used(), 0, "child handles refunded");
	assert_eq!(child.account().threads().used(), 0, "child threads refunded");
	assert_eq!(parent.account().memory().used(), 0, "parent aggregate memory refunded");
	assert_eq!(parent.account().handles().used(), 0, "parent aggregate handles refunded");
	assert_eq!(parent.account().threads().used(), 0, "parent aggregate threads refunded");
}

#[test_case]
fn ipc_round_trip_and_zero_copy() {
	use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
	use object::channel::{Channel, Message};

	// Round-trip correctness: a request and a reply each deliver their exact
	// bytes through the channel primitive (the path the latency benchmark times).
	let (client, server) = Channel::create();
	client.send(Message::new(alloc::vec::Vec::from(*b"req"), alloc::vec::Vec::new(), 0)).unwrap();
	let request = server.recv().unwrap();
	assert_eq!(&request.bytes[..], b"req");
	server.send(Message::new(alloc::vec::Vec::from(*b"reply"), alloc::vec::Vec::new(), 0)).unwrap();
	let reply = client.recv().unwrap();
	assert_eq!(&reply.bytes[..], b"reply");

	// Zero-copy: a 1 MB buffer is transferred as a capability, not copied. The
	// producer marks the far end of the buffer and sends only a 3-byte note plus
	// the handle; the consumer maps the same object and reads the mark back. That
	// the far-end mark survives while only 3 bytes crossed the channel proves the
	// pages were shared, not copied. Runs in a thread (syscalls need a handle table).
	static DONE: AtomicBool = AtomicBool::new(false);
	static MARKER: AtomicU64 = AtomicU64::new(0);
	static NOTE_LEN: AtomicU64 = AtomicU64::new(0);
	extern "C" fn body(_arg: u64) {
		const BUF_LEN: u64 = 0x10_0000; // 1 MB
		const MARK: u64 = 0xa5a5_0000_5a5a_1111;
		unsafe {
			let mut client: u64 = 0;
			let mut server: u64 = 0;
			let created = arch::syscall::invoke(syscall::SYS_CHANNEL_CREATE, &mut client as *mut u64 as u64, &mut server as *mut u64 as u64, 0, 0);
			assert!(!syscall::sys_is_err(created));
			// produce: mark the last 8 bytes of a 1 MB object, then unmap it
			let mo = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, BUF_LEN, 0, 0, 0);
			assert!(!syscall::sys_is_err(mo));
			let virt = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, mo, 0, 0, 0);
			assert!(!syscall::sys_is_err(virt));
			((virt + BUF_LEN - 8) as *mut u64).write_volatile(MARK);
			arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, mo, 0, 0, 0);
			// transfer the capability with a tiny note instead of the buffer bytes
			let note = *b"BIG";
			let sent = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, client, note.as_ptr() as u64, note.len() as u64, mo);
			assert!(!syscall::sys_is_err(sent));
			// consume: receive the note + handle, map the object, read the far mark
			let mut buf = [0u8; 8];
			let mut xfer: u64 = 0;
			let n = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, server, buf.as_mut_ptr() as u64, buf.len() as u64, &mut xfer as *mut u64 as u64);
			assert!(!syscall::sys_is_err(n));
			NOTE_LEN.store(n as u64, Ordering::SeqCst);
			assert_ne!(xfer, 0);
			let virt2 = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, xfer, 0, 0, 0);
			assert!(!syscall::sys_is_err(virt2));
			MARKER.store(((virt2 + BUF_LEN - 8) as *const u64).read_volatile(), Ordering::SeqCst);
			arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, xfer, 0, 0, 0);
			arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, xfer, 0, 0, 0);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	// the far-end mark came through intact, and only the 3-byte note crossed the
	// channel: the 1 MB buffer was shared by capability, never copied.
	assert_eq!(MARKER.load(Ordering::SeqCst), 0xa5a5_0000_5a5a_1111);
	assert_eq!(NOTE_LEN.load(Ordering::SeqCst), 3);
}
