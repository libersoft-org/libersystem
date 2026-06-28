// The component SDK example: a Rust guest built against the `liber` world.
//
// `just sdk` compiles this crate for wasm32-unknown-unknown and copies the module
// into src/volume, where StorageService serves it as vol://system/app.wasm. The
// kernel's component_host loads it from storage, wires its three imports to typed
// services with no ambient authority, and invokes its exports.
//
// `run` exercises the whole world: it reads its one granted file, transforms the
// bytes (ASCII upper-casing, proving the guest touched them), logs the result, and
// writes it back - control flow plus memory plus all three host calls. `score`
// exercises the float path on real toolchain output (f64 multiply / add / truncate).

#![no_std]
#![no_main]

mod world;

// A scratch buffer in the guest's linear memory. 256 bytes is enough for the demo
// files; the host reads into it, the guest transforms it in place, the host writes
// and logs from it.
static mut BUF: [u8; 256] = [0u8; 256];

// Read the granted input, upper-case its ASCII letters in place, log the result,
// write it back, and return the number of bytes processed.
#[unsafe(no_mangle)]
pub extern "C" fn run() -> i32 {
	let buf: &mut [u8] = unsafe { &mut *core::ptr::addr_of_mut!(BUF) };
	let n: usize = world::read_input(buf);
	for byte in &mut buf[..n] {
		if byte.is_ascii_lowercase() {
			*byte -= 32;
		}
	}
	world::log_message(&buf[..n]);
	world::write_output(&buf[..n]);
	n as i32
}

// A pure float computation, exercising f64 arithmetic and the float-to-int
// conversion in genuine toolchain output: score(x) = floor(x * 1.5 + 2.0).
#[unsafe(no_mangle)]
pub extern "C" fn score(x: i32) -> i32 {
	((x as f64) * 1.5 + 2.0) as i32
}
