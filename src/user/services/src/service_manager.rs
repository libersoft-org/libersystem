// ServiceManager - the userspace service supervisor.
//
// SystemManager spawns this program from the init package and hands it a bootstrap
// channel, then over it the init package itself as a shared buffer. ServiceManager
// maps the package and brings up the core services in dependency order: a service
// is started only once every service it depends on is up. Each service is spawned
// with its own report channel; ServiceManager waits for its "online" report,
// records its state, keeps that channel as the service's control channel, and
// relays the report up to SystemManager. In a test boot (per the "MODE" flag the
// kernel sends down the chain) it then exercises the stop path on a leaf service
// and the restart policy and watchdog on a managed canary; a production boot skips
// those drills. It reports in itself and then stands as a supervisor for the life
// of the system.
//
// As a supervisor it does not exit after bring-up: it blocks on every live control
// channel at once and reacts when one needs it. A real service that crashes peer-
// closes its control channel, which the supervisor observes and records. The managed
// canary additionally proves the recovery machinery an unattended edge node depends
// on: a crashed canary is restarted per a back-off policy (escalating once a restart
// budget is spent), and a hung one - detected by a missed heartbeat - is killed and
// restarted. The shell can drive a reverse-dependency `stop <service>` over an admin
// channel (dependents stop before their dependencies), and SystemGraphService can
// query restart / watchdog counters over the `supervisor` interface for observability.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::log;
use proto::system::network;
use proto::system::process;
use proto::system::supervisor;
use proto::system::{Entry, Error, Field, Severity, SupervisorStat};
use rt::*;

// A service in the boot manifest: its package entry name and the names of the
// services that must be up before it may start.
struct Service {
	name: &'static [u8],
	deps: &'static [&'static [u8]],
}

// The number of managed services. (A fixed size keeps the state array on the
// stack, which a no_std program with no heap needs.)
const N: usize = 20;

// The core service manifest. The array order is deliberately NOT the start order:
// DeviceManager, StorageService, and the shell are listed before LogService, but
// all depend (directly or transitively) on it, so the dependency resolver must
// start LogService first. This proves the ordering is driven by declared
// dependencies, not by manifest position. SystemGraphService comes up after every
// component it observes (so it holds their process handles for the live graph), and
// the shell is the last component up: it depends on StorageService (which it talks to
// over IPC) and on SystemGraphService (whose graph its `graph` command renders).
const MANIFEST: [Service; N] = [
	Service { name: b"device_manager", deps: &[b"log_service"] },
	Service { name: b"storage_service", deps: &[b"log_service", b"device_manager"] },
	Service { name: b"media_storage", deps: &[b"log_service", b"device_manager"] },
	Service { name: b"iso_storage", deps: &[b"log_service", b"device_manager"] },
	Service { name: b"udf_storage", deps: &[b"log_service", b"device_manager"] },
	Service { name: b"usb_storage", deps: &[b"log_service", b"storage_service"] },
	Service { name: b"network_service", deps: &[b"log_service", b"device_manager", b"process_service"] },
	Service {
		name: b"shell",
		deps: &[
			b"storage_service",
			b"media_storage",
			b"iso_storage",
			b"udf_storage",
			b"usb_storage",
			b"device_service",
			b"process_service",
			b"config_service",
			b"network_service",
			b"time_service",
			b"console_service",
			b"audio_service",
			b"input_service",
			b"permission_manager",
			b"resource_manager",
			b"system_graph_service",
			b"session_service",
		],
	},
	Service { name: b"log_service", deps: &[] },
	Service { name: b"device_service", deps: &[b"log_service", b"process_service"] },
	Service { name: b"process_service", deps: &[b"log_service", b"storage_service"] },
	Service { name: b"config_service", deps: &[b"log_service", b"process_service"] },
	Service { name: b"time_service", deps: &[b"log_service", b"network_service", b"process_service"] },
	Service { name: b"console_service", deps: &[b"log_service", b"time_service", b"audio_service", b"input_service", b"session_service", b"process_service", b"permission_manager"] },
	Service { name: b"audio_service", deps: &[b"log_service", b"device_manager", b"process_service"] },
	Service { name: b"input_service", deps: &[b"log_service", b"device_manager", b"process_service"] },
	Service {
		name: b"system_graph_service",
		deps: &[
			b"log_service",
			b"device_manager",
			b"storage_service",
			b"network_service",
			b"device_service",
			b"process_service",
			b"config_service",
			b"time_service",
			b"console_service",
			b"audio_service",
			b"input_service",
			b"permission_manager",
			b"resource_manager",
		],
	},
	Service {
		name: b"permission_manager",
		deps: &[
			b"log_service",
			b"storage_service",
			b"media_storage",
			b"iso_storage",
			b"udf_storage",
			b"usb_storage",
			b"network_service",
			b"time_service",
			b"config_service",
			b"device_service",
			b"audio_service",
			b"process_service",
			b"resource_manager",
		],
	},
	Service { name: b"resource_manager", deps: &[b"log_service", b"process_service"] },
	Service { name: b"session_service", deps: &[b"log_service", b"process_service"] },
];

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

// The watchdog's heartbeat deadline: a healthy service answers a probe in one
// synchronous round-trip, far inside this window; a service that misses it is hung.
// (~1s at the 100-ticks-per-second monotonic clock.)
const WATCHDOG_TICKS: u64 = 100;

// The restart budget the supervisor spends on a managed service before it escalates
// (gives up and leaves it failed) rather than restarting it again.
const MAX_RESTARTS: u32 = 3;

// The base back-off between restart attempts, scaled by the attempt count so repeated
// failures wait progressively longer. A bounded one-shot sleep, idle-friendly under the
// test scheduler (which advances finite deadlines).
const RESTART_BACKOFF_TICKS: u64 = 10;

// What last took a managed component down, surfaced to observability as a string the
// component itself could not report (a crashed component reports nothing).
#[derive(Clone, Copy, PartialEq)]
enum Failure {
	None,
	Crashed,
	Hung,
	Stopped,
}

impl Failure {
	fn as_bytes(self) -> &'static [u8] {
		match self {
			Failure::None => b"",
			Failure::Crashed => b"crashed",
			Failure::Hung => b"hung",
			Failure::Stopped => b"stopped",
		}
	}
}

// The supervisor's per-component bookkeeping: how often it has had to intervene and
// why. Surfaced over the `supervisor` interface and folded into the System Graph.
#[derive(Clone, Copy)]
struct Supervised {
	restarts: u32,
	watchdog_trips: u32,
	failure: Failure,
}

impl Supervised {
	const fn new() -> Supervised {
		Supervised { restarts: 0, watchdog_trips: 0, failure: Failure::None }
	}
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

	// 1c. receive the boot mode flag ("MODE" + one byte, relayed down the chain from
	//     the kernel). The bring-up self-tests - the stop-path exercise and the canary
	//     crash / hang drills - run only in a test boot (1); a production boot (0)
	//     never deliberately faults a process or stops a service.
	let selftest: bool = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, .. } if len == 5 && &buf[..4] == b"MODE" => buf[4] == 1,
		_ => exit(),
	};

	// 2. bring the services up in dependency order. Each pass starts every pending
	//    service whose dependencies are all Running; repeat until a pass makes no
	//    progress (everything started, or what is left is blocked on a failed or
	//    missing dependency). StorageService's service-channel client end is kept
	//    alive in `storage_client` so the service stays standing after it reports in.
	let mut state: [State; N] = [State::Pending; N];
	let mut channels: [u64; N] = [0u64; N];
	// The spawned Process handle of each component, kept so SystemGraphService can be
	// handed a read-only duplicate of every component it observes (the live data source
	// for that component's graph node).
	let mut procs: [u64; N] = [0u64; N];
	let mut storage_client: u64 = 0;
	let mut block_client: u64 = 0;
	let mut block2_client: u64 = 0;
	let mut block3_client: u64 = 0;
	let mut media_client: u64 = 0;
	let mut iso_client: u64 = 0;
	let mut block4_client: u64 = 0;
	let mut udf_client: u64 = 0;
	// The USB stick's block channel, handed up by the xhci driver in DeviceManager's
	// phase 2 (0 when no mass-storage device is attached), and the usb StorageService
	// instance's client end minted when that instance bootstraps.
	let mut block5_client: u64 = 0;
	let mut usb_client: u64 = 0;
	// The xHCI driver's USB bus query channel (the typed `usb` inventory), handed up
	// with the phase-2 driver channels and granted to the `lsusb` command through
	// PermissionManager.
	let mut usbq_client: u64 = 0;
	let mut net_frames: u64 = 0;
	let mut net_client: u64 = 0;
	let mut gpu_client: u64 = 0;
	let mut snd_client: u64 = 0;
	let mut input_raw: u64 = 0;
	let mut input_client: u64 = 0;
	// The console end of the InputService -> ConsoleService pointer-forward channel,
	// minted when InputService bootstraps and handed to ConsoleService when it bootstraps
	// (InputService is a declared dependency of ConsoleService, so it starts first).
	let mut pointer_console: u64 = 0;
	let mut audio_client: u64 = 0;
	let mut time_client: u64 = 0;
	let mut console_client: u64 = 0;
	let mut console_control: u64 = 0;
	let mut log_client: u64 = 0;
	let mut device_client: u64 = 0;
	let mut process_client: u64 = 0;
	let mut config_client: u64 = 0;
	let mut graph_client: u64 = 0;
	// The PermissionManager service-channel client end, kept after PermissionManager
	// bootstraps and later handed to the shell for its `perm` command.
	let mut perm_client: u64 = 0;
	// The ResourceManager service-channel client end, kept after ResourceManager
	// bootstraps and later handed to the shell for its `usage` command.
	let mut res_client: u64 = 0;
	// The SessionService service-channel client end (the session factory), kept after
	// SessionService bootstraps and handed to ConsoleService (so it can mint a session
	// per VT it spawns); the shell's VT 1 session is minted from it into `session1`.
	let mut session_client: u64 = 0;
	// VT 1's session channel, minted once from the session factory and reused across
	// every restart of the VT 1 shell: the supervisor holds it, so the session - and thus
	// the cwd - survives a shell crash / restart. A fresh duplicate is handed to each
	// shell the supervisor (re)starts.
	let mut session1: u64 = 0;
	// The admin channel the shell drives `stop <service>` over (the supervisor keeps the
	// server end), and the channel SystemGraphService queries the `supervisor` interface
	// on (the supervisor serves it). Both are minted when the shell and SystemGraphService
	// bootstrap, and stood on in the supervise loop.
	let mut admin_server: u64 = 0;
	// A second admin channel: the one PermissionManager grants to the sandboxed `stop`
	// command (the supervisor capability), so `stop` run as its own ELF reaches the
	// supervisor's teardown path. The supervisor keeps this server end too and stands on
	// it alongside the shell's in the supervise loop; minted when PermissionManager
	// bootstraps.
	let mut admin_server2: u64 = 0;
	let mut stats_server: u64 = 0;
	// A second supervisor-status channel: the one PermissionManager grants to the sandboxed
	// `lssvc` command (the services capability), so `lssvc` run as its own ELF queries the
	// supervisor's typed status view. The supervisor keeps this server end and stands on it
	// alongside SystemGraphService's in the supervise loop; minted when PermissionManager
	// bootstraps.
	let mut stats_server2: u64 = 0;
	loop {
		let mut progress: bool = false;
		let mut i: usize = 0;
		while i < N {
			if state[i] == State::Pending && deps_satisfied(MANIFEST[i].deps, &state) {
				let mut proc_handle: u64 = 0;
				let started: State = unsafe { start_service(&package, MANIFEST[i].name, bootstrap, pkg_handle, pkg_len, &mut block_client, &mut block2_client, &mut block3_client, &mut block4_client, &mut block5_client, &mut media_client, &mut iso_client, &mut udf_client, &mut usb_client, &mut usbq_client, &mut net_frames, &mut net_client, &mut gpu_client, &mut snd_client, &mut audio_client, &mut time_client, &mut console_client, &mut console_control, &mut storage_client, &mut log_client, &mut device_client, &mut process_client, &mut config_client, &mut input_raw, &mut input_client, &mut pointer_console, &mut graph_client, &mut perm_client, &mut res_client, &mut session_client, &mut session1, &mut admin_server, &mut admin_server2, &mut stats_server, &mut stats_server2, &procs, &state, &mut proc_handle, &mut channels[i], &mut buf) };
				state[i] = started;
				procs[i] = proc_handle;
				progress = true;
				// M61 box 8: once the system StorageService is up, drive DeviceManager's phase
				// 2 - it now loads the non-bootstrap drivers from the volume, which is only
				// mountable now. This runs before the driver-consuming services (which depend
				// on process_service, so come up later), so their driver channels are ready.
				if MANIFEST[i].name == b"storage_service" && started == State::Running {
					if let Some(dm) = index_of(b"device_manager") {
						unsafe { drive_runtime_drivers(channels[dm], storage_client, &mut net_frames, &mut gpu_client, &mut snd_client, &mut input_raw, &mut block5_client, &mut usbq_client, &mut buf) };
					}
				}
			}
			i += 1;
		}
		if !progress {
			break;
		}
	}

	// The drivers' online facts, snapshotted after bring-up: a non-zero client channel
	// means the driver came up and handed its channel over (DeviceManager launched it and
	// it reported in). Folded into the supervisor's status view, so `lssvc` lists the
	// drivers alongside the managed services - drivers are services too.
	let driver_state: [(&'static [u8], bool); 6] = [
		(b"driver.virtio_blk", block_client != 0),
		(b"driver.virtio_net", net_frames != 0),
		(b"driver.virtio_gpu", gpu_client != 0),
		(b"driver.virtio_snd", snd_client != 0),
		(b"driver.virtio_input", input_raw != 0),
		(b"driver.xhci", block5_client != 0),
	];

	// 3. in a test boot, exercise the stop path on a leaf service. DeviceManager is the
	//    safe choice: nothing depends on it, so stopping it does not tear down the running
	//    system (stopping log_service, storage_service, or the interactive shell would).
	//    This proves the supervisor can stop a service and track the transition, and
	//    the transition is recorded in the journal like the startup events. A production
	//    boot skips the exercise and leaves DeviceManager running.
	let mut sup: [Supervised; N] = [Supervised::new(); N];
	if selftest {
		if let Some(dev) = index_of(b"device_manager") {
			if state[dev] == State::Running {
				state[dev] = unsafe { stop_service(channels[dev], bootstrap, &mut buf) };
				sup[dev].failure = Failure::Stopped;
				channels[dev] = 0;
				unsafe { emit_event(log_client, b"device_manager", b"stopped") };
			}
		}
	}

	// 3b. bring up the managed canary and, in a test boot, exercise the restart policy
	//     and the watchdog on it. The supervisor owns the canary outright (it is not in
	//     the manifest and no other component holds a channel to it), so it can crash it,
	//     hang it, and restart it without disturbing the running system - proving the
	//     unattended-recovery machinery an edge node depends on. A crash is detected by
	//     the control channel peer-closing and recovered by a policy restart with
	//     back-off; a hang is caught by a missed heartbeat and recovered by a kill +
	//     restart. Each transition is reported up the boot chain and journaled. A
	//     production boot spawns and supervises the canary but never deliberately faults
	//     it. (Transparent restart of a real service other components hold channels to
	//     needs a re-resolve/broker, deferred; the canary stands in for the policy, while
	//     crash detection below applies to every real service.)
	let (park, _park_peer): (u64, u64) = match unsafe { channel() } {
		Some(pair) => pair,
		None => (0, 0),
	};
	let mut canary_proc: u64 = 0;
	let mut canary_ctrl: u64 = 0;
	let mut canary_sup: Supervised = Supervised::new();
	unsafe {
		let (proc, ctrl): (u64, u64) = spawn_canary(&package, &mut buf);
		if proc != 0 {
			canary_proc = proc;
			canary_ctrl = ctrl;
			send_blocking(bootstrap, b"WatchdogProbe: online", 0);
			emit_event(log_client, b"watchdog_probe", b"online");
			if selftest {
				// prove the heartbeat path on a healthy canary: it answers the probe in time.
				let _alive: bool = heartbeat(canary_ctrl, clock() + WATCHDOG_TICKS);
				// crash -> restart: command a real fault, observe the peer-close, restart per policy.
				send_blocking(canary_ctrl, b"CRASH", 0);
				if restart_canary(&package, &mut canary_proc, &mut canary_ctrl, &mut canary_sup, Failure::Crashed, park, &mut buf) {
					send_blocking(bootstrap, b"WatchdogProbe: restarted", 0);
					emit_event(log_client, b"watchdog_probe", b"restarted");
				}
				// hang -> watchdog -> restart: command a hang, miss the heartbeat, kill the hung
				// process, then restart per policy.
				send_blocking(canary_ctrl, b"HANG", 0);
				if !heartbeat(canary_ctrl, clock() + WATCHDOG_TICKS) {
					canary_sup.watchdog_trips += 1;
					signal(canary_proc, SIG_KILL);
					if restart_canary(&package, &mut canary_proc, &mut canary_ctrl, &mut canary_sup, Failure::Hung, park, &mut buf) {
						send_blocking(bootstrap, b"WatchdogProbe: recovered", 0);
						emit_event(log_client, b"watchdog_probe", b"recovered");
					}
				}
			}
		}
	}

	// 4. report in once the whole set has settled - every service either Running or,
	//    for the leaf we exercised the stop path on, Stopped, with none left Failed.
	//    Keep `storage_client` alive in case the shell never took it (e.g. a spawn
	//    failure), so StorageService's service channel does not peer-close prematurely;
	//    once the shell owns it, the shell keeps the service standing.
	if all_settled(&state) {
		unsafe {
			send_blocking(bootstrap, b"ServiceManager: online", 0);
		}
	}
	let _ = storage_client;

	// 5. stand as the supervisor. Unlike the earlier milestone, ServiceManager does not
	//    exit after bring-up: it blocks on every live control channel at once so it can
	//    react when something needs it - a real service crashing (its channel peer-closes),
	//    the canary failing (restart per policy), the shell asking to `stop` a service
	//    (reverse-dependency teardown), or SystemGraphService querying the supervisor state.
	//    No timer stands here, so the loop sleeps at ~0% CPU until an event arrives.
	unsafe {
		supervise(&mut state, &mut channels, &mut sup, &procs, &package, &mut canary_proc, &mut canary_ctrl, &mut canary_sup, admin_server, admin_server2, stats_server, stats_server2, &driver_state, log_client, bootstrap, park, &mut buf);
	}
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

// The pinned bootstrap set (M61 box 8): the services ServiceManager raw-spawns from the
// init package, because they are on the path to mounting the system volume and so cannot
// be loaded from it. media / iso / udf storage reuse the pinned storage_service binary.
// Every other service is loaded from the volume's `bin/` through ProcessService.
fn is_pinned(name: &[u8]) -> bool {
	matches!(name, b"log_service" | b"device_manager" | b"storage_service" | b"media_storage" | b"iso_storage" | b"udf_storage" | b"usb_storage" | b"process_service")
}

// Load a non-pinned service from the system volume's `bin/` through ProcessService,
// handing the new process `bootstrap` as its bootstrap channel. Mints a dedicated
// launcher connection to the `process` factory (so the client end kept for the shell
// stays pristine). Returns the new process handle, or a negative value on failure.
unsafe fn launch_from_volume(process_client: u64, name: &[u8], bootstrap: u64) -> i64 {
	unsafe {
		if process_client == 0 {
			return -1;
		}
		let name_str: &str = match core::str::from_utf8(name) {
			Ok(s) => s,
			Err(_) => return -1,
		};
		let launcher: u64 = match service_connect(process_client) {
			Some(h) => h,
			None => return -1,
		};
		let started = process::Client::new(ChannelTransport { chan: launcher }).launch(name_str, &bootstrap);
		close(launcher);
		match started {
			Some(Ok(s)) => s.task as i64,
			_ => -1,
		}
	}
}

// Drive DeviceManager's phase 2 (M61 box 8): now that the system volume is mounted, hand
// it a fresh StorageService connection over its control channel with a "DRIVERS" message,
// so it loads the non-bootstrap drivers from vol://system/drivers/ and hands their channels
// back - the net driver's frame channel, the gpu display channel, the snd control channel,
// the pointer event channel, the USB stick's block channel (each 0 when that device is
// absent), and the xHCI driver's USB bus query channel (the `lsusb` inventory). Kept for
// bootstrapping NetworkService, ConsoleService, AudioService, InputService, the usb
// StorageService instance and PermissionManager's `usb` grant against the drivers.
unsafe fn drive_runtime_drivers(dm_control: u64, storage_client: u64, net_frames: &mut u64, gpu_client: &mut u64, snd_client: &mut u64, input_raw: &mut u64, block5_client: &mut u64, usbq_client: &mut u64, buf: &mut [u8]) {
	unsafe {
		if dm_control == 0 {
			return;
		}
		let storage: u64 = service_connect(storage_client).unwrap_or(0);
		if !send_blocking(dm_control, b"DRIVERS", storage) {
			return;
		}
		if let Received::Message { handle: net, .. } = recv_blocking(dm_control, buf) {
			*net_frames = net;
		}
		if let Received::Message { handle: gpu, .. } = recv_blocking(dm_control, buf) {
			*gpu_client = gpu;
		}
		if let Received::Message { handle: snd, .. } = recv_blocking(dm_control, buf) {
			*snd_client = snd;
		}
		if let Received::Message { handle: input, .. } = recv_blocking(dm_control, buf) {
			*input_raw = input;
		}
		if let Received::Message { handle: usb, .. } = recv_blocking(dm_control, buf) {
			*block5_client = usb;
		}
		if let Received::Message { handle: usbq, .. } = recv_blocking(dm_control, buf) {
			*usbq_client = usbq;
		}
	}
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
unsafe fn start_service(package: &Package, name: &[u8], up: u64, pkg_handle: u64, pkg_len: usize, block_client: &mut u64, block2_client: &mut u64, block3_client: &mut u64, block4_client: &mut u64, block5_client: &mut u64, media_client: &mut u64, iso_client: &mut u64, udf_client: &mut u64, usb_client: &mut u64, usbq_client: &mut u64, net_frames: &mut u64, net_client: &mut u64, gpu_client: &mut u64, snd_client: &mut u64, audio_client: &mut u64, time_client: &mut u64, console_client: &mut u64, console_control: &mut u64, storage_client: &mut u64, log_client: &mut u64, device_client: &mut u64, process_client: &mut u64, config_client: &mut u64, input_raw: &mut u64, input_client: &mut u64, pointer_console: &mut u64, graph_client: &mut u64, perm_client: &mut u64, res_client: &mut u64, session_client: &mut u64, session1: &mut u64, admin_server: &mut u64, admin_server2: &mut u64, stats_server: &mut u64, stats_server2: &mut u64, procs: &[u64; N], state: &[State; N], proc_out: &mut u64, control: &mut u64, buf: &mut [u8]) -> State {
	unsafe {
		let (manager_side, service_side): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return State::Failed,
		};
		// The pinned bootstrap set is raw-spawned from the init package (it is on the path
		// to mounting the system volume, so it cannot load from it); every other service is
		// loaded from the volume's `bin/` through ProcessService (M61 box 8). media / iso /
		// udf storage are extra instances of the pinned storage_service binary.
		let proc: i64 = if is_pinned(name) {
			let elf_name: &[u8] = if name == b"media_storage" || name == b"iso_storage" || name == b"udf_storage" || name == b"usb_storage" { b"storage_service" } else { name };
			match package.lookup(elf_name) {
				Some(elf) => spawn(elf, service_side),
				None => return State::Failed,
			}
		} else {
			launch_from_volume(*process_client, name, service_side)
		};
		if proc < 0 {
			return State::Failed;
		}
		// Keep the spawned Process handle so SystemGraphService can be handed a read-only
		// duplicate of it (the live data source for this component's graph node).
		*proc_out = proc as u64;
		if name == b"log_service" && !bootstrap_serve(manager_side, log_client) {
			return State::Failed;
		}
		if name == b"device_manager" && !bootstrap_package(manager_side, pkg_handle, pkg_len, buf) {
			return State::Failed;
		}
		if name == b"storage_service" && !bootstrap_storage(manager_side, *block_client, storage_client) {
			return State::Failed;
		}
		if name == b"media_storage" && !bootstrap_media_storage(manager_side, *block2_client, media_client) {
			return State::Failed;
		}
		if name == b"iso_storage" && !bootstrap_iso_storage(manager_side, *block3_client, iso_client) {
			return State::Failed;
		}
		if name == b"udf_storage" && !bootstrap_udf_storage(manager_side, *block4_client, udf_client) {
			return State::Failed;
		}
		if name == b"usb_storage" && !bootstrap_usb_storage(manager_side, *block5_client, usb_client) {
			return State::Failed;
		}
		if name == b"device_service" && !bootstrap_serve(manager_side, device_client) {
			return State::Failed;
		}
		if name == b"process_service" && !bootstrap_process_service(manager_side, pkg_handle, pkg_len, *storage_client, process_client, buf) {
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
		if name == b"audio_service" && !bootstrap_audio_service(manager_side, *snd_client, audio_client) {
			return State::Failed;
		}
		if name == b"input_service" && !bootstrap_input(manager_side, *input_raw, input_client, pointer_console) {
			return State::Failed;
		}
		if name == b"console_service" && !bootstrap_console_service(manager_side, *storage_client, *log_client, *device_client, *process_client, *config_client, *net_client, *gpu_client, *time_client, *audio_client, *session_client, *perm_client, *pointer_console, console_client, console_control) {
			return State::Failed;
		}
		if name == b"system_graph_service" && !bootstrap_system_graph_service(manager_side, procs, state, *device_client, graph_client, stats_server) {
			return State::Failed;
		}
		if name == b"permission_manager" && !bootstrap_permission_manager(manager_side, *storage_client, *media_client, *iso_client, *udf_client, *usb_client, *usbq_client, *log_client, *net_client, *time_client, *config_client, *device_client, *audio_client, *res_client, *process_client, perm_client, admin_server2, stats_server2) {
			return State::Failed;
		}
		if name == b"resource_manager" && !bootstrap_resource_manager(manager_side, res_client, pkg_handle, pkg_len, buf) {
			return State::Failed;
		}
		if name == b"session_service" && !bootstrap_serve(manager_side, session_client) {
			return State::Failed;
		}
		if name == b"shell" && !bootstrap_shell(manager_side, *storage_client, *media_client, *iso_client, *udf_client, *usb_client, *log_client, *device_client, *process_client, *config_client, *net_client, *time_client, *audio_client, *input_client, *console_client, *console_control, *graph_client, *perm_client, *res_client, *session_client, session1, admin_server) {
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
				// The shell is reaped by its console channel closing when it logs out
				// (Ctrl+D) or exits; release our Process handle to it so a clean exit
				// drops its handle table - and thus that channel. A leaked handle would
				// pin the shell alive forever, so the console could never reap the VT.
				// (Every other service is meant to stand for the life of the system.)
				if name == b"shell" {
					close(proc as u64);
					*proc_out = 0;
				}
				// DeviceManager sends a follow-up "BLOCK2" message carrying the second disk's
				// block service channel, then "BLOCK3" and "BLOCK4" for the third and fourth
				// disks; keep them to bootstrap the media / iso / udf StorageService instances
				// (each handle is 0 when that disk is absent). The net / gpu / snd / input
				// driver channels arrive later, in DeviceManager's phase 2, once the volume they
				// load from is mounted (driven right after StorageService comes up, below).
				if name == b"device_manager" {
					if let Received::Message { handle: block2, .. } = recv_blocking(manager_side, buf) {
						*block2_client = block2;
					}
					if let Received::Message { handle: block3, .. } = recv_blocking(manager_side, buf) {
						*block3_client = block3;
					}
					if let Received::Message { handle: block4, .. } = recv_blocking(manager_side, buf) {
						*block4_client = block4;
					}
				}
				// PermissionManager follows its "online" report with the sandbox proof: the
				// bytes the sandboxed component read through its one granted capability, then a
				// decisions summary of exactly which capabilities it was and was not given.
				// These are the manager's internal verification (and are asserted by the
				// kernel's permission scenario); the live audit trail is served over the
				// Permission contract and read with `perm`, so they are drained here rather
				// than relayed into the boot chain, which carries only state reports.
				if name == b"permission_manager" {
					let _ = recv_blocking(manager_side, buf);
					let _ = recv_blocking(manager_side, buf);
				}
				// ResourceManager follows its "online" report with the budget proof: a summary of
				// the pages it granted under the cap, the over-budget refusal it contained, and the
				// pages it regranted after raising the budget at runtime. This is the manager's
				// internal verification (and is asserted by the kernel's resource scenario); the
				// live budgets are served over the resources contract and read with `usage`, so it
				// is drained here rather than relayed into the boot chain, which carries only state
				// reports.
				if name == b"resource_manager" {
					let _ = recv_blocking(manager_side, buf);
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

// Mirror a runtime service transition to the debug console, so an operator watching
// the console sees a service stop, crash, or restart the moment it happens - the
// journal carries the same event for `log`, but a state change must never be silent.
// Bring-up reports are not mirrored here: the boot chain already prints those.
unsafe fn console_report(source: &[u8], event: &[u8]) {
	let mut line: Vec<u8> = Vec::new();
	line.extend_from_slice(b"supervisor: ");
	line.extend_from_slice(source);
	line.push(b' ');
	line.extend_from_slice(event);
	line.push(b'\n');
	unsafe { print(&line) };
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
// emitting on the original. Finally the supervisor mints a fresh ADMIN channel and
// transfers the client end to the shell (so its `stop <service>` command can drive
// reverse-dependency teardown), keeping the server end in `*admin_server` to serve.
unsafe fn bootstrap_shell(manager_side: u64, storage_client: u64, media_client: u64, iso_client: u64, udf_client: u64, usb_client: u64, log_client: u64, device_client: u64, process_client: u64, config_client: u64, net_client: u64, time_client: u64, audio_client: u64, input_client: u64, console_client: u64, console_control: u64, graph_client: u64, perm_client: u64, res_client: u64, session_client: u64, session1: &mut u64, admin_server: &mut u64) -> bool {
	unsafe {
		if !send_blocking(manager_side, b"STORAGE", storage_client) {
			return false;
		}
		if !send_blocking(manager_side, b"MEDIA", media_client) {
			return false;
		}
		if !send_blocking(manager_side, b"ISO", iso_client) {
			return false;
		}
		if !send_blocking(manager_side, b"UDF", udf_client) {
			return false;
		}
		if !send_blocking(manager_side, b"USB", usb_client) {
			return false;
		}
		let log_dup: i64 = duplicate(log_client, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		if log_dup < 0 {
			return false;
		}
		if !send_blocking(manager_side, b"LOG", log_dup as u64) {
			return false;
		}
		// The device / config / time / audio / resource clients are the serve ROOTS of
		// their services (`serve_multi` ends when the root closes), and the thin-launcher
		// shell closes them on receipt (governed tools reach these services through
		// PermissionManager's sub-connections instead). Hand the shell duplicates - like
		// LOG above - so the supervisor keeps every root alive for the life of the system;
		// transferring the originals let the shell's close tear the services down.
		let device_dup: i64 = duplicate(device_client, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		if device_dup < 0 {
			return false;
		}
		if !send_blocking(manager_side, b"DEVICE", device_dup as u64) {
			return false;
		}
		if !send_blocking(manager_side, b"PROCESS", process_client) {
			return false;
		}
		let config_dup: i64 = duplicate(config_client, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		if config_dup < 0 {
			return false;
		}
		if !send_blocking(manager_side, b"CONFIG", config_dup as u64) {
			return false;
		}
		if !send_blocking(manager_side, b"NET", net_client) {
			return false;
		}
		let time_dup: i64 = duplicate(time_client, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		if time_dup < 0 {
			return false;
		}
		if !send_blocking(manager_side, b"TIME", time_dup as u64) {
			return false;
		}
		let audio_dup: i64 = duplicate(audio_client, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		if audio_dup < 0 {
			return false;
		}
		if !send_blocking(manager_side, b"AUDIO", audio_dup as u64) {
			return false;
		}
		if !send_blocking(manager_side, b"INPUT", input_client) {
			return false;
		}
		// The SystemGraphService client, so the shell's `graph` command can render the live
		// system graph. Sent right after INPUT to match the shell's receive order.
		if !send_blocking(manager_side, b"GRAPH", graph_client) {
			return false;
		}
		// The PermissionManager client, so the shell's `perm` command can render the
		// permission audit trail. Sent right after GRAPH to match the shell's receive order.
		if !send_blocking(manager_side, b"PERM", perm_client) {
			return false;
		}
		// The ResourceManager client, so the shell's `usage` command can render the live
		// per-Domain budgets. Sent right after PERM to match the shell's receive order - a
		// duplicate, like the other launcher-dropped clients above, so the supervisor keeps
		// the serve root.
		let res_dup: i64 = duplicate(res_client, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		if res_dup < 0 {
			return false;
		}
		if !send_blocking(manager_side, b"RESOURCE", res_dup as u64) {
			return false;
		}
		// VT 1's session capability. The session is minted once from the session factory
		// and kept in `*session1` for the life of the system, so it - and thus the cwd -
		// survives a restart of the VT 1 shell; each (re)started shell receives a fresh
		// transferable duplicate. Sent right after RESOURCE to match the shell's receive
		// order.
		if *session1 == 0 {
			*session1 = match service_connect(session_client) {
				Some(h) => h,
				None => return false,
			};
		}
		let session_dup: i64 = duplicate(*session1, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		if session_dup < 0 {
			return false;
		}
		if !send_blocking(manager_side, b"SESSION", session_dup as u64) {
			return false;
		}
		if !send_blocking(manager_side, b"CONSOLE", console_client) {
			return false;
		}
		// VT 1's control channel to ConsoleService (the shell end; the console holds the
		// other end). Carries SET_FG / CLEAR_FG out and JOB_STOPPED back for job-control
		// signals, sent right after CONSOLE to match the shell's receive order.
		if !send_blocking(manager_side, b"CONTROL", console_control) {
			return false;
		}
		// A fresh ADMIN channel the shell drives `stop <service>` over: the supervisor
		// keeps the server end (in `*admin_server`) and stands on it in the supervise
		// loop; the client end is transferred to the shell, which receives it last.
		let (admin_srv, admin_cli): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, b"ADMIN", admin_cli) {
			return false;
		}
		*admin_server = admin_srv;
		true
	}
}

// Register every observed component with SystemGraphService and hand it the live data
// sources for the graph: one "NODE" message per Running component (excluding the shell
// and SystemGraphService itself), carrying the component's name and its declared
// dependency edges as the payload and a read-only duplicate of that component's Process
// as the transferred handle (the source of its live counters and state), then a
// dedicated DeviceService connection ("DEVICE") for the device nodes, then a fresh
// "SUPERVISOR" channel (the supervisor keeps the server end in `*stats_server` and
// serves the supervisor interface on it, so SystemGraphService can merge restart /
// watchdog counters into the graph), and finally the channel its clients reach it on
// ("SERVE"), kept in `*graph_client` for the shell. SystemGraphService comes up after
// every component it observes, so their handles are all captured and their state is
// Running when its node set is built.
unsafe fn bootstrap_system_graph_service(manager_side: u64, procs: &[u64; N], state: &[State; N], device_client: u64, graph_client: &mut u64, stats_server: &mut u64) -> bool {
	unsafe {
		let mut i: usize = 0;
		while i < N {
			let name: &[u8] = MANIFEST[i].name;
			if state[i] == State::Running && procs[i] != 0 && name != b"shell" && name != b"system_graph_service" {
				let dup: i64 = duplicate(procs[i], RIGHT_READ | RIGHT_TRANSFER);
				if dup < 0 {
					return false;
				}
				let mut payload: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
				payload.extend_from_slice(b"NODE");
				payload.extend_from_slice(name);
				payload.push(b'\n');
				let mut first: bool = true;
				for &dep in MANIFEST[i].deps {
					if !first {
						payload.push(b',');
					}
					payload.extend_from_slice(dep);
					first = false;
				}
				if !send_blocking(manager_side, &payload, dup as u64) {
					return false;
				}
			}
			i += 1;
		}
		// A dedicated DeviceService connection for the device nodes, minted from the
		// supervisor's DeviceService client so it never races the shell's own connection.
		match service_connect(device_client) {
			Some(dev) => {
				if !send_blocking(manager_side, b"DEVICE", dev) {
					return false;
				}
			}
			None => return false,
		}
		// A fresh SUPERVISOR channel: the supervisor keeps the server end (in
		// `*stats_server`) and serves the supervisor interface on it, so SystemGraphService
		// can query restart / watchdog counters and merge them into the graph. Sent right
		// after DEVICE to match SystemGraphService's receive order.
		let (stats_srv, stats_cli): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, b"SUPERVISOR", stats_cli) {
			return false;
		}
		*stats_server = stats_srv;
		// The channel its clients (the shell) reach it on; the client end is kept in
		// `*graph_client` for the shell's own bootstrap.
		bootstrap_serve(manager_side, graph_client)
	}
}

// Hand PermissionManager the clients it may grant onward - a fresh StorageService
// connection, a duplicable LogService client, a fresh NetworkService connection (so it
// holds, and can be seen to withhold, a capability it possesses), a fresh TimeService
// connection (the one capability the governed `date` command may reach), then fresh
// ConfigService, DeviceService, and AudioService connections (the capabilities the governed
// `config` / `set`, `lsdev`, and `beep` commands may reach) - then a fresh ProcessService
// connection (the loading mechanism it drives to start the components it governs) and the
// channel its clients reach it on ("SERVE", the client end kept in `*perm_client` for the
// shell's `perm` command). The order matches PermissionManager's receive order: STORAGE,
// LOG, NETWORK, TIME, CONFIG, DEVICE, AUDIO, RESOURCE, PROCESS_GRANT, PROCESS, SERVE. The
// grantable clients carry RIGHT_DUPLICATE so the manager can attenuate and hand a strictly
// narrower client to each component it sandboxes. (The grantable permission capability - a
// connection to the manager's own serve channel - is not passed here: the manager mints that
// self-connection itself.)
unsafe fn bootstrap_permission_manager(manager_side: u64, storage_client: u64, media_client: u64, iso_client: u64, udf_client: u64, usb_client: u64, usbq_client: u64, log_client: u64, net_client: u64, time_client: u64, config_client: u64, device_client: u64, audio_client: u64, resource_client: u64, process_client: u64, perm_client: &mut u64, admin_server2: &mut u64, stats_server2: &mut u64) -> bool {
	unsafe {
		// A fresh StorageService connection for the manager (independent of the shell's),
		// duplicable so the manager can grant a narrowed copy to a sandboxed component.
		let storage: u64 = match service_connect(storage_client) {
			Some(h) => h,
			None => return false,
		};
		if !send_blocking(manager_side, b"STORAGE", storage) {
			return false;
		}
		// A duplicable LogService client, so the manager can grant a narrowed copy.
		let log_dup: i64 = duplicate(log_client, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER | RIGHT_DUPLICATE);
		if log_dup < 0 || !send_blocking(manager_side, b"LOG", log_dup as u64) {
			return false;
		}
		// A fresh NetworkService connection the manager holds but withholds from the
		// sandboxed probe (whose manifest does not grant network) - the policy actively
		// declines to pass on a capability it possesses.
		let mut net = network::Client::new(ChannelTransport { chan: net_client });
		let perm_net: u64 = match net.open() {
			Some(Ok(h)) => h,
			_ => return false,
		};
		if !send_blocking(manager_side, b"NETWORK", perm_net) {
			return false;
		}
		// A fresh TimeService connection the manager grants to the governed `date` command
		// (whose manifest grants time) - the one capability that command is allowed to reach.
		let time_conn: u64 = match service_connect(time_client) {
			Some(h) => h,
			None => return false,
		};
		if !send_blocking(manager_side, b"TIME", time_conn) {
			return false;
		}
		// A fresh ConfigService connection the manager grants to the governed `config` / `set`
		// commands (whose manifests grant config).
		let config_conn: u64 = match service_connect(config_client) {
			Some(h) => h,
			None => return false,
		};
		if !send_blocking(manager_side, b"CONFIG", config_conn) {
			return false;
		}
		// A fresh DeviceService connection the manager grants to the governed `dev` command
		// (whose manifest grants device).
		let device_conn: u64 = match service_connect(device_client) {
			Some(h) => h,
			None => return false,
		};
		if !send_blocking(manager_side, b"DEVICE", device_conn) {
			return false;
		}
		// A fresh AudioService connection the manager grants to the governed `beep` command
		// (whose manifest grants audio).
		let audio_conn: u64 = match service_connect(audio_client) {
			Some(h) => h,
			None => return false,
		};
		if !send_blocking(manager_side, b"AUDIO", audio_conn) {
			return false;
		}
		// A fresh ResourceManager connection the manager grants to the governed `usage` command
		// (whose manifest grants resource).
		let resource_conn: u64 = match service_connect(resource_client) {
			Some(h) => h,
			None => return false,
		};
		if !send_blocking(manager_side, b"RESOURCE", resource_conn) {
			return false;
		}
		// A fresh ProcessService connection the manager grants to the governed `ps` command
		// (whose manifest grants process) - a dedicated connection, separate from the launch
		// mechanism below, so a granted tool's queries never interleave with the manager's loads.
		let process_grant: u64 = match service_connect(process_client) {
			Some(h) => h,
			None => return false,
		};
		if !send_blocking(manager_side, b"PROCESS_GRANT", process_grant) {
			return false;
		}
		// A fresh admin channel the manager grants to the governed `stop` command (whose
		// manifest grants supervisor): the supervisor keeps the server end (in `*admin_server2`)
		// and stands on it in the supervise loop, while the client end is handed to the manager,
		// which duplicates a narrowed copy onto the sandboxed `stop` tool. A dedicated channel,
		// separate from the shell's own admin channel, so a granted tool's teardown requests
		// never race the shell's built-in `stop`.
		let (admin_srv2, admin_cli2): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, b"SUPERVISOR", admin_cli2) {
			return false;
		}
		*admin_server2 = admin_srv2;
		// Four fresh non-system volume StorageService connections the manager bundles with the
		// system `storage` client under the `volumes` capability it grants the governed `lsvol`
		// command: media (FAT/exFAT), iso (ISO9660), udf (UDF), usb (FAT off the USB stick).
		// Each is minted off the volume's own service factory; a volume whose disk is absent
		// has no factory (its client is 0) and is handed over as 0, which `lsvol` shows as
		// zero files.
		let media_conn: u64 = service_connect(media_client).unwrap_or(0);
		if !send_blocking(manager_side, b"STORAGE_MEDIA", media_conn) {
			return false;
		}
		let iso_conn: u64 = service_connect(iso_client).unwrap_or(0);
		if !send_blocking(manager_side, b"STORAGE_ISO", iso_conn) {
			return false;
		}
		let udf_conn: u64 = service_connect(udf_client).unwrap_or(0);
		if !send_blocking(manager_side, b"STORAGE_UDF", udf_conn) {
			return false;
		}
		let usb_conn: u64 = service_connect(usb_client).unwrap_or(0);
		if !send_blocking(manager_side, b"STORAGE_USB", usb_conn) {
			return false;
		}
		// A fresh supervisor-status channel the manager grants to the governed `lssvc` command
		// (whose manifest grants services): the supervisor keeps the server end (in
		// `*stats_server2`) and serves the `supervisor` interface on it alongside
		// SystemGraphService's, while the client end is handed to the manager. A dedicated
		// channel, so a granted tool's queries never race the graph's.
		let (status_srv, status_cli): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, b"SERVICES", status_cli) {
			return false;
		}
		*stats_server2 = status_srv;
		// The xHCI driver's USB bus query channel the manager grants to the governed `lsusb`
		// command (whose manifest grants usb): handed up by DeviceManager in phase 2, held by
		// the supervisor until here (0 when the driver never came up - the manager simply
		// cannot grant what it does not hold).
		if !send_blocking(manager_side, b"USBBUS", usbq_client) {
			return false;
		}
		// A fresh ProcessService connection the manager drives to load the components it
		// governs - the loading mechanism, kept separate from the granting policy.
		let proc_conn: u64 = match service_connect(process_client) {
			Some(h) => h,
			None => return false,
		};
		if !send_blocking(manager_side, b"PROCESS", proc_conn) {
			return false;
		}
		// The channel its clients reach it on; the client end kept for the shell.
		bootstrap_serve(manager_side, perm_client)
	}
}

// Hand ResourceManager a read-only view of the init package (to launch the component it
// governs from) and the channel its clients reach it on ("SERVE", the client end kept in
// `*res_client` for the shell's `usage` command). The order matches ResourceManager's
// receive order: PACKAGE, SERVE. The manager holds no service clients - it governs its
// component's Domain through the kernel's resource syscalls (create the sub-Domain, set
// its limits, read its stats), not by granting service connections.
unsafe fn bootstrap_resource_manager(manager_side: u64, res_client: &mut u64, pkg_handle: u64, pkg_len: usize, buf: &mut [u8]) -> bool {
	unsafe {
		// The init package, so the manager can spawn the component it governs.
		if !bootstrap_package(manager_side, pkg_handle, pkg_len, buf) {
			return false;
		}
		// The channel its clients reach it on; the client end kept for the shell.
		bootstrap_serve(manager_side, res_client)
	}
}

// Hand a service a read-only view of the init package so it can spawn programs from
// it (DeviceManager spawns drivers, ProcessService launches programs): duplicate
// our package handle (read + map + transfer) and send "PACKAGE" + the byte length
// with the duplicate. We keep our own handle and mapping; the kernel allows the
// same object to be mapped in both address spaces.
unsafe fn bootstrap_package(manager_side: u64, pkg_handle: u64, pkg_len: usize, buf: &mut [u8]) -> bool {
	unsafe { bootstrap_package_rights(manager_side, pkg_handle, pkg_len, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER, buf) }
}

// The general form: hand a service the package under an explicit rights set. The
// launchers that still carry the package (DeviceManager, ProcessService as a bring-up
// fallback, ResourceManager) get read + map + transfer - enough to map it and pass it on.
unsafe fn bootstrap_package_rights(manager_side: u64, pkg_handle: u64, pkg_len: usize, rights: u32, buf: &mut [u8]) -> bool {
	unsafe {
		let dup: i64 = duplicate(pkg_handle, rights);
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

// Hand InputService the channel its clients reach it on ("SERVE", the client end kept
// in `*input_client` for the shell) and the raw pointer-event channel routed up from
// the virtio_input pointer driver via DeviceManager ("INPUT"; the handle is 0 when no
// pointer device is present, e.g. under test - InputService still serves an empty
// stream), then "FORWARD" transferring the input end of a fresh pointer-forward
// channel - InputService forwards every raw pointer event over it to ConsoleService,
// whose end is kept in `*pointer_console` for ConsoleService's own bootstrap (it starts
// later, since it declares input_service as a dependency). The order matches
// InputService's receive order: SERVE, INPUT, FORWARD.
unsafe fn bootstrap_input(manager_side: u64, input_raw: u64, input_client: &mut u64, pointer_console: &mut u64) -> bool {
	unsafe {
		if !bootstrap_serve(manager_side, input_client) {
			return false;
		}
		if !send_blocking(manager_side, b"INPUT", input_raw) {
			return false;
		}
		let (input_fwd, console_fwd): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, b"FORWARD", input_fwd) {
			return false;
		}
		*pointer_console = console_fwd;
		true
	}
}

// Hand ProcessService the StorageService client it loads the on-disk program binaries
// through ("STORAGE", a fresh factory connection), a read-only view of the init package
// (the bring-up fallback), and the channel its clients reach it on. The service-channel
// client end is kept in `*process_client` and later transferred to the shell for
// `ps`/`run`. The receive order matches ProcessService's: package, storage, serve.
unsafe fn bootstrap_process_service(manager_side: u64, pkg_handle: u64, pkg_len: usize, storage_client: u64, process_client: &mut u64, buf: &mut [u8]) -> bool {
	unsafe { bootstrap_package(manager_side, pkg_handle, pkg_len, buf) && send_factory(manager_side, b"STORAGE", storage_client) && bootstrap_serve(manager_side, process_client) }
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

// Bootstrap the media StorageService instance: hand it the second virtio-blk disk's
// block service ("FATBLOCK"), which it mounts as the FAT vol://media volume,
// then mint its service channel ("SERVE"); the client end is kept in `*media_client`
// and later handed to the shell. The block handle is 0 when no second disk is present,
// so the instance simply fails to mount and reports failed.
unsafe fn bootstrap_media_storage(manager_side: u64, block2_client: u64, media_client: &mut u64) -> bool {
	unsafe {
		if !send_blocking(manager_side, b"FATBLOCK", block2_client) {
			return false;
		}
		bootstrap_serve(manager_side, media_client)
	}
}

// Bootstrap the ISO StorageService instance: hand it the third virtio-blk disk's block
// service ("ISOBLOCK"), which it mounts as the read-only ISO9660 vol://iso volume, then
// mint its service channel ("SERVE"); the client end is kept in `*iso_client` and later
// handed to the shell. The block handle is 0 when no third disk is present, so the
// instance simply fails to mount and reports failed.
unsafe fn bootstrap_iso_storage(manager_side: u64, block3_client: u64, iso_client: &mut u64) -> bool {
	unsafe {
		if !send_blocking(manager_side, b"ISOBLOCK", block3_client) {
			return false;
		}
		bootstrap_serve(manager_side, iso_client)
	}
}

// Bootstrap the UDF StorageService instance: hand it the fourth virtio-blk disk's block
// service ("UDFBLOCK"), which it mounts as the read-only UDF vol://udf volume, then mint
// its service channel ("SERVE"); the client end is kept in `*udf_client` and later handed
// to the shell. The block handle is 0 when no fourth disk is present, so the instance
// simply fails to mount and reports failed.
unsafe fn bootstrap_udf_storage(manager_side: u64, block4_client: u64, udf_client: &mut u64) -> bool {
	unsafe {
		if !send_blocking(manager_side, b"UDFBLOCK", block4_client) {
			return false;
		}
		bootstrap_serve(manager_side, udf_client)
	}
}

// Bootstrap the USB StorageService instance: hand it the USB stick's block service
// ("USBBLOCK", served by the xhci driver over the Bulk-Only Transport and routed up in
// DeviceManager's phase 2), which it mounts as the writable FAT vol://usb volume, then
// mint its service channel ("SERVE"); the client end is kept in `*usb_client` and later
// handed to the shell. The block handle is 0 when the xhci driver never came up (no
// controller, or the driver failed) - the instance must still come up, so a dead-peer
// stand-in channel is handed over instead: the removable FAT backing mounts lazily and
// every probe of the dead channel fails like absent media, so vol://usb simply shows
// as unavailable rather than failing the boot chain (the shell depends on this
// instance).
unsafe fn bootstrap_usb_storage(manager_side: u64, block5_client: u64, usb_client: &mut u64) -> bool {
	unsafe {
		let block: u64 = if block5_client != 0 {
			block5_client
		} else {
			let (dead_server, dead_client): (u64, u64) = match channel() {
				Some(pair) => pair,
				None => return false,
			};
			close(dead_server);
			dead_client
		};
		if !send_blocking(manager_side, b"USBBLOCK", block) {
			return false;
		}
		bootstrap_serve(manager_side, usb_client)
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

// Hand AudioService the virtio-snd driver's control channel ("SND" - a 0 handle when
// no sound device is present, routed up from the snd driver via DeviceManager) and the
// channel its clients reach it on ("SERVE"). The service-channel client end is kept in
// `*audio_client` and later handed to the shell (and to ConsoleService as a factory)
// for the `beep` command. (AudioService depends on device_manager, so `snd_client` is
// already set by the time this runs; it is 0 when there is no sound device, and
// AudioService then answers `beep` with a not-found error.)
unsafe fn bootstrap_audio_service(manager_side: u64, snd_client: u64, audio_client: &mut u64) -> bool {
	unsafe {
		if !send_blocking(manager_side, b"SND", snd_client) {
			return false;
		}
		bootstrap_serve(manager_side, audio_client)
	}
}

// Hand ConsoleService the client end of a fresh console channel over "CLIENT" (VT 1's
// terminal: the shell writes its output to it and reads its keystrokes from it), then
// a *factory* connection to every multi-client service plus a read-only view of the
// init package. ConsoleService is the session spawner: when it opens an additional
// virtual terminal it mints a fresh per-VT client from each factory (`service_connect`
// / `network.open`) and spawns that VT's shell with the full capability set, so every
// VT runs a fully-capable shell over its own independent service connections. The
// factories are independent connections (not the supervisor's own clients or VT 1's),
// so minting from them never crosses the supervisor's lifecycle traffic. ConsoleService
// maps the framebuffer itself (the kernel console then stops drawing) and attaches to
// the kernel console input for keys.
unsafe fn bootstrap_console_service(manager_side: u64, storage_client: u64, log_client: u64, device_client: u64, process_client: u64, config_client: u64, net_client: u64, gpu_client: u64, time_client: u64, audio_client: u64, session_client: u64, perm_client: u64, pointer_console: u64, console_client: &mut u64, console_control: &mut u64) -> bool {
	unsafe {
		let (service_end, client_end): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, b"CLIENT", service_end) {
			return false;
		}
		*console_client = client_end;
		// VT 1's control channel: the console end goes to ConsoleService now (right after
		// CLIENT, the order its __user_main expects), the shell end is kept for the shell's
		// own bootstrap (it starts later in the boot order).
		let (control_console, control_shell): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, b"CONTROL", control_console) {
			return false;
		}
		*console_control = control_shell;
		// A factory connection per serve_multi service, minted with `service_connect`.
		if !send_factory(manager_side, b"FSTORAGE", storage_client) {
			return false;
		}
		if !send_factory(manager_side, b"FLOG", log_client) {
			return false;
		}
		if !send_factory(manager_side, b"FDEVICE", device_client) {
			return false;
		}
		if !send_factory(manager_side, b"FPROCESS", process_client) {
			return false;
		}
		if !send_factory(manager_side, b"FCONFIG", config_client) {
			return false;
		}
		if !send_factory(manager_side, b"FTIME", time_client) {
			return false;
		}
		if !send_factory(manager_side, b"FAUDIO", audio_client) {
			return false;
		}
		// The SessionService factory, so ConsoleService can mint a fresh per-VT session for
		// each additional virtual terminal it spawns. Sent right after FAUDIO to match the
		// console's receive order.
		if !send_factory(manager_side, b"FSESSION", session_client) {
			return false;
		}
		// The PermissionManager factory, so ConsoleService can mint a fresh per-VT launcher
		// client for each shell it spawns. Sent right after FSESSION to match the console's
		// receive order.
		if !send_factory(manager_side, b"FPERM", perm_client) {
			return false;
		}
		// NetworkService is multi-client through its own typed `open`, not serve_multi.
		let mut net = network::Client::new(ChannelTransport { chan: net_client });
		let net_fac: u64 = match net.open() {
			Some(Ok(h)) => h,
			_ => return false,
		};
		if !send_blocking(manager_side, b"FNET", net_fac) {
			return false;
		}
		// The gpu driver's display channel (0 when there is no virtio-gpu device, e.g.
		// under test - ConsoleService then falls back to the boot framebuffer).
		if !send_blocking(manager_side, b"GPU", gpu_client) {
			return false;
		}
		// The pointer-forward channel from InputService (0 when no pointer device this
		// boot): ConsoleService reads raw pointer events off it to drive selection,
		// scrollback, and SGR mouse reports.
		send_blocking(manager_side, b"POINTER", pointer_console)
	}
}

// Mint an independent factory connection to a serve_multi service and transfer it to
// ConsoleService under `tag`. The factory is a fresh client connection, so the
// session spawner can mint per-VT clients from it without racing other holders.
unsafe fn send_factory(manager_side: u64, tag: &[u8], root: u64) -> bool {
	unsafe {
		match service_connect(root) {
			Some(fac) => send_blocking(manager_side, tag, fac),
			None => false,
		}
	}
}

// Spawn the managed watchdog canary from the init package, keeping its control channel
// (the canary serves directly on its bootstrap channel, so the supervisor's end is both
// the bootstrap peer and the control channel). Returns (Process, control), or (0, 0) on
// failure. Waits for the canary's "online" report so the caller knows it is up before
// exercising the restart and watchdog paths against it. The canary is raw-spawned from
// the package (not loaded from the volume) because the supervisor exercises it after
// stopping DeviceManager - which drops virtio_blk, so the system volume is unavailable.
unsafe fn spawn_canary(package: &Package, buf: &mut [u8]) -> (u64, u64) {
	unsafe {
		let elf: &[u8] = match package.lookup(b"watchdog_probe") {
			Some(e) => e,
			None => return (0, 0),
		};
		let (ctrl, probe_side): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return (0, 0),
		};
		let proc: i64 = spawn(elf, probe_side);
		if proc < 0 {
			return (0, 0);
		}
		match recv_blocking(ctrl, buf) {
			Received::Message { .. } => (proc as u64, ctrl),
			Received::Closed => {
				close(proc as u64);
				(0, 0)
			}
		}
	}
}

// Restart the managed canary per the restart policy: record what took it down, drain
// and release its dead endpoints, then - unless the restart budget is spent - back off
// (longer after repeated failures) and respawn, charging one restart. Returns true if a
// replacement is running, false if the budget is exhausted (the caller escalates). The
// canary stands in for the policy a real service restart would follow.
unsafe fn restart_canary(package: &Package, proc: &mut u64, ctrl: &mut u64, sup: &mut Supervised, failure: Failure, park: u64, buf: &mut [u8]) -> bool {
	unsafe {
		sup.failure = failure;
		// Reap the old endpoints so the dead process is fully gone before its replacement.
		drain_closed(*ctrl, buf);
		if *ctrl != 0 {
			close(*ctrl);
			*ctrl = 0;
		}
		if *proc != 0 {
			close(*proc);
			*proc = 0;
		}
		// Spend from the restart budget; once exhausted, escalate rather than restart again.
		if sup.restarts >= MAX_RESTARTS {
			return false;
		}
		// Back off before the respawn, scaled by the attempt count. A bounded one-shot
		// sleep, so the test scheduler still advances it deterministically.
		sleep_ticks(park, RESTART_BACKOFF_TICKS * (sup.restarts as u64 + 1));
		let (new_proc, new_ctrl): (u64, u64) = spawn_canary(package, buf);
		if new_proc == 0 {
			return false;
		}
		*proc = new_proc;
		*ctrl = new_ctrl;
		sup.restarts += 1;
		true
	}
}

// Drain a channel until its peer is gone, discarding any queued messages. Used to wait
// out a dying process so its control channel is fully closed before it is replaced.
unsafe fn drain_closed(channel: u64, buf: &mut [u8]) {
	unsafe {
		if channel == 0 {
			return;
		}
		loop {
			match recv_blocking(channel, buf) {
				Received::Message { .. } => {}
				Received::Closed => return,
			}
		}
	}
}

// Sleep for `ticks` by waiting on the never-written `park` channel until the deadline
// passes. A bounded one-shot wait that sleeps the thread at ~0% CPU; the test scheduler
// advances the finite deadline, so the sleep is deterministic under test.
unsafe fn sleep_ticks(park: u64, ticks: u64) {
	unsafe {
		if park == 0 {
			return;
		}
		wait(park, clock() + ticks);
	}
}

// Stand as the supervisor after bring-up. Each iteration builds a wait set from every
// live control channel - the Running services' (kind 0), the canary's (kind 1), the
// shell's admin channel (kind 2), and SystemGraphService's stats channel (kind 3) - and
// blocks on all of them at once with no deadline, so the supervisor sleeps at ~0% CPU
// until one needs attention. A service or the canary peer-closing means it crashed (the
// canary is restarted per policy; a real service is recorded Failed and dropped from the
// wait set so its dead channel does not busy-loop); an admin message drives a reverse-
// dependency stop; a stats request is answered over the `supervisor` interface. Returns
// when nothing is left to watch.
unsafe fn supervise(state: &mut [State; N], channels: &mut [u64; N], sup: &mut [Supervised; N], procs: &[u64; N], package: &Package, canary_proc: &mut u64, canary_ctrl: &mut u64, canary_sup: &mut Supervised, admin_server: u64, admin_server2: u64, stats_server: u64, stats_server2: u64, drivers: &[(&'static [u8], bool)], log_client: u64, up: u64, park: u64, buf: &mut [u8]) {
	unsafe {
		let mut admin: u64 = admin_server;
		let mut admin2: u64 = admin_server2;
		let mut stats: u64 = stats_server;
		let mut stats2: u64 = stats_server2;
		loop {
			let mut handles: [u64; N + 5] = [0u64; N + 5];
			let mut kinds: [u8; N + 5] = [0u8; N + 5];
			let mut idxs: [usize; N + 5] = [0usize; N + 5];
			let mut count: usize = 0;
			let mut i: usize = 0;
			while i < N {
				if state[i] == State::Running && channels[i] != 0 {
					handles[count] = channels[i];
					kinds[count] = 0;
					idxs[count] = i;
					count += 1;
				}
				i += 1;
			}
			if *canary_ctrl != 0 {
				handles[count] = *canary_ctrl;
				kinds[count] = 1;
				count += 1;
			}
			if admin != 0 {
				handles[count] = admin;
				kinds[count] = 2;
				count += 1;
			}
			if stats != 0 {
				handles[count] = stats;
				kinds[count] = 3;
				count += 1;
			}
			if admin2 != 0 {
				handles[count] = admin2;
				kinds[count] = 4;
				count += 1;
			}
			if stats2 != 0 {
				handles[count] = stats2;
				kinds[count] = 5;
				count += 1;
			}
			if count == 0 {
				return;
			}
			let ready: i64 = wait_any(&handles[..count], 0);
			if ready < 0 {
				return;
			}
			let r: usize = ready as usize;
			match kinds[r] {
				0 => {
					// A real service's control channel fired; a peer-close means it crashed.
					let idx: usize = idxs[r];
					if let Polled::Closed = try_recv(channels[idx], buf) {
						state[idx] = State::Failed;
						sup[idx].failure = Failure::Crashed;
						channels[idx] = 0;
						emit_event(log_client, MANIFEST[idx].name, b"crashed");
						console_report(MANIFEST[idx].name, b"crashed");
					}
				}
				1 => {
					// The canary's control channel fired; a peer-close means it crashed, so
					// restart it per policy (escalating once the budget is spent).
					if let Polled::Closed = try_recv(*canary_ctrl, buf) {
						if restart_canary(package, canary_proc, canary_ctrl, canary_sup, Failure::Crashed, park, buf) {
							emit_event(log_client, b"watchdog_probe", b"restarted");
							console_report(b"watchdog_probe", b"restarted");
						} else {
							emit_event(log_client, b"watchdog_probe", b"escalated");
							console_report(b"watchdog_probe", b"escalated");
						}
					}
				}
				2 => {
					// The shell asked to stop a service; tear down its dependents first.
					if !handle_admin(admin, state, channels, sup, procs, log_client, up, buf) {
						admin = 0;
					}
				}
				3 => {
					// SystemGraphService queried the supervisor state; answer one request.
					if !serve_stats_once(stats, state, sup, canary_sup, drivers, buf) {
						stats = 0;
					}
				}
				4 => {
					// The sandboxed `stop` tool (granted the supervisor capability) asked to
					// stop a service over its own admin channel; tear down its dependents first.
					if !handle_admin(admin2, state, channels, sup, procs, log_client, up, buf) {
						admin2 = 0;
					}
				}
				_ => {
					// The sandboxed `lssvc` tool (granted the services capability) queried the
					// supervisor state over its own status channel; answer one request.
					if !serve_stats_once(stats2, state, sup, canary_sup, drivers, buf) {
						stats2 = 0;
					}
				}
			}
		}
	}
}

// Handle one admin request from the shell: the bare name of a service to stop. An
// unknown name (or one already down) gets a NOTFOUND reply; otherwise the service and
// its dependents are torn down and the newline-joined list of what stopped is replied
// for the shell to print. Returns false once the admin channel's peer (the shell) is
// gone, so the supervisor drops it from its wait set.
unsafe fn handle_admin(admin: u64, state: &mut [State; N], channels: &mut [u64; N], sup: &mut [Supervised; N], procs: &[u64; N], log_client: u64, up: u64, buf: &mut [u8]) -> bool {
	unsafe {
		let len: usize = match recv_blocking(admin, buf) {
			Received::Message { len, .. } => len,
			Received::Closed => return false,
		};
		// Copy the name out of `buf`, since the teardown reuses it to drain control channels.
		let mut namebuf: [u8; 64] = [0u8; 64];
		let nlen: usize = len.min(namebuf.len()).min(buf.len());
		namebuf[..nlen].copy_from_slice(&buf[..nlen]);
		let name: &[u8] = &namebuf[..nlen];
		match index_of(name) {
			Some(target) if state[target] == State::Running => {
				let stopped: Vec<u8> = stop_subtree(target, state, channels, sup, procs, log_client, up, buf);
				let mut reply: Vec<u8> = Vec::new();
				reply.extend_from_slice(b"STOPPED\n");
				reply.extend_from_slice(&stopped);
				send_blocking(admin, &reply, 0);
			}
			_ => {
				send_blocking(admin, b"NOTFOUND", 0);
			}
		}
		true
	}
}

// Tear down a service and every component that transitively depends on it, dependents
// first. The scope is the target plus its reverse-dependency closure, minus the shell
// (the controlling terminal is exempt: force-killing the issuing client is hostile, and
// its Process handle is not held here anyway). Components are stopped in reverse-
// topological order - repeatedly a current leaf of the scoped subgraph, one nothing in
// scope still depends on - so a dependent always stops before its dependency. A service
// is stopped by killing its process (services serve on their SERVE channels and ignore
// their bootstrap, so the cooperative STOP protocol does not reach them) and draining
// its control channel to the peer-close. Returns the newline-joined names of everything
// stopped, in teardown order.
unsafe fn stop_subtree(target: usize, state: &mut [State; N], channels: &mut [u64; N], sup: &mut [Supervised; N], procs: &[u64; N], log_client: u64, up: u64, buf: &mut [u8]) -> Vec<u8> {
	unsafe {
		let mut scope: [bool; N] = [false; N];
		scope[target] = true;
		// Fixpoint: add any Running component that depends on something already in scope,
		// until nothing new is added - the full reverse-dependency closure of the target.
		loop {
			let mut grew: bool = false;
			let mut i: usize = 0;
			while i < N {
				if !scope[i] && state[i] == State::Running && depends_on_scoped(i, &scope) {
					scope[i] = true;
					grew = true;
				}
				i += 1;
			}
			if !grew {
				break;
			}
		}
		// Exempt the shell: it transitively depends on everything, so it would always be in
		// scope, but the issuing terminal must survive (and procs[shell] == 0 regardless).
		if let Some(sh) = index_of(b"shell") {
			scope[sh] = false;
		}
		let mut stopped: Vec<u8> = Vec::new();
		loop {
			let mut progress: bool = false;
			let mut i: usize = 0;
			while i < N {
				if scope[i] && state[i] == State::Running && !has_running_dependent(i, &scope, state) {
					if procs[i] != 0 {
						signal(procs[i], SIG_KILL);
					}
					drain_closed(channels[i], buf);
					if channels[i] != 0 {
						close(channels[i]);
						channels[i] = 0;
					}
					state[i] = State::Stopped;
					sup[i].failure = Failure::Stopped;
					emit_event(log_client, MANIFEST[i].name, b"stopped");
					console_report(MANIFEST[i].name, b"stopped");
					if !stopped.is_empty() {
						stopped.push(b'\n');
					}
					stopped.extend_from_slice(MANIFEST[i].name);
					progress = true;
				}
				i += 1;
			}
			if !progress {
				break;
			}
		}
		let _ = up;
		stopped
	}
}

// Whether component `i` depends on any component currently in the teardown scope.
fn depends_on_scoped(i: usize, scope: &[bool; N]) -> bool {
	for &dep in MANIFEST[i].deps {
		if let Some(d) = index_of(dep) {
			if scope[d] {
				return true;
			}
		}
	}
	false
}

// Whether any in-scope Running component still depends on component `i` - i.e. `i` is
// not yet a leaf of the scoped subgraph and must not be stopped this round.
fn has_running_dependent(i: usize, scope: &[bool; N], state: &[State; N]) -> bool {
	let mut j: usize = 0;
	while j < N {
		if j != i && scope[j] && state[j] == State::Running && index_of_dep(j, i) {
			return true;
		}
		j += 1;
	}
	false
}

// Whether component `j` declares component `i` among its dependencies.
fn index_of_dep(j: usize, i: usize) -> bool {
	for &dep in MANIFEST[j].deps {
		if index_of(dep) == Some(i) {
			return true;
		}
	}
	false
}

// Answer one request on a supervisor stats channel (SystemGraphService's, or the
// sandboxed `lssvc` tool's): decode it, build the per-component status list (state,
// restarts, watchdog trips, last failure for each manifest service, the canary, and
// each driver), and reply over the `supervisor` interface. Returns false once the
// channel's peer is gone, so the supervisor drops it from its wait set.
unsafe fn serve_stats_once(stats: u64, state: &[State; N], sup: &[Supervised; N], canary_sup: &Supervised, drivers: &[(&'static [u8], bool)], buf: &mut [u8]) -> bool {
	unsafe {
		let (len, handle): (usize, u64) = match recv_blocking(stats, buf) {
			Received::Message { len, handle } => (len, handle),
			Received::Closed => return false,
		};
		let mut api = StatsApi { state, sup, canary_sup, drivers };
		let mut reply: [u8; 4096] = [0u8; 4096];
		let mut reply_handle: u64 = 0;
		if let Some(n) = supervisor::dispatch(&mut api, &buf[..len], handle, &mut reply, &mut reply_handle) {
			send_blocking(stats, &reply[..n], reply_handle);
		}
		true
	}
}

// The name of a service's lifecycle state, as the status view reports it.
fn state_name(state: State) -> &'static str {
	match state {
		State::Pending => "pending",
		State::Running => "running",
		State::Stopped => "stopped",
		State::Failed => "failed",
	}
}

// The supervisor's view of its own bookkeeping, served over the `supervisor` interface.
struct StatsApi<'a> {
	state: &'a [State; N],
	sup: &'a [Supervised; N],
	canary_sup: &'a Supervised,
	drivers: &'a [(&'static [u8], bool)],
}

impl<'a> supervisor::Service for StatsApi<'a> {
	fn status(&mut self) -> Result<Vec<SupervisorStat>, Error> {
		let mut out: Vec<SupervisorStat> = Vec::new();
		let mut i: usize = 0;
		while i < N {
			out.push(SupervisorStat { name: String::from_utf8_lossy(MANIFEST[i].name).into_owned(), state: String::from(state_name(self.state[i])), restarts: self.sup[i].restarts, watchdog_trips: self.sup[i].watchdog_trips, last_failure: String::from_utf8_lossy(self.sup[i].failure.as_bytes()).into_owned() });
			i += 1;
		}
		out.push(SupervisorStat { name: String::from("watchdog_probe"), state: String::from("running"), restarts: self.canary_sup.restarts, watchdog_trips: self.canary_sup.watchdog_trips, last_failure: String::from_utf8_lossy(self.canary_sup.failure.as_bytes()).into_owned() });
		// Drivers are services too: DeviceManager launches them and each hands its channel
		// over when it reports in, so a driver whose channel arrived is running and one
		// that never reported is pending (absent hardware, or not launched this boot).
		for &(name, online) in self.drivers {
			out.push(SupervisorStat { name: String::from_utf8_lossy(name).into_owned(), state: String::from(if online { "running" } else { "pending" }), restarts: 0, watchdog_trips: 0, last_failure: String::new() });
		}
		Ok(out)
	}
}
