// TODO(aarch64): implement the PL011 UART (QEMU virt maps it at 0x0900_0000).
// Stub for now so the tree cross-compiles; nothing here runs until M116.

use core::fmt::{self, Write};

pub fn init() {}

pub fn enable_rx_irq() {}

pub fn enable_async() {}

pub fn drain_tx() {}

pub fn flush_sync() {}

pub fn write_bytes(bytes: &[u8]) -> usize {
	bytes.len()
}

pub fn read_byte() -> Option<u8> {
	None
}

pub struct SerialWriter;

impl Write for SerialWriter {
	fn write_str(&mut self, _s: &str) -> fmt::Result {
		Ok(())
	}
}
