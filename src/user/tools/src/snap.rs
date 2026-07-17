// snap - manage the system volume's named snapshots, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it exactly
// one capability - a StorageService (volume) client - and forwards it the shell's stdout
// console and the argument string (the sub-form: "list", "create <name>", "delete <name>",
// or "cat <name> <vol://...>"). snap lists, creates, deletes, or reads from a snapshot through
// its storage grant, prints the result to the inherited stdout, and exits. A standalone
// command, not a shell built-in: it reaches the filesystem only through the one capability the
// permission store granted it, and renders on the same terminal as the shell that launched it.

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
		// 2. receive the argument string - the snapshot sub-form.
		let args: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		// 3. receive the one capability the manifest grants: a StorageService client.
		let storage: u64 = recv_tagged(bootstrap, &mut buf, b"STORAGE").unwrap_or_else(|| exit());
		snap(storage, &args);
	}
	exit();
}

// Route the snapshot sub-form to its handler: the first token is the subcommand, the rest its
// argument(s). All operate on the system volume through the one storage grant.
unsafe fn snap(storage: u64, args: &[u8]) {
	unsafe {
		let (sub, rest): (&[u8], &[u8]) = match args.iter().position(|&b: &u8| b == b' ') {
			Some(sp) => (&args[..sp], &args[sp + 1..]),
			None => (args, b""),
		};
		match sub {
			b"list" | b"" => snap_list(storage),
			b"create" => snap_create(storage, rest),
			b"delete" => snap_delete(storage, rest),
			b"cat" => match rest.iter().position(|&b: &u8| b == b' ') {
				Some(sp) => {
					let uri: &[u8] = &rest[sp + 1..];
					if !snap_cat(storage, &rest[..sp], uri) {
						print(b"snap cat: could not read ");
						print(uri);
						print(b"\n");
					}
				}
				None => print(b"usage: snap cat <name> <vol://...>\n"),
			},
			_ => print(b"snap: unknown subcommand\n"),
		}
	}
}

// List the volume's named snapshots (each as name + pinned generation), oldest first.
unsafe fn snap_list(storage: u64) {
	unsafe {
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let snaps = match client.snap_list() {
			Some(Ok(s)) => s,
			_ => {
				print(b"snap: StorageService unavailable\n");
				return;
			}
		};
		print(b"snapshots (");
		print_usize(snaps.len());
		print(b"):\n");
		for s in &snaps {
			print(b"  ");
			print(s.name.as_bytes());
			print(b" (generation ");
			print_usize(s.generation as usize);
			print(b")\n");
		}
	}
}

// Create a named read-only snapshot of the volume, pinning the current state.
unsafe fn snap_create(storage: u64, name: &[u8]) {
	unsafe {
		let snapshot: String = String::from_utf8_lossy(name).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		match client.snap_create(&snapshot) {
			Some(Ok(())) => {
				print(b"created snapshot ");
				print(name);
				print(b"\n");
			}
			_ => {
				print(b"snap create: could not create ");
				print(name);
				print(b"\n");
			}
		}
	}
}

// Delete a named snapshot, releasing the blocks it pinned.
unsafe fn snap_delete(storage: u64, name: &[u8]) {
	unsafe {
		let snapshot: String = String::from_utf8_lossy(name).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		match client.snap_delete(&snapshot) {
			Some(Ok(())) => {
				print(b"deleted snapshot ");
				print(name);
				print(b"\n");
			}
			_ => {
				print(b"snap delete: could not delete ");
				print(name);
				print(b"\n");
			}
		}
	}
}

// Read a file from inside a named snapshot, printing an earlier state of the volume - the
// snapshot counterpart of `cat`. Returns whether the file could be read.
unsafe fn snap_cat(storage: u64, name: &[u8], uri: &[u8]) -> bool {
	unsafe {
		let snapshot: String = String::from_utf8_lossy(name).into_owned();
		let path: String = String::from_utf8_lossy(uri).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let result = match client.snap_open(&snapshot, &path) {
			Some(Ok(r)) => r,
			_ => return false,
		};
		if result.file == 0 || result.size == 0 {
			return false;
		}
		let mapped: u64 = match map_object(result.file) {
			Some(base) => base,
			None => return false,
		};
		let contents: &[u8] = core::slice::from_raw_parts(mapped as *const u8, result.size as usize);
		print(contents);
		if contents.last() != Some(&b'\n') {
			print(b"\n");
		}
		unmap_object(result.file);
		close(result.file);
		true
	}
}

// Print a usize as decimal digits to stdout.
unsafe fn print_usize(mut n: usize) {
	unsafe {
		if n == 0 {
			print(b"0");
			return;
		}
		let mut buf: [u8; 20] = [0u8; 20];
		let mut i: usize = 20;
		while n > 0 {
			i -= 1;
			buf[i] = b'0' + (n % 10) as u8;
			n /= 10;
		}
		print(&buf[i..]);
	}
}
