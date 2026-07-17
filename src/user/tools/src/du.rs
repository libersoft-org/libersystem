// du - report the recursive disk usage of a directory tree, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it
// exactly one capability - the `volumes` bundle of StorageService clients - and forwards
// it the shell's stdout console, the argument string (flags and an optional path), and the
// inherited working directory. du resolves the path against that cwd, walks the tree
// through its storage grant summing every file's size, and prints the cumulative bytes of
// each directory (children before their parent, the way `du` does) with the whole tree's
// total last, then exits. No current tool reports this - `lsvol` shows a volume's overall
// size / used / free, `ls` one directory's entries, but neither the size of a subtree.
//
// Flags:
//   -s        summary only: print just the argument's total, not each subdirectory
//   -h        human-readable sizes (kB / MB / GB ...) instead of raw bytes
//   json      render the walk as a JSON array of {path, bytes} records (json-min: minified)
//
// A standalone command, not a shell built-in: it reaches the filesystem only through the
// one capability the permission store granted it, and renders on the same terminal as the
// shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::codec::JsonMode;
use proto::path;
use proto::system::{FileInfo, FileType, volume};
use rt::*;

const USAGE: &[u8] = b"usage: du [-s] [-h] [json | json-min] [path]
  -s  summary only: just the total for the path
  -h  human-readable sizes (kB..YB) instead of raw bytes
  json / json-min  a JSON array of {path, bytes} (pretty / minified)
";

// The deepest directory nesting du descends - a guard against a pathological tree
// (a real filesystem tree has no cycles, but the bound keeps the stack finite).
const MAX_DEPTH: u32 = 64;

// One directory's cumulative size, accumulated during the walk for the text / JSON output.
struct DirUsage {
	path: String,
	bytes: u64,
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message).
		inherit_stdout(bootstrap);
		// 2. receive the argument string - flags and an optional path, in any order.
		let arg_raw: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		let mut summary_only: bool = false;
		let mut human: bool = false;
		let mut mode: Option<JsonMode> = None;
		let mut arg: Vec<u8> = Vec::new();
		for token in arg_raw.split(|&b| b == b' ').filter(|t: &&[u8]| !t.is_empty()) {
			match token {
				b"-s" => summary_only = true,
				b"-h" => human = true,
				b"json" | b"json-min" => mode = JsonMode::parse(token),
				_ if token.starts_with(b"-") => {
					print(USAGE);
					exit();
				}
				_ if arg.is_empty() => arg.extend_from_slice(token),
				_ => {
					print(USAGE);
					exit();
				}
			}
		}
		// 3. receive the five volume clients the `volumes` capability bundles (SYSTEM /
		//    MEDIA / ISO / UDF / USB, in grant order); a volume whose disk is absent is 0.
		let system: u64 = recv_tagged(bootstrap, &mut buf, b"SYSTEM").unwrap_or(0);
		let media: u64 = recv_tagged(bootstrap, &mut buf, b"MEDIA").unwrap_or(0);
		let iso: u64 = recv_tagged(bootstrap, &mut buf, b"ISO").unwrap_or(0);
		let udf: u64 = recv_tagged(bootstrap, &mut buf, b"UDF").unwrap_or(0);
		let usb: u64 = recv_tagged(bootstrap, &mut buf, b"USB").unwrap_or(0);
		// 4. receive the inherited working directory and resolve the path against it.
		let cwd: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => Vec::new(),
		};
		let cwd_str: &str = core::str::from_utf8(&cwd).unwrap_or("");
		let uri: String = match path::resolve(cwd_str, &arg) {
			Some(u) => u,
			None => {
				print(b"du: invalid path\n");
				exit();
			}
		};
		let storage: u64 = path::volume_client(cwd_str, &arg, system, media, iso, udf, usb);
		du(storage, uri, summary_only, human, mode);
	}
	exit();
}

// Walk the tree rooted at `uri` through the storage grant and print each directory's
// cumulative size (children before their parent), the whole tree's total last.
unsafe fn du(storage: u64, uri: String, summary_only: bool, human: bool, mode: Option<JsonMode>) {
	unsafe {
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let mut usage: Vec<DirUsage> = Vec::new();
		let total: Option<u64> = walk(&mut client, &uri, 0, &mut usage);
		let total: u64 = match total {
			Some(t) => t,
			None => {
				print(b"du: cannot read the path\n");
				return;
			}
		};
		// The argument itself is the last (outermost) directory line.
		usage.push(DirUsage { path: uri, bytes: total });
		if let Some(mode) = mode {
			let mut out = String::from("[");
			let rows: &[DirUsage] = if summary_only { &usage[usage.len() - 1..] } else { &usage };
			for (i, u) in rows.iter().enumerate() {
				if i > 0 {
					out.push(',');
				}
				let _ = core::fmt::Write::write_fmt(&mut out, format_args!("{{\"path\":\"{}\",\"bytes\":{}}}", u.path, u.bytes));
			}
			out.push(']');
			print(mode.render(out).as_bytes());
			print(b"\n");
			return;
		}
		let rows: &[DirUsage] = if summary_only { &usage[usage.len() - 1..] } else { &usage };
		for u in rows {
			let mut line = String::new();
			push_size(&mut line, u.bytes, human);
			line.push('\t');
			line.push_str(&u.path);
			line.push('\n');
			print(line.as_bytes());
		}
	}
}

// Sum the sizes under `uri`, recording one DirUsage per directory (post-order, so a
// child is recorded before its parent). Returns the subtree's total bytes, or None if
// this directory itself cannot be listed (the caller reports it for the root; a deeper
// unreadable directory contributes 0 rather than aborting the whole walk).
unsafe fn walk(client: &mut volume::Client<ChannelTransport>, uri: &str, depth: u32, usage: &mut Vec<DirUsage>) -> Option<u64> {
	unsafe {
		let consumer: u64 = client.list(uri)?;
		let entries: Vec<FileInfo> = drain_stream(consumer, volume::list_read);
		let mut total: u64 = 0;
		for e in &entries {
			if e.r#type == FileType::Dir {
				// Descend, unless the depth guard is hit (then count the dir's own entry
				// size only, without recursing further).
				if depth < MAX_DEPTH {
					let child: String = format!("{uri}/{}", e.name);
					if let Some(sub) = walk(client, &child, depth + 1, usage) {
						total = total.saturating_add(sub);
					}
				}
			} else {
				total = total.saturating_add(e.size);
			}
		}
		// Record every directory but the argument root here (the caller pushes the root
		// last, so it prints outermost); children are already in `usage`, before us.
		if depth > 0 {
			usage.push(DirUsage { path: String::from(uri), bytes: total });
		}
		Some(total)
	}
}

// Append a byte count to `out`: raw when `!human`, else scaled to the largest whole
// unit (kB / MB / GB ...) with one decimal.
fn push_size(out: &mut String, bytes: u64, human: bool) {
	if !human {
		let _ = core::fmt::Write::write_fmt(out, format_args!("{bytes}"));
		return;
	}
	// A u64 byte count tops out below 16 EB, so kB..EB covers every representable
	// size (a larger unit's scale would overflow the shift).
	let units: [(&str, u64); 6] = [("EB", 1 << 60), ("PB", 1 << 50), ("TB", 1 << 40), ("GB", 1 << 30), ("MB", 1 << 20), ("kB", 1 << 10)];
	for &(unit, scale) in &units {
		if bytes >= scale {
			let _ = core::fmt::Write::write_fmt(out, format_args!("{}.{} {}", bytes / scale, bytes % scale * 10 / scale, unit));
			return;
		}
	}
	let _ = core::fmt::Write::write_fmt(out, format_args!("{bytes} B"));
}
