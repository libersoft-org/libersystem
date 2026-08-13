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
use proto::system::{Entry, Error, Field, OpenOpts, Severity, log, volume};
use rt::*;
use wasm::module::FuncType;
use wasm::{Instance, Module, ValidatedModule, Value};

include!(concat!(env!("OUT_DIR"), "/program_paths.rs"));

// The world itself - which imports are granted and how a `(ptr, len)` becomes a memory window -
// lives in `component-world`, beside its tests. This binary is built for `*-unknown-none`, so
// nothing in it can run on the host: the only coverage those two decisions had was one kernel
// end-to-end path, which needs a boot per assertion and only ever takes the happy one.
use wasm::world::{LogOutcome, ReadOutcome, WorldFn, WorldHost, WorldServices, WriteOutcome, resolve};

// The IPC half of the world: the two typed-service capabilities this host was granted (a
// StorageService client and a LogService client) and the two paths it may use them on. It holds no
// ambient authority - only these two channels, reachable only through the three wired imports.
//
// THE DISPATCH ITSELF IS NOT HERE ANY MORE. It is `wasm::world::WorldHost`, which is in a crate
// that builds for the development machine, so a write refused, a write that fails, a service that
// never answers and a log grant that does not respond are ordinary tests instead of paths only a
// kernel boot could reach - and a boot only ever takes the happy one. What is left below is exactly
// the part that needs a running service.
//
// The two paths are resolved ONCE, before the instance exists. They used to be looked up per call
// and a missing one was a trap mid-run, which is a startup misconfiguration reported as a guest
// fault at whatever moment the guest happened to ask.
struct ComponentServices {
	storage: u64,
	logsvc: u64,
	input_path: &'static str,
	output_path: &'static str,
}

impl WorldServices for ComponentServices {
	fn read(&mut self, dst: &mut [u8]) -> ReadOutcome {
		unsafe { read_file(self.storage, self.input_path.as_bytes(), dst) }
	}

	fn write(&mut self, bytes: &[u8]) -> WriteOutcome {
		unsafe { write_file(self.storage, self.output_path.as_bytes(), bytes) }
	}

	fn log(&mut self, text: &str) -> LogOutcome {
		unsafe { emit_log(self.logsvc, text.as_bytes()) }
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
// Read the granted input file, answering WHY it did not happen rather than whether it did - the
// same distinction `write_file` already made, applied to the operation beside it.
//
// A volume that ANSWERED and refused is `Refused`, and the component should not retry it; a service
// that did not answer, or a read that failed once the file was open, is `Failed`. Both used to be
// `None` and both reached the guest as `STATUS_IO`, so a component told to try again was sometimes
// being told to try something it may not do.
unsafe fn read_file(storage: u64, uri: &[u8], dst: &mut [u8]) -> ReadOutcome {
	unsafe {
		let opts: OpenOpts = OpenOpts { path: String::from_utf8_lossy(uri).into_owned(), write: false, create: false };
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let result = match client.open(&opts) {
			Some(Ok(r)) => r,
			// The volume answered and said no: that is the grant, not the machine.
			Some(Err(_)) => return ReadOutcome::Refused,
			// Nothing came back at all.
			None => return ReadOutcome::Failed,
		};
		if result.file == 0 {
			return ReadOutcome::Failed;
		}
		match read_into(result.file, result.size, dst) {
			Some(n) => ReadOutcome::Read(n),
			None => ReadOutcome::Failed,
		}
	}
}

// Write `bytes` to the granted output file over StorageService, answering WHY it did not happen
// rather than whether it did. `WriteOutcome` is the world's - it is the distinction the guest is
// told, so it belongs beside the status codes that carry it.
unsafe fn write_file(storage: u64, uri: &[u8], bytes: &[u8]) -> WriteOutcome {
	unsafe {
		let Some(data) = make_buffer(bytes) else { return WriteOutcome::Failed };
		let path: String = String::from_utf8_lossy(uri).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		match client.write(&path, &data) {
			Some(Ok(())) => WriteOutcome::Wrote(bytes.len()),
			// `Denied` is the volume refusing; every other answer is a fault on the way there.
			// `NotFound` and `Invalid` are the host having asked wrongly, `Again` and `Closed` are
			// the service - none of them is "you may not", which is the only thing `Denied` means.
			Some(Err(Error::Denied)) => WriteOutcome::Refused,
			_ => WriteOutcome::Failed,
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
unsafe fn emit_log(logsvc: u64, msg: &[u8]) -> LogOutcome {
	let Ok(text) = core::str::from_utf8(msg) else {
		// The world's `log` takes TEXT, and the host has already checked this - reaching here means
		// the two disagree, which is this host's fault rather than the grant's.
		return LogOutcome::Failed;
	};
	let entry: Entry = Entry { timestamp: unsafe { clock() }, severity: Severity::Info, source: String::from("component"), fields: alloc::vec![Field { key: String::from("message"), value: String::from(text) }] };
	let mut client = log::Client::new(ChannelTransport { chan: logsvc });
	match client.emit(&entry) {
		Some(Ok(())) => LogOutcome::Logged,
		Some(Err(_)) => LogOutcome::Refused,
		None => LogOutcome::Failed,
	}
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
	// PARSED AND THEN VALIDATED, before anything about it is believed.
	//
	// `Instance::new` used to take a `Module` and decode the bodies itself, holding any error until
	// the first `invoke` - so a component's data segments and globals were installed before
	// anything had established that its code was well formed. `ValidatedModule` is the only thing
	// an instance can be built from now, and the import resolution below reads it through
	// `.module()`.
	let module: Module = match wasm::parse(&bytes) {
		Ok(m) => m,
		Err(_) => exit(),
	};
	let validated: ValidatedModule = match wasm::validate(module) {
		Ok(m) => m,
		Err(_) => exit(),
	};
	let module: &Module = validated.module();

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
	let mut instance: Instance = match Instance::new(&validated) {
		Ok(i) => i,
		Err(_) => exit(),
	};
	// THE TWO PATHS BEFORE THE INSTANCE, not per call. A manifest without one of them is a
	// misconfigured host and it says so here, rather than trapping the guest at whichever moment it
	// happened to ask.
	let input_path = factory_path("hello").unwrap_or_else(|| exit());
	let output_path = runtime_path("liber-component-output").unwrap_or_else(|| exit());
	let services = ComponentServices { storage, logsvc, input_path, output_path };
	let mut host: WorldHost<ComponentServices> = WorldHost::new(services, imports);

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
	let written: usize = match unsafe { read_file(storage, output_path.as_bytes(), &mut readback) } {
		ReadOutcome::Read(n) => n,
		// The readback is the test's evidence that the write landed; a refusal and a failure are
		// both "nothing to report", and the assertion on the other side is what says so.
		ReadOutcome::Refused | ReadOutcome::Failed => 0,
	};

	let mut report: Vec<u8> = Vec::with_capacity(17 + written + host.output().len());
	report.push(host.logged() as u8);
	report.extend_from_slice(&score.to_le_bytes());
	report.extend_from_slice(&score_negative.to_le_bytes());
	report.extend_from_slice(&count.to_le_bytes());
	report.extend_from_slice(&(written as u32).to_le_bytes());
	report.extend_from_slice(&readback[..written]);
	report.extend_from_slice(host.output());
	unsafe {
		send_blocking(bootstrap, &report, 0);
	}
	exit();
}
