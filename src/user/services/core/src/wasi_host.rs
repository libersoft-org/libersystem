// wasi_host - the minimal WASI-style host: it runs a WebAssembly component and maps
// the component's imports onto native services over IPC.
//
// The kernel (or a supervisor) hands this program a bootstrap channel and, over it,
// a StorageService client - the one capability the host is granted. The host loads
// the embedded component, instantiates it on the `wasm` runtime, and runs its `run`
// export. The component's only import, `liber:vfs@1 read`, is wired to read the host's
// single granted file (`vol://system/hello.txt`) through StorageService into the
// component's linear memory. The component has no `open` import and no other
// capability, so it can reach nothing it was not explicitly given - a WASI "world"
// is exactly the set of imports the host wires up.
//
// After running the component the host reports the bytes the component read back
// over the bootstrap channel (the kernel scenario / test reads them), then exits.

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
use proto::system::{OpenOpts, picker, volume};
use rt::*;
use wasm::{Host, Instance, Trap, Value};

// The single file the host grants the component (its whole "world"). The component
// cannot name a file - it only calls `read`, and the host reads this one.
const GRANTED: &[u8] = b"vol://system/hello.txt";

// The first Wasm component, hand-encoded. In WAT:
//   (module
//     (import "liber:vfs@1" "read" (func $read (param i32 i32) (result i32)))
//     (memory 1)
//     (func (export "run") (result i32)
//       i32.const 0  i32.const 256  call $read))
// `run` asks the host to read up to 256 bytes into memory at offset 0 and returns
// the byte count.
const COMPONENT: &[u8] = &[
	0x00,
	0x61,
	0x73,
	0x6d,
	0x01,
	0x00,
	0x00,
	0x00, // magic + version
	0x01,
	0x0b,
	0x02,
	0x60,
	0x02,
	0x7f,
	0x7f,
	0x01,
	0x7f,
	0x60,
	0x00,
	0x01,
	0x7f, // types: (i32,i32)->i32, ()->i32
	0x02,
	0x14,
	0x01,
	0x0b,
	b'l',
	b'i',
	b'b',
	b'e',
	b'r',
	b':',
	b'v',
	b'f',
	b's',
	b'@',
	b'1',
	0x04,
	b'r',
	b'e',
	b'a',
	b'd',
	0x00,
	0x00, // import liber:vfs@1 read : type 0
	0x03,
	0x02,
	0x01,
	0x01, // function 0 (combined index 1) : type 1
	0x05,
	0x03,
	0x01,
	0x00,
	0x01, // memory: min 1 page
	0x07,
	0x07,
	0x01,
	0x03,
	b'r',
	b'u',
	b'n',
	0x00,
	0x01, // export "run" -> func 1
	0x0a,
	0x0b,
	0x01,
	0x09,
	0x00,
	0x41,
	0x00,
	0x41,
	0x80,
	0x02,
	0x10,
	0x00,
	0x0b, // code: i32.const 0; i32.const 256; call 0; end
];

// How the host backs the component's `read` import - its whole granted world. With
// `Storage` the host opens one fixed file directly over StorageService; with
// `Picker` it has no filesystem access of its own and must ask the FilePicker,
// which (on the user's pick) grants exactly the chosen file - the powerbox.
enum Grant {
	Storage(u64),
	Picker(u64),
}

// The WASI host's SERVICES, behind the shared world seam.
//
// This used to be a private `Host` implementation with its own dispatch, and it answered a failed
// read with `Trap` while `WorldHost` answered the same import on the same versioned ABI with
// `STATUS_IO`. Two hosts of one interface with two error semantics, both shipping in one image, and
// which a component got depended on which host launched it - which is the thing a version on an
// interface exists to make impossible.
//
// So the dispatch, the window rules and the status mapping come from `WorldHost` and what is left
// here is the part that is genuinely this host's: which grant the bytes come from. The comment
// beside the window fix already said the rule - "One set of rules, in one place, for both hosts" -
// and this is the rest of it.
struct WasiServices {
	grant: Grant,
}

impl wasm::world::WorldServices for WasiServices {
	fn read(&mut self, dst: &mut [u8]) -> wasm::world::ReadOutcome {
		let read = match self.grant {
			Grant::Storage(storage) => unsafe { read_fixed(storage, dst) },
			Grant::Picker(picker) => unsafe { read_picked(picker, dst) },
		};
		// `Failed` rather than `Refused`: these two helpers answer `None` for a service that did not
		// reply as well as for an open that was refused, and telling the guest `DENIED` when the
		// truth may be "the service is gone" is the conflation the typed outcomes exist to end.
		// Separating them properly means the helpers reporting which, which is work for the day
		// this host's grants grow past two.
		match read {
			Some(n) => wasm::world::ReadOutcome::Read(n),
			None => wasm::world::ReadOutcome::Failed,
		}
	}

	// Neither is granted here: this host demonstrates the READ half of the world - a fixed file, or
	// one the user picked through the powerbox - and a component asking for either is refused at
	// instantiation by the import resolution below, not at the call. These exist because the trait
	// does, and answering them by refusing is more honest than a panic on a path that cannot run.
	fn write(&mut self, _bytes: &[u8]) -> wasm::world::WriteOutcome {
		wasm::world::WriteOutcome::Refused
	}

	fn log(&mut self, _text: &str) -> wasm::world::LogOutcome {
		wasm::world::LogOutcome::Refused
	}
}

// Open the host's one fixed granted file over StorageService and copy up to
// `dst.len()` bytes into `dst`, returning the number copied. None on any failure.
unsafe fn read_fixed(storage: u64, dst: &mut [u8]) -> Option<usize> {
	unsafe {
		let opts: OpenOpts = OpenOpts { path: alloc::string::String::from_utf8_lossy(GRANTED).into_owned(), write: false, create: false };
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

// Ask the FilePicker for the user's chosen file - the powerbox: the host has no
// filesystem access of its own, only the picker - then copy up to `dst.len()` bytes
// of that granted file into `dst`. The picker returns the file as a handle<file>
// capability, which the host maps and reads. None on any failure.
unsafe fn read_picked(picker: u64, dst: &mut [u8]) -> Option<usize> {
	unsafe {
		let mut client = picker::Client::new(ChannelTransport { chan: picker });
		let picked = match client.pick() {
			Some(Ok(p)) => p,
			_ => return None,
		};
		if picked.file == 0 {
			return None;
		}
		read_into(picked.file, picked.size, dst)
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];

	// 1. receive the capability the host is granted: a StorageService client (read a
	//    fixed file) or a FilePicker client (read the user-picked file - the powerbox:
	//    no filesystem access of our own).
	let grant: Grant = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 && &buf[..7] == b"STORAGE" => Grant::Storage(handle),
		Received::Message { len, handle } if handle != 0 && len >= 6 && &buf[..6] == b"PICKER" => Grant::Picker(handle),
		_ => exit(),
	};

	// 2. load + instantiate the component and run it. `run` calls the read import,
	//    which the host services from whatever it was granted.
	// Parsed and then VALIDATED - see `component_host` for why the two are separate steps and why
	// an instance can be built from nothing else.
	let module = match wasm::parse(COMPONENT) {
		Ok(m) => m,
		Err(_) => exit(),
	};
	let validated = match wasm::validate(module) {
		Ok(m) => m,
		Err(_) => exit(),
	};
	let module = validated.module();
	// EVERY IMPORT RESOLVED BEFORE THE INSTANCE EXISTS, by name AND by declared signature.
	//
	// This bound its one import by INDEX - "import 0 is read" - so a module declaring something
	// else in that slot, or declaring `read` with a different type, was instantiated and refused at
	// the call site if at all. The import list is the manifest of requested authority; a module
	// asking for a camera is asking for a camera, whether or not the call is reachable.
	for import in module.imports.iter() {
		let signature = module.types.get(import.type_index as usize);
		if wasm::world::resolve(&import.module, &import.field, signature) != Some(wasm::world::WorldFn::Read) {
			exit();
		}
	}
	let mut instance: Instance = match Instance::new(&validated) {
		Ok(i) => i,
		Err(_) => exit(),
	};
	// The imports, in the order the module declares them - which is the order `WorldHost` dispatches
	// by. Only `Read` resolves here; the loop above has already refused anything else.
	let imports: alloc::vec::Vec<wasm::world::WorldFn> = alloc::vec![wasm::world::WorldFn::Read; module.imports.len()];
	let mut host = wasm::world::WorldHost::new(WasiServices { grant }, imports);
	let count: i32 = match instance.invoke("run", &[], &mut host) {
		Ok(results) => results.first().map(|v: &Value| v.as_i32()).unwrap_or(0),
		Err(_) => exit(),
	};

	// 3. report the bytes the component read (now in its linear memory) back over
	//    the bootstrap channel, then exit.
	let n: usize = (count.max(0) as usize).min(instance.memory().len());
	unsafe {
		send_blocking(bootstrap, &instance.memory()[..n], 0);
	}
	exit();
}
