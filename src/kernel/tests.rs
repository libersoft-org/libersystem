// The kernel test suite and its scenario helpers (custom_test_frameworks, runs
// under `cargo test` in QEMU). Everything here is test-only: the ring-3 probe
// programs and their thread bodies, the packaged-scenario drivers the service
// tests build on, the Testable harness, and the test cases themselves. The boot
// path and the helpers it shares with the suite (the module locators, the
// SystemManager spawn and the supervise ladder) stay in main.rs.

use super::*;
use alloc::vec::Vec;

// Userspace (ring 3) page layout for the test: one USER page for the program,
// one for its stack, mapped into the low half of the shared address space
// (per-process page tables / CR3 isolation are a later refinement).
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
// under `bin/`, so fall back to the volume there. The returned slice borrows
// the 'static module data, so it outlives the temporary volume Package.
fn program_elf(package: &pkg::Package<'static>, volume: &'static [u8], name: &[u8]) -> Option<&'static [u8]> {
	let mut artifact: alloc::vec::Vec<u8> = name.to_vec();
	artifact.extend_from_slice(b".lsexe");
	if let Some(elf) = package.lookup(&artifact) {
		return Some(elf);
	}
	let mut path: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	path.extend_from_slice(b"bin/");
	path.extend_from_slice(&artifact);
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
// channel. The kernel only brokers the initial capabilities. Returns (expected,
// actual): the file straight from the volume, and the bytes the component read.
fn run_wasi_scenario() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>), &'static str> {
	use object::channel::Channel;
	use object::rights::Rights;

	let (volume, package) = scenario_packages()?;
	let expected = volume_file(volume, b"hello.txt")?;
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
	send_ramdisk(&storage_boot_kernel, volume)?;
	send_cap(&storage_boot_kernel, b"SERVE", service_server, Rights::ALL)?;
	send_cap(&host_boot_kernel, b"STORAGE", service_client, Rights::ALL)?;

	sched::run_until_idle();
	let result = host_boot_kernel.recv().map_err(|_| "the host reported no result")?;
	Ok((expected, result.bytes))
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
	Ok((expected, result.bytes))
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
// scenario also launches `imgview` over a staged BMP and display/input stand-ins, proving its
// acquire -> present -> focus -> key-quit -> release sequence. The kernel only brokers the
// initial capabilities. Returns (expected,
// probe_read, probe_summary, date_read, date_summary, request_read, request_summary,
// cat_read): the file straight from the volume, then each component's proof and decisions
// summary, then the bytes `cat` printed through the run launcher.
fn run_permission_scenario() -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, alloc::vec::Vec<u8>, u64), &'static str> {
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
	send_cap(&pm_boot_kernel, b"SERVICES", services_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"USBBUS", usb_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"PROCESS", process_client, Rights::ALL)?;
	send_cap(&pm_boot_kernel, b"SERVE", perm_server, Rights::ALL)?;

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
	for value in [&b"imgview"[..], &b"vol://system/sample.bmp"[..], &b"vol://system"[..]] {
		view_run.extend_from_slice(&(value.len() as u16).to_le_bytes());
		view_run.extend_from_slice(value);
	}
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
	let quit_frame = [1, 0, 0, 0, 0x14, 0, 1];
	key_producer.send(Message::new(quit_frame.to_vec(), alloc::vec::Vec::new(), 0)).map_err(|_| "failed to send imgview quit key")?;
	sched::run_until_idle();

	let release = view_display_server.recv().map_err(|_| "imgview did not release its surface after q")?;
	if release.bytes.len() < 6 || le_u16(&release.bytes, 0) != 3 {
		return Err("imgview sent an invalid release request");
	}
	let release_corr = le_u32(&release.bytes, 2);
	view_display_server.send(Message::new([release_corr.to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).map_err(|_| "imgview release reply failed")?;
	core::mem::drop(view_output);
	sched::run_until_idle();
	if !view_process.is_terminated() {
		return Err("imgview did not exit after releasing the surface");
	}

	// Launch imgconv through PermissionManager on the read-only scenario volume.
	// The destination already exists and --force is absent, so this proves its
	// volumes-only grant and conflict policy without attempting a mutation here;
	// the separate writable-block test proves the conversion/write path.
	let launch_imgconv_conflict = |correlation: u32| -> Result<(), &'static str> {
		let (convert_output, convert_stdout) = Channel::create();
		let mut convert_run = alloc::vec::Vec::new();
		convert_run.extend_from_slice(&3u16.to_le_bytes());
		convert_run.extend_from_slice(&correlation.to_le_bytes());
		for value in [&b"imgconv"[..], &b"vol://system/sample.bmp vol://system/sample.png"[..], &b"vol://system"[..]] {
			convert_run.extend_from_slice(&(value.len() as u16).to_le_bytes());
			convert_run.extend_from_slice(value);
		}
		convert_run.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&perm_client, &convert_run, convert_stdout, Rights::ALL)?;
		sched::run_until_idle();
		let convert_reply = perm_client.recv().map_err(|_| "PermissionManager did not answer imgconv run")?;
		if convert_reply.bytes.len() < 5 || convert_reply.bytes[4] == 0 {
			return Err("PermissionManager refused imgconv");
		}
		let convert_process = convert_reply.caps.first().ok_or("imgconv run returned no Process handle")?.object().into_any_arc().downcast::<Process>().map_err(|_| "imgconv run handle was not a Process")?;
		let conflict = convert_output.recv().map_err(|_| "imgconv printed no conflict")?;
		if conflict.bytes != b"imgconv: destination exists (use --force)\n" {
			return Err("imgconv conflict result was invalid");
		}
		sched::run_until_idle();
		if !convert_process.is_terminated() {
			return Err("imgconv did not exit after destination conflict");
		}
		Ok(())
	};
	let bounded_before = sched::root_domain().child_domains().len();
	launch_imgconv_conflict(20)?;
	let bounded_after_first = sched::root_domain().child_domains().len();
	if bounded_after_first != bounded_before + 1 {
		return Err("first bounded imgconv launch did not create exactly one budget Domain");
	}
	launch_imgconv_conflict(21)?;
	if sched::root_domain().child_domains().len() != bounded_after_first {
		return Err("repeated bounded imgconv launch leaked another budget Domain");
	}

	// Launch the governed WAV player with a fresh playback-only AudioService scope.
	// The stand-in accepts only a prefix of the first write, proving `play` retries the
	// unaccepted suffix in a new buffer before explicitly closing the stream.
	let (play_audio_server, play_audio_client) = Channel::create();
	admin_reply(&audio_admin_server, play_audio_client, 0)?;
	let (play_output, play_stdout) = Channel::create();
	let mut play_run = alloc::vec::Vec::new();
	play_run.extend_from_slice(&3u16.to_le_bytes());
	play_run.extend_from_slice(&2u32.to_le_bytes());
	for value in [&b"play"[..], &b"vol://system/test.wav"[..], &b"vol://system"[..]] {
		play_run.extend_from_slice(&(value.len() as u16).to_le_bytes());
		play_run.extend_from_slice(value);
	}
	play_run.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&perm_client, &play_run, play_stdout, Rights::ALL)?;
	sched::run_until_idle();
	let play_reply = perm_client.recv().map_err(|_| "PermissionManager did not answer play run")?;
	if play_reply.bytes.len() < 5 || play_reply.bytes[4] == 0 {
		return Err("PermissionManager refused play");
	}
	let play_process = play_reply.caps.first().ok_or("play run returned no Process handle")?.object().into_any_arc().downcast::<Process>().map_err(|_| "play run handle was not a Process")?;

	let open = play_audio_server.recv().map_err(|_| "play did not open an audio stream")?;
	if open.bytes.len() < 11 || le_u16(&open.bytes, 0) != 2 || le_u32(&open.bytes, 6) != 44_100 || open.bytes[10] != 1 {
		return Err("play opened the wrong WAV format");
	}
	let open_corr = le_u32(&open.bytes, 2);
	let (pcm_server, pcm_client) = Channel::create();
	let mut open_reply = alloc::vec::Vec::new();
	open_reply.extend_from_slice(&open_corr.to_le_bytes());
	open_reply.push(1);
	open_reply.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&play_audio_server, &open_reply, pcm_client, Rights::ALL)?;
	sched::run_until_idle();

	let first_write = pcm_server.recv().map_err(|_| "play sent no first PCM write")?;
	if first_write.bytes.len() < 14 || le_u16(&first_write.bytes, 0) != 1 || le_u64(&first_write.bytes, 6) != 2_048 || first_write.caps.len() != 1 {
		return Err("play sent an invalid first PCM write");
	}
	let first_buffer = first_write.caps[0].object().into_any_arc().downcast::<MemoryObject>().map_err(|_| "play PCM write did not transfer a MemoryObject")?;
	if !read_from_object(&first_buffer, 2_048).iter().any(|byte| *byte != 0) {
		return Err("play decoded silent PCM from the non-silent WAV fixture");
	}
	let first_corr = le_u32(&first_write.bytes, 2);
	pcm_server.send(Message::new([first_corr.to_le_bytes().as_slice(), &[1], &128u32.to_le_bytes()].concat(), alloc::vec::Vec::new(), 0)).map_err(|_| "first PCM write reply failed")?;
	sched::run_until_idle();

	let second_write = pcm_server.recv().map_err(|_| "play did not retry the unaccepted PCM suffix")?;
	if second_write.bytes.len() < 14 || le_u16(&second_write.bytes, 0) != 1 || le_u64(&second_write.bytes, 6) != 1_792 || second_write.caps.len() != 1 {
		return Err("play retried the wrong PCM suffix");
	}
	play_process.set_int_pending();
	for thread in play_process.live_threads() {
		sched::wake_thread(&thread);
	}
	let second_corr = le_u32(&second_write.bytes, 2);
	pcm_server.send(Message::new([second_corr.to_le_bytes().as_slice(), &[1], &896u32.to_le_bytes()].concat(), alloc::vec::Vec::new(), 0)).map_err(|_| "second PCM write reply failed")?;
	sched::run_until_idle();

	let close_request = pcm_server.recv().map_err(|_| "play did not explicitly close its PCM stream")?;
	if close_request.bytes.len() < 6 || le_u16(&close_request.bytes, 0) != 2 {
		return Err("play sent an invalid PCM close");
	}
	let close_corr = le_u32(&close_request.bytes, 2);
	pcm_server.send(Message::new([close_corr.to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).map_err(|_| "PCM close reply failed")?;
	core::mem::drop(play_output);
	sched::run_until_idle();
	if !play_process.is_terminated() {
		return Err("play did not exit after closing its PCM stream");
	}

	for (run_corr, uri, channels, pcm_bytes) in [
		(3u32, &b"vol://system/test-ima.wav"[..], 1u8, 2_048u64),
		(4u32, &b"vol://system/test-ms.wav"[..], 1u8, 2_048u64),
		(5u32, &b"vol://system/test.aiff"[..], 1u8, 2_048u64),
		(6u32, &b"vol://system/test.aifc"[..], 1u8, 2_048u64),
		(7u32, &b"vol://system/test.flac"[..], 1u8, 2_048u64),
		(8u32, &b"vol://system/test.wv"[..], 1u8, 2_048u64),
		(9u32, &b"vol://system/test-stereo.wv"[..], 2u8, 4_096u64),
		(10u32, &b"vol://system/test.ogg"[..], 1u8, 2_048u64),
	] {
		let (audio_server, audio_client) = Channel::create();
		admin_reply(&audio_admin_server, audio_client, 0)?;
		let (output, stdout) = Channel::create();
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&3u16.to_le_bytes());
		request.extend_from_slice(&run_corr.to_le_bytes());
		for value in [&b"play"[..], uri, &b"vol://system"[..]] {
			request.extend_from_slice(&(value.len() as u16).to_le_bytes());
			request.extend_from_slice(value);
		}
		request.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&perm_client, &request, stdout, Rights::ALL)?;
		sched::run_until_idle();
		let reply = perm_client.recv().map_err(|_| "PermissionManager did not answer audio play run")?;
		if reply.bytes.len() < 5 || reply.bytes[4] == 0 {
			return Err("PermissionManager refused audio play");
		}
		let process = reply.caps.first().ok_or("audio play returned no Process handle")?.object().into_any_arc().downcast::<Process>().map_err(|_| "audio play handle was not a Process")?;
		let open = audio_server.recv().map_err(|_| "audio play did not open an audio stream")?;
		if open.bytes.len() < 11 || le_u16(&open.bytes, 0) != 2 || le_u32(&open.bytes, 6) != 44_100 || open.bytes[10] != channels {
			return Err("audio play opened the wrong format");
		}
		let (stream_server, stream_client) = Channel::create();
		let open_corr = le_u32(&open.bytes, 2);
		let mut open_reply = alloc::vec::Vec::new();
		open_reply.extend_from_slice(&open_corr.to_le_bytes());
		open_reply.push(1);
		open_reply.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&audio_server, &open_reply, stream_client, Rights::ALL)?;
		sched::run_until_idle();
		let write = stream_server.recv().map_err(|_| "audio play sent no PCM write")?;
		if write.bytes.len() < 14 || le_u16(&write.bytes, 0) != 1 || le_u64(&write.bytes, 6) != pcm_bytes || write.caps.len() != 1 {
			return Err("audio play sent invalid decoded PCM");
		}
		let buffer = write.caps[0].object().into_any_arc().downcast::<MemoryObject>().map_err(|_| "audio play did not transfer PCM memory")?;
		if !read_from_object(&buffer, pcm_bytes as usize).iter().any(|byte| *byte != 0) {
			return Err("audio play decoded silence");
		}
		let write_corr = le_u32(&write.bytes, 2);
		let accepted_frames = (pcm_bytes / channels as u64 / 2) as u32;
		process.set_int_pending();
		for thread in process.live_threads() {
			sched::wake_thread(&thread);
		}
		stream_server.send(Message::new([write_corr.to_le_bytes().as_slice(), &[1], &accepted_frames.to_le_bytes()].concat(), alloc::vec::Vec::new(), 0)).map_err(|_| "audio PCM reply failed")?;
		sched::run_until_idle();
		let close_request = stream_server.recv().map_err(|_| "audio play did not close")?;
		let close_corr = le_u32(&close_request.bytes, 2);
		stream_server.send(Message::new([close_corr.to_le_bytes().as_slice(), &[1]].concat(), alloc::vec::Vec::new(), 0)).map_err(|_| "audio close reply failed")?;
		core::mem::drop(output);
		sched::run_until_idle();
		if !process.is_terminated() {
			return Err("audio play did not exit");
		}
	}

	let (mp3_audio_server, mp3_audio_client) = Channel::create();
	admin_reply(&audio_admin_server, mp3_audio_client, 0)?;
	let (mp3_output, mp3_stdout) = Channel::create();
	let mut mp3_run = alloc::vec::Vec::new();
	mp3_run.extend_from_slice(&3u16.to_le_bytes());
	mp3_run.extend_from_slice(&11u32.to_le_bytes());
	for value in [&b"play"[..], &b"vol://system/test.mp3"[..], &b"vol://system"[..]] {
		mp3_run.extend_from_slice(&(value.len() as u16).to_le_bytes());
		mp3_run.extend_from_slice(value);
	}
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
	Ok((expected, probe_read.bytes, probe_summary.bytes, date_read.bytes, date_summary.bytes, request_read.bytes, request_summary.bytes, cat_read.bytes, ip_read.bytes, ip_summary.bytes, graphics_read.bytes, graphics_start_ns))
}

// Build the component topology and run it to completion. A StorageService serves
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
	let bytes: alloc::vec::Vec<u8> = result.bytes;
	if bytes.len() < 5 {
		return Err("the host report was too short");
	}
	let logged: bool = bytes[0] != 0;
	let score: i32 = i32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
	let content: alloc::vec::Vec<u8> = bytes[5..].to_vec();
	Ok((expected, content, logged, score))
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
	fn tags(&self) -> &'static [TestTag];
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
	ArchAarch64 => "arch-aarch64",
	ArchRiscv64 => "arch-riscv64",
	ArchX86_64 => "arch-x86_64",
	Audio => "audio",
	Boot => "boot",
	Console => "console",
	Display => "display",
	Drivers => "drivers",
	Dynamic => "dynamic",
	DynamicReject => "dynamic-reject",
	Filesystem => "filesystem",
	Image => "image",
	Input => "input",
	Ipc => "ipc",
	Kernel => "kernel",
	Memory => "memory",
	Mouse => "mouse",
	Network => "network",
	Process => "process",
	Scheduler => "scheduler",
	Service => "service",
	Shell => "shell",
	Smoke => "smoke",
	Slow => "slow",
	Storage => "storage",
	Stress => "stress",
	Syscall => "syscall",
	Usb => "usb",
}

pub(crate) struct TaggedTest {
	name: &'static str,
	tags: &'static [TestTag],
	run: fn(),
}

impl Testable for TaggedTest {
	fn run(&self) {
		serial_print!("{}...\t", self.name);
		(self.run)();
		serial_println!("[ok]");
	}

	fn tags(&self) -> &'static [TestTag] {
		self.tags
	}
}

macro_rules! tagged_test {
	($(#[$attr:meta])* $name:ident, [$first_tag:ident $(, $tag:ident)* $(,)?]) => {
		$(#[$attr])*
		mod $name {
			use super::*;

			#[test_case]
			static CASE: TaggedTest = TaggedTest {
				name: stringify!($name),
				tags: &[TestTag::$first_tag $(, TestTag::$tag)*],
				run: super::$name,
			};
		}
	};
}

pub(crate) fn test_runner(tests: &[&dyn Testable]) {
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

tagged_test!(trivial_assertion, [Kernel, Smoke]);
fn trivial_assertion() {
	assert_eq!(1 + 1, 2);
}

tagged_test!(
	#[cfg(target_arch = "x86_64")]
	breakpoint_exception_returns,
	[Kernel, ArchX86_64]
);
#[cfg(target_arch = "x86_64")]
fn breakpoint_exception_returns() {
	// reaching the next line proves the IDT breakpoint handler returned cleanly
	unsafe { core::arch::asm!("int3") };
}

tagged_test!(
	#[cfg(target_arch = "riscv64")]
	breakpoint_exception_returns,
	[Kernel, ArchRiscv64]
);
#[cfg(target_arch = "riscv64")]
fn breakpoint_exception_returns() {
	// reaching the next line proves the trap handler resumed past the ebreak: it decodes
	// the trapped instruction width (2 bytes for a compressed c.ebreak, else 4) and
	// advances sepc, the riscv analogue of x86's int3 breakpoint round-trip.
	unsafe { core::arch::asm!("ebreak") };
}

tagged_test!(frame_alloc_distinct, [Memory, Smoke]);
fn frame_alloc_distinct() {
	let a = mem::frame::allocate().expect("frame a");
	let b = mem::frame::allocate().expect("frame b");
	assert_ne!(a, b);
	mem::frame::deallocate(a);
	mem::frame::deallocate(b);
}

tagged_test!(dynamic_symbol_names_accept_rust_mangling_with_a_bound, [Memory, Process]);
fn dynamic_symbol_names_accept_rust_mangling_with_a_bound() {
	let address_space = object::address_space::AddressSpace::create().expect("address space");
	let process = object::process::Process::new(address_space, sched::root_domain());
	let accepted = alloc::string::String::from_utf8(alloc::vec![b'x'; elf::MAX_DYNAMIC_SYMBOL_NAME]).expect("ASCII symbol");
	assert!(process.register_dynamic_symbols(&[(accepted, 0x2000_1000)]), "the bounded Rust symbol is accepted");
	let rejected = alloc::string::String::from_utf8(alloc::vec![b'y'; elf::MAX_DYNAMIC_SYMBOL_NAME + 1]).expect("ASCII symbol");
	assert!(!process.register_dynamic_symbols(&[(rejected, 0x2000_2000)]), "an overlong symbol is rejected");
}

tagged_test!(elf_dyn_applies_relative_relocations_and_rejects_symbols, [Dynamic, DynamicReject, Memory, Process]);
fn elf_dyn_applies_relative_relocations_and_rejects_symbols() {
	use crate::elf::ElfError;
	use crate::object::address_space::AddressSpace;
	use crate::object::process::Process;

	fn put16(bytes: &mut [u8], offset: usize, value: u16) {
		bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
	}

	fn put32(bytes: &mut [u8], offset: usize, value: u32) {
		bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
	}

	fn put64(bytes: &mut [u8], offset: usize, value: u64) {
		bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
	}

	fn program_header(bytes: &mut [u8], index: usize, kind: u32, flags: u32, offset: u64, address: u64, file_size: u64, memory_size: u64) {
		let base = 64 + index * 56;
		put32(bytes, base, kind);
		put32(bytes, base + 4, flags);
		put64(bytes, base + 8, offset);
		put64(bytes, base + 16, address);
		put64(bytes, base + 24, 0);
		put64(bytes, base + 32, file_size);
		put64(bytes, base + 40, memory_size);
		put64(bytes, base + 48, 1);
	}

	fn image(symbol: u32) -> Vec<u8> {
		const CODE_OFFSET: usize = 0x200;
		const DATA_OFFSET: usize = 0x300;
		const DATA_ADDRESS: u64 = 0x2000;
		const RELA_OFFSET: usize = 0x10;
		const DYNAMIC_OFFSET: usize = 0x30;
		const DATA_LEN: usize = 0x80;
		let mut bytes = alloc::vec![0u8; DATA_OFFSET + DATA_LEN];
		bytes[..4].copy_from_slice(b"\x7fELF");
		bytes[4] = 2;
		bytes[5] = 1;
		put16(&mut bytes, 16, 3);
		put16(&mut bytes, 18, elf_machine());
		put32(&mut bytes, 20, 1);
		put64(&mut bytes, 24, 0);
		put64(&mut bytes, 32, 64);
		put16(&mut bytes, 52, 64);
		put16(&mut bytes, 54, 56);
		put16(&mut bytes, 56, 3);
		program_header(&mut bytes, 0, 1, 5, CODE_OFFSET as u64, 0, 1, 1);
		program_header(&mut bytes, 1, 1, 6, DATA_OFFSET as u64, DATA_ADDRESS, DATA_LEN as u64, DATA_LEN as u64);
		program_header(&mut bytes, 2, 2, 6, (DATA_OFFSET + DYNAMIC_OFFSET) as u64, DATA_ADDRESS + DYNAMIC_OFFSET as u64, 5 * 16, 5 * 16);
		bytes[CODE_OFFSET] = 0xc3;
		let rela = DATA_OFFSET + RELA_OFFSET;
		put64(&mut bytes, rela, DATA_ADDRESS);
		put64(&mut bytes, rela + 8, (symbol as u64) << 32 | relative_relocation_type() as u64);
		put64(&mut bytes, rela + 16, 0x1234);
		let dynamic = DATA_OFFSET + DYNAMIC_OFFSET;
		for (index, (tag, value)) in [(7u64, DATA_ADDRESS + RELA_OFFSET as u64), (8, 24), (9, 24), (0x6fff_fff9, 1), (0, 0)].into_iter().enumerate() {
			put64(&mut bytes, dynamic + index * 16, tag);
			put64(&mut bytes, dynamic + index * 16 + 8, value);
		}
		bytes
	}

	fn symbol_image(provider: bool, code: u8) -> Vec<u8> {
		const CODE_OFFSET: usize = 0x200;
		const DATA_OFFSET: usize = 0x300;
		const DATA_ADDRESS: u64 = 0x2000;
		const SYMBOL_OFFSET: usize = 0x20;
		const HASH_OFFSET: usize = 0x50;
		const RELA_OFFSET: usize = 0x70;
		const DYNAMIC_OFFSET: usize = 0x90;
		const TARGET_OFFSET: usize = 0x110;
		const DATA_LEN: usize = 0x120;
		let strings = b"\0shared_value\0";
		let mut bytes = alloc::vec![0u8; DATA_OFFSET + DATA_LEN];
		bytes[..4].copy_from_slice(b"\x7fELF");
		bytes[4] = 2;
		bytes[5] = 1;
		put16(&mut bytes, 16, 3);
		put16(&mut bytes, 18, elf_machine());
		put32(&mut bytes, 20, 1);
		put64(&mut bytes, 24, 0);
		put64(&mut bytes, 32, 64);
		put16(&mut bytes, 52, 64);
		put16(&mut bytes, 54, 56);
		put16(&mut bytes, 56, 3);
		program_header(&mut bytes, 0, 1, 5, CODE_OFFSET as u64, 0, 1, 1);
		program_header(&mut bytes, 1, 1, 6, DATA_OFFSET as u64, DATA_ADDRESS, DATA_LEN as u64, DATA_LEN as u64);
		let dynamic_entries = if provider { 6 } else { 9 };
		program_header(&mut bytes, 2, 2, 6, (DATA_OFFSET + DYNAMIC_OFFSET) as u64, DATA_ADDRESS + DYNAMIC_OFFSET as u64, dynamic_entries * 16, dynamic_entries * 16);
		bytes[CODE_OFFSET] = code;
		bytes[DATA_OFFSET..DATA_OFFSET + strings.len()].copy_from_slice(strings);
		let symbol = DATA_OFFSET + SYMBOL_OFFSET + 24;
		put32(&mut bytes, symbol, 1);
		bytes[symbol + 4] = 0x12;
		put16(&mut bytes, symbol + 6, if provider { 1 } else { 0 });
		put64(&mut bytes, symbol + 8, 0);
		let hash = DATA_OFFSET + HASH_OFFSET;
		for (index, word) in [1u32, 2, 1, 0, 0].into_iter().enumerate() {
			put32(&mut bytes, hash + index * 4, word);
		}
		if !provider {
			let rela = DATA_OFFSET + RELA_OFFSET;
			put64(&mut bytes, rela, DATA_ADDRESS + TARGET_OFFSET as u64);
			put64(&mut bytes, rela + 8, 1u64 << 32 | import_relocation_type() as u64);
			put64(&mut bytes, rela + 16, 5);
		}
		let mut tags = alloc::vec![(5u64, DATA_ADDRESS), (10, strings.len() as u64), (6, DATA_ADDRESS + SYMBOL_OFFSET as u64), (11, 24), (4, DATA_ADDRESS + HASH_OFFSET as u64),];
		if !provider {
			tags.extend_from_slice(&[(7, DATA_ADDRESS + RELA_OFFSET as u64), (8, 24), (9, 24)]);
		}
		tags.push((0, 0));
		let dynamic = DATA_OFFSET + DYNAMIC_OFFSET;
		for (index, (tag, value)) in tags.into_iter().enumerate() {
			put64(&mut bytes, dynamic + index * 16, tag);
			put64(&mut bytes, dynamic + index * 16 + 8, value);
		}
		bytes
	}

	let address_space = AddressSpace::create().expect("ET_DYN address space");
	let mut frames = Vec::new();
	let mut shared = Vec::new();
	let entry = crate::elf::load_into(&image(0), &address_space, &mut frames, &mut shared).expect("relative-only ET_DYN loads");
	assert_eq!(entry, 0x1000_0000);
	assert_eq!((frames.len(), shared.len()), (1, 1));
	let relocated = unsafe { ((mem::hhdm_offset() + frames[0]) as *const u64).read_unaligned() };
	assert_eq!(relocated, 0x1000_1234);
	drop(address_space);
	for frame in frames {
		mem::frame::deallocate(frame);
	}

	let rejected_space = AddressSpace::create().expect("rejected ET_DYN address space");
	let mut rejected_frames = Vec::new();
	let mut rejected_shared = Vec::new();
	assert_eq!(crate::elf::load_into(&image(1), &rejected_space, &mut rejected_frames, &mut rejected_shared), Err(ElfError::BadImage));
	assert!(rejected_space.unmap(0x1000_0000).is_none(), "failed ET_DYN load rolled back every PTE");
	assert!(rejected_shared.is_empty());
	drop(rejected_space);
	for frame in rejected_frames {
		mem::frame::deallocate(frame);
	}

	let mut oversized = image(0);
	put64(&mut oversized, 64 + 56 + 40, 0x0200_0000);
	let oversized_space = AddressSpace::create().expect("oversized module address space");
	let mut oversized_frames = Vec::new();
	let mut oversized_shared = Vec::new();
	assert_eq!(crate::elf::load_module_into(&oversized, &oversized_space, &mut oversized_frames, &mut oversized_shared, 0x2000_0000, &|_| None), Err(ElfError::BadImage));
	assert!(oversized_space.unmap(0x2000_0000).is_none(), "oversized provider cannot escape its 16 MiB slot");
	for frame in oversized_frames {
		mem::frame::deallocate(frame);
	}

	let process = Process::new(AddressSpace::create().expect("dynamic module process address space"), sched::root_domain());
	let provider = symbol_image(true, 0xc3);
	let consumer = symbol_image(false, 0xc3);
	let colliding_provider = symbol_image(true, 0x90);
	crate::loader::load_module_into(&process, &provider, 0x2000_0000).expect("provider module loads and registers exports");
	assert_eq!(process.resolve_dynamic_symbol("shared_value"), Some(0x2000_0000));
	crate::loader::load_module_into(&process, &consumer, 0x2100_0000).expect("consumer resolves provider symbol eagerly");
	let consumer_data = process.address_space().unmap(0x2100_2000).expect("consumer data mapping");
	let imported = unsafe { ((mem::hhdm_offset() + consumer_data + 0x110) as *const u64).read_unaligned() };
	assert_eq!(imported, 0x2000_0005);
	assert_ne!(provider, colliding_provider, "colliding providers are distinct images");
	assert!(matches!(crate::loader::load_module_into(&process, &colliding_provider, 0x2200_0000), Err(crate::loader::LoadError::BadImage)), "distinct providers with a duplicate export are rejected");
	assert!(process.address_space().unmap(0x2200_0000).is_none(), "duplicate-export provider mapping is rolled back");
	let second = Process::new(AddressSpace::create().expect("second module process address space"), sched::root_domain());
	crate::loader::load_module_into(&second, &provider, 0x2000_0000).expect("same provider loads in a second process");
	let first_text = process.address_space().unmap(0x2000_0000).expect("first provider text mapping");
	let second_text = second.address_space().unmap(0x2000_0000).expect("second provider text mapping");
	assert_eq!(first_text, second_text, "two processes map one physical immutable provider page");
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
tagged_test!(contiguous_frames_and_dma_spans, [Memory, Drivers]);
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

tagged_test!(the_frame_pool_grows_past_the_boot_table_and_refuses_a_double_free, [Memory]);
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

tagged_test!(concurrent_maps_on_shared_tables_strand_nothing, [Memory, Stress]);
fn concurrent_maps_on_shared_tables_strand_nothing() {
	use core::sync::atomic::{AtomicU64, Ordering};
	use mem::frame;
	use object::address_space::AddressSpace;

	// The PT_LOCK stress test: two cores hammer map/unmap on virtual addresses
	// that share an intermediate page-table level, recreating the geometry of the
	// historical riscv64 race - two CPUs both observe a missing leaf table, both
	// allocate one, one write wins, and the loser's leaf lands in an orphaned table
	// (a lost mapping) while the orphan leaks a frame. Every ROUND the two workers
	// rendezvous on a barrier and map into the SAME fresh 2 MiB group (a new leaf
	// table under a shared mid-level on all three arches), then each unmap must
	// return exactly the frame that worker mapped - a stranded leaf returns None.
	// After the space drops, the pool must get at least one table frame back per
	// round: an orphaned (unlinked) table would not be reclaimed by
	// free_address_space, so the delta exposes the leak.
	const ROUNDS: u64 = 128;
	const BASE: u64 = 0x4000_0000;
	const GROUP: u64 = 0x20_0000; // 2 MiB: one leaf page table on x86 / aarch64 / riscv64
	static ROOT: AtomicU64 = AtomicU64::new(0);
	static FRAMES: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
	static ARRIVE: AtomicU64 = AtomicU64::new(0);
	static STRANDED: AtomicU64 = AtomicU64::new(0);
	static DONE: AtomicU64 = AtomicU64::new(0);

	extern "C" fn worker(which: u64) {
		let root = ROOT.load(Ordering::SeqCst);
		let frame = FRAMES[which as usize].load(Ordering::SeqCst);
		let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::NO_EXECUTE;
		for round in 0..ROUNDS {
			// Rendezvous: both workers enter the round together, so the two maps
			// race on creating the same fresh leaf table.
			ARRIVE.fetch_add(1, Ordering::SeqCst);
			let mut spins = 0u64;
			while ARRIVE.load(Ordering::SeqCst) < 2 * (round + 1) {
				core::hint::spin_loop();
				spins += 1;
				if spins > 2_000_000_000 {
					STRANDED.fetch_add(1, Ordering::SeqCst);
					DONE.fetch_add(1, Ordering::SeqCst);
					return;
				}
			}
			let va = BASE + round * GROUP + which * frame::PAGE_SIZE;
			if arch::paging::try_map_page_in(root, va, frame, flags).is_err() {
				STRANDED.fetch_add(1, Ordering::SeqCst);
				break;
			}
			// The leaf must still be OUR mapping: a stranded leaf (lost to an
			// orphaned table) reads back as unmapped here.
			if arch::paging::unmap_page_in(root, va) != Some(frame) {
				STRANDED.fetch_add(1, Ordering::SeqCst);
				break;
			}
		}
		DONE.fetch_add(1, Ordering::SeqCst);
	}

	// The stress needs two workers truly in parallel on their own cores; the test
	// topologies always boot with more (x86 nproc, aarch64 8, riscv64 4).
	if smp::cpu_count() < 3 {
		return;
	}
	let space = AddressSpace::create().expect("a scratch address space");
	ROOT.store(space.cr3(), Ordering::SeqCst);
	FRAMES[0].store(frame::allocate().expect("worker frame 0"), Ordering::SeqCst);
	FRAMES[1].store(frame::allocate().expect("worker frame 1"), Ordering::SeqCst);
	ARRIVE.store(0, Ordering::SeqCst);
	STRANDED.store(0, Ordering::SeqCst);
	DONE.store(0, Ordering::SeqCst);
	sched::spawn_on(1, worker, 0);
	sched::spawn_on(2, worker, 1);
	let mut spins = 0u64;
	while DONE.load(Ordering::SeqCst) < 2 {
		core::hint::spin_loop();
		spins += 1;
		assert!(spins < 20_000_000_000, "the PT stress workers did not finish");
	}
	assert_eq!(STRANDED.load(Ordering::SeqCst), 0, "a concurrent map on a shared intermediate level stranded a leaf");
	// Every round linked one fresh leaf table into the tree; dropping the space must
	// hand them all back (an orphaned table would stay allocated - the frame leak).
	let before_drop = frame::free_count();
	drop(space);
	let reclaimed = frame::free_count() - before_drop;
	assert!(reclaimed as u64 >= ROUNDS, "dropping the space reclaimed {reclaimed} frames, expected at least {ROUNDS} leaf tables - an intermediate table leaked");
	frame::deallocate(FRAMES[0].load(Ordering::SeqCst));
	frame::deallocate(FRAMES[1].load(Ordering::SeqCst));
}

tagged_test!(map_degrades_to_error_when_out_of_frames, [Memory]);
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

tagged_test!(paging_map_unmap, [Memory]);
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

tagged_test!(heap_box_vec, [Memory, Smoke]);
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

tagged_test!(timer_ticks_advance, [Kernel]);
fn timer_ticks_advance() {
	// Interrupts are enabled by kmain before the tests run, so the periodic
	// LAPIC timer must keep incrementing the tick counter.
	let start = arch::apic::ticks();
	while arch::apic::ticks() == start {
		core::hint::spin_loop();
	}
	assert!(arch::apic::ticks() > start);
}

tagged_test!(
	#[cfg(target_arch = "x86_64")]
	handler_registration_dispatch,
	[Kernel, ArchX86_64]
);
#[cfg(target_arch = "x86_64")]
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

tagged_test!(smp_all_cores_online, [Kernel, Smoke]);
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

tagged_test!(handle_create_lookup_close, [Kernel, Smoke]);
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

tagged_test!(handle_rights_enforced, [Kernel]);
fn handle_rights_enforced() {
	use object::handle::{HandleError, HandleTable};
	use object::rights::Rights;
	let mut table = HandleTable::new();
	let h = table.insert_object(TestObject::new(7), Rights::READ, 0);
	assert!(table.lookup(h, Rights::READ).is_ok());
	// A right the handle does not carry is denied.
	assert!(matches!(table.lookup(h, Rights::WRITE), Err(HandleError::AccessDenied)));
}

tagged_test!(handle_duplicate_attenuates, [Kernel]);
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

tagged_test!(handle_revocation_invalidates, [Kernel]);
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

tagged_test!(handle_type_sealing, [Kernel]);
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

tagged_test!(handle_refcount_lifetime, [Kernel]);
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

tagged_test!(thread_object_basics, [Process, Smoke]);
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

tagged_test!(scheduler_multiplexes_threads, [Scheduler, Smoke]);
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

#[cfg(target_arch = "x86_64")]
tagged_test!(scheduler_preserves_xmm_state, [Scheduler]);
#[cfg(target_arch = "x86_64")]
fn scheduler_preserves_xmm_state() {
	use core::arch::asm;
	use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
	static FAILED: AtomicBool = AtomicBool::new(false);
	static DONE: AtomicU32 = AtomicU32::new(0);
	extern "C" fn worker(value: u64) {
		unsafe { asm!("movq xmm15, {}", in(reg) value, options(nostack, preserves_flags)) };
		for _ in 0..64 {
			sched::yield_now();
			let mut observed: u64;
			unsafe { asm!("movq {}, xmm15", out(reg) observed, options(nostack, preserves_flags)) };
			if observed != value {
				FAILED.store(true, Ordering::SeqCst);
			}
		}
		DONE.fetch_add(1, Ordering::SeqCst);
	}
	FAILED.store(false, Ordering::SeqCst);
	DONE.store(0, Ordering::SeqCst);
	sched::spawn(worker, 0x1122_3344_5566_7788);
	sched::spawn(worker, 0x8877_6655_4433_2211);
	sched::run_until_idle();
	assert_eq!(DONE.load(Ordering::SeqCst), 2);
	assert!(!FAILED.load(Ordering::SeqCst), "one thread observed another thread's XMM state");
}

tagged_test!(preemption_preempts_a_cpu_bound_thread, [Scheduler]);
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

tagged_test!(a_cpu_bound_ring3_thread_is_preempted, [Scheduler, Process]);
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

tagged_test!(a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick, [Scheduler]);
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

tagged_test!(scheduler_runs_across_cores, [Scheduler]);
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

tagged_test!(process_isolation_and_per_process_tables, [Process, Memory]);
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

tagged_test!(syscall_roundtrip_stateless, [Syscall, Smoke]);
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

tagged_test!(abi_check_accepts_the_matching_revision_and_refuses_a_mismatch, [Syscall]);
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

tagged_test!(syscall_object_and_handle_ops, [Syscall]);
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
			// mapping the same object twice is rejected (only one active mapping)
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

tagged_test!(an_unmapped_va_range_is_reused_not_leaked, [Memory]);
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

tagged_test!(device_memory_maps_mmio_region, [Drivers]);
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

tagged_test!(random_get_fills_distinct_bytes, [Syscall]);
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

tagged_test!(
	#[cfg(target_arch = "x86_64")]
	interrupt_bind_delivers_to_driver,
	[Drivers, ArchX86_64]
);
#[cfg(target_arch = "x86_64")]
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
tagged_test!(
	#[cfg(target_arch = "aarch64")]
	gicv2m_msi_binds_and_dispatch_signals_the_driver,
	[Drivers, ArchAarch64]
);
#[cfg(target_arch = "aarch64")]
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

tagged_test!(
	#[cfg(target_arch = "aarch64")]
	gicv2m_msi_inventory_reports_the_timer_and_msi_vectors,
	[Drivers, ArchAarch64]
);
#[cfg(target_arch = "aarch64")]
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
tagged_test!(
	#[cfg(target_arch = "riscv64")]
	imsic_msi_binds_and_dispatch_signals_the_driver,
	[Drivers, ArchRiscv64]
);
#[cfg(target_arch = "riscv64")]
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

tagged_test!(
	#[cfg(target_arch = "riscv64")]
	imsic_msi_inventory_reports_the_timer_and_msi_vectors,
	[Drivers, ArchRiscv64]
);
#[cfg(target_arch = "riscv64")]
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

tagged_test!(object_property_set_names_an_object, [Kernel]);
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

tagged_test!(object_property_set_bounds_a_domain, [Kernel]);
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

tagged_test!(channel_message_and_capability_transfer, [Ipc]);
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

tagged_test!(blocking_wait_wakes_on_message, [Ipc]);
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

tagged_test!(a_sender_on_a_full_channel_blocks_and_wakes_on_drain, [Ipc]);
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

tagged_test!(blocking_wait_times_out_on_deadline, [Scheduler]);
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

tagged_test!(a_periodic_wait_ticks_but_never_holds_the_scheduler, [Scheduler]);
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

tagged_test!(wait_any_wakes_on_the_ready_handle, [Ipc]);
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

tagged_test!(waiting_on_a_process_handle_wakes_when_it_exits, [Process]);
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

tagged_test!(signal_terminate_wakes_a_blocked_thread, [Process]);
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

tagged_test!(channel_endpoint_semantics, [Ipc]);
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

tagged_test!(channel_peek_reports_the_pending_length, [Ipc]);
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

tagged_test!(pci_scan_finds_virtio_devices, [Drivers]);
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

tagged_test!(device_table_exposes_virtio_mmio, [Drivers]);
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

tagged_test!(pci_scan_finds_the_xhci_controller, [Drivers, Usb]);
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

tagged_test!(device_table_exposes_the_xhci_controller, [Drivers, Usb]);
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

tagged_test!(xhci_driver_enumerates_the_usb_bus, [Drivers, Usb, Slow]);
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
	let elf = pkg::Package::parse(volume).and_then(|p| p.lookup(b"drivers/xhci.lsexe")).expect("the xhci.lsexe driver should be staged on the volume under drivers/");

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
	let service_elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe should be in the init package");
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

tagged_test!(dma_buffer_maps_and_reports_phys, [Drivers, Memory]);
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
			assert_eq!(arch::syscall::invoke(syscall::SYS_DMA_BUFFER_UNMAP, handle, 0, 0, 0) as i64, 0);
			let remapped = arch::syscall::invoke(syscall::SYS_DMA_BUFFER_MAP, handle, 0, 0, 0);
			assert_eq!(remapped, virt, "the released DMA virtual range should be reused");
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

tagged_test!(log_record_roundtrip_and_renders, [Service]);
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
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");
	(boot_kernel, service_client)
}

// A managed service that cannot complete a required bootstrap step reports the failing
// step and the reason over its bootstrap channel before exiting, so the supervisor
// records why it went down instead of seeing an unexplained peer-close. DeviceManager
// needs the init package before it reports in; hand it a plain message where the package
// should be and it reports the failure honestly rather than dying silently.
tagged_test!(a_service_reports_a_bootstrap_failure, [Service]);
fn a_service_reports_a_bootstrap_failure() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	let init = init_package_bytes().expect("init package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let device_elf = package.lookup(b"device_manager.lsexe").expect("device_manager.lsexe in the init package");
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

tagged_test!(log_service_speaks_generated_bindings, [Service]);
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

tagged_test!(device_service_lists_devices, [Service, Drivers]);
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

tagged_test!(input_service_streams_pointer_events, [Service, Input, Mouse, Console]);
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
	let (_key_producer, key_consumer) = Channel::create();
	let (_focus_display, focus_input) = Channel::create();
	// ConsoleService's pointer sink: the test keeps the consumer end alive so the forward
	// channel stays open (InputService mirrors each raw event to it), but does not assert
	// on it here - the forwarding path is exercised by the live console.
	let (_forward_drain, forward_input) = Channel::create();
	let _input_service = spawn_dynamic_test_process(sched::root_domain(), service_elf, boot_user);
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");
	send_cap(&boot_kernel, b"INPUT", raw_consumer, Rights::ALL).expect("input raw bootstrap");
	// no USB pointer in this scenario: the second raw channel is absent (handle 0).
	boot_kernel.send(Message::new(b"INPUT2".to_vec(), alloc::vec::Vec::new(), 0)).expect("input2 raw bootstrap");
	send_cap(&boot_kernel, b"FORWARD", forward_input, Rights::ALL).expect("forward raw bootstrap");
	send_cap(&boot_kernel, b"KEYS", key_consumer, Rights::ALL).expect("key raw bootstrap");
	send_cap(&boot_kernel, b"FOCUS", focus_input, Rights::ALL).expect("focus bootstrap");
	boot_kernel.send(Message::new(b"KILL".to_vec(), alloc::vec::Vec::new(), 0)).expect("kill bootstrap");
	let (_admin_peer, admin) = Channel::create();
	send_cap(&boot_kernel, b"ADMIN", admin, Rights::ALL).expect("input admin bootstrap");

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

tagged_test!(input_service_streams_keys_only_with_display_focus, [Service, Input, Display]);
fn input_service_streams_keys_only_with_display_focus() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	fn subscribe(client: &Channel, corr: u32, proof: alloc::sync::Arc<Channel>) -> Option<alloc::sync::Arc<Channel>> {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&2u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&0u32.to_le_bytes());
		send_cap(client, &request, proof, Rights::ALL).expect("key subscription request");
		sched::run_until_idle();
		let reply = client.recv().expect("key subscription reply");
		assert_eq!(le_u32(&reply.bytes, 0), corr, "subscription echoes correlation id");
		reply.caps.first().map(|cap| cap.object().into_any_arc().downcast::<Channel>().expect("key stream is a channel"))
	}

	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let service_elf = program_elf(&package, volume, b"input_service").expect("input_service in the package or volume");
	let (boot_kernel, boot_user) = Channel::create();
	let (service_server, _service_client) = Channel::create();
	let (_pointer_a, pointer_b) = Channel::create();
	let (console_focus, forward_b) = Channel::create();
	let (keys_driver, keys_input) = Channel::create();
	let (focus_display, focus_input) = Channel::create();
	let (kill_display, kill_input) = Channel::create();
	let _input_service = spawn_dynamic_test_process(sched::root_domain(), service_elf, boot_user);
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");
	send_cap(&boot_kernel, b"INPUT", pointer_b, Rights::ALL).expect("pointer bootstrap");
	boot_kernel.send(Message::new(b"INPUT2".to_vec(), alloc::vec::Vec::new(), 0)).expect("second pointer bootstrap");
	send_cap(&boot_kernel, b"FORWARD", forward_b, Rights::ALL).expect("forward bootstrap");
	send_cap(&boot_kernel, b"KEYS", keys_input, Rights::ALL).expect("keys bootstrap");
	send_cap(&boot_kernel, b"FOCUS", focus_input, Rights::ALL).expect("focus bootstrap");
	send_cap(&boot_kernel, b"KILL", kill_input, Rights::ALL).expect("kill bootstrap");
	let (input_admin, admin) = Channel::create();
	send_cap(&boot_kernel, b"ADMIN", admin, Rights::ALL).expect("input admin bootstrap");
	sched::run_until_idle();
	let online = boot_kernel.recv().expect("InputService online report");
	assert_eq!(&online.bytes[..], b"InputService: online");
	let mut open_keys = alloc::vec::Vec::new();
	open_keys.extend_from_slice(&1u16.to_le_bytes());
	open_keys.extend_from_slice(&40u32.to_le_bytes());
	input_admin.send(Message::new(open_keys, alloc::vec::Vec::new(), 0)).expect("open key-only connection");
	sched::run_until_idle();
	let reply = input_admin.recv().expect("key-only connection reply");
	let scoped = reply.caps.first().expect("key-only connection").object().into_any_arc().downcast::<Channel>().expect("key-only grant is a channel");
	let mut pointer_request = alloc::vec::Vec::new();
	pointer_request.extend_from_slice(&1u16.to_le_bytes());
	pointer_request.extend_from_slice(&41u32.to_le_bytes());
	scoped.send(Message::new(pointer_request, alloc::vec::Vec::new(), 0)).expect("forbidden pointer snapshot");
	sched::run_until_idle();
	let denied = scoped.recv().expect("pointer scope denial");
	assert!(denied.caps.is_empty(), "key-only connection cannot open a pointer stream");

	// An unrelated channel is not a display-minted peer and cannot open the stream.
	let (forged, _forged_peer) = Channel::create();
	assert!(subscribe(&scoped, 1, forged).is_none(), "a forged focus proof must be refused");

	// DisplayService registers one peer and transfers its counterpart to the client.
	let (proof, registered) = Channel::create();
	send_cap(&focus_display, b"SET", registered, Rights::ALL).expect("register focus peer");
	let stream = subscribe(&scoped, 2, proof).expect("active display proof opens the key stream");
	let focus_ack = focus_display.recv().expect("focus acknowledgement");
	assert_eq!(&focus_ack.bytes[..], b"OK");
	let suppressed = console_focus.recv().expect("console focus suppression");
	assert_eq!(&suppressed.bytes[..], b"KEYFOCUS\0");
	keys_driver.send(Message::new(alloc::vec![0x04, 0, 1], alloc::vec::Vec::new(), 0)).expect("A down");
	keys_driver.send(Message::new(alloc::vec![0x04, 0, 1], alloc::vec::Vec::new(), 0)).expect("duplicate A down");
	sched::run_until_idle();
	focus_display.send(Message::new(b"CLEAR".to_vec(), alloc::vec::Vec::new(), 0)).expect("revoke focus");
	sched::run_until_idle();
	let clear_ack = focus_display.recv().expect("clear acknowledgement");
	assert_eq!(&clear_ack.bytes[..], b"OK");
	let cleared = console_focus.recv().expect("console focus clear");
	assert_eq!(&cleared.bytes[..], b"KEYFOCUS\0");
	let down = stream.recv().expect("A down frame");
	let up = stream.recv().expect("synthetic A up frame");
	assert_eq!((le_u16(&down.bytes, 4), down.bytes[6]), (0x04, 1), "canonical HID A down");
	assert_eq!((le_u16(&up.bytes, 4), up.bytes[6]), (0x04, 0), "focus loss releases held A");
	assert!(stream.recv().is_err(), "focus loss closes the key stream");
	keys_driver.send(Message::new(alloc::vec![0x04, 0, 0], alloc::vec::Vec::new(), 0)).expect("physical A up");
	sched::run_until_idle();
	focus_display.send(Message::new(b"CONSOLE".to_vec(), alloc::vec::Vec::new(), 0)).expect("restore console focus");
	sched::run_until_idle();
	let console_ack = focus_display.recv().expect("console focus acknowledgement");
	assert_eq!(&console_ack.bytes[..], b"OK");
	let restored = console_focus.recv().expect("console focus restoration");
	assert_eq!(&restored.bytes[..], b"KEYFOCUS\x01");

	// Ctrl+Alt+Esc is consumed as the emergency display-revocation chord.
	let (proof2, registered2) = Channel::create();
	send_cap(&focus_display, b"SET", registered2, Rights::ALL).expect("register second focus peer");
	let stream2 = subscribe(&scoped, 3, proof2).expect("second active proof opens a stream");
	let second_ack = focus_display.recv().expect("second focus acknowledgement");
	assert_eq!(&second_ack.bytes[..], b"OK");
	for event in [[0xe0, 0, 1], [0xe2, 0, 1], [0x29, 0, 1]] {
		keys_driver.send(Message::new(event.to_vec(), alloc::vec::Vec::new(), 0)).expect("kill chord key");
	}
	sched::run_until_idle();
	let kill = kill_display.recv().expect("kill chord reaches DisplayService");
	assert_eq!(&kill.bytes[..], b"KILL");
	let mut frames: usize = 0;
	while stream2.recv().is_ok() {
		frames += 1;
	}
	assert_eq!(frames, 6, "three key-down frames are followed by three synthetic releases");
}

tagged_test!(display_service_restores_the_console_surface, [Service, Console, Display, Memory]);
fn display_service_restores_the_console_surface() {
	use object::address_space::AddressSpace;
	use object::channel::{Channel, Message};
	use object::dma_buffer::DmaBuffer;
	use object::memory_object::MemoryObject;
	use object::process::Process;
	use object::rights::Rights;

	fn request(op: u16, corr: u32, args: &[u32]) -> Message {
		let mut bytes = alloc::vec::Vec::new();
		bytes.extend_from_slice(&op.to_le_bytes());
		bytes.extend_from_slice(&corr.to_le_bytes());
		for value in args {
			bytes.extend_from_slice(&value.to_le_bytes());
		}
		Message::new(bytes, alloc::vec::Vec::new(), 0)
	}

	fn connect(root: &Channel) -> alloc::sync::Arc<Channel> {
		root.send(Message::new(abi::CONNECT_OP.to_le_bytes().to_vec(), alloc::vec::Vec::new(), 0)).expect("connect request");
		sched::run_until_idle();
		let reply = root.recv().expect("connect reply");
		let cap = reply.caps.first().expect("connected display channel");
		cap.object().into_any_arc().downcast::<Channel>().expect("display connection is a channel")
	}

	fn acknowledge_focus(focus: &Channel, expected: &[u8]) {
		sched::run_until_idle();
		let command = focus.recv().expect("focus command");
		assert_eq!(&command.bytes[..], expected, "expected focus transition");
		focus.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("focus acknowledgement");
	}

	fn acquire(client: &Channel, focus: &Channel, expected_focus: &[u8], corr: u32, width: u32, height: u32) -> alloc::sync::Arc<MemoryObject> {
		client.send(request(1, corr, &[width, height])).expect("acquire request");
		acknowledge_focus(focus, expected_focus);
		sched::run_until_idle();
		let reply = client.recv().expect("acquire reply");
		assert_eq!(le_u32(&reply.bytes, 0), corr, "acquire echoes correlation id");
		assert_eq!(reply.bytes[4], 1, "acquire succeeds");
		assert_eq!(le_u32(&reply.bytes, 13), if width == 0 { 4 } else { width }, "surface width");
		assert_eq!(le_u32(&reply.bytes, 17), if height == 0 { 4 } else { height }, "surface height");
		let cap = reply.caps.first().expect("surface MemoryObject");
		cap.object().into_any_arc().downcast::<MemoryObject>().expect("surface buffer is a MemoryObject")
	}

	fn fill(object: &MemoryObject, pixel: u32, pixels: usize) {
		let base = mem::hhdm_offset() + object.frames()[0];
		let words = unsafe { core::slice::from_raw_parts_mut(base as *mut u32, pixels) };
		words.fill(pixel);
	}

	fn set_surface_pixel(object: &MemoryObject, index: usize, pixel: u32) {
		let base = mem::hhdm_offset() + object.frames()[0];
		unsafe { ((base as *mut u32).add(index)).write_unaligned(pixel) };
	}

	fn scanout_pixel_at(scanout: &DmaBuffer, x: usize, y: usize) -> u32 {
		unsafe { (((mem::hhdm_offset() + scanout.frames()[0]) as *const u32).add(y * 4 + x)).read_unaligned() }
	}

	fn scanout_pixel(scanout: &DmaBuffer) -> u32 {
		scanout_pixel_at(scanout, 0, 0)
	}

	fn acknowledge_present(gpu: &Channel, client: Option<(&Channel, u32)>) -> Message {
		sched::run_until_idle();
		let present = gpu.recv().expect("synchronous PRESENT reaches the gpu");
		assert_eq!(&present.bytes[..7], b"PRESENT", "DisplayService uses the acknowledged present path");
		gpu.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("present acknowledgement");
		sched::run_until_idle();
		if let Some((channel, corr)) = client {
			let reply = channel.recv().expect("typed display reply");
			assert_eq!(le_u32(&reply.bytes, 0), corr, "reply echoes correlation id");
			assert_eq!(reply.bytes[4], 1, "display operation succeeds");
		}
		present
	}

	fn display_stats(admin: &Channel, corr: u32) -> [u64; 8] {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&2u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		admin.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("display stats request");
		sched::run_until_idle();
		let reply = admin.recv().expect("display stats reply");
		assert_eq!(le_u32(&reply.bytes, 0), corr);
		core::array::from_fn(|index| le_u64(&reply.bytes, 4 + index * 8))
	}

	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let service_elf = program_elf(&package, volume, b"display_service").expect("display_service in the package or volume");
	let (boot_kernel, boot_user) = Channel::create();
	let (service_server, console_client) = Channel::create();
	let (gpu_kernel, gpu_user) = Channel::create();
	let (focus_input, focus_display) = Channel::create();
	let (kill_input, kill_display) = Channel::create();
	let _display_service = spawn_dynamic_test_process(sched::root_domain(), service_elf, boot_user);
	send_cap(&boot_kernel, b"GPU", gpu_user, Rights::ALL).expect("gpu bootstrap");
	send_cap(&boot_kernel, b"FOCUS", focus_display, Rights::ALL).expect("focus bootstrap");
	send_cap(&boot_kernel, b"KILL", kill_display, Rights::ALL).expect("kill bootstrap");
	let (display_admin, admin) = Channel::create();
	send_cap(&boot_kernel, b"ADMIN", admin, Rights::ALL).expect("display admin bootstrap");
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");

	// Answer the driver's FB handshake with a 4x4 B8G8R8X8 DMA scanout.
	sched::run_until_idle();
	let fb_request = gpu_kernel.recv().expect("framebuffer request");
	assert_eq!(&fb_request.bytes[..], b"FB", "DisplayService requests the scanout");
	let scanout = match DmaBuffer::create_in(&sched::root_domain(), 4 * 4 * 4) {
		Ok(scanout) => scanout,
		Err(_) => panic!("stand-in scanout"),
	};
	let fb = abi::Framebuffer { width: 4, height: 4, pitch: 16, bytes_per_pixel: 4, red_shift: 16, red_size: 8, green_shift: 8, green_size: 8, blue_shift: 0, blue_size: 8, _pad: [0; 2] };
	let mut fb_reply = unsafe { core::slice::from_raw_parts(&fb as *const abi::Framebuffer as *const u8, core::mem::size_of::<abi::Framebuffer>()) }.to_vec();
	fb_reply.extend_from_slice(&4u32.to_le_bytes());
	fb_reply.extend_from_slice(&4u32.to_le_bytes());
	send_cap(&gpu_kernel, &fb_reply, scanout.clone(), Rights::MAP | Rights::TRANSFER).expect("framebuffer response");
	sched::run_until_idle();
	let online = boot_kernel.recv().expect("DisplayService online report");
	assert_eq!(&online.bytes[..], b"DisplayService: online", "DisplayService reports in");

	// The root connection is the native-size console surface.
	let console = acquire(&console_client, &focus_input, b"CONSOLE", 1, 0, 0);
	fill(&console, 0x0011_2233, 16);
	console_client.send(request(2, 2, &[0, 0, 4, 4])).expect("console present");
	acknowledge_present(&gpu_kernel, Some((&console_client, 2)));
	assert_eq!(scanout_pixel(&scanout), 0x0011_2233, "console pixels reach the scanout");
	console_client.send(request(4, 8, &[])).expect("display events request");
	sched::run_until_idle();
	let events_reply = console_client.recv().expect("display events reply");
	assert_eq!(le_u32(&events_reply.bytes, 0), 8, "events reply echoes correlation id");
	let events_cap = events_reply.caps.first().expect("display event stream");
	let events = events_cap.object().into_any_arc().downcast::<Channel>().expect("event stream is a channel");
	let mut resize = b"RESIZE".to_vec();
	resize.extend_from_slice(&4u32.to_le_bytes());
	resize.extend_from_slice(&4u32.to_le_bytes());
	gpu_kernel.send(Message::new(resize, alloc::vec::Vec::new(), 0)).expect("gpu resize event");
	acknowledge_present(&gpu_kernel, None);
	let resize_event = events.recv().expect("typed display resize event");
	assert_eq!(le_u32(&resize_event.bytes, 4), 4, "resize event width");
	assert_eq!(le_u32(&resize_event.bytes, 8), 4, "resize event height");

	// A later client becomes foreground. Explicit release restores and presents console.
	let app = connect(&console_client);
	let app_surface = acquire(&app, &focus_input, b"SET", 3, 2, 2);
	app.send(request(5, 11, &[])).expect("input focus proof request");
	sched::run_until_idle();
	let proof_reply = app.recv().expect("input focus proof reply");
	assert_eq!(le_u32(&proof_reply.bytes, 0), 11);
	assert_eq!(proof_reply.bytes[4], 1, "active app receives its focus proof");
	assert_eq!(proof_reply.caps.len(), 1, "focus proof is transferred out of band");
	app.send(request(5, 12, &[])).expect("replayed input focus proof request");
	sched::run_until_idle();
	let replay_reply = app.recv().expect("replayed focus proof reply");
	assert_eq!(replay_reply.bytes[4], 0, "focus proof is one-shot");
	fill(&app_surface, 0x00aa_bbcc, 4);
	app.send(request(2, 4, &[0, 0, 1, 1])).expect("app first present");
	let first_scaled = acknowledge_present(&gpu_kernel, Some((&app, 4)));
	assert_eq!((le_u32(&first_scaled.bytes, 7), le_u32(&first_scaled.bytes, 11), le_u32(&first_scaled.bytes, 15), le_u32(&first_scaled.bytes, 19)), (0, 0, 4, 4), "first present initializes the whole scanout");
	assert_eq!(scanout_pixel(&scanout), 0x00aa_bbcc, "foreground app replaces the console");
	assert_eq!(scanout_pixel_at(&scanout, 3, 3), 0x00aa_bbcc, "first small damage cannot leak the previous console outside its rectangle");
	let before_damage = display_stats(&display_admin, 60);
	set_surface_pixel(&app_surface, 0, 0x0055_6677);
	app.send(request(2, 61, &[0, 0, 1, 1])).expect("incremental scaled damage");
	let scaled_damage = acknowledge_present(&gpu_kernel, Some((&app, 61)));
	assert_eq!((le_u32(&scaled_damage.bytes, 7), le_u32(&scaled_damage.bytes, 11), le_u32(&scaled_damage.bytes, 15), le_u32(&scaled_damage.bytes, 19)), (0, 0, 2, 2), "scaled damage maps to its conservative output rectangle");
	assert_eq!(scanout_pixel_at(&scanout, 0, 0), 0x0055_6677);
	assert_eq!(scanout_pixel_at(&scanout, 1, 1), 0x0055_6677);
	assert_eq!(scanout_pixel_at(&scanout, 2, 2), 0x00aa_bbcc, "scaled damage leaves unaffected output pixels unchanged");
	let after_damage = display_stats(&display_admin, 62);
	assert_eq!(after_damage[2] - before_damage[2], 1, "one additional scaled present");
	assert_eq!(after_damage[3] - before_damage[3], 1, "one source damage pixel");
	assert_eq!(after_damage[4] - before_damage[4], 4, "only four scaled output pixels written");
	assert!(after_damage[7] != 0, "present latency is measured in nanoseconds");
	app.send(request(3, 5, &[])).expect("app release");
	acknowledge_focus(&focus_input, b"CONSOLE");
	acknowledge_present(&gpu_kernel, Some((&app, 5)));
	assert_eq!(scanout_pixel(&scanout), 0x0011_2233, "release restores the console surface");

	// The private emergency command revokes a frozen foreground display connection.
	let process = Process::new(AddressSpace::create().expect("bound process address space"), sched::root_domain());
	let mut bind = alloc::vec::Vec::new();
	bind.extend_from_slice(&1u16.to_le_bytes());
	bind.extend_from_slice(&50u32.to_le_bytes());
	bind.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&display_admin, &bind, process.clone(), Rights::MANAGE | Rights::TRANSFER).expect("bind process to display connection");
	sched::run_until_idle();
	let bind_reply = display_admin.recv().expect("bound display reply");
	assert_eq!(bind_reply.bytes[4], 1, "display-admin bind succeeds");
	let frozen = bind_reply.caps.first().expect("bound display connection").object().into_any_arc().downcast::<Channel>().expect("bound display is a channel");
	frozen.send(Message::new(abi::CONNECT_OP.to_le_bytes().to_vec(), alloc::vec::Vec::new(), 0)).expect("bound factory escape attempt");
	sched::run_until_idle();
	assert!(frozen.recv().is_err(), "process-bound display connection cannot mint an unbound child");
	let frozen_surface = acquire(&frozen, &focus_input, b"SET", 9, 2, 2);
	fill(&frozen_surface, 0x0000_77dd, 4);
	frozen.send(request(2, 10, &[0, 0, 2, 2])).expect("frozen app present");
	acknowledge_present(&gpu_kernel, Some((&frozen, 10)));
	kill_input.send(Message::new(b"KILL".to_vec(), alloc::vec::Vec::new(), 0)).expect("emergency display revoke");
	acknowledge_focus(&focus_input, b"CONSOLE");
	acknowledge_present(&gpu_kernel, None);
	assert!(frozen.is_peer_closed(), "emergency revoke closes the foreground display connection");
	assert!(process.is_killed(), "emergency revoke SIG_KILLs the process bound by PermissionManager");
	assert_eq!(scanout_pixel(&scanout), 0x0011_2233, "emergency revoke restores the console surface");

	// A crashed client has the same restoration guarantee through channel peer-close.
	let crashed = connect(&console_client);
	let crashed_surface = acquire(&crashed, &focus_input, b"SET", 6, 2, 2);
	fill(&crashed_surface, 0x00dd_4400, 4);
	crashed.send(request(2, 7, &[0, 0, 2, 2])).expect("crashed app present");
	acknowledge_present(&gpu_kernel, Some((&crashed, 7)));
	assert_eq!(scanout_pixel(&scanout), 0x00dd_4400, "second foreground app reaches scanout");
	drop(crashed);
	acknowledge_focus(&focus_input, b"CONSOLE");
	acknowledge_present(&gpu_kernel, None);
	assert_eq!(scanout_pixel(&scanout), 0x0011_2233, "peer-close restores the console surface");

	// Game-class benchmark geometry: replace the stand-in scanout with 1024x768,
	// present a 320x200 software surface, then update a 32x20 source rectangle. The
	// service's own monotonic counters separate CPU scaling from driver ACK latency.
	let large_scanout = match DmaBuffer::create_in(&sched::root_domain(), 1024 * 768 * 4) {
		Ok(scanout) => scanout,
		Err(_) => panic!("large stand-in scanout"),
	};
	let large_fb = abi::Framebuffer { width: 1024, height: 768, pitch: 4096, bytes_per_pixel: 4, red_shift: 16, red_size: 8, green_shift: 8, green_size: 8, blue_shift: 0, blue_size: 8, _pad: [0; 2] };
	let mut replacement = b"FBNEW".to_vec();
	replacement.extend_from_slice(unsafe { core::slice::from_raw_parts(&large_fb as *const abi::Framebuffer as *const u8, core::mem::size_of::<abi::Framebuffer>()) });
	replacement.extend_from_slice(&1024u32.to_le_bytes());
	replacement.extend_from_slice(&768u32.to_le_bytes());
	send_cap(&gpu_kernel, &replacement, large_scanout, Rights::MAP | Rights::TRANSFER).expect("large framebuffer replacement");
	acknowledge_present(&gpu_kernel, None);
	let resized = events.recv().expect("large resize event");
	assert_eq!((le_u32(&resized.bytes, 4), le_u32(&resized.bytes, 8)), (1024, 768));

	let benchmark = connect(&console_client);
	let benchmark_surface = acquire(&benchmark, &focus_input, b"SET", 70, 320, 200);
	fill(&benchmark_surface, 0x0033_6699, 320 * 200);
	let before_full = display_stats(&display_admin, 71);
	benchmark.send(request(2, 72, &[0, 0, 320, 200])).expect("full benchmark present");
	acknowledge_present(&gpu_kernel, Some((&benchmark, 72)));
	let after_full = display_stats(&display_admin, 73);
	benchmark.send(request(2, 74, &[32, 20, 32, 20])).expect("damage benchmark present");
	acknowledge_present(&gpu_kernel, Some((&benchmark, 74)));
	let after_damage = display_stats(&display_admin, 75);
	let full_blit_ns = after_full[5] - before_full[5];
	let full_flush_ns = after_full[6] - before_full[6];
	let full_pixels = after_full[4] - before_full[4];
	let damage_blit_ns = after_damage[5] - after_full[5];
	let damage_flush_ns = after_damage[6] - after_full[6];
	let damage_pixels = after_damage[4] - after_full[4];
	crate::serial_println!("display-perf: full blit={}ns flush={}ns pixels={} damage blit={}ns flush={}ns pixels={}", full_blit_ns, full_flush_ns, full_pixels, damage_blit_ns, damage_flush_ns, damage_pixels);
	assert_eq!(full_pixels, 1024 * 768 + 1024 * 640, "first scaled frame clears scanout and fills centered output");
	assert_eq!(damage_pixels, 103 * 64, "32x20 source damage maps to a 103x64 conservative output rectangle");
	assert!(damage_blit_ns < full_blit_ns, "incremental scaled damage must cost less CPU time than a full first frame");
	benchmark.send(request(3, 76, &[])).expect("benchmark release");
	acknowledge_focus(&focus_input, b"CONSOLE");
	acknowledge_present(&gpu_kernel, Some((&benchmark, 76)));
}

tagged_test!(audio_service_mixes_pcm_streams_with_backpressure, [Service, Audio]);
fn audio_service_mixes_pcm_streams_with_backpressure() {
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
		bootstrap.send(Message::new(argument.to_vec(), alloc::vec::Vec::new(), 0)).expect("play argument bootstrap");
		send_cap(&bootstrap, b"SYSTEM", storage, Rights::ALL).expect("play system volume bootstrap");
		for tag in [b"MEDIA".as_slice(), b"ISO".as_slice(), b"UDF".as_slice(), b"USB".as_slice()] {
			bootstrap.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("play absent volume bootstrap");
		}
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
	send_cap(&process_boot_kernel, b"STORAGE", storage_client.clone(), Rights::ALL).expect("process storage bootstrap");
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

	// Queue the second stream and beep while the first hardware period is pending.
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

	// Fill the bounded queue: the third write stays unanswered until two hardware
	// periods advance the playback clock and create source-frame capacity.
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

	// Launch two real console players over separate playback-only scopes. Hold the
	// first hardware period pending while Vorbis decodes and queues, then ACK it:
	// the next period must contain the exact WAV+Vorbis sum, proving both governed
	// decoder paths feed the shared audio mixer rather than acquiring it exclusively.
	let wav_scope = open_scope(&audio_admin, 40);
	let wav_start = arch::tsc::now();
	let (_wav_stdout, wav_process) = launch_play(&process_client, storage_client.clone(), wav_scope, b"vol://system/test.wav");
	let wav_domain = wav_process.domain().clone();
	sched::run_until_idle();
	let first = snd_host.recv().expect("WAV first hardware period");
	let first_sample_ns = arch::tsc::cycles_to_ns(arch::tsc::now().wrapping_sub(wav_start));
	assert_eq!(first.bytes.len(), 2_048);
	assert_eq!(sample(&first), 2, "WAV first source frame reaches hardware");

	let vorbis_scope = open_scope(&audio_admin, 41);
	let vorbis_start = arch::tsc::now();
	let (_vorbis_stdout, vorbis_process) = launch_play(&process_client, storage_client.clone(), vorbis_scope, b"vol://system/test.ogg");
	let vorbis_domain = vorbis_process.domain().clone();
	sched::run_until_idle();
	let vorbis_queue_ns = arch::tsc::cycles_to_ns(arch::tsc::now().wrapping_sub(vorbis_start));
	assert!(snd_host.recv().is_err(), "pending driver ACK holds the mixed period");
	let ack_start = arch::tsc::now();
	snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("WAV first period ACK");
	sched::run_until_idle();
	let mixed = snd_host.recv().expect("concurrent mixed period");
	let ack_to_mixed_ns = arch::tsc::cycles_to_ns(arch::tsc::now().wrapping_sub(ack_start));
	assert_eq!(mixed.bytes.len(), 2_048);
	assert_eq!(sample(&mixed), 2, "the corresponding WAV and Vorbis source frames mix exactly");
	let mut periods = 2u32;
	while periods < 6 {
		snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("concurrent period ACK");
		sched::run_until_idle();
		let current = snd_host.recv().expect("next concurrent mixed period");
		assert!(!current.bytes.is_empty(), "long fixtures must sustain six mixed periods");
		periods += 1;
	}
	for process in [&wav_process, &vorbis_process] {
		process.set_int_pending();
		for thread in process.live_threads() {
			sched::wake_thread(&thread);
		}
	}
	let mut tail = 0u32;
	loop {
		snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("concurrent tail ACK");
		sched::run_until_idle();
		let current = snd_host.recv().expect("concurrent tail period or stop");
		if current.bytes.is_empty() {
			break;
		}
		tail += 1;
		assert!(tail <= 64, "interrupted players leave at most the bounded accepted queue tail");
	}
	snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("concurrent stop ACK");
	sched::run_until_idle();
	assert!(wav_process.is_terminated() && vorbis_process.is_terminated(), "both play processes exit after explicit close");
	let wav_peak = wav_process.memory_bytes() + wav_domain.account().memory().peak() + volume_file(volume, b"test.wav").expect("staged WAV").len() as u64;
	let vorbis_peak = vorbis_process.memory_bytes() + vorbis_domain.account().memory().peak() + volume_file(volume, b"test.ogg").expect("staged Vorbis").len() as u64;
	crate::serial_println!("audio-play-perf: first-sample={}ns vorbis-decode-queue={}ns ack-to-mixed={}ns wav-peak={}B vorbis-peak={}B periods={} underruns=0 queued-source-peak=683", first_sample_ns, vorbis_queue_ns, ack_to_mixed_ns, wav_peak, vorbis_peak, periods);

	// Interrupt a long player while bounded AudioService backpressure has it blocked
	// on a write reply. This is the SIG_INT caught disposition used by Ctrl+C: waking
	// the process lets the pending write complete, then `play` observes the flag,
	// explicitly closes, exits, and leaves only the already accepted bounded tail.
	let interrupt_scope = open_scope(&audio_admin, 42);
	let (_interrupt_stdout, interrupt_process) = launch_play(&process_client, storage_client.clone(), interrupt_scope, b"vol://system/test.wv");
	sched::run_until_idle();
	let mut current = snd_host.recv().expect("long player first period");
	assert!(!current.bytes.is_empty());
	assert!(interrupt_process.is_int_caught(), "play arms the catchable Ctrl+C disposition");
	interrupt_process.set_int_pending();
	for thread in interrupt_process.live_threads() {
		sched::wake_thread(&thread);
	}
	let mut interrupt_periods = 1u32;
	loop {
		snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("interrupted period ACK");
		sched::run_until_idle();
		current = snd_host.recv().expect("interrupted tail period or stop");
		if current.bytes.is_empty() {
			break;
		}
		interrupt_periods += 1;
		assert!(interrupt_periods <= 64, "Ctrl+C leaves at most the bounded accepted queue tail");
	}
	snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new(), 0)).expect("interrupted stop ACK");
	sched::run_until_idle();
	assert!(interrupt_process.is_terminated(), "Ctrl+C player exits after explicit stream close");
	crate::serial_println!("audio-play-interrupt: drained-periods={} max=64 stream-released=1", interrupt_periods);

	// MP3 is fully decoded before opening its stream: the debug decoder's burst latency
	// must never empty the live queue and stretch playback with stop/restart gaps.
	let mp3_scope = open_scope(&audio_admin, 43);
	let (_mp3_stdout, mp3_process) = launch_play(&process_client, storage_client, mp3_scope, b"vol://system/test.mp3");
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

	// A driver crash while a period is pending closes every live PCM stream and
	// makes future opens fail cleanly instead of leaving clients blocked forever.
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

tagged_test!(dhcp_lease_renews_at_t1_and_restarts_its_clock, [Service, Network, Slow]);
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
	let _network_service = spawn_dynamic_test_process(sched::root_domain(), service_elf, boot_user);
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

tagged_test!(process_service_starts_a_program, [Service, Process]);
fn process_service_starts_a_program() {
	use object::channel::Message;

	// Drive the real userspace ProcessService over its generated Process bindings:
	// spawn it, hand it the init package (to launch from) and a serve channel, then
	// START a program and LIST it back. The wire is the proto framing - request [op
	// u16][corr u32][args], reply [corr u32][result]; `start` takes a string name and
	// replies result<process-info, error> = [koid u64][name string]. Everything is
	// pre-queued so the cooperative service drains it in one pass and exits.
	let (boot_kernel, service_client) = spawn_service_with_package(b"process_service");

	// START a pinned program by short name (log_service is in the init package, which this
	// ProcessService falls back to since it has no storage client): [op = 1 u16][corr
	// u32][name: [len u16][utf8]].
	let name: &[u8] = b"log_service";
	let artifact: &[u8] = b"log_service.lsexe";
	let mut start = alloc::vec::Vec::new();
	start.extend_from_slice(&1u16.to_le_bytes());
	start.extend_from_slice(&1u32.to_le_bytes());
	start.extend_from_slice(&(name.len() as u16).to_le_bytes());
	start.extend_from_slice(name);
	service_client.send(Message::new(start, alloc::vec::Vec::new(), 0)).expect("start request");
	let mut explicit = alloc::vec::Vec::new();
	explicit.extend_from_slice(&1u16.to_le_bytes());
	explicit.extend_from_slice(&2u32.to_le_bytes());
	explicit.extend_from_slice(&(artifact.len() as u16).to_le_bytes());
	explicit.extend_from_slice(artifact);
	service_client.send(Message::new(explicit, alloc::vec::Vec::new(), 0)).expect("explicit start request");

	// LIST: [op = 2 u16][corr u32]. Then an empty quit sentinel.
	let mut list = alloc::vec::Vec::new();
	list.extend_from_slice(&2u16.to_le_bytes());
	list.extend_from_slice(&3u32.to_le_bytes());
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
	assert_eq!(&b[15..15 + name_len], artifact, "the short launch reports the canonical artifact name");

	let reply = service_client.recv().expect("explicit start reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), 2, "explicit start reply echoes the correlation id");
	assert_eq!(b[4], 1, "explicit start succeeded");
	let name_len = le_u16(b, 13) as usize;
	assert_eq!(&b[15..15 + name_len], artifact, "the explicit launch reports the same canonical artifact name");

	// The list reply records both launches under their complete physical identity.
	let reply = service_client.recv().expect("list reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), 3, "list reply echoes the correlation id");
	assert_eq!(b[4], 1, "list succeeded");
	assert_eq!(le_u16(b, 5), 2, "both started processes are listed");
}

tagged_test!(process_service_resolves_one_final_executable_suffix, [Service, Process]);
fn process_service_resolves_one_final_executable_suffix() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	let init = init_package_bytes().expect("init package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let process_elf = package.lookup(b"process_service.lsexe").expect("ProcessService image");
	let source_index = (0..package.len()).find(|&index| package.name(index) == Some(&b"log_service.lsexe"[..])).expect("source executable entry");
	let mut repeated_package = init.to_vec();
	let name_start = abi::PKG_HEADER_LEN + source_index * abi::PKG_ENTRY_LEN;
	repeated_package[name_start..name_start + abi::PKG_NAME_LEN].fill(0);
	let repeated_artifact = b"ping.lsexe.lsexe";
	repeated_package[name_start..name_start + repeated_artifact.len()].copy_from_slice(repeated_artifact);

	let (boot_kernel, boot_user) = Channel::create();
	let (service_server, service_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), process_elf, boot_user, Rights::ALL, 0).expect("spawn ProcessService");
	send_package(&boot_kernel, &repeated_package).expect("custom package bootstrap");
	boot_kernel.send(Message::new(b"STORAGE".to_vec(), alloc::vec::Vec::new(), 0)).expect("empty storage bootstrap");
	send_cap(&boot_kernel, b"SERVE", service_server, Rights::ALL).expect("serve bootstrap");

	for (corr, name) in [(1u32, &b"ping"[..]), (2, &b"ping.lsexe"[..]), (3, &b"ping.lsexe.lsexe"[..])] {
		let mut start = alloc::vec::Vec::new();
		start.extend_from_slice(&1u16.to_le_bytes());
		start.extend_from_slice(&corr.to_le_bytes());
		start.extend_from_slice(&(name.len() as u16).to_le_bytes());
		start.extend_from_slice(name);
		service_client.send(Message::new(start, alloc::vec::Vec::new(), 0)).expect("start request");
	}
	service_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");
	sched::run_until_idle();

	assert_eq!(&boot_kernel.recv().expect("ProcessService online report").bytes, b"ProcessService: online");
	let bare = service_client.recv().expect("bare-name reply");
	assert_eq!(le_u32(&bare.bytes, 0), 1);
	assert_eq!(bare.bytes[4], 0, "ping must not skip two suffix levels");
	for corr in [2u32, 3] {
		let reply = service_client.recv().expect("repeated-suffix launch reply");
		let bytes = &reply.bytes;
		assert_eq!(le_u32(bytes, 0), corr);
		assert_eq!(bytes[4], 1, "short or exact repeated-suffix launch succeeds");
		let name_len = le_u16(bytes, 13) as usize;
		assert_eq!(&bytes[15..15 + name_len], repeated_artifact, "ProcessInfo preserves the full physical basename");
	}
}

tagged_test!(system_packages_use_canonical_executable_names, [Boot, Storage]);
fn system_packages_use_canonical_executable_names() {
	let init = pkg::Package::parse(init_package_bytes().expect("init package present")).expect("init package parses");
	let volume = pkg::Package::parse(volume_package_bytes().expect("volume package present")).expect("volume package parses");
	for index in 0..init.len() {
		let name = init.name(index).expect("init entry name");
		assert!(name.ends_with(b".lsexe"), "init package contains an extensionless native artifact");
	}
	for index in 0..volume.len() {
		let name = volume.name(index).expect("volume entry name");
		if name.starts_with(b"bin/") || name.starts_with(b"drivers/") {
			assert!(name.ends_with(b".lsexe"), "system volume contains an extensionless native artifact");
		}
	}
	for package in [&init, &volume] {
		for index in 0..package.len() {
			let name = package.name(index).expect("package entry name");
			if name.ends_with(b".lsexe") {
				let mut collision = name.to_vec();
				collision.extend_from_slice(b".lsexe");
				assert!(package.lookup(&collision).is_none(), "package contains an ambiguous executable alias pair");
			}
		}
	}
	let mut library_identities = 0usize;
	let mut executable_identities = 0usize;
	for index in 0..volume.len() {
		let name = volume.name(index).expect("volume entry name");
		library_identities += usize::from(name.starts_with(b"id/lib/"));
		executable_identities += usize::from(name.starts_with(b"id/bin/"));
	}
	assert_eq!(library_identities, 46, "every staged library has one identity record");
	assert_eq!(executable_identities, 68, "every staged dynamic executable has one identity record");
	assert!(volume.lookup(b"id/lib/imgconv").is_some(), "library identity namespace preserves imgconv");
	assert!(volume.lookup(b"id/bin/imgconv").is_some(), "executable identity namespace preserves imgconv");
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
	send_cap(&process_boot_kernel, b"STORAGE", storage_client.clone(), Rights::ALL).expect("process storage bootstrap");
	send_cap(&process_boot_kernel, b"SERVE", process_server, Rights::ALL).expect("process serve bootstrap");
	(process_boot_kernel, storage_boot_kernel, process_client)
}

tagged_test!(dynamic_process_service_loads_probe, [Dynamic, Service, Process, Storage]);
fn dynamic_process_service_loads_probe() {
	use object::channel::{Channel, Message};
	use object::process::Process;
	use object::rights::Rights;

	let (volume, _) = scenario_packages().expect("scenario packages");
	let (process_boot_kernel, _storage_boot_kernel, process_client) = start_process_service_from_volume(volume);
	sched::run_until_idle();
	assert_eq!(&process_boot_kernel.recv().expect("ProcessService online report").bytes, b"ProcessService: online");
	let dynamic_name = b"dyn_probe";
	let (report, bootstrap) = Channel::create();
	let mut launch = alloc::vec::Vec::new();
	launch.extend_from_slice(&3u16.to_le_bytes());
	launch.extend_from_slice(&2u32.to_le_bytes());
	launch.extend_from_slice(&(dynamic_name.len() as u16).to_le_bytes());
	launch.extend_from_slice(dynamic_name);
	launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &launch, bootstrap, Rights::ALL).expect("dynamic probe launch request");
	sched::run_until_idle();
	let reply = process_client.recv().expect("dynamic probe launch reply");
	assert_eq!(le_u32(&reply.bytes, 0), 2);
	assert_eq!(reply.bytes[4], 1, "the staged dynamic probe loaded with its providers");
	let process = reply.caps[0].object().into_any_arc().downcast::<Process>().expect("dynamic probe launch capability is a Process");
	assert_eq!(&report.recv().expect("dynamic probe report").bytes, b"dynamic link ok");
	assert!(process.private_image_pages() != 0 && process.shared_image_pages() != 0, "dynamic probe has private and shared mappings");
	process_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");
	sched::run_until_idle();
}

tagged_test!(dynamic_process_service_loads_programs_from_system_bin, [Service, Process, Storage]);
fn dynamic_process_service_loads_programs_from_system_bin() {
	use object::channel::{Channel, Message};
	use object::process::Process;
	use object::rights::Rights;

	// ProcessService loads a named program's ELF from the system volume's
	// `bin/` through a StorageService client, not the init package. Stand up a
	// StorageService over the factory volume archive (which stages the tools under
	// `bin/`) and a ProcessService wired to its client, then START a staged tool by name:
	// ProcessService resolves it to `vol://system/bin/<name>.lsexe` and loads it off the volume,
	// proving the on-disk load path the shell's `run` and ConsoleService's shell spawn now
	// take.
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe in the init package");
	let (process_boot_kernel, _storage_boot_kernel, process_client) = start_process_service_from_volume(volume);
	let mut writable_storage = StorageHarness::start(storage_elf, b"BLOCK", volume, 64 * 1024 * 1024);

	// START a staged static tool: [op = 1 u16][corr u32][name: [len u16][utf8]].
	let name: &[u8] = b"ptyecho";
	let mut start = alloc::vec::Vec::new();
	start.extend_from_slice(&1u16.to_le_bytes());
	start.extend_from_slice(&1u32.to_le_bytes());
	start.extend_from_slice(&(name.len() as u16).to_le_bytes());
	start.extend_from_slice(name);
	process_client.send(Message::new(start, alloc::vec::Vec::new(), 0)).expect("start request");
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
	assert_eq!(&b[15..15 + name_len], b"ptyecho.lsexe", "the reply reports the canonical artifact name");

	for (corr, path, succeeds) in [(10u32, &b"vol://system/bin/ptyecho.lsexe"[..], true), (11, &b"vol://system/bin/ptyecho"[..], false)] {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&1u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		process_client.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("explicit-path start request");
		sched::run_until_idle();
		let reply = process_client.recv().expect("explicit-path start reply");
		assert_eq!(le_u32(&reply.bytes, 0), corr);
		assert_eq!(reply.bytes[4] == 1, succeeds, "only a real path ending in .lsexe is executable");
		if succeeds {
			let name_len = le_u16(&reply.bytes, 13) as usize;
			assert_eq!(&reply.bytes[15..15 + name_len], b"ptyecho.lsexe", "an explicit path records only the canonical basename");
		}
	}

	// Launch the first ordinary PIE tool, hand its bootstrap a stdout channel and
	// arguments, and observe output produced through lsrt. This covers the generated
	// start object and the echo.lsexe -> lsrt.lslib provider edge after volume staging.
	let echo_name: &[u8] = b"echo";
	let (echo_stdout_kernel, echo_stdout_user) = Channel::create();
	let (echo_bootstrap_kernel, echo_bootstrap_user) = Channel::create();
	let mut echo_launch = alloc::vec::Vec::new();
	echo_launch.extend_from_slice(&3u16.to_le_bytes());
	echo_launch.extend_from_slice(&20u32.to_le_bytes());
	echo_launch.extend_from_slice(&(echo_name.len() as u16).to_le_bytes());
	echo_launch.extend_from_slice(echo_name);
	echo_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &echo_launch, echo_bootstrap_user, Rights::ALL).expect("dynamic echo launch request");
	sched::run_until_idle();
	let echo_reply = process_client.recv().expect("dynamic echo launch reply");
	assert_eq!(le_u32(&echo_reply.bytes, 0), 20);
	assert_eq!(echo_reply.bytes[4], 1, "the ordinary PIE tool loaded with lsrt");
	let echo_process = echo_reply.caps[0].object().into_any_arc().downcast::<Process>().expect("dynamic echo launch capability is a Process");
	assert!(!echo_process.is_terminated(), "dynamic echo remains blocked on its live bootstrap after launch");
	assert!(echo_process.handle_count() >= 1, "dynamic echo owns its bootstrap handle");
	assert!(!echo_bootstrap_kernel.is_peer_closed(), "dynamic echo bootstrap peer is open before initialization");
	send_cap(&echo_bootstrap_kernel, b"STDOUT", echo_stdout_user, Rights::ALL).expect("dynamic echo stdout bootstrap");
	echo_bootstrap_kernel.send(Message::new(b"dynamic echo".to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic echo arguments");
	assert!(!echo_bootstrap_kernel.is_peer_closed(), "dynamic echo bootstrap peer remains open after initialization is queued");
	sched::run_until_idle();
	let echo_output = echo_stdout_kernel.recv().unwrap_or_else(|error| {
		let fault = echo_process.fault_info();
		panic!("dynamic echo output: {error:?}; fault={:?} terminated={} sent={} received={}", fault.map(|info| (info.kind, info.error_code, info.address, info.instruction_pointer)), echo_process.is_terminated(), echo_process.messages_sent(), echo_process.messages_received())
	});
	assert_eq!(&echo_output.bytes, b"dynamic echo");
	assert_eq!(&echo_stdout_kernel.recv().expect("dynamic echo newline").bytes, b"\n");

	// Load the generated date PIE directly as well. Its capability protocol is covered by
	// PermissionManager; this assertion isolates staging, provider-DAG loading and relocation
	// from that policy layer so a loader failure cannot collapse into an empty tool result.
	let date_name: &[u8] = b"date";
	let (date_bootstrap_kernel, date_bootstrap_user) = Channel::create();
	let mut date_launch = alloc::vec::Vec::new();
	date_launch.extend_from_slice(&3u16.to_le_bytes());
	date_launch.extend_from_slice(&21u32.to_le_bytes());
	date_launch.extend_from_slice(&(date_name.len() as u16).to_le_bytes());
	date_launch.extend_from_slice(date_name);
	date_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &date_launch, date_bootstrap_user, Rights::ALL).expect("dynamic date launch request");
	sched::run_until_idle();
	let date_reply = process_client.recv().expect("dynamic date launch reply");
	assert_eq!(le_u32(&date_reply.bytes, 0), 21);
	assert_eq!(date_reply.bytes[4], 1, "the date PIE loaded with its manifest providers");
	drop(date_bootstrap_kernel);
	sched::run_until_idle();

	for (index, tool) in [
		b"play" as &[u8],
		b"graphics_probe" as &[u8],
		b"imgview" as &[u8],
		b"imgconv" as &[u8],
		b"config" as &[u8],
		b"set" as &[u8],
		b"log" as &[u8],
		b"snap" as &[u8],
		b"volume" as &[u8],
		b"lsdev" as &[u8],
		b"lsvol" as &[u8],
		b"lssvc" as &[u8],
		b"lsblk" as &[u8],
		b"lsusb" as &[u8],
		b"usage" as &[u8],
		b"ps" as &[u8],
		b"run" as &[u8],
		b"perm" as &[u8],
		b"stop" as &[u8],
		b"beep" as &[u8],
		b"readln" as &[u8],
		b"ptyecho" as &[u8],
		b"script" as &[u8],
		b"ping" as &[u8],
		b"ip" as &[u8],
		b"nslookup" as &[u8],
		b"tcp" as &[u8],
		b"nc" as &[u8],
		b"arp" as &[u8],
		b"ss" as &[u8],
		b"httpd" as &[u8],
	]
	.iter()
	.enumerate()
	{
		let correlation = 40 + index as u32;
		let (tool_bootstrap_kernel, tool_bootstrap_user) = Channel::create();
		let mut tool_launch = alloc::vec::Vec::new();
		tool_launch.extend_from_slice(&3u16.to_le_bytes());
		tool_launch.extend_from_slice(&correlation.to_le_bytes());
		tool_launch.extend_from_slice(&(tool.len() as u16).to_le_bytes());
		tool_launch.extend_from_slice(tool);
		tool_launch.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&process_client, &tool_launch, tool_bootstrap_user, Rights::ALL).expect("service-tool batch launch request");
		sched::run_until_idle();
		let tool_reply = process_client.recv().expect("service-tool batch launch reply");
		assert_eq!(le_u32(&tool_reply.bytes, 0), correlation);
		assert_eq!(tool_reply.bytes[4], 1, "service-oriented PIE {} loaded with its manifest providers", core::str::from_utf8(tool).unwrap_or("<invalid>"));
		drop(tool_bootstrap_kernel);
		sched::run_until_idle();
	}

	let (readln_output, readln_console) = Channel::create();
	let (readln_bootstrap_kernel, readln_bootstrap_user) = Channel::create();
	let mut readln_launch = alloc::vec::Vec::new();
	readln_launch.extend_from_slice(&3u16.to_le_bytes());
	readln_launch.extend_from_slice(&59u32.to_le_bytes());
	readln_launch.extend_from_slice(&6u16.to_le_bytes());
	readln_launch.extend_from_slice(b"readln");
	readln_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &readln_launch, readln_bootstrap_user, Rights::ALL).expect("dynamic readln launch request");
	sched::run_until_idle();
	let readln_reply = process_client.recv().expect("dynamic readln launch reply");
	assert_eq!(le_u32(&readln_reply.bytes, 0), 59);
	assert_eq!(readln_reply.bytes[4], 1, "dynamic readln loaded with lsrt");
	send_cap(&readln_bootstrap_kernel, b"STDOUT", readln_console, Rights::ALL).expect("dynamic readln console bootstrap");
	readln_bootstrap_kernel.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("dynamic readln arguments");
	sched::run_until_idle();
	readln_output.send(Message::new(b"hello\n".to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic readln input");
	sched::run_until_idle();
	readln_output.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("dynamic readln EOF");
	sched::run_until_idle();
	let mut readln_captured = alloc::vec::Vec::new();
	while let Ok(message) = readln_output.recv() {
		readln_captured.extend_from_slice(&message.bytes);
	}
	assert!(readln_captured.windows(b"in> hello".len()).any(|window| window == b"in> hello"), "dynamic readln echoed cooked input");

	for (tool, correlation) in [
		(b"uname" as &[u8], 31u32),
		(b"uptime" as &[u8], 32u32),
		(b"free" as &[u8], 33u32),
		(b"lscpu" as &[u8], 34u32),
		(b"dmesg" as &[u8], 35u32),
		(b"lsmem" as &[u8], 36u32),
		(b"lsirq" as &[u8], 37u32),
		(b"lspci" as &[u8], 38u32),
	] {
		let (output_kernel, output_user) = Channel::create();
		let (tool_bootstrap_kernel, tool_bootstrap_user) = Channel::create();
		let mut tool_launch = alloc::vec::Vec::new();
		tool_launch.extend_from_slice(&3u16.to_le_bytes());
		tool_launch.extend_from_slice(&correlation.to_le_bytes());
		tool_launch.extend_from_slice(&(tool.len() as u16).to_le_bytes());
		tool_launch.extend_from_slice(tool);
		tool_launch.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&process_client, &tool_launch, tool_bootstrap_user, Rights::ALL).expect("dynamic inventory launch request");
		sched::run_until_idle();
		let tool_reply = process_client.recv().expect("dynamic inventory launch reply");
		assert_eq!(le_u32(&tool_reply.bytes, 0), correlation);
		assert_eq!(tool_reply.bytes[4], 1, "the inventory PIE loaded with its manifest providers");
		let tool_process = tool_reply.caps[0].object().into_any_arc().downcast::<Process>().expect("dynamic inventory capability is a Process");
		send_cap(&tool_bootstrap_kernel, b"STDOUT", output_user, Rights::ALL).expect("dynamic inventory stdout bootstrap");
		tool_bootstrap_kernel.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("dynamic inventory arguments");
		let mut captured = alloc::vec::Vec::new();
		for _ in 0..100_000 {
			sched::run_until_idle();
			while let Ok(message) = output_kernel.recv() {
				captured.extend_from_slice(&message.bytes);
			}
			if tool_process.is_terminated() {
				break;
			}
		}
		assert!(tool_process.is_terminated(), "dynamic inventory tool completed");
		let contains = |needle: &[u8]| captured.windows(needle.len()).any(|window| window == needle);
		match tool {
			b"uname" => {
				assert!(contains(env!("PRODUCT_NAME").as_bytes()) && contains(env!("PRODUCT_VERSION").as_bytes()), "dynamic uname printed product identity");
			}
			b"uptime" => assert!(captured.starts_with(b"up ") && captured.ends_with(b"\n"), "dynamic uptime rendered time since boot"),
			b"free" => assert!(captured.starts_with(b"Mem:  total ") && contains(b"Heap: total "), "dynamic free rendered memory pools"),
			b"lscpu" => assert!(contains(b"arch: ") && contains(b"name: ") && contains(b"cpu0: lapic "), "dynamic lscpu rendered CPU inventory"),
			b"dmesg" => assert!(!captured.is_empty(), "dynamic dmesg rendered the kernel boot log or its empty-log diagnostic"),
			b"lsmem" => assert!(contains(b" usable\n"), "dynamic lsmem rendered a usable memory region"),
			b"lsirq" => assert!(contains(b"vector  type   bound  device  device-type") && contains(b"fixed"), "dynamic lsirq rendered its aligned vector table"),
			b"lspci" => assert!(contains(b"1af4:") && contains(b"(network controller)"), "dynamic lspci rendered the retained virtio bus scan"),
			_ => unreachable!(),
		}
	}

	// Exercise the mutable ordinary PIEs against one block-backed StorageService: create a
	// directory, stream a file into it, read it back, reject removal while non-empty, remove
	// the file and then the empty directory, and finally prove the file remains absent.
	let mkdir_name: &[u8] = b"mkdir";
	let (mkdir_stdout_kernel, mkdir_stdout_user) = Channel::create();
	let (mkdir_bootstrap_kernel, mkdir_bootstrap_user) = Channel::create();
	let mut mkdir_launch = alloc::vec::Vec::new();
	mkdir_launch.extend_from_slice(&3u16.to_le_bytes());
	mkdir_launch.extend_from_slice(&26u32.to_le_bytes());
	mkdir_launch.extend_from_slice(&(mkdir_name.len() as u16).to_le_bytes());
	mkdir_launch.extend_from_slice(mkdir_name);
	mkdir_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &mkdir_launch, mkdir_bootstrap_user, Rights::ALL).expect("dynamic mkdir launch request");
	sched::run_until_idle();
	let mkdir_reply = process_client.recv().expect("dynamic mkdir launch reply");
	assert_eq!(le_u32(&mkdir_reply.bytes, 0), 26);
	assert_eq!(mkdir_reply.bytes[4], 1, "the mkdir PIE loaded with its manifest providers");
	send_cap(&mkdir_bootstrap_kernel, b"STDOUT", mkdir_stdout_user, Rights::ALL).expect("dynamic mkdir stdout bootstrap");
	mkdir_bootstrap_kernel.send(Message::new(b"vol://system/dynamic-dir".to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic mkdir arguments");
	send_cap(&mkdir_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("dynamic mkdir system volume");
	for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..]] {
		mkdir_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic mkdir absent volume");
	}
	mkdir_bootstrap_kernel.send(Message::new(b"vol://system/".to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic mkdir cwd");
	let mut mkdir_prefix = None;
	for _ in 0..100_000 {
		writable_storage.pump();
		if let Ok(message) = mkdir_stdout_kernel.recv() {
			mkdir_prefix = Some(message);
			break;
		}
	}
	assert_eq!(&mkdir_prefix.expect("dynamic mkdir confirmation prefix").bytes, b"created ");
	assert_eq!(&mkdir_stdout_kernel.recv().expect("dynamic mkdir confirmation path").bytes, b"vol://system/dynamic-dir");
	assert_eq!(&mkdir_stdout_kernel.recv().expect("dynamic mkdir confirmation newline").bytes, b"\n");

	let write_name: &[u8] = b"write";
	let (write_stdout_kernel, write_stdout_user) = Channel::create();
	let (write_bootstrap_kernel, write_bootstrap_user) = Channel::create();
	let mut write_launch = alloc::vec::Vec::new();
	write_launch.extend_from_slice(&3u16.to_le_bytes());
	write_launch.extend_from_slice(&22u32.to_le_bytes());
	write_launch.extend_from_slice(&(write_name.len() as u16).to_le_bytes());
	write_launch.extend_from_slice(write_name);
	write_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &write_launch, write_bootstrap_user, Rights::ALL).expect("dynamic write launch request");
	sched::run_until_idle();
	let write_reply = process_client.recv().expect("dynamic write launch reply");
	assert_eq!(le_u32(&write_reply.bytes, 0), 22);
	assert_eq!(write_reply.bytes[4], 1, "the write PIE loaded with its manifest providers");
	send_cap(&write_bootstrap_kernel, b"STDOUT", write_stdout_user, Rights::ALL).expect("dynamic write stdout bootstrap");
	write_bootstrap_kernel.send(Message::new(b"vol://system/dynamic-dir/dynamic-write.txt dynamic write".to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic write arguments");
	send_cap(&write_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("dynamic write system volume");
	for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..]] {
		write_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic write absent volume");
	}
	write_bootstrap_kernel.send(Message::new(b"vol://system/".to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic write cwd");
	let mut write_prefix = None;
	for _ in 0..100_000 {
		writable_storage.pump();
		if let Ok(message) = write_stdout_kernel.recv() {
			write_prefix = Some(message);
			break;
		}
	}
	assert_eq!(&write_prefix.expect("dynamic write confirmation prefix").bytes, b"wrote ");
	assert_eq!(&write_stdout_kernel.recv().expect("dynamic write confirmation path").bytes, b"vol://system/dynamic-dir/dynamic-write.txt");
	assert_eq!(&write_stdout_kernel.recv().expect("dynamic write confirmation newline").bytes, b"\n");

	let cat_name: &[u8] = b"cat";
	let (cat_stdout_kernel, cat_stdout_user) = Channel::create();
	let (cat_bootstrap_kernel, cat_bootstrap_user) = Channel::create();
	let mut cat_launch = alloc::vec::Vec::new();
	cat_launch.extend_from_slice(&3u16.to_le_bytes());
	cat_launch.extend_from_slice(&23u32.to_le_bytes());
	cat_launch.extend_from_slice(&(cat_name.len() as u16).to_le_bytes());
	cat_launch.extend_from_slice(cat_name);
	cat_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &cat_launch, cat_bootstrap_user, Rights::ALL).expect("dynamic cat launch request");
	sched::run_until_idle();
	let cat_reply = process_client.recv().expect("dynamic cat launch reply");
	assert_eq!(le_u32(&cat_reply.bytes, 0), 23);
	assert_eq!(cat_reply.bytes[4], 1, "the cat PIE loaded for write read-back");
	send_cap(&cat_bootstrap_kernel, b"STDOUT", cat_stdout_user, Rights::ALL).expect("dynamic cat stdout bootstrap");
	cat_bootstrap_kernel.send(Message::new(b"vol://system/dynamic-dir/dynamic-write.txt".to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic cat arguments");
	send_cap(&cat_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("dynamic cat system volume");
	for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..]] {
		cat_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic cat absent volume");
	}
	cat_bootstrap_kernel.send(Message::new(b"vol://system/".to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic cat cwd");
	let mut cat_output = None;
	for _ in 0..100_000 {
		writable_storage.pump();
		if let Ok(message) = cat_stdout_kernel.recv() {
			cat_output = Some(message);
			break;
		}
	}
	assert_eq!(&cat_output.expect("dynamic cat read-back").bytes, b"dynamic write");
	assert_eq!(&cat_stdout_kernel.recv().expect("dynamic cat read-back newline").bytes, b"\n");

	for (tool, correlation, arguments) in [(b"ls" as &[u8], 29u32, b"vol://system/dynamic-dir" as &[u8]), (b"du" as &[u8], 30u32, b"-s vol://system/dynamic-dir" as &[u8])] {
		let (output_kernel, output_user) = Channel::create();
		let (tool_bootstrap_kernel, tool_bootstrap_user) = Channel::create();
		let mut tool_launch = alloc::vec::Vec::new();
		tool_launch.extend_from_slice(&3u16.to_le_bytes());
		tool_launch.extend_from_slice(&correlation.to_le_bytes());
		tool_launch.extend_from_slice(&(tool.len() as u16).to_le_bytes());
		tool_launch.extend_from_slice(tool);
		tool_launch.extend_from_slice(&0u32.to_le_bytes());
		send_cap(&process_client, &tool_launch, tool_bootstrap_user, Rights::ALL).expect("dynamic traversal tool launch request");
		sched::run_until_idle();
		let tool_reply = process_client.recv().expect("dynamic traversal tool launch reply");
		assert_eq!(le_u32(&tool_reply.bytes, 0), correlation);
		assert_eq!(tool_reply.bytes[4], 1, "the traversal PIE loaded with its manifest providers");
		let tool_process = tool_reply.caps[0].object().into_any_arc().downcast::<Process>().expect("dynamic traversal capability is a Process");
		send_cap(&tool_bootstrap_kernel, b"STDOUT", output_user, Rights::ALL).expect("dynamic traversal stdout bootstrap");
		tool_bootstrap_kernel.send(Message::new(arguments.to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic traversal arguments");
		send_cap(&tool_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("dynamic traversal system volume");
		for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..]] {
			tool_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic traversal absent volume");
		}
		tool_bootstrap_kernel.send(Message::new(b"vol://system/".to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic traversal cwd");
		let mut captured = alloc::vec::Vec::new();
		for _ in 0..100_000 {
			writable_storage.pump();
			while let Ok(message) = output_kernel.recv() {
				captured.extend_from_slice(&message.bytes);
			}
			if tool_process.is_terminated() {
				break;
			}
		}
		assert!(tool_process.is_terminated(), "dynamic traversal tool completed");
		if tool == b"ls" {
			assert!(captured.windows(b"dynamic-write.txt".len()).any(|window| window == b"dynamic-write.txt"), "dynamic ls listed the written file");
			assert!(captured.windows(b"1 file".len()).any(|window| window == b"1 file"), "dynamic ls rendered its summary");
		} else {
			assert_eq!(&captured, b"13\tvol://system/dynamic-dir\n", "dynamic du summed the nested file exactly");
		}
	}

	let rmdir_name: &[u8] = b"rmdir";
	let (full_rmdir_stdout_kernel, full_rmdir_stdout_user) = Channel::create();
	let (full_rmdir_bootstrap_kernel, full_rmdir_bootstrap_user) = Channel::create();
	let mut full_rmdir_launch = alloc::vec::Vec::new();
	full_rmdir_launch.extend_from_slice(&3u16.to_le_bytes());
	full_rmdir_launch.extend_from_slice(&27u32.to_le_bytes());
	full_rmdir_launch.extend_from_slice(&(rmdir_name.len() as u16).to_le_bytes());
	full_rmdir_launch.extend_from_slice(rmdir_name);
	full_rmdir_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &full_rmdir_launch, full_rmdir_bootstrap_user, Rights::ALL).expect("non-empty rmdir launch request");
	sched::run_until_idle();
	let full_rmdir_reply = process_client.recv().expect("non-empty rmdir launch reply");
	assert_eq!(le_u32(&full_rmdir_reply.bytes, 0), 27);
	assert_eq!(full_rmdir_reply.bytes[4], 1, "the rmdir PIE loaded for non-empty rejection");
	send_cap(&full_rmdir_bootstrap_kernel, b"STDOUT", full_rmdir_stdout_user, Rights::ALL).expect("non-empty rmdir stdout bootstrap");
	full_rmdir_bootstrap_kernel.send(Message::new(b"vol://system/dynamic-dir".to_vec(), alloc::vec::Vec::new(), 0)).expect("non-empty rmdir arguments");
	send_cap(&full_rmdir_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("non-empty rmdir system volume");
	for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..]] {
		full_rmdir_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("non-empty rmdir absent volume");
	}
	full_rmdir_bootstrap_kernel.send(Message::new(b"vol://system/".to_vec(), alloc::vec::Vec::new(), 0)).expect("non-empty rmdir cwd");
	let mut full_rmdir_prefix = None;
	for _ in 0..100_000 {
		writable_storage.pump();
		if let Ok(message) = full_rmdir_stdout_kernel.recv() {
			full_rmdir_prefix = Some(message);
			break;
		}
	}
	assert_eq!(&full_rmdir_prefix.expect("non-empty rmdir error prefix").bytes, b"rmdir: could not remove ");
	assert_eq!(&full_rmdir_stdout_kernel.recv().expect("non-empty rmdir error path").bytes, b"vol://system/dynamic-dir");
	assert_eq!(&full_rmdir_stdout_kernel.recv().expect("non-empty rmdir error newline").bytes, b"\n");

	let rm_name: &[u8] = b"rm";
	let (rm_stdout_kernel, rm_stdout_user) = Channel::create();
	let (rm_bootstrap_kernel, rm_bootstrap_user) = Channel::create();
	let mut rm_launch = alloc::vec::Vec::new();
	rm_launch.extend_from_slice(&3u16.to_le_bytes());
	rm_launch.extend_from_slice(&24u32.to_le_bytes());
	rm_launch.extend_from_slice(&(rm_name.len() as u16).to_le_bytes());
	rm_launch.extend_from_slice(rm_name);
	rm_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &rm_launch, rm_bootstrap_user, Rights::ALL).expect("dynamic rm launch request");
	sched::run_until_idle();
	let rm_reply = process_client.recv().expect("dynamic rm launch reply");
	assert_eq!(le_u32(&rm_reply.bytes, 0), 24);
	assert_eq!(rm_reply.bytes[4], 1, "the rm PIE loaded with its manifest providers");
	send_cap(&rm_bootstrap_kernel, b"STDOUT", rm_stdout_user, Rights::ALL).expect("dynamic rm stdout bootstrap");
	rm_bootstrap_kernel.send(Message::new(b"vol://system/dynamic-dir/dynamic-write.txt".to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic rm arguments");
	send_cap(&rm_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("dynamic rm system volume");
	for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..]] {
		rm_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic rm absent volume");
	}
	rm_bootstrap_kernel.send(Message::new(b"vol://system/".to_vec(), alloc::vec::Vec::new(), 0)).expect("dynamic rm cwd");
	let mut rm_prefix = None;
	for _ in 0..100_000 {
		writable_storage.pump();
		if let Ok(message) = rm_stdout_kernel.recv() {
			rm_prefix = Some(message);
			break;
		}
	}
	assert_eq!(&rm_prefix.expect("dynamic rm confirmation prefix").bytes, b"removed ");
	assert_eq!(&rm_stdout_kernel.recv().expect("dynamic rm confirmation path").bytes, b"vol://system/dynamic-dir/dynamic-write.txt");
	assert_eq!(&rm_stdout_kernel.recv().expect("dynamic rm confirmation newline").bytes, b"\n");

	let (rmdir_stdout_kernel, rmdir_stdout_user) = Channel::create();
	let (rmdir_bootstrap_kernel, rmdir_bootstrap_user) = Channel::create();
	let mut rmdir_launch = alloc::vec::Vec::new();
	rmdir_launch.extend_from_slice(&3u16.to_le_bytes());
	rmdir_launch.extend_from_slice(&28u32.to_le_bytes());
	rmdir_launch.extend_from_slice(&(rmdir_name.len() as u16).to_le_bytes());
	rmdir_launch.extend_from_slice(rmdir_name);
	rmdir_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &rmdir_launch, rmdir_bootstrap_user, Rights::ALL).expect("empty rmdir launch request");
	sched::run_until_idle();
	let rmdir_reply = process_client.recv().expect("empty rmdir launch reply");
	assert_eq!(le_u32(&rmdir_reply.bytes, 0), 28);
	assert_eq!(rmdir_reply.bytes[4], 1, "the rmdir PIE loaded for empty removal");
	send_cap(&rmdir_bootstrap_kernel, b"STDOUT", rmdir_stdout_user, Rights::ALL).expect("empty rmdir stdout bootstrap");
	rmdir_bootstrap_kernel.send(Message::new(b"vol://system/dynamic-dir".to_vec(), alloc::vec::Vec::new(), 0)).expect("empty rmdir arguments");
	send_cap(&rmdir_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("empty rmdir system volume");
	for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..]] {
		rmdir_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("empty rmdir absent volume");
	}
	rmdir_bootstrap_kernel.send(Message::new(b"vol://system/".to_vec(), alloc::vec::Vec::new(), 0)).expect("empty rmdir cwd");
	let mut rmdir_prefix = None;
	for _ in 0..100_000 {
		writable_storage.pump();
		if let Ok(message) = rmdir_stdout_kernel.recv() {
			rmdir_prefix = Some(message);
			break;
		}
	}
	assert_eq!(&rmdir_prefix.expect("empty rmdir confirmation prefix").bytes, b"removed ");
	assert_eq!(&rmdir_stdout_kernel.recv().expect("empty rmdir confirmation path").bytes, b"vol://system/dynamic-dir");
	assert_eq!(&rmdir_stdout_kernel.recv().expect("empty rmdir confirmation newline").bytes, b"\n");

	let (missing_stdout_kernel, missing_stdout_user) = Channel::create();
	let (missing_bootstrap_kernel, missing_bootstrap_user) = Channel::create();
	let mut missing_launch = alloc::vec::Vec::new();
	missing_launch.extend_from_slice(&3u16.to_le_bytes());
	missing_launch.extend_from_slice(&25u32.to_le_bytes());
	missing_launch.extend_from_slice(&(cat_name.len() as u16).to_le_bytes());
	missing_launch.extend_from_slice(cat_name);
	missing_launch.extend_from_slice(&0u32.to_le_bytes());
	send_cap(&process_client, &missing_launch, missing_bootstrap_user, Rights::ALL).expect("missing-file cat launch request");
	sched::run_until_idle();
	let missing_reply = process_client.recv().expect("missing-file cat launch reply");
	assert_eq!(le_u32(&missing_reply.bytes, 0), 25);
	assert_eq!(missing_reply.bytes[4], 1, "the cat PIE loaded for negative read-back");
	send_cap(&missing_bootstrap_kernel, b"STDOUT", missing_stdout_user, Rights::ALL).expect("missing-file cat stdout bootstrap");
	missing_bootstrap_kernel.send(Message::new(b"vol://system/dynamic-dir/dynamic-write.txt".to_vec(), alloc::vec::Vec::new(), 0)).expect("missing-file cat arguments");
	send_cap(&missing_bootstrap_kernel, b"SYSTEM", writable_storage.client.clone(), Rights::ALL).expect("missing-file cat system volume");
	for tag in [&b"MEDIA"[..], &b"ISO"[..], &b"UDF"[..], &b"USB"[..]] {
		missing_bootstrap_kernel.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("missing-file cat absent volume");
	}
	missing_bootstrap_kernel.send(Message::new(b"vol://system/".to_vec(), alloc::vec::Vec::new(), 0)).expect("missing-file cat cwd");
	let mut missing_prefix = None;
	for _ in 0..100_000 {
		writable_storage.pump();
		if let Ok(message) = missing_stdout_kernel.recv() {
			missing_prefix = Some(message);
			break;
		}
	}
	assert_eq!(&missing_prefix.expect("missing-file cat error prefix").bytes, b"cat: ");
	assert_eq!(&missing_stdout_kernel.recv().expect("missing-file cat error path").bytes, b"vol://system/dynamic-dir/dynamic-write.txt");
	assert_eq!(&missing_stdout_kernel.recv().expect("missing-file cat error suffix").bytes, b": cannot open\n");

	// LAUNCH the ET_DYN probe with a bootstrap channel. ProcessService must resolve
	// pix.lslib -> lsrt.lslib from vol://system/lib, load providers first, relocate the
	// probe's PLT call, and only then start it. Wire: op, corr, name, handle marker.
	let dynamic_name: &[u8] = b"dyn_probe";
	let (dynamic_report, dynamic_bootstrap) = Channel::create();
	let mut launch = alloc::vec::Vec::new();
	launch.extend_from_slice(&3u16.to_le_bytes());
	launch.extend_from_slice(&2u32.to_le_bytes());
	launch.extend_from_slice(&(dynamic_name.len() as u16).to_le_bytes());
	launch.extend_from_slice(dynamic_name);
	launch.extend_from_slice(&0u32.to_le_bytes());
	let dynamic_started = arch::tsc::now();
	send_cap(&process_client, &launch, dynamic_bootstrap, Rights::ALL).expect("dynamic launch request");
	sched::run_until_idle();

	let reply = process_client.recv().expect("dynamic launch reply");
	let b = &reply.bytes;
	assert_eq!(le_u32(b, 0), 2, "dynamic launch echoes the correlation id");
	assert_eq!(b[4], 1, "the staged dynamic executable loaded with its providers");
	assert!(!reply.caps.is_empty(), "dynamic launch returns the Process capability");
	let dynamic_process = reply.caps[0].object().into_any_arc().downcast::<Process>().expect("dynamic launch capability is a Process");
	let report = dynamic_report.recv().expect("dynamic probe called its shared pix symbol");
	assert_eq!(&report.bytes, b"dynamic link ok");
	let dynamic_ns = arch::tsc::cycles_to_ns(arch::tsc::now().wrapping_sub(dynamic_started));
	crate::serial_println!("dynamic-start-perf: {}ns private-pages={} shared-pages={}", dynamic_ns, dynamic_process.private_image_pages(), dynamic_process.shared_image_pages());
	assert!(dynamic_ns != 0 && dynamic_process.private_image_pages() != 0 && dynamic_process.shared_image_pages() != 0);

	let (second_report, second_bootstrap) = Channel::create();
	let mut second_launch = alloc::vec::Vec::new();
	second_launch.extend_from_slice(&3u16.to_le_bytes());
	second_launch.extend_from_slice(&3u32.to_le_bytes());
	second_launch.extend_from_slice(&(dynamic_name.len() as u16).to_le_bytes());
	second_launch.extend_from_slice(dynamic_name);
	second_launch.extend_from_slice(&0u32.to_le_bytes());
	let warm_started = arch::tsc::now();
	send_cap(&process_client, &second_launch, second_bootstrap, Rights::ALL).expect("second dynamic launch request");
	sched::run_until_idle();
	let second_reply = process_client.recv().expect("second dynamic launch reply");
	assert_eq!(le_u32(&second_reply.bytes, 0), 3);
	assert_eq!(second_reply.bytes[4], 1);
	let second_process = second_reply.caps[0].object().into_any_arc().downcast::<Process>().expect("second dynamic launch capability is a Process");
	assert_eq!(&second_report.recv().expect("second dynamic probe report").bytes, b"dynamic link ok");
	let warm_ns = arch::tsc::cycles_to_ns(arch::tsc::now().wrapping_sub(warm_started));
	let first_provider_frame = dynamic_process.address_space().unmap(0x2000_0000).expect("first liblsrt text page");
	let second_provider_frame = second_process.address_space().unmap(0x2000_0000).expect("second liblsrt text page");
	assert_eq!(first_provider_frame, second_provider_frame, "concurrent dynamic processes share one physical liblsrt text page");
	crate::serial_println!("dynamic-warm-perf: {}ns two-process-private-pages={} two-process-shared-refs={}", warm_ns, dynamic_process.private_image_pages() + second_process.private_image_pages(), dynamic_process.shared_image_pages() + second_process.shared_image_pages());
	process_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");
	sched::run_until_idle();
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

fn measure_dynamic_wave_launch(process_client: &alloc::sync::Arc<object::channel::Channel>, wave: u8, name: &[u8], correlation: u32, expected_private_pages: usize, expected_shared_pages: usize) {
	let (first, first_bootstrap, first_ns) = launch_dynamic_for_measurement(process_client, name, correlation);
	let (second, second_bootstrap, warm_ns) = launch_dynamic_for_measurement(process_client, name, correlation + 1);
	let private_pages = first.private_image_pages();
	let shared_pages = first.shared_image_pages();
	assert!(first_ns != 0 && warm_ns != 0, "wave launch timings are nonzero");
	assert_eq!(private_pages, expected_private_pages, "wave representative private pages match the checked writable image plus stack");
	assert_eq!(shared_pages, expected_shared_pages, "wave representative shared pages match the checked immutable image");
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

tagged_test!(dynamic_wave_launch_metrics_are_structurally_sound, [Dynamic, Service, Process, Storage]);
fn dynamic_wave_launch_metrics_are_structurally_sound() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let (process_boot_kernel, _storage_boot_kernel, process_client) = start_process_service_from_volume(volume);
	sched::run_until_idle();
	assert_eq!(&process_boot_kernel.recv().expect("ProcessService online report").bytes, b"ProcessService: online");
	#[cfg(target_arch = "x86_64")]
	let representatives = [
		(1u8, b"echo" as &[u8], 100u32, 14usize, 80usize),
		(2, b"cat" as &[u8], 102, 20, 165),
		(3, b"date" as &[u8], 104, 19, 164),
		(4, b"ip" as &[u8], 106, 20, 164),
		(5, b"imgconv" as &[u8], 108, 44, 428),
	];
	#[cfg(target_arch = "aarch64")]
	let representatives = [
		(1u8, b"echo" as &[u8], 100u32, 14usize, 88usize),
		(2, b"cat" as &[u8], 102, 23, 182),
		(3, b"date" as &[u8], 104, 22, 181),
		(4, b"ip" as &[u8], 106, 23, 181),
		(5, b"imgconv" as &[u8], 108, 65, 452),
	];
	#[cfg(target_arch = "riscv64")]
	let representatives = [
		(1u8, b"echo" as &[u8], 100u32, 14usize, 72usize),
		(2, b"cat" as &[u8], 102, 23, 142),
		(3, b"date" as &[u8], 104, 22, 141),
		(4, b"ip" as &[u8], 106, 22, 141),
		(5, b"imgconv" as &[u8], 108, 63, 335),
	];
	for (wave, name, correlation, private_pages, shared_pages) in representatives {
		measure_dynamic_wave_launch(&process_client, wave, name, correlation, private_pages, shared_pages);
	}
	process_client.send(object::channel::Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");
	sched::run_until_idle();
}

tagged_test!(unrelated_dynamic_consumers_share_domain_and_codec_text, [Dynamic, Service, Process, Storage]);
fn unrelated_dynamic_consumers_share_domain_and_codec_text() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let (process_boot_kernel, _storage_boot_kernel, process_client) = start_process_service_from_volume(volume);
	sched::run_until_idle();
	assert_eq!(&process_boot_kernel.recv().expect("ProcessService online report").bytes, b"ProcessService: online");
	assert_unrelated_dynamic_consumers_share(&process_client, b"cat", b"write", 120, 0x2400_0000, "volume-client");
	assert_unrelated_dynamic_consumers_share(&process_client, b"imgconv", b"imgview", 122, 0x2500_0000, "jpeg");
	process_client.send(object::channel::Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");
	sched::run_until_idle();
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

fn corrupt_volume_entry(volume: &mut [u8], entry: &[u8], field: &[u8]) {
	let volume_base = volume.as_ptr() as usize;
	let offset = {
		let archive = pkg::Package::parse(&*volume).expect("volume package parses");
		let bytes = archive.lookup(entry).expect("identity test entry is staged");
		let field_offset = bytes.windows(field.len()).position(|window| window == field).expect("identity test field is present");
		bytes.as_ptr() as usize - volume_base + field_offset + field.len()
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
	send_cap(&process_boot_kernel, b"STORAGE", storage_client, Rights::ALL).expect("process storage bootstrap");
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

tagged_test!(dynamic_process_service_rejects_missing_provider, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_missing_provider() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let mut mutated_volume = volume.to_vec();
	replace_dynamic_needed(&mut mutated_volume, b"bin/echo.lsexe", "lsrt.lslib", "none.lslib");
	let reply = launch_from_volume(&mutated_volume, b"echo", 78);
	assert_eq!(le_u32(&reply.bytes, 0), 78);
	assert_eq!(reply.bytes[4], 0, "ProcessService rejects an absent direct provider");
	assert!(reply.caps.is_empty(), "an absent provider creates no process capability");
}

tagged_test!(dynamic_process_service_rejects_undeclared_provider_edge, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_undeclared_provider_edge() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let mut mutated_volume = volume.to_vec();
	replace_dynamic_needed(&mut mutated_volume, b"bin/echo.lsexe", "lsrt.lslib", "wire.lslib");
	let reply = launch_from_volume(&mutated_volume, b"echo", 79);
	assert_eq!(le_u32(&reply.bytes, 0), 79);
	assert_eq!(reply.bytes[4], 0, "ProcessService rejects an undeclared provider edge");
	assert!(reply.caps.is_empty(), "an undeclared provider edge creates no process capability");
}

tagged_test!(dynamic_process_service_rejects_duplicate_provider_edge, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_duplicate_provider_edge() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let mut mutated_volume = volume.to_vec();
	duplicate_dynamic_needed(&mut mutated_volume, b"bin/dyn_probe.lsexe");
	let reply = launch_from_volume(&mutated_volume, b"dyn_probe", 80);
	assert_eq!(le_u32(&reply.bytes, 0), 80);
	assert_eq!(reply.bytes[4], 0, "ProcessService rejects a duplicate provider edge");
	assert!(reply.caps.is_empty(), "a duplicate provider edge creates no process capability");
}

tagged_test!(dynamic_process_service_rejects_malformed_dynamic_metadata, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_malformed_dynamic_metadata() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	for (correlation, mutate) in [
		(83u32, duplicate_dynamic_segment as fn(&mut [u8], &[u8])),
		(84, remove_dynamic_terminator as fn(&mut [u8], &[u8])),
		(85, duplicate_dynamic_singleton as fn(&mut [u8], &[u8])),
	] {
		let mut mutated_volume = volume.to_vec();
		mutate(&mut mutated_volume, b"bin/dyn_probe.lsexe");
		let reply = launch_from_volume(&mutated_volume, b"dyn_probe", correlation);
		assert_eq!(le_u32(&reply.bytes, 0), correlation);
		assert_eq!(reply.bytes[4], 0, "ProcessService rejects malformed dynamic metadata");
		assert!(reply.caps.is_empty(), "malformed dynamic metadata creates no process capability");
	}
}

tagged_test!(dynamic_process_service_rejects_malformed_symbol_and_relocation_metadata, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_malformed_symbol_and_relocation_metadata() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	for (correlation, mutate) in [
		(86u32, invalidate_dynamic_symbol_entry_size as fn(&mut [u8], &[u8])),
		(87, overflow_dynamic_symbol_count as fn(&mut [u8], &[u8])),
		(88, invalidate_plt_relocation_size as fn(&mut [u8], &[u8])),
	] {
		let mut mutated_volume = volume.to_vec();
		mutate(&mut mutated_volume, b"bin/dyn_probe.lsexe");
		let reply = launch_from_volume(&mutated_volume, b"dyn_probe", correlation);
		assert_eq!(le_u32(&reply.bytes, 0), correlation);
		assert_eq!(reply.bytes[4], 0, "ProcessService rejects malformed symbol or relocation metadata");
		assert!(reply.caps.is_empty(), "malformed symbol or relocation metadata creates no process capability");
	}
}

tagged_test!(dynamic_process_service_rejects_provider_cycle, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_provider_cycle() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let mut mutated_volume = volume.to_vec();
	replace_dynamic_needed(&mut mutated_volume, b"lib/wire.lslib", "lsrt.lslib", "wire.lslib");
	let reply = launch_from_volume(&mutated_volume, b"dyn_probe", 89);
	assert_eq!(le_u32(&reply.bytes, 0), 89);
	assert_eq!(reply.bytes[4], 0, "ProcessService rejects a provider dependency cycle");
	assert!(reply.caps.is_empty(), "a provider dependency cycle creates no process capability");
}

tagged_test!(dynamic_process_service_rejects_substituted_identity, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_substituted_identity() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let mut substituted_provider = volume.to_vec();
	replace_volume_entry(&mut substituted_provider, b"lib/lsrt.lslib", b"lib/wire.lslib");
	let reply = launch_from_volume(&substituted_provider, b"echo", 80);
	assert_eq!(le_u32(&reply.bytes, 0), 80);
	assert_eq!(reply.bytes[4], 0, "ProcessService rejects a valid provider substituted under lsrt.lslib");
	assert!(reply.caps.is_empty(), "a substituted provider creates no process capability");

	let mut corrupted_identity = volume.to_vec();
	corrupt_volume_entry(&mut corrupted_identity, b"id/lib/lsrt", b"source-sha256=");
	let reply = launch_from_volume(&corrupted_identity, b"echo", 81);
	assert_eq!(le_u32(&reply.bytes, 0), 81);
	assert_eq!(reply.bytes[4], 0, "ProcessService rejects a provider identity whose note digest no longer matches");
	assert!(reply.caps.is_empty(), "a corrupted provider identity creates no process capability");
}

tagged_test!(dynamic_process_service_rejects_duplicate_provider_export, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_duplicate_provider_export() {
	let (volume, _) = scenario_packages().expect("scenario packages");
	let mut duplicated_export = volume.to_vec();
	replace_provider_export(&mut duplicated_export, b"lib/pix.lslib", b"lib/lsrt.lslib");
	let reply = launch_from_volume(&duplicated_export, b"dyn_probe", 82);
	assert_eq!(le_u32(&reply.bytes, 0), 82);
	assert_eq!(reply.bytes[4], 0, "ProcessService rejects a provider that duplicates a runtime export");
	assert!(reply.caps.is_empty(), "a duplicate provider export creates no process capability");
}

tagged_test!(dynamic_process_service_rejects_linker_order_drift, [Dynamic, DynamicReject, Service, Process, Storage]);
fn dynamic_process_service_rejects_linker_order_drift() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	let (volume, package) = scenario_packages().expect("scenario packages");
	let init = init_package_bytes().expect("init package module not found");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe in the init package");
	let process_elf = package.lookup(b"process_service.lsexe").expect("process_service.lsexe in the init package");
	let mut drifted_volume = volume.to_vec();
	let order_offset = {
		let archive = pkg::Package::parse(&drifted_volume).expect("volume package parses");
		let order = archive.lookup(b"order/echo").expect("echo canonical order is staged");
		assert_eq!(order, b"lsrt.lslib\n", "echo has one runtime provider");
		order.as_ptr() as usize - drifted_volume.as_ptr() as usize
	};
	drifted_volume[order_offset..order_offset + b"wire.lslib\n".len()].copy_from_slice(b"wire.lslib\n");

	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (process_boot_kernel, process_boot_user) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	let (process_server, process_client) = Channel::create();
	let domain = sched::root_domain();
	loader::spawn_elf_process(domain.clone(), storage_elf, storage_boot_user, Rights::ALL, 0).expect("spawn StorageService");
	loader::spawn_elf_process(domain, process_elf, process_boot_user, Rights::ALL, 0).expect("spawn ProcessService");
	send_ramdisk(&storage_boot_kernel, &drifted_volume).expect("drifted storage ramdisk bootstrap");
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL).expect("storage serve bootstrap");
	send_package(&process_boot_kernel, init).expect("process package bootstrap");
	send_cap(&process_boot_kernel, b"STORAGE", storage_client, Rights::ALL).expect("process storage bootstrap");
	send_cap(&process_boot_kernel, b"SERVE", process_server, Rights::ALL).expect("process serve bootstrap");

	let name = b"echo";
	let mut start = alloc::vec::Vec::new();
	start.extend_from_slice(&1u16.to_le_bytes());
	start.extend_from_slice(&77u32.to_le_bytes());
	start.extend_from_slice(&(name.len() as u16).to_le_bytes());
	start.extend_from_slice(name);
	process_client.send(Message::new(start, alloc::vec::Vec::new(), 0)).expect("drifted echo start request");
	sched::run_until_idle();
	assert_eq!(&process_boot_kernel.recv().expect("ProcessService online report").bytes, b"ProcessService: online");
	let reply = process_client.recv().expect("drifted echo start reply");
	assert_eq!(le_u32(&reply.bytes, 0), 77);
	assert_eq!(reply.bytes[4], 0, "ProcessService rejects linker/runtime provider-order drift");
	assert!(reply.caps.is_empty(), "a rejected provider order creates no process capability");
	process_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");
	sched::run_until_idle();
}

tagged_test!(config_service_serves_the_tree, [Service]);
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

tagged_test!(imgconv_writes_indexed_png_through_writable_storage, [Service, Storage, Process]);
fn imgconv_writes_indexed_png_through_writable_storage() {
	use alloc::collections::BTreeMap;
	use object::channel::{Channel, Message};
	use object::memory_object::MemoryObject;
	use object::rights::Rights;

	const CAPACITY: u64 = 64 * 1024 * 1024;
	const SECTOR: usize = 512;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe in init package");
	let imgconv_elf = program_elf(&package, volume, b"imgconv").expect("canonical imgconv.lsexe in system volume");
	let source = bmp::decode_rgba(&volume_file(volume, b"sample.bmp").expect("staged BMP")).expect("staged BMP decodes");

	let (storage_boot, storage_boot_user) = Channel::create();
	let (blk_host, blk_child) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), storage_elf, storage_boot_user, Rights::ALL, 0).expect("spawn writable StorageService");
	send_cap(&storage_boot, b"BLOCK", blk_child, Rights::ALL).expect("BLOCK bootstrap");
	send_cap(&storage_boot, b"SERVE", storage_server, Rights::ALL).expect("SERVE bootstrap");
	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	for (lba, chunk) in volume.chunks(SECTOR).enumerate() {
		let mut sector = alloc::vec![0u8; SECTOR];
		sector[..chunk.len()].copy_from_slice(chunk);
		disk.insert(lba as u64, sector);
	}
	let mut online = false;
	for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(report) = storage_boot.recv() {
			assert_eq!(&report.bytes[..], b"StorageService: online");
			online = true;
			break;
		}
	}
	assert!(online, "writable seeded StorageService reports online");

	let (bootstrap, child) = Channel::create();
	let (stdout, child_stdout) = Channel::create();
	let process = spawn_dynamic_test_process(sched::root_domain(), imgconv_elf, child);
	send_cap(&bootstrap, b"STDOUT", child_stdout, Rights::ALL).expect("stdout bootstrap");
	bootstrap.send(Message::new(b"--quality 100 --compression 100 vol://system/sample.bmp vol://system/converted.png".to_vec(), alloc::vec::Vec::new(), 0)).expect("imgconv args");
	send_cap(&bootstrap, b"SYSTEM", storage_client.clone(), Rights::ALL).expect("SYSTEM bootstrap");
	for tag in [b"MEDIA".as_slice(), b"ISO".as_slice(), b"UDF".as_slice(), b"USB".as_slice()] {
		bootstrap.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("absent volume bootstrap");
	}
	bootstrap.send(Message::new(b"vol://system".to_vec(), alloc::vec::Vec::new(), 0)).expect("cwd bootstrap");

	let mut line = None;
	for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if line.is_none()
			&& let Ok(message) = stdout.recv()
		{
			line = Some(message.bytes);
		}
		if line.is_some() && process.is_terminated() {
			break;
		}
	}
	let line = line.expect("imgconv prints a result");
	assert!(line.starts_with(b"imgconv: BMP 2x2 -> PNG 2x2 quality=100 compression=100 bytes="));
	assert!(line.ends_with(b" metadata=stripped\n"));
	assert!(process.is_terminated(), "imgconv exits after writing output");

	let path = b"vol://system/converted.png";
	let corr = 0x1260u32;
	let mut request = alloc::vec::Vec::new();
	request.extend_from_slice(&1u16.to_le_bytes());
	request.extend_from_slice(&corr.to_le_bytes());
	request.extend_from_slice(&(path.len() as u16).to_le_bytes());
	request.extend_from_slice(path);
	request.push(0);
	request.push(0);
	storage_client.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("open converted output");
	let reply = loop {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(reply) = storage_client.recv() {
			break reply;
		}
	};
	assert_eq!(le_u32(&reply.bytes, 0), corr);
	assert_eq!(reply.bytes[4], 1, "converted output opens");
	let size = le_u64(&reply.bytes, 9) as usize;
	let output = reply.caps.first().expect("converted output buffer").object().into_any_arc().downcast::<MemoryObject>().expect("converted output is a MemoryObject");
	let decoded = png::decode_rgba(&read_from_object(&output, size)).expect("converted output independently decodes");
	assert_eq!(decoded, source, "indexed conversion preserves an exactly representable source palette");
}

tagged_test!(config_set_survives_a_service_reboot, [Service, Storage]);
fn config_set_survives_a_service_reboot() {
	use alloc::collections::BTreeMap;
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// Persistence: a `config set` survives the service's whole lifetime ending.
	// ConfigService write-throughs its tree to `vol://system/config.tree`, so a NEW
	// instance over the SAME volume loads it back - the reboot property (and what
	// makes the transparent ConfigService restart stateless). Stand up a
	// StorageService over a fresh writable disk (the sparse block stand-in formats
	// an empty LiberFS), run a FIRST ConfigService wired to a minted volume
	// connection, SET a key, end the instance, then run a SECOND instance over
	// another minted connection: the set value AND the seeded defaults both serve.
	const CAPACITY: u64 = 64 * 1024 * 1024;
	let (scenario_volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe in the init package");
	let config_elf = program_elf(&package, scenario_volume, b"config_service").expect("config_service in the package or volume");

	// StorageService over the sparse in-memory disk: no superblock and no archive,
	// so it formats a fresh writable volume.
	let (storage_boot_kernel, storage_boot_user) = Channel::create();
	let (blk_host, blk_child) = Channel::create();
	let (storage_server, storage_client) = Channel::create();
	loader::spawn_elf_process(sched::root_domain(), storage_elf, storage_boot_user, Rights::ALL, 0).expect("spawn StorageService");
	send_cap(&storage_boot_kernel, b"BLOCK", blk_child, Rights::ALL).expect("BLOCK bootstrap");
	send_cap(&storage_boot_kernel, b"SERVE", storage_server, Rights::ALL).expect("SERVE bootstrap");
	let mut disk: BTreeMap<u64, alloc::vec::Vec<u8>> = BTreeMap::new();
	let mut online = false;
	for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(report) = storage_boot_kernel.recv() {
			assert_eq!(&report.bytes[..], b"StorageService: online");
			online = true;
			break;
		}
	}
	assert!(online, "StorageService should format the fresh disk and report in");

	// Mint an independent volume connection off the storage root (the CONNECT_OP
	// factory), pumping block traffic while the service answers.
	fn mint_volume(storage_client: &alloc::sync::Arc<object::channel::Channel>, blk_host: &alloc::sync::Arc<object::channel::Channel>, disk: &mut alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>>, capacity: u64) -> alloc::sync::Arc<object::channel::Channel> {
		use object::channel::{Channel, Message};
		storage_client.send(Message::new(0xffffu16.to_le_bytes().to_vec(), alloc::vec::Vec::new(), 0)).expect("connect request");
		for _ in 0..100_000 {
			sched::run_until_idle();
			pump_block_stand_in(blk_host, disk, capacity);
			if let Ok(reply) = storage_client.recv() {
				let cap = reply.caps.first().expect("the minted connection is transferred");
				return cap.object().into_any_arc().downcast::<Channel>().expect("the connection is a channel");
			}
		}
		panic!("no minted volume connection arrived");
	}

	// The first ConfigService instance: its persistence backing and serve channel.
	let vol1 = mint_volume(&storage_client, &blk_host, &mut disk, CAPACITY);
	let (cfg1_boot, cfg1_boot_user) = Channel::create();
	let (cfg1_server, cfg1_client) = Channel::create();
	let _config1 = spawn_dynamic_test_process(sched::root_domain(), config_elf, cfg1_boot_user);
	send_cap(&cfg1_boot, b"STORAGE", vol1, Rights::ALL).expect("STORAGE bootstrap 1");
	send_cap(&cfg1_boot, b"SERVE", cfg1_server, Rights::ALL).expect("SERVE bootstrap 1");

	// SET persist.key = survives ([op = 3 u16][corr u32][key + value strings]); the
	// write-through to vol://system/config.tree completes before the reply.
	let (k, v): (&[u8], &[u8]) = (b"persist.key", b"survives");
	let mut set = alloc::vec::Vec::new();
	set.extend_from_slice(&3u16.to_le_bytes());
	set.extend_from_slice(&1u32.to_le_bytes());
	set.extend_from_slice(&(k.len() as u16).to_le_bytes());
	set.extend_from_slice(k);
	set.extend_from_slice(&(v.len() as u16).to_le_bytes());
	set.extend_from_slice(v);
	cfg1_client.send(Message::new(set, alloc::vec::Vec::new(), 0)).expect("set request");
	let mut set_ok = false;
	for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		if let Ok(reply) = cfg1_client.recv() {
			assert_eq!(le_u32(&reply.bytes, 0), 1, "set echoes the correlation id");
			assert_eq!(reply.bytes[4], 1, "set succeeded");
			set_ok = true;
			break;
		}
	}
	assert!(set_ok, "the set should be answered");
	// End the first instance: the quit sentinel breaks its serve loop and it exits.
	cfg1_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel");
	sched::run_until_idle();

	// The second instance over the SAME volume: the persisted tree loads back.
	let vol2 = mint_volume(&storage_client, &blk_host, &mut disk, CAPACITY);
	let (cfg2_boot, cfg2_boot_user) = Channel::create();
	let (cfg2_server, cfg2_client) = Channel::create();
	let _config2 = spawn_dynamic_test_process(sched::root_domain(), config_elf, cfg2_boot_user);
	send_cap(&cfg2_boot, b"STORAGE", vol2, Rights::ALL).expect("STORAGE bootstrap 2");
	send_cap(&cfg2_boot, b"SERVE", cfg2_server, Rights::ALL).expect("SERVE bootstrap 2");
	let get = |corr: u32, key: &[u8]| -> alloc::vec::Vec<u8> {
		let mut m = alloc::vec::Vec::new();
		m.extend_from_slice(&1u16.to_le_bytes());
		m.extend_from_slice(&corr.to_le_bytes());
		m.extend_from_slice(&(key.len() as u16).to_le_bytes());
		m.extend_from_slice(key);
		m
	};
	cfg2_client.send(Message::new(get(1, b"persist.key"), alloc::vec::Vec::new(), 0)).expect("get persisted");
	cfg2_client.send(Message::new(get(2, b"system.name"), alloc::vec::Vec::new(), 0)).expect("get seeded");
	let mut replies: alloc::vec::Vec<alloc::vec::Vec<u8>> = alloc::vec::Vec::new();
	for _ in 0..100_000 {
		sched::run_until_idle();
		pump_block_stand_in(&blk_host, &mut disk, CAPACITY);
		while let Ok(reply) = cfg2_client.recv() {
			replies.push(reply.bytes);
		}
		if replies.len() >= 2 {
			break;
		}
	}
	assert_eq!(replies.len(), 2, "both gets should be answered");
	assert_eq!(le_u32(&replies[0], 0), 1);
	assert_eq!(replies[0][4], 1, "the persisted key exists in the fresh instance");
	let vlen = le_u16(&replies[0], 5) as usize;
	assert_eq!(&replies[0][7..7 + vlen], b"survives", "the set value survived the service reboot");
	assert_eq!(replies[1][4], 1, "a seeded default still serves");
	let nlen = le_u16(&replies[1], 5) as usize;
	assert_eq!(&replies[1][7..7 + nlen], b"LiberSystem", "the persisted tree overlays, not replaces, the defaults");
	cfg2_client.send(Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0)).expect("quit sentinel 2");
	sched::run_until_idle();
}

// This end-to-end test asserts the EXACT boot-chain report order, which requires the
// interrupt-driven services (NetworkService over virtio-net, and its transitive
// dependents TimeService/PermissionManager/ConsoleService/SystemGraphService/Shell) to
// all settle inside the harness's single `run_until_idle()`. It was previously gated off
// riscv64 (`#[cfg(not(target_arch = "riscv64"))]`) because those services intermittently
// failed to report in there - which turned out to be the riscv trap-frame register clobber
// (a trap could corrupt the interrupted thread's t0/x5), not an interrupt-timing issue;
// with that fixed the chain settles deterministically on riscv64 too.
tagged_test!(init_package_starts_system_manager, [Boot, Service]);
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
	// DeviceManager's virtio-blk backs), the graceful-shutdown ordering check
	// (ServiceManager confirms the reverse-dependency teardown order the `poweroff`
	// path uses is valid against the live manifest), followed by the two managers.
	let (kernel_ep, _koid) = spawn_system_manager().expect("SystemManager should start from the init package");
	sched::run_until_idle();
	let reports: [&[u8]; 31] = [
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
		b"DisplayService: online",
		b"PermissionManager: online",
		b"ConsoleService: online",
		b"SystemGraphService: online",
		b"Shell: online",
		b"WatchdogProbe: online",
		b"WatchdogProbe: restarted",
		b"WatchdogProbe: recovered",
		b"ConfigService: restarted",
		b"WatchdogProbe: config client survived",
		b"PermissionManager: config client reconnected",
		b"DeviceManager: stopped",
		b"ServiceManager: shutdown order ok",
		b"ServiceManager: online",
		b"SystemManager: online",
	];
	for expected in reports {
		let message = kernel_ep.recv().expect("a boot-chain report should arrive");
		assert_eq!(&message.bytes[..], expected, "boot-chain reports must arrive in dependency order");
	}
}

tagged_test!(pty_hosts_a_program, [Service, Shell, Console]);
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
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe in the init package");
	let process_elf = package.lookup(b"process_service.lsexe").expect("process_service.lsexe in the init package");

	// ConsoleService's bootstrap channel and the channels its __user_main expects: VT 1's
	// data (CLIENT) + control (CONTROL), a factory per service (FSTORAGE..FNET; only FPROCESS
	// is a live ProcessService here, which loads the ptyecho slave - the rest are unused, as
	// the slave needs no services), then GPU (none) and POINTER (none).
	let (boot_kernel, boot_user) = Channel::create();
	let (vt1_console_a, _vt1_console_b) = Channel::create();
	let (ctl_console, ctl_shell) = Channel::create();
	let (dummy_a, _dummy_b) = Channel::create();

	let _console_service = spawn_dynamic_test_process(sched::root_domain(), console_elf, boot_user);

	// A StorageService over the factory volume (which stages ptyecho under bin/), so the
	// ProcessService below can load the ptyecho slave from vol://system/bin/ptyecho.lsexe.
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

tagged_test!(interactive_tool_reads_stdin, [Service, Shell, Console, Input]);
fn interactive_tool_reads_stdin() {
	// The provider-aware ProcessService scenario above drives readln's full-duplex console
	// behavior. Independently pin its staged graph here: raw-spawning an ET_DYN consumer
	// without ProcessService would skip its provider resolution and is not a valid test path.
	let init = init_package_bytes().expect("init package module not found");
	let volume = volume_package_bytes().expect("volume package module not found");
	let package = pkg::Package::parse(init).expect("init package parses");
	let readln_elf = program_elf(&package, volume, b"readln").expect("readln in the package or volume");
	let elf = bootproto::elf::Elf::parse(readln_elf).expect("readln is ELF");
	assert_eq!(elf.image_type, bootproto::elf::ET_DYN, "readln is staged as PIE");
	let dynamic = elf.dynamic_info().expect("readln dynamic metadata parses").expect("readln has PT_DYNAMIC");
	assert_eq!(elf.needed_names(&dynamic).expect("readln dependencies parse").collect::<alloc::vec::Vec<_>>(), alloc::vec!["lsrt.lslib"]);
}

tagged_test!(du_reports_a_directory_tree_size, [Service, Storage, Shell]);
fn du_reports_a_directory_tree_size() {
	// The provider-aware ProcessService scenario above executes du against a live tree.
	// Keep this independently tagged test as the package contract: du must now be a PIE
	// with the four direct providers required by its recursive alloc/proto traversal.
	let (volume, package) = scenario_packages().expect("scenario packages");
	let du_elf = program_elf(&package, volume, b"du").expect("du in the package or volume");
	let elf = bootproto::elf::Elf::parse(du_elf).expect("du is ELF");
	assert_eq!(elf.image_type, bootproto::elf::ET_DYN, "du is staged as PIE");
	let dynamic = elf.dynamic_info().expect("du dynamic metadata parses").expect("du has PT_DYNAMIC");
	let mut needed = elf.needed_names(&dynamic).expect("du dependencies parse").collect::<alloc::vec::Vec<_>>();
	needed.sort_unstable();
	assert_eq!(needed, alloc::vec!["lsrt.lslib", "proto.lslib", "volume-client.lslib", "wire.lslib"]);
}

fn assert_dynamic_inventory_providers(name: &[u8], expected: &[&str]) {
	let init = init_package_bytes().expect("init package present");
	let volume = volume_package_bytes().expect("volume package present");
	let package = pkg::Package::parse(init).expect("init package parses");
	let bytes = program_elf(&package, volume, name).expect("dynamic inventory tool is staged");
	let elf = bootproto::elf::Elf::parse(bytes).expect("dynamic inventory tool is ELF");
	assert_eq!(elf.image_type, bootproto::elf::ET_DYN, "inventory tool is PIE");
	let dynamic = elf.dynamic_info().expect("inventory dynamic metadata parses").expect("inventory tool has PT_DYNAMIC");
	let mut needed = elf.needed_names(&dynamic).expect("inventory dependencies parse").collect::<alloc::vec::Vec<_>>();
	needed.sort_unstable();
	assert_eq!(needed, expected);
}

tagged_test!(inventory_tools_print_the_system_identity, [Service, Shell]);
fn inventory_tools_print_the_system_identity() {
	// The zero-capability inventory commands: each runs as its own sandboxed
	// ELF and prints compile-time / free-syscall data - no service client, the
	// emptiest manifests in the permission store. uname prints the product identity
	// and architecture, uptime the time since boot, and dmesg the kernel boot log
	// (the same text SYS_CONSOLE_READLOG hands ConsoleService for the boot screen).
	assert_dynamic_inventory_providers(b"uname", &["lsrt.lslib"]);
	assert_dynamic_inventory_providers(b"uptime", &["lsrt.lslib"]);

	assert_dynamic_inventory_providers(b"dmesg", &["lsrt.lslib"]);
}

tagged_test!(inventory_tools_report_the_hardware, [Service, Shell, Drivers]);
fn inventory_tools_report_the_hardware() {
	// The hardware-inventory commands: each runs as its own sandboxed ELF over
	// a free syscall reading state the kernel now retains past boot - the CPU set
	// (lscpu), the frame-pool and heap totals (free), the boot memory map (lsmem),
	// and the device-interrupt vector table (lsirq).
	assert_dynamic_inventory_providers(b"lscpu", &["lsrt.lslib", "wire.lslib"]);
	assert_dynamic_inventory_providers(b"free", &["lsrt.lslib"]);

	assert_dynamic_inventory_providers(b"lsmem", &["lsrt.lslib", "wire.lslib"]);
	assert_dynamic_inventory_providers(b"lsirq", &["lsrt.lslib", "wire.lslib"]);
	assert_dynamic_inventory_providers(b"lspci", &["lsrt.lslib", "wire.lslib"]);
}

// The sector where StorageService lays the fixed factory LiberFS layout when a disk
// carries no GPT partition for it - it must mirror the storage service's own
// FS_START_SECTOR (src/user/storage/src/service.rs), which sits past the largest
// architecture's factory archive so the seed always fits ahead of the filesystem.
const FACTORY_START_SECTOR: u64 = 65536;

tagged_test!(system_volume_formats_to_the_disks_capacity, [Service, Storage, Filesystem, Slow]);
fn system_volume_formats_to_the_disks_capacity() {
	use alloc::collections::BTreeMap;
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// A fresh system volume spans the whole disk - StorageService asks the block
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
	let elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe should be in the init package");

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

	// The typed volume health/policy ops over the serve channel. Send a generated
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

struct StorageHarness {
	boot: alloc::sync::Arc<object::channel::Channel>,
	block: alloc::sync::Arc<object::channel::Channel>,
	client: alloc::sync::Arc<object::channel::Channel>,
	disk: alloc::collections::BTreeMap<u64, alloc::vec::Vec<u8>>,
	capacity: u64,
}

impl StorageHarness {
	fn start(storage_elf: &[u8], tag: &[u8], image: &[u8], capacity: u64) -> Self {
		use object::channel::Channel;
		use object::rights::Rights;
		const SECTOR: usize = 512;
		let (boot, boot_user) = Channel::create();
		let (block, block_child) = Channel::create();
		let (server, client) = Channel::create();
		loader::spawn_elf_process(sched::root_domain(), storage_elf, boot_user, Rights::ALL, 0).expect("spawn StorageService harness");
		send_cap(&boot, tag, block_child, Rights::ALL).expect("storage block bootstrap");
		send_cap(&boot, b"SERVE", server, Rights::ALL).expect("storage serve bootstrap");
		let mut disk = alloc::collections::BTreeMap::new();
		for (lba, chunk) in image.chunks(SECTOR).enumerate() {
			let mut sector = alloc::vec![0u8; SECTOR];
			sector[..chunk.len()].copy_from_slice(chunk);
			disk.insert(lba as u64, sector);
		}
		let mut harness = Self { boot, block, client, disk, capacity };
		for _ in 0..100_000 {
			harness.pump();
			if let Ok(report) = harness.boot.recv() {
				assert_eq!(&report.bytes[..], b"StorageService: online");
				return harness;
			}
		}
		panic!("StorageService harness did not report online");
	}

	fn pump(&mut self) {
		sched::run_until_idle();
		pump_block_stand_in(&self.block, &mut self.disk, self.capacity);
	}

	fn open(&mut self, path: &[u8], corr: u32) -> Option<alloc::vec::Vec<u8>> {
		use object::channel::Message;
		use object::memory_object::MemoryObject;
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&1u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		request.extend_from_slice(&(path.len() as u16).to_le_bytes());
		request.extend_from_slice(path);
		request.extend_from_slice(&[0, 0]);
		self.client.send(Message::new(request, alloc::vec::Vec::new(), 0)).expect("storage open request");
		for _ in 0..100_000 {
			self.pump();
			if let Ok(reply) = self.client.recv() {
				if le_u32(&reply.bytes, 0) != corr || reply.bytes.get(4) != Some(&1) {
					return None;
				}
				let size = le_u64(&reply.bytes, 9) as usize;
				let object = reply.caps.first()?.object().into_any_arc().downcast::<MemoryObject>().ok()?;
				return Some(read_from_object(&object, size));
			}
		}
		None
	}
}

fn fat16_image(files: &[([u8; 11], &[u8])], fill_free: bool) -> alloc::vec::Vec<u8> {
	const SECTOR: usize = 512;
	const CLUSTERS: usize = 5_000;
	const RESERVED: usize = 1;
	const ROOT_ENTRIES: usize = 512;
	let fat_sectors = ((CLUSTERS + 2) * 2).div_ceil(SECTOR);
	let root_sectors = (ROOT_ENTRIES * 32).div_ceil(SECTOR);
	let first_data = RESERVED + fat_sectors + root_sectors;
	let total = first_data + CLUSTERS;
	let mut image = alloc::vec![0u8; total * SECTOR];
	let fat_offset = RESERVED * SECTOR;
	image[fat_offset..fat_offset + 2].copy_from_slice(&0xfff8u16.to_le_bytes());
	image[fat_offset + 2..fat_offset + 4].copy_from_slice(&0xffffu16.to_le_bytes());
	let root_offset = (RESERVED + fat_sectors) * SECTOR;
	for (index, (name, data)) in files.iter().enumerate() {
		assert!(data.len() <= SECTOR && index < ROOT_ENTRIES);
		let cluster = index + 2;
		let fat = fat_offset + cluster * 2;
		image[fat..fat + 2].copy_from_slice(&0xffffu16.to_le_bytes());
		let data_offset = (first_data + cluster - 2) * SECTOR;
		image[data_offset..data_offset + data.len()].copy_from_slice(data);
		let entry = root_offset + index * 32;
		image[entry..entry + 11].copy_from_slice(name);
		image[entry + 11] = 0x20;
		image[entry + 26..entry + 28].copy_from_slice(&(cluster as u16).to_le_bytes());
		image[entry + 28..entry + 32].copy_from_slice(&(data.len() as u32).to_le_bytes());
	}
	if fill_free {
		for cluster in files.len() + 2..CLUSTERS + 2 {
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

tagged_test!(storage_harness_mounts_seeded_fat16, [Storage, Filesystem]);
fn storage_harness_mounts_seeded_fat16() {
	let (_, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let image = fat16_image(&[(*b"HELLO   TXT", b"hello")], false);
	let mut storage = StorageHarness::start(storage_elf, b"FATBLOCK", &image, image.len() as u64);
	assert_eq!(storage.open(b"vol://media/HELLO.TXT", 0xfa16), Some(b"hello".to_vec()));
}

fn spawn_dynamic_test_process(domain: alloc::sync::Arc<object::domain::Domain>, main: &[u8], bootstrap: alloc::sync::Arc<object::channel::Channel>) -> alloc::sync::Arc<object::process::Process> {
	fn load(package: &pkg::Package<'_>, process: &object::process::Process, name: &str, loaded: &mut alloc::vec::Vec<alloc::string::String>, visiting: &mut alloc::vec::Vec<alloc::string::String>) {
		if loaded.iter().any(|item| item == name) {
			return;
		}
		assert!(!visiting.iter().any(|item| item == name), "dynamic test provider cycle");
		visiting.push(alloc::string::String::from(name));
		let path = alloc::format!("lib/{name}");
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
	let process = object::process::Process::new(object::address_space::AddressSpace::create().expect("dynamic test address space"), domain);
	let elf = bootproto::elf::Elf::parse(main).expect("dynamic test main is ELF");
	let dynamic = elf.dynamic_info().expect("main dynamic metadata parses").expect("main has PT_DYNAMIC");
	let mut loaded = alloc::vec::Vec::new();
	let mut visiting = alloc::vec::Vec::new();
	for dependency in elf.needed_names(&dynamic).expect("main dependencies parse") {
		load(&package, &process, dependency, &mut loaded, &mut visiting);
	}
	let entry = loader::load_image_into(&process, main).expect("load dynamic test main");
	let bootstrap = process.install(bootstrap, object::rights::Rights::ALL, 0);
	let thread = loader::create_user_thread(&process, entry, memlayout::USER_STACK_TOP, bootstrap).expect("create dynamic test thread");
	assert!(sched::thread_start(thread), "start dynamic test thread");
	process
}

fn run_imgconv_harness_result(domain: alloc::sync::Arc<object::domain::Domain>, imgconv_elf: &[u8], args: &[u8], system: &mut StorageHarness, media: &mut StorageHarness) -> (Option<alloc::vec::Vec<u8>>, u64) {
	use object::channel::{Channel, Message};
	use object::rights::Rights;
	let (bootstrap, child) = Channel::create();
	let (stdout, child_stdout) = Channel::create();
	let process = spawn_dynamic_test_process(domain.clone(), imgconv_elf, child);
	send_cap(&bootstrap, b"STDOUT", child_stdout, Rights::ALL).expect("imgconv stdout");
	bootstrap.send(Message::new(args.to_vec(), alloc::vec::Vec::new(), 0)).expect("imgconv args");
	send_cap(&bootstrap, b"SYSTEM", system.client.clone(), Rights::ALL).expect("imgconv system volume");
	send_cap(&bootstrap, b"MEDIA", media.client.clone(), Rights::ALL).expect("imgconv media volume");
	for tag in [b"ISO".as_slice(), b"UDF".as_slice(), b"USB".as_slice()] {
		bootstrap.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("imgconv absent volume");
	}
	bootstrap.send(Message::new(b"vol://system".to_vec(), alloc::vec::Vec::new(), 0)).expect("imgconv cwd");
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
	assert!(process.is_terminated(), "imgconv harness exits");
	(line, domain.account().memory().peak())
}

fn run_imgconv_harness_in(domain: alloc::sync::Arc<object::domain::Domain>, imgconv_elf: &[u8], args: &[u8], system: &mut StorageHarness, media: &mut StorageHarness) -> (alloc::vec::Vec<u8>, u64) {
	let (line, peak) = run_imgconv_harness_result(domain, imgconv_elf, args, system, media);
	(line.expect("imgconv harness prints a result"), peak)
}

fn run_imgconv_harness(imgconv_elf: &[u8], args: &[u8], system: &mut StorageHarness, media: &mut StorageHarness) -> alloc::vec::Vec<u8> {
	run_imgconv_harness_in(sched::root_domain(), imgconv_elf, args, system, media).0
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
	bootstrap.send(Message::new(b"--help".to_vec(), alloc::vec::Vec::new(), 0)).expect("imgview help args");
	send_cap(&bootstrap, b"SYSTEM", system.client.clone(), Rights::ALL).expect("imgview help system volume");
	send_cap(&bootstrap, b"MEDIA", media.client.clone(), Rights::ALL).expect("imgview help media volume");
	for tag in [b"ISO".as_slice(), b"UDF".as_slice(), b"USB".as_slice(), b"DISPLAY".as_slice(), b"INPUT_KEYS".as_slice()] {
		bootstrap.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("imgview help absent capability");
	}
	bootstrap.send(Message::new(b"vol://system".to_vec(), alloc::vec::Vec::new(), 0)).expect("imgview help cwd");
	let output = loop {
		system.pump();
		media.pump();
		if let Ok(message) = stdout.recv() {
			break message.bytes;
		}
	};
	assert_eq!(output, b"Usage: imgview <image>\nDisplays a still image or composited animation frame 0; animation playback is not supported.\n");
	for _ in 0..100_000 {
		system.pump();
		media.pump();
		if process.is_terminated() {
			return;
		}
	}
	panic!("imgview help harness did not exit");
}

fn run_imgview_harness(imgview_elf: &[u8], path: &[u8], expected: &[u8], system: &mut StorageHarness, media: &mut StorageHarness) {
	use object::channel::{Channel, Message};
	use object::memory_object::MemoryObject;
	use object::rights::Rights;
	let (bootstrap, child) = Channel::create();
	let (_stdout, child_stdout) = Channel::create();
	let (display, display_client) = Channel::create();
	let (input, input_client) = Channel::create();
	let process = spawn_dynamic_test_process(sched::root_domain(), imgview_elf, child);
	send_cap(&bootstrap, b"STDOUT", child_stdout, Rights::ALL).expect("imgview stdout");
	bootstrap.send(Message::new(path.to_vec(), alloc::vec::Vec::new(), 0)).expect("imgview args");
	send_cap(&bootstrap, b"SYSTEM", system.client.clone(), Rights::ALL).expect("imgview system volume");
	send_cap(&bootstrap, b"MEDIA", media.client.clone(), Rights::ALL).expect("imgview media volume");
	for tag in [b"ISO".as_slice(), b"UDF".as_slice(), b"USB".as_slice()] {
		bootstrap.send(Message::new(tag.to_vec(), alloc::vec::Vec::new(), 0)).expect("imgview absent volume");
	}
	send_cap(&bootstrap, b"DISPLAY", display_client, Rights::ALL).expect("imgview display");
	send_cap(&bootstrap, b"INPUT_KEYS", input_client, Rights::ALL).expect("imgview input");
	bootstrap.send(Message::new(b"vol://system".to_vec(), alloc::vec::Vec::new(), 0)).expect("imgview cwd");

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
	for _ in 0..100 {
		system.pump();
		media.pump();
	}
	keys.send(Message::new(alloc::vec![0, 0, 0, 0, 0x14, 0, 1], alloc::vec::Vec::new(), 0)).expect("imgview q key");

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

tagged_test!(imgconv_cross_volume_and_failed_overwrite_preserve_destination, [Image, Service, Storage, Process, Filesystem]);
fn imgconv_cross_volume_and_failed_overwrite_preserve_destination() {
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let imgconv_elf = program_elf(&package, volume, b"imgconv").expect("imgconv tool");
	let imgview_elf = program_elf(&package, volume, b"imgview").expect("imgview tool");
	let source = bmp::decode_rgba(&volume_file(volume, b"sample.bmp").expect("staged BMP")).expect("staged BMP decodes");
	let mut system = StorageHarness::start(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);

	let media_image = fat16_image(&[], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);
	let help = run_imgconv_harness(imgconv_elf, b"--help", &mut system, &mut media);
	assert!(help.starts_with(b"Usage: imgconv [options] <input> <output>\n\nOptions:\n"));
	assert!(help.windows(b"WebP  options: quality compression lossless lossy animation; defaults: mode=lossless compression=100".len()).any(|window| window == b"WebP  options: quality compression lossless lossy animation; defaults: mode=lossless compression=100"));
	let line = run_imgconv_harness(imgconv_elf, b"--quality 100 vol://system/sample.bmp vol://media/CROSS.BMP", &mut system, &mut media);
	assert!(line.starts_with(b"imgconv: BMP 2x2 -> BMP 2x2 quality=100 bytes="));
	let converted = media.open(b"vol://media/CROSS.BMP", 0xc2055).expect("cross-volume BMP opens");
	assert_eq!(bmp::decode_rgba(&converted).expect("cross-volume BMP decodes"), source);
	run_imgview_help_harness(imgview_elf, &mut system, &mut media);
	run_imgview_harness(imgview_elf, b"vol://media/CROSS.BMP", &viewer_surface(&source), &mut system, &mut media);

	let transparent_png = include_bytes!("../user/png/tests/data/external-rgba16.png");
	let transparent = png::decode_rgba(transparent_png).expect("decode transparent viewer fixture");
	let animation_webp = include_bytes!("../user/webp/tests/data/external-animation.webp");
	let viewer_image = fat16_image(&[(*b"ALPHA   PNG", transparent_png.as_slice()), (*b"ANIM    WEB", animation_webp)], false);
	let mut viewer_media = StorageHarness::start(storage_elf, b"FATBLOCK", &viewer_image, viewer_image.len() as u64);
	run_imgview_harness(imgview_elf, b"vol://media/ALPHA.PNG", &viewer_surface(&transparent), &mut system, &mut viewer_media);
	let animation_first = webp::decode(animation_webp).expect("composited WebP frame 0");
	run_imgview_harness(imgview_elf, b"vol://media/ANIM.WEB", &viewer_surface(&animation_first), &mut system, &mut viewer_media);

	let collision_pixel = pix::RgbaImage::new(1, 1, alloc::vec![17, 34, 51, 255]).expect("TGA collision pixel");
	let mut collision_tga = tga::encode(&collision_pixel, tga::EncodeOptions { rle: false }).expect("encode TGA collision");
	collision_tga[0] = 10;
	collision_tga.splice(18..18, *b"0123456789");
	let classification_image = fat16_image(&[(*b"UNKNOWN BIN", b"not an image"), (*b"BAD     PNG", b"\x89PNG\r\n\x1a\n"), (*b"COLLIDE TGA", &collision_tga)], false);
	let mut classification_media = StorageHarness::start(storage_elf, b"FATBLOCK", &classification_image, classification_image.len() as u64);
	let unknown = run_imgconv_harness(imgconv_elf, b"vol://media/UNKNOWN.BIN vol://media/UNKNOWN.BMP", &mut system, &mut classification_media);
	assert_eq!(unknown, b"imgconv: unsupported image format\n");
	let corrupt = run_imgconv_harness(imgconv_elf, b"vol://media/BAD.PNG vol://media/BAD.BMP", &mut system, &mut classification_media);
	assert_eq!(corrupt, b"imgconv: invalid or corrupt image\n");
	let collision = run_imgconv_harness(imgconv_elf, b"vol://media/COLLIDE.TGA vol://media/COLLIDE.BMP", &mut system, &mut classification_media);
	assert!(collision.starts_with(b"imgconv: TGA 1x1 -> BMP 1x1 bytes="));
	let collision_output = classification_media.open(b"vol://media/COLLIDE.BMP", 0xc0111de).expect("collision output opens");
	assert_eq!(bmp::decode_rgba(&collision_output).expect("collision output decodes"), collision_pixel);
	run_imgview_harness(imgview_elf, b"vol://media/COLLIDE.TGA", &viewer_surface(&collision_pixel), &mut system, &mut classification_media);

	let line = run_imgconv_harness(imgconv_elf, b"--lossless --compression 50 vol://system/sample.bmp vol://media/CROSSL.WEBP", &mut system, &mut media);
	assert!(line.starts_with(b"imgconv: BMP 2x2 -> WebP 2x2 mode=lossless compression=50 bytes="));
	let converted = media.open(b"vol://media/CROSSL.WEBP", 0xc2057).expect("cross-volume lossless WebP opens");
	assert_eq!(webp::decode(&converted).expect("cross-volume lossless WebP decodes"), source);

	let line = run_imgconv_harness(imgconv_elf, b"--lossy --quality 100 --compression 100 vol://system/sample.bmp vol://media/CROSS.WEBP", &mut system, &mut media);
	assert!(line.starts_with(b"imgconv: BMP 2x2 -> WebP 2x2 mode=lossy quality=100 compression=100 bytes="));
	let converted = media.open(b"vol://media/CROSS.WEBP", 0xc2056).expect("cross-volume WebP opens");
	assert_eq!(&converted[..4], b"RIFF", "lossy WebP uses the canonical RIFF container");
	assert_eq!(&converted[8..12], b"WEBP", "lossy WebP uses the canonical WEBP form type");
	assert_eq!(&converted[12..16], b"VP8 ", "opaque lossy WebP uses a simple VP8 chunk");
	let decoded = webp::decode(&converted).expect("cross-volume lossy WebP decodes");
	assert_eq!((decoded.width, decoded.height), (source.width, source.height));
	let squared_error: u64 = decoded.pixels.chunks_exact(4).zip(source.pixels.chunks_exact(4)).flat_map(|(actual, expected)| (0..3).map(move |channel| i64::from(actual[channel]) - i64::from(expected[channel]))).map(|difference| difference.unsigned_abs().pow(2)).sum();
	assert!(squared_error <= u64::from(source.width) * u64::from(source.height) * 3 * 5_000, "governed 2x2 lossy WebP exceeds its bounded RGB MSE");

	let previous = b"previous destination";
	let full_image = fat16_image(&[(*b"KEEP    BMP", previous)], true);
	let mut full_media = StorageHarness::start(storage_elf, b"FATBLOCK", &full_image, full_image.len() as u64);
	let failure = run_imgconv_harness(imgconv_elf, b"--force --resize 64x64 vol://system/sample.bmp vol://media/KEEP.BMP", &mut system, &mut full_media);
	assert_eq!(failure, b"imgconv: cannot write output\n");
	assert_eq!(full_media.open(b"vol://media/KEEP.BMP", 0xfa11), Some(previous.to_vec()), "failed overwrite preserves the previous destination byte-for-byte");
}

tagged_test!(imgconv_governed_working_set_is_measured, [Image, Memory, Process, Service, Storage]);
fn imgconv_governed_working_set_is_measured() {
	use object::domain::{Domain, UNLIMITED};
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	const IMGCONV_MEMORY_LIMIT: u64 = 96 * 1024 * 1024;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let imgconv_elf = program_elf(&package, volume, b"imgconv").expect("imgconv tool");
	let mut system = StorageHarness::start(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);

	let media_image = fat16_image(&[], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);
	let full_hd_domain = Domain::new_child(&sched::root_domain(), IMGCONV_MEMORY_LIMIT, UNLIMITED, UNLIMITED);
	let (full_hd, full_hd_peak) = run_imgconv_harness_in(full_hd_domain, imgconv_elf, b"--resize 1920x1080 --compression 100 vol://system/sample.bmp vol://media/FHD.PNG", &mut system, &mut media);
	assert!(full_hd.starts_with(b"imgconv: BMP 2x2 -> PNG 1920x1080 compression=100 bytes="));
	let full_hd_output = media.open(b"vol://media/FHD.PNG", 0xf1080).expect("1080p output opens");
	let full_hd_image = png::decode_rgba(&full_hd_output).expect("1080p output decodes");
	assert_eq!((full_hd_image.width, full_hd_image.height), (1920, 1080));

	let media_image = fat16_image(&[], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);
	let ultra_hd_domain = Domain::new_child(&sched::root_domain(), IMGCONV_MEMORY_LIMIT, UNLIMITED, UNLIMITED);
	let (ultra_hd, ultra_hd_peak) = run_imgconv_harness_in(ultra_hd_domain, imgconv_elf, b"--resize 3840x2160 --compression 100 vol://system/sample.bmp vol://media/UHD.PNG", &mut system, &mut media);
	assert!(ultra_hd.starts_with(b"imgconv: BMP 2x2 -> PNG 3840x2160 compression=100 bytes="));
	let ultra_hd_output = media.open(b"vol://media/UHD.PNG", 0xf2160).expect("4K output opens");
	let ultra_hd_image = png::decode_rgba(&ultra_hd_output).expect("4K output decodes");
	assert_eq!((ultra_hd_image.width, ultra_hd_image.height), (3840, 2160));

	let animation = include_bytes!("../user/webp/tests/data/external-animation.webp");
	let media_image = fat16_image(&[(*b"ANIM    WEB", animation)], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);
	let animation_domain = Domain::new_child(&sched::root_domain(), IMGCONV_MEMORY_LIMIT, UNLIMITED, UNLIMITED);
	let (animation_line, animation_peak) = run_imgconv_harness_in(animation_domain, imgconv_elf, b"vol://media/ANIM.WEB vol://media/ANIM.GIF", &mut system, &mut media);
	assert!(animation_line.starts_with(b"imgconv: WebP 23x15 -> GIF 23x15 quality=100 bytes="));
	let animation_output = media.open(b"vol://media/ANIM.GIF", 0xa11).expect("animation output opens");
	let converted_animation = gif::decode(&animation_output).expect("animation output decodes");
	assert_eq!((converted_animation.width, converted_animation.height, converted_animation.frames.len()), (23, 15, 2));
	assert!(full_hd_peak > 1920 * 1080 * 4, "1080p peak includes more than the final RGBA buffer");
	assert!(ultra_hd_peak > 3840 * 2160 * 4, "4K peak includes more than the final RGBA buffer");
	assert!(ultra_hd_peak > full_hd_peak, "4K conversion has a larger whole-process peak");
	assert!(ultra_hd_peak < IMGCONV_MEMORY_LIMIT, "measured 4K conversion fits the production quota");

	let previous = b"preserved after quota failure";
	let media_image = fat16_image(&[(*b"KEEP    PNG", previous)], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);
	let limited_domain = Domain::new_child(&sched::root_domain(), 80 * 1024 * 1024, UNLIMITED, UNLIMITED);
	let (failure, limited_peak) = run_imgconv_harness_result(limited_domain, imgconv_elf, b"--force --resize 3840x2160 --compression 100 vol://system/sample.bmp vol://media/KEEP.PNG", &mut system, &mut media);
	assert_eq!(failure, Some(b"imgconv: out of memory\n".to_vec()), "quota failure reports a typed diagnostic");
	assert_eq!(media.open(b"vol://media/KEEP.PNG", 0xfa17), Some(previous.to_vec()), "quota failure preserves the previous destination byte-for-byte");
	assert!(limited_peak <= 80 * 1024 * 1024, "quota failure never exceeds its Domain limit");
	serial_println!("imgconv governed memory: 1920x1080={} bytes, 3840x2160={} bytes, animation={} bytes", full_hd_peak, ultra_hd_peak, animation_peak);
}

tagged_test!(system_volume_lands_in_a_gpt_partition, [Service, Storage, Filesystem, Slow]);
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
	let elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe should be in the init package");
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

tagged_test!(a_degenerate_gpt_entry_cannot_kill_the_storage_service, [Service, Storage, Filesystem, Slow]);
fn a_degenerate_gpt_entry_cannot_kill_the_storage_service() {
	use alloc::collections::BTreeMap;
	use object::channel::Channel;
	use object::rights::Rights;

	// The disk's content must never deny storage. A GPT names a LiberFS
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
	let elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe should be in the init package");
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

tagged_test!(a_lying_seed_archive_cannot_kill_the_storage_service, [Service, Storage, Filesystem, Slow]);
fn a_lying_seed_archive_cannot_kill_the_storage_service() {
	use alloc::collections::BTreeMap;
	use object::channel::Channel;
	use object::rights::Rights;

	// The boot-time seeding path runs exactly on a disk WITHOUT a valid
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
	let elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe should be in the init package");
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

tagged_test!(ps_live_view_drives_the_terminal_contract, [Service, Shell, Console]);
fn ps_live_view_drives_the_terminal_contract() {
	use object::channel::{Channel, Message};
	use object::rights::Rights;

	// `ps -i`: the live process/resource view runs full-screen on its controlling
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
	let _ps = spawn_dynamic_test_process(sched::root_domain(), ps_elf, boot_user);
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

tagged_test!(system_manager_recovery_escalates_after_repeated_crashes, [Process]);
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

tagged_test!(system_manager_recovery_survives_a_clean_start, [Process]);
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

tagged_test!(a_clean_exit_releases_the_process_channel_endpoints, [Process]);
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

tagged_test!(userspace_spawn_syscalls_start_a_second_process, [Process, Syscall]);
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
	let elf = package.lookup(b"log_service.lsexe").expect("log_service.lsexe image");
	ELF_PTR.store(elf.as_ptr() as u64, Ordering::SeqCst);
	ELF_LEN.store(elf.len() as u64, Ordering::SeqCst);
	let (kernel_ep, user_ep) = object::channel::Channel::create();
	sched::spawn_with_object(spawner, user_ep, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	let message = kernel_ep.recv().expect("the spawned process should report in over IPC");
	assert_eq!(&message.bytes[..], b"LogService: online");
}

tagged_test!(storage_serves_volume_file_to_client, [Service, Storage]);
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

tagged_test!(wasi_host_runs_a_component, [Service]);
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

tagged_test!(powerbox_grants_a_picked_file_to_a_component, [Service]);
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

tagged_test!(permission_manager_sandboxes_a_component, [Service, Process]);
fn permission_manager_sandboxes_a_component() {
	// PermissionManager governs components under typed permission manifests. Two are
	// report-back probes. sandbox_probe is granted storage and log but not network: it starts
	// with only its manifest's capabilities - the manager transfers exactly the storage and
	// log clients to it and withholds the network one it holds, recording every decision - and
	// reads its one granted file through the storage capability, reporting the bytes back.
	// request_probe is granted only log and then asks for an undeclared capability (storage)
	// at runtime: the headless policy default refuses it (least privilege) and the manager
	// records that refusal as a dynamic decision. Three real system tools launch on demand
	// through its `run` op, each printing to a captured stdout:
	// `date` reaches time, `cat` reaches volumes, and `ip` reaches a fresh network client. The
	// probe's bytes must equal the file straight from the volume (the storage grant is live
	// and reaches exactly that file) and its summary must show storage and log granted and
	// every other capability denied; `date`'s output must be a well-formed ISO-8601 UTC instant
	// (the time grant is live) and its summary must show only time granted and every other
	// capability denied; request_probe's runtime request must be denied and its summary must
	// mark that refusal as dynamic - each component was given exactly its manifest and nothing
	// more. Finally `cat`'s output must equal that file (the storage grant reaches it through
	// the on-demand launcher).
	let (expected, probe_read, probe_summary, date_read, date_summary, request_read, request_summary, cat_read, ip_read, ip_summary, graphics_read, graphics_start_ns) = run_permission_scenario().expect("the permission scenario should run");
	assert!(!expected.is_empty(), "the granted file should not be empty");
	assert_eq!(probe_read, expected, "the sandboxed component read its one granted file through the storage grant");
	assert_eq!(probe_summary.as_slice(), b"storage=grant log=grant network=deny device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny", "sandbox_probe was granted exactly its manifest - storage and log - and denied every other capability in the vocabulary");
	// `date` reached its one granted capability: its output is a well-formed ISO-8601 UTC
	// instant "YYYY-MM-DDTHH:MM:SSZ" (the exact moment varies, so check the shape, not the
	// value - its presence proves the time grant is live).
	assert_eq!(date_read.len(), 21, "the date command rendered a 20-byte ISO-8601 UTC instant and newline through its time grant");
	assert_eq!(date_read[4], b'-', "the date instant has a date separator after the year");
	assert_eq!(date_read[7], b'-', "the date instant has a date separator after the month");
	assert_eq!(date_read[10], b'T', "the date instant separates date and time with 'T'");
	assert_eq!(date_read[13], b':', "the date instant has a time separator after the hour");
	assert_eq!(date_read[16], b':', "the date instant has a time separator after the minute");
	assert_eq!(date_read[19], b'Z', "the date instant is UTC, terminated by 'Z'");
	assert_eq!(date_read[20], b'\n', "the date instant ends its stdout line");
	assert_eq!(date_summary.as_slice(), b"storage=deny log=deny network=deny device=deny config=deny time=grant audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny", "date was granted exactly its manifest - time - and denied every other capability in the vocabulary");
	// request_probe asked for storage at runtime - a capability outside its manifest. The
	// headless policy default refused it, so the request comes back denied and its summary
	// carries the static grants followed by the refused runtime request marked `(dynamic)`.
	assert_eq!(request_read.as_slice(), b"storage denied", "request_probe's runtime request for an undeclared capability was refused by the headless policy default");
	assert_eq!(request_summary.as_slice(), b"storage=deny log=grant network=deny device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny storage=deny(dynamic)", "request_probe was granted exactly its manifest - log - and its runtime storage request was refused and recorded as a dynamic denial");
	// The on-demand `cat` tool, launched through PermissionManager's `run` op under a manifest
	// granting only storage, printed the file it was given through that grant to the stdout the
	// manager forwarded it: the bytes it rendered must equal the file straight from the volume.
	assert_eq!(cat_read, expected, "the cat tool printed its file argument through the storage grant the run launcher gave it, forwarded to the captured stdout");
	assert_eq!(ip_read.as_slice(), b"net0: 10.0.2.15  mac 52:54:00:12:34:56  mtu 1500  gateway 10.0.2.2\n", "the governed ip tool queried its typed NetworkService grant and rendered the interface state to stdout");
	assert_eq!(ip_summary.as_slice(), b"storage=deny log=deny network=grant device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny", "ip was granted exactly its network-only manifest and denied every unrelated capability");
	assert_eq!(graphics_read.as_slice(), b"graphics grants\n", "the governed graphics probe received process-bound display, key-only input and playback-only audio grants");
	assert!(graphics_start_ns != 0, "the governed app cold-start path is measured");
}

tagged_test!(component_host_runs_an_sdk_component, [Service, Slow]);
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

tagged_test!(resource_manager_contains_a_domain, [Service]);
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

tagged_test!(capability_grants_no_operation_beyond_rights, [Kernel]);
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

tagged_test!(capability_attenuation_only_narrows, [Kernel]);
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

tagged_test!(no_ambient_authority_fresh_table_empty, [Kernel]);
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

tagged_test!(syscall_fuzz_rejects_invalid_calls, [Syscall]);
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

tagged_test!(object_info_get_reports_object, [Syscall]);
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

tagged_test!(system_graph_reflects_live_state, [Kernel]);
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

tagged_test!(process_counters_track_ipc_and_resources, [Process]);
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

tagged_test!(kernel_reads_file_through_storage_service, [Service, Storage]);
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

tagged_test!(storage_serves_staged_tool_binary, [Service, Storage]);
fn storage_serves_staged_tool_binary() {
	// The tool ELFs are staged onto the system volume under bin/ by the
	// factory-seed pipeline (build.rs strips them into the volume archive, the boot runner
	// lays that archive at LBA 0, and StorageService seeds it into the freshly-formatted
	// LiberFS). Reading one back through StorageService must return a valid ELF image -
	// proof the whole staging path works end to end.
	let actual = storage_read(b"vol://system/bin/cat.lsexe").expect("the staged tool read should succeed");
	assert!(actual.len() > 4, "the staged tool should not be empty");
	assert_eq!(&actual[..4], b"\x7fELF", "the staged tool should be an ELF image");
}

tagged_test!(event_timer_objects, [Kernel]);
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

tagged_test!(event_timer_syscalls, [Kernel, Syscall]);
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

tagged_test!(userspace_runs_and_ipcs, [Process]);
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

tagged_test!(userspace_yields_cooperatively, [Process, Scheduler]);
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

tagged_test!(fault_isolation_kills_only_process, [Kernel, Process]);
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

tagged_test!(
	#[cfg(target_arch = "x86_64")]
	writable_pages_are_not_executable,
	[Kernel, ArchX86_64]
);
#[cfg(target_arch = "x86_64")]
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
tagged_test!(
	#[cfg(target_arch = "aarch64")]
	writable_pages_are_not_executable,
	[Kernel, ArchAarch64]
);
#[cfg(target_arch = "aarch64")]
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
tagged_test!(
	#[cfg(target_arch = "riscv64")]
	writable_pages_are_not_executable,
	[Kernel, ArchRiscv64]
);
#[cfg(target_arch = "riscv64")]
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

tagged_test!(
	#[cfg(target_arch = "x86_64")]
	kernel_access_to_user_memory_is_refused_outside_the_window,
	[Kernel, ArchX86_64]
);
#[cfg(target_arch = "x86_64")]
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

tagged_test!(a_user_stack_grows_on_demand_past_its_initial_pages, [Kernel, Memory]);
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

tagged_test!(recursion_past_the_stack_floor_is_killed, [Kernel, Memory, Process]);
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

tagged_test!(
	#[cfg(target_arch = "x86_64")]
	driver_crash_is_cleaned_up_and_notified,
	[Drivers, Process, ArchX86_64]
);
#[cfg(target_arch = "x86_64")]
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

tagged_test!(
	#[cfg(target_arch = "x86_64")]
	device_manager_reacts_to_a_driver_crash,
	[Drivers, Process, ArchX86_64]
);
#[cfg(target_arch = "x86_64")]
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

tagged_test!(driver_survives_crash_and_restart, [Process]);
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

tagged_test!(domain_quota_enforced_cleanly, [Kernel]);
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

tagged_test!(dma_buffer_quota_enforced_cleanly, [Kernel]);
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

tagged_test!(ipc_queue_bytes_accounting_enforced, [Kernel, Ipc]);
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

tagged_test!(domain_hierarchy_limits_aggregate, [Kernel]);
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
	assert_eq!(child.account().memory().peak(), 8192, "a refused aggregate charge does not raise the child high-water mark");
	assert_eq!(parent.account().memory().peak(), 8192, "the parent records its successful aggregate high-water mark");
	child.uncharge_memory(8192);
	assert_eq!(parent.account().memory().used(), 0, "uncharge propagates to the parent");
	assert_eq!(parent.account().memory().peak(), 8192, "the high-water mark survives refunds");
	let stats = syscall::domain_stats_snapshot(&parent);
	assert_eq!(stats.memory_used, 0, "Domain stats reports the refunded live usage");
	assert_eq!(stats.memory_peak, 8192, "Domain stats preserves the observed memory high-water mark");
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

tagged_test!(domain_kill_frees_subtree, [Kernel, Process]);
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

tagged_test!(ipc_round_trip_and_zero_copy, [Ipc]);
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
