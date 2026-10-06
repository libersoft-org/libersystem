use super::*;
use alloc::vec::Vec;

// ONE RECORD of a property block, as the kernel writes it: kind, depth, name length, value length, the name, and the
// value padded to a multiple of four bytes.
fn record(block: &mut Vec<u8>, kind: u8, depth: u8, name: &[u8], value: &[u8]) {
	block.push(kind);
	block.push(depth);
	block.extend_from_slice(&(name.len() as u16).to_le_bytes());
	block.extend_from_slice(&(value.len() as u32).to_le_bytes());
	block.extend_from_slice(name);
	block.extend_from_slice(value);
	block.resize(block.len() + ((value.len() + 3) & !3) - value.len(), 0);
}

fn clock(block: &mut Vec<u8>, hz: u64) {
	record(block, DEVICE_PROPERTY_CLOCK, 0, b"clocks", &hz.to_le_bytes());
}

#[test]
fn a_pl011_s_clock_is_the_one_its_clock_names_call_uartclk() {
	// QEMU virt's PL011: two references to the one 24 MHz `apb-pclk`, named `uartclk` and `apb_pclk`.
	let mut block = Vec::new();
	record(&mut block, DEVICE_PROPERTY_VALUE, 0, b"compatible", b"arm,pl011\0arm,primecell\0");
	clock(&mut block, 24_000_000);
	clock(&mut block, 24_000_000);
	record(&mut block, DEVICE_PROPERTY_VALUE, 0, b"clock-names", b"uartclk\0apb_pclk\0");
	assert_eq!(describe(&block), Ok(Described { clock_hz: Some(24_000_000), reg_shift: 0, reg_io_width: 1 }));
	// A board whose bus clock comes first: the named one is still the UART's.
	let mut block = Vec::new();
	clock(&mut block, 100_000_000);
	clock(&mut block, 48_000_000);
	record(&mut block, DEVICE_PROPERTY_VALUE, 0, b"clock-names", b"apb_pclk\0uartclk\0");
	assert_eq!(describe(&block).map(|described| described.clock_hz), Ok(Some(48_000_000)));
	// With no names, the first.
	let mut block = Vec::new();
	clock(&mut block, 7_372_800);
	assert_eq!(describe(&block).map(|described| described.clock_hz), Ok(Some(7_372_800)));
}

#[test]
fn a_clock_the_kernel_could_not_resolve_is_no_clock_and_the_firmware_s_divisor_is_kept() {
	let mut block = Vec::new();
	record(&mut block, DEVICE_PROPERTY_UNRESOLVED, 0, b"clocks", &[]);
	record(&mut block, DEVICE_PROPERTY_VALUE, 0, b"clock-names", b"uartclk\0");
	assert_eq!(describe(&block).map(|described| described.clock_hz), Ok(None));
	assert_eq!(describe(&[]), Ok(Described::default()), "and an empty block describes nothing at all");
}

#[test]
fn a_16550_s_clock_frequency_shift_and_width_are_read_and_a_layout_it_cannot_drive_is_refused() {
	// QEMU virt's riscv64 16550: `clock-frequency` of 3.6864 MHz, byte registers.
	let mut block = Vec::new();
	record(&mut block, DEVICE_PROPERTY_VALUE, 0, b"interrupts", &[0, 0, 0, 10, 0, 0, 0, 4]);
	record(&mut block, DEVICE_PROPERTY_VALUE, 0, b"clock-frequency", &3_686_400u32.to_be_bytes());
	record(&mut block, DEVICE_PROPERTY_VALUE, 0, b"compatible", b"ns16550a\0");
	assert_eq!(describe(&block), Ok(Described { clock_hz: Some(3_686_400), reg_shift: 0, reg_io_width: 1 }));
	// A DesignWare part's layout: registers four bytes apart, read and written as words.
	let mut block = Vec::new();
	record(&mut block, DEVICE_PROPERTY_VALUE, 0, b"reg-shift", &2u32.to_be_bytes());
	record(&mut block, DEVICE_PROPERTY_VALUE, 0, b"reg-io-width", &4u32.to_be_bytes());
	record(&mut block, DEVICE_PROPERTY_VALUE, 0, b"clock-frequency", &50_000_000u64.to_be_bytes());
	assert_eq!(describe(&block), Ok(Described { clock_hz: Some(50_000_000), reg_shift: 2, reg_io_width: 4 }));
	let mut wide = Vec::new();
	record(&mut wide, DEVICE_PROPERTY_VALUE, 0, b"reg-shift", &4u32.to_be_bytes());
	assert!(describe(&wide).is_err(), "registers sixteen bytes apart are no 16550's");
	let mut width = Vec::new();
	record(&mut width, DEVICE_PROPERTY_VALUE, 0, b"reg-io-width", &3u32.to_be_bytes());
	assert!(describe(&width).is_err(), "nor is a three-byte access");
	// A ZERO clock is no clock.
	let mut zero = Vec::new();
	record(&mut zero, DEVICE_PROPERTY_VALUE, 0, b"clock-frequency", &0u32.to_be_bytes());
	assert_eq!(describe(&zero).map(|described| described.clock_hz), Ok(None));
}

#[test]
fn a_child_node_s_properties_describe_nothing_of_the_uart_and_a_cut_record_ends_the_block() {
	let mut block = Vec::new();
	record(&mut block, DEVICE_PROPERTY_NODE, 1, b"child", &[]);
	record(&mut block, DEVICE_PROPERTY_VALUE, 1, b"clock-frequency", &1_000_000u32.to_be_bytes());
	record(&mut block, DEVICE_PROPERTY_VALUE, 1, b"reg-shift", &9u32.to_be_bytes());
	assert_eq!(describe(&block), Ok(Described::default()), "a child's clock and spacing are its own");
	let mut cut = Vec::new();
	record(&mut cut, DEVICE_PROPERTY_VALUE, 0, b"clock-frequency", &1_843_200u32.to_be_bytes());
	let whole = cut.len();
	record(&mut cut, DEVICE_PROPERTY_VALUE, 0, b"reg-shift", &9u32.to_be_bytes());
	cut.truncate(whole + 10);
	assert_eq!(describe(&cut).map(|described| described.clock_hz), Ok(Some(1_843_200)), "the records before a cut one are read, and the cut one is not");
}

#[test]
fn both_engines_compute_their_setting_through_the_shared_contract() {
	assert_eq!(<uart::Uart<NoRegisters> as Engine>::line(3_686_400, CONSOLE_BAUD), Some(uart::Line { divisor: 2 }), "QEMU virt's 16550 at the console's rate");
	assert_eq!(<uart::Uart<NoRegisters> as Engine>::line(u64::from(u32::MAX) + 1, CONSOLE_BAUD), None, "a clock past what the 16550's arithmetic takes is refused, not truncated");
	assert_eq!(<pl011::Pl011<NoRegisters> as Engine>::line(24_000_000, CONSOLE_BAUD), Some(pl011::Line { ibrd: 13, fbrd: 1 }), "QEMU virt's PL011 at the console's rate");
}

// No registers at all: the setting is arithmetic.
struct NoRegisters;

impl uart::Registers for NoRegisters {
	fn read(&mut self, _offset: u16) -> u8 {
		0
	}

	fn write(&mut self, _offset: u16, _value: u8) {}
}

impl pl011::Registers for NoRegisters {
	fn read(&mut self, _offset: u16) -> u32 {
		0
	}

	fn write(&mut self, _offset: u16, _value: u32) {}
}
