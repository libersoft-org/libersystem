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
//   1. loads the component from storage (vol://system/components/liber_component/app.wasm), rather than
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
use ipc_client::ChannelTransport;
use proto::system::{Entry, Field, OpenOpts, Severity, log, volume};
use rt::*;
use wasm::module::FuncType;
use wasm::{Host, Instance, Module, Trap, Value};

include!(concat!(env!("OUT_DIR"), "/program_paths.rs"));

// The world itself - which imports are granted and how a `(ptr, len)` becomes a memory window -
// lives in `component-world`, beside its tests. This binary is built for `*-unknown-none`, so
// nothing in it can run on the host: the only coverage those two decisions had was one kernel
// end-to-end path, which needs a boot per assertion and only ever takes the happy one.
use wasm::world::{STATUS_DENIED, STATUS_FAULT, STATUS_IO, WorldFn, resolve, window};

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
		// The dispatch table was built at instantiation from imports this world offers, so an index
		// outside it is the interpreter contradicting itself rather than a component asking for
		// something it was not granted.
		let Some(op) = self.imports.get(import as usize).copied() else {
			return Err(Trap("import index outside the resolved world"));
		};
		match op {
			// liber.read(ptr, max) -> n: read the one granted input file through
			// StorageService into the component's memory, return the byte count.
			WorldFn::Read => {
				let Some((ptr, end)) = window(args, memory.len()) else {
					return Ok(alloc::vec![Value::I32(STATUS_FAULT)]);
				};
				let input_path = factory_path("hello").ok_or(Trap("missing manifest input path"))?;
				// A FAILED READ IS A STATUS, not a trap. The component asked a legitimate question
				// and the answer is that the granted file could not be read; killing the instance
				// tells it nothing and gives it no chance to say so to its caller.
				let n: i32 = match unsafe { read_file(self.storage, input_path.as_bytes(), &mut memory[ptr..end]) } {
					Some(n) => n as i32,
					None => STATUS_IO,
				};
				Ok(alloc::vec![Value::I32(n)])
			}
			// liber.write(ptr, len) -> n: persist the component's bytes to the one
			// granted output file through StorageService, return the byte count (zero
			// when the granted volume is read-only). The bytes are captured for the
			// report either way - they are the component's output, seen through the
			// granted write path, not a guess at where they sit in linear memory.
			WorldFn::Write => {
				let Some((ptr, end)) = window(args, memory.len()) else {
					return Ok(alloc::vec![Value::I32(STATUS_FAULT)]);
				};
				self.output = memory[ptr..end].to_vec();
				let output_path = runtime_path("liber-component-output").ok_or(Trap("missing manifest output path"))?;
				// `Ok(0)` for a refused write and `Ok(0)` for an empty one were the same answer.
				// A volume that says no is `Denied`, and the component can act on that.
				let n: i32 = match unsafe { write_file(self.storage, output_path.as_bytes(), &memory[ptr..end]) } {
					Some(n) => n as i32,
					None => STATUS_DENIED,
				};
				Ok(alloc::vec![Value::I32(n)])
			}
			// liber.log(ptr, len): emit the component's bytes as one structured entry
			// through LogService - the console/cli of the world.
			WorldFn::Log => {
				let Some((ptr, end)) = window(args, memory.len()) else {
					return Ok(alloc::vec![Value::I32(STATUS_FAULT)]);
				};
				if unsafe { emit_log(self.logsvc, &memory[ptr..end]) } {
					self.logged = true;
					return Ok(alloc::vec![Value::I32(0)]);
				}
				// The log used to return NOTHING, so a component could not tell whether its one
				// diagnostic channel had worked.
				Ok(alloc::vec![Value::I32(STATUS_IO)])
			}
		}
	}
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

// Write `bytes` to the granted output file over StorageService, returning the number of bytes the
// service accepted - or `None` when the volume refused the write.
//
// `None` RATHER THAN ZERO. A refused write and a zero-byte write returned the same `0`, so the
// world above could not report the one distinction it exists to carry.
unsafe fn write_file(storage: u64, uri: &[u8], bytes: &[u8]) -> Option<usize> {
	unsafe {
		let data: proto::codec::Buffer = make_buffer(bytes)?;
		let path: String = String::from_utf8_lossy(uri).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		match client.write(&path, &data) {
			Some(Ok(())) => Some(bytes.len()),
			_ => None,
		}
	}
}

// Emit `msg` as one structured log entry over LogService. Returns whether the
// service accepted it - the host's proof the log grant is live.
//
// The guest's `log` takes TEXT and the SDK's signature now says so, so bytes that are not text are
// a guest that broke its side of the world - refused here rather than turned into replacement
// characters somewhere nobody can see. `String::from_utf8_lossy` was answering a question the
// caller had not asked.
unsafe fn emit_log(logsvc: u64, msg: &[u8]) -> bool {
	let Ok(text) = core::str::from_utf8(msg) else {
		return false;
	};
	let entry: Entry = Entry { timestamp: unsafe { clock() }, severity: Severity::Info, source: String::from("component"), fields: alloc::vec![Field { key: String::from("message"), value: String::from(text) }] };
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
	let component_path = factory_path("liber-component").unwrap_or_else(|| exit());
	let bytes: Vec<u8> = unsafe { load_component(storage, component_path.as_bytes()) }.unwrap_or_else(|| exit());
	let module: Module = match wasm::parse(&bytes) {
		Ok(m) => m,
		Err(_) => exit(),
	};

	// 3. resolve every import by name into the dispatch table, then instantiate.
	// Every import resolved and checked BEFORE the instance exists. One the world does not offer -
	// by name or by type - refuses the component here.
	let mut imports: Vec<WorldFn> = Vec::new();
	for i in module.imports.iter() {
		let signature: Option<&FuncType> = module.types.get(i.type_index as usize);
		match resolve(&i.module, &i.field, signature) {
			Some(op) => imports.push(op),
			None => exit(),
		}
	}
	let mut instance: Instance = Instance::new(&module);
	let mut host: ComponentHost = ComponentHost { storage, logsvc, imports, logged: false, output: Vec::new() };

	// 4. run the component: `run` reads its granted file, transforms it, logs it, and
	//    writes it back; `score` exercises the float path on real toolchain output.
	// THE COUNT IS REPORTED. It was read into `_count` and dropped, so nothing on either side of
	// the boundary checked the export's return ABI - and the guest threw its own write result away
	// too, which meant a failed write was invisible from end to end. It is now the component's
	// answer: how many bytes it processed, or one of the world's negative statuses.
	let count: i32 = match instance.invoke("run", &[], &mut host) {
		Ok(results) => results.first().map(|v: &Value| v.as_i32()).unwrap_or(0),
		Err(_) => exit(),
	};
	// Two values, because one of them was chosen where the truncation cannot show. `score(10)` is
	// 17 whether the conversion rounds down or toward zero; `score(-3)` is -2 truncating and -3
	// flooring, which is the only place the interpreter's float-to-int conversion can be seen to be
	// the one the toolchain emitted.
	let score: i32 = instance.invoke("score", &[Value::I32(10)], &mut host).ok().and_then(|r: Vec<Value>| r.first().map(|v: &Value| v.as_i32())).unwrap_or(0);
	let score_negative: i32 = instance.invoke("score", &[Value::I32(-3)], &mut host).ok().and_then(|r: Vec<Value>| r.first().map(|v: &Value| v.as_i32())).unwrap_or(0);

	// 5. report back over the bootstrap channel: a one-byte log-grant flag, the two scores and the
	//    `run` count as little-endian i32s, then the bytes the component produced (those it handed
	//    to its `write` import, captured through the granted write path). The supervisor / test
	//    reads and checks these - and reads the OUTPUT FILE back through StorageService, which is
	//    the only thing that can prove the write happened.
	// WHAT IS ACTUALLY IN THE FILE, read back through StorageService after the run.
	//
	// The report used to carry `host.output` alone - the copy taken from the component's memory on
	// the way INTO the write - and the test compared that. So `write_file` could return zero, the
	// volume could be read-only, the service could refuse, and the assertion still passed, because
	// the bytes it compared had never been near the filesystem. The host holds the storage grant, so
	// it is the one that can open the file again; the test compares both.
	let mut readback: [u8; 512] = [0u8; 512];
	let written: usize = match runtime_path("liber-component-output") {
		Some(path) => unsafe { read_file(storage, path.as_bytes(), &mut readback) }.unwrap_or(0),
		None => 0,
	};

	let mut report: Vec<u8> = Vec::with_capacity(17 + written + host.output.len());
	report.push(host.logged as u8);
	report.extend_from_slice(&score.to_le_bytes());
	report.extend_from_slice(&score_negative.to_le_bytes());
	report.extend_from_slice(&count.to_le_bytes());
	report.extend_from_slice(&(written as u32).to_le_bytes());
	report.extend_from_slice(&readback[..written]);
	report.extend_from_slice(&host.output);
	unsafe {
		send_blocking(bootstrap, &report, 0);
	}
	exit();
}
