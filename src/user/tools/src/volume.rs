// volume - the system volume's health and policy verbs, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it exactly
// one capability - a StorageService (volume) client - and forwards it the shell's stdout
// console and the argument string (the sub-form: "status", "compress on|off", "fsck", or
// "restore <vol://...> [snapshot]"). volume reports the filesystem's identity and health
// (label, pool and free bytes, the compression switch, read-only), flips transparent
// compression for new writes, verifies every live block against its checksum naming the
// damaged files, or restores a file from a snapshot - through its storage grant, printing to
// the inherited stdout. A standalone command, not a shell built-in: it reaches the filesystem
// only through the one capability the permission store granted it, and renders on the same
// terminal as the shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::volume;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the volume sub-form.
		let args: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		// 3. receive the one capability the manifest grants: a StorageService client.
		let storage: u64 = recv_tagged(bootstrap, &mut buf, b"STORAGE").unwrap_or_else(|| exit());
		run(storage, &args);
	}
	exit();
}

// Route the volume sub-form to its handler: the first token is the subcommand, the rest its
// argument(s). All operate on the system volume through the one storage grant.
unsafe fn run(storage: u64, args: &[u8]) {
	unsafe {
		let (sub, rest): (&[u8], &[u8]) = match args.iter().position(|&b: &u8| b == b' ') {
			Some(sp) => (&args[..sp], &args[sp + 1..]),
			None => (args, b""),
		};
		match sub {
			b"status" | b"" => status(storage),
			b"compress" => match rest {
				b"on" => set_compression(storage, true),
				b"off" => set_compression(storage, false),
				_ => print(b"usage: volume compress on|off\n"),
			},
			b"fsck" => fsck(storage),
			b"restore" => match rest.iter().position(|&b: &u8| b == b' ') {
				Some(sp) => restore(storage, &rest[..sp], &rest[sp + 1..]),
				None if !rest.is_empty() => restore(storage, rest, b""),
				None => print(b"usage: volume restore <vol://...> [snapshot]\n"),
			},
			_ => print(b"volume: unknown subcommand (status, compress on|off, fsck, restore)\n"),
		}
	}
}

// Report the filesystem's identity and health: label, pool and free bytes, the compression
// switch, and whether the mount is read-only.
unsafe fn status(storage: u64) {
	unsafe {
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let st = match client.status() {
			Some(Ok(st)) => st,
			_ => {
				print(b"volume: StorageService unavailable\n");
				return;
			}
		};
		let mut out = String::new();
		{
			use core::fmt::Write as _;
			let used: u64 = st.total_bytes - st.free_bytes;
			let _ = writeln!(out, "vol://system \"{}\"", st.label);
			let _ = writeln!(out, "  filesystem:  {}", st.filesystem);
			let _ = writeln!(out, "  size:        {} ({} bytes)", human(st.total_bytes), st.total_bytes);
			let _ = writeln!(out, "  used:        {} ({} bytes)", human(used), used);
			let _ = writeln!(out, "  free:        {} ({} bytes)", human(st.free_bytes), st.free_bytes);
			let _ = writeln!(out, "  compression: {}", if st.compression { "on" } else { "off" });
			let _ = writeln!(out, "  mount:       {}", if st.read_only { "READ-ONLY (degraded or snapshot)" } else { "read-write" });
		}
		print(out.as_bytes());
	}
}

// Flip transparent compression for new whole-file writes.
unsafe fn set_compression(storage: u64, enabled: bool) {
	unsafe {
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		match client.set_compression(&enabled) {
			Some(Ok(())) => print(if enabled { b"compression on (new writes compress)\n" as &[u8] } else { b"compression off (new writes stay raw)\n" }),
			Some(Err(_)) => print(b"volume compress: refused (read-only volume?)\n"),
			None => print(b"volume: StorageService unavailable\n"),
		}
	}
}

// Verify every live block against its checksum and name the damaged files.
unsafe fn fsck(storage: u64) {
	unsafe {
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let report = match client.fsck() {
			Some(Ok(r)) => r,
			_ => {
				print(b"volume: StorageService unavailable\n");
				return;
			}
		};
		if report.checksum_failures == 0 {
			print(b"fsck: clean (0 checksum failures)\n");
			return;
		}
		let mut out = String::new();
		{
			use core::fmt::Write as _;
			let _ = writeln!(out, "fsck: {} checksum failure(s) in {} file(s):", report.checksum_failures, report.damaged.len());
			for path in &report.damaged {
				let _ = writeln!(out, "  {path}");
			}
			let _ = writeln!(out, "restore with: volume restore <vol://system/...> [snapshot]");
		}
		print(out.as_bytes());
	}
}

// Copy a file out of a named snapshot (or, with no name, the previous generation) over the
// live file - the recovery verb for what fsck named.
unsafe fn restore(storage: u64, uri: &[u8], snapshot: &[u8]) {
	unsafe {
		let path: String = String::from_utf8_lossy(uri).into_owned();
		let snap: String = String::from_utf8_lossy(snapshot).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		match client.restore(&path, &snap) {
			Some(Ok(())) => {
				print(b"restored ");
				print(uri);
				if !snapshot.is_empty() {
					print(b" from snapshot ");
					print(snapshot);
				} else {
					print(b" from the previous generation");
				}
				print(b"\n");
			}
			Some(Err(_)) => {
				print(b"volume restore: could not restore ");
				print(uri);
				print(b" (missing file or snapshot?)\n");
			}
			None => print(b"volume: StorageService unavailable\n"),
		}
	}
}

// Render a byte count in a human unit (kB/MB/GB, one decimal place).
fn human(bytes: u64) -> String {
	use core::fmt::Write as _;
	let mut out = String::new();
	let units: [(&str, u64); 3] = [("GB", 1 << 30), ("MB", 1 << 20), ("kB", 1 << 10)];
	for (name, unit) in units {
		if bytes >= unit {
			let whole: u64 = bytes / unit;
			let tenth: u64 = (bytes % unit) * 10 / unit;
			let _ = write!(out, "{whole}.{tenth} {name}");
			return out;
		}
	}
	let _ = write!(out, "{bytes} B");
	out
}
