// The raw host imports and the wrappers over them.
//
// The module names are what the host matches on, together with the exact signature; the host that
// instantiates the module supplies the implementations.

use crate::status::{Error, clamp_count};

#[link(wasm_import_module = "liber:vfs@1")]
unsafe extern "C" {
	safe fn read(ptr: i32, max: i32) -> i32;
	safe fn write(ptr: i32, len: i32) -> i32;
}

#[link(wasm_import_module = "liber:log@1")]
unsafe extern "C" {
	#[link_name = "log"]
	safe fn log_raw(ptr: i32, len: i32) -> i32;
}

// Read up to `buf.len()` bytes of the granted input into `buf`; `Ok(n)` is how many bytes the host
// delivered, and `Ok(0)` means the input really was empty.
pub fn read_input(buf: &mut [u8]) -> Result<usize, Error> {
	clamp_count(read(buf.as_mut_ptr() as i32, buf.len() as i32), buf.len())
}

// Write `buf` to the granted output; `Ok(n)` is how many bytes the host persisted. A read-only
// grant is `Err(Denied)` rather than `Ok(0)`, which is the difference this world exists to carry.
pub fn write_output(buf: &[u8]) -> Result<usize, Error> {
	clamp_count(write(buf.as_ptr() as i32, buf.len() as i32), buf.len())
}

// Emit `msg` as one structured log entry through the granted LogService.
//
// TEXT, and the signature says so. This took `&[u8]` while the host did `String::from_utf8_lossy`,
// so bytes that were not text became replacement characters somewhere the caller could not see -
// the wrong answer to either contract. A caller that really has bytes can decide for itself how to
// render them.
pub fn log_message(msg: &str) -> Result<(), Error> {
	let status: i32 = log_raw(msg.as_ptr() as i32, msg.len() as i32);
	if status < 0 { Err(Error::from_status(status)) } else { Ok(()) }
}
