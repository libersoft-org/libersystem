// ServiceManager - the userspace service supervisor.
//
// SystemManager spawns this program from the init package and hands it a bootstrap
// channel, then over it the init package itself as a shared buffer. ServiceManager
// maps the package and brings up the core services in dependency order: a service
// is started only once every service it depends on is up. Each service is spawned
// with its own report channel; ServiceManager waits for its "online" report,
// records its state, keeps that channel as the service's control channel, and
// relays the report up to SystemManager. After the whole set is up it exercises
// the stop path on a leaf service, then reports in itself.
//
// The supervisor is intentionally minimal for this milestone: start in dependency
// order, track each service's state, and stop a service on request. A restart
// policy and a heartbeat/watchdog are a later phase.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use proto::system::log;
use proto::system::network;
use proto::system::{Entry, Field, Severity};
use rt::*;

// A service in the boot manifest: its package entry name and the names of the
// services that must be up before it may start.
struct Service {
	name: &'static [u8],
	deps: &'static [&'static [u8]],
}

// The number of managed services. (A fixed size keeps the state array on the
// stack, which a no_std program with no heap needs.)
const N: usize = 10;

// The core service manifest. The array order is deliberately NOT the start order:
// DeviceManager, StorageService, and the shell are listed before LogService, but
// all depend (directly or transitively) on it, so the dependency resolver must
// start LogService first. This proves the ordering is driven by declared
// dependencies, not by manifest position. The shell is the last component up: it
// depends on StorageService, which it talks to over IPC.
const MANIFEST: [Service; N] = [Service { name: b"device_manager", deps: &[b"log_service"] }, Service { name: b"storage_service", deps: &[b"log_service", b"device_manager"] }, Service { name: b"network_service", deps: &[b"log_service", b"device_manager"] }, Service { name: b"shell", deps: &[b"storage_service", b"device_service", b"process_service", b"config_service", b"network_service", b"time_service", b"console_service"] }, Service { name: b"log_service", deps: &[] }, Service { name: b"device_service", deps: &[b"log_service"] }, Service { name: b"process_service", deps: &[b"log_service"] }, Service { name: b"config_service", deps: &[b"log_service"] }, Service { name: b"time_service", deps: &[b"log_service", b"network_service"] }, Service { name: b"console_service", deps: &[b"log_service", b"time_service"] }];

// The lifecycle state ServiceManager tracks for each service.
#[derive(Clone, Copy, PartialEq)]
enum State {
	// Not started yet (its dependencies are not all up).
	Pending,
	// Started and reported in.
	Running,
	// Stopped on request after having run.
	Stopped,
	// Could not be started, or did not report in.
	Failed,
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. receive the init package shared buffer and map it. Keep the handle so we
	//    can share the package with DeviceManager (which spawns drivers from it).
	let (pkg_handle, archive): (u64, &[u8]) = unsafe { recv_package(bootstrap, &mut buf) }.unwrap_or_else(|| exit());
	let pkg_len: usize = archive.len();
	let package: Package = Package::parse(archive).unwrap_or_else(|| exit());

	// 1b. receive (and release) the legacy ramdisk volume buffer. StorageService now
	//     reads its volume from the virtio-blk disk over the block service channel
	//     routed up from DeviceManager, so the embedded ramdisk is no longer used in
	//     the boot path; we drop the capability rather than delegate it. (The kernel's
	//     direct StorageService test still drives the older RAMDISK bootstrap mode.)
	match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 && &buf[..7] == b"RAMDISK" => unsafe {
			close(handle);
		},
		_ => exit(),
	};

	// 2. bring the services up in dependency order. Each pass starts every pending
	//    service whose dependencies are all Running; repeat until a pass makes no
	//    progress (everything started, or what is left is blocked on a failed or
	//    missing dependency). StorageService's service-channel client end is kept
	//    alive in `storage_client` so the service stays standing after it reports in.
	let mut state: [State; N] = [State::Pending; N];
	let mut channels: [u64; N] = [0u64; N];
	let mut storage_client: u64 = 0;
	let mut block_client: u64 = 0;
	let mut net_frames: u64 = 0;
	let mut net_client: u64 = 0;
	let mut time_client: u64 = 0;
	let mut console_client: u64 = 0;
	let mut log_client: u64 = 0;
	let mut device_client: u64 = 0;
	let mut process_client: u64 = 0;
	let mut config_client: u64 = 0;
	loop {
		let mut progress: bool = false;
		let mut i: usize = 0;
		while i < N {
			if state[i] == State::Pending && deps_satisfied(MANIFEST[i].deps, &state) {
				state[i] = unsafe { start_service(&package, MANIFEST[i].name, bootstrap, pkg_handle, pkg_len, &mut block_client, &mut net_frames, &mut net_client, &mut time_client, &mut console_client, &mut storage_client, &mut log_client, &mut device_client, &mut process_client, &mut config_client, &mut channels[i], &mut buf) };
				progress = true;
			}
			i += 1;
		}
		if !progress {
			break;
		}
	}

	// 3. exercise the stop path on a leaf service. DeviceManager is the safe choice:
	//    nothing depends on it, so stopping it does not tear down the running system
	//    (stopping log_service, storage_service, or the interactive shell would).
	//    This proves the supervisor can stop a service and track the transition, and
	//    the transition is recorded in the journal like the startup events.
	if let Some(dev) = index_of(b"device_manager") {
		if state[dev] == State::Running {
			state[dev] = unsafe { stop_service(channels[dev], bootstrap, &mut buf) };
			unsafe { emit_event(log_client, b"device_manager", b"stopped") };
		}
	}

	// 4. report in once the whole set has settled - every service either Running or,
	//    for the leaf we exercised the stop path on, Stopped, with none left Failed.
	//    Keep `storage_client` alive until exit in case the shell never took it (e.g.
	//    a spawn failure), so StorageService's service channel does not peer-close
	//    prematurely; once the shell owns it, the shell keeps the service standing.
	if all_settled(&state) {
		unsafe {
			send_blocking(bootstrap, b"ServiceManager: online", 0);
		}
	}
	let _ = storage_client;
	exit();
}

// Whether every service reached a healthy end state - Running, or Stopped for a
// service the supervisor deliberately stopped - with none left Failed or Pending.
// ServiceManager announces itself online only once its whole set is accounted for.
fn all_settled(state: &[State; N]) -> bool {
	let mut i: usize = 0;
	while i < N {
		if state[i] != State::Running && state[i] != State::Stopped {
			return false;
		}
		i += 1;
	}
	true
}

// Whether every dependency in `deps` is in the Running state.
fn deps_satisfied(deps: &[&[u8]], state: &[State; N]) -> bool {
	for &dep in deps {
		match index_of(dep) {
			Some(idx) if state[idx] == State::Running => {}
			_ => return false,
		}
	}
	true
}

// The manifest index of the service named `name`, if any.
fn index_of(name: &[u8]) -> Option<usize> {
	let mut i: usize = 0;
	while i < N {
		if MANIFEST[i].name == name {
			return Some(i);
		}
		i += 1;
	}
	None
}

// Start one service: look it up in the package, spawn it with a fresh report
// channel, wait for its "online" report, and relay that report up to `up`. Returns
// the resulting state (Running on success, Failed otherwise).
//
// Three services are bootstrapped specially before they report in: LogService is
// handed the channel its clients reach it on (we keep the client end in
// `*log_client`); StorageService needs the disk-backed block service channel and a
// service channel (we keep the client end in `*storage_client`); the shell needs
// both client channels - the StorageService one so its `cat` round-trips, the
// LogService one so its `log` command can query the journal. Once a service reports
// in, the supervisor records a structured "online" event in the journal.
unsafe fn start_service(package: &Package, name: &[u8], up: u64, pkg_handle: u64, pkg_len: usize, block_client: &mut u64, net_frames: &mut u64, net_client: &mut u64, time_client: &mut u64, console_client: &mut u64, storage_client: &mut u64, log_client: &mut u64, device_client: &mut u64, process_client: &mut u64, config_client: &mut u64, control: &mut u64, buf: &mut [u8]) -> State {
	unsafe {
		let elf: &[u8] = match package.lookup(name) {
			Some(e) => e,
			None => return State::Failed,
		};
		let (manager_side, service_side): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return State::Failed,
		};
		if spawn(elf, service_side) < 0 {
			return State::Failed;
		}
		if name == b"log_service" && !bootstrap_serve(manager_side, log_client) {
			return State::Failed;
		}
		if name == b"device_manager" && !bootstrap_package(manager_side, pkg_handle, pkg_len, buf) {
			return State::Failed;
		}
		if name == b"storage_service" && !bootstrap_storage(manager_side, *block_client, storage_client) {
			return State::Failed;
		}
		if name == b"device_service" && !bootstrap_serve(manager_side, device_client) {
			return State::Failed;
		}
		if name == b"process_service" && !bootstrap_process_service(manager_side, pkg_handle, pkg_len, process_client, buf) {
			return State::Failed;
		}
		if name == b"config_service" && !bootstrap_serve(manager_side, config_client) {
			return State::Failed;
		}
		if name == b"network_service" && !bootstrap_network_service(manager_side, *net_frames, net_client) {
			return State::Failed;
		}
		if name == b"time_service" && !bootstrap_time_service(manager_side, *net_client, time_client) {
			return State::Failed;
		}
		if name == b"console_service" && !bootstrap_console_service(manager_side, console_client) {
			return State::Failed;
		}
		if name == b"shell" && !bootstrap_shell(manager_side, *storage_client, *log_client, *device_client, *process_client, *config_client, *net_client, *time_client, *console_client, pkg_handle, pkg_len, buf) {
			return State::Failed;
		}
		match recv_blocking(manager_side, buf) {
			Received::Message { len, handle } => {
				// DeviceManager hands its block-read service channel up with its report;
				// keep it so StorageService can be bootstrapped against the disk.
				if name == b"device_manager" {
					*block_client = handle;
				}
				// Relay the service's own report up to SystemManager, in start order, and
				// keep its report channel as the control channel used to stop it later.
				send_blocking(up, &buf[..len], 0);
				*control = manager_side;
				// Record the lifecycle event in the journal (LogService is up by now).
				emit_event(*log_client, name, b"online");
				// DeviceManager sends a follow-up "NET" message carrying the net driver's
				// frame channel; keep it to bootstrap NetworkService against the driver.
				if name == b"device_manager" {
					if let Received::Message { handle: net, .. } = recv_blocking(manager_side, buf) {
						*net_frames = net;
					}
				}
				State::Running
			}
			Received::Closed => State::Failed,
		}
	}
}

// Emit one structured Entry to LogService over the `log_client` channel: an Info
// record tagged with the service `source` and an `event` field (e.g.
// "online"/"stopped"). A no-op until LogService is up (log_client == 0). The
// supervisor logs service lifecycle the way systemd journals unit start/stop.
unsafe fn emit_event(log_client: u64, source: &[u8], event: &[u8]) {
	if log_client == 0 {
		return;
	}
	let entry = Entry { timestamp: unsafe { clock() }, severity: Severity::Info, source: String::from_utf8_lossy(source).into_owned(), fields: alloc::vec![Field { key: String::from("event"), value: String::from_utf8_lossy(event).into_owned() }] };
	// Emit the record through the generated Log client (a round-trip over the log
	// channel); best-effort, so the result is ignored.
	let mut client = log::Client::new(ChannelTransport { chan: log_client });
	let _ = client.emit(&entry);
}

// Stop a running service over its control channel: send the "STOP" sentinel, then
// wait for the service's "stopped" acknowledgement and relay it up like its start
// report. Returns Stopped on a clean shutdown (or if the service was already gone).
unsafe fn stop_service(control: u64, up: u64, buf: &mut [u8]) -> State {
	unsafe {
		if control == 0 || !send_blocking(control, b"STOP", 0) {
			return State::Failed;
		}
		if let Received::Message { len, .. } = recv_blocking(control, buf) {
			send_blocking(up, &buf[..len], 0);
		}
		State::Stopped
	}
}

// Hand the shell the client channels it needs: the StorageService one (so its
// `cat` round-trips to storage over IPC), a LogService one (so its `log` command
// can query the journal), the DeviceService one (`dev`), the ProcessService one
// (`ps`/`run`), and the ConfigService one (`config`/`set`). All but the LogService
// client are transferred (the shell becomes their sole owner); the LogService
// client is *duplicated* and the copy transferred, since the supervisor keeps
// emitting on the original.
unsafe fn bootstrap_shell(manager_side: u64, storage_client: u64, log_client: u64, device_client: u64, process_client: u64, config_client: u64, net_client: u64, time_client: u64, console_client: u64, pkg_handle: u64, pkg_len: usize, buf: &mut [u8]) -> bool {
	unsafe {
		if !send_blocking(manager_side, b"STORAGE", storage_client) {
			return false;
		}
		let log_dup: i64 = duplicate(log_client, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		if log_dup < 0 {
			return false;
		}
		if !send_blocking(manager_side, b"LOG", log_dup as u64) {
			return false;
		}
		if !send_blocking(manager_side, b"DEVICE", device_client) {
			return false;
		}
		if !send_blocking(manager_side, b"PROCESS", process_client) {
			return false;
		}
		if !send_blocking(manager_side, b"CONFIG", config_client) {
			return false;
		}
		if !send_blocking(manager_side, b"NET", net_client) {
			return false;
		}
		if !send_blocking(manager_side, b"TIME", time_client) {
			return false;
		}
		if !send_blocking(manager_side, b"CONSOLE", console_client) {
			return false;
		}
		// The shell spawns foreground programs (echo, later the net tools) from the
		// init package, so hand it a read-only view of it like the other launchers.
		bootstrap_package(manager_side, pkg_handle, pkg_len, buf)
	}
}

// Hand a service a read-only view of the init package so it can spawn programs from
// it (DeviceManager spawns drivers, ProcessService launches programs): duplicate
// our package handle (read + map + transfer) and send "PACKAGE" + the byte length
// with the duplicate. We keep our own handle and mapping; the kernel allows the
// same object to be mapped in both address spaces.
unsafe fn bootstrap_package(manager_side: u64, pkg_handle: u64, pkg_len: usize, buf: &mut [u8]) -> bool {
	unsafe {
		let dup: i64 = duplicate(pkg_handle, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		if dup < 0 {
			return false;
		}
		buf[..7].copy_from_slice(b"PACKAGE");
		buf[7..15].copy_from_slice(&(pkg_len as u64).to_le_bytes());
		send_blocking(manager_side, &buf[..15], dup as u64)
	}
}

// Hand a service the channel its clients reach it on: create a fresh service channel
// and transfer one end with the "SERVE" tag, keeping the other end in `*client` for
// the supervisor to later hand to the shell. The shared bootstrap for every SERVE-
// only service (Log, Device, Config) and the tail of Storage and Process.
unsafe fn bootstrap_serve(manager_side: u64, client: &mut u64) -> bool {
	unsafe {
		let (service_server, service_client): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, b"SERVE", service_server) {
			return false;
		}
		*client = service_client;
		true
	}
}

// Hand ProcessService a read-only view of the init package (to launch programs
// from) and the channel its clients reach it on. The service-channel client end is
// kept in `*process_client` and later transferred to the shell for `ps`/`run`.
unsafe fn bootstrap_process_service(manager_side: u64, pkg_handle: u64, pkg_len: usize, process_client: &mut u64, buf: &mut [u8]) -> bool {
	unsafe { bootstrap_package(manager_side, pkg_handle, pkg_len, buf) && bootstrap_serve(manager_side, process_client) }
}

// Hand StorageService its disk-backed volume and a service channel over
// `manager_side`: "BLOCK" transferring the block-read service channel (routed up
// from the virtio-blk driver via DeviceManager), then "SERVE" transferring one end
// of a fresh service channel. The other end is stored in `*storage_client`.
unsafe fn bootstrap_storage(manager_side: u64, block_client: u64, storage_client: &mut u64) -> bool {
	unsafe {
		if !send_blocking(manager_side, b"BLOCK", block_client) {
			return false;
		}
		bootstrap_serve(manager_side, storage_client)
	}
}

// Hand NetworkService the net driver's frame channel ("FRAMES", routed up from the
// virtio-net driver via DeviceManager - it moves frames over it) and the channel
// its clients reach it on ("SERVE"). The service-channel client end is kept in
// `*net_client` and later transferred to the shell for the `ip`/`ping`/`nslookup`
// commands.
unsafe fn bootstrap_network_service(manager_side: u64, net_frames: u64, net_client: &mut u64) -> bool {
	unsafe {
		if !send_blocking(manager_side, b"FRAMES", net_frames) {
			return false;
		}
		bootstrap_serve(manager_side, net_client)
	}
}

// Hand TimeService its own NetworkService client (for the SNTP query) and the channel
// its clients reach it on. The network client is minted from the multi-client
// `network.open()` on the supervisor's NetworkService channel and transferred as
// "NET"; then "SERVE" transfers one end of a fresh service channel, the other kept in
// `*time_client` and later handed to the shell for the `date` command and `log`
// wall-clock rendering. (TimeService depends on network_service, so `net_client` is
// already set by the time this runs.)
unsafe fn bootstrap_time_service(manager_side: u64, net_client: u64, time_client: &mut u64) -> bool {
	unsafe {
		let mut net = network::Client::new(ChannelTransport { chan: net_client });
		let time_net: u64 = match net.open() {
			Some(Ok(h)) => h,
			_ => return false,
		};
		if !send_blocking(manager_side, b"NET", time_net) {
			return false;
		}
		bootstrap_serve(manager_side, time_client)
	}
}

// Hand ConsoleService the client end of a fresh console channel over "CLIENT": it is
// the terminal the shell talks to (the shell writes its output to it and reads its
// keystrokes from it). The other end is kept in `*console_client` and later handed to
// the shell as "CONSOLE". ConsoleService maps the framebuffer itself (the kernel
// console then stops drawing) and attaches to the kernel console input for keys.
unsafe fn bootstrap_console_service(manager_side: u64, console_client: &mut u64) -> bool {
	unsafe {
		let (service_end, client_end): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, b"CLIENT", service_end) {
			return false;
		}
		*console_client = client_end;
		true
	}
}
