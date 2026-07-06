// ls - list a directory's entries, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it
// exactly one capability - a StorageService (volume) client - and forwards it the shell's
// stdout console, the argument string (flags and the directory path, relative or
// absolute), and the inherited working directory. ls resolves the path against that cwd,
// lists the directory through its storage grant, sorts the entries and prints one row per
// entry - the name (directories in colour with a trailing '/'), the size and the
// last-modification time - with a summary line (directory / file counts and total bytes)
// at the end, then exits.
//
// Flags:
//   -s KEY[a|d]   sort key - u unsorted, a alphabet, e extension, s size, c creation
//                 time, m modification time - with an optional direction suffix, a
//                 ascending or d descending (directories group first under every key
//                 except u; default -s aa)
//   -u UNIT       size unit - b raw bytes, k|m|g|t|p|e|z|y one fixed unit (kB through
//                 YB), h the largest fitting unit (default -u b)
//   json          render the listing as a JSON array of file-info records (indented
//                 and colored; json-min for the minified machine form)
//
// A standalone command, not a shell built-in: it reaches the filesystem only through the
// one capability the permission store granted it, and renders on the same terminal as the
// shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::codec::JsonMode;
use proto::path;
use proto::system::{FileInfo, FileType, Timestamp, volume};
use rt::*;

// What the listing is ordered by; directories group first under every key but None.
#[derive(Clone, Copy, PartialEq)]
enum SortKey {
	None,
	Name,
	Ext,
	Size,
	Mtime,
	Ctime,
}

// How a size renders: raw byte counts, the largest fitting unit, or one fixed unit
// (carried as a power-of-two shift so even ZB and YB stay representable).
#[derive(Clone, Copy, PartialEq)]
enum Unit {
	Bytes,
	Auto,
	Fixed(u32, &'static str),
}

const USAGE: &[u8] = b"usage: ls [-s KEY[a|d]] [-u UNIT] [json | json-min] [path]
  -s  sort key: u unsorted, a alphabet, e extension, s size,
      c creation time, m modification time; append a or d for
      ascending or descending (default: -s aa)
  -u  size unit: b bytes, k m g t p e z y a fixed unit (kB..YB),
      h the largest fitting unit (default: -u b)
  json / json-min  a JSON array of the entries (pretty / minified)
";

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - flags and the directory path (relative to cwd
		//    or an absolute URI), in any order.
		let arg_raw: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		let mut key: SortKey = SortKey::Name;
		let mut reverse: bool = false;
		let mut unit: Unit = Unit::Bytes;
		let mut mode: Option<JsonMode> = None;
		let mut arg: Vec<u8> = Vec::new();
		let mut want_unit: bool = false;
		let mut want_sort: bool = false;
		for token in arg_raw.split(|&b| b == b' ').filter(|t: &&[u8]| !t.is_empty()) {
			if want_unit {
				unit = match parse_unit(token) {
					Some(u) => u,
					None => {
						print(USAGE);
						exit();
					}
				};
				want_unit = false;
				continue;
			}
			if want_sort {
				(key, reverse) = match parse_sort(token) {
					Some(s) => s,
					None => {
						print(USAGE);
						exit();
					}
				};
				want_sort = false;
				continue;
			}
			match token {
				b"-u" => want_unit = true,
				b"-s" => want_sort = true,
				b"json" | b"json-min" => mode = JsonMode::parse(token),
				_ if token.starts_with(b"-u") && token.len() > 2 => {
					unit = match parse_unit(&token[2..]) {
						Some(u) => u,
						None => {
							print(USAGE);
							exit();
						}
					};
				}
				_ if token.starts_with(b"-s") && token.len() > 2 => {
					(key, reverse) = match parse_sort(&token[2..]) {
						Some(s) => s,
						None => {
							print(USAGE);
							exit();
						}
					};
				}
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
		if want_unit || want_sort {
			print(USAGE);
			exit();
		}
		// 3. receive the four volume clients the `volumes` capability bundles (SYSTEM / MEDIA /
		//    ISO / UDF, in grant order); a volume whose disk is absent arrives as 0.
		let system: u64 = recv_tagged(bootstrap, &mut buf, b"SYSTEM").unwrap_or(0);
		let media: u64 = recv_tagged(bootstrap, &mut buf, b"MEDIA").unwrap_or(0);
		let iso: u64 = recv_tagged(bootstrap, &mut buf, b"ISO").unwrap_or(0);
		let udf: u64 = recv_tagged(bootstrap, &mut buf, b"UDF").unwrap_or(0);
		let usb: u64 = recv_tagged(bootstrap, &mut buf, b"USB").unwrap_or(0);
		// 4. receive the inherited working directory (the last bootstrap message), and resolve
		//    the path argument against it so a relative path reaches the same directory the shell would.
		let cwd: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => Vec::new(),
		};
		let cwd_str: &str = core::str::from_utf8(&cwd).unwrap_or("");
		let uri: String = match path::resolve(cwd_str, &arg) {
			Some(u) => u,
			None => {
				print(b"ls: invalid path\n");
				exit();
			}
		};
		// route the path to the client for the volume it names.
		let storage: u64 = path::volume_client(cwd_str, &arg, system, media, iso, udf, usb);
		ls(storage, uri.as_bytes(), key, reverse, unit, mode);
	}
	exit();
}

// The size unit a `-u` argument names: raw bytes, one fixed unit, or the largest
// fitting one.
fn parse_unit(token: &[u8]) -> Option<Unit> {
	match token {
		b"b" => Some(Unit::Bytes),
		b"k" => Some(Unit::Fixed(10, "kB")),
		b"m" => Some(Unit::Fixed(20, "MB")),
		b"g" => Some(Unit::Fixed(30, "GB")),
		b"t" => Some(Unit::Fixed(40, "TB")),
		b"p" => Some(Unit::Fixed(50, "PB")),
		b"e" => Some(Unit::Fixed(60, "EB")),
		b"z" => Some(Unit::Fixed(70, "ZB")),
		b"y" => Some(Unit::Fixed(80, "YB")),
		b"h" => Some(Unit::Auto),
		_ => None,
	}
}

// The sort order a `-s` argument names: a key letter with an optional direction suffix
// (a ascending - the default - or d descending).
fn parse_sort(token: &[u8]) -> Option<(SortKey, bool)> {
	let key: SortKey = match token.first()? {
		b'u' => SortKey::None,
		b'a' => SortKey::Name,
		b'e' => SortKey::Ext,
		b's' => SortKey::Size,
		b'c' => SortKey::Ctime,
		b'm' => SortKey::Mtime,
		_ => return None,
	};
	let reverse: bool = match &token[1..] {
		b"" | b"a" => false,
		b"d" => true,
		_ => return None,
	};
	Some((key, reverse))
}

// List the directory through the storage grant and print its entries to stdout - sorted
// by the chosen key (directories grouped first, ties broken by name), one aligned row per
// entry (name, size, modification time) and a closing summary - reporting a concise error
// if it cannot be listed.
unsafe fn ls(storage: u64, uri: &[u8], key: SortKey, reverse: bool, unit: Unit, mode: Option<JsonMode>) {
	unsafe {
		let path: &str = match core::str::from_utf8(uri) {
			Ok(s) => s,
			Err(_) => {
				print(b"ls: invalid path\n");
				return;
			}
		};
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		// the listing arrives as a stream of entries (one frame each), so a big
		// directory never has to fit one reply.
		let consumer: u64 = match client.list(path) {
			Some(c) => c,
			None => {
				print(b"ls: StorageService unavailable\n");
				return;
			}
		};
		// JSON: an array of the entries' generated records, in the chosen order (the
		// sort applies like in the text form; the sizes stay raw bytes - units are a
		// text-rendering concern).
		if let Some(mode) = mode {
			let mut files: Vec<FileInfo> = drain_stream(consumer, volume::list_read);
			sort_files(&mut files, key, reverse);
			let mut out = String::from("[");
			for (i, f) in files.iter().enumerate() {
				if i > 0 {
					out.push(',');
				}
				out.push_str(&f.to_json());
			}
			out.push(']');
			print(mode.render(out).as_bytes());
			print(b"\n");
			return;
		}
		if key == SortKey::None {
			// unsorted: render each entry as its frame arrives, so a huge listing
			// starts printing immediately (per-row widths - global column alignment
			// would need the whole set first).
			print(uri);
			print(b":\n");
			let mut dirs: usize = 0;
			let mut plain: usize = 0;
			let mut total: u64 = 0;
			loop {
				match recv_vec_blocking(consumer) {
					ReceivedVec::Message { bytes, .. } => {
						if let Some(f) = volume::list_read(&bytes) {
							let shown: usize = f.name.len() + if f.r#type == FileType::Dir { 1 } else { 0 };
							row(&f, shown, size_text(&f, unit).len(), unit, &mut dirs, &mut plain, &mut total);
						}
					}
					ReceivedVec::Closed => break,
				}
			}
			close(consumer);
			summary(dirs, plain, total, unit);
			return;
		}
		let mut files: Vec<FileInfo> = drain_stream(consumer, volume::list_read);
		sort_files(&mut files, key, reverse);
		print(uri);
		print(b":\n");
		// column widths: the display name (a directory carries a trailing '/') and the
		// size, so the rows align whatever the mix.
		let mut name_w: usize = 0;
		let mut size_w: usize = 0;
		for f in &files {
			let nw: usize = f.name.len() + if f.r#type == FileType::Dir { 1 } else { 0 };
			if nw > name_w {
				name_w = nw;
			}
			let sw: usize = size_text(f, unit).len();
			if sw > size_w {
				size_w = sw;
			}
		}
		let mut dirs: usize = 0;
		let mut plain: usize = 0;
		let mut total: u64 = 0;
		for f in &files {
			row(f, name_w, size_w, unit, &mut dirs, &mut plain, &mut total);
		}
		summary(dirs, plain, total, unit);
	}
}

// Order the entries: directories first under every key but `u`, then the chosen key
// (ascending unless the `d` suffix asked otherwise), ties broken by name so equal
// keys stay in a stable, readable order.
fn sort_files(files: &mut [FileInfo], key: SortKey, reverse: bool) {
	if key == SortKey::None {
		return;
	}
	files.sort_by(|a: &FileInfo, b: &FileInfo| {
		let dir_first = (b.r#type == FileType::Dir).cmp(&(a.r#type == FileType::Dir));
		if dir_first != core::cmp::Ordering::Equal {
			return dir_first;
		}
		let by_key = match key {
			SortKey::Ext => extension(&a.name).cmp(extension(&b.name)),
			SortKey::Size => a.size.cmp(&b.size),
			SortKey::Mtime => a.mtime.cmp(&b.mtime),
			SortKey::Ctime => a.ctime.cmp(&b.ctime),
			_ => core::cmp::Ordering::Equal,
		};
		let by_key = if reverse { by_key.reverse() } else { by_key };
		if by_key != core::cmp::Ordering::Equal {
			return by_key;
		}
		let by_name = a.name.cmp(&b.name);
		if key == SortKey::Name && reverse { by_name.reverse() } else { by_name }
	});
}

// Print one listing row (padded to the given column widths), counting it into the
// summary tallies.
unsafe fn row(f: &FileInfo, name_w: usize, size_w: usize, unit: Unit, dirs: &mut usize, plain: &mut usize, total: &mut u64) {
	unsafe {
		let is_dir: bool = f.r#type == FileType::Dir;
		let shown: usize = f.name.len() + if is_dir { 1 } else { 0 };
		print(b"  ");
		if is_dir {
			*dirs += 1;
			print(b"\x1b[1;34m");
			print(f.name.as_bytes());
			print(b"/\x1b[0m");
		} else {
			*plain += 1;
			*total += f.size;
			print(f.name.as_bytes());
		}
		pad(name_w - shown);
		let size: String = size_text(f, unit);
		pad(1 + size_w - size.len());
		print(size.as_bytes());
		print(b"  ");
		print_mtime(f.mtime);
		print(b"\n");
	}
}

// The closing summary: how much lives here, at a glance.
unsafe fn summary(dirs: usize, plain: usize, total: u64, unit: Unit) {
	unsafe {
		print_usize(dirs);
		print(if dirs == 1 { b" directory, " } else { b" directories, " });
		print_usize(plain);
		print(if plain == 1 { b" file, " } else { b" files, " });
		print(render_size(total, unit).as_bytes());
		print(b" total\n");
	}
}

// The extension a `-s e` sort orders by: the bytes after the name's last '.', or the
// empty string for a name without one (which therefore sorts first).
fn extension(name: &str) -> &str {
	match name.rfind('.') {
		Some(dot) => &name[dot + 1..],
		None => "",
	}
}

// The size column's text for one entry: a directory shows "-", a file its size in the
// chosen unit (a bare number in the default byte mode - the summary carries the label).
fn size_text(f: &FileInfo, unit: Unit) -> String {
	use core::fmt::Write as _;
	if f.r#type == FileType::Dir {
		return String::from("-");
	}
	if unit == Unit::Bytes {
		let mut out = String::new();
		let _ = write!(out, "{}", f.size);
		return out;
	}
	render_size(f.size, unit)
}

// Render a byte count in the chosen unit: raw bytes, the largest fitting unit (`-u h`),
// or one fixed unit, the latter two with one decimal place.
fn render_size(bytes: u64, unit: Unit) -> String {
	use core::fmt::Write as _;
	let mut out = String::new();
	match unit {
		Unit::Bytes => {
			let _ = write!(out, "{bytes} bytes");
		}
		Unit::Auto => {
			// a u64 byte count tops out below 16 EB, so the auto ladder ends there.
			let units: [(&str, u32); 6] = [("EB", 60), ("PB", 50), ("TB", 40), ("GB", 30), ("MB", 20), ("kB", 10)];
			for (name, shift) in units {
				if bytes >= 1 << shift {
					return scaled(bytes, shift, name);
				}
			}
			let _ = write!(out, "{bytes} B");
		}
		Unit::Fixed(shift, name) => {
			return scaled(bytes, shift, name);
		}
	}
	out
}

// A byte count scaled to the unit `1 << shift` with one decimal place, labelled `name`
// (u128 arithmetic, so even the YB shift stays exact).
fn scaled(bytes: u64, shift: u32, name: &str) -> String {
	use core::fmt::Write as _;
	let mut out = String::new();
	let whole: u128 = (bytes as u128) >> shift;
	let tenth: u128 = (((bytes as u128) & ((1u128 << shift) - 1)) * 10) >> shift;
	let _ = write!(out, "{whole}.{tenth} {name}");
	out
}

// Print the modification-time column: "YYYY-MM-DD HH:MM" UTC, or "-" when the backing
// filesystem carries no timestamp (mtime 0).
unsafe fn print_mtime(mtime: u64) {
	unsafe {
		if mtime == 0 {
			print(b"-");
			return;
		}
		let ts: Timestamp = Timestamp { unix_secs: mtime };
		let mut out: [u8; 24] = [0u8; 24];
		let n: usize = ts.render(&mut out);
		if n >= 16 {
			// the ISO instant, cut to the minute, with a space for the 'T'.
			out[10] = b' ';
			print(&out[..16]);
		} else {
			print(b"-");
		}
	}
}

// Print `n` spaces (column padding).
unsafe fn pad(n: usize) {
	unsafe {
		for _ in 0..n {
			print(b" ");
		}
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
