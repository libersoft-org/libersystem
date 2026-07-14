use super::*;

// Load a non-pinned service from the system volume's `bin/` through ProcessService,
// handing the new process `bootstrap` as its bootstrap channel. Mints a dedicated
// launcher connection to the `process` factory (so the client end kept for the shell
// stays pristine). Returns the new process handle, or a negative value on failure.
pub(super) unsafe fn launch_from_volume(process_client: u64, name: &[u8], bootstrap: u64) -> i64 {
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

// Drive DeviceManager's phase 2: now that the system volume is mounted, hand
// it a fresh StorageService connection over its control channel with a "DRIVERS" message,
// so it loads the non-bootstrap drivers from vol://system/drivers/ and hands their channels
// back - the net driver's frame channel, the gpu display channel, the snd control channel,
// the pointer event channel, the USB stick's block channel (each 0 when that device is
// absent), the xHCI driver's USB bus query channel (the `lsusb` inventory) and its
// pointer-event channel (a USB pointing device). Kept for bootstrapping NetworkService,
// ConsoleService, AudioService, InputService, the usb StorageService instance and
// PermissionManager's `usb` grant against the drivers.
pub(super) unsafe fn drive_runtime_drivers(dm_control: u64, storage_client: u64, net_frames: &mut u64, gpu_client: &mut u64, snd_client: &mut u64, input_raw: &mut u64, block5_client: &mut u64, usbq_client: &mut u64, usb_pointer: &mut u64, raw_keys: &mut u64, buf: &mut [u8]) {
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
		if let Received::Message { handle: ptr, .. } = recv_blocking(dm_control, buf) {
			*usb_pointer = ptr;
		}
		if let Received::Message { handle: keys, .. } = recv_blocking(dm_control, buf) {
			*raw_keys = keys;
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
pub(super) unsafe fn start_service(package: &Package, name: &[u8], up: u64, pkg_handle: u64, pkg_len: usize, block_client: &mut u64, block2_client: &mut u64, block3_client: &mut u64, block4_client: &mut u64, block5_client: &mut u64, media_client: &mut u64, iso_client: &mut u64, udf_client: &mut u64, usb_client: &mut u64, usbq_client: &mut u64, net_frames: &mut u64, net_client: &mut u64, gpu_client: &mut u64, display_client: &mut u64, snd_client: &mut u64, audio_client: &mut u64, time_client: &mut u64, console_client: &mut u64, console_control: &mut u64, storage_client: &mut u64, log_client: &mut u64, device_client: &mut u64, process_client: &mut u64, config_client: &mut u64, input_raw: &mut u64, usb_pointer: &mut u64, raw_keys: &mut u64, input_client: &mut u64, input_focus: &mut u64, input_kill: &mut u64, pointer_console: &mut u64, graph_client: &mut u64, perm_client: &mut u64, res_client: &mut u64, session_client: &mut u64, session1: &mut u64, admin_server: &mut u64, admin_server2: &mut u64, stats_server: &mut u64, stats_server2: &mut u64, procs: &[u64; N], state: &[State; N], proc_out: &mut u64, control: &mut u64, failure_out: &mut String, buf: &mut [u8]) -> State {
	unsafe {
		let (manager_side, service_side): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return State::Failed,
		};
		// The pinned bootstrap set is raw-spawned from the init package (it is on the path
		// to mounting the system volume, so it cannot load from it); every other service is
		// loaded from the volume's `bin/` through ProcessService. media / iso /
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
		if name == b"config_service" && !bootstrap_config_service(manager_side, *storage_client, config_client) {
			return State::Failed;
		}
		if name == b"network_service" && !bootstrap_network_service(manager_side, *net_frames, *config_client, net_client) {
			return State::Failed;
		}
		if name == b"time_service" && !bootstrap_time_service(manager_side, *net_client, time_client) {
			return State::Failed;
		}
		if name == b"audio_service" && !bootstrap_audio_service(manager_side, *snd_client, audio_client) {
			return State::Failed;
		}
		if name == b"input_service" && !bootstrap_input(manager_side, *input_raw, *usb_pointer, *raw_keys, input_client, input_focus, input_kill, pointer_console) {
			return State::Failed;
		}
		if name == b"display_service" && !bootstrap_display_service(manager_side, *gpu_client, *input_focus, *input_kill, display_client) {
			return State::Failed;
		}
		if name == b"console_service" && !bootstrap_console_service(manager_side, *storage_client, *log_client, *device_client, *process_client, *config_client, *net_client, *display_client, *time_client, *audio_client, *session_client, *perm_client, *pointer_console, console_client, console_control) {
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
				// A service that could not complete a bootstrap step reports the failing step
				// and the reason (BOOTSTRAP_FAILURE) in place of its "online" report: record it
				// so the supervisor status and the journal explain the failure, instead of the
				// supervisor seeing an unexplained peer-close.
				if len >= BOOTSTRAP_FAILURE.len() && &buf[..BOOTSTRAP_FAILURE.len()] == BOOTSTRAP_FAILURE {
					let start: usize = (BOOTSTRAP_FAILURE.len() + 1).min(len);
					*failure_out = String::from_utf8_lossy(&buf[start..len]).into_owned();
					emit_event(*log_client, name, failure_out.as_bytes());
					return State::Failed;
				}
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
			Received::Closed => {
				// The service closed its bootstrap channel without reporting - it crashed during
				// bring-up before it could send a failure report. Record that so the status view
				// still carries a reason rather than a bare "failed".
				*failure_out = String::from("bootstrap channel closed without a report");
				State::Failed
			}
		}
	}
}

// Emit one structured Entry to LogService over the `log_client` channel: an Info
// record tagged with the service `source` and an `event` field (e.g.
// "online"/"stopped"). A no-op until LogService is up (log_client == 0). The
// supervisor logs service lifecycle the way systemd journals unit start/stop.
pub(super) unsafe fn emit_event(log_client: u64, source: &[u8], event: &[u8]) {
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
pub(super) unsafe fn console_report(source: &[u8], event: &[u8]) {
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
pub(super) unsafe fn stop_service(control: u64, up: u64, buf: &mut [u8]) -> State {
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

// Grant the shell a capability by DUPLICATING the supervisor's client and transferring
// the copy, so the supervisor keeps the original (the serve root's client end) alive for
// the life of the system - the shell exiting then closes only its copy and the service
// survives, so a logout reloads a fresh shell instead of tearing the system down. An
// absent capability (client 0) is sent as a bare tag with no handle (the shell reads it
// as "not granted"). Returns false only if duplicating a real client fails.
unsafe fn send_shell_cap(manager_side: u64, tag: &[u8], client: u64) -> bool {
	unsafe {
		if client == 0 {
			return send_blocking(manager_side, tag, 0);
		}
		let dup: i64 = duplicate(client, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		if dup < 0 {
			return false;
		}
		send_blocking(manager_side, tag, dup as u64)
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
		// Every service client the shell is handed is a DUPLICATE (see send_shell_cap): the
		// supervisor keeps every serve root's client end alive for the life of the system, so
		// a shell exit / logout closes only its copies and reloads a fresh shell rather than
		// tearing the running system down. The volume clients that are absent this boot
		// (media / iso / udf / usb with no disk) arrive as 0 and are sent as a bare tag.
		if !send_shell_cap(manager_side, CAP_STORAGE, storage_client) {
			return false;
		}
		if !send_shell_cap(manager_side, CAP_MEDIA, media_client) {
			return false;
		}
		if !send_shell_cap(manager_side, CAP_ISO, iso_client) {
			return false;
		}
		if !send_shell_cap(manager_side, CAP_UDF, udf_client) {
			return false;
		}
		if !send_shell_cap(manager_side, CAP_USB, usb_client) {
			return false;
		}
		let log_dup: i64 = duplicate(log_client, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		if log_dup < 0 {
			return false;
		}
		if !send_blocking(manager_side, CAP_LOG, log_dup as u64) {
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
		if !send_blocking(manager_side, CAP_DEVICE, device_dup as u64) {
			return false;
		}
		if !send_shell_cap(manager_side, CAP_PROCESS, process_client) {
			return false;
		}
		let config_dup: i64 = duplicate(config_client, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		if config_dup < 0 {
			return false;
		}
		if !send_blocking(manager_side, CAP_CONFIG, config_dup as u64) {
			return false;
		}
		if !send_shell_cap(manager_side, CAP_NET, net_client) {
			return false;
		}
		let time_dup: i64 = duplicate(time_client, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		if time_dup < 0 {
			return false;
		}
		if !send_blocking(manager_side, CAP_TIME, time_dup as u64) {
			return false;
		}
		let audio_dup: i64 = duplicate(audio_client, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		if audio_dup < 0 {
			return false;
		}
		if !send_blocking(manager_side, CAP_AUDIO, audio_dup as u64) {
			return false;
		}
		if !send_shell_cap(manager_side, CAP_INPUT, input_client) {
			return false;
		}
		// The SystemGraphService client, so the shell's `graph` command can render the live
		// system graph.
		if !send_shell_cap(manager_side, CAP_GRAPH, graph_client) {
			return false;
		}
		// The PermissionManager client, so the shell's `perm` command can render the
		// permission audit trail.
		if !send_shell_cap(manager_side, CAP_PERM, perm_client) {
			return false;
		}
		// The ResourceManager client, so the shell's `usage` command can render the live
		// per-Domain budgets - a
		// duplicate, like the other launcher-dropped clients above, so the supervisor keeps
		// the serve root.
		let res_dup: i64 = duplicate(res_client, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		if res_dup < 0 {
			return false;
		}
		if !send_blocking(manager_side, CAP_RESOURCE, res_dup as u64) {
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
		if !send_blocking(manager_side, CAP_SESSION, session_dup as u64) {
			return false;
		}
		if !send_blocking(manager_side, CAP_CONSOLE, console_client) {
			return false;
		}
		// VT 1's control channel to ConsoleService (the shell end; the console holds the
		// other end). Carries SET_FG / CLEAR_FG out and JOB_STOPPED back for job-control
		// signals.
		if !send_blocking(manager_side, CAP_CONTROL, console_control) {
			return false;
		}
		// A fresh ADMIN channel the shell drives `stop <service>` over: the supervisor
		// keeps the server end (in `*admin_server`) and stands on it in the supervise
		// loop; the client end is transferred to the shell, which receives it last.
		let (admin_srv, admin_cli): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, CAP_ADMIN, admin_cli) {
			return false;
		}
		*admin_server = admin_srv;
		send_ready(manager_side)
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
pub(super) unsafe fn bootstrap_serve(manager_side: u64, client: &mut u64) -> bool {
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

// Hand ConfigService its persistence backing - a fresh system-volume connection
// ("STORAGE", minted from the storage root; ConfigService depends on
// process_service and thus storage_service, so the volume is mounted by now) -
// then the channel its clients reach it on. The tree then loads from and
// write-through-persists to `vol://system/config.tree`, so a `config set`
// survives a restart and a reboot.
unsafe fn bootstrap_config_service(manager_side: u64, storage_client: u64, config_client: &mut u64) -> bool {
	unsafe {
		if storage_client != 0 && !send_factory(manager_side, b"STORAGE", storage_client) {
			return false;
		}
		bootstrap_serve(manager_side, config_client)
	}
}

// Hand InputService the channel its clients reach it on ("SERVE", the client end kept
// in `*input_client` for the shell) and the raw pointer-event channels routed up from
// the virtio_input pointer driver and the xhci driver via DeviceManager ("INPUT" and
// "INPUT2"; a handle is 0 when that pointer source is absent, e.g. under test -
// InputService still serves an empty stream), then "FORWARD" transferring the input
// end of a fresh pointer-forward channel - InputService forwards every raw pointer
// event over it to ConsoleService, whose end is kept in `*pointer_console` for
// ConsoleService's own bootstrap (it starts later, since it declares input_service as
// a dependency). "KEYS" carries the merged keyboard-driver consumer, while private
// "FOCUS" and "KILL" pairs connect InputService to DisplayService. The order matches
// InputService's receive order: SERVE, INPUT, INPUT2, FORWARD, KEYS, FOCUS, KILL.
unsafe fn bootstrap_input(manager_side: u64, input_raw: u64, usb_pointer: u64, raw_keys: u64, input_client: &mut u64, input_focus: &mut u64, input_kill: &mut u64, pointer_console: &mut u64) -> bool {
	unsafe {
		if !bootstrap_serve(manager_side, input_client) {
			return false;
		}
		if !send_blocking(manager_side, b"INPUT", input_raw) {
			return false;
		}
		if !send_blocking(manager_side, b"INPUT2", usb_pointer) {
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
		if !send_blocking(manager_side, b"KEYS", raw_keys) {
			return false;
		}
		let (input_end, display_end): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, b"FOCUS", input_end) {
			return false;
		}
		*input_focus = display_end;
		let (input_end, display_end): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, b"KILL", input_end) {
			return false;
		}
		*input_kill = display_end;
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
unsafe fn bootstrap_network_service(manager_side: u64, net_frames: u64, config_client: u64, net_client: &mut u64) -> bool {
	unsafe {
		if !send_blocking(manager_side, b"FRAMES", net_frames) {
			return false;
		}
		// The config tree's client (the `net.arp-cache` policy), minted fresh - the
		// service depends on config_service, so the tree serves by now. Handle 0
		// tells the service to fall back to its compiled-in default.
		let cfg: u64 = match service_connect(config_client) {
			Some(c) => c,
			None => 0,
		};
		if !send_blocking(manager_side, b"CONFIG", cfg) {
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

// Hand DisplayService the raw virtio-gpu channel and create the typed multi-client
// display root. With no gpu, the service maps the boot framebuffer instead.
unsafe fn bootstrap_display_service(manager_side: u64, gpu_client: u64, input_focus: u64, input_kill: u64, display_client: &mut u64) -> bool {
	unsafe {
		if !send_blocking(manager_side, b"GPU", gpu_client) {
			return false;
		}
		if !send_blocking(manager_side, b"FOCUS", input_focus) {
			return false;
		}
		if !send_blocking(manager_side, b"KILL", input_kill) {
			return false;
		}
		bootstrap_serve(manager_side, display_client)
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
unsafe fn bootstrap_console_service(manager_side: u64, storage_client: u64, log_client: u64, device_client: u64, process_client: u64, config_client: u64, net_client: u64, display_client: u64, time_client: u64, audio_client: u64, session_client: u64, perm_client: u64, pointer_console: u64, console_client: &mut u64, console_control: &mut u64) -> bool {
	unsafe {
		let (service_end, client_end): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, CAP_CLIENT, service_end) {
			return false;
		}
		*console_client = client_end;
		// VT 1's control channel: the console end goes to ConsoleService now, the shell end
		// is kept for the shell's own bootstrap (it starts later in the boot order).
		let (control_console, control_shell): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return false,
		};
		if !send_blocking(manager_side, CAP_CONTROL, control_console) {
			return false;
		}
		*console_control = control_shell;
		// A factory connection per serve_multi service, minted with `service_connect`.
		if !send_factory(manager_side, CAP_FSTORAGE, storage_client) {
			return false;
		}
		if !send_factory(manager_side, CAP_FLOG, log_client) {
			return false;
		}
		if !send_factory(manager_side, CAP_FDEVICE, device_client) {
			return false;
		}
		if !send_factory(manager_side, CAP_FPROCESS, process_client) {
			return false;
		}
		if !send_factory(manager_side, CAP_FCONFIG, config_client) {
			return false;
		}
		if !send_factory(manager_side, CAP_FTIME, time_client) {
			return false;
		}
		if !send_factory(manager_side, CAP_FAUDIO, audio_client) {
			return false;
		}
		// The SessionService factory, so ConsoleService can mint a fresh per-VT session for
		// each additional virtual terminal it spawns.
		if !send_factory(manager_side, CAP_FSESSION, session_client) {
			return false;
		}
		// The PermissionManager factory, so ConsoleService can mint a fresh per-VT launcher
		// client for each shell it spawns.
		if !send_factory(manager_side, CAP_FPERM, perm_client) {
			return false;
		}
		// NetworkService is multi-client through its own typed `open`, not serve_multi.
		let mut net = network::Client::new(ChannelTransport { chan: net_client });
		let net_fac: u64 = match net.open() {
			Some(Ok(h)) => h,
			_ => return false,
		};
		if !send_blocking(manager_side, CAP_FNET, net_fac) {
			return false;
		}
		// An independent typed DisplayService connection. The service owns either the
		// virtio-gpu backing or the boot framebuffer; ConsoleService only sees a surface.
		let display: u64 = match service_connect(display_client) {
			Some(channel) => channel,
			None => return false,
		};
		if !send_blocking(manager_side, CAP_DISPLAY, display) {
			return false;
		}
		// The pointer-forward channel from InputService (0 when no pointer device this
		// boot): ConsoleService reads raw pointer events off it to drive selection,
		// scrollback, and SGR mouse reports.
		if !send_blocking(manager_side, CAP_POINTER, pointer_console) {
			return false;
		}
		send_ready(manager_side)
	}
}

// Mint an independent factory connection to a serve_multi service and transfer it to
// ConsoleService under `tag`. The factory is a fresh client connection, so the
// session spawner can mint per-VT clients from it without racing other holders.
pub(super) unsafe fn send_factory(manager_side: u64, tag: &[u8], root: u64) -> bool {
	unsafe {
		match service_connect(root) {
			Some(fac) => send_blocking(manager_side, tag, fac),
			None => false,
		}
	}
}
