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
	//     `SYS_SYSTEM_POWER` now checks. This process never stops the machine itself; it holds
	//     the capability only to pass it to ServiceManager, which is where the graceful
	//     shutdown lives and which delegates it onward to the keyboard driver.
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
	if unsafe { spawn(svc_elf, svc_side) } < 0 {
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
		send_blocking(sm_side, b"POWER", power);
		let mode_msg: [u8; 5] = [b'M', b'O', b'D', b'E', mode];
		send_blocking(sm_side, &mode_msg, 0);
		// The three console/display capabilities, last, in the order they arrived.
		if console_cap_count > 0 {
			send_caps(sm_side, b"CONSOLECAPS", &console_caps[..console_cap_count]);
		}
	}

	// 4. relay every report ServiceManager sends up to the kernel. ServiceManager's
	//    own "ServiceManager: online" is the terminal report of the boot chain: once
	//    we have relayed it the set is up, so we stop and report in ourselves. Using
	//    that explicit marker (rather than waiting for ServiceManager to close its
	//    end) keeps the hand-off deterministic under the cooperative scheduler.
	loop {
		match unsafe { recv_blocking(sm_side, &mut buf) } {
			Received::Message { len, .. } => {
				let report: &[u8] = &buf[..len];
				let terminal: bool = report == b"ServiceManager: online";
				unsafe {
					send_blocking(bootstrap, report, 0);
				}
				if terminal {
					break;
				}
			}
			Received::Closed => break,
		}
	}
	unsafe {
		send_blocking(bootstrap, b"SystemManager: online", 0);
	}
	exit();
}
