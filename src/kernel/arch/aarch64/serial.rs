// TODO(aarch64): implement the PL011 UART. Stub for now so the tree compiles.

use core::fmt::{self, Write};

pub fn init() {}

pub struct SerialWriter;

impl Write for SerialWriter {
	fn write_str(&mut self, _s: &str) -> fmt::Result {
		Ok(())
	}
}
