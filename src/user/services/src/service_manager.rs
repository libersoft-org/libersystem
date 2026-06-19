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

use rt::*;

// A service in the boot manifest: its package entry name and the names of the
// services that must be up before it may start.
struct Service {
	name: &'static [u8],
	deps: &'static [&'static [u8]],
}

// The number of managed services. (A fixed size keeps the state array on the
// stack, which a no_std program with no heap needs.)
const N: usize = 4;

// The core service manifest. The array order is deliberately NOT the start order:
// DeviceManager, StorageService, and the shell are listed before LogService, but
// all depend (directly or transitively) on it, so the dependency resolver must
// start LogService first. This proves the ordering is driven by declared
// dependencies, not by manifest position. The shell is the last component up: it
// depends on StorageService, which it talks to over IPC.
const MANIFEST: [Service; N] = [Service { name: b"device_manager", deps: &[b"log_service"] }, Service { name: b"storage_service", deps: &[b"log_service"] }, Service { name: b"shell", deps: &[b"storage_service"] }, Service { name: b"log_service", deps: &[] }];

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

	// 1. receive the init package shared buffer and map it.
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

	// 1b. receive the ramdisk volume buffer to hand to StorageService when it starts.
	let (ramdisk_handle, ramdisk_len): (u64, usize) = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 + 8 && &buf[..7] == b"RAMDISK" => {
			let length: usize = u64::from_le_bytes([buf[7], buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14]]) as usize;
			(handle, length)
		}
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
	loop {
		let mut progress: bool = false;
		let mut i: usize = 0;
		while i < N {
			if state[i] == State::Pending && deps_satisfied(MANIFEST[i].deps, &state) {
				state[i] = unsafe { start_service(&package, MANIFEST[i].name, bootstrap, ramdisk_handle, ramdisk_len, &mut storage_client, &mut channels[i], &mut buf) };
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
	//    This proves the supervisor can stop a service and track the transition.
	if let Some(dev) = index_of(b"device_manager") {
		if state[dev] == State::Running {
			state[dev] = unsafe { stop_service(channels[dev], bootstrap, &mut buf) };
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
// Two services are bootstrapped specially before they report in: StorageService
// needs the ramdisk volume and a service channel (we keep the client end in
// `*storage_client`); the shell needs that StorageService client channel, which we
// transfer to it so its `cat` can round-trip to the service over IPC.
unsafe fn start_service(package: &Package, name: &[u8], up: u64, ramdisk: u64, ramdisk_len: usize, storage_client: &mut u64, control: &mut u64, buf: &mut [u8]) -> State {
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
		if name == b"storage_service" && !bootstrap_storage(manager_side, ramdisk, ramdisk_len, storage_client, buf) {
			return State::Failed;
		}
		if name == b"shell" && !bootstrap_shell(manager_side, *storage_client) {
			return State::Failed;
		}
		match recv_blocking(manager_side, buf) {
			Received::Message { len, .. } => {
				// Relay the service's own report up to SystemManager, in start order, and
				// keep its report channel as the control channel used to stop it later.
				send_blocking(up, &buf[..len], 0);
				*control = manager_side;
				State::Running
			}
			Received::Closed => State::Failed,
		}
	}
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

// Hand the shell the StorageService client channel so it can talk to the service
// over IPC (its `cat` round-trips through it). Transfers `storage_client` to the
// shell over `manager_side`; the shell then owns that endpoint.
unsafe fn bootstrap_shell(manager_side: u64, storage_client: u64) -> bool {
	unsafe { send_blocking(manager_side, b"STORAGE", storage_client) }
}

// Hand StorageService its ramdisk and a service channel over `manager_side`:
// "RAMDISK" + length transferring the volume buffer, then "SERVE" transferring one
// end of a fresh service channel. The other end is stored in `*storage_client`.
unsafe fn bootstrap_storage(manager_side: u64, ramdisk: u64, ramdisk_len: usize, storage_client: &mut u64, buf: &mut [u8]) -> bool {
	unsafe {
		buf[..7].copy_from_slice(b"RAMDISK");
		buf[7..15].copy_from_slice(&(ramdisk_len as u64).to_le_bytes());
		if !send_blocking(manager_side, &buf[..15], ramdisk) {
			return false;
		}
		let (service_server, service_client): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, b"SERVE", service_server) {
			return false;
		}
		*storage_client = service_client;
		true
	}
}
