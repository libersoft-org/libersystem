// The `liber` world bindings - the reusable part of the component SDK.
//
// A LiberSystem component is a WebAssembly module that imports this small,
// capability-oriented world and exports an entry point. The host
// (src/user/services/core/src/component_host.rs) resolves each import by name and wires
// it to a typed system service - never to ambient authority:
//
//   liber.read(ptr, max) -> n     a granted file, read through StorageService
//   liber.write(ptr, len) -> n    a granted file, written through StorageService
//   liber.log(ptr, len)           one structured entry, emitted to LogService
//
// The component never names a path, a channel, or a service: it only sees these
// three functions, and reaches exactly the capabilities the host was granted.

// The raw host imports. The `liber` module name is what the host matches on; the
// host that instantiates the module supplies the implementations.
#[link(wasm_import_module = "liber")]
unsafe extern "C" {
	safe fn read(ptr: i32, max: i32) -> i32;
	safe fn write(ptr: i32, len: i32) -> i32;
	safe fn log(ptr: i32, len: i32);
}

// Read up to `buf.len()` bytes of the granted input into `buf`; returns how many
// bytes the host delivered.
pub fn read_input(buf: &mut [u8]) -> usize {
	let n: i32 = read(buf.as_mut_ptr() as i32, buf.len() as i32);
	if n < 0 { 0 } else { (n as usize).min(buf.len()) }
}

// Write `buf` to the granted output; returns how many bytes the host persisted (0
// when the grant is read-only).
//
// CLAMPED, like `read_input` above. This returned whatever the host said, so a host regression
// answering a million for a hundred-byte write handed a million straight to the application -
// through the wrapper whose whole job is to be the safe side of that boundary. A count larger than
// what was offered is not a count; the host cannot have written more than it was given.
pub fn write_output(buf: &[u8]) -> usize {
	let n: i32 = write(buf.as_ptr() as i32, buf.len() as i32);
	if n < 0 { 0 } else { (n as usize).min(buf.len()) }
}

// Emit `msg` as one structured log entry through the granted LogService.
//
// TEXT, and the signature says so. This took `&[u8]` while the host did `String::from_utf8_lossy`,
// so bytes that were not text became replacement characters somewhere the caller could not see -
// the wrong answer to either contract. A caller that really has bytes can decide for itself how to
// render them.
pub fn log_message(msg: &str) {
	log(msg.as_ptr() as i32, msg.len() as i32);
}

// A guest has no unwinder: a panic aborts the instance. The host surfaces the trap
// to its caller, so the component never spins here in practice.
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
	core::arch::wasm32::unreachable()
}
