// TODO(riscv64): implement the NS16550 / SBI console. Stub for now.

use core::fmt::{self, Write};

pub fn init() {}

pub struct SerialWriter;

impl Write for SerialWriter {
	fn write_str(&mut self, _s: &str) -> fmt::Result {
		Ok(())
	}
}
