// wasi_host - the minimal WASI-style host: it runs a WebAssembly component and maps
// the component's imports onto native services over IPC.
//
// The kernel (or a supervisor) hands this program a bootstrap channel and, over it,
// a StorageService client - the one capability the host is granted. The host loads
// the embedded component, instantiates it on the `wasm` runtime, and runs its `run`
// export. The component's only import, `liber.read`, is wired to read the host's
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

use proto::codec::Transport;
use proto::system::{picker, volume, OpenOpts};
use rt::*;
use wasm::{Host, Instance, Trap, Value};

// The single file the host grants the component (its whole "world"). The component
// cannot name a file - it only calls `read`, and the host reads this one.
const GRANTED: &[u8] = b"vol://system/hello.txt";

// The first Wasm component, hand-encoded. In WAT:
//   (module
//     (import "liber" "read" (func $read (param i32 i32) (result i32)))
//     (memory 1)
//     (func (export "run") (result i32)
//       i32.const 0  i32.const 256  call $read))
// `run` asks the host to read up to 256 bytes into memory at offset 0 and returns
// the byte count.
const COMPONENT: &[u8] = &[
	0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic + version
	0x01, 0x0b, 0x02, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, 0x60, 0x00, 0x01, 0x7f, // types: (i32,i32)->i32, ()->i32
	0x02, 0x0e, 0x01, 0x05, b'l', b'i', b'b', b'e', b'r', 0x04, b'r', b'e', b'a', b'd', 0x00, 0x00, // import liber.read : type 0
	0x03, 0x02, 0x01, 0x01, // function 0 (combined index 1) : type 1
	0x05, 0x03, 0x01, 0x00, 0x01, // memory: min 1 page
	0x07, 0x07, 0x01, 0x03, b'r', b'u', b'n', 0x00, 0x01, // export "run" -> func 1
	0x0a, 0x0b, 0x01, 0x09, 0x00, 0x41, 0x00, 0x41, 0x80, 0x02, 0x10, 0x00, 0x0b, // code: i32.const 0; i32.const 256; call 0; end
];

// A proto Transport over an rt channel: send the request, then block for the reply.
// The generated volume client calls through this to reach StorageService.
struct ChannelTransport {
	chan: u64,
}

impl Transport for ChannelTransport {
	fn call(&mut self, request: &[u8], request_handle: u64) -> Option<(alloc::vec::Vec<u8>, u64)> {
		unsafe {
			if !send_blocking(self.chan, request, request_handle) {
				return None;
			}
			let mut reply: [u8; 256] = [0u8; 256];
			match recv_blocking(self.chan, &mut reply) {
				Received::Message { len, handle } => Some((reply[..len].to_vec(), handle)),
				Received::Closed => None,
			}
		}
	}
}

// How the host backs the component's `read` import - its whole granted world. With
// `Storage` the host opens one fixed file directly over StorageService; with
// `Picker` it has no filesystem access of its own and must ask the FilePicker,
// which (on the user's pick) grants exactly the chosen file - the powerbox.
enum Grant {
	Storage(u64),
	Picker(u64),
}

// The WASI host: services the component's `read` import from whatever it was
// granted, copying the resulting bytes into the component's linear memory.
struct WasiHost {
	grant: Grant,
}

impl Host for WasiHost {
	fn call_import(&mut self, import: u32, args: &[Value], memory: &mut [u8]) -> Result<alloc::vec::Vec<Value>, Trap> {
		// import 0 is `liber.read(ptr, max) -> count`; nothing else is granted.
		if import != 0 {
			return Err(Trap("import not granted"));
		}
		let ptr: usize = args.first().map(|v: &Value| v.as_i32() as usize).unwrap_or(0);
		let max: usize = args.get(1).map(|v: &Value| v.as_i32() as usize).unwrap_or(0);
		let end: usize = ptr.saturating_add(max).min(memory.len());
		if ptr > end {
			return Err(Trap("read pointer out of bounds"));
		}
		let dst: &mut [u8] = &mut memory[ptr..end];
		let n: usize = match self.grant {
			Grant::Storage(storage) => unsafe { read_fixed(storage, dst) },
			Grant::Picker(picker) => unsafe { read_picked(picker, dst) },
		}
		.ok_or(Trap("granted read failed"))?;
		Ok(alloc::vec![Value::I32(n as i32)])
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
		let mapped: u64 = syscall(SYS_MEMORY_MAP, result.file, 0, 0, 0);
		if sys_is_err(mapped) {
			syscall(SYS_HANDLE_CLOSE, result.file, 0, 0, 0);
			return None;
		}
		let n: usize = (result.size as usize).min(dst.len());
		core::ptr::copy_nonoverlapping(mapped as *const u8, dst.as_mut_ptr(), n);
		syscall(SYS_MEMORY_UNMAP, result.file, 0, 0, 0);
		syscall(SYS_HANDLE_CLOSE, result.file, 0, 0, 0);
		Some(n)
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
		let mapped: u64 = syscall(SYS_MEMORY_MAP, picked.file, 0, 0, 0);
		if sys_is_err(mapped) {
			syscall(SYS_HANDLE_CLOSE, picked.file, 0, 0, 0);
			return None;
		}
		let n: usize = (picked.size as usize).min(dst.len());
		core::ptr::copy_nonoverlapping(mapped as *const u8, dst.as_mut_ptr(), n);
		syscall(SYS_MEMORY_UNMAP, picked.file, 0, 0, 0);
		syscall(SYS_HANDLE_CLOSE, picked.file, 0, 0, 0);
		Some(n)
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
	let module = match wasm::parse(COMPONENT) {
		Ok(m) => m,
		Err(_) => exit(),
	};
	let mut instance: Instance = Instance::new(&module);
	let mut host: WasiHost = WasiHost { grant };
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
