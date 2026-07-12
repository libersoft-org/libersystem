// component_host - the WASI component host.
//
// This is the evolution of wasi_host: a host that runs a *real* WebAssembly
// component - one built by the Rust SDK (src/sdk) and emitted by the ordinary
// toolchain, not hand-encoded - and wires the component's imports onto typed system
// services with no ambient authority.
//
// A supervisor hands this program a bootstrap channel and, over it, exactly two
// capabilities: a StorageService client and a LogService client. The host then:
//
//   1. loads the component from storage (vol://system/app.wasm), rather than
//      embedding it in the kernel image - StorageService serves it from the ramdisk
//      volume that `just sdk` stages it into;
//   2. resolves each of the component's imports by its (module, field) name into a
//      typed operation - the `liber` world: `read` and `write` map to StorageService,
//      `log` maps to LogService - and traps any import it does not recognize;
//   3. instantiates and runs the component on the `wasm` runtime, servicing each
//      import from the matching granted service.
//
// The component never names a path, a channel, or a service. It only sees three
// functions, and through them reaches exactly the two capabilities the host was
// granted and nothing else - a WASI "world" is precisely the set of imports the host
// wires up. After running it the host reports a small result (whether the log grant
// was reached, the component's float result, and the bytes it produced) back over
// the bootstrap channel, then exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::{log, volume, Entry, Field, OpenOpts, Severity};
use rt::*;
use wasm::{Host, Instance, Module, Trap, Value};

// The component is loaded from storage, not embedded: StorageService serves it over
// vol:// from the ramdisk volume that `just sdk` builds it into.
const COMPONENT_URI: &[u8] = b"vol://system/app.wasm";

// The one input file the component's `read` import is wired to. The component never
// names it - it only calls `read`, and the host reads exactly this file.
const INPUT_URI: &[u8] = b"vol://system/hello.txt";

// Where the component's `write` import persists its output. On the read-only ramdisk
// volume this write is denied (and reported as zero bytes written); on a writable
// disk it lands here. Either way the component cannot choose the path.
const OUTPUT_URI: &[u8] = b"vol://system/out.txt";

// The `liber` world: the imports the host recognizes, resolved by name. Anything
// else is `Unknown` and traps when the component calls it - the component reaches
// nothing the host did not explicitly wire to a granted service.
#[derive(Clone, Copy)]
enum WorldFn {
	Read,
	Write,
	Log,
	Unknown,
}

// Resolve one import's (module, field) name to its world operation. This is the
// whole authority surface: only these three names are wired.
fn resolve(module: &str, field: &str) -> WorldFn {
	match (module, field) {
		("liber", "read") => WorldFn::Read,
		("liber", "write") => WorldFn::Write,
		("liber", "log") => WorldFn::Log,
		_ => WorldFn::Unknown,
	}
}

// The host: the two typed-service capabilities it was granted (a StorageService
// client and a LogService client) and the per-import dispatch table resolved at
// instantiation. It holds no ambient authority - only these two channels, reachable
// only through the three wired imports.
struct ComponentHost {
	storage: u64,
	logsvc: u64,
	imports: Vec<WorldFn>,
	// for the report: whether the log grant was reached and the entry accepted, and
	// the bytes the component handed to its `write` import (its output, captured
	// through the granted write path regardless of whether the volume persisted it).
	logged: bool,
	output: Vec<u8>,
}

impl Host for ComponentHost {
	fn call_import(&mut self, import: u32, args: &[Value], memory: &mut [u8]) -> Result<Vec<Value>, Trap> {
		match self.imports.get(import as usize).copied().unwrap_or(WorldFn::Unknown) {
			// liber.read(ptr, max) -> n: read the one granted input file through
			// StorageService into the component's memory, return the byte count.
			WorldFn::Read => {
				let (ptr, end): (usize, usize) = window(args, memory.len())?;
				let n: usize = unsafe { read_file(self.storage, INPUT_URI, &mut memory[ptr..end]) }.ok_or(Trap("granted read failed"))?;
				Ok(alloc::vec![Value::I32(n as i32)])
			}
			// liber.write(ptr, len) -> n: persist the component's bytes to the one
			// granted output file through StorageService, return the byte count (zero
			// when the granted volume is read-only). The bytes are captured for the
			// report either way - they are the component's output, seen through the
			// granted write path, not a guess at where they sit in linear memory.
			WorldFn::Write => {
				let (ptr, end): (usize, usize) = window(args, memory.len())?;
				self.output = memory[ptr..end].to_vec();
				let n: usize = unsafe { write_file(self.storage, OUTPUT_URI, &memory[ptr..end]) };
				Ok(alloc::vec![Value::I32(n as i32)])
			}
			// liber.log(ptr, len): emit the component's bytes as one structured entry
			// through LogService - the console/cli of the world.
			WorldFn::Log => {
				let (ptr, end): (usize, usize) = window(args, memory.len())?;
				if unsafe { emit_log(self.logsvc, &memory[ptr..end]) } {
					self.logged = true;
				}
				Ok(Vec::new())
			}
			// any other import is outside the granted world.
			WorldFn::Unknown => Err(Trap("import not granted")),
		}
	}
}

// Resolve a (ptr, len) argument pair into a bounds-checked [ptr, end) memory window.
fn window(args: &[Value], mem_len: usize) -> Result<(usize, usize), Trap> {
	let ptr: usize = args.first().map(|v: &Value| v.as_i32() as usize).unwrap_or(0);
	let len: usize = args.get(1).map(|v: &Value| v.as_i32() as usize).unwrap_or(0);
	let end: usize = ptr.saturating_add(len).min(mem_len);
	if ptr > end {
		return Err(Trap("memory window out of bounds"));
	}
	Ok((ptr, end))
}

// Load the component module from storage: open it over StorageService, map the
// returned shared buffer, copy its bytes out, and release the mapping and handle.
// This is how a component reaches the runtime without being embedded in the kernel.
unsafe fn load_component(storage: u64, uri: &[u8]) -> Option<Vec<u8>> {
	unsafe {
		let opts: OpenOpts = OpenOpts { path: String::from_utf8_lossy(uri).into_owned(), write: false, create: false };
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let result = match client.open(&opts) {
			Some(Ok(r)) => r,
			_ => return None,
		};
		if result.file == 0 || result.size == 0 {
			return None;
		}
		let mapped: u64 = map_object(result.file)?;
		let bytes: Vec<u8> = core::slice::from_raw_parts(mapped as *const u8, result.size as usize).to_vec();
		unmap_object(result.file);
		close(result.file);
		Some(bytes)
	}
}

// Read the granted input file over StorageService into `dst`, returning the number
// of bytes copied. None on any failure.
unsafe fn read_file(storage: u64, uri: &[u8], dst: &mut [u8]) -> Option<usize> {
	unsafe {
		let opts: OpenOpts = OpenOpts { path: String::from_utf8_lossy(uri).into_owned(), write: false, create: false };
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let result = match client.open(&opts) {
			Some(Ok(r)) => r,
			_ => return None,
		};
		if result.file == 0 {
			return None;
		}
		read_into(result.file, result.size, dst)
	}
}

// Write `bytes` to the granted output file over StorageService, returning the number
// of bytes the service accepted (zero if the volume is read-only or the write fails).
unsafe fn write_file(storage: u64, uri: &[u8], bytes: &[u8]) -> usize {
	unsafe {
		let data: proto::codec::Buffer = match make_buffer(bytes) {
			Some(b) => b,
			None => return 0,
		};
		let path: String = String::from_utf8_lossy(uri).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		match client.write(&path, &data) {
			Some(Ok(())) => bytes.len(),
			_ => 0,
		}
	}
}

// Emit `msg` as one structured log entry over LogService. Returns whether the
// service accepted it - the host's proof the log grant is live.
unsafe fn emit_log(logsvc: u64, msg: &[u8]) -> bool {
	let entry: Entry = Entry { timestamp: unsafe { clock() }, severity: Severity::Info, source: String::from("component"), fields: alloc::vec![Field { key: String::from("message"), value: String::from_utf8_lossy(msg).into_owned() }] };
	let mut client = log::Client::new(ChannelTransport { chan: logsvc });
	matches!(client.emit(&entry), Some(Ok(())))
}

// Stage `bytes` in a fresh MemoryObject and return a transferable read-only buffer
// (read + map + transfer) over it for a zero-copy `write`. The generated client's
// send consumes the handle. A zero-length write still allocates one byte so the
// create cannot fail on an empty request.
unsafe fn make_buffer(bytes: &[u8]) -> Option<proto::codec::Buffer> {
	unsafe {
		let obj: i64 = memory_object_create(bytes.len().max(1) as u64);
		if obj < 0 {
			return None;
		}
		let obj: u64 = obj as u64;
		let mapped: u64 = match map_object(obj) {
			Some(base) => base,
			None => {
				close(obj);
				return None;
			}
		};
		core::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped as *mut u8, bytes.len());
		unmap_object(obj);
		let granted: i64 = duplicate(obj, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		close(obj);
		if granted < 0 {
			return None;
		}
		Some(proto::codec::Buffer { handle: granted as u64, len: bytes.len() as u64 })
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];

	// 1. receive the two typed capabilities the host is granted, in order: a
	//    StorageService client (filesystem) and a LogService client (the console).
	//    The host never receives - and so can never reach - anything else.
	let storage: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"STORAGE") }.unwrap_or_else(|| exit());
	let logsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"LOG") }.unwrap_or_else(|| exit());

	// 2. load the component from storage and parse it. It is an ordinary toolchain
	//    artifact, not embedded in the kernel image.
	let bytes: Vec<u8> = unsafe { load_component(storage, COMPONENT_URI) }.unwrap_or_else(|| exit());
	let module: Module = match wasm::parse(&bytes) {
		Ok(m) => m,
		Err(_) => exit(),
	};

	// 3. resolve every import by name into the dispatch table, then instantiate.
	//    An import outside the `liber` world resolves to `Unknown` and will trap if
	//    the component ever calls it - no ambient authority.
	let imports: Vec<WorldFn> = module.imports.iter().map(|i: &wasm::module::Import| resolve(&i.module, &i.field)).collect();
	let mut instance: Instance = Instance::new(&module);
	let mut host: ComponentHost = ComponentHost { storage, logsvc, imports, logged: false, output: Vec::new() };

	// 4. run the component: `run` reads its granted file, transforms it, logs it, and
	//    writes it back; `score` exercises the float path on real toolchain output.
	let _count: i32 = match instance.invoke("run", &[], &mut host) {
		Ok(results) => results.first().map(|v: &Value| v.as_i32()).unwrap_or(0),
		Err(_) => exit(),
	};
	let score: i32 = instance.invoke("score", &[Value::I32(10)], &mut host).ok().and_then(|r: Vec<Value>| r.first().map(|v: &Value| v.as_i32())).unwrap_or(0);

	// 5. report back over the bootstrap channel: a one-byte log-grant flag, the
	//    score as a little-endian i32, then the bytes the component produced (those it
	//    handed to its `write` import, captured through the granted write path). The
	//    supervisor / test reads and checks these.
	let mut report: Vec<u8> = Vec::with_capacity(5 + host.output.len());
	report.push(host.logged as u8);
	report.extend_from_slice(&score.to_le_bytes());
	report.extend_from_slice(&host.output);
	unsafe {
		send_blocking(bootstrap, &report, 0);
	}
	exit();
}
