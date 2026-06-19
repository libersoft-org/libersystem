// ServiceManager - the userspace service supervisor.
//
// SystemManager spawns this program from the init package and hands it a bootstrap
// channel, then over it the init package itself as a shared buffer. ServiceManager
// maps the package and brings up the core services in dependency order: a service
// is started only once every service it depends on is up. Each service is spawned
// with its own report channel; ServiceManager waits for its "online" report,
// records its state, and relays that report up to SystemManager. After the whole
// set is up it reports in itself.
//
// The supervisor is intentionally minimal for this milestone: start in dependency
// order and track each service's state. A restart policy and a heartbeat/watchdog
// are a later phase.

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
const N: usize = 3;

// The core service manifest. The array order is deliberately NOT the start order:
// DeviceManager and StorageService are listed before LogService but both depend on
// it, so the dependency resolver must start LogService first. This proves the
// ordering is driven by declared dependencies, not by manifest position.
const MANIFEST: [Service; N] = [Service { name: b"device_manager", deps: &[b"log_service"] }, Service { name: b"storage_service", deps: &[b"log_service"] }, Service { name: b"log_service", deps: &[] }];

// The lifecycle state ServiceManager tracks for each service.
#[derive(Clone, Copy, PartialEq)]
enum State {
	// Not started yet (its dependencies are not all up).
	Pending,
	// Started and reported in.
	Running,
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
	let mut storage_client: u64 = 0;
	loop {
		let mut progress: bool = false;
		let mut i: usize = 0;
		while i < N {
			if state[i] == State::Pending && deps_satisfied(MANIFEST[i].deps, &state) {
				state[i] = unsafe { start_service(&package, MANIFEST[i].name, bootstrap, ramdisk_handle, ramdisk_len, &mut storage_client, &mut buf) };
				progress = true;
			}
			i += 1;
		}
		if !progress {
			break;
		}
	}

	// 3. report in once the set is up. Keep `storage_client` alive until exit so
	//    StorageService's service channel does not peer-close out from under it.
	unsafe {
		send_blocking(bootstrap, b"ServiceManager: online", 0);
	}
	let _ = storage_client;
	exit();
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
// StorageService is bootstrapped specially: before it reports in, it needs the
// ramdisk volume and a service channel. We transfer the ramdisk capability and one
// end of a fresh service channel to it, keeping the client end in `*storage_client`
// so the service stays standing (a closed client end would peer-close its service).
unsafe fn start_service(package: &Package, name: &[u8], up: u64, ramdisk: u64, ramdisk_len: usize, storage_client: &mut u64, buf: &mut [u8]) -> State {
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
		match recv_blocking(manager_side, buf) {
			Received::Message { len, .. } => {
				// Relay the service's own report up to SystemManager, in start order.
				send_blocking(up, &buf[..len], 0);
				State::Running
			}
			Received::Closed => State::Failed,
		}
	}
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
