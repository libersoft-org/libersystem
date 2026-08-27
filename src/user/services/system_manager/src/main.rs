// SystemManager - the first userspace process.
//
// The kernel loads this program from the init package into a fresh Process and
// drops it into ring 3 at `_start` (provided by the shared `rt` runtime) with a
// bootstrap channel handle in rdi. Over that channel the kernel hands it the init
// package as a read-only shared buffer. SystemManager maps the package, spawns
// ServiceManager from it (the next link of the boot chain), relays ServiceManager's
// report up to the kernel, reports in itself, and exits. Later work grows it
// into a standing process that supervises ServiceManager and performs recovery.

#![no_std]
#![no_main]

// The ring-3 entry stub, syscall wrapper, panic handler, spawn/IPC helpers, and
// ABI constants all come from the shared userspace runtime crate.
use proto::system::{Error, system_power};
use rt::*;

// `rt`'s `_start` enters here with the bootstrap channel handle in rdi.
#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. receive the init package shared buffer and map it.
	let (pkg_handle, pkg_base, pkg_len): (u64, u64, usize) = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 + 8 && &buf[..7] == b"PACKAGE" => {
			let length: usize = u64::from_le_bytes([buf[7], buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14]]) as usize;
			let base: u64 = unsafe { syscall(SYS_MEMORY_MAP, handle, 0, 0, 0) };
			if sys_is_err(base) {
				exit();
			}
			(handle, base, length)
		}
		_ => exit(),
	};

	// 1b. receive the ramdisk volume buffer to delegate to StorageService. We never
	//     map it ourselves - just hold the capability and its length to pass down.
	//
	//     TWO tags arrive here, and this process does not care which: `RAMDISK` is an archive to
	//     unpack, `LIVEVOL` a whole LiberFS image to mount, and only StorageService acts on the
	//     difference. What matters is that the tag is RELAYED rather than rewritten - accepting
	//     both and forwarding one would tell ServiceManager the wrong thing about a live medium.
	//
	//     Accepting only `RAMDISK` here is what kept the LiveCD from booting: the tag did not
	//     match, the arm below exited, and the boot chain stopped before its first service with
	//     nothing said. Hence the report on the way out - a bootstrap that cannot proceed should
	//     name what it received, not vanish.
	let (volume_tag, ramdisk_handle, ramdisk_len): ([u8; 7], u64, usize) = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 + 8 && (&buf[..7] == b"RAMDISK" || &buf[..7] == b"LIVEVOL") => {
			let length: usize = u64::from_le_bytes([buf[7], buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14]]) as usize;
			let mut tag: [u8; 7] = [0u8; 7];
			tag.copy_from_slice(&buf[..7]);
			(tag, handle, length)
		}
		_ => {
			unsafe { print(b"SystemManager: expected RAMDISK or LIVEVOL in the bootstrap; boot chain stops here\n") };
			exit()
		}
	};

	// 1b2. receive the power capability - a root-Domain handle carrying MANAGE, which is what
	//     `SYS_SYSTEM_POWER` checks.
	//
	//     IT STOPS HERE. It used to be passed down to ServiceManager, on to DeviceManager, and on
	//     again to the two keyboard drivers - four more holders of a capability the kernel's own
	//     comment describes as being able to `sys_domain_kill` the whole system, all so that
	//     Ctrl+Alt+Del would work. What goes down the chain now is a client of the SystemPower
	//     service below, which can stop the machine and can do nothing else.
	let power: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle, .. } if len == 5 && &buf[..5] == b"POWER" && handle != 0 => handle,
		_ => exit(),
	};

	// 1c. receive the boot mode flag ("MODE" + one byte, 1 = test boot) to relay down
	//     to ServiceManager, which gates its bring-up self-tests on it.
	let mode: u8 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, .. } if len == 5 && &buf[..4] == b"MODE" => buf[4],
		_ => exit(),
	};

	// 1d. receive the three console/display capabilities in one message, in the kernel's order:
	//     DisplayController, ConsoleInputSource, ConsoleSink. Like the power capability, this
	//     process holds them only to pass them on.
	//
	//     LAST in the sequence, and every hop below adds its forward at the end of its own
	//     sequence too. The bootstrap is read positionally - `recv_tagged` checks the tag of the
	//     NEXT message rather than searching for it - so anything inserted in the middle shifts
	//     every read after it and stops the boot chain where it stands.
	let mut console_caps: [u64; MAX_MESSAGE_CAPS] = [0; MAX_MESSAGE_CAPS];
	let console_cap_count: usize = match unsafe { recv_message_caps(bootstrap, &mut buf, &mut console_caps) } {
		(len, count) if len >= 11 && &buf[..11] == b"CONSOLECAPS" => count,
		_ => 0,
	};

	// 1e. the boot window, in monotonic ticks: the absolute deadline this attempt's window closes
	//     at, and the length it was computed from. Relayed down like the mode flag - this process
	//     bounds nothing itself, but DeviceManager cannot invent a bind budget and ServiceManager
	//     is what keeps the length across a DeviceManager restart.
	//
	//     ZERO WHEN ABSENT, AND THAT IS NOT FATAL. Every hop treats a zero window as "no budget was
	//     published" and falls back to its own default. A boot that stops because a supervisor
	//     could not be told how long it had would be a worse failure than one that runs on the
	//     compiled-in number.
	let (boot_deadline, boot_window): (u64, u64) = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, .. } if len >= 7 + 16 && &buf[..7] == b"BOOTWIN" => (u64::from_le_bytes(buf[7..15].try_into().unwrap_or([0; 8])), u64::from_le_bytes(buf[15..23].try_into().unwrap_or([0; 8]))),
		_ => (0, 0),
	};

	// 1f. which volume the loader chose as this boot's system volume. Relayed down to whoever
	//     mounts one - a format cannot answer it, because two LiberFS volumes differ only by uuid
	//     and two FAT volumes not even by that.
	let mut root_selection: [u8; 24] = [0u8; 24];
	if let Received::Message { len, .. } = unsafe { recv_blocking(bootstrap, &mut buf) }
		&& len >= 7 + 24
		&& &buf[..7] == b"ROOTSEL"
	{
		root_selection.copy_from_slice(&buf[7..31]);
	}

	// 2. find ServiceManager in the package and spawn it, handing it one end of a
	//    fresh control channel as its bootstrap.
	let archive: &[u8] = unsafe { core::slice::from_raw_parts(pkg_base as *const u8, pkg_len) };
	let svc_elf: &[u8] = match Package::parse(archive).and_then(|p| p.lookup(b"service_manager.lsexe")) {
		Some(elf) => elf,
		None => exit(),
	};
	let (sm_side, svc_side): (u64, u64) = match unsafe { channel() } {
		Some(pair) => pair,
		None => exit(),
	};
	// The SystemPower pair: this process serves on one end and the other travels down the boot
	// chain in place of the root-Domain handle.
	let (power_server, power_client): (u64, u64) = match unsafe { channel() } {
		Some(pair) => pair,
		None => exit(),
	};
	// ONE CHILD DOMAIN FOR THE WHOLE CONTROL PLANE, and this process owns it.
	//
	// ServiceManager and every service it starts live inside it, because `SYS_PROCESS_CREATE` puts
	// a new process in the CALLER'S domain when it is given handle 0 - which is what ServiceManager
	// and ProcessService already pass. So the branch forms itself: nothing downstream has to be
	// told which Domain it belongs to, and nothing downstream holds a Domain handle at all.
	//
	// THAT IS THE POINT. A ServiceManager holding `MANAGE` on its own subtree could
	// `sys_domain_kill` the branch it lives in; it does not need to, and it now cannot. Tearing the
	// branch down is this process's, because this process is the one that outlives it.
	//
	// NOT resource-bounded here. Limits are ResourceManager's subject and this milestone says so;
	// a number invented at this line would be a policy nobody declared.
	let branch_domain: i64 = unsafe { domain_create(u64::MAX, u64::MAX, u64::MAX) };
	if branch_domain < 0 {
		unsafe { print(b"SystemManager: cannot create the control-plane Domain; boot chain stops here\n") };
		exit();
	}
	if unsafe { spawn_in(svc_elf, svc_side, branch_domain as u64) } < 0 {
		exit();
	}

	// 3. hand the package and the ramdisk down to ServiceManager so it can spawn the
	//    services it supervises. Unmap the package first (a MemoryObject allows only
	//    one active mapping, so ServiceManager could not map it otherwise), then
	//    transfer both capabilities with the same framing the kernel used.
	unsafe {
		syscall(SYS_MEMORY_UNMAP, pkg_handle, 0, 0, 0);
		let mut pkg_msg: [u8; 7 + 8] = [0u8; 7 + 8];
		pkg_msg[..7].copy_from_slice(b"PACKAGE");
		pkg_msg[7..].copy_from_slice(&(pkg_len as u64).to_le_bytes());
		send_blocking(sm_side, &pkg_msg, pkg_handle);
		let mut rd_msg: [u8; 7 + 8] = [0u8; 7 + 8];
		rd_msg[..7].copy_from_slice(&volume_tag);
		rd_msg[7..].copy_from_slice(&(ramdisk_len as u64).to_le_bytes());
		send_blocking(sm_side, &rd_msg, ramdisk_handle);
		// The narrow door, in the position the root-Domain handle used to travel in. One end is
		// kept here and served for the life of the system; the other goes down the chain to
		// whoever owns the Power key.
		send_blocking(sm_side, b"SYSPOWER", power_client);
		let mode_msg: [u8; 5] = [b'M', b'O', b'D', b'E', mode];
		send_blocking(sm_side, &mode_msg, 0);
		// The three console/display capabilities, in the order they arrived.
		//
		// SENT WHETHER OR NOT THERE ARE ANY. It used to be skipped when the count was zero, which
		// desynchronises everything after it: the next hop reads this bootstrap POSITIONALLY, so a
		// missing message means its CONSOLECAPS read consumes whatever came next instead. That was
		// harmless while this was the last message and stopped being harmless the moment one was
		// added after it. An empty CONSOLECAPS is what "the boot granted none" looks like on the
		// wire, and every reader downstream already treats a zero capability as not granted.
		send_caps(sm_side, b"CONSOLECAPS", &console_caps[..console_cap_count]);
		// The boot window, last, in the position it arrived in.
		let mut window_msg: [u8; 7 + 16] = [0u8; 7 + 16];
		window_msg[..7].copy_from_slice(b"BOOTWIN");
		window_msg[7..15].copy_from_slice(&boot_deadline.to_le_bytes());
		window_msg[15..].copy_from_slice(&boot_window.to_le_bytes());
		send_blocking(sm_side, &window_msg, 0);
		// The loader's choice, last, in the position it arrived in.
		let mut root_msg: [u8; 7 + 24] = [0u8; 7 + 24];
		root_msg[..7].copy_from_slice(b"ROOTSEL");
		root_msg[7..].copy_from_slice(&root_selection);
		send_blocking(sm_side, &root_msg, 0);
	}

	// 4. relay every report ServiceManager sends up to the kernel. ServiceManager's
	//    own "ServiceManager: online" is the terminal report of the boot chain: once
	//    we have relayed it the set is up, so we stop and report in ourselves. Using
	//    that explicit marker (rather than waiting for ServiceManager to close its
	//    end) keeps the hand-off deterministic under the cooperative scheduler.
	// RELAYING AND SERVING ARE ONE LOOP, and they have to be.
	//
	// SystemPower cannot wait until the boot chain is up: ServiceManager asks for its connection
	// WHILE it is bringing the system up, and DeviceManager asks for two more for the keyboard
	// drivers. A manager that relayed reports first and served afterwards would be a manager
	// nobody could reach during the one phase that needs it - which is a deadlock, and it is
	// exactly what the first version of this did.
	unsafe { serve_system_power(power, power_server, sm_side, bootstrap, branch_domain as u64, &mut buf) };

	// 5. STAY. This process used to exit here, which left the ServiceManager branch with no owner
	//    for the whole life of the system - and left the root-Domain handle needing a home, which
	//    is why it was handed down to a keyboard driver.
	//
	//    What it does from here is narrow on purpose: it serves SystemPower and it watches the
	//    branch it owns. It takes no part in per-service wiring, application policy or driver
	//    policy - ServiceManager remains the one service supervisor - because a manager that
	//    absorbed those would be a second supervisor with the broadest capability in the system.
	//
	//    If it ends, the kernel reboots: the branch below it is full of processes nobody is
	//    supervising, and starting a replacement beside them would be two managers over one
	//    orphaned tree.
	exit();
}

// Serve SystemPower, and watch the branch this process owns, until one of them ends.
//
// TWO CHANNELS, ONE LOOP. A request to stop the machine and the death of ServiceManager are both
// things this process must notice, and a blocking wait on either alone would miss the other.
unsafe fn serve_system_power(power: u64, requests: u64, branch: u64, up: u64, domain: u64, buf: &mut [u8]) {
	unsafe {
		// THE BRANCH GOES DOWN WITH ITS SUPERVISOR, through the Domain that contains it.
		//
		// Whichever way this loop ends, what is left below is a set of processes whose supervisor
		// has gone, and this process is the only holder of `MANAGE` on the Domain they all live
		// in - which is the entire reason M12 put them in one. One syscall ends every one of them,
		// where a list of Process handles kept by hand could be missing a member and never say so.
		//
		// The kernel reboots after this, having seen this process end; the teardown still happens
		// first, because "the owner tears down what it owns" is the property, not "something
		// eventually stops the machine".
		let _guard = BranchGuard { domain };
		// NO FALLBACK WITHOUT A WAIT SET, and the one that was here was worse than none.
		//
		// It served the requests alone, on the theory that a machine which cannot be turned off is
		// worse than one whose branch death is noticed late. It could not: minting a per-holder
		// channel calls `waitset_add`, which fails on a zero handle, so every connect request was
		// refused - and the loop polled only the root channel, so a minted connection would never
		// have been read anyway. A comment promising a working power path over code that had none
		// is worse than the honest failure, which is this: the boot chain stops here and the kernel
		// escalates, as it does for any other way this process fails to come up.
		let created: i64 = waitset_create();
		if created < 0 {
			print(b"SystemManager: cannot create the power wait set; boot chain stops here\n");
			return;
		}
		let set: u64 = created as u64;
		if waitset_add(set, requests) < 0 || waitset_add(set, branch) < 0 {
			return;
		}
		// ONE CHANNEL PER HOLDER, minted on request. Two callers sharing a channel take each
		// other's replies - this system has a comment about that beside the op that exists to stop
		// it - and the Power key is owned by two keyboard drivers at once.
		// EACH CONNECTION WITH THE KOID THE WAIT SET KNOWS IT BY. A member is removed by koid and
		// not by handle - and a member left behind when its peer closes is permanently readable,
		// which turns this loop into a spin that starves the whole system. That is not a
		// hypothetical: it is what the first version of this did, sixteen million times.
		let mut connections: [(u64, u64); MAX_POWER_CLIENTS] = [(0, 0); MAX_POWER_CLIENTS];
		connections[0] = (requests, koid_of(requests));
		loop {
			if waitset_wait(set, 0, 0) < 0 {
				return;
			}
			// The branch first: what arrives on it is a boot report to relay, and a CLOSED end is
			// the event that ends this loop - answering a power request from a system whose
			// supervisor has gone is no better than letting the kernel reboot.
			// ONE REPORT PER PASS, NOT A DRAIN. Draining the branch in an inner loop pushes reports
			// at the reader back to back, and that reader is a channel with a bounded queue:
			// filling it makes `send_blocking` wait, and when the writable wait is refused it
			// SPINS - which keeps this process runnable, stops the scheduler ever going idle, and
			// so stops the very reader that would drain the queue. The system deadlocked on its own
			// last boot report. One at a time paces the writer to the reader, which is what the
			// blocking-receive loop this replaced did by construction.
			match try_recv(branch, buf) {
				Polled::Closed => return,
				Polled::Message { len, .. } => {
					let report: &[u8] = &buf[..len];
					let terminal: bool = report == b"ServiceManager: online";
					send_blocking(up, report, 0);
					// The boot chain's terminal report. This process reports in behind it and then
					// keeps going: it owns the branch below for the life of the system.
					if terminal {
						send_blocking(up, b"SystemManager: online", 0);
					}
				}
				Polled::Empty => {}
			}
			for index in 0..MAX_POWER_CLIENTS {
				let (chan, koid) = connections[index];
				if chan == 0 {
					continue;
				}
				match serve_power_once(power, chan, set, &mut connections, buf) {
					PowerStep::Continue => {}
					PowerStep::Gone => {
						// THE SET FIRST, THE HANDLE AFTER. A closed peer is permanently readable,
						// so a member left behind wakes this loop every pass forever - which keeps
						// the process runnable, stops the scheduler going idle, and starves the
						// very reader that would drain what this one is trying to send. The
						// storage service carries the same note beside `release_client`, having
						// found it as a test that never finished.
						waitset_remove(set, koid);
						close(chan);
						connections[index] = (0, 0);
					}
				}
			}
		}
	}
}

// Tears down the control-plane branch when the serving loop ends, by any of the ways it can end.
//
// A GUARD RATHER THAN A LINE AT THE BOTTOM, because that loop returns from five places - a wait
// that fails, a member that cannot be added, the branch closing, the request channel closing - and
// four of them would have been easy to miss.
struct BranchGuard {
	domain: u64,
}

impl Drop for BranchGuard {
	fn drop(&mut self) {
		unsafe { domain_kill(self.domain) };
	}
}

// The most holders of the Power key at once. Two keyboard drivers, the device manager that
// launches them and one spare: a bound rather than a guess, because a service that minted a channel
// per request would be one a caller could exhaust.
const MAX_POWER_CLIENTS: usize = 4;

// The kernel object id behind a handle, which is what a wait set names its members by.
unsafe fn koid_of(handle: u64) -> u64 {
	let mut info = ObjectInfo { koid: 0, object_type: 0, rights: 0, generation: 0, size: 0 };
	let read: i64 = unsafe { syscall(SYS_OBJECT_INFO_GET, handle, &mut info as *mut ObjectInfo as u64, core::mem::size_of::<ObjectInfo>() as u64, 0) } as i64;
	if read < 0 { 0 } else { info.koid }
}

enum PowerStep {
	Continue,
	Gone,
}

// Answer at most one SystemPower request on one connection.
//
// THE AUTHORITY NEVER LEAVES THIS PROCESS. What a caller gets back is whether the request was
// accepted; the syscall is made here, with the handle that stayed here.
unsafe fn serve_power_once(power: u64, requests: u64, set: u64, connections: &mut [(u64, u64); MAX_POWER_CLIENTS], buf: &mut [u8]) -> PowerStep {
	unsafe {
		let len: usize = match try_recv(requests, buf) {
			Polled::Message { len, .. } => len,
			Polled::Empty => return PowerStep::Continue,
			Polled::Closed => return PowerStep::Gone,
		};
		// A connect request gets its own channel, so no two holders share a queue.
		if len >= 2 && u16::from_le_bytes([buf[0], buf[1]]) == CONNECT_OP {
			let mut slot: Option<usize> = None;
			for (index, entry) in connections.iter().enumerate() {
				if entry.0 == 0 {
					slot = Some(index);
					break;
				}
			}
			match (slot, channel()) {
				(Some(index), Some((server, client))) if waitset_add(set, server) >= 0 => {
					connections[index] = (server, koid_of(server));
					send_blocking(requests, &[], client);
				}
				// Refused by sending nothing back with the reply: a caller that gets no capability
				// knows it has no connection, which is true and is better than a channel nobody
				// polls.
				(_, pair) => {
					if let Some((server, client)) = pair {
						close(server);
						close(client);
					}
					send_blocking(requests, &[], 0);
				}
			}
			return PowerStep::Continue;
		}
		let mut reply: [u8; 64] = [0u8; 64];
		let mut reply_handle = proto::codec::Handles::new();
		let mut api = PowerApi { power };
		if let Some(n) = system_power::dispatch(&mut api, &buf[..len], &mut proto::codec::Handles::new(), &mut reply, &mut reply_handle) {
			send_blocking(requests, &reply[..n], 0);
		}
		PowerStep::Continue
	}
}

// The two ops, and nothing else. A client of this can stop the machine and can do NOTHING further -
// which is the whole difference between this and the handle it replaced.
struct PowerApi {
	power: u64,
}

impl system_power::Service for PowerApi {
	fn reboot(&mut self) -> Result<(), Error> {
		// Returns only if the kernel refused: a reboot that happens does not come back.
		let result: i64 = unsafe { syscall(SYS_SYSTEM_POWER, self.power, POWER_REBOOT, 0, 0) } as i64;
		if result < 0 { Err(Error::Denied) } else { Ok(()) }
	}

	fn power_off(&mut self) -> Result<(), Error> {
		let result: i64 = unsafe { syscall(SYS_SYSTEM_POWER, self.power, POWER_OFF, 0, 0) } as i64;
		if result < 0 { Err(Error::Denied) } else { Ok(()) }
	}
}
